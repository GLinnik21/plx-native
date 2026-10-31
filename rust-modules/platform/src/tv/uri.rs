//! The television's URI-mode media service as a dumb transport (issue #1064, the first probe).
//!
//! `com.webos.media` (uMediaserver) loads a URL itself when it is asked over the platform bus:
//! `load {"uri": ..}`, then a stream of replies on the subscription, then `play`. This module is
//! only the PIPE to that service: it registers a client, sends one named method with a payload
//! string, and hands back what arrived. It reads no JSON, decides nothing and logs no payload;
//! what a reply MEANS is the probe machine's business (`plx_media::player::uriprobe`).
//!
//! **Nothing here blocks the caller.** [`UriMedia::open`], [`call`](UriMedia::call) and
//! [`close`](UriMedia::close) post a request to the transport's one worker and return; the worker
//! is the only thread that touches the bus handle. Every outcome, including a refused
//! registration and a call the bus would not send, comes back through [`poll`](UriMedia::poll) as a
//! [`UriReply`]:
//!
//! | `method` | `ok` | `payload` |
//! |---|---|---|
//! | `"register"` | the registration succeeded | the hub's own refusal text when it did not |
//! | the method sent (`"load"`, ...) | the bus delivered a message on that call | the message's JSON; or the hub's text when the call was not sent |
//! | `"dropped"` | `false` | how many replies were lost to a full queue since the last such reply |
//!
//! `ok` is the TRANSPORT's verdict, never the method's: a reply whose JSON says
//! `"returnValue": false` arrives with `ok: true` and the machine grades it.
//!
//! The whole module is the `devtriggers` surface (the probe is a developer test), so a release build
//! carries neither the trait nor the webOS worker behind it.

/// How the client names itself on the bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// `com.webos.media.client.plxprobe`: the name the Developer Mode role file grants outbound
    /// permission to `com.webos.media` (docs/distribution.md).
    Named,
    /// A plain anonymous client, the one registration the hub accepts from this process (`ls2`).
    Anon,
}

/// One message from the transport. `Debug` prints the method, the verdict and the payload's LENGTH
/// only: a payload can carry the media URL (an event echoing its `uri`), and a derive would write it
/// into whatever formats this value.
pub struct UriReply {
    pub method: &'static str,
    pub payload: String,
    pub ok: bool,
}

impl std::fmt::Debug for UriReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UriReply {{ method: {:?}, ok: {}, payload: <{} bytes> }}", self.method, self.ok, self.payload.len())
    }
}

/// The pipe. `Sync` because the installed port holds one `&'static dyn UriMedia`.
pub trait UriMedia: Sync {
    /// Ask the worker to register as `shape`. `Err` only when no worker could be started or one is
    /// already open; the registration's own outcome is the `"register"` reply of [`poll`](Self::poll).
    fn open(&self, shape: Shape) -> Result<(), String>;
    /// Queue `payload` for `luna://com.webos.media/<method>`. `Err` only when there is no open
    /// worker or the payload cannot be sent as text; the bus's answer, and its refusal to send, are
    /// replies. A call keeps its token: every later message on it arrives under the same `method`.
    fn call(&self, method: &'static str, payload: &str) -> Result<(), String>;
    /// Everything that has arrived since the last poll, oldest first. Never blocks.
    fn poll(&self) -> Vec<UriReply>;
    /// Ask the worker to unregister (uMediaserver unloads a pipeline whose load client leaves the
    /// bus) and end. Idempotent.
    fn close(&self);
}

/// The transport the installed port holds. Only `tv` reads the table; this is the `devtriggers` build's
/// second way out of it (the other is the sink), read by the URI-mode probe's driver.
pub fn installed() -> &'static dyn UriMedia { super::port().uri_media }

/// The bounded queue between a transport's worker (which pushes) and its poller (which drains).
/// A full queue drops the NEWEST reply and counts it; the count comes back as one `dropped` reply
/// at the end of the next drain, so a poller that fell behind says so instead of silently missing
/// messages.
pub struct ReplyQueue {
    sender: std::sync::mpsc::SyncSender<UriReply>,
    receiver: std::sync::Mutex<std::sync::mpsc::Receiver<UriReply>>,
    dropped: std::sync::atomic::AtomicU64,
}

impl ReplyQueue {
    /// A queue that holds `depth` replies.
    pub fn new(depth: usize) -> ReplyQueue {
        let (sender, receiver) = std::sync::mpsc::sync_channel(depth);
        ReplyQueue { sender, receiver: std::sync::Mutex::new(receiver), dropped: std::sync::atomic::AtomicU64::new(0) }
    }

    /// Queue one reply, or count it dropped. Never blocks.
    pub fn push(&self, method: &'static str, payload: String, ok: bool) {
        if self.sender.try_send(UriReply { method, payload, ok }).is_err() {
            self.dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Everything queued, oldest first, then the `dropped` reply if any were lost.
    pub fn drain(&self) -> Vec<UriReply> {
        let receiver = self.receiver.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut replies: Vec<UriReply> = receiver.try_iter().collect();
        let dropped = self.dropped.swap(0, std::sync::atomic::Ordering::Relaxed);
        if dropped > 0 {
            replies.push(UriReply { method: "dropped", payload: dropped.to_string(), ok: false });
        }
        replies
    }
}

/// No bus: host and test builds, and a port that was never installed.
pub struct NoUriMedia;

impl UriMedia for NoUriMedia {
    fn open(&self, _shape: Shape) -> Result<(), String> { Err("no bus".to_owned()) }
    fn call(&self, _method: &'static str, _payload: &str) -> Result<(), String> { Err("no bus".to_owned()) }
    fn poll(&self) -> Vec<UriReply> { Vec::new() }
    fn close(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_prints_the_length_of_a_payload_never_its_text() {
        let reply = UriReply {
            method: "load",
            payload: r#"{"uri":"http://192.168.1.5:32400/video?X-Plex-Token=SECRETTOKEN"}"#.to_owned(),
            ok: true,
        };
        let shown = format!("{reply:?}");
        assert!(shown.contains("load") && shown.contains("true") && shown.contains("bytes"), "{shown}");
        for leak in ["192.168", "SECRETTOKEN", "X-Plex-Token", "http"] {
            assert!(!shown.contains(leak), "Debug leaked {leak:?}: {shown}");
        }
    }

    #[test]
    fn no_uri_media_answers_no_bus_and_yields_nothing() {
        let none = NoUriMedia;
        assert_eq!(none.open(Shape::Named), Err("no bus".to_owned()));
        assert_eq!(none.open(Shape::Anon), Err("no bus".to_owned()));
        assert_eq!(none.call("load", "{}"), Err("no bus".to_owned()));
        assert!(none.poll().is_empty());
        none.close();
    }

    #[test]
    fn a_full_queue_drops_the_newest_and_reports_the_count_once() {
        let queue = ReplyQueue::new(2);
        for n in 0..5 {
            queue.push("load", n.to_string(), true);
        }
        let drained = queue.drain();
        let shown: Vec<(&str, &str, bool)> = drained.iter().map(|r| (r.method, r.payload.as_str(), r.ok)).collect();
        assert_eq!(shown, [("load", "0", true), ("load", "1", true), ("dropped", "3", false)]);
        assert!(queue.drain().is_empty(), "the count is reported once");
        queue.push("play", String::new(), true);
        assert_eq!(queue.drain().len(), 1, "room again after a drain");
    }

    #[test]
    fn an_uninstalled_port_hands_out_the_no_bus_transport() {
        assert_eq!(installed().open(Shape::Anon), Err("no bus".to_owned()));
    }
}
