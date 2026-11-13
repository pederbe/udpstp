//! Authenticated Control PDUs captured from OB-UDPST v9.1.0, in
//! `vectors/obudpst-control.txt` (`cargo xtask capture`; provenance is in
//! the file's header). They pin mode 1's HMAC coverage and the Derived
//! Keys to an independent implementation: our wire layer must verify each
//! digest with the same Shared Key, and reproduce every PDU, digest and
//! checksum included, from its decoded fields.
//!
//! OB-UDPST-specific content in the captures (the fields of RFC 9946 that
//! OB-UDPST fills its own way; none of it is outside the RFC's fields):
//! - protocolVer is 20, OB-UDPST's version for RFC 9946.
//! - mcIdent is OB-UDPST's random non-zero identifier, mcCount 1.
//! - modifierBitmap is CHSR_JUMBO_STATUS, OB-UDPST's default.
//! - The Activation Request's parameters are OB-UDPST's defaults, with
//!   srIndexConf 10 and CHTA_SRIDX_ISSTART clear (`-I 10`, a Fixed-Rate
//!   Test, which OB-UDPST allows anyone) and useOwDelVar False.
//! - OB-UDPST's reserved-field extensions are all zero in these PDUs.

use super::common;

use crate::wire::auth::derive_keys;
use crate::wire::{ActivationPdu, SetupPdu, checksum};
use common::unhex;

const VECTORS: &str = include_str!("vectors/obudpst-control.txt");

/// The Shared Key the captures used, as OB-UDPST reads it from a key file.
const SHARED_KEY: &[u8] = b"udpstp-kat-key";
const KEY_ID: u8 = 5;

/// One captured run: its name and its PDUs, (kind, bytes), in order.
struct Run {
    name: String,
    pdus: Vec<(String, Vec<u8>)>,
}

fn runs() -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for line in VECTORS.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let (kind, rest) = line.split_once(' ').expect("a kind and a value");
        if kind == "run" {
            runs.push(Run {
                name: rest.to_string(),
                pdus: Vec::new(),
            });
        } else {
            let run = runs.last_mut().expect("a run line first");
            run.pdus.push((kind.to_string(), unhex(rest)));
        }
    }
    runs
}

fn pdu<'a>(run: &'a Run, kind: &str) -> &'a [u8] {
    &run.pdus
        .iter()
        .find(|(k, _)| k == kind)
        .unwrap_or_else(|| panic!("{}: no {kind}", run.name))
        .1
}

/// The run's Derived Keys: from the Shared Key and the Setup Request's
/// authUnixTime (RFC 9946 §5.4).
fn keys(run: &Run) -> crate::wire::auth::DerivedKeys {
    let setup = SetupPdu::decode(pdu(run, "setup-request")).expect("a Setup Request");
    derive_keys(SHARED_KEY, setup.auth_unix_time)
}

#[test]
fn the_captures_hold_every_control_pdu_of_each_run() {
    let runs = runs();
    assert_eq!(runs.len(), 3);
    for run in &runs {
        let kinds: Vec<&str> = run.pdus.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "setup-request",
                "setup-response",
                "activation-request",
                "activation-response"
            ],
            "{}",
            run.name
        );
    }
}

// Mode 1: the authDigest is the HMAC-SHA-256 of the whole PDU with
// authDigest and checkSum zeroed; the Client Authentication Key signs
// requests, the Server Authentication Key responses.
#[udpstp_req::req("RFC9946-5.3.1-L3", "RFC9946-5.4.1-6")]
#[test]
fn our_wire_layer_verifies_every_obudpst_digest() {
    for run in runs() {
        let keys = keys(&run);
        for (kind, bytes) in &run.pdus {
            let key = if kind.ends_with("request") {
                keys.client()
            } else {
                keys.server()
            };
            let verified = match kind.as_str() {
                "setup-request" | "setup-response" => SetupPdu::verify_received(bytes, key),
                _ => ActivationPdu::verify_received(bytes, key),
            };
            assert!(verified, "{}: the {kind} verifies", run.name);
            let other = if kind.ends_with("request") {
                keys.server()
            } else {
                keys.client()
            };
            let crossed = match kind.as_str() {
                "setup-request" | "setup-response" => SetupPdu::verify_received(bytes, other),
                _ => ActivationPdu::verify_received(bytes, other),
            };
            assert!(
                !crossed,
                "{}: the {kind} fails with the other key",
                run.name
            );
        }
    }
}

// HMAC coverage; the checksum over the PDU, computed after the digest; the
// Activation PDU layout.
#[udpstp_req::req("RFC9946-6.1-5", "RFC9946-5.3.1-3", "RFC9946-5.6-3", "RFC9946-7.1-7")]
#[test]
fn our_wire_layer_reproduces_every_obudpst_pdu_from_its_fields() {
    for run in runs() {
        let keys = keys(&run);
        for (kind, bytes) in &run.pdus {
            let key = if kind.ends_with("request") {
                keys.client()
            } else {
                keys.server()
            };
            let rebuilt = match kind.as_str() {
                "setup-request" | "setup-response" => {
                    let mut pdu = SetupPdu::decode(bytes).expect("decodes");
                    assert_eq!(pdu.auth_mode, 1, "{}: mode 1", run.name);
                    assert_eq!(pdu.key_id, KEY_ID, "{}", run.name);
                    let checksummed = pdu.checksum != 0;
                    pdu.auth_digest = [0; 32];
                    pdu.checksum = 0;
                    pdu.sign(key);
                    let mut rebuilt = pdu.encode().to_vec();
                    if checksummed {
                        checksum::fill_control(&mut rebuilt);
                    }
                    rebuilt
                }
                _ => {
                    let mut pdu = ActivationPdu::decode(bytes).expect("decodes");
                    let checksummed = pdu.checksum != 0;
                    pdu.auth_digest = [0; 32];
                    pdu.checksum = 0;
                    pdu.sign(key);
                    let mut rebuilt = pdu.encode().to_vec();
                    if checksummed {
                        checksum::fill_control(&mut rebuilt);
                    }
                    rebuilt
                }
            };
            assert_eq!(&rebuilt, bytes, "{}: the {kind}", run.name);
        }
    }
}

#[udpstp_req::req("RFC9946-5.6-4")]
#[test]
fn the_checksum_build_s_pdus_carry_checksums_that_verify() {
    for run in runs() {
        let checksummed = run.name.starts_with("downstream-checksum");
        for (kind, bytes) in &run.pdus {
            let field = u16::from_be_bytes([bytes[bytes.len() - 2], bytes[bytes.len() - 1]]);
            assert_eq!(
                field != 0,
                checksummed,
                "{}: the {kind}'s checkSum",
                run.name
            );
            assert!(
                !checksummed || checksum::control_valid(bytes),
                "{}: the {kind}",
                run.name
            );
        }
    }
}

#[test]
fn the_upstream_setup_request_states_its_max_bandwidth_with_usdir() {
    let runs = runs();
    let run = runs
        .iter()
        .find(|r| r.name.starts_with("upstream"))
        .expect("an upstream run");
    let setup = SetupPdu::decode(pdu(run, "setup-request")).expect("decodes");
    assert_eq!(setup.max_bandwidth, 0x8000 | 20);
    let response = ActivationPdu::decode(pdu(run, "activation-response")).expect("decodes");
    let rate = response.sending_rate;
    assert!(
        rate.tx_interval1 != 0 || rate.tx_interval2 != 0,
        "an Upstream Activation Response carries the Sending Rate Structure: {rate:?}"
    );
}

#[test]
fn the_activation_requests_carry_obudpst_s_defaults_and_row_10() {
    for run in runs() {
        let request = ActivationPdu::decode(pdu(&run, "activation-request")).expect("decodes");
        assert_eq!(request.protocol_version, 20, "{}", run.name);
        assert_eq!(request.sr_index_conf, 10, "{}", run.name);
        assert_eq!(
            request.modifier_bitmap & crate::wire::activation::modifier::SR_INDEX_IS_START,
            0,
            "{}: a Fixed-Rate Test",
            run.name
        );
        assert_eq!(request.use_ow_del_var, 0, "{}", run.name);
    }
}
