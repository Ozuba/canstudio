//! A single CAN / CAN-FD frame in transit (parsing, live capture, transmit).
//! Long-lived storage uses the columnar [`crate::FrameStore`] instead.

use dbc::EXTENDED_FLAG;

/// Frame flag bits.
pub mod flags {
    /// 29-bit identifier.
    pub const EXT: u8 = 1 << 0;
    /// CAN-FD frame.
    pub const FD: u8 = 1 << 1;
    /// FD bit-rate switch.
    pub const BRS: u8 = 1 << 2;
    /// FD error-state indicator.
    pub const ESI: u8 = 1 << 3;
    /// Remote transmission request.
    pub const RTR: u8 = 1 << 4;
    /// Error frame (id holds the `CAN_ERR_*` class bits).
    pub const ERR: u8 = 1 << 5;
    /// Transmitted by this application.
    pub const TX: u8 = 1 << 6;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// Seconds (absolute UNIX time for live/recorded data).
    pub ts: f64,
    /// Identifier without flags (11 or 29 bit), or error class bits.
    pub id: u32,
    pub flags: u8,
    pub channel: u8,
    pub len: u8,
    pub data: [u8; 64],
}

impl Default for Frame {
    fn default() -> Self {
        Frame { ts: 0.0, id: 0, flags: 0, channel: 0, len: 0, data: [0; 64] }
    }
}

impl Frame {
    pub fn new(id: u32, extended: bool, payload: &[u8]) -> Self {
        let mut f = Frame { id, flags: if extended { flags::EXT } else { 0 }, ..Default::default() };
        f.set_data(payload);
        if payload.len() > 8 {
            f.flags |= flags::FD;
        }
        f
    }

    pub fn set_data(&mut self, payload: &[u8]) {
        let n = payload.len().min(64);
        self.data[..n].copy_from_slice(&payload[..n]);
        self.len = n as u8;
    }

    pub fn data(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }

    /// Lookup key shared with the DBC model: id | [`EXTENDED_FLAG`] when extended.
    pub fn key(&self) -> u32 {
        key(self.id, self.flags)
    }
}

/// Key bit marking error frames, so their class bits never collide with ids.
pub const ERROR_KEY: u32 = 0x2000_0000;

/// Build a lookup key from an id and its flags.
#[inline]
pub fn key(id: u32, f: u8) -> u32 {
    if f & flags::ERR != 0 {
        id | ERROR_KEY
    } else if f & flags::EXT != 0 {
        id | EXTENDED_FLAG
    } else {
        id
    }
}

pub fn is_error_key(key: u32) -> bool {
    key & (ERROR_KEY | EXTENDED_FLAG) == ERROR_KEY
}

/// Split a key into (id, extended).
#[inline]
pub fn split_key(key: u32) -> (u32, bool) {
    if is_error_key(key) {
        return (key & !ERROR_KEY, false);
    }
    (key & !EXTENDED_FLAG, key & EXTENDED_FLAG != 0)
}

/// Format an id the way candump does: 3 hex digits standard, 8 extended.
pub fn fmt_id(id: u32, extended: bool) -> String {
    if extended { format!("{id:08X}") } else { format!("{id:03X}") }
}

pub fn fmt_key(key: u32) -> String {
    if is_error_key(key) {
        return format!("ERR {:X}", key & !ERROR_KEY);
    }
    let (id, ext) = split_key(key);
    fmt_id(id, ext)
}

/// Valid CAN-FD payload lengths; anything else is padded to the next one.
pub const FD_LENGTHS: [usize; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 16, 20, 24, 32, 48, 64];

pub fn fd_len_ceil(n: usize) -> usize {
    FD_LENGTHS.iter().copied().find(|&l| l >= n).unwrap_or(64)
}
