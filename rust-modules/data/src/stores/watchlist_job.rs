//! Addressed one-shot Watchlist resolve jobs.
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use plx_machine::job::Port;
use plx_machine::machine::Addr;
use plx_plex::plex::{Client, ClientKeys, KeyedClients, ServerId};
use crate::watchlist::Tag;

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
pub struct ResolveLanding { pub addr: Addr, pub tag: Tag, pub sid: ServerId, pub rk: String, pub end: ResolveEnd }
#[derive(Default)]
pub struct ResolveMailbox(Mutex<Vec<ResolveLanding>>);
impl ResolveMailbox {
    pub fn take(&self) -> Vec<ResolveLanding> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect()
    }
}
struct Claim { state: AtomicU8, answered: AtomicBool }
#[derive(Clone)]
pub struct ResolvePort {
    addr: Addr, tag: Tag, sid: ServerId, rk: String,
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
        Self { addr, tag: req.tag, sid: req.sid, rk: req.rk.clone(), mailbox,
            claim: Arc::new(Claim { state: AtomicU8::new(0), answered: AtomicBool::new(false) }) }
    }
    /// Only the runner may start, and only while the owner has not cancelled the queued job.
    pub fn start(&self) -> bool {
        self.claim.state.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).is_ok()
    }
    pub fn cancel_queued(&self) {
        if self.claim.state.compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
            self.refused(self.addr);
        }
    }
}
impl Port<ResolveEnd> for ResolvePort {
    fn done(&self, addr: Addr, end: ResolveEnd) {
        if addr != self.addr { return; }
        // Retire a queued job before publishing its terminal; start competes on this same state.
        let _ = self.claim.state.compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst);
        if self.claim.answered.swap(true, Ordering::SeqCst) { return; }
        self.mailbox.0.lock().unwrap_or_else(|e| e.into_inner()).push(ResolveLanding {
            addr, tag: self.tag, sid: self.sid, rk: self.rk.clone(), end,
        });
        plx_machine::idle::invalidate();
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
