//! The Sending Rate Table's variants (RFC 9946 §6.1; ): the Server
//! serves the CHSR_JUMBO_STATUS and CHSR_TRADITIONAL_MTU it is set to,
//! refuses a Client that asks for others, and its rows' packets follow the
//! size rules, fitted to its MTU above 1 Gbps.

use super::harness;

use std::time::Duration;

use crate::protocol::{Direction, TableShape};
use crate::wire::ActivationPdu;
use harness::{RowRequest, Scenario};

const KEY: &[u8] = b"an operator key";

/// The srStruct an Upstream Fixed-Rate Test at `row` gets, with the Server
/// set to `shape` and the Client asking for its modifiers.
fn row(
    seed: u64,
    shape: fn(&mut crate::protocol::ServerConfig),
    jumbo: bool,
    traditional: bool,
    row: u16,
) -> Option<ActivationPdu> {
    let outcome = Scenario::new(seed)
        .operator(7, KEY)
        .server_config(shape)
        .client_config(move |c| {
            c.jumbo = jumbo;
            c.traditional_mtu = traditional;
        })
        .client_direction(Direction::Upstream)
        .client_requests_row(RowRequest::FixedRate(row))
        .run_for(Duration::from_millis(300));
    outcome.activation_responses().first().cloned()
}

/// The largest IP packet a srStruct sends, IPv4-based (ADR 0004).
fn largest(pdu: &ActivationPdu) -> u32 {
    let s = &pdu.sending_rate;
    [s.udp_payload1, s.udp_payload2, s.udp_addon2 & 0x7FFF_FFFF]
        .into_iter()
        .filter(|&p| p > 0)
        .max()
        .unwrap_or(0)
        + 28
}

// A Server whose Jumbo or Traditional MTU setting differs rejects the
// Setup; one that matches serves it;
#[udpstp_req::req("RFC9946-6.2.1-2", "RFC9946-12.3.3-L1")]
#[test]
fn a_client_asking_for_the_server_s_modifiers_is_served() {
    fn plain(c: &mut crate::protocol::ServerConfig) {
        c.table = TableShape {
            jumbo: false,
            ..TableShape::default()
        };
    }
    fn traditional(c: &mut crate::protocol::ServerConfig) {
        c.table = TableShape {
            traditional_mtu: true,
            ..TableShape::default()
        };
    }
    assert!(row(1, plain, false, false, 875).is_some(), "no jumbo, both");
    assert!(
        row(2, plain, true, false, 875).is_none(),
        "jumbo asked, not served"
    );
    assert!(
        row(3, traditional, true, true, 875).is_some(),
        "traditional, both"
    );
    assert!(
        row(4, traditional, true, false, 875).is_none(),
        "traditional served, not asked"
    );
}

// IP packets up to 1250 bytes to 1 Gbps, 1500 with CHSR_TRADITIONAL_MTU,
// and above 1 Gbps up to 9000 with CHSR_JUMBO_STATUS;
#[udpstp_req::req("RFC9946-6.1-8", "RFC9946-6.1-9", "RFC9946-6.1-11")]
#[test]
fn srstruct_packet_sizes_follow_the_modifiers() {
    fn default(_: &mut crate::protocol::ServerConfig) {}
    fn traditional(c: &mut crate::protocol::ServerConfig) {
        c.table = TableShape {
            traditional_mtu: true,
            ..TableShape::default()
        };
    }
    fn jumbo_9000(c: &mut crate::protocol::ServerConfig) {
        c.table = TableShape {
            mtu: 9000,
            ..TableShape::default()
        };
    }
    fn plain(c: &mut crate::protocol::ServerConfig) {
        c.table = TableShape {
            jumbo: false,
            ..TableShape::default()
        };
    }
    let size = |pdu: Option<ActivationPdu>| largest(&pdu.expect("activated"));
    assert_eq!(size(row(5, default, true, false, 875)), 1250);
    assert_eq!(size(row(6, traditional, true, true, 875)), 1500);
    // Above 1 Gbps: jumbo-sized with a 9000-byte MTU, else fitted to 1500.
    assert_eq!(size(row(7, jumbo_9000, true, false, 1062)), 9000);
    assert_eq!(size(row(8, default, true, false, 1062)), 1500);
    assert_eq!(size(row(9, plain, false, false, 1062)), 1250);
}
