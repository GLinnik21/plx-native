//! Ordered, addressed delivery for a Watchlist stream job.
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use plx_machine::machine::Addr;
use crate::watchlist::Tag;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamEnd { Done, NotStarted, Panicked, Cancelled }
pub enum StreamEvent<T> { Post(T), End(StreamEnd) }

pub struct StreamLanding<T> {
    pub addr: Addr,
    pub tag: Tag,
    pub event: StreamEvent<T>,
    owed: Arc<AtomicBool>,
}
impl<T> StreamLanding<T> {
    pub fn ack_terminal(&self) -> bool {
        matches!(self.event, StreamEvent::End(_)) && self.owed.swap(false, Ordering::SeqCst)
    }
}

pub struct StreamMailbox<T>(Mutex<Vec<StreamLanding<T>>>);

impl<T> StreamMailbox<T> {
    pub fn take_not_started(&self) -> Vec<StreamLanding<T>> {
        let mut mail = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let (refused, rest) = std::mem::take(&mut *mail).into_iter().partition(|l| matches!(l.event, StreamEvent::End(StreamEnd::NotStarted)));
        *mail = rest;
        refused
    }
}
impl<T> Default for StreamMailbox<T> {
    fn default() -> Self { Self(Mutex::new(Vec::new())) }
}
impl<T> StreamMailbox<T> {
    pub fn take(&self) -> Vec<StreamLanding<T>> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
    pub fn has_mail(&self) -> bool {
        !self.0.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase { Queued, Started, Terminal }
struct Claim { phase: Phase, cancelled: bool }
pub struct StreamPort<T> {
    addr: Addr,
    tag: Tag,
    mailbox: Arc<StreamMailbox<T>>,
    claim: Arc<Mutex<Claim>>,
    owed: Arc<AtomicBool>,
}
impl<T> Clone for StreamPort<T> {
    fn clone(&self) -> Self {
        Self { addr: self.addr, tag: self.tag, mailbox: Arc::clone(&self.mailbox),
            claim: Arc::clone(&self.claim), owed: Arc::clone(&self.owed) }
    }
}
impl<T> StreamPort<T> {
    pub fn new(addr: Addr, tag: Tag, mailbox: Arc<StreamMailbox<T>>) -> Self {
        Self { addr, tag, mailbox, claim: Arc::new(Mutex::new(Claim { phase: Phase::Queued, cancelled: false })),
            owed: Arc::new(AtomicBool::new(true)) }
    }
    fn publish(&self, event: StreamEvent<T>) {
        self.mailbox.0.lock().unwrap_or_else(|e| e.into_inner()).push(StreamLanding {
            addr: self.addr, tag: self.tag, event, owed: Arc::clone(&self.owed),
        });

    }
    pub fn start(&self) -> Option<StreamProducer<T>> {
        let mut claim = self.claim.lock().unwrap_or_else(|e| e.into_inner());
        if claim.phase != Phase::Queued { return None; }
        claim.phase = Phase::Started;
        Some(StreamProducer { port: self.clone(), finished: false })
    }
    pub fn post(&self, addr: Addr, value: T) -> bool {
        if addr != self.addr { return false; }
        let claim = self.claim.lock().unwrap_or_else(|e| e.into_inner());
        if claim.phase != Phase::Started { return false; }
        self.publish(StreamEvent::Post(value));
        true
    }
    pub fn done(&self, addr: Addr, end: StreamEnd) {
        if end == StreamEnd::NotStarted { self.refused(addr); return; }
        if addr != self.addr { return; }
        let mut claim = self.claim.lock().unwrap_or_else(|e| e.into_inner());
        if claim.phase == Phase::Terminal { return; }
        claim.phase = Phase::Terminal;
        self.publish(StreamEvent::End(end));
    }
    pub fn refused(&self, addr: Addr) {
        if addr != self.addr { return; }
        let mut claim = self.claim.lock().unwrap_or_else(|e| e.into_inner());
        if claim.phase != Phase::Queued { return; }
        claim.phase = Phase::Terminal;
        self.publish(StreamEvent::End(StreamEnd::NotStarted));
    }
    pub fn cancel(&self) {
        let mut claim = self.claim.lock().unwrap_or_else(|e| e.into_inner());
        claim.cancelled = true;
        if claim.phase == Phase::Queued {
            claim.phase = Phase::Terminal;
            self.publish(StreamEvent::End(StreamEnd::Cancelled));
        }
    }
    pub fn is_cancelled(&self) -> bool {
        self.claim.lock().unwrap_or_else(|e| e.into_inner()).cancelled
    }
    pub fn is_owed(&self) -> bool { self.owed.load(Ordering::SeqCst) }
}
impl<T: Send + 'static> plx_machine::job::Port<StreamEnd> for StreamPort<T> {
    fn done(&self, addr: Addr, end: StreamEnd) { StreamPort::done(self, addr, end); }
    fn refused(&self, addr: Addr) { StreamPort::refused(self, addr); }
}

impl<T> std::fmt::Debug for StreamPort<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("StreamPort").field("addr", &self.addr).field("tag", &self.tag).finish() }
}
impl<T> PartialEq for StreamPort<T> {
    fn eq(&self, other: &Self) -> bool { Arc::ptr_eq(&self.claim, &other.claim) }
}
impl<T> StreamPort<T> {
    pub fn is_running(&self) -> bool { self.claim.lock().unwrap_or_else(|e| e.into_inner()).phase != Phase::Terminal }
}
pub struct StreamProducer<T> { port: StreamPort<T>, finished: bool }
impl<T> StreamProducer<T> {
    pub fn post(&self, addr: Addr, value: T) -> bool { self.port.post(addr, value) }
    pub fn is_cancelled(&self) -> bool { self.port.is_cancelled() }
    pub fn finish(mut self, end: StreamEnd) {
        self.port.done(self.port.addr, end);
        self.finished = true;
    }
}
impl<T> Drop for StreamProducer<T> {
    fn drop(&mut self) {
        if !self.finished {
            let end = if std::thread::panicking() { StreamEnd::Panicked } else { StreamEnd::Cancelled };
            self.port.done(self.port.addr, end);
        }
    }
}

#[cfg(test)]
#[path = "watchlist_stream_port_tests.rs"]
mod tests;
