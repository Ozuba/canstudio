//! Robustness check: mutate corpus files and parse/write/encode them, looking for panics.
//! Run with `cargo run -p dbc --example fuzz` (debug mode also catches overflows).
use std::panic;

fn rng(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("dbc_tests");
    let files: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| std::fs::read(e.ok()?.path()).ok())
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .collect();
    let tokens = [
        " ",
        ":",
        "|",
        "@",
        "(",
        ")",
        "[",
        "]",
        ",",
        ";",
        "\"",
        "-",
        "+",
        "m",
        "M",
        "0",
        "99999999999999999999",
        "-1",
        "\n",
        "BO_ ",
        " SG_ ",
        "VAL_ ",
        "SG_MUL_VAL_ ",
        "BA_ ",
        "BA_DEF_ ",
        "SIG_VALTYPE_ ",
        "CM_ ",
        "\\",
    ];
    let mut seed = 0x9E3779B97F4A7C15u64;
    let mut panics = 0;
    panic::set_hook(Box::new(|_| {}));
    for iter in 0..20000 {
        let base = &files[(rng(&mut seed) as usize) % files.len()];
        let mut chars: Vec<char> = base.chars().collect();
        for _ in 0..(1 + rng(&mut seed) % 8) {
            if chars.is_empty() {
                break;
            }
            let pos = (rng(&mut seed) as usize) % chars.len();
            match rng(&mut seed) % 3 {
                0 => {
                    chars.remove(pos);
                }
                1 => {
                    let t = tokens[(rng(&mut seed) as usize) % tokens.len()];
                    for (k, c) in t.chars().enumerate() {
                        chars.insert(pos + k, c);
                    }
                }
                _ => {
                    let len = (rng(&mut seed) as usize % 40).min(chars.len() - pos);
                    chars.drain(pos..pos + len);
                }
            }
        }
        let text: String = chars.into_iter().collect();
        let r = panic::catch_unwind(|| {
            let (d, _) = dbc::parse_lossy(&text);
            let out = dbc::write(&d);
            let (d2, _) = dbc::parse_lossy(&out);
            let db = dbc::CompiledDb::new([&d2]);
            for m in &db.messages {
                let data = [0xA5u8; 64];
                for i in 0..m.signals.len() {
                    let _ = m.decode(i, &data);
                }
                let vals: Vec<f64> = m.signals.iter().map(|s| s.max).collect();
                let _ = m.encode(&vals);
            }
        });
        if r.is_err() {
            panics += 1;
            if panics <= 3 {
                let p = std::env::temp_dir().join(format!("dbc_crash_{iter}.dbc"));
                std::fs::write(&p, &text).unwrap();
                eprintln!("panic at iteration {iter}, saved {}", p.display());
            }
        }
    }
    eprintln!("{panics} panics in 20000 mutated inputs");
}
