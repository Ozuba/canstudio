//! Lazily decoded, incrementally extended signal time series.

use std::collections::HashMap;

use dbc::{CompiledDb, SignalRef};

use crate::store::FrameStore;

/// Decoded samples of one signal (relative time, physical value).
#[derive(Default, Clone, Debug)]
pub struct Series {
    pub t: Vec<f64>,
    pub v: Vec<f64>,
    /// Position in the message's frame index already decoded.
    cursor: usize,
}

impl Series {
    pub fn len(&self) -> usize {
        self.t.len()
    }

    pub fn is_empty(&self) -> bool {
        self.t.is_empty()
    }

    /// Sample index range with `t0 <= t <= t1`, widened by one sample on each
    /// side so lines run to the plot edges.
    pub fn visible(&self, t0: f64, t1: f64) -> std::ops::Range<usize> {
        let a = self.t.partition_point(|&t| t < t0).saturating_sub(1);
        let b = (self.t.partition_point(|&t| t <= t1) + 1).min(self.t.len());
        a..b.max(a)
    }

    /// Last sample at or before `t`.
    pub fn at(&self, t: f64) -> Option<(f64, f64)> {
        let i = self.t.partition_point(|&x| x <= t).checked_sub(1)?;
        Some((self.t[i], self.v[i]))
    }

    /// Sample closest in time to `t`.
    pub fn nearest(&self, t: f64) -> Option<(f64, f64)> {
        let i = self.t.partition_point(|&x| x < t);
        [i.checked_sub(1), (i < self.t.len()).then_some(i)]
            .into_iter()
            .flatten()
            .min_by(|&a, &b| (self.t[a] - t).abs().total_cmp(&(self.t[b] - t).abs()))
            .map(|i| (self.t[i], self.v[i]))
    }

    /// (min, max) of the values in an index range, ignoring NaNs.
    pub fn bounds(&self, range: std::ops::Range<usize>) -> Option<(f64, f64)> {
        self.v[range].iter().filter(|v| v.is_finite()).fold(None, |acc, &v| match acc {
            None => Some((v, v)),
            Some((lo, hi)) => Some((lo.min(v), hi.max(v))),
        })
    }
}

/// Cache of decoded series keyed by signal. Call [`SeriesCache::clear`] when
/// the database or the frame store is replaced; appends are picked up
/// automatically.
#[derive(Default)]
pub struct SeriesCache {
    map: HashMap<SignalRef, Series>,
}

impl SeriesCache {
    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Decoded series for `r`, extended with any frames appended since the last call.
    pub fn get(&mut self, store: &FrameStore, db: &CompiledDb, r: SignalRef) -> &Series {
        let series = self.map.entry(r).or_default();
        let msg = &db.messages[r.msg];
        let idx = store.indices(msg.id);
        if series.cursor < idx.len() {
            series.t.reserve(idx.len() - series.cursor);
            series.v.reserve(idx.len() - series.cursor);
            for &i in &idx[series.cursor..] {
                if store.is_remote(i as usize) {
                    continue;
                }
                if let Some(v) = msg.decode(r.sig, store.data(i as usize)) {
                    series.t.push(store.ts[i as usize]);
                    series.v.push(v);
                }
            }
            series.cursor = idx.len();
        }
        series
    }

    /// Already decoded series, without updating.
    pub fn peek(&self, r: SignalRef) -> Option<&Series> {
        self.map.get(&r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Frame;

    #[test]
    fn remote_frames_carry_no_values() {
        // A remote request for the message (RTR, DLC 8) has no payload; it must not plot as zeros.
        let dbc = dbc::parse("BO_ 5 M: 8 X\n SG_ A : 0|8@1+ (1,0) [0|0] \"\" X\n").unwrap();
        let db = CompiledDb::new([&dbc]);
        let mut store = FrameStore::with_t0(0.0);
        store.push(&Frame { ts: 1.0, ..Frame::new(5, false, &[7; 8]) });
        store.push(&Frame { ts: 2.0, flags: crate::flags::RTR, len: 8, ..Frame::new(5, false, &[]) });
        store.push(&Frame { ts: 3.0, ..Frame::new(5, false, &[9; 8]) });
        let mut cache = SeriesCache::default();
        assert_eq!(cache.get(&store, &db, SignalRef { msg: 0, sig: 0 }).v, vec![7.0, 9.0]);
    }

    #[test]
    fn incremental_decode() {
        let dbc = dbc::parse("BO_ 5 M: 2 X\n SG_ A : 0|8@1+ (2,1) [0|0] \"\" X\n").unwrap();
        let db = CompiledDb::new([&dbc]);
        let mut store = FrameStore::with_t0(10.0);
        let mut cache = SeriesCache::default();
        let r = SignalRef { msg: 0, sig: 0 };
        for i in 0..3u8 {
            let mut f = Frame::new(5, false, &[i]);
            f.ts = 10.0 + i as f64;
            store.push(&f);
            store.push(&Frame { ts: 10.5 + i as f64, ..Frame::new(6, false, &[9]) });
        }
        assert_eq!(cache.get(&store, &db, r).v, vec![1.0, 3.0, 5.0]);
        let mut f = Frame::new(5, false, &[10]);
        f.ts = 20.0;
        store.push(&f);
        let s = cache.get(&store, &db, r);
        assert_eq!(s.t, vec![0.0, 1.0, 2.0, 10.0]);
        assert_eq!(s.at(1.5), Some((1.0, 3.0)));
        assert_eq!(s.nearest(1.6), Some((2.0, 5.0)));
        assert_eq!(s.visible(0.5, 1.5), 0..3);
        assert_eq!(s.bounds(0..4), Some((1.0, 21.0)));
    }
}
