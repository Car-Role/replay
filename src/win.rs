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

pub fn notify_recorder(msg: u32) {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW};
    unsafe {
        if let Ok(h) = FindWindowW(crate::app::WINDOW_CLASS, None) {
            let _ = PostMessageW(Some(h), msg, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn open_path(p: &Path) {
    let _ = std::process::Command::new("explorer.exe").arg(p).spawn();
}
