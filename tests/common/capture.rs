//! Packet capture in the network-namespace tier: `tcpdump` on a router's
//! interface, and the UDP datagrams read back from its pcap file.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::netns::{Node, Topology};

/// A UDP datagram seen on the wire.
#[derive(Debug, Clone)]
pub struct Datagram {
    pub source: SocketAddr,
    pub destination: SocketAddr,
    /// The UDP payload's length, from the UDP header.
    pub length: usize,
    /// The payload as captured: the first bytes only of a long one.
    pub payload: Vec<u8>,
}

/// A running `tcpdump`, writing UDP traffic to a pcap file: the first 200
/// bytes of each frame, enough for any Control PDU.
pub struct Capture {
    child: Child,
    file: PathBuf,
}

impl Capture {
    /// Starts capturing UDP on `interface` in `node`'s namespace, and waits
    /// until tcpdump listens.
    pub fn start(topology: &Topology, node: Node, interface: &str) -> Self {
        let file = std::env::temp_dir().join(format!(
            "udpstp-{}-{}.pcap",
            topology.ns(node),
            std::process::id()
        ));
        let _ = std::fs::remove_file(&file);
        let child = topology
            .command(node, "tcpdump")
            .args(["-Z", "root", "-i", interface, "-U", "-n", "-s", "200", "-w"])
            .arg(&file)
            .arg("udp")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("tcpdump starts (is it installed?)");
        let capture = Self { child, file };
        // tcpdump writes the file's header once it listens.
        let until = Instant::now() + Duration::from_secs(10);
        while std::fs::metadata(&capture.file).map_or(true, |m| m.len() < 24) {
            assert!(Instant::now() < until, "tcpdump didn't start");
            std::thread::sleep(Duration::from_millis(50));
        }
        capture
    }

    /// Stops the capture and returns the UDP datagrams it saw, in order.
    pub fn stop(mut self) -> Vec<Datagram> {
        // SIGINT makes tcpdump flush and close the file.
        let _ = Command::new("kill")
            .args(["-INT", &self.child.id().to_string()])
            .status();
        let _ = self.child.wait();
        let bytes = std::fs::read(&self.file).expect("the pcap file");
        let _ = std::fs::remove_file(&self.file);
        parse_pcap(&bytes)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The UDP datagrams in a pcap file of Ethernet frames.
pub fn parse_pcap(bytes: &[u8]) -> Vec<Datagram> {
    let u32_at = |b: &[u8], at: usize, little: bool| {
        let v = [b[at], b[at + 1], b[at + 2], b[at + 3]];
        if little {
            u32::from_le_bytes(v)
        } else {
            u32::from_be_bytes(v)
        }
    };
    assert!(bytes.len() >= 24, "a pcap header");
    let little = match &bytes[..4] {
        [0xd4, 0xc3, 0xb2, 0xa1] | [0x4d, 0x3c, 0xb2, 0xa1] => true,
        [0xa1, 0xb2, 0xc3, 0xd4] | [0xa1, 0xb2, 0x3c, 0x4d] => false,
        magic => panic!("not a pcap file: {magic:02x?}"),
    };
    assert_eq!(u32_at(bytes, 20, little), 1, "Ethernet frames");
    let mut at = 24;
    let mut datagrams = Vec::new();
    while at + 16 <= bytes.len() {
        let len = u32_at(bytes, at + 8, little) as usize;
        let frame = &bytes[at + 16..(at + 16 + len).min(bytes.len())];
        at += 16 + len;
        if let Some(datagram) = udp(frame) {
            datagrams.push(datagram);
        }
    }
    datagrams
}

/// The UDP datagram in an Ethernet frame, if it carries one.
fn udp(frame: &[u8]) -> Option<Datagram> {
    let ethertype = u16::from_be_bytes([*frame.get(12)?, *frame.get(13)?]);
    let ip = frame.get(14..)?;
    let (source, destination, segment) = match ethertype {
        0x0800 => {
            let header = usize::from(ip.first()? & 0x0f) * 4;
            if *ip.get(9)? != 17 {
                return None;
            }
            let source = IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(ip.get(12..16)?).ok()?));
            let destination =
                IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(ip.get(16..20)?).ok()?));
            (source, destination, ip.get(header..)?)
        }
        0x86dd => {
            if *ip.get(6)? != 17 {
                return None;
            }
            let source = IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(ip.get(8..24)?).ok()?));
            let destination =
                IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(ip.get(24..40)?).ok()?));
            (source, destination, ip.get(40..)?)
        }
        _ => return None,
    };
    let port = |at: usize| {
        Some(u16::from_be_bytes([
            *segment.get(at)?,
            *segment.get(at + 1)?,
        ]))
    };
    let length = usize::from(port(4)?).saturating_sub(8);
    Some(Datagram {
        source: SocketAddr::new(source, port(0)?),
        destination: SocketAddr::new(destination, port(2)?),
        length,
        payload: segment.get(8..(8 + length).min(segment.len()))?.to_vec(),
    })
}
