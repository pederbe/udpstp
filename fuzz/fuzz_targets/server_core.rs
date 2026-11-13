//! The Server core under hostile input: random datagrams on any of
//! its sockets from a few addresses, signed Setup and Activation Requests
//! with random fields (so the fuzzer gets past authentication), sockets
//! opening and closing, Data Phases ending, and time steps of either clock.
//!
//! Properties: no panics; after every step each of the Server's tables is
//! within its limit; and once every socket has closed and the timers have
//! run, no Test Connection, Test or Data Phase is left holding Admission.
#![no_main]


#[path = "../../src/protocol/mod.rs"]
#[allow(dead_code, unused_imports)]
mod protocol;

#[path = "../../src/wire/mod.rs"]
#[allow(dead_code, unused_imports)]
mod wire;
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use crate::protocol::{
    DatagramMeta, Endpoint, IpVersion, KeyEntry, KeyTable, Lifetime, MonoTime, Now, Output, Server,
    ServerConfig, SocketId, WallTime,
};
use crate::wire::activation::{ActivationPdu, SendingRate};
use crate::wire::setup::{SetupPdu, cmd_request};
use crate::wire::{PROTOCOL_VERSION, auth_mode, checksum};

const KEY_ID: u8 = 5;
const KEY: &[u8] = b"udpstp fuzz key";
/// The wall clock's start: 2026-09-26.
const WALL_START: u64 = 1_790_380_800;

#[derive(Debug, Arbitrary)]
struct Input {
    allow_unauthenticated: bool,
    checksum: bool,
    troubleshooting: bool,
    max_test_connections: u8,
    test_connections_per_test: u8,
    tests_per_source: u8,
    steps: Vec<Step>,
}

#[derive(Debug, Arbitrary)]
enum Step {
    /// Any datagram, on the Standard Port or a Test Connection's socket.
    Datagram {
        socket: u8,
        from: u8,
        payload: Vec<u8>,
    },
    /// A Setup Request signed with the Key Table's key.
    Setup {
        from: u8,
        mc_ident: u16,
        mc_index: u8,
        mc_count: u8,
        max_bandwidth: u16,
        modifier_bitmap: u8,
        /// Seconds from the Server's wall clock.
        time_offset: i8,
        mode: u8,
    },
    /// An Activation Request to a Test Connection's socket, signed with
    /// the keys its Setup Request derived.
    Activation {
        socket: u8,
        from: u8,
        fields: ActivationFields,
    },
    /// A socket the Server asked for is bound.
    Open { socket: u8 },
    /// A socket closed, on request or because its Data Phase ended.
    Close { socket: u8 },
    /// The monotonic clock advances, and the Server's timers run.
    Advance { millis: u16 },
    /// The wall clock steps.
    WallClock { seconds: i16 },
}

#[derive(Debug, Arbitrary)]
struct ActivationFields {
    /// cmdRequest, reduced to NONE, UPSTREAM, DOWNSTREAM or one above.
    cmd_request: u8,
    low_thresh: u16,
    upper_thresh: u16,
    trial_int: u16,
    test_int_time: u16,
    dscp_ecn: u8,
    sr_index_conf: u16,
    use_ow_del_var: u8,
    high_speed_delta: u8,
    slow_adj_thresh: u16,
    seq_err_thresh: u16,
    ignore_ooo_dup: u8,
    modifier_bitmap: u8,
    rate_adj_algo: u8,
    sub_int_period: u16,
    time_offset: i8,
}

/// The Client addresses the steps pick from: few, so Sources collide.
fn client(from: u8) -> SocketAddr {
    let port = 40000 + u16::from(from % 4);
    match from % 5 {
        0 | 1 => SocketAddr::from((Ipv4Addr::new(192, 0, 2, 1), port)),
        2 => SocketAddr::from((Ipv4Addr::new(192, 0, 2, 2), port)),
        3 => SocketAddr::from((Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1), port)),
        _ => SocketAddr::from((Ipv6Addr::new(0x2001, 0xdb8, 0, 1, 0, 0, 0, 1), port)),
    }
}

fn local(ip: IpAddr, port: u16) -> SocketAddr {
    SocketAddr::new(ip, port)
}

fn server_ip(of: SocketAddr) -> IpAddr {
    match of {
        SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1)),
        SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 1, 0, 0, 0, 0, 1)),
    }
}

/// What the runtime knows of one of the Server's sockets.
struct Socket {
    local_ip: IpAddr,
    /// Its port, once bound.
    port: Option<u16>,
    /// The authUnixTime of the Setup Request it answers, for the keys.
    setup_time: Option<u32>,
}

struct Runtime {
    server: Server,
    mono: Duration,
    wall: Duration,
    sockets: BTreeMap<u32, Socket>,
    /// The authUnixTime of the latest signed Setup Request.
    last_setup_time: Option<u32>,
    next_port: u16,
    checksum: bool,
}

impl Runtime {
    fn now(&self) -> Now {
        Now {
            mono: MonoTime::from_duration(self.mono),
            wall: WallTime::from_unix(self.wall),
        }
    }

    /// Acts on the Server's outputs as a runtime would, then checks its
    /// tables.
    fn drain(&mut self) {
        while let Some(output) = self.server.poll_output() {
            match output {
                Output::OpenSocket {
                    socket, local_ip, ..
                } => {
                    self.sockets.insert(
                        socket.0,
                        Socket {
                            local_ip,
                            port: None,
                            setup_time: self.last_setup_time,
                        },
                    );
                }
                Output::CloseSocket(socket) => {
                    self.sockets.remove(&socket.0);
                    let now = self.now();
                    self.server.handle_socket_closed(now, socket);
                }
                Output::StartDataPhase { socket, .. } => {
                    // Taken at once, as a runtime hands it to its engine.
                    assert!(self.server.take_data_phase(socket).is_some());
                }
                // Datagrams, connections and events change nothing here.
                _ => {}
            }
        }
        let footprint = self.server.footprint();
        for (name, table) in footprint.tables() {
            assert!(
                table.entries <= table.limit,
                "{name}: {} entries, limit {}",
                table.entries,
                table.limit
            );
        }
    }

    /// The socket a step's index picks: the Standard Port or one of the
    /// Test Connections' bound sockets.
    fn pick(&self, index: u8) -> Option<(SocketId, SocketAddr, Option<u32>)> {
        let bound: Vec<_> = self
            .sockets
            .iter()
            .filter_map(|(&id, s)| {
                s.port
                    .map(|port| (id, local(s.local_ip, port), s.setup_time))
            })
            .collect();
        match usize::from(index) % (bound.len() + 1) {
            0 => None,
            i => {
                let (id, addr, time) = bound[i - 1];
                Some((SocketId(id), addr, time))
            }
        }
    }

    fn deliver(&mut self, socket: SocketId, local: SocketAddr, source: SocketAddr, payload: &[u8]) {
        let meta = DatagramMeta {
            socket,
            source,
            local,
            ip_version: if source.is_ipv4() {
                IpVersion::V4
            } else {
                IpVersion::V6
            },
            tos: None,
            actual_len: payload.len(),
        };
        let now = self.now();
        self.server.handle_datagram(now, meta, payload);
    }

    fn deliver_to_standard_port(&mut self, source: SocketAddr, payload: &[u8]) {
        let local = local(server_ip(source), crate::wire::STANDARD_PORT);
        self.deliver(SocketId::PRIMARY, local, source, payload);
    }

    fn wall_secs(&self) -> u32 {
        u32::try_from(self.wall.as_secs()).unwrap_or(u32::MAX)
    }

    fn step(&mut self, step: Step) {
        match step {
            Step::Datagram {
                socket,
                from,
                payload,
            } => match self.pick(socket) {
                None => self.deliver_to_standard_port(client(from), &payload),
                Some((id, local, _)) => self.deliver(id, local, client(from), &payload),
            },
            Step::Setup {
                from,
                mc_ident,
                mc_index,
                mc_count,
                max_bandwidth,
                modifier_bitmap,
                time_offset,
                mode,
            } => {
                let time = self
                    .wall_secs()
                    .saturating_add_signed(i32::from(time_offset));
                let mut pdu = SetupPdu {
                    protocol_version: PROTOCOL_VERSION,
                    mc_index,
                    mc_count,
                    mc_ident,
                    cmd_request: cmd_request::SETUP_REQUEST,
                    cmd_response: 0,
                    max_bandwidth,
                    test_port: 0,
                    modifier_bitmap,
                    auth_mode: mode % 4,
                    auth_unix_time: time,
                    auth_digest: [0; 32],
                    key_id: KEY_ID,
                    checksum: 0,
                };
                let keys = crate::wire::auth::derive_keys(KEY, time);
                if pdu.auth_mode != auth_mode::UNAUTHENTICATED {
                    pdu.sign(keys.client());
                }
                let mut bytes = pdu.encode();
                if self.checksum {
                    checksum::fill_control(&mut bytes);
                }
                self.last_setup_time = Some(time);
                self.deliver_to_standard_port(client(from), &bytes);
            }
            Step::Activation {
                socket,
                from,
                fields,
            } => {
                let Some((id, local, setup_time)) = self.pick(socket) else {
                    return;
                };
                let keys = crate::wire::auth::derive_keys(KEY, setup_time.unwrap_or(0));
                let mut pdu = ActivationPdu {
                    protocol_version: PROTOCOL_VERSION,
                    cmd_request: fields.cmd_request % 4,
                    cmd_response: 0,
                    low_thresh: fields.low_thresh,
                    upper_thresh: fields.upper_thresh,
                    trial_int: fields.trial_int,
                    test_int_time: fields.test_int_time,
                    dscp_ecn: fields.dscp_ecn,
                    sr_index_conf: fields.sr_index_conf,
                    use_ow_del_var: fields.use_ow_del_var,
                    high_speed_delta: fields.high_speed_delta,
                    slow_adj_thresh: fields.slow_adj_thresh,
                    seq_err_thresh: fields.seq_err_thresh,
                    ignore_ooo_dup: fields.ignore_ooo_dup,
                    modifier_bitmap: fields.modifier_bitmap,
                    rate_adj_algo: fields.rate_adj_algo,
                    sending_rate: SendingRate::default(),
                    sub_int_period: fields.sub_int_period,
                    auth_mode: auth_mode::MODE_1,
                    auth_unix_time: self
                        .wall_secs()
                        .saturating_add_signed(i32::from(fields.time_offset)),
                    auth_digest: [0; 32],
                    key_id: KEY_ID,
                    checksum: 0,
                };
                pdu.sign(keys.client());
                let mut bytes = pdu.encode();
                if self.checksum {
                    checksum::fill_control(&mut bytes);
                }
                self.deliver(id, local, client(from), &bytes);
            }
            Step::Open { socket } => {
                let waiting: Vec<u32> = self
                    .sockets
                    .iter()
                    .filter(|(_, s)| s.port.is_none())
                    .map(|(&id, _)| id)
                    .collect();
                if waiting.is_empty() {
                    return;
                }
                let id = waiting[usize::from(socket) % waiting.len()];
                let port = self.next_port;
                self.next_port = self.next_port.wrapping_add(1).max(49152);
                let s = self.sockets.get_mut(&id).expect("waiting");
                s.port = Some(port);
                let local = local(s.local_ip, port);
                let now = self.now();
                self.server.handle_socket_opened(now, SocketId(id), local);
            }
            Step::Close { socket } => {
                let ids: Vec<u32> = self.sockets.keys().copied().collect();
                if ids.is_empty() {
                    return;
                }
                let id = ids[usize::from(socket) % ids.len()];
                self.sockets.remove(&id);
                let now = self.now();
                self.server.handle_socket_closed(now, SocketId(id));
            }
            Step::Advance { millis } => {
                self.advance(Duration::from_millis(u64::from(millis)));
            }
            Step::WallClock { seconds } => {
                let secs = self
                    .wall
                    .as_secs()
                    .saturating_add_signed(i64::from(seconds));
                self.wall = Duration::from_secs(secs);
            }
        }
    }

    /// Advances both clocks, running every timer that falls due on the way.
    fn advance(&mut self, by: Duration) {
        let end = self.mono + by;
        let mut timeouts = 0u32;
        while let Some(due) = self.server.poll_timeout() {
            let due = due.as_duration().max(self.mono);
            if due > end {
                break;
            }
            // A deadline that handling doesn't move on would spin forever.
            timeouts += 1;
            assert!(timeouts < 1_000_000, "the Server's timers don't advance");
            self.wall += due - self.mono;
            self.mono = due;
            let now = self.now();
            self.server.handle_timeout(now);
            self.drain();
        }
        self.wall += end - self.mono;
        self.mono = end;
    }
}

fuzz_target!(|input: Input| {
    let mut keys = KeyTable::default();
    let row = KeyEntry::new(
        "fuzz",
        KEY_ID,
        KEY.to_vec(),
        Lifetime::new(None, None),
        Lifetime::new(None, None),
    );
    keys.insert(row).expect("one row");
    let config = ServerConfig {
        allow_unauthenticated: input.allow_unauthenticated,
        checksum: input.checksum,
        troubleshooting: input.troubleshooting,
        keys,
        max_test_connections: usize::from(input.max_test_connections % 8) + 1,
        test_connections_per_test: usize::from(input.test_connections_per_test % 4) + 1,
        tests_per_source: usize::from(input.tests_per_source % 3) + 1,
        ..ServerConfig::default()
    };
    let mut runtime = Runtime {
        server: Server::new(config),
        mono: Duration::from_secs(1),
        wall: Duration::from_secs(WALL_START),
        sockets: BTreeMap::new(),
        last_setup_time: None,
        next_port: 49152,
        checksum: input.checksum,
    };
    for step in input.steps.into_iter().take(256) {
        runtime.step(step);
        runtime.drain();
    }
    // Every socket closes, and the timers run out.
    let ids: Vec<u32> = runtime.sockets.keys().copied().collect();
    for id in ids {
        runtime.sockets.remove(&id);
        let now = runtime.now();
        runtime.server.handle_socket_closed(now, SocketId(id));
        runtime.drain();
    }
    runtime.advance(Duration::from_secs(3600));
    runtime.drain();
    let footprint = runtime.server.footprint();
    assert_eq!(footprint.test_connections.entries, 0, "{footprint:?}");
    assert_eq!(footprint.tests.entries, 0, "{footprint:?}");
    assert_eq!(footprint.data_phases.entries, 0, "{footprint:?}");
    assert!(
        runtime.sockets.is_empty(),
        "the Server opened sockets after all closed"
    );
});
