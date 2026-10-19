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
                let deadline = std::time::Instant::now() + Duration::from_secs(4);
                while std::time::Instant::now() < deadline {
                    match dec.frames.recv_timeout(Duration::from_millis(500)) {
                        Ok(f) if f.generation == generation => {
                            let (w, h, rgba) = shrink(&f.rgba, f.width, f.height, thumb_h);
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
            let mut acc = [0u32; 3];
            let mut n = 0u32;
            // Sample a 3x3 grid inside the box; enough for a thumbnail.
            for sy in 0..3 {
                let yy = y0 + (y1 - y0) * sy / 3;
                for sx in 0..3 {
                    let xx = x0 + (x1 - x0) * sx / 3;
                    let i = (yy * w + xx) * 4;
                    acc[0] += u32::from(src[i]);
                    acc[1] += u32::from(src[i + 1]);
                    acc[2] += u32::from(src[i + 2]);
                    n += 1;
                }
            }
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
