//! Sharing logic that is the same on every platform: Streamable uploads,
//! base64, and reading a clip's duration. The HTTP transport, clipboard and
//! secret storage live in `sys`.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::config::{CustomUploader, ShareConfig, UploadTarget};
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

/// A multipart form: text fields, then the file.
fn form(path: &Path, fields: &[(String, String)], file_field: &str) -> (String, Vec<Part>) {
    let name = path.file_name().map(|n| n.to_string_lossy().replace('"', "'")).unwrap_or_else(|| "clip.mp4".into());
    let boundary = format!("----ReplayBoundary{:016x}", crate::util::now_ticks());
    let mut prefix = String::new();
    for (k, v) in fields {
        prefix.push_str(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n"));
    }
    prefix.push_str(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{file_field}\"; filename=\"{name}\"\r\nContent-Type: video/mp4\r\n\r\n"));
    let suffix = format!("\r\n--{boundary}--\r\n");
    let parts = vec![Part::Bytes(prefix.into_bytes()), Part::File(path.to_path_buf()), Part::Bytes(suffix.into_bytes())];
    (boundary, parts)
}

fn check_size(path: &Path, limit_mb: u64, service: &str) -> Result<(), String> {
    let size = std::fs::metadata(path).map(|m| m.len()).map_err(|e| e.to_string())?;
    if size > limit_mb * 1_000_000 {
        return Err(format!(
            "{service} takes files up to {limit_mb} MB and this clip is {:.0} MB. Export a smaller copy in the editor first",
            size as f64 / 1e6
        ));
    }
    Ok(())
}

/// Uploads to Catbox (`hours` None) or Litterbox (temporary). Returns the link.
pub fn upload_catbox(path: &Path, hours: Option<u32>, progress: &dyn Fn(u64, u64)) -> Result<String, String> {
    let (url, service, limit) = match hours {
        None => ("https://catbox.moe/user/api.php", "Catbox", 200),
        Some(_) => ("https://litterbox.catbox.moe/resources/internals/api.php", "Litterbox", 1000),
    };
    check_size(path, limit, service)?;
    let mut fields = vec![("reqtype".to_string(), "fileupload".to_string())];
    if let Some(h) = hours {
        let h = [1, 12, 24, 72].into_iter().find(|v| *v >= h).unwrap_or(72);
        fields.push(("time".into(), format!("{h}h")));
    }
    let (boundary, parts) = form(path, &fields, "fileToUpload");
    let headers = vec![("Content-Type".to_string(), format!("multipart/form-data; boundary={boundary}"))];
    let r = net::request("POST", url, &headers, &parts, progress)?;
    let text = String::from_utf8_lossy(&r.body).trim().to_string();
    if r.status == 200 && text.starts_with("https://") {
        Ok(text)
    } else {
        Err(format!("{service} returned HTTP {}: {}", r.status, text.chars().take(200).collect::<String>()))
    }
}

/// Finds `a.b.0.c` in JSON.
fn json_path<'a>(v: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    path.split('.').filter(|p| !p.is_empty()).try_fold(v, |v, key| match key.parse::<usize>() {
        Ok(i) if v.is_array() => v.get(i),
        _ => v.get(key),
    })
}

/// Uploads with a custom (ShareX-style) uploader. Returns the link.
pub fn upload_custom(path: &Path, c: &CustomUploader, headers_text: &str, progress: &dyn Fn(u64, u64)) -> Result<String, String> {
    if c.url.is_empty() {
        return Err("set up the custom uploader in Settings > Sharing first".into());
    }
    let fields: Vec<(String, String)> = c.fields.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let file_field = if c.file_field.is_empty() { "file" } else { c.file_field.as_str() };
    let (boundary, parts) = form(path, &fields, file_field);
    let mut headers = vec![("Content-Type".to_string(), format!("multipart/form-data; boundary={boundary}"))];
    for line in headers_text.lines() {
        if let Some((k, v)) = line.split_once(':')
            && !k.trim().is_empty()
        {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let r = net::request("POST", &c.url, &headers, &parts, progress)?;
    let text = String::from_utf8_lossy(&r.body).trim().to_string();
    if !(200..300).contains(&r.status) {
        return Err(format!("the uploader returned HTTP {}: {}", r.status, text.chars().take(200).collect::<String>()));
    }
    let link = if c.link.trim().is_empty() {
        text.clone()
    } else {
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|_| format!("the reply is not JSON: {}", text.chars().take(200).collect::<String>()))?;
        match json_path(&v, c.link.trim()) {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => return Err(format!("the reply has no \"{}\": {}", c.link, text.chars().take(200).collect::<String>())),
        }
    };
    if link.starts_with("http://") || link.starts_with("https://") {
        Ok(link)
    } else {
        Err(format!("the reply has no link: {}", link.chars().take(200).collect::<String>()))
    }
}

/// Reads a ShareX `.sxcu` file into a custom uploader and its headers text.
pub fn import_sxcu(text: &str) -> Result<(CustomUploader, String), String> {
    let v: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| format!("not a ShareX uploader file: {e}"))?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let method = s("RequestMethod");
    if !method.is_empty() && !method.eq_ignore_ascii_case("POST") {
        return Err(format!("only POST uploaders work here (this one uses {method})"));
    }
    let body = s("Body");
    if !body.is_empty() && body != "MultipartFormData" {
        return Err(format!("only form uploads work here (this one sends {body})"));
    }
    let map = |k: &str| -> Vec<(String, String)> {
        v.get(k).and_then(|x| x.as_object()).map(|o| o.iter().map(|(a, b)| (a.clone(), b.as_str().unwrap_or_default().to_string())).collect()).unwrap_or_default()
    };
    // ShareX writes the link as "{json:data.link}" (or "$json:data.link$" in old files).
    let url_tpl = s("URL");
    let link = url_tpl
        .trim_start_matches("{json:")
        .trim_end_matches('}')
        .trim_start_matches("$json:")
        .trim_end_matches('$')
        .to_string();
    let link = if url_tpl.contains("json:") { link } else { String::new() };
    let headers = map("Headers").into_iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n");
    let up = CustomUploader {
        name: s("Name"),
        url: s("RequestURL"),
        file_field: s("FileFormName"),
        fields: map("Arguments").into_iter().collect(),
        headers_protected: String::new(),
        link,
    };
    if up.url.is_empty() {
        return Err("the file has no RequestURL".into());
    }
    Ok((up, headers))
}

/// Uploads with the chosen service. Returns the link.
pub fn upload(path: &Path, cfg: &ShareConfig, progress: &dyn Fn(u64, u64)) -> Result<String, String> {
    match cfg.target {
        UploadTarget::Streamable => {
            let password = crate::sys::secret::unprotect(&cfg.streamable_password_protected).unwrap_or_default();
            upload_streamable(path, &cfg.streamable_email, &password, progress)
        }
        UploadTarget::Catbox => upload_catbox(path, None, progress),
        UploadTarget::Litterbox => upload_catbox(path, Some(cfg.litterbox_hours), progress),
        UploadTarget::Custom => {
            let headers = crate::sys::secret::unprotect(&cfg.custom.headers_protected).unwrap_or_default();
            upload_custom(path, &cfg.custom, &headers, progress)
        }
    }
}

/// True when the chosen service has what it needs to upload.
pub fn ready(cfg: &ShareConfig) -> Result<(), &'static str> {
    match cfg.target {
        UploadTarget::Streamable if cfg.streamable_email.is_empty() || cfg.streamable_password_protected.is_empty() => {
            Err("Add your Streamable login in Settings > Sharing first.")
        }
        UploadTarget::Custom if cfg.custom.url.is_empty() => Err("Set up the custom uploader in Settings > Sharing first."),
        _ => Ok(()),
    }
}

/// Upload on a background thread with the stored settings.
pub fn upload_in_background(path: PathBuf, cfg: ShareConfig, done: impl FnOnce(Result<String, String>) + Send + 'static) {
    std::thread::Builder::new()
        .name("upload".into())
        .spawn(move || {
            let r = upload(&path, &cfg, &|_, _| {});
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
    fn sxcu_import_and_json_links() {
        let sx = r#"{"Version":"16.0.1","Name":"Example","DestinationType":"ImageUploader, FileUploader","RequestMethod":"POST",
            "RequestURL":"https://example.com/upload","Headers":{"Authorization":"Bearer abc"},"Body":"MultipartFormData",
            "Arguments":{"expires":"24h"},"FileFormName":"file","URL":"{json:data.files.0.url}"}"#;
        let (up, headers) = import_sxcu(sx).unwrap();
        assert_eq!(up.url, "https://example.com/upload");
        assert_eq!(up.file_field, "file");
        assert_eq!(up.fields.get("expires").map(String::as_str), Some("24h"));
        assert_eq!(up.link, "data.files.0.url");
        assert_eq!(headers, "Authorization: Bearer abc");
        let v: serde_json::Value = serde_json::from_str(r#"{"data":{"files":[{"url":"https://x.test/a.mp4"}]}}"#).unwrap();
        assert_eq!(json_path(&v, &up.link).and_then(|x| x.as_str()), Some("https://x.test/a.mp4"));
        assert!(import_sxcu(r#"{"RequestMethod":"PUT","RequestURL":"https://e.com"}"#).is_err());
    }

    #[test]
    fn base64_round_trip() {
        for s in ["", "f", "fo", "foo", "foob", "fooba", "foobar"] {
            assert_eq!(base64_decode(&base64_encode(s.as_bytes())).unwrap(), s.as_bytes());
        }
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
    }
}
