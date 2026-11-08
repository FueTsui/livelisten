//! The install manifest (product details from the app, files added by the packer) and the
//! install record the installer leaves next to the program for upgrades and uninstalling.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::codec::Method;

pub const FORMAT: u32 = 1;
/// Written into the install folder; marks it as ours and lists what to remove.
pub const RECORD_FILE: &str = "install-record.json";
pub const UNINSTALLER: &str = "Uninstall.exe";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Names {
    pub zh: String,
    pub en: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Base {
    RoamingAppData,
    LocalAppData,
}

/// A folder of user data removed only when the user asks for it while uninstalling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataDir {
    pub base: Base,
    pub path: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Tasks {
    pub start_menu: bool,
    pub desktop: bool,
    /// Start at login through the per-user Run key.
    pub startup: bool,
}

impl Default for Tasks {
    fn default() -> Self {
        Tasks { start_menu: true, desktop: false, startup: false }
    }
}

/// Product details from the app's `setup/install-manifest.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Product {
    /// Folder name, registry key and Run value name.
    pub id: String,
    pub name: Names,
    pub publisher: String,
    /// Main program, relative to the install folder.
    pub exe: String,
    /// Build-time only: the icon linked into the setup program.
    #[serde(default, skip_serializing)]
    pub icon: Option<String>,
    /// Arguments of the start-at-login entry.
    #[serde(default)]
    pub background_args: String,
    /// Arguments that ask a running instance to exit normally.
    #[serde(default)]
    pub quit_args: Vec<String>,
    #[serde(default)]
    pub data_dirs: Vec<DataDir>,
    /// Windows Credential Manager entries starting with this are user data too.
    #[serde(default)]
    pub credential_prefix: String,
    /// What the user data is called in setup texts, e.g. "settings, note cards and saved keys".
    #[serde(default)]
    pub data_label: Option<Names>,
    /// Environment of programs started in isolated test mode; `{roamingAppData}` and
    /// `{localAppData}` stand for the redirected folders.
    #[serde(default)]
    pub test_environment: BTreeMap<String, String>,
    #[serde(default)]
    pub tasks: Tasks,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    Installer,
    Uninstaller,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileEntry {
    /// Relative path with `/` separators.
    pub path: String,
    pub size: u64,
    /// SHA-256 of the installed file, lowercase hex.
    pub sha256: String,
    /// Position of the packed bytes from the start of the payload.
    pub offset: u64,
    pub packed: u64,
    pub method: Method,
}

/// The manifest at the end of the payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Packed {
    pub format: u32,
    pub role: Role,
    pub version: String,
    pub product: Product,
    pub files: Vec<FileEntry>,
}

/// What an installation consists of, kept in [`RECORD_FILE`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Record {
    pub format: u32,
    pub id: String,
    pub version: String,
    /// Installed program files, relative, `/` separated.
    pub files: Vec<String>,
    /// Shortcuts created by the installer, absolute paths.
    pub shortcuts: Vec<String>,
}

/// A relative path that stays inside the install folder and is valid on Windows.
pub fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() < 400
        && path.split('/').all(|part| {
            let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
            let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && stem.as_bytes()[3].is_ascii_digit());
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with(['.', ' '])
                && !part.chars().any(|c| c.is_control() || "<>:\"\\|?*".contains(c))
                && !reserved
        })
}

fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

fn safe_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 32
        && version.chars().all(|c| c.is_ascii_alphanumeric() || ".-+".contains(c))
}

impl Product {
    pub fn validate(&self) -> Result<(), String> {
        if !safe_id(&self.id) {
            return Err("product id must be letters, digits, '.', '_' or '-'".into());
        }
        if self.name.zh.trim().is_empty() || self.name.en.trim().is_empty() || self.publisher.trim().is_empty() {
            return Err("product names and publisher are required".into());
        }
        if !safe_relative(&self.exe) {
            return Err("exe must be a relative path".into());
        }
        if self.data_dirs.iter().any(|d| !safe_relative(&d.path)) {
            return Err("data folders must be relative to their base".into());
        }
        Ok(())
    }
}

impl Packed {
    /// Checks everything the installer relies on; `blob_len` is the size of the packed files.
    pub fn validate(&self, blob_len: u64) -> Result<(), String> {
        if self.format != FORMAT {
            return Err(format!("unknown manifest format {}", self.format));
        }
        if !safe_version(&self.version) {
            return Err("invalid version".into());
        }
        self.product.validate()?;
        let mut seen = HashSet::new();
        for file in &self.files {
            if !safe_relative(&file.path) {
                return Err(format!("unsafe path {:?}", file.path));
            }
            let lower = file.path.to_lowercase();
            if !seen.insert(lower.clone()) || lower == RECORD_FILE.to_lowercase() || lower == UNINSTALLER.to_lowercase()
            {
                return Err(format!("duplicate or reserved path {:?}", file.path));
            }
            if file.sha256.len() != 64 || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("invalid hash for {:?}", file.path));
            }
            if file.offset.checked_add(file.packed).is_none_or(|end| end > blob_len) {
                return Err(format!("{:?} lies outside the payload", file.path));
            }
        }
        if self.role == Role::Installer && !seen.contains(&self.product.exe.to_lowercase()) {
            return Err("the main program is not in the package".into());
        }
        if self.role == Role::Uninstaller && !self.files.is_empty() {
            return Err("an uninstaller carries no files".into());
        }
        Ok(())
    }

    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// Compares `major.minor.patch`; pre-release and build suffixes are ignored.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |v: &str| -> Vec<u64> {
        v.split(['-', '+']).next().unwrap_or("").split('.').map(|p| p.parse().unwrap_or(0)).collect()
    };
    let (mut a, mut b) = (parse(a), parse(b));
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    a.cmp(&b)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn product() -> Product {
        Product {
            id: "Demo".into(),
            name: Names { zh: "演示".into(), en: "Demo".into() },
            publisher: "Tester".into(),
            exe: "Demo.exe".into(),
            icon: None,
            background_args: "--background".into(),
            quit_args: vec![],
            data_dirs: vec![],
            credential_prefix: String::new(),
            data_label: None,
            test_environment: BTreeMap::new(),
            tasks: Tasks::default(),
        }
    }

    #[test]
    fn rejects_paths_that_leave_the_folder_or_are_invalid() {
        for good in ["a.dll", "docs/USAGE.md", "zh-CN/Microsoft.ui.xaml.dll.mui", "runtime/Lib/a b.py"] {
            assert!(safe_relative(good), "{good}");
        }
        for bad in
            ["", "../x", "a/../../x", "/abs", "C:/x", "a\\b", "a//b", "con", "LPT1.txt", "a:stream", "x.", "x ", "./a"]
        {
            assert!(!safe_relative(bad), "{bad}");
        }
    }

    #[test]
    fn validates_offsets_duplicates_and_roles() {
        let file = |path: &str, offset: u64| FileEntry {
            path: path.into(),
            size: 3,
            sha256: "0".repeat(64),
            offset,
            packed: 3,
            method: Method::Store,
        };
        let mut packed = Packed {
            format: FORMAT,
            role: Role::Installer,
            version: "1.2.3".into(),
            product: product(),
            files: vec![file("Demo.exe", 0), file("b.dll", 3)],
        };
        assert_eq!(packed.validate(6), Ok(()));
        assert!(packed.validate(5).is_err(), "past the end");
        packed.files.push(file("DEMO.EXE", 0));
        assert!(packed.validate(6).is_err(), "case-insensitive duplicate");
        packed.files.truncate(1);
        packed.files[0].path = "other.exe".into();
        assert!(packed.validate(6).is_err(), "main program missing");
        packed.files[0].path = RECORD_FILE.into();
        assert!(packed.validate(6).is_err(), "reserved name");
        packed.role = Role::Uninstaller;
        packed.files.clear();
        assert_eq!(packed.validate(0), Ok(()));
    }

    #[test]
    fn compares_versions_numerically() {
        use std::cmp::Ordering::*;
        assert_eq!(compare_versions("0.10.0", "0.9.9"), Greater);
        assert_eq!(compare_versions("1.0", "1.0.0"), Equal);
        assert_eq!(compare_versions("1.0.0-beta", "1.0.0"), Equal);
        assert_eq!(compare_versions("0.1.0", "0.1.1"), Less);
    }
}
