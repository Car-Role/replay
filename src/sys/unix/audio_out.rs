//! Preview audio for the library window, through cpal. The clip's tracks
//! are mixed live with the volume sliders and resampled to the device rate.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::media::{AudioTracks, CHANNELS, SAMPLE_RATE};

struct Shared {
    playing: AtomicBool,
    /// Position in 48 kHz frames, scaled by 1000 for sub-frame resampling steps.
    pos_milli: AtomicU64,
    game_gain: AtomicU32,
    mic_gain: AtomicU32,
}

pub struct AudioOut {
    shared: Arc<Shared>,
    _stream: Option<Mutex<cpal::Stream>>,
    pub frames: u64,
}

// SAFETY: the stream handle is only kept alive here and never used across threads.
unsafe impl Send for AudioOut {}

impl AudioOut {
    pub fn start(tracks: AudioTracks) -> Self {
        let frames = tracks.game.as_ref().or(tracks.mic.as_ref()).map(|v| (v.len() / CHANNELS as usize) as u64).unwrap_or(0);
        let shared = Arc::new(Shared {
            playing: AtomicBool::new(false),
            pos_milli: AtomicU64::new(0),
            game_gain: AtomicU32::new(100),
            mic_gain: AtomicU32::new(100),
        });
        let stream = match open(shared.clone(), tracks, frames) {
            Ok(s) => Some(Mutex::new(s)),
            Err(e) => {
                log::warn!("preview audio unavailable: {e}");
                None
            }
        };
        // Without a device the position must still advance; report no frames
        // so the editor uses its wall clock instead.
        let frames = if stream.is_some() { frames } else { 0 };
        Self { shared, _stream: stream, frames }
    }

    pub fn set_playing(&self, on: bool) {
        self.shared.playing.store(on, Ordering::Release);
    }

    pub fn seek(&self, seconds: f64) {
        let f = (seconds.max(0.0) * f64::from(SAMPLE_RATE) * 1000.0) as u64;
        self.shared.pos_milli.store(f, Ordering::Release);
    }

    pub fn set_gains(&self, game: u32, mic: u32) {
        self.shared.game_gain.store(game, Ordering::Relaxed);
        self.shared.mic_gain.store(mic, Ordering::Relaxed);
    }

    pub fn position(&self) -> f64 {
        self.shared.pos_milli.load(Ordering::Acquire) as f64 / 1000.0 / f64::from(SAMPLE_RATE)
    }
}

fn open(shared: Arc<Shared>, tracks: AudioTracks, total: u64) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("no audio output device")?;
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    let config = supported.config();
    let out_ch = config.channels as usize;
    // Input frames per output frame, times 1000.
    let step = (f64::from(SAMPLE_RATE) / f64::from(config.sample_rate) * 1000.0) as u64;
    let sample = move |i: usize, track: &Option<Vec<i16>>| track.as_ref().and_then(|v| v.get(i)).copied().unwrap_or(0) as i32;
    let callback = move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
        if !shared.playing.load(Ordering::Acquire) {
            out.fill(0.0);
            return;
        }
        let gg = shared.game_gain.load(Ordering::Relaxed) as i32;
        let mg = shared.mic_gain.load(Ordering::Relaxed) as i32;
        let mut pos = shared.pos_milli.load(Ordering::Acquire);
        for frame in out.chunks_mut(out_ch) {
            let f = (pos / 1000) as usize;
            if f as u64 >= total {
                frame.fill(0.0);
                continue;
            }
            for (c, slot) in frame.iter_mut().enumerate() {
                let i = f * CHANNELS as usize + c.min(1);
                let v = (sample(i, &tracks.game) * gg + sample(i, &tracks.mic) * mg) / 100;
                *slot = v.clamp(i16::MIN as i32, i16::MAX as i32) as f32 / 32768.0;
            }
            pos += step;
        }
        shared.pos_milli.store(pos, Ordering::Release);
    };
    let stream = device
        .build_output_stream::<f32, _, _>(config, callback, |e| log::warn!("audio output error: {e}"), None)
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}
