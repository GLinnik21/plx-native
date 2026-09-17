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
    pub(crate) fn adapter_for_test(&self) -> Arc<crate::pms::PmsAdapter> { Arc::clone(&self.adapter) }

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
            borrowed: false,
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
        let mut ignored = |_| false;
        let _ = controlled_with_directory(Some(HubsCmd::Reset), 0.0, directory.view(), &mut ignored);
        let mut launched = Vec::new();

        let _ = controlled_with_directory(Some(HubsCmd::RefetchHubs), 0.0, directory.view(),
            &mut |request| {
                launched.push(request.descriptor().2);
                false
            });

        assert_eq!(launched, [own.raw()],
            "the retained pin table excludes the unpinned source");
        let _ = controlled_with_directory(Some(HubsCmd::Reset), 0.0, directory.view(), &mut ignored);
        crate::plex::reset_servers_for_test();
    }
}
