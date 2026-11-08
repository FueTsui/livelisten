//! Windows installer for per-user desktop apps (dev standard 8.1, REL-01 to REL-08).
//!
//! The setup program carries its payload appended to itself. The product details come
//! from the app's install manifest, so the same code installs any app. The setup program
//! with an empty payload is the uninstaller.

pub mod args;
pub mod codec;
pub mod dialogs;
pub mod hash;
pub mod install;
pub mod log;
pub mod manifest;
pub mod paths;
pub mod payload;
pub mod process;
pub mod registry;
pub mod shortcut;
pub mod text;
pub mod ui;
pub mod uninstall;

use std::cmp::Ordering;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use args::Args;
use install::{CLOSE_TIMEOUT, Failure, Plan, Quiet, Report, UPDATE_CLOSE_TIMEOUT, Unattended};
use manifest::Role;
use paths::Env;
use payload::Package;
use text::tr;

fn failure_message(failure: &Failure, name: &str) -> String {
    match failure {
        Failure::Invalid(reason) | Failure::RolledBack(reason) => reason.clone(),
        Failure::Cancelled => String::new(),
        Failure::InUse(names) => format!(
            "{} {}\n{}{name}{}",
            names.join(", "),
            tr("仍在运行，没有替换或删除任何文件。", "is still running; no file was replaced or removed."),
            tr("请先退出", "Quit "),
            tr("（托盘图标 → 退出）后再试。", " first (tray icon → Quit), then try again."),
        ),
    }
}

/// A failure as a message box, with the log file named.
fn show_failure(failure: &Failure, name: &str, uninstalling: bool) {
    let message = failure_message(failure, name);
    if message.is_empty() {
        return;
    }
    let what = if uninstalling {
        tr("卸载未完成", "Uninstalling did not finish")
    } else {
        tr("安装未完成", "Setup did not finish")
    };
    let log = log::path().map(|p| format!("\n\n{}{}", tr("日志：", "Log: "), p.display())).unwrap_or_default();
    dialogs::error(&format!("{what}{}{message}{log}", tr("：", ": ")));
}

fn raw_switch(raw: &[OsString], name: &str) -> bool {
    raw.iter().any(|a| a.to_string_lossy().eq_ignore_ascii_case(name))
}

/// Runs the setup program or uninstaller and returns its exit code (REL-04).
pub fn run() -> i32 {
    let raw: Vec<OsString> = std::env::args_os().skip(1).collect();
    if raw.len() == 3 && raw[0] == uninstall::CLEANUP_SWITCH {
        if let Ok(pid) = raw[2].to_string_lossy().parse() {
            uninstall::cleanup(Path::new(&raw[1]), pid);
        }
        return 0;
    }
    let args = match args::parse(raw.clone()) {
        Ok(args) => args,
        Err(problem) => {
            let quiet = (raw_switch(&raw, "/SILENT") || raw_switch(&raw, "/VERYSILENT"))
                && raw_switch(&raw, "/SUPPRESSMSGBOXES");
            if !quiet {
                dialogs::error(&format!("{problem}\n\n{}", args::USAGE));
            }
            return 1;
        }
    };
    if args.help {
        dialogs::info(args::USAGE);
        return 0;
    }
    let Ok(exe) = std::env::current_exe() else { return 1 };
    if let Some(Some(path)) = &args.log {
        let _ = log::open(path.clone());
    }
    let _com = shortcut::Com::start();
    let env = match Env::detect() {
        Ok(env) => env,
        Err(e) => {
            if args.may_show_messages() {
                dialogs::error(&e.to_string());
            }
            return 1;
        }
    };
    let package = match Package::open(&exe, &mut |_, _| {}) {
        Ok(package) => package,
        Err(e) => {
            log!("{e}");
            if args.may_show_messages() {
                dialogs::error(install::damaged_text());
            }
            return 1;
        }
    };
    let m = &package.manifest;
    if log::path().is_none() {
        let role = if m.role == Role::Uninstaller { "Uninstall" } else { "Setup" };
        let _ = log::open(std::env::temp_dir().join(format!("{}-{role}-{}.log", m.product.id, log::stamp())));
    }
    log!(
        "{} {} {:?} {}",
        m.product.id,
        m.version,
        m.role,
        raw.iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ")
    );
    if let Some(root) = &env.test_root {
        log!("isolated test mode in {}", root.display());
    }
    if args.verify {
        log!("package verified: {} files", m.files.len());
        if !args.quiet() {
            dialogs::info(&format!(
                "{} {} {}: {} {}",
                text::name(&m.product.name),
                m.version,
                tr("安装包完整", "package is intact"),
                m.files.len(),
                tr("个文件", "files")
            ));
        }
        return 0;
    }
    let code = match package.manifest.role {
        Role::Installer => install_flow(&args, &env, package),
        Role::Uninstaller => uninstall_flow(&args, &env, &exe, package),
    };
    log!("exit code {code}");
    code
}

/// Silent modes ask before closing a running program, unless message boxes are suppressed.
fn approve_closing(args: &Args, dir: &Path, name: &str) -> bool {
    if args.update || args.suppress_msgboxes || process::running_in(dir).is_empty() {
        return true;
    }
    dialogs::confirm(&format!(
        "{name}{}",
        tr(
            " 正在运行。继续将先请它正常退出，必要时再结束它。\n\n点击“确定”继续，点击“取消”退出。",
            " is running. Continuing asks it to exit first and ends it if needed.\n\nChoose OK to continue or Cancel to stop."
        )
    ))
}

fn install_flow(args: &Args, env: &Env, mut package: Package) -> i32 {
    let product = package.manifest.product.clone();
    let version = package.manifest.version.clone();
    let name = text::name(&product.name).to_string();
    let show = args.may_show_messages();
    let fail = |failure: Failure| -> i32 {
        log!("failed with exit code {}: {failure:?}", failure.code());
        if show {
            show_failure(&failure, &name, false);
        }
        failure.code()
    };
    let registry = env.registry();
    let existing = match install::find_existing(env, &registry, &product, args.dir.as_deref()) {
        Ok(existing) => existing,
        Err(failure) => return fail(failure),
    };
    if args.update && existing.is_none() {
        return fail(Failure::Invalid(format!(
            "{}{name}{}",
            tr("没有找到已安装的", "No installation of "),
            tr("。/UPDATE 只用于升级。", " was found. /UPDATE only upgrades.")
        )));
    }
    let installed = existing.as_ref().map(|e| e.record.version.clone());
    let newer = installed.clone().filter(|old| manifest::compare_versions(&version, old) == Ordering::Less);
    if let Some(old) = &newer {
        if args.update {
            return fail(Failure::Invalid(format!(
                "{} {version} < {old}",
                tr("不能用较旧的版本更新：", "Cannot update to an older version:")
            )));
        }
        let question = format!(
            "{} {old} → {version}",
            tr("已安装较新的版本。要替换成旧版本吗？", "A newer version is installed. Replace it with the older one?")
        );
        if args.quiet() && show && !dialogs::ask(&question) {
            return 2;
        }
    }
    let mut tasks = existing.as_ref().map_or(product.tasks, |e| e.tasks);
    if let Some(on) = args.task("startmenuicon") {
        tasks.start_menu = on;
    }
    if let Some(on) = args.task("desktopicon") {
        tasks.desktop = on;
    }
    if let Some(on) = args.task("startup") {
        tasks.startup = on;
    }
    let dir = existing
        .as_ref()
        .map(|e| e.dir.clone())
        .or_else(|| args.dir.clone())
        .unwrap_or_else(|| env.default_dir(&product));
    let plan = Plan {
        dir,
        tasks,
        // A silent install never starts the program (scripts and deployments expect none);
        // an update starts it again only when it was running.
        launch: !args.no_launch && !args.update && !args.quiet(),
        relaunch_if_running: args.update && !args.no_launch,
        close_timeout: if args.update { UPDATE_CLOSE_TIMEOUT } else { CLOSE_TIMEOUT },
        existing: existing.clone(),
    };
    if existing.is_none() && (args.quiet() || args.update) {
        if let Err(problem) = install::check_new_dir(&plan.dir, env, &package.manifest) {
            return fail(Failure::Invalid(problem));
        }
    }
    // An in-app update never shows the options: progress only.
    let auto = args.silent || args.update;
    if (args.very_silent || auto) && !approve_closing(args, &plan.dir, &name) {
        return 2;
    }
    let report: &dyn Report = if args.update { &Unattended } else { &Quiet };
    if args.very_silent {
        return match install::install(env, &mut package, &plan, report) {
            Ok(_) => 0,
            Err(failure) => fail(failure),
        };
    }

    let uninstaller = existing.as_ref().map(|e| e.dir.join(manifest::UNINSTALLER)).filter(|p| p.is_file());
    let env_vars = env.child_environment(&product);
    let (check_env, manifest, exe_product) = (env.clone(), package.manifest.clone(), product.clone());
    let view = ui::View {
        role: ui::Role::Install,
        name: name.clone(),
        version,
        product_id: product.id.clone(),
        data_label: text::data_label(&product),
        dir: plan.dir.clone(),
        dir_locked: existing.is_some(),
        installed,
        newer,
        tasks,
        launch: plan.launch,
        delete_data: false,
        auto,
        updating: args.update,
        suppress: args.suppress_msgboxes,
        uninstaller,
        check_dir: Box::new(move |dir| install::check_new_dir(dir, &check_env, &manifest)),
        running: Box::new(|dir| !process::running_in(dir).is_empty()),
        launch_program: Box::new(move |dir| {
            if let Err(e) = process::launch(&install::main_exe(dir, &exe_product), &env_vars) {
                log!("could not start the program: {e}");
            }
        }),
        log_path: log::path(),
    };
    // The window takes the package; without a window, setup falls back to message boxes.
    let package = std::sync::Arc::new(std::sync::Mutex::new(Some(package)));
    let (work_env, work_package, work_plan, work_name) = (env.clone(), package.clone(), plan.clone(), name.clone());
    let work: ui::Work = Box::new(move |choice: ui::Choice, report: &dyn Report| {
        // The window runs this on its own thread, which needs COM for the shortcuts.
        let _com = shortcut::Com::start();
        let mut plan = work_plan;
        plan.dir = choice.dir;
        plan.tasks = choice.tasks;
        plan.launch = choice.launch;
        let Some(mut package) = work_package.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return Err((3, String::new()));
        };
        let report: &dyn Report = if plan.close_timeout == UPDATE_CLOSE_TIMEOUT { &Unattended } else { report };
        install::install(&work_env, &mut package, &plan, report)
            .map(|done| ui::Done::Installed { previous: done.previous, warnings: done.warnings })
            .map_err(|failure| {
                log!("failed with exit code {}: {failure:?}", failure.code());
                (failure.code(), failure_message(&failure, &work_name))
            })
    });
    match ui::run(view, work) {
        Ok(outcome) => {
            if let Some(uninstaller) = outcome.open_uninstaller {
                open_uninstaller(&uninstaller);
            }
            outcome.code
        }
        Err(problem) => {
            log!("the setup window could not be shown: {problem}");
            let Some(mut package) = package.lock().unwrap_or_else(|e| e.into_inner()).take() else { return 3 };
            if !auto
                && !dialogs::confirm(&format!(
                    "{}{name} {}{}",
                    tr("无法显示安装窗口，将用默认选项安装 ", "The setup window cannot be shown. Install "),
                    package.manifest.version,
                    tr("。", " with the default options?")
                ))
            {
                return 2;
            }
            match install::install(env, &mut package, &plan, report) {
                Ok(_) => {
                    if !auto {
                        dialogs::info(&format!("{name} {}", tr("已安装。", "is installed.")));
                    }
                    0
                }
                Err(failure) => fail(failure),
            }
        }
    }
}

/// Opens the installed version's uninstaller from the temporary folder, so its working
/// folder never keeps the installation in use.
fn open_uninstaller(uninstaller: &PathBuf) {
    if let Err(e) = std::process::Command::new(uninstaller).current_dir(std::env::temp_dir()).spawn() {
        log!("could not open the uninstaller: {e}");
        dialogs::error(&e.to_string());
    }
}

fn uninstall_flow(args: &Args, env: &Env, exe: &Path, package: Package) -> i32 {
    let product = package.manifest.product.clone();
    let name = text::name(&product.name).to_string();
    let label = text::data_label(&product);
    let Some(dir) = exe.parent().map(Path::to_path_buf) else { return 1 };
    let show = args.may_show_messages();
    let leftovers = |left: &uninstall::Leftovers| -> Vec<String> {
        left.paths.iter().map(|p| format!("{} {}", tr("未能删除", "Could not remove"), p.display())).collect()
    };
    if args.very_silent {
        return match uninstall::uninstall(env, &product, &dir, args.delete_data, &Quiet) {
            Ok(_) => 0,
            Err(failure) => {
                log!("failed with exit code {}: {failure:?}", failure.code());
                if show {
                    show_failure(&failure, &name, true);
                }
                failure.code()
            }
        };
    }
    if args.silent && !approve_closing(args, &dir, &name) {
        return 2;
    }
    let view = ui::View {
        role: ui::Role::Uninstall,
        name: name.clone(),
        version: package.manifest.version.clone(),
        product_id: product.id.clone(),
        data_label: label.clone(),
        dir: dir.clone(),
        dir_locked: true,
        installed: None,
        newer: None,
        tasks: product.tasks,
        launch: false,
        delete_data: args.delete_data,
        auto: args.silent,
        updating: false,
        suppress: args.suppress_msgboxes,
        uninstaller: None,
        check_dir: Box::new(|_| Ok(())),
        running: Box::new(|dir| !process::running_in(dir).is_empty()),
        launch_program: Box::new(|_| {}),
        log_path: log::path(),
    };
    let (work_env, work_product, work_name) = (env.clone(), product.clone(), name.clone());
    let work: ui::Work = Box::new(move |choice: ui::Choice, report: &dyn Report| {
        let _com = shortcut::Com::start();
        uninstall::uninstall(&work_env, &work_product, &choice.dir, choice.delete_data, report)
            .map(|left| ui::Done::Uninstalled { warnings: leftovers(&left) })
            .map_err(|failure| {
                log!("failed with exit code {}: {failure:?}", failure.code());
                (failure.code(), failure_message(&failure, &work_name))
            })
    });
    match ui::run(view, work) {
        Ok(outcome) => outcome.code,
        Err(problem) => {
            log!("the uninstall window could not be shown: {problem}");
            let delete_data = if args.silent {
                args.delete_data
            } else {
                match dialogs::confirm_uninstall(&name, &label) {
                    None => return 2,
                    Some(delete) => delete || args.delete_data,
                }
            };
            match uninstall::uninstall(env, &product, &dir, delete_data, &Quiet) {
                Ok(_) => {
                    if !args.silent {
                        dialogs::info(&format!("{name}{}", tr(" 已卸载。", " was uninstalled.")));
                    }
                    0
                }
                Err(failure) => {
                    if show {
                        show_failure(&failure, &name, true);
                    }
                    failure.code()
                }
            }
        }
    }
}
