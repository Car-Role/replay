//! Elementary-stream helpers: Annex B NAL units (H.264/HEVC) and AV1 OBUs.
//!
//! Encoders emit Annex B (start-code delimited) or low-overhead OBU streams.
//! MP4 wants length-prefixed NAL units and size-delimited OBUs without
//! temporal delimiters, plus a codec configuration record. This module does
//! those conversions.

use crate::config::Codec;

/// Splits an Annex B stream into NAL units (without start codes).
pub fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut start: Option<usize> = None;
    while i + 2 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(s) = start {
                // Trailing zero bytes belong to the next 4-byte start code.
                let mut end = i;
                while end > s && data[end - 1] == 0 {
                    end -= 1;
                }
                if end > s {
                    out.push(&data[s..end]);
                }
            }
            i += 3;
            start = Some(i);
        } else {
            i += 1;
        }
    }
    match start {
        Some(s) if s < data.len() => out.push(&data[s..]),
        None if !data.is_empty() => out.push(data), // Already a bare NAL.
        _ => {}
    }
    out
}

/// Removes emulation-prevention bytes (00 00 03 -> 00 00).
pub fn unescape_rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

fn h264_type(nal: &[u8]) -> u8 {
    nal[0] & 0x1f
}

fn hevc_type(nal: &[u8]) -> u8 {
    (nal[0] >> 1) & 0x3f
}

const H264_SPS: u8 = 7;
const H264_PPS: u8 = 8;
const H264_AUD: u8 = 9;
const HEVC_VPS: u8 = 32;
const HEVC_SPS: u8 = 33;
const HEVC_PPS: u8 = 34;
const HEVC_AUD: u8 = 35;

/// Parameter sets found in a keyframe, in the order MP4 config records need.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct ParamSets {
    pub vps: Vec<Vec<u8>>,
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
}

impl ParamSets {
    pub fn is_complete(&self, codec: Codec) -> bool {
        match codec {
            Codec::H264 => !self.sps.is_empty() && !self.pps.is_empty(),
            Codec::Hevc => !self.vps.is_empty() && !self.sps.is_empty() && !self.pps.is_empty(),
            Codec::Av1 => !self.sps.is_empty(),
        }
    }
}

/// Collects parameter sets from an Annex B access unit (H.264/HEVC) or an
/// OBU temporal unit (AV1; the sequence header goes into `sps`).
pub fn find_param_sets(codec: Codec, data: &[u8]) -> ParamSets {
    let mut ps = ParamSets::default();
    match codec {
        Codec::H264 => {
            for nal in split_annexb(data) {
                match h264_type(nal) {
                    H264_SPS => ps.sps.push(nal.to_vec()),
                    H264_PPS => ps.pps.push(nal.to_vec()),
                    _ => {}
                }
            }
        }
        Codec::Hevc => {
            for nal in split_annexb(data) {
                if nal.len() < 2 {
                    continue;
                }
                match hevc_type(nal) {
                    HEVC_VPS => ps.vps.push(nal.to_vec()),
                    HEVC_SPS => ps.sps.push(nal.to_vec()),
                    HEVC_PPS => ps.pps.push(nal.to_vec()),
                    _ => {}
                }
            }
        }
        Codec::Av1 => {
            if let Ok(obus) = parse_obus(data) {
                for o in obus {
                    if o.kind == OBU_SEQUENCE_HEADER {
                        ps.sps.push(o.to_sized_bytes());
                        break;
                    }
                }
            }
        }
    }
    ps
}

/// Converts one encoder access unit into an MP4 sample.
/// Drops parameter sets, AUDs and AV1 temporal delimiters, which live in the
/// sample entry or carry no picture data.
pub fn to_mp4_sample(codec: Codec, data: &[u8], out: &mut Vec<u8>) {
    out.clear();
    match codec {
        Codec::H264 | Codec::Hevc => {
            for nal in split_annexb(data) {
                if nal.is_empty() {
                    continue;
                }
                let drop = match codec {
                    Codec::H264 => matches!(h264_type(nal), H264_SPS | H264_PPS | H264_AUD),
                    _ => nal.len() < 2 || matches!(hevc_type(nal), HEVC_VPS | HEVC_SPS | HEVC_PPS | HEVC_AUD),
                };
                if !drop {
                    out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                    out.extend_from_slice(nal);
                }
            }
        }
        Codec::Av1 => match parse_obus(data) {
            Ok(obus) => {
                for o in obus {
                    if o.kind != OBU_TEMPORAL_DELIMITER && o.kind != OBU_PADDING {
                        o.write_sized(out);
                    }
                }
            }
            Err(_) => out.extend_from_slice(data),
        },
    }
}

// ---------------------------------------------------------------- avcC / hvcC

pub fn avcc(ps: &ParamSets) -> Option<Vec<u8>> {
    let sps = ps.sps.first()?;
    if sps.len() < 4 {
        return None;
    }
    let mut v = vec![1, sps[1], sps[2], sps[3], 0xff, 0xe0 | ps.sps.len() as u8];
    for s in &ps.sps {
        v.extend_from_slice(&(s.len() as u16).to_be_bytes());
        v.extend_from_slice(s);
    }
    v.push(ps.pps.len() as u8);
    for p in &ps.pps {
        v.extend_from_slice(&(p.len() as u16).to_be_bytes());
        v.extend_from_slice(p);
    }
    if matches!(sps[1], 100 | 110 | 122 | 144) {
        // High profile extension: 4:2:0, 8-bit, no SPS extensions.
        v.extend_from_slice(&[0xfc | 1, 0xf8, 0xf8, 0]);
    }
    Some(v)
}

pub fn hvcc(ps: &ParamSets) -> Option<Vec<u8>> {
    let sps = unescape_rbsp(ps.sps.first()?);
    // 2-byte NAL header, then 1 byte of ids/sub-layer count, then the
    // 12-byte general profile_tier_level, which is byte aligned.
    if sps.len() < 15 {
        return None;
    }
    let max_sub_layers = ((sps[2] >> 1) & 0x7) + 1;
    let nesting = sps[2] & 1;
    let ptl = &sps[3..15];
    let mut v = Vec::with_capacity(64);
    v.push(1);
    v.extend_from_slice(ptl); // profile space/tier/idc, compat flags, constraint flags, level
    v.extend_from_slice(&[0xf0, 0x00]); // min_spatial_segmentation_idc = 0
    v.push(0xfc); // parallelismType = 0
    v.push(0xfc | 1); // chroma 4:2:0
    v.push(0xf8); // luma bit depth 8
    v.push(0xf8); // chroma bit depth 8
    v.extend_from_slice(&[0, 0]); // avgFrameRate unknown
    v.push((max_sub_layers << 3) | (nesting << 2) | 3);
    v.push(3);
    for (kind, list) in [(HEVC_VPS, &ps.vps), (HEVC_SPS, &ps.sps), (HEVC_PPS, &ps.pps)] {
        v.push(0x80 | kind);
        v.extend_from_slice(&(list.len() as u16).to_be_bytes());
        for n in list {
            v.extend_from_slice(&(n.len() as u16).to_be_bytes());
            v.extend_from_slice(n);
        }
    }
    Some(v)
}

// ----------------------------------------------------------------------- AV1

pub const OBU_SEQUENCE_HEADER: u8 = 1;
pub const OBU_TEMPORAL_DELIMITER: u8 = 2;
pub const OBU_PADDING: u8 = 15;

pub struct Obu<'a> {
    pub kind: u8,
    pub header: &'a [u8], // 1 or 2 bytes, as in the stream
    pub payload: &'a [u8],
}

impl Obu<'_> {
    /// Writes the OBU with `obu_has_size_field = 1`.
    pub fn write_sized(&self, out: &mut Vec<u8>) {
        out.push(self.header[0] | 0x02);
        out.extend_from_slice(&self.header[1..]);
        write_leb128(out, self.payload.len() as u64);
        out.extend_from_slice(self.payload);
    }

    pub fn to_sized_bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.payload.len() + 4);
        self.write_sized(&mut v);
        v
    }
}

pub fn write_leb128(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        out.push(b);
        if v == 0 {
            break;
        }
    }
}

fn read_leb128(data: &[u8]) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for i in 0..8 {
        let b = *data.get(i)?;
        v |= u64::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

pub fn parse_obus(data: &[u8]) -> Result<Vec<Obu<'_>>, &'static str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let h = data[i];
        if h & 0x80 != 0 {
            return Err("forbidden bit set");
        }
        let kind = (h >> 3) & 0x0f;
        let ext = h & 0x04 != 0;
        let has_size = h & 0x02 != 0;
        let hlen = if ext { 2 } else { 1 };
        if i + hlen > data.len() {
            return Err("truncated header");
        }
        let header = &data[i..i + hlen];
        i += hlen;
        let size = if has_size {
            let (s, n) = read_leb128(&data[i..]).ok_or("bad leb128")?;
            i += n;
            s as usize
        } else {
            data.len() - i
        };
        if i + size > data.len() {
            return Err("truncated payload");
        }
        out.push(Obu { kind, header, payload: &data[i..i + size] });
        i += size;
    }
    Ok(out)
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl BitReader<'_> {
    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.data.get(self.pos / 8)?;
            let bit = (byte >> (7 - (self.pos % 8))) & 1;
            v = (v << 1) | u32::from(bit);
            self.pos += 1;
        }
        Some(v)
    }

    fn uvlc(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bits(1)? == 0 {
            zeros += 1;
            if zeros >= 32 {
                return Some(u32::MAX);
            }
        }
        Some(self.bits(zeros)? + ((1u32 << zeros) - 1))
    }
}

/// Builds the `av1C` record from a sized sequence header OBU.
/// Our encoder input is always 8-bit 4:2:0, so the color fields are fixed.
pub fn av1c(seq_header_obu: &[u8]) -> Option<Vec<u8>> {
    let obus = parse_obus(seq_header_obu).ok()?;
    let sh = obus.iter().find(|o| o.kind == OBU_SEQUENCE_HEADER)?;
    let mut r = BitReader { data: sh.payload, pos: 0 };
    let profile = r.bits(3)?;
    let _still = r.bits(1)?;
    let reduced = r.bits(1)?;
    let (level, tier) = if reduced == 1 {
        (r.bits(5)?, 0)
    } else {
        let mut decoder_model = false;
        let mut buffer_delay_len = 0;
        if r.bits(1)? == 1 {
            // timing_info
            r.bits(32)?;
            r.bits(32)?;
            if r.bits(1)? == 1 {
                r.uvlc()?;
            }
            decoder_model = r.bits(1)? == 1;
            if decoder_model {
                buffer_delay_len = r.bits(5)? + 1;
                r.bits(32)?;
                r.bits(5)?;
                r.bits(5)?;
            }
        }
        let initial_display_delay = r.bits(1)? == 1;
        let ops = r.bits(5)? + 1;
        let mut first = None;
        for _ in 0..ops {
            r.bits(12)?;
            let lvl = r.bits(5)?;
            let tier = if lvl > 7 { r.bits(1)? } else { 0 };
            if decoder_model && r.bits(1)? == 1 {
                r.bits(buffer_delay_len)?;
                r.bits(buffer_delay_len)?;
                r.bits(1)?;
            }
            if initial_display_delay && r.bits(1)? == 1 {
                r.bits(4)?;
            }
            first.get_or_insert((lvl, tier));
        }
        first?
    };
    let mut v = vec![0x81, ((profile as u8) << 5) | level as u8];
    // tier, high_bitdepth=0, twelve_bit=0, mono=0, subsampling x=1 y=1, position=0
    v.push(((tier as u8) << 7) | 0b0000_1100);
    v.push(0);
    v.extend_from_slice(seq_header_obu);
    Some(v)
}

/// AudioSpecificConfig for AAC-LC.
pub fn aac_asc(sample_rate: u32, channels: u32) -> [u8; 2] {
    const RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];
    let idx = RATES.iter().position(|&r| r == sample_rate).unwrap_or(3) as u16;
    let v: u16 = (2 << 11) | (idx << 7) | ((channels as u16) << 3);
    v.to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_mixed_start_codes() {
        let s = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5];
        let n = split_annexb(&s);
        assert_eq!(n, vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 4, 5][..]]);
    }

    #[test]
    fn unescapes() {
        assert_eq!(unescape_rbsp(&[1, 0, 0, 3, 1, 0, 0, 3]), vec![1, 0, 0, 1, 0, 0]);
    }

    #[test]
    fn h264_sample_drops_param_sets() {
        let au = [0, 0, 0, 1, 0x09, 0xf0, 0, 0, 0, 1, 0x67, 0x64, 0, 0x28, 0, 0, 0, 1, 0x68, 0xee, 0, 0, 1, 0x65, 0xaa];
        let ps = find_param_sets(Codec::H264, &au);
        assert!(ps.is_complete(Codec::H264));
        let mut out = Vec::new();
        to_mp4_sample(Codec::H264, &au, &mut out);
        assert_eq!(out, vec![0, 0, 0, 2, 0x65, 0xaa]);
        let c = avcc(&ps).unwrap();
        assert_eq!(&c[..4], &[1, 0x64, 0, 0x28]);
    }

    #[test]
    fn leb128_round_trip() {
        for v in [0u64, 1, 127, 128, 300, 1 << 20] {
            let mut b = Vec::new();
            write_leb128(&mut b, v);
            assert_eq!(read_leb128(&b), Some((v, b.len())));
        }
    }

    #[test]
    fn av1_strips_temporal_delimiter_and_adds_sizes() {
        // TD (sized, empty) + frame OBU without size field.
        let tu = [0x12, 0x00, 0x30, 0xaa, 0xbb];
        let mut out = Vec::new();
        to_mp4_sample(Codec::Av1, &tu, &mut out);
        assert_eq!(out, vec![0x32, 2, 0xaa, 0xbb]);
    }

    #[test]
    fn av1c_from_simple_sequence_header() {
        // profile 0, not still, not reduced, no timing, no display delay,
        // 1 operating point, idc 0, level 8 (4.0) -> tier bit read.
        let mut w = BitWriter::default();
        w.put(0, 3);
        w.put(0, 1);
        w.put(0, 1);
        w.put(0, 1); // timing_info_present
        w.put(0, 1); // initial_display_delay_present
        w.put(0, 5); // operating_points_cnt_minus_1
        w.put(0, 12);
        w.put(8, 5);
        w.put(1, 1); // tier
        let payload = w.finish();
        let mut obu = vec![0x0a];
        write_leb128(&mut obu, payload.len() as u64);
        obu.extend_from_slice(&payload);
        let c = av1c(&obu).unwrap();
        assert_eq!(c[0], 0x81);
        assert_eq!(c[1], 8);
        assert_eq!(c[2] >> 7, 1);
        assert_eq!(&c[4..], &obu[..]);
    }

    #[test]
    fn asc_for_48k_stereo() {
        assert_eq!(aac_asc(48000, 2), [0x11, 0x90]);
        assert_eq!(aac_asc(44100, 2), [0x12, 0x10]);
    }

    #[derive(Default)]
    struct BitWriter {
        bits: Vec<u8>,
    }
    impl BitWriter {
        fn put(&mut self, v: u32, n: u32) {
            for i in (0..n).rev() {
                self.bits.push(((v >> i) & 1) as u8);
            }
        }
        fn finish(self) -> Vec<u8> {
            self.bits
                .chunks(8)
                .map(|c| c.iter().enumerate().fold(0u8, |a, (i, b)| a | (b << (7 - i))))
                .collect()
        }
    }
}
