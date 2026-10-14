//! Real-time multi-track playback through cpal.
//!
//! The audio callback owns the playhead and mixes every track sample-accurately,
//! so stems can never drift apart. The UI side only swaps in new track lists
//! and gain targets (via `ArcSwap`) and reads the published position.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use arc_swap::ArcSwap;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use crate::model::AudioBuffer;

use super::PlaybackEngine;

struct Shared {
    tracks: ArcSwap<Vec<Arc<AudioBuffer>>>,
    /// Target (left, right) gain per track, master and pan included.
    gains: ArcSwap<Vec<[f32; 2]>>,
    playing: AtomicBool,
    /// Pending seek in seconds (f64 bits), NaN when none.
    seek_to: AtomicU64,
    /// Last published playhead in seconds (f64 bits).
    pos: AtomicU64,
    /// When `pos` was published, in ns since `epoch`.
    stamp_ns: AtomicU64,
    /// Loop region (f64 bits), NaN when off.
    loop_start: AtomicU64,
    loop_end: AtomicU64,
    epoch: Instant,
}

impl Shared {
    fn now_ns(&self) -> u64 {
        self.epoch.elapsed().as_nanos() as u64
    }
    fn loop_region(&self) -> Option<(f64, f64)> {
        let a = f64::from_bits(self.loop_start.load(Ordering::Acquire));
        let b = f64::from_bits(self.loop_end.load(Ordering::Acquire));
        (a.is_finite() && b.is_finite() && b > a).then_some((a, b))
    }
    fn publish(&self, t: f64) {
        self.pos.store(t.to_bits(), Ordering::Release);
        self.stamp_ns.store(self.now_ns(), Ordering::Release);
    }
}

pub struct CpalPlayback {
    shared: Arc<Shared>,
    _stream: cpal::Stream,
    device_name: String,
    device_rate: u32,
}

impl CpalPlayback {
    pub fn new() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("No audio output device found")?;
        let config = device.default_output_config().map_err(|e| format!("Audio device error: {e}"))?;
        let device_name = device.to_string();
        let device_rate = config.sample_rate();

        let shared = Arc::new(Shared {
            tracks: ArcSwap::from_pointee(Vec::new()),
            gains: ArcSwap::from_pointee(Vec::new()),
            playing: AtomicBool::new(false),
            seek_to: AtomicU64::new(f64::NAN.to_bits()),
            pos: AtomicU64::new(0f64.to_bits()),
            stamp_ns: AtomicU64::new(0),
            loop_start: AtomicU64::new(f64::NAN.to_bits()),
            loop_end: AtomicU64::new(f64::NAN.to_bits()),
            epoch: Instant::now(),
        });

        let format = config.sample_format();
        let config: cpal::StreamConfig = config.into();
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, &config, shared.clone()),
            SampleFormat::I16 => build::<i16>(&device, &config, shared.clone()),
            SampleFormat::U16 => build::<u16>(&device, &config, shared.clone()),
            SampleFormat::I32 => build::<i32>(&device, &config, shared.clone()),
            other => return Err(format!("Unsupported output sample format {other}")),
        }?;
        stream.play().map_err(|e| format!("Couldn't start audio: {e}"))?;

        Ok(Self { shared, _stream: stream, device_name, device_rate })
    }
}

fn build<T>(device: &cpal::Device, config: &cpal::StreamConfig, shared: Arc<Shared>) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let mut mixer = Mixer::new(config.sample_rate);
    let mut scratch: Vec<f32> = Vec::new();
    device
        .build_output_stream(
            *config,
            move |out: &mut [T], _| {
                let frames = out.len() / channels;
                if scratch.len() < frames * 2 {
                    scratch.resize(frames * 2, 0.0);
                }
                let buf = &mut scratch[..frames * 2];
                mixer.render(&shared, buf);
                for (frame, lr) in out.chunks_mut(channels).zip(buf.chunks(2)) {
                    match channels {
                        1 => frame[0] = T::from_sample((lr[0] + lr[1]) * 0.5),
                        _ => {
                            frame[0] = T::from_sample(lr[0]);
                            frame[1] = T::from_sample(lr[1]);
                            for s in &mut frame[2..] {
                                *s = T::from_sample(0.0);
                            }
                        }
                    }
                }
            },
            |err| eprintln!("audio stream error: {err}"),
            None,
        )
        .map_err(|e| format!("Couldn't open audio output: {e}"))
}

/// Callback-side state. Pure enough to unit-test without a device.
struct Mixer {
    rate: f64,
    t: f64,
    current: Vec<[f32; 2]>,
    /// Per-sample smoothing coefficient for ~5 ms gain ramps.
    ramp: f32,
}

impl Mixer {
    fn new(rate: u32) -> Self {
        Self {
            rate: rate as f64,
            t: 0.0,
            current: Vec::new(),
            ramp: 1.0 - (-1.0 / (0.005 * rate as f32)).exp(),
        }
    }

    fn render(&mut self, shared: &Shared, out: &mut [f32]) {
        let seek = f64::from_bits(shared.seek_to.swap(f64::NAN.to_bits(), Ordering::AcqRel));
        if !seek.is_nan() {
            self.t = seek;
        }
        out.fill(0.0);
        if !shared.playing.load(Ordering::Acquire) {
            shared.publish(self.t);
            return;
        }
        let tracks = shared.tracks.load();
        let gains = shared.gains.load();
        self.mix(&tracks, &gains, shared.loop_region(), out);
        shared.publish(self.t);
    }

    fn mix(&mut self, tracks: &[Arc<AudioBuffer>], gains: &[[f32; 2]], region: Option<(f64, f64)>, out: &mut [f32]) {
        if self.current.len() != tracks.len() {
            // Only happens when the track list changes.
            self.current = gains.iter().take(tracks.len()).copied().collect();
            self.current.resize(tracks.len(), [0.0; 2]);
        }
        let dt = 1.0 / self.rate;
        for frame in out.chunks_mut(2) {
            let (mut l, mut r) = (0.0f32, 0.0f32);
            for (i, buf) in tracks.iter().enumerate() {
                let target = gains.get(i).copied().unwrap_or([0.0; 2]);
                let g = &mut self.current[i];
                g[0] += (target[0] - g[0]) * self.ramp;
                g[1] += (target[1] - g[1]) * self.ramp;
                if g[0] < 1e-5 && g[1] < 1e-5 {
                    continue;
                }
                let (sl, sr) = sample_at(buf, self.t);
                l += sl * g[0];
                r += sr * g[1];
            }
            frame[0] = l.clamp(-1.0, 1.0);
            frame[1] = r.clamp(-1.0, 1.0);
            self.t += dt;
            if let Some((a, b)) = region
                && self.t >= b
                && self.t < b + 0.5
            {
                self.t = a + (self.t - b);
            }
        }
    }
}

/// Linear-interpolated stereo sample at time `t` (handles any source rate).
fn sample_at(buf: &AudioBuffer, t: f64) -> (f32, f32) {
    let pos = t * buf.sample_rate as f64;
    if pos < 0.0 {
        return (0.0, 0.0);
    }
    let i = pos as usize;
    let frac = (pos - i as f64) as f32;
    let d = &buf.data;
    let Some(a) = d.get(i * 2..i * 2 + 2) else { return (0.0, 0.0) };
    let b = d.get(i * 2 + 2..i * 2 + 4).unwrap_or(a);
    (a[0] + (b[0] - a[0]) * frac, a[1] + (b[1] - a[1]) * frac)
}

/// Constant-power pan law, normalised so centre = unity on both sides.
pub fn pan_gains(gain: f32, pan: f32) -> [f32; 2] {
    let angle = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    let norm = std::f32::consts::SQRT_2;
    [gain * angle.cos() * norm, gain * angle.sin() * norm]
}

impl PlaybackEngine for CpalPlayback {
    fn play(&mut self) {
        self.shared.playing.store(true, Ordering::Release);
    }
    fn pause(&mut self) {
        self.shared.playing.store(false, Ordering::Release);
    }
    fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Acquire)
    }
    fn seek(&mut self, seconds: f64) {
        let t = seconds.max(0.0);
        self.shared.seek_to.store(t.to_bits(), Ordering::Release);
        self.shared.publish(t);
    }
    fn position(&self) -> f64 {
        let s = &self.shared;
        let pending = f64::from_bits(s.seek_to.load(Ordering::Acquire));
        if !pending.is_nan() {
            return pending;
        }
        let pos = f64::from_bits(s.pos.load(Ordering::Acquire));
        if !self.is_playing() {
            return pos;
        }
        // Extrapolate between callbacks so the playhead moves smoothly.
        let since = s.now_ns().saturating_sub(s.stamp_ns.load(Ordering::Acquire)) as f64 / 1e9;
        let t = pos + since.min(0.1);
        match s.loop_region() {
            Some((a, b)) if pos < b && t >= b => a + (t - b),
            _ => t,
        }
    }
    fn set_loop(&mut self, region: Option<(f64, f64)>) {
        let (a, b) = region.unwrap_or((f64::NAN, f64::NAN));
        self.shared.loop_start.store(a.to_bits(), Ordering::Release);
        self.shared.loop_end.store(b.to_bits(), Ordering::Release);
    }
    fn set_tracks(&mut self, tracks: Vec<Arc<AudioBuffer>>) {
        self.shared.tracks.store(Arc::new(tracks));
    }
    fn set_mix(&mut self, gains: &[f32], pans: &[f32], master: f32) {
        let lr = gains
            .iter()
            .zip(pans.iter().chain(std::iter::repeat(&0.0)))
            .map(|(&g, &p)| pan_gains(g * master, p))
            .collect();
        self.shared.gains.store(Arc::new(lr));
    }
    fn describe(&self) -> String {
        format!("{} · {:.1} kHz", self.device_name, self.device_rate as f32 / 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant(v: f32, frames: usize) -> Arc<AudioBuffer> {
        Arc::new(AudioBuffer { sample_rate: 100, data: vec![v; frames * 2] })
    }

    #[test]
    fn pan_law_centre_is_unity() {
        let [l, r] = pan_gains(1.0, 0.0);
        assert!((l - 1.0).abs() < 1e-5 && (r - 1.0).abs() < 1e-5);
        let [l, r] = pan_gains(1.0, -1.0);
        assert!(r.abs() < 1e-5 && l > 1.0);
    }

    #[test]
    fn mixes_tracks_with_gains_and_advances_clock() {
        let mut m = Mixer::new(100);
        m.ramp = 1.0; // no smoothing in tests
        let tracks = vec![constant(0.25, 100), constant(0.5, 100)];
        let gains = vec![[1.0, 1.0], [0.0, 0.0]]; // second track muted
        let mut out = vec![0.0; 20];
        m.mix(&tracks, &gains, None, &mut out);
        assert!(out.iter().all(|s| (s - 0.25).abs() < 1e-6));
        assert!((m.t - 0.1).abs() < 1e-9);
    }

    #[test]
    fn loop_region_wraps_sample_accurately() {
        let mut m = Mixer::new(100);
        m.t = 0.25;
        let mut out = vec![0.0; 2 * 10]; // 0.1 s
        m.mix(&[constant(0.5, 100)], &[[1.0, 1.0]], Some((0.1, 0.3)), &mut out);
        assert!((m.t - 0.15).abs() < 1e-9, "wrapped back to the loop start: {}", m.t);
        // Far past the end (user seeked out of the cue): no wrap.
        m.t = 0.9;
        m.mix(&[constant(0.5, 100)], &[[1.0, 1.0]], Some((0.1, 0.3)), &mut out);
        assert!((m.t - 1.0).abs() < 1e-9);
    }

    #[test]
    fn silent_past_end_of_buffer() {
        let mut m = Mixer::new(100);
        m.ramp = 1.0;
        m.t = 5.0;
        let mut out = vec![1.0; 4];
        m.mix(&[constant(0.5, 10)], &[[1.0, 1.0]], None, &mut out);
        assert!(out.iter().all(|&s| s == 0.0));
    }
}
