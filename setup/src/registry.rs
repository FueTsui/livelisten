//! The two per-user registry places the installer touches: its "Installed apps" entry and
//! the start-at-login Run value. In isolated test mode they live in a JSON file instead.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, KEY_WRITE, REG_DWORD, REG_EXPAND_SZ, REG_OPTION_NON_VOLATILE,
    REG_SZ, REG_VALUE_TYPE, RRF_RT_ANY, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegGetValueW,
    RegOpenKeyExW, RegSetValueExW,
};
use windows::core::{HSTRING, PCWSTR};

const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// The values of the "Installed apps" entry; nothing else is read or restored.
pub const ENTRY_VALUES: [&str; 11] = [
    "DisplayName",
    "DisplayVersion",
    "Publisher",
    "InstallLocation",
    "DisplayIcon",
    "UninstallString",
    "QuietUninstallString",
    "NoModify",
    "NoRepair",
    "EstimatedSize",
    "InstallDate",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Dword(u32),
    Str(String),
}

pub type Entry = BTreeMap<String, Value>;

pub fn text(entry: &Entry, name: &str) -> Option<String> {
    match entry.get(name) {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub enum Registry {
    /// `HKEY_CURRENT_USER`.
    Real,
    /// Isolated test mode.
    File(PathBuf),
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    uninstall: BTreeMap<String, Entry>,
    run: BTreeMap<String, String>,
}

fn win32(error: WIN32_ERROR) -> io::Result<()> {
    if error.is_ok() { Ok(()) } else { Err(io::Error::from_raw_os_error(error.0 as i32)) }
}

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the key was opened by RegOpenKeyExW or RegCreateKeyExW and is closed once.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

fn open(path: &str, access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> io::Result<Option<Key>> {
    let mut key = HKEY::default();
    // SAFETY: `key` receives an open handle that `Key` closes.
    let error = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(path), None, access, &mut key) };
    if error == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    win32(error)?;
    Ok(Some(Key(key)))
}

fn create(path: &str) -> io::Result<Key> {
    let mut key = HKEY::default();
    // SAFETY: `key` receives an open handle that `Key` closes.
    win32(unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
    })?;
    Ok(Key(key))
}

fn read(key: &Key, name: &str) -> io::Result<Option<Value>> {
    let name = HSTRING::from(name);
    let (mut kind, mut size) = (REG_VALUE_TYPE::default(), 0u32);
    // SAFETY: a null data pointer asks only for the type and size.
    let error =
        unsafe { RegGetValueW(key.0, PCWSTR::null(), &name, RRF_RT_ANY, Some(&mut kind), None, Some(&mut size)) };
    if error == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    win32(error)?;
    let mut data = vec![0u8; size as usize + 2];
    size = data.len() as u32;
    // SAFETY: `data` is writable for `size` bytes.
    win32(unsafe {
        RegGetValueW(
            key.0,
            PCWSTR::null(),
            &name,
            RRF_RT_ANY,
            Some(&mut kind),
            Some(data.as_mut_ptr().cast()),
            Some(&mut size),
        )
    })?;
    data.truncate(size as usize);
    Ok(match kind {
        REG_DWORD if data.len() >= 4 => Some(Value::Dword(u32::from_le_bytes(data[..4].try_into().unwrap()))),
        REG_SZ | REG_EXPAND_SZ => {
            let wide: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            Some(Value::Str(String::from_utf16_lossy(&wide).trim_end_matches('\0').to_string()))
        }
        _ => None,
    })
}

fn write(key: &Key, name: &str, value: &Value) -> io::Result<()> {
    let (kind, data) = match value {
        Value::Dword(n) => (REG_DWORD, n.to_le_bytes().to_vec()),
        Value::Str(s) => (REG_SZ, s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()),
    };
    // SAFETY: `data` holds the value in the layout `kind` describes.
    win32(unsafe { RegSetValueExW(key.0, &HSTRING::from(name), None, kind, Some(&data)) })
}

impl Registry {
    fn load(path: &PathBuf) -> io::Result<Store> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Store::default()),
            Err(e) => Err(e),
        }
    }

    fn save(path: &PathBuf, store: &Store) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(store).map_err(io::Error::other)?)?;
        std::fs::rename(temp, path)
    }

    fn update(path: &PathBuf, change: impl FnOnce(&mut Store)) -> io::Result<()> {
        let mut store = Self::load(path)?;
        change(&mut store);
        Self::save(path, &store)
    }

    /// The "Installed apps" entry of `id`, limited to [`ENTRY_VALUES`].
    pub fn entry(&self, id: &str) -> io::Result<Option<Entry>> {
        match self {
            Registry::File(path) => Ok(Self::load(path)?.uninstall.get(id).cloned()),
            Registry::Real => {
                let Some(key) = open(&format!(r"{UNINSTALL}\{id}"), KEY_READ)? else { return Ok(None) };
                let mut entry = Entry::new();
                for name in ENTRY_VALUES {
                    if let Some(value) = read(&key, name)? {
                        entry.insert(name.to_string(), value);
                    }
                }
                Ok(Some(entry))
            }
        }
    }

    /// Replaces the entry of `id` as a whole.
    pub fn set_entry(&self, id: &str, entry: &Entry) -> io::Result<()> {
        match self {
            Registry::File(path) => Self::update(path, |s| {
                s.uninstall.insert(id.to_string(), entry.clone());
            }),
            Registry::Real => {
                self.delete_entry(id)?;
                let key = create(&format!(r"{UNINSTALL}\{id}"))?;
                entry.iter().try_for_each(|(name, value)| write(&key, name, value))
            }
        }
    }

    pub fn delete_entry(&self, id: &str) -> io::Result<()> {
        match self {
            Registry::File(path) => Self::update(path, |s| {
                s.uninstall.remove(id);
            }),
            Registry::Real => {
                let path = HSTRING::from(format!(r"{UNINSTALL}\{id}"));
                // SAFETY: deletes one subkey tree below the user's Uninstall key.
                let error = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &path) };
                if error == ERROR_FILE_NOT_FOUND { Ok(()) } else { win32(error) }
            }
        }
    }

    pub fn run(&self, name: &str) -> io::Result<Option<String>> {
        match self {
            Registry::File(path) => Ok(Self::load(path)?.run.get(name).cloned()),
            Registry::Real => {
                let Some(key) = open(RUN, KEY_READ)? else { return Ok(None) };
                Ok(match read(&key, name)? {
                    Some(Value::Str(s)) => Some(s),
                    _ => None,
                })
            }
        }
    }

    /// Writes the Run value, or removes it with `None`.
    pub fn set_run(&self, name: &str, value: Option<&str>) -> io::Result<()> {
        match (self, value) {
            (Registry::File(path), value) => Self::update(path, |s| match value {
                Some(v) => {
                    s.run.insert(name.to_string(), v.to_string());
                }
                None => {
                    s.run.remove(name);
                }
            }),
            (Registry::Real, Some(value)) => write(&create(RUN)?, name, &Value::Str(value.to_string())),
            (Registry::Real, None) => {
                let Some(key) = open(RUN, KEY_SET_VALUE)? else { return Ok(()) };
                // SAFETY: removes one value of the user's Run key.
                let error = unsafe { RegDeleteValueW(key.0, &HSTRING::from(name)) };
                if error == ERROR_FILE_NOT_FOUND { Ok(()) } else { win32(error) }
            }
        }
    }
}

/// The program a Run value starts: the first, possibly quoted, token.
pub fn run_target(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    let program = match value.strip_prefix('"') {
        Some(rest) => rest.split_once('"')?.0,
        None => value.split_whitespace().next()?,
    };
    Some(PathBuf::from(program))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_round_trips() {
        let dir = std::env::temp_dir().join(format!("setup-registry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let registry = Registry::File(dir.join("registry.json"));
        assert_eq!(registry.entry("Demo").unwrap(), None);
        let mut entry = Entry::new();
        entry.insert("DisplayName".into(), Value::Str("演示".into()));
        entry.insert("NoModify".into(), Value::Dword(1));
        registry.set_entry("Demo", &entry).unwrap();
        assert_eq!(registry.entry("Demo").unwrap(), Some(entry));
        registry.set_run("Demo", Some("\"C:\\x\\Demo.exe\" --background")).unwrap();
        assert_eq!(registry.run("Demo").unwrap().as_deref(), Some("\"C:\\x\\Demo.exe\" --background"));
        registry.set_run("Demo", None).unwrap();
        registry.delete_entry("Demo").unwrap();
        assert_eq!((registry.run("Demo").unwrap(), registry.entry("Demo").unwrap()), (None, None));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn finds_the_program_of_a_run_value() {
        assert_eq!(run_target(r#""C:\A B\Demo.exe" --background"#), Some(PathBuf::from(r"C:\A B\Demo.exe")));
        assert_eq!(run_target(r"C:\x\Demo.exe --background"), Some(PathBuf::from(r"C:\x\Demo.exe")));
        assert_eq!(run_target(r#""unterminated"#), None);
        assert_eq!(run_target("  "), None);
    }
}
