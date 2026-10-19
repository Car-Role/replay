//! Hardware video encoder through a Media Foundation transform (on NVIDIA
//! this is NVENC). Input is NV12 GPU surfaces; output is the compressed
//! bitstream. The transform runs in async mode: it posts NeedInput and
//! HaveOutput events, which are forwarded to the session worker.

use std::mem::ManuallyDrop;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{E_FAIL, E_NOTIMPL};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_BOOL, VT_UI4};
use windows::core::{GUID, HRESULT, IUnknownImpl, Interface, Result, implement};

use crate::config::{Codec, RateControl};
use crate::gpu::{Gpu, NVIDIA_VENDOR};

#[derive(Clone, Debug)]
pub struct EncoderSettings {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub peak_bitrate: u32,
    pub rate_control: RateControl,
    pub gop: u32,
    /// True for live recording (output right away); false for exports,
    /// where the encoder may look ahead for better quality.
    pub low_latency: bool,
}

#[derive(Debug)]
pub enum EncoderEvent {
    NeedInput,
    HaveOutput,
    /// Every frame given before `drain` has been output.
    DrainComplete,
    Error(HRESULT),
}

pub struct EncodedFrame<'a> {
    pub time: i64,
    pub key: bool,
    pub data: &'a [u8],
}

pub struct VideoEncoder {
    mft: IMFTransform,
    codec_api: Option<ICodecAPI>,
    stopped: Arc<AtomicBool>,
    provides_samples: bool,
    out_size: u32,
}

fn subtype(c: Codec) -> GUID {
    match c {
        Codec::H264 => MFVideoFormat_H264,
        Codec::Hevc => MFVideoFormat_HEVC,
        Codec::Av1 => MFVideoFormat_AV1,
    }
}

fn pack(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

fn variant_u32(v: u32) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_UI4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { ulVal: v },
            }),
        },
    }
}

fn variant_bool(v: bool) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_BOOL,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { boolVal: v.into() },
            }),
        },
    }
}

/// Lists hardware encoder activates for `codec`, best match first:
/// same adapter as `gpu`, then NVIDIA, then anything.
pub fn find_encoders(codec: Codec, gpu: Option<&Gpu>) -> Result<Vec<(IMFActivate, String)>> {
    unsafe {
        let out = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: subtype(codec) };
        let mut acts: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut n = 0u32;
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            None,
            Some(&out),
            &mut acts,
            &mut n,
        )?;
        let mut list = Vec::new();
        for i in 0..n as usize {
            if let Some(a) = (*acts.add(i)).take() {
                let name = get_string(&a, &MFT_FRIENDLY_NAME_Attribute);
                let vendor = get_string(&a, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute);
                let luid = a.GetUINT64(&MFT_ENUM_ADAPTER_LUID).ok();
                let mut score = 0;
                if let (Some(g), Some(l)) = (gpu, luid)
                    && l == pack(g.luid.HighPart as u32, g.luid.LowPart)
                {
                    score += 2;
                }
                if vendor.eq_ignore_ascii_case(&format!("VEN_{NVIDIA_VENDOR:04X}")) {
                    score += 1;
                }
                list.push((score, a, name));
            }
        }
        CoTaskMemFree(Some(acts as _));
        list.sort_by_key(|(s, _, _)| std::cmp::Reverse(*s));
        Ok(list.into_iter().map(|(_, a, n)| (a, n)).collect())
    }
}

fn get_string(a: &IMFAttributes, key: &GUID) -> String {
    let mut buf = [0u16; 256];
    let mut len = 0;
    unsafe {
        if a.GetString(key, &mut buf, Some(&mut len)).is_ok() {
            return String::from_utf16_lossy(&buf[..len as usize]);
        }
    }
    String::new()
}

#[implement(IMFAsyncCallback)]
struct EventPump {
    generator: IMFMediaEventGenerator,
    stopped: Arc<AtomicBool>,
    sink: Box<dyn Fn(EncoderEvent) + Send + Sync>,
}

impl IMFAsyncCallback_Impl for EventPump_Impl {
    fn GetParameters(&self, _flags: *mut u32, _queue: *mut u32) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Invoke(&self, result: windows::core::Ref<IMFAsyncResult>) -> Result<()> {
        let event = match unsafe { self.generator.EndGetEvent(result.ok()?) } {
            Ok(e) => e,
            Err(e) => {
                if !self.stopped.load(Ordering::Acquire) {
                    (self.sink)(EncoderEvent::Error(e.code()));
                }
                return Ok(());
            }
        };
        if self.stopped.load(Ordering::Acquire) {
            return Ok(());
        }
        let kind = unsafe { event.GetType()? };
        match kind {
            k if k == METransformNeedInput.0 as u32 => (self.sink)(EncoderEvent::NeedInput),
            k if k == METransformHaveOutput.0 as u32 => (self.sink)(EncoderEvent::HaveOutput),
            k if k == METransformDrainComplete.0 as u32 => (self.sink)(EncoderEvent::DrainComplete),
            k if k == MEError.0 as u32 => {
                let status = unsafe { event.GetStatus().unwrap_or(E_FAIL) };
                (self.sink)(EncoderEvent::Error(status));
                return Ok(());
            }
            _ => {}
        }
        let me: IMFAsyncCallback = self.to_interface();
        unsafe { self.generator.BeginGetEvent(&me, None) }
    }
}

impl VideoEncoder {
    pub fn new(gpu: &Gpu, s: &EncoderSettings, sink: impl Fn(EncoderEvent) + Send + Sync + 'static) -> Result<Self> {
        let candidates = find_encoders(s.codec, Some(gpu))?;
        if candidates.is_empty() {
            return Err(windows::core::Error::new(MF_E_TOPO_CODEC_NOT_FOUND, format!("no hardware {:?} encoder", s.codec)));
        }
        let sink: Arc<dyn Fn(EncoderEvent) + Send + Sync> = Arc::new(sink);
        let mut last_err = None;
        for (act, name) in candidates {
            let sink = sink.clone();
            match Self::open(gpu, s, &act, name.clone(), move |e| sink(e)) {
                Ok(e) => return Ok(e),
                Err(e) => {
                    log::warn!("encoder {name} failed: {e}");
                    unsafe {
                        let _ = act.ShutdownObject();
                    }
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.expect("at least one candidate"))
    }

    fn open(
        gpu: &Gpu,
        s: &EncoderSettings,
        act: &IMFActivate,
        name: String,
        sink: impl Fn(EncoderEvent) + Send + Sync + 'static,
    ) -> Result<Self> {
        unsafe {
            let mft: IMFTransform = act.ActivateObject()?;
            let attrs = mft.GetAttributes()?;
            attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            if s.low_latency {
                let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, gpu.manager.as_raw() as usize)?;

            // Rate control and GOP must be set before the media types on some encoders.
            if let Ok(api) = mft.cast::<ICodecAPI>() {
                let set = |key: &GUID, v: VARIANT, what: &str| {
                    if let Err(e) = api.SetValue(key, &v) {
                        log::debug!("{name}: {what} not accepted: {e}");
                    }
                };
                let mode = match s.rate_control {
                    RateControl::Cbr => eAVEncCommonRateControlMode_CBR,
                    RateControl::Vbr => eAVEncCommonRateControlMode_PeakConstrainedVBR,
                };
                set(&CODECAPI_AVEncCommonRateControlMode, variant_u32(mode.0 as u32), "rate control");
                set(&CODECAPI_AVEncCommonMeanBitRate, variant_u32(s.bitrate), "bitrate");
                set(&CODECAPI_AVEncCommonMaxBitRate, variant_u32(s.peak_bitrate), "max bitrate");
                set(&CODECAPI_AVEncMPVGOPSize, variant_u32(s.gop), "GOP");
                set(&CODECAPI_AVEncMPVDefaultBPictureCount, variant_u32(0), "B-frames");
                set(&CODECAPI_AVLowLatencyMode, variant_bool(s.low_latency), "low latency");
                // 0 = fastest, 100 = best. NVENC presets are cheap; favor quality a bit
                // while recording, and more for exports.
                set(&CODECAPI_AVEncCommonQualityVsSpeed, variant_u32(if s.low_latency { 60 } else { 90 }), "quality vs speed");
            }

            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out.SetGUID(&MF_MT_SUBTYPE, &subtype(s.codec))?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, s.bitrate)?;
            out.SetUINT64(&MF_MT_FRAME_SIZE, pack(s.width, s.height))?;
            out.SetUINT64(&MF_MT_FRAME_RATE, pack(s.fps, 1))?;
            out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            match s.codec {
                Codec::H264 => out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?,
                Codec::Hevc => out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH265VProfile_Main_420_8.0 as u32)?,
                Codec::Av1 => {}
            }
            mft.SetOutputType(0, &out, 0)?;

            let mut set_input = false;
            for i in 0.. {
                let Ok(t) = mft.GetInputAvailableType(0, i) else { break };
                if t.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    t.SetUINT64(&MF_MT_FRAME_SIZE, pack(s.width, s.height))?;
                    t.SetUINT64(&MF_MT_FRAME_RATE, pack(s.fps, 1))?;
                    t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
                    let _ = t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32);
                    mft.SetInputType(0, &t, 0)?;
                    set_input = true;
                    break;
                }
            }
            if !set_input {
                return Err(windows::core::Error::new(MF_E_INVALIDMEDIATYPE, "encoder does not take NV12"));
            }

            let info = mft.GetOutputStreamInfo(0)?;
            let provides_samples = info.dwFlags
                & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32)
                != 0;

            let generator: IMFMediaEventGenerator = mft.cast()?;
            let stopped = Arc::new(AtomicBool::new(false));
            let pump: IMFAsyncCallback =
                EventPump { generator: generator.clone(), stopped: stopped.clone(), sink: Box::new(sink) }.into();
            generator.BeginGetEvent(&pump, None)?;

            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            log::info!(
                "encoder: {name}, {:?} {}x{} @{} fps, {} kbps",
                s.codec,
                s.width,
                s.height,
                s.fps,
                s.bitrate / 1000
            );
            let codec_api = mft.cast::<ICodecAPI>().ok();
            Ok(Self { mft, codec_api, stopped, provides_samples, out_size: info.cbSize.max(1 << 20) })
        }
    }

    /// Makes the next submitted frame a keyframe. Returns false if unsupported.
    pub fn force_keyframe(&self) -> bool {
        match &self.codec_api {
            Some(api) => unsafe { api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &variant_u32(1)).is_ok() },
            None => false,
        }
    }

    /// Asks for every pending frame; a DrainComplete event follows.
    pub fn drain(&self) -> Result<()> {
        unsafe { self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0) }
    }

    /// Call only after a NeedInput event.
    pub fn submit(&self, sample: &IMFSample) -> Result<()> {
        unsafe { self.mft.ProcessInput(0, sample, 0) }
    }

    /// Call after a HaveOutput event. Gives the frame to `f`.
    pub fn take_output(&mut self, mut f: impl FnMut(EncodedFrame)) -> Result<()> {
        unsafe {
            for _ in 0..2 {
                let own = if self.provides_samples {
                    None
                } else {
                    let s = MFCreateSample()?;
                    s.AddBuffer(&MFCreateMemoryBuffer(self.out_size)?)?;
                    Some(s)
                };
                let mut buf = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(own),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                }];
                let mut status = 0;
                let r = self.mft.ProcessOutput(0, &mut buf, &mut status);
                let [mut b] = buf;
                let sample = ManuallyDrop::take(&mut b.pSample);
                ManuallyDrop::drop(&mut b.pEvents);
                match r {
                    Ok(()) => {
                        let Some(sample) = sample else { return Ok(()) };
                        let time = sample.GetSampleTime().unwrap_or(0);
                        let key = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
                        let mb = sample.ConvertToContiguousBuffer()?;
                        let mut ptr = std::ptr::null_mut();
                        let mut len = 0;
                        mb.Lock(&mut ptr, None, Some(&mut len))?;
                        let data = std::slice::from_raw_parts(ptr, len as usize);
                        f(EncodedFrame { time, key, data });
                        mb.Unlock()?;
                        return Ok(());
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        let t = self.mft.GetOutputAvailableType(0, 0)?;
                        self.mft.SetOutputType(0, &t, 0)?;
                        let info = self.mft.GetOutputStreamInfo(0)?;
                        self.out_size = info.cbSize.max(1 << 20);
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        }
    }

    /// Codec private data from the output type, when the encoder exposes it.
    pub fn sequence_header(&self) -> Option<Vec<u8>> {
        unsafe {
            let t = self.mft.GetOutputCurrentType(0).ok()?;
            let size = t.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER).ok()?;
            let mut v = vec![0u8; size as usize];
            t.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut v, None).ok()?;
            Some(v)
        }
    }
}

impl Drop for VideoEncoder {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            if let Ok(s) = self.mft.cast::<IMFShutdown>() {
                let _ = s.Shutdown();
            }
        }
    }
}

/// Wraps a GPU texture as an MF sample (no copy).
pub fn texture_sample(tex: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D) -> Result<IMFSample> {
    unsafe {
        let buf = MFCreateDXGISurfaceBuffer(&windows::Win32::Graphics::Direct3D11::ID3D11Texture2D::IID, tex, 0, false)?;
        if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
            buf.SetCurrentLength(b2.GetContiguousLength()?)?;
        }
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buf)?;
        Ok(sample)
    }
}
