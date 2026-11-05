mod common;

#[test]
fn counter_snapshots_follow_the_private_test_peers() {
    common::first_steps::counter_snapshots_follow_the_private_test_peers();
}

#[test]
fn the_first_rounds_steps_are_the_pythons() {
    common::first_steps::the_first_rounds_steps_are_the_pythons();
}

#[test]
fn the_run_loop_is_the_pythons() {
    common::first_steps::the_run_loop_is_the_pythons();
}

#[test]
fn capped_servers_are_isolated_recorded_and_restored() {
    common::first_steps::capped_servers_are_isolated_recorded_and_restored();
}

#[test]
fn capped_server_cleanup_survives_host_failures_and_interruptions() {
    common::first_steps::capped_server_cleanup_survives_host_failures_and_interruptions();
}
