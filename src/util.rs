//! Shared helpers: clock, strings, thread setup.

use std::sync::OnceLock;

use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadDescription, SetThreadPriority, THREAD_PRIORITY};
use windows::core::HSTRING;

use crate::ring::Ticks;

fn qpc_freq() -> i64 {
    static F: OnceLock<i64> = OnceLock::new();
    *F.get_or_init(|| {
        let mut f = 0;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut f);
        }
        f.max(1)
    })
}

/// Now, in 100 ns ticks on the QPC clock. Matches the clock of WGC frame
/// times and WASAPI QPC positions.
pub fn now_ticks() -> Ticks {
    let mut c = 0;
    unsafe {
        let _ = QueryPerformanceCounter(&mut c);
    }
    let f = qpc_freq();
    ((c as i128) * 10_000_000 / f as i128) as Ticks
}

pub fn local_time() -> SYSTEMTIME {
    unsafe { GetLocalTime() }
}

/// Null-terminated UTF-16 for Win32 calls.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

pub fn set_thread(name: &str, priority: THREAD_PRIORITY) {
    unsafe {
        let t = GetCurrentThread();
        let _ = SetThreadDescription(t, &HSTRING::from(name));
        let _ = SetThreadPriority(t, priority);
    }
}

/// Makes a string safe as a file or folder name.
pub fn sanitize_file_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.').trim();
    let mut out: String = trimmed.chars().take(80).collect();
    if out.is_empty() {
        out = "Clip".into();
    }
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1",
        "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(&out)) {
        out.push('_');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_file_name("Half-Life: Alyx"), "Half-Life_ Alyx");
        assert_eq!(sanitize_file_name("  ..  "), "Clip");
        assert_eq!(sanitize_file_name("con"), "con_");
    }
}
