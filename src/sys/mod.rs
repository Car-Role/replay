//! Platform backends. Each one offers the same API to the shared code:
//! `aac`, `player`, `shell`, `secret`, `net`, `cli`, plus `list_microphones`,
//! `available_codecs`, `init_ui` and `PLATFORM`.

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(unix)]
pub mod unix;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub use self::macos::*;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub use self::linux::*;
