//! AAC-LC encoding with AudioToolbox's AudioConverter. Decoding uses the
//! shared symphonia decoder.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_audio_toolbox::{
    AudioConverterDispose, AudioConverterFillComplexBuffer, AudioConverterNew, AudioConverterRef, AudioConverterSetProperty,
    kAudioConverterEncodeBitRate,
};
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioStreamPacketDescription, kAudioFormatFlagIsPacked,
    kAudioFormatFlagIsSignedInteger, kAudioFormatLinearPCM, kAudioFormatMPEG4AAC,
};

pub use crate::sys::unix::aac_decode::AacDecoder;
use crate::media::{AAC_FRAME, CHANNELS, SAMPLE_RATE};

/// Returned by the input callback when it has no more data right now.
const NO_DATA_YET: i32 = 0x6e6f6474; // 'nodt'

pub struct AacEncoder {
    conv: AudioConverterRef,
    fifo: Vec<i16>,
    out: Vec<u8>,
}

// SAFETY: the converter is used from one thread at a time.
unsafe impl Send for AacEncoder {}

struct Feed {
    data: *const i16,
    frames: u32,
    given: bool,
    end: bool,
}

unsafe extern "C-unwind" fn input_proc(
    _conv: AudioConverterRef,
    io_packets: NonNull<u32>,
    io_data: NonNull<AudioBufferList>,
    _desc: *mut *mut AudioStreamPacketDescription,
    user: *mut c_void,
) -> i32 {
    unsafe {
        let feed = &mut *(user as *mut Feed);
        let list = io_data.as_ptr();
        if feed.given || feed.frames == 0 {
            *io_packets.as_ptr() = 0;
            (*list).mBuffers[0].mDataByteSize = 0;
            (*list).mBuffers[0].mData = std::ptr::null_mut();
            return if feed.end { 0 } else { NO_DATA_YET };
        }
        feed.given = true;
        *io_packets.as_ptr() = feed.frames;
        (*list).mNumberBuffers = 1;
        (*list).mBuffers[0] = AudioBuffer {
            mNumberChannels: CHANNELS,
            mDataByteSize: feed.frames * CHANNELS * 2,
            mData: feed.data as *mut c_void,
        };
        0
    }
}

impl AacEncoder {
    pub fn new(bitrate_kbps: u32) -> Result<Self, String> {
        let src = AudioStreamBasicDescription {
            mSampleRate: f64::from(SAMPLE_RATE),
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked,
            mBytesPerPacket: CHANNELS * 2,
            mFramesPerPacket: 1,
            mBytesPerFrame: CHANNELS * 2,
            mChannelsPerFrame: CHANNELS,
            mBitsPerChannel: 16,
            mReserved: 0,
        };
        let dst = AudioStreamBasicDescription {
            mSampleRate: f64::from(SAMPLE_RATE),
            mFormatID: kAudioFormatMPEG4AAC,
            mFormatFlags: 0,
            mBytesPerPacket: 0,
            mFramesPerPacket: AAC_FRAME as u32,
            mBytesPerFrame: 0,
            mChannelsPerFrame: CHANNELS,
            mBitsPerChannel: 0,
            mReserved: 0,
        };
        let mut conv: AudioConverterRef = std::ptr::null_mut();
        let status = unsafe { AudioConverterNew(NonNull::from(&src), NonNull::from(&dst), NonNull::from(&mut conv)) };
        if status != 0 || conv.is_null() {
            return Err(format!("AAC encoder unavailable (OSStatus {status})"));
        }
        let bitrate: u32 = bitrate_kbps * 1000;
        unsafe {
            AudioConverterSetProperty(conv, kAudioConverterEncodeBitRate, 4, NonNull::from(&bitrate).cast());
        }
        Ok(Self { conv, fifo: Vec::new(), out: vec![0u8; 8192] })
    }

    /// Asks the converter for one packet. Returns false when it needs more input.
    fn pull(&mut self, feed: &mut Feed, f: &mut impl FnMut(&[u8])) -> bool {
        let mut packets: u32 = 1;
        let mut list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer { mNumberChannels: CHANNELS, mDataByteSize: self.out.len() as u32, mData: self.out.as_mut_ptr().cast() }],
        };
        let mut desc = AudioStreamPacketDescription { mStartOffset: 0, mVariableFramesInPacket: 0, mDataByteSize: 0 };
        let status = unsafe {
            AudioConverterFillComplexBuffer(
                self.conv,
                Some(input_proc),
                (feed as *mut Feed).cast(),
                NonNull::from(&mut packets),
                NonNull::from(&mut list),
                &mut desc,
            )
        };
        if packets > 0 {
            let n = list.mBuffers[0].mDataByteSize as usize;
            f(&self.out[..n.min(self.out.len())]);
            return true;
        }
        let _ = status;
        false
    }

    pub fn encode(&mut self, pcm: &[i16], mut f: impl FnMut(&[u8])) -> Result<(), String> {
        self.fifo.extend_from_slice(pcm);
        let block = AAC_FRAME * CHANNELS as usize;
        while self.fifo.len() >= block {
            let mut feed = Feed { data: self.fifo.as_ptr(), frames: AAC_FRAME as u32, given: false, end: false };
            // Drain everything the converter has for this block.
            while self.pull(&mut feed, &mut f) {}
            if feed.given {
                self.fifo.drain(..block);
            } else {
                break;
            }
        }
        Ok(())
    }

    pub fn finish(&mut self, mut f: impl FnMut(&[u8])) -> Result<(), String> {
        let block = AAC_FRAME * CHANNELS as usize;
        if !self.fifo.is_empty() {
            let pad = block - self.fifo.len() % block;
            if pad < block {
                self.fifo.extend(std::iter::repeat_n(0, pad));
            }
            self.encode(&[], &mut f)?;
        }
        let mut feed = Feed { data: std::ptr::null(), frames: 0, given: false, end: true };
        for _ in 0..8 {
            if !self.pull(&mut feed, &mut f) {
                break;
            }
        }
        Ok(())
    }
}

impl Drop for AacEncoder {
    fn drop(&mut self) {
        unsafe {
            AudioConverterDispose(self.conv);
        }
    }
}
