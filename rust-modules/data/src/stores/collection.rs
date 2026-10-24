//! Physically owned Collection model and its landing adapter. The store starts no worker: its pump
//! emits the next request as a [`StoreJob`](super::StoreJob) that its owner takes and owes to a
//! runner (`Stores::collection_pump`'s caller, `Bridge::collection_pump`), and it reads no registry:
//! which client a server holds arrives as [`ClientKeys`], filled by the host right before the call.

use crate::collection::{CollectionAdapter, CollectionPort, CollectionState, CollectionTarget, CollectionView};
use plx_machine::machine::{Cx, Effects, Handled, Host, Machine};
use plx_plex::plex::ClientKeys;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use super::StoreEv;
use std::ops::Range;

#[derive(Clone, Debug)]
pub enum CollectionCmd {
    Open { target: CollectionTarget },
    /// The screen's window: the indices it shows, the focused index and a pending restore target.
    /// Every landing evicts the pages outside these (see `PageCache::evict`).
    Window { wanted: Range<usize>, focus: Option<usize>, restore: Option<usize> },
    /// A restore's saved kept counts, one per page of a `total`-item listing, and the
    /// [`generation`](crate::collection::Collection::generation) of the listing they were saved
    /// from. With them the restore reads only its target's pages; without them the listing is read
    /// forward to it. A store still holding that listing (the page was only covered, never closed)
    /// ignores them and keeps its rows and its own counts, however far it has read since.
    Restore { total: usize, counts: Vec<u8>, generation: u32 },
    Close,
    Reset,
    /// Optimistic view-state edit (`viewstate::publish_view`), including the fan-out's copies.
    ApplyView { at: crate::viewstate::ViewAt, edit: crate::viewstate::ViewEdit },
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
    /// Join the owner's view-state publish order (`Stores::view_ledger`).
    pub(crate) fn share_ledger(&mut self, ledger: &crate::viewledger::ViewLedger) {
        self.state.ledger = ledger.clone();
    }

    fn bump(&self) {
        self.notice_dirty.store(true, Ordering::Relaxed);
        self.notice_gen.fetch_add(1, Ordering::Relaxed);
    }
    pub fn gen(&self) -> u32 { self.notice_gen.load(Ordering::Relaxed) }
    pub fn take_notice(&self) -> Option<u32> {
        self.notice_dirty.swap(false, Ordering::Relaxed).then(|| self.gen())
    }
    pub fn view(&self) -> CollectionView<'_> { self.state.view() }
    /// The members the page for `arg` holds cards for (its wanted window), or nothing when the
    /// store holds a different collection.
    pub fn held_keys(&self, arg: &super::content_arg::ContentArg) -> super::HeldKeys {
        let mut keys = super::HeldKeys::new();
        let (super::content_arg::ContentArg::Collection(r), Some(c)) = (arg, self.state.view().current()) else { return keys };
        if c.id.same_collection(r) { c.held_keys(&mut keys); }
        keys
    }
    /// Run one command. `keys` is the registry's answer as of this call. A command emits no job.
    pub fn run(&mut self, cmd: CollectionCmd, keys: &ClientKeys) -> bool {
        if matches!(cmd, CollectionCmd::Reset) { self.adapter = Arc::new(Default::default()); }
        let changed = self.state.run(&self.adapter, cmd, keys);
        if changed { self.bump(); }
        changed
    }
    /// One pump: take the landing and emit the next request, if the window needs one. `keys` is the
    /// registry's answer as of this call. It does NOT start the request it emitted: the caller
    /// takes it with [`CollectionStore::take_jobs`] and hands it to a runner in the same turn (the
    /// one production caller is `Bridge::collection_pump`; the application's test rigs fail a frame
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
    pub fn collection_port(&self) -> CollectionPort {
        CollectionPort::of(&self.adapter)
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
        // Unreached: nothing steps this store as a Machine (the only `StoreEv::Cmd` sender is
        // Browse, and nothing sends `StoreEv::Pump`). A future sender must pass the host's keys
        // and drain the jobs the pump emits; these empty keys read as "no server holds a client"
        // (the page fails), and real keys without the drain would strand the job.
        let keys = ClientKeys::default();
        match ev {
            StoreEv::Cmd(cmd) => { self.run(cmd.clone(), &keys); }
            StoreEv::Pump { .. } => { self.pump(&plx_machine::landgate::Gate::default(), &keys); }
        }
        Handled::Yes
    }
}
