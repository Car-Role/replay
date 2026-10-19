//! Reads an MP4 back through Media Foundation to prove it demuxes and decodes.

use std::path::Path;

use windows::Win32::Media::MediaFoundation::*;
use windows::core::HSTRING;

pub struct StreamReport {
    pub index: u32,
    pub major: &'static str,
    pub samples: u64,
    pub decoded: u64,
    pub first_time: i64,
    pub last_time: i64,
    /// Highest absolute 16-bit PCM value (audio only).
    pub peak: i32,
}

pub fn check(path: &Path) -> Result<Vec<StreamReport>, String> {
    unsafe {
        let attrs = {
            let mut a = None;
            MFCreateAttributes(&mut a, 2).map_err(|e| e.to_string())?;
            a.expect("attrs")
        };
        let _ = attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1);
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attrs).map_err(|e| format!("open: {e}"))?;
        let mut reports = Vec::new();
        for i in 0.. {
            let Ok(t) = reader.GetNativeMediaType(i, 0) else { break };
            let major = t.GetGUID(&MF_MT_MAJOR_TYPE).map_err(|e| e.to_string())?;
            let is_video = major == MFMediaType_Video;
            let _ = reader.SetStreamSelection(i, true);
            // Ask for decoded output so the decoder runs too.
            let out = MFCreateMediaType().map_err(|e| e.to_string())?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &major).map_err(|e| e.to_string())?;
            out.SetGUID(&MF_MT_SUBTYPE, if is_video { &MFVideoFormat_NV12 } else { &MFAudioFormat_PCM }).map_err(|e| e.to_string())?;
            let decodes = reader.SetCurrentMediaType(i, None, &out).is_ok();
            reports.push((StreamReport { index: i, major: if is_video { "video" } else { "audio" }, samples: 0, decoded: 0, first_time: -1, last_time: 0, peak: 0 }, decodes));
        }
        if reports.is_empty() {
            return Err("no streams".into());
        }
        loop {
            let mut stream = 0;
            let mut flags = 0;
            let mut time = 0;
            let mut sample = None;
            let r = reader.ReadSample(MF_SOURCE_READER_ANY_STREAM.0 as u32, 0, Some(&mut stream), Some(&mut flags), Some(&mut time), Some(&mut sample));
            if let Err(e) = r {
                return Err(format!("read: {e}"));
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                if let Some((rep, _)) = reports.iter_mut().find(|(r, _)| r.index == stream) {
                    rep.index |= 0x8000_0000;
                }
                if reports.iter().all(|(r, _)| r.index & 0x8000_0000 != 0) {
                    break;
                }
                continue;
            }
            if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                return Err("reader error".into());
            }
            if let Some((rep, decodes)) = reports.iter_mut().find(|(r, _)| r.index & 0x7fff_ffff == stream)
                && sample.is_some()
            {
                rep.samples += 1;
                if *decodes {
                    rep.decoded += 1;
                }
                if rep.first_time < 0 {
                    rep.first_time = time;
                }
                rep.last_time = time;
                if rep.major == "audio"
                    && *decodes
                    && let Some(s) = &sample
                    && let Ok(b) = s.ConvertToContiguousBuffer()
                {
                    let mut p = std::ptr::null_mut();
                    let mut len = 0;
                    if b.Lock(&mut p, None, Some(&mut len)).is_ok() {
                        let pcm = std::slice::from_raw_parts(p.cast::<i16>(), len as usize / 2);
                        rep.peak = rep.peak.max(pcm.iter().map(|v| i32::from(*v).abs()).max().unwrap_or(0));
                        let _ = b.Unlock();
                    }
                }
            }
        }
        Ok(reports
            .into_iter()
            .map(|(mut r, _)| {
                r.index &= 0x7fff_ffff;
                r
            })
            .collect())
    }
}
