//! Min/max decimation: reduce a visible sample range to at most a few points
//! per horizontal pixel while preserving every peak, so millions of samples
//! draw as fast as a few thousand and still look exact.

use std::ops::Range;

/// Points to draw for samples `range` of (`t`, `v`), visible over `x0..x1`
/// on a plot `px` pixels wide. `step` draws sample-and-hold steps.
/// Non-finite values (NaN/inf from float signals) are skipped.
pub fn decimate(t: &[f64], v: &[f64], range: Range<usize>, x0: f64, x1: f64, px: usize, step: bool) -> Vec<[f64; 2]> {
    let samples = t[range.clone()].iter().zip(&v[range]).map(|(&t, &v)| [t, v]).filter(|p| p[1].is_finite());
    let px = px.max(1);
    if samples.clone().nth(px * 2).is_none() {
        let pts: Vec<[f64; 2]> = samples.collect();
        return if step { steps(&pts) } else { pts };
    }

    let scale = px as f64 / (x1 - x0).max(f64::MIN_POSITIVE);
    let mut out = Vec::with_capacity(px * 4 + 2);
    let mut samples = samples.peekable();
    while let Some(first) = samples.next() {
        let bucket = ((first[0] - x0) * scale).floor();
        let (mut lo, mut hi, mut last) = (first, first, first);
        while let Some(p) = samples.next_if(|p| ((p[0] - x0) * scale).floor() == bucket) {
            if p[1] < lo[1] {
                lo = p;
            }
            if p[1] > hi[1] {
                hi = p;
            }
            last = p;
        }
        // Entry, extremes in time order, exit: keeps edges and peaks intact.
        let (a, b) = if lo[0] <= hi[0] { (lo, hi) } else { (hi, lo) };
        out.extend([first, a, b, last]);
    }
    out.dedup();
    out
}

/// Sample-and-hold rendering: each value holds until the next sample.
fn steps(pts: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut out = Vec::with_capacity(pts.len() * 2);
    for (i, &p) in pts.iter().enumerate() {
        if i > 0 {
            out.push([p[0], pts[i - 1][1]]);
        }
        out.push(p);
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
    fn non_finite_samples_are_dropped() {
        let t = [0.0, 1.0, 2.0, 3.0];
        let v = [1.0, f64::NAN, f64::INFINITY, 2.0];
        for step in [false, true] {
            let out = decimate(&t, &v, 0..4, 0.0, 3.0, 100, step);
            assert!(out.iter().all(|p| p[1].is_finite()), "{out:?}");
        }
        let n = 10_000;
        let t: Vec<f64> = (0..n).map(|i| i as f64).collect();
        let v: Vec<f64> = (0..n).map(|i| if i % 3 == 0 { f64::NAN } else { i as f64 }).collect();
        assert!(decimate(&t, &v, 0..n, 0.0, n as f64, 50, false).iter().all(|p| p[1].is_finite()));
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
