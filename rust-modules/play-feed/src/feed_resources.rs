//! Owned native Engine, Load receipt, abandoned worker and release resources.
//! Route and product report settlement remain in the common player coordinator.

/// Main-thread owned load correlation. The worker sees only this token and its return channel;
/// the adapter maps it back to the exact route attempt after Engine installation.
#[derive(Clone)]
pub struct LoadReceipt {
    token: u64,
    sender: std::sync::mpsc::Sender<LoadOutcome>,
}
impl LoadReceipt {
    fn new(token: u64, sender: std::sync::mpsc::Sender<LoadOutcome>) -> Self {
        Self { token, sender }
    }
    pub fn token(&self) -> u64 { self.token }
    pub fn deliver(self, native_epoch: u32, ok: bool, elapsed_ms: Option<u64>) {
        let _ = self.sender.send(LoadOutcome { token: self.token, native_epoch, ok, elapsed_ms });
    }
}

/// The native Load call's synchronous verdict, addressed without reading route state on-worker.
struct LoadOutcome {
    token: u64,
    native_epoch: u32,
    ok: bool,
    /// Load time excluding the sound-output read; absent if the native epoch did not open its gate.
    elapsed_ms: Option<u64>,
}

// Physical buffer-feed resources for one player adapter. The common adapter still decides
// route settlement, reporting, release policy and admission.

use crate::feed_state;
use plx_playback_contract::feed::{defer_media_join, LoadRaw};
use plx_playback_contract::AttemptId;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::mpsc;

pub type UnloadSpawner = fn(plx_platform::tv::sink::UnloadTicket)
    -> Option<std::thread::JoinHandle<()>>;

pub struct FeedResources {
    shared: &'static crate::shared::PhysicalShared,
    hooks: &'static crate::native_executor::NativeHooks,
    engine: Option<feed_state::Engine>,
    load_tx: mpsc::Sender<LoadOutcome>,
    load_rx: mpsc::Receiver<LoadOutcome>,
    load_tickets: BTreeMap<u64, LoadTicket>,
    next_load_token: u64,
    abandoned_load: Option<feed_state::AbandonedLoad>,
    reaped_release: Option<(AttemptId, plx_playback_contract::ReleaseStatus)>,
    // The main-thread release tail may inspect/take this through `&self`; the adapter is !Send.
    pending_release: RefCell<Option<feed_state::PendingRelease>>,
    unload_spawner: Option<UnloadSpawner>,
}

struct LoadTicket {
    attempt: AttemptId,
    native_epoch: u32,
}

pub use plx_playback_contract::feed::AcceptedLoadOutcome;

impl FeedResources {
    pub fn new(shared: &'static crate::shared::PhysicalShared,
        hooks: &'static crate::native_executor::NativeHooks) -> Self {
        let (load_tx, load_rx) = mpsc::channel();
        Self {
            shared,
            hooks,
            engine: None,
            load_tx,
            load_rx,
            load_tickets: BTreeMap::new(),
            next_load_token: 0,
            abandoned_load: None,
            reaped_release: None,
            pending_release: RefCell::new(None),
            unload_spawner: None,
        }
    }

    pub fn shared(&self) -> &'static crate::shared::PhysicalShared { self.shared }

    /// The composition's hooks for the physical executor. `'static`, so a caller copies it out
    /// before it borrows the Engine out of `self`.
    pub fn hooks(&self) -> &'static crate::native_executor::NativeHooks { self.hooks }

    /// Override the task-start port for this owner; None uses the composition's default.
    pub fn set_unload_spawner(&mut self, spawner: Option<UnloadSpawner>) {
        self.unload_spawner = spawner;
    }

    pub fn unload_spawner(&self) -> Option<UnloadSpawner> { self.unload_spawner }

    pub fn engine(&mut self) -> Option<&mut feed_state::Engine> { self.engine.as_mut() }
    pub fn uses_stream_queues(&self) -> bool {
        self.engine.as_ref().is_some_and(feed_state::Engine::uses_stream_queues)
    }
    pub fn pending_seek_ready(&self, seek_reloads: bool) -> bool {
        self.engine.as_ref().is_some_and(|eng| {
            matches!(&eng.source, feed_state::Source::Stream)
                && eng.stage >= crate::shared::Stage::Playing
                && !(eng.flushed && eng.rebase_pending && !seek_reloads)
        })
    }
    pub fn begin_seek_discontinuity(&mut self) -> bool {
        self.engine.as_ref().is_some_and(|eng| self.shared.begin_native_media_discontinuity(eng.native_epoch))
    }
    pub fn is_live(&self) -> bool { self.engine.is_some() }

    /// The Engine's Load-side facts for one sample ([`LoadRaw`]); the idle value with no Engine.
    pub fn load_raw(&self) -> LoadRaw {
        let Some(engine) = self.engine.as_ref() else { return LoadRaw::IDLE; };
        LoadRaw {
            stage: Some(engine.stage),
            load_thread_finished: engine.load_th.as_ref().is_none_or(|thread| thread.is_finished()),
            route_actions_ready: matches!(&engine.source, feed_state::Source::Stream)
                && engine.stage >= crate::shared::Stage::Playing
                && !(engine.flushed && engine.rebase_pending),
        }
    }

    /// Retiring a preview: defer the join of a Load that has not returned, marking the preview
    /// abandoned. Answers `false` and marks nothing otherwise (the caller joins), and with no
    /// Engine.
    pub fn defer_preview_join(&mut self) -> bool {
        let load = self.load_raw();
        let defer = defer_media_join(load.loading(), load.load_thread_finished);
        if defer {
            if let Some(engine) = self.engine.as_mut() { engine.preview_abandon = true; }
        }
        defer
    }
    pub fn install(&mut self, value: feed_state::Engine) { self.engine = Some(value); }
    pub fn take(&mut self) -> Option<feed_state::Engine> { self.engine.take() }

    pub fn load_receipt(&mut self, attempt: AttemptId, native_epoch: u32) -> LoadReceipt {
        self.next_load_token = self.next_load_token.checked_add(1).expect("Load receipt IDs exhausted");
        let token = self.next_load_token;
        self.load_tickets.insert(token, LoadTicket { attempt, native_epoch });
        LoadReceipt::new(token, self.load_tx.clone())
    }

    pub fn cancel_load_receipt(&mut self, token: u64) { self.load_tickets.remove(&token); }

    pub fn retire_load_attempt(&mut self, attempt: AttemptId) {
        self.load_tickets.retain(|_, ticket| ticket.attempt != attempt);
    }

    /// Accept only a token and native epoch still owned by the installed Engine. Return verdicts
    /// in channel order; the common adapter settles their route attempts after this admission.
    pub fn take_load_outcomes(&mut self) -> Vec<AcceptedLoadOutcome> {
        let outcomes: Vec<_> = self.load_rx.try_iter().collect();
        let mut accepted = Vec::new();
        for outcome in outcomes {
            let Some(ticket) = self.load_tickets.remove(&outcome.token) else { continue; };
            if ticket.native_epoch != outcome.native_epoch
                || !self.engine.as_ref().is_some_and(|engine| engine.attempt == ticket.attempt && engine.native_epoch == ticket.native_epoch) {
                continue;
            }
            accepted.push(AcceptedLoadOutcome {
                attempt: ticket.attempt,
                ok: outcome.ok,
                elapsed_ms: outcome.elapsed_ms,
            });
        }
        accepted
    }

    pub fn park_abandoned_load(&mut self, load: feed_state::AbandonedLoad) {
        debug_assert!(self.abandoned_load.is_none(), "a second abandoned Load cannot exist");
        self.abandoned_load = Some(load);
    }
    pub fn has_abandoned_load(&self) -> bool { self.abandoned_load.is_some() }
    pub fn abandoned_load_mut(&mut self) -> Option<&mut feed_state::AbandonedLoad> { self.abandoned_load.as_mut() }
    pub fn take_abandoned_load(&mut self) -> Option<feed_state::AbandonedLoad> { self.abandoned_load.take() }
    pub fn note_reaped_release(&mut self, attempt: AttemptId,
        status: plx_playback_contract::ReleaseStatus) {
        self.reaped_release = Some((attempt, status));
    }
    pub fn take_reaped_release(&mut self) -> Option<(AttemptId, plx_playback_contract::ReleaseStatus)> {
        self.reaped_release.take()
    }
    pub fn has_reaped_release(&self) -> bool { self.reaped_release.is_some() }

    pub fn park_release(&self, release: feed_state::PendingRelease) {
        let mut slot = self.pending_release.borrow_mut();
        debug_assert!(slot.is_none(), "a second off-frame release cannot exist");
        *slot = Some(release);
    }
    pub fn has_pending_release(&self) -> bool { self.pending_release.borrow().is_some() }
    pub fn take_finished_release(&self) -> Option<feed_state::PendingRelease> {
        let mut slot = self.pending_release.borrow_mut();
        if slot.as_ref().is_some_and(|p| p.is_finished()) { slot.take() } else { None }
    }
    pub fn take_pending_release_any(&self) -> Option<feed_state::PendingRelease> {
        self.pending_release.borrow_mut().take()
    }
}
