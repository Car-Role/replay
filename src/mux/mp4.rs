//! Minimal fast-start MP4 writer.
//!
//! All samples are known before writing, so `moov` goes in front of `mdat`
//! and the file plays while it streams. Samples are interleaved in 0.5 s
//! chunks. No B-frames are used, so decode time equals presentation time.

use std::io::{self, Seek, SeekFrom, Write};

use crate::config::Codec;

pub const VIDEO_TIMESCALE: u32 = 90_000;
const MOVIE_TIMESCALE: u32 = 1000;
const CHUNK_SECONDS: f64 = 0.5;

#[derive(Clone, Copy, Debug)]
pub struct SampleInfo {
    /// Decode/presentation time in track timescale units, starting at 0.
    pub time: u64,
    pub duration: u32,
    pub size: u32,
    pub key: bool,
}

pub enum TrackKind {
    Video { codec: Codec, width: u32, height: u32, config: Vec<u8> },
    Audio { sample_rate: u32, channels: u32, asc: Vec<u8>, bitrate: u32 },
}

pub struct Track {
    pub kind: TrackKind,
    pub timescale: u32,
    pub samples: Vec<SampleInfo>,
    /// Delay before the first sample, in track timescale units (written as an edit list).
    pub start_offset: u64,
    pub name: String,
    /// Disabled tracks are kept in the file but players skip them by default.
    pub enabled: bool,
}

impl Track {
    fn media_duration(&self) -> u64 {
        self.samples.last().map(|s| s.time + u64::from(s.duration)).unwrap_or(0)
    }

    fn movie_duration(&self) -> u64 {
        (self.media_duration() + self.start_offset) * u64::from(MOVIE_TIMESCALE) / u64::from(self.timescale)
    }

    fn handler(&self) -> &'static [u8; 4] {
        match self.kind {
            TrackKind::Video { .. } => b"vide",
            TrackKind::Audio { .. } => b"soun",
        }
    }
}

struct Chunk {
    track: usize,
    first_sample: usize,
    count: usize,
}

/// Writes the file. `fill(track, sample, buf)` must write exactly
/// `samples[sample].size` bytes for each sample, in the order requested.
pub fn write<W: Write>(
    out: &mut W,
    tracks: &[Track],
    mut fill: impl FnMut(usize, usize, &mut Vec<u8>) -> io::Result<()>,
) -> io::Result<u64> {
    let chunks = plan_chunks(tracks);
    let mdat_payload: u64 = tracks.iter().flat_map(|t| t.samples.iter()).map(|s| u64::from(s.size)).sum();
    let large = mdat_payload + 1024 * 1024 > u64::from(u32::MAX);

    let ftyp = ftyp(tracks);
    // The moov size does not depend on offset values, only on whether co64 is used.
    let probe = moov(tracks, &chunks, &vec![0; chunks.len()], large);
    let mdat_header = if large { 16 } else { 8 };
    let mut offset = (ftyp.len() + probe.len() + mdat_header) as u64;
    let mut offsets = Vec::with_capacity(chunks.len());
    for c in &chunks {
        offsets.push(offset);
        let t = &tracks[c.track];
        offset += t.samples[c.first_sample..c.first_sample + c.count].iter().map(|s| u64::from(s.size)).sum::<u64>();
    }
    let moov = moov(tracks, &chunks, &offsets, large);
    debug_assert_eq!(moov.len(), probe.len());

    out.write_all(&ftyp)?;
    out.write_all(&moov)?;
    if large {
        out.write_all(&1u32.to_be_bytes())?;
        out.write_all(b"mdat")?;
        out.write_all(&(mdat_payload + 16).to_be_bytes())?;
    } else {
        out.write_all(&((mdat_payload + 8) as u32).to_be_bytes())?;
        out.write_all(b"mdat")?;
    }
    let mut buf = Vec::with_capacity(1 << 20);
    for c in &chunks {
        for i in c.first_sample..c.first_sample + c.count {
            buf.clear();
            fill(c.track, i, &mut buf)?;
            if buf.len() != tracks[c.track].samples[i].size as usize {
                return Err(io::Error::other("sample size changed between passes"));
            }
            out.write_all(&buf)?;
        }
    }
    Ok(ftyp.len() as u64 + moov.len() as u64 + mdat_header as u64 + mdat_payload)
}

/// Writes a file whose samples arrive over time, for long recordings.
/// Samples go to `mdat` as they arrive and `moov` goes at the end, so the
/// file is playable only after `finish`.
pub struct StreamWriter<W: Write + Seek> {
    out: W,
    tracks: Vec<Track>,
    chunks: Vec<Chunk>,
    offsets: Vec<u64>,
    pos: u64,
    mdat_start: u64,
}

impl<W: Write + Seek> StreamWriter<W> {
    /// `tracks` give the kinds and names; their sample lists are ignored.
    pub fn new(mut out: W, mut tracks: Vec<Track>) -> io::Result<Self> {
        for t in &mut tracks {
            t.samples.clear();
        }
        let f = ftyp(&tracks);
        out.write_all(&f)?;
        let mdat_start = f.len() as u64;
        // 64-bit mdat size, set in `finish`.
        out.write_all(&1u32.to_be_bytes())?;
        out.write_all(b"mdat")?;
        out.write_all(&0u64.to_be_bytes())?;
        Ok(Self { out, tracks, chunks: Vec::new(), offsets: Vec::new(), pos: mdat_start + 16, mdat_start })
    }

    pub fn track_mut(&mut self, i: usize) -> &mut Track {
        &mut self.tracks[i]
    }

    /// Bytes written so far.
    pub fn len(&self) -> u64 {
        self.pos
    }

    /// Appends samples to one track as one chunk. `data` holds the samples'
    /// bytes back to back. Sample durations may be 0; `finish` sets them.
    pub fn append(&mut self, track: usize, samples: &[SampleInfo], data: &[u8]) -> io::Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        debug_assert_eq!(samples.iter().map(|s| s.size as usize).sum::<usize>(), data.len());
        let first = self.tracks[track].samples.len();
        self.out.write_all(data)?;
        self.chunks.push(Chunk { track, first_sample: first, count: samples.len() });
        self.offsets.push(self.pos);
        self.pos += data.len() as u64;
        self.tracks[track].samples.extend_from_slice(samples);
        Ok(())
    }

    /// Sets missing durations, drops empty tracks, writes `moov` and the
    /// `mdat` size. `last_duration[t]` is the duration of track t's last sample.
    pub fn finish(mut self, last_duration: impl Fn(usize) -> u32) -> io::Result<W> {
        for (ti, t) in self.tracks.iter_mut().enumerate() {
            let n = t.samples.len();
            for i in 0..n {
                if t.samples[i].duration == 0 {
                    t.samples[i].duration = match t.samples.get(i + 1) {
                        Some(next) => next.time.saturating_sub(t.samples[i].time).max(1) as u32,
                        None => last_duration(ti).max(1),
                    };
                }
            }
        }
        // Remove tracks without samples and renumber the chunks.
        let keep: Vec<usize> = (0..self.tracks.len()).filter(|&i| !self.tracks[i].samples.is_empty()).collect();
        let tracks: Vec<Track> = std::mem::take(&mut self.tracks)
            .into_iter()
            .enumerate()
            .filter(|(i, _)| keep.contains(i))
            .map(|(_, t)| t)
            .collect();
        for c in &mut self.chunks {
            c.track = keep.iter().position(|&k| k == c.track).unwrap_or(usize::MAX);
        }
        let moov = moov(&tracks, &self.chunks, &self.offsets, true);
        self.out.write_all(&moov)?;
        let mdat_size = self.pos - self.mdat_start;
        self.out.seek(SeekFrom::Start(self.mdat_start + 8))?;
        self.out.write_all(&mdat_size.to_be_bytes())?;
        self.out.seek(SeekFrom::End(0))?;
        self.out.flush()?;
        Ok(self.out)
    }
}

fn plan_chunks(tracks: &[Track]) -> Vec<Chunk> {
    let mut next = vec![0usize; tracks.len()];
    let mut chunks = Vec::new();
    let mut window = 1u64;
    loop {
        let mut any = false;
        let end_sec = window as f64 * CHUNK_SECONDS;
        for (ti, t) in tracks.iter().enumerate() {
            let limit = (end_sec * f64::from(t.timescale)) as u64;
            let start = next[ti];
            let mut i = start;
            while i < t.samples.len() && t.samples[i].time + t.start_offset < limit {
                i += 1;
            }
            if i > start {
                chunks.push(Chunk { track: ti, first_sample: start, count: i - start });
                next[ti] = i;
            }
            any |= next[ti] < t.samples.len();
        }
        if !any {
            return chunks;
        }
        window += 1;
    }
}

// ------------------------------------------------------------------- boxes

struct B {
    buf: Vec<u8>,
    stack: Vec<usize>,
}

impl B {
    fn new() -> Self {
        Self { buf: Vec::with_capacity(64 * 1024), stack: Vec::new() }
    }
    fn open(&mut self, kind: &[u8; 4]) {
        self.stack.push(self.buf.len());
        self.buf.extend_from_slice(&[0, 0, 0, 0]);
        self.buf.extend_from_slice(kind);
    }
    fn open_full(&mut self, kind: &[u8; 4], version: u8, flags: u32) {
        self.open(kind);
        self.u32((u32::from(version) << 24) | (flags & 0x00ff_ffff));
    }
    fn close(&mut self) {
        let start = self.stack.pop().expect("unbalanced box");
        let size = (self.buf.len() - start) as u32;
        self.buf[start..start + 4].copy_from_slice(&size.to_be_bytes());
    }
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    fn bytes(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }
    fn zeros(&mut self, n: usize) {
        self.buf.resize(self.buf.len() + n, 0);
    }
    fn matrix(&mut self) {
        for v in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
            self.u32(v);
        }
    }
}

fn ftyp(tracks: &[Track]) -> Vec<u8> {
    let mut b = B::new();
    b.open(b"ftyp");
    b.bytes(b"isom");
    b.u32(0x200);
    b.bytes(b"isom");
    b.bytes(b"iso2");
    b.bytes(b"mp41");
    for t in tracks {
        if let TrackKind::Video { codec, .. } = t.kind {
            b.bytes(match codec {
                Codec::H264 => b"avc1",
                Codec::Hevc => b"hvc1",
                Codec::Av1 => b"av01",
            });
        }
    }
    b.close();
    b.buf
}

fn moov(tracks: &[Track], chunks: &[Chunk], offsets: &[u64], large: bool) -> Vec<u8> {
    let mut b = B::new();
    let duration = tracks.iter().map(Track::movie_duration).max().unwrap_or(0);
    b.open(b"moov");
    b.open_full(b"mvhd", 1, 0);
    b.u64(0);
    b.u64(0);
    b.u32(MOVIE_TIMESCALE);
    b.u64(duration);
    b.u32(0x0001_0000); // rate 1.0
    b.u16(0x0100); // volume 1.0
    b.zeros(10);
    b.matrix();
    b.zeros(24);
    b.u32(tracks.len() as u32 + 1);
    b.close();

    let mut first_audio = true;
    for (ti, t) in tracks.iter().enumerate() {
        let is_audio = matches!(t.kind, TrackKind::Audio { .. });
        // Audio tracks share an alternate group so players pick one at a time.
        let group = if is_audio { 1 } else { 0 };
        let enabled = t.enabled && (!is_audio || first_audio);
        if is_audio && t.enabled {
            first_audio = false;
        }
        trak(&mut b, ti, t, chunks, offsets, large, duration, group, enabled);
    }
    b.close();
    b.buf
}

#[allow(clippy::too_many_arguments)]
fn trak(b: &mut B, ti: usize, t: &Track, chunks: &[Chunk], offsets: &[u64], large: bool, movie_dur: u64, group: u16, enabled: bool) {
    b.open(b"trak");
    let flags = if enabled { 0x3 } else { 0x2 };
    b.open_full(b"tkhd", 1, flags);
    b.u64(0);
    b.u64(0);
    b.u32(ti as u32 + 1);
    b.u32(0);
    b.u64(t.movie_duration().min(movie_dur));
    b.zeros(8);
    b.u16(0); // layer
    b.u16(group);
    match t.kind {
        TrackKind::Audio { .. } => b.u16(0x0100),
        TrackKind::Video { .. } => b.u16(0),
    }
    b.u16(0);
    b.matrix();
    match t.kind {
        TrackKind::Video { width, height, .. } => {
            b.u32(width << 16);
            b.u32(height << 16);
        }
        TrackKind::Audio { .. } => {
            b.u32(0);
            b.u32(0);
        }
    }
    b.close();

    if t.start_offset > 0 {
        b.open(b"edts");
        b.open_full(b"elst", 1, 0);
        b.u32(2);
        let empty = t.start_offset * u64::from(MOVIE_TIMESCALE) / u64::from(t.timescale);
        b.u64(empty);
        b.u64(u64::MAX); // media_time = -1: empty edit
        b.u32(0x0001_0000);
        b.u64(t.media_duration() * u64::from(MOVIE_TIMESCALE) / u64::from(t.timescale));
        b.u64(0);
        b.u32(0x0001_0000);
        b.close();
        b.close();
    }

    b.open(b"mdia");
    b.open_full(b"mdhd", 1, 0);
    b.u64(0);
    b.u64(0);
    b.u32(t.timescale);
    b.u64(t.media_duration());
    b.u16(0x55c4); // "und"
    b.u16(0);
    b.close();

    b.open_full(b"hdlr", 0, 0);
    b.u32(0);
    b.bytes(t.handler());
    b.zeros(12);
    b.bytes(t.name.as_bytes());
    b.u8(0);
    b.close();

    b.open(b"minf");
    match t.kind {
        TrackKind::Video { .. } => {
            b.open_full(b"vmhd", 0, 1);
            b.zeros(8);
            b.close();
        }
        TrackKind::Audio { .. } => {
            b.open_full(b"smhd", 0, 0);
            b.zeros(4);
            b.close();
        }
    }
    b.open(b"dinf");
    b.open_full(b"dref", 0, 0);
    b.u32(1);
    b.open_full(b"url ", 0, 1);
    b.close();
    b.close();
    b.close();

    b.open(b"stbl");
    stsd(b, t);
    stts(b, t);
    if matches!(t.kind, TrackKind::Video { .. }) && t.samples.iter().any(|s| !s.key) {
        b.open_full(b"stss", 0, 0);
        let keys: Vec<u32> = t.samples.iter().enumerate().filter(|(_, s)| s.key).map(|(i, _)| i as u32 + 1).collect();
        b.u32(keys.len() as u32);
        for k in keys {
            b.u32(k);
        }
        b.close();
    }

    // stsc: run-length coded samples-per-chunk for this track's chunks.
    let mine: Vec<(usize, &Chunk)> = chunks.iter().enumerate().filter(|(_, c)| c.track == ti).collect();
    b.open_full(b"stsc", 0, 0);
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for (n, (_, c)) in mine.iter().enumerate() {
        if runs.last().map(|r| r.1) != Some(c.count as u32) {
            runs.push((n as u32 + 1, c.count as u32));
        }
    }
    b.u32(runs.len() as u32);
    for (first, count) in runs {
        b.u32(first);
        b.u32(count);
        b.u32(1);
    }
    b.close();

    b.open_full(b"stsz", 0, 0);
    b.u32(0);
    b.u32(t.samples.len() as u32);
    for s in &t.samples {
        b.u32(s.size);
    }
    b.close();

    if large {
        b.open_full(b"co64", 0, 0);
        b.u32(mine.len() as u32);
        for (ci, _) in &mine {
            b.u64(offsets[*ci]);
        }
    } else {
        b.open_full(b"stco", 0, 0);
        b.u32(mine.len() as u32);
        for (ci, _) in &mine {
            b.u32(offsets[*ci] as u32);
        }
    }
    b.close();

    b.close(); // stbl
    b.close(); // minf
    b.close(); // mdia
    b.close(); // trak
}

fn stts(b: &mut B, t: &Track) {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for s in &t.samples {
        match runs.last_mut() {
            Some((n, d)) if *d == s.duration => *n += 1,
            _ => runs.push((1, s.duration)),
        }
    }
    b.open_full(b"stts", 0, 0);
    b.u32(runs.len() as u32);
    for (n, d) in runs {
        b.u32(n);
        b.u32(d);
    }
    b.close();
}

fn stsd(b: &mut B, t: &Track) {
    b.open_full(b"stsd", 0, 0);
    b.u32(1);
    match &t.kind {
        TrackKind::Video { codec, width, height, config } => {
            let (entry, cfg_box) = match codec {
                Codec::H264 => (b"avc1", b"avcC"),
                Codec::Hevc => (b"hvc1", b"hvcC"),
                Codec::Av1 => (b"av01", b"av1C"),
            };
            b.open(entry);
            b.zeros(6);
            b.u16(1);
            b.zeros(16);
            b.u16(*width as u16);
            b.u16(*height as u16);
            b.u32(0x0048_0000);
            b.u32(0x0048_0000);
            b.u32(0);
            b.u16(1);
            let mut name = [0u8; 32];
            let label = b"Replay";
            name[0] = label.len() as u8;
            name[1..=label.len()].copy_from_slice(label);
            b.bytes(&name);
            b.u16(0x0018);
            b.u16(0xffff);
            b.open(cfg_box);
            b.bytes(config);
            b.close();
            // BT.709 limited range, which is what the video processor outputs.
            b.open(b"colr");
            b.bytes(b"nclx");
            b.u16(1);
            b.u16(1);
            b.u16(1);
            b.u8(0);
            b.close();
            b.open(b"pasp");
            b.u32(1);
            b.u32(1);
            b.close();
            b.close();
        }
        TrackKind::Audio { sample_rate, channels, asc, bitrate } => {
            b.open(b"mp4a");
            b.zeros(6);
            b.u16(1);
            b.zeros(8);
            b.u16(*channels as u16);
            b.u16(16);
            b.zeros(4);
            b.u32(sample_rate << 16);
            b.open_full(b"esds", 0, 0);
            // ES_Descriptor
            b.u8(0x03);
            b.u8((3 + 2 + 13 + 2 + asc.len() + 3) as u8);
            b.u16(0);
            b.u8(0);
            // DecoderConfigDescriptor
            b.u8(0x04);
            b.u8((13 + 2 + asc.len()) as u8);
            b.u8(0x40); // MPEG-4 audio
            b.u8(0x15); // audio stream
            b.bytes(&[0, 0x18, 0]); // buffer size
            b.u32(*bitrate);
            b.u32(*bitrate);
            // DecoderSpecificInfo
            b.u8(0x05);
            b.u8(asc.len() as u8);
            b.bytes(asc);
            // SLConfigDescriptor
            b.u8(0x06);
            b.u8(1);
            b.u8(0x02);
            b.close();
            b.close();
        }
    }
    b.close();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_box<'a>(mut data: &'a [u8], path: &[&[u8; 4]]) -> Option<&'a [u8]> {
        let (first, rest) = path.split_first()?;
        while data.len() >= 8 {
            let size = u32::from_be_bytes(data[..4].try_into().unwrap()) as usize;
            let kind = &data[4..8];
            if kind == *first {
                let body = &data[8..size];
                return if rest.is_empty() { Some(body) } else { find_box(body, rest) };
            }
            data = &data[size..];
        }
        None
    }

    fn video_track(n: usize) -> Track {
        let samples = (0..n)
            .map(|i| SampleInfo { time: i as u64 * 1500, duration: 1500, size: 10, key: i % 30 == 0 })
            .collect();
        Track {
            kind: TrackKind::Video { codec: Codec::H264, width: 1920, height: 1080, config: vec![1, 2, 3] },
            timescale: VIDEO_TIMESCALE,
            samples,
            start_offset: 0,
            name: "Video".into(),
            enabled: true,
        }
    }

    fn audio_track(n: usize, offset: u64) -> Track {
        let samples = (0..n).map(|i| SampleInfo { time: i as u64 * 1024, duration: 1024, size: 7, key: true }).collect();
        Track {
            kind: TrackKind::Audio { sample_rate: 48000, channels: 2, asc: vec![0x11, 0x90], bitrate: 192_000 },
            timescale: 48000,
            samples,
            start_offset: offset,
            name: "Game".into(),
            enabled: true,
        }
    }

    #[test]
    fn writes_fast_start_layout_with_valid_offsets() {
        let tracks = [video_track(120), audio_track(94, 0), audio_track(94, 4800)];
        let mut out = Vec::new();
        let total = write(&mut out, &tracks, |t, i, buf| {
            let s = tracks[t].samples[i];
            buf.resize(s.size as usize, (t as u8) << 4 | (i % 16) as u8);
            Ok(())
        })
        .unwrap();
        assert_eq!(total as usize, out.len());
        assert_eq!(&out[4..8], b"ftyp");
        let ftyp_len = u32::from_be_bytes(out[..4].try_into().unwrap()) as usize;
        assert_eq!(&out[ftyp_len + 4..ftyp_len + 8], b"moov");

        // The first stco offset of track 2 must point at its first sample.
        let moov = find_box(&out, &[b"moov"]).unwrap();
        let mut traks = Vec::new();
        let mut d = moov;
        while d.len() >= 8 {
            let size = u32::from_be_bytes(d[..4].try_into().unwrap()) as usize;
            if &d[4..8] == b"trak" {
                traks.push(&d[8..size]);
            }
            d = &d[size..];
        }
        assert_eq!(traks.len(), 3);
        for (ti, trak) in traks.iter().enumerate() {
            let stco = find_box(trak, &[b"mdia", b"minf", b"stbl", b"stco"]).unwrap();
            let first = u32::from_be_bytes(stco[8..12].try_into().unwrap()) as usize;
            assert_eq!(out[first], (ti as u8) << 4, "track {ti}");
        }
        assert!(find_box(traks[2], &[b"edts", b"elst"]).is_some());
        assert!(find_box(traks[0], &[b"mdia", b"minf", b"stbl", b"stss"]).is_some());
    }

    #[test]
    fn stream_writer_puts_moov_last_with_valid_offsets() {
        let tracks = vec![video_track(0), audio_track(0, 0), audio_track(0, 0)];
        let mut w = StreamWriter::new(std::io::Cursor::new(Vec::new()), tracks).unwrap();
        // Two rounds of chunks; the third track never gets samples.
        for round in 0..2u64 {
            let v: Vec<SampleInfo> = (0..30).map(|i| SampleInfo { time: (round * 30 + i) * 1500, duration: 0, size: 10, key: i == 0 }).collect();
            w.append(0, &v, &vec![0xA0 | round as u8; 300]).unwrap();
            let a: Vec<SampleInfo> = (0..24).map(|i| SampleInfo { time: (round * 24 + i) * 1024, duration: 1024, size: 4, key: true }).collect();
            w.append(1, &a, &vec![0xB0 | round as u8; 96]).unwrap();
        }
        let out = w.finish(|_| 1500).unwrap().into_inner();
        let ftyp_len = u32::from_be_bytes(out[..4].try_into().unwrap()) as usize;
        assert_eq!(&out[ftyp_len + 4..ftyp_len + 8], b"mdat");
        let mdat_size = u64::from_be_bytes(out[ftyp_len + 8..ftyp_len + 16].try_into().unwrap()) as usize;
        assert_eq!(&out[ftyp_len + mdat_size + 4..ftyp_len + mdat_size + 8], b"moov");
        let moov = &out[ftyp_len + mdat_size + 8..];
        let mut traks = Vec::new();
        let mut d = moov;
        while d.len() >= 8 {
            let size = u32::from_be_bytes(d[..4].try_into().unwrap()) as usize;
            if &d[4..8] == b"trak" {
                traks.push(&d[8..size]);
            }
            d = &d[size..];
        }
        assert_eq!(traks.len(), 2, "the empty track is dropped");
        for (ti, (trak, tag)) in traks.iter().zip([0xA0u8, 0xB0]).enumerate() {
            let co64 = find_box(trak, &[b"mdia", b"minf", b"stbl", b"co64"]).unwrap();
            assert_eq!(u32::from_be_bytes(co64[4..8].try_into().unwrap()), 2, "track {ti}");
            for c in 0..2usize {
                let off = u64::from_be_bytes(co64[8 + c * 8..16 + c * 8].try_into().unwrap()) as usize;
                assert_eq!(out[off], tag | c as u8, "track {ti} chunk {c}");
            }
        }
    }

    #[test]
    fn chunks_cover_every_sample_once() {
        let tracks = [video_track(301), audio_track(250, 0)];
        let chunks = plan_chunks(&tracks);
        for (ti, t) in tracks.iter().enumerate() {
            let total: usize = chunks.iter().filter(|c| c.track == ti).map(|c| c.count).sum();
            assert_eq!(total, t.samples.len());
        }
    }
}
