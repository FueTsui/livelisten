//! Installing and upgrading (REL-01, REL-02, REL-03).
//!
//! Every file is extracted into a staging folder inside the install folder and checked
//! against its SHA-256 first. Only then are running programs closed and the files swapped
//! in one by one by renaming, each previous file set aside. If a swap or the registration
//! fails, every change is undone, so the previous version still starts. Shortcuts and
//! start at login are applied last; a failure there is reported but never undoes a
//! finished installation. Settings, user data and credentials live elsewhere and are
//! never touched, and files the user added to the folder stay where they are.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::core::HSTRING;

use crate::hash;
use crate::log;
use crate::manifest::{FORMAT, Packed, Product, RECORD_FILE, Record, Role, Tasks, UNINSTALLER};
use crate::paths::{self, Env};
use crate::payload::{Package, PackageError, Writer};
use crate::process::{self, Quit};
use crate::registry::{self, Entry, Registry, Value};
use crate::shortcut;
use crate::text::{self, tr};

/// How long programs get to exit after being asked, before ending them is considered.
pub const CLOSE_TIMEOUT: Duration = Duration::from_secs(15);
/// `/UPDATE` waits longer and never ends programs: an unattended update must not lose work.
pub const UPDATE_CLOSE_TIMEOUT: Duration = Duration::from_secs(120);
/// Staging folders inside the install folder start with this.
const STAGING_PREFIX: &str = ".setup-";

#[derive(Debug, PartialEq)]
pub enum Failure {
    /// Exit code 1: damaged package, invalid arguments or an unusable folder.
    Invalid(String),
    /// Exit code 2.
    Cancelled,
    /// Exit code 3: the previous state is kept or was restored.
    RolledBack(String),
    /// Exit code 7: these programs kept running; nothing was replaced.
    InUse(Vec<String>),
}

impl Failure {
    pub fn code(&self) -> i32 {
        match self {
            Failure::Invalid(_) => 1,
            Failure::Cancelled => 2,
            Failure::RolledBack(_) => 3,
            Failure::InUse(_) => 7,
        }
    }
}

impl From<PackageError> for Failure {
    fn from(e: PackageError) -> Self {
        match e {
            PackageError::Damaged(reason) => Failure::Invalid(format!("{} ({reason})", damaged_text())),
            PackageError::Io(e) => Failure::RolledBack(e.to_string()),
        }
    }
}

pub fn damaged_text() -> &'static str {
    tr(
        "安装包已损坏或不完整，没有写入任何文件。请重新下载安装包，并用 SHA256SUMS.txt 核对哈希。",
        "The installer is damaged or incomplete; nothing was written. Download it again and check its hash against SHA256SUMS.txt.",
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Extracting,
    Closing,
    Replacing,
    Finishing,
}

/// Progress and questions while installing or uninstalling.
pub trait Report: Sync {
    fn progress(&self, stage: Stage, done: u64, total: u64);
    fn cancelled(&self) -> bool;
    /// These programs did not exit in time; `true` allows ending them.
    fn force_close(&self, names: &[String]) -> bool;
}

/// Silent runs: no cancelling, and programs that ignore the quit request are ended.
pub struct Quiet;

impl Report for Quiet {
    fn progress(&self, _: Stage, _: u64, _: u64) {}
    fn cancelled(&self) -> bool {
        false
    }
    fn force_close(&self, names: &[String]) -> bool {
        log!("ending after the timeout: {}", names.join(", "));
        true
    }
}

/// `/UPDATE`: no cancelling, and programs that do not exit stop the update (exit code 7).
pub struct Unattended;

impl Report for Unattended {
    fn progress(&self, _: Stage, _: u64, _: u64) {}
    fn cancelled(&self) -> bool {
        false
    }
    fn force_close(&self, names: &[String]) -> bool {
        log!("still running after the update timeout, not ended: {}", names.join(", "));
        false
    }
}

/// An installation found on this computer.
#[derive(Clone, Debug)]
pub struct Existing {
    pub dir: PathBuf,
    pub record: Record,
    /// Which options are in effect now, kept when upgrading.
    pub tasks: Tasks,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub dir: PathBuf,
    pub tasks: Tasks,
    pub launch: bool,
    /// `/UPDATE`: start the program again only if it was running.
    pub relaunch_if_running: bool,
    pub close_timeout: Duration,
    pub existing: Option<Existing>,
}

/// The result of a finished installation.
#[derive(Debug, PartialEq)]
pub struct Installed {
    pub dir: PathBuf,
    /// The version this installation replaced.
    pub previous: Option<String>,
    /// Options that could not be applied; the installation itself is complete.
    pub warnings: Vec<String>,
    pub launched: bool,
}

pub fn read_record(dir: &Path, id: &str) -> Option<Record> {
    let record: Record = serde_json::from_slice(&fs::read(dir.join(RECORD_FILE)).ok()?).ok()?;
    (record.format == FORMAT && record.id == id).then_some(record)
}

pub fn main_exe(dir: &Path, product: &Product) -> PathBuf {
    dir.join(product.exe.replace('/', "\\"))
}

/// The Run value this installer writes for start at login.
pub fn run_command(dir: &Path, product: &Product) -> String {
    let exe = main_exe(dir, product);
    if product.background_args.is_empty() {
        format!("\"{}\"", exe.display())
    } else {
        format!("\"{}\" {}", exe.display(), product.background_args)
    }
}

/// True when the Run value starts exactly the program of this installation.
pub fn run_points_here(value: &str, dir: &Path, product: &Product) -> bool {
    registry::run_target(value).is_some_and(|program| process::same_path(&program, &main_exe(dir, product)))
}

fn shortcut_paths(env: &Env, product: &Product) -> (PathBuf, PathBuf) {
    let file = format!("{}.lnk", text::name(&product.name));
    (env.start_menu.join(&file), env.desktop.join(file))
}

/// Finds the installation: the "Installed apps" entry first, then the requested folder,
/// then the default folder. A folder counts only if it holds this product's install record.
pub fn find_existing(
    env: &Env,
    registry: &Registry,
    product: &Product,
    requested: Option<&Path>,
) -> Result<Option<Existing>, Failure> {
    let registered = registry
        .entry(&product.id)
        .ok()
        .flatten()
        .and_then(|entry| registry::text(&entry, "InstallLocation"))
        .map(PathBuf::from)
        .filter(|dir| read_record(dir, &product.id).is_some());
    if let (Some(installed), Some(requested)) = (&registered, requested) {
        if !process::same_path(installed, requested) {
            return Err(Failure::Invalid(
                format!("{} {}", tr("已经安装在", "Already installed in"), installed.display())
                    + tr("。要换位置，请先卸载。", ". Uninstall it first to use another folder."),
            ));
        }
    }
    let default = env.default_dir(product);
    let dir = registered
        .or_else(|| requested.map(Path::to_path_buf).filter(|d| read_record(d, &product.id).is_some()))
        .or_else(|| Some(default).filter(|d| requested.is_none() && read_record(d, &product.id).is_some()));
    let Some(dir) = dir else { return Ok(None) };
    let record = read_record(&dir, &product.id).expect("checked above");
    let has_link = |base: &Path| record.shortcuts.iter().map(Path::new).any(|s| s.starts_with(base) && s.exists());
    let tasks = Tasks {
        start_menu: has_link(&env.start_menu),
        desktop: has_link(&env.desktop),
        startup: registry.run(&product.id).ok().flatten().is_some_and(|v| run_points_here(&v, &dir, product)),
    };
    Ok(Some(Existing { dir, record, tasks }))
}

/// A folder for a new installation: a full path that is not a shared folder and is empty
/// or does not exist yet.
pub fn check_new_dir(dir: &Path, env: &Env, packed: &Packed) -> Result<(), String> {
    let text = dir.to_string_lossy();
    if !dir.is_absolute() || !text.as_bytes().get(1).is_some_and(|&b| b == b':') {
        return Err(
            tr("请填写完整路径，例如 D:\\Apps\\LiveListen。", "Use a full path such as D:\\Apps\\LiveListen.").into()
        );
    }
    if paths::protected(dir, env) {
        return Err(tr(
            "不能直接安装到这个系统或用户文件夹，请在它下面新建一个文件夹。",
            "This system or user folder cannot be used directly; choose a new folder inside it.",
        )
        .into());
    }
    let longest = packed.files.iter().map(|f| f.path.len()).max().unwrap_or(0);
    // The staging folder adds ".setup-<pid>\new\" while installing.
    if text.chars().count() + longest + 2 + 24 > 259 {
        return Err(
            tr("安装路径太长，请选一个更短的位置。", "The folder path is too long; choose a shorter one.").into()
        );
    }
    match fs::read_dir(dir).map(|mut entries| entries.next().is_some()) {
        Ok(true) => Err(tr(
            "这个文件夹不是空的，也不是已有的安装位置。请选择空文件夹或新文件夹。",
            "This folder is not empty and holds no installation. Choose an empty or new folder.",
        )
        .into()),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) if dir.is_file() => Err(tr("这个位置是一个文件。", "This location is a file.").into()),
        Err(e) => Err(e.to_string()),
    }
}

fn free_space(dir: &Path) -> Option<u64> {
    let mut free = 0u64;
    // SAFETY: `free` receives the bytes available to this user on the volume of `dir`.
    unsafe { GetDiskFreeSpaceExW(&HSTRING::from(dir.as_os_str()), Some(&mut free), None, None) }.ok()?;
    Some(free)
}

/// Renames, retrying briefly: a virus scan may hold a newly written file for a moment.
fn rename_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut last = None;
    for attempt in 0..5 {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => {
                log!("rename {} -> {} failed (attempt {}): {e}", from.display(), to.display(), attempt + 1);
                last = Some(e);
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
    Err(last.unwrap())
}

/// Why a file could not be replaced, in words the user can act on.
fn replace_failed(path: &Path, e: &io::Error) -> String {
    let busy = matches!(e.raw_os_error(), Some(5 | 32 | 33));
    let hint = if busy {
        tr(
            "这个文件正被其他程序使用（例如杀毒扫描或打开它的窗口）。请关闭后重试。",
            "This file is in use by another program (a virus scan or a window that has it open). Close it and try again.",
        )
    } else {
        tr("无法替换这个文件。", "This file could not be replaced.")
    };
    format!("{hint} {}: {e}", path.display())
}

fn remove_tree(dir: &Path) {
    for _ in 0..5 {
        match fs::remove_dir_all(dir) {
            Ok(()) => return,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(_) => std::thread::sleep(Duration::from_millis(300)),
        }
    }
    log!("could not remove {}", dir.display());
}

/// Removes staging folders a crashed run left inside the install folder.
pub fn clear_stale_staging(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(STAGING_PREFIX) && entry.path().is_dir() {
            log!("removing a leftover {name}");
            remove_tree(&entry.path());
        }
    }
}

/// Removes the folders of `files` (relative, `/` separated) that are empty now.
pub fn remove_empty_folders(dir: &Path, files: &[String]) {
    let mut folders = Vec::new();
    for file in files {
        let path = dir.join(file.replace('/', "\\"));
        let mut parent = path.parent();
        while let Some(folder) = parent.filter(|p| *p != dir && p.starts_with(dir)) {
            folders.push(folder.to_path_buf());
            parent = folder.parent();
        }
    }
    folders.sort_by_key(|f| std::cmp::Reverse(f.components().count()));
    folders.dedup();
    for folder in folders {
        let _ = fs::remove_dir(folder); // only empty folders go
    }
}

/// Writes the uninstaller: this setup program with a payload that carries no files.
pub fn write_uninstaller(package: &mut Package, path: &Path) -> Result<(), Failure> {
    let stub = package.stub().map_err(|e| Failure::RolledBack(e.to_string()))?;
    let mut manifest = package.manifest.clone();
    manifest.role = Role::Uninstaller;
    manifest.files.clear();
    Writer::create(path, &stub)
        .and_then(|writer| writer.finish(&manifest))
        .map_err(|e| Failure::RolledBack(format!("{}: {e}", path.display())))
}

fn extract(package: &mut Package, staging: &Path, report: &dyn Report) -> Result<(), Failure> {
    let files = package.manifest.files.clone();
    let total = package.manifest.total_size().max(1);
    let mut done = 0u64;
    for entry in &files {
        if report.cancelled() {
            return Err(Failure::Cancelled);
        }
        let data = package.read(entry)?;
        let target = staging.join(entry.path.replace('/', "\\"));
        let write = || -> io::Result<()> {
            if let Some(dir) = target.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(&target, &data)?;
            if hash::hex(&hash::file_digest(&target)?) != entry.sha256 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "written file does not match its hash"));
            }
            Ok(())
        };
        write().map_err(|e| Failure::RolledBack(format!("{}: {e}", entry.path)))?;
        done += entry.size;
        report.progress(Stage::Extracting, done, total);
    }
    Ok(())
}

/// Files swapped into the install folder, with what they replaced.
struct Transaction {
    /// Where replaced files are set aside.
    old: PathBuf,
    /// (installed path, the previous file set aside, whether the new file is in place)
    steps: Vec<(PathBuf, Option<PathBuf>, bool)>,
}

impl Transaction {
    fn set_aside(&self, path: &Path, relative: &str) -> io::Result<Option<PathBuf>> {
        if !path.exists() {
            return Ok(None);
        }
        let backup = self.old.join(relative.replace('/', "\\"));
        if let Some(parent) = backup.parent() {
            fs::create_dir_all(parent)?;
        }
        rename_retry(path, &backup)?;
        Ok(Some(backup))
    }

    /// Puts `new` at `dir/relative`, keeping the previous file for a rollback.
    fn replace(&mut self, dir: &Path, relative: &str, new: &Path) -> Result<(), String> {
        let path = dir.join(relative.replace('/', "\\"));
        let backup = self.set_aside(&path, relative).map_err(|e| replace_failed(&path, &e))?;
        self.steps.push((path.clone(), backup, false));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| replace_failed(&path, &e))?;
        }
        rename_retry(new, &path).map_err(|e| replace_failed(&path, &e))?;
        self.steps.last_mut().expect("pushed above").2 = true;
        Ok(())
    }

    fn remove(&mut self, dir: &Path, relative: &str) -> Result<(), String> {
        let path = dir.join(relative.replace('/', "\\"));
        if let Some(backup) = self.set_aside(&path, relative).map_err(|e| replace_failed(&path, &e))? {
            self.steps.push((path, Some(backup), false));
        }
        Ok(())
    }

    fn rollback(&mut self) {
        log!("rolling back {} file changes", self.steps.len());
        for (path, backup, placed) in self.steps.drain(..).rev() {
            if placed {
                let _ = fs::remove_file(&path);
            }
            if let Some(backup) = backup {
                if let Err(e) = rename_retry(&backup, &path) {
                    log!("could not restore {}: {e}", path.display());
                }
            }
        }
    }
}

/// The "Installed apps" entry.
fn register(registry: &Registry, packed: &Packed, dir: &Path) -> io::Result<()> {
    let product = &packed.product;
    let exe = main_exe(dir, product);
    let uninstaller = dir.join(UNINSTALLER);
    let mut entry = Entry::new();
    let mut put = |name: &str, value: String| {
        entry.insert(name.to_string(), Value::Str(value));
    };
    put("DisplayName", text::name(&product.name).to_string());
    put("DisplayVersion", packed.version.clone());
    put("Publisher", product.publisher.clone());
    put("InstallLocation", dir.display().to_string());
    put("DisplayIcon", format!("{},0", exe.display()));
    put("UninstallString", format!("\"{}\"", uninstaller.display()));
    put("QuietUninstallString", format!("\"{}\" /VERYSILENT", uninstaller.display()));
    put("InstallDate", log::today());
    entry.insert("NoModify".into(), Value::Dword(1));
    entry.insert("NoRepair".into(), Value::Dword(1));
    entry.insert("EstimatedSize".into(), Value::Dword((packed.total_size() / 1024).min(u32::MAX as u64) as u32));
    registry.set_entry(&product.id, &entry)
}

/// Shortcuts and start at login; returns what could not be applied.
fn apply_tasks(env: &Env, registry: &Registry, packed: &Packed, plan: &Plan) -> Vec<String> {
    let product = &packed.product;
    let dir = &plan.dir;
    let exe = main_exe(dir, product);
    let mut warnings = Vec::new();
    let (start, desktop) = shortcut_paths(env, product);
    for (link, wanted) in [(&start, plan.tasks.start_menu), (&desktop, plan.tasks.desktop)] {
        let ours = || shortcut::target(link).is_some_and(|t| process::inside(&t, dir));
        if wanted {
            if let Err(e) = shortcut::create(link, &exe, text::name(&product.name)) {
                warnings.push(format!(
                    "{} {}: {e}",
                    tr("无法创建快捷方式", "Could not create the shortcut"),
                    link.display()
                ));
            }
        } else if link.exists() && ours() {
            let _ = fs::remove_file(link);
        }
    }
    // Shortcuts of the previous installation under another name.
    if let Some(existing) = &plan.existing {
        for old in existing.record.shortcuts.iter().map(PathBuf::from) {
            if old != start && old != desktop && shortcut::target(&old).is_some_and(|t| process::inside(&t, dir)) {
                let _ = fs::remove_file(&old);
            }
        }
    }
    let run = if plan.tasks.startup {
        registry.set_run(&product.id, Some(&run_command(dir, product)))
    } else if registry.run(&product.id).ok().flatten().is_some_and(|v| run_points_here(&v, dir, product)) {
        registry.set_run(&product.id, None)
    } else {
        Ok(())
    };
    if let Err(e) = run {
        warnings.push(format!("{}: {e}", tr("无法设置开机自动启动", "Could not set start at login")));
    }
    warnings
}

/// Installs or upgrades according to `plan`.
pub fn install(env: &Env, package: &mut Package, plan: &Plan, report: &dyn Report) -> Result<Installed, Failure> {
    let packed = package.manifest.clone();
    let product = &packed.product;
    let dir = &plan.dir;
    if dir.parent().is_none() {
        return Err(Failure::Invalid(tr("安装位置无效。", "Invalid install folder.").into()));
    }
    log!("installing {} {} into {}", product.id, packed.version, dir.display());
    let created = !dir.exists();
    fs::create_dir_all(dir).map_err(|e| Failure::RolledBack(format!("{}: {e}", dir.display())))?;
    // Leaves nothing behind when an installation into a new folder stops early.
    let abandon = |staging: &Path| {
        remove_tree(staging);
        if created {
            let _ = fs::remove_dir(dir);
        }
    };
    clear_stale_staging(dir);
    let need = packed.total_size() + (64 << 20);
    if let Some(free) = free_space(dir).filter(|&free| free < need) {
        if created {
            let _ = fs::remove_dir(dir);
        }
        return Err(Failure::RolledBack(format!(
            "{} {} MB, {} {} MB",
            tr("磁盘空间不足：需要", "Not enough disk space: needs"),
            need >> 20,
            tr("可用", "available"),
            free >> 20
        )));
    }
    let staging = dir.join(format!("{STAGING_PREFIX}{}", std::process::id()));
    remove_tree(&staging);
    let new = staging.join("new");
    fs::create_dir_all(&new).map_err(|e| Failure::RolledBack(format!("{}: {e}", staging.display())))?;
    let previous = read_record(dir, &product.id);
    let registry = env.registry();
    let (start, desktop) = shortcut_paths(env, product);
    let links: Vec<PathBuf> = [(start, plan.tasks.start_menu), (desktop, plan.tasks.desktop)]
        .into_iter()
        .filter_map(|(link, on)| on.then_some(link))
        .collect();

    // 1. Extract and check everything before touching installed files.
    let prepared = (|| -> Result<(), Failure> {
        extract(package, &new, report)?;
        write_uninstaller(package, &new.join(UNINSTALLER))?;
        let record = Record {
            format: FORMAT,
            id: product.id.clone(),
            version: packed.version.clone(),
            files: packed.files.iter().map(|f| f.path.clone()).collect(),
            shortcuts: links.iter().map(|l| l.display().to_string()).collect(),
        };
        let json = serde_json::to_vec_pretty(&record).map_err(|e| Failure::RolledBack(e.to_string()))?;
        fs::write(new.join(RECORD_FILE), json).map_err(|e| Failure::RolledBack(e.to_string()))
    })();
    if let Err(failure) = prepared {
        abandon(&staging);
        return Err(failure);
    }

    report.progress(Stage::Closing, 0, 1);
    let env_vars = env.child_environment(product);
    let quit = Quit { exe: main_exe(dir, product), args: &product.quit_args, env: &env_vars };
    let was_running = match process::close_all(dir, Some(quit), plan.close_timeout, &|names| report.force_close(names))
    {
        Ok(running) => running,
        Err(names) => {
            abandon(&staging);
            return Err(Failure::InUse(names));
        }
    };

    // 2. Swap the files in and register; undo every change on failure.
    report.progress(Stage::Replacing, 0, 1);
    let before = registry.entry(&product.id).unwrap_or(None);
    let mut transaction = Transaction { old: staging.join("old"), steps: Vec::new() };
    let mut shipped: Vec<String> = packed.files.iter().map(|f| f.path.clone()).collect();
    shipped.extend([UNINSTALLER.to_string(), RECORD_FILE.to_string()]);
    let obsolete: Vec<String> = {
        let kept: HashSet<String> = shipped.iter().map(|p| p.to_lowercase()).collect();
        let old = previous.as_ref().map_or(&[][..], |r| &r.files[..]);
        old.iter().filter(|p| !kept.contains(&p.to_lowercase())).cloned().collect()
    };
    let committed = (|| -> Result<(), String> {
        for path in &shipped {
            transaction.replace(dir, path, &new.join(path.replace('/', "\\")))?;
        }
        for path in &obsolete {
            transaction.remove(dir, path)?;
        }
        report.progress(Stage::Finishing, 0, 1);
        register(&registry, &packed, dir).map_err(|e| format!("{}: {e}", tr("无法登记安装信息", "Could not register")))
    })();
    if let Err(reason) = committed {
        log!("replacing failed: {reason}; rolling back");
        transaction.rollback();
        let _ = match &before {
            Some(entry) => registry.set_entry(&product.id, entry),
            None => registry.delete_entry(&product.id),
        };
        abandon(&staging);
        return Err(Failure::RolledBack(format!(
            "{reason}\n{}",
            tr("已恢复原有文件，之前的版本仍可使用。", "The previous files were restored and still work.")
        )));
    }
    remove_tree(&staging);
    remove_empty_folders(dir, &obsolete);

    // 3. Options never undo a finished installation.
    let warnings = apply_tasks(env, &registry, &packed, plan);
    for warning in &warnings {
        log!("warning: {warning}");
    }
    log!("installed {} files", packed.files.len());

    let mut launched = false;
    if plan.launch || (plan.relaunch_if_running && was_running) {
        match process::launch(&main_exe(dir, product), &env_vars) {
            Ok(()) => launched = true,
            Err(e) => log!("could not start the program: {e}"),
        }
    }
    Ok(Installed { dir: dir.clone(), previous: previous.map(|r| r.version), warnings, launched })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::manifest::Names;
    use crate::payload::tests::build;
    use std::sync::atomic::{AtomicBool, Ordering};

    pub(crate) struct Test {
        pub root: PathBuf,
        pub env: Env,
        pub setup: PathBuf,
    }

    impl Test {
        pub fn new(name: &str, files: &[(&str, Vec<u8>)]) -> Test {
            let root = std::env::temp_dir().join(format!("setup-install-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            let setup = root.join("setup.exe");
            build(&setup, files);
            Test { env: Env::for_test(&root), root, setup }
        }

        pub fn package(&self) -> Package {
            Package::open(&self.setup, &mut |_, _| {}).unwrap()
        }

        pub fn plan(&self, existing: Option<Existing>) -> Plan {
            let dir = existing.as_ref().map(|e| e.dir.clone()).unwrap_or_else(|| self.env.default_dir(&product()));
            let tasks = existing.as_ref().map(|e| e.tasks).unwrap_or_default();
            Plan { dir, tasks, launch: false, relaunch_if_running: false, close_timeout: CLOSE_TIMEOUT, existing }
        }
    }

    impl Drop for Test {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn product() -> Product {
        crate::manifest::tests::product()
    }

    fn staging_left(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(STAGING_PREFIX))
            .collect()
    }

    struct Cancel(AtomicBool);

    impl Report for Cancel {
        fn progress(&self, _: Stage, _: u64, _: u64) {}
        fn cancelled(&self) -> bool {
            self.0.load(Ordering::Relaxed)
        }
        fn force_close(&self, _: &[String]) -> bool {
            false
        }
    }

    #[test]
    fn installs_upgrades_and_keeps_options_and_added_files() {
        let _com = shortcut::Com::start();
        let t = Test::new("upgrade", &[("Demo.exe", b"v1".to_vec()), ("lib/old.dll", b"old".to_vec())]);
        let mut package = t.package();
        let mut plan = t.plan(None);
        plan.tasks.startup = true;
        let done = install(&t.env, &mut package, &plan, &Quiet).unwrap();
        assert_eq!((done.previous, done.warnings.len(), done.launched), (None, 0, false));
        let dir = plan.dir.clone();
        assert_eq!(fs::read(dir.join("Demo.exe")).unwrap(), b"v1");
        assert!(dir.join(UNINSTALLER).is_file() && dir.join(RECORD_FILE).is_file());
        let registry = t.env.registry();
        let entry = registry.entry("Demo").unwrap().unwrap();
        assert_eq!(registry::text(&entry, "InstallLocation"), Some(dir.display().to_string()));
        assert!(run_points_here(&registry.run("Demo").unwrap().unwrap(), &dir, &product()));
        let start_link =
            t.env.start_menu.join(format!("{}.lnk", text::name(&Names { zh: "演示".into(), en: "Demo".into() })));
        assert!(start_link.is_file());
        // The uninstaller is a valid package that carries no files.
        let uninstaller = Package::open(&dir.join(UNINSTALLER), &mut |_, _| {}).unwrap();
        assert_eq!((uninstaller.manifest.role, uninstaller.manifest.files.len()), (Role::Uninstaller, 0));
        drop(uninstaller);

        fs::write(dir.join("notes.txt"), b"mine").unwrap();
        drop(package);
        let t2 = Test::new("upgrade-v2", &[("Demo.exe", b"v2".to_vec()), ("new.dll", b"new".to_vec())]);
        let existing = find_existing(&t.env, &registry, &product(), None).unwrap().unwrap();
        assert_eq!(existing.dir, dir);
        assert_eq!(existing.tasks, Tasks { start_menu: true, desktop: false, startup: true });
        let mut plan = t.plan(Some(existing));
        plan.tasks.desktop = true;
        let mut package = t2.package();
        package.manifest.product.name = Names { zh: "DemoApp".into(), en: "DemoApp".into() };
        package.manifest.product.publisher = "New Publisher".into();
        let done = install(&t.env, &mut package, &plan, &Quiet).unwrap();
        assert_eq!(done.previous.as_deref(), Some("1.0.0"));
        let entry = registry.entry("Demo").unwrap().unwrap();
        assert_eq!(registry::text(&entry, "DisplayName").as_deref(), Some("DemoApp"));
        assert_eq!(registry::text(&entry, "Publisher").as_deref(), Some("New Publisher"));
        assert!(t.env.start_menu.join("DemoApp.lnk").is_file());
        assert!(!start_link.exists(), "the old shortcut name is removed");
        assert_eq!(fs::read(dir.join("Demo.exe")).unwrap(), b"v2");
        assert!(!dir.join("lib").exists(), "files dropped by the new version and their empty folders are removed");
        assert_eq!(fs::read(dir.join("notes.txt")).unwrap(), b"mine", "files the user added stay");
        assert!(run_points_here(&registry.run("Demo").unwrap().unwrap(), &dir, &product()));
        assert!(t.env.desktop.read_dir().unwrap().count() == 1, "the new option is applied");
        assert!(staging_left(&dir).is_empty());
        assert_eq!(fs::read_dir(dir.parent().unwrap()).unwrap().count(), 1);

        // Turning an option off removes what it made.
        let existing = find_existing(&t.env, &registry, &product(), None).unwrap().unwrap();
        let mut plan = t.plan(Some(existing));
        plan.tasks.desktop = false;
        plan.tasks.startup = false;
        install(&t.env, &mut package, &plan, &Quiet).unwrap();
        assert_eq!(t.env.desktop.read_dir().unwrap().count(), 0);
        assert_eq!(registry.run("Demo").unwrap(), None);
    }

    #[test]
    fn a_failed_swap_restores_every_file() {
        let _com = shortcut::Com::start();
        let t = Test::new("rollback", &[("a.dll", b"a1".to_vec()), ("Demo.exe", b"v1".to_vec())]);
        let plan = t.plan(None);
        install(&t.env, &mut t.package(), &plan, &Quiet).unwrap();
        let t2 = Test::new("rollback-v2", &[("a.dll", b"a2".to_vec()), ("Demo.exe", b"v2".to_vec())]);
        // An open handle without delete sharing blocks renaming the file, as a busy file does.
        let lock = {
            use std::os::windows::fs::OpenOptionsExt;
            fs::OpenOptions::new().read(true).share_mode(1).open(plan.dir.join("Demo.exe")).unwrap()
        };
        let existing = find_existing(&t.env, &t.env.registry(), &product(), None).unwrap();
        let result = install(&t.env, &mut t2.package(), &t.plan(existing), &Quiet);
        assert!(matches!(result, Err(Failure::RolledBack(_))), "{result:?}");
        drop(lock);
        assert_eq!(fs::read(plan.dir.join("a.dll")).unwrap(), b"a1", "an already replaced file is put back");
        assert_eq!(fs::read(plan.dir.join("Demo.exe")).unwrap(), b"v1");
        assert_eq!(read_record(&plan.dir, "Demo").unwrap().version, "1.0.0");
        assert!(staging_left(&plan.dir).is_empty(), "the staging folder is removed");
    }

    #[test]
    fn cancelling_writes_nothing_and_folders_are_checked() {
        let t = Test::new("cancel", &[("Demo.exe", b"v1".to_vec())]);
        let plan = t.plan(None);
        let cancel = Cancel(AtomicBool::new(true));
        assert_eq!(install(&t.env, &mut t.package(), &plan, &cancel), Err(Failure::Cancelled));
        assert!(!plan.dir.exists());

        let package = t.package();
        let packed = &package.manifest;
        assert!(check_new_dir(&plan.dir, &t.env, packed).is_ok());
        assert!(check_new_dir(Path::new("relative"), &t.env, packed).is_err());
        assert!(check_new_dir(&t.env.desktop, &t.env, packed).is_err());
        fs::create_dir_all(t.root.join("busy")).unwrap();
        fs::write(t.root.join("busy").join("x"), b"x").unwrap();
        assert!(check_new_dir(&t.root.join("busy"), &t.env, packed).is_err(), "non-empty folder");
        let long = t.root.join("x".repeat(230));
        assert!(check_new_dir(&long, &t.env, packed).is_err(), "too long");
    }

    #[test]
    fn a_registered_installation_cannot_move_and_run_entries_must_match_exactly() {
        let _com = shortcut::Com::start();
        let t = Test::new("moved", &[("Demo.exe", b"v1".to_vec())]);
        let plan = t.plan(None);
        install(&t.env, &mut t.package(), &plan, &Quiet).unwrap();
        let other = t.root.join("Other");
        let found = find_existing(&t.env, &t.env.registry(), &product(), Some(&other));
        assert!(matches!(found, Err(Failure::Invalid(_))));
        let same = find_existing(&t.env, &t.env.registry(), &product(), Some(&plan.dir)).unwrap();
        assert!(same.is_some());
        let dir = &plan.dir;
        assert!(run_points_here(&run_command(dir, &product()), dir, &product()));
        let elsewhere = format!("\"{}\\Demo.exe\" --background", other.display());
        assert!(!run_points_here(&elsewhere, dir, &product()));
    }
}
