//! Finds running games by reading /proc: processes started by Steam
//! (SteamAppId), Lutris, Heroic, Bottles, the include list, or known games.
//! Each process is classified once; later scans only list /proc.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::config::DetectionConfig;

/// Steam and launcher helpers that carry game variables but are not games.
const HELPERS: &[&str] = &[
    "steam", "steamwebhelper", "reaper", "pressure-vessel", "pv-bwrap", "srt-bwrap", "steam-runtime-l", "wineserver",
    "services.exe", "winedevice.exe", "plugplay.exe", "explorer.exe", "rpcss.exe", "svchost.exe", "tabtip.exe",
    "conhost.exe", "start.exe", "wine64-preloader", "wine-preloader", "proton", "python3", "sh", "bash", "lutris",
    "heroic", "bottles", "gamemoded", "mangohud", "gamescope", "steamerrorreporter", "crashpad_handler",
];

/// Games that run without a launcher; matched against the command line.
const KNOWN: &[&str] = &["runelite", "osclient", "minecraft", "prismlauncher", "factorio", "terraria", "osu!", "veloren"];

#[derive(Clone)]
struct Verdict {
    start: u64,
    /// (executable, game name)
    game: Option<(String, String)>,
}

pub struct Detector {
    cache: HashMap<u32, Verdict>,
    uid: u32,
}

fn read(p: impl AsRef<Path>) -> Option<Vec<u8>> {
    fs::read(p).ok()
}

/// Process start time (field 22 of /proc/<pid>/stat), to tell reused pids apart.
fn start_time(pid: u32) -> Option<u64> {
    let stat = String::from_utf8(read(format!("/proc/{pid}/stat"))?).ok()?;
    let after = &stat[stat.rfind(')')? + 2..];
    after.split_whitespace().nth(19)?.parse().ok()
}

fn steam_game_name(app_id: &str) -> Option<String> {
    let home = dirs::home_dir()?;
    let roots = [".steam/steam/steamapps", ".local/share/Steam/steamapps", ".var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps"];
    let mut libraries: Vec<std::path::PathBuf> = roots.iter().map(|r| home.join(r)).collect();
    // Extra library folders.
    for root in roots {
        if let Ok(text) = fs::read_to_string(home.join(root).join("libraryfolders.vdf")) {
            for line in text.lines() {
                let l = line.trim();
                if let Some(rest) = l.strip_prefix("\"path\"") {
                    let p = rest.trim().trim_matches('"');
                    libraries.push(Path::new(p).join("steamapps"));
                }
            }
        }
    }
    for lib in libraries {
        if let Ok(text) = fs::read_to_string(lib.join(format!("appmanifest_{app_id}.acf"))) {
            for line in text.lines() {
                if let Some(rest) = line.trim().strip_prefix("\"name\"") {
                    let name = rest.trim().trim_matches('"').to_string();
                    if !name.is_empty() {
                        return Some(name);
                    }
                }
            }
        }
    }
    None
}

fn pretty(exe: &str) -> String {
    let stem = exe.trim_end_matches(".exe").trim_end_matches(".x86_64");
    let mut c = stem.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

impl Detector {
    pub fn new() -> Self {
        Self { cache: HashMap::new(), uid: unsafe { libc::getuid() } }
    }

    fn classify(&self, pid: u32, cfg: &DetectionConfig) -> Option<(String, String)> {
        let name = self.classify_name(pid, cfg)?;
        let exe = fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()))
            .unwrap_or_default();
        Some((exe, name))
    }

    fn classify_name(&self, pid: u32, cfg: &DetectionConfig) -> Option<String> {
        let comm = String::from_utf8_lossy(&read(format!("/proc/{pid}/comm"))?).trim().to_ascii_lowercase();
        let exe = fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()))
            .unwrap_or_else(|| comm.clone());
        let cmdline = String::from_utf8_lossy(&read(format!("/proc/{pid}/cmdline")).unwrap_or_default()).replace('\0', " ");
        // Proton games show up as wine processes named after the Windows .exe.
        let win_exe = cmdline
            .split(['\\', '/', ' '])
            .find(|s| s.to_ascii_lowercase().ends_with(".exe"))
            .map(|s| s.to_ascii_lowercase());
        let names: Vec<&str> = [Some(exe.as_str()), Some(comm.as_str()), win_exe.as_deref()].into_iter().flatten().collect();

        if names.iter().any(|n| cfg.exclude.iter().any(|e| e == n)) {
            return None;
        }
        if let Some(n) = names.iter().find(|n| cfg.include.iter().any(|e| e == **n)) {
            return Some(pretty(n));
        }
        if !cfg.enabled || names.iter().any(|n| HELPERS.iter().any(|h| n.starts_with(h))) {
            return None;
        }
        let env = read(format!("/proc/{pid}/environ")).unwrap_or_default();
        let var = |key: &str| {
            env.split(|&b| b == 0).find_map(|kv| {
                let kv = String::from_utf8_lossy(kv);
                kv.strip_prefix(&format!("{key}=")).map(String::from)
            })
        };
        if let Some(id) = var("SteamAppId").or_else(|| var("SteamGameId")).filter(|v| v != "0" && !v.is_empty()) {
            return Some(steam_game_name(&id).unwrap_or_else(|| pretty(win_exe.as_deref().unwrap_or(&exe))));
        }
        if let Some(name) = var("LUTRIS_GAME_NAME").or_else(|| var("HEROIC_APP_NAME")).or_else(|| var("BOTTLES_NAME")) {
            return Some(name);
        }
        if var("LUTRIS_GAME_UUID").is_some() {
            return Some(pretty(win_exe.as_deref().unwrap_or(&exe)));
        }
        let lower = cmdline.to_ascii_lowercase();
        KNOWN.iter().find(|k| lower.contains(*k)).map(|k| pretty(k))
    }

    /// Running games as (pid, executable, name), oldest first.
    pub fn scan(&mut self, cfg: &DetectionConfig) -> Vec<(u32, String, String)> {
        use std::os::unix::fs::MetadataExt;
        let mut seen = Vec::new();
        let mut found = Vec::new();
        let Ok(rd) = fs::read_dir("/proc") else { return found };
        for e in rd.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            if e.metadata().map(|m| m.uid()).ok() != Some(self.uid) {
                continue;
            }
            seen.push(pid);
            let start = start_time(pid).unwrap_or(0);
            let verdict = match self.cache.get(&pid) {
                Some(v) if v.start == start => v.clone(),
                _ => {
                    let v = Verdict { start, game: self.classify(pid, cfg) };
                    self.cache.insert(pid, v.clone());
                    v
                }
            };
            if let Some((exe, name)) = verdict.game {
                found.push((start, pid, exe, name));
            }
        }
        self.cache.retain(|pid, _| seen.contains(pid));
        found.sort();
        found.into_iter().map(|(_, pid, exe, name)| (pid, exe, name)).collect()
    }

    pub fn forget(&mut self) {
        self.cache.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_names() {
        assert_eq!(pretty("eldenring.exe"), "Eldenring");
        assert_eq!(pretty("runelite"), "Runelite");
    }
}
