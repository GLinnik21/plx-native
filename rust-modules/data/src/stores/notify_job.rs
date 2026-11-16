//! Addressed Notify stream channel and off-frame body. Scheduling belongs to the app.
use plx_machine::{job::Port, machine::Addr};
use plx_plex::plex::{Client, ClientKey, ServerId, notify::{Notification, NotifyEnd}};
use plx_base::task::OffFrame;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};

#[derive(Clone, Debug, PartialEq)]
pub struct NotifyStreamReq { pub sid: ServerId, pub key: ClientKey }
#[derive(Debug)]
pub enum NotifyTerminal { End(NotifyEnd), NotStarted }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotifyAdmission { pub addr: Addr, pub origin_frame: u64, pub accepted: bool }

#[derive(Default)]
pub(super) struct Inbox {
    pub events: Vec<(ClientKey, Notification)>,
    pub end: Option<NotifyEnd>,
    terminal_sent: bool,
    admission: Option<NotifyAdmission>,
    admission_sent: bool,
}
#[derive(Clone)]
pub(super) struct Link {
    pub addr: Addr, pub key: ClientKey, origin_frame: u64,
    cancel: Arc<AtomicBool>, inbox: Arc<Mutex<Inbox>>,
}
impl Link {
    pub fn new(addr: Addr, key: ClientKey, origin_frame: u64) -> Self {
        Self { addr, key, origin_frame, cancel: Arc::default(), inbox: Arc::default() }
    }
    pub fn lock(&self) -> std::sync::MutexGuard<'_, Inbox> {
        self.inbox.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn take_admission(&self) -> Option<NotifyAdmission> { self.lock().admission.take() }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
        self.finish(NotifyEnd::Cancelled);
    }
    pub fn is_cancelled(&self) -> bool { self.cancel.load(Ordering::Acquire) }
    pub fn push(&self, n: Notification) {
        let mut inbox = self.lock();
        if !inbox.terminal_sent { inbox.events.push((self.key, n)); }
    }
    pub fn finish(&self, end: NotifyEnd) { Self::finish_locked(&mut self.lock(), end); }
    fn finish_locked(inbox: &mut Inbox, end: NotifyEnd) {
        if !inbox.terminal_sent { inbox.terminal_sent = true; inbox.end = Some(end); }
    }
}
#[derive(Clone)]
pub struct NotifyStreamPort { pub(super) link: Link }
impl std::fmt::Debug for NotifyStreamPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotifyStreamPort").field("addr", &self.link.addr).field("key", &self.link.key).finish()
    }
}
impl PartialEq for NotifyStreamPort {
    fn eq(&self, other: &Self) -> bool { self.link.addr == other.link.addr && self.link.key == other.link.key }
}
impl NotifyStreamPort {
    pub(super) fn new(link: Link) -> Self { Self { link } }
    pub fn is_cancelled(&self) -> bool { self.link.is_cancelled() }
    pub fn cancel_flag(&self) -> &AtomicBool { &self.link.cancel }
    pub fn post(&self, addr: Addr, n: Notification) {
        if addr == self.link.addr { self.link.push(n); }
    }
    /// INITIAL scheduling receipt only. Held acceptance is final, even after take.
    /// A later release failure must use done(NotStarted), never a second receipt.
    pub fn admit(&self, addr: Addr, accepted: bool) {
        if addr != self.link.addr { return; }
        let mut inbox = self.link.lock();
        if inbox.admission_sent { return; }
        inbox.admission_sent = true;
        inbox.admission = Some(NotifyAdmission { addr, origin_frame: self.link.origin_frame, accepted });
        if !accepted {
            Link::finish_locked(&mut inbox, if self.is_cancelled() { NotifyEnd::Cancelled } else { NotifyEnd::Transport });
        }
    }
}
impl Port<NotifyTerminal> for NotifyStreamPort {
    fn done(&self, addr: Addr, out: NotifyTerminal) {
        if addr != self.link.addr { return; }
        self.link.finish(match out { NotifyTerminal::End(end) => end, NotifyTerminal::NotStarted => NotifyEnd::Transport });
    }
    fn refused(&self, addr: Addr) { self.admit(addr, false); }
}

pub fn finish_notify_stream_job(port: &NotifyStreamPort, addr: Addr, client: &'static Client,
    req: NotifyStreamReq, off: &OffFrame) {
    if addr != port.link.addr || req.key != port.link.key { return; }
    let end = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if port.is_cancelled() { return NotifyEnd::Cancelled; }
        if client.id() != req.sid || ClientKey::of(client) != req.key || !client.has_token()
            || client.data_io_disabled() || !client.credential_transport_open() {
            return NotifyEnd::Transport;
        }
        client.notifications(off, port.cancel_flag(), &mut |n| port.post(addr, n))
    })).unwrap_or(NotifyEnd::Transport);
    port.done(addr, NotifyTerminal::End(end));
}
