mod common;

#[test]
fn scale_body_matches_the_reference() {
    common::scale_steps::replay(false, false);
}

#[test]
#[ignore = "Nightly: exhaustive host failures and interruptions"]
fn all_scale_body_faults_match_the_reference() {
    common::scale_steps::replay(false, true);
}

// Design §7.5: the reference's scripted failure belongs to its host.
// Pin the interleaving that exposed a race in the fake clock on Linux.
#[test]
fn a_client_interrupt_can_precede_the_prototypes_parallel_alive_check() {
    common::scale_steps::replay(true, false);
}
