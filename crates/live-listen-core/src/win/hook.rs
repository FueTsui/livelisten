//! The input hook thread: global shortcuts, ending continuous dictation, holding input back
//! while the last text is written, and foreground tracking.
//!
//! Hook callbacks only run the pure [`Matcher`] and queue work, so they return quickly.
//! Synthetic input is sent from the message loop after the callback has returned.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentThreadId, INFINITE};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext};
use windows::Win32::UI::Input::{RAWINPUTDEVICE, RIDEV_INPUTSINK, RIDEV_REMOVE, RegisterRawInputDevices};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EVENT_SYSTEM_DESKTOPSWITCH,
    EVENT_SYSTEM_FOREGROUND, HC_ACTION, HHOOK, HWND_MESSAGE, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, MSG, MSLLHOOKSTRUCT,
    MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, OBJID_WINDOW, PM_NOREMOVE, PM_REMOVE, PeekMessageW,
    PostThreadMessageW, QS_ALLINPUT, RegisterClassW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WINDOW_EX_STYLE, WINDOW_STYLE, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP,
    WM_INPUT, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_QUIT, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SYSKEYDOWN, WM_USER, WM_XBUTTONDOWN, WM_XBUTTONUP, WNDCLASSW,
};
use windows::core::w;

use super::{focus, input};
use crate::config::HotkeySettings;
use crate::hotkey::{Binding, Chord, Matcher, Mode, Signal, is_modifier, vk};

/// Keys and clicks pressed while the last text is on its way are held back so they land after
/// it, but at most this long after the hotkey was let go. Without any held input, the wait is
/// up to the engine.
pub const BUFFER_LIMIT: Duration = Duration::from_secs(4);

/// Keyboard events seen by raw input with no hook callback in between: Windows removed the
/// hook (it does so silently when a callback is too slow). Typing about eight keys, or holding
/// the hold key for half a second, is enough to notice.
const HOOK_SILENT_EVENTS: u32 = 16;
/// Waits before trying again when a fresh hook still sees nothing (for example while an
/// elevated app in front hides its input), so a hook that cannot help is not churned.
const REINSTALL_BACKOFF: [Duration; 3] = [Duration::from_secs(30), Duration::from_secs(120), Duration::from_secs(600)];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bindings {
    pub hold: Option<Binding>,
    pub toggle: Option<Binding>,
    pub captions: Option<Binding>,
    pub note: Option<Binding>,
    pub cards: Option<Binding>,
}

impl Bindings {
    pub fn from_settings(hotkeys: &HotkeySettings) -> Bindings {
        let parse = |s: &str| if s.trim().is_empty() { None } else { Binding::parse(s).ok() };
        Bindings {
            hold: parse(&hotkeys.hold),
            toggle: parse(&hotkeys.toggle),
            captions: parse(&hotkeys.captions),
            note: parse(&hotkeys.note),
            cards: parse(&hotkeys.cards),
        }
    }

    fn apply(&self, matcher: &mut Matcher) {
        matcher.set_bindings(self.hold, self.toggle, self.captions);
        matcher.set_more(self.note, self.cards);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookEvent {
    Signal(Signal),
    /// Another app's window came to the front.
    Foreground {
        window: isize,
        input_count: u64,
    },
    /// An edit reached another app; any planned correction is no longer safe.
    Edited,
    /// Held-back input was released because the engine did not answer in time.
    TimedOut,
    /// The keyboard hook could not be reinstalled (`false`), or works again (`true`).
    Health(bool),
}

pub type HookSink = Box<dyn Fn(HookEvent) + Send>;

/// Key presses and clicks of the user that reached other apps (not swallowed, not injected
/// by this app).
static USER_INPUT: AtomicU64 = AtomicU64::new(0);
/// Invalidates input immediately, even if the engine has not consumed the hook event yet.
static INPUT_REVISION: AtomicU64 = AtomicU64::new(0);
/// Keyboard hook callbacks so far.
static HOOK_CALLS: AtomicU64 = AtomicU64::new(0);
/// Raw keyboard events since the last keyboard hook callback.
static RAW_UNSEEN: AtomicU32 = AtomicU32::new(0);
/// The input desktop changed (lock screen, Ctrl+Alt+Del, UAC and back).
static DESKTOP_SWITCHED: AtomicBool = AtomicBool::new(false);

pub fn input_revision() -> u64 {
    INPUT_REVISION.load(Ordering::Acquire)
}

/// Grows with every key press or click of the user that reached another app.
pub fn user_input_count() -> u64 {
    USER_INPUT.load(Ordering::Relaxed)
}

enum Command {
    Bindings(Bindings),
    Begin(Mode),
    HoldInput,
    Release(bool),
    Record(bool),
}

#[derive(Clone, Copy, Debug)]
struct KeyEvent {
    vk: u16,
    scan: u16,
    extended: bool,
    down: bool,
}

#[derive(Clone, Debug)]
enum Replay {
    Key(Chord),
    Click { button: u8, x: i32, y: i32 },
}

#[derive(Clone, Debug)]
enum Inject {
    Mask,
    /// Mask first, then send the key-up that was held back, so Start or a menu stays closed.
    MaskThenUp(KeyEvent),
    Replay(Replay),
    Key(KeyEvent),
    /// Mask, then let go of the hold modifier in the system (see `Matcher::tick`).
    ReleaseHeld(u16),
    /// Press a modifier again before a key that belongs with it.
    Press(u16),
    Mouse {
        button: u8,
        down: bool,
        x: i32,
        y: i32,
    },
}

struct State {
    matcher: Matcher,
    mode: Mode,
    dictating: bool,
    pending: Option<Replay>,
    buffer: Vec<Inject>,
    buffer_since: Option<Instant>,
    inject: Vec<Inject>,
    sink: HookSink,
    mouse: Option<HHOOK>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.with(|cell| cell.try_borrow_mut().ok().and_then(|mut guard| guard.as_mut().map(f)))
}

impl State {
    fn on_key(&mut self, ev: KeyEvent) -> bool {
        if self.mode == Mode::Buffering {
            if !ev.down && self.matcher.take_up(ev.vk) {
                return true;
            }
            self.matcher.track(ev.vk, ev.down);
            if !self.has_held_input() {
                self.buffer_since = Some(Instant::now());
            }
            self.buffer.push(Inject::Key(ev));
            return true;
        }
        let decision = self.matcher.key(ev.vk, ev.down, self.mode, Instant::now());
        let mut swallow = decision.swallow;
        if decision.mask {
            let releases_mask_key = !ev.down && matches!(ev.vk, vk::LWIN | vk::RWIN | vk::LMENU | vk::RMENU);
            if releases_mask_key && !swallow {
                swallow = true;
                self.inject.push(Inject::MaskThenUp(ev));
            } else {
                self.inject.push(Inject::Mask);
            }
        }
        if let Some(m) = decision.repress {
            self.inject.push(Inject::Press(m));
            self.inject.push(Inject::Key(ev));
        }
        // Continuous mode lets modifiers through while assembling an ending chord. They
        // have not edited the target: Ctrl+Enter must still allow the final text to finish.
        let chord_prefix = matches!(self.mode, Mode::Continuous { .. }) && is_modifier(ev.vk);
        self.handle(decision.signals);
        if ev.down && (!swallow || decision.repress.is_some()) && !chord_prefix && self.dictating {
            self.invalidate_input();
        }
        swallow
    }

    fn on_mouse(&mut self, button: u8, down: bool, x: i32, y: i32) -> bool {
        if self.mode == Mode::Buffering {
            if !down && self.matcher.take_mouse_up() {
                return true;
            }
            if !self.has_held_input() {
                self.buffer_since = Some(Instant::now());
            }
            self.buffer.push(Inject::Mouse { button, down, x, y });
            return true;
        }
        let own = down && focus::own_window_at(x, y);
        let decision = self.matcher.mouse(button, down, x, y, own, self.mode);
        self.handle(decision.signals);
        if down && !decision.swallow && !own && self.dictating {
            self.invalidate_input();
        }
        decision.swallow
    }

    fn invalidate_input(&self) {
        INPUT_REVISION.fetch_add(1, Ordering::AcqRel);
        (self.sink)(HookEvent::Edited);
    }

    fn handle(&mut self, signals: Vec<Signal>) {
        for signal in signals {
            match &signal {
                Signal::HoldEnd if self.dictating => self.start_buffering(None),
                Signal::EndedByKey { replay, chord } => {
                    self.start_buffering(replay.then(|| Replay::Key(chord.clone())));
                }
                Signal::EndedByClick { replay, button, x, y } => {
                    self.start_buffering(replay.then_some(Replay::Click { button: *button, x: *x, y: *y }));
                }
                Signal::EndedByTap => self.start_buffering(None),
                Signal::Recorded(_) => self.set_mode(Mode::Normal),
                _ => {}
            }
            (self.sink)(HookEvent::Signal(signal));
        }
    }

    fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.matcher.enter(mode);
    }

    fn start_buffering(&mut self, pending: Option<Replay>) {
        self.pending = pending;
        self.buffer_since = Some(Instant::now());
        self.set_mode(Mode::Buffering);
    }

    fn release(&mut self, replay: bool) {
        if let Some(p) = self.pending.take() {
            if replay {
                self.inject.push(Inject::Replay(p));
            }
        }
        self.inject.append(&mut self.buffer);
        self.buffer_since = None;
        self.dictating = false;
        self.set_mode(Mode::Normal);
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::Bindings(b) => b.apply(&mut self.matcher),
            Command::Begin(mode) => {
                if self.mode == Mode::Buffering {
                    self.release(true);
                }
                self.dictating = true;
                self.set_mode(mode);
            }
            Command::HoldInput => {
                if self.dictating && self.mode != Mode::Buffering {
                    self.start_buffering(None);
                }
            }
            Command::Release(replay) => {
                if self.mode == Mode::Buffering {
                    self.release(replay);
                } else if self.mode != Mode::Recording {
                    self.dictating = false;
                    self.set_mode(Mode::Normal);
                }
            }
            Command::Record(on) => {
                if on && self.mode == Mode::Normal && !self.dictating {
                    self.set_mode(Mode::Recording);
                } else if !on && self.mode == Mode::Recording {
                    self.set_mode(Mode::Normal);
                }
            }
        }
    }

    fn deadline(&self) -> Option<Instant> {
        let buffer = self.buffer_since.filter(|_| self.has_held_input()).map(|t| t + BUFFER_LIMIT);
        match (self.matcher.next_deadline(), buffer) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn on_timer(&mut self, now: Instant) {
        let signals = self.matcher.tick(now);
        if let Some(key) = self.matcher.take_release() {
            self.inject.push(Inject::ReleaseHeld(key));
        }
        self.handle(signals);
        if self.has_held_input() && self.buffer_since.is_some_and(|t| now >= t + BUFFER_LIMIT) {
            log::warn!("held-back input released after timeout");
            // Invalidate before replay, rather than waiting for the engine's message queue.
            INPUT_REVISION.fetch_add(1, Ordering::AcqRel);
            self.release(true);
            (self.sink)(HookEvent::TimedOut);
        }
    }

    fn wants_mouse(&self) -> bool {
        self.dictating
            || matches!(self.mode, Mode::Continuous { .. } | Mode::Buffering)
            || self.matcher.next_deadline().is_some()
    }

    fn has_held_input(&self) -> bool {
        self.pending.is_some() || !self.buffer.is_empty()
    }

    /// Forgets held keys whose key-ups this hook cannot have seen; ends a running hold.
    fn reset_keys(&mut self) {
        let signals = self.matcher.reset_keys();
        self.handle(signals);
    }
}

/// Decides when to reinstall a keyboard hook that went silent.
#[derive(Debug, Default)]
struct Watchdog {
    /// Reinstalls in a row that brought no callbacks back.
    streak: usize,
    /// `HOOK_CALLS` at the last reinstall.
    calls: u64,
    next: Option<Instant>,
}

impl Watchdog {
    /// True when the silent hook should be reinstalled now. The first attempt of an incident is
    /// immediate; while the fresh hook sees nothing either, attempts back off.
    fn due(&mut self, now: Instant, calls: u64) -> bool {
        if calls != self.calls {
            // Callbacks arrived since the last reinstall: this is a new incident.
            self.streak = 0;
            self.next = None;
        }
        if self.next.is_some_and(|t| now < t) {
            return false;
        }
        self.next = Some(now + REINSTALL_BACKOFF[self.streak.min(REINSTALL_BACKOFF.len() - 1)]);
        self.streak += 1;
        self.calls = calls;
        true
    }
}

fn perform(injections: Vec<Inject>) {
    for item in injections {
        match item {
            Inject::Mask => {
                input::mask();
            }
            Inject::MaskThenUp(ev) => {
                input::mask();
                input::raw_key(ev.vk, ev.scan, ev.extended, false);
            }
            Inject::Replay(Replay::Key(chord)) => {
                input::chord(&chord);
            }
            Inject::Replay(Replay::Click { button, x, y }) => {
                input::mouse_button(button, true, x, y);
            }
            Inject::Key(ev) => {
                input::raw_key(ev.vk, ev.scan, ev.extended, ev.down);
            }
            Inject::ReleaseHeld(key) => {
                // The mask keeps a lone tap from opening Start or a menu, switching the
                // input method (Shift) or showing the pointer (Ctrl).
                input::mask();
                input::modifier(key, false);
            }
            Inject::Press(key) => {
                input::modifier(key, true);
            }
            Inject::Mouse { button, down, x, y } => {
                input::mouse_button(button, down, x, y);
            }
        }
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    HOOK_CALLS.fetch_add(1, Ordering::Relaxed);
    RAW_UNSEEN.store(0, Ordering::Relaxed);
    if code == HC_ACTION as i32 {
        // SAFETY: for HC_ACTION, lparam points to a KBDLLHOOKSTRUCT that is valid during the call.
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if info.dwExtraInfo != input::MARKER {
            let ev = KeyEvent {
                vk: info.vkCode as u16,
                scan: info.scanCode as u16,
                extended: info.flags.0 & LLKHF_EXTENDED.0 != 0,
                down: matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN),
            };
            if with_state(|s| s.on_key(ev)) == Some(true) {
                return LRESULT(1);
            }
            if ev.down {
                USER_INPUT.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    // SAFETY: passes the event on unchanged.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn mouse_button_of(message: u32, data: u32) -> Option<(u8, bool)> {
    let x = if (data >> 16) & 0xFFFF == 2 { 5 } else { 4 };
    match message {
        WM_LBUTTONDOWN => Some((1, true)),
        WM_LBUTTONUP => Some((1, false)),
        WM_RBUTTONDOWN => Some((2, true)),
        WM_RBUTTONUP => Some((2, false)),
        WM_MBUTTONDOWN => Some((3, true)),
        WM_MBUTTONUP => Some((3, false)),
        WM_XBUTTONDOWN => Some((x, true)),
        WM_XBUTTONUP => Some((x, false)),
        _ => None,
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for HC_ACTION, lparam points to an MSLLHOOKSTRUCT that is valid during the call.
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if info.dwExtraInfo != input::MARKER {
            if let Some((button, down)) = mouse_button_of(wparam.0 as u32, info.mouseData) {
                if with_state(|s| s.on_mouse(button, down, info.pt.x, info.pt.y)) == Some(true) {
                    return LRESULT(1);
                }
                if down && !focus::own_window_at(info.pt.x, info.pt.y) {
                    USER_INPUT.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
    // SAFETY: passes the event on unchanged.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn foreground_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    if event == EVENT_SYSTEM_FOREGROUND && object == OBJID_WINDOW.0 {
        let input_count = user_input_count();
        with_state(|s| (s.sink)(HookEvent::Foreground { window: hwnd.0 as isize, input_count }));
    }
}

unsafe extern "system" fn desktop_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    _hwnd: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    if event == EVENT_SYSTEM_DESKTOPSWITCH {
        DESKTOP_SWITCHED.store(true, Ordering::Release);
    }
}

/// Counts keyboard input that reaches this desktop, whether or not the hook sees it.
unsafe extern "system" fn raw_input_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_INPUT {
        RAW_UNSEEN.fetch_add(1, Ordering::Relaxed);
    }
    // SAFETY: default handling, which also frees the raw input buffer of WM_INPUT.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn install_keyboard() -> windows::core::Result<HHOOK> {
    // SAFETY: the callback is a valid low-level keyboard procedure for this thread's lifetime.
    unsafe {
        let module = GetModuleHandleW(None).ok();
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module.map(Into::into), 0)
    }
}

const WITNESS_CLASS: windows::core::PCWSTR = w!("LiveListenKeyboardWitness");

/// A message-only window that receives keyboard raw input in the background, as a witness
/// that keys are being pressed.
fn start_raw_input() -> Option<HWND> {
    // SAFETY: registers a window class with a valid procedure and creates a message-only
    // window on this thread; both live until `stop_raw_input`.
    unsafe {
        let instance: Option<HINSTANCE> = GetModuleHandleW(None).ok().map(Into::into);
        let class = WNDCLASSW {
            lpfnWndProc: Some(raw_input_proc),
            hInstance: instance.unwrap_or_default(),
            lpszClassName: WITNESS_CLASS,
            ..Default::default()
        };
        // Zero also means the class exists from an earlier hook thread; creation decides.
        RegisterClassW(&class);
        let window = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            WITNESS_CLASS,
            None,
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            instance,
            None,
        )
        .map_err(|e| log::warn!("keyboard watchdog window failed: {e}"))
        .ok()?;
        let device = RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x06, dwFlags: RIDEV_INPUTSINK, hwndTarget: window };
        if let Err(e) = RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32) {
            log::warn!("keyboard watchdog unavailable: {e}");
            let _ = DestroyWindow(window);
            return None;
        }
        Some(window)
    }
}

fn stop_raw_input(window: HWND) {
    let device =
        RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x06, dwFlags: RIDEV_REMOVE, hwndTarget: HWND::default() };
    // SAFETY: undoes `start_raw_input` on the thread that created the window.
    unsafe {
        let _ = RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32);
        let _ = DestroyWindow(window);
    }
}

/// The hook thread. Dropping it removes the hooks.
pub struct Hook {
    thread: u32,
    commands: Arc<Mutex<VecDeque<Command>>>,
    join: Option<JoinHandle<()>>,
}

impl Hook {
    pub fn start(bindings: Bindings, sink: HookSink) -> Result<Hook, String> {
        let commands = Arc::new(Mutex::new(VecDeque::new()));
        let queue = Arc::clone(&commands);
        let (ready_tx, ready_rx) = mpsc::channel();
        let join = std::thread::Builder::new()
            .name("input-hook".into())
            .spawn(move || run(bindings, sink, queue, ready_tx))
            .map_err(|e| e.to_string())?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(thread)) => Ok(Hook { thread, commands, join: Some(join) }),
            Ok(Err(e)) => {
                let _ = join.join();
                Err(e)
            }
            Err(_) => Err("input hook did not start".into()),
        }
    }

    fn post(&self, command: Command) {
        self.commands.lock().unwrap_or_else(|e| e.into_inner()).push_back(command);
        // SAFETY: posting to a thread id has no memory-safety requirements.
        let _ = unsafe { PostThreadMessageW(self.thread, WM_APP, WPARAM(0), LPARAM(0)) };
    }

    pub fn set_bindings(&self, bindings: Bindings) {
        self.post(Command::Bindings(bindings));
    }

    /// A dictation session started (hold-to-talk uses `Mode::Normal`).
    pub fn begin(&self, mode: Mode) {
        self.post(Command::Begin(mode));
    }

    /// Holds back input while the last text is written.
    pub fn hold_input(&self) {
        self.post(Command::HoldInput);
    }

    /// Ends the session: replays the ending key or click if asked, then the held-back input.
    pub fn release(&self, replay: bool) {
        self.post(Command::Release(replay));
    }

    pub fn record(&self, on: bool) {
        self.post(Command::Record(on));
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        // SAFETY: posting to a thread id has no memory-safety requirements.
        let _ = unsafe { PostThreadMessageW(self.thread, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn sync_mouse_hook() {
    let want = with_state(|s| (s.wants_mouse(), s.mouse.is_some()));
    match want {
        Some((true, false)) => {
            // SAFETY: the callback is a valid low-level mouse procedure for this thread's lifetime.
            let hook = unsafe {
                let module = GetModuleHandleW(None).ok();
                SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module.map(Into::into), 0)
            };
            match hook {
                Ok(h) => {
                    with_state(|s| s.mouse = Some(h));
                }
                Err(e) => log::warn!("mouse hook failed: {e}"),
            }
        }
        Some((false, true)) => {
            if let Some(h) = with_state(|s| s.mouse.take()).flatten() {
                // SAFETY: `h` was returned by SetWindowsHookExW on this thread.
                let _ = unsafe { UnhookWindowsHookEx(h) };
            }
        }
        _ => {}
    }
}

fn run(
    bindings: Bindings,
    sink: HookSink,
    commands: Arc<Mutex<VecDeque<Command>>>,
    ready: mpsc::Sender<Result<u32, String>>,
) {
    let mut msg = MSG::default();
    // SAFETY: creates this thread's message queue so posted commands are not lost.
    unsafe {
        let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let mut keyboard = match install_keyboard() {
        Ok(h) => Some(h),
        Err(e) => {
            let _ = ready.send(Err(format!("keyboard hook failed: {e}")));
            return;
        }
    };
    // SAFETY: out-of-context WinEvent hooks call back on this thread while it pumps messages.
    let foreground = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(foreground_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    // SAFETY: as above; the secure desktop takes the keyboard without key-ups reaching here.
    let desktop = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_DESKTOPSWITCH,
            EVENT_SYSTEM_DESKTOPSWITCH,
            None,
            Some(desktop_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    };
    let witness = start_raw_input();
    let mut watchdog = Watchdog::default();
    let mut healthy = true;
    let mut matcher = Matcher::default();
    bindings.apply(&mut matcher);
    STATE.with(|cell| {
        *cell.borrow_mut() = Some(State {
            matcher,
            mode: Mode::Normal,
            dictating: false,
            pending: None,
            buffer: Vec::new(),
            buffer_since: None,
            inject: Vec::new(),
            sink,
            mouse: None,
        });
    });
    // SAFETY: GetCurrentThreadId has no preconditions.
    let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));
    'outer: loop {
        let timeout = with_state(|s| s.deadline())
            .flatten()
            .map(|d| d.saturating_duration_since(Instant::now()).as_millis().min(u128::from(u32::MAX - 1)) as u32)
            .unwrap_or(INFINITE);
        // SAFETY: waits on no handles; returns when input arrives or the timeout elapses.
        unsafe { MsgWaitForMultipleObjectsEx(None, timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
        // SAFETY: standard message pump on this thread's queue; hook callbacks run in here.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_QUIT {
                break 'outer;
            }
            if msg.message != WM_APP {
                // SAFETY: `msg` was filled by PeekMessageW.
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        let switched = DESKTOP_SWITCHED.swap(false, Ordering::AcqRel);
        let silent = witness.is_some() && RAW_UNSEEN.load(Ordering::Relaxed) >= HOOK_SILENT_EVENTS;
        if switched || (silent && watchdog.due(Instant::now(), HOOK_CALLS.load(Ordering::Relaxed))) {
            if silent {
                log::warn!("keyboard hook stopped receiving input; reinstalling");
            } else {
                log::info!("input desktop switched; key state reset");
            }
            RAW_UNSEEN.store(0, Ordering::Relaxed);
            with_state(State::reset_keys);
            // A fresh hook also covers one Windows removed while the desktop was away.
            match install_keyboard() {
                Ok(fresh) => {
                    if let Some(old) = keyboard.replace(fresh) {
                        // SAFETY: `old` was installed by this thread; failure means Windows removed it.
                        let _ = unsafe { UnhookWindowsHookEx(old) };
                    }
                    if !healthy {
                        healthy = true;
                        log::info!("keyboard hook restored");
                        with_state(|s| (s.sink)(HookEvent::Health(true)));
                    }
                }
                Err(e) => {
                    log::error!("keyboard hook reinstall failed: {e}");
                    if healthy {
                        healthy = false;
                        with_state(|s| (s.sink)(HookEvent::Health(false)));
                    }
                }
            }
        }
        let pending: Vec<Command> = commands.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
        with_state(|s| {
            for command in pending {
                s.apply(command);
            }
            s.on_timer(Instant::now());
        });
        if let Some(injections) = with_state(|s| std::mem::take(&mut s.inject)) {
            perform(injections);
        }
        sync_mouse_hook();
    }
    if let Some(h) = with_state(|s| s.mouse.take()).flatten() {
        // SAFETY: `h` was returned by SetWindowsHookExW on this thread.
        let _ = unsafe { UnhookWindowsHookEx(h) };
    }
    if let Some(window) = witness {
        stop_raw_input(window);
    }
    // SAFETY: all hooks were installed by this thread.
    unsafe {
        if let Some(keyboard) = keyboard {
            let _ = UnhookWindowsHookEx(keyboard);
        }
        for hook in [foreground, desktop] {
            if !hook.is_invalid() {
                let _ = UnhookWinEvent(hook);
            }
        }
    }
    STATE.with(|cell| *cell.borrow_mut() = None);
}

#[cfg(test)]
mod tests {
    use super::*;

    static INPUT_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn state(sink: HookSink) -> State {
        State {
            matcher: Matcher::default(),
            mode: Mode::Normal,
            dictating: false,
            pending: None,
            buffer: Vec::new(),
            buffer_since: None,
            inject: Vec::new(),
            sink,
            mouse: None,
        }
    }

    #[test]
    fn hold_dictation_keeps_mouse_tracking_until_released() {
        let mut s = state(Box::new(|_| {}));
        assert!(!s.wants_mouse());
        s.apply(Command::Begin(Mode::Normal));
        assert!(s.wants_mouse(), "active hold mode still observes caret-moving clicks");
        s.apply(Command::Release(false));
        assert!(!s.wants_mouse());
    }

    #[test]
    fn ctrl_a_during_hold_invalidates_input_even_when_replayed() {
        let _serial = INPUT_TEST_LOCK.lock().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let output = Arc::clone(&events);
        let mut s = state(Box::new(move |event| output.lock().unwrap().push(event)));
        s.matcher.set_bindings(Some(Binding::parse("RCtrl").unwrap()), None, None);
        assert!(!s.on_key(KeyEvent { vk: vk::RCONTROL, scan: 0, extended: true, down: true }));
        s.on_timer(Instant::now() + Duration::from_millis(300));
        s.apply(Command::Begin(Mode::Normal));
        // Inspect queued injections only: this test never calls SendInput or starts hooks.
        s.inject.clear();
        let before = input_revision();
        assert!(s.on_key(KeyEvent { vk: u16::from(b'A'), scan: 0, extended: false, down: true }));
        assert!(
            matches!(s.inject.as_slice(), [Inject::Press(vk::RCONTROL), Inject::Key(key)] if key.vk == u16::from(b'A'))
        );
        assert_ne!(input_revision(), before);
        let got = events.lock().unwrap();
        assert!(got.contains(&HookEvent::Signal(Signal::HoldInterrupted)));
        assert!(got.contains(&HookEvent::Edited), "a replayed Ctrl+A can select existing text");
    }

    #[test]
    fn continuous_ctrl_enter_and_shift_enter_preserve_final_input() {
        let _serial = INPUT_TEST_LOCK.lock().unwrap();
        for modifier in [vk::LCONTROL, vk::LSHIFT] {
            let events = Arc::new(Mutex::new(Vec::new()));
            let output = Arc::clone(&events);
            let mut s = state(Box::new(move |event| output.lock().unwrap().push(event)));
            s.apply(Command::Begin(Mode::Continuous { replay: true }));
            let before = input_revision();
            assert!(!s.on_key(KeyEvent { vk: modifier, scan: 0, extended: false, down: true }));
            assert_eq!(input_revision(), before, "a chord prefix does not invalidate final text");
            assert!(events.lock().unwrap().is_empty());
            assert!(s.on_key(KeyEvent { vk: vk::RETURN, scan: 0, extended: false, down: true }));
            let chord = Chord { key: vk::RETURN, mods: vec![modifier] };
            assert_eq!(
                events.lock().unwrap().as_slice(),
                &[HookEvent::Signal(Signal::EndedByKey { replay: true, chord: chord.clone() })]
            );
            assert!(matches!(&s.pending, Some(Replay::Key(pending)) if *pending == chord));
            assert_eq!(s.mode, Mode::Buffering);
            assert_eq!(input_revision(), before, "the ending chord waits for final text before replay");
        }
    }

    #[test]
    fn pending_enter_alone_times_out_before_replay() {
        let _serial = INPUT_TEST_LOCK.lock().unwrap();
        let before = input_revision();
        let events = Arc::new(Mutex::new(Vec::new()));
        let output = Arc::clone(&events);
        let mut s = state(Box::new(move |event| {
            if event == HookEvent::TimedOut {
                assert_ne!(input_revision(), before, "input is invalidated before the event is delivered");
            }
            output.lock().unwrap().push(event);
        }));
        s.dictating = true;
        s.start_buffering(Some(Replay::Key(Chord { key: vk::RETURN, mods: Vec::new() })));
        let due = s.buffer_since.unwrap() + BUFFER_LIMIT;
        assert_eq!(s.deadline(), Some(due));
        assert!(s.buffer.is_empty());
        s.on_timer(due);
        assert!(s.pending.is_none());
        assert_eq!(s.mode, Mode::Normal);
        assert!(matches!(s.inject.as_slice(), [Inject::Replay(Replay::Key(_))]));
        assert!(events.lock().unwrap().contains(&HookEvent::TimedOut));
    }

    #[test]
    fn first_held_key_gets_its_own_buffer_period() {
        let mut s = state(Box::new(|_| {}));
        s.start_buffering(None);
        s.buffer_since = Some(Instant::now() - BUFFER_LIMIT * 2);
        assert!(s.deadline().is_none(), "waiting for recognition alone never holds user input");
        s.on_key(KeyEvent { vk: 0x41, scan: 0, extended: false, down: true });
        assert!(s.deadline().unwrap() > Instant::now() + BUFFER_LIMIT / 2);
    }

    #[test]
    fn desktop_switch_during_a_hold_ends_dictation_and_holds_input() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let output = Arc::clone(&events);
        let mut s = state(Box::new(move |event| output.lock().unwrap().push(event)));
        s.matcher.set_bindings(Some(Binding::parse("RCtrl").unwrap()), None, None);
        let t0 = Instant::now();
        s.matcher.key(vk::RCONTROL, true, Mode::Normal, t0);
        s.on_timer(t0 + Duration::from_millis(300));
        s.apply(Command::Begin(Mode::Normal));
        s.inject.clear();
        s.reset_keys();
        assert!(events.lock().unwrap().contains(&HookEvent::Signal(Signal::HoldEnd)));
        assert_eq!(s.mode, Mode::Buffering, "the last text is written before input moves on");
        assert!(!s.matcher.holding());
    }

    #[test]
    fn watchdog_reinstalls_at_once_then_backs_off_while_still_silent() {
        let mut w = Watchdog::default();
        let t0 = Instant::now();
        assert!(w.due(t0, 100), "first silence of an incident");
        assert!(!w.due(t0 + Duration::from_secs(5), 100), "the fresh hook saw nothing either");
        assert!(w.due(t0 + REINSTALL_BACKOFF[0], 100));
        assert!(!w.due(t0 + REINSTALL_BACKOFF[0] + Duration::from_secs(60), 100));
        assert!(w.due(t0 + REINSTALL_BACKOFF[0] + REINSTALL_BACKOFF[1], 100));
        let late = t0 + Duration::from_secs(3600);
        assert!(w.due(late, 100));
        assert!(!w.due(late + REINSTALL_BACKOFF[2] - Duration::from_secs(1), 100), "backoff stays at its cap");
        assert!(w.due(late + Duration::from_secs(1), 250), "callbacks came back: a new incident starts fresh");
    }

    #[test]
    fn an_edit_invalidates_planned_input_without_waiting_for_engine() {
        let _serial = INPUT_TEST_LOCK.lock().unwrap();
        let mut s = state(Box::new(|_| {}));
        s.apply(Command::Begin(Mode::Normal));
        let revision = input_revision();
        s.invalidate_input();
        assert_ne!(revision, input_revision());
    }
}
