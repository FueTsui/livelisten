//! Synthetic input via `SendInput`. Every event carries [`MARKER`] so the hook can skip it.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC, MOUSE_EVENT_FLAGS,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, MapVirtualKeyW, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_CHAR, WM_KEYDOWN, WM_KEYUP,
};

use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
};

use crate::hotkey::{Binding, Chord, vk};

/// `dwExtraInfo` of input injected by this app ("LLST").
pub const MARKER: usize = 0x4C4C_5354;

fn keyboard(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: MARKER },
        },
    }
}

/// One `SendInput` call: Windows never interleaves other input into it.
fn send(inputs: &[INPUT]) -> bool {
    if inputs.is_empty() {
        return true;
    }
    // SAFETY: `inputs` is a slice of fully initialized INPUT values.
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    sent as usize == inputs.len()
}

const MODIFIERS: [u16; 8] =
    [vk::LSHIFT, vk::RSHIFT, vk::LCONTROL, vk::RCONTROL, vk::LMENU, vk::RMENU, vk::LWIN, vk::RWIN];

/// Releases held modifiers first so Ctrl+Backspace or Alt+letter shortcuts are not
/// triggered. They are not pressed again: a key the user lets go meanwhile would stick.
fn without_modifiers(body: Vec<INPUT>) -> Vec<INPUT> {
    let held: Vec<u16> = MODIFIERS.iter().copied().filter(|&k| is_down(k)).collect();
    let mut inputs = Vec::with_capacity(body.len() + held.len() + 2);
    if held.iter().any(|&k| matches!(k, vk::LMENU | vk::RMENU | vk::LWIN | vk::RWIN)) {
        inputs.push(virtual_key(vk::MASK, true));
        inputs.push(virtual_key(vk::MASK, false));
    }
    inputs.extend(held.iter().map(|&k| virtual_key(k, false)));
    inputs.extend(body);
    inputs
}

/// How typed text reaches the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Key events through `SendInput`, the keyboard layout and the input method.
    Keys,
    /// `WM_CHAR` and Backspace messages sent straight to this window.
    Messages(isize),
}

/// The last window written with messages, so the switch is logged once.
static LAST_MESSAGES: AtomicIsize = AtomicIsize::new(0);

/// Picks how to write `texts` into the focused window `window`.
///
/// Qt windows get messages. Qt keeps one record per virtual key, and every Unicode key event
/// arrives as VK_PACKET. Microsoft Pinyin swallows the key-up of full-width punctuation, so Qt
/// takes the next Unicode key for an auto-repeat of that punctuation and inserts it instead:
/// "，我们" became "，，们" in WeChat, and a dictation ending in "？" turned the first
/// character of the next one into "？". `WM_CHAR` skips both the key record and the input
/// method. Keys stay in use while a modifier is held, because Qt reads modifiers from the
/// keyboard state and drops Ctrl+text, and for text outside the Basic Multilingual Plane,
/// because Qt drops a lone surrogate from `WM_CHAR`.
pub fn route<'a>(window: isize, mut texts: impl Iterator<Item = &'a str>) -> Route {
    if window == 0 || !is_qt_class(&class_name(window)) {
        return Route::Keys;
    }
    if MODIFIERS.iter().any(|&k| is_down(k)) || texts.any(|t| t.chars().any(|c| u32::from(c) > 0xFFFF)) {
        return Route::Keys;
    }
    if LAST_MESSAGES.swap(window, Ordering::Relaxed) != window {
        log::info!("writing to a Qt window with window messages");
    }
    Route::Messages(window)
}

/// Qt 5 and 6 name their native windows "Qt<version>QWindow...", e.g. "Qt51514QWindowIcon".
pub fn is_qt_class(name: &str) -> bool {
    name.starts_with("Qt") && name.contains("QWindow")
}

fn class_name(window: isize) -> String {
    let mut class = [0u16; 64];
    // SAFETY: the buffer is valid for its full length; invalid handles return 0.
    let len = unsafe { GetClassNameW(HWND(window as *mut core::ffi::c_void), &mut class) } as usize;
    String::from_utf16_lossy(&class[..len.min(class.len())])
}

/// A busy target must not hold the real-time engine for seconds. The engine sends one
/// character (or one Backspace pair) per turn and rechecks cancellation between turns.
const MESSAGE_TIMEOUT_MS: u32 = 15;

/// Sends a message and waits until the window has handled it, which keeps the order.
fn message(window: isize, msg: u32, wparam: usize, lparam: isize) -> bool {
    let mut result = 0usize;
    // SAFETY: a stale handle only makes the call fail; `result` is a valid out pointer.
    let sent = unsafe {
        SendMessageTimeoutW(
            HWND(window as *mut core::ffi::c_void),
            msg,
            WPARAM(wparam),
            LPARAM(lparam),
            SMTO_ABORTIFHUNG,
            MESSAGE_TIMEOUT_MS,
            Some(&mut result),
        )
    };
    sent.0 != 0
}

/// Types text and returns how many characters reached the target. Unicode key events
/// bypass the keyboard layout and the IME. A `SendInput` call that Windows cut short counts
/// as nothing typed, so the text stays available to copy.
pub fn type_text(route: Route, text: &str) -> usize {
    if let Route::Messages(window) = route {
        let mut units = [0u16; 2];
        return text
            .chars()
            .take_while(|c| c.encode_utf16(&mut units).iter().all(|&u| message(window, WM_CHAR, usize::from(u), 1)))
            .count();
    }
    let mut body = Vec::new();
    for unit in text.encode_utf16() {
        body.push(keyboard(0, unit, KEYEVENTF_UNICODE));
        body.push(keyboard(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
    }
    if body.is_empty() || send(&without_modifiers(body)) { text.chars().count() } else { 0 }
}

fn is_extended(key: u16) -> bool {
    matches!(
        key,
        0x21..=0x28
            | vk::SNAPSHOT
            | vk::INSERT
            | vk::DELETE
            | vk::LWIN
            | vk::RWIN
            | vk::APPS
            | vk::DIVIDE
            | vk::NUMLOCK
            | vk::RCONTROL
            | vk::RMENU
    )
}

fn virtual_key(key: u16, down: bool) -> INPUT {
    // SAFETY: MapVirtualKeyW only reads its integer arguments.
    let scan = unsafe { MapVirtualKeyW(u32::from(key), MAPVK_VK_TO_VSC) } as u16;
    let mut flags = if down { KEYBD_EVENT_FLAGS(0) } else { KEYEVENTF_KEYUP };
    if is_extended(key) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    keyboard(key, scan, flags)
}

/// Pause after each Backspace. Input methods built on the Text Services Framework, such as
/// Microsoft Pinyin, drop or reorder Backspaces that arrive in one burst, which deletes the
/// wrong characters. Measured in Notepad: a burst of 5 failed every time, 5 ms apart never did.
pub const BACKSPACE_PACE: Duration = Duration::from_millis(15);

/// Deletes up to `count` characters before the caret, one Backspace at a time, and returns
/// how many Backspaces were delivered.
pub fn backspace(route: Route, count: usize) -> usize {
    if let Route::Messages(window) = route {
        // SAFETY: MapVirtualKeyW only reads its integer arguments.
        let scan = unsafe { MapVirtualKeyW(u32::from(vk::BACK), MAPVK_VK_TO_VSC) } as isize;
        let down = 1 | (scan << 16);
        let up = down | 0xC000_0000;
        let key = usize::from(vk::BACK);
        return (0..count)
            .take_while(|_| message(window, WM_KEYDOWN, key, down) && message(window, WM_KEYUP, key, up))
            .count();
    }
    for done in 0..count {
        let tap = vec![virtual_key(vk::BACK, true), virtual_key(vk::BACK, false)];
        if !send(&without_modifiers(tap)) {
            return done;
        }
        std::thread::sleep(BACKSPACE_PACE);
    }
    count
}

/// Re-sends a key event exactly as the hook saw it.
pub fn raw_key(key: u16, scan: u16, extended: bool, down: bool) -> bool {
    let mut flags = if down { KEYBD_EVENT_FLAGS(0) } else { KEYEVENTF_KEYUP };
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    send(&[keyboard(key, scan, flags)])
}

/// Presses or lets go of a modifier key.
pub fn modifier(key: u16, down: bool) -> bool {
    send(&[virtual_key(key, down)])
}

/// Taps the unassigned mask key so a held Win or Alt does not open Start or a menu.
pub fn mask() -> bool {
    send(&[virtual_key(vk::MASK, true), virtual_key(vk::MASK, false)])
}

pub fn is_down(key: u16) -> bool {
    // SAFETY: GetAsyncKeyState only reads its integer argument.
    unsafe { GetAsyncKeyState(i32::from(key)) < 0 }
}

/// Replays a key with the modifiers that were held, pressing only those no longer held.
pub fn chord(chord: &Chord) -> bool {
    let missing: Vec<u16> = chord.mods.iter().copied().filter(|&m| !is_down(m)).collect();
    let mut inputs: Vec<INPUT> = missing.iter().map(|&m| virtual_key(m, true)).collect();
    inputs.push(virtual_key(chord.key, true));
    inputs.push(virtual_key(chord.key, false));
    inputs.extend(missing.iter().rev().map(|&m| virtual_key(m, false)));
    send(&inputs)
}

/// Presses or releases a mouse button at screen coordinates (1 left, 2 right, 3 middle, 4-5 X).
pub fn mouse_button(button: u8, down: bool, x: i32, y: i32) -> bool {
    // SAFETY: GetSystemMetrics only reads its integer argument.
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2),
        )
    };
    let nx = (i64::from(x - vx) * 65_535 / i64::from(vw - 1)) as i32;
    let ny = (i64::from(y - vy) * 65_535 / i64::from(vh - 1)) as i32;
    let (flag, data): (MOUSE_EVENT_FLAGS, u32) = match (button, down) {
        (1, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (1, false) => (MOUSEEVENTF_LEFTUP, 0),
        (2, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (2, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (3, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (3, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (b, true) => (MOUSEEVENTF_XDOWN, u32::from(b.saturating_sub(3))),
        (b, false) => (MOUSEEVENTF_XUP, u32::from(b.saturating_sub(3))),
    };
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: nx,
                dy: ny,
                mouseData: data,
                dwFlags: flag | MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                time: 0,
                dwExtraInfo: MARKER,
            },
        },
    };
    send(&[input])
}

/// True when another app (or Windows) already owns this shortcut.
pub fn hotkey_in_use(binding: &Binding) -> bool {
    let mut mods = MOD_NOREPEAT;
    if binding.mods.ctrl {
        mods |= MOD_CONTROL;
    }
    if binding.mods.alt {
        mods |= MOD_ALT;
    }
    if binding.mods.shift {
        mods |= MOD_SHIFT;
    }
    if binding.mods.win {
        mods |= MOD_WIN;
    }
    const PROBE_ID: i32 = 0x4C4C;
    // SAFETY: registers a thread hotkey without a window and removes it again at once.
    unsafe {
        match RegisterHotKey(None, PROBE_ID, mods, u32::from(binding.key)) {
            Ok(()) => {
                let _ = UnregisterHotKey(None, PROBE_ID);
                false
            }
            Err(_) => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_qt_class;

    #[test]
    fn qt_windows_are_recognized_by_class() {
        assert!(is_qt_class("Qt51514QWindowIcon"));
        assert!(is_qt_class("Qt5QWindowIcon"));
        assert!(is_qt_class("Qt662QWindowOwnDCIcon"));
        assert!(!is_qt_class("Chrome_WidgetWin_1"));
        assert!(!is_qt_class("Edit"));
        assert!(!is_qt_class("QtWebEngine"));
    }
}
