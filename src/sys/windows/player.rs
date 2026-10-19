//! Preview playback for the library window.
//!
//! Video: a Media Foundation source reader decodes to RGB at preview size on
//! a background thread. Audio: the clip's tracks are decoded once, then a
//! WASAPI thread plays them with live volume control. The audio position is
//! the master clock.

use std::mem::ManuallyDrop;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::Win32::Media::Audio::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Variant::VT_I8;
use windows::core::{GUID, HSTRING};

use crate::audio::{CHANNELS, SAMPLE_RATE};

use crate::media::{AudioTracks, Frame};

enum Cmd {
    Seek(f64, u64),
    Stop,
}

/// Background video decoder.
pub struct VideoDecoder {
    cmd: Sender<Cmd>,
    pub frames: Receiver<Frame>,
    thread: Option<JoinHandle<()>>,
}

const MAX_PREVIEW_WIDTH: u32 = 1280;

pub(crate) fn propvariant_i64(v: i64) -> ManuallyDrop<PROPVARIANT> {
    ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_I8,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 { hVal: v },
            }),
        },
    })
}

impl VideoDecoder {
    pub fn open(path: &Path, wake: impl Fn() + Send + 'static) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (frame_tx, frame_rx) = mpsc::sync_channel(3);
        let path = path.to_path_buf();
        let thread = std::thread::Builder::new()
            .name("preview video".into())
            .spawn(move || {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                }
                if let Err(e) = decode_loop(&path, &cmd_rx, &frame_tx, &wake) {
                    log::warn!("preview of {} failed: {e}", path.display());
                }
                unsafe { CoUninitialize() };
            })
            .ok();
        Self { cmd: cmd_tx, frames: frame_rx, thread }
    }

    pub fn seek(&self, seconds: f64, generation: u64) {
        let _ = self.cmd.send(Cmd::Seek(seconds, generation));
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        let _ = self.cmd.send(Cmd::Stop);
        // Unblock a pending send.
        while self.frames.try_recv().is_ok() {}
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

const FIRST_VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

fn decode_loop(path: &Path, cmd: &Receiver<Cmd>, out: &SyncSender<Frame>, wake: &dyn Fn()) -> Result<(), String> {
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 2).map_err(|e| e.to_string())?;
        let attrs = attrs.expect("attributes");
        attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1).map_err(|e| e.to_string())?;
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attrs).map_err(|e| e.to_string())?;
        let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
        reader.SetStreamSelection(FIRST_VIDEO, true).map_err(|e| e.to_string())?;
        let native = reader.GetNativeMediaType(FIRST_VIDEO, 0).map_err(|e| e.to_string())?;
        let size = native.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or((1920u64 << 32) | 1080);
        let (sw, sh) = ((size >> 32) as u32, size as u32);
        let w = sw.min(MAX_PREVIEW_WIDTH) & !1;
        let h = ((u64::from(sh) * u64::from(w) / u64::from(sw.max(1))) as u32).max(2) & !1;
        let t = MFCreateMediaType().map_err(|e| e.to_string())?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| e.to_string())?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(|e| e.to_string())?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(w) << 32) | u64::from(h)).map_err(|e| e.to_string())?;
        reader.SetCurrentMediaType(FIRST_VIDEO, None, &t).map_err(|e| format!("this video cannot be decoded here: {e}"))?;

        let mut generation = 0u64;
        let mut skip_until = 0.0f64;
        let mut ended = false;
        loop {
            // Handle commands; block while the stream has ended.
            let next = if ended { cmd.recv().map_err(|_| RecvTimeoutError::Disconnected) } else { cmd.try_recv().map_err(|_| RecvTimeoutError::Timeout) };
            match next {
                Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
                Ok(Cmd::Seek(s, g)) => {
                    generation = g;
                    skip_until = s - 0.001;
                    let pv = propvariant_i64((s.max(0.0) * 1e7) as i64);
                    let _ = reader.SetCurrentPosition(&GUID::zeroed(), &*pv);
                    ended = false;
                    continue;
                }
                Err(_) => {}
            }
            let mut flags = 0;
            let mut time = 0;
            let mut sample = None;
            if reader.ReadSample(FIRST_VIDEO, 0, None, Some(&mut flags), Some(&mut time), Some(&mut sample)).is_err() {
                ended = true;
                continue;
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                ended = true;
                continue;
            }
            let Some(sample) = sample else { continue };
            let pts = time as f64 / 1e7;
            if pts < skip_until {
                continue;
            }
            let Ok(buf) = sample.ConvertToContiguousBuffer() else { continue };
            let mut p = std::ptr::null_mut();
            let mut len = 0;
            if buf.Lock(&mut p, None, Some(&mut len)).is_err() {
                continue;
            }
            let src = std::slice::from_raw_parts(p, len as usize);
            let (wu, hu) = (w as usize, h as usize);
            let mut rgba = vec![0u8; wu * hu * 4];
            if src.len() >= rgba.len() {
                for (d, s) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
                    d[0] = s[2];
                    d[1] = s[1];
                    d[2] = s[0];
                    d[3] = 255;
                }
            }
            let _ = buf.Unlock();
            let mut frame = Frame { generation, pts, width: wu, height: hu, rgba };
            // Wait for room, but stay responsive to seeks.
            loop {
                match out.try_send(frame) {
                    Ok(()) => {
                        wake();
                        break;
                    }
                    Err(TrySendError::Disconnected(_)) => return Ok(()),
                    Err(TrySendError::Full(f)) => match cmd.recv_timeout(Duration::from_millis(8)) {
                        Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
                        Ok(Cmd::Seek(s, g)) => {
                            generation = g;
                            skip_until = s - 0.001;
                            let pv = propvariant_i64((s.max(0.0) * 1e7) as i64);
                            let _ = reader.SetCurrentPosition(&GUID::zeroed(), &*pv);
                            break;
                        }
                        Err(RecvTimeoutError::Timeout) => frame = f,
                    },
                }
            }
        }
    }
}

/// Decodes a music file (MP3, AAC, WAV, FLAC, WMA) to 48 kHz stereo PCM.
pub fn decode_audio_file(path: &Path) -> Result<Vec<i16>, String> {
    const FIRST_AUDIO: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
    unsafe {
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
        reader.SetStreamSelection(FIRST_AUDIO, true).map_err(|_| "the file has no audio".to_string())?;
        let want = |rate: Option<u32>| -> windows::core::Result<IMFMediaType> {
            let t = MFCreateMediaType()?;
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
            t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            if let Some(r) = rate {
                t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, r)?;
                t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, CHANNELS)?;
                t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, CHANNELS * 2)?;
                t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, r * CHANNELS * 2)?;
            }
            Ok(t)
        };
        // Ask for 48 kHz stereo; if the reader cannot convert, take its PCM and convert here.
        let exact = want(Some(SAMPLE_RATE)).and_then(|t| reader.SetCurrentMediaType(FIRST_AUDIO, None, &t)).is_ok();
        if !exact {
            let t = want(None).map_err(|e| e.to_string())?;
            reader.SetCurrentMediaType(FIRST_AUDIO, None, &t).map_err(|e| format!("this audio cannot be decoded: {e}"))?;
        }
        let cur = reader.GetCurrentMediaType(FIRST_AUDIO).map_err(|e| e.to_string())?;
        let rate = cur.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(SAMPLE_RATE);
        let chans = cur.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(CHANNELS).max(1);
        let mut pcm: Vec<i16> = Vec::new();
        loop {
            let mut flags = 0u32;
            let mut sample = None;
            reader.ReadSample(FIRST_AUDIO, 0, None, Some(&mut flags), None, Some(&mut sample)).map_err(|e| e.to_string())?;
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
            let Some(sample) = sample else { continue };
            let buf = sample.ConvertToContiguousBuffer().map_err(|e| e.to_string())?;
            let mut p = std::ptr::null_mut();
            let mut len = 0;
            buf.Lock(&mut p, None, Some(&mut len)).map_err(|e| e.to_string())?;
            let s = std::slice::from_raw_parts(p.cast::<i16>(), len as usize / 2);
            pcm.extend_from_slice(s);
            let _ = buf.Unlock();
            if pcm.len() > 48_000 * 2 * 60 * 60 {
                return Err("the music file is longer than an hour".into());
            }
        }
        Ok(to_48k_stereo(&pcm, rate, chans))
    }
}

/// Converts interleaved PCM to 48 kHz stereo (linear resampling).
pub fn to_48k_stereo(pcm: &[i16], rate: u32, chans: u32) -> Vec<i16> {
    let chans = chans.max(1) as usize;
    let frames = pcm.len() / chans;
    let stereo = |i: usize| -> (i32, i32) {
        let f = &pcm[i * chans..i * chans + chans];
        if chans == 1 { (i32::from(f[0]), i32::from(f[0])) } else { (i32::from(f[0]), i32::from(f[1])) }
    };
    if rate == SAMPLE_RATE {
        let mut out = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let (l, r) = stereo(i);
            out.push(l as i16);
            out.push(r as i16);
        }
        return out;
    }
    let out_frames = (frames as u64 * u64::from(SAMPLE_RATE) / u64::from(rate.max(1))) as usize;
    let mut out = Vec::with_capacity(out_frames * 2);
    for j in 0..out_frames {
        let pos = j as f64 * f64::from(rate) / f64::from(SAMPLE_RATE);
        let i = pos as usize;
        let frac = pos - i as f64;
        let (l0, r0) = stereo(i.min(frames - 1));
        let (l1, r1) = stereo((i + 1).min(frames - 1));
        out.push((f64::from(l0) + (f64::from(l1 - l0)) * frac) as i16);
        out.push((f64::from(r0) + (f64::from(r1 - r0)) * frac) as i16);
    }
    out
}

// ------------------------------------------------------------------ audio

struct Shared {
    playing: AtomicBool,
    /// Next frame to write.
    write_pos: AtomicU64,
    /// Frame now heard (write position minus device padding).
    heard_pos: AtomicU64,
    seek_to: Mutex<Option<u64>>,
    game_gain: AtomicU32,
    mic_gain: AtomicU32,
    stop: AtomicBool,
}

pub struct AudioOut {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    pub frames: u64,
}

impl AudioOut {
    pub fn start(tracks: AudioTracks) -> Self {
        let frames = tracks
            .game
            .as_ref()
            .or(tracks.mic.as_ref())
            .map(|v| (v.len() / CHANNELS as usize) as u64)
            .unwrap_or(0);
        let shared = Arc::new(Shared {
            playing: AtomicBool::new(false),
            write_pos: AtomicU64::new(0),
            heard_pos: AtomicU64::new(0),
            seek_to: Mutex::new(None),
            game_gain: AtomicU32::new(100),
            mic_gain: AtomicU32::new(100),
            stop: AtomicBool::new(false),
        });
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name("preview audio".into())
            .spawn(move || {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                }
                if let Err(e) = render_loop(&s, &tracks, frames) {
                    log::warn!("preview audio stopped: {e}");
                }
                unsafe { CoUninitialize() };
            })
            .ok();
        Self { shared, thread, frames }
    }

    pub fn set_playing(&self, on: bool) {
        self.shared.playing.store(on, Ordering::Release);
    }

    pub fn seek(&self, seconds: f64) {
        let f = (seconds.max(0.0) * f64::from(SAMPLE_RATE)) as u64;
        self.shared.heard_pos.store(f, Ordering::Release);
        if let Ok(mut s) = self.shared.seek_to.lock() {
            *s = Some(f);
        }
    }

    pub fn set_gains(&self, game: u32, mic: u32) {
        self.shared.game_gain.store(game, Ordering::Relaxed);
        self.shared.mic_gain.store(mic, Ordering::Relaxed);
    }

    pub fn position(&self) -> f64 {
        self.shared.heard_pos.load(Ordering::Acquire) as f64 / f64::from(SAMPLE_RATE)
    }
}

impl Drop for AudioOut {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn render_loop(s: &Shared, tracks: &AudioTracks, total: u64) -> windows::core::Result<()> {
    unsafe {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
        let fmt = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: CHANNELS as u16,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * CHANNELS * 2,
            nBlockAlign: (CHANNELS * 2) as u16,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            1_000_000,
            0,
            &fmt,
            None,
        )?;
        let size = client.GetBufferSize()?;
        let render: IAudioRenderClient = client.GetService()?;
        let mut running = false;
        while !s.stop.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(10));
            if let Some(f) = s.seek_to.lock().ok().and_then(|mut g| g.take()) {
                if running {
                    client.Stop()?;
                    running = false;
                }
                client.Reset()?;
                s.write_pos.store(f, Ordering::Release);
                s.heard_pos.store(f, Ordering::Release);
            }
            let playing = s.playing.load(Ordering::Acquire);
            if !playing {
                if running {
                    client.Stop()?;
                    running = false;
                }
                continue;
            }
            let padding = client.GetCurrentPadding()?;
            let mut pos = s.write_pos.load(Ordering::Acquire);
            let free = size - padding;
            let n = free.min((total.saturating_sub(pos)) as u32);
            if n > 0 {
                let data = render.GetBuffer(n)?;
                let out = std::slice::from_raw_parts_mut(data.cast::<i16>(), n as usize * CHANNELS as usize);
                let gg = s.game_gain.load(Ordering::Relaxed) as i32;
                let mg = s.mic_gain.load(Ordering::Relaxed) as i32;
                let start = pos as usize * CHANNELS as usize;
                for (k, o) in out.iter_mut().enumerate() {
                    let i = start + k;
                    let g = tracks.game.as_ref().and_then(|v| v.get(i)).copied().unwrap_or(0) as i32 * gg;
                    let m = tracks.mic.as_ref().and_then(|v| v.get(i)).copied().unwrap_or(0) as i32 * mg;
                    *o = ((g + m) / 100).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                }
                render.ReleaseBuffer(n, 0)?;
                pos += u64::from(n);
                s.write_pos.store(pos, Ordering::Release);
            }
            if !running {
                client.Start()?;
                running = true;
            }
            let heard = pos.saturating_sub(u64::from(client.GetCurrentPadding()?));
            s.heard_pos.store(heard, Ordering::Release);
        }
        let _ = client.Stop();
        Ok(())
    }
}
