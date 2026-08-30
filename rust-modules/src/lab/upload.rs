//! The upload itself: **snapshot on the main thread, send on a worker**.
//!
//! # The split, and why it is exactly here
//!
//! [`crate::player::diag`] is main-thread by contract — the panel it was written for must not tell
//! a story that never happened, so the whole struct is one instant's read. Everything after that
//! (scrub, serialise, gzip, TLS, the blocking POST) is unbounded work on a link we do not control,
//! and none of it may touch the SDL loop: the feed pump, the ACB control calls and the render all
//! live there. So [`request`] samples and hands off, and the worker never calls back into the app
//! except through atomics.
//!
//! `task::spawn_small` rather than `thread::spawn`, for this crate's usual reason: a refused spawn
//! is a return value here, not a panic that kills the app (`task.rs`, and the `RLIMIT_NPROC`
//! measurement in `tools/threadprobe.c`). A refusal shows in the toast as a failure, which is the
//! honest reading.
//!
//! # Single flight
//!
//! One upload at a time, refused rather than queued. The trigger is a button a tester presses when
//! nothing appears to be happening, so the realistic input is four presses in two seconds; a queue
//! would answer that by sending four near-identical documents over a link that was already the
//! reason nothing appeared to happen.
use crate::lab::config;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering::Relaxed};
use std::sync::Mutex;

/// What the toast is saying. A `u8` because it is written from a worker and read from the render
/// thread every frame.
pub(crate) const PHASE_IDLE: u8 = 0;
pub(crate) const PHASE_SENDING: u8 = 1;
pub(crate) const PHASE_OK: u8 = 2;
pub(crate) const PHASE_FAIL: u8 = 3;

static PHASE: AtomicU8 = AtomicU8::new(PHASE_IDLE);
static INFLIGHT: AtomicBool = AtomicBool::new(false);
static SEQ: AtomicU32 = AtomicU32::new(0);
/// ring-clock ms at which the toast stops being drawn; 0 = not showing
static UNTIL_MS: AtomicU32 = AtomicU32::new(0);
static DETAIL: Mutex<String> = Mutex::new(String::new());
/// The route name the app last reported, for the envelope. `&'static str` from `app.rs`'s own
/// route table, so this stores a name that already exists rather than allocating one per frame.
static ROUTE: Mutex<&'static str> = Mutex::new("home");

/// How long a finished toast stays up. Long enough to read across a room and photograph, short
/// enough not to sit over the failure the tester is looking at.
const TOAST_MS: u32 = 5_000;

/// The app's current route, for the envelope. Called once per frame from the tail of the loop with
/// the same `&'static str` the heartbeat uses.
pub(crate) fn note_route(r: &'static str) {
    let mut g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
    *g = r;
}

pub(crate) fn phase() -> u8 {
    PHASE.load(Relaxed)
}

/// The toast's second line: a byte count on success, a reason on failure.
pub(crate) fn detail() -> String {
    DETAIL.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Is the toast still within its window? Drives both the draw and the expiry.
pub(crate) fn showing() -> bool {
    match phase() {
        PHASE_IDLE => false,
        PHASE_SENDING => true,
        _ => crate::diag::ring::t_ms() < UNTIL_MS.load(Relaxed),
    }
}

/// Retire a finished toast. Called once per frame; the invalidate is what gets the frame that
/// paints its absence — nothing else on screen moved.
pub(crate) fn update(_now: u32) {
    if matches!(phase(), PHASE_OK | PHASE_FAIL) && !showing() {
        set_phase(PHASE_IDLE, String::new());
    }
}

fn set_phase(p: u8, detail: String) {
    PHASE.store(p, Relaxed);
    *DETAIL.lock().unwrap_or_else(|e| e.into_inner()) = detail;
    UNTIL_MS.store(
        match p {
            PHASE_OK | PHASE_FAIL => crate::diag::ring::t_ms().saturating_add(TOAST_MS),
            _ => 0,
        },
        Relaxed,
    );
    // A toast is a clock-driven overlay, not a spring — `ui::idle::note_spring` cannot see it, and
    // an uninvalidated one appears only on the next keypress (the failure mode `Xfade` and
    // `Spinner` both shipped with; see `docs/agent-reference.md`'s note on the present gate).
    crate::ui::idle::invalidate();
}

/// **Main thread.** Sample everything, then hand it to a worker.
pub(crate) fn request(reason: &str) {
    let Some(cfg) = config::get() else { return };
    if INFLIGHT.swap(true, Relaxed) {
        crate::log("lab: upload already in flight — press ignored");
        return;
    }
    let seq = SEQ.fetch_add(1, Relaxed) + 1;
    let route = *ROUTE.lock().unwrap_or_else(|e| e.into_inner());
    // Before the snapshot, so the document contains the line that says why it exists.
    crate::log(&format!("lab: snapshot seq={seq} reason={reason} route={route}"));
    let doc = crate::lab::snapshot::build(seq, reason, &cfg.session, route);
    set_phase(PHASE_SENDING, String::new());
    let url = cfg.url();
    let secret = cfg.secret.clone();
    let session = cfg.session.clone();
    let pin = cfg.pin.clone();
    let spawned = crate::task::spawn_small("labup", move || {
        let (phase, detail) = send(&url, &secret, &session, &pin, seq, doc);
        set_phase(phase, detail);
        INFLIGHT.store(false, Relaxed);
    });
    if !spawned {
        INFLIGHT.store(false, Relaxed);
        set_phase(PHASE_FAIL, "no thread".into());
    }
}

/// The worker half. Returns the phase and the toast's detail line.
fn send(url: &str, secret: &str, session: &str, pin: &str, seq: u32, doc: String) -> (u8, String) {
    let raw = doc.into_bytes();
    let raw_len = raw.len();
    let (body, encoding) = match crate::diag::zlib::gzip(&raw) {
        Some(gz) => (gz, "gzip"),
        None => (raw, "identity"),
    };
    let headers = vec![
        format!("Authorization: Bearer {secret}"),
        format!("X-Plx-Session: {session}"),
        format!("X-Plx-Seq: {seq}"),
        "Content-Type: application/x-ndjson".to_string(),
        format!("Content-Encoding: {encoding}"),
        // curl would otherwise add `Expect: 100-continue` for a body over 1 KB and wait a second
        // for a receiver that never sends one.
        "Expect:".to_string(),
    ];
    let sent = body.len();
    let t = crate::net::Timeouts { connect_s: 8, total_s: 60, low_speed_bps: 0, low_speed_s: 0 };
    match crate::net::post_pinned(url, &headers, &body, pin, t) {
        Some(r) if r.ok() => {
            crate::log(&format!("lab: uploaded seq={seq} {raw_len}B -> {sent}B ({encoding}) status={}", r.status));
            (PHASE_OK, format!("{} KB sent", (sent + 512) / 1024))
        }
        Some(r) => {
            crate::log(&format!("lab: upload seq={seq} REFUSED status={}", r.status));
            (PHASE_FAIL, format!("receiver said {}", r.status))
        }
        None => {
            crate::log(&format!("lab: upload seq={seq} did not complete (transport)"));
            (PHASE_FAIL, "no answer from receiver".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A second press while one is in flight is refused, not queued — and the refusal releases
    /// nothing, so the first upload still owns the flag.
    #[test]
    fn the_flight_flag_admits_exactly_one() {
        let _g = crate::testlock::serial();
        INFLIGHT.store(false, Relaxed);
        assert!(!INFLIGHT.swap(true, Relaxed), "first press takes it");
        assert!(INFLIGHT.swap(true, Relaxed), "second press finds it taken");
        INFLIGHT.store(false, Relaxed);
    }

    /// The toast expires on the ring clock and the expiry returns the phase to idle, so nothing
    /// keeps asking for frames after it is gone.
    #[test]
    fn a_finished_toast_expires_back_to_idle() {
        let _g = crate::testlock::serial();
        set_phase(PHASE_OK, "12 KB sent".into());
        assert!(showing());
        UNTIL_MS.store(0, Relaxed); // as if TOAST_MS had elapsed
        assert!(!showing());
        update(0);
        assert_eq!(phase(), PHASE_IDLE);
        assert_eq!(detail(), "");
    }

    /// While sending, the toast is up regardless of the clock — an upload over a slow link must
    /// not have its "Uploading…" line time out from under it.
    #[test]
    fn the_sending_toast_does_not_expire() {
        let _g = crate::testlock::serial();
        set_phase(PHASE_SENDING, String::new());
        UNTIL_MS.store(0, Relaxed);
        assert!(showing());
        update(0);
        assert_eq!(phase(), PHASE_SENDING);
        set_phase(PHASE_IDLE, String::new());
    }
}
