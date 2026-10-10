//! Load a candump log, report load speed, and print decoded frames in a
//! `cantools decode --single-line`-like format for cross-checking.
//!
//! Usage: cargo run --release --example decode -- <db.dbc> <log> [frames]

use std::sync::atomic::AtomicU64;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (dbc, errors) = dbc::read_file(args[1].as_ref()).expect("read dbc");
    for e in &errors {
        eprintln!("dbc: {e}");
    }
    let db = dbc::CompiledDb::new([&dbc]);
    let t = Instant::now();
    let loaded = canbus::candump::load(args[2].as_ref(), &AtomicU64::new(0)).expect("load log");
    let store = loaded.store;
    eprintln!(
        "loaded {} frames ({} bad lines, {} ids, {:.0} MB) in {:.2?}",
        store.len(),
        loaded.bad_lines,
        store.keys.len(),
        store.memory_bytes() as f64 / 1e6,
        t.elapsed()
    );
    let t = Instant::now();
    let mut cache = canbus::SeriesCache::default();
    let mut samples = 0;
    for (mi, m) in db.messages.iter().enumerate() {
        for si in 0..m.signals.len() {
            samples += cache.get(&store, &db, dbc::SignalRef { msg: mi, sig: si }).len();
        }
    }
    eprintln!("decoded {samples} samples of every signal in {:.2?}", t.elapsed());

    let n: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
    for i in 0..n.min(store.len()) {
        let f = store.get(i);
        let Some(m) = db.message(f.key) else { continue };
        let sigs: Vec<String> =
            m.decode_all(f.data).map(|(si, v)| format!("{}: {}", m.signals[si].name, dbc::codec::format_num(v))).collect();
        println!("{:.6} {} {}", f.ts + store.t0, m.name, sigs.join(", "));
    }
}
