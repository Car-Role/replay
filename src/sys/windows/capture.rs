//! Windows.Graphics.Capture of one window or monitor.
//!
//! Frames arrive on a system thread as GPU textures. They are passed on
//! without copying. `MinUpdateInterval` lets the compositor skip frames we do
//! not need, so a 240 Hz game costs about the same as a 60 Hz one.

use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::{IInspectable, Interface, Result};

use crate::gpu::Gpu;
use crate::ring::Ticks;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Window(isize),
    Monitor(isize),
}

pub struct Capture {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    _item: GraphicsCaptureItem,
    size: SizeInt32,
    gpu_device: windows::Graphics::DirectX::Direct3D11::IDirect3DDevice,
}

const POOL_BUFFERS: i32 = 2;

pub struct Frame {
    pub frame: Direct3D11CaptureFrame,
}

impl Frame {
    pub fn texture(&self) -> Result<ID3D11Texture2D> {
        let surface = self.frame.Surface()?;
        unsafe { surface.cast::<IDirect3DDxgiInterfaceAccess>()?.GetInterface() }
    }

    pub fn content_size(&self) -> Result<(u32, u32)> {
        let s = self.frame.ContentSize()?;
        Ok((s.Width.max(0) as u32, s.Height.max(0) as u32))
    }

    /// Capture time in 100 ns QPC ticks.
    pub fn time(&self) -> Result<Ticks> {
        Ok(self.frame.SystemRelativeTime()?.Duration)
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        // Hands the surface back to the pool right away.
        let _ = self.frame.Close();
    }
}

// SAFETY: Direct3D11CaptureFrame is an agile WinRT object.
unsafe impl Send for Frame {}

impl Capture {
    pub fn start(
        gpu: &Gpu,
        target: Target,
        fps: u32,
        cursor: bool,
        on_frame: impl Fn(Frame) + Send + 'static,
        on_closed: impl Fn() + Send + 'static,
    ) -> Result<Self> {
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = unsafe {
            match target {
                Target::Window(h) => interop.CreateForWindow(HWND(h as _))?,
                Target::Monitor(m) => interop.CreateForMonitor(HMONITOR(m as _))?,
            }
        };
        let size = item.Size()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &gpu.winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            POOL_BUFFERS,
            size,
        )?;
        pool.FrameArrived(&TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |pool, _| {
            if let Some(pool) = pool.as_ref()
                && let Ok(frame) = pool.TryGetNextFrame()
            {
                on_frame(Frame { frame });
            }
            Ok(())
        }))?;
        item.Closed(&TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(move |_, _| {
            on_closed();
            Ok(())
        }))?;

        let session = pool.CreateCaptureSession(&item)?;
        let _ = session.SetIsCursorCaptureEnabled(cursor);
        // Both calls need newer Windows builds; failure only means a yellow
        // border or more frames than needed, so it is not fatal.
        if let Err(e) = session.SetIsBorderRequired(false) {
            log::debug!("SetIsBorderRequired: {e}");
        }
        // Ask for frames a bit faster than the target so every output slot
        // gets a fresh frame even when the game runs at a multiple of the
        // display rate.
        let interval = (10_000_000 / i64::from(fps.max(1))) * 7 / 10;
        if let Err(e) = session.SetMinUpdateInterval(TimeSpan { Duration: interval }) {
            log::debug!("SetMinUpdateInterval: {e}");
        }
        session.StartCapture()?;
        Ok(Self { pool, session, _item: item, size, gpu_device: gpu.winrt_device.clone() })
    }

    /// Resizes the pool when the source size changes.
    pub fn ensure_size(&mut self, w: u32, h: u32) -> Result<bool> {
        if w as i32 == self.size.Width && h as i32 == self.size.Height || w == 0 || h == 0 {
            return Ok(false);
        }
        self.size = SizeInt32 { Width: w as i32, Height: h as i32 };
        self.pool.Recreate(&self.gpu_device, DirectXPixelFormat::B8G8R8A8UIntNormalized, POOL_BUFFERS, self.size)?;
        Ok(true)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

pub fn is_supported() -> bool {
    GraphicsCaptureSession::IsSupported().unwrap_or(false)
}
