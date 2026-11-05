//! A scripted host's interruption, either an injected answer or a real
//! SIGINT handled by the production signal listener in a child process.

use bench_rounds::interrupt::Flag;

#[derive(Clone, Default)]
pub struct Interrupt {
    pub flag: Flag,
    real: bool,
}

impl Interrupt {
    #[cfg(target_os = "linux")]
    pub fn installed(flag: Flag) -> Self {
        Self { flag, real: true }
    }

    pub fn raise(&self) {
        if !self.real {
            self.flag.raise("SIGINT");
            return;
        }
        #[cfg(target_os = "linux")]
        {
            use std::time::{Duration, Instant};

            use nix::sys::signal::{Signal, kill};
            use nix::unistd::Pid;

            assert!(!self.flag.raised(), "only the selected call may interrupt");
            kill(Pid::this(), Signal::SIGINT).unwrap();
            let began = Instant::now();
            while !self.flag.raised() {
                assert!(
                    began.elapsed() < Duration::from_secs(5),
                    "SIGINT was not handled"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(self.flag.cause(), Some("SIGINT"));
            assert_eq!(self.flag.count(), 1);
        }
        #[cfg(not(target_os = "linux"))]
        unreachable!("real per-step signals run in Linux children");
    }
}
