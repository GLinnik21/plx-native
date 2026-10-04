//! Physically owned Collection model and its worker adapter.

use crate::collection::{CollectionAdapter, CollectionState, CollectionTarget, CollectionView};
use plx_plex::plex::ServerId;
use plx_machine::machine::{Cx, Effects, Handled, Host, Machine};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use super::StoreEv;

#[derive(Clone, Debug)]
pub enum CollectionCmd {
    Open { target: CollectionTarget },
    Close,
    Reset,
    /// Optimistic watched-state edit from the view-state fan-out (see `viewstate::fan_out`).
    SetWatchedLocal { sid: ServerId, rk: String, on: bool },
}

pub struct CollectionStore {
    state: CollectionState,
    adapter: Arc<CollectionAdapter>,
    notice_gen: AtomicU32,
    notice_dirty: AtomicBool,
}

impl Default for CollectionStore {
    fn default() -> Self {
        Self { state: Default::default(), adapter: Arc::new(Default::default()),
            notice_gen: AtomicU32::new(0), notice_dirty: AtomicBool::new(false) }
    }
}

impl CollectionStore {
    fn bump(&self) {
        self.notice_dirty.store(true, Ordering::Relaxed);
        self.notice_gen.fetch_add(1, Ordering::Relaxed);
    }
    pub fn gen(&self) -> u32 { self.notice_gen.load(Ordering::Relaxed) }
    pub fn take_notice(&self) -> Option<u32> {
        self.notice_dirty.swap(false, Ordering::Relaxed).then(|| self.gen())
    }
    pub fn view(&self) -> CollectionView<'_> { self.state.view() }
    pub fn run(&mut self, cmd: CollectionCmd) -> bool {
        if matches!(cmd, CollectionCmd::Reset) { self.adapter = Arc::new(Default::default()); }
        let changed = self.state.run(&self.adapter, cmd);
        if changed { self.bump(); }
        changed
    }
    pub fn pump(&mut self, gate: &plx_machine::landgate::Gate) -> bool {
        let changed = self.state.pump_with_gate(&self.adapter, gate);
        if changed { self.bump(); }
        changed
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn generation_for_test(&self) -> u32 { self.state.generation() }
    #[cfg(any(test, feature = "test-support"))]
    pub fn adapter_for_test(&self) -> Arc<CollectionAdapter> { Arc::clone(&self.adapter) }
    #[cfg(any(test, feature = "test-support"))]
    pub fn install_for_test(&mut self, items: Vec<crate::pms::PmsMovie>, status: crate::collection::CollectionStatus) {
        self.state.install_for_test(items, status);
        self.bump();
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn edit_for_test(&mut self, edit: impl FnOnce(&mut crate::collection::Collection)) {
        self.state.edit_for_test(edit);
        self.bump();
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn take_landing_for_test(&mut self) -> bool {
        let changed = self.state.take_landing_for_test(&self.adapter);
        if changed { self.bump(); }
        changed
    }
}

impl<H: Host> Machine<H> for CollectionStore {
    type Ev = StoreEv<CollectionCmd>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, _fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            StoreEv::Cmd(cmd) => { self.run(cmd.clone()); }
            StoreEv::Pump { .. } => { self.pump(&plx_machine::landgate::Gate::default()); }
        }
        Handled::Yes
    }
}
