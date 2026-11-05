//! Numbers as the Python keeps them where a report prints them: [`Num`], an
//! int or a float, and Python's `statistics.median`, `max`, `sum`, `round`
//! and `f` format, so the port's reports come out byte for byte the Python's
//!.
//!
//! The kind matters because a report prints `fmt(median)`: the median of
//! `[1, 2, 3]` is the int `2` and prints `2`, the median of `[1, 3]` is the
//! float `2.0` and prints `2.0` at one digit. And the arithmetic matters
//! because a float sum is compensated: CPython 3.12 and later sum floats
//! with Neumaier's algorithm, so `sum([0.1] * 10)` is exactly `1.0`, where
//! a plain fold gives `0.9999999999999999`. [`sum`] follows CPython 3.14 on
//! Linux, where the Python tooling runs, which also compensates the ints it
//! meets after a float (3.12 and 3.13 add those plainly). On Windows, where
//! a C `long` is 32 bits, CPython adds ints beyond 2^31 another way.
//!
//! Python's ints are unbounded; a [`Num::Int`] holds 128 bits, which every
//! count a file can hold (64 bits at most) and every sum of fewer than 2⁶³
//! of them fits.

use std::cmp::Ordering;

use crate::py::Json;

/// An int or a float, as Python keeps them apart.
///
/// Its `PartialEq` says whether two are the same: the same kind and value,
/// floats bit for bit (all NaNs the same). Python's comparisons, which take
/// `1 == 1.0`, are [`Num::py_cmp`].
#[derive(Clone, Copy, Debug)]
pub enum Num {
    Int(i128),
    Float(f64),
}

impl Num {
    /// A JSON number. A bool isn't one: Python would add `True` as 1, and
    /// the port calls a file with a bool where a number belongs unreadable
    /// (the design's §7.7).
    pub fn from_json(value: &Json) -> Option<Num> {
        match value {
            Json::Int(i) => Some(Num::Int(*i)),
            Json::Float(f) => Some(Num::Float(*f)),
            _ => None,
        }
    }

    /// `float(self)`: an int rounded to the nearest float, ties to even.
    pub fn to_f64(self) -> f64 {
        match self {
            Num::Int(i) => i as f64,
            Num::Float(f) => f,
        }
    }

    /// Python's comparison: exact between an int and a float, as Python
    /// compares them (never through a rounded float), and unordered with a
    /// NaN.
    pub fn py_cmp(self, other: Num) -> Option<Ordering> {
        match (self, other) {
            (Num::Int(a), Num::Int(b)) => Some(a.cmp(&b)),
            (Num::Float(a), Num::Float(b)) => a.partial_cmp(&b),
            (Num::Int(a), Num::Float(b)) => int_float_cmp(a, b),
            (Num::Float(a), Num::Int(b)) => int_float_cmp(b, a).map(Ordering::reverse),
        }
    }

    /// `self + other`: exact for two ints; a float otherwise, the int
    /// rounded to a float first.
    pub fn py_add(self, other: Num) -> Num {
        match (self, other) {
            // Beyond 127 bits, which no sum of counts reaches, Python would
            // go on exactly.
            (Num::Int(a), Num::Int(b)) => Num::Int(a.saturating_add(b)),
            (a, b) => Num::Float(a.to_f64() + b.to_f64()),
        }
    }

    /// `round(self)` without `ndigits` (to an int, half to even), or
    /// `round(self, ndigits)` (a float stays a float, an int stays itself).
    /// `None` where Python raises: a NaN or an infinity to an int, or an int
    /// beyond 127 bits.
    pub fn round(self, ndigits: Option<u32>) -> Option<Num> {
        match (self, ndigits) {
            (Num::Int(_), _) => Some(self),
            (Num::Float(x), Some(n)) => Some(Num::Float(py_round(x, n))),
            (Num::Float(x), None) => {
                let r = x.round_ties_even();
                // Every float in (-2^127, 2^127) that is an integer converts
                // exactly.
                (r.is_finite() && r.abs() < 2f64.powi(127)).then_some(Num::Int(r as i128))
            }
        }
    }
}

impl PartialEq for Num {
    fn eq(&self, other: &Num) -> bool {
        match (self, other) {
            (Num::Int(a), Num::Int(b)) => a == b,
            (Num::Float(a), Num::Float(b)) => {
                a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
            }
            _ => false,
        }
    }
}

/// An int against a float, exactly.
fn int_float_cmp(i: i128, f: f64) -> Option<Ordering> {
    const TWO_127: f64 = 170141183460469231731687303715884105728.0;
    if f.is_nan() {
        return None;
    }
    if f >= TWO_127 {
        return Some(Ordering::Less);
    }
    if f < -TWO_127 {
        return Some(Ordering::Greater);
    }
    // In [-2^127, 2^127), the integer part is an i128 exactly.
    let whole = f.trunc();
    Some(i.cmp(&(whole as i128)).then_with(|| {
        let fraction = f - whole;
        if fraction > 0.0 {
            Ordering::Less
        } else if fraction < 0.0 {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }))
}

/// The order `sorted()` puts numbers in. A NaN, which no rule's input
/// holds, goes last here; Python's place for one is an accident of its
/// sort.
fn sort_order(a: &Num, b: &Num) -> Ordering {
    a.py_cmp(*b).unwrap_or_else(|| {
        let nan = |n: &Num| matches!(n, Num::Float(f) if f.is_nan());
        nan(a).cmp(&nan(b))
    })
}

/// `statistics.median(values)`: the middle value of the sorted values,
/// which keeps its kind, or the mean of the middle two, always a float;
/// `None` for no values (Python raises).
pub fn median(values: &[Num]) -> Option<Num> {
    let mut sorted = values.to_vec();
    // Stable, as sorted() is: of equal values, 2 and 2.0, the first stays
    // first.
    sorted.sort_by(sort_order);
    let n = sorted.len();
    if n == 0 {
        return None;
    }
    if n % 2 == 1 {
        return Some(sorted[n / 2]);
    }
    // `(a + b) / 2`: an int sum is exact, and dividing by 2 after rounding
    // it to a float is the correctly rounded quotient Python's true
    // division gives.
    Some(Num::Float(
        sorted[n / 2 - 1].py_add(sorted[n / 2]).to_f64() / 2.0,
    ))
}

/// `sorted(values)` of floats: stable, in Python's order.
pub(crate) fn sorted_floats(values: &[f64]) -> Vec<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| sort_order(&Num::Float(*a), &Num::Float(*b)));
    sorted
}

/// `max(values)`: the first of the largest, of its own kind; `None` for no
/// values (Python raises, or gives `default`).
pub fn max(values: &[Num]) -> Option<Num> {
    let (first, rest) = values.split_first()?;
    let mut max = *first;
    for &v in rest {
        if v.py_cmp(max) == Some(Ordering::Greater) {
            max = v;
        }
    }
    Some(max)
}

/// `sum(values)`, as CPython 3.14 on Linux adds (`builtin_sum_impl`): ints
/// in a C `long`, 64 bits there, while they fit; from the first float on,
/// Neumaier's compensated sum, the ints it meets rounded to floats and
/// compensated too; and plain additions from an int that doesn't fit a
/// `long` on. On Windows a `long` has 32 bits, so there the plain
/// additions start at 2^31.
pub fn sum(values: &[Num]) -> Num {
    enum State {
        Ints(i64),
        Floats { hi: f64, lo: f64 },
        Plain(Num),
    }
    let mut state = State::Ints(0);
    for &item in values {
        state = match state {
            State::Ints(total) => match item {
                Num::Int(b) => match i64::try_from(b).ok().and_then(|b| total.checked_add(b)) {
                    Some(total) => State::Ints(total),
                    None => State::Plain(Num::Int(total.into()).py_add(item)),
                },
                Num::Float(x) => State::Floats {
                    hi: total as f64 + x,
                    lo: 0.0,
                },
            },
            State::Floats { hi, lo } => {
                let x = item.to_f64();
                let t = hi + x;
                let lo = if hi.abs() >= x.abs() {
                    lo + ((hi - t) + x)
                } else {
                    lo + ((x - t) + hi)
                };
                State::Floats { hi: t, lo }
            }
            State::Plain(total) => State::Plain(total.py_add(item)),
        };
    }
    match state {
        State::Ints(total) => Num::Int(total.into()),
        // The compensation is added only where it can't turn an infinite
        // sum into a NaN, nor lose a negative zero's sign.
        State::Floats { hi, lo } => Num::Float(if lo != 0.0 && lo.is_finite() {
            hi + lo
        } else {
            hi
        }),
        State::Plain(total) => total,
    }
}

/// `round(x, ndigits)` for a float: the nearest float to `x` rounded to
/// `ndigits` decimals, half to even on `x`'s exact binary value, as
/// CPython rounds through its correctly rounded decimal conversion. So
/// `round(2.675, 2)` is `2.67`, since 2.675 is a little under it, and
/// `round(0.125, 2)` is `0.12`.
pub fn py_round(x: f64, ndigits: u32) -> f64 {
    // Beyond 323 digits, Python gives x back: no float has a digit there
    // that rounding could change.
    if !x.is_finite() || ndigits > 323 {
        return x;
    }
    fixed(x, ndigits as usize).parse().unwrap_or(x)
}

/// `format(x, f".{digits}f")`, which `f"{x:.2f}"` writes: `digits`
/// decimals, half to even on the exact binary value; `nan`, `inf` and
/// `-inf` as Python writes them.
pub fn fixed(x: f64, digits: usize) -> String {
    if x.is_nan() {
        "nan".to_string()
    } else if x.is_infinite() {
        if x > 0.0 { "inf" } else { "-inf" }.to_string()
    } else {
        format!("{x:.digits$}")
    }
}

/// The Server-scale report's `fmt(v, digits, unit)`: `–` for none, a
/// float with `digits` decimals, an int as it is, each with `unit` after.
pub fn fmt(v: Option<Num>, digits: usize, unit: &str) -> String {
    match v {
        None => "–".to_string(),
        Some(Num::Float(x)) => format!("{}{unit}", fixed(x, digits)),
        Some(Num::Int(i)) => format!("{i}{unit}"),
    }
}

/// `repr(x)` for a float, which `str(x)`, an f-string's `{x}` and
/// `json.dumps` write too: the shortest digits that read back as `x`, laid
/// out as Python lays them out. Positional from `0.0001` up to 16 digits
/// before the point, always with a digit after it (`2.0`); in exponent
/// form elsewhere, with a sign and at least two digits (`1e-05`,
/// `1.5e+16`); `nan`, `inf` and `-inf`.
pub fn repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_string();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    // Rust's `{:e}` writes the shortest digits that read back as `x`, the
    // nearest of them to it, as Python's repr chooses them (David Gay's
    // mode 0): "1.2345e-7", "0e0".
    let scientific = format!("{x:e}");
    let (mantissa, exponent) = scientific.split_once('e').expect("{:e} writes an exponent");
    let exponent: i32 = exponent.parse().expect("{:e} writes an integer exponent");
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(m) => ("-", m),
        None => ("", mantissa),
    };
    let mut digits: String = mantissa.chars().filter(|&c| c != '.').collect();
    // Where the point goes: the value is 0.DIGITS × 10^point.
    let mut point = exponent + 1;
    if let Some((even, even_point)) = tie_to_even(x.abs(), digits.len()) {
        (digits, point) = (even, even_point);
    }
    let n = digits.len() as i32;
    let body = if point <= -4 || point > 16 {
        let (first, rest) = digits.split_at(1);
        let rest = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        let e = point - 1;
        let e_sign = if e < 0 { '-' } else { '+' };
        format!("{first}{rest}e{e_sign}{:02}", e.unsigned_abs())
    } else if point <= 0 {
        format!("0.{}{digits}", "0".repeat(point.unsigned_abs() as usize))
    } else if point >= n {
        format!("{digits}{}.0", "0".repeat((point - n) as usize))
    } else {
        let (whole, fraction) = digits.split_at(point as usize);
        format!("{whole}.{fraction}")
    };
    format!("{sign}{body}")
}

/// Where `x`, positive, lies exactly halfway between two decimals of `n`
/// digits that both read back as `x`: the one whose last digit is even,
/// with its point. Of two shortest candidates equally near, David Gay's
/// dtoa, which Python's `repr` uses, keeps the even one; Rust's shortest
/// formatting rounds up. So `1164944778159002.25` is `1164944778159002.2`
/// in Python, `…002.3` in Rust.
fn tie_to_even(x: f64, n: usize) -> Option<(String, i32)> {
    // A tie needs x to be exactly a decimal of n + 1 digits ending in 5:
    // its correctly rounded digits end so first, then its exact ones,
    // which a double's never outnumber 800, stop there.
    let rounded = format!("{x:.*e}", n);
    if !rounded.split_once('e')?.0.ends_with('5') {
        return None;
    }
    let exact = format!("{x:.800e}");
    let (mantissa, exponent) = exact.split_once('e')?;
    let digits: String = mantissa.chars().filter(|&c| c != '.').collect();
    let digits = digits.trim_end_matches('0');
    let point = exponent.parse::<i32>().ok()? + 1;
    if digits.len() != n + 1 || !digits.ends_with('5') {
        return None;
    }
    // The two candidates: the digits below, and those one above.
    let below = &digits[..n];
    let even = if (below.as_bytes()[n - 1] - b'0').is_multiple_of(2) {
        (below.to_string(), point)
    } else {
        let mut up = below.as_bytes().to_vec();
        let mut at = n;
        loop {
            if at == 0 {
                up.insert(0, b'1');
                break;
            }
            at -= 1;
            if up[at] == b'9' {
                up[at] = b'0';
            } else {
                up[at] += 1;
                break;
            }
        }
        let carried = i32::from(up.len() > n);
        up.truncate(n);
        (String::from_utf8(up).ok()?, point + carried)
    };
    let (digits, point) = (even.0.trim_end_matches('0').to_string(), even.1);
    // Both read back as x at a tie; checked all the same.
    let back: f64 = format!("0.{digits}e{point}").parse().ok()?;
    (back == x).then_some((digits, point))
}

/// `max(a, b)` of two floats, as Python's two-argument `max` takes it:
/// `b` only where it is greater than `a`. So of equals the first stays
/// (`max(0.0, -0.0)` is `0.0`, `max(-0.0, 0.0)` is `-0.0`), and a NaN
/// stays where it comes first and is passed over where it comes second.
/// `f64::max` differs on both.
pub fn max2(a: f64, b: f64) -> f64 {
    if b > a { b } else { a }
}

impl Num {
    /// `self / other`, Python's true division: a float, correctly rounded,
    /// also between two ints. `None` where Python raises
    /// (`ZeroDivisionError`), and where the port can't promise Python's
    /// rounding: an int beyond 2^53, which no count a Round reads reaches.
    pub fn py_div(self, other: Num) -> Option<f64> {
        const EXACT: i128 = 1 << 53;
        let exact = |n: Num| match n {
            Num::Int(i) => (-EXACT..=EXACT).contains(&i).then_some(i as f64),
            Num::Float(f) => Some(f),
        };
        let (a, b) = (exact(self)?, exact(other)?);
        // Both are exact floats, so one IEEE division rounds once, as
        // Python's division of two ints does.
        (b != 0.0).then(|| a / b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Num::{Float, Int};

    #[test]
    fn median_keeps_the_middle_values_kind() {
        assert_eq!(median(&[Int(1), Int(2), Int(3)]), Some(Int(2)));
        assert_eq!(median(&[Int(1), Int(3)]), Some(Float(2.0)));
        assert_eq!(median(&[Int(3), Float(1.0), Int(2)]), Some(Int(2)));
        // Equal values keep their order, as sorted() is stable.
        assert_eq!(median(&[Int(2), Float(2.0), Int(1)]), Some(Int(2)));
        assert_eq!(median(&[Float(2.0), Int(2), Int(1)]), Some(Float(2.0)));
        assert_eq!(median(&[Float(-0.0), Float(0.0), Int(5)]), Some(Float(0.0)));
        assert_eq!(
            median(&[Float(0.0), Float(-0.0), Int(5)]),
            Some(Float(-0.0))
        );
        assert_eq!(median(&[]), None);
    }

    #[test]
    fn max_keeps_the_first_of_the_largest() {
        assert_eq!(max(&[Int(1), Float(2.0), Int(2)]), Some(Float(2.0)));
        assert_eq!(max(&[Int(2), Float(2.0)]), Some(Int(2)));
        assert_eq!(max(&[]), None);
        // Compared exactly: 2^53 + 1 is more than the float 2^53.
        let big = (1i128 << 53) + 1;
        assert_eq!(max(&[Float(2f64.powi(53)), Int(big)]), Some(Int(big)));
    }

    #[test]
    fn sum_compensates_floats_as_cpython_does() {
        assert_eq!(sum(&[]), Int(0));
        assert_eq!(sum(&[Int(1), Int(2)]), Int(3));
        assert_eq!(sum(&[Int(1), Float(2.5)]), Float(3.5));
        assert_eq!(sum(&[Float(0.1); 10]), Float(1.0));
        assert_eq!(sum(&[Float(1e100), Float(1.0), Float(-1e100)]), Float(1.0));
        // CPython 3.14 compensates an int after a float too.
        assert_eq!(sum(&[Float(1e100), Int(1), Float(-1e100)]), Float(1.0));
        assert_eq!(sum(&[Float(-0.0)]), Float(0.0));
        assert_eq!(
            sum(&[Float(f64::INFINITY), Float(1.0)]),
            Float(f64::INFINITY)
        );
        // An int beyond a word ends the fast paths: plain additions after.
        let over = i128::from(u64::MAX);
        assert_eq!(sum(&[Int(over), Int(1)]), Int(over + 1));
        assert_eq!(
            sum(&[Int(over), Float(0.1), Float(0.1), Float(0.1)]),
            Float(over as f64 + 0.1 + 0.1 + 0.1)
        );
    }

    #[test]
    fn round_and_fixed_round_half_to_even_on_the_binary_value() {
        assert_eq!(py_round(2.675, 2), 2.67);
        assert_eq!(py_round(0.125, 2), 0.12);
        assert_eq!(py_round(0.375, 2), 0.38);
        assert_eq!(py_round(2.5, 0), 2.0);
        assert_eq!(py_round(-0.001, 2).to_bits(), (-0.0f64).to_bits());
        assert_eq!(py_round(5e-324, 400), 5e-324);
        assert_eq!(Float(2.5).round(None), Some(Int(2)));
        assert_eq!(Float(3.5).round(None), Some(Int(4)));
        assert_eq!(Float(-2.5).round(None), Some(Int(-2)));
        assert_eq!(Float(f64::NAN).round(None), None);
        assert_eq!(Int(7).round(Some(2)), Some(Int(7)));
        assert_eq!(fixed(0.125, 2), "0.12");
        assert_eq!(fixed(-0.0, 1), "-0.0");
        assert_eq!(fixed(2.0, 0), "2");
        assert_eq!(fixed(1e16, 1), "10000000000000000.0");
        assert_eq!(fixed(f64::NAN, 1), "nan");
        assert_eq!(fixed(f64::NEG_INFINITY, 1), "-inf");
    }

    #[test]
    fn fmt_prints_each_kind_as_the_report_does() {
        assert_eq!(fmt(None, 1, ""), "–");
        assert_eq!(fmt(Some(Int(3)), 2, " %"), "3 %");
        assert_eq!(fmt(Some(Float(3.0)), 2, " %"), "3.00 %");
        assert_eq!(fmt(Some(Float(0.05)), 1, ""), "0.1");
    }

    #[test]
    fn repr_writes_floats_as_python_does() {
        for (x, text) in [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (2.0, "2.0"),
            (0.1, "0.1"),
            (0.1 + 0.2, "0.30000000000000004"),
            (1.5, "1.5"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (0.000123, "0.000123"),
            (1.23e-5, "1.23e-05"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (9999999999999998.0, "9999999999999998.0"),
            (123456789012345678.0, "1.2345678901234568e+17"),
            (1790000000.1234567, "1790000000.1234567"),
            (1e22, "1e+22"),
            (1e100, "1e+100"),
            (f64::MAX, "1.7976931348623157e+308"),
            (5e-324, "5e-324"),
            (-2.5e-7, "-2.5e-07"),
            // Halfway between two shortest candidates, found by the
            // differential harness: Python keeps the even last digit.
            (1_164_944_778_159_002.0 + 0.25, "1164944778159002.2"),
            (1_993_638_082.0 + 109.0 / 256.0, "1993638082.4257812"),
            (-(1_993_638_082.0 + 109.0 / 256.0), "-1993638082.4257812"),
            (f64::NAN, "nan"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
        ] {
            assert_eq!(repr(x), text, "{x:e}");
        }
    }

    #[test]
    fn max2_keeps_the_first_of_equals_as_python_does() {
        assert_eq!(max2(0.0, -0.0).to_bits(), 0.0f64.to_bits());
        assert_eq!(max2(-0.0, 0.0).to_bits(), (-0.0f64).to_bits());
        assert!(max2(f64::NAN, 1.0).is_nan());
        assert_eq!(max2(1.0, f64::NAN), 1.0);
        assert_eq!(max2(1.0, 2.0), 2.0);
    }

    #[test]
    fn py_div_divides_as_python_does() {
        assert_eq!(Int(1).py_div(Int(3)), Some(1.0 / 3.0));
        assert_eq!(
            Int(0).py_div(Int(-5)).map(f64::to_bits),
            Some((-0.0f64).to_bits())
        );
        assert_eq!(Int(7).py_div(Float(2.0)), Some(3.5));
        assert_eq!(Int(1).py_div(Int(0)), None);
        assert_eq!(Float(1.0).py_div(Float(-0.0)), None);
        assert_eq!(Int((1 << 53) + 1).py_div(Int(1)), None);
    }

    #[test]
    fn compares_ints_and_floats_exactly() {
        let two_53 = 1i128 << 53;
        assert_eq!(
            Int(two_53 + 1).py_cmp(Float(2f64.powi(53))),
            Some(Ordering::Greater)
        );
        assert_eq!(
            Int(two_53).py_cmp(Float(2f64.powi(53))),
            Some(Ordering::Equal)
        );
        assert_eq!(Int(-3).py_cmp(Float(-2.5)), Some(Ordering::Less));
        assert_eq!(Int(-2).py_cmp(Float(-2.5)), Some(Ordering::Greater));
        assert_eq!(
            Int(i128::MAX).py_cmp(Float(f64::INFINITY)),
            Some(Ordering::Less)
        );
        assert_eq!(Int(0).py_cmp(Float(f64::NAN)), None);
        assert_eq!(Float(-0.0).py_cmp(Int(0)), Some(Ordering::Equal));
        assert_ne!(Float(-0.0), Float(0.0));
    }
}
