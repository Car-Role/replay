//! Code shared by the macOS and Linux backends: the tray recorder, IPC,
//! HTTP, keychain secrets, audio timeline, preview audio, desktop helpers.

pub mod aac_decode;
pub mod app;
pub mod audio_out;
pub mod hotkeys;
pub mod ipc;
pub mod net;
pub mod secret;
pub mod shell;
pub mod timeline;
