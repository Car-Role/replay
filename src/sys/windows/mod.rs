//! Windows backend: WGC capture, Media Foundation (NVENC) encoding, WASAPI
//! audio, and a Win32 tray app.

pub mod app;
pub mod audio;
pub mod capture;
pub mod cli;
pub mod detect;
pub mod encoder;
pub mod gpu;
pub mod hdr;
pub mod net;
pub mod overlay;
pub mod player;
pub mod secret;
pub mod session;
pub mod shell;
pub mod transcode;
pub mod tray;
pub mod verify;
pub mod webcam;

use crate::config::Codec;

pub const PLATFORM: &str = "windows";

/// AAC through the Media Foundation encoder and decoder.
pub mod aac {
    pub struct AacEncoder(super::audio::AacEncoder);
    pub struct AacDecoder(super::audio::AacDecoder);

    impl AacEncoder {
        pub fn new(bitrate_kbps: u32) -> Result<Self, String> {
            super::audio::AacEncoder::new(bitrate_kbps).map(Self).map_err(|e| e.to_string())
        }
        pub fn encode(&mut self, pcm: &[i16], f: impl FnMut(&[u8])) -> Result<(), String> {
            self.0.encode(pcm, f).map_err(|e| e.to_string())
        }
        pub fn finish(&mut self, f: impl FnMut(&[u8])) -> Result<(), String> {
            self.0.finish(f).map_err(|e| e.to_string())
        }
    }

    impl AacDecoder {
        pub fn new(asc: &[u8]) -> Result<Self, String> {
            super::audio::AacDecoder::new(asc).map(Self).map_err(|e| e.to_string())
        }
        pub fn decode(&mut self, frame: &[u8], out: &mut Vec<i16>) -> Result<(), String> {
            self.0.decode(frame, out).map_err(|e| e.to_string())
        }
    }
}

pub fn list_cameras() -> Vec<(String, String)> {
    webcam::list_cameras()
}

pub fn list_microphones() -> Vec<(String, String)> {
    audio::list_microphones()
}

/// Codecs and whether a hardware encoder exists for each.
pub fn available_codecs() -> Vec<(Codec, bool)> {
    Codec::ALL.iter().map(|&c| (c, encoder::find_encoders(c, None).map(|l| !l.is_empty()).unwrap_or(false))).collect()
}

/// Process setup for the library window (winit needs an STA COM thread).
pub fn init_ui() {
    cli::init_media(false);
}

/// Process setup for command-line tools that use codecs.
pub fn init_tool() {
    cli::init_media(true);
}
