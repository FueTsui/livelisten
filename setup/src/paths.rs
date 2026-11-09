//! Where things go, and the isolated test mode (REL-06).
//!
//! In debug builds, `SETUP_TEST_ROOT=<folder>` redirects the install folder, Start menu,
//! desktop, AppData and the registry (to `<folder>\registry.json`) into that folder.
//! Release builds never read the variable.

use std::io;
use std::path::{Path, PathBuf};

use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_LocalAppData, FOLDERID_Programs, FOLDERID_RoamingAppData, FOLDERID_UserProgramFiles,
    KF_FLAG_CREATE, SHGetKnownFolderPath,
};
use windows::core::GUID;

use crate::manifest::{Base, DataDir, Product};
use crate::registry::Registry;

pub const TEST_ROOT_VAR: &str = "SETUP_TEST_ROOT";

#[derive(Clone, Debug)]
pub struct Env {
    /// Set in isolated test mode.
    pub test_root: Option<PathBuf>,
    pub roaming_app_data: PathBuf,
    pub local_app_data: PathBuf,
    /// Per-user programs folder, `%LOCALAPPDATA%\Programs`.
    pub user_programs: PathBuf,
    /// Start menu programs folder.
    pub start_menu: PathBuf,
    pub desktop: PathBuf,
}

fn known(id: &GUID) -> io::Result<PathBuf> {
    // SAFETY: SHGetKnownFolderPath returns a CoTaskMem string that is copied and freed here.
    unsafe {
        let raw = SHGetKnownFolderPath(id, KF_FLAG_CREATE, None).map_err(io::Error::other)?;
        let path = PathBuf::from(raw.to_string().map_err(io::Error::other)?);
        CoTaskMemFree(Some(raw.0 as *const _));
        Ok(path)
    }
}

impl Env {
    pub fn detect() -> io::Result<Env> {
        #[cfg(debug_assertions)]
        if let Some(root) = std::env::var_os(TEST_ROOT_VAR).filter(|v| !v.is_empty()) {
            let root = PathBuf::from(root);
            if !root.is_absolute() {
                return Err(io::Error::other(format!("{TEST_ROOT_VAR} must be a full path")));
            }
            return Ok(Env::for_test(&root));
        }
        Ok(Env {
            test_root: None,
            roaming_app_data: known(&FOLDERID_RoamingAppData)?,
            local_app_data: known(&FOLDERID_LocalAppData)?,
            user_programs: known(&FOLDERID_UserProgramFiles)?,
            start_menu: known(&FOLDERID_Programs)?,
            desktop: known(&FOLDERID_Desktop)?,
        })
    }

    pub fn for_test(root: &Path) -> Env {
        Env {
            test_root: Some(root.to_path_buf()),
            roaming_app_data: root.join("AppData").join("Roaming"),
            local_app_data: root.join("AppData").join("Local"),
            user_programs: root.join("AppData").join("Local").join("Programs"),
            start_menu: root.join("StartMenu"),
            desktop: root.join("Desktop"),
        }
    }

    pub fn registry(&self) -> Registry {
        match &self.test_root {
            Some(root) => Registry::File(root.join("registry.json")),
            None => Registry::Real,
        }
    }

    pub fn default_dir(&self, product: &Product) -> PathBuf {
        self.user_programs.join(&product.id)
    }

    pub fn data_dir(&self, dir: &DataDir) -> PathBuf {
        let base = match dir.base {
            Base::RoamingAppData => &self.roaming_app_data,
            Base::LocalAppData => &self.local_app_data,
        };
        base.join(dir.path.replace('/', "\\"))
    }

    /// Extra environment for programs this installer starts (the app, its quit request).
    pub fn child_environment(&self, product: &Product) -> Vec<(String, String)> {
        if self.test_root.is_none() {
            return Vec::new();
        }
        let roaming = self.roaming_app_data.to_string_lossy();
        let local = self.local_app_data.to_string_lossy();
        product
            .test_environment
            .iter()
            .map(|(k, v)| (k.clone(), v.replace("{roamingAppData}", &roaming).replace("{localAppData}", &local)))
            .collect()
    }
}

/// True if an install path would put program files in a product data folder.
pub fn in_data_dir(dir: &Path, env: &Env, product: &Product) -> bool {
    let inside = |dir: &Path, data: &Path| crate::process::same_path(dir, data) || crate::process::inside(dir, data);
    let absolute = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    product.data_dirs.iter().any(|data| {
        let data = env.data_dir(data);
        inside(&absolute, &data)
            || std::fs::canonicalize(dir)
                .ok()
                .zip(std::fs::canonicalize(data).ok())
                .is_some_and(|(dir, data)| inside(&dir, &data))
    })
}

/// Folders an installation must never replace or empty, whatever `/DIR=` says.
pub fn protected(dir: &Path, env: &Env) -> bool {
    let normalized = dir.to_string_lossy().trim_end_matches('\\').to_lowercase();
    let parts = normalized.split('\\').filter(|p| !p.is_empty()).count();
    if parts < 2 {
        return true; // a drive root
    }
    let mut roots = vec![
        env.roaming_app_data.clone(),
        env.local_app_data.clone(),
        env.user_programs.clone(),
        env.start_menu.clone(),
        env.desktop.clone(),
    ];
    for var in ["USERPROFILE", "SystemRoot", "ProgramFiles", "ProgramFiles(x86)", "ProgramData", "PUBLIC"] {
        if let Some(value) = std::env::var_os(var) {
            roots.push(PathBuf::from(value));
        }
    }
    if let Some(root) = &env.test_root {
        roots.push(root.clone());
    }
    roots.iter().any(|r| r.to_string_lossy().trim_end_matches('\\').to_lowercase() == normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mode_keeps_everything_under_the_root() {
        let env = Env::for_test(Path::new(r"C:\t\root"));
        for path in [&env.roaming_app_data, &env.local_app_data, &env.user_programs, &env.start_menu, &env.desktop] {
            assert!(path.starts_with(r"C:\t\root"), "{}", path.display());
        }
        assert!(matches!(env.registry(), Registry::File(p) if p == Path::new(r"C:\t\root\registry.json")));
    }

    #[test]
    fn refuses_roots_and_shared_folders() {
        let env = Env::for_test(Path::new(r"C:\t\root"));
        assert!(protected(Path::new(r"D:\"), &env));
        assert!(protected(Path::new(r"C:\t\root\AppData\Local\Programs\"), &env));
        assert!(protected(Path::new(r"C:\t\root\Desktop"), &env));
        assert!(!protected(Path::new(r"C:\t\root\AppData\Local\Programs\Demo"), &env));
        if let Some(home) = std::env::var_os("USERPROFILE") {
            assert!(protected(Path::new(&home), &env));
        }
    }
}
