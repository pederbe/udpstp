//! Checksum use on receipt (RFC 9946 §5.6), the one rule for every PDU:
//! "The PDU receiver SHALL subsequently verify the PDU checksum whenever
//! checksum processing has been configured and the field is populated."
//! udpstp-wire says where a PDU's checkSum field is and whether it matches;
//! this says whether it must.

/// Whether a received PDU whose checkSum field holds `field` passes. Where
/// checksum use is `configured`, a populated field must match its PDU, as
/// `matches` says, and an unpopulated (zero) one passes: its sender doesn't
/// fill it. Where it isn't, the field is ignored, whatever it holds, where
/// OB-UDPST verifies every populated one
/// (conformance: checksum-verified-only-when-configured).
pub(crate) fn passes(configured: bool, field: u16, matches: impl FnOnce() -> bool) -> bool {
    !configured || field == 0 || matches()
}
