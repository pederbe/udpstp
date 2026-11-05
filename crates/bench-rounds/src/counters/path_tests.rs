//! The first Round's private-path counter correction. Saved AMS evidence
//! showed a public-route snapshot beside private-VLAN Test traffic.
//! These fixtures exercise docs/testing.md's First-Round counter collection
//! through the emitted script without real network tools. The ethtool(8)
//! query options supply the setting groups: -n with rx-flow-hash udp4,
//! -x for RSS, -l for channels, -g for rings and -c for coalescing.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::process::Command;

use super::*;

struct Fixture {
    dir: crate::secrets::SecretDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            dir: crate::secrets::SecretDir::create().unwrap(),
        };
        fs::create_dir_all(fixture.path("bin")).unwrap();
        for (dev, drops) in [("public0", 0), ("private0", 23), ("vlan2035", 4)] {
            let stats = fixture.path(&format!("net/{dev}/statistics"));
            fs::create_dir_all(&stats).unwrap();
            for (name, value) in [
                ("rx_dropped", drops),
                ("tx_dropped", 2),
                ("rx_missed_errors", 7),
            ] {
                fs::write(stats.join(name), value.to_string()).unwrap();
            }
        }
        symlink("../private0", fixture.path("net/vlan2035/lower_private0")).unwrap();
        fixture.tool(
            "ip",
            r#"case "$*" in
  '-o route get 10.196.0.2') echo '10.196.0.2 dev vlan2035 src 10.196.0.1';;
  '-o route get 2001:db8::2') echo '2001:db8::2 dev private0 src 2001:db8::1';;
  '-o route get 1.1.1.1') echo '1.1.1.1 dev public0 src 203.0.113.1';;
  *) exit 1;;
esac
"#,
        );
        fixture.tool(
            "ethtool",
            r#"printf '%s\n' "$*" >> ethtool.calls
if [ "$2" = public0 ]; then
    echo 'public-interface-decoy'
    exit 0
fi
[ "$2" = private0 ] || exit 1
case "$1" in
  -S) printf '     rx_no_dma_resources: 18816179\n     rx_queue_7_packets: 91778379\n     pps_allowance_exceeded: 12\n';;
  -i) printf 'driver: ixgbe\nbus-info: 0000:01:00.0\n';;
  -n) [ "$*" = '-n private0 rx-flow-hash udp4' ] || exit 1
      printf 'UDP over IPV4 flows use these fields for computing Hash flow key:\nIP SA\nIP DA\nL4 bytes 0 & 1 [TCP/UDP src port]\nL4 bytes 2 & 3 [TCP/UDP dst port]\n';;
  -x) printf 'RX flow hash indirection table for private0 with 8 RX ring(s):\n    0: 0 1 2 3 4 5 6 7\nRSS hash key:\n01:23:45:67:89:ab:cd:ef\nRSS hash function:\n    toeplitz: on\n    xor: off\n';;
  -l) printf 'Channel parameters for private0:\nPre-set maximums:\nCombined: 8\nCurrent hardware settings:\nCombined: 8\n';;
  -g) printf 'Ring parameters for private0:\nPre-set maximums:\nRX: 4096\nCurrent hardware settings:\nRX: 512\n';;
  -c) printf 'Coalesce parameters for private0:\nAdaptive RX: off  TX: off\nrx-usecs: 1\nrx-frames: 0\n';;
  *) exit 1;;
esac
"#,
        );
        fixture.tool(
            "tc",
            r#"case "$*" in
  '-s qdisc show dev private0') printf 'qdisc mq 0: root\n Sent 90 bytes 6 pkt (dropped 9, overlimits 0 requeues 0)\n';;
  '-s qdisc show dev vlan2035') echo 'qdisc noqueue 0: root';;
  *) exit 1;;
esac
"#,
        );
        fixture
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    fn tool(&self, name: &str, body: &str) {
        let path = self.path(&format!("bin/{name}"));
        fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn collect(&self, peer: &str) -> String {
        let script = towards_at(peer, self.path("net").to_str().unwrap());
        let output = Command::new("bash")
            .args(["-c", &script])
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("bin").display()),
            )
            .current_dir(self.dir.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    }
}

#[test]
fn a_private_vlan_uses_its_underlying_nic_and_retains_resource_counters() {
    let fixture = Fixture::new();
    let text = fixture.collect("10.196.0.2");
    assert!(text.contains("counter_peer: 10.196.0.2\n"), "{text}");
    assert!(text.contains("route_device: vlan2035\n"), "{text}");
    assert!(text.contains("counter_device: private0\n"), "{text}");
    assert_eq!(parse(&text).unwrap()["rx_drops"], 23, "{text}");
    assert_eq!(parse(&text).unwrap()["qdisc_dropped"], 9, "{text}");
    assert!(text.contains("rx_no_dma_resources: 18816179"), "{text}");
    assert!(text.contains("rx_queue_7_packets: 91778379"), "{text}");
    assert_eq!(allowance(&text).unwrap()["pps_allowance_exceeded"], 12);
    for (group, command, evidence) in [
        (
            "udp4_rx_flow_hash",
            "-n private0 rx-flow-hash udp4",
            "L4 bytes 2 & 3 [TCP/UDP dst port]",
        ),
        ("rss", "-x private0", "01:23:45:67:89:ab:cd:ef"),
        ("channels", "-l private0", "Combined: 8"),
        ("rings", "-g private0", "RX: 512"),
        ("coalescing", "-c private0", "rx-usecs: 1"),
    ] {
        assert!(
            text.contains(&format!(
                "nic_settings_begin: {group}\nnic_settings_command: ethtool {command}\nnic_settings_status: 0\n"
            )),
            "{text}"
        );
        assert!(
            text.contains(&format!("  nic_settings: {evidence}\n")),
            "{text}"
        );
        assert!(
            text.contains(&format!("nic_settings_end: {group}\n")),
            "{text}"
        );
    }
    assert!(
        text.contains("  nic_settings:     0: 0 1 2 3 4 5 6 7\n"),
        "{text}"
    );
    assert!(
        text.contains("  nic_settings:     toeplitz: on\n"),
        "{text}"
    );
    assert!(text.contains("  nic_settings:     xor: off\n"), "{text}");
    assert_eq!(
        fs::read_to_string(fixture.path("ethtool.calls")).unwrap(),
        "-i private0\n-S private0\n-n private0 rx-flow-hash udp4\n-x private0\n-l private0\n-g private0\n-c private0\n"
    );
    assert!(!text.contains("public0"), "{text}");
    assert!(!text.contains("public-interface-decoy"), "{text}");
    assert!(!text.contains("nic_settings_unavailable:"), "{text}");
}

#[test]
fn unavailable_settings_keep_status_and_error_text_without_losing_counters() {
    let fixture = Fixture::new();
    fixture.tool(
        "ethtool",
        r#"[ "$2" = private0 ] || exit 1
case "$1" in
  -S) printf '     pps_allowance_exceeded: 12\n';;
  -i) echo 'driver: ixgbe';;
  -n|-g) echo 'Cannot get device settings: Operation not supported' >&2; exit 95;;
  -x|-c) echo 'netlink error: Operation not permitted' >&2; exit 1;;
  -l) echo 'Cannot get device channels: Operation not supported' >&2; exit 2;;
  *) exit 1;;
esac
"#,
    );
    let text = fixture.collect("10.196.0.2");
    for (group, status, error) in [
        (
            "udp4_rx_flow_hash",
            95,
            "Cannot get device settings: Operation not supported",
        ),
        ("rss", 1, "netlink error: Operation not permitted"),
        (
            "channels",
            2,
            "Cannot get device channels: Operation not supported",
        ),
        (
            "rings",
            95,
            "Cannot get device settings: Operation not supported",
        ),
        ("coalescing", 1, "netlink error: Operation not permitted"),
    ] {
        assert!(
            text.contains(&format!(
                "nic_settings_unavailable: {group}\nnic_settings_status: {status}\n  nic_settings: {error}\nnic_settings_end: {group}\n"
            )),
            "{text}"
        );
    }
    let counters = parse(&text).unwrap();
    assert_eq!(counters["rx_drops"], 23, "{text}");
    assert_eq!(counters["qdisc_dropped"], 9, "{text}");
    assert!(counters.contains_key("RcvbufErrors"), "{text}");
    assert_eq!(allowance(&text).unwrap()["pps_allowance_exceeded"], 12);
}

#[test]
fn a_missing_ethtool_keeps_setting_groups_unavailable() {
    let fixture = Fixture::new();
    fixture.tool("ethtool", "exec ./missing-ethtool \"$@\"\n");
    let text = fixture.collect("10.196.0.2");
    assert!(
        text.contains("driver_counters_unavailable: private0\n"),
        "{text}"
    );
    for group in [
        "udp4_rx_flow_hash",
        "rss",
        "channels",
        "rings",
        "coalescing",
    ] {
        assert!(
            text.contains(&format!(
                "nic_settings_unavailable: {group}\nnic_settings_status: 127\n"
            )),
            "{text}"
        );
    }
    let counters = parse(&text).unwrap();
    assert_eq!(counters["rx_drops"], 23, "{text}");
    assert!(counters.contains_key("RcvbufErrors"), "{text}");
    assert!(allowance(&text).unwrap().is_empty(), "{text}");
}

#[test]
fn setting_output_stays_outside_both_numeric_counter_namespaces() {
    let fixture = Fixture::new();
    fixture.tool(
        "ethtool",
        r#"[ "$2" = private0 ] || exit 1
case "$1" in
  -S) printf '     pps_allowance_exceeded: 12\n';;
  -i) echo 'driver: ixgbe';;
  *) printf 'rx_drops: 999\npps_allowance_exceeded: 999\nUdp: Setting\nUdp: 999\n';;
esac
"#,
    );
    let text = fixture.collect("10.196.0.2");
    assert!(text.contains("  nic_settings: rx_drops: 999\n"), "{text}");
    let counters = parse(&text).unwrap();
    assert_eq!(counters["rx_drops"], 23, "{text}");
    assert!(!counters.contains_key("Setting"), "{text}");
    assert_eq!(allowance(&text).unwrap()["pps_allowance_exceeded"], 12);
}

#[test]
fn a_direct_ipv6_path_needs_no_lower_device() {
    let text = Fixture::new().collect("2001:db8::2");
    assert!(text.contains("route_device: private0\n"), "{text}");
    assert!(text.contains("counter_device: private0\n"), "{text}");
    assert_eq!(parse(&text).unwrap()["rx_drops"], 23);
}

#[test]
fn a_missing_route_keeps_nic_counters_unavailable() {
    let fixture = Fixture::new();
    let text = fixture.collect("10.196.99.2");
    assert!(
        text.contains("counter_device_unavailable: no-route\n"),
        "{text}"
    );
    let counters = parse(&text).unwrap();
    assert!(!counters.contains_key("rx_drops"), "{text}");
    assert!(!counters.contains_key("qdisc_dropped"), "{text}");
    assert!(counters.contains_key("RcvbufErrors"), "{text}");
    assert!(!text.contains("nic_settings_begin:"), "{text}");
    assert!(!fixture.path("ethtool.calls").exists());
}

#[test]
fn ambiguous_or_cyclic_lower_devices_are_not_guessed() {
    for ambiguous in [true, false] {
        let fixture = Fixture::new();
        if ambiguous {
            symlink("../public0", fixture.path("net/vlan2035/lower_public0")).unwrap();
        } else {
            symlink("../vlan2035", fixture.path("net/private0/lower_vlan2035")).unwrap();
        }
        let text = fixture.collect("10.196.0.2");
        assert!(text.contains("counter_device_unavailable:"), "{text}");
        assert!(!parse(&text).unwrap().contains_key("rx_drops"), "{text}");
        assert!(!text.contains("nic_settings_begin:"), "{text}");
        assert!(!fixture.path("ethtool.calls").exists());
    }
}

#[test]
fn peer_text_is_quoted_and_never_executed() {
    let fixture = Fixture::new();
    let text = fixture.collect("10.196.0.2; touch injected");
    assert!(!fixture.path("injected").exists());
    assert!(
        text.contains("counter_device_unavailable: no-route\n"),
        "{text}"
    );
}
