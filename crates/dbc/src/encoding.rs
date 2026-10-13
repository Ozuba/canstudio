//! Text encoding of DBC files. Vector tools and cantools use Windows-1252, so
//! that is the default; files that are valid UTF-8 with non-ASCII text are
//! kept as UTF-8. A file is always written back in the encoding it was read in.

/// Character encoding of a DBC file on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Encoding {
    #[default]
    Cp1252,
    Utf8,
}

impl Encoding {
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Cp1252 => "Windows-1252",
            Encoding::Utf8 => "UTF-8",
        }
    }
}

/// Windows-1252 code points for bytes 0x80..=0x9F. The five bytes the code page
/// leaves undefined map to the same C1 control code point, so they round-trip.
const CP1252_HIGH: [char; 32] = [
    '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}', '\u{02C6}', '\u{2030}', '\u{0160}',
    '\u{2039}', '\u{0152}', '\u{008D}', '\u{017D}', '\u{008F}', '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}',
    '\u{2013}', '\u{2014}', '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
];

/// Decode file bytes, detecting the encoding.
pub fn decode(bytes: &[u8]) -> (String, Encoding) {
    if !bytes.is_ascii()
        && let Ok(s) = std::str::from_utf8(bytes)
    {
        return (s.to_string(), Encoding::Utf8);
    }
    let text = bytes.iter().map(|&b| if (0x80..0xA0).contains(&b) { CP1252_HIGH[(b - 0x80) as usize] } else { b as char }).collect();
    (text, Encoding::Cp1252)
}

/// Encode text for writing. Characters Windows-1252 cannot represent become `?`.
pub fn encode(text: &str, encoding: Encoding) -> Vec<u8> {
    match encoding {
        Encoding::Utf8 => text.as_bytes().to_vec(),
        Encoding::Cp1252 => text
            .chars()
            .map(|c| match c as u32 {
                0..0x80 | 0xA0..0x100 => c as u8,
                _ => CP1252_HIGH.iter().position(|&h| h == c).map_or(b'?', |i| 0x80 + i as u8),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_and_round_trips() {
        assert_eq!(decode(b"plain ascii").1, Encoding::Cp1252);
        assert_eq!(decode("360°".as_bytes()), ("360°".to_string(), Encoding::Utf8));
        // 0x80 is the euro sign in Windows-1252 (Latin-1 would give a control code).
        assert_eq!(decode(b"\x80 \xB0 \xE4"), ("€ ° ä".to_string(), Encoding::Cp1252));
        let all: Vec<u8> = (0..=255).collect();
        let (text, enc) = decode(&all);
        assert_eq!(enc, Encoding::Cp1252);
        assert_eq!(encode(&text, enc), all, "every byte must round-trip");
        assert_eq!(encode("€ 日", Encoding::Cp1252), b"\x80 ?");
    }
}
