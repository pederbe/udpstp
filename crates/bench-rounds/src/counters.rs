//! A Linux host's counters, as a Round reads them before and after each
//! Round Run: UDP's, the softnet backlog's, the NIC's and its queueing
//! discipline's drops, the CPUs' thermal throttles, and the platform's
//! allowance counters among `ethtool -S`'s lines; and how far they rose.
//!
//! The counter script, [`SCRIPT`], lives here beside its reader, since the
//! lines it writes and the parser that reads them change together (the
//! design's Revision 4). It is the Python's `COUNTERS_SCRIPT`
//! (`scripts/bench/round.py:245-292`), byte for byte.
//! First-Round Tests between the Round's hosts use [`towards`] instead,
//! recording the peer path, all underlying driver counters and the NIC's
//! receive settings.
//!
//! The counter script's output is read as the Python reads it
//! (`scripts/bench/scale_report.py:177-209`, `report.py:57-116`), in three
//! parts told apart by position: the `Udp:` header and values; the lines
//! the script writes itself, at the line's start; and `ethtool -S`'s,
//! indented, of which only the allowance counters are read.
//!
//! # Garbled counters
//!
//! Where the Python raises on a counter it can't read, a [`Garbled`] says
//! so. Where it reads one anyway from digits the script never writes (a
//! superscript that `isdigit()` takes but `int()` refuses, other scripts'
//! digits, underscores), the port refuses it too, as the design allows for
//! a garbled file: the kernel
//! writes ASCII digits.

use std::collections::BTreeMap;
use std::fmt;

use indexmap::IndexMap;

use crate::py::{self, is_space};

// The queueing discipline's drops' script ([`QDISC_DROPPED`]), as a
// literal for [`SCRIPT`]'s `concat!`.
macro_rules! qdisc_dropped {
    () => {
        r#"awk '/^qdisc/ { root = / root/; if (root) seen = 1 } root && /dropped/ { n = $0; sub(/.*dropped /, "", n); sub(/,.*/, "", n); s += n } END { if (seen) print "qdisc_dropped: " s + 0 }'"#
    };
}

// The thermal throttles' script after the directory of the CPUs
// ([`throttles_script`]), as a literal for [`SCRIPT`]'s `concat!`.
macro_rules! throttles_after_cpus {
    () => {
        r#"/cpu[0-9]*; do [ -r "$c/thermal_throttle/core_throttle_count" ] && echo "$(cat "$c/topology/physical_package_id" 2>/dev/null):$(cat "$c/topology/core_id" 2>/dev/null) $(cat "$c/thermal_throttle/core_throttle_count") $(cat "$c/thermal_throttle/package_throttle_count" 2>/dev/null || echo 0)"; done | awk '!seen[$1]++ { core += $2 } $3 > pkg { pkg = $3 } END { if (NR) { print "core_throttle: " core; print "package_throttle: " pkg + 0 } }'"#
    };
}

/// What `tc -s qdisc` on its standard input says the queueing discipline
/// dropped: its root's count, since an mq root sums its children's.
/// Nothing without a root queueing discipline (no `tc`, no device), so
/// that a missing count never reads as none dropped (`QDISC_DROPPED`).
pub const QDISC_DROPPED: &str = qdisc_dropped!();

/// Where the kernel keeps the CPUs.
pub const CPUS: &str = "/sys/devices/system/cpu";

/// What the CPUs' thermal throttling counted so far, where the kernel says
/// (under `cpus`, [`CPUS`] on a host): each core's count once, since a
/// core's logical CPUs share it, summed, and the package's. Nothing where
/// there is none, as on a VM (`throttles_script`). `cpus` goes into the
/// script as it is, as in the Python.
pub fn throttles_script(cpus: &str) -> String {
    format!("for c in {cpus}{}", throttles_after_cpus!())
}

/// Each host's counters before and after each Round Run
/// (`COUNTERS_SCRIPT`): UDP's, the softnet backlog's drops, the NIC's
/// drops, the ENA's allowance counters, the NIC's queueing discipline's
/// drops, with the whole of `tc -s qdisc` for the record, and the CPUs'
/// thermal throttles. The lines it writes itself start at the line's
/// start; `ethtool -S`'s are indented. A counter it can't read is left
/// out rather than written as 0.
///
/// The NIC's drops are the kernel's device statistics: `rx_dropped` and
/// `tx_dropped`, where the ENA driver puts its device's drops (its
/// `ethtool -S` has no drop counter of its own), from the device's
/// keep-alive, about once a second; and `rx_missed_errors`, where a NIC
/// such as the e1000e counts the frames it missed for want of receive
/// buffers. The kernel's `rx_dropped` also counts the frames it had no
/// handler for, which on a LAN come whatever the Test does
///.
pub const SCRIPT: &str = concat!(
    r#"
grep -E '^Udp' /proc/net/snmp; grep -E '^Udp6' /proc/net/snmp6 2>/dev/null
[ -r /proc/net/softnet_stat ] && { s=0; while read -r _ d _; do s=$((s + 0x$d)); done < /proc/net/softnet_stat; echo "softnet_dropped: $s"; }
dev=$(ip -o route get 1.1.1.1 2>/dev/null | sed -n 's/.* dev \([^ ]*\).*/\1/p')
for way in rx tx; do n=$(cat "/sys/class/net/$dev/statistics/${way}_dropped" 2>/dev/null) && echo "${way}_drops: $n"; done
n=$(cat "/sys/class/net/$dev/statistics/rx_missed_errors" 2>/dev/null) && echo "rx_missed: $n"
ethtool -S "$dev" 2>/dev/null | grep -Ei 'allowance|drop|miss|err' || true
qdisc=$(tc -s qdisc show dev "$dev" 2>/dev/null)
echo "$qdisc" | "#,
    qdisc_dropped!(),
    r#"
echo "$qdisc" | sed 's/^/  tc: /'
"#,
    "for c in /sys/devices/system/cpu",
    throttles_after_cpus!(),
    "\n",
);

/// First-Round counters for the route to the Test peer, including the
/// underlying NIC of a VLAN. Driver counters are retained in full as raw
/// evidence alongside read-only receive settings, whose output and command
/// statuses are kept outside the counter namespaces. They do not change
/// the report's judging rules.
pub fn towards(peer: &str) -> String {
    towards_at(peer, "/sys/class/net")
}

fn towards_at(peer: &str, net: &str) -> String {
    let mut script = format!(
        "counter_peer={}\ncounter_net={}\n",
        crate::quote::quote(peer),
        crate::quote::quote(net),
    );
    script.push_str(
        r#"grep -E '^Udp' /proc/net/snmp; grep -E '^Udp6' /proc/net/snmp6 2>/dev/null
[ -r /proc/net/softnet_stat ] && { s=0; while read -r _ d _; do s=$((s + 0x$d)); done < /proc/net/softnet_stat; echo "softnet_dropped: $s"; }
route=$(ip -o route get "$counter_peer" 2>/dev/null)
route_dev=$(printf '%s\n' "$route" | sed -n 's/.* dev \([^ ]*\).*/\1/p')
printf 'counter_peer: %s\ncounter_route: %s\nroute_device: %s\n' "$counter_peer" "$route" "$route_dev"
dev=$route_dev
if [ -z "$dev" ]; then
    echo 'counter_device_unavailable: no-route'
else
    for depth in {1..16}; do
        if [ ! -d "$counter_net/$dev" ]; then
            echo 'counter_device_unavailable: missing-device'
            dev=
            break
        fi
        lowers=()
        for lower in "$counter_net/$dev"/lower_*; do
            if [ -e "$lower" ] || [ -L "$lower" ]; then
                lowers+=("${lower##*/lower_}")
            fi
        done
        if [ "${#lowers[@]}" -eq 0 ]; then
            break
        fi
        if [ "${#lowers[@]}" -ne 1 ]; then
            echo 'counter_device_unavailable: multiple-lower-devices'
            dev=
            break
        fi
        dev=${lowers[0]}
        printf 'counter_lower_device: %s\n' "$dev"
        if [ "$depth" -eq 16 ]; then
            echo 'counter_device_unavailable: lower-device-cycle-or-depth'
            dev=
        fi
    done
fi
if [ -n "$dev" ]; then
    printf 'counter_device: %s\n' "$dev"
    for way in rx tx; do n=$(cat "$counter_net/$dev/statistics/${way}_dropped" 2>/dev/null) && echo "${way}_drops: $n"; done
    n=$(cat "$counter_net/$dev/statistics/rx_missed_errors" 2>/dev/null) && echo "rx_missed: $n"
    ethtool -i "$dev" 2>/dev/null
    if driver_counters=$(ethtool -S "$dev" 2>/dev/null); then
        printf '%s\n' "$driver_counters"
    else
        printf 'driver_counters_unavailable: %s\n' "$dev"
    fi
    for setting in udp4_rx_flow_hash rss channels rings coalescing; do
        case "$setting" in
            udp4_rx_flow_hash) set -- -n "$dev" rx-flow-hash udp4;;
            rss) set -- -x "$dev";;
            channels) set -- -l "$dev";;
            rings) set -- -g "$dev";;
            coalescing) set -- -c "$dev";;
        esac
        printf 'nic_settings_begin: %s\n' "$setting"
        printf 'nic_settings_command: ethtool'
        printf ' %s' "$@"
        printf '\n'
        if settings_output=$(ethtool "$@" 2>&1); then
            settings_status=0
        else
            settings_status=$?
            printf 'nic_settings_unavailable: %s\n' "$setting"
        fi
        printf 'nic_settings_status: %s\n' "$settings_status"
        printf '%s\n' "$settings_output" | sed 's/^/  nic_settings: /'
        printf 'nic_settings_end: %s\n' "$setting"
    done
    qdisc=$(tc -s qdisc show dev "$dev" 2>/dev/null)
    echo "$qdisc" | "#,
    );
    script.push_str(QDISC_DROPPED);
    script.push_str(
        r#"
    echo "$qdisc" | sed 's/^/  tc: /'
    if [ "$dev" != "$route_dev" ]; then
        for way in rx tx; do n=$(cat "$counter_net/$route_dev/statistics/${way}_dropped" 2>/dev/null) && echo "route_${way}_drops: $n"; done
        tc -s qdisc show dev "$route_dev" 2>/dev/null | sed 's/^/  route_tc: /'
    fi
fi
"#,
    );
    script.push_str(&throttles_script(CPUS));
    script.push('\n');
    script
}

/// The lines the counter script writes itself (`scale_report.py`'s
/// `OWN_COUNTERS`): the drops, and the CPUs' thermal throttles.
pub const OWN_COUNTERS: &[&str] = &[
    "softnet_dropped",
    "rx_drops",
    "rx_missed",
    "tx_drops",
    "qdisc_dropped",
    "core_throttle",
    "package_throttle",
];

/// Counters by name, in the order the Python's dict holds them.
pub type Counters = IndexMap<String, i128>;

/// A counter the port won't read: where the Python raises, or reads digits
/// no kernel writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Garbled(String);

impl fmt::Display for Garbled {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "a garbled counter: {}", self.0)
    }
}

impl std::error::Error for Garbled {}

/// `parse_counters(text)`: the UDP counters and the lines the script
/// writes itself.
///
/// The first two `Udp:` lines are the header and the values, paired in
/// order until either ends (a name given twice keeps its first place and
/// its last value). Each unindented line `NAME: DIGITS` of
/// [`OWN_COUNTERS`] gives that counter, a later line replacing an earlier
/// one; a value that isn't digits leaves the line out, as the Python does.
pub fn parse(text: &str) -> Result<Counters, Garbled> {
    let lines = py::splitlines(text);
    let mut found = Counters::new();
    let udp: Vec<Vec<&str>> = lines
        .iter()
        .filter(|line| line.starts_with("Udp:"))
        .map(|line| py::split(line))
        .collect();
    if udp.len() >= 2 {
        // zip() stops at the shorter, and int() sees only the values it
        // pairs with a name.
        for (name, value) in udp[0].iter().skip(1).zip(udp[1].iter().skip(1)) {
            found.insert((*name).to_string(), int(value)?);
        }
    }
    for line in &lines {
        if line.chars().next().is_some_and(is_space) {
            // `ethtool -S`'s, whatever a NIC names its own counters.
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !OWN_COUNTERS.contains(&name) {
            continue;
        }
        if let Some(value) = own_value(value)? {
            found.insert(name.to_string(), value);
        }
    }
    Ok(found)
}

/// `value.strip().isdigit()`, then `int(value)`, of what follows a
/// counter's colon. `int()` strips less than `strip()`: of an ASCII
/// string only ASCII whitespace, so U+001C to U+001F around the digits,
/// which `strip()` takes, make it raise.
fn own_value(value: &str) -> Result<Option<i128>, Garbled> {
    let Some(number) = digits(py::strip(value))? else {
        return Ok(None);
    };
    let int_space = |c: char| py::is_space(c) && !('\u{1c}'..='\u{1f}').contains(&c);
    if value.trim_matches(int_space) != py::strip(value) {
        return Err(Garbled(format!(
            "{value:?}: whitespace around the digits that int() refuses"
        )));
    }
    Ok(Some(number))
}

/// `int(v)` of a word of the `Udp:` values: an optional sign and ASCII
/// digits. The rest the Python either refuses or reads from digits the
/// kernel never writes.
fn int(word: &str) -> Result<i128, Garbled> {
    let unsigned = word.strip_prefix(['+', '-']).unwrap_or(word);
    let ascii = !unsigned.is_empty() && unsigned.bytes().all(|b| b.is_ascii_digit());
    ascii
        .then(|| word.parse::<i128>().ok())
        .flatten()
        .ok_or_else(|| Garbled(format!("{word:?} among the UDP counters")))
}

/// A value after a counter's colon, stripped: its number where it is
/// ASCII digits; `None` where it isn't digits at all, which leaves the
/// line out as `isdigit()` does; refused where it is digits of another
/// kind, which `isdigit()` takes.
fn digits(value: &str) -> Result<Option<i128>, Garbled> {
    if value.is_empty() || !value.chars().all(char::is_numeric) {
        return Ok(None);
    }
    if value.bytes().all(|b| b.is_ascii_digit()) {
        return value
            .parse()
            .map(Some)
            .map_err(|_| Garbled(format!("{value:?} is too large")));
    }
    Err(Garbled(format!(
        "{value:?}: digits the kernel doesn't write"
    )))
}

/// `missing_counters(text, required)`: which of `required` the counter
/// script's output lacks, in `required`'s order: the lines it writes
/// itself, UDP's, and the platform's allowance counters.
pub fn missing(text: &str, required: &[&str]) -> Result<Vec<String>, Garbled> {
    let mut found: Vec<String> = parse(text)?.into_keys().collect();
    for line in py::splitlines(text) {
        if let Some((name, _)) = allowance_line(line)? {
            found.push(name.to_string());
        }
    }
    Ok(required
        .iter()
        .filter(|name| !found.iter().any(|f| f == *name))
        .map(|name| (*name).to_string())
        .collect())
}

/// `host_drops`' rise: each of `names`, in their order, that both files
/// hold and that rose, by how much. Nothing where either file is missing,
/// unreadable or holds no counters, as in the Python.
pub fn rise(before: Option<&Counters>, after: Option<&Counters>, names: &[&str]) -> Counters {
    let (Some(before), Some(after)) = (before, after) else {
        return Counters::new();
    };
    if before.is_empty() || after.is_empty() {
        return Counters::new();
    }
    names
        .iter()
        .filter_map(|&name| {
            let (b, a) = (before.get(name)?, after.get(name)?);
            (a > b).then(|| (name.to_string(), a - b))
        })
        .collect()
}

/// An `ethtool -S` line of one of the platform's allowance counters, such
/// as the ENA's `     pps_allowance_exceeded: 12`, as `report.py`'s
/// `ALLOWANCE` matches it: `^\s*([A-Za-z0-9_]+_allowance_exceeded):\s*(\d+)\s*$`.
fn allowance_line(line: &str) -> Result<Option<(&str, i128)>, Garbled> {
    const SUFFIX: &str = "_allowance_exceeded";
    let line = line.trim_start_matches(is_space);
    let Some((name, value)) = line.split_once(':') else {
        return Ok(None);
    };
    let named = name.len() > SUFFIX.len()
        && name.ends_with(SUFFIX)
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if !named {
        return Ok(None);
    }
    Ok(digits(py::strip(value))?.map(|v| (name, v)))
}

/// `allowance_counters`: the platform's allowance counters in a counter
/// file's text, `{name: value}`, a later line replacing an earlier one.
pub fn allowance(text: &str) -> Result<Counters, Garbled> {
    let mut found = Counters::new();
    for line in py::splitlines(text) {
        if let Some((name, value)) = allowance_line(line)? {
            found.insert(name.to_string(), value);
        }
    }
    Ok(found)
}

/// `platform_limits(prefix)`: how far each of the platform's allowance
/// counters rose on each host during one Round Run, `{role: {counter:
/// rise}}`, from the files a Round writes before and after it,
/// `<base>.<role>.before.txt` and `.after.txt`.
///
/// `files` is the directory those files are in: every name in it with its
/// text, or `None` for a file that couldn't be read; roles come in the
/// names' order, as the Python sorts them. Only a rise counts: a host
/// without such counters (not on AWS), or a counter or file missing from
/// either side, counts as nothing.
pub fn platform_limits(
    base: &str,
    files: &BTreeMap<String, Option<String>>,
) -> Result<IndexMap<String, Counters>, Garbled> {
    let mut limits = IndexMap::new();
    let prefix = format!("{base}.");
    for name in files.keys() {
        let Some(role) = name
            .strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix(".before.txt"))
        else {
            continue;
        };
        if role.is_empty() || role.contains('.') {
            continue;
        }
        let read = |name: &str| -> Result<Option<Counters>, Garbled> {
            files
                .get(name)
                .and_then(Option::as_deref)
                .map(allowance)
                .transpose()
        };
        let before = read(name)?;
        let after = read(&format!("{base}.{role}.after.txt"))?;
        let (Some(before), Some(mut after)) = (before, after) else {
            continue;
        };
        if before.is_empty() || after.is_empty() {
            continue;
        }
        // In order of name, as `sorted(after)`.
        after.sort_keys();
        let rose: Counters = after
            .iter()
            .filter_map(|(k, &a)| {
                let &b = before.get(k)?;
                (a > b).then(|| (k.clone(), a - b))
            })
            .collect();
        if !rose.is_empty() {
            limits.insert(role.to_string(), rose);
        }
    }
    Ok(limits)
}

/// `limits_text(limits)`: a Round Run's platform limits in words, such as
/// `server pps_allowance_exceeded +12`.
pub fn limits_text(limits: &IndexMap<String, Counters>) -> String {
    limits
        .iter()
        .map(|(role, counters)| {
            let rises: Vec<String> = counters.iter().map(|(k, v)| format!("{k} +{v}")).collect();
            format!("{role} {}", rises.join(", "))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(all(test, target_os = "linux"))]
mod path_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// What the counter script prints on a c6in host, as the Python's
    /// tests build it (`tests/test_scale.py`'s `ena_counters`).
    fn ena(rx_drops: u32, pps: u32) -> String {
        [
            "Udp: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors InCsumErrors IgnoredMulti MemErrors",
            "Udp: 91234 3 0 88321 0 0 0 0 0",
            "UdpLite: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors InCsumErrors IgnoredMulti MemErrors",
            "UdpLite: 0 0 0 0 0 0 0 0 0",
            "Udp6InDatagrams                 \t0",
            "softnet_dropped: 0",
            &format!("rx_drops: {rx_drops}"),
            "tx_drops: 0",
            "rx_missed: 0",
            "     bw_in_allowance_exceeded: 0",
            &format!("     pps_allowance_exceeded: {pps}"),
            "     conntrack_allowance_available: 40000",
            "     queue_0_rx_xdp_drop: 0",
            "qdisc_dropped: 0",
            "  tc: qdisc mq 0: root",
        ]
        .join("\n")
            + "\n"
    }

    #[test]
    fn reads_udps_counters_and_the_scripts_own_lines() {
        let found = parse(&ena(4, 0)).unwrap();
        assert_eq!(found["InDatagrams"], 91234);
        assert_eq!(found["NoPorts"], 3);
        assert_eq!(found["rx_drops"], 4);
        // UdpLite's lines aren't UDP's, and ethtool's are indented.
        assert!(!found.contains_key("conntrack_allowance_available"));
        assert_eq!(found.len(), 9 + 5);
        // An indented `rx_drops` of some NIC doesn't stand in for the
        // device's.
        let text = ena(4, 0) + "     rx_drops: 999\n";
        assert_eq!(parse(&text).unwrap()["rx_drops"], 4);
    }

    #[test]
    fn a_value_that_isnt_digits_leaves_its_line_out() {
        let found =
            parse("rx_drops: -5\ntx_drops: 5x\nqdisc_dropped: \nsoftnet_dropped:  7 \n").unwrap();
        assert_eq!(found, Counters::from([("softnet_dropped".to_string(), 7)]));
        // A name with a space before its colon isn't one of the script's.
        assert!(parse("rx_drops : 5\n").unwrap().is_empty());
    }

    #[test]
    fn refuses_what_the_python_refuses_or_reads_from_digits_no_kernel_writes() {
        // int() refuses a superscript that isdigit() takes.
        assert!(parse("rx_drops: \u{b2}\n").is_err());
        // int() reads Arabic-Indic digits; the kernel writes none.
        assert!(parse("rx_drops: \u{663}\n").is_err());
        assert!(parse("Udp: InDatagrams\nUdp: x\n").is_err());
        assert!(parse("Udp: InDatagrams\nUdp: 1_000\n").is_err());
        // Values beyond the header's names are never read.
        assert_eq!(
            parse("Udp: InDatagrams\nUdp: 5 x\n").unwrap(),
            Counters::from([("InDatagrams".to_string(), 5)])
        );
        // strip() takes U+001F, int() of an ASCII string doesn't: Python
        // raises. The allowance counters' \s takes it.
        assert!(parse("tx_drops: \u{1f}9\n").is_err());
        assert_eq!(
            parse("tx_drops: \u{a0}9\n").unwrap(),
            Counters::from([("tx_drops".to_string(), 9)])
        );
        assert_eq!(
            allowance("  x_allowance_exceeded:\u{1f}9\n").unwrap(),
            Counters::from([("x_allowance_exceeded".to_string(), 9)])
        );
    }

    #[test]
    fn names_what_a_counter_file_lacks() {
        let everything = [
            "RcvbufErrors",
            "InErrors",
            "softnet_dropped",
            "rx_drops",
            "rx_missed",
            "tx_drops",
            "qdisc_dropped",
            "pps_allowance_exceeded",
        ];
        assert_eq!(
            missing(&ena(0, 0), &everything).unwrap(),
            Vec::<String>::new()
        );
        let text = ena(0, 0).replace("tx_drops: 0\n", "");
        assert_eq!(missing(&text, &everything).unwrap(), ["tx_drops"]);
        assert_eq!(missing("", &["InErrors"]).unwrap(), ["InErrors"]);
    }

    #[test]
    fn a_rise_counts_only_where_both_files_hold_the_counter() {
        let before = parse(&ena(1, 0)).unwrap();
        let after = parse(&ena(5, 0)).unwrap();
        assert_eq!(
            rise(
                Some(&before),
                Some(&after),
                &["rx_drops", "tx_drops", "absent"]
            ),
            Counters::from([("rx_drops".to_string(), 4)])
        );
        assert!(rise(None, Some(&after), &["rx_drops"]).is_empty());
        assert!(rise(Some(&Counters::new()), Some(&after), &["rx_drops"]).is_empty());
        // A fall isn't a rise.
        assert!(rise(Some(&after), Some(&before), &["rx_drops"]).is_empty());
    }

    #[test]
    fn a_run_is_limited_where_an_allowance_counter_rose() {
        let file = |name: &str, text: &str| (name.to_string(), Some(text.to_string()));
        let files = BTreeMap::from([
            file("001-a.server.before.txt", &ena(0, 5)),
            file("001-a.server.after.txt", &ena(0, 1209)),
            file("001-a.client.before.txt", &ena(0, 0)),
            file("001-a.client.after.txt", &ena(0, 0)),
            // Another Run's, and a host without the ENA's counters.
            file("001-ab.server.before.txt", &ena(0, 0)),
            file("001-a.home.before.txt", "Udp: InDatagrams\nUdp: 5\n"),
            file("001-a.home.after.txt", "Udp: InDatagrams\nUdp: 9\n"),
            ("001-a.lost.before.txt".to_string(), None),
        ]);
        let limits = platform_limits("001-a", &files).unwrap();
        assert_eq!(
            limits,
            IndexMap::from([(
                "server".to_string(),
                Counters::from([("pps_allowance_exceeded".to_string(), 1204)])
            )])
        );
        assert_eq!(limits_text(&limits), "server pps_allowance_exceeded +1204");
        assert!(platform_limits("002-b", &files).unwrap().is_empty());
    }
}
