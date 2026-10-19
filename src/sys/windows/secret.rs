//! Secrets encrypted with DPAPI for the current Windows user.

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData};
use windows::core::w;

use crate::share::{base64_decode, base64_encode};

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
}

/// Encrypts for the current Windows user; returns base64 for the config file.
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

#[cfg(test)]
mod tests {
    #[test]
    fn round_trip() {
        let p = super::protect("hunter2");
        assert_eq!(super::unprotect(&p).as_deref(), Some("hunter2"));
    }
}
