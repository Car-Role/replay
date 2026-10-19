//! Preview video for the library window, decoded with GStreamer.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;

use super::gst as g;
use crate::media::Frame;

pub use crate::sys::unix::audio_out::AudioOut;

enum Cmd {
    Seek(f64, u64),
    Stop,
}

pub struct VideoDecoder {
    cmd: Sender<Cmd>,
    pub frames: Receiver<Frame>,
    thread: Option<JoinHandle<()>>,
}

const MAX_PREVIEW_WIDTH: u32 = 1280;

impl VideoDecoder {
    pub fn open(path: &Path, wake: impl Fn() + Send + 'static) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (frame_tx, frame_rx) = mpsc::sync_channel(3);
        let path = path.to_path_buf();
        let thread = std::thread::Builder::new()
            .name("preview video".into())
            .spawn(move || {
                if let Err(e) = decode_loop(&path, &cmd_rx, &frame_tx, &wake) {
                    log::warn!("preview of {} failed: {e}", path.display());
                }
            })
            .ok();
        Self { cmd: cmd_tx, frames: frame_rx, thread }
    }

    pub fn seek(&self, seconds: f64, generation: u64) {
        let _ = self.cmd.send(Cmd::Seek(seconds, generation));
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        let _ = self.cmd.send(Cmd::Stop);
        while self.frames.try_recv().is_ok() {}
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn preview_size(path: &Path) -> (u32, u32) {
    let (sw, sh) = crate::mux::reader::read(path)
        .ok()
        .and_then(|m| match m.video()?.kind {
            crate::mux::reader::TrackKindIn::Video { width, height, .. } => Some((width, height)),
            _ => None,
        })
        .unwrap_or((1920, 1080));
    let w = sw.min(MAX_PREVIEW_WIDTH).max(2) & !1;
    let h = ((u64::from(sh) * u64::from(w) / u64::from(sw.max(1))) as u32).max(2) & !1;
    (w, h)
}

fn decode_loop(path: &Path, cmd: &Receiver<Cmd>, out: &SyncSender<Frame>, wake: &dyn Fn()) -> Result<(), String> {
    let (w, h) = preview_size(path);
    let location = path.display().to_string().replace('"', "\\\"");
    let desc = format!(
        "filesrc location=\"{location}\" ! qtdemux name=d d.video_0 ! queue ! decodebin ! videoconvert ! videoscale ! video/x-raw,format=RGBA,width={w},height={h},pixel-aspect-ratio=1/1 ! appsink name=sink sync=false max-buffers=3 drop=false"
    );
    let (pipeline, sink) = g::pipeline(&desc)?;
    pipeline.set_state(gst::State::Paused).map_err(|e| e.to_string())?;
    let _ = pipeline.state(gst::ClockTime::from_seconds(5));
    pipeline.set_state(gst::State::Playing).map_err(|e| e.to_string())?;

    let mut generation = 0u64;
    let mut skip_until = 0.0f64;
    let seek = |s: f64| {
        let _ = pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::ClockTime::from_nseconds((s.max(0.0) * 1e9) as u64),
        );
    };
    loop {
        match cmd.try_recv() {
            Ok(Cmd::Stop) | Err(mpsc::TryRecvError::Disconnected) => break,
            Ok(Cmd::Seek(s, gen_)) => {
                generation = gen_;
                skip_until = s - 0.001;
                seek(s);
                continue;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(50)) else {
            // End of stream or nothing yet: wait for a command.
            match cmd.recv_timeout(Duration::from_millis(50)) {
                Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(Cmd::Seek(s, gen_)) => {
                    generation = gen_;
                    skip_until = s - 0.001;
                    seek(s);
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            continue;
        };
        let Some(b) = sample.buffer() else { continue };
        let pts = b.pts().map(|p| p.nseconds() as f64 / 1e9).unwrap_or(0.0);
        if pts < skip_until {
            continue;
        }
        let Ok(m) = b.map_readable() else { continue };
        let mut frame = Frame { generation, pts, width: w as usize, height: h as usize, rgba: m.as_slice().to_vec() };
        drop(m);
        loop {
            match out.try_send(frame) {
                Ok(()) => {
                    wake();
                    break;
                }
                Err(TrySendError::Disconnected(_)) => {
                    let _ = pipeline.set_state(gst::State::Null);
                    return Ok(());
                }
                Err(TrySendError::Full(f)) => match cmd.recv_timeout(Duration::from_millis(8)) {
                    Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => {
                        let _ = pipeline.set_state(gst::State::Null);
                        return Ok(());
                    }
                    Ok(Cmd::Seek(s, gen_)) => {
                        generation = gen_;
                        skip_until = s - 0.001;
                        seek(s);
                        break;
                    }
                    Err(RecvTimeoutError::Timeout) => frame = f,
                },
            }
        }
    }
    let _ = pipeline.set_state(gst::State::Null);
    Ok(())
}
