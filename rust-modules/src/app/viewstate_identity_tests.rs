//! **Which registry changes a queued household write survives.** A press waits in ViewState's queue
//! while the registry moves under it. The session owner decides, with `RosterCommit::Switch` versus
//! `Refresh { same_identity }`, whether the identity asking the servers changed, and its plan drives
//! the revocation edge in the registry that moves `Client::grant_epoch` together with the token swap.
//! ViewState compares that epoch (`plex::HeldClient::same_asker`) and not the profile counter, which
//! is published by a LATER commit.
//!
//! The registry changes here are the real owner decisions (`auth::owner::scenarios`) executed by
//! `execute_session_registry`, as `session_roster_art_tests` does for poster art. The press and the
//! pump go through the Bridge's own inputs (`viewstate_run`, `viewstate_pump`: the registry's keys and
//! `session::current_gen`), and the grading is on the `ViewWrite` job the step emits into the test
//! runner, which parks it after the same full-key check the production launch makes.

use super::*;
use plx_data::stores::viewstate::ViewStateCmd;
use plx_data::stores::StoreJob;
use plx_data::viewstate::{Scope, Write};
use plx_plex::plex::{ClientKey, ServerId};
use plx_session::auth::execute_session_registry;
use plx_session::auth::owner::scenarios::{
    activation_under_another_accounts_token, admin_boot_refresh_of_the_seated_profile,
    late_roster_of_the_seated_profile, member_account_on_admin_seat, refresh_under_another_accounts_token,
    switch_away_from_the_seated_profile,
};
use plx_session::auth::owner::CommitPlan;

/// The grant plex.tv hands out on the boot's refresh: not the stored token string.
const ROTATED_GRANT: &str = "plex-tv-grant-for-the-same-owner";

/// Press `Watched` on item `alpha` of `sid` through the Bridge, queued behind a write in flight, and
/// return the closure that lands that held write. The registry is changed between the two.
fn press_behind_a_held_write(bridge: &mut Bridge, sid: ServerId) -> Box<dyn FnOnce()> {
    bridge.stores.viewstate.borrow_mut().hold_inflight_for_test(sid, "held");
    assert!(bridge.viewstate_run(ViewStateCmd::Request {
        sid, rk: "alpha".into(), write: Write::Watched, scope: Scope::Leaf, guid: String::new(),
    }).changed);
    assert_eq!(bridge.stores.view_ledger.since(0).len(), 1, "the press is in the ledger until its fate is known");
    bridge.stores.viewstate.borrow().late_completion_for_test()
}

/// Land the held write, run the Bridge's ViewState pump, and return the writes the step emitted
/// (the test runner parks a job only while the registry still holds the client it names).
fn land_and_pump(bridge: &mut Bridge, land_held: Box<dyn FnOnce()>) -> Vec<(ServerId, ClientKey)> {
    land_held();
    let _ = bridge.viewstate_pump();
    bridge.take_held_store_jobs_for_test().into_iter().filter_map(|job| match job {
        StoreJob::ViewWrite(job, _) => Some(job.req.server()),
        _ => None,
    }).collect()
}

fn execute(plan: &CommitPlan) {
    for registry in &plan.registry {
        assert!(execute_session_registry(registry, "synthetic-client"));
    }
}

fn a_fresh_registry() {
    plx_plex::plex::reset_servers_for_test();
    plx_plex::plex::grant::reset_for_test();
}

fn nothing_went_out(bridge: &Bridge, jobs: &[(ServerId, ClientKey)]) {
    assert!(jobs.is_empty(), "no write was emitted under the new credential: {jobs:?}");
    assert!(bridge.stores.view_ledger.since(0).is_empty(), "the dropped press was retired");
    assert!(!bridge.stores.viewstate.borrow().is_busy_for_test(), "and nothing waits");
}

/// **A boot refresh of the seated profile keeps the press.** The stored server is registered, the
/// press is made, then discovery reaches the same server and user under plex.tv's current grant and
/// the owner commits a same-identity refresh: the token string changes in place and no profile
/// moved. The write goes out ONCE, under the key the registry holds now.
#[test]
fn a_boot_roster_refresh_of_the_seated_profile_keeps_a_queued_write() {
    let _g = plx_base::testlock::serial();
    a_fresh_registry();
    let (sid, plan) = admin_boot_refresh_of_the_seated_profile(ROTATED_GRANT);
    let mut bridge = Bridge::for_test(|| 0);
    let land_held = press_behind_a_held_write(&mut bridge, sid);

    execute(&plan);
    let jobs = land_and_pump(&mut bridge, land_held);

    let now = ClientKey::of(plx_plex::plex::client_for(sid).unwrap());
    assert_eq!(jobs, [(sid, now)], "one write, under the refreshed credential");
    a_fresh_registry();
}

/// The same for the switch's own late `ProfileRoster` re-committing the already-seated profile.
#[test]
fn a_late_profile_roster_of_the_seated_profile_keeps_a_queued_write() {
    let _g = plx_base::testlock::serial();
    a_fresh_registry();
    let (sid, plan) = late_roster_of_the_seated_profile();
    let mut bridge = Bridge::for_test(|| 0);
    let land_held = press_behind_a_held_write(&mut bridge, sid);

    execute(&plan);
    let jobs = land_and_pump(&mut bridge, land_held);

    let now = ClientKey::of(plx_plex::plex::client_for(sid).unwrap());
    assert_eq!(jobs, [(sid, now)], "one write, under the refreshed credential");
    a_fresh_registry();
}

/// **A switch is not a refresh, and the owner's Switch commit is where the registry says so.** The
/// swap has run (every token blanked, the picked profile's installed in place); the profile is
/// published by a later commit, so `current_gen` still reads the press's. The press is dropped.
#[test]
fn a_profile_switch_commit_drops_a_queued_write_before_the_profile_is_published() {
    let _g = plx_base::testlock::serial();
    a_fresh_registry();
    let (sid, plan) = switch_away_from_the_seated_profile();
    let mut bridge = Bridge::for_test(|| 0);
    let land_held = press_behind_a_held_write(&mut bridge, sid);
    let published = plx_plex::plex::session::current_gen();

    execute(&plan);
    assert_eq!(plx_plex::plex::session::current_gen(), published,
        "premise: the swap does not publish the profile, a later commit does");
    let jobs = land_and_pump(&mut bridge, land_held);

    nothing_went_out(&bridge, &jobs);
    a_fresh_registry();
}

/// A roster listed under ANOTHER account's token (the terminal reconcile of a member account on the
/// admin's seat) is not the seated identity's refresh: the owner revokes before the foreign retoken.
#[test]
fn a_refresh_under_another_accounts_token_drops_a_queued_write() {
    let _g = plx_base::testlock::serial();
    a_fresh_registry();
    let (sid, plan) = refresh_under_another_accounts_token();
    let mut bridge = Bridge::for_test(|| 0);
    let land_held = press_behind_a_held_write(&mut bridge, sid);

    execute(&plan);
    let jobs = land_and_pump(&mut bridge, land_held);

    nothing_went_out(&bridge, &jobs);
    a_fresh_registry();
}

#[test]
fn an_activation_under_another_accounts_token_drops_a_queued_write() {
    let _g = plx_base::testlock::serial();
    a_fresh_registry();
    let (sid, plan) = activation_under_another_accounts_token();
    let mut bridge = Bridge::for_test(|| 0);
    let land_held = press_behind_a_held_write(&mut bridge, sid);

    execute(&plan);
    let jobs = land_and_pump(&mut bridge, land_held);

    nothing_went_out(&bridge, &jobs);
    a_fresh_registry();
}

/// Sign-out and a sign-in as the same owner: the old slot is retired and the machine registers
/// again under a NEW slot, so the press finds no client (and `publish_profile(None)` moved the
/// profile besides). This pin is the registry's rule 2, which the grant comparison does not reach.
#[test]
fn a_sign_out_and_in_as_the_same_owner_drops_a_queued_write() {
    let _g = plx_base::testlock::serial();
    a_fresh_registry();
    let (_owner, sid, _members_view) = member_account_on_admin_seat();
    let mut bridge = Bridge::for_test(|| 0);
    let land_held = press_behind_a_held_write(&mut bridge, sid);

    plx_plex::plex::revoke_all();
    let again = plx_plex::plex::register_for_test("profile-machine", "127.0.0.1", 9, "the-same-owners-token", "cid");
    assert_ne!(again, sid, "premise: the signed-in owner holds a new slot");
    let jobs = land_and_pump(&mut bridge, land_held);

    nothing_went_out(&bridge, &jobs);
    a_fresh_registry();
}

/// A withdrawn plaintext grant blanks the live slot in place (live to blank) and the re-mint admits
/// a token again (blank to live): two revocation edges for one slot, and the identity is not the one
/// the press saw.
#[test]
fn a_regrade_of_the_plaintext_credential_drops_a_queued_write() {
    use plx_plex::plex::{grant, CredentialPolicy, Origin};
    let _g = plx_base::testlock::serial();
    a_fresh_registry();
    let origin = Origin::http("192.168.0.10", 32400);
    let evidence = grant::eligible_evidence_for_test();
    let install = || plx_plex::plex::register_pinned_with_client_id_and_policy(
        "lan-http", &origin, "granted-token", None, "regrade-test-client",
        plx_plex::plex::ConnectionFacts::default(), CredentialPolicy::HttpsOnly);
    grant::mint(grant::scope(), "lan-http", &origin, &evidence).unwrap();
    let sid = install();
    let client = plx_plex::plex::client_for(sid).unwrap();
    assert!(client.image_transcode_path("/thumb", 2, 2, false).ends_with("X-Plex-Token=granted-token"),
        "premise: the grant makes the slot live");
    let mut bridge = Bridge::for_test(|| 0);
    let land_held = press_behind_a_held_write(&mut bridge, sid);

    grant::network_changed(); // the foreground return withdraws the grant: the slot is blanked in place
    assert!(client.image_transcode_path("/thumb", 2, 2, false).ends_with("X-Plex-Token="),
        "premise: the withdrawn grant blanked the token");
    grant::mint(grant::scope(), "lan-http", &origin, &evidence).unwrap();
    assert_eq!(install(), sid, "premise: re-minted into the same slot");
    let jobs = land_and_pump(&mut bridge, land_held);

    nothing_went_out(&bridge, &jobs);
    a_fresh_registry();
}
