#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod clip;
mod config;
mod edit;
mod hotkey;
mod ipc;
mod logger;
mod media;
mod mux;
mod paths;
mod ring;
mod share;
mod sys;
mod ui;
mod update;
mod util;

// Short paths for the Windows backend modules, which refer to each other as `crate::<name>`.
#[cfg(windows)]
use sys::windows::{app, audio, capture, detect, encoder, gpu, session, tray, verify};

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
    let text_mode = mode.starts_with("--") && !matches!(mode, "--library" | "--settings" | "--save" | "--save-all");
    sys::cli::prepare(text_mode);
    update::finish_pending();

    match mode {
        "--library" | "--settings" => {
            logger::init("library", cfg!(debug_assertions));
            sys::init_ui();
            ui::run(args.get(1).map(std::path::PathBuf::from), mode == "--settings");
        }
        "--save" | "--save-all" => {
            if !sys::cli::send_save(mode == "--save-all") {
                std::process::exit(2);
            }
        }
        "--trim" | "--join" => {
            logger::init("probe", false);
            sys::init_tool();
            std::process::exit(if edit_cli(mode, &args) { 0 } else { 1 });
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
replay --trim <file> <a> <b>   save a trimmed copy (seconds)
replay --join <file>...        join clips with the same video settings
replay --probe                 show capture and encoder support
replay --detect                show which open windows count as games
replay --selftest [s] [codec]  record the screen and check the result
replay --verify <file>         read a clip back and report each track
replay --version";

fn edit_cli(mode: &str, args: &[String]) -> bool {
    let segments: Vec<edit::Segment> = if mode == "--trim" {
        let (Some(p), Some(a), Some(b)) = (args.get(1), args.get(2), args.get(3)) else {
            println!("usage: replay --trim <file> <start s> <end s>");
            return false;
        };
        vec![edit::Segment { path: p.into(), start: a.parse().unwrap_or(0.0), end: b.parse().unwrap_or(f64::MAX) }]
    } else {
        args[1..].iter().map(|p| edit::Segment { path: p.into(), start: 0.0, end: f64::MAX }).collect()
    };
    let Some(first) = segments.first() else { return false };
    let out = edit::edited_name(&first.path, if mode == "--trim" { "trim" } else { "joined" });
    let cfg = config::Config::load();
    let mix = edit::Mix { game: cfg.audio.game_volume, mic: cfg.audio.microphone_volume, keep_separate: true, bitrate_kbps: cfg.audio.bitrate_kbps };
    let t = std::time::Instant::now();
    match edit::export(&segments, &out, &mix, &|_| {}) {
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
