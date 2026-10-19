//! MP4 reader for the editor: sample tables, codec configs and track names.
//! It reads what `mp4.rs` writes (and most simple MP4 files without B-frames).

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use crate::config::Codec;

#[derive(Clone, Debug, PartialEq)]
pub enum TrackKindIn {
    Video { codec: Codec, width: u32, height: u32, config: Vec<u8> },
    Audio { sample_rate: u32, channels: u32, asc: Vec<u8> },
}

#[derive(Clone, Copy, Debug)]
pub struct SampleIn {
    pub offset: u64,
    pub size: u32,
    /// In track timescale units, from the media start.
    pub time: u64,
    pub duration: u32,
    pub key: bool,
}

#[derive(Clone, Debug)]
pub struct TrackIn {
    pub kind: TrackKindIn,
    pub name: String,
    pub timescale: u32,
    pub samples: Vec<SampleIn>,
    /// Empty edit before the first sample, in track timescale units.
    pub start_offset: u64,
    /// Whether players show this track by default.
    #[allow(dead_code)]
    pub enabled: bool,
}

impl TrackIn {
    /// Presentation time of sample `i`, in seconds.
    pub fn seconds(&self, i: usize) -> f64 {
        (self.samples[i].time + self.start_offset) as f64 / f64::from(self.timescale)
    }

    pub fn duration_seconds(&self) -> f64 {
        self.samples
            .last()
            .map(|s| (s.time + u64::from(s.duration) + self.start_offset) as f64 / f64::from(self.timescale))
            .unwrap_or(0.0)
    }
}

#[derive(Clone, Debug)]
pub struct Mp4In {
    pub tracks: Vec<TrackIn>,
}

impl Mp4In {
    pub fn video(&self) -> Option<&TrackIn> {
        self.tracks.iter().find(|t| matches!(t.kind, TrackKindIn::Video { .. }))
    }

    pub fn audio_named(&self, name: &str) -> Option<&TrackIn> {
        self.tracks.iter().find(|t| matches!(t.kind, TrackKindIn::Audio { .. }) && t.name == name)
    }

    pub fn audio(&self) -> impl Iterator<Item = &TrackIn> {
        self.tracks.iter().filter(|t| matches!(t.kind, TrackKindIn::Audio { .. }))
    }

    pub fn duration(&self) -> f64 {
        self.tracks.iter().map(TrackIn::duration_seconds).fold(0.0, f64::max)
    }
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

struct Cur<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn new(d: &'a [u8]) -> Self {
        Self { d, p: 0 }
    }
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.p + n > self.d.len() {
            return Err(bad("truncated box"));
        }
        let s = &self.d[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().expect("2 bytes")))
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().expect("4 bytes")))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().expect("8 bytes")))
    }
    fn skip(&mut self, n: usize) -> io::Result<()> {
        self.take(n).map(|_| ())
    }
}

/// Iterates child boxes as (type, body).
fn boxes(data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut p = 0usize;
    std::iter::from_fn(move || {
        if p + 8 > data.len() {
            return None;
        }
        let size32 = u32::from_be_bytes(data[p..p + 4].try_into().ok()?) as u64;
        let kind: [u8; 4] = data[p + 4..p + 8].try_into().ok()?;
        let (header, size) = match size32 {
            0 => (8, (data.len() - p) as u64),
            1 => (16, u64::from_be_bytes(data.get(p + 8..p + 16)?.try_into().ok()?)),
            s => (8, s),
        };
        let end = p.checked_add(size as usize)?;
        if size < header as u64 || end > data.len() {
            return None;
        }
        let body = &data[p + header..end];
        p = end;
        Some((kind, body))
    })
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data).find(|(k, _)| k == kind).map(|(_, b)| b)
}

fn path<'a>(data: &'a [u8], kinds: &[&[u8; 4]]) -> Option<&'a [u8]> {
    kinds.iter().try_fold(data, |d, k| child(d, k))
}

pub fn read(file: &Path) -> io::Result<Mp4In> {
    let mut f = File::open(file)?;
    let len = f.metadata()?.len();
    // Find moov without reading mdat.
    let mut pos = 0u64;
    let moov = loop {
        if pos + 8 > len {
            return Err(bad("no moov box"));
        }
        f.seek(SeekFrom::Start(pos))?;
        let mut h = [0u8; 16];
        f.read_exact(&mut h[..8])?;
        let mut size = u64::from(u32::from_be_bytes(h[..4].try_into().expect("4")));
        let mut header = 8;
        if size == 1 {
            f.read_exact(&mut h[8..16])?;
            size = u64::from_be_bytes(h[8..16].try_into().expect("8"));
            header = 16;
        } else if size == 0 {
            size = len - pos;
        }
        if size < header || pos + size > len {
            return Err(bad("bad box size"));
        }
        if &h[4..8] == b"moov" {
            if size > 256 << 20 {
                return Err(bad("moov too large"));
            }
            let mut body = vec![0u8; (size - header) as usize];
            f.read_exact(&mut body)?;
            break body;
        }
        pos += size;
    };
    parse_moov(&moov)
}

fn parse_moov(moov: &[u8]) -> io::Result<Mp4In> {
    let mvhd = child(moov, b"mvhd").ok_or_else(|| bad("no mvhd"))?;
    let movie_scale = {
        let mut c = Cur::new(mvhd);
        let v = c.u8()?;
        c.skip(3)?;
        c.skip(if v == 1 { 16 } else { 8 })?;
        c.u32()?
    };
    let mut tracks = Vec::new();
    for (k, trak) in boxes(moov) {
        if &k != b"trak" {
            continue;
        }
        if let Some(t) = parse_trak(trak, movie_scale)? {
            tracks.push(t);
        }
    }
    Ok(Mp4In { tracks })
}

fn parse_trak(trak: &[u8], movie_scale: u32) -> io::Result<Option<TrackIn>> {
    let tkhd = child(trak, b"tkhd").ok_or_else(|| bad("no tkhd"))?;
    let enabled = tkhd.get(3).is_some_and(|f| f & 1 != 0);
    let mdia = child(trak, b"mdia").ok_or_else(|| bad("no mdia"))?;
    let mdhd = child(mdia, b"mdhd").ok_or_else(|| bad("no mdhd"))?;
    let timescale = {
        let mut c = Cur::new(mdhd);
        let v = c.u8()?;
        c.skip(3)?;
        c.skip(if v == 1 { 16 } else { 8 })?;
        c.u32()?.max(1)
    };
    let hdlr = child(mdia, b"hdlr").ok_or_else(|| bad("no hdlr"))?;
    let handler: [u8; 4] = hdlr.get(8..12).ok_or_else(|| bad("short hdlr"))?.try_into().expect("4");
    let name = hdlr.get(24..).map(|n| String::from_utf8_lossy(n).trim_end_matches('\0').trim().to_string()).unwrap_or_default();
    let stbl = path(mdia, &[b"minf", b"stbl"]).ok_or_else(|| bad("no stbl"))?;
    let stsd = child(stbl, b"stsd").ok_or_else(|| bad("no stsd"))?;
    let kind = match (&handler, parse_stsd(stsd)?) {
        (b"vide" | b"soun", Some(k)) => k,
        _ => return Ok(None),
    };

    // Empty edit (start delay).
    let mut start_offset = 0u64;
    if let Some(elst) = path(trak, &[b"edts", b"elst"]) {
        let mut c = Cur::new(elst);
        let v = c.u8()?;
        c.skip(3)?;
        let n = c.u32()?;
        if n > 0 {
            let (dur, media_time) = if v == 1 { (c.u64()?, c.u64()? as i64) } else { (u64::from(c.u32()?), i64::from(c.u32()? as i32)) };
            if media_time == -1 {
                start_offset = dur * u64::from(timescale) / u64::from(movie_scale.max(1));
            }
        }
    }

    let mut times: Vec<(u64, u32)> = Vec::new();
    {
        let stts = child(stbl, b"stts").ok_or_else(|| bad("no stts"))?;
        let mut c = Cur::new(stts);
        c.skip(4)?;
        let n = c.u32()?;
        let mut t = 0u64;
        for _ in 0..n {
            let count = c.u32()?;
            let delta = c.u32()?;
            for _ in 0..count {
                times.push((t, delta));
                t += u64::from(delta);
            }
        }
    }
    let sizes: Vec<u32> = {
        let stsz = child(stbl, b"stsz").ok_or_else(|| bad("no stsz"))?;
        let mut c = Cur::new(stsz);
        c.skip(4)?;
        let fixed = c.u32()?;
        let n = c.u32()? as usize;
        if fixed != 0 { vec![fixed; n] } else { (0..n).map(|_| c.u32()).collect::<io::Result<_>>()? }
    };
    let chunk_offsets: Vec<u64> = if let Some(stco) = child(stbl, b"stco") {
        let mut c = Cur::new(stco);
        c.skip(4)?;
        let n = c.u32()?;
        (0..n).map(|_| c.u32().map(u64::from)).collect::<io::Result<_>>()?
    } else if let Some(co64) = child(stbl, b"co64") {
        let mut c = Cur::new(co64);
        c.skip(4)?;
        let n = c.u32()?;
        (0..n).map(|_| c.u64()).collect::<io::Result<_>>()?
    } else {
        return Err(bad("no chunk offsets"));
    };
    let stsc: Vec<(u32, u32)> = {
        let b = child(stbl, b"stsc").ok_or_else(|| bad("no stsc"))?;
        let mut c = Cur::new(b);
        c.skip(4)?;
        let n = c.u32()?;
        (0..n)
            .map(|_| {
                let first = c.u32()?;
                let per = c.u32()?;
                c.u32()?;
                Ok((first, per))
            })
            .collect::<io::Result<_>>()?
    };
    let keys: Option<Vec<u32>> = child(stbl, b"stss")
        .map(|b| {
            let mut c = Cur::new(b);
            c.skip(4)?;
            let n = c.u32()?;
            (0..n).map(|_| c.u32()).collect::<io::Result<Vec<u32>>>()
        })
        .transpose()?;

    // Expand chunk table into per-sample offsets.
    let mut samples = Vec::with_capacity(sizes.len());
    let mut si = 0usize;
    for (ci, &chunk_off) in chunk_offsets.iter().enumerate() {
        let chunk_no = ci as u32 + 1;
        let per = stsc.iter().rev().find(|(first, _)| *first <= chunk_no).map(|(_, p)| *p).unwrap_or(0);
        let mut off = chunk_off;
        for _ in 0..per {
            let (Some(&size), Some(&(time, duration))) = (sizes.get(si), times.get(si)) else { break };
            samples.push(SampleIn { offset: off, size, time, duration, key: true });
            off += u64::from(size);
            si += 1;
        }
    }
    if let Some(keys) = keys {
        for s in &mut samples {
            s.key = false;
        }
        for k in keys {
            if let Some(s) = samples.get_mut(k as usize - 1) {
                s.key = true;
            }
        }
    }
    Ok(Some(TrackIn { kind, name, timescale, samples, start_offset, enabled }))
}

fn parse_stsd(stsd: &[u8]) -> io::Result<Option<TrackKindIn>> {
    // fullbox header (4) + entry count (4), then the first sample entry box.
    let entries = stsd.get(8..).ok_or_else(|| bad("short stsd"))?;
    let Some((kind, entry)) = boxes(entries).next() else { return Ok(None) };
    let codec = match &kind {
        b"avc1" | b"avc3" => Some(Codec::H264),
        b"hvc1" | b"hev1" => Some(Codec::Hevc),
        b"av01" => Some(Codec::Av1),
        _ => None,
    };
    if let Some(codec) = codec {
        let mut c = Cur::new(entry);
        c.skip(24)?;
        let width = u32::from(c.u16()?);
        let height = u32::from(c.u16()?);
        let children = entry.get(78..).ok_or_else(|| bad("short visual entry"))?;
        let cfg_kind: &[u8; 4] = match codec {
            Codec::H264 => b"avcC",
            Codec::Hevc => b"hvcC",
            Codec::Av1 => b"av1C",
        };
        let config = child(children, cfg_kind).ok_or_else(|| bad("no codec config"))?.to_vec();
        return Ok(Some(TrackKindIn::Video { codec, width, height, config }));
    }
    if &kind == b"mp4a" {
        let mut c = Cur::new(entry);
        c.skip(16)?;
        let channels = u32::from(c.u16()?);
        c.skip(6)?;
        let sample_rate = c.u32()? >> 16;
        let esds = entry.get(28..).and_then(|ch| child(ch, b"esds")).ok_or_else(|| bad("no esds"))?;
        let asc = find_descriptor(&esds[4..], 0x05).unwrap_or_default();
        return Ok(Some(TrackKindIn::Audio { sample_rate, channels, asc }));
    }
    Ok(None)
}

/// Finds an MPEG-4 descriptor by tag, descending into ES and DecoderConfig.
fn find_descriptor(mut d: &[u8], want: u8) -> Option<Vec<u8>> {
    while d.len() >= 2 {
        let tag = d[0];
        let mut len = 0usize;
        let mut i = 1;
        loop {
            let b = *d.get(i)?;
            len = (len << 7) | usize::from(b & 0x7f);
            i += 1;
            if b & 0x80 == 0 || i > 4 {
                break;
            }
        }
        let body = d.get(i..i + len)?;
        if tag == want {
            return Some(body.to_vec());
        }
        let inner = match tag {
            0x03 => body.get(3..),  // ES_ID + flags (no optional fields in our files)
            0x04 => body.get(13..), // DecoderConfigDescriptor fixed part
            _ => None,
        };
        if let Some(inner) = inner
            && let Some(found) = find_descriptor(inner, want)
        {
            return Some(found);
        }
        d = &d[i + len..];
    }
    None
}

pub fn read_sample(f: &mut File, s: &SampleIn, out: &mut Vec<u8>) -> io::Result<()> {
    out.resize(s.size as usize, 0);
    f.seek(SeekFrom::Start(s.offset))?;
    f.read_exact(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::mp4::{self, SampleInfo, Track, TrackKind, VIDEO_TIMESCALE};

    #[test]
    fn reads_back_what_the_writer_wrote() {
        let video = Track {
            kind: TrackKind::Video { codec: Codec::Av1, width: 1920, height: 1080, config: vec![0x81, 8, 0x0c, 0] },
            timescale: VIDEO_TIMESCALE,
            samples: (0..90).map(|i| SampleInfo { time: i * 1500, duration: 1500, size: 5 + (i % 7) as u32, key: i % 30 == 0 }).collect(),
            start_offset: 0,
            name: "Video".into(),
            enabled: true,
        };
        let audio = Track {
            kind: TrackKind::Audio { sample_rate: 48000, channels: 2, asc: vec![0x11, 0x90], bitrate: 192_000 },
            timescale: 48000,
            samples: (0..70).map(|i| SampleInfo { time: i * 1024, duration: 1024, size: 9, key: true }).collect(),
            start_offset: 2400,
            name: "Game".into(),
            enabled: true,
        };
        let tracks = [video, audio];
        let dir = std::env::temp_dir().join(format!("replay-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("rt.mp4");
        let mut f = std::fs::File::create(&p).unwrap();
        mp4::write(&mut f, &tracks, |t, i, buf| {
            buf.resize(tracks[t].samples[i].size as usize, (t * 100 + i) as u8);
            Ok(())
        })
        .unwrap();
        drop(f);

        let m = read(&p).unwrap();
        assert_eq!(m.tracks.len(), 2);
        let v = m.video().unwrap();
        assert_eq!(v.kind, TrackKindIn::Video { codec: Codec::Av1, width: 1920, height: 1080, config: vec![0x81, 8, 0x0c, 0] });
        assert_eq!(v.samples.len(), 90);
        assert_eq!(v.samples.iter().filter(|s| s.key).count(), 3);
        let a = m.audio_named("Game").unwrap();
        assert_eq!(a.kind, TrackKindIn::Audio { sample_rate: 48000, channels: 2, asc: vec![0x11, 0x90] });
        assert_eq!(a.start_offset, 2400);
        let mut file = std::fs::File::open(&p).unwrap();
        let mut buf = Vec::new();
        for (t, track) in m.tracks.iter().enumerate() {
            for (i, s) in track.samples.iter().enumerate() {
                read_sample(&mut file, s, &mut buf).unwrap();
                assert!(buf.iter().all(|&b| b == (t * 100 + i) as u8), "track {t} sample {i}");
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
