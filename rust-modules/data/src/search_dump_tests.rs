//! Dump mode at the Search site: the real pump, a dump-armed local gate and a worker that posts
//! late. See `plx_machine::landgate`'s module doc for which sites dump mode covers.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

const WORKER_DELAY: std::time::Duration = std::time::Duration::from_millis(25);

fn dump_gate() -> plx_machine::landgate::Gate {
    let gate = plx_machine::landgate::Gate::default();
    gate.arm_dump(std::time::Duration::from_secs(20));
    gate
}

/// A request that is out when the pump runs lands on THAT pump however late the worker posts
/// (25 ms here, against a pump that reaches its take in microseconds). A plain take (the site's
/// conversion reverted) returns empty and the first assertion fails.
#[test]
fn dump_mode_a_request_out_lands_on_the_pump_that_runs_whatever_the_worker() {
    let _g = fresh();
    let mut owner = Owner::default();
    register(&mut owner, 1);
    owner.set_query("wallace");
    let gen = owner.state.gen;
    owner.adapter.mailbox(0).claim(gen);
    hold_off(&mut owner);
    let gate = dump_gate();
    let worker = Arc::clone(&owner.adapter);
    let join = std::thread::spawn(move || {
        std::thread::sleep(WORKER_DELAY);
        land(&worker, 0, gen, Some(answered(0, vec![media("A Close Shave")]).items));
    });
    assert!(pump_with_optional_directory(&mut owner.state, &owner.adapter, 0.0, None, &gate),
        "the answer the pump owed must be taken by the pump that asked");
    assert_eq!(titles(&owner.state.shelves()[0]), ["A Close Shave"]);
    assert_eq!(owner.state.state(), State::Ready);
    assert!(!owner.adapter.mailbox(0).busy(), "the TAKE released the claim");
    join.join().unwrap();
}

/// A request that was superseded while its worker was out can deliver its answer FIRST. That stale
/// answer must neither end the dump wait nor release the NEW request's claim: B's answer lands on
/// the pump that was owed it. (With `Fetch::take` in the site, the stale answer released B's claim
/// and B landed on a later pump, by worker timing.)
#[test]
fn dump_mode_a_superseded_requests_stale_answer_does_not_release_the_new_claim() {
    let _g = fresh();
    let mut owner = Owner::default();
    register(&mut owner, 1);
    owner.set_query("wal");
    let stale = owner.state.gen;
    owner.adapter.mailbox(0).claim(stale); // request A is out
    owner.set_query("wallace"); // supersedes A: its worker is still running
    let current = owner.state.gen;
    owner.adapter.mailbox(0).claim(current); // request B is out
    hold_off(&mut owner);
    let gate = dump_gate();
    let worker = Arc::clone(&owner.adapter);
    let join = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(5));
        land(&worker, 0, stale, Some(answered(0, vec![media("Wallander")]).items));
        std::thread::sleep(WORKER_DELAY);
        land(&worker, 0, current, Some(answered(0, vec![media("A Close Shave")]).items));
    });
    assert!(pump_with_optional_directory(&mut owner.state, &owner.adapter, 0.0, None, &gate),
        "B was out when the pump ran, so B's answer must land on this pump");
    assert_eq!(titles(&owner.state.shelves()[0]), ["A Close Shave"]);
    join.join().unwrap();
}
