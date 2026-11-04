//! The accepts for a loopback test server: the one that fixes the socket's mode, and the ones that
//! are bounded.
//!
//! A fixture makes its listener nonblocking so the acceptor can poll a stop flag or a
//! deadline. Darwin hands the accepted socket the listener's O_NONBLOCK; Linux does not. A
//! reader that assumed blocking I/O then sees `WouldBlock` whenever a parallel suite accepts
//! before the request line is buffered, and a writer can drop a body on a full send buffer —
//! flakes that exist on the Mac only. Every such acceptor accepts through here, so the
//! connection behaves as on Linux and read/write timeouts mean what they say.
//!
//! **A server thread never waits for its client without a bound.** A plain `listener.accept()` in
//! a spawned thread blocks until somebody connects, and the test that joins it blocks with it. The
//! client under test does not always connect: a request whose caller deadline has already passed
//! returns without dialing (`plx_plex::http`'s `Deadline` pre-check), and a loaded machine can
//! carry a short deadline past before the client's first `connect`. The joiner then waits forever,
//! the suite never finishes, and a run that should have failed (or should never have depended on
//! the clock) hangs for an hour instead. [`accept_within`], [`accept_client`] and [`serve_once`] turn
//! "no client arrived" into an error, or a panic the joiner sees, after a wait the caller names.

use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// How long a fixture server waits for its client by default. A client that is going to dial does
/// so within milliseconds of its server thread starting; this only has to outlast a loaded Mac
/// running two `make check`s, and a client that has not come by then is not coming.
pub const CLIENT_WAIT: Duration = Duration::from_secs(10);

/// The caller deadline for a loopback exchange that either completes before it or deliberately
/// waits it out (a silent peer). Far above any scheduling delay a loaded host adds to an exchange
/// that takes a few milliseconds, so a response that is going to arrive always beats it, and under
/// `plx_net`'s 15 s inactivity budget, so the CALLER still owns the instant (past that budget
/// `request_until_outcome` reads an expiry as the transport's liveness, not as the caller's
/// deadline).
pub const LOOPBACK_DEADLINE: Duration = Duration::from_secs(2);

/// Sleep until `deadline` has passed (plus a little), so a test can look at an outcome the caller
/// computed before the boundary from the far side of it. Costs the time left to the deadline: a
/// test that crosses a [`LOOPBACK_DEADLINE`] spends about that long asleep.
pub fn cross(deadline: Instant) {
    let left = deadline.saturating_duration_since(Instant::now());
    if !left.is_zero() {
        std::thread::sleep(left + Duration::from_millis(10));
    }
    assert!(Instant::now() >= deadline, "the fixture did not cross its deadline");
}

/// `listener.accept()`, with the accepted socket put back in blocking mode.
pub fn accept(listener: &TcpListener) -> io::Result<(TcpStream, SocketAddr)> {
    let (socket, peer) = listener.accept()?;
    socket.set_nonblocking(false)?;
    Ok((socket, peer))
}

/// [`accept`], but only for `wait`: makes the listener nonblocking and polls it, and returns a
/// `TimedOut` error naming the listener's address when nobody has connected by then. The accepted
/// socket is blocking, as [`accept`] leaves it. A fixture that loops over several connections
/// calls this once per connection.
pub fn accept_within(listener: &TcpListener, wait: Duration) -> io::Result<(TcpStream, SocketAddr)> {
    listener.set_nonblocking(true)?;
    let until = Instant::now() + wait;
    loop {
        match accept(listener) {
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
            other => return other,
        }
        if Instant::now() >= until {
            let at = listener.local_addr().map_or_else(|_| "the listener".into(), |a| a.to_string());
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("no client reached {at} within {wait:?}"),
            ));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// [`accept_within`] for a server that cannot go on without its client: when nobody connects
/// within `wait` it panics ("its client never arrived", naming the listener), so the thread running
/// it ends and whoever joins that thread fails instead of blocking. A scoped server (`thread::scope`
/// over state the test borrows, or a fixture that keeps its listener for a second look) cannot hand
/// its listener to [`serve_once`]; it accepts through this.
pub fn accept_client(listener: &TcpListener, wait: Duration) -> (TcpStream, SocketAddr) {
    accept_within(listener, wait).unwrap_or_else(|e| panic!("loopback server: its client never arrived: {e}"))
}

/// Serve ONE client on a new thread: wait up to `wait` for it ([`accept_client`]), then hand its
/// socket to `serve` and return what `serve` returns through the handle. If nobody connects, the
/// thread panics with that error, so the test's `join()` fails instead of blocking.
pub fn serve_once<T: Send + 'static>(
    listener: TcpListener,
    wait: Duration,
    serve: impl FnOnce(TcpStream) -> T + Send + 'static,
) -> std::thread::JoinHandle<T> {
    std::thread::Builder::new()
        .name("loopback server".into())
        .spawn(move || {
            let (socket, _) = accept_client(&listener, wait);
            serve(socket)
        })
        .expect("the OS refused a loopback server thread")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn bound() -> (TcpListener, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("address").port();
        (listener, port)
    }

    /// The reproduction of the hang, as a permanent test: a listener nobody dials used to block
    /// its `accept()` for ever; it is now an error after the wait the caller named.
    #[test]
    fn a_client_that_never_dials_is_an_error_after_the_wait_not_a_hang() {
        let (listener, port) = bound();
        let began = Instant::now();
        let err = accept_within(&listener, Duration::from_millis(50)).expect_err("nobody dialed");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(err.to_string().contains(&port.to_string()), "{err}");
        assert!(began.elapsed() >= Duration::from_millis(50));
        assert!(began.elapsed() < Duration::from_secs(5), "bounded by the wait: {:?}", began.elapsed());
    }

    #[test]
    fn a_server_whose_client_never_dials_fails_its_joiner() {
        let (listener, _port) = bound();
        let server = serve_once(listener, Duration::from_millis(50), |_socket| ());
        let payload = server.join().expect_err("the joiner sees the failure instead of blocking");
        let message = payload.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(message.contains("its client never arrived"), "{message}");
    }

    #[test]
    fn accepting_a_client_that_never_dials_panics_naming_the_listener() {
        let (listener, port) = bound();
        let payload = std::panic::catch_unwind(|| accept_client(&listener, Duration::from_millis(50)))
            .expect_err("nobody dialed");
        let message = payload.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(message.contains("its client never arrived") && message.contains(&port.to_string()), "{message}");
    }

    #[test]
    fn accepting_a_client_that_dials_hands_back_its_blocking_socket() {
        let (listener, port) = bound();
        let client = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        let (mut socket, peer) = accept_client(&listener, CLIENT_WAIT);
        assert_eq!(peer, client.local_addr().expect("client address"));
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            (&client).write_all(b"ping").expect("write");
            client
        });
        let mut got = [0u8; 4];
        socket.read_exact(&mut got).expect("a blocking read waits for the bytes");
        assert_eq!(&got, b"ping");
        drop(writer.join().expect("writer"));
    }

    #[test]
    fn a_client_that_dials_is_served_and_its_socket_blocks() {
        let (listener, port) = bound();
        let server = serve_once(listener, CLIENT_WAIT, |mut socket| {
            // The client writes only after a pause: a socket that inherited the listener's
            // O_NONBLOCK (Darwin) would answer this read with `WouldBlock` instead of waiting.
            let mut got = [0u8; 4];
            socket.read_exact(&mut got).expect("a blocking read waits for the bytes");
            got
        });
        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        std::thread::sleep(Duration::from_millis(50));
        client.write_all(b"ping").expect("write");
        assert_eq!(&server.join().expect("served"), b"ping");
    }

    #[test]
    fn cross_returns_only_once_the_deadline_has_passed() {
        let deadline = Instant::now() + Duration::from_millis(30);
        cross(deadline);
        assert!(Instant::now() >= deadline);
        cross(deadline); // already behind us: returns at once
    }
}
