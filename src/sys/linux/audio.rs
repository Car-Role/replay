//! Linux audio: PulseAudio/PipeWire capture through GStreamer, and AAC
//! encoding with whichever GStreamer AAC encoder is installed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use super::gst as g;
use crate::media::{CHANNELS, SAMPLE_RATE};
use crate::ring::PacketRing;
use crate::sys::unix::app::Stop;
use crate::sys::unix::timeline::Timeline;

const PCM_CAPS: &str = "audio/x-raw,format=S16LE,rate=48000,channels=2,layout=interleaved";

pub struct AacEncoder {
    pipeline: gst::Pipeline,
    src: gst_app::AppSrc,
    sink: gst_app::AppSink,
    frames: u64,
}

impl AacEncoder {
    pub fn new(bitrate_kbps: u32) -> Result<Self, String> {
        let enc = ["fdkaacenc", "avenc_aac", "voaacenc", "faac"]
            .into_iter()
            .find(|e| g::has_element(e))
            .ok_or("no AAC encoder found; install gstreamer1.0-libav (or the fdk-aac plugin)")?;
        let desc = format!(
            "appsrc name=src format=time is-live=false caps={PCM_CAPS} ! audioconvert ! {enc} bitrate={} ! aacparse ! audio/mpeg,mpegversion=4,stream-format=raw ! appsink name=sink sync=false",
            bitrate_kbps * 1000
        );
        let (pipeline, sink) = g::pipeline(&desc)?;
        let src = pipeline.by_name("src").and_then(|e| e.downcast::<gst_app::AppSrc>().ok()).ok_or("no app source")?;
        pipeline.set_state(gst::State::Playing).map_err(|e| format!("AAC encoder: {e}"))?;
        Ok(Self { pipeline, src, sink, frames: 0 })
    }

    fn drain(&self, f: &mut impl FnMut(&[u8])) {
        while let Some(s) = self.sink.try_pull_sample(gst::ClockTime::ZERO) {
            if let Some(b) = s.buffer()
                && let Ok(m) = b.map_readable()
            {
                f(m.as_slice());
            }
        }
    }

    pub fn encode(&mut self, pcm: &[i16], mut f: impl FnMut(&[u8])) -> Result<(), String> {
        if pcm.is_empty() {
            return Ok(());
        }
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        let n = (pcm.len() / CHANNELS as usize) as u64;
        let mut buf = gst::Buffer::from_mut_slice(bytes);
        {
            let b = buf.get_mut().expect("new buffer is writable");
            b.set_pts(gst::ClockTime::from_nseconds(self.frames * 1_000_000_000 / u64::from(SAMPLE_RATE)));
            b.set_duration(gst::ClockTime::from_nseconds(n * 1_000_000_000 / u64::from(SAMPLE_RATE)));
        }
        self.frames += n;
        self.src.push_buffer(buf).map_err(|e| format!("AAC encoder: {e:?}"))?;
        self.drain(&mut f);
        Ok(())
    }

    pub fn finish(&mut self, mut f: impl FnMut(&[u8])) -> Result<(), String> {
        let _ = self.src.end_of_stream();
        while let Ok(s) = self.sink.pull_sample() {
            if let Some(b) = s.buffer()
                && let Ok(m) = b.map_readable()
            {
                f(m.as_slice());
            }
        }
        Ok(())
    }
}

impl Drop for AacEncoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// A running capture that feeds a [`Timeline`].
pub struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Stop for Capture {
    fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        Stop::stop(self);
    }
}

/// `device`: a PulseAudio source name, "@DEFAULT_MONITOR@" for everything
/// the speakers play, or empty for the default microphone.
pub fn capture(device: &str, bitrate_kbps: u32, ring: Arc<Mutex<PacketRing>>) -> Result<Capture, String> {
    let dev = if device.is_empty() { String::new() } else { format!("device=\"{device}\"") };
    let desc = format!(
        "pulsesrc {dev} do-timestamp=true buffer-time=200000 latency-time=20000 ! audioconvert ! audioresample ! {PCM_CAPS} ! appsink name=sink sync=false max-buffers=200"
    );
    let (pipeline, sink) = g::pipeline(&desc)?;
    let mut timeline = Timeline::new(bitrate_kbps, ring)?;
    pipeline.set_state(gst::State::Playing).map_err(|e| format!("audio capture: {e}"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let name = if device.is_empty() { "default microphone".to_string() } else { device.to_string() };
    let thread = std::thread::Builder::new()
        .name("audio".into())
        .spawn(move || {
            let mut pcm: Vec<i16> = Vec::new();
            while !s.load(Ordering::Acquire) {
                match sink.try_pull_sample(gst::ClockTime::from_mseconds(50)) {
                    Some(sample) => {
                        let Some(b) = sample.buffer() else { continue };
                        let at = g::ticks(&pipeline, b.pts());
                        let Ok(m) = b.map_readable() else { continue };
                        pcm.clear();
                        pcm.extend(m.as_slice().chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])));
                        if let Err(e) = timeline.push(at, &pcm) {
                            log::warn!("audio ({name}): {e}");
                        }
                    }
                    None => {
                        if let Some(e) = g::bus_error(&pipeline) {
                            log::warn!("audio ({name}) stopped: {e}");
                            break;
                        }
                        let _ = timeline.idle(crate::util::now_ticks());
                    }
                }
            }
            let _ = pipeline.set_state(gst::State::Null);
        })
        .map_err(|e| e.to_string())?;
    log::info!("audio started: {}", if device.is_empty() { "default microphone" } else { device });
    Ok(Capture { stop, thread: Some(thread) })
}

/// Microphones as (source name, description), from `pactl`.
pub fn list_microphones() -> Vec<(String, String)> {
    let Ok(out) = std::process::Command::new("pactl").args(["list", "sources"]).output() else { return Vec::new() };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut list = Vec::new();
    let mut name = None;
    for line in text.lines().map(str::trim) {
        if let Some(n) = line.strip_prefix("Name: ") {
            name = Some(n.to_string());
        } else if let Some(d) = line.strip_prefix("Description: ")
            && let Some(n) = name.take()
            && !n.ends_with(".monitor")
        {
            list.push((n, d.to_string()));
        }
    }
    list
}
