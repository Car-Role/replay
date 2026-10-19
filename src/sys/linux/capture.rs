//! Linux screen capture: the XDG desktop portal grants a PipeWire stream of
//! one monitor; GStreamer encodes it (NVENC or VA-API when present).
//!
//! Wayland does not let apps capture single windows without asking each
//! time, so Linux records the chosen monitor while any game runs. The
//! portal's restore token is saved, so the screen picker shows only once on
//! desktops that support it.

use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use ashpd::desktop::PersistMode;
use ashpd::desktop::screencast::{
    CursorMode, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions, SourceType, StartCastOptions,
};
use ashpd::desktop::{CreateSessionOptions, Session};
use gstreamer as gst;
use gstreamer::prelude::*;

use super::gst as g;
use crate::config::{Codec, Config};
use crate::media::VideoState;
use crate::mux::bitstream::{ParamSets, find_param_sets};
use crate::ring::PacketRing;
use crate::sys::unix::app::{GameInfo, Recording, ring_for};

fn token_path() -> std::path::PathBuf {
    crate::paths::data_dir().join("portal-restore-token")
}

/// An open screen-cast session. Dropping it ends the share.
pub struct Portal {
    rt: tokio::runtime::Runtime,
    _proxy: Screencast,
    session: Option<Session<Screencast>>,
    pub fd: OwnedFd,
    pub node: u32,
    pub size: (u32, u32),
}

impl Drop for Portal {
    fn drop(&mut self) {
        if let Some(s) = self.session.take() {
            let _ = self.rt.block_on(s.close());
        }
    }
}

impl Portal {
    pub fn open() -> Result<Self, String> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
        let token = std::fs::read_to_string(token_path()).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        let (proxy, session, fd, node, size, new_token) = rt.block_on(async {
            let proxy = Screencast::new().await.map_err(|e| format!("the screen-cast portal is not available: {e}"))?;
            let session = proxy.create_session(CreateSessionOptions::default()).await.map_err(|e| e.to_string())?;
            let opts = SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::Hidden)
                .set_sources(SourceType::Monitor)
                .set_multiple(false)
                .set_persist_mode(PersistMode::ExplicitlyRevoked)
                .set_restore_token(token.as_deref());
            proxy.select_sources(&session, opts).await.map_err(|e| e.to_string())?;
            let streams = proxy
                .start(&session, None, StartCastOptions::default())
                .await
                .map_err(|e| e.to_string())?
                .response()
                .map_err(|e| format!("screen sharing was not allowed: {e}"))?;
            let stream = streams.streams().first().ok_or("no screen was chosen")?;
            let node = stream.pipe_wire_node_id();
            let size = stream.size().map(|(w, h)| (w.max(2) as u32, h.max(2) as u32)).unwrap_or((1920, 1080));
            let new_token = streams.restore_token().map(String::from);
            let fd = proxy
                .open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default())
                .await
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((proxy, session, fd, node, size, new_token))
        })?;
        if let Some(t) = new_token {
            let _ = std::fs::create_dir_all(crate::paths::data_dir());
            let _ = std::fs::write(token_path(), t);
        }
        log::info!("screen cast: PipeWire node {node}, {}x{}", size.0, size.1);
        Ok(Self { rt, _proxy: proxy, session: Some(session), fd, node, size })
    }
}

pub struct LinuxRecording {
    game: GameInfo,
    video: Arc<Mutex<PacketRing>>,
    state: Arc<Mutex<VideoState>>,
    game_audio: Option<Arc<Mutex<PacketRing>>>,
    audio: Option<super::audio::Capture>,
    failure: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    _portal: Portal,
}

impl LinuxRecording {
    pub fn start(game: &GameInfo, cfg: &Config, portal: Portal) -> Result<Self, String> {
        let v = &cfg.video;
        let mut codec = v.codec;
        let enc = match g::encoder_for(codec) {
            Some(e) => e,
            None => {
                // Fall back to H.264, which every system can encode.
                codec = Codec::H264;
                g::encoder_for(codec).ok_or("no video encoder found; install gstreamer1.0-plugins-ugly (x264) or VA-API drivers")?
            }
        };
        if !g::is_hardware(enc) {
            log::warn!("no hardware encoder for {codec:?}; using {enc} (uses more CPU)");
        }
        let (w, h) = v.resolution.output_size(portal.size.0, portal.size.1);
        let desc = format!(
            "pipewiresrc fd={} path={} do-timestamp=true keepalive-time=1000 ! videoconvert ! videoscale ! videorate ! video/x-raw,format=NV12,width={w},height={h},framerate={}/1,pixel-aspect-ratio=1/1 ! {} ! appsink name=sink sync=false max-buffers=240 drop=true",
            portal.fd.as_raw_fd(),
            portal.node,
            v.fps,
            g::encoder_fragment(enc, codec, cfg)
        );
        let (pipeline, sink) = g::pipeline(&desc)?;
        pipeline.set_state(gst::State::Playing).map_err(|e| format!("cannot start capture: {e}"))?;
        log::info!("recording {} with {enc} at {w}x{h} @{} fps", game.name, v.fps);

        let video = Arc::new(Mutex::new(ring_for(cfg, v.peak_kbps())));
        let state = Arc::new(Mutex::new(VideoState { codec, width: w, height: h, fps: v.fps, params: ParamSets::default(), frames: 0, dropped: 0 }));
        let failure = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (ring, st, fail, s) = (video.clone(), state.clone(), failure.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("video".into())
            .spawn(move || {
                let mut frames = 0u64;
                while !s.load(Ordering::Acquire) {
                    match sink.try_pull_sample(gst::ClockTime::from_mseconds(500)) {
                        Some(sample) => {
                            let Some(b) = sample.buffer() else { continue };
                            let pts = g::ticks(&pipeline, b.pts());
                            let key = !b.flags().contains(gst::BufferFlags::DELTA_UNIT);
                            let Ok(m) = b.map_readable() else { continue };
                            if key {
                                let ps = find_param_sets(codec, m.as_slice());
                                if ps.is_complete(codec)
                                    && let Ok(mut st) = st.lock()
                                {
                                    st.params = ps;
                                }
                            }
                            if let Ok(mut r) = ring.lock() {
                                r.push(pts, key, m.as_slice());
                            }
                            frames += 1;
                            if frames.is_multiple_of(60)
                                && let Ok(mut st) = st.lock()
                            {
                                st.frames = frames;
                            }
                        }
                        None => {
                            if let Some(e) = g::bus_error(&pipeline) {
                                if let Ok(mut f) = fail.lock() {
                                    *f = Some(e);
                                }
                                break;
                            }
                        }
                    }
                }
                let _ = pipeline.set_state(gst::State::Null);
            })
            .map_err(|e| e.to_string())?;

        // Desktop audio (everything the speakers play): per-app capture is not
        // available through PulseAudio, so the "Game" track is the full mix.
        let (game_audio, audio) = if cfg.audio.game_audio {
            let ring = Arc::new(Mutex::new(ring_for(cfg, cfg.audio.bitrate_kbps)));
            match super::audio::capture("@DEFAULT_MONITOR@", cfg.audio.bitrate_kbps, ring.clone()) {
                Ok(c) => (Some(ring), Some(c)),
                Err(e) => {
                    log::warn!("desktop audio: {e}");
                    (None, None)
                }
            }
        } else {
            (None, None)
        };
        Ok(Self { game: game.clone(), video, state, game_audio, audio, failure, stop, thread: Some(thread), _portal: portal })
    }
}

impl Recording for LinuxRecording {
    fn game(&self) -> &GameInfo {
        &self.game
    }
    fn video(&self) -> Arc<Mutex<PacketRing>> {
        self.video.clone()
    }
    fn video_state(&self) -> VideoState {
        self.state.lock().map(|s| s.clone()).expect("state lock")
    }
    fn game_audio(&self) -> Option<Arc<Mutex<PacketRing>>> {
        self.game_audio.clone()
    }
    fn failure(&self) -> Option<String> {
        self.failure.lock().ok().and_then(|f| f.clone())
    }
    fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        if let Some(mut a) = self.audio.take() {
            crate::sys::unix::app::Stop::stop(&mut a);
        }
    }
}

impl Drop for LinuxRecording {
    fn drop(&mut self) {
        Recording::stop(self);
    }
}
