//! Notification-area icon, balloon messages and the generated icons.

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_NOSOUND, NIIF_USER, NIIF_WARNING, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, GetSystemMetrics, HICON, ICONINFO, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, SM_CXSMICON,
};
use windows::core::PCWSTR;

pub const TRAY_ID: u32 = 1;

pub struct Icons {
    pub idle: HICON,
    pub recording: HICON,
}

impl Icons {
    /// Loads the icons embedded by build.rs at the small-icon size for this
    /// DPI. Falls back to drawn icons if the resources are missing.
    pub fn create() -> Self {
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64);
        let load = |id: u16| unsafe {
            let module = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).ok()?;
            LoadImageW(Some(module.into()), PCWSTR(id as usize as *const u16), IMAGE_ICON, size, size, LR_DEFAULTCOLOR)
                .ok()
                .map(|h| HICON(h.0))
        };
        Self {
            idle: load(1).unwrap_or_else(|| make_icon(size as u32, false)),
            recording: load(2).unwrap_or_else(|| make_icon(size as u32, true)),
        }
    }
}

impl Drop for Icons {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyIcon(self.idle);
            let _ = DestroyIcon(self.recording);
        }
    }
}

/// A round "record" dot: gray ring when idle, red disc when recording.
fn make_icon(size: u32, recording: bool) -> HICON {
    let mut px = vec![0u32; (size * size) as usize];
    let c = (size as f32 - 1.0) / 2.0;
    let r_out = size as f32 * 0.45;
    let r_in = size as f32 * 0.28;
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let edge = (r_out - d + 0.5).clamp(0.0, 1.0);
            if edge <= 0.0 {
                continue;
            }
            let (rgb, alpha) = if recording {
                (0x00e5_3935u32, edge)
            } else {
                let inner = (d - r_in + 0.5).clamp(0.0, 1.0);
                (0x00b0_b0b0, edge * inner.max(0.0))
            };
            let a = (alpha * 255.0) as u32;
            // Premultiplied BGRA.
            let r = ((rgb >> 16) & 0xff) * a / 255;
            let g = ((rgb >> 8) & 0xff) * a / 255;
            let b = (rgb & 0xff) * a / 255;
            px[(y * size + x) as usize] = (a << 24) | (r << 16) | (g << 8) | b;
        }
    }
    unsafe {
        let color = CreateBitmap(size as i32, size as i32, 1, 32, Some(px.as_ptr().cast()));
        let mask_bits = vec![0u8; (size.div_ceil(16) * 2 * size) as usize];
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, Some(mask_bits.as_ptr().cast()));
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}

fn copy_into(dst: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

fn base(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: TRAY_ID, ..Default::default() }
}

pub fn add(hwnd: HWND, msg: u32, icon: HICON, tip: &str) {
    let mut d = base(hwnd);
    d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    d.uCallbackMessage = msg;
    d.hIcon = icon;
    copy_into(&mut d.szTip, tip);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &d);
        d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &d);
    }
}

pub fn update(hwnd: HWND, icon: HICON, tip: &str) {
    let mut d = base(hwnd);
    d.uFlags = NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    d.hIcon = icon;
    copy_into(&mut d.szTip, tip);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
    }
}

pub fn balloon(hwnd: HWND, title: &str, text: &str, warning: bool, icon: HICON) {
    let mut d = base(hwnd);
    d.uFlags = NIF_INFO;
    copy_into(&mut d.szInfoTitle, title);
    copy_into(&mut d.szInfo, text);
    d.dwInfoFlags = if warning { NIIF_WARNING } else { NIIF_USER } | NIIF_NOSOUND;
    d.hBalloonIcon = icon;
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
    }
}

pub fn remove(hwnd: HWND) {
    let d = base(hwnd);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &d);
    }
}
