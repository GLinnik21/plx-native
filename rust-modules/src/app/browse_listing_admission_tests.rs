//! A controlled Home run never starts a Library listing read. The grid's pages
//! (`StoreJob::BrowsePage`) and the filter menus' value lists (`StoreJob::BrowseDirectory`) are
//! `Admission::Direct`: the runner starts them and the run's ledger records nothing for them, which
//! is only sound because no controlled run can ask for one. Two walls say so, and this file pins
//! both, in record mode (`replay == false`: the launch runs and its answer is written down) and in
//! replay mode (the recording answers, nothing runs):
//!
//! - the effect encoder (`bootstrap::effects`) accepts only `Browse(Reset)` and `Browse(Discovery)`
//!   as Browse commands and refuses `StoreWork::Browse`, so a frame that would carry a page or a
//!   directory command fails the run before it reaches the Bridge, and the replay fixtures cannot
//!   hold one;
//! - the commands a controlled run does deliver (Reset, a Discovery landing) step Browse without
//!   asking for a page or a directory, even on a table that has a Library section and a client for
//!   it, which is the table that DOES ask outside a controlled run (the first test).

use plx_data::stores::browse::{BrowseCmd, LibraryWork, SectionAddress};
use plx_data::stores::{StoreJob, StoreWork};
use plx_plex::plex::ServerId;

use super::*;

/// Puts the registry back to its inactive state when a test ends, however it ends, after any
/// worker the dispatch started has finished. Declare it AFTER the serial guard, so it drops first.
struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        plx_base::task::drain_workers_for_test();
        plx_plex::plex::reset_servers_for_test();
    }
}

/// Two servers registered at ports nothing listens on, so a fetch that does start fails at once.
fn two_servers() -> (ServerId, ServerId, &'static plx_plex::plex::Client) {
    plx_plex::plex::reset_servers_for_test();
    let own = plx_plex::plex::register_for_test("listing-admission", "127.0.0.1", 9, "t", "cid");
    let shared = plx_plex::plex::register_for_test("listing-admission-shared", "127.0.0.1", 10, "t", "cid");
    assert!(plx_plex::plex::set_current(own));
    (own, shared, plx_plex::plex::client_for(own).expect("registered"))
}

/// One message to a store, as the central dispatcher delivers it.
fn deliver(bridge: &mut Bridge, msg: AppMsg) {
    let parts = CxParts { tick: Tick::default(), press: Default::default(),
        focus: Default::default(), owner: InputOwner::Entry(EntryId(0)) };
    let mut present = plx_machine::present::Present::default();
    let mut out = Vec::new();
    let to = MachineId::Store(StoreId::Browse.ord());
    let mut fx = Effects::new(&mut out, to, &mut present);
    bridge.deliver(to, &msg, &parts, &mut fx);
}

/// The Browse store of `bridge` holds a registered two-source table whose first section is current:
/// the table a grid or a filter menu is asked of.
fn seeded(bridge: &mut Bridge, own: ServerId, shared: ServerId) -> SectionAddress {
    bridge.seed_registered_browse_for_test([own, shared]);
    let mut browse = bridge.stores.browse.borrow_mut();
    browse.run_for_test(BrowseCmd::SetCur(0));
    let id = browse.hubs_snapshot().view().id().expect("a section is current");
    SectionAddress { epoch: id.epoch, sid: id.sid, section: id.section }
}

/// A discovery landing for source 0 of `bridge`'s table that brings a Movies section, as a recording
/// carries it (`plx_data::browse::record::decode` of its wire form).
fn discovery_landing(bridge: &Bridge, client: &'static plx_plex::plex::Client) -> plx_data::browse::record::Result {
    let epoch = bridge.stores.browse.borrow().table_epoch_for_test();
    plx_data::browse::record::decode(serde_json::json!({
        "kind": "discovery", "version": 1, "epoch": epoch, "source": 0, "sid": client.id().raw(),
        "client": client.instance_gen(), "token_gen": client.token_gen(), "name": "home",
        "what": { "Sections": [[1, "Movies", "movie"]] },
    }), |instance| (instance == client.instance_gen()).then_some(client)).expect("a well-formed landing")
}

/// Browse asks for no page and no directory: nothing is on any outbox, and neither claim is up
/// (a started read holds its claim until a pump takes its landing, and a controlled run has none).
#[track_caller]
fn assert_no_listing_read(bridge: &Bridge, after: &str) {
    bridge.assert_no_store_jobs_left_for_test();
    let browse = bridge.stores.browse.borrow();
    assert!(!browse.page_read_out_for_test(), "{after}: a page read was started");
    assert!(!browse.directory_read_out_for_test(), "{after}: a directory read was started");
}

/// The premise: outside a controlled run the very table the pins below use DOES ask. A filter menu
/// tick asks for the genre list and a pump asks for the first page, each as the typed job its store
/// emits.
#[test]
fn outside_a_controlled_run_the_same_table_asks_for_a_directory_and_a_page() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    let (own, shared, _client) = two_servers();
    let mut bridge = Bridge::for_test(|| 0);
    bridge.park_page_reads_for_test();
    let target = seeded(&mut bridge, own, shared);

    bridge.browse_run(BrowseCmd::Addressed { target, work: LibraryWork::Genres });
    deliver(&mut bridge, AppMsg::StoreWork(StoreWork::Browse));

    let started: Vec<&str> = bridge.take_held_store_jobs_for_test().iter().map(|job| match job {
        StoreJob::BrowseDirectory(..) => "BrowseDirectory",
        StoreJob::BrowsePage(..) => "BrowsePage",
        _ => "other",
    }).collect();
    assert_eq!(started, ["BrowseDirectory", "BrowsePage"]);
}

/// A controlled run's Browse commands (Reset, a Discovery landing) step the store without asking
/// for a page or a directory, on a table that has a section and a client for it, in both modes.
#[test]
fn a_controlled_runs_browse_commands_emit_no_page_or_directory_read() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    let mt = unsafe { plx_base::task::MainThread::assume() };
    for replay in [false, true] {
        let (own, shared, client) = two_servers();
        let initial = crate::app::bootstrap::Initial::synthetic_home(17, 32517, None).unwrap();
        let mut bridge = Bridge::controlled_home(|| 0, &initial, &mt, replay);
        seeded(&mut bridge, own, shared);
        let mode = if replay { "replay" } else { "record" };

        let landing = discovery_landing(&bridge, client);
        deliver(&mut bridge, AppMsg::Store(StoreCmd::Browse(BrowseCmd::Discovery(landing))));
        assert_no_listing_read(&bridge, &format!("{mode}: a discovery landing"));

        deliver(&mut bridge, AppMsg::Store(StoreCmd::Browse(BrowseCmd::Reset)));
        assert_no_listing_read(&bridge, &format!("{mode}: a reset"));
        assert_eq!(bridge.controlled_failure(), None, "{mode}: the run did not fail");
    }
}

/// The encoder accepts the two commands a controlled run delivers and refuses every command and
/// every pump that would ask for a page or a directory: the frame carrying one fails the run, so no
/// recording holds it and no replay can reach it.
#[test]
fn the_controlled_encoder_refuses_every_way_to_ask_for_a_page_or_a_directory() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    let (own, shared, client) = two_servers();
    let mut bridge = Bridge::for_test(|| 0);
    let target = seeded(&mut bridge, own, shared);
    let encode = |command: StoreCmd| crate::app::bootstrap::effects::app(&AppFx::Store(StoreId::Browse, command));

    assert!(encode(StoreCmd::Browse(BrowseCmd::Reset)).is_ok(), "Reset is a controlled command");
    let landing = discovery_landing(&bridge, client);
    assert!(encode(StoreCmd::Browse(BrowseCmd::Discovery(landing))).is_ok(), "so is a discovery landing");

    for work in [LibraryWork::Genres, LibraryWork::Letters, LibraryWork::Want { lo: 0, hi: 12, focus: None },
        LibraryWork::Retry, LibraryWork::Requery] {
        assert_eq!(encode(StoreCmd::Browse(BrowseCmd::Addressed { target, work: work.clone() })),
            Err("unsupported Home store command"), "{work:?} is not a controlled command");
    }
    assert_eq!(crate::app::bootstrap::effects::app(&AppFx::StoreWork(StoreWork::Browse)),
        Err("unsupported Home store work"), "the Browse pump is not a controlled pump");
}
