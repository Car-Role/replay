//! One recording session per game: capture -> convert -> encode -> ring,
//! plus the game's own audio. Each session has a video thread and an audio
//! thread; both sleep while no frames or audio arrive.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Media::MediaFoundation::IMFSample;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Threading::THREAD_PRIORITY_ABOVE_NORMAL;
use windows::core::Result;

use crate::audio;
use crate::capture::{Capture, Frame, Target};
use crate::config::{Codec, Config};
use crate::encoder::{EncoderEvent, EncoderSettings, VideoEncoder, texture_sample};
use crate::gpu::{Converter, Gpu};
use crate::mux::bitstream::{ParamSets, find_param_sets};
use crate::ring::{PacketRing, TICKS_PER_SEC, Ticks};

const SLOTS: usize = 6;

#[derive(Clone, Debug)]
pub struct GameInfo {
    pub pid: u32,
    pub hwnd: isize,
    pub exe: String,
    pub name: String,
}

pub use crate::media::VideoState;

pub enum SessionEnd {
    WindowClosed,
    Failed(String),
}

enum Msg {
    Frame(Frame),
    Enc(EncoderEvent),
    Closed,
    Stop,
}

pub struct Session {
    pub game: GameInfo,
    pub video: Arc<Mutex<PacketRing>>,
    pub video_state: Arc<Mutex<VideoState>>,
    pub game_audio: Option<Arc<Mutex<PacketRing>>>,
    /// Seconds a saved replay of this game holds.
    pub replay_seconds: u32,
    pub ended: Option<Instant>,
    stop: Arc<AtomicBool>,
    tx: Sender<Msg>,
    threads: Vec<JoinHandle<()>>,
}

pub fn ring_for(replay_seconds: u32, kbps: u32) -> PacketRing {
    // Keep one extra GOP plus margin so a full-length clip is always available.
    let secs = i64::from(replay_seconds) + 2;
    let bytes = (u64::from(kbps) * 1000 / 8) * (secs as u64 + 3) * 5 / 4;
    PacketRing::new(secs * TICKS_PER_SEC, bytes as usize)
}

impl Session {
    pub fn start(
        gpu: Arc<Gpu>,
        game: GameInfo,
        target: Target,
        cfg: &Config,
        out_size: (u32, u32),
        on_end: impl Fn(u32, SessionEnd) + Send + 'static,
    ) -> Self {
        let v = &cfg.video;
        let settings = EncoderSettings {
            codec: v.codec,
            width: out_size.0,
            height: out_size.1,
            fps: v.fps,
            bitrate: v.bitrate_kbps * 1000,
            peak_bitrate: v.peak_kbps() * 1000,
            rate_control: v.rate_control,
            // Keyframes are forced every 0.5 s of real time (see the worker);
            // this GOP is only the fallback for encoders that ignore that.
            gop: (v.fps * 2).max(1),
            low_latency: true,
        };
        let video = Arc::new(Mutex::new(ring_for(cfg.replay_seconds, v.peak_kbps())));
        let video_state = Arc::new(Mutex::new(VideoState {
            codec: v.codec,
            width: out_size.0,
            height: out_size.1,
            fps: v.fps,
            params: ParamSets::default(),
            frames: 0,
            dropped: 0,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let mut threads = Vec::new();

        let worker = VideoWorker {
            gpu,
            target,
            settings,
            cursor: v.capture_cursor,
            ring: video.clone(),
            state: video_state.clone(),
            tx: tx.clone(),
        };
        let pid = game.pid;
        let name = game.name.clone();
        threads.push(
            std::thread::Builder::new()
                .name(format!("video-{pid}"))
                .spawn(move || {
                    crate::util::set_thread("replay video", THREAD_PRIORITY_ABOVE_NORMAL);
                    unsafe {
                        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                    }
                    let end = match worker.run(rx) {
                        Ok(true) => Some(SessionEnd::WindowClosed),
                        Ok(false) => None,
                        Err(e) => Some(SessionEnd::Failed(e.to_string())),
                    };
                    unsafe { CoUninitialize() };
                    if let Some(end) = end {
                        if let SessionEnd::Failed(e) = &end {
                            log::error!("{name}: video stopped: {e}");
                        }
                        on_end(pid, end);
                    }
                })
                .expect("spawn video thread"),
        );

        let game_audio = cfg.audio.game_audio.then(|| {
            let ring = Arc::new(Mutex::new(ring_for(cfg.replay_seconds, cfg.audio.bitrate_kbps)));
            let (r, s, kbps) = (ring.clone(), stop.clone(), cfg.audio.bitrate_kbps);
            threads.push(
                std::thread::Builder::new()
                    .name(format!("audio-{pid}"))
                    .spawn(move || audio::run(audio::Source::Process(pid), kbps, r, s))
                    .expect("spawn audio thread"),
            );
            ring
        });

        log::info!("recording {} ({}, pid {pid}) at {}x{} {} fps", game.name, game.exe, out_size.0, out_size.1, v.fps);
        Self { game, video, video_state, game_audio, replay_seconds: cfg.replay_seconds, ended: None, stop, tx, threads }
    }

    pub fn is_running(&self) -> bool {
        self.ended.is_none()
    }

    /// Stops the threads. The rings stay readable so a clip can still be saved.
    pub fn stop(&mut self) {
        if self.ended.is_some() {
            return;
        }
        self.ended = Some(Instant::now());
        self.stop.store(true, Ordering::Release);
        let _ = self.tx.send(Msg::Stop);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        if let Ok(s) = self.video_state.lock() {
            log::info!("stopped {}: {} frames, {} dropped", self.game.name, s.frames, s.dropped);
        }
    }

    pub fn memory_bytes(&self) -> usize {
        let v = self.video.lock().map(|r| r.bytes()).unwrap_or(0);
        let a = self.game_audio.as_ref().and_then(|r| r.lock().ok().map(|r| r.bytes())).unwrap_or(0);
        v + a
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

struct VideoWorker {
    gpu: Arc<Gpu>,
    target: Target,
    settings: EncoderSettings,
    cursor: bool,
    ring: Arc<Mutex<PacketRing>>,
    state: Arc<Mutex<VideoState>>,
    tx: Sender<Msg>,
}

struct Pending {
    slot: usize,
    pts: Ticks,
}

impl VideoWorker {
    fn start_capture(&self, target: Target) -> Result<Capture> {
        let (tf, tc) = (self.tx.clone(), self.tx.clone());
        Capture::start(
            &self.gpu,
            target,
            self.settings.fps,
            self.cursor,
            move |f| {
                let _ = tf.send(Msg::Frame(f));
            },
            move || {
                let _ = tc.send(Msg::Closed);
            },
        )
    }

    /// Returns Ok(true) when the window closed, Ok(false) when stopped.
    fn run(self, rx: Receiver<Msg>) -> Result<bool> {
        let s = &self.settings;
        let frame_ticks = TICKS_PER_SEC / i64::from(s.fps);
        let textures = (0..SLOTS).map(|_| self.gpu.create_nv12(s.width, s.height)).collect::<Result<Vec<_>>>()?;
        let samples = textures.iter().map(texture_sample).collect::<Result<Vec<IMFSample>>>()?;

        let tx = self.tx.clone();
        let mut enc = VideoEncoder::new(&self.gpu, s, move |e| {
            let _ = tx.send(Msg::Enc(e));
        })?;
        let mut capture = self.start_capture(self.target)?;
        let started = Instant::now();
        let mut switched_to_monitor = false;

        let mut conv: Option<Converter> = None;
        let mut free: Vec<usize> = (0..SLOTS).collect();
        let mut inflight: VecDeque<usize> = VecDeque::new();
        let mut pending: Option<Pending> = None;
        let mut credits = 0u32;
        let mut t0: Option<Ticks> = None;
        let mut last_slot = -1i64;
        let mut frames = 0u64;
        let mut dropped = 0u64;
        let mut fallback_params = false;
        let mut crop: Option<(u32, u32, u32, u32)> = None;
        let mut crop_age = u32::MAX;
        let key_interval = TICKS_PER_SEC / 2;
        let mut last_key: Option<Ticks> = None;
        let mut force_ok = true;

        loop {
            let msg = match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => {
                    // Some exclusive-fullscreen games give no window frames.
                    // Fall back to capturing their monitor.
                    if frames == 0 && !switched_to_monitor && started.elapsed() > Duration::from_secs(4)
                        && let Target::Window(h) = self.target
                        && let Some(mon) = crate::detect::fullscreen_monitor(h)
                    {
                        log::warn!("no window frames; capturing the monitor instead");
                        capture = self.start_capture(Target::Monitor(mon))?;
                        switched_to_monitor = true;
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return Ok(false),
            };
            match msg {
                Msg::Stop => return Ok(false),
                Msg::Closed => return Ok(true),
                Msg::Enc(EncoderEvent::Error(hr)) => {
                    return Err(windows::core::Error::new(hr, "encoder error"));
                }
                Msg::Enc(EncoderEvent::NeedInput) => credits += 1,
                Msg::Enc(EncoderEvent::DrainComplete) => {}
                Msg::Enc(EncoderEvent::HaveOutput) => {
                    let base = t0.unwrap_or(0);
                    let ring = &self.ring;
                    let codec = s.codec;
                    let state = &self.state;
                    enc.take_output(|f| {
                        if f.key
                            && let Ok(mut st) = state.lock()
                        {
                            let ps = find_param_sets(codec, f.data);
                            if ps.is_complete(codec) && ps != st.params {
                                st.params = ps;
                            }
                        }
                        if let Ok(mut r) = ring.lock() {
                            r.push(base + f.time, f.key, f.data);
                        }
                    })?;
                    if let Some(slot) = inflight.pop_front() {
                        free.push(slot);
                    }
                    if !fallback_params {
                        fallback_params = true;
                        self.seed_params_from_media_type(&enc);
                    }
                }
                Msg::Frame(frame) => {
                    let t = frame.time()?;
                    let base = *t0.get_or_insert(t);
                    let k = ((t - base) as f64 / frame_ticks as f64).round() as i64;
                    if k <= last_slot {
                        continue; // Faster than the target rate.
                    }
                    let tex = frame.texture()?;
                    let mut desc = Default::default();
                    unsafe { tex.GetDesc(&mut desc) };
                    let content = frame.content_size()?;
                    if capture.ensure_size(content.0, content.1)? {
                        log::debug!("source resized to {}x{}", content.0, content.1);
                    }
                    if conv.as_ref().map(|c| c.in_size()) != Some((desc.Width, desc.Height)) {
                        conv = Some(Converter::new(
                            self.gpu.clone(),
                            (desc.Width, desc.Height),
                            (s.width, s.height),
                            &textures,
                        )?);
                    }
                    // Reuse a slot that waits for the encoder, or a free one.
                    let slot = match pending.take() {
                        Some(p) => Some(p.slot),
                        None => free.pop(),
                    };
                    let slot = match slot {
                        Some(sl) => sl,
                        None if inflight.len() >= SLOTS - 1 => {
                            // The encoder never returned these; take the oldest back.
                            inflight.pop_front().expect("non-empty")
                        }
                        None => {
                            dropped += 1;
                            continue;
                        }
                    };
                    let shown = (content.0.min(desc.Width), content.1.min(desc.Height));
                    // Window targets: record the client area only. Re-check
                    // twice a second; window moves and resizes are rare.
                    let target_window = match (self.target, switched_to_monitor) {
                        (Target::Window(h), false) => Some(h),
                        _ => None,
                    };
                    crop_age = crop_age.saturating_add(1);
                    if crop_age > self.settings.fps / 2 {
                        crop_age = 0;
                        crop = target_window.and_then(crate::detect::client_crop);
                    }
                    let rect = match crop {
                        Some((x, y, w, h)) if x + w <= shown.0 && y + h <= shown.1 => (x, y, w, h),
                        _ => (0, 0, shown.0, shown.1),
                    };
                    conv.as_mut().expect("converter").convert(&tex, rect, slot)?;
                    drop(frame);
                    last_slot = k;
                    pending = Some(Pending { slot, pts: k * frame_ticks });
                    frames += 1;
                    if frames.is_multiple_of(60)
                        && let Ok(mut st) = self.state.lock()
                    {
                        st.frames = frames;
                        st.dropped = dropped;
                    }
                }
            }
            if credits > 0
                && let Some(p) = pending.take()
            {
                let sample = &samples[p.slot];
                // Time-based keyframes keep cut points every 0.5 s even when
                // the game draws fewer frames (menus, pauses, low fps).
                if force_ok && last_key.is_none_or(|k| p.pts - k >= key_interval) {
                    force_ok = enc.force_keyframe() || last_key.is_none();
                    last_key = Some(p.pts);
                }
                unsafe {
                    sample.SetSampleTime(p.pts)?;
                    sample.SetSampleDuration(frame_ticks)?;
                }
                enc.submit(sample)?;
                credits -= 1;
                inflight.push_back(p.slot);
            }
        }
    }

    /// Some encoders only put parameter sets in the media type.
    fn seed_params_from_media_type(&self, enc: &VideoEncoder) {
        let Some(blob) = enc.sequence_header() else { return };
        let codec = self.settings.codec;
        let ps = match codec {
            Codec::Av1 if blob.first() == Some(&0x81) && blob.len() > 4 => find_param_sets(codec, &blob[4..]),
            _ => find_param_sets(codec, &blob),
        };
        if let Ok(mut st) = self.state.lock()
            && !st.params.is_complete(codec)
            && ps.is_complete(codec)
        {
            log::debug!("parameter sets taken from the encoder media type");
            st.params = ps;
        }
    }
}
