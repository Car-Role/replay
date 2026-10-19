//! Well-known folders.
//!
//! - Windows: `Videos`, `%APPDATA%\Replay`, `%LOCALAPPDATA%\Replay`
//! - macOS: `~/Movies`, `~/Library/Application Support/Replay`
//! - Linux: `$XDG_VIDEOS_DIR`, `$XDG_CONFIG_HOME/replay`, `$XDG_DATA_HOME/replay`

use std::path::PathBuf;

#[cfg(windows)]
mod imp {
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

    pub fn videos() -> Option<PathBuf> {
        known_folder(&FOLDERID_Videos)
    }
    pub fn config() -> Option<PathBuf> {
        known_folder(&FOLDERID_RoamingAppData).map(|p| p.join("Replay"))
    }
    pub fn data() -> Option<PathBuf> {
        known_folder(&FOLDERID_LocalAppData).map(|p| p.join("Replay"))
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::path::PathBuf;

    pub fn videos() -> Option<PathBuf> {
        dirs::video_dir()
    }
    pub fn config() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("Replay"))
    }
    pub fn data() -> Option<PathBuf> {
        dirs::data_local_dir().map(|p| p.join("Replay"))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::PathBuf;

    pub fn videos() -> Option<PathBuf> {
        dirs::video_dir().or_else(|| dirs::home_dir().map(|h| h.join("Videos")))
    }
    pub fn config() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("replay"))
    }
    pub fn data() -> Option<PathBuf> {
        dirs::data_local_dir().map(|p| p.join("replay"))
    }
}

fn home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn videos_dir() -> PathBuf {
    imp::videos().unwrap_or_else(|| home().join(if cfg!(target_os = "macos") { "Movies" } else { "Videos" }))
}

/// Settings folder.
pub fn config_dir() -> PathBuf {
    imp::config().unwrap_or_else(|| home().join(".replay"))
}

/// Logs, caches and downloaded updates.
pub fn data_dir() -> PathBuf {
    imp::data().unwrap_or_else(|| home().join(".replay"))
}
