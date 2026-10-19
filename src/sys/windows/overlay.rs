//! The in-game notice: a small card in a corner of the game's monitor that
//! says "Saving replay", "Replay saved" or "Recording". It is a layered,
//! click-through, topmost window that never takes focus, and it is left out
//! of screen capture, so it never shows up in clips.
//!
//! The card is drawn with GDI into a 32-bit bitmap once per message; only
//! the fade changes after that. Nothing runs while it is hidden.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::config::OverlayCorner;

/// Leaves the notice visible to screen capture (for `--overlay-test`).
pub static SHOW_IN_CAPTURE: AtomicBool = AtomicBool::new(false);

const CLASS: PCWSTR = w!("ReplayOverlay");
const TIMER_FADE: usize = 1;
const FRAME_MS: u32 = 16;
const FADE_IN_MS: u32 = 120;
const FADE_OUT_MS: u32 = 300;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Work in progress (accent ring).
    Busy,
    /// Done (green check).
    Done,
    /// A manual recording runs (red dot). Stays until replaced.
    Recording,
    /// Something failed (red cross).
    Error,
}

struct State {
    hwnd: HWND,
    /// The drawn card, premultiplied BGRA.
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    size: SIZE,
    pos: POINT,
    /// When the card appeared, and how long (ms) it stays before fading out.
    shown: Instant,
    hold: u32,
    visible: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16))
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wparam.0 == TIMER_FADE => {
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    step(st);
                }
            });
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Draws the current fade frame and sets the timer for the next change.
fn step(st: &mut State) {
    let age = st.shown.elapsed().as_millis().min(u128::from(u32::MAX - 1)) as u32;
    let alpha = alpha_at(age, st.hold);
    if alpha == 0 && age > FADE_IN_MS {
        hide(st);
        return;
    }
    present(st, alpha);
    let next = if age < FADE_IN_MS || age >= st.hold {
        FRAME_MS
    } else if st.hold == u32::MAX {
        // Shown until replaced: no timer while it holds.
        unsafe {
            let _ = KillTimer(Some(st.hwnd), TIMER_FADE);
        }
        return;
    } else {
        st.hold - age
    };
    unsafe {
        SetTimer(Some(st.hwnd), TIMER_FADE, next.max(1), None);
    }
}

/// Opacity (0..=255) at `age` ms for a card shown `hold` ms.
fn alpha_at(age: u32, hold: u32) -> u8 {
    if age < FADE_IN_MS {
        return (age * 255 / FADE_IN_MS) as u8;
    }
    if hold == u32::MAX || age < hold {
        return 255;
    }
    let out = age - hold;
    if out >= FADE_OUT_MS { 0 } else { (255 - out * 255 / FADE_OUT_MS) as u8 }
}

fn hide(st: &mut State) {
    unsafe {
        let _ = KillTimer(Some(st.hwnd), TIMER_FADE);
        let _ = ShowWindow(st.hwnd, SW_HIDE);
    }
    st.visible = false;
}

fn present(st: &State, alpha: u8) {
    unsafe {
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: alpha, AlphaFormat: AC_SRC_ALPHA as u8 };
        let src = POINT::default();
        let _ = UpdateLayeredWindow(st.hwnd, None, Some(&st.pos), Some(&st.size), Some(st.dc), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
    }
}

fn create_window() -> Option<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let wc = WNDCLASSW { lpfnWndProc: Some(wnd_proc), hInstance: instance.into(), lpszClassName: CLASS, ..Default::default() };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            CLASS,
            w!("Replay notice"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .ok()?;
        // Keep the card out of every capture, including Replay's own.
        if !SHOW_IN_CAPTURE.load(Ordering::Relaxed)
            && let Err(e) = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)
        {
            log::debug!("overlay: capture exclusion not available: {e}");
        }
        Some(hwnd)
    }
}

/// Shows a notice on the monitor of `near` (a game window), or where the
/// last one was. `hold_ms` of `None` keeps it until the next call or `clear`.
pub fn show(near: Option<isize>, corner: OverlayCorner, kind: Kind, title: &str, detail: &str, hold_ms: Option<u32>) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.is_none() {
            let Some(hwnd) = create_window() else { return };
            let dc = unsafe { CreateCompatibleDC(None) };
            *s = Some(State {
                hwnd,
                dc,
                bitmap: HBITMAP::default(),
                old: HGDIOBJ::default(),
                size: SIZE::default(),
                pos: POINT::default(),
                shown: Instant::now(),
                hold: 0,
                visible: false,
            });
        }
        let st = s.as_mut().expect("created");
        let (work, dpi) = monitor_of(near.map(|h| HWND(h as _)).unwrap_or(st.hwnd));
        let scale = dpi as f32 / 96.0;
        draw(st, scale, kind, title, detail);
        let margin = (24.0 * scale) as i32;
        st.pos = match corner {
            OverlayCorner::TopLeft => POINT { x: work.left + margin, y: work.top + margin },
            OverlayCorner::TopRight => POINT { x: work.right - margin - st.size.cx, y: work.top + margin },
            OverlayCorner::BottomLeft => POINT { x: work.left + margin, y: work.bottom - margin - st.size.cy },
            OverlayCorner::BottomRight => POINT { x: work.right - margin - st.size.cx, y: work.bottom - margin - st.size.cy },
        };
        // A new message on a visible card replaces it without fading in again.
        let now = Instant::now();
        st.shown = if st.visible { now.checked_sub(Duration::from_millis(u64::from(FADE_IN_MS))).unwrap_or(now) } else { now };
        st.hold = hold_ms.map(|h| h + FADE_IN_MS).unwrap_or(u32::MAX);
        unsafe {
            if !st.visible {
                present(st, 0);
                let _ = ShowWindow(st.hwnd, SW_SHOWNOACTIVATE);
            }
            let _ = SetWindowPos(st.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        st.visible = true;
        step(st);
    });
}

/// The monitor's full rectangle (games cover the taskbar) and its DPI.
fn monitor_of(hwnd: HWND) -> (RECT, u32) {
    unsafe {
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut info);
        let (mut dx, mut dy) = (96, 96);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        (info.rcMonitor, dx.max(96))
    }
}

/// Font faces: Windows 11 fonts when installed, else the Windows 10 ones.
fn faces() -> (PCWSTR, PCWSTR) {
    let fonts = std::path::PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| r"C:\Windows".into())).join("Fonts");
    let text = if fonts.join("SegUIVar.ttf").exists() { w!("Segoe UI Variable Text") } else { w!("Segoe UI") };
    let icons = if fonts.join("SegoeIcons.ttf").exists() { w!("Segoe Fluent Icons") } else { w!("Segoe MDL2 Assets") };
    (text, icons)
}

fn font(height: i32, weight: i32, face: PCWSTR) -> HFONT {
    unsafe {
        CreateFontW(-height, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, (FF_DONTCARE.0 | DEFAULT_PITCH.0) as u32, face)
    }
}

fn text_width(dc: HDC, f: HFONT, s: &[u16]) -> i32 {
    unsafe {
        let old = SelectObject(dc, f.into());
        let mut size = SIZE::default();
        let _ = GetTextExtentPoint32W(dc, s, &mut size);
        SelectObject(dc, old);
        size.cx
    }
}

/// Draws the card into the state's bitmap at `scale`.
fn draw(st: &mut State, scale: f32, kind: Kind, title: &str, detail: &str) {
    let px = |v: f32| (v * scale).round() as i32;
    let title_w: Vec<u16> = title.encode_utf16().collect();
    let detail_w: Vec<u16> = detail.encode_utf16().collect();
    unsafe {
        let dc = st.dc;
        let (text_face, icon_face) = faces();
        let f_title = font(px(15.0), FW_SEMIBOLD.0 as i32, text_face);
        let f_detail = font(px(13.0), FW_NORMAL.0 as i32, text_face);
        let f_icon = font(px(16.0), FW_NORMAL.0 as i32, icon_face);
        let text_w = text_width(dc, f_title, &title_w).max(text_width(dc, f_detail, &detail_w));
        let (pad, icon_d, gap) = (px(14.0), px(32.0), px(12.0));
        let width = (pad + icon_d + gap + text_w + pad + px(4.0)).clamp(px(220.0), px(460.0));
        let height = px(64.0);
        let radius = 8.0 * scale;

        // A top-down 32-bit DIB we can read and write directly.
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let Ok(bmp) = CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0) else { return };
        if !st.bitmap.is_invalid() {
            SelectObject(dc, st.old);
            let _ = DeleteObject(st.bitmap.into());
        }
        st.old = SelectObject(dc, bmp.into());
        st.bitmap = bmp;
        st.size = SIZE { cx: width, cy: height };

        // Background (Fluent dark "acrylic-like" card) and a 1 px border.
        let bg = (32u8, 32u8, 36u8);
        let brush = CreateSolidBrush(rgb(bg.0, bg.1, bg.2));
        let full = RECT { left: 0, top: 0, right: width, bottom: height };
        FillRect(dc, &full, brush);
        let _ = DeleteObject(brush.into());

        // Status circle with a glyph.
        let (circle, glyph) = match kind {
            Kind::Busy => ((0x60u8, 0xCDu8, 0xFFu8), '\u{E895}'),
            Kind::Done => ((0x6Cu8, 0xCBu8, 0x5Fu8), '\u{E73E}'),
            Kind::Recording => ((0xFFu8, 0x4Bu8, 0x4Bu8), '\u{E7C8}'),
            Kind::Error => ((0xFFu8, 0x99u8, 0xA4u8), '\u{E711}'),
        };
        let cx = pad + icon_d / 2;
        let cy = height / 2;
        let cbrush = CreateSolidBrush(rgb(circle.0, circle.1, circle.2));
        let pen = CreatePen(PS_NULL, 0, COLORREF(0));
        let ob = SelectObject(dc, cbrush.into());
        let op = SelectObject(dc, pen.into());
        let _ = Ellipse(dc, cx - icon_d / 2, cy - icon_d / 2, cx + icon_d / 2, cy + icon_d / 2);
        SelectObject(dc, ob);
        SelectObject(dc, op);
        let _ = DeleteObject(cbrush.into());
        let _ = DeleteObject(pen.into());

        SetBkMode(dc, TRANSPARENT);
        let mut g: Vec<u16> = glyph.to_string().encode_utf16().collect();
        let of = SelectObject(dc, f_icon.into());
        SetTextColor(dc, rgb(0x10, 0x10, 0x14));
        let mut r = RECT { left: cx - icon_d / 2, top: cy - icon_d / 2, right: cx + icon_d / 2, bottom: cy + icon_d / 2 };
        DrawTextW(dc, &mut g, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);

        let tx = pad + icon_d + gap;
        SelectObject(dc, f_title.into());
        SetTextColor(dc, rgb(0xFF, 0xFF, 0xFF));
        let mut title_buf = title_w.clone();
        let mut r = RECT { left: tx, top: px(12.0), right: width - pad, bottom: px(34.0) };
        DrawTextW(dc, &mut title_buf, &mut r, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
        SelectObject(dc, f_detail.into());
        SetTextColor(dc, rgb(0xC5, 0xC5, 0xCC));
        let mut detail_buf = detail_w.clone();
        let mut r = RECT { left: tx, top: px(34.0), right: width - pad, bottom: px(54.0) };
        DrawTextW(dc, &mut detail_buf, &mut r, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
        SelectObject(dc, of);
        for f in [f_title, f_detail, f_icon] {
            let _ = DeleteObject(f.into());
        }
        let _ = GdiFlush();

        // GDI leaves alpha at 0. Set it from an anti-aliased rounded
        // rectangle, add the border, and premultiply.
        let pixels = std::slice::from_raw_parts_mut(bits.cast::<[u8; 4]>(), (width * height) as usize);
        let (w, h) = (width as f32, height as f32);
        for y in 0..height {
            for x in 0..width {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                // Distance outside the rounded rectangle (negative inside).
                let qx = (fx - w / 2.0).abs() - (w / 2.0 - radius);
                let qy = (fy - h / 2.0).abs() - (h / 2.0 - radius);
                let outside = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius;
                let cover = (0.5 - outside).clamp(0.0, 1.0);
                let p = &mut pixels[(y * width + x) as usize];
                // Border: a light line just inside the edge.
                let border = (1.0 - (outside + scale).abs()).clamp(0.0, 1.0) * 0.18;
                for c in &mut p[..3] {
                    let v = f32::from(*c) * (1.0 - border) + 255.0 * border;
                    *c = (v * cover * 0.94).round() as u8;
                }
                p[3] = (cover * 0.94 * 255.0).round() as u8;
            }
        }
    }
}
