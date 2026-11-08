//! Audio capture: microphone, computer playback (loopback) or both, delivered as 16 kHz mono
//! PCM16 frames of 40 ms. A WAV file source exists for tests and demos without a microphone.

pub mod resample;

use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use resample::{Resampler, TARGET_RATE, level, to_pcm16};

/// Samples per delivered frame (40 ms at 16 kHz).
pub const FRAME: usize = 640;
/// Longest time a device may take to start before the capture counts as failed.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
/// How often a capture that follows the default devices checks whether they changed.
const DEFAULT_CHECK: Duration = Duration::from_secs(2);
/// Mixed sources keep their sum below full scale with a soft knee above this level.
const MIX_KNEE: f32 = 0.8;
/// The error text that reports a changed default device; the owner reopens the capture.
pub const DEFAULT_CHANGED: &str = "default device changed";

/// One frame of 16 kHz mono PCM16 with its speech level in `[0, 1]`.
#[derive(Clone, Debug)]
pub struct AudioFrame {
    pub pcm: Vec<i16>,
    pub level: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Device name; `None` is the system default microphone.
    Microphone(Option<String>),
    /// What the default output device is playing.
    System,
    /// Microphone and computer playback mixed.
    Mixed(Option<String>),
    /// A 16 kHz mono PCM16 WAV played at real-time speed, followed by silence.
    File(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioError {
    NoDevice,
    Unsupported(String),
    Device(String),
    File(String),
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AudioError::NoDevice => f.write_str("no audio device"),
            AudioError::Unsupported(s) => write!(f, "unsupported audio format: {s}"),
            AudioError::Device(s) => write!(f, "audio device error: {s}"),
            AudioError::File(s) => write!(f, "audio file error: {s}"),
        }
    }
}

impl std::error::Error for AudioError {}

pub type FrameSink = Box<dyn FnMut(AudioFrame) + Send>;
pub type ErrorSink = Arc<dyn Fn(String) + Send + Sync>;
/// Receives the result of opening the source, exactly once, from the audio thread.
pub type ReadySink = Box<dyn FnOnce(Result<(), AudioError>) + Send>;

/// Limits a sum of two sources smoothly instead of clipping it hard.
fn soft_limit(x: f32) -> f32 {
    let magnitude = x.abs();
    if magnitude <= MIX_KNEE {
        return x;
    }
    let room = 1.0 - MIX_KNEE;
    (MIX_KNEE + room * ((magnitude - MIX_KNEE) / room).tanh()).copysign(x)
}

/// Turns resampled floats into fixed-size frames; mixes two inputs when both are present.
struct Pipeline {
    resamplers: Vec<Resampler>,
    queues: Vec<VecDeque<f32>>,
    sink: FrameSink,
    scratch: Vec<f32>,
}

impl Pipeline {
    fn new(rates: &[u32], sink: FrameSink) -> Self {
        Pipeline {
            resamplers: rates.iter().map(|&r| Resampler::new(r)).collect(),
            queues: rates.iter().map(|_| VecDeque::new()).collect(),
            sink,
            scratch: Vec::new(),
        }
    }

    fn push(&mut self, slot: usize, mono: &[f32]) {
        self.scratch.clear();
        self.resamplers[slot].process(mono, &mut self.scratch);
        self.queues[slot].extend(self.scratch.iter().copied());
        self.drain();
    }

    fn drain(&mut self) {
        // Loopback delivers nothing while the computer is silent, so a backlog on one side
        // is flushed, mixed with silence, instead of waiting forever.
        let backlog = self.queues.iter().any(|q| q.len() >= FRAME * 3);
        loop {
            let ready = self.queues.iter().filter(|q| q.len() >= FRAME).count();
            if ready < self.queues.len() && !backlog {
                return;
            }
            if ready == 0 {
                return;
            }
            let mut mixed = vec![0.0f32; FRAME];
            for q in &mut self.queues {
                let take = q.len().min(FRAME);
                for (m, s) in mixed.iter_mut().zip(q.drain(..take)) {
                    *m += s;
                }
            }
            if self.queues.len() > 1 {
                for m in &mut mixed {
                    *m = soft_limit(*m);
                }
            }
            let mut pcm = Vec::with_capacity(FRAME);
            to_pcm16(&mixed, &mut pcm);
            let level = level(&pcm);
            (self.sink)(AudioFrame { pcm, level });
        }
    }
}

/// A capture. Dropping it stops the device streams.
pub struct Capture {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    /// The device started. Until then the audio thread may still be inside a driver call,
    /// so dropping the capture lets that thread finish on its own instead of waiting for it.
    running: Arc<AtomicBool>,
}

impl Capture {
    /// Opens the source on its own thread and returns at once. Frames go to `sink` as soon as
    /// the device runs, possibly before `on_ready` has been called with `Ok`. With
    /// `follow_default`, a change of the default device that the source uses is reported
    /// through `on_error` with [`DEFAULT_CHANGED`].
    pub fn open(
        source: Source,
        follow_default: bool,
        sink: FrameSink,
        on_error: ErrorSink,
        on_ready: ReadySink,
    ) -> Result<Capture, AudioError> {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let running = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&running);
        let ready: ReadySink = Box::new(move |result| {
            if result.is_ok() {
                flag.store(true, Ordering::Release);
            }
            on_ready(result);
        });
        let thread = std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || match source {
                Source::File(path) => play_file(path, sink, stop_rx, ready),
                other => run_devices(other, follow_default, sink, on_error, stop_rx, ready),
            })
            .map_err(|e| AudioError::Device(e.to_string()))?;
        Ok(Capture { stop: Some(stop_tx), thread: Some(thread), running })
    }

    /// Opens the source and waits until the device runs (at most [`OPEN_TIMEOUT`]).
    pub fn start(source: Source, sink: FrameSink, on_error: ErrorSink) -> Result<Capture, AudioError> {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), AudioError>>();
        let capture = Capture::open(
            source,
            false,
            sink,
            on_error,
            Box::new(move |result| {
                let _ = ready_tx.send(result);
            }),
        )?;
        match ready_rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(Ok(())) => Ok(capture),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(AudioError::Device("audio device did not start".into())),
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            if self.running.load(Ordering::Acquire) {
                let _ = thread.join();
            }
            // Otherwise the thread sees the closed stop channel once its open call returns.
        }
    }
}

/// True when both handles name the same endpoint (Windows compares endpoint ids).
fn same_device(a: &cpal::Device, b: &cpal::Device) -> bool {
    use cpal::platform::DeviceInner;
    #[allow(unreachable_patterns)]
    match (a.as_inner(), b.as_inner()) {
        (DeviceInner::Wasapi(x), DeviceInner::Wasapi(y)) => x == y,
        _ => a.name().ok() == b.name().ok(),
    }
}

fn find_input(host: &cpal::Host, name: Option<&str>) -> Result<cpal::Device, AudioError> {
    if let Some(name) = name.filter(|n| !n.is_empty()) {
        if let Ok(mut devices) = host.input_devices() {
            if let Some(d) = devices.find(|d| d.name().is_ok_and(|n| n == name)) {
                return Ok(d);
            }
        }
        log::warn!("selected microphone is missing; using the default");
    }
    host.default_input_device().ok_or(AudioError::NoDevice)
}

/// One opened device and whether it stands for the current default input or output.
struct Opened {
    device: cpal::Device,
    config: cpal::SupportedStreamConfig,
    default: Option<bool>,
}

fn open_devices(host: &cpal::Host, source: &Source) -> Result<Vec<Opened>, AudioError> {
    let mut list = Vec::new();
    let mic = match source {
        Source::Microphone(n) | Source::Mixed(n) => Some(n.as_deref()),
        _ => None,
    };
    if let Some(name) = mic {
        let device = find_input(host, name)?;
        let config = device.default_input_config().map_err(|e| AudioError::Device(e.to_string()))?;
        let default = name.is_none_or(str::is_empty).then_some(false);
        list.push(Opened { device, config, default });
    }
    if matches!(source, Source::System | Source::Mixed(_)) {
        let device = host.default_output_device().ok_or(AudioError::NoDevice)?;
        let config = device.default_output_config().map_err(|e| AudioError::Device(e.to_string()))?;
        list.push(Opened { device, config, default: Some(true) });
    }
    Ok(list)
}

fn start_streams(devices: &[Opened], sink: FrameSink, on_error: &ErrorSink) -> Result<Vec<cpal::Stream>, AudioError> {
    let rates: Vec<u32> = devices.iter().map(|d| d.config.sample_rate().0).collect();
    let pipeline = Arc::new(Mutex::new(Pipeline::new(&rates, sink)));
    let mut streams = Vec::new();
    for (slot, opened) in devices.iter().enumerate() {
        let config: cpal::StreamConfig = opened.config.config();
        let device = &opened.device;
        let stream = match opened.config.sample_format() {
            SampleFormat::F32 => build::<f32>(device, &config, slot, &pipeline, on_error),
            SampleFormat::I16 => build::<i16>(device, &config, slot, &pipeline, on_error),
            SampleFormat::I32 => build::<i32>(device, &config, slot, &pipeline, on_error),
            SampleFormat::U16 => build::<u16>(device, &config, slot, &pipeline, on_error),
            other => Err(AudioError::Unsupported(format!("{other:?}"))),
        }?;
        stream.play().map_err(|e| AudioError::Device(e.to_string()))?;
        streams.push(stream);
    }
    Ok(streams)
}

/// True when a default device that this capture follows is no longer the default.
fn default_changed(host: &cpal::Host, devices: &[Opened]) -> bool {
    devices.iter().any(|opened| {
        let now = match opened.default {
            Some(true) => host.default_output_device(),
            Some(false) => host.default_input_device(),
            None => return false,
        };
        now.is_some_and(|now| !same_device(&now, &opened.device))
    })
}

fn run_devices(
    source: Source,
    follow_default: bool,
    sink: FrameSink,
    on_error: ErrorSink,
    stop: mpsc::Receiver<()>,
    ready: ReadySink,
) {
    let host = cpal::default_host();
    let started = open_devices(&host, &source)
        .and_then(|devices| start_streams(&devices, sink, &on_error).map(|streams| (devices, streams)));
    let (devices, streams) = match started {
        Ok(running) => running,
        Err(e) => {
            ready(Err(e));
            return;
        }
    };
    ready(Ok(()));
    // Run until the owner drops the Capture; dropping the streams stops the devices. A loopback
    // stream keeps running, silent, on a device that is no longer the default, so a capture
    // that follows the default checks for a change and reports it once.
    let follows = follow_default && devices.iter().any(|d| d.default.is_some());
    if follows {
        while let Err(mpsc::RecvTimeoutError::Timeout) = stop.recv_timeout(DEFAULT_CHECK) {
            if default_changed(&host, &devices) {
                on_error(DEFAULT_CHANGED.into());
                let _ = stop.recv();
                break;
            }
        }
    } else {
        let _ = stop.recv();
    }
    drop(streams);
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    slot: usize,
    pipeline: &Arc<Mutex<Pipeline>>,
    on_error: &ErrorSink,
) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels.max(1));
    let pipeline = Arc::clone(pipeline);
    let on_error = Arc::clone(on_error);
    let mut mono: Vec<f32> = Vec::new();
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                mono.clear();
                for frame in data.chunks(channels) {
                    let sum: f32 = frame.iter().map(|&s| f32::from_sample_(s)).sum();
                    mono.push(sum / channels as f32);
                }
                if let Ok(mut p) = pipeline.lock() {
                    p.push(slot, &mono);
                }
            },
            move |e| on_error(e.to_string()),
            None,
        )
        .map_err(|e| AudioError::Device(e.to_string()))
}

/// Reads a 16 kHz mono PCM16 WAV file fully into memory.
pub fn read_wav(path: &std::path::Path) -> Result<Vec<i16>, AudioError> {
    let data = fs::read(path).map_err(|e| AudioError::File(e.to_string()))?;
    let bad = |m: &str| AudioError::File(m.to_string());
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err(bad("not a WAV file"));
    }
    let mut offset = 12;
    let mut format_ok = false;
    while offset + 8 <= data.len() {
        let id = &data[offset..offset + 4];
        let size =
            u32::from_le_bytes([data[offset + 4], data[offset + 5], data[offset + 6], data[offset + 7]]) as usize;
        let body = offset + 8;
        let end = body.checked_add(size).filter(|&e| e <= data.len()).ok_or_else(|| bad("truncated WAV"))?;
        if id == b"fmt " && size >= 16 {
            let f = &data[body..end];
            let format = u16::from_le_bytes([f[0], f[1]]);
            let channels = u16::from_le_bytes([f[2], f[3]]);
            let rate = u32::from_le_bytes([f[4], f[5], f[6], f[7]]);
            let bits = u16::from_le_bytes([f[14], f[15]]);
            format_ok = format == 1 && channels == 1 && rate == TARGET_RATE && bits == 16;
            if !format_ok {
                return Err(bad("need 16 kHz mono PCM16"));
            }
        } else if id == b"data" {
            if !format_ok {
                return Err(bad("missing format chunk"));
            }
            return Ok(data[body..end].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect());
        }
        offset = end + (size & 1);
    }
    Err(bad("missing data chunk"))
}

fn play_file(path: PathBuf, mut sink: FrameSink, stop: mpsc::Receiver<()>, ready: ReadySink) {
    let samples = match read_wav(&path) {
        Ok(s) => s,
        Err(e) => {
            ready(Err(e));
            return;
        }
    };
    ready(Ok(()));
    let started = Instant::now();
    let silence = vec![0i16; FRAME];
    for index in 0.. {
        let begin = index * FRAME;
        let pcm = if begin < samples.len() {
            let mut frame = samples[begin..samples.len().min(begin + FRAME)].to_vec();
            frame.resize(FRAME, 0);
            frame
        } else {
            silence.clone()
        };
        let level = level(&pcm);
        sink(AudioFrame { pcm, level });
        let due = started + Duration::from_millis(40 * (index as u64 + 1));
        let wait = due.saturating_duration_since(Instant::now());
        match stop.recv_timeout(wait) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            _ => return,
        }
    }
}

/// Names of the available microphones.
pub fn input_devices() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices().map(|devices| devices.filter_map(|d| d.name().ok()).collect()).unwrap_or_default()
}

/// Name of the default microphone, if any.
pub fn default_input() -> Option<String> {
    cpal::default_host().default_input_device().and_then(|d| d.name().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixer_pads_a_silent_side() {
        let frames = Arc::new(Mutex::new(Vec::new()));
        let out = Arc::clone(&frames);
        let mut p = Pipeline::new(&[16_000, 16_000], Box::new(move |f| out.lock().unwrap().push(f)));
        p.push(0, &vec![0.25; FRAME]);
        assert!(frames.lock().unwrap().is_empty(), "waits for the other side");
        p.push(1, &vec![0.25; FRAME]);
        assert_eq!(frames.lock().unwrap().len(), 1);
        assert!((frames.lock().unwrap()[0].pcm[100] - 16384).abs() < 5);
        p.push(0, &vec![0.1; FRAME * 3]);
        assert_eq!(frames.lock().unwrap().len(), 4, "backlog is mixed with silence");
    }

    #[test]
    fn mixed_peaks_are_limited_softly() {
        assert_eq!(soft_limit(0.5), 0.5, "normal levels pass unchanged");
        assert_eq!(soft_limit(-0.8), -0.8);
        assert!(soft_limit(1.6) < 1.0 && soft_limit(1.6) > 0.95);
        assert!(soft_limit(-1.6) > -1.0);
        assert!(soft_limit(0.9) > soft_limit(0.85), "still rises above the knee");
        let frames = Arc::new(Mutex::new(Vec::new()));
        let out = Arc::clone(&frames);
        let mut p = Pipeline::new(&[16_000, 16_000], Box::new(move |f| out.lock().unwrap().push(f)));
        p.push(0, &vec![0.6; FRAME]);
        p.push(1, &vec![0.6; FRAME]);
        assert!(frames.lock().unwrap()[0].pcm[0] < 32_600, "two loud sources do not clip hard");
    }

    #[test]
    fn file_capture_reports_ready_and_stops_on_drop() {
        let path = std::env::temp_dir().join(format!("ll-open-{}.wav", std::process::id()));
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&36u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&16_000u32.to_le_bytes());
        wav.extend_from_slice(&32_000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&0u32.to_le_bytes());
        fs::write(&path, &wav).unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (frame_tx, frame_rx) = mpsc::channel();
        let capture = Capture::open(
            Source::File(path.clone()),
            true,
            Box::new(move |f| {
                let _ = frame_tx.send(f);
            }),
            Arc::new(|_| {}),
            Box::new(move |r| {
                let _ = ready_tx.send(r);
            }),
        )
        .unwrap();
        assert_eq!(ready_rx.recv_timeout(Duration::from_secs(3)).unwrap(), Ok(()));
        assert!(frame_rx.recv_timeout(Duration::from_secs(3)).is_ok());
        drop(capture);
        let missing = Capture::open(
            Source::File(path.with_extension("missing")),
            false,
            Box::new(|_| {}),
            Arc::new(|_| {}),
            Box::new(|_| {}),
        );
        assert!(missing.is_ok(), "opening returns before the file is read");
        assert!(
            Capture::start(Source::File(path.with_extension("missing")), Box::new(|_| {}), Arc::new(|_| {})).is_err()
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn reads_wav_and_rejects_other_formats() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("ll-audio-{}.wav", std::process::id()));
        let mut wav = Vec::new();
        let samples: [i16; 4] = [1, -2, 300, -400];
        let data_len = (samples.len() * 2) as u32;
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&16_000u32.to_le_bytes());
        wav.extend_from_slice(&32_000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            wav.extend_from_slice(&s.to_le_bytes());
        }
        fs::write(&path, &wav).unwrap();
        assert_eq!(read_wav(&path).unwrap(), samples);
        wav[24..28].copy_from_slice(&44_100u32.to_le_bytes());
        fs::write(&path, &wav).unwrap();
        assert!(read_wav(&path).is_err());
        let _ = fs::remove_file(&path);
    }
}
