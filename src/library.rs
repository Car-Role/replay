//! Clip details that the video file does not hold: favorites and tags.
//! Stored as `library.json` in the data folder, keyed by the clip's full
//! path. Also renames clips and keeps the clips folder under the user's
//! storage limits.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::config::Config;

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct ClipMeta {
    #[serde(default)]
    pub favorite: bool,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl ClipMeta {
    fn is_empty(&self) -> bool {
        !self.favorite && self.tags.is_empty()
    }
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Library {
    #[serde(default)]
    pub clips: BTreeMap<String, ClipMeta>,
}

fn path() -> PathBuf {
    crate::paths::data_dir().join("library.json")
}

/// Links of uploaded clips, keyed by the clip's full path.
pub fn uploads_path() -> PathBuf {
    crate::paths::data_dir().join("uploads.json")
}

fn key(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn write_json(p: &Path, value: &impl Serialize) -> std::io::Result<()> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, p)
}

impl Library {
    pub fn load() -> Self {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        write_json(&path(), self)
    }

    pub fn get(&self, p: &Path) -> ClipMeta {
        self.clips.get(&key(p)).cloned().unwrap_or_default()
    }

    pub fn is_favorite(&self, p: &Path) -> bool {
        self.clips.get(&key(p)).is_some_and(|m| m.favorite)
    }

    /// Changes one clip's details. Empty entries are removed.
    pub fn update(&mut self, p: &Path, f: impl FnOnce(&mut ClipMeta)) {
        let k = key(p);
        let mut m = self.clips.remove(&k).unwrap_or_default();
        f(&mut m);
        if !m.is_empty() {
            self.clips.insert(k, m);
        }
    }

    pub fn forget(&mut self, p: &Path) {
        self.clips.remove(&key(p));
    }

    pub fn moved(&mut self, from: &Path, to: &Path) {
        if let Some(m) = self.clips.remove(&key(from)) {
            self.clips.insert(key(to), m);
        }
    }

    /// Every tag in use, sorted.
    pub fn all_tags(&self) -> Vec<String> {
        let mut v: Vec<String> = self.clips.values().flat_map(|m| m.tags.iter().cloned()).collect();
        v.sort_by_key(|t| t.to_lowercase());
        v.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        v
    }
}

/// Turns "clutch, Funny  clutch" into ["clutch", "Funny"].
pub fn parse_tags(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in text.split([',', ' ', '#']).map(str::trim).filter(|t| !t.is_empty()) {
        let t: String = t.chars().take(32).collect();
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&t)) {
            out.push(t);
        }
    }
    out
}

/// Renames a clip file (same folder) and moves its details and upload link.
/// Returns the new path.
pub fn rename_clip(old: &Path, new_stem: &str) -> Result<PathBuf, String> {
    let stem = crate::util::sanitize_file_name(new_stem.trim());
    if stem.is_empty() {
        return Err("type a name".into());
    }
    let ext = old.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "mp4".into());
    let new = old.with_file_name(format!("{stem}.{ext}"));
    if new == old {
        return Ok(new);
    }
    // Windows names ignore case: allow a rename that only changes case.
    let same_file = new.to_string_lossy().eq_ignore_ascii_case(&old.to_string_lossy());
    if new.exists() && !same_file {
        return Err(format!("a clip named \"{stem}\" already exists"));
    }
    std::fs::rename(old, &new).map_err(|e| format!("cannot rename: {e}"))?;
    let mut lib = Library::load();
    lib.moved(old, &new);
    let _ = lib.save();
    let up = uploads_path();
    if let Some(mut map) = std::fs::read(&up).ok().and_then(|b| serde_json::from_slice::<BTreeMap<String, String>>(&b).ok())
        && let Some(url) = map.remove(&key(old))
    {
        map.insert(key(&new), url);
        let _ = write_json(&up, &map);
    }
    Ok(new)
}

/// A clip found on disk, for the storage limits.
#[derive(Clone, Debug)]
pub struct DiskClip {
    pub path: PathBuf,
    pub size: u64,
    pub modified: SystemTime,
}

/// Every .mp4 in the clips folder and its game folders.
pub fn clips_on_disk(dir: &Path) -> Vec<DiskClip> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                if depth < 2 {
                    stack.push((p, depth + 1));
                }
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("mp4")) {
                out.push(DiskClip { path: p, size: md.len(), modified: md.modified().unwrap_or(SystemTime::UNIX_EPOCH) });
            }
        }
    }
    out
}

/// Picks the clips to delete so the folder fits the limits: first clips
/// older than the age limit, then the oldest clips until the folder is under
/// the size limit. Favorites are never picked.
pub fn over_limits(clips: &[DiskClip], is_favorite: impl Fn(&Path) -> bool, max_bytes: u64, max_age: Option<Duration>, now: SystemTime) -> Vec<PathBuf> {
    let mut candidates: Vec<&DiskClip> = clips.iter().filter(|c| !is_favorite(&c.path)).collect();
    candidates.sort_by_key(|c| c.modified);
    let mut total: u64 = clips.iter().map(|c| c.size).sum();
    let mut out = Vec::new();
    for c in candidates {
        let too_old = max_age.is_some_and(|a| now.duration_since(c.modified).is_ok_and(|age| age > a));
        let too_big = max_bytes > 0 && total > max_bytes;
        if !too_old && !too_big {
            // Sorted oldest first, so no later clip is older either.
            break;
        }
        total = total.saturating_sub(c.size);
        out.push(c.path.clone());
    }
    out
}

/// Deletes clips over the user's storage limits. Returns (count, bytes).
pub fn enforce_storage(cfg: &Config) -> (usize, u64) {
    let s = &cfg.storage;
    if s.max_gb == 0 && s.max_age_days == 0 {
        return (0, 0);
    }
    let clips = clips_on_disk(&cfg.output_dir);
    let lib = Library::load();
    let max_bytes = u64::from(s.max_gb) * 1_000_000_000;
    let max_age = (s.max_age_days > 0).then(|| Duration::from_secs(u64::from(s.max_age_days) * 86_400));
    let pick = over_limits(&clips, |p| lib.is_favorite(p), max_bytes, max_age, SystemTime::now());
    let mut count = 0;
    let mut bytes = 0;
    for p in pick {
        let size = clips.iter().find(|c| c.path == p).map(|c| c.size).unwrap_or(0);
        match std::fs::remove_file(&p) {
            Ok(()) => {
                log::info!("storage limit: deleted {}", p.display());
                count += 1;
                bytes += size;
            }
            Err(e) => log::warn!("storage limit: cannot delete {}: {e}", p.display()),
        }
    }
    (count, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(name: &str, size: u64, days_old: u64, now: SystemTime) -> DiskClip {
        DiskClip { path: PathBuf::from(name), size, modified: now - Duration::from_secs(days_old * 86_400) }
    }

    #[test]
    fn tags_are_split_and_deduplicated() {
        assert_eq!(parse_tags("clutch, Funny  #clutch ,"), vec!["clutch", "Funny"]);
        assert!(parse_tags("  ").is_empty());
    }

    #[test]
    fn limits_delete_oldest_and_keep_favorites() {
        let now = SystemTime::now();
        let clips = vec![clip("a", 50, 10, now), clip("b", 50, 9, now), clip("c", 50, 1, now), clip("fav", 50, 20, now)];
        let fav = |p: &Path| p == Path::new("fav");
        // 200 bytes in total; a 120 byte limit deletes the two oldest that are not favorites.
        assert_eq!(over_limits(&clips, fav, 120, None, now), vec![PathBuf::from("a"), PathBuf::from("b")]);
        // Age limit only.
        assert_eq!(over_limits(&clips, fav, 0, Some(Duration::from_secs(5 * 86_400)), now), vec![PathBuf::from("a"), PathBuf::from("b")]);
        // Under the limit: nothing.
        assert!(over_limits(&clips, fav, 1000, None, now).is_empty());
    }

    #[test]
    fn meta_round_trip_and_cleanup() {
        let mut lib = Library::default();
        let p = Path::new("C:/clips/a.mp4");
        lib.update(p, |m| m.favorite = true);
        assert!(lib.is_favorite(p));
        lib.moved(p, Path::new("C:/clips/b.mp4"));
        assert!(lib.is_favorite(Path::new("C:/clips/b.mp4")));
        lib.update(Path::new("C:/clips/b.mp4"), |m| m.favorite = false);
        assert!(lib.clips.is_empty());
    }
}
