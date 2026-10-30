//! Live updates: Jellyfin's WebSocket, as the app's server-update feed.
//!
//! The data layer keeps one feed per server open (`plx_data::stores::notify`) and turns what it
//! reports into quiet re-reads of the items the screens hold (`stores::notify_map`). On Plex the
//! feed is the server's event stream (`plx_plex::plex::notify`); here it is `/socket`, read with
//! [`socket`](super::socket) over plain TCP or over a TLS connection libcurl holds open
//! (`plx_net::net::connect_tls`), and each message the app can use becomes one of the feed's
//! sessions:
//!
//! * `UserDataChanged` for this user (a watched mark, a resume point, a play count, from any
//!   device, this one included) reports each changed item `stopped`, which re-reads it where it is
//!   held and refreshes Continue Watching.
//! * `LibraryChanged` reports the items added, removed and updated (the first few of each) as
//!   the library changes they are, which re-read the server's Home, shelves and library grids. An
//!   update counts: an added item is listed only once its scan finishes, which Jellyfin reports so.
//! * `UserUpdated` for this user (a name or avatar edited on any device) has the backend read the
//!   user again and swap in the new name and avatar (`backend::refresh_signed_in_user`); it adds
//!   nothing to the feed.
//! * `ForceKeepAlive` sets how often this client says `KeepAlive`, which the server needs to keep
//!   the connection; every message, the server's `KeepAlive` answers included, is a ping.
//!
//! The feed is a hint, never a playback event of ours (the data layer's rule), so nothing here
//! touches a timeline or a stop. Each report gets its own session key: the map keeps the last state
//! per session, and two changes to one item would otherwise read as no news.

use super::ids::normalize;
use super::socket::{self, Conn, Frame, Noise, Reader};
use plx_plex::plex::notify::{LibraryEntry, LibraryKind, LibraryState, Notification, NotifyEnd, PlaySession, PlayState};
use plx_net::net::connect::TlsSocket;
use plx_net::net::ConnectEnd;
use plx_plex::plex::Origin;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How often to say `KeepAlive` until the server asks for a rate (`ForceKeepAlive`).
const KEEPALIVE: Duration = Duration::from_secs(30);

/// A connection that has heard nothing for this long past its keep-alive is gone.
const IDLE_MARGIN: Duration = Duration::from_secs(20);

/// How many items of each kind (added, removed, updated) one `LibraryChanged` reports; a scan can
/// name thousands, and any one entry already re-lists the server's pages.
const LIBRARY_IDS: usize = 64;

/// How long the TCP connect and each write may take.
const CONNECT: Duration = Duration::from_secs(5);

/// How long a read waits before the loop looks at its cancel flag and its timers.
const READ_TICK: Duration = Duration::from_millis(250);

/// Where the socket is and what it is opened with.
pub struct Target {
    pub origin: Origin,
    /// `/socket` under the server's base path.
    pub path: String,
    /// The `Authorization` header that signs the upgrade in.
    pub headers: Vec<String>,
    /// Whose changes are this user's.
    pub user_id: String,
}

/// What one message from the server means to the feed.
#[derive(Default)]
pub struct Messages {
    user_id: String,
    seq: u64,
}

/// One message, read: what to tell the feed, and a keep-alive rate the server asked for.
#[derive(Debug, PartialEq, Eq)]
pub struct Read {
    pub notification: Notification,
    pub keepalive: Option<Duration>,
    /// The server says this user was edited (`UserUpdated`): a name or avatar may have changed.
    pub user_updated: bool,
}

impl Read {
    fn of(notification: Notification) -> Self {
        Read { notification, keepalive: None, user_updated: false }
    }
}

impl Messages {
    pub fn new(user_id: &str) -> Self {
        Messages { user_id: normalize(user_id), seq: 0 }
    }

    pub fn read(&mut self, text: &str) -> Read {
        let ping = Read::of(Notification::Ping);
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return ping };
        let data = &v["Data"];
        let kind = v["MessageType"].as_str().unwrap_or("");
        let mine = |key: &str| normalize(data[key].as_str().unwrap_or("")) == self.user_id;
        let changed: Vec<String> = match kind {
            "ForceKeepAlive" => {
                let secs = data.as_u64().unwrap_or(60).max(10);
                return Read { keepalive: Some(Duration::from_secs(secs / 2)), ..ping };
            }
            "UserDataChanged" if mine("UserId") => data["UserDataList"]
                .as_array()
                .map(|list| list.iter().filter_map(|d| d["ItemId"].as_str()).map(normalize).collect())
                .unwrap_or_default(),
            "LibraryChanged" => return library(data),
            "UserUpdated" if mine("Id") => {
                return Read { user_updated: true, ..ping };
            }
            _ => return ping,
        };
        let sessions: Vec<PlaySession> = changed
            .into_iter()
            .filter(|id| !id.is_empty())
            .map(|id| {
                self.seq += 1;
                PlaySession {
                    session_key: Some(format!("jf-{}", self.seq)),
                    client_identifier: None,
                    rating_key: Some(id),
                    key: None,
                    view_offset_ms: None,
                    state: PlayState::Stopped,
                }
            })
            .collect();
        if sessions.is_empty() {
            return ping;
        }
        Read::of(Notification::Play(sessions))
    }
}

/// Items added, removed and updated (the first [`LIBRARY_IDS`] of each), as the library changes
/// they are. Jellyfin names no kind. An update moves the lists too: an item Jellyfin reports added
/// may not be listed until its scan finishes, which it reports as an update.
fn library(data: &serde_json::Value) -> Read {
    let count = |k: &str| data[k].as_array().map_or(0, Vec::len);
    plx_base::eventlog::log(&format!(
        "jellyfin: library changed added={} removed={} updated={}",
        count("ItemsAdded"),
        count("ItemsRemoved"),
        count("ItemsUpdated")
    ));
    let entries: Vec<LibraryEntry> = [("ItemsAdded", LibraryState::Created), ("ItemsRemoved", LibraryState::Deleted), ("ItemsUpdated", LibraryState::Updated)]
        .into_iter()
        .flat_map(|(k, state)| {
            data[k]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str())
                .map(normalize)
                .filter(|id| !id.is_empty())
                .take(LIBRARY_IDS)
                .map(move |item_id| LibraryEntry { item_id, kind: LibraryKind::Unknown, state, section: None })
        })
        .collect();
    Read::of(if entries.is_empty() { Notification::Ping } else { Notification::Library(entries) })
}

/// Open the socket at `target` and run it until it ends or `cancel` is set: over a TLS connection
/// libcurl verified as it does every request for an `https://` server, over plain TCP otherwise.
pub fn open(target: &Target, cancel: &AtomicBool, on: &mut dyn FnMut(Notification)) -> NotifyEnd {
    if !target.origin.is_tls() {
        return match Tcp::connect(&target.origin) {
            Ok(mut conn) => run(&mut conn, target, cancel, on),
            Err(_) => NotifyEnd::Transport,
        };
    }
    let mut end = NotifyEnd::Transport;
    let url = format!("{}/", target.origin.base());
    match plx_net::net::connect_tls(&url, plx_net::net::Tls::Ca, None, &mut |socket| end = run(socket, target, cancel, on)) {
        ConnectEnd::Ran => end,
        ConnectEnd::Transport(_) => NotifyEnd::Transport,
        ConnectEnd::Unsupported => NotifyEnd::Unsupported,
    }
}

/// The handshake, then messages until the connection ends.
pub fn run(conn: &mut dyn Conn, target: &Target, cancel: &AtomicBool, on: &mut dyn FnMut(Notification)) -> NotifyEnd {
    let mut noise = Noise::new();
    let key = noise.key();
    let host = target.origin.authority();
    if conn.write_all(socket::handshake_request(&host, &target.path, &key, &target.headers).as_bytes()).is_err() {
        return NotifyEnd::Transport;
    }
    let started = Instant::now();
    let mut early = Vec::new();
    match socket::read_handshake(conn, &key, &mut early, || cancel.load(Ordering::Acquire) || started.elapsed() > CONNECT * 2) {
        Ok(()) => {}
        Err(socket::Error::Status(status)) => return NotifyEnd::Refused { status },
        Err(_) if cancel.load(Ordering::Acquire) => return NotifyEnd::Cancelled,
        Err(_) => return NotifyEnd::Transport,
    }
    let mut messages = Messages::new(&target.user_id);
    let mut reader = Reader::default();
    reader.feed(&early);
    let mut keepalive = KEEPALIVE;
    // `None`: a KeepAlive is owed now.
    let (mut heard, mut said) = (Instant::now(), Some(Instant::now()));
    let mut chunk = [0u8; 4096];
    loop {
        loop {
            match reader.next() {
                Ok(None) => break,
                Ok(Some(frame)) => {
                    heard = Instant::now();
                    match frame {
                        Frame::Text(text) => {
                            let read = messages.read(&text);
                            if let Some(k) = read.keepalive {
                                keepalive = k;
                                said = None; // answer at once
                            }
                            on(read.notification);
                            if read.user_updated {
                                super::backend::refresh_signed_in_user();
                            }
                        }
                        Frame::Ping(payload) => {
                            if conn.write_all(&socket::pong_frame(&payload, noise.mask())).is_err() {
                                return NotifyEnd::Transport;
                            }
                        }
                        Frame::Pong => {}
                        Frame::Close => {
                            let _ = conn.write_all(&socket::close_frame(noise.mask()));
                            return NotifyEnd::Closed;
                        }
                    }
                }
                Err(_) => return NotifyEnd::Transport,
            }
        }
        if cancel.load(Ordering::Acquire) {
            let _ = conn.write_all(&socket::close_frame(noise.mask()));
            return NotifyEnd::Cancelled;
        }
        if heard.elapsed() > keepalive + IDLE_MARGIN {
            return NotifyEnd::Silent;
        }
        if said.is_none_or(|t| t.elapsed() >= keepalive) {
            if conn.write_all(&socket::text_frame(r#"{"MessageType":"KeepAlive"}"#, noise.mask())).is_err() {
                return NotifyEnd::Transport;
            }
            said = Some(Instant::now());
        }
        match conn.read(&mut chunk) {
            Ok(0) => return NotifyEnd::Closed,
            Ok(n) => reader.feed(&chunk[..n]),
            Err(e) if socket::timed_out(&e) => {}
            Err(_) => return NotifyEnd::Transport,
        }
    }
}

impl Conn for TlsSocket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        TlsSocket::read(self, buf)
    }
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        TlsSocket::write_all(self, bytes)
    }
}

/// A plain TCP connection, for an `http://` server.
struct Tcp(std::net::TcpStream);

impl Tcp {
    fn connect(origin: &Origin) -> io::Result<Tcp> {
        use std::net::ToSocketAddrs;
        let port = u16::try_from(origin.port()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let mut last = io::Error::from(io::ErrorKind::NotFound);
        for addr in (origin.host(), port).to_socket_addrs()? {
            match std::net::TcpStream::connect_timeout(&addr, CONNECT) {
                Ok(s) => {
                    s.set_read_timeout(Some(READ_TICK))?;
                    s.set_write_timeout(Some(CONNECT))?;
                    let _ = s.set_nodelay(true);
                    return Ok(Tcp(s));
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }
}

impl Conn for Tcp {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        io::Read::read(&mut self.0, buf)
    }
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        io::Write::write_all(&mut self.0, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(n: &Notification) -> Vec<(&str, &str)> {
        match n {
            Notification::Play(s) => s
                .iter()
                .map(|p| (p.session_key.as_deref().unwrap(), p.rating_key.as_deref().unwrap()))
                .collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn this_users_data_changes_report_each_item_stopped_under_its_own_key() {
        let mut m = Messages::new("5F0A-11");
        let r = m.read(r#"{"MessageType":"UserDataChanged","Data":{"UserId":"5f0a11","UserDataList":[{"ItemId":"e1","Played":true},{"ItemId":"e2"}]}}"#);
        assert_eq!(keys(&r.notification), [("jf-1", "e1"), ("jf-2", "e2")]);
        let Notification::Play(s) = &r.notification else { unreachable!() };
        assert!(s.iter().all(|p| p.state == PlayState::Stopped));
        // The same item again is news again.
        let r = m.read(r#"{"MessageType":"UserDataChanged","Data":{"UserId":"5f0a11","UserDataList":[{"ItemId":"e1"}]}}"#);
        assert_eq!(keys(&r.notification), [("jf-3", "e1")]);
        // Another user's changes are not this user's.
        let r = m.read(r#"{"MessageType":"UserDataChanged","Data":{"UserId":"other","UserDataList":[{"ItemId":"e1"}]}}"#);
        assert_eq!(r.notification, Notification::Ping);
    }

    #[test]
    fn library_changes_report_the_items_added_and_removed() {
        let mut m = Messages::new("u");
        let r = m.read(r#"{"MessageType":"LibraryChanged","Data":{"ItemsAdded":["A1","a2"],"ItemsRemoved":["r1"],"ItemsUpdated":["x"]}}"#);
        let Notification::Library(e) = &r.notification else { panic!("{:?}", r.notification) };
        let got: Vec<(&str, LibraryState)> = e.iter().map(|e| (e.item_id.as_str(), e.state)).collect();
        assert_eq!(got, [("a1", LibraryState::Created), ("a2", LibraryState::Created), ("r1", LibraryState::Deleted), ("x", LibraryState::Updated)]);
        assert!(e.iter().all(|e| e.kind == LibraryKind::Unknown));
        // An update alone moves the lists too: a scan that finishes an added item reports it so.
        let r = m.read(r#"{"MessageType":"LibraryChanged","Data":{"ItemsAdded":[],"ItemsUpdated":["x"]}}"#);
        assert!(matches!(&r.notification, Notification::Library(e) if e.len() == 1 && e[0].state == LibraryState::Updated));
        let r = m.read(r#"{"MessageType":"LibraryChanged","Data":{"ItemsAdded":[]}}"#);
        assert_eq!(r.notification, Notification::Ping);
        // A scan naming thousands is cut to the first few of each kind.
        let many: Vec<String> = (0..100).map(|i| format!("\"i{i}\"")).collect();
        let r = m.read(&format!(r#"{{"MessageType":"LibraryChanged","Data":{{"ItemsAdded":[{}]}}}}"#, many.join(",")));
        let Notification::Library(e) = &r.notification else { panic!() };
        assert_eq!(e.len(), LIBRARY_IDS);
    }

    #[test]
    fn the_server_sets_the_keep_alive_and_everything_else_is_a_ping() {
        let mut m = Messages::new("u");
        assert_eq!(m.read(r#"{"MessageType":"ForceKeepAlive","Data":60}"#).keepalive, Some(Duration::from_secs(30)));
        let r = m.read(r#"{"MessageType":"UserUpdated","Data":{"Id":"U","Name":"n"}}"#);
        assert!(r.user_updated && r.notification == Notification::Ping);
        assert!(!m.read(r#"{"MessageType":"UserUpdated","Data":{"Id":"other"}}"#).user_updated);
        for other in [r#"{"MessageType":"KeepAlive"}"#, r#"{"MessageType":"Play","Data":{}}"#, "not json"] {
            assert_eq!(m.read(other), Read { notification: Notification::Ping, keepalive: None, user_updated: false });
        }
    }

    /// A scripted server: the handshake answer and frames come in, what the client writes is kept.
    struct Server {
        incoming: Vec<u8>,
        written: Vec<u8>,
        cancel_when_drained: Option<std::sync::Arc<AtomicBool>>,
    }
    impl Conn for Server {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.incoming.is_empty() {
                if let Some(c) = &self.cancel_when_drained {
                    c.store(true, Ordering::Release);
                }
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let n = self.incoming.len().min(buf.len());
            buf[..n].copy_from_slice(&self.incoming[..n]);
            self.incoming.drain(..n);
            Ok(n)
        }
        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            if self.written.is_empty() {
                // The upgrade: answer it with the accept for the key it carried.
                let req = String::from_utf8_lossy(bytes).to_string();
                let key = req.lines().find_map(|l| l.strip_prefix("Sec-WebSocket-Key: ")).unwrap().to_string();
                let mut answer = format!("HTTP/1.1 101 Switching Protocols\r\nSec-WebSocket-Accept: {}\r\n\r\n", socket::accept_for(&key)).into_bytes();
                answer.extend(std::mem::take(&mut self.incoming));
                self.incoming = answer;
            }
            self.written.extend_from_slice(bytes);
            Ok(())
        }
    }

    fn text(t: &str) -> Vec<u8> {
        let mut f = vec![0x81, t.len() as u8];
        f.extend_from_slice(t.as_bytes());
        f
    }

    fn target() -> Target {
        Target {
            origin: Origin::parse("http://10.0.0.5:8096").unwrap(),
            path: "/jf/socket".into(),
            headers: vec!["Authorization: MediaBrowser Token=\"t\"".into()],
            user_id: "u1".into(),
        }
    }

    #[test]
    fn a_session_upgrades_reports_changes_answers_the_keep_alive_and_ends_on_cancel() {
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let mut frames = text(r#"{"MessageType":"ForceKeepAlive","Data":60}"#);
        frames.extend(text(r#"{"MessageType":"UserDataChanged","Data":{"UserId":"u1","UserDataList":[{"ItemId":"m1"}]}}"#));
        let mut server = Server { incoming: frames, written: Vec::new(), cancel_when_drained: Some(cancel.clone()) };
        let mut got = Vec::new();
        let end = run(&mut server, &target(), &cancel, &mut |n| got.push(n));
        assert_eq!(end, NotifyEnd::Cancelled);
        assert_eq!(got.len(), 2);
        assert_eq!(keys(&got[1]), [("jf-1", "m1")]);
        let written = String::from_utf8_lossy(&server.written);
        assert!(written.starts_with("GET /jf/socket HTTP/1.1\r\nHost: 10.0.0.5:8096\r\n"));
        assert!(written.contains("Authorization: MediaBrowser Token=\"t\"\r\n"));
        // After the upgrade: a masked KeepAlive (answering ForceKeepAlive at once), then the close.
        let frames = &server.written[written.find("\r\n\r\n").unwrap() + 4..];
        assert_eq!(frames[0], 0x81);
        assert_eq!(frames[1] & 0x7F, r#"{"MessageType":"KeepAlive"}"#.len() as u8);
        assert_eq!(&frames[frames.len() - 6..frames.len() - 4], &[0x88, 0x80]);
    }

    #[test]
    fn a_refused_upgrade_reports_its_status() {
        struct Refuse;
        impl Conn for Refuse {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let a = b"HTTP/1.1 401 Unauthorized\r\n\r\n";
                buf[..a.len()].copy_from_slice(a);
                Ok(a.len())
            }
            fn write_all(&mut self, _: &[u8]) -> io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(run(&mut Refuse, &target(), &AtomicBool::new(false), &mut |_| {}), NotifyEnd::Refused { status: 401 });
    }
}
