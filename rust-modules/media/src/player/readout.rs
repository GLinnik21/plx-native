//! The player's own readout: the members only the player writes, and the last facts the feed
//! sampled, as one `Copy` value with one writer.
//!
//! They lived in the feed's process-global mailbox (`FEED_SHARED`: `pb_state`, `ended`,
//! `start_queued`, `speed_unsupported`, `rate_reload`, `play_rate_permille`), although the feed
//! never wrote any of them. They moved here (D2 v2, 1.2 and 3.3) so a reader asks the session,
//! and a test and a replay drive the same events the product does. A writer is an event; the
//! session applies it through [`PlayerReadout::step`] and nothing else can change a field, and the
//! mailbox statics are gone.
//!
//! The machine is pure: no clock, no registry, no thread. It names no feed type; the facts it
//! holds are the contract's [`FeedRaw`], so it builds whatever backends a composition installs.
//! It has no effects of its own: an intent that also yields a feed command (the seek family and the
//! transport: Pause, the paused-seek preroll, the owed re-pause; audio is L3b's) is one event here and one `FeedCommand` the caller sends, so the two
//! halves never name the same member twice. The pump feeds it: the session's one ingestion
//! (`ingest_feed_facts`) takes the raw facts, and `present` derives the state and the end of
//! stream at the pump's derivation point, both addressed by attempt. `state` and `ended` are read
//! from here alone (`player::state`, `player::ended`), and a `SessionReset` at every real call of
//! the Feed's session reset retires them.
//!
//! The speed members follow their writers. `play_rate_permille` is the OFFERED rate: what the next
//! Load plays at, which `session_rate_for` lowers to 1x when this playback does not offer the
//! viewer's pick. It is not the viewer's intent, which is the session's `requested_rate_permille`
//! and survives that downgrade. `rate_reload` is the one-shot the pump takes with the seek that
//! carries a speed change (read it, then step `RateReloadTaken`). `start_queued` is the release
//! machine's answer, stepped by `PlayerAdapter::release_step` after every step of the machine.

#![deny(clippy::wildcard_enum_match_arm)]

use plx_playback_contract::adaptive::{AbrReadout, CarriedSeed, FailureEdit, SeedReadout};
use plx_playback_contract::feed::{FeedRaw, PresentationTick, Stage};
use plx_playback_contract::subtitle::Spec;
use plx_playback_contract::RATE_NORMAL;
use std::sync::Arc;

use super::attempt_view::PlaybackState;

/// Everything the player knows about the playback on screen that the feed does not own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerReadout {
    /// The pump's derived UI state, published once a frame.
    state: PlaybackState,
    /// The pipeline drained to its end of stream and the pump has seen it.
    ended: bool,
    /// A native start is queued behind the previous object's still-running `Unload`.
    start_queued: bool,
    /// This television did not take a speed session's PCM; latched until the app restarts.
    speed_unsupported: bool,
    /// The next direct-play seek must be a full reload, because it carries a speed change.
    rate_reload: bool,
    /// The viewer's playback speed in thousandths (1000 = 1x).
    play_rate_permille: i32,
    /// The feed's raw observations from the last ingestion.
    raw: FeedRaw,
    /// How many seeks this session has asked for: the epoch of the newest request. Monotonic for
    /// the life of the process; a reset retires the requests it numbered ([`Self::seek_settled`]).
    seek_epoch: u32,
    /// Requests up to this epoch are over without the feed's say: a session reset ended them with
    /// the playback they belonged to, or the player gave up on them.
    seek_settled: u32,
    /// Where the seek that waits for the pump is aimed: the newest request's target, or the one a
    /// refused native `Seek` asked to redo. The pump takes it ([`ReadoutEvent::SeekTaken`]); the
    /// feed keeps its own copy of it to gate feeding, written by the matching commands only.
    pending_seek_ns: Option<i64>,
    /// How many requests the pending target folded since the pump last took one: what the
    /// `coalesced=` log lines report (a burst of N taps applies one seek and reports N - 1).
    seek_requests: u32,
    /// The playhead the HUD freezes at while a seek resolves: the newest request's display target,
    /// until the player gives up on it or the session resets.
    seek_display_ns: Option<i64>,
    /// Target retained while a seek's replacement Load has yet to report its own position.
    reload_seek_ns: Option<i64>,
    /// The viewer's transport is Paused: the last `CommitPaused` since the playback began. A reload
    /// keeps it (the new Engine arms its hold from it); a real stop clears it. The feed keeps its
    /// own copy to gate feeding, written by the matching commands only.
    paused: bool,
    /// The viewer is still logically paused while a seek is allowed to decode one frame and then
    /// re-pause (the paused-seek preroll). A reload keeps it; a real stop and its close clear it.
    seek_preroll: bool,
    /// The foreground restore of a paused film owes a re-pause the instant its resume frame is on
    /// screen. Cleared by whatever closes the preroll and by a real stop; a reload keeps it.
    resume_pend: bool,
    /// How many of the feed's decoder revocations ([`FeedRaw::decoder_revocations`]) the app loop
    /// has taken; the revocation is pending while the feed's count differs from it.
    taken_decoder_revocations: u32,
    /// How many of the feed's native-audio refusals ([`FeedRaw::native_audio_refusals`]) the pump
    /// has taken.
    taken_native_audio_refusals: u32,
    /// How many of the side reader's give-ups ([`FeedRaw::side_failures`]) the pump has taken.
    taken_side_failures: u32,
    /// The side reader the route wants and the pump has not yet handed the feed
    /// ([`ReadoutEvent::SideReaderWanted`]): `None` = nothing waits, `Some(None)` = stop the reader.
    side_reader_want: Option<Option<Arc<Spec>>>,
    /// The audio stream the viewer's pick or the route's plan chose for the demuxer: its 0-based
    /// index, `-1` for the file's own default. The player's own choice, not a feed fact: a session
    /// reset and a new item leave it alone (a retry resets the track selection too, and a reload
    /// keeps the pick), and only a track write changes it ([`ReadoutEvent::AudioSelected`]). The
    /// feed's copy, which its demuxer reads, is written by `FeedCommand::SelectAudio` from this one
    /// before every start.
    audio_choice: i32,
    /// The display base the player last commanded the feed to open with ([`ReadoutEvent::OpenRebased`]):
    /// what a transcode restart's `Rebase` carried, until the feed's session reset zeroed it (so does
    /// [`ReadoutEvent::SessionReset`]). A start's side subtitle reader is captured before the attempt
    /// exists to sample the feed's own `disp_base`, and the readout is idle then.
    open_disp_base_ns: i64,
    /// The adaptive worker's last photograph ([`ReadoutEvent::Abr`]): the Auto-quality
    /// measurements and decisions the diagnostics show. The worker is its only writer.
    abr: AbrReadout,
    /// **The HLS worker's last delivery estimate, carried to the next controller** (I8), with the
    /// frame-tick stamp (`PlaybackSession::now_ms` units) it was observed at
    /// ([`ReadoutEvent::AbrSeed`]). It outlives a session reset (a reload is the same item on the
    /// same link) and dies with the playback (`AbrSeed(None)`).
    abr_seed: Option<(SeedReadout, u32)>,
    /// Why the last Original source experiment or open failed: a `player::ABR_FAILURE_ORIGINAL_*`
    /// code and the HTTP status when one was observed ([`ReadoutEvent::OriginalFailure`]). The
    /// playback's, not the engine's: it outlives a session reset, because the rollback reload that
    /// restores HLS must not erase the only fact explaining why Original is no longer attempted,
    /// and it dies with the playback (a real stop clears it) or when a new source attempt or probe
    /// supersedes it. Three writers, one owner: the player's pump and route (events stepped here)
    /// and the adaptive worker (its edits ride the open's channel and are stepped on the drain).
    original_failure: Option<(u8, i32)>,
    /// **Does the viewer's colour pick reach the selected ASS/SSA subtitle?** The fact the media side
    /// read from the subtitle registries (`sidecar`, `ass_source`) and delivered once a tick
    /// ([`ReadoutEvent::SubtitleInk`], by `player::deliver_subtitle_ink`), so the Subtitles menu reads
    /// a field of the session and never the registries. `None` while the file's header is not known
    /// (or no ASS/SSA track is selected). Not a feed fact and not reset by a session reset: the
    /// registries it was read from outlive an engine, and the next tick delivers it afresh.
    subtitle_takes_colour: Option<bool>,
}

/// A one-shot fact the feed counts and the player takes: the feed keeps a monotonic count of the
/// edge (zeroed by its own session reset), the readout holds how many it has taken, and the edge
/// is pending while the two differ. A count of 0 is never an edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// The pipeline took the video decoder back from an established session.
    DecoderRevoked,
    /// The pipeline refused natively fed audio (the callback, or the stall watchdog).
    NativeAudioRefused,
    /// The side subtitle reader gave up (and no stop has voided it).
    SideReaderFailed,
}

/// How close to the container's duration the playhead must be, after the EOS, to count as the end.
pub(super) const EOS_TAIL_NS: i64 = 1_000_000_000;

/// Whether a stream whose EOS has been pushed has ended: the pipeline said so
/// (`Shared::pipeline_eos`), or the playhead is within [`EOS_TAIL_NS`] of the duration. The
/// duration is the longest track's, so a film whose audio outlasts its video by more than the
/// tail never gets there on the arithmetic alone.
pub(super) fn stream_ended(pos_ns: i64, duration_ns: i64, pipeline_eos: bool) -> bool {
    pipeline_eos || (duration_ns > 0 && pos_ns >= duration_ns - EOS_TAIL_NS)
}

/// What one pump frame hands the derivation of the UI state and the end of stream: the physical
/// tick, the two sampled facts that have no [`FeedRaw`] field (a runtime rebuffer, the container's
/// duration), and the two inputs that are the player's own registries, not the readout's (a claim
/// hold and a route flight). A seek in flight, the pipeline's own end of stream, the viewer's Pause
/// with its preroll and a seek that waits for the pump are the readout's: the requested epoch
/// against the landed one, the ingested raw facts, and the transport events stepped at the calls
/// that command the feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Presentation {
    pub tick: PresentationTick,
    /// The HLS runtime rebuffer, distinct from the initial Load and from seek priming.
    pub rebuffering: bool,
    /// The container's duration, 0 while unknown.
    pub duration_ns: i64,
    /// No claim hold is up and no route flight is out: the two ingredients of "the pump is doing no
    /// work for a picture" that are not the readout's. The readout combines them with its own
    /// Pause (no preroll open) and its own pending seek, and asks after taking the frame's sample in.
    pub nothing_in_flight: bool,
}

/// What can happen to the readout. Intent-named: a writer states what occurred, never a field
/// value to store, so a transition that touches several members is one event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadoutEvent {
    /// The pump derived this frame's state.
    StateDerived(PlaybackState),
    /// The pipeline drained to its end of stream.
    StreamEnded,
    /// A seek was requested: a drained stream is no longer the end, a seek is in flight until the
    /// feed reports this request's epoch landed, `target_ns` is the pending target (overwriting an
    /// earlier one, which is the coalescing) and `display_ns` the playhead the HUD freezes at. The
    /// caller sends the matching `FeedCommand::Seek` carrying [`PlayerReadout::seek_epoch`] read
    /// after this event.
    SeekRequested { target_ns: i64, display_ns: i64 },
    /// The player gave up on the seek in flight (every request so far): the spinner and the frozen
    /// playhead end without a landing. The pending target is left to the pump, as it always was.
    /// The caller sends `FeedCommand::AbandonSeek`.
    SeekAbandoned,
    /// The pump took the pending seek (applying it, flying it, or reloading at it): nothing
    /// waits any more and the requests it folded are spent. Read [`PlayerReadout::pending_seek`]
    /// and [`PlayerReadout::seek_coalesced`] first. The caller sends `FeedCommand::SeekTaken`, so
    /// the feed's copy goes with this one in the same call.
    SeekTaken,
    /// A taken seek was put back (the clock transition was busy, or a newer tap outlived a reload):
    /// `target_ns` waits again, as a single request. Not a request: the epoch, the in-flight seek
    /// and the display target are as they were. The caller sends `FeedCommand::SeekRequeued`. The
    /// feed's own request to redo a refused native `Seek` is not this event: it arrives in the
    /// ingested raw facts ([`ReadoutEvent::FactsIngested`]).
    SeekRequeued { target_ns: i64 },
    /// A seek moved to a fresh native Load after the previous session reset.
    ReloadSeek { target_ns: i64 },
    /// One session's facts are gone (its engine was torn down): back to Idle, nothing ended, no
    /// raw facts. The viewer's speed, the speed latch and the release queue outlive a session.
    SessionReset,
    /// A new item: the speed is a choice about what is playing, so it returns to 1x.
    NewItem,
    /// The release machine's queue changed: whether a start waits behind an `Unload`.
    ReleaseQueue { queued: bool },
    /// The viewer picked a speed, or the session's offer forced it to another.
    RateSelected { permille: i32 },
    /// The next direct-play seek is to carry a speed change as a full reload.
    RateReloadArmed,
    /// The pump took that one-shot reload. Read [`PlayerReadout::rate_reload`] first.
    RateReloadTaken,
    /// The television refused a speed session's PCM: speed is off for the life of the process, the
    /// viewer is back at 1x, and the reload that lands there is armed. One decision, one event.
    SpeedRefused,
    /// The feed's raw observations, sampled for the live attempt.
    FactsIngested(FeedRaw),
    /// The pump presented a frame: derive the UI state, and the end of stream, from the facts last
    /// ingested and what the pump saw. The session steps it only for the attempt that is live
    /// and only after the post-tick sample, so a frame that returned early derives nothing.
    Presented(Presentation),
    /// The viewer's transport was committed to `paused` (the feed's `FeedCommand::CommitPaused`, or
    /// the commit its own user Pause and Resume run). A reload does not step it.
    CommitPaused { paused: bool },
    /// The one-frame paused-seek preroll was opened: by a caller (a restore about to Load, the
    /// dispatcher of a seek under the viewer's Pause) or by the feed itself
    /// ([`PausedSeekStep::preroll_opened`](plx_playback_contract::feed::PausedSeekStep)). Opens
    /// nothing unless the viewer is paused, exactly as the feed's own open.
    BeginPausedSeek,
    /// The preroll was closed (the accepted re-pause, a Resume, an abandoned seek, a failed Load):
    /// the foreground restore's re-pause is no longer owed either.
    FinishSeekPreroll,
    /// The foreground restore of a paused film armed its re-pause for the instant the resume frame
    /// is on screen.
    ResumePending,
    /// A REAL stop (not a reload): the playback and its transport are gone, so the Pause, the
    /// preroll and the owed re-pause go with it. [`ReadoutEvent::SessionReset`] fires on reloads
    /// too, which keep all three.
    TransportStopped,
    /// The player took the pending `edge`: it is spent until the feed counts another one. Read
    /// [`PlayerReadout::decoder_revoked`] or [`PlayerReadout::native_audio_refused`] first.
    Taken { edge: Edge },
    /// The route chose the audio stream the demuxer feeds: the index, `-1` for the file's default.
    /// The player sends `FeedCommand::SelectAudio` from [`PlayerReadout::audio_choice`] before the
    /// start that spawns the demuxer.
    AudioSelected { idx: i32 },
    /// The route wants the side subtitle reader to match `want` (`None` = stopped): replaces any
    /// request the pump has not yet taken, the newest wins. The pump hands it to the feed as
    /// `FeedCommand::SyncSideReader` and sends [`Self::SideReaderWantTaken`].
    SideReaderWanted(Option<Arc<Spec>>),
    /// The pump handed the wanted side reader to the feed (read [`PlayerReadout::side_reader_want`] first).
    SideReaderWantTaken,
    /// A transcode restart's `FeedCommand::Rebase` was sent: the encode is 0-based at `disp_base`, which
    /// the feed carries as its display base from the first frame. Appended with the command, before the
    /// start that follows it.
    OpenRebased { disp_base: i64 },
    /// The adaptive worker's photograph, taken in for the attempt that is live (the session checks
    /// the address). A session reset returns it to [`AbrReadout::IDLE`].
    Abr(AbrReadout),
    /// The worker's carried link estimate, observed at the frame-tick stamp `observed_at_ms`
    /// (`None`: the worker published none, or the playback is over and the estimate goes with it).
    /// Unlike the feed's facts it survives [`ReadoutEvent::SessionReset`]: a reload keeps the link
    /// it measured (as it keeps the Original failure, the transport pause and the audio pick).
    AbrSeed(Option<(SeedReadout, u32)>),
    /// The Original failure was noted or cleared (see [`PlayerReadout::original_failure`]). Like
    /// [`ReadoutEvent::AbrSeed`] it survives [`ReadoutEvent::SessionReset`], so a reload keeps it;
    /// a REAL stop steps `Cleared` itself, at the end of the teardown.
    OriginalFailure(FailureEdit),
    /// The media side's reading of whether the colour pick reaches the selected ASS/SSA subtitle
    /// ([`PlayerReadout::subtitle_takes_colour`]); `None`: not known.
    SubtitleInk(Option<bool>),
}

impl PlayerReadout {
    /// No playback: what a session holds before the first play.
    pub const IDLE: Self = Self {
        state: PlaybackState::Idle,
        ended: false,
        start_queued: false,
        speed_unsupported: false,
        rate_reload: false,
        play_rate_permille: RATE_NORMAL,
        raw: FeedRaw::IDLE,
        seek_epoch: 0,
        seek_settled: 0,
        pending_seek_ns: None,
        seek_requests: 0,
        seek_display_ns: None,
        reload_seek_ns: None,
        paused: false,
        seek_preroll: false,
        resume_pend: false,
        taken_decoder_revocations: 0,
        taken_native_audio_refusals: 0,
        taken_side_failures: 0,
        side_reader_want: None,
        audio_choice: -1,
        open_disp_base_ns: 0,
        abr: AbrReadout::IDLE,
        abr_seed: None,
        original_failure: None,
        subtitle_takes_colour: None,
    };

    /// Does the colour pick reach the selected ASS/SSA subtitle (`None`: the header is not known yet)?
    pub fn subtitle_takes_colour(&self) -> Option<bool> { self.subtitle_takes_colour }
    pub fn state(&self) -> PlaybackState { self.state }
    pub fn ended(&self) -> bool { self.ended }
    pub fn start_queued(&self) -> bool { self.start_queued }
    pub fn speed_unsupported(&self) -> bool { self.speed_unsupported }
    pub fn rate_reload(&self) -> bool { self.rate_reload }
    pub fn play_rate_permille(&self) -> i32 { self.play_rate_permille }
    pub fn raw(&self) -> &FeedRaw { &self.raw }
    /// The side reader gave up and nobody has taken that yet.
    pub fn side_reader_failed(&self) -> bool {
        edge_pending(self.raw.side_failures, self.taken_side_failures)
    }
    /// The side reader the route wants, not yet handed the feed: `Some(Some(spec))` = run a reader
    /// on `spec`, `Some(None)` = stop it, `None` = nothing waits.
    pub fn side_reader_want(&self) -> Option<&Option<Arc<Spec>>> { self.side_reader_want.as_ref() }
    /// The adaptive worker's last photograph ([`AbrReadout::IDLE`] before one and after a reset).
    pub fn abr(&self) -> &AbrReadout { &self.abr }
    /// Put the two members of the worker's photograph that an attempt's facts carry (the last
    /// segment's media rate and the reserve, which the error context classes) into `facts`.
    pub fn fill_abr_facts(&self, facts: &mut plx_playback_contract::readout::AttemptFacts) {
        facts.media_kbps = (self.abr.media_kbps > 0).then_some(self.abr.media_kbps);
        facts.abr_buffer_ms = (self.abr.buffer_ms > 0).then_some(self.abr.buffer_ms);
    }
    /// The carried link estimate with the milliseconds since it was observed, as the route takes
    /// it ([`PlaybackSession::observe_link_seed`](crate::route::PlaybackSession::observe_link_seed)).
    pub fn abr_seed(&self, now_ms: u32) -> Option<CarriedSeed> {
        self.abr_seed.map(|(seed, observed_at_ms)| (seed, u64::from(now_ms.wrapping_sub(observed_at_ms))))
    }
    /// Why the last Original source experiment or open failed: the `player::ABR_FAILURE_ORIGINAL_*`
    /// code and the HTTP status (`0` when none was observed), `None` when nothing failed or the
    /// failure was cleared.
    pub fn original_failure(&self) -> Option<(u8, i32)> { self.original_failure }
    /// The audio stream the demuxer is to feed (`-1`: the file's own default).
    pub fn audio_choice(&self) -> i32 { self.audio_choice }
    /// The display base the player last commanded the feed to open with (0 after any session reset).
    pub fn open_disp_base_ns(&self) -> i64 { self.open_disp_base_ns }
    /// The epoch of the newest requested seek (the count of requests), 0 before any.
    pub fn seek_epoch(&self) -> u32 { self.seek_epoch }
    /// A seek is in flight: one was requested that neither landed (the feed's last ingested
    /// `seek_landed_epoch`), nor was given up on, nor ended with a session reset. It is derived,
    /// not stored, so no writer can leave it armed: this is the wedge `abandon_seek` exists for.
    pub fn seeking(&self) -> bool {
        self.seek_epoch > self.seek_settled.max(self.raw.seek_landed_epoch)
    }
    /// The target of the seek that waits for the pump, if one does (never negative).
    pub fn pending_seek(&self) -> Option<i64> { self.pending_seek_ns }
    /// Until the active seek lands, only its requested target can be saved on stop.
    pub fn unconfirmed_seek_target(&self) -> Option<i64> {
        self.pending_seek_ns
            .or_else(|| self.seek_display_ns.filter(|_| self.seeking()))
            .or(self.reload_seek_ns)
    }
    /// How many requests the pending target folded beyond the first: what the pump logs as
    /// `coalesced=` when it applies it. Read it before [`ReadoutEvent::SeekTaken`].
    pub fn seek_coalesced(&self) -> u32 { self.seek_requests.saturating_sub(1) }
    /// The playhead the HUD freezes at while a seek resolves, until the seek is given up on or the
    /// session resets.
    pub fn seek_display_ns(&self) -> Option<i64> { self.seek_display_ns }
    /// The viewer's transport is Paused (a claim hold's loan included: see `transport::viewer_paused`).
    pub fn paused(&self) -> bool { self.paused }
    /// A paused seek is allowed to decode one frame before the re-pause.
    pub fn seek_preroll(&self) -> bool { self.seek_preroll }
    /// Feeding is an actuator concern, not the viewer's pause intent: the feed gate is open unless
    /// the viewer is Paused, and a paused seek's preroll is the one bounded exception (it must
    /// decode a landed frame under a Pause). Derived from [`Self::paused`] and
    /// [`Self::seek_preroll`], the same two members the feed's own `Transport::feed_allowed`
    /// derives it from (a rig frame asserts the copies agree), so media reads its own and the gate's
    /// word is not needed.
    pub fn feed_allowed(&self) -> bool { !self.paused || self.seek_preroll }
    /// The foreground restore's re-pause is owed.
    pub fn resume_pend(&self) -> bool { self.resume_pend }
    /// The pipeline took the video decoder back and nobody has taken that yet.
    pub fn decoder_revoked(&self) -> bool {
        edge_pending(self.raw.decoder_revocations, self.taken_decoder_revocations)
    }
    /// The pipeline refused natively fed audio and nobody has taken that yet.
    pub fn native_audio_refused(&self) -> bool {
        edge_pending(self.raw.native_audio_refusals, self.taken_native_audio_refusals)
    }

    /// Apply `event` through the sole reducer; this pure view has no effects. The session addresses the
    /// events that carry an attempt's facts before it steps them.
    pub fn step(&mut self, event: ReadoutEvent) {
        self.transition(event);
    }
}

/// Whether the feed's count of an edge is ahead of what the player has taken. A count the feed
/// zeroed (its session reset) is no edge, whatever was taken before.
fn edge_pending(counted: u32, taken: u32) -> bool { counted != 0 && counted != taken }

impl PlayerReadout {
    /// The only writer of every readout field, including coalescing and consumed-edge counters.
    fn transition(&mut self, event: ReadoutEvent) {
        match event {
            ReadoutEvent::StateDerived(state) => self.state = state,
            ReadoutEvent::StreamEnded => self.ended = true,
            ReadoutEvent::SeekRequested { target_ns, display_ns } => {
                self.ended = false;
                self.seek_epoch = self.seek_epoch.saturating_add(1);
                self.pending_seek_ns = (target_ns >= 0).then_some(target_ns);
                self.seek_requests = self.seek_requests.saturating_add(1);
                self.seek_display_ns = (display_ns >= 0).then_some(display_ns);
            }
            ReadoutEvent::SeekAbandoned => {
                self.seek_settled = self.seek_epoch;
                self.seek_display_ns = None;
            }
            ReadoutEvent::SeekTaken => {
                self.pending_seek_ns = None;
                self.seek_requests = 0;
            }
            ReadoutEvent::SeekRequeued { target_ns } => {
                self.pending_seek_ns = (target_ns >= 0).then_some(target_ns);
                self.seek_requests = 1;
            },
            ReadoutEvent::ReloadSeek { target_ns } => self.reload_seek_ns = Some(target_ns),
            ReadoutEvent::SessionReset => {
                self.state = PlaybackState::Idle;
                self.ended = false;
                self.raw = FeedRaw::IDLE;
                self.seek_settled = self.seek_epoch;
                // The feed's transport reset (`reset_for_reload`, `reset`) clears its copy of the
                // pending target at the same teardown, and its session reset the display target.
                self.pending_seek_ns = None;
                self.seek_requests = 0;
                self.seek_display_ns = None;
                self.reload_seek_ns = None;
                // The feed's own reset zeroed its counts at the same teardown.
                self.taken_decoder_revocations = 0;
                self.taken_native_audio_refusals = 0;
                self.taken_side_failures = 0;
                // A reader wanted for the engine that is gone describes a stream that is gone
                // (the feed's stop retires its own queued request the same way).
                self.side_reader_want = None;
                // ...and its display base with them.
                self.open_disp_base_ns = 0;
                // The worker's photograph went with the engine (the feed's mirrors reset at the same
                // teardown); its carried link estimate did not (I8).
                self.abr = AbrReadout::IDLE;
            }
            ReadoutEvent::Abr(abr) => self.abr = abr,
            ReadoutEvent::AbrSeed(seed) => self.abr_seed = seed,
            ReadoutEvent::SubtitleInk(known) => self.subtitle_takes_colour = known,
            ReadoutEvent::OriginalFailure(FailureEdit::Noted { kind, status }) => {
                self.original_failure = Some((kind, status.max(0)));
            }
            ReadoutEvent::OriginalFailure(FailureEdit::Cleared) => self.original_failure = None,
            ReadoutEvent::NewItem => {
                self.play_rate_permille = RATE_NORMAL;
                self.rate_reload = false;
            }
            ReadoutEvent::ReleaseQueue { queued } => self.start_queued = queued,
            ReadoutEvent::RateSelected { permille } => self.play_rate_permille = permille,
            ReadoutEvent::RateReloadArmed => self.rate_reload = true,
            ReadoutEvent::RateReloadTaken => self.rate_reload = false,
            ReadoutEvent::SpeedRefused => {
                self.speed_unsupported = true;
                self.play_rate_permille = RATE_NORMAL;
                self.rate_reload = true;
            }
            ReadoutEvent::FactsIngested(raw) => {
                // The feed asks to redo a refused native `Seek` as a reload: a requeue epoch that
                // differs from the previous sample's is ONE new pending seek (the feed armed its
                // own copy when it asked). The same epoch again adds nothing; a reset that zeroed
                // the feed's epoch makes the next request new again.
                let asked = raw.seek_requeue;
                if asked.epoch != 0 && asked.epoch != self.raw.seek_requeue.epoch {
                    self.pending_seek_ns = (asked.target_ns >= 0).then_some(asked.target_ns);
                    self.seek_requests = 1;
                }
                if raw.frames > 0 {
                    // The callback gate counts only the replacement Load here. A seeded
                    // target without a report from the new Load leaves frames at zero.
                    self.reload_seek_ns = None;
                }
                self.raw = raw;
            }
            ReadoutEvent::CommitPaused { paused } => self.paused = paused,
            ReadoutEvent::BeginPausedSeek => {
                if self.paused { self.seek_preroll = true; }
            }
            ReadoutEvent::FinishSeekPreroll => {
                self.seek_preroll = false;
                self.resume_pend = false;
            }
            ReadoutEvent::ResumePending => self.resume_pend = true,
            ReadoutEvent::TransportStopped => {
                self.paused = false;
                self.seek_preroll = false;
                self.resume_pend = false;
            }
            ReadoutEvent::Presented(p) => {
                // Order matters: a seek in flight outranks "we have frames", because the frames
                // on the panel are the PRE-seek ones and the HUD must show the target, not them.
                self.state = if self.seeking() {
                    PlaybackState::Seeking
                } else if p.tick.stage == Stage::Loading {
                    PlaybackState::Connecting
                } else if p.rebuffering {
                    PlaybackState::Buffering
                } else if self.raw.frames == 0 {
                    // No picture yet. Work pending for one is Buffering; a paused player with
                    // nothing in flight is waiting on the viewer, which is neither a spinner nor
                    // `Playing` (no frame is presented, so `is_playing` stays false).
                    if p.nothing_in_flight && self.paused && !self.seek_preroll
                        && self.pending_seek_ns.is_none() {
                        PlaybackState::Paused
                    } else {
                        PlaybackState::Buffering
                    }
                } else {
                    PlaybackState::Playing
                };
                if p.tick.eos_pushed && !self.ended
                    && stream_ended(self.raw.playpos_ns, p.duration_ns, self.raw.pipeline_eos) {
                    self.ended = true;
                }
            }
            ReadoutEvent::Taken { edge } => match edge {
                Edge::DecoderRevoked => self.taken_decoder_revocations = self.raw.decoder_revocations,
                Edge::NativeAudioRefused => {
                    self.taken_native_audio_refusals = self.raw.native_audio_refusals;
                }
                Edge::SideReaderFailed => self.taken_side_failures = self.raw.side_failures,
            },
            ReadoutEvent::SideReaderWanted(want) => self.side_reader_want = Some(want),
            ReadoutEvent::SideReaderWantTaken => self.side_reader_want = None,
            ReadoutEvent::AudioSelected { idx } => self.audio_choice = idx,
            ReadoutEvent::OpenRebased { disp_base } => self.open_disp_base_ns = disp_base,
        }
    }
}

impl Default for PlayerReadout {
    fn default() -> Self { Self::IDLE }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::attempt_view::Backend;
    use crate::route::{PlaybackSession, RouteStartAttempt};
    use plx_playback_contract::feed::{FeedFacts, FeedLoad, SeekRequeue};
    use plx_playback_contract::feed_fake::FakeFeed;

    #[test]
    fn explicit_and_sampled_requeues_fold_only_the_requests_they_replace() {
        let mut r = stepped(&[tap(1), tap(2), ReadoutEvent::SeekRequeued { target_ns: 3_000_000_000 }]);
        assert_eq!((r.seek_epoch(), r.pending_seek(), r.seek_coalesced()), (2, Some(3_000_000_000), 0));
        assert_eq!(r.seek_display_ns(), Some(2_000_000_000));
        let raw = FeedRaw { seek_requeue: SeekRequeue { epoch: 7, target_ns: 4_000_000_000 }, ..FeedRaw::IDLE };
        r.step(ReadoutEvent::FactsIngested(raw));
        assert_eq!((r.seek_epoch(), r.pending_seek(), r.seek_coalesced()), (2, Some(4_000_000_000), 0));
        r.step(tap(5));
        r.step(ReadoutEvent::FactsIngested(raw));
        assert_eq!((r.seek_epoch(), r.pending_seek(), r.seek_coalesced()), (3, Some(5_000_000_000), 1));
        r.step(ReadoutEvent::SessionReset);
        assert_eq!((r.seek_epoch(), r.pending_seek(), r.seek_coalesced()), (3, None, 0));
        assert!(!r.seeking());
    }

    fn stepped(events: &[ReadoutEvent]) -> PlayerReadout {
        let mut readout = PlayerReadout::IDLE;
        for event in events { readout.step(event.clone()); }
        readout
    }

    /// A seek request for `secs`, the HUD freezing at the same target.
    fn tap(secs: i64) -> ReadoutEvent {
        ReadoutEvent::SeekRequested { target_ns: secs * 1_000_000_000, display_ns: secs * 1_000_000_000 }
    }

    /// The feed gate is open for a playing transport, shut for a viewer Pause, and open again
    /// while a paused seek's preroll lets one landed frame through; closing the preroll, resuming
    /// and stopping each leave it as the feed's own `feed_allowed` leaves its gate.
    #[test]
    fn feeding_is_allowed_unless_paused_and_a_paused_seek_preroll_reopens_it() {
        assert!(PlayerReadout::IDLE.feed_allowed(), "nothing paused: the gate is open");
        let paused = stepped(&[ReadoutEvent::CommitPaused { paused: true }]);
        assert!(!paused.feed_allowed(), "a viewer Pause shuts the gate");
        let mut r = stepped(&[ReadoutEvent::CommitPaused { paused: true }, ReadoutEvent::BeginPausedSeek]);
        assert!(r.paused() && r.seek_preroll());
        assert!(r.feed_allowed(), "a paused seek's preroll lets the keyframe through");
        r.step(ReadoutEvent::FinishSeekPreroll);
        assert!(r.paused() && !r.seek_preroll() && !r.feed_allowed(), "the gate shuts behind the closed preroll");
        r.step(ReadoutEvent::BeginPausedSeek);
        r.step(ReadoutEvent::CommitPaused { paused: false });
        assert!(r.feed_allowed(), "a Resume opens it, preroll or not");
        r.step(ReadoutEvent::CommitPaused { paused: true });
        r.step(ReadoutEvent::TransportStopped);
        assert!(r.feed_allowed(), "a stop retires the Pause and the preroll with it");
        let unpaused_seek = stepped(&[ReadoutEvent::BeginPausedSeek]);
        assert!(!unpaused_seek.seek_preroll() && unpaused_seek.feed_allowed(), "no preroll opens without a Pause");
    }

    #[test]
    fn a_readout_starts_idle_at_normal_speed() {
        let r = PlayerReadout::IDLE;
        assert_eq!(r.state(), PlaybackState::Idle);
        assert!(!r.ended() && !r.start_queued() && !r.speed_unsupported() && !r.rate_reload());
        assert_eq!(r.play_rate_permille(), RATE_NORMAL);
        assert_eq!(*r.raw(), FeedRaw::IDLE);
    }

    /// The player's audio choice is its own: a session reset (every reload) and a new item leave it,
    /// as the feed's `desired_audio_idx` was left by the feed's reset; only a track write moves it.
    #[test]
    fn the_audio_choice_survives_a_session_reset_and_a_new_item_and_only_a_selection_moves_it() {
        assert_eq!(PlayerReadout::IDLE.audio_choice(), -1);
        let r = stepped(&[ReadoutEvent::AudioSelected { idx: 3 }, ReadoutEvent::SessionReset]);
        assert_eq!(r.audio_choice(), 3, "a reload keeps the pick");
        let r = stepped(&[ReadoutEvent::AudioSelected { idx: 3 }, ReadoutEvent::NewItem]);
        assert_eq!(r.audio_choice(), 3, "a retry's new-item reset does not clear it either");
        let r = stepped(&[ReadoutEvent::AudioSelected { idx: 3 }, ReadoutEvent::AudioSelected { idx: -1 }]);
        assert_eq!(r.audio_choice(), -1);
    }

    /// The commanded display base is the player's record of a restart's `Rebase`, held until the feed's
    /// session reset (which zeroes the feed's own) retires it.
    #[test]
    fn a_commanded_display_base_lasts_until_the_session_resets() {
        assert_eq!(PlayerReadout::IDLE.open_disp_base_ns(), 0);
        let r = stepped(&[ReadoutEvent::OpenRebased { disp_base: 600_000_000_000 }]);
        assert_eq!(r.open_disp_base_ns(), 600_000_000_000);
        assert_eq!(r.raw().disp_base, 0, "no sample has carried it yet");
        let r = stepped(&[ReadoutEvent::OpenRebased { disp_base: 600_000_000_000 }, ReadoutEvent::SessionReset]);
        assert_eq!(r.open_disp_base_ns(), 0);
    }

    #[test]
    fn the_derived_state_and_the_end_of_stream_follow_their_events() {
        let r = stepped(&[ReadoutEvent::StateDerived(PlaybackState::Playing), ReadoutEvent::StreamEnded]);
        assert_eq!(r.state(), PlaybackState::Playing);
        assert!(r.ended());
        let r = stepped(&[ReadoutEvent::StreamEnded, tap(1)]);
        assert!(!r.ended(), "a seek supersedes the end");
    }

    #[test]
    fn a_session_reset_forgets_the_session_but_not_the_viewers_choices() {
        let mut facts = FeedRaw::IDLE;
        facts.frames = 90;
        facts.flags.demux_failed = true;
        let r = stepped(&[
            ReadoutEvent::RateSelected { permille: 1500 },
            ReadoutEvent::ReleaseQueue { queued: true },
            ReadoutEvent::SpeedRefused,
            ReadoutEvent::StateDerived(PlaybackState::Error),
            ReadoutEvent::StreamEnded,
            ReadoutEvent::FactsIngested(facts),
            ReadoutEvent::SessionReset,
        ]);
        assert_eq!(r.state(), PlaybackState::Idle);
        assert!(!r.ended());
        assert_eq!(*r.raw(), FeedRaw::IDLE, "one session's facts do not outlive it");
        assert!(r.speed_unsupported(), "the latch is for the life of the process");
        assert!(r.start_queued(), "the queue outlives a session reset exactly as the machine does");
        assert!(r.rate_reload(), "a pending reload survives the reset that carries it");
    }

    #[test]
    fn a_new_item_returns_the_speed_to_normal_and_drops_a_pending_reload() {
        let r = stepped(&[
            ReadoutEvent::RateSelected { permille: 1500 },
            ReadoutEvent::RateReloadArmed,
            ReadoutEvent::NewItem,
        ]);
        assert_eq!(r.play_rate_permille(), RATE_NORMAL);
        assert!(!r.rate_reload());
    }

    #[test]
    fn a_speed_change_arms_one_reload_and_the_pump_takes_it_once() {
        let mut r = stepped(&[ReadoutEvent::RateSelected { permille: 1250 }, ReadoutEvent::RateReloadArmed]);
        assert_eq!(r.play_rate_permille(), 1250);
        assert!(r.rate_reload());
        r.step(ReadoutEvent::RateReloadTaken);
        assert!(!r.rate_reload());
        assert_eq!(r.play_rate_permille(), 1250, "taking the reload keeps the speed");
    }

    #[test]
    fn refusing_the_speed_session_latches_and_returns_to_normal_in_one_event() {
        let r = stepped(&[ReadoutEvent::RateSelected { permille: 2000 }, ReadoutEvent::SpeedRefused]);
        assert!(r.speed_unsupported());
        assert_eq!(r.play_rate_permille(), RATE_NORMAL);
        assert!(r.rate_reload(), "the reload that lands at 1x is armed");
    }

    #[test]
    fn the_last_ingested_facts_replace_the_previous_ones() {
        let mut first = FeedRaw::IDLE;
        first.playpos_ns = 5;
        let mut second = FeedRaw::IDLE;
        second.frames = 3;
        let r = stepped(&[ReadoutEvent::FactsIngested(first), ReadoutEvent::FactsIngested(second)]);
        assert_eq!(*r.raw(), second, "a sample replaces, it does not merge");
    }

    #[test]
    fn the_release_queue_follows_the_machine() {
        let r = stepped(&[ReadoutEvent::ReleaseQueue { queued: true }]);
        assert!(r.start_queued());
        let r = stepped(&[ReadoutEvent::ReleaseQueue { queued: true }, ReadoutEvent::ReleaseQueue { queued: false }]);
        assert!(!r.start_queued());
    }

    // ---- the session's ingestion and the addressed `Presented` event (L3a-2 step 3) ----

    fn facts(frames: i32, duration_ns: Option<i64>) -> FeedFacts {
        let mut facts = FeedFacts::default();
        facts.raw.frames = frames;
        facts.raw.playpos_ns = 7;
        facts.attempt.duration_ns = duration_ns;
        facts
    }

    fn tick(stage: Stage, eos_pushed: bool) -> PresentationTick {
        PresentationTick { stage, eos_pushed }
    }

    /// No claim hold and no route flight: the readout's own Pause alone decides "paused and idle".
    const CLEAR: bool = true;

    /// One sample splits between the attempt view and the readout, and only the attempt the view
    /// belongs to can deliver it: a late worker of a previous attempt changes neither half.
    #[test]
    fn a_feed_sample_reaches_both_halves_addressed_by_its_attempt() {
        let _serial = plx_base::testlock::serial();
        let (a, b) = (RouteStartAttempt::fixture(), RouteStartAttempt::fixture_n(2));
        let mut ps = PlaybackSession::IDLE;
        ps.begin_attempt_view(a, Backend::Feed);
        assert!(ps.ingest_feed_facts(a, facts(5, Some(100))));
        assert_eq!(ps.readout().raw().frames, 5, "the raw half reaches the readout");
        assert_eq!(ps.attempt_view().facts().duration_ns, Some(100), "the mapped half reaches the view");

        ps.begin_attempt_view(b, Backend::Feed);
        assert!(!ps.ingest_feed_facts(a, facts(9, Some(900))), "a stale attempt is refused");
        assert_eq!(ps.readout().raw().frames, 5, "its raw facts did not reach the readout");
        assert_eq!(ps.attempt_view().facts().duration_ns, None, "nor the view");
        assert!(ps.ingest_feed_facts(b, facts(9, Some(900))));
        assert_eq!(ps.readout().raw().frames, 9);
    }

    /// The Feed's sample is the Feed's: an attempt the view began for the other backend refuses it.
    #[test]
    fn a_feed_sample_is_refused_for_an_attempt_of_the_other_backend() {
        let _serial = plx_base::testlock::serial();
        let c = RouteStartAttempt::fixture_n(3);
        let mut ps = PlaybackSession::IDLE;
        ps.begin_attempt_view(c, Backend::Uri);
        assert!(!ps.ingest_feed_facts(c, facts(5, None)));
        assert_eq!(*ps.readout().raw(), FeedRaw::IDLE);
        assert!(!ps.present(c, tick(Stage::Playing, false), facts(5, None), CLEAR));
        assert_eq!(ps.readout().state(), PlaybackState::Idle);
    }

    /// `Presented` is addressed like the ingestion: a frame the pump presented for an attempt that
    /// has since been replaced derives nothing and ingests nothing.
    #[test]
    fn a_presented_frame_for_a_replaced_attempt_changes_nothing() {
        let _serial = plx_base::testlock::serial();
        let (a, b) = (RouteStartAttempt::fixture(), RouteStartAttempt::fixture_n(2));
        let mut ps = PlaybackSession::IDLE;
        ps.begin_attempt_view(a, Backend::Feed);
        ps.begin_attempt_view(b, Backend::Feed);
        assert!(!ps.present(a, tick(Stage::Streaming, true), facts(30, Some(10)), CLEAR));
        assert_eq!(ps.readout().state(), PlaybackState::Idle);
        assert!(!ps.readout().ended());
        assert_eq!(*ps.readout().raw(), FeedRaw::IDLE);
        assert!(ps.present(b, tick(Stage::Streaming, false), facts(30, Some(10)), CLEAR));
        assert_eq!(ps.readout().state(), PlaybackState::Playing);
        assert_eq!(ps.readout().raw().frames, 30, "the post-tick sample is the readout's raw facts");
    }

    /// A test scripts a `FakeFeed`'s sample and delivers it through the real ingestion: the helper
    /// writes no readout field.
    #[test]
    fn a_scripted_fake_feed_sample_is_delivered_through_the_real_ingestion() {
        let _serial = plx_base::testlock::serial();
        let a = RouteStartAttempt::fixture();
        let (fake, handle) = FakeFeed::new();
        handle.script(|s| {
            s.facts = facts(12, Some(4));
            s.facts.raw.load.stage = Some(Stage::Loading);
        });
        let mut ps = PlaybackSession::IDLE;
        ps.begin_attempt_view(a, Backend::Feed);
        assert!(ps.ingest_feed_facts(a, fake.sample().expect("a session is installed")));
        assert_eq!(ps.readout().raw().load.stage, Some(Stage::Loading), "the Load half arrives too");
        assert_eq!(ps.readout().raw().frames, 12);
        assert_eq!(handle.calls(), vec![plx_playback_contract::feed_fake::FeedCall::Sample]);
    }

    fn presented(raw_frames: i32, p: Presentation) -> PlayerReadout {
        let mut raw = FeedRaw::IDLE;
        raw.frames = raw_frames;
        raw.playpos_ns = 99_500_000_000;
        stepped(&[ReadoutEvent::FactsIngested(raw), ReadoutEvent::Presented(p)])
    }

    /// [`presented`] under the viewer's Pause.
    fn presented_paused(raw_frames: i32, p: Presentation) -> PlayerReadout {
        let mut raw = FeedRaw::IDLE;
        raw.frames = raw_frames;
        stepped(&[
            ReadoutEvent::CommitPaused { paused: true },
            ReadoutEvent::FactsIngested(raw),
            ReadoutEvent::Presented(p),
        ])
    }

    fn presented_seeking(raw_frames: i32, p: Presentation) -> PlayerReadout {
        let mut raw = FeedRaw::IDLE;
        raw.frames = raw_frames;
        stepped(&[
            tap(1),
            ReadoutEvent::FactsIngested(raw),
            ReadoutEvent::Presented(p),
        ])
    }

    fn pres(stage: Stage) -> Presentation {
        Presentation {
            tick: tick(stage, false), rebuffering: false, duration_ns: 100_000_000_000,
            nothing_in_flight: true,
        }
    }

    /// The ladder's order: a seek outranks everything, then a Load still opening, then a rebuffer,
    /// then "no picture yet" (Paused when idle, else Buffering), else Playing.
    #[test]
    fn the_presented_frame_derives_the_state_in_the_ladders_order() {
        assert_eq!(presented_seeking(30, pres(Stage::Loading)).state(),
            PlaybackState::Seeking, "a seek outranks the frames on the panel");
        assert_eq!(presented(30, pres(Stage::Loading)).state(), PlaybackState::Connecting);
        assert_eq!(presented(30, Presentation { rebuffering: true, ..pres(Stage::Streaming) }).state(),
            PlaybackState::Buffering);
        assert_eq!(presented(0, pres(Stage::Streaming)).state(), PlaybackState::Buffering,
            "no picture yet, work pending");
        assert_eq!(presented_paused(0, pres(Stage::Streaming)).state(),
            PlaybackState::Paused, "no picture, nothing in flight: waiting on the viewer");
        assert_eq!(presented_paused(30, pres(Stage::Streaming)).state(),
            PlaybackState::Playing, "paused-and-idle only matters before the first picture");
        assert_eq!(presented(30, pres(Stage::Streaming)).state(), PlaybackState::Playing);
    }

    /// The end of stream needs the EOS pushed, then the pipeline's own word or a playhead inside the
    /// tail of a known duration; it is latched until a seek supersedes it.
    #[test]
    fn the_presented_frame_ends_the_stream_only_after_the_eos_and_the_tail() {
        let pushed = |p: Presentation| Presentation { tick: tick(Stage::Streaming, true), ..p };
        assert!(!presented(30, pres(Stage::Streaming)).ended(), "the EOS is not pushed yet");
        assert!(presented(30, pushed(pres(Stage::Streaming))).ended(), "within the tail of the duration");
        assert!(!presented(30, pushed(Presentation { duration_ns: 0, ..pres(Stage::Streaming) })).ended(),
            "no duration, no arithmetic");
        let mut said = FeedRaw::IDLE;
        said.pipeline_eos = true;
        assert!(stepped(&[
            ReadoutEvent::FactsIngested(said),
            ReadoutEvent::Presented(pushed(Presentation { duration_ns: 0, ..pres(Stage::Streaming) })),
        ]).ended(), "the pipeline said so: the ingested raw fact, not an input of the frame");
        let mut r = presented(30, pushed(pres(Stage::Streaming)));
        r.step(ReadoutEvent::Presented(pres(Stage::Streaming)));
        assert!(r.ended(), "a later frame without the EOS does not un-end it");
        r.step(tap(1));
        assert!(!r.ended());
    }

    // ---- the seek family: the requested epoch, the derived `seeking` (L3b step 1) ----

    fn landed(epoch: u32) -> ReadoutEvent {
        let mut raw = FeedRaw::IDLE;
        raw.seek_landed_epoch = epoch;
        ReadoutEvent::FactsIngested(raw)
    }

    /// A seek requested twice before the first lands is in flight until the LATER epoch lands: the
    /// first landing is not the second request's.
    #[test]
    fn a_seek_requested_twice_is_in_flight_until_the_later_epoch_lands() {
        let mut r = stepped(&[tap(1), tap(1)]);
        assert_eq!(r.seek_epoch(), 2);
        assert!(r.seeking());
        r.step(landed(1));
        assert!(r.seeking(), "the first request's landing does not end the second");
        r.step(landed(2));
        assert!(!r.seeking());
        r.step(tap(1));
        assert!(r.seeking(), "a request after a landing is a new flight");
        assert_eq!(r.seek_epoch(), 3, "the epoch counts requests for the life of the readout");
    }

    /// Giving up on a seek ends it without a landing: nothing can leave `seeking` armed, because
    /// nothing stores it. A request after that is a new flight.
    #[test]
    fn an_abandoned_seek_is_no_longer_in_flight() {
        let mut r = stepped(&[tap(1), tap(1)]);
        r.step(ReadoutEvent::SeekAbandoned);
        assert!(!r.seeking(), "every request so far is given up on");
        r.step(landed(1));
        assert!(!r.seeking(), "a late landing of an abandoned seek changes nothing");
        r.step(tap(1));
        assert!(r.seeking());
    }

    /// A real stop or a reload retires the seek it interrupted exactly as the feed's own reset
    /// cleared its flag, and a request made after the reset is in flight again even though the
    /// feed's epoch cell was zeroed with the session.
    #[test]
    fn a_session_reset_ends_the_seek_that_was_in_flight() {
        let mut r = stepped(&[tap(1), tap(1), ReadoutEvent::SessionReset]);
        assert!(!r.seeking());
        assert_eq!(r.seek_epoch(), 2, "the count is not the session's");
        r.step(tap(1));
        assert!(r.seeking());
        r.step(landed(3));
        assert!(!r.seeking());
    }

    /// A seek in flight outranks the frames on the panel in the presented state, and the frame
    /// after the landing is Playing again.
    #[test]
    fn the_presented_state_is_seeking_exactly_while_a_requested_seek_has_not_landed() {
        assert_eq!(presented_seeking(30, pres(Stage::Streaming)).state(), PlaybackState::Seeking);
        let mut raw = FeedRaw::IDLE;
        raw.frames = 30;
        raw.seek_landed_epoch = 1;
        let r = stepped(&[
            tap(1),
            ReadoutEvent::FactsIngested(raw),
            ReadoutEvent::Presented(pres(Stage::Streaming)),
        ]);
        assert_eq!(r.state(), PlaybackState::Playing, "the landing is one ingestion away");
    }

    // ---- the pending seek: where it waits, what it folded, who may re-arm it (L3b step 1b) ----

    /// The sample the feed's native-Seek-refused downgrade leaves: request number `epoch`, aimed at
    /// `secs`.
    fn requeue_sample(epoch: u32, secs: i64) -> ReadoutEvent {
        let mut raw = FeedRaw::IDLE;
        raw.seek_requeue = SeekRequeue { epoch, target_ns: secs * 1_000_000_000 };
        ReadoutEvent::FactsIngested(raw)
    }

    const fn secs(s: i64) -> i64 { s * 1_000_000_000 }

    /// A burst of taps leaves ONE pending target, the newest, and reports how many it folded; a take
    /// spends both; the seek in flight and the HUD's display target are the request's, not the take's.
    #[test]
    fn a_burst_of_taps_leaves_one_pending_target_and_counts_the_folded_requests() {
        let mut r = stepped(&[tap(10), tap(20), tap(30)]);
        assert_eq!(r.pending_seek(), Some(secs(30)), "the newest target wins");
        assert_eq!(r.seek_coalesced(), 2, "three requests applied as one merged two");
        assert_eq!(r.seek_display_ns(), Some(secs(30)));
        r.step(ReadoutEvent::SeekTaken);
        assert_eq!(r.pending_seek(), None);
        assert_eq!(r.seek_coalesced(), 0, "a take spends the count");
        assert!(r.seeking(), "taking the target is not landing it");
        assert_eq!(r.seek_display_ns(), Some(secs(30)), "the playhead stays frozen until the seek resolves");
        r.step(ReadoutEvent::SeekTaken);
        assert_eq!(r.pending_seek(), None, "a take with nothing waiting changes nothing");
    }

    /// Giving up ends the spinner and the frozen playhead but leaves the target to the pump, as it
    /// always did; a session reset takes everything the session held, as the feed's resets do.
    #[test]
    fn giving_up_keeps_the_pending_target_and_a_reset_takes_it() {
        let mut r = stepped(&[tap(10), ReadoutEvent::SeekAbandoned]);
        assert_eq!(r.seek_display_ns(), None, "no stale frozen playhead");
        assert_eq!(r.pending_seek(), Some(secs(10)), "giving up leaves the target to the pump");
        r.step(ReadoutEvent::SessionReset);
        assert_eq!((r.pending_seek(), r.seek_display_ns(), r.seek_coalesced()), (None, None, 0));
        let r = stepped(&[tap(10), tap(20), ReadoutEvent::SessionReset]);
        assert_eq!((r.pending_seek(), r.seek_display_ns(), r.seek_coalesced()), (None, None, 0));
    }

    /// A taken seek put back waits again as ONE request and is not a request: nothing is numbered,
    /// nothing is put in flight, the display target is untouched.
    #[test]
    fn a_requeued_seek_waits_again_without_being_requested_again() {
        let mut r = stepped(&[tap(10), ReadoutEvent::SeekTaken, ReadoutEvent::SeekAbandoned]);
        let epoch = r.seek_epoch();
        r.step(ReadoutEvent::SeekRequeued { target_ns: secs(10) });
        assert_eq!(r.pending_seek(), Some(secs(10)));
        assert_eq!(r.seek_coalesced(), 0, "one request waits");
        assert_eq!(r.seek_epoch(), epoch, "nothing was numbered");
        assert!(!r.seeking(), "re-arming a target puts nothing in flight");
        assert_eq!(r.seek_display_ns(), None, "and does not freeze the playhead");
        r.step(tap(40));
        assert_eq!(r.seek_coalesced(), 1, "a tap on top of a requeued one folds it");
        assert_eq!(r.pending_seek(), Some(secs(40)));
    }

    /// The feed's request to redo a refused native Seek is adopted ONCE per epoch: the same sample
    /// again (every ingestion re-reads the cell) adds nothing, even after the pump took the seek,
    /// and a feed-only reset that zeroed the epoch makes the next request new again.
    #[test]
    fn the_feeds_requeue_becomes_one_pending_seek_per_new_epoch() {
        let mut r = stepped(&[tap(10), ReadoutEvent::SeekTaken]);
        r.step(requeue_sample(1, 40));
        assert_eq!(r.pending_seek(), Some(secs(40)), "adopted from the sample");
        assert_eq!(r.seek_coalesced(), 0);
        assert_eq!(r.seek_epoch(), 1, "the feed redoing a seek is not a new request");
        r.step(ReadoutEvent::SeekTaken);
        r.step(requeue_sample(1, 40));
        r.step(requeue_sample(1, 40));
        assert_eq!(r.pending_seek(), None, "the same epoch again adds nothing once it was taken");

        // The cell was zeroed with the feed's session and the next downgrade counts from 1 again.
        r.step(requeue_sample(0, 0));
        r.step(requeue_sample(1, 55));
        assert_eq!(r.pending_seek(), Some(secs(55)), "a new request after a reset is new");
        r.step(ReadoutEvent::SeekTaken);
        r.step(requeue_sample(2, 60));
        assert_eq!(r.pending_seek(), Some(secs(60)), "a later epoch is a later request");

        // The player's reset forgets what it had seen of the old session.
        r.step(ReadoutEvent::SeekTaken);
        r.step(ReadoutEvent::SessionReset);
        r.step(requeue_sample(1, 70));
        assert_eq!(r.pending_seek(), Some(secs(70)));
    }

    /// A seek waiting for the pump is work: a paused player with one is not "waiting on the viewer".
    #[test]
    fn a_seek_waiting_for_the_pump_keeps_a_paused_player_buffering() {
        let paused = pres(Stage::Streaming);
        assert_eq!(presented_paused(0, paused).state(), PlaybackState::Paused);
        let mut raw = FeedRaw::IDLE;
        raw.seek_requeue = SeekRequeue { epoch: 1, target_ns: secs(5) };
        let r = stepped(&[
            ReadoutEvent::CommitPaused { paused: true },
            ReadoutEvent::FactsIngested(raw),
            ReadoutEvent::Presented(paused),
        ]);
        assert_eq!(r.state(), PlaybackState::Buffering, "the feed's own requeue, adopted from the very sample the frame took");
    }

    // ---- the transport: the Pause, the paused-seek preroll, the owed re-pause (L3b step 2c) ----

    /// The preroll opens only under a Pause, exactly as the feed's own open, and an open that finds
    /// it open again changes nothing (a coalesced tap retargets the feed's owned preroll).
    #[test]
    fn a_preroll_opens_only_under_a_pause_and_opening_it_twice_changes_nothing() {
        let r = stepped(&[ReadoutEvent::BeginPausedSeek]);
        assert!(!r.seek_preroll(), "a transport that is not paused opens nothing");
        let once = stepped(&[ReadoutEvent::CommitPaused { paused: true }, ReadoutEvent::BeginPausedSeek]);
        assert!(once.paused() && once.seek_preroll());
        let mut twice = once.clone();
        twice.step(ReadoutEvent::BeginPausedSeek);
        assert_eq!(twice, once);
        let resumed = stepped(&[ReadoutEvent::CommitPaused { paused: true }, ReadoutEvent::BeginPausedSeek,
            ReadoutEvent::CommitPaused { paused: false }]);
        assert!(!resumed.paused() && resumed.seek_preroll(),
            "the commit alone does not close a preroll: the close is its own event, as it is the feed's");
    }

    /// (b) A reload replaces the Engine and keeps the viewer's Pause, the preroll and the owed
    /// re-pause (`Transport::reset_for_reload`): the session reset it fires does not touch them. A
    /// real stop clears all three (`Transport::reset`).
    #[test]
    fn a_reload_keeps_the_pause_and_a_real_stop_clears_all_three() {
        let mut r = stepped(&[ReadoutEvent::CommitPaused { paused: true }, ReadoutEvent::BeginPausedSeek,
            ReadoutEvent::ResumePending]);
        r.step(ReadoutEvent::SessionReset);
        assert!(r.paused(), "a reload keeps the viewer's Pause");
        assert!(r.seek_preroll(), "and the preroll that outlives the reload");
        assert!(r.resume_pend(), "and the re-pause the restore still owes");
        r.step(ReadoutEvent::TransportStopped);
        assert!(!r.paused() && !r.seek_preroll() && !r.resume_pend(), "a real stop retires all three");
    }

    /// (c) The owed re-pause ends with the preroll's close, whatever closes it (the feed's own
    /// `finish_seek_preroll` cleared it too, and does so with no preroll open), and with a real
    /// stop; a reload's session reset leaves it.
    #[test]
    fn the_owed_repause_ends_with_the_preroll_close_and_the_real_stop_not_with_a_reload() {
        let mut r = stepped(&[ReadoutEvent::CommitPaused { paused: true }, ReadoutEvent::ResumePending]);
        assert!(r.resume_pend());
        r.step(ReadoutEvent::SessionReset);
        assert!(r.resume_pend(), "a reload does not clear it");
        r.step(ReadoutEvent::FinishSeekPreroll);
        assert!(!r.resume_pend(), "the close ends it, with no preroll open as with one");
        assert!(r.paused(), "and closes nothing of the Pause");
        r.step(ReadoutEvent::ResumePending);
        r.step(ReadoutEvent::TransportStopped);
        assert!(!r.resume_pend(), "a real stop ends it");
    }

    /// "Paused and idle" is the readout's Pause with no preroll open and no seek waiting, plus the
    /// two inputs it does not own: each one alone keeps the HUD at Buffering.
    #[test]
    fn paused_and_idle_needs_the_pause_the_shut_gate_and_nothing_in_flight() {
        let paused = ReadoutEvent::CommitPaused { paused: true };
        let mut raw = FeedRaw::IDLE;
        raw.frames = 0;
        let derive = |events: &[ReadoutEvent], p: Presentation| {
            let mut r = stepped(events);
            r.step(ReadoutEvent::FactsIngested(raw));
            r.step(ReadoutEvent::Presented(p));
            r.state()
        };
        assert_eq!(derive(&[paused.clone()], pres(Stage::Streaming)), PlaybackState::Paused);
        assert_eq!(derive(&[], pres(Stage::Streaming)), PlaybackState::Buffering, "not paused");
        assert_eq!(derive(&[paused.clone(), ReadoutEvent::BeginPausedSeek], pres(Stage::Streaming)),
            PlaybackState::Buffering, "a preroll open is work: the gate is not shut");
        assert_eq!(derive(&[paused.clone()], Presentation { nothing_in_flight: false, ..pres(Stage::Streaming) }),
            PlaybackState::Buffering, "a claim hold or a flight is work");
        assert_eq!(derive(&[paused, tap(1)], pres(Stage::Streaming)), PlaybackState::Seeking);
    }

    // ---- the side reader: the failure edge the feed counts, and the want the pump hands over ----

    fn side_failures(count: u32) -> ReadoutEvent {
        ReadoutEvent::FactsIngested(FeedRaw { side_failures: count, ..FeedRaw::IDLE })
    }

    /// The reader's give-up is a counted edge like the decoder's: pending until taken, spent after
    /// it, pending again for the next raise, never pending for a stop's voided `0`, and a session
    /// reset forgets what was taken (the feed's stop zeroes its count with it).
    #[test]
    fn a_side_reader_failure_is_taken_once_and_a_stop_voids_it() {
        let mut r = stepped(&[side_failures(1)]);
        assert!(r.side_reader_failed(), "a raise nobody has taken is pending");
        r.step(ReadoutEvent::Taken { edge: Edge::SideReaderFailed });
        assert!(!r.side_reader_failed(), "taken once, it is spent");
        r.step(side_failures(1));
        assert!(!r.side_reader_failed(), "the next sample of the same count is no new edge");
        r.step(side_failures(2));
        assert!(r.side_reader_failed(), "the next raise is a new edge");
        r.step(side_failures(0));
        assert!(!r.side_reader_failed(), "a stop voided it before it was taken");
        r.step(ReadoutEvent::Taken { edge: Edge::SideReaderFailed });
        r.step(side_failures(3));
        assert!(r.side_reader_failed());
        r.step(ReadoutEvent::SessionReset);
        assert!(!r.side_reader_failed(), "the session's raw facts are gone with it");
    }

    /// The route's wish for the reader waits in the readout until the pump takes it; the newest
    /// wins (a stop after a start is one request, not two), and a session reset drops it.
    #[test]
    fn the_side_reader_the_route_wants_waits_for_the_pump_and_the_newest_wins() {
        let spec = |ordinal| Arc::new(Spec {
            url: String::new(), offset_ns: 0, part_kbps: 0, ordinal, from_playhead: false, delta: None,
        });
        let mut r = PlayerReadout::IDLE;
        assert_eq!(r.side_reader_want(), None);
        r.step(ReadoutEvent::SideReaderWanted(Some(spec(2))));
        r.step(ReadoutEvent::SideReaderWanted(Some(spec(5))));
        assert_eq!(r.side_reader_want().map(|w| w.as_ref().map(|s| s.ordinal)), Some(Some(5)));
        r.step(ReadoutEvent::SideReaderWanted(None));
        assert_eq!(r.side_reader_want(), Some(&None), "stopping is a wish too");
        r.step(ReadoutEvent::SideReaderWantTaken);
        assert_eq!(r.side_reader_want(), None, "the pump took it");
        r.step(ReadoutEvent::SideReaderWanted(Some(spec(1))));
        r.step(ReadoutEvent::SessionReset);
        assert_eq!(r.side_reader_want(), None, "a wish for an engine that is gone is dropped");
    }

    // ---- the one-shot edges the feed counts and the readout takes (L3b-4) ----

    fn counted(revocations: u32, refusals: u32) -> ReadoutEvent {
        let mut raw = FeedRaw::IDLE;
        raw.decoder_revocations = revocations;
        raw.native_audio_refusals = refusals;
        ReadoutEvent::FactsIngested(raw)
    }

    /// An edge is pending while the feed's count differs from what was taken: a take spends it, the
    /// same count ingested again (every pump frame re-samples it) adds nothing, a newer count is a
    /// new edge, and the two edges are taken separately.
    #[test]
    fn a_taken_edge_is_counted_once_and_a_new_one_is_seen() {
        let mut r = stepped(&[counted(1, 0)]);
        assert!(r.decoder_revoked() && !r.native_audio_refused());
        r.step(ReadoutEvent::Taken { edge: Edge::DecoderRevoked });
        assert!(!r.decoder_revoked(), "taken");
        r.step(counted(1, 0));
        r.step(counted(1, 0));
        assert!(!r.decoder_revoked(), "the same count ingested again adds nothing");
        r.step(counted(2, 1));
        assert!(r.decoder_revoked(), "a new revocation is seen");
        assert!(r.native_audio_refused(), "the other edge is its own");
        r.step(ReadoutEvent::Taken { edge: Edge::NativeAudioRefused });
        assert!(r.decoder_revoked() && !r.native_audio_refused(), "taking one leaves the other pending");
    }

    /// The feed zeroes its counts with its session and the readout zeroes what it took at the same
    /// teardown, so the next session's first edge is seen; a count the feed zeroed alone is no edge.
    #[test]
    fn a_session_reset_zeroes_the_takes_and_a_zero_count_is_never_an_edge() {
        let mut r = stepped(&[counted(3, 2), ReadoutEvent::Taken { edge: Edge::DecoderRevoked }]);
        r.step(ReadoutEvent::SessionReset);
        assert!(!r.decoder_revoked() && !r.native_audio_refused(), "the reset forgets an unread edge");
        r.step(counted(1, 1));
        assert!(r.decoder_revoked() && r.native_audio_refused(), "a new session's first edge counts from 1 again");
        let mut r = stepped(&[counted(3, 3), ReadoutEvent::Taken { edge: Edge::DecoderRevoked }, counted(0, 0)]);
        assert!(!r.decoder_revoked(), "a feed-only reset left taken above the count: 0 is not an edge");
        r.step(counted(1, 0));
        assert!(r.decoder_revoked());
    }

    // ---- the readout is the READER of `state` and `ended` (L3a-2 step 4a) ----

    /// What `player::state` and `player::ended` say follows the readout: a presented end of stream
    /// is what the app's end-of-film rule polls, and a real stop's `SessionReset` clears it, so a
    /// replay does not find the finished play's end.
    #[test]
    fn the_readers_follow_the_readout_after_a_presented_eos_and_a_real_stop_clears_it() {
        let _serial = plx_base::testlock::serial();
        let a = RouteStartAttempt::fixture();
        let mut ps = PlaybackSession::IDLE;
        ps.begin_attempt_view(a, Backend::Feed);
        assert_eq!(crate::player::ended(&ps), Some(false), "a live feed attempt has not ended");
        assert!(ps.present(a, tick(Stage::Streaming, true), facts(30, Some(100)), CLEAR));
        assert_eq!(crate::player::state(&ps), PlaybackState::Playing, "the reader is the readout's state");
        assert_eq!(crate::player::ended(&ps), Some(true), "the reader is the readout's end of stream");
        ps.step_readout(ReadoutEvent::SessionReset);
        assert_eq!(crate::player::ended(&ps), Some(false), "a real stop retires the end");
        assert_eq!(crate::player::state(&ps), PlaybackState::Idle);
    }

    /// The URI backend keeps its own fact: its end of stream is an `AttemptFacts` value its driver
    /// writes, and the Feed's readout does not speak for it.
    #[test]
    fn the_uri_backends_end_of_stream_is_still_its_own_fact() {
        let _serial = plx_base::testlock::serial();
        let a = RouteStartAttempt::fixture();
        let mut ps = PlaybackSession::IDLE;
        ps.begin_attempt_view(a, Backend::Uri);
        let mut f = *ps.attempt_view().facts();
        f.ended = Some(true);
        assert!(ps.ingest_attempt_facts(a, Backend::Uri, f));
        assert_eq!(crate::player::ended(&ps), Some(true));
        ps.step_readout(ReadoutEvent::SessionReset);
        assert_eq!(crate::player::ended(&ps), Some(true), "a Feed reset leaves a URI attempt's fact alone");
    }

    fn seed(samples: u32) -> SeedReadout {
        SeedReadout { slow_kbps: 40_000, fast_kbps: 42_000, uncertainty_pm: 100, samples }
    }

    /// The worker's photograph is taken in as sent and a session reset (every reload and stop) returns
    /// it to idle; the carried link estimate survives the reset (a reload is the same item on the
    /// same link, I8) and only the playback's end clears it.
    #[test]
    fn the_photograph_dies_with_the_session_and_the_carried_estimate_with_the_playback() {
        let photograph = AbrReadout { net_kbps: 4_000, action: 5, target_kbps: 3_000, ..AbrReadout::IDLE };
        let r = stepped(&[ReadoutEvent::Abr(photograph), ReadoutEvent::AbrSeed(Some((seed(19), 10_000)))]);
        assert_eq!(*r.abr(), photograph);
        assert_eq!(r.abr_seed(10_000).map(|(s, unobserved)| (s.samples, unobserved)), Some((19, 0)));
        assert_eq!(r.abr_seed(15_000).map(|(_, unobserved)| unobserved), Some(5_000), "it ages on the frame tick");
        let reloaded = stepped(&[ReadoutEvent::Abr(photograph),
            ReadoutEvent::AbrSeed(Some((seed(19), 10_000))), ReadoutEvent::SessionReset]);
        assert_eq!(*reloaded.abr(), AbrReadout::IDLE, "the engine's photograph went with it");
        assert!(reloaded.abr_seed(10_000).is_some(), "the link estimate outlives a reload");
        let stopped = stepped(&[ReadoutEvent::AbrSeed(Some((seed(19), 10_000))),
            ReadoutEvent::SessionReset, ReadoutEvent::AbrSeed(None)]);
        assert_eq!(stopped.abr_seed(10_000), None, "a real stop clears it");
    }

    /// The colour-pick fact the media side delivers is a field of the readout: the menu reads it there,
    /// and a session reset (an engine torn down) leaves it, because it describes the subtitle registries.
    #[test]
    fn the_subtitle_ink_fact_is_a_readout_field_a_session_reset_leaves_alone() {
        assert_eq!(PlayerReadout::IDLE.subtitle_takes_colour(), None);
        let r = stepped(&[ReadoutEvent::SubtitleInk(Some(false))]);
        assert_eq!(r.subtitle_takes_colour(), Some(false));
        assert_eq!(stepped(&[ReadoutEvent::SubtitleInk(Some(false)), ReadoutEvent::SessionReset]).subtitle_takes_colour(), Some(false));
        assert_eq!(stepped(&[ReadoutEvent::SubtitleInk(Some(true)), ReadoutEvent::SubtitleInk(None)]).subtitle_takes_colour(), None);
    }

    const HTTP_503: FailureEdit = FailureEdit::Noted { kind: crate::player::ABR_FAILURE_ORIGINAL_HTTP, status: 503 };

    /// The Original failure is the playback's: a note sets the pair (a negative status reads as
    /// none), a clear drops it, and neither a session reset (the rollback reload that restores HLS
    /// is one) nor anything else but a clear touches it.
    #[test]
    fn the_original_failure_is_noted_cleared_and_survives_a_session_reset() {
        assert_eq!(PlayerReadout::IDLE.original_failure(), None);
        let noted = stepped(&[ReadoutEvent::OriginalFailure(HTTP_503)]);
        assert_eq!(noted.original_failure(), Some((crate::player::ABR_FAILURE_ORIGINAL_HTTP, 503)));
        let reloaded = stepped(&[ReadoutEvent::OriginalFailure(HTTP_503), ReadoutEvent::SessionReset,
            ReadoutEvent::SessionReset]);
        assert_eq!(reloaded.original_failure(), Some((crate::player::ABR_FAILURE_ORIGINAL_HTTP, 503)),
            "the rollback's reload keeps the cause it is explaining");
        let cleared = stepped(&[ReadoutEvent::OriginalFailure(HTTP_503), ReadoutEvent::SessionReset,
            ReadoutEvent::OriginalFailure(FailureEdit::Cleared)]);
        assert_eq!(cleared.original_failure(), None, "a real stop's own event clears it");
        let renoted = stepped(&[ReadoutEvent::OriginalFailure(HTTP_503), ReadoutEvent::OriginalFailure(
            FailureEdit::Noted { kind: crate::player::ABR_FAILURE_ORIGINAL_OPEN, status: -1 })]);
        assert_eq!(renoted.original_failure(), Some((crate::player::ABR_FAILURE_ORIGINAL_OPEN, 0)),
            "a later note replaces the pair, and a negative status is no status");
    }

    /// The pump's own note (a refused Original open, on the main thread) is the same event the worker's
    /// edit is: the pair is the readout's whichever thread noticed.
    #[test]
    fn the_players_note_of_a_refused_original_is_a_readout_event() {
        let mut ps = PlaybackSession::IDLE;
        crate::player::note_original_failure(&mut ps, crate::player::ABR_FAILURE_ORIGINAL_HTTP, 404);
        assert_eq!(ps.readout().original_failure(), Some((crate::player::ABR_FAILURE_ORIGINAL_HTTP, 404)));
        crate::player::note_original_failure(&mut ps, crate::player::ABR_FAILURE_ORIGINAL_OPEN, 0);
        assert_eq!(ps.readout().original_failure(), Some((crate::player::ABR_FAILURE_ORIGINAL_OPEN, 0)));
    }

    /// The failure edit is addressed like the photograph: a late worker of a previous attempt changes
    /// nothing.
    #[test]
    fn the_workers_failure_edit_is_addressed_by_its_attempt_and_backend() {
        let _serial = plx_base::testlock::serial();
        let (a, b, c) = (RouteStartAttempt::fixture(), RouteStartAttempt::fixture_n(2), RouteStartAttempt::fixture_n(3));
        let mut ps = PlaybackSession::IDLE;
        ps.begin_attempt_view(a, Backend::Feed);
        assert!(ps.ingest_original_failure(a, HTTP_503));
        assert_eq!(ps.readout().original_failure().map(|(_, status)| status), Some(503));
        ps.begin_attempt_view(b, Backend::Feed);
        assert!(!ps.ingest_original_failure(a, FailureEdit::Cleared), "a stale attempt is refused");
        assert_eq!(ps.readout().original_failure().map(|(_, status)| status), Some(503), "its clear did not land");
        ps.begin_attempt_view(c, Backend::Uri);
        assert!(!ps.ingest_original_failure(c, FailureEdit::Cleared), "the worker is the Feed's");
    }

    /// The two members of the photograph an attempt's facts carry follow the sparse form: `None`
    /// until a positive value, as the feed's own sample mapped them.
    #[test]
    fn the_attempt_facts_take_the_photographs_media_rate_and_reserve_as_positive_or_none() {
        use plx_playback_contract::readout::AttemptFacts;
        let mut facts = AttemptFacts { media_kbps: Some(7), abr_buffer_ms: Some(7), ..Default::default() };
        PlayerReadout::IDLE.fill_abr_facts(&mut facts);
        assert_eq!((facts.media_kbps, facts.abr_buffer_ms), (None, None), "-1 is unknown");
        let r = stepped(&[ReadoutEvent::Abr(AbrReadout { media_kbps: 2_500, buffer_ms: 0, ..AbrReadout::IDLE })]);
        r.fill_abr_facts(&mut facts);
        assert_eq!((facts.media_kbps, facts.abr_buffer_ms), (Some(2_500), None), "0 ms is not a reserve");
    }

    /// The worker's photograph is addressed like the sample: only the attempt the view belongs to,
    /// and only for the Feed, can deliver it. A late worker of a previous attempt changes neither
    /// the photograph nor the carried estimate.
    #[test]
    fn the_workers_photograph_is_addressed_by_its_attempt_and_backend() {
        let _serial = plx_base::testlock::serial();
        let (a, b, c) = (RouteStartAttempt::fixture(), RouteStartAttempt::fixture_n(2), RouteStartAttempt::fixture_n(3));
        let mut ps = PlaybackSession::IDLE;
        ps.begin_attempt_view(a, Backend::Feed);
        assert!(ps.ingest_abr(a, net_readout(4_000), Some((seed(19), 0))));
        assert_eq!(ps.readout().abr().net_kbps, 4_000);
        ps.begin_attempt_view(b, Backend::Feed);
        assert!(!ps.ingest_abr(a, net_readout(9), Some((seed(3), 0))), "a stale attempt is refused");
        assert_eq!(ps.readout().abr().net_kbps, 4_000, "its photograph did not reach the readout");
        assert_eq!(ps.readout().abr_seed(0).map(|(s, _)| s.samples), Some(19), "nor its estimate");
        ps.begin_attempt_view(c, Backend::Uri);
        assert!(!ps.ingest_abr(c, net_readout(9), None), "the photograph is the Feed's");
    }

    fn net_readout(kbps: i64) -> AbrReadout { AbrReadout { net_kbps: kbps, ..AbrReadout::IDLE } }

    /// `report.rs` reads the media rate and the reserve off the attempt's facts, and the attempt view
    /// keeps them past a session reset. The sample's facts no longer carry them (the feed does not
    /// see the worker's measurements), so the session lays the photograph's two members over each
    /// sample it ingests.
    #[test]
    fn a_sample_ingested_after_a_photograph_carries_its_media_rate_and_reserve() {
        let _serial = plx_base::testlock::serial();
        let a = RouteStartAttempt::fixture();
        let mut ps = PlaybackSession::IDLE;
        ps.begin_attempt_view(a, Backend::Feed);
        assert!(ps.ingest_feed_facts(a, facts(5, Some(100))));
        let view = ps.attempt_view().facts();
        assert_eq!((view.media_kbps, view.abr_buffer_ms), (None, None), "nothing measured yet");
        assert!(ps.ingest_abr(a, AbrReadout { media_kbps: 2_500, buffer_ms: 8_000, ..AbrReadout::IDLE }, None));
        assert!(ps.ingest_feed_facts(a, facts(6, Some(100))));
        let view = ps.attempt_view().facts();
        assert_eq!((view.media_kbps, view.abr_buffer_ms), (Some(2_500), Some(8_000)));
        ps.step_readout(ReadoutEvent::SessionReset);
        assert_eq!((ps.attempt_view().facts().media_kbps, ps.attempt_view().facts().abr_buffer_ms),
            (Some(2_500), Some(8_000)), "the terminal facts outlive the reset");
    }
}
