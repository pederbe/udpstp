//! Nonblocking engine reports. Disposable notices share a fixed queue
//! budget. Results are bounded by the Test's number of Sub-Intervals,
//! and lifecycle/accounting records by admitted Test Connections.

use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use udpstp_core::DataEvent;

use super::FromEngine;

const NOTICES: usize = 256;

struct Message {
    report: FromEngine,
    _permit: Option<OwnedSemaphorePermit>,
}

#[derive(Clone)]
pub(crate) struct Sender {
    sender: mpsc::UnboundedSender<Message>,
    notices: Arc<Semaphore>,
}

pub(crate) struct Receiver(mpsc::UnboundedReceiver<Message>);

pub(crate) fn channel() -> (Sender, Receiver) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (
        Sender {
            sender,
            notices: Arc::new(Semaphore::new(NOTICES)),
        },
        Receiver(receiver),
    )
}

impl Sender {
    pub(crate) fn send(&self, report: FromEngine) -> bool {
        let disposable = matches!(&report, FromEngine::Event { event, .. } if !reliable(event));
        let permit = if disposable {
            let Ok(permit) = self.notices.clone().try_acquire_owned() else {
                return false;
            };
            Some(permit)
        } else {
            None
        };
        self.sender
            .send(Message {
                report,
                _permit: permit,
            })
            .is_ok()
    }

    /// One final aggregate per counter kind and admitted Test Connection.
    fn final_counter(&self, socket: udpstp_core::SocketId, event: DataEvent) {
        let _ = self.sender.send(Message {
            report: FromEngine::Event { socket, event },
            _permit: None,
        });
    }
}

/// Counter notices that could not enter the shared queue. At most one
/// aggregate per event kind, so packet rate cannot grow retained memory.
#[derive(Default)]
pub(super) struct Counters(Vec<DataEvent>);

impl Counters {
    pub(super) fn retain(&mut self, event: DataEvent) {
        if matches!(
            event,
            DataEvent::TrialIntervalClassified(_) | DataEvent::LostStatusBackoff { .. }
        ) {
            return;
        }
        if let Some(total) = self
            .0
            .iter_mut()
            .find(|total| std::mem::discriminant(*total) == std::mem::discriminant(&event))
        {
            use DataEvent::*;
            match (total, event) {
                (
                    BurstsSkipped {
                        bursts: b,
                        datagrams: d,
                        bytes: n,
                    },
                    BurstsSkipped {
                        bursts,
                        datagrams,
                        bytes,
                    },
                )
                | (
                    BurstsWithheld {
                        bursts: b,
                        datagrams: d,
                        bytes: n,
                    },
                    BurstsWithheld {
                        bursts,
                        datagrams,
                        bytes,
                    },
                ) => {
                    *b = b.saturating_add(bursts);
                    *d = d.saturating_add(datagrams);
                    *n = n.saturating_add(bytes);
                }
                (InvalidLoadPdus { count: n }, InvalidLoadPdus { count })
                | (IgnoredLoadPdus { count: n }, IgnoredLoadPdus { count })
                | (
                    StatusPdusFailedVerification { count: n },
                    StatusPdusFailedVerification { count },
                )
                | (StatusPdusMissing { count: n }, StatusPdusMissing { count })
                | (PeerMissedStatusPdus { count: n }, PeerMissedStatusPdus { count })
                | (PeerRxStopped { count: n }, PeerRxStopped { count })
                | (DelaySamplesDropped { count: n }, DelaySamplesDropped { count })
                | (PathMtuExceeded { count: n }, PathMtuExceeded { count })
                | (HopLimitExceeded { count: n }, HopLimitExceeded { count })
                | (IcmpErrors { count: n }, IcmpErrors { count })
                | (InvalidStatusPdus { count: n }, InvalidStatusPdus { count })
                | (StaleStatusPdus { count: n }, StaleStatusPdus { count }) => {
                    *n = n.saturating_add(count)
                }
                (
                    SendingRateCapped {
                        structures: n,
                        highest_kbps: high,
                        ..
                    },
                    SendingRateCapped {
                        structures,
                        highest_kbps,
                        ..
                    },
                ) => {
                    *n = n.saturating_add(structures);
                    *high = (*high).max(highest_kbps);
                }
                (total, event) => *total = event,
            }
        } else {
            self.0.push(event);
        }
    }

    pub(super) fn finish(&mut self, report: &Sender, socket: udpstp_core::SocketId) {
        for event in self.0.drain(..) {
            report.final_counter(socket, event);
        }
    }
}

// Only these events have a finite per-Test bound. In particular, every
// peer-paced diagnostic counter and classification uses the notice budget.
fn reliable(event: &DataEvent) -> bool {
    matches!(
        event,
        DataEvent::SubIntervalMeasured(_)
            | DataEvent::Sent(_)
            | DataEvent::SentEstimated(_)
            | DataEvent::LoadBytes { .. }
            | DataEvent::TrialIntervalsCounted(_)
            | DataEvent::FirstLoadPduReceived
            | DataEvent::FirstStatusPduReceived
            | DataEvent::StopReceived
            | DataEvent::TrafficStopped { .. }
            | DataEvent::Ended(_)
    )
}

impl Receiver {
    pub(crate) async fn recv(&mut self) -> Option<FromEngine> {
        self.0.recv().await.map(|message| message.report)
    }

    #[cfg(test)]
    pub(crate) fn try_recv(&mut self) -> Result<FromEngine, mpsc::error::TryRecvError> {
        self.0.try_recv().map(|message| message.report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use udpstp_core::{EndReason, SocketId};

    #[test]
    fn saturated_reports_preserve_sender_result_and_warning_totals() {
        let (sender, mut receiver) = channel();
        for _ in 0..NOTICES {
            assert!(sender.send(FromEngine::Event {
                socket: SocketId(1),
                event: DataEvent::InvalidStatusPdus { count: 1 }
            }));
        }
        let mut counters = Counters::default();
        for _ in 0..10_000 {
            for event in [
                DataEvent::BurstsSkipped {
                    bursts: 1,
                    datagrams: 2,
                    bytes: 100,
                },
                DataEvent::BurstsWithheld {
                    bursts: 1,
                    datagrams: 3,
                    bytes: 200,
                },
                DataEvent::StatusPdusFailedVerification { count: 4 },
            ] {
                if !sender.send(FromEngine::Event {
                    socket: SocketId(1),
                    event,
                }) {
                    counters.retain(event);
                }
            }
        }
        counters.finish(&sender, SocketId(1));
        let events: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok())
            .filter_map(|message| match message {
                FromEngine::Event { event, .. } => Some(event),
                _ => None,
            })
            .collect();
        assert_eq!(events.len(), NOTICES + 3);
        assert_eq!(
            udpstp_core::LoadSender::of(udpstp_core::Direction::Upstream, &[&events]),
            udpstp_core::LoadSender::ThisHost {
                sent: 0,
                unsent: 50_000
            }
        );
        assert!(events.contains(&DataEvent::BurstsSkipped {
            bursts: 10_000,
            datagrams: 20_000,
            bytes: 1_000_000
        }));
        assert!(events.contains(&DataEvent::BurstsWithheld {
            bursts: 10_000,
            datagrams: 30_000,
            bytes: 2_000_000
        }));
        assert!(events.contains(&DataEvent::StatusPdusFailedVerification { count: 40_000 }));
    }

    #[test]
    fn a_stalled_consumer_bounds_notices_and_preserves_accounting_and_exit() {
        let (sender, mut receiver) = channel();
        for sequence in 0..10_000 {
            sender.send(FromEngine::Event {
                socket: SocketId(1),
                event: if sequence % 2 == 0 {
                    DataEvent::TrialIntervalClassified(udpstp_core::Classification {
                        spdu_seq_no: sequence,
                        class: udpstp_core::IntervalClass::Clean,
                        seq_errors: 0,
                        delay_var: Some(0),
                        row: None,
                    })
                } else {
                    DataEvent::InvalidStatusPdus { count: 1 }
                },
            });
        }
        sender.send(FromEngine::Event {
            socket: SocketId(1),
            event: DataEvent::LoadBytes { bytes: 1234 },
        });
        sender.send(FromEngine::Exited {
            socket: SocketId(1),
            reason: Some(EndReason::Completed),
            error: None,
            facts: Default::default(),
        });
        for _ in 0..NOTICES {
            assert!(matches!(
                receiver.try_recv(),
                Ok(FromEngine::Event {
                    event: DataEvent::InvalidStatusPdus { .. }
                        | DataEvent::TrialIntervalClassified(_),
                    ..
                })
            ));
        }
        assert!(matches!(
            receiver.try_recv(),
            Ok(FromEngine::Event {
                event: DataEvent::LoadBytes { bytes: 1234 },
                ..
            })
        ));
        assert!(matches!(receiver.try_recv(), Ok(FromEngine::Exited { .. })));
        assert!(receiver.try_recv().is_err());
        sender.send(FromEngine::Event {
            socket: SocketId(1),
            event: DataEvent::StaleStatusPdus { count: 2 },
        });
        assert!(receiver.try_recv().is_ok());
    }
}
