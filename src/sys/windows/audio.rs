//! Audio capture (WASAPI) and AAC coding (Media Foundation).
//!
//! Game audio uses process loopback, so only the game's own sound is
//! recorded (not Discord or music). The microphone is a normal capture
//! stream. Both are requested as 48 kHz stereo 16-bit; Windows converts.
//! A timeline fills gaps with silence so the track always stays in sync
//! with the video clock.

use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::Com::{
    BLOB, CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
    IAgileObject, IAgileObject_Impl,
};
use windows::Win32::System::Threading::{AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{GUID, HSTRING, IUnknown, Interface, Result, implement, w};

use crate::ring::{PacketRing, TICKS_PER_SEC, Ticks};

pub use crate::media::{AAC_FRAME, CHANNELS, SAMPLE_RATE};

#[derive(Clone, Debug)]
pub enum Source {
    /// Process loopback of a process and its children.
    Process(u32),
    /// Capture endpoint id; empty means the default communications mic.
    Microphone(String),
    /// Process loopback of the running voice chat app (see `VOICE_CHAT_APPS`).
    /// Silent while none runs; follows the app when it restarts.
    VoiceChat,
}

/// Voice chat apps recorded on the "Voice chat" track. Their voice runs in a
/// child process, so the top process is captured with its whole tree.
pub const VOICE_CHAT_APPS: &[&str] = &["discord.exe", "discordptb.exe", "discordcanary.exe"];

/// The top process of a running voice chat app (one whose parent is not
/// the same app), or None.
fn voice_chat_pid() -> Option<u32> {
    use std::collections::HashMap;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
    let mut procs: HashMap<u32, (u32, String)> = HashMap::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let name = crate::util::from_wide(&e.szExeFile).to_ascii_lowercase();
            procs.insert(e.th32ProcessID, (e.th32ParentProcessID, name));
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    let is_app = |n: &str| VOICE_CHAT_APPS.contains(&n);
    procs
        .iter()
        .filter(|(_, (parent, name))| is_app(name) && !procs.get(parent).is_some_and(|(_, pn)| pn == name))
        .map(|(pid, _)| *pid)
        .min()
}

fn pcm_format() -> WAVEFORMATEX {
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM as u16,
        nChannels: CHANNELS as u16,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * CHANNELS * 2,
        nBlockAlign: (CHANNELS * 2) as u16,
        wBitsPerSample: 16,
        cbSize: 0,
    }
}

// ------------------------------------------------------------------ AAC

fn find_mft(category: GUID, input: GUID, output: GUID) -> Result<IMFTransform> {
    unsafe {
        let i = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Audio, guidSubtype: input };
        let o = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Audio, guidSubtype: output };
        let mut acts: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut n = 0;
        MFTEnumEx(category, MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER, Some(&i), Some(&o), &mut acts, &mut n)?;
        let mut found = None;
        for k in 0..n as usize {
            if let Some(a) = (*acts.add(k)).take()
                && found.is_none()
            {
                found = a.ActivateObject::<IMFTransform>().ok();
            }
        }
        CoTaskMemFree(Some(acts as _));
        found.ok_or_else(|| windows::core::Error::new(MF_E_TOPO_CODEC_NOT_FOUND, "no AAC transform"))
    }
}

fn audio_type(subtype: GUID) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        t.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, CHANNELS)?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        Ok(t)
    }
}

fn pcm_type() -> Result<IMFMediaType> {
    let t = audio_type(MFAudioFormat_PCM)?;
    unsafe {
        t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, CHANNELS * 2)?;
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, SAMPLE_RATE * CHANNELS * 2)?;
    }
    Ok(t)
}

fn memory_sample(bytes: &[u8], time: Option<i64>) -> Result<IMFSample> {
    unsafe {
        let buf = MFCreateMemoryBuffer(bytes.len().max(1) as u32)?;
        let mut p = std::ptr::null_mut();
        buf.Lock(&mut p, None, None)?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        buf.Unlock()?;
        buf.SetCurrentLength(bytes.len() as u32)?;
        let s = MFCreateSample()?;
        s.AddBuffer(&buf)?;
        if let Some(t) = time {
            s.SetSampleTime(t)?;
        }
        Ok(s)
    }
}

/// Pulls every pending output from a synchronous transform.
fn drain(mft: &IMFTransform, out_size: u32, mut f: impl FnMut(&[u8])) -> Result<()> {
    unsafe {
        loop {
            let s = MFCreateSample()?;
            s.AddBuffer(&MFCreateMemoryBuffer(out_size)?)?;
            let mut b = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(Some(s)),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0;
            let r = mft.ProcessOutput(0, &mut b, &mut status);
            let [mut b] = b;
            let sample = ManuallyDrop::take(&mut b.pSample);
            ManuallyDrop::drop(&mut b.pEvents);
            match r {
                Ok(()) => {
                    if let Some(sample) = sample {
                        let mb = sample.ConvertToContiguousBuffer()?;
                        let mut p = std::ptr::null_mut();
                        let mut len = 0;
                        mb.Lock(&mut p, None, Some(&mut len))?;
                        if len > 0 {
                            f(std::slice::from_raw_parts(p, len as usize));
                        }
                        mb.Unlock()?;
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }
}

pub struct AacEncoder {
    mft: IMFTransform,
    out_size: u32,
    fed: u64,
}

impl AacEncoder {
    pub fn new(bitrate_kbps: u32) -> Result<Self> {
        let mft = find_mft(MFT_CATEGORY_AUDIO_ENCODER, MFAudioFormat_PCM, MFAudioFormat_AAC)?;
        unsafe {
            mft.SetInputType(0, &pcm_type()?, 0)?;
            let out = audio_type(MFAudioFormat_AAC)?;
            out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bitrate_kbps * 1000 / 8)?;
            out.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
            out.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)?;
            mft.SetOutputType(0, &out, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            let out_size = mft.GetOutputStreamInfo(0)?.cbSize.max(8192);
            Ok(Self { mft, out_size, fed: 0 })
        }
    }

    /// Encodes interleaved 16-bit PCM. Calls `f` once per AAC frame.
    pub fn encode(&mut self, pcm: &[i16], f: impl FnMut(&[u8])) -> Result<()> {
        if pcm.is_empty() {
            return Ok(());
        }
        let bytes = unsafe { std::slice::from_raw_parts(pcm.as_ptr().cast::<u8>(), pcm.len() * 2) };
        let frames = (pcm.len() / CHANNELS as usize) as u64;
        let sample = memory_sample(bytes, Some(frames_to_ticks(self.fed)))?;
        unsafe { sample.SetSampleDuration(frames_to_ticks(frames))? };
        self.fed += frames;
        unsafe { self.mft.ProcessInput(0, &sample, 0)? };
        drain(&self.mft, self.out_size, f)
    }

    /// Flushes buffered samples at the end of a file.
    pub fn finish(&mut self, f: impl FnMut(&[u8])) -> Result<()> {
        unsafe { self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)? };
        drain(&self.mft, self.out_size, f)
    }
}

pub struct AacDecoder {
    mft: IMFTransform,
    out_size: u32,
}

impl AacDecoder {
    pub fn new(asc: &[u8]) -> Result<Self> {
        let mft = find_mft(MFT_CATEGORY_AUDIO_DECODER, MFAudioFormat_AAC, MFAudioFormat_PCM)?;
        unsafe {
            let t = audio_type(MFAudioFormat_AAC)?;
            t.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
            // HEAACWAVEINFO tail (12 bytes) followed by the AudioSpecificConfig.
            let mut user = vec![0u8; 12];
            user[2] = 0xfe;
            user.extend_from_slice(asc);
            t.SetBlob(&MF_MT_USER_DATA, &user)?;
            mft.SetInputType(0, &t, 0)?;
            mft.SetOutputType(0, &pcm_type()?, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            let out_size = mft.GetOutputStreamInfo(0)?.cbSize.max(AAC_FRAME as u32 * CHANNELS * 2 * 2);
            Ok(Self { mft, out_size })
        }
    }

    /// Decodes one raw AAC frame, appending PCM to `out`.
    pub fn decode(&mut self, frame: &[u8], out: &mut Vec<i16>) -> Result<()> {
        unsafe { self.mft.ProcessInput(0, &memory_sample(frame, Some(0))?, 0)? };
        drain(&self.mft, self.out_size, |b| {
            out.extend(b.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c)));
        })
    }
}

// ------------------------------------------------------------- timeline

const GAP_TOLERANCE: Ticks = TICKS_PER_SEC * 40 / 1000;
const SILENCE_AFTER: Ticks = TICKS_PER_SEC * 120 / 1000;
const SILENCE_SLACK: Ticks = TICKS_PER_SEC * 60 / 1000;
const MAX_FILL: Ticks = TICKS_PER_SEC * 5;
const POLL: Duration = Duration::from_millis(40);

/// Keeps an audio stream continuous on the QPC clock and encodes it.
pub struct Timeline {
    base: Option<Ticks>,
    fed: u64,     // PCM frames given to the encoder
    emitted: u64, // AAC frames produced
    enc: AacEncoder,
    ring: Arc<Mutex<PacketRing>>,
    silence: Vec<i16>,
    /// Faults inside a running stream, logged once a minute for bug reports.
    faults: Faults,
}

/// Gaps filled with silence and overlaps dropped while sound was flowing.
/// Silence after the source stops (a game going quiet) is not counted.
#[derive(Default)]
struct Faults {
    label: &'static str,
    gaps: u32,
    gap_ms: f64,
    drops: u32,
    drop_ms: f64,
    /// Silence was filled because nothing arrived (the source went quiet).
    quiet: bool,
    since: Option<std::time::Instant>,
}

impl Faults {
    fn report(&mut self) {
        let since = *self.since.get_or_insert_with(std::time::Instant::now);
        if since.elapsed() < Duration::from_secs(60) {
            return;
        }
        if self.gaps > 0 || self.drops > 0 {
            log::warn!(
                "{}: {} gaps filled with silence ({:.0} ms), {} overlaps dropped ({:.0} ms) in the last minute",
                self.label,
                self.gaps,
                self.gap_ms,
                self.drops,
                self.drop_ms
            );
        }
        *self = Self { label: self.label, quiet: self.quiet, since: Some(std::time::Instant::now()), ..Self::default() };
    }
}

fn frames_to_ticks(f: u64) -> Ticks {
    (f as i128 * TICKS_PER_SEC as i128 / SAMPLE_RATE as i128) as Ticks
}

fn ticks_to_frames(t: Ticks) -> u64 {
    (t.max(0) as i128 * SAMPLE_RATE as i128 / TICKS_PER_SEC as i128) as u64
}

impl Timeline {
    pub fn new(bitrate_kbps: u32, ring: Arc<Mutex<PacketRing>>, label: &'static str) -> Result<Self> {
        let faults = Faults { label, ..Faults::default() };
        Ok(Self { base: None, fed: 0, emitted: 0, enc: AacEncoder::new(bitrate_kbps)?, ring, silence: Vec::new(), faults })
    }

    fn next_time(&self) -> Option<Ticks> {
        self.base.map(|b| b + frames_to_ticks(self.fed))
    }

    fn feed(&mut self, pcm: &[i16]) -> Result<()> {
        let base = self.base.expect("base set before feed");
        self.fed += (pcm.len() / CHANNELS as usize) as u64;
        let ring = &self.ring;
        let emitted = &mut self.emitted;
        self.enc.encode(pcm, |aac| {
            let pts = base + frames_to_ticks(*emitted * AAC_FRAME as u64);
            *emitted += 1;
            if let Ok(mut r) = ring.lock() {
                r.push(pts, true, aac);
            }
        })
    }

    fn feed_silence(&mut self, frames: u64) -> Result<()> {
        let n = frames as usize * CHANNELS as usize;
        if self.silence.len() < n {
            self.silence.resize(n, 0);
        }
        let s = std::mem::take(&mut self.silence);
        let r = self.feed(&s[..n]);
        self.silence = s;
        r
    }

    /// Adds captured PCM that started at QPC time `at`.
    pub fn push(&mut self, at: Ticks, pcm: &[i16]) -> Result<()> {
        let Some(next) = self.next_time() else {
            self.base = Some(at);
            return self.feed(pcm);
        };
        let drift = at - next;
        let flowing = !std::mem::take(&mut self.faults.quiet);
        self.faults.report();
        if drift > GAP_TOLERANCE {
            if flowing {
                self.faults.gaps += 1;
                self.faults.gap_ms += drift as f64 / (TICKS_PER_SEC as f64 / 1000.0);
            }
            self.feed_silence(ticks_to_frames(drift.min(MAX_FILL)))?;
        } else if drift < -GAP_TOLERANCE {
            // We are ahead (silence was filled while this data was in flight): drop the overlap.
            if flowing {
                self.faults.drops += 1;
                self.faults.drop_ms += -drift as f64 / (TICKS_PER_SEC as f64 / 1000.0);
            }
            let skip = ticks_to_frames(-drift) as usize * CHANNELS as usize;
            if skip >= pcm.len() {
                return Ok(());
            }
            return self.feed(&pcm[skip..]);
        }
        self.feed(pcm)
    }

    /// Called when no data arrived for a while; keeps the track running.
    pub fn idle(&mut self, now: Ticks) -> Result<()> {
        let Some(next) = self.next_time() else { return Ok(()) };
        if now - next > SILENCE_AFTER {
            self.faults.quiet = true;
            self.feed_silence(ticks_to_frames((now - SILENCE_SLACK - next).min(MAX_FILL)))?;
        }
        Ok(())
    }
}

// --------------------------------------------------------------- capture

#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct ActivateHandler {
    tx: Mutex<Option<mpsc::Sender<Result<IAudioClient>>>>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivateHandler_Impl {
    fn ActivateCompleted(&self, op: windows::core::Ref<IActivateAudioInterfaceAsyncOperation>) -> Result<()> {
        let result = (|| unsafe {
            let mut hr = Default::default();
            let mut unk: Option<IUnknown> = None;
            op.ok()?.GetActivateResult(&mut hr, &mut unk)?;
            hr.ok()?;
            unk.ok_or_else(|| windows::core::Error::from(E_NOINTERFACE_))?.cast::<IAudioClient>()
        })();
        if let Some(tx) = self.tx.lock().ok().and_then(|mut t| t.take()) {
            let _ = tx.send(result);
        }
        Ok(())
    }
}

impl IAgileObject_Impl for ActivateHandler_Impl {}

const E_NOINTERFACE_: windows::core::HRESULT = windows::Win32::Foundation::E_NOINTERFACE;

fn process_loopback_client(pid: u32) -> Result<IAudioClient> {
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    // The PROPVARIANT only borrows `params`; it must never be cleared.
    let pv = ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: (&raw mut params).cast(),
                    },
                },
            }),
        },
    });
    let (tx, rx) = mpsc::channel();
    let handler: IActivateAudioInterfaceCompletionHandler = ActivateHandler { tx: Mutex::new(Some(tx)) }.into();
    unsafe {
        ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(&*pv), &handler)?;
    }
    rx.recv_timeout(Duration::from_secs(5))
        .map_err(|_| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "audio activation timed out"))?
}

fn microphone_client(id: &str) -> Result<IAudioClient> {
    unsafe {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let dev = if id.is_empty() {
            en.GetDefaultAudioEndpoint(eCapture, eCommunications)?
        } else {
            en.GetDevice(&HSTRING::from(id))?
        };
        dev.Activate::<IAudioClient>(CLSCTX_ALL, None)
    }
}

struct Stream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: HANDLE,
    /// The captured app, for sources that must follow it when it closes.
    watch: Option<HANDLE>,
}

impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = windows::Win32::Foundation::CloseHandle(self.event);
            if let Some(h) = self.watch {
                let _ = windows::Win32::Foundation::CloseHandle(h);
            }
        }
    }
}

fn open(source: &Source) -> Result<Stream> {
    let mut watch = None;
    let (client, loopback) = match source {
        Source::Process(pid) => (process_loopback_client(*pid)?, true),
        Source::Microphone(id) => (microphone_client(id)?, false),
        Source::VoiceChat => {
            use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
            let pid = voice_chat_pid().ok_or_else(|| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "no voice chat app is running"))?;
            watch = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }.ok();
            (process_loopback_client(pid)?, true)
        }
    };
    let mut flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    if loopback {
        flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
    }
    unsafe {
        let fmt = pcm_format();
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 2_000_000, 0, &fmt, None)?;
        let event = CreateEventW(None, false, false, None)?;
        client.SetEventHandle(event)?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        Ok(Stream { client, capture, event, watch })
    }
}

/// Runs a capture loop until `stop` is set. Reopens the device after errors
/// (for example when a headset is unplugged).
pub fn run(source: Source, bitrate_kbps: u32, ring: Arc<Mutex<PacketRing>>, stop: Arc<AtomicBool>) {
    let label = match &source {
        Source::Process(_) => "replay game audio",
        Source::Microphone(_) => "replay mic",
        Source::VoiceChat => "replay voice chat",
    };
    crate::util::set_thread(label, windows::Win32::System::Threading::THREAD_PRIORITY_NORMAL);
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let mut task = 0;
    let mmcss = unsafe { AvSetMmThreadCharacteristicsW(w!("Audio"), &mut task) }.ok();

    let mut timeline = match Timeline::new(bitrate_kbps, ring, label) {
        Ok(t) => t,
        Err(e) => {
            log::error!("AAC encoder unavailable: {e}");
            return;
        }
    };
    let mut pcm: Vec<i16> = Vec::with_capacity(SAMPLE_RATE as usize);
    let mut failures = 0u32;
    while !stop.load(Ordering::Acquire) {
        let stream = match open(&source) {
            Ok(s) => {
                log::info!("audio started: {source:?}");
                failures = 0;
                s
            }
            Err(e) => {
                failures += 1;
                if failures == 1 || failures.is_multiple_of(30) {
                    log::warn!("audio open failed ({source:?}): {e}");
                }
                // Keep the track continuous while the device is missing.
                let _ = timeline.idle(crate::util::now_ticks());
                std::thread::sleep(Duration::from_millis(1000));
                continue;
            }
        };
        if let Err(e) = capture_loop(&stream, &mut timeline, &mut pcm, &stop) {
            log::warn!("audio stream ended ({source:?}): {e}");
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    if let Some(h) = mmcss {
        unsafe {
            let _ = AvRevertMmThreadCharacteristics(h);
        }
    }
    unsafe { CoUninitialize() };
}

fn capture_loop(s: &Stream, timeline: &mut Timeline, pcm: &mut Vec<i16>, stop: &AtomicBool) -> Result<()> {
    let mut polls = 0u32;
    let mut clock_logged = false;
    while !stop.load(Ordering::Acquire) {
        // About every 2 s: a followed app that closed ends the stream, so it
        // is found again (and the track stays silent meanwhile).
        polls = polls.wrapping_add(1);
        if let Some(h) = s.watch
            && polls % 50 == 0
            && unsafe { windows::Win32::System::Threading::WaitForSingleObject(h, 0) } == windows::Win32::Foundation::WAIT_OBJECT_0
        {
            return Err(windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "the app closed"));
        }
        // Poll instead of waking on every 10 ms engine period: the 200 ms
        // buffer leaves plenty of room, and this cuts wakeups by 4x.
        std::thread::sleep(POLL);
        let mut got = false;
        loop {
            let next = unsafe { s.capture.GetNextPacketSize()? };
            if next == 0 {
                break;
            }
            let mut data = std::ptr::null_mut();
            let mut frames = 0;
            let mut flags = 0;
            let mut qpc = 0u64;
            unsafe { s.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))? };
            got = true;
            let n = frames as usize * CHANNELS as usize;
            pcm.clear();
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                pcm.resize(n, 0);
            } else {
                pcm.extend_from_slice(unsafe { std::slice::from_raw_parts(data.cast::<i16>(), n) });
            }
            unsafe { s.capture.ReleaseBuffer(frames)? };
            // The QPC position is already in 100 ns units.
            if !clock_logged {
                clock_logged = true;
                log::info!("{}: {}", timeline.faults.label, if qpc != 0 { "device timestamps" } else { "no device timestamps, using arrival time" });
            }
            let at = if qpc != 0 { qpc as Ticks } else { crate::util::now_ticks() };
            timeline.push(at, pcm)?;
        }
        if !got {
            timeline.idle(crate::util::now_ticks())?;
        }
    }
    Ok(())
}

/// Microphones that are active now, as (endpoint id, friendly name).
pub fn list_microphones() -> Vec<(String, String)> {
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::System::Com::STGM_READ;
    let mut out = Vec::new();
    unsafe {
        let Ok(en) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else {
            return out;
        };
        let Ok(col) = en.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) else { return out };
        let n = col.GetCount().unwrap_or(0);
        for i in 0..n {
            let Ok(dev) = col.Item(i) else { continue };
            let Ok(id) = dev.GetId() else { continue };
            let id_s = id.to_string().unwrap_or_default();
            CoTaskMemFree(Some(id.0 as _));
            let name = dev
                .OpenPropertyStore(STGM_READ)
                .and_then(|ps| ps.GetValue(&PKEY_Device_FriendlyName))
                .map(|v| v.to_string())
                .unwrap_or_else(|_| id_s.clone());
            out.push((id_s, name));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_frame_conversion() {
        assert_eq!(ticks_to_frames(TICKS_PER_SEC), 48000);
        assert_eq!(frames_to_ticks(48000), TICKS_PER_SEC);
        assert_eq!(frames_to_ticks(1024), 213_333);
    }
}

#[cfg(test)]
mod mf_tests {
    use super::*;

    fn mf() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).unwrap();
        }
    }

    #[test]
    fn aac_encodes_small_chunks_and_decodes() {
        mf();
        let mut enc = AacEncoder::new(192).unwrap();
        let mut frames = Vec::new();
        let chunk: Vec<i16> = (0..480 * 2).map(|i| ((i as f32 * 0.05).sin() * 8000.0) as i16).collect();
        for n in 0..200 {
            enc.encode(&chunk, |p| frames.push(p.to_vec())).unwrap_or_else(|e| panic!("chunk {n}: {e}"));
        }
        enc.finish(|p| frames.push(p.to_vec())).unwrap();
        // 200 x 480 = 96000 samples = 93.75 AAC frames (+ priming).
        assert!(frames.len() >= 93, "{} frames", frames.len());
        let mut dec = AacDecoder::new(&crate::mux::bitstream::aac_asc(48000, 2)).unwrap();
        let mut pcm = Vec::new();
        for f in &frames {
            dec.decode(f, &mut pcm).unwrap();
        }
        assert!(pcm.len() >= 90 * 1024 * 2, "{} samples", pcm.len());
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    #[test]
    #[ignore]
    fn aac_speed() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).unwrap();
        }
        let secs = 60;
        let pcm: Vec<i16> = (0..48000 * 2 * secs).map(|i| ((i as f32 * 0.031).sin() * 9000.0) as i16).collect();
        for chunk in [480usize, 1024 * 4, 48000] {
            let mut enc = AacEncoder::new(192).unwrap();
            let t = std::time::Instant::now();
            let mut n = 0;
            for c in pcm.chunks(chunk * 2) {
                enc.encode(c, |_| n += 1).unwrap();
            }
            let e = t.elapsed().as_secs_f64();
            println!("chunk {chunk:>6}: {secs} s audio in {:.3} s = {:.0}x realtime, {:.2}% of a core live", e, secs as f64 / e, e / secs as f64 * 100.0);
        }
    }
}
