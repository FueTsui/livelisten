//! Command-line arguments (REL-04). Switches are case-insensitive.

use std::ffi::OsString;
use std::path::PathBuf;

/// Names accepted by `/TASKS=`; a leading `!` turns a task off.
pub const TASKS: [&str; 3] = ["startmenuicon", "desktopicon", "startup"];

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    /// Progress window only, no questions.
    pub silent: bool,
    /// No window at all.
    pub very_silent: bool,
    /// No message boxes in silent modes; the exit code reports the result.
    pub suppress_msgboxes: bool,
    /// Accepted for compatibility; this installer never needs a restart.
    pub no_restart: bool,
    pub dir: Option<PathBuf>,
    /// `Some(Some(file))` names the log file; otherwise it goes to the temporary folder.
    pub log: Option<Option<PathBuf>>,
    /// Chosen tasks; tasks not named keep their default (or, when upgrading, their state).
    pub tasks: Vec<(String, bool)>,
    /// Upgrade an existing installation only, and start the program again if it was running.
    pub update: bool,
    pub no_launch: bool,
    /// Uninstaller: also delete settings, user data and saved credentials.
    pub delete_data: bool,
    /// Check the package and exit.
    pub verify: bool,
    pub help: bool,
}

impl Args {
    pub fn quiet(&self) -> bool {
        self.silent || self.very_silent
    }

    /// Message boxes are shown unless a silent mode suppresses them.
    pub fn may_show_messages(&self) -> bool {
        !(self.quiet() && self.suppress_msgboxes)
    }

    pub fn task(&self, name: &str) -> Option<bool> {
        self.tasks.iter().rev().find(|(n, _)| n == name).map(|(_, on)| *on)
    }
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Args, String> {
    let mut out = Args::default();
    for arg in args {
        let arg = arg.into_string().map_err(|_| "arguments must be valid Unicode".to_string())?;
        let (switch, value) = match arg.split_once('=') {
            Some((s, v)) => (s.to_ascii_uppercase(), Some(v.trim_matches('"').to_string())),
            None => (arg.to_ascii_uppercase(), None),
        };
        match (switch.as_str(), value) {
            ("/SILENT", None) => out.silent = true,
            ("/VERYSILENT", None) => out.very_silent = true,
            ("/SUPPRESSMSGBOXES", None) => out.suppress_msgboxes = true,
            ("/NORESTART", None) => out.no_restart = true,
            ("/UPDATE", None) => out.update = true,
            ("/NOLAUNCH", None) => out.no_launch = true,
            ("/DELETEDATA", None) => out.delete_data = true,
            ("/VERIFY", None) => out.verify = true,
            ("/?" | "/HELP", None) => out.help = true,
            ("/LOG", None) => out.log = Some(None),
            ("/LOG", Some(path)) if !path.is_empty() => out.log = Some(Some(PathBuf::from(path))),
            ("/DIR", Some(path)) => {
                let dir = PathBuf::from(&path);
                let absolute = path.len() > 3 && path.as_bytes()[1] == b':' && path.as_bytes()[2] == b'\\';
                if !absolute || !dir.is_absolute() {
                    return Err(format!("/DIR needs a full path such as C:\\Apps\\Name, not {path:?}"));
                }
                out.dir = Some(dir);
            }
            ("/TASKS", Some(list)) => {
                for item in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    let (name, on) = match item.strip_prefix('!') {
                        Some(name) => (name.to_ascii_lowercase(), false),
                        None => (item.to_ascii_lowercase(), true),
                    };
                    if !TASKS.contains(&name.as_str()) {
                        return Err(format!("unknown task {name:?}; known tasks: {}", TASKS.join(", ")));
                    }
                    out.tasks.push((name, on));
                }
            }
            _ => return Err(format!("unknown argument {arg:?}")),
        }
    }
    Ok(out)
}

pub const USAGE: &str = "\
/SILENT              progress window only, no questions
/VERYSILENT          no window
/SUPPRESSMSGBOXES    no message boxes in silent modes (read the exit code)
/NORESTART           accepted; no restart is ever needed
/DIR=<folder>        install folder (new installations only)
/LOG[=<file>]        log file (a log is always written, by default to the temporary folder)
/TASKS=<list>        startmenuicon, desktopicon, startup; prefix ! to turn one off
/UPDATE              upgrade the existing installation only
/NOLAUNCH            do not start the program afterwards (silent installs never start it;
                     /UPDATE starts it again only when it was running)
/VERIFY              check the package and exit
Uninstaller: /SILENT /VERYSILENT /SUPPRESSMSGBOXES /LOG[=<file>] /DELETEDATA

Exit codes: 0 success; 1 damaged package or invalid arguments; 2 cancelled;
3 failed and rolled back; 7 the running program could not be closed, nothing replaced.";

#[cfg(test)]
mod tests {
    use super::*;

    fn p(list: &[&str]) -> Result<Args, String> {
        parse(list.iter().map(OsString::from))
    }

    #[test]
    fn parses_the_documented_switches() {
        let args = p(&[
            "/verysilent",
            "/SUPPRESSMSGBOXES",
            "/NoRestart",
            r#"/DIR="D:\Apps\Live Listen""#,
            "/LOG=C:\\t\\setup.log",
            "/TASKS=desktopicon,!startmenuicon",
            "/UPDATE",
            "/NOLAUNCH",
        ])
        .unwrap();
        assert!(args.very_silent && args.quiet() && !args.may_show_messages());
        assert_eq!(args.dir, Some(PathBuf::from(r"D:\Apps\Live Listen")));
        assert_eq!(args.log, Some(Some(PathBuf::from(r"C:\t\setup.log"))));
        assert_eq!(args.task("desktopicon"), Some(true));
        assert_eq!(args.task("startmenuicon"), Some(false));
        assert_eq!(args.task("startup"), None);
        assert!(args.update && args.no_launch && args.no_restart);
        assert_eq!(p(&["/LOG"]).unwrap().log, Some(None));
        assert!(p(&[]).unwrap().may_show_messages());
    }

    #[test]
    fn rejects_unknown_or_malformed_arguments() {
        for bad in [
            &["/FORCE"][..],
            &["/DIR=relative\\path"],
            &["/DIR=\\\\server\\share"],
            &["/DIR=C:"],
            &["/TASKS=everything"],
            &["/SILENT=1"],
            &["install"],
        ] {
            assert!(p(bad).is_err(), "{bad:?}");
        }
    }
}
