//! `candump -l` log format: `(1786124687.075542) can0 123#DEADBEEF`.
//!
//! Supported: standard/extended ids, CAN-FD (`123##<flags><data>`), remote
//! frames (`123#R`, `123#R4`), error frames (8-digit id with `CAN_ERR_FLAG`),
//! micro- or nanosecond timestamps and trailing direction markers. Loading
//! memory-maps the file and parses newline-aligned chunks in parallel.

use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use rayon::prelude::*;

use crate::frame::{Frame, flags, fmt_id, key};
use crate::store::FrameStore;

/// `CAN_ERR_FLAG` as printed in the 8-digit id of error frames.
const CAN_ERR_FLAG: u32 = 0x2000_0000;

/// One parsed line, timestamp still split for precision.
#[derive(Debug, PartialEq)]
pub struct Line<'a> {
    pub secs: i64,
    pub nanos: u32,
    pub iface: &'a str,
    pub frame: Frame,
}

/// Parse one log line. Returns `None` for blank, comment or malformed lines.
pub fn parse_line(line: &[u8]) -> Option<Line<'_>> {
    let line = line.trim_ascii();
    let rest = line.strip_prefix(b"(")?;
    let close = rest.iter().position(|&b| b == b')')?;
    let (secs, nanos) = parse_ts(&rest[..close])?;
    let rest = rest[close + 1..].trim_ascii_start();
    let sp = rest.iter().position(u8::is_ascii_whitespace)?;
    let iface = std::str::from_utf8(&rest[..sp]).ok()?;
    let body = rest[sp..].trim_ascii_start();
    let body = &body[..body.iter().position(u8::is_ascii_whitespace).unwrap_or(body.len())];
    Some(Line { secs, nanos, iface, frame: parse_frame(body)? })
}

/// Parse a cansend-style frame: `123#11223344`, `1FFFFFFF##1AABB`, `123#R`.
pub fn parse_frame(body: &[u8]) -> Option<Frame> {
    let hash = body.iter().position(|&b| b == b'#')?;
    let id_txt = &body[..hash];
    let raw = hex_u32(id_txt)?;
    let mut f = Frame::default();
    if id_txt.len() > 3 {
        if raw & CAN_ERR_FLAG != 0 {
            f.flags |= flags::ERR;
            f.id = raw & !CAN_ERR_FLAG;
        } else {
            f.flags |= flags::EXT;
            f.id = raw & 0x1FFF_FFFF;
        }
    } else {
        f.id = raw;
    }
    let mut data = &body[hash + 1..];
    match data.first() {
        Some(b'#') => {
            f.flags |= flags::FD;
            let fd_flags = hex_nibble(*data.get(1)?)?;
            if fd_flags & 1 != 0 {
                f.flags |= flags::BRS;
            }
            if fd_flags & 2 != 0 {
                f.flags |= flags::ESI;
            }
            data = &data[2..];
        }
        Some(b'R' | b'r') => {
            f.flags |= flags::RTR;
            f.len = data.get(1).and_then(|&d| hex_nibble(d)).unwrap_or(0).min(8);
            return Some(f);
        }
        _ => {}
    }
    let mut n = 0;
    let mut i = 0;
    while i + 1 < data.len() && n < 64 {
        match data[i] {
            b'.' => i += 1,
            b'_' => break, // classic CAN raw DLC suffix
            _ => {
                f.data[n] = (hex_nibble(data[i])? << 4) | hex_nibble(data[i + 1])?;
                n += 1;
                i += 2;
            }
        }
    }
    f.len = n as u8;
    Some(f)
}

fn parse_ts(s: &[u8]) -> Option<(i64, u32)> {
    let dot = s.iter().position(|&b| b == b'.').unwrap_or(s.len());
    let secs = std::str::from_utf8(&s[..dot]).ok()?.parse().ok()?;
    let mut nanos = 0u32;
    let frac = s.get(dot + 1..).unwrap_or(&[]);
    for i in 0..9 {
        let d = match frac.get(i) {
            Some(c) if c.is_ascii_digit() => (c - b'0') as u32,
            Some(_) => return None,
            None => 0,
        };
        nanos = nanos * 10 + d;
    }
    Some((secs, nanos))
}

#[inline]
fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn hex_u32(s: &[u8]) -> Option<u32> {
    if s.is_empty() || s.len() > 8 {
        return None;
    }
    s.iter().try_fold(0u32, |acc, &c| Some((acc << 4) | hex_nibble(c)? as u32))
}

/// Result of loading a log.
pub struct Loaded {
    pub store: FrameStore,
    /// Non-empty lines that could not be parsed.
    pub bad_lines: usize,
}

/// Load a candump log file. `progress` receives the number of bytes parsed.
pub fn load(path: &Path, progress: &AtomicU64) -> io::Result<Loaded> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() == 0 {
        return Ok(Loaded { store: FrameStore::default(), bad_lines: 0 });
    }
    // SAFETY: the log is opened read-only; concurrent truncation by another
    // process is the usual mmap caveat and acceptable for a viewer.
    let map = unsafe { memmap2::Mmap::map(&file)? };
    Ok(parse_bytes(&map, progress))
}

/// Parse a whole log held in memory, in parallel.
pub fn parse_bytes(bytes: &[u8], progress: &AtomicU64) -> Loaded {
    // Time base: the first valid line's whole seconds.
    let base = bytes.split(|&b| b == b'\n').find_map(parse_line).map_or(0, |l| l.secs);

    let chunks = split_chunks(bytes, rayon::current_num_threads() * 4);
    let parts: Vec<(FrameStore, usize)> = chunks
        .par_iter()
        .map(|chunk| {
            let mut store = FrameStore::with_t0(base as f64);
            let mut bad = 0;
            let mut last_iface = ("", 0u8);
            for line in chunk.split(|&b| b == b'\n') {
                match parse_line(line) {
                    Some(l) => {
                        if l.iface != last_iface.0 {
                            last_iface = (l.iface, store.channel_index(l.iface));
                        }
                        let ts = (l.secs - base) as f64 + l.nanos as f64 * 1e-9;
                        let f = &l.frame;
                        store.push_rel(ts, key(f.id, f.flags), f.flags, last_iface.1, f.data());
                    }
                    None if line.trim_ascii().is_empty() => {}
                    None => bad += 1,
                }
            }
            progress.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            (store, bad)
        })
        .collect();

    let mut store = FrameStore::with_t0(base as f64);
    let mut bad_lines = 0;
    for (part, bad) in parts {
        store.extend_from(part);
        bad_lines += bad;
    }
    store.sort_if_needed();
    Loaded { store, bad_lines }
}

/// Split into roughly `n` pieces, each ending on a newline.
fn split_chunks(bytes: &[u8], n: usize) -> Vec<&[u8]> {
    let target = (bytes.len() / n.max(1)).max(1 << 16);
    let mut out = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let cut = if rest.len() <= target {
            rest.len()
        } else {
            rest[target..].iter().position(|&b| b == b'\n').map_or(rest.len(), |p| target + p + 1)
        };
        out.push(&rest[..cut]);
        rest = &rest[cut..];
    }
    out
}

/// Format one frame as a log line (without newline).
pub fn format_line(f: &Frame, iface: &str) -> String {
    let us = (f.ts * 1e6).round() as i64;
    let id = if f.flags & flags::ERR != 0 { format!("{:08X}", f.id | CAN_ERR_FLAG) } else { fmt_id(f.id, f.flags & flags::EXT != 0) };
    let mut s = format!("({}.{:06}) {iface} {id}#", us.div_euclid(1_000_000), us.rem_euclid(1_000_000));
    if f.flags & flags::RTR != 0 {
        s.push('R');
        if f.len > 0 {
            s.push_str(&f.len.to_string());
        }
        return s;
    }
    if f.flags & flags::FD != 0 {
        let fl = (f.flags & flags::BRS != 0) as u8 | ((f.flags & flags::ESI != 0) as u8) << 1;
        s.push('#');
        s.push(char::from_digit(fl as u32, 16).unwrap());
    }
    for b in f.data() {
        s.push_str(&format!("{b:02X}"));
    }
    s
}

/// Streaming candump writer (recording, export).
pub struct Writer<W: Write> {
    out: BufWriter<W>,
}

impl Writer<std::fs::File> {
    pub fn create(path: &Path) -> io::Result<Self> {
        Ok(Writer { out: BufWriter::new(std::fs::File::create(path)?) })
    }
}

impl<W: Write> Writer<W> {
    pub fn new(w: W) -> Self {
        Writer { out: BufWriter::new(w) }
    }

    pub fn write(&mut self, f: &Frame, iface: &str) -> io::Result<()> {
        writeln!(self.out, "{}", format_line(f, iface))
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    pub fn into_inner(self) -> io::Result<W> {
        self.out.into_inner().map_err(|e| e.into_error())
    }
}

/// Write the frames in `range` of a store as a candump log.
pub fn save(store: &FrameStore, range: std::ops::Range<usize>, path: &Path) -> io::Result<()> {
    let mut w = Writer::create(path)?;
    for i in range {
        let f = store.get(i);
        let iface = store.channels.get(f.channel as usize).map_or("can0", String::as_str);
        w.write(&f.to_frame(store.t0), iface)?;
    }
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(s: &str) -> Frame {
        parse_line(s.as_bytes()).unwrap().frame
    }

    #[test]
    fn classic_short_and_extended() {
        let l = parse_line(b"(1786124687.075542) can0 025#231F0003").unwrap();
        assert_eq!((l.secs, l.nanos, l.iface), (1786124687, 75_542_000, "can0"));
        assert_eq!((l.frame.id, l.frame.flags, l.frame.data()), (0x25, 0, &[0x23, 0x1F, 0, 3][..]));
        let f = frame("(1.5) can1 566#0000");
        assert_eq!(f.data(), &[0, 0]);
        let f = frame("(1.5) can1 18FEF100#01");
        assert_eq!((f.id, f.flags), (0x18FEF100, flags::EXT));
        let f = frame("(1.5) can1 123#");
        assert_eq!(f.len, 0);
    }

    #[test]
    fn fd_rtr_err_and_nanos() {
        let f = frame("(0.000000001) vcan0 123##3112233445566778899AA");
        assert_eq!(f.flags, flags::FD | flags::BRS | flags::ESI);
        assert_eq!(f.len, 10);
        let f = frame("(1.0) can0 123#R3");
        assert_eq!((f.flags, f.len), (flags::RTR, 3));
        let f = frame("(1.0) can0 20000080#0000000000000000");
        assert_eq!((f.id, f.flags), (0x80, flags::ERR));
        let l = parse_line(b"(1.123456789) can0 1#00").unwrap();
        assert_eq!(l.nanos, 123_456_789);
        // trailing direction marker
        assert_eq!(frame("(1.0) can0 7FF#01 R").data(), &[1]);
    }

    #[test]
    fn malformed_lines() {
        for l in ["", "garbage", "(x) can0 1#00", "(1.0) can0 XYZ#00", "(1.0) can0 123#0G"] {
            assert!(parse_line(l.as_bytes()).is_none(), "{l}");
        }
    }

    #[test]
    fn write_parse_round_trip() {
        let lines = [
            "(1786124687.075542) can0 025#231F000300000000",
            "(1786124687.075600) can1 18FEF100#01",
            "(1786124687.075700) can0 123##1112233445566778899AABB",
            "(1786124687.075800) can0 123#R2",
            "(1786124687.075900) can0 20000004#0004000000000000",
            "(1786124687.076000) can0 566#",
        ];
        let text = lines.join("\n");
        let progress = AtomicU64::new(0);
        let loaded = parse_bytes(text.as_bytes(), &progress);
        assert_eq!(loaded.bad_lines, 0);
        assert_eq!(loaded.store.len(), lines.len());
        assert_eq!(progress.load(Ordering::Relaxed), text.len() as u64);
        let mut w = Writer::new(Vec::new());
        for i in 0..loaded.store.len() {
            let f = loaded.store.get(i);
            w.write(&f.to_frame(loaded.store.t0), &loaded.store.channels[f.channel as usize]).unwrap();
        }
        let out = String::from_utf8(w.into_inner().unwrap()).unwrap();
        assert_eq!(out.lines().collect::<Vec<_>>(), lines);
    }

    #[test]
    fn parallel_chunks_preserve_order() {
        let text: String = (0..50_000)
            .map(|i| format!("({}.{:06}) can0 {:03X}#{:02X}\n", 1000 + i / 1000, (i % 1000) * 1000, i % 0x7FF, i % 256))
            .collect();
        let loaded = parse_bytes(text.as_bytes(), &AtomicU64::new(0));
        assert_eq!(loaded.store.len(), 50_000);
        assert!(loaded.store.ts.is_sorted());
        assert_eq!(loaded.store.data(49_999), &[(49_999 % 256) as u8]);
    }
}
