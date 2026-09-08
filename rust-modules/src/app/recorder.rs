//! The product's recorder and replay driver (restructure spec §5.3, §5.5) over the LEGACY loop.
//!
//! **What phase 2 records**: every frame's tick and present bit; every input the loop acted on
//! (SDL keys as `sym`/`wcode`/edge, remote-FIFO and lab tokens, pointer motion and clicks, the
//! four lifecycle codes); and on every frame with an input or drained effect, `st` — the hash of what IS a
//! machine today: the press (`ui::press::Press`, a `LogicalState`), the route and overlay words,
//! and the focus fingerprint (`focusprobe`), which is the legacy screens' state as one line. No
//! replay injection of adapter results is wired yet — the stores still talk to the network
//! themselves — so replay still runs LIVE against the synthetic PMS. Home's addressed results
//! are now encoded in full; the other stores still need their captured/injected result boundaries.
//! Replay compares observed Home results in frame/order/address/payload, and missing or extra
//! arrivals prevent a SAME verdict even if state hashes happen to match. This is still live-assisted.
//! The product dispatcher now records its drained library effect TAGS and screen lifecycle;
//! these tags are not complete application payloads and are not yet replay-graded/injected.
//!
//! **Current replay driver**: `plxnative-recplay=<dir>` drives the loop on the recorded ticks (the
//! `AppClock`), re-injects each frame's inputs through the same synthesis the remote FIFO uses,
//! compares `st` frame by frame, logs every mismatch as its own `replay: diverge` line and
//! CONTINUES, then logs one summary and ends the run. A landing arriving on a different frame than
//! it did when recorded is the expected source of a divergence in this phase and is reported,
//! never hidden.
//!
//! Arming is at boot only (`Writer::open` refuses any other frame). The directory is the runtime
//! root's `plxnative-recordings/latest` (not `plxnative-rec/`, which is the trigger FILE's own
//! name) — private, gitignored, refused by the outbound guard; a
//! committed fixture is `tools/plxnative-rec import` of one taken against `tests/mock_pms.py`.
#![allow(clippy::too_many_arguments)]

use serde_json::{json, Value};

use crate::ui::machine::{Canon, LogicalState, Tick};
use crate::ui::rec::{DirSink, Header, RecError, Recording, Writer};

/// The coarse boot facts handed to the recorder. `RecordedInit` adds Home's owned initial
/// contents; other machines still need to join it. This probe contains only protocol constants
/// and numbers (tests/fixtures/replay/ALPHABET.json carries its pattern).
#[derive(serde::Serialize)]
pub(super) struct AppInit {
    pub route: &'static str,
    pub session: bool,
    pub servers: u32,
    pub consent_asked: u32,
    pub consent_errors: bool,
    pub consent_usage: bool,
    pub seed: u32,
}

impl AppInit {
    pub const SHAPE: &'static str =
        "AppInit{route:str,session:bool,servers:u32,consent_asked:u32,consent_errors:bool,consent_usage:bool,seed:u32}";
}

impl LogicalState for AppInit {
    fn write(&self, w: &mut Canon) {
        w.str(self.route)
            .bool(self.session)
            .u32(self.servers)
            .u32(self.consent_asked)
            .bool(self.consent_errors)
            .bool(self.consent_usage)
            .u32(self.seed);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "route={} session={} servers={} consent={}/{}/{} seed={}",
            self.route,
            self.session as u8,
            self.servers,
            self.consent_asked,
            self.consent_errors as u8,
            self.consent_usage as u8,
            self.seed
        ));
    }
}

/// Boot contents currently captured in addition to the coarse application probe. Other store,
/// session and adapter initial conditions still need to join this before closed replay is wired.
struct RecordedInit<'a> {
    app: &'a AppInit,
    hubs: &'a crate::pms::initial::Initial,
}

impl LogicalState for RecordedInit<'_> {
    fn write(&self, w: &mut Canon) { self.app.write(w); self.hubs.write(w); }
    fn probe(&self, out: &mut String) { self.app.probe(out); }
}

impl crate::pms::initial::Sink for Canon {
    fn u32(&mut self, v: u32) { Canon::u32(self, v); }
    fn u64(&mut self, v: u64) { Canon::u64(self, v); }
    fn boolean(&mut self, v: bool) { Canon::bool(self, v); }
    fn text(&mut self, v: &str) { Canon::str(self, v); }
}

fn initial_header(app: &AppInit) -> Header {
    let hubs = crate::pms::initial::Initial::capture();
    let mut header = Header::new(state_fp(), &RecordedInit { app, hubs: &hubs });
    header.init_data = json!({"app": app, "hubs": hubs});
    header
}

/// The state SHAPE of what phase 2 hashes: bump by changing a `SHAPE` string, never silently.
///
/// **`tree:u64` joined it in phase 5b** and the bump was deliberate: the Settings family's state
/// left the legacy globals the focus fingerprint reads and became instances on the container tree,
/// so without folding `Dispatcher::state_hash` in, a replay would have graded every press inside
/// Settings, Privacy, Legal and first-run Favourites as identical — a recording that diverges by
/// opening the wrong page would have come back `SAME`. It invalidates every committed fixture,
/// which is the cost the pin below exists to make visible rather than silent.
pub(super) fn state_fp() -> u64 {
    crate::ui::rec::state_fp(&[
        crate::ui::press::Press::SHAPE,
        crate::ui::input::STATE_SHAPE,
        "AppFrame{route:str,overlay:str,focus:str,tree:u64}",
        crate::ui::containers::STATE_SHAPE,
        super::bridge::ARG_SHAPE,
        crate::ui::screen::RETURN_STATE_SHAPE,
        crate::screens::registry::PAGE_MEMORY_SHAPE,
        crate::screens::home::SHAPE,
        crate::screens::library::SHAPE[0],
        crate::screens::library::SHAPE[1],
        crate::screens::library::SHAPE[2],
        crate::screens::library::menu::SHAPE[0],
        crate::screens::library::menu::SHAPE[1],
        crate::pms::record::SHAPE,
        crate::pms::initial::SHAPE,
        crate::screens::detail::SHAPE,
        crate::screens::person::PersonScreen::SHAPE,
        crate::screens::filmography::FilmographyScreen::SHAPE,
        AppInit::SHAPE,
    ])
}

/// The hash of the frame's logical state (spec §5.4), as phase 2 defines it.
///
/// `tree` is `Dispatcher::state_hash` — every live instance's `LogicalState`, the tree's shape and
/// surface phases, the engine's focus, queue depth and queued press identities. It is folded in WHOLE rather than
/// sampled, because that function is already the spec's own definition of "the state of the
/// machines" (§5.4) and re-deriving a summary here would be a second definition to keep in step.
pub(super) fn state_hash(
    press: &crate::ui::press::Press,
    route: &str,
    overlay: &str,
    focus: &str,
    tree: u64,
) -> u64 {
    let mut c = Canon::new();
    press.write(&mut c);
    c.str(route).str(overlay).str(focus).u64(tree);
    c.finish()
}

pub(super) struct Rec {
    w: Writer,
    f: u64,
    events: bool,
    spent_ns: u64,
}

pub(super) struct Replay {
    rec: Recording,
    at: usize,
    graded: u64,
    diverged: u64,
    present_diffs: u64,
    result_diffs: u64,
    result_at: usize,
    started: bool,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultEnvelope {
    f: u64,
    t: String,
    to: String,
    req: u32,
    payload: Value,
}

impl Replay {
    fn same(&self) -> bool {
        self.diverged == 0 && self.present_diffs == 0 && self.result_diffs == 0
    }
}

pub(super) enum Recplay {
    Off,
    Recording(Rec),
    Replaying(Replay),
}

/// Observe the real application drain: library effect tags, lifecycle records and complete Home
/// result payloads. Application effect codecs and replay injection remain separate work.
impl crate::ui::dispatch::Tap<super::bridge::AppHost> for Recplay {
    fn result(&mut self, _frame: u64, addr: &crate::ui::machine::Addr, msg: &crate::screens::registry::AppMsg) {
        if let Self::Replaying(replay) = self {
            let payload = match msg {
                crate::screens::registry::AppMsg::HubsResult(result) => Some(crate::pms::record::encode(result)),
                _ => None,
            };
            let frame = replay.rec.frames.get(replay.at);
            let expected = frame.and_then(|f| f.results.get(replay.result_at));
            let matches = expected.is_some_and(|e| {
                payload.as_ref().is_some_and(|p| e.get("payload") == Some(p))
                    && e["to"] == machine_name(addr.to) && e["req"] == addr.req.0
            });
            if !matches {
                replay.result_diffs += 1;
                // The payload may be household data. Only the frame, ordinal and finite reason
                // belong in the shareable event log, never either side's serialized result.
                crate::log(&format!("replay: result diverge f={} index={} reason={}",
                    frame.map_or(replay.at as u64, |f| f.f), replay.result_at,
                    if expected.is_some() { "changed" } else { "extra" }));
            }
            replay.result_at += 1;
            return;
        }
        let Self::Recording(rec) = self else { return };
        let crate::screens::registry::AppMsg::HubsResult(result) = msg else { return };
        let start = std::time::Instant::now();
        rec.w.result(rec.f, &machine_name(addr.to), addr.req.0, crate::pms::record::encode(result));
        rec.events = true;
        rec.spent_ns += start.elapsed().as_nanos() as u64;
    }
    fn effect(&mut self, _frame: u64, stamped: &crate::ui::machine::Stamped<super::bridge::AppHost>) {
        use crate::ui::machine::{Delivery, Fx, MachineId};
        use crate::ui::screen::ScreenEvent;
        let Self::Recording(rec) = self else { return };
        let start = std::time::Instant::now();
        let name = match &stamped.fx {
            Fx::Nav(_) => "Nav", Fx::Mount(_) => "Mount", Fx::Unmount(_) => "Unmount",
            Fx::Deliver(_, _) => "Deliver", Fx::Timer { .. } => "Timer",
            Fx::CancelTimer(_) => "CancelTimer", Fx::Press(_) => "Press",
            Fx::Remember { .. } => "Remember", Fx::Log(_) => "Log", Fx::App(_) => "App",
        };
        let address = match &stamped.fx {
            Fx::Deliver(to, Delivery::Screen(ScreenEvent::Async(req, _))) => Some((machine_name(*to), req.0)),
            _ => None,
        };
        rec.w.effect(rec.f, &machine_name(stamped.from), name, address);
        if let Fx::Deliver(MachineId::Instance(id), Delivery::Screen(event)) = &stamped.fx {
            if matches!(event, ScreenEvent::Mount | ScreenEvent::Enter(_) | ScreenEvent::RestoreMemory(_)
                | ScreenEvent::Cover | ScreenEvent::Uncover | ScreenEvent::WillLeave(_)
                | ScreenEvent::Unmount | ScreenEvent::Suspend | ScreenEvent::Resume) {
                rec.w.life(rec.f, id.0, event.name());
            }
        }
        rec.events = true;
        rec.spent_ns += start.elapsed().as_nanos() as u64;
    }
}

fn machine_name(id: crate::ui::machine::MachineId) -> String {
    use crate::ui::machine::MachineId;
    match id {
        MachineId::Instance(id) => format!("inst:{}", id.0),
        MachineId::Store(id) => format!("store:{}", id.0),
        MachineId::Session => "Session".into(), MachineId::Consent => "Consent".into(),
        MachineId::Input => "Input".into(), MachineId::Present => "Present".into(),
        MachineId::Nav => "Nav".into(), MachineId::Player => "Player".into(),
        MachineId::Cache => "Cache".into(),
    }
}

impl Recplay {
    /// Read the two triggers ONCE at boot. Both armed is refused (a run cannot record its own
    /// replay); a recording is refused on a boot that is not frame 0 by `Writer::open` itself.
    pub(super) fn arm(init: &AppInit, triggers: Vec<String>) -> Recplay {
        let rec = crate::dev::read("rec");
        let play = crate::dev::read("recplay");
        match (rec, play) {
            (Some(_), Some(_)) => {
                crate::log("rec: REFUSED — plxnative-rec and plxnative-recplay are both armed");
                Recplay::Off
            }
            (Some(opt), None) => {
                let dir = crate::paths::runtime_dir().join("plxnative-recordings").join("latest");
                let _ = std::fs::remove_dir_all(&dir);
                let sink = match DirSink::create(&dir) {
                    Ok(s) => s,
                    Err(e) => {
                        crate::log(&format!("rec: REFUSED — cannot create the directory: {e}"));
                        return Recplay::Off;
                    }
                };
                // Capture only for an armed recording. An ordinary/shipping boot pays no catalog
                // clone, and the worker mailbox remains for the first recorded result ingest.
                let mut header = initial_header(init);
                header.clock_start_ms = super::clock::now();
                header.build = env!("PLX_VERSION").to_string();
                header.features = features();
                header.triggers = triggers;
                header.blobs = opt == "blobs";
                match Writer::open(Box::new(sink), &header, 0) {
                    Ok(w) => {
                        crate::log(&format!("rec: recording to {}", dir.display()));
                        Recplay::Recording(Rec {
                            w,
                            f: 0,
                            events: false,
                            spent_ns: 0,
                        })
                    }
                    Err(e) => {
                        crate::log(&format!("rec: REFUSED — {e:?}"));
                        Recplay::Off
                    }
                }
            }
            (None, Some(dir)) => {
                let path = std::path::Path::new(&dir);
                match Recording::load(path, state_fp()) {
                    Ok(rec) => {
                        crate::log(&format!(
                            "replay: {} frames from {} (build {} recorded, {} replaying)",
                            rec.frames.len(),
                            path.display(),
                            rec.header.build,
                            env!("PLX_VERSION")
                        ));
                        if let Some(diff) = triggers_differ(&rec.header.triggers, &triggers) {
                            crate::log(&format!("replay: TRIGGERS DIFFER — {diff}"));
                        }
                        let mut probe = String::new();
                        init.probe(&mut probe);
                        if probe != rec.header.init_probe {
                            crate::log(&format!(
                                "replay: INITIAL CONDITIONS DIFFER — recorded [{}] now [{}]",
                                rec.header.init_probe, probe
                            ));
                        }
                        Recplay::Replaying(Replay {
                            rec,
                            at: 0,
                            graded: 0,
                            diverged: 0,
                            present_diffs: 0,
                            result_diffs: 0,
                            result_at: 0,
                            started: false,
                        })
                    }
                    Err(RecError::Schema { theirs, ours }) => {
                        crate::log(&format!("replay: REFUSED — schema {theirs} recorded, {ours} here"));
                        Recplay::Off
                    }
                    Err(RecError::StateShape { theirs, ours }) => {
                        crate::log(&format!(
                            "replay: REFUSED — state shape {theirs:#x} recorded, {ours:#x} here"
                        ));
                        Recplay::Off
                    }
                    Err(e) => {
                        crate::log(&format!("replay: REFUSED — {e:?}"));
                        Recplay::Off
                    }
                }
            }
            (None, None) => Recplay::Off,
        }
    }

    /// Replay: the recording boot's clock at arming — what the replaying boot's own origin
    /// (`App.t0`, the dev-script and heartbeat origin) is re-seated to, so a delay measured from
    /// boot means the same thing on both sides.
    pub(super) fn clock_start(&self) -> Option<u32> {
        match self {
            Recplay::Replaying(r) => Some(r.rec.header.clock_start_ms),
            _ => None,
        }
    }

    /// Replay: the recorded tick of the NEXT frame, or `None` when the recording is exhausted.
    pub(super) fn replay_tick(&self) -> Option<Tick> {
        match self {
            Recplay::Replaying(r) => r.rec.frames.get(r.at).and_then(|f| f.tick),
            _ => None,
        }
    }

    /// Replay: the inputs recorded for the current frame, to be re-injected before ingest.
    pub(super) fn replay_inputs(&self) -> Vec<Value> {
        match self {
            Recplay::Replaying(r) => r.rec.frames.get(r.at).map(|f| f.inputs.clone()).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// `None` selects the live adapter; `Some(empty)` is a recorded frame with NO arrivals and
    /// must never fall back to a live mailbox. Decode the whole frame before delivering any of it.
    /// The caller must supply the bootstrap's recorded-client bindings; there is no registry
    /// lookup or best-effort rebinding here. Product boot restoration still needs to wire this.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn replay_results(
        &self,
        mut client: impl FnMut(u32) -> Option<&'static crate::plex::Client>,
    ) -> Result<Option<super::bridge::AppResults>, &'static str> {
        let Self::Replaying(replay) = self else { return Ok(None) };
        let Some(frame) = replay.rec.frames.get(replay.at) else { return Ok(Some(Vec::new())) };
        let mut out = Vec::with_capacity(frame.results.len());
        for value in &frame.results {
            let envelope: ResultEnvelope = serde_json::from_value(value.clone())
                .map_err(|_| "invalid result envelope")?;
            let to = crate::ui::machine::MachineId::Store(crate::stores::StoreId::Hubs.ord());
            if envelope.f != frame.f || envelope.t != "async" || envelope.to != machine_name(to) {
                return Err("unsupported result envelope");
            }
            let result = crate::pms::record::decode(envelope.payload, &mut client)?;
            if result.request_id() != envelope.req { return Err("result request mismatch"); }
            out.push((crate::ui::machine::Addr { to, req: crate::ui::machine::RequestId(envelope.req) },
                crate::screens::registry::AppMsg::HubsResult(result)));
        }
        Ok(Some(out))
    }

    pub(super) fn tick(&mut self, now: u32, dt: f32) {
        if let Recplay::Recording(r) = self {
            let t0 = std::time::Instant::now();
            r.w.tick(r.f, Tick { ms: now, dt_us: (dt * 1_000_000.0) as u32 });
            r.spent_ns += t0.elapsed().as_nanos() as u64;
        }
    }

    pub(super) fn input(&mut self, encoded: Value) {
        if let Recplay::Recording(r) = self {
            let t0 = std::time::Instant::now();
            r.w.input(r.f, encoded);
            r.events = true;
            r.spent_ns += t0.elapsed().as_nanos() as u64;
        }
    }

    pub(super) fn present(&mut self, bit: bool) {
        match self {
            Recplay::Recording(r) => {
                let t0 = std::time::Instant::now();
                r.w.present(r.f, bit, None);
                r.spent_ns += t0.elapsed().as_nanos() as u64;
            }
            Recplay::Replaying(r) => {
                if let Some(fr) = r.rec.frames.get(r.at) {
                    if let Some(rec_bit) = fr.present {
                        if rec_bit != bit {
                            r.present_diffs += 1;
                            crate::log(&format!("replay: present f={} recorded={rec_bit} got={bit}", fr.f));
                        }
                    }
                }
            }
            Recplay::Off => {}
        }
    }

    /// The frame's tail. `hash` is computed only when a state record is due (an event frame while
    /// recording; a graded frame while replaying). Returns `true` when a replay has just ended.
    pub(super) fn end_frame(&mut self, hash: &dyn Fn() -> u64) -> bool {
        match self {
            Recplay::Off => false,
            Recplay::Recording(r) => {
                let t0 = std::time::Instant::now();
                if r.events {
                    r.w.state(r.f, hash());
                }
                if let Err(e) = r.w.flush_frame() {
                    crate::log(&format!("rec: write failed, stopping: {e:?}"));
                }
                r.events = false;
                r.f += 1;
                r.spent_ns += t0.elapsed().as_nanos() as u64;
                false
            }
            Recplay::Replaying(r) => {
                r.started = true;
                if let Some(fr) = r.rec.frames.get(r.at) {
                    for index in r.result_at..fr.results.len() {
                        r.result_diffs += 1;
                        crate::log(&format!("replay: result diverge f={} index={} reason=missing", fr.f, index));
                    }
                    if let Some(expected) = fr.st {
                        r.graded += 1;
                        let got = hash();
                        if got != expected {
                            r.diverged += 1;
                            crate::log(&format!(
                                "replay: diverge f={} expected={expected:#018x} got={got:#018x} inputs={}",
                                fr.f,
                                fr.inputs.len()
                            ));
                        }
                    }
                }
                r.result_at = 0;
                r.at += 1;
                if r.at >= r.rec.frames.len() {
                    crate::log(&format!(
                        "replay: done frames={} graded={} diverged={} present_diffs={} result_diffs={} verdict={}",
                        r.rec.frames.len(),
                        r.graded,
                        r.diverged,
                        r.present_diffs,
                        r.result_diffs,
                        if r.same() { "SAME" } else { "DIVERGED" }
                    ));
                    return true;
                }
                false
            }
        }
    }

    /// Microseconds the recorder spent this second — the heartbeat's `rec=`; resets.
    pub(super) fn take_spent_us(&mut self) -> Option<u64> {
        match self {
            Recplay::Recording(r) => {
                let us = r.spent_ns / 1000;
                r.spent_ns = 0;
                Some(us)
            }
            _ => None,
        }
    }

    pub(super) fn finish(self) {
        if let Recplay::Recording(r) = self {
            r.w.finish();
            crate::log("rec: finished");
        }
    }
}

/// The recorded boot's trigger set against this boot's, as one line naming what is missing and
/// what is extra (the recorder's own `plxnative-rec` and the replay's `plxnative-recplay` are
/// the expected difference and are not reported). `None` when the sets agree. Dev flags reach
/// the loop from the filesystem until phase 4 turns them into recorded `Sys` results, so this is
/// phase 2's assertion that a replay boot was armed the way the recording boot was.
pub(super) fn triggers_differ(recorded: &[String], now: &[String]) -> Option<String> {
    let skip = |n: &str| n == "plxnative-rec" || n == "plxnative-recplay";
    let missing: Vec<&str> = recorded
        .iter()
        .map(String::as_str)
        .filter(|n| !skip(n) && !now.iter().any(|m| m == n))
        .collect();
    let extra: Vec<&str> = now
        .iter()
        .map(String::as_str)
        .filter(|n| !skip(n) && !recorded.iter().any(|m| m == n))
        .collect();
    if missing.is_empty() && extra.is_empty() {
        return None;
    }
    Some(format!("missing=[{}] extra=[{}]", missing.join(","), extra.join(",")))
}

fn features() -> Vec<String> {
    let mut v = Vec::new();
    if cfg!(feature = "devtools") {
        v.push("devtools".into());
    }
    if cfg!(feature = "devtriggers") {
        v.push("devtriggers".into());
    }
    if cfg!(feature = "hostsim") {
        v.push("hostsim".into());
    }
    if cfg!(feature = "lab-diagnostics") {
        v.push("lab-diagnostics".into());
    }
    v
}

/// Input encodings — the application's half of the codec (spec §5.5); `replay_inputs` hands
/// these back to the loop, which re-injects them by kind.
pub(super) fn enc_key(sym: u32, wcode: u32, down: bool, repeat: bool) -> Value {
    json!({"kind": "key", "sym": sym, "wcode": wcode, "down": down, "repeat": repeat})
}

pub(super) fn enc_token(tok: &str) -> Value {
    json!({"kind": "token", "tok": tok})
}

pub(super) fn enc_pointer(kind: &str, x: i32, y: i32) -> Value {
    json!({"kind": kind, "x": x, "y": y})
}

pub(super) fn enc_lifecycle(code: u32) -> Value {
    json!({"kind": "lifecycle", "code": code})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_header_contains_home_boot_contents_and_hashes_hidden_state() {
        let _guard = crate::testlock::serial();
        crate::pms::seed_for_test(2, crate::pms::HubState::Ready);
        let app = AppInit { route: "home", session: false, servers: 1, consent_asked: 0,
            consent_errors: false, consent_usage: false, seed: 0 };
        let header = initial_header(&app);
        assert_eq!(header.init_data["hubs"]["catalog"]["items"].as_array().unwrap().len(), 2);
        let mut data = header.init_data["hubs"].clone();
        let original: crate::pms::initial::Initial = serde_json::from_value(data.clone()).unwrap();
        assert_eq!(RecordedInit { app: &app, hubs: &original }.hash(), header.init_hash);
        data["sources"][0]["retry_n"] = json!(123);
        let changed: crate::pms::initial::Initial = serde_json::from_value(data).unwrap();
        assert_ne!(RecordedInit { app: &app, hubs: &changed }.hash(), header.init_hash);
        crate::pms::reset();
    }

    #[test]
    fn replay_grades_result_payloads_addresses_order_and_missing_or_extra_arrivals() {
        use crate::ui::dispatch::Tap;
        use crate::ui::machine::{Addr, MachineId, RequestId};
        use crate::screens::registry::AppMsg;
        let _guard = crate::testlock::serial();
        crate::pms::seed_for_test(1, crate::pms::HubState::Ready);
        crate::pms::queue_test_landing(Some(2));
        crate::pms::queue_test_landing(Some(3));
        let results = crate::stores::hubs::take_results();
        let addr = Addr { to: MachineId::Store(crate::stores::StoreId::Hubs.ord()), req: RequestId(results[0].request_id()) };
        let expected: Vec<_> = results.iter().map(|r| json!({ "f": 0, "t": "async",
            "to": machine_name(addr.to), "req": addr.req.0, "payload": crate::pms::record::encode(r) })).collect();
        let boot = |expected: Vec<Value>| {
            let init = AppInit { route: "home", session: false, servers: 1, consent_asked: 0,
                consent_errors: false, consent_usage: false, seed: 0 };
            Recplay::Replaying(Replay {
                rec: Recording { header: Header::new(state_fp(), &init),
                    frames: vec![crate::ui::rec::Frame { f: 0, results: expected, st: Some(7), ..Default::default() }],
                    metrics: Default::default(), stopped_at: None },
                at: 0, graded: 0, diverged: 0, present_diffs: 0, result_diffs: 0,
                result_at: 0, started: false,
            })
        };
        assert!(Recplay::Off.replay_results(|_| None).unwrap().is_none());
        assert!(boot(vec![]).replay_results(|_| None).unwrap().unwrap().is_empty());
        let decoded = boot(expected.clone()).replay_results(|_| None).unwrap().unwrap();
        assert_eq!(decoded.len(), 2);
        for ((got_addr, msg), expected) in decoded.iter().zip(&expected) {
            assert_eq!(*got_addr, addr);
            let AppMsg::HubsResult(result) = msg else { unreachable!() };
            assert_eq!(crate::pms::record::encode(result), expected["payload"]);
        }
        for (key, value) in [("f", json!(1)), ("t", json!("in")), ("to", json!("store:4")),
            ("req", json!(addr.req.0 + 1)), ("unknown", json!(true)), ("payload", json!({}))] {
            let mut invalid = expected[1].clone();
            invalid[key] = value;
            // A good first record does not authorize a partially decoded frame.
            assert!(boot(vec![expected[0].clone(), invalid]).replay_results(|_| None).is_err());
        }
        // The state hash matches in EVERY case. It cannot excuse an unconsumed or changed result.
        for (order, count) in [(vec![0, 1], 0), (vec![1, 0], 2), (vec![0], 1),
            (vec![], 2), (vec![0, 1, 0], 1)] {
            let mut replay = boot(expected.clone());
            for i in order { replay.result(99, &addr, &AppMsg::HubsResult(results[i].clone())); }
            assert!(replay.end_frame(&|| 7));
            let Recplay::Replaying(r) = replay else { unreachable!() };
            assert_eq!(r.diverged, 0);
            assert_eq!(r.result_diffs, count);
            assert_eq!(r.same(), count == 0);
        }
        for wrong in [Addr { req: RequestId(addr.req.0 + 1), ..addr },
            Addr { to: MachineId::Store(crate::stores::StoreId::Search.ord()), ..addr }] {
            let mut replay = boot(vec![expected[0].clone()]);
            replay.result(99, &wrong, &AppMsg::HubsResult(results[0].clone()));
            replay.end_frame(&|| 7);
            let Recplay::Replaying(r) = replay else { unreachable!() };
            assert_eq!(r.result_diffs, 1);
            assert!(!r.same());
        }
        // Late results must not be "matched" across frames, and reporting one missing result
        // must not prevent the next frame from being graded independently.
        let mut replay = boot(vec![expected[0].clone()]);
        if let Recplay::Replaying(r) = &mut replay {
            let mut later = expected[1].clone();
            later["f"] = json!(1);
            r.rec.frames.push(crate::ui::rec::Frame {
                f: 1, results: vec![later], st: Some(7), ..Default::default()
            });
        }
        assert!(!replay.end_frame(&|| 7)); // missing at frame 0
        replay.result(100, &addr, &AppMsg::HubsResult(results[1].clone()));
        assert!(replay.end_frame(&|| 7));
        let Recplay::Replaying(r) = replay else { unreachable!() };
        assert_eq!(r.graded, 2);
        assert_eq!(r.result_diffs, 1, "the next frame starts at result ordinal zero");
        assert!(!r.same());
        crate::pms::reset();
    }

    #[test]
    fn the_application_bridge_records_its_real_drain_and_lifecycle() {
        let _guard = crate::testlock::serial();
        crate::browse::reset();
        crate::pms::seed_for_test(1, crate::pms::HubState::Ready);
        let init = AppInit { route: "home", session: false, servers: 1, consent_asked: 0,
            consent_errors: false, consent_usage: false, seed: 0 };
        let sink = crate::ui::rec::MemSink::default();
        let segments = sink.segments.clone();
        let writer = Writer::open(Box::new(sink), &Header::new(state_fp(), &init), 0).unwrap();
        let mut rec = Recplay::Recording(Rec { w: writer, f: 0, events: false, spent_ns: 0 });
        let mut d = crate::ui::dispatch::Dispatcher::<super::super::bridge::AppHost>::new();
        let mut rig = super::super::bridge::Bridge::for_test(|| 0);
        rec.tick(0, 0.016);
        let request = crate::pms::queue_test_landing(Some(3));
        super::super::bridge::frame_with_tap(&mut d, &mut rig, super::super::Route::Home,
            &super::super::Trail::new(), Tick { ms: 0, dt_us: 16000 }, vec![], &mut rec);
        rec.end_frame(&|| d.state_hash());
        let bytes = segments.borrow()[0].clone();
        let text = String::from_utf8(bytes).unwrap();
        let records: Vec<Value> = text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        assert!(records.iter().any(|r| r["t"] == "eff"), "the bridge must not discard its observer");
        let results: Vec<_> = records.iter().filter(|r| r["t"] == "async").collect();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["req"], request);
        assert_eq!(results[0]["to"], "store:1");
        let decoded = crate::pms::record::decode(results[0]["payload"].clone(), |_| None).unwrap();
        assert_eq!(decoded.request_id(), request);
        assert_eq!(results[0]["payload"]["build"]["shelves"][0]["items"].as_array().unwrap().len(), 3);
        for event in ["mount", "enter"] {
            assert!(records.iter().any(|r| r["t"] == "life" && r["ev"] == event));
        }
        assert!(records.iter().any(|r| r["t"] == "st"), "drained effects make this a graded frame");
        assert!(records.iter().all(|r| r["f"] == 0), "use the recorder's frame origin, not dispatcher frame 1");
        // Exercise writer → frame reader → the real dispatcher/store, not just two codec
        // helpers. This reuses the current store epoch; it deliberately does not pretend to
        // restore full initial conditions or grade a whole scenario's state fingerprint.
        let mut replay = Recplay::Replaying(Replay {
            rec: Recording { header: Header::new(state_fp(), &init), frames: vec![crate::ui::rec::Frame {
                f: 0, results: results.into_iter().cloned().collect(), ..Default::default()
            }], metrics: Default::default(), stopped_at: None },
            at: 0, graded: 0, diverged: 0, present_diffs: 0, result_diffs: 0,
            result_at: 0, started: false,
        });
        let supplied = replay.replay_results(|_| None).unwrap().unwrap();
        crate::pms::queue_test_landing(Some(9));
        let before = crate::pms::catalog_gen();
        super::super::bridge::frame_with_results(&mut d, &mut rig, super::super::Route::Home,
            &super::super::Trail::new(), Tick { ms: 16, dt_us: 16000 }, vec![], || supplied, &mut replay);
        assert!(crate::pms::catalog_gen() > before, "the supplied result was applied");
        assert_eq!(crate::pms::hub_len(0), 3);
        assert_eq!(crate::stores::hubs::take_results().len(), 1, "live arrivals were not consumed");
        let Recplay::Replaying(r) = replay else { unreachable!() };
        assert_eq!(r.result_at, 1);
        assert_eq!(r.result_diffs, 0);
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
    }

    #[test]
    fn the_init_probe_is_synthetic_and_the_shape_is_pinned() {
        let init = AppInit {
            route: "home",
            session: true,
            servers: 1,
            consent_asked: 3,
            consent_errors: true,
            consent_usage: false,
            seed: 7,
        };
        let mut p = String::new();
        init.probe(&mut p);
        assert_eq!(p, "route=home session=1 servers=1 consent=3/1/0 seed=7");
        // Phase 8: Home initial contents and their canonical traversal join the recording shape.
        // Older fixtures did not capture these contents and must be refused, not rebaselined.
        assert_eq!(state_fp(), 0x6c35_6005_c114_626c);
    }

    #[test]
    fn a_replay_boot_armed_differently_from_the_recording_is_reported() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            triggers_differ(&s(&["plxnative-focus", "plxnative-rec"]), &s(&["plxnative-focus", "plxnative-recplay"])),
            None,
            "the two driver triggers are the expected difference"
        );
        assert_eq!(
            triggers_differ(&s(&["plxnative-focus", "plxnative-grid"]), &s(&["plxnative-focus", "plxnative-noidle"])),
            Some("missing=[plxnative-grid] extra=[plxnative-noidle]".to_string())
        );
    }

    #[test]
    fn the_pre_content_navigation_recording_shape_is_refused() {
        for old in [0x8446_64d2_3399_0e72, 0x002c_b89e_e6a9_3668, 0x51ac_a85c_c16b_4b59,
            0x8af1_d09e_bbb1_1d47, 0x76d4_1ddb_e172_6b88, 0x702b_f9f7_e7c8_fe57,
            0x7252_4cf7_ed8d_97a3, 0x5ba4_34ad_d5db_5bdf, 0xe61b_6d55_f442_8637] {
            let manifest = format!(r#"{{"schema": {}, "state_fp": {old}}}"#, crate::ui::rec::SCHEMA);
            assert_eq!(crate::ui::rec::Recording::parse(&manifest, &[], state_fp()).err(),
                Some(crate::ui::rec::RecError::StateShape { theirs: old, ours: state_fp() }));
        }
    }

    #[test]
    fn the_state_hash_moves_with_the_focus_line_and_the_press() {
        let mut press = crate::ui::press::Press::new();
        let a = state_hash(&press, "home", "", "focus route=home sel=0", 0);
        let b = state_hash(&press, "home", "", "focus route=home sel=1", 0);
        assert_ne!(a, b);
        press.begin(10);
        let c = state_hash(&press, "home", "", "focus route=home sel=0", 0);
        assert_ne!(a, c);
    }

    /// …and with the TREE, which is the half phase 5b added. Every screen in the Settings family
    /// is an instance on the dispatcher whose state the focus fingerprint cannot see: without
    /// this term a replay that opened Legal instead of Privacy would hash identically to one that
    /// did not, and come back `verdict=SAME`.
    #[test]
    fn the_state_hash_moves_with_the_container_tree() {
        let press = crate::ui::press::Press::new();
        let a = state_hash(&press, "home", " overlay=settings", "focus route=home", 0x11);
        let b = state_hash(&press, "home", " overlay=settings", "focus route=home", 0x12);
        assert_ne!(a, b, "the same page and focus over a different tree is a different state");
    }
}
