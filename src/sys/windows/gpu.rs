//! Direct3D 11 device on the NVIDIA adapter, shared by every session, plus the
//! GPU color converter (BGRA -> NV12 with scaling) built on the D3D11 video
//! processor. No pixel data ever goes to the CPU.

use std::sync::Arc;

use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Win32::Foundation::{HMODULE, LUID, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Media::MediaFoundation::{IMFDXGIDeviceManager, MFCreateDXGIDeviceManager};
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;
use windows::core::{Interface, Result};

pub const NVIDIA_VENDOR: u32 = 0x10de;

pub struct Gpu {
    pub device: ID3D11Device,
    pub video_device: ID3D11VideoDevice,
    pub video_context: ID3D11VideoContext1,
    pub manager: IMFDXGIDeviceManager,
    pub winrt_device: IDirect3DDevice,
    pub luid: LUID,
    pub vendor: u32,
    pub name: String,
}

// SAFETY: the device is free-threaded, and the immediate context is protected
// by ID3D11Multithread (enabled in `create`), so cross-thread use is safe.
unsafe impl Send for Gpu {}
unsafe impl Sync for Gpu {}

impl Gpu {
    /// Picks the NVIDIA adapter when present, else the first hardware adapter.
    pub fn create() -> Result<Arc<Gpu>> {
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            let mut chosen: Option<(IDXGIAdapter1, DXGI_ADAPTER_DESC1)> = None;
            let mut i = 0;
            while let Ok(a) = factory.EnumAdapters1(i) {
                i += 1;
                let d = a.GetDesc1()?;
                if (d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0 {
                    continue;
                }
                let better = match &chosen {
                    None => true,
                    Some((_, cd)) => d.VendorId == NVIDIA_VENDOR && cd.VendorId != NVIDIA_VENDOR,
                };
                if better {
                    chosen = Some((a, d));
                }
            }
            let (adapter, desc) = chosen.ok_or_else(|| windows::core::Error::from(DXGI_ERROR_NOT_FOUND))?;

            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device = device.expect("device");
            let context = context.expect("context");
            let _ = device.cast::<ID3D11Multithread>()?.SetMultithreadProtected(true);

            let mut token = 0;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.expect("manager");
            manager.ResetDevice(&device, token)?;

            let dxgi: IDXGIDevice = device.cast()?;
            let winrt_device: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;

            let name = crate::util::from_wide(&desc.Description);
            log::info!("GPU: {name} (vendor {:04x})", desc.VendorId);
            Ok(Arc::new(Gpu {
                video_device: device.cast()?,
                video_context: context.cast()?,
                device,
                manager,
                winrt_device,
                luid: desc.AdapterLuid,
                vendor: desc.VendorId,
                name,
            }))
        }
    }

    pub fn is_lost(&self) -> bool {
        unsafe { self.device.GetDeviceRemovedReason().is_err() }
    }

    pub fn create_nv12(&self, w: u32, h: u32) -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut t = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut t))? };
        Ok(t.expect("texture"))
    }
}

/// Converts captured BGRA frames (or decoded NV12 frames) into NV12 encoder
/// surfaces, scaled to fit the output with black bars when the aspect ratio
/// differs.
pub struct Converter {
    gpu: Arc<Gpu>,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    in_size: (u32, u32),
    out_size: (u32, u32),
    outputs: Vec<ID3D11VideoProcessorOutputView>,
    /// Cached input views by (texture, array slice).
    inputs: Vec<(ID3D11Texture2D, u32, ID3D11VideoProcessorInputView)>,
}

impl Converter {
    pub fn new(gpu: Arc<Gpu>, in_size: (u32, u32), out_size: (u32, u32), targets: &[ID3D11Texture2D]) -> Result<Self> {
        Self::with_input(gpu, in_size, out_size, targets, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709)
    }

    /// For decoded video: input frames are BT.709 studio-range YUV.
    pub fn for_video(gpu: Arc<Gpu>, in_size: (u32, u32), out_size: (u32, u32), targets: &[ID3D11Texture2D]) -> Result<Self> {
        Self::with_input(gpu, in_size, out_size, targets, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709)
    }

    fn with_input(
        gpu: Arc<Gpu>,
        in_size: (u32, u32),
        out_size: (u32, u32),
        targets: &[ID3D11Texture2D],
        input_space: DXGI_COLOR_SPACE_TYPE,
    ) -> Result<Self> {
        unsafe {
            let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                InputWidth: in_size.0,
                InputHeight: in_size.1,
                OutputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                OutputWidth: out_size.0,
                OutputHeight: out_size.1,
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
            };
            let enumerator = gpu.video_device.CreateVideoProcessorEnumerator(&desc)?;
            let processor = gpu.video_device.CreateVideoProcessor(&enumerator, 0)?;
            let vc = &gpu.video_context;
            vc.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            vc.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            vc.VideoProcessorSetStreamColorSpace1(&processor, 0, input_space);
            vc.VideoProcessorSetOutputColorSpace1(&processor, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
            let black = D3D11_VIDEO_COLOR {
                Anonymous: D3D11_VIDEO_COLOR_0 { RGBA: D3D11_VIDEO_COLOR_RGBA { R: 0.0, G: 0.0, B: 0.0, A: 1.0 } },
            };
            vc.VideoProcessorSetOutputBackgroundColor(&processor, false, &black);

            let mut outputs = Vec::with_capacity(targets.len());
            for t in targets {
                let vdesc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
                };
                let mut view = None;
                gpu.video_device.CreateVideoProcessorOutputView(t, &enumerator, &vdesc, Some(&mut view))?;
                outputs.push(view.expect("view"));
            }
            Ok(Self { gpu, enumerator, processor, in_size, out_size, outputs, inputs: Vec::new() })
        }
    }

    pub fn in_size(&self) -> (u32, u32) {
        self.in_size
    }

    fn input_view(&mut self, src: &ID3D11Texture2D, slice: u32) -> Result<ID3D11VideoProcessorInputView> {
        if let Some((_, _, v)) = self.inputs.iter().find(|(t, s, _)| t.as_raw() == src.as_raw() && *s == slice) {
            return Ok(v.clone());
        }
        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: slice },
            },
        };
        let mut view = None;
        unsafe {
            self.gpu.video_device.CreateVideoProcessorInputView(src, &self.enumerator, &desc, Some(&mut view))?;
        }
        let view = view.expect("view");
        // Capture pools rotate a few surfaces and decoders a few more; keep
        // a small cache of their views.
        if self.inputs.len() >= 24 {
            self.inputs.remove(0);
        }
        self.inputs.push((src.clone(), slice, view.clone()));
        Ok(view)
    }

    /// Draws the `crop` rectangle (x, y, w, h) of `src` into output slot `slot`.
    pub fn convert(&mut self, src: &ID3D11Texture2D, crop: (u32, u32, u32, u32), slot: usize) -> Result<()> {
        self.convert_slice(src, 0, crop, slot)
    }

    /// Like `convert`, for one slice of a texture array (decoder output).
    pub fn convert_slice(&mut self, src: &ID3D11Texture2D, slice: u32, crop: (u32, u32, u32, u32), slot: usize) -> Result<()> {
        let view = self.input_view(src, slice)?;
        let (cx, cy) = (crop.0 as i32, crop.1 as i32);
        let (cw, ch) = (crop.2.max(1), crop.3.max(1));
        let (ow, oh) = self.out_size;
        // Aspect fit.
        let scale = f64::min(ow as f64 / cw as f64, oh as f64 / ch as f64);
        let dw = ((cw as f64 * scale).round() as i32).min(ow as i32) & !1;
        let dh = ((ch as f64 * scale).round() as i32).min(oh as i32) & !1;
        let dx = ((ow as i32 - dw) / 2) & !1;
        let dy = ((oh as i32 - dh) / 2) & !1;
        let src_rect = RECT { left: cx, top: cy, right: cx + cw as i32, bottom: cy + ch as i32 };
        let dst_rect = RECT { left: dx, top: dy, right: dx + dw, bottom: dy + dh };
        unsafe {
            let vc = &self.gpu.video_context;
            vc.VideoProcessorSetStreamSourceRect(&self.processor, 0, true, Some(&src_rect));
            vc.VideoProcessorSetStreamDestRect(&self.processor, 0, true, Some(&dst_rect));
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: std::mem::ManuallyDrop::new(Some(view)),
                ..Default::default()
            };
            let streams = [stream];
            let r = vc.VideoProcessorBlt(&self.processor, &self.outputs[slot], 0, &streams);
            let [mut stream] = streams;
            std::mem::ManuallyDrop::drop(&mut stream.pInputSurface);
            r
        }
    }
}
