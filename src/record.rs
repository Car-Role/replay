//! Manual recording of a whole session. A background thread takes new
//! packets from the game's replay buffers twice a second and appends them to
//! a file, so a recording can run for hours without using more memory. The
//! mixed "Game + Mic" track is decoded, mixed and encoded as it goes.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::clip::AudioOptions;
use crate::config::Codec;
use crate::media::{AAC_FRAME, CHANNELS, SAMPLE_RATE, VideoState, mix_into};
use crate::mux::bitstream::{aac_asc, av1c, avcc, find_param_sets, hvcc, to_mp4_sample};
use crate::mux::mp4::{SampleInfo, StreamWriter, Track, TrackKind, VIDEO_TIMESCALE};
use crate::ring::{PacketRing, TICKS_PER_SEC, Ticks};
use crate::sys::aac::{AacDecoder, AacEncoder};

const POLL: Duration = Duration::from_millis(500);
/// When one audio source runs this far ahead of the other, the other is
/// treated as silent up to one second behind, so the mix keeps going.
const MAX_LAG_FRAMES: u64 = 2 * SAMPLE_RATE as u64;

/// The buffers of one game being recorded.
#[derive(Clone)]
pub struct Sources {
    pub video: Arc<Mutex<PacketRing>>,
    pub state: Arc<Mutex<VideoState>>,
    pub game: Option<Arc<Mutex<PacketRing>>>,
    pub mic: Option<Arc<Mutex<PacketRing>>>,
    pub voice: Option<Arc<Mutex<PacketRing>>>,
}

impl Sources {
    /// The audio rings in `clip::STEMS` order.
    fn audio(&self) -> [&Option<Arc<Mutex<PacketRing>>>; 3] {
        [&self.game, &self.mic, &self.voice]
    }
}

/// A running recording. Dropping it does not stop it; call `stop`.
pub struct Recording {
    pub game: String,
    pub started: Instant,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Recording {
    /// Asks the recording to finish. `done` (from `start`) runs when the file is complete.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }

    /// Stops and waits for the file to be finished (at exit).
    pub fn stop_and_wait(&mut self) {
        self.stop();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Starts recording. `done` gets the file path, or the error.
pub fn start(
    output_dir: &Path,
    game: &str,
    src: Sources,
    opts: AudioOptions,
    done: impl FnOnce(Result<PathBuf, String>) + Send + 'static,
) -> Result<Recording, String> {
    let path = crate::clip::clip_path_with(output_dir, game, " (session)");
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    // The codec objects belong to the recording thread, so the writer is
    // made there; the start result comes back over a channel.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let p2 = path.clone();
    let thread = std::thread::Builder::new()
        .name("record".into())
        .spawn(move || {
            crate::sys::init_tool();
            let mut writer = match Writer::new(&p2, &src, &opts) {
                Ok(w) => {
                    let _ = ready_tx.send(Ok(()));
                    w
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let result = (|| {
                loop {
                    let last = s2.load(Ordering::Acquire);
                    writer.poll(&src, last)?;
                    if last {
                        break;
                    }
                    let until = Instant::now() + POLL;
                    while Instant::now() < until && !s2.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
                writer.finish()
            })();
            if let Err(e) = &result {
                log::error!("recording failed: {e}");
            }
            done(result);
        })
        .map_err(|e| e.to_string())?;
    match ready_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            let _ = thread.join();
            return Err(e);
        }
        Err(_) => return Err("the recording thread stopped".into()),
    }
    log::info!("recording session of {game} to {}", path.display());
    Ok(Recording { game: game.to_string(), started: Instant::now(), stop, thread: Some(thread) })
}

struct AudioIn {
    track: usize,
    /// pts of the last packet taken.
    last: Option<Ticks>,
    count: u64,
    /// Decoder and output position (frames from t0) for the mix.
    dec: Option<AacDecoder>,
    pos: u64,
    /// This source's place in the mixer.
    slot: usize,
}

struct Mixer {
    track: usize,
    enc: AacEncoder,
    /// Decoded PCM of each mixed source from frame `base` on (interleaved stereo).
    pcm: Vec<Vec<i16>>,
    ends: Vec<u64>,
    base: u64,
    gains: Vec<u32>,
    count: u64,
}

impl Mixer {
    /// Places decoded PCM of source `i` at frame `at`.
    fn put(&mut self, i: usize, at: u64, pcm: &[i16]) {
        let ch = CHANNELS as usize;
        let mut pcm = pcm;
        let mut at = at;
        if at < self.base {
            // Already mixed; drop the part before the mix position.
            let skip = ((self.base - at) as usize * ch).min(pcm.len());
            pcm = &pcm[skip..];
            at = self.base;
        }
        let start = (at - self.base) as usize * ch;
        let buf = &mut self.pcm[i];
        if buf.len() < start + pcm.len() {
            buf.resize(start + pcm.len(), 0);
        }
        buf[start..start + pcm.len()].copy_from_slice(pcm);
        self.ends[i] = self.ends[i].max(at + (pcm.len() / ch) as u64);
    }

    /// Mixes and encodes what every source covers (or everything, at the end).
    fn drain(&mut self, flush: bool, out: &mut Vec<Vec<u8>>) -> Result<(), String> {
        let lo = self.ends.iter().copied().min().unwrap_or(0);
        let hi = self.ends.iter().copied().max().unwrap_or(0);
        let ready = if flush {
            hi
        } else if hi - lo > MAX_LAG_FRAMES {
            hi - SAMPLE_RATE as u64
        } else {
            lo
        };
        if ready <= self.base {
            return Ok(());
        }
        let n = (ready - self.base) as usize * CHANNELS as usize;
        let mut mixed = vec![0i16; n];
        for (i, buf) in self.pcm.iter_mut().enumerate() {
            if buf.len() < n {
                buf.resize(n, 0);
            }
            mix_into(&mut mixed, &buf[..n], 100, self.gains[i]);
            buf.drain(..n);
        }
        self.base = ready;
        for e in &mut self.ends {
            *e = (*e).max(ready);
        }
        self.enc.encode(&mixed, |p| out.push(p.to_vec()))?;
        if flush {
            self.enc.finish(|p| out.push(p.to_vec()))?;
        }
        Ok(())
    }
}

struct Writer {
    w: StreamWriter<BufWriter<File>>,
    part: PathBuf,
    path: PathBuf,
    codec: Codec,
    fps: u32,
    t0: Option<Ticks>,
    last_video: Ticks,
    last_video_time: Option<u64>,
    first_key: Option<Vec<u8>>,
    audio: [Option<AudioIn>; 3],
    mixer: Option<Mixer>,
    scratch: Vec<u8>,
}

fn audio_track(name: &str, bitrate_kbps: u32) -> Track {
    Track {
        kind: TrackKind::Audio { sample_rate: SAMPLE_RATE, channels: CHANNELS, asc: aac_asc(SAMPLE_RATE, CHANNELS).to_vec(), bitrate: bitrate_kbps * 1000 },
        timescale: SAMPLE_RATE,
        samples: Vec::new(),
        start_offset: 0,
        name: name.into(),
        enabled: true,
    }
}

impl Writer {
    fn new(path: &Path, src: &Sources, opts: &AudioOptions) -> Result<Self, String> {
        let state = src.state.lock().map_err(|_| "recorder state unavailable")?.clone();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        let part = path.with_extension("mp4.part");
        let file = File::create(&part).map_err(|e| format!("cannot write {}: {e}", part.display()))?;
        let mut tracks = vec![Track {
            kind: TrackKind::Video { codec: state.codec, width: state.width, height: state.height, config: Vec::new() },
            timescale: VIDEO_TIMESCALE,
            samples: Vec::new(),
            start_offset: 0,
            name: "Video".into(),
            enabled: true,
        }];
        let present: Vec<usize> = src.audio().iter().enumerate().filter(|(_, r)| r.is_some()).map(|(i, _)| i).collect();
        let mut mixer = None;
        if present.len() > 1 && opts.mixed_track {
            let gains = opts.gains();
            mixer = Some(Mixer {
                track: tracks.len(),
                enc: AacEncoder::new(opts.bitrate_kbps)?,
                pcm: vec![Vec::new(); present.len()],
                ends: vec![0; present.len()],
                base: 0,
                gains: present.iter().map(|&i| gains[i]).collect(),
                count: 0,
            });
            tracks.push(audio_track(&crate::clip::mix_name(&present), opts.bitrate_kbps));
        }
        let asc = aac_asc(SAMPLE_RATE, CHANNELS);
        let mut audio: [Option<AudioIn>; 3] = [None, None, None];
        for (slot, &i) in present.iter().enumerate() {
            let dec = if mixer.is_some() { Some(AacDecoder::new(&asc)?) } else { None };
            audio[i] = Some(AudioIn { track: tracks.len(), last: None, count: 0, dec, pos: 0, slot });
            tracks.push(audio_track(crate::clip::STEMS[i], opts.bitrate_kbps));
        }
        let w = StreamWriter::new(BufWriter::with_capacity(4 << 20, file), tracks).map_err(|e| e.to_string())?;
        Ok(Self {
            w,
            part,
            path: path.to_path_buf(),
            codec: state.codec,
            fps: state.fps,
            t0: None,
            last_video: 0,
            last_video_time: None,
            first_key: None,
            audio,
            mixer,
            scratch: Vec::new(),
        })
    }

    /// Appends everything new in the buffers.
    fn poll(&mut self, src: &Sources, last: bool) -> Result<(), String> {
        let io = |e: std::io::Error| format!("cannot write the recording: {e}");
        // Video. The first poll starts at the newest keyframe.
        let packets = {
            let ring = src.video.lock().map_err(|_| "video buffer unavailable")?;
            match self.t0 {
                None => ring.snapshot(0),
                Some(_) => ring.snapshot_from(self.last_video + 1),
            }
        };
        if self.t0.is_none() {
            let Some(first) = packets.first() else { return Ok(()) };
            self.t0 = Some(first.pts);
        }
        let t0 = self.t0.expect("set");
        let to_ts = |t: Ticks| ((t - t0).max(0) as i128 * VIDEO_TIMESCALE as i128 / TICKS_PER_SEC as i128) as u64;
        let mut infos = Vec::with_capacity(packets.len());
        let mut data = Vec::new();
        for p in &packets {
            if p.key && self.first_key.is_none() {
                self.first_key = Some(p.data.to_vec());
            }
            to_mp4_sample(self.codec, &p.data, &mut self.scratch);
            let mut time = to_ts(p.pts);
            if let Some(prev) = self.last_video_time
                && time <= prev
            {
                time = prev + 1;
            }
            self.last_video_time = Some(time);
            infos.push(SampleInfo { time, duration: 0, size: self.scratch.len() as u32, key: p.key });
            data.extend_from_slice(&self.scratch);
            self.last_video = p.pts;
        }
        self.w.append(0, &infos, &data).map_err(io)?;

        // Audio: the separate tracks are copied; the mix is decoded, mixed and encoded.
        for (i, ring) in src.audio().into_iter().enumerate() {
            let (Some(ring), Some(a)) = (ring, self.audio[i].as_mut()) else { continue };
            let from = a.last.map(|l| l + 1).unwrap_or(t0);
            let packets = ring.lock().map_err(|_| "audio buffer unavailable")?.snapshot_from(from);
            let Some(first) = packets.first() else { continue };
            if a.last.is_none() {
                let offset = ((first.pts - t0).max(0) as i128 * SAMPLE_RATE as i128 / TICKS_PER_SEC as i128) as u64;
                self.w.track_mut(a.track).start_offset = offset;
                a.pos = offset;
            }
            let mut infos = Vec::with_capacity(packets.len());
            let mut data = Vec::new();
            let mut pcm = Vec::new();
            for p in &packets {
                infos.push(SampleInfo { time: a.count * AAC_FRAME as u64, duration: AAC_FRAME as u32, size: p.data.len() as u32, key: true });
                data.extend_from_slice(&p.data);
                a.count += 1;
                a.last = Some(p.pts);
                if let (Some(dec), Some(m)) = (a.dec.as_mut(), self.mixer.as_mut()) {
                    pcm.clear();
                    dec.decode(&p.data, &mut pcm)?;
                    m.put(a.slot, a.pos, &pcm);
                    a.pos += (pcm.len() / CHANNELS as usize) as u64;
                }
            }
            self.w.append(a.track, &infos, &data).map_err(io)?;
        }
        if let Some(m) = self.mixer.as_mut() {
            let mut out = Vec::new();
            m.drain(last, &mut out)?;
            let mut infos = Vec::with_capacity(out.len());
            for p in &out {
                infos.push(SampleInfo { time: m.count * AAC_FRAME as u64, duration: AAC_FRAME as u32, size: p.len() as u32, key: true });
                m.count += 1;
            }
            let track = m.track;
            self.w.append(track, &infos, &out.concat()).map_err(io)?;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<PathBuf, String> {
        let params = self.first_key.as_deref().map(|k| find_param_sets(self.codec, k));
        let config = params.and_then(|p| match self.codec {
            Codec::H264 => avcc(&p),
            Codec::Hevc => hvcc(&p),
            Codec::Av1 => p.sps.first().and_then(|s| av1c(s)),
        });
        let Some(config) = config else {
            let part = self.part.clone();
            drop(self);
            let _ = std::fs::remove_file(part);
            return Err("nothing was recorded".into());
        };
        if let TrackKind::Video { config: c, .. } = &mut self.w.track_mut(0).kind {
            *c = config;
        }
        let nominal = VIDEO_TIMESCALE / self.fps.max(1);
        let size = self.w.len();
        let file = self
            .w
            .finish(|t| if t == 0 { nominal } else { AAC_FRAME as u32 })
            .and_then(|w| w.into_inner().map_err(|e| e.into_error()))
            .map_err(|e| format!("cannot finish the recording: {e}"))?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        std::fs::rename(&self.part, &self.path).map_err(|e| e.to_string())?;
        log::info!("recording saved: {} ({:.1} MB)", self.path.display(), size as f64 / 1e6);
        Ok(self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixer_waits_for_every_source() {
        let Ok(enc) = AacEncoder::new(128) else { return }; // No codec on this machine.
        let mut m = Mixer { track: 1, enc, pcm: vec![Vec::new(); 3], ends: vec![0; 3], base: 0, gains: vec![100; 3], count: 0 };
        let mut out = Vec::new();
        m.put(0, 0, &vec![1000i16; 4800 * 2]);
        m.put(1, 0, &vec![1000i16; 4800 * 2]);
        m.drain(false, &mut out).unwrap();
        assert_eq!(m.base, 0, "voice chat has not arrived yet");
        m.put(2, 0, &vec![1000i16; 1200 * 2]);
        m.drain(false, &mut out).unwrap();
        assert_eq!(m.base, 1200);
    }

    #[test]
    fn mixer_waits_for_both_sources_then_flushes() {
        let Ok(enc) = AacEncoder::new(128) else { return }; // No codec on this machine.
        let mut m = Mixer { track: 1, enc, pcm: vec![Vec::new(), Vec::new()], ends: vec![0, 0], base: 0, gains: vec![100, 100], count: 0 };
        let mut out = Vec::new();
        m.put(0, 0, &vec![1000i16; 4800 * 2]);
        m.drain(false, &mut out).unwrap();
        assert_eq!(m.base, 0, "the mic has not arrived yet");
        m.put(1, 0, &vec![1000i16; 2400 * 2]);
        m.drain(false, &mut out).unwrap();
        assert_eq!(m.base, 2400);
        m.drain(true, &mut out).unwrap();
        assert_eq!(m.base, 4800);
        // Far behind: the lagging source counts as silent up to 1 s behind.
        m.put(0, 4800, &vec![0i16; 3 * 48000 * 2]);
        m.drain(false, &mut out).unwrap();
        assert_eq!(m.base, 4800 + 2 * 48000);
    }
}
