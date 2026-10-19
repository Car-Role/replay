//! HTTPS over WinHTTP (built into Windows, uses the system proxy settings).

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use windows::Win32::Networking::WinHttp::*;
use windows::core::{HSTRING, PCWSTR};

pub enum Part {
    Bytes(Vec<u8>),
    File(PathBuf),
}

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

struct Handle(*mut core::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn last_error(what: &str) -> String {
    format!("{what} failed: {}", windows::core::Error::from_thread())
}

/// Splits `https://host[:port]/path` into its parts.
fn split_url(url: &str) -> Result<(bool, String, u16, String), String> {
    let (secure, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(format!("unsupported URL: {url}"));
    };
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().map_err(|_| format!("bad port in {url}"))?),
        None => (hostport, if secure { 443 } else { 80 }),
    };
    Ok((secure, host.to_string(), port, path.to_string()))
}

fn part_len(p: &Part) -> Result<u64, String> {
    match p {
        Part::Bytes(b) => Ok(b.len() as u64),
        Part::File(f) => std::fs::metadata(f).map(|m| m.len()).map_err(|e| format!("{}: {e}", f.display())),
    }
}

/// Sends a request, streaming `body`, and passes the response body to `sink`.
fn exchange(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: &[Part],
    up: &dyn Fn(u64, u64),
    sink: &mut dyn FnMut(&[u8], u64) -> Result<(), String>,
) -> Result<u16, String> {
    let (secure, host, port, path) = split_url(url)?;
    let total: u64 = body.iter().map(part_len).sum::<Result<u64, String>>()?;
    let total32 = u32::try_from(total).map_err(|_| "request body is larger than 4 GB".to_string())?;
    unsafe {
        let session = Handle(WinHttpOpen(
            &HSTRING::from(crate::share::USER_AGENT),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if session.0.is_null() {
            return Err(last_error("WinHttpOpen"));
        }
        let _ = WinHttpSetTimeouts(session.0, 30_000, 30_000, 120_000, 300_000);
        let conn = Handle(WinHttpConnect(session.0, &HSTRING::from(host.as_str()), port, 0));
        if conn.0.is_null() {
            return Err(last_error("connect"));
        }
        let req = Handle(WinHttpOpenRequest(
            conn.0,
            &HSTRING::from(method),
            &HSTRING::from(path.as_str()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            if secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) },
        ));
        if req.0.is_null() {
            return Err(last_error("open request"));
        }
        if !headers.is_empty() {
            let text: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
            let wide: Vec<u16> = text.encode_utf16().collect();
            WinHttpAddRequestHeaders(req.0, &wide, WINHTTP_ADDREQ_FLAG_ADD).map_err(|e| e.to_string())?;
        }
        WinHttpSendRequest(req.0, None, None, 0, total32, 0).map_err(|e| format!("cannot reach {host}: {e}"))?;

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
            up(sent, total);
            Ok(())
        };
        for p in body {
            match p {
                Part::Bytes(b) => write(b)?,
                Part::File(path) => {
                    let mut f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
                    let mut buf = vec![0u8; 1 << 20];
                    loop {
                        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
                        if n == 0 {
                            break;
                        }
                        write(&buf[..n])?;
                    }
                }
            }
        }
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).map_err(|e| format!("no response from {host}: {e}"))?;

        let query_num = |flag: u32| -> Option<u64> {
            let mut buf = [0u16; 32];
            let mut len = (buf.len() * 2) as u32;
            WinHttpQueryHeaders(req.0, flag, PCWSTR::null(), Some(buf.as_mut_ptr().cast()), &mut len, std::ptr::null_mut()).ok()?;
            String::from_utf16_lossy(&buf[..len as usize / 2]).trim().parse().ok()
        };
        let status = query_num(WINHTTP_QUERY_STATUS_CODE).unwrap_or(0) as u16;
        let length = query_num(WINHTTP_QUERY_CONTENT_LENGTH).unwrap_or(0);
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let mut n = 0u32;
            WinHttpReadData(req.0, chunk.as_mut_ptr().cast(), chunk.len() as u32, &mut n)
                .map_err(|e| format!("download interrupted: {e}"))?;
            if n == 0 {
                break;
            }
            sink(&chunk[..n as usize], length)?;
        }
        Ok(status)
    }
}

pub fn request(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: &[Part],
    progress: &dyn Fn(u64, u64),
) -> Result<Response, String> {
    let mut out = Vec::new();
    let status = exchange(method, url, headers, body, progress, &mut |d, _| {
        out.extend_from_slice(d);
        Ok(())
    })?;
    Ok(Response { status, body: out })
}

/// Downloads `url` (following redirects) into `dest`.
pub fn download(url: &str, headers: &[(String, String)], dest: &Path, progress: &dyn Fn(u64, u64)) -> Result<(), String> {
    let mut file = File::create(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let mut got = 0u64;
    let status = exchange("GET", url, headers, &[], &|_, _| {}, &mut |d, total| {
        file.write_all(d).map_err(|e| e.to_string())?;
        got += d.len() as u64;
        progress(got, total);
        Ok(())
    })?;
    if status != 200 {
        let _ = std::fs::remove_file(dest);
        return Err(format!("download failed with HTTP {status}"));
    }
    file.sync_all().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_urls() {
        assert_eq!(split_url("https://api.github.com/repos/a/b").unwrap(), (true, "api.github.com".into(), 443, "/repos/a/b".into()));
        assert_eq!(split_url("http://localhost:8080").unwrap(), (false, "localhost".into(), 8080, "/".into()));
        assert!(split_url("ftp://x").is_err());
    }
}
