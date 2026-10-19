//! Webcam for the picture-in-picture overlay. A thread reads the camera
//! with a Media Foundation source reader (on the recorder's GPU device when
//! the driver allows it) and copies each frame into one shared BGRA texture.
//! Recording sessions draw that texture into a corner of their frames.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::core::{Interface, PWSTR};

use crate::gpu::Gpu;

const FIRST_VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

/// Lists cameras as (id, name). The id is the device's symbolic link.
pub fn list_cameras() -> Vec<(String, String)> {
    let mut out = Vec::new();
    unsafe {
        let mut attrs = None;
        if MFCreateAttributes(&mut attrs, 1).is_err() {
            return out;
        }
        let Some(attrs) = attrs else { return out };
        if attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID).is_err() {
            return out;
        }
        let mut devices: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut n = 0u32;
        if MFEnumDeviceSources(&attrs, &mut devices, &mut n).is_err() {
            return out;
        }
        for i in 0..n as usize {
            if let Some(a) = (*devices.add(i)).take() {
                let name = string_attr(&a, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME);
                let id = string_attr(&a, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK);
                out.push((id, name));
            }
        }
        CoTaskMemFree(Some(devices as _));
    }
    out
}

fn string_attr(a: &IMFActivate, key: &windows::core::GUID) -> String {
    unsafe {
        let mut p = PWSTR::null();
        let mut len = 0;
        if a.GetAllocatedString(key, &mut p, &mut len).is_ok() {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as _));
            return s;
        }
    }
    String::new()
}

/// A running camera. Sessions read `texture`; it always holds the newest frame.
pub struct Webcam {
    pub texture: ID3D11Texture2D,
    pub size: (u32, u32),
    /// Frames received, so sessions know whether the picture is ready.
    pub frames: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

// SAFETY: the texture belongs to a free-threaded device with multithread
// protection on; it is only used through that device's context.
unsafe impl Send for Webcam {}
unsafe impl Sync for Webcam {}

impl Webcam {
    /// Opens the camera with `id` (empty: the first camera).
    pub fn start(gpu: Arc<Gpu>, id: &str) -> Result<Arc<Webcam>, String> {
        let cams = list_cameras();
        let (link, name) = cams
            .iter()
            .find(|(i, _)| id.is_empty() || i == id)
            .cloned()
            .ok_or_else(|| if cams.is_empty() { "no camera was found".to_string() } else { "the chosen camera is not connected".to_string() })?;
        // The reader is made on the camera thread; its size comes back here.
        let (tx, rx) = std::sync::mpsc::channel::<Result<(u32, u32), String>>();
        let stop = Arc::new(AtomicBool::new(false));
        let frames = Arc::new(AtomicU64::new(0));
        let shared_tex: Arc<std::sync::Mutex<Option<ID3D11Texture2D>>> = Arc::new(std::sync::Mutex::new(None));
        let (s2, f2, t2, g2) = (stop.clone(), frames.clone(), shared_tex.clone(), gpu.clone());
        let thread = std::thread::Builder::new()
            .name("webcam".into())
            .spawn(move || {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                }
                if let Err(e) = run(&g2, &link, &s2, &f2, &t2, &tx) {
                    let _ = tx.send(Err(e.clone()));
                    log::warn!("webcam stopped: {e}");
                }
                unsafe { CoUninitialize() };
            })
            .map_err(|e| e.to_string())?;
        let size = match rx.recv() {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => return Err("the camera thread stopped".into()),
        };
        let texture = shared_tex.lock().map_err(|_| "camera state unavailable")?.clone().ok_or("no camera texture")?;
        log::info!("webcam: {name} at {}x{}", size.0, size.1);
        Ok(Arc::new(Webcam { texture, size, frames, stop, thread: Some(thread) }))
    }
}

impl Drop for Webcam {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(
    gpu: &Gpu,
    link: &str,
    stop: &AtomicBool,
    frames: &AtomicU64,
    shared_tex: &std::sync::Mutex<Option<ID3D11Texture2D>>,
    ready: &std::sync::mpsc::Sender<Result<(u32, u32), String>>,
) -> Result<(), String> {
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 2).map_err(|e| e.to_string())?;
        let attrs = attrs.expect("attributes");
        attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID).map_err(|e| e.to_string())?;
        attrs.SetString(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, &windows::core::HSTRING::from(link)).map_err(|e| e.to_string())?;
        let source = MFCreateDeviceSource(&attrs).map_err(|e| format!("cannot open the camera (is another app using it?): {e}"))?;

        // Prefer a mode near 1280x720 at 30 fps; small frames keep the cost low.
        let mut rattrs = None;
        MFCreateAttributes(&mut rattrs, 3).map_err(|e| e.to_string())?;
        let rattrs = rattrs.expect("attributes");
        rattrs.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &gpu.manager).map_err(|e| e.to_string())?;
        rattrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1).map_err(|e| e.to_string())?;
        let reader = MFCreateSourceReaderFromMediaSource(&source, &rattrs).map_err(|e| e.to_string())?;
        pick_native_mode(&reader);
        let t = MFCreateMediaType().map_err(|e| e.to_string())?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| e.to_string())?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(|e| e.to_string())?;
        reader.SetCurrentMediaType(FIRST_VIDEO, None, &t).map_err(|e| format!("the camera picture cannot be converted: {e}"))?;
        let cur = reader.GetCurrentMediaType(FIRST_VIDEO).map_err(|e| e.to_string())?;
        let packed = cur.GetUINT64(&MF_MT_FRAME_SIZE).map_err(|e| e.to_string())?;
        let (w, h) = ((packed >> 32) as u32, packed as u32);

        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut tex = None;
        gpu.device.CreateTexture2D(&desc, None, Some(&mut tex)).map_err(|e| e.to_string())?;
        let tex = tex.expect("texture");
        *shared_tex.lock().map_err(|_| "camera state unavailable")? = Some(tex.clone());
        let ctx = gpu.device.GetImmediateContext().map_err(|e| e.to_string())?;
        let _ = ready.send(Ok((w, h)));

        while !stop.load(Ordering::Acquire) {
            let mut flags = 0u32;
            let mut sample = None;
            if reader.ReadSample(FIRST_VIDEO, 0, None, Some(&mut flags), None, Some(&mut sample)).is_err() {
                return Err("the camera stopped sending pictures".into());
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                return Err("the camera was disconnected".into());
            }
            let Some(sample) = sample else { continue };
            let Ok(buf) = sample.GetBufferByIndex(0) else { continue };
            if let Ok(dxgi) = buf.cast::<IMFDXGIBuffer>() {
                // GPU frame: copy it straight into the shared texture.
                let mut p = std::ptr::null_mut();
                if dxgi.GetResource(&ID3D11Texture2D::IID, &mut p).is_ok() {
                    let src = ID3D11Texture2D::from_raw(p);
                    let sub = dxgi.GetSubresourceIndex().unwrap_or(0);
                    let src_res: ID3D11Resource = src.cast().map_err(|e| e.to_string())?;
                    let dst_res: ID3D11Resource = tex.cast().map_err(|e| e.to_string())?;
                    ctx.CopySubresourceRegion(&dst_res, 0, 0, 0, 0, &src_res, sub, None);
                }
            } else if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
                // System-memory frame: upload it.
                let mut p = std::ptr::null_mut();
                let mut pitch = 0i32;
                if b2.Lock2D(&mut p, &mut pitch).is_ok() {
                    let dst_res: ID3D11Resource = tex.cast().map_err(|e| e.to_string())?;
                    if pitch >= 0 {
                        ctx.UpdateSubresource(&dst_res, 0, None, p as *const _, pitch as u32, 0);
                    } else {
                        // Bottom-up: `p` is the top row and rows go backwards in memory.
                        let row = w as usize * 4;
                        let mut top_down = vec![0u8; row * h as usize];
                        for y in 0..h as usize {
                            let src = p.offset(y as isize * pitch as isize);
                            std::ptr::copy_nonoverlapping(src, top_down[y * row..].as_mut_ptr(), row);
                        }
                        ctx.UpdateSubresource(&dst_res, 0, None, top_down.as_ptr() as *const _, row as u32, 0);
                    }
                    let _ = b2.Unlock2D();
                }
            }
            frames.fetch_add(1, Ordering::Release);
        }
        Ok(())
    }
}

/// Chooses the camera's own mode closest to 1280x720 at 30 fps.
fn pick_native_mode(reader: &IMFSourceReader) {
    unsafe {
        let mut best: Option<(i64, IMFMediaType)> = None;
        for i in 0.. {
            let Ok(t) = reader.GetNativeMediaType(FIRST_VIDEO, i) else { break };
            let size = t.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
            let rate = t.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or(0);
            let (w, h) = ((size >> 32) as i64, (size & 0xffff_ffff) as i64);
            let fps = if rate & 0xffff_ffff == 0 { 0 } else { (rate >> 32) as i64 / (rate & 0xffff_ffff) as i64 };
            let score = (w - 1280).abs() + (h - 720).abs() + (fps - 30).abs() * 40;
            if best.as_ref().is_none_or(|(s, _)| score < *s) {
                best = Some((score, t));
            }
        }
        if let Some((_, t)) = best {
            let _ = reader.SetCurrentMediaType(FIRST_VIDEO, None, &t);
        }
    }
}
