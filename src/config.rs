//! User settings, stored as TOML in `%APPDATA%\Replay\config.toml`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

impl Codec {
    pub const ALL: [Codec; 3] = [Codec::Av1, Codec::Hevc, Codec::H264];

    pub fn label(self) -> &'static str {
        match self {
            Codec::H264 => "H.264",
            Codec::Hevc => "HEVC (H.265)",
            Codec::Av1 => "AV1",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Resolution {
    Native,
    #[serde(rename = "720p")]
    P720,
    #[serde(rename = "1080p")]
    P1080,
    #[serde(rename = "1440p")]
    P1440,
    #[serde(rename = "2160p")]
    P2160,
}

impl Resolution {
    pub const ALL: [Resolution; 5] =
        [Resolution::Native, Resolution::P720, Resolution::P1080, Resolution::P1440, Resolution::P2160];

    pub fn label(self) -> &'static str {
        match self {
            Resolution::Native => "Same as monitor",
            Resolution::P720 => "720p",
            Resolution::P1080 => "1080p",
            Resolution::P1440 => "1440p",
            Resolution::P2160 => "2160p (4K)",
        }
    }

    /// Output size for a source of `w`x`h`. Never upscales; keeps aspect;
    /// returns even dimensions as encoders require.
    pub fn output_size(self, w: u32, h: u32) -> (u32, u32) {
        let target_h = match self {
            Resolution::Native => h,
            Resolution::P720 => 720,
            Resolution::P1080 => 1080,
            Resolution::P1440 => 1440,
            Resolution::P2160 => 2160,
        };
        let (ow, oh) = if target_h >= h || h == 0 {
            (w, h)
        } else {
            ((u64::from(w) * u64::from(target_h) / u64::from(h)) as u32, target_h)
        };
        ((ow.max(2)) & !1, (oh.max(2)) & !1)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RateControl {
    /// Peak-constrained VBR: average target with a 1.5x ceiling.
    Vbr,
    Cbr,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct VideoConfig {
    pub codec: Codec,
    pub fps: u32,
    pub resolution: Resolution,
    pub bitrate_kbps: u32,
    pub rate_control: RateControl,
    pub capture_cursor: bool,
    /// On HDR monitors, convert to normal colors (otherwise clips look washed out).
    pub hdr_to_sdr: bool,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            codec: Codec::Av1,
            fps: 60,
            resolution: Resolution::P1080,
            bitrate_kbps: 25_000,
            rate_control: RateControl::Vbr,
            capture_cursor: false,
            hdr_to_sdr: true,
        }
    }
}

impl VideoConfig {
    pub fn peak_kbps(&self) -> u32 {
        match self.rate_control {
            RateControl::Cbr => self.bitrate_kbps,
            RateControl::Vbr => self.bitrate_kbps.saturating_mul(3) / 2,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct AudioConfig {
    pub game_audio: bool,
    pub microphone: bool,
    /// Endpoint id of the microphone; empty means the Windows default.
    pub microphone_device: String,
    /// Microphone gain in the mixed track, in percent.
    pub microphone_volume: u32,
    /// Game gain in the mixed track, in percent.
    pub game_volume: u32,
    pub bitrate_kbps: u32,
    /// Adds a first track with game and microphone mixed together.
    pub mixed_track: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            game_audio: true,
            microphone: true,
            microphone_device: String::new(),
            microphone_volume: 100,
            game_volume: 100,
            bitrate_kbps: 192,
            mixed_track: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct HotkeyConfig {
    pub save_focused: String,
    pub save_all: String,
    pub toggle_game: String,
    /// Starts or stops a full recording of the game in front.
    pub record: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            save_focused: "Alt+F10".into(),
            save_all: "Alt+Shift+F10".into(),
            toggle_game: "Ctrl+Alt+F10".into(),
            record: "Alt+F9".into(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct DetectionConfig {
    pub enabled: bool,
    /// Record any non-excluded app that covers a whole monitor and uses a 3D API.
    pub fullscreen_apps: bool,
    /// Executable names that are always recorded (for example `game.exe`).
    pub include: Vec<String>,
    /// Executable names that are never recorded.
    pub exclude: Vec<String>,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        Self { enabled: true, fullscreen_apps: true, include: Vec::new(), exclude: Vec::new() }
    }
}

/// Where clips are uploaded.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UploadTarget {
    Streamable,
    Catbox,
    Litterbox,
    Custom,
}

impl UploadTarget {
    pub const ALL: [UploadTarget; 4] = [UploadTarget::Streamable, UploadTarget::Catbox, UploadTarget::Litterbox, UploadTarget::Custom];

    pub fn label(self) -> &'static str {
        match self {
            UploadTarget::Streamable => "Streamable",
            UploadTarget::Catbox => "Catbox",
            UploadTarget::Litterbox => "Litterbox",
            UploadTarget::Custom => "Custom uploader",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            UploadTarget::Streamable => "Your Streamable account. Links show a video player",
            UploadTarget::Catbox => "No account. Files up to 200 MB stay online; anyone with the link can see them",
            UploadTarget::Litterbox => "No account. Files up to 1 GB are deleted after the time you choose",
            UploadTarget::Custom => "Any service with an upload form, set up like a ShareX custom uploader",
        }
    }
}

/// A ShareX-style custom uploader: a multipart POST and where to find the link.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct CustomUploader {
    pub name: String,
    pub url: String,
    /// The form field that carries the file.
    pub file_field: String,
    /// Extra form fields.
    pub fields: BTreeMap<String, String>,
    /// Request headers ("Name: value" lines), encrypted like the password.
    pub headers_protected: String,
    /// Where the link is in a JSON reply ("data.link"); empty: the whole reply.
    pub link: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ShareConfig {
    pub target: UploadTarget,
    pub auto_upload: bool,
    pub copy_link_after_upload: bool,
    pub streamable_email: String,
    /// Stored encrypted with Windows DPAPI for the current user, base64 encoded.
    pub streamable_password_protected: String,
    /// How long Litterbox keeps files: 1, 12, 24 or 72 hours.
    pub litterbox_hours: u32,
    pub custom: CustomUploader,
}

impl Default for ShareConfig {
    fn default() -> Self {
        Self {
            target: UploadTarget::Streamable,
            auto_upload: false,
            copy_link_after_upload: true,
            streamable_email: String::new(),
            streamable_password_protected: String::new(),
            litterbox_hours: 72,
            custom: CustomUploader::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct UpdateConfig {
    /// Check GitHub for a new release once a day and download it in the background.
    pub auto_check: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self { auto_check: true }
    }
}

/// Settings for one game that replace the general ones. Empty fields use
/// the general setting.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct GameSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate_kbps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_seconds: Option<u32>,
}

impl GameSettings {
    pub fn is_empty(&self) -> bool {
        *self == GameSettings::default()
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayCorner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl OverlayCorner {
    pub const ALL: [OverlayCorner; 4] = [OverlayCorner::TopLeft, OverlayCorner::TopRight, OverlayCorner::BottomLeft, OverlayCorner::BottomRight];

    pub fn label(self) -> &'static str {
        match self {
            OverlayCorner::TopLeft => "Top left",
            OverlayCorner::TopRight => "Top right",
            OverlayCorner::BottomLeft => "Bottom left",
            OverlayCorner::BottomRight => "Bottom right",
        }
    }
}

/// The notice shown over the game when a replay is saved.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct OverlayConfig {
    pub enabled: bool,
    pub corner: OverlayCorner,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self { enabled: true, corner: OverlayCorner::TopRight }
    }
}

/// Picture-in-picture webcam in recordings.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct WebcamConfig {
    pub enabled: bool,
    /// Camera id; empty means the first camera.
    pub device: String,
    pub corner: OverlayCorner,
    /// Height of the webcam picture, in percent of the video height.
    pub size: u32,
}

impl Default for WebcamConfig {
    fn default() -> Self {
        Self { enabled: false, device: String::new(), corner: OverlayCorner::BottomRight, size: 25 }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct StorageConfig {
    /// Largest size of the clips folder in GB; 0 means no limit.
    pub max_gb: u32,
    /// Clips older than this many days are deleted; 0 keeps them.
    pub max_age_days: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    pub output_dir: PathBuf,
    pub replay_seconds: u32,
    pub notify: bool,
    pub sound: bool,
    pub start_with_windows: bool,
    pub video: VideoConfig,
    pub audio: AudioConfig,
    pub hotkeys: HotkeyConfig,
    pub detection: DetectionConfig,
    pub share: ShareConfig,
    pub updates: UpdateConfig,
    pub storage: StorageConfig,
    pub overlay: OverlayConfig,
    pub webcam: WebcamConfig,
    /// Per-game settings, keyed by the lower-case executable name.
    pub games: BTreeMap<String, GameSettings>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            output_dir: crate::paths::videos_dir().join("Replay"),
            replay_seconds: 120,
            notify: true,
            sound: true,
            start_with_windows: false,
            video: VideoConfig::default(),
            audio: AudioConfig::default(),
            hotkeys: HotkeyConfig::default(),
            detection: DetectionConfig::default(),
            share: ShareConfig::default(),
            updates: UpdateConfig::default(),
            storage: StorageConfig::default(),
            overlay: OverlayConfig::default(),
            webcam: WebcamConfig::default(),
            games: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        crate::paths::config_dir().join("config.toml")
    }

    /// Loads the config, or returns defaults when the file is missing or broken.
    /// A broken file is kept as `config.toml.bad` so the user does not lose it.
    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(c) => c.sanitized(),
                Err(e) => {
                    log::error!("config parse error, using defaults: {e}");
                    let _ = std::fs::rename(&path, path.with_extension("toml.bad"));
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, path)
    }

    /// The settings a game records with: the general ones, with that game's
    /// own settings on top.
    pub fn for_game(&self, exe: &str) -> Config {
        let mut c = self.clone();
        if let Some(g) = self.games.get(exe) {
            if let Some(v) = g.fps {
                c.video.fps = v;
            }
            if let Some(v) = g.resolution {
                c.video.resolution = v;
            }
            if let Some(v) = g.bitrate_kbps {
                c.video.bitrate_kbps = v;
            }
            if let Some(v) = g.replay_seconds {
                c.replay_seconds = v;
            }
        }
        c.sanitized()
    }

    /// The longest replay any game keeps (the microphone buffer covers it).
    pub fn max_replay_seconds(&self) -> u32 {
        self.games.values().filter_map(|g| g.replay_seconds).chain([self.replay_seconds]).max().unwrap_or(self.replay_seconds)
    }

    /// Clamps values to ranges the pipeline supports.
    pub fn sanitized(mut self) -> Self {
        self.replay_seconds = self.replay_seconds.clamp(5, 30 * 60);
        self.video.fps = self.video.fps.clamp(10, 240);
        self.video.bitrate_kbps = self.video.bitrate_kbps.clamp(500, 200_000);
        self.audio.bitrate_kbps = match self.audio.bitrate_kbps {
            0..=111 => 96,
            112..=143 => 128,
            144..=175 => 160,
            _ => 192,
        };
        self.audio.microphone_volume = self.audio.microphone_volume.min(400);
        self.webcam.size = self.webcam.size.clamp(10, 50);
        self.audio.game_volume = self.audio.game_volume.min(400);
        let mut games = BTreeMap::new();
        for (exe, mut g) in std::mem::take(&mut self.games) {
            g.fps = g.fps.map(|v| v.clamp(10, 240));
            g.bitrate_kbps = g.bitrate_kbps.map(|v| v.clamp(500, 200_000));
            g.replay_seconds = g.replay_seconds.map(|v| v.clamp(5, 30 * 60));
            let exe = exe.trim().to_ascii_lowercase();
            if !g.is_empty() && !exe.is_empty() {
                games.insert(exe, g);
            }
        }
        self.games = games;
        for list in [&mut self.detection.include, &mut self.detection.exclude] {
            for s in list.iter_mut() {
                *s = s.trim().to_ascii_lowercase();
            }
            list.retain(|s| !s.is_empty());
            list.sort();
            list.dedup();
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let c = Config::default();
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let c: Config = toml::from_str("replay_seconds = 30\n[video]\ncodec = \"hevc\"\n").unwrap();
        assert_eq!(c.replay_seconds, 30);
        assert_eq!(c.video.codec, Codec::Hevc);
        assert_eq!(c.video.fps, 60);
    }

    #[test]
    fn game_settings_override_and_round_trip() {
        let mut c = Config::default();
        c.games.insert("game.exe".into(), GameSettings { fps: Some(144), replay_seconds: Some(300), ..Default::default() });
        c.games.insert("empty.exe".into(), GameSettings::default());
        let c = c.sanitized();
        assert!(!c.games.contains_key("empty.exe"));
        let g = c.for_game("game.exe");
        assert_eq!((g.video.fps, g.replay_seconds, g.video.bitrate_kbps), (144, 300, 25_000));
        assert_eq!(c.for_game("other.exe").video.fps, 60);
        assert_eq!(c.max_replay_seconds(), 300);
        let back: Config = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn output_size_never_upscales_and_is_even() {
        assert_eq!(Resolution::P1080.output_size(3840, 2160), (1920, 1080));
        assert_eq!(Resolution::P1080.output_size(1280, 720), (1280, 720));
        assert_eq!(Resolution::P1080.output_size(3440, 1440), (2580, 1080));
        assert_eq!(Resolution::Native.output_size(1367, 769), (1366, 768));
    }
}
