//! Robustness check: mutated candump text must never panic.
//! Run with `cargo run -p canbus --example fuzz_candump -- some.log`.
use std::sync::atomic::AtomicU64;

fn main() {
    let mut seed = 0x2545F4914F6CDD1Du64;
    let mut rng = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let base = std::fs::read_to_string(std::env::args().nth(1).unwrap()).unwrap();
    let lines: Vec<&str> = base.lines().take(5000).collect();
    let alphabet = b"()#R.0123456789ABCDEFabcdef \tcanx_-\n\r\xff\x00";
    let mut text = Vec::new();
    for _ in 0..200_000 {
        let mut line = lines[(rng() as usize) % lines.len()].as_bytes().to_vec();
        for _ in 0..(rng() % 4) {
            let pos = (rng() as usize) % (line.len() + 1);
            match rng() % 3 {
                0 if pos < line.len() => {
                    line.remove(pos);
                }
                1 => line.insert(pos, alphabet[(rng() as usize) % alphabet.len()]),
                _ => line.truncate(pos),
            }
        }
        text.extend_from_slice(&line);
        text.push(b'\n');
    }
    let loaded = canbus::candump::parse_bytes(&text, &AtomicU64::new(0));
    let st = canbus::stats::compute(&loaded.store, 0..loaded.store.len(), Default::default(), None);
    let mut w = canbus::candump::Writer::new(Vec::new());
    for i in 0..loaded.store.len() {
        let f = loaded.store.get(i);
        w.write(&f.to_frame(loaded.store.t0), "can0").unwrap();
    }
    eprintln!("ok: {} frames, {} bad lines, {} ids, load {:.2}", loaded.store.len(), loaded.bad_lines, st.ids.len(), st.load);
}
