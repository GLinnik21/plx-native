//! Controlled recording/replay over the production boot and frame loop (§5.3–5.5).
//!
//! `bootstrap::Initial` captures Session authority/entropy, consent, automation and pre-work
//! Home, Settings and synthetic content inputs. Replay validates the recording before native boot,
//! restores those inputs,
//! binds its primary Client explicitly, and supplies Home/Browse arrivals through Bridge's
//! ordinary dispatcher. Replay request executors do not launch data workers; the bound Client
//! also denies/counts transport attempts. Detail and Person replies are supplied at their
//! original store consumers; admissions retain the request identity and spawn answer.
//! Account and playback remain outside this contract.
//!
//! Supported effects carry complete private payloads, including screen-input time/source/edge
//! and focus identity. Missing/extra/changed effects or results prevent SAME. A codec error
//! invalidates the Writer and replay acceptance; equal unsupported markers are never matches.
//! Recorded keys and supplied keys use the same production controlled ingress, retaining the
//! original event timestamp and source rather than synthesizing a new SDL timestamp.
//!
//! AppFrameV4 combines press/route/overlay/focus/tree with cached Session, physical Consent and typed initial-input
//! digests. Private initial/effect data may contain credentials; shareable probes contain no
//! raw identity. Only explicit synthetic construction is eligible for fixture import.
//!
//! **Historical landing gate (phase 11, §3.3 step 3).** Before controlled initialization, the
//! stores fetched live during replay and the gate constrained when answers were observed.
//! Every landing SITE — Home's hubs through `bridge::take_hubs_results`, and
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
//! committed fixture uses the full synthetic initializer and `tests/mock_pms.py`, then passes
//! `tools/plxnative-rec`'s closed alphabet and the actual replay gate. This is not all-domain or
//! cross-target AppInit acceptance.
#![allow(clippy::too_many_arguments)]

use serde_json::{json, Value};

use crate::ui::machine::{Canon, LogicalState, Tick};
use crate::ui::rec::{DirSink, Header, Recording, Writer};
#[cfg(test)]
use crate::ui::rec::RecError;

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

#[cfg(test)]
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
/// container tree, the cached Session digest, the return state, the recorded PMS fixtures and
/// the app's own still-coarse init.
///
/// Pinned by `the_app_shape_census_is_pinned` below, which does NOT move when a screen lands —
/// the registry's own pin does that, beside the entry that caused it.
const APP_SHAPES: &[&str] = &[
    crate::ui::press::Press::SHAPE,
    crate::ui::input::STATE_SHAPE,
    "TextInputWire{kind:text,text:str,panel:bool,ms:u32,dt_us:u32,source:{Sdl,RemoteFifo,Script,Replay}}",
    "AppFrameV4{route:str,overlay:str,focus:str,tree:u64,session:u64,consent:u64,initial:u64}",
    super::bridge::ConsentMachine::SHAPE,
    crate::ui::containers::STATE_SHAPE,
    crate::ui::screen::RETURN_STATE_SHAPE,
    crate::pms::record::SHAPE,
    crate::pms::initial::SHAPE,
    AppInit::SHAPE,
    super::bootstrap::SHAPE,
    super::bootstrap::ADMISSION_SHAPE,
];

/// Product wire compatibility joins the existing TextInputWire/content codec census. The
/// generic frame envelope is schema 3; product recordings without ordered resolutions and
/// canonical final focus must be refused before parsing/boot, including Targets mode.
const RESOLUTION_SHAPE: &str = "ProductResolutionV1{fo:Option<(entry:u32,elem:u32,group:Option<u32>)>,rs:ordered<Focus{phase:u8,entry:u32,focus:Option<(u32,u32)>,group:Option<u32>,outcome:u8,from:Option<(u32,u32)>,to:Option<(u32,u32)>,by:u8}|Hit{phase:u8,entry:Option<u32>,hit:Option<(u32,u32)>,focus:Option<(u32,u32)>,activate:Option<((u32,u32),u8)>,miss:bool}>,input:post-hit,metrics:Width(bytes,i32,bool)|Cap(i32)|Line(i32)->f32bits}";

/// The state SHAPE of the product hash: bump by changing a `SHAPE` string, never silently.
///
/// Session joined AppFrameV2 as its cached logical digest. The previous five-term product hash
/// could not distinguish a Session change with otherwise identical visible/tree state. This
/// invalidated old recordings. AppFrameV3 additionally bound the complete typed initial-input
/// digest; AppFrameV4 adds the physical Consent owner's current decision. The controlled Home
/// boot consumer restores both before work.
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
    shapes.push(super::bootstrap::CONTENT_SHAPE);
    shapes.push(RESOLUTION_SHAPE);
    shapes.push("MeasurementV1{query:Width(text:bytes,sz:i32,bold:bool)|Cap(sz:i32)|Line(sz:i32),answer:f32bits:u32}");
    shapes.extend_from_slice(crate::screens::registry::SCREEN_SHAPES);
    crate::ui::rec::state_fp(&shapes)
}

/// The hash of the frame's currently covered logical state (spec §5.4).
///
/// `tree` is `Dispatcher::state_hash` — every live instance's `LogicalState`, the tree's shape and
/// surface phases, the engine's focus, queue depth and queued press identities. It is folded in WHOLE rather than
/// sampled, because that function is already the spec's own definition of "the state of the
/// machines" (§5.4) and re-deriving a summary here would be a second definition to keep in step.
/// `session` and `consent` are their owners' cached, side-effect-free subhashes, obtained by
/// the common run tail. Only those u64 values are serialized; this neither adds raw secrets nor
/// claims all-domain state coverage.
pub(crate) fn state_hash(
    press: &crate::ui::press::Press,
    route: &str,
    overlay: &str,
    focus: &str,
    tree: u64,
    session: u64,
    consent: u64,
    initial: u64,
) -> u64 {
    let mut c = Canon::new();
    press.write(&mut c);
    c.str(route).str(overlay).str(focus).u64(tree).u64(session).u64(consent).u64(initial);
    c.finish()
}

pub(crate) struct Rec {
    w: Writer,
    f: u64,
    /// Last post-drain focus observed in this product frame. A product frame can run more than
    /// one dispatcher frame during bootstrap; only the final observation belongs on the tape.
    focus: Option<(u32, u32, Option<u32>)>,
    events: bool,
    spent_ns: u64,
    failure: Option<&'static str>,
}

pub(crate) struct Replay {
    resolution: ResolutionReplay,
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
    effect_at: usize,
    input_at: usize,
    input_diffs: u64,
    effect_diffs: u64,
    started: bool,
    failure: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ReplayMode { #[default] Targets, Resolve }
impl ReplayMode {
    /// One bounded trigger, with an explicit version and no environment-dependent mode.
    /// Bare absolute paths retain the historical Targets spelling.
    pub(crate) fn parse(value: &str) -> Result<(Self, &str), &'static str> {
        let (mode, path) = if let Some(path) = value.strip_prefix("v1\ntargets\n") { (Self::Targets, path) }
            else if let Some(path) = value.strip_prefix("v1\nresolve\n") { (Self::Resolve, path) }
            else { (Self::Targets, value) };
        if value.len() > libc::PATH_MAX as usize || !path.starts_with('/')
            || path.chars().any(char::is_control) || path.trim() != path {
            return Err("invalid replay mode or directory");
        }
        Ok((mode, path))
    }
}

#[derive(Default)]
struct ResolutionReplay {
    mode: ReplayMode,
    at: usize,
    final_seen: bool,
    final_focus: Option<(u32, u32, Option<u32>)>,
    focus_diffs: u64,
    hit_diffs: u64,
}

type WireKey = (u32, u32);
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
#[serde(tag="kind", deny_unknown_fields)]
enum ResolutionWire {
    Focus { phase: u8, entry: u32, focus: Option<WireKey>, group: Option<u32>,
        outcome: u8, from: Option<WireKey>, to: Option<WireKey>, by: u8 },
    Hit { phase: u8, entry: Option<u32>, hit: Option<WireKey>, focus: Option<WireKey>,
        activate: Option<(WireKey, u8)>, miss: bool },
}

fn wire_key(k: crate::ui::machine::FocusKey<u32>) -> WireKey { (k.entry.0, k.elem) }
fn focus_key(k: WireKey) -> crate::ui::machine::FocusKey<u32> {
    crate::ui::machine::FocusKey { entry:crate::ui::machine::EntryId(k.0), elem:k.1 }
}
fn resolution_hash(value:&impl serde::Serialize)->u64 {
    let mut canon=Canon::new();
    canon.str(&serde_json::to_string(value).expect("finite numeric resolution"));
    canon.finish()
}
impl ResolutionWire {
    fn focus(phase: u8, entry: crate::ui::machine::EntryId, answer: crate::ui::dispatch::FocusAnswer<u32>) -> Self {
        use crate::ui::{focus::Outcome, screen::{By, EdgeRule}, machine::NavOpKind};
        let (outcome, from, to, by) = match answer.outcome {
            Outcome::Nothing => (0,None,None,0),
            Outcome::Moved { from, to, by } => (1, from.map(wire_key), Some(wire_key(to)),
                match by { By::Dir=>0, By::Pointer=>1, By::Restore=>2, By::Reconcile=>3 }),
            Outcome::Edge(rule) => (match rule { EdgeRule::Geometric=>2, EdgeRule::Stop=>3,
                EdgeRule::Screen=>4, EdgeRule::Nav(NavOpKind::Back)=>5, EdgeRule::Nav(NavOpKind::Dismiss)=>6 },None,None,0),
        };
        Self::Focus { phase, entry:entry.0, focus:answer.focus.map(wire_key), group:answer.group.map(|g|g.0),
            outcome, from, to, by }
    }
    fn focus_answer(&self) -> Result<crate::ui::dispatch::FocusAnswer<u32>, &'static str> {
        use crate::ui::{focus::Outcome, screen::{By, EdgeRule}, machine::{NavOpKind, GroupId}};
        let Self::Focus { phase, entry, focus, group, outcome, from, to, by } = *self else { return Err("wrong resolution family"); };
        if phase > 2 || entry == 0 || [focus,from,to].into_iter().flatten().any(|k| k.0 != entry)
            || (focus.is_none() && group.is_some()) { return Err("invalid focus identity"); }
        let outcome = match outcome {
            1 => {
                let by = match by { 0 if phase==1 => By::Dir, 2 if phase<=1 => By::Restore,
                    3 if phase==2 => By::Reconcile, _ => return Err("invalid focus cause") };
                if to.is_none() || focus != to || (phase!=0 && from == to) { return Err("invalid focus move"); }
                Outcome::Moved { from:from.map(focus_key), to:focus_key(to.unwrap()), by }
            }
            code if from.is_none() && to.is_none() && by == 0 => match code {
                0 => Outcome::Nothing,
                2..=6 if phase == 1 => Outcome::Edge(match code { 2=>EdgeRule::Geometric,3=>EdgeRule::Stop,
                    4=>EdgeRule::Screen,5=>EdgeRule::Nav(NavOpKind::Back),_=>EdgeRule::Nav(NavOpKind::Dismiss) }),
                _ => return Err("invalid focus outcome"),
            },
            _ => return Err("invalid focus outcome"),
        };
        Ok(crate::ui::dispatch::FocusAnswer { outcome, focus:focus.map(focus_key), group:group.map(GroupId) })
    }
    fn hit(phase: u8, entry: Option<crate::ui::machine::EntryId>, answer: crate::ui::hit::Resolution<u32>) -> Self {
        use crate::ui::screen::Activate;
        Self::Hit { phase, entry:entry.map(|e|e.0), hit:answer.hit.map(wire_key), focus:answer.focus.map(wire_key),
            activate:answer.activate.map(|(k,a)|(wire_key(k),match a { Activate::Press=>0,Activate::Immediate=>1,Activate::Direct=>2 })), miss:answer.miss }
    }
    fn hit_answer(&self) -> Result<crate::ui::hit::Resolution<u32>, &'static str> {
        use crate::ui::screen::Activate;
        let Self::Hit { phase,entry,hit,focus,activate,miss } = *self else { return Err("wrong resolution family"); };
        if phase > 2 || entry == Some(0) || [hit,focus,activate.map(|a|a.0)].into_iter().flatten().any(|k|Some(k.0)!=entry)
            || (focus.is_some() && focus!=hit) || activate.is_some_and(|a|Some(a.0)!=hit || a.1>2 || phase!=1)
            || (miss && (phase!=1 || hit.is_some() || focus.is_some() || activate.is_some()))
            || (phase==2 && (focus.is_some() || activate.is_some())) { return Err("invalid hit resolution"); }
        Ok(crate::ui::hit::Resolution { hit:hit.map(focus_key), focus:focus.map(focus_key), miss,
            activate:activate.map(|(k,a)|(focus_key(k), match a { 0=>Activate::Press,1=>Activate::Immediate,_=>Activate::Direct })) })
    }
    fn decode(value: Value) -> Result<Self, &'static str> {
        let v: Self = serde_json::from_value(value.clone()).map_err(|_|"invalid resolution encoding")?;
        // Option fields must be explicit null, not omitted.
        if serde_json::to_value(&v).ok().as_ref()!=Some(&value) { return Err("noncanonical resolution"); }
        match &v { Self::Focus { .. } => { v.focus_answer()?; }, Self::Hit { .. } => { v.hit_answer()?; } }
        Ok(v)
    }
    fn identity(&self) -> (bool,u8,Option<u32>) {
        match self { Self::Focus {phase,entry,..}=>(true,*phase,Some(*entry)), Self::Hit {phase,entry,..}=>(false,*phase,*entry) }
    }
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

/// Validate every frame before SDL/resource construction. No unsupported family can select
/// a live supplier as a fallback; content resources require the typed content initial domain.
pub(crate) fn validate_controlled(recording: &Recording, initial: &super::bootstrap::Initial)
    -> Result<(), &'static str> {
    if recording.stopped_at.is_some() || recording.frames.is_empty() || recording.header.clock_start_ms != initial.clock_start {
        return Err("invalid controlled clock");
    }
    if recording.header.features != features() || recording.header.triggers != initial.triggers
        || recording.header.blobs {
        return Err("unsupported controlled recording configuration");
    }
    let mut detail = crate::metadata::record::Validator::default();
    for (index, frame) in recording.frames.iter().enumerate() {
        if (index == 0 || !frame.inputs.is_empty() || !frame.effects.is_empty() || !frame.results.is_empty())
            && frame.st.is_none() {
            return Err("missing controlled state grade");
        }
        if index != 0 && frame.present.is_none() { return Err("missing controlled presentation grade"); }
        if frame.f != index as u64 || frame.tick.is_none_or(|tick| tick.dt_us > 50_000) {
            return Err("invalid controlled frame sequence");
        }
        if index == 0 && (frame.tick.is_none_or(|tick| tick.ms != initial.clock_start || tick.dt_us != 0)
            || !frame.inputs.is_empty() || !frame.results.is_empty()) {
            return Err("invalid bootstrap frame");
        }
        for input in &frame.inputs {
            decode_input(input)?;
        }
        // Admissions and terminals share a frame when the local spawn is refused. Arrays retain
        // their own order, so establish every synchronous answer before validating that frame's
        // asynchronous completion batch.
        for effect in &frame.effects {
            if effect["payload"]["content_resource"] == true {
                detail.admission(&effect["payload"])?;
            }
        }
        let mut stores = std::collections::BTreeSet::new();
        let mut counts = std::collections::BTreeMap::<u32,u32>::new();
        for value in &frame.results {
            let envelope: ResultEnvelope = serde_json::from_value(value.clone()).map_err(|_| "invalid result envelope")?;
            let (store, req) = match envelope.payload["kind"].as_str() {
                Some("content") if initial.content.is_some() => {
                    let result = super::bootstrap::stores::validate_result(&envelope.payload)?;
                    if envelope.payload["store"] == "metadata" {
                        detail.completion(&envelope.payload["data"])?;
                    }
                    result
                }
                Some("hubs") => (crate::stores::StoreId::Hubs,
                    crate::pms::record::validate_binding(envelope.payload, initial.primary_client)?),
                Some("discovery") => (crate::stores::StoreId::Browse,
                    crate::browse::record::validate_binding(envelope.payload, initial.primary_client)?),
                _ => return Err("unsupported controlled result family"),
            };
            if envelope.f != frame.f || envelope.t != "async" || envelope.req != req
                || envelope.to != machine_name(crate::ui::machine::MachineId::Store(store.ord())) {
                return Err("mismatched controlled result address");
            }
            stores.insert(store.ord().0);
            if store == crate::stores::StoreId::Person {
                *counts.entry(store.ord().0).or_default() += 1;
            } else { counts.insert(store.ord().0, 1); }
        }
        let landed: std::collections::BTreeSet<_> = frame.lands.iter().map(|(ord, _, _)| *ord).collect();
        if stores != landed || frame.lands.iter().any(|(ord, _, count)| counts.get(ord) != Some(count))
            || landed.len() != frame.lands.len() {
            return Err("incoherent controlled landing schedule");
        }
        for effect in &frame.effects {
            if effect["payload"]["content_resource"] == true {
                if initial.content.is_none() || effect["from"] != "Cache" || effect["e"] != "App" {
                    return Err("invalid content resource origin");
                }
                super::bootstrap::stores::validate_admission(&effect["payload"], initial.primary_client)?;
            }
            if effect["e"] == "Request" {
                if effect["from"] != "Cache" { return Err("invalid admission origin"); }
                super::bootstrap::validate_admission(&effect["payload"], initial.primary_client)?;
            }
            if effect["f"] != frame.f || effect["t"] != "eff"
                || effect.get("payload").is_none() || effect["payload"].get("unsupported").is_some()
                || !matches!(effect["e"].as_str(), Some("Nav" | "Mount" | "Unmount" | "Deliver" |
                    "Timer" | "CancelTimer" | "Press" | "Remember" | "Log" | "App" | "Request")) {
                return Err("unsupported controlled effect");
            }
        }
    }
    Ok(())
}
pub(crate) fn validate_resolution_recording(recording:&Recording) -> Result<(), &'static str> {
    for frame in &recording.frames {
        if frame.focus.is_none() { return Err("missing controlled focus truth"); }
        if frame.focus.flatten().is_some_and(|(entry,_,_)|entry==0) { return Err("invalid focus entry"); }
        for row in &frame.resolutions {
            if row["f"] != frame.f || row["t"] != "rs" { return Err("invalid resolution frame"); }
            ResolutionWire::decode(row["payload"].clone())?;
        }
        let hits:Vec<_>=frame.resolutions.iter().filter_map(|row|match ResolutionWire::decode(row["payload"].clone()).ok()? {
            ResolutionWire::Hit{hit,..}=>Some(hit.map(|k|k.1)), _=>None }).collect();
        let pointer_inputs:Vec<_>=frame.inputs.iter().filter_map(|row|match decode_input(row).ok()?.kind {
            crate::ui::machine::InputKind::Pointer{hit,..}|crate::ui::machine::InputKind::Click{hit,..}
                |crate::ui::machine::InputKind::Drag{hit,..}=>Some(hit), _=>None }).collect();
        // Legacy pointers have no rs rows; current product pages are all Engine pages. Runtime
        // observation accounting still protects generic legacy test doubles.
        if hits!=pointer_inputs { return Err("incoherent pointer truth"); }
    }
    Ok(())
}
impl Replay {
    fn resolution_diff(&mut self, focus: bool, reason: &'static str, index: usize, hashes:Option<(u64,u64)>) {
        if focus { self.resolution.focus_diffs += 1; } else { self.resolution.hit_diffs += 1; }
        // Focus/hit truth is a subset of INPUT-resolution grading. Keep it in input_diffs too:
        // the existing adoption gate refuses input differences, including state-only rebaseline
        // with an unrelated state mismatch. Dedicated counters identify the exact subcategory.
        self.input_diffs += 1;
        let hashes=hashes.map(|(expected,got)|format!(" expected={expected:#018x} got={got:#018x}")).unwrap_or_default();
        crate::log(&format!("replay: input diverge f={} resolution_index={index} reason={reason} resolution={}{hashes}",
            self.rec.frames.get(self.at).map_or(self.at as u64,|f|f.f), if focus { "focus" } else { "hit" }));
    }
    fn same(&self) -> bool {
        self.failure.is_none() && self.diverged == 0 && self.present_diffs == 0
            && self.result_diffs == 0 && self.land_diffs == 0 && self.input_diffs == 0
            && self.effect_diffs == 0
    }
}

pub(crate) enum Recplay {
    Off,
    Recording(Rec),
    Replaying(Replay),
}

/// Observe the real drain with complete supported effect and result payloads.
impl crate::ui::dispatch::Tap<super::bridge::AppHost> for Recplay {
    fn resolution_mode(&self) -> Option<bool> {
        if let Self::Replaying(r)=self { Some(r.resolution.mode==ReplayMode::Resolve) } else { None }
    }
    fn resolution_active(&self) -> bool { !matches!(self,Self::Off) }
    fn resolution_error(&mut self, reason: &'static str) { self.refuse(reason); }
    fn resolve_focus(&mut self, _f:u64, phase:u8, entry:crate::ui::machine::EntryId,
        actual:Option<crate::ui::dispatch::FocusAnswer<u32>>) -> Option<crate::ui::dispatch::FocusAnswer<u32>> {
        if matches!(self,Self::Off) { return actual; }
        self.resolve_observation((true,phase,Some(entry.0)), actual.map(|a|ResolutionWire::focus(phase,entry,a)))
            .and_then(|v|v.focus_answer().ok())
    }
    fn resolve_hit(&mut self, _f:u64, kind:crate::ui::hit::PointerKind, entry:Option<crate::ui::machine::EntryId>,
        actual:Option<crate::ui::hit::Resolution<u32>>) -> Option<crate::ui::hit::Resolution<u32>> {
        if matches!(self,Self::Off) { return actual; }
        use crate::ui::hit::PointerKind;
        let phase = match kind { PointerKind::Move=>0, PointerKind::Click=>1, PointerKind::Drag=>2 };
        self.resolve_observation((false,phase,entry.map(|e|e.0)),actual.map(|a|ResolutionWire::hit(phase,entry,a)))
            .and_then(|v|v.hit_answer().ok())
    }
    fn focus_continuation(&mut self, _f:u64, engine:bool, actual:Option<(u32,u32,Option<u32>)>)
        -> Option<Option<(u32,u32,Option<u32>)>> {
        let Self::Replaying(r)=self else { return None; };
        let expected = r.rec.frames.get(r.at).and_then(|f|f.focus);
        let _ = (engine, actual); // final product-frame truth is checked after the last drain.
        expected
    }
    fn focus(&mut self, _f:u64, focus:Option<(u32,u32,Option<u32>)>) {
        match self {
            Self::Recording(r)=>{
                let start=std::time::Instant::now();
                r.focus=focus;
                r.spent_ns+=start.elapsed().as_nanos() as u64;
            },
            Self::Replaying(r)=>{
                r.resolution.final_seen=true;
                r.resolution.final_focus=focus;
            },
            Self::Off=>{},
        }
    }
    fn input(&mut self, _frame: u64, input: &crate::ui::machine::InputEvent<u32>) {
        if matches!(self,Self::Off) { return; }
        match super::bootstrap::effects::input(input) {
            Ok(encoded) => {
                if let Self::Replaying(replay) = self {
                    {
                        let frame = replay.rec.frames.get(replay.at);
                        let expected = frame.and_then(|frame|frame.inputs.get(replay.input_at));
                        let expected_body = expected
                            .and_then(|value| decode_input(value).ok())
                            .and_then(|event| super::bootstrap::effects::input(&event).ok());
                        if expected_body.as_ref() != Some(&encoded) {
                            replay.input_diffs += 1;
                            crate::log(&format!("replay: input diverge f={} input_index={} reason={}",
                                frame.map_or(replay.at as u64, |frame| frame.f), replay.input_at,
                                if expected.is_some() { "changed" } else { "extra" }));
                        }
                        replay.input_at += 1;
                    }
                } else {
                    self.input(encoded);
                }
            }
            Err(reason) => self.refuse(reason),
        }
    }
    fn result(&mut self, _frame: u64, addr: &crate::ui::machine::Addr, msg: &crate::screens::registry::AppMsg) {
        if let Self::Replaying(replay) = self {
            let payload = match msg {
                crate::screens::registry::AppMsg::HubsResult(result) => Some(crate::pms::record::encode(result)),
                crate::screens::registry::AppMsg::Store(crate::stores::StoreCmd::Browse(
                    crate::stores::browse::BrowseCmd::Discovery(result))) => Some(crate::browse::record::encode(result)),
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
        let payload = match msg {
            crate::screens::registry::AppMsg::HubsResult(result) => crate::pms::record::encode(result),
            crate::screens::registry::AppMsg::Store(crate::stores::StoreCmd::Browse(
                crate::stores::browse::BrowseCmd::Discovery(result))) => crate::browse::record::encode(result),
            _ => { self.refuse("unsupported adapter result"); return; },
        };
        let start = std::time::Instant::now();
        rec.w.result(rec.f, &machine_name(addr.to), addr.req.0, payload);
        rec.events = true;
        rec.spent_ns += start.elapsed().as_nanos() as u64;
    }
    fn effect(&mut self, _frame: u64, stamped: &crate::ui::machine::Stamped<super::bridge::AppHost>) {
        if matches!(self,Self::Off) { return; }
        use crate::ui::machine::{Delivery, Fx, MachineId};
        use crate::ui::screen::ScreenEvent;
        let name = match &stamped.fx {
            Fx::Nav(_) => "Nav", Fx::Mount(_) => "Mount", Fx::Unmount(_) => "Unmount",
            Fx::Deliver(_, _) => "Deliver", Fx::Timer { .. } => "Timer",
            Fx::CancelTimer(_) => "CancelTimer", Fx::Press(_) => "Press",
            Fx::Remember { .. } => "Remember", Fx::Log(_) => "Log", Fx::App(_) => "App",
        };
        let payload = match super::bootstrap::effects::encode(&stamped.fx) {
            Ok(payload) => payload,
            Err(reason) => { self.refuse(reason); return; }
        };
        self.observe_effect(&machine_name(stamped.from), name, payload);
        let Self::Recording(rec) = self else { return };
        let start = std::time::Instant::now();
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

pub(crate) fn machine_name(id: crate::ui::machine::MachineId) -> String {
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
    fn resolve_observation(&mut self, identity:(bool,u8,Option<u32>), actual:Option<ResolutionWire>) -> Option<ResolutionWire> {
        match self {
            Self::Off => actual,
            Self::Recording(r) => {
                let start=std::time::Instant::now();
                if let Some(value)=&actual { r.w.resolution(r.f,serde_json::to_value(value).expect("finite resolution")); }
                r.spent_ns+=start.elapsed().as_nanos() as u64;
                actual
            }
            Self::Replaying(r) => {
                let at=r.resolution.at;
                r.resolution.at+=1;
                let expected=r.rec.frames.get(r.at).and_then(|f|f.resolutions.get(at))
                    .and_then(|v|ResolutionWire::decode(v["payload"].clone()).ok());
                if expected.as_ref().is_none_or(|v|v.identity()!=identity) {
                    r.resolution_diff(identity.0,"order",at,None);
                    r.failure=Some("missing or mismatched resolution observation");
                    return None;
                }
                if r.resolution.mode==ReplayMode::Resolve && actual!=expected {
                    r.resolution_diff(identity.0,"changed",at,Some((resolution_hash(&expected),resolution_hash(&actual))));
                }
                expected
            }
        }
    }
    pub(crate) fn content_begin(&self) {
        let (requests, results) = if let Self::Replaying(r) = self {
            r.rec.frames.get(r.at).map(|f| (
                f.effects.iter().filter(|v| v["payload"]["content_resource"] == true)
                    .map(|v| v["payload"].clone()).collect(),
                f.results.iter().filter(|v| v["payload"]["kind"] == "content")
                    .map(|v| v["payload"].clone()).collect(),
            )).unwrap_or_default()
        } else { Default::default() };
        super::bootstrap::stores::begin(requests, results);
    }
    pub(crate) fn content_results(&mut self) {
        for payload in super::bootstrap::stores::take_results() {
            let (store, req) = match super::bootstrap::stores::validate_result(&payload) {
                Ok(v) => v, Err(e) => { self.refuse(e); return; }
            };
            let to = machine_name(crate::ui::machine::MachineId::Store(store.ord()));
            match self {
                Self::Recording(r) => { r.w.result(r.f, &to, req, payload); r.events = true; }
                Self::Replaying(r) => {
                    let expected = r.rec.frames.get(r.at).and_then(|f| f.results.get(r.result_at));
                    if expected.is_none_or(|v| v["payload"] != payload || v["to"] != to || v["req"] != req) {
                        r.result_diffs += 1;
                    }
                    r.result_at += 1;
                }
                Self::Off => {}
            }
        }
    }
    pub(crate) fn content_end(&mut self) {
        let (requests, failure) = super::bootstrap::stores::finish();
        for request in requests { self.observe_effect("Cache", "App", request); }
        if let Some(reason) = failure { self.refuse(reason); }
    }
    pub(crate) fn abort_startup(&mut self) -> Result<(), &'static str> {
        crate::ui::landgate::disarm();
        match std::mem::replace(self,Self::Off) {
            Self::Recording(record) => record.w.abort().map_err(|_| "recording startup rollback incomplete"),
            Self::Off => Ok(()),
            Self::Replaying(_) => Err("replay does not own recording artifacts"),
        }
    }
    pub(crate) fn failure(&self) -> Option<&'static str> {
        match self { Self::Off => None, Self::Recording(rec) => rec.failure, Self::Replaying(replay) => replay.failure }
    }
    pub(crate) fn outcome_failed(&self) -> bool {
        match self {
            Self::Off => false,
            Self::Recording(rec) => rec.failure.is_some() || rec.w.stopped(),
            Self::Replaying(replay) => !replay.same() || replay.at != replay.rec.frames.len(),
        }
    }
    pub(crate) fn refuse(&mut self, reason: &'static str) {
        match self {
            Self::Off => {}
            Self::Recording(rec) => { rec.failure = Some(reason); rec.w.invalidate(rec.f); }
            Self::Replaying(replay) => replay.failure = Some(reason),
        }
    }
    fn observe_effect(&mut self, from: &str, name: &str, payload: Value) {
        if payload.get("unsupported").is_some() { self.refuse("unsupported effect payload"); return; }
        match self {
            Self::Off => {}
            Self::Recording(rec) => {
                rec.w.effect_payload(rec.f, from, name, payload);
                rec.events = true;
            }
            Self::Replaying(replay) => {
                let frame = replay.rec.frames.get(replay.at);
                let expected = frame.and_then(|frame| frame.effects.get(replay.effect_at));
                if expected.is_none_or(|value| value["from"] != from || value["e"] != name
                    || value["payload"] != payload) {
                    replay.effect_diffs += 1;
                    crate::log(&format!("replay: effect diverge f={} index={}",
                        frame.map_or(replay.at as u64, |frame| frame.f), replay.effect_at));
                    if replay.at < 3 {
                        crate::log(&format!("replay: effect trace kind={name} from={from} event={} tick_ms={} tick_dt={}",
                            payload.get("delivery").and_then(|v| v.get("event")).and_then(Value::as_str).unwrap_or("none"),
                            payload.pointer("/delivery/body/ms").and_then(Value::as_u64).unwrap_or(0),
                            payload.pointer("/delivery/body/dt_us").and_then(Value::as_u64).unwrap_or(0)));
                    }
                }
                replay.effect_at += 1;
            }
        }
    }

    pub(crate) fn resource_requests(&mut self, requests: Vec<Value>) {
        for request in requests { self.observe_effect("Cache", "Request", request); }
    }
    pub(crate) fn prepare_resources(&self, bridge: &mut super::bridge::Bridge) {
        match self {
            Self::Recording(_)=>bridge.prepare_measurements(None),
            Self::Replaying(replay)=>bridge.prepare_measurements(Some(&replay.rec.metrics)),
            Self::Off=>{},
        }
        if let Self::Replaying(replay) = self {
            let admissions = replay.rec.frames.get(replay.at).into_iter()
                .flat_map(|frame| frame.effects.iter()).filter(|effect| effect["e"] == "Request")
                .map(|effect| effect["payload"].clone()).collect();
            bridge.supply_admissions(admissions);
        }
    }
    pub(crate) fn controlled(mode: super::bootstrap::Preflight, initial: &super::bootstrap::Initial)
        -> Result<Self, &'static str> {
        initial.validate()?;
        match mode {
            super::bootstrap::Preflight::Live => Err("controlled recorder requires an explicit mode"),
            super::bootstrap::Preflight::Record => {
                let dir = crate::paths::runtime_dir().join("plxnative-recordings").join("latest");
                let sink = DirSink::create(&dir).map_err(|_| "cannot create private recording")?;
                Self::recording_with_sink(initial, Box::new(sink))
            }
            super::bootstrap::Preflight::Replay { recording, mode, .. } => {
                validate_resolution_recording(&recording)?;
                crate::ui::landgate::arm_sparse_replay(recording.land_schedule());
                Ok(Self::Replaying(Replay { resolution:ResolutionReplay { mode, ..Default::default() }, rec: recording, at: 0, graded: 0, diverged: 0,
                    present_diffs: 0, result_diffs: 0, land_diffs: 0, result_at: 0, effect_at: 0,
                    input_at: 0, input_diffs: 0, effect_diffs: 0, started: false, failure: None }))
            }
        }
    }

    pub(crate) fn measurements(&mut self, bridge: &super::bridge::Bridge) {
        if matches!(self,Self::Off) { return; }
        match bridge.take_measurements() {
            Err(reason)=>self.refuse(reason),
            Ok(rows)=>{
                if let Self::Recording(rec)=self {
                    let start=std::time::Instant::now();
                    for (key,bits) in rows { rec.w.metric(rec.f,&key,bits); }
                    rec.spent_ns+=start.elapsed().as_nanos() as u64;
                }
            },
        }
    }
    pub(crate) fn recording_with_sink(initial: &super::bootstrap::Initial, sink: Box<dyn crate::ui::rec::Sink>)
        -> Result<Self, &'static str> {
        initial.validate()?;
        let mut header = Header::new(state_fp(), initial);
        header.init_data = serde_json::to_value(initial).map_err(|_| "cannot encode initial state")?;
        header.clock_start_ms = initial.clock_start;
        header.build = env!("PLX_VERSION").into();
        header.features = features();
        header.triggers = initial.triggers.clone();
        let w = Writer::open(sink, &header, 0).map_err(|_| "cannot open recording")?;
        crate::ui::landgate::arm_recording();
        Ok(Self::Recording(Rec { w, f:0, focus:None, events:false, spent_ns:0, failure:None }))
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
    /// lookup or best-effort rebinding here. Controlled boot installs the mapping explicitly.
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
            if envelope.payload["kind"] == "content" { continue; }
            let discovery = envelope.payload["kind"] == "discovery";
            let store = if discovery { crate::stores::StoreId::Browse } else { crate::stores::StoreId::Hubs };
            let to = crate::ui::machine::MachineId::Store(store.ord());
            if envelope.f != frame.f || envelope.t != "async" || envelope.to != machine_name(to) {
                return Err("unsupported result envelope");
            }
            let msg = if discovery {
                let result = crate::browse::record::decode(envelope.payload, &mut client)?;
                if result.request_id() != envelope.req { return Err("result request mismatch"); }
                crate::screens::registry::AppMsg::Store(crate::stores::StoreCmd::Browse(
                    crate::stores::browse::BrowseCmd::Discovery(result)))
            } else {
                let result = crate::pms::record::decode(envelope.payload, &mut client)?;
                if result.request_id() != envelope.req { return Err("result request mismatch"); }
                crate::screens::registry::AppMsg::HubsResult(result)
            };
            out.push((crate::ui::machine::Addr { to, req: crate::ui::machine::RequestId(envelope.req) },
                msg));
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
                r.w.focus(r.f,r.focus);
                r.focus=None;
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
                    r.failure = Some("recording storage failure");
                    crate::log(&format!("rec: write failed, stopping: {e:?}"));
                }
                r.events = false;
                r.f += 1;
                r.spent_ns += t0.elapsed().as_nanos() as u64;
                false
            }
            Recplay::Replaying(r) => {
                r.started = true;
                let remaining = r.rec.frames.get(r.at).map_or(0,|f|f.resolutions.len()).saturating_sub(r.resolution.at);
                for index in r.resolution.at..r.resolution.at+remaining {
                    let focus = r.rec.frames[r.at].resolutions[index]["payload"]["kind"]=="Focus";
                    r.resolution_diff(focus,"missing",index,None);
                }
                if let Some(expected) = r.rec.frames.get(r.at).and_then(|f|f.focus) {
                    if !r.resolution.final_seen {
                        r.resolution_diff(true,"missing-final",r.resolution.at,None);
                    } else if r.resolution.final_focus != expected {
                        r.resolution_diff(true,"changed-final",r.resolution.at,Some((
                            resolution_hash(&expected),resolution_hash(&r.resolution.final_focus))));
                        r.failure=Some("final focus continuation was not applied");
                    }
                }
                // Landings the gate could not place on their recorded frame. `late` means the
                // worker was slower here than it was when recorded (holding cannot conjure a
                // result); `extra` means the recording had none left for that store.
                for (frame, ord, why) in crate::ui::landgate::take_diffs() {
                    r.land_diffs += 1;
                    crate::log(&format!("replay: land diverge f={frame} store={ord} reason={}", why.name()));
                }
                if let Some(fr) = r.rec.frames.get(r.at) {
                    let scripts = fr.inputs.len();
                    if r.input_at < scripts {
                        let missing = scripts - r.input_at;
                        r.input_diffs += missing as u64;
                        crate::log(&format!("replay: input diverge f={} input_index={} reason=missing count={missing}",
                            fr.f, r.input_at));
                    }
                    for index in r.result_at..fr.results.len() {
                        r.result_diffs += 1;
                        crate::log(&format!("replay: result diverge f={} index={} reason=missing", fr.f, index));
                    }
                    r.effect_diffs += fr.effects.len().saturating_sub(r.effect_at) as u64;
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
                r.effect_at = 0;
                r.input_at = 0;
                r.resolution.at = 0;
                r.resolution.final_seen = false;
                r.resolution.final_focus = None;
                r.at += 1;
                if r.at >= r.rec.frames.len() {
                    // Recorded landings this run never produced. They can only be known at the
                    // end: until the recording is exhausted, "not yet" and "never" look alike.
                    for (ord, frame, count) in crate::ui::landgate::unmatched_counts() {
                        r.land_diffs += u64::from(count);
                        crate::log(&format!(
                            "replay: land diverge f={frame} store={ord} reason={}",
                            crate::ui::landgate::Diff::Missing.name()
                        ));
                    }
                    crate::log(&format!(
                        "replay: done frames={} graded={} diverged={} present_diffs={} input_diffs={} result_diffs={} land_diffs={} effect_diffs={} focus_diffs={} hit_diffs={} verdict={}",
                        r.rec.frames.len(),
                        r.graded,
                        r.diverged,
                        r.present_diffs,
                        r.input_diffs,
                        r.result_diffs,
                        r.land_diffs,
                        r.effect_diffs,
                        r.resolution.focus_diffs,
                        r.resolution.hit_diffs,
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

    pub(crate) fn finish(self) -> bool {
        let mut failed = self.outcome_failed();
        crate::ui::landgate::disarm();
        if let Recplay::Recording(r) = self {
            if r.w.finish().is_err() {
                failed = true;
                crate::log("rec: final storage flush failed");
            } else if !failed {
                crate::log("rec: finished");
            }
        }
        failed
    }
}

/// The recorded boot's trigger set against this boot's, as one line naming what is missing and
/// what is extra (the recorder's own `plxnative-rec` and the replay's `plxnative-recplay` are
/// the expected difference and are not reported). `None` when the sets agree. Dev flags reach
/// the loop from the filesystem until phase 4 turns them into recorded `Sys` results, so this is
/// phase 2's assertion that a replay boot was armed the way the recording boot was.
#[cfg(test)]
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

pub(crate) fn features() -> Vec<String> {
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
pub(crate) fn decode_input(value:&Value) -> Result<crate::ui::machine::InputEvent<u32>, &'static str> {
    use crate::ui::machine::{InputEvent,InputKind,Source,Tick};
    if value["body"]["kind"].is_null() {
        // Pointer-up already uses the product's synthetic OK-up envelope (sym/wcode zero).
        let tick=Tick { ms:value["ms"].as_u64().and_then(|n|n.try_into().ok()).ok_or("invalid input time")?,
            dt_us:value["dt_us"].as_u64().and_then(|n|n.try_into().ok()).ok_or("invalid input delta")? };
        let release=super::bridge::release_input(tick);
        let mut canonical=value.clone();
        if let Some(v)=canonical.as_object_mut(){v.remove("f");v.remove("t");}
        if super::bootstrap::effects::input(&release)?==canonical { return Ok(release); }
        return super::bootstrap::effects::decode_input(value);
    }
    let num=|v:&Value|v.as_u64().and_then(|n|u32::try_from(n).ok()).ok_or("invalid pointer integer");
    let body=&value["body"];
    let x=f32::from_bits(num(&body["x_bits"])?);
    let y=f32::from_bits(num(&body["y_bits"])?);
    if !x.is_finite() || !y.is_finite() || x.abs()>32768.0 || y.abs()>32768.0 { return Err("invalid pointer coordinates"); }
    let hit=if body["hit"].is_null() { None } else { Some(num(&body["hit"])?) };
    let kind=match body["kind"].as_str() { Some("pointer")=>InputKind::Pointer{x,y,hit},
        Some("click")=>InputKind::Click{x,y,hit},Some("drag")=>InputKind::Drag{x,y,hit},_=>return Err("invalid pointer kind") };
    let source=match value["source"].as_str() { Some("Sdl")=>Source::Sdl,Some("RemoteFifo")=>Source::RemoteFifo,
        _=>return Err("invalid pointer source") };
    let event=InputEvent{at:Tick{ms:num(&value["ms"])?,dt_us:num(&value["dt_us"])?},source,kind};
    let mut canonical=value.clone();
    if let Some(v)=canonical.as_object_mut(){v.remove("f");v.remove("t");}
    if super::bootstrap::effects::input(&event)?!=canonical { return Err("noncanonical pointer input"); }
    Ok(event)
}

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
    fn product_pre_resolution_wire_shape_is_refused_before_boot() {
        let mut before=APP_SHAPES.to_vec();
        before.push(super::super::bootstrap::CONTENT_SHAPE);
        before.extend_from_slice(crate::screens::registry::SCREEN_SHAPES);
        let old=crate::ui::rec::state_fp(&before);
        assert_ne!(old,state_fp());
        let manifest=json!({"schema":crate::ui::rec::SCHEMA,"state_fp":old}).to_string();
        assert_eq!(Recording::parse(&manifest,&[],state_fp()).err(),Some(RecError::StateShape{theirs:old,ours:state_fp()}));
        eprintln!("product wire fingerprint: {old:#018x} -> {:#018x}",state_fp());
    }

    fn replay_for_test(rec:Recording, mode:ReplayMode) -> Recplay {
        Recplay::Replaying(Replay { resolution:ResolutionReplay {mode,..Default::default()}, rec,
            at:0,graded:0,diverged:0,present_diffs:0,result_diffs:0,land_diffs:0,result_at:0,
            effect_at:0,input_at:0,input_diffs:0,effect_diffs:0,started:false,failure:None })
    }

    #[test]
    fn product_mode_encoding_is_bounded_explicit_and_fail_closed() {
        assert_eq!(ReplayMode::parse("/tmp/synthetic"),Ok((ReplayMode::Targets,"/tmp/synthetic")));
        assert_eq!(ReplayMode::parse("v1\ntargets\n/tmp/synthetic"),Ok((ReplayMode::Targets,"/tmp/synthetic")));
        assert_eq!(ReplayMode::parse("v1\nresolve\n/tmp/synthetic"),Ok((ReplayMode::Resolve,"/tmp/synthetic")));
        for bad in ["", "resolve", "v2\nresolve\n/tmp/x", "v1\nresolve\nx", "v1\nother\n/tmp/x",
            "v1\ntargets\n/tmp/x\nresolve", "/tmp/x\0", "/tmp/x\n", " /tmp/x"] {
            assert!(ReplayMode::parse(bad).is_err());
        }
        assert!(ReplayMode::parse(&format!("/{}","x".repeat(libc::PATH_MAX as usize))).is_err());
    }

    #[test]
    fn product_none_focus_and_missing_extra_truth_never_false_same() {
        use crate::ui::dispatch::Tap;
        // end_frame consumes the process-wide landing diffs even in this geometry-only case.
        let _serial = crate::testlock::serial();
        let initial=super::super::bootstrap::Initial::synthetic_home(1,32517,None).unwrap();
        let record=||Recording {header:Header::new(state_fp(),&initial),frames:vec![crate::ui::rec::Frame {
            f:0,focus:Some(None),st:Some(7),..Default::default()}],metrics:Default::default(),stopped_at:None};
        for mode in [ReplayMode::Targets,ReplayMode::Resolve] {
            let mut replay=replay_for_test(record(),mode);
            assert_eq!(Tap::<Product>::focus_continuation(&mut replay,1,true,None),Some(None));
            Tap::<Product>::focus(&mut replay,1,None);
            replay.end_frame(&||7);
            let Recplay::Replaying(r)=replay else {unreachable!()};
            assert!(r.same());
        }
        for calls in [0] {
            let mut replay=replay_for_test(record(),ReplayMode::Resolve);
            for _ in 0..calls { Tap::<Product>::focus_continuation(&mut replay,1,true,None); }
            replay.end_frame(&||7);
            let Recplay::Replaying(r)=replay else {unreachable!()};
            assert!(!r.same());
            assert_eq!(r.resolution.focus_diffs,1);
        }
        let mut replay=replay_for_test(record(),ReplayMode::Resolve);
        for _ in 0..2 {
            assert_eq!(Tap::<Product>::focus_continuation(&mut replay,1,true,None),Some(None));
            Tap::<Product>::focus(&mut replay,1,None);
        }
        replay.end_frame(&||7);
        let Recplay::Replaying(r)=replay else {unreachable!()};
        assert!(r.same(),"several dispatcher drains in one product frame have one final truth");
        let mut missing=record();missing.frames[0].focus=None;
        assert_eq!(validate_resolution_recording(&missing),Err("missing controlled focus truth"));
        let mut impossible=record();impossible.frames[0].focus=Some(Some((0,0,None)));
        assert!(validate_resolution_recording(&impossible).is_err());
        let mut replay=replay_for_test(record(),ReplayMode::Resolve);
        assert_eq!(Tap::<Product>::focus_continuation(&mut replay,1,true,Some((1,2,Some(3)))),Some(None));
        Tap::<Product>::focus(&mut replay,1,Some((1,2,Some(3))));
        replay.end_frame(&||7);
        let Recplay::Replaying(r)=replay else {unreachable!()};
        assert_eq!(r.resolution.focus_diffs,1);
        assert!(!r.same());
    }

    type Product = super::super::bridge::AppHost;
    /// The real product tap with a deliberately changed algorithm answer. Mutation happens at
    /// the resolution boundary, before Dispatcher publishes any effects, not after a frame.
    struct ChangedAnswer<'a> { rec:&'a mut Recplay, focus:bool, hit:bool }
    impl crate::ui::dispatch::Tap<Product> for ChangedAnswer<'_> {
        fn resolution_mode(&self)->Option<bool>{ crate::ui::dispatch::Tap::<Product>::resolution_mode(self.rec) }
        fn resolution_active(&self)->bool{crate::ui::dispatch::Tap::<Product>::resolution_active(self.rec)}
        fn resolution_error(&mut self,e:&'static str){ self.rec.refuse(e); }
        fn resolve_focus(&mut self,f:u64,p:u8,e:crate::ui::machine::EntryId,mut a:Option<crate::ui::dispatch::FocusAnswer<u32>>)
            ->Option<crate::ui::dispatch::FocusAnswer<u32>> {
            if self.focus && p==1 {
                if let Some(a)=&mut a { a.focus=None; a.group=None; a.outcome=crate::ui::focus::Outcome::Nothing; }
            }
            crate::ui::dispatch::Tap::<Product>::resolve_focus(self.rec,f,p,e,a)
        }
        fn resolve_hit(&mut self,f:u64,p:crate::ui::hit::PointerKind,e:Option<crate::ui::machine::EntryId>,mut a:Option<crate::ui::hit::Resolution<u32>>)
            ->Option<crate::ui::hit::Resolution<u32>> {
            if self.hit { if let Some(a)=&mut a { a.hit=None; a.focus=None; a.activate=None; } }
            crate::ui::dispatch::Tap::<Product>::resolve_hit(self.rec,f,p,e,a)
        }
        fn focus_continuation(&mut self,f:u64,e:bool,a:Option<(u32,u32,Option<u32>)>) ->Option<Option<(u32,u32,Option<u32>)>> {
            crate::ui::dispatch::Tap::<Product>::focus_continuation(self.rec,f,e,a)
        }
        fn focus(&mut self,f:u64,a:Option<(u32,u32,Option<u32>)>){crate::ui::dispatch::Tap::<Product>::focus(self.rec,f,a);}
        fn input(&mut self,f:u64,e:&crate::ui::machine::InputEvent<u32>){crate::ui::dispatch::Tap::<Product>::input(self.rec,f,e);}
        fn effect(&mut self,f:u64,e:&crate::ui::machine::Stamped<Product>){crate::ui::dispatch::Tap::<Product>::effect(self.rec,f,e);}
        fn result(&mut self,f:u64,a:&crate::ui::machine::Addr,m:&crate::screens::registry::AppMsg){crate::ui::dispatch::Tap::<Product>::result(self.rec,f,a,m);}
    }

    fn product_settings_run(rec:&mut Recplay, changed_focus:bool, changed_hit:bool) -> Vec<u64> {
        product_settings_run_with_queries(rec,changed_focus,changed_hit,&mut Vec::new())
    }
    fn product_settings_run_with_queries(rec:&mut Recplay, changed_focus:bool, changed_hit:bool,
        queries:&mut Vec<crate::ui::rec::MetricKey>) -> Vec<u64> {
        use crate::ui::{dispatch::Dispatcher,machine::{Tick,Key,InputKind}};
        use super::super::bridge;
        let mut d=Dispatcher::<Product>::new();
        let mut rig=bridge::Bridge::for_test(||0);
        rec.prepare_resources(&mut rig);
        bridge::show_page(&mut d,crate::screens::registry::AppArg::Settings(crate::screens::family::SettingsPage::Root));
        let mut hashes=Vec::new();
        for f in 0..7 {
            let tick=Tick {ms:f*100,dt_us:16000};
            let input=if matches!(rec,Recplay::Replaying(_)) {
                rec.replay_inputs().iter().map(|v|decode_input(v).unwrap()).collect()
            } else if f==4 {
                let stop=d.last_stops().iter().find(|s|s.key.entry==d.focus().unwrap().entry).expect("presented test map");
                let rect=stop.rect.intersect(stop.clip);
                vec![bridge::pointer_input(rect.x+rect.w/2.0,rect.y+rect.h/2.0,tick)]
            } else if f>0 && f<4 {
                bridge::script_key(if f==1 {Key::Down} else {Key::Right},tick)
            } else { Vec::new() };
            let pointer=input.iter().any(|e|matches!(e.kind,InputKind::Pointer{..}));
            rec.tick(tick.ms,0.016);
            // The host has no rasterizer; exercise its three layout capabilities through
            // the same Bridge split that the real dispatcher and renderer receive.
            {
                use crate::ui::dispatch::Rig;
                let split=rig.split();
                split.measure.width(c"Settings",28,true);
                split.measure.cap_h(28);
                split.measure.line_h(28);
            }
            {
                let mut tap=ChangedAnswer {rec,focus:changed_focus,hit:changed_hit};
                d.frame_with(&mut rig,tick,input,Vec::new(),&mut tap,false);
            }
            if rec.failure().is_some(){ break; }
            // Rendering needs GL. Substitute only that resource: a presented stop for the
            // actual product's selected control. Pointer resolution itself is the real HitMap.
            if let Some(key)=d.focus() {
                d.input.hit.fill(vec![crate::ui::screen::Stop { key, rect:crate::ui::Rect::FULL,
                    rest_rect:crate::ui::Rect::FULL,clip:crate::ui::Rect::FULL,
                    hover:crate::ui::screen::Hover::Focus,activate:crate::ui::screen::Activate::Press }]);
                d.input.hit.swap();
            }
            let hash=d.state_hash();
            hashes.push(hash);
            rec.measurements(&rig);
            rec.present(true);
            rec.end_frame(&||hash);
            if pointer { assert!(d.focus().is_some()); }
        }
        queries.extend(rig.measurement_queries());
        hashes
    }

    fn product_settings_recording() -> (Recording,Vec<u64>) {
        let initial=super::super::bootstrap::Initial::synthetic_home(1,32517,Some("root".into())).unwrap();
        let sink=crate::ui::rec::MemSink::default();
        let segments=sink.segments.clone();
        let manifest=Header::new(state_fp(),&initial).to_json().to_string();
        let mut rec=Recplay::recording_with_sink(&initial,Box::new(sink)).unwrap();
        let expected=product_settings_run(&mut rec,false,false);
        assert_eq!(rec.failure(),None);
        rec.finish();
        let recording=Recording::parse(&manifest,&segments.borrow().iter().map(Vec::as_slice).collect::<Vec<_>>(),state_fp()).unwrap();
        (recording,expected)
    }

    fn copy_recording(rec:&Recording) -> Recording {
        Recording {header:rec.header.clone(),frames:rec.frames.clone(),metrics:rec.metrics.clone(),stopped_at:rec.stopped_at}
    }

    #[test]
    fn product_replay_uses_captured_metrics_and_missing_queries_refuse_same() {
        let _serial=crate::testlock::serial();
        let (recording,expected)=product_settings_recording();
        assert!(!recording.metrics.is_empty(),"real product resolution must capture measurements");
        for mode in [ReplayMode::Targets,ReplayMode::Resolve] {
            let mut replay=replay_for_test(copy_recording(&recording),mode);
            assert_eq!(product_settings_run(&mut replay,false,false),expected);
            assert!(!replay.outcome_failed());
            for key in recording.metrics.keys() {
                let mut missing=copy_recording(&recording);
                missing.metrics.remove(key);
                let mut replay=replay_for_test(missing,mode);
                product_settings_run(&mut replay,false,false);
                assert!(replay.outcome_failed(),"a table miss must fail before SAME");
                assert_eq!(replay.failure(),Some("replay measurement table miss"));
            }
        }
        crate::ui::landgate::disarm();
    }

    #[test]
    fn product_record_and_resolve_query_order_is_identical() {
        let _serial=crate::testlock::serial();
        let initial=super::super::bootstrap::Initial::synthetic_home(1,32517,Some("root".into())).unwrap();
        let sink=crate::ui::rec::MemSink::default();
        let segments=sink.segments.clone();
        let manifest=Header::new(state_fp(),&initial).to_json().to_string();
        let mut rec=Recplay::recording_with_sink(&initial,Box::new(sink)).unwrap();
        let mut queries=Vec::new();
        let expected=product_settings_run_with_queries(&mut rec,false,false,&mut queries);
        rec.finish();
        let recording=Recording::parse(&manifest,&segments.borrow().iter().map(Vec::as_slice).collect::<Vec<_>>(),state_fp()).unwrap();
        let mut replay=replay_for_test(recording,ReplayMode::Resolve);
        let mut replay_queries=Vec::new();
        assert_eq!(product_settings_run_with_queries(&mut replay,false,false,&mut replay_queries),expected);
        assert_eq!(replay_queries,queries);
        assert!(!replay.outcome_failed());
        crate::ui::landgate::disarm();
    }

    #[test]
    fn product_controlled_bridge_replay_is_font_free_and_fails_before_attachment() {
        use crate::ui::dispatch::Rig;
        let _serial=crate::testlock::serial();
        let initial=super::super::bootstrap::Initial::synthetic_home(1,32517,None).unwrap();
        let mt=unsafe {crate::task::MainThread::assume()};
        let mut bridge=super::super::bridge::Bridge::controlled_home(||0,&initial,&mt,true);
        bridge.split().measure.width(c"no fonts loaded",28,false);
        assert!(bridge.take_measurements().is_err(),"construction-order violations must latch");
        let mut bridge=super::super::bridge::Bridge::controlled_home(||0,&initial,&mt,true);
        let table=[
            (crate::ui::rec::MetricKey::Width {text:b"no fonts loaded".to_vec(),sz:28,bold:false},0x80000000),
            (crate::ui::rec::MetricKey::Cap {sz:28},0x00000001),
            (crate::ui::rec::MetricKey::Line {sz:28},0x41abcdef),
        ].into_iter().collect();
        bridge.prepare_measurements(Some(&table));
        let m=bridge.split().measure;
        assert_eq!(m.width(c"no fonts loaded",28,false).to_bits(),0x80000000);
        assert_eq!(m.cap_h(28).to_bits(),0x00000001);
        assert_eq!(m.line_h(28).to_bits(),0x41abcdef);
        assert!(bridge.take_measurements().unwrap().is_empty());
    }

    #[test]
    fn product_recorded_continuations_preserve_cross_group_memory_without_avalanche() {
        use crate::ui::{dispatch::Dispatcher,machine::{Tick,Key}};
        use super::super::bridge;
        let _serial=crate::testlock::serial();
        let run=|rec:&mut Recplay,changed:bool| {
            let mut d=Dispatcher::<Product>::new();
            let mut rig=bridge::Bridge::for_test(||0);
            rec.prepare_resources(&mut rig);
            bridge::show_page(&mut d,crate::screens::registry::AppArg::Settings(
                crate::screens::family::SettingsPage::ConsentStage(0)));
            let mut states=Vec::new();
            let mut focuses=Vec::new();
            for (f,key) in [None,Some(Key::Right),Some(Key::Right),Some(Key::Left),
                Some(Key::Right),Some(Key::Left)].into_iter().enumerate() {
                let tick=Tick {ms:f as u32*100,dt_us:16000};
                let input=if matches!(rec,Recplay::Replaying(_)) {
                    rec.replay_inputs().iter().map(|v|decode_input(v).unwrap()).collect()
                } else {key.map_or_else(Vec::new,|key|bridge::script_key(key,tick))};
                rec.tick(tick.ms,0.016);
                let mut tap=ChangedAnswer {rec,focus:changed,hit:false};
                d.frame_with(&mut rig,tick,input,Vec::new(),&mut tap,false);
                assert_eq!(rec.failure(),None);
                focuses.push(d.focus_record());
                let hash=d.state_hash();
                states.push(hash);
                rec.measurements(&rig);
                rec.present(true);
                rec.end_frame(&||hash);
            }
            (states,focuses,rig.measurement_queries())
        };
        let initial=super::super::bootstrap::Initial::synthetic_home(1,32517,Some("root".into())).unwrap();
        let sink=crate::ui::rec::MemSink::default();
        let segments=sink.segments.clone();
        let manifest=Header::new(state_fp(),&initial).to_json().to_string();
        let mut rec=Recplay::recording_with_sink(&initial,Box::new(sink)).unwrap();
        let (states,focuses,queries)=run(&mut rec,false);
        assert_ne!(focuses[1].unwrap().2,focuses[2].unwrap().2,"leave the actual action-band group");
        assert_eq!(focuses[1],focuses[3],"return must remember the trailing answer");
        assert_eq!(focuses[3],focuses[5],"second round trip");
        assert!(!queries.is_empty(),"real consent geometry queries the capability");
        rec.finish();
        let recording=Recording::parse(&manifest,&segments.borrow().iter().map(Vec::as_slice).collect::<Vec<_>>(),state_fp()).unwrap();
        for mode in [ReplayMode::Targets,ReplayMode::Resolve] {
            for changed in [false,true] {
                let mut replay=replay_for_test(copy_recording(&recording),mode);
                let (actual,focus,actual_queries)=run(&mut replay,changed);
                assert_eq!(actual,states,"recorded memory must survive algorithm experiments");
                assert_eq!(focus,focuses);
                if mode==ReplayMode::Resolve { assert_eq!(actual_queries,queries,"same query order, including real cross-group geometry"); }
                let Recplay::Replaying(r)=replay else { unreachable!() };
                assert_eq!(r.effect_diffs,0);
                assert_eq!(r.same(),mode==ReplayMode::Targets || !changed);
            }
        }
        crate::ui::landgate::disarm();
    }

    #[test]
    fn product_targets_and_resolve_grade_pointwise_and_continue_before_effects() {
        let _serial=crate::testlock::serial();
        let (recording,expected)=product_settings_recording();
        validate_resolution_recording(&recording).unwrap();
        assert!(recording.frames.iter().all(|f|f.focus.is_some()));
        let pointer=recording.frames[4].inputs.iter().find(|v|v["body"]["kind"]=="pointer").unwrap();
        assert!(pointer["body"]["hit"].is_u64(),"record AFTER the real hit-map answer");
        let directions=recording.frames.iter().flat_map(|f|&f.resolutions)
            .filter(|v|v["payload"]["kind"]=="Focus" && v["payload"]["phase"]==1).count() as u64;
        assert!(directions>=2,"exercise multiple real product focus answers");
        for (mode,changed_focus,changed_hit) in [(ReplayMode::Targets,false,false),(ReplayMode::Resolve,false,false),
            (ReplayMode::Resolve,true,false),(ReplayMode::Resolve,false,true),(ReplayMode::Targets,true,true)] {
            let mut rec=replay_for_test(copy_recording(&recording),mode);
            assert_eq!(product_settings_run(&mut rec,changed_focus,changed_hit),expected,
                "every state hash must continue from recorded answers within the drain");
            let Recplay::Replaying(r)=rec else {unreachable!()};
            assert_eq!(r.effect_diffs,0,"FocusMoved and downstream screen effects must not avalanche");
            assert_eq!(r.resolution.focus_diffs,if mode==ReplayMode::Resolve && changed_focus {directions}else{0});
            assert_eq!(r.resolution.hit_diffs,u64::from(mode==ReplayMode::Resolve && changed_hit));
            assert_eq!(r.same(),mode==ReplayMode::Targets || (!changed_focus && !changed_hit));
        }
        crate::ui::landgate::disarm();
    }

    #[test]
    fn product_resolution_tape_rejects_missing_extra_order_and_impossible_targets() {
        let _serial=crate::testlock::serial();
        let (recording,_)=product_settings_recording();
        for mode in [ReplayMode::Targets,ReplayMode::Resolve] {
            for case in 0..9 {
                let mut changed=copy_recording(&recording);
                match case {
                    0=>{changed.frames[0].resolutions.remove(0);},
                    1=>{let extra=changed.frames[0].resolutions[0].clone();changed.frames[0].resolutions.push(extra);},
                    2=>{changed.frames[0].resolutions.swap(0,1);},
                    3=>{
                        let p=&mut changed.frames[0].resolutions[0]["payload"];
                        p["focus"][1]=json!(0x7fffffff);p["to"][1]=json!(0x7fffffff);
                    },
                    4=>changed.frames[0].resolutions[0]["payload"]["group"]=json!(0x7fffffff),
                    5=>{let (entry,elem,_)=changed.frames[0].focus.flatten().unwrap();
                        changed.frames[0].focus=Some(Some((entry,elem,Some(0x7fffffff))));},
                    6=>{let (_,elem,group)=changed.frames[0].focus.flatten().unwrap();
                        changed.frames[0].focus=Some(Some((999,elem,group)));},
                    7=>changed.frames[0].resolutions[0]["payload"]["group"]=Value::Null,
                    _=>{
                        // A coherent pair used to evade Targets: final fo silently repaired it.
                        for row in &mut changed.frames[0].resolutions {
                            if row["payload"]["kind"]=="Focus" {
                                row["payload"]["group"]=Value::Null;
                            }
                        }
                    },
                }
                let mut rec=replay_for_test(changed,mode);
                product_settings_run(&mut rec,false,false);
                assert!(rec.outcome_failed(),"case {case} mode {mode:?} must never yield SAME");
            }
        }
        for mutate in [0,1,2,3] {
            let mut changed=copy_recording(&recording);
            match mutate {
                0=>changed.frames[0].resolutions[0]["payload"]["phase"]=json!(9),
                1=>{changed.frames[0].resolutions[0]["payload"].as_object_mut().unwrap().remove("group");},
                2=>changed.frames[0].resolutions[0]["f"]=json!(1),
                _=>changed.frames[4].inputs[0]["body"]["hit"]=Value::Null,
            }
            assert!(validate_resolution_recording(&changed).is_err());
        }
        crate::ui::landgate::disarm();
    }

    #[test]
    fn product_recording_observes_post_drain_focus_including_none() {
        use crate::ui::dispatch::Tap;
        let _serial = crate::testlock::serial();
        let initial = super::super::bootstrap::Initial::synthetic_home(1, 32517, None).unwrap();
        let sink = crate::ui::rec::MemSink::default();
        let segments = sink.segments.clone();
        let mut rec = Recplay::recording_with_sink(&initial, Box::new(sink)).unwrap();
        rec.tick(0, 0.0);
        Tap::focus(&mut rec, 1, None);
        Tap::focus(&mut rec, 2, None);
        rec.end_frame(&|| 7);
        rec.tick(16, 0.016);
        Tap::focus(&mut rec, 2, Some((1, 2, Some(3))));
        rec.end_frame(&|| 8);
        rec.finish();
        let rows: Vec<Value> = segments.borrow().iter().flat_map(|s| s.split(|b| *b == b'\n'))
            .filter(|s| !s.is_empty()).map(|s| serde_json::from_slice(s).unwrap()).collect();
        let focus: Vec<_> = rows.iter().filter(|v| v["t"] == "fo").collect();
        assert_eq!(focus.len(), 2, "the product must write the dispatcher focus truth");
        assert_eq!(focus[0]["entry"], Value::Null);
        assert_eq!(focus[1]["group"], 3);
    }

    #[test]
    fn confirmed_erasure_retires_buffered_writer_before_owner_drain() {
        let _serial = crate::testlock::serial();
        let initial = super::super::bootstrap::Initial::synthetic_home(17,32517,None).unwrap();
        let sink = crate::ui::rec::MemSink::default();
        let segments = sink.segments.clone();
        let writer = Writer::open(Box::new(sink), &Header::new(state_fp(), &initial), 0).unwrap();
        let mut rec = Recplay::Recording(Rec { w:writer, f:0, focus:None, events:false, spent_ns:0, failure:None });
        rec.tick(0, 0.0);
        let mut bridge = super::super::bridge::Bridge::for_test(||0);
        let mut pages = crate::ui::dispatch::Dispatcher::<super::super::bridge::AppHost>::new();
        super::super::run::request_local_erasure(&mut rec, &mut bridge, &mut pages);
        assert!(matches!(rec, Recplay::Off), "confirmed erasure must revoke the writer before queued owner work");
        let bytes = segments.borrow().clone();
        pages.frame_with(&mut bridge, Tick::default(), Vec::new(), Vec::new(), &mut rec, false);
        rec.tick(1, 0.016);
        rec.end_frame(&||0);
        rec.finish();
        assert_eq!(*segments.borrow(), bytes, "later frames/shutdown cannot recreate recording bytes");
        assert_eq!(bridge.auth_read().0.phase, crate::auth::Phase::Deleted);
    }

    #[test]
    fn final_storage_flush_failure_reaches_application_exit() {
        struct Disk(bool);
        impl std::io::Write for Disk {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.0 { Err(std::io::Error::other("injected midwrite failure")) } else { Ok(bytes.len()) }
            }
            fn flush(&mut self) -> std::io::Result<()> { Err(std::io::Error::other("injected final flush failure")) }
        }
        impl crate::ui::rec::Sink for Disk {
            fn manifest(&mut self, _: &str) -> std::io::Result<()> { Ok(()) }
            fn segment(&mut self, _: u32) -> std::io::Result<Box<dyn std::io::Write>> { Ok(Box::new(Disk(self.0))) }
        }
        let _serial = crate::testlock::serial();
        let initial = super::super::bootstrap::Initial::synthetic_home(17, 32517, None).unwrap();
        for midwrite in [false, true] {
            let writer = Writer::open(Box::new(Disk(midwrite)), &Header::new(state_fp(), &initial), 0).unwrap();
            let mut rec = Recplay::Recording(Rec { w: writer, f: 0, focus: None, events: false, spent_ns: 0, failure: None });
            rec.tick(0, 0.0);
            rec.end_frame(&||0);
            assert_eq!(rec.failure().is_some(), midwrite, "midwrite failure reaches the production frame tail");
            assert!(super::super::finish_recording(&mut rec), "storage error must fail application outcome");
            assert!(matches!(rec, Recplay::Off), "failed writer is still retired");
            let mut rec = Recplay::recording_with_sink(&initial, Box::new(Disk(midwrite))).unwrap();
            rec.tick(0, 0.0);
            let mut bridge = super::super::bridge::Bridge::for_test(||0);
            let mut pages = crate::ui::dispatch::Dispatcher::<super::super::bridge::AppHost>::new();
            assert!(super::super::run::request_local_erasure(&mut rec,&mut bridge,&mut pages));
            pages.frame_with(&mut bridge, Tick::default(), Vec::new(), Vec::new(), &mut rec, false);
            assert_eq!(bridge.auth_read().0.phase, crate::auth::Phase::Deleted);
            assert_eq!(bridge.auth_read().0.delete_leftovers, 1, "retirement failure joins the existing erase ACK");
        }
    }

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
    fn replay_rejects_missing_duplicate_changed_and_extra_script_inputs() {
        use crate::ui::dispatch::Tap;
        use crate::ui::machine::{Edge, InputEvent, InputKind, Key, Source};
        let event = |key, ms| InputEvent { at:Tick { ms, dt_us:0 }, source:Source::Script,
            kind:InputKind::Key { key, sym:0, wcode:0, edge:Edge::Down, at_edge:false } };
        let expected_events = vec![event(Key::Down, 10), event(Key::Ok, 20)];
        let expected: Vec<_> = expected_events.iter().map(|event| {
            let mut value = super::super::bootstrap::effects::input(event).unwrap();
            value["f"] = json!(0);
            value["t"] = json!("in");
            value
        }).collect();
        let boot = |inputs: Vec<Value>| {
            let init = AppInit { route:"home",session:false,servers:0,consent_asked:0,
                consent_errors:false,consent_usage:false,seed:0 };
            Recplay::Replaying(Replay { resolution:Default::default(), rec:Recording { header:Header::new(state_fp(), &init),
                frames:vec![crate::ui::rec::Frame { f:0, inputs, st:Some(7), ..Default::default() }],
                metrics:Default::default(), stopped_at:None }, at:0,graded:0,diverged:0,
                present_diffs:0,result_diffs:0,land_diffs:0,result_at:0,effect_at:0,
                input_at:0,input_diffs:0,effect_diffs:0,started:false,failure:None })
        };

        let mut exact = boot(expected.clone());
        for event in &expected_events { Tap::input(&mut exact, 0, event); }
        exact.end_frame(&|| 7);
        let Recplay::Replaying(exact) = exact else { unreachable!() };
        assert!(exact.same());

        let cases = [
            vec![expected_events[0].clone()],
            vec![expected_events[0].clone(), expected_events[0].clone(), expected_events[1].clone()],
            vec![expected_events[0].clone(), event(Key::Left, 20)],
            vec![expected_events[0].clone(), expected_events[1].clone(), event(Key::Back, 30)],
        ];
        for actual in cases {
            let mut replay = boot(expected.clone());
            for event in &actual { Tap::input(&mut replay, 0, event); }
            replay.end_frame(&|| 7);
            let Recplay::Replaying(replay) = replay else { unreachable!() };
            assert!(!replay.same(), "every Script stream mutation must prevent SAME");
        }
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
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
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
                resolution:Default::default(),
                rec: Recording { header: Header::new(state_fp(), &init),
                    frames: vec![crate::ui::rec::Frame { f: 0, results: expected, st: Some(7), ..Default::default() }],
                    metrics: Default::default(), stopped_at: None },
                at: 0, graded: 0, diverged: 0, present_diffs: 0, result_diffs: 0, land_diffs: 0,
                result_at: 0, effect_at: 0, input_at: 0, input_diffs: 0,
                effect_diffs: 0, started: false, failure: None,
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
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
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
        let mut rec = Recplay::Recording(Rec { w: writer, f: 0, focus: None, events: false, spent_ns: 0, failure: None });
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
            resolution:Default::default(),
            rec: Recording { header: Header::new(state_fp(), &init), frames: vec![crate::ui::rec::Frame {
                f: 0, results: results.into_iter().cloned().collect(), ..Default::default()
            }], metrics: Default::default(), stopped_at: None },
            at: 0, graded: 0, diverged: 0, present_diffs: 0, result_diffs: 0, land_diffs: 0,
            result_at: 0, effect_at: 0, input_at: 0, input_diffs: 0,
            effect_diffs: 0, started: false, failure: None,
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
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
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
        // AppFrameV2 adds Session's cached digest. AppFrameV4 adds the physical Consent owner;
        // both predecessor censuses remain pinned separately below, and recordings on either
        // combined shape are explicitly refused.
        assert_eq!(crate::ui::rec::state_fp(APP_SHAPES), 0x7609_c82f_0914_2f33);
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
        assert_eq!(rec.land_schedule(), std::collections::BTreeMap::from([(1,vec![(2,1)])]));
        let _armed = crate::ui::landgate::Armed;
        crate::ui::landgate::arm_sparse_replay(rec.land_schedule());
        // the worker's answer is in the mailbox from frame 0
        crate::pms::queue_test_landing(Some(4));
        let mut seen = Vec::new();
        for f in 0..4u64 {
            crate::ui::landgate::begin_frame(f);
            if !super::super::bridge::take_hubs_results().is_empty() {
                seen.push(f);
            }
        }
        assert_eq!(seen, vec![2], "the live arrival waited for its recorded frame");
        assert!(crate::ui::landgate::take_diffs().is_empty());
        assert!(crate::ui::landgate::unmatched().is_empty());
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
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
        assert_eq!(super::super::bridge::take_hubs_results().len(), 1);
        assert_eq!(
            crate::ui::landgate::take_diffs(),
            vec![(5, crate::stores::StoreId::Hubs.ord().0, crate::ui::landgate::Diff::Extra)]
        );
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
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
            0x1006_0b14_b43f_5f57, 0x19e7_c63b_e018_e61f, 0x489b_bd48_8180_e355] {
            let manifest = format!(r#"{{"schema": {}, "state_fp": {old}}}"#, crate::ui::rec::SCHEMA);
            assert_eq!(crate::ui::rec::Recording::parse(&manifest, &[], state_fp()).err(),
                Some(crate::ui::rec::RecError::StateShape { theirs: old, ours: state_fp() }));
        }
    }

    #[test]
    fn the_state_hash_moves_with_the_focus_line_and_the_press() {
        let mut press = crate::ui::press::Press::new();
        let a = state_hash(&press, "home", "", "focus route=home sel=0", 0, 0, 0, 0);
        let b = state_hash(&press, "home", "", "focus route=home sel=1", 0, 0, 0, 0);
        assert_ne!(a, b);
        press.begin(10);
        let c = state_hash(&press, "home", "", "focus route=home sel=0", 0, 0, 0, 0);
        assert_ne!(a, c);
    }

    /// …and with the TREE, which is the half phase 5b added. Every screen in the Settings family
    /// is an instance on the dispatcher whose state the focus fingerprint cannot see: without
    /// this term a replay that opened Legal instead of Privacy would hash identically to one that
    /// did not, and come back `verdict=SAME`.
    #[test]
    fn the_state_hash_moves_with_the_container_tree() {
        let press = crate::ui::press::Press::new();
        let a = state_hash(&press, "home", " overlay=settings", "focus route=home", 0x11, 0, 0, 0);
        let b = state_hash(&press, "home", " overlay=settings", "focus route=home", 0x12, 0, 0, 0);
        assert_ne!(a, b, "the same page and focus over a different tree is a different state");
    }

    #[test]
    fn the_state_hash_moves_with_the_physical_consent_owner() {
        let press = crate::ui::press::Press::new();
        let denied = state_hash(&press, "home", "", "focus route=home", 0, 0, 0x11, 0);
        let allowed = state_hash(&press, "home", "", "focus route=home", 0, 0, 0x12, 0);
        assert_ne!(
            denied, allowed,
            "a consent decision changed without moving the canonical application state"
        );
    }

    fn record_session_frame(bridge: &super::super::bridge::Bridge) -> Recording {
        struct CaptureManifest {
            sink: crate::ui::rec::MemSink,
            manifest: std::rc::Rc<std::cell::RefCell<String>>,
        }
        impl crate::ui::rec::Sink for CaptureManifest {
            fn segment(&mut self, index: u32) -> std::io::Result<Box<dyn std::io::Write>> {
                crate::ui::rec::Sink::segment(&mut self.sink, index)
            }
            fn manifest(&mut self, text: &str) -> std::io::Result<()> {
                *self.manifest.borrow_mut() = text.to_owned();
                Ok(())
            }
        }
        let init = AppInit { route: "home", session: false, servers: 0, consent_asked: 0,
            consent_errors: false, consent_usage: false, seed: 0 };
        let header = Header::new(state_fp(), &init);
        let manifest = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let sink = crate::ui::rec::MemSink::default();
        let segments = sink.segments.clone();
        let writer = Writer::open(Box::new(CaptureManifest { sink, manifest: manifest.clone() }), &header, 0).unwrap();
        let mut rec = Recplay::Recording(Rec { w: writer, f: 0, focus: None, events: false, spent_ns: 0, failure: None });
        rec.tick(0, 0.016);
        if let Recplay::Recording(r)=&mut rec { r.events=true; } // force a state grade; this unit exercises no input delivery
        let press = crate::ui::press::Press::new();
        assert!(!super::super::run::recorder_end_frame(
            &mut rec, bridge, &press, "home", "", "fixed synthetic focus", 17,
        ));
        let bytes = segments.borrow();
        let slices: Vec<_> = bytes.iter().map(Vec::as_slice).collect();
        let parsed = Recording::parse(&manifest.borrow(), &slices, state_fp()).unwrap();
        parsed
    }

    fn session_command(bridge: &mut super::super::bridge::Bridge, command: crate::auth::SessionCmd) {
        let mut d = crate::ui::dispatch::Dispatcher::<super::super::bridge::AppHost>::new();
        super::super::bridge::execute_session_command(&mut d, command);
        d.frame_with(bridge, Tick::default(), Vec::new(), Vec::new(), &mut crate::ui::dispatch::NoTap, false);
    }

    #[test]
    fn session_discriminates_product_hash_with_all_other_terms_equal() {
        let _guard = crate::testlock::serial();
        let base = super::super::bridge::Bridge::for_test(|| 0);
        let mut other = super::super::bridge::Bridge::for_test(|| 0);
        let initial = base.session_subhash();
        let hash = record_session_frame(&base).frames[0].st.unwrap();
        assert_eq!(initial, other.session_subhash());
        assert_eq!(hash, record_session_frame(&other).frames[0].st.unwrap());
        session_command(&mut other, crate::auth::SessionCmd::DismissPinError);
        assert_eq!(initial, other.session_subhash(), "no-op Session command retains cached hash");
        assert_eq!(hash, record_session_frame(&other).frames[0].st.unwrap());
        session_command(&mut other, crate::auth::SessionCmd::NoteDeleteLeftovers(1));
        assert_ne!(initial, other.session_subhash(), "real owner transition changes logical Session");
        assert_ne!(hash, record_session_frame(&other).frames[0].st.unwrap(),
            "same press/route/overlay/focus/tree cannot hide a changed Session");
    }

    #[test]
    fn session_recorded_hash_is_graded_through_the_same_run_tail() {
        let _guard = crate::testlock::serial();
        let base = super::super::bridge::Bridge::for_test(|| 0);
        let mut other = super::super::bridge::Bridge::for_test(|| 0);
        for changed in [false, true] {
            if changed { session_command(&mut other, crate::auth::SessionCmd::NoteDeleteLeftovers(1)); }
            let mut replay = Recplay::Replaying(Replay {
                resolution:Default::default(),
                rec: record_session_frame(&base), at: 0, graded: 0, diverged: 0,
                present_diffs: 0, result_diffs: 0, land_diffs: 0, result_at: 0, effect_at: 0,
                input_at: 0, input_diffs: 0, effect_diffs: 0, started: false, failure: None,
            });
            let press = crate::ui::press::Press::new();
            assert_eq!(crate::ui::dispatch::Tap::<Product>::focus_continuation(
                &mut replay, 0, true, None), Some(None));
            crate::ui::dispatch::Tap::<Product>::focus(&mut replay, 0, None);
            assert!(super::super::run::recorder_end_frame(
                &mut replay, &other, &press, "home", "", "fixed synthetic focus", 17,
            ));
            let Recplay::Replaying(result) = replay else { unreachable!() };
            assert_eq!(result.graded, 1);
            assert_eq!(result.diverged, u64::from(changed));
            assert_eq!(result.same(), !changed);
        }
    }

    #[test]
    fn consent_and_session_frame_shapes_refuse_their_predecessors() {
        let mut pre_settings = APP_SHAPES.to_vec();
        pre_settings[APP_SHAPES.len() - 2] = super::super::bootstrap::PRE_SETTINGS_SHAPE;
        assert_eq!(crate::ui::rec::state_fp(&pre_settings), 0x9f03_9e4f_2ff6_4d19,
            "retain the pre-typed-Settings census");
        let mut pre_consent = pre_settings;
        pre_consent.remove(4);
        pre_consent[3] =
            "AppFrameV3{route:str,overlay:str,focus:str,tree:u64,session:u64,initial:u64}";
        assert_eq!(crate::ui::rec::state_fp(&pre_consent), 0xc3a2_f751_52f6_b9eb,
            "retain the pre-physical-Consent census");
        assert_eq!(crate::ui::rec::state_fp(&pre_consent[..pre_consent.len()-1]), 0xcadd_9035_05e4_2375,
            "retain the controlled-init predecessor without synchronous admission");
        let mut old_app = pre_consent[..pre_consent.len()-2].to_vec();
        old_app[3] = "AppFrameV2{route:str,overlay:str,focus:str,tree:u64,session:u64}";
        assert_eq!(crate::ui::rec::state_fp(&old_app),0x0881_e546_9753_6ca0,
            "retain the Session-only predecessor census");
        old_app[3] = "AppFrame{route:str,overlay:str,focus:str,tree:u64}";
        assert_eq!(crate::ui::rec::state_fp(&old_app), 0x79dc_9274_0550_1805,
            "retain the predecessor app census pin, not a rewritten recording");
        old_app.extend_from_slice(crate::screens::registry::SCREEN_SHAPES);
        let old = crate::ui::rec::state_fp(&old_app);
        assert_ne!(old, state_fp());
        let manifest = format!(r#"{{"schema": {}, "state_fp": {old}}}"#, crate::ui::rec::SCHEMA);
        assert_eq!(Recording::parse(&manifest, &[], state_fp()).err(),
            Some(RecError::StateShape { theirs: old, ours: state_fp() }));
    }

    #[test]
    fn identical_unsupported_effect_markers_cannot_be_same() {
        let _guard = crate::testlock::serial();
        let bridge = super::super::bridge::Bridge::for_test(|| 0);
        let mut recording = record_session_frame(&bridge);
        let payload = json!({"unsupported":"unsupported Home screen delivery"});
        recording.frames[0].effects.push(json!({"f":0,"t":"eff","from":"Nav",
            "e":"Deliver","payload":payload}));
        let mut replay = Recplay::Replaying(Replay { resolution:Default::default(), rec:recording, at:0, graded:0, diverged:0,
            present_diffs:0,result_diffs:0,land_diffs:0,result_at:0,effect_at:0,
            input_at:0,input_diffs:0,effect_diffs:0,
            started:false,failure:None });
        replay.observe_effect("Nav", "Deliver", payload);
        assert!(replay.failure().is_some(), "equal unsupported markers are a codec failure, never an effect match");
        let Recplay::Replaying(replay) = replay else { unreachable!() };
        assert!(!replay.same());
    }

    #[test]
    fn unsupported_codec_stops_the_actual_writer() {
        use crate::ui::dispatch::Tap;
        use crate::ui::machine::{Fx, MachineId, NavOp, Stamped};
        let init = AppInit { route:"home",session:false,servers:0,consent_asked:0,
            consent_errors:false,consent_usage:false,seed:0 };
        let sink = crate::ui::rec::MemSink::default();
        let segments = sink.segments.clone();
        let writer = Writer::open(Box::new(sink), &Header::new(state_fp(), &init), 0).unwrap();
        let mut rec = Recplay::Recording(Rec { w:writer,f:0,focus:None,events:false,spent_ns:0,failure:None });
        Tap::effect(&mut rec, 0, &Stamped { from:MachineId::Nav,
            fx:Fx::Nav(NavOp::Root(crate::screens::registry::AppArg::Search)) });
        assert!(rec.failure().is_some());
        let Recplay::Recording(rec) = &rec else { unreachable!() };
        assert!(rec.w.stopped());
        let rows: Vec<Value> = segments.borrow().iter().flat_map(|bytes| bytes.split(|b| *b == b'\n'))
            .filter(|line| !line.is_empty()).map(|line| serde_json::from_slice(line).unwrap()).collect();
        assert!(rows.iter().any(|row| row["t"] == "stopped" && row["why"] == "unsupported"));
        assert!(!rows.iter().any(|row| row["t"] == "eff"));
    }
}
