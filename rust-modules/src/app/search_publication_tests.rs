//! Search publication freeze/notify timing across frames.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::{frame};

#[test]
fn search_publication_is_frozen_for_the_frame_then_notified_once_at_the_next_split() {
    let _guard = crate::testlock::serial();
    use crate::stores::search::SearchCmd;
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    // Search is owned by `rig` now, so there is no process-wide state to reset — the store's
    // whole lifetime is this test's own `rig` binding.
    rig.search_run(SearchCmd::SetQuery("before".into()));
    let _ = crate::stores::take_notices();
    frame(&mut d, &mut rig, AppArg::Search, tick(0), vec![]);
    let count = |d: &Dispatcher<AppHost>| notices(d).split("notices=").nth(1).unwrap().parse::<u32>().unwrap();
    let baseline = count(&d);
    let retained = rig.search.clone();
    d.emit(MachineId::Nav, Fx::App(AppFx::Store(StoreId::Search,
        StoreCmd::Search(SearchCmd::SetQuery("after".into())))));
    frame(&mut d, &mut rig, AppArg::Search, tick(1), vec![]);
    assert_eq!(rig.stores.search_snapshot(rig.directory.view()).view().query(), "after",
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
    let _ = crate::stores::take_notices();
    frame(&mut d, &mut rig, AppArg::Search, tick(8), vec![]);
    assert_eq!(rig.search.view().query(), "unqueued");
    assert_eq!(count(&d), baseline + 3, "a changed publication with no queued notice still reconciles");
}
