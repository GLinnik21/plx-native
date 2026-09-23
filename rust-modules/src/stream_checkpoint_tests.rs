//! The caller's checkpoint inside blocking connect/send/header/body waits: a stop ends the wait
//! with its own result long before the deadline, a Continue slice resumes the same operation in
//! place (partial header and chunk state intact, no second connection), and slices pace the
//! checks rather than spinning.

use super::*;
use crate::checkpoint::TestCheckpoint;
use std::sync::mpsc;
use std::time::Duration;

const SLICE: Duration = Duration::from_millis(20);
/// Every wait on another thread is bounded by this, so a regression fails instead of hanging.
const BOUND: Duration = Duration::from_secs(5);

/// Bounded wait for `cond`; false if it never held.
fn wait_for(cond: impl Fn() -> bool) -> bool {
    let until = Instant::now() + BOUND;
    while Instant::now() < until {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    cond()
}

/// A one-connection server that writes `parts` in order: the first right after the request head,
/// each later one only when the test sends `go`. After the last it holds the connection until one
/// more `go` (or [`BOUND`]), then reports whether a SECOND connection had been attempted.
struct Scripted {
    port: u16,
    go: mpsc::Sender<()>,
    written: mpsc::Receiver<usize>,
    done: Option<std::thread::JoinHandle<bool>>,
}

impl Scripted {
    fn start(parts: Vec<Vec<u8>>) -> Scripted {
        use std::io::{Read, Write};
        let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = srv.local_addr().unwrap().port();
        let (go, go_rx) = mpsc::channel::<()>();
        let (written_tx, written) = mpsc::channel();
        let done = std::thread::spawn(move || {
            let Ok((mut s, _)) = srv.accept() else {
                return false;
            };
            let mut head = Vec::new();
            let mut tmp = [0u8; 1024];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut tmp) {
                    Ok(0) | Err(_) => return false,
                    Ok(n) => head.extend_from_slice(&tmp[..n]),
                }
            }
            for (i, part) in parts.iter().enumerate() {
                if i > 0 && go_rx.recv_timeout(BOUND).is_err() {
                    return false;
                }
                if s.write_all(part).and_then(|_| s.flush()).is_err() {
                    return false;
                }
                let _ = written_tx.send(i);
            }
            let _ = go_rx.recv_timeout(BOUND);
            srv.set_nonblocking(true).expect("nonblocking");
            let second = srv.accept().is_ok();
            drop(s);
            second
        });
        Scripted {
            port,
            go,
            written,
            done: Some(done),
        }
    }

    fn wait_written(&self, part: usize) {
        assert_eq!(
            self.written.recv_timeout(BOUND),
            Ok(part),
            "server wrote part {part}"
        );
    }

    /// Release the hold and report whether anything dialled a second connection.
    fn finish(mut self) -> bool {
        let _ = self.go.send(());
        self.done.take().unwrap().join().unwrap()
    }
}

impl Drop for Scripted {
    fn drop(&mut self) {
        // Unconditional cleanup: a failed assertion must not leave the server parked in a recv.
        if let Some(done) = self.done.take() {
            for _ in 0..8 {
                let _ = self.go.send(());
            }
            let _ = done.join();
        }
    }
}

fn open_with(
    hs: *mut HttpStream,
    port: u16,
    deadline: Instant,
    checkpoint: &mut dyn Checkpoint,
) -> Result<(), HttpOpenError> {
    let host = std::ffi::CString::new("127.0.0.1").unwrap();
    let path = std::ffi::CString::new("/seg").unwrap();
    http_open_until_result(
        hs,
        host.as_ptr(),
        port as c_int,
        path.as_ptr(),
        std::ptr::null(),
        "GET",
        deadline,
        checkpoint,
    )
}

#[test]
fn a_checkpoint_stop_ends_a_withheld_header_wait_long_before_its_deadline() {
    let server = Scripted::start(vec![]);
    let mut hs = http_stream_boxed();
    let mut cp = TestCheckpoint::stopping_after(3, SLICE);
    let started = Instant::now();
    let result = open_with(
        &mut *hs,
        server.port,
        started + Duration::from_secs(30),
        &mut cp,
    );
    let took = started.elapsed();

    assert_eq!(result, Err(HttpOpenError::Stopped));
    assert_eq!(cp.calls(), 4, "three Continue answers, then the Stop");
    // Three slices were waited out, not spun through; and nowhere near the 30 s deadline.
    assert!(
        took >= Duration::from_millis(45) && took < BOUND,
        "stop took {took:?}"
    );
    assert_eq!(
        hs.fd(),
        -1,
        "a stopped request is retired, never left half-read"
    );
    assert!(!server.finish(), "a controlled stop must not redial");
}

#[test]
fn continue_slices_neither_end_an_open_early_nor_spin() {
    let server = Scripted::start(vec![]);
    let mut hs = http_stream_boxed();
    let mut cp = TestCheckpoint::every(SLICE);
    let started = Instant::now();
    let result = open_with(
        &mut *hs,
        server.port,
        started + Duration::from_millis(300),
        &mut cp,
    );
    let took = started.elapsed();

    // The deadline, not a slice, ended it — and it is still reported as the deadline.
    assert_eq!(result, Err(HttpOpenError::Deadline));
    assert!(
        took >= Duration::from_millis(290),
        "a slice ended the open early: {took:?}"
    );
    let calls = cp.calls();
    assert!(
        (5..=40).contains(&calls),
        "~15 checks expected over 300 ms at a 20 ms slice, saw {calls}"
    );
    assert!(!server.finish());
}

#[test]
fn a_partial_header_block_survives_checkpoint_slices_on_one_connection() {
    let server = Scripted::start(vec![
        b"HTTP/1.1 200 OK\r\nContent-Le".to_vec(),
        b"ngth: 2\r\n\r\nok".to_vec(),
    ]);
    let mut hs = http_stream_boxed();
    let addr = (&mut *hs) as *mut HttpStream as usize;
    let mut cp = TestCheckpoint::every(SLICE);
    let calls = cp.calls.clone();
    let port = server.port;
    let result = std::thread::scope(|sc| {
        let opener = sc.spawn(move || {
            open_with(
                addr as *mut HttpStream,
                port,
                Instant::now() + Duration::from_secs(30),
                &mut cp,
            )
        });
        server.wait_written(0);
        let seen = calls.load(Ordering::Acquire);
        assert!(
            wait_for(|| calls.load(Ordering::Acquire) >= seen + 3),
            "the header wait never consulted its checkpoint"
        );
        assert!(!opener.is_finished(), "a Continue slice ended the open");
        server.go.send(()).unwrap();
        opener.join().unwrap()
    });

    assert_eq!(result, Ok(()));
    assert_eq!(hs_status(&*hs), 200);
    assert_eq!(hs_content_length(&*hs), 2);
    let mut body = [0u8; 8];
    let n = http_read_until(&mut *hs, body.as_mut_ptr(), 8, None, &mut NoCheckpoint);
    assert_eq!(&body[..n as usize], b"ok");
    assert!(
        !server.finish(),
        "the open resumed in place rather than reconnecting"
    );
}

#[test]
fn a_blocked_body_read_stops_on_request_and_keeps_its_connection() {
    let server = Scripted::start(vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nabc".to_vec(),
        b"def".to_vec(),
    ]);
    let mut hs = http_stream_boxed();
    assert_eq!(
        open_with(
            &mut *hs,
            server.port,
            Instant::now() + BOUND,
            &mut NoCheckpoint
        ),
        Ok(())
    );
    server.wait_written(0);
    let mut got = Vec::new();
    let mut buf = [0u8; 16];
    while got.len() < 3 {
        let n = http_read_until(&mut *hs, buf.as_mut_ptr(), 16, None, &mut NoCheckpoint);
        assert!(n > 0, "prefix read returned {n}");
        got.extend_from_slice(&buf[..n as usize]);
    }
    assert_eq!(got, b"abc");

    let addr = (&mut *hs) as *mut HttpStream as usize;
    let mut cp = TestCheckpoint::every(SLICE);
    let (calls, stop) = (cp.calls.clone(), cp.stop.clone());
    let r = std::thread::scope(|sc| {
        let reader = sc.spawn(move || {
            let mut b = [0u8; 16];
            http_read_until(addr as *mut HttpStream, b.as_mut_ptr(), 16, None, &mut cp)
        });
        assert!(
            wait_for(|| calls.load(Ordering::Acquire) >= 3),
            "the blocked read never consulted its checkpoint"
        );
        assert!(
            !reader.is_finished(),
            "a Continue slice returned from the read early"
        );
        stop.store(true, Ordering::Release);
        reader.join().unwrap()
    });
    assert_eq!(r, HTTP_READ_STOPPED);
    assert!(
        hs.fd() >= 0,
        "a stop is not a transport failure: the socket stays"
    );

    // The same response resumes where it stopped.
    server.go.send(()).unwrap();
    server.wait_written(1);
    let n = http_read_until(&mut *hs, buf.as_mut_ptr(), 16, None, &mut NoCheckpoint);
    assert_eq!(&buf[..n.max(0) as usize], b"def");
    assert!(!server.finish());
}

/// Bytes already in the socket's kernel queue are RECEIVED: a read hands them over without
/// consulting its checkpoint, the same as bytes in the header buffer. Asking first let a hold that
/// landed after `read_cb`'s own check stop a transfer whose remainder had already arrived.
#[test]
fn bytes_already_queued_in_the_kernel_are_returned_before_the_checkpoint_is_asked() {
    for deadline in [None, Some(Instant::now() + BOUND)] {
        let server = Scripted::start(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n".to_vec(),
            b"abc".to_vec(),
        ]);
        let mut hs = http_stream_boxed();
        assert_eq!(
            open_with(
                &mut *hs,
                server.port,
                Instant::now() + BOUND,
                &mut NoCheckpoint
            ),
            Ok(())
        );
        server.wait_written(0);
        server.go.send(()).unwrap();
        server.wait_written(1);
        // Loopback delivery is synchronous with the write; this only rules out a slow scheduler.
        std::thread::sleep(Duration::from_millis(20));
        let mut cp = TestCheckpoint::stopping_after(0, SLICE);
        let mut buf = [0u8; 16];
        let n = http_read_until(&mut *hs, buf.as_mut_ptr(), 16, deadline, &mut cp);
        assert_eq!(
            (n, cp.calls()),
            (3, 0),
            "deadline={deadline:?}: queued bytes must be delivered without asking the checkpoint"
        );
        assert_eq!(&buf[..3], b"abc");
        assert!(!server.finish());
    }
}

#[test]
fn a_chunked_body_split_across_checkpoint_slices_survives() {
    let parts: Vec<Vec<u8>> = vec![
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5".to_vec(),
        b"\r\nhe".to_vec(),
        b"llo\r\n0\r\n".to_vec(),
        b"\r\n".to_vec(),
    ];
    let last = parts.len() - 1;
    let server = Scripted::start(parts);
    let mut hs = http_stream_boxed();
    assert_eq!(
        open_with(
            &mut *hs,
            server.port,
            Instant::now() + BOUND,
            &mut NoCheckpoint
        ),
        Ok(())
    );
    let addr = (&mut *hs) as *mut HttpStream as usize;
    let mut cp = TestCheckpoint::every(SLICE);
    let calls = cp.calls.clone();
    let body = std::thread::scope(|sc| {
        let reader = sc.spawn(move || {
            let mut body = Vec::new();
            let mut b = [0u8; 16];
            loop {
                let n = http_read_until(addr as *mut HttpStream, b.as_mut_ptr(), 16, None, &mut cp);
                if n <= 0 {
                    return (body, n);
                }
                body.extend_from_slice(&b[..n as usize]);
            }
        });
        server.wait_written(0);
        for part in 1..=last {
            // Each fragment lands only after the reader has sat through checkpoint slices.
            let seen = calls.load(Ordering::Acquire);
            assert!(
                wait_for(|| calls.load(Ordering::Acquire) >= seen + 2),
                "the chunked read never consulted its checkpoint before part {part}"
            );
            server.go.send(()).unwrap();
            server.wait_written(part);
        }
        reader.join().unwrap()
    });

    assert_eq!(body, (b"hello".to_vec(), 0));
    assert!(
        http_body_done(&*hs),
        "trailer consumed: the connection is reusable"
    );
    assert!(!server.finish());
}
