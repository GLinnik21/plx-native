//! The webOS transport for the URI-mode probe (issue #1064): [`crate::tv::uri::UriMedia`] over
//! `com.webos.media`.
//!
//! **One worker owns the bus handle.** `open` starts a thread that registers (`Shape::Named` is
//! `com.webos.media.client.plxprobe`, the name the Developer Mode role file grants outbound
//! permission to that service; `Shape::Anon` is the plain anonymous client the rest of `ls2`
//! uses), calls, pumps its private glib context every few milliseconds, and unregisters. It is the
//! ONLY thread that ever touches its `LSHandle`: the callers' `call` and `close` post an [`Op`] to
//! it and return, so nothing here shares a handle across threads and nothing blocks the frame.
//!
//! Each call is an `LSCall`, which keeps its token: every message the service sends on it (the
//! `load` reply, then the events of a subscription) reaches [`on_message`] on the worker and goes
//! onto a bounded reply queue the main thread drains with `poll`. A full queue drops the newest
//! message and counts it; the count returns as one `dropped` reply. The callback's context lives
//! in a worker-owned list that outlives the registration (it is dropped after it), and is leaked
//! if the hub refuses to unregister, because the handle is then still attached to a callback.
//!
//! The registration outlives playback on purpose: uMediaserver unloads a pipeline whose load
//! client leaves the bus. `close` therefore lets the worker keep pumping for a short linger after
//! its last op, so the `unload` it was just given is flushed before the unregister.
//!
//! Log lines carry the prefix `ls2uri:` and never a payload.
use super::{error_text, g_main_context_iteration, register_as, LSCall, LSError, LSErrorFree, LSErrorInit, LSMessageGetPayload, Registration};
use crate::tv::uri::{ReplyQueue, Shape, UriMedia, UriReply};
use std::ffi::{CStr, CString};
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How many undrained replies the queue holds before it drops the newest. The main thread polls
/// every frame; a session produces a handful of messages and a `currentTime` a second.
const REPLY_QUEUE: usize = 256;
/// How often the worker wakes to pump its glib context when no op arrives.
const PUMP_EVERY: Duration = Duration::from_millis(5);
/// How long the worker keeps pumping after `close` so the last call is flushed.
const CLOSE_LINGER: Duration = Duration::from_millis(300);
/// The dispatch rounds one pump runs at most, so a flood of messages cannot starve an op.
const PUMP_ROUNDS: usize = 64;

/// What the callers post to the worker.
enum Op {
    Call { method: &'static str, payload: CString },
    Close,
}

/// What the worker and the callbacks share with the poller.
struct Shared {
    replies: ReplyQueue,
    ended: AtomicBool,
}

/// The context of one `LSCall`: which method its messages answer.
struct CallContext {
    method: &'static str,
    shared: Arc<Shared>,
}

/// The transport's handle on its worker: the op channel, the reply queue and the shared flags.
struct Worker {
    ops: mpsc::Sender<Op>,
    shared: Arc<Shared>,
}

/// The transport the port installs. The worker is process state (there is one bus identity and
/// one probe), so it sits here rather than in the unit struct.
pub struct Transport;

static WORKER: Mutex<Option<Worker>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<Worker>> {
    WORKER.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl UriMedia for Transport {
    fn open(&self, shape: Shape) -> Result<(), String> {
        let mut slot = lock();
        if slot.as_ref().is_some_and(|worker| !worker.shared.ended.load(Ordering::Acquire)) {
            return Err("already open".to_owned());
        }
        let (ops, inbox) = mpsc::channel();
        let shared = Arc::new(Shared { replies: ReplyQueue::new(REPLY_QUEUE), ended: AtomicBool::new(false) });
        let worker_shared = Arc::clone(&shared);
        let spawned = plx_base::task::spawn("ls2uri", move || run(shape, inbox, worker_shared));
        if spawned.is_none() {
            return Err("worker thread refused".to_owned());
        }
        *slot = Some(Worker { ops, shared });
        Ok(())
    }

    fn call(&self, method: &'static str, payload: &str) -> Result<(), String> {
        let slot = lock();
        let Some(worker) = slot.as_ref() else { return Err("not open".to_owned()) };
        if worker.shared.ended.load(Ordering::Acquire) {
            return Err("worker ended".to_owned());
        }
        let payload = CString::new(payload).map_err(|_| "payload holds a NUL".to_owned())?;
        worker.ops.send(Op::Call { method, payload }).map_err(|_| "worker gone".to_owned())
    }

    fn poll(&self) -> Vec<UriReply> {
        let slot = lock();
        slot.as_ref().map_or_else(Vec::new, |worker| worker.shared.replies.drain())
    }

    fn close(&self) {
        // Taking the worker out is what makes a following `open` start a fresh one. Its thread
        // is not joined: the caller is the frame loop, and the worker ends itself.
        let worker = lock().take();
        if let Some(worker) = worker {
            let _ = worker.ops.send(Op::Close);
        }
    }
}

extern "C" fn on_message(_handle: *mut c_void, message: *mut c_void, context: *mut c_void) -> bool {
    if context.is_null() || message.is_null() {
        return true;
    }
    // SAFETY: `context` is the `CallContext` box `send` registered with this call; the worker
    // keeps it alive until after the registration is gone.
    let context = unsafe { &*(context as *const CallContext) };
    let payload = unsafe { LSMessageGetPayload(message) };
    let text = if payload.is_null() { String::new() } else { unsafe { CStr::from_ptr(payload) }.to_string_lossy().into_owned() };
    context.shared.replies.push(context.method, text, true);
    true
}

/// One `LSCall` to `luna://com.webos.media/<method>`. `Err` carries the hub's own words.
fn send(registration: &Registration, method: &'static str, payload: &CStr, shared: &Arc<Shared>) -> Result<Box<CallContext>, String> {
    let uri = CString::new(format!("luna://com.webos.media/{method}")).map_err(|_| "uri holds a NUL".to_owned())?;
    let context = Box::new(CallContext { method, shared: Arc::clone(shared) });
    let mut error: LSError = unsafe { std::mem::zeroed() };
    unsafe { LSErrorInit(&mut error) };
    let called = unsafe {
        LSCall(
            registration.handle,
            uri.as_ptr(),
            payload.as_ptr(),
            on_message,
            &*context as *const CallContext as *mut c_void,
            std::ptr::null_mut(),
            &mut error,
        )
    };
    let result = if called { Ok(context) } else { Err(error_text(&error)) };
    unsafe { LSErrorFree(&mut error) };
    result
}

/// Dispatch whatever the bus has delivered, without waiting.
fn pump(registration: &Registration) {
    for _ in 0..PUMP_ROUNDS {
        if unsafe { g_main_context_iteration(registration.context, 0) } == 0 {
            break;
        }
    }
}

/// The worker's whole life. Declaration order is drop order: `registration` (and with it the
/// handle) goes before `contexts`, so no callback outlives what it points at.
fn run(shape: Shape, inbox: mpsc::Receiver<Op>, shared: Arc<Shared>) {
    let started = Instant::now();
    let name = match shape {
        Shape::Named => Some(c"com.webos.media.client.plxprobe"),
        Shape::Anon => None,
    };
    let label = if name.is_some() { "named" } else { "anon" };
    let mut contexts: Vec<Box<CallContext>> = Vec::new();
    let mut registration = match register_as(name) {
        Ok(registration) => registration,
        Err(fail) => {
            plx_base::eventlog::log(&format!("ls2uri: register shape={label} refused after {}ms", started.elapsed().as_millis()));
            shared.replies.push("register", fail.to_string(), false);
            shared.ended.store(true, Ordering::Release);
            return;
        }
    };
    plx_base::eventlog::log(&format!("ls2uri: register shape={label} ok after {}ms", started.elapsed().as_millis()));
    shared.replies.push("register", String::new(), true);
    loop {
        match inbox.recv_timeout(PUMP_EVERY) {
            Ok(Op::Call { method, payload }) => match send(&registration, method, &payload, &shared) {
                Ok(context) => {
                    contexts.push(context);
                    plx_base::eventlog::log(&format!("ls2uri: call {method} sent"));
                }
                Err(text) => {
                    plx_base::eventlog::log(&format!("ls2uri: call {method} not sent ({text})"));
                    shared.replies.push(method, text, false);
                }
            },
            Ok(Op::Close) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        pump(&registration);
    }
    if !contexts.is_empty() {
        let until = Instant::now() + CLOSE_LINGER;
        while Instant::now() < until {
            pump(&registration);
            std::thread::sleep(PUMP_EVERY);
        }
    }
    shared.ended.store(true, Ordering::Release);
    if !registration.unregister() {
        // The handle is still attached to callbacks that point into `contexts`.
        std::mem::forget(contexts);
    }
    plx_base::eventlog::log("ls2uri: closed");
}
