//! Sharing logic that is the same on every platform: Streamable uploads,
//! base64, and reading a clip's duration. The HTTP transport, clipboard and
//! secret storage live in `sys`.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::config::ShareConfig;
use crate::sys::net::{self, Part};

pub const USER_AGENT: &str = concat!("Replay/", env!("CARGO_PKG_VERSION"));

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

/// Uploads a clip to Streamable. Returns the share URL.
pub fn upload_streamable(path: &Path, email: &str, password: &str, progress: &dyn Fn(u64, u64)) -> Result<String, String> {
    if email.is_empty() || password.is_empty() {
        return Err("add your Streamable email and password in Settings > Sharing".into());
    }
    let name = path.file_name().map(|n| n.to_string_lossy().replace('"', "'")).unwrap_or_else(|| "clip.mp4".into());
    let title = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let boundary = format!("----ReplayBoundary{:016x}", crate::util::now_ticks());
    let prefix = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\n{title}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: video/mp4\r\n\r\n"
    );
    let suffix = format!("\r\n--{boundary}--\r\n");
    let auth = base64_encode(format!("{email}:{password}").as_bytes());
    let headers = vec![
        ("Authorization".to_string(), format!("Basic {auth}")),
        ("Content-Type".to_string(), format!("multipart/form-data; boundary={boundary}")),
    ];
    let parts = [Part::Bytes(prefix.into_bytes()), Part::File(path.to_path_buf()), Part::Bytes(suffix.into_bytes())];
    let r = net::request("POST", "https://api.streamable.com/upload", &headers, &parts, progress)?;
    let text = String::from_utf8_lossy(&r.body);
    match r.status {
        200 | 201 => {
            let v: serde_json::Value = serde_json::from_str(&text).map_err(|_| format!("unexpected reply: {text}"))?;
            let code = v.get("shortcode").and_then(|c| c.as_str()).ok_or_else(|| format!("unexpected reply: {text}"))?;
            Ok(format!("https://streamable.com/{code}"))
        }
        401 => Err("Streamable rejected the email or password".into()),
        s => Err(format!("Streamable returned HTTP {s}: {}", text.chars().take(200).collect::<String>())),
    }
}

/// Upload on a background thread with the stored credentials.
pub fn upload_in_background(path: PathBuf, cfg: ShareConfig, done: impl FnOnce(Result<String, String>) + Send + 'static) {
    std::thread::Builder::new()
        .name("upload".into())
        .spawn(move || {
            let password = crate::sys::secret::unprotect(&cfg.streamable_password_protected).unwrap_or_default();
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
}
