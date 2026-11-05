//! Multicast and broadcast addresses in the Setup exchange (RFC 9946 §6):
//! the Client's source and the Server's destination address must be
//! unicast (or anycast), and a Setup Request or Response from or to a
//! multicast or broadcast address is dropped silently. Broadcast here is
//! 255.255.255.255: a subnet's directed broadcast can't be told from a
//! host address without its prefix length.

mod harness;

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use harness::{
    ControlPhaseFailure, Outcome, ResponseIgnoreReason, Scenario, SilentRejectionReason,
};

/// Multicast and broadcast addresses of both families, and in an
/// IPv4-mapped IPv6 address.
fn group_addresses() -> Vec<IpAddr> {
    [
        "224.0.0.1",
        "239.255.255.250",
        "255.255.255.255",
        "ff02::1",
        "ff0e::1234",
        "::ffff:224.0.0.5",
        "::ffff:255.255.255.255",
    ]
    .iter()
    .map(|ip| ip.parse().unwrap())
    .collect()
}

fn unauthenticated(seed: u64) -> Scenario {
    Scenario::new(seed)
        .server_allows_unauthenticated()
        .client_unauthenticated()
}

/// Whether the Server sent anything from its Standard Port.
fn server_answered(outcome: &Outcome) -> bool {
    outcome
        .trace
        .iter()
        .any(|t| t.source == harness::SERVER_ADDR.parse().unwrap())
}

#[udpstp_req::req("RFC9946-6-2")]
#[test]
fn a_setup_request_from_a_group_address_is_dropped_silently() {
    for (seed, ip) in (1..).zip(group_addresses()) {
        let outcome = unauthenticated(seed)
            .setup_pdus_from(ip)
            .run_for(Duration::from_millis(100));
        outcome.check(
            outcome.server_dropped_setup_because(SilentRejectionReason::MulticastOrBroadcast)
                && !server_answered(&outcome)
                && outcome.server_admitted_count() == 0,
            &format!("from {ip}"),
        );
    }
}

#[udpstp_req::req("RFC9946-6-2")]
#[test]
fn a_setup_request_to_a_group_address_is_dropped_silently() {
    for (seed, ip) in (20..).zip(group_addresses()) {
        let outcome = unauthenticated(seed)
            .setup_pdus_to(ip)
            .run_for(Duration::from_millis(100));
        outcome.check(
            outcome.server_dropped_setup_because(SilentRejectionReason::MulticastOrBroadcast)
                && !server_answered(&outcome)
                && outcome.server_admitted_count() == 0,
            &format!("to {ip}"),
        );
    }
}

// The reply leaves from the address the request arrived on (RFC 9946
// §6.2.2); ADR 0007 (without one, the request is ignored and the Client
// retries);
#[test]
fn a_setup_request_without_an_arrival_address_is_ignored() {
    for (seed, ip) in [(60, "0.0.0.0"), (61, "::")] {
        let outcome = unauthenticated(seed)
            .setup_pdus_to(ip.parse().unwrap())
            .run_for(Duration::from_millis(100));
        outcome.check(
            outcome.server_ignored_setup_because(harness::SetupIgnoreReason::ArrivalAddressUnknown)
                && !server_answered(&outcome)
                && outcome.server_admitted_count() == 0,
            &format!("to {ip}"),
        );
    }
}

#[udpstp_req::req("RFC9946-6-2")]
#[test]
fn a_setup_response_to_a_group_address_is_dropped() {
    for (seed, ip) in (40..).zip(group_addresses()) {
        // A scripted Server answers whatever it gets.
        let outcome = Scenario::new(seed)
            .client_unauthenticated()
            .setup_pdus_to(ip)
            .scripted_server(|_, request| {
                let mut pdu = request.to_vec();
                pdu[8] = 2;
                pdu[9] = 1;
                pdu[12..14].copy_from_slice(&24601u16.to_be_bytes());
                Some(pdu)
            })
            .run_for(Duration::from_millis(100));
        outcome.check(
            outcome
                .client_dropped_setup_response_because(ResponseIgnoreReason::MulticastOrBroadcast)
                && outcome.client_accepted_test_port().is_none()
                && outcome.activation_requests().is_empty(),
            &format!("to {ip}"),
        );
    }
}

#[udpstp_req::req("RFC9946-6-1")]
#[test]
fn a_client_does_not_send_to_a_group_address() {
    for (seed, ip) in (60..).zip(group_addresses()) {
        let outcome = unauthenticated(seed)
            .client_server_address(SocketAddr::new(ip, 24601))
            .run_for(Duration::from_millis(100));
        outcome.check(
            outcome.client_failed_because(ControlPhaseFailure::MulticastOrBroadcastServer)
                && outcome.trace.is_empty(),
            &format!("to {ip}: {:?}", outcome.client_events),
        );
    }
}

// Other sources, a subnet's last address included, are answered.
#[udpstp_req::req("RFC9946-6-L1")]
#[test]
fn unicast_sources_are_answered() {
    for (seed, ip) in (80..).zip(["198.51.100.255", "10.0.0.0", "2001:db8::ff"]) {
        let ip: IpAddr = ip.parse().unwrap();
        let outcome = unauthenticated(seed)
            .setup_pdus_from(ip)
            .run_for(Duration::from_millis(100));
        outcome.check(
            server_answered(&outcome)
                && !outcome
                    .server_dropped_setup_because(SilentRejectionReason::MulticastOrBroadcast),
            &format!("from {ip}"),
        );
    }
}

// A Setup Response from a group address isn't from the configured Server.
#[udpstp_req::req("RFC9946-6-2")]
#[test]
fn a_setup_response_from_a_group_address_is_ignored() {
    for (seed, ip) in (100..).zip(group_addresses()) {
        let outcome = Scenario::new(seed)
            .client_unauthenticated()
            .setup_pdus_from(ip)
            .scripted_server(|_, request| {
                let mut pdu = request.to_vec();
                pdu[8] = 2;
                pdu[9] = 1;
                pdu[12..14].copy_from_slice(&24601u16.to_be_bytes());
                Some(pdu)
            })
            .run_for(Duration::from_millis(100));
        outcome.check(
            outcome.client_accepted_test_port().is_none()
                && outcome.activation_requests().is_empty(),
            &format!("from {ip}"),
        );
    }
}

// docs/spec.md story 89 (Test packets from multicast or broadcast
// addresses are dropped: an Activation Request from one gets no answer and
// no Load PDUs)
#[test]
fn an_activation_request_from_a_group_address_is_dropped() {
    for (seed, ip) in (120..).zip(group_addresses()) {
        let outcome = unauthenticated(seed)
            .activation_pdus_from(ip)
            .run_for(Duration::from_millis(200));
        outcome.check(
            outcome.server_dropped_activation_because(SilentRejectionReason::MulticastOrBroadcast)
                && outcome.activation_responses().is_empty()
                && outcome.load_pdus_to_client().is_empty(),
            &format!("from {ip}"),
        );
    }
}
