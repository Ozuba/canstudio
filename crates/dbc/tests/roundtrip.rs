//! Round-trip every DBC in the cantools `dbc_tests` corpus (plus the bundled
//! TeR databases when present): parse -> write -> parse must reproduce the
//! exact same model, and the writer must be idempotent.

use std::fs;
use std::path::{Path, PathBuf};

fn dbc_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<_> =
        rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dbc"))).collect();
    files.sort();
    files
}

#[test]
fn roundtrip_corpus() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = dbc_files(&root.join("dbc_tests"));
    files.extend(dbc_files(&root.join("../../TeR_DATABASES")));
    assert!(!files.is_empty(), "no corpus found");

    let mut failures = Vec::new();
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let (dbc1, errors) = dbc::read_file(path).unwrap();
        if let Some(e) = errors.first() {
            failures.push(format!("{name}: parse error: {e}"));
            continue;
        }
        let out1 = dbc::write(&dbc1);
        let mut dbc2 = match dbc::parse(&out1) {
            Ok(d) => d,
            Err(e) => {
                failures.push(format!("{name}: re-parse error: {e}"));
                continue;
            }
        };
        if dbc1.new_symbols.is_empty() {
            dbc2.new_symbols.clear(); // the writer fills in the default NS_ list
        }
        if dbc1 != dbc2 {
            let (a, b) = (format!("{dbc1:?}"), format!("{dbc2:?}"));
            let at = a.bytes().zip(b.bytes()).position(|(x, y)| x != y).unwrap_or(0);
            let ctx = |s: &str| s.get(at.saturating_sub(60)..(at + 60).min(s.len())).unwrap_or("").to_string();
            failures.push(format!("{name}: model not stable\n    {}\n    {}", ctx(&a), ctx(&b)));
        }
        if out1 != dbc::write(&dbc2) {
            failures.push(format!("{name}: writer not idempotent"));
        }
    }

    eprintln!("checked {} files, {} failures", files.len(), failures.len());
    for f in &failures {
        eprintln!("  FAIL {f}");
    }
    assert!(failures.is_empty(), "{} files failed round-trip", failures.len());
}
