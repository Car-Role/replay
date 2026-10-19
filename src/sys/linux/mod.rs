//! Linux backend: XDG portal + PipeWire screen capture, GStreamer encoding
//! (NVENC or VA-API when present), PulseAudio/PipeWire audio, and the
//! shared tray recorder.

pub mod audio;
pub mod capture;
pub mod detect;
pub mod gst;
pub mod player;

use std::sync::{Arc, Mutex};

pub use crate::sys::unix::{net, secret, shell};
use crate::config::{Codec, Config};
use crate::ring::PacketRing;
use crate::sys::unix::app::{Backend, GameInfo, Recording, Stop};

pub const PLATFORM: &str = "linux";

pub mod aac {
    pub use super::audio::AacEncoder;
    pub use crate::sys::unix::aac_decode::AacDecoder;
}

pub fn list_microphones() -> Vec<(String, String)> {
    audio::list_microphones()
}

pub fn available_codecs() -> Vec<(Codec, bool)> {
    Codec::ALL.iter().map(|&c| (c, gst::encoder_for(c).is_some())).collect()
}

pub fn init_ui() {
    let _ = gst::init();
}

pub fn init_tool() {
    let _ = gst::init();
}

/// Records the chosen monitor while any game runs.
struct LinuxBackend {
    detector: detect::Detector,
}

impl Backend for LinuxBackend {
    fn running_games(&mut self, cfg: &Config) -> Vec<GameInfo> {
        let found = self.detector.scan(&cfg.detection);
        let Some((pid, exe, name)) = found.first().cloned() else { return Vec::new() };
        // One recording of the screen covers every running game.
        vec![GameInfo { key: "screen".into(), pid, exe, name }]
    }

    fn focused_key(&self) -> Option<String> {
        Some("screen".into())
    }

    fn start(&mut self, game: &GameInfo, cfg: &Config) -> Result<Box<dyn Recording>, String> {
        let portal = capture::Portal::open()?;
        Ok(Box::new(capture::LinuxRecording::start(game, cfg, portal)?))
    }

    fn start_mic(&mut self, cfg: &Config, ring: Arc<Mutex<PacketRing>>) -> Result<Box<dyn Stop>, String> {
        Ok(Box::new(audio::capture(&cfg.audio.microphone_device, cfg.audio.bitrate_kbps, ring)?))
    }

    fn config_changed(&mut self) {
        self.detector.forget();
    }
}

pub mod cli {
    use super::*;

    pub fn prepare(_text_mode: bool) {}

    pub fn handle(mode: &str, args: &[String]) -> bool {
        let _ = args;
        match mode {
            "--probe" => {
                crate::logger::init("probe", true);
                match gst::init() {
                    Ok(()) => println!("GStreamer: available"),
                    Err(e) => println!("GStreamer: {e}"),
                }
                for c in Codec::ALL {
                    match gst::encoder_for(c) {
                        Some(e) => println!("{:>12}: {e}{}", c.label(), if gst::is_hardware(e) { " (hardware)" } else { " (software)" }),
                        None => println!("{:>12}: none", c.label()),
                    }
                }
                for (id, name) in list_microphones() {
                    println!("mic: {name}  [{id}]");
                }
                println!("config: {}", Config::path().display());
            }
            "--detect" => {
                crate::logger::init("probe", false);
                let cfg = Config::load();
                let found = detect::Detector::new().scan(&cfg.detection);
                if found.is_empty() {
                    println!("No games are running.");
                }
                for (pid, exe, name) in found {
                    println!("GAME  {name}  ({exe}, pid {pid})");
                }
            }
            _ => return false,
        }
        true
    }

    pub fn send_save(all: bool) -> bool {
        crate::sys::unix::ipc::send(crate::ipc::RecorderMsg::Save(all))
    }

    pub fn run_recorder() {
        if let Err(e) = gst::init() {
            log::error!("{e}");
            crate::sys::unix::app::notify("Replay cannot record", &format!("{e}. Install the GStreamer packages listed on the download page."));
        }
        crate::sys::unix::app::run(Box::new(LinuxBackend { detector: detect::Detector::new() }));
    }
}
