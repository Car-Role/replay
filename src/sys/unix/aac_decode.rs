//! AAC-LC decoding with symphonia (pure Rust): one frame in, PCM out, no delay.

use symphonia::core::audio::{Channels, SampleBuffer};
use symphonia::core::codecs::{CODEC_TYPE_AAC, CodecParameters, Decoder, DecoderOptions};
use symphonia::core::formats::Packet;

use crate::media::{AAC_FRAME, CHANNELS, SAMPLE_RATE};

pub struct AacDecoder {
    dec: Box<dyn Decoder>,
    ts: u64,
    buf: Option<SampleBuffer<i16>>,
}

impl AacDecoder {
    pub fn new(asc: &[u8]) -> Result<Self, String> {
        let mut params = CodecParameters::new();
        params
            .for_codec(CODEC_TYPE_AAC)
            .with_sample_rate(SAMPLE_RATE)
            .with_channels(Channels::FRONT_LEFT | Channels::FRONT_RIGHT)
            .with_extra_data(asc.to_vec().into_boxed_slice());
        let dec = symphonia::default::get_codecs()
            .make(&params, &DecoderOptions::default())
            .map_err(|e| format!("AAC decoder: {e}"))?;
        Ok(Self { dec, ts: 0, buf: None })
    }

    /// Decodes one raw AAC frame, appending 48 kHz stereo PCM to `out`.
    pub fn decode(&mut self, frame: &[u8], out: &mut Vec<i16>) -> Result<(), String> {
        let packet = Packet::new_from_slice(0, self.ts, AAC_FRAME as u64, frame);
        self.ts += AAC_FRAME as u64;
        let decoded = match self.dec.decode(&packet) {
            Ok(d) => d,
            // A damaged frame becomes silence; the track keeps its length.
            Err(symphonia::core::errors::Error::DecodeError(_)) => {
                out.extend(std::iter::repeat_n(0, AAC_FRAME * CHANNELS as usize));
                return Ok(());
            }
            Err(e) => return Err(format!("AAC decode: {e}")),
        };
        let spec = *decoded.spec();
        let frames = decoded.capacity() as u64;
        let buf = match &mut self.buf {
            Some(b) if b.capacity() >= decoded.capacity() * spec.channels.count() => b,
            _ => self.buf.insert(SampleBuffer::<i16>::new(frames, spec)),
        };
        buf.copy_interleaved_ref(decoded);
        let ch = spec.channels.count().max(1);
        if ch == CHANNELS as usize {
            out.extend_from_slice(buf.samples());
        } else {
            for f in buf.samples().chunks(ch) {
                let l = f[0];
                let r = if ch > 1 { f[1] } else { l };
                out.push(l);
                out.push(r);
            }
        }
        Ok(())
    }
}
