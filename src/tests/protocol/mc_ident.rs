//! The mcIdent a Client draws from a UDP socket it holds for the Test
//! (conformance: mc-ident-from-a-held-port).

use std::collections::HashSet;
use std::net::SocketAddr;

use crate::protocol::mc_ident_for;

// : Clients on one host hold different ports at once, so each port
// must give its own mcIdent, and none zero (RFC 9946 §6.1)
#[udpstp_req::req("RFC9946-6.1-L3")]
#[test]
fn every_port_of_one_address_gives_its_own_non_zero_mc_ident() {
    for address in ["192.0.2.7", "10.0.0.1", "2001:db8::7", "0.0.0.0"] {
        let ip = address.parse().unwrap();
        let idents: HashSet<u16> = (1..=u16::MAX)
            .map(|port| {
                mc_ident_for(SocketAddr::new(ip, port))
                    .expect("a port gives an mcIdent")
                    .get()
            })
            .collect();
        assert_eq!(idents.len(), usize::from(u16::MAX), "{address}");
    }
    assert_eq!(mc_ident_for("192.0.2.7:0".parse().unwrap()), None);
}

// : hosts behind one NAT hold ports from the same ephemeral range, so
// their addresses must spread their mcIdents over all 16 bits
#[test]
fn hosts_behind_a_nat_draw_from_the_whole_range() {
    let port = 50_000;
    let idents: HashSet<u16> = (1..=254)
        .map(|host| {
            let local = SocketAddr::from(([192, 168, 1, host], port));
            mc_ident_for(local).unwrap().get()
        })
        .collect();
    assert!(idents.len() >= 250, "{} of 254 distinct", idents.len());
    // Both halves of the range, not only the port's neighbourhood.
    assert!(idents.iter().any(|&i| i < 0x8000) && idents.iter().any(|&i| i >= 0x8000));
}
