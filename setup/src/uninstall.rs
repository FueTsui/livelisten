//! Uninstalling (REL-05). Settings and user data always stay.
//! Only files from the install record, the shortcuts this installer created, and a Run
//! value that starts exactly this installation are removed. A running uninstaller cannot
//! delete itself, so a copy in the temporary folder waits until it has exited and then
//! removes it and the folder, if empty.

use std::fs;
use std::io;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, WAIT_FAILED};
use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, DETACHED_PROCESS, INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};

use crate::install::{self, CLOSE_TIMEOUT, Failure, Report, Stage};
use crate::log;
use crate::manifest::{Product, RECORD_FILE, UNINSTALLER};
use crate::paths::Env;
use crate::process::{self, Quit};
use crate::registry;
use crate::shortcut;
use crate::text::tr;

/// The switch that runs the temporary copy: `--cleanup <folder> <uninstaller pid>`.
pub const CLEANUP_SWITCH: &str = "--cleanup";

/// What could not be removed.
#[derive(Debug, Default, PartialEq)]
pub struct Leftovers {
    pub paths: Vec<PathBuf>,
}

fn system_dir() -> PathBuf {
    let mut buffer = [0u16; 260];
    // SAFETY: `buffer` receives the system folder path.
    let len = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    PathBuf::from(String::from_utf16_lossy(&buffer[..len.min(buffer.len())]))
}

/// Starts a copy of this uninstaller in the temporary folder that removes the original and
/// the empty folder once this process has exited, however long its window stays open.
fn schedule_removal(product: &Product, dir: &Path) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let temp = std::env::temp_dir();
    let helper = temp.join(format!("{}-Uninstall-{}.exe", product.id, std::process::id()));
    fs::copy(&exe, &helper)?;
    Command::new(&helper)
        .arg(CLEANUP_SWITCH)
        .arg(dir)
        .arg(std::process::id().to_string())
        .current_dir(&temp)
        .creation_flags(CREATE_NO_WINDOW.0 | DETACHED_PROCESS.0)
        .spawn()
        .map(drop)
}

/// Runs in the temporary copy: waits for the uninstaller `pid`, then removes the uninstaller
/// from `dir` and `dir` itself if it is empty, and finally lets cmd.exe delete this copy.
/// It acts only after a finished uninstall (no install record left) and only from the
/// temporary folder.
pub fn cleanup(dir: &Path, pid: u32) {
    let exe = std::env::current_exe().unwrap_or_default();
    if !exe.parent().is_some_and(|p| process::same_path(p, &std::env::temp_dir())) || dir.join(RECORD_FILE).exists() {
        return;
    }
    // SAFETY: waits on a handle of the uninstaller, which started this copy and is still
    // running or has just exited; the handle is closed right after.
    unsafe {
        if let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            if WaitForSingleObject(handle, INFINITE) == WAIT_FAILED {
                std::thread::sleep(Duration::from_secs(2));
            }
            let _ = CloseHandle(handle);
        }
    }
    let uninstaller = dir.join(UNINSTALLER);
    for _ in 0..120 {
        if !uninstaller.exists() || fs::remove_file(&uninstaller).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = fs::remove_dir(dir);
    // A running program cannot delete itself; cmd.exe removes this copy after it exits.
    // The path reaches cmd.exe through an environment variable, which it expands once
    // inside quotes, so no character in it can act as a command.
    let system = system_dir();
    let _ = Command::new(system.join("cmd.exe"))
        .raw_arg(r#"/d /s /c ""%SETUP_PING%" -n 3 127.0.0.1 >nul & del /f /q "%SETUP_FILE%"""#)
        .env("SETUP_PING", system.join("PING.EXE"))
        .env("SETUP_FILE", &exe)
        .current_dir(&system)
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn();
}

/// Removes the installation in `dir`, preserving all user data.
pub fn uninstall(env: &Env, product: &Product, dir: &Path, report: &dyn Report) -> Result<Leftovers, Failure> {
    if crate::paths::in_data_dir(dir, env, product) {
        return Err(Failure::Invalid(tr("不能卸载数据目录。", "Cannot uninstall a data folder.").into()));
    }
    let Some(record) = install::read_record(dir, &product.id) else {
        return Err(Failure::Invalid(
            tr(
                "找不到安装记录，无法确定要删除哪些文件。",
                "The install record is missing, so the files to remove are unknown.",
            )
            .into(),
        ));
    };
    log!("uninstalling {} {} from {}", product.id, record.version, dir.display());
    report.progress(Stage::Closing, 0, 1);
    let env_vars = env.child_environment(product);
    let quit = Quit { exe: install::main_exe(dir, product), args: &product.quit_args, env: &env_vars };
    if let Err(names) = process::close_all(dir, Some(quit), CLOSE_TIMEOUT) {
        return Err(Failure::InUse(names));
    }
    report.progress(Stage::Replacing, 0, 1);
    let mut left = Leftovers::default();
    for link in record.shortcuts.iter().map(PathBuf::from) {
        let ours = link.exists() && shortcut::target(&link).is_some_and(|t| process::inside(&t, dir));
        if ours && fs::remove_file(&link).is_err() {
            left.paths.push(link);
        }
    }
    let registry = env.registry();
    if registry.run(&product.id).ok().flatten().is_some_and(|v| install::run_points_here(&v, dir, product)) {
        let _ = registry.set_run(&product.id, None);
    }
    let total = record.files.len().max(1) as u64;
    for (index, file) in record.files.iter().enumerate() {
        let path = dir.join(file.replace('/', "\\"));
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) => left.paths.push(path),
        }
        report.progress(Stage::Replacing, index as u64 + 1, total);
    }
    install::remove_empty_folders(dir, &record.files);
    report.progress(Stage::Finishing, 0, 1);
    let _ = fs::remove_file(dir.join(RECORD_FILE));
    let registered = registry.entry(&product.id).ok().flatten().and_then(|e| registry::text(&e, "InstallLocation"));
    if registered.is_some_and(|location| process::same_path(Path::new(&location), dir)) {
        let _ = registry.delete_entry(&product.id);
    }
    let uninstaller = dir.join(UNINSTALLER);
    let running_from_dir = std::env::current_exe().is_ok_and(|exe| process::same_path(&exe, &uninstaller));
    if running_from_dir {
        if let Err(e) = schedule_removal(product, dir) {
            log!("could not schedule removing the uninstaller: {e}");
            left.paths.push(uninstaller);
        }
    } else {
        let _ = fs::remove_file(&uninstaller);
        let _ = fs::remove_dir(dir);
    }
    log!("uninstalled; {} items left", left.paths.len());
    Ok(left)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::tests::Test;
    use crate::install::{Quiet, install};
    use crate::manifest::{Base, DataDir};

    #[test]
    fn removes_the_installation_and_keeps_data() {
        let _com = shortcut::Com::start();
        let t = Test::new("uninstall", &[("Demo.exe", b"v1".to_vec()), ("lib/a.dll", b"a".to_vec())]);
        let mut product = crate::manifest::tests::product();
        product.data_dirs = vec![DataDir { base: Base::RoamingAppData, path: "Demo".into() }];
        let mut plan = t.plan(None);
        plan.tasks.startup = true;
        install(&t.env, &mut t.package(), &plan, &Quiet).unwrap();
        let data = t.env.roaming_app_data.join("Demo");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("settings.json"), b"{}").unwrap();
        fs::write(data.join("cards.json"), b"{\"cards\":[]}").unwrap();
        fs::create_dir_all(data.join("随手记")).unwrap();
        fs::write(data.join("随手记").join("daily.md"), b"note document").unwrap();
        // Another installation's Run value and shortcut must survive.
        let registry = t.env.registry();
        let foreign_link = t.env.desktop.join("Other.lnk");
        shortcut::create(&foreign_link, &t.root.join("Other.exe"), "other").unwrap();

        let dir = plan.dir.clone();
        assert_eq!(uninstall(&t.env, &product, &dir, &Quiet), Ok(Leftovers::default()));
        assert!(!dir.exists(), "program folder removed");
        assert!(data.join("settings.json").is_file(), "settings kept by default");
        assert_eq!(registry.entry("Demo").unwrap(), None);
        assert_eq!(registry.run("Demo").unwrap(), None);
        assert!(foreign_link.is_file());
        assert_eq!(fs::read_dir(&t.env.start_menu).unwrap().count(), 0, "own shortcut removed");

        install(&t.env, &mut t.package(), &t.plan(None), &Quiet).unwrap();
        fs::write(dir.join("mine.txt"), b"x").unwrap();
        fs::create_dir(dir.join(".setup-personal")).unwrap();
        fs::write(dir.join(".setup-personal/daily.md"), b"custom document").unwrap();
        registry.set_run("Demo", Some("\"C:\\Elsewhere\\Demo.exe\" --background")).unwrap();
        uninstall(&t.env, &product, &dir, &Quiet).unwrap();
        assert_eq!(fs::read(data.join("settings.json")).unwrap(), b"{}");
        assert_eq!(fs::read(data.join("cards.json")).unwrap(), b"{\"cards\":[]}");
        assert_eq!(fs::read(data.join("随手记").join("daily.md")).unwrap(), b"note document");
        assert_eq!(fs::read(dir.join(".setup-personal/daily.md")).unwrap(), b"custom document");
        assert!(registry.run("Demo").unwrap().is_some(), "a Run value of another installation stays");
        assert!(dir.join("mine.txt").is_file() && !dir.join("Demo.exe").exists(), "files the user added stay");
    }

    #[test]
    fn refuses_a_folder_without_an_install_record() {
        let t = Test::new("no-record", &[("Demo.exe", b"v1".to_vec())]);
        let dir = t.root.join("NotInstalled");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("keep.txt"), b"x").unwrap();
        let product = crate::manifest::tests::product();
        assert!(matches!(uninstall(&t.env, &product, &dir, &Quiet), Err(Failure::Invalid(_))));
        assert!(dir.join("keep.txt").exists());
    }

    #[test]
    fn cleanup_acts_only_from_the_temporary_folder_after_an_uninstall() {
        let t = Test::new("cleanup", &[("Demo.exe", b"v1".to_vec())]);
        let dir = t.root.join("App");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(UNINSTALLER), b"x").unwrap();
        // The test runner does not live in the temporary folder, so nothing is removed.
        cleanup(&dir, std::process::id());
        assert!(dir.join(UNINSTALLER).is_file());
    }
}
