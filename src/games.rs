//! The list of games Replay has found, shared between the recorder (which
//! writes it) and the library window (which shows it and lets the user turn
//! recording on or off). Stored as `games.json` in the data folder.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::{Config, DetectionConfig};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GameRecord {
    /// Lower-case executable name; the key used by the include/exclude lists.
    pub exe: String,
    pub name: String,
    #[serde(default)]
    pub path: String,
    /// How it was found, in words ("Steam library", "Added by you", ...).
    pub how: String,
    pub first_seen: u64,
    pub last_seen: u64,
    /// True while the recorder has an active recording for it.
    #[serde(default)]
    pub recording: bool,
    /// True while a full session recording of it runs.
    #[serde(default)]
    pub capturing: bool,
    /// When that session recording started (Unix seconds).
    #[serde(default)]
    pub capture_started: u64,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct GameDb {
    pub games: Vec<GameRecord>,
}

fn path() -> PathBuf {
    crate::paths::data_dir().join("games.json")
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl GameDb {
    pub fn load() -> Self {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_vec_pretty(self) {
            let tmp = p.with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(tmp, p);
            }
        }
    }

    pub fn find(&self, exe: &str) -> Option<&GameRecord> {
        self.games.iter().find(|g| g.exe == exe)
    }

    /// Adds or refreshes a game.
    pub fn seen(&mut self, exe: &str, name: &str, path: &str, how: &str) {
        let now = now_secs();
        match self.games.iter_mut().find(|g| g.exe == exe) {
            Some(g) => {
                g.last_seen = now;
                if !name.is_empty() {
                    g.name = name.to_string();
                }
                if !path.is_empty() {
                    g.path = path.to_string();
                }
                if g.how != "Added by you" {
                    g.how = how.to_string();
                }
            }
            None => self.games.push(GameRecord {
                exe: exe.to_string(),
                name: name.to_string(),
                path: path.to_string(),
                how: how.to_string(),
                first_seen: now,
                last_seen: now,
                recording: false,
                capturing: false,
                capture_started: 0,
            }),
        }
    }

    /// Marks exactly these executables as recording now.
    pub fn set_recording(&mut self, exes: &[String]) -> bool {
        let mut changed = false;
        for g in &mut self.games {
            let on = exes.contains(&g.exe);
            if g.recording != on {
                g.recording = on;
                changed = true;
            }
        }
        changed
    }

    /// Marks exactly these executables as having a session recording.
    pub fn set_capturing(&mut self, exes: &[String]) -> bool {
        let mut changed = false;
        for g in &mut self.games {
            let on = exes.contains(&g.exe);
            if g.capturing != on {
                g.capturing = on;
                g.capture_started = if on { now_secs() } else { 0 };
                changed = true;
            }
        }
        changed
    }

    pub fn forget(&mut self, exe: &str) {
        self.games.retain(|g| g.exe != exe);
    }
}

/// Whether recording is allowed for `exe` by the user's lists.
pub fn is_enabled(d: &DetectionConfig, exe: &str) -> bool {
    !d.exclude.iter().any(|e| e == exe)
}

/// Turns recording of a game on or off in the config.
pub fn set_enabled(cfg: &mut Config, exe: &str, on: bool, added_by_user: bool) {
    let d = &mut cfg.detection;
    d.exclude.retain(|e| e != exe);
    d.include.retain(|e| e != exe);
    if on {
        if added_by_user {
            d.include.push(exe.to_string());
        }
    } else {
        d.exclude.push(exe.to_string());
    }
}

/// Recorder side: notes a detected game and returns nothing; cheap enough
/// to call on every new recording.
pub fn note_detected(exe: &str, name: &str, path: &str, how: &str) {
    let mut db = GameDb::load();
    db.seen(exe, name, path, how);
    db.save();
}

/// Recorder side: publishes which games are buffering right now, and which
/// have a session recording.
pub fn note_recording(exes: &[String], capturing: &[String]) {
    let mut db = GameDb::load();
    let a = db.set_recording(exes);
    let b = db.set_capturing(capturing);
    if a || b {
        db.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggling_updates_lists() {
        let mut cfg = Config::default();
        set_enabled(&mut cfg, "game.exe", false, false);
        assert_eq!(cfg.detection.exclude, vec!["game.exe"]);
        assert!(!is_enabled(&cfg.detection, "game.exe"));
        set_enabled(&mut cfg, "game.exe", true, false);
        assert!(cfg.detection.exclude.is_empty() && cfg.detection.include.is_empty());
        set_enabled(&mut cfg, "mine.exe", true, true);
        assert_eq!(cfg.detection.include, vec!["mine.exe"]);
    }

    #[test]
    fn seen_and_recording_flags() {
        let mut db = GameDb::default();
        db.seen("a.exe", "A", "C:/a.exe", "Steam library");
        db.seen("a.exe", "A2", "", "Fullscreen");
        assert_eq!(db.games.len(), 1);
        assert_eq!(db.games[0].name, "A2");
        assert_eq!(db.games[0].path, "C:/a.exe");
        assert!(db.set_recording(&["a.exe".into()]));
        assert!(!db.set_recording(&["a.exe".into()]));
        assert!(db.games[0].recording);
    }
}
