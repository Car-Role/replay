//! Keeps a captured audio stream continuous on the monotonic clock and
//! encodes it to AAC into a packet ring. Gaps become silence, so the track
//! always lines up with the video.

use std::sync::{Arc, Mutex};

use crate::media::{AAC_FRAME, CHANNELS, SAMPLE_RATE};
use crate::ring::{PacketRing, TICKS_PER_SEC, Ticks};
use crate::sys::aac::AacEncoder;

const GAP_TOLERANCE: Ticks = TICKS_PER_SEC * 40 / 1000;
const SILENCE_AFTER: Ticks = TICKS_PER_SEC * 120 / 1000;
const SILENCE_SLACK: Ticks = TICKS_PER_SEC * 60 / 1000;
const MAX_FILL: Ticks = TICKS_PER_SEC * 5;

fn frames_to_ticks(f: u64) -> Ticks {
    (f as i128 * TICKS_PER_SEC as i128 / SAMPLE_RATE as i128) as Ticks
}

fn ticks_to_frames(t: Ticks) -> u64 {
    (t.max(0) as i128 * SAMPLE_RATE as i128 / TICKS_PER_SEC as i128) as u64
}

pub struct Timeline {
    base: Option<Ticks>,
    fed: u64,
    emitted: u64,
    enc: AacEncoder,
    ring: Arc<Mutex<PacketRing>>,
    silence: Vec<i16>,
}

impl Timeline {
    pub fn new(bitrate_kbps: u32, ring: Arc<Mutex<PacketRing>>) -> Result<Self, String> {
        Ok(Self { base: None, fed: 0, emitted: 0, enc: AacEncoder::new(bitrate_kbps)?, ring, silence: Vec::new() })
    }

    fn next_time(&self) -> Option<Ticks> {
        self.base.map(|b| b + frames_to_ticks(self.fed))
    }

    fn feed(&mut self, pcm: &[i16]) -> Result<(), String> {
        let base = self.base.expect("base set before feed");
        self.fed += (pcm.len() / CHANNELS as usize) as u64;
        let ring = &self.ring;
        let emitted = &mut self.emitted;
        self.enc.encode(pcm, |aac| {
            let pts = base + frames_to_ticks(*emitted * AAC_FRAME as u64);
            *emitted += 1;
            if let Ok(mut r) = ring.lock() {
                r.push(pts, true, aac);
            }
        })
    }

    fn feed_silence(&mut self, frames: u64) -> Result<(), String> {
        let n = frames as usize * CHANNELS as usize;
        if self.silence.len() < n {
            self.silence.resize(n, 0);
        }
        let s = std::mem::take(&mut self.silence);
        let r = self.feed(&s[..n]);
        self.silence = s;
        r
    }

    /// Adds interleaved 48 kHz stereo PCM that started at `at`.
    pub fn push(&mut self, at: Ticks, pcm: &[i16]) -> Result<(), String> {
        let Some(next) = self.next_time() else {
            self.base = Some(at);
            return self.feed(pcm);
        };
        let drift = at - next;
        if drift > GAP_TOLERANCE {
            self.feed_silence(ticks_to_frames(drift.min(MAX_FILL)))?;
        } else if drift < -GAP_TOLERANCE {
            let skip = ticks_to_frames(-drift) as usize * CHANNELS as usize;
            if skip >= pcm.len() {
                return Ok(());
            }
            return self.feed(&pcm[skip..]);
        }
        self.feed(pcm)
    }

    /// Call when no data arrived for a while.
    pub fn idle(&mut self, now: Ticks) -> Result<(), String> {
        let Some(next) = self.next_time() else { return Ok(()) };
        if now - next > SILENCE_AFTER {
            self.feed_silence(ticks_to_frames((now - SILENCE_SLACK - next).min(MAX_FILL)))?;
        }
        Ok(())
    }
}

/// Converts any channel count and sample rate to 48 kHz stereo i16 with a
/// linear resampler. Keeps state between calls so chunks join smoothly.
pub struct Converter {
    in_rate: u32,
    in_channels: usize,
    pos: f64,
    last: [f32; 2],
}

impl Converter {
    pub fn new(in_rate: u32, in_channels: usize) -> Self {
        Self { in_rate: in_rate.max(1), in_channels: in_channels.max(1), pos: 0.0, last: [0.0; 2] }
    }

    fn frame(&self, data: &[f32], i: usize) -> [f32; 2] {
        let c = self.in_channels;
        let l = data[i * c];
        let r = if c > 1 { data[i * c + 1] } else { l };
        [l, r]
    }

    /// Appends converted samples from interleaved f32 input to `out`.
    pub fn process(&mut self, data: &[f32], out: &mut Vec<i16>) {
        let frames = data.len() / self.in_channels;
        if frames == 0 {
            return;
        }
        let step = f64::from(self.in_rate) / f64::from(SAMPLE_RATE);
        let to_i16 = |v: f32| (v.clamp(-1.0, 1.0) * 32767.0) as i16;
        // Position `pos` is relative to the frame before this chunk (`last`).
        while self.pos < frames as f64 {
            let i = self.pos.floor() as isize - 1;
            let frac = (self.pos - self.pos.floor()) as f32;
            let a = if i < 0 { self.last } else { self.frame(data, i as usize) };
            let b = self.frame(data, (i + 1) as usize);
            out.push(to_i16(a[0] + (b[0] - a[0]) * frac));
            out.push(to_i16(a[1] + (b[1] - a[1]) * frac));
            self.pos += step;
        }
        self.pos -= frames as f64;
        self.last = self.frame(data, frames - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converter_keeps_rate_and_channels() {
        let mut c = Converter::new(48_000, 1);
        let mut out = Vec::new();
        c.process(&vec![0.5f32; 480], &mut out);
        assert_eq!(out.len(), 960);
        assert!(out[10..].iter().all(|&v| (v - 16383).abs() <= 1));

        let mut c = Converter::new(44_100, 2);
        let mut out = Vec::new();
        for _ in 0..100 {
            c.process(&vec![0.0f32; 441 * 2], &mut out);
        }
        // 1 s of 44.1 kHz input gives 1 s of 48 kHz output (within one frame).
        assert!((out.len() as i64 / 2 - 48_000).abs() <= 1, "{}", out.len());
    }
}
