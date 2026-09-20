//! Home's hub catalog store boundary over `crate::pms` (`docs/stores-as-machines.md`). Each
//! production `Bridge` owns one [`HubsStore`]; no free selector can connect two Bridges.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(crate) enum HubsCmd {
    /// A refetch is owed (a view-state write landed, a profile settled).
    RefetchHubs,
    /// The read-out's Retry: clear the back-off and ask again.
    Retry,
    /// The profile/account switch.
    Reset,
    /// The optimistic half of a view-state write on the hub catalog (`pms::LocalEdit`).
    EditItem { sid: crate::plex::ServerId, rk: String, edit: crate::pms::LocalEdit },
}

pub(crate) use crate::pms::Landing as HubsResult;

/// One Hubs owner: logical state, the worker adapter all current fetches capture, and notice.
pub(crate) struct HubsStore {
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
    /// Seed a fresh owner from restored boot initial conditions (`pms::initial::Initial::restore`),
    /// before any `Bridge`/`Stores` exists.
    pub(crate) fn from_parts(state: crate::pms::PmsState, adapter: crate::pms::PmsAdapter) -> Self {
        Self { state, adapter: Arc::new(adapter), notice_gen: AtomicU32::new(0), notice_dirty: AtomicBool::new(false) }
    }

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

    pub(crate) fn snapshot(&self) -> crate::pms::HubsSnapshot {
        crate::pms::hubs_snapshot(&self.state)
    }

    #[cfg(test)]
    pub(crate) fn state(&self) -> &crate::pms::PmsState { &self.state }

    #[cfg(test)]
    pub(crate) fn state_mut(&mut self) -> &mut crate::pms::PmsState { &mut self.state }

    #[cfg(test)]
    pub(crate) fn adapter_for_test(&self) -> Arc<crate::pms::PmsAdapter> { Arc::clone(&self.adapter) }

    /// Test hook: put this owner in a known place — one source, `items` rows in one shelf. Mirrors
    /// `crate::pms::seed_for_test`, threaded onto this store's own owned `(state, adapter)` pair
    /// rather than the deleted process-wide catalog.
    #[cfg(test)]
    pub(crate) fn seed_for_test(&mut self, items: usize, hub_state: crate::pms::HubState) {
        let adapter = self.adapter_for_test();
        crate::pms::seed_for_test(&mut self.state, &adapter, items, hub_state);
    }

    /// Test hook: a directory-scoped Hubs source — see `crate::pms::seed_for_directory_test`.
    #[cfg(test)]
    pub(crate) fn seed_for_directory_test(
        &mut self,
        sid: crate::plex::ServerId,
        items: usize,
        hub_state: crate::pms::HubState,
        directory: crate::stores::browse::DirectoryView<'_>,
    ) {
        let adapter = self.adapter_for_test();
        crate::pms::seed_for_directory_test(&mut self.state, &adapter, sid, items, hub_state, directory);
    }

    /// Test hook: the two-library Home fixture — see `crate::pms::seed_two_library_home_for_test`.
    #[cfg(test)]
    pub(crate) fn seed_two_library_home_for_test(
        &mut self,
        sid: crate::plex::ServerId,
        directory: crate::stores::browse::DirectoryView<'_>,
    ) {
        crate::pms::seed_two_library_home_for_test(&mut self.state, sid, directory);
    }

    /// Test hook: a grid of `rows` shelves, `items` per shelf — see `crate::pms::seed_grid_for_test`.
    #[cfg(test)]
    pub(crate) fn seed_grid_for_test(&mut self, rows: usize, items: usize) {
        let adapter = self.adapter_for_test();
        crate::pms::seed_grid_for_test(&mut self.state, &adapter, rows, items);
    }

    /// Test hook: queue a landing straight into this owner's mailbox — see
    /// `crate::pms::queue_test_landing`.
    #[cfg(test)]
    pub(crate) fn queue_test_landing(&self, items: Option<usize>) -> u32 {
        crate::pms::queue_test_landing(&self.state, &self.adapter, items)
    }

    #[cfg(test)]
    pub(crate) fn reverse_test_shelves(&mut self) { crate::pms::reverse_test_shelves(&mut self.state); }

    #[cfg(test)]
    pub(crate) fn reverse_test_hubs(&mut self) { crate::pms::reverse_test_hubs(&mut self.state); }

    #[cfg(test)]
    pub(crate) fn remove_test_item(&mut self, rk: &str) { crate::pms::remove_test_item(&mut self.state, rk); }

    #[cfg(test)]
    pub(crate) fn hub_len_for_test(&self, i: usize) -> usize { crate::pms::hub_len(&self.state, i) }

    #[cfg(test)]
    pub(crate) fn hub_item_for_test(&self, hub: usize, col: usize) -> Option<&crate::pms::PmsMovie> {
        crate::pms::hub_item(&self.state, hub, col)
    }

    /// A clone of this owner's current worker adapter, for a caller that must spawn its own
    /// fetch (`crate::pms::spawn_fetch`) rather than go through `controlled_with_directory`'s
    /// default launcher. Capture it BEFORE calling into this store again — see the module doc
    /// on why an old worker landing into a retired `Arc` is the whole point of rotation.
    pub(crate) fn adapter(&self) -> Arc<crate::pms::PmsAdapter> { Arc::clone(&self.adapter) }

    /// The adapter boundary: drain owned results without applying any store state.
    pub(crate) fn take_results(&self) -> Vec<HubsResult> {
        crate::pms::take_landings(&self.adapter)
    }

    pub(crate) fn land_with_directory(
        &mut self,
        result: &HubsResult,
        directory: crate::stores::browse::DirectoryView<'_>,
    ) -> super::StoreOutcome {
        let outcome = crate::pms::land_with_directory(&mut self.state, &self.adapter, result, directory);
        if outcome.changed { self.bump(); }
        outcome
    }

    pub(crate) fn tick_with_directory(
        &mut self,
        dt: f32,
        directory: crate::stores::browse::DirectoryView<'_>,
    ) -> super::StoreOutcome {
        let before = self.state.catalog_gen;
        let endpoints = crate::pms::tick_with_directory(&mut self.state, &self.adapter, dt, directory);
        let changed = self.state.catalog_gen != before;
        if changed { self.bump(); }
        super::StoreOutcome { changed, endpoints }
    }

    /// Test-only standalone shape for bootstrap fixtures with no Browse directory.
    #[cfg(test)]
    pub(crate) fn controlled(&mut self, cmd: Option<HubsCmd>, dt: f32,
        launch: &mut dyn FnMut(crate::pms::HubRequest) -> bool) -> super::StoreOutcome {
        crate::testlock::assert_held("controlled hubs store");
        let command = cmd.is_some();
        let outcome = crate::pms::controlled_work(&mut self.state, &self.adapter, cmd, dt, launch);
        if command || outcome.changed { self.bump(); }
        outcome
    }

    /// Controlled Home work scoped by the Bridge's retained Browse directory. The retained view is
    /// the decision input for this frame.
    pub(crate) fn controlled_with_directory(&mut self, cmd: Option<HubsCmd>, dt: f32,
        directory: crate::stores::browse::DirectoryView<'_>,
        launch: &mut dyn FnMut(crate::pms::HubRequest) -> bool) -> super::StoreOutcome {
        #[cfg(test)]
        crate::testlock::assert_held("controlled hubs store with Browse owner");
        let command = cmd.is_some();
        let outcome = crate::pms::controlled_work_with_directory(&mut self.state, &self.adapter, cmd, dt, directory, launch);
        if command || outcome.changed { self.bump(); }
        outcome
    }

    /// Synchronous addressed command path. Reset rotates the adapter before clearing state, so an
    /// old worker can only finish into the retired mailbox it captured.
    #[cfg(test)]
    pub(crate) fn run(&mut self, cmd: HubsCmd) -> super::StoreOutcome {
        if matches!(&cmd, HubsCmd::Reset) {
            self.adapter = Arc::new(Default::default());
        }
        let answer = crate::pms::run(&mut self.state, &self.adapter, cmd);
        self.bump();
        answer
    }

    /// Synchronous command path with the Browse owner publication captured by the application.
    /// Reset rotates the adapter before clearing state, so an old worker can only finish into the
    /// retired mailbox it captured.
    pub(crate) fn run_with_directory(
        &mut self,
        cmd: HubsCmd,
        directory: crate::stores::browse::DirectoryView<'_>,
    ) -> super::StoreOutcome {
        if matches!(&cmd, HubsCmd::Reset) {
            self.adapter = Arc::new(Default::default());
        }
        let answer = crate::pms::run_with_directory(&mut self.state, &self.adapter, cmd, directory);
        self.bump();
        answer
    }
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    fn section(sid: crate::plex::ServerId, section: usize, pinned: bool)
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
    fn controlled_hubs_uses_the_supplied_directory() {
        let _guard = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let own = crate::plex::register_for_test(
            "hubs-owned", "127.0.0.1", 9, "synthetic", "fixture");
        let hidden = crate::plex::register_for_test(
            "hubs-hidden", "127.0.0.1", 10, "synthetic", "fixture");
        let directory = crate::stores::browse::DirectorySnapshot::fixture(
            7, 0, vec![section(own, 0, true), section(hidden, 1, false)]);
        let mut store = HubsStore::default();
        let mut ignored = |_| false;
        let _ = store.controlled_with_directory(Some(HubsCmd::Reset), 0.0, directory.view(), &mut ignored);
        let mut launched = Vec::new();

        let _ = store.controlled_with_directory(Some(HubsCmd::RefetchHubs), 0.0, directory.view(),
            &mut |request| {
                launched.push(request.descriptor().2);
                false
            });

        assert_eq!(launched, [own.raw()],
            "the retained pin table excludes the unpinned source");
        let _ = store.controlled_with_directory(Some(HubsCmd::Reset), 0.0, directory.view(), &mut ignored);
        crate::plex::reset_servers_for_test();
    }

    /// **The two-owner regression.** A worker captures a clone of its owner's `Arc<PmsAdapter>`
    /// before it spawns (`HubsStore::adapter`); nothing about a landing knows which `HubsStore`
    /// minted the request it answers except which `Arc` it lands into. Two independently-owned
    /// stores (as two `Bridge`s would be, one per signed-in session) must not see each other's
    /// arrivals, and a `Reset`'s adapter rotation must make a worker started before it land into a
    /// mailbox nobody reads any more.
    #[test]
    fn a_landing_reaches_only_the_owner_whose_adapter_it_was_minted_from() {
        let _guard = crate::testlock::serial();
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
