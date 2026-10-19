//! Clip editing: trim, join and audio remix.
//!
//! Video is copied without re-encoding, so cuts start on the keyframe at or
//! before the chosen start (recordings have one every 0.5 s). Audio is
//! decoded, mixed with the chosen volumes and encoded again, which is fast.

use std::fs::File;
use std::path::{Path, PathBuf};

use crate::audio::{AAC_FRAME, AacDecoder, CHANNELS, SAMPLE_RATE, mix_into};
use crate::clip::{AudioTrackData, audio_track, encode_track, write_tracks};
use crate::mux::mp4::{SampleInfo, Track, TrackKind, VIDEO_TIMESCALE};
use crate::mux::reader::{self, Mp4In, TrackIn, TrackKindIn, read_sample};

#[derive(Clone, Debug)]
pub struct Segment {
    pub path: PathBuf,
    /// Seconds from the clip start.
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug)]
pub struct Mix {
    /// Percent; 100 keeps the level.
    pub game: u32,
    pub mic: u32,
    /// Also keep separate Game and Microphone tracks after the mix.
    pub keep_separate: bool,
    pub bitrate_kbps: u32,
}

/// Game and mic tracks of a file. Files from other programs have one
/// unnamed audio track, which is treated as game audio.
pub fn audio_sources(m: &Mp4In) -> (Option<&TrackIn>, Option<&TrackIn>) {
    let game = m.audio_named("Game").or_else(|| m.audio().find(|t| t.name != "Microphone" && t.name != "Game + Mic"));
    let mic = m.audio_named("Microphone");
    (game, mic)
}

/// Decodes `track` between `from` and `to` seconds into exactly
/// `(to - from) * 48000` stereo frames (silence where there is no audio).
pub fn decode_range(file: &mut File, track: &TrackIn, from: f64, to: f64) -> Result<Vec<i16>, String> {
    let TrackKindIn::Audio { sample_rate, channels, asc } = &track.kind else { return Err("not audio".into()) };
    if *sample_rate != SAMPLE_RATE || *channels != CHANNELS {
        return Err(format!("unsupported audio format ({sample_rate} Hz, {channels} ch)"));
    }
    let frames = ((to - from).max(0.0) * f64::from(SAMPLE_RATE)).round() as usize;
    let mut out = vec![0i16; frames * CHANNELS as usize];
    let mut dec = AacDecoder::new(asc).map_err(|e| e.to_string())?;
    let frame_secs = AAC_FRAME as f64 / f64::from(SAMPLE_RATE);
    // Start two frames early: AAC frames overlap, so the first decoded frame is not clean.
    let first = (0..track.samples.len()).find(|&i| track.seconds(i) + frame_secs > from).unwrap_or(track.samples.len());
    let begin = first.saturating_sub(2);
    let mut buf = Vec::new();
    let mut pcm = Vec::new();
    for i in begin..track.samples.len() {
        let t = track.seconds(i);
        if t >= to {
            break;
        }
        read_sample(file, &track.samples[i], &mut buf).map_err(|e| e.to_string())?;
        pcm.clear();
        dec.decode(&buf, &mut pcm).map_err(|e| e.to_string())?;
        if i < first {
            continue;
        }
        let pos = ((t - from) * f64::from(SAMPLE_RATE)).round() as i64 * i64::from(CHANNELS);
        for (k, v) in pcm.iter().enumerate() {
            let p = pos + k as i64;
            if p >= 0 && (p as usize) < out.len() {
                out[p as usize] = *v;
            }
        }
    }
    Ok(out)
}

/// Picks the sample range for [start, end): from the keyframe at or before
/// `start` up to the first sample at or after `end`.
fn video_range(v: &TrackIn, start: f64, end: f64) -> (usize, usize) {
    let n = v.samples.len();
    let mut a = 0;
    for i in 0..n {
        if v.seconds(i) > start + 1e-6 {
            break;
        }
        if v.samples[i].key {
            a = i;
        }
    }
    let b = (a..n).find(|&i| v.seconds(i) >= end - 1e-6).unwrap_or(n).max(a + 1).min(n);
    (a, b)
}

/// Writes a new file from the segments. Returns its path.
pub fn export(segments: &[Segment], out: &Path, mix: &Mix, progress: &(dyn Fn(f32) + Sync)) -> Result<PathBuf, String> {
    if segments.is_empty() {
        return Err("nothing to export".into());
    }
    let files: Vec<Mp4In> = segments
        .iter()
        .map(|s| reader::read(&s.path).map_err(|e| format!("{}: {e}", s.path.display())))
        .collect::<Result<_, _>>()?;
    let first_video = files[0].video().ok_or("the clip has no video")?.kind.clone();
    for (s, f) in segments.iter().zip(&files) {
        let v = f.video().ok_or_else(|| format!("{} has no video", s.path.display()))?;
        if v.kind != first_video {
            return Err(format!(
                "{} was recorded with different video settings, so it cannot be joined without re-encoding",
                s.path.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
    }
    let TrackKindIn::Video { codec, width, height, config } = first_video else { unreachable!() };

    // Video sample plan: (segment, sample index, output time in seconds).
    let mut plan: Vec<(usize, usize, f64)> = Vec::new();
    let mut ranges = Vec::new(); // (t0, length) per segment
    let mut acc = 0.0;
    for (si, (seg, f)) in segments.iter().zip(&files).enumerate() {
        let v = f.video().expect("checked");
        let (a, b) = video_range(v, seg.start, seg.end);
        let t0 = v.seconds(a);
        let t_end = if b < v.samples.len() { v.seconds(b) } else { v.duration_seconds() };
        for i in a..b {
            plan.push((si, i, acc + v.seconds(i) - t0));
        }
        ranges.push((t0, t_end - t0));
        acc += t_end - t0;
    }
    let total = acc;

    // Audio: decode, line up per segment, mix.
    let mut handles: Vec<File> = segments.iter().map(|s| File::open(&s.path)).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    let total_frames = (total * f64::from(SAMPLE_RATE)).round() as usize;
    let mut game_pcm: Option<Vec<i16>> = None;
    let mut mic_pcm: Option<Vec<i16>> = None;
    let mut pos = 0usize;
    for (si, f) in files.iter().enumerate() {
        let (t0, len) = ranges[si];
        let frames = (len * f64::from(SAMPLE_RATE)).round() as usize;
        let (game, mic) = audio_sources(f);
        for (src, dst) in [(game, &mut game_pcm), (mic, &mut mic_pcm)] {
            if let Some(track) = src {
                let pcm = decode_range(&mut handles[si], track, t0, t0 + len)?;
                let buf = dst.get_or_insert_with(|| vec![0; total_frames * CHANNELS as usize]);
                let at = pos * CHANNELS as usize;
                let n = pcm.len().min(buf.len().saturating_sub(at));
                buf[at..at + n].copy_from_slice(&pcm[..n]);
            }
        }
        pos += frames;
        progress(0.4 * (si + 1) as f32 / files.len() as f32);
    }

    let mut audio: Vec<AudioTrackData> = Vec::new();
    let enc = |name: &str, pcm: &[i16]| encode_track(name, pcm, mix.bitrate_kbps).map_err(|e| e.to_string());
    match (&game_pcm, &mic_pcm) {
        (Some(g), Some(m)) => {
            let mut mixed = vec![0i16; g.len()];
            mix_into(&mut mixed, g, 0, mix.game);
            mix_into(&mut mixed, m, 100, mix.mic);
            audio.push(enc("Game + Mic", &mixed)?);
            if mix.keep_separate {
                audio.push(enc("Game", g)?);
                audio.push(enc("Microphone", m)?);
            }
        }
        (Some(one), None) | (None, Some(one)) => {
            let gain = if game_pcm.is_some() { mix.game } else { mix.mic };
            let mut v = vec![0i16; one.len()];
            mix_into(&mut v, one, 0, gain);
            audio.push(enc(if game_pcm.is_some() { "Game" } else { "Microphone" }, &v)?);
        }
        (None, None) => {}
    }
    progress(0.6);

    // Video track from the plan (samples are already in MP4 format).
    let to_ts = |s: f64| (s * f64::from(VIDEO_TIMESCALE)).round() as u64;
    let mut vs = Vec::with_capacity(plan.len());
    for (k, &(si, i, t)) in plan.iter().enumerate() {
        let time = to_ts(t);
        let next = plan.get(k + 1).map(|n| to_ts(n.2)).unwrap_or(to_ts(total));
        let src = files[si].video().expect("checked").samples[i];
        vs.push(SampleInfo { time, duration: next.saturating_sub(time).max(1) as u32, size: src.size, key: src.key || k == 0 });
    }
    let mut tracks = vec![Track {
        kind: TrackKind::Video { codec, width, height, config },
        timescale: VIDEO_TIMESCALE,
        samples: vs,
        start_offset: 0,
        name: "Video".into(),
        enabled: true,
    }];
    tracks.extend(audio.iter().map(audio_track));

    let out_path = unique(out);
    let n_video = plan.len().max(1);
    write_tracks(&out_path, &tracks, |t, i, buf| {
        if t == 0 {
            let (si, idx, _) = plan[i];
            let s = files[si].video().expect("checked").samples[idx];
            reader::read_sample(&mut handles[si], &s, buf)?;
            if i % 64 == 0 {
                progress(0.6 + 0.4 * i as f32 / n_video as f32);
            }
        } else {
            buf.extend_from_slice(&audio[t - 1].packets[i]);
        }
        Ok(())
    })?;
    progress(1.0);
    Ok(out_path)
}

/// `name.mp4` -> `name (2).mp4` when taken.
pub fn unique(p: &Path) -> PathBuf {
    if !p.exists() {
        return p.to_path_buf();
    }
    let stem = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let dir = p.parent().unwrap_or(Path::new("."));
    (2..).map(|n| dir.join(format!("{stem} ({n}).mp4"))).find(|c| !c.exists()).expect("unbounded")
}

/// Default output name for an edit of `source`.
pub fn edited_name(source: &Path, suffix: &str) -> PathBuf {
    let stem = source.file_stem().unwrap_or_default().to_string_lossy();
    source.with_file_name(format!("{stem} ({suffix}).mp4"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::reader::SampleIn;

    fn track(keys_every: usize, n: usize) -> TrackIn {
        TrackIn {
            kind: TrackKindIn::Video { codec: crate::config::Codec::Av1, width: 2, height: 2, config: vec![] },
            name: "Video".into(),
            timescale: 60,
            samples: (0..n).map(|i| SampleIn { offset: 0, size: 1, time: i as u64, duration: 1, key: i % keys_every == 0 }).collect(),
            start_offset: 0,
            enabled: true,
        }
    }

    #[test]
    fn range_starts_on_keyframe_at_or_before_start() {
        let t = track(30, 600); // 10 s at 60 fps, keyframe every 0.5 s
        assert_eq!(video_range(&t, 1.2, 2.0), (60, 120));
        assert_eq!(video_range(&t, 1.5, 2.0), (90, 120));
        assert_eq!(video_range(&t, 0.0, 100.0), (0, 600));
    }
}
