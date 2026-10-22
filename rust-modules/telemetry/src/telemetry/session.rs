//! **Sentry release-health sessions: crash-free sessions per release.**
//!
//! Sentry's release health needs one `session` envelope item per session — nothing here is a
//! crash report, and a session that ends well is still sent. It records WHEN the app was in use
//! and HOW the session ended, never what was watched or browsed: start time, duration, an end
//! status, an error count and the release/environment. The schema is Sentry's developer
//! documentation, <https://develop.sentry.dev/sdk/telemetry/sessions/> (checked 2026-10-22):
//!
//! ```text
//! {"sid": <uuid>, "did": <string, hashed by Sentry>, "init": true, "started": <ISO UTC>,
//!  "timestamp": <ISO UTC>, "duration": <seconds>, "status": ok|exited|crashed|abnormal,
//!  "errors": <number>, "attrs": {"release": <string>, "environment": <string>}}
//! ```
//!
//! `started` and `attrs.release` are required; `started`, `did` and `attrs` must not change after
//! the first transmission; a terminal status admits no later update; `init: true` forces `seq` to
//! 0. **One terminal item per session, sent whole with `init: true`**, which is the shape the
//! documentation's own example gives, so there is no `seq` to keep and a retry resends the same
//! bytes under the same `event_id` (the sid without dashes). The documentation does not describe
//! server-side de-duplication, so none is claimed: a send whose 200 was lost could count one
//! start twice.
//!
//! # The rules (the session model Datadog's RUM uses)
//!
//! * a session ends `exited` after **15 minutes without input** ([`IDLE_SECS`]) — the idle time
//!   is not part of its duration — or when the app goes to the **background**, or at a clean
//!   exit; and is split after **4 hours** ([`MAX_SESSION_SECS`]);
//! * **video playing is activity.** A film is watched with no remote input for far longer than 15
//!   minutes, and a crash in the middle of it must land in a session, not between two;
//! * a death is only knowable on the NEXT boot. The open session's identity is persisted
//!   ([`Stored`], written at its start and at most every 5 minutes of activity afterwards,
//!   off the frame thread) and [`recover`] reports it as `crashed` when this boot's crash
//!   recovery found crash records (`crashreport::recover_pending`), else `abnormal` — a power
//!   cut, a kill, a launcher close. A Rust panic leaves its record in the crash log like a fault,
//!   so it reads `crashed` too. A crashed session carries `errors: 1`, as the schema asks.
//!
//! # Consent and identity
//!
//! Only while crash-report consent is on ([`gate`]), and **`did` is the Crash report ID** —
//! never the Analytics ID: the two identifiers are never joined. Withdrawal and sign-out call
//! [`stop`], which forgets the live session and deletes the persisted one. The release and
//! environment are persisted WITH the session, so a session an upgrade interrupted is reported
//! under the version that ran it; one whose `did` is no longer the current Crash report ID (the
//! person switched crash reports off and on) is dropped, never re-attributed.
//!
//! # Shape
//!
//! [`Machine`] is a pure `state + event + clock -> effects` reducer and carries every rule above,
//! so each one is a test with an injected clock. The functions at the bottom ([`input`], [`tick`],
//! [`lifecycle`], …) are the thin driver: they feed the machine events from the app's seams
//! and execute its effects (state file, spool).

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// Inactivity that ends a session.
pub const IDLE_SECS: u64 = 15 * 60;
/// The longest one session runs; a busy one is split, so a long-running TV still reports.
pub const MAX_SESSION_SECS: u64 = 4 * 60 * 60;
/// Session items allowed to wait in the spool at once. A television that is offline for weeks
/// ends a session every few hours; the cap keeps those from crowding out crash reports under
/// the spool's own caps (oldest session items go first).
pub const MAX_PENDING_SESSIONS: usize = 16;
/// How often activity refreshes the persisted state: bounds the duration a crash can lose.
const CHECKPOINT_SECS: u64 = 5 * 60;

/// How a session ended, in Sentry's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Exited,
    Crashed,
    Abnormal,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Status::Exited => "exited",
            Status::Crashed => "crashed",
            Status::Abnormal => "abnormal",
        }
    }
}

/// What survives a death: the minimal state of the open session. No content, no path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stored {
    pub sid: String,
    /// The Crash report ID the session started under.
    pub did: String,
    pub release: String,
    pub environment: String,
    /// Unix seconds.
    pub started: u64,
    /// Unix seconds of the last checkpointed activity.
    pub last_active: u64,
}

/// A session whose end is known, ready to be framed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    pub stored: Stored,
    pub status: Status,
    pub duration_s: u64,
    pub errors: u32,
}

impl Finished {
    /// The Sentry `session` item payload.
    pub fn payload(&self) -> serde_json::Value {
        serde_json::json!({
            "sid": self.stored.sid,
            "did": self.stored.did,
            "init": true,
            "started": iso(self.stored.started),
            "timestamp": iso(self.stored.started.saturating_add(self.duration_s)),
            "duration": self.duration_s,
            "status": self.status.word(),
            "errors": self.errors,
            "attrs": {
                "release": self.stored.release,
                "environment": self.stored.environment,
            },
        })
    }

    /// Is the start time worth sending against `now_unix`: the plausibility window the crash
    /// reports use, so a session begun and ended on a television whose clock was never set is
    /// dropped rather than filed under a made-up date.
    pub fn reportable(&self, now_unix: u64) -> bool {
        super::crashreport::crash_timestamp(Some(self.stored.started), now_unix).is_some()
    }

    /// The spool's `event_id`: the sid without dashes, 32 lowercase hex like every other id there.
    pub fn event_id(&self) -> String {
        self.stored.sid.replace('-', "")
    }
}

/// UTC `YYYY-MM-DDTHH:MM:SSZ`, the form the documentation's examples use.
pub(super) fn iso(unix: u64) -> String {
    let (y, m, d) = plx_net::net::civil_date(unix as i64);
    let s = unix % 86_400;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3_600, s % 3_600 / 60, s % 60)
}

/// How every spooled session body begins. A body is `serde_json::to_vec` of a `Value` whose map
/// keeps its keys sorted, so `attrs` leads; a Sentry event has no top-level `attrs`.
pub const SESSION_BODY_PREFIX: &[u8] = br#"{"attrs":"#;

/// Is this spooled Sentry body a session item rather than an event? A prefix test, because the
/// spool trim, the pending-session count at every session end (on the frame thread) and the send
/// order all ask it of every record, and a full parse of each would cost far more than the answer.
/// A test pins that [`Finished::payload`] serialises with this prefix, so a change fails loudly.
pub fn is_session_body(body: &[u8]) -> bool {
    body.starts_with(SESSION_BODY_PREFIX)
}

/// A random version-4 UUID, from the same source the analytics session id uses.
fn new_sid() -> Option<String> {
    let mut hex = crate::diag::random_hex_id()?.into_bytes();
    if hex.len() != 32 {
        return None;
    }
    hex[12] = b'4';
    hex[16] = b"89ab"[(hex[16] as char).to_digit(16)? as usize & 3];
    let h = String::from_utf8(hex).ok()?;
    Some(format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]))
}

// ---------------------------------------------------------------------------------------------
// The machine
// ---------------------------------------------------------------------------------------------

/// Two readings of "now": monotonic seconds for every duration (a wall clock jumps when the set
/// first reaches NTP) and Unix seconds for what is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    pub mono: u64,
    pub unix: u64,
}

/// What a new session is made of, produced only when one actually starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ctx {
    pub sid: String,
    pub did: String,
    pub release: String,
    pub environment: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Boot, or crash reports were switched on: the app is in use now.
    Start,
    /// Consent withdrawn or the account's tenure ended: forget the session, send nothing.
    Stop,
    /// A key, a pointer or a text event.
    Input,
    /// The loop's once-per-frame beat; `playing` is the player route being on screen.
    Tick { playing: bool },
    Background,
    Foreground,
    /// The loop ended cleanly.
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionFx {
    /// Write the open session's state.
    Persist(Stored),
    /// Delete the persisted state.
    Clear,
    /// A session ended: queue its report.
    Finish(Finished),
}

#[derive(Debug)]
struct Live {
    stored: Stored,
    started_mono: u64,
    last_active_mono: u64,
    checkpoint_mono: u64,
}

impl Live {
    /// Unix seconds of the start by the clock of `clk`: its length is measured monotonically.
    fn corrected_start(&self, clk: Clock) -> u64 {
        clk.unix.saturating_sub(clk.mono.saturating_sub(self.started_mono))
    }
}

#[derive(Debug)]
pub struct Machine {
    live: Option<Live>,
    foreground: bool,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine {
    pub const fn new() -> Self {
        Self { live: None, foreground: true }
    }

    pub fn is_open(&self) -> bool {
        self.live.is_some()
    }

    /// Advance on `event`. `ctx` is asked for only when a session is about to start, so a frame
    /// that starts nothing reads no consent and mints no id.
    pub fn step(&mut self, event: Event, clk: Clock, ctx: &mut dyn FnMut() -> Option<Ctx>) -> Vec<SessionFx> {
        let mut fx = Vec::new();
        match event {
            Event::Start => {
                if self.live.is_none() && self.foreground {
                    self.begin(clk, ctx, &mut fx);
                }
            }
            Event::Stop => {
                if self.live.take().is_some() {
                    fx.push(SessionFx::Clear);
                }
            }
            Event::Foreground => {
                self.foreground = true;
                if self.live.is_none() {
                    self.begin(clk, ctx, &mut fx);
                }
            }
            Event::Background => {
                self.foreground = false;
                self.end_exited(clk, &mut fx);
            }
            Event::Shutdown => self.end_exited(clk, &mut fx),
            Event::Input | Event::Tick { playing: true } => self.activity(clk, ctx, &mut fx),
            Event::Tick { playing: false } => {
                self.expire(clk, &mut fx);
            }
        }
        fx
    }

    fn begin(&mut self, clk: Clock, ctx: &mut dyn FnMut() -> Option<Ctx>, fx: &mut Vec<SessionFx>) {
        let Some(c) = ctx() else { return };
        let stored = Stored {
            sid: c.sid,
            did: c.did,
            release: c.release,
            environment: c.environment,
            started: clk.unix,
            last_active: clk.unix,
        };
        fx.push(SessionFx::Persist(stored.clone()));
        self.live = Some(Live {
            stored,
            started_mono: clk.mono,
            last_active_mono: clk.mono,
            checkpoint_mono: clk.mono,
        });
    }

    fn finish(&mut self, status: Status, duration_s: u64, clk: Clock, fx: &mut Vec<SessionFx>) {
        if let Some(mut live) = self.live.take() {
            // The wall clock may have been unset (or stepped by NTP) when the session began: the
            // start is the end minus the monotonic length, from the clock as it is now.
            live.stored.started = live.corrected_start(clk);
            fx.push(SessionFx::Finish(Finished { stored: live.stored, status, duration_s, errors: 0 }));
            fx.push(SessionFx::Clear);
        }
    }

    fn end_exited(&mut self, clk: Clock, fx: &mut Vec<SessionFx>) {
        if let Some(live) = &self.live {
            let duration = clk.mono.saturating_sub(live.started_mono);
            self.finish(Status::Exited, duration, clk, fx);
        }
    }

    /// End the session when it has been idle for [`IDLE_SECS`] (its duration stops at the last
    /// activity) or has run [`MAX_SESSION_SECS`]. Returns whether it ended.
    fn expire(&mut self, clk: Clock, fx: &mut Vec<SessionFx>) -> bool {
        let Some(live) = &self.live else { return false };
        let idle = clk.mono.saturating_sub(live.last_active_mono);
        let age = clk.mono.saturating_sub(live.started_mono);
        if idle >= IDLE_SECS {
            let duration = live.last_active_mono.saturating_sub(live.started_mono);
            self.finish(Status::Exited, duration, clk, fx);
            true
        } else if age >= MAX_SESSION_SECS {
            self.finish(Status::Exited, age, clk, fx);
            true
        } else {
            false
        }
    }

    /// Activity: split what has expired, start a session when none is open, and refresh the
    /// persisted state at most every [`CHECKPOINT_SECS`].
    fn activity(&mut self, clk: Clock, ctx: &mut dyn FnMut() -> Option<Ctx>, fx: &mut Vec<SessionFx>) {
        self.expire(clk, fx);
        if self.live.is_none() {
            if self.foreground {
                self.begin(clk, ctx, fx);
            }
            return;
        }
        if let Some(live) = &mut self.live {
            live.last_active_mono = clk.mono;
            live.stored.last_active = clk.unix;
            if clk.mono.saturating_sub(live.checkpoint_mono) >= CHECKPOINT_SECS {
                live.checkpoint_mono = clk.mono;
                live.stored.started = live.corrected_start(clk);
                fx.push(SessionFx::Persist(live.stored.clone()));
            }
        }
    }
}

/// The previous process's unfinished session, classified. `None` when it cannot be reported
/// truthfully: it started under another Crash report ID than the current `did`, or its start
/// time is not a plausible one (the same window the crash reports use).
pub fn classify_unfinished(stored: Stored, crashes_found: usize, now_unix: u64, did: &str) -> Option<Finished> {
    if stored.did != did || super::crashreport::crash_timestamp(Some(stored.started), now_unix).is_none() {
        return None;
    }
    let crashed = crashes_found > 0;
    let duration_s = stored.last_active.saturating_sub(stored.started);
    Some(Finished {
        stored,
        status: if crashed { Status::Crashed } else { Status::Abnormal },
        duration_s,
        errors: u32::from(crashed),
    })
}

// ---------------------------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------------------------

static MACHINE: Mutex<Machine> = Mutex::new(Machine::new());
/// Bumped whenever the persisted state is deleted, so a checkpoint write already queued behind it
/// cannot put the file back.
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Serialises the file operations (a write in flight and a delete).
static FILE_IO: Mutex<()> = Mutex::new(());

/// May a session be tracked or sent at all: crash-report consent, and a Sentry destination in
/// this build (the same gate as `crashreport::may_read_crash_data`). Test builds have no DSN.
fn gate() -> bool {
    super::consent::allows_errors() && (super::sender::has_sentry() || cfg!(any(test, feature = "test-support")))
}

fn clock() -> Clock {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    Clock {
        mono: START.get_or_init(std::time::Instant::now).elapsed().as_secs(),
        unix: super::crashreport::now_secs(),
    }
}

fn make_ctx() -> Option<Ctx> {
    if !gate() {
        return None;
    }
    Some(Ctx {
        sid: new_sid()?,
        did: super::consent::errors_id()?,
        release: super::release().to_string(),
        environment: super::sender::ENVIRONMENT.to_string(),
    })
}

fn drive(event: Event) {
    let clk = clock();
    let fx = {
        let mut m = MACHINE.lock().unwrap_or_else(|e| e.into_inner());
        m.step(event, clk, &mut make_ctx)
    };
    for effect in fx {
        match effect {
            SessionFx::Persist(stored) => persist_off_thread(stored),
            SessionFx::Clear => clear_state(),
            SessionFx::Finish(f) if !f.reportable(clk.unix) => {
                plx_base::eventlog::log("telemetry: a session ended on a clock that was never set; dropped");
            }
            // The process is ending: nothing runs after this, so it is written here.
            SessionFx::Finish(f) if event == Event::Shutdown => {
                spool(&f);
            }
            SessionFx::Finish(f) => spool_off_thread(f),
        }
    }
}

/// Spool a finished session on the storage worker and then flush: the spool is read in full to
/// count the pending sessions and written synced, neither of which belongs on the frame thread.
fn spool_off_thread(f: Finished) {
    let job = f.clone();
    let queued = plx_base::storage_worker::submit(move || {
        if spool(&job) {
            super::flush_soon();
        }
    });
    if queued.is_err() {
        if spool(&f) {
            super::flush_soon();
        }
    }
    #[cfg(any(test, feature = "test-support"))]
    plx_base::storage_worker::drain_for_test();
}

/// The app is in use: boot, or crash reports were just switched on.
pub fn start() {
    drive(Event::Start);
}

/// Crash reports were switched off, or the account's tenure ended. Forgets the live session AND
/// deletes whatever is persisted, whether or not this process had one open.
pub fn stop() {
    drive(Event::Stop);
    clear_state();
}

/// One input event.
pub fn input() {
    drive(Event::Input);
}

/// The loop's per-frame beat. A frame with no session open and nothing playing costs one lock.
pub fn tick(playing: bool) {
    let open = MACHINE.lock().unwrap_or_else(|e| e.into_inner()).is_open();
    if open || playing {
        drive(Event::Tick { playing });
    }
}

/// An SDL window lifecycle event: WILL/DID background end the session, DID foreground begins one.
pub fn lifecycle(sdl_event: u32) {
    match sdl_event {
        0x103 | 0x104 => drive(Event::Background),
        0x106 => drive(Event::Foreground),
        _ => {}
    }
}

/// The loop ended cleanly.
pub fn shutdown() {
    drive(Event::Shutdown);
}

/// At boot, after crash recovery: report the previous process's unfinished session, then begin
/// this one. `crashes_found` is `crashreport::recover_pending`'s count.
pub fn recover_and_start(crashes_found: usize) {
    recover_and_start_at(crashes_found, super::crashreport::now_secs());
}

/// [`recover_and_start`] against an injected clock.
///
/// The state file is deleted only once its session is spooled or definitively dropped (a
/// different Crash report ID, consent off, a start outside the plausible window). On a clock that
/// is not yet set it can be neither classified nor dated, so it is KEPT for a later boot, and no
/// session begins now: the new one's state would overwrite it.
fn recover_and_start_at(crashes_found: usize, now_unix: u64) {
    if let Some(stored) = read_state() {
        if now_unix < super::crashreport::FIRST_PLAUSIBLE_NOW {
            plx_base::eventlog::log("telemetry: clock not set; an unfinished session is kept for a later boot");
            return;
        }
        if let (true, Some(did)) = (gate(), super::consent::errors_id()) {
            match classify_unfinished(stored, crashes_found, now_unix, &did) {
                Some(f) => {
                    if !spool(&f) {
                        return; // keep it: the next boot tries again
                    }
                }
                None => plx_base::eventlog::log("telemetry: an unfinished session could not be reported truthfully; dropped"),
            }
        }
        clear_state();
    }
    start();
}

fn state_paths() -> Vec<std::path::PathBuf> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(root) = TEST_ROOT.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return vec![root.join("telemetry-session.json")];
    }
    plx_base::paths::telemetry_session_candidates()
}

#[cfg(any(test, feature = "test-support"))]
static TEST_ROOT: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

/// Point the state file at a directory of the test's own, and reset the live machine.
#[cfg(any(test, feature = "test-support"))]
pub fn redirect_for_test(root: Option<std::path::PathBuf>) {
    *TEST_ROOT.lock().unwrap_or_else(|e| e.into_inner()) = root;
    *MACHINE.lock().unwrap_or_else(|e| e.into_inner()) = Machine::new();
}

fn read_state() -> Option<Stored> {
    state_paths()
        .iter()
        .filter_map(|p| plx_plex::plex::session::read_owned_regular(p))
        .find_map(|b| serde_json::from_slice::<Stored>(&b).ok())
}

/// Write the state on the storage worker: a synced write is not for the frame thread.
fn persist_off_thread(stored: Stored) {
    let generation = GENERATION.load(Ordering::SeqCst);
    let paths = state_paths();
    if plx_base::storage_worker::submit(move || write_if_current(generation, &paths, &stored)).is_err() {
        plx_base::eventlog::log("telemetry: the session state could not be queued for persistence");
    }
    #[cfg(any(test, feature = "test-support"))]
    plx_base::storage_worker::drain_for_test();
}

/// The worker's half: write unless the state was deleted since `generation` was read.
fn write_if_current(generation: u64, paths: &[std::path::PathBuf], stored: &Stored) {
    let _io = FILE_IO.lock().unwrap_or_else(|e| e.into_inner());
    if GENERATION.load(Ordering::SeqCst) != generation {
        return; // the session ended or was withdrawn while this waited
    }
    let Ok(json) = serde_json::to_vec(stored) else { return };
    if !paths.iter().any(|p| plx_plex::plex::session::write_atomic(p, &json).is_ok()) {
        plx_base::eventlog::log("telemetry: could not persist the session state to ANY candidate path");
    }
}

fn clear_state() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    let _io = FILE_IO.lock().unwrap_or_else(|e| e.into_inner());
    for p in state_paths() {
        let _ = std::fs::remove_file(p);
    }
}

/// Queue a finished session: Errors category (withdrawal purges it with the crash reports it
/// travels with), after making room under [`MAX_PENDING_SESSIONS`].
fn spool(f: &Finished) -> bool {
    use super::queue::{Category, Dest, Record};
    let Ok(body) = serde_json::to_vec(&f.payload()) else { return true };
    let record = Record { category: Category::Errors, dest: Dest::Sentry, event_id: f.event_id(), body };
    let pending: Vec<String> = super::spool::read()
        .into_iter()
        .filter(|r| r.dest == Dest::Sentry && is_session_body(&r.body))
        .map(|r| r.event_id)
        .collect();
    if pending.len() >= MAX_PENDING_SESSIONS {
        let excess = pending.len() + 1 - MAX_PENDING_SESSIONS;
        super::spool::commit_retiring(&pending[..excess]);
    }
    let appended = super::spool::append_if(&record, super::consent::allows_errors);
    if appended == Some(false) {
        plx_base::eventlog::log("telemetry: a session report could not be spooled");
    }
    appended != Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(mono: u64) -> Clock {
        Clock { mono, unix: 1_800_000_000 + mono }
    }

    fn ctx_n(n: u32) -> impl FnMut() -> Option<Ctx> {
        let mut n = n;
        move || {
            n += 1;
            Some(Ctx {
                sid: format!("00000000-0000-4000-8000-{n:012}"),
                did: "crash-id".into(),
                release: "plxnative@9.9.9".into(),
                environment: "production".into(),
            })
        }
    }

    fn finished(fx: &[SessionFx]) -> Vec<&Finished> {
        fx.iter().filter_map(|e| if let SessionFx::Finish(f) = e { Some(f) } else { None }).collect()
    }

    #[test]
    fn a_session_starts_on_start_and_persists_its_identity() {
        let mut m = Machine::new();
        let fx = m.step(Event::Start, at(0), &mut ctx_n(0));
        assert!(matches!(&fx[..], [SessionFx::Persist(s)] if s.did == "crash-id" && s.started == 1_800_000_000));
        assert!(m.is_open());
        // A second Start while open changes nothing and asks for no new identity.
        assert!(m.step(Event::Start, at(1), &mut || panic!("no id needed")).is_empty());
    }

    #[test]
    fn background_and_shutdown_end_a_session_as_exited() {
        for ev in [Event::Background, Event::Shutdown] {
            let mut m = Machine::new();
            m.step(Event::Start, at(0), &mut ctx_n(0));
            let fx = m.step(ev, at(90), &mut ctx_n(0));
            let f = finished(&fx);
            assert_eq!(f.len(), 1);
            assert_eq!((f[0].status, f[0].duration_s, f[0].errors), (Status::Exited, 90, 0));
            assert_eq!(fx.last(), Some(&SessionFx::Clear));
            assert!(!m.is_open());
        }
    }

    #[test]
    fn foreground_begins_a_new_session_after_a_background() {
        let mut m = Machine::new();
        m.step(Event::Start, at(0), &mut ctx_n(0));
        m.step(Event::Background, at(10), &mut ctx_n(0));
        // While backgrounded, input (a late event) does not start one.
        assert!(m.step(Event::Input, at(11), &mut ctx_n(5)).is_empty());
        let fx = m.step(Event::Foreground, at(500), &mut ctx_n(9));
        assert!(matches!(&fx[..], [SessionFx::Persist(s)] if s.sid.ends_with("000000000010")));
    }

    #[test]
    fn fifteen_idle_minutes_end_a_session_at_its_last_activity() {
        let mut m = Machine::new();
        m.step(Event::Start, at(0), &mut ctx_n(0));
        m.step(Event::Input, at(100), &mut ctx_n(0));
        // 14:59 of silence keeps it.
        assert!(m.step(Event::Tick { playing: false }, at(100 + IDLE_SECS - 1), &mut ctx_n(0)).is_empty());
        // 15:00 ends it; the idle quarter hour is not duration.
        let fx = m.step(Event::Tick { playing: false }, at(100 + IDLE_SECS), &mut ctx_n(0));
        assert_eq!(finished(&fx)[0].duration_s, 100);
        assert!(!m.is_open());
        // The next input after a long gap starts the following session.
        let fx = m.step(Event::Input, at(5_000), &mut ctx_n(7));
        assert!(matches!(&fx[..], [SessionFx::Persist(s)] if s.started == 1_800_005_000));
    }

    #[test]
    fn an_input_after_a_long_gap_splits_without_a_tick_in_between() {
        let mut m = Machine::new();
        m.step(Event::Start, at(0), &mut ctx_n(0));
        let fx = m.step(Event::Input, at(IDLE_SECS + 30), &mut ctx_n(3));
        assert_eq!(finished(&fx)[0].duration_s, 0);
        assert!(matches!(fx.last(), Some(SessionFx::Persist(_))), "{fx:?}");
        assert!(m.is_open());
    }

    #[test]
    fn four_busy_hours_split_a_session_and_continue_in_the_next() {
        let mut m = Machine::new();
        m.step(Event::Start, at(0), &mut ctx_n(0));
        let mut t = 0;
        while t + 600 < MAX_SESSION_SECS {
            t += 600;
            assert!(finished(&m.step(Event::Input, at(t), &mut ctx_n(0))).is_empty());
        }
        let fx = m.step(Event::Input, at(MAX_SESSION_SECS), &mut ctx_n(4));
        let f = finished(&fx);
        assert_eq!((f[0].status, f[0].duration_s), (Status::Exited, MAX_SESSION_SECS));
        assert!(m.is_open(), "the activity that crossed the line opens the next session");
    }

    #[test]
    fn playback_with_no_input_is_activity_for_forty_minutes() {
        let mut m = Machine::new();
        m.step(Event::Start, at(0), &mut ctx_n(0));
        for t in (0..40 * 60).step_by(5) {
            let fx = m.step(Event::Tick { playing: true }, at(t), &mut ctx_n(0));
            assert!(finished(&fx).is_empty(), "split at {t}");
        }
        assert!(m.is_open());
        // The film ends; 15 quiet minutes later the session closes with the film inside it.
        let fx = m.step(Event::Tick { playing: false }, at(40 * 60 + IDLE_SECS), &mut ctx_n(0));
        assert_eq!(finished(&fx)[0].duration_s, 40 * 60 - 5);
    }

    #[test]
    fn the_persisted_state_is_refreshed_at_most_every_five_minutes() {
        let mut m = Machine::new();
        m.step(Event::Start, at(0), &mut ctx_n(0));
        let mut writes = 0;
        for t in 1..=900 {
            let fx = m.step(Event::Input, at(t), &mut ctx_n(0));
            writes += fx.iter().filter(|e| matches!(e, SessionFx::Persist(_))).count();
        }
        assert_eq!(writes, 3, "900 s of constant input checkpoints once per 300 s");
    }

    #[test]
    fn stop_forgets_without_reporting_and_a_fresh_start_is_a_new_session() {
        let mut m = Machine::new();
        m.step(Event::Start, at(0), &mut ctx_n(0));
        let fx = m.step(Event::Stop, at(5), &mut ctx_n(0));
        assert_eq!(fx, vec![SessionFx::Clear]);
        assert!(finished(&fx).is_empty());
        assert!(m.step(Event::Stop, at(6), &mut ctx_n(0)).is_empty());
        // No consent means no ctx: nothing starts.
        assert!(m.step(Event::Input, at(7), &mut || None).is_empty());
        assert!(!m.is_open());
    }

    /// A television whose clock was never set when the session began reports the session from the
    /// clock it has when the session ENDS: `started` is `unix_now - (mono_now - started_mono)`, and a
    /// result outside the plausible window is not reported at all.
    #[test]
    fn a_session_started_on_an_unset_clock_is_reported_from_the_clock_at_its_end() {
        let mut m = Machine::new();
        m.step(Event::Start, Clock { mono: 0, unix: 12 }, &mut ctx_n(0));
        let end = Clock { mono: 90, unix: 1_800_000_090 };
        let fx = m.step(Event::Shutdown, end, &mut ctx_n(0));
        let f = &finished(&fx)[0];
        assert_eq!(f.stored.started, 1_800_000_000, "the start is the end minus the monotonic length");
        assert!(f.reportable(end.unix));

        let mut m = Machine::new();
        m.step(Event::Start, Clock { mono: 0, unix: 12 }, &mut ctx_n(0));
        let still_unset = Clock { mono: 90, unix: 102 };
        let fx = m.step(Event::Shutdown, still_unset, &mut ctx_n(0));
        assert!(!finished(&fx)[0].reportable(still_unset.unix), "an unset clock at the end is no start time");
    }

    #[test]
    fn a_checkpoint_carries_a_start_time_corrected_by_the_clock_it_now_has() {
        let mut m = Machine::new();
        m.step(Event::Start, Clock { mono: 0, unix: 12 }, &mut ctx_n(0));
        let fx = m.step(Event::Input, Clock { mono: CHECKPOINT_SECS, unix: 1_800_000_300 }, &mut ctx_n(0));
        assert!(matches!(&fx[..], [SessionFx::Persist(s)] if s.started == 1_800_000_000 && s.last_active == 1_800_000_300), "{fx:?}");
    }

    /// Session bodies are serialised with sorted keys, so `attrs` leads. The recogniser reads that
    /// prefix instead of parsing the body: if a serialisation change moves it, THIS fails loudly.
    #[test]
    fn a_serialised_session_begins_with_the_prefix_the_recogniser_reads() {
        let f = classify_unfinished(stored("d"), 0, 1_800_100_000, "d").unwrap();
        let body = serde_json::to_vec(&f.payload()).unwrap();
        assert!(body.starts_with(SESSION_BODY_PREFIX), "{}", String::from_utf8_lossy(&body));
        assert!(is_session_body(&body));
        assert!(!is_session_body(br#"{"sid":"x","started":1,"attrs":{}}"#), "not the serialised shape");
        assert!(!is_session_body(br#"{"contexts":{},"event_id":"a"}"#));
    }

    fn stored(did: &str) -> Stored {
        Stored {
            sid: "11111111-1111-4111-8111-111111111111".into(),
            did: did.into(),
            release: "plxnative@1.2.3".into(),
            environment: "production".into(),
            started: 1_800_000_000,
            last_active: 1_800_000_420,
        }
    }

    #[test]
    fn an_unfinished_session_is_crashed_with_a_crash_record_and_abnormal_without() {
        let now = 1_800_100_000;
        let c = classify_unfinished(stored("d"), 1, now, "d").expect("reported");
        assert_eq!((c.status, c.errors, c.duration_s), (Status::Crashed, 1, 420));
        assert_eq!(c.stored.release, "plxnative@1.2.3", "reported under the release that ran it");
        let a = classify_unfinished(stored("d"), 0, now, "d").expect("reported");
        assert_eq!((a.status, a.errors, a.duration_s), (Status::Abnormal, 0, 420));
    }

    #[test]
    fn an_unfinished_session_of_another_crash_report_id_or_an_implausible_time_is_dropped() {
        let now = 1_800_100_000;
        assert!(classify_unfinished(stored("old"), 1, now, "new").is_none());
        assert!(classify_unfinished(stored("d"), 1, now + 40 * 86_400, "d").is_none(), "too old");
        assert!(classify_unfinished(stored("d"), 1, 5, "d").is_none(), "an unset clock");
    }

    /// The exact item a crashed session puts on the wire (field set and values), pinned because a
    /// wrong field is a silent 200 from Sentry.
    #[test]
    fn the_session_payload_is_sentrys_schema() {
        let f = classify_unfinished(stored("did-1"), 1, 1_800_100_000, "did-1").unwrap();
        assert_eq!(
            f.payload(),
            serde_json::json!({
                "sid": "11111111-1111-4111-8111-111111111111",
                "did": "did-1",
                "init": true,
                "started": "2027-01-15T08:00:00Z",
                "timestamp": "2027-01-15T08:07:00Z",
                "duration": 420,
                "status": "crashed",
                "errors": 1,
                "attrs": {"release": "plxnative@1.2.3", "environment": "production"},
            })
        );
        assert_eq!(f.event_id(), "11111111111141118111111111111111");
        assert!(is_session_body(&serde_json::to_vec(&f.payload()).unwrap()));
        assert!(!is_session_body(br#"{"event_id":"a","sid":"x","started":1,"attrs":{}}"#));
        assert!(!is_session_body(br#"{"level":"fatal"}"#));
        assert!(!is_session_body(b"not json"));
    }

    #[test]
    fn a_new_sid_is_a_version_4_uuid() {
        let sid = new_sid().expect("random source");
        let parts: Vec<&str> = sid.split('-').collect();
        assert_eq!(parts.iter().map(|p| p.len()).collect::<Vec<_>>(), [8, 4, 4, 4, 12]);
        assert!(parts[2].starts_with('4') && "89ab".contains(&parts[3][..1]), "{sid}");
        assert!(sid.bytes().all(|b| b == b'-' || b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    }

    // ---- the driver, against a scratch directory ----------------------------------------------

    struct Rig {
        _serial: plx_base::testlock::Serial,
        root: std::path::PathBuf,
    }

    impl Rig {
        fn new(tag: &str, errors_id: Option<&str>) -> Self {
            let serial = plx_base::testlock::serial();
            let root = std::env::temp_dir().join(format!("plx-session-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            redirect_for_test(Some(root.clone()));
            super::super::spool::set_test_path(Some(root.join("spool.bin")));
            super::super::consent::install(consent_with(errors_id));
            Rig { _serial: serial, root }
        }
        fn state(&self) -> Option<Stored> {
            read_state()
        }
        fn spooled(&self) -> Vec<super::super::queue::Record> {
            super::super::spool::read()
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            redirect_for_test(None);
            super::super::spool::set_test_path(None);
            super::super::consent::install(super::super::consent::Consent::default());
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn consent_with(errors_id: Option<&str>) -> super::super::consent::Consent {
        super::super::consent::Consent {
            asked_version: super::super::consent::POLICY_VERSION,
            errors: errors_id.is_some(),
            errors_id: errors_id.map(str::to_string),
            install_id: Some("analytics-id".into()),
            usage: true,
            ..Default::default()
        }
    }

    fn body_of(r: &super::super::queue::Record) -> serde_json::Value {
        serde_json::from_slice(&r.body).unwrap()
    }

    #[test]
    fn nothing_is_tracked_or_persisted_without_crash_report_consent() {
        let rig = Rig::new("noconsent", None);
        start();
        input();
        tick(true);
        lifecycle(0x104);
        shutdown();
        assert!(rig.state().is_none());
        assert!(rig.spooled().is_empty());
    }

    #[test]
    fn a_clean_session_is_spooled_with_the_crash_report_id_and_never_the_analytics_id() {
        let rig = Rig::new("clean", Some("crash-id"));
        start();
        let live = rig.state().expect("persisted at start");
        assert_eq!(live.did, "crash-id");
        lifecycle(0x103);
        assert!(rig.state().is_none(), "ended, so cleared");
        let spooled = rig.spooled();
        assert_eq!(spooled.len(), 1);
        let body = body_of(&spooled[0]);
        assert_eq!(body["did"], "crash-id");
        assert_ne!(body["did"], "analytics-id");
        assert_eq!(body["status"], "exited");
        assert_eq!(body["attrs"]["release"], super::super::release());
        assert_eq!(spooled[0].event_id, live.sid.replace('-', ""));
        assert!(is_session_body(&spooled[0].body));
    }

    #[test]
    fn a_session_the_last_process_left_open_is_reported_crashed_or_abnormal_on_the_next_boot() {
        for (crashes, want, errors) in [(2usize, "crashed", 1), (0, "abnormal", 0)] {
            let rig = Rig::new("reboot", Some("crash-id"));
            start();
            let live = rig.state().expect("persisted");
            // The process dies: the in-memory machine is gone, the file remains.
            *MACHINE.lock().unwrap() = Machine::new();
            recover_and_start(crashes);
            let spooled = rig.spooled();
            assert_eq!(spooled.len(), 1, "{want}");
            let body = body_of(&spooled[0]);
            assert_eq!((body["status"].as_str(), body["errors"].as_u64()), (Some(want), Some(errors)));
            assert_eq!(body["sid"], live.sid.as_str());
            let now = rig.state().expect("the new boot began its own session");
            assert_ne!(now.sid, live.sid);
        }
    }

    /// A boot on a clock that is not set yet can neither date nor classify the old session, so its
    /// state file stays for a later boot (and no new session overwrites it); once the clock is set
    /// the same file is reported and removed.
    #[test]
    fn an_unfinished_session_survives_a_boot_on_an_unset_clock_and_is_reported_on_a_later_one() {
        let rig = Rig::new("unsetclock", Some("crash-id"));
        start();
        let live = rig.state().expect("persisted");
        *MACHINE.lock().unwrap() = Machine::new();
        recover_and_start_at(1, 12);
        assert_eq!(rig.state(), Some(live.clone()), "kept, and not replaced by a new session");
        assert!(rig.spooled().is_empty());
        recover_and_start_at(1, super::super::crashreport::now_secs());
        let spooled = rig.spooled();
        assert_eq!(spooled.len(), 1);
        assert_eq!(body_of(&spooled[0])["sid"], live.sid.as_str());
        assert_ne!(rig.state().expect("this boot's own").sid, live.sid);
    }

    #[test]
    fn a_crash_report_id_that_changed_while_the_app_was_dead_drops_the_old_session() {
        let rig = Rig::new("rotated", Some("old-id"));
        start();
        *MACHINE.lock().unwrap() = Machine::new();
        super::super::consent::install(consent_with(Some("new-id")));
        recover_and_start(1);
        assert!(rig.spooled().is_empty());
        assert_eq!(rig.state().expect("fresh session").did, "new-id");
    }

    #[test]
    fn withdrawal_deletes_the_persisted_and_the_live_session_and_the_spooled_report() {
        let rig = Rig::new("withdraw", Some("crash-id"));
        start();
        assert!(rig.state().is_some());
        // The app's order on withdrawal: publish the new decision, purge the spool, stop.
        super::super::consent::install(consent_with(None));
        stop();
        assert!(rig.state().is_none());
        input();
        assert!(rig.state().is_none(), "a withdrawn session does not come back on the next key");
        // A report spooled earlier goes with the spool's own purge of the Errors category.
        super::super::consent::install(consent_with(Some("crash-id")));
        start();
        lifecycle(0x104);
        assert_eq!(rig.spooled().len(), 1);
        super::super::spool::purge_withdrawn(&consent_with(None));
        assert!(rig.spooled().is_empty());
    }

    #[test]
    fn a_checkpoint_queued_behind_a_clear_cannot_resurrect_the_file() {
        let rig = Rig::new("race", Some("crash-id"));
        start();
        let live = rig.state().unwrap();
        let generation = GENERATION.load(Ordering::SeqCst);
        clear_state();
        assert!(rig.state().is_none());
        write_if_current(generation, &state_paths(), &live);
        assert!(rig.state().is_none(), "a write captured before the clear is refused");
        write_if_current(GENERATION.load(Ordering::SeqCst), &state_paths(), &live);
        assert_eq!(rig.state(), Some(live), "a current one lands");
    }

    #[test]
    fn a_flood_of_sessions_keeps_the_newest_few_and_leaves_crash_reports_alone() {
        use super::super::queue::{Category, Dest, Record};
        let rig = Rig::new("flood", Some("crash-id"));
        let crash = Record {
            category: Category::Errors,
            dest: Dest::Sentry,
            event_id: "c".repeat(32),
            body: br#"{"event_id":"cccccccccccccccccccccccccccccccc","level":"fatal"}"#.to_vec(),
        };
        assert!(super::super::spool::append(&crash));
        for _ in 0..(MAX_PENDING_SESSIONS + 9) {
            lifecycle(0x106); // foreground begins a session...
            lifecycle(0x104); // ...background ends it
        }
        let spooled = rig.spooled();
        let sessions = spooled.iter().filter(|r| is_session_body(&r.body)).count();
        assert_eq!(sessions, MAX_PENDING_SESSIONS);
        assert!(spooled.iter().any(|r| r.event_id == crash.event_id), "the crash report survived");
    }

    #[test]
    fn deleting_all_local_data_names_the_session_state() {
        let names: Vec<String> = plx_base::paths::telemetry_session_candidates()
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        assert!(!names.is_empty());
        assert!(names.iter().all(|n| n.ends_with("telemetry-session.json")), "{names:?}");
    }
}
