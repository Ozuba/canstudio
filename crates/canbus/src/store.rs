//! Columnar, append-only frame storage with a per-identifier index.
//!
//! Millions of frames are common (a 30 minute log is ~5 M frames), so frames
//! are stored as parallel columns (~30 B/frame) instead of an array of
//! 80-byte structs. Timestamps are stored relative to [`FrameStore::t0`] to
//! keep sub-microsecond precision in `f64`.

use std::collections::HashMap;

use crate::frame::{self, Frame};

#[derive(Default, Clone)]
pub struct FrameStore {
    /// Absolute time (UNIX seconds) of relative time zero.
    pub t0: f64,
    /// Timestamps relative to `t0`, non-decreasing.
    pub ts: Vec<f64>,
    pub key: Vec<u32>,
    pub flags: Vec<u8>,
    pub channel: Vec<u8>,
    off: Vec<u64>,
    data: Vec<u8>,
    /// Frame indices per key, in time order.
    by_key: HashMap<u32, Vec<u32>>,
    /// Keys in first-seen order.
    pub keys: Vec<u32>,
    /// Interface names indexed by `channel`.
    pub channels: Vec<String>,
}

/// Borrowed view of one stored frame.
#[derive(Clone, Copy, Debug)]
pub struct FrameRef<'a> {
    pub index: usize,
    /// Relative timestamp.
    pub ts: f64,
    pub key: u32,
    pub flags: u8,
    pub channel: u8,
    pub data: &'a [u8],
}

impl FrameRef<'_> {
    pub fn id(&self) -> u32 {
        frame::split_key(self.key).0
    }

    pub fn to_frame(&self, t0: f64) -> Frame {
        let mut f = Frame { ts: self.ts + t0, id: self.id(), flags: self.flags, channel: self.channel, ..Default::default() };
        f.set_data(self.data);
        f
    }
}

impl FrameStore {
    pub fn with_t0(t0: f64) -> Self {
        FrameStore { t0, ..Default::default() }
    }

    pub fn len(&self) -> usize {
        self.ts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ts.is_empty()
    }

    pub fn channel_index(&mut self, name: &str) -> u8 {
        match self.channels.iter().position(|c| c == name) {
            Some(i) => i as u8,
            None => {
                self.channels.push(name.to_string());
                (self.channels.len() - 1) as u8
            }
        }
    }

    /// Append a frame with a relative timestamp. Out-of-order timestamps are
    /// clamped so the time column stays sorted for binary search.
    pub fn push_rel(&mut self, ts: f64, key: u32, flags: u8, channel: u8, data: &[u8]) {
        let ts = self.ts.last().map_or(ts, |&last| ts.max(last));
        self.append(ts, key, flags, channel, data);
    }

    /// Append keeping the timestamp as is. Bulk loaders use this and call
    /// [`FrameStore::sort_if_needed`] afterwards, so out-of-order input is
    /// reordered rather than having its timestamps altered.
    pub(crate) fn append(&mut self, ts: f64, key: u32, flags: u8, channel: u8, data: &[u8]) {
        let idx = self.ts.len() as u32;
        self.ts.push(ts);
        self.key.push(key);
        self.flags.push(flags);
        self.channel.push(channel);
        self.off.push(self.data.len() as u64);
        self.data.extend_from_slice(data);
        self.by_key
            .entry(key)
            .or_insert_with(|| {
                self.keys.push(key);
                Vec::new()
            })
            .push(idx);
    }

    /// Append a frame with an absolute timestamp.
    pub fn push(&mut self, f: &Frame) {
        if self.is_empty() && self.t0 == 0.0 {
            self.t0 = f.ts;
        }
        self.push_rel(f.ts - self.t0, f.key(), f.flags, f.channel, f.data());
    }

    pub fn data(&self, i: usize) -> &[u8] {
        let start = self.off[i] as usize;
        let end = self.off.get(i + 1).map_or(self.data.len(), |&o| o as usize);
        &self.data[start..end]
    }

    pub fn get(&self, i: usize) -> FrameRef<'_> {
        FrameRef { index: i, ts: self.ts[i], key: self.key[i], flags: self.flags[i], channel: self.channel[i], data: self.data(i) }
    }

    /// Frame indices for one key, in time order.
    pub fn indices(&self, key: u32) -> &[u32] {
        self.by_key.get(&key).map_or(&[], Vec::as_slice)
    }

    /// Relative time span covered by the store.
    pub fn time_range(&self) -> Option<(f64, f64)> {
        Some((*self.ts.first()?, *self.ts.last()?))
    }

    /// Index range of frames with `t0 <= ts < t1` (relative times).
    pub fn range(&self, t0: f64, t1: f64) -> std::ops::Range<usize> {
        self.ts.partition_point(|&t| t < t0)..self.ts.partition_point(|&t| t < t1)
    }

    /// Frame index range for `key` restricted to relative time `[t0, t1)`.
    pub fn key_range(&self, key: u32, t0: f64, t1: f64) -> &[u32] {
        let idx = self.indices(key);
        let a = idx.partition_point(|&i| self.ts[i as usize] < t0);
        let b = idx.partition_point(|&i| self.ts[i as usize] < t1);
        &idx[a..b]
    }

    /// Last frame for `key` at or before relative time `t`.
    pub fn last_before(&self, key: u32, t: f64) -> Option<FrameRef<'_>> {
        let idx = self.indices(key);
        let n = idx.partition_point(|&i| self.ts[i as usize] <= t);
        n.checked_sub(1).map(|j| self.get(idx[j] as usize))
    }

    /// Remote request (RTR): has a length but no payload.
    pub fn is_remote(&self, i: usize) -> bool {
        self.flags[i] & frame::flags::RTR != 0
    }

    /// Last frame for `key` at or before `t` that carries data, skipping remote
    /// requests (bounded look-back, so remote-only ids stay cheap).
    pub fn last_data_before(&self, key: u32, t: f64) -> Option<FrameRef<'_>> {
        let idx = self.indices(key);
        let n = idx.partition_point(|&i| self.ts[i as usize] <= t);
        idx[..n].iter().rev().take(1024).map(|&i| i as usize).find(|&i| !self.is_remote(i)).map(|i| self.get(i))
    }

    /// Copy the frames in `range` into a new store (used to trim logs).
    pub fn slice(&self, range: std::ops::Range<usize>) -> FrameStore {
        let mut out = FrameStore::with_t0(self.t0);
        out.channels = self.channels.clone();
        for i in range {
            let f = self.get(i);
            out.push_rel(f.ts, f.key, f.flags, f.channel, f.data);
        }
        out
    }

    /// Concatenate column chunks produced in parallel (already time-ordered).
    pub(crate) fn extend_from(&mut self, other: FrameStore) {
        let remap: Vec<u8> = other.channels.iter().map(|c| self.channel_index(c)).collect();
        let base = self.ts.len() as u32;
        let data_base = self.data.len() as u64;
        self.ts.extend(&other.ts);
        self.key.extend(&other.key);
        self.flags.extend(&other.flags);
        self.channel.extend(other.channel.iter().map(|&c| remap[c as usize]));
        self.off.extend(other.off.iter().map(|o| o + data_base));
        self.data.extend(&other.data);
        for key in other.keys {
            let src = &other.by_key[&key];
            self.by_key
                .entry(key)
                .or_insert_with(|| {
                    self.keys.push(key);
                    Vec::new()
                })
                .extend(src.iter().map(|i| i + base));
        }
    }

    /// Sort by timestamp if needed (merged multi-interface logs can be slightly out of order).
    pub(crate) fn sort_if_needed(&mut self) {
        if self.ts.is_sorted() {
            return;
        }
        let mut order: Vec<usize> = (0..self.len()).collect();
        order.sort_by(|&a, &b| self.ts[a].total_cmp(&self.ts[b]));
        let mut out = FrameStore::with_t0(self.t0);
        out.channels = std::mem::take(&mut self.channels);
        for i in order {
            let f = self.get(i);
            out.push_rel(f.ts, f.key, f.flags, f.channel, f.data);
        }
        *self = out;
    }

    /// Approximate heap usage in bytes.
    pub fn memory_bytes(&self) -> usize {
        self.len() * (8 + 4 + 1 + 1 + 8 + 4) + self.data.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_and_ranges() {
        let mut s = FrameStore::with_t0(100.0);
        for i in 0..10 {
            let mut f = Frame::new(if i % 2 == 0 { 0x10 } else { 0x20 }, false, &[i as u8]);
            f.ts = 100.0 + i as f64 * 0.1;
            s.push(&f);
        }
        assert_eq!(s.indices(0x10), &[0, 2, 4, 6, 8]);
        assert_eq!(s.range(0.25, 0.65), 3..7);
        assert_eq!(s.key_range(0x20, 0.25, 0.65), &[3, 5]);
        assert_eq!(s.last_before(0x10, 0.45).unwrap().data, &[4]);
        assert_eq!(s.data(9), &[9]);
        let sl = s.slice(2..5);
        assert_eq!(sl.len(), 3);
        assert_eq!(sl.indices(0x20), &[1]);
    }

    #[test]
    fn extend_remaps_channels_and_indices() {
        let mut a = FrameStore::default();
        let c = a.channel_index("can0");
        a.push_rel(0.0, 1, 0, c, &[1]);
        let mut b = FrameStore::default();
        let c1 = b.channel_index("can1");
        let c0 = b.channel_index("can0");
        b.push_rel(1.0, 1, 0, c1, &[2, 3]);
        b.push_rel(2.0, 2, 0, c0, &[]);
        a.extend_from(b);
        assert_eq!(a.channels, vec!["can0", "can1"]);
        assert_eq!(a.channel, vec![0, 1, 0]);
        assert_eq!(a.indices(1), &[0, 1]);
        assert_eq!(a.data(1), &[2, 3]);
        assert_eq!(a.data(2), &[] as &[u8]);
    }
}
