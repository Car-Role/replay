//! Small Windows helpers for the library window.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDIBits,
    GetObjectW, HBITMAP,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOS_FILEMUSTEXIST, FOS_PICKFOLDERS,
    FileOpenDialog, IFileOpenDialog, IShellItemImageFactory, SHCreateItemFromParsingName, SHFILEOPSTRUCTW,
    SHFileOperationW, SIGDN_FILESYSPATH, SIIGBF_BIGGERSIZEOK, SIIGBF_RESIZETOFIT,
};
use windows::core::{HSTRING, PCWSTR, w};

/// Windows' own thumbnail (from the shell cache), as RGBA.
pub fn thumbnail(path: &Path, w: i32, h: i32) -> Option<(usize, usize, Vec<u8>)> {
    unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None).ok()?;
        let bmp: HBITMAP = factory.GetImage(SIZE { cx: w, cy: h }, SIIGBF_RESIZETOFIT | SIIGBF_BIGGERSIZEOK).ok()?;
        let mut info = BITMAP::default();
        GetObjectW(bmp.into(), size_of::<BITMAP>() as i32, Some((&raw mut info).cast()));
        let (bw, bh) = (info.bmWidth.max(1) as usize, info.bmHeight.unsigned_abs().max(1) as usize);
        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: bw as i32,
                biHeight: -(bh as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; bw * bh * 4];
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(dc, bmp, 0, bh as u32, Some(px.as_mut_ptr().cast()), &mut bi, DIB_RGB_COLORS);
        let _ = DeleteDC(dc);
        let _ = DeleteObject(bmp.into());
        if lines == 0 {
            return None;
        }
        for p in px.as_chunks_mut::<4>().0 {
            p.swap(0, 2);
            p[3] = 255;
        }
        Some((bw, bh, px))
    }
}

fn dialog(folders: bool, filter: Option<(&str, &str)>) -> Option<PathBuf> {
    unsafe {
        let d: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let mut opts = d.GetOptions().ok()?;
        opts |= FOS_FILEMUSTEXIST;
        if folders {
            opts |= FOS_PICKFOLDERS;
        }
        d.SetOptions(opts).ok()?;
        let (name, spec);
        if let Some((n, s)) = filter {
            name = HSTRING::from(n);
            spec = HSTRING::from(s);
            let f = [COMDLG_FILTERSPEC { pszName: PCWSTR(name.as_ptr()), pszSpec: PCWSTR(spec.as_ptr()) }];
            d.SetFileTypes(&f).ok()?;
        }
        d.Show(None).ok()?;
        let item = d.GetResult().ok()?;
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s.map(PathBuf::from)
    }
}

pub fn pick_folder() -> Option<PathBuf> {
    dialog(true, None)
}

pub fn pick_exe() -> Option<PathBuf> {
    dialog(false, Some(("Programs", "*.exe")))
}

/// Moves files to the Recycle Bin (they can be restored).
pub fn recycle(paths: &[PathBuf]) -> bool {
    if paths.is_empty() {
        return true;
    }
    let mut list: Vec<u16> = Vec::new();
    for p in paths {
        list.extend(p.as_os_str().to_string_lossy().encode_utf16());
        list.push(0);
    }
    list.push(0);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(list.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI).0 as u16,
        ..Default::default()
    };
    unsafe { SHFileOperationW(&mut op) == 0 && !op.fAnyOperationsAborted.as_bool() }
}

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

/// Adds or removes Replay from the per-user startup list.
pub fn set_autostart(on: bool) -> bool {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_SET_VALUE, &mut key).is_err() {
            return false;
        }
        let ok = if on {
            match std::env::current_exe() {
                Ok(exe) => {
                    let value: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain(std::iter::once(0)).collect();
                    let bytes = std::slice::from_raw_parts(value.as_ptr().cast::<u8>(), value.len() * 2);
                    RegSetValueExW(key, w!("Replay"), None, REG_SZ, Some(bytes)).is_ok()
                }
                Err(_) => false,
            }
        } else {
            let r = RegDeleteValueW(key, w!("Replay"));
            r.is_ok() || r == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND
        };
        let _ = RegCloseKey(key);
        ok
    }
}

pub fn recorder_running() -> bool {
    unsafe { windows::Win32::UI::WindowsAndMessaging::FindWindowW(crate::app::WINDOW_CLASS, None).is_ok() }
}

/// Sends a message to the running recorder. False when it is not running.
pub fn send_to_recorder(msg: crate::ipc::RecorderMsg) -> bool {
    use crate::ipc::RecorderMsg;
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW};
    let (id, wparam) = match msg {
        RecorderMsg::Reload => (crate::app::WM_APP_RELOAD, 0),
        RecorderMsg::UpdateStaged => (crate::app::WM_APP_UPDATE_STAGED, 0),
        RecorderMsg::InstallUpdate => (crate::app::WM_APP_INSTALL_UPDATE, 0),
        RecorderMsg::Save(all) => (crate::app::WM_APP_SAVE, usize::from(all)),
        RecorderMsg::Show => (crate::app::WM_APP_SHOW, 0),
    };
    unsafe {
        match FindWindowW(crate::app::WINDOW_CLASS, None) {
            Ok(h) => PostMessageW(Some(h), id, WPARAM(wparam), LPARAM(0)).is_ok(),
            Err(_) => false,
        }
    }
}

/// Opens a web page in the default browser.
pub fn open_url(url: &str) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    if !url.starts_with("https://") {
        return;
    }
    let u = crate::util::wide(url);
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(u.as_ptr()), None, None, SW_SHOWNORMAL);
    }
}

pub fn open_path(p: &Path) {
    let _ = std::process::Command::new("explorer.exe").arg(p).spawn();
}

/// Opens Explorer with the file selected.
pub fn reveal(path: &Path) {
    let _ = std::process::Command::new("explorer.exe").arg(format!("/select,{}", path.display())).spawn();
}

unsafe fn set_clipboard(format: u32, bytes: &[u8]) -> bool {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::SetClipboardData;
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    unsafe {
        let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, bytes.len()) else { return false };
        let p = GlobalLock(h);
        if p.is_null() {
            return false;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(h);
        SetClipboardData(format, Some(HANDLE(h.0))).is_ok()
    }
}

fn with_clipboard(f: impl FnOnce()) -> bool {
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard};
    unsafe {
        // Another app may hold the clipboard briefly.
        for _ in 0..10 {
            if OpenClipboard(None).is_ok() {
                let _ = EmptyClipboard();
                f();
                let _ = CloseClipboard();
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        false
    }
}

pub fn copy_text(text: &str) -> bool {
    use windows::Win32::System::Ole::CF_UNICODETEXT;
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };
    with_clipboard(|| unsafe {
        set_clipboard(u32::from(CF_UNICODETEXT.0), bytes);
    })
}

/// Puts the file on the clipboard, so Ctrl+V pastes it in Discord or Explorer.
pub fn copy_file(path: &Path) -> bool {
    use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
    use windows::Win32::System::Ole::CF_HDROP;
    use windows::Win32::UI::Shell::DROPFILES;
    let header = size_of::<DROPFILES>();
    let mut data = vec![0u8; header];
    let df = DROPFILES { pFiles: header as u32, fWide: true.into(), ..Default::default() };
    unsafe { std::ptr::write_unaligned(data.as_mut_ptr().cast::<DROPFILES>(), df) };
    for c in path.as_os_str().to_string_lossy().encode_utf16().chain([0, 0]) {
        data.extend_from_slice(&c.to_le_bytes());
    }
    let effect = 1u32.to_le_bytes(); // DROPEFFECT_COPY
    with_clipboard(|| unsafe {
        set_clipboard(u32::from(CF_HDROP.0), &data);
        let fmt = RegisterClipboardFormatW(w!("Preferred DropEffect"));
        set_clipboard(fmt, &effect);
    })
}
