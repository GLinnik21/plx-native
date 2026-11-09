//! Persistent URI executor. The caller owns route identity, source authorization and backend
//! admission. The native session and every bus request stay behind `Lifecycle`.
use crate::acb_bind::{AcbBind, Event as AcbEvent, Failure as AcbFailure, Fx as AcbFx};
use crate::acb_drive::{self, Descriptor, Executed, Verdict};
use crate::picture_gate::{Event as PictureEvent, Fx as PictureFx, PictureGate};
use crate::register_ladder::{self, Outcome, Step};
use crate::session_io;
use crate::uri_lifecycle::{Event as LifeEvent, Fx as LifeFx, Lifecycle, Phase};
use crate::acb_envelope::Ls2VideoInfo;
use crate::uri_protocol::{AbrBps, AcbSource, Ev, Message, Method, UriCall};
use plx_base::task::MainThread;
use plx_platform::tv::plane::{Refusal, Shown};
use plx_platform::tv::sink::{DisplaySink, VP_ACB, VP_EXPORTED};
use plx_platform::tv::uri::{Shape, UriMedia};
use plx_playback_contract::{AttemptId, BackendEvent, BackendKind, Capabilities, CommandId,
    ContentTime, DispatchResult, DriverEvent, DriverSet, EvidenceSource, ExitDisposition,
    Failure, Observation, Observed, Operation, Playback, ReleaseStatus, SourceKey, SourceKind,
    TrackChoice, TrackKind, TrackSelection, UriDriver};
use std::collections::{BTreeMap, VecDeque};
use std::os::raw::c_int;

/// A monotonic clock supplied by the runner. The ownership reducer only receives its value.
pub trait MonotonicClock { fn now_ms(&self) -> u64; }

/// The display path the URI executor is composed with (`Binding` is the contract's value). `Acb`
/// is the webOS 4.x plane: the paced bind sequence of `acb_bind` through the one plane owner,
/// released by `UNLOADED` and its completion fence. The executor refuses to start on it unless the
/// display is in ACB mode and the gate (`acb_bind::gate`) passes, and it forwards only a descriptor
/// the service itself reported. Routed in production only for a viewer who turned on Settings > Playback
/// engine > Experimental: nothing else is offered to it. A `devtriggers` build can compose a dev qualifier and
/// certified capabilities for it (the app's `dev::uri_product`); its attempt then also writes the
/// `uri-trace:` stream (`uri_trace`).
pub use plx_playback_contract::uri::Binding;

/// The display path composition chooses for the executor, a pure function of what the set and the
/// boot report:
///
///   - the ACB plane when the display is in ACB mode and the same gate the executor re-checks at
///     every Start holds (a handle the library confirmed initialized, player type 0 (the URI
///     engine's own boot handle); `connectDass` is a developer arm and is not asked for);
///   - the exported window ([`Binding::Exported`] on [`EXPORTED_CANVAS`]) when the display is in
///     `VP_EXPORTED` mode and `firmware_major` is one the table `strata` lists. Production passes
///     [`EXPORTED_STRATA`], whose doc comment is the evidence rule for an entry; the table is a
///     parameter (never a `cfg(test)` constant) so a test passes its own. A major of 0, the
///     unknown answer, is never admitted, whatever the table lists, and nothing is inferred from a
///     neighbouring major;
///   - otherwise nothing: a webOS 5+ release without an entry plays Standard.
///
/// `plane` and `initialized` are the URI engine's own handle's (player type 0, the plane owner's
/// `facts_of(PlaneHandle::Uri)` and `initialized(PlaneHandle::Uri)`), never Feed's: a normal boot
/// initializes Feed's handle as player type 10 and the URI engine's as 0. A 5+ set has no ACB
/// handle, so the exported window's answer does not read them.
pub fn product_binding(display_mode: i32, plane: plx_platform::tv::plane::PlaneFacts, initialized: bool,
    firmware_major: u32, strata: &[u32]) -> Binding {
    let gate = crate::acb_bind::gate(false, display_mode, plane.handle, plane.player_type, false, initialized);
    if gate.ready {
        Binding::Acb
    } else if display_mode == VP_EXPORTED && firmware_major != 0 && strata.contains(&firmware_major) {
        Binding::Exported { canvas: EXPORTED_CANVAS }
    } else {
        Binding::Unbound
    }
}

/// The firmware majors on which the product backend selects the exported window
/// ([`product_binding`]): the only way production ever composes [`Binding::Exported`]. An entry is
/// a webOS firmware major as `device::info().major` reports it (5 for 5.x, 11 for 11.x). The table
/// is empty, so every webOS 5+ release plays Standard until a major earns an entry (a webOS 4.x set
/// takes the ACB plane, which this table does not touch).
///
/// A major is admitted only with all three parts of the evidence in `docs/uri-mode.md`'s Results
/// table, each recorded there with the tester log's decisive lines. A release is never admitted on
/// the developer probe's evidence alone: a run of the product backend is required.
///
///   - (a) decompile: that major's own firmware image reads `option.windowId` at load
///     (`parseLoadMessage`);
///   - (b) picture: a tester's PRODUCT-path run on a debug build (the boot log shows
///     `evidence=override`) shows a moving picture with sound, placed and proportioned right,
///     supported by the probe's summary tokens `load=ok`, `playing=yes`,
///     `currentTime_advanced=yes` and `window=exported`;
///   - (c) controls: the probe's `drive` tokens (`pause=froze`, `resume=sent` with `resume_from=`
///     at or after the paused position minus 1000 ms, `seek=hit`, and `audio=ok` or an
///     `audio_at_load=`) AND the product run's pause, seek and audio doing what the viewer asked.
///
/// One exception: a major with (a) but no tester run is admitted when BOTH neighbouring majors
/// (`major - 1` and `major + 1`) are already admitted on tester evidence, and its own image shows
/// the same `windowId` path. Its Results row then says "bracketed, no tester run". One admitted
/// neighbour is not a bracket, and a major with no image to read has no (a).
///
/// Each entry carries a comment naming its Results row. `0` is never an entry: it is the unknown
/// answer.
pub const EXPORTED_STRATA: &[u32] = &[];

/// The canvas the exported window is placed over: the destination of EVERY placement, the whole
/// 1920x1080 canvas, as Feed (`feed_pump`) and the developer probe (`uriprobe_drive`) place it. Only
/// the SOURCE rectangle varies: the canvas at the start, then the coded size the service reports in
/// `videoInfo`.
pub const EXPORTED_CANVAS: (c_int, c_int) = (1_920, 1_080);

/// No firmware capability is inferred from the display mode or transport response.
pub struct Config {
    pub transport: &'static dyn UriMedia,
    pub display: &'static dyn DisplaySink,
    /// The process's plane owner: grants the one ACB binding and releases it.
    pub plane: &'static plx_platform::tv::plane::PlaneOwner,
    pub clock: Box<dyn MonotonicClock>,
    pub app_id: String,
    pub binding: Binding,
    pub capabilities: Capabilities,
    pub startup_timeout_ms: u64,
    pub release_allowance_ms: u64,
    pub source_ttl_ms: u64,
}

pub use plx_playback_contract::uri::{NativeTrackIndex, UriSource};
use plx_playback_contract::uri::native_index;

struct StagedSource { attempt: AttemptId, key: SourceKey, source: UriSource, deadline_ms: u64 }

#[derive(Clone, Copy)]
enum ControlKind { Transport, Track(TrackSelection), Seek }
#[derive(Clone, Copy)]
struct PendingControl { command: CommandId, operation: Operation, kind: ControlKind,
    deadline_ms: u64 }

/// The adaptive-streaming rates the production `load` carries: none. The one place they are named,
/// so the `adaptive` member of a derived envelope is judged against the Load that was sent.
const LOAD_BPS: AbrBps = AbrBps { start: None, maximum: None, minimum: None };

/// The active URL and the staged URL are deliberately excluded from `Debug` and all notices.
/// The caller must keep this object until `Release::Released`, or retain it in quarantine.
pub struct UriBackend {
    config: Config,
    lifecycle: Lifecycle,
    staged: Option<StagedSource>,
    active: Option<AttemptId>,
    last_started: Option<AttemptId>,
    url: Option<String>,
    tracks: Vec<NativeTrackIndex>,
    /// The audio track the service last reported selected (`audioInfo.track`, or a
    /// `trackSelected`): a selection of exactly that track changes nothing, so the service never
    /// confirms it and none is sent.
    audio_current: Option<i64>,
    window: bool,
    window_id: Option<String>,
    /// The coded size the window was last re-placed for (`videoInfo`): the same size is not placed
    /// twice. `None` until the service reports one, so the start's canvas placement is not it.
    window_coded: Option<(u32, u32)>,
    start_deadline_ms: Option<u64>,
    loaded: bool,
    play_sent: bool,
    played_once: bool,
    subscribed: bool,
    dispatching: Option<PendingControl>,
    controls: BTreeMap<plx_platform::tv::uri::RequestId, PendingControl>,
    pending: VecDeque<DriverEvent>,
    close_reported: bool,
    quarantine_reported: bool,
    /// The identities this attempt has had refused, one `Outcome` per rung tried
    /// (`register_ladder`): the one input of the decision that picks the next registration.
    ladder: Vec<Outcome>,
    /// The identity to register as once the refused registration's release is confirmed. Set
    /// while that quiet close runs; a Stop, a fault or the release itself clears it.
    reopen: Option<Shape>,
    acb: AcbBind,
    /// The envelope for the loaded media that an ACB bind sends: the one the service sent in the ACB
    /// schema, else the one derived from its `videoInfo`. Never anything else.
    acb_source: Option<AcbSource>,
    foreground_sent: bool,
    /// Why the plane owner refused the bind, when it did; the failure reported is typed from it.
    acb_refusal: Option<Refusal>,
    /// The transport state the service last accepted, and whether the plane was told it is paused.
    transport_paused: bool,
    plane_paused: bool,
    /// When `Observation::Presented` may be published (see `picture_gate`): the service's
    /// `playing`, the plane owner's `Shown`, then a clock advance. Only an ACB attempt is ever
    /// told the plane's verdict, so any other binding never presents.
    picture: PictureGate,
    /// The attempt's `uri-trace:` stream (devtriggers only; see `uri_trace`).
    #[cfg(feature = "devtriggers")]
    trace: crate::uri_trace::Trace,
}

impl UriBackend {
    pub fn new(config: Config) -> Self {
        assert!(!config.capabilities.packet_feed, "URI transport cannot accept packet feeds");
        assert!(!config.capabilities.audio_off && !config.capabilities.subtitle_off,
            "URI transport has no confirmed track-disable operation");
        Self { config, lifecycle: Lifecycle::new(), staged: None, active: None,
            last_started: None, url: None, tracks: Vec::new(), audio_current: None,
            window: false, window_id: None, window_coded: None,
            start_deadline_ms: None, loaded: false, play_sent: false, played_once: false,
            subscribed: false,
            dispatching: None, controls: BTreeMap::new(),
            pending: VecDeque::new(), close_reported: false, quarantine_reported: false,
            ladder: Vec::new(), reopen: None,
            acb: AcbBind::new(false), acb_source: None, foreground_sent: false, acb_refusal: None,
            transport_paused: false, plane_paused: false, picture: PictureGate::new(),
            #[cfg(feature = "devtriggers")]
            trace: crate::uri_trace::Trace::new(0) }
    }

    /// Give exactly one authorized, owned URL to the next matching Start. Superseding a staged
    /// source destroys its old locator; a failed, cancelled or expired Start cannot reuse it.
    pub fn stage_source(&mut self, attempt: AttemptId, key: SourceKey, source: UriSource) -> Result<(), Failure> {
        if !source.has_url() || self.last_started.is_some_and(|last| attempt <= last) {
            return Err(Failure::Stale);
        }
        if !source.valid_track_map() {
            return Err(Failure::Unsupported);
        }
        let deadline_ms = self.config.clock.now_ms().saturating_add(self.config.source_ttl_ms);
        self.staged = Some(StagedSource { attempt, key, source, deadline_ms });
        Ok(())
    }

    /// Cancel an unconsumed locator without changing a live native attempt.
    pub fn cancel_staged(&mut self, attempt: AttemptId) {
        if self.staged.as_ref().is_some_and(|source| source.attempt == attempt) {
            self.staged = None;
        }
    }

    /// Native ownership remains live through close and quarantine. Only the real release effect
    /// makes application exit clean; a deadline never does.
    pub fn exit_disposition(&self) -> ExitDisposition {
        if self.active.is_some() || !self.lifecycle.released() {
            ExitDisposition::PreserveNativeOwnership
        } else {
            ExitDisposition::Clean
        }
    }

    /// The plane remains owned after service Playing until native release: the exported window
    /// until it is destroyed, an ACB binding from the plane owner's grant until the `UNLOADED`
    /// fence. This is an ownership fact, not proof that a picture reached the panel.
    pub fn plane_owned(&self) -> bool {
        (self.window && self.played_once)
            || (self.config.binding == Binding::Acb && self.active.is_some() && self.lifecycle.plane_open())
    }

    /// Whether the active source has an exact native index for this selected source track.
    /// This is source evidence, independent of a general SelectTrack capability flag; a PMS
    /// single-track output cannot switch to a different source track in place.
    pub fn maps_track(&self, selection: TrackSelection) -> bool {
        let enabled = match selection.kind {
            TrackKind::Audio => self.config.capabilities.audio_track,
            TrackKind::Subtitle => self.config.capabilities.subtitle_track,
        };
        self.active.is_some() && enabled && native_index(&self.tracks, selection).is_some()
    }

    fn observe(&mut self, observation: Observation) {
        if let Some(attempt) = self.active {
            self.pending.push_back(DriverEvent::Observation { attempt, observation });
        }
    }

    fn release_notice(&mut self, status: ReleaseStatus) {
        if let Some(attempt) = self.active {
            self.pending.push_back(DriverEvent::Release { attempt, status });
        }
    }

    fn refuse_before_open(&mut self, attempt: AttemptId, reason: Failure) -> DispatchResult {
        // The common reducer requires a resource-owner verdict even if admission failed before
        // registration. Do not claim release while another attempt or native slot is occupied.
        if self.active.is_none() && self.lifecycle.released() {
            self.pending.push_back(DriverEvent::Release { attempt, status: ReleaseStatus::Released });
        }
        DispatchResult::Refused(reason)
    }

    fn start(&mut self, mt: &MainThread, attempt: AttemptId, source: SourceKey,
        requirements: plx_playback_contract::Requirements) -> DispatchResult {
        // A Start consumes its matching authorization even if capability or binding admission
        // fails. A wrong attempt cannot consume another attempt's staged URL.
        let staged = if self.staged.as_ref().is_some_and(|staged| staged.attempt == attempt) {
            self.staged.take()
        } else { None };
        if self.active.is_some() || !self.lifecycle.released() { return self.refuse_before_open(attempt, Failure::Busy); }
        if self.last_started.is_some_and(|last| attempt <= last) { return self.refuse_before_open(attempt, Failure::Stale); }
        if requirements.source == SourceKind::PacketFeed { return self.refuse_before_open(attempt, Failure::Unsupported); }
        if !self.config.capabilities.supports(requirements) { return self.refuse_before_open(attempt, Failure::Unsupported); }
        let Some(staged) = staged else { return self.refuse_before_open(attempt, Failure::Refused); };
        if staged.attempt != attempt || staged.key != source || self.config.clock.now_ms() >= staged.deadline_ms {
            return self.refuse_before_open(attempt, Failure::Stale);
        }
        if !staged.source.can_supply(requirements) {
            return self.refuse_before_open(attempt, Failure::Unsupported);
        }
        if matches!(self.config.binding, Binding::Exported { .. }) && self.config.display.window_mode() != VP_EXPORTED {
            return self.refuse_before_open(attempt, Failure::Unsupported);
        }
        if self.config.binding == Binding::Acb
            && (self.config.display.window_mode() != VP_ACB || !self.acb_gate(mt)) {
            return self.refuse_before_open(attempt, Failure::Unsupported);
        }
        let window_id = match self.config.binding {
            Binding::Exported { canvas } => {
                let Some(id) = session_io::create_exported_window(self.config.display, mt) else {
                    return self.refuse_before_open(attempt, Failure::Backend);
                };
                self.window = true;
                self.window_coded = None;
                self.place_window(mt, canvas, canvas);
                Some(id)
            }
            // 4.x ignores the window option; the service wants the key present (measured sequence).
            Binding::Acb => Some(crate::acb_bind::DUMMY_WINDOW_ID.to_owned()),
            Binding::Unbound => None,
        };
        self.last_started = Some(attempt);
        self.active = Some(attempt);
        let (url, tracks) = staged.source.into_parts();
        self.url = Some(url);
        self.tracks = tracks;
        self.audio_current = None;
        self.window_id = window_id;
        self.start_deadline_ms = Some(self.config.clock.now_ms().saturating_add(self.config.startup_timeout_ms));
        self.loaded = false;
        self.play_sent = false;
        self.played_once = false;
        self.subscribed = false;
        self.dispatching = None;
        self.controls.clear();
        self.close_reported = false;
        self.quarantine_reported = false;
        self.ladder.clear();
        self.reopen = None;
        self.acb_source = None;
        self.foreground_sent = false;
        self.acb_refusal = None;
        self.transport_paused = false;
        self.plane_paused = false;
        self.picture_step(PictureEvent::Reset);
        #[cfg(feature = "devtriggers")]
        { self.trace = crate::uri_trace::Trace::new(self.config.clock.now_ms()); }
        self.lifecycle = Lifecycle::new();
        self.feed(LifeEvent::Open(register_ladder::first()), mt, self.config.clock.now_ms());
        DispatchResult::Queued
    }

    fn begin_close(&mut self, mt: &MainThread, now_ms: u64) {
        // A close the player or a failure asks for outranks the ladder's pending retry.
        self.reopen = None;
        // No ACB verb may run after UNLOADED has been asked for: a stray display step would send
        // PLAYING over a release in flight.
        self.acb.step(AcbEvent::Cancel, now_ms, &mut Vec::new());
        self.acb_source = None;
        self.url = None;
        self.start_deadline_ms = None;
        self.dispatching = None;
        self.controls.clear();
        if self.active.is_none() || matches!(self.lifecycle.phase(), Phase::Closing | Phase::Quarantined) {
            return;
        }
        if let Some(media_id) = self.lifecycle.media_id().cloned() {
            self.feed(LifeEvent::Call(UriCall::Unload { media_id }), mt, now_ms);
        }
        self.feed(LifeEvent::Close { now_ms, allowance_ms: self.config.release_allowance_ms }, mt, now_ms);
    }

    /// Whether a denied `method` is the refusal of the registered identity itself: the registration,
    /// or the Load that is its first call (before any media exists). Anything later is the media's.
    fn identity_refused(&self, method: Method) -> bool {
        matches!(method, Method::Register | Method::Load) && self.lifecycle.media_id().is_none()
    }

    /// The identity just tried was refused: when the ladder has a rung left, release its
    /// registration quietly (no notice the player sees, the locator kept) and register as the next
    /// once that release is confirmed (`LifeFx::Released`). False when every rung is spent, and
    /// the caller then takes the one existing failure path.
    fn climb_ladder(&mut self, mt: &MainThread, now_ms: u64) -> bool {
        self.ladder.push(Outcome::Refused);
        let Step::Register(next) = register_ladder::decide(&self.ladder) else { return false };
        let tried = register_ladder::RUNGS.get(self.ladder.len() - 1).copied().map_or("?", register_ladder::label);
        plx_base::eventlog::log(&format!("uri: register shape={tried} refused; retrying as {}",
            register_ladder::label(next)));
        self.reopen = Some(next);
        self.feed(LifeEvent::Close { now_ms, allowance_ms: self.config.release_allowance_ms }, mt, now_ms);
        true
    }

    /// Place the exported window with `source` as the SOURCE rectangle over the whole `canvas` (the
    /// destination is always the canvas, as Feed and the probe place it), and log it: the source
    /// and the seam's answer, never a window id.
    fn place_window(&self, mt: &MainThread, source: (c_int, c_int), canvas: (c_int, c_int)) {
        let rv = session_io::place_exported_window(self.config.display, mt, source, canvas);
        plx_base::eventlog::log(&format!("uri: exported window placed src={}x{} rv={rv}", source.0, source.1));
    }

    /// The service reported the coded size of the picture (`videoInfo` with a width and a height):
    /// re-place the window with it as the source rectangle, as the probe does, so a source that is
    /// not 1920x1080 is not stretched from the canvas. Only a live exported window is placed, and the
    /// same size is not placed twice. A closing or quarantined attempt is never delivered a reply, so
    /// no placement follows a release request, and none follows the destroy.
    fn replace_window(&mut self, dims: Option<(u32, u32)>, mt: &MainThread) {
        let (Binding::Exported { canvas }, true, Some(coded)) = (self.config.binding, self.window, dims)
            else { return };
        if self.window_coded == Some(coded) { return; }
        let (Ok(width), Ok(height)) = (c_int::try_from(coded.0), c_int::try_from(coded.1)) else { return };
        self.window_coded = Some(coded);
        self.place_window(mt, (width, height), canvas);
    }

    /// A Seek has been dispatched and has not landed (or timed out).
    fn seek_pending(&self) -> bool {
        self.controls.values().chain(self.dispatching.iter())
            .any(|pending| matches!(pending.kind, ControlKind::Seek))
    }

    /// Step the picture rule and publish what it decides. The line says which rule fired, so a
    /// device log shows whether the three conditions were met (the lines before it are
    /// `acb display fullscreen=1 rv=1 task=N`, `acb playing rv=1 task=M`, the two
    /// `acb callback task=.. ev=1` lines and `uri-acb: display/PLAYING completed`).
    fn picture_step(&mut self, event: PictureEvent) {
        let mut fx = Vec::new();
        self.picture.step(event, &mut fx);
        for effect in fx {
            match effect {
                PictureFx::Presented { from_ms, clock_ms } => {
                    plx_base::eventlog::log(&format!(
                        "uri: presented (service playing; plane window and PLAYING completed; clock advanced from {from_ms} to {clock_ms} ms)"));
                    self.observe(Observation::Presented);
                }
            }
        }
    }

    /// Ask the plane owner whether the firmware has completed the bind's window and PLAYING calls,
    /// until it has answered. Only an ACB attempt whose sequence has dispatched them has anything
    /// to ask about; every other binding never feeds the picture rule a plane verdict.
    fn watch_plane_show(&mut self, mt: &MainThread) {
        if self.config.binding != Binding::Acb || !self.acb.bound() || !self.picture.wants_plane() {
            return;
        }
        match self.config.plane.shown(self.config.display, mt) {
            Shown::Done => {
                plx_base::eventlog::log("uri-acb: display/PLAYING completed (event 1 on both tasks)");
                self.picture_step(PictureEvent::PlaneShown);
            }
            Shown::Failed { ev } => {
                plx_base::eventlog::log(&format!(
                    "uri-acb: display/PLAYING completion failed (event {ev}); no picture will be claimed"));
                self.picture_step(PictureEvent::PlaneFailed);
            }
            Shown::No | Shown::Waiting { .. } => {}
        }
    }

    fn maybe_subscribe(&mut self, mt: &MainThread, now_ms: u64) {
        if self.lifecycle.phase() != Phase::Open { return; }
        let Some(media_id) = self.lifecycle.media_id().cloned() else { return; };
        if !self.subscribed {
            self.subscribed = true;
            // The measured ACB sequence announces the media as the foreground connection before
            // it subscribes; a service that refuses it ends the start (see `delivered`).
            if self.config.binding == Binding::Acb && !self.foreground_sent {
                self.foreground_sent = true;
                self.feed(LifeEvent::Call(UriCall::NotifyForeground { media_id: media_id.clone() }), mt, now_ms);
            }
            self.feed(LifeEvent::Call(UriCall::Subscribe { media_id }), mt, now_ms);
        }
    }

    /// A delivered method reply is useful only when its native request still names this exact
    /// command. Callback play state and clock reports remain separate observations.
    fn control_message(&mut self, method: Method, denied: bool, message: &Message,
        mt: &MainThread) -> bool {
        if let Some(request) = message.reply_request {
            if let Some(pending) = self.controls.get(&request).copied() {
                let matches_method = matches!((method, pending.kind),
                    (Method::Play, ControlKind::Transport) if pending.operation == Operation::Play)
                    || matches!((method, pending.kind),
                        (Method::Pause, ControlKind::Transport) if pending.operation == Operation::Pause)
                    || matches!((method, pending.kind),
                        (Method::Seek, ControlKind::Seek)
                            | (Method::SelectTrack, ControlKind::Track(_)));
                if matches_method && denied {
                    self.controls.remove(&request);
                    self.retire_control(request);
                    if let Some(attempt) = self.active {
                        self.pending.push_back(DriverEvent::ControlRefused { attempt,
                            command: pending.command, operation: pending.operation,
                            failure: Failure::Refused });
                    }
                    return true;
                }
                if matches_method && matches!(pending.kind, ControlKind::Transport)
                    && message.accepted() == Some(true) {
                    self.controls.remove(&request);
                    self.retire_control(request);
                    if let Some(attempt) = self.active {
                        self.pending.push_back(DriverEvent::ControlAccepted { attempt,
                            command: pending.command, operation: pending.operation,
                            native_acknowledged: true });
                    }
                    self.mirror_transport(pending.operation, mt);
                }
            }
        }
        if let Some((request, acknowledged)) = message.landed_seek {
            self.land_seek(request, acknowledged, message.current_time);
        }
        if message.confirmed_track.is_some() {
            if let Some((kind, index)) = message.selected_track {
                let kind = match kind { "audio" => Some(TrackKind::Audio),
                    "text" => Some(TrackKind::Subtitle), _ => None };
                if let Some(kind) = kind {
                    let matches: Vec<_> = self.controls.iter().filter_map(|(request, pending)| {
                        let ControlKind::Track(selected) = pending.kind else { return None };
                        (selected.kind == kind && native_index(&self.tracks, selected) == Some(index))
                            .then_some(*request)
                    }).collect();
                    if let [request] = matches.as_slice() {
                        let pending = self.controls.remove(request).expect("confirmed track is pending");
                        self.retire_control(*request);
                        let ControlKind::Track(selected) = pending.kind else { unreachable!() };
                        self.observe(Observation::TrackSelected(Observed {
                            value: selected, from: EvidenceSource::BackendReport,
                        }));
                        if let Some(attempt) = self.active {
                            self.pending.push_back(DriverEvent::TrackSettled { attempt,
                                command: pending.command, kind: selected.kind,
                                choice: TrackChoice::Selected(selected.id) });
                        }
                    }
                }
            }
        }
        false
    }

    /// The service reported the pending Seek `request` done (the lifecycle attributed its
    /// `seekDone`): the startup plan's wait is over. It lands on the event alone, in whatever order
    /// the call's own reply came (first, later or, for all this knows, never), and one that already
    /// refused is gone from the ledger, so a `seekDone` after a refusal lands nothing.
    fn land_seek(&mut self, request: plx_platform::tv::uri::RequestId, acknowledged: bool,
        current_time: Option<i64>) {
        if !self.controls.get(&request).is_some_and(|pending| matches!(pending.kind, ControlKind::Seek)) {
            return;
        }
        let pending = self.controls.remove(&request).expect("landed seek is pending");
        self.retire_control(request);
        plx_base::eventlog::log(&format!(
            "uri: control {:?} landed on the service's seekDone (its reply acknowledged first: {})",
            pending.operation, if acknowledged { "yes" } else { "no" }));
        if let Some(attempt) = self.active {
            let position = current_time.and_then(|ms| u64::try_from(ms).ok())
                .map(|ms| Observed { value: ContentTime::from_millis(ms),
                    from: EvidenceSource::BackendReport });
            self.pending.push_back(DriverEvent::SeekLanded {
                attempt, command: pending.command, position,
            });
            self.picture_step(PictureEvent::Seeked);
        }
    }

    fn retire_control(&mut self, request: plx_platform::tv::uri::RequestId) {
        // A confirmed request cannot correlate a later selection of the same native index.
        // Keep the lifecycle's request set aligned with the interactive command ledger.
        self.lifecycle.step(LifeEvent::Retire { request }, &mut Vec::new());
    }

    fn delivered(&mut self, method: Method, ok: bool, message: Message,
        mt: &MainThread, now_ms: u64) {
        #[cfg(feature = "devtriggers")]
        for line in self.trace.lines(&message, now_ms) { plx_base::eventlog::log(&line); }
        let denied = !ok || message.accepted() == Some(false);
        let refused_control = self.control_message(method, denied, &message, mt);
        if denied {
            if matches!(method, Method::Play | Method::Pause | Method::Seek | Method::SelectTrack) {
                if refused_control && method == Method::Play { self.play_sent = false; }
                return;
            }
            if self.identity_refused(method) && self.climb_ladder(mt, now_ms) { return; }
            self.observe(Observation::Failed(Failure::Refused));
            if matches!(method, Method::Register | Method::Load | Method::Subscribe)
                || (method == Method::NotifyForeground && self.config.binding == Binding::Acb) {
                self.begin_close(mt, now_ms);
            }
            return;
        }
        if method == Method::Register {
            self.feed(LifeEvent::Call(UriCall::Load { window_id: self.window_id.clone(), bps: LOAD_BPS }), mt, now_ms);
            return;
        }
        if method == Method::Load {
            // The URL is needed only to build the one Load payload. Never retain it for controls.
            self.url = None;
        }
        // The envelope can arrive in the same message as `loadCompleted`: take it first, so the
        // sequence that `loadCompleted` starts already has the facts it waits for.
        if self.config.binding == Binding::Acb {
            if let Some(source) = message.acb_source.as_ref() {
                self.take_source(source.clone(), mt, now_ms);
            } else if let Some(info) = message.ls2_video.as_ref() {
                self.derive_source(info, mt, now_ms);
            }
        }
        // What the service says it is playing, taken before a select dispatched later reads it.
        if let Some(track) = message.audio_track {
            self.audio_current = Some(track);
        } else if let Some(("audio", track)) = message.selected_track {
            self.audio_current = Some(track);
        }
        let mut ended = false;
        for event in &message.normalized {
            let playback = match event {
                Ev::LoadCompleted => {
                    self.loaded = true;
                    self.start_deadline_ms = None;
                    if self.config.binding == Binding::Acb {
                        if let Some(media_id) = self.lifecycle.media_id().cloned() {
                            self.acb_step(AcbEvent::Loaded { media_id }, mt, now_ms);
                        }
                    }
                    Some(Playback::ReadyAwaitPlay)
                }
                Ev::Playing => {
                    if !self.play_sent || self.controls.values().any(|pending| matches!(pending.kind, ControlKind::Track(_))) {
                        self.observe(Observation::Failed(Failure::Backend));
                        ended = true;
                        None
                    } else {
                        self.played_once = true;
                        plx_base::eventlog::log("uri: service playing");
                        self.picture_step(PictureEvent::ServicePlaying);
                        Some(Playback::Playing)
                    }
                }
                Ev::Paused => Some(Playback::Paused),
                Ev::EndOfStream => { ended = true; Some(Playback::Ended) },
                Ev::Error => {
                    self.observe(Observation::Failed(Failure::Backend));
                    ended = true;
                    None
                }
                Ev::CurrentTime => {
                    if let Some(ms) = message.current_time.and_then(|ms| u64::try_from(ms).ok()) {
                        self.observe(Observation::Position(Observed {
                            value: ContentTime::from_millis(ms), from: EvidenceSource::BackendReport,
                        }));
                        // A report that arrives while a Seek is pending (a `currentTime` can
                        // come before the `seekDone` that lands it) may be the seek target: a
                        // position jump, not movement of the stream that was playing.
                        if self.seek_pending() { self.picture_step(PictureEvent::Seeked); }
                        self.picture_step(PictureEvent::Clock(ms));
                    }
                    None
                }
                // Only a running attempt can rebuffer: buffering before the first Playing is
                // startup and stays unreported rather than being called a rebuffer. Buffering
                // that starts while a Seek is pending is seek priming and is not reported
                // either; an end is always reported so a reported start cannot stay open. The
                // order of `bufferingStart` against `seekDone` is not measured on the device.
                Ev::BufferingStart | Ev::BufferingEnd => {
                    if self.played_once && !(*event == Ev::BufferingStart && self.seek_pending()) {
                        self.observe(Observation::Rebuffering(Observed {
                            value: *event == Ev::BufferingStart, from: EvidenceSource::BackendReport }));
                    }
                    None
                }
                Ev::TrackSelected => None,
                Ev::VideoInfo => {
                    self.replace_window(message.dims, mt);
                    None
                }
                _ => None,
            };
            if let Some(value) = playback {
                self.observe(Observation::Playback(Observed { value, from: EvidenceSource::BackendReport }));
            }
        }
        if ended { self.begin_close(mt, now_ms); }
        else { self.maybe_subscribe(mt, now_ms); }
    }

    /// Hold `source` as the envelope to send and tell the sequence its facts.
    fn take_source(&mut self, source: AcbSource, mt: &MainThread, now_ms: u64) {
        let video = source.video;
        self.acb_source = Some(source);
        self.acb_step(AcbEvent::Video(video), mt, now_ms);
    }

    /// The 4.5 service sends `videoInfo` and never the ACB schema, so the envelope is the
    /// firmware's own spelling of those facts (`acb_envelope`). An envelope the service sent in the
    /// ACB schema is always preferred; among derived ones the latest wins.
    fn derive_source(&mut self, info: &Ls2VideoInfo, mt: &MainThread, now_ms: u64) {
        if self.acb_source.as_ref().is_some_and(|held| !held.is_derived()) { return; }
        let source = AcbSource::derived(info, LOAD_BPS.is_adaptive());
        if self.acb_source.as_ref() == Some(&source) { return; }
        if self.acb_source.is_none() { plx_base::eventlog::log("uri-acb: envelope derived from videoInfo"); }
        self.take_source(source, mt, now_ms);
    }

    /// The ACB gate, run through the shared sequence's own first step before anything is opened.
    fn acb_gate(&mut self, mt: &MainThread) -> bool {
        self.acb = AcbBind::new(false);
        self.acb_refusal = None;
        let now_ms = self.config.clock.now_ms();
        self.acb_step(AcbEvent::Start, mt, now_ms);
        self.acb.passed_gate()
    }

    /// Step the shared ACB sequence and run what it asks, each answer fed straight back.
    fn acb_step(&mut self, first: AcbEvent, mt: &MainThread, now_ms: u64) {
        let mut queue = VecDeque::from([first]);
        while let Some(event) = queue.pop_front() {
            let mut out = Vec::new();
            self.acb.step(event, now_ms, &mut out);
            for effect in out {
                match effect {
                    AcbFx::Run { request, step, op, retry } => {
                        if !retry {
                            plx_base::eventlog::log(&format!("uri-acb: request={} step={step:?}", request.number()));
                        }
                        let executed = self.acb_run(&op, mt, now_ms);
                        let completed_ms = self.config.clock.now_ms();
                        queue.push_back(match executed.verdict {
                            Verdict::Invoked(invoked) => AcbEvent::Answered { request, invoked, completed_ms },
                            Verdict::Deferred => AcbEvent::Deferred { request },
                            Verdict::Refused(why) => {
                                plx_base::eventlog::log(&format!("uri-acb: bind not granted (Refused({why:?}))"));
                                self.acb_refusal = Some(why);
                                AcbEvent::Answered { request, invoked: false, completed_ms }
                            }
                        });
                    }
                    AcbFx::WaitingVideo => plx_base::eventlog::log(
                        "uri-acb: waiting for the service's source envelope (real video facts)"),
                    AcbFx::Answered { step, invoked } => plx_base::eventlog::log(
                        &format!("uri-acb: answer step={step:?} invoked={invoked} (not plane proof)")),
                    AcbFx::GatePassed => {}
                    AcbFx::Bound => {
                        plx_base::eventlog::log("uri-acb: bound (a dispatch result, not a picture)");
                        if self.transport_paused { self.mirror_pause(mt); }
                    }
                    AcbFx::Failed(failure) => {
                        // Before a session exists (the gate) the caller refuses the start itself.
                        if self.active.is_some() && self.lifecycle.session().is_some() {
                            let failure = match failure {
                                AcbFailure::Refused => match self.acb_refusal {
                                    Some(Refusal::NoPlane) => Failure::Unavailable,
                                    Some(Refusal::Bound) => Failure::Busy,
                                    Some(Refusal::Quarantined(_)) => Failure::ReleaseUncertain,
                                    None => Failure::Backend,
                                },
                                AcbFailure::Timeout | AcbFailure::VideoTimeout => Failure::TimedOut,
                            };
                            plx_base::eventlog::log(&format!("uri-acb: the bind failed ({failure:?}); closing"));
                            self.observe(Observation::Failed(failure));
                            self.begin_close(mt, now_ms);
                        }
                    }
                }
            }
        }
    }

    /// Perform one step of the sequence. The descriptor is only ever the held envelope
    /// ([`Self::acb_source`]) for exactly these facts; nothing is built here.
    fn acb_run(&mut self, op: &crate::acb_bind::Op, mt: &MainThread, now_ms: u64) -> Executed {
        let source = self.acb_source.as_ref();
        let mut descriptor = |media_id: &crate::uri_protocol::MediaId, video: crate::uri_protocol::VideoFacts| {
            source.filter(|source| &source.media_id == media_id && source.video == video)
                .map(|source| Descriptor { text: source.text().to_owned(),
                    label: if source.is_derived() { "derived-envelope" } else { "observed-envelope" } })
                .ok_or("the service reported no source envelope for these video facts")
        };
        let executed = acb_drive::execute(op, mt, self.config.display, self.config.plane, &mut descriptor,
            &mut |_, line| plx_base::eventlog::log(&format!("uri-acb: {line}")));
        if let (true, Some(session)) = (executed.touched, self.lifecycle.session()) {
            self.feed(LifeEvent::AcbTouched { session }, mt, now_ms);
        }
        executed
    }

    /// Mirror an accepted transport pause or resume to the plane, as Feed does (Kodi parity: the
    /// pipeline's Pause/Play alone leave the ACB play state stale). Only once the bind is complete.
    fn mirror_transport(&mut self, operation: Operation, mt: &MainThread) {
        match operation {
            Operation::Pause => {
                self.transport_paused = true;
                self.mirror_pause(mt);
            }
            Operation::Play => {
                self.transport_paused = false;
                if self.plane_paused && self.acb.bound() {
                    self.plane_paused = false;
                    // SAFETY: main thread, witnessed by `mt`; the bind completed.
                    unsafe { self.config.display.plane_resume(mt) };
                }
            }
            _ => {}
        }
    }

    fn mirror_pause(&mut self, mt: &MainThread) {
        if self.config.binding == Binding::Acb && self.acb.bound() && !self.plane_paused {
            self.plane_paused = true;
            // SAFETY: main thread, witnessed by `mt`; the bind completed.
            unsafe { self.config.display.plane_pause(mt) };
        }
    }

    fn feed(&mut self, first: LifeEvent, mt: &MainThread, now_ms: u64) {
        let mut queue = VecDeque::from([first]);
        while let Some(event) = queue.pop_front() {
            let mut effects = Vec::new();
            self.lifecycle.step(event, &mut effects);
            for effect in effects {
                if let LifeFx::Call { request, call, .. } = &effect {
                    if let Some(pending) = self.dispatching {
                        let matches = matches!((call, pending.kind),
                            (UriCall::Play { .. }, ControlKind::Transport)
                                | (UriCall::Pause { .. }, ControlKind::Transport)
                                | (UriCall::Seek { .. }, ControlKind::Seek)
                                | (UriCall::SelectTrack { .. }, ControlKind::Track(_)));
                        if matches { self.controls.insert(*request, pending); }
                    }
                    // The select the service is about to be asked for: the one place the native index
                    // that goes out is named, with the Plex stream it stands for (the confirmation does
                    // not say which index it confirms). Audio only; the subtitle path is unchanged.
                    if let (UriCall::SelectTrack { kind: "audio", index, .. }, Some(PendingControl {
                        kind: ControlKind::Track(track), .. })) = (call, self.dispatching) {
                        plx_base::eventlog::log(&format!(
                            "uri: selectTrack sent kind=audio index={index} stream={}", track.id));
                    }
                }
                if let Some(answer) = session_io::transport_answer(
                    &effect, self.config.transport, self.url.as_deref().unwrap_or(""), &self.config.app_id,
                ) {
                    queue.push_back(answer);
                    continue;
                }
                match effect {
                    LifeFx::Deliver { method, ok, message } => self.delivered(method, ok, message, mt, now_ms),
                    // The refused identity's registration is released: the ladder's next rung registers.
                    LifeFx::Released if self.reopen.is_some() => {
                        if let Some(next) = self.reopen.take() { queue.push_back(LifeEvent::Open(next)); }
                    }
                    LifeFx::Released => {
                        self.release_notice(ReleaseStatus::Released);
                        if self.window { session_io::destroy_exported_window(self.config.display, mt); }
                        self.window = false;
                        self.window_id = None;
                        self.window_coded = None;
                        self.active = None;
                        self.url = None;
                        self.acb_source = None;
                        self.tracks.clear();
                        self.audio_current = None;
                        self.dispatching = None;
                        self.controls.clear();
                        self.loaded = false;
                        self.play_sent = false;
                        self.played_once = false;
                        self.subscribed = false;
                    }
                    LifeFx::Fault => {
                        // A quiet close that never completed is now the attempt's release: the player
                        // hears it began, as it would for any close, before it hears it is stuck.
                        if self.reopen.take().is_some() && !self.close_reported {
                            self.close_reported = true;
                            self.release_notice(ReleaseStatus::Closing);
                        }
                        self.observe(Observation::Failed(Failure::ReleaseUncertain));
                    }
                    LifeFx::UnloadAcb { session } => {
                        // However the close began (a Stop, the service ending, an unsolicited bus
                        // release), no ACB verb may follow UNLOADED: a stray display step would
                        // send PLAYING over a release in flight.
                        self.acb.step(AcbEvent::Cancel, now_ms, &mut Vec::new());
                        self.acb_source = None;
                        let plane = self.config.plane.unload(self.config.display, mt);
                        queue.push_back(LifeEvent::AcbUnloadDispatched { session, plane });
                    }
                    LifeFx::AcbReleased | LifeFx::AcbQuarantined(_) | LifeFx::Open { .. } | LifeFx::Call { .. } | LifeFx::Close { .. } => {}
                }
            }
            if self.active.is_some() {
                if self.lifecycle.phase() == Phase::Closing && !self.close_reported && self.reopen.is_none() {
                    self.close_reported = true;
                    self.release_notice(ReleaseStatus::Closing);
                }
                if self.lifecycle.phase() == Phase::Quarantined && !self.quarantine_reported {
                    self.quarantine_reported = true;
                    self.release_notice(ReleaseStatus::Quarantined);
                }
            }
        }
    }
}

impl UriBackend {
    fn dispatch(&mut self, mt: &MainThread, attempt: AttemptId,
        command: CommandId, operation: Operation) -> DispatchResult {
        if let Operation::Start { source, requirements } = operation {
            return self.start(mt, attempt, source, requirements);
        }
        if self.active != Some(attempt) { return DispatchResult::Refused(Failure::Stale); }
        let now_ms = self.config.clock.now_ms();
        if matches!(operation, Operation::Stop(_)) {
            if self.lifecycle.phase() == Phase::Quarantined {
                return DispatchResult::Refused(Failure::ReleaseUncertain);
            }
            self.begin_close(mt, now_ms);
            return DispatchResult::Queued;
        }
        if self.lifecycle.phase() != Phase::Open || !self.loaded {
            return DispatchResult::Refused(Failure::Busy);
        }
        let Some(media_id) = self.lifecycle.media_id().cloned() else { return DispatchResult::Refused(Failure::Busy); };
        let (call, control) = match operation {
            Operation::Play if self.config.capabilities.pause => {
                if self.controls.values().any(|pending| matches!(pending.kind, ControlKind::Track(_))) {
                    return DispatchResult::Refused(Failure::Busy);
                }
                self.play_sent = true;
                (UriCall::Play { media_id }, Some(ControlKind::Transport))
            }
            Operation::Pause if self.config.capabilities.pause =>
                (UriCall::Pause { media_id }, Some(ControlKind::Transport)),
            Operation::Seek(time) if self.config.capabilities.seek => {
                let Ok(position) = i64::try_from(time.as_millis()) else { return DispatchResult::Refused(Failure::Unsupported); };
                (UriCall::Seek { media_id, position }, Some(ControlKind::Seek))
            }
            Operation::SelectTrack { kind, choice } => {
                let enabled = match kind {
                    TrackKind::Audio => self.config.capabilities.audio_track,
                    TrackKind::Subtitle => self.config.capabilities.subtitle_track,
                };
                if !enabled { return DispatchResult::Refused(Failure::Unsupported); }
                let TrackChoice::Selected(id) = choice else {
                    return DispatchResult::Refused(Failure::Unsupported);
                };
                let track = TrackSelection { kind, id };
                if self.controls.values().any(|pending| matches!(pending.kind,
                    ControlKind::Track(previous) if previous.kind == kind)) {
                    return DispatchResult::Refused(Failure::Busy);
                }
                let Some(index) = native_index(&self.tracks, track) else {
                    return DispatchResult::Refused(Failure::Unsupported);
                };
                if kind == TrackKind::Audio && self.audio_current == Some(index) {
                    // The service reported this very track selected: it will confirm nothing (a
                    // selection that changes nothing sends no `trackSelected`), and the startup
                    // plan holds the first Play behind this command. It is settled here, with
                    // the evidence that settles it, and no native call is made.
                    plx_base::eventlog::log(&format!(
                        "uri: audio index={index} is already selected (audioInfo); no selectTrack sent"));
                    self.observe(Observation::TrackSelected(Observed {
                        value: track, from: EvidenceSource::BackendReport }));
                    self.pending.push_back(DriverEvent::TrackSettled { attempt, command, kind,
                        choice: TrackChoice::Selected(id) });
                    return DispatchResult::Queued;
                }
                (UriCall::SelectTrack { media_id, kind: match kind { TrackKind::Audio => "audio", TrackKind::Subtitle => "text" }, index }, Some(ControlKind::Track(track)))
            }
            _ => return DispatchResult::Refused(Failure::Unsupported),
        };
        self.dispatching = control.map(|kind| PendingControl { command, operation, kind,
            deadline_ms: now_ms.saturating_add(self.config.startup_timeout_ms) });
        self.feed(LifeEvent::Call(call), mt, now_ms);
        self.dispatching = None;
        DispatchResult::Queued
    }

    fn poll(&mut self, mt: &MainThread) -> Vec<DriverEvent> {
        let now_ms = self.config.clock.now_ms();
        if self.staged.as_ref().is_some_and(|source| now_ms >= source.deadline_ms) {
            self.staged = None;
        }
        if let Some(session) = self.lifecycle.poll_session() {
            match self.config.transport.poll(session) {
                Ok(poll) => {
                    for reply in poll.replies { self.feed(LifeEvent::Reply(reply), mt, now_ms); }
                    self.feed(LifeEvent::Status { session, status: poll.close }, mt, now_ms);
                }
                Err(_) => self.feed(LifeEvent::PollFailed { session }, mt, now_ms),
            }
        }
        self.feed(LifeEvent::Tick { now_ms }, mt, now_ms);
        if let Some(session) = self.lifecycle.session().filter(|session| self.lifecycle.acb_fence_due(*session)) {
            // UNLOADED is in flight: drain the firmware's completions into the plane owner, and
            // let the lifecycle release (or quarantine) on what it says.
            let plane = self.config.plane.pump(self.config.display, mt);
            self.feed(LifeEvent::AcbPlane { session, plane }, mt, now_ms);
        }
        self.acb_step(AcbEvent::Tick, mt, now_ms);
        // After this poll's replies: a clock sample delivered in the same poll as the completions
        // is a sample from before them, never the advance that follows them.
        self.watch_plane_show(mt);
        if self.start_deadline_ms.is_some_and(|deadline| now_ms >= deadline)
            && matches!(self.lifecycle.phase(), Phase::Opening | Phase::Open) {
            self.observe(Observation::Failed(Failure::TimedOut));
            self.begin_close(mt, now_ms);
        }
        if self.lifecycle.phase() == Phase::Open {
            let expired: Vec<_> = self.controls.iter().filter_map(|(request, pending)|
                (now_ms >= pending.deadline_ms).then_some(*request)).collect();
            for request in expired {
                let Some(pending) = self.controls.remove(&request) else { continue };
                self.retire_control(request);
                // The refusal reaches the common startup owner, which ends a start whose
                // mandatory control it was and logs nothing: this line names the closer.
                plx_base::eventlog::log(&format!(
                    "uri: control {:?} unconfirmed after {} ms; refused (TimedOut)",
                    pending.operation, self.config.startup_timeout_ms));
                if let Some(attempt) = self.active {
                    self.pending.push_back(DriverEvent::ControlRefused { attempt,
                        command: pending.command, operation: pending.operation,
                        failure: Failure::TimedOut });
                }
                if pending.operation == Operation::Play { self.play_sent = false; }
            }
        }
        self.pending.drain(..).collect()
    }
}

/// A URI-only set composes with the common runner directly. A larger application may wrap it
/// alongside Feed; neither backend can claim the other's attempt or staged locator.
impl DriverSet for UriBackend {
    type Context = MainThread;

    fn capabilities(&self, backend: BackendKind) -> Option<Capabilities> {
        (backend == BackendKind::Uri).then_some(self.config.capabilities)
    }

    fn dispatch(&mut self, backend: BackendKind, mt: &MainThread, attempt: AttemptId,
        command: CommandId, operation: Operation) -> DispatchResult {
        if backend != BackendKind::Uri { return DispatchResult::Refused(Failure::Unavailable); }
        UriBackend::dispatch(self, mt, attempt, command, operation)
    }

    fn poll(&mut self, mt: &MainThread) -> Vec<BackendEvent> {
        UriBackend::poll(self, mt).into_iter().map(|event| BackendEvent {
            backend: BackendKind::Uri, event,
        }).collect()
    }

    fn discard_staged(&mut self, backend: BackendKind, attempt: AttemptId, key: SourceKey) {
        if backend == BackendKind::Uri
            && self.staged.as_ref().is_some_and(|staged| staged.attempt == attempt && staged.key == key) {
            self.staged = None;
        }
    }

    fn exit_disposition(&self) -> ExitDisposition { UriBackend::exit_disposition(self) }
}

/// The adapter's view of this driver beyond `DriverSet`: the contract names no driver crate, so the
/// player holds `Box<dyn UriDriver>` and the composition root boxes a [`UriBackend`] into it.
impl UriDriver for UriBackend {
    fn stage_source(&mut self, attempt: AttemptId, key: SourceKey, source: UriSource)
        -> Result<(), Failure> {
        UriBackend::stage_source(self, attempt, key, source)
    }

    fn cancel_staged(&mut self, attempt: AttemptId) { UriBackend::cancel_staged(self, attempt) }

    fn plane_owned(&self) -> bool { UriBackend::plane_owned(self) }

    fn maps_track(&self, selection: TrackSelection) -> bool { UriBackend::maps_track(self, selection) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn composition_selects_the_acb_plane_only_when_the_gate_holds() {
        use plx_platform::tv::plane::PlaneFacts;
        use plx_platform::tv::sink::{VP_ACB, VP_NONE};
        let facts = PlaneFacts { handle: true, player_type: 0 };
        // The exported-window table has no say on an ACB display: not even a listed major changes
        // the ACB answer, and a failed gate stays Unbound rather than falling to the exported window.
        for (major, strata) in [(0, &[][..]), (4, &[][..]), (11, &[11][..]), (11, &[][..])] {
            let at = |mode, plane, initialized| product_binding(mode, plane, initialized, major, strata);
            assert_eq!(at(VP_ACB, facts, true), Binding::Acb, "major {major}");
            assert_eq!(at(VP_ACB, facts, false), Binding::Unbound, "a created handle is not an initialized one");
            assert_eq!(at(VP_ACB, PlaneFacts { handle: true, player_type: 10 }, true), Binding::Unbound);
            assert_eq!(at(VP_ACB, PlaneFacts::ABSENT, true), Binding::Unbound);
            assert_eq!(at(VP_NONE, facts, true), Binding::Unbound);
        }
    }

    /// The exported window is selected per firmware major, from the table the caller passes
    /// (production's is `EXPORTED_STRATA`; a test passes its own, so no table forks the build).
    #[test]
    fn the_exported_window_is_selected_only_for_a_listed_firmware_major() {
        use plx_platform::tv::plane::PlaneFacts;
        use plx_platform::tv::sink::{VP_ACB, VP_NONE};
        let exported = Binding::Exported { canvas: EXPORTED_CANVAS };
        assert_eq!(EXPORTED_CANVAS, (1_920, 1_080));
        // A 5+ set has no ACB handle; a handle that exists must not change the answer either.
        for plane in [PlaneFacts::ABSENT, PlaneFacts { handle: true, player_type: 0 }] {
            for initialized in [false, true] {
                let at = |mode, major, strata: &[u32]| product_binding(mode, plane, initialized, major, strata);
                assert_eq!(at(VP_EXPORTED, 11, &[11]), exported, "a listed major");
                assert_eq!(at(VP_EXPORTED, 5, &[5, 11]), exported, "one of several");
                assert_eq!(at(VP_EXPORTED, 10, &[11]), Binding::Unbound, "an unlisted major");
                assert_eq!(at(VP_EXPORTED, 12, &[11]), Binding::Unbound, "nothing is inferred from a neighbour");
                assert_eq!(at(VP_EXPORTED, 0, &[11]), Binding::Unbound, "unknown firmware");
                assert_eq!(at(VP_EXPORTED, 0, &[0, 11]), Binding::Unbound, "0 is never admitted, even if listed");
                assert_eq!(at(VP_EXPORTED, 11, &[]), Binding::Unbound, "an empty table admits nothing");
                assert_eq!(at(VP_NONE, 11, &[11]), Binding::Unbound, "no window path to bind");
                assert_ne!(at(VP_ACB, 11, &[11]), exported, "the table is for the exported window only");
            }
        }
    }

    use super::*;
    use plx_platform::tv::sink::{NoSink, PlaneUnload, ScriptedPlaneSink, ScriptedWindowSink, WindowCall};
    use plx_platform::tv::uri::{CloseStatus, RequestId, SessionId, UriPoll, UriReply};
    use plx_playback_contract::{BackendState, Event as ContractEvent, Fidelity,
        Fx as ContractFx, InitialPlayback, Intent, Requirements, SourceKind, StopReason};
    use std::sync::{atomic::{AtomicU64, Ordering}, Mutex};

    const CAPS: Capabilities = Capabilities { progressive_uri: true, hls_uri: false,
        packet_feed: false, pause: true,
        seek: true, audio_track: true, subtitle_track: true, audio_off: false, subtitle_off: false };
    const REQ: Requirements = Requirements { source: SourceKind::ProgressiveUri,
        video: Fidelity::Exact, audio: Fidelity::Exact, audio_track: TrackChoice::Unspecified,
        subtitle_track: TrackChoice::Unspecified };
    const INITIAL: InitialPlayback = InitialPlayback { position: None, paused: false };
    const KEY: SourceKey = SourceKey::new(17);
    const A: AttemptId = AttemptId::new(7, 1);
    const B: AttemptId = AttemptId::new(7, 2);
    const URL: &str = "https://private.invalid/file?X-Plex-Token=SECRET";

    struct Clock(std::sync::Arc<AtomicU64>);
    impl MonotonicClock for Clock { fn now_ms(&self) -> u64 { self.0.load(Ordering::Relaxed) } }
    struct BusState { session: SessionId, serial: u64,
        calls: Vec<(&'static str, RequestId, String)>, replies: VecDeque<UriReply>,
        close: CloseStatus, hold: bool, refuse_open: bool, fail_poll: bool, shapes: Vec<Shape> }
    struct Bus(Mutex<BusState>);
    impl Bus {
        fn new() -> Self { Self(Mutex::new(BusState { session: SessionId::new(0),
            serial: 0, calls: Vec::new(), replies: VecDeque::new(),
            close: CloseStatus::Released, hold: false, refuse_open: false, fail_poll: false,
            shapes: Vec::new() })) }
        fn say(&self, method: &'static str, payload: &str) {
            let mut state = self.0.lock().unwrap();
            let request = if method == "register" { RequestId::new(0) } else {
                state.calls.iter().rev().find(|(name, _, _)| *name == method)
                    .map(|(_, request, _)| *request).expect("call was sent") };
            let session = state.session;
            state.replies.push_back(UriReply { session, request, method,
                payload: payload.into(), ok: true });
        }
        fn calls(&self, method: &str) -> usize {
            self.0.lock().unwrap().calls.iter().filter(|(name, _, _)| *name == method).count()
        }
        /// The identities the driver asked to register as, in order.
        fn shapes(&self) -> Vec<Shape> { self.0.lock().unwrap().shapes.clone() }
        /// The worker's answer to a registration the hub refused: a failed `register` reply, and
        /// the release the worker confirms because no live handle was ever made.
        fn refuse_register(&self) {
            let mut state = self.0.lock().unwrap();
            let session = state.session;
            state.replies.push_back(UriReply { session, request: RequestId::new(0), method: "register",
                payload: "code -1027: Invalid permissions for com.webos.media.client.plxprobe".into(),
                ok: false });
            state.close = CloseStatus::Released;
        }
    }
    impl UriMedia for Bus {
        fn open(&self, shape: Shape) -> Result<SessionId, String> {
            let mut state = self.0.lock().unwrap();
            state.shapes.push(shape);
            if state.refuse_open { return Err("refused".into()); }
            state.serial += 1;
            state.session = SessionId::new(state.serial);
            state.close = CloseStatus::Open;
            Ok(state.session)
        }
        fn call(&self, session: SessionId, request: RequestId, method: &'static str,
            payload: &str) -> Result<(), String> {
            let mut state = self.0.lock().unwrap();
            assert_eq!(session, state.session);
            state.calls.push((method, request, payload.into()));
            Ok(())
        }
        fn poll(&self, session: SessionId) -> Result<UriPoll, String> {
            let mut state = self.0.lock().unwrap();
            assert_eq!(session, state.session);
            if state.fail_poll { return Err("poll failed".into()); }
            let close = state.close;
            Ok(UriPoll { replies: state.replies.drain(..).collect(), close })
        }
        fn close(&self, session: SessionId) -> Result<(), String> {
            let mut state = self.0.lock().unwrap();
            assert_eq!(session, state.session);
            state.close = if state.hold { CloseStatus::Closing } else { CloseStatus::Released };
            Ok(())
        }
    }
    struct Rig { driver: UriBackend, clock: std::sync::Arc<AtomicU64>, bus: &'static Bus,
        mt: MainThread, command: plx_playback_contract::CommandId }
    impl Rig {
        fn new() -> Self {
            Self::with(Binding::Unbound, Box::leak(Box::new(NoSink)),
                Box::leak(Box::new(plx_platform::tv::plane::PlaneOwner::new())))
        }
        /// An ACB-bound backend over a scripted sink and its own plane owner, booted as a normal boot
        /// boots it: Feed's handle (player type 10) and the URI engine's (player type 0, the
        /// measured sequence), each confirmed initialized.
        fn acb() -> (Self, &'static ScriptedPlaneSink, &'static plx_platform::tv::plane::PlaneOwner) {
            let sink: &'static ScriptedPlaneSink =
                Box::leak(Box::new(ScriptedPlaneSink::new(PlaneUnload { rv: 1, task: 5 })));
            let plane: &'static plx_platform::tv::plane::PlaneOwner =
                Box::leak(Box::new(plx_platform::tv::plane::PlaneOwner::new()));
            plane.boot(plx_platform::tv::plane::PlaneFacts { handle: true, player_type: 10 });
            plane.boot_handle(plx_platform::tv::plane::PlaneHandle::Uri,
                plx_platform::tv::plane::PlaneFacts { handle: true, player_type: 0 },
                plx_platform::tv::plane::HandleSeat { id: 0, initialized: true });
            (Self::with(Binding::Acb, sink, plane), sink, plane)
        }
        fn with(binding: Binding, display: &'static dyn DisplaySink,
            plane: &'static plx_platform::tv::plane::PlaneOwner) -> Self {
            let clock = std::sync::Arc::new(AtomicU64::new(0));
            let bus = Box::leak(Box::new(Bus::new()));
            let driver = UriBackend::new(Config { transport: bus, display, plane,
                clock: Box::new(Clock(clock.clone())),
                app_id: "app.test".into(), binding, capabilities: CAPS,
                startup_timeout_ms: 500, release_allowance_ms: 100, source_ttl_ms: 50 });
            let mut state = BackendState::new();
            let mut fx = Vec::new();
            state.step(ContractEvent::Intent(Intent::Start {
                backend: BackendKind::Uri, attempt: A, source: KEY, requirements: REQ,
                initial: INITIAL }), Some(CAPS), &mut fx);
            let [ContractFx::Dispatch { command, .. }] = fx.as_slice() else { panic!("dispatch") };
            Self { driver, clock, bus, mt: unsafe { MainThread::assume() }, command: *command }
        }
        fn start(&mut self, attempt: AttemptId) -> DispatchResult {
            self.start_with(attempt, vec![
                NativeTrackIndex { selection: TrackSelection { kind: TrackKind::Audio, id: 3 }, index: 3 },
                NativeTrackIndex { selection: TrackSelection { kind: TrackKind::Audio, id: 4 }, index: 4 },
            ])
        }
        /// Start `attempt` on a source carrying `tracks`.
        fn start_with(&mut self, attempt: AttemptId, tracks: Vec<NativeTrackIndex>) -> DispatchResult {
            self.driver.stage_source(attempt, KEY, UriSource::new(URL.into(), tracks)).unwrap();
            self.driver.dispatch(&self.mt, attempt, self.command,
                Operation::Start { source: KEY, requirements: REQ })
        }
        fn opened(&mut self) {
            assert_eq!(self.start(A), DispatchResult::Queued);
            self.finish_open();
        }
        /// The service registers and loads: the attempt is open and awaiting the first Play.
        fn finish_open(&mut self) {
            self.bus.say("register", "");
            assert!(self.driver.poll(&self.mt).is_empty());
            assert_eq!(self.bus.calls("load"), 1);
            self.bus.say("load", r#"{"returnValue":true,"mediaId":"m1","loadCompleted":true}"#);
            assert_eq!(self.driver.poll(&self.mt), [DriverEvent::Observation { attempt: A,
                observation: Observation::Playback(Observed { value: Playback::ReadyAwaitPlay,
                    from: EvidenceSource::BackendReport }) }]);
            assert_eq!(self.bus.calls("subscribe"), 1);
            assert_eq!(self.bus.calls("play"), 0);
        }
    }

    #[test]
    fn an_accepted_playing_is_logged_and_traced_in_the_probes_words() {
        plx_base::eventlog::with_private_log(|| {
            let mut rig = Rig::new(); rig.opened();
            assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Play), DispatchResult::Queued);
            rig.bus.say("play", r#"{"returnValue":true,"mediaId":"m1","playing":true,"currentTime":123}"#);
            rig.driver.poll(&rig.mt);
            let log = std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default();
            assert!(log.contains("uri: service playing"), "{log}");
            #[cfg(feature = "devtriggers")]
            {
                assert!(log.contains("uri-trace: t=") && log.contains(" ev=playing\n"), "{log}");
                assert!(log.contains(" ev=currentTime value=123 (first report)"), "{log}");
            }
        });
    }

    #[test]
    fn native_clock_and_ack_never_prove_a_picture_and_controls_remain_addressed() {
        let mut rig = Rig::new(); rig.opened();
        assert!(rig.driver.url.is_none(), "load consumed the private locator");
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Play), DispatchResult::Queued);
        rig.bus.say("play", r#"{"returnValue":true,"mediaId":"m1","playing":true,"currentTime":123}"#);
        let notices = rig.driver.poll(&rig.mt);
        assert!(notices.contains(&DriverEvent::Observation { attempt: A,
            observation: Observation::Playback(Observed { value: Playback::Playing,
                from: EvidenceSource::BackendReport }) }));
        assert!(!notices.iter().any(|event| matches!(event, DriverEvent::Observation {
            observation: Observation::Presented, .. })));
        assert_eq!(rig.driver.dispatch(&rig.mt, B, rig.command, Operation::Pause), DispatchResult::Refused(Failure::Stale));
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Pause), DispatchResult::Queued);
        rig.bus.say("pause", r#"{"returnValue":true,"mediaId":"m1","paused":true}"#);
        assert!(rig.driver.poll(&rig.mt).contains(&DriverEvent::Observation { attempt: A,
            observation: Observation::Playback(Observed { value: Playback::Paused,
                from: EvidenceSource::BackendReport }) }));
    }

    #[test]
    fn buffering_is_a_rebuffer_only_after_playback_has_run_and_is_never_guessed_before() {
        let mut rig = Rig::new(); rig.opened();
        let rebuffer = |attempt, value| DriverEvent::Observation { attempt, observation:
            Observation::Rebuffering(Observed { value, from: EvidenceSource::BackendReport }) };
        rig.bus.say("subscribe", r#"{"mediaId":"m1","bufferingStart":true}"#);
        assert!(rig.driver.poll(&rig.mt).is_empty(), "startup buffering is not a rebuffer");
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Play), DispatchResult::Queued);
        rig.bus.say("play", r#"{"returnValue":true,"mediaId":"m1","playing":true,"currentTime":1}"#);
        rig.driver.poll(&rig.mt);
        rig.bus.say("subscribe", r#"{"mediaId":"m1","bufferingStart":true}"#);
        assert_eq!(rig.driver.poll(&rig.mt), [rebuffer(A, true)]);
        rig.bus.say("subscribe", r#"{"mediaId":"m1","bufferingEnd":true}"#);
        assert_eq!(rig.driver.poll(&rig.mt), [rebuffer(A, false)]);
        // Buffering while a Seek is pending is seek priming, not a rebuffer.
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Seek(ContentTime::from_millis(5000))), DispatchResult::Queued);
        rig.bus.say("subscribe", r#"{"mediaId":"m1","bufferingStart":true}"#);
        assert!(!rig.driver.poll(&rig.mt).iter().any(|event| matches!(event,
            DriverEvent::Observation { observation: Observation::Rebuffering(_), .. })));
    }

    #[test]
    fn track_event_before_ack_requires_native_correlation() {
        let mut rig = Rig::new(); rig.opened();
        let track = TrackSelection { kind: TrackKind::Audio, id: 3 };
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::SelectTrack { kind: track.kind, choice: TrackChoice::Selected(track.id) }), DispatchResult::Queued);
        rig.bus.say("subscribe", r#"{"mediaId":"m1","trackSelected":{"type":"audio","index":3}}"#);
        assert!(rig.driver.poll(&rig.mt).is_empty());
        rig.bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Observation { attempt: A,
            observation: Observation::TrackSelected(Observed { value: track,
                from: EvidenceSource::BackendReport }) },
            DriverEvent::TrackSettled { attempt: A, command: rig.command,
                kind: TrackKind::Audio, choice: TrackChoice::Selected(track.id) }]);
        // The service now has track 3 selected: asking for it again changes nothing, so no native
        // call is made and the command is settled from that report.
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::SelectTrack { kind: track.kind, choice: TrackChoice::Selected(track.id) }),
            DispatchResult::Queued);
        assert_eq!(rig.bus.calls("selectTrack"), 1);
        assert_eq!(rig.driver.poll(&rig.mt), track_settled(&rig));
        // Away and back: the first request for index 3 is retired, so the second one is the only
        // match for its `trackSelected` and the repeated selection is not ambiguous.
        let other = TrackSelection { kind: TrackKind::Audio, id: 4 };
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::SelectTrack { kind: other.kind, choice: TrackChoice::Selected(other.id) }),
            DispatchResult::Queued);
        rig.bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
        rig.bus.say("subscribe", r#"{"mediaId":"m1","trackSelected":{"type":"audio","index":4}}"#);
        assert_eq!(rig.driver.poll(&rig.mt).len(), 2);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::SelectTrack { kind: track.kind, choice: TrackChoice::Selected(track.id) }),
            DispatchResult::Queued);
        assert_eq!(rig.bus.calls("selectTrack"), 3, "away from the service's track, it is asked");
        rig.bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
        assert!(rig.driver.poll(&rig.mt).is_empty());
        rig.bus.say("subscribe", r#"{"mediaId":"m1","trackSelected":{"type":"audio","index":3}}"#);
        assert_eq!(rig.driver.poll(&rig.mt), track_settled(&rig),
            "a retired native request cannot make a repeated selection ambiguous");
    }

    /// **W6.** The select the backend SENDS is named in the log with the kind, the native index and the
    /// Plex stream id, so a harness can tell which index went out (the confirmation says nothing of it).
    /// RED while the backend wrote no line for a sent select. The index differs from the stream id on
    /// purpose: the two numberings must not be mistaken for each other.
    #[test]
    fn an_audio_select_that_is_sent_names_its_kind_native_index_and_stream_id() {
        plx_base::eventlog::with_private_log(|| {
            let mut rig = Rig::new();
            assert_eq!(rig.start_with(A, vec![
                NativeTrackIndex { selection: TrackSelection { kind: TrackKind::Audio, id: 99100102 }, index: 1 },
            ]), DispatchResult::Queued);
            rig.finish_open();
            let select = Operation::SelectTrack { kind: TrackKind::Audio, choice: TrackChoice::Selected(99100102) };
            assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, select), DispatchResult::Queued);
            assert_eq!(rig.bus.calls("selectTrack"), 1);
            let log = std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default();
            assert!(log.contains("uri: selectTrack sent kind=audio index=1 stream=99100102\n"), "{log}");
            assert_eq!(log.matches("uri: selectTrack sent").count(), 1, "{log}");
            assert!(!log.contains("no selectTrack sent"), "{log}");
        });
    }

    const SELECT_AUDIO_3: Operation = Operation::SelectTrack { kind: TrackKind::Audio,
        choice: TrackChoice::Selected(3) };
    const AUDIO_3: TrackSelection = TrackSelection { kind: TrackKind::Audio, id: 3 };

    fn track_settled(rig: &Rig) -> [DriverEvent; 2] {
        [DriverEvent::Observation { attempt: A, observation: Observation::TrackSelected(Observed {
            value: AUDIO_3, from: EvidenceSource::BackendReport }) },
         DriverEvent::TrackSettled { attempt: A, command: rig.command, kind: TrackKind::Audio,
            choice: TrackChoice::Selected(AUDIO_3.id) }]
    }

    /// The 4.5 service names the audio track it selected in `audioInfo` and sends no
    /// `trackSelected` for a selection that changes nothing, so a startup selection of that very
    /// track is already settled (RED: the control waited for an event that never came, held the
    /// first Play behind it and timed out after the start deadline).
    #[test]
    fn the_audio_track_the_service_reports_selected_needs_no_native_selection() {
        plx_base::eventlog::with_private_log(|| {
            let mut rig = Rig::new(); rig.opened();
            rig.bus.say("subscribe", r#"{"audioInfo":{"track":3,"immersive":"none","mediaId":"m1"}}"#);
            assert!(rig.driver.poll(&rig.mt).is_empty());
            assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, SELECT_AUDIO_3), DispatchResult::Queued);
            assert_eq!(rig.bus.calls("selectTrack"), 0, "nothing to select: the service already has it");
            assert_eq!(rig.driver.poll(&rig.mt), track_settled(&rig));
            assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Play), DispatchResult::Queued,
                "no track control is left to hold the Play behind");
            assert_eq!(rig.bus.calls("play"), 1);
            let log = std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default();
            assert!(log.contains("uri: audio index=3 is already selected (audioInfo); no selectTrack sent"), "{log}");
            assert!(!log.contains("uri: selectTrack sent"), "the settle sent nothing, so it names no send: {log}");
        });
    }

    /// The service's report can land after the select is already on the bus: the acknowledgement
    /// and then the report settle it, as `trackSelected` would.
    #[test]
    fn an_audio_report_naming_the_pending_track_settles_it_as_trackselected_does() {
        let mut rig = Rig::new(); rig.opened();
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, SELECT_AUDIO_3), DispatchResult::Queued);
        assert_eq!(rig.bus.calls("selectTrack"), 1, "nothing says the service has it yet");
        rig.bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
        assert!(rig.driver.poll(&rig.mt).is_empty(), "an acknowledgement alone selects nothing");
        rig.bus.say("subscribe", r#"{"audioInfo":{"track":3,"mediaId":"m1"}}"#);
        assert_eq!(rig.driver.poll(&rig.mt), track_settled(&rig));
    }

    /// Only the loaded media's report about the very track asked for counts.
    #[test]
    fn an_audio_report_of_another_track_or_media_settles_nothing() {
        let mut rig = Rig::new(); rig.opened();
        rig.bus.say("subscribe", r#"{"audioInfo":{"track":1,"mediaId":"m1"}}"#);
        rig.driver.poll(&rig.mt);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, SELECT_AUDIO_3), DispatchResult::Queued);
        assert_eq!(rig.bus.calls("selectTrack"), 1, "the service has another track selected");
        rig.bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
        rig.bus.say("subscribe", r#"{"audioInfo":{"track":3,"mediaId":"another"}}"#);
        assert!(rig.driver.poll(&rig.mt).is_empty());
        rig.bus.say("subscribe", r#"{"audioInfo":{"track":1,"mediaId":"m1"}}"#);
        assert!(rig.driver.poll(&rig.mt).is_empty());
    }

    /// A control the service never answers is refused when its deadline passes, and the driver
    /// says which one and why: the refusal ends the start through the common startup owner, which
    /// logs nothing of its own.
    #[test]
    fn an_unconfirmed_control_names_itself_when_its_deadline_refuses_it() {
        plx_base::eventlog::with_private_log(|| {
            let mut rig = Rig::new(); rig.opened();
            assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, SELECT_AUDIO_3), DispatchResult::Queued);
            rig.bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
            rig.at(499);
            assert!(rig.driver.poll(&rig.mt).is_empty());
            rig.at(500);
            assert!(rig.driver.poll(&rig.mt).contains(&DriverEvent::ControlRefused { attempt: A,
                command: rig.command, operation: SELECT_AUDIO_3, failure: Failure::TimedOut }));
            let log = std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default();
            assert!(log.contains("uri: control SelectTrack { kind: Audio, choice: Selected(3) } unconfirmed after 500 ms; refused (TimedOut)"), "{log}");
        });
    }

    #[test]
    fn quarantine_blocks_successor_until_release_and_expired_source_is_not_reused() {
        let mut rig = Rig::new(); rig.opened();
        rig.bus.0.lock().unwrap().hold = true;
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Stop(StopReason::Stop)), DispatchResult::Queued);
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Closing }]);
        rig.driver.stage_source(B, KEY, UriSource::new(URL.into(), Vec::new())).unwrap();
        assert_eq!(rig.driver.dispatch(&rig.mt, B, rig.command,
            Operation::Start { source: KEY, requirements: REQ }), DispatchResult::Refused(Failure::Busy));
        rig.clock.store(101, Ordering::Relaxed);
        assert!(rig.driver.poll(&rig.mt).contains(&DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Quarantined }));
        rig.bus.0.lock().unwrap().close = CloseStatus::Released;
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Released }]);
        assert_eq!(rig.driver.dispatch(&rig.mt, B, rig.command,
            Operation::Start { source: KEY, requirements: REQ }), DispatchResult::Refused(Failure::Refused),
            "a refused Start consumed B's authorization while A was closing");
    }

    #[test]
    fn immediate_registration_refusal_still_reports_confirmed_release() {
        let mut rig = Rig::new();
        rig.bus.0.lock().unwrap().refuse_open = true;
        assert_eq!(rig.start(A), DispatchResult::Queued);
        assert_eq!(rig.driver.poll(&rig.mt), [
            DriverEvent::Observation { attempt: A, observation: Observation::Failed(Failure::Refused) },
            DriverEvent::Release { attempt: A, status: ReleaseStatus::Released },
        ]);
    }

    /// The bus-identity ladder (`register_ladder`): the named client the Developer Mode role file
    /// grants may be refused at register on a set that does not grant it. The attempt then goes
    /// on as the anonymous client instead of failing (RED against the driver that registered as
    /// the named client only: it reported `Failed(Refused)` and released).
    #[test]
    fn a_named_registration_the_hub_refuses_goes_on_as_the_anonymous_client() {
        let mut rig = Rig::new();
        assert_eq!(rig.start(A), DispatchResult::Queued);
        assert_eq!(rig.bus.shapes(), [Shape::Named], "the first rung is the named client");
        rig.bus.refuse_register();
        assert!(rig.driver.poll(&rig.mt).is_empty(),
            "a refused first rung is neither a failure nor a release the player may see");
        assert_eq!(rig.bus.shapes(), [Shape::Named, Shape::Anon],
            "the refused registration is released, then the anonymous client registers");
        assert!(!rig.driver.lifecycle.released() && rig.driver.active == Some(A));
        rig.finish_open();
        assert_eq!(rig.bus.calls("load"), 1, "only the anonymous registration reached a Load");
    }

    /// The first call a registered identity makes is the second place a name that is not granted
    /// shows: the hub answers the Load "Denied method call". The same ladder moves on, and the
    /// locator that Load needs survives the quiet close of the refused registration.
    #[test]
    fn a_load_the_named_identity_is_denied_goes_on_as_the_anonymous_client_with_the_same_locator() {
        let mut rig = Rig::new();
        assert_eq!(rig.start(A), DispatchResult::Queued);
        rig.bus.say("register", "");
        assert!(rig.driver.poll(&rig.mt).is_empty());
        assert_eq!(rig.bus.calls("load"), 1);
        rig.bus.say("load", r#"{"returnValue":false,"errorText":"Denied method call load for com.webos.media.client.plxprobe"}"#);
        assert!(rig.driver.poll(&rig.mt).is_empty(), "a denied first Load on the named client is not a failure");
        assert!(rig.driver.poll(&rig.mt).is_empty());
        assert_eq!(rig.bus.shapes(), [Shape::Named, Shape::Anon]);
        rig.bus.say("register", "");
        assert!(rig.driver.poll(&rig.mt).is_empty());
        assert_eq!(rig.bus.calls("load"), 2);
        assert!(rig.bus.0.lock().unwrap().calls.last().unwrap().2.contains("private.invalid/file"),
            "the second Load carries the same authorized locator");
        rig.bus.say("load", r#"{"returnValue":true,"mediaId":"m1","loadCompleted":true}"#);
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Observation { attempt: A,
            observation: Observation::Playback(Observed { value: Playback::ReadyAwaitPlay,
                from: EvidenceSource::BackendReport }) }]);
    }

    /// A refusal of BOTH rungs takes the one existing path: the same `Failed(Refused)` and release
    /// notices a single refused registration gave before the ladder (the player's URI failure
    /// handling, `adapter::note_uri_failure`, is keyed on that observation, and its handoff to Feed
    /// is pinned by the app's `refused_product_uri_load_recovers_the_same_request_only_after_native_release`
    /// and `a_hub_that_refuses_both_register_rungs_hands_the_same_request_to_feed`). Exactly one
    /// anonymous retry is made, never a third registration.
    #[test]
    fn a_refusal_of_both_rungs_is_the_one_failed_refused_and_a_release_and_no_third_registration() {
        let mut rig = Rig::new();
        assert_eq!(rig.start(A), DispatchResult::Queued);
        rig.bus.refuse_register();
        assert!(rig.driver.poll(&rig.mt).is_empty());
        rig.bus.refuse_register();
        assert_eq!(rig.driver.poll(&rig.mt), [
            DriverEvent::Observation { attempt: A, observation: Observation::Failed(Failure::Refused) },
            DriverEvent::Release { attempt: A, status: ReleaseStatus::Closing },
            DriverEvent::Release { attempt: A, status: ReleaseStatus::Released },
        ]);
        assert_eq!(rig.bus.shapes(), [Shape::Named, Shape::Anon]);
        assert_eq!(rig.bus.calls("load"), 0);
        assert!(rig.driver.poll(&rig.mt).is_empty());
        assert_eq!(rig.bus.shapes().len(), 2, "no third registration");
        assert_eq!(rig.driver.exit_disposition(), ExitDisposition::Clean);
    }

    /// The refused registration's quiet close is bounded by the same release allowance as any
    /// close: when the hub never confirms it the attempt is quarantined and fails, announcing the
    /// close first, and the retry never registers.
    #[test]
    fn a_quiet_close_the_hub_never_confirms_quarantines_and_never_registers_again() {
        let mut rig = Rig::new();
        rig.bus.0.lock().unwrap().hold = true;
        assert_eq!(rig.start(A), DispatchResult::Queued);
        rig.bus.refuse_register();
        rig.bus.0.lock().unwrap().close = CloseStatus::Closing;
        assert!(rig.driver.poll(&rig.mt).is_empty());
        rig.clock.store(101, Ordering::Relaxed);
        assert_eq!(rig.driver.poll(&rig.mt), [
            DriverEvent::Release { attempt: A, status: ReleaseStatus::Closing },
            DriverEvent::Observation { attempt: A, observation: Observation::Failed(Failure::ReleaseUncertain) },
            DriverEvent::Release { attempt: A, status: ReleaseStatus::Quarantined },
        ]);
        rig.bus.0.lock().unwrap().close = CloseStatus::Released;
        let notices = rig.driver.poll(&rig.mt);
        assert!(notices.contains(&DriverEvent::Release { attempt: A, status: ReleaseStatus::Released }), "{notices:?}");
        assert_eq!(rig.bus.shapes(), [Shape::Named], "a late release of a quarantined close is not a retry");
    }

    /// A Stop while the refused registration is still closing ends the attempt: the ladder's
    /// pending retry must not reopen a registration the player has asked to release.
    #[test]
    fn a_stop_during_the_quiet_close_of_a_refused_rung_does_not_register_again() {
        let mut rig = Rig::new();
        rig.bus.0.lock().unwrap().hold = true;
        assert_eq!(rig.start(A), DispatchResult::Queued);
        rig.bus.refuse_register();
        rig.bus.0.lock().unwrap().close = CloseStatus::Closing;
        assert!(rig.driver.poll(&rig.mt).is_empty());
        assert_eq!(rig.bus.shapes(), [Shape::Named], "the refused registration is still closing");
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Stop(StopReason::Stop)),
            DispatchResult::Queued);
        rig.bus.0.lock().unwrap().close = CloseStatus::Released;
        let notices = rig.driver.poll(&rig.mt);
        assert!(notices.contains(&DriverEvent::Release { attempt: A, status: ReleaseStatus::Released }), "{notices:?}");
        assert_eq!(rig.bus.shapes(), [Shape::Named], "a released registration the player stopped is not retried");
    }

    #[test]
    fn terminal_event_and_release_in_one_poll_keep_their_order_and_fence_old_bus_replies() {
        let mut rig = Rig::new(); rig.opened();
        let old_session = rig.bus.0.lock().unwrap().session;
        rig.bus.say("subscribe", r#"{"mediaId":"m1","endOfStream":true}"#);
        rig.bus.0.lock().unwrap().close = CloseStatus::Released;
        assert_eq!(rig.driver.poll(&rig.mt), [
            DriverEvent::Observation { attempt: A, observation: Observation::Playback(Observed {
                value: Playback::Ended, from: EvidenceSource::BackendReport }) },
            DriverEvent::Release { attempt: A, status: ReleaseStatus::Closing },
            DriverEvent::Release { attempt: A, status: ReleaseStatus::Released },
        ]);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Start { source: KEY, requirements: REQ }), DispatchResult::Refused(Failure::Stale));
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Released }],
            "even a duplicate direct Start gets a proven empty-owner verdict");
        assert_eq!(rig.start(B), DispatchResult::Queued);
        assert_ne!(rig.bus.0.lock().unwrap().session, old_session,
            "the transport assigns a new native session across lifecycle resets");
        rig.bus.0.lock().unwrap().replies.push_back(UriReply { session: old_session,
            request: RequestId::new(0), method: "register", payload: String::new(), ok: true });
        assert!(rig.driver.poll(&rig.mt).is_empty());
        assert_eq!(rig.bus.calls("load"), 1, "the previous native generation cannot load the new source");
    }

    #[test]
    fn cancelled_or_expired_staged_locator_cannot_start_an_attempt() {
        let mut rig = Rig::new();
        rig.driver.stage_source(A, KEY, UriSource::new(URL.into(), Vec::new())).unwrap();
        rig.driver.cancel_staged(A);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Start { source: KEY, requirements: REQ }), DispatchResult::Refused(Failure::Refused));
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Released }], "a pre-open refusal has a proven empty owner");
        rig.driver.stage_source(A, KEY, UriSource::new(URL.into(), Vec::new())).unwrap();
        rig.clock.store(50, Ordering::Relaxed);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Start { source: KEY, requirements: REQ }), DispatchResult::Refused(Failure::Stale));
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Released }]);
        assert!(rig.driver.staged.is_none());
        rig.driver.stage_source(B, KEY, UriSource::new(URL.into(), Vec::new())).unwrap();
        rig.clock.store(100, Ordering::Relaxed);
        assert!(rig.driver.poll(&rig.mt).is_empty());
        assert!(rig.driver.staged.is_none(), "poll drops an unused authorization at its deadline");
    }

    #[test]
    fn capability_and_binding_refusals_both_destroy_the_matching_url() {
        let mut rig = Rig::new();
        rig.driver.stage_source(A, KEY, UriSource::new(URL.into(), Vec::new())).unwrap();
        let hls = Requirements { source: SourceKind::HlsUri, ..REQ };
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Start { source: KEY, requirements: hls }), DispatchResult::Refused(Failure::Unsupported));
        assert!(rig.driver.staged.is_none());
        rig.driver.poll(&rig.mt);
        rig.driver.stage_source(A, KEY, UriSource::new(URL.into(), Vec::new())).unwrap();
        rig.driver.config.binding = Binding::Exported { canvas: (1920, 1080) };
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Start { source: KEY, requirements: REQ }), DispatchResult::Refused(Failure::Unsupported));
        assert!(rig.driver.staged.is_none());
        assert_eq!(rig.bus.0.lock().unwrap().serial, 0);
    }

    #[test]
    fn ready_never_plays_or_selects_and_explicit_track_confirmation_gates_play() {
        let mut rig = Rig::new();
        let audio = TrackSelection { kind: TrackKind::Audio, id: 42 };
        let subtitle = TrackSelection { kind: TrackKind::Subtitle, id: 77 };
        let required = Requirements {
            audio_track: TrackChoice::Selected(audio.id),
            subtitle_track: TrackChoice::Selected(subtitle.id), ..REQ
        };
        rig.driver.stage_source(A, KEY, UriSource::new(URL.into(), vec![
            NativeTrackIndex { selection: audio, index: 3 },
            NativeTrackIndex { selection: subtitle, index: 5 },
        ])).unwrap();
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Start { source: KEY, requirements: required }), DispatchResult::Queued);
        rig.bus.say("register", "");
        assert!(rig.driver.poll(&rig.mt).is_empty());
        rig.bus.say("load", r#"{"returnValue":true,"mediaId":"m1","loadCompleted":true}"#);
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Observation { attempt: A,
            observation: Observation::Playback(Observed { value: Playback::ReadyAwaitPlay,
                from: EvidenceSource::BackendReport }) }]);
        assert_eq!(rig.bus.calls("selectTrack"), 0);
        assert_eq!(rig.bus.calls("play"), 0);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Pause),
            DispatchResult::Queued, "a paused initial intent stays in common ownership");
        assert_eq!(rig.bus.calls("play"), 0);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::SelectTrack { kind: TrackKind::Audio, choice: TrackChoice::Selected(audio.id) }),
            DispatchResult::Queued);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::SelectTrack { kind: TrackKind::Subtitle, choice: TrackChoice::Selected(subtitle.id) }),
            DispatchResult::Queued);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Play),
            DispatchResult::Refused(Failure::Busy));
        let (session, audio_request, subtitle_request) = {
            let state = rig.bus.0.lock().unwrap();
            let mut requests = state.calls.iter().filter(|(method, _, _)| *method == "selectTrack");
            (state.session, requests.next().unwrap().1, requests.next().unwrap().1)
        };
        rig.bus.0.lock().unwrap().replies.push_back(UriReply { session,
            request: RequestId::new(999), method: "selectTrack",
            payload: r#"{"returnValue":true,"mediaId":"m1"}"#.into(), ok: true });
        assert!(rig.driver.poll(&rig.mt).is_empty());
        for (request, kind, index, selected) in [
            (audio_request, "audio", 3, audio), (subtitle_request, "text", 5, subtitle)
        ] {
            rig.bus.0.lock().unwrap().replies.push_back(UriReply { session, request,
                method: "selectTrack", payload: r#"{"returnValue":true,"mediaId":"m1"}"#.into(),
                ok: true });
            assert!(rig.driver.poll(&rig.mt).is_empty(), "ACK cannot confirm selection");
            assert_eq!(rig.bus.calls("play"), 0);
            rig.bus.say("subscribe", &format!(
                r#"{{"mediaId":"m1","trackSelected":{{"type":"{kind}","index":{index}}}}}"#));
            assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Observation { attempt: A,
                observation: Observation::TrackSelected(Observed { value: selected,
                    from: EvidenceSource::BackendReport }) },
                DriverEvent::TrackSettled { attempt: A, command: rig.command,
                    kind: selected.kind, choice: TrackChoice::Selected(selected.id) }]);
        }
        assert_eq!(rig.bus.calls("play"), 0);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Play),
            DispatchResult::Queued);
        assert_eq!(rig.bus.calls("play"), 1);
    }

    #[test]
    fn missing_original_mapping_refuses_before_open_and_reducer_returns_to_vacant() {
        let mut rig = Rig::new();
        let required = Requirements { audio_track: TrackChoice::Selected(42), ..REQ };
        rig.driver.stage_source(A, KEY, UriSource::new(URL.into(), Vec::new())).unwrap();
        let mut state = BackendState::new();
        let mut effects = Vec::new();
        state.step(ContractEvent::Intent(Intent::Start {
            backend: BackendKind::Uri, attempt: A, source: KEY, requirements: required,
            initial: INITIAL,
        }), Some(CAPS), &mut effects);
        let [ContractFx::Dispatch { command, operation, .. }] = effects.as_slice() else { panic!("dispatch") };
        let (command, operation) = (*command, *operation);
        let result = rig.driver.dispatch(&rig.mt, A, command, operation);
        assert_eq!(result, DispatchResult::Refused(Failure::Unsupported));
        assert!(rig.driver.staged.is_none(), "failed admission destroys the authorized URL");
        effects.clear();
        state.step(ContractEvent::CommandAnswer { backend: BackendKind::Uri,
            attempt: A, command, result }, None, &mut effects);
        assert_eq!(state.snapshot().phase, plx_playback_contract::Phase::Releasing);
        for notice in rig.driver.poll(&rig.mt) {
            effects.clear();
            state.step(BackendEvent { backend: BackendKind::Uri, event: notice }.into(), None, &mut effects);
        }
        assert_eq!(state.snapshot().phase, plx_playback_contract::Phase::Vacant);
        assert_eq!(rig.bus.0.lock().unwrap().serial, 0, "no native registration was attempted");
    }

    #[test]
    fn refused_track_reports_exact_command_and_waits_for_owner_stop() {
        let mut rig = Rig::new(); rig.opened();
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::SelectTrack { kind: TrackKind::Audio, choice: TrackChoice::Selected(3) }),
            DispatchResult::Queued);
        let reply = {
            let state = rig.bus.0.lock().unwrap();
            let (_, request, _) = state.calls.iter().rev().find(|(method, _, _)| *method == "selectTrack").unwrap();
            UriReply { session: state.session, request: *request,
                method: "selectTrack", payload: "refused".into(), ok: false }
        };
        rig.bus.0.lock().unwrap().replies.push_back(reply);
        let notices = rig.driver.poll(&rig.mt);
        assert!(notices.contains(&DriverEvent::ControlRefused { attempt: A,
            command: rig.command,
            operation: Operation::SelectTrack { kind: TrackKind::Audio,
                choice: TrackChoice::Selected(3) }, failure: Failure::Refused }));
        assert!(!notices.iter().any(|event| matches!(event, DriverEvent::Release { .. })),
            "the common startup owner decides if this was mandatory or interactive");
        assert_eq!(rig.bus.calls("play"), 0);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Stop(StopReason::Replace)), DispatchResult::Queued);
        assert!(rig.driver.poll(&rig.mt).contains(&DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Closing }));
    }

    #[test]
    fn later_pause_refusal_is_correlated_and_does_not_end_playback() {
        let mut rig = Rig::new(); rig.opened();
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Play), DispatchResult::Queued);
        rig.bus.say("play", r#"{"returnValue":true,"mediaId":"m1","playing":true}"#);
        assert!(rig.driver.poll(&rig.mt).contains(&DriverEvent::ControlAccepted { attempt: A,
            command: rig.command, operation: Operation::Play, native_acknowledged: true }));
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Pause), DispatchResult::Queued);
        let (session, old_request) = {
            let state = rig.bus.0.lock().unwrap();
            (state.session, state.calls.iter().rev().find(|(method, _, _)| *method == "pause").unwrap().1)
        };
        rig.bus.0.lock().unwrap().replies.push_back(UriReply { session,
            request: old_request, method: "pause", payload: "refused".into(), ok: false });
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::ControlRefused { attempt: A,
            command: rig.command, operation: Operation::Pause, failure: Failure::Refused }]);
        assert_eq!(rig.driver.exit_disposition(), ExitDisposition::PreserveNativeOwnership);

        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Pause), DispatchResult::Queued);
        rig.bus.0.lock().unwrap().replies.push_back(UriReply { session,
            request: old_request, method: "pause", payload: "refused".into(), ok: false });
        assert!(rig.driver.poll(&rig.mt).is_empty(), "retired native request cannot refuse a retry");
        rig.bus.say("pause", r#"{"returnValue":true,"mediaId":"m1","paused":true}"#);
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::ControlAccepted { attempt: A,
            command: rig.command, operation: Operation::Pause, native_acknowledged: true },
            DriverEvent::Observation { attempt: A, observation: Observation::Playback(Observed {
                value: Playback::Paused, from: EvidenceSource::BackendReport }) }]);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Play), DispatchResult::Queued);
        rig.bus.say("play", r#"{"returnValue":false,"mediaId":"m1"}"#);
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::ControlRefused { attempt: A,
            command: rig.command, operation: Operation::Play, failure: Failure::Refused }]);
        assert_eq!(rig.driver.exit_disposition(), ExitDisposition::PreserveNativeOwnership,
            "a later Play refusal leaves the live attempt available for another viewer press");
    }

    #[test]
    fn seek_landing_requires_exact_request_and_off_is_unsupported() {
        let mut rig = Rig::new(); rig.opened();
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::SelectTrack { kind: TrackKind::Subtitle, choice: TrackChoice::Off }),
            DispatchResult::Refused(Failure::Unsupported));
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Seek(ContentTime::from_millis(5000))), DispatchResult::Queued);
        let (session, request) = {
            let state = rig.bus.0.lock().unwrap();
            (state.session, state.calls.iter().rev().find(|(method, _, _)| *method == "seek").unwrap().1)
        };
        // A report of the clock is not the seek's landing; only a `seekDone` is.
        rig.bus.say("subscribe", r#"{"mediaId":"m1","currentTime":5000}"#);
        assert!(!any_landing(&rig.driver.poll(&rig.mt)));
        rig.bus.0.lock().unwrap().replies.push_back(UriReply { session,
            request: RequestId::new(999), method: "seek",
            payload: r#"{"returnValue":true,"mediaId":"m1","seekDone":true}"#.into(), ok: true });
        assert!(rig.driver.poll(&rig.mt).is_empty());
        rig.bus.0.lock().unwrap().replies.push_back(UriReply { session, request,
            method: "seek", payload: r#"{"returnValue":true,"mediaId":"m1","seekDone":true,"currentTime":5000}"#.into(),
            ok: true });
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::SeekLanded { attempt: A,
            command: rig.command, position: Some(Observed {
                value: ContentTime::from_millis(5000), from: EvidenceSource::BackendReport }) },
            DriverEvent::Observation { attempt: A, observation: Observation::Position(Observed {
                value: ContentTime::from_millis(5000), from: EvidenceSource::BackendReport }) }],
            "a reply to the seek that carries the `seekDone` lands it and reports sparse position");
        assert_eq!(rig.bus.calls("play"), 0, "seeking never chooses playback policy");
    }

    // The 4.5 service's own shapes, from the ls-monitor capture of a bound session
    // (`bound-controls/bus.jsonl`): the seek call is answered at once with the plain method reply,
    // which carries NO `seekDone`; the `seekDone` is the subscription's own event, about 110 ms
    // later, carrying the media id in its object.
    const SEEK_ACK: &str = r#"{"errorCode":0,"returnValue":true,"errorText":"No Error","mediaId":"m1"}"#;
    const SEEK_DONE: &str = r#"{"seekDone":{"mediaId":"m1"}}"#;

    /// An open attempt with one Seek (the startup resume) dispatched and not yet landed.
    fn seeking() -> Rig {
        let mut rig = Rig::new(); rig.opened();
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Seek(ContentTime::from_millis(77_830))), DispatchResult::Queued);
        assert_eq!(rig.bus.calls("seek"), 1);
        rig
    }
    fn landed(rig: &Rig) -> DriverEvent {
        DriverEvent::SeekLanded { attempt: A, command: rig.command, position: None }
    }
    fn any_landing(events: &[DriverEvent]) -> bool {
        events.iter().any(|event| matches!(event, DriverEvent::SeekLanded { .. }))
    }

    #[test]
    fn a_seek_lands_on_the_subscriptions_seekdone_after_the_replys_ack() {
        // The device order: call sent, the reply at once, `seekDone` ~110 ms later.
        let mut rig = seeking();
        rig.bus.say("seek", SEEK_ACK);
        assert!(rig.driver.poll(&rig.mt).is_empty(), "the ack alone is not the landing");
        rig.bus.say("subscribe", SEEK_DONE);
        assert_eq!(rig.driver.poll(&rig.mt), [landed(&rig)]);
        assert_eq!(rig.bus.calls("play"), 0, "seeking never chooses playback policy");
    }

    #[test]
    fn a_seek_lands_on_seekdone_that_beats_the_reply_and_the_late_ack_adds_nothing() {
        let mut rig = seeking();
        rig.bus.say("subscribe", SEEK_DONE);
        assert_eq!(rig.driver.poll(&rig.mt), [landed(&rig)]);
        rig.bus.say("seek", SEEK_ACK);
        assert!(rig.driver.poll(&rig.mt).is_empty(), "the retired request lands nothing twice");
    }

    #[test]
    fn a_seek_lands_on_seekdone_when_the_service_never_replies() {
        let mut rig = seeking();
        rig.bus.say("subscribe", SEEK_DONE);
        assert_eq!(rig.driver.poll(&rig.mt), [landed(&rig)]);
    }

    #[test]
    fn a_refused_seek_is_refused_and_a_later_seekdone_lands_nothing() {
        let mut rig = seeking();
        rig.bus.say("seek", r#"{"errorCode":-1,"returnValue":false,"errorText":"refused","mediaId":"m1"}"#);
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::ControlRefused { attempt: A,
            command: rig.command, operation: Operation::Seek(ContentTime::from_millis(77_830)),
            failure: Failure::Refused }]);
        rig.bus.say("subscribe", SEEK_DONE);
        assert!(!any_landing(&rig.driver.poll(&rig.mt)), "no pending seek is left to land");
    }

    #[test]
    fn a_seekdone_before_any_seek_was_sent_settles_no_later_seek() {
        let mut rig = Rig::new(); rig.opened();
        rig.bus.say("subscribe", SEEK_DONE);
        assert!(!any_landing(&rig.driver.poll(&rig.mt)));
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Seek(ContentTime::from_millis(77_830))), DispatchResult::Queued);
        assert!(!any_landing(&rig.driver.poll(&rig.mt)), "the earlier report is not this seek's");
        rig.bus.say("subscribe", SEEK_DONE);
        assert_eq!(rig.driver.poll(&rig.mt), [landed(&rig)]);
    }

    #[test]
    fn one_seekdone_never_picks_between_two_pending_seeks() {
        let mut rig = seeking();
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Seek(ContentTime::from_millis(90_000))), DispatchResult::Queued);
        assert_eq!(rig.bus.calls("seek"), 2);
        rig.bus.say("subscribe", SEEK_DONE);
        assert!(!any_landing(&rig.driver.poll(&rig.mt)), "which of the two it reports is unknown");
    }

    #[test]
    fn a_seekdone_of_another_media_or_of_none_lands_no_seek() {
        for payload in [r#"{"seekDone":{"mediaId":"other"}}"#, r#"{"seekDone":true}"#,
            r#"{"seekDone":{}}"#] {
            let mut rig = seeking();
            rig.bus.say("subscribe", payload);
            assert!(!any_landing(&rig.driver.poll(&rig.mt)), "{payload}");
        }
    }

    #[test]
    fn acknowledged_track_without_confirmation_times_out_and_waits_for_owner_stop() {
        let mut rig = Rig::new(); rig.opened();
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::SelectTrack { kind: TrackKind::Audio, choice: TrackChoice::Selected(3) }),
            DispatchResult::Queued);
        rig.bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
        assert!(rig.driver.poll(&rig.mt).is_empty());
        rig.clock.store(500, Ordering::Relaxed);
        let notices = rig.driver.poll(&rig.mt);
        assert!(notices.contains(&DriverEvent::ControlRefused { attempt: A,
            command: rig.command,
            operation: Operation::SelectTrack { kind: TrackKind::Audio,
                choice: TrackChoice::Selected(3) }, failure: Failure::TimedOut }));
        assert!(!notices.iter().any(|event| matches!(event, DriverEvent::Release { .. })),
            "the common startup owner decides if this was mandatory or interactive");
        assert_eq!(rig.bus.calls("play"), 0);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Stop(StopReason::Replace)), DispatchResult::Queued);
        assert!(rig.driver.poll(&rig.mt).contains(&DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Closing }));
    }

    #[test]
    fn startup_deadline_stops_at_ready_and_exit_waits_for_real_release() {
        let mut rig = Rig::new();
        assert_eq!(rig.driver.exit_disposition(), ExitDisposition::Clean);
        rig.opened();
        assert_eq!(rig.driver.exit_disposition(), ExitDisposition::PreserveNativeOwnership);
        rig.clock.store(501, Ordering::Relaxed);
        assert!(rig.driver.poll(&rig.mt).is_empty(), "common owner may stay paused at ready");
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Stop(StopReason::Exit)), DispatchResult::Queued);
        assert_eq!(rig.driver.exit_disposition(), ExitDisposition::PreserveNativeOwnership);
        rig.driver.poll(&rig.mt);
        assert_eq!(rig.driver.exit_disposition(), ExitDisposition::Clean);
    }

    // ---- the ACB binding ----

    const ENVELOPE: &str = r#"{"mediaId":"m1","sourceInfo":{"context":"m1","video":{"width":1280,"height":720,"frameRate":24}},"loadCompleted":true}"#;

    impl Rig {
        fn at(&self, ms: u64) { self.clock.store(ms, Ordering::Relaxed); }
        /// Started, registered, loaded, subscribed: the service has not said `loadCompleted` yet.
        fn acb_loading(&mut self) { self.acb_loading_as(A) }
        fn acb_loading_as(&mut self, attempt: AttemptId) {
            assert_eq!(self.start(attempt), DispatchResult::Queued);
            self.bus.say("register", "");
            self.driver.poll(&self.mt);
            self.bus.say("load", r#"{"returnValue":true,"mediaId":"m1"}"#);
            self.driver.poll(&self.mt);
        }
        /// The service says `message` on the subscription and the backend polls it in.
        fn subscription_says(&mut self, message: &str) {
            self.bus.say("subscribe", message);
            self.driver.poll(&self.mt);
        }
        fn bus_methods(&self) -> Vec<&'static str> {
            self.bus.0.lock().unwrap().calls.iter().map(|(name, _, _)| *name).collect()
        }
        /// Run the paced sequence to its end; `loadCompleted` (with the envelope) is at t=0.
        fn acb_bound(&mut self) {
            self.acb_loading();
            self.bus.say("subscribe", ENVELOPE);
            self.driver.poll(&self.mt);
            for t in [200, 400, 600, 900] { self.at(t); self.driver.poll(&self.mt); }
        }
        /// `acb_bound` for `attempt`, with `loadCompleted` at `base` ms.
        fn acb_bound_as(&mut self, attempt: AttemptId, base: u64) {
            self.at(base);
            self.acb_loading_as(attempt);
            self.bus.say("subscribe", ENVELOPE);
            self.driver.poll(&self.mt);
            for t in [200, 400, 600, 900] { self.at(base + t); self.driver.poll(&self.mt); }
        }
        /// The service accepts the first Play and reports `playing`; returns that poll's events.
        fn service_plays(&mut self) -> Vec<DriverEvent> { self.service_plays_as(A) }
        fn service_plays_as(&mut self, attempt: AttemptId) -> Vec<DriverEvent> {
            assert_eq!(self.driver.dispatch(&self.mt, attempt, self.command, Operation::Play), DispatchResult::Queued);
            self.bus.say("play", r#"{"returnValue":true,"mediaId":"m1","playing":true}"#);
            self.driver.poll(&self.mt)
        }
        /// The service reports `currentTime` (ms) on its subscription; returns that poll's events.
        fn clock_says(&mut self, ms: u64) -> Vec<DriverEvent> {
            self.bus.say("subscribe", &format!(r#"{{"mediaId":"m1","currentTime":{ms}}}"#));
            self.driver.poll(&self.mt)
        }
    }

    /// How many `Presented` observations `events` carry.
    fn presented(events: &[DriverEvent]) -> usize {
        events.iter().filter(|event| matches!(event, DriverEvent::Observation {
            observation: Observation::Presented, .. })).count()
    }

    #[test]
    fn acb_presents_once_when_the_service_plays_the_plane_completed_and_the_clock_then_advances() {
        let (mut rig, sink, _) = Rig::acb();
        rig.acb_bound();
        assert_eq!(presented(&rig.service_plays()), 0, "a state report is not a picture");
        // The clock runs, but neither plane completion has arrived: no picture is claimed.
        assert_eq!(presented(&rig.clock_says(1_000)), 0);
        assert_eq!(presented(&rig.clock_says(1_040)), 0);
        // Both completions arrive (the display window's task, then PLAYING's): armed, and the
        // very poll that learns of them cannot also be the advance.
        sink.mailbox.push(3, 1);
        sink.mailbox.push(4, 1);
        assert_eq!(presented(&rig.driver.poll(&rig.mt)), 0);
        assert_eq!(presented(&rig.clock_says(1_040)), 0, "a clock that did not advance proves nothing");
        // The first advance after everything else is the picture, and it is reported once.
        assert_eq!(presented(&rig.clock_says(1_080)), 1);
        assert_eq!(presented(&rig.clock_says(1_120)), 0);
        assert_eq!(presented(&rig.driver.poll(&rig.mt)), 0);
    }

    /// A bound ACB attempt whose service has said `playing`; the plane's completions have not come.
    fn acb_playing() -> (Rig, &'static ScriptedPlaneSink) {
        let (mut rig, sink, _) = Rig::acb();
        rig.acb_bound();
        assert_eq!(presented(&rig.service_plays()), 0, "a state report is not a picture");
        (rig, sink)
    }

    /// The firmware's completion callbacks of the bind's window and PLAYING calls (the ids the
    /// scripted sink answered `plane_finish` with).
    fn plane_tasks() -> (i64, i64) {
        use plx_platform::tv::sink::{SCRIPTED_DISPLAY_TASK, SCRIPTED_PLAYING_TASK};
        (i64::from(SCRIPTED_DISPLAY_TASK), i64::from(SCRIPTED_PLAYING_TASK))
    }

    #[test]
    fn acb_presents_only_when_both_of_the_binds_own_tasks_complete_with_event_1() {
        let (display, playing) = plane_tasks();
        // (case, the completions the firmware delivers, how many pictures are reported). The
        // first two rows are the positive legs: they fail on a tree that has no producer.
        let cases: [(&str, Vec<(i64, i32)>, usize); 9] = [
            ("both complete", vec![(display, 1), (playing, 1)], 1),
            ("both complete, PLAYING first", vec![(playing, 1), (display, 1)], 1),
            ("only the window completes", vec![(display, 1)], 0),
            ("only PLAYING completes", vec![(playing, 1)], 0),
            ("the window times out", vec![(display, -2), (playing, 1)], 0),
            ("PLAYING errors", vec![(display, 1), (playing, -1)], 0),
            ("the window is cancelled", vec![(display, 0), (playing, 1)], 0),
            ("other tasks complete (video data, LOADED)", vec![(1, 1), (2, 1)], 0),
            ("no completion at all", vec![], 0),
        ];
        for (case, completions, expected) in cases {
            let (mut rig, sink) = acb_playing();
            for (task, ev) in completions { sink.mailbox.push(task, ev); }
            assert_eq!(presented(&rig.driver.poll(&rig.mt)), 0, "{case}: learning of them is not the advance");
            let reported: usize = [1_000, 1_040, 1_080, 1_120].into_iter()
                .map(|ms| presented(&rig.clock_says(ms))).sum();
            assert_eq!(reported, expected, "{case}");
        }
    }

    #[test]
    fn acb_a_sample_delivered_with_the_completions_is_not_the_advance_and_a_resume_needs_two() {
        let (display, playing) = plane_tasks();
        // A clock report that shares a poll with the completions was observed before them.
        let (mut rig, sink) = acb_playing();
        sink.mailbox.push(display, 1);
        sink.mailbox.push(playing, 1);
        rig.bus.say("subscribe", r#"{"mediaId":"m1","currentTime":600000}"#);
        assert_eq!(presented(&rig.driver.poll(&rig.mt)), 0);
        assert_eq!(presented(&rig.clock_says(600_000)), 0, "a clock that stands still");
        assert_eq!(presented(&rig.clock_says(600_040)), 1);
        // A resumed start's first report is its resume position: with no sample to compare it
        // with, it is not movement (and the second report, which is, is the picture).
        let (mut rig, sink) = acb_playing();
        sink.mailbox.push(display, 1);
        sink.mailbox.push(playing, 1);
        assert_eq!(presented(&rig.driver.poll(&rig.mt)), 0);
        assert_eq!(presented(&rig.clock_says(600_000)), 0);
        assert_eq!(presented(&rig.clock_says(600_040)), 1);
        assert_eq!(presented(&rig.clock_says(600_080)), 0, "once");
    }

    #[test]
    fn acb_a_seek_that_lands_before_the_picture_is_not_an_advance() {
        let (display, playing) = plane_tasks();
        let (mut rig, sink) = acb_playing();
        assert_eq!(presented(&rig.clock_says(100)), 0);
        sink.mailbox.push(display, 1);
        sink.mailbox.push(playing, 1);
        assert_eq!(presented(&rig.driver.poll(&rig.mt)), 0);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Seek(ContentTime::from_millis(600_000))), DispatchResult::Queued);
        let (session, request) = {
            let state = rig.bus.0.lock().unwrap();
            (state.session, state.calls.iter().rev().find(|(method, _, _)| *method == "seek").unwrap().1)
        };
        rig.bus.0.lock().unwrap().replies.push_back(UriReply { session, request, method: "seek",
            payload: r#"{"returnValue":true,"mediaId":"m1","seekDone":true,"currentTime":600000}"#.into(),
            ok: true });
        let landed = rig.driver.poll(&rig.mt);
        assert!(landed.iter().any(|event| matches!(event, DriverEvent::SeekLanded { .. })), "{landed:?}");
        assert_eq!(presented(&landed), 0, "the jump from 100 to the seek target is not playback");
        assert_eq!(presented(&rig.clock_says(600_040)), 1, "the clock then moves on");
        // The subscription's `seekDone` is what lands a seek (the reply carries none on the real
        // service); while the Seek is pending its target is not movement.
        let (mut rig, sink) = acb_playing();
        assert_eq!(presented(&rig.clock_says(100)), 0);
        sink.mailbox.push(display, 1);
        sink.mailbox.push(playing, 1);
        assert_eq!(presented(&rig.driver.poll(&rig.mt)), 0);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Seek(ContentTime::from_millis(600_000))), DispatchResult::Queued);
        let (session, request) = {
            let state = rig.bus.0.lock().unwrap();
            (state.session, state.calls.iter().rev().find(|(method, _, _)| *method == "seek").unwrap().1)
        };
        rig.bus.say("subscribe", r#"{"mediaId":"m1","seekDone":true,"currentTime":600000}"#);
        let landed = rig.driver.poll(&rig.mt);
        assert!(landed.iter().any(|event| matches!(event, DriverEvent::SeekLanded { .. })), "{landed:?}");
        assert_eq!(presented(&landed), 0, "the subscription's report landing the seek");
        rig.bus.0.lock().unwrap().replies.push_back(UriReply { session, request, method: "seek",
            payload: r#"{"returnValue":true,"mediaId":"m1","seekDone":true,"currentTime":600000}"#.into(),
            ok: true });
        assert_eq!(presented(&rig.driver.poll(&rig.mt)), 0, "the late reply of the retired seek");
        assert_eq!(presented(&rig.clock_says(600_040)), 1);
    }

    #[test]
    fn acb_an_attempt_that_is_closing_claims_no_picture() {
        let (display, playing) = plane_tasks();
        // The same events with and without the Stop: the positive leg is the first. What holds the
        // second is the lifecycle, which delivers no service report once the close has begun (and
        // the plane is no longer asked: the sequence is cancelled), so this also passes if the
        // backend did nothing about it; a new attempt starts the picture rule afresh.
        for (stopped, expected) in [(false, 1), (true, 0)] {
            let (mut rig, sink) = acb_playing();
            assert_eq!(presented(&rig.clock_says(1_000)), 0);
            sink.mailbox.push(display, 1);
            sink.mailbox.push(playing, 1);
            assert_eq!(presented(&rig.driver.poll(&rig.mt)), 0);
            if stopped {
                assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Stop(StopReason::Stop)),
                    DispatchResult::Queued);
            }
            assert_eq!(presented(&rig.clock_says(1_040)), expected, "stopped: {stopped}");
        }
    }

    #[test]
    fn acb_a_successor_attempt_presents_afresh() {
        let (display, playing) = plane_tasks();
        let (mut rig, sink) = acb_playing();
        sink.mailbox.push(display, 1);
        sink.mailbox.push(playing, 1);
        rig.driver.poll(&rig.mt);
        assert_eq!(presented(&rig.clock_says(1_000)), 0);
        assert_eq!(presented(&rig.clock_says(1_040)), 1, "attempt A presents");
        // A is stopped and released (the UNLOADED completion), then B is bound and played.
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Stop(StopReason::Stop)), DispatchResult::Queued);
        rig.at(2_000);
        rig.driver.poll(&rig.mt);
        sink.mailbox.push(5, 1);
        let released = rig.driver.poll(&rig.mt);
        assert!(released.contains(&DriverEvent::Release { attempt: A, status: ReleaseStatus::Released }), "{released:?}");
        rig.acb_bound_as(B, 3_000);
        assert_eq!(presented(&rig.service_plays_as(B)), 0);
        // B's window and PLAYING are new tasks of the same scripted ids; its own completions
        // are what count, and its clock is its own.
        sink.mailbox.push(display, 1);
        sink.mailbox.push(playing, 1);
        assert_eq!(presented(&rig.driver.poll(&rig.mt)), 0);
        assert_eq!(presented(&rig.clock_says(10)), 0);
        let events = rig.clock_says(50);
        assert!(events.contains(&DriverEvent::Observation { attempt: B, observation: Observation::Presented }), "{events:?}");
        assert_eq!(presented(&events), 1);
    }

    #[test]
    fn acb_an_unbound_attempt_never_presents_whatever_the_mailbox_holds() {
        // A guard that passes on a tree without the producer too: without an ACB binding nothing
        // asks the plane owner and nothing is claimed. The positive leg is the tests above.
        let (display, playing) = plane_tasks();
        let sink: &'static ScriptedPlaneSink =
            Box::leak(Box::new(ScriptedPlaneSink::new(PlaneUnload { rv: 1, task: 5 })));
        let plane: &'static plx_platform::tv::plane::PlaneOwner =
            Box::leak(Box::new(plx_platform::tv::plane::PlaneOwner::new()));
        plane.boot(plx_platform::tv::plane::PlaneFacts { handle: true, player_type: 0 });
        let mut rig = Rig::with(Binding::Unbound, sink, plane);
        rig.opened();
        assert_eq!(presented(&rig.service_plays()), 0);
        sink.mailbox.push(display, 1);
        sink.mailbox.push(playing, 1);
        for ms in [1_000, 1_040, 1_080, 1_120] { assert_eq!(presented(&rig.clock_says(ms)), 0, "{ms}"); }
        assert!(sink.calls().is_empty(), "an unbound attempt calls no ACB verb: {:?}", sink.calls());
    }

    #[test]
    fn acb_start_refuses_unless_every_gate_fact_holds() {
        // The URI engine's handle as the boot recorded it; Feed's is irrelevant to this gate.
        let boot = |handle, player_type, initialized| -> &'static plx_platform::tv::plane::PlaneOwner {
            let plane = Box::leak(Box::new(plx_platform::tv::plane::PlaneOwner::new()));
            plane.boot_handle(plx_platform::tv::plane::PlaneHandle::Uri,
                plx_platform::tv::plane::PlaneFacts { handle, player_type },
                plx_platform::tv::plane::HandleSeat { id: 0, initialized });
            plane
        };
        let scripted = || -> &'static ScriptedPlaneSink {
            Box::leak(Box::new(ScriptedPlaneSink::new(PlaneUnload { rv: 1, task: 5 })))
        };
        let cases: [(&str, &'static ScriptedPlaneSink, &'static dyn DisplaySink, &'static plx_platform::tv::plane::PlaneOwner); 4] = {
            let (a, b, c, d) = (scripted(), scripted(), scripted(), scripted());
            [("a created handle is not an initialized one", a, a, boot(true, 0, false)),
             ("the measured sequence initialized player type 0", b, b, boot(true, 10, true)),
             ("no handle", c, c, boot(false, 0, true)),
             ("not the ACB display", d, Box::leak(Box::new(NoSink)), boot(true, 0, true))]
        };
        for (case, sink, display, plane) in cases {
            let mut rig = Rig::with(Binding::Acb, display, plane);
            assert_eq!(rig.start(A), DispatchResult::Refused(Failure::Unsupported), "{case}");
            assert!(rig.bus_methods().is_empty(), "{case}: nothing was opened");
            assert!(sink.calls().is_empty(), "{case}: no verb ran");
            // The refusal leaves the backend startable.
            assert!(rig.driver.exit_disposition() == ExitDisposition::Clean, "{case}");
        }
    }

    #[test]
    fn acb_load_carries_the_dummy_window_and_the_foreground_notice_precedes_subscribe() {
        let (mut rig, _, _) = Rig::acb();
        rig.acb_loading();
        assert_eq!(rig.bus_methods(), ["load", "notifyForeground", "subscribe"]);
        let state = rig.bus.0.lock().unwrap();
        assert!(state.calls[0].2.contains(r#""windowId":"window_id_dummy""#));
        assert!(state.calls[1].2.contains(r#""connectionId":"m1""#));
    }

    #[test]
    fn acb_a_refused_foreground_notice_ends_the_start() {
        let (mut rig, _, _) = Rig::acb();
        rig.acb_loading();
        {
            let mut state = rig.bus.0.lock().unwrap();
            let request = state.calls.iter().rev().find(|(name, _, _)| *name == "notifyForeground").unwrap().1;
            let session = state.session;
            state.replies.push_back(UriReply { session, request, method: "notifyForeground",
                payload: r#"{"returnValue":false}"#.into(), ok: true });
        }
        let events = rig.driver.poll(&rig.mt);
        assert!(events.contains(&DriverEvent::Observation { attempt: A,
            observation: Observation::Failed(Failure::Refused) }));
        assert!(rig.bus_methods().contains(&"unload"), "the session is closing");
    }

    #[test]
    fn acb_binds_in_the_measured_order_and_pace_after_load_completed() {
        let (mut rig, sink, plane) = Rig::acb();
        rig.acb_loading();
        assert!(sink.calls().is_empty(), "nothing binds before the service has loaded the media");
        rig.bus.say("subscribe", ENVELOPE);
        let events = rig.driver.poll(&rig.mt);
        assert!(events.iter().any(|event| matches!(event, DriverEvent::Observation { observation:
            Observation::Playback(Observed { value: Playback::ReadyAwaitPlay, .. }), .. })));
        assert_eq!(sink.calls(), ["plane_media_id m1"]);
        assert_eq!(plane.phase(), plx_platform::tv::plane::Phase::Bound);
        assert!(rig.driver.plane_owned(), "from the owner's grant on, the plane is this attempt's");
        for (t, expect) in [(199, 1), (200, 2), (399, 2), (400, 2), (599, 2), (600, 3), (899, 3), (900, 4)] {
            rig.at(t);
            rig.driver.poll(&rig.mt);
            assert_eq!(sink.calls().len(), expect, "t={t}: {:?}", sink.calls());
        }
        let calls = sink.calls();
        let envelope_len = serde_json::from_str::<serde_json::Value>(ENVELOPE).unwrap()["sourceInfo"].to_string().len();
        assert_eq!(calls, ["plane_media_id m1", "plane_loaded", &format!("plane_send_video_data len={envelope_len}"), "plane_finish"]);
        assert_eq!(rig.bus.calls("play"), 0, "the service's play is the caller's, never the sequence's");
    }

    #[test]
    fn acb_without_the_service_envelope_binds_nothing_and_times_out() {
        let (mut rig, sink, _) = Rig::acb();
        rig.acb_loading();
        rig.bus.say("subscribe", r#"{"mediaId":"m1","videoInfo":{"width":1280,"height":720,"frameRate":24},"loadCompleted":true}"#);
        rig.driver.poll(&rig.mt);
        rig.at(19_999);
        assert!(rig.driver.poll(&rig.mt).is_empty());
        rig.at(20_000);
        assert!(rig.driver.poll(&rig.mt).contains(&DriverEvent::Observation { attempt: A,
            observation: Observation::Failed(Failure::TimedOut) }));
        assert!(sink.calls().is_empty(), "a descriptor is never made up from bare video facts");
        assert!(!rig.driver.plane_owned());
    }

    /// What the 4.5 set's LS2 service sent on the subscription for the measured 720p24 H.264 clip
    /// (2026-11-05), one message each and in this order, with the media id the rig's Load reply
    /// gives (`m1`) in place of the captured one: `mediaId` sits INSIDE each event object, and the
    /// service never sent the ACB `context`+`video` schema.
    const CAPTURED_VIDEO_INFO: &str = r#"{"videoInfo":{"width":1280,"height":720,"aspectRatio":"16:9","frameRate":24,"bitRate":120,"mode3D":"2d","actual3D":"2d","scanType":"progressive","pixelAspectRatio":"1:1","afd":16,"rotation":0,"userData":"userData=(null)","specificRendering":"none","hfr":false,"hdrType":"none","SEI":{"displayPrimariesX0":0,"displayPrimariesX1":0,"displayPrimariesX2":0,"displayPrimariesY0":0,"displayPrimariesY1":0,"displayPrimariesY2":0,"whitePointX":0,"whitePointY":0,"minDisplayMasteringLuminance":0,"maxDisplayMasteringLuminance":0,"maxContentLightLevel":0,"maxPicAverageLightLevel":0},"VUI":{"transferCharacteristics":2,"colorPrimaries":2,"matrixCoeffs":2},"mediaId":"m1"}}"#;
    const CAPTURED_SOURCE_INFO: &str = r#"{"sourceInfo":{"numPrograms":1,"seekable":true,"trickable":true,"startDate":-1,"rotation":0,"programInfo":[{"duration":90000,"numAudioTracks":1,"audioTrackInfo":[{"bitRate":0,"codec":"aac","profile":"lc","level":2,"sampleRate":48,"channels":1,"pid":257,"ctag":-1,"audioType":0,"trackId":-1,"immersive":"none","periodStart":0}],"numVideoTracks":1,"videoTrackInfo":[{"angleNumber":0,"codec":"h264","profile":"high","level":4.1,"frameRate":24,"pid":256,"ctag":-1,"trackId":-1,"orgCodec":"h264","width":1280,"height":720,"periodStart":0}],"numSubtitleTracks":0,"subtitleTrackInfo":""}],"mediaId":"m1"}}"#;
    const CAPTURED_LOAD_COMPLETED: &str = r#"{"loadCompleted":{"mediaId":"m1"}}"#;

    /// The envelope the firmware's own conversion makes of [`CAPTURED_VIDEO_INFO`] (report:
    /// `docs/measurements/uri-acb-envelope-2026-11-05.md`), as compact JSON.
    fn captured_envelope() -> serde_json::Value {
        serde_json::json!({"context":"m1","content":"movie","video":{
            "frameRate":24,"scanType":"VIDEO_PROGRESSIVE","width":1280,"height":720,"bitRate":120,
            "adaptive":false,"path":"network","pixelAspectRatio":{"width":1,"height":1},"afd":16,
            "rotation":0,"data3D":{"originalPattern":"2d","currentPattern":"2d","typeLR":"LR"},
            "hfr":false,"hdrType":"none",
            "mediaSei":{"displayPrimariesX0":0,"displayPrimariesX1":0,"displayPrimariesX2":0,"displayPrimariesY0":0,"displayPrimariesY1":0,"displayPrimariesY2":0,"whitePointX":0,"whitePointY":0,"minDisplayMasteringLuminance":0,"maxDisplayMasteringLuminance":0,"maxContentLightLevel":0,"maxPicAverageLightLevel":0},
            "mediaVui":{"transferCharacteristics":2,"colorPrimaries":2,"matrixCoeffs":2}}})
    }

    #[test]
    fn acb_binds_with_the_envelope_derived_from_the_services_video_info() {
        let (mut rig, sink, plane) = Rig::acb();
        rig.acb_loading();
        for message in [r#"{"audioInfo":{"sampleRate":48,"channels":1,"codec":"aac","mediaId":"m1"}}"#,
            r#"{"bufferRange":{"beginTime":0,"endTime":0,"remainingTime":0,"percent":0,"mediaId":"m1"}}"#,
            CAPTURED_VIDEO_INFO, CAPTURED_SOURCE_INFO] {
            rig.subscription_says(message);
        }
        assert!(sink.calls().is_empty(), "nothing binds before the service has loaded the media");
        rig.subscription_says(CAPTURED_LOAD_COMPLETED);
        assert_eq!(sink.calls(), ["plane_media_id m1"]);
        assert_eq!(plane.phase(), plx_platform::tv::plane::Phase::Bound);
        for t in [200, 400, 600, 900] { rig.at(t); rig.driver.poll(&rig.mt); }
        let len = captured_envelope().to_string().len();
        assert_eq!(sink.calls(), ["plane_media_id m1", "plane_loaded",
            &format!("plane_send_video_data len={len}"), "plane_finish"]);
        assert!(rig.driver.plane_owned());
    }

    impl Rig {
        /// Pace the sequence out from `loadCompleted` at t=0, as `acb_bound` does.
        fn pace_out(&mut self) { for t in [200, 400, 600, 900] { self.at(t); self.driver.poll(&self.mt); } }
        /// The poll at `ms` carries the start's `TimedOut` failure.
        fn assert_times_out_at(&mut self, ms: u64) {
            self.at(ms - 1);
            assert!(self.driver.poll(&self.mt).is_empty());
            self.at(ms);
            assert!(self.driver.poll(&self.mt).contains(&DriverEvent::Observation { attempt: A,
                observation: Observation::Failed(Failure::TimedOut) }));
        }
    }

    #[test]
    fn acb_video_info_after_load_completed_ends_the_wait_and_binds() {
        let (mut rig, sink, _) = Rig::acb();
        rig.acb_loading();
        rig.subscription_says(CAPTURED_LOAD_COMPLETED);
        assert!(sink.calls().is_empty(), "no facts yet: the sequence waits");
        rig.at(5_000);
        rig.subscription_says(CAPTURED_VIDEO_INFO);
        assert_eq!(sink.calls(), ["plane_media_id m1"]);
        for t in [200, 400, 600, 900] { rig.at(5_000 + t); rig.driver.poll(&rig.mt); }
        let len = captured_envelope().to_string().len();
        assert_eq!(sink.calls(), ["plane_media_id m1", "plane_loaded", &format!("plane_send_video_data len={len}"), "plane_finish"]);
    }

    #[test]
    fn acb_load_completed_without_a_usable_video_info_still_times_out_and_sends_nothing() {
        let derived_from = |info: &str| info.to_owned();
        let missing_par = CAPTURED_VIDEO_INFO.replace(r#""pixelAspectRatio":"1:1","#, "");
        let bad_par = CAPTURED_VIDEO_INFO.replace(r#""pixelAspectRatio":"1:1""#, r#""pixelAspectRatio":"16/9""#);
        let unknown_3d = CAPTURED_VIDEO_INFO.replace(r#""mode3D":"2d""#, r#""mode3D":"hologram""#);
        let other_media = CAPTURED_VIDEO_INFO.replace(r#""mediaId":"m1""#, r#""mediaId":"m2""#);
        let no_nested_id = format!(r#"{{"mediaId":"m1",{}"#, &CAPTURED_VIDEO_INFO.replace(r#","mediaId":"m1""#, "")[1..]);
        for (why, video_info) in [("no videoInfo at all", None), ("a missing member", Some(missing_par)),
            ("a malformed pixel aspect ratio", Some(bad_par)), ("an unknown 3D string", Some(unknown_3d)),
            ("another media's facts", Some(other_media)), ("an identity only outside the event", Some(derived_from(&no_nested_id)))] {
            let (mut rig, sink, _) = Rig::acb();
            rig.acb_loading();
            if let Some(video_info) = video_info { rig.subscription_says(&video_info); }
            rig.subscription_says(CAPTURED_LOAD_COMPLETED);
            rig.assert_times_out_at(20_000);
            assert!(sink.calls().is_empty(), "{why}: a descriptor is never made up");
            assert!(!rig.driver.plane_owned(), "{why}");
        }
    }

    #[test]
    fn acb_the_services_own_acb_envelope_is_preferred_to_a_derived_one_in_either_order() {
        let own_len = serde_json::from_str::<serde_json::Value>(ENVELOPE).unwrap()["sourceInfo"].to_string().len();
        // The service's envelope arrives after `videoInfo`: it replaces the derived one.
        let (mut rig, sink, _) = Rig::acb();
        rig.acb_loading();
        rig.subscription_says(CAPTURED_VIDEO_INFO);
        rig.subscription_says(ENVELOPE);
        rig.pace_out();
        assert_eq!(sink.calls()[2], format!("plane_send_video_data len={own_len}"), "{:?}", sink.calls());
        // It arrives first: a later `videoInfo` does not displace it.
        let (mut rig, sink, _) = Rig::acb();
        rig.acb_loading();
        rig.subscription_says(ENVELOPE);
        rig.subscription_says(CAPTURED_VIDEO_INFO);
        rig.pace_out();
        assert_eq!(sink.calls()[2], format!("plane_send_video_data len={own_len}"), "{:?}", sink.calls());
        assert_eq!(sink.calls().len(), 4);
    }

    #[test]
    fn acb_a_later_video_info_replaces_a_derived_envelope_before_the_descriptor_is_sent() {
        let (mut rig, sink, _) = Rig::acb();
        rig.acb_loading();
        rig.subscription_says(&CAPTURED_VIDEO_INFO.replace(r#""rotation":0"#, r#""rotation":90"#));
        rig.subscription_says(CAPTURED_VIDEO_INFO);
        rig.subscription_says(CAPTURED_LOAD_COMPLETED);
        rig.pace_out();
        let len = captured_envelope().to_string().len();
        assert_eq!(sink.calls()[2], format!("plane_send_video_data len={len}"), "{:?}", sink.calls());
    }

    #[test]
    fn acb_a_refused_bind_is_a_typed_failure_that_unloads_nothing() {
        let (mut rig, sink, plane) = Rig::acb();
        // Another backend (Feed, over its own handle) already holds the plane.
        assert_eq!(plane.bind(sink, &rig.mt, plx_platform::tv::plane::PlaneHandle::Feed,
            plx_platform::tv::plane::BindPolicy::Rebind, "other"),
            plx_platform::tv::plane::BindVerdict::Granted);
        rig.acb_loading();
        rig.bus.say("subscribe", ENVELOPE);
        let events = rig.driver.poll(&rig.mt);
        assert!(events.contains(&DriverEvent::Observation { attempt: A,
            observation: Observation::Failed(Failure::Busy) }));
        assert!(!sink.calls().iter().any(|call| call.starts_with("plane_media_id")), "{:?}", sink.calls());
        rig.driver.poll(&rig.mt);
        assert_eq!(sink.unload_count(), 0, "the other binding is not ours to unload");
    }

    #[test]
    fn acb_a_bind_during_a_release_is_retried_on_a_later_tick() {
        let (mut rig, sink, plane) = Rig::acb();
        // Feed's handle held the plane and is being released: the URI engine's bind waits for that
        // handle's UNLOADED.
        plane.bind(sink, &rig.mt, plx_platform::tv::plane::PlaneHandle::Feed,
            plx_platform::tv::plane::BindPolicy::Rebind, "previous");
        plane.unload(sink, &rig.mt);
        rig.acb_loading();
        rig.bus.say("subscribe", ENVELOPE);
        rig.driver.poll(&rig.mt);
        assert!(!sink.calls().iter().any(|call| call.starts_with("plane_media_id")));
        sink.mailbox.push(5, 1);
        rig.at(100);
        rig.driver.poll(&rig.mt);
        assert!(sink.calls().contains(&"plane_media_id m1".to_owned()), "{:?}", sink.calls());
    }

    #[test]
    fn acb_close_cancels_the_sequence_unloads_once_and_releases_on_the_completion() {
        let (mut rig, sink, plane) = Rig::acb();
        rig.acb_loading();
        rig.bus.say("subscribe", ENVELOPE);
        rig.driver.poll(&rig.mt);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Stop(StopReason::Stop)), DispatchResult::Queued);
        // The bus releases; UNLOADED is asked for; the sequence's later steps never run.
        rig.at(1_000);
        let events = rig.driver.poll(&rig.mt);
        assert!(!events.iter().any(|event| matches!(event, DriverEvent::Release { status: ReleaseStatus::Released, .. })));
        assert_eq!(sink.unload_count(), 1);
        assert_eq!(sink.calls(), ["plane_media_id m1", "plane_unload"], "no step runs after UNLOADED");
        assert_eq!(rig.driver.exit_disposition(), ExitDisposition::PreserveNativeOwnership);
        // No completion yet: the backend keeps pumping and does not release.
        rig.at(1_100);
        assert!(!rig.driver.poll(&rig.mt).iter().any(|event| matches!(event, DriverEvent::Release { .. })));
        // The firmware's callback for the UNLOADED task (and another task's, which is no fence).
        sink.mailbox.push(4, 1);
        rig.driver.poll(&rig.mt);
        assert_ne!(plane.phase(), plx_platform::tv::plane::Phase::Released);
        sink.mailbox.push(5, 1);
        let events = rig.driver.poll(&rig.mt);
        assert!(events.contains(&DriverEvent::Release { attempt: A, status: ReleaseStatus::Released }), "{events:?}");
        assert_eq!(plane.phase(), plx_platform::tv::plane::Phase::Released);
        assert_eq!(rig.driver.exit_disposition(), ExitDisposition::Clean);
        assert!(!rig.driver.plane_owned());
    }

    #[test]
    fn acb_a_poll_failure_then_a_bus_release_runs_no_step_after_unloaded() {
        let (mut rig, sink, _) = Rig::acb();
        rig.acb_loading();
        rig.bus.say("subscribe", ENVELOPE);
        rig.driver.poll(&rig.mt);
        assert_eq!(sink.calls(), ["plane_media_id m1"]);
        // A failed poll quarantines the session without any Stop, so `begin_close` never ran and
        // the paced sequence is still alive when the bus reports the release.
        rig.bus.0.lock().unwrap().fail_poll = true;
        rig.at(10);
        rig.driver.poll(&rig.mt);
        {
            let mut bus = rig.bus.0.lock().unwrap();
            bus.fail_poll = false;
            bus.close = CloseStatus::Released;
        }
        rig.at(20);
        rig.driver.poll(&rig.mt);
        assert_eq!(sink.unload_count(), 1);
        for t in [200, 400, 600, 900, 1_500] { rig.at(t); rig.driver.poll(&rig.mt); }
        assert_eq!(sink.calls(), ["plane_media_id m1", "plane_unload"],
            "no display step may follow UNLOADED, however the close began");
    }

    #[test]
    fn acb_close_without_a_completion_quarantines_at_the_owners_deadline() {
        let (mut rig, sink, plane) = Rig::acb();
        rig.acb_loading();
        rig.bus.say("subscribe", ENVELOPE);
        rig.driver.poll(&rig.mt);
        rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Stop(StopReason::Stop));
        rig.driver.poll(&rig.mt);
        assert_eq!(plane.phase(), plx_platform::tv::plane::Phase::Releasing);
        // The owner measures its deadline on its own monotonic clock.
        std::thread::sleep(std::time::Duration::from_millis(plx_platform::tv::plane::RELEASE_FENCE_MS + 100));
        let events = rig.driver.poll(&rig.mt);
        assert!(events.contains(&DriverEvent::Release { attempt: A, status: ReleaseStatus::Quarantined }), "{events:?}");
        assert_eq!(plane.phase(), plx_platform::tv::plane::Phase::Quarantined(plx_platform::tv::plane::Why::Deadline));
        assert_eq!(rig.driver.exit_disposition(), ExitDisposition::PreserveNativeOwnership);
        assert_eq!(sink.unload_count(), 1);
    }

    #[test]
    fn acb_pause_and_resume_mirror_to_the_plane_once_bound() {
        let (mut rig, sink, _) = Rig::acb();
        rig.acb_bound();
        assert_eq!(sink.calls().last().map(String::as_str), Some("plane_finish"));
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Pause), DispatchResult::Queued);
        rig.bus.say("pause", r#"{"returnValue":true,"mediaId":"m1","paused":true}"#);
        rig.driver.poll(&rig.mt);
        assert_eq!(sink.calls().last().map(String::as_str), Some("plane_pause"));
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Play), DispatchResult::Queued);
        rig.bus.say("play", r#"{"returnValue":true,"mediaId":"m1"}"#);
        rig.driver.poll(&rig.mt);
        assert_eq!(sink.calls().last().map(String::as_str), Some("plane_resume"));
    }

    #[test]
    fn acb_the_first_play_is_not_mirrored_and_a_pause_before_the_bind_applies_when_it_completes() {
        let (mut rig, sink, _) = Rig::acb();
        rig.acb_loading();
        rig.bus.say("subscribe", ENVELOPE);
        rig.driver.poll(&rig.mt);
        // The service's first Play and an early Pause both land while the sequence is running.
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Play), DispatchResult::Queued);
        rig.bus.say("play", r#"{"returnValue":true,"mediaId":"m1"}"#);
        rig.driver.poll(&rig.mt);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Pause), DispatchResult::Queued);
        rig.bus.say("pause", r#"{"returnValue":true,"mediaId":"m1","paused":true}"#);
        rig.driver.poll(&rig.mt);
        assert!(!sink.calls().iter().any(|call| call == "plane_pause" || call == "plane_resume"), "{:?}", sink.calls());
        for t in [200, 400, 600, 900] { rig.at(t); rig.driver.poll(&rig.mt); }
        assert_eq!(sink.calls().last().map(String::as_str), Some("plane_pause"), "{:?}", sink.calls());
    }

    // ---- the exported window (webOS 5+): placed as Feed and the probe place it ----

    /// The full canvas, as the tests spell it: the destination of every placement.
    const FULL: (std::os::raw::c_int, std::os::raw::c_int) = (1920, 1080);
    /// A placement with `src` as the source rectangle over the whole canvas.
    fn placed(src: (std::os::raw::c_int, std::os::raw::c_int)) -> WindowCall {
        WindowCall::Place { src, dst: (0, 0, FULL.0, FULL.1) }
    }
    /// The captured `videoInfo` with another coded size.
    fn video_info_of(width: u32, height: u32) -> String {
        CAPTURED_VIDEO_INFO.replace(r#""width":1280,"height":720"#, &format!(r#""width":{width},"height":{height}"#))
    }

    impl Rig {
        /// An exported-window backend (`Binding::Exported` on the full canvas, the display in
        /// `VP_EXPORTED`) over a sink that records every window verb and accepts every placement.
        fn exported() -> (Self, &'static ScriptedWindowSink) {
            let sink: &'static ScriptedWindowSink = Box::leak(Box::new(ScriptedWindowSink::new(1)));
            let plane = Box::leak(Box::new(plx_platform::tv::plane::PlaneOwner::new()));
            (Self::with(Binding::Exported { canvas: FULL }, sink, plane), sink)
        }
    }

    #[test]
    fn an_exported_start_creates_one_window_and_places_the_canvas_as_its_source() {
        let (mut rig, sink) = Rig::exported();
        rig.opened();
        assert_eq!(sink.calls(), [WindowCall::Create, placed(FULL)]);
        let load = rig.bus.0.lock().unwrap().calls.iter().find(|(method, _, _)| *method == "load")
            .map(|(_, _, payload)| payload.clone()).expect("the load was sent");
        assert!(load.contains("scripted-window"), "the window id rides the load: {load}");
    }

    #[test]
    fn a_video_info_re_places_the_window_with_the_coded_size_and_the_destination_stays_the_canvas() {
        plx_base::eventlog::with_private_log(|| {
            let (mut rig, sink) = Rig::exported();
            rig.opened();
            rig.subscription_says(CAPTURED_VIDEO_INFO);
            assert_eq!(sink.calls(), [WindowCall::Create, placed(FULL), placed((1280, 720))]);
            // A coded size that did not change places nothing again; a new one places again.
            rig.subscription_says(CAPTURED_VIDEO_INFO);
            assert_eq!(sink.calls().len(), 3, "the same coded size is not placed twice");
            rig.subscription_says(&video_info_of(3840, 2160));
            assert_eq!(sink.calls(), [WindowCall::Create, placed(FULL), placed((1280, 720)), placed((3840, 2160))]);
            let log = std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default();
            assert!(log.contains("uri: exported window placed src=1920x1080 rv=1"), "{log}");
            assert!(log.contains("uri: exported window placed src=1280x720 rv=1"), "{log}");
            assert!(log.contains("uri: exported window placed src=3840x2160 rv=1"), "{log}");
        });
    }

    #[test]
    fn a_video_info_with_no_size_places_nothing() {
        let (mut rig, sink) = Rig::exported();
        rig.opened();
        for info in [r#"{"videoInfo":{"frameRate":24,"mediaId":"m1"}}"#,
            r#"{"videoInfo":{"width":0,"height":720,"mediaId":"m1"}}"#,
            r#"{"videoInfo":{"width":1280,"mediaId":"m1"}}"#] {
            rig.subscription_says(info);
        }
        assert_eq!(sink.calls(), [WindowCall::Create, placed(FULL)]);
    }

    #[test]
    fn the_window_is_never_re_placed_while_it_closes_or_after_it_is_destroyed_and_is_destroyed_once() {
        let (mut rig, sink) = Rig::exported();
        rig.opened();
        rig.subscription_says(CAPTURED_VIDEO_INFO);
        assert_eq!(sink.calls(), [WindowCall::Create, placed(FULL), placed((1280, 720))]);
        let before = sink.calls();
        rig.bus.0.lock().unwrap().hold = true;
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Stop(StopReason::Stop)),
            DispatchResult::Queued);
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Release { attempt: A, status: ReleaseStatus::Closing }]);
        rig.subscription_says(&video_info_of(3840, 2160));
        assert_eq!(sink.calls(), before, "no placement while the close runs");
        // The close is not confirmed in time: quarantined, the window still held and never destroyed.
        rig.at(101);
        assert!(rig.driver.poll(&rig.mt).contains(&DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Quarantined }));
        rig.subscription_says(&video_info_of(3840, 2160));
        assert_eq!(sink.calls(), before, "no placement while quarantined");
        assert_eq!(sink.destroys(), 0, "a quarantined window is not destroyed");
        // The release the hub confirms late destroys it, exactly once.
        rig.bus.0.lock().unwrap().close = CloseStatus::Released;
        assert!(rig.driver.poll(&rig.mt).contains(&DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Released }));
        assert_eq!(sink.destroys(), 1);
        rig.subscription_says(&video_info_of(3840, 2160));
        rig.driver.poll(&rig.mt);
        assert_eq!(sink.calls(), [before, vec![WindowCall::Destroy]].concat(), "nothing after the destroy");
    }

    #[test]
    fn only_the_exported_binding_creates_or_places_a_window() {
        let sink: &'static ScriptedWindowSink = Box::leak(Box::new(ScriptedWindowSink::new(1)));
        let plane = Box::leak(Box::new(plx_platform::tv::plane::PlaneOwner::new()));
        let mut rig = Rig::with(Binding::Unbound, sink, plane);
        rig.opened();
        rig.subscription_says(CAPTURED_VIDEO_INFO);
        assert!(sink.calls().is_empty(), "{:?}", sink.calls());
    }
}
