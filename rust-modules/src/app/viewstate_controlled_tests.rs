//! A controlled run (a recording or a replay of a Home boot) never writes a household's watch
//! state. ViewState's two doors, the detail hero's store command and the item menu's committed
//! row, both end in a server write (`/:/scrobble`, `/:/unscrobble`, the Continue Watching
//! removal), and a recorded run has no codec arm or admission rule that could reproduce one, so
//! each is refused by `bootstrap::effects` before the Bridge delivers it. The end-of-stream
//! fan-out is the third door and needs a second server, which a controlled boot refuses
//! (`unsupported Home server binding`); replay's IO denial on the recorded client is the fourth.
//!
//! This pins the first two, and with them the fact that nothing reaches the ViewState store or an
//! outbox. A change that gives either effect a recording arm fails here, before it can write to a
//! real account from a replay. It is a pin and not a fails-first test: it is green on the tree
//! that added it.
//!
//! No server is contacted: a controlled Bridge holds no client, and nothing here registers one.

use super::*;
use plx_data::stores::viewstate::ViewStateCmd;
use plx_data::stores::{StoreCmd, StoreId};
use plx_data::viewstate::{Scope, Write};
use plx_plex::plex::ServerId;
use plx_screens::item_menu::Action;
use plx_screens::registry::ItemMenuReq;

/// Send `fx` to a fresh controlled Bridge and say what that Bridge did with it.
fn refused(replay: bool, fx: AppFx) -> (Option<&'static str>, Bridge, usize) {
    let mt = unsafe { plx_base::task::MainThread::assume() };
    let initial = super::super::bootstrap::Initial::synthetic_home(17, 32517, None).unwrap();
    let mut bridge = Bridge::controlled_home(|| 0, &initial, &mt, replay);
    let mut emitted = Vec::new();
    let mut present = Present::new();
    let mut out = Effects::new(&mut emitted, MachineId::Instance(InstanceId(1)), &mut present);
    bridge.app_effect(MachineId::Instance(InstanceId(1)), fx, &mut out);
    (bridge.controlled_failure(), bridge, emitted.len())
}

#[test]
fn controlled_view_state_writes_fail_closed_before_any_io() {
    let _serial = plx_base::testlock::serial();
    let sid = ServerId::from_raw(1);
    for replay in [false, true] {
        for write in [Write::Watched, Write::Unwatched, Write::RemoveFromDeck] {
            let detail_hero = AppFx::Store(StoreId::ViewState, StoreCmd::ViewState(ViewStateCmd::Request {
                sid, rk: "1".into(), write, scope: Scope::Leaf, guid: String::new(),
            }));
            let (failure, mut bridge, emitted) = refused(replay, detail_hero);
            assert_eq!(failure, Some("unsupported Home store command"),
                "the detail hero's {write:?} must be refused (replay = {replay})");
            assert_eq!(emitted, 0, "a refused store command is not delivered");
            assert!(bridge.take_item_menu_reqs().is_empty());
            assert!(!bridge.stores.has_jobs(), "nothing sits on an outbox");
            assert!(!bridge.stores.viewstate.borrow().is_busy_for_test(), "no write was queued");
        }
        for act in [Action::MarkWatched("1".into()), Action::MarkUnwatched("1".into())] {
            let item_menu = AppFx::ItemMenu(ItemMenuReq {
                act: act.clone(), sid, item: None, season: None, loaded_episode: false, from_home: true,
            });
            let (failure, mut bridge, emitted) = refused(replay, item_menu);
            assert_eq!(failure, Some("unsupported Home application effect"),
                "the item menu's {act:?} must be refused (replay = {replay})");
            assert_eq!(emitted, 0);
            assert!(bridge.take_item_menu_reqs().is_empty(),
                "the committed row never reaches the loop that would write it");
            assert!(!bridge.stores.has_jobs());
            assert!(!bridge.stores.viewstate.borrow().is_busy_for_test());
        }
    }
}
