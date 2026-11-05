//! A finite diagnostic workload for ADR 0008's optional UDP GRO experiment.
//!
//! These datagrams are not RFC Load PDUs. Packet conservation, software
//! timestamp diagnostics and physical acceptance are separate results.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::SocketAddrV4;
use std::path::{Path, PathBuf};

use bench_rounds::interrupt::Flag;
use clap::{Args as ClapArgs, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(target_os = "linux")]
mod linux;
pub mod model;

pub use model::*;

/// The receive workload and saved-result evaluator.
#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub operation: Operation,
}

#[derive(Debug, Subcommand)]
pub enum Operation {
    /// Run one bounded diagnostic pair on two existing static hosts.
    Pair(Box<crate::receive_pair::Args>),
    /// Send a finite diagnostic sequence. Never enables UDP GSO.
    Send(Workload),
    /// Receive that sequence with header-only or UDP GRO reads.
    Receive {
        #[command(flatten)]
        workload: Workload,
        #[arg(long, value_enum)]
        mode: ReceiveMode,
    },
    /// Compare saved results without opening a socket.
    Evaluate {
        #[arg(long)]
        sender: PathBuf,
        #[arg(long)]
        receiver: PathBuf,
        /// Optional versioned physical-host evidence with raw receipt hashes.
        #[arg(long)]
        host: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
}

/// Explicit finite inputs. Address/port pairs are reversed at the other end.
#[derive(Clone, Debug, ClapArgs, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workload {
    #[arg(long)]
    pub bind: SocketAddrV4,
    #[arg(long)]
    pub peer: SocketAddrV4,
    /// A nonzero diagnostic identifier, as an unsigned decimal integer.
    #[arg(long)]
    pub run_id: u64,
    /// Source revision, exactly 40 hexadecimal characters.
    #[arg(long)]
    pub source: String,
    #[arg(long)]
    pub count: u64,
    #[arg(long, default_value_t = 1_000_000)]
    pub pps: u64,
    /// Hard lifetime from socket readiness, including startup and drain.
    #[arg(long)]
    pub deadline_ms: u64,
    /// Receiver waits this long after the last datagram, within the deadline.
    #[arg(long, default_value_t = 500)]
    pub drain_ms: u64,
    /// Sender waits this long before its absolute scheduling grid starts.
    #[arg(long, default_value_t = 100)]
    pub start_delay_ms: u64,
    /// Pin this application to one permitted logical CPU, without privileges.
    #[arg(long)]
    pub cpu: usize,
    #[arg(long, default_value_t = 16_777_216)]
    pub socket_buffer: usize,
    #[arg(long)]
    pub out: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ReceiveMode {
    HeaderOnly,
    Gro,
}

impl Workload {
    pub fn validate(&self) -> Result<(), String> {
        if self.bind.ip().is_unspecified()
            || self.peer.ip().is_unspecified()
            || self.bind.port() == 0
            || self.peer.port() == 0
            || self.bind == self.peer
        {
            return Err(
                "explicit distinct unicast IPv4 addresses and nonzero ports are required".into(),
            );
        }
        if self.bind.ip().is_multicast()
            || self.peer.ip().is_multicast()
            || self.bind.ip().is_broadcast()
            || self.peer.ip().is_broadcast()
        {
            return Err("multicast and broadcast are outside this workload".into());
        }
        if self.run_id == 0
            || self.source.len() != 40
            || !self.source.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err("a nonzero run ID and 40-digit source revision are required".into());
        }
        if !(1..=120_000_000).contains(&self.count) || !(1..=1_000_000).contains(&self.pps) {
            return Err("count must be 1..120000000 and rate 1..1000000 pps".into());
        }
        if self.count.div_ceil(self.pps) > 120
            || !(1..=180_000).contains(&self.deadline_ms)
            || !(1..=10_000).contains(&self.drain_ms)
            || self.start_delay_ms > 10_000
            || self.deadline_ms <= self.start_delay_ms
            || self.socket_buffer == 0
            || self.socket_buffer > 64 * 1024 * 1024
        {
            return Err("workload duration/buffers exceed the finite diagnostic limits".into());
        }
        Ok(())
    }
}

/// Runs one command. A complete, ineligible observation is still useful output.
pub fn run(args: &Args, flag: &Flag) -> Result<(), String> {
    #[cfg(feature = "hotpath")]
    let _profile = hotpath::HotpathGuardBuilder::new("receive-probe").build();
    match &args.operation {
        Operation::Pair(args) => crate::receive_pair::run(args, flag).and_then(|code| {
            if code == 0 { Ok(()) } else { Err(format!("pair stopped with status {code}; inspect outcome.json and retained workspace receipts")) }
        }),
        Operation::Evaluate {
            sender,
            receiver,
            host,
            out,
        } => {
            let s: ProbeResult = read_json(sender)?;
            let r: ProbeResult = read_json(receiver)?;
            let h: Option<HostEvidence> = host.as_deref().map(read_json).transpose()?;
            if let (Some(path), Some(evidence)) = (host, &h) {
                evidence.verify_files(path.parent().unwrap_or(Path::new(".")))?;
            }
            let evaluation = evaluate(&s, &r, h.as_ref());
            write_json(out, &evaluation)
        }
        Operation::Send(workload) | Operation::Receive { workload, .. } => {
            workload.validate()?;
            #[cfg(target_os = "linux")]
            {
                let mode = match &args.operation {
                    Operation::Receive { mode, .. } => Some(*mode),
                    _ => None,
                };
                linux::run(workload, mode, flag)
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = flag;
                Err("receive-probe sockets require native Linux; saved-result evaluation is portable".into())
            }
        }
    }
}

pub(crate) const JSON_LIMIT: u64 = 4 * 1024 * 1024;

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(JSON_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > JSON_LIMIT {
        return Err("receive-probe result exceeds 4 MiB".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > JSON_LIMIT {
        return Err("receive-probe result exceeds 4 MiB".into());
    }
    // Each operation has its own result path. Existing evidence is never replaced.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(&bytes)
        .and_then(|()| file.write_all(b"\n"))
        .map_err(|e| e.to_string())
}

pub fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buf = [0_u8; 65536];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
