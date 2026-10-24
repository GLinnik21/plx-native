//! Home's hub catalog store boundary over `crate::pms` (`docs/stores-as-machines.md`). Each
//! production `Bridge` owns one [`HubsStore`]; no free selector can connect two Bridges.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum HubsCmd {
    /// A refetch is owed (a view-state write landed, a profile settled).
    RefetchHubs,
    /// Move one row's window a page. `seen` is the catalog generation of the view the ask was
    /// computed from (`HubsView::generation`): a frame captures its views before that frame's
    /// landings are delivered, so a tick can ask from a window the store has already moved, and
    /// such an ask is refused. The publication it missed re-arms the row, which asks again from
    /// the window that stands if the ask is still due.
    Page { sid: plx_plex::plex::ServerId, id: String, key: String, before: bool, seen: u32 },
    CancelPage { sid: plx_plex::plex::ServerId, id: String, key: String },
    /// The screen holds cards for rows `lo..=hi` (catalog order); every other row keeps only its
    /// descriptor. Sent when the range changes, from the tick that knows which rows are on screen.
    Hold { lo: usize, hi: usize },
    /// The read-out's Retry: clear the back-off and ask again.
    Retry,
    /// The profile/account switch.
    Reset,
    /// The optimistic half of a view-state change on the hub catalog (`viewstate::publish_view`).
    ApplyView { at: crate::viewstate::ViewAt, edit: crate::viewstate::ViewEdit },
    /// Read source `sid` again WITHOUT publishing it: the answer is held on the source
    /// (`pms::Src::staged`) until [`HubsCmd::Publish`] says it may land. Unlike `RefetchHubs` it
    /// never supersedes a flight, touches no other source and moves no generation, so a read in
    /// flight for the person is never dropped by it. The data layer enforces its own preconditions:
    /// it is not made while the source is loading or not Ready, or while a whole-source (prompted)
    /// read of any source is out, which would publish what it would have staged. An ask that cannot
    /// be made yet (the source has a page read out, or the spacing after its last quiet read has
    /// not run out) is kept and made by a later tick; a failed quiet read backs off and asks no
    /// rediscovery. A sender need not gate it.
    RefetchSourceQuiet { sid: plx_plex::plex::ServerId },
    /// Sent every frame by the application: may what is staged land now? Nothing staged does
    /// nothing. `home_top_visible` is "Home is the page the person is looking at"; `focused_hub`
    /// is the catalog row (the index `HubsView::hub` takes) the focus is in, if it is in a row;
    /// `press_live` is "a press is armed", which holds the Continue Watching rows only. While Home is hidden the staged answer lands at once.
    /// While it is visible it lands only if nothing the person can see or is pointing at moves:
    /// no row added, removed or reordered, and every row that changed is outside the held window
    /// or is Continue Watching with the focus elsewhere.
    Publish { home_top_visible: bool, focused_hub: Option<usize>, press_live: bool },
}

impl HubsCmd {
    /// Page requests move a window, not the catalog, so they do not bump the notice generation.
    pub fn is_paging(&self) -> bool { matches!(self, HubsCmd::Page { .. } | HubsCmd::CancelPage { .. }) }

    /// Commands the application sends on no one's behalf (a quiet read, the per-frame publish
    /// check): they raise the notice only through what they changed, never because they ran.
    pub fn is_background(&self) -> bool {
        matches!(self, HubsCmd::RefetchSourceQuiet { .. } | HubsCmd::Publish { .. })
    }

    /// Does this command raise the notice whatever it changed? Paging does not (see above), and a
    /// report of what the server holds (`Facts`, `Offset`) raises it only through the rows it
    /// altered: one every row already agrees with relays nothing out.
    fn always_notifies(&self) -> bool {
        !self.is_paging() && !self.is_background() && !matches!(self, HubsCmd::ApplyView {
            edit: crate::viewstate::ViewEdit::Facts { .. } | crate::viewstate::ViewEdit::Offset { .. }, ..
        })
    }
}

pub use crate::pms::Landing as HubsResult;

/// One Hubs owner: logical state, the worker adapter all current fetches capture, and notice.
pub struct HubsStore {
    state: crate::pms::PmsState,
    adapter: Arc<crate::pms::PmsAdapter>,
    notice_gen: AtomicU32,
    notice_dirty: AtomicBool,
}

impl Default for HubsStore {
    fn default() -> Self {
        Self {
            state: Default::default(),
            adapter: Arc::new(Default::default()),
            notice_gen: AtomicU32::new(0),
            notice_dirty: AtomicBool::new(false),
        }
    }
}

impl HubsStore {
    /// Join the owner's view-state publish order (`Stores::view_ledger`).
    pub(crate) fn share_ledger(&mut self, ledger: &crate::viewledger::ViewLedger) {
        self.state.share_ledger(ledger);
    }

    /// Retire the mailbox at the store command boundary, including when recording controls how
    /// subsequent work is launched. Every command entry point shares this identity-reset rule.
    fn prepare_command(&mut self, cmd: Option<&HubsCmd>) {
        if matches!(cmd, Some(HubsCmd::Reset)) {
            self.adapter = Arc::new(Default::default());
        }
    }

    /// Seed a fresh owner from restored boot initial conditions (`pms::initial::Initial::restore`),
    /// before any `Bridge`/`Stores` exists.
    ///
    /// Takes the owner's [`crate::viewledger::ViewLedger`] (`Stores::view_ledger`) so a store built
    /// this way cannot be left outside the view-state publish order: there is no second step to forget.
    pub fn from_parts(mut state: crate::pms::PmsState, adapter: crate::pms::PmsAdapter,
        ledger: &crate::viewledger::ViewLedger) -> Self {
        state.share_ledger(ledger);
        Self { state, adapter: Arc::new(adapter), notice_gen: AtomicU32::new(0), notice_dirty: AtomicBool::new(false) }
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

    pub fn snapshot(&self) -> crate::pms::HubsSnapshot {
        crate::pms::hubs_snapshot(&self.state)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn state(&self) -> &crate::pms::PmsState { &self.state }

    #[cfg(any(test, feature = "test-support"))]
    pub fn state_mut(&mut self) -> &mut crate::pms::PmsState { &mut self.state }

    #[cfg(any(test, feature = "test-support"))]
    pub fn adapter_for_test(&self) -> Arc<crate::pms::PmsAdapter> { Arc::clone(&self.adapter) }

    /// Test hook: put this owner in a known place — one source, `items` rows in one shelf. Mirrors
    /// `crate::pms::seed_for_test`, threaded onto this store's own owned `(state, adapter)` pair
    /// rather than the deleted process-wide catalog.
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_for_test(&mut self, items: usize, hub_state: crate::pms::HubState) {
        let adapter = self.adapter_for_test();
        crate::pms::seed_for_test(&mut self.state, &adapter, items, hub_state);
    }

    /// Test hook: a directory-scoped Hubs source — see `crate::pms::seed_for_directory_test`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_for_directory_test(
        &mut self,
        sid: plx_plex::plex::ServerId,
        items: usize,
        hub_state: crate::pms::HubState,
        directory: crate::stores::browse::DirectoryView<'_>,
    ) {
        let adapter = self.adapter_for_test();
        crate::pms::seed_for_directory_test(&mut self.state, &adapter, sid, items, hub_state, directory);
    }

    /// Test hook: the two-library Home fixture — see `crate::pms::seed_two_library_home_for_test`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_two_library_home_for_test(
        &mut self,
        sid: plx_plex::plex::ServerId,
        directory: crate::stores::browse::DirectoryView<'_>,
    ) {
        crate::pms::seed_two_library_home_for_test(&mut self.state, sid, directory);
    }

    /// Test hook: the Home a real library has, `items` Continue Watching episodes as the deck row
    /// above one Recently Added shelf (`r1`..`r3`): see `crate::pms::seed_deck_home_for_test`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_deck_home_for_test(&mut self, items: usize) {
        let adapter = self.adapter_for_test();
        crate::pms::seed_deck_home_for_test(&mut self.state, &adapter, items);
    }

    /// Test hook: Home as one server's `/hubs` answer makes it, projected as the refresh projects it
    /// — see `crate::pms::seed_hubs_response_for_test`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_hubs_response_for_test(&mut self, mc: &plx_plex::plex::MediaContainer) {
        let adapter = self.adapter_for_test();
        crate::pms::seed_hubs_response_for_test(&mut self.state, &adapter, mc);
    }

    /// Test hook: a grid of `rows` shelves, `items` per shelf — see `crate::pms::seed_grid_for_test`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_grid_for_test(&mut self, rows: usize, items: usize) {
        let adapter = self.adapter_for_test();
        crate::pms::seed_grid_for_test(&mut self.state, &adapter, rows, items);
    }

    /// Test hook: queue a landing straight into this owner's mailbox — see
    /// `crate::pms::queue_test_landing`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn queue_test_landing(&self, items: Option<usize>) -> u32 {
        crate::pms::queue_test_landing(&self.state, &self.adapter, items)
    }

    /// Test hook: land the cards of every held row that waits for them — see
    /// `crate::pms::land_kept_rows_for_test`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn land_kept_rows_for_test(&mut self) -> bool { crate::pms::land_kept_rows_for_test(&mut self.state) }

    #[cfg(any(test, feature = "test-support"))]
    pub fn reverse_test_shelves(&mut self) { crate::pms::reverse_test_shelves(&mut self.state); }

    #[cfg(any(test, feature = "test-support"))]
    pub fn reverse_test_hubs(&mut self) { crate::pms::reverse_test_hubs(&mut self.state); }

    #[cfg(any(test, feature = "test-support"))]
    pub fn remove_test_item(&mut self, rk: &str) { crate::pms::remove_test_item(&mut self.state, rk); }

    #[cfg(any(test, feature = "test-support"))]
    pub fn retag_test_item_as_collection(&mut self, rk: &str) {
        crate::pms::retag_test_item_as_collection(&mut self.state, rk);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn hub_len_for_test(&self, i: usize) -> usize { crate::pms::hub_len(&self.state, i) }

    #[cfg(any(test, feature = "test-support"))]
    pub fn hub_item_for_test(&self, hub: usize, col: usize) -> Option<&crate::pms::PmsMovie> {
        crate::pms::hub_item(&self.state, hub, col)
    }

    /// A clone of this owner's current worker adapter. Capture it BEFORE calling into this store
    /// again: a `Reset` rotates it, and a port made from the old one lands into a mailbox nothing
    /// reads any more, which is the whole point of rotation.
    pub fn adapter(&self) -> Arc<crate::pms::PmsAdapter> { Arc::clone(&self.adapter) }

    /// Take the fetches this owner's steps have asked for since the last take, in the order they were
    /// asked ([`crate::stores::StoreJob::Hubs`]). The application's runner starts each, right after the step
    /// that asked (`Bridge::start_store_jobs_of`).
    #[must_use]
    pub fn take_jobs(&mut self) -> Vec<crate::stores::StoreJob> {
        std::mem::take(&mut self.state.jobs)
    }

    /// Does the outbox hold a job nobody has taken yet?
    pub fn has_jobs(&self) -> bool {
        !self.state.jobs.is_empty()
    }

    /// TEST ONLY: answer every job on the outbox the way a runner that starts nothing does
    /// (`Port::refused`), once each.
    #[cfg(any(test, feature = "test-support"))]
    pub fn refuse_jobs_for_test(&mut self) {
        use plx_machine::job::Port;
        for job in self.take_jobs() {
            let crate::stores::StoreJob::Hubs(job, port) = job else { panic!("another store's job on the Hubs outbox") };
            port.refused(job.addr);
        }
    }

    /// The adapter boundary: drain owned results without applying any store state.
    pub fn take_results(&self) -> Vec<HubsResult> {
        crate::pms::take_landings(&self.adapter)
    }

    /// Does a worker spawned for this owner still owe a landing? The claim the landing gate's dump
    /// mode waits on at Home's take; [`crate::pms::owed`] says why it is not `Src::fetching`.
    pub fn owed(&self) -> bool {
        crate::pms::owed(&self.adapter)
    }

    /// The cards in the rows Home holds (`HubsCmd::Hold`), for the re-read of what they show.
    pub fn held_keys(&self) -> super::HeldKeys { crate::pms::held_keys(&self.state) }

    /// Every source Home reads, in display order: what a re-read of the whole page asks one by one
    /// ([`HubsCmd::RefetchSourceQuiet`]).
    pub fn source_ids(&self) -> Vec<plx_plex::plex::ServerId> { crate::pms::source_ids(&self.state) }

    /// Is a quiet read's answer waiting for [`HubsCmd::Publish`]?
    pub fn has_staged(&self) -> bool { crate::pms::has_staged(&self.state) }

    /// Has source `sid`'s read landed (or failed)? See [`crate::pms::source_settled`].
    pub fn source_settled(&self, sid: plx_plex::plex::ServerId) -> bool {
        crate::pms::source_settled(&self.state, sid)
    }

    /// Test hook: how many authoritative refetches (`RefetchHubs`) this store has started.
    #[cfg(any(test, feature = "test-support"))]
    pub fn refetch_count_for_test(&self) -> u32 { crate::pms::hub_gen_for_test(&self.state) }

    /// Test hook: how many launched workers still owe a landing.
    #[cfg(any(test, feature = "test-support"))]
    pub fn owed_for_test(&self) -> u32 { crate::pms::owed_count_for_test(&self.adapter) }

    /// Test hook: the request ids of the launches refused and not yet settled by a step.
    #[cfg(any(test, feature = "test-support"))]
    pub fn refused_for_test(&self) -> Vec<u32> { crate::pms::refused_for_test(&self.adapter) }

    pub fn land_with_directory(
        &mut self,
        result: &HubsResult,
        directory: crate::stores::browse::DirectoryView<'_>,
        clients: &plx_plex::plex::ClientKeys,
    ) -> super::StoreOutcome {
        let outcome = crate::pms::land_with_directory(&mut self.state, &self.adapter, result, directory, clients);
        if outcome.changed { self.bump(); }
        outcome
    }

    pub fn tick_with_directory(
        &mut self,
        dt: f32,
        directory: crate::stores::browse::DirectoryView<'_>,
        clients: &plx_plex::plex::ClientKeys,
    ) -> super::StoreOutcome {
        let before = self.state.catalog_gen;
        let endpoints = crate::pms::tick_with_directory(&mut self.state, &self.adapter, dt, directory, clients);
        let changed = self.state.catalog_gen != before;
        if changed { self.bump(); }
        super::StoreOutcome { changed, endpoints }
    }

    /// Synchronous addressed command path. Reset rotates the adapter before clearing state, so an
    /// old worker can only finish into the retired mailbox it captured.
    #[cfg(any(test, feature = "test-support"))]
    pub fn run(&mut self, cmd: HubsCmd) -> super::StoreOutcome {
        self.prepare_command(Some(&cmd));
        let notice = cmd.always_notifies();
        let answer = crate::pms::run(&mut self.state, &self.adapter, cmd);
        if notice || answer.changed { self.bump(); }
        answer
    }

    /// Synchronous command path with the Browse owner publication captured by the application.
    /// Reset rotates the adapter before clearing state, so an old worker can only finish into the
    /// retired mailbox it captured.
    pub fn run_with_directory(
        &mut self,
        cmd: HubsCmd,
        directory: crate::stores::browse::DirectoryView<'_>,
        clients: &plx_plex::plex::ClientKeys,
    ) -> super::StoreOutcome {
        self.prepare_command(Some(&cmd));
        let notice = cmd.always_notifies();
        let answer = crate::pms::run_with_directory(&mut self.state, &self.adapter, cmd, directory, clients);
        if notice || answer.changed { self.bump(); }
        answer
    }
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    #[test]
    fn every_reset_entry_point_retires_workers_and_accepts_new_landings() {
        let _guard = plx_base::testlock::serial();
        let directory = crate::stores::browse::DirectorySnapshot::fixture(0, 0, vec![]);
        let keys = crate::pms::registry_keys_for_test();
        for path in ["run", "run_with_directory"] {
            let mut store = HubsStore::default();
            store.seed_for_test(1, crate::pms::HubState::Ready);
            let retired = store.adapter();
            crate::pms::queue_test_landing(&store.state, &retired, Some(1));

            match path {
                "run" => { let _ = store.run(HubsCmd::Reset); }
                "run_with_directory" => { let _ = store.run_with_directory(HubsCmd::Reset, directory.view(), &keys); }
                _ => unreachable!(),
            }

            assert!(!Arc::ptr_eq(&retired, &store.adapter()), "{path} kept the old worker adapter");
            store.seed_for_test(1, crate::pms::HubState::Ready);
            // Also finish a worker AFTER reset: emptying the old mailbox alone cannot fence it.
            crate::pms::queue_test_landing(&store.state, &retired, Some(1));
            assert!(store.take_results().is_empty(), "{path} admitted a retired worker");
            assert_eq!(crate::pms::take_landings(&retired).len(), 2);
            store.queue_test_landing(Some(1));
            assert_eq!(store.take_results().len(), 1, "{path} lost a new worker's landing");
        }
    }

    /// The per-frame publish check and a quiet ask are background chatter: they raise the notice only
    /// through what they changed, so a Home with nothing staged is not repainted every frame.
    #[test]
    fn background_commands_raise_the_notice_only_when_they_change_something() {
        let _guard = plx_base::testlock::serial();
        let directory = crate::stores::browse::DirectorySnapshot::fixture(0, 0, vec![]);
        let keys = crate::pms::registry_keys_for_test();
        let mut store = HubsStore::default();
        store.seed_for_test(2, crate::pms::HubState::Ready);
        let before = store.gen();
        for visible in [true, false] {
            let out = store.run_with_directory(
                HubsCmd::Publish { home_top_visible: visible, focused_hub: None, press_live: false }, directory.view(), &keys);
            assert!(!out.changed);
        }
        assert_eq!(store.gen(), before, "Publish with nothing staged does not raise the notice");
        assert_eq!(store.take_notice(), None);
        assert!(HubsCmd::Publish { home_top_visible: true, focused_hub: None, press_live: false }.is_background());
        assert!(!HubsCmd::RefetchHubs.is_background());
    }

    fn section(sid: plx_plex::plex::ServerId, section: usize, pinned: bool)
        -> crate::stores::browse::SectionView {
        crate::stores::browse::SectionView {
            sid: Some(sid),
            key: section as i64 + 1,
            kind: crate::stores::browse::SecKind::Movie,
            row: crate::stores::browse::SrcRow {
                section,
                title: format!("Library {section}"),
                pinned,
                current: section == 0,
                ..Default::default()
            },
        }
    }

    #[test]
    fn a_hubs_step_asks_only_for_the_sources_the_supplied_directory_pins() {
        let _guard = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let own = plx_plex::plex::register_for_test(
            "hubs-owned", "127.0.0.1", 9, "synthetic", "fixture");
        let hidden = plx_plex::plex::register_for_test(
            "hubs-hidden", "127.0.0.1", 10, "synthetic", "fixture");
        let directory = crate::stores::browse::DirectorySnapshot::fixture(
            7, 0, vec![section(own, 0, true), section(hidden, 1, false)]);
        let keys = crate::pms::registry_keys_for_test();
        let mut store = HubsStore::default();
        let _ = store.run_with_directory(HubsCmd::Reset, directory.view(), &keys);

        let _ = store.run_with_directory(HubsCmd::RefetchHubs, directory.view(), &keys);

        let asked: Vec<u16> = store.take_jobs().into_iter().map(|job| {
            let crate::stores::StoreJob::Hubs(job, _) = job else { panic!("another store's job on the Hubs outbox") };
            job.req.descriptor().2
        }).collect();
        assert_eq!(asked, [own.raw()], "the retained pin table excludes the unpinned source");
        assert!(!store.has_jobs(), "taking the jobs empties the outbox");
        let _ = store.run_with_directory(HubsCmd::Reset, directory.view(), &keys);
        plx_plex::plex::reset_servers_for_test();
    }

    /// **The two-owner regression.** A job's port holds a clone of its owner's `Arc<PmsAdapter>`
    /// from the step that emitted it (`HubsStore::adapter`); nothing about a landing knows which
    /// `HubsStore` minted the request it answers except which `Arc` it lands into. Two independently-owned
    /// stores (as two `Bridge`s would be, one per signed-in session) must not see each other's
    /// arrivals, and a `Reset`'s adapter rotation must make a worker started before it land into a
    /// mailbox nobody reads any more.
    #[test]
    fn a_landing_reaches_only_the_owner_whose_adapter_it_was_minted_from() {
        let _guard = plx_base::testlock::serial();
        let mut a = HubsStore::default();
        let b = HubsStore::default();
        let a_adapter = a.adapter();
        crate::pms::seed_for_test(&mut a.state, &a_adapter, 1, crate::pms::HubState::Ready);

        // A worker spawned off owner A's adapter lands there, and there alone.
        crate::pms::queue_test_landing(&a.state, &a_adapter, Some(1));
        assert_eq!(a.take_results().len(), 1, "A's own worker landed in A's mailbox");
        assert!(b.take_results().is_empty(), "B never received a landing it did not mint");

        // A worker captures the adapter it is about to land into BEFORE the reset that retires
        // it — here, queuing straight into that still-current `Arc`, the same thing a real worker
        // finishing in the gap between capture and rotation would do.
        let retired = a.adapter();
        crate::pms::queue_test_landing(&a.state, &retired, Some(1));

        // Reset rotates A's adapter. The already-queued landing stays in the RETIRED mailbox —
        // the fresh one `run` installed after Reset is empty, so the current owner (and any other
        // owner) never observes it.
        let _ = a.run(HubsCmd::Reset);
        assert!(!Arc::ptr_eq(&retired, &a.adapter()), "Reset must rotate the adapter Arc");
        assert!(
            a.take_results().is_empty(),
            "a landing queued into the retired adapter must not reach the rotated-in current owner"
        );
        assert!(b.take_results().is_empty(), "and it must never reach a different owner either");
    }
}
