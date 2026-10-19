//! Sharing: clipboard, Explorer, and uploads to Streamable over WinHTTP.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};
use windows::Win32::UI::Shell::DROPFILES;
use windows::core::{PCWSTR, w};

use crate::config::ShareConfig;

/// Opens Explorer with the file selected.
pub fn reveal(path: &Path) {
    let _ = std::process::Command::new("explorer.exe").arg(format!("/select,{}", path.display())).spawn();
}

/// Reads the movie duration (seconds) from an MP4 written by this app.
pub fn duration_hint(path: &Path) -> Option<u64> {
    let mut f = File::open(path).ok()?;
    let mut head = vec![0u8; 4 << 20];
    let n = f.read(&mut head).ok()?;
    let head = &head[..n];
    let i = head.windows(4).position(|w| w == b"mvhd")?;
    let b = &head[i + 4..];
    let (scale, dur) = if b[0] == 1 {
        (u32::from_be_bytes(b[20..24].try_into().ok()?) as u64, u64::from_be_bytes(b[24..32].try_into().ok()?))
    } else {
        (u32::from_be_bytes(b[12..16].try_into().ok()?) as u64, u32::from_be_bytes(b[16..20].try_into().ok()?) as u64)
    };
    (scale > 0).then(|| dur / scale)
}

unsafe fn set_clipboard(format: u32, bytes: &[u8]) -> bool {
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
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };
    with_clipboard(|| unsafe {
        set_clipboard(u32::from(CF_UNICODETEXT.0), bytes);
    })
}

/// Puts the file on the clipboard, so Ctrl+V pastes it in Discord or Explorer.
pub fn copy_file(path: &Path) -> bool {
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

// ------------------------------------------------------------ secrets

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
}

/// Encrypts with DPAPI for the current Windows user; returns base64.
pub fn protect(secret: &str) -> String {
    if secret.is_empty() {
        return String::new();
    }
    unsafe {
        let input = blob(secret.as_bytes());
        let mut out = CRYPT_INTEGER_BLOB::default();
        if CryptProtectData(&input, w!("Replay"), None, None, None, 0, &mut out).is_err() {
            return String::new();
        }
        let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData.cast())));
        base64_encode(&bytes)
    }
}

pub fn unprotect(stored: &str) -> Option<String> {
    let bytes = base64_decode(stored)?;
    unsafe {
        let input = blob(&bytes);
        let mut out = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(&input, None, None, None, None, 0, &mut out).ok()?;
        let s = String::from_utf8_lossy(std::slice::from_raw_parts(out.pbData, out.cbData as usize)).into_owned();
        let _ = LocalFree(Some(HLOCAL(out.pbData.cast())));
        Some(s)
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                s.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for ch in s.bytes().filter(|b| !b.is_ascii_whitespace()) {
        if ch == b'=' {
            break;
        }
        let v = B64.iter().position(|&b| b == ch)? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

// ------------------------------------------------------------- upload

struct Internet(*mut core::ffi::c_void);

impl Drop for Internet {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn win_err(what: &str) -> String {
    format!("{what} failed: {}", windows::core::Error::from_thread())
}

/// Uploads a clip to Streamable. Returns the share URL.
pub fn upload_streamable(path: &Path, email: &str, password: &str, progress: &dyn Fn(u64, u64)) -> Result<String, String> {
    if email.is_empty() || password.is_empty() {
        return Err("add your Streamable email and password in Settings > Sharing".into());
    }
    let file_len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    let name = path.file_name().map(|n| n.to_string_lossy().replace('"', "'")).unwrap_or_else(|| "clip.mp4".into());
    let title = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let boundary = format!("----ReplayBoundary{:016x}", crate::util::now_ticks());
    let prefix = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\n{title}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: video/mp4\r\n\r\n"
    );
    let suffix = format!("\r\n--{boundary}--\r\n");
    let total = prefix.len() as u64 + file_len + suffix.len() as u64;
    let auth = base64_encode(format!("{email}:{password}").as_bytes());
    let headers = format!("Authorization: Basic {auth}\r\nContent-Type: multipart/form-data; boundary={boundary}\r\n");

    unsafe {
        let session = Internet(WinHttpOpen(
            w!("Replay/0.1"),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if session.0.is_null() {
            return Err(win_err("WinHttpOpen"));
        }
        // Resolve, connect, send, receive: 30 s each; large uploads keep sending.
        let _ = WinHttpSetTimeouts(session.0, 30_000, 30_000, 120_000, 300_000);
        let conn = Internet(WinHttpConnect(session.0, w!("api.streamable.com"), INTERNET_DEFAULT_HTTPS_PORT, 0));
        if conn.0.is_null() {
            return Err(win_err("connect"));
        }
        let req = Internet(WinHttpOpenRequest(
            conn.0,
            w!("POST"),
            w!("/upload"),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if req.0.is_null() {
            return Err(win_err("open request"));
        }
        let wh: Vec<u16> = headers.encode_utf16().collect();
        WinHttpAddRequestHeaders(req.0, &wh, WINHTTP_ADDREQ_FLAG_ADD).map_err(|e| e.to_string())?;
        let total32 = u32::try_from(total).map_err(|_| "file is larger than 4 GB".to_string())?;
        WinHttpSendRequest(req.0, None, None, 0, total32, 0).map_err(|e| format!("send failed: {e}"))?;

        let mut sent = 0u64;
        let mut write = |data: &[u8]| -> Result<(), String> {
            let mut off = 0;
            while off < data.len() {
                let mut n = 0u32;
                WinHttpWriteData(req.0, Some(data[off..].as_ptr().cast()), (data.len() - off) as u32, &mut n)
                    .map_err(|e| format!("upload interrupted: {e}"))?;
                off += n as usize;
            }
            sent += data.len() as u64;
            progress(sent, total);
            Ok(())
        };
        write(prefix.as_bytes())?;
        let mut f = File::open(path).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            write(&buf[..n])?;
        }
        write(suffix.as_bytes())?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).map_err(|e| format!("no response: {e}"))?;

        let mut status = 0u32;
        let mut len = 4u32;
        let _ = WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&raw mut status).cast()),
            &mut len,
            std::ptr::null_mut(),
        );
        let mut body = Vec::new();
        loop {
            let mut chunk = [0u8; 8192];
            let mut n = 0u32;
            if WinHttpReadData(req.0, chunk.as_mut_ptr().cast(), chunk.len() as u32, &mut n).is_err() || n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n as usize]);
        }
        let text = String::from_utf8_lossy(&body);
        match status {
            200 | 201 => {
                let v: serde_json::Value = serde_json::from_str(&text).map_err(|_| format!("unexpected reply: {text}"))?;
                let code = v.get("shortcode").and_then(|c| c.as_str()).ok_or_else(|| format!("unexpected reply: {text}"))?;
                Ok(format!("https://streamable.com/{code}"))
            }
            401 => Err("Streamable rejected the email or password".into()),
            _ => Err(format!("Streamable returned HTTP {status}: {}", text.chars().take(200).collect::<String>())),
        }
    }
}

/// Upload on a background thread with the stored credentials.
pub fn upload_in_background(path: PathBuf, cfg: ShareConfig, done: impl FnOnce(Result<String, String>) + Send + 'static) {
    std::thread::Builder::new()
        .name("upload".into())
        .spawn(move || {
            let password = unprotect(&cfg.streamable_password_protected).unwrap_or_default();
            let r = upload_streamable(&path, &cfg.streamable_email, &password, &|_, _| {});
            match &r {
                Ok(url) => log::info!("uploaded {} to {url}", path.display()),
                Err(e) => log::warn!("upload of {} failed: {e}", path.display()),
            }
            done(r);
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for s in ["", "f", "fo", "foo", "foob", "fooba", "foobar"] {
            assert_eq!(base64_decode(&base64_encode(s.as_bytes())).unwrap(), s.as_bytes());
        }
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
    }

    #[test]
    fn dpapi_round_trip() {
        let p = protect("hunter2");
        assert_eq!(unprotect(&p).as_deref(), Some("hunter2"));
    }
}
