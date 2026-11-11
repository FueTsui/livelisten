//! The engine thread: owns settings, audio, speech providers, dictation, captions and note
//! cards, and talks to the UI through JSON commands ([`Engine::call`]) and JSON events (the sink).
//!
//! Recognized text never goes to the log; logs keep events, codes, lengths and timings.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::audio::{self, AudioError, AudioFrame, Capture, Source};
use crate::captions::{Captions, Line, LineSource};
use crate::cards::CardKind;
use crate::config::{self, EndAction, Preconnect, Settings, SourceKind};
use crate::dictation::{self, WriteOp, Writer};
use crate::hotkey::{Binding, Mode, Signal};
use crate::paths::{self, AppDirs};
use crate::protocol::{Capabilities, FromProvider, PROTOCOL_VERSION, ToProvider};
use crate::provider::{self, ProviderOutput, ProviderProcess, ProviderSpec};
use crate::secrets;
use crate::win::focus::{self, Element, Target, Uia};

mod notes;

use crate::win::hook::{Bindings, Hook, HookEvent, input_revision, user_input_count};
use crate::win::input;
use notes::Notes;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const LANGUAGE: &str = "zh-CN";
const SAMPLE_RATE: u32 = 16_000;
/// How long to wait for the last text after dictation stops, counted from the latest text.
const FINISH_WAIT: Duration = Duration::from_secs(3);
/// Longest wait for the last text after dictation stops, while text keeps arriving.
const FINISH_MAX: Duration = Duration::from_secs(12);
/// Doubao sends dictation text without punctuation and corrects it only when a session ends,
/// so one long session ends in one big rewrite. After this long, a dictation splits its
/// recognition at the next pause (see [`is_pause`]).
const DICTATION_SPLIT_AFTER: Duration = Duration::from_secs(4);
/// Text unchanged this long, with a quiet microphone, counts as a pause in dictation.
const DICTATION_PAUSE: Duration = Duration::from_millis(900);
const DICTATION_QUIET: Duration = Duration::from_millis(300);
/// How long the last text waits for the target window to come back after a short switch.
const RESUME_WAIT: Duration = Duration::from_millis(1500);
/// A dictation this long that produced no text tells the user why.
const NOTHING_HEARD_AFTER_MS: u64 = 1500;
/// Audio kept for one reconnect attempt (90 s of 40 ms frames).
const RETRY_FRAMES: usize = 90 * 25;
const _: () = assert!(RETRY_FRAMES < provider::AUDIO_BACKLOG, "a replay must fit in the provider queue");
/// One paced Backspace per step keeps control handling responsive during long corrections.
const BACKSPACE_STEP: usize = 1;
const TYPE_STEP: usize = 16;
/// Audio counts as voice above this level (-50 dBFS), or above the noise floor (see
/// [`NoiseFloor`]) where the room is louder.
const VOICE_LEVEL: f32 = 0.2;
/// Voice must be this far above the quietest level of the last seconds (6 dB)...
const FLOOR_MARGIN: f32 = 0.12;
/// ...but never louder than -40 dBFS, so normal speech and dialog always count.
const VOICE_LEVEL_MAX: f32 = 0.4;
/// The noise floor is the quietest frame of the last five one-second buckets.
const FLOOR_BUCKET_FRAMES: u32 = 25;
const FLOOR_BUCKETS: usize = 5;
/// Audio sent ahead of the first voiced frame (8 frames = 320 ms).
const PREROLL_FRAMES: usize = 8;
const CAPTION_SILENCE: Duration = Duration::from_secs(6);
/// Longest caption session. The speech service confirms most text only when a session ends,
/// so a long session keeps sentences open on screen.
const CAPTION_MAX: Duration = Duration::from_secs(60);
/// After this long, a session also ends at the next pause in the recognized text, so
/// continuous sound (a video, a noisy room) still yields confirmed sentences.
const CAPTION_CYCLE_AFTER: Duration = Duration::from_secs(8);
/// Recognized text unchanged this long counts as a pause.
const CAPTION_PAUSE: Duration = Duration::from_millis(900);
const CAPTION_LINES: usize = 8;
const IDLE_CHECK: Duration = Duration::from_secs(30);
/// The Doubao standby connection expires after about 18 seconds, so it is renewed this often.
const WARM_REFRESH: Duration = Duration::from_secs(5);

/// Standby connection timing of a preconnect level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WarmPolicy {
    /// Keeps a connection ready this long after each dictation, hold-key press, change of
    /// service or credential, and level change.
    window: Duration,
    /// Renewal pauses while nobody has touched the keyboard or mouse this long (the user is
    /// away or the session is locked). The next input resumes it; the hold key itself also
    /// prepares a connection.
    idle: Duration,
    /// Also keeps a connection ready for `window` after start-up.
    at_start: bool,
}

impl WarmPolicy {
    /// `None` when preconnect is off. Balanced skips start-up and keeps a fifth of the
    /// aggressive window, so it opens about a fifth of the connections after each dictation.
    fn of(level: Preconnect) -> Option<WarmPolicy> {
        match level {
            Preconnect::Off => None,
            Preconnect::Balanced => {
                Some(WarmPolicy { window: Duration::from_secs(2 * 60), idle: Duration::from_secs(60), at_start: false })
            }
            Preconnect::Aggressive => Some(WarmPolicy {
                window: Duration::from_secs(10 * 60),
                idle: Duration::from_secs(3 * 60),
                at_start: true,
            }),
        }
    }
}
/// The UI Automation focus check before input runs at most this often during one dictation.
/// Window focus is still checked before every step, and user input stops writing at once.
const UIA_RECHECK: Duration = Duration::from_millis(250);

/// Receives one JSON event per call.
pub type EventSink = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Settings folder; `None` uses `%APPDATA%\LiveListen`.
    pub config_dir: Option<PathBuf>,
    /// Install the keyboard hook (off in tests).
    pub hooks: bool,
    /// Folders searched, with their parents, for `doubao_asr` and the bridge script.
    pub search: Vec<PathBuf>,
    /// Test input: play this WAV instead of the microphone.
    pub dictation_audio: Option<PathBuf>,
    /// Test input: play this WAV instead of the captions source.
    pub captions_audio: Option<PathBuf>,
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

impl Options {
    /// App defaults: hooks on; search next to the executable and in the working folder;
    /// test audio from `LIVE_LISTEN_TEST_AUDIO` and `LIVE_LISTEN_TEST_CAPTIONS_AUDIO`.
    pub fn for_app(config_dir: Option<PathBuf>) -> Options {
        let mut search = Vec::new();
        if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
            search.push(dir);
        }
        if let Ok(dir) = std::env::current_dir() {
            search.push(dir);
        }
        Options {
            config_dir,
            hooks: true,
            search,
            dictation_audio: env_path("LIVE_LISTEN_TEST_AUDIO"),
            captions_audio: env_path("LIVE_LISTEN_TEST_CAPTIONS_AUDIO"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Channel {
    Dictation = 0,
    Captions = 1,
}

impl Channel {
    fn scope(self) -> &'static str {
        match self {
            Channel::Dictation => "dictation",
            Channel::Captions => "captions",
        }
    }
}

enum Msg {
    Call(Value, mpsc::Sender<Value>),
    Hook(HookEvent),
    Frame {
        capture: u64,
        frame: AudioFrame,
    },
    AudioFailed {
        capture: u64,
    },
    /// The device of a capture started, or could not start.
    CaptureReady {
        capture: u64,
        result: Result<(), AudioError>,
    },
    Provider {
        channel: Channel,
        generation: u64,
        output: ProviderOutput,
    },
    Probed {
        id: String,
        result: Result<(String, Capabilities), String>,
    },
    Quit,
}

/// The UI-facing handle. Dropping it stops the engine thread.
pub struct Engine {
    tx: mpsc::Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

fn error_reply(code: &str) -> Value {
    json!({"ok": false, "error": code})
}

impl Engine {
    pub fn start(options: Options, events: EventSink) -> Result<Engine, String> {
        Self::start_initializing(options, events, Duration::from_secs(15), Core::new)
    }

    fn start_initializing(
        options: Options,
        events: EventSink,
        timeout: Duration,
        initialize: impl FnOnce(Options, EventSink, mpsc::Sender<Msg>) -> Core + Send + 'static,
    ) -> Result<Engine, String> {
        let (tx, rx) = mpsc::channel();
        let core_tx = tx.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (accept_tx, accept_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("engine".into())
            .spawn(move || {
                let mut core = initialize(options, events, core_tx);
                // A timed-out ll_start caller may already have released its callback
                // context. Never enter the event loop until it accepts this engine.
                if ready_tx.send(()).is_err() || accept_rx.recv().is_err() {
                    core.shutdown();
                    return;
                }
                run(core, rx);
            })
            .map_err(|e| e.to_string())?;
        ready_rx.recv_timeout(timeout).map_err(|_| "engine did not start".to_string())?;
        accept_tx.send(()).map_err(|_| "engine did not start".to_string())?;
        Ok(Engine { tx, thread: Some(thread) })
    }

    /// Runs one JSON command (`{"cmd": "...", ...}`) and returns the JSON reply.
    pub fn call(&self, request: &str) -> String {
        let Ok(value) = serde_json::from_str::<Value>(request) else {
            return error_reply("bad_request").to_string();
        };
        let shutdown = value["cmd"] == "shutdown";
        let (reply_tx, reply_rx) = mpsc::channel();
        if self.tx.send(Msg::Call(value, reply_tx)).is_err() {
            return error_reply("stopped").to_string();
        }
        // The shutdown caller waits off the UI thread for a definite result. The
        // writer barrier is bounded, but OS recovery-file I/O has no hard deadline;
        // returning a timeout could let the caller resume a core that then exits.
        let response = if shutdown {
            reply_rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        } else {
            reply_rx.recv_timeout(Duration::from_secs(20))
        };
        match response {
            Ok(v) => v.to_string(),
            Err(mpsc::RecvTimeoutError::Timeout) => error_reply("timeout").to_string(),
            Err(mpsc::RecvTimeoutError::Disconnected) => error_reply("stopped").to_string(),
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Slot {
    channel: Channel,
    process: Option<ProviderProcess>,
    key: String,
    provider_id: String,
    generation: u64,
    idle_since: Instant,
    busy: bool,
    /// Audio frames the current process did not take because it stopped reading.
    dropped: u64,
}

impl Slot {
    fn new(channel: Channel) -> Slot {
        Slot {
            channel,
            process: None,
            key: String::new(),
            provider_id: String::new(),
            generation: 0,
            idle_since: Instant::now(),
            busy: false,
            dropped: 0,
        }
    }

    fn release(&mut self) {
        if let Some(process) = self.process.take() {
            log::info!("stopping {} speech service", self.channel.scope());
            std::thread::spawn(move || process.shutdown());
        }
    }
}

/// Credentials are excluded from cache keys. Invalidate only the matching service so
/// its next session reloads the credential without disrupting an active recording.
fn invalidate_speech_credential(slots: &mut [Slot], name: &str) {
    let Some(id) = name.strip_prefix("speech.") else { return };
    for slot in slots.iter_mut().filter(|slot| slot.provider_id == id) {
        slot.key.clear();
        if !slot.busy {
            slot.release();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trigger {
    Hold,
    Toggle,
}

/// What dictated text is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Purpose {
    /// Typed into the focused text box.
    Type,
    /// Kept as a note card.
    Note,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Connecting,
    Listening,
    Finishing,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Connecting => "connecting",
            Phase::Listening => "listening",
            Phase::Finishing => "finishing",
        }
    }
}

struct Dictation {
    /// Current provider session (changes after a reconnect or a provider-side rollover).
    session: u64,
    /// First session id; names this dictation in the caption model.
    mirror_id: u64,
    trigger: Trigger,
    purpose: Purpose,
    phase: Phase,
    target: Option<Target>,
    /// The focused element of the target when dictation started, if UI Automation knows it.
    element: Option<Element>,
    blocked: Option<&'static str>,
    paused: bool,
    writer: Writer,
    /// The last write stopped after one step of a long correction; the rest follows.
    behind: bool,
    /// Text of earlier provider sessions of this dictation.
    base: String,
    text: String,
    stable: usize,
    capture: Option<Capture>,
    capture_id: u64,
    /// Milliseconds from the start until the microphone ran; `None` while it is opening.
    mic_ms: Option<u64>,
    /// Voice detection for pauses, relative to the room's noise.
    floor: NoiseFloor,
    /// When UI Automation last confirmed the focused element (see [`UIA_RECHECK`]).
    element_checked: Option<Instant>,
    started: Instant,
    first_text_ms: Option<u64>,
    deadline: Option<Instant>,
    replay: bool,
    timed_out: bool,
    mirrored: bool,
    audio: Vec<Vec<i16>>,
    retried: bool,
    /// Session started at a pause; it takes the audio while `session` delivers its last text.
    next: Option<u64>,
    /// When the session taking the audio started, the last voiced frame, the last text change.
    part_started: Instant,
    last_voice: Instant,
    changed: Instant,
    /// When the user stopped dictating.
    stopped_at: Option<Instant>,
    /// User input count when another window came to the front. Writing resumes if the target
    /// returns before the user types or clicks, because the caret has not moved.
    resume_input: Option<u64>,
    /// Hook-side invalidation is observed before input, without waiting for its event.
    input_revision: u64,
    /// The final text is in and waits for the target window to come back.
    final_wait: bool,
    /// For the log: when the service connected, how much audio arrived, how much of it was
    /// voiced and how loud it got. Tells a silent microphone from a slow service.
    connected_ms: Option<u64>,
    samples: u64,
    voiced: u32,
    peak: f32,
}

impl Dictation {
    fn at_pause(&self, now: Instant) -> bool {
        self.phase == Phase::Listening
            && self.capture.is_some()
            && self.next.is_none()
            && is_pause(
                now.duration_since(self.part_started),
                now.duration_since(self.changed),
                now.duration_since(self.last_voice),
                self.text.len() > self.base.len(),
            )
    }

    fn resumable(&self) -> bool {
        self.resume_input.is_some_and(|n| n == user_input_count())
    }

    /// Text still arrives after the user stopped: wait a little longer, up to `FINISH_MAX`.
    fn extend_finish(&mut self) {
        if self.phase != Phase::Finishing || self.final_wait {
            return;
        }
        if let Some(stopped) = self.stopped_at {
            let later = (Instant::now() + FINISH_WAIT).min(stopped + FINISH_MAX);
            self.deadline = Some(self.deadline.map_or(later, |d| d.max(later)));
        }
    }
}

/// Tells voice from steady background sound. A fixed threshold counts a noisy room or a
/// humming fan as voice forever, so caption sessions never end and dictation never finds a
/// pause. The floor is the quietest frame of the last few seconds; speech has gaps between
/// words, so the floor follows the background rather than the speaker.
#[derive(Clone, Debug, Default)]
struct NoiseFloor {
    buckets: VecDeque<f32>,
    current: Option<f32>,
    frames: u32,
}

impl NoiseFloor {
    fn threshold(&self) -> f32 {
        match self.buckets.iter().copied().reduce(f32::min) {
            // Until one second is known, only the fixed level applies.
            None => VOICE_LEVEL,
            Some(floor) => (floor + FLOOR_MARGIN).clamp(VOICE_LEVEL, VOICE_LEVEL_MAX),
        }
    }

    /// Classifies one frame, then learns from it.
    fn voiced(&mut self, level: f32) -> bool {
        let voiced = level >= self.threshold();
        self.current = Some(self.current.map_or(level, |c| c.min(level)));
        self.frames += 1;
        if self.frames >= FLOOR_BUCKET_FRAMES {
            if self.buckets.len() == FLOOR_BUCKETS {
                self.buckets.pop_front();
            }
            self.buckets.extend(self.current.take());
            self.frames = 0;
        }
        voiced
    }
}

/// A pause in dictation: the recognition ran long enough, has text, the text stopped
/// changing and the microphone is quiet.
fn is_pause(age: Duration, since_change: Duration, since_voice: Duration, has_text: bool) -> bool {
    has_text && age >= DICTATION_SPLIT_AFTER && since_change >= DICTATION_PAUSE && since_voice >= DICTATION_QUIET
}

struct CaptionSession {
    id: u64,
    started: Instant,
    last_voice: Instant,
    /// Latest recognized text and when it last changed.
    text: String,
    changed: Instant,
}

impl CaptionSession {
    fn new(id: u64, now: Instant) -> CaptionSession {
        CaptionSession { id, started: now, last_voice: now, text: String::new(), changed: now }
    }

    /// Why the session should end now: `Some(true)` when speech is likely to go on.
    fn ending(&self, now: Instant) -> Option<bool> {
        if now.duration_since(self.last_voice) >= CAPTION_SILENCE {
            return Some(false);
        }
        let age = now.duration_since(self.started);
        let paused = !self.text.is_empty() && now.duration_since(self.changed) >= CAPTION_PAUSE;
        (age >= CAPTION_MAX || (age >= CAPTION_CYCLE_AFTER && paused)).then_some(true)
    }
}

#[derive(Default)]
struct CaptionsState {
    on: bool,
    capture: Option<Capture>,
    capture_id: u64,
    /// When the current capture was opened, until its device runs.
    opening: Option<Instant>,
    floor: NoiseFloor,
    session: Option<CaptionSession>,
    /// Sessions whose text is still arriving.
    open: Vec<u64>,
    preroll: VecDeque<Vec<i16>>,
    model: Captions,
    held_for_dictation: bool,
    retry_at: Option<Instant>,
}

struct Core {
    options: Options,
    dirs: AppDirs,
    settings: Settings,
    events: EventSink,
    tx: mpsc::Sender<Msg>,
    hook: Option<Hook>,
    /// False after Windows removed the keyboard hook and it could not be reinstalled.
    hook_ok: bool,
    uia: Option<Uia>,
    slots: [Slot; 2],
    next_id: u64,
    dictation: Option<Dictation>,
    captions: CaptionsState,
    caps: HashMap<String, Capabilities>,
    last_foreground: isize,
    bar_visible: bool,
    level_tick: u32,
    next_idle_check: Instant,
    /// When to renew the standby dictation connection, and until when to keep doing so.
    next_warm: Instant,
    warm_until: Instant,
    /// Renewal is paused because the user is away (see [`WarmPolicy::idle`]).
    warm_paused: bool,
    /// Time since the last keyboard or mouse input; replaced in tests.
    user_idle: fn() -> Duration,
    /// Clock for caption times.
    epoch: Instant,
    notes: Notes,
    /// All accepted content has a durable copy; no more messages may mutate it.
    exit_prepared: bool,
}

fn run(mut core: Core, rx: mpsc::Receiver<Msg>) {
    'engine: loop {
        let timeout = core.next_timeout();
        match rx.recv_timeout(timeout) {
            Ok(Msg::Quit) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(msg) => core.handle(msg),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if core.exit_prepared {
            break;
        }
        // Drain a short burst before the next paced input step. Audio frames must not put
        // one Backspace's delay in front of every later control message.
        let until = Instant::now() + Duration::from_millis(2);
        for _ in 0..32 {
            match rx.try_recv() {
                Ok(Msg::Quit) => break 'engine,
                Ok(msg) => core.handle(msg),
                Err(_) => break,
            }
            if core.exit_prepared {
                break 'engine;
            }
            if Instant::now() >= until {
                break;
            }
        }
        core.tick(Instant::now());
    }
    // Disconnect queued callers before resource cleanup, which can take longer.
    drop(rx);
    core.shutdown();
}

fn audio_code(error: &AudioError) -> &'static str {
    match error {
        AudioError::NoDevice => "no_device",
        AudioError::Unsupported(_) => "unsupported_format",
        AudioError::Device(_) => "device",
        AudioError::File(_) => "file",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Applied {
    Done,
    /// A long correction was cut after [`BACKSPACE_STEP`]; nothing after it was typed.
    Behind,
    Failed,
}

/// Executes planned ops and confirms in `writer` exactly what reached the target.
fn apply_ops(ops: &[WriteOp], target: &focus::Target, writer: &mut Writer, revision: u64) -> Applied {
    if ops.is_empty() {
        return Applied::Done;
    }
    let window = if target.focus != 0 { target.focus } else { target.window };
    let texts = ops.iter().filter_map(|op| match op {
        WriteOp::Type(text) => Some(text.as_str()),
        WriteOp::Backspace(_) => None,
    });
    let route = input::route(window, texts);
    let text_limit = if matches!(route, input::Route::Messages(_)) { 1 } else { TYPE_STEP };
    apply_step(ops, writer, text_limit, |op| {
        if revision != input_revision() {
            return 0;
        }
        match op {
            WriteOp::Backspace(count) => input::backspace(route, *count),
            WriteOp::Type(text) => input::type_text(route, text),
        }
    })
}

/// Executes only one bounded part of the plan. The next engine turn replans from the
/// confirmed writer state, so cancellation and new recognition text are respected.
fn apply_step(
    ops: &[WriteOp],
    writer: &mut Writer,
    text_limit: usize,
    mut inject: impl FnMut(&WriteOp) -> usize,
) -> Applied {
    let Some(first) = ops.first() else { return Applied::Done };
    let (step, wanted, remaining) = match first {
        WriteOp::Backspace(count) => {
            let n = (*count).min(BACKSPACE_STEP);
            (WriteOp::Backspace(n), n, n < *count)
        }
        WriteOp::Type(text) => {
            let prefix: String = text.chars().take(text_limit).collect();
            let remaining = prefix.len() < text.len();
            let n = prefix.chars().count();
            (WriteOp::Type(prefix), n, remaining)
        }
    };
    let done = inject(&step).min(wanted);
    match step {
        WriteOp::Backspace(_) => writer.deleted(done),
        WriteOp::Type(text) => writer.typed(&text.chars().take(done).collect::<String>()),
    }
    if done < wanted {
        Applied::Failed
    } else if remaining || ops.len() > 1 {
        Applied::Behind
    } else {
        Applied::Done
    }
}

/// True while the target window is in front with the same focused control and, when UI
/// Automation knows it, the same focused element. An element UI Automation cannot check
/// counts as unchanged.
fn on_target(uia: Option<&Uia>, target: &Target, element: Option<&mut Element>) -> bool {
    if !focus::is_window(target.window) || !focus::is_focused(target) {
        return false;
    }
    match (uia, element) {
        (Some(uia), Some(element)) => uia.still_focused(element) != Some(false),
        _ => true,
    }
}

/// The only secrets the UI may name: keys of user-added speech services.
fn valid_secret_name(name: &str) -> bool {
    name.strip_prefix("speech.")
        .is_some_and(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c)))
}

impl Core {
    fn new(options: Options, events: EventSink, tx: mpsc::Sender<Msg>) -> Core {
        let dirs = AppDirs::resolve(options.config_dir.as_deref());
        let _ = std::fs::create_dir_all(&dirs.config);
        paths::init_logging(&dirs);
        let stored_version = Settings::stored_version(&dirs.settings_file());
        let mut settings = Settings::load(&dirs.settings_file());
        let issues = settings.sanitize();
        if !issues.is_empty() {
            log::warn!("settings had {} invalid values; defaults used", issues.len());
        }
        log::info!("engine {VERSION} starting");
        if let Some(version) = stored_version.filter(|&v| v < config::SETTINGS_VERSION) {
            // Keys of removed features must not stay behind in Credential Manager, and the
            // settings file drops their fields once it is written in the current version.
            for name in Settings::removed_secrets(version) {
                if secrets::has(name) && secrets::delete(name).is_ok() {
                    log::info!("secret {name} of a removed feature deleted");
                }
            }
            match settings.save(&dirs.settings_file()) {
                Ok(()) => log::info!("settings upgraded from version {version}"),
                Err(e) => log::warn!("settings upgrade not saved: {}", e.kind()),
            }
        }
        let uia = Uia::new();
        let hook = if options.hooks {
            let hook_tx = Mutex::new(tx.clone());
            let sink = Box::new(move |event| {
                let _ = hook_tx.lock().unwrap_or_else(|e| e.into_inner()).send(Msg::Hook(event));
            });
            match Hook::start(Bindings::from_settings(&settings.hotkeys), sink) {
                Ok(h) => Some(h),
                Err(e) => {
                    log::error!("input hook unavailable: {e}");
                    None
                }
            }
        } else {
            None
        };
        let foreground = focus::foreground();
        let notes = Notes::open(&dirs.config);
        let start_window = WarmPolicy::of(settings.dictation.preconnect_level)
            .filter(|policy| policy.at_start)
            .map_or(Duration::ZERO, |policy| policy.window);
        let mut core = Core {
            options,
            dirs,
            settings,
            events,
            tx,
            hook,
            hook_ok: true,
            uia,
            slots: [Slot::new(Channel::Dictation), Slot::new(Channel::Captions)],
            next_id: 0,
            dictation: None,
            captions: CaptionsState::default(),
            caps: HashMap::new(),
            last_foreground: if focus::is_own(foreground) { 0 } else { foreground },
            bar_visible: true,
            level_tick: 0,
            next_idle_check: Instant::now() + IDLE_CHECK,
            next_warm: Instant::now() + WARM_REFRESH,
            warm_until: Instant::now() + start_window,
            warm_paused: false,
            user_idle: focus::user_idle,
            epoch: Instant::now(),
            notes,
            exit_prepared: false,
        };
        if !start_window.is_zero() {
            core.warm_up();
        }
        core
    }

    fn prepare_shutdown(&mut self) -> Result<Value, String> {
        self.notes.flush().map_err(str::to_string)?;
        self.exit_prepared = true;
        Ok(json!({}))
    }

    fn shutdown(&mut self) {
        if !self.exit_prepared {
            // ll_destroy remains a best-effort fallback for callers that do not
            // use the acknowledged shutdown command.
            if let Err(code) = self.prepare_shutdown() {
                log::error!("forced shutdown could not preserve all changes: {code}");
            }
        }
        if let Some(d) = self.dictation.take() {
            for id in [Some(d.session), d.next].into_iter().flatten() {
                self.send(Channel::Dictation, &ToProvider::Cancel { id });
            }
        }
        self.captions.capture = None;
        self.hook = None;
        for slot in &mut self.slots {
            if let Some(process) = slot.process.take() {
                process.shutdown();
            }
        }
        log::info!("engine stopped");
    }

    fn emit(&self, value: Value) {
        (self.events)(&value.to_string());
    }

    fn emit_error(&self, scope: &str, code: &str) {
        log::warn!("{scope} error: {code}");
        self.emit(json!({"event": "error", "scope": scope, "code": code}));
    }

    fn notice(&self, code: &str) {
        self.emit(json!({"event": "notice", "code": code}));
    }

    fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn next_timeout(&self) -> Duration {
        let now = Instant::now();
        let mut timeout = self.next_idle_check.saturating_duration_since(now);
        if self.slots.iter().any(|slot| slot.process.is_some()) {
            timeout = timeout.min(Duration::from_millis(250));
        }
        if self.keeps_warm(now) {
            timeout = timeout.min(self.next_warm.saturating_duration_since(now));
        }
        if let Some(deadline) = self.dictation.as_ref().and_then(|d| d.deadline) {
            timeout = timeout.min(deadline.saturating_duration_since(now));
        }
        if self.captions.session.is_some() {
            timeout = timeout.min(Duration::from_millis(500));
        }
        if self.dictation.as_ref().is_some_and(|d| d.final_wait) {
            timeout = timeout.min(Duration::from_millis(100));
        }
        if let Some(deadline) = self.notes_deadline() {
            timeout = timeout.min(deadline.saturating_duration_since(now));
        }
        if self.dictation.as_ref().is_some_and(|d| d.behind) {
            timeout = Duration::ZERO;
        }
        timeout.max(Duration::from_millis(5))
    }

    fn handle(&mut self, msg: Msg) {
        if self.exit_prepared {
            if let Msg::Call(_, reply) = msg {
                let _ = reply.send(error_reply("stopped"));
            }
            return;
        }
        match msg {
            Msg::Call(request, reply) => {
                let response = self.call(&request);
                let _ = reply.send(response);
            }
            Msg::Hook(event) => self.on_hook(event),
            Msg::Frame { capture, frame } => self.on_frame(capture, frame),
            Msg::AudioFailed { capture } => self.on_audio_failed(capture),
            Msg::CaptureReady { capture, result } => self.on_capture_ready(capture, result),
            Msg::Provider { channel, generation, output } => self.on_provider(channel, generation, output),
            Msg::Probed { id, result } => {
                let value = match result {
                    Ok((name, caps)) => {
                        self.caps.insert(id.clone(), caps.clone());
                        json!({"event": "provider.checked", "id": id, "ok": true, "name": name, "capabilities": caps})
                    }
                    Err(code) => json!({"event": "provider.checked", "id": id, "ok": false, "error": code}),
                };
                self.emit(value);
            }
            Msg::Quit => {}
        }
    }

    fn tick(&mut self, now: Instant) {
        for channel in [Channel::Dictation, Channel::Captions] {
            let slot = &mut self.slots[channel as usize];
            if slot.process.as_ref().is_some_and(|process| process.stalled(now)) {
                log::warn!("{} speech service stdin made no progress; restarting", channel.scope());
                if let Some(mut process) = slot.process.take() {
                    process.terminate();
                    std::thread::spawn(move || process.shutdown());
                }
                // Ignore the old process's late stdout after this synthetic failure.
                slot.generation += 1;
                let generation = slot.generation;
                self.on_provider(channel, generation, ProviderOutput::Exited { stderr: Vec::new() });
            }
        }
        if self.dictation.as_ref().is_some_and(|d| d.final_wait) {
            self.finish_when_target_returns(now);
        } else if self.dictation.as_ref().and_then(|d| d.deadline).is_some_and(|t| now >= t) {
            log::warn!("dictation final text did not arrive in time");
            if let Some((ids, text)) = self.dictation.as_ref().map(|d| ([Some(d.session), d.next], d.text.clone())) {
                for id in ids.into_iter().flatten() {
                    self.send(Channel::Dictation, &ToProvider::Cancel { id });
                }
                self.write(Some(&text));
                if let Some(d) = self.dictation.as_mut().filter(|d| d.behind) {
                    // Finish typing what was recognized; the session itself is over.
                    d.final_wait = true;
                    d.deadline = None;
                } else {
                    self.complete("timeout");
                }
            }
        } else if self.dictation.as_ref().is_some_and(|d| d.behind) {
            self.write(None);
        }
        if self.dictation.as_ref().is_some_and(|d| d.at_pause(now)) {
            self.split_dictation(now);
        }
        if let Some(more) = self.captions.session.as_ref().and_then(|s| s.ending(now)) {
            self.end_caption_session();
            if more {
                // Sound goes on: have a connection ready for the next session.
                self.send(Channel::Captions, &ToProvider::Prepare);
            }
        }
        self.notes_tick();
        self.capture_timeouts(now);
        let policy = self.warm_policy();
        if let Some(policy) = policy.filter(|_| self.dictation.is_none() && now < self.warm_until) {
            let away = (self.user_idle)() >= policy.idle;
            if away != self.warm_paused {
                self.warm_paused = away;
                log::info!("{}", if away { "preconnect paused while the user is away" } else { "preconnect resumed" });
            }
        }
        if self.keeps_warm(now) && now >= self.next_warm {
            self.next_warm = now + WARM_REFRESH;
            self.warm_up();
        }
        if policy.is_some() && self.dictation.is_none() && now >= self.warm_until {
            let slot = &mut self.slots[Channel::Dictation as usize];
            if !slot.busy && slot.process.is_some() {
                slot.release();
            }
        }
        if now >= self.next_idle_check {
            self.next_idle_check = now + IDLE_CHECK;
            let minutes = u64::from(self.settings.services.idle_release_minutes.max(1));
            let keep_dictation = policy.is_some() && now < self.warm_until;
            for slot in &mut self.slots {
                let keep = keep_dictation && slot.channel == Channel::Dictation;
                let idle = now.duration_since(slot.idle_since) >= Duration::from_secs(minutes * 60);
                if slot.process.is_some() && !slot.busy && !keep && idle {
                    slot.release();
                }
            }
        }
    }

    // ----- speech providers -----

    /// The service's process, using the proxy chosen in the general settings.
    fn spec_for(&self, id: &str) -> Result<ProviderSpec, String> {
        let mut spec = self.plain_spec_for(id)?;
        let general = &self.settings.general;
        spec.env.extend(provider::proxy_env(&general.proxy_mode, &general.proxy_url));
        Ok(spec)
    }

    fn plain_spec_for(&self, id: &str) -> Result<ProviderSpec, String> {
        let services = &self.settings.services;
        if id == config::DOUBAO_ID {
            return provider::doubao_spec(&services.doubao, &self.options.search).map_err(|e| e.code().to_string());
        }
        let custom =
            services.custom_speech.iter().find(|c| c.id == id).ok_or_else(|| "unknown_provider".to_string())?;
        let official = provider::is_official(custom);
        let secret =
            if official || !custom.secret_env.trim().is_empty() { secrets::get(&format!("speech.{id}")) } else { None };
        let spec = if official {
            provider::official_spec(custom, secret, &self.options.search)
        } else {
            provider::custom_spec(custom, secret)
        };
        spec.map_err(|e| e.code().to_string())
    }

    fn ensure_provider(&mut self, channel: Channel) -> Result<(), String> {
        let id = self.settings.services.speech.clone();
        let spec = self.spec_for(&id)?;
        let key = spec.key();
        let tx = Mutex::new(self.tx.clone());
        let slot = &mut self.slots[channel as usize];
        if let Some(process) = slot.process.as_mut() {
            if slot.key == key && process.is_running() {
                return Ok(());
            }
        }
        slot.release();
        slot.generation += 1;
        let generation = slot.generation;
        let sink = Box::new(move |output| {
            let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send(Msg::Provider { channel, generation, output });
        });
        let process = ProviderProcess::spawn(&spec, sink).map_err(|e| {
            log::warn!("speech service failed to start: {}", e.kind());
            "spawn_failed".to_string()
        })?;
        log::info!("{} speech service started ({id})", channel.scope());
        slot.process = Some(process);
        slot.key = key;
        slot.provider_id = id;
        slot.idle_since = Instant::now();
        slot.dropped = 0;
        Ok(())
    }

    /// Sends a message to the provider of `channel`. Control messages are always queued;
    /// audio is dropped (and counted) only while the provider has stopped reading.
    fn send(&mut self, channel: Channel, message: &ToProvider) {
        let slot = &mut self.slots[channel as usize];
        let Some(process) = slot.process.as_mut() else { return };
        if let Err(e) = process.send(message) {
            if matches!(message, ToProvider::Audio { .. }) && e.kind() == std::io::ErrorKind::WouldBlock {
                slot.dropped += 1;
                if slot.dropped.is_power_of_two() {
                    log::warn!(
                        "{} speech service is not reading; {} audio frames dropped",
                        channel.scope(),
                        slot.dropped
                    );
                }
            } else {
                // The process is gone; its exit event follows and ends the sessions.
                log::warn!("{} speech service input failed: {}", channel.scope(), e.kind());
            }
        }
    }

    fn warm_policy(&self) -> Option<WarmPolicy> {
        WarmPolicy::of(self.settings.dictation.preconnect_level)
    }

    /// True while the standby dictation connection should be renewed: preconnect is on, no
    /// dictation runs, the window after start-up or the last dictation is open, and the user
    /// is at the computer.
    fn keeps_warm(&self, now: Instant) -> bool {
        self.warm_policy()
            .is_some_and(|policy| self.dictation.is_none() && now < self.warm_until && (self.user_idle)() < policy.idle)
    }

    /// Opens the standby window of the current level and prepares a connection now; does
    /// nothing while preconnect is off.
    fn rewarm(&mut self, now: Instant) {
        if let Some(policy) = self.warm_policy() {
            self.warm_until = now + policy.window;
            self.next_warm = now + WARM_REFRESH;
            self.warm_up();
        }
    }

    fn warm_up(&mut self) {
        if self.warm_policy().is_none() || self.dictation.is_some() {
            return;
        }
        if self.ensure_provider(Channel::Dictation).is_ok() {
            self.send(Channel::Dictation, &ToProvider::Prepare);
        }
    }

    fn on_provider(&mut self, channel: Channel, generation: u64, output: ProviderOutput) {
        if self.slots[channel as usize].generation != generation {
            return;
        }
        match output {
            ProviderOutput::Message(message) => self.on_message(channel, message),
            ProviderOutput::Malformed => log::debug!("{} provider sent a malformed line", channel.scope()),
            ProviderOutput::Exited { stderr } => {
                log::warn!("{} speech service exited: {}", channel.scope(), provider::stderr_summary(&stderr));
                let slot = &mut self.slots[channel as usize];
                slot.release();
                slot.busy = false;
                match channel {
                    Channel::Dictation => {
                        // Both sessions are gone: the part split off earlier keeps what it showed.
                        self.finish_part(None);
                        if let Some(id) = self.dictation.as_ref().map(|d| d.session) {
                            self.dictation_failed(id, "provider_exited");
                        }
                    }
                    Channel::Captions => {
                        let at = self.clock_ms();
                        for id in std::mem::take(&mut self.captions.open) {
                            self.captions.model.end(id, None, at);
                        }
                        if self.captions.session.take().is_some() {
                            self.emit_error("captions", "provider_exited");
                            self.captions.retry_at = Some(Instant::now() + Duration::from_secs(5));
                        }
                        self.emit_lines();
                        self.emit_state();
                    }
                }
            }
        }
    }

    fn is_dictation(&self, id: u64) -> bool {
        self.dictation.as_ref().is_some_and(|d| d.session == id || d.next == Some(id))
    }

    fn on_message(&mut self, channel: Channel, message: FromProvider) {
        match message {
            FromProvider::Hello { protocol, capabilities, .. } => {
                if protocol != PROTOCOL_VERSION {
                    log::warn!("speech service speaks protocol {protocol}, expected {PROTOCOL_VERSION}");
                }
                let id = self.slots[channel as usize].provider_id.clone();
                self.caps.insert(id, capabilities);
            }
            FromProvider::Connected { id } => {
                let Some(d) = self.dictation.as_mut().filter(|d| d.session == id) else { return };
                if d.connected_ms.is_none() {
                    d.connected_ms = Some(d.started.elapsed().as_millis() as u64);
                }
                if d.phase == Phase::Connecting {
                    d.phase = Phase::Listening;
                    self.emit_state();
                }
            }
            FromProvider::Text { id, text, stable } => {
                if self.is_dictation(id) {
                    self.dictation_text(id, &text, stable);
                } else {
                    self.caption_text(id, &text, stable);
                }
            }
            FromProvider::Final { id, text } => {
                if self.is_dictation(id) {
                    self.dictation_final(id, Some(&text));
                } else {
                    self.caption_end(id, Some(&text));
                }
            }
            FromProvider::End { id, cancelled } => {
                if self.is_dictation(id) {
                    if !cancelled {
                        self.dictation_final(id, None);
                    }
                } else {
                    self.caption_end(id, None);
                }
            }
            FromProvider::Error { id, code, message } => {
                log::warn!("{} service error {code} ({} chars)", channel.scope(), message.chars().count());
                match id {
                    Some(id) if self.is_dictation(id) => self.dictation_failed(id, &code),
                    Some(id) => {
                        if self.captions.open.contains(&id) {
                            let current = self.captions.session.as_ref().is_some_and(|s| s.id == id);
                            self.caption_end(id, None);
                            if current {
                                self.captions.retry_at = Some(Instant::now() + Duration::from_secs(5));
                            }
                            self.emit_error("captions", &code);
                        }
                    }
                    None => self.emit_error(channel.scope(), &code),
                }
            }
            FromProvider::Unknown => {}
        }
    }

    // ----- audio -----

    /// Opens a capture without waiting for the device: drivers can take seconds (Bluetooth
    /// headsets switch profile), and the engine must keep answering meanwhile. The result
    /// arrives as [`Msg::CaptureReady`]; frames may arrive before it.
    fn start_capture(&mut self, source: Source, follow_default: bool) -> Result<(Capture, u64), String> {
        let id = self.new_id();
        let frames = self.tx.clone();
        let errors = Mutex::new(self.tx.clone());
        let ready = Mutex::new(self.tx.clone());
        let sink = Box::new(move |frame| {
            let _ = frames.send(Msg::Frame { capture: id, frame });
        });
        let on_error = Arc::new(move |message: String| {
            if message == audio::DEFAULT_CHANGED {
                log::info!("default audio device changed; reopening");
            } else {
                log::warn!("audio stream error: {message}");
            }
            let _ = errors.lock().unwrap_or_else(|e| e.into_inner()).send(Msg::AudioFailed { capture: id });
        });
        let on_ready = Box::new(move |result| {
            let _ = ready.lock().unwrap_or_else(|e| e.into_inner()).send(Msg::CaptureReady { capture: id, result });
        });
        Capture::open(source, follow_default, sink, on_error, on_ready).map(|c| (c, id)).map_err(|e| {
            log::warn!("audio capture failed: {e}");
            audio_code(&e).to_string()
        })
    }

    fn on_capture_ready(&mut self, capture: u64, result: Result<(), AudioError>) {
        let dictation = self.dictation.as_ref().is_some_and(|d| d.capture.is_some() && d.capture_id == capture);
        if dictation {
            match result {
                Ok(()) => {
                    if let Some(d) = self.dictation.as_mut() {
                        let ms = d.started.elapsed().as_millis() as u64;
                        d.mic_ms = Some(ms);
                        log::info!("microphone ready after {ms} ms");
                    }
                }
                Err(e) => {
                    log::warn!("audio capture failed: {e}");
                    self.dictation_capture_failed(audio_code(&e));
                }
            }
            return;
        }
        if self.captions.capture.is_some() && self.captions.capture_id == capture {
            match result {
                Ok(()) => {
                    if let Some(opened) = self.captions.opening.take() {
                        log::info!("captions audio ready after {} ms", opened.elapsed().as_millis());
                    }
                }
                Err(e) => {
                    log::warn!("audio capture failed: {e}");
                    self.captions_capture_failed(audio_code(&e));
                }
            }
        }
    }

    /// A device that never reports ready counts as failed after [`audio::OPEN_TIMEOUT`].
    fn capture_timeouts(&mut self, now: Instant) {
        let late = |since: Instant| now.duration_since(since) >= audio::OPEN_TIMEOUT;
        if self.dictation.as_ref().is_some_and(|d| d.capture.is_some() && d.mic_ms.is_none() && late(d.started)) {
            log::warn!("audio capture failed: the microphone did not start");
            self.dictation_capture_failed("device");
        }
        if self.captions.capture.is_some() && self.captions.opening.is_some_and(late) {
            log::warn!("audio capture failed: the captions source did not start");
            self.captions_capture_failed("device");
        }
    }

    /// The microphone could not start: end the dictation before anything was heard.
    fn dictation_capture_failed(&mut self, code: &str) {
        let Some(d) = self.dictation.as_mut() else { return };
        d.capture = None;
        d.paused = true;
        let ids = [Some(d.session), d.next];
        for id in ids.into_iter().flatten() {
            self.send(Channel::Dictation, &ToProvider::Cancel { id });
        }
        self.emit_error("dictation", &format!("audio_{code}"));
        self.complete("error");
    }

    fn captions_capture_failed(&mut self, code: &str) {
        self.captions.capture = None;
        self.captions.opening = None;
        self.end_caption_session();
        self.captions.on = false;
        self.emit_error("captions", &format!("audio_{code}"));
        self.emit_state();
    }

    fn microphone(&self) -> Option<String> {
        let name = self.settings.dictation.microphone.trim();
        if name.is_empty() { None } else { Some(name.to_string()) }
    }

    fn dictation_source(&self) -> Source {
        match &self.options.dictation_audio {
            Some(path) => Source::File(path.clone()),
            None => Source::Microphone(self.microphone()),
        }
    }

    fn captions_source(&self) -> Source {
        if let Some(path) = &self.options.captions_audio {
            return Source::File(path.clone());
        }
        match self.settings.captions.source {
            SourceKind::Microphone => Source::Microphone(self.microphone()),
            SourceKind::System => Source::System,
            SourceKind::Mixed => Source::Mixed(self.microphone()),
        }
    }

    fn on_frame(&mut self, capture: u64, frame: AudioFrame) {
        let dictation_capture = self.dictation.as_ref().filter(|d| d.capture.is_some()).map(|d| d.capture_id);
        if dictation_capture == Some(capture) {
            self.level_tick = self.level_tick.wrapping_add(1);
            if self.bar_visible && self.level_tick % 2 == 0 {
                self.emit(json!({"event": "level", "value": (frame.level * 100.0).round() / 100.0}));
            }
            let Some(d) = self.dictation.as_mut() else { return };
            let id = d.next.unwrap_or(d.session);
            d.samples += frame.pcm.len() as u64;
            d.peak = d.peak.max(frame.level);
            if d.floor.voiced(frame.level) {
                d.last_voice = Instant::now();
                d.voiced += 1;
            }
            if d.audio.len() < RETRY_FRAMES {
                d.audio.push(frame.pcm.clone());
            }
            self.send(Channel::Dictation, &ToProvider::audio(id, &frame.pcm));
            return;
        }
        if self.captions.capture.is_some() && self.captions.capture_id == capture {
            self.caption_frame(frame);
        }
    }

    fn on_audio_failed(&mut self, capture: u64) {
        if self.dictation.as_ref().is_some_and(|d| d.capture.is_some() && d.capture_id == capture) {
            self.emit_error("dictation", "microphone_lost");
            self.stop_dictation(false);
            return;
        }
        if self.captions.capture.is_some() && self.captions.capture_id == capture {
            // The device went away or another one became the default; open the current one.
            self.captions.capture = None;
            self.captions.opening = None;
            self.end_caption_session();
            if self.captions.on {
                self.start_captions_capture();
            }
        }
    }

    // ----- dictation -----

    fn start_dictation(&mut self, trigger: Trigger, purpose: Purpose) {
        if self.dictation.is_some() {
            return;
        }
        let target = match purpose {
            Purpose::Type => {
                let foreground = focus::foreground();
                // The taskbar or desktop in front means a click on the tray or the desktop;
                // the text belongs to the window the user typed in before.
                let elsewhere = foreground == 0 || focus::is_own(foreground) || focus::is_shell(foreground);
                let window = if elsewhere { self.last_foreground } else { foreground };
                focus::target_of(window).filter(|t| t.pid != focus::own_pid() && !focus::is_shell(t.window))
            }
            Purpose::Note => None,
        };
        // Only the target's own element counts: the app's windows never take focus, but the
        // foreground may already have changed since the hotkey.
        let element =
            target.filter(|t| focus::foreground() == t.window).and_then(|_| self.uia.as_ref().and_then(Uia::focused));
        let mut blocked = None;
        match &target {
            None if purpose == Purpose::Note => {}
            None => blocked = Some("no_target"),
            Some(t) => {
                if self.uia.as_ref().is_some_and(|u| u.focused_is_password(Some(t))) {
                    log::info!("dictation refused: password field");
                    self.notice("password");
                    return;
                }
                if focus::blocked_by_elevation(t) {
                    blocked = Some("elevated");
                }
            }
        }
        if let Err(code) = self.ensure_provider(Channel::Dictation) {
            self.emit_error("dictation", &code);
            return;
        }
        let session = self.new_id();
        self.send(
            Channel::Dictation,
            &ToProvider::Start { id: session, sample_rate: SAMPLE_RATE, language: LANGUAGE.into() },
        );
        let (capture, capture_id) = match self.start_capture(self.dictation_source(), false) {
            Ok(c) => c,
            Err(code) => {
                self.send(Channel::Dictation, &ToProvider::Cancel { id: session });
                self.emit_error("dictation", &format!("audio_{code}"));
                return;
            }
        };
        let mode = match (trigger, purpose) {
            (Trigger::Hold, _) => Mode::Normal,
            (Trigger::Toggle, Purpose::Note) => Mode::Continuous { replay: false },
            (Trigger::Toggle, Purpose::Type) => {
                Mode::Continuous { replay: self.settings.dictation.end_action == EndAction::EndAndReplay }
            }
        };
        if let Some(hook) = &self.hook {
            hook.begin(mode);
        }
        self.slots[Channel::Dictation as usize].busy = true;
        log::info!("dictation started ({trigger:?}, {purpose:?}, target {})", blocked.unwrap_or("ok"));
        if let Some(code) = blocked {
            self.notice(code);
        }
        let mut mirrored = false;
        if purpose == Purpose::Type
            && self.captions.on
            && self.settings.captions.source == SourceKind::Microphone
            && self.options.captions_audio.is_none()
        {
            // One recognition: captions show the dictation text while the microphone is in use.
            self.captions.capture = None;
            self.end_caption_session();
            self.captions.held_for_dictation = true;
            self.captions.model.begin(session, LineSource::Dictation);
            mirrored = true;
        }
        self.dictation = Some(Dictation {
            session,
            mirror_id: session,
            trigger,
            purpose,
            phase: Phase::Connecting,
            target,
            element,
            blocked,
            paused: false,
            writer: Writer::new(self.settings.dictation.write_mode),
            behind: false,
            base: String::new(),
            text: String::new(),
            stable: 0,
            capture: Some(capture),
            capture_id,
            mic_ms: None,
            floor: NoiseFloor::default(),
            element_checked: None,
            started: Instant::now(),
            first_text_ms: None,
            deadline: None,
            replay: false,
            timed_out: false,
            mirrored,
            audio: Vec::new(),
            retried: false,
            next: None,
            part_started: Instant::now(),
            last_voice: Instant::now(),
            changed: Instant::now(),
            stopped_at: None,
            resume_input: None,
            input_revision: input_revision(),
            final_wait: false,
            connected_ms: None,
            samples: 0,
            voiced: 0,
            peak: 0.0,
        });
        self.emit_state();
    }

    fn stop_dictation(&mut self, replay: bool) {
        let Some(d) = self.dictation.as_mut() else {
            if let Some(hook) = &self.hook {
                hook.release(replay);
            }
            return;
        };
        if d.phase == Phase::Finishing {
            d.replay |= replay;
            return;
        }
        d.replay = replay;
        d.capture = None;
        d.phase = Phase::Finishing;
        let now = Instant::now();
        d.stopped_at = Some(now);
        d.deadline = Some(now + FINISH_WAIT);
        let id = d.next.unwrap_or(d.session);
        let held_note = d.trigger == Trigger::Hold && d.purpose == Purpose::Note;
        self.send(Channel::Dictation, &ToProvider::Stop { id });
        // A held note has no target to protect while recognition finishes.
        if !held_note {
            if let Some(hook) = &self.hook {
                hook.hold_input();
            }
        }
        self.emit_state();
    }

    fn cancel_dictation(&mut self) {
        let Some(d) = self.dictation.as_mut() else { return };
        d.replay = false;
        d.capture = None;
        d.paused = true;
        let ids = [Some(d.session), d.next];
        for id in ids.into_iter().flatten() {
            self.send(Channel::Dictation, &ToProvider::Cancel { id });
        }
        self.complete("cancelled");
    }

    fn dictation_text(&mut self, id: u64, raw: &str, stable: usize) {
        if self.dictation.as_ref().is_some_and(|d| d.next == Some(id)) {
            // A provider that runs sessions side by side: the earlier part is done.
            self.finish_part(None);
        }
        let Some(d) = self.dictation.as_mut() else { return };
        let piece = dictation::sanitize(raw);
        let stable = stable.min(piece.chars().count());
        let text = format!("{}{}", d.base, piece);
        if text != d.text {
            d.changed = Instant::now();
            d.extend_finish();
        }
        d.text = text;
        d.stable = d.base.chars().count() + stable;
        if d.first_text_ms.is_none() && !piece.trim().is_empty() {
            let ms = d.started.elapsed().as_millis() as u64;
            d.first_text_ms = Some(ms);
            log::info!("first text after {ms} ms");
        }
        let became_listening = d.phase == Phase::Connecting;
        if became_listening {
            d.phase = Phase::Listening;
        }
        self.write(None);
        self.mirror_update(false);
        self.emit_text();
        if became_listening {
            self.emit_state();
        }
    }

    /// Splits the recognition at a pause: the current session stops, so the service confirms
    /// and punctuates what was said so far, and a new session takes the audio that follows.
    /// Later corrections then only touch the newest part instead of everything typed.
    fn split_dictation(&mut self, now: Instant) {
        let id = self.new_id();
        let Some(d) = self.dictation.as_mut() else { return };
        let old = d.session;
        d.next = Some(id);
        d.audio.clear();
        d.part_started = now;
        d.changed = now;
        log::debug!("dictation split at a pause");
        self.send(Channel::Dictation, &ToProvider::Prepare);
        self.send(Channel::Dictation, &ToProvider::Stop { id: old });
        self.send(Channel::Dictation, &ToProvider::Start { id, sample_rate: SAMPLE_RATE, language: LANGUAGE.into() });
    }

    /// The session stopped at a pause delivered its last text (`None`: keep what it showed);
    /// the session started after it takes over.
    fn finish_part(&mut self, text: Option<&str>) {
        let Some(d) = self.dictation.as_mut() else { return };
        let Some(next) = d.next.take() else { return };
        if let Some(t) = text {
            d.text = format!("{}{}", d.base, dictation::sanitize(t));
        }
        d.base = d.text.clone();
        d.stable = d.text.chars().count();
        d.session = next;
        d.changed = Instant::now();
        d.extend_finish();
        self.write(None);
        self.mirror_update(false);
        self.emit_text();
    }

    /// The provider finished a session: with `text` from `final`, or `None` after a bare `end`.
    fn dictation_final(&mut self, id: u64, text: Option<&str>) {
        let Some(d) = self.dictation.as_ref() else { return };
        if d.next.is_some() {
            if d.session == id {
                self.finish_part(text);
                return;
            }
            // The newer session ended first: the earlier part keeps what it showed.
            self.finish_part(None);
        }
        let Some(d) = self.dictation.as_mut() else { return };
        if let Some(t) = text {
            d.text = format!("{}{}", d.base, dictation::sanitize(t));
        }
        d.stable = d.text.chars().count();
        if d.phase != Phase::Finishing {
            // The service ended its session on its own; continue in a new one.
            d.base = d.text.clone();
            d.audio.clear();
            let id = self.new_id();
            if let Some(d) = self.dictation.as_mut() {
                d.session = id;
                d.part_started = Instant::now();
                d.changed = Instant::now();
            }
            log::info!("dictation continues in a new service session");
            self.send(
                Channel::Dictation,
                &ToProvider::Start { id, sample_rate: SAMPLE_RATE, language: LANGUAGE.into() },
            );
            self.write(None);
            self.mirror_update(false);
            self.emit_text();
            return;
        }
        let text = d.text.clone();
        self.write(Some(&text));
        self.mirror_update(true);
        self.emit_text();
        if let Some(d) = self.dictation.as_mut().filter(|d| d.behind) {
            // A long correction is still being typed, step by step.
            d.final_wait = true;
            return;
        }
        if let Some(d) = self.dictation.as_mut().filter(|d| d.paused && d.resumable() && !d.timed_out) {
            d.final_wait = true;
            d.deadline = Some(Instant::now() + RESUME_WAIT);
            return;
        }
        self.complete("done");
    }

    /// The final text is in; finish a correction still in progress, and give the target
    /// window a moment to come back after a short switch.
    fn finish_when_target_returns(&mut self, now: Instant) {
        let Some(text) = self.dictation.as_ref().map(|d| d.text.clone()) else { return };
        self.write(Some(&text));
        let waiting = self
            .dictation
            .as_ref()
            .is_some_and(|d| d.behind || (d.paused && d.resumable() && d.deadline.is_some_and(|t| now < t)));
        if !waiting {
            self.complete("done");
        }
    }

    /// Resumes writing when the target is back in front and the user has not typed or clicked
    /// since writing paused, so the caret is still where the text ended.
    fn try_resume(&mut self) {
        let Some(d) = self.dictation.as_mut() else { return };
        let Some(target) = d.target else { return };
        if !d.paused || d.resume_input.is_none() || d.timed_out || d.blocked.is_some() {
            return;
        }
        if !d.resumable() || d.input_revision != input_revision() {
            d.resume_input = None;
            return;
        }
        if !on_target(self.uia.as_ref(), &target, d.element.as_mut()) {
            return;
        }
        d.paused = false;
        d.resume_input = None;
        d.writer.thaw();
        log::info!("dictation target back in front; writing resumed");
        self.emit_state();
    }

    fn dictation_failed(&mut self, id: u64, code: &str) {
        if self.dictation.as_ref().is_some_and(|d| d.next.is_some() && d.session == id) {
            log::warn!("dictation part ended with {code}; keeping its text");
            self.finish_part(None);
            return;
        }
        let Some(d) = self.dictation.as_ref() else { return };
        let can_retry = !d.retried && d.audio.len() < RETRY_FRAMES;
        if can_retry && self.ensure_provider(Channel::Dictation).is_ok() {
            let id = self.new_id();
            let Some(d) = self.dictation.as_mut() else { return };
            d.retried = true;
            if d.next.is_some() {
                d.next = Some(id);
            } else {
                d.session = id;
            }
            let audio = d.audio.clone();
            let finishing = d.phase == Phase::Finishing;
            if finishing {
                d.deadline = Some(Instant::now() + FINISH_WAIT);
            }
            log::info!("dictation reconnecting after {code}");
            self.notice("reconnecting");
            self.send(
                Channel::Dictation,
                &ToProvider::Start { id, sample_rate: SAMPLE_RATE, language: LANGUAGE.into() },
            );
            for pcm in &audio {
                self.send(Channel::Dictation, &ToProvider::audio(id, pcm));
            }
            if finishing {
                self.send(Channel::Dictation, &ToProvider::Stop { id });
            }
            return;
        }
        self.emit_error("dictation", code);
        if let Some(d) = self.dictation.as_mut() {
            d.paused = true;
            d.capture = None;
        }
        self.complete("error");
    }

    /// Types new text into the target, or pauses when the target is gone or lost focus.
    /// A long correction goes in steps; `behind` asks the engine for the next one.
    fn write(&mut self, final_text: Option<&str>) {
        self.try_resume();
        let uia = self.uia.as_ref();
        let Some(d) = self.dictation.as_mut() else { return };
        d.behind = false;
        if d.purpose == Purpose::Type && d.input_revision != input_revision() {
            d.paused = true;
            d.resume_input = None;
            d.writer.freeze();
        }
        if d.blocked.is_some() || d.paused || d.timed_out {
            return;
        }
        let Some(target) = d.target else { return };
        let ops = match final_text {
            Some(text) => d.writer.finish(text),
            None => d.writer.update(&d.text, d.stable),
        };
        // The window check is cheap and runs every time; the element check only before input,
        // and at most every UIA_RECHECK while a long correction goes in step by step.
        let in_front = focus::is_window(target.window) && focus::is_focused(&target);
        let element_ok =
            if !in_front || ops.is_empty() || d.element_checked.is_some_and(|at| at.elapsed() < UIA_RECHECK) {
                true
            } else {
                let ok = on_target(uia, &target, d.element.as_mut());
                if ok {
                    d.element_checked = Some(Instant::now());
                }
                ok
            };
        if !in_front || !element_ok {
            d.paused = true;
            d.element_checked = None;
            d.writer.freeze();
            d.resume_input = Some(user_input_count());
            let why = if in_front { "focused element changed".into() } else { focus::describe_loss(&target) };
            log::info!("dictation target lost focus ({why}); writing paused");
            self.notice("paused");
            self.emit_text();
            self.emit_state();
            return;
        }
        match apply_ops(&ops, &target, &mut d.writer, d.input_revision) {
            Applied::Done => {}
            Applied::Behind => d.behind = true,
            Applied::Failed => {
                d.paused = true;
                d.writer.freeze();
                log::warn!("text input was rejected");
                self.notice("write_failed");
                self.emit_text();
            }
        }
    }

    fn complete(&mut self, reason: &str) {
        let Some(mut d) = self.dictation.take() else { return };
        drop(d.capture.take());
        let wrote = d.blocked.is_none() && !d.paused && !d.timed_out && d.target.is_some();
        let note = d.purpose == Purpose::Note;
        let unwritten =
            if wrote || note || reason == "cancelled" { String::new() } else { d.writer.unwritten(&d.text) };
        if let Some(hook) = &self.hook {
            hook.release(d.replay && !d.timed_out);
        }
        let duration = d.started.elapsed().as_millis() as u64;
        let audio_ms = d.samples * 1000 / u64::from(SAMPLE_RATE);
        log::info!(
            "dictation {reason}: {} chars, {} unwritten, first text {:?} ms, connected {:?} ms, audio {audio_ms} ms ({} voiced frames, peak {:.2}), microphone {:?} ms, {duration} ms",
            d.text.chars().count(),
            unwritten.chars().count(),
            d.first_text_ms,
            d.connected_ms,
            d.voiced,
            d.peak,
            d.mic_ms
        );
        let heard_nothing = d.purpose == Purpose::Type
            && matches!(reason, "done" | "timeout")
            && d.text.trim().is_empty()
            && audio_ms >= NOTHING_HEARD_AFTER_MS;
        self.emit(json!({
            "event": "dictation.done",
            "reason": reason,
            "purpose": if note { "note" } else { "type" },
            "text": d.text,
            "unwritten": unwritten,
            "latencyMs": d.first_text_ms,
            "durationMs": duration,
        }));
        let slot = &mut self.slots[Channel::Dictation as usize];
        slot.busy = false;
        slot.idle_since = Instant::now();
        if d.mirrored {
            let at = self.clock_ms();
            self.captions.model.end(d.mirror_id, Some(&d.text), at);
            self.emit_lines();
        }
        // A note that failed before any word was heard already showed its error.
        if note && reason != "cancelled" && !(reason == "error" && d.text.trim().is_empty()) {
            self.save_note(&d.text);
        }
        if heard_nothing {
            // Without this, a muted or busy microphone ends dictation without a word.
            self.notice(if d.voiced == 0 { "no_voice" } else { "no_text" });
        }
        if self.captions.held_for_dictation {
            self.captions.held_for_dictation = false;
            if self.captions.on {
                self.start_captions_capture();
            }
        }
        self.emit_state();
        // Reloads a changed credential/service before preparing the next connection.
        self.rewarm(Instant::now());
    }

    fn emit_text(&self) {
        let Some(d) = &self.dictation else { return };
        let held = d.blocked.is_some() || d.paused || d.timed_out;
        let unwritten = if held { d.writer.unwritten(&d.text) } else { String::new() };
        self.emit(json!({"event": "dictation.text", "text": d.text, "unwritten": unwritten}));
    }

    fn mirror_update(&mut self, finished: bool) {
        let Some(d) = self.dictation.as_ref().filter(|d| d.mirrored) else { return };
        let (id, text) = (d.mirror_id, d.text.clone());
        let stable = if finished { text.chars().count() } else { d.stable };
        let at = self.clock_ms();
        self.captions.model.update(id, &text, stable, at);
        self.emit_lines();
    }

    // ----- captions -----

    fn set_captions(&mut self, on: bool) {
        if on == self.captions.on {
            return;
        }
        self.captions.on = on;
        if on {
            log::info!("captions on");
            let mic_in_use = self.dictation.is_some()
                && self.settings.captions.source == SourceKind::Microphone
                && self.options.captions_audio.is_none();
            if mic_in_use {
                if let Some(d) = self.dictation.as_mut() {
                    if !d.mirrored {
                        d.mirrored = true;
                        let id = d.mirror_id;
                        self.captions.model.begin(id, LineSource::Dictation);
                    }
                }
                self.captions.held_for_dictation = true;
            } else {
                self.start_captions_capture();
            }
        } else {
            log::info!("captions off");
            self.captions.capture = None;
            self.captions.opening = None;
            self.captions.session = None;
            // Sessions already stopped can still be sending buffered audio or waiting
            // for their final result. Closing captions cancels those requests too.
            for id in std::mem::take(&mut self.captions.open) {
                self.send(Channel::Captions, &ToProvider::Cancel { id });
            }
            self.captions.preroll.clear();
            self.captions.held_for_dictation = false;
            self.captions.model.clear();
            let slot = &mut self.slots[Channel::Captions as usize];
            slot.busy = false;
            slot.idle_since = Instant::now();
            if let Some(d) = self.dictation.as_mut() {
                d.mirrored = false;
            }
            self.emit_lines();
        }
        self.emit_state();
    }

    fn start_captions_capture(&mut self) {
        // Captions follow the default devices: playback moves to headphones, a headset
        // becomes the default microphone, and the captions move with them.
        match self.start_capture(self.captions_source(), true) {
            Ok((capture, id)) => {
                self.captions.capture = Some(capture);
                self.captions.capture_id = id;
                self.captions.opening = Some(Instant::now());
                self.captions.floor = NoiseFloor::default();
            }
            Err(code) => {
                self.captions.on = false;
                self.emit_error("captions", &format!("audio_{code}"));
                self.emit_state();
            }
        }
    }

    fn caption_frame(&mut self, frame: AudioFrame) {
        let now = Instant::now();
        let voiced = self.captions.floor.voiced(frame.level);
        if let Some(s) = self.captions.session.as_mut() {
            if voiced {
                s.last_voice = now;
            }
            let id = s.id;
            self.send(Channel::Captions, &ToProvider::audio(id, &frame.pcm));
            return;
        }
        self.captions.preroll.push_back(frame.pcm);
        while self.captions.preroll.len() > PREROLL_FRAMES {
            self.captions.preroll.pop_front();
        }
        if !voiced || self.captions.retry_at.is_some_and(|t| now < t) {
            return;
        }
        if let Err(code) = self.ensure_provider(Channel::Captions) {
            self.emit_error("captions", &code);
            self.captions.retry_at = Some(now + Duration::from_secs(10));
            return;
        }
        self.captions.retry_at = None;
        let id = self.new_id();
        self.send(Channel::Captions, &ToProvider::Start { id, sample_rate: SAMPLE_RATE, language: LANGUAGE.into() });
        let preroll: Vec<Vec<i16>> = self.captions.preroll.drain(..).collect();
        for pcm in &preroll {
            self.send(Channel::Captions, &ToProvider::audio(id, pcm));
        }
        self.captions.model.begin(id, LineSource::Captions);
        self.captions.open.push(id);
        self.captions.session = Some(CaptionSession::new(id, now));
        self.slots[Channel::Captions as usize].busy = true;
        self.emit_state();
    }

    fn end_caption_session(&mut self) {
        if let Some(s) = self.captions.session.take() {
            self.send(Channel::Captions, &ToProvider::Stop { id: s.id });
            let slot = &mut self.slots[Channel::Captions as usize];
            slot.busy = false;
            slot.idle_since = Instant::now();
            self.emit_state();
        }
    }

    fn caption_text(&mut self, id: u64, raw: &str, stable: usize) {
        if !self.captions.open.contains(&id) {
            return;
        }
        if let Some(s) = self.captions.session.as_mut().filter(|s| s.id == id && s.text != raw) {
            s.text = raw.to_string();
            s.changed = Instant::now();
        }
        let text = dictation::sanitize(raw);
        let at = self.clock_ms();
        self.captions.model.update(id, &text, stable, at);
        self.emit_lines();
    }

    fn caption_end(&mut self, id: u64, final_text: Option<&str>) {
        if !self.captions.open.contains(&id) {
            return;
        }
        self.captions.open.retain(|&o| o != id);
        let text = final_text.map(dictation::sanitize);
        let at = self.clock_ms();
        self.captions.model.end(id, text.as_deref(), at);
        if self.captions.session.as_ref().is_some_and(|s| s.id == id) {
            self.captions.session = None;
            let slot = &mut self.slots[Channel::Captions as usize];
            slot.busy = false;
            slot.idle_since = Instant::now();
            self.emit_state();
        }
        self.emit_lines();
    }

    fn emit_lines(&self) {
        let lines: Vec<Line> = self.captions.model.recent(CAPTION_LINES);
        self.emit(json!({"event": "captions.lines", "lines": lines}));
    }

    // ----- input hook -----

    fn on_hook(&mut self, event: HookEvent) {
        match event {
            HookEvent::Foreground { window, input_count } => {
                if window != 0 && !focus::is_own(window) {
                    // A click on the tray icon or the desktop must not replace the window the
                    // user was typing in.
                    if !focus::is_shell(window) {
                        self.last_foreground = window;
                    }
                    let lost = self
                        .dictation
                        .as_ref()
                        .and_then(|d| d.target.filter(|t| t.window != window && !d.paused && d.blocked.is_none()));
                    if let Some(target) = lost {
                        if let Some(d) = self.dictation.as_mut() {
                            d.paused = true;
                            d.writer.freeze();
                            d.resume_input = Some(input_count);
                        }
                        log::info!(
                            "dictation target lost focus ({}); writing paused",
                            focus::describe_front(window, &target)
                        );
                        self.notice("paused");
                        self.emit_text();
                        self.emit_state();
                    } else if self
                        .dictation
                        .as_ref()
                        .is_some_and(|d| d.paused && d.resumable() && d.target.is_some_and(|t| t.window == window))
                    {
                        // The target is back: type what is waiting if the caret has not moved.
                        self.write(None);
                        self.emit_text();
                    }
                }
            }
            HookEvent::TimedOut => {
                if let Some(d) = self.dictation.as_mut().filter(|d| d.input_revision != input_revision()) {
                    d.timed_out = true;
                    d.replay = false;
                }
            }
            HookEvent::Edited => {
                if let Some(d) = self
                    .dictation
                    .as_mut()
                    .filter(|d| d.purpose == Purpose::Type && d.input_revision != input_revision())
                {
                    d.paused = true;
                    d.behind = false;
                    d.resume_input = None;
                    d.writer.freeze();
                    self.notice("paused");
                    self.emit_text();
                    self.emit_state();
                }
            }
            HookEvent::Health(ok) => {
                if self.hook_ok != ok {
                    self.hook_ok = ok;
                    if !ok {
                        self.emit_error("hotkey", "hook_unavailable");
                    }
                }
            }
            HookEvent::Signal(signal) => self.on_signal(signal),
        }
    }

    fn on_signal(&mut self, signal: Signal) {
        match signal {
            // A new press is explicit activity, also after the window expired. Its
            // preparation stays alive until HoldStart follows (~250 ms).
            Signal::HoldArmed => self.rewarm(Instant::now()),
            Signal::HoldStart => self.start_dictation(Trigger::Hold, Purpose::Type),
            Signal::HoldEnd | Signal::HoldInterrupted => {
                if self.dictation.as_ref().is_some_and(|d| d.trigger == Trigger::Hold && d.purpose == Purpose::Type) {
                    self.stop_dictation(false);
                }
            }
            Signal::HoldDisarmed => {}
            Signal::Toggle => {
                if self.dictation.is_some() {
                    self.stop_dictation(false);
                } else {
                    self.start_dictation(Trigger::Toggle, Purpose::Type);
                }
            }
            Signal::Captions => {
                let on = !self.captions.on;
                self.set_captions(on);
            }
            Signal::NoteHoldStart => {
                if self.dictation.as_ref().is_some_and(|d| d.purpose == Purpose::Type) {
                    self.notice("busy");
                } else {
                    self.start_dictation(Trigger::Hold, Purpose::Note);
                }
            }
            Signal::NoteHoldEnd => {
                if self.dictation.as_ref().is_some_and(|d| d.trigger == Trigger::Hold && d.purpose == Purpose::Note) {
                    self.stop_dictation(false);
                }
            }
            Signal::NoteToggle => self.toggle_note(),
            Signal::NoteBlank => {
                let _ = self.add_card(CardKind::Note, "", true);
            }
            Signal::Cards => {
                let on = !self.cards_shown();
                self.set_cards_shown(on);
            }
            // The setting belongs to the UI's settings tree; it flips and saves it.
            Signal::ShowText => self.emit(json!({"event": "ui.toggle", "target": "dictationText"})),
            Signal::EndedByKey { replay, .. } | Signal::EndedByClick { replay, .. } => self.stop_dictation(replay),
            Signal::EndedByTap => self.stop_dictation(false),
            Signal::Recorded(binding) => self.emit(json!({"event": "hotkey.recorded", "binding": binding})),
        }
    }

    // ----- UI commands -----

    fn emit_state(&self) {
        self.emit(self.state());
    }

    fn state(&self) -> Value {
        let dictation = match &self.dictation {
            None => json!({"phase": "idle"}),
            Some(d) => json!({
                "phase": d.phase.name(),
                "trigger": if d.trigger == Trigger::Hold { "hold" } else { "toggle" },
                "purpose": if d.purpose == Purpose::Note { "note" } else { "type" },
                "paused": d.paused,
                "blocked": d.blocked,
            }),
        };
        let listening = self.captions.session.is_some() || self.captions.held_for_dictation;
        json!({
            "event": "state",
            "dictation": dictation,
            "captions": {"on": self.captions.on, "listening": listening},
            "cards": {"shown": self.cards_shown()},
        })
    }

    /// Dictation started from the tray or a menu: that click put the taskbar or this app in
    /// front. Bring back the window the user typed in, so the text goes there.
    fn restore_target(&self) {
        let front = focus::foreground();
        let elsewhere = front == 0 || focus::is_own(front) || focus::is_shell(front);
        if elsewhere && self.last_foreground != front && focus::bring_to_front(self.last_foreground) {
            log::info!("dictation target brought back to the front");
        }
    }

    fn toggle_note(&mut self) {
        match self.dictation.as_ref().map(|d| d.purpose) {
            Some(Purpose::Note) => self.stop_dictation(false),
            Some(Purpose::Type) => self.notice("busy"),
            None => self.start_dictation(Trigger::Toggle, Purpose::Note),
        }
    }

    fn call(&mut self, request: &Value) -> Value {
        if self.exit_prepared {
            return error_reply("stopped");
        }
        let cmd = request["cmd"].as_str().unwrap_or("");
        let result = match self.notes_call(cmd, request) {
            Some(result) => result,
            None => self.core_call(cmd, request),
        };
        match result {
            Ok(mut value) => {
                if let Value::Object(map) = &mut value {
                    map.insert("ok".into(), Value::Bool(true));
                }
                value
            }
            Err(code) => error_reply(&code),
        }
    }

    fn core_call(&mut self, cmd: &str, request: &Value) -> Result<Value, String> {
        match cmd {
            "shutdown" => self.prepare_shutdown(),
            "status" => Ok(self.status()),
            "settings.get" => Ok(json!({"settings": self.settings})),
            "settings.defaults" => Ok(json!({"settings": Settings::default()})),
            "settings.set" => self.set_settings(request),
            "devices" => Ok(json!({"microphones": audio::input_devices(), "default": audio::default_input()})),
            "providers" => Ok(self.providers()),
            "provider.check" => self.check_provider(request),
            "secret.set" => self.set_secret(request),
            "secret.delete" => self.delete_secret(request),
            "secret.status" => Ok(self.secret_status(request)),
            "hotkey.record" => match &self.hook {
                Some(hook) => {
                    hook.record(request["on"].as_bool().unwrap_or(true));
                    Ok(json!({}))
                }
                None => Err("hook_unavailable".into()),
            },
            "hotkey.capture" => {
                let on = request["on"].as_bool().ok_or("capture_invalid_request")?;
                let window = if on {
                    request["window"]
                        .as_i64()
                        .and_then(|window| isize::try_from(window).ok())
                        .filter(|&window| window != 0)
                        .ok_or("capture_invalid_request")?
                } else {
                    0
                };
                let token = if on {
                    0
                } else {
                    request["token"].as_u64().filter(|&token| token != 0).ok_or("capture_invalid_request")?
                };
                if on && self.dictation.is_some() {
                    return Err("capture_busy".into());
                }
                let reply = self.hook.as_ref().ok_or("capture_unavailable")?.capture(on, window, token)?;
                Ok(json!({"active": reply.active, "token": reply.token}))
            }
            "hotkey.check" => {
                let binding = request["binding"].as_str().unwrap_or("");
                Ok(self.check_hotkey(binding, request["field"].as_str().unwrap_or("")))
            }
            "dictation.start" => {
                // Tray triple-clicks start continuous dictation without toggling an
                // existing session, even if the UI has not received its state yet.
                if self.dictation.is_none() {
                    self.restore_target();
                }
                self.start_dictation(Trigger::Toggle, Purpose::Type);
                Ok(json!({}))
            }
            "dictation.toggle" => {
                if self.dictation.is_some() {
                    self.stop_dictation(false);
                } else {
                    self.restore_target();
                    self.start_dictation(Trigger::Toggle, Purpose::Type);
                }
                Ok(json!({}))
            }
            "note.toggle" => {
                self.toggle_note();
                Ok(json!({}))
            }
            "dictation.stop" => {
                self.stop_dictation(false);
                Ok(json!({}))
            }
            "dictation.cancel" => {
                self.cancel_dictation();
                Ok(json!({}))
            }
            "captions.toggle" => {
                let on = !self.captions.on;
                self.set_captions(on);
                Ok(json!({"on": self.captions.on}))
            }
            "captions.set" => {
                self.set_captions(request["on"].as_bool().unwrap_or(false));
                Ok(json!({"on": self.captions.on}))
            }
            "captions.lines" => Ok(json!({"lines": self.captions.model.recent(CAPTION_LINES)})),
            "ui.visibility" => {
                if let Some(bar) = request["bar"].as_bool() {
                    self.bar_visible = bar;
                }
                Ok(json!({}))
            }
            "diagnostics.export" => self.export_diagnostics(),
            _ => Err("unknown_command".into()),
        }
    }

    fn status(&self) -> Value {
        let id = &self.settings.services.speech;
        let slot = &self.slots[Channel::Dictation as usize];
        let mut value = self.state();
        if let Value::Object(map) = &mut value {
            map.remove("event");
            map.insert("version".into(), json!(VERSION));
            map.insert("hook".into(), json!(self.hook.is_some() && self.hook_ok));
            map.insert(
                "provider".into(),
                json!({"id": id, "running": slot.process.is_some(), "capabilities": self.caps.get(id)}),
            );
            map.insert("configDir".into(), json!(self.dirs.config));
            map.insert("logFile".into(), json!(self.dirs.log_file()));
        }
        value
    }

    fn set_settings(&mut self, request: &Value) -> Result<Value, String> {
        let mut next: Settings =
            serde_json::from_value(request["settings"].clone()).map_err(|_| "bad_settings".to_string())?;
        let issues = next.sanitize();
        next.save(&self.dirs.settings_file()).map_err(|_| "save_failed".to_string())?;
        let old = std::mem::replace(&mut self.settings, next);
        self.apply_settings(&old);
        log::info!("settings saved ({} issues)", issues.len());
        Ok(json!({"settings": self.settings, "issues": issues}))
    }

    fn apply_settings(&mut self, old: &Settings) {
        let new = self.settings.clone();
        if new.hotkeys != old.hotkeys {
            if let Some(hook) = &self.hook {
                hook.set_bindings(Bindings::from_settings(&new.hotkeys));
            }
        }
        let source_changed =
            new.captions.source != old.captions.source || new.dictation.microphone != old.dictation.microphone;
        if source_changed && self.captions.on && self.captions.capture.is_some() {
            self.captions.capture = None;
            self.end_caption_session();
            self.start_captions_capture();
        }
        if new.services != old.services {
            for slot in &mut self.slots {
                if !slot.busy {
                    slot.release();
                }
            }
        }
        let (was, level) = (old.dictation.preconnect_level, new.dictation.preconnect_level);
        if was != Preconnect::Off && level == Preconnect::Off {
            let slot = &mut self.slots[Channel::Dictation as usize];
            if !slot.busy {
                // Shutdown releases an unused official preconnection immediately.
                slot.release();
            }
        }
        if level != was || new.services != old.services {
            self.rewarm(Instant::now());
        }
    }

    fn providers(&self) -> Value {
        let active = &self.settings.services.speech;
        let mut list = Vec::new();
        let doubao = self.spec_for(config::DOUBAO_ID);
        list.push(json!({
            "id": config::DOUBAO_ID,
            "name": "Doubao",
            "builtin": true,
            "active": active == config::DOUBAO_ID,
            "available": doubao.is_ok(),
            "error": doubao.err(),
            "capabilities": self.caps.get(config::DOUBAO_ID),
        }));
        for custom in &self.settings.services.custom_speech {
            let official = provider::is_official(custom);
            let spec = if official {
                provider::official_spec(custom, None, &self.options.search)
            } else {
                provider::custom_spec(custom, None)
            };
            let secret = official || !custom.secret_env.trim().is_empty();
            list.push(json!({
                "id": custom.id,
                "name": custom.name,
                "builtin": false,
                "active": *active == custom.id,
                "available": spec.is_ok(),
                "error": spec.err().map(|e| e.code()),
                "capabilities": self.caps.get(&custom.id),
                "official": official,
                "needsSecret": secret,
                "hasSecret": secret && secrets::has(&format!("speech.{}", custom.id)),
            }));
        }
        json!({"providers": list})
    }

    fn check_provider(&mut self, request: &Value) -> Result<Value, String> {
        let id = request["id"].as_str().map(str::to_string).unwrap_or_else(|| self.settings.services.speech.clone());
        let spec = match self.spec_for(&id) {
            Ok(spec) => spec,
            Err(code) => return Ok(json!({"started": false, "error": code})),
        };
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name("provider-check".into())
            .spawn(move || {
                let result = provider::probe(&spec, Duration::from_secs(20));
                let _ = tx.send(Msg::Probed { id, result });
            })
            .map_err(|_| "spawn_failed".to_string())?;
        Ok(json!({"started": true}))
    }

    fn set_secret(&mut self, request: &Value) -> Result<Value, String> {
        let name = request["name"].as_str().unwrap_or("");
        let value = request["value"].as_str().unwrap_or("");
        if !valid_secret_name(name) || value.is_empty() {
            return Err("invalid_secret".into());
        }
        secrets::set(name, value).map_err(|_| "secret_store_failed".to_string())?;
        self.speech_credential_changed(name);
        log::info!("secret {name} saved");
        Ok(json!({"name": name, "stored": true}))
    }

    fn delete_secret(&mut self, request: &Value) -> Result<Value, String> {
        let name = request["name"].as_str().unwrap_or("");
        if !valid_secret_name(name) {
            return Err("invalid_secret".into());
        }
        secrets::delete(name).map_err(|_| "secret_store_failed".to_string())?;
        self.speech_credential_changed(name);
        log::info!("secret {name} deleted");
        Ok(json!({"name": name, "stored": false}))
    }

    fn speech_credential_changed(&mut self, name: &str) {
        invalidate_speech_credential(&mut self.slots, name);
        if name.strip_prefix("speech.") == Some(&self.settings.services.speech) {
            // warm_up defers while dictation is active; finishing then reloads the
            // currently selected service, never its invalidated old credential.
            self.rewarm(Instant::now());
        }
    }

    fn secret_status(&self, request: &Value) -> Value {
        let mut status = serde_json::Map::new();
        for name in request["names"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if valid_secret_name(name) {
                status.insert(name.to_string(), Value::Bool(secrets::has(name)));
            }
        }
        json!({"secrets": status})
    }

    fn check_hotkey(&self, text: &str, field: &str) -> Value {
        if text.trim().is_empty() {
            return json!({"valid": true, "display": "", "conflict": null});
        }
        let Ok(binding) = Binding::parse(text) else {
            return json!({"valid": false, "conflict": null});
        };
        if binding.is_modifier_only() && !matches!(field, "hold" | "note") {
            return json!({"valid": false, "display": binding.to_string(), "conflict": "modifier_only"});
        }
        let duplicate =
            self.settings.hotkeys.fields().iter().any(|(f, v)| *f != field && Binding::parse(v).ok() == Some(binding));
        let conflict = if duplicate {
            Some("duplicate")
        } else if !binding.is_modifier_only() && input::hotkey_in_use(&binding) {
            Some("in_use")
        } else {
            None
        };
        json!({"valid": true, "display": binding.to_string(), "conflict": conflict})
    }

    fn export_diagnostics(&self) -> Result<Value, String> {
        let stamp = paths::timestamp().replace([':', '.'], "-");
        let path = self.dirs.config.join(format!("diagnostics-{stamp}.txt"));
        let stored: Vec<(String, bool)> = self
            .settings
            .services
            .custom_speech
            .iter()
            .filter(|c| provider::is_official(c) || !c.secret_env.trim().is_empty())
            .map(|c| {
                let name = format!("speech.{}", c.id);
                let has = secrets::has(&name);
                (name, has)
            })
            .collect();
        let mut out = String::new();
        out.push_str(&format!("Live Listen {VERSION}\n\n"));
        out.push_str(&format!("status: {}\n\n", serde_json::to_string_pretty(&self.status()).unwrap_or_default()));
        out.push_str(&format!("settings: {}\n\n", serde_json::to_string_pretty(&self.settings).unwrap_or_default()));
        out.push_str(&format!("stored secrets: {stored:?}\n\n--- log ---\n"));
        for line in paths::log_tail(&self.dirs, 300) {
            out.push_str(&line);
            out.push('\n');
        }
        std::fs::write(&path, out).map_err(|_| "write_failed".to_string())?;
        Ok(json!({"path": path}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_timeout_never_runs_late_commands_or_callbacks() {
        let dir = temp_dir("startup-timeout");
        let (release_tx, release_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        let events = Arc::new(Mutex::new(Vec::new()));
        let received = Arc::clone(&events);
        let result = Engine::start_initializing(
            Options { config_dir: Some(dir.clone()), ..Options::default() },
            Arc::new(move |json| received.lock().unwrap().push(json.to_owned())),
            Duration::from_millis(5),
            move |options, events, tx| {
                release_rx.recv().unwrap();
                let core = Core::new(options, events, tx.clone());
                tx.send(Msg::Call(json!({"cmd": "status"}), reply_tx)).unwrap();
                core
            },
        );
        assert!(result.is_err());
        release_tx.send(()).unwrap();
        // The late core must dispose the queued command without handling it. Entering
        // run() would emit state through an already-released FFI callback context.
        assert!(matches!(reply_rx.recv_timeout(Duration::from_secs(3)), Err(mpsc::RecvTimeoutError::Disconnected)));
        assert!(events.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn caption_errors_back_off_only_when_the_current_session_failed() {
        let dir = temp_dir("caption-backoff");
        let (tx, _rx) = mpsc::channel();
        let mut core = Core::new(Options { config_dir: Some(dir.clone()), ..Options::default() }, Arc::new(|_| {}), tx);
        core.captions.on = true;
        core.captions.open = vec![1, 2];
        core.captions.session = Some(CaptionSession::new(2, Instant::now()));
        for id in [1, 2] {
            core.captions.model.begin(id, LineSource::Captions);
        }
        core.on_message(
            Channel::Captions,
            FromProvider::Error { id: Some(1), code: "service".into(), message: String::new() },
        );
        assert!(core.captions.retry_at.is_none(), "a late older error must not stop the new session");
        assert_eq!(core.captions.session.as_ref().map(|s| s.id), Some(2));
        core.on_message(
            Channel::Captions,
            FromProvider::Error { id: Some(2), code: "service".into(), message: String::new() },
        );
        assert!(core.captions.retry_at.is_some_and(|at| at > Instant::now() + Duration::from_secs(4)));
        assert!(core.captions.session.is_none());
        core.caption_frame(AudioFrame { pcm: vec![1; audio::FRAME], level: 1.0 });
        assert!(core.slots[1].process.is_none(), "new voiced frames cannot reconnect during backoff");
        core.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn closing_captions_cancels_current_and_finalizing_provider_sessions() {
        let dir = temp_dir("caption-cancel");
        let (tx, _rx) = mpsc::channel();
        let mut core = Core::new(Options { config_dir: Some(dir.clone()), ..Options::default() }, Arc::new(|_| {}), tx);
        let script = r#"
import json, sys
for line in sys.stdin:
    event = json.loads(line)
    if event['type'] == 'cancel':
        print(json.dumps({'type':'connected','id':event['id']}), flush=True)
    elif event['type'] == 'shutdown':
        break
"#;
        let (out_tx, out_rx) = mpsc::channel();
        let process = ProviderProcess::spawn(
            &ProviderSpec {
                id: "cancel-fixture".into(),
                name: "cancel-fixture".into(),
                program: provider::find_in_path("python.exe")
                    .expect("Python 3 is required for the offline provider test"),
                args: vec!["-B".into(), "-c".into(), script.into()],
                env: Vec::new(),
                secret_env: Vec::new(),
            },
            Box::new(move |event| {
                let _ = out_tx.send(event);
            }),
        )
        .unwrap();
        core.slots[1].process = Some(process);
        core.slots[1].busy = true;
        core.captions.on = true;
        core.captions.open = vec![1, 2];
        core.captions.session = Some(CaptionSession::new(2, Instant::now()));
        core.set_captions(false);
        for id in [1, 2] {
            assert_eq!(
                out_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
                ProviderOutput::Message(FromProvider::Connected { id })
            );
        }
        assert!(core.captions.open.is_empty() && core.captions.session.is_none());
        assert!(!core.slots[1].busy);
        core.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn selected_provider_preconnect_obeys_levels_switch_and_windows() {
        // A local stdio fixture verifies the engine's real process messages without
        // credentials, microphones, global hooks or a network connection.
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/voice-refine-check/engine-tests")
            .join(format!("preconnect-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = r#"
import json, sys
print(json.dumps({'type':'hello','protocol':1,'name':'local-preconnect-test','capabilities':{}}), flush=True)
for line in sys.stdin:
    event = json.loads(line)
    with open(sys.argv[1], 'a', encoding='utf-8') as output:
        output.write(event['type'] + '\n')
    if event['type'] == 'shutdown':
        break
"#;
        let python = provider::find_in_path("python.exe").expect("Python 3 is required for the offline provider test");
        let mut settings = Settings::default();
        settings.dictation.preconnect_level = Preconnect::Off;
        settings.services.speech = "local-a".into();
        for id in ["local-a", "local-b"] {
            settings.services.custom_speech.push(config::CustomSpeechProvider {
                id: id.into(),
                name: id.into(),
                command: python.to_string_lossy().into_owned(),
                args: vec!["-B".into(), "-c".into(), script.into(), dir.join(id).to_string_lossy().into_owned()],
                ..Default::default()
            });
        }
        settings.save(&dir.join("settings.json")).unwrap();
        let (tx, _rx) = mpsc::channel();
        let mut core = Core::new(
            Options { config_dir: Some(dir.clone()), hooks: false, ..Options::default() },
            Arc::new(|_| {}),
            tx,
        );
        let wait_message = |id: &str, message: &str, count: usize| {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let contents = std::fs::read_to_string(dir.join(id)).unwrap_or_default();
                if contents.lines().filter(|line| *line == message).count() >= count {
                    return;
                }
                assert!(Instant::now() < deadline, "fixture did not receive {message}");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        core.warm_up();
        assert!(core.slots[0].process.is_none(), "disabled preconnect must not start a provider");
        core.warm_until = Instant::now() - Duration::from_secs(1);
        let old = core.settings.clone();
        core.settings.dictation.preconnect_level = Preconnect::Aggressive;
        core.apply_settings(&old);
        wait_message("local-a", "prepare", 1);
        assert!(core.warm_until > Instant::now() + Duration::from_secs(590));

        let old = core.settings.clone();
        core.settings.services.speech = "local-b".into();
        core.apply_settings(&old);
        wait_message("local-a", "shutdown", 1);
        wait_message("local-b", "prepare", 1);
        assert_eq!(core.slots[0].provider_id, "local-b");

        let old = core.settings.clone();
        core.settings.dictation.preconnect_level = Preconnect::Off;
        core.apply_settings(&old);
        wait_message("local-b", "shutdown", 1);
        assert!(core.slots[0].process.is_none());

        let old = core.settings.clone();
        core.settings.dictation.preconnect_level = Preconnect::Aggressive;
        core.apply_settings(&old);
        wait_message("local-b", "prepare", 2);
        core.warm_until = Instant::now() - Duration::from_secs(1);
        core.speech_credential_changed("speech.local-b");
        wait_message("local-b", "shutdown", 2);
        wait_message("local-b", "prepare", 3);
        assert!(core.warm_until > Instant::now() + Duration::from_secs(590));
        core.speech_credential_changed("speech.local-a");
        assert_eq!(core.slots[0].provider_id, "local-b", "unselected credentials cannot warm the old service");
        core.warm_until = Instant::now() - Duration::from_secs(1);
        core.tick(Instant::now());
        wait_message("local-b", "shutdown", 3);
        assert!(core.slots[0].process.is_none(), "the ten-minute window releases its idle connection");
        core.on_signal(Signal::HoldArmed);
        core.tick(Instant::now());
        wait_message("local-b", "prepare", 4);
        assert!(core.slots[0].process.is_some(), "an armed key must survive the tick before HoldStart");
        assert!(core.warm_until > Instant::now() + Duration::from_secs(590));
        let old = core.settings.clone();
        core.settings.dictation.preconnect_level = Preconnect::Balanced;
        core.apply_settings(&old);
        wait_message("local-b", "prepare", 5);
        let left = core.warm_until.saturating_duration_since(Instant::now());
        assert!(left > Duration::from_secs(110) && left <= Duration::from_secs(120), "balanced keeps two minutes");
        let old = core.settings.clone();
        core.settings.dictation.preconnect_level = Preconnect::Off;
        core.apply_settings(&old);
        wait_message("local-b", "shutdown", 4);
        core.on_signal(Signal::HoldArmed);
        assert!(core.slots[0].process.is_none(), "HoldArmed also respects disabled preconnect");
        core.shutdown();
    }

    #[test]
    fn start_command_keeps_existing_dictation() {
        // A fresh workspace-local settings folder explicitly disables preconnect.
        // No microphone, provider, global hook or credential store is used.
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/voice-custom-check/engine-tests");
        std::fs::create_dir_all(&parent).unwrap();
        let dir = parent.join(format!("start-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let mut settings = Settings::default();
        settings.dictation.preconnect_level = Preconnect::Off;
        settings.save(&dir.join("settings.json")).unwrap();
        let (tx, _rx) = mpsc::channel();
        let mut core =
            Core::new(Options { config_dir: Some(dir), hooks: false, ..Options::default() }, Arc::new(|_| {}), tx);
        let now = Instant::now();
        core.dictation = Some(Dictation {
            session: 42,
            mirror_id: 42,
            trigger: Trigger::Toggle,
            purpose: Purpose::Type,
            phase: Phase::Listening,
            target: None,
            element: None,
            blocked: None,
            paused: false,
            writer: Writer::new(crate::config::WriteMode::Stable),
            behind: false,
            base: String::new(),
            text: "keep existing text".into(),
            stable: 0,
            capture: None,
            capture_id: 0,
            mic_ms: None,
            floor: NoiseFloor::default(),
            element_checked: None,
            started: now,
            first_text_ms: None,
            deadline: None,
            replay: false,
            timed_out: false,
            mirrored: false,
            audio: Vec::new(),
            retried: false,
            next: None,
            part_started: now,
            last_voice: now,
            changed: now,
            stopped_at: None,
            resume_input: None,
            input_revision: 0,
            final_wait: false,
            connected_ms: None,
            samples: 0,
            voiced: 0,
            peak: 0.0,
        });
        for phase in [Phase::Connecting, Phase::Listening, Phase::Finishing] {
            core.dictation.as_mut().unwrap().phase = phase;
            for _ in 0..3 {
                assert!(core.core_call("dictation.start", &json!({})).is_ok());
                let d = core.dictation.as_ref().unwrap();
                assert_eq!(d.phase, phase, "start must not stop or restart the current session");
                assert_eq!(d.session, 42);
                assert_eq!(d.text, "keep existing text");
                assert_eq!(d.trigger, Trigger::Toggle);
                assert!(d.stopped_at.is_none());
            }
        }
        assert_eq!(core.next_id, 0, "no new provider session or capture was requested");
        core.shutdown();
    }

    #[test]
    fn long_input_yields_after_one_bounded_step() {
        let mut writer = Writer::new(crate::config::WriteMode::Live);
        writer.typed("旧".repeat(300).as_str());
        let ops = writer.finish("新文本");
        let mut calls = Vec::new();
        assert_eq!(
            apply_step(&ops, &mut writer, TYPE_STEP, |op| {
                calls.push(op.clone());
                1
            }),
            Applied::Behind
        );
        assert_eq!(calls, vec![WriteOp::Backspace(1)]);
        assert_eq!(writer.written().chars().count(), 299);

        let mut writer = Writer::new(crate::config::WriteMode::Stable);
        let wanted = "这是一段很长的输入🙂".repeat(20);
        let ops = writer.finish(&wanted);
        let result = apply_step(&ops, &mut writer, 1, |op| {
            assert!(matches!(op, WriteOp::Type(text) if text.chars().count() == 1));
            1
        });
        assert_eq!(result, Applied::Behind, "Qt gets one character per turn");
        assert_eq!(writer.unwritten(&wanted).chars().count(), wanted.chars().count() - 1);
    }

    #[test]
    fn failed_input_step_keeps_unwritten_text_recoverable() {
        let mut writer = Writer::new(crate::config::WriteMode::Stable);
        let ops = writer.finish("完整内容");
        assert_eq!(apply_step(&ops, &mut writer, TYPE_STEP, |_| 0), Applied::Failed);
        assert_eq!(writer.unwritten("完整内容"), "完整内容");
        assert_eq!(apply_step(&ops, &mut writer, TYPE_STEP, |_| 2), Applied::Failed);
        assert_eq!(writer.written(), "完整");
        assert_eq!(writer.unwritten("完整内容"), "内容");
    }

    fn engine(dir: &Path) -> (Engine, Arc<Mutex<Vec<Value>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink_events = Arc::clone(&events);
        let sink: EventSink =
            Arc::new(move |e: &str| sink_events.lock().unwrap().push(serde_json::from_str(e).unwrap()));
        let options = Options { config_dir: Some(dir.to_path_buf()), hooks: false, ..Options::default() };
        (Engine::start(options, sink).unwrap(), events)
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("live-listen-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn call(engine: &Engine, request: Value) -> Value {
        serde_json::from_str(&engine.call(&request.to_string())).unwrap()
    }

    #[test]
    fn settings_round_trip_and_validation() {
        let dir = temp_dir("settings");
        let (engine, _) = engine(&dir);
        let got = call(&engine, json!({"cmd": "settings.get"}));
        assert_eq!(got["ok"], true);
        let mut settings = got["settings"].clone();
        settings["hotkeys"]["toggle"] = json!("Ctrl+Alt+Nope");
        let saved = call(&engine, json!({"cmd": "settings.set", "settings": settings}));
        assert_eq!(saved["ok"], true);
        assert_eq!(saved["issues"][0]["field"], "hotkeys.toggle");
        assert!(dir.join("settings.json").is_file());
        assert_eq!(call(&engine, json!({"cmd": "nope"}))["error"], "unknown_command");
        assert_eq!(engine.call("{not json"), error_reply("bad_request").to_string());
        drop(engine);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shutdown_command_disconnects_later_calls() {
        let dir = temp_dir("shutdown");
        let (engine, _) = engine(&dir);
        assert_eq!(call(&engine, json!({"cmd": "cards.add", "text": "退出前保存"}))["ok"], true);
        assert_eq!(call(&engine, json!({"cmd": "shutdown"}))["ok"], true);
        assert_eq!(call(&engine, json!({"cmd": "cards.add", "text": "退出后丢弃"}))["error"], "stopped");
        assert_eq!(call(&engine, json!({"cmd": "shutdown"}))["error"], "stopped");
        drop(engine);
        assert_eq!(crate::cards::CardBook::load(&dir.join("cards.json")).cards.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hotkey_check_reports_duplicates_and_modifier_only() {
        let dir = temp_dir("hotkey");
        let (engine, _) = engine(&dir);
        let dup = call(&engine, json!({"cmd": "hotkey.check", "binding": "ctrl+alt+c", "field": "toggle"}));
        assert_eq!(dup["conflict"], "duplicate");
        assert_eq!(dup["display"], "Ctrl+Alt+C");
        let lone = call(&engine, json!({"cmd": "hotkey.check", "binding": "RAlt", "field": "toggle"}));
        assert_eq!(lone["conflict"], "modifier_only");
        let hold = call(&engine, json!({"cmd": "hotkey.check", "binding": "Ctrl+Right", "field": "hold"}));
        assert_eq!(hold["valid"], true);
        assert_eq!(hold["conflict"], "duplicate");
        let note = call(&engine, json!({"cmd": "hotkey.check", "binding": "rightalt", "field": "note"}));
        assert_eq!(note["valid"], true);
        assert_eq!(note["display"], "RAlt");
        assert!(note["conflict"].is_null());
        assert_eq!(call(&engine, json!({"cmd": "hotkey.record"}))["error"], "hook_unavailable");
        for request in [
            json!({"cmd": "hotkey.capture"}),
            json!({"cmd": "hotkey.capture", "on": true, "window": 0}),
            json!({"cmd": "hotkey.capture", "on": false}),
        ] {
            assert_eq!(call(&engine, request)["error"], "capture_invalid_request");
        }
        assert_eq!(
            call(&engine, json!({"cmd": "hotkey.capture", "on": true, "window": 42}))["error"],
            "capture_unavailable"
        );
        drop(engine);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn card_commands_persist_across_restarts() {
        let dir = temp_dir("cards");
        let (app, events) = engine(&dir);
        let added = call(&app, json!({"cmd": "cards.add", "text": "买牛奶", "kind": "todo"}));
        let id = added["id"].as_u64().unwrap();
        let moved = call(&app, json!({"cmd": "cards.update", "id": id, "patch": {"x": 40, "y": 50}}));
        assert_eq!(moved["ok"], true);
        call(&app, json!({"cmd": "cards.update", "id": id, "patch": {"done": true}}));
        call(&app, json!({"cmd": "cards.show", "on": false}));
        let changes = events.lock().unwrap().iter().filter(|e| e["event"] == "cards.changed").count();
        assert_eq!(changes, 3, "moving a card does not broadcast");
        drop(app);
        let (app, _) = engine(&dir);
        let list = call(&app, json!({"cmd": "cards.list"}));
        assert_eq!(list["showOnDesktop"], false);
        let card = &list["cards"][0];
        assert_eq!(
            (card["kind"].as_str(), card["done"].as_bool(), card["x"].as_i64()),
            (Some("todo"), Some(true), Some(40))
        );
        assert_eq!(call(&app, json!({"cmd": "cards.delete", "id": id}))["ok"], true);
        assert_eq!(call(&app, json!({"cmd": "cards.delete", "id": id}))["error"], "unknown_card");
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn noise_floor_ignores_steady_background_but_keeps_speech() {
        let mut quiet = NoiseFloor::default();
        for _ in 0..FLOOR_BUCKET_FRAMES * 3 {
            assert!(!quiet.voiced(0.0));
        }
        assert_eq!(quiet.threshold(), VOICE_LEVEL, "a silent room keeps the fixed level");
        assert!(quiet.voiced(0.25));

        // A fan at -46 dBFS (0.28) counted as voice with the fixed level.
        let mut noisy = NoiseFloor::default();
        assert!(noisy.voiced(0.28), "the first second still uses the fixed level");
        for _ in 0..FLOOR_BUCKET_FRAMES * 2 {
            noisy.voiced(0.28);
        }
        assert!(!noisy.voiced(0.3), "steady noise is not voice");
        assert!(noisy.voiced(0.6), "speech at -30 dBFS is");
        // Speech has gaps between words: the floor stays at the background.
        for i in 0..FLOOR_BUCKET_FRAMES * 6 {
            noisy.voiced(if i % 5 == 0 { 0.28 } else { 0.7 });
        }
        assert!((noisy.threshold() - 0.4).abs() < 1e-6, "{}", noisy.threshold());

        // Loud continuous sound never raises the bar above -40 dBFS.
        let mut music = NoiseFloor::default();
        for _ in 0..FLOOR_BUCKET_FRAMES * 6 {
            music.voiced(0.8);
        }
        assert_eq!(music.threshold(), VOICE_LEVEL_MAX);
        assert!(music.voiced(0.45), "dialog over music still counts");
    }

    #[test]
    fn preconnect_levels_differ_at_start_up_and_pause_while_the_user_is_away() {
        assert_eq!(WarmPolicy::of(Preconnect::Off), None);
        for level in [Preconnect::Aggressive, Preconnect::Balanced] {
            let dir = temp_dir(&format!("preconnect-away-{level:?}"));
            let mut settings = Settings::default();
            settings.dictation.preconnect_level = level;
            settings.save(&dir.join("settings.json")).unwrap();
            let (tx, _rx) = mpsc::channel();
            let mut core = Core::new(
                Options { config_dir: Some(dir.clone()), hooks: false, ..Options::default() },
                Arc::new(|_| {}),
                tx,
            );
            let aggressive = level == Preconnect::Aggressive;
            let now = Instant::now();
            core.user_idle = || Duration::from_secs(5);
            assert_eq!(core.keeps_warm(now), aggressive, "only aggressive keeps a connection after start-up");
            core.warm_until = now + WarmPolicy::of(level).unwrap().window; // as after a dictation
            assert!(core.keeps_warm(now));
            core.user_idle = || Duration::from_secs(90);
            assert_eq!(core.keeps_warm(now), aggressive, "balanced pauses after one idle minute");
            core.user_idle = || Duration::from_secs(3 * 60 + 1);
            assert!(!core.keeps_warm(now), "nobody at the computer");
            core.tick(now);
            assert!(core.warm_paused);
            core.user_idle = || Duration::from_millis(10);
            assert!(core.keeps_warm(now), "the next input resumes it");
            core.tick(now);
            assert!(!core.warm_paused);
            core.warm_until = now - Duration::from_secs(1);
            assert!(!core.keeps_warm(now), "the window still ends");
            core.shutdown();
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn microphone_failure_after_start_ends_dictation_with_its_error() {
        // A missing test-audio file fails on the audio thread after start_dictation returned.
        let dir = temp_dir("capture-failure");
        let script = r#"
import json, sys
print(json.dumps({'type':'hello','protocol':1,'name':'capture-test','capabilities':{}}), flush=True)
for line in sys.stdin:
    if json.loads(line)['type'] == 'shutdown':
        break
"#;
        let python = provider::find_in_path("python.exe").expect("Python 3 is required for the offline provider test");
        let mut settings = Settings::default();
        settings.dictation.preconnect_level = Preconnect::Off;
        settings.services.speech = "local".into();
        settings.services.custom_speech.push(config::CustomSpeechProvider {
            id: "local".into(),
            name: "local".into(),
            command: python.to_string_lossy().into_owned(),
            args: vec!["-B".into(), "-c".into(), script.into()],
            ..Default::default()
        });
        settings.save(&dir.join("settings.json")).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink_events = Arc::clone(&events);
        let (tx, rx) = mpsc::channel();
        let mut core = Core::new(
            Options {
                config_dir: Some(dir.clone()),
                hooks: false,
                dictation_audio: Some(dir.join("missing.wav")),
                ..Options::default()
            },
            Arc::new(move |e: &str| sink_events.lock().unwrap().push(serde_json::from_str::<Value>(e).unwrap())),
            tx,
        );
        core.start_dictation(Trigger::Toggle, Purpose::Note);
        assert!(core.dictation.as_ref().is_some_and(|d| d.phase == Phase::Connecting), "state shows at once");
        let deadline = Instant::now() + Duration::from_secs(5);
        while core.dictation.is_some() && Instant::now() < deadline {
            if let Ok(msg) = rx.recv_timeout(Duration::from_millis(100)) {
                core.handle(msg);
            }
        }
        assert!(core.dictation.is_none());
        assert!(!core.slots[0].busy);
        let got = events.lock().unwrap();
        assert!(got.iter().any(|e| e["event"] == "error" && e["code"] == "audio_file"));
        assert!(got.iter().any(|e| e["event"] == "dictation.done" && e["reason"] == "error"));
        assert!(!got.iter().any(|e| e["event"] == "notice" && e["code"] == "note_empty"));
        drop(got);
        core.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn dictation_splits_only_at_real_pauses() {
        let ms = Duration::from_millis;
        assert!(is_pause(ms(5_000), ms(1_000), ms(400), true));
        assert!(!is_pause(ms(5_000), ms(1_000), ms(400), false), "nothing new to confirm");
        assert!(!is_pause(ms(3_000), ms(1_000), ms(400), true), "too soon after the last split");
        assert!(!is_pause(ms(5_000), ms(500), ms(400), true), "text still changing");
        assert!(!is_pause(ms(5_000), ms(1_000), ms(100), true), "still speaking");
    }

    #[test]
    fn caption_sessions_end_at_pauses_in_long_speech() {
        let t0 = Instant::now();
        let mut s = CaptionSession::new(1, t0);
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        s.last_voice = at(9_000);
        assert_eq!(s.ending(at(9_000)), None, "no text yet");
        s.text = "我们开始".into();
        s.changed = at(8_500);
        assert_eq!(s.ending(at(9_000)), None, "text changed 0.5 s ago");
        assert_eq!(s.ending(at(9_500)), Some(true), "paused after 8 s");
        s.changed = at(3_000);
        assert_eq!(s.ending(at(5_000)), None, "too early to cut");
        s.last_voice = at(1_000);
        assert_eq!(s.ending(at(7_000)), Some(false), "silence");
        let mut long = CaptionSession::new(2, t0);
        long.text = "一直在说".into();
        long.last_voice = at(60_000);
        long.changed = at(60_000);
        assert_eq!(long.ending(at(60_000)), Some(true), "longest session");
    }

    #[test]
    fn secret_names_are_restricted() {
        assert!(!valid_secret_name("translation"), "the translation key of the removed feature");
        assert!(!valid_secret_name("summary"));
        assert!(valid_secret_name("speech.my-asr"));
        assert!(!valid_secret_name("speech."));
        assert!(!valid_secret_name("speech.a/b"));
        assert!(!valid_secret_name("other"));
    }

    #[test]
    fn credential_changes_invalidate_only_matching_service_without_ending_active_session() {
        let mut slots = [Slot::new(Channel::Dictation), Slot::new(Channel::Captions)];
        slots[0].provider_id = "official-one".into();
        slots[0].key = "cached-one".into();
        slots[0].busy = true;
        slots[0].generation = 7;
        slots[1].provider_id = "legacy-two".into();
        slots[1].key = "cached-two".into();
        invalidate_speech_credential(&mut slots, "other.official-one");
        assert_eq!(slots[0].key, "cached-one");
        invalidate_speech_credential(&mut slots, "speech.official-one");
        assert!(slots[0].key.is_empty(), "next ensure_provider must reload its credential");
        assert!(slots[0].busy, "the active session is allowed to finish");
        assert_eq!(slots[0].generation, 7, "active provider messages retain their generation");
        assert_eq!(slots[1].key, "cached-two", "unrelated providers remain cached");
        invalidate_speech_credential(&mut slots, "speech.legacy-two");
        assert!(slots[1].key.is_empty(), "legacy secretEnv providers also reload changed credentials");
    }
}
