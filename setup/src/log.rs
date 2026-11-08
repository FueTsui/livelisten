//! The setup log, always written (to the temporary folder unless `/LOG=<file>` names one)
//! so a failure can point to it. It records steps, paths and error codes; never file
//! contents, settings or credentials.

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use windows::Win32::System::SystemInformation::GetLocalTime;

static LOG: Mutex<Option<(File, PathBuf)>> = Mutex::new(None);

/// Opens the log file, appending to it.
pub fn open(path: PathBuf) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    *LOG.lock().unwrap_or_else(|e| e.into_inner()) = Some((file, path));
    Ok(())
}

/// Where the log is written, once it is open.
pub fn path() -> Option<PathBuf> {
    LOG.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|(_, path)| path.clone())
}

pub fn line(text: &str) {
    if let Some((file, _)) = LOG.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        // SAFETY: GetLocalTime only fills and returns a SYSTEMTIME.
        let t = unsafe { GetLocalTime() };
        let _ = writeln!(
            file,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} {text}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
        );
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::log::line(&format!($($arg)*)) };
}

/// Now as `YYYYMMDD-HHMMSS`, for log file names.
pub fn stamp() -> String {
    // SAFETY: GetLocalTime only fills and returns a SYSTEMTIME.
    let t = unsafe { GetLocalTime() };
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// Today as `YYYYMMDD`, the format of the "Installed apps" install date.
pub fn today() -> String {
    // SAFETY: GetLocalTime only fills and returns a SYSTEMTIME.
    let t = unsafe { GetLocalTime() };
    format!("{:04}{:02}{:02}", t.wYear, t.wMonth, t.wDay)
}
