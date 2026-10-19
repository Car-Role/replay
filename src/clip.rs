//! Turns ring snapshots into an MP4 file.
//!
//! Track layout: video, then the mix ("Game + Mic", default), "Game",
//! "Microphone" and "Voice chat". Sites such as Discord and Streamable play
//! only the first audio track, so the mix comes first; editors still get
//! separate tracks.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use crate::media::{AAC_FRAME, CHANNELS, SAMPLE_RATE, VideoState, mix_into};
use crate::sys::aac::{AacDecoder, AacEncoder};
use crate::config::Config;
use crate::mux::bitstream::{aac_asc, av1c, avcc, find_param_sets, hvcc, to_mp4_sample};
use crate::mux::mp4::{self, SampleInfo, Track, TrackKind, VIDEO_TIMESCALE};
use crate::ring::{Packet, TICKS_PER_SEC, Ticks};
use crate::config::Codec;

/// The separate audio tracks ("stems"), in file order: game, mic, voice chat.
pub const STEMS: [&str; 3] = ["Game", "Microphone", "Voice chat"];
/// Their short names, used to name the mixed track ("Game + Mic").
const SHORT: [&str; 3] = ["Game", "Mic", "Voice"];

/// The name of the mixed track made from the stems in `present`.
pub fn mix_name(present: &[usize]) -> String {
    present.iter().map(|&i| SHORT[i]).collect::<Vec<_>>().join(" + ")
}

/// True for a track name that is a mix or music rather than one stem.
pub fn is_mix_name(name: &str) -> bool {
    name.contains(" + ") || name == "Mix" || name == "Music"
}

pub struct ClipSource {
    pub game_name: String,
    pub video: Vec<Packet>,
    pub state: VideoState,
    pub game_audio: Option<Vec<Packet>>,
    pub mic: Option<Vec<Packet>>,
    pub voice: Option<Vec<Packet>>,
}

pub struct AudioOptions {
    pub bitrate_kbps: u32,
    pub mixed_track: bool,
    pub game_volume: u32,
    pub mic_volume: u32,
    pub voice_volume: u32,
}

impl AudioOptions {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            bitrate_kbps: cfg.audio.bitrate_kbps,
            mixed_track: cfg.audio.mixed_track,
            game_volume: cfg.audio.game_volume,
            mic_volume: cfg.audio.microphone_volume,
            voice_volume: cfg.audio.voice_chat_volume,
        }
    }

    /// Gains of the stems, in `STEMS` order.
    pub fn gains(&self) -> [u32; 3] {
        [self.game_volume, self.mic_volume, self.voice_volume]
    }
}

/// Builds the file name `<dir>\<Game>\<Game> 2026-10-18 21-05-33.mp4`.
pub fn clip_path(output_dir: &Path, game: &str) -> PathBuf {
    clip_path_with(output_dir, game, "")
}

/// Like `clip_path`, with `suffix` after the time (for example " (session)").
pub fn clip_path_with(output_dir: &Path, game: &str, suffix: &str) -> PathBuf {
    let t = crate::util::local_time();
    let game = crate::util::sanitize_file_name(game);
    let dir = output_dir.join(&game);
    let stem = format!(
        "{game} {:04}-{:02}-{:02} {:02}-{:02}-{:02}{suffix}",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    );
    let mut p = dir.join(format!("{stem}.mp4"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{stem} ({n}).mp4"));
        n += 1;
    }
    p
}

/// Encoded audio ready for the muxer.
pub struct AudioTrackData {
    pub name: String,
    pub packets: Vec<Vec<u8>>,
    /// Delay from the video start, in samples.
    pub offset: u64,
}

fn audio_from_ring(name: &str, packets: &[Packet], t0: Ticks, t_end: Ticks) -> Option<AudioTrackData> {
    let mut it = packets.iter().filter(|p| p.pts >= t0 && p.pts < t_end).peekable();
    let first = it.peek()?.pts;
    let offset = ((first - t0) as i128 * SAMPLE_RATE as i128 / TICKS_PER_SEC as i128) as u64;
    Some(AudioTrackData { name: name.into(), packets: it.map(|p| p.data.to_vec()).collect(), offset })
}

/// Decodes AAC tracks, mixes them with gains, and encodes a new track.
pub fn mix_tracks(name: &str, tracks: &[(&AudioTrackData, u32)], bitrate_kbps: u32) -> Result<AudioTrackData, String> {
    let asc = aac_asc(SAMPLE_RATE, CHANNELS);
    let mut mixed: Vec<i16> = Vec::new();
    for (i, (t, gain)) in tracks.iter().enumerate() {
        let mut dec = AacDecoder::new(&asc)?;
        let mut pcm = Vec::with_capacity(t.packets.len() * AAC_FRAME * CHANNELS as usize);
        for p in &t.packets {
            dec.decode(p, &mut pcm)?;
        }
        let start = t.offset as usize * CHANNELS as usize;
        if mixed.len() < start + pcm.len() {
            mixed.resize(start + pcm.len(), 0);
        }
        let dst = &mut mixed[start..start + pcm.len()];
        // The first track is scaled as it is copied in; later tracks are added.
        let base_gain = if i == 0 { 0 } else { 100 };
        mix_into(dst, &pcm, base_gain, *gain);
    }
    encode_track(name, &mixed, bitrate_kbps)
}

/// Writes a clip from ring snapshots. Returns the file path.
pub fn write_clip(src: ClipSource, output_dir: &Path, opts: &AudioOptions) -> Result<PathBuf, String> {
    let video = &src.video;
    let first = video.first().ok_or("the buffer is empty")?;
    let t0 = first.pts;
    let frame = TICKS_PER_SEC / i64::from(src.state.fps.max(1));
    let t_end = video.last().map(|p| p.pts + frame).unwrap_or(t0);

    let rings = [&src.game_audio, &src.mic, &src.voice];
    let stems: Vec<(usize, AudioTrackData)> = rings
        .iter()
        .enumerate()
        .filter_map(|(i, r)| r.as_deref().and_then(|p| audio_from_ring(STEMS[i], p, t0, t_end)).map(|a| (i, a)))
        .collect();
    let mut audio = Vec::new();
    if opts.mixed_track && stems.len() > 1 {
        let gains = opts.gains();
        let parts: Vec<(&AudioTrackData, u32)> = stems.iter().map(|(i, a)| (a, gains[*i])).collect();
        let present: Vec<usize> = stems.iter().map(|(i, _)| *i).collect();
        match mix_tracks(&mix_name(&present), &parts, opts.bitrate_kbps) {
            Ok(mix) => audio.push(mix),
            Err(e) => log::warn!("audio mix failed, saving separate tracks only: {e}"),
        }
    }
    audio.extend(stems.into_iter().map(|(_, a)| a));

    let path = clip_path(output_dir, &src.game_name);
    let samples: Vec<VideoSample> = video
        .iter()
        .map(|p| VideoSample { time: p.pts - t0, key: p.key, data: &p.data[..] })
        .collect();
    write_mp4(&path, &src.state, &samples, &audio)?;
    Ok(path)
}

pub struct VideoSample<'a> {
    /// Ticks from the clip start.
    pub time: Ticks,
    pub key: bool,
    /// Encoder output (Annex B or OBUs).
    pub data: &'a [u8],
}

/// MP4 track for encoded AAC packets.
pub fn audio_track(a: &AudioTrackData) -> Track {
    let samples = a
        .packets
        .iter()
        .enumerate()
        .map(|(i, p)| SampleInfo { time: (i * AAC_FRAME) as u64, duration: AAC_FRAME as u32, size: p.len() as u32, key: true })
        .collect();
    Track {
        kind: TrackKind::Audio {
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            asc: aac_asc(SAMPLE_RATE, CHANNELS).to_vec(),
            bitrate: 192_000,
        },
        timescale: SAMPLE_RATE,
        samples,
        start_offset: a.offset,
        name: a.name.clone(),
        enabled: true,
    }
}

/// Encodes PCM into an AAC track.
pub fn encode_track(name: &str, pcm: &[i16], bitrate_kbps: u32) -> Result<AudioTrackData, String> {
    let mut enc = AacEncoder::new(bitrate_kbps)?;
    let mut packets = Vec::new();
    enc.encode(pcm, |p| packets.push(p.to_vec()))?;
    enc.finish(|p| packets.push(p.to_vec()))?;
    Ok(AudioTrackData { name: name.into(), packets, offset: 0 })
}

/// Writes `tracks` to `path` through a temporary `.part` file.
pub fn write_tracks(
    path: &Path,
    tracks: &[Track],
    fill: impl FnMut(usize, usize, &mut Vec<u8>) -> std::io::Result<()>,
) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let part = path.with_extension("mp4.part");
    let result = (|| {
        let file = File::create(&part)?;
        let mut w = BufWriter::with_capacity(4 << 20, file);
        mp4::write(&mut w, tracks, fill)?;
        let file = w.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&part, path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&part);
        return Err(format!("cannot write {}: {e}", path.display()));
    }
    Ok(())
}

/// Shared by the recorder and the editor.
pub fn write_mp4(path: &Path, state: &VideoState, video: &[VideoSample], audio: &[AudioTrackData]) -> Result<(), String> {
    let codec = state.codec;
    let mut params = state.params.clone();
    if let Some(k) = video.iter().find(|s| s.key) {
        let ps = find_param_sets(codec, k.data);
        if ps.is_complete(codec) {
            params = ps;
        }
    }
    let config = match codec {
        Codec::H264 => avcc(&params),
        Codec::Hevc => hvcc(&params),
        Codec::Av1 => params.sps.first().and_then(|s| av1c(s)),
    }
    .ok_or("the encoder has not produced codec headers yet")?;

    let to_ts = |t: Ticks| (t.max(0) as i128 * VIDEO_TIMESCALE as i128 / TICKS_PER_SEC as i128) as u64;
    let nominal = VIDEO_TIMESCALE / state.fps.max(1);
    let mut scratch = Vec::new();
    let mut vs = Vec::with_capacity(video.len());
    let mut prev_time = None;
    for (i, s) in video.iter().enumerate() {
        let mut time = to_ts(s.time);
        if let Some(p) = prev_time
            && time <= p
        {
            time = p + 1;
        }
        prev_time = Some(time);
        let next = video.get(i + 1).map(|n| to_ts(n.time));
        let duration = match next {
            Some(n) if n > time => (n - time) as u32,
            Some(_) => 1,
            None => nominal,
        };
        to_mp4_sample(codec, s.data, &mut scratch);
        vs.push(SampleInfo { time, duration, size: scratch.len() as u32, key: s.key });
    }

    let mut tracks = vec![Track {
        kind: TrackKind::Video { codec, width: state.width, height: state.height, config },
        timescale: VIDEO_TIMESCALE,
        samples: vs,
        start_offset: 0,
        name: "Video".into(),
        enabled: true,
    }];
    tracks.extend(audio.iter().map(audio_track));

    write_tracks(path, &tracks, |t, i, buf| {
        if t == 0 {
            to_mp4_sample(codec, video[i].data, buf);
        } else {
            buf.extend_from_slice(&audio[t - 1].packets[i]);
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_track_names() {
        assert_eq!(mix_name(&[0, 1]), "Game + Mic");
        assert_eq!(mix_name(&[0, 1, 2]), "Game + Mic + Voice");
        assert_eq!(mix_name(&[0, 2]), "Game + Voice");
        for name in ["Game + Mic", "Game + Voice", "Mix", "Music"] {
            assert!(is_mix_name(name), "{name}");
        }
        for name in STEMS {
            assert!(!is_mix_name(name), "{name}");
        }
    }
}
