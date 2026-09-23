//! The Library table and its per-section listing, as a machine over `crate::browse`
//! (`docs/stores-as-machines.md`). The vocabulary is [`BrowseCmd`]; each [`BrowseStore`] owns its
//! main-thread state, worker adapter and notice while `crate::browse` retains the core
//! implementation.

use crate::plex::ServerId;
use crate::ui::machine::{Cx, Effects, Handled, Machine};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use super::StoreEv;
#[cfg(test)]
use super::StoreId;

#[allow(unused_imports)] // Shared owner vocabulary; some members are feature/test specific.
pub(crate) use crate::browse::{
    Cursor, CursorAt, GenreEntry, SecFetch, SecKind, SortEntry, SourceState, SrcGroup, SrcRow,
};
#[allow(unused_imports)] // Shared owner vocabulary; some members are feature/test specific.
pub(crate) use crate::browse::section_hubs::{HubsId, HubsSnapshot, HubsView, Publication};
#[allow(unused_imports)] // Shared owner vocabulary; some members are feature/test specific.
pub(crate) use crate::browse::view::{
    DirectorySnapshot, DirectoryView, ListingId, ListingSnapshot, ListingView, SectionView,
};

/// Owner-aware first-run gate shared by boot and login. The retained directory is the granted
/// Browse publication for this Bridge.
pub(crate) mod onboard {
    pub(crate) fn asks(directory: super::DirectoryView<'_>) -> bool {
        let session = crate::plex::session::peek();
        crate::plex::pins::asks(
            directory.sources().len(),
            session.pins_for(&crate::plex::session::current_profile_key()),
        )
    }
}

/// One frame's retained Browse publication. The directory is captured first because resolving
/// profile pins may repoint the current section; listing and section hubs are captured only after
/// that decision, so all three views describe one owner state.
#[derive(Clone)]
pub(crate) struct BrowsePublications {
    pub(crate) listing: ListingSnapshot,
    pub(crate) directory: DirectorySnapshot,
    pub(crate) section_hubs: HubsSnapshot,
}

/// Every mutation of the browse store a screen may ask for.
#[derive(Clone, Debug)]
pub(crate) enum BrowseCmd {
    Discovery(crate::browse::record::Result),
    RetrySource {
        epoch: u32,
        sid: ServerId,
    },
    /// Execute deferred Library work against the source and table epoch captured by the screen.
    Addressed {
        target: SectionAddress,
        work: LibraryWork,
    },
    /// Point the listing at section `i` (a pill or library-row press, committed at the fade floor).
    #[cfg(test)]
    SetCur(usize),
    RecheckShares,
    /// The Home editor's draft commit: one record for the whole session, carrying **the rows the
    /// viewer ANSWERED** — `(section index, the value they left it at)` — and not the rows the
    /// editor happened to be showing.
    ///
    /// The distinction is provenance, and only the screen holding the draft has it
    /// (`screens::onboard`'s `answered`, which is the set of rows a press actually moved, recorded
    /// by the press rather than recovered from the values afterwards). A row missing from this list
    /// is a value nobody chose: it keeps re-deriving from its default rather than being frozen as
    /// though it were a decision (`plex::pins::answers`). An EMPTY list is still a commit — the
    /// question was put and every default was left alone.
    ApplyPins(Vec<(usize, bool)>),
    RetryDiscovery,
    /// The profile/account switch: wipe everything and supersede everything in flight.
    Reset,
    HubsInvalidateAll,
    /// The optimistic half of a view-state write, on the grid and the shelves.
    SetWatchedLocal {
        sid: ServerId,
        rk: String,
        on: bool,
    },
    LeftTheDeck {
        sid: ServerId,
        rk: String,
    },
}

/// The production owner of Browse's main-thread state, worker transport and notice generation.
pub(crate) struct BrowseStore {
    state: crate::browse::BrowseState,
    adapter: Arc<crate::browse::BrowseAdapter>,
    notice_gen: AtomicU32,
    notice_dirty: AtomicBool,
}

impl Default for BrowseStore {
    fn default() -> Self {
        Self {
            state: Default::default(),
            adapter: Arc::new(Default::default()),
            notice_gen: AtomicU32::new(0),
            notice_dirty: AtomicBool::new(false),
        }
    }
}

impl BrowseStore {
    fn bump(&self) -> u32 {
        self.notice_dirty.store(true, Ordering::Relaxed);
        self.notice_gen.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub(crate) fn gen(&self) -> u32 {
        self.notice_gen.load(Ordering::Relaxed)
    }

    pub(crate) fn take_notice(&self) -> Option<u32> {
        self.notice_dirty.swap(false, Ordering::Relaxed).then(|| self.gen())
    }

    pub(crate) fn run(&mut self, cmd: BrowseCmd) -> bool {
        let save_cursor = matches!(&cmd, BrowseCmd::Addressed {
            work: LibraryWork::SaveCursor { .. }, ..
        });
        let quiet = matches!(&cmd, BrowseCmd::Addressed {
            work: LibraryWork::Want { .. } | LibraryWork::Letters | LibraryWork::Genres, ..
        });
        let roster_changed = if matches!(&cmd, BrowseCmd::RecheckShares) {
            self.sync_roster()
        } else {
            false
        };
        if matches!(&cmd, BrowseCmd::Reset) {
            self.adapter = Arc::new(Default::default());
        }
        let discovery = matches!(&cmd, BrowseCmd::Discovery(_));
        let changed = self.state.run_owned(&self.adapter, cmd) || roster_changed;
        if (save_cursor && changed) || (!quiet && !save_cursor && (!discovery || changed)) {
            self.bump();
        }
        changed
    }

    pub(crate) fn pump_with_gate(&mut self, gate: &crate::ui::landgate::Gate) -> super::StoreOutcome {
        if !self.state.pump_needs_work(&self.adapter) {
            return Default::default();
        }
        let roster_changed = self.sync_roster();
        let source_gen = self.state.source_list_gen();
        let mut outcome = self.state.pump_owned_with_gate(&self.adapter, gate);
        outcome.changed |= roster_changed || source_gen != self.state.source_list_gen();
        if outcome.changed {
            self.bump();
        }
        outcome
    }
    #[cfg(test)]
    pub(crate) fn pump(&mut self) -> super::StoreOutcome {
        self.pump_with_gate(crate::ui::landgate::fixture_gate())
    }

    pub(crate) fn discover_pump_with_gate(&mut self, gate: &crate::ui::landgate::Gate) -> super::StoreOutcome {
        if !self.state.discovery_needs_pump(&self.adapter) {
            return Default::default();
        }
        let roster_changed = self.sync_roster();
        let source_gen = self.state.source_list_gen();
        let mut outcome = self.state.discover_pump_owned_with_gate(&self.adapter, gate);
        outcome.changed |= roster_changed || source_gen != self.state.source_list_gen();
        if outcome.changed { self.bump(); }
        outcome
    }
    #[cfg(test)]
    pub(crate) fn discover_pump(&mut self) -> super::StoreOutcome {
        self.discover_pump_with_gate(crate::ui::landgate::fixture_gate())
    }

    pub(crate) fn listing_snapshot(&mut self) -> ListingSnapshot {
        self.state.listing_snapshot()
    }

    pub(crate) fn capture_directory(&mut self, snapshot: &mut DirectorySnapshot) {
        snapshot.capture_from(&mut self.state);
    }

    pub(crate) fn hubs_snapshot(&mut self) -> HubsSnapshot {
        self.state.hubs_snapshot(self.state.cur())
    }

    pub(crate) fn take_discovery(&mut self) -> Option<crate::browse::record::Result> {
        crate::browse::record::take_from(&self.adapter)
    }

    fn sync_roster(&mut self) -> bool {
        let sync = self.state.sync_roster_owned();
        if sync.retire_adapter {
            self.adapter = Arc::new(Default::default());
        }
        sync.changed
    }

    pub(crate) fn controlled_discover(
        &mut self,
        launch: &mut dyn FnMut(crate::browse::DiscoveryRequest) -> bool,
    ) {
        let changed = self.sync_roster();
        self.state.controlled_discover_owned(&self.adapter, launch);
        if changed {
            self.bump();
        }
    }

    pub(crate) fn apply_discovery(
        &mut self,
        result: &crate::browse::record::Result,
        preferences: &crate::plex::session::Session,
    ) -> super::StoreOutcome {
        let outcome = crate::browse::record::apply_to(
            &mut self.state, &self.adapter, result, Some(preferences));
        if outcome.changed {
            self.bump();
        }
        outcome
    }

    #[cfg(test)]
    pub(crate) fn spawn_page_for_test(
        &mut self,
        client: &'static crate::plex::Client,
        title: &str,
    ) -> (std::sync::mpsc::SyncSender<()>, std::sync::mpsc::Receiver<()>) {
        crate::browse::spawn_owned_page_for_test(&self.state, &self.adapter, client, title)
    }

    #[cfg(test)]
    pub(crate) fn seed_items_for_test(&mut self, count: usize) {
        crate::browse::seed_items_for_owner_test(&mut self.state, count);
    }

    #[cfg(test)]
    pub(crate) fn seed_sources_for_test(&mut self, count: usize, reachable: bool) {
        crate::browse::seed_sources_for_owner_test(&mut self.state, count, reachable);
    }

    #[cfg(test)]
    pub(crate) fn seed_pins_for_test(&mut self, pinned: &[bool]) {
        crate::browse::seed_pins_for_owner_test(&mut self.state, pinned);
    }

    #[cfg(test)]
    pub(crate) fn set_pinned_for_test(&mut self, index: usize, on: bool) {
        crate::browse::set_pinned_for_owner_test(&mut self.state, index, on);
    }

    #[cfg(test)]
    pub(crate) fn land_pin_for_test(&mut self, pinned: bool) {
        crate::browse::land_pin_for_owner_test(&mut self.state, pinned);
    }

    #[cfg(test)]
    pub(crate) fn seed_two_source_table_for_test(&mut self) {
        crate::browse::seed_two_source_table_for_owner_test(&mut self.state);
    }

    #[cfg(test)]
    pub(crate) fn seed_registered_table_for_test(&mut self, sids: [ServerId; 2]) {
        crate::browse::seed_registered_table_for_owner_test(&mut self.state, sids);
    }

    #[cfg(test)]
    pub(crate) fn append_section_for_test(
        &mut self,
        source: usize,
        key: i64,
        title: &str,
        kind: SecKind,
    ) {
        crate::browse::append_section_for_owner_test(&mut self.state, source, key, title, kind);
    }

    #[cfg(test)]
    pub(crate) fn seed_letter_counts_for_test(&mut self, letters: &[(&str, i64)]) {
        crate::browse::seed_letter_counts_for_owner_test(&mut self.state, letters);
    }

    #[cfg(test)]
    pub(crate) fn seed_query_choices_for_test(
        &mut self,
        sorts: Vec<SortEntry>,
        genres: Vec<GenreEntry>,
    ) {
        crate::browse::seed_query_choices_for_owner_test(&mut self.state, sorts, genres);
    }

    #[cfg(test)]
    pub(crate) fn seed_shelves_for_test(
        &mut self,
        section: usize,
        titles: &[&str],
        per_row: usize,
    ) {
        crate::browse::section_hubs::seed_shelves_for_owner_test(
            &mut self.state,
            section,
            titles,
            per_row,
        );
    }

    #[cfg(test)]
    pub(crate) fn seed_landscape_for_test(&mut self, section: usize, show: &str) {
        crate::browse::section_hubs::seed_landscape_for_owner_test(
            &mut self.state,
            section,
            show,
        );
    }

    #[cfg(test)]
    pub(crate) fn pinned_for_test(&self, index: usize) -> bool {
        self.state.pinned_for_test(index)
    }

    #[cfg(test)]
    pub(crate) fn current_for_test(&self) -> usize {
        self.state.cur()
    }

    #[cfg(test)]
    pub(crate) fn table_epoch_for_test(&self) -> u32 {
        self.state.table_epoch()
    }

    #[cfg(test)]
    pub(crate) fn resolve_section_for_test(
        &self,
        epoch: u32,
        sid: ServerId,
        key: i64,
    ) -> Option<usize> {
        self.state.resolve_section(epoch, sid, key)
    }

    #[cfg(test)]
    pub(crate) fn prepare_discovery_replay_for_test(&mut self, sid: ServerId, epoch: u32) {
        self.state.prepare_discovery_replay_for_test(&self.adapter, sid, epoch);
    }

    #[cfg(test)]
    pub(crate) fn discovery_policy_for_test(&self) -> (bool, u32, bool) {
        self.state.discovery_policy_for_test(&self.adapter)
    }

    #[cfg(test)]
    pub(crate) fn queue_discovery_for_test(
        &mut self,
        client: &'static crate::plex::Client,
        token_gen: u32,
        ok: bool,
    ) {
        crate::browse::queue_discovery_for_owner_test(
            &mut self.state, &self.adapter, client, token_gen, ok);
    }

    #[cfg(test)]
    pub(crate) fn prepare_page_for_test(&mut self, sid: ServerId) {
        crate::browse::prepare_page_for_owner_test(&mut self.state, sid);
    }

    // Dev-only: used only by `app/bridge.rs`'s `production_bridges_do_not_share_browse_state_or_landings`,
    // gated under `devtriggers` (see that test's own comment).
    #[cfg(all(test, feature = "devtriggers"))]
    pub(crate) fn has_page_result_for_test(&self) -> bool {
        crate::browse::adapter_has_page_for_test(&self.adapter)
    }

    #[cfg(test)]
    fn queue_genre_for_test(&mut self, client: &'static crate::plex::Client) {
        crate::browse::queue_genre_for_owner_test(&mut self.state, &self.adapter, client);
    }

    #[cfg(test)]
    fn queue_page_failure_for_test(&mut self, client: &'static crate::plex::Client) {
        crate::browse::queue_page_failure_for_owner_test(&mut self.state, &self.adapter, client);
    }

    #[cfg(test)]
    fn source_list_gen_for_test(&self) -> u32 {
        self.state.source_list_gen()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SectionAddress {
    pub epoch: u32,
    pub sid: ServerId,
    pub section: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum QueryEdit {
    Sort { key: String, desc: bool },
    Unwatched(bool),
    Genre(Option<String>),
}

#[derive(Clone, Debug)]
pub(crate) enum LibraryWork {
    SaveCursor {
        query: u32,
        cursor: crate::browse::Cursor,
    },
    /// Selection and query coexist and commit in this order, inside one store delivery.
    Commit {
        select: bool,
        choice: bool,
        query: Option<QueryEdit>,
    },
    Want {
        lo: usize,
        hi: usize,
    },
    Letters,
    Genres,
    Hubs {
        may_publish: bool,
    },
    Retry,
}

impl<H: super::StoreEffectHost> Machine<H> for BrowseStore {
    type Ev = StoreEv<BrowseCmd>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            StoreEv::Cmd(c) => {
                self.run(c.clone());
            }
            StoreEv::Pump { .. } => {
                self.pump_with_gate(&crate::ui::landgate::Gate::default()).endpoints.emit(fx);
            }
        }
        Handled::Yes
    }
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    #[test]
    fn explicit_store_commands_answer_now_and_capture_one_consistent_publication() {
        let _guard = crate::testlock::serial();
        let stores = crate::stores::Stores::default();
        {
            let mut browse = stores.browse.borrow_mut();
            browse.seed_two_source_table_for_test();
            browse.seed_items_for_test(3);
        }
        let mut directory = DirectorySnapshot::default();
        let publication = stores.capture_browse(&mut directory);
        let view = publication.directory.view();
        assert_eq!(view.section_count(), 4);
        assert_eq!(view.tab_count(), 2);
        assert_eq!(view.tab_kind(0), Some(crate::browse::SecKind::Movie));
        assert_eq!(view.tab_of_kind(crate::browse::SecKind::Show), Some(1));
        assert_eq!(view.pinned_count(), 2);
        assert_eq!(view.favorite_sections().len(), 4);
        assert_eq!(publication.listing.view().total(), 3);
        assert_eq!(publication.section_hubs.view().id().unwrap().section,
            view.sections()[view.current().unwrap()].key);

        assert!(stores.browse_run(BrowseCmd::SetCur(2)),
            "the direct command returns the store's answer synchronously");
        let publication = stores.capture_browse(&mut directory);
        let view = publication.directory.view();
        let current = view.current().unwrap();
        let id = publication.listing.view().id().unwrap();
        assert_eq!(current, 2);
        assert_eq!(id.section, view.sections()[current].key,
            "listing capture must follow directory's possible current-section repoint");
        assert_eq!(publication.section_hubs.view().id().unwrap().section, id.section,
            "section hubs and listing must describe the same post-directory section");
    }

    #[test]
    fn separate_store_aggregates_never_share_state_transport_or_notices() {
        let _guard = crate::testlock::serial();
        let first = crate::stores::Stores::default();
        let second = crate::stores::Stores::default();
        first.browse.borrow_mut().seed_two_source_table_for_test();
        first.browse.borrow_mut().seed_items_for_test(1);
        assert!(first.browse.borrow_mut().listing_snapshot().view().item(0).is_some());
        assert!(second.browse.borrow_mut().listing_snapshot().view().item(0).is_none());
        assert!(!Arc::ptr_eq(
            &first.browse.borrow().adapter,
            &second.browse.borrow().adapter,
        ));
        // Establish a clean baseline on `first` before the real op: notices are owned per
        // `Stores` instance (one per `Bridge`), not shared, so this has no effect on `second` —
        // it just guards against a notice this test's own seeding might one day bump (it
        // currently doesn't: seeding writes `self.state` directly and never calls `bump()`,
        // which is exactly why the assertion below expects generation 1, not 2+) — the same
        // idiom the other contract tests in this module already use before their own
        // exact-equality check.
        let _ = first.take_notices();
        first.browse_run(BrowseCmd::Reset);
        assert_eq!(first.take_notices(), [(StoreId::Browse, 1)]);
        assert!(second.take_notices().is_empty());
    }

    #[test]
    fn snapshots_and_an_idle_pump_are_quiet() {
        let _guard = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let stores = crate::stores::Stores::default();
        let _ = stores.take_notices();
        let before = stores.browse.borrow().gen();
        let mut directory = DirectorySnapshot::default();
        {
            let mut browse = stores.browse.borrow_mut();
            let _ = browse.listing_snapshot();
            browse.capture_directory(&mut directory);
            let _ = browse.hubs_snapshot();
            assert_eq!(browse.pump(), super::super::StoreOutcome::default());
            assert_eq!(browse.discover_pump(), super::super::StoreOutcome::default());
        }
        assert_eq!(stores.browse.borrow().gen(), before);
        assert!(stores.take_notices().is_empty());
    }

    #[test]
    fn an_owned_discovery_landing_is_consumed_once_and_noticed_once() {
        let _guard = crate::testlock::serial();
        let session = crate::plex::session::TempSession::new("browse-preowner");
        session.watching("u-browse-preowner");
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test(
            "browse-preowner", "127.0.0.1", 9, "synthetic", "fixture");
        let client = crate::plex::client_for(sid).unwrap();
        let stores = crate::stores::Stores::default();
        stores.browse.borrow_mut().queue_discovery_for_test(
            client, client.token_gen(), false);
        let before_landing = stores.browse.borrow().gen();
        let endpoints = stores.browse.borrow_mut().discover_pump();
        assert_eq!(endpoints.endpoints.iter().map(|request| request.sid).collect::<Vec<_>>(), [sid]);
        assert_eq!(stores.browse.borrow().gen(), before_landing + 1);
        assert_eq!(stores.browse.borrow().take_notice(), Some(before_landing + 1));
        assert_eq!(stores.browse.borrow().take_notice(), None,
            "one discovery landing owes one notice on the addressed Browse owner");
        assert_eq!(stores.browse.borrow_mut().discover_pump().endpoints.iter().count(), 0,
            "the transferred adapter result is consumed exactly once");
        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn controlled_discovery_apply_and_directory_landing_each_bump_exactly_once() {
        let _guard = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test(
            "browse-owned-landings", "127.0.0.1", 9, "synthetic", "fixture");
        let stores = crate::stores::Stores::default();
        stores.browse.borrow_mut().seed_registered_table_for_test([sid, sid]);
        let _ = stores.take_notices();
        let client = crate::plex::client_for(sid).unwrap();

        stores.browse.borrow_mut().queue_discovery_for_test(
            client, client.token_gen(), true);
        let result = stores.browse.borrow_mut().take_discovery().unwrap();
        let before_discovery = stores.browse.borrow().gen();
        let _ = stores.browse.borrow_mut().apply_discovery(
            &result, &crate::plex::session::Session::default());
        assert_eq!(stores.browse.borrow().gen(), before_discovery + 1);
        assert_eq!(stores.take_notices(), [(StoreId::Browse, before_discovery + 1)]);
        assert!(stores.take_notices().is_empty());

        stores.browse.borrow_mut().queue_genre_for_test(client);
        let before_directory = stores.browse.borrow().gen();
        assert!(stores.browse.borrow_mut().pump().changed,
            "a current directory landing is an observable Browse change");
        assert_eq!(stores.browse.borrow().gen(), before_directory + 1);
        assert_eq!(stores.take_notices(), [(StoreId::Browse, before_directory + 1)]);
        assert!(stores.take_notices().is_empty());

        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn reset_rotates_the_adapter_away_from_a_late_old_worker() {
        let _guard = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test(
            "browse-reset-worker", "127.0.0.1", 9, "synthetic", "fixture");
        let stores = crate::stores::Stores::default();
        stores.browse.borrow_mut().seed_registered_table_for_test([sid, sid]);
        let client = crate::plex::client_for(sid).unwrap();
        stores.browse.borrow_mut().prepare_page_for_test(sid);
        let old = Arc::clone(&stores.browse.borrow().adapter);
        let (release, landed) = stores.browse.borrow_mut()
            .spawn_page_for_test(client, "retired-result");

        stores.browse.borrow_mut().run(BrowseCmd::Reset);
        let new = Arc::clone(&stores.browse.borrow().adapter);
        assert!(!Arc::ptr_eq(&old, &new), "reset must rotate worker transport identity");
        crate::browse::set_adapter_fetching_for_test(&new, true);
        release.send(()).unwrap();
        landed.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(crate::browse::adapter_has_page_for_test(&old));
        assert!(!crate::browse::adapter_has_page_for_test(&new),
            "a retired worker must not overwrite the replacement mailbox");
        assert!(crate::browse::adapter_fetching_for_test(&new),
            "a retired worker must not clear the replacement flight");

        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn roster_removal_rotates_transport_away_from_a_held_old_worker() {
        let _guard = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test(
            "browse-roster-retire", "127.0.0.1", 9, "synthetic", "fixture");
        let stores = crate::stores::Stores::default();
        stores.browse.borrow_mut().seed_registered_table_for_test([sid, sid]);
        let client = crate::plex::client_for(sid).unwrap();
        stores.browse.borrow_mut().prepare_page_for_test(sid);
        let old = Arc::clone(&stores.browse.borrow().adapter);
        let (release, landed) = stores.browse.borrow_mut()
            .spawn_page_for_test(client, "retired-by-roster");

        crate::plex::reset_servers_for_test();
        assert!(stores.browse.borrow_mut().pump().changed);
        let new = Arc::clone(&stores.browse.borrow().adapter);
        assert!(!Arc::ptr_eq(&old, &new),
            "source removal must retire the adapter before any replacement work is admitted");
        crate::browse::set_adapter_fetching_for_test(&new, true);
        release.send(()).unwrap();
        landed.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(crate::browse::adapter_has_page_for_test(&old));
        assert!(!crate::browse::adapter_has_page_for_test(&new));
        assert!(crate::browse::adapter_fetching_for_test(&new));

    }

    #[test]
    fn extracted_discovery_result_cannot_clear_the_post_reset_source_flight() {
        let _guard = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test(
            "browse-result-retire", "127.0.0.1", 9, "synthetic", "fixture");
        let stores = crate::stores::Stores::default();
        stores.browse.borrow_mut().seed_registered_table_for_test([sid, sid]);
        let client = crate::plex::client_for(sid).unwrap();
        stores.browse.borrow_mut().queue_discovery_for_test(
            client, client.token_gen(), true);
        let result = stores.browse.borrow_mut().take_discovery().unwrap();

        stores.browse.borrow_mut().run(BrowseCmd::Reset);
        let replacement = Arc::clone(&stores.browse.borrow().adapter);
        crate::browse::set_adapter_src_fetching_for_test(&replacement, true);
        let _ = stores.take_notices();
        let before = stores.browse.borrow().gen();
        let outcome = stores.browse.borrow_mut().apply_discovery(
            &result, &crate::plex::session::Session::default());
        assert!(!outcome.changed);
        assert_eq!(stores.browse.borrow().gen(), before);
        assert!(stores.take_notices().is_empty());
        assert!(crate::browse::adapter_src_fetching_for_test(&replacement),
            "a result extracted from the retired adapter must not release the new source flight");

        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn controlled_roster_addition_is_one_published_change_even_when_spawn_is_refused() {
        let _guard = crate::testlock::serial();
        // `sync_roster_owned` also watches the process-global session generation. Keep it settled
        // so the exact notice count below grades this roster addition, not an async session read.
        let _session = crate::plex::session::TempSession::new("controlled-roster-spawn-refused");
        crate::plex::reset_servers_for_test();
        let stores = crate::stores::Stores::default();
        let _ = stores.take_notices();
        let before_notice = stores.browse.borrow().gen();
        let before_sources = stores.browse.borrow().source_list_gen_for_test();
        crate::plex::register_for_test(
            "", "127.0.0.1", 9, "synthetic", "fixture");
        let mut launches = 0;
        stores.browse_controlled_discover(&mut |_| {
            launches += 1;
            false
        });
        assert_eq!(launches, 1);
        let owned_sources = stores.browse.borrow().source_list_gen_for_test();
        assert_ne!(owned_sources, before_sources);
        assert_eq!(stores.browse.borrow().gen(), before_notice + 1);
        assert_eq!(stores.take_notices(), [(StoreId::Browse, before_notice + 1)]);
        assert!(stores.take_notices().is_empty());
        let _ = crate::ui::idle::take_local_damage();

        let settled_notice = stores.browse.borrow().gen();
        let settled_sources = stores.browse.borrow().source_list_gen_for_test();
        let mut second_launches = 0;
        stores.browse_controlled_discover(&mut |_| {
            second_launches += 1;
            false
        });
        assert_eq!(second_launches, 0, "the refused source is still in retry backoff");
        assert_eq!(stores.browse.borrow().gen(), settled_notice);
        assert_eq!(stores.browse.borrow().source_list_gen_for_test(), settled_sources);
        assert!(stores.take_notices().is_empty());
        assert_eq!(crate::ui::idle::take_local_damage(), 0,
            "an empty-to-empty machine identity must not invalidate the settled frame");

        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn direct_controlled_discovery_mutates_only_the_addressed_owner() {
        let _guard = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let selected = crate::stores::Stores::default();
        let decoy = crate::stores::Stores::default();
        let selected_before = selected.browse.borrow().source_list_gen_for_test();
        let decoy_before = decoy.browse.borrow().source_list_gen_for_test();
        crate::plex::register_for_test(
            "", "127.0.0.1", 9, "synthetic", "fixture");

        let mut launches = 0;
        selected.browse_controlled_discover(&mut |_| {
            launches += 1;
            false
        });

        assert_eq!(launches, 1);
        assert_ne!(selected.browse.borrow().source_list_gen_for_test(), selected_before,
            "the explicitly addressed owner must admit the discovery request");
        assert_eq!(decoy.browse.borrow().source_list_gen_for_test(), decoy_before,
            "an unaddressed owner is not part of the direct contract");
        assert_eq!(selected.take_notices().into_iter()
            .filter(|(id, _)| *id == StoreId::Browse).count(), 1,
            "the direct call owes exactly one notice from the addressed Browse owner");
        assert!(decoy.take_notices().is_empty());

        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn isolated_current_page_failure_is_one_observable_change_and_notice() {
        let _guard = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test(
            "browse-page-failure", "127.0.0.1", 9, "synthetic", "fixture");
        crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unreachable);
        let stores = crate::stores::Stores::default();
        stores.browse.borrow_mut().seed_registered_table_for_test([sid, sid]);
        let client = crate::plex::client_for(sid).unwrap();
        stores.browse.borrow_mut().prepare_page_for_test(sid);
        let _ = stores.browse.borrow_mut().discover_pump();
        let _ = stores.take_notices();
        stores.browse.borrow_mut().queue_page_failure_for_test(client);
        let before = stores.browse.borrow().gen();

        let outcome = stores.browse.borrow_mut().pump();
        assert!(outcome.changed, "Loading to Failed is an observable listing publication");
        assert_eq!(stores.browse.borrow_mut().listing_snapshot().view().fetch(),
            crate::browse::SecFetch::Failed);
        assert_eq!(stores.browse.borrow().gen(), before + 1);
        assert_eq!(stores.take_notices(), [(StoreId::Browse, before + 1)]);
        assert!(stores.take_notices().is_empty());

        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn library_switch_events_count_only_new_committed_choices() {
        let _guard = crate::testlock::serial();
        let session = crate::plex::session::TempSession::new("library-switch-events");
        session.watching("u-library-switch-events");
        struct Cleanup;
        impl Drop for Cleanup {
            fn drop(&mut self) {
                crate::plex::reset_servers_for_test();
            }
        }
        let _cleanup = Cleanup;
        crate::plex::reset_servers_for_test();
        let own =
            crate::plex::register_for_test("switch-own", "127.0.0.1", 9, "synthetic", "fixture");
        let shared = crate::plex::register_for_test(
            "switch-shared",
            "127.0.0.1",
            10,
            "synthetic",
            "fixture",
        );
        let stores = crate::stores::Stores::default();
        stores.browse.borrow_mut().seed_registered_table_for_test([own, shared]);
        stores.browse_run(BrowseCmd::SetCur(0));
        let a = SectionAddress {
            epoch: stores.browse.borrow().table_epoch_for_test(),
            sid: own,
            section: 1,
        };
        let b = SectionAddress { sid: shared, ..a };
        assert_eq!(
            stores.browse.borrow().resolve_section_for_test(b.epoch, b.sid, b.section),
            Some(2)
        );
        let commit = |target, select, choice| {
            stores.browse_run(BrowseCmd::Addressed {
                target,
                work: LibraryWork::Commit {
                    select,
                    choice,
                    query: None,
                },
            })
        };
        let observe = |target, select, choice| {
            crate::diag::test_events::capture(|| commit(target, select, choice))
        };
        let switched = crate::diag::schema::DiagEvent::FeatureUsed {
            feature: crate::diag::schema::Feature::LibrarySwitch,
        };

        // Same-current includes the final A commit after a pending A→B→A was superseded.
        assert_eq!(observe(a, true, true), (true, vec![]));
        assert_eq!(
            observe(SectionAddress { section: 999, ..b }, true, true),
            (false, vec![])
        );
        assert_eq!(
            observe(
                SectionAddress {
                    epoch: b.epoch.wrapping_add(1),
                    ..b
                },
                true,
                true
            ),
            (false, vec![])
        );
        assert_eq!(
            observe(b, false, true),
            (false, vec![]),
            "foreign work without selection cannot count"
        );
        assert_eq!(stores.browse.borrow().current_for_test(), 0);
        assert_eq!(
            observe(b, true, false),
            (true, vec![]),
            "boot/repoint is not a viewer choice"
        );
        assert_eq!(stores.browse.borrow().current_for_test(), 2);
        assert_eq!(
            observe(b, true, true),
            (true, vec![]),
            "choosing the current library is quiet"
        );
        assert_eq!(observe(a, true, true), (true, vec![switched]));
        assert_eq!(stores.browse.borrow().current_for_test(), 0);
        assert_eq!(
            observe(a, true, true),
            (true, vec![]),
            "repeated delivery cannot count twice"
        );
        assert_eq!(
            observe(b, true, true),
            (true, vec![switched]),
            "a later real switch counts once"
        );
        let (accepted, events) = crate::diag::test_events::capture(|| {
            stores.browse_run(BrowseCmd::Addressed {
                target: b,
                work: LibraryWork::Commit {
                    select: false,
                    choice: false,
                    query: Some(QueryEdit::Unwatched(true)),
                },
            })
        });
        assert!(accepted);
        assert!(events.is_empty(), "query changes are not library switches");
    }
}
