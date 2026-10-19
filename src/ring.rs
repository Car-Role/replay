//! In-memory ring of encoded packets.
//!
//! The ring keeps only compressed data, so memory use follows the real bitrate.
//! Old data is dropped one GOP at a time, so the oldest packet is always a
//! keyframe and any snapshot decodes on its own. Evicted buffers are reused, so
//! the steady state does no heap allocation.

use std::collections::VecDeque;
use std::sync::Arc;

/// Time in 100 ns units on the QueryPerformanceCounter clock.
pub type Ticks = i64;
pub const TICKS_PER_SEC: Ticks = 10_000_000;

#[derive(Clone)]
pub struct Packet {
    pub pts: Ticks,
    pub key: bool,
    pub data: Arc<Vec<u8>>,
}

pub struct PacketRing {
    packets: VecDeque<Packet>,
    bytes: usize,
    max_age: Ticks,
    max_bytes: usize,
    spare: Vec<Vec<u8>>,
}

const MAX_SPARE: usize = 64;

impl PacketRing {
    pub fn new(max_age: Ticks, max_bytes: usize) -> Self {
        Self { packets: VecDeque::new(), bytes: 0, max_age, max_bytes, spare: Vec::new() }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.packets.len()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn newest_pts(&self) -> Option<Ticks> {
        self.packets.back().map(|p| p.pts)
    }

    /// Time covered by the ring.
    pub fn span(&self) -> Ticks {
        match (self.packets.front(), self.packets.back()) {
            (Some(a), Some(b)) => b.pts - a.pts,
            _ => 0,
        }
    }

    /// Copies `data` into the ring. Packets must arrive in pts order.
    pub fn push(&mut self, pts: Ticks, key: bool, data: &[u8]) {
        if self.packets.is_empty() && !key {
            // A stream must start on a keyframe; anything before it is undecodable.
            return;
        }
        let mut buf = self.take_buffer(data.len());
        buf.extend_from_slice(data);
        self.bytes += buf.len();
        self.packets.push_back(Packet { pts, key, data: Arc::new(buf) });
        self.trim();
    }

    fn take_buffer(&mut self, len: usize) -> Vec<u8> {
        // Prefer a spare buffer that already fits, so it does not reallocate.
        if let Some(i) = self.spare.iter().position(|b| b.capacity() >= len) {
            let mut b = self.spare.swap_remove(i);
            b.clear();
            return b;
        }
        match self.spare.pop() {
            Some(mut b) => {
                b.clear();
                b.reserve(len);
                b
            }
            None => Vec::with_capacity(len),
        }
    }

    fn recycle(&mut self, p: Packet) {
        // A save in progress may still hold the Arc; then the buffer is freed later.
        if let Ok(b) = Arc::try_unwrap(p.data)
            && self.spare.len() < MAX_SPARE
        {
            self.spare.push(b);
        }
    }

    /// Drops whole GOPs from the front while the next GOP still covers the
    /// retention window, or while the ring is over its byte budget.
    fn trim(&mut self) {
        let Some(newest) = self.newest_pts() else { return };
        let cutoff = newest - self.max_age;
        loop {
            let Some(next_key) = self.packets.iter().skip(1).position(|p| p.key).map(|i| i + 1) else {
                return;
            };
            let over_age = self.packets[next_key].pts <= cutoff;
            let over_size = self.bytes > self.max_bytes;
            if !over_age && !over_size {
                return;
            }
            for _ in 0..next_key {
                let p = self.packets.pop_front().expect("index checked");
                self.bytes -= p.data.len();
                self.recycle(p);
            }
        }
    }

    /// Returns the packets of the last `duration`, starting at a keyframe.
    /// The returned packets share memory with the ring (no copy).
    pub fn snapshot(&self, duration: Ticks) -> Vec<Packet> {
        let Some(newest) = self.newest_pts() else { return Vec::new() };
        let from = newest - duration;
        let start = self
            .packets
            .iter()
            .rposition(|p| p.key && p.pts <= from)
            .unwrap_or(0);
        self.packets.range(start..).cloned().collect()
    }

    /// Returns all packets with `pts >= from`, for streams where every packet is a sync point.
    pub fn snapshot_from(&self, from: Ticks) -> Vec<Packet> {
        let start = self.packets.partition_point(|p| p.pts < from);
        self.packets.range(start..).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Ticks = TICKS_PER_SEC;

    fn fill(ring: &mut PacketRing, seconds: i64, fps: i64, gop: i64) {
        for i in 0..seconds * fps {
            ring.push(i * S / fps, i % gop == 0, &[0u8; 100]);
        }
    }

    #[test]
    fn starts_on_keyframe() {
        let mut r = PacketRing::new(10 * S, usize::MAX);
        r.push(0, false, &[1]);
        assert_eq!(r.len(), 0);
        r.push(1, true, &[1]);
        r.push(2, false, &[1]);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn trims_by_age_on_gop_boundaries() {
        let mut r = PacketRing::new(2 * S, usize::MAX);
        fill(&mut r, 10, 60, 30);
        let first = r.snapshot(i64::MAX);
        assert!(first[0].key);
        let span = r.newest_pts().unwrap() - first[0].pts;
        assert!(span >= 2 * S, "kept {span}");
        assert!(span < 2 * S + S / 2 + S / 60, "kept {span}");
    }

    #[test]
    fn trims_by_size() {
        let mut r = PacketRing::new(100 * S, 100 * 60);
        fill(&mut r, 10, 60, 30);
        assert!(r.bytes() <= 100 * 60);
        assert!(r.snapshot(i64::MAX)[0].key);
    }

    #[test]
    fn snapshot_starts_at_keyframe_before_window() {
        let mut r = PacketRing::new(10 * S, usize::MAX);
        fill(&mut r, 5, 60, 30);
        let snap = r.snapshot(S);
        assert!(snap[0].key);
        let newest = r.newest_pts().unwrap();
        assert!(snap[0].pts <= newest - S);
        assert!(snap[0].pts > newest - S - S / 2 - 1);
    }

    #[test]
    fn snapshot_from_is_inclusive() {
        let mut r = PacketRing::new(10 * S, usize::MAX);
        for i in 0..10 {
            r.push(i * 100, true, &[0]);
        }
        let s = r.snapshot_from(450);
        assert_eq!(s[0].pts, 500);
        assert_eq!(s.len(), 5);
    }

    #[test]
    fn reuses_buffers() {
        let mut r = PacketRing::new(S, usize::MAX);
        fill(&mut r, 5, 60, 30);
        assert!(!r.spare.is_empty());
    }
}
