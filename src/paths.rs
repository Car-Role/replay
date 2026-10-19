//! Well-known folders.

use std::path::PathBuf;

use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    FOLDERID_LocalAppData, FOLDERID_RoamingAppData, FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};
use windows::core::GUID;

fn known_folder(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s.map(PathBuf::from)
    }
}

pub fn videos_dir() -> PathBuf {
    known_folder(&FOLDERID_Videos)
        .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Videos")))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `%APPDATA%\Replay` for settings.
pub fn config_dir() -> PathBuf {
    known_folder(&FOLDERID_RoamingAppData)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Replay")
}

/// `%LOCALAPPDATA%\Replay` for logs and caches.
pub fn data_dir() -> PathBuf {
    known_folder(&FOLDERID_LocalAppData)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Replay")
}
