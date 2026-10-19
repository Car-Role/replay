//! HDR games. When Windows HDR is on for a monitor, frames are captured as
//! 16-bit float scRGB and tone-mapped on the GPU to normal (SDR) BT.709
//! colors, so clips look right on every screen and site. Without this, HDR
//! captures look grey and washed out.

use windows::Win32::Devices::Display::*;
use windows::Win32::Foundation::{HWND, LUID};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MONITORINFOEXW, MonitorFromWindow};
use windows::core::{Interface, PCSTR, Result, s};

/// scRGB value of SDR white when Windows does not report it (about 200 nits).
const DEFAULT_WHITE: f32 = 2.5;

/// Uses the HDR path on SDR monitors too (for `--selftest --hdr`).
pub static FORCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The monitor a capture target shows on.
pub fn monitor_of(target: crate::capture::Target) -> isize {
    match target {
        crate::capture::Target::Monitor(m) => m,
        crate::capture::Target::Window(h) => unsafe { MonitorFromWindow(HWND(h as _), MONITOR_DEFAULTTOPRIMARY).0 as isize },
    }
}

/// Some(SDR white in scRGB units) when HDR is on for the monitor.
pub fn monitor_hdr(monitor: isize) -> Option<f32> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            a += 1;
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                o += 1;
                let Ok(desc) = output.GetDesc() else { continue };
                if desc.Monitor.0 as isize != monitor {
                    continue;
                }
                let o6: IDXGIOutput6 = output.cast().ok()?;
                let d1 = o6.GetDesc1().ok()?;
                if d1.ColorSpace != DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 {
                    // SDR monitor: FP16 capture is plain sRGB-scaled linear, white = 1.0.
                    return FORCE.load(std::sync::atomic::Ordering::Relaxed).then_some(1.0);
                }
                return Some(sdr_white(monitor).unwrap_or(DEFAULT_WHITE));
            }
        }
        None
    }
}

/// The "SDR content brightness" of the monitor, in scRGB units (1.0 = 80 nits).
fn sdr_white(monitor: isize) -> Option<f32> {
    unsafe {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(HMONITOR(monitor as _), (&raw mut info).cast::<MONITORINFO>()).as_bool() {
            return None;
        }
        let device = String::from_utf16_lossy(&info.szDevice).trim_end_matches('\0').to_string();
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm).is_err() {
            return None;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None).is_err() {
            return None;
        }
        for p in &paths[..np as usize] {
            let mut name = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: header(DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(), p.sourceInfo.adapterId, p.sourceInfo.id),
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut name.header) != 0 {
                continue;
            }
            let src = String::from_utf16_lossy(&name.viewGdiDeviceName).trim_end_matches('\0').to_string();
            if src != device {
                continue;
            }
            let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
                header: header(DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(), p.targetInfo.adapterId, p.targetInfo.id),
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut white.header) != 0 {
                return None;
            }
            // SDRWhiteLevel is in thousandths of 80 nits.
            return Some((white.SDRWhiteLevel as f32 / 1000.0).clamp(1.0, 6.0));
        }
        None
    }
}

fn header(kind: DISPLAYCONFIG_DEVICE_INFO_TYPE, size: usize, adapter: LUID, id: u32) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: kind, size: size as u32, adapterId: adapter, id }
}

const SHADER: &str = r"
cbuffer C : register(b0) { float scale; float3 pad; };
Texture2D<float4> src : register(t0);

float4 vs(uint id : SV_VertexID) : SV_Position {
    float2 uv = float2((id << 1) & 2, id & 2);
    return float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
}

float4 ps(float4 pos : SV_Position) : SV_Target {
    // scRGB is linear BT.709 where 1.0 is 80 nits. Scale so SDR white is 1.0.
    float3 c = max(src.Load(int3(pos.xy, 0)).rgb / scale, 0);
    // Roll highlights off smoothly above the knee, keeping their hue.
    float m = max(c.r, max(c.g, c.b));
    const float knee = 0.9;
    if (m > knee) {
        float t = knee + (1 - knee) * (1 - exp(-(m - knee) / (1 - knee)));
        c *= t / m;
    }
    c = saturate(c);
    float3 lo = c * 12.92;
    float3 hi = 1.055 * pow(c, 1.0 / 2.4) - 0.055;
    return float4(c <= 0.0031308 ? lo : hi, 1);
}
";

fn compile(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob> {
    unsafe {
        let mut blob = None;
        let mut errors = None;
        let r = D3DCompile(SHADER.as_ptr().cast(), SHADER.len(), s!("tonemap"), None, None, entry, target, 0, 0, &mut blob, Some(&mut errors));
        if let Err(e) = r {
            if let Some(err) = errors {
                let msg = std::slice::from_raw_parts(err.GetBufferPointer() as *const u8, err.GetBufferSize());
                log::error!("tone-map shader: {}", String::from_utf8_lossy(msg));
            }
            return Err(e);
        }
        Ok(blob.expect("blob"))
    }
}

fn bytes(b: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()) }
}

/// Tone-maps FP16 scRGB frames into an 8-bit BGRA texture.
pub struct ToneMapper {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    constants: ID3D11Buffer,
    out: Option<(ID3D11Texture2D, ID3D11RenderTargetView, (u32, u32))>,
    views: Vec<(ID3D11Texture2D, ID3D11ShaderResourceView)>,
}

// SAFETY: used from one session thread; the device is free-threaded and the
// context is locked with ID3D11Multithread around each pass.
unsafe impl Send for ToneMapper {}

impl ToneMapper {
    pub fn new(device: &ID3D11Device) -> Result<Self> {
        unsafe {
            let vsb = compile(s!("vs"), s!("vs_5_0"))?;
            let psb = compile(s!("ps"), s!("ps_5_0"))?;
            let mut vs = None;
            device.CreateVertexShader(bytes(&vsb), None, Some(&mut vs))?;
            let mut ps = None;
            device.CreatePixelShader(bytes(&psb), None, Some(&mut ps))?;
            let desc = D3D11_BUFFER_DESC { ByteWidth: 16, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, ..Default::default() };
            let mut constants = None;
            device.CreateBuffer(&desc, None, Some(&mut constants))?;
            let context = device.GetImmediateContext()?;
            Ok(Self {
                device: device.clone(),
                context,
                vs: vs.expect("vs"),
                ps: ps.expect("ps"),
                constants: constants.expect("buffer"),
                out: None,
                views: Vec::new(),
            })
        }
    }

    /// Draws `src` (FP16 scRGB) tone-mapped into the output texture and returns it.
    pub fn map(&mut self, src: &ID3D11Texture2D, white: f32) -> Result<ID3D11Texture2D> {
        unsafe {
            let mut d = D3D11_TEXTURE2D_DESC::default();
            src.GetDesc(&mut d);
            let size = (d.Width, d.Height);
            if self.out.as_ref().map(|o| o.2) != Some(size) {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: size.0,
                    Height: size.1,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                let mut t = None;
                self.device.CreateTexture2D(&desc, None, Some(&mut t))?;
                let t = t.expect("texture");
                let mut rtv = None;
                self.device.CreateRenderTargetView(&t, None, Some(&mut rtv))?;
                self.out = Some((t, rtv.expect("rtv"), size));
            }
            let srv = match self.views.iter().find(|(t, _)| t.as_raw() == src.as_raw()) {
                Some((_, v)) => v.clone(),
                None => {
                    let mut v = None;
                    self.device.CreateShaderResourceView(src, None, Some(&mut v))?;
                    let v = v.expect("srv");
                    if self.views.len() >= 4 {
                        self.views.remove(0);
                    }
                    self.views.push((src.clone(), v.clone()));
                    v
                }
            };
            let (out, rtv, _) = self.out.as_ref().expect("set");
            let ctx = &self.context;
            // Other threads use the same context; keep this pass together.
            let mt: ID3D11Multithread = ctx.cast()?;
            mt.Enter();
            let constants = [white.max(0.1), 0.0, 0.0, 0.0];
            ctx.UpdateSubresource(&self.constants, 0, None, constants.as_ptr().cast(), 0, 0);
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: size.0 as f32, Height: size.1 as f32, MinDepth: 0.0, MaxDepth: 1.0 }]));
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetShaderResources(0, Some(&[Some(srv)]));
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
            ctx.Draw(3, 0);
            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.OMSetRenderTargets(None, None);
            mt.Leave();
            Ok(out.clone())
        }
    }
}
