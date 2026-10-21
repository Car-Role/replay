//! Plain-text reports to paste into a bug report: one clip's tracks with
//! audio levels, clipping and dropouts, and the system (see `system`).
//! They hold no file paths with user names and no clip content.

use std::fs::File;
use std::path::Path;

use crate::media::{CHANNELS, SAMPLE_RATE};
use crate::mux::reader::{self, TrackKindIn};

/// Levels and faults of one decoded audio track.
#[derive(Debug, PartialEq)]
pub struct AudioStats {
    /// Highest sample, in dBFS (0 is full scale).
    pub peak_db: f64,
    /// Loudness over the parts with sound, in dBFS.
    pub rms_db: f64,
    /// Share of samples at full scale, in percent (distortion).
    pub clipped_pct: f64,
    /// Runs of exact digital silence inside sound: missing audio.
    pub gaps: usize,
    pub longest_gap_ms: f64,
}

const FULL: i32 = 32_767;
/// Shortest and longest digital silence counted as a dropout.
const GAP_MIN_MS: f64 = 5.0;
const GAP_MAX_MS: f64 = 2000.0;
/// Sound before and after a gap must reach this level (-50 dBFS).
const ACTIVE: i32 = 104;

fn db(v: f64) -> f64 {
    if v <= 0.0 { -120.0 } else { 20.0 * (v / f64::from(FULL)).log10() }
}

/// Measures interleaved stereo PCM at 48 kHz.
pub fn audio_stats(pcm: &[i16]) -> AudioStats {
    let ch = CHANNELS as usize;
    let frames = pcm.len() / ch;
    let mut peak = 0i32;
    let mut clipped = 0usize;
    let (mut sum, mut loud) = (0f64, 0usize);
    for &s in pcm {
        let a = i32::from(s).abs();
        peak = peak.max(a);
        if a >= FULL - 1 {
            clipped += 1;
        }
        if a >= ACTIVE {
            sum += f64::from(s) * f64::from(s);
            loud += 1;
        }
    }
    // Dropouts: frames where every channel is exactly zero, between sound.
    let silent = |f: usize| pcm[f * ch..f * ch + ch].iter().all(|&s| s == 0);
    let active_near = |from: usize, to: usize| (from..to.min(frames)).any(|f| pcm[f * ch..f * ch + ch].iter().any(|&s| i32::from(s).abs() >= ACTIVE));
    let frames_per_ms = f64::from(SAMPLE_RATE) / 1000.0;
    let window = (20.0 * frames_per_ms) as usize;
    let (mut gaps, mut longest) = (0usize, 0f64);
    let mut f = 0;
    while f < frames {
        if !silent(f) {
            f += 1;
            continue;
        }
        let start = f;
        while f < frames && silent(f) {
            f += 1;
        }
        let ms = (f - start) as f64 / frames_per_ms;
        let inside = start > 0 && f < frames;
        if inside && (GAP_MIN_MS..=GAP_MAX_MS).contains(&ms) && active_near(start.saturating_sub(window), start) && active_near(f, f + window) {
            gaps += 1;
            longest = longest.max(ms);
        }
    }
    AudioStats {
        peak_db: db(f64::from(peak)),
        rms_db: if loud == 0 { -120.0 } else { db((sum / loud as f64).sqrt()) },
        clipped_pct: if pcm.is_empty() { 0.0 } else { clipped as f64 * 100.0 / pcm.len() as f64 },
        gaps,
        longest_gap_ms: longest,
    }
}

/// A report on one clip: video settings, then each audio track's levels.
pub fn clip(path: &Path) -> Result<String, String> {
    let m = reader::read(path).map_err(|e| e.to_string())?;
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let size = std::fs::metadata(path).map(|md| md.len()).unwrap_or(0);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut out = format!("Clip: {name}\nReplay {}\nLength {:.2} s, {:.1} MB\n", crate::update::CURRENT, m.duration(), size as f64 / 1e6);
    if let Some(v) = m.video()
        && let TrackKindIn::Video { codec, width, height, .. } = &v.kind
    {
        out += &format!("Video: {} {width}x{height}, {} fps, {} kbit/s\n", codec.label(), crate::edit::video_fps(&m), crate::edit::video_kbps(&m));
    }
    for t in m.audio() {
        let TrackKindIn::Audio { sample_rate, channels, .. } = &t.kind else { continue };
        let label = if t.name.is_empty() { "(unnamed)" } else { t.name.as_str() };
        let secs = t.duration_seconds();
        let kbps = t.samples.iter().map(|s| u64::from(s.size)).sum::<u64>() as f64 * 8.0 / secs.max(0.001) / 1000.0;
        out += &format!("Audio \"{label}\": {sample_rate} Hz, {channels} ch, {kbps:.0} kbit/s, {secs:.2} s\n");
        match crate::edit::decode_range(&mut file, t, 0.0, secs) {
            Ok(pcm) => {
                let s = audio_stats(&pcm);
                out += &format!(
                    "  peak {:.1} dBFS, loudness {:.1} dBFS, clipped {:.3}%, dropouts {}{}\n",
                    s.peak_db,
                    s.rms_db,
                    s.clipped_pct,
                    s.gaps,
                    if s.gaps > 0 { format!(" (longest {:.0} ms)", s.longest_gap_ms) } else { String::new() }
                );
            }
            Err(e) => out += &format!("  cannot decode: {e}\n"),
        }
    }
    Ok(out)
}

/// Replaces the user's home folder with `%USERPROFILE%` in text for reports.
pub fn redact(text: &str) -> String {
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_default();
    if home.len() < 4 {
        return text.to_string();
    }
    text.replace(&home, "%USERPROFILE%").replace(&home.replace('\\', "/"), "%USERPROFILE%")
}

/// The last `n` warnings and errors of the recorder log, redacted.
pub fn recent_problems(n: usize) -> Vec<String> {
    let path = crate::paths::data_dir().join("replay.log");
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| l.contains(" WARN ") || l.contains(" ERROR ")).collect();
    lines[lines.len().saturating_sub(n)..].iter().map(|l| redact(l)).collect()
}

/// A report on this computer and Replay's settings.
pub fn system(cfg: &crate::config::Config) -> String {
    let mut out = format!("Replay {}\n", crate::update::CURRENT);
    out += &crate::sys::system_report(&cfg.audio.microphone_device);
    let v = &cfg.video;
    out += &format!(
        "Video: {} {}, {} fps, {} kbit/s, {:?}, replay {} s, HDR to SDR {}\n",
        v.codec.label(),
        v.resolution.label(),
        v.fps,
        v.bitrate_kbps,
        v.rate_control,
        cfg.replay_seconds,
        on(v.hdr_to_sdr)
    );
    let a = &cfg.audio;
    out += &format!(
        "Audio settings: game {}, mic {}, voice chat {}, mixed track {}, {} kbit/s; volumes game {}%, mic {}%, voice {}%\n",
        on(a.game_audio),
        on(a.microphone),
        on(a.voice_chat),
        on(a.mixed_track),
        a.bitrate_kbps,
        a.game_volume,
        a.microphone_volume,
        a.voice_chat_volume
    );
    out += &format!("Games with their own settings: {}, turned off: {}\n", cfg.games.len(), cfg.detection.exclude.len());
    let problems = recent_problems(15);
    if problems.is_empty() {
        out += "Recent recorder warnings: none\n";
    } else {
        out += "Recent recorder warnings:\n";
        for l in problems {
            out += &format!("  {l}\n");
        }
    }
    out
}

fn on(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize, amp: f64) -> Vec<i16> {
        (0..frames).flat_map(|i| {
            let v = (amp * (i as f64 * 0.05).sin()) as i16;
            [v, v]
        })
        .collect()
    }

    #[test]
    fn finds_dropouts_and_clipping() {
        let mut pcm = tone(48_000, 10_000.0);
        // A 50 ms hole in the middle of the sound.
        for s in &mut pcm[48_000..48_000 + 2400 * 2] {
            *s = 0;
        }
        pcm.extend(std::iter::repeat_n(i16::MAX, 480));
        let s = audio_stats(&pcm);
        assert_eq!(s.gaps, 1);
        assert!((s.longest_gap_ms - 50.0).abs() < 1.0, "{}", s.longest_gap_ms);
        assert!(s.clipped_pct > 0.0);
        assert!(s.peak_db > -0.1);
    }

    #[test]
    fn silence_at_the_edges_is_not_a_dropout() {
        let mut pcm = vec![0i16; 4800 * 2];
        pcm.extend(tone(9600, 8000.0));
        pcm.extend(vec![0i16; 4800 * 2]);
        let s = audio_stats(&pcm);
        assert_eq!(s.gaps, 0);
        assert_eq!(s.clipped_pct, 0.0);
    }

    #[test]
    fn redacts_the_home_folder() {
        let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) else { return };
        let text = format!("saved {home}\\Videos\\clip.mp4");
        assert!(!redact(&text).contains(&home));
    }
}
