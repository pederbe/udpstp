//! Lost Status PDUs as the Data Phase reports them (RFC 9946 §8.1, §8.2;
//! OB-UDPST's WARN_LOC_STATUS and WARN_REM_STATUS; ): the Load Sender
//! counts gaps in spduSeqNo, and the Load Receiver counts Load PDUs whose
//! spduSeqErr says the Load Sender missed some.

mod harness;

use std::time::Duration;

use harness::{DataEvent, Outcome, RowRequest, Scenario};
use udpstp_core::Direction;
use udpstp_core::SocketId;

const STATUS: [u8; 2] = [0xFE, 0xED];

fn test(seed: u64, direction: Direction) -> Outcome {
    // Two Trial Intervals' Status PDUs are lost.
    Scenario::new(seed)
        .operator(7, b"an operator key")
        .client_direction(direction)
        .client_requests_row(RowRequest::FixedRate(5))
        .network_drops(
            STATUS,
            Duration::from_millis(300),
            Some(Duration::from_millis(400)),
        )
        .run_for(Duration::from_millis(800))
}

fn counted(events: &[(SocketId, DataEvent)], pick: fn(&DataEvent) -> Option<u32>) -> u32 {
    events.iter().filter_map(|(_, e)| pick(e)).sum()
}

// spduSeqNo lets the Load Sender detect lost Status PDUs.
#[udpstp_req::req("RFC9946-8.1-L8")]
#[test]
fn a_load_sender_counts_the_status_pdus_it_never_received() {
    let outcome = test(1, Direction::Upstream);
    let missing = counted(&outcome.client_data_events, |e| match e {
        DataEvent::StatusPdusMissing { count } => Some(*count),
        _ => None,
    });
    outcome.check((1..=3).contains(&missing), &format!("{missing} missing"));
}

// spduSeqErr reports the Status PDUs the Load Sender missed.
#[udpstp_req::req("RFC9946-8.1-L8")]
#[test]
fn a_load_receiver_counts_load_pdus_reporting_missed_status_pdus() {
    let outcome = test(2, Direction::Downstream);
    let reported = counted(&outcome.client_data_events, |e| match e {
        DataEvent::PeerMissedStatusPdus { count } => Some(*count),
        _ => None,
    });
    outcome.check(reported > 0, &format!("{reported} reported"));
}
