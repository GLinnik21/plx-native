//! Physically owned subtitle-search model and its worker adapter. Shape copied from
//! `stores/collection.rs`; the model and its reasoning live in `crate::subsearch`.
//!
//! Deliberately NOT routed through `stores::tape`: its controlled-record schema is a closed list
//! of store names, and Search — the other store a screen pumps every frame — also lands through
//! the plain [`super::take_landing`] gate. A replay fixture that needs this store's landings adds
//! a record schema then.

use crate::subsearch::{SubSearchAdapter, SubSearchState, SubSearchView};
pub use crate::subsearch::SubSearchCmd;
use plx_machine::machine::{Cx, Effects, Handled, Host, Machine};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use super::StoreEv;

pub struct SubSearchStore {
    state: SubSearchState,
    adapter: Arc<SubSearchAdapter>,
    notice_gen: AtomicU32,
    notice_dirty: AtomicBool,
}

impl Default for SubSearchStore {
    fn default() -> Self {
        Self { state: Default::default(), adapter: Arc::new(Default::default()),
            notice_gen: AtomicU32::new(0), notice_dirty: AtomicBool::new(false) }
    }
}

impl SubSearchStore {
    fn bump(&self) {
        self.notice_dirty.store(true, Ordering::Relaxed);
        self.notice_gen.fetch_add(1, Ordering::Relaxed);
    }
    pub fn gen(&self) -> u32 { self.notice_gen.load(Ordering::Relaxed) }
    pub fn take_notice(&self) -> Option<u32> {
        self.notice_dirty.swap(false, Ordering::Relaxed).then(|| self.gen())
    }
    pub fn view(&self) -> SubSearchView<'_> { self.state.view() }
    pub fn run(&mut self, cmd: SubSearchCmd) -> bool {
        // a Reset orphans any worker still out: it lands into a mailbox nothing reads any more
        if matches!(cmd, SubSearchCmd::Reset) { self.adapter = Arc::new(Default::default()); }
        let changed = self.state.run(&self.adapter, cmd);
        if changed { self.bump(); }
        changed
    }
    pub fn pump(&mut self, gate: &plx_machine::landgate::Gate) -> bool {
        let changed = self.state.pump_with_gate(&self.adapter, gate);
        if changed { self.bump(); }
        changed
    }
}

impl<H: Host> Machine<H> for SubSearchStore {
    type Ev = StoreEv<SubSearchCmd>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, _fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            StoreEv::Cmd(cmd) => { self.run(cmd.clone()); }
            StoreEv::Pump { .. } => { self.pump(&plx_machine::landgate::Gate::default()); }
        }
        Handled::Yes
    }
}
