//! Min/max decimation: reduce a visible sample range to at most a few points
//! per horizontal pixel while preserving every peak, so millions of samples
//! draw as fast as a few thousand and still look exact.

use std::ops::Range;

/// Points to draw for samples `range` of (`t`, `v`), visible over `x0..x1`
/// on a plot `px` pixels wide. `step` draws sample-and-hold steps.
pub fn decimate(t: &[f64], v: &[f64], range: Range<usize>, x0: f64, x1: f64, px: usize, step: bool) -> Vec<[f64; 2]> {
    let (t, v) = (&t[range.clone()], &v[range]);
    let px = px.max(1);
    if t.len() <= px * 2 {
        return if step { steps(t, v) } else { t.iter().zip(v).map(|(&t, &v)| [t, v]).collect() };
    }

    let scale = px as f64 / (x1 - x0).max(f64::MIN_POSITIVE);
    let mut out = Vec::with_capacity(px * 4 + 2);
    let mut i = 0;
    while i < t.len() {
        let bucket = ((t[i] - x0) * scale).floor();
        let (start, first) = (i, v[i]);
        let (mut lo, mut hi) = ((t[i], v[i]), (t[i], v[i]));
        i += 1;
        while i < t.len() && ((t[i] - x0) * scale).floor() == bucket {
            if v[i] < lo.1 {
                lo = (t[i], v[i]);
            }
            if v[i] > hi.1 {
                hi = (t[i], v[i]);
            }
            i += 1;
        }
        let last = (t[i - 1], v[i - 1]);
        // Entry, extremes in time order, exit: keeps edges and peaks intact.
        out.push([t[start], first]);
        let (a, b) = if lo.0 <= hi.0 { (lo, hi) } else { (hi, lo) };
        out.push([a.0, a.1]);
        out.push([b.0, b.1]);
        out.push([last.0, last.1]);
    }
    out.dedup();
    out
}

/// Sample-and-hold rendering: each value holds until the next sample.
fn steps(t: &[f64], v: &[f64]) -> Vec<[f64; 2]> {
    let mut out = Vec::with_capacity(t.len() * 2);
    for i in 0..t.len() {
        if i > 0 {
            out.push([t[i], v[i - 1]]);
        }
        out.push([t[i], v[i]]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_ranges_pass_through() {
        let t = [0.0, 1.0, 2.0];
        let v = [1.0, 2.0, 3.0];
        assert_eq!(decimate(&t, &v, 0..3, 0.0, 2.0, 100, false).len(), 3);
        assert_eq!(decimate(&t, &v, 0..3, 0.0, 2.0, 100, true), vec![[0.0, 1.0], [1.0, 1.0], [1.0, 2.0], [2.0, 2.0], [2.0, 3.0]]);
    }

    #[test]
    fn keeps_peaks_and_bounds_size() {
        let n = 1_000_000;
        let t: Vec<f64> = (0..n).map(|i| i as f64 * 1e-3).collect();
        let mut v: Vec<f64> = (0..n).map(|i| (i as f64 * 1e-3).sin()).collect();
        v[123_456] = 50.0;
        v[654_321] = -50.0;
        let out = decimate(&t, &v, 0..n, 0.0, 1000.0, 800, false);
        assert!(out.len() <= 800 * 4 + 8, "{}", out.len());
        assert!(out.iter().any(|p| p[1] == 50.0));
        assert!(out.iter().any(|p| p[1] == -50.0));
        assert!(out.windows(2).all(|w| w[0][0] <= w[1][0]), "time order");
    }
}
