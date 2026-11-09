//! Shortcut bindings and the pure key matcher behind the low-level keyboard hook.
//!
//! The matcher decides, for every physical key or mouse button event, whether the event is
//! passed on or swallowed and which signals the engine receives. It holds no OS state, so the
//! rules (hold to talk, continuous dictation ended by any input, shortcut capture) are testable.

use std::fmt;
use std::time::{Duration, Instant};

/// Windows virtual-key codes used by the matcher.
pub mod vk {
    pub const BACK: u16 = 0x08;
    pub const TAB: u16 = 0x09;
    pub const RETURN: u16 = 0x0D;
    pub const SHIFT: u16 = 0x10;
    pub const CONTROL: u16 = 0x11;
    pub const MENU: u16 = 0x12;
    pub const PAUSE: u16 = 0x13;
    pub const CAPITAL: u16 = 0x14;
    pub const ESCAPE: u16 = 0x1B;
    pub const SPACE: u16 = 0x20;
    pub const PRIOR: u16 = 0x21;
    pub const NEXT: u16 = 0x22;
    pub const END: u16 = 0x23;
    pub const HOME: u16 = 0x24;
    pub const LEFT: u16 = 0x25;
    pub const UP: u16 = 0x26;
    pub const RIGHT: u16 = 0x27;
    pub const DOWN: u16 = 0x28;
    pub const SNAPSHOT: u16 = 0x2C;
    pub const INSERT: u16 = 0x2D;
    pub const DELETE: u16 = 0x2E;
    pub const LWIN: u16 = 0x5B;
    pub const RWIN: u16 = 0x5C;
    pub const APPS: u16 = 0x5D;
    pub const NUMPAD0: u16 = 0x60;
    pub const MULTIPLY: u16 = 0x6A;
    pub const ADD: u16 = 0x6B;
    pub const SUBTRACT: u16 = 0x6D;
    pub const DECIMAL: u16 = 0x6E;
    pub const DIVIDE: u16 = 0x6F;
    pub const F1: u16 = 0x70;
    pub const NUMLOCK: u16 = 0x90;
    pub const SCROLL: u16 = 0x91;
    pub const LSHIFT: u16 = 0xA0;
    pub const RSHIFT: u16 = 0xA1;
    pub const LCONTROL: u16 = 0xA2;
    pub const RCONTROL: u16 = 0xA3;
    pub const LMENU: u16 = 0xA4;
    pub const RMENU: u16 = 0xA5;
    /// Unassigned key injected to stop a lone Win or Alt release from opening Start or a menu.
    pub const MASK: u16 = 0xE8;
}

/// Name and virtual-key code of every key a binding may use, in display spelling.
const KEY_NAMES: &[(&str, u16)] = &[
    ("Space", vk::SPACE),
    ("Enter", vk::RETURN),
    ("Tab", vk::TAB),
    ("Esc", vk::ESCAPE),
    ("Backspace", vk::BACK),
    ("Insert", vk::INSERT),
    ("Delete", vk::DELETE),
    ("Home", vk::HOME),
    ("End", vk::END),
    ("PageUp", vk::PRIOR),
    ("PageDown", vk::NEXT),
    ("Left", vk::LEFT),
    ("Up", vk::UP),
    ("Right", vk::RIGHT),
    ("Down", vk::DOWN),
    ("CapsLock", vk::CAPITAL),
    ("ScrollLock", vk::SCROLL),
    ("Pause", vk::PAUSE),
    ("PrintScreen", vk::SNAPSHOT),
    ("NumLock", vk::NUMLOCK),
    ("Menu", vk::APPS),
    ("NumMultiply", vk::MULTIPLY),
    ("NumAdd", vk::ADD),
    ("NumSubtract", vk::SUBTRACT),
    ("NumDecimal", vk::DECIMAL),
    ("NumDivide", vk::DIVIDE),
    (";", 0xBA),
    ("=", 0xBB),
    (",", 0xBC),
    ("-", 0xBD),
    (".", 0xBE),
    ("/", 0xBF),
    ("`", 0xC0),
    ("[", 0xDB),
    ("\\", 0xDC),
    ("]", 0xDD),
    ("'", 0xDE),
    ("LCtrl", vk::LCONTROL),
    ("RCtrl", vk::RCONTROL),
    ("LAlt", vk::LMENU),
    ("RAlt", vk::RMENU),
    ("LShift", vk::LSHIFT),
    ("RShift", vk::RSHIFT),
    ("LWin", vk::LWIN),
    ("RWin", vk::RWIN),
];

/// Modifier state, independent of side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

impl Mods {
    pub fn is_empty(self) -> bool {
        self == Mods::default()
    }
}

/// A shortcut: modifiers plus one key. A lone side-specific modifier such as `RCtrl` is
/// supported by the two hold actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub mods: Mods,
    pub key: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid shortcut: {}", self.0)
    }
}

impl std::error::Error for ParseError {}

pub fn is_modifier(key: u16) -> bool {
    matches!(
        key,
        vk::SHIFT
            | vk::CONTROL
            | vk::MENU
            | vk::LSHIFT
            | vk::RSHIFT
            | vk::LCONTROL
            | vk::RCONTROL
            | vk::LMENU
            | vk::RMENU
            | vk::LWIN
            | vk::RWIN
    )
}

fn key_from_name(name: &str) -> Option<u16> {
    let lower = name.to_ascii_lowercase();
    if lower.len() == 1 {
        let c = lower.as_bytes()[0];
        if c.is_ascii_alphanumeric() {
            return Some(u16::from(c.to_ascii_uppercase()));
        }
    }
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u16>().ok()) {
        if (1..=24).contains(&n) {
            return Some(vk::F1 + n - 1);
        }
    }
    if let Some(n) = lower.strip_prefix("num").and_then(|n| n.parse::<u16>().ok()) {
        if n <= 9 {
            return Some(vk::NUMPAD0 + n);
        }
    }
    let alias = match lower.as_str() {
        "escape" => Some(vk::ESCAPE),
        "return" => Some(vk::RETURN),
        "pgup" => Some(vk::PRIOR),
        "pgdn" => Some(vk::NEXT),
        "rightctrl" | "rcontrol" => Some(vk::RCONTROL),
        "leftctrl" | "lcontrol" => Some(vk::LCONTROL),
        "rightalt" | "altgr" => Some(vk::RMENU),
        "leftalt" => Some(vk::LMENU),
        _ => None,
    };
    alias.or_else(|| KEY_NAMES.iter().find(|(n, _)| n.eq_ignore_ascii_case(&lower)).map(|&(_, code)| code))
}

/// Display name of a virtual key.
pub fn key_name(key: u16) -> String {
    match key {
        0x30..=0x39 | 0x41..=0x5A => char::from(key as u8).to_string(),
        k if (vk::F1..vk::F1 + 24).contains(&k) => format!("F{}", k - vk::F1 + 1),
        k if (vk::NUMPAD0..=vk::NUMPAD0 + 9).contains(&k) => format!("Num{}", k - vk::NUMPAD0),
        k => KEY_NAMES
            .iter()
            .find(|&&(_, code)| code == k)
            .map(|(n, _)| (*n).to_string())
            .unwrap_or_else(|| format!("VK{k:02X}")),
    }
}

impl Binding {
    pub fn parse(text: &str) -> Result<Binding, ParseError> {
        let tokens: Vec<&str> = text.split('+').map(str::trim).collect();
        if tokens.iter().any(|t| t.is_empty()) {
            return Err(ParseError(text.into()));
        }
        let (last, rest) = tokens.split_last().ok_or_else(|| ParseError(text.into()))?;
        let mut mods = Mods::default();
        for token in rest {
            match token.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => mods.ctrl = true,
                "alt" => mods.alt = true,
                "shift" => mods.shift = true,
                "win" | "windows" => mods.win = true,
                _ => return Err(ParseError(text.into())),
            }
        }
        let key = key_from_name(last).ok_or_else(|| ParseError(text.into()))?;
        if is_modifier(key) && !mods.is_empty() {
            return Err(ParseError(text.into()));
        }
        Ok(Binding { mods, key })
    }

    /// True for a lone modifier such as `RCtrl`.
    pub fn is_modifier_only(&self) -> bool {
        is_modifier(self.key)
    }
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts: Vec<String> = Vec::new();
        if self.mods.ctrl {
            parts.push("Ctrl".into());
        }
        if self.mods.alt {
            parts.push("Alt".into());
        }
        if self.mods.shift {
            parts.push("Shift".into());
        }
        if self.mods.win {
            parts.push("Win".into());
        }
        parts.push(key_name(self.key));
        f.write_str(&parts.join("+"))
    }
}

/// Which input state the engine is in, as seen by the hook.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Idle, or hold-to-talk dictation.
    #[default]
    Normal,
    /// Continuous dictation: the next key or click ends it.
    Continuous { replay: bool },
    /// Waiting for the final text; all input is held back and replayed afterwards.
    Buffering,
    /// Capturing a new shortcut for the settings page.
    Recording,
}

/// A key together with the side-specific modifiers held when it was pressed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chord {
    pub mods: Vec<u16>,
    pub key: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signal {
    /// The hold key went down: a good moment to warm up the connection.
    HoldArmed,
    HoldStart,
    HoldEnd,
    /// Another key was pressed while hold-to-talk was running.
    HoldInterrupted,
    /// The hold key was tapped or combined with another key; no dictation.
    HoldDisarmed,
    Toggle,
    Captions,
    NoteHoldStart,
    NoteHoldEnd,
    /// Same action as the tray's spoken note entry.
    NoteToggle,
    /// Open a blank note card for editing on the desktop.
    NoteBlank,
    /// Show or hide the desktop cards.
    Cards,
    /// Continuous dictation was ended by this key; replay it after the last text if asked.
    EndedByKey {
        replay: bool,
        chord: Chord,
    },
    /// Continuous dictation was ended by a modifier tap (never replayed).
    EndedByTap,
    /// Continuous dictation was ended by a mouse button.
    EndedByClick {
        replay: bool,
        button: u8,
        x: i32,
        y: i32,
    },
    /// A shortcut was captured; empty means the capture was cancelled with Esc.
    Recorded(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    pub swallow: bool,
    /// Inject the mask key so a held Win or Alt does not open Start or a menu on release.
    pub mask: bool,
    /// Press this modifier again, then send the swallowed key: the user pressed a shortcut
    /// with the hold modifier after the system was told it is up.
    pub repress: Option<u16>,
    pub signals: Vec<Signal>,
}

impl Decision {
    fn pass() -> Self {
        Decision::default()
    }

    fn swallow(mask: bool) -> Self {
        Decision { swallow: true, mask, ..Decision::default() }
    }

    fn with(mut self, signal: Signal) -> Self {
        self.signals.push(signal);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hold {
    Idle,
    Armed(Instant),
    Active,
    /// Another key was involved; wait until the hold key is released.
    Suppressed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NoteHold {
    Idle,
    Armed { key: u16, since: Instant },
    Active(u16),
}

#[derive(Clone, Debug, Default)]
struct Recording {
    mods_seen: Vec<u16>,
    swallowed: Vec<u16>,
}

/// Key matcher. Feed it physical (not self-injected) events in order.
#[derive(Clone, Debug)]
pub struct Matcher {
    pub hold: Option<Binding>,
    pub toggle: Option<Binding>,
    pub captions: Option<Binding>,
    pub note: Option<Binding>,
    pub note_toggle: Option<Binding>,
    pub note_blank: Option<Binding>,
    pub cards: Option<Binding>,
    /// How long a lone modifier must be held before dictation starts.
    pub hold_delay: Duration,
    pressed: Vec<u16>,
    hold_state: Hold,
    swallow_up: Vec<u16>,
    swallow_mouse_up: bool,
    pending_tap: Option<u16>,
    note_hold: NoteHold,
    capturing: bool,
    capture_guard: Option<Instant>,
    recording: Recording,
    /// The hold modifier, let go of in the system once dictation started (see `tick`).
    released: Option<u16>,
    /// That release, still to be sent by the hook.
    release_due: Option<u16>,
}

impl Default for Matcher {
    fn default() -> Self {
        Matcher {
            hold: None,
            toggle: None,
            captions: None,
            note: None,
            note_toggle: None,
            note_blank: None,
            cards: None,
            hold_delay: Duration::from_millis(250),
            pressed: Vec::new(),
            hold_state: Hold::Idle,
            swallow_up: Vec::new(),
            swallow_mouse_up: false,
            pending_tap: None,
            note_hold: NoteHold::Idle,
            capturing: false,
            capture_guard: None,
            recording: Recording::default(),
            released: None,
            release_due: None,
        }
    }
}

fn mod_of(key: u16, mods: &mut Mods) {
    match key {
        vk::LCONTROL | vk::RCONTROL | vk::CONTROL => mods.ctrl = true,
        vk::LMENU | vk::RMENU | vk::MENU => mods.alt = true,
        vk::LSHIFT | vk::RSHIFT | vk::SHIFT => mods.shift = true,
        vk::LWIN | vk::RWIN => mods.win = true,
        _ => {}
    }
}

impl Matcher {
    /// Replaces the bindings; hold state is reset.
    pub fn set_bindings(&mut self, hold: Option<Binding>, toggle: Option<Binding>, captions: Option<Binding>) {
        self.hold = hold;
        self.toggle = toggle;
        self.captions = captions;
        self.hold_state = Hold::Idle;
    }

    /// Replaces the note bindings while allowing an already active hold to finish.
    pub fn set_notes(&mut self, note: Option<Binding>, toggle: Option<Binding>, blank: Option<Binding>) {
        if self.note != note {
            self.disarm_note();
        }
        self.note = note;
        self.note_toggle = toggle;
        self.note_blank = blank;
    }

    fn disarm_note(&mut self) {
        if matches!(self.note_hold, NoteHold::Armed { .. }) {
            self.note_hold = NoteHold::Idle;
        }
    }

    /// Side-independent modifiers currently held, excluding `except`.
    fn mods(&self, except: u16) -> Mods {
        let mut mods = Mods::default();
        for &k in self.pressed.iter().filter(|&&k| k != except) {
            mod_of(k, &mut mods);
        }
        mods
    }

    fn held_modifiers(&self, except: u16) -> Vec<u16> {
        self.pressed.iter().copied().filter(|&k| k != except && is_modifier(k)).collect()
    }

    /// When the hold timer next needs a `tick`.
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.capturing {
            return None;
        }
        if let Some(until) = self.capture_guard {
            return self.pressed.is_empty().then_some(until);
        }
        let dictation = match self.hold_state {
            Hold::Armed(since) => Some(since),
            _ => None,
        };
        let note = match self.note_hold {
            NoteHold::Armed { since, .. } => Some(since),
            _ => None,
        };
        dictation.into_iter().chain(note).min().map(|since| since + self.hold_delay)
    }

    /// Starts recording once a lone modifier has been held long enough.
    pub fn tick(&mut self, now: Instant) -> Vec<Signal> {
        if self.capture_suppressed(now) {
            return Vec::new();
        }
        if let NoteHold::Armed { key, since } = self.note_hold {
            if now >= since + self.hold_delay {
                self.note_hold = NoteHold::Active(key);
                return vec![Signal::NoteHoldStart];
            }
        }
        match self.hold_state {
            Hold::Armed(since) if now >= since + self.hold_delay => {
                self.hold_state = Hold::Active;
                // The system saw the modifier go down and its repeats keep it down, so text
                // typed meanwhile would arrive as Ctrl+text, and Qt windows could not take
                // messages. Let it go in the system once; swallow its repeats and key-up.
                if let Some(h) = self.hold.filter(|h| h.is_modifier_only() && self.pressed.contains(&h.key)) {
                    self.swallow_up.push(h.key);
                    self.released = Some(h.key);
                    self.release_due = Some(h.key);
                }
                vec![Signal::HoldStart]
            }
            _ => Vec::new(),
        }
    }

    /// Records a key state change without matching (used while input is buffered).
    pub fn track(&mut self, key: u16, down: bool) {
        if down {
            if !self.pressed.contains(&key) {
                self.pressed.push(key);
            }
        } else {
            self.pressed.retain(|&k| k != key);
        }
    }

    /// Frontend capture receives every physical event; only business matching is paused.
    /// Keep already-held keys so stopping capture cannot turn their releases into actions.
    pub fn begin_capture(&mut self) -> Vec<Signal> {
        self.capturing = true;
        self.capture_guard = None;
        self.pending_tap = None;
        self.disarm_note();
        match std::mem::replace(&mut self.hold_state, Hold::Idle) {
            Hold::Armed(_) => vec![Signal::HoldDisarmed],
            _ => Vec::new(),
        }
    }

    pub fn end_capture(&mut self, now: Instant) {
        self.capturing = false;
        self.capture_guard = Some(now + Duration::from_millis(300));
    }

    fn capture_suppressed(&mut self, now: Instant) -> bool {
        if self.capturing {
            return true;
        }
        if let Some(until) = self.capture_guard {
            if now < until || !self.pressed.is_empty() {
                return true;
            }
            self.capture_guard = None;
        }
        false
    }

    /// The hold modifier to let go of in the system, once.
    pub fn take_release(&mut self) -> Option<u16> {
        self.release_due.take()
    }

    /// Consumes a pending swallow for this key-up while input is buffered.
    pub fn take_up(&mut self, key: u16) -> bool {
        self.pressed.retain(|&k| k != key);
        if self.note_hold == NoteHold::Active(key) {
            self.note_hold = NoteHold::Idle;
        }
        if self.released == Some(key) {
            self.released = None;
        }
        match self.swallow_up.iter().position(|&k| k == key) {
            Some(i) => {
                self.swallow_up.remove(i);
                true
            }
            None => false,
        }
    }

    /// Consumes a pending swallow for the next mouse button release.
    pub fn take_mouse_up(&mut self) -> bool {
        std::mem::take(&mut self.swallow_mouse_up)
    }

    /// Forgets every key held, for when key-ups cannot have been seen: the secure desktop
    /// (Win+L, Ctrl+Alt+Del, UAC) took the keyboard, or the hook was removed. Without this a
    /// stale modifier spoils every shortcut until it is pressed again. A running hold ends as
    /// if its key were let go.
    pub fn reset_keys(&mut self) -> Vec<Signal> {
        self.pressed.clear();
        self.swallow_up.clear();
        self.swallow_mouse_up = false;
        self.pending_tap = None;
        let note = std::mem::replace(&mut self.note_hold, NoteHold::Idle);
        self.released = None;
        self.release_due = None;
        self.recording = Recording::default();
        let dictation = match std::mem::replace(&mut self.hold_state, Hold::Idle) {
            Hold::Armed(_) => Some(Signal::HoldDisarmed),
            Hold::Active => Some(Signal::HoldEnd),
            Hold::Idle | Hold::Suppressed => None,
        };
        dictation.into_iter().chain(matches!(note, NoteHold::Active(_)).then_some(Signal::NoteHoldEnd)).collect()
    }

    /// True while a hold action is recording.
    pub fn holding(&self) -> bool {
        self.hold_state == Hold::Active || matches!(self.note_hold, NoteHold::Active(_))
    }

    /// Resets per-session state when the engine changes mode.
    pub fn enter(&mut self, mode: Mode) {
        self.pending_tap = None;
        if mode != Mode::Normal {
            self.disarm_note();
        }
        if mode == Mode::Recording {
            self.recording = Recording::default();
            self.hold_state = Hold::Idle;
        }
        if matches!(mode, Mode::Continuous { .. }) {
            self.hold_state = Hold::Idle;
        }
    }

    pub fn key(&mut self, key: u16, down: bool, mode: Mode, now: Instant) -> Decision {
        let capture_suppressed = self.capture_suppressed(now);
        let repeat = down && self.pressed.contains(&key);
        if down {
            if !repeat {
                self.pressed.push(key);
            }
        } else {
            self.pressed.retain(|&k| k != key);
        }
        if capture_suppressed {
            if !down {
                // Matching is paused, but the key-up still settles the old swallow ledger.
                self.take_up(key);
                self.recording.swallowed.retain(|&k| k != key);
            }
            return Decision::pass();
        }
        if mode == Mode::Recording {
            return self.record(key, down, repeat);
        }
        // Key-ups of keys swallowed while recording stay swallowed after recording ends.
        if !down {
            if let Some(i) = self.recording.swallowed.iter().position(|&k| k == key) {
                self.recording.swallowed.remove(i);
                return Decision::swallow(false);
            }
        }
        if !down {
            return self.key_up(key, mode);
        }
        if repeat {
            return if self.swallow_up.contains(&key) { Decision::swallow(false) } else { Decision::pass() };
        }
        // A lone note modifier must stay alone until the timer fires. In particular,
        // AltGr's Ctrl+Alt pair and a quick Alt chord must not start recording.
        self.disarm_note();
        let mods = self.mods(key);
        let mask = mods.alt || mods.win;
        // Shortcuts first, so they keep working during continuous dictation.
        let shortcuts = [
            (self.toggle, Signal::Toggle),
            (self.captions, Signal::Captions),
            (self.note_toggle, Signal::NoteToggle),
            (self.note_blank, Signal::NoteBlank),
            (self.cards, Signal::Cards),
        ];
        for (binding, signal) in shortcuts {
            if let Some(b) = binding {
                if !b.is_modifier_only() && b.key == key && b.mods == mods {
                    self.swallow_up.push(key);
                    self.pending_tap = None;
                    return Decision::swallow(mask).with(signal);
                }
            }
        }
        if mode == Mode::Normal && self.hold_state == Hold::Idle && self.note_hold == NoteHold::Idle {
            if let Some(b) = self.note.filter(|b| b.key == key && b.mods == mods) {
                if b.is_modifier_only() {
                    if self.pressed.len() == 1 {
                        self.note_hold = NoteHold::Armed { key, since: now };
                    }
                } else {
                    self.note_hold = NoteHold::Active(key);
                    self.swallow_up.push(key);
                    return Decision::swallow(mask).with(Signal::NoteHoldStart);
                }
            }
        }
        let mut decision = Decision::pass();
        let in_session =
            matches!(mode, Mode::Continuous { .. } | Mode::Buffering) || matches!(self.note_hold, NoteHold::Active(_));
        if let (Some(h), false) = (self.hold, in_session) {
            if key == h.key && h.is_modifier_only() {
                if mods.is_empty() && self.hold_state == Hold::Idle {
                    self.hold_state = Hold::Armed(now);
                    return decision.with(Signal::HoldArmed);
                }
            } else if key == h.key && h.mods == mods && self.hold_state != Hold::Active {
                self.hold_state = Hold::Active;
                self.swallow_up.push(key);
                return Decision::swallow(mask).with(Signal::HoldArmed).with(Signal::HoldStart);
            }
        }
        if self.hold.is_some_and(|h| h.key != key) {
            match self.hold_state {
                Hold::Armed(_) => {
                    self.hold_state = Hold::Suppressed;
                    decision = decision.with(Signal::HoldDisarmed);
                }
                Hold::Active => {
                    self.hold_state = Hold::Suppressed;
                    decision = decision.with(Signal::HoldInterrupted);
                    if let Some(m) = self.released.take() {
                        // A slow shortcut such as Ctrl+C: the key needs its modifier back.
                        self.swallow_up.retain(|&k| k != m);
                        self.release_due = None;
                        decision.swallow = true;
                        decision.repress = Some(m);
                    }
                }
                _ => {}
            }
        }
        if let Mode::Continuous { replay } = mode {
            if is_modifier(key) {
                self.pending_tap = Some(key);
                return decision;
            }
            self.pending_tap = None;
            let chord = Chord { mods: self.held_modifiers(key), key };
            self.swallow_up.push(key);
            decision.swallow = true;
            decision.mask = mask;
            return decision.with(Signal::EndedByKey { replay, chord });
        }
        decision
    }

    fn key_up(&mut self, key: u16, mode: Mode) -> Decision {
        let mut decision = Decision::pass();
        match self.note_hold {
            NoteHold::Armed { key: held, .. } if held == key => self.disarm_note(),
            NoteHold::Active(held) if held == key => {
                self.note_hold = NoteHold::Idle;
                // Notes do not type into the foreground app, so the modifier can stay
                // down during recording. Mask its release to keep Start and menus closed.
                decision.mask = matches!(key, vk::LWIN | vk::RWIN | vk::LMENU | vk::RMENU);
                decision = decision.with(Signal::NoteHoldEnd);
            }
            _ => {}
        }
        if let Some(i) = self.swallow_up.iter().position(|&k| k == key) {
            self.swallow_up.remove(i);
            decision.swallow = true;
        }
        if self.hold.is_some_and(|h| h.key == key) {
            if self.released == Some(key) {
                self.released = None;
                self.release_due = None;
            }
            match std::mem::replace(&mut self.hold_state, Hold::Idle) {
                Hold::Armed(_) => decision = decision.with(Signal::HoldDisarmed),
                Hold::Active => decision = decision.with(Signal::HoldEnd),
                _ => {}
            }
        }
        if matches!(mode, Mode::Continuous { .. }) && self.pending_tap == Some(key) {
            self.pending_tap = None;
            decision.mask = matches!(key, vk::LWIN | vk::RWIN | vk::LMENU | vk::RMENU);
            decision = decision.with(Signal::EndedByTap);
        }
        decision
    }

    fn record(&mut self, key: u16, down: bool, repeat: bool) -> Decision {
        let rec = &mut self.recording;
        if !down {
            let was_swallowed = rec.swallowed.contains(&key);
            rec.swallowed.retain(|&k| k != key);
            // A lone modifier pressed and released records a side-specific binding.
            if is_modifier(key) && rec.mods_seen == [key] {
                rec.mods_seen.clear();
                let binding = Binding { mods: Mods::default(), key };
                return Decision::swallow(false).with(Signal::Recorded(binding.to_string()));
            }
            if rec.swallowed.iter().all(|&k| !is_modifier(k)) {
                rec.mods_seen.clear();
            }
            return Decision { swallow: was_swallowed, ..Decision::default() };
        }
        if !repeat {
            rec.swallowed.push(key);
        }
        if is_modifier(key) {
            if !repeat && !rec.mods_seen.contains(&key) {
                rec.mods_seen.push(key);
            }
            return Decision::swallow(false);
        }
        let mut mods = Mods::default();
        for &k in &rec.mods_seen {
            mod_of(k, &mut mods);
        }
        rec.mods_seen.clear();
        let text = if key == vk::ESCAPE && mods.is_empty() { String::new() } else { Binding { mods, key }.to_string() };
        Decision::swallow(mods.alt || mods.win).with(Signal::Recorded(text))
    }

    /// Mouse button event. `own_window` is true when the click lands on this app.
    pub fn mouse(&mut self, button: u8, down: bool, x: i32, y: i32, own_window: bool, mode: Mode) -> Decision {
        if !down {
            if std::mem::take(&mut self.swallow_mouse_up) {
                return Decision::swallow(false);
            }
            return Decision::pass();
        }
        self.disarm_note();
        if let Hold::Armed(_) = self.hold_state {
            self.hold_state = Hold::Suppressed;
            return Decision::pass().with(Signal::HoldDisarmed);
        }
        match mode {
            Mode::Continuous { replay } if !own_window => {
                self.pending_tap = None;
                self.swallow_mouse_up = !replay;
                Decision::swallow(false).with(Signal::EndedByClick { replay, button, x, y })
            }
            _ => Decision::pass(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(text: &str) -> Binding {
        Binding::parse(text).unwrap()
    }

    fn matcher() -> Matcher {
        let mut m = Matcher::default();
        m.set_bindings(Some(b("RCtrl")), Some(b("Ctrl+Alt+Space")), Some(b("Ctrl+Alt+C")));
        m
    }

    #[test]
    fn note_modifier_starts_once_after_hold_and_ends_on_release() {
        let mut m = matcher();
        m.set_notes(Some(b("RAlt")), None, None);
        let now = Instant::now();
        let deadline = now + m.hold_delay;
        assert_eq!(m.key(vk::RMENU, true, Mode::Normal, now), Decision::pass());
        assert_eq!(m.key(vk::RMENU, false, Mode::Normal, now), Decision::pass(), "a tap does not record");
        assert!(m.tick(deadline).is_empty());

        assert_eq!(m.key(vk::RMENU, true, Mode::Normal, now), Decision::pass());
        assert_eq!(m.next_deadline(), Some(deadline));
        assert!(m.tick(deadline - Duration::from_millis(1)).is_empty());
        assert_eq!(m.tick(deadline), [Signal::NoteHoldStart]);
        assert!(m.tick(deadline).is_empty());
        m.enter(Mode::Normal);
        assert_eq!(m.key(vk::RMENU, true, Mode::Normal, deadline), Decision::pass(), "no restart on repeat");
        assert_eq!(m.key(u16::from(b'A'), true, Mode::Normal, deadline), Decision::pass(), "other keys do not end it");
        assert_eq!(m.mouse(1, true, 0, 0, false, Mode::Normal), Decision::pass());
        let d = m.key(vk::RMENU, false, Mode::Normal, deadline);
        assert_eq!(d.signals, [Signal::NoteHoldEnd]);
        assert!(d.mask && !d.swallow, "the hook masks and replays the Alt release");
        assert!(m.key(vk::RMENU, false, Mode::Normal, deadline).signals.is_empty());
    }

    #[test]
    fn quick_modifier_chords_altgr_and_mouse_cancel_pending_note_hold() {
        let now = Instant::now();
        for chord in [
            vec![vk::RMENU, u16::from(b'A')],
            vec![u16::from(b'A'), vk::RMENU],
            vec![vk::LCONTROL, vk::RMENU], // Windows AltGr synthetic Ctrl first.
            vec![vk::RMENU, vk::LCONTROL],
            vec![vk::LSHIFT, vk::RMENU],
            vec![vk::RMENU, vk::LWIN],
        ] {
            let mut m = matcher();
            m.set_notes(Some(b("RAlt")), None, None);
            for &key in &chord {
                assert_eq!(m.key(key, true, Mode::Normal, now), Decision::pass());
            }
            assert!(m.tick(now + m.hold_delay).is_empty(), "{chord:?}");
            for &key in chord.iter().rev() {
                assert_eq!(m.key(key, false, Mode::Normal, now), Decision::pass());
            }
        }
        let mut m = matcher();
        m.set_notes(Some(b("RAlt")), None, None);
        m.key(vk::RMENU, true, Mode::Normal, now);
        assert_eq!(m.mouse(1, true, 0, 0, true, Mode::Normal), Decision::pass());
        assert!(m.next_deadline().is_none());
        assert!(m.tick(now + m.hold_delay).is_empty());
        assert!(m.key(vk::RMENU, false, Mode::Normal, now).signals.is_empty());
    }

    #[test]
    fn capture_passes_keys_cancels_armed_hold_and_waits_for_release() {
        let mut m = matcher();
        m.set_notes(Some(b("RAlt")), None, None);
        let now = Instant::now();
        m.key(vk::RCONTROL, true, Mode::Normal, now);
        assert_eq!(m.begin_capture(), [Signal::HoldDisarmed]);
        assert!(m.tick(now + Duration::from_secs(1)).is_empty());
        assert_eq!(m.key(vk::RCONTROL, false, Mode::Normal, now), Decision::pass());
        for key in [vk::LCONTROL, vk::LMENU, vk::SPACE] {
            assert_eq!(m.key(key, true, Mode::Normal, now), Decision::pass());
        }
        m.end_capture(now);
        let later = now + Duration::from_secs(1);
        for key in [vk::SPACE, vk::LMENU, vk::LCONTROL] {
            assert_eq!(m.key(key, false, Mode::Normal, later), Decision::pass());
        }
        assert_eq!(m.key(vk::RMENU, true, Mode::Normal, later), Decision::pass());
        assert_eq!(m.tick(later + m.hold_delay), [Signal::NoteHoldStart]);
        assert_eq!(m.key(vk::RMENU, false, Mode::Normal, later + m.hold_delay).signals, [Signal::NoteHoldEnd]);
    }

    #[test]
    fn capture_release_guard_covers_immediate_repeats_and_restarts() {
        let mut m = matcher();
        m.set_notes(Some(b("RAlt")), None, None);
        let now = Instant::now();
        m.begin_capture();
        m.end_capture(now);
        for down in [true, false] {
            assert_eq!(m.key(vk::RMENU, down, Mode::Normal, now + Duration::from_millis(100)), Decision::pass());
        }
        m.begin_capture();
        assert_eq!(m.key(vk::RMENU, true, Mode::Normal, now + Duration::from_secs(1)), Decision::pass());
        m.end_capture(now + Duration::from_secs(1));
        assert!(m.tick(now + Duration::from_secs(3)).is_empty());
        assert_eq!(m.key(vk::RMENU, true, Mode::Normal, now + Duration::from_secs(3)), Decision::pass());
        assert_eq!(m.key(vk::RMENU, false, Mode::Normal, now + Duration::from_secs(3)), Decision::pass());
        assert_eq!(m.key(vk::RCONTROL, true, Mode::Normal, now + Duration::from_secs(3)).signals, [Signal::HoldArmed]);
    }

    #[test]
    fn capture_of_preheld_note_and_secure_desktop_reset_cannot_trigger() {
        let mut m = matcher();
        m.set_notes(Some(b("RAlt")), None, None);
        let now = Instant::now();
        m.key(vk::RMENU, true, Mode::Normal, now);
        m.begin_capture();
        m.end_capture(now);
        assert_eq!(m.key(vk::RMENU, false, Mode::Normal, now + Duration::from_secs(1)), Decision::pass());
        m.begin_capture();
        m.key(vk::RCONTROL, true, Mode::Normal, now);
        m.end_capture(now);
        assert!(m.reset_keys().is_empty());
        assert_eq!(m.key(vk::RMENU, true, Mode::Normal, now + Duration::from_secs(1)), Decision::pass());
        assert_eq!(m.tick(now + Duration::from_secs(2)), [Signal::NoteHoldStart]);
        assert_eq!(m.reset_keys(), [Signal::NoteHoldEnd]);
        assert!(m.key(vk::RMENU, false, Mode::Normal, now + Duration::from_secs(2)).signals.is_empty());
    }

    #[test]
    fn capture_releases_old_swallowed_keys_without_poisoning_later_input() {
        let mut m = matcher();
        let now = Instant::now();
        for key in [vk::LCONTROL, vk::LMENU, vk::SPACE] {
            m.key(key, true, Mode::Normal, now);
        }
        assert!(m.swallow_up.contains(&vk::SPACE));
        m.begin_capture();
        for key in [vk::SPACE, vk::LCONTROL, vk::LMENU] {
            assert_eq!(m.key(key, false, Mode::Normal, now), Decision::pass());
        }
        m.end_capture(now);
        for down in [true, false] {
            assert_eq!(m.key(vk::SPACE, down, Mode::Normal, now + Duration::from_secs(1)), Decision::pass());
        }
    }

    #[test]
    fn parse_and_display_round_trip() {
        for text in ["RCtrl", "Ctrl+Alt+Space", "Ctrl+Shift+F9", "Alt+Win+K", "F8", "Alt+`", "Ctrl+\\", "Num5"] {
            assert_eq!(b(text).to_string(), text);
        }
        assert_eq!(b("ctrl + alt + space").to_string(), "Ctrl+Alt+Space");
        assert_eq!(b("rightctrl").to_string(), "RCtrl");
        for bad in ["", "Ctrl", "Ctrl+", "Ctrl+RCtrl", "Hyper+A", "F25", "Ctrl+Nope"] {
            assert!(Binding::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn every_named_special_key_round_trips_with_modifiers() {
        for &(name, key) in KEY_NAMES {
            assert_eq!(Binding::parse(name).unwrap().key, key, "{name}");
            for mask in 0..16 {
                if is_modifier(key) && mask != 0 {
                    continue;
                }
                let binding = Binding {
                    mods: Mods { ctrl: mask & 1 != 0, alt: mask & 2 != 0, shift: mask & 4 != 0, win: mask & 8 != 0 },
                    key,
                };
                let displayed = binding.to_string();
                assert_eq!(Binding::parse(&displayed).unwrap(), binding, "{displayed}");
                assert_eq!(Binding::parse(&displayed.to_ascii_lowercase()).unwrap(), binding, "{displayed}");
            }
        }
        assert_eq!(b("Backspace").to_string(), "Backspace");
    }

    #[test]
    fn hold_modifier_starts_after_delay_and_ends_on_release() {
        let mut m = matcher();
        let t0 = Instant::now();
        let d = m.key(vk::RCONTROL, true, Mode::Normal, t0);
        assert!(!d.swallow);
        assert_eq!(d.signals, vec![Signal::HoldArmed]);
        assert!(m.tick(t0 + Duration::from_millis(100)).is_empty());
        assert!(m.key(vk::RCONTROL, true, Mode::Normal, t0 + Duration::from_millis(200)).signals.is_empty());
        assert_eq!(m.take_release(), None, "not before dictation starts");
        assert_eq!(m.tick(t0 + Duration::from_millis(260)), vec![Signal::HoldStart]);
        assert_eq!(m.take_release(), Some(vk::RCONTROL));
        assert_eq!(m.take_release(), None, "once");
        assert!(m.key(vk::RCONTROL, true, Mode::Normal, t0 + Duration::from_millis(500)).swallow, "repeat");
        let d = m.key(vk::RCONTROL, false, Mode::Normal, t0 + Duration::from_millis(900));
        assert!(d.swallow, "the system already saw it go up");
        assert_eq!(d.signals, vec![Signal::HoldEnd]);
        assert!(!m.key(vk::RCONTROL, true, Mode::Normal, t0 + Duration::from_secs(2)).swallow, "next press");
    }

    #[test]
    fn keys_released_on_the_secure_desktop_do_not_spoil_shortcuts() {
        let mut m = matcher();
        let t0 = Instant::now();
        // Win+L: both key-ups happen on the lock screen, out of the hook's sight.
        m.key(vk::LWIN, true, Mode::Normal, t0);
        m.key(u16::from(b'L'), true, Mode::Normal, t0);
        assert!(m.key(vk::RCONTROL, true, Mode::Normal, t0).signals.is_empty(), "stale Win blocks the hold key");
        m.key(vk::RCONTROL, false, Mode::Normal, t0);
        assert!(m.reset_keys().is_empty());
        let later = t0 + Duration::from_secs(5);
        assert_eq!(m.key(vk::RCONTROL, true, Mode::Normal, later).signals, vec![Signal::HoldArmed]);
        m.key(vk::RCONTROL, false, Mode::Normal, later);
        for k in [vk::LCONTROL, vk::LMENU] {
            m.key(k, true, Mode::Normal, later);
        }
        let d = m.key(vk::SPACE, true, Mode::Normal, later);
        assert!(d.swallow);
        assert_eq!(d.signals, vec![Signal::Toggle]);
    }

    #[test]
    fn reset_during_a_hold_ends_it_like_a_release() {
        let mut m = matcher();
        let t0 = Instant::now();
        m.key(vk::RCONTROL, true, Mode::Normal, t0);
        assert_eq!(m.tick(t0 + Duration::from_millis(260)), vec![Signal::HoldStart]);
        assert_eq!(m.take_release(), Some(vk::RCONTROL));
        assert_eq!(m.reset_keys(), vec![Signal::HoldEnd], "the mic must not stay on behind the lock screen");
        assert!(!m.holding());
        let d = m.key(vk::RCONTROL, false, Mode::Normal, t0 + Duration::from_secs(1));
        assert!(!d.swallow && d.signals.is_empty(), "a late key-up is passed on and ends nothing");
        m.key(vk::RCONTROL, true, Mode::Normal, t0);
        assert_eq!(m.reset_keys(), vec![Signal::HoldDisarmed]);
    }

    #[test]
    fn hold_modifier_used_in_a_shortcut_does_not_dictate() {
        let mut m = matcher();
        let t0 = Instant::now();
        m.key(vk::RCONTROL, true, Mode::Normal, t0);
        let d = m.key(u16::from(b'C'), true, Mode::Normal, t0 + Duration::from_millis(50));
        assert!(!d.swallow);
        assert_eq!(d.signals, vec![Signal::HoldDisarmed]);
        assert!(m.tick(t0 + Duration::from_secs(1)).is_empty());
        assert!(m.key(vk::RCONTROL, false, Mode::Normal, t0 + Duration::from_secs(1)).signals.is_empty());
    }

    #[test]
    fn key_during_hold_interrupts() {
        let mut m = matcher();
        let t0 = Instant::now();
        m.key(vk::RCONTROL, true, Mode::Normal, t0);
        m.tick(t0 + Duration::from_millis(300));
        let d = m.key(u16::from(b'V'), true, Mode::Normal, t0 + Duration::from_millis(400));
        assert_eq!(d.signals, vec![Signal::HoldInterrupted]);
        // Ctrl was let go of in the system: press it again before the V.
        assert!(d.swallow);
        assert_eq!(d.repress, Some(vk::RCONTROL));
        assert!(!m.key(u16::from(b'V'), false, Mode::Normal, t0 + Duration::from_millis(450)).swallow);
        assert!(!m.key(vk::RCONTROL, true, Mode::Normal, t0 + Duration::from_millis(500)).swallow, "repeat");
        assert!(!m.key(vk::RCONTROL, false, Mode::Normal, t0 + Duration::from_millis(600)).swallow);
    }

    #[test]
    fn toggle_shortcut_is_swallowed_with_its_key_up() {
        let mut m = matcher();
        let t = Instant::now();
        assert!(!m.key(vk::LCONTROL, true, Mode::Normal, t).swallow);
        assert!(!m.key(vk::LMENU, true, Mode::Normal, t).swallow);
        let d = m.key(vk::SPACE, true, Mode::Normal, t);
        assert!(d.swallow && d.mask);
        assert_eq!(d.signals, vec![Signal::Toggle]);
        assert!(m.key(vk::SPACE, true, Mode::Normal, t).swallow, "repeat");
        assert!(m.key(vk::SPACE, false, Mode::Normal, t).swallow);
        let mode = Mode::Continuous { replay: false };
        m.enter(mode);
        assert!(m.key(vk::LMENU, false, mode, t).signals.is_empty());
        assert!(m.key(vk::LCONTROL, false, mode, t).signals.is_empty());
    }

    #[test]
    fn any_key_ends_continuous_dictation() {
        let mut m = matcher();
        let t = Instant::now();
        let mode = Mode::Continuous { replay: true };
        m.enter(mode);
        let d = m.key(vk::RETURN, true, mode, t);
        assert!(d.swallow);
        assert_eq!(
            d.signals,
            vec![Signal::EndedByKey { replay: true, chord: Chord { mods: vec![], key: vk::RETURN } }]
        );
        let mut m = matcher();
        m.enter(mode);
        assert!(!m.key(vk::LCONTROL, true, mode, t).swallow);
        let d = m.key(vk::RETURN, true, mode, t);
        assert_eq!(
            d.signals,
            vec![Signal::EndedByKey { replay: true, chord: Chord { mods: vec![vk::LCONTROL], key: vk::RETURN } }]
        );
    }

    #[test]
    fn modifier_tap_ends_and_masks_win() {
        let mut m = matcher();
        let t = Instant::now();
        let mode = Mode::Continuous { replay: false };
        m.enter(mode);
        assert!(m.key(vk::LWIN, true, mode, t).signals.is_empty());
        let d = m.key(vk::LWIN, false, mode, t);
        assert!(d.mask && !d.swallow);
        assert_eq!(d.signals, vec![Signal::EndedByTap]);
    }

    #[test]
    fn tray_note_and_card_shortcuts_are_independent_of_hold_and_work_in_continuous_mode() {
        let mut m = matcher();
        m.set_notes(Some(b("RAlt")), Some(b("Ctrl+Alt+N")), Some(b("Ctrl+Alt+L")));
        m.cards = Some(b("Ctrl+Alt+K"));
        let t = Instant::now();
        let mode = Mode::Continuous { replay: false };
        m.enter(mode);
        m.key(vk::LCONTROL, true, mode, t);
        m.key(vk::LMENU, true, mode, t);
        let d = m.key(u16::from(b'N'), true, mode, t);
        assert!(d.swallow);
        assert_eq!(d.signals, vec![Signal::NoteToggle]);
        m.key(u16::from(b'N'), false, mode, t);
        assert_eq!(m.key(u16::from(b'K'), true, mode, t).signals, vec![Signal::Cards]);
        m.key(u16::from(b'K'), false, mode, t);
        assert_eq!(m.key(u16::from(b'L'), true, mode, t).signals, [Signal::NoteBlank]);
        assert!(m.key(u16::from(b'L'), true, mode, t).signals.is_empty(), "repeat does not add more cards");
        assert!(m.key(u16::from(b'L'), false, mode, t).swallow);
    }

    #[test]
    fn note_chord_ends_on_release_even_after_rebinding() {
        let mut m = matcher();
        m.set_notes(Some(b("Ctrl+Right")), None, None);
        let now = Instant::now();
        m.key(vk::LCONTROL, true, Mode::Normal, now);
        let started = m.key(vk::RIGHT, true, Mode::Normal, now);
        assert!(started.swallow);
        assert_eq!(started.signals, [Signal::NoteHoldStart]);
        m.set_notes(Some(b("RAlt")), None, None);
        assert!(m.key(vk::LCONTROL, false, Mode::Normal, now).signals.is_empty());
        let stopped = m.key(vk::RIGHT, false, Mode::Normal, now);
        assert!(stopped.swallow);
        assert_eq!(stopped.signals, [Signal::NoteHoldEnd]);
    }

    #[test]
    fn captions_shortcut_does_not_end_continuous_dictation() {
        let mut m = matcher();
        let t = Instant::now();
        let mode = Mode::Continuous { replay: false };
        m.enter(mode);
        m.key(vk::LCONTROL, true, mode, t);
        m.key(vk::LMENU, true, mode, t);
        let d = m.key(u16::from(b'C'), true, mode, t);
        assert_eq!(d.signals, vec![Signal::Captions]);
    }

    #[test]
    fn clicks_end_continuous_dictation_except_on_own_windows() {
        let mut m = matcher();
        let mode = Mode::Continuous { replay: false };
        assert!(m.mouse(1, true, 5, 5, true, mode).signals.is_empty());
        let d = m.mouse(1, true, 10, 20, false, mode);
        assert!(d.swallow);
        assert_eq!(d.signals, vec![Signal::EndedByClick { replay: false, button: 1, x: 10, y: 20 }]);
        assert!(m.mouse(1, false, 10, 20, false, Mode::Normal).swallow);
        assert!(!m.mouse(1, false, 10, 20, false, Mode::Normal).swallow);
    }

    #[test]
    fn recording_captures_combos_and_lone_modifiers() {
        let mut m = matcher();
        let t = Instant::now();
        let r = Mode::Recording;
        m.enter(r);
        assert!(m.key(vk::LCONTROL, true, r, t).swallow);
        assert!(m.key(vk::LSHIFT, true, r, t).swallow);
        let d = m.key(u16::from(b'K'), true, r, t);
        assert_eq!(d.signals, vec![Signal::Recorded("Ctrl+Shift+K".into())]);
        m.key(u16::from(b'K'), false, r, t);
        m.key(vk::LSHIFT, false, r, t);
        m.key(vk::LCONTROL, false, r, t);
        m.enter(r);
        m.key(vk::RMENU, true, r, t);
        let d = m.key(vk::RMENU, false, r, t);
        assert_eq!(d.signals, vec![Signal::Recorded("RAlt".into())]);
        m.enter(r);
        let d = m.key(vk::ESCAPE, true, r, t);
        assert_eq!(d.signals, vec![Signal::Recorded(String::new())]);
    }
}
