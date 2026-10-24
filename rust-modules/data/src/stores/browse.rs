//! The Library table and its per-section listing, as a machine over `crate::browse`
//! (`docs/stores-as-machines.md`). The vocabulary is [`BrowseCmd`]; each [`BrowseStore`] owns its
//! main-thread state, worker adapter and notice while `crate::browse` retains the core
//! implementation.

use plx_plex::plex::{ClientKeys, ServerId};
use plx_machine::machine::{Cx, Effects, Handled, Machine};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use super::StoreEv;
#[cfg(test)]
use super::StoreId;

#[allow(unused_imports)] // Shared owner vocabulary; some members are feature/test specific.
pub use crate::browse::{
    Cursor, CursorAt, GenreEntry, LibraryType, SecFetch, SecKind, SortEntry, SourceState, SrcGroup, SrcRow,
};
#[allow(unused_imports)] // Shared owner vocabulary; some members are feature/test specific.
pub use crate::browse::section_hubs::{HubsId, HubsSnapshot, HubsView, Publication};
#[allow(unused_imports)] // Shared owner vocabulary; some members are feature/test specific.
pub use crate::browse::view::{
    DirectorySnapshot, DirectoryView, ListingId, ListingSnapshot, ListingView, SectionView,
};

/// Owner-aware first-run gate shared by boot and login. The retained directory is the granted
/// Browse publication for this Bridge.
pub mod onboard {
    pub fn asks(directory: super::DirectoryView<'_>) -> bool {
        asks_for_sources(directory.sources().len())
    }

    /// The gate for a roster of `sources`, judged for the profile that is watching NOW
    /// (`plex::pins::asks`): everyone is asked once, so the profile matters only for its recorded
    /// answer. Read off the same published profile key the record is filed under.
    pub fn asks_for_sources(sources: usize) -> bool {
        let session = plx_plex::plex::session::peek();
        let profile = plx_plex::plex::session::current_profile_key();
        plx_plex::plex::pins::asks(sources, session.pins_for(&profile))
    }
}

/// One frame's retained Browse publication. The directory is captured first because resolving
/// profile pins may repoint the current section; listing and section hubs are captured only after
/// that decision, so all three views describe one owner state.
#[derive(Clone)]
pub struct BrowsePublications {
    pub listing: ListingSnapshot,
    pub directory: DirectorySnapshot,
    pub section_hubs: HubsSnapshot,
}

/// Every mutation of the browse store a screen may ask for.
#[derive(Clone, Debug)]
pub enum BrowseCmd {
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
    #[cfg(any(test, feature = "test-support"))]
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
    /// One server's section shelves may have moved (a notification, never a press): only its
    /// sections are re-read, behind its failure ladder and spaced like the quiet Home refetch.
    HubsInvalidateServer { sid: ServerId },
    /// Items were created or removed on one server (a notification, never a press). The open
    /// library grid is never listed again while it is visible: it is marked stale, and the Library
    /// page re-queries it, keeping the reader's place, when a page covers it
    /// (`library_on_stack`: a Library page is on the navigation stack, visible or covered; when
    /// none is, nobody is looking and the re-query starts at once). Every other section of the
    /// server forgets its pages and fetches afresh on its next visit.
    ListingsChanged { sid: ServerId, library_on_stack: bool },
    /// The optimistic half of a view-state change (`viewstate::publish_view`), on the grid and the
    /// shelves: a watched edit reaches the listing and the section shelves, a deck removal the
    /// section shelves' Continue Watching.
    ApplyView { at: crate::viewstate::ViewAt, edit: crate::viewstate::ViewEdit },
}

/// The production owner of Browse's main-thread state, worker transport and notice generation.
pub struct BrowseStore {
    state: crate::browse::BrowseState,
    adapter: Arc<crate::browse::BrowseAdapter>,
    notice_gen: AtomicU32,
    notice_dirty: AtomicBool,
    /// Endpoint refreshes a pump had no room to emit in its step; the next pump sends them first.
    endpoint_carry: super::EndpointRefreshSet,
    /// The reads this store's steps have asked for and nobody has started yet: a section's hubs
    /// ([`super::StoreJob::BrowseHubs`]), a value list ([`super::StoreJob::BrowseDirectory`]) and a
    /// listing page ([`super::StoreJob::BrowsePage`]); taken by [`BrowseStore::take_jobs`].
    jobs: Vec<super::StoreJob>,
}

impl Default for BrowseStore {
    fn default() -> Self {
        Self {
            state: Default::default(),
            adapter: Arc::new(Default::default()),
            notice_gen: AtomicU32::new(0),
            notice_dirty: AtomicBool::new(false),
            endpoint_carry: Default::default(),
            jobs: Vec::new(),
        }
    }
}

impl BrowseStore {
    /// Join the owner's view-state publish order (`Stores::view_ledger`).
    pub(crate) fn share_ledger(&mut self, ledger: &crate::viewledger::ViewLedger) {
        self.state.ledger = ledger.clone();
    }

    fn bump(&self) -> u32 {
        self.notice_dirty.store(true, Ordering::Relaxed);
        self.notice_gen.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn gen(&self) -> u32 {
        self.notice_gen.load(Ordering::Relaxed)
    }

    pub fn take_notice(&self) -> Option<u32> {
        self.notice_dirty.swap(false, Ordering::Relaxed).then(|| self.gen())
    }

    /// Take the reads this owner's steps have asked for since the last take, in the order they were
    /// asked ([`super::StoreJob::BrowseHubs`], [`super::StoreJob::BrowseDirectory`],
    /// [`super::StoreJob::BrowsePage`]). The application's runner starts each, in the call that ran
    /// the step (`Bridge::start_store_jobs_of`).
    #[must_use]
    pub fn take_jobs(&mut self) -> Vec<super::StoreJob> {
        std::mem::take(&mut self.jobs)
    }

    /// Does the outbox hold a job nobody has taken yet?
    pub fn has_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// Run one command; `keys` is the registry's answer as of this call (the Bridge fills it
    /// immediately before, `refill_client_keys`), which the section-hubs or directory reads it may emit
    /// are decided from.
    pub fn run(&mut self, cmd: BrowseCmd, keys: &ClientKeys) -> bool {
        use crate::viewstate::ViewEdit;
        let change_sensitive = matches!(&cmd, BrowseCmd::Discovery(_) | BrowseCmd::Addressed {
            work: LibraryWork::SaveCursor { .. } | LibraryWork::Hubs { .. } | LibraryWork::HubHold { .. }
                | LibraryWork::Requery, ..
        }) || matches!(&cmd, BrowseCmd::ApplyView { edit: ViewEdit::Facts { .. } | ViewEdit::Offset { .. }, .. });
        let quiet = matches!(&cmd, BrowseCmd::Addressed {
            work: LibraryWork::Want { .. } | LibraryWork::Letters | LibraryWork::Genres | LibraryWork::HubPage { .. } | LibraryWork::HubPageCancel { .. }, ..
        });
        let roster_changed = if matches!(&cmd, BrowseCmd::RecheckShares) {
            self.sync_roster()
        } else {
            false
        };
        if matches!(&cmd, BrowseCmd::Reset) {
            self.adapter = Arc::new(Default::default());
        }
        let changed = self.state.run_owned(&self.adapter, cmd, keys, &mut self.jobs) || roster_changed;
        if !quiet && (!change_sensitive || changed) {
            self.bump();
        }
        changed
    }

    /// Run one command on a registry that holds no client (no section-hubs or directory read leaves): the
    /// command of a test about something else.
    #[cfg(any(test, feature = "test-support"))]
    pub fn run_for_test(&mut self, cmd: BrowseCmd) -> bool {
        self.run(cmd, &ClientKeys::default())
    }

    pub fn pump_with_gate(&mut self, gate: &plx_machine::landgate::Gate, keys: &ClientKeys) -> super::StoreOutcome {
        if !self.state.pump_needs_work(&self.adapter) {
            return Default::default();
        }
        let roster_changed = self.sync_roster();
        let source_gen = self.state.source_list_gen();
        let mut outcome = self.state.pump_owned_with_gate(&self.adapter, gate, keys, &mut self.jobs);
        outcome.changed |= roster_changed || source_gen != self.state.source_list_gen();
        if outcome.changed {
            self.bump();
        }
        outcome
    }
    /// [`BrowseStore::pump_with_gate`] on the fixture gate and a registry that holds no client (no
    /// section-hubs or page read leaves): the pump of a test about something else.
    #[cfg(any(test, feature = "test-support"))]
    pub fn pump(&mut self) -> super::StoreOutcome {
        self.pump_with_gate(plx_machine::landgate::fixture_gate(), &ClientKeys::default())
    }

    pub fn discover_pump_with_gate(&mut self, gate: &plx_machine::landgate::Gate) -> super::StoreOutcome {
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
    #[cfg(any(test, feature = "test-support"))]
    pub fn discover_pump(&mut self) -> super::StoreOutcome {
        self.discover_pump_with_gate(plx_machine::landgate::fixture_gate())
    }

    /// The items the addressed Library page holds (see [`BrowseState::held_keys`]).
    pub fn held_keys(&self, target: SectionAddress) -> super::HeldKeys {
        self.state.held_keys(target)
    }

    /// Is any shelf of the addressed section committed (see [`BrowseState::hubs_committed`])?
    pub fn hubs_committed(&self, target: SectionAddress) -> bool {
        self.state.hubs_committed(target)
    }

    /// Is a membership re-query's first page still out with no Library page on top to pump it? The
    /// application pumps this store itself while it holds (`Bridge::reconcile_pump`).
    pub fn requery_owed(&self) -> bool {
        self.state.requery_owed()
    }

    pub fn listing_snapshot(&mut self) -> ListingSnapshot {
        self.state.listing_snapshot()
    }

    pub fn capture_directory(&mut self, snapshot: &mut DirectorySnapshot) {
        snapshot.capture_from(&mut self.state);
    }

    pub fn hubs_snapshot(&mut self) -> HubsSnapshot {
        self.state.hubs_snapshot(self.state.cur())
    }

    /// TEST ONLY: is the section hubs' one shared claim up: a read is out (running, parked, or
    /// answered and not yet taken by the pump)?
    #[cfg(any(test, feature = "test-support"))]
    pub fn hubs_read_out_for_test(&self) -> bool {
        crate::browse::hubs_claim_up_for_test(&self.adapter)
    }

    /// TEST ONLY: has a section-hubs worker answered, with the answer still waiting for the pump?
    #[cfg(any(test, feature = "test-support"))]
    pub fn hubs_answer_waiting_for_test(&self) -> bool {
        crate::browse::hubs_answer_waiting_for_test(&self.adapter)
    }

    /// TEST ONLY: is the listing page's claim up: a page read is out (running, parked, or answered
    /// and not yet taken by the pump)?
    #[cfg(any(test, feature = "test-support"))]
    pub fn page_read_out_for_test(&self) -> bool {
        crate::browse::adapter_fetching_for_test(&self.adapter)
    }

    /// TEST ONLY: has a page worker answered, the answer still waiting for the pump?
    #[cfg(any(test, feature = "test-support"))]
    pub fn page_answer_waiting_for_test(&self) -> bool {
        crate::browse::adapter_has_page_for_test(&self.adapter)
    }

    /// TEST ONLY: is a directory (genres or letters) claim up?
    #[cfg(any(test, feature = "test-support"))]
    pub fn directory_read_out_for_test(&self) -> bool {
        crate::browse::directory_claim_up_for_test(&self.adapter)
    }

    /// TEST ONLY: has a directory worker answered, the answer still waiting for the pump?
    #[cfg(any(test, feature = "test-support"))]
    pub fn directory_answer_waiting_for_test(&self) -> bool {
        crate::browse::directory_answered_for_test(&self.adapter)
    }

    pub fn take_discovery(&mut self) -> Option<crate::browse::record::Result> {
        crate::browse::record::take_from(&self.adapter)
    }

    /// Does a discovery request this owner launched still owe its answer? The claim the landing
    /// gate's dump mode waits on at the Bridge's controlled-Home discovery take.
    pub fn discovery_owed(&self) -> bool {
        crate::browse::discovery_owed(&self.adapter)
    }

    fn sync_roster(&mut self) -> bool {
        let sync = self.state.sync_roster_owned();
        if sync.retire_adapter {
            self.adapter = Arc::new(Default::default());
        }
        sync.changed
    }

    pub fn controlled_discover(
        &mut self,
        launch: &mut dyn FnMut(crate::browse::DiscoveryRequest) -> bool,
    ) {
        let changed = self.sync_roster();
        self.state.controlled_discover_owned(&self.adapter, launch);
        if changed {
            self.bump();
        }
    }

    pub fn apply_discovery(
        &mut self,
        result: &crate::browse::record::Result,
        preferences: &plx_plex::plex::session::Session,
    ) -> super::StoreOutcome {
        let outcome = crate::browse::record::apply_to(
            &mut self.state, &self.adapter, result, Some(preferences));
        if outcome.changed {
            self.bump();
        }
        outcome
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn spawn_page_for_test(
        &mut self,
        client: &'static plx_plex::plex::Client,
        title: &str,
    ) -> (std::sync::mpsc::SyncSender<()>, std::sync::mpsc::Receiver<()>) {
        crate::browse::spawn_owned_page_for_test(&self.state, &self.adapter, client, title)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_items_for_test(&mut self, count: usize) {
        crate::browse::seed_items_for_owner_test(&mut self.state, count);
    }

    /// [`seed_items_for_test`](Self::seed_items_for_test) with `count` collections: the listing a
    /// section answers on `type=18`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_collections_for_test(&mut self, count: usize) {
        crate::browse::seed_collections_for_owner_test(&mut self.state, count);
    }

    /// [`seed_items_for_test`](Self::seed_items_for_test) with `count` numbered episodes: the
    /// listing a show section answers on `type=4`, the library type already `Episodes`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_episodes_for_test(&mut self, count: usize) {
        crate::browse::seed_episodes_for_owner_test(&mut self.state, count);
    }

    /// **Hold the listing's fetch**: the single-flight flag stays taken, so the page the section is
    /// waiting for is never asked for and nothing lands. A host test's server is a closed port, whose
    /// fetch fails at once; a page that wants to be read while its listing is LOADING needs the
    /// listing to stay so. A seeded listing ([`seed_items_for_test`](Self::seed_items_for_test)) still
    /// lands over the hold.
    #[cfg(any(test, feature = "test-support"))]
    pub fn hold_listing_for_test(&mut self) {
        crate::browse::hold_listing_for_owner_test(&self.adapter);
    }

    /// [`seed_collections_for_test`](Self::seed_collections_for_test) with each collection's
    /// `(title, thumb)` stated.
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_named_collections_for_test(&mut self, rows: &[(&str, &str)]) {
        crate::browse::seed_named_collections_for_owner_test(&mut self.state, rows);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_sources_for_test(&mut self, count: usize, reachable: bool) {
        crate::browse::seed_sources_for_owner_test(&mut self.state, count, reachable);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_pins_for_test(&mut self, pinned: &[bool]) {
        crate::browse::seed_pins_for_owner_test(&mut self.state, pinned);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_pinned_for_test(&mut self, index: usize, on: bool) {
        crate::browse::set_pinned_for_owner_test(&mut self.state, index, on);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn land_pin_for_test(&mut self, pinned: bool) {
        crate::browse::land_pin_for_owner_test(&mut self.state, pinned);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_two_source_table_for_test(&mut self) {
        crate::browse::seed_two_source_table_for_owner_test(&mut self.state);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_registered_table_for_test(&mut self, sids: [ServerId; 2]) {
        crate::browse::seed_registered_table_for_owner_test(&mut self.state, sids);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn append_section_for_test(
        &mut self,
        source: usize,
        key: i64,
        title: &str,
        kind: SecKind,
    ) {
        crate::browse::append_section_for_owner_test(&mut self.state, source, key, title, kind);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_letter_counts_for_test(&mut self, letters: &[(&str, i64)]) {
        crate::browse::seed_letter_counts_for_owner_test(&mut self.state, letters);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn owe_letters_for_test(&mut self) {
        crate::browse::owe_letters_for_owner_test(&mut self.state);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_query_choices_for_test(
        &mut self,
        sorts: Vec<SortEntry>,
        genres: Vec<GenreEntry>,
    ) {
        crate::browse::seed_query_choices_for_owner_test(&mut self.state, sorts, genres);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_shelves_for_test(
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

    /// A landing held behind the publication gate, as the worker leaves one: staged, not drawn.
    #[cfg(any(test, feature = "test-support"))]
    pub fn stage_shelves_for_test(&mut self, section: usize) {
        crate::browse::section_hubs::stage_shelves_for_owner_test(&mut self.state, section);
    }

    /// Edit every card of section `section`'s published shelves
    /// ([`seed_named_shelves_for_test`](Self::seed_named_shelves_for_test)'s cards carry only a key
    /// and a title).
    #[cfg(any(test, feature = "test-support"))]
    pub fn edit_shelf_items_for_test(&mut self, section: usize, edit: &dyn Fn(&mut crate::pms::PmsMovie)) {
        crate::browse::section_hubs::edit_shelf_items_for_owner_test(&mut self.state, section, edit);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_named_shelves_for_test(
        &mut self,
        section: usize,
        rows: &[(&str, &str, &str)],
        per_row: usize,
    ) {
        crate::browse::section_hubs::seed_named_shelves_for_owner_test(
            &mut self.state,
            section,
            rows,
            per_row,
        );
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn land_held_rows_for_test(&mut self, section: usize) {
        crate::browse::section_hubs::land_held_rows_for_owner_test(&mut self.state, section);
    }

    /// The rows of `section` the ring gave up (placeholders) and the band the store holds.
    #[cfg(any(test, feature = "test-support"))]
    pub fn released_rows_for_test(&self, section: usize) -> (Vec<usize>, (usize, usize)) {
        crate::browse::section_hubs::released_rows_for_owner_test(&self.state, section)
    }

    /// One row of `section` takes a fresh window of `count` cards through the store's own landing
    /// (`section_hubs::land_row`).
    #[cfg(any(test, feature = "test-support"))]
    pub fn land_row_for_test(&mut self, section: usize, row: usize, count: usize) -> bool {
        crate::browse::section_hubs::land_row_for_owner_test(&mut self.state, section, row, count)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_paging_for_test(&mut self, section: usize, total: usize) {
        crate::browse::section_hubs::seed_paging_for_owner_test(&mut self.state, section, total);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_first_shelf_items_for_test(&mut self, section: usize, rks: &[String]) {
        crate::browse::section_hubs::seed_first_shelf_items_for_owner_test(
            &mut self.state,
            section,
            rks,
        );
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_landscape_for_test(&mut self, section: usize, show: &str) {
        crate::browse::section_hubs::seed_landscape_for_owner_test(
            &mut self.state,
            section,
            show,
        );
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn pinned_for_test(&self, index: usize) -> bool {
        self.state.pinned_for_test(index)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn current_for_test(&self) -> usize {
        self.state.cur()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn table_epoch_for_test(&self) -> u32 {
        self.state.table_epoch()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn resolve_section_for_test(
        &self,
        epoch: u32,
        sid: ServerId,
        key: i64,
    ) -> Option<usize> {
        self.state.resolve_section(epoch, sid, key)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn prepare_discovery_replay_for_test(&mut self, sid: ServerId, epoch: u32) {
        self.state.prepare_discovery_replay_for_test(&self.adapter, sid, epoch);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn discovery_policy_for_test(&self) -> (bool, u32, bool) {
        self.state.discovery_policy_for_test(&self.adapter)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn queue_discovery_for_test(
        &mut self,
        client: &'static plx_plex::plex::Client,
        token_gen: u32,
        ok: bool,
    ) {
        crate::browse::queue_discovery_for_owner_test(
            &mut self.state, &self.adapter, client, token_gen, ok);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn prepare_page_for_test(&mut self, sid: ServerId) {
        crate::browse::prepare_page_for_owner_test(&mut self.state, sid);
    }

    // Dev-only: used only by `app/bridge.rs`'s `production_bridges_do_not_share_browse_state_or_landings`,
    // gated under `devtriggers` (see that test's own comment).
    #[cfg(all(any(test, feature = "test-support"), feature = "devtriggers"))]
    pub fn has_page_result_for_test(&self) -> bool {
        crate::browse::adapter_has_page_for_test(&self.adapter)
    }

    #[cfg(test)]
    fn queue_genre_for_test(&mut self, client: &'static plx_plex::plex::Client) {
        crate::browse::queue_genre_for_owner_test(&mut self.state, &self.adapter, client);
    }

    #[cfg(test)]
    fn queue_page_failure_for_test(&mut self, client: &'static plx_plex::plex::Client) {
        crate::browse::queue_page_failure_for_owner_test(&mut self.state, &self.adapter, client);
    }

    #[cfg(test)]
    fn source_list_gen_for_test(&self) -> u32 {
        self.state.source_list_gen()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionAddress {
    pub epoch: u32,
    pub sid: ServerId,
    pub section: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryEdit {
    Sort { key: String, desc: bool },
    Unwatched(bool),
    Genre(Option<String>),
    LibraryType(LibraryType),
}

#[derive(Clone, Debug)]
pub enum LibraryWork {
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
        /// The focused slot, when focus is on the grid: its page stays loaded (`Keep::focus`)
        /// however far the wanted window has moved from it.
        focus: Option<usize>,
    },
    Letters,
    Genres,
    Hubs {
        may_publish: bool,
    },
    /// Move one section hub row's window a page toward its end or its start; the row is named by
    /// its hub identifier and key. `seen` is the revision of the hub view the ask was computed
    /// from (`HubsView::revision`): a frame captures its views before its landings are delivered,
    /// so the ask can come from a window the store has since replaced, and such an ask is refused
    /// (the publication it missed re-arms every row of the page, changed or not, and the row asks
    /// again on its next tick from the window that stands if the ask is still due).
    HubPage {
        id: String,
        key: String,
        before: bool,
        seen: u64,
    },
    /// Withdraw the page ask for row `id`: focus left the edge it was sent from, and a landing now
    /// would slide the window from under it. A read already out is discarded when it lands; an ask
    /// that has landed, or never was, makes this a no-op.
    HubPageCancel {
        id: String,
        key: String,
    },
    /// The rows (hub order, inclusive) the page holds cards for. The store keeps those and gives
    /// the cards of the rest up to descriptors; sent when the range changes.
    HubHold {
        lo: usize,
        hi: usize,
    },
    Retry,
    /// Re-fetch the current section's query if a read-back made its membership stale (the Unwatched
    /// switch, ordering by last viewed). The Library page asks once a PAGE has been pushed over it,
    /// never while it is visible or under a modal (a compact menu): the reader's ground does not
    /// move under the cursor, and the listing is fresh when the page is shown again. It is the
    /// same query refreshed, so [`crate::browse::view::ListingId::query`] does not move and the
    /// page keeps its scroll and focus.
    Requery,
}

impl<H: super::StoreEffectHost> Machine<H> for BrowseStore {
    type Ev = StoreEv<BrowseCmd>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            // The registry is read by the host, never in a step (`plx_plex::plex::ClientKeys`), and
            // a machine step is handed no keys: this arm cannot decide a section-hubs or directory read. It is
            // not how the application drives Browse, which calls `Stores::browse_run` and
            // `BrowseStore::pump_with_gate` with the keys it filled and starts the jobs they leave
            // (`Bridge::browse_run`); no sender of `StoreEv::Pump` exists either.
            StoreEv::Cmd(c) => {
                self.run(c.clone(), &ClientKeys::default());
            }
            StoreEv::Pump { .. } => {
                let mut due = std::mem::take(&mut self.endpoint_carry);
                due.merge(self.pump_with_gate(&plx_machine::landgate::Gate::default(), &ClientKeys::default())
                    .endpoints);
                self.endpoint_carry = due.emit(fx);
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
        let _guard = plx_base::testlock::serial();
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

        assert!(stores.browse_run_for_test(BrowseCmd::SetCur(2)),
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
        let _guard = plx_base::testlock::serial();
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
        first.browse_run_for_test(BrowseCmd::Reset);
        assert_eq!(first.take_notices(), [(StoreId::Browse, 1)]);
        assert!(second.take_notices().is_empty());
    }

    #[test]
    fn snapshots_and_an_idle_pump_are_quiet() {
        let _guard = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
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

    /// `held_keys` is the Library page's hold: the shelf rows inside the hold window and the grid
    /// rows inside the wanted window of the current section, deduplicated; a stale address holds nothing.
    #[test]
    fn held_keys_match_hold_window() {
        let _guard = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let mut browse = BrowseStore::default();
        browse.seed_two_source_table_for_test();
        browse.seed_items_for_test(30);
        browse.seed_shelves_for_test(0, &["a", "b"], 2);
        let id = browse.hubs_snapshot().view().id().unwrap();
        let target = SectionAddress { epoch: id.epoch, sid: id.sid, section: id.section };
        let rks = |keys: &super::super::HeldKeys| keys.iter().map(|(_, k)| k.clone()).collect::<Vec<_>>();
        // No grid window yet: only the shelves (both rows sit inside the default hold).
        let shelf_only = browse.held_keys(target);
        assert_eq!(shelf_only.len(), 4, "two shelves of two cards: {:?}", rks(&shelf_only));
        browse.run_for_test(BrowseCmd::Addressed { target, work: LibraryWork::Want { lo: 3, hi: 6, focus: None } });
        let held = browse.held_keys(target);
        let grid: Vec<_> = rks(&held).into_iter().filter(|k| k.parse::<usize>().is_ok_and(|n| (4..=6).contains(&n))).collect();
        assert_eq!(grid, ["4", "5", "6"], "the grid window is items 3..6: {:?}", rks(&held));
        assert!(shelf_only.iter().all(|k| held.contains(k)), "shelf keys stay held alongside the grid");
        assert_eq!(held.len(), shelf_only.len() + 3, "the shelves plus the three grid items: {:?}", rks(&held));
        assert!(!rks(&held).contains(&"7".to_string()), "past the window");
        assert!(browse.held_keys(SectionAddress { epoch: target.epoch + 1, ..target }).is_empty(), "a stale address holds nothing");
    }

    /// `hubs_committed` is the drawn set: nothing before the first publication, and still nothing
    /// while a landing is only staged behind the gate; a stale address has none.
    #[test]
    fn hubs_committed_reads_the_drawn_set_not_the_staged_one() {
        let _guard = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let mut browse = BrowseStore::default();
        browse.seed_two_source_table_for_test();
        let id = browse.hubs_snapshot().view().id().unwrap();
        let target = SectionAddress { epoch: id.epoch, sid: id.sid, section: id.section };
        assert!(!browse.hubs_committed(target), "nothing has been published");
        crate::browse::section_hubs::stage_shelves_for_owner_test(&mut browse.state, 0);
        assert!(!browse.hubs_committed(target), "a staged set is not drawn");
        assert!(browse.run_for_test(BrowseCmd::Addressed { target, work: LibraryWork::Hubs { may_publish: true } }));
        assert!(browse.hubs_committed(target), "the committed set is drawn");
        assert!(!browse.hubs_committed(SectionAddress { epoch: target.epoch + 1, ..target }), "a stale address names no section");
    }

    #[test]
    fn hubs_housekeeping_notices_only_a_new_publication() {
        let _guard = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let mut browse = BrowseStore::default();
        browse.seed_two_source_table_for_test();
        browse.seed_shelves_for_test(0, &["published"], 1);
        let id = browse.hubs_snapshot().view().id().unwrap();
        let target = SectionAddress { epoch: id.epoch, sid: id.sid, section: id.section };
        browse.take_notice();
        let before = browse.gen();
        let request = |target, may_publish| BrowseCmd::Addressed {
            target, work: LibraryWork::Hubs { may_publish },
        };
        for may_publish in [false, true] {
            assert!(!browse.run_for_test(request(target, may_publish)));
            assert!(!browse.run_for_test(request(SectionAddress { epoch: target.epoch + 1, ..target }, may_publish)));
        }
        assert_eq!(browse.gen(), before, "unchanged and stale Hubs work must stay quiet");
        assert_eq!(browse.take_notice(), None);

        crate::browse::section_hubs::stage_shelves_for_owner_test(&mut browse.state, 0);
        assert!(!browse.run_for_test(request(target, false)));
        assert_eq!(browse.gen(), before, "a held staged publication is still unchanged");
        assert!(browse.run_for_test(request(target, true)));
        assert_eq!(browse.gen(), before + 1);
        assert_eq!(browse.take_notice(), Some(before + 1), "a committed shelf set still owes its notice");
        assert!(!browse.run_for_test(request(target, true)));
        assert_eq!(browse.take_notice(), None, "the same publication is noticed once");
        assert_eq!(browse.gen(), before + 1);
    }

    #[test]
    fn an_owned_discovery_landing_is_consumed_once_and_noticed_once() {
        let _guard = plx_base::testlock::serial();
        let session = plx_plex::plex::session::TempSession::new("browse-preowner");
        session.watching("u-browse-preowner");
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test(
            "browse-preowner", "127.0.0.1", 9, "synthetic", "fixture");
        let client = plx_plex::plex::client_for(sid).unwrap();
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
        plx_plex::plex::reset_servers_for_test();
    }

    #[test]
    fn controlled_discovery_apply_and_directory_landing_each_bump_exactly_once() {
        let _guard = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test(
            "browse-owned-landings", "127.0.0.1", 9, "synthetic", "fixture");
        let stores = crate::stores::Stores::default();
        stores.browse.borrow_mut().seed_registered_table_for_test([sid, sid]);
        let _ = stores.take_notices();
        let client = plx_plex::plex::client_for(sid).unwrap();

        stores.browse.borrow_mut().queue_discovery_for_test(
            client, client.token_gen(), true);
        let result = stores.browse.borrow_mut().take_discovery().unwrap();
        let before_discovery = stores.browse.borrow().gen();
        let _ = stores.browse.borrow_mut().apply_discovery(
            &result, &plx_plex::plex::session::Session::default());
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

        plx_plex::plex::reset_servers_for_test();
    }

    #[test]
    fn reset_rotates_the_adapter_away_from_a_late_old_worker() {
        let _guard = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test(
            "browse-reset-worker", "127.0.0.1", 9, "synthetic", "fixture");
        let stores = crate::stores::Stores::default();
        stores.browse.borrow_mut().seed_registered_table_for_test([sid, sid]);
        let client = plx_plex::plex::client_for(sid).unwrap();
        stores.browse.borrow_mut().prepare_page_for_test(sid);
        let old = Arc::clone(&stores.browse.borrow().adapter);
        let (release, landed) = stores.browse.borrow_mut()
            .spawn_page_for_test(client, "retired-result");

        stores.browse.borrow_mut().run_for_test(BrowseCmd::Reset);
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

        plx_plex::plex::reset_servers_for_test();
    }

    #[test]
    fn roster_removal_rotates_transport_away_from_a_held_old_worker() {
        let _guard = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test(
            "browse-roster-retire", "127.0.0.1", 9, "synthetic", "fixture");
        let stores = crate::stores::Stores::default();
        stores.browse.borrow_mut().seed_registered_table_for_test([sid, sid]);
        let client = plx_plex::plex::client_for(sid).unwrap();
        stores.browse.borrow_mut().prepare_page_for_test(sid);
        let old = Arc::clone(&stores.browse.borrow().adapter);
        let (release, landed) = stores.browse.borrow_mut()
            .spawn_page_for_test(client, "retired-by-roster");

        plx_plex::plex::reset_servers_for_test();
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
        let _guard = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test(
            "browse-result-retire", "127.0.0.1", 9, "synthetic", "fixture");
        let stores = crate::stores::Stores::default();
        stores.browse.borrow_mut().seed_registered_table_for_test([sid, sid]);
        let client = plx_plex::plex::client_for(sid).unwrap();
        stores.browse.borrow_mut().queue_discovery_for_test(
            client, client.token_gen(), true);
        let result = stores.browse.borrow_mut().take_discovery().unwrap();

        stores.browse.borrow_mut().run_for_test(BrowseCmd::Reset);
        let replacement = Arc::clone(&stores.browse.borrow().adapter);
        crate::browse::set_adapter_src_fetching_for_test(&replacement, true);
        let _ = stores.take_notices();
        let before = stores.browse.borrow().gen();
        let outcome = stores.browse.borrow_mut().apply_discovery(
            &result, &plx_plex::plex::session::Session::default());
        assert!(!outcome.changed);
        assert_eq!(stores.browse.borrow().gen(), before);
        assert!(stores.take_notices().is_empty());
        assert!(crate::browse::adapter_src_fetching_for_test(&replacement),
            "a result extracted from the retired adapter must not release the new source flight");

        plx_plex::plex::reset_servers_for_test();
    }

    #[test]
    fn controlled_roster_addition_is_one_published_change_even_when_spawn_is_refused() {
        let _guard = plx_base::testlock::serial();
        // `sync_roster_owned` also watches the process-global session generation. Keep it settled
        // so the exact notice count below grades this roster addition, not an async session read.
        let _session = plx_plex::plex::session::TempSession::new("controlled-roster-spawn-refused");
        plx_plex::plex::reset_servers_for_test();
        let stores = crate::stores::Stores::default();
        let _ = stores.take_notices();
        let before_notice = stores.browse.borrow().gen();
        let before_sources = stores.browse.borrow().source_list_gen_for_test();
        plx_plex::plex::register_for_test(
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
        let _ = plx_machine::idle::take_local_damage();

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
        assert_eq!(plx_machine::idle::take_local_damage(), 0,
            "an empty-to-empty machine identity must not invalidate the settled frame");

        plx_plex::plex::reset_servers_for_test();
    }

    #[test]
    fn direct_controlled_discovery_mutates_only_the_addressed_owner() {
        let _guard = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let selected = crate::stores::Stores::default();
        let decoy = crate::stores::Stores::default();
        let selected_before = selected.browse.borrow().source_list_gen_for_test();
        let decoy_before = decoy.browse.borrow().source_list_gen_for_test();
        plx_plex::plex::register_for_test(
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

        plx_plex::plex::reset_servers_for_test();
    }

    #[test]
    fn isolated_current_page_failure_is_one_observable_change_and_notice() {
        let _guard = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test(
            "browse-page-failure", "127.0.0.1", 9, "synthetic", "fixture");
        plx_plex::plex::publish_probe_result(sid, plx_plex::plex::probe::Outcome::Unreachable);
        let stores = crate::stores::Stores::default();
        stores.browse.borrow_mut().seed_registered_table_for_test([sid, sid]);
        let client = plx_plex::plex::client_for(sid).unwrap();
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

        plx_plex::plex::reset_servers_for_test();
    }

    #[test]
    fn library_switch_events_count_only_new_committed_choices() {
        let _guard = plx_base::testlock::serial();
        let session = plx_plex::plex::session::TempSession::new("library-switch-events");
        session.watching("u-library-switch-events");
        struct Cleanup;
        impl Drop for Cleanup {
            fn drop(&mut self) {
                plx_plex::plex::reset_servers_for_test();
            }
        }
        let _cleanup = Cleanup;
        plx_plex::plex::reset_servers_for_test();
        let own =
            plx_plex::plex::register_for_test("switch-own", "127.0.0.1", 9, "synthetic", "fixture");
        let shared = plx_plex::plex::register_for_test(
            "switch-shared",
            "127.0.0.1",
            10,
            "synthetic",
            "fixture",
        );
        let stores = crate::stores::Stores::default();
        stores.browse.borrow_mut().seed_registered_table_for_test([own, shared]);
        stores.browse_run_for_test(BrowseCmd::SetCur(0));
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
            stores.browse_run_for_test(BrowseCmd::Addressed {
                target,
                work: LibraryWork::Commit {
                    select,
                    choice,
                    query: None,
                },
            })
        };
        let observe = |target, select, choice| {
            plx_telemetry::diag::test_events::capture(|| commit(target, select, choice))
        };
        let switched = plx_telemetry::diag::schema::DiagEvent::FeatureUsed {
            feature: plx_telemetry::diag::schema::Feature::LibrarySwitch,
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
        let (accepted, events) = plx_telemetry::diag::test_events::capture(|| {
            stores.browse_run_for_test(BrowseCmd::Addressed {
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
