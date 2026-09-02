//! **Lab Control** — an authenticated outbound command channel for a television with no SSH.
//!
//! LG Cloud Test Lab accepts an `.ipk` and gives the tester a picture plus a virtual remote, but
//! exposes no inbound socket. The useful direction is therefore the same one diagnostics already
//! proved: the app opens pinned HTTPS to `lab.plxnative.com`. A long poll waits there until the
//! host queues a command, then the SDL thread dispatches it through the same synthetic-input seam
//! as the development FIFO and the next poll acknowledges it.
//!
//! No WebSocket dependency is hidden here. The oldest supported television has libcurl 7.53.1,
//! before libcurl's WebSocket API, while [`crate::net::post_pinned`] already supplies TLS, the
//! per-session SPKI pin, deadlines and a bounded response body. A held HTTP request also crosses
//! Cloud Test Lab's outbound-only NAT without opening a second public port.
//!
//! # Delivery contract
//!
//! The receiver keeps one command in flight until its id is acknowledged. A lost response is
//! therefore redelivered. Within one app process `run` waits for main-thread dispatch before it
//! returns the acknowledgement, so a retry of the acknowledgement does not press the key twice.
//! A process crash between dispatch and acknowledgement can replay that one command after launch:
//! this is deliberately **at least once across crashes**, which is the only honest guarantee
//! without writable durable state on the rented set.

use super::{config, ControlCommand};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

/// A command should enter SDL on the next frame. This ceiling detects a stopped/wedged main loop
/// without holding the receiver's queue forever.
const DISPATCH_TIMEOUT: Duration = Duration::from_secs(10);
/// `wait:<ms>` is a sequencing primitive, not a way to park the control worker indefinitely.
const MAX_WAIT_MS: u64 = 10_000;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct Ack {
    id: u32,
    ok: bool,
    detail: String,
}

#[derive(Default)]
struct MailState {
    pending: VecDeque<ControlCommand>,
    completed: VecDeque<Ack>,
}

struct Mailbox {
    state: Mutex<MailState>,
    completed: Condvar,
}

static MAILBOX: OnceLock<Mailbox> = OnceLock::new();
static STARTED: AtomicBool = AtomicBool::new(false);

fn mailbox() -> &'static Mailbox {
    MAILBOX.get_or_init(|| Mailbox {
        state: Mutex::new(MailState::default()),
        completed: Condvar::new(),
    })
}

#[derive(Serialize)]
struct PollRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    ack: Option<&'a Ack>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct WireCommand {
    id: u32,
    token: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct PollResponse {
    #[serde(default)]
    command: Option<WireCommand>,
}

fn parse_response(body: &[u8]) -> Result<Option<ControlCommand>, ()> {
    let r: PollResponse = serde_json::from_slice(body).map_err(|_| ())?;
    Ok(r.command.map(|c| ControlCommand {
        id: c.id,
        token: c.token,
    }))
}

fn wait_ms(token: &str) -> Option<u64> {
    let ms = token.strip_prefix("wait:")?.parse::<u64>().ok()?;
    (ms <= MAX_WAIT_MS).then_some(ms)
}

/// Start the one persistent long-poll worker. Called after `net::global_init`, never from
/// [`super::boot`], because curl's process-global initialisation must precede every worker.
pub(crate) fn start() {
    let Some(cfg) = config::get() else { return };
    if !cfg.control {
        return;
    }
    // On an old OpenSSL whose lock callbacks could not be installed, net.rs serialises every
    // HTTPS request behind one mutex. A 15-second long poll would then starve sign-in and uploads;
    // keep diagnostics working and name why control is unavailable instead.
    if !crate::net::threaded_tls_ready() {
        crate::log("lab-control: disabled — this firmware cannot run concurrent TLS safely");
        return;
    }
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }

    let endpoint = cfg.control_url();
    let secret = cfg.secret.clone();
    let session = cfg.session.clone();
    let pin = cfg.pin.clone();
    if !crate::task::spawn_small("labctl", move || run(endpoint, secret, session, pin)) {
        STARTED.store(false, Ordering::Release);
        crate::log("lab-control: no worker thread — command channel unavailable");
    }
}

fn run(url: String, secret: String, session: String, pin: String) {
    let headers = vec![
        format!("Authorization: Bearer {secret}"),
        format!("X-Plx-Session: {session}"),
        "Content-Type: application/json".to_string(),
        "Expect:".to_string(),
    ];
    let timeouts = crate::net::Timeouts {
        connect_s: 8,
        // The receiver holds an idle poll for 15 seconds. Leave handshake and response headroom.
        total_s: 25,
        total_ms: 0,
        low_speed_bps: 0,
        low_speed_s: 0,
    };
    let mut ack: Option<Ack> = None;
    let mut connected = false;
    let mut ever_connected = false;
    let mut failure_reported = false;
    let mut backoff_s = 1u64;

    loop {
        let body = match serde_json::to_vec(&PollRequest { ack: ack.as_ref() }) {
            Ok(body) => body,
            Err(_) => return, // Ack contains only primitives; serialization cannot realistically fail.
        };
        match crate::net::post_pinned(&url, &headers, &body, &pin, timeouts) {
            Some(r) if r.status == 200 => {
                // Receiving the response proves the receiver consumed the acknowledgement in this
                // request. Clear it before considering the next command carried by that response.
                ack = None;
                if !connected {
                    crate::log(if ever_connected {
                        "lab-control: receiver reconnected"
                    } else {
                        "lab-control: receiver connected"
                    });
                }
                connected = true;
                ever_connected = true;
                failure_reported = false;
                backoff_s = 1;
                match parse_response(&r.body) {
                    Ok(Some(command)) => {
                        if let Some(ms) = wait_ms(&command.token) {
                            std::thread::sleep(Duration::from_millis(ms));
                            ack = Some(Ack {
                                id: command.id,
                                ok: true,
                                detail: format!("waited {ms}ms"),
                            });
                        } else {
                            ack = Some(dispatch_and_wait(command));
                        }
                    }
                    Ok(None) => {} // idle long poll expired; immediately open the next one
                    Err(()) => {
                        crate::log("lab-control: receiver returned malformed JSON");
                    }
                }
            }
            Some(r) => {
                if !failure_reported {
                    crate::log(&format!(
                        "lab-control: receiver refused poll status={} — retrying",
                        r.status
                    ));
                }
                failure_reported = true;
                connected = false;
                std::thread::sleep(Duration::from_secs(backoff_s));
                backoff_s = (backoff_s * 2).min(8);
            }
            None => {
                if !failure_reported {
                    crate::log("lab-control: receiver unreachable — retrying");
                }
                failure_reported = true;
                connected = false;
                std::thread::sleep(Duration::from_secs(backoff_s));
                backoff_s = (backoff_s * 2).min(8);
            }
        }
    }
}

fn dispatch_and_wait(command: ControlCommand) -> Ack {
    let id = command.id;
    let mb = mailbox();
    let mut state = mb.state.lock().unwrap_or_else(|e| e.into_inner());
    state.pending.push_back(command);
    let waited = mb
        .completed
        .wait_timeout_while(state, DISPATCH_TIMEOUT, |s| {
            !s.completed.iter().any(|a| a.id == id)
        })
        .unwrap_or_else(|e| e.into_inner());
    state = waited.0;
    if let Some(i) = state.completed.iter().position(|a| a.id == id) {
        return state
            .completed
            .remove(i)
            .expect("position came from this deque");
    }

    // If the main loop never took it, retire the command so it cannot execute after the receiver
    // has already been told it failed. If it was taken and then wedged in dispatch, finishing it
    // later leaves one harmless stale completion which the next call ignores by id.
    state.pending.retain(|c| c.id != id);
    Ack {
        id,
        ok: false,
        detail: "main loop did not dispatch within 10s".into(),
    }
}

/// Main-thread half: take every command waiting to enter the SDL event queue.
pub(crate) fn take() -> Vec<ControlCommand> {
    let mut state = mailbox().state.lock().unwrap_or_else(|e| e.into_inner());
    state.pending.drain(..).collect()
}

/// Main-thread half: report whether the token was accepted by the synthetic input dispatcher.
pub(crate) fn finish(id: u32, ok: bool) {
    let mb = mailbox();
    let mut state = mb.state.lock().unwrap_or_else(|e| e.into_inner());
    state.completed.retain(|a| a.id != id);
    state.completed.push_back(Ack {
        id,
        ok,
        detail: if ok {
            "dispatched".into()
        } else {
            "unsupported command".into()
        },
    });
    while state.completed.len() > 8 {
        state.completed.pop_front();
    }
    mb.completed.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_parser_accepts_idle_and_one_command() {
        assert_eq!(parse_response(br#"{"ok":true,"command":null}"#), Ok(None));
        assert_eq!(
            parse_response(br#"{"ok":true,"command":{"id":7,"token":"down"}}"#),
            Ok(Some(ControlCommand {
                id: 7,
                token: "down".into()
            }))
        );
        assert!(parse_response(b"not json").is_err());
    }

    #[test]
    fn waits_are_bounded_and_unambiguous() {
        assert_eq!(wait_ms("wait:0"), Some(0));
        assert_eq!(wait_ms("wait:10000"), Some(10_000));
        assert_eq!(wait_ms("wait:10001"), None);
        assert_eq!(wait_ms("wait:-1"), None);
        assert_eq!(wait_ms("up"), None);
    }

    #[test]
    fn main_thread_mailbox_round_trips_a_result() {
        let _g = crate::testlock::serial();
        let mb = mailbox();
        let mut state = mb.state.lock().unwrap_or_else(|e| e.into_inner());
        state.pending.clear();
        state.completed.clear();
        state.pending.push_back(ControlCommand {
            id: 9,
            token: "ok".into(),
        });
        drop(state);

        assert_eq!(
            take(),
            vec![ControlCommand {
                id: 9,
                token: "ok".into()
            }]
        );
        finish(9, true);
        let mut state = mb.state.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            state.completed.pop_front(),
            Some(Ack {
                id: 9,
                ok: true,
                detail: "dispatched".into()
            })
        );
    }
}
