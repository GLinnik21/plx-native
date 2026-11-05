//! Nonblocking, addressed transport to the developer URI probe. Payloads stay opaque here.
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape { Named, Anon }

/// A registration's identity. A stale owner cannot address its successor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionId(u64);
impl SessionId { pub const fn new(value: u64) -> Self { Self(value) } }

/// One call, not a subscription event's causal identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RequestId(u64);
impl RequestId { pub const fn new(value: u64) -> Self { Self(value) } }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseStatus { Open, Closing, Released, Quarantined }

/// Sticky release evidence, separate from the bounded reply stream.
pub struct CloseSignal(AtomicU8);
impl Default for CloseSignal { fn default() -> Self { Self::new() } }
impl CloseSignal {
    pub const fn new() -> Self { Self(AtomicU8::new(0)) }
    pub fn status(&self) -> CloseStatus {
        match self.0.load(Ordering::Acquire) {
            0 => CloseStatus::Open, 1 => CloseStatus::Closing, 2 => CloseStatus::Released, _ => CloseStatus::Quarantined,
        }
    }
    /// True once only: the caller which must post Close to the worker.
    pub fn request_close(&self) -> bool { self.0.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_ok() }
    /// Only actual unregister success (or registration failure without a live handle) releases.
    pub fn finish(&self, released: bool) {
        let _ = self.0.try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            (current != 2).then_some(if released { 2 } else { 3 })
        });
    }
}

/// The real worker's slot. Closing and quarantined occupants cannot be replaced.
pub struct SessionSlot<T> { next: u64, current: Option<(SessionId, T)> }
impl<T> Default for SessionSlot<T> { fn default() -> Self { Self::new() } }
impl<T> SessionSlot<T> {
    pub const fn new() -> Self { Self { next: 0, current: None } }
    pub fn open(&mut self, released: impl FnOnce(&T) -> bool, create: impl FnOnce(SessionId) -> Result<T, String>) -> Result<SessionId, String> {
        if self.current.as_ref().is_some_and(|(_, value)| !released(value)) { return Err("registration occupied".to_owned()); }
        self.next = self.next.checked_add(1).ok_or_else(|| "session identities exhausted".to_owned())?;
        let id = SessionId(self.next);
        let value = create(id)?;
        self.current = Some((id, value));
        Ok(id)
    }
    pub fn get(&self, id: SessionId) -> Result<&T, String> {
        self.current.as_ref().filter(|(current, _)| *current == id).map(|(_, value)| value).ok_or_else(|| "stale session".to_owned())
    }
}

pub struct UriReply { pub session: SessionId, pub request: RequestId, pub method: &'static str, pub payload: String, pub ok: bool }
impl std::fmt::Debug for UriReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let method = match self.method {
            "register" | "load" | "subscribe" | "play" | "pause" | "seek" | "selectTrack" | "setSubtitleEnable" | "setWindowId" | "unload" | "dropped" => self.method,
            _ => "other",
        };
        f.debug_struct("UriReply").field("method", &method).field("session", &self.session).field("request", &self.request).field("ok", &self.ok).field("payload_bytes", &self.payload.len()).finish()
    }
}
pub struct UriPoll { pub replies: Vec<UriReply>, pub close: CloseStatus }

pub trait UriMedia: Sync {
    fn open(&self, shape: Shape) -> Result<SessionId, String>;
    /// Success means queued, never that the bus sent it or the service accepted it.
    fn call(&self, session: SessionId, request: RequestId, method: &'static str, payload: &str) -> Result<(), String>;
    fn poll(&self, session: SessionId) -> Result<UriPoll, String>;
    fn close(&self, session: SessionId) -> Result<(), String>;
}
pub fn installed() -> &'static dyn UriMedia { super::port().uri_media }

pub struct ReplyQueue {
    session: SessionId,
    sender: std::sync::mpsc::SyncSender<UriReply>,
    receiver: std::sync::Mutex<std::sync::mpsc::Receiver<UriReply>>,
    dropped: std::sync::atomic::AtomicU64,
}
impl ReplyQueue {
    pub fn new(session: SessionId, depth: usize) -> Self {
        let (sender, receiver) = std::sync::mpsc::sync_channel(depth);
        Self { session, sender, receiver: std::sync::Mutex::new(receiver), dropped: std::sync::atomic::AtomicU64::new(0) }
    }
    pub fn push(&self, request: RequestId, method: &'static str, payload: String, ok: bool) {
        if self.sender.try_send(UriReply { session: self.session, request, method, payload, ok }).is_err() { self.dropped.fetch_add(1, Ordering::Relaxed); }
    }
    pub fn drain(&self) -> Vec<UriReply> {
        let receiver = self.receiver.lock().unwrap_or_else(|p| p.into_inner());
        let mut replies: Vec<_> = receiver.try_iter().collect();
        let dropped = self.dropped.swap(0, Ordering::Relaxed);
        if dropped > 0 { replies.push(UriReply { session: self.session, request: RequestId(0), method: "dropped", payload: dropped.to_string(), ok: false }); }
        replies
    }
}
pub struct NoUriMedia;
impl UriMedia for NoUriMedia {
    fn open(&self, _shape: Shape) -> Result<SessionId, String> { Err("no bus".to_owned()) }
    fn call(&self, _session: SessionId, _request: RequestId, _method: &'static str, _payload: &str) -> Result<(), String> { Err("no bus".to_owned()) }
    fn poll(&self, _session: SessionId) -> Result<UriPoll, String> { Err("no bus".to_owned()) }
    fn close(&self, _session: SessionId) -> Result<(), String> { Err("no bus".to_owned()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    const REQUEST: RequestId = RequestId::new(1);
    #[test]
    fn uri_slot_retains_closing_and_quarantine_and_rejects_stale_owners() {
        let mut slot = SessionSlot::new();
        let first = slot.open(|s: &CloseSignal| s.status() == CloseStatus::Released, |_| Ok(CloseSignal::new())).unwrap();
        let signal = slot.get(first).unwrap();
        assert!(signal.request_close()); assert!(!signal.request_close());
        assert_eq!(signal.status(), CloseStatus::Closing);
        assert!(slot.open(|s| s.status() == CloseStatus::Released, |_| Ok(CloseSignal::new())).is_err());
        slot.get(first).unwrap().finish(false);
        assert!(slot.open(|s| s.status() == CloseStatus::Released, |_| Ok(CloseSignal::new())).is_err());
        slot.get(first).unwrap().finish(true);
        let second = slot.open(|s| s.status() == CloseStatus::Released, |_| Ok(CloseSignal::new())).unwrap();
        assert_ne!(first, second); assert!(slot.get(first).is_err());
        assert_eq!(slot.get(second).unwrap().status(), CloseStatus::Open);
    }
    #[test]
    fn uri_terminal_status_survives_queue_overflow() {
        let signal = CloseSignal::new();
        let queue = ReplyQueue::new(SessionId::new(1), 1);
        queue.push(REQUEST, "load", "one".into(), true);
        queue.push(REQUEST, "load", "two".into(), true);
        assert!(signal.request_close()); signal.finish(true);
        let poll = UriPoll { replies: queue.drain(), close: signal.status() };
        assert_eq!(poll.close, CloseStatus::Released);
        assert_eq!(poll.replies.len(), 2); assert_eq!(poll.replies[1].method, "dropped");
        assert!(queue.drain().is_empty()); assert_eq!(signal.status(), CloseStatus::Released);
    }
    #[test]
    fn uri_debug_hides_payload_and_arbitrary_method_text() {
        let reply = UriReply { session: SessionId::new(1), request: REQUEST, method: "SECRETMETHOD", payload: "http://host/?token=SECRET".into(), ok: true };
        let shown = format!("{reply:?}");
        assert!(shown.contains("true"));
        for secret in ["http", "host", "token", "SECRET"] { assert!(!shown.contains(secret), "{shown}"); }
    }
    #[test]
    fn no_uri_media_answers_no_bus() {
        assert_eq!(NoUriMedia.open(Shape::Named), Err("no bus".into()));
        assert!(NoUriMedia.call(SessionId::new(1), REQUEST, "load", "{}").is_err());
        assert!(NoUriMedia.poll(SessionId::new(1)).is_err());
        assert!(NoUriMedia.close(SessionId::new(1)).is_err());
        assert_eq!(installed().open(Shape::Anon), Err("no bus".into()));
    }
}
