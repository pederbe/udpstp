#![forbid(unsafe_code)]
//! The Server as a Windows service (ADR 0014): `udpstp server
//! --windows-service`, which the Service Control Manager starts. It runs
//! the same Server as a console does, but stops on the service's stop and
//! shutdown controls instead of the console's events, which a service gets
//! too and mustn't act on, and reports its state to the Service Control
//! Manager: starting, running once it listens, stopping, and stopped with
//! its exit status. It logs to `log_file`, since a service has no stdout.
//!
//! The dispatcher and the control handler are the `windows-service`
//! crate's safe functions. Its `define_windows_service!` macro would expand
//! `unsafe` into this crate, so the service's entry point is written here,
//! never reading the arguments the Service Control Manager passes: the
//! Server's options come from the service's command line.

use std::time::Duration;

use crate::names::stop_reason;

/// The service's name, which its virtual account `NT SERVICE\udpstp-server`
/// and the firewall rule's service filter follow.
#[cfg_attr(not(windows), allow(dead_code))]
pub const NAME: &str = "udpstp-server";

/// What the Service Control Manager is told of the Server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Started, not yet listening.
    Starting,
    /// Listening.
    Running,
    /// Stopping its Tests.
    Stopping,
    /// Stopped, with the exit status it would have had in a console.
    Stopped { exit_status: u8 },
}

/// A service's state, as the Service Control Manager takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    StartPending,
    Running,
    StopPending,
    Stopped,
}

/// [`Phase`] as the Service Control Manager wants it reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub state: State,
    /// Whether it takes the stop and shutdown controls now: only while
    /// running, so neither interrupts its start or its stop.
    pub accepts_stop: bool,
    /// How long the Service Control Manager should wait before it expects
    /// another report, while starting or stopping.
    pub wait_hint: Duration,
    /// The service-specific exit code, where it stopped with a status
    /// other than 0.
    pub exit_status: Option<u8>,
}

/// How long a start may take before it reports again: reading the key
/// file and probing the socket buffers.
const START_WAIT_HINT: Duration = Duration::from_secs(10);

/// How long a stop may take: the Server stops its Tests and waits at most
/// 2 s for them to end (`udpstp-runtime`'s `STOP_WAIT`).
const STOP_WAIT_HINT: Duration = Duration::from_secs(5);

impl Phase {
    pub fn status(self) -> Status {
        let status = |state, wait_hint| Status {
            state,
            accepts_stop: false,
            wait_hint,
            exit_status: None,
        };
        match self {
            Self::Starting => status(State::StartPending, START_WAIT_HINT),
            Self::Running => Status {
                accepts_stop: true,
                ..status(State::Running, Duration::ZERO)
            },
            Self::Stopping => status(State::StopPending, STOP_WAIT_HINT),
            Self::Stopped { exit_status } => Status {
                exit_status: (exit_status != 0).then_some(exit_status),
                ..status(State::Stopped, Duration::ZERO)
            },
        }
    }
}

/// A control from the Service Control Manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Stop,
    Shutdown,
    /// Asks for the current status, which needs no answer of its own.
    Interrogate,
    /// Any other, which the Server doesn't take.
    Other,
}

/// What the control handler makes of a control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handled {
    /// It stops the Server, which logs this reason.
    Stop(&'static str),
    /// Taken, with nothing to do.
    Done,
    /// Not taken.
    NotImplemented,
}

pub fn handle(control: Control) -> Handled {
    match control {
        Control::Stop => Handled::Stop(stop_reason::SERVICE_STOP),
        Control::Shutdown => Handled::Stop(stop_reason::SERVICE_SHUTDOWN),
        Control::Interrogate => Handled::Done,
        Control::Other => Handled::NotImplemented,
    }
}

#[cfg(windows)]
pub use scm::run;

/// The glue to the Service Control Manager, through `windows-service`.
#[cfg(windows)]
mod scm {
    use std::process::ExitCode;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU8, Ordering};

    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_dispatcher;

    use super::{Control, Handled, NAME, Phase, State};
    use crate::interrupt::errln;
    use crate::names::stop_reason;
    use crate::{ServerArgs, color, exit};

    /// The command line, for the service's entry point, which the
    /// dispatcher calls on a thread of its own.
    static ARGS: OnceLock<(ServerArgs, color::Mode)> = OnceLock::new();

    /// The Server's exit status, once the service has stopped.
    static EXIT_STATUS: AtomicU8 = AtomicU8::new(exit::INITIALIZATION_ERROR);

    /// Hands this process to the Service Control Manager, which calls the
    /// service's entry point and returns once it has stopped; an error
    /// where the process wasn't started as a service.
    pub fn run(args: ServerArgs, mode: color::Mode) -> ExitCode {
        let _ = ARGS.set((args, mode));
        match service_dispatcher::start(NAME, ffi_service_main) {
            Ok(()) => ExitCode::from(EXIT_STATUS.load(Ordering::SeqCst)),
            Err(error) => {
                errln!("{}", mode.stderr().diagnostic(color::Role::Error, &format!(
                    "udpstp: configuration error: --windows-service, but the Service Control Manager didn't start this process: {error}"
                )));
                ExitCode::from(exit::CONFIGURATION_ERROR)
            }
        }
    }

    /// The service's entry point. Its arguments, the start parameters, are
    /// never read, so it needs no `unsafe`.
    extern "system" fn ffi_service_main(_argc: u32, _argv: *mut *mut u16) {
        if let Some((args, mode)) = ARGS.get() {
            service_main(args, *mode);
        }
    }

    fn service_main(args: &ServerArgs, mode: color::Mode) {
        let (stops, mut stopped) = tokio::sync::mpsc::unbounded_channel();
        let handler = move |control: ServiceControl| {
            let control = match control {
                ServiceControl::Stop => Control::Stop,
                ServiceControl::Shutdown => Control::Shutdown,
                ServiceControl::Interrogate => Control::Interrogate,
                _ => Control::Other,
            };
            match super::handle(control) {
                Handled::Stop(reason) => {
                    let _ = stops.send(reason);
                    ServiceControlHandlerResult::NoError
                }
                Handled::Done => ServiceControlHandlerResult::NoError,
                Handled::NotImplemented => ServiceControlHandlerResult::NotImplemented,
            }
        };
        let Ok(handle) = service_control_handler::register(NAME, handler) else {
            return;
        };
        let report = move |phase: Phase| {
            let status = phase.status();
            let _ = handle.set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: match status.state {
                    State::StartPending => ServiceState::StartPending,
                    State::Running => ServiceState::Running,
                    State::StopPending => ServiceState::StopPending,
                    State::Stopped => ServiceState::Stopped,
                },
                controls_accepted: if status.accepts_stop {
                    ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
                } else {
                    ServiceControlAccept::empty()
                },
                exit_code: match status.exit_status {
                    Some(code) => ServiceExitCode::ServiceSpecific(code.into()),
                    None => ServiceExitCode::Win32(0),
                },
                checkpoint: u32::from(status.wait_hint > std::time::Duration::ZERO),
                wait_hint: status.wait_hint,
                process_id: None,
            });
        };
        report(Phase::Starting);
        let stop = Box::pin(async move {
            let reason = stopped.recv().await.unwrap_or(stop_reason::SERVICE_STOP);
            report(Phase::Stopping);
            reason
        });
        let exit_status = crate::run_server(args, mode, Some(stop), || report(Phase::Running));
        EXIT_STATUS.store(exit_status, Ordering::SeqCst);
        report(Phase::Stopped { exit_status });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    //  (the Service Control Manager is told the Server starts, runs
    // once it listens, stops, and stopped with its exit status; it takes
    // the stop and shutdown controls only while running)
    #[test]
    fn each_phase_reports_its_state() {
        assert_eq!(
            Phase::Starting.status(),
            Status {
                state: State::StartPending,
                accepts_stop: false,
                wait_hint: START_WAIT_HINT,
                exit_status: None
            }
        );
        assert_eq!(
            Phase::Running.status(),
            Status {
                state: State::Running,
                accepts_stop: true,
                wait_hint: Duration::ZERO,
                exit_status: None
            }
        );
        assert_eq!(
            Phase::Stopping.status(),
            Status {
                state: State::StopPending,
                accepts_stop: false,
                wait_hint: STOP_WAIT_HINT,
                exit_status: None
            }
        );
        assert_eq!(
            Phase::Stopped { exit_status: 0 }.status().exit_status,
            None,
            "a clean stop is Win32 exit code 0"
        );
        assert_eq!(
            Phase::Stopped { exit_status: 51 }.status().exit_status,
            Some(51),
            "a key file error is the service-specific exit code 51"
        );
    }

    //  (the stop and shutdown controls stop the Server, each by its own
    // name in the log; others are left to the Service Control Manager)
    #[test]
    fn the_stop_and_shutdown_controls_stop_the_server() {
        assert_eq!(
            handle(Control::Stop),
            Handled::Stop(stop_reason::SERVICE_STOP)
        );
        assert_eq!(
            handle(Control::Shutdown),
            Handled::Stop(stop_reason::SERVICE_SHUTDOWN)
        );
        assert_eq!(handle(Control::Interrogate), Handled::Done);
        assert_eq!(handle(Control::Other), Handled::NotImplemented);
    }

    //  (a clean stop leaves the Server time to stop its Tests: at
    // least the runtime's 2 s wait for them)
    #[test]
    fn the_stop_wait_hint_covers_the_servers_stop() {
        assert!(STOP_WAIT_HINT >= Duration::from_secs(3));
    }
}
