#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod clip;
mod config;
mod edit;
mod games;
mod hotkey;
mod ipc;
mod library;
mod logger;
mod media;
mod mux;
mod paths;
mod record;
mod ring;
mod share;
mod sys;
mod ui;
mod update;
mod util;

// Short paths for the Windows backend modules, which refer to each other as `crate::<name>`.
#[cfg(windows)]
use sys::windows::{app, audio, capture, detect, encoder, gpu, overlay, session, tray, verify, webcam};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // A new version started by the updater waits for the old one to exit.
    if args.first().map(String::as_str) == Some("--after-update") {
        if let Some(pid) = args.get(1).and_then(|p| p.parse().ok()) {
            update::wait_for_exit(pid);
        }
        args.drain(..args.len().min(2));
    }
    let mode = args.first().map(String::as_str).unwrap_or("");
    let text_mode = mode.starts_with("--") && !matches!(mode, "--library" | "--settings" | "--games" | "--save" | "--save-all" | "--record");
    sys::cli::prepare(text_mode);
    update::finish_pending();

    match mode {
        "--library" | "--settings" | "--games" => {
            logger::init("library", cfg!(debug_assertions));
            sys::init_ui();
            ui::run(args.get(1).map(std::path::PathBuf::from), mode.trim_start_matches("--"));
        }
        "--record" => {
            if !sys::shell::send_to_recorder(ipc::RecorderMsg::Record) {
                std::process::exit(2);
            }
        }
        "--save" | "--save-all" => {
            if !sys::cli::send_save(mode == "--save-all") {
                std::process::exit(2);
            }
        }
        "--export" => {
            logger::init("probe", false);
            sys::init_tool();
            std::process::exit(if export_cli(&args) { 0 } else { 1 });
        }
        "--trim" | "--join" => {
            logger::init("probe", false);
            sys::init_tool();
            std::process::exit(if edit_cli(mode, &args) { 0 } else { 1 });
        }
        "--check-update" => {
            logger::init("probe", false);
            std::process::exit(if check_update_cli(&args) { 0 } else { 1 });
        }
        "--version" | "-V" => println!("replay {}", env!("CARGO_PKG_VERSION")),
        "--help" | "-h" => println!("{}", HELP),
        _ => {
            if sys::cli::handle(mode, &args) {
                return;
            }
            logger::init("replay", cfg!(debug_assertions));
            log::info!("Replay {} starting ({})", env!("CARGO_PKG_VERSION"), sys::PLATFORM);
            sys::cli::run_recorder();
        }
    }
}

const HELP: &str = "\
replay                         start the recorder (tray / menu bar)
replay --library [clip]        open the clip library
replay --settings              open the library on the Settings tab
replay --save | --save-all     ask the running recorder to save a replay
replay --record                start or stop recording the game in front
replay --trim <file> <a> <b>   save a trimmed copy (seconds)
replay --join <file>... [--music <file>]
                               join clips with the same video settings
replay --export <file> <a> <b> [--size MB] [--vertical 0..1] [--codec h264|hevc|av1]
                               re-encode a part of a clip (exact cut)
replay --probe                 show capture and encoder support
replay --detect                show which open windows count as games
replay --selftest [s] [codec]  record the screen and check the result
replay --verify <file>         read a clip back and report each track
replay --check-update          download and verify a newer release, if any
       [--as-version <x.y.z>]  act as that version (for testing)
replay --version";

/// Downloads and verifies the latest release without installing it.
fn check_update_cli(args: &[String]) -> bool {
    let current = match args.iter().position(|a| a == "--as-version") {
        Some(i) => args.get(i + 1).map(String::as_str).unwrap_or(update::CURRENT),
        None => update::CURRENT,
    };
    match update::check_against(current) {
        Ok(None) => {
            println!("No release is newer than {current}.");
            true
        }
        Ok(Some(rel)) => {
            println!("Release {} is newer than {current}. Downloading...", rel.version);
            match update::download(&rel, &|_, _| {}) {
                Ok(staged) => {
                    println!("OK: signature and hash verified. Staged at {}", staged.file.display());
                    true
                }
                Err(e) => {
                    println!("FAILED: {e}");
                    false
                }
            }
        }
        Err(e) => {
            println!("FAILED: {e}");
            false
        }
    }
}

/// `replay --export <file> <start> <end> [--size MB] [--vertical X] [--codec C]`
fn export_cli(args: &[String]) -> bool {
    let (Some(p), Some(a), Some(b)) = (args.get(1), args.get(2), args.get(3)) else {
        println!("usage: replay --export <file> <start s> <end s> [--size MB] [--vertical 0..1] [--codec h264|hevc|av1]");
        return false;
    };
    let opt = |name: &str| args.iter().position(|x| x == name).and_then(|i| args.get(i + 1)).cloned();
    let choice = edit::ExportChoice {
        size_mb: opt("--size").and_then(|v| v.parse().ok()),
        vertical: opt("--vertical").and_then(|v| v.parse().ok()),
        codec: opt("--codec").map(|c| match c.as_str() {
            "h264" => config::Codec::H264,
            "hevc" => config::Codec::Hevc,
            _ => config::Codec::Av1,
        }),
    };
    let (start, end) = (a.parse().unwrap_or(0.0), b.parse().unwrap_or(f64::MAX));
    let cfg = config::Config::load();
    let small = choice.size_mb.is_some();
    let audio_kbps = if small { 128 } else { cfg.audio.bitrate_kbps * 3 };
    let seconds = crate::mux::reader::read(std::path::Path::new(p)).map(|m| m.duration()).unwrap_or(0.0).min(end) - start;
    let spec = match edit::encode_spec(std::path::Path::new(p), seconds, audio_kbps, &choice) {
        Ok(s) => s,
        Err(e) => {
            println!("FAIL: {e}");
            return false;
        }
    };
    println!("{spec:?}");
    let mix = edit::Mix {
        game: cfg.audio.game_volume,
        mic: cfg.audio.microphone_volume,
        keep_separate: !small,
        bitrate_kbps: if small { 128 } else { cfg.audio.bitrate_kbps },
        music: None,
    };
    let seg = vec![edit::Segment { path: p.into(), start, end }];
    let out = edit::edited_name(std::path::Path::new(p), "export");
    let t = std::time::Instant::now();
    match edit::export(&seg, &out, &mix, &edit::VideoMode::Encode(spec), &|_| {}) {
        Ok(path) => {
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            println!("wrote {} ({:.2} MB) in {} ms", path.display(), size as f64 / 1e6, t.elapsed().as_millis());
            true
        }
        Err(e) => {
            println!("FAIL: {e}");
            false
        }
    }
}

fn edit_cli(mode: &str, args: &[String]) -> bool {
    let segments: Vec<edit::Segment> = if mode == "--trim" {
        let (Some(p), Some(a), Some(b)) = (args.get(1), args.get(2), args.get(3)) else {
            println!("usage: replay --trim <file> <start s> <end s>");
            return false;
        };
        vec![edit::Segment { path: p.into(), start: a.parse().unwrap_or(0.0), end: b.parse().unwrap_or(f64::MAX) }]
    } else {
        let mut skip = false;
        args[1..]
            .iter()
            .filter(|a| {
                let keep = !skip && *a != "--music";
                skip = *a == "--music";
                keep
            })
            .map(|p| edit::Segment { path: p.into(), start: 0.0, end: f64::MAX })
            .collect()
    };
    let music = args.iter().position(|a| a == "--music").and_then(|i| args.get(i + 1)).map(|p| edit::Music { path: p.into(), volume: 60, fade_out: true });
    let Some(first) = segments.first() else { return false };
    let out = edit::edited_name(&first.path, if mode == "--trim" { "trim" } else { "joined" });
    let cfg = config::Config::load();
    let mix = edit::Mix { game: cfg.audio.game_volume, mic: cfg.audio.microphone_volume, keep_separate: true, bitrate_kbps: cfg.audio.bitrate_kbps, music };
    let t = std::time::Instant::now();
    match edit::export(&segments, &out, &mix, &edit::VideoMode::Copy, &|_| {}) {
        Ok(p) => {
            println!("wrote {} in {} ms", p.display(), t.elapsed().as_millis());
            true
        }
        Err(e) => {
            println!("FAIL: {e}");
            false
        }
    }
}
