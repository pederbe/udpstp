#[test]
fn short_datagram_reports_one_event_without_reply() {
    use crate::protocol::{
        DatagramMeta, Endpoint, IpVersion, MonoTime, Now, Output, Server, ServerConfig, SocketId,
        WallTime,
    };
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::Duration;

    let mut server = Server::new(ServerConfig::default());
    let now = Now {
        mono: MonoTime::from_duration(Duration::from_secs(1)),
        wall: WallTime::from_unix(Duration::from_secs(1_790_000_000)),
    };
    // A datagram arrived on the Standard Port: too short for a Setup Request.
    let source = SocketAddr::from((Ipv4Addr::new(192, 0, 2, 1), 40000));
    let local = SocketAddr::from((Ipv4Addr::new(198, 51, 100, 1), 24601));
    let payload = [0u8; 8];
    let meta = DatagramMeta {
        socket: SocketId::PRIMARY,
        source,
        local,
        ip_version: IpVersion::V4,
        tos: None,
        actual_len: payload.len(),
    };
    server.handle_datagram(now, meta, &payload);
    // Act on everything it asks for: here it only reports the datagram.
    let mut events = Vec::new();
    while let Some(output) = server.poll_output() {
        match output {
            Output::Transmit(transmit) => unreachable!("no reply: {transmit:?}"),
            Output::Event(event) => events.push(event),
            _ => { /* open, connect or close sockets; start Data Phases */ }
        }
    }
    assert_eq!(events.len(), 1, "{events:?}");
    // Then sleep until the next deadline, or the next datagram.
    let _next = server.poll_timeout();
}
