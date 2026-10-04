//! The physically owned view-state write queue (`docs/stores-as-machines.md`). Each production
//! `Bridge` owns one [`ViewStateStore`]: main-thread state, an `Arc` worker adapter, and notice.

use plx_plex::plex::ServerId;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

/// The exact Detail page a completed burst must reconcile. `keep` is the episode whose filmstrip
/// position survives the reload; `None` is the page hero's own watch toggle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailRefresh {
    pub sid: ServerId,
    pub rk: String,
    pub keep: Option<String>,
}

/// Every mutation of the view-state write store.
#[derive(Clone)]
pub enum ViewStateCmd {
    Request {
        sid: ServerId,
        rk: String,
        write: crate::viewstate::Write,
        /// The originating Detail page, if this press should reconcile one after the burst.
        detail: Option<DetailRefresh>,
        /// Portable item identity when the caller has the exact item's guid; empty asks the worker
        /// to resolve it from `(sid, rk)`.
        guid: String,
    },
    Reset,
}

/// One ViewState owner. Reset rotates `adapter` before clearing `state`, so a detached old worker
/// can only complete into the retired mailbox it captured.
pub struct ViewStateStore {
    state: crate::viewstate::ViewStateState,
    adapter: Arc<crate::viewstate::ViewStateAdapter>,
    notice_gen: AtomicU32,
    notice_dirty: AtomicBool,
}

impl Default for ViewStateStore {
    fn default() -> Self {
        Self {
            state: Default::default(),
            adapter: Arc::new(Default::default()),
            notice_gen: AtomicU32::new(0),
            notice_dirty: AtomicBool::new(false),
        }
    }
}

impl ViewStateStore {
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

    /// Synchronous command path. Optimistic edits reach every supplied owner before this returns.
    pub fn run(
        &mut self,
        cmd: ViewStateCmd,
        browse: &mut dyn FnMut(crate::stores::browse::BrowseCmd) -> bool,
        hubs: &mut dyn FnMut(crate::stores::hubs::HubsCmd) -> super::StoreOutcome,
        person: &mut dyn FnMut(crate::stores::person::PersonCmd) -> bool,
        collection: &mut dyn FnMut(crate::stores::collection::CollectionCmd) -> bool,
        search: &mut dyn FnMut(crate::stores::search::SearchCmd) -> bool,
        metadata: &mut dyn FnMut(crate::stores::metadata::MetadataCmd) -> bool,
    ) -> bool {
        if matches!(&cmd, ViewStateCmd::Reset) {
            self.adapter = Arc::new(Default::default());
        }
        let answer = self.state.run(&self.adapter, cmd, browse, hubs, person, collection, search, metadata);
        self.bump();
        answer
    }

    /// Route-unconditional landing pass for this owner's adapter.
    pub fn pump_with_gate(
        &mut self,
        gate: &plx_machine::landgate::Gate,
        browse: &mut dyn FnMut(crate::stores::browse::BrowseCmd) -> bool,
        hubs: &mut dyn FnMut(crate::stores::hubs::HubsCmd) -> super::StoreOutcome,
        person: &mut dyn FnMut(crate::stores::person::PersonCmd) -> bool,
        collection: &mut dyn FnMut(crate::stores::collection::CollectionCmd) -> bool,
        search: &mut dyn FnMut(crate::stores::search::SearchCmd) -> bool,
        metadata: &mut dyn FnMut(crate::stores::metadata::MetadataCmd) -> bool,
    ) -> super::EndpointRefreshSet {
        let busy = self.state.is_busy();
        let endpoints = self.state.pump_with_gate(&self.adapter, gate,
            browse, hubs, person, collection, search, metadata);
        if busy != self.state.is_busy() {
            self.bump();
        }
        endpoints
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn pump(
        &mut self,
        browse: &mut dyn FnMut(crate::stores::browse::BrowseCmd) -> bool,
        hubs: &mut dyn FnMut(crate::stores::hubs::HubsCmd) -> super::StoreOutcome,
        person: &mut dyn FnMut(crate::stores::person::PersonCmd) -> bool,
        collection: &mut dyn FnMut(crate::stores::collection::CollectionCmd) -> bool,
        search: &mut dyn FnMut(crate::stores::search::SearchCmd) -> bool,
        metadata: &mut dyn FnMut(crate::stores::metadata::MetadataCmd) -> bool,
    ) -> super::EndpointRefreshSet {
        self.pump_with_gate(plx_machine::landgate::fixture_gate(), browse, hubs, person, collection, search,
            metadata)
    }

    pub fn take_detail_refresh(&mut self) -> Option<DetailRefresh> {
        self.state.take_detail_refresh()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn hold_inflight_for_test(&mut self, sid: ServerId, rk: &str) {
        self.state.hold_inflight_for_test(sid, rk);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn owe_hubs_refresh_for_test(&mut self) {
        self.state.owe_hubs_refresh_for_test();
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_ownership_fixture_for_test(&mut self) {
        self.state.seed_ownership_fixture(&self.adapter);
        self.bump();
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn ownership_fixture_for_test(&self) -> crate::viewstate::OwnershipFixture {
        self.state.ownership_fixture(&self.adapter)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn late_completion_for_test(&self) -> Box<dyn FnOnce()> {
        self.state.late_completion(&self.adapter)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_post_reset_flight_for_test(&mut self) {
        self.state.seed_post_reset_flight();
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn adapter_for_test(&self) -> Arc<crate::viewstate::ViewStateAdapter> {
        Arc::clone(&self.adapter)
    }
}
