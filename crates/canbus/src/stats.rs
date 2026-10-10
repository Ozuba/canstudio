//! Bus statistics: per-identifier timing, bus load and error-frame classes.

use std::collections::HashMap;
use std::ops::Range;

use crate::frame::{flags, is_error_key};
use crate::store::FrameStore;

/// Bit rates used for bus-load estimation.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Bitrate {
    pub nominal: u32,
    /// CAN-FD data phase rate (used for frames with BRS).
    pub data: u32,
}

impl Default for Bitrate {
    fn default() -> Self {
        Bitrate { nominal: 500_000, data: 2_000_000 }
    }
}

/// Exact on-wire length in bits of a classic CAN frame, including stuff
/// bits (computed from the real bit stream and CRC-15), CRC delimiter, ACK,
/// end of frame and intermission.
pub fn classic_frame_bits(id: u32, flag: u8, data: &[u8]) -> u32 {
    let ext = flag & flags::EXT != 0;
    let rtr = flag & flags::RTR != 0;
    let mut bits: Vec<bool> = Vec::with_capacity(160);
    fn push(bits: &mut Vec<bool>, v: u32, n: u32) {
        bits.extend((0..n).rev().map(|i| v >> i & 1 != 0));
    }
    push(&mut bits, 0, 1); // SOF
    if ext {
        push(&mut bits, id >> 18, 11);
        push(&mut bits, 0b11, 2); // SRR, IDE
        push(&mut bits, id & 0x3FFFF, 18);
        push(&mut bits, rtr as u32, 1);
        push(&mut bits, 0, 2); // r1, r0
    } else {
        push(&mut bits, id, 11);
        push(&mut bits, rtr as u32, 1);
        push(&mut bits, 0, 2); // IDE, r0
    }
    let len = data.len().min(8);
    push(&mut bits, len as u32, 4);
    if !rtr {
        data[..len].iter().for_each(|&b| push(&mut bits, b as u32, 8));
    }
    let crc = bits.iter().fold(0u16, |crc, &b| {
        let fb = ((crc >> 14) & 1 != 0) ^ b;
        let crc = (crc << 1) & 0x7FFF;
        if fb { crc ^ 0x4599 } else { crc }
    });
    push(&mut bits, crc as u32, 15);
    // Stuffing: after five equal bits a complementary bit is inserted (and counts).
    let (mut stuffed, mut run, mut last) = (0, 0, None);
    for &b in &bits {
        if Some(b) == last {
            run += 1;
        } else {
            (run, last) = (1, Some(b));
        }
        if run == 5 {
            stuffed += 1;
            (run, last) = (1, Some(!b));
        }
    }
    bits.len() as u32 + stuffed + 13
}

/// On-wire duration of a frame in seconds. Classic frames are exact; CAN-FD
/// uses a worst-case stuffing estimate with the data phase at the data rate.
pub fn frame_time(id: u32, flag: u8, data: &[u8], rate: Bitrate) -> f64 {
    let nominal = rate.nominal.max(1) as f64;
    if flag & flags::ERR != 0 {
        // Error flag + delimiter + intermission.
        return (6.0 * 2.0 + 8.0 + 3.0) / nominal;
    }
    if flag & flags::FD == 0 {
        return classic_frame_bits(id, flag, data) as f64 / nominal;
    }
    let len = data.len();
    let arb = if flag & flags::EXT != 0 { 32 + 17 } else { 13 + 17 } + 10;
    let crc = if len <= 16 { 17 } else { 21 };
    let data_bits = 8 * len + crc + 4 + (8 * len + crc) / 4;
    let data_rate = if flag & flags::BRS != 0 { rate.data.max(1) as f64 } else { nominal };
    arb as f64 / nominal + data_bits as f64 / data_rate
}

/// Timing statistics for one identifier.
#[derive(Clone, Debug, Default)]
pub struct IdStats {
    pub key: u32,
    pub count: usize,
    pub first: f64,
    pub last: f64,
    pub period_mean: f64,
    pub period_min: f64,
    pub period_max: f64,
    /// Standard deviation of the period (jitter).
    pub period_std: f64,
    pub len_min: usize,
    pub len_max: usize,
    /// Index of the most recent frame.
    pub last_index: usize,
    /// Share of the bus time used by this id.
    pub load: f64,
}

impl IdStats {
    pub fn rate(&self) -> f64 {
        if self.period_mean > 0.0 { 1.0 / self.period_mean } else { 0.0 }
    }
}

/// Linux `CAN_ERR_*` error classes (bits of the error frame id).
pub const ERROR_CLASSES: [(u32, &str); 10] = [
    (0x001, "TX timeout"),
    (0x002, "Lost arbitration"),
    (0x004, "Controller"),
    (0x008, "Protocol violation"),
    (0x010, "Transceiver"),
    (0x020, "No ACK"),
    (0x040, "Bus off"),
    (0x080, "Bus error"),
    (0x100, "Restarted"),
    (0x200, "Error counters"),
];

/// Human-readable description of an error frame.
pub fn describe_error(class: u32, data: &[u8]) -> String {
    let mut parts: Vec<String> = ERROR_CLASSES.iter().filter(|(bit, _)| class & bit != 0).map(|(_, n)| n.to_string()).collect();
    if class & 0x004 != 0
        && let Some(&ctrl) = data.get(1)
    {
        for (bit, n) in [
            (0x01, "RX overflow"),
            (0x02, "TX overflow"),
            (0x04, "RX warning"),
            (0x08, "TX warning"),
            (0x10, "RX passive"),
            (0x20, "TX passive"),
            (0x40, "Active"),
        ] {
            if ctrl & bit != 0 {
                parts.push(n.to_string());
            }
        }
    }
    if class & 0x200 != 0 && data.len() >= 8 {
        parts.push(format!("TEC {} REC {}", data[6], data[7]));
    }
    if parts.is_empty() { "Error".into() } else { parts.join(", ") }
}

#[derive(Clone, Debug, Default)]
pub struct BusStats {
    pub frames: usize,
    pub duration: f64,
    /// Fraction of bus time occupied (0..1+).
    pub load: f64,
    pub ext_frames: usize,
    pub fd_frames: usize,
    pub rtr_frames: usize,
    pub error_frames: usize,
    /// Counts per entry of [`ERROR_CLASSES`].
    pub error_classes: [usize; 10],
    pub ids: Vec<IdStats>,
}

impl BusStats {
    pub fn frame_rate(&self) -> f64 {
        if self.duration > 0.0 { self.frames as f64 / self.duration } else { 0.0 }
    }
}

/// Compute statistics over a frame index range. `span` overrides the time
/// span used for rates and load (e.g. a fixed rolling window).
pub fn compute(store: &FrameStore, range: Range<usize>, rate: Bitrate, span: Option<f64>) -> BusStats {
    let mut st = BusStats::default();
    if range.is_empty() {
        return st;
    }
    let t0 = store.ts[range.start];
    let t1 = store.ts[range.end - 1];
    st.duration = span.unwrap_or(t1 - t0);

    struct Acc {
        stats: IdStats,
        prev: f64,
        sum: f64,
        sum_sq: f64,
        busy: f64,
    }
    let mut accs: HashMap<u32, Acc> = HashMap::new();
    let mut busy = 0.0;

    for i in range {
        let (ts, key, fl, len) = (store.ts[i], store.key[i], store.flags[i], store.data(i).len());
        st.frames += 1;
        let ft = frame_time(crate::frame::split_key(key).0, fl, store.data(i), rate);
        busy += ft;
        st.ext_frames += (fl & flags::EXT != 0) as usize;
        st.fd_frames += (fl & flags::FD != 0) as usize;
        st.rtr_frames += (fl & flags::RTR != 0) as usize;
        if is_error_key(key) {
            st.error_frames += 1;
            for (n, (bit, _)) in ERROR_CLASSES.iter().enumerate() {
                if key & bit != 0 {
                    st.error_classes[n] += 1;
                }
            }
        }
        let a = accs.entry(key).or_insert_with(|| Acc {
            stats: IdStats { key, first: ts, period_min: f64::INFINITY, len_min: len, ..Default::default() },
            prev: ts,
            sum: 0.0,
            sum_sq: 0.0,
            busy: 0.0,
        });
        let s = &mut a.stats;
        if s.count > 0 {
            let dt = ts - a.prev;
            a.sum += dt;
            a.sum_sq += dt * dt;
            s.period_min = s.period_min.min(dt);
            s.period_max = s.period_max.max(dt);
        }
        a.prev = ts;
        a.busy += ft;
        s.count += 1;
        s.last = ts;
        s.last_index = i;
        s.len_min = s.len_min.min(len);
        s.len_max = s.len_max.max(len);
    }

    st.load = if st.duration > 0.0 { busy / st.duration } else { 0.0 };
    st.ids = accs
        .into_values()
        .map(|a| {
            let mut s = a.stats;
            let n = (s.count - 1) as f64;
            if n > 0.0 {
                s.period_mean = a.sum / n;
                s.period_std = (a.sum_sq / n - s.period_mean * s.period_mean).max(0.0).sqrt();
            } else {
                s.period_min = 0.0;
            }
            s.load = if st.duration > 0.0 { a.busy / st.duration } else { 0.0 };
            s
        })
        .collect();
    st.ids.sort_by_key(|s| s.key);
    st
}

/// Bus load over time in bins of `bin` seconds: `(bin centre, load fraction)`.
pub fn load_timeline(store: &FrameStore, range: Range<usize>, bin: f64, rate: Bitrate) -> Vec<[f64; 2]> {
    if range.is_empty() || bin <= 0.0 {
        return Vec::new();
    }
    let t0 = store.ts[range.start];
    let mut out: Vec<[f64; 2]> = Vec::new();
    for i in range {
        let b = ((store.ts[i] - t0) / bin) as usize;
        if out.len() <= b {
            out.extend((out.len()..=b).map(|k| [t0 + (k as f64 + 0.5) * bin, 0.0]));
        }
        out[b][1] += frame_time(crate::frame::split_key(store.key[i]).0, store.flags[i], store.data(i), rate) / bin;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Frame;

    #[test]
    fn periodic_stream() {
        let mut store = FrameStore::with_t0(0.0);
        for i in 0..101 {
            let jitter = if i % 2 == 0 { 0.0005 } else { -0.0005 };
            store.push(&Frame { ts: 1.0 + i as f64 * 0.01 + jitter, ..Frame::new(0x100, false, &[0; 8]) });
            if i % 10 == 0 {
                store.push(&Frame { ts: 1.0 + i as f64 * 0.01 + 0.002, ..Frame::new(0x1ABCDEF, true, &[0; 4]) });
            }
        }
        let st = compute(&store, 0..store.len(), Bitrate::default(), None);
        assert_eq!(st.frames, 112);
        let s = st.ids.iter().find(|s| s.key == 0x100).unwrap();
        assert_eq!(s.count, 101);
        assert!((s.period_mean - 0.01).abs() < 1e-6);
        assert!((s.period_std - 0.001).abs() < 1e-5);
        assert!((s.rate() - 100.0).abs() < 0.01);
        assert_eq!(st.ext_frames, 11);
        // 101 classic 8-byte frames at ~0.27 ms each over 1 s ≈ 2.7 % load.
        assert!(st.load > 0.025 && st.load < 0.035, "{}", st.load);
        let tl = load_timeline(&store, 0..store.len(), 0.1, Bitrate::default());
        assert_eq!(tl.len(), 11);
    }

    #[test]
    fn frame_times() {
        let r = Bitrate::default();
        // Classic 8-byte standard frame: 111 + up to 24 stuff bits at 500k.
        // Classic 8-byte standard frame: 111 bits + 0..24 stuff bits.
        let t = frame_time(0x123, 0, &[0x55; 8], r);
        assert!((111.0 / 500e3..=135.0 / 500e3).contains(&t));
        // All-zero payload stuffs heavily; alternating bits do not.
        assert!(classic_frame_bits(0, 0, &[0; 8]) > classic_frame_bits(0x555, 0, &[0x55; 8]));
        assert!((111..=135).contains(&classic_frame_bits(0, 0, &[0; 8])));
        assert!(frame_time(1, flags::FD | flags::BRS, &[0; 64], r) < frame_time(1, flags::FD, &[0; 64], r));
    }

    #[test]
    fn error_description() {
        assert_eq!(describe_error(0x004, &[0, 0x10]), "Controller, RX passive");
        assert_eq!(describe_error(0x040, &[]), "Bus off");
    }
}
