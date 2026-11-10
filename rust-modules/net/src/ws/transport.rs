use super::connection::close_code_allowed;
use super::frame::CONTROL_PAYLOAD;
use super::{Connection, Event, Fault};
use crate::net::connect_only::{self, Io, OpenError, Socket};
use crate::net::Tls;
use std::cell::Cell;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::raw::c_long;
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

const CONNECT_TIMEOUT_S: c_long = 10;
const UPGRADE_WAIT: Duration = Duration::from_secs(10);
const CLOSE_WAIT: Duration = Duration::from_secs(5);
const LINGER: Duration = Duration::from_secs(2);
const GOING_AWAY_WAIT: Duration = Duration::from_millis(300);
const POLL_CAP: Duration = Duration::from_millis(250);
const READ_CHUNK: usize = if cfg!(test) { 512 } else { 16 * 1024 };
const PASS_BUDGET: usize = 1 << 20;
const GOING_AWAY: u16 = 1001;

pub struct Dial<'a> {
    pub url: &'a str,
    pub trust: Tls<'a>,
    pub limit: usize,
    pub idle: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    Refused(Refusal),
    Closed { code: Option<u16>, reason: String },
    Failed(Fault),
    Lost,
    TimedOut,
    Idle,
    NoEntropy,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    Url,
    Entropy,
    Open(OpenError),
}

enum Outgoing {
    Text(String),
    Binary(Vec<u8>),
    Close(u16, String),
}

#[derive(Clone)]
pub struct Outbox {
    queue: mpsc::Sender<Outgoing>,
    wake: Arc<UnixStream>,
    live: Arc<AtomicBool>,
}

pub struct Inbox {
    queue: mpsc::Receiver<Outgoing>,
    wake: UnixStream,
    live: Arc<AtomicBool>,
    _writer: Arc<UnixStream>,
}

pub fn channel() -> Option<(Outbox, Inbox)> {
    let (reader, writer) = UnixStream::pair().ok()?;
    reader.set_nonblocking(true).ok()?;
    writer.set_nonblocking(true).ok()?;
    let (sender, receiver) = mpsc::channel();
    let writer = Arc::new(writer);
    let live = Arc::new(AtomicBool::new(true));
    Some((
        Outbox { queue: sender, wake: Arc::clone(&writer), live: Arc::clone(&live) },
        Inbox { queue: receiver, wake: reader, live, _writer: writer },
    ))
}

impl Outbox {
    pub fn text(&self, text: String) -> bool {
        self.push(Outgoing::Text(text))
    }

    pub fn binary(&self, data: Vec<u8>) -> bool {
        self.push(Outgoing::Binary(data))
    }

    pub fn close(&self, code: u16, reason: String) -> bool {
        if !close_code_allowed(code) || 2 + reason.len() > CONTROL_PAYLOAD || !self.push(Outgoing::Close(code, reason)) {
            return false;
        }
        self.live.store(false, Ordering::Release);
        true
    }

    fn push(&self, message: Outgoing) -> bool {
        if !self.live.load(Ordering::Acquire) || self.queue.send(message).is_err() {
            return false;
        }
        let _ = (&*self.wake).write(&[1]);
        true
    }
}

impl Inbox {
    fn drain_wake(&self) {
        let mut sink = [0u8; 64];
        while matches!((&self.wake).read(&mut sink), Ok(n) if n > 0) {}
    }

    fn shut(&self) {
        self.live.store(false, Ordering::Release);
    }
}

impl Drop for Inbox {
    fn drop(&mut self) {
        self.shut();
    }
}

pub fn run(
    off: &plx_base::task::OffFrame,
    dial: Dial<'_>,
    cancel: &AtomicBool,
    inbox: Inbox,
    on: &mut dyn FnMut(Event),
) -> End {
    let Some((secure, authority, path)) = split(dial.url) else {
        return End::Refused(Refusal::Url);
    };
    let Some(mut pool) = Pool::open() else {
        return End::Refused(Refusal::Entropy);
    };
    let Some(nonce) = pool.take::<16>() else {
        return End::Refused(Refusal::Entropy);
    };
    let short = Rc::new(Cell::new(false));
    let masks = {
        let short = Rc::clone(&short);
        move || {
            pool.take::<4>().unwrap_or_else(|| {
                short.set(true);
                [0; 4]
            })
        }
    };
    let Some(codec) = Connection::open(authority, &path, nonce, dial.limit, masks) else {
        return End::Refused(Refusal::Url);
    };
    if cancel.load(Ordering::Acquire) {
        return End::Cancelled;
    }
    let socket = match connect_only::open(off, authority, secure.then_some(dial.trust), CONNECT_TIMEOUT_S, cancel) {
        Ok(socket) => socket,
        Err(OpenError::Cancelled) => return End::Cancelled,
        Err(error) => return End::Refused(Refusal::Open(error)),
    };
    Session { socket, codec, wire: Wire::default(), short, idle: dial.idle }.drive(cancel, &inbox, on)
}

struct Session<M> {
    socket: Socket,
    codec: Connection<M>,
    wire: Wire,
    short: Rc<Cell<bool>>,
    idle: Duration,
}

impl<M: FnMut() -> [u8; 4]> Session<M> {
    fn drive(mut self, cancel: &AtomicBool, inbox: &Inbox, on: &mut dyn FnMut(Event)) -> End {
        let started = Instant::now();
        let mut heard = started;
        let mut opened = false;
        let mut closing: Option<Instant> = None;
        let mut buffer = vec![0u8; READ_CHUNK];
        loop {
            if cancel.load(Ordering::Acquire) {
                inbox.shut();
                return self.going_away();
            }
            if opened && closing.is_none() {
                while let Ok(message) = inbox.queue.try_recv() {
                    match message {
                        Outgoing::Text(text) => {
                            self.codec.send_text(&text);
                        }
                        Outgoing::Binary(data) => {
                            self.codec.send_binary(&data);
                        }
                        Outgoing::Close(code, reason) => {
                            if self.codec.close(code, &reason) {
                                closing = Some(Instant::now() + CLOSE_WAIT);
                                break;
                            }
                        }
                    }
                }
            }
            if let Some(end) = self.send_out() {
                return end;
            }
            let mut fresh = false;
            let mut ended = None;
            let mut eof = false;
            let mut taken = 0usize;
            while ended.is_none() && taken < PASS_BUDGET {
                match self.socket.recv(&mut buffer) {
                    Io::Moved(n) => {
                        taken += n;
                        heard = Instant::now();
                        self.codec.receive(&buffer[..n], &mut |event| match event {
                            Event::Opened => {
                                fresh = true;
                                on(Event::Opened);
                            }
                            Event::Closed { code, reason } => ended = Some(End::Closed { code, reason }),
                            Event::Failed(fault) => ended = Some(End::Failed(fault)),
                            message => on(message),
                        });
                    }
                    Io::Again => break,
                    Io::Closed | Io::Failed(_) => {
                        eof = true;
                        break;
                    }
                }
            }
            if let Some(end) = ended {
                inbox.shut();
                return if matches!(end, End::Failed(Fault::Handshake(_))) { end } else { self.finish(end, cancel) };
            }
            if eof {
                return End::Lost;
            }
            if let Some(end) = self.send_out() {
                return end;
            }
            opened |= fresh;
            let deadline = match (opened, closing) {
                (false, _) => Some(started + UPGRADE_WAIT),
                (true, Some(at)) => Some(at),
                (true, None) => heard.checked_add(self.idle),
            };
            let now = Instant::now();
            if deadline.is_some_and(|at| now >= at) {
                return if opened && closing.is_none() { End::Idle } else { End::TimedOut };
            }
            if fresh || taken >= PASS_BUDGET {
                continue;
            }
            let timeout = deadline.map_or(POLL_CAP, |at| (at - now).min(POLL_CAP));
            wait(self.socket.fd(), Some(&inbox.wake), !self.wire.is_empty(), timeout);
            inbox.drain_wake();
        }
    }

    fn send_out(&mut self) -> Option<End> {
        if self.short.get() {
            return Some(End::NoEntropy);
        }
        self.wire.push(self.codec.take_outgoing());
        (!self.wire.flush(&mut self.socket)).then_some(End::Lost)
    }

    fn finish(mut self, end: End, cancel: &AtomicBool) -> End {
        if let Some(stop) = self.send_out() {
            return if stop == End::NoEntropy { stop } else { end };
        }
        let until = Instant::now() + LINGER;
        let mut buffer = [0u8; 512];
        loop {
            let now = Instant::now();
            if now >= until || cancel.load(Ordering::Acquire) || !self.wire.flush(&mut self.socket) {
                return end;
            }
            match self.socket.recv(&mut buffer) {
                Io::Moved(_) => continue,
                Io::Closed | Io::Failed(_) => return end,
                Io::Again => {}
            }
            wait(self.socket.fd(), None, !self.wire.is_empty(), (until - now).min(POLL_CAP));
        }
    }

    fn going_away(mut self) -> End {
        if !self.codec.close(GOING_AWAY, "") {
            return End::Cancelled;
        }
        if let Some(end) = self.send_out() {
            return if end == End::NoEntropy { end } else { End::Cancelled };
        }
        let until = Instant::now() + GOING_AWAY_WAIT;
        loop {
            let now = Instant::now();
            if self.wire.is_empty() || now >= until || !self.wire.flush(&mut self.socket) {
                return End::Cancelled;
            }
            wait(self.socket.fd(), None, true, until - now);
        }
    }
}

#[derive(Default)]
struct Wire {
    inflight: Vec<u8>,
    at: usize,
    queued: Vec<u8>,
}

impl Wire {
    fn push(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        if self.inflight.is_empty() {
            self.inflight = bytes;
            self.at = 0;
        } else {
            self.queued.extend_from_slice(&bytes);
        }
    }

    fn is_empty(&self) -> bool {
        self.inflight.is_empty()
    }

    fn flush(&mut self, socket: &mut Socket) -> bool {
        while !self.inflight.is_empty() {
            match socket.send(&self.inflight[self.at..]) {
                Io::Moved(n) => {
                    self.at += n;
                    if self.at == self.inflight.len() {
                        self.inflight = std::mem::take(&mut self.queued);
                        self.at = 0;
                    }
                }
                Io::Again => return true,
                Io::Closed | Io::Failed(_) => return false,
            }
        }
        true
    }
}

struct Pool {
    file: File,
    bytes: [u8; 256],
    at: usize,
}

impl Pool {
    fn open() -> Option<Pool> {
        let mut pool = Pool { file: File::open("/dev/urandom").ok()?, bytes: [0; 256], at: 0 };
        pool.file.read_exact(&mut pool.bytes).ok()?;
        Some(pool)
    }

    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        if self.at + N > self.bytes.len() {
            self.file.read_exact(&mut self.bytes).ok()?;
            self.at = 0;
        }
        let mut out = [0u8; N];
        out.copy_from_slice(&self.bytes[self.at..self.at + N]);
        self.at += N;
        Some(out)
    }
}

fn split(url: &str) -> Option<(bool, &str, String)> {
    let scheme = |prefix: &str| url.get(..prefix.len()).is_some_and(|head| head.eq_ignore_ascii_case(prefix));
    let (secure, rest) = if scheme("wss://") {
        (true, &url[6..])
    } else if scheme("ws://") {
        (false, &url[5..])
    } else {
        return None;
    };
    let (authority, target) = rest.split_at(rest.find(['/', '?']).unwrap_or(rest.len()));
    let path = if target.starts_with('/') { target.to_owned() } else { format!("/{target}") };
    Some((secure, authority, path))
}

fn wait(socket: std::os::raw::c_int, wake: Option<&UnixStream>, writable: bool, timeout: Duration) {
    let socket_events = if writable { libc::POLLIN | libc::POLLOUT } else { libc::POLLIN };
    let mut fds = [
        libc::pollfd { fd: socket, events: socket_events, revents: 0 },
        libc::pollfd { fd: wake.map_or(-1, AsRawFd::as_raw_fd), events: libc::POLLIN, revents: 0 },
    ];
    let ms = timeout.as_millis().clamp(1, 60_000) as libc::c_int;
    unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
