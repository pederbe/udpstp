//! The Key Table: RFC 7210 rows with send and accept lifetimes, and keyIds
//! shared across non-overlapping lifetimes (RFC 9946 §5.4, RFC 7210 §2-§3,
//! ADR 0010).

mod harness;

use std::time::Duration;

use harness::{
    ControlPhaseFailure, KeyTableError, Lifetime, Scenario, SilentRejectionReason, key_row,
    load_keys,
};

const KEY_ID: u8 = 7;
const KEY: &[u8] = b"a shared key";
const RUN: Duration = Duration::from_secs(1);
/// Every host's wall clock at the start (harness default), in seconds.
const T0: u64 = harness::WALL_START_UNIX_SECS;
/// keyId's offset in the Test Setup PDU (RFC 9946 Figure 2).
const SETUP_KEY_ID: usize = 52;

type Span = (Option<u64>, Option<u64>);
const ALWAYS: Span = (None, None);

/// A mode 1 Setup where the Server's only row for KEY_ID has `accept`.
fn server_accepting(seed: u64, accept: Span) -> harness::Outcome {
    let keys = load_keys([key_row("server", KEY_ID, KEY, ALWAYS, accept)]);
    Scenario::new(seed)
        .server_keys(keys.expect("one row"))
        .client_mode1(KEY_ID, KEY)
        .run_for(RUN)
}

// RFC 7210 §3 (a key is selected for incoming messages only when
// AcceptLifeTimeStart <= current time <= AcceptLifeTimeEnd)
#[udpstp_req::req("RFC9946-5.4-3", "RFC9946-5.3.1-6")]
#[test]
fn a_key_outside_its_accept_lifetime_is_an_authentication_failure() {
    let cases = [
        ("not yet valid", (Some(T0 + 1), None)),
        ("expired", (None, Some(T0 - 1))),
    ];
    for (seed, (what, accept)) in (1..).zip(cases) {
        let outcome = server_accepting(seed, accept);

        outcome.check(
            outcome.sent_by(harness::SERVER_ADDR).is_empty(),
            &format!("{what}: the Server stayed silent"),
        );
        outcome.check(
            outcome.server_dropped_setup_because(SilentRejectionReason::KeyOutsideLifetime),
            &format!("{what}: dropped as KeyOutsideLifetime"),
        );
    }
}

// RFC 7210 §3 (the lifetime bounds are inclusive). The simulated Setup
// arrives within the first second after T0.
#[udpstp_req::req("RFC9946-5.4-3")]
#[test]
fn a_key_is_accepted_on_the_edges_of_its_accept_lifetime() {
    let cases = [
        ("starting now", (Some(T0), None)),
        ("ending now", (None, Some(T0))),
    ];
    for (seed, (what, accept)) in (10..).zip(cases) {
        let outcome = server_accepting(seed, accept);

        outcome.check(
            outcome.client_accepted_test_port().is_some(),
            &format!("{what}: the Setup was accepted"),
        );
    }
}

// docs/spec.md Key table (entries may share a keyId if their lifetimes don't
// overlap); RFC 7210 §3 (rollover)
#[udpstp_req::req("RFC9946-5.4-3")]
#[test]
fn a_shared_key_id_uses_the_row_valid_at_the_time() {
    let rollover = T0 + 100;
    let old = (None, Some(rollover - 1));
    let new = (Some(rollover), None);
    let server = || {
        load_keys([
            key_row("old", KEY_ID, b"old key", old, old),
            key_row("new", KEY_ID, b"new key", new, new),
        ])
        .expect("the rows don't overlap")
    };
    let cases = [
        (
            "before the rollover, the old key",
            T0,
            b"old key".as_slice(),
            true,
        ),
        (
            "after it, the new key",
            rollover,
            b"new key".as_slice(),
            true,
        ),
        (
            "after it, not the old key",
            rollover,
            b"old key".as_slice(),
            false,
        ),
    ];
    for (seed, (what, wall, client_key, accepted)) in (20..).zip(cases) {
        let outcome = Scenario::new(seed)
            .wall_start(wall)
            .server_keys(server())
            .client_mode1(KEY_ID, client_key)
            .run_for(RUN);

        outcome.check(
            outcome.client_accepted_test_port().is_some() == accepted,
            &format!("{what}: accepted {accepted}"),
        );
        if !accepted {
            outcome.check(
                outcome.server_dropped_setup_because(SilentRejectionReason::AuthenticationFailed),
                &format!("{what}: the new row's key doesn't verify it"),
            );
        }
    }
}

// docs/spec.md Key table (overlapping entries for one keyId are rejected
// when the table is loaded)
#[test]
fn rows_sharing_a_key_id_must_not_overlap() {
    let first = (None, Some(T0 + 10));
    let later = (Some(T0 + 11), None);
    let touching = (Some(T0 + 10), None);
    let cases = [
        ("accept lifetimes overlap", later, touching, true),
        ("send lifetimes overlap", touching, later, true),
        ("both open at both ends", ALWAYS, ALWAYS, true),
        ("adjacent lifetimes", later, later, false),
    ];
    for (what, send, accept, rejected) in cases {
        let loaded = load_keys([
            key_row("first", KEY_ID, KEY, first, first),
            key_row("second", KEY_ID, KEY, send, accept),
        ]);
        let expected = rejected.then(|| KeyTableError::OverlappingLifetimes {
            key_id: KEY_ID,
            names: ("first".into(), "second".into()),
        });
        assert_eq!(loaded.err(), expected, "{what}");
    }
    let other_ids = load_keys([
        key_row("first", 1, KEY, ALWAYS, ALWAYS),
        key_row("second", 2, KEY, ALWAYS, ALWAYS),
    ]);
    assert!(other_ids.is_ok(), "rows with other keyIds may overlap");
}

// RFC 7210 §2 (a lifetime is a start and an end)
#[test]
fn a_lifetime_ending_before_it_starts_is_rejected() {
    let backwards = (Some(T0 + 10), Some(T0));
    let loaded = load_keys([key_row("backwards", KEY_ID, KEY, ALWAYS, backwards)]);
    assert_eq!(
        loaded.err(),
        Some(KeyTableError::EmptyLifetime {
            name: "backwards".into()
        })
    );
    let one_second = (Some(T0), Some(T0));
    assert!(
        load_keys([key_row("one second", KEY_ID, KEY, ALWAYS, one_second)]).is_ok(),
        "a lifetime of one second is not empty"
    );
}

/// The keyId of the first Setup Request the Client sent, if any.
fn sent_key_id(outcome: &harness::Outcome) -> Option<u8> {
    let sent = outcome.sent_by(harness::CLIENT_ADDR);
    sent.first()
        .and_then(|t| t.payload.get(SETUP_KEY_ID).copied())
}

// RFC 7210 §3 (a sender selects a key whose send lifetime includes the
// current time, preferring the most recent SendLifetimeStart)
#[udpstp_req::req("RFC9946-5.4-3")]
#[test]
fn the_client_sends_with_the_newest_valid_key() {
    let client = load_keys([
        key_row("older", 1, b"key one", (Some(T0 - 100), None), ALWAYS),
        key_row("newer", 2, b"key two", (Some(T0 - 10), None), ALWAYS),
        key_row("future", 3, b"key three", (Some(T0 + 10), None), ALWAYS),
        key_row(
            "expired",
            4,
            b"key four",
            (Some(T0 - 5), Some(T0 - 1)),
            ALWAYS,
        ),
    ]);
    let outcome = Scenario::new(30)
        .server_key(2, b"key two")
        .client_keys(client.expect("no overlaps"), None)
        .run_for(RUN);

    outcome.check(
        sent_key_id(&outcome) == Some(2),
        "the Setup Request carries keyId 2",
    );
    outcome.check(
        outcome.client_accepted_test_port().is_some(),
        "and is signed with that key",
    );
}

// RFC 7210 §3 (no key whose send lifetime includes the current time)
#[test]
fn a_client_without_a_valid_send_key_does_not_start() {
    let client = load_keys([key_row(
        "future",
        KEY_ID,
        KEY,
        (Some(T0 + 10), None),
        ALWAYS,
    )]);
    let outcome = Scenario::new(31)
        .server_key(KEY_ID, KEY)
        .client_keys(client.expect("one row"), None)
        .run_for(RUN);

    outcome.check(
        outcome.trace.is_empty(),
        "no Setup Request was sent: nothing was",
    );
    outcome.check(
        outcome.client_failed_because(ControlPhaseFailure::NoKeyForSending),
        "the Client reported that no key is valid for sending",
    );
}

/// A Client table like a loaded OB-UDPST key file: rows without lifetimes.
fn two_timeless_rows() -> harness::KeyTable {
    load_keys([
        key_row("first", 1, b"key one", ALWAYS, ALWAYS),
        key_row("second", 2, b"key two", ALWAYS, ALWAYS),
    ])
    .expect("different keyIds")
}

// RFC 7210 §3 (the newest SendLifetimeStart decides; equal ones leave
// the choice open, so the Client doesn't guess)
#[test]
fn equally_new_send_keys_need_a_pinned_key_id() {
    let outcome = Scenario::new(32)
        .server_key(1, b"key one")
        .client_keys(two_timeless_rows(), None)
        .run_for(RUN);

    outcome.check(
        outcome.trace.is_empty(),
        "no Setup Request was sent: nothing was",
    );
    outcome.check(
        outcome.client_failed_because(ControlPhaseFailure::AmbiguousKeyForSending),
        "the Client reported the ambiguity",
    );

    let outcome = Scenario::new(33)
        .server_key(1, b"key one")
        .client_keys(two_timeless_rows(), Some(1))
        .run_for(RUN);
    outcome.check(
        sent_key_id(&outcome) == Some(1) && outcome.client_accepted_test_port().is_some(),
        "with keyId 1 pinned, the Client sends with it",
    );
}

// RFC 7210 §2 (lifetimes are written YYYYMMDDHHMMSSZ, in UTC). The
// expected seconds come from Python's calendar.timegm.
#[test]
fn rfc7210_times_convert_to_unix_seconds() {
    for (text, secs) in [
        ("19700101000000Z", 0),
        ("20000229235959Z", 951_868_799),
        ("20260921180000Z", harness::WALL_START_UNIX_SECS),
        ("21060207062816Z", 4_294_967_296),
        ("99991231235959Z", 253_402_300_799),
    ] {
        assert_eq!(Lifetime::parse_time(text), Some(secs), "{text}");
    }
    for bad in [
        "20260921180000",
        "2026092118000Z",
        "2026092118000aZ",
        "20260931000000Z",
        "20250229000000Z",
        "21000229000000Z",
        "20261301000000Z",
        "20260001000000Z",
        "20260100000000Z",
        "20260101240000Z",
        "20260101006000Z",
        "20261231235960Z",
        "19691231235959Z",
        "+2026010100000Z",
    ] {
        assert_eq!(Lifetime::parse_time(bad), None, "{bad}");
    }
}
