//! Streaming conversion of device audio to 16 kHz mono.
//!
//! A windowed-sinc low-pass filter removes content above 7 kHz, then linear interpolation
//! picks output samples. The filter runs only at the input positions that an output sample
//! needs (one in three at 48 kHz), so the cost stays far below 1% CPU.

use std::f64::consts::PI;

/// Sample rate every speech provider receives.
pub const TARGET_RATE: u32 = 16_000;

const TAPS: usize = 95;
const CUTOFF_HZ: f64 = 7_000.0;

pub struct Resampler {
    /// Input samples per output sample.
    ratio: f64,
    taps: Vec<f32>,
    /// Unconsumed input, led by the filter history: filter output `k` uses `raw[k..k + taps]`.
    raw: Vec<f32>,
    /// Position of the next output sample, in filter outputs.
    pos: f64,
    /// The last filter output computed, by index, for the next interpolation.
    last: Option<(usize, f32)>,
}

impl Resampler {
    pub fn new(input_rate: u32) -> Self {
        let input_rate = input_rate.max(1);
        let taps = if input_rate > TARGET_RATE { lowpass(input_rate, CUTOFF_HZ, TAPS) } else { Vec::new() };
        let raw = vec![0.0; taps.len().saturating_sub(1)];
        Resampler { ratio: f64::from(input_rate) / f64::from(TARGET_RATE), taps, raw, pos: 0.0, last: None }
    }

    /// Filter output `k`; without a filter (upsampling) that is the input sample itself.
    fn filtered(&mut self, k: usize) -> f32 {
        if let Some((_, value)) = self.last.filter(|&(index, _)| index == k) {
            return value;
        }
        let value = if self.taps.is_empty() {
            self.raw[k]
        } else {
            self.raw[k..k + self.taps.len()].iter().zip(&self.taps).map(|(x, h)| x * h).sum()
        };
        self.last = Some((k, value));
        value
    }

    /// Feeds mono samples in `[-1, 1]` and appends 16 kHz samples to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.ratio == 1.0 {
            // Already 16 kHz: pass through without the one-sample interpolation delay.
            out.extend_from_slice(input);
            return;
        }
        self.raw.extend_from_slice(input);
        let available = (self.raw.len() + 1).saturating_sub(self.taps.len().max(1));
        while self.pos + 1.0 < available as f64 {
            let i = self.pos.floor() as usize;
            let frac = (self.pos - i as f64) as f32;
            let a = self.filtered(i);
            // At a whole position the next output contributes nothing.
            let y = if frac == 0.0 { a } else { a * (1.0 - frac) + self.filtered(i + 1) * frac };
            out.push(y);
            self.pos += self.ratio;
        }
        let consumed = (self.pos.floor() as usize).min(available);
        self.raw.drain(..consumed);
        self.pos -= consumed as f64;
        self.last = self.last.and_then(|(k, v)| k.checked_sub(consumed).map(|k| (k, v)));
    }
}

/// Blackman-windowed sinc low-pass filter with unity gain at DC.
fn lowpass(rate: u32, cutoff: f64, n: usize) -> Vec<f32> {
    let fc = cutoff / f64::from(rate);
    let m = (n - 1) as f64;
    let raw: Vec<f64> = (0..n)
        .map(|i| {
            let x = i as f64 - m / 2.0;
            let sinc = if x == 0.0 { 2.0 * fc } else { (2.0 * PI * fc * x).sin() / (PI * x) };
            let t = i as f64 / m;
            let window = 0.42 - 0.5 * (2.0 * PI * t).cos() + 0.08 * (4.0 * PI * t).cos();
            sinc * window
        })
        .collect();
    let sum: f64 = raw.iter().sum();
    raw.iter().map(|v| (v / sum) as f32).collect()
}

/// Converts `[-1, 1]` floats to PCM16.
pub fn to_pcm16(samples: &[f32], out: &mut Vec<i16>) {
    out.extend(samples.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0).round() as i16));
}

/// Speech level in `[0, 1]`, mapped from -60..-10 dBFS.
pub fn level(pcm: &[i16]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let energy: f64 = pcm.iter().map(|&s| f64::from(s) * f64::from(s)).sum::<f64>() / pcm.len() as f64;
    let rms = energy.sqrt() / 32768.0;
    if rms <= 1e-9 {
        return 0.0;
    }
    let db = 20.0 * rms.log10();
    (((db + 60.0) / 50.0) as f32).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f64, seconds: f64) -> Vec<f32> {
        let n = (f64::from(rate) * seconds) as usize;
        (0..n).map(|i| (2.0 * PI * freq * i as f64 / f64::from(rate)).sin() as f32 * 0.5).collect()
    }

    fn resample_all(rate: u32, input: &[f32]) -> Vec<f32> {
        let mut r = Resampler::new(rate);
        let mut out = Vec::new();
        for chunk in input.chunks(441) {
            r.process(chunk, &mut out);
        }
        out
    }

    fn amplitude(samples: &[f32]) -> f32 {
        let tail = &samples[samples.len() / 4..];
        let rms = (tail.iter().map(|s| s * s).sum::<f32>() / tail.len() as f32).sqrt();
        rms * std::f32::consts::SQRT_2
    }

    #[test]
    fn keeps_speech_band_and_length() {
        for rate in [48_000, 44_100, 16_000, 8_000] {
            let out = resample_all(rate, &sine(rate, 1_000.0, 1.0));
            assert!((out.len() as i64 - 16_000).abs() < 40, "{rate}: {}", out.len());
            assert!((amplitude(&out) - 0.5).abs() < 0.05, "{rate}: {}", amplitude(&out));
        }
    }

    #[test]
    fn removes_content_above_target_nyquist() {
        let out = resample_all(48_000, &sine(48_000, 12_000.0, 1.0));
        assert!(amplitude(&out) < 0.02, "{}", amplitude(&out));
    }

    /// The earlier implementation: filter every input sample, then interpolate.
    fn reference(rate: u32, input: &[f32], chunk: usize) -> Vec<f32> {
        let ratio = f64::from(rate) / f64::from(TARGET_RATE);
        let taps = if rate > TARGET_RATE { lowpass(rate, CUTOFF_HZ, TAPS) } else { Vec::new() };
        let mut history = vec![0.0f32; taps.len().saturating_sub(1)];
        let mut filtered: Vec<f32> = Vec::new();
        let mut pos = 0.0f64;
        let mut out = Vec::new();
        for part in input.chunks(chunk) {
            if taps.is_empty() {
                filtered.extend_from_slice(part);
            } else {
                let n = taps.len();
                let mut buf = std::mem::take(&mut history);
                buf.extend_from_slice(part);
                if buf.len() >= n {
                    for window in buf.windows(n) {
                        filtered.push(window.iter().zip(&taps).map(|(x, h)| x * h).sum());
                    }
                    history = buf[buf.len() - (n - 1)..].to_vec();
                } else {
                    history = buf;
                }
            }
            while pos + 1.0 < filtered.len() as f64 {
                let i = pos.floor() as usize;
                let frac = (pos - i as f64) as f32;
                out.push(filtered[i] * (1.0 - frac) + filtered[i + 1] * frac);
                pos += ratio;
            }
            let consumed = (pos.floor() as usize).min(filtered.len());
            filtered.drain(..consumed);
            pos -= consumed as f64;
        }
        out
    }

    #[test]
    fn lazy_filter_matches_the_full_filter_exactly() {
        for rate in [48_000, 44_100, 32_000, 22_050, 8_000] {
            let input: Vec<f32> = (0..rate as usize / 2)
                .map(|i| (i as f32 * 0.37).sin() * 0.4 + (i as f32 * 0.011).cos() * 0.3)
                .collect();
            for chunk in [1, 7, 94, 95, 441, 480, 4800] {
                let mut r = Resampler::new(rate);
                let mut got = Vec::new();
                for part in input.chunks(chunk) {
                    r.process(part, &mut got);
                }
                let want = reference(rate, &input, chunk);
                assert_eq!(got.len(), want.len(), "{rate} Hz, chunks of {chunk}");
                for (g, w) in got.iter().zip(&want) {
                    assert!((g - w).abs() <= f32::EPSILON, "{rate} Hz, chunks of {chunk}: {g} != {w}");
                }
            }
        }
    }

    #[test]
    fn level_maps_loudness() {
        assert_eq!(level(&[]), 0.0);
        assert_eq!(level(&[0; 320]), 0.0);
        assert!(level(&[16_000; 320]) > 0.9);
        assert!(level(&[30; 320]) < 0.2);
    }
}
