//! Fast signal decoding and encoding against one or more compiled databases.
//!
//! [`CompiledDb`] flattens the editable [`Dbc`] model into lookup-friendly
//! structures: every signal gets a precomputed [`BitLayout`] so decoding a
//! frame is a handful of shifts, and multiplexing conditions are resolved to
//! signal indices. Frame ids use the same convention as the DBC: the raw id
//! with [`EXTENDED_FLAG`] set for 29-bit frames.

use std::collections::HashMap;

use crate::model::*;

/// Where a signal's bits live in the payload, precomputed from the DBC
/// start-bit/size/byte-order triple. Supports payloads up to 64 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BitLayout {
    /// First payload byte touched.
    lo: u8,
    /// Number of payload bytes touched (at most 9 for a 64 bit signal).
    bytes: u8,
    /// Right shift applied to the accumulated bytes.
    shift: u8,
    size: u8,
    big_endian: bool,
}

impl BitLayout {
    pub fn new(start_bit: u64, size: u64, order: ByteOrder) -> Option<Self> {
        if size == 0 || size > 64 {
            return None;
        }
        let (lo, hi, shift) = match order {
            ByteOrder::LittleEndian => {
                let end = start_bit + size - 1;
                (start_bit / 8, end / 8, start_bit % 8)
            }
            ByteOrder::BigEndian => {
                // DBC Motorola start bit is the MSB in "sawtooth" numbering;
                // map it to a linear MSB-first index and walk forward.
                let msb = (start_bit / 8) * 8 + 7 - start_bit % 8;
                let lsb = msb + size - 1;
                (msb / 8, lsb / 8, 7 - lsb % 8)
            }
        };
        (hi < 64).then(|| BitLayout {
            lo: lo as u8,
            bytes: (hi - lo + 1) as u8,
            shift: shift as u8,
            size: size as u8,
            big_endian: order == ByteOrder::BigEndian,
        })
    }

    /// Minimum payload length needed to hold this signal.
    pub fn required_len(&self) -> usize {
        self.lo as usize + self.bytes as usize
    }

    fn mask(&self) -> u64 {
        if self.size == 64 { u64::MAX } else { (1u64 << self.size) - 1 }
    }

    fn window(&self, data: &[u8]) -> Option<u128> {
        let bytes = data.get(self.lo as usize..self.required_len())?;
        Some(if self.big_endian {
            bytes.iter().fold(0u128, |acc, &b| (acc << 8) | b as u128)
        } else {
            bytes.iter().rev().fold(0u128, |acc, &b| (acc << 8) | b as u128)
        })
    }

    /// Extract the raw bits, or `None` if the payload is too short.
    #[inline]
    pub fn extract(&self, data: &[u8]) -> Option<u64> {
        Some((self.window(data)? >> self.shift) as u64 & self.mask())
    }

    /// Write raw bits into `data` (which must be long enough).
    pub fn insert(&self, data: &mut [u8], raw: u64) {
        let Some(mut acc) = self.window(data) else { return };
        let mask = (self.mask() as u128) << self.shift;
        acc = (acc & !mask) | (((raw & self.mask()) as u128) << self.shift);
        let n = self.bytes as usize;
        for i in 0..n {
            let byte_shift = if self.big_endian { (n - 1 - i) * 8 } else { i * 8 };
            data[self.lo as usize + i] = (acc >> byte_shift) as u8;
        }
    }
}

/// Condition under which a multiplexed signal is present.
#[derive(Clone, Debug)]
pub struct MuxCond {
    /// Index of the switch signal in the same message.
    pub switch: usize,
    /// Inclusive raw-value ranges of the switch.
    pub ranges: Vec<(u64, u64)>,
}

#[derive(Clone, Debug)]
pub struct CompiledSignal {
    pub name: String,
    pub layout: Option<BitLayout>,
    pub value_type: ValueType,
    pub factor: f64,
    pub offset: f64,
    pub min: f64,
    pub max: f64,
    pub unit: String,
    pub mux: Option<MuxCond>,
    pub is_switch: bool,
    /// Value table sorted by raw value.
    pub labels: Vec<(i64, String)>,
}

impl CompiledSignal {
    fn new(s: &Signal) -> Self {
        let mut labels = s.value_descriptions.clone();
        labels.sort_by_key(|(v, _)| *v);
        CompiledSignal {
            name: s.name.clone(),
            layout: BitLayout::new(s.start_bit, s.size, s.byte_order),
            value_type: s.value_type,
            factor: s.factor,
            offset: s.offset,
            min: s.min,
            max: s.max,
            unit: s.unit.clone(),
            mux: None,
            is_switch: s.multiplexer.is_switch(),
            labels,
        }
    }

    pub fn size(&self) -> u32 {
        self.layout.map_or(0, |l| l.size as u32)
    }

    pub fn is_enum(&self) -> bool {
        !self.labels.is_empty()
    }

    /// Raw bits to physical value.
    #[inline]
    pub fn to_phys(&self, raw: u64) -> f64 {
        let size = self.size();
        let v = match self.value_type {
            ValueType::Unsigned => raw as f64,
            ValueType::Signed => {
                let unused = 64 - size;
                ((raw << unused) as i64 >> unused) as f64
            }
            ValueType::Float32 => f32::from_bits(raw as u32) as f64,
            ValueType::Float64 => f64::from_bits(raw),
        };
        v * self.factor + self.offset
    }

    /// Physical value to raw bits, rounding and saturating to the signal width.
    pub fn to_raw(&self, phys: f64) -> u64 {
        let size = self.size();
        match self.value_type {
            ValueType::Float32 => (((phys - self.offset) / self.factor) as f32).to_bits() as u64,
            ValueType::Float64 => ((phys - self.offset) / self.factor).to_bits(),
            ValueType::Unsigned => {
                let max = if size >= 64 { u64::MAX as f64 } else { ((1u64 << size) - 1) as f64 };
                ((phys - self.offset) / self.factor).round().clamp(0.0, max) as u64
            }
            ValueType::Signed => {
                let half = (1i128 << (size.max(1) - 1)) as f64;
                let v = ((phys - self.offset) / self.factor).round().clamp(-half, half - 1.0) as i64;
                v as u64
            }
        }
    }

    /// Integer value used for value-table lookups.
    pub fn table_key(&self, phys: f64) -> i64 {
        ((phys - self.offset) / self.factor).round() as i64
    }

    /// Value-table label for a physical value.
    pub fn label(&self, phys: f64) -> Option<&str> {
        let key = self.table_key(phys);
        self.labels.binary_search_by_key(&key, |(v, _)| *v).ok().map(|i| self.labels[i].1.as_str())
    }

    /// Physical value formatted with its label or unit.
    pub fn format(&self, phys: f64) -> String {
        match self.label(phys) {
            Some(l) => format!("{l} ({})", self.table_key(phys)),
            None if self.unit.is_empty() => format_num(phys),
            None => format!("{} {}", format_num(phys), self.unit),
        }
    }
}

/// Compact numeric formatting: integers without decimals, else up to 6 significant digits.
pub fn format_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[derive(Clone, Debug)]
pub struct CompiledMessage {
    /// Index of the source database and of the message within it.
    pub db: usize,
    pub index: usize,
    /// Frame key: raw id, with [`EXTENDED_FLAG`] for extended frames.
    pub id: u32,
    pub name: String,
    pub size: usize,
    pub transmitter: String,
    pub cycle_time_ms: Option<f64>,
    pub signals: Vec<CompiledSignal>,
}

impl CompiledMessage {
    fn new(db_idx: usize, index: usize, dbc: &Dbc, m: &Message) -> Self {
        let mut signals: Vec<CompiledSignal> = m.signals.iter().map(CompiledSignal::new).collect();
        let plain_switch = m.signals.iter().position(|s| s.multiplexer == Multiplexer::Multiplexor);
        for (i, s) in m.signals.iter().enumerate() {
            signals[i].mux = match (&s.ext_mux, s.multiplexer.mux_value()) {
                (Some(e), _) => {
                    m.signals.iter().position(|x| x.name == e.switch).map(|sw| MuxCond { switch: sw, ranges: e.ranges.clone() })
                }
                (None, Some(v)) => plain_switch
                    .or_else(|| m.signals.iter().position(|x| x.multiplexer.is_switch() && x.name != s.name))
                    .map(|sw| MuxCond { switch: sw, ranges: vec![(v, v)] }),
                (None, None) => None,
            };
        }
        CompiledMessage {
            db: db_idx,
            index,
            id: m.id,
            name: m.name.clone(),
            size: m.size as usize,
            transmitter: m.transmitter.clone(),
            cycle_time_ms: dbc.cycle_time_ms(m),
            signals,
        }
    }

    pub fn signal_index(&self, name: &str) -> Option<usize> {
        self.signals.iter().position(|s| s.name == name)
    }

    /// Whether signal `idx` is present in this payload (multiplexing chain satisfied).
    pub fn is_active(&self, idx: usize, data: &[u8]) -> bool {
        let mut cur = idx;
        // Bounded walk up the switch chain; guards against cyclic definitions.
        for _ in 0..8 {
            let Some(cond) = &self.signals[cur].mux else { return true };
            let sw = &self.signals[cond.switch];
            let Some(raw) = sw.layout.and_then(|l| l.extract(data)) else { return false };
            if !cond.ranges.iter().any(|&(lo, hi)| (lo..=hi).contains(&raw)) {
                return false;
            }
            cur = cond.switch;
        }
        false
    }

    /// Decode one signal; `None` if absent from this payload.
    #[inline]
    pub fn decode(&self, idx: usize, data: &[u8]) -> Option<f64> {
        let s = &self.signals[idx];
        let raw = s.layout?.extract(data)?;
        self.is_active(idx, data).then(|| s.to_phys(raw))
    }

    /// Decode every present signal.
    pub fn decode_all<'a>(&'a self, data: &'a [u8]) -> impl Iterator<Item = (usize, f64)> + 'a {
        (0..self.signals.len()).filter_map(move |i| Some((i, self.decode(i, data)?)))
    }

    /// Encode physical values (one per signal, in signal order) into a payload.
    /// Signals whose multiplex condition is not met by the switch values are skipped.
    pub fn encode(&self, values: &[f64]) -> Vec<u8> {
        let mut data = vec![0u8; self.size.max(self.required_len())];
        // Switches first so multiplexed signals can test their condition.
        let order = (0..self.signals.len())
            .filter(|&i| self.signals[i].is_switch || self.signals[i].mux.is_none())
            .chain((0..self.signals.len()).filter(|&i| !self.signals[i].is_switch && self.signals[i].mux.is_some()));
        for i in order {
            let s = &self.signals[i];
            let (Some(layout), Some(&v)) = (s.layout, values.get(i)) else { continue };
            if self.is_active(i, &data) {
                layout.insert(&mut data, s.to_raw(v));
            }
        }
        data
    }

    fn required_len(&self) -> usize {
        self.signals.iter().filter_map(|s| s.layout).map(|l| l.required_len()).max().unwrap_or(0)
    }
}

/// Address of a signal inside a [`CompiledDb`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SignalRef {
    pub msg: usize,
    pub sig: usize,
}

/// One or more databases merged for decoding. The first database to define an id wins.
#[derive(Clone, Debug, Default)]
pub struct CompiledDb {
    pub messages: Vec<CompiledMessage>,
    by_id: HashMap<u32, usize>,
    /// Ids defined in more than one database (later definitions ignored).
    pub conflicts: Vec<u32>,
}

impl CompiledDb {
    pub fn new<'a>(dbs: impl IntoIterator<Item = &'a Dbc>) -> Self {
        let mut db = CompiledDb::default();
        for (di, dbc) in dbs.into_iter().enumerate() {
            for (mi, m) in dbc.messages.iter().enumerate() {
                if db.by_id.contains_key(&m.id) {
                    db.conflicts.push(m.id);
                    continue;
                }
                db.by_id.insert(m.id, db.messages.len());
                db.messages.push(CompiledMessage::new(di, mi, dbc, m));
            }
        }
        db
    }

    /// Message index for a frame key (raw id | [`EXTENDED_FLAG`] if extended).
    #[inline]
    pub fn lookup(&self, key: u32) -> Option<usize> {
        self.by_id.get(&key).copied()
    }

    pub fn message(&self, key: u32) -> Option<&CompiledMessage> {
        self.lookup(key).map(|i| &self.messages[i])
    }

    pub fn signal(&self, r: SignalRef) -> &CompiledSignal {
        &self.messages[r.msg].signals[r.sig]
    }

    /// Resolve a `Message.Signal` path (stable across recompiles).
    pub fn find(&self, msg: &str, sig: &str) -> Option<SignalRef> {
        let m = self.messages.iter().position(|m| m.name == msg)?;
        Some(SignalRef { msg: m, sig: self.messages[m].signal_index(sig)? })
    }

    pub fn path(&self, r: SignalRef) -> String {
        let m = &self.messages[r.msg];
        format!("{}.{}", m.name, m.signals[r.sig].name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn db(src: &str) -> CompiledDb {
        CompiledDb::new([&parse(src).unwrap()])
    }

    #[test]
    fn little_endian_unsigned_and_signed() {
        let db = db(r#"
BO_ 100 M: 8 X
 SG_ A : 0|8@1+ (1,0) [0|0] "" X
 SG_ B : 8|12@1- (0.5,-1) [0|0] "" X
 SG_ C : 20|4@1+ (1,0) [0|0] "" X
"#);
        let m = db.message(100).unwrap();
        // B = 0xFFF (-1) -> -1*0.5-1 = -1.5 ; C = 0xA
        let data = [0x12, 0xFF, 0xAF, 0, 0, 0, 0, 0];
        assert_eq!(m.decode(0, &data), Some(0x12 as f64));
        assert_eq!(m.decode(1, &data), Some(-1.5));
        assert_eq!(m.decode(2, &data), Some(10.0));
    }

    #[test]
    fn motorola_matches_cantools() {
        // From cantools motohawk.dbc: Temperature 0|12@0- (0.01,250), AverageRadius 6|6@0+ (0.1,0), Enable 7|1@0+.
        let db = db(r#"
BO_ 496 ExampleMessage: 8 PCM1
 SG_ Enable : 7|1@0+ (1,0) [0|0] "-" Vector__XXX
 SG_ AverageRadius : 6|6@0+ (0.1,0) [0|5] "m" Vector__XXX
 SG_ Temperature : 0|12@0- (0.01,250) [229.53|270.47] "degK" Vector__XXX
"#);
        let m = db.message(496).unwrap();
        // `cantools decode motohawk.dbc` on 1F0#C006E00000000000:
        // Enable=1, AverageRadius=3.2, Temperature=250.55.
        let data = [0xc0, 0x06, 0xe0, 0, 0, 0, 0, 0];
        assert_eq!(m.decode(0, &data), Some(1.0));
        assert!((m.decode(1, &data).unwrap() - 3.2).abs() < 1e-9);
        assert!((m.decode(2, &data).unwrap() - 250.55).abs() < 1e-9);
        assert_eq!(m.encode(&[1.0, 3.2, 250.55]), data);
    }

    #[test]
    fn motorola_cross_byte_layout() {
        // 16-bit Motorola signal with MSB at bit 7: bytes 0..2 big-endian.
        let l = BitLayout::new(7, 16, ByteOrder::BigEndian).unwrap();
        assert_eq!(l.extract(&[0x12, 0x34]), Some(0x1234));
        // 12-bit Motorola starting mid-byte (start 3): bits 3..0 of byte 0 then byte 1.
        let l = BitLayout::new(3, 12, ByteOrder::BigEndian).unwrap();
        assert_eq!(l.extract(&[0xAB, 0xCD]), Some(0xBCD));
        let mut d = [0u8; 2];
        l.insert(&mut d, 0xBCD);
        assert_eq!(d, [0x0B, 0xCD]);
    }

    #[test]
    fn short_frames_skip_signals() {
        let db = db(r#"
BO_ 1 M: 8 X
 SG_ A : 0|8@1+ (1,0) [0|0] "" X
 SG_ B : 56|8@1+ (1,0) [0|0] "" X
"#);
        let m = db.message(1).unwrap();
        assert_eq!(m.decode(0, &[5, 6]), Some(5.0));
        assert_eq!(m.decode(1, &[5, 6]), None);
    }

    #[test]
    fn floats() {
        let db = db(r#"
BO_ 2 F: 8 X
 SG_ A : 0|32@1- (1,0) [0|0] "" X
 SG_ D : 0|64@1- (1,0) [0|0] "" X
SIG_VALTYPE_ 2 A : 1;
SIG_VALTYPE_ 2 D : 2;
"#);
        let m = db.message(2).unwrap();
        let mut data = 1.5f32.to_bits().to_le_bytes().to_vec();
        data.extend([0; 4]);
        assert_eq!(m.decode(0, &data), Some(1.5));
        let data = (-2.25f64).to_bits().to_le_bytes();
        assert_eq!(m.decode(1, &data), Some(-2.25));
    }

    #[test]
    fn simple_and_extended_mux() {
        let db = db(r#"
BO_ 3 M: 8 X
 SG_ S M : 0|8@1+ (1,0) [0|0] "" X
 SG_ A m1 : 8|8@1+ (1,0) [0|0] "" X
 SG_ B m2M : 8|8@1+ (1,0) [0|0] "" X
 SG_ C m5 : 16|8@1+ (1,0) [0|0] "" X
SG_MUL_VAL_ 3 C B 5-7;
"#);
        let m = db.message(3).unwrap();
        assert_eq!(m.decode(1, &[1, 9, 0]), Some(9.0));
        assert_eq!(m.decode(2, &[1, 9, 0]), None);
        assert_eq!(m.decode(2, &[2, 6, 4]), Some(6.0));
        assert_eq!(m.decode(3, &[2, 6, 4]), Some(4.0));
        assert_eq!(m.decode(3, &[2, 9, 4]), None); // B out of range
        assert_eq!(m.decode(3, &[1, 6, 4]), None); // B itself inactive
        // Mux values beyond 255 (as in ALL.dbc).
        let db = super::tests::db(
            r#"
BO_ 4 M: 8 X
 SG_ S M : 0|16@1+ (1,0) [0|0] "" X
 SG_ A m505 : 16|8@1+ (1,0) [0|0] "" X
"#,
        );
        let m = db.message(4).unwrap();
        assert_eq!(m.decode(1, &[0xF9, 0x01, 7]), Some(7.0));
        assert_eq!(m.encode(&[505.0, 7.0]), vec![0xF9, 0x01, 7, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn fd_payload_and_extended_ids() {
        let db = db(r#"
BO_ 2147483905 FD: 64 X
 SG_ Last : 504|8@1+ (1,0) [0|0] "" X
"#);
        assert!(db.message(0x101).is_none());
        let m = db.message(0x101 | EXTENDED_FLAG).unwrap();
        let mut data = [0u8; 64];
        data[63] = 42;
        assert_eq!(m.decode(0, &data), Some(42.0));
    }

    #[test]
    fn labels_and_saturation() {
        let db = db(r#"
BO_ 5 E: 1 X
 SG_ State : 0|3@1+ (1,0) [0|0] "" X
VAL_ 5 State 1 "INIT" 2 "POST" ;
"#);
        let s = &db.message(5).unwrap().signals[0];
        assert_eq!(s.label(2.0), Some("POST"));
        assert_eq!(s.format(1.0), "INIT (1)");
        assert_eq!(s.to_raw(100.0), 7);
    }
}
