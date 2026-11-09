//! Finding and closing programs that run from the install folder (REL-03). Programs are
//! matched by their full path only, so same-named programs elsewhere are never touched.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE};
use windows::core::{BOOL, PWSTR};

use crate::log;

#[derive(Clone, Debug)]
pub struct Running {
    pub pid: u32,
    pub path: PathBuf,
}

/// True when `path` is inside `dir` (compared without case, as Windows does).
pub fn inside(path: &Path, dir: &Path) -> bool {
    let path = path.to_string_lossy().to_lowercase();
    let dir = dir.to_string_lossy().trim_end_matches('\\').to_lowercase();
    path.len() > dir.len() + 1 && path.starts_with(&dir) && path.as_bytes()[dir.len()] == b'\\'
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    // A trailing separator does not change the folder (the temporary folder ends in one).
    let key = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
    key(a) == key(b)
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful open and is closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn image_path(pid: u32) -> Option<PathBuf> {
    // SAFETY: limited query access only reads the image name; the handle is closed by `Handle`.
    let process = Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?);
    let mut buffer = vec![0u16; 32768];
    let mut len = buffer.len() as u32;
    // SAFETY: `buffer` holds `len` UTF-16 units.
    unsafe { QueryFullProcessImageNameW(process.0, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut len) }.ok()?;
    Some(PathBuf::from(String::from_utf16_lossy(&buffer[..len as usize])))
}

/// Processes whose program file lies inside `dir`, except this one.
pub fn running_in(dir: &Path) -> Vec<Running> {
    let mut found = Vec::new();
    // SAFETY: a process snapshot is read and closed by `Handle`.
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else { return found };
    let snapshot = Handle(snapshot);
    let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    // SAFETY: `entry` has its size set as the API requires.
    let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) }.is_ok();
    while more {
        let pid = entry.th32ProcessID;
        if pid != std::process::id() && pid != 0 {
            if let Some(path) = image_path(pid).filter(|p| inside(p, dir)) {
                found.push(Running { pid, path });
            }
        }
        // SAFETY: as above.
        more = unsafe { Process32NextW(snapshot.0, &mut entry) }.is_ok();
    }
    found
}

fn names(list: &[Running]) -> Vec<String> {
    let mut names: Vec<String> =
        list.iter().filter_map(|r| r.path.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
    names.sort();
    names.dedup();
    names
}

extern "system" fn close_windows_of(window: HWND, pids: LPARAM) -> BOOL {
    // SAFETY: `pids` is the `&Vec<u32>` passed to EnumWindows below, alive for the call.
    let pids = unsafe { &*(pids.0 as *const Vec<u32>) };
    let mut pid = 0u32;
    // SAFETY: only reads the owning process id of a window handle from EnumWindows.
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    if pids.contains(&pid) {
        // SAFETY: posting WM_CLOSE asks the window to close the way its close button does.
        let _ = unsafe { PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
    true.into()
}

/// How the main program is asked to exit normally.
pub struct Quit<'a> {
    pub exe: PathBuf,
    pub args: &'a [String],
    pub env: &'a [(String, String)],
}

/// Requests a normal exit from programs in `dir` so they have a chance to save their data.
/// Returns whether the main program was running, or the names still running after `timeout`.
pub fn close_all(dir: &Path, quit: Option<Quit<'_>>, timeout: Duration) -> Result<bool, Vec<String>> {
    let list = running_in(dir);
    if list.is_empty() {
        return Ok(false);
    }
    log!("running from the install folder: {}", names(&list).join(", "));
    let main_running = quit.as_ref().is_some_and(|q| list.iter().any(|r| same_path(&r.path, &q.exe)));
    match quit.as_ref().filter(|q| main_running && !q.args.is_empty()) {
        Some(q) => {
            let child =
                Command::new(&q.exe).args(q.args).envs(q.env.iter().map(|(k, v)| (k, v))).current_dir(dir).spawn();
            match child {
                Ok(mut child) => {
                    let end = Instant::now() + Duration::from_secs(10);
                    while Instant::now() < end && matches!(child.try_wait(), Ok(None)) {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    log!("asked the program to exit");
                }
                Err(e) => log!("quit request failed: {e}"),
            }
        }
        None => {
            let pids: Vec<u32> = list.iter().map(|r| r.pid).collect();
            // SAFETY: the callback reads `pids` only during this call.
            let _ = unsafe { EnumWindows(Some(close_windows_of), LPARAM(&pids as *const Vec<u32> as isize)) };
        }
    }
    let end = Instant::now() + timeout;
    let mut left = running_in(dir);
    while !left.is_empty() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(200));
        left = running_in(dir);
    }
    if left.is_empty() { Ok(main_running) } else { Err(names(&left)) }
}

/// Starts the installed program without waiting for it.
pub fn launch(exe: &Path, env: &[(String, String)]) -> std::io::Result<()> {
    let mut command = Command::new(exe);
    command.envs(env.iter().map(|(k, v)| (k, v)));
    if let Some(dir) = exe.parent() {
        command.current_dir(dir);
    }
    command.spawn().map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_paths_inside_the_folder() {
        let dir = Path::new(r"C:\Users\A\AppData\Local\Programs\Demo");
        assert!(inside(Path::new(r"c:\users\a\appdata\local\programs\demo\Demo.exe"), dir));
        assert!(inside(Path::new(r"C:\Users\A\AppData\Local\Programs\Demo\runtime\python.exe"), dir));
        assert!(!inside(Path::new(r"C:\Users\A\AppData\Local\Programs\Demo2\Demo.exe"), dir));
        assert!(!inside(Path::new(r"C:\Other\Demo.exe"), dir));
        assert!(!inside(dir, dir));
    }

    #[test]
    fn same_path_ignores_case_and_a_trailing_separator() {
        assert!(same_path(Path::new(r"C:\Users\A\AppData\Local\Temp\"), Path::new(r"c:\users\a\appdata\local\temp")));
        assert!(same_path(Path::new(r"C:\Demo"), Path::new(r"C:\Demo")));
        assert!(!same_path(Path::new(r"C:\Demo"), Path::new(r"C:\Demo2")));
    }

    #[test]
    fn finds_and_closes_a_program_by_its_path() {
        let dir = std::env::temp_dir().join(format!("setup-process-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A copy of a console program that waits: it has no window and no quit request.
        let system = std::env::var("SystemRoot").unwrap();
        let exe = dir.join("waiter.exe");
        std::fs::copy(Path::new(&system).join(r"System32\PING.EXE"), &exe).unwrap();
        let mut child = Command::new(&exe).args(["-n", "60", "127.0.0.1"]).spawn().unwrap();
        let start = Instant::now();
        while running_in(&dir).is_empty() && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(running_in(&dir).len(), 1);
        let refused = close_all(&dir, None, Duration::from_millis(300));
        assert_eq!(refused, Err(vec!["waiter.exe".to_string()]));
        assert!(child.try_wait().unwrap().is_none(), "setup leaves the program running after a timeout");
        child.kill().unwrap();
        let _ = child.wait();
        assert!(running_in(&dir).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
