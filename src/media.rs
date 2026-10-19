//! Media types shared by every platform backend.

use std::time::Instant;

use crate::config::Codec;
use crate::mux::bitstream::ParamSets;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
/// PCM frames per AAC-LC frame.
pub const AAC_FRAME: usize = 1024;

/// What the muxer needs to know about a recorded video stream.
#[derive(Clone, Debug)]
pub struct VideoState {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub params: ParamSets,
    pub frames: u64,
    pub dropped: u64,
}

/// Mixes `src` into `dst` with gains in percent, saturating.
pub fn mix_into(dst: &mut [i16], src: &[i16], dst_gain: u32, src_gain: u32) {
    let g1 = dst_gain as i32;
    let g2 = src_gain as i32;
    for (d, s) in dst.iter_mut().zip(src.iter()) {
        let v = (i32::from(*d) * g1 + i32::from(*s) * g2) / 100;
        *d = v.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    }
}

/// A decoded preview frame (RGBA, tightly packed).
pub struct Frame {
    pub generation: u64,
    pub pts: f64,
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// Fully decoded clip audio for preview playback (48 kHz stereo).
pub struct AudioTracks {
    pub game: Option<Vec<i16>>,
    pub mic: Option<Vec<i16>>,
    pub voice: Option<Vec<i16>>,
}

/// Wall clock for clips without audio (or when no output device exists).
pub struct WallClock {
    base: f64,
    since: Option<Instant>,
}

impl Default for WallClock {
    fn default() -> Self {
        Self::new()
    }
}

impl WallClock {
    pub fn new() -> Self {
        Self { base: 0.0, since: None }
    }
    pub fn position(&self) -> f64 {
        self.base + self.since.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0)
    }
    pub fn set_playing(&mut self, on: bool) {
        if on && self.since.is_none() {
            self.since = Some(Instant::now());
        } else if !on && let Some(t) = self.since.take() {
            self.base += t.elapsed().as_secs_f64();
        }
    }
    pub fn seek(&mut self, s: f64) {
        self.base = s;
        if self.since.is_some() {
            self.since = Some(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixing_saturates() {
        let mut a = [30000i16, -30000, 100];
        mix_into(&mut a, &[30000, -30000, 100], 100, 100);
        assert_eq!(a, [i16::MAX, i16::MIN, 200]);
        let mut b = [1000i16];
        mix_into(&mut b, &[1000], 50, 200);
        assert_eq!(b, [2500]);
    }
}
