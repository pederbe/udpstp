//! The median, percentile and quantile the Server-scale rules were written
//! with (`scripts/bench/scale_report.py:157-172`), over floats. The median
//! that keeps an int's kind, for a figure a report prints, is
//! [`num::median`].
//!
//! Each does its arithmetic as the Python does, in the same order, and
//! takes an index as Python takes one: truncated toward zero, and counted
//! from the end when negative.

use crate::num::{self, Num};

/// `statistics.median(values)` over floats, as the Python's `median` takes
/// it once its `None`s are gone: the middle of the sorted values, or the
/// mean of the middle two; `None` for no values.
pub fn median(values: &[f64]) -> Option<f64> {
    let values: Vec<Num> = values.iter().map(|&v| Num::Float(v)).collect();
    num::median(&values).map(Num::to_f64)
}

/// `percentile(values, p)`: of the sorted values, the one at
/// `int(p / 100 * n)`, the last at most; `None` for no values, or where
/// Python raises.
pub fn percentile(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let sorted = num::sorted_floats(values);
    let at = (p / 100.0 * sorted.len() as f64).trunc();
    // int() of a NaN or an infinity raises, before min() is reached.
    if !at.is_finite() {
        return None;
    }
    item(&sorted, at.min((sorted.len() - 1) as f64))
}

/// `quantile(ordered, q)`: the value at `q`, from 0 to 1, of a list the
/// caller sorted, by the nearest rank: `ordered[int(q * (n - 1) + 0.5)]`;
/// `None` where Python raises (no values, or an index beyond them).
pub fn quantile(ordered: &[f64], q: f64) -> Option<f64> {
    let at = (q * (ordered.len() as f64 - 1.0) + 0.5).trunc();
    item(ordered, at)
}

/// `values[int(at)]`, with `at` already truncated: from the end when
/// negative, `None` where Python raises (a NaN, an infinity, or an index
/// beyond the list).
fn item(values: &[f64], at: f64) -> Option<f64> {
    if !at.is_finite() {
        return None;
    }
    let len = values.len() as f64;
    let at = if at < 0.0 { at + len } else { at };
    // A whole number in [0, len) converts exactly.
    (0.0..len).contains(&at).then(|| values[at as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_is_the_middle_or_the_mean_of_the_middle_two() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[1.0, 2.0]), Some(1.5));
        // Stable: of -0.0 and 0.0, equal, the first stays first.
        assert_eq!(
            median(&[0.0, -0.0, 5.0]).map(f64::to_bits),
            Some((-0.0f64).to_bits())
        );
    }

    #[test]
    fn percentile_truncates_and_counts_back_from_the_end() {
        let values = [5.0, 1.0, 3.0, 2.0, 4.0];
        assert_eq!(percentile(&[], 50.0), None);
        // int(0.5 * 5) = 2, int(0.99 * 5) = 4, min(4, 5) = 4.
        assert_eq!(percentile(&values, 50.0), Some(3.0));
        assert_eq!(percentile(&values, 99.0), Some(5.0));
        assert_eq!(percentile(&values, 100.0), Some(5.0));
        assert_eq!(percentile(&values, 0.0), Some(1.0));
        // int(-0.5) = 0; int(-1.5) = -1, the last; int(-10) is beyond.
        assert_eq!(percentile(&values, -10.0), Some(1.0));
        assert_eq!(percentile(&values, -30.0), Some(5.0));
        assert_eq!(percentile(&values, -200.0), None);
        assert_eq!(percentile(&values, f64::NAN), None);
    }

    #[test]
    fn quantile_takes_the_nearest_rank() {
        let ordered = [1.0, 2.0, 3.0, 4.0, 5.0];
        // int(0.25 * 4 + 0.5) = 1, int(0.75 * 4 + 0.5) = 3, int(4.5) = 4.
        assert_eq!(quantile(&ordered, 0.25), Some(2.0));
        assert_eq!(quantile(&ordered, 0.75), Some(4.0));
        assert_eq!(quantile(&ordered, 0.0), Some(1.0));
        assert_eq!(quantile(&ordered, 1.0), Some(5.0));
        assert_eq!(quantile(&ordered, 1.5), None);
        // int(-3.5) = -3: the third from the end.
        assert_eq!(quantile(&ordered, -1.0), Some(3.0));
        assert_eq!(quantile(&[], 0.5), None);
    }
}
