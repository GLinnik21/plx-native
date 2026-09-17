//! The PURE half of the route split (spec §9, §15.2): functions of their arguments alone, plus
//! the plain data types they need. Nothing here reads a `static mut`, touches [`super::decision`]'s
//! `SESSION`/`PLAYER_CONTROL`/`PLAY_SLOT`/`ENCODER_CLEANUP`/`SCROBBLE_JOIN`/`TIMELINE_STOP_FENCE`/
//! `QUALITY`, or calls `task::spawn*` — that is what makes [`build_stream`] safe to run on the
//! resolve worker. Everything else in the former `route.rs` (session state, the synchronized
//! `PlayerControl`, PMS/native I/O, the encoder/scrobble/timeline machinery) lives in
//! [`super::decision`]. `ci/check-deps.sh`'s `wall` gate holds this file to zero
//! `Instant::now`/`SystemTime::now`/`.elapsed()`; a function that needs wall time is not pure and
//! belongs in `decision.rs`.

use crate::plex::ServerId;
use std::sync::atomic::Ordering;

use super::decision::{
    measure_remote_original, measure_remote_remux, put_selection, resolve_playqueue,
    server_decision, ActiveEncoderState, AutomaticRouteIntent, MdeVerdict, PlayerControl,
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
    pub(super) acodec: String,
    pub(super) fps: f64,
    pub(super) dovi: crate::metadata::Dovi,
    pub(super) immersive: bool,
    pub(super) audio_sid: i64,
    pub(super) audio_ordinal: Option<i32>,
    pub(super) subtitle_ordinal: Option<i32>,
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
/// `ceiling` is an ARGUMENT rather than a read of [`quality`], for the same reason `remux` and
/// `no_video_copy` are: [`build_stream`] runs on the resolve worker and must take it from
/// [`ResolveEnv`], while [`retranscode`] runs on the main thread and reads the live selection. A
/// read inside here would be a `static` touched from a worker.
pub(super) fn transcode_spec<'a>(
    rk: &'a str,
    session: &'a str,
    encoder_session: &'a str,
    remux: bool,
    no_video_copy: bool,
    offset: crate::plex::TranscodeOffset,
    aud: i64,
    sub: i64,
    ceiling: Option<crate::plex::Ceiling>,
    delivery: crate::plex::TranscodeDelivery,
) -> crate::plex::TranscodeSpec<'a> {
    crate::plex::TranscodeSpec {
        rating_key: rk,
        session,
        encoder_session,
        delivery,
        remux,
        no_video_copy,
        audio_stream_id: aud,
        subtitle_stream_id: sub,
        offset,
        ceiling,
    }
}


pub(crate) use crate::plex::session::PlaybackQuality as Quality;


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
    /// the loaded detail's streams when it IS this item — saves the worker a GET
    pub cached_item: Option<crate::metadata::PlayingItem>,
    /// The user's pick off the quality ladder, captured at the press like everything else here.
    /// The worker must not call [`quality`] itself for the reason this struct exists: it reads a
    /// process-global the main thread can move while the resolve is in flight.
    pub quality: Quality,
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
    /// Does the direct-played audio track carry Dolby Atmos, for the Load payload's
    /// `contents.immersive` node. Set on the DIRECT-PLAY branch only, for the same reason `dovi`
    /// is: it describes the FILE's own elementary stream.
    pub immersive: bool,
    pub audio_sid: i64,
    pub remux: bool,
    /// The selected transcode delivery. Direct play leaves the progressive default unused.
    pub delivery: crate::plex::TranscodeDelivery,
    /// This plan's transcode may not be satisfied by a video stream COPY — the flag rides all the
    /// way to `plex::TranscodeSpec::no_video_copy`, and `apply_plan` stores it so a seek or an
    /// audio switch rebuilds the same constraint. Set only where the refusal is about what the
    /// pixels ARE (a Dolby Vision base layer we cannot display), never for a size or codec one:
    /// those the server's own caps already express, and a copy that satisfies them is a free win.
    pub no_video_copy: bool,
    /// The fixed quality ceiling this plan resolved under (`None` = Original, including Auto's
    /// proven Original state; adaptive Auto begins at whatever rung [`crate::abr::bootstrap`]
    /// returned — 480p when nothing about the link is knowable for free, otherwise the catalog
    /// entry its bounded source probe pays for) —
    /// installed as [`Session::cur_ceiling`] so a seek or a track switch rebuilds the SAME query. Copied
    /// straight from `env.quality.ceiling()`, for the same reason `sid` is copied from the env:
    /// the worker must not re-read a preference the main thread can move underneath it.
    pub ceiling: Option<crate::plex::Ceiling>,
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
    /// the subtitle stream the server already had selected for this part (0 = none/off), so the
    /// menu checkmark and the timeline report agree with what is on screen — and a later
    /// transcode of this item burns the subtitle the user was already watching.
    pub sub_sid: i64,
    /// client-renderer ordinal for that subtitle (`metadata::sub_render_ordinal`). None = subs off.
    pub sub_render_ordinal: Option<i32>,
    /// the playing item's track store, fetched off-thread and installed by apply_plan
    pub playing: Option<crate::metadata::PlayingItem>,
    /// The server's PRE-FLIGHT refusal (see [`refusal`]), when `/decision` said it can neither
    /// direct play nor convert this item. A plan carrying one has an EMPTY `url` by construction —
    /// that is how it fails, on the same path as every other unresolvable plan — and the sentence
    /// rides along so the read-out can quote the server instead of guessing. `None` on every other
    /// plan, including one that simply failed to reach the server.
    pub verdict: Option<String>,
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
    // Server-adjudicated: the Media Decision Engine decides direct-play vs transcode from our
    // capability profile. An unusable / unreachable `/decision` must not Original (PMS 1.43
    // 503s a Part without a registered decision); remux/re-encode still registers via a
    // separate `transcode_decision`. The local-sample/demo path (rk empty) skips MDE entirely.
    // Smart direct-play: the video decodes natively (H264/HEVC) AND some audio track is
    // direct-playable (AAC/AC3/E-AC3) — even if the DEFAULT track isn't. We own the demuxer, so
    // we direct-play the raw file and FEED a direct-playable track (e.g. a 4K HEVC item: TrueHD
    // default + an AC3 track → native 4K HEVC + AC3, no transcode — beats the server's
    // video-downscaling transcode). The chosen audio rides `audioStreamID` on `/decision` so MDE
    // evaluates that sibling rather than vetoing the TrueHD/DTS default. When `/decision` is
    // unreachable the plan fails closed (no Original Part — PMS 1.43 503s without a registered
    // decision) and may still remux/re-encode; an explicit MDE transcode also forbids remux.
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
    // ONE predicate, resolved once: it answers the direct-play gate here and the Load payload's
    // `DolbyHdrInfo` node later (`engine::build_av_payload`, off `stream_dovi()` + the same
    // latched trigger). Two predicates is what this used to be, and the pair could disagree —
    // which for Dolby Vision means either a declared stream we refused to play or, worse, a
    // Profile 5 direct-played with nothing declared: the wrong colours, back again.
    let dv = dovi.presentation_now();
    let video_dp = video_direct_plays(vcodec, src_w, src_h, dv, crate::devcaps::caps());
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
    let audio_sel = if rk.is_empty() {
        None
    } else {
        pick_dp_audio(tracks, acodec)
    };
    let audio_id = audio_sel.as_ref().map(|(_, _, id)| *id).unwrap_or(0);
    let subtitle_id = plan
        .playing
        .as_ref()
        .map(|p| mde_subtitle_stream_id(&p.subs))
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
    let tentative_quality = quality_policy(env.quality, true, env.src_kbps, src_w, src_h);
    let mut allowed = flavors_allowed(link, tentative_quality);
    // MDE verdict for this resolve: Some(original)=Part.decision=directplay, Some(!original)=
    // start.mkv, None=unreachable/unusable OR never asked (gates already refused Original).
    // PMS 1.43 503s a Part GET without a registered decision, so None must never become Original.
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
        // the sibling; a re-encode names a real selected pick or pref-lang English so 720p
        // does not copy a foreign AC3 sibling.
        server_decision(client, rk, &session, audio_id, subtitle_id)
    };
    let mut directplay = mde.as_ref().is_some_and(|v| v.original);
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
    if matches!(env.quality, Quality::Auto | Quality::Original)
        && matches!(
            location,
            Some(crate::plex::probe::Location::Local) | Some(crate::plex::probe::Location::Remote)
        )
        && (directplay || remux_candidate)
        && !part.is_empty()
    {
        let (aidx, achosen, asid) = audio_sel
            .as_ref()
            .map(|(idx, codec, sid)| (*idx, codec.clone(), *sid))
            .unwrap_or((-1, acodec.to_string(), 0));
        let direct = directplay;
        let fps = if direct {
            plan.playing.as_ref().map(|p| p.video_fps).unwrap_or(0.0)
        } else {
            0.0
        };
        let immersive = direct
            && plan
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
        let audio_ordinal = if direct && aidx >= 0 {
            Some(
                plan.playing
                    .as_ref()
                    .map(|p| crate::metadata::audio_ordinal(&p.audio, aidx as usize))
                    .unwrap_or(aidx),
            )
        } else {
            None
        };
        let subtitle_ordinal = direct
            .then(|| {
                plan.playing
                    .as_ref()
                    .and_then(|p| pick_dp_subtitle(&p.subs))
                    .map(|(_, ord)| ord)
            })
            .flatten();
        plan.auto_original = Some(AutoOriginalCandidate {
            url: client.direct_play_url(part, &session).to_url(),
            probe_part: part.to_owned(),
            direct,
            vcodec: vcodec.to_string(),
            acodec: achosen,
            fps,
            dovi: if direct {
                dovi
            } else {
                crate::metadata::Dovi::NONE
            },
            immersive,
            audio_sid: asid,
            audio_ordinal,
            subtitle_ordinal,
        });
    }
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
    // A preview that is already not direct-playable (MDE denied Original, or the extra carries
    // no Part) is refused unconditionally below by `preview::accepts_direct_play` regardless of
    // what Auto's bandwidth probe would decide — `adaptive` cannot rescue a `directplay=false`
    // preview. Skipping the probe here (and, on the remux leg, the `put_selection` PUT that
    // would otherwise register a transcode this preview can never play) is the only branch worth
    // guarding: it is the one place this function does live network I/O before that refusal
    // check, and a preview is exactly the request most likely to hit a non-direct-playable item.
    let preview_already_refused = env.preview && (!directplay || part.is_empty());
    let decision = match (env.quality, link_kind) {
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
                        let probe_audio = encode_audio_id(true, audio_id, env.audio_sid, tracks);
                        put_selection(env.sid, plan.part_id, probe_audio, env.sub_sid);
                        measure_remote_remux(
                            client,
                            rk,
                            &session,
                            probe_audio,
                            env.sub_sid,
                            source_transport_kbps,
                        )
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
    let adaptive = auto_uses_hls(env.quality, auto_original);
    if adaptive {
        allowed = flavors_allowed(
            link,
            quality_policy(env.quality, false, env.src_kbps, src_w, src_h),
        );
        directplay = false;
        plan.delivery = crate::plex::TranscodeDelivery::FixedHls {
            seconds_per_segment: 2,
        };
        let rung = decision
            .as_ref()
            .map(|d| d.rung)
            .unwrap_or(crate::abr::Rung::P480);
        plan.ceiling = Some(rung.ceiling());
        crate::player::log(&format!(
            "route: Auto adaptive — source {source_transport_kbps}kbps {src_w}x{src_h}; starting {}kbps HLS ({:?})",
            rung.kbps(),
            decision.as_ref().map(|d| d.reason),
        ));
    } else {
        plan.ceiling = env.quality.ceiling();
        if env.quality == Quality::Auto {
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
    plan.auto_original_watched = env.quality == Quality::Auto && auto_original;
    if env.quality != Quality::Auto && !tentative_quality.direct_play {
        crate::player::log(&format!(
            "route: quality ceiling {} — source {}kbps {src_w}x{src_h}; denying direct play + remux, re-encoding",
            env.quality.label(),
            env.src_kbps
        ));
    }
    if env.preview && !crate::player::preview::accepts_direct_play(directplay, !part.is_empty(), adaptive) {
        crate::player::log("preview: refused — not a direct play");
        plan.url.clear();
        return plan;
    }
    if (directplay || rk.is_empty()) && !part.is_empty() {
        // direct-play: the pipeline decodes the SOURCE codecs natively, so the Load payload uses
        // them (h264/hevc + the chosen audio track's codec). If a specific track was picked
        // (aidx >= 0), tell the demuxer to feed that stream — by CONTAINER ordinal, not the
        // list position (audio_ordinal sorts on PMS Stream.index).
        let (aidx, achosen, asid) = audio_sel.unwrap_or((-1, acodec.to_string(), 0));
        // source fps for the Load esInfo — from the playing item's own store (present for the
        // straight-from-Home path too, which never ran load_detail)
        let fps = plan.playing.as_ref().map(|p| p.video_fps).unwrap_or(0.0);
        plan.vcodec = vcodec.to_string();
        plan.acodec = achosen.clone();
        plan.fps = fps;
        // Only here: this is the branch that feeds the FILE's own elementary stream, so it is the
        // only one whose Load payload may describe the file's Dolby Vision.
        plan.dovi = dovi;
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
        // record the picked track's stream id so the timeline reports what actually plays
        // (0 = default/unknown → the param is omitted, the server shows the part default)
        plan.audio_sid = asid;
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
        // honour a subtitle the server already has selected for this part (chosen on another
        // client, or by this app in an earlier session) — free here, since the direct-play path
        // renders subtitles itself. apply_plan installs it on the main thread.
        let sub_sel = plan
            .playing
            .as_ref()
            .and_then(|p| pick_dp_subtitle(&p.subs));
        if let Some((ssid, ord)) = sub_sel {
            plan.sub_sid = ssid;
            plan.sub_render_ordinal = Some(ord);
        }
        // direct-play: no transcode session (transcode_session() stays empty). Carry the
        // session id + identity on the file GET so PMS keys the /status/sessions entry by
        // SESS (not a token= fallback), keeping the timeline correlation consistent.
        plan.url = client.direct_play_url(part, &session).to_url();
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
    let remux = video_dp && allowed.remux && !no_video_copy && !mde_forbids_copy;
    // Remux copies, so this PUT names the smart-DP sibling. A re-encode transcodes a real
    // selected source track (English DTS → AC3) and must not PUT that sibling or a 720p start
    // replaces the pick with a foreign AC3 copy. A selected flag that only echoes default is
    // not a pick; `encode_audio_id` then keeps an English sibling or, if the sibling is a
    // foreign dub, the first English track (unselected DTS included).
    let encode_audio = encode_audio_id(remux, audio_id, env.audio_sid, tracks);
    if remux {
        let achosen = audio_sel
            .as_ref()
            .map(|(_, c, _)| c.clone())
            .unwrap_or_else(|| acodec.to_string());
        plan.vcodec = vcodec.to_string();
        plan.acodec = achosen;
    } else if matches!(
        plan.delivery,
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
    if encode_audio > 0 {
        plan.audio_sid = encode_audio;
    }
    // keep the flavor so a later seek rebuilds the same query for start.mkv?...&offset=T
    // Both halves of this line landed in the same batch from different units and each is
    // load-bearing: `remux` (not `video_dp`) is the relay gate — a copy of a 31 Mbit/s stream
    // down a 2 Mbit/s tunnel cannot play, so `link.remux` demotes it to a real re-encode — and
    // `env.sid` routes the selection to the server the ITEM came from. Dropping either compiles
    // and passes: without the gate a relay stalls, without the sid a friend's audio pick is PUT
    // to our own server, which answers 200 and changes nothing on theirs.
    plan.remux = remux;
    plan.no_video_copy = no_video_copy;
    // `plan.ceiling` is NOT set here — it was set for every flavour up at the decision, which is
    // what the direct-play branch needed too. Spending it below is the third reader of the same
    // reasoning `remux` and `no_video_copy` carry: a seek and an audio switch rebuild this query
    // from `Session`, and one that dropped the ceiling would hand the encoder back the full
    // 4K/60 Mbps bound the moment the user touched the scrubber.
    // Remux: the smart-DP sibling MDE and the remux probe already named — `env.audio_sid` is
    // the part default (TrueHD) at resolve start; putting that undoes smart-DP. Re-encode:
    // `encode_audio_id` (a real selected pick, else English, else that sibling). Subtitle stays
    // `env.sub_sid`: a positive id here is a burn, and Original client-renders instead.
    put_selection(env.sid, plan.part_id, encode_audio, env.sub_sid);
    if remux_probed && adaptive {
        // Probe registered start.mkv on this playback identity. HLS `/decision` reuses it;
        // closeResourceSession=1 would 503 the next start. A failed sample already stopped
        // inside measure_remote_remux; this covers a completed sample that still falls to HLS.
        // Stay-remux Original does not stop: the play-path decision owns that session.
        let _ = client.transcode_stop_physical(&session);
    }
    let sp = transcode_spec(
        rk,
        &session,
        &session,
        remux,
        no_video_copy,
        crate::plex::TranscodeOffset::Fresh,
        encode_audio,
        env.sub_sid,
        plan.ceiling,
        plan.delivery,
    );
    if let Some(mc) = client.transcode_decision(&sp) {
        // The server has already answered, and it is allowed to answer NO. Stop here rather than
        // stream a `start.mkv` it has just said it cannot produce: the plan leaves with no URL —
        // the ordinary "this did not resolve" failure — and carries the verdict so the read-out can
        // quote the server's own sentence instead of the generic "Playback failed" this used to be.
        if let Some(v) = refusal(&mc) {
            crate::player::log(&format!(
                "decision: REFUSED general={:?} transcode={:?} — {v}",
                mc.general_decision_code, mc.transcode_decision_code
            ));
            plan.verdict = Some(v);
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


/// Preferred audio language (ISO-639 code). Content is often authored with a foreign default
/// dub (e.g. The Office ships a Russian "kubik" track flagged default); we prefer the English
/// track when the item has one, rather than following the file's default flag.
pub(super) const PREF_AUDIO_LANG: &str = "eng";


/// Pick the audio track to DIRECT-PLAY from the playing item's track store
/// (metadata::playing(), loaded by build_stream), returning (list_idx, codec, stream_id):
/// list_idx -1 = codec-default (demuxer matches by payload codec — only when the track list is
/// unavailable), else the index into `playing().audio`, with that track's Plex stream id so the
/// timeline can report the truth. Order of preference:
///   1. the stream the SERVER already has selected for this part (PMS `Stream.selected`), when
///      that selection is a real CHOICE and direct-playable — a track picked on another Plex
///      client (phone, web, another TV) or here in an earlier session outranks our own defaults,
///      which used to silently overwrite it on every play;
///   2. a direct-playable track in PREF_AUDIO_LANG (English), so English shows don't open in a
///      foreign default dub — the Load payload uses THAT track's codec so there is no mismatch;
///   3. the file's flagged default track, if its codec is direct-playable — by EXPLICIT index
///      (matching by codec alone fed the first same-codec stream, not the flagged default, when
///      another track of that codec preceded it);
///   4. any other direct-playable track (TrueHD/DTS-default item with an AC3 sibling — smart-DP).
/// None when NO audio track is direct-playable (→ transcode).
///
/// Rung 1 carries TWO gates, and both are load-bearing, because PMS reports a selected AUDIO
/// stream on essentially every part — there is no "nothing selected" state for audio (verified
/// against the live server: parts this client has never PUT a selection for still come back with
/// the file's default flagged `selected`).
///   - **It must differ from the file's `default` flag.** A selection that merely echoes the
///     container default is not evidence that anyone chose anything, and honouring it verbatim
///     would delete the English rung below — whose whole reason to exist is that a foreign dub is
///     often the file default (The Morning Show reports its Russian default as `selected`). When
///     the server's pick is a DIFFERENT stream, something actually chose it: a user on another
///     client, or this app's own `put_selection` in an earlier session. The cost of the gate is
///     that a choice which LANDS on the default is indistinguishable from no choice at all and
///     falls through to the ladder — that covers both an account-language preference matching the
///     default and a user here picking the default-flagged track by hand, so neither round-trips.
///     Fixing it needs state the part does not carry: the account's own defaultAudioLanguage, or
///     a remembered per-item pick. Both are separate gaps; neither is guessable from this flag.
///   - **It must be direct-playable.** Otherwise we fall through instead of forcing a transcode to
///     obey it, which would drop the whole smart-direct-play class (a TrueHD/DTS pick with an AC3
///     sibling) onto the server's video-downscaling encoder for one audio track.
/// PURE: takes the playing item's audio tracks explicitly instead of reaching into
/// `metadata::playing()`. That matters twice over. (a) `playing()` hands out a `&'static
/// PlayingItem` whose `Vec`s `ui/track_menu.rs` and `ui/info_panel.rs` hold slices into during
/// playback — a worker replacing the store would drop those out from under the draw path, so the
/// resolve must never touch it. (b) Being pure makes the selection ladder host-testable, which it
/// has never been; see the tests at the foot of this file.
pub(super) fn pick_dp_audio(
    tracks: &[crate::metadata::Stream],
    default_acodec: &str,
) -> Option<(i32, String, i64)> {
    let dp = crate::plex::is_dp_audio;
    if tracks.is_empty() {
        // no track info — fall back to the codec-default (or transcode if that isn't DP)
        return if dp(default_acodec) {
            Some((-1, default_acodec.to_string(), 0))
        } else {
            None
        };
    }
    let pick = |i: usize| (i as i32, tracks[i].codec.to_lowercase(), tracks[i].id);
    // 1. the server's own current selection, when it is a real pick (differs from the file's
    //    default flag — see the doc) and direct-playable: honours a choice made elsewhere
    if let Some(i) = tracks
        .iter()
        .position(|s| s.selected && !s.default && dp(&s.codec.to_lowercase()))
    {
        return Some(pick(i));
    }
    // 2. preferred-language, direct-playable
    if let Some(i) = tracks
        .iter()
        .position(|s| dp(&s.codec.to_lowercase()) && s.lang_code == PREF_AUDIO_LANG)
    {
        return Some(pick(i));
    }
    // 3. the file's flagged default track, if direct-playable (explicit index)
    if let Some(i) = tracks
        .iter()
        .position(|s| s.default && dp(&s.codec.to_lowercase()))
    {
        return Some(pick(i));
    }
    if dp(default_acodec) && !tracks.iter().any(|s| s.default) {
        // Media[0].audioCodec is DP but no stream carries the default flag — codec-match
        return Some((-1, default_acodec.to_string(), 0));
    }
    // 4. any direct-playable track (smart direct-play over a non-DP default)
    tracks
        .iter()
        .position(|s| dp(&s.codec.to_lowercase()))
        .map(pick)
}


/// Stream id named on the remux/re-encode PUT and start.mkv.
///
/// A remux COPIES, so this is the smart-DP sibling (`dp_audio_id`) — putting a selected
/// TrueHD/DTS track would ship audio the TV cannot decode. A re-encode can transcode a real
/// selected pick (`selected && !default`) to AC3, so naming the sibling would replace English
/// DTS with a foreign AC3 copy. A `selected` flag that only echoes `default` is not a choice
/// (The Morning Show: the Russian default reads `selected`); that falls through rather than
/// beating English.
///
/// After that pick, a sibling already in [`PREF_AUDIO_LANG`] stays — taking "first English, any
/// codec" would PUT English TrueHD on 720p when an English AC3 sibling exists, undoing smart-DP.
/// Only when the sibling is a foreign dub does the first English track win, so an unselected
/// English DTS is encoded instead of a Russian AC3 copy. Never `0` (an omitted PUT encodes the
/// part default).
/// `env_audio_sid` is the session/retry pick and wins on re-encode when set, including a remux
/// leftover sibling (mid-play quality drop keeps what is already playing). A cold play zeros
/// it (`request_play`).
fn encode_audio_id(
    remux: bool,
    dp_audio_id: i64,
    env_audio_sid: i64,
    tracks: &[crate::metadata::Stream],
) -> i64 {
    if remux {
        return dp_audio_id;
    }
    if env_audio_sid > 0 {
        return env_audio_sid;
    }
    if let Some(id) = tracks
        .iter()
        .find(|s| s.selected && !s.default)
        .map(|s| s.id)
        .filter(|&id| id > 0)
    {
        return id;
    }
    let sibling_is_pref = tracks
        .iter()
        .any(|s| s.id == dp_audio_id && s.lang_code == PREF_AUDIO_LANG);
    if sibling_is_pref {
        return dp_audio_id;
    }
    tracks
        .iter()
        .find(|s| s.lang_code == PREF_AUDIO_LANG)
        .map(|s| s.id)
        .filter(|&id| id > 0)
        .unwrap_or(dp_audio_id)
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
/// Two deliberate limits, both about what the client renderer can actually deliver:
///   - an EXTERNAL (sidecar) selection returns None. It is not in the container, so nothing would
///     render; only a server burn can show it, and silently forcing a transcode to obey a stored
///     flag is not a trade the user asked for.
///   - this is the direct-play path only. The transcode path keeps PUTting `subtitleStreamID=0`
///     (subs off) as before: honouring a selection there means a server-side BURN, i.e. a
///     re-encode carrying a picture-quality cost, which is a trade to put behind the settings
///     surface this app does not have yet rather than to make silently at every play. Once a
///     direct-played item DOES go to the transcoder mid-session (a DTS/TrueHD audio pick), the
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
    if ord < 0 || subs[i].id <= 0 {
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
fn mde_subtitle_stream_id(subs: &[crate::metadata::Stream]) -> i64 {
    pick_dp_subtitle(subs)
        .and_then(|(id, _)| {
            subs.iter()
                .find(|s| s.id == id)
                .filter(|s| crate::plex::is_dp_subtitle(&s.codec))
                .map(|_| id)
        })
        .unwrap_or(0)
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
    /// `build_stream` spells this exact case `plan.remux = video_dp` on the transcode branch.
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
        .any(|a| crate::plex::is_dp_audio(&a.codec));
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
