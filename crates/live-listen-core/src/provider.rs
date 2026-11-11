//! Speech provider processes: locating the built-in Doubao bridge, spawning providers and
//! exchanging protocol messages with them.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::config::{CustomSpeechProvider, DoubaoSettings};
use crate::protocol::{Capabilities, FromProvider, ToProvider};

/// Lines longer than this are dropped; providers send short JSON objects.
const MAX_LINE: usize = 1 << 20;
const STDERR_LINES: usize = 20;
const STDERR_LINE_BYTES: usize = 1200; // At most 300 Unicode scalar values below.
/// Audio messages queued for a provider that is not reading; audio beyond this is dropped.
/// Room for a full reconnect replay (90 s of 40 ms frames) plus live audio. Control
/// messages (`start`, `stop`, `cancel`, ...) are never dropped.
pub const AUDIO_BACKLOG: usize = 4096;
/// Longest wait for the provider's reader and writer threads after it was asked to exit.
const THREAD_WAIT: Duration = Duration::from_secs(2);
/// Lack of stdin progress indicates a stuck process, not a slow recognition response.
const WRITE_STALL: Duration = Duration::from_secs(3);

/// How to start one provider process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSpec {
    pub id: String,
    pub name: String,
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(String, String)>,
    /// Names of environment variables that carry secrets (values are never logged).
    pub secret_env: Vec<String>,
}

impl ProviderSpec {
    /// Changes when the process must be restarted.
    pub fn key(&self) -> String {
        let mut key = format!("{}|{}", self.id, self.program.display());
        for a in &self.args {
            key.push('|');
            key.push_str(&a.to_string_lossy());
        }
        for (k, v) in &self.env {
            key.push('|');
            key.push_str(k);
            if !self.secret_env.contains(k) {
                key.push('=');
                key.push_str(v);
            }
        }
        key
    }
}

/// Why a provider cannot be started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecError {
    PythonMissing,
    ModuleMissing,
    BridgeMissing,
    CommandMissing,
    OfficialConfigInvalid,
}

impl SpecError {
    pub fn code(self) -> &'static str {
        match self {
            SpecError::PythonMissing => "python_missing",
            SpecError::ModuleMissing => "module_missing",
            SpecError::BridgeMissing => "bridge_missing",
            SpecError::CommandMissing => "command_missing",
            SpecError::OfficialConfigInvalid => "official_config_invalid",
        }
    }
}

fn ancestors_containing(starts: &[PathBuf], relative: &Path) -> Option<PathBuf> {
    for start in starts {
        for dir in start.ancestors() {
            // A drive root is writable by other users; never run code found there.
            if dir.parent().is_none() {
                break;
            }
            if dir.join(relative).is_file() {
                return Some(dir.to_path_buf());
            }
        }
    }
    None
}

/// Environment that makes a speech service process use the proxy chosen in the settings.
/// The bundled bridges connect with `websocket-client` and `urllib`, which read these
/// variables. `system` follows the manual proxy of the Windows settings (a setup script is
/// not evaluated); without one the process keeps the environment it inherits.
pub fn proxy_env(mode: &str, url: &str) -> Vec<(String, String)> {
    match mode {
        "direct" => vec![("NO_PROXY".into(), "*".into())],
        "custom" => http_proxy(url).map(|proxy| proxy_vars(&proxy, LOCAL_BYPASS)).unwrap_or_default(),
        _ => system_proxy().map(|(proxy, bypass)| proxy_vars(&proxy, &bypass)).unwrap_or_default(),
    }
}

const LOCAL_BYPASS: &str = "localhost,127.0.0.1";

fn proxy_vars(proxy: &str, bypass: &str) -> Vec<(String, String)> {
    vec![
        ("HTTP_PROXY".into(), proxy.to_string()),
        ("HTTPS_PROXY".into(), proxy.to_string()),
        ("NO_PROXY".into(), bypass.to_string()),
    ]
}

/// An HTTP proxy address without credentials, or `None`.
fn http_proxy(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let host = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://"))?;
    let usable = !host.is_empty() && !host.contains(|c: char| c.is_whitespace() || "@/?#".contains(c));
    usable.then(|| url.to_string())
}

/// Reads the `ProxyServer` and `ProxyOverride` values of the Windows proxy settings.
/// `ProxyServer` is `host:port` or a list such as `http=a:1;https=b:2;socks=c:3`.
fn parse_system_proxy(server: &str, overrides: &str) -> Option<(String, String)> {
    let server = server.trim();
    let address = if server.contains('=') {
        let entry = |scheme: &str| {
            server.split(';').find_map(|part| {
                let (name, value) = part.trim().split_once('=')?;
                name.trim().eq_ignore_ascii_case(scheme).then(|| value.trim().to_string())
            })
        };
        entry("https").or_else(|| entry("http"))?
    } else {
        server.to_string()
    };
    let address = if address.contains("://") { address } else { format!("http://{address}") };
    let proxy = http_proxy(&address)?;
    let mut bypass: Vec<String> = LOCAL_BYPASS.split(',').map(str::to_string).collect();
    for host in overrides.split(';').map(str::trim).filter(|host| !host.is_empty() && *host != "<local>") {
        // `*.example.com` is `.example.com` for the bridges; other patterns are passed as they are.
        let host = host.strip_prefix('*').filter(|rest| rest.starts_with('.')).unwrap_or(host).to_string();
        if !bypass.contains(&host) {
            bypass.push(host);
        }
    }
    Some((proxy, bypass.join(",")))
}

/// The manual proxy of the current user's Windows settings, with its bypass list.
fn system_proxy() -> Option<(String, String)> {
    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    if registry_dword(KEY, "ProxyEnable")? == 0 {
        return None;
    }
    parse_system_proxy(
        &registry_string(KEY, "ProxyServer")?,
        &registry_string(KEY, "ProxyOverride").unwrap_or_default(),
    )
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn registry_dword(key: &str, name: &str) -> Option<u32> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::PCWSTR;
    let (key, name) = (wide(key), wide(name));
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: both names are NUL-terminated and the buffer is four bytes, as `size` says.
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(std::ptr::from_mut(&mut value).cast()),
            Some(&mut size),
        )
    };
    result.is_ok().then_some(value)
}

fn registry_string(key: &str, name: &str) -> Option<String> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::PCWSTR;
    let (key, name) = (wide(key), wide(name));
    let mut buffer = vec![0u16; 2048];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: both names are NUL-terminated and `size` is the buffer's length in bytes.
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if result.is_err() {
        return None;
    }
    let length = (size as usize / 2).min(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..length]);
    Some(text.trim_end_matches('\0').to_string())
}

/// Finds an executable on PATH, skipping the Microsoft Store app-execution aliases.
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| !dir.to_string_lossy().to_ascii_lowercase().contains("windowsapps"))
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Prefer the application's self-contained runtime without changing PATH. The
/// caller handles any explicitly configured interpreter before reaching here.
fn python_command(search: &[PathBuf]) -> Result<(PathBuf, Vec<OsString>), SpecError> {
    let bundled = Path::new("runtime").join("python").join("python.exe");
    if let Some(root) = ancestors_containing(search, &bundled) {
        return Ok((root.join(bundled), Vec::new()));
    }
    match find_in_path("python.exe") {
        Some(python) => Ok((python, Vec::new())),
        None => Ok((find_in_path("py.exe").ok_or(SpecError::PythonMissing)?, vec![OsString::from("-3")])),
    }
}

/// Describes the Doubao bridge: `python -B providers/doubao_live.py`, with the folder that
/// contains `doubao_asr` passed through `LIVE_LISTEN_DOUBAO_ROOT`.
pub fn doubao_spec(settings: &DoubaoSettings, search: &[PathBuf]) -> Result<ProviderSpec, SpecError> {
    let module = Path::new("doubao_asr").join("doubao.py");
    let root = if settings.module_root.trim().is_empty() {
        let mut starts = search.to_vec();
        if let Some(env) = std::env::var_os("LIVE_LISTEN_DOUBAO_ROOT") {
            starts.insert(0, PathBuf::from(env));
        }
        ancestors_containing(&starts, &module).ok_or(SpecError::ModuleMissing)?
    } else {
        let root = PathBuf::from(settings.module_root.trim());
        if !root.join(&module).is_file() {
            return Err(SpecError::ModuleMissing);
        }
        root
    };
    let bridge_rel = Path::new("providers").join("doubao_live.py");
    let bridge_dir = ancestors_containing(search, &bridge_rel)
        .or_else(|| ancestors_containing(&[root.join("live-listen")], &bridge_rel))
        .ok_or(SpecError::BridgeMissing)?;
    let (program, mut args) = if settings.python.trim().is_empty() {
        python_command(search)?
    } else {
        let p = PathBuf::from(settings.python.trim());
        if !p.is_file() {
            return Err(SpecError::PythonMissing);
        }
        (p, Vec::new())
    };
    args.push("-B".into());
    args.push(bridge_dir.join(bridge_rel).into_os_string());
    Ok(ProviderSpec {
        id: crate::config::DOUBAO_ID.into(),
        name: "Doubao".into(),
        program,
        args,
        env: vec![
            ("LIVE_LISTEN_DOUBAO_ROOT".into(), root.display().to_string()),
            ("PYTHONIOENCODING".into(), "utf-8".into()),
            ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
        ],
        secret_env: Vec::new(),
    })
}

/// Official services use the bundled bridge; credentials never appear in argv or config.
pub fn official_spec(
    custom: &CustomSpeechProvider,
    secret: Option<String>,
    search: &[PathBuf],
) -> Result<ProviderSpec, SpecError> {
    if !matches!(custom.provider_type.as_str(), "doubao" | "openai" | "qwen" | "tencent") {
        return Err(SpecError::OfficialConfigInvalid);
    }
    let relative = Path::new("providers").join("official_live.py");
    let folder = ancestors_containing(search, &relative).ok_or(SpecError::BridgeMissing)?;
    let (program, mut args) = python_command(search)?;
    args.push("-B".into());
    args.push(folder.join(relative).into_os_string());
    let config = serde_json::json!({
        "providerType": custom.provider_type, "endpoint": custom.endpoint,
        "model": custom.model, "appId": custom.app_id,
        "resourceId": custom.resource_id, "language": custom.language,
    });
    let mut env = vec![
        ("LIVE_LISTEN_OFFICIAL_CONFIG".into(), config.to_string()),
        ("PYTHONIOENCODING".into(), "utf-8".into()),
        ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
    ];
    if let Some(value) = secret {
        env.push(("LIVE_LISTEN_OFFICIAL_CREDENTIAL".into(), value));
    }
    Ok(ProviderSpec {
        id: custom.id.clone(),
        name: custom.name.clone(),
        program,
        args,
        env,
        secret_env: vec!["LIVE_LISTEN_OFFICIAL_CREDENTIAL".into()],
    })
}

pub fn is_official(custom: &CustomSpeechProvider) -> bool {
    !custom.provider_type.is_empty() && custom.provider_type != "command"
}

/// Describes a user-added provider. The secret, if any, is passed in an environment variable.
pub fn custom_spec(custom: &CustomSpeechProvider, secret: Option<String>) -> Result<ProviderSpec, SpecError> {
    let command = custom.command.trim();
    if command.is_empty() {
        return Err(SpecError::CommandMissing);
    }
    let path = PathBuf::from(command);
    let program = if path.is_absolute() {
        path.is_file().then_some(path).ok_or(SpecError::CommandMissing)?
    } else {
        find_in_path(command).or_else(|| find_in_path(&format!("{command}.exe"))).ok_or(SpecError::CommandMissing)?
    };
    let mut env = Vec::new();
    let mut secret_env = Vec::new();
    let name = custom.secret_env.trim();
    if let (false, Some(value)) = (name.is_empty(), secret) {
        env.push((name.to_string(), value));
        secret_env.push(name.to_string());
    }
    Ok(ProviderSpec {
        id: custom.id.clone(),
        name: custom.name.clone(),
        program,
        args: custom.args.iter().map(OsString::from).collect(),
        env,
        secret_env,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderOutput {
    Message(FromProvider),
    /// A line that is not a protocol message (counted, not shown).
    Malformed,
    /// The process closed its output; `stderr` holds its last lines for the log.
    Exited {
        stderr: Vec<String>,
    },
}

pub type OutputSink = Box<dyn Fn(ProviderOutput) + Send + Sync>;

/// A running provider process. Input is written by a separate thread so a stalled
/// provider never blocks the caller.
pub struct ProviderProcess {
    child: Child,
    stdin: Arc<InputQueue>,
    writer: Option<JoinHandle<()>>,
    reader: Option<JoinHandle<()>>,
}

struct InputLine {
    bytes: Vec<u8>,
    audio: Option<u64>,
}

struct QueueState {
    lines: VecDeque<InputLine>,
    audio: usize,
    closed: bool,
    writing: bool,
    progress: Instant,
}

/// One ordered queue; cancel removes only unsent audio of its own session. Stop remains
/// behind the audio it finalizes. The lock is never held while writing to the pipe.
struct InputQueue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

impl InputQueue {
    fn new() -> InputQueue {
        InputQueue {
            state: Mutex::new(QueueState {
                lines: VecDeque::new(),
                audio: 0,
                closed: false,
                writing: false,
                progress: Instant::now(),
            }),
            ready: Condvar::new(),
        }
    }

    fn send(&self, message: &ToProvider) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(message)?;
        bytes.push(b'\n');
        let audio = if let ToProvider::Audio { id, .. } = message { Some(*id) } else { None };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if audio.is_some() && state.audio >= AUDIO_BACKLOG {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if let ToProvider::Cancel { id } = message {
            state.lines.retain(|line| line.audio != Some(*id));
            state.audio = state.lines.iter().filter(|line| line.audio.is_some()).count();
        }
        if !state.writing && state.lines.is_empty() {
            state.progress = Instant::now();
        }
        state.audio += usize::from(audio.is_some());
        state.lines.push_back(InputLine { bytes, audio });
        self.ready.notify_one();
        Ok(())
    }

    fn next(&self) -> Option<InputLine> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while state.lines.is_empty() && !state.closed {
            state = self.ready.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        let line = state.lines.pop_front()?;
        state.audio -= usize::from(line.audio.is_some());
        state.writing = true;
        Some(line)
    }

    fn written(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.writing = false;
        state.progress = Instant::now();
    }

    fn stalled(&self, now: Instant) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed
            || ((state.writing || !state.lines.is_empty())
                && now.saturating_duration_since(state.progress) >= WRITE_STALL)
    }

    fn close(&self, discard: bool) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        if discard {
            state.lines.clear();
            state.audio = 0;
        }
        self.ready.notify_all();
    }
}

impl ProviderProcess {
    pub fn spawn(spec: &ProviderSpec, sink: OutputSink) -> io::Result<ProviderProcess> {
        let mut command = Command::new(&spec.program);
        command.args(&spec.args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if spec.env.iter().any(|(k, _)| k == "NO_PROXY") {
            // The settings decide the proxy: one inherited from the environment must not win.
            for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
                command.env_remove(name);
            }
        }
        for (k, v) in &spec.env {
            command.env(k, v);
        }
        if let Some(dir) = spec.program.parent() {
            if dir.is_dir() && spec.id != crate::config::DOUBAO_ID {
                command.current_dir(dir);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().ok_or_else(|| io::Error::other("no stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| io::Error::other("no stderr"))?;
        let stdin = child.stdin.take().ok_or_else(|| io::Error::other("no stdin"))?;
        let input = Arc::new(InputQueue::new());
        let writer_input = Arc::clone(&input);
        let writer = std::thread::Builder::new().name("provider-stdin".into()).spawn(move || {
            let mut out = BufWriter::new(stdin);
            while let Some(line) = writer_input.next() {
                if out.write_all(&line.bytes).and_then(|()| out.flush()).is_err() {
                    writer_input.close(true);
                    break;
                }
                writer_input.written();
            }
        })?;
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let err_tail = Arc::clone(&tail);
        let err_thread = std::thread::Builder::new().name("provider-stderr".into()).spawn(move || {
            read_bounded_lines(stderr, STDERR_LINE_BYTES, |line, _overflow| {
                let mut t = err_tail.lock().unwrap_or_else(|e| e.into_inner());
                if t.len() == STDERR_LINES {
                    t.pop_front();
                }
                t.push_back(String::from_utf8_lossy(line).chars().take(300).collect::<String>());
            });
        })?;
        let reader = std::thread::Builder::new().name("provider-stdout".into()).spawn(move || {
            read_lines(stdout, &sink);
            let _ = err_thread.join();
            let stderr: Vec<String> = tail.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect();
            sink(ProviderOutput::Exited { stderr });
        })?;
        Ok(ProviderProcess { child, stdin: input, writer: Some(writer), reader: Some(reader) })
    }

    /// Queues one message. Audio fails with `WouldBlock` while [`AUDIO_BACKLOG`] audio messages
    /// are still waiting (the provider stopped reading); other messages are always queued.
    /// Fails with `BrokenPipe` once the provider is gone.
    pub fn send(&mut self, message: &ToProvider) -> io::Result<()> {
        self.stdin.send(message)
    }

    /// Checked by the engine even after capture stopped, so a blocked pipe cannot hold the
    /// next dictation forever. Cleanup is performed off the real-time thread.
    pub fn stalled(&self, now: Instant) -> bool {
        self.stdin.stalled(now)
    }

    pub fn terminate(&mut self) {
        self.stdin.close(true);
        let _ = self.child.kill();
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Asks the provider to exit, then terminates it if it does not.
    pub fn shutdown(mut self) {
        self.stop(Duration::from_millis(1500));
    }

    fn stop(&mut self, grace: Duration) {
        let _ = self.send(&ToProvider::Shutdown);
        self.stdin.close(false); // Closing stdin also ends providers that only watch for EOF.
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if !matches!(self.child.try_wait(), Ok(None)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        // A process the provider started may keep the pipes open after the provider is gone;
        // its threads are then left to end on their own instead of holding up the caller.
        let deadline = Instant::now() + THREAD_WAIT;
        for thread in [self.writer.take(), self.reader.take()].into_iter().flatten() {
            if !join_until(thread, deadline) {
                log::warn!("speech service pipes still open after it exited; not waiting");
            }
        }
    }
}

/// Joins `thread` if it ends before `deadline`; otherwise leaves it running.
fn join_until(thread: JoinHandle<()>, deadline: Instant) -> bool {
    while !thread.is_finished() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = thread.join();
    true
}

impl Drop for ProviderProcess {
    fn drop(&mut self) {
        self.stop(Duration::from_millis(300));
    }
}

fn read_lines<R: Read>(stream: R, sink: &OutputSink) {
    read_bounded_lines(stream, MAX_LINE, |line, overflow| {
        if overflow {
            sink(ProviderOutput::Malformed);
        } else {
            emit_line(line, sink);
        }
    });
}

/// Drain oversized lines without allocating their entire contents; stderr needs the
/// same limit as protocol output even when a provider never writes a newline.
fn read_bounded_lines<R: Read>(stream: R, limit: usize, mut emit: impl FnMut(&[u8], bool)) {
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    loop {
        line.clear();
        let mut overflow = false;
        loop {
            let available = match reader.fill_buf() {
                Ok(b) => b,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return,
            };
            if available.is_empty() {
                if !line.is_empty() || overflow {
                    emit(&line, overflow);
                }
                return;
            }
            let (chunk, done) = match available.iter().position(|&b| b == b'\n') {
                Some(i) => (&available[..i], Some(i + 1)),
                None => (available, None),
            };
            let take = chunk.len().min(limit.saturating_sub(line.len()));
            line.extend_from_slice(&chunk[..take]);
            overflow |= take < chunk.len();
            let used = done.unwrap_or(available.len());
            reader.consume(used);
            if done.is_some() {
                break;
            }
        }
        emit(&line, overflow);
    }
}

fn emit_line(line: &[u8], sink: &OutputSink) {
    let text = String::from_utf8_lossy(line);
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    match serde_json::from_str::<FromProvider>(text) {
        Ok(message) => sink(ProviderOutput::Message(message)),
        Err(_) => sink(ProviderOutput::Malformed),
    }
}

/// The exception name from a provider's last stderr line. Messages are not logged because
/// they may contain service details.
pub fn stderr_summary(stderr: &[String]) -> String {
    let last = stderr.iter().rev().map(|l| l.trim()).find(|l| !l.is_empty()).unwrap_or("");
    let head = last.split(':').next().unwrap_or("").trim();
    // Stderr is arbitrary provider output. Character filtering is not redaction:
    // a bare token could otherwise be mistaken for an exception name.
    match head {
        "" => "no output",
        "AttributeError" => "AttributeError",
        "BrokenPipeError" => "BrokenPipeError",
        "ConnectionError" => "ConnectionError",
        "FileNotFoundError" => "FileNotFoundError",
        "ImportError" => "ImportError",
        "KeyError" => "KeyError",
        "MemoryError" => "MemoryError",
        "ModuleNotFoundError" => "ModuleNotFoundError",
        "OSError" => "OSError",
        "PermissionError" => "PermissionError",
        "RuntimeError" => "RuntimeError",
        "SyntaxError" => "SyntaxError",
        "TimeoutError" => "TimeoutError",
        "TypeError" => "TypeError",
        "UnicodeDecodeError" => "UnicodeDecodeError",
        "UnicodeEncodeError" => "UnicodeEncodeError",
        "ValueError" => "ValueError",
        _ => "provider stderr",
    }
    .into()
}

/// Starts a provider, waits for `hello`, then stops it again.
pub fn probe(spec: &ProviderSpec, timeout: Duration) -> Result<(String, Capabilities), String> {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let process = ProviderProcess::spawn(
        spec,
        Box::new(move |out| {
            let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send(out);
        }),
    )
    .map_err(|_| "spawn_failed".to_string())?;
    let deadline = Instant::now() + timeout;
    let result = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(ProviderOutput::Message(FromProvider::Hello { name, capabilities, .. })) => {
                break Ok((name, capabilities));
            }
            Ok(ProviderOutput::Message(FromProvider::Error { code, .. })) => break Err(code),
            Ok(ProviderOutput::Exited { stderr }) => {
                log::warn!("provider exited during check: {}", stderr_summary(&stderr));
                break Err("provider_exited".into());
            }
            Ok(_) => continue,
            Err(_) => break Err("timeout".into()),
        }
    };
    process.shutdown();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_settings_become_the_environment_of_speech_services() {
        assert_eq!(proxy_env("direct", "http://127.0.0.1:7890"), vec![("NO_PROXY".to_string(), "*".to_string())]);
        let custom = proxy_env("custom", " http://127.0.0.1:7890/ ");
        assert_eq!(custom[0], ("HTTP_PROXY".to_string(), "http://127.0.0.1:7890".to_string()));
        assert_eq!(custom[1], ("HTTPS_PROXY".to_string(), "http://127.0.0.1:7890".to_string()));
        assert_eq!(custom[2], ("NO_PROXY".to_string(), "localhost,127.0.0.1".to_string()));
        for bad in ["", "127.0.0.1:7890", "socks5://127.0.0.1:1080", "http://user:pw@host:1", "http://host:1/path"] {
            assert!(proxy_env("custom", bad).is_empty(), "{bad}");
        }
        assert_eq!(
            parse_system_proxy("127.0.0.1:7890", "<local>;*.corp.example;10.*"),
            Some(("http://127.0.0.1:7890".into(), "localhost,127.0.0.1,.corp.example,10.*".into()))
        );
        assert_eq!(
            parse_system_proxy("http=a:1;https=b:2;socks=c:3", ""),
            Some(("http://b:2".into(), "localhost,127.0.0.1".into()))
        );
        assert_eq!(parse_system_proxy("http=a:1", "").unwrap().0, "http://a:1");
        assert_eq!(parse_system_proxy("socks=c:3", ""), None, "a SOCKS-only setting cannot serve the bridges");
        assert_eq!(parse_system_proxy("", ""), None);
    }

    #[test]
    fn bundled_python_is_used_by_both_bridges_but_explicit_python_wins() {
        let dir = std::env::temp_dir().join(format!("ll-bundled-python-{}", std::process::id()));
        for relative in ["runtime/python", "providers", "doubao_asr", "nested"] {
            std::fs::create_dir_all(dir.join(relative)).unwrap();
        }
        for relative in [
            "runtime/python/python.exe",
            "explicit.exe",
            "providers/doubao_live.py",
            "providers/official_live.py",
            "doubao_asr/doubao.py",
        ] {
            std::fs::write(dir.join(relative), b"offline path fixture; never executed").unwrap();
        }
        let search = [dir.join("nested")];
        let bundled = dir.join("runtime/python/python.exe");
        assert_eq!(python_command(&search).unwrap().0, bundled);
        let settings = DoubaoSettings { module_root: dir.to_string_lossy().into_owned(), ..Default::default() };
        assert_eq!(doubao_spec(&settings, &search).unwrap().program, bundled);
        let explicit = DoubaoSettings { python: dir.join("explicit.exe").to_string_lossy().into_owned(), ..settings };
        assert_eq!(doubao_spec(&explicit, &search).unwrap().program, dir.join("explicit.exe"));
        let official = CustomSpeechProvider { provider_type: "openai".into(), ..Default::default() };
        assert_eq!(official_spec(&official, None, &search).unwrap().program, bundled);
        let no_bundle = dir.join("no-bundle");
        std::fs::create_dir_all(&no_bundle).unwrap();
        std::fs::remove_file(&bundled).unwrap();
        let expected = find_in_path("python.exe").or_else(|| find_in_path("py.exe"));
        assert_eq!(python_command(&[no_bundle]).ok().map(|(program, _)| program), expected);
        std::fs::remove_dir_all(dir).unwrap();
    }
    use std::sync::mpsc;

    fn take(queue: &InputQueue) -> serde_json::Value {
        let line = queue.next().unwrap();
        let value = serde_json::from_slice(&line.bytes).unwrap();
        queue.written();
        value
    }

    #[test]
    fn stop_follows_audio_but_cancel_removes_its_unsent_audio() {
        let queue = InputQueue::new();
        queue.send(&ToProvider::Start { id: 1, sample_rate: 16_000, language: "zh".into() }).unwrap();
        queue.send(&ToProvider::audio(1, &[1, 2])).unwrap();
        queue.send(&ToProvider::Stop { id: 1 }).unwrap();
        assert_eq!(take(&queue)["type"], "start");
        assert_eq!(take(&queue)["type"], "audio");
        assert_eq!(take(&queue)["type"], "stop");

        queue.send(&ToProvider::audio(1, &[1, 2])).unwrap();
        queue.send(&ToProvider::audio(2, &[3, 4])).unwrap();
        queue.send(&ToProvider::Cancel { id: 1 }).unwrap();
        let remaining = take(&queue);
        assert_eq!(remaining["type"], "audio");
        assert_eq!(remaining["id"], 2);
        assert_eq!(take(&queue)["type"], "cancel");
        assert_eq!(queue.state.lock().unwrap().audio, 0);
    }

    #[test]
    fn stalled_pipe_is_detected_even_after_capture_stops() {
        let queue = InputQueue::new();
        assert!(!queue.stalled(Instant::now() + WRITE_STALL * 2), "idle is healthy");
        queue.send(&ToProvider::Stop { id: 1 }).unwrap();
        let _in_flight = queue.next().unwrap();
        let last = queue.state.lock().unwrap().progress;
        assert!(!queue.stalled(last + WRITE_STALL / 2));
        assert!(queue.stalled(last + WRITE_STALL));
        queue.written();
        assert!(!queue.stalled(Instant::now() + WRITE_STALL * 2));
        queue.close(true);
        assert!(queue.send(&ToProvider::Prepare).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn offline_reader_receives_stop_after_audio_and_cancel_without_the_backlog() {
        // The built-in provider already requires Python. This helper only reads stdin and
        // returns frame counts; it never imports the ASR module or opens a network socket.
        let program = find_in_path("python.exe").expect("Python 3 is required for the offline provider test");
        let script = r#"
import json, sys, time
counts = {}
for raw in sys.stdin:
    m = json.loads(raw)
    kind = m['type']
    sid = m.get('id', 0)
    if kind == 'start':
        counts[sid] = 0
        if sid == 2:
            time.sleep(0.15)
    elif kind == 'audio':
        counts[sid] += 1
    elif kind in ('stop', 'cancel'):
        print(json.dumps({'type':'final','id':sid,'text':str(counts[sid])}), flush=True)
    elif kind == 'shutdown':
        break
"#;
        let spec = ProviderSpec {
            id: "offline-reader".into(),
            name: "offline-reader".into(),
            program,
            args: vec!["-B".into(), "-u".into(), "-c".into(), script.into()],
            env: Vec::new(),
            secret_env: Vec::new(),
        };
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let mut process = ProviderProcess::spawn(
            &spec,
            Box::new(move |out| {
                let _ = tx.lock().unwrap().send(out);
            }),
        )
        .unwrap();
        let start = |id| ToProvider::Start { id, sample_rate: 16_000, language: "zh".into() };
        process.send(&start(1)).unwrap();
        for _ in 0..8 {
            process.send(&ToProvider::audio(1, &[0; 640])).unwrap();
        }
        process.send(&ToProvider::Stop { id: 1 }).unwrap();
        let wait_final = |id| loop {
            match rx.recv_timeout(Duration::from_secs(5)).expect("helper receives the control message") {
                ProviderOutput::Message(FromProvider::Final { id: got, text, .. }) if got == id => {
                    break text.parse::<usize>().unwrap();
                }
                ProviderOutput::Exited { .. } => panic!("offline helper exited early"),
                _ => {}
            }
        };
        assert_eq!(wait_final(1), 8, "normal stop follows every audio frame");
        process.send(&start(2)).unwrap();
        for _ in 0..512 {
            process.send(&ToProvider::audio(2, &[0; 640])).unwrap();
        }
        process.send(&ToProvider::Cancel { id: 2 }).unwrap();
        assert!(wait_final(2) < 512, "cancel discards its unsent audio instead of waiting for all of it");
        process.shutdown();
    }

    #[test]
    fn line_reader_parses_messages_and_survives_garbage() {
        let input = "{\"type\":\"connected\",\"id\":1}\nnot json\n\n{\"type\":\"final\",\"id\":1,\"text\":\"好\"}";
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let sink: OutputSink = Box::new(move |o| tx.lock().unwrap().send(o).unwrap());
        read_lines(input.as_bytes(), &sink);
        let got: Vec<_> = rx.try_iter().collect();
        assert_eq!(
            got,
            vec![
                ProviderOutput::Message(FromProvider::Connected { id: 1 }),
                ProviderOutput::Malformed,
                ProviderOutput::Message(FromProvider::Final { id: 1, text: "好".into() }),
            ]
        );
    }

    #[test]
    fn oversized_lines_are_dropped() {
        let mut input = vec![b'x'; MAX_LINE + 10];
        input.extend_from_slice(b"\n{\"type\":\"end\",\"id\":4}\n");
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let sink: OutputSink = Box::new(move |o| tx.lock().unwrap().send(o).unwrap());
        read_lines(&input[..], &sink);
        let got: Vec<_> = rx.try_iter().collect();
        assert_eq!(
            got,
            vec![ProviderOutput::Malformed, ProviderOutput::Message(FromProvider::End { id: 4, cancelled: false })]
        );
    }

    #[test]
    fn stderr_reader_bounds_unterminated_lines_and_recovers_at_newline() {
        let input = io::repeat(b'x').take((MAX_LINE * 8) as u64).chain(&b"\nValueError: fixture\nlast"[..]);
        let mut got = Vec::new();
        read_bounded_lines(input, STDERR_LINE_BYTES, |line, overflow| got.push((line.to_vec(), overflow)));
        assert_eq!(got[0], (vec![b'x'; STDERR_LINE_BYTES], true));
        assert_eq!(got[1], (b"ValueError: fixture".to_vec(), false));
        assert_eq!(got[2], (b"last".to_vec(), false));
        let mut tail = Vec::new();
        read_bounded_lines(io::repeat(b'z').take(8000), 300, |line, overflow| tail.push((line.len(), overflow)));
        assert_eq!(tail, [(300, true)]);
    }

    #[test]
    fn stderr_summary_never_logs_untrusted_tokens_or_error_details() {
        for raw in [
            "sk-synthetic-secret-without-colon",
            "Authorization: Bearer synthetic-secret",
            "synthetic-user:synthetic-password",
            "ValueErrorsyntheticcredential",
            "arbitrary text without colon",
        ] {
            assert_eq!(stderr_summary(&[raw.into()]), "provider stderr");
        }
        assert_eq!(stderr_summary(&["ValueError: synthetic-secret".into()]), "ValueError");
        assert_eq!(stderr_summary(&["ModuleNotFoundError: private-path".into(), "  ".into()]), "ModuleNotFoundError");
        assert_eq!(stderr_summary(&["RuntimeError".into()]), "RuntimeError");
        assert_eq!(stderr_summary(&["  ".into()]), "no output");
        assert_eq!(stderr_summary(&[]), "no output");
    }

    #[test]
    fn spec_key_hides_secret_values() {
        let spec = ProviderSpec {
            id: "x".into(),
            name: "X".into(),
            program: PathBuf::from("C:/x.exe"),
            args: vec!["--a".into()],
            env: vec![("TOKEN".into(), "s3cret".into()), ("MODE".into(), "fast".into())],
            secret_env: vec!["TOKEN".into()],
        };
        let key = spec.key();
        assert!(!key.contains("s3cret"));
        assert!(key.contains("MODE=fast"));
    }

    #[cfg(windows)]
    #[test]
    fn blocked_pipe_accepts_control_and_can_be_terminated() {
        // PING.EXE never reads its input, like a provider that stopped reading.
        let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
        let spec = ProviderSpec {
            id: "stalled".into(),
            name: "stalled".into(),
            program: root.join("System32").join("PING.EXE"),
            args: vec!["-n".into(), "6".into(), "127.0.0.1".into()],
            env: Vec::new(),
            secret_env: Vec::new(),
        };
        let mut process = ProviderProcess::spawn(&spec, Box::new(|_| {})).unwrap();
        let pcm = vec![0i16; 640];
        let refused = (0..AUDIO_BACKLOG + 300).filter(|_| process.send(&ToProvider::audio(1, &pcm)).is_err()).count();
        assert!(refused > 0, "audio beyond the backlog is dropped");
        assert!(process.send(&ToProvider::Stop { id: 1 }).is_ok(), "stop is never dropped");
        assert!(process.send(&ToProvider::Cancel { id: 1 }).is_ok());
        // The process never reads stdin, so one in-flight pipe write cannot make progress.
        // Use a future clock value to exercise the same watchdog without a multi-second wait.
        assert!(process.stalled(Instant::now() + WRITE_STALL * 2));
        let started = Instant::now();
        process.terminate();
        process.shutdown();
        assert!(started.elapsed() < Duration::from_secs(5), "shutdown has a time limit");
    }

    #[test]
    fn custom_spec_requires_an_existing_command() {
        let custom = CustomSpeechProvider {
            id: "c".into(),
            command: "definitely-not-a-real-program-ll".into(),
            ..Default::default()
        };
        assert_eq!(custom_spec(&custom, None), Err(SpecError::CommandMissing));
    }

    #[test]
    fn official_family_validation_preserves_legacy_routing() {
        let mut custom = CustomSpeechProvider::default();
        assert!(!is_official(&custom));
        custom.provider_type = "command".into();
        assert!(!is_official(&custom));
        custom.provider_type = "unknown-api".into();
        assert!(is_official(&custom));
        assert_eq!(official_spec(&custom, None, &[]), Err(SpecError::OfficialConfigInvalid));
    }

    #[cfg(windows)]
    #[test]
    fn official_bridge_local_probe_and_secret_transport() {
        // Synthetic credentials only. Probe exchanges hello/shutdown, never Start/audio;
        // official_live opens no socket until Start, so this test cannot incur API usage.
        let search = [PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")];
        let credential = r#"{"apiKey":"synthetic-api-key","secretId":"synthetic-id","secretKey":"synthetic-key"}"#;
        for family in ["doubao", "openai", "qwen", "tencent"] {
            let custom = CustomSpeechProvider {
                id: format!("test-{family}"),
                name: family.into(),
                provider_type: family.into(),
                app_id: "12345678".into(),
                ..Default::default()
            };
            let spec = official_spec(&custom, Some(credential.into()), &search).unwrap();
            assert!(spec.args.iter().all(|a| !a.to_string_lossy().contains("synthetic")));
            assert!(!spec.key().contains("synthetic"));
            let configuration = &spec.env.iter().find(|(key, _)| key == "LIVE_LISTEN_OFFICIAL_CONFIG").unwrap().1;
            assert!(!configuration.contains("apiKey"));
            assert!(!configuration.contains("secretKey"));
            assert_eq!(spec.secret_env, vec!["LIVE_LISTEN_OFFICIAL_CREDENTIAL"]);
            let (name, capabilities) = probe(&spec, Duration::from_secs(5)).unwrap();
            assert_eq!(name, family);
            assert!(capabilities.interim);
        }
    }
}
