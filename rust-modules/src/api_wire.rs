//! The typed wire protocol of the UDS control surface, and its single intake into the
//! frame thread — a leaf on purpose, which is why the module-cycle ratchet allows it.
//!
//! `crate::api` owns the frame thread's queue and the answers; `crate::api_sock` owns the
//! server thread. `api` already consumes `api_sock`'s reply types, so any call from
//! `api_sock` back into `api` would close a module cycle (the same is true of
//! `crate::shot`, whose captures answer parked reads). So the request types live HERE,
//! and the frame thread installs the one intake at boot: the socket hands every parsed
//! request to [`push_req`] and a capture reports through [`shot_landed`]/[`shot_failed`]
//! without ever naming `crate::api`.
use serde::Deserialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Mutex;

/// A request on the typed protocol: the client's id + the verb.
#[derive(Debug)]
pub struct Request {
    /// The client's own correlation id; echoed unchanged on the response.
    pub id: u64,
    pub verb: Verb,
}

/// The typed request vocabulary — the FIFO's `api:*` verbs with their argument shapes.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Verb {
    /// `api:play:<rk>` — the headless play ritual (resolve from PMS, seat the engine).
    Play { rk: String },
    /// `api:seek:<ms>` — an absolute engine seek (milliseconds).
    Seek { ms: u64 },
    /// `api:audio:<sid>` — the track menu's audio commit for the stream with this id.
    Audio { sid: i64 },
    /// `api:sub:<sid>` — the track menu's subtitle commit for the stream with this id.
    Sub { sid: i64 },
    /// `api:quality:<rung>` — the `…` menu's quality commit for this rung.
    Quality { rung: String },
    /// `api:profile:<index>` — the session machine's profile commit.
    Profile { index: usize },
    /// `api:pause` — the PAUSE key.
    Pause,
    /// `api:resume` — the PLAY key.
    Resume,
    /// `api:stop` — the STOP key's leave-playback ritual.
    Stop,
    /// Read: the frame thread's snapshot (the value `state.json` holds).
    State,
    /// Read: a capture of the next frame (a numbered PNG beside the state file).
    Shot,
}

/// The frame thread's door, installed once at boot (before the socket accepts anything).
type RequestSink = fn(Request) -> bool;
type ShotLandedSink = fn(String);
type ShotFailedSink = fn(String);

/// The bound on requests that may arrive BEFORE the door is installed: enough for a
/// client that connects while boot is still finishing, small enough that a flood still
/// gets the structured `queue_full` answer.
const PRE_INSTALL_MAX: usize = 64;

static SINK: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());
static SHOT_LANDED: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());
static SHOT_FAILED: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());
/// Requests that arrived before the door was installed (the socket's round-trip test;
/// a client connecting before boot finishes). Delivered in order by [`install`].
static PRE_INSTALL: Mutex<VecDeque<Request>> = Mutex::new(VecDeque::new());

/// Install the frame thread's intake (boot, next to the socket's own start). Requests
/// that arrived before the install are delivered in order; the door's own queue bounds
/// them, and anything it does not take is dropped.
pub fn install(request: RequestSink, shot_landed: ShotLandedSink, shot_failed: ShotFailedSink) {
    SINK.store(request as *const () as *mut (), Ordering::Release);
    SHOT_LANDED.store(shot_landed as *const () as *mut (), Ordering::Release);
    SHOT_FAILED.store(shot_failed as *const () as *mut (), Ordering::Release);
    let queued = PRE_INSTALL.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect::<Vec<_>>();
    for req in queued {
        if !(request)(req) {
            break;
        }
    }
}

/// Hand one parsed request to the frame thread. `false` = the door's queue is full
/// (the caller answers the structured `queue_full` error). Until the door is installed
/// a request is buffered (up to [`PRE_INSTALL_MAX`]) and delivered at install time — a
/// well-formed request is never answered on the spot for lacking a door.
pub fn push_req(req: Request) -> bool {
    let p = SINK.load(Ordering::Acquire) as *const RequestSink;
    if !p.is_null() {
        return unsafe { (*p)(req) };
    }
    let mut guard = PRE_INSTALL.lock().unwrap_or_else(|e| e.into_inner());
    if guard.len() >= PRE_INSTALL_MAX {
        return false;
    }
    guard.push_back(req);
    true
}

/// A shot capture landed (shot.rs, frame thread): the parked shot read answers with the
/// numbered file. A capture nobody parked a read for is a no-op.
pub fn shot_landed(path: String) {
    let p = SHOT_LANDED.load(Ordering::Acquire) as *const ShotLandedSink;
    if !p.is_null() {
        unsafe { (*p)(path) };
    }
}

/// A shot capture failed (shot.rs, frame thread): the parked shot read answers with the
/// reason.
pub fn shot_failed(reason: String) {
    let p = SHOT_FAILED.load(Ordering::Acquire) as *const ShotFailedSink;
    if !p.is_null() {
        unsafe { (*p)(reason) };
    }
}
