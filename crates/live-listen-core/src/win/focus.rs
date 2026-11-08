//! The dictation target: which window and control receive text, and whether they may.

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomation2, IUIAutomationElement, UIA_ComboBoxControlTypeId,
    UIA_DocumentControlTypeId, UIA_EditControlTypeId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GA_ROOTOWNER, GUITHREADINFO, GWL_STYLE, GetAncestor, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo,
    GetWindowLongPtrW, GetWindowThreadProcessId, IsWindow, SetForegroundWindow, WindowFromPoint,
};
use windows::core::Interface;

/// A top-level window and the control that had keyboard focus when dictation started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub window: isize,
    pub focus: isize,
    pub pid: u32,
}

fn hwnd(value: isize) -> HWND {
    HWND(value as *mut core::ffi::c_void)
}

pub fn own_pid() -> u32 {
    // SAFETY: GetCurrentProcessId has no preconditions.
    unsafe { GetCurrentProcessId() }
}

pub fn window_pid(window: isize) -> u32 {
    let mut pid = 0u32;
    // SAFETY: an invalid handle only makes the call return 0.
    unsafe { GetWindowThreadProcessId(hwnd(window), Some(&mut pid)) };
    pid
}

pub fn is_window(window: isize) -> bool {
    // SAFETY: IsWindow accepts any handle value.
    window != 0 && unsafe { IsWindow(Some(hwnd(window))) }.as_bool()
}

pub fn is_own(window: isize) -> bool {
    window != 0 && window_pid(window) == own_pid()
}

pub fn foreground() -> isize {
    // SAFETY: GetForegroundWindow has no preconditions.
    unsafe { GetForegroundWindow() }.0 as isize
}

/// True when the top-level window under the screen point belongs to this app.
pub fn own_window_at(x: i32, y: i32) -> bool {
    // SAFETY: both calls accept any point and handle values.
    let root = unsafe { GetAncestor(WindowFromPoint(windows::Win32::Foundation::POINT { x, y }), GA_ROOT) };
    is_own(root.0 as isize)
}

pub fn target_of(window: isize) -> Option<Target> {
    if !is_window(window) {
        return None;
    }
    let mut pid = 0u32;
    // SAFETY: `window` was checked with IsWindow; `pid` is a valid out pointer.
    let thread = unsafe { GetWindowThreadProcessId(hwnd(window), Some(&mut pid)) };
    let mut info = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
    // SAFETY: `info.cbSize` is set as the API requires.
    let focus = if unsafe { GetGUIThreadInfo(thread, &mut info) }.is_ok() { info.hwndFocus.0 as isize } else { 0 };
    Some(Target { window, focus, pid })
}

/// Class name of a window, for the log.
pub fn class_name(window: isize) -> String {
    let mut class = [0u16; 64];
    // SAFETY: the buffer is valid for its full length; invalid handles return 0.
    let len = unsafe { GetClassNameW(hwnd(window), &mut class) } as usize;
    String::from_utf16_lossy(&class[..len.min(class.len())])
}

/// Describes the window now in front, for the log: its class and whose it is.
pub fn describe_front(window: isize, target: &Target) -> String {
    if window == 0 {
        return "no window in front".into();
    }
    // SAFETY: GetAncestor accepts any handle value.
    let root_owner = unsafe { GetAncestor(hwnd(window), GA_ROOTOWNER) }.0 as isize;
    let owner = if root_owner == target.window {
        "a popup of the target"
    } else if window_pid(window) == target.pid {
        "same app"
    } else {
        "another app"
    };
    format!("{} in front, {owner}", class_name(window))
}

/// Taskbar, notification-area overflow and desktop windows. They take the foreground when
/// the user clicks the tray icon or the desktop, but are never a place to type dictation.
pub fn is_shell(window: isize) -> bool {
    window != 0 && is_shell_class(&class_name(window))
}

fn is_shell_class(name: &str) -> bool {
    matches!(
        name,
        "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "NotifyIconOverflowWindow"
            | "TopLevelWindowForOverflowXamlIsland"
            | "Progman"
            | "WorkerW"
    )
}

/// Asks Windows to bring `window` back to the front. Windows allows it only while this
/// process may set the foreground, for example right after the user clicked its tray icon.
pub fn bring_to_front(window: isize) -> bool {
    // SAFETY: SetForegroundWindow accepts any handle value; it fails for invalid ones.
    is_window(window) && unsafe { SetForegroundWindow(hwnd(window)) }.as_bool()
}

/// How long ago the user last pressed a key, clicked or moved the mouse, in this session.
pub fn user_idle() -> std::time::Duration {
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    // SAFETY: `info.cbSize` is set as the API requires; GetTickCount has no preconditions.
    let (ok, now) = unsafe { (GetLastInputInfo(&mut info).as_bool(), GetTickCount()) };
    if !ok {
        return std::time::Duration::ZERO;
    }
    std::time::Duration::from_millis(u64::from(now.wrapping_sub(info.dwTime)))
}

/// Says how the target stopped being the place to type, for the log.
pub fn describe_loss(target: &Target) -> String {
    if !is_window(target.window) {
        return "window closed".into();
    }
    let front = foreground();
    if front != target.window {
        return describe_front(front, target);
    }
    let focus = target_of(target.window).map_or(0, |t| t.focus);
    format!("focused control changed to {}", class_name(focus))
}

/// True while the target window is in front and its focused control is unchanged.
pub fn is_focused(target: &Target) -> bool {
    if foreground() != target.window {
        return false;
    }
    if target.focus == 0 {
        return true;
    }
    target_of(target.window).is_some_and(|now| now.focus == 0 || now.focus == target.focus)
}

fn process_elevated(pid: u32) -> Option<bool> {
    // SAFETY: handles are checked and closed; the out buffer matches TOKEN_ELEVATION.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut token = HANDLE::default();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
        let _ = CloseHandle(process);
        if opened.is_err() {
            // A token we cannot inspect belongs to a more privileged process.
            return Some(true);
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut size = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut TOKEN_ELEVATION as *mut core::ffi::c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        );
        let _ = CloseHandle(token);
        ok.ok().map(|_| elevation.TokenIsElevated != 0)
    }
}

/// Windows blocks synthetic input into elevated windows unless this app is elevated too.
pub fn blocked_by_elevation(target: &Target) -> bool {
    matches!((process_elevated(own_pid()), process_elevated(target.pid)), (Some(false), Some(true)))
}

/// Classic Win32 edit control with the password style.
fn win32_password(focus: isize) -> bool {
    if focus == 0 {
        return false;
    }
    let mut class = [0u16; 32];
    // SAFETY: the buffer is valid for its full length; invalid handles return 0.
    let len = unsafe { GetClassNameW(hwnd(focus), &mut class) } as usize;
    let name = String::from_utf16_lossy(&class[..len.min(class.len())]);
    if !name.eq_ignore_ascii_case("Edit") {
        return false;
    }
    const ES_PASSWORD: isize = 0x20;
    // SAFETY: reading the style of a window handle has no side effects.
    let style = unsafe { GetWindowLongPtrW(hwnd(focus), GWL_STYLE) };
    style & ES_PASSWORD != 0
}

/// UI Automation client, created on the engine thread and used only there.
pub struct Uia(IUIAutomation);

/// The UI Automation element that had keyboard focus. Web pages and other apps that draw
/// their own controls keep one window handle for every text box, so only this tells a
/// switch between two boxes of the same window.
pub struct Element(IUIAutomationElement);

impl Element {
    /// Text boxes and documents. Anything else (a window or pane) stands in for the real
    /// control while the app is still building its accessibility tree, as browsers do.
    fn is_text_field(&self) -> bool {
        // SAFETY: a property read on a valid COM interface owned by this thread.
        unsafe { self.0.CurrentControlType() }.is_ok_and(|t| {
            t == UIA_EditControlTypeId || t == UIA_DocumentControlTypeId || t == UIA_ComboBoxControlTypeId
        })
    }
}

/// Longest wait for an app to answer UI Automation; a hung app must not stall dictation.
const UIA_TIMEOUT_MS: u32 = 500;

impl Uia {
    pub fn new() -> Option<Uia> {
        // SAFETY: COM is initialized for this thread before the instance is created.
        let uia = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?
        };
        if let Ok(uia2) = uia.cast::<IUIAutomation2>() {
            // SAFETY: plain setters on a valid COM interface owned by this thread.
            unsafe {
                let _ = uia2.SetConnectionTimeout(UIA_TIMEOUT_MS);
                let _ = uia2.SetTransactionTimeout(UIA_TIMEOUT_MS);
            }
        }
        Some(Uia(uia))
    }

    /// The element with keyboard focus now, if UI Automation can tell.
    pub fn focused(&self) -> Option<Element> {
        // SAFETY: a call on a valid COM interface owned by this thread.
        unsafe { self.0.GetFocusedElement() }.ok().map(Element)
    }

    /// Whether `element` still has keyboard focus; `None` when UI Automation cannot tell.
    /// A stand-in element (see [`Element::is_text_field`]) is replaced by the text box that
    /// has focus now, unless that is a password field.
    pub fn still_focused(&self, element: &mut Element) -> Option<bool> {
        // SAFETY: calls on valid COM interfaces owned by this thread.
        unsafe {
            let now = self.0.GetFocusedElement().ok()?;
            if self.0.CompareElements(&element.0, &now).ok()?.as_bool() {
                return Some(true);
            }
            let now = Element(now);
            if element.is_text_field() || !now.is_text_field() || now.0.CurrentIsPassword().is_ok_and(|p| p.as_bool()) {
                return Some(false);
            }
            *element = now;
            Some(true)
        }
    }

    /// True when the focused element is a password field (UI Automation, then Win32 style).
    pub fn focused_is_password(&self, target: Option<&Target>) -> bool {
        // SAFETY: calls on a valid COM interface owned by this thread.
        let uia = unsafe { self.0.GetFocusedElement().and_then(|e| e.CurrentIsPassword()) };
        if let Ok(flag) = uia {
            if flag.as_bool() {
                return true;
            }
        }
        target.is_some_and(|t| win32_password(t.focus))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_classes_are_never_targets() {
        for class in ["Shell_TrayWnd", "Shell_SecondaryTrayWnd", "TopLevelWindowForOverflowXamlIsland", "WorkerW"] {
            assert!(is_shell_class(class), "{class}");
        }
        // Start and Search are CoreWindows with a text box; dictating into them stays possible.
        for class in ["Windows.UI.Core.CoreWindow", "Notepad", "Chrome_WidgetWin_1", ""] {
            assert!(!is_shell_class(class), "{class}");
        }
    }
}
