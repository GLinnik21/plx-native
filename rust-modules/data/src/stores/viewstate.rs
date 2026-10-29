//! The physically owned view-state write queue (`docs/stores-as-machines.md`). Each production
//! `Bridge` owns one [`ViewStateStore`]: main-thread state, an `Arc` mailbox adapter every sent
//! write's port captures, and notice.

use plx_plex::plex::{ClientKeys, ServerId};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

/// Every mutation of the view-state write store.
#[derive(Clone)]
pub enum ViewStateCmd {
    Request {
        sid: ServerId,
        rk: String,
        write: crate::viewstate::Write,
        /// How far down the library the press reaches: a show's or season's episode rows too.
        scope: crate::viewstate::Scope,
        /// Portable item identity when the caller has the exact item's guid; empty asks the worker
        /// to resolve it from `(sid, rk)`.
        guid: String,
    },
    Reset,
}

/// One ViewState owner. Reset rotates `adapter` before clearing `state`, so a write sent before the
/// reset can only answer into the retired mailbox its port captured.
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

    /// Synchronous command path. Optimistic edits reach every supplied owner before this returns.
    /// The outcome's `endpoints` are the ones the edits' Hubs step settled: the caller carries them.
    pub fn run(
        &mut self,
        cmd: ViewStateCmd,
        keys: &ClientKeys,
        profile: u32,
        browse: &mut dyn FnMut(crate::stores::browse::BrowseCmd) -> bool,
        hubs: &mut dyn FnMut(crate::stores::hubs::HubsCmd) -> super::StoreOutcome,
        person: &mut dyn FnMut(crate::stores::person::PersonCmd) -> bool,
        collection: &mut dyn FnMut(crate::stores::collection::CollectionCmd) -> bool,
        search: &mut dyn FnMut(crate::stores::search::SearchCmd) -> bool,
        metadata: &mut dyn FnMut(crate::stores::metadata::MetadataCmd) -> bool,
    ) -> super::StoreOutcome {
        if matches!(&cmd, ViewStateCmd::Reset) {
            self.adapter = Arc::new(Default::default());
        }
        let answer = self.state.run(&self.adapter, cmd, keys, profile, browse, hubs, person, collection, search, metadata);
        self.bump();
        answer
    }

    /// Route-unconditional landing pass for this owner's adapter.
    pub fn pump_with_gate(
        &mut self,
        gate: &plx_machine::landgate::Gate,
        keys: &ClientKeys,
        profile: u32,
        browse: &mut dyn FnMut(crate::stores::browse::BrowseCmd) -> bool,
        hubs: &mut dyn FnMut(crate::stores::hubs::HubsCmd) -> super::StoreOutcome,
        person: &mut dyn FnMut(crate::stores::person::PersonCmd) -> bool,
        collection: &mut dyn FnMut(crate::stores::collection::CollectionCmd) -> bool,
        search: &mut dyn FnMut(crate::stores::search::SearchCmd) -> bool,
        metadata: &mut dyn FnMut(crate::stores::metadata::MetadataCmd) -> bool,
    ) -> super::EndpointRefreshSet {
        let busy = self.state.is_busy();
        let endpoints = self.state.pump_with_gate(&self.adapter, gate, keys, profile,
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
        self.pump_with_gate(plx_machine::landgate::fixture_gate(), &ClientKeys::default(), 0, browse, hubs, person,
            collection, search, metadata)
    }

    /// Hold the terminal hub refetch while the player is on top; see
    /// [`crate::viewstate::ViewStateState::hold_refresh`].
    pub fn set_hold_refresh(&mut self, on: bool) {
        self.state.hold_refresh = on;
    }

    /// Fan an end-of-stream watch out to the title's other copies; see
    /// [`crate::viewstate::ViewStateState::fan_out_origin_watched`].
    pub fn fan_out_origin_watched(&mut self, sid: ServerId, rk: &str, keys: &ClientKeys, profile: u32) -> bool {
        let queued = self.state.fan_out_origin_watched(&self.adapter, sid, rk, keys, profile);
        if queued {
            self.bump();
        }
        queued
    }

    /// Items whose landed write is owed a read-back (`stores::viewreconcile`).
    pub fn take_reread(&mut self) -> Vec<(ServerId, String)> {
        self.state.take_reread()
    }

    /// Take the writes the step has sent since the last take, in order. The caller OWES each one to
    /// a runner (`Bridge::viewstate_run` / `viewstate_pump` / `reconcile_pump` hand them over in the
    /// same call): a write dropped here is never answered, and the queue behind it never starts.
    #[must_use]
    pub fn take_jobs(&mut self) -> Vec<super::StoreJob> {
        self.state.take_jobs()
    }

    /// Does the outbox hold a write nobody has taken yet?
    pub fn has_jobs(&self) -> bool {
        self.state.has_jobs()
    }

    /// Is a write queued or in flight?
    pub fn is_busy(&self) -> bool {
        self.state.is_busy()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn is_busy_for_test(&self) -> bool {
        self.state.is_busy()
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
