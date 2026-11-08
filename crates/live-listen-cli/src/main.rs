//! `live-listen` command line: devices, speech services and a one-shot listen test.
//!
//! Exit codes: 0 success, 1 runtime failure, 2 usage error.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use live_listen_core::audio::{self, AudioFrame, Capture, Source};
use live_listen_core::config::{self, Settings};
use live_listen_core::paths::AppDirs;
use live_listen_core::protocol::{FromProvider, ToProvider};
use live_listen_core::provider::{self, ProviderOutput, ProviderProcess, ProviderSpec};
use live_listen_core::{dictation, engine, secrets};
use serde_json::{Value, json};

const USAGE: &str = "\
Usage: live-listen <command> [options]

Commands:
  devices                     List microphones
  providers                   List speech services and whether they can start
  check [--provider ID]       Start a speech service and show what it supports
  listen [options]            Recognize audio once and print the text
      --source mic|system|mixed   Audio source (default mic)
      --device NAME               Microphone name (default: system default)
      --wav PATH                  Use a 16 kHz mono PCM16 WAV instead of a device
      --seconds N                 Listen time for devices (default 10)
      --provider ID               Speech service (default: from settings)
  config                      Show the settings file location and contents

Options for all commands:
  --config DIR    Settings folder (default %APPDATA%\\LiveListen)
  --json          Print JSON (one object per line, ASCII only)
  --version       Print the version";

#[cfg(test)]
mod provider_route_tests {
    use super::*;

    #[test]
    fn all_official_services_use_the_bundled_bridge_without_a_command() {
        let search = [PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")];
        for kind in ["doubao", "openai", "qwen", "tencent"] {
            let custom = config::CustomSpeechProvider {
                id: format!("test-{kind}"),
                name: kind.into(),
                provider_type: kind.into(),
                ..Default::default()
            };
            let spec = custom_provider_spec(&custom, Some("synthetic-credential".into()), &search).unwrap();
            assert!(spec.args.iter().any(|arg| arg.to_string_lossy().ends_with("official_live.py")));
            assert!(spec.env.iter().any(|(name, _)| name == "LIVE_LISTEN_OFFICIAL_CONFIG"));
            assert!(spec.secret_env.iter().any(|name| name == "LIVE_LISTEN_OFFICIAL_CREDENTIAL"));
            assert!(!spec.key().contains("synthetic-credential"));
        }
    }

    #[test]
    fn legacy_command_service_keeps_its_existing_route() {
        let custom = config::CustomSpeechProvider {
            id: "legacy".into(),
            command: provider::find_in_path("python.exe")
                .expect("Python 3 is required for the offline provider test")
                .to_string_lossy()
                .into_owned(),
            args: vec!["-B".into()],
            ..Default::default()
        };
        let spec = custom_provider_spec(&custom, None, &[]).unwrap();
        assert_eq!(spec.args, [std::ffi::OsString::from("-B")]);
        assert!(spec.env.is_empty());
    }
}

struct Args {
    command: String,
    json: bool,
    config: Option<PathBuf>,
    source: String,
    device: Option<String>,
    wav: Option<PathBuf>,
    seconds: Option<f64>,
    provider: Option<String>,
}

enum Failure {
    Usage(String),
    Runtime(String),
}

fn parse(mut raw: impl Iterator<Item = String>) -> Result<Args, Failure> {
    let mut args = Args {
        command: String::new(),
        json: false,
        config: None,
        source: "mic".into(),
        device: None,
        wav: None,
        seconds: None,
        provider: None,
    };
    let need = |name: &str, v: Option<String>| v.ok_or_else(|| Failure::Usage(format!("{name} needs a value")));
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "--json" => args.json = true,
            "--config" => args.config = Some(PathBuf::from(need("--config", raw.next())?)),
            "--source" => args.source = need("--source", raw.next())?,
            "--device" => args.device = Some(need("--device", raw.next())?),
            "--wav" => args.wav = Some(PathBuf::from(need("--wav", raw.next())?)),
            "--provider" => args.provider = Some(need("--provider", raw.next())?),
            "--seconds" => {
                let v = need("--seconds", raw.next())?;
                let n: f64 = v.parse().map_err(|_| Failure::Usage(format!("bad --seconds value: {v}")))?;
                if !(0.5..=600.0).contains(&n) {
                    return Err(Failure::Usage("--seconds must be between 0.5 and 600".into()));
                }
                args.seconds = Some(n);
            }
            "--version" | "-V" => args.command = "version".into(),
            "--help" | "-h" | "help" => args.command = "help".into(),
            other if other.starts_with('-') => return Err(Failure::Usage(format!("unknown option: {other}"))),
            other if args.command.is_empty() => args.command = other.to_string(),
            other => return Err(Failure::Usage(format!("unexpected argument: {other}"))),
        }
    }
    Ok(args)
}

/// JSON with every non-ASCII character escaped, safe for any console code page.
fn ascii_json(value: &Value) -> String {
    let text = value.to_string();
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut buf = [0u16; 2];
            for unit in c.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

fn load_settings(args: &Args) -> (AppDirs, Settings) {
    let dirs = AppDirs::resolve(args.config.as_deref());
    let mut settings = Settings::load(&dirs.settings_file());
    settings.sanitize();
    (dirs, settings)
}

fn search_paths() -> Vec<PathBuf> {
    let mut search = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)) {
        search.push(dir);
    }
    if let Ok(dir) = std::env::current_dir() {
        search.push(dir);
    }
    search
}

fn spec_for(settings: &Settings, id: &str) -> Result<ProviderSpec, String> {
    if id == config::DOUBAO_ID {
        return provider::doubao_spec(&settings.services.doubao, &search_paths()).map_err(|e| e.code().to_string());
    }
    let custom =
        settings.services.custom_speech.iter().find(|c| c.id == id).ok_or_else(|| "unknown_provider".to_string())?;
    let secret = if provider::is_official(custom) || !custom.secret_env.trim().is_empty() {
        secrets::get(&format!("speech.{id}"))
    } else {
        None
    };
    custom_provider_spec(custom, secret, &search_paths())
}

fn custom_provider_spec(
    custom: &config::CustomSpeechProvider,
    secret: Option<String>,
    search: &[PathBuf],
) -> Result<ProviderSpec, String> {
    let spec = if provider::is_official(custom) {
        provider::official_spec(custom, secret, search)
    } else {
        provider::custom_spec(custom, secret)
    };
    spec.map_err(|e| e.code().to_string())
}

fn devices(args: &Args) {
    let list = audio::input_devices();
    let default = audio::default_input();
    if args.json {
        println!("{}", ascii_json(&json!({"microphones": list, "default": default})));
        return;
    }
    if list.is_empty() {
        println!("No microphones found.");
    }
    for name in list {
        let mark = if default.as_deref() == Some(name.as_str()) { " (default)" } else { "" };
        println!("{name}{mark}");
    }
}

fn providers(args: &Args) {
    let (_, settings) = load_settings(args);
    let mut ids = vec![config::DOUBAO_ID.to_string()];
    ids.extend(settings.services.custom_speech.iter().map(|c| c.id.clone()));
    for id in ids {
        let spec = spec_for(&settings, &id);
        let active = settings.services.speech == id;
        if args.json {
            let value = json!({"id": id, "active": active, "available": spec.is_ok(), "error": spec.as_ref().err()});
            println!("{}", ascii_json(&value));
        } else {
            let state = match &spec {
                Ok(s) => format!("ready ({})", s.program.display()),
                Err(code) => format!("unavailable: {code}"),
            };
            println!("{}{id}: {state}", if active { "* " } else { "  " });
        }
    }
}

fn check(args: &Args) -> Result<(), Failure> {
    let (_, settings) = load_settings(args);
    let id = args.provider.clone().unwrap_or_else(|| settings.services.speech.clone());
    let spec = spec_for(&settings, &id).map_err(Failure::Runtime)?;
    let (name, caps) = provider::probe(&spec, Duration::from_secs(20)).map_err(Failure::Runtime)?;
    if args.json {
        println!("{}", ascii_json(&json!({"id": id, "name": name, "capabilities": caps})));
    } else {
        println!("{id} ({name}) is ready.");
        println!("interim: {}, final: {}, timestamps: {}", caps.interim, caps.finals, caps.timestamps);
    }
    Ok(())
}

enum Event {
    Frame(AudioFrame),
    AudioFailed,
    Provider(ProviderOutput),
}

fn listen(args: &Args) -> Result<(), Failure> {
    let (_, settings) = load_settings(args);
    let id = args.provider.clone().unwrap_or_else(|| settings.services.speech.clone());
    let spec = spec_for(&settings, &id).map_err(Failure::Runtime)?;
    let source = match (&args.wav, args.source.as_str()) {
        (Some(path), _) => Source::File(path.clone()),
        (None, "mic") => Source::Microphone(args.device.clone()),
        (None, "system") => Source::System,
        (None, "mixed") => Source::Mixed(args.device.clone()),
        (None, other) => return Err(Failure::Usage(format!("unknown source: {other}"))),
    };
    let listen_for = match &args.wav {
        Some(path) => {
            let samples = audio::read_wav(path).map_err(|e| Failure::Runtime(e.to_string()))?.len();
            Duration::from_secs_f64(samples as f64 / 16_000.0 + 1.0)
        }
        None => Duration::from_secs_f64(args.seconds.unwrap_or(10.0)),
    };
    let (tx, rx) = mpsc::channel();
    let provider_tx = tx.clone();
    let mut process = ProviderProcess::spawn(
        &spec,
        Box::new(move |output| {
            let _ = provider_tx.send(Event::Provider(output));
        }),
    )
    .map_err(|e| Failure::Runtime(format!("speech service failed to start: {e}")))?;
    const SESSION: u64 = 1;
    let start = ToProvider::Start { id: SESSION, sample_rate: 16_000, language: "zh-CN".into() };
    process.send(&start).map_err(|e| Failure::Runtime(e.to_string()))?;
    let frame_tx = tx.clone();
    let error_tx = std::sync::Mutex::new(tx);
    let started = Instant::now();
    let mut capture = Some(
        Capture::start(
            source,
            Box::new(move |frame| {
                let _ = frame_tx.send(Event::Frame(frame));
            }),
            Arc::new(move |_message: String| {
                let _ = error_tx.lock().unwrap_or_else(|e| e.into_inner()).send(Event::AudioFailed);
            }),
        )
        .map_err(|e| Failure::Runtime(e.to_string()))?,
    );
    if !args.json {
        eprintln!("Listening for {:.0} s...", listen_for.as_secs_f64());
    }
    let mut stop_at = Some(started + listen_for);
    let mut give_up = started + listen_for + Duration::from_secs(8);
    let mut latency: Option<u128> = None;
    let mut last = String::new();
    let result = loop {
        let now = Instant::now();
        if stop_at.is_some_and(|t| now >= t) {
            stop_at = None;
            capture = None;
            let _ = process.send(&ToProvider::Stop { id: SESSION });
            give_up = now + Duration::from_secs(8);
        }
        if now >= give_up {
            break Err(Failure::Runtime("no final text from the speech service".into()));
        }
        let wait = stop_at.unwrap_or(give_up).saturating_duration_since(now).min(Duration::from_millis(200));
        let event = match rx.recv_timeout(wait) {
            Ok(e) => e,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break Err(Failure::Runtime("speech service stopped".into())),
        };
        match event {
            Event::Frame(frame) => {
                if capture.is_some() {
                    let _ = process.send(&ToProvider::audio(SESSION, &frame.pcm));
                }
            }
            Event::AudioFailed => break Err(Failure::Runtime("audio device stopped".into())),
            Event::Provider(ProviderOutput::Message(message)) => match message {
                FromProvider::Text { text, stable, .. } => {
                    let text = dictation::sanitize(&text);
                    if latency.is_none() && !text.trim().is_empty() {
                        latency = Some(started.elapsed().as_millis());
                    }
                    if args.json {
                        println!("{}", ascii_json(&json!({"type": "text", "text": text, "stable": stable})));
                    } else if text != last {
                        eprintln!("... {text}");
                    }
                    last = text;
                }
                FromProvider::Final { text, .. } => {
                    let text = dictation::sanitize(&text);
                    let elapsed = started.elapsed().as_millis();
                    if args.json {
                        let value = json!({"type": "final", "text": text, "latencyMs": latency, "elapsedMs": elapsed});
                        println!("{}", ascii_json(&value));
                    } else {
                        println!("{text}");
                        if let Some(ms) = latency {
                            eprintln!("first text after {ms} ms");
                        }
                    }
                    break Ok(());
                }
                FromProvider::End { .. } => {
                    if args.json {
                        println!("{}", ascii_json(&json!({"type": "final", "text": last, "latencyMs": latency})));
                    } else {
                        println!("{last}");
                    }
                    break Ok(());
                }
                FromProvider::Error { code, .. } => {
                    break Err(Failure::Runtime(format!("speech service error: {code}")));
                }
                _ => {}
            },
            Event::Provider(ProviderOutput::Exited { stderr }) => {
                break Err(Failure::Runtime(format!("speech service exited: {}", provider::stderr_summary(&stderr))));
            }
            Event::Provider(ProviderOutput::Malformed) => {}
        }
    };
    drop(capture);
    process.shutdown();
    result
}

fn show_config(args: &Args) {
    let (dirs, settings) = load_settings(args);
    if args.json {
        println!("{}", ascii_json(&json!({"file": dirs.settings_file(), "logs": dirs.logs, "settings": settings})));
    } else {
        println!("Settings: {}", dirs.settings_file().display());
        println!("Logs:     {}", dirs.logs.display());
        println!("{}", serde_json::to_string_pretty(&settings).unwrap_or_default());
    }
}

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(Failure::Usage(m)) | Err(Failure::Runtime(m)) => {
            eprintln!("{m}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = match args.command.as_str() {
        "version" => {
            println!("live-listen {}", engine::VERSION);
            Ok(())
        }
        "help" => {
            println!("{USAGE}");
            Ok(())
        }
        "devices" => {
            devices(&args);
            Ok(())
        }
        "providers" => {
            providers(&args);
            Ok(())
        }
        "check" => check(&args),
        "listen" => listen(&args),
        "config" => {
            show_config(&args);
            Ok(())
        }
        "" => Err(Failure::Usage("missing command".into())),
        other => Err(Failure::Usage(format!("unknown command: {other}"))),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(m)) => {
            eprintln!("{m}\n\n{USAGE}");
            ExitCode::from(2)
        }
        Err(Failure::Runtime(m)) => {
            if args.json {
                println!("{}", ascii_json(&json!({"type": "error", "message": m})));
            } else {
                eprintln!("error: {m}");
            }
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Args, Failure> {
        parse(list.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_listen_options() {
        let a = args(&["listen", "--wav", "a.wav", "--seconds", "3", "--json"]).ok().unwrap();
        assert_eq!(a.command, "listen");
        assert!(a.json);
        assert_eq!(a.wav, Some(PathBuf::from("a.wav")));
        assert_eq!(a.seconds, Some(3.0));
        assert!(matches!(args(&["listen", "--seconds", "0"]), Err(Failure::Usage(_))));
        assert!(matches!(args(&["listen", "--bogus"]), Err(Failure::Usage(_))));
        assert!(matches!(args(&["listen", "extra", "more"]), Err(Failure::Usage(_))));
    }

    #[test]
    fn json_output_is_ascii() {
        let text = ascii_json(&json!({"text": "你好😀"}));
        assert!(text.is_ascii());
        let back: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back["text"], "你好😀");
    }
}
