//! The pre-registered Server-scale rules. Values come from
//! `scripts/bench/scale_report.py`, as fixed by `server-scale-plan.md`
//! and the amendments in `data-phase-prototype-criteria.md`.

// The prototype criteria's P1 and P2 gates, also applied to the real engines.
pub const P2A_OVER: f64 = 1.0;
pub const P2B_NOT_SENT: f64 = 1.0;
pub const P2C_SHORT: f64 = 2.0;
pub const LOSS: i64 = 0;
pub const P1_P99: i64 = 100;
pub const P1_P999: i64 = 500;
pub const P1_WORST: i64 = 200;
pub const P5_W5_CPUS: i64 = 6;

// Server-scale plan: validity, sufficient valid runs, and the close-call band.
pub const CLIENT_BUSY: f64 = 0.75;
pub const CLIENT_BUSIEST: f64 = 0.90;
pub const CLIENT_NOT_SENT: f64 = P2B_NOT_SENT;
pub const OTHER_CPUS: f64 = 0.25;
pub const VALID_SHARE: f64 = 0.5;
pub const MIN_VALID: usize = 3;
pub const CLOSE_CALL: (f64, f64) = (0.25, 0.75);
pub const DESIGN_CANDIDATE: f64 = 1.5;
pub const CLIENT_ROLES: [&str; 2] = ["client1", "client2"];
pub const CURVE_ENGINES: [&str; 3] = ["udpstp", "prototype-b", "obudpst"];

// The rehearsal's first-Sub-Interval credit, and the home-lab path rules.
pub const START_SHORT_PCT: f64 = 10.0;
pub const BIN_S: f64 = 0.05;
pub const PATH_LOST: &str = "the path lost";
pub const PATH_LEFT_OUT: &str = "the path left out";
pub const PATH_LEFT_OUT_SHARE: f64 = 0.05;
pub const HOST_DROPS: [&str; 7] = [
    "RcvbufErrors",
    "InErrors",
    "softnet_dropped",
    "rx_drops",
    "rx_missed",
    "tx_drops",
    "qdisc_dropped",
];
pub const THROTTLES: [&str; 2] = ["core_throttle", "package_throttle"];
pub const PLATFORM_COUNTERS: [&str; 5] = [
    "bw_in_allowance_exceeded",
    "bw_out_allowance_exceeded",
    "pps_allowance_exceeded",
    "conntrack_allowance_exceeded",
    "linklocal_allowance_exceeded",
];
pub const SERVER_SEND_DROPS: [&str; 2] = ["tx_drops", "qdisc_dropped"];
pub const DEFAULT_NIC_DROPS: &str = "rx_drops";
pub const SILENT_REJECTIONS: [&str; 2] = [
    "SetupRequestSilentlyRejected",
    "ActivationRequestSilentlyRejected",
];

// The home-lab check once more: the executor and precision it actually ran.
pub const DATA_PHASE_ENDED: &str = "DataPhaseEnded";
pub const DATA_PHASE_ENGINES: &str = "DataPhaseEngines";
pub const POOL_FAST_PATH: &str = "epoll";
pub const SEND_LAG_P99: &str = "send_lag_p99_us";
pub const BURST_LATENESS_P99: &str = "burst_lateness_p99_us";
pub const THREADS_ENV: &str = "UDPSTP_ENGINE_THREADS";

pub fn executor_name(name: &str) -> &str {
    match name {
        "pool" => "the pooled executor",
        "per-test-connection" => "the per-Test-Connection executor",
        "both" => "both executors",
        other => other,
    }
}

pub fn engine_name(name: &str) -> &str {
    match name {
        "prototype-b" => "Prototype B (per logical CPU, unpinned)",
        "obudpst" => "OB-UDPST (floor)",
        "prototype-a" => "Prototype A (control)",
        "prototype-b-core" => "Prototype B (ADR 0008: per core, pinned)",
        other => other,
    }
}
