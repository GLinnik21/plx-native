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
//! CONTINUES, then logs one summary and ends the run.
//!
//! **The landing SCHEDULE (phase 11, §3.3 step 3).** The stores still fetch live during a replay,
//! but the frame a result is OBSERVED on is no longer whatever the network and the thread
//! scheduler produced. Every landing SITE — Home's hubs through `bridge::take_live_results`, and
//! each legacy pump's mailbox take (`metadata` detail/season/alt-sources, `person`, `viewstate`,
//! `browse`'s four, `search`'s per-source slot) — consumes its mailbox through `ui::landgate`,
//! which during a replay holds an EARLY arrival until the frame the recording consumed it on. A
//! LATE arrival is delivered at once and counted, exactly as before: holding cannot manufacture a
//! result that has not come. An arrival the recording never saw is `extra`; a recorded landing
//! this run never produced is `missing`, reported when the replay ends. All three ride the
//! verdict as `land_diffs`.
//!
//! Three things about the shape are deliberate, and each was a wrong turn first.
//! **(1) The gate wraps the TAKE, never the pump.** A pump both lands and spawns, so gating the
//! pump would have suppressed the request whose landing it was waiting for — turning every browse
//! and search landing into a guaranteed late one.
//! **(2) The schedule is per STORE and per FRAME, deduplicated on both sides** — one `land`
//! record per (frame, store), one cursor step per (frame, store) — so a store with five landing
//! sites needs no site identity of its own and the recording stays one line per frame per store.
//! **(3) It is a SCHEMA change** (`ui::rec::SCHEMA` 1 → 2): a schema-1 recording carries no `land`
//! records at all, so replaying one under the gate would grade nothing while looking as though it
//! graded everything. It is refused instead, and `tools/plxnative-rec rerecord` is the verb.
//!
//! Measured on flow 12 (2026-09-10, three runs of three): the recording's single `async` record
//! sat on frame 1, every replay observed it on frame 0, and because a spring started one frame
//! earlier never re-converges bit for bit, 927 of 928 frames diverged.
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
pub(crate) struct AppInit {
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

/// **The LOOP's own half of the shape census.** Every SCREEN's shape — and the argument and page
/// memory that mount one — is `screens::registry::SCREEN_SHAPES`, declared in the module a new
/// screen is added to (§0 criterion 5: a conversion touches `screens/<name>.rs`, the registry,
/// `dev/scenarios.rs` and `tests/manifest.json` and nothing else, and this file used to be a
/// fifth). These are what is left: the press machine, the input state, the frame line, the
/// container tree, the return state, the recorded PMS fixtures and the app's own init.
///
/// Pinned by `the_app_shape_census_is_pinned` below, which does NOT move when a screen lands —
/// the registry's own pin does that, beside the entry that caused it.
const APP_SHAPES: &[&str] = &[
    crate::ui::press::Press::SHAPE,
    crate::ui::input::STATE_SHAPE,
    "TextInputWire{kind:text,text:str,panel:bool,ms:u32,dt_us:u32,source:{Sdl,RemoteFifo,Script,Replay}}",
    "AppFrame{route:str,overlay:str,focus:str,tree:u64}",
    crate::ui::containers::STATE_SHAPE,
    crate::ui::screen::RETURN_STATE_SHAPE,
    crate::pms::record::SHAPE,
    crate::pms::initial::SHAPE,
    AppInit::SHAPE,
];

/// The state SHAPE of what phase 2 hashes: bump by changing a `SHAPE` string, never silently.
///
/// **`tree:u64` joined it in phase 5b** and the bump was deliberate: the Settings family's state
/// left the legacy globals the focus fingerprint reads and became instances on the container tree,
/// so without folding `Dispatcher::state_hash` in, a replay would have graded every press inside
/// Settings, Privacy, Legal and first-run Favourites as identical — a recording that diverges by
/// opening the wrong page would have come back `SAME`. It invalidates every committed fixture,
/// which is the cost the pin below exists to make visible rather than silent.
///
/// **Phase 9 bumps it twice over**: `ARG_SHAPE` lost `Player{overlay:…}` and gained
/// `PlayerOverlay{…}`, and the player itself now contributes state at all — its HUD timer, cursor
/// and scrub gesture were `static mut`s and `TX` atomics that no `LogicalState` could see, so a
/// recording that diverged by leaving the transport up, or by scrubbing to a different second,
/// came back `SAME`.
///
/// **Phase 10 bumps it once per page panel converted.** The Detail page's *Also available* picker
/// contributes `screens::alt_sources::SHAPE` and `ARG_SHAPE` gains its `AltSources` variant; the
/// page's own `screens::detail::SHAPE` narrows its `panel:u8` at the same time, because which panel
/// is up is the CONTAINER's record now (`Navigation::write` writes every surface's argument, phase
/// and instance hash) and a second copy on the page would be two producers of one fact. The
/// *Track information* sheet is the second bump: `screens::tracks_panel::SHAPE` joins the
/// inventory and `ARG_SHAPE` gains `TracksPanel{page:i32}`. Its PAGE is in the shape deliberately
/// — that cursor moves nothing else in the app, so without it a replay grades the sheet opening
/// and closing and nothing between. Every committed fixture is invalidated by each bump, which is
/// the cost this pin exists to make visible rather than silent — `tools/plxnative-rec rerecord` is
/// the verb (`tests/fixtures/replay/README.md`).
pub(crate) fn state_fp() -> u64 {
    let mut shapes: Vec<&str> = APP_SHAPES.to_vec();
    shapes.extend_from_slice(crate::screens::registry::SCREEN_SHAPES);
    crate::ui::rec::state_fp(&shapes)
}

/// The hash of the frame's logical state (spec §5.4), as phase 2 defines it.
///
/// `tree` is `Dispatcher::state_hash` — every live instance's `LogicalState`, the tree's shape and
/// surface phases, the engine's focus, queue depth and queued press identities. It is folded in WHOLE rather than
/// sampled, because that function is already the spec's own definition of "the state of the
/// machines" (§5.4) and re-deriving a summary here would be a second definition to keep in step.
pub(crate) fn state_hash(
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

pub(crate) struct Rec {
    w: Writer,
    f: u64,
    events: bool,
    spent_ns: u64,
}

pub(crate) struct Replay {
    rec: Recording,
    at: usize,
    graded: u64,
    diverged: u64,
    present_diffs: u64,
    result_diffs: u64,
    /// Landings observed on a frame other than the one the recording observed them on, plus the
    /// recorded landings this run never produced (`ui::landgate`, §3.3 step 3).
    land_diffs: u64,
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
        self.diverged == 0 && self.present_diffs == 0 && self.result_diffs == 0 && self.land_diffs == 0
    }
}

pub(crate) enum Recplay {
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
    pub(crate) fn arm(init: &AppInit, triggers: Vec<String>) -> Recplay {
        let rec = crate::dev::scenarios::rec_trigger();
        let play = crate::dev::scenarios::recplay_trigger();
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
                        // From here every landing SITE stamps the frame it consumed its mailbox
                        // on — the schedule a replay of this recording is held to (§3.3 step 3).
                        crate::ui::landgate::arm_recording();
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
                        // The landing SCHEDULE: from here a live arrival is held to the frame the
                        // recording consumed it on (§3.3 step 3, `ui::landgate`).
                        crate::ui::landgate::arm_replay(rec.land_schedule());
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
                            land_diffs: 0,
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
    pub(crate) fn clock_start(&self) -> Option<u32> {
        match self {
            Recplay::Replaying(r) => Some(r.rec.header.clock_start_ms),
            _ => None,
        }
    }

    /// The loop's frame index, published to `ui::landgate` before any landing site runs. One
    /// relaxed atomic load when neither trigger is armed.
    pub(crate) fn begin_frame(&self) {
        match self {
            Recplay::Recording(r) => crate::ui::landgate::begin_frame(r.f),
            Recplay::Replaying(r) => crate::ui::landgate::begin_frame(
                r.rec.frames.get(r.at).map_or(r.at as u64, |f| f.f),
            ),
            Recplay::Off => {}
        }
    }

    /// Replay: the recorded tick of the NEXT frame, or `None` when the recording is exhausted.
    pub(crate) fn replay_tick(&self) -> Option<Tick> {
        match self {
            Recplay::Replaying(r) => r.rec.frames.get(r.at).and_then(|f| f.tick),
            _ => None,
        }
    }

    /// Replay: the inputs recorded for the current frame, to be re-injected before ingest.
    pub(crate) fn replay_inputs(&self) -> Vec<Value> {
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
    pub(crate) fn replay_results(
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

    pub(crate) fn tick(&mut self, now: u32, dt: f32) {
        if let Recplay::Recording(r) = self {
            let t0 = std::time::Instant::now();
            r.w.tick(r.f, Tick { ms: now, dt_us: (dt * 1_000_000.0) as u32 });
            r.spent_ns += t0.elapsed().as_nanos() as u64;
        }
    }

    pub(crate) fn input(&mut self, encoded: Value) {
        if let Recplay::Recording(r) = self {
            let t0 = std::time::Instant::now();
            r.w.input(r.f, encoded);
            r.events = true;
            r.spent_ns += t0.elapsed().as_nanos() as u64;
        }
    }

    pub(crate) fn present(&mut self, bit: bool) {
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
    pub(crate) fn end_frame(&mut self, hash: &dyn Fn() -> u64) -> bool {
        match self {
            Recplay::Off => false,
            Recplay::Recording(r) => {
                let t0 = std::time::Instant::now();
                // The frame's LANDING SCHEDULE (§3.3 step 3): one record per store that consumed
                // a mailbox this frame, whichever of its sites did it. Written before `st`, so a
                // reader sees the arrival above the state it produced.
                for (ord, n) in crate::ui::landgate::take_frame_lands() {
                    let gen = crate::stores::StoreId::from_ord(ord).map_or(0, crate::stores::gen);
                    r.w.land(r.f, ord.0, gen, n);
                    r.events = true;
                }
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
                // Landings the gate could not place on their recorded frame. `late` means the
                // worker was slower here than it was when recorded (holding cannot conjure a
                // result); `extra` means the recording had none left for that store.
                for (frame, ord, why) in crate::ui::landgate::take_diffs() {
                    r.land_diffs += 1;
                    crate::log(&format!("replay: land diverge f={frame} store={ord} reason={}", why.name()));
                }
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
                    // Recorded landings this run never produced. They can only be known at the
                    // end: until the recording is exhausted, "not yet" and "never" look alike.
                    for (ord, frame) in crate::ui::landgate::unmatched() {
                        r.land_diffs += 1;
                        crate::log(&format!(
                            "replay: land diverge f={frame} store={ord} reason={}",
                            crate::ui::landgate::Diff::Missing.name()
                        ));
                    }
                    crate::log(&format!(
                        "replay: done frames={} graded={} diverged={} present_diffs={} result_diffs={} land_diffs={} verdict={}",
                        r.rec.frames.len(),
                        r.graded,
                        r.diverged,
                        r.present_diffs,
                        r.result_diffs,
                        r.land_diffs,
                        if r.same() { "SAME" } else { "DIVERGED" }
                    ));
                    return true;
                }
                false
            }
        }
    }

    /// Microseconds the recorder spent this second — the heartbeat's `rec=`; resets.
    pub(crate) fn take_spent_us(&mut self) -> Option<u64> {
        match self {
            Recplay::Recording(r) => {
                let us = r.spent_ns / 1000;
                r.spent_ns = 0;
                Some(us)
            }
            _ => None,
        }
    }

    pub(crate) fn finish(self) {
        crate::ui::landgate::disarm();
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
pub(crate) fn triggers_differ(recorded: &[String], now: &[String]) -> Option<String> {
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
pub(crate) fn enc_key(sym: u32, wcode: u32, down: bool, repeat: bool) -> Value {
    json!({"kind": "key", "sym": sym, "wcode": wcode, "down": down, "repeat": repeat})
}

pub(crate) fn enc_token(tok: &str) -> Value {
    json!({"kind": "token", "tok": tok})
}

pub(crate) fn enc_text(text: &str, panel: bool, at: crate::ui::machine::Tick, source: crate::ui::machine::Source) -> Value {
    use crate::ui::machine::Source;
    let source = match source { Source::Sdl => 0, Source::RemoteFifo => 1, Source::Script => 2, Source::Replay => 3 };
    json!({"kind":"text", "text":text, "panel":panel, "ms":at.ms, "dt_us":at.dt_us, "source":source})
}

pub(crate) fn dec_text(v: &Value) -> Option<Vec<crate::ui::machine::InputEvent<u32>>> {
    use crate::ui::machine::{Source, Tick};
    if v["kind"].as_str()? != "text" { return None; }
    let source = match v["source"].as_u64()? { 0 => Source::Sdl, 1 => Source::RemoteFifo,
        2 => Source::Script, 3 => Source::Replay, _ => return None };
    let at = Tick { ms: v["ms"].as_u64()?.try_into().ok()?, dt_us: v["dt_us"].as_u64()?.try_into().ok()? };
    Some(super::events::text_inputs(v["text"].as_str()?, v["panel"].as_bool()?, at, source))
}

pub(crate) fn enc_pointer(kind: &str, x: i32, y: i32) -> Value {
    json!({"kind": kind, "x": x, "y": y})
}

pub(crate) fn enc_lifecycle(code: u32) -> Value {
    json!({"kind": "lifecycle", "code": code})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_records_preserve_commit_boundaries_clock_source_and_panel_observation() {
        use crate::ui::machine::{Canon, InputEvent, Source, Tick};
        let digest = |events: &[InputEvent<u32>]| {
            let mut c = Canon::new(); c.seq(events.len());
            for event in events { event.write_with(&mut c, &|elem, c| { c.u32(*elem); }); }
            c.finish()
        };
        let text = "synthetic whole commit длиннее тридцати двух байтов 🙂 ";
        for source in [Source::Sdl, Source::RemoteFifo, Source::Script, Source::Replay] {
            for panel in [false, true] {
                let at = Tick { ms: u32::MAX - 10, dt_us: 16_667 };
                let expected = super::super::events::text_inputs(text, panel, at, source);
                let wire = enc_text(text, panel, at, source);
                let actual = dec_text(&wire).unwrap();
                assert_eq!(actual.len(), if panel { 2 } else { 1 });
                assert_eq!(digest(&actual), digest(&expected));
                let mut invalid = wire.clone(); invalid["source"] = json!(99);
                assert!(dec_text(&invalid).is_none());
                invalid = wire.clone(); invalid["ms"] = json!(u64::from(u32::MAX) + 1);
                assert!(dec_text(&invalid).is_none());
                invalid = wire; invalid["panel"] = json!("yes");
                assert!(dec_text(&invalid).is_none());
            }
        }
        assert!(super::super::events::text_inputs("", true, Tick { ms: 0, dt_us: 0 }, Source::Sdl).is_empty());
    }

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
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
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
                at: 0, graded: 0, diverged: 0, present_diffs: 0, result_diffs: 0, land_diffs: 0,
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
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
    }

    #[test]
    fn the_application_bridge_records_its_real_drain_and_lifecycle() {
        let _guard = crate::testlock::serial();
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
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
        super::super::bridge::show_page(&mut d, super::super::AppArg::Home);
        super::super::bridge::frame_with_tap(&mut d, &mut rig,
            Tick { ms: 0, dt_us: 16000 }, vec![], &mut rec);
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
            at: 0, graded: 0, diverged: 0, present_diffs: 0, result_diffs: 0, land_diffs: 0,
            result_at: 0, started: false,
        });
        let supplied = replay.replay_results(|_| None).unwrap().unwrap();
        crate::pms::queue_test_landing(Some(9));
        let before = crate::pms::catalog_gen();
        super::super::bridge::frame_with_results(&mut d, &mut rig,
            Tick { ms: 16, dt_us: 16000 }, vec![], || supplied, &mut replay);
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
        // Phase 7: scoped modal stacks and opaque content identity memory change the shape.
        // Previous pin: 0x8446_64d2_3399_0e72. Old fixtures must be refused and rerecorded;
        // this is a schema transition, not a behavior rebaseline.
        // Phase 8: owned Library geometry, section memory, deferred actions, menus and shared
        // animation state now join Home's captured initial contents in the shape inventory.
        // This is a schema pin, not a fixture rebaseline: older shapes must still be refused.
        // Rail viewport/presence springs and their target now join the owned Library shape.
        // No recording or anchor is rewritten: the previous shape remains incompatible.
        // The open Filter menu now records its immediate, not-yet-committed desired value.
        // The diagnostic's document-end reversal direction is owned logical state too.
        // Query-reset intent and its observed query survive Library entry eviction.
        // Pending normalized input now includes its full payload, including whole text commits.
        // The owned Search state and its entry restoration payload join the inventory.
        // Input now binds accepted keyboard requests to their instance and hashes queued requests.
        // Text records include their original clock, source and observed panel capability.
        // The grid's focus pop and shrink springs join the owned Library shape.
        // Merge of main (phase 7, 0x002c_b89e_e6a9_3668) into phase 8: main's own addition to
        // `AppMsg` (`DetailRestore`) was already present at this position in phase 8's own
        // inventory, so the merge is a pure union with no new hashed term and the pin is
        // unchanged from the pre-merge phase 8 value.
        // Phase 9: the player is an owned screen, so `ARG_SHAPE` loses `Player{overlay:…}`, gains
        // `PlayerOverlay{kind}`, and the player's own state joins the inventory for the first time
        // (`PlayerScreen` + `PlayerOverlayScreen`). A schema transition, not a rebaseline: every
        // fixture recorded against 0xd1f5_9fcf_db3a_98fc must be refused and rerecorded, because a
        // recording taken before this could not hash the transport's timer, cursor or scrub at all.
        // Phase 10, the Detail page's *Also available* picker: `ARG_SHAPE` gains `AltSources{…}`,
        // `AltSourcesScreen` joins the inventory, and `screens::detail::SHAPE`'s `panel:u8` narrows
        // to `about_panel_open:u8` because which SURFACE is up is `Navigation::write`'s record and
        // a second copy on the page would be two producers of one fact. A schema transition, not a
        // rebaseline: a fixture recorded against 0x1006_0b14_b43f_5f57 must be refused and
        // rerecorded, because a recording taken before this hashed the picker's cursor nowhere at
        // all — its UP/DOWN moved a `static mut TABLE` no `LogicalState` could see.
        // Phase 10, the Detail page's *Track information* sheet: `ARG_SHAPE` gains
        // `TracksPanel{page:i32}` and `TracksPanelScreen` joins the inventory. The same transition
        // for the same reason — a fixture recorded against 0xb4a9_96a9_e8f6_0b37 hashed that
        // sheet's PAGE nowhere, and paging it moves nothing else in the app, so a replay of one
        // graded the sheet appearing and disappearing with a hole between.
        // Phase 10: the PROFILE MENU is an owned surface. `ARG_SHAPE` loses
        // `Account{over:BarHost{…}}` (the route it rode on is deleted) and gains `AccountMenu`,
        // and the menu's own state — its header, its row set and its cursor — joins the inventory
        // for the first time (`screens::account_menu::SHAPE`). Another schema transition rather
        // than a rebaseline: the rows and the selection were `static mut ROWS`/`TABLE`, which no
        // `LogicalState` could see, so a recording taken before this came back `SAME` for a replay
        // that landed on a DIFFERENT row of the menu.
        // Phase 10 again: the ITEM CONTEXT MENU is an owned surface too. `ARG_SHAPE` loses
        // `ItemMenu{over:MenuHost{…}}` and gains `ItemMenu{sid,rk,kind,host,focus,anchor,…}` —
        // which is the six `static mut`s the popover carried, promoted to the entry's argument —
        // and the panel's own rows, actions and cursor join the inventory
        // (`screens::item_menu::SHAPE`). `PlayerScreen` also SHRINKS in the same commit: its
        // `held: HeldKey` had had no producer since phase 9 and went with the loop's client-side
        // repeat timer, so `player::SHAPE` loses `held:{sym,down_sym}`. A schema transition on all
        // three counts, so the recordings are refused and rerecorded rather than rebaselined.
        // **The MERGE of those two lanes is itself a third shape, and neither lane's own pin
        // describes it.** The page-panel lane pinned 0xcbf9_7184_91da_329c over an `ARG_SHAPE`
        // that still carried `Account{over:BarHost{…}}`/`ItemMenu{over:MenuHost{…}}` in its
        // `Route` alphabet; the shared-modal lane pinned 0x732f_34cd_7c07_6197 over one with no
        // `AltSources`/`TracksPanel` in it. The union has all four surfaces, one `Route` alphabet
        // with neither menu in it, and canon tags 6/7 for the two Detail panels against 8/9 for
        // the two menus — so BOTH of those values must be refused here, exactly as the values
        // before them are.
        //
        // **Phase 10, lane A: the pin SPLITS, and this half stops moving when a screen lands.**
        // `state_fp()` is [`APP_SHAPES`] folded in front of `screens::registry::SCREEN_SHAPES`,
        // and the screen half carries its own pin in that module
        // (`the_screen_shape_inventory_is_pinned`) — because the criterion this phase proves is
        // that a new screen touches the registry and not this file. What is asserted here is the
        // LOOP's own list. The values above are the history of the COMBINED one and are kept: every
        // recording ever refused was refused against one of them, and `state_fp()` still reports a
        // combined value (0x6a5c_ca67_6290_b770 at the moment of the split, unchanged by it —
        // the move preserved both the order and the strings).
        assert_eq!(crate::ui::rec::state_fp(APP_SHAPES), 0x79dc_9274_0550_1805);
    }

    /// The gate at the REAL hubs landing site, through the recording the driver loads: a result
    /// the worker produced before its recorded frame is not observed until that frame, and the
    /// verdict says so. Without `ui::landgate` this is the flow-12 defect measured 2026-09-10 —
    /// the recording's one `async` record on frame 1, every replay observing it on frame 0, and
    /// 927 of 928 frames diverging because a spring started a frame early never re-converges.
    #[test]
    fn a_hubs_landing_is_delivered_on_its_recorded_frame_during_replay() {
        let _guard = crate::testlock::serial();
        crate::pms::seed_for_test(1, crate::pms::HubState::Ready);
        let _ = crate::stores::hubs::take_results();
        // a recording in which Hubs landed on FRAME 2 and nowhere else
        let manifest = format!(r#"{{"schema": {}, "state_fp": {}}}"#, crate::ui::rec::SCHEMA, state_fp());
        let seg = b"{\"f\":0,\"t\":\"tick\",\"ms\":0,\"dt_us\":16000}\n                    {\"f\":1,\"t\":\"tick\",\"ms\":16,\"dt_us\":16000}\n                    {\"f\":2,\"t\":\"tick\",\"ms\":32,\"dt_us\":16000}\n                    {\"f\":2,\"t\":\"land\",\"ord\":1,\"gen\":3,\"n\":1}\n";
        let rec = Recording::parse(&manifest, &[seg.as_slice()], state_fp()).unwrap();
        assert_eq!(rec.land_schedule(), vec![vec![], vec![(2, 1)]]);
        let _armed = crate::ui::landgate::Armed;
        crate::ui::landgate::arm_replay(rec.land_schedule());
        // the worker's answer is in the mailbox from frame 0
        crate::pms::queue_test_landing(Some(4));
        let mut seen = Vec::new();
        for f in 0..4u64 {
            crate::ui::landgate::begin_frame(f);
            if !super::super::bridge::take_live_results().is_empty() {
                seen.push(f);
            }
        }
        assert_eq!(seen, vec![2], "the live arrival waited for its recorded frame");
        assert!(crate::ui::landgate::take_diffs().is_empty());
        assert!(crate::ui::landgate::unmatched().is_empty());
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
    }

    /// …and a landing the recording never saw is delivered at once and counted, so the gate can
    /// only ever DELAY an arrival — it can neither invent one nor hide one.
    #[test]
    fn a_hubs_landing_the_recording_never_saw_is_delivered_at_once_and_counted() {
        let _guard = crate::testlock::serial();
        crate::pms::seed_for_test(1, crate::pms::HubState::Ready);
        let _ = crate::stores::hubs::take_results();
        let _armed = crate::ui::landgate::Armed;
        crate::ui::landgate::arm_replay(vec![]);
        crate::pms::queue_test_landing(Some(4));
        crate::ui::landgate::begin_frame(5);
        assert_eq!(super::super::bridge::take_live_results().len(), 1);
        assert_eq!(
            crate::ui::landgate::take_diffs(),
            vec![(5, crate::stores::StoreId::Hubs.ord().0, crate::ui::landgate::Diff::Extra)]
        );
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
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
            0x7252_4cf7_ed8d_97a3, 0x5ba4_34ad_d5db_5bdf, 0xe61b_6d55_f442_8637,
            0x1006_0b14_b43f_5f57, 0x19e7_c63b_e018_e61f] {
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
