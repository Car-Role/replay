//! GStreamer helpers: initialisation, picking hardware encoders, pulling
//! samples with monotonic-clock timestamps.

use std::sync::OnceLock;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::config::{Codec, Config, RateControl};
use crate::ring::Ticks;

pub fn init() -> Result<(), String> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| gst::init().map_err(|e| format!("GStreamer is not available: {e}"))).clone()
}

pub fn has_element(name: &str) -> bool {
    init().is_ok() && gst::ElementFactory::find(name).is_some()
}

/// Encoders in order of preference: NVIDIA (nvcodec), VA-API, then software.
fn candidates(codec: Codec) -> &'static [&'static str] {
    match codec {
        Codec::H264 => &["nvh264enc", "vah264enc", "vah264lpenc", "vaapih264enc", "x264enc"],
        Codec::Hevc => &["nvh265enc", "vah265enc", "vah265lpenc", "vaapih265enc", "x265enc"],
        Codec::Av1 => &["nvav1enc", "vaav1enc", "vaav1lpenc", "svtav1enc"],
    }
}

pub fn encoder_for(codec: Codec) -> Option<&'static str> {
    candidates(codec).iter().copied().find(|n| has_element(n))
}

pub fn is_hardware(name: &str) -> bool {
    name.starts_with("nv") || name.starts_with("va")
}

/// Pipeline text for the encoder and parser, with the properties this
/// element supports.
pub fn encoder_fragment(name: &str, codec: Codec, cfg: &Config) -> String {
    let v = &cfg.video;
    let kbps = v.bitrate_kbps;
    let gop = (v.fps / 2).max(1);
    let probe = gst::ElementFactory::make(name).build().ok();
    let has = |p: &str| probe.as_ref().is_some_and(|e| e.find_property(p).is_some());
    let mut props: Vec<String> = Vec::new();
    let mut set = |p: &str, val: String| {
        if has(p) {
            props.push(format!("{p}={val}"));
        }
    };
    match name {
        n if n.starts_with("nv") => {
            set("bitrate", kbps.to_string());
            set("max-bitrate", v.peak_kbps().to_string());
            set("gop-size", gop.to_string());
            set("bframes", "0".into());
            set("zerolatency", "true".into());
            set("rc-mode", if v.rate_control == RateControl::Cbr { "cbr".into() } else { "vbr".into() });
        }
        n if n.starts_with("vaapi") => {
            set("bitrate", kbps.to_string());
            set("keyframe-period", gop.to_string());
            set("rate-control", if v.rate_control == RateControl::Cbr { "cbr".into() } else { "vbr".into() });
        }
        n if n.starts_with("va") => {
            set("bitrate", kbps.to_string());
            set("key-int-max", gop.to_string());
            set("b-frames", "0".into());
            set("rate-control", if v.rate_control == RateControl::Cbr { "cbr".into() } else { "vbr".into() });
        }
        "x264enc" | "x265enc" => {
            set("bitrate", kbps.to_string());
            set("key-int-max", gop.to_string());
            set("tune", "zerolatency".into());
            set("speed-preset", "veryfast".into());
            set("bframes", "0".into());
        }
        "svtav1enc" => {
            set("target-bitrate", kbps.to_string());
            set("intra-period-length", gop.to_string());
            set("preset", "10".into());
        }
        _ => {}
    }
    let parse = match codec {
        Codec::H264 => "h264parse config-interval=-1 ! video/x-h264,stream-format=byte-stream,alignment=au",
        Codec::Hevc => "h265parse config-interval=-1 ! video/x-h265,stream-format=byte-stream,alignment=au",
        Codec::Av1 => "av1parse ! video/x-av1,stream-format=obu-stream,alignment=tu",
    };
    format!("{name} {} ! {parse}", props.join(" "))
}

pub fn pipeline(desc: &str) -> Result<(gst::Pipeline, gst_app::AppSink), String> {
    init()?;
    let el = gst::parse::launch(desc).map_err(|e| format!("cannot build pipeline: {e}"))?;
    let pipeline = el.downcast::<gst::Pipeline>().map_err(|_| "not a pipeline".to_string())?;
    let sink = pipeline
        .by_name("sink")
        .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
        .ok_or("pipeline has no app sink")?;
    Ok((pipeline, sink))
}

/// First error on the pipeline bus, if any.
pub fn bus_error(p: &gst::Pipeline) -> Option<String> {
    let bus = p.bus()?;
    while let Some(msg) = bus.pop() {
        if let gst::MessageView::Error(e) = msg.view() {
            return Some(format!("{} ({})", e.error(), e.debug().map(|d| d.to_string()).unwrap_or_default()));
        }
    }
    None
}

/// Buffer time on the monotonic clock, in 100 ns ticks (GStreamer's
/// system clock is CLOCK_MONOTONIC; base time + running time = clock time).
pub fn ticks(p: &gst::Pipeline, pts: Option<gst::ClockTime>) -> Ticks {
    match (p.base_time(), pts) {
        (Some(base), Some(pts)) => ((base.nseconds() + pts.nseconds()) / 100) as Ticks,
        _ => crate::util::now_ticks(),
    }
}
