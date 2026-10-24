//! Physically owned subtitle-search model and its landing adapter. Shape copied from
//! `stores/collection.rs`; the model and its reasoning live in `crate::subsearch`. The store starts
//! no worker: its pump emits the next request as a [`StoreJob`](super::StoreJob) that its owner
//! takes and owes to a runner (`Stores::subtitle_search_pump`'s caller, `Bridge::subsearch_pump`),
//! and it reads no registry: which client a server holds arrives as [`ClientKeys`], filled by the
//! host right before the call.
//!
//! Deliberately NOT routed through `stores::tape`: its controlled-record schema is a closed list
//! of store names, and Search — the other store a screen pumps every frame — also lands through
//! the store gate rather than the tape: both take through [`super::take_landing_owed`] (the
//! dump-mode wait) with `Fetch::take_current`. Plain [`super::take_landing`] is a single poll in
//! dump mode and is counted by `Gate::unconverted_takes`, so a new site uses the owed spelling. A replay fixture that
//! needs this store's landings adds a record schema then.

use crate::subsearch::{SubSearchAdapter, SubSearchPort, SubSearchState, SubSearchView};
pub use crate::subsearch::SubSearchCmd;
use plx_machine::machine::{Cx, Effects, Handled, Host, Machine};
use plx_plex::plex::ClientKeys;
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
    /// Run one command. A command reads no client and emits no job: the request is the pump's.
    pub fn run(&mut self, cmd: SubSearchCmd) -> bool {
        // a Reset orphans any worker still out: it lands into a mailbox nothing reads any more
        if matches!(cmd, SubSearchCmd::Reset) { self.adapter = Arc::new(Default::default()); }
        let changed = self.state.run(&self.adapter, cmd);
        if changed { self.bump(); }
        changed
    }
    /// One pump: take the landing and emit the next request, if one is owed. `keys` is the
    /// registry's answer as of this call. It does NOT start the request it emitted: the caller
    /// takes it with [`SubSearchStore::take_jobs`] and hands it to a runner in the same turn (the
    /// one production caller is `Bridge::subsearch_pump`; the application's test rigs fail a frame
    /// that ends with a job still here, `Bridge::assert_no_store_jobs_left_for_test`).
    pub fn pump(&mut self, gate: &plx_machine::landgate::Gate, keys: &ClientKeys) -> bool {
        let changed = self.state.pump_with_gate(&self.adapter, gate, keys);
        if changed { self.bump(); }
        changed
    }

    /// Take the jobs the pump has emitted since the last take, in order. The caller OWES each one
    /// to a runner: a job dropped here is a request whose claim is never released.
    #[must_use]
    pub fn take_jobs(&mut self) -> Vec<super::StoreJob> {
        std::mem::take(&mut self.state.jobs)
    }

    /// Does the outbox hold a job nobody has taken yet?
    pub fn has_jobs(&self) -> bool {
        !self.state.jobs.is_empty()
    }

    /// The port that lands an answer into THIS store's current adapter: what a job emitted now
    /// carries (`assert_fx!` compares it by adapter identity).
    pub fn subsearch_port(&self) -> SubSearchPort {
        SubSearchPort::of(&self.adapter)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn adapter_for_test(&self) -> &Arc<SubSearchAdapter> { &self.adapter }
}

impl<H: Host> Machine<H> for SubSearchStore {
    type Ev = StoreEv<SubSearchCmd>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, _fx: &mut Effects<'_, H>) -> Handled {
        // Unreached: nothing steps this store as a Machine (`step_store` is `unreachable!` for it).
        // A future sender must pass the host's keys and drain the jobs the pump emits; these empty
        // keys read as "no server holds a client" (the pump backs off and emits nothing), and real
        // keys without the drain would strand the job.
        match ev {
            StoreEv::Cmd(cmd) => { self.run(cmd.clone()); }
            StoreEv::Pump { .. } => { self.pump(&plx_machine::landgate::Gate::default(), &ClientKeys::default()); }
        }
        Handled::Yes
    }
}
