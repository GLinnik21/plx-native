//! Search publication freeze/notify timing across frames.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::{frame};

#[test]
fn search_publication_is_frozen_for_the_frame_then_notified_once_at_the_next_split() {
    let _guard = plx_base::testlock::serial();
    use plx_data::stores::search::SearchCmd;
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    // Search is owned by `rig` now, so there is no process-wide state to reset — the store's
    // whole lifetime is this test's own `rig` binding.
    rig.search_run(SearchCmd::SetQuery("before".into()));
    let _ = rig.stores.take_notices();
    frame(&mut d, &mut rig, AppArg::Search, tick(0), vec![]);
    let count = |d: &Dispatcher<AppHost>| notices(d).split("notices=").nth(1).unwrap().parse::<u32>().unwrap();
    let baseline = count(&d);
    let retained = rig.search.clone();
    d.emit(MachineId::Nav, Fx::App(AppFx::Store(StoreId::Search,
        StoreCmd::Search(SearchCmd::SetQuery("after".into())))));
    frame(&mut d, &mut rig, AppArg::Search, tick(1), vec![]);
    assert_eq!(rig.stores.search_snapshot(rig.directory.view(), &Bridge::household_ids()).view().query(), "after",
        "real store delivery ran");
    assert_eq!(rig.search.view().query(), "before", "the frame split precedes the store drain");
    assert!(rig.search.same_publication(&retained));
    frame(&mut d, &mut rig, AppArg::Search, tick(2), vec![]);
    assert_eq!(rig.search.view().query(), "after");
    assert_eq!(retained.view().query(), "before");
    assert_eq!(count(&d), baseline + 1, "capture and pending store notice must coalesce");
    let settled = rig.search.clone();
    for i in 3..7 {
        frame(&mut d, &mut rig, AppArg::Search, tick(i), vec![]);
        assert!(rig.search.same_publication(&settled));
        assert_eq!(count(&d), baseline + 1, "unchanged publications must stay quiet");
    }
    rig.search_run(SearchCmd::SetQuery("after ".into()));
    frame(&mut d, &mut rig, AppArg::Search, tick(7), vec![]);
    assert_eq!(rig.search.view().query(), "after ");
    assert_eq!(rig.search.view().query_gen(), settled.view().query_gen());
    assert_eq!(count(&d), baseline + 2, "raw text changes still update the field");
    rig.search_run(SearchCmd::SetQuery("unqueued".into()));
    let _ = rig.stores.take_notices();
    frame(&mut d, &mut rig, AppArg::Search, tick(8), vec![]);
    assert_eq!(rig.search.view().query(), "unqueued");
    assert_eq!(count(&d), baseline + 3, "a changed publication with no queued notice still reconciles");
}

/// **The Bridge hands Search's source scope the signed-in profile's real household, every frame.**
///
/// The data layer reads no session global for it any more (the scope takes the household as an
/// argument), so the only thing between a managed or Guest profile's household server and a
/// `household: false` mark that credits it to a friend is `Bridge::capture_views` passing
/// `household_ids()` (and `Bridge::with_measure`, for the publication a Bridge is built with). The
/// scope's own tests compute the household themselves and would stay green if either handed `&[]`
/// or a stale list. This one builds a Bridge and runs the capture itself, and moves the session
/// between two captures to prove the verdict follows it in the same frame.
#[test]
fn capture_views_grades_the_scope_with_the_sessions_current_household() {
    const OWNER_ID: i64 = 123;
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("search-scope-household");
    plx_plex::plex::reset_servers_for_test();
    let session_with = |home_users: Vec<plx_plex::plex::session::HomeUserRef>|
        plx_plex::plex::session::Session {
            client_id: "synthetic-client".into(), home_users, ..Default::default()
        };
    plx_plex::plex::session::save(&session_with(vec![
        plx_plex::plex::session::HomeUserRef { id: OWNER_ID, ..Default::default() }]));
    let sid = plx_plex::plex::register_for_test("scope-house", "127.0.0.1", 9, "t", "scope");
    plx_plex::plex::describe_server(sid, "Synthetic house", "", plx_plex::plex::GrantEvidence {
        owned: false, home: false, owner_id: OWNER_ID,
    });
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    assert!(rig.search.view().scope().sources()[0].household,
        "the first publication, taken at construction, is graded with the household too");

    rig.capture_views(&mut d);
    let inside = rig.search.clone();
    assert_eq!(inside.view().scope().sources().len(), 1, "the registered server is the scope");
    assert!(inside.view().scope().sources()[0].household,
        "the owner is a member of the signed-in profile's household");

    plx_plex::plex::session::save(&session_with(Vec::new()));
    rig.capture_views(&mut d);
    assert!(!rig.search.view().scope().sources()[0].household,
        "a household without that member regrades the scope in the very next capture");
    assert!(!rig.search.same_publication(&inside), "the regraded scope is a new publication");

    plx_plex::plex::reset_servers_for_test();
}
