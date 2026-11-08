//! Shell shortcuts (`.lnk`) through IShellLinkW. The calling thread must have COM started
//! ([`Com::start`]).

use std::io;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::MAX_PATH;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile,
    STGM_READ,
};
use windows::Win32::UI::Shell::{IShellLinkW, SLGP_RAWPATH, ShellLink};
use windows::core::{HSTRING, Interface};

/// COM for the current thread, released on drop.
pub struct Com(bool);

impl Com {
    pub fn start() -> Com {
        // SAFETY: initializes COM for this thread; balanced by CoUninitialize in Drop.
        Com(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx above on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}

pub fn create(link: &Path, target: &Path, description: &str) -> io::Result<()> {
    if let Some(dir) = link.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // SAFETY: plain COM calls on a ShellLink object owned by this function.
    unsafe {
        let shell: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(io::Error::other)?;
        shell.SetPath(&HSTRING::from(target.as_os_str())).map_err(io::Error::other)?;
        if let Some(dir) = target.parent() {
            shell.SetWorkingDirectory(&HSTRING::from(dir.as_os_str())).map_err(io::Error::other)?;
        }
        shell.SetIconLocation(&HSTRING::from(target.as_os_str()), 0).map_err(io::Error::other)?;
        shell.SetDescription(&HSTRING::from(description)).map_err(io::Error::other)?;
        let file: IPersistFile = shell.cast().map_err(io::Error::other)?;
        file.Save(&HSTRING::from(link.as_os_str()), true).map_err(io::Error::other)
    }
}

/// The program a shortcut starts, or `None` when it cannot be read.
pub fn target(link: &Path) -> Option<PathBuf> {
    // SAFETY: plain COM calls on a ShellLink object owned by this function.
    unsafe {
        let shell: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = shell.cast().ok()?;
        file.Load(&HSTRING::from(link.as_os_str()), STGM_READ).ok()?;
        let mut buffer = vec![0u16; MAX_PATH as usize * 4];
        shell.GetPath(&mut buffer, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32).ok()?;
        let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        Some(PathBuf::from(String::from_utf16_lossy(&buffer[..len])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_reads_a_shortcut() {
        let _com = Com::start();
        let dir = std::env::temp_dir().join(format!("setup-shortcut-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let target = dir.join("App Folder").join("演示.exe");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"MZ").unwrap();
        let link = dir.join("Start").join("演示.lnk");
        create(&link, &target, "demo").unwrap();
        assert_eq!(target_of(&link), Some(target));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn target_of(link: &Path) -> Option<PathBuf> {
        target(link)
    }
}
