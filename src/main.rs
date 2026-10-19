#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod capture;
mod clip;
mod config;
mod detect;
mod edit;
mod encoder;
mod gpu;
mod hotkey;
mod logger;
mod mux;
mod paths;
mod ring;
mod session;
mod share;
mod tray;
mod util;
mod verify;
mod win;
mod player;
mod ui;

use std::time::Duration;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, WPARAM};
use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_NOSOCKET, MFShutdown, MFStartup};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW};
use windows::core::w;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("");
    if matches!(mode, "--probe" | "--selftest" | "--detect" | "--verify" | "--trim" | "--join" | "--help" | "-h") {
        // Release builds have no console of their own; borrow the parent's.
        unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    match mode {
        "--probe" => {
            logger::init("probe", true);
            init_media(true);
            probe();
        }
        "--selftest" => {
            logger::init("selftest", true);
            init_media(true);
            let secs = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(8);
            let codec = args.get(2).map(String::as_str);
            std::process::exit(if selftest(secs, codec) { 0 } else { 1 });
        }
        "--library" | "--settings" => {
            logger::init("library", cfg!(debug_assertions));
            // winit needs a single-threaded COM apartment on this thread.
            init_media(false);
            ui::run(args.get(1).map(std::path::PathBuf::from), mode == "--settings");
        }
        "--save" | "--save-all" => {
            unsafe {
                match FindWindowW(app::WINDOW_CLASS, None) {
                    Ok(h) => {
                        let all = usize::from(mode == "--save-all");
                        let _ = PostMessageW(Some(h), app::WM_APP_SAVE, WPARAM(all), LPARAM(0));
                    }
                    Err(_) => std::process::exit(2),
                }
            }
        }
        "--verify" => {
            logger::init("probe", false);
            init_media(true);
            let Some(path) = args.get(1) else { return };
            match verify::check(std::path::Path::new(path)) {
                Ok(streams) => {
                    for r in streams {
                        println!(
                            "stream {} {}: {} samples, {} decoded, {:.2}-{:.2} s{}",
                            r.index, r.major, r.samples, r.decoded, r.first_time as f64 / 1e7, r.last_time as f64 / 1e7,
                            if r.major == "audio" { format!(", peak {:.1} dBFS", 20.0 * (r.peak.max(1) as f64 / 32768.0).log10()) } else { String::new() }
                        );
                    }
                }
                Err(e) => {
                    println!("FAIL: {e}");
                    std::process::exit(1);
                }
            }
        }
        "--trim" | "--join" => {
            logger::init("probe", false);
            init_media(true);
            let segments: Vec<edit::Segment> = if mode == "--trim" {
                let (Some(p), Some(a), Some(b)) = (args.get(1), args.get(2), args.get(3)) else {
                    println!("usage: replay --trim <file> <start s> <end s>");
                    return;
                };
                vec![edit::Segment { path: p.into(), start: a.parse().unwrap_or(0.0), end: b.parse().unwrap_or(f64::MAX) }]
            } else {
                args[1..].iter().map(|p| edit::Segment { path: p.into(), start: 0.0, end: f64::MAX }).collect()
            };
            let Some(first) = segments.first() else { return };
            let out = edit::edited_name(&first.path, if mode == "--trim" { "trim" } else { "joined" });
            let cfg = config::Config::load();
            let mix = edit::Mix { game: cfg.audio.game_volume, mic: cfg.audio.microphone_volume, keep_separate: true, bitrate_kbps: cfg.audio.bitrate_kbps };
            let t = std::time::Instant::now();
            match edit::export(&segments, &out, &mix, &|_| {}) {
                Ok(p) => println!("wrote {} in {} ms", p.display(), t.elapsed().as_millis()),
                Err(e) => {
                    println!("FAIL: {e}");
                    std::process::exit(1);
                }
            }
        }
        "--detect" => {
            logger::init("probe", false);
            detect_report();
        }
        "--help" | "-h" => {
            println!("replay [--save | --save-all | --probe | --detect | --selftest [seconds] [h264|hevc|av1] | --library]");
        }
        _ => {
            logger::init("replay", cfg!(debug_assertions));
            log::info!("Replay {} starting", env!("CARGO_PKG_VERSION"));
            if !single_instance() {
                log::info!("already running; asking it to show the library");
                return;
            }
            init_media(true);
            if !capture::is_supported() {
                log::error!("Windows.Graphics.Capture is not supported on this system");
            }
            if let Err(e) = app::run() {
                log::error!("fatal: {e}");
            }
            unsafe {
                let _ = MFShutdown();
            }
        }
    }
}

fn init_media(mta: bool) {
    unsafe {
        let _ = CoInitializeEx(None, if mta { COINIT_MULTITHREADED } else { COINIT_APARTMENTTHREADED });
        if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) {
            log::error!("Media Foundation is not available: {e}");
        }
    }
}

/// Returns false (after pinging the running copy) when Replay already runs.
fn single_instance() -> bool {
    unsafe {
        let m = CreateMutexW(None, false, w!("Local\\Replay.Recorder.Instance"));
        if m.is_ok() && GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(h) = FindWindowW(app::WINDOW_CLASS, None) {
                let _ = PostMessageW(Some(h), app::WM_APP_SHOW, WPARAM(0), LPARAM(0));
            }
            return false;
        }
        // Keep the mutex for the life of the process.
        std::mem::forget(m);
        let _ = HWND::default();
        true
    }
}

fn probe() {
    println!("Windows.Graphics.Capture supported: {}", capture::is_supported());
    match gpu::Gpu::create() {
        Ok(g) => {
            println!("GPU: {} (vendor {:04x})", g.name, g.vendor);
            for c in config::Codec::ALL {
                match encoder::find_encoders(c, Some(&g)) {
                    Ok(list) if !list.is_empty() => {
                        let names: Vec<_> = list.iter().map(|(_, n)| n.as_str()).collect();
                        println!("{:>5}: {}", c.label(), names.join(", "));
                    }
                    _ => println!("{:>5}: none", c.label()),
                }
            }
        }
        Err(e) => println!("GPU: error {e}"),
    }
    for (id, name) in audio::list_microphones() {
        println!("mic: {name}  [{id}]");
    }
    let cfg = config::Config::load();
    println!("config: {}", config::Config::path().display());
    println!("clips:  {}", cfg.output_dir.display());
}

/// Records the primary monitor (plus microphone) and verifies the clip.
fn selftest(secs: u64, codec: Option<&str>) -> bool {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};

    let mut cfg = config::Config::load();
    if let Some(c) = codec {
        cfg.video.codec = match c {
            "h264" => config::Codec::H264,
            "hevc" => config::Codec::Hevc,
            _ => config::Codec::Av1,
        };
    }
    cfg.replay_seconds = secs as u32;
    cfg.output_dir = paths::data_dir().join("selftest");
    let gpu = match gpu::Gpu::create() {
        Ok(g) => g,
        Err(e) => {
            println!("FAIL: GPU: {e}");
            return false;
        }
    };
    let mon = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    let fg = unsafe { windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow() };
    let (mw, mh) = detect::monitor_size(fg.0 as isize);
    let out = cfg.video.resolution.output_size(mw, mh);
    let game = session::GameInfo { pid: std::process::id(), hwnd: 0, exe: "selftest".into(), name: "Self test".into() };
    let mut s = session::Session::start(gpu, game, capture::Target::Monitor(mon.0 as isize), &cfg, out, |_, end| {
        if let session::SessionEnd::Failed(e) = end {
            println!("session failed: {e}");
        }
    });
    let mic_ring = std::sync::Arc::new(std::sync::Mutex::new(session::ring_for(&cfg, cfg.audio.bitrate_kbps)));
    let mic_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mic_thread = {
        let (r, st, kbps) = (mic_ring.clone(), mic_stop.clone(), cfg.audio.bitrate_kbps);
        std::thread::spawn(move || audio::run(audio::Source::Microphone(String::new()), kbps, r, st))
    };
    println!("recording the primary monitor for {secs} s with {:?}...", cfg.video.codec);
    let threads_before = thread_times();
    let cpu_before = cpu_time();
    let t = std::time::Instant::now();
    // Play sounds from this process so the process-loopback path has data.
    for _ in 0..secs.div_ceil(2) {
        unsafe {
            use windows::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC};
            let _ = PlaySoundW(w!("SystemAsterisk"), None, SND_ALIAS | SND_ASYNC);
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    std::thread::sleep(Duration::from_secs(secs + 1).saturating_sub(t.elapsed()));
    let wall = t.elapsed().as_secs_f64();
    let cpu = cpu_time() - cpu_before;
    let mem = working_set();
    let threads_after = thread_times();

    let dur = (secs as i64) * ring::TICKS_PER_SEC;
    let video = s.video.lock().unwrap().snapshot(dur);
    let state = s.video_state.lock().unwrap().clone();
    let t0 = video.first().map(|p| p.pts).unwrap_or(0);
    let keys: Vec<i64> = video.iter().filter(|p| p.key).map(|p| p.pts).collect();
    let bytes: usize = video.iter().map(|p| p.data.len()).sum();
    let game_audio = s.game_audio.as_ref().map(|r| r.lock().unwrap().snapshot_from(t0));
    let mic = Some(mic_ring.lock().unwrap().snapshot_from(t0));
    mic_stop.store(true, std::sync::atomic::Ordering::Release);
    let _ = mic_thread.join();
    s.stop();

    println!(
        "video: {} frames, {} keyframes, {:.1} Mbit/s",
        video.len(),
        keys.len(),
        bytes as f64 * 8.0 / secs as f64 / 1e6
    );
    if keys.len() >= 2 {
        let gaps: Vec<f64> = keys.windows(2).map(|w| (w[1] - w[0]) as f64 / 1e7).collect();
        println!("keyframe spacing: {:.2}-{:.2} s", gaps.iter().cloned().fold(f64::MAX, f64::min), gaps.iter().cloned().fold(0.0, f64::max));
    }
    println!(
        "audio packets: game {}, mic {}",
        game_audio.as_ref().map_or(0, Vec::len),
        mic.as_ref().map_or(0, Vec::len)
    );
    println!("process CPU while recording: {:.2}% of one core", cpu / wall * 100.0);
    println!("process memory (working set): {:.1} MB", mem as f64 / 1e6);
    let mut per: Vec<(String, f64)> = threads_after
        .iter()
        .map(|(id, (name, t))| {
            let before = threads_before.get(id).map(|(_, b)| *b).unwrap_or(0.0);
            (if name.is_empty() { format!("thread {id}") } else { name.clone() }, (t - before) / wall * 100.0)
        })
        .filter(|(_, p)| *p >= 0.05)
        .collect();
    per.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (name, p) in per.iter().take(8) {
        println!("  {p:5.2}%  {name}");
    }
    if video.is_empty() {
        println!("FAIL: no video frames (is the screen changing?)");
        return false;
    }
    let src = clip::ClipSource { game_name: "Self test".into(), video, state, game_audio, mic };
    let path = match clip::write_clip(src, &cfg.output_dir, &clip::AudioOptions::from_config(&cfg)) {
        Ok(p) => p,
        Err(e) => {
            println!("FAIL: save: {e}");
            return false;
        }
    };
    println!("saved: {}", path.display());
    match verify::check(&path) {
        Ok(streams) => {
            for r in &streams {
                println!(
                    "  stream {} {}: {} samples read, {} decoded, {:.2}-{:.2} s{}",
                    r.index,
                    r.major,
                    r.samples,
                    r.decoded,
                    r.first_time as f64 / 1e7,
                    r.last_time as f64 / 1e7,
                    if r.major == "audio" { format!(", peak {:.1} dBFS", 20.0 * (r.peak.max(1) as f64 / 32768.0).log10()) } else { String::new() }
                );
            }
            let ok = streams.iter().any(|r| r.major == "video" && r.samples > 0);
            println!("{}", if ok { "PASS" } else { "FAIL: no video samples read back" });
            ok
        }
        Err(e) => {
            println!("FAIL: read back: {e}");
            false
        }
    }
}

fn cpu_time() -> f64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
    }
    let t = |f: FILETIME| ((u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)) as f64 / 1e7;
    t(k) + t(u)
}

fn working_set() -> usize {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut c = PROCESS_MEMORY_COUNTERS { cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
    unsafe {
        let _ = GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb);
    }
    c.WorkingSetSize
}

/// CPU seconds per thread of this process, with thread names.
fn thread_times() -> std::collections::HashMap<u32, (String, f64)> {
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next};
    use windows::Win32::System::Threading::{GetThreadDescription, GetThreadTimes, OpenThread, THREAD_QUERY_LIMITED_INFORMATION};
    let mut out = std::collections::HashMap::new();
    let pid = std::process::id();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) else { return out };
        let mut e = THREADENTRY32 { dwSize: size_of::<THREADENTRY32>() as u32, ..Default::default() };
        let mut ok = Thread32First(snap, &mut e).is_ok();
        while ok {
            if e.th32OwnerProcessID == pid
                && let Ok(h) = OpenThread(THREAD_QUERY_LIMITED_INFORMATION, false, e.th32ThreadID)
            {
                let (mut c, mut x, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
                let _ = GetThreadTimes(h, &mut c, &mut x, &mut k, &mut u);
                let t = |f: FILETIME| ((u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)) as f64 / 1e7;
                let name = GetThreadDescription(h).ok().and_then(|p| p.to_string().ok()).unwrap_or_default();
                out.insert(e.th32ThreadID, (name, t(k) + t(u)));
                let _ = CloseHandle(h);
            }
            ok = Thread32Next(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// Prints the detector's verdict for every visible top-level window.
fn detect_report() {
    use windows::Win32::Foundation::LPARAM;
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible};
    use windows::core::BOOL;
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        let list = unsafe { &mut *(l.0 as *mut Vec<isize>) };
        if unsafe { IsWindowVisible(h) }.as_bool() {
            list.push(h.0 as isize);
        }
        true.into()
    }
    let mut windows_list: Vec<isize> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM((&raw mut windows_list) as isize));
    }
    let cfg = config::Config::load();
    let mut d = detect::Detector::new(cfg.detection.clone());
    for h in windows_list {
        let mut t = [0u16; 128];
        let n = unsafe { GetWindowTextW(HWND(h as _), &mut t) } as usize;
        let title = String::from_utf16_lossy(&t[..n]);
        if title.trim().is_empty() {
            continue;
        }
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(HWND(h as _), Some(&mut pid)) };
        let exe = detect::process_exe(pid).unwrap_or_else(|| "?".into());
        match d.classify(h) {
            Some((g, why)) => println!("GAME  {:<28} {:<26} name=\"{}\" by {why:?}", exe, title.chars().take(26).collect::<String>(), g.name),
            None => println!("  -   {:<28} {}", exe, title.chars().take(40).collect::<String>()),
        }
    }
}
