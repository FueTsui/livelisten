//! Native message boxes, the folder picker and the program icon. The message boxes also
//! carry setup when its window cannot be shown (no usable OpenGL driver).

use std::path::PathBuf;

use windows::Win32::Foundation::{HINSTANCE, HWND};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDIBits,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TDCBF_CANCEL_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION, TaskDialogIndirect,
};
use windows::Win32::UI::Shell::{
    FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GetIconInfo, HICON, ICONINFO, IDOK, IDYES, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, MB_DEFBUTTON2,
    MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_OKCANCEL, MB_YESNO, MESSAGEBOX_RESULT,
    MESSAGEBOX_STYLE, MessageBoxW,
};
use windows::core::{HSTRING, PCWSTR};

use crate::text::tr;

fn instance() -> HINSTANCE {
    // SAFETY: the module handle of this executable.
    unsafe { GetModuleHandleW(None) }.map(Into::into).unwrap_or_default()
}

/// MAKEINTRESOURCE: a resource id passed where a name is expected.
fn int_resource(id: u16) -> PCWSTR {
    PCWSTR(usize::from(id) as *const u16)
}

fn message_box(text: &str, style: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    let title = HSTRING::from(tr("安装程序", "Setup"));
    // SAFETY: a modal message box with valid strings.
    unsafe { MessageBoxW(None, &HSTRING::from(text), &title, style) }
}

pub fn error(text: &str) {
    message_box(text, MB_OK | MB_ICONERROR);
}

pub fn info(text: &str) {
    message_box(text, MB_OK | MB_ICONINFORMATION);
}

/// Yes / No, No by default.
pub fn ask(text: &str) -> bool {
    message_box(text, MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) == IDYES
}

/// OK / Cancel.
pub fn confirm(text: &str) -> bool {
    message_box(text, MB_OKCANCEL | MB_ICONINFORMATION) == IDOK
}

/// The uninstall confirmation when the setup window cannot be shown.
pub fn confirm_uninstall(name: &str, data: &str) -> bool {
    let title = HSTRING::from(format!("{} {name}", tr("卸载", "Uninstall")));
    let main = HSTRING::from(format!("{}{name}{}", tr("要卸载", "Uninstall "), tr("吗？", "?")));
    let content = HSTRING::from(format!(
        "{}{data}{}",
        tr("将删除程序文件、快捷方式和卸载信息。", "Program files, shortcuts and the uninstall entry are removed. "),
        tr("会保留，重新安装后可继续使用。", " are kept and work again after reinstalling.")
    ));
    let label = HSTRING::from(tr("卸载", "Uninstall"));
    let buttons = [TASKDIALOG_BUTTON { nButtonID: 100, pszButtonText: PCWSTR(label.as_ptr()) }];
    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hInstance: instance(),
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION,
        dwCommonButtons: TDCBF_CANCEL_BUTTON,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszMainInstruction: PCWSTR(main.as_ptr()),
        pszContent: PCWSTR(content.as_ptr()),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: 100,
        ..Default::default()
    };
    let mut pressed = 0;
    // SAFETY: every string and the button array outlive this modal call.
    unsafe { TaskDialogIndirect(&config, Some(&mut pressed), None, None) }.is_ok() && pressed == 100
}

/// The system folder picker, owned by `owner`.
pub fn browse_folder(owner: isize, title: &str) -> Option<PathBuf> {
    let owner = (owner != 0).then_some(HWND(owner as *mut _));
    // SAFETY: a modal folder picker; the returned string is freed.
    unsafe {
        (|| -> windows::core::Result<PathBuf> {
            let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
            dialog.SetOptions(dialog.GetOptions()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)?;
            dialog.SetTitle(&HSTRING::from(title))?;
            dialog.Show(owner)?;
            let item = dialog.GetResult()?;
            let raw = item.GetDisplayName(SIGDN_FILESYSPATH)?;
            let path = raw.to_string();
            CoTaskMemFree(Some(raw.0 as *const _));
            Ok(PathBuf::from(path?))
        })()
        .ok()
    }
}

/// The program icon (resource 1) as RGBA pixels of `size` × `size`.
pub fn icon_rgba(size: i32) -> Option<(Vec<u8>, u32, u32)> {
    // SAFETY: loads this program's own icon, reads its colour bitmap into `pixels` and
    // releases every handle it created.
    unsafe {
        let handle = LoadImageW(Some(instance()), int_resource(1), IMAGE_ICON, size, size, LR_DEFAULTCOLOR).ok()?;
        let icon = HICON(handle.0);
        let mut info = ICONINFO::default();
        let read = GetIconInfo(icon, &mut info);
        let mut pixels = vec![0u8; (size * size * 4) as usize];
        let mut lines = 0;
        if read.is_ok() {
            let dc = CreateCompatibleDC(None);
            let mut header = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: size,
                    biHeight: -size, // top-down rows
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            lines = GetDIBits(
                dc,
                info.hbmColor,
                0,
                size as u32,
                Some(pixels.as_mut_ptr().cast()),
                &mut header,
                DIB_RGB_COLORS,
            );
            let _ = DeleteDC(dc);
            let _ = DeleteObject(info.hbmColor.into());
            let _ = DeleteObject(info.hbmMask.into());
        }
        let _ = DestroyIcon(icon);
        if lines == 0 {
            return None;
        }
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2); // BGRA to RGBA
        }
        Some((pixels, size as u32, size as u32))
    }
}
