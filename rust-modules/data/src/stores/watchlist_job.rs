//! Addressed one-shot Watchlist resolve jobs.
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU8, Ordering};
use plx_machine::job::Port;
use plx_machine::machine::Addr;
use plx_plex::plex::{Client, ClientKeys, KeyedClients, ServerId};
use crate::watchlist::Tag;

#[path = "watchlist_stream_port.rs"]
pub mod stream;
#[path = "watchlist_ports.rs"]
pub mod ports;
#[path = "watchlist_stream_job.rs"]
mod streaming;
pub use streaming::*;

#[derive(Clone, PartialEq)]
pub struct ResolveReq {
    pub tag: Tag,
    pub keys: ClientKeys,
    pub sid: ServerId,
    pub rk: String,
}
impl std::fmt::Debug for ResolveReq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolveReq").field("tag", &self.tag).finish_non_exhaustive()
    }
}

pub enum ResolveEnd { Answer(Option<Option<[u8; 12]>>), NotStarted, Panicked }
pub struct ResolveLanding { pub addr: Addr, pub tag: Tag, pub sid: ServerId, pub rk: String, pub key: Option<plx_plex::plex::ClientKey>, pub end: ResolveEnd }
#[derive(Default)]
pub struct ResolveMailbox(Mutex<Vec<ResolveLanding>>);

impl ResolveMailbox {
    pub fn take_not_started(&self) -> Vec<ResolveLanding> {
        let mut mail = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let (refused, rest) = std::mem::take(&mut *mail).into_iter().partition(|l| matches!(l.end, ResolveEnd::NotStarted));
        *mail = rest;
        refused
    }
}
impl ResolveMailbox {
    pub fn take(&self) -> Vec<ResolveLanding> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect()
    }
}
struct Claim { state: AtomicU8 }
#[derive(Clone)]
pub struct ResolvePort {
    addr: Addr, tag: Tag, sid: ServerId, rk: String, key: Option<plx_plex::plex::ClientKey>,
    mailbox: Arc<ResolveMailbox>, claim: Arc<Claim>,
}
impl std::fmt::Debug for ResolvePort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("ResolvePort").field("addr", &self.addr).finish() }
}
impl PartialEq for ResolvePort {
    fn eq(&self, other: &Self) -> bool { Arc::ptr_eq(&self.claim, &other.claim) }
}
impl ResolvePort {
    pub fn new(addr: Addr, req: &ResolveReq, mailbox: Arc<ResolveMailbox>) -> Self {
        Self { addr, tag: req.tag, sid: req.sid, rk: req.rk.clone(), key: req.keys.key(req.sid), mailbox,
            claim: Arc::new(Claim { state: AtomicU8::new(0) }) }
    }
    /// Only the runner may start, and only while the owner has not cancelled the queued job.
    pub fn start(&self) -> bool {
        self.claim.state.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).is_ok()
    }
    pub fn cancel_queued(&self) {
        self.refused(self.addr);
    }
}
impl Port<ResolveEnd> for ResolvePort {
    fn done(&self, addr: Addr, end: ResolveEnd) {
        if addr != self.addr { return; }
        // Refusal can win only before entry; an entered worker owns its answer.
        let retired = match &end {
            ResolveEnd::NotStarted => self.claim.state.compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst),
            _ => match self.claim.state.compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(state) => Ok(state),
                Err(0) => self.claim.state.compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst),
                Err(state) => Err(state),
            },
        };
        if retired.is_err() { return; }
        self.mailbox.0.lock().unwrap_or_else(|e| e.into_inner()).push(ResolveLanding {
            addr, tag: self.tag, sid: self.sid, rk: self.rk.clone(), key: self.key, end,
        });

    }
    fn refused(&self, addr: Addr) { self.done(addr, ResolveEnd::NotStarted); }
}

/// The host supplies the registry lookup. KeyedClients rechecks the captured full key.
pub fn finish_resolve_job(port: &ResolvePort, addr: Addr, req: ResolveReq,
    lookup: fn(ServerId) -> Option<&'static Client>) {
    let end = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let clients = KeyedClients::new(&req.keys, lookup);
        let guid = clients.client(req.sid).and_then(|client| client.metadata(&req.rk)).map(|m| m.guid);
        ResolveEnd::Answer(guid.map(|g| plx_plex::plex::resolve_catalog_hex(None, || Some(g))))
    })).unwrap_or(ResolveEnd::Panicked);
    port.done(addr, end);
}

/// An owned write claim. Credentials are captured when the store makes the decision.
pub struct WriteReq {
    pub tag: Tag,
    pub auth: crate::watchlist::WatchlistAuth,
    pub write: crate::watchlist::ledger::PendingWrite,
}
impl std::fmt::Debug for WriteReq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteReq").field("tag", &self.tag).finish_non_exhaustive()
    }
}
impl PartialEq for WriteReq {
    fn eq(&self, other: &Self) -> bool {
        self.tag == other.tag && self.write == other.write
            && self.auth.generation == other.auth.generation
            && self.auth.client_id == other.auth.client_id && self.auth.credential == other.auth.credential
            && self.auth.user.id == other.auth.user.id && self.auth.user.uuid == other.auth.user.uuid
    }
}

pub enum WriteEnd {
    Written { write: crate::watchlist::ledger::PendingWrite, outcome: crate::watchlist::ledger::WriteOutcome },
    NotStarted { write: crate::watchlist::ledger::PendingWrite },
}
pub struct WriteLanding { pub addr: Addr, pub tag: Tag, pub end: WriteEnd }
#[derive(Default)]
pub struct WriteMailbox(Mutex<Vec<WriteLanding>>);

impl WriteMailbox {
    pub fn take_not_started(&self) -> Vec<WriteLanding> {
        let mut mail = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let (refused, rest) = std::mem::take(&mut *mail).into_iter().partition(|l| matches!(l.end, WriteEnd::NotStarted { .. }));
        *mail = rest;
        refused
    }
}
impl WriteMailbox {
    pub fn has_mail(&self) -> bool {
        !self.0.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
    }
    pub fn take(&self) -> Vec<WriteLanding> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect()
    }
}

/// State 0 is queued, 1 started, 2 terminal. Start and every terminal compete on
/// this one atomic, so a refusal cannot release a write that has reached a worker.
#[derive(Clone)]
pub struct WritePort {
    addr: Addr, tag: Tag, write: crate::watchlist::ledger::PendingWrite,
    mailbox: Arc<WriteMailbox>, state: Arc<AtomicU8>,
}
impl std::fmt::Debug for WritePort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WritePort").field("addr", &self.addr).finish()
    }
}
impl PartialEq for WritePort {
    fn eq(&self, other: &Self) -> bool { Arc::ptr_eq(&self.state, &other.state) }
}
impl WritePort {
    pub fn new(addr: Addr, req: &WriteReq, mailbox: Arc<WriteMailbox>) -> Self {
        Self { addr, tag: req.tag, write: req.write, mailbox, state: Arc::new(AtomicU8::new(0)) }
    }
    pub fn start(&self) -> bool {
        self.state.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).is_ok()
    }
    pub fn cancel_queued(&self) { self.refused(self.addr); }
}
impl Port<WriteEnd> for WritePort {
    fn done(&self, addr: Addr, end: WriteEnd) {
        if addr != self.addr { return; }
        let previous = match &end {
            WriteEnd::NotStarted { write } if *write == self.write =>
                self.state.compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst),
            WriteEnd::Written { write, .. } if *write == self.write => {
                match self.state.compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst) {
                    Ok(state) => Ok(state),
                    Err(0) => self.state.compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst),
                    Err(state) => Err(state),
                }
            }
            _ => return,
        };
        if previous.is_err() { return; }
        self.mailbox.0.lock().unwrap_or_else(|e| e.into_inner()).push(WriteLanding { addr, tag: self.tag, end });

    }
    fn refused(&self, addr: Addr) { self.done(addr, WriteEnd::NotStarted { write: self.write }); }
}

/// Runs only after `start` wins. A panic may have reached the wire and is a failed write.
pub fn finish_write_job(port: &WritePort, addr: Addr, req: WriteReq, io: &dyn crate::watchlist::WatchlistIo) {
    use crate::watchlist::ledger::WriteOutcome;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if io.write(&req.auth, &crate::watchlist::encode_hex(&req.write.hex), req.write.intent).is_ok() {
            WriteOutcome::Landed
        } else {
            WriteOutcome::Failed
        }
    })).unwrap_or(WriteOutcome::Failed);
    port.done(addr, WriteEnd::Written { write: req.write, outcome });
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_machine::machine::{MachineId, RequestId};
    use crate::stores::StoreId;

    fn fixture() -> (ResolveReq, ResolvePort, Addr, Arc<ResolveMailbox>) {
        let tag = Tag { profile_gen: 9, epoch: 4, req: 7 };
        let addr = Addr { to: MachineId::Store(StoreId::Watchlist.ord()), req: RequestId(tag.req) };
        let req = ResolveReq { tag, keys: ClientKeys::default(), sid: ServerId::from_raw(0), rk: "7".into() };
        let mailbox = Arc::new(ResolveMailbox::default());
        let port = ResolvePort::new(addr, &req, Arc::clone(&mailbox));
        (req, port, addr, mailbox)
    }

    fn write_fixture() -> (WriteReq, WritePort, Addr, Arc<WriteMailbox>) {
        use crate::watchlist::ledger::{Intent, PendingWrite};
        let tag = Tag { profile_gen: 1, epoch: 2, req: 3 };
        let addr = Addr { to: MachineId::Store(StoreId::Watchlist.ord()), req: RequestId(tag.req) };
        let req = WriteReq { tag, auth: crate::watchlist::WatchlistInput::for_test(1).auth().unwrap().clone(),
            write: PendingWrite { hex: [7; 12], intent: Intent::Add, seq: 42 } };
        let mailbox = Arc::new(WriteMailbox::default());
        let port = WritePort::new(addr, &req, Arc::clone(&mailbox));
        (req, port, addr, mailbox)
    }

    #[test]
    fn write_wrong_address_does_not_retire_and_started_beats_cancel() {
        let (req, port, addr, mailbox) = write_fixture();
        let wrong = Addr { to: addr.to, req: RequestId(addr.req.0 + 1) };
        port.refused(wrong);
        assert!(port.start());
        port.cancel_queued();
        assert!(mailbox.take().is_empty());
        port.done(addr, WriteEnd::Written { write: req.write, outcome: crate::watchlist::ledger::WriteOutcome::Failed });
        port.refused(addr);
        let answers = mailbox.take();
        assert_eq!(answers.len(), 1);
        assert!(matches!(answers[0].end, WriteEnd::Written { outcome: crate::watchlist::ledger::WriteOutcome::Failed, .. }));
    }

    #[test]
    fn write_cancel_or_terminal_before_start_prevents_worker() {
        let (req, port, _addr, mailbox) = write_fixture();
        port.cancel_queued();
        assert!(!port.start());
        assert!(matches!(mailbox.take().pop().unwrap().end, WriteEnd::NotStarted { write } if write == req.write));
        let (req, port, addr, mailbox) = write_fixture();
        port.done(addr, WriteEnd::Written { write: req.write, outcome: crate::watchlist::ledger::WriteOutcome::Failed });
        assert!(!port.start());
        assert_eq!(mailbox.take().len(), 1);
    }

    #[test]
    fn resolve_port_answers_once_and_rejects_wrong_address() {
        let (_, port, addr, mailbox) = fixture();
        let wrong = Addr { to: addr.to, req: RequestId(addr.req.0 + 1) };
        port.done(wrong, ResolveEnd::Answer(Some(None)));
        assert!(mailbox.take().is_empty());
        assert!(port.start());
        port.done(addr, ResolveEnd::Answer(Some(None)));
        port.refused(addr);
        let mut answers = mailbox.take();
        assert_eq!(answers.len(), 1);
        assert!(matches!(answers.pop().unwrap().end, ResolveEnd::Answer(Some(None))));
    }

    #[test]
    fn resolve_refusal_after_worker_entry_cannot_preempt_answer() {
        let (_, port, addr, mailbox) = fixture();
        assert!(port.start());
        port.refused(addr);
        port.cancel_queued();
        assert!(mailbox.take().is_empty());
        port.done(addr, ResolveEnd::Answer(Some(None)));
        let answers = mailbox.take();
        assert_eq!(answers.len(), 1);
        assert!(matches!(answers[0].end, ResolveEnd::Answer(Some(None))));
    }

    #[test]
    fn resolve_refused_before_start_cannot_start() {
        let (_, port, addr, mailbox) = fixture();
        port.refused(addr);
        assert!(!port.start(), "a refused queued job must never start");
        assert_eq!(mailbox.take().len(), 1);
    }

    #[test]
    fn resolve_answer_before_start_cannot_start() {
        let (_, port, addr, mailbox) = fixture();
        port.done(addr, ResolveEnd::Answer(Some(None)));
        assert!(!port.start(), "a terminal queued job must never start");
        assert_eq!(mailbox.take().len(), 1);
    }

    #[test]
    fn resolve_cancel_before_start_cannot_start_and_answers_once() {
        let (_, port, addr, mailbox) = fixture();
        port.cancel_queued();
        assert!(!port.start());
        port.refused(addr);
        let answers = mailbox.take();
        assert_eq!(answers.len(), 1);
        assert!(matches!(answers[0].end, ResolveEnd::NotStarted));
    }

    #[test]
    fn resolve_worker_panic_answers_once() {
        let (mut req, port, addr, mailbox) = fixture();
        req.keys = ClientKeys::from_keys([(req.sid, plx_plex::plex::ClientKey { instance: 1, token: 2, grant: 3 })]);
        assert!(port.start());
        finish_resolve_job(&port, addr, req, |_| panic!("scripted lookup panic"));
        port.refused(addr);
        let answers = mailbox.take();
        assert_eq!(answers.len(), 1);
        assert!(matches!(answers[0].end, ResolveEnd::Panicked));
    }

    #[test]
    fn resolve_missing_key_is_unanswered() {
        let (req, port, addr, mailbox) = fixture();
        assert!(port.start());
        finish_resolve_job(&port, addr, req, |_| panic!("lookup without a captured key"));
        let answers = mailbox.take();
        assert_eq!(answers.len(), 1);
        assert!(matches!(answers[0].end, ResolveEnd::Answer(None)));
    }
}
