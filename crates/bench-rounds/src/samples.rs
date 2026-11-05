//! What a host's samples say about a Round Run's window, as the judging
//! reads them (`scripts/bench/scale_report.py:263-321`): the logical CPUs
//! busy, the busiest one, the softirq and irq parts; the sampled
//! process's threads and their wake-ups; and what other processes used.
//!
//! The samples are `udpstp-bench sample`'s JSON lines, each read into a
//! [`Json`]. Their format matches the retained independent reference. A
//! sample's `cpus` keep `/proc/stat`'s order, as a Python dict does, and
//! the busy shares are added in that order with a plain `+=`, so the sum
//! comes out as the Python's to the bit (a sorted map would put CPU `10`
//! before `2`).
//!
//! What the Python raises on, a sample of the wrong shape, is a
//! [`PyError`]. Where it computes something anyway from a garbled sample
//! (a `true` counted as 1, a float among the counters), the port refuses
//! it ([`PyErrorKind::Refused`]), as the design allows
//!.

use std::cmp::Ordering;

use indexmap::IndexMap;

use crate::num::{self, Num};
use crate::py::{Json, PyError, PyErrorKind};

/// The unit of `/proc`'s CPU times on Linux (`scale_report.py`'s
/// `USER_HZ`).
pub const USER_HZ: i128 = 100;

/// `window(samples, t0, t1)`: the first and last samples whose `t` is in
/// `[t0, t1]`, if there are two or more. A sample without a `t` has one of
/// 0, as `.get("t", 0)` gives it.
pub fn window(samples: &[Json], t0: f64, t1: f64) -> Result<Option<(&Json, &Json)>, PyError> {
    let mut inside = Vec::new();
    for sample in samples {
        let t = sample.get_or("t", &Json::Int(0))?;
        let t = number(t, "a sample's time")?;
        // `t0 <= t <= t1`, compared exactly as Python compares an int and
        // a float.
        let after = matches!(
            Num::Float(t0).py_cmp(t),
            Some(Ordering::Less | Ordering::Equal)
        );
        if after
            && matches!(
                t.py_cmp(Num::Float(t1)),
                Some(Ordering::Less | Ordering::Equal)
            )
        {
            inside.push(sample);
        }
    }
    Ok((inside.len() > 1).then(|| (inside[0], inside[inside.len() - 1])))
}

/// Logical CPUs busy over a window, and how.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuUse {
    /// The busy shares of the logical CPUs, added: how many were busy.
    pub busy: f64,
    /// The busiest one's share.
    pub busiest: f64,
    pub softirq: f64,
    pub irq: f64,
    /// How many logical CPUs counted.
    pub cpus: i128,
}

impl CpuUse {
    /// The dict the Python's record holds: `busy`, `busiest`, `softirq`,
    /// `irq` and `cpus`, in that order.
    pub fn to_json(&self) -> Json {
        Json::Dict(IndexMap::from([
            ("busy".to_string(), Json::Float(self.busy)),
            ("busiest".to_string(), Json::Float(self.busiest)),
            ("softirq".to_string(), Json::Float(self.softirq)),
            ("irq".to_string(), Json::Float(self.irq)),
            ("cpus".to_string(), Json::Int(self.cpus)),
        ]))
    }
}

/// `cpu_use(samples, t0, t1, only)`: over the window, each logical CPU's
/// busy share (its time less idle and iowait, over its time), added in the
/// first sample's order; the busiest; the softirq and irq shares; and how
/// many counted. With `only`, those logical CPUs alone. `None` without a
/// window.
pub fn cpu_use(
    samples: &[Json],
    t0: f64,
    t1: f64,
    only: Option<&[i128]>,
) -> Result<Option<CpuUse>, PyError> {
    let Some((first, last)) = window(samples, t0, t1)? else {
        return Ok(None);
    };
    let mut total = 0.0;
    let mut busiest = 0.0;
    let mut soft = 0.0;
    let mut irq = 0.0;
    let mut cpus = 0;
    let first_cpus = first.item("cpus")?;
    let Json::Dict(first_cpus) = first_cpus else {
        return Err(refused(format!(
            "a sample's cpus are a {}, not a dict",
            first_cpus.type_name()
        )));
    };
    for (cpu, a) in first_cpus {
        // Read for each logical CPU, as the Python reads it: a first
        // sample without any never looks.
        let b = last.item("cpus")?.get(cpu)?;
        if !b.truthy() {
            continue;
        }
        if let Some(only) = only {
            let number: i128 = cpu
                .parse()
                .ok()
                .filter(|_| cpu.bytes().all(|c| c.is_ascii_digit()))
                .ok_or_else(|| refused(format!("the logical CPU {cpu:?}")))?;
            if !only.contains(&number) {
                continue;
            }
        }
        // `[y - x for x, y in zip(a, b)]`, the counters' rise, over the
        // counters zip() pairs.
        let d = rises(a, b)?;
        let span = num::sum(&d.iter().map(|&v| Num::Int(v)).collect::<Vec<_>>());
        let Num::Int(span) = span else {
            unreachable!("a sum of ints is an int");
        };
        if span == 0 {
            continue;
        }
        let at = |i: usize| {
            d.get(i).copied().ok_or_else(|| {
                PyError::new(PyErrorKind::IndexError, "list index out of range".into())
            })
        };
        let share = divide(span - at(3)? - at(4)?, span)?;
        total += share;
        busiest = num::max2(busiest, share);
        irq += divide(at(5)?, span)?;
        soft += divide(at(6)?, span)?;
        cpus += 1;
    }
    Ok(Some(CpuUse {
        busy: total,
        busiest,
        softirq: soft,
        irq,
        cpus,
    }))
}

/// The sampled process's threads over a window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wakeups {
    /// Its threads in the last sample.
    pub threads: usize,
    /// Their voluntary context switches a second, `None` where the window
    /// took no time.
    pub wakeups_per_s: Option<f64>,
}

impl Wakeups {
    /// The dict the Python's record holds: `threads`, `wakeups_per_s`.
    pub fn to_json(&self) -> Json {
        Json::Dict(IndexMap::from([
            ("threads".to_string(), Json::Int(self.threads as i128)),
            (
                "wakeups_per_s".to_string(),
                self.wakeups_per_s.map_or(Json::Null, Json::Float),
            ),
        ]))
    }
}

/// `wakeups(samples, t0, t1)`: the sampled process's threads in the last
/// sample, and the voluntary context switches a second of those also in
/// the first; `None` without a window.
pub fn wakeups(samples: &[Json], t0: f64, t1: f64) -> Result<Option<Wakeups>, PyError> {
    let Some((first, last)) = window(samples, t0, t1)? else {
        return Ok(None);
    };
    let dt = time_between(first, last)?;
    let none = Json::Dict(IndexMap::new());
    let last_threads = dict(last.get_or("threads", &none)?, "a sample's threads")?;
    let mut switches = Vec::new();
    for (tid, b) in last_threads {
        // `tid in first.get("threads", {})`, read for each thread of the
        // last sample, as the Python reads it.
        let first_threads = match first.get_or("threads", &none)? {
            Json::Dict(d) => d,
            Json::List(_) | Json::Str(_) => {
                return Err(refused("a sample's threads aren't a dict".into()));
            }
            other => {
                return Err(PyError::new(
                    PyErrorKind::TypeError,
                    format!("argument of type '{}' is not iterable", other.type_name()),
                ));
            }
        };
        if let Some(a) = first_threads.get(tid) {
            let (b, a) = (int(b.at(2)?)?, int(a.at(2)?)?);
            switches.push(Num::Int(b - a));
        }
    }
    let switches = num::sum(&switches);
    let wakeups_per_s = if truthy(dt) {
        Some(
            switches
                .py_div(dt)
                .ok_or_else(|| refused("a window beyond the port's exact division".into()))?,
        )
    } else {
        None
    };
    Ok(Some(Wakeups {
        threads: last_threads.len(),
        wakeups_per_s,
    }))
}

/// `other_cpus(samples, t0, t1)`: the logical CPUs of the host that
/// neither the sampled process nor interrupts used over the window, at
/// least 0; `None` without a window, or where a sample lacks the process's
/// time, or the window took no time. A lower bound, since a softirq's time
/// can count in both.
pub fn other_cpus(samples: &[Json], t0: f64, t1: f64) -> Result<Option<f64>, PyError> {
    let pair = window(samples, t0, t1)?;
    let usage = cpu_use(samples, t0, t1, None)?;
    let (Some((first, last)), Some(usage)) = (pair, usage) else {
        return Ok(None);
    };
    let has_process = |s: &Json| s.as_dict().is_some_and(|d| d.contains_key("process"));
    if !has_process(first) || !has_process(last) {
        return Ok(None);
    }
    let dt = time_between(first, last)?;
    if matches!(
        dt.py_cmp(Num::Int(0)),
        Some(Ordering::Less | Ordering::Equal)
    ) {
        return Ok(None);
    }
    let process = int(last.item("process")?)? - int(first.item("process")?)?;
    let process = Num::Float(divide(process, USER_HZ)?)
        .py_div(dt)
        .ok_or_else(|| refused("a window beyond the port's exact division".into()))?;
    Ok(Some(num::max2(
        0.0,
        usage.busy - process - usage.softirq - usage.irq,
    )))
}

/// `last["t"] - first["t"]`, keeping Python's kinds.
fn time_between(first: &Json, last: &Json) -> Result<Num, PyError> {
    let a = number(first.item("t")?, "a sample's time")?;
    let b = number(last.item("t")?, "a sample's time")?;
    Ok(match (b, a) {
        (Num::Int(b), Num::Int(a)) => Num::Int(b - a),
        (b, a) => Num::Float(b.to_f64() - a.to_f64()),
    })
}

fn truthy(n: Num) -> bool {
    match n {
        Num::Int(i) => i != 0,
        Num::Float(f) => f != 0.0,
    }
}

/// `a / b` of two counts, as Python divides two ints.
fn divide(a: i128, b: i128) -> Result<f64, PyError> {
    Num::Int(a)
        .py_div(Num::Int(b))
        .ok_or_else(|| refused(format!("{a} / {b}, beyond the counts a host writes")))
}

/// A number where one belongs: an int or a float; a bool, which Python
/// would take as one, refused as garbled.
fn number(value: &Json, what: &str) -> Result<Num, PyError> {
    match value {
        Json::Bool(_) => Err(refused(format!("{what} is a bool"))),
        other => Num::from_json(other).ok_or_else(|| {
            PyError::new(
                PyErrorKind::TypeError,
                format!("{what} is a {}, not a number", other.type_name()),
            )
        }),
    }
}

/// A counter: an int. A float or a bool, which Python would compute with,
/// refused as garbled; anything else fails as the Python's subtraction
/// does.
fn int(value: &Json) -> Result<i128, PyError> {
    match value {
        Json::Int(i) => Ok(*i),
        Json::Float(_) | Json::Bool(_) => Err(refused(format!(
            "a counter is a {}, not an int",
            value.type_name()
        ))),
        other => Err(PyError::new(
            PyErrorKind::TypeError,
            format!("a counter is a {}, not an int", other.type_name()),
        )),
    }
}

/// How far a logical CPU's counters rose from `a` to `b`, two lists of
/// ints, each counter with its namesake until either list ends.
fn rises(a: &Json, b: &Json) -> Result<Vec<i128>, PyError> {
    fn list(v: &Json) -> Result<&[Json], PyError> {
        v.as_list().ok_or_else(|| {
            refused(format!(
                "a logical CPU's counters are a {}, not a list",
                v.type_name()
            ))
        })
    }
    let (a, b) = (list(a)?, list(b)?);
    a.iter()
        .zip(b)
        .map(|(x, y)| Ok(int(y)? - int(x)?))
        .collect()
}

fn dict<'a>(value: &'a Json, what: &str) -> Result<&'a IndexMap<String, Json>, PyError> {
    value.as_dict().ok_or_else(|| {
        PyError::new(
            PyErrorKind::AttributeError,
            format!(
                "{what}: '{}' object has no attribute 'items'",
                value.type_name()
            ),
        )
    })
}

fn refused(message: String) -> PyError {
    PyError::new(PyErrorKind::Refused, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::py::Json;

    const T0: f64 = 1_790_000_000.0;

    fn json(text: &str) -> Json {
        Json::parse(text).unwrap()
    }

    /// Two samples at T0+2 and T0+9, as the Python's tests build them
    /// (`tests/test_scale.py`'s `samples`): every logical CPU `busy` of its
    /// time busy, the first at `busiest`; the process used all of it but
    /// `other` logical CPUs.
    fn samples(busy: f64, cpus: usize, busiest: Option<f64>, other: f64) -> Vec<Json> {
        let mut first_cpus = Vec::new();
        let mut last_cpus = Vec::new();
        let mut total = 0.0;
        for c in 0..cpus {
            let share = if c == 0 {
                busiest.unwrap_or(busy)
            } else {
                busy
            };
            total += share;
            first_cpus.push(format!("\"{c}\": [0, 0, 0, 0, 0, 0, 0, 0]"));
            last_cpus.push(format!(
                "\"{c}\": [{}, 0, 0, {}, 0, 0, 0, 0]",
                (700.0 * share) as i64,
                (700.0 * (1.0 - share)) as i64
            ));
        }
        let process = ((total - other) * 100.0 * 7.0) as i64;
        vec![
            json(&format!(
                r#"{{"t": {}, "cpus": {{{}}}, "process": 0, "threads": {{"0": ["udpstp-engine", 0, 0, 0], "1": ["udpstp-engine", 0, 0, 0]}}}}"#,
                crate::num::repr(T0 + 2.0),
                first_cpus.join(", ")
            )),
            json(&format!(
                r#"{{"t": {}, "cpus": {{{}}}, "process": {process}, "threads": {{"0": ["udpstp-engine", 10, 7000, 0], "1": ["udpstp-engine", 10, 7000, 0]}}}}"#,
                crate::num::repr(T0 + 9.0),
                last_cpus.join(", ")
            )),
        ]
    }

    #[test]
    fn the_window_is_the_first_and_last_sample_inside_it() {
        let s = samples(0.5, 2, None, 0.0);
        let (first, last) = window(&s, T0, T0 + 10.0).unwrap().unwrap();
        assert_eq!(first, &s[0]);
        assert_eq!(last, &s[1]);
        // One sample inside is no window.
        assert_eq!(window(&s, T0 + 5.0, T0 + 10.0).unwrap(), None);
        // A sample without a time is at 0.
        let untimed = vec![json("{}"), json("{}")];
        assert!(window(&untimed, -1.0, 1.0).unwrap().is_some());
        assert!(window(&[json("[]")], 0.0, 1.0).is_err());
        assert!(window(&[json(r#"{"t": true}"#)], 0.0, 1.0).is_err());
    }

    #[test]
    fn cpu_use_counts_each_logical_cpus_busy_share() {
        let s = samples(0.5, 8, Some(0.9), 0.0);
        let usage = cpu_use(&s, T0, T0 + 10.0, None).unwrap().unwrap();
        assert_eq!(usage.cpus, 8);
        // int(700 * (1 - 0.9)) is 69: the first logical CPU was busy 630 of
        // 699 ticks.
        let busiest = 630.0 / 699.0;
        assert_eq!(usage.busiest, busiest);
        assert_eq!(usage.busy, (0..7).fold(busiest, |t, _| t + 0.5));
        let only = cpu_use(&s, T0, T0 + 10.0, Some(&[1, 2])).unwrap().unwrap();
        assert_eq!((only.cpus, only.busy), (2, 1.0));
        assert_eq!(cpu_use(&s, T0 + 5.0, T0 + 10.0, None).unwrap(), None);
    }

    #[test]
    fn cpu_use_adds_the_shares_in_proc_stats_order() {
        // Sixteen logical CPUs whose shares, added in /proc/stat's order,
        // differ in the last bit from the same shares added with CPU "10"
        // before "2", as a map sorted by name would add them.
        let shares: Vec<(i64, i64)> = (0..16)
            .map(|c| (1 + (c * 37 % 97), 100 + (c * 53 % 89)))
            .collect();
        let cpus = |rise: bool| -> String {
            shares
                .iter()
                .enumerate()
                .map(|(c, &(busy, idle))| {
                    let (busy, idle) = if rise { (busy, idle) } else { (0, 0) };
                    format!("\"{c}\": [{busy}, 0, 0, {idle}, 0, 0, 0, 0]")
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let s = vec![
            json(&format!(r#"{{"t": 1.0, "cpus": {{{}}}}}"#, cpus(false))),
            json(&format!(r#"{{"t": 2.0, "cpus": {{{}}}}}"#, cpus(true))),
        ];
        let usage = cpu_use(&s, 0.0, 3.0, None).unwrap().unwrap();
        let share = |&(busy, idle): &(i64, i64)| busy as f64 / (busy + idle) as f64;
        let in_order = shares.iter().fold(0.0, |t, s| t + share(s));
        let mut by_name: Vec<(String, (i64, i64))> = shares
            .iter()
            .enumerate()
            .map(|(c, &s)| (c.to_string(), s))
            .collect();
        by_name.sort();
        let sorted = by_name.iter().fold(0.0, |t, (_, s)| t + share(s));
        assert_ne!(in_order, sorted, "the fixture must tell the orders apart");
        assert_eq!(usage.busy, in_order);
    }

    #[test]
    fn wakeups_and_other_cpus() {
        let s = samples(0.5, 8, None, 1.5);
        let w = wakeups(&s, T0, T0 + 10.0).unwrap().unwrap();
        assert_eq!(w.threads, 2);
        assert_eq!(w.wakeups_per_s, Some(14000.0 / 7.0));
        let other = other_cpus(&s, T0, T0 + 10.0).unwrap().unwrap();
        assert!((other - 1.5).abs() < 1e-9, "{other}");
        // Other processes' use is never below 0.
        let s = samples(0.5, 8, None, -2.0);
        assert_eq!(other_cpus(&s, T0, T0 + 10.0).unwrap(), Some(0.0));
    }

    #[test]
    fn a_garbled_sample_is_refused() {
        let s = vec![
            json(r#"{"t": 1, "cpus": {"0": [0, 0, 0, 0, 0, 0, 0, 0]}}"#),
            json(r#"{"t": 2, "cpus": {"0": [5, 0, 0, 5.0, 0, 0, 0, 0]}}"#),
        ];
        let e = cpu_use(&s, 0.0, 3.0, None).unwrap_err();
        assert_eq!(e.kind(), PyErrorKind::Refused);
        // Too few counters: an IndexError, as in the Python.
        let s = vec![
            json(r#"{"t": 1, "cpus": {"0": [0, 0, 0]}}"#),
            json(r#"{"t": 2, "cpus": {"0": [5, 0, 0]}}"#),
        ];
        assert_eq!(
            cpu_use(&s, 0.0, 3.0, None).unwrap_err().kind(),
            PyErrorKind::IndexError
        );
    }
}
