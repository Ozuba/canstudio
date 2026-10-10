//! CSV export of plotted signals.

use std::io::Write;

use super::Pane;
use crate::session::Session;

/// Export every plotted signal in `[t0, t1]`.
///
/// * wide: one row per sample time of any signal, each column holding the
///   signal's last value (sample and hold); empty before its first sample.
/// * long: `time,signal,value` with the raw samples.
pub fn csv(s: &mut Session, panes: &[Pane], t0: f64, t1: f64, wide: bool) {
    let Some(path) = crate::util::dialog::save("signals.csv", "csv") else { return };
    let mut cols = Vec::new();
    for t in panes.iter().flat_map(|p| &p.traces) {
        if let Some(r) = t.resolved(s)
            && !cols.iter().any(|(r2, _)| *r2 == r)
        {
            cols.push((r, format!("{}.{}", t.msg, t.sig)));
        }
    }
    let series: Vec<(Vec<f64>, Vec<f64>)> = cols
        .iter()
        .map(|(r, _)| {
            let se = s.series(*r);
            let rng = se.t.partition_point(|&t| t < t0)..se.t.partition_point(|&t| t <= t1);
            (se.t[rng.clone()].to_vec(), se.v[rng].to_vec())
        })
        .collect();

    let result = (|| -> std::io::Result<usize> {
        let mut w = std::io::BufWriter::new(std::fs::File::create(&path)?);
        let mut rows = 0;
        if wide {
            write!(w, "time")?;
            for (_, name) in &cols {
                write!(w, ",{name}")?;
            }
            writeln!(w)?;
            let mut times: Vec<f64> = series.iter().flat_map(|(t, _)| t.iter().copied()).collect();
            times.sort_by(f64::total_cmp);
            times.dedup();
            let mut cursor = vec![0usize; series.len()];
            for &t in &times {
                write!(w, "{t:.6}")?;
                for (k, (ts, vs)) in series.iter().enumerate() {
                    while cursor[k] < ts.len() && ts[cursor[k]] <= t {
                        cursor[k] += 1;
                    }
                    match cursor[k].checked_sub(1) {
                        Some(i) => write!(w, ",{}", vs[i])?,
                        None => write!(w, ",")?,
                    }
                }
                writeln!(w)?;
                rows += 1;
            }
        } else {
            writeln!(w, "time,signal,value")?;
            for ((_, name), (ts, vs)) in cols.iter().zip(&series) {
                for (t, v) in ts.iter().zip(vs) {
                    writeln!(w, "{t:.6},{name},{v}")?;
                    rows += 1;
                }
            }
        }
        w.flush()?;
        Ok(rows)
    })();
    match result {
        Ok(rows) => s.info(format!("Exported {rows} rows to {}", path.display())),
        Err(e) => s.error(format!("Export failed: {e}")),
    }
}
