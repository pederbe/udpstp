//! The reference's exact one-sided Mann-Whitney calculation. Order counts
//! are integers, including beyond a machine word; only the final ratio is
//! rounded to binary64. No normal approximation or tie correction is used.

use std::cmp::Ordering;

use bench_rounds::num::Num;

use super::Error;

#[derive(Clone, Default)]
struct Count(Vec<u64>);

impl Count {
    fn one() -> Self {
        Self(vec![1])
    }
    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .len()
            .cmp(&other.0.len())
            .then_with(|| self.0.iter().rev().cmp(other.0.iter().rev()))
    }
    fn add(&mut self, other: &Self) {
        self.0.resize(self.0.len().max(other.0.len()), 0);
        let mut carry = false;
        for (i, word) in self.0.iter_mut().enumerate() {
            let (sum, a) = word.overflowing_add(other.0.get(i).copied().unwrap_or(0));
            let (sum, b) = sum.overflowing_add(u64::from(carry));
            *word = sum;
            carry = a || b;
        }
        if carry {
            self.0.push(1);
        }
    }
    fn double(&mut self) {
        let mut carry = 0;
        for word in &mut self.0 {
            let next = *word >> 63;
            *word = (*word << 1) | carry;
            carry = next;
        }
        if carry != 0 {
            self.0.push(carry);
        }
    }
    fn subtract(&mut self, other: &Self) {
        debug_assert!(self.cmp(other) != Ordering::Less);
        let mut borrow = false;
        for (i, word) in self.0.iter_mut().enumerate() {
            let (difference, a) = word.overflowing_sub(other.0.get(i).copied().unwrap_or(0));
            let (difference, b) = difference.overflowing_sub(u64::from(borrow));
            *word = difference;
            borrow = a || b;
        }
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }
    /// Correctly rounded `self / denominator`, for a probability in [0, 1].
    fn probability(&self, denominator: &Self) -> f64 {
        if self.0.is_empty() {
            return 0.0;
        }
        if self.cmp(denominator) == Ordering::Equal {
            return 1.0;
        }
        let mut remainder = self.clone();
        let mut first = None;
        let mut significand = 0u64;
        // A normal number retains 53 bits; subnormal numbers retain the
        // bits through 2^-1074. The following bit and remainder round ties
        // to even, just as Python's division of positive integers does.
        for position in 1..=1075usize {
            remainder.double();
            let bit = remainder.cmp(denominator) != Ordering::Less;
            if bit {
                remainder.subtract(denominator);
            }
            if first.is_none() && bit {
                first = Some(position);
            }
            let last = first.map_or(1074, |p| (p + 52).min(1074));
            if position <= last {
                significand = (significand << 1) | u64::from(bit);
            } else {
                if bit && (!remainder.0.is_empty() || significand & 1 != 0) {
                    significand += 1;
                }
                let Some(first) = first else {
                    return 0.0;
                };
                if first > 1022 {
                    return f64::from_bits(significand);
                }
                let exponent = (1023 - first) as u64;
                return f64::from_bits((exponent << 52) + significand - (1 << 52));
            }
        }
        unreachable!("the guard bit always rounds the probability")
    }
}

/// How likely samples drawn alike would put `xs` at least this far below
/// `ys`. A tie counts as half a pair, exactly as the pre-registered rule.
pub fn lower_p(xs: &[Num], ys: &[Num]) -> Result<f64, Error> {
    let (m, n) = (xs.len(), ys.len());
    let mut twice_u = 0usize;
    for &x in xs {
        for &y in ys {
            twice_u = twice_u
                .checked_add(match x.py_cmp(y) {
                    Some(Ordering::Less) => 2,
                    Some(Ordering::Equal) => 1,
                    _ => 0,
                })
                .ok_or_else(|| Error("too many Mann-Whitney pairs".into()))?;
        }
    }
    // Only the previous i and current row are needed from the reference's
    // three-dimensional recurrence. Addition order cannot affect integers.
    let mut previous = vec![vec![Count::one()]; n + 1];
    for i in 1..=m {
        let mut current = vec![vec![Count::one()]];
        for j in 1..=n {
            let length = i
                .checked_mul(j)
                .and_then(|v| v.checked_add(1))
                .ok_or_else(|| Error("too many Mann-Whitney pairs".into()))?;
            let mut row = vec![Count::default(); length];
            for (k, w) in current[j - 1].iter().enumerate() {
                row[k + i].add(w);
            }
            for (k, w) in previous[j].iter().enumerate() {
                row[k].add(w);
            }
            current.push(row);
        }
        previous = current;
    }
    let mut total = Count::default();
    let mut tail = Count::default();
    for (k, count) in previous[n].iter().enumerate() {
        total.add(count);
        if k >= twice_u.div_ceil(2) {
            tail.add(count);
        }
    }
    Ok(tail.probability(&total))
}
