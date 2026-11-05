//! The times the Python tooling writes and reads, as pure functions: from a
//! Unix time as `datetime.fromtimestamp(t, UTC)` rounds it, the log's
//! `HH:MM:SS`, the ledger's month, the Round ID's stamp and `isoformat()`;
//! and the timestamps udpstp, OB-UDPST and the `aws` CLI write, read as
//! the Python reads them, `datetime.fromisoformat(text.replace("Z",
//! "+00:00")).timestamp()`.
//!
//! The calendar is Python's: proleptic Gregorian, years 1 to 9999. The
//! reference is CPython 3.14 on Linux, where the Python tooling runs; on
//! Windows, CPython's `fromtimestamp` refuses times before 1970 and far
//! ahead, which its C library's `gmtime` can't convert.

use std::fmt;

/// A moment in UTC to the microsecond, as a `datetime` in UTC holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Utc {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub micro: u32,
}

const MICROS_PER_DAY: i64 = 86_400_000_000;
/// 0001-01-01T00:00:00 and 9999-12-31T23:59:59.999999, from the epoch.
const FIRST: i64 = -62_135_596_800_000_000;
const LAST: i64 = 253_402_300_799_999_999;

impl Utc {
    /// `datetime.fromtimestamp(t, UTC)`: the seconds, and the fraction
    /// rounded to the microsecond half to even, as CPython rounds it
    /// (`pytime_double_to_denominator`). `None` where Python raises: a
    /// NaN, an infinity, or a year beyond 1 to 9999.
    pub fn from_timestamp(t: f64) -> Option<Utc> {
        let whole = t.trunc();
        let micro = ((t - whole) * 1e6).round_ties_even();
        let (seconds, micro) = if micro >= 1e6 {
            (whole + 1.0, micro - 1e6)
        } else if micro < 0.0 {
            (whole - 1.0, micro + 1e6)
        } else {
            (whole, micro)
        };
        // Beyond ±2^63 s Python raises OverflowError; a NaN fails too.
        if !(-9.223_372_036_854_776e18..9.223_372_036_854_776e18).contains(&seconds) {
            return None;
        }
        let micros = (seconds as i64)
            .checked_mul(1_000_000)?
            .checked_add(micro as i64)?;
        Utc::from_micros(micros)
    }

    /// The moment `micros` microseconds after the epoch; `None` beyond
    /// years 1 to 9999.
    pub fn from_micros(micros: i64) -> Option<Utc> {
        if !(FIRST..=LAST).contains(&micros) {
            return None;
        }
        let (year, month, day) = civil_from_days(micros.div_euclid(MICROS_PER_DAY));
        let of_day = micros.rem_euclid(MICROS_PER_DAY);
        let seconds = of_day / 1_000_000;
        Some(Utc {
            year: year as i32,
            month,
            day,
            hour: (seconds / 3600) as u32,
            minute: (seconds / 60 % 60) as u32,
            second: (seconds % 60) as u32,
            micro: (of_day % 1_000_000) as u32,
        })
    }

    /// `strftime("%H:%M:%S")`: the log's stamp.
    pub fn hms(&self) -> String {
        format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }

    /// `strftime("%Y-%m")`: the month the ledger's cap counts.
    pub fn year_month(&self) -> String {
        format!("{:04}-{:02}", self.year, self.month)
    }

    /// `strftime("%Y%m%dT%H%M%S")`, ISO 8601's basic form to the second:
    /// the Round ID's stamp, after its `r`.
    pub fn basic(&self) -> String {
        format!(
            "{:04}{:02}{:02}T{:02}{:02}{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// `isoformat()`: the microseconds only where there are some, and the
    /// offset `+00:00`.
    pub fn isoformat(&self) -> String {
        self.iso('T')
    }

    fn iso(&self, sep: char) -> String {
        let micro = if self.micro == 0 {
            String::new()
        } else {
            format!(".{:06}", self.micro)
        };
        format!(
            "{:04}-{:02}-{:02}{sep}{:02}:{:02}:{:02}{micro}+00:00",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// `str(datetime)`: `isoformat()` with a space between the date and the
/// time.
impl fmt::Display for Utc {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.iso(' '))
    }
}

/// `datetime.fromisoformat(text.replace("Z", "+00:00"))`, as microseconds
/// since the epoch.
///
/// It reads the forms udpstp, OB-UDPST and the `aws` CLI write, which
/// Python reads: `YYYY-MM-DD`, any one character, `HH:MM:SS`, a fraction
/// after `.` or `,` (the first six digits kept, the rest dropped, never
/// rounded), and a `Z` or a `±HH:MM` offset below 24 hours. It refuses
/// the rest, among them forms Python reads but no producer writes: a time
/// without an offset (which Python reads in the local time zone), an hour
/// 24, the basic forms, other offsets, and the oddities of CPython's
/// parser.
pub fn parse(text: &str) -> Result<i64, BadTime> {
    let text = text.replace('Z', "+00:00");
    let bad = || BadTime(text.clone());
    let chars: Vec<char> = text.chars().collect();
    let number = |at: usize, len: usize| -> Result<i64, BadTime> {
        let digits = chars.get(at..at + len).ok_or_else(bad)?;
        // to_digit(10) takes only ASCII digits, as CPython's parser does.
        digits.iter().try_fold(0, |n, c| {
            c.to_digit(10)
                .map(|d| n * 10 + i64::from(d))
                .ok_or_else(bad)
        })
    };
    let expect = |at: usize, c: char| {
        if chars.get(at) == Some(&c) {
            Ok(())
        } else {
            Err(bad())
        }
    };
    let year = number(0, 4)?;
    expect(4, '-')?;
    let month = number(5, 2)?;
    expect(7, '-')?;
    let day = number(8, 2)?;
    // Position 10 is the separator: any one character.
    let hour = number(11, 2)?;
    expect(13, ':')?;
    let minute = number(14, 2)?;
    expect(16, ':')?;
    let second = number(17, 2)?;
    let mut at = 19;
    let mut micro = 0;
    if matches!(chars.get(at), Some('.' | ',')) {
        let digits = chars[at + 1..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .count();
        if digits == 0 {
            return Err(bad());
        }
        micro = number(at + 1, digits.min(6))? * 10_i64.pow(6 - digits.min(6) as u32);
        at += 1 + digits;
    }
    let sign = match chars.get(at) {
        Some('+') => 1,
        Some('-') => -1,
        _ => return Err(bad()),
    };
    let offset_hours = number(at + 1, 2)?;
    expect(at + 3, ':')?;
    let offset_minutes = number(at + 4, 2)?;
    if chars.len() != at + 6 {
        return Err(bad());
    }
    let in_range = (1..=9999).contains(&year)
        && (1..=12).contains(&month)
        && (1..=days_in_month(year, month)).contains(&day)
        && hour < 24
        && minute < 60
        && second < 60
        && offset_hours < 24
        && offset_minutes < 60;
    if !in_range {
        return Err(bad());
    }
    let seconds = days_from_civil(year, month as u32, day as u32) * 86_400
        + hour * 3600
        + minute * 60
        + second
        - sign * (offset_hours * 3600 + offset_minutes * 60);
    Ok(seconds * 1_000_000 + micro)
}

/// [`parse`], then `.timestamp()`: the seconds since the epoch.
pub fn timestamp(text: &str) -> Result<f64, BadTime> {
    parse(text).map(seconds)
}

/// `timedelta.total_seconds()` of `micros` microseconds: the microseconds
/// over 10^6, correctly rounded, as Python's integer division is.
pub fn seconds(micros: i64) -> f64 {
    let approx = micros as f64 / 1e6;
    // Up to 2^53 the microseconds are a float exactly, and one division
    // rounds once. Beyond (after 2255, before 1685) `micros as f64`
    // rounds already, so the nearest of the floats around the quotient is
    // found exactly.
    if micros.unsigned_abs() <= 1 << 53 {
        return approx;
    }
    let target = i128::from(micros) << 60;
    let distance = |c: f64| (scaled(c) - target).unsigned_abs();
    let mut best = approx;
    for c in [approx.next_down(), approx.next_up()] {
        let (d, b) = (distance(c), distance(best));
        if d < b || (d == b && c.to_bits() & 1 == 0) {
            best = c;
        }
    }
    best
}

/// `c` × 10^6 × 2^60, exactly: for a quotient of more than 2^53
/// microseconds, `c` is at least 2^33, so its last bit is worth at least
/// 2^-19 and every term is an integer that fits.
fn scaled(c: f64) -> i128 {
    let bits = c.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1075;
    let mantissa = i128::from((bits & ((1 << 52) - 1)) | (1 << 52));
    let v = (mantissa * 1_000_000) << (exponent + 60);
    if c < 0.0 { -v } else { v }
}

/// A timestamp [`parse`] doesn't read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BadTime(String);

impl fmt::Display for BadTime {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "not a timestamp the tooling reads: {:?}", self.0)
    }
}

impl std::error::Error for BadTime {}

fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_from_march = i64::from((month + 9) % 12);
    let day_of_year = (153 * month_from_march + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The date `days` days after 1970-01-01 (Howard Hinnant's
/// `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_from_march + 2) / 5 + 1) as u32;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(t: f64) -> Utc {
        Utc::from_timestamp(t).unwrap()
    }

    #[test]
    fn formats_as_the_python_does() {
        let t = utc(1_790_000_000.25);
        assert_eq!(t.hms(), "14:13:20");
        assert_eq!(t.year_month(), "2026-09");
        assert_eq!(t.basic(), "20260921T141320");
        assert_eq!(t.isoformat(), "2026-09-21T14:13:20.250000+00:00");
        assert_eq!(t.to_string(), "2026-09-21 14:13:20.250000+00:00");
        // No microseconds, none written.
        assert_eq!(utc(0.0).isoformat(), "1970-01-01T00:00:00+00:00");
        assert_eq!(utc(-62_135_596_800.0).year_month(), "0001-01");
    }

    #[test]
    fn rounds_to_the_microsecond_half_to_even() {
        // 0.5 µs is a tie in the float: to 0; 1.5 µs to 2; 2.5 µs to 2.
        assert_eq!(utc(0.0000005).micro, 0);
        assert_eq!(utc(0.0000015).micro, 2);
        assert_eq!(utc(0.0000025).micro, 2);
        // Up into the next second.
        assert_eq!(utc(0.9999996), utc(1.0));
        // Before the epoch the fraction counts down from the next second.
        assert_eq!(utc(-0.25).isoformat(), "1969-12-31T23:59:59.750000+00:00");
        assert_eq!(Utc::from_timestamp(f64::NAN), None);
        assert_eq!(Utc::from_timestamp(f64::INFINITY), None);
        assert_eq!(Utc::from_timestamp(253_402_300_800.0), None);
        assert!(Utc::from_timestamp(253_402_300_799.0).is_some());
    }

    #[test]
    fn reads_the_producers_timestamps() {
        // OB-UDPST's and udpstp's BOMTime.
        assert_eq!(
            parse("2026-05-08T14:47:14.950027Z"),
            Ok(1_778_251_634_950_027)
        );
        // The aws CLI's Expiration.
        assert_eq!(
            parse("2026-09-29T14:34:56+00:00"),
            Ok(1_790_692_496_000_000)
        );
        assert_eq!(
            parse("2026-09-29T20:04:56+05:30"),
            parse("2026-09-29T14:34:56Z")
        );
        assert_eq!(
            parse("2026-09-29 14:34:56,5-00:00"),
            Ok(1_790_692_496_500_000)
        );
        // Digits past the sixth are dropped, not rounded.
        assert_eq!(parse("1970-01-01T00:00:00.9999999Z"), Ok(999_999));
        assert_eq!(
            timestamp("2026-05-08T14:47:14.950027Z"),
            Ok(1_778_251_634.950027)
        );
        assert_eq!(parse("2024-02-29T00:00:00Z"), Ok(1_709_164_800_000_000));
    }

    #[test]
    fn refuses_what_python_refuses_and_what_no_producer_writes() {
        for text in [
            // Refused by Python too.
            "2026-02-29T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-09-31T00:00:00Z",
            "0000-01-01T00:00:00Z",
            "2026-09-29T23:59:60Z",
            "2026-09-29T12:00:00+24:00",
            "2026-09-29T12:00:00ZZ",
            " 2026-09-29T12:00:00Z",
            "2026-09-29T12:00:00Z ",
            "2026-9-29T12:00:00Z",
            // Read by Python, written by no producer.
            "2026-09-29T12:00:00",
            "2026-09-29",
            "2026-09-29T24:00:00Z",
            "2026-09-29T12:00Z",
            "20260929T120000Z",
            "2026-09-29T12:00:00+0530",
            "2026-09-29T12:00:00+05:75",
            "2026-09-29T12:00:00.Z",
        ] {
            assert!(parse(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn seconds_divide_correctly_rounded() {
        assert_eq!(seconds(1_500_000), 1.5);
        assert_eq!(seconds(-1), -1e-6);
        // Beyond 2^53, as Python 3.14's int division gives them.
        assert_eq!(seconds((1 << 53) + 1), 9007199254.740993);
        assert_eq!(seconds(253_402_300_799_999_999), 253402300800.0);
        assert_eq!(seconds(-62_135_596_800_000_001), -62135596800.0);
        assert_eq!(seconds(9_007_199_254_740_993_123), 9007199254740.992);
    }

    #[test]
    fn the_calendar_round_trips() {
        for days in -719_162..2_932_897 {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
            assert!(
                (1..=12).contains(&m)
                    && (1..=days_in_month(y, i64::from(m))).contains(&i64::from(d))
            );
        }
    }
}
