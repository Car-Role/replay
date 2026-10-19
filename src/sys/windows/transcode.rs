//! Re-encoding for exports: exact-frame trims, size targets and vertical
//! crops. Clips are decoded on the GPU (Media Foundation source reader with
//! the D3D11 device), cropped and scaled by the video processor, and encoded
//! by the same hardware encoder the recorder uses. Frames stay on the GPU.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::*;
use windows::core::{GUID, HSTRING, Interface};

use crate::config::{Codec, RateControl};
use crate::encoder::{EncoderEvent, EncoderSettings, VideoEncoder, texture_sample};
use crate::gpu::{Converter, Gpu};
use crate::mux::bitstream::find_param_sets;
use crate::ring::TICKS_PER_SEC;

const SLOTS: usize = 6;
const FIRST_VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const EVENT_WAIT: Duration = Duration::from_secs(10);

/// One piece of a source file, in seconds, and the part of its picture to keep.
pub struct Piece {
    pub path: PathBuf,
    pub start: f64,
    pub end: f64,
    /// (x, y, w, h) in source pixels.
    pub crop: (u32, u32, u32, u32),
}

pub struct Job {
    pub pieces: Vec<Piece>,
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// Constant bitrate, for size targets.
    pub cbr: bool,
}

/// An encoded frame. `time` is in 100 ns ticks from the output start.
pub struct Encoded<'a> {
    pub time: i64,
    pub key: bool,
    pub data: &'a [u8],
}

/// Runs the job. Returns codec headers from the encoder's media type when it
/// has them (some encoders do not repeat them in keyframes).
pub fn run(job: &Job, progress: &(dyn Fn(f32) + Sync), sink: &mut dyn FnMut(Encoded) -> Result<(), String>) -> Result<Option<Vec<u8>>, String> {
    let gpu = Gpu::create().map_err(|e| format!("no usable GPU: {e}"))?;
    let settings = EncoderSettings {
        codec: job.codec,
        width: job.width,
        height: job.height,
        fps: job.fps,
        bitrate: job.bitrate_kbps * 1000,
        peak_bitrate: if job.cbr { job.bitrate_kbps * 1000 } else { job.bitrate_kbps * 1500 },
        rate_control: if job.cbr { RateControl::Cbr } else { RateControl::Vbr },
        gop: (job.fps * 2).max(1),
        low_latency: false,
    };
    let (tx, rx) = mpsc::channel();
    let mut enc = VideoEncoder::new(&gpu, &settings, move |e| {
        let _ = tx.send(e);
    })
    .map_err(|e| format!("the {} encoder did not start: {e}", job.codec.label()))?;
    let textures = (0..SLOTS).map(|_| gpu.create_nv12(job.width, job.height)).collect::<windows::core::Result<Vec<_>>>().map_err(|e| e.to_string())?;
    let samples = textures.iter().map(texture_sample).collect::<windows::core::Result<Vec<IMFSample>>>().map_err(|e| e.to_string())?;

    let mut st = State { rx, credits: 0, free: (0..SLOTS).collect(), inflight: VecDeque::new(), header: None };
    let total: f64 = job.pieces.iter().map(|p| (p.end - p.start).max(0.0)).sum::<f64>().max(0.001);
    let frame_ticks = TICKS_PER_SEC / i64::from(job.fps.max(1));
    let mut base = 0.0f64;
    let mut last_out: Option<i64> = None;
    let mut conv: Option<(Converter, (u32, u32))> = None;

    for piece in &job.pieces {
        let reader = open_reader(&gpu, piece)?;
        unsafe {
            let pv = crate::sys::windows::player::propvariant_i64((piece.start.max(0.0) * 1e7) as i64);
            reader.SetCurrentPosition(&GUID::zeroed(), &*pv).map_err(|e| format!("cannot seek: {e}"))?;
        }
        loop {
            let mut flags = 0u32;
            let mut time = 0i64;
            let mut sample = None;
            unsafe {
                reader.ReadSample(FIRST_VIDEO, 0, None, Some(&mut flags), Some(&mut time), Some(&mut sample)).map_err(|e| format!("cannot decode {}: {e}", piece.path.display()))?;
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
            let Some(sample) = sample else { continue };
            let pts = time as f64 / 1e7;
            if pts < piece.start - 0.0005 {
                continue;
            }
            if pts >= piece.end - 0.0005 {
                break;
            }
            let (tex, slice) = texture_of(&sample)?;
            let mut desc = Default::default();
            unsafe { tex.GetDesc(&mut desc) };
            let in_size = (desc.Width, desc.Height);
            if conv.as_ref().map(|c| c.1) != Some(in_size) {
                let c = Converter::for_video(gpu.clone(), in_size, (job.width, job.height), &textures).map_err(|e| format!("video processor: {e}"))?;
                conv = Some((c, in_size));
            }
            let slot = st.free_slot(&mut enc, sink)?;
            conv.as_mut().expect("set").0.convert_slice(&tex, slice, piece.crop, slot).map_err(|e| format!("video processor: {e}"))?;
            drop(tex);
            drop(sample);

            // Output time: position in the output; strictly increasing.
            let mut out = ((base + pts - piece.start) * TICKS_PER_SEC as f64).round() as i64;
            if let Some(prev) = last_out
                && out <= prev
            {
                out = prev + 1;
            }
            last_out = Some(out);
            st.wait_credit(&mut enc, sink)?;
            unsafe {
                let s = &samples[slot];
                s.SetSampleTime(out).map_err(|e| e.to_string())?;
                s.SetSampleDuration(frame_ticks).map_err(|e| e.to_string())?;
                enc.submit(s).map_err(|e| format!("encoder: {e}"))?;
            }
            st.credits -= 1;
            st.inflight.push_back(slot);
            progress((((base + pts - piece.start) / total) as f32).clamp(0.0, 0.99));
        }
        base += (piece.end - piece.start).max(0.0);
    }

    enc.drain().map_err(|e| format!("encoder: {e}"))?;
    loop {
        match st.rx.recv_timeout(EVENT_WAIT) {
            Ok(EncoderEvent::HaveOutput) => st.output(&mut enc, sink)?,
            Ok(EncoderEvent::DrainComplete) => break,
            Ok(EncoderEvent::NeedInput) => {}
            Ok(EncoderEvent::Error(hr)) => return Err(format!("encoder error {hr:?}")),
            Err(RecvTimeoutError::Timeout) => break, // Some encoders never report the drain.
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    if st.header.is_none() {
        st.header = enc.sequence_header();
    }
    progress(1.0);
    Ok(st.header)
}

struct State {
    rx: Receiver<EncoderEvent>,
    credits: u32,
    free: Vec<usize>,
    inflight: VecDeque<usize>,
    header: Option<Vec<u8>>,
}

impl State {
    fn event(&mut self, enc: &mut VideoEncoder, sink: &mut dyn FnMut(Encoded) -> Result<(), String>) -> Result<(), String> {
        match self.rx.recv_timeout(EVENT_WAIT) {
            Ok(EncoderEvent::NeedInput) => self.credits += 1,
            Ok(EncoderEvent::HaveOutput) => self.output(enc, sink)?,
            Ok(EncoderEvent::DrainComplete) => {}
            Ok(EncoderEvent::Error(hr)) => return Err(format!("encoder error {hr:?}")),
            Err(_) => return Err("the encoder stopped responding".into()),
        }
        Ok(())
    }

    fn output(&mut self, enc: &mut VideoEncoder, sink: &mut dyn FnMut(Encoded) -> Result<(), String>) -> Result<(), String> {
        let mut result = Ok(());
        enc.take_output(|f| {
            if result.is_ok() {
                result = sink(Encoded { time: f.time, key: f.key, data: f.data });
            }
        })
        .map_err(|e| format!("encoder: {e}"))?;
        result?;
        if let Some(slot) = self.inflight.pop_front() {
            self.free.push(slot);
        }
        if self.header.is_none() {
            self.header = enc.sequence_header();
        }
        Ok(())
    }

    fn free_slot(&mut self, enc: &mut VideoEncoder, sink: &mut dyn FnMut(Encoded) -> Result<(), String>) -> Result<usize, String> {
        loop {
            if let Some(s) = self.free.pop() {
                return Ok(s);
            }
            self.event(enc, sink)?;
        }
    }

    fn wait_credit(&mut self, enc: &mut VideoEncoder, sink: &mut dyn FnMut(Encoded) -> Result<(), String>) -> Result<(), String> {
        while self.credits == 0 {
            self.event(enc, sink)?;
        }
        Ok(())
    }
}

/// A source reader that decodes to NV12 textures on our device.
fn open_reader(gpu: &Gpu, piece: &Piece) -> Result<IMFSourceReader, String> {
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 3).map_err(|e| e.to_string())?;
        let attrs = attrs.expect("attributes");
        attrs.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &gpu.manager).map_err(|e| e.to_string())?;
        attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1).map_err(|e| e.to_string())?;
        attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1).map_err(|e| e.to_string())?;
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(piece.path.as_os_str()), &attrs)
            .map_err(|e| format!("cannot open {}: {e}", piece.path.display()))?;
        let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
        reader.SetStreamSelection(FIRST_VIDEO, true).map_err(|e| e.to_string())?;
        let t = MFCreateMediaType().map_err(|e| e.to_string())?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| e.to_string())?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12).map_err(|e| e.to_string())?;
        reader.SetCurrentMediaType(FIRST_VIDEO, None, &t).map_err(|e| format!("this video cannot be decoded here: {e}"))?;
        Ok(reader)
    }
}

/// The GPU texture (and array slice) behind a decoded sample.
fn texture_of(sample: &IMFSample) -> Result<(ID3D11Texture2D, u32), String> {
    unsafe {
        let buf = sample.GetBufferByIndex(0).map_err(|e| e.to_string())?;
        let dxgi: IMFDXGIBuffer = buf.cast().map_err(|_| "the decoder did not use the GPU".to_string())?;
        let mut ptr = std::ptr::null_mut();
        dxgi.GetResource(&ID3D11Texture2D::IID, &mut ptr).map_err(|e| e.to_string())?;
        let tex = ID3D11Texture2D::from_raw(ptr);
        let slice = dxgi.GetSubresourceIndex().map_err(|e| e.to_string())?;
        Ok((tex, slice))
    }
}

/// Codec headers from a keyframe, or from the encoder's media type blob.
pub fn params_from(codec: Codec, first_key: Option<&[u8]>, header: Option<&[u8]>) -> crate::mux::bitstream::ParamSets {
    if let Some(k) = first_key {
        let ps = find_param_sets(codec, k);
        if ps.is_complete(codec) {
            return ps;
        }
    }
    match header {
        Some(h) if codec == Codec::Av1 && h.first() == Some(&0x81) && h.len() > 4 => find_param_sets(codec, &h[4..]),
        Some(h) => find_param_sets(codec, h),
        None => Default::default(),
    }
}
