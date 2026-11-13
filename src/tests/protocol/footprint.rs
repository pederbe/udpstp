//! The Server's footprint: how full each of its tables is. The
//! fuzz target `server_core` (fuzz/) checks the limits under hostile
//! input; these tests check what the footprint reports.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use crate::protocol::{
    DatagramMeta, Endpoint, IpVersion, MonoTime, Now, Output, Server, ServerConfig, SocketId,
    WallTime,
};
use crate::wire::setup::{SetupPdu, cmd_request, modifier};
use crate::wire::{PROTOCOL_VERSION, auth_mode};

const CLIENT: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 40000);
const LOCAL: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1)), 24601);

fn now() -> Now {
    Now {
        mono: MonoTime::from_duration(Duration::from_secs(1)),
        wall: WallTime::from_unix(Duration::from_secs(1_790_380_800)),
    }
}

fn unauthenticated_setup_request() -> Vec<u8> {
    SetupPdu {
        protocol_version: PROTOCOL_VERSION,
        mc_index: 0,
        mc_count: 1,
        mc_ident: 7,
        cmd_request: cmd_request::SETUP_REQUEST,
        cmd_response: 0,
        max_bandwidth: 0,
        test_port: 0,
        modifier_bitmap: modifier::JUMBO_STATUS,
        auth_mode: auth_mode::UNAUTHENTICATED,
        auth_unix_time: 0,
        auth_digest: [0; 32],
        key_id: 0,
        checksum: 0,
    }
    .encode()
    .to_vec()
}

/// The socket the Server asked for to answer a Setup Request.
fn requested_socket(server: &mut Server) -> SocketId {
    let outputs: Vec<Output> = std::iter::from_fn(|| server.poll_output()).collect();
    outputs
        .iter()
        .find_map(|output| match output {
            Output::OpenSocket { socket, .. } => Some(*socket),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no socket for the Test Connection: {outputs:?}"))
}

#[test]
fn a_new_server_holds_nothing_and_reports_its_limits() {
    let footprint = Server::new(ServerConfig {
        max_test_connections: 3,
        ..ServerConfig::default()
    })
    .footprint();
    for (name, table) in footprint.tables() {
        assert_eq!(table.entries, 0, "{name}");
        assert!(table.limit > 0, "{name}");
    }
    // A cap of 3 plus the share, whose default floor is the per-Test
    // cap of 24 (conformance: unactivated-test-connection-share).
    assert_eq!(footprint.test_connections.limit, 27);
    assert_eq!(footprint.tests.limit, 27);
    assert_eq!(footprint.data_phases.limit, 3);
}

#[test]
fn an_admitted_test_connection_is_counted_until_its_socket_closes() {
    let mut server = Server::new(ServerConfig {
        allow_unauthenticated: true,
        ..ServerConfig::default()
    });
    let payload = unauthenticated_setup_request();
    let meta = DatagramMeta {
        socket: SocketId::PRIMARY,
        source: CLIENT,
        local: LOCAL,
        ip_version: IpVersion::V4,
        tos: None,
        actual_len: payload.len(),
    };
    server.handle_datagram(now(), meta, &payload);
    let socket = requested_socket(&mut server);
    assert_eq!(
        server.footprint().test_connections.entries,
        1,
        "waiting for its socket"
    );

    server.handle_socket_opened(now(), socket, SocketAddr::new(LOCAL.ip(), 50000));
    while server.poll_output().is_some() {}
    let set_up = server.footprint();
    assert_eq!(set_up.test_connections.entries, 1, "set up");
    assert_eq!(set_up.tests.entries, 1);

    server.handle_socket_closed(now(), socket);
    let closed = server.footprint();
    assert_eq!(closed.test_connections.entries, 0);
    assert_eq!(closed.tests.entries, 0);
}
