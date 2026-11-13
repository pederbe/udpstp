//! Authentication Mode 2 as the Client's default (docs/spec.md stories 17 and
//! 35-37; ): mode 1 only when chosen, per Server if need be; no
//! fallback; and a Server that rejects mode 2 without being able to sign
//! its answer is reported as probably not supporting it.

use super::harness;

use std::time::Duration;

use crate::protocol::Event;
use crate::wire::auth::hmac_sha256;
use harness::{ControlExchange, ControlPhaseFailure, ResponseIgnoreReason, Scenario};

const KEY_ID: u8 = 7;
const KEY: &[u8] = b"a shared key";

/// The authMode of every Setup Request, in order, and their Servers.
fn setup_auth_modes(outcome: &harness::Outcome) -> Vec<(std::net::IpAddr, u8)> {
    outcome
        .trace
        .iter()
        .filter(|t| t.payload.starts_with(&[0xAC, 0xE1]) && t.payload[8] == 1)
        .map(|t| (t.destination.ip(), t.payload[15]))
        .collect()
}

// RFC 9946 §6.2 (a Server that doesn't run the authMode answers
// AUTHINV); docs/spec.md stories 17 and 37 (a specific diagnostic, and no
// automatic fallback to mode 1)
#[udpstp_req::req("RFC9097-10-3", "RFC9946-11-3")]
#[test]
fn a_server_without_mode_2_is_reported_and_the_client_does_not_fall_back() {
    // As OB-UDPST answers mode 2: AUTHINV, signed with keys it never
    // derived.
    let outcome = Scenario::new(1)
        .client_mode2(KEY_ID, KEY)
        .scripted_server(|_, pdu| {
            (pdu.starts_with(&[0xAC, 0xE1]) && pdu[8] == 1).then(|| {
                let mut b = pdu.to_vec();
                b[8] = 2;
                b[9] = 6;
                b[20..52].fill(0);
                b[54..56].fill(0);
                let digest = hmac_sha256(&[0; 32], &b);
                b[20..52].copy_from_slice(&digest);
                b
            })
        })
        .run_for(Duration::from_secs(4));
    let events = &outcome.clients_events[0];
    outcome.check(
        events.contains(&Event::AuthMode2Unsupported)
            && events.contains(&Event::ControlPhaseFailed(
                ControlPhaseFailure::TestInitiationTimedOut {
                    exchange: ControlExchange::Setup,
                    last_ignored: Some(ResponseIgnoreReason::AuthenticationFailed),
                },
            )),
        &format!("the diagnostic, then the timer: {events:?}"),
    );
    let modes = setup_auth_modes(&outcome);
    outcome.check(
        modes.len() == 1 && modes[0].1 == 2,
        &format!("one Setup Request, in mode 2: {modes:?}"),
    );
}

// docs/spec.md story 36 (mode 1 chosen explicitly, per Server)
#[test]
fn mode_1_is_used_only_with_the_server_it_was_chosen_for() {
    let outcome = Scenario::new(2)
        .server_key(KEY_ID, KEY)
        .client_mode2(KEY_ID, KEY)
        .server_config(|c| c.test_connections_per_test = 8)
        .add_server(|c| c.test_connections_per_test = 8)
        .test_plan(|p| p.auth_mode_1.push(p.servers[1]))
        .coordinated(2, 2)
        .run_for(Duration::from_millis(500));
    let second = harness::extra_server_addr(0, false).ip();
    let modes = setup_auth_modes(&outcome);
    outcome.check(
        modes.len() == 2
            && modes
                .iter()
                .all(|&(server, mode)| mode == if server == second { 1 } else { 2 }),
        &format!("mode 2 with the first Server, 1 with the second: {modes:?}"),
    );
    let activated = outcome
        .client_connection_events
        .iter()
        .filter(|(_, e)| matches!(e, Event::TestActivated { .. }))
        .count();
    outcome.check(activated == 2, "both activated");
}
