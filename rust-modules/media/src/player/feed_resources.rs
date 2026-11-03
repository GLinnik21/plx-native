//! Physical buffer-feed resources for one player adapter. This is a private ownership boundary:
//! the common adapter still decides route settlement, reporting, release policy and admission.

use super::{engine, threads};
use crate::route::RouteStartAttempt;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::mpsc;

pub(super) struct FeedResources {
    engine: Option<engine::Engine>,
    load_tx: mpsc::Sender<threads::LoadOutcome>,
    load_rx: mpsc::Receiver<threads::LoadOutcome>,
    load_tickets: BTreeMap<u64, LoadTicket>,
    next_load_token: u64,
    abandoned_load: Option<engine::AbandonedLoad>,
    // The main-thread release tail may inspect/take this through `&self`; the adapter is !Send.
    pending_release: RefCell<Option<engine::PendingRelease>>,
}

struct LoadTicket {
    attempt: RouteStartAttempt,
    native_epoch: u32,
    trace_generation: u32,
}

pub(super) struct AcceptedLoadOutcome {
    pub attempt: RouteStartAttempt,
    pub trace_generation: u32,
    pub ok: bool,
    pub elapsed_ms: Option<u64>,
}

impl FeedResources {
    pub fn new() -> Self {
        let (load_tx, load_rx) = mpsc::channel();
        Self {
            engine: None,
            load_tx,
            load_rx,
            load_tickets: BTreeMap::new(),
            next_load_token: 0,
            abandoned_load: None,
            pending_release: RefCell::new(None),
        }
    }

    pub fn engine(&mut self) -> Option<&mut engine::Engine> { self.engine.as_mut() }
    pub fn is_live(&self) -> bool { self.engine.is_some() }
    pub fn install(&mut self, value: engine::Engine) { self.engine = Some(value); }
    pub fn take(&mut self) -> Option<engine::Engine> { self.engine.take() }

    pub fn load_receipt(&mut self, attempt: RouteStartAttempt, native_epoch: u32,
        trace_generation: u32) -> threads::LoadReceipt {
        self.next_load_token = self.next_load_token.checked_add(1).expect("Load receipt IDs exhausted");
        let token = self.next_load_token;
        self.load_tickets.insert(token, LoadTicket { attempt, native_epoch, trace_generation });
        threads::LoadReceipt::new(token, self.load_tx.clone())
    }

    pub fn cancel_load_receipt(&mut self, token: u64) { self.load_tickets.remove(&token); }

    pub fn retire_load_attempt(&mut self, attempt: RouteStartAttempt) {
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
                || !self.engine.as_ref().is_some_and(|engine| engine.route_start == ticket.attempt && engine.native_epoch == ticket.native_epoch) {
                continue;
            }
            accepted.push(AcceptedLoadOutcome {
                attempt: ticket.attempt,
                trace_generation: ticket.trace_generation,
                ok: outcome.ok,
                elapsed_ms: outcome.elapsed_ms,
            });
        }
        accepted
    }

    pub fn park_abandoned_load(&mut self, load: engine::AbandonedLoad) {
        debug_assert!(self.abandoned_load.is_none(), "a second abandoned Load cannot exist");
        self.abandoned_load = Some(load);
    }
    pub fn has_abandoned_load(&self) -> bool { self.abandoned_load.is_some() }
    pub fn abandoned_load_mut(&mut self) -> Option<&mut engine::AbandonedLoad> { self.abandoned_load.as_mut() }
    pub fn take_abandoned_load(&mut self) -> Option<engine::AbandonedLoad> { self.abandoned_load.take() }

    pub fn park_release(&self, release: engine::PendingRelease) {
        let mut slot = self.pending_release.borrow_mut();
        debug_assert!(slot.is_none(), "a second off-frame release cannot exist");
        *slot = Some(release);
    }
    pub fn has_pending_release(&self) -> bool { self.pending_release.borrow().is_some() }
    pub fn take_finished_release(&self) -> Option<engine::PendingRelease> {
        let mut slot = self.pending_release.borrow_mut();
        if slot.as_ref().is_some_and(|p| p.is_finished()) { slot.take() } else { None }
    }
    pub fn take_pending_release_any(&self) -> Option<engine::PendingRelease> {
        self.pending_release.borrow_mut().take()
    }
}
