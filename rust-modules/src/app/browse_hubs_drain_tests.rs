//! Every `Bridge` entry that can step Browse starts the reads the step left in the same call: the
//! section hubs' (`StoreJob::BrowseHubs`), the filter menus' value lists (`StoreJob::BrowseDirectory`)
//! and the grid's pages (`StoreJob::BrowsePage`). The test runner parks a started read, so a drain
//! that goes missing from an entry shows twice: the job is still on Browse's outbox (the frame check
//! names it) and the runner has nothing parked. Each test names the kinds its entry starts.
//!
//! The rig is a Library section whose first read the system refused: its shelves are armed, the
//! section hubs' one claim is down and the refusal waits to be settled, so the next step the entry runs
//! asks for the read again. The grid's page and the value lists have never been asked for, so the
//! entries that pump the store (or that carry `LibraryWork::Genres` / `Letters`) ask for those too.

use super::test_support::*;
use super::*;
use plx_data::stores::browse::{BrowseCmd, LibraryWork, SectionAddress};
use plx_data::stores::StoreJob;
use plx_data::stores::notify::{Connector, Link, NotifyStore};
use plx_data::stores::notify_map::Shown;
use plx_ui::dispatch::Rig as _;
use plx_plex::plex::notify::{LibraryEntry, LibraryKind, LibraryState, Notification};
use plx_plex::plex::{GrantEvidence, ServerId};
use std::sync::{Arc, Mutex};

/// Joins a frame's effects and parts, as the dispatcher hands them to `Bridge::deliver`.
fn deliver_browse_work(rig: &mut Bridge) {
    let parts = CxParts { tick: tick(0), press: Default::default(), focus: Default::default(),
        owner: InputOwner::Entry(EntryId(0)) };
    let mut present = Present::new();
    let mut effects = Vec::new();
    let to = MachineId::Store(StoreId::Browse.ord());
    let mut fx = Effects::new(&mut effects, to, &mut present);
    assert_eq!(rig.deliver(to, &AppMsg::StoreWork(plx_data::stores::StoreWork::Browse), &parts, &mut fx),
        Handled::Yes);
}

/// The armed section, its first read refused by the system: nothing is out, nothing is on an outbox.
struct Rig {
    bridge: Bridge,
    target: SectionAddress,
    own: ServerId,
    _cleanup: DirectoryPolicyCleanup,
}

fn armed_rig() -> Rig {
    plx_plex::plex::reset_servers_for_test();
    let own = plx_plex::plex::register_for_test("hubs-drain-own", "127.0.0.1", 9, "synthetic", "fixture");
    let shared = plx_plex::plex::register_for_test("hubs-drain-shared", "127.0.0.1", 10, "synthetic", "fixture");
    assert!(plx_plex::plex::set_current(own));
    let cleanup = DirectoryPolicyCleanup;
    let mut bridge = Bridge::for_test(|| 0);
    bridge.park_page_reads_for_test();
    bridge.seed_registered_browse_for_test([own, shared]);
    bridge.browse_run(BrowseCmd::SetCur(0));
    let id = bridge.stores.browse.borrow_mut().hubs_snapshot().view().id().expect("a section is current");
    let target = SectionAddress { epoch: id.epoch, sid: id.sid, section: id.section };
    crate::app::adapters::stores::with_refused_hubs_launches_for_test(|| {
        bridge.browse_run(BrowseCmd::Addressed { target, work: LibraryWork::Hubs { may_publish: true } });
    });
    let rig = Rig { bridge, target, own, _cleanup: cleanup };
    rig.assert_down();
    rig
}

impl Rig {
    /// Nothing is out and nothing waits on an outbox.
    fn assert_down(&self) {
        assert!(!self.bridge.stores.browse.borrow().hubs_read_out_for_test(), "no claim is up");
        self.bridge.assert_no_store_jobs_left_for_test();
    }

    /// The entry started exactly the reads of these kinds, in the order its step asked: Browse's
    /// outbox is empty (the entry drained it) and the runner holds them.
    fn assert_started(&mut self, kinds: &[&str]) {
        self.bridge.assert_no_store_jobs_left_for_test();
        let started: Vec<&str> = self.bridge.take_held_store_jobs_for_test().iter().map(kind).collect();
        assert_eq!(started, kinds, "the entry started the reads its step asked for, in the call that emitted them");
    }

    /// The entry started exactly one section-hubs read, and its claim is up.
    fn assert_one_read_started(&mut self) {
        assert!(self.bridge.stores.browse.borrow().hubs_read_out_for_test(), "the entry's step asked");
        self.assert_started(&["BrowseHubs"]);
    }
}

/// The kind of a Browse job, by name.
fn kind(job: &StoreJob) -> &'static str {
    match job {
        StoreJob::BrowseHubs(..) => "BrowseHubs",
        StoreJob::BrowseDirectory(..) => "BrowseDirectory",
        StoreJob::BrowsePage(..) => "BrowsePage",
        _ => "other",
    }
}

/// The entry the Library page's shelves go through (`content::library_publication_command`).
#[test]
fn browse_run_starts_the_read_its_step_emitted_in_the_same_call() {
    let _guard = plx_base::testlock::serial();
    let mut rig = armed_rig();
    let target = rig.target;
    rig.bridge.browse_run(BrowseCmd::Addressed { target, work: LibraryWork::Hubs { may_publish: true } });
    rig.assert_one_read_started();
}

/// The Browse pump (`StoreWork::Browse`) settles the refusal and asks again, and starts the read; the
/// same step asks for the grid's first page, which is started too.
#[test]
fn the_browse_pump_arm_starts_the_reads_it_asked_for_again_after_a_refusal() {
    let _guard = plx_base::testlock::serial();
    let mut rig = armed_rig();
    deliver_browse_work(&mut rig.bridge);
    assert!(rig.bridge.stores.browse.borrow().hubs_read_out_for_test(), "the entry's step asked for the shelves");
    rig.assert_started(&["BrowseHubs", "BrowsePage"]);
}

/// The Library page's filter menu asks for the genre list, and the A-Z rail for the letters
/// (`LibraryWork::Genres` / `Letters`, sent every tick): `browse_run` starts each read in the call
/// that emitted it. Two ticks before the answer ask for nothing more.
#[test]
fn browse_run_starts_a_directory_read_its_step_emitted_in_the_same_call() {
    let _guard = plx_base::testlock::serial();
    for work in [LibraryWork::Genres, LibraryWork::Letters] {
        let mut rig = armed_rig();
        rig.bridge.stores.browse.borrow_mut().owe_letters_for_test();
        let target = rig.target;
        rig.bridge.browse_run(BrowseCmd::Addressed { target, work: work.clone() });
        rig.bridge.browse_run(BrowseCmd::Addressed { target, work: work.clone() });
        rig.assert_started(&["BrowseDirectory"]);
    }
}

/// ViewState's terminal `HubsInvalidateAll` reaches the section-hubs step through the `Stores`
/// closure: `VIEWSTATE_FILLS` names Browse, so the pass drains it.
#[test]
fn the_viewstate_pump_starts_the_read_its_burst_end_invalidation_asked_for() {
    let _guard = plx_base::testlock::serial();
    let mut rig = armed_rig();
    rig.bridge.stores.viewstate.borrow_mut().owe_hubs_refresh_for_test();
    let _ = rig.bridge.viewstate_pump();
    rig.assert_one_read_started();
}

/// A connector that keeps the link it was given, for the test to push a notification down.
#[derive(Clone, Default)]
struct Wire(Arc<Mutex<Vec<Link>>>);

impl Connector for Wire {
    fn connect(&mut self, _sid: ServerId, link: Link) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(link);
        true
    }
}

/// A library change on an owned server invalidates its sections' shelves
/// (`BrowseCmd::HubsInvalidateServer`) through the notification map: `NOTIFY_FILLS` names Browse,
/// so the pass drains it.
#[test]
fn the_notify_pump_starts_the_read_a_library_change_asked_for() {
    let _guard = plx_base::testlock::serial();
    let mut rig = armed_rig();
    plx_plex::plex::describe_server(rig.own, "Server A", "", GrantEvidence::ours());
    let wire = Wire::default();
    rig.bridge.stores.notify = NotifyStore::with_connector_for_test(Box::new(wire.clone()));

    let _ = rig.bridge.notify_pump(&[], Shown::ALL);
    let link = wire.0.lock().unwrap_or_else(|e| e.into_inner()).first().cloned().expect("the feed connected");
    link.push(Notification::Library(vec![LibraryEntry {
        item_id: "a".into(), kind: LibraryKind::Movie, state: LibraryState::Created, section: None }]));
    let _ = rig.bridge.notify_pump(&[], Shown::ALL);

    rig.assert_one_read_started();
}

/// The membership re-query a page pushed over the Library page owes is pumped by
/// `Bridge::reconcile_pump` (the covered Library page does not tick its own store), and that
/// pump's section-hubs step is drained there too.
#[test]
fn the_reconcile_pump_starts_the_read_its_browse_pump_asked_for() {
    let _guard = plx_base::testlock::serial();
    let mut rig = armed_rig();
    let (target, own) = (rig.target, rig.own);
    rig.bridge.browse_run(BrowseCmd::Addressed { target, work: LibraryWork::Commit {
        select: false, choice: false, query: Some(plx_data::stores::browse::QueryEdit::Unwatched(true)) } });
    rig.bridge.stores.browse.borrow_mut().seed_items_for_test(3);
    rig.bridge.browse_run(BrowseCmd::ApplyView { at: plx_data::viewstate::ViewAt::leaf(own, "1"),
        edit: plx_data::viewstate::ViewEdit::Facts { watched: true, unwatched: false, resume_ms: 0 } });
    rig.bridge.browse_run(BrowseCmd::Addressed { target, work: LibraryWork::Requery });
    assert!(rig.bridge.stores.browse.borrow().requery_owed(), "premise: a membership re-query is owed");
    rig.assert_down();

    let _ = rig.bridge.reconcile_pump();

    assert!(rig.bridge.stores.browse.borrow().hubs_read_out_for_test(), "the pump's step asked for the shelves");
    rig.assert_started(&["BrowseHubs", "BrowsePage"]);
    plx_base::task::drain_workers_for_test();
}
