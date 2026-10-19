//! Shared helpers: clock, local time, strings, thread setup.

/// Local wall-clock time, for file names and logs.
#[derive(Clone, Copy, Debug)]
pub struct LocalTime {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub millis: u16,
}

#[cfg(windows)]
mod imp {
    use std::sync::OnceLock;

    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    use windows::Win32::System::SystemInformation::GetLocalTime;
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadDescription, SetThreadPriority, THREAD_PRIORITY};
    use windows::core::HSTRING;

    use super::LocalTime;
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

    /// QPC clock in 100 ns ticks; the clock of WGC frames and WASAPI positions.
    pub fn now_ticks() -> Ticks {
        let mut c = 0;
        unsafe {
            let _ = QueryPerformanceCounter(&mut c);
        }
        ((c as i128) * 10_000_000 / qpc_freq() as i128) as Ticks
    }

    pub fn local_time() -> LocalTime {
        let t = unsafe { GetLocalTime() };
        LocalTime {
            year: t.wYear,
            month: t.wMonth,
            day: t.wDay,
            hour: t.wHour,
            minute: t.wMinute,
            second: t.wSecond,
            millis: t.wMilliseconds,
        }
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
}

#[cfg(unix)]
mod imp {
    use super::LocalTime;
    use crate::ring::Ticks;

    /// CLOCK_MONOTONIC in 100 ns ticks.
    pub fn now_ticks() -> Ticks {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        unsafe {
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
        }
        ts.tv_sec as Ticks * 10_000_000 + ts.tv_nsec as Ticks / 100
    }

    pub fn local_time() -> LocalTime {
        let mut tv = libc::timeval { tv_sec: 0, tv_usec: 0 };
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe {
            libc::gettimeofday(&mut tv, std::ptr::null_mut());
            libc::localtime_r(&tv.tv_sec, &mut tm);
        }
        LocalTime {
            year: (tm.tm_year + 1900) as u16,
            month: (tm.tm_mon + 1) as u16,
            day: tm.tm_mday as u16,
            hour: tm.tm_hour as u16,
            minute: tm.tm_min as u16,
            second: tm.tm_sec as u16,
            millis: (tv.tv_usec / 1000) as u16,
        }
    }
}

pub use imp::*;

/// Makes a string safe as a file or folder name on every platform.
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

    #[test]
    fn clock_moves_forward() {
        let a = now_ticks();
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(now_ticks() > a);
    }
}
