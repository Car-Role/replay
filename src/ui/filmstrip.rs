//! Builds the trim timeline's filmstrip: evenly spaced frames of the clip,
//! decoded on a background thread with a separate decoder and shrunk to
//! thumbnails.

use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use crate::sys::player::VideoDecoder;

pub struct Thumb {
    pub index: usize,
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// Starts decoding `count` thumbnails `thumb_h` pixels high.
pub fn build(path: &Path, duration: f64, count: usize, thumb_h: usize, wake: impl Fn() + Send + 'static) -> Receiver<Thumb> {
    let (tx, rx) = mpsc::channel();
    let path = path.to_path_buf();
    std::thread::Builder::new()
        .name("filmstrip".into())
        .spawn(move || {
            let dec = VideoDecoder::open(&path, || {});
            for i in 0..count {
                let t = duration * (i as f64 + 0.5) / count as f64;
                let generation = i as u64 + 1;
                dec.seek(t, generation);
                let mut skipped = 0;
                let deadline = std::time::Instant::now() + Duration::from_secs(4);
                while std::time::Instant::now() < deadline {
                    match dec.frames.recv_timeout(Duration::from_millis(500)) {
                        // The first frames after a seek can be black while the
                        // decoder settles; take the next one (1/60 s later).
                        Ok(f) if f.generation == generation && is_black(&f.rgba) && skipped < 6 => {
                            skipped += 1;
                        }
                        Ok(f) if f.generation == generation => {
                            let (w, h, mut rgba) = shrink(&f.rgba, f.width, f.height, thumb_h);
                            lift_dark(&mut rgba);
                            if tx.send(Thumb { index: i, width: w, height: h, rgba }).is_err() {
                                return;
                            }
                            wake();
                            break;
                        }
                        Ok(_) => {}
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            }
        })
        .ok();
    rx
}

/// Brightens dark tiles (filmstrip only, never the player) so dark
/// moments can still be found by scrubbing.
fn lift_dark(rgba: &mut [u8]) {
    let n = (rgba.len() / 4).max(1) as u64;
    let mean: u64 = rgba.chunks(4).map(|p| (u64::from(p[0]) * 2 + u64::from(p[1]) * 5 + u64::from(p[2])) / 8).sum::<u64>() / n;
    if mean >= 60 || mean == 0 {
        return;
    }
    let gain = (70.0 / mean as f32).min(2.5);
    for p in rgba.chunks_mut(4) {
        for c in &mut p[..3] {
            *c = (f32::from(*c) * gain).min(255.0) as u8;
        }
    }
}

/// True when almost every sampled pixel is near black.
fn is_black(rgba: &[u8]) -> bool {
    let step = (rgba.len() / 4 / 2000).max(1) * 4;
    let (mut n, mut dark) = (0, 0);
    let mut i = 0;
    while i + 3 < rgba.len() {
        n += 1;
        if u16::from(rgba[i]) + u16::from(rgba[i + 1]) + u16::from(rgba[i + 2]) < 24 {
            dark += 1;
        }
        i += step;
    }
    n > 0 && dark * 100 / n >= 97
}

/// Box-filter downscale of an RGBA image to height `th`.
pub fn shrink(src: &[u8], w: usize, h: usize, th: usize) -> (usize, usize, Vec<u8>) {
    if w == 0 || h == 0 || src.len() < w * h * 4 {
        return (1, 1, vec![0, 0, 0, 255]);
    }
    let th = th.min(h).max(1);
    let tw = (w * th / h).max(1);
    let mut out = vec![0u8; tw * th * 4];
    for y in 0..th {
        let y0 = y * h / th;
        let y1 = ((y + 1) * h / th).max(y0 + 1);
        for x in 0..tw {
            let x0 = x * w / tw;
            let x1 = ((x + 1) * w / tw).max(x0 + 1);
            // Average every pixel in the box, so thin bright details do not
            // turn into speckles.
            let mut acc = [0u32; 3];
            let mut n = 0u32;
            for yy in y0..y1.min(h) {
                for xx in x0..x1.min(w) {
                    let i = (yy * w + xx) * 4;
                    acc[0] += u32::from(src[i]);
                    acc[1] += u32::from(src[i + 1]);
                    acc[2] += u32::from(src[i + 2]);
                    n += 1;
                }
            }
            let n = n.max(1);
            let o = (y * tw + x) * 4;
            out[o] = (acc[0] / n) as u8;
            out[o + 1] = (acc[1] / n) as u8;
            out[o + 2] = (acc[2] / n) as u8;
            out[o + 3] = 255;
        }
    }
    (tw, th, out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn shrink_keeps_aspect() {
        let src = vec![200u8; 1280 * 720 * 4];
        let (w, h, px) = super::shrink(&src, 1280, 720, 72);
        assert_eq!((w, h), (128, 72));
        assert_eq!(px.len(), 128 * 72 * 4);
        assert_eq!(px[0], 200);
    }
}
