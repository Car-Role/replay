//! Clip editing: trim, join, audio remix and re-encoded exports.
//!
//! By default video is copied without re-encoding, so cuts start on the
//! keyframe at or before the chosen start (recordings have one every 0.5 s).
//! `VideoMode::Encode` decodes and encodes the video again on the GPU: for
//! exact-frame cuts, size targets (Discord) and vertical 9:16 crops. Audio is
//! always decoded, mixed with the chosen volumes and encoded again.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::config::Codec;

use crate::media::{AAC_FRAME, CHANNELS, SAMPLE_RATE, mix_into};
use crate::sys::aac::AacDecoder;
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
    /// A music file under the whole export.
    pub music: Option<Music>,
}

#[derive(Clone, Debug)]
pub struct Music {
    pub path: PathBuf,
    /// Percent.
    pub volume: u32,
    /// Fade the music out over the last few seconds.
    pub fade_out: bool,
}

/// Seconds of the music fade in and fade out.
const FADE_IN: f64 = 0.5;
const FADE_OUT: f64 = 3.0;

/// Music shaped to `frames` frames: cut (or padded with silence), volume and fades.
fn shape_music(pcm: &[i16], frames: usize, m: &Music) -> Vec<i16> {
    let ch = CHANNELS as usize;
    let mut out = vec![0i16; frames * ch];
    let n = pcm.len().min(out.len());
    out[..n].copy_from_slice(&pcm[..n]);
    let rate = f64::from(SAMPLE_RATE);
    let fade_in = (FADE_IN * rate) as usize;
    let fade_out = if m.fade_out { ((FADE_OUT * rate) as usize).min(frames / 2) } else { 0 };
    let music_end = (n / ch).min(frames);
    for f in 0..music_end {
        let mut g = f64::from(m.volume) / 100.0;
        if f < fade_in {
            g *= f as f64 / fade_in as f64;
        }
        if fade_out > 0 && f + fade_out > frames {
            g *= (frames - f) as f64 / fade_out as f64;
        }
        for c in 0..ch {
            let v = &mut out[f * ch + c];
            *v = (f64::from(*v) * g).clamp(-32768.0, 32767.0) as i16;
        }
    }
    out
}

/// How the video of an export is made.
#[derive(Clone, Debug, PartialEq)]
pub enum VideoMode {
    /// Copy the compressed video (fast, cuts on keyframes).
    Copy,
    /// Decode and encode again.
    Encode(EncodeSpec),
}

#[derive(Clone, Debug, PartialEq)]
pub struct EncodeSpec {
    pub codec: Codec,
    pub bitrate_kbps: u32,
    /// Constant bitrate, so a size target is met.
    pub cbr: bool,
    /// Largest output height; None keeps the source height.
    pub max_height: Option<u32>,
    /// Crop to 9:16 for phones. The value is the crop's horizontal centre
    /// (0 = left edge, 1 = right edge).
    pub vertical: Option<f32>,
}

/// Video bitrate and largest height for a file of at most `target_mb`
/// megabytes (10^6 bytes) and `seconds` long, with `audio_kbps` of audio.
pub fn size_target(target_mb: f64, seconds: f64, audio_kbps: u32) -> (u32, Option<u32>) {
    // 10% margin: the encoder overshoots its target by up to about 7%.
    let total_kbps = target_mb * 8000.0 * 0.90 / seconds.max(0.5);
    let video = (total_kbps - f64::from(audio_kbps)).max(150.0) as u32;
    let height = match video {
        0..=599 => Some(480),
        600..=1499 => Some(720),
        1500..=3999 => Some(1080),
        _ => None,
    };
    (video.min(100_000), height)
}

/// The source rectangle (x, y, w, h) and the output size for a frame of
/// `src_w` x `src_h`. Sizes are even.
pub fn plan_frame(src_w: u32, src_h: u32, spec: &EncodeSpec) -> ((u32, u32, u32, u32), (u32, u32)) {
    let (src_w, src_h) = (src_w.max(2), src_h.max(2));
    let crop = match spec.vertical {
        Some(center) => {
            let w = ((u64::from(src_h) * 9 / 16) as u32).min(src_w) & !1;
            let x = (center.clamp(0.0, 1.0) * src_w as f32 - w as f32 / 2.0).clamp(0.0, (src_w - w) as f32) as u32 & !1;
            (x, 0, w, src_h)
        }
        None => (0, 0, src_w, src_h),
    };
    let max_h = spec.max_height.or(spec.vertical.map(|_| 1920));
    let (mut ow, mut oh) = (crop.2, crop.3);
    if let Some(mh) = max_h
        && oh > mh
    {
        ow = (u64::from(ow) * u64::from(mh) / u64::from(oh)) as u32;
        oh = mh;
    }
    (crop, ((ow.max(2)) & !1, (oh.max(2)) & !1))
}

/// What the user picked for an export.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExportChoice {
    /// Largest file size in MB (10^6 bytes).
    pub size_mb: Option<f64>,
    /// Vertical 9:16 crop centre, 0..1.
    pub vertical: Option<f32>,
    /// Output codec; None keeps the source codec.
    pub codec: Option<Codec>,
}

/// The encode settings for an export of `seconds` of `source`. `audio_kbps`
/// is the total bitrate of the audio tracks the export will have.
pub fn encode_spec(source: &Path, seconds: f64, audio_kbps: u32, c: &ExportChoice) -> Result<EncodeSpec, String> {
    let m = reader::read(source).map_err(|e| format!("{}: {e}", source.display()))?;
    let v = m.video().ok_or("the clip has no video")?;
    let TrackKindIn::Video { codec: src_codec, .. } = v.kind else { unreachable!() };
    let codec = c.codec.unwrap_or(src_codec);
    let (bitrate_kbps, max_height, cbr) = match c.size_mb {
        Some(mb) => {
            let (k, h) = size_target(mb, seconds, audio_kbps);
            (k, h, true)
        }
        None => {
            // Same quality as the source; H.264 needs more bits than HEVC or AV1.
            let mut k = video_kbps(&m).max(2_000);
            if codec == Codec::H264 && src_codec != Codec::H264 {
                k = k * 7 / 5;
            }
            (k.min(150_000), None, false)
        }
    };
    Ok(EncodeSpec { codec, bitrate_kbps, cbr, max_height, vertical: c.vertical })
}

/// Average video bitrate of a file, in kbit/s.
pub fn video_kbps(m: &Mp4In) -> u32 {
    let Some(v) = m.video() else { return 0 };
    let bytes: u64 = v.samples.iter().map(|s| u64::from(s.size)).sum();
    (bytes as f64 * 8.0 / 1000.0 / v.duration_seconds().max(0.1)) as u32
}

/// Frames per second of a file's video, rounded.
pub fn video_fps(m: &Mp4In) -> u32 {
    m.video()
        .filter(|v| v.samples.len() > 1)
        .map(|v| ((v.samples.len() - 1) as f64 / (v.seconds(v.samples.len() - 1) - v.seconds(0)).max(0.001)).round() as u32)
        .unwrap_or(60)
        .clamp(10, 240)
}

/// True when the files can be joined without re-encoding.
pub fn same_video_settings(paths: &[PathBuf]) -> bool {
    let kinds: Vec<Option<TrackKindIn>> = paths.iter().map(|p| reader::read(p).ok().and_then(|m| m.video().map(|v| v.kind.clone()))).collect();
    kinds.iter().all(|k| k.is_some() && *k == kinds[0])
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
    let mut dec = AacDecoder::new(asc)?;
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
        dec.decode(&buf, &mut pcm)?;
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
pub fn export(segments: &[Segment], out: &Path, mix: &Mix, video: &VideoMode, progress: &(dyn Fn(f32) + Sync)) -> Result<PathBuf, String> {
    if segments.is_empty() {
        return Err("nothing to export".into());
    }
    if let VideoMode::Encode(spec) = video {
        return export_encoded(segments, out, mix, spec, progress);
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

    let audio = mix_audio(game_pcm, mic_pcm, total_frames, mix)?;
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

/// Decodes the segments' game and mic audio into one timeline each.
fn decode_segments(files: &[Mp4In], segments: &[Segment], ranges: &[(f64, f64)], progress: &dyn Fn(f32)) -> Result<(Option<Vec<i16>>, Option<Vec<i16>>), String> {
    let mut handles: Vec<File> = segments.iter().map(|s| File::open(&s.path)).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    let total: f64 = ranges.iter().map(|r| r.1).sum();
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
        progress((si + 1) as f32 / files.len() as f32);
    }
    Ok((game_pcm, mic_pcm))
}

/// Mixes and encodes the audio tracks of an export.
fn mix_audio(game_pcm: Option<Vec<i16>>, mic_pcm: Option<Vec<i16>>, frames: usize, mix: &Mix) -> Result<Vec<AudioTrackData>, String> {
    let enc = |name: &str, pcm: &[i16]| encode_track(name, pcm, mix.bitrate_kbps);
    if let Some(m) = &mix.music {
        let raw = crate::sys::player::decode_audio_file(&m.path)?;
        let music = shape_music(&raw, frames, m);
        let mut mixed = music.clone();
        if let Some(g) = &game_pcm {
            mix_into(&mut mixed, g, 100, mix.game);
        }
        if let Some(mi) = &mic_pcm {
            mix_into(&mut mixed, mi, 100, mix.mic);
        }
        let mut audio = vec![enc("Mix", &mixed)?];
        if mix.keep_separate {
            if let Some(g) = &game_pcm {
                audio.push(enc("Game", g)?);
            }
            if let Some(mi) = &mic_pcm {
                audio.push(enc("Microphone", mi)?);
            }
            audio.push(enc("Music", &music)?);
        }
        return Ok(audio);
    }
    let mut audio: Vec<AudioTrackData> = Vec::new();
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
    Ok(audio)
}

/// Export with the video decoded and encoded again.
fn export_encoded(segments: &[Segment], out: &Path, mix: &Mix, spec: &EncodeSpec, progress: &(dyn Fn(f32) + Sync)) -> Result<PathBuf, String> {
    let files: Vec<Mp4In> = segments
        .iter()
        .map(|s| reader::read(&s.path).map_err(|e| format!("{}: {e}", s.path.display())))
        .collect::<Result<_, _>>()?;
    let mut pieces = Vec::new();
    let mut ranges = Vec::new();
    let mut size = None;
    for (seg, f) in segments.iter().zip(&files) {
        let v = f.video().ok_or_else(|| format!("{} has no video", seg.path.display()))?;
        let TrackKindIn::Video { width, height, .. } = v.kind else { unreachable!() };
        let dur = v.duration_seconds();
        let (start, end) = (seg.start.clamp(0.0, dur), seg.end.clamp(0.0, dur));
        if end - start < 0.05 {
            continue;
        }
        let (crop, out_size) = plan_frame(width, height, spec);
        // The first clip sets the output size; others are fitted into it.
        size.get_or_insert(out_size);
        pieces.push(crate::sys::transcode::Piece { path: seg.path.clone(), start, end, crop });
        ranges.push((start, end - start));
    }
    let (width, height) = size.ok_or("the selection is empty")?;
    let fps = video_fps(&files[0]);

    // Video first (the slow part), into a temporary file of MP4 samples.
    let out_path = unique(out);
    let tmp_path = out_path.with_extension("video.tmp");
    let mut tmp = std::io::BufWriter::new(File::create(&tmp_path).map_err(|e| format!("cannot write {}: {e}", tmp_path.display()))?);
    let mut infos: Vec<SampleInfo> = Vec::new();
    let mut first_key: Option<Vec<u8>> = None;
    let mut scratch = Vec::new();
    let job = crate::sys::transcode::Job { pieces, codec: spec.codec, width, height, fps, bitrate_kbps: spec.bitrate_kbps, cbr: spec.cbr };
    let video_progress = |f: f32| progress(f * 0.85);
    let to_ts = |t: i64| (t.max(0) as i128 * VIDEO_TIMESCALE as i128 / crate::ring::TICKS_PER_SEC as i128) as u64;
    let result = crate::sys::transcode::run(&job, &video_progress, &mut |f| {
        if f.key && first_key.is_none() {
            first_key = Some(f.data.to_vec());
        }
        crate::mux::bitstream::to_mp4_sample(spec.codec, f.data, &mut scratch);
        tmp.write_all(&scratch).map_err(|e| e.to_string())?;
        let mut time = to_ts(f.time);
        if let Some(prev) = infos.last()
            && time <= prev.time
        {
            time = prev.time + 1;
        }
        infos.push(SampleInfo { time, duration: 0, size: scratch.len() as u32, key: f.key || infos.is_empty() });
        Ok(())
    });
    let cleanup = || {
        let _ = std::fs::remove_file(&tmp_path);
    };
    let header = match result {
        Ok(h) => h,
        Err(e) => {
            drop(tmp);
            cleanup();
            return Err(e);
        }
    };
    if let Err(e) = tmp.flush() {
        cleanup();
        return Err(e.to_string());
    }
    drop(tmp);
    if infos.is_empty() {
        cleanup();
        return Err("no frames were encoded".into());
    }
    let total: f64 = ranges.iter().map(|r| r.1).sum();
    let end_ts = (total * f64::from(VIDEO_TIMESCALE)).round() as u64;
    for i in 0..infos.len() {
        let next = infos.get(i + 1).map(|n| n.time).unwrap_or(end_ts.max(infos[i].time + 1));
        infos[i].duration = next.saturating_sub(infos[i].time).max(1) as u32;
    }
    let params = crate::sys::transcode::params_from(spec.codec, first_key.as_deref(), header.as_deref());
    let config = match spec.codec {
        Codec::H264 => crate::mux::bitstream::avcc(&params),
        Codec::Hevc => crate::mux::bitstream::hvcc(&params),
        Codec::Av1 => params.sps.first().and_then(|s| crate::mux::bitstream::av1c(s)),
    };
    let Some(config) = config else {
        cleanup();
        return Err("the encoder did not produce codec headers".into());
    };

    // Audio.
    let audio = (|| {
        let (g, m) = decode_segments(&files, segments, &ranges, &|f| progress(0.85 + 0.05 * f))?;
        mix_audio(g, m, (total * f64::from(SAMPLE_RATE)).round() as usize, mix)
    })();
    let audio = match audio {
        Ok(a) => a,
        Err(e) => {
            cleanup();
            return Err(e);
        }
    };
    progress(0.92);

    let mut tracks = vec![Track {
        kind: TrackKind::Video { codec: spec.codec, width, height, config },
        timescale: VIDEO_TIMESCALE,
        samples: infos,
        start_offset: 0,
        name: "Video".into(),
        enabled: true,
    }];
    tracks.extend(audio.iter().map(audio_track));
    let offsets: Vec<u64> = tracks[0].samples.iter().scan(0u64, |o, s| {
        let at = *o;
        *o += u64::from(s.size);
        Some(at)
    }).collect();
    let mut src = match File::open(&tmp_path) {
        Ok(f) => f,
        Err(e) => {
            cleanup();
            return Err(e.to_string());
        }
    };
    let sizes: Vec<u32> = tracks[0].samples.iter().map(|s| s.size).collect();
    let r = write_tracks(&out_path, &tracks, |t, i, buf| {
        if t == 0 {
            src.seek(SeekFrom::Start(offsets[i]))?;
            let start = buf.len();
            buf.resize(start + sizes[i] as usize, 0);
            src.read_exact(&mut buf[start..])?;
        } else {
            buf.extend_from_slice(&audio[t - 1].packets[i]);
        }
        Ok(())
    });
    drop(src);
    cleanup();
    r?;
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
    fn music_is_cut_and_faded() {
        let m = Music { path: PathBuf::new(), volume: 50, fade_out: true };
        let pcm = vec![1000i16; 48_000 * 2 * 10];
        let out = shape_music(&pcm, 48_000 * 6, &m);
        assert_eq!(out.len(), 48_000 * 2 * 6);
        assert_eq!(out[0], 0, "fades in");
        assert_eq!(out[48_000 * 2], 500, "half volume after the fade in");
        assert!(out[out.len() - 2].abs() < 5, "faded out at the end");
        // Short music leaves silence after it.
        let short = shape_music(&pcm[..48_000 * 2], 48_000 * 3, &m);
        assert_eq!(short[48_000 * 2 * 2], 0);
    }

    #[test]
    fn size_targets_fit_and_scale_down() {
        let (v, h) = size_target(10.0, 30.0, 128);
        // 10 MB over 30 s with a 10% margin: 2400 kbit/s in total.
        assert!((2200..2300).contains(&v), "{v}");
        assert_eq!(h, Some(1080));
        assert_eq!(size_target(10.0, 120.0, 128).1, Some(480));
        assert_eq!(size_target(500.0, 60.0, 128).1, None);
    }

    #[test]
    fn vertical_crop_is_9_by_16_and_inside_the_frame() {
        let spec = EncodeSpec { codec: Codec::H264, bitrate_kbps: 8000, cbr: false, max_height: None, vertical: Some(0.5) };
        let ((x, y, w, h), out) = plan_frame(1920, 1080, &spec);
        assert_eq!((w, h), (606, 1080));
        assert_eq!((x, y), (656, 0));
        assert_eq!(out, (606, 1080));
        let left = EncodeSpec { vertical: Some(0.0), ..spec.clone() };
        assert_eq!(plan_frame(1920, 1080, &left).0.0, 0);
        let right = EncodeSpec { vertical: Some(1.0), ..spec.clone() };
        assert_eq!(plan_frame(1920, 1080, &right).0.0, 1920 - 606);
        // 4K sources are scaled to 1920 high.
        assert_eq!(plan_frame(3840, 2160, &spec).1, (1078, 1920));
        let plain = EncodeSpec { vertical: None, max_height: Some(720), ..spec };
        assert_eq!(plan_frame(1920, 1080, &plain), ((0, 0, 1920, 1080), (1280, 720)));
    }

    #[test]
    fn range_starts_on_keyframe_at_or_before_start() {
        let t = track(30, 600); // 10 s at 60 fps, keyframe every 0.5 s
        assert_eq!(video_range(&t, 1.2, 2.0), (60, 120));
        assert_eq!(video_range(&t, 1.5, 2.0), (90, 120));
        assert_eq!(video_range(&t, 0.0, 100.0), (0, 600));
    }
}
