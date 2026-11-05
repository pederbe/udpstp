//! The Server's Admission under any sequence of Setups, sockets opening and
//! closing, activations, load bytes and time, driven directly: no
//! request has to be signed to reach it, as the Server core target's do.
//!
//! Properties (`udpstp_core::fuzzing::admission`), checked against the
//! driver's own account of each Test: no panics; after every step each
//! table is within its limit; the rate held in each direction, by every
//! admitted Test Connection of the activated Tests, within the Admitted
//! Rate Cap; each Source's activated Tests within the Tests per Source; and
//! each Test's Test Connections over its life, the closed ones included,
//! within the per-Test cap. Once every socket has closed, nothing is
//! left admitted.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| udpstp_core::fuzzing::admission(data));
