//! Reproducible Data Phase layout workload with fixed Load PDU counts.

#[path = "../src/protocol/mod.rs"]
#[allow(dead_code, unused_imports)]
mod protocol;

#[path = "../src/wire/mod.rs"]
#[allow(dead_code, unused_imports)]
mod wire;
use std::hint::black_box;
use std::mem::size_of;
use std::time::{Duration, Instant};

use crate::protocol::layout_diagnostics::{
    layouts, lookback_padding, now, owned_bytes, phase, unreported_datagrams,
};
use crate::protocol::{DataEvent, DataPhase, Now, RxBatch, RxDatagram, RxPdu};
use crate::wire::load::{LOAD_HEADER_LEN, LoadHeader, test_action};
use crate::wire::status::STATUS_PDU_LEN;

#[cfg(not(feature = "hotpath-alloc"))]
#[global_allocator]
static ALLOCATOR: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

// A fixed-rate Status PDU, formed independently from RFC 9946 section 8.2.
// No Load Receiver is used to generate the Load Sender's feedback.
fn feedback(at: Now, seq: u32) -> [u8; STATUS_PDU_LEN] {
    let mut pdu = [0u8; STATUS_PDU_LEN];
    pdu[..2].copy_from_slice(&[0xFE, 0xED]);
    pdu[2] = test_action::TEST;
    for (offset, value) in [
        (4, seq),
        (8, 100),
        (12, 1222),
        (16, 10),
        (140, 50_000),
        (152, at.wall.as_unix().as_secs() as u32),
        (156, at.wall.as_unix().subsec_nanos()),
    ] {
        pdu[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }
    pdu[163] = 1;
    pdu
}

#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn sender_turn(phase: &mut DataPhase, at: Now, buf: &mut [u8]) -> u64 {
    let mut datagrams = 0;
    while let Some(plan) = phase.poll_burst(at, Duration::ZERO) {
        phase.write_datagrams(&plan, at, buf);
        // Observe the written bytes, preventing elimination of payload work.
        black_box(&buf[..plan.bytes()]);
        phase.report_sent(&plan, plan.datagrams(), at);
        datagrams += u64::from(plan.datagrams());
    }
    datagrams
}

#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn receiver_turn(phase: &mut DataPhase, at: Now, batch: &[RxDatagram]) {
    phase.handle_load_batch(RxBatch {
        before: at,
        after: at,
        datagrams: black_box(batch),
    });
}

fn drain(phase: &mut DataPhase) {
    while let Some(event) = phase.poll_event() {
        // Invalid traffic must not silently become a faster workload.
        assert!(!matches!(
            event,
            DataEvent::InvalidLoadPdus { .. }
                | DataEvent::IgnoredLoadPdus { .. }
                | DataEvent::InvalidStatusPdus { .. }
                | DataEvent::Ended { .. }
        ));
        black_box(event);
    }
}

#[cfg(not(feature = "hotpath-alloc"))]
fn stats() -> stats_alloc::Stats {
    stats_alloc::INSTRUMENTED_SYSTEM.stats()
}

#[cfg_attr(feature = "hotpath", hotpath::main)]
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let role = args.first().map(String::as_str).unwrap_or("receiver");
    assert!(matches!(role, "sender" | "receiver"));
    let connections: usize = args.get(1).map_or(1, |s| s.parse().unwrap());
    let turns: u64 = args.get(2).map_or(10_000, |s| s.parse().unwrap());
    assert!((1..=255).contains(&connections));
    assert!((1..=40_000).contains(&turns));
    assert!(
        args.len() <= 3,
        "usage: layout_cost [sender|receiver [1..255 [1..40000]]]"
    );
    for (name, size, alignment) in layouts() {
        println!("layout,{name},{size},{alignment}");
    }
    println!("padding,Lookback,{}", lookback_padding());
    println!("profiling,{}", cfg!(feature = "hotpath"));
    #[cfg(not(feature = "hotpath-alloc"))]
    let before = stats();
    let mut phases: Vec<_> = (0..connections)
        .map(|n| phase(role == "receiver", n as u64 + 1))
        .collect();
    #[cfg(not(feature = "hotpath-alloc"))]
    {
        let delta = stats() - before;
        println!(
            "constructed,{role},{connections},{},{}",
            delta.allocations, delta.bytes_allocated
        );
    }
    let mut buf = vec![0u8; 64 * 1222];
    let mut batch = [RxDatagram {
        header: [0; LOAD_HEADER_LEN],
        actual_len: 1222,
        kernel_wall: None,
        tos: Some(0),
    }; 64];
    let mut seq = 1u32;
    let mut datagrams = 0u64;
    let mut received_reported = 0u64;
    // 1.1 s warm-up passes the first Status PDU and Sub-Interval.
    let warmup = 11_000;
    let mut elapsed = Duration::ZERO;
    #[cfg(not(feature = "hotpath-alloc"))]
    let mut measured = stats();
    let mut started = Instant::now();
    for turn in 0..warmup + turns {
        if turn == warmup {
            println!(
                "warmed-storage,{role},{},{}",
                size_of::<DataPhase>(),
                owned_bytes(&phases[0])
            );
            #[cfg(not(feature = "hotpath-alloc"))]
            {
                measured = stats();
            }
            datagrams = 0;
            started = Instant::now();
        }
        let at = now(turn * 100);
        if role == "receiver" {
            for d in &mut batch {
                LoadHeader {
                    seq_no: seq,
                    lpdu_time: (
                        at.wall.as_unix().as_secs() as u32,
                        at.wall.as_unix().subsec_nanos(),
                    ),
                    udp_payload: 1222,
                    ..LoadHeader::default()
                }
                .encode_into(&mut d.header);
                d.kernel_wall = Some(at.wall);
                seq = seq.wrapping_add(1);
            }
        }
        for phase in &mut phases {
            if role == "receiver" {
                receiver_turn(phase, at, &batch);
                datagrams += batch.len() as u64;
                while let Some(len) = phase.poll_transmit(at, &mut buf) {
                    // RFC 9946 section 8.2: tiRxDatagrams, bytes 144..148.
                    assert_eq!(len, STATUS_PDU_LEN);
                    received_reported +=
                        u64::from(u32::from_be_bytes(buf[144..148].try_into().unwrap()));
                    black_box(&buf[..len]);
                }
            } else {
                // A fixed-rate Status PDU every Trial Interval
                // keeps the Load Sender live and exercises its feedback path.
                if turn % 500 == 0 {
                    let status = feedback(at, (turn / 500 + 1) as u32);
                    phase.handle_pdu(RxPdu {
                        now: at,
                        kernel_wall: Some(at.wall),
                        pdu: &status,
                    });
                }
                datagrams += sender_turn(phase, at, &mut buf);
            }
            drain(phase);
        }
        if turn + 1 == warmup + turns {
            elapsed = started.elapsed();
        }
    }
    #[cfg(not(feature = "hotpath-alloc"))]
    {
        let delta = stats() - measured;
        println!(
            "steady,{},{},{},{}",
            delta.allocations, delta.reallocations, delta.bytes_allocated, delta.bytes_deallocated
        );
        // ADR 0008: warm steady-state Data Phase work must not allocate.
        if !cfg!(feature = "hotpath") {
            assert_eq!((delta.allocations, delta.reallocations), (0, 0));
        }
    }
    let per_turn = if role == "receiver" { 64 } else { 10 };
    assert_eq!(
        datagrams,
        turns * connections as u64 * per_turn,
        "the timed span must offer the fixed number of Load PDUs"
    );
    if role == "receiver" {
        let actual = received_reported + phases.iter().map(unreported_datagrams).sum::<u64>();
        let expected = (warmup + turns) * connections as u64 * 64;
        assert_eq!(
            actual, expected,
            "Status PDUs and the open Trial Interval must account for every offered Load PDU"
        );
        println!("verified,receiver,{actual}");
    } else {
        println!("verified,sender,{datagrams}");
    }
    println!(
        "sample,{role},{connections},{turns},{datagrams},{}",
        elapsed.as_nanos()
    );
    black_box(phases);
}
