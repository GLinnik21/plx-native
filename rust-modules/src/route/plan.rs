//! The PURE half of the route split (spec §9, §15.2): functions of their arguments alone, plus
//! the plain data types they need. Nothing here reads a `static mut`, touches [`super::decision`]'s
//! `SESSION`/`PLAYER_CONTROL`/`PLAY_SLOT`/`ENCODER_CLEANUP`/`SCROBBLE_JOIN`/`TIMELINE_STOP_FENCE`/
//! `QUALITY`, or calls `task::spawn*` — that is what makes [`build_stream`] safe to run on the
//! resolve worker. Everything else in the former `route.rs` (session state, the synchronized
//! `PlayerControl`, PMS/native I/O, the encoder/scrobble/timeline machinery) lives in
//! [`super::decision`]. `ci/check-deps.sh`'s `wall` gate holds this file to zero
//! `Instant::now`/`SystemTime::now`/`.elapsed()`; a function that needs wall time is not pure and
//! belongs in `decision.rs`.

use crate::metadata::lang_matches;
use crate::plex::ServerId;
use std::sync::atomic::Ordering;

use super::decision::{
    measure_remote_original, measure_remote_remux, put_selection, resolve_playqueue,
    server_decision, forced_server_decision, ActiveEncoderState, AutomaticRouteIntent, MdeVerdict, PlayerControl,
    ENCODER_GENERATION,
};

/// One worker's right to observe or replace the active route. Both fields are required: `encoder`
/// addresses PMS, while `epoch` distinguishes semantic routes which intentionally reuse that
/// exact Streaming Resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RouteLease {
    pub(super) epoch: u64,
    pub(super) encoder: String,
}


impl RouteLease {
    pub(crate) fn encoder(&self) -> &str {
        &self.encoder
    }
}


/// Everything a media worker must still own before it may publish a route-affecting result.
/// `route` rejects same-id ABA, `engine_epoch` rejects a worker from an earlier Load,
/// `media_epoch` rejects evidence collected before an applied seek, and `applied_revision`
/// names the physical route contract this worker actually serves. Desired user edits deliberately
/// do not change this ticket until their PMS/native effect commits: a refusal must leave the
/// unchanged worker authorized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkerTicket {
    pub(super) route: RouteLease,
    pub(super) engine_epoch: u64,
    pub(super) media_epoch: u64,
    pub(super) applied_revision: u64,
}


impl WorkerTicket {
    pub(crate) fn encoder(&self) -> &str {
        self.route.encoder()
    }
}


/// Identity of one physical `sf_load` attempt inside a prepared route transaction. Attempts are
/// never reused: a late result from A cannot settle retry B even though both open the same URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RouteStartAttempt {
    pub(super) serial: u64,
    pub(super) attempt: u64,
}


impl RouteStartAttempt {
    #[cfg(all(test, feature = "hostsim"))]
    pub(crate) const fn fixture() -> Self {
        Self {
            serial: 1,
            attempt: 1,
        }
    }
}


pub(super) fn next_route_epoch(epoch: u64) -> u64 {
    let next = epoch.wrapping_add(1);
    if next == 0 {
        1
    } else {
        next
    }
}


pub(super) fn lease_of(active: &ActiveEncoderState) -> RouteLease {
    RouteLease {
        epoch: active.epoch,
        encoder: active.id.clone(),
    }
}


pub(super) fn next_generation(value: u64) -> u64 {
    let next = value.wrapping_add(1);
    if next == 0 {
        1
    } else {
        next
    }
}


pub(super) fn worker_ticket_of(control: &PlayerControl) -> WorkerTicket {
    WorkerTicket {
        route: lease_of(&control.active),
        engine_epoch: control.engine_epoch,
        media_epoch: control.media_epoch,
        applied_revision: control.applied_revision,
    }
}


pub(super) fn ticket_is_current(control: &PlayerControl, ticket: &WorkerTicket) -> bool {
    ticket == &worker_ticket_of(control)
}


pub(super) fn automatic_ticket(intent: &AutomaticRouteIntent) -> &WorkerTicket {
    match intent {
        AutomaticRouteIntent::OriginalToHls { ticket, .. }
        | AutomaticRouteIntent::HlsToOriginal { ticket, .. } => ticket,
    }
}


pub(super) fn next_encoder_generation() -> u64 {
    ENCODER_GENERATION.fetch_add(1, Ordering::Relaxed) + 1
}


/// The audio track a route has decided to carry, frozen at the moment of decision (issue #266's
/// vocabulary). Used as `Option<CarriedAudio>` everywhere: `None` means "server default, facts
/// unknown", and every reader fails CLOSED on it — an unknown track is never treated as
/// loudness-capable or immersive just because nobody has said otherwise.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct CarriedAudio {
    /// PMS `Stream.id` — the id this track is selected/burned by (`audioStreamID`,
    /// `PUT /library/parts`).
    pub(crate) sid: i64,
    /// The demuxer-facing ordinal (`metadata::Stream.index` order) — what `set_audio_track` feeds
    /// on a direct play. `-1` mirrors the pre-refactor "unknown" sentinel.
    pub(crate) ordinal: i32,
    pub(crate) codec: String,
    pub(crate) channels: i64,
    /// PMS 1.43.4+ Plex Pass: can the server honor `boostDialog`/`normalizeLoudness` for this
    /// track (issue #266; wire field is `plex::Stream::can_normalize_loudness`).
    pub(crate) can_normalize_loudness: bool,
    /// Dolby Atmos (`metadata::Stream::has_atmos`) — the Load payload's `contents.immersive`.
    pub(crate) immersive: bool,
}

impl CarriedAudio {
    /// The constructor for a track whose facts were read: a copy of a `metadata::Stream` this
    /// resolve actually fetched and looked at, never assembled field-by-field, so nothing can hand
    /// out capability facts for a track nobody read. (The other is [`named`](Self::named).)
    pub(crate) fn from_stream(s: &crate::metadata::Stream, ordinal: i32) -> Self {
        CarriedAudio {
            sid: s.id,
            ordinal,
            // Lowercased like every other codec this module compares (`audio_sel`, the payload):
            // PMS ids are lowercase already, and a stray capital must not make a recovery's Load
            // payload disagree with the direct play it restores.
            codec: s.codec.to_lowercase(),
            channels: s.channels,
            can_normalize_loudness: s.can_normalize_loudness,
            immersive: s.has_atmos(),
        }
    }

    /// A track the route NAMES (`sid`, e.g. a retry's or the session's earlier pick) whose stream
    /// this resolve never read — its track list was not fetched or does not contain it. Only the
    /// id is known: no codec, no channels, and every capability fact false, so every reader still
    /// fails closed. It exists so the playing session keeps reporting the id it asked for (the
    /// timeline's `audioStreamID`, a later rebuild's pick) exactly as before issue #266.
    pub(crate) fn named(sid: i64, ordinal: i32) -> Self {
        CarriedAudio {
            sid,
            ordinal,
            codec: String::new(),
            channels: 0,
            can_normalize_loudness: false,
            immersive: false,
        }
    }
}

/// The track a route carries, as [`CarriedAudio`] — the fetched stream whose id the route names on
/// the wire (`audioStreamID` / the PUT / the direct-play pick). `sid <= 0` (server default) or a
/// track the fetched list does not contain is `None`: facts unknown, and nothing is invented.
///
/// `immersive` survives only on a route that feeds the FILE's own audio (`feeds_source` — direct
/// play), for `Plan::immersive`'s reason: `contents.immersive` describes the elementary stream the
/// pipeline decodes, and on a remux or a transcode that is the server's output, not this track.
fn carried_track(
    tracks: &[crate::metadata::Stream],
    sid: i64,
    ordinal: i32,
    feeds_source: bool,
) -> Option<CarriedAudio> {
    let stream = tracks.iter().find(|t| sid > 0 && t.id == sid)?;
    let mut carried = CarriedAudio::from_stream(stream, ordinal);
    carried.immersive &= feeds_source;
    Some(carried)
}

/// The track a PLAN installs as `Session::cur_audio`: [`carried_track`] when the stream was read,
/// else [`CarriedAudio::named`] for a nonzero id the route still names on the wire. Unlike an
/// Original candidate (which stays `None`, so a recovery falls back to the source codec), the
/// playing session must keep the id it asked for even when nothing else about it is known.
fn plan_track(
    tracks: &[crate::metadata::Stream],
    sid: i64,
    ordinal: i32,
    feeds_source: bool,
) -> Option<CarriedAudio> {
    carried_track(tracks, sid, ordinal, feeds_source)
        .or_else(|| (sid != 0).then(|| CarriedAudio::named(sid, ordinal)))
}

/// Everything needed to restore Auto's zero-video-encode state after HLS. `url` is the cold-start
/// playback target; `probe_part` is the raw Part key used to bind runtime measurement and direct
/// playback to the exact live HLS Streaming Resource. `direct` says whether the Part itself is
/// playable or whether PMS must container-remux it while copying the video.
#[derive(Clone)]
pub(super) struct AutoOriginalCandidate {
    pub(super) url: String,
    pub(super) probe_part: String,
    pub(super) direct: bool,
    pub(super) vcodec: String,
    pub(super) fps: f64,
    pub(super) dovi: crate::metadata::Dovi,
    pub(super) dv_decision: crate::metadata::DvDecision,
    /// The audio track this candidate carries. `None` = server default, facts unknown (issue
    /// #266 fails closed on it: no enhancement is ever offered without a known, capable track).
    pub(super) audio: Option<CarriedAudio>,
    pub(super) subtitle_ordinal: Option<i32>,
}

impl AutoOriginalCandidate {
    /// Follow a mid-play audio pick on a Direct or Remux route (issue #266). These two methods are
    /// the ONLY writers of the candidate's audio and subtitle halves once it is installed.
    ///
    /// Why retarget instead of dropping, as the HLS family does: on Direct/Remux the candidate is
    /// also the way BACK from an enhanced remux (`EnhancementReleased`), so it must describe the
    /// track the viewer is hearing now — a release that restored the capture-time track would
    /// undo their pick. On HLS the candidate is only a recovery target and the old drop stays.
    ///
    /// The track is taken as a unit (sid, ordinal, codec, channels, capability, immersive), so the
    /// release's Load payload can never pair one track's ordinal with another's codec. `immersive`
    /// survives only on a direct candidate, for `carried_track`'s reason. `false` means the pick
    /// cannot be carried by this candidate — a direct candidate cannot feed a track the TV cannot
    /// decode — and the caller drops the candidate.
    pub(super) fn retarget_audio(&mut self, a: &CarriedAudio, direct_plays: bool) -> bool {
        if self.direct && !direct_plays {
            return false;
        }
        let mut carried = a.clone();
        carried.immersive &= self.direct;
        self.audio = Some(carried);
        true
    }

    /// Follow a mid-play subtitle pick (see [`retarget_audio`](Self::retarget_audio)). `ordinal`
    /// is `None` for Off and `Some(render ordinal)` for a pick; an external sidecar pick has no
    /// demuxer ordinal (`-1`), which is stored as `None` because `player::sidecar` draws it once
    /// the route is direct and `request_subtitle(-1)` never touches that renderer. A pick the
    /// client cannot draw would need a burn, which the Original route by definition does not do:
    /// `false`, and the caller drops the candidate.
    pub(super) fn retarget_subtitle(&mut self, ordinal: Option<i32>, client_renderable: bool) -> bool {
        if ordinal.is_some() && !client_renderable {
            return false;
        }
        self.subtitle_ordinal = ordinal.filter(|o| *o >= 0);
        true
    }
}


pub(super) fn source_probe_sample_outcome(
    sample: crate::curlio::ThroughputSample,
) -> crate::player::report::TraceOutcome {
    if sample.target_reached {
        crate::player::report::TraceOutcome::Succeeded
    } else {
        // A non-empty prefix is useful only as a right-censored observation. `curlio` currently
        // collapses the terminal deadline/read reason once bytes exist, so naming it successful
        // would be stronger than the evidence. Keep the trace honest until that result type grows
        // a terminal-cause field.
        crate::player::report::TraceOutcome::Inconclusive
    }
}


/// **Why [`HlsAbrControl::prime`] would not register a candidate encoder**, in the one distinction
/// the caller's backoff turns on. It maps straight onto `crate::abr::RejectCause` and is a
/// separate type only because `route` must not decide an ABR policy question — it reports which
/// exit it took, and `ff.rs` translates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrimeRefusal {
    /// The session moved underneath the request: the active encoder changed or the server client
    /// vanished. **Says nothing about the rung**, so it must not arm N11's backoff — the same
    /// reading `origin_changed` already gets one branch later.
    Session,
    /// The decision API completed without a usable decision: HTTP rejection, malformed success,
    /// or a transport failure. The typed request chain preserves each as non-deadline evidence;
    /// all three remain inconclusive about the rung and must not arm its backoff.
    Control,
    /// The caller-owned absolute snapshot actually stopped the PMS request. This is the only
    /// outcome eligible for a reserve retry; observing the clock after any other completed cause
    /// cannot manufacture it.
    Deadline,
    /// PMS was asked for this rung's ceiling and refused it. The one exit that IS about the
    /// candidate, and the one that should arm the backoff: re-proposing buys the same answer at
    /// the same price.
    Rung,
}


pub(super) fn classify_prime_decision(
    session_active: bool,
    outcome: crate::plex::JsonDeadlineOutcome,
) -> Result<crate::plex::MediaContainer, PrimeRefusal> {
    if !session_active {
        return Err(PrimeRefusal::Session);
    }
    match outcome {
        crate::plex::JsonDeadlineOutcome::Response {
            parsed: Some(decision),
            ..
        } => Ok(decision),
        crate::plex::JsonDeadlineOutcome::Response { parsed: None, .. }
        | crate::plex::JsonDeadlineOutcome::Transport => Err(PrimeRefusal::Control),
        crate::plex::JsonDeadlineOutcome::Deadline => Err(PrimeRefusal::Deadline),
    }
}


/// This playback's universal-transcoder spec, rebuilt from the module state (rk + session are
/// borrowed from the caller's locals; audio/subtitle ride the CURRENT selection) — so every
/// (re)start of the item's transcode carries identical params.
///
/// `contract` is an ARGUMENT rather than a read of [`quality`], for the same reason it always was
/// (back when it was four separate fields — `remux`, `no_video_copy`, `ceiling`, `delivery`):
/// [`build_stream`] runs on the resolve worker and must take it from [`ResolveEnv`], while
/// [`retranscode`] runs on the main thread and reads the live selection. A read inside here would
/// be a `static` touched from a worker. Folding the four (now five, with issue #266's
/// [`AudioEnhancements`](crate::plex::AudioEnhancements)) into one [`EncodeContract`] argument is
/// what keeps every caller stating the whole shape at once rather than four/five positional bools
/// and options a reader has to keep straight by position.
pub(super) fn transcode_spec<'a>(
    rk: &'a str,
    session: &'a str,
    encoder_session: &'a str,
    offset: crate::plex::TranscodeOffset,
    aud: i64,
    sub: i64,
    contract: crate::plex::EncodeContract,
) -> crate::plex::TranscodeSpec<'a> {
    crate::plex::TranscodeSpec {
        rating_key: rk,
        session,
        encoder_session,
        contract,
        audio_stream_id: aud,
        subtitle_stream_id: sub,
        offset,
    }
}


pub(crate) use crate::plex::session::{PlaybackQuality as Quality, DirectPlayMode};


/// The ladder IN ORDER, best first. The ONE place row order lives, so the picker's index mapping
/// cannot drift from what was drawn (`ui::more_menu`'s rule, and its bug).
pub(crate) const QUALITY_LADDER: [Quality; 7] = [
    Quality::Auto,
    Quality::Original,
    Quality::P1080High,
    Quality::P1080,
    Quality::P720,
    Quality::P720Low,
    Quality::P480,
];


/// The explicit support/readiness gate for automatic playback. The measured PMS contract,
/// segmented demux, per-encoder wire identity, prime/commit transaction and single-Load LG
/// resolution gate are all present. Keeping this named (instead of deleting it after launch)
/// preserves one fail-closed switch should a future protocol change invalidate that evidence.
pub(crate) const fn auto_quality_ready() -> bool {
    true
}


pub(super) fn quality_ladder_for(auto_ready: bool) -> &'static [Quality] {
    if auto_ready {
        &QUALITY_LADDER
    } else {
        &QUALITY_LADDER[1..]
    }
}


pub(super) fn supported_quality(q: Quality) -> Quality {
    if q == Quality::Auto && !auto_quality_ready() {
        Quality::Original
    } else {
        q
    }
}


/// **What the user's chosen ceiling allows a plan to ask for** — the same two flags
/// [`crate::plex::link_policy`] returns, deliberately, so [`build_stream`] can compose the two by
/// AND and the stricter always wins. A relay link cannot be loosened by picking a high rung, and a
/// low rung is not rescued by a fast link.
///
/// PURE, and the whole routing half of this feature is here:
///
/// * **Original restricts nothing** — the migration regression gate.
/// * **Auto includes Original as its top state.** `auto_original` is true immediately on a
///   verified LAN and only after a bounded file-throughput measurement on a direct Remote. A
///   relay, an unknown link, or an inconclusive/slow Remote measurement selects encoded HLS.
/// * A source MEASURED under the rung keeps both fast paths. Picking "1080p · 8 Mbps" must not
///   send a 3 Mbit/s 720p episode to an encoder; there is nothing there to fix.
/// * Anything else loses BOTH — direct play *and* the remux, for the one reason `link_policy`
///   already states twice: they ship the same bytes at the same rate, one container apart, and
///   neither carries a cap the server could come in under. What survives is the re-encode, which
///   is the only flavor that can honour the ask at all.
///
/// **Unmeasured fails CLOSED** ([`crate::plex::Ceiling::admits`] holds the full argument): `0` is
/// "the server did not say", and the only way to honour an explicit ask about a file you have not
/// measured is to route it where the server applies the bound for you. That is the opposite of
/// [`video_direct_plays`]'s unknown-passes rule, and deliberately so: a device bound is a
/// capability, a user ceiling is an instruction.
pub(super) fn quality_policy(
    q: Quality,
    auto_original: bool,
    src_kbps: i64,
    src_w: i64,
    src_h: i64,
) -> crate::plex::LinkPolicy {
    if q == Quality::Auto {
        return if auto_uses_hls(q, auto_original) {
            crate::plex::LinkPolicy {
                direct_play: false,
                remux: false,
            }
        } else {
            crate::plex::LinkPolicy::UNRESTRICTED
        };
    }
    match q.ceiling() {
        None => crate::plex::LinkPolicy::UNRESTRICTED,
        Some(c) if c.admits(src_kbps, src_w, src_h) => crate::plex::LinkPolicy::UNRESTRICTED,
        Some(_) => crate::plex::LinkPolicy {
            direct_play: false,
            remux: false,
        },
    }
}


pub(super) fn auto_uses_hls(q: Quality, auto_original: bool) -> bool {
    q == Quality::Auto && !auto_original
}


/// The shared source plan owns both the finite object and its conservation deadline. Keep this
/// narrow wrapper for the route tests and for converting Plex's signed bitrate into ABR units.
pub(super) fn remote_probe_plan(source_kbps: i64) -> Option<crate::abr::SourceProbePlan> {
    crate::abr::source_probe_plan(
        u32::try_from(source_kbps).ok()?,
        crate::abr::PROBE_BUDGET_MS,
    )
}


#[cfg(test)]
pub(super) fn remote_probe_target_bytes(source_kbps: i64) -> Option<usize> {
    remote_probe_plan(source_kbps).map(|plan| plan.target_bytes)
}

/// **Two ceilings mean the stricter one**, per flavor, and this is the only place the two are put
/// together. A ceiling can only ever REMOVE a flavor: a fast link cannot restore what a low rung
/// denied, and a high rung cannot restore what a relay denied.
///
/// A named function rather than two `&&`s inline at the decision site, so the composition the
/// tests grade is literally the composition [`build_stream`] runs — a re-implementation in a test
/// would agree with itself forever while the shipped path drifted.
pub(super) fn flavors_allowed(
    link: crate::plex::LinkPolicy,
    quality: crate::plex::LinkPolicy,
) -> crate::plex::LinkPolicy {
    crate::plex::LinkPolicy {
        direct_play: link.direct_play && quality.direct_play,
        remux: link.remux && quality.remux,
    }
}

/// The family of route a caller is BUILDING (or, mid-play, is on) — the only shape question the
/// Plex Pass audio enhancement (issue #266) asks. `Direct` = the raw Part; `Remux` = the
/// codec-preserving progressive-MKV copy; `Other` = every encoder rung, fixed quality, HLS and
/// relay. The enhancement decorates the Original route and nothing else (I5): M3 measured that on
/// a re-encode shape the params change nothing observable, because the audio is transcoded there
/// at baseline anyway.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum RouteFamily {
    Direct,
    Remux,
    Other,
}

/// Everything [`enhancements_offered`] reads, gathered by the caller from whichever side of the
/// playback it stands on (the resolve worker's locals, or the live session). Borrowed so the
/// predicate can never be handed a copy that has drifted from the thing it describes.
#[derive(Clone, Copy)]
pub(super) struct EnhancementFacts<'a> {
    /// The server's Plex Pass state (`serverinfo::subscription_of`), captured at the request.
    pub(super) pass: crate::plex::serverinfo::Subscription,
    /// The frozen non-enhanced Original route. `None` = Original was never feasible here (forced
    /// direct play, a fixed rung, relay, a non-Original MDE…), which is I5's whole exclusion list.
    pub(super) base: Option<&'a AutoOriginalCandidate>,
    /// The audio track the route carries. `None` = server default, facts unknown: fails closed.
    pub(super) carried: Option<&'a CarriedAudio>,
    /// A subtitle will be on screen — embedded pick, server-selected sidecar, or a burn (I6).
    pub(super) subtitle_shown: bool,
    /// This playback's server already refused or ignored the params once.
    pub(super) refused: bool,
}

/// **Is the Plex Pass audio enhancement OFFERED for a route of family `target`?** One predicate for
/// every caller — the resolve, recovery, and (later) the menu — so "the toggle is visible" and
/// "the resolve asks for it" can never disagree.
///
/// - **Plex Pass `Yes` only (I1).** `Unknown` fails closed exactly like `No`: an unknown server is
///   not assumed to hold a subscription it may not have, and with it hidden the app's URLs are
///   byte-identical to a build without the feature.
/// - **A known, capable carried track.** `canNormalizeLoudness` is PMS 1.43.4's own per-stream
///   statement that it has the loudness analysis the DSP needs; absent = no.
/// - **No Dolby Vision on the base route (I7).** A declared DV direct play needs its payload's
///   `DolbyHdrInfo` node, which rides the direct play and would be lost in a remux; an unusable
///   base layer is never copied at all (`EncodeContract::no_video_copy`).
/// - **No subtitle on screen (I6).** M4: an enhanced remux never carries a text subtitle into the
///   progressive MKV — PMS answers "unavailable", or re-encodes the video to burn it.
/// - **Not already refused** by this playback's server, and **Direct or Remux only (I5)**.
pub(super) fn enhancements_offered(f: &EnhancementFacts, target: RouteFamily) -> bool {
    use crate::plex::serverinfo::Subscription;
    f.pass == Subscription::Yes
        && f.base.is_some_and(|b| {
            b.dv_decision.presentation.declared().is_none() && !b.dovi.base_layer_unusable()
        })
        && f.carried.is_some_and(|a| a.can_normalize_loudness)
        && !f.subtitle_shown
        && !f.refused
        && matches!(target, RouteFamily::Direct | RouteFamily::Remux)
}

/// The ONLY source of `EncodeContract::audio`: the viewer's preference where the enhancement is
/// offered, `NONE` everywhere else. Every contract that reaches the wire passes through here, so
/// a preference can never leak onto a route the predicate refused.
pub(super) fn desired_audio(
    pref: crate::plex::AudioEnhancements,
    offered: bool,
) -> crate::plex::AudioEnhancements {
    if offered {
        pref
    } else {
        crate::plex::AudioEnhancements::NONE
    }
}

/// Will a subtitle be on screen for this item? The explicit pick, or the sidecar the direct-play
/// landing restores on its own (`metadata::server_selected_sidecar` — the same predicate
/// `player::sidecar::restore_server_selection` switches it on with). Mid-play the fact is
/// `cur_sub_sid != 0` instead; this is the resolve-time half.
pub(super) fn renders_subtitle(
    item: Option<&crate::metadata::PlayingItem>,
    sub_pick: Option<(i64, i32)>,
) -> bool {
    sub_pick.is_some() || item.is_some_and(|i| crate::metadata::server_selected_sidecar(i).is_some())
}

/// The flavour ceiling an enhancement imposes, composed ONLY through [`flavors_allowed`] — the same
/// door the link and the quality rung go through, so it can only ever remove a flavour. An
/// enhanced route is a remux by definition (M1: PMS answers the params with a Part transcode —
/// video copy, audio re-encoded with the DSP), so asking for one denies direct play and nothing
/// else.
pub(super) fn enhancement_policy(audio: crate::plex::AudioEnhancements) -> crate::plex::LinkPolicy {
    crate::plex::LinkPolicy {
        direct_play: !audio.any(),
        remux: true,
    }
}

/// What to do with the server's answer to an enhanced decision.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Fallback {
    Keep,
    /// Build the plan again without the enhancement: the server refused it, or ignored it.
    Retry,
}

/// **Pure: did the server honour the enhanced ask?** `Retry` when it refused outright
/// ([`refusal`]), or when the audio came back `copy` despite the params — an old or
/// non-conforming PMS that dropped them silently, which would otherwise play the non-enhanced
/// audio one container down while claiming the enhancement. An unreachable decision (`None`) is
/// `Keep`: the same "no answer is not a refusal" rule every other transcode start follows, and
/// the outcome is then graded `Unverified` by [`classify_outcome`].
pub(super) fn enhancement_fallback(
    mc: Option<&crate::plex::MediaContainer>,
    audio: crate::plex::AudioEnhancements,
) -> Fallback {
    if audio.is_none() {
        return Fallback::Keep;
    }
    let Some(mc) = mc else {
        return Fallback::Keep;
    };
    if refusal(mc).is_some() || decision_audio(mc) == Some("copy") {
        Fallback::Retry
    } else {
        Fallback::Keep
    }
}

/// The audio lane's own stream decision off a `/decision` body (`copy`/`transcode`), if it says.
fn decision_audio(mc: &crate::plex::MediaContainer) -> Option<&str> {
    mc.metadata
        .first()
        .and_then(|m| m.media.first())
        .and_then(|md| md.part.first())
        .and_then(|p| p.stream.iter().find(|s| s.stream_type == 2))
        .map(|s| s.decision.as_str())
}

/// Grade a kept enhanced decision (issue #266's `EnhancementOutcome`). `Applied` only when the
/// server transcoded audio this profile would otherwise have COPIED — the one case where the
/// transcode is provably the DSP's doing (M2's AC3 2.0). A carried track outside the copy list
/// (`plex::is_dp_audio_track`, the same caps the profile's `audioCodec`/`audio.channels` lanes are
/// built from) is transcoded at baseline too, so the ask was delivered but cannot be told apart
/// from what would have happened anyway: `Unverified` (M2's AAC 5.1 under the measured profile).
/// No answer at all is `Unverified` for the same reason.
pub(super) fn classify_outcome(
    mc: Option<&crate::plex::MediaContainer>,
    carried: Option<&CarriedAudio>,
    audio: crate::plex::AudioEnhancements,
) -> super::decision::EnhancementOutcome {
    use super::decision::EnhancementOutcome;
    if audio.is_none() {
        return EnhancementOutcome::Off;
    }
    let copyable = carried.is_some_and(|a| crate::plex::is_dp_audio_track(&a.codec, a.channels));
    match mc.and_then(decision_audio) {
        Some("transcode") if copyable => EnhancementOutcome::Applied,
        _ => EnhancementOutcome::Unverified,
    }
}


/// Read the transcoder's OUTPUT codecs from a /decision response and store them as the stream
/// codecs the Load payload is built from. The decision's Part.Stream[].codec is the codec each
/// lane will actually ARRIVE in (it equals the source codec only when that lane is copied).
/// Assuming "a container remux copies the audio" broke mp4 items whose audio PMS re-encodes to
/// the transcode-target's AC3: the payload said AAC, the stream carried AC3, and the
/// configured-for-AAC pipeline played silence (the `movie_hevc_aac_mp4` harness case).
/// PURE: the codec pair the server's /decision OUTPUT actually declares, or None if it names
/// neither. The Load payload must match this, not the source file — a transcode changes the
/// codec and rate, and describing the source to the decoder gives silent audio.
pub(super) fn decision_codecs(mc: &crate::plex::MediaContainer) -> Option<(String, String)> {
    let streams = mc
        .metadata
        .first()
        .and_then(|m| m.media.first())
        .and_then(|md| md.part.first())
        .map(|p| &p.stream)?;
    let (mut vc, mut ac) = (None, None);
    for s in streams {
        match s.stream_type {
            1 if vc.is_none() && !s.codec.is_empty() => vc = Some(s.codec.to_lowercase()),
            2 if ac.is_none() && !s.codec.is_empty() => ac = Some(s.codec.to_lowercase()),
            _ => {}
        }
    }
    match (vc, ac) {
        (Some(v), Some(a)) => Some((v, a)),
        _ => None,
    }
}


/// `generalDecisionCode` 2000 — "Neither direct play nor conversion is available." The server has
/// adjudicated the whole request and can serve NEITHER lane; there is nothing left for the client
/// to try, which is what makes it a stop rather than another fallback.
pub(super) const DECISION_UNPLAYABLE: i64 = 2000;


/// **Why a plan leaves without a URL on purpose**, as a typed verdict rather than a sentence.
///
/// The server's own refusal is quoted verbatim ([`PlayVerdict::Server`]: PMS wrote it, in the
/// server's language, and it may be empty). Every other arm is the APP's policy decision, so it is
/// stored as a variant and worded only where it is read ([`PlayVerdict::text`]) — a stored sentence
/// would freeze one language into playback state that tests, logs and replays also read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlayVerdict {
    /// `/decision`'s own sentence, quoted and never translated. `""` when it gave none.
    Server(String),
    /// Direct Play is Disabled, and this stream can only be played as the original.
    DirectPlayDisabled,
    /// Force Direct Play is on, and this is why the original cannot play.
    Forced(ForcedFailure),
}

/// The limitation a Force Direct Play verdict names. Each one carries the same recovery step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ForcedFailure {
    NoOriginal,
    Container,
    Video,
    Audio,
    Unauthorized,
    /// The resolve reached the transcode path, which Force never takes.
    OpenFailed,
    /// A later audio-track pick needs conversion.
    AudioNeedsConversion,
}

impl PlayVerdict {
    /// The read-out's sentence: the server's verbatim, or the app's own from the catalog.
    pub(crate) fn text(&self) -> &str {
        use crate::i18n::msg;
        match self {
            Self::Server(sentence) => sentence,
            Self::DirectPlayDisabled => msg::widgets_verdict_direct_play_disabled(),
            Self::Forced(why) => match why {
                ForcedFailure::NoOriginal => msg::widgets_verdict_forced_no_original(),
                ForcedFailure::Container => msg::widgets_verdict_forced_container(),
                ForcedFailure::Video => msg::widgets_verdict_forced_video(),
                ForcedFailure::Audio => msg::widgets_verdict_forced_audio(),
                ForcedFailure::Unauthorized => msg::widgets_verdict_forced_unauthorized(),
                ForcedFailure::OpenFailed => msg::widgets_verdict_forced_open(),
                ForcedFailure::AudioNeedsConversion => msg::widgets_verdict_forced_audio_conversion(),
            },
        }
    }
}

/// PURE: the server's pre-flight refusal, or None.
///
/// `/decision` is asked BEFORE a byte of video moves, and it can answer "no" — verified live
/// against PMS 1.43.3 on a VP9 source: `generalDecisionCode 2000` beside
/// `transcodeDecisionCode 4007, "Cannot convert this item. Implementation for video encoder 'vp9'
/// not found."`. The app used to parse `general_decision_code` and only LOG it, then hand
/// `start.mkv` to the pipeline anyway — so a server that had already said no produced "Buffering…"
/// followed by a generic failure, and the one sentence that explained it was in a log the user
/// cannot reach.
///
/// **The CODE is authoritative and the text is only the human sentence.** Grading on the text would
/// be grading on server copy that is localised, versioned and free to change; grading on the code
/// is why a server that refuses without saying why still stops us (`Some("")`).
///
/// Of the two sentences the body carries, the TRANSCODE one is preferred: `generalDecisionText`
/// restates the code ("Neither direct play nor conversion is available") while
/// `transcodeDecisionText` names the actual cause. The general one is the fallback for a server
/// that sends only it.
pub(super) fn refusal(mc: &crate::plex::MediaContainer) -> Option<String> {
    if mc.general_decision_code != Some(DECISION_UNPLAYABLE) {
        return None;
    }
    let text = if !mc.transcode_decision_text.is_empty() {
        &mc.transcode_decision_text
    } else {
        &mc.general_decision_text
    };
    Some(text.trim().to_string())
}


/// Fresh opaque session id per playback. Reads the kernel UUID (the TV is Linux); falls
/// back to a ratingKey + monotonic-counter token if that read fails.
pub(super) fn new_sess(rk: &str) -> String {
    if let Ok(u) = std::fs::read_to_string("/proc/sys/kernel/random/uuid") {
        let t = u.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTR: AtomicU64 = AtomicU64::new(1);
    format!("plxnative-{rk}-{}", CTR.fetch_add(1, Ordering::Relaxed))
}


/// The episode queued after the one now playing — everything the Up Next control draws AND
/// everything [`request_play`] needs to start it, so playing it costs no PMS round trip either.
///
/// It comes free with the `continuous=1` PlayQueue every playback already creates (see
/// [`crate::plex::Client::create_play_queue`]); nothing here asks the server "what's next".
#[derive(Clone, Default)]
pub(crate) struct UpNext {
    pub(crate) rk: String,
    pub(crate) part: String,
    pub(crate) vcodec: String,
    pub(crate) acodec: String,
    pub(crate) show_title: String, // grandparentTitle
    pub(crate) ep_title: String,
    pub(crate) season: i64,
    pub(crate) index: i64,
    pub(crate) thumb: String,
    pub(crate) dur_ms: i64,
    pub(crate) resume_ms: i64,
}


/// Build the Up Next descriptor from a queue row. Episodes only: `continuous=1` on a movie
/// returns just the movie itself (verified live — total count 1), and "up next" is a show idea.
/// The gate belongs HERE, on the one-item control — the retained row list is deliberately not
/// episode-gated, because a queue list has to be able to show whatever the queue holds.
pub(super) fn up_next_of(r: &crate::plex::QueueRow) -> Option<UpNext> {
    if r.kind != "episode" || r.rk.is_empty() {
        return None;
    }
    Some(UpNext {
        rk: r.rk.clone(),
        part: r.part.clone(),
        vcodec: r.vcodec.clone(),
        acodec: r.acodec.clone(),
        show_title: r.show_title.clone(),
        ep_title: r.title.clone(),
        season: r.season,
        index: r.index,
        thumb: r.thumb.clone(),
        dur_ms: r.dur_ms,
        resume_ms: r.resume_ms,
    })
}


/// Every piece of [`Session`] the resolve used to READ, captured on the main thread and passed by
/// value.
///
/// Making the worker WRITE-pure was not enough: it still cloned `machine_id` and `sess` — Strings
/// that `apply_plan` reassigns on every landing — so a superseded worker could clone a buffer as
/// it was being dropped (heap corruption on a device with no debugger), and read the two sids as
/// non-atomic i64s, which on armv7 is a tearable two-word load.
///
/// The `sid` is the same idea one step further out: it is not a static the worker could read, it is
/// a *function call* — `plex::client_opt()` — which is worse, because `Send` cannot see a function
/// call and a worker that resolves its own server therefore compiles clean and passes every test.
/// It is captured here, at the request, and every PMS call the worker makes is `client_for(sid)`.
#[derive(Clone, Default)]
pub(crate) struct ResolveEnv {
    /// WHICH SERVER this playback's item lives on — the scope for every server-local key the
    /// resolve then uses (`rk`, `Part.key`, `Stream.id`). Not "the current server" (see
    /// [`Session::cur_sid`]): captured on the main thread with everything else here, because the
    /// resolve worker must not read the current server itself.
    pub sid: ServerId,
    /// `machine_id`, but only when it was learned from `sid`'s own server (`machine_sid`);
    /// otherwise empty, so the worker re-asks rather than addressing a queue to the wrong machine.
    pub machine_id: String,
    pub audio_sid: i64,
    pub sub_sid: i64,
    /// A retry carries the viewer's explicit Off as well as a positive subtitle id.
    pub subtitle_override: Option<i64>,
    /// the loaded detail's streams when it IS this item — saves the worker a GET
    pub cached_item: Option<crate::metadata::PlayingItem>,
    /// The user's pick off the quality ladder, captured at the press like everything else here.
    /// The worker must not call [`quality`] itself for the reason this struct exists: it reads a
    /// process-global the main thread can move while the resolve is in flight.
    pub quality: Quality,
    /// Captured with the quality preference; never re-read by a worker.
    pub direct_play_mode: DirectPlayMode,
    /// The SOURCE's whole-stream bitrate in **kbps**, or `0` when nobody has measured it — the
    /// other half of what [`quality_policy`] needs, beside the frame size the playing-item store
    /// already carries.
    ///
    /// It comes off the LOADED DETAIL (`metadata::current().bitrate`, `Media[0]`) when that detail
    /// is this item, which is the ordinary path: a card's OK opens the detail page and Play is
    /// pressed there. **Playing straight from a shelf leaves it `0`**, and `0` fails closed (see
    /// [`crate::plex::Ceiling::admits`]) — so with a rung selected, such a play routes to the
    /// re-encode rather than guessing the file is small enough. Carrying the bitrate on
    /// `PlayingItem` instead would measure every path, and is named as the follow-up in this
    /// unit's PR: that store is `metadata.rs`'s, not this lane's.
    pub src_kbps: i64,
    /// Trailer sessions omit `continuous=1` so EOS cannot Up-Next into a sibling extra.
    pub omit_queue_continuous: bool,
    /// Hero preview. Skip the PlayQueue entirely, and refuse anything that is not a direct play.
    pub preview: bool,
    /// The viewer's Plex Pass audio-DSP preference (`player::audio_enhancements`), captured at the
    /// request like the quality: the worker must not read the atomic the main thread moves. Only
    /// ever reaches the wire through [`desired_audio`]. A preview and a start-failure retry
    /// capture `NONE` (`request_play_inner`).
    pub audio_enhancements: crate::plex::AudioEnhancements,
    /// `serverinfo::subscription_of(sid)` at the request — for the OFFERING of the enhancement only
    /// (I1/I8), never a flavour or profile input.
    pub pass: crate::plex::serverinfo::Subscription,
    /// Test-only replacement for the process cache. Capability is an explicit policy input in
    /// regressions; no test mutates the production `OnceLock` or makes the whole host a DV set.
    #[cfg(test)]
    pub(super) dv_capability: Option<crate::webos::caps::DvCapability>,
}


/// Does the loaded detail describe the leaf `rk` is about to play?
///
/// **Its own ratingKey, OR its on-deck episode's** — and the second half is not an optimisation.
/// A SHOW's `Detail.rk` is the show's key while the play `rk` is the EPISODE's, so an rk-only test
/// (which is all `cached_playing` needs, because it is fetching stream lists a show container does
/// not have) never matches on the commonest path in the app: press Play on a show page. With a
/// rung selected that put every episode in the library into the "unmeasured, fail closed" bucket
/// while [`playback_preview`] — which reads the same `Detail`'s numbers directly — still promised
/// Direct Play for it. Two answers to one question, which is the mismatch that preview exists to
/// prevent.
///
/// The show's technical fields ARE the on-deck episode's: `metadata::fetch_item_streams` backfills
/// them from exactly the leaf `playback_preview` answers for. An episode reached some OTHER way (a
/// season list, Up Next) still measures 0 and still fails closed — honest, and the residue that
/// `PlayingItem` carrying its own bitrate would close (`ResolveEnv::src_kbps`).
///
/// The SERVER half of the test is load-bearing on both arms: a ratingKey names an item only within
/// one server, so a bare-rk match against a colliding item on the other machine would hand the
/// ceiling the wrong file's bitrate.
pub(super) fn detail_describes(d: &crate::metadata::Detail, sid: ServerId, rk: &str) -> bool {
    crate::plex::same_item((d.sid, &d.rk), (sid, rk))
        || d.on_deck
            .as_ref()
            .is_some_and(|ep| crate::plex::same_item((d.sid, &ep.rk), (sid, rk)))
}


/// The source rate to judge against a ceiling, in kbps: **the VIDEO stream's own**, falling back
/// to the whole-file figure.
///
/// The distinction is the units the ceiling is spent in. `Ceiling::max_kbps` ships as
/// `maxVideoBitrate`, which bounds the VIDEO lane alone, while `Detail::bitrate` is `Media[0]`'s
/// whole-stream number — video plus every audio track. Comparing the second against the first
/// makes each rung bite about one AC-3 track early: a 7.9 Mbit/s video beside a 640 kbit/s track
/// measures 8.5 and loses direct play to the "1080p · 8 Mbps" rung, for an encode that would then
/// be capped at a rate its video already met.
///
/// `Detail::video` is the stream's own record and carries its own bitrate; it is `None` for a show
/// that never got an episode backfill and for an audio-only part, and PMS omits the field often
/// enough that the whole-file fallback has to stay. Falling back is the conservative direction,
/// which is the right one here — see [`crate::plex::Ceiling::admits`].
pub(super) fn source_kbps(d: &crate::metadata::Detail) -> i64 {
    match d.video.as_ref().map(|v| v.bitrate) {
        Some(b) if b > 0 => b,
        _ => d.bitrate,
    }
}

/// Rate the quality ceiling judges for this play. A trailer extra is a different file from the
/// loaded parent: using the movie's 4K figure (or 0, which [`crate::plex::Ceiling::admits`]
/// fails closed on) would force every non-Auto rung through the encoder.
pub(super) fn resolve_src_kbps(
    d: Option<&crate::metadata::Detail>,
    sid: ServerId,
    rk: &str,
) -> i64 {
    let Some(d) = d else {
        return 0;
    };
    if let Some(extra) = d
        .extras
        .iter()
        .find(|e| crate::plex::same_item((d.sid, e.rk.as_str()), (sid, rk)))
    {
        return extra.bitrate;
    }
    if detail_describes(d, sid, rk) {
        source_kbps(d)
    } else {
        0
    }
}


/// Everything `resolve` DECIDES, as owned data. No `static mut`, no `SHARED`, no ACB/Starfish —
/// so it is `Send` and the resolve can run on a worker. `apply_plan` (main thread) is the ONLY
/// code that installs it. Adding a field here is how you add a resolve output; writing a static
/// from the worker is how you reintroduce the races the audit found.
#[derive(Default)]
pub(crate) struct Plan {
    pub direct_play_mode: DirectPlayMode,
    /// The server this plan was resolved against — copied straight from [`ResolveEnv::sid`], so
    /// what `apply_plan` installs as `cur_sid` is the id the request captured and not a re-read of
    /// whatever became current while the worker ran. `UNSET` only on the default `Plan` a panicking
    /// resolve lands, which carries no URL either and so never starts an engine.
    pub sid: ServerId,
    pub url: String,
    pub tsession: String,
    pub sess: String,
    pub part_id: i64,
    pub pq_id: String,
    pub pq_item_id: String,
    pub machine_id: String, // "" = leave the cached one alone
    pub vcodec: String,
    pub acodec: String,
    /// The SOURCE file's codecs, kept beside the ones above because on a transcode those are the
    /// server's OUTPUT. "hevc → h264" is the whole server-side transform, and it is invisible if
    /// only one half is recorded. Equal to `vcodec`/`acodec` for a direct play and for a remux.
    pub src_vcodec: String,
    pub src_acodec: String,
    pub fps: f64,
    /// The direct-played file's Dolby Vision layering, for the Load payload's `DolbyHdrInfo`
    /// node. Set on the DIRECT-PLAY branch only, beside `fps` and for the same reason: the
    /// transcode branch's payload describes the server's OUTPUT, which is not this file.
    pub dovi: crate::metadata::Dovi,
    /// The direct-play declaration resolved from one cached capability snapshot. Payload builds,
    /// reloads and recovery consume this stored answer; remux/transcode leave it at `NONE`.
    pub dv_decision: crate::metadata::DvDecision,
    /// Does the direct-played audio track carry Dolby Atmos, for the Load payload's
    /// `contents.immersive` node. Set on the DIRECT-PLAY branch only, for the same reason `dovi`
    /// is: it describes the FILE's own elementary stream.
    pub immersive: bool,
    /// The encode's flavor/delivery/ceiling/audio-DSP shape — see [`crate::plex::EncodeContract`].
    /// Was four independent fields (`remux`, `delivery`, `no_video_copy`, `ceiling`) until issue
    /// #266 needed a fifth that only ever means anything alongside `remux == true`; one value
    /// installed as [`Session::cur_contract`] is what lets a seek or a track switch rebuild the
    /// exact same query instead of four/five fields that could drift out of the coupling that
    /// matters (`no_video_copy` only under re-encode, `ceiling` only under re-encode, `audio` only
    /// under remux — see the field docs on `EncodeContract` itself).
    pub contract: crate::plex::EncodeContract,
    /// The audio track this plan carries, installed as `Session::cur_audio` (the one source of the
    /// timeline's `audioStreamID`). `CarriedAudio::from_stream` of the fetched stream the route
    /// names; [`CarriedAudio::named`] (id only, every fact unknown) when the route names an id
    /// whose stream was never read; `None` when it names no track (server default).
    pub audio: Option<CarriedAudio>,
    /// What the server did with the requested audio enhancement (`contract.audio`), installed as
    /// `Session::cur_enhancement`: `Off` unless an enhanced decision was asked for.
    pub(super) enhancement: super::decision::EnhancementOutcome,
    /// What this plan MEASURED the source at — `(kbps, w, h)`, any of them `0` for "nobody said".
    /// Carried so [`set_quality`] can re-ask [`quality_policy`] for the item already playing when
    /// the user picks a different rung, instead of guessing. See [`Session::cur_src`].
    pub src_measure: (i64, i64, i64),
    /// Whole-file wire rate used by Auto's runtime Original watchdog (video + audio).
    pub transport_kbps: i64,
    /// `video_direct_plays` for this source — see [`Session::cur_source_decodable`].
    ///
    /// **`bool::default()` is the wrong default and it is not a style point.** `false` is the
    /// claim "this television cannot decode the source", which the quality menu renders as a line
    /// of copy; `build_stream` has an exit that returns before the gate runs at all. So the
    /// initializer sets `true` explicitly and the gate overwrites it, which makes every exit carry
    /// something that was either measured or honestly absent.
    pub source_decodable: bool,
    /// This plan admitted Original specifically on a measured direct Remote link.
    pub auto_original_watched: bool,
    /// What the startup probe measured, kept so the live estimator can be SEEDED with it instead
    /// of starting from nothing — and so a later mode transition can hand the next worker the same
    /// evidence. `0` when this plan never probed (Local, Relay, a fixed rung, or Original).
    pub auto_prior_kbps: u32,
    /// Bootstrap's already-decided HLS contingency, retained even when the immediate route is
    /// Original. See [`Session::auto_bootstrap_rung`].
    pub auto_bootstrap_rung: Option<crate::abr::Rung>,
    /// A measured Remote can begin on HLS and later recover. Preserve the exact no-video-encode
    /// source declaration even when this plan's immediate output is H264/AAC HLS.
    pub(super) auto_original: Option<AutoOriginalCandidate>,
    /// demuxer stream ordinal to feed (direct-play, non-default track). None = leave as-is.
    pub feed_audio_ordinal: Option<i32>,
    /// the subtitle selected for this part or by the show preference (0 = none/off), so the
    /// menu checkmark and the timeline report agree with what is on screen — and a later
    /// transcode of this item burns the subtitle the user was already watching.
    pub sub_sid: i64,
    /// client-renderer ordinal for that subtitle (`metadata::sub_render_ordinal`). None = subs off.
    pub sub_render_ordinal: Option<i32>,
    /// The subtitle-language preference this play resolved under — the SHOW's own pref if it set
    /// one, else the ACCOUNT's — as a BCP-47 code, for the Subtitles menu's "yours" grouping
    /// (`metadata::sub_layout::sub_sections`). `ui/` sees only this code, never a Plex account type.
    pub sub_pref_lang: Option<String>,
    /// the playing item's track store, fetched off-thread and installed by apply_plan
    pub playing: Option<crate::metadata::PlayingItem>,
    /// The server's PRE-FLIGHT refusal (see [`refusal`]), when `/decision` said it can neither
    /// direct play nor convert this item. A plan carrying one has an EMPTY `url` by construction —
    /// that is how it fails, on the same path as every other unresolvable plan — and the sentence
    /// rides along so the read-out can quote the server instead of guessing. `None` on every other
    /// plan, including one that simply failed to reach the server.
    pub verdict: Option<PlayVerdict>,
    /// the episode queued after this one, straight off the `continuous=1` PlayQueue
    pub up_next: Option<UpNext>,
    /// that same PlayQueue's whole returned window, projected on the worker (see `queue`)
    pub queue: Vec<crate::plex::QueueRow>,
}


/// Pick the stream URL for an item: direct-play only what the pipeline decodes natively (H264/
/// HEVC + a direct-playable audio track); else ask the server to remux or transcode into
/// progressive MKV. On the transcode path this also runs the /decision handshake.
///
/// PURE: runs on the resolve worker. It must neither WRITE nor READ any `static mut` — every
/// input arrives in `ResolveEnv`, every output leaves in `Plan`, and `apply_plan` installs both
/// on the main thread. Write-purity alone is not enough: `apply_plan` reassigns the `machine_id`
/// and `sess` Strings, so a still-running superseded worker reading them is a use-after-free.
///
/// **And it must not ask which server is current.** `plex::client_opt()` / `plex::current_server()`
/// are not statics, they are calls, so nothing in the type system stops a worker making one — but
/// the answer is "whatever the user is looking at NOW", which for an item from a shared source is
/// the wrong authority for every id in this function. The server arrives in `env.sid` and the only
/// client here is `client_for` of it.
pub(super) fn build_stream(rk: &str, part: &str, vcodec: &str, acodec: &str, env: &ResolveEnv) -> Plan {
    // The part id is derived from THIS call's `part`, before anything else runs, and published
    // here rather than by the caller after we return. It used to be written by play_movie /
    // play_episode *after* build_stream finished, so `put_selection` — which runs inside this
    // function — read the PREVIOUS item's part (or 0, and silently skipped, on the first play
    // of the process). Every non-MKV item takes the remux branch, so that mis-targeted PUT
    // failed to suppress a server-default subtitle and burned it into the transcode.
    // The arguments ARE the source codecs, whatever this function goes on to choose — captured
    // once, here, so no later branch has to remember to.
    let mut plan = Plan {
        // carried through every exit below, the failing ones included: a plan without a server is
        // a plan `apply_plan` cannot install an honest `cur_sid` from.
        sid: env.sid,
        direct_play_mode: env.direct_play_mode,
        part_id: part_id_of(part),
        src_vcodec: vcodec.to_string(),
        src_acodec: acodec.to_string(),
        // **`bool::default()` is `false` and `false` here is a CLAIM** — "this television cannot
        // decode the source" — which the quality menu turns into a line of copy. The exit two lines
        // below returns this plan without ever reaching the gate, so an unresolvable playback would
        // assert something nobody looked at. Every exit therefore carries `true` ("nobody has said
        // otherwise") until the gate says otherwise.
        source_decodable: true,
        ..Default::default()
    };
    if env.direct_play_mode == DirectPlayMode::Disabled && rk.is_empty() {
        plan.verdict = Some(PlayVerdict::DirectPlayDisabled);
        return plan;
    }
    let forced = env.direct_play_mode == DirectPlayMode::Forced;
    let playback_quality = if forced { Quality::Original } else { env.quality };
    let client = match crate::plex::client_for(env.sid) {
        Some(c) => c,
        None => return plan,
    };
    // fresh per-playback session id (BOTH direct-play and transcode report through it) +
    // a PlayQueue so the server tracks this as a real player with a playQueueItemID.
    let session = new_sess(rk);
    plan.sess = session.clone();
    if !rk.is_empty() && !env.preview {
        let q = resolve_playqueue(
            client,
            rk,
            &session,
            &env.machine_id,
            !env.omit_queue_continuous,
        );
        plan.machine_id = q.machine_id;
        plan.pq_id = q.id;
        plan.pq_item_id = q.item_id;
        plan.up_next = q.up_next;
        plan.queue = q.rows;
    }
    // the playing item's OWN track lists (menu + audio pick + esInfo fps read them) — the
    // loaded detail can be a different item (show page / straight-from-Home play)
    // detail already had this item's streams — no GET
    plan.playing = env
        .cached_item
        .clone()
        .or_else(|| crate::metadata::fetch_playing_item(env.sid, rk));
    if let (Some(id), Some(item)) = (env.subtitle_override, plan.playing.as_mut()) {
        // The retry's explicit selection owns both embedded and sidecar restoration; a stale
        // server-side selection must not turn subtitles back on after the viewer chose Off.
        for sub in &mut item.subs { sub.selected = id > 0 && sub.id == id; }
    }
    // Server-adjudicated: the Media Decision Engine decides direct-play vs transcode from our
    // capability profile. An unusable / unreachable `/decision` must not Original — the server
    // never adjudicated it against the profile's limits (see the MDE verdict note below for why
    // this is no longer a 503 claim); remux/re-encode still registers via a
    // separate `transcode_decision`. The local-sample/demo path (rk empty) skips MDE entirely.
    // Smart direct-play: the video decodes natively (H264/HEVC) AND some audio track is
    // direct-playable (AAC/AC3/E-AC3) — even if the DEFAULT track isn't. We own the demuxer, so
    // we direct-play the raw file and FEED a direct-playable track (e.g. a 4K HEVC item: TrueHD
    // default + an AC3 track → native 4K HEVC + AC3, no transcode — beats the server's
    // video-downscaling transcode). The chosen audio rides `audioStreamID` on `/decision` so MDE
    // evaluates that sibling rather than vetoing the TrueHD/DTS default. When `/decision` is
    // unreachable the plan fails closed (no Original Part without the server's verdict) and may
    // still remux/re-encode; an explicit MDE transcode also forbids remux.
    // The video gate consults the DEVICE's own decoder table (devcaps), not this codebase's
    // memory of the dev TV: "the panel decodes HEVC" was the last dev-environment claim still
    // asserted as universal (issue #22's bug class — docs/plex-pass-audit.md, closing section).
    // This is belt-and-braces with the profile — a no-hevc profile means PMS should never
    // *offer* hevc direct-play, and when `/decision` is unreachable the local gate must still
    // agree with the profile on BOTH axes it asserts: the codec
    // AND the width/height bound. Codec agreement alone left the resolution half open — the
    // profile's `*`-scoped limitation makes PMS transcode a 4K source down for a 1080p-bounded
    // SoC, but a fallback that never asked the server never meets the limitation, so a 4K file with
    // any AAC/AC3 track (nearly every file has one) would direct-play straight onto the bounded
    // decoder. See `video_direct_plays` for the gate itself.
    let (src_w, src_h) = plan
        .playing
        .as_ref()
        .map(|p| (p.width, p.height))
        .unwrap_or((0, 0));
    // The DV layering rides the same playing-item store as the frame size, for the same reason:
    // it is the PLAYED LEAF's, not the detail page's (a show page's Detail describes whichever
    // episode backfilled it). Absent store → default `Dovi`, which is all-zero and refuses
    // nothing.
    let dovi = plan.playing.as_ref().map(|p| p.dovi).unwrap_or_default();
    // Freeze the capability and derived presentation together. A late configd answer affects the
    // next route only; it cannot change this candidate between the gate and Starfish Load.
    #[cfg(test)]
    let dv_decision = match env.dv_capability {
        Some(capability) => crate::metadata::DvDecision {
            capability,
            presentation: dovi.presentation(
                !crate::metadata::dv_withheld(),
                capability,
                vcodec == "hevc",
            ),
        },
        None => dovi.decision_now(vcodec == "hevc"),
    };
    #[cfg(not(test))]
    let dv_decision = dovi.decision_now(vcodec == "hevc");
    let dv = dv_decision.presentation;
    let video_dp = if forced { video_feed_supported(vcodec, dv) } else {
        video_direct_plays(vcodec, src_w, src_h, dv, crate::devcaps::caps())
    };
    // Carried to the session so the quality menu can say whether "Original" means anything for
    // this item without evaluating the gate a second time against a different set of facts.
    plan.source_decodable = video_dp;
    // **Refusing direct play is only half of it.** The transcode query below grants the server
    // `directStream=1` — permission to COPY the video rather than encode it — and PMS takes that
    // permission whenever the source fits the caps the query carries. Those caps are resolution,
    // bitrate and the profile's limitation axes, and **not one of them can say "Dolby Vision"**,
    // so a refused Profile 5 file came back `Part.decision=transcode` with the video's own
    // decision `copy`: the identical IPT-PQ bitstream, one container down, and the identical
    // wrong colours the refusal was for (measured against the dev PMS 2026-08-21 — before this
    // line existed, the whole gate above changed the container and nothing else). Withdrawing the
    // permission is what makes the refusal mean something, and it is withdrawn ONLY here: a size
    // or codec refusal is one the server's own caps already express, and a copy that satisfies
    // them is a free win worth keeping.
    //
    // **This stays the base-layer question, and does NOT become `dv.refusal().is_some()`.** A copy
    // arrives with no `DolbyHdrInfo` node attached — the declaration rides the direct play, not
    // the file — so the test is the pre-declaration one: is this bitstream a correct picture when
    // nobody has been told what it is? Declaring a Profile 5 makes direct play right and leaves a
    // copy of it exactly as wrong as before.
    let no_video_copy = dovi.base_layer_unusable();
    if dovi.present {
        crate::player::log(&format!(
            "dv: capability={} presentation={} profile={} bl_compat={}",
            dv_decision.capability.label(),
            dv.label(),
            dovi.profile,
            dovi.bl_compat,
        ));
    }
    if let Some(why) = dv.refusal() {
        // Worth a line of its own: from the outside this looks like a 4K HEVC file with a normal
        // audio track being sent to the transcoder for no reason, and the DOVI fields that
        // explain it are not in any other log line. `ff.rs` logs the demuxer's own reading of the
        // configuration record at open, which is the ground truth this decision only approximates.
        // NB the server is allowed to answer that it cannot do it — this PMS refuses a Profile 5
        // outright ("File is unplayable. DoVi (Profile 5) color space is not supported."), which
        // `refusal` below turns into the player's read-out quoting that sentence. A read-out that
        // names the reason beats a picture in the wrong colours with nothing to explain it.
        crate::player::log(&format!(
            "route: dolby vision P{} (bl_compat={} el={}) — {why}, base layer is not self-displayable; re-encoding (no copy)",
            dovi.profile, dovi.bl_compat, dovi.el_present as i32
        ));
    } else if let Some(n) = dv.declared() {
        // The other half of the same story, and worth its own line for the same reason: from the
        // outside a Profile 5 that suddenly direct-plays looks like the refusal having silently
        // regressed. This says it was a decision, and names the values the payload will carry.
        crate::player::log(&format!(
            "route: dolby vision P{} (bl_compat={} el={}) — declaring DolbyHdrInfo (trackType={} profileId={}); direct play",
            dovi.profile, dovi.bl_compat, dovi.el_present as i32, n.track_type, n.profile_id
        ));
    }
    // MKV and MP4 both direct-play. MP4 once died after AU#0 (b1002de) because the mov demuxer's
    // random access needed seeks the then-unseekable AVIO could not serve; `ff.rs::seek_cb` has
    // reopened with a byte Range since, and mp4 was re-measured on-device 2026-08-11: sequential
    // play, a 140s in-place seek and the harness's rapid burst all pass (issue #22 — the mkv-only
    // gate was sending every mp4 to the transcoder, which a server without Plex Pass then failed).
    // Anything else (.mov/.avi/…) still goes to Plex for a container-only REMUX to progressive
    // MKV (copy the codecs, no re-encode — keeps 4K/HDR).
    let streamable = part_is_streamable(part);
    // snapshot the track list on the MAIN thread and pass it by reference — the resolve worker
    // (step 7) gets an owned copy instead, and never touches the `&'static` store.
    let tracks = plan
        .playing
        .as_ref()
        .map(|p| p.audio.as_slice())
        .unwrap_or(&[]);
    // The SHOW's own language settings (its Advanced dialog), for an episode: one small read
    // per play (at most two GETs sharing a 1500 ms budget). See the preference pickers.
    let show_prefs = plan
        .playing
        .as_ref()
        .filter(|p| !p.show_rk.is_empty())
        .and_then(|p| client.show_language_prefs(&p.show_rk))
        .unwrap_or_default();
    // Capture identity and generation as ONE publication. The credential helper keeps plex.tv
    // and PMS authority separate and permits the owner account-token fallback only for a proven
    // legacy owner session.
    let active_profile = crate::plex::session::current_snapshot();
    let mut account_subtitles = None;
    let account_audio = match active_profile.user.as_ref() {
        Some(user) => match crate::plex::session::plex_tv_credential(user) {
            Some(credential) => {
                let stored_session = crate::plex::session::peek();
                if stored_session.client_id.is_empty() {
                    AccountAudioLanguage::NoCredential
                } else {
                    match crate::plex::account::AccountClient::audio_preferences(
                        &stored_session.client_id, &credential, user, active_profile.generation,
                    ) {
                        crate::plex::account::AudioPreferencesOutcome::Available(prefs) => {
                            account_subtitles = Some((prefs.subtitle_language.clone(), prefs.subtitle_mode, prefs.subtitle_forced));
                            match prefs.language {
                            Some(language) => AccountAudioLanguage::Set(language),
                            None => AccountAudioLanguage::NotSet {
                                auto_select_audio: prefs.auto_select_audio,
                                stated_language: prefs.stated_language,
                            },
                        } },
                        crate::plex::account::AudioPreferencesOutcome::TimedOut =>
                            AccountAudioLanguage::TimedOut,
                        crate::plex::account::AudioPreferencesOutcome::Failed =>
                            AccountAudioLanguage::Unavailable,
                    }
                }
            }
            None => AccountAudioLanguage::NoCredential,
        },
        None => AccountAudioLanguage::NoCredential,
    };
    // every audio pick below (direct play, remux, re-encode) ranks against these same prefs
    let audio_prefs = AudioLangPrefs { show: show_prefs.audio.as_deref(),
        account: account_audio.language() };
    let audio_sel = if env.audio_sid > 0 {
        tracks.iter().enumerate().find(|(_, t)| t.id == env.audio_sid
            && audio_direct_plays(env.direct_play_mode, &t.codec, t.channels))
            .map(|(i, t)| (i as i32, t.codec.to_lowercase(), t.id))
            .or_else(|| (!forced).then(|| pick_dp_audio_pref(tracks, acodec, audio_prefs)).flatten())
    } else if rk.is_empty() {
        None
    } else {
        pick_dp_audio_mode(tracks, acodec, audio_prefs, env.direct_play_mode)
    };
    if let Some(lang) = show_prefs.audio.as_deref() {
        let hit = audio_sel
            .as_ref()
            .and_then(|(i, _, _)| usize::try_from(*i).ok())
            .and_then(|i| tracks.get(i))
            .is_some_and(|s| lang_matches(lang, &s.lang_code));
        crate::player::log(&format!(
            "route: show prefers audio {lang} — {}",
            if hit {
                "playing that track"
            } else if tracks.iter().any(|s| lang_matches(lang, &s.lang_code)) {
                "a track in it exists but is not direct-playable; using the usual order"
            } else {
                "no track in it; using the usual order"
            }
        ));
    }
    if !rk.is_empty() {
        crate::player::log(&account_audio_language_log(&account_audio, tracks, audio_sel.as_ref()));
    }
    // the language of the audio that will play — what "shown with foreign audio" is judged by
    let audio_lang: String = audio_sel
        .as_ref()
        .and_then(|(i, _, _)| usize::try_from(*i).ok())
        .and_then(|i| tracks.get(i))
        .or_else(|| tracks.iter().find(|s| s.default))
        .or_else(|| tracks.first())
        .map(|s| s.lang_code.clone())
        .unwrap_or_default();
    // The show's own pref wins over the account's — the same precedence `pick_dp_subtitle_account`
    // gives them below — so the Subtitles menu's "yours" grouping never disagrees with what was
    // actually picked.
    plan.sub_pref_lang = show_prefs
        .subtitle
        .clone()
        .or_else(|| account_subtitles.as_ref().and_then(|(language, _, _)| language.clone()));
    // ONE subtitle decision for the three places below (MDE handshake, the Original candidate,
    // the direct-play plan), so they cannot disagree about what will be on screen.
    let sub_pick = plan
        .playing
        .as_ref()
        .and_then(|p| {
            let account = account_subtitles.as_ref().map(|(language, mode, forced)| SubtitleLangPrefs {
                language: language.as_deref(), mode: *mode, forced: *forced,
            }).unwrap_or_default();
            if let Some(id) = env.subtitle_override {
                p.subs.iter().position(|s| s.id == id && !s.external && embedded_subtitle_renderable(&s.codec))
                    .and_then(|i| (id > 0).then_some((id, crate::metadata::sub_render_ordinal(&p.subs, i))))
            } else {
                pick_dp_subtitle_account(&p.subs, &show_prefs, account, &audio_lang)
            }
        });
    if sub_pick.is_some()
        && show_prefs.subtitle.is_some()
        && !plan.playing.as_ref().is_some_and(|p| p.subs.iter().any(|s| s.selected))
    {
        crate::player::log(&format!(
            "route: show subtitles {} (mode {}) — turning on an embedded track",
            show_prefs.subtitle.as_deref().unwrap_or(""),
            show_prefs.subtitle_mode
        ));
    }
    let audio_id = audio_sel.as_ref().map(|(_, _, id)| *id).unwrap_or(0);
    let subtitle_id = plan
        .playing
        .as_ref()
        .map(|p| mde_subtitle_id_of(&p.subs, sub_pick))
        .unwrap_or(0);
    // What the CONNECTION to this server allows, beside what the pipeline can decode: a Plex
    // relay is a ~2 Mbit/s tunnel, so neither of the two flavors that ship the file's own bytes
    // (direct play, and the uncapped container remux) can be asked for over one. Unrestricted on
    // every other tier and on a server whose link nobody has recorded, which is all of them today.
    // The reasoning, and what is measured versus documented, is at `plex::link_policy`.
    let location = client.link();
    let link = crate::plex::link_policy(location);
    // …and what the USER has asked for, on top of what the link allows. Same two flags, composed
    // by AND, so the STRICTER of the two always wins: a relay link cannot be loosened by picking a
    // high rung, and a low rung is not rescued by a fast link. The reasoning — and why a ceiling
    // has to arrive HERE, before a flavor is chosen, rather than as a number on the spec — is at
    // `quality_policy` and `Quality`.
    // Auto tentatively admits Original. A direct Remote earns that admission below with an
    // actual-file sample; Local gets it immediately, while Relay is still denied independently
    // by `link`. Fixed rungs retain their ordinary ceiling policy.
    let tentative_quality = quality_policy(playback_quality, true, env.src_kbps, src_w, src_h);
    let mut allowed = direct_play_policy(env.direct_play_mode, flavors_allowed(link, tentative_quality));
    // MDE verdict for this resolve: Some(original)=Part.decision=directplay, Some(!original)=
    // start.mkv, None=unreachable/unusable OR never asked (gates already refused Original).
    // None must never become Original: an Original the server never judged against the profile
    // is a local guess about the server's own limits. (This line used to say PMS 1.43 503s a Part
    // GET without a registered decision. Measured against PMS 1.43.4 for issue #266 (M5), a
    // `Range: 0-1023` Part GET answered 206 with no decision at all, after MDE, and after MDE
    // followed by an enhanced remux decision on the same session — the 503 did not reproduce, so
    // the rule stands on the adjudication alone.)
    // `video_forbids_copy` is independent: Part=transcode + video=copy (TrueHD-only, a selected
    // sub MDE still refuses, …) is a remux, not a full re-encode.
    let skip_mde = !allowed.direct_play || !video_dp || !streamable || rk.is_empty();
    let mde: Option<MdeVerdict> = if skip_mde {
        None
    } else {
        // Register the session before any Part GET. Smart-DP used to skip this because MDE would
        // evaluate a TrueHD/DTS default and veto; naming the chosen AAC/AC3/EAC3 sibling on the
        // query is what keeps that class on Original. subtitleStreamID is an advertised embedded
        // track Original will client-render, or 0 so a sidecar / unadvertised codec does not
        // force a burn. MDE and the remux probe always name that sibling (a copy cannot carry
        // TrueHD/DTS). The play-path PUT and start.mkv use `encode_audio_id`: remux still names
        // the sibling; a re-encode walks the same `audio_intents` ranking (the PMS selection, then
        // show/account language, then the direct-play pick), so 720p does not copy a foreign AC3.
        if forced { forced_server_decision(client, rk, &session, audio_id, subtitle_id) }
        else { server_decision(client, rk, &session, audio_id, subtitle_id) }
    };
    let mut directplay = mde.as_ref().is_some_and(|v| v.original && (!forced || !v.video_forbids_copy));
    if forced {
        let failure = if part.is_empty() { Some(ForcedFailure::NoOriginal) }
            else if !streamable { Some(ForcedFailure::Container) }
            else if !video_dp { Some(ForcedFailure::Video) }
            else if audio_sel.is_none() && (!tracks.is_empty() || !audio_direct_plays(env.direct_play_mode, acodec, 0)) {
                Some(ForcedFailure::Audio)
            } else if !rk.is_empty() && !directplay { Some(ForcedFailure::Unauthorized) }
            else { None };
        if let Some(failure) = failure {
            plan.verdict = Some(PlayVerdict::Forced(failure));
            return plan;
        }
    }
    // An unreachable MDE (None after we asked, or never asked) still allows remux when the
    // video gate and link policy do. A video-stream `transcode` (bit depth, …) forbids remux.
    let mde_forbids_copy = mde.as_ref().is_some_and(|v| v.video_forbids_copy);

    // A container-only remux also preserves the original video and avoids the GPU, so it belongs
    // to Auto's Original state and must pass the same remote bandwidth gate as direct play.
    let remux_candidate = video_dp && allowed.remux && !no_video_copy && !mde_forbids_copy;
    let source_transport_kbps = plan
        .playing
        .as_ref()
        .map(|p| p.bitrate)
        .filter(|&v| v > 0)
        .unwrap_or(env.src_kbps);
    // Keep the exact zero-video-encode flavour before a fixed rung or Auto's immediate HLS decision
    // overwrites `directplay`. Recovery must restore the source declaration which WOULD have been
    // installed, not derive one later from the transcode currently on screen. Manual Original needs
    // it too: after a fixed rung with a burned subtitle, returning to Original must restore direct
    // play and the client-rendered subtitle rather than build another encoder. Remote Auto also
    // uses the candidate as the target of its throughput probes.
    if !forced && matches!(playback_quality, Quality::Auto | Quality::Original)
        && matches!(
            location,
            Some(crate::plex::probe::Location::Local) | Some(crate::plex::probe::Location::Remote)
        )
        && (directplay || remux_candidate)
        && !part.is_empty()
    {
        let aidx = audio_sel.as_ref().map_or(-1, |(idx, _, _)| *idx);
        let direct = directplay;
        let fps = if direct {
            plan.playing.as_ref().map(|p| p.video_fps).unwrap_or(0.0)
        } else {
            0.0
        };
        let audio_ordinal = if direct && aidx >= 0 {
            plan.playing
                .as_ref()
                .map(|p| crate::metadata::audio_ordinal(&p.audio, aidx as usize))
                .unwrap_or(aidx)
        } else {
            -1
        };
        let subtitle_ordinal = direct.then(|| sub_pick.map(|(_, ord)| ord)).flatten();
        plan.auto_original = Some(AutoOriginalCandidate {
            url: client.direct_play_url(part, &session).to_url(),
            probe_part: part.to_owned(),
            direct,
            vcodec: vcodec.to_string(),
            fps,
            dovi: if direct {
                dovi
            } else {
                crate::metadata::Dovi::NONE
            },
            dv_decision: if direct {
                dv_decision
            } else {
                crate::metadata::DvDecision::NONE
            },
            // The explicit pick's own fetched stream; a server-default candidate (no pick) is
            // `None` and recovery falls back to the source codec (`Session::src_acodec`).
            audio: carried_track(tracks, audio_id, audio_ordinal, direct),
            subtitle_ordinal,
        });
    }
    // Issue #266: the audio enhancement decorates the Original route only (I5), so it is judged
    // against the candidate captured above — its DV facts (I7), its carried track, and the family
    // it would play as. `env.sub_sid` is a burn, and a burn is a subtitle on screen too (I6).
    let subtitle_shown = renders_subtitle(plan.playing.as_ref(), sub_pick) || env.sub_sid > 0;
    let enhancement_for = |candidate: Option<&AutoOriginalCandidate>, target: RouteFamily| {
        desired_audio(
            env.audio_enhancements,
            enhancements_offered(
                &EnhancementFacts {
                    pass: env.pass,
                    base: candidate,
                    carried: candidate.and_then(|c| c.audio.as_ref()),
                    subtitle_shown,
                    refused: false,
                },
                target,
            ),
        )
    };
    // What the remote remux probe must sample: "the remux we would actually play" includes its
    // audio DSP, and the play path reuses the probe's session.
    let pre_audio = plan.auto_original.as_ref().map_or(crate::plex::AudioEnhancements::NONE, |c| {
        enhancement_for(Some(c), if c.direct { RouteFamily::Direct } else { RouteFamily::Remux })
    });
    // **Cold start, decided in one place.** Feasibility first (is Original even possible for this
    // item), then the link's own class, then — on a direct Remote only — one bounded measurement.
    // `abr::bootstrap` owns the policy; this site owns only the facts it needs.
    let bootstrap_catalog = crate::abr::HlsActuatorCatalog::measured().limited_to(
        (
            u16::try_from(crate::devcaps::caps().hevc_max.0).unwrap_or(u16::MAX),
            u16::try_from(crate::devcaps::caps().hevc_max.1).unwrap_or(u16::MAX),
        ),
        (
            u16::try_from(src_w).unwrap_or(u16::MAX),
            u16::try_from(src_h).unwrap_or(u16::MAX),
        ),
    );
    let policy = crate::abr::AbrPolicy::measured();
    let original_feasible = (directplay || remux_candidate) && plan.auto_original.is_some();
    let link_kind = match location {
        Some(crate::plex::probe::Location::Local) => Some(crate::abr::LinkKind::Local),
        Some(crate::plex::probe::Location::Remote) => Some(crate::abr::LinkKind::Remote),
        Some(crate::plex::probe::Location::Relay) => Some(crate::abr::LinkKind::Relay),
        None => None,
    };
    // Captured before Auto overwrites `directplay` for HLS: a remux probe registered start.mkv
    // on this playback identity, and a later HLS `/decision` must physical-stop that encoder
    // first. A successful remux Original leaves the session for the play-path decision.
    let mut remux_probed = false;
    // Issue #266: the remote remux probe asked the enhancement first and the server said no.
    let mut probe_refused_enhancement = false;
    // A preview that is already not direct-playable (MDE denied Original, or the extra carries
    // no Part) is refused unconditionally below by `preview::accepts_direct_play` regardless of
    // what Auto's bandwidth probe would decide — `adaptive` cannot rescue a `directplay=false`
    // preview. Skipping the probe here (and, on the remux leg, the `put_selection` PUT that
    // would otherwise register a transcode this preview can never play) is the only branch worth
    // guarding: it is the one place this function does live network I/O before that refusal
    // check, and a preview is exactly the request most likely to hit a non-direct-playable item.
    let preview_already_refused = env.preview && (!directplay || part.is_empty());
    let decision = match (playback_quality, link_kind) {
        (Quality::Auto, Some(link)) => {
            // The probe is the only expensive input, so it is only taken where it can change the
            // answer: a direct Remote with a feasible Original. Local needs no proof and Relay
            // cannot be talked into carrying a remux.
            let probe = (link == crate::abr::LinkKind::Remote
                && original_feasible
                && !preview_already_refused)
                .then(|| {
                    if directplay {
                        measure_remote_original(
                            &client.direct_play_url(part, &session).to_url(),
                            source_transport_kbps,
                        )
                    } else {
                        // Part GET 503s after a transcode MDE. Sample the remux we would actually play.
                        remux_probed = true;
                        // GET parameters do not install PMS's part selection. Use the same
                        // remux policy as playback, before either the decision or media GET.
                        // A client-rendered subtitle is not a burn; only env.sub_sid requests one.
                        let probe_audio = encode_audio_id(true, audio_id, env.audio_sid, tracks, audio_prefs);
                        put_selection(env.sid, plan.part_id, probe_audio, env.sub_sid);
                        let probe = measure_remote_remux(
                            client,
                            rk,
                            &session,
                            probe_audio,
                            env.sub_sid,
                            source_transport_kbps,
                            pre_audio,
                        );
                        probe_refused_enhancement = probe.enhancement_refused;
                        probe.sample
                    }
                })
                .flatten();
            Some(crate::abr::bootstrap(
                link,
                original_feasible,
                u32::try_from(source_transport_kbps).unwrap_or(0),
                probe,
                &bootstrap_catalog,
                &policy,
            ))
        }
        _ => None,
    };
    if let Some(decision) = decision.as_ref() {
        plan.auto_prior_kbps = decision.prior.map(|prior| prior.slow_kbps).unwrap_or(0);
        plan.auto_bootstrap_rung = Some(decision.rung);
    }
    let auto_original = decision.as_ref().is_some_and(|d| d.original);
    let adaptive = auto_uses_hls(playback_quality, auto_original);
    if adaptive {
        allowed = flavors_allowed(
            link,
            quality_policy(playback_quality, false, env.src_kbps, src_w, src_h),
        );
        directplay = false;
        plan.contract.delivery = crate::plex::TranscodeDelivery::FixedHls {
            seconds_per_segment: 2,
        };
        let rung = decision
            .as_ref()
            .map(|d| d.rung)
            .unwrap_or(crate::abr::Rung::P480);
        plan.contract.ceiling = Some(rung.ceiling());
        crate::player::log(&format!(
            "route: Auto adaptive — source {source_transport_kbps}kbps {src_w}x{src_h}; starting {}kbps HLS ({:?})",
            rung.kbps(),
            decision.as_ref().map(|d| d.reason),
        ));
    } else {
        plan.contract.ceiling = playback_quality.ceiling();
        if playback_quality == Quality::Auto {
            crate::player::log(&format!(
                "route: Auto Original — source {source_transport_kbps}kbps {src_w}x{src_h}; no video encode"
            ));
        }
    }
    // The ceiling and source measurement ride every plan so seeks and track changes rebuild the
    // same flavor instead of silently dropping the user's choice.
    plan.src_measure = (env.src_kbps, src_w, src_h);
    plan.transport_kbps = source_transport_kbps;
    // See `Session::cur_auto_original_watched`: Auto running Original is the whole condition, and
    // the link's tier is not part of it.
    // A preview is direct-play or nothing: an Original→HLS rescue would turn a trailer into a
    // transcode, so its Original is never watched.
    plan.auto_original_watched = playback_quality == Quality::Auto && auto_original && !env.preview;
    if playback_quality != Quality::Auto && !tentative_quality.direct_play {
        crate::player::log(&format!(
            "route: quality ceiling {:?} — source {}kbps {src_w}x{src_h}; denying direct play + remux, re-encoding",
            playback_quality,
            env.src_kbps
        ));
    }
    // The codec-preserving remux, decided once for both readers below: the enhancement's family
    // and the transcode branch's flavour. See the long note at the transcode branch for each term.
    let remux = video_dp && allowed.remux && !no_video_copy && !mde_forbids_copy;
    // Issue #266, decided HERE — after the adaptive override has had its say (HLS is `Other`) and
    // before either branch is taken. The enhancement turns the Original route it decorates into
    // a remux (M1: PMS answers either param with a Part transcode, video copy, audio re-encoded
    // with the DSP), so it applies only where that remux is itself allowed, and it is spent
    // through the same `flavors_allowed` door the link and the rung use.
    let planned_family = if adaptive {
        RouteFamily::Other
    } else if directplay {
        RouteFamily::Direct
    } else if remux {
        RouteFamily::Remux
    } else {
        RouteFamily::Other
    };
    let audio = if remux && planned_family != RouteFamily::Other && !probe_refused_enhancement {
        enhancement_for(plan.auto_original.as_ref(), RouteFamily::Remux)
    } else {
        crate::plex::AudioEnhancements::NONE
    };
    // Remembered for the fallback: a refused enhancement restores the route it decorated.
    let enhanced_from_direct = audio.any() && directplay;
    if audio.any() {
        allowed = flavors_allowed(allowed, enhancement_policy(audio));
        directplay = allowed.direct_play && directplay;
        crate::player::log(&format!(
            "enhancement: boost_dialog={} normalize_loudness={} — {} becomes an enhanced remux",
            audio.boost_dialog,
            audio.normalize_loudness,
            if enhanced_from_direct { "direct play" } else { "remux" },
        ));
    }
    plan.contract.audio = audio;
    if env.preview && !crate::player::preview::accepts_direct_play(directplay, !part.is_empty(), adaptive) {
        crate::player::log("preview: refused — not a direct play");
        plan.url.clear();
        return plan;
    }
    if (directplay || rk.is_empty()) && !part.is_empty() {
        // direct-play: the pipeline decodes the SOURCE codecs natively, so the Load payload uses
        // them (h264/hevc + the chosen audio track's codec).
        fill_direct_plan(&mut plan, client, part, &session, vcodec, acodec, audio_sel.as_ref(), dovi, dv_decision, sub_pick);
        return plan;
    }
    if forced {
        plan.verdict = Some(PlayVerdict::Forced(ForcedFailure::OpenFailed));
        return plan;
    }
    // Transcode OR container-remux, both served via start.mkv. If the SOURCE video is
    // direct-playable (h264/hevc) we only reached here because the container isn't streamable, so
    // ask Plex to REMUX — copy both codecs into MKV, no re-encode (keeps 4K + HDR10); the Load
    // payload then uses the SOURCE codecs. Otherwise it's a real RE-ENCODE to the profile's
    // target chain (hevc first when the SoC decodes it — keeps 4K + HDR10 — else h264; see
    // profile_for). The guess below is only the /decision-unreachable fallback: decision_codecs
    // overrides it with the server's ACTUAL output, but the guess still tracks devcaps because
    // a payload naming hevc on a SoC without the decoder configures a pipeline that cannot start.
    // A direct-playable source means "ask Plex to REMUX" — unless the link forbids a copy, in
    // which case this is a re-encode after all and every line below must agree (the payload guess,
    // the stored flavor a seek rebuilds from, and the /decision query itself).
    // `!no_video_copy` is the third term and it is not redundant with `video_dp`. A remux COPIES
    // the video, so a Dolby Vision file whose base layer needs a declaration would come back with
    // the same RPU one container down and a payload built on this branch — which declares nothing.
    // Before the declaration existed the gate above already excluded every such file (they were
    // all refused); now a Profile 5 can PASS it and reach here for a different reason — an
    // unstreamable container, or no direct-playable audio track — and would have been quietly
    // remuxed into the very picture the whole change is about. It also keeps the invariant
    // `plex::Client::transcode_query` relies on: `remux` and `no_video_copy` are never both true.
    // `allowed.remux` is `link.remux` AND the user's ceiling — see `flavors_allowed` above. The
    // ceiling is the newer of the two terms and it denies a remux for the reason the relay does: a
    // copy ships the source at the source's own rate, which is precisely what the rung says the
    // link cannot carry. `!mde_forbids_copy` is the MDE half: a VIDEO stream decision of
    // `transcode` must not be answered with a local codec-copy remux. Part.decision=transcode
    // alone is not that veto.
    // (`remux` is computed above, beside the enhancement's family.)
    // Remux copies, so this PUT names the smart-DP sibling. A re-encode transcodes a real
    // selected source track (English DTS → AC3) and must not PUT that sibling or a 720p start
    // replaces the pick with a foreign AC3 copy. A selected flag that only echoes default is
    // not a pick; `encode_audio_id` then keeps a sibling in the show language, or the first
    // track in that language (unselected DTS included), else the direct-play pick.
    let encode_audio = encode_audio_id(remux, audio_id, env.audio_sid, tracks, audio_prefs);
    if remux && audio.any() {
        // I4: an enhanced remux RE-ENCODES the audio (M1), so the source codec is exactly the
        // wrong guess — a payload describing it is silent audio. `ac3` is the first target in
        // `transcoder::profile_for_delivery`; `decision_codecs` below replaces it with the answer.
        plan.vcodec = vcodec.to_string();
        plan.acodec = "ac3".into();
    } else if remux {
        let achosen = audio_sel
            .as_ref()
            .map(|(_, c, _)| c.clone())
            .unwrap_or_else(|| acodec.to_string());
        plan.vcodec = vcodec.to_string();
        plan.acodec = achosen;
    } else if matches!(
        plan.contract.delivery,
        crate::plex::TranscodeDelivery::FixedHls { .. }
    ) {
        plan.vcodec = "h264".into();
        plan.acodec = "aac".into();
    } else {
        plan.vcodec = crate::devcaps::caps().encode_vcodec().into();
        plan.acodec = "ac3".into();
    }
    // Carry the SOURCE track this path will PUT and name on start.mkv. The demuxer is NOT
    // pointed at a source ordinal here (the old set_audio_track(aidx) indexed the SERVER's
    // output, whose stream layout is the transcoder's, not the source's) — the payload-codec
    // match finds the lane.
    plan.audio = plan_track(tracks, encode_audio, -1, false);
    // keep the flavor so a later seek rebuilds the same query for start.mkv?...&offset=T
    // Both halves of this line landed in the same batch from different units and each is
    // load-bearing: `remux` (not `video_dp`) is the relay gate — a copy of a 31 Mbit/s stream
    // down a 2 Mbit/s tunnel cannot play, so `link.remux` demotes it to a real re-encode — and
    // `env.sid` routes the selection to the server the ITEM came from. Dropping either compiles
    // and passes: without the gate a relay stalls, without the sid a friend's audio pick is PUT
    // to our own server, which answers 200 and changes nothing on theirs.
    plan.contract.remux = remux;
    plan.contract.no_video_copy = no_video_copy;
    // `plan.contract.ceiling` is NOT set here — it was set for every flavour up at the decision,
    // which is what the direct-play branch needed too. Spending it below is the third reader of
    // the same reasoning `remux` and `no_video_copy` carry: a seek and an audio switch rebuild
    // this query from `Session`, and one that dropped the ceiling would hand the encoder back the
    // full 4K/60 Mbps bound the moment the user touched the scrubber.
    // Remux: the smart-DP sibling MDE and the remux probe already named — `env.audio_sid` is
    // the part default (TrueHD) at resolve start; putting that undoes smart-DP. Re-encode:
    // `encode_audio_id` (the PMS selection, else show/account language, else that sibling).
    // Subtitle stays
    // `env.sub_sid`: a positive id here is a burn, and Original client-renders instead.
    put_selection(env.sid, plan.part_id, encode_audio, env.sub_sid);
    if remux_probed && adaptive {
        // Probe registered start.mkv on this playback identity. HLS `/decision` reuses it;
        // closeResourceSession=1 would 503 the next start. A failed sample already stopped
        // inside measure_remote_remux; this covers a completed sample that still falls to HLS.
        // Stay-remux Original does not stop: the play-path decision owns that session.
        let _ = client.transcode_stop_physical(&session);
    }
    let mut sp = transcode_spec(
        rk,
        &session,
        &session,
        crate::plex::TranscodeOffset::Fresh,
        encode_audio,
        env.sub_sid,
        plan.contract,
    );
    // The enhanced decision rides the MDE's own `session`, as every remux here always has: M5
    // measured that a Part GET on a session that has seen MDE and then an enhanced remux decision
    // still answers 206, so re-registering the same id is not a hazard to either route.
    let mut decision = client.transcode_decision(&sp);
    if enhancement_fallback(decision.as_ref(), audio) == Fallback::Retry {
        // Refused outright, or ignored (audio `copy` despite the params): rebuild once without
        // the enhancement, on the same session, and remember that this server said no.
        crate::player::log("enhancement: refused/ignored by server; fell back");
        plan.contract.audio = crate::plex::AudioEnhancements::NONE;
        plan.enhancement = super::decision::EnhancementOutcome::Refused;
        if enhanced_from_direct {
            // Back to the direct play the enhancement decorated. The MDE is re-asked first so the
            // Part GET follows a decision that is the direct play's own — M5 found PMS serving the
            // Part regardless, so this is belt-and-braces, not a 503 workaround.
            let _ = server_decision(client, rk, &session, audio_id, subtitle_id);
            plan.contract.remux = false;
            plan.contract.no_video_copy = false;
            fill_direct_plan(&mut plan, client, part, &session, vcodec, acodec, audio_sel.as_ref(), dovi, dv_decision, sub_pick);
            return plan;
        }
        if let Some((_, c, _)) = audio_sel.as_ref() {
            plan.acodec = c.clone();
        } else {
            plan.acodec = acodec.to_string();
        }
        sp = transcode_spec(
            rk,
            &session,
            &session,
            crate::plex::TranscodeOffset::Fresh,
            encode_audio,
            env.sub_sid,
            plan.contract,
        );
        decision = client.transcode_decision(&sp);
    } else if probe_refused_enhancement && pre_audio.any() {
        // The remote remux probe already asked and was refused; the play was built without it.
        plan.enhancement = super::decision::EnhancementOutcome::Refused;
    } else {
        plan.enhancement = classify_outcome(decision.as_ref(), plan.audio.as_ref(), audio);
    }
    if let Some(mc) = decision {
        // The server has already answered, and it is allowed to answer NO. Stop here rather than
        // stream a `start.mkv` it has just said it cannot produce: the plan leaves with no URL —
        // the ordinary "this did not resolve" failure — and carries the verdict so the read-out can
        // quote the server's own sentence instead of the generic "Playback failed" this used to be.
        if let Some(v) = refusal(&mc) {
            crate::player::log(&format!(
                "decision: REFUSED general={:?} transcode={:?} — {v}",
                mc.general_decision_code, mc.transcode_decision_code
            ));
            plan.verdict = Some(PlayVerdict::Server(v));
            return plan;
        }
        // the Load payload must match the server's ACTUAL output codecs
        if let Some((v, a)) = decision_codecs(&mc) {
            plan.vcodec = v;
            plan.acodec = a;
        }
    }
    plan.url = client.transcode_start_url(&sp).to_url();
    plan.tsession = session;
    plan
}


/// The direct-play plan: the pipeline decodes the SOURCE codecs natively, so the Load payload uses
/// them (h264/hevc + the chosen audio track's codec). If a specific track was picked (aidx >= 0),
/// tell the demuxer to feed that stream — by CONTAINER ordinal, not the list position
/// (audio_ordinal sorts on PMS Stream.index). A function of its own because [`build_stream`]
/// reaches it twice: the ordinary Original, and an Original whose enhanced remux the server
/// refused (issue #266's fallback).
#[allow(clippy::too_many_arguments)]
fn fill_direct_plan(
    plan: &mut Plan,
    client: &crate::plex::Client,
    part: &str,
    session: &str,
    vcodec: &str,
    acodec: &str,
    audio_sel: Option<&(i32, String, i64)>,
    dovi: crate::metadata::Dovi,
    dv_decision: crate::metadata::DvDecision,
    sub_pick: Option<(i64, i32)>,
) {
    let (aidx, achosen, asid) = audio_sel.cloned().unwrap_or((-1, acodec.to_string(), 0));
    // source fps for the Load esInfo — from the playing item's own store (present for the
    // straight-from-Home path too, which never ran load_detail)
    let fps = plan.playing.as_ref().map(|p| p.video_fps).unwrap_or(0.0);
    plan.vcodec = vcodec.to_string();
    plan.acodec = achosen.clone();
    plan.fps = fps;
    // Only here: this is the branch that feeds the FILE's own elementary stream, so it is the
    // only one whose Load payload may describe the file's Dolby Vision.
    plan.dovi = dovi;
    plan.dv_decision = dv_decision;
    // **Dolby Atmos, and it is the same sentence one codec over.** `contents.immersive` tells
    // the pipeline that the E-AC3 it is about to decode carries JOC, which is what raises the
    // television's own Atmos read-out and what puts the sound engine in the right mode.
    //
    // Read off the track we ACTUALLY PICKED, not off the part: a film routinely ships an Atmos
    // 7.1 beside a plain 5.1 and a commentary, and declaring the part's best track while
    // feeding the user's chosen one is a lie the pipeline has no way to detect. `aidx` is the
    // list position `audio_sel` chose; with no explicit pick, the server's `selected` flag is
    // the same track `acodec` came from.
    //
    // **Set on this branch only, and the omission on the others is deliberate.** A transcode's
    // audio is re-encoded and its Atmos is gone, so declaring it would be false. A REMUX copies
    // the audio and would in fact still carry JOC — but `plan.dovi` already draws the line at
    // this branch on the same reasoning (a copy's payload describes what the server sends, and
    // the declaration rides the direct play), and one rule that is occasionally conservative
    // beats two rules that can disagree. Nothing is lost visibly: an undeclared Atmos plays as
    // ordinary E-AC3, which is what it does today.
    plan.immersive = plan
        .playing
        .as_ref()
        .and_then(|p| {
            if aidx >= 0 {
                p.audio.get(aidx as usize)
            } else {
                p.audio.iter().find(|a| a.selected)
            }
        })
        .is_some_and(|a| a.has_atmos());
    if plan.immersive {
        crate::player::log("audio: dolby atmos — declaring contents.immersive=ATMOS");
    }
    if aidx >= 0 {
        // NB this used to call player::set_audio_track, which stores SHARED.desired_audio_idx —
        // read by the DEMUX THREAD on every reopen. A worker writing it would change the audio
        // track of whatever is currently on screen. apply_plan does it, on the main thread.
        plan.feed_audio_ordinal = Some(
            plan.playing
                .as_ref()
                .map(|p| crate::metadata::audio_ordinal(&p.audio, aidx as usize))
                .unwrap_or(aidx),
        );
    }
    // Record the picked track so the timeline reports what actually plays (sid 0 = default/unknown
    // → `None`, the param is omitted and the server shows the part default).
    let ordinal = plan.feed_audio_ordinal.unwrap_or(-1);
    plan.audio = plan_track(plan.playing.as_ref().map_or(&[][..], |p| &p.audio[..]), asid, ordinal, true);
    // honour a subtitle the server already has selected for this part (chosen on another
    // client, or by this app in an earlier session), else the SHOW's subtitle settings —
    // free here, since the direct-play path renders subtitles itself. apply_plan installs it
    // on the main thread.
    if let Some((ssid, ord)) = sub_pick {
        plan.sub_sid = ssid;
        plan.sub_render_ordinal = Some(ord);
    }
    // direct-play: no transcode session (transcode_session() stays empty). Carry the
    // session id + identity on the file GET so PMS keys the /status/sessions entry by
    // SESS (not a token= fallback), keeping the timeline correlation consistent.
    plan.url = client.direct_play_url(part, session).to_url();
}


/// The Plex language preferences an audio pick honours, most specific first: the SHOW's
/// `audioLanguage` (its Advanced dialog, #160), then the active profile's enabled
/// `defaultAudioLanguage` (#203). There is deliberately no built-in language: one
/// used to sit here as a hard-coded English, and it opened a French user's French-default MKVs
/// in English whenever an English track existed (#202).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct AudioLangPrefs<'a> {
    /// `audioLanguage`, e.g. `"hu-HU"`. `""` / `"-1"` mean "Account default", i.e. unset here.
    pub show: Option<&'a str>,
    /// The active Plex profile's `defaultAudioLanguage`, e.g. `"fr"`.
    pub account: Option<&'a str>,
}

/// What happened while resolving the active profile's plex.tv audio preference. Keeping this
/// separate from [`AudioLangPrefs`] preserves the reason an account rung was unavailable for the
/// one per-resolve diagnostic without retaining credentials or identity data.
#[derive(Clone, Debug, PartialEq, Eq)]
enum AccountAudioLanguage {
    NoCredential,
    TimedOut,
    Unavailable,
    /// plex.tv answered but offered no language to use; the fields say which half was missing.
    NotSet { auto_select_audio: Option<bool>, stated_language: Option<String> },
    Set(String),
}

impl AccountAudioLanguage {
    fn language(&self) -> Option<&str> {
        match self {
            Self::Set(language) => Some(language),
            Self::NoCredential | Self::TimedOut | Self::Unavailable | Self::NotSet { .. } => None,
        }
    }
}

/// The account-language resolver diagnostic. This is pure so every emitted outcome stays
/// host-testable; it intentionally contains only the language code, never account identity.
fn account_audio_language_log(
    account: &AccountAudioLanguage,
    tracks: &[crate::metadata::Stream],
    audio_sel: Option<&(i32, String, i64)>,
) -> String {
    match account {
        AccountAudioLanguage::NoCredential => {
            "route: account audio language — no plex.tv credential for this profile".into()
        }
        AccountAudioLanguage::Unavailable => {
            "route: account audio language — unavailable (request failed)".into()
        }
        AccountAudioLanguage::TimedOut => {
            "route: account audio language — unavailable (timed out)".into()
        }
        AccountAudioLanguage::NotSet { auto_select_audio: Some(true), .. } => {
            "route: account audio language — not set".into()
        }
        AccountAudioLanguage::NotSet { auto_select_audio, stated_language } => format!(
            "route: account audio language — automatic audio selection {} (language {})",
            if auto_select_audio.is_some() { "off" } else { "not reported" },
            stated_language.as_deref().unwrap_or("not set"),
        ),
        AccountAudioLanguage::Set(lang) => {
            let picked_language = audio_sel
                .and_then(|(i, _, _)| usize::try_from(*i).ok())
                .and_then(|i| tracks.get(i))
                .is_some_and(|track| lang_matches(lang, &track.lang_code));
            let matching_track = |track: &crate::metadata::Stream| {
                lang_matches(lang, &track.lang_code)
            };
            let direct_playable_match = tracks.iter().any(|track| {
                matching_track(track) && crate::plex::is_dp_audio_track(&track.codec, track.channels)
            });
            let outcome = if picked_language {
                "playing that track"
            } else if direct_playable_match {
                "outranked by the PMS selection or show preference"
            } else if tracks.iter().any(matching_track) {
                "a track in it exists but is not direct-playable; using the usual order"
            } else {
                "no track in it; using the usual order"
            };
            format!("route: account prefers audio {lang} — {outcome}")
        }
    }
}

impl<'a> AudioLangPrefs<'a> {
    /// The preferences in precedence order, the unset ones dropped.
    fn in_order(self) -> impl Iterator<Item = &'a str> {
        self.show
            .into_iter()
            .chain(self.account)
            .map(str::trim)
            .filter(|l| !l.is_empty() && *l != "-1")
    }
}

/// One thing an audio pick may honour. [`audio_intents`] ranks them — THE precedence, decided
/// once, here, for every path that names an audio track: the direct-play pick
/// ([`pick_dp_audio_pref`]) and the remux / re-encode pick ([`encode_audio_id`]) each walk that
/// same ranking and take the first entry they can carry, so the two cannot rank the same inputs
/// differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AudioIntent<'a> {
    /// PMS's selection (`Stream.selected` on a stream that is NOT the file's `default`). It may be
    /// a manual pick OR PMS's automatic account-language pick; the wire cannot distinguish them.
    Selection(usize),
    /// A Plex language preference the item has a track in.
    Language(&'a str),
    /// Nothing to honour: the file's own default track, then any direct-playable one.
    FileDefault,
}

/// Order: a PMS per-part selection > the Plex language preferences in [`AudioLangPrefs`] order (a
/// preference with no track in it is left out) > the file's default. A path that cannot carry an
/// entry moves on to the NEXT one — a French DTS pick with no direct-playable French sibling
/// falls to the show's language before the file's default, not straight to the default.
///
/// A selection must differ from the file's `default` flag, because PMS reports a selected AUDIO
/// stream on essentially every part. Live measurement also shows PMS applying the requesting
/// profile's `defaultAudioLanguage` to `Stream.selected`: that auto-choice and a manual choice are
/// indistinguishable, so a selected non-default stream deliberately outranks the show preference.
/// A selected stream that is ALSO the file default carries no extra intent and resolves through
/// [`AudioIntent::FileDefault`] unless a more specific readable preference points elsewhere.
///
/// A PMS selection ranks first whatever its codec: the paths differ only in how they CARRY it (direct play
/// takes a direct-playable sibling in its language, a re-encode encodes the track itself). A
/// preference below it therefore never wins just because the pick is a DTS.
fn audio_intents<'a>(tracks: &[crate::metadata::Stream], prefs: AudioLangPrefs<'a>) -> Vec<AudioIntent<'a>> {
    let selection = tracks.iter().position(|s| s.selected && !s.default)
        .map(AudioIntent::Selection);
    let langs = prefs
        .in_order()
        .filter(|l| tracks.iter().any(|s| lang_matches(l, &s.lang_code)))
        .map(AudioIntent::Language);
    selection.into_iter().chain(langs).chain(std::iter::once(AudioIntent::FileDefault)).collect()
}


/// Pick the audio track to DIRECT-PLAY from the playing item's track store
/// (metadata::playing(), loaded by build_stream), returning (list_idx, codec, stream_id):
/// list_idx -1 = codec-default (demuxer matches by payload codec — only when the track list is
/// unavailable), else the index into `playing().audio`, with that track's Plex stream id so the
/// timeline can report the truth. [`audio_intents`] ranks what to honour; this takes the first
/// entry it can carry:
///   - [`AudioIntent::Selection`]: that track when it is direct-playable, else a direct-playable track
///     in ITS language (an unsupported English DTS pick plays as the English AC3 beside it, not
///     as the default dub) — the Load payload uses THAT track's codec so there is no mismatch;
///   - [`AudioIntent::Language`]: the first direct-playable track in it;
///   - [`AudioIntent::FileDefault`]: the file's flagged default track if its codec
///     is direct-playable — by EXPLICIT index (matching by codec alone fed the first same-codec
///     stream, not the flagged default, when another track of that codec preceded it);
///   - then any other direct-playable track (TrueHD/DTS-default item with an AC3 sibling —
///     smart-DP).
/// None when NO audio track is direct-playable (→ transcode).
///
/// An intent this path cannot carry falls through to the default rather than forcing a transcode
/// to obey it, which would drop the whole smart-direct-play class (a TrueHD/DTS pick with an AC3
/// sibling) onto the server's video-downscaling encoder for one audio track.
///
/// PURE: takes the playing item's audio tracks explicitly instead of reaching into
/// `metadata::playing()`. That matters twice over. (a) `playing()` (via `MetadataView`) hands out
/// a `&'a PlayingItem` whose `Vec`s `ui/track_menu.rs` and `ui/info_panel.rs` hold slices into
/// during playback — a worker replacing the store would drop those out from under the draw path,
/// so the resolve must never touch it. (b) Being pure makes the selection ladder host-testable,
/// which it has never been; see the tests at the foot of this file.
#[cfg(test)]
pub(super) fn pick_dp_audio(
    tracks: &[crate::metadata::Stream],
    default_acodec: &str,
) -> Option<(i32, String, i64)> {
    pick_dp_audio_pref(tracks, default_acodec, AudioLangPrefs::default())
}

/// [`pick_dp_audio`] with the item's Plex language preferences. The SHOW's setting mattered
/// first: it lives on the SHOW, an episode's part carries nothing of it, and when the preferred
/// dub is the file's default flag the server's selection cannot tell it from no choice — so a
/// series set to Hungarian opened in English (#160).
pub(super) fn pick_dp_audio_pref(
    tracks: &[crate::metadata::Stream],
    default_acodec: &str,
    prefs: AudioLangPrefs<'_>,
) -> Option<(i32, String, i64)> {
    pick_dp_audio_mode(tracks, default_acodec, prefs, DirectPlayMode::Auto)
}

pub(super) fn audio_direct_plays(mode: DirectPlayMode, codec: &str, channels: i64) -> bool {
    if mode == DirectPlayMode::Forced {
        crate::plex::DP_AUDIO_CODECS.split(',').any(|c| c.eq_ignore_ascii_case(codec))
    } else {
        crate::plex::is_dp_audio_track(codec, channels)
    }
}

fn pick_dp_audio_mode(
    tracks: &[crate::metadata::Stream], default_acodec: &str,
    prefs: AudioLangPrefs<'_>, mode: DirectPlayMode,
) -> Option<(i32, String, i64)> {
    pick_dp_audio_eligible(tracks, default_acodec, prefs,
        |codec, channels| audio_direct_plays(mode, codec, channels))
}

fn pick_dp_audio_eligible(
    tracks: &[crate::metadata::Stream], default_acodec: &str, prefs: AudioLangPrefs<'_>,
    eligible: impl Fn(&str, i64) -> bool,
) -> Option<(i32, String, i64)> {
    let dp = |codec: &str| eligible(codec, 0);
    if tracks.is_empty() {
        // no track info — fall back to the codec-default (or transcode if that isn't DP)
        return if dp(default_acodec) {
            Some((-1, default_acodec.to_string(), 0))
        } else {
            None
        };
    }
    let pick = |i: usize| (i as i32, tracks[i].codec.to_lowercase(), tracks[i].id);
    let dp_at = |s: &crate::metadata::Stream| eligible(&s.codec, s.channels);
    let honoured = audio_intents(tracks, prefs).into_iter().find_map(|intent| match intent {
        AudioIntent::Selection(i) if dp_at(&tracks[i]) => Some(i),
        AudioIntent::Selection(i) => tracks
            .iter()
            .position(|s| dp_at(s) && lang_matches(&tracks[i].lang_code, &s.lang_code)),
        AudioIntent::Language(l) => tracks
            .iter()
            .position(|s| dp_at(s) && lang_matches(l, &s.lang_code)),
        // the file's flagged default track, if direct-playable (explicit index)
        AudioIntent::FileDefault => tracks.iter().position(|s| s.default && dp_at(s)),
    });
    if let Some(i) = honoured {
        return Some(pick(i));
    }
    if !tracks.iter().any(|s| s.default) {
        // Once PMS supplied tracks, their concrete channel count outranks the codec-only
        // Media default. Never erase a known 8-channel refusal by rechecking it as unknown/0.
        if let Some(i) = tracks.iter().position(|s| s.codec.eq_ignore_ascii_case(default_acodec) && dp_at(s)) {
            return Some(pick(i));
        }
    }
    // any direct-playable track (smart direct-play over a non-DP default)
    tracks.iter().position(dp_at).map(pick)
}

/// Stream id named on the remux/re-encode PUT and start.mkv — [`audio_intents`]' ranking, carried
/// by an encoder instead of a direct play (the first entry with a usable id wins).
///
/// A remux COPIES, so this is the smart-DP sibling (`dp_audio_id`) — putting a selected
/// TrueHD or unsupported DTS track would ship audio the TV cannot decode. `env_audio_sid` is the session/retry
/// pick and wins on re-encode when set, including a remux leftover sibling (mid-play quality drop
/// keeps what is already playing); a cold play zeros it (`request_play`). Otherwise:
///   - [`AudioIntent::Selection`]: that track itself — a re-encode can transcode a selected DTS to
///     AC3, so naming the sibling would replace English DTS with a foreign AC3 copy;
///   - [`AudioIntent::Language`]: the direct-play pick when it is already in that language (so
///     lowering video quality preserves a preferred dub and does not needlessly encode a lossless
///     sibling), else the first track in it, whatever its codec;
///   - [`AudioIntent::FileDefault`], or an unusable id: the direct-play pick itself, so a
///     transcode speaks the language direct play would have — the file's default when it is
///     direct-playable; `0` when nothing is, and an omitted PUT encodes the part default.
fn encode_audio_id(
    remux: bool,
    dp_audio_id: i64,
    env_audio_sid: i64,
    tracks: &[crate::metadata::Stream],
    prefs: AudioLangPrefs<'_>,
) -> i64 {
    if remux {
        return dp_audio_id;
    }
    if env_audio_sid > 0 {
        return env_audio_sid;
    }
    audio_intents(tracks, prefs)
        .into_iter()
        .find_map(|intent| match intent {
            AudioIntent::Selection(i) => Some(tracks[i].id).filter(|&id| id > 0),
            AudioIntent::Language(l)
                if tracks.iter().any(|s| s.id == dp_audio_id && lang_matches(l, &s.lang_code)) =>
            {
                Some(dp_audio_id)
            }
            AudioIntent::Language(l) => {
                tracks.iter().find(|s| s.id > 0 && lang_matches(l, &s.lang_code)).map(|s| s.id)
            }
            AudioIntent::FileDefault => Some(dp_audio_id),
        })
        .unwrap_or(dp_audio_id)
}


/// Whether the client can render this embedded subtitle codec.
fn embedded_subtitle_renderable(codec: &str) -> bool {
    // Advertised bitmap/ASS/text codecs plus ff::sub_kind's raw UTF-8 packet formats.
    crate::plex::is_dp_subtitle(codec) || matches!(codec,
        "vplayer" | "pjs" | "jacosub" | "microdvd" | "sami" | "realtext" |
        "subviewer" | "subviewer1" | "stl" | "mpl2")
}

/// The subtitle to turn ON at the start of a DIRECT-PLAY, from the server's own per-part
/// selection — returning (stream id, embedded-subtitle ordinal for the client renderer), or
/// None to start with subtitles off (the shipped behaviour when the server has no selection).
///
/// This is the read-back half of `put_selection`: we have always written the user's pick to
/// `/library/parts/…` and never consulted the one already there, so a subtitle enabled from Plex
/// Web or a phone was dropped on the floor at every play. The ordinal is
/// `metadata::sub_render_ordinal`, i.e. the SAME identifier space the track menu commits and the
/// demuxer enumerates (embedded streams only, sorted on PMS `Stream.index`) — not a list position.
///
/// Unlike the audio rung this carries no "is it a real pick?" gate, because subtitles do have a
/// "nothing selected" state and use it: probed against the live server, parts carrying a
/// `default`-flagged subtitle come back with no selection at all, so a selection is a choice even
/// when it lands on the container default. The case that would blur it is an ACCOUNT-level
/// subtitle mode (always-show / auto-select forced), which makes PMS select a stream nobody
/// picked on this part — subtitles would then come up on every direct play of a foreign-audio
/// item. That is self-correcting (turning them off PUTs `subtitleStreamID=0`, which is a real
/// per-part override) and it is arguably the account setting working, but if it ever needs
/// suppressing, the gate belongs here — not on the flag itself.
///
/// Two deliberate limits of this embedded-track selection:
///   - an EXTERNAL (sidecar) selection returns None because it has no container ordinal.
///     Renderable text sidecars are restored separately by `apply_plan`; image sidecars
///     still require a server burn.
///   - this is the direct-play path only. The transcode path keeps PUTting `subtitleStreamID=0`
///     (subs off) as before: honouring a selection there means a server-side BURN, i.e. a
///     re-encode carrying a picture-quality cost, which is a trade to put behind the settings
///     surface explicitly rather than to make silently at every play. Once a
///     direct-played item DOES go to the transcoder mid-session (an unsupported DTS/TrueHD audio pick), the
///     seeded `cur_sub_sid` rides along, so the subtitle already on screen keeps burning. Note the
///     read-back is therefore ONE-WAY on that path: an item that starts as a transcode still PUTs
///     `subtitleStreamID=0`, which not only suppresses the burn but CLEARS the server's selection
///     for everyone. That predates this change; honouring it instead is the same burn decision.
pub(super) fn pick_dp_subtitle(subs: &[crate::metadata::Stream]) -> Option<(i64, i32)> {
    let i = subs.iter().position(|s| s.selected && !s.external)?;
    let ord = crate::metadata::sub_render_ordinal(subs, i);
    // Both halves must be usable or neither is: the id is what the menu checkmark and the
    // timeline report key on, so rendering a stream we cannot NAME would show a subtitle while
    // the menu says Off. (`ord < 0` is unreachable through the `!external` filter above — it is
    // kept so a change on either side degrades to "off" instead of feeding the renderer a -1.)
    if ord < 0 || subs[i].id <= 0 || !embedded_subtitle_renderable(&subs[i].codec) {
        return None;
    }
    Some((subs[i].id, ord))
}

/// Stream id named on the MDE `/decision` handshake, or `0`.
///
/// [`pick_dp_subtitle`] is what Original will client-render. MDE only sees that id when the
/// codec is in [`crate::plex::DP_SUBTITLE_CODECS`]: a sidecar, or a selected embedded track
/// we render but do not advertise (`vplayer`, …), is sent as `0` so MDE evaluates subs off
/// instead of answering transcode (which then forbids a codec-copy remux).
#[cfg(test)]
fn mde_subtitle_stream_id(subs: &[crate::metadata::Stream]) -> i64 {
    mde_subtitle_id_of(subs, pick_dp_subtitle(subs))
}

/// [`mde_subtitle_stream_id`] for a subtitle already decided — the resolve decides ONCE
/// (`pick_dp_subtitle_pref`) and names the result here.
fn mde_subtitle_id_of(subs: &[crate::metadata::Stream], pick: Option<(i64, i32)>) -> i64 {
    pick
        .and_then(|(id, _)| {
            subs.iter()
                .find(|s| s.id == id)
                .filter(|s| crate::plex::is_dp_subtitle(&s.codec))
                .map(|_| id)
        })
        .unwrap_or(0)
}


/// Account subtitle defaults; each explicit show field overrides its inherited counterpart.
/// PMS per-part selection always wins. Automatic picks stay embedded/client-rendered: selecting
/// an account default never requests a subtitle burn on the transcode path.
#[derive(Clone, Copy, Default)]
pub(super) struct SubtitleLangPrefs<'a> {
    pub language: Option<&'a str>,
    pub mode: i64,
    pub forced: i64,
}

#[cfg(test)]
pub(super) fn pick_dp_subtitle_pref(
    subs: &[crate::metadata::Stream], prefs: &crate::plex::ShowLangPrefs, audio_lang: &str,
) -> Option<(i64, i32)> {
    pick_dp_subtitle_account(subs, prefs, SubtitleLangPrefs::default(), audio_lang)
}

fn pick_dp_subtitle_account(
    subs: &[crate::metadata::Stream], prefs: &crate::plex::ShowLangPrefs,
    account: SubtitleLangPrefs<'_>, audio_lang: &str,
) -> Option<(i64, i32)> {
    if let Some(pick) = pick_dp_subtitle(subs) {
        return Some(pick);
    }
    if subs.iter().any(|s| s.selected) {
        return None;
    }
    let mode = if prefs.subtitle_mode == -1 { account.mode } else { i64::from(prefs.subtitle_mode) };
    let lang = prefs.subtitle.as_deref().or(account.language)?;
    let want = match mode {
        2 => true,
        1 => !audio_lang.is_empty() && !lang_matches(lang, audio_lang),
        _ => false,
    };
    if !want {
        return None;
    }
    let embedded = |i: usize| {
        let ord = crate::metadata::sub_render_ordinal(subs, i);
        (ord >= 0 && subs[i].id > 0).then_some((subs[i].id, ord))
    };
    let prefer_forced = account.forced == 1 || account.forced == 2;
    let tiers: [&dyn Fn(&crate::metadata::Stream) -> bool; 3] = [
        &|s| s.forced == prefer_forced && !s.sdh,
        &|s| s.forced == prefer_forced,
        &|_| true,
    ];
    tiers.iter().find_map(|tier| {
        (0..subs.len())
            .filter(|&i| !subs[i].external && embedded_subtitle_renderable(&subs[i].codec)
                && lang_matches(lang, &subs[i].lang_code)
                && (account.forced != 2 || subs[i].forced)
                && (account.forced != 3 || !subs[i].forced)
                && tier(&subs[i]))
            .find_map(embedded)
    })
}

/// Software feed formats and Dolby Vision declaration support, independent of device limits.
pub(super) fn video_feed_supported(vcodec: &str, dv: crate::metadata::DvPresentation) -> bool {
    matches!(vcodec, "h264" | "hevc") && dv.refusal().is_none()
}

pub(super) fn direct_play_policy(mode: DirectPlayMode, policy: crate::plex::LinkPolicy) -> crate::plex::LinkPolicy {
    match mode {
        DirectPlayMode::Auto => policy,
        DirectPlayMode::Forced => crate::plex::LinkPolicy { direct_play: true, remux: false },
        DirectPlayMode::Disabled => crate::plex::LinkPolicy { direct_play: false, remux: policy.remux },
    }
}

/// PURE: the local direct-play VIDEO test — the codec, the source's stated frame size and its
/// Dolby Vision layering must ALL clear what this device and this pipeline can actually show.
///
/// The codec half: h264 unconditionally (every webOS SoC decodes it), hevc only when the table
/// lists the decoder — anything else the pipeline cannot feed at all. The resolution half is the
/// local agreement with the profile's `*`-scoped `video.width`/`video.height` limitation: the
/// profile makes PMS transcode a 4K source down for a 1080p-bounded SoC, but when `/decision` is
/// unreachable the fallback never asks PMS, so without this test a 4K file with one
/// direct-playable audio track was fed verbatim to a decoder whose table says 1920x1088 — the
/// wrong-side failure devcaps' own doc names (issue #22's over-claim class), invisible on the
/// dev TV, whose bound is 4096x2176.
///
/// **The Dolby Vision half is the same shape of bug, found the same way, and it is NOT about the
/// decoder.** Every profile's base layer is ordinary HEVC and every one of them decodes here — so
/// a codec-name gate cannot see the difference, which is exactly why this one is needed. What
/// differs is whether the base layer MEANS anything on its own: Profile 8.1's does (it is HDR10,
/// and dropping the RPU costs only the dynamic metadata), Profile 5's does not (single-layer
/// IPT-PQ, no fallback — it decodes cleanly and displays in visibly wrong colours), and Profile
/// 7's is only half the picture.
///
/// **That half arrives here already DECIDED**, as a [`DvPresentation`] rather than as the raw
/// record, and that is the point: the same value the caller passes here is the value the Load
/// payload reads for its `DolbyHdrInfo` node. A stream we DECLARE is one the pipeline puts in
/// Dolby Vision mode, so Profile 5 direct-plays correctly and this gate must let it through; a
/// stream we do not declare falls back to `Dovi::base_layer_unusable`, the pre-declaration rule,
/// which carries the never-convict-on-silence reasoning. Taking the decision as an argument is
/// what makes "the gate and the payload can never disagree" checkable in one place —
/// [`Dovi::presentation`] — instead of being a coincidence between two functions.
///
/// **Refusing here is only half the work, and the other half is not in this function.** A refusal
/// sends the item down the transcode branch — but that branch's query grants PMS `directStream=1`,
/// permission to COPY the video rather than encode it, and the server takes it whenever the source
/// fits the caps: resolution, bitrate, and the profile's own limitation axes. None of those can say
/// "Dolby Vision", so a refused Profile 5 came back `Part.decision=transcode` with the video's own
/// decision `copy` — the same bitstream, the same wrong colours, one container down. `build_stream`
/// therefore also sets [`crate::plex::TranscodeSpec::no_video_copy`], off `base_layer_unusable` and
/// never off this gate: a COPY carries no declaration, so it stays wrong even for a profile we are
/// happy to direct-play. The measurement is in `docs/pms-api.md` §"What the server actually does
/// with a Dolby Vision source". A server that cannot encode the result is then allowed to say so —
/// this PMS answers general code 2000, *"File is unplayable. DoVi (Profile 5) color space is not
/// supported."*, which [`DvPresentation::Refuse`] turns into the player's read-out. A read-out that
/// names the reason is the honest end of that road; a picture in the wrong colours is not.
///
/// Unknown dimensions (0) PASS: PMS omitting a Media attribute is not evidence of 4K, and
/// failing open is yesterday's behavior for every file the server never measured — the same
/// misread-degrades-to-assumed rule `devcaps::parse` applies, and `Dovi` applies it too.
pub(super) fn video_direct_plays(
    vcodec: &str,
    src_w: i64,
    src_h: i64,
    dv: crate::metadata::DvPresentation,
    caps: &crate::devcaps::Caps,
) -> bool {
    let codec_ok = vcodec == "h264" || (vcodec == "hevc" && caps.hevc);
    let (bw, bh) = caps.hevc_max;
    codec_ok && src_w <= bw as i64 && src_h <= bh as i64 && dv.refusal().is_none()
}


/// The detail page's "how this plays" answer, BEFORE anything is played — the same FOUR gates
/// `build_stream` will apply (codec+resolution via [`video_direct_plays`], container via
/// [`part_is_streamable`], one direct-playable audio track, and the user's quality ceiling via
/// [`quality_policy`] — applied last and able only to downgrade), asked of the loaded `Detail`.
/// The ceiling is the one a reader debugging "why does this ordinary h264/AC-3 MKV say Converts"
/// will not think of, which is why it is named in the list rather than left to the code.
/// An approximation by design: the real decision can still consult the server (`server_decision`
/// when no DP audio track is found), so this leans the same way that fallback usually lands.
/// It exists for `Details Screen.dc.html`'s facts row and must stay a READ-ONLY preview —
/// nothing in the playback path may branch on it (the path re-derives for itself).
///
/// **THREE answers, not two, and the third is the one a two-valued preview got wrong.** "The
/// server has to do something" and "the server has to re-encode the picture" are different facts
/// (`is_remux`'s doc says so for the LIVE session; this is the same distinction before Play), and
/// the UI hangs a Plex Pass claim on the difference: hardware conversion and HDR tone mapping are
/// both properties of an ENCODE, so naming either one for a stream where no encoder runs points
/// the user at a purchase that would fix nothing — `player::error_shape`'s own rule, and the
/// polarity issue #22 is about.
#[derive(PartialEq, Clone, Copy, Debug)]
pub(crate) enum Preview {
    DirectPlay,
    /// Container-only REMUX — Plex's own "Direct Stream". The video (and usually the audio) is
    /// COPIED into progressive MKV because the container is not one the demuxer streams, or
    /// because no audio track direct-plays; the pixels arrive untouched, 4K and HDR10 intact.
    /// `build_stream` spells this exact case `plan.contract.remux = video_dp` on the transcode
    /// branch.
    Remux,
    /// A real re-encode: the server decodes and re-encodes the video.
    Converts,
}

/// [`playback_preview`]'s pure core — the three-way answer from the fields it actually needs, so
/// a caller holding an EPISODE's file and a show's stream list can ask the same question.
pub(crate) fn playback_preview_of(
    part: &str,
    vcodec: &str,
    width: i64,
    height: i64,
    dv: crate::metadata::DvPresentation,
    audio_streams: &[crate::metadata::Stream],
) -> Option<Preview> {
    if part.is_empty() {
        return None; // nothing playable loaded (a show still resolving its episode)
    }
    let video = video_direct_plays(vcodec, width, height, dv, crate::devcaps::caps());
    let audio = audio_streams
        .iter()
        .any(|a| crate::plex::is_dp_audio_track(&a.codec, a.channels));
    // Mirrors `build_stream`'s own ladder: the video gate decides whether an ENCODER runs at all,
    // and only once it has passed do the container and the audio decide between pulling the file
    // ourselves and asking the server to repackage it.
    Some(if !video {
        Preview::Converts
    } else if part_is_streamable(part) && audio {
        Preview::DirectPlay
    } else {
        Preview::Remux
    })
}


/// True when the part's container is one the buffer-feed demuxer streams over HTTP: MKV, or
/// MP4/M4V since the AVIO became seekable (see the `streamable` note at the decision site — the
/// old mkv-only gate was measured obsolete on-device 2026-08-11). Other containers (mov/avi/…)
/// are sent to Plex for a container remux instead of direct-play. Matches the container
/// extension in the part-key filename; the m4v spelling is the same mov demuxer and the same
/// `container=mp4` in PMS metadata.
pub(super) fn part_is_streamable(part_key: &str) -> bool {
    let name = part_key.rsplit('/').next().unwrap_or(part_key);
    let name = name.split('?').next().unwrap_or(name);
    name.ends_with(".mkv") || name.ends_with(".mp4") || name.ends_with(".m4v")
}


/// Extract the numeric Part id from a Plex part key (/library/parts/{id}/…/file.mkv).
pub(super) fn part_id_of(part_key: &str) -> i64 {
    let mut it = part_key.split('/');
    while let Some(seg) = it.next() {
        if seg == "parts" {
            return it.next().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
        }
    }
    0
}

// ---- async resolve: worker computes an owned Plan, main thread installs it ------------------
// The house idiom (metadata::load_season / browse.rs): generation counter + single-flight +
// a monotone one-slot mailbox + a per-frame pump that applies on the MAIN thread.
//
// Cancellation is FLAG-ONLY by design: `cancel_play` bumps the generation so a landing is
// discarded, but it cannot wake a worker blocked in recv(2) — publishing the socket fd to make
// that possible broke the seek path and was reverted (docs/async-model-decision.md). That costs
// nothing here: the freeze is fixed by getting the resolve OFF the loop, and a worker lingering
// in the background is invisible once the UI has already moved on.

pub(super) struct AbandonedPlanResources {
    pub(super) sid: ServerId,
    pub(super) identities: Vec<String>,
}

pub(super) fn abandoned_plan_resources(plan: &Plan) -> Option<AbandonedPlanResources> {
    let mut identities = Vec::with_capacity(2);
    if !plan.tsession.is_empty() {
        identities.push(plan.tsession.clone());
    }
    if !plan.sess.is_empty() && !identities.iter().any(|id| id == &plan.sess) {
        identities.push(plan.sess.clone());
    }
    if identities.is_empty() {
        None
    } else {
        Some(AbandonedPlanResources {
            sid: plan.sid,
            identities,
        })
    }
}


pub(super) fn take_resume_for(pending: &mut Option<(u32, i64)>, gen: u32) -> i64 {
    match pending.take() {
        Some((owner, ns)) if owner == gen => ns,
        Some(other) => {
            // A later request already owns this value. Put it back; this landing cannot steal
            // another generation's position.
            *pending = Some(other);
            0
        }
        None => 0,
    }
}

#[cfg(test)]
#[path = "plan_test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "plan_session_tests.rs"]
mod session_tests;

#[cfg(test)]
#[path = "plan_quality_ceiling_tests.rs"]
mod quality_ceiling_tests;

#[cfg(test)]
#[path = "plan_track_selection_tests.rs"]
mod track_selection_tests;

#[cfg(test)]
#[path = "plan_dolby_vision_tests.rs"]
mod dolby_vision_tests;

#[cfg(test)]
#[path = "plan_mde_decision_tests.rs"]
mod mde_decision_tests;
