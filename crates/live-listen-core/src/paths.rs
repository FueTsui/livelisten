//! Where the app keeps settings and logs, and a small rotating file logger.
//!
//! Logs record events, codes and lengths only: never recognized text, keystrokes or secrets.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_LOG_BYTES: u64 = 1 << 20;
const KEEP_LOGS: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppDirs {
    pub config: PathBuf,
    pub logs: PathBuf,
}

impl AppDirs {
    /// `%APPDATA%\LiveListen` and `%LOCALAPPDATA%\LiveListen\Logs`, or everything under
    /// `override_dir` (tests and portable runs).
    pub fn resolve(override_dir: Option<&Path>) -> AppDirs {
        if let Some(dir) = override_dir {
            return AppDirs { config: dir.to_path_buf(), logs: dir.join("Logs") };
        }
        let base = directories::BaseDirs::new();
        let config =
            base.as_ref().map(|b| b.config_dir().join("LiveListen")).unwrap_or_else(|| PathBuf::from("LiveListen"));
        let logs = base
            .as_ref()
            .map(|b| b.data_local_dir().join("LiveListen").join("Logs"))
            .unwrap_or_else(|| config.join("Logs"));
        AppDirs { config, logs }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.json")
    }

    pub fn log_file(&self) -> PathBuf {
        self.logs.join("live-listen.log")
    }
}

struct FileLogger {
    path: PathBuf,
    file: Mutex<Option<(File, u64)>>,
    level: log::LevelFilter,
}

fn rotate(path: &Path) {
    for i in (1..KEEP_LOGS).rev() {
        let from = path.with_extension(format!("{i}.log"));
        let _ = fs::rename(&from, path.with_extension(format!("{}.log", i + 1)));
    }
    let _ = fs::rename(path, path.with_extension("1.log"));
}

fn open(path: &Path) -> Option<(File, u64)> {
    let file = OpenOptions::new().create(true).append(true).open(path).ok()?;
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    Some((file, size))
}

/// UTC timestamp `YYYY-MM-DDTHH:MM:SS.mmmZ` without a date library.
/// Milliseconds since the Unix epoch.
pub fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or_default()
}

pub fn timestamp() -> String {
    iso_from_ms(unix_ms())
}

/// UTC ISO 8601 with milliseconds for a Unix time in milliseconds.
pub fn iso_from_ms(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z", rem / 3600, rem % 3600 / 60, rem % 60, ms % 1000)
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!("{} {:<5} {}: {}\n", timestamp(), record.level(), record.target(), record.args());
        let mut guard = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if guard.as_ref().is_some_and(|(_, size)| *size + line.len() as u64 > MAX_LOG_BYTES) {
            *guard = None;
            rotate(&self.path);
        }
        if guard.is_none() {
            *guard = open(&self.path);
        }
        if let Some((file, size)) = guard.as_mut() {
            if file.write_all(line.as_bytes()).is_ok() {
                *size += line.len() as u64;
            }
        }
    }

    fn flush(&self) {
        if let Some((file, _)) = self.file.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let _ = file.flush();
        }
    }
}

/// Installs the file logger once per process. `LIVE_LISTEN_DEBUG=1` enables debug lines.
/// Writes to a temporary file first, then replaces the old file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("tmp");
    {
        let mut file = File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temp, path)
}

pub fn init_logging(dirs: &AppDirs) {
    let _ = fs::create_dir_all(&dirs.logs);
    let level =
        if std::env::var_os("LIVE_LISTEN_DEBUG").is_some() { log::LevelFilter::Debug } else { log::LevelFilter::Info };
    let path = dirs.log_file();
    let logger = FileLogger { file: Mutex::new(open(&path)), path, level };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(level);
    }
}

/// The last `count` lines of the current log, for diagnostics export.
pub fn log_tail(dirs: &AppDirs, count: usize) -> Vec<String> {
    let text = fs::read_to_string(dirs.log_file()).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(count)..].iter().map(|s| (*s).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_has_iso_shape() {
        let t = timestamp();
        assert_eq!(t.len(), 24);
        assert!(t.starts_with("20") && t.ends_with('Z') && t.as_bytes()[10] == b'T');
    }

    #[test]
    fn override_dir_keeps_everything_together() {
        let dirs = AppDirs::resolve(Some(Path::new("C:/tmp/ll")));
        assert_eq!(dirs.settings_file(), Path::new("C:/tmp/ll").join("settings.json"));
        assert!(dirs.log_file().starts_with(Path::new("C:/tmp/ll").join("Logs")));
    }
}
