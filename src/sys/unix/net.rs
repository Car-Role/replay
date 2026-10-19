//! HTTPS over ureq (Rust TLS), with streamed uploads and downloads.

use std::fs::File;
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub enum Part {
    Bytes(Vec<u8>),
    File(PathBuf),
}

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30 * 60)))
        .user_agent(crate::share::USER_AGENT)
        .build()
        .into()
}

/// Reads the parts in order and reports progress.
struct PartsReader<'a> {
    readers: Vec<Box<dyn Read + 'a>>,
    sent: u64,
    total: u64,
    progress: &'a dyn Fn(u64, u64),
}

impl Read for PartsReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while let Some(r) = self.readers.first_mut() {
            let n = r.read(buf)?;
            if n > 0 {
                self.sent += n as u64;
                (self.progress)(self.sent, self.total);
                return Ok(n);
            }
            self.readers.remove(0);
        }
        Ok(0)
    }
}

pub fn request(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: &[Part],
    progress: &dyn Fn(u64, u64),
) -> Result<Response, String> {
    let mut total = 0u64;
    let mut readers: Vec<Box<dyn Read>> = Vec::new();
    for p in body {
        match p {
            Part::Bytes(b) => {
                total += b.len() as u64;
                readers.push(Box::new(Cursor::new(b.clone())));
            }
            Part::File(path) => {
                let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
                total += f.metadata().map(|m| m.len()).unwrap_or(0);
                readers.push(Box::new(f));
            }
        }
    }
    let agent = agent();
    let resp = if method == "GET" {
        let mut req = agent.get(url);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        req.call()
    } else {
        let mut req = agent.post(url);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        req = req.header("Content-Length", total.to_string());
        let mut reader = PartsReader { readers, sent: 0, total, progress };
        req.send(ureq::SendBody::from_reader(&mut reader))
    };
    let mut resp = resp.map_err(|e| format!("request to {url} failed: {e}"))?;
    let status = resp.status().as_u16();
    let body = resp.body_mut().with_config().limit(64 << 20).read_to_vec().map_err(|e| e.to_string())?;
    Ok(Response { status, body })
}

/// Downloads `url` (following redirects) into `dest`.
pub fn download(url: &str, headers: &[(String, String)], dest: &Path, progress: &dyn Fn(u64, u64)) -> Result<(), String> {
    let mut req = agent().get(url);
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let mut resp = req.call().map_err(|e| format!("download failed: {e}"))?;
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(format!("download failed with HTTP {status}"));
    }
    let total = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut reader = resp.body_mut().with_config().limit(1 << 30).reader();
    let mut file = File::create(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut got = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        got += n as u64;
        progress(got, total);
    }
    file.sync_all().map_err(|e| e.to_string())
}
