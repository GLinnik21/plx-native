//! The IMPURE half of the route split (spec §9, §15.2): session state, the synchronized
//! [`PlayerControl`], PMS/native I/O, and the encoder/scrobble/timeline machinery — everything
//! [`super::plan`] is not. `PlaybackSession` is the main-thread projection used to build URLs/payloads;
//! [`PLAYER_CONTROL`] is the synchronized authority for route ownership and route-changing
//! intents. The player engine reads the URL/session through the accessors here; appkit::player_hud
//! reads the HUD strings through title_cptr()/ctxline_cptr(). This file is exempt from the
//! `wall` gate that `plan.rs` must pass — a network/adapter effect is allowed to read wall time —
//! but as of this split it still contains none: the one wall-clock field this module owned
//! (`PlaybackSession::auto_last_switch`) is now a frame-tick millisecond stamp, not an `Instant`.
//!
//! The claim worker's plumbing (the work/fallback enums, the landing mailbox, the spawn, the drain
//! and the stale-landing discard rules) is [`super::flight`]; the route DECISIONS that feed it
//! (the claim table, the enhancement step, the plans and the PMS attempts) stay here.

use plx_plex::plex::ServerId;
use plx_data::pms::PmsMovie;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::Mutex;
use super::flight::*;
use super::plan::*;

// ---- ONE playback session, as ONE value -----------------------------------------------------

/// Everything needed to resolve the item again after a terminal playback failure.
///
/// A failed `/decision` has no Engine and an HTTP-open failure has an Engine whose URL is already
/// terminal, so neither can be recovered by poking the live route.  The user-facing quality
/// picker starts a NEW resolve instead.  Keep the original request here, before the worker runs:
/// even a plan that returns no URL must remain retryable, and a numeric Part id alone cannot
/// reconstruct the source key PMS expects.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PlaybackRequest {
    sid: ServerId,
    rk: String,
    part: String,
    vcodec: String,
    acodec: String,
    title: String,
    ctx: String,
    /// Background hero preview. No PlayQueue, no timeline, no scrobble, resume at 0, and the
    /// caller must not push the player route.
    preview: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RetryContext {
    resume_ns: i64,
    audio_sid: i64,
    sub_sid: i64,
    sub_offset_ms: i64,
    direct_play_mode: DirectPlayMode,
    /// Issue #266: resolve this retry WITHOUT the audio enhancement. Set only by a start-failure
    /// rescue ([`retry_current_play`]) of a playback that was enhanced: an enhanced `start.mkv`
    /// that failed to open is the one failure a plain retry would repeat verbatim.
    suppress_enhancement: bool,
}

/// Everything the main thread needs to resolve and render the playback in progress, in one struct.
///
/// **Owned by [`crate::player::machine::Player`], a field of `App`, and reached ONLY as a
/// parameter** (restructure spec §2.2, phase 9). Every field below was its own `static mut`; phase
/// 9 collected them into `static mut SESSION` and then deleted that too, so the value now has one
/// owner and the borrow checker — rather than a `MAIN THREAD ONLY` comment — is what says a second
/// writer cannot exist. The SHAPE was the original hazard rather than any one field:
/// [`apply_plan`] installed all of them but the two HUD buffers, [`request_play`] owned those
/// two and the five the outgoing item leaves behind, and a dozen small functions each poked one or
/// two more on the side — so "what a session IS" was written down nowhere and no writer could be
/// read against the whole. The failure that shape produced is still documented at the line that
/// fixed it, in [`build_stream`] — the part id was published by the CALLER after the resolve
/// returned, so `put_selection`, which runs INSIDE the resolve, addressed the PREVIOUS item's part,
/// and the server-default subtitle that PUT exists to suppress was burned into the transcode
/// instead.
///
/// Route ownership and worker/main route transitions are deliberately not fields here: the
/// synchronized [`PLAYER_CONTROL`] is their authority. [`ResolveEnv`] exists for the other
/// direction: the resolve worker is handed owned copies and reads none of this. The accessors that
/// lend rather than copy — [`play_verdict`], [`up_next`] and [`with_queue`], plus the raw pointers
/// [`title_cptr`]/[`ctxline_cptr`] hand to `draw_text` — now borrow from the caller's `&`, so the
/// "valid until the next main-thread write" caveat those docs carried is enforced rather than
/// asserted: a frame's draw cannot hold one across [`apply_plan`] or [`request_play`], because
/// those take `&mut`.

pub struct PlaybackSession {
    /// Frozen for one logical playback, including its retries and track changes.
    direct_play_mode: DirectPlayMode,
    /// This playback was refused by the cached device sandbox preflight. No Engine exists.
    /// Cleared with the playback verdict on exit/reset, never a process-global error latch.
    pub jail_load_blocked: bool,
    /// Read-only publication of Player.repair for the HUD. Never authorizes a resource effect.
    pub repair_status: plx_platform::tv::sandbox::State,
    /// The request which produced this attempt, retained for terminal Retry / Choose quality.
    /// Written synchronously by [`request_play`] rather than by [`apply_plan`], because the
    /// server can refuse before a playable plan exists.
    request: Option<PlaybackRequest>,
    /// Resume target which has not yet been proven by a presented frame.  It survives a refused
    /// retry plan so the next quality choice does not restart a two-hour film at zero; cleared by
    /// [`confirm_resume_presented`] once the replacement actually shows a frame.
    requested_resume_ns: i64,
    /// The stream URL for this playback (empty = nothing resolved, or torn down).
    url: String,
    /// The server-side transcode session id, EMPTY on a direct play — that emptiness is the
    /// "is this a transcode?" test ([`is_transcoding`]) and the key the stop is sent with.
    tsession: String,
    /// The server's PRE-FLIGHT refusal for the last resolve, or None.
    ///
    /// `Some(sentence)` means `/decision` answered "neither direct play nor conversion is
    /// available" (`generalDecisionCode` 2000) BEFORE a byte of video moved, so this playback never
    /// got a URL — see [`refusal`]. The String is the server's OWN sentence, carried so the
    /// player's read-out can quote it verbatim; it is `""` when the server named a code but no
    /// reason, which is why the refusal itself lives in the `Option` and not in the emptiness of
    /// the text.
    ///
    /// [`apply_plan`] installs it and [`request_play`] retires it, so it always describes the item
    /// the player is showing.
    play_verdict: Option<PlayVerdict>,
    /// The resolve itself could not produce a plan (no client, worker panic, or worker spawn
    /// refusal).  Unlike `play_verdict`, this is OUR failure rather than a PMS sentence; it makes
    /// an empty plan terminal and retryable instead of falling back to an idle black frame.
    resolve_failed: bool,
    /// This playback's encode shape — flavor, delivery, the no-copy switch, the fixed quality
    /// ceiling and (issue #266) the Plex Pass audio DSP. A seek or retranscode rebuilds the
    /// identical start.mkv query from (`cur_rk`, `sess`, the `cur_*_sid` pair, this contract) via
    /// `plex::TranscodeSpec` — replaces the old stored offset-free TBASE query string.
    ///
    /// Was four separate fields (`cur_remux`, `cur_delivery`, `cur_no_video_copy`, `cur_ceiling`),
    /// each carried for the same reason: a seek ([`plan_rebase`]) and an audio switch
    /// ([`retranscode`]) rebuild the start.mkv query from scratch, and a rebuild that read the
    /// LIVE selection instead of this stored shape would change the encode's resolution mid-film,
    /// hand the server back a copy permission mid-playback, or drop the enhancement DSP on the
    /// next seek. Folding the four into one `EncodeContract` is what
    /// keeps that coupling a single write instead of four fields that could drift independently.
    ///
    /// **[`set_quality`] is the ONE writer that may move `ceiling` mid-film**, and that is the
    /// whole distinction: an explicit pick is new information about what the link can carry, while
    /// a seek is not, so a seek rebuilds from what is stored here and a pick replaces it (and asks
    /// the pump for a fresh transcode when the answer actually changed). Nothing measures a link
    /// or moves a rung on its own — the adaptive switch is not here.
    cur_contract: plx_plex::plex::EncodeContract,
    /// What the last successful transcode decision actually did with the Plex Pass audio DSP
    /// (issue #266) — distinct from `cur_contract.audio`, which is what the accepted decision
    /// carried. Installed by `apply_plan` from `Plan::enhancement`; reset to `Off` by a new
    /// request. See `route::decision::EnhancementOutcome`.
    cur_enhancement: EnhancementOutcome,
    /// What the resolve measured the playing source at — `(kbps, w, h)`, `0` where nobody said.
    /// The input [`set_quality`] re-runs [`quality_policy`] on when a rung is picked mid-film, so
    /// that decision is made from the same numbers `build_stream` used rather than from a guess.
    cur_src: (i64, i64, i64),
    /// Whole Part transport bitrate, including audio. Auto's progressive watchdog compares the
    /// live socket against this rather than `cur_src.0` (video only), because the wire has to carry
    /// both lanes. `0` means PMS did not provide one and disables the watchdog fail-safely.
    cur_transport_kbps: i64,
    /// **Can this television decode the SOURCE video stream as it stands** — `video_direct_plays`
    /// evaluated by [`build_stream`], carried rather than re-derived.
    ///
    /// It answers the one question the word "Original" is a claim about: false means the server
    /// MUST re-encode the pixels, whatever rung is picked and whatever the link does, so the
    /// quality menu's Original row cannot deliver the original. AV1, VP9 and MPEG-2 are the cases;
    /// see the `!video_dp` arm's own comment.
    ///
    /// **Carried, because re-deriving it at draw time would be a second copy of the gate.** The
    /// menu is drawn from a different thread of control and a different set of facts than the
    /// resolve — `metadata::playing()` is `None` for the whole 0.5-3 s resolve window — and a
    /// second evaluation could disagree with the routing decision it is describing. This is the
    /// same argument [`PlaybackSession::cur_remux`] carries for the neighbouring question, and the reason
    /// `playback_preview_of` exists rather than a duplicate of the gate on the detail page.
    ///
    /// **`true` when nothing has resolved yet**, so an absent fact annotates nothing. A menu is
    /// only reachable inside a live player session, so the window is small; and the failure
    /// direction matters — claiming "the source cannot be preserved" about a source nobody has
    /// looked at is a worse read-out than saying nothing.
    cur_source_decodable: bool,
    /// **Auto chose Original for this playback, so the progressive transfer watchdog runs.**
    /// The only reader is [`auto_original_watch`], so this field's whole meaning is that question.
    ///
    /// It used to be `cur_auto_remote_original` and to carry `Location::Remote`, on the argument
    /// that a Local link "needs no throughput proof". That is true of the PRE-FLIGHT question —
    /// whether to spend a probe before choosing Original — and false of the runtime one:
    /// `Location` is decided from the address shape (`plex::probe::configured_tier`) and describes
    /// TOPOLOGY, which does not imply throughput. Wi-Fi, powerline, a busy switch or a second
    /// stream in the house all produce a LAN that cannot carry a 10 Mbps source.
    ///
    /// Measured 2026-08-27 (`docs/measurements/local-original-blind.md`): a 10 634 kbps
    /// direct-play source on the local PMS with the link held at 2 500 kbps ran at **8–25 % of
    /// real time for the rest of the playback**, with ZERO `abr:` lines in the whole log. The two
    /// `recover_auto_to_original` writers never carried the conjunct, so an Original REACHED from
    /// HLS was supervised on any link while the same state chosen at play time on a LAN was not —
    /// which is what makes it an oversight rather than a design.
    cur_auto_original_watched: bool,
    /// The zero-encode flavor Auto may return to after a remote link recovers. Kept even while
    /// HLS is active: the HLS worker owns only measurements, while the main thread owns the
    /// codec/session transition back to this exact source declaration.
    auto_original: Option<AutoOriginalCandidate>,
    /// Debug pipeline-tier substitute for PMS's fixed-rendition endpoints. Empty in every
    /// production plan; see [`arm_auto_fixture`].
    auto_fixture_base: String,
    /// **Visible mode switches this playback has already shown the viewer**, and when the last one
    /// was. Lives here, on the main thread, because it has to OUTLIVE the demux workers: each
    /// Original↔HLS transition replaces the engine, so a counter held by a worker would reset to
    /// zero on exactly the event it exists to count, and flapping would be invisible to the very
    /// controller meant to prevent it. Captured into each worker at spawn
    /// ([`crate::abr::TransitionHistory`]) and advanced there by the worker's own elapsed time.
    auto_switches: u32,
    /// Milliseconds, in the frame tick's units ([`PlaybackSession::now_ms`], a frame-tick-shaped
    /// stamp rather than an `Instant`; see [`note_visible_switch`]/[`auto_history`]). `None`
    /// before the first switch.
    auto_last_switch: Option<u32>,
    /// The startup probe's measurement, kept so a mode transition can hand the next worker a
    /// starting estimate instead of an empty one. Explicitly a weak prior, never a measurement of
    /// the request it is handed to — see [`crate::abr::CapacityEstimate::demote_to_prior`].
    auto_prior_kbps: u32,
    /// The HLS contingency [`crate::abr::bootstrap`] selected while it still owned the evidence.
    /// Usually installed immediately; retained while Auto tries Original so an HTTP-open refusal
    /// can take the same branch without calling the refusal a zero-rate sample or mistaking source
    /// demand for link capacity.
    auto_bootstrap_rung: Option<crate::abr::Rung>,
    /// ratingKey of the currently-playing item (movie or episode), so an audio-track
    /// switch can force a fresh transcode of the same item.
    cur_rk: String,
    /// The SERVER the currently-playing item came from — the other half of its identity.
    ///
    /// A ratingKey names an item only within one server: `1` is a real item on our own server and a
    /// different real item on a friend's share, and the same goes for `Part.id`, `Stream.id`,
    /// `playQueueID` and the resume point. Every PMS call in this file used to resolve its server
    /// implicitly, at the instant of the call, through `client_opt()` — i.e. whichever server
    /// happened to be CURRENT right then. Merged Home shelves make "the item is from B while
    /// current is A" ordinary, and every one of those calls would then land on A: the PlayQueue,
    /// the track PUT, the transcode stop, and — ten seconds at a time, forever — the progress
    /// report that writes the resume point.
    ///
    /// So the server is captured ONCE, at [`request_play`], and carried by value from there:
    /// `ResolveEnv` → `Plan` → here. Nothing below this line re-resolves it. `UNSET` before the
    /// first play and on a plan that never resolved, which resolves to no client at all rather than
    /// to slot 0.
    cur_sid: ServerId,
    /// current audio track carried by any TRANSCODE of the current item, and — on the Original
    /// family — the direct-played/remuxed track's own facts (codec, channels, loudness capability,
    /// immersive), when known. `None` means "server default, facts unknown" and every reader fails
    /// CLOSED on it (issue #266's `CarriedAudio` — `route::plan`). `cur_audio_sid()` below is the
    /// old accessor kept as a projection: `.map_or(0, |a| a.sid)`, so every existing caller that
    /// only ever wanted the wire id keeps compiling unchanged.
    cur_audio: Option<CarriedAudio>,
    /// current subtitle selection (0 = none) — the picked Plex stream id, regardless of how it
    /// renders: burned into any TRANSCODE (our client profile advertises no soft-sub support, so
    /// Plex's decision is burn), or client-rendered from the demuxer on direct play
    /// (`player::request_subtitle`).
    cur_sub_sid: i64,
    /// Is [`Self::cur_sub_sid`] an external sidecar (`metadata::Stream::sidecar_renderable`) rather
    /// than an embedded (in-container) track? Meaningless when `cur_sub_sid == 0`. Issue #266 I6:
    /// this is what tells `route::decision::facts`'s [`super::plan::SubtitleEffect::Sidecar`]
    /// (unaffected by the audio enhancement's remux) apart from
    /// [`super::plan::SubtitleEffect::Embedded`] (needs a forced burn to survive one, M7).
    cur_sub_sidecar: bool,
    /// Can the app itself draw [`Self::cur_sub_sid`] (an embedded ordinal the demuxer exposes, or a
    /// sidecar)? Set from `client_renderable` when the pick is committed and from the plan's
    /// `sub_render_ordinal` at a cold start. With the route shape it decides whether a remux may
    /// leave the subtitle to the app instead of having the server burn it ([`side_subs_allowed`]).
    /// Meaningless when `cur_sub_sid == 0`.
    cur_sub_client_drawable: bool,
    /// The app's own drawing of a subtitle over a remux was refused or failed on this playback,
    /// so the server burn is the only route left for it. Session-scoped: cleared with the item.
    side_subs_refused: bool,
    /// The subtitle-language preference this play resolved under — the show's pref if it set one,
    /// else the account's — carried straight from [`super::plan::Plan::sub_pref_lang`] so the
    /// Subtitles menu's "yours" grouping (`metadata::sub_layout::sub_sections`) survives a reload.
    cur_sub_pref_lang: Option<String>,
    /// the playing item's Part id (from the part key), so an audio switch can PUT the
    /// server-side stream selection — the transcoder encodes the part's SELECTED audio.
    cur_part_id: i64,
    /// Opaque internal playback generation, regenerated on each play_movie/play_episode. It is
    /// also the first encoder's PMS session id. Adaptive replacements keep this app generation
    /// stable but use their own coupled PMS wire id; the active encoder mutex supplies timeline,
    /// seek and teardown with the currently published wire identity.
    sess: String,
    /// GET /identity machineIdentifier, cached — needed for the PlayQueue uri.
    ///
    /// It is cached PER SERVER, which is what `machine_sid` is for: the id goes into
    /// `uri=server://{machineIdentifier}/…` on the PlayQueue POST, so one cached globally is a
    /// mis-addressed queue the moment a second server exists — server A's fingerprint POSTed to B,
    /// naming a server B has never heard of. Only a cache learned from THIS playback's server is
    /// usable, and `resolve_playqueue` prefers the registry's own per-server id over both.
    ///
    /// It is also one of the two CONDITIONAL writes in [`apply_plan`] (the codec quartet below is
    /// the other): the pair is left alone on a plan that fetched no id (`machine_id == ""`), so a
    /// cache learned by an earlier playback survives into the next one and spares it a `/identity`
    /// round trip.
    machine_id: String,
    machine_sid: ServerId,
    /// This playback's PlayQueue ids for the timeline (empty if /playQueues failed).
    pq_id: String,
    pq_item_id: String,
    /// The item's OWN codecs, as the file has them — captured once per playback and never
    /// overwritten by `apply_decision_codecs`, which replaces `stream_*` with the transcode OUTPUT.
    ///
    /// Two different questions, and the diagnostics read-out needs both: "what is this file" and
    /// "what is the server actually sending". With only the second recorded, a transcode reported
    /// its output as though it were the source and the whole server-side transform was invisible.
    src_vcodec: String,
    src_acodec: String,
    /// The streamed item's Media video/audio codec (h264/hevc, ac3/eac3/aac), so the player picks
    /// the H265 Load payload for a native HEVC direct-play and the matching audio codec.
    stream_vcodec: String,
    stream_acodec: String,
    /// Direct-play source video frame rate (0 = unknown/transcode → omit from the Load esInfo).
    stream_fps: f64,
    /// The direct-played file's raw Dolby Vision layering, retained for diagnostics. The Load
    /// payload consumes `stream_dv_decision` below: re-evaluating this record after a late probe
    /// would make a reload describe a different route from the one which passed the gate.
    ///
    /// `Dovi::NONE` on every transcode and remux: what arrives then is the server's output, and
    /// the only DV file that reaches those paths is one we refused to declare in the first place.
    stream_dovi: plx_data::metadata::Dovi,
    /// Capability and presentation frozen when `stream_dovi` entered this physical route. An
    /// audio switch, reload, Original recovery and rollback all copy this beside the raw record.
    stream_dv_decision: plx_data::metadata::DvDecision,
    /// **Does the audio elementary stream we are feeding carry Dolby Atmos?** The Load payload's
    /// `contents.immersive` node turns on it ([`crate::player::engine`]).
    ///
    /// Rides the session for exactly the reason `stream_dovi` does, and the failure it prevents is
    /// the audible twin of that one: an audio-track switch tears the engine down and rebuilds the
    /// payload from here, so a value that lived only in the plan would silently stop declaring
    /// Atmos the moment the user opened the track menu — on the very track they had just chosen
    /// *because* it is the Atmos one.
    ///
    /// `false` on every transcode and remux; see where it is set for why that is deliberate rather
    /// than an omission.
    stream_immersive: bool,
    /// HUD strings as fixed NUL-terminated C buffers, so title_cptr()/ctxline_cptr() hand
    /// draw_text (extern "C", *const c_char) a pointer that stays valid for the whole frame.
    ///
    /// The pair a landing does NOT install: [`request_play`] writes them synchronously, at the
    /// press, so the HUD has a title for the whole resolve — which is why [`apply_plan`]'s single
    /// assignment carries them across rather than overwriting them.
    title: [c_char; 128],
    ctxline: [c_char; 96],
    /// The next episode of the item now playing, or None (a movie, the last episode, or a queue
    /// that failed). Installed by [`apply_plan`]; [`request_play`] retires it the moment a new item
    /// resolves. Read through [`up_next`], which lends a `&'static`.
    up_next: Option<UpNext>,
    /// The whole queue behind the item now playing — the playing row included, in queue order,
    /// projected to `plex::QueueRow` ON THE RESOLVE WORKER (a `Metadata` row carries its entire
    /// Media/Part/Stream/Role tree; a show's queue is dozens of them, and this device is 32-bit).
    /// Same lifecycle as `up_next`: installed by `apply_plan`, retired by `request_play`.
    ///
    /// Whatever the server sent is kept, uncapped — a projected row is ~300 bytes on this device,
    /// so even a whole show is tens of KB. The capping that matters is the DRAWING (a still per row
    /// is a GL texture); that belongs to the overlay, and `apply_plan` deliberately warms only
    /// `up_next`'s.
    queue: Vec<plx_plex::plex::QueueRow>,
    /// **This frame's millisecond stamp, mirrored from [`crate::player::machine::Player::now_ms`]**
    /// (spec §4.1), whose `set_now` is its ONLY writer — the loop calls it once per iteration from
    /// the same `fr.now` every other phase of the frame reads.
    ///
    /// It lives here rather than travelling as a parameter because the session travels alone
    /// through the whole route layer and the tick has to travel with it: the two readers below
    /// ([`note_visible_switch`] and [`auto_history`]) sit five and seven calls deep inside
    /// `start_bufferfeed` and `pump`, and a `now_ms` argument threaded to exactly those two would
    /// have to be carried by a dozen functions that have no other use for it — which is how the
    /// second clock read this replaces got there in the first place.
    now_ms: u32,
    /// This session is a hero preview. Skips watch-state writes and must not be repaired onto
    /// the player route.
    preview: bool,
    /// This session was RESOLVED for a hero preview. Installed with `preview` by [`apply_plan`]
    /// and, unlike it, never cleared by the preview machinery: `preview` tracks whether the
    /// machine still owns the engine, this says what the session may write. See
    /// [`preview_request`].
    resolved_as_preview: bool,
}

/// What the server actually did with a requested Plex Pass audio enhancement (issue #266) — as
/// opposed to `EncodeContract::audio`, which is what was ASKED for. Distinguishing "on and
/// working" from "the server ignored it" matters because PMS 1.43.4 does both: measurement M2
/// (`/tmp/plx266/measurements.md`) found an AC3 2.0 source where the params changed the decision
/// (a real DSP transcode) beside an AAC 5.1 source where the audio was transcoded to AC3 either
/// way, params or not — so "the decision shows a transcode" alone cannot tell the two apart.
///
/// Graded by `build_stream` (`classify_outcome`, or `Refused` from its fallback) and installed by
/// `apply_plan` beside `cur_contract` — written only after a decision, never at the selection (I9).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) enum EnhancementOutcome {
    #[default]
    Off,
    /// Remux, the audio decision was a transcode, and the source codec is in the profile's own
    /// copy list — the params demonstrably did something.
    Applied,
    /// The audio would have been transcoded anyway (PMS M2's AAC 5.1 case) — asked for, delivered,
    /// but not provably BECAUSE of the ask.
    Unverified,
    /// The server refused the params outright, or silently ignored them (audio came back `copy`
    /// despite the ask) — an old or non-conforming PMS.
    Refused,
}

impl PlaybackSession {
    /// Nothing playing: what the module holds before the first play, and the value the static is
    /// born as. Every String empty, every id 0 or `UNSET`, both HUD buffers NUL.
    pub const IDLE: PlaybackSession = PlaybackSession {
        direct_play_mode: DirectPlayMode::Auto,
        jail_load_blocked: false,
        repair_status: plx_platform::tv::sandbox::State::Idle,
        request: None,
        requested_resume_ns: 0,
        url: String::new(),
        tsession: String::new(),
        play_verdict: None,
        resolve_failed: false,
        cur_contract: plx_plex::plex::EncodeContract::original(false, plx_plex::plex::AudioEnhancements::NONE),
        cur_enhancement: EnhancementOutcome::Off,
        cur_src: (0, 0, 0),
        cur_transport_kbps: 0,
        cur_source_decodable: true,
        cur_auto_original_watched: false,
        auto_original: None,
        auto_fixture_base: String::new(),
        auto_switches: 0,
        auto_last_switch: None,
        auto_prior_kbps: 0,
        auto_bootstrap_rung: None,
        cur_rk: String::new(),
        cur_sid: ServerId::UNSET,
        cur_audio: None,
        cur_sub_sid: 0,
        cur_sub_sidecar: false,
        cur_sub_client_drawable: false,
        side_subs_refused: false,
        cur_sub_pref_lang: None,
        cur_part_id: 0,
        sess: String::new(),
        machine_id: String::new(),
        machine_sid: ServerId::UNSET,
        pq_id: String::new(),
        pq_item_id: String::new(),
        src_vcodec: String::new(),
        src_acodec: String::new(),
        stream_vcodec: String::new(),
        stream_acodec: String::new(),
        stream_fps: 0.0,
        stream_dovi: plx_data::metadata::Dovi::NONE,
        stream_dv_decision: plx_data::metadata::DvDecision::NONE,
        stream_immersive: false,
        title: [0; 128],
        ctxline: [0; 96],
        up_next: None,
        queue: Vec::new(),
        now_ms: 0,
        preview: false,
        resolved_as_preview: false,
    };
}

impl PlaybackSession {
    /// **The per-frame PUBLICATION a screen is shown** (spec §2.3).
    ///
    /// The container tree hands a screen its application state through `Cx.views`, and `AppViews`
    /// is built by `Bridge::split` out of the RIG — so a screen can only ever be shown state the
    /// rig owns, and the session is owned by `App.player`. Rather than widen the library's
    /// `Rig::split` seam (or hand a screen the `&mut` that would let it write the machine's state
    /// from inside a draw), the loop publishes this copy into the rig once per frame, exactly as
    /// `capture_views` publishes every store's snapshot beside it.
    ///
    /// **Everything except [`queue`](Self::queue).** The PlayQueue rows are the only unbounded
    /// field and nothing outside this module reads them ([`with_queue`] has no caller in `ui/`,
    /// `screens/` or `app/`), so they are the one thing a per-frame copy must not carry. The
    /// destructuring is deliberate and load-bearing: adding a field to [`PlaybackSession`] without
    /// deciding whether a screen may see it FAILS THE BUILD here rather than silently publishing
    /// it or silently dropping it.
    pub fn publication(&self) -> PlaybackSession {
        let PlaybackSession {
            direct_play_mode,
            jail_load_blocked,
            repair_status,
            request,
            requested_resume_ns,
            url,
            tsession,
            play_verdict,
            resolve_failed,
            cur_contract,
            cur_enhancement,
            cur_src,
            cur_transport_kbps,
            cur_source_decodable,
            cur_auto_original_watched,
            auto_original,
            auto_fixture_base,
            auto_switches,
            auto_last_switch,
            auto_prior_kbps,
            auto_bootstrap_rung,
            cur_rk,
            cur_sid,
            cur_audio,
            cur_sub_sid,
            cur_sub_sidecar,
            cur_sub_client_drawable,
            side_subs_refused,
            cur_sub_pref_lang,
            cur_part_id,
            sess,
            machine_id,
            machine_sid,
            pq_id,
            pq_item_id,
            src_vcodec,
            src_acodec,
            stream_vcodec,
            stream_acodec,
            stream_fps,
            stream_dovi,
            stream_dv_decision,
            stream_immersive,
            title,
            ctxline,
            up_next,
            now_ms,
            queue: _,
            preview: _,
            resolved_as_preview,
        } = self;
        PlaybackSession {
            direct_play_mode: *direct_play_mode,
            jail_load_blocked: *jail_load_blocked,
            repair_status: *repair_status,
            request: request.clone(),
            requested_resume_ns: *requested_resume_ns,
            url: url.clone(),
            tsession: tsession.clone(),
            play_verdict: play_verdict.clone(),
            resolve_failed: *resolve_failed,
            cur_contract: *cur_contract,
            cur_enhancement: *cur_enhancement,
            cur_src: *cur_src,
            cur_transport_kbps: *cur_transport_kbps,
            cur_source_decodable: *cur_source_decodable,
            cur_auto_original_watched: *cur_auto_original_watched,
            auto_original: auto_original.clone(),
            auto_fixture_base: auto_fixture_base.clone(),
            auto_switches: *auto_switches,
            auto_last_switch: *auto_last_switch,
            auto_prior_kbps: *auto_prior_kbps,
            auto_bootstrap_rung: *auto_bootstrap_rung,
            cur_rk: cur_rk.clone(),
            cur_sid: *cur_sid,
            cur_audio: cur_audio.clone(),
            cur_sub_sid: *cur_sub_sid,
            cur_sub_sidecar: *cur_sub_sidecar,
            cur_sub_client_drawable: *cur_sub_client_drawable,
            side_subs_refused: *side_subs_refused,
            cur_sub_pref_lang: cur_sub_pref_lang.clone(),
            cur_part_id: *cur_part_id,
            sess: sess.clone(),
            machine_id: machine_id.clone(),
            machine_sid: *machine_sid,
            pq_id: pq_id.clone(),
            pq_item_id: pq_item_id.clone(),
            src_vcodec: src_vcodec.clone(),
            src_acodec: src_acodec.clone(),
            stream_vcodec: stream_vcodec.clone(),
            stream_acodec: stream_acodec.clone(),
            stream_fps: *stream_fps,
            stream_dovi: *stream_dovi,
            stream_dv_decision: *stream_dv_decision,
            stream_immersive: *stream_immersive,
            title: *title,
            ctxline: *ctxline,
            up_next: up_next.clone(),
            now_ms: *now_ms,
            queue: Vec::new(),
            // A screen copy is not the live preview. The loop reads the real session.
            preview: false,
            resolved_as_preview: *resolved_as_preview,
        }
    }
}

impl Default for PlaybackSession {
    fn default() -> Self {
        Self::IDLE
    }
}

impl PlaybackSession {
    /// **The frame tick, written once per iteration by [`crate::player::machine::Player::set_now`]
    /// and by nothing else** (spec §4.1). See the [`now_ms`](Self::now_ms) field.
    pub fn set_now(&mut self, now_ms: u32) {
        self.now_ms = now_ms;
    }
}

/// **A borrowable idle session**, for a test that builds a `Cx` and has no playback in it.
///
/// `AppViews::session` is a borrow, and `&PlaybackSession::IDLE` is a temporary that dies at the
/// end of the statement — this is the one long-lived idle value those fixtures share.
#[cfg(any(test, feature = "test-support"))]
pub fn idle_session_for_test() -> &'static PlaybackSession {
    static IDLE: std::sync::OnceLock<PlaybackSession> = std::sync::OnceLock::new();
    IDLE.get_or_init(|| PlaybackSession::IDLE)
}

/// Put a session back to [`PlaybackSession::IDLE`] — the whole session at once, HUD buffers and the
/// `/identity` cache included.
///
/// **Test-only, and that is a statement about the app rather than about scoping.** No production
/// path ends a session by clearing all of it: a real teardown clears the transcode session, its
/// remux flag and the URL ([`scrobble_stop`] then [`clear_url`], both from `engine::teardown`) and
/// deliberately leaves the rest standing, because callers read it AFTER the stop — `app.rs`'s
/// `exit_player` calls `stop_bufferfeed` and then asks [`cur_rk`] for the episode to open the show
/// page at.
/// Widening teardown into a full reset would hand it an empty string. So the reset serves the case
/// where a session really does end with nothing left to read: a test that installed a plan owes the
/// next one an idle module, exactly as `fresh_registry` owes it an empty server table.
#[cfg(test)]
fn reset_session(ps: &mut PlaybackSession) {
    *ps = PlaybackSession::IDLE;
}

// ---- accessors: the player reads the URL/session; the HUD reads the title/ctxline ----
// Their signatures and meanings are the module's whole public surface — `app.rs`, `player/` and
// `ui/` call them heavily — so collecting the state behind them changed the BODIES only.
pub fn url(ps: &PlaybackSession) -> String {
    ps.url.clone()
}
/// Is there a stream URL at all? The in-place twin of [`url`], for the callers that only want the
/// emptiness — [`is_transcoding`]'s idiom, and for the same reason: a universal-transcode
/// `start.mkv` URL is several hundred bytes, and the player route is exempt from the idle present
/// gate, so a `!url().is_empty()` in a draw is a heap allocation and a memcpy at ~60/s.
pub fn has_url(ps: &PlaybackSession) -> bool {
    !ps.url.is_empty()
}
/// Whole-file transport requirement captured by the playback resolve. Diagnostics uses it for
/// manual Original, where no adaptive controller exists to publish `dg_abr_kbps`.
pub fn transport_kbps(ps: &PlaybackSession) -> i64 {
    ps.cur_transport_kbps
}
pub fn set_url(ps: &mut PlaybackSession, s: &str) {
    ps.url = s.to_owned()
}
pub fn clear_url(ps: &mut PlaybackSession) {
    ps.url.clear()
}
pub fn transcode_session(ps: &PlaybackSession) -> String {
    ps.tsession.clone()
}

/// Thread-safe active PMS resource identity. While transcoding it names the coupled physical
/// encoder/Streaming Resource; while direct-playing it names the logical Streaming Resource only.
/// `PlaybackSession::tsession` remains the main-thread playback classification bit, so owning a direct
/// resource does not relabel it as a transcode. Adaptive HLS can replace the server identity from
/// its demux worker without racing that `static mut` state. Teardown atomically takes this value,
/// so a late candidate can never publish itself after the stop owner has retired the playback.
#[derive(Clone)]
struct ActiveHlsRoute {
    url: String,
    rung: crate::abr::Rung,
    /// Actual master declaration + decoded raster, never inferred from `rung`.
    observed: Option<(crate::abr::ObservedHlsVariant, u32)>,
}

pub(super) struct ActiveEncoderState {
    /// Monotone semantic route generation. The PMS id may deliberately stay unchanged while the
    /// route changes from HLS to direct Original, so the id alone is not an ownership token.
    pub(super) epoch: u64,
    pub(super) id: String,
    hls: Option<ActiveHlsRoute>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserRouteIntent {
    Retranscode,
    NativeAudioReload,
    AdaptiveReload,
    RecoverOriginal(RecoveryCause),
}

/// Why an HLS/remux → Original recovery is being attempted. The cause decides which applied
/// contract may authorise it and whether the Auto watchdog keeps watching the result, so it rides
/// the intent instead of being re-derived from a quality atomic that may have moved since.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryCause {
    /// The Auto worker proved the source fits: only an applied Auto contract may take it.
    Automatic,
    /// The viewer picked Original: the DESIRED quality must still say so at claim time.
    ManualOriginal,
    /// Issue #266: the enhanced remux is no longer wanted (toggle off, subtitle on, a track the
    /// enhancement cannot carry) and the candidate direct-plays. The route stays Original-family,
    /// so either applied Auto or Original may take it, and an Auto playback keeps its watchdog.
    EnhancementReleased,
}

impl RecoveryCause {
    /// The log prefix this cause's recovery lines carry.
    fn log_tag(self) -> &'static str {
        match self {
            RecoveryCause::Automatic => "auto",
            RecoveryCause::ManualOriginal => "quality",
            RecoveryCause::EnhancementReleased => "enhancement",
        }
    }

    /// Whether [`admit_original_part`] must ask the server before this cause's Direct trial is
    /// published. `Automatic` already has its answer: the Auto watchdog samples this exact Part
    /// on this exact identity on its own worker thread before it ever proposes the recovery
    /// (`probe_original_while_hls_cancellable`), so a second admission on the main thread would
    /// only add up to its own budget (`PART_ADMISSION_BUDGET`) of UI/feed block for a question
    /// already answered. `ManualOriginal` and `EnhancementReleased` fire from something the viewer
    /// just did, with no prior sample of this Part, so they still need to ask.
    fn needs_part_admission(self) -> bool {
        match self {
            RecoveryCause::Automatic => false,
            RecoveryCause::ManualOriginal | RecoveryCause::EnhancementReleased => true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutomaticRouteIntent {
    OriginalToHls {
        ticket: WorkerTicket,
        conservative_kbps: u32,
        position_ns: i64,
    },
    HlsToOriginal {
        ticket: WorkerTicket,
        evidence_kbps: u32,
        position_ns: i64,
    },
}

/// Result of handing an automatic decision to the main-thread route owner. `Busy` means the same
/// worker ticket is still current but another explicit/trial transition owns the boundary; callers
/// retain their decision and retry. Only `Accepted` transfers responsibility strongly enough for a
/// producer to exit, and only `Stale` proves that this worker may never publish again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomaticIntentResult {
    Accepted,
    Busy,
    Stale,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteIntent {
    User(UserRouteIntent),
    Automatic(AutomaticRouteIntent),
}

#[derive(Clone, Debug)]
pub struct ClaimedRouteAction {
    serial: u64,
    pub ticket: WorkerTicket,
    pub intent: RouteIntent,
    /// Issue #266: a track pick's own reload was DISPLACED by an enhancement reconcile queued in
    /// its place (`PlayerControl::displaced_pick`). Taken with the user intent it rode, so a
    /// claim that cannot honour the enhancement still owes the pick its legacy reload.
    pub displaced_pick: bool,
    /// Set only for a claim whose PMS half was dispatched to a worker
    /// ([`execute_retranscode_claim`]'s `Pending` arm). Captures the revision/quality/projection
    /// this exact claim was built from, so a second edit that queues mid-flight (which DOES
    /// advance `desired_revision`/`ps` immediately, since it has no reason to wait) cannot get
    /// silently recorded as if this claim had applied it. `None` for every claim settled inside
    /// the same frame it was claimed in, where reading `PlayerControl`/`ps` live is exactly
    /// correct because nothing else could have run in between.
    pub(super) claim_snapshot: Option<Box<ClaimSnapshot>>,
}

impl ClaimedRouteAction {
    /// The claim's identity, for a holder outside `route` (the player's presentation hold) that
    /// must tell THIS claim's flight from any other.
    pub fn serial(&self) -> u64 {
        self.serial
    }
}

/// See [`ClaimedRouteAction::claim_snapshot`]. `projection` is intentionally not `Debug` (it embeds
/// [`AutoOriginalCandidate`], which carries no derive) — the manual impl below reports the two
/// scalar fields, which are what a log line about a stale claim actually wants.
#[derive(Clone)]
pub(super) struct ClaimSnapshot {
    revision: u64,
    quality: Quality,
    pub(super) projection: AppliedRouteProjection,
}

impl std::fmt::Debug for ClaimSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaimSnapshot")
            .field("revision", &self.revision)
            .field("quality", &self.quality)
            .finish_non_exhaustive()
    }
}

/// Identity of one prepared route transaction.
///
/// PMS/Session preparation and native construction are two different external effects. A
/// transaction may mint multiple never-reused [`RouteStartAttempt`]s; the attempt, not this
/// transaction, prevents a late `Load` result from settling a retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteStartTransaction {
    pub(super) serial: u64,
}

/// Synchronous result of the native half of a prepared route transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteStartResult {
    Started,
    NoRoute,
    StartFailed,
}

/// Native half of an Original handoff.  A successful `sf_load` only proves that the payload was
/// accepted; the old HLS route cannot be retired until the new source produces a decoded frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OriginalTrialPhase {
    /// A recovery FLIGHT ([`super::flight`]) holds the trial's own start transaction: the foreground
    /// restore's rebuild of a session suspended mid-trial, or a rollback's rebase. The trial's
    /// rollback snapshot (`PlayerControl::pending_original`) is NOT part of the phase, so flying
    /// leaves it exactly as it was, and every way out of the flight hands the transaction back to
    /// the trial (`Prepared` when it installs or is dropped, `Failed` when it is refused) —
    /// where an ordinary [`ControlPhase::Preparing`] would have erased it.
    Preparing(u64),
    Prepared(u64),
    Starting(u64, u64),
    AwaitingFrame(u64),
    Failed(u64),
}

/// The only three ways an external route effect may return to the synchronized owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteApplyResult {
    /// PMS accepted and installed the candidate projection. Native `Load` is still outstanding;
    /// this moves `Applying -> Prepared`. [`claim_route_start_attempt`] later moves it to
    /// `Starting`, and it never moves directly to `Stable`.
    Prepared,
    /// The external system refused or failed before changing the applied route.
    Rejected,
    /// A newer command/lease superseded this result while its effect was in flight.
    Cancelled,
}

/// The part of [`Session`] which describes bytes already accepted as the live route.
///
/// User controls are allowed to stage a different projection in `Session` while their PMS/native
/// effect is being built, because `Session` is main-thread-confined.  They are not allowed to make
/// that proposal look applied after the effect is refused.  `PlayerControl` therefore retains this
/// complete value at every commit and restores it on `Rejected`/`Cancelled`; no individual setter
/// has to remember which neighbouring fields form one decoder/server contract.
#[derive(Clone)]
pub(super) struct AppliedRouteProjection {
    url: String,
    tsession: String,
    contract: plx_plex::plex::EncodeContract,
    enhancement: EnhancementOutcome,
    auto_original_watched: bool,
    auto_original: Option<AutoOriginalCandidate>,
    audio: Option<CarriedAudio>,
    subtitle_sid: i64,
    subtitle_sidecar: bool,
    subtitle_client_drawable: bool,
    stream_vcodec: String,
    pub(super) stream_acodec: String,
    stream_fps: f64,
    stream_dovi: plx_data::metadata::Dovi,
    stream_dv_decision: plx_data::metadata::DvDecision,
    stream_immersive: bool,
}

fn route_projection(ps: &PlaybackSession) -> AppliedRouteProjection {
    let s = &*ps;
    AppliedRouteProjection {
        url: s.url.clone(),
        tsession: s.tsession.clone(),
        contract: s.cur_contract,
        enhancement: s.cur_enhancement,
        auto_original_watched: s.cur_auto_original_watched,
        auto_original: s.auto_original.clone(),
        audio: s.cur_audio.clone(),
        subtitle_sid: s.cur_sub_sid,
        subtitle_sidecar: s.cur_sub_sidecar,
        subtitle_client_drawable: s.cur_sub_client_drawable,
        stream_vcodec: s.stream_vcodec.clone(),
        stream_acodec: s.stream_acodec.clone(),
        stream_fps: s.stream_fps,
        stream_dovi: s.stream_dovi,
        stream_dv_decision: s.stream_dv_decision,
        stream_immersive: s.stream_immersive,
    }
}

fn install_route_projection(ps: &mut PlaybackSession, projection: &AppliedRouteProjection) {
    { let s = &mut *ps; {
        s.url = projection.url.clone();
        s.tsession = projection.tsession.clone();
        s.cur_contract = projection.contract;
        s.cur_enhancement = projection.enhancement;
        s.cur_auto_original_watched = projection.auto_original_watched;
        s.auto_original = projection.auto_original.clone();
        s.cur_audio = projection.audio.clone();
        s.cur_sub_sid = projection.subtitle_sid;
        s.cur_sub_sidecar = projection.subtitle_sidecar;
        s.cur_sub_client_drawable = projection.subtitle_client_drawable;
        s.stream_vcodec = projection.stream_vcodec.clone();
        s.stream_acodec = projection.stream_acodec.clone();
        s.stream_fps = projection.stream_fps;
        s.stream_dovi = projection.stream_dovi;
        s.stream_dv_decision = projection.stream_dv_decision;
        s.stream_immersive = projection.stream_immersive;
    } };
}

/// Publish the main-thread projection which now belongs to the physical route.  Capture before
/// taking `PLAYER_CONTROL`: session access is main-thread-only, while workers only need the owned
/// clone behind the mutex.
fn publish_applied_route_projection(ps: &PlaybackSession) {
    let projection = route_projection(ps);
    PLAYER_CONTROL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .applied_projection = Some(projection);
}

/// Commit a contract change which requires no PMS/native route replacement.  This is still a
/// reducer event: otherwise a later rejected action restores the older snapshot and silently
/// undoes the already-visible quality/subtitle choice.
fn commit_in_place_route_projection(ps: &PlaybackSession, quality_contract: bool) {
    let projection = route_projection(ps);
    let audio_stream_id = projection.audio.as_ref().map_or(0, |a| a.sid);
    let subtitle_stream_id = projection.subtitle_sid;
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    match control.phase {
        ControlPhase::Stable | ControlPhase::StagingUser(_) | ControlPhase::Completing(_) => {
            if quality_contract {
                control.applied_revision = control.desired_revision;
                control.applied_quality = control.desired_quality;
            }
            control.applied_projection = Some(projection);
        }
        ControlPhase::OriginalTrial(_) if !quality_contract => {
            // A client-rendered subtitle edit belongs to the candidate being graded, not to the
            // retained HLS rollback route. First-frame confirmation will publish this snapshot.
            if let Some(pending) = control.pending_original.as_mut() {
                pending.candidate_projection = projection;
            }
        }
        _ => return,
    }
    if let Some(timeline) = control.timeline.as_mut() {
        timeline.audio_stream_id = audio_stream_id;
        timeline.subtitle_stream_id = subtitle_stream_id;
    }
}

/// Immutable main-thread projection consumed by the periodic timeline worker. `Session` itself is
/// deliberately absent: it is main-thread-confined, while this owned clone lives under the same
/// mutex as the active encoder and engine epoch. A report therefore observes either the complete
/// old playback projection or the complete new one, never a hybrid assembled across a reload.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TimelineProjection {
    sid: ServerId,
    rating_key: String,
    logical_session: String,
    play_queue_id: String,
    play_queue_item_id: String,
    audio_stream_id: i64,
    subtitle_stream_id: i64,
}

/// One reporter's authority to sample the playback which spawned it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimelineLease {
    engine_epoch: u64,
    /// Every stop announced before this Engine was published must finish its final old-playback
    /// timeline effect before this reporter may send the replacement playback's first update.
    required_stop: u64,
}

#[derive(Clone)]
struct TimelineSnapshot {
    sid: ServerId,
    rating_key: String,
    state: plx_plex::plex::TimelineState,
    time_ms: i64,
    duration_ms: i64,
    session: String,
    play_queue_id: String,
    play_queue_item_id: String,
    audio_stream_id: i64,
    subtitle_stream_id: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlPhase {
    Idle,
    Resolving,
    Stable,
    /// A user edit has crossed the desired-contract boundary but has not yet either committed
    /// in-place or entered `pending_user`. Automatic publication is Busy throughout this window.
    StagingUser(u64),
    Applying(u64),
    /// Candidate preparation is in flight while the old Engine/route is still recoverable.
    Preparing(u64),
    /// Candidate preparation committed but no physical Load attempt currently owns it.
    Prepared(u64),
    /// One exact physical Load attempt is in flight: `(transaction, attempt)`.
    Starting(u64, u64),
    /// Native start/frame proof committed; transaction-attached user effects are being reduced
    /// while automatic workers remain fenced. `Stable` is published only after they are queued.
    Completing(u64),
    OriginalTrial(OriginalTrialPhase),
    Failed(u64),
    Stopping,
}

impl ControlPhase {
    /// The serial of the start transaction a flight holds in this phase: an ordinary
    /// [`ControlPhase::Preparing`], or an Original trial's own ([`OriginalTrialPhase::Preparing`]).
    fn preparing_serial(self) -> Option<u64> {
        match self {
            ControlPhase::Preparing(serial) | ControlPhase::OriginalTrial(OriginalTrialPhase::Preparing(serial)) => {
                Some(serial)
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResolveFallback {
    Idle,
    Stable,
    Failed(u64),
    /// An Original trial's own transaction (`OriginalTrial(Prepared(serial))`), handed back by an
    /// Engine-less recovery flight a newer request superseded. The trial's snapshot
    /// (`pending_original`) was never touched, and its candidate projection is the session's, so a
    /// cancelled resolve restores the PHASE only (no applied projection over the candidate's).
    Trial(u64),
}

/// The synchronized authority for route ownership and route-changing intents. `Session` remains
/// the main-thread projection used to build URLs/payloads; workers are never allowed to infer
/// ownership from it. PMS/native I/O is deliberately performed after an action is claimed and
/// this mutex is released, then completed through a typed transition below.
pub(super) struct PlayerControl {
    pub(super) active: ActiveEncoderState,
    pub(super) engine_epoch: u64,
    pub(super) media_epoch: u64,
    /// Latest user-visible contract edit. It fences automatic publication through pending/phase,
    /// but is not a worker credential.
    desired_revision: u64,
    /// Quality preference represented by `desired_revision`. The process-wide picker is only the
    /// durable user preference; it cannot also describe bytes PMS has not accepted yet.
    desired_quality: Quality,
    /// Revision represented by the physical route and therefore carried by WorkerTicket.
    pub(super) applied_revision: u64,
    /// Quality policy which owns the physical worker. A refused Fixed/Original request leaves this
    /// unchanged, so an already-accepted Auto handoff is never relabelled as the failed desire.
    applied_quality: Quality,
    /// Last complete `Session` projection whose external effect committed.  A refused proposal is
    /// rolled back to this value as one transition rather than by a collection of field fixes.
    applied_projection: Option<AppliedRouteProjection>,
    next_action: u64,
    pending_user: Option<UserRouteIntent>,
    /// The USER intent the claim that owns `Applying(serial)` took out of `pending_user`, with its
    /// `displaced_pick` marker: `(serial, intent, displaced_pick)`. A claim flight that an app-switch
    /// suspend drops ([`begin_engine_teardown`]) still owes the viewer's pick, so the teardown puts
    /// the intent back in `pending_user` for the foreground restore's Engine to claim. Only read
    /// under a matching `Applying(serial)`; a settled claim's record is dead weight, never consulted.
    claimed_user: Option<(u64, UserRouteIntent, bool)>,
    /// Issue #266: the pending user intent stands in for a track pick's reload (see
    /// [`reconcile_enhancement`]). Lives and dies with `pending_user`: cleared at every site that
    /// clears it, taken with it at claim, so it can never outlive the pick it describes.
    displaced_pick: bool,
    pending_auto: Option<AutomaticRouteIntent>,
    /// Latest requested playhead which has not yet crossed a real media discontinuity.
    pending_seek_ns: Option<i64>,
    phase: ControlPhase,
    /// The serial of the flight a worker is running (`flight.rs`): set when a claim or a seek
    /// dispatches its PMS half, cleared when the landing is drained or discarded. The PHASE stays
    /// the fence (`Applying(serial)` for a claim, `Preparing(serial)` for a seek), so a record a
    /// teardown strands cannot wedge anything; what the record adds is the one fact a phase cannot
    /// say, that a `Preparing` is a flight a worker will land rather than a preparation one frame
    /// runs inline.
    flight: Option<u64>,
    /// The serial of the flight that is a RECOVERY (`FlightOwner::Recovery`): a failed start's
    /// replacement route being prepared while the Engine that failed waits for it. Set by
    /// [`begin_recovery_flight`] beside `flight` and cleared with it ([`PlayerControl::clear_flight`]).
    flight_recovery: Option<u64>,
    /// The serial of the recovery flight that is a COLD RESUME's (`RebaseFor::Resume`): a resolved
    /// transcode's route being rebuilt at the saved position before its first Load, with no Engine
    /// anywhere. Set by [`begin_resume_flight`] beside `flight_recovery`, cleared with it. It is
    /// the one fact the HUD (`player::state` answers `Resolving`) and the app loop's drain
    /// ([`resume_flight_open`]) need that a recovery's own marker does not say.
    flight_resume: Option<u64>,
    /// The serial of the recovery flight that has NO ENGINE anywhere: the cold resume's
    /// ([`begin_resume_flight`]), the foreground restore's resume and the rollback of an Original
    /// trial whose Load failed before an Engine existed ([`begin_engineless_flight`]). No pump
    /// waits on such a flight, so the app loop drains it, the HUD reads `Resolving` through it
    /// ([`engineless_flight_open`]) and an app-switch suspend drops it. Set beside
    /// `flight_recovery`, cleared with it.
    flight_engineless: Option<u64>,
    /// The position, in nanoseconds, a RECOVERY flight rebuilds the route at: the offset the viewer
    /// is "at" while the flight is out. Set beside `flight_recovery` at the dispatch
    /// ([`begin_recovery_flight`]), cleared with it. An Engine-less flight (a cold or foreground
    /// resume, an Engine-less rollback) has seeded NO clock yet (`playpos_ns` is seeded at the
    /// landing), so [`recovery_flight_offset_ns`] is the only place the position survives for a
    /// suspend that snapshots it while the flight is still flying (`player::intended_pos_ns`).
    flight_offset_ns: Option<i64>,
    /// Exact phase hidden by an asynchronous resolve. URL presence cannot distinguish a retained
    /// live route from a failed candidate which still owns cleanup state.
    resolve_fallback: Option<ResolveFallback>,
    /// Native Load results cross back from the media thread here. The main thread drains them only
    /// after the Engine is installed, so a fast `sf_load` cannot publish Stable in front of its
    /// own Engine slot. Tokens make late results harmless rather than requiring queue erasure.
    next_start_attempt: u64,
    start_results: Vec<(RouteStartAttempt, RouteStartResult)>,
    last_start_result: Option<(RouteStartAttempt, RouteStartResult)>,
    /// Commands staged while an Original trial owns neither a proven candidate nor a restored
    /// HLS Engine. They belong to the exact rollback transaction and are consumed only after its
    /// matching native start succeeds.
    start_deferred: Option<(u64, DeferredOriginalEffects)>,
    pending_original: Option<PendingOriginal>,
    timeline: Option<TimelineProjection>,
}

impl PlayerControl {
    /// Forget the flight a worker was running, of whichever kind.
    fn clear_flight(&mut self) {
        self.flight = None;
        self.flight_recovery = None;
        self.flight_resume = None;
        self.flight_engineless = None;
        self.flight_offset_ns = None;
    }
}

/// The physical stream the demux worker has committed, including the HLS declaration that has to
/// survive a main-thread reload.  `Session` remains main-thread-confined; keeping only the worker-
/// mutable projection here is what lets an ABR commit publish encoder + URL + rung atomically
/// without racing the rest of the route.
static PLAYER_CONTROL: std::sync::Mutex<PlayerControl> = std::sync::Mutex::new(PlayerControl {
    active: ActiveEncoderState {
        epoch: 0,
        id: String::new(),
        hls: None,
    },
    engine_epoch: 1,
    media_epoch: 1,
    desired_revision: 1,
    desired_quality: Quality::Original,
    applied_revision: 1,
    applied_quality: Quality::Original,
    applied_projection: None,
    next_action: 0,
    pending_user: None,
    claimed_user: None,
    displaced_pick: false,
    pending_auto: None,
    pending_seek_ns: None,
    phase: ControlPhase::Stable,
    flight: None,
    flight_recovery: None,
    flight_resume: None,
    flight_engineless: None,
    flight_offset_ns: None,
    resolve_fallback: None,
    next_start_attempt: 0,
    start_results: Vec::new(),
    last_start_result: None,
    start_deferred: None,
    pending_original: None,
    timeline: None,
});

fn advance_route(active: &mut ActiveEncoderState) {
    active.epoch = next_route_epoch(active.epoch);
}

fn desired_contract_revision() -> u64 {
    PLAYER_CONTROL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .desired_revision
}

fn applied_quality() -> Quality {
    PLAYER_CONTROL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .applied_quality
}

fn desired_quality() -> Quality {
    PLAYER_CONTROL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .desired_quality
}

fn retarget_automatic_intent(
    intent: &mut AutomaticRouteIntent,
    ticket: WorkerTicket,
    position_ns: Option<i64>,
) {
    match intent {
        AutomaticRouteIntent::OriginalToHls {
            ticket: current,
            position_ns: position,
            ..
        }
        | AutomaticRouteIntent::HlsToOriginal {
            ticket: current,
            position_ns: position,
            ..
        } => {
            *current = ticket;
            if let Some(position_ns) = position_ns {
                *position = position_ns.max(0);
            }
        }
    }
}

/// Cross one explicit desired-contract boundary while holding [`PLAYER_CONTROL`]. An automatic
/// handoff which was already accepted remains labelled with the *applied* ticket that produced it:
/// its producer may have stopped after publication, and relabelling that evidence as the new
/// desire is precisely the PMS-refusal race this split exists to prevent. Seek is different: it
/// changes the target carried by that same accepted handoff and is handled explicitly below.
fn advance_user_contract_locked(control: &mut PlayerControl) {
    control.desired_revision = next_generation(control.desired_revision);
}

/// Fence an outgoing worker before the main thread publishes any part of a new user contract.
/// Quality and track setters call this before changing their durable/session projection; the
/// later route-action request deliberately crosses a second boundary when it enters the action
/// queue, because either boundary can also be reached independently.
struct UserEditGuard {
    serial: Option<u64>,
}

impl Drop for UserEditGuard {
    fn drop(&mut self) {
        let Some(serial) = self.serial.take() else {
            return;
        };
        let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        if control.phase == ControlPhase::StagingUser(serial) {
            // Every projection/pending-intent write happened before this edge. Workers can now
            // observe the complete edit, never the half between a persisted checkmark and action.
            control.phase = ControlPhase::Stable;
        }
    }
}

fn begin_user_contract_boundary() -> UserEditGuard {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    advance_user_contract_locked(&mut control);
    let serial = if control.phase == ControlPhase::Stable {
        control.next_action = next_generation(control.next_action);
        let serial = control.next_action;
        control.phase = ControlPhase::StagingUser(serial);
        Some(serial)
    } else {
        None
    };
    UserEditGuard { serial }
}

/// Publish a quality preference into the desired half of the route reducer before any Session
/// projection changes. The applied half moves only when PMS/native commits the matching action.
fn begin_user_quality_boundary(quality: Quality) -> UserEditGuard {
    let guard = begin_user_contract_boundary();
    PLAYER_CONTROL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .desired_quality = quality;
    guard
}

fn merge_user_route_intent(
    pending: Option<UserRouteIntent>,
    incoming: UserRouteIntent,
    preserve_original_recovery: bool,
) -> UserRouteIntent {
    use UserRouteIntent::{AdaptiveReload, NativeAudioReload, RecoverOriginal, Retranscode};
    match (pending, incoming) {
        (_, RecoverOriginal(cause)) => RecoverOriginal(cause),
        (Some(RecoverOriginal(cause)), Retranscode) if preserve_original_recovery => {
            RecoverOriginal(cause)
        }
        (Some(RecoverOriginal(_)), newer) => newer,
        (Some(Retranscode), NativeAudioReload | AdaptiveReload)
        | (Some(NativeAudioReload | AdaptiveReload), Retranscode) => Retranscode,
        (Some(NativeAudioReload), AdaptiveReload) | (Some(AdaptiveReload), NativeAudioReload) => {
            NativeAudioReload
        }
        (_, newer) => newer,
    }
}

/// Begin a new explicit playback request. The outgoing Engine may keep rendering while the PMS
/// resolve runs, but none of its asynchronous ABR evidence may mutate the route after this point.
/// A matching landing is the successful exit from `Resolving`; cancellation or spawn failure
/// restores the captured Idle/Stable/Failed fallback.
fn begin_playback_request() -> bool {
    // A fresh request always starts a new item/session. Any retranscode-claim landing still in
    // the mailbox belongs to whatever was playing before and must never reach this one — see
    // `discard_retranscode_claim_slot`'s doc for the corruption this prevents.
    discard_retranscode_claim_slot();
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    let fallback = match control.phase {
        // A newer resolve may supersede an older worker, but it inherits the first resolve's
        // fallback. Re-snapshotting `Resolving` would lose the route hidden underneath it.
        ControlPhase::Resolving => None,
        // A cold resume's flight (no Engine, nothing started) is superseded by the newer request:
        // its landing was discarded above (or is discarded at the worker's post, now that the phase
        // names no flight), stopping only the replacement it registered. The prepared route it was
        // rebuilding is the one left in `Failed`, exactly as a start that never got a Load.
        ControlPhase::Preparing(serial) if control.flight_engineless == Some(serial) => {
            control.clear_flight();
            Some(ResolveFallback::Failed(serial))
        }
        // The same for a recovery flight over an Original TRIAL's transaction (the foreground
        // restore's resume of a session suspended mid-trial, an Engine-less rollback): it is
        // dropped exactly as a suspend drops it, the transaction going back to the trial as
        // `Prepared` with the snapshot untouched, and a resolve that gives up restores that.
        ControlPhase::OriginalTrial(OriginalTrialPhase::Preparing(serial))
            if control.flight_engineless == Some(serial) =>
        {
            control.clear_flight();
            Some(ResolveFallback::Trial(serial))
        }
        ControlPhase::Stable => Some(ResolveFallback::Stable),
        ControlPhase::Failed(serial) => Some(ResolveFallback::Failed(serial)),
        // Stopping is observable only while the synchronous main-thread teardown owns the loop —
        // [`finish_engine_teardown`] publishes `Idle` the moment that teardown returns. Either
        // way there is no live Engine left to preserve, which is why they share one fallback.
        ControlPhase::Idle | ControlPhase::Stopping => Some(ResolveFallback::Idle),
        // Do not hide a native/PMS transaction under Resolving. Its matching completion would
        // otherwise have nowhere truthful to land, and cancelling the resolve would fabricate an
        // Idle route around a live Engine.
        ControlPhase::Preparing(_)
        | ControlPhase::Prepared(_)
        | ControlPhase::Starting(_, _)
        | ControlPhase::StagingUser(_)
        | ControlPhase::Applying(_)
        | ControlPhase::Completing(_)
        | ControlPhase::OriginalTrial(_) => return false,
    };
    control.desired_revision = next_generation(control.desired_revision);
    control.desired_quality = quality();
    control.pending_user = None;
    control.displaced_pick = false;
    control.pending_auto = None;
    control.pending_seek_ns = None;
    if let Some(fallback) = fallback {
        control.resolve_fallback = Some(fallback);
    }
    control.phase = ControlPhase::Resolving;
    true
}

/// Publish the PMS half of a resolve. A refused plan has no Engine and therefore lands in `Idle`;
/// a playable plan lands in `Prepared`. Claiming a physical `Load` moves it to `Starting`, and only
/// settling that exact attempt through [`settle_route_start`] may publish `Stable`. In particular,
/// installing a URL is not evidence that the television accepted it.
fn prepare_playback_landing(ps: &PlaybackSession, playable: bool) -> Option<RouteStartTransaction> {
    let projection = playable.then(|| route_projection(ps));
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    // Normal landings arrive from `Resolving`, whose request already captured the preference.
    // Fixture/direct-plan installs deliberately bypass that async request; in that case the
    // current durable picker is the only desired contract and must own the installed worker.
    if control.phase != ControlPhase::Resolving {
        control.desired_quality = quality();
    }
    control.engine_epoch = next_generation(control.engine_epoch);
    control.media_epoch = next_generation(control.media_epoch);
    if playable {
        control.applied_revision = control.desired_revision;
        control.applied_quality = control.desired_quality;
        control.applied_projection = projection;
    }
    control.pending_auto = None;
    if !playable {
        control.timeline = None;
    }
    let serial = if playable {
        // A dev fixture can install its real route from inside start_bufferfeed, after that call
        // already reserved a route-start transaction. Reuse the same transaction owner instead of
        // replacing it between construction and settlement.
        let serial = match control.phase {
            ControlPhase::Preparing(serial)
            | ControlPhase::Prepared(serial)
            | ControlPhase::Starting(serial, _) => serial,
            _ => {
                control.next_action = next_generation(control.next_action);
                control.next_action
            }
        };
        if !matches!(control.phase, ControlPhase::Starting(_, _)) {
            control.phase = ControlPhase::Prepared(serial);
        }
        serial
    } else {
        control.phase = ControlPhase::Idle;
        control.resolve_fallback = None;
        return None;
    };
    control.resolve_fallback = None;
    Some(RouteStartTransaction { serial })
}

/// Route unit tests install plans without a native Engine. Treat their explicit plan helper as
/// the successful native boundary; reducer-specific tests call `prepare_playback_landing`
/// directly to inspect `Prepared`, then explicitly claim and settle an attempt to inspect
/// `Starting` or `Failed`.
fn settle_plan_start_in_unit_test(ps: &mut PlaybackSession, start: Option<RouteStartTransaction>) {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(start) = start {
        if let Some(attempt) = claim_route_start_attempt(start) {
            let _ = settle_route_start(ps, attempt, RouteStartResult::Started);
        }
    }
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = (ps, start);
}

/// Settle a resolve which will never produce a landing (cancelled or failed to spawn). Restore the
/// exact phase hidden by `Resolving`: URL presence cannot distinguish a live Stable route from a
/// failed candidate which merely retains its cleanup projection. A late worker cannot apply
/// because PLAY_GEN owns that separate mailbox.
fn cancel_playback_request(ps: &mut PlaybackSession, _playable: bool) {
    let (fallback, restore) = {
        let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        if control.phase != ControlPhase::Resolving {
            return;
        }
        let fallback = control.resolve_fallback.unwrap_or(ResolveFallback::Idle);
        let restore = matches!(
            fallback,
            ResolveFallback::Stable | ResolveFallback::Failed(_)
        )
        .then(|| control.applied_projection.clone())
        .flatten();
        (fallback, restore)
    };
    // request_play publishes the incoming item's track/reset projection before its worker runs.
    // Restore the retained route while Resolving still blocks workers; Stable must be the last
    // publication, never a window in front of a hybrid Session.
    if let Some(applied) = restore {
        install_route_projection(ps, &applied);
    }
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if control.phase != ControlPhase::Resolving {
        return;
    }
    control.pending_auto = None;
    control.resolve_fallback = None;
    control.phase = match fallback {
        ResolveFallback::Idle => ControlPhase::Idle,
        ResolveFallback::Stable => ControlPhase::Stable,
        ResolveFallback::Failed(serial) => ControlPhase::Failed(serial),
        ResolveFallback::Trial(serial) => ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial)),
    };
}

/// Settle the deterministic main-thread half of a resolve whose worker could not be spawned.
/// `request_play` deliberately leaves the outgoing URL installed while resolving; retain that
/// still-playable route instead of unconditionally landing the controller in `Idle`.
fn settle_failed_resolve_spawn(ps: &mut PlaybackSession) {
    cancel_playback_request(ps, has_url(ps));
}

/// Capture the complete ownership generation for a newly spawned media worker.
pub fn worker_ticket() -> WorkerTicket {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    worker_ticket_of(&control)
}

/// Publish an automatic route request iff all evidence still belongs to the current engine,
/// media position, user contract and semantic route. A refusal is ordinary supersession: the
/// worker keeps/abandons its local transaction as appropriate and no playback error is raised.
pub fn publish_automatic_route_intent(
    intent: AutomaticRouteIntent,
) -> AutomaticIntentResult {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if !ticket_is_current(&control, automatic_ticket(&intent)) {
        return AutomaticIntentResult::Stale;
    }
    if control.phase != ControlPhase::Stable
        || control.pending_user.is_some()
        || control.pending_auto.is_some()
        || control.pending_seek_ns.is_some()
    {
        return AutomaticIntentResult::Busy;
    }
    control.pending_auto = Some(intent);
    AutomaticIntentResult::Accepted
}

/// Queue the latest explicit route contract. Unlike an automatic request it survives pre-roll and
/// an Original trial. Multiple user changes coalesce to the newest desired contract; their durable
/// fields already live in `Session`, so one later rebuild applies the whole projection.
pub fn request_user_route_intent(ps: &PlaybackSession, intent: UserRouteIntent) {
    queue_user_route_intent(ps, intent, false);
}

/// [`request_user_route_intent`], optionally marking that the queued intent stands in for a
/// track pick's own reload (issue #266's `displaced_pick`). One lock for both writes, so a claim
/// can never take the intent without the marker that explains it.
fn queue_user_route_intent(ps: &PlaybackSession, intent: UserRouteIntent, displaced_pick: bool) {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    control.displaced_pick |= displaced_pick;
    // Setters already crossed an early boundary before publishing their projection. Queueing the
    // resulting route action is a second, independently reachable boundary: callers such as the
    // automatic-recovery UI can request an action directly, and both paths must fence old tickets.
    advance_user_contract_locked(&mut control);
    // Subtitle Off is the one Retranscode which does not invalidate an in-flight Original
    // recovery. The candidate is updated to carry `None`, and a failed Original open necessarily
    // rebases the restored HLS route (`dispatch_rollback_rebase`), which reads the current subtitle id.
    // Audio, subtitle On, and a fixed/Auto quality pick invalidate either the candidate or the
    // `Original` selection before reaching this merge, so their newer actions still win.
    let preserve_original_recovery =
        quality() == Quality::Original && ps.auto_original.is_some();
    control.pending_user = Some(merge_user_route_intent(
        control.pending_user.take(),
        intent,
        preserve_original_recovery,
    ));
}

/// Record a desired seek without pretending it has already changed the physical media timeline.
/// Automatic publication is Busy while this obligation is pending; an already accepted handoff
/// is retargeted because its producer may already have stopped. Only [`commit_user_seek`] advances
/// the worker-visible media epoch.
pub fn note_user_seek_intent(position_ns: i64) {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    control.pending_seek_ns = Some(position_ns.max(0));
    let ticket = worker_ticket_of(&control);
    if let Some(automatic) = control.pending_auto.as_mut() {
        retarget_automatic_intent(automatic, ticket, Some(position_ns));
    }
}

/// Commit the exact point at which queues/route cross to the requested timeline. Calling this
/// before a real flush/reload revokes every pre-seek observation; calling it after a PMS refusal
/// would be a lie, so refusal uses [`reject_user_seek`] instead.
pub fn commit_user_seek() -> bool {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if control.pending_seek_ns.take().is_none() {
        return false;
    }
    control.media_epoch = next_generation(control.media_epoch);
    let ticket = worker_ticket_of(&control);
    if let Some(automatic) = control.pending_auto.as_mut() {
        retarget_automatic_intent(automatic, ticket, None);
    }
    true
}

/// Settle a seek request whose external rebuild was refused before changing any bytes.
pub fn reject_user_seek() {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    control.pending_seek_ns = None;
}

pub fn cancel_user_route_intent(intent: UserRouteIntent) {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if control.pending_user == Some(intent) {
        control.pending_user = None;
        control.displaced_pick = false;
    }
}

/// Reserve one main-thread action. The mutex is released before PMS or native I/O; `serial`
/// prevents a completion from settling any later action by mistake.
pub fn claim_route_action() -> Option<ClaimedRouteAction> {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if control.phase != ControlPhase::Stable {
        return None;
    }
    let mut displaced_pick = false;
    let intent = if let Some(user) = control.pending_user.take() {
        displaced_pick = std::mem::take(&mut control.displaced_pick);
        RouteIntent::User(user)
    } else {
        let automatic = control.pending_auto.take()?;
        if !ticket_is_current(&control, automatic_ticket(&automatic)) {
            return None;
        }
        RouteIntent::Automatic(automatic)
    };
    control.next_action = next_generation(control.next_action);
    let serial = control.next_action;
    let ticket = worker_ticket_of(&control);
    control.phase = ControlPhase::Applying(serial);
    control.claimed_user = match intent {
        RouteIntent::User(user) => Some((serial, user, displaced_pick)),
        RouteIntent::Automatic(_) => None,
    };
    Some(ClaimedRouteAction {
        serial,
        ticket,
        intent,
        displaced_pick,
        claim_snapshot: None,
    })
}

/// Settle the PMS/projection half of a claimed action which did not enter the explicit Original
/// trial phase. A prepared candidate advances the applied contract but remains non-publishable in
/// `Prepared`; claiming a physical `Load` moves it to `Starting`, and only [`settle_route_start`]
/// may expose it as `Stable`. Refusal/cancellation restores the previous complete projection while
/// the phase still blocks workers, then publishes Stable.
///
/// Returns whether this call actually owned the transition. `false` means the phase had already
/// moved past `Applying(action.serial)` — a landing whose PMS half ran on a worker can arrive after
/// a teardown/new playback superseded it — and NEITHER `ps` nor `PlayerControl` was touched;
/// callers whose next step is a reload (`retranscode_tail`/`native_audio_tail`) must skip it rather
/// than reload onto an action nobody owns any more.
pub fn finish_route_action(ps: &mut PlaybackSession, action: &ClaimedRouteAction, result: RouteApplyResult) -> bool {
    // Phase ownership is checked FIRST, before anything reads `ps`: a superseded action's `ps` may
    // already belong to a different playback (see `take_ready_flight`'s own doc), and
    // `route_projection` must never run against it.
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if control.phase != ControlPhase::Applying(action.serial) {
        return false;
    }
    if result == RouteApplyResult::Prepared {
        // A claim whose PMS half ran on a worker carries the revision/quality/projection it was
        // BUILT from (finding: a second edit queued mid-flight must not get marked applied — see
        // `execute_retranscode_claim`'s capture of `ClaimSnapshot`). Every other claim settles
        // within the same frame it was claimed in, so nothing else could have advanced
        // `desired_revision`/`ps` in between and reading them live is exactly correct.
        let (revision, quality, projection) = match &action.claim_snapshot {
            Some(snapshot) => (snapshot.revision, snapshot.quality, snapshot.projection.clone()),
            None => (control.desired_revision, control.desired_quality, route_projection(ps)),
        };
        if matches!(action.intent, RouteIntent::User(_)) {
            control.applied_revision = revision;
            control.applied_quality = quality;
        }
        // Automatic actions retain the applied contract revision carried by their ticket.
        // Route identity has its own epoch; rebinding an automatic result to a later rejected
        // user desire would immediately revoke the worker which the automatic action created.
        control.applied_projection = Some(projection);
        control.phase = ControlPhase::Prepared(action.serial);
        return true;
    }
    let restore = control.applied_projection.clone();
    drop(control);
    if let Some(applied) = restore {
        install_route_projection(ps, &applied);
    }
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if control.phase == ControlPhase::Applying(action.serial) {
        control.phase = ControlPhase::Stable;
    }
    true
}

/// Return the pending route-start transaction while the reducer is between preparation and a
/// `Load` result. The exact physical attempt is minted only by [`claim_route_start_attempt`]; a dev
/// fixture which prepares its route inside `start_bufferfeed` is covered too.
pub fn pending_route_start() -> Option<RouteStartTransaction> {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    match control.phase {
        ControlPhase::Preparing(serial)
        | ControlPhase::Prepared(serial)
        | ControlPhase::Starting(serial, _) => Some(RouteStartTransaction { serial }),
        ControlPhase::OriginalTrial(OriginalTrialPhase::Preparing(serial))
        | ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial))
        | ControlPhase::OriginalTrial(OriginalTrialPhase::Starting(serial, _)) => {
            Some(RouteStartTransaction { serial })
        }
        _ => None,
    }
}

/// Return or reserve the semantic transaction for an Engine replacement which does not already
/// belong to a prepared plan/action. [`claim_route_start_attempt`] mints the exact physical attempt.
/// A failed-candidate retry gets a new transaction while retaining its already-prepared URL, so no
/// PMS decision is repeated merely because native construction failed synchronously.
pub fn begin_route_start() -> Option<RouteStartTransaction> {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    match control.phase {
        ControlPhase::Preparing(serial)
        | ControlPhase::Prepared(serial)
        | ControlPhase::Starting(serial, _) => Some(RouteStartTransaction { serial }),
        ControlPhase::OriginalTrial(OriginalTrialPhase::Preparing(serial))
        | ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial))
        | ControlPhase::OriginalTrial(OriginalTrialPhase::Starting(serial, _)) => {
            Some(RouteStartTransaction { serial })
        }
        ControlPhase::Stable => {
            control.next_action = next_generation(control.next_action);
            let serial = control.next_action;
            control.phase = ControlPhase::Preparing(serial);
            Some(RouteStartTransaction { serial })
        }
        // `Failed` and `Idle` are the two phases which own no live Engine, so both go straight to
        // `Prepared`: there is nothing left for `Preparing`'s "old route is still recoverable"
        // window to protect, and entering it would let an ordinary `NoRoute` abort publish
        // `Stable` around a decoder that no longer exists. `Idle` is the phase a COMPLETED stop
        // publishes ([`finish_engine_teardown`]), and it is how a dev fixture replays a stream
        // that ran to EOS — that entry asks for a start owner directly rather than through
        // [`begin_playback_request`].
        ControlPhase::Failed(_) | ControlPhase::Idle => {
            control.next_action = next_generation(control.next_action);
            let serial = control.next_action;
            control.phase = ControlPhase::Prepared(serial);
            Some(RouteStartTransaction { serial })
        }
        _ => None,
    }
}

/// Candidate preparation succeeded and the caller is about to destroy/replace the native Engine.
/// An ordinary post-teardown failure cannot truthfully restore the old Engine; an Original trial
/// is the explicit exception, retaining its HLS rollback projection.
pub fn prepare_route_start(ticket: RouteStartTransaction) -> bool {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    match control.phase {
        ControlPhase::Preparing(serial) if serial == ticket.serial => {
            control.phase = ControlPhase::Prepared(serial);
            true
        }
        // A recovery flight's landing hands the trial's own transaction back to the trial.
        ControlPhase::OriginalTrial(OriginalTrialPhase::Preparing(serial)) if serial == ticket.serial => {
            control.phase = ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial));
            true
        }
        ControlPhase::Prepared(serial) | ControlPhase::Starting(serial, _)
            if serial == ticket.serial =>
        {
            true
        }
        ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial))
        | ControlPhase::OriginalTrial(OriginalTrialPhase::Starting(serial, _))
            if serial == ticket.serial =>
        {
            true
        }
        _ => false,
    }
}

/// Mint the physical Load attempt only after candidate preparation and destructive teardown have
/// completed. A second attempt always receives a different id, even inside the same transaction.
pub fn claim_route_start_attempt(
    ticket: RouteStartTransaction,
) -> Option<RouteStartAttempt> {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    let original = match control.phase {
        ControlPhase::Prepared(serial) if serial == ticket.serial => false,
        ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial))
            if serial == ticket.serial =>
        {
            true
        }
        _ => return None,
    };
    control.next_start_attempt = control
        .next_start_attempt
        .checked_add(1)
        .expect("native Load attempt identity exhausted");
    let attempt = control.next_start_attempt;
    control.phase = if original {
        ControlPhase::OriginalTrial(OriginalTrialPhase::Starting(ticket.serial, attempt))
    } else {
        ControlPhase::Starting(ticket.serial, attempt)
    };
    Some(RouteStartAttempt {
        serial: ticket.serial,
        attempt,
    })
}

/// PMS/resume preparation failed before teardown. Only ordinary `Preparing` has a proven live
/// fallback; an ordinary rejection after `Prepared` is terminal. An Original rejection remains
/// `OriginalTrialPhase::Failed` and recoverable through [`rollback_original_recovery`].
pub fn reject_route_start_preparation(ticket: RouteStartTransaction) -> bool {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    control.phase = match control.phase {
        ControlPhase::Preparing(serial) if serial == ticket.serial => ControlPhase::Stable,
        ControlPhase::Prepared(serial) if serial == ticket.serial => ControlPhase::Failed(serial),
        ControlPhase::OriginalTrial(OriginalTrialPhase::Preparing(serial))
        | ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial))
            if serial == ticket.serial =>
        {
            ControlPhase::OriginalTrial(OriginalTrialPhase::Failed(serial))
        }
        _ => return false,
    };
    control.start_deferred = control
        .start_deferred
        .take()
        .filter(|(serial, _)| *serial != ticket.serial);
    true
}

/// Settle a transaction which never reached a callable native thread. This is distinct from the
/// media-thread result because no callback can race it; accepting either Prepared or Starting is
/// nevertheless useful for a thread-spawn refusal after an attempt id was minted.
pub fn abort_route_start(ticket: RouteStartTransaction, result: RouteStartResult) -> bool {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    let phase = match control.phase {
        ControlPhase::Preparing(serial) if serial == ticket.serial => {
            if result == RouteStartResult::NoRoute {
                ControlPhase::Stable
            } else {
                ControlPhase::Failed(serial)
            }
        }
        ControlPhase::Prepared(serial) | ControlPhase::Starting(serial, _)
            if serial == ticket.serial =>
        {
            ControlPhase::Failed(serial)
        }
        ControlPhase::OriginalTrial(OriginalTrialPhase::Preparing(serial))
        | ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial))
        | ControlPhase::OriginalTrial(OriginalTrialPhase::Starting(serial, _))
            if serial == ticket.serial =>
        {
            ControlPhase::OriginalTrial(OriginalTrialPhase::Failed(serial))
        }
        _ => return false,
    };
    control.start_deferred = control
        .start_deferred
        .take()
        .filter(|(serial, _)| *serial != ticket.serial);
    control.timeline = None;
    control.phase = phase;
    true
}

/// Publish the native half of one prepared route. An ordinary failed candidate deliberately
/// remains the applied projection in `Failed`: teardown may already have destroyed the old Engine
/// and PMS may already have retired its encoder, so restoring the old *description* would fabricate
/// a live route. An Original failure remains `OriginalTrialPhase::Failed` with the retained HLS
/// rollback projection until the explicit rollback edge.
pub fn settle_route_start(ps: &mut PlaybackSession, ticket: RouteStartAttempt, result: RouteStartResult) -> bool {
    let deferred = {
        let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        let ordinary = control.phase == ControlPhase::Starting(ticket.serial, ticket.attempt);
        let original = control.phase
            == ControlPhase::OriginalTrial(OriginalTrialPhase::Starting(
                ticket.serial,
                ticket.attempt,
            ));
        if !ordinary && !original {
            return false;
        }
        control.last_start_result = Some((ticket, result));
        match (ordinary, result) {
            (true, RouteStartResult::Started) => {
                let effects = control
                    .start_deferred
                    .take()
                    .filter(|(serial, _)| *serial == ticket.serial)
                    .map(|(_, effects)| effects);
                if effects.is_some() {
                    control.phase = ControlPhase::Completing(ticket.serial);
                } else {
                    control.phase = ControlPhase::Stable;
                }
                effects
            }
            (true, RouteStartResult::NoRoute | RouteStartResult::StartFailed) => {
                control.start_deferred = control
                    .start_deferred
                    .take()
                    .filter(|(serial, _)| *serial != ticket.serial);
                control.timeline = None;
                control.phase = ControlPhase::Failed(ticket.serial);
                None
            }
            (false, RouteStartResult::Started) => {
                control.phase =
                    ControlPhase::OriginalTrial(OriginalTrialPhase::AwaitingFrame(ticket.serial));
                None
            }
            (false, RouteStartResult::NoRoute | RouteStartResult::StartFailed) => {
                control.phase =
                    ControlPhase::OriginalTrial(OriginalTrialPhase::Failed(ticket.serial));
                None
            }
        }
    };
    if let Some(effects) = deferred {
        apply_deferred_original_effects(ps, effects);
        let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        if control.phase == ControlPhase::Completing(ticket.serial) {
            control.phase = ControlPhase::Stable;
        }
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteStartStatus {
    Pending,
    Started,
    Failed,
    /// A later physical Load replaced the observed attempt before foreground consumed its
    /// completion. Following this exact token keeps the app lifecycle attached to the Engine
    /// which now owns the screen instead of tearing it down as if the old result were a failure.
    Superseded(RouteStartAttempt),
    Stale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveEngineStartRelation {
    /// The caller rediscovered the Engine which already owns this exact in-flight Load.
    CurrentAttempt,
    /// No replacement transaction is waiting; the live Engine is an ordinary idempotent start.
    NoPendingRoute,
    /// A different prepared transaction requires a new Engine and cannot borrow this one.
    Conflict(RouteStartTransaction),
}

/// Classify a start request which found a live native Engine. Keeping this comparison inside the
/// reducer avoids an ABA-prone pair of `pending_route_start`/`route_start_status` observations:
/// both the semantic transaction and physical attempt are compared under one lock.
pub fn classify_live_engine_start(existing: RouteStartAttempt) -> LiveEngineStartRelation {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    match control.phase {
        ControlPhase::Starting(serial, attempt)
            if serial == existing.serial && attempt == existing.attempt =>
        {
            LiveEngineStartRelation::CurrentAttempt
        }
        ControlPhase::OriginalTrial(OriginalTrialPhase::Starting(serial, attempt))
            if serial == existing.serial && attempt == existing.attempt =>
        {
            LiveEngineStartRelation::CurrentAttempt
        }
        ControlPhase::Preparing(serial)
        | ControlPhase::Prepared(serial)
        | ControlPhase::Starting(serial, _) => {
            LiveEngineStartRelation::Conflict(RouteStartTransaction { serial })
        }
        ControlPhase::OriginalTrial(OriginalTrialPhase::Preparing(serial))
        | ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial))
        | ControlPhase::OriginalTrial(OriginalTrialPhase::Starting(serial, _)) => {
            LiveEngineStartRelation::Conflict(RouteStartTransaction { serial })
        }
        _ => LiveEngineStartRelation::NoPendingRoute,
    }
}

/// Observe one exact physical start without consuming another subsystem's result. There can only
/// be one live start owner; retaining the last completion is sufficient for the foreground
/// reducer to bridge the media-thread return into its next main-loop tick.
pub fn route_start_status(ticket: RouteStartAttempt) -> RouteStartStatus {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    let pending = match control.phase {
        ControlPhase::Starting(serial, attempt)
        | ControlPhase::OriginalTrial(OriginalTrialPhase::Starting(serial, attempt)) => {
            Some(RouteStartAttempt { serial, attempt })
        }
        _ => None,
    };
    if let Some(current) = pending {
        if current == ticket {
            return RouteStartStatus::Pending;
        }
        return if current.attempt > ticket.attempt {
            RouteStartStatus::Superseded(current)
        } else {
            RouteStartStatus::Stale
        };
    }

    // A terminal result is observable only while the reducer phase still says that exact Load
    // owns the physical route. `last_start_result` is diagnostic history after teardown; allowing
    // it to start the foreground clock from Prepared/Stopping would resurrect a destroyed Engine.
    let completed = match (control.phase, control.last_start_result) {
        (
            ControlPhase::Stable | ControlPhase::Completing(_),
            Some((attempt, RouteStartResult::Started)),
        ) => Some((attempt, RouteStartStatus::Started)),
        (
            ControlPhase::Failed(serial),
            Some((attempt, RouteStartResult::NoRoute | RouteStartResult::StartFailed)),
        ) if attempt.serial == serial => Some((attempt, RouteStartStatus::Failed)),
        (
            ControlPhase::OriginalTrial(OriginalTrialPhase::AwaitingFrame(serial)),
            Some((attempt, RouteStartResult::Started)),
        ) if attempt.serial == serial => Some((attempt, RouteStartStatus::Started)),
        (
            ControlPhase::OriginalTrial(OriginalTrialPhase::Failed(serial)),
            Some((attempt, RouteStartResult::NoRoute | RouteStartResult::StartFailed)),
        ) if attempt.serial == serial => Some((attempt, RouteStartStatus::Failed)),
        _ => None,
    };
    match completed {
        Some((attempt, status)) if attempt == ticket => status,
        Some((attempt, _)) if attempt.attempt > ticket.attempt => {
            RouteStartStatus::Superseded(attempt)
        }
        _ => RouteStartStatus::Stale,
    }
}

/// Media-thread half of the start handshake. Queue rather than settling here: `sf_load` can return
/// before the spawning main thread has installed its Engine, and Stable must never lead that slot.
pub fn publish_route_start_result(ticket: RouteStartAttempt, result: RouteStartResult) {
    PLAYER_CONTROL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .start_results
        .push((ticket, result));
}

/// Main-thread publication point for every completed native Load call. Late/stale results are
/// intentionally drained too; [`settle_route_start`] rejects their exact serial.
pub fn drain_route_start_results(ps: &mut PlaybackSession) {
    let results = {
        let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut control.start_results)
    };
    for (ticket, result) in results {
        let _ = settle_route_start(ps, ticket, result);
    }
}

/// A route which had reached Stable later proved terminal (demux/HTTP/native callback failure).
/// Revoke the Engine/media tickets and expose Failed as one reducer edge rather than changing only
/// the UI playback enum while automatic workers still believe the route is publishable.
pub fn fail_current_engine() {
    // Whether a flight was outstanding (`flight_outstanding`'s own condition, matched here
    // directly because the reducer already holds `PLAYER_CONTROL`'s lock).
    let mut flight_ended = false;
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    let serial = match control.phase {
        // A seek's flight owns this transaction and its worker may still be running: like a
        // claim's, its late landing is discarded (the phase no longer matches), and the hold goes.
        ControlPhase::Preparing(serial) => {
            flight_ended = control.flight == Some(serial);
            serial
        }
        ControlPhase::Prepared(serial) | ControlPhase::Starting(serial, _) => serial,
        ControlPhase::Stable | ControlPhase::Completing(_) => {
            control.next_action = next_generation(control.next_action);
            control.next_action
        }
        ControlPhase::StagingUser(serial) => serial,
        ControlPhase::OriginalTrial(trial) => match trial {
            // The trial's own transaction is a flight's: like a seek's, its late landing is
            // discarded (the phase no longer matches) and the hold goes.
            OriginalTrialPhase::Preparing(serial) => {
                flight_ended = control.flight == Some(serial);
                serial
            }
            OriginalTrialPhase::Prepared(serial)
            | OriginalTrialPhase::Starting(serial, _)
            | OriginalTrialPhase::AwaitingFrame(serial)
            | OriginalTrialPhase::Failed(serial) => serial,
        },
        ControlPhase::Failed(_) | ControlPhase::Idle | ControlPhase::Stopping => return,
        ControlPhase::Resolving => return,
        // A claim's worker owns this phase, and the Engine it was going to reload just died. The
        // failure is published NOW under the claim's own serial: the pump returns on a terminal
        // failure before it ever drains a landing, so a failure parked behind `Applying` would
        // leave the phase (and the claim hold's spinner) in flight forever instead of reaching
        // the error read-out. The claim's late landing is safe: `take_ready_flight`
        // discards any landing whose serial is no longer `Applying`, stopping the encoder it
        // started, and the epoch bump below refuses the worker's own commit if it has not made
        // one yet.
        ControlPhase::Applying(serial) => {
            flight_ended = true;
            serial
        }
    };
    control.engine_epoch = next_generation(control.engine_epoch);
    control.media_epoch = next_generation(control.media_epoch);
    control.pending_auto = None;
    control.start_deferred = None;
    control.timeline = None;
    control.clear_flight();
    control.phase = ControlPhase::Failed(serial);
    drop(control);
    if flight_ended {
        // The claim's presentation hold (`player::claim_hold`) has nothing left to give back to.
        crate::player::claim_hold::clear();
    }
}

/// Invalidate the workers belonging to the Engine being torn down. Pending explicit contracts are
/// retained; a reload can therefore never consume a quality/track request merely by resetting the
/// atomics that used to carry it.
pub fn begin_engine_teardown(for_reload: bool) {
    if !for_reload {
        // A full stop (not an in-place reload) ends the item this claim was for. Any landing
        // still sitting in the mailbox after this point belongs to whatever plays next, not to
        // this one — see `discard_retranscode_claim_slot`'s doc.
        discard_retranscode_claim_slot();
    }
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    control.engine_epoch = next_generation(control.engine_epoch);
    control.media_epoch = next_generation(control.media_epoch);
    control.pending_auto = None;
    // A seek's flight does not survive the Engine it was going to replace. The route it was
    // preparing is still the live one (nothing was committed), so the transaction goes back to
    // `Stable`: an app-switch's foreground restore reserves its own start transaction next, and a
    // `Preparing` left behind would refuse it. The landing, if the worker has not posted it yet,
    // meets a phase that no longer matches and is discarded.
    let mut dropped_start_flight = false;
    let mut dropped_claim_flight = false;
    if !for_reload {
        control.desired_revision = next_generation(control.desired_revision);
        control.pending_user = None;
        control.displaced_pick = false;
        control.pending_seek_ns = None;
        control.phase = ControlPhase::Stopping;
        control.clear_flight();
        control.timeline = None;
        } else {
        let current = control.phase;
        control.phase = match current {
            ControlPhase::Preparing(serial) if control.flight == Some(serial) => {
                control.clear_flight();
                dropped_start_flight = true;
                ControlPhase::Stable
            }
            // A CLAIM's flight does not survive the Engine it was going to reload either. Left in
            // `Applying`, the foreground restore's rebuild would be refused (`flight_phase_open`)
            // and nothing would drain the landing (no Engine, no pump), so the viewer would sit on
            // the spinner until Back. It ends the way `fail_current_engine`'s `Applying` arm ends
            // one: the phase settles (to `Stable`, the physical route being whatever the foreground
            // restore rebuilds), the landing is discarded below (a worker that has not posted yet
            // meets a flight that is no longer current) and the hold is released. What the claim
            // owed the viewer is not dropped with it: the intent goes back to `pending_user`, and
            // the restored Engine claims it once it is `Stable`.
            ControlPhase::Applying(serial) if control.flight == Some(serial) => {
                control.clear_flight();
                dropped_claim_flight = true;
                if let Some((claimed, intent, displaced)) = control.claimed_user.take() {
                    if claimed == serial {
                        control.pending_user = Some(match control.pending_user.take() {
                            Some(newer) => merge_user_route_intent(Some(intent), newer, false),
                            None => intent,
                        });
                        control.displaced_pick |= displaced;
                    }
                }
                ControlPhase::Stable
            }
            ControlPhase::Starting(serial, _) => {
                // The physical attempt being torn down can still publish from its media thread.
                // Return the transaction to Prepared so a retry mints a fresh attempt; the old
                // result no longer matches even when both attempts open the same candidate URL.
                ControlPhase::Prepared(serial)
            }
            ControlPhase::OriginalTrial(OriginalTrialPhase::Starting(serial, _)) => {
                ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial))
            }
            // A recovery flight inside the trial does not survive the Engine it was going to
            // replace either, and what it hands back is the TRIAL's transaction, not `Stable`: the
            // snapshot (`pending_original`) was never touched, and the foreground restore reserves
            // the same `Prepared` transaction again.
            ControlPhase::OriginalTrial(OriginalTrialPhase::Preparing(serial)) => {
                control.clear_flight();
                dropped_start_flight = true;
                ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial))
            }
            ControlPhase::OriginalTrial(OriginalTrialPhase::AwaitingFrame(serial)) => {
                // Frame proof belongs to the Engine being destroyed. The Original candidate and
                // rollback snapshot remain valid, but a replacement Load must prove presentation
                // again before either resource may be committed or retired.
                ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial))
            }
            phase => phase,
        };
    }
    drop(control);
    if dropped_start_flight || dropped_claim_flight {
        discard_retranscode_claim_slot();
        crate::player::claim_hold::clear();
    }
}

/// The reducer's phase, for a log line. A refused start owner names the phase it was refused
/// from, because "refused" alone cannot be diagnosed from a device log.
pub fn control_phase_label() -> String {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    format!("{:?}", control.phase)
}

/// Whether a flight may currently be running on a worker: the primitive under
/// [`flight_outstanding`] (`flight.rs`), which is the name every gate outside this file uses. A
/// claim's flight holds `Applying`; a seek's holds `Preparing` AND names itself in `flight` (a
/// `Preparing` an inline caller opened is a preparation one frame completes, not a flight). The
/// pump's own seek branches (the stuck in-place retry/reload and the plain pending-seek reducer)
/// must skip while this holds and leave `TX.seek_to_ns` exactly where it is: neither branch checks
/// `ControlPhase`, and either one planning a rebuild or calling `commit_user_seek` against an
/// action it does not own corrupts or silently rejects that very flight (see the pump's own doc on
/// the call sites that consult this).
pub(super) fn flight_phase_open() -> bool {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    match control.phase {
        ControlPhase::Applying(_) => true,
        phase => phase.preparing_serial().is_some_and(|serial| control.flight == Some(serial)),
    }
}

/// Whether the reducer is still in the flight phase for exactly this serial — the "owns" check a
/// worker landing must pass before it may touch `PlaybackSession`, and the player's presentation
/// hold's test that its flight is still flying. False after a teardown, a fresh playback request
/// or the flight's own settlement: a landing for a superseded serial (or a later flight already
/// out) must be dropped rather than applied; see `take_ready_flight`. The primitive under
/// [`flight_is_current`] (`flight.rs`), the name every holder outside this file uses.
pub(super) fn flight_phase_is(serial: u64) -> bool {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    match control.phase {
        ControlPhase::Applying(s) => s == serial,
        phase => phase.preparing_serial() == Some(serial) && control.flight == Some(serial),
    }
}

/// Whether the flight that is outstanding is a recovery's ([`begin_recovery_flight`]): the pump's
/// failure branches wait on it instead of failing the Engine it will replace. A cold resume's flight
/// ([`begin_resume_flight`]) is one too by reducer phase, but it has no Engine, so no pump asks.
pub(super) fn recovery_flight_open() -> bool {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    control.phase.preparing_serial().is_some_and(|serial| {
        control.flight == Some(serial) && control.flight_recovery == Some(serial)
    })
}

/// Whether the flight that is outstanding is a COLD RESUME's ([`begin_resume_flight`]): the app
/// loop's drain waits on it, and the HUD reads `Resolving` through it.
pub(super) fn resume_flight_open() -> bool {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    control.phase.preparing_serial().is_some_and(|serial| {
        control.flight == Some(serial) && control.flight_resume == Some(serial)
    })
}

/// Whether the flight `serial` is a cold resume's. Read BEFORE [`end_flight`] clears the marker, by
/// the drain that has to settle a refusal differently ([`release_rebase_start`]).
pub(super) fn resume_flight_is(serial: u64) -> bool {
    PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).flight_resume == Some(serial)
}

/// [`begin_recovery_flight`] for a cold resume: a worker is about to rebuild a resolved transcode
/// at the saved position, before any Engine has been started.
pub(super) fn begin_resume_flight(serial: u64, offset_ns: i64) {
    begin_engineless_flight(serial, offset_ns);
    PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).flight_resume = Some(serial);
}

/// [`begin_recovery_flight`] for a recovery no Engine waits on (see `flight_engineless`).
pub(super) fn begin_engineless_flight(serial: u64, offset_ns: i64) {
    begin_recovery_flight(serial, offset_ns);
    PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).flight_engineless = Some(serial);
}

/// Whether the outstanding flight is a recovery with NO Engine ([`begin_engineless_flight`]): a
/// cold or foreground resume's rebuild, or the rollback of a trial whose Load never produced an
/// Engine. The HUD reads `Resolving` through it, an app-switch suspend drops it, and the app loop
/// (not a pump) drains it.
pub(super) fn engineless_flight_open() -> bool {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    control.phase.preparing_serial().is_some_and(|serial| {
        control.flight == Some(serial) && control.flight_engineless == Some(serial)
    })
}

/// [`begin_flight`] for a recovery: a worker is about to run the PMS half of the replacement route
/// a failed start is owed.
pub(super) fn begin_recovery_flight(serial: u64, offset_ns: i64) {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    control.flight = Some(serial);
    control.flight_recovery = Some(serial);
    control.flight_offset_ns = Some(offset_ns);
}

/// The position the outstanding RECOVERY flight rebuilds the route at, while it is out; `None` with
/// no such flight. A cold or foreground resume's flight has no Engine and no landing yet, so the
/// published playhead is still zero: this is what the app snapshots when the OS takes the screen
/// mid-flight (`player::intended_pos_ns`), so the foreground restore resumes where the viewer was
/// rather than from the start.
pub(super) fn recovery_flight_offset() -> Option<i64> {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    let serial = control.phase.preparing_serial()?;
    (control.flight == Some(serial) && control.flight_recovery == Some(serial)).then_some(control.flight_offset_ns)?
}

/// A worker is about to run the PMS half of the action/transaction `serial`.
pub(super) fn begin_flight(serial: u64) {
    PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).flight = Some(serial);
}

/// The flight `serial` has landed (or was discarded): forget it. A different flight's record is
/// left alone.
pub(super) fn end_flight(serial: u64) {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if control.flight == Some(serial) {
        control.clear_flight();
    }
}

/// The synchronous main-thread teardown announced by `begin_engine_teardown(false)` has RETURNED:
/// every worker is joined, the native object is retired and the URL is cleared. `Stopping` is that
/// teardown's fence and nothing more — it exists so a worker which was still running when the stop
/// began cannot publish into the route it is destroying — so leaving it latched afterwards
/// describes a teardown which never finishes.
///
/// It cost LG App Self Checklist #46: a replayed `plxnative-playurl` stream asks
/// [`begin_route_start`] for an owner directly (no [`begin_playback_request`] resolve in front of
/// it, because there is no library item behind the fixture), and a latched `Stopping` refused it —
/// `start_bufferfeed: route reducer refused a start owner`, one Load where the case needs two.
///
/// `Idle` rather than `Stable`: a completed stop owns no publishable route, so automatic
/// publication and [`claim_route_action`] must stay closed. Only `Stopping` moves; a phase already
/// replaced by a newer request is left exactly as that request left it.
pub fn finish_engine_teardown() {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if control.phase == ControlPhase::Stopping {
        control.phase = ControlPhase::Idle;
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn pending_user_route_intent(intent: UserRouteIntent) -> bool {
    PLAYER_CONTROL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pending_user
        == Some(intent)
}

/// Test-only: put the reducer in `Applying(serial)` without a real claim, for a test that owns a
/// presentation hold (`player::claim_hold`) and needs its serial to name a claim still flying.
#[cfg(any(test, feature = "test-support"))]
pub fn force_applying_for_test(serial: u64) {
    PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).phase = ControlPhase::Applying(serial);
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_player_control_for_test(ps: &PlaybackSession) {
    let projection = route_projection(ps);
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    control.engine_epoch = next_generation(control.engine_epoch);
    control.media_epoch = next_generation(control.media_epoch);
    control.desired_revision = next_generation(control.desired_revision);
    control.desired_quality = quality();
    control.applied_revision = control.desired_revision;
    control.applied_quality = control.desired_quality;
    control.applied_projection = Some(projection);
    advance_route(&mut control.active);
    control.active.id.clear();
    control.active.hls = None;
    control.next_action = 0;
    // Physical attempts are process-monotonic. The result queue outlives an Engine reset, so
    // reusing an id here would make a late completion an ABA match for the next fixture/session.
    control.pending_user = None;
    control.claimed_user = None;
    control.displaced_pick = false;
    control.pending_auto = None;
    control.pending_seek_ns = None;
    control.phase = ControlPhase::Stable;
    control.clear_flight();
    control.resolve_fallback = None;
    control.start_results.clear();
    control.last_start_result = None;
    control.start_deferred = None;
    control.pending_original = None;
    control.timeline = None;
}

/// **Candidate encoder names are allocated HERE, process-globally, and that is the whole point.**
///
/// The name is `<logical_session>-abr-<n>`, and the two halves used to have different lifetimes:
/// `logical_session` is `sess()`, which SURVIVES a seek. Before the seek path learned to allocate a
/// replacement, it also REUSED the live physical id; meanwhile `n` was a `u64` local to the demux
/// worker, reset to 0 by every `Load`. So a playback that committed one switch and was then
/// scrubbed came back with `ACTIVE_ENCODER = <sess>-abr-1` and a counter at zero, and the next
/// transaction primed a candidate named `<sess>-abr-1` — **the live session's own id**.
///
/// Both exits then kill the playback, which is why it presents as a server fault rather than as a
/// client bug. On rollback, `abandon(candidate)` is `transcode_stop` on the live encoder. On
/// commit, `replace_active_encoder(expected, candidate)` trivially succeeds when the two are
/// equal, and the caller's `retire(previous)` stops the session it just switched to. The symptom
/// is a run of 404s and `HLS segment was not produced in time`, because 404 is `NotReady` by
/// design and the retry loop cannot tell a stopped session from a slow one.
///
/// A monotonic global makes the collision unrepresentable rather than merely unlikely, so `prime`
/// takes no generation from its caller: there is no value a worker could pass that repeats one.
pub(super) static ENCODER_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One exact PMS transcode cleanup observation in flight. `stop_needed` is true only until one
/// stop request was accepted; after that, completed HLS segments drive exact state checks. PMS
/// 1.43.4 owns two independently-lived objects: `session=` names the physical encoder, while
/// `X-Plex-Session-Identifier` names the Streaming Resource charged against the bandwidth
/// governor. A physical ping=404 proves only the first half. Once it is absent we synchronously
/// close (or observe 404 for) the second half before releasing this record.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EncoderCleanupCheck {
    sid: ServerId,
    session: String,
    stop_needed: bool,
    physical_absent: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct PendingEncoderCleanup {
    sid: ServerId,
    session: String,
    checking: bool,
    stop_needed: bool,
    physical_absent: bool,
}

/// Process-wide because a seek/reload replaces [`HlsAbrControl`] while the server cleanup worker
/// outlives it. Entries are scoped by server: an unreachable shared PMS must not suppress quality
/// experiments against a different machine.
#[derive(Default)]
struct EncoderCleanupLedger {
    pending: Vec<PendingEncoderCleanup>,
}

impl EncoderCleanupLedger {
    fn remember(&mut self, sid: ServerId, session: &str) -> bool {
        self.remember_with_state(sid, session, true, false)
    }

    fn remember_with_state(
        &mut self,
        sid: ServerId,
        session: &str,
        stop_needed: bool,
        physical_absent: bool,
    ) -> bool {
        if self
            .pending
            .iter()
            .any(|entry| entry.sid == sid && entry.session == session)
        {
            return false;
        }
        self.pending.push(PendingEncoderCleanup {
            sid,
            session: session.to_owned(),
            checking: false,
            stop_needed,
            physical_absent,
        });
        true
    }

    fn is_clear(&self, sid: ServerId) -> bool {
        !self.pending.iter().any(|entry| entry.sid == sid)
    }

    /// Claim every unchecked entry for this PMS. A second caller sees `checking=true` and starts
    /// no duplicate stop or ping while the first network request is in flight.
    fn take_unchecked(&mut self, sid: ServerId) -> Vec<EncoderCleanupCheck> {
        self.pending
            .iter_mut()
            .filter(|entry| entry.sid == sid && !entry.checking)
            .map(|entry| {
                entry.checking = true;
                EncoderCleanupCheck {
                    sid: entry.sid,
                    session: entry.session.clone(),
                    stop_needed: entry.stop_needed,
                    physical_absent: entry.physical_absent,
                }
            })
            .collect()
    }

    /// Publish one completed server observation. Only physical absence plus exact logical
    /// reconciliation removes an entry. A stop which failed to receive a 2xx is retried when
    /// later completed media asks for another check; an accepted stop is never re-enqueued merely
    /// because its asynchronous worker still exists.
    fn finish(
        &mut self,
        check: EncoderCleanupCheck,
        present: Option<bool>,
        stop_accepted: Option<bool>,
        resource_reconciled: Option<bool>,
    ) {
        let Some(index) = self
            .pending
            .iter()
            .position(|entry| entry.sid == check.sid && entry.session == check.session)
        else {
            return;
        };
        let entry = &mut self.pending[index];
        entry.checking = false;
        match stop_accepted {
            Some(true) => entry.stop_needed = false,
            Some(false) => entry.stop_needed = true,
            None => {}
        }
        if present == Some(false) {
            entry.physical_absent = true;
            // A transport-lost stop response can race a worker which did land. Once ping exact-
            // looks up the physical key as absent, retrying that stop cannot add information.
            entry.stop_needed = false;
        }
        if entry.physical_absent && resource_reconciled == Some(true) {
            self.pending.swap_remove(index);
        }
    }
}

static ENCODER_CLEANUP: Mutex<EncoderCleanupLedger> = Mutex::new(EncoderCleanupLedger {
    pending: Vec::new(),
});

fn finish_encoder_cleanup_check(
    check: EncoderCleanupCheck,
    present: Option<bool>,
    stop_accepted: Option<bool>,
    resource_reconciled: Option<bool>,
) {
    let physical_absent = check.physical_absent || present == Some(false);
    ENCODER_CLEANUP
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .finish(check, present, stop_accepted, resource_reconciled);
    match (physical_absent, resource_reconciled, present, stop_accepted) {
        (true, Some(true), _, _) => crate::player::log(
            "abr: PMS encoder cleanup reconciled; Streaming Resource is released",
        ),
        (true, None, _, _) => crate::player::log(
            "abr: PMS physical encoder is gone but resource close was inconclusive; retaining cleanup ownership",
        ),
        (_, _, _, Some(false)) => crate::player::log(
            "abr: PMS encoder stop was not accepted; retaining cleanup ownership",
        ),
        (_, _, None, _) => crate::player::log(
            "abr: PMS encoder cleanup ping was inconclusive; retaining cleanup ownership",
        ),
        _ => {}
    }
}

fn run_encoder_cleanup_check(check: EncoderCleanupCheck) {
    let Some(client) = plx_plex::plex::client_for(check.sid) else {
        let stop_accepted = check.stop_needed.then_some(false);
        finish_encoder_cleanup_check(check, None, stop_accepted, None);
        return;
    };
    let stop_accepted = (check.stop_needed && !check.physical_absent)
        .then(|| client.transcode_stop(&check.session));
    let present = if check.physical_absent {
        Some(false)
    } else {
        client.transcode_session_present(&check.session)
    };
    let physical_absent = check.physical_absent || present == Some(false);
    let resource_reconciled = if physical_absent {
        client.transcode_resource_reconciled(&check.session)
    } else {
        None
    };
    finish_encoder_cleanup_check(check, present, stop_accepted, resource_reconciled);
}

/// Start every currently unowned cleanup observation for `sid`. There is no sleep, attempt count
/// or wall-clock release: an accepted stop is checked once after each later completed HLS segment,
/// and only physical 404 followed by a successful/idempotent logical close removes it. Network
/// work stays off the demux thread; if the OS refuses the tiny worker, the already-degraded
/// fallback performs the same finite check inline.
fn drive_encoder_cleanup(sid: ServerId) -> bool {
    let checks = ENCODER_CLEANUP
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take_unchecked(sid);
    for check in checks {
        plx_base::task::spawn_small_or_inline(
            "abr-cleanup",
            const { &plx_base::task::BlockingLabel::new("encoder cleanup check (worker thread refused)") },
            move || run_encoder_cleanup_check(check),
        );
    }
    ENCODER_CLEANUP
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_clear(sid)
}

fn request_encoder_cleanup(sid: ServerId, session: &str) {
    if session.is_empty() {
        return;
    }
    let inserted = ENCODER_CLEANUP
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remember(sid, session);
    if inserted {
        crate::player::log("abr: queued exact PMS encoder cleanup");
    }
    let _ = drive_encoder_cleanup(sid);
}

fn next_encoder_session(logical_session: &str) -> String {
    format!("{logical_session}-abr-{}", next_encoder_generation())
}

fn install_active_encoder(value: &str) {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    let active = &mut control.active;
    advance_route(active);
    active.id = value.to_owned();
    active.hls = None;
}

fn install_active_hls(value: &str, url: &str, rung: crate::abr::Rung) {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    let active = &mut control.active;
    advance_route(active);
    active.id = value.to_owned();
    active.hls = Some(ActiveHlsRoute {
        url: url.to_owned(),
        rung,
        observed: None,
    });
}

fn take_active_encoder() -> String {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    let active = &mut control.active;
    advance_route(active);
    active.hls = None;
    std::mem::take(&mut active.id)
}

// Dev-only: used only by the `#[cfg(feature = "devtriggers")]` tests in this module's `tests`
// submodule below (see the comment on the first one).
#[cfg(all(test, feature = "devtriggers"))]
fn replace_active_encoder(expected: &str, replacement: &str) -> bool {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    let active = &mut control.active;
    if active.id != expected {
        return false;
    }
    advance_route(active);
    active.id = replacement.to_owned();
    active.hls = None;
    true
}

/// Publish a main-thread route replacement only while the complete action/worker generation is
/// still current. Unlike the legacy string helper this rejects same-id ABA, a direct seek epoch,
/// a new Engine and a user contract which superseded an in-flight PMS request.
fn replace_active_encoder_for(expected: &WorkerTicket, replacement: &str) -> Option<WorkerTicket> {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if !ticket_is_current(&control, expected) {
        return None;
    }
    {
        let active = &mut control.active;
        advance_route(active);
        active.id = replacement.to_owned();
        active.hls = None;
    }
    Some(worker_ticket_of(&control))
}

fn replace_active_hls_for(
    expected: &WorkerTicket,
    replacement: &str,
    url: &str,
    rung: crate::abr::Rung,
    observed: Option<(crate::abr::ObservedHlsVariant, u32)>,
) -> Option<WorkerTicket> {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if !ticket_is_current(&control, expected) {
        return None;
    }
    {
        let active = &mut control.active;
        advance_route(active);
        active.id = replacement.to_owned();
        active.hls = Some(ActiveHlsRoute {
            url: url.to_owned(),
            rung,
            observed,
        });
    }
    Some(worker_ticket_of(&control))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActiveHlsCommitRefusal {
    RouteMoved,
    TransitionRejected,
}

/// The process route no longer belongs to the worker which tried to publish a bounded local
/// transition. Kept separate from [`HlsCommitRefusal`]: this door changes no HLS route and has no
/// controller-rejection or server-session arm.
// Dev-only: used only by the `#[cfg(feature = "devtriggers")]` tests in this module's `tests`
// submodule below (see the comment on the first one).
#[cfg(all(any(test, feature = "test-support"), feature = "devtriggers"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveEncoderRefusal {
    RouteMoved,
}

/// Run one bounded publication while ACTIVE still names `expected`.
///
/// Production enters this under the AU queue mutex, fixing the global order at AQ -> ACTIVE. The
/// callback executes before ACTIVE is released; a check which returned `bool` and published later
/// would reopen a gap for seek/retranscode to retire this worker between those two operations.
// Dev-only: used only by the `#[cfg(feature = "devtriggers")]` tests in this module's `tests`
// submodule below (see the comment on the first one).
#[cfg(all(test, feature = "devtriggers"))]
fn with_active_route<T>(
    expected: &RouteLease,
    publication: impl FnOnce() -> T,
) -> Result<T, ActiveEncoderRefusal> {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    let active = &control.active;
    if active.epoch != expected.epoch || active.id != expected.encoder {
        return Err(ActiveEncoderRefusal::RouteMoved);
    }
    Ok(publication())
}

/// Change the process route only if the caller's local transition succeeds while the ACTIVE lock
/// still proves the expected encoder. Production invokes this under the AU queue's abort mutex,
/// giving the fixed order AQ -> ACTIVE -> controller/local state. The closure must be bounded and
/// perform no I/O; `None` leaves every route field untouched.
fn replace_active_hls_with<T>(
    expected: &WorkerTicket,
    replacement: &str,
    url: &str,
    rung: crate::abr::Rung,
    observed: Option<(crate::abr::ObservedHlsVariant, u32)>,
    transition: impl FnOnce() -> Option<T>,
) -> Result<(T, WorkerTicket), ActiveHlsCommitRefusal> {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if control.phase != ControlPhase::Stable || !ticket_is_current(&control, expected) {
        return Err(ActiveHlsCommitRefusal::RouteMoved);
    }
    let value = transition().ok_or(ActiveHlsCommitRefusal::TransitionRejected)?;
    {
        let active = &mut control.active;
        advance_route(active);
        active.id = replacement.to_owned();
        active.hls = Some(ActiveHlsRoute {
            url: url.to_owned(),
            rung,
            observed,
        });
    }
    let ticket = worker_ticket_of(&control);
    Ok((value, ticket))
}

/// Publish the response facts discovered by the demux worker without changing encoder identity.
/// A concurrent replacement wins; an observation from the retired worker is then simply stale.
fn observe_active_hls(
    expected: &WorkerTicket,
    variant: crate::abr::ObservedHlsVariant,
    evidence_kbps: u32,
) {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if ticket_is_current(&control, expected) {
        let active = &mut control.active;
        if let Some(hls) = active.hls.as_mut() {
            if hls.observed.map(|(observed, _)| observed) != Some(variant) {
                hls.observed = Some((variant, evidence_kbps));
            }
        }
    }
}

#[cfg(test)]
fn active_encoder() -> String {
    PLAYER_CONTROL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .active
        .id
        .clone()
}

// Dev-only: used only by the `#[cfg(feature = "devtriggers")]` tests in this module's `tests`
// submodule below (see the comment on the first one).
#[cfg(all(test, feature = "devtriggers"))]
fn active_route_lease() -> RouteLease {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    lease_of(&control.active)
}

fn active_hls() -> Option<(WorkerTicket, ActiveHlsRoute)> {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    control
        .active
        .hls
        .clone()
        .map(|hls| (worker_ticket_of(&control), hls))
}

/// Reconcile the worker-owned adaptive projection into the main-thread session immediately before
/// an operation that rebuilds or snapshots the route.  Ordinary playback never needs this copy;
/// seek, manual Original and track/quality reloads do, because they construct a new URL from the
/// stream that is live NOW rather than from the bootstrap stream that created the worker.
fn sync_active_hls_to_session(ps: &mut PlaybackSession) -> Option<(WorkerTicket, ActiveHlsRoute)> {
    // Capture the physical HLS commit and advance only those same physical fields in the applied
    // projection while holding the route mutex.  A user may already have staged a different
    // audio/subtitle/quality contract in Session; cloning Session wholesale here would falsely
    // bless that proposal merely because an older HLS worker changed rung underneath it.
    let active = {
        let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        let hls = control.active.hls.clone()?;
        let ticket = worker_ticket_of(&control);
        let mut applied = control
            .applied_projection
            .clone()
            .unwrap_or_else(|| route_projection(ps));
        applied.url = hls.url.clone();
        applied.tsession = ticket.encoder().to_owned();
        applied.contract.ceiling = Some(hls.rung.ceiling());
        applied.contract.remux = false;
        control.applied_projection = Some(applied.clone());
        (ticket, hls)
    };
    { let s = &mut *ps; {
        s.url = active.1.url.clone();
        s.tsession = active.0.encoder().to_owned();
        // An adaptive commit changes the encoder, URL and requested rung, not the delivery
        // contract. Preserve the route's negotiated segment duration instead of fabricating one
        // here: seek/reload must carry the exact server contract that created this worker.
        s.cur_contract.ceiling = Some(active.1.rung.ceiling());
        s.cur_contract.remux = false;
    } };
    // The applied clone retained above intentionally excludes Session's staged user fields:
    // rejection combines the newest physical HLS route with the last accepted track contract.
    Some(active)
}

pub(super) fn is_worker_ticket_current(expected: &WorkerTicket) -> bool {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    ticket_is_current(&control, expected)
}

/// Owned, worker-safe inputs for HLS replacement sessions. Constructed on the main thread before
/// the demux worker starts; it never reads the mutable route session afterwards.
#[derive(Clone)]
pub struct HlsAbrControl {
    trace_generation: u32,
    sid: ServerId,
    rating_key: String,
    logical_session: String,
    audio_stream_id: i64,
    subtitle_stream_id: i64,
    seconds_per_segment: u8,
    pub initial_rung: crate::abr::Rung,
    /// Response facts carried with the active physical route across seek/reload. They seed the
    /// worker's response state but never alter the request actuator above.
    pub initial_observed: Option<(crate::abr::ObservedHlsVariant, u32)>,
    fixture_base: String,
    /// Raw Part key in production; a complete URL only for the no-PMS fixture.  Runtime source
    /// measurements bind this Part to the exact active HLS resource instead of minting an AdHoc
    /// identity: PMS's token fallback makes a supposedly separate identity non-owning anyway.
    original_probe_part: String,
    original_source_kbps: u32,
    /// This playback's actuator set, with the device's decode bound and the source raster already
    /// applied — so the worker cannot propose a rendition that could never decode or that would
    /// only make PMS upscale.
    pub catalog: crate::abr::HlsActuatorCatalog,
    /// The startup probe as a weak prior for the live estimator, when there was one.
    pub prior: Option<crate::abr::CapacityEstimate>,
    /// Visible switches already spent, so anti-flapping survives the engine replacement that each
    /// switch performs.
    pub history: crate::abr::TransitionHistory,
    /// The source carries something no transcode can give back (Dolby Vision, Atmos). Makes
    /// Original's utility bonus about this file rather than about Original in general.
    pub original_features: crate::abr::SourceFeatures,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoOriginalReload {
    Direct,
    Remux,
}

pub struct PrimedHls {
    pub url: String,
    pub encoder_session: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginalProbeResult {
    Measured(crate::curlio::ThroughputSample),
    /// The request reached no usable body. The client-side HLS route remained selected; PMS-side
    /// cursor continuity is not inferred. The outcome is telemetry, not a zero-capacity sample.
    Failed {
        outcome: crate::player::report::TraceOutcome,
        failure: OriginalProbeFailure,
    },
    /// The active route changed while the finite GET was in flight. Its bytes belong to an old
    /// resource epoch and cannot update the new worker's source evidence.
    Stale,
}

/// Photograph-safe detail for an Original source probe failure. The report trace keeps a broad
/// outcome class, but the on-screen panel must not collapse a PMS 503, a deadline and a broken
/// connection into the same `Original check failed` sentence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginalProbeFailure {
    HttpStatus(i32),
    Deadline,
    Transport,
    NoBody,
    Other,
}

/// Why the final candidate ownership transaction did not publish. No arm performs cleanup: the
/// caller still owns the candidate on every refusal and must retire it outside AQ/ACTIVE locks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlsCommitRefusal {
    Session,
    RouteMoved,
    TransitionRejected,
}

impl HlsAbrControl {
    pub fn trace_generation(&self) -> u32 {
        self.trace_generation
    }

    pub fn request_original_recovery(
        &self,
        ticket: &WorkerTicket,
        evidence_kbps: u32,
        position_ns: i64,
    ) -> AutomaticIntentResult {
        publish_automatic_route_intent(AutomaticRouteIntent::HlsToOriginal {
            ticket: ticket.clone(),
            evidence_kbps,
            position_ns,
        })
    }

    pub fn observe_active_variant(
        &self,
        expected: &WorkerTicket,
        variant: crate::abr::ObservedHlsVariant,
        evidence_kbps: u32,
    ) {
        observe_active_hls(expected, variant, evidence_kbps);
    }

    /// Whether every superseded/rejected physical encoder on this PMS has crossed the server's
    /// exact cleanup point, without starting a request.
    pub fn encoder_cleanup_ready(&self) -> bool {
        if !self.fixture_base.is_empty() {
            return true;
        }
        ENCODER_CLEANUP
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_clear(self.sid)
    }

    /// Drive one coalesced background ping from a newly completed active HLS quantum. It never
    /// releases on elapsed time or on the earlier `/stop` acknowledgement.
    pub fn observe_encoder_cleanup(&self) -> bool {
        if self.fixture_base.is_empty() {
            drive_encoder_cleanup(self.sid)
        } else {
            true
        }
    }

    pub fn can_recover_original(&self) -> bool {
        self.has_original_candidate() && self.original_source_kbps > 0
    }

    /// Is there a source to go back TO, independent of whether its rate is known. Split out so the
    /// worker's disarm line can say WHICH of the two terms was missing — a plan that never carried
    /// a candidate and a candidate whose bitrate nobody published are different bugs, and the
    /// single boolean could not tell them apart.
    pub fn has_original_candidate(&self) -> bool {
        !self.original_probe_part.is_empty()
    }

    /// Measure the raw Part with the exact identity of the current HLS Streaming Resource.
    ///
    /// PMS resolves a Part request by exact identity, then by token alias, and creates an AdHoc
    /// resource only after both miss.  Destroying HLS first therefore turns a harmless bounded
    /// read into a fresh server-admission decision; on the incident server that decision is
    /// `99_341 > 92_000` kbps and PMS 1.43.4 turns the refusal into HTTP 500.  Reusing the active
    /// encoder id makes ownership deterministic and needs no client-side stop, close or
    /// replacement decision. It does not prove that PMS preserves the old HLS cursor: observed PMS
    /// can rebind the shared resource during the raw Part read, so a successful recovery must leave
    /// from the same media boundary instead of asking that cursor for one more segment.
    // Dev-only: used only by the `#[cfg(feature = "devtriggers")]` tests in this module's `tests`
    // submodule below (see the comment on the first one).
    #[cfg(all(any(test, feature = "test-support"), feature = "devtriggers"))]
    pub fn probe_original_while_hls(
        &self,
        expected: &WorkerTicket,
        plan: crate::abr::SourceProbePlan,
    ) -> OriginalProbeResult {
        self.probe_original_while_hls_cancellable(expected, plan, || false)
    }

    pub fn probe_original_while_hls_cancellable<F>(
        &self,
        expected: &WorkerTicket,
        plan: crate::abr::SourceProbePlan,
        cancelled: F,
    ) -> OriginalProbeResult
    where
        F: FnOnce() -> bool,
    {
        use crate::curlio::{OpenErr, ThroughputFailure as Failure};
        use crate::player::report::{OriginalProbePhase as Phase, TraceOutcome as Outcome};

        if !is_worker_ticket_current(expected) {
            return OriginalProbeResult::Stale;
        }
        let url = if self.fixture_base.is_empty() {
            let Some(client) = plx_plex::plex::client_for(self.sid) else {
                return OriginalProbeResult::Failed {
                    outcome: Outcome::Inconclusive,
                    failure: OriginalProbeFailure::Other,
                };
            };
            client
                .direct_play_url(&self.original_probe_part, expected.encoder())
                .to_url()
        } else {
            self.original_probe_part.clone()
        };
        crate::player::report::note_original_probe_for(
            self.trace_generation,
            Phase::SampleSource,
            Outcome::Started,
        );
        let sample = crate::curlio::sample_active_throughput_result(
            &url,
            plan.target_bytes,
            std::time::Duration::from_millis(plan.budget_ms),
            std::time::Duration::from_millis(plan.budget_ms),
            cancelled,
        );
        if !is_worker_ticket_current(expected) {
            crate::player::report::note_original_probe_for(
                self.trace_generation,
                Phase::SampleSource,
                Outcome::Inconclusive,
            );
            return OriginalProbeResult::Stale;
        }
        match sample {
            Ok(sample) => {
                crate::player::report::note_original_probe_for(
                    self.trace_generation,
                    Phase::SampleSource,
                    source_probe_sample_outcome(sample),
                );
                OriginalProbeResult::Measured(sample)
            }
            Err(failure) => {
                let detail = match &failure {
                    Failure::Open(OpenErr::Status(status)) => {
                        OriginalProbeFailure::HttpStatus(*status)
                    }
                    Failure::Open(OpenErr::Deadline) | Failure::BodyDeadline => {
                        OriginalProbeFailure::Deadline
                    }
                    Failure::Open(OpenErr::Transport(_) | OpenErr::Multi(_))
                    | Failure::BodyRead { .. } => OriginalProbeFailure::Transport,
                    Failure::NoBody { .. } => OriginalProbeFailure::NoBody,
                    _ => OriginalProbeFailure::Other,
                };
                let outcome = match failure {
                    Failure::Open(OpenErr::Deadline) | Failure::BodyDeadline => Outcome::Deadline,
                    Failure::Open(OpenErr::Transport(_) | OpenErr::Multi(_))
                    | Failure::BodyRead { .. } => Outcome::Transport,
                    Failure::Open(OpenErr::Status(503 | 509)) => Outcome::Refused,
                    Failure::Open(OpenErr::Status(500..=599)) => Outcome::ServerState,
                    Failure::NoBody { .. } => Outcome::NoBody,
                    _ => Outcome::Inconclusive,
                };
                crate::player::report::note_original_probe_for(
                    self.trace_generation,
                    Phase::SampleSource,
                    outcome,
                );
                crate::player::log(&format!(
                    "abr: Original source request produced no capacity sample failure={failure:?}"
                ));
                OriginalProbeResult::Failed {
                    outcome,
                    failure: detail,
                }
            }
        }
    }

    pub fn original_source_kbps(&self) -> u32 {
        self.original_source_kbps
    }

    /// Register a distinct fixed-rendition encoder at the current content boundary. The old
    /// encoder remains active and readable; this only returns the candidate's master URL.
    ///
    /// **The refusal is TYPED, because only this function knows which of its exits it took and the
    /// caller's answer differs by exit.** `ff.rs` classifies every reject as `Candidate` or
    /// `Circumstance` — does the failure say anything about the RUNG — and it was reading a bare
    /// `None` as `Candidate` for all four. Three of the four say nothing about the rung at all:
    /// the active encoder moved underneath (the same event as `origin_changed`, already
    /// `Circumstance`), the server's client is gone, or the control-plane result was unusable.
    /// Only a PMS `refusal` is about the rung, and it is the only one that should arm N11's
    /// backoff — which charges a full `E_tx` refill debt, up to ~4x `E_tx` of blocked climbing,
    /// against exits that spent one round trip or none at all.
    pub fn prime(
        &self,
        off: &plx_base::task::OffFrame,
        expected: &WorkerTicket,
        proposal: crate::abr::Proposal,
        offset_micros: i64,
        deadline: Option<std::time::Instant>,
    ) -> Result<PrimedHls, PrimeRefusal> {
        self.prime_rung(off, expected, proposal.rung, offset_micros, deadline)
    }

    fn prime_rung(
        &self,
        off: &plx_base::task::OffFrame,
        expected: &WorkerTicket,
        rung: crate::abr::Rung,
        offset_micros: i64,
        deadline: Option<std::time::Instant>,
    ) -> Result<PrimedHls, PrimeRefusal> {
        if !is_worker_ticket_current(expected) {
            return Err(PrimeRefusal::Session);
        }
        // Allocated here rather than taken from the worker: see `ENCODER_GENERATION`. A
        // worker-scoped counter outlived by `logical_session` is what let a candidate be named
        // after the live encoder and then stopped as if it were a spare.
        let encoder_session = next_encoder_session(&self.logical_session);
        if !self.fixture_base.is_empty() {
            return Ok(PrimedHls {
                url: format!(
                    "{}/{}/master.m3u8?offset={}.{:06}&X-Plex-Token=fixture-only",
                    self.fixture_base.trim_end_matches('/'),
                    rung.kbps(),
                    offset_micros.max(0) / 1_000_000,
                    offset_micros.max(0) % 1_000_000,
                ),
                encoder_session,
            });
        }
        let Some(client) = plx_plex::plex::client_for(self.sid) else {
            return Err(PrimeRefusal::Session);
        };
        // PMS exposes the two session fields separately, but the overlap TV spike proved it
        // cannot prime a replacement while it shares the old X-Plex id: the first encoder dies
        // before the candidate produces segment zero. Couple both wire fields per encoder.
        let spec = transcode_spec(
            &self.rating_key,
            &encoder_session,
            &encoder_session,
            plx_plex::plex::TranscodeOffset::from_micros(offset_micros),
            self.audio_stream_id,
            self.subtitle_stream_id,
            plx_plex::plex::EncodeContract {
                remux: false,
                no_video_copy: true,
                ceiling: Some(rung.ceiling()),
                delivery: plx_plex::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: self.seconds_per_segment,
                },
                audio: plx_plex::plex::AudioEnhancements::NONE,
            },
        );
        // The deadline-bearing path preserves the cause where it is issued. A completed HTTP
        // response (including malformed 2xx) and a transport failure are Control; only the timer
        // which actually stopped the request is Deadline. The active encoder is checked on the far
        // side of the request so a concurrent route change has priority over every one of them.
        let decision = match deadline {
            Some(at) => {
                let outcome = client.transcode_decision_until(off, &spec, at);
                classify_prime_decision(is_worker_ticket_current(expected), outcome)
            }
            None => {
                let decision = client.transcode_decision(off, &spec);
                if !is_worker_ticket_current(expected) {
                    Err(PrimeRefusal::Session)
                } else {
                    decision.ok_or(PrimeRefusal::Control)
                }
            }
        };
        let decision = match decision {
            Ok(decision) => decision,
            Err(refusal) => {
                // A lost response may still have registered both PMS objects. It cannot be allowed
                // to become the invisible overlap which shrinks the next grant.
                request_encoder_cleanup(self.sid, &encoder_session);
                return Err(refusal);
            }
        };
        if !is_worker_ticket_current(expected) {
            request_encoder_cleanup(self.sid, &encoder_session);
            return Err(PrimeRefusal::Session);
        }
        if refusal(&decision).is_some() {
            request_encoder_cleanup(self.sid, &encoder_session);
            return Err(PrimeRefusal::Rung);
        }
        Ok(PrimedHls {
            url: client.transcode_start_url(&spec).to_url(),
            encoder_session,
        })
    }

    /// Publish a successfully primed encoder together with the caller's controller/local state.
    /// Production calls this while holding the AU queue's abort mutex; this function then holds
    /// ACTIVE while invoking `transition`, giving one AQ -> ACTIVE linearization order. `None`
    /// leaves the route untouched, so a controller precondition can never fail after the process
    /// route has already moved. The closure must perform no I/O or cleanup.
    pub fn commit_transition<T>(
        &self,
        expected: &WorkerTicket,
        candidate: &PrimedHls,
        proposal: crate::abr::Proposal,
        observed: (crate::abr::ObservedHlsVariant, u32),
        transition: impl FnOnce() -> Option<T>,
    ) -> Result<(T, WorkerTicket), HlsCommitRefusal> {
        if self.fixture_base.is_empty() && plx_plex::plex::client_for(self.sid).is_none() {
            return Err(HlsCommitRefusal::Session);
        }
        replace_active_hls_with(
            expected,
            &candidate.encoder_session,
            &candidate.url,
            proposal.rung,
            Some(observed),
            transition,
        )
        .map_err(|refusal| match refusal {
            ActiveHlsCommitRefusal::RouteMoved => HlsCommitRefusal::RouteMoved,
            ActiveHlsCommitRefusal::TransitionRejected => HlsCommitRefusal::TransitionRejected,
        })
    }

    /// Route-only compatibility door used by focused route tests. Production uses
    /// [`Self::commit_transition`] so controller, route and worker-local ownership cannot split.
    #[cfg(any(test, feature = "test-support"))]
    pub fn commit(
        &self,
        expected: &WorkerTicket,
        candidate: &PrimedHls,
        proposal: crate::abr::Proposal,
        observed: (crate::abr::ObservedHlsVariant, u32),
    ) -> bool {
        self.commit_transition(expected, candidate, proposal, observed, || Some(()))
            .is_ok()
    }

    pub fn retire(&self, encoder: String) {
        if !self.fixture_base.is_empty() {
            return;
        }
        request_encoder_cleanup(self.sid, &encoder);
    }

    pub fn abandon(&self, candidate: &str) {
        if !self.fixture_base.is_empty() {
            return;
        }
        // Returning to the proven cursor has zero media-control cost only if retiring the failed
        // encoder does not hold the demux worker. The stop+ping lifecycle stays on tiny workers;
        // the common ledger survives seek/reload and supplies the exact resource-release barrier.
        request_encoder_cleanup(self.sid, candidate);
    }
}

/// **The feasibility filter, as one object the worker cannot argue with.** Built from two
/// independent facts and nothing else: what this device's own codec table says it decodes
/// (`devcaps`, which exists because "4K yes" was once a constant describing one television), and
/// what raster the source actually has. Neither is a preference and neither belongs in a utility
/// weight — a candidate outside these bounds is removed before anything is scored.
fn auto_catalog(ps: &PlaybackSession) -> crate::abr::HlsActuatorCatalog {
    let caps = plx_platform::devcaps::caps();
    let device = (
        u16::try_from(caps.hevc_max.0).unwrap_or(u16::MAX),
        u16::try_from(caps.hevc_max.1).unwrap_or(u16::MAX),
    );
    let (_, width, height) = ps.cur_src;
    let source = (
        u16::try_from(width).unwrap_or(u16::MAX),
        u16::try_from(height).unwrap_or(u16::MAX),
    );
    crate::abr::HlsActuatorCatalog::measured().limited_to(device, source)
}

/// Visible switches spent so far, aged. Read on the main thread at worker spawn; the worker
/// advances it with its own clock from there.
///
/// `now_ms` is THIS FRAME's millisecond stamp — [`PlaybackSession::now_ms`], which
/// `player::machine::Player::set_now` writes once per iteration from the loop's `fr.now` and
/// nothing else writes at all. It is not `Instant::now()`, and since phase 9 it is not a second
/// clock read either (spec §4.1): this module makes no wall-clock read of any kind.
fn auto_history(ps: &PlaybackSession, now_ms: u32) -> crate::abr::TransitionHistory {
    let s = &*ps;
    crate::abr::TransitionHistory {
        visible_switches: s.auto_switches,
        since_last_ms: s
            .auto_last_switch
            .map(|at| u64::from(now_ms.wrapping_sub(at))),
    }
}

/// Record that the viewer just saw a mode change. Called by BOTH halves of the transaction, which
/// is the point: a fallback and a recovery are equally visible, and it is their ALTERNATION that
/// the penalty exists to price. `now_ms`: see [`auto_history`]'s doc.
fn note_visible_switch(ps: &mut PlaybackSession, now_ms: u32) {
    { let s = &mut *ps; {
        s.auto_switches = s.auto_switches.saturating_add(1);
        s.auto_last_switch = Some(now_ms);
    } };
}

/// The source-probe measurement this playback already paid for, as a weak prior. `None` once it is
/// too old to mean anything or when there never was one.
/// **What the next controller starts from, and the seek is why it has two sources** (I8).
///
/// The CARRIED estimate wins when there is one. A seek destroys the engine and builds a fresh
/// `Controller`, and before this the only thing that survived was `auto_prior_kbps` — whose writer
/// on the Original->HLS fallback path is `measured_kbps` *at the moment the link failed*. So after
/// one bad patch every subsequent seek re-seeded from the worst rate the playback had ever
/// measured, at `MAX_UNCERTAINTY_PM` with one sample, and the ladder re-ramped for five to ten
/// segments: ten to twenty seconds of visibly softer picture after every skip.
///
/// **`auto_prior_kbps` is not deleted and is not a fallback of convenience.** It remains the
/// BOOTSTRAP seed — the startup probe, and the rate measured when Original was abandoned — which
/// is the right seed when there is no live HLS estimate to carry, i.e. the first controller of a
/// playback. `from_prior` states its own weakness (uncertainty at the cap, one sample); the
/// carried snapshot states what was actually observed. Two different claims, two constructors.
///
/// Only the DELIVERY estimate crosses. The buffer, the risk history and any pending transaction
/// describe a position that no longer exists and are reset by the new `Controller`'s construction.
fn auto_prior(ps: &PlaybackSession) -> Option<crate::abr::CapacityEstimate> {
    let carried = crate::player::SHARED.abr_seed();
    carried.or_else(|| {
        let kbps = ps.auto_prior_kbps;
        (kbps > 0).then(|| crate::abr::CapacityEstimate::from_prior(kbps))
    })
}

/// Does this playback's source carry something a transcode cannot give back? Dolby Vision and
/// Atmos are the two that matter here, and both are recorded on the Original candidate rather than
/// inferred from the stream now playing (which, mid-HLS, is a re-encode of them).
fn auto_original_features(ps: &PlaybackSession) -> crate::abr::SourceFeatures {
    ps
        .auto_original
        .as_ref()
        .map(|candidate| crate::abr::SourceFeatures {
            dv: candidate.dovi.profile > 0,
            atmos: candidate.audio.as_ref().is_some_and(|a| a.immersive),
        })
        .unwrap_or_default()
}

/// Main-thread capture immediately before spawning the HLS demux worker.
pub fn hls_abr_control(ps: &PlaybackSession) -> Option<(HlsAbrControl, WorkerTicket)> {
    if forced_direct_play(ps) { return None; }
    let seconds_per_segment = match cur_delivery(ps) {
        plx_plex::plex::TranscodeDelivery::FixedHls {
            seconds_per_segment,
        } => seconds_per_segment,
        plx_plex::plex::TranscodeDelivery::ProgressiveMkv => return None,
    };
    let live = active_hls();
    let ticket = live
        .as_ref()
        .map(|(ticket, _)| ticket.clone())
        .unwrap_or_else(worker_ticket);
    if ticket.encoder().is_empty() {
        return None;
    }
    // A manual Original open that failed is restored onto HLS while the picker still reflects the
    // user's attempted choice.  That route still needs rung control, but it must not immediately
    // auto-retry the same failed source. Selecting Auto later adopts this worker in place; a
    // subsequent seek/reload constructs a fresh controller with Original recovery enabled again.
    let original = (applied_quality() == Quality::Auto)
        .then(|| ps.auto_original.as_ref())
        .flatten();
    Some((
        HlsAbrControl {
            trace_generation: playback_trace_generation(),
            sid: cur_sid(ps),
            rating_key: cur_rk(ps),
            logical_session: sess(ps),
            audio_stream_id: cur_audio_sid(ps),
            subtitle_stream_id: cur_sub_sid(ps),
            seconds_per_segment,
            initial_rung: live
                .as_ref()
                .map(|(_, hls)| hls.rung)
                .or_else(|| cur_ceiling(ps).and_then(crate::abr::Rung::from_ceiling))
                .unwrap_or(crate::abr::Rung::P480),
            initial_observed: live.as_ref().and_then(|(_, hls)| hls.observed),
            fixture_base: ps.auto_fixture_base.clone(),
            original_probe_part: original.map(|c| c.probe_part.clone()).unwrap_or_default(),
            // **Whole-file rate if PMS gave one, else the video rate — but NEVER zero while a
            // candidate exists.** `cur_transport_kbps`'s zero means "PMS did not say", and
            // `can_recover_original` reads this as "there is no way back", which silently deletes
            // the entire recovery feature — `ff.rs` then never constructs `OriginalRecovery`, and
            // `probe_due` is the only thing that logs a reason, so the deletion is invisible.
            // See `a_missing_whole_file_bitrate_must_not_silently_delete_original_recovery`.
            //
            // The video rate is the same quantity minus the audio track. It makes
            // `source_requirement_kbps` slightly optimistic, which the probe then corrects with a
            // real measurement of the real file — that is what the probe is for.
            original_source_kbps: original
                .and_then(|_| {
                    let s = &*ps;
                    u32::try_from(s.cur_transport_kbps)
                        .ok()
                        .filter(|&kbps| kbps > 0)
                        .or_else(|| u32::try_from(s.cur_src.0).ok().filter(|&kbps| kbps > 0))
                })
                .unwrap_or(0),
            catalog: auto_catalog(ps),
            prior: auto_prior(ps),
            history: auto_history(ps, ps.now_ms),
            original_features: auto_original_features(ps),
        },
        ticket,
    ))
}

/// Main-thread capture for a progressive demux worker. `Some` is the complete authorization to
/// turn sustained starvation into an HLS replacement, plus everything the decision needs; the
/// worker never reads route's mutable session directly.
#[derive(Clone, Debug)]
pub struct AutoOriginalWatch {
    pub ticket: WorkerTicket,
    pub source_kbps: u32,
    pub catalog: crate::abr::HlsActuatorCatalog,
    pub history: crate::abr::TransitionHistory,
    pub features: crate::abr::SourceFeatures,
}

impl AutoOriginalWatch {
    pub fn request_hls_fallback(
        &self,
        conservative_kbps: u32,
        position_ns: i64,
    ) -> AutomaticIntentResult {
        publish_automatic_route_intent(AutomaticRouteIntent::OriginalToHls {
            ticket: self.ticket.clone(),
            conservative_kbps,
            position_ns,
        })
    }

    /// A direct in-place seek keeps this demux worker and semantic route but invalidates every
    /// pre-seek measurement. Return a fresh ticket only for that exact case; an engine/route move
    /// belongs to another worker and may not be adopted.
    pub fn refresh_ticket_after_seek(&mut self) -> bool {
        let current = worker_ticket();
        if current.engine_epoch != self.ticket.engine_epoch || current.route != self.ticket.route {
            return false;
        }
        self.ticket = current;
        true
    }
}

pub fn auto_original_watch(ps: &PlaybackSession) -> Option<AutoOriginalWatch> {
    if forced_direct_play(ps) { return None; }
    let s = &*ps;
    if applied_quality() != Quality::Auto
        || !s.cur_auto_original_watched
        || !matches!(
            s.cur_contract.delivery,
            plx_plex::plex::TranscodeDelivery::ProgressiveMkv
        )
    {
        return None;
    }
    let source_kbps = u32::try_from(s.cur_transport_kbps)
        .ok()
        .filter(|&kbps| kbps > 0)?;
    Some(AutoOriginalWatch {
        ticket: worker_ticket(),
        source_kbps,
        catalog: auto_catalog(ps),
        history: auto_history(ps, ps.now_ms),
        features: auto_original_features(ps),
    })
}

/// Arm the no-Plex pipeline tier for the same Original→HLS transaction production uses. The only
/// substitution is URL allocation: [`HlsAbrControl`] maps a rung to fixture playlists rather than
/// asking PMS to create an encoder. Transport, FFmpeg, buffer measurement, the controller, pump
/// handoff and Starfish are unchanged, which makes a mid-request bandwidth profile testable on a
/// TV without a library, account, token, or external server.
///
/// `start_hls` skips the Original phase entirely, removes the synthetic Original candidate and
/// returns the playlist to open. Use it for every case that grades the HLS CONTROLLER; leave it
/// off only where the transition itself is what is being graded, and give that case a
/// `network_profile` that starves for real. Removing the candidate is load-bearing: otherwise a
/// loopback source probe can escape to Original before a request-indexed HLS cliff occurs.
/// [`crate::player::playurl::PlayUrl::auto_start_hls`] has the history — the alternative was declaring a
/// source rate no link could carry and relying on a starvation horizon that did not check whether
/// the reserve was draining.
pub fn arm_auto_fixture(
    ps: &mut PlaybackSession,
    original_url: &str,
    source_kbps: u32,
    hls_base: &str,
    start_hls: bool,
    source_raster: (u16, u16),
) -> Option<String> {
    { let s = &mut *ps; {
        s.url = original_url.to_owned();
        s.cur_rk = "__auto_fixture__".into();
        s.sess = "auto-fixture".into();
        s.cur_contract.delivery = plx_plex::plex::TranscodeDelivery::ProgressiveMkv;
        s.cur_contract.ceiling = None;
        // **Saying the source raster out loud is load-bearing rather than cosmetic**: an unknown
        // one is treated as unbounded (`HlsActuatorCatalog::limited_to`), which makes the 4K
        // actuator feasible on every case. It was a hardcoded 1080p because
        // `tests/serve_fixtures.py` served no 22000 rung, so such a candidate would 404 and read
        // on the television as a rejected encoder — a fixture gap standing in for a policy, and
        // the thing that kept the plan's I9 blocked. The server answers 22000 now, so the caller
        // declares it (`player::playurl::PlayUrl::source_raster`) and the default is still 1080p.
        s.cur_src = (
            i64::from(source_kbps),
            i64::from(source_raster.0),
            i64::from(source_raster.1),
        );
        s.cur_transport_kbps = i64::from(source_kbps);
        s.cur_auto_original_watched = true;
        s.auto_bootstrap_rung = Some(crate::abr::Rung::P480);
        s.auto_original = Some(AutoOriginalCandidate {
            url: original_url.to_owned(),
            probe_part: original_url.to_owned(),
            direct: true,
            vcodec: "h264".into(),
            fps: 0.0,
            dovi: plx_data::metadata::Dovi::NONE,
            dv_decision: plx_data::metadata::DvDecision::NONE,
            audio: Some(CarriedAudio {
                codec: "aac".into(),
                ..CarriedAudio::named(0, -1)
            }),
            audio_converted: false,
            subtitle_ordinal: None,
        });
        s.auto_fixture_base = hls_base.trim_end_matches('/').to_owned();
    } };
    install_active_encoder("");
    crate::player::log(&format!(
        "auto fixture: Original source={}kbps armed",
        source_kbps
    ));
    if !start_hls {
        // The fixture is a real playback entry point (used by the device harness), not a bag of
        // Session test setters.  Publish the installed Original through the same reducer landing
        // as a resolved Plex item so its worker owns the selected Auto contract.  Without this,
        // the durable picker said Auto while `applied_quality` still named the previous playback,
        // and the progressive watchdog was correctly refused as belonging to another contract.
        settle_plan_start_in_unit_test(ps, prepare_playback_landing(ps, true));
        return None;
    }
    // Install exactly the state `fallback_auto_to_hls` leaves behind, at the bootstrap rung, and
    // hand the caller the playlist to open. See `player::playurl::PlayUrl::auto_start_hls` for why this exists
    // at all: the alternative was declaring a source rate no link could carry and relying on the
    // starvation horizon to fire on a reserve that was visibly FILLING.
    let rung = crate::abr::Rung::P480;
    let base = hls_base.trim_end_matches('/');
    let url = format!(
        "{base}/{}/master.m3u8?X-Plex-Token=<plex-token>",
        rung.kbps()
    );
    let encoder = format!("auto-fixture-{}", rung.kbps());
    { let s = &mut *ps; {
        s.cur_auto_original_watched = false;
        // `start_hls` means this is an HLS-only controller fixture, not merely an HLS entry point.
        // A synthetic whole-file request on loopback is not constrained by a later HLS-only
        // segment profile and can therefore pre-empt the very collapse the case was built to
        // grade. Original recovery has its own end-to-end fixture with `start_hls == false`.
        s.auto_original = None;
        s.cur_contract.remux = false;
        s.cur_contract.delivery = plx_plex::plex::TranscodeDelivery::FixedHls {
            seconds_per_segment: 2,
        };
        s.cur_contract.ceiling = Some(rung.ceiling());
        s.url = url.clone();
        s.tsession = encoder.clone();
        s.stream_vcodec = "h264".into();
        s.stream_acodec = "aac".into();
    } };
    install_active_hls(&encoder, &url, rung);
    crate::player::log(&format!(
        "auto fixture: starting in {}kbps {}x{} HLS (no Original phase)",
        rung.kbps(),
        rung.raster().0,
        rung.raster().1,
    ));
    settle_plan_start_in_unit_test(ps, prepare_playback_landing(ps, true));
    Some(url)
}

/// Main-thread half of the progressive watchdog transaction. The demux worker has stopped at a
/// packet boundary and published its CONSERVATIVE delivery estimate — not the last window's raw
/// rate, which is one sample of a distribution; atomically move the route to the best HLS state
/// that estimate sustains, then build the replacement encoder at the current movie position. The
/// caller performs the fresh Starfish Load only when this returns a URL.
// Dev-only: used only by the `#[cfg(feature = "devtriggers")]` tests in this module's `tests`
// submodule below (see the comment on the first one).
#[cfg(all(any(test, feature = "test-support"), feature = "devtriggers"))]
pub fn fallback_auto_to_hls(ps: &mut PlaybackSession, measured_kbps: u32, offset_secs: i64) -> Option<String> {
    let expected = worker_ticket();
    fallback_auto_to_hls_for(ps, &expected, measured_kbps, offset_secs)
}

/// The three steps of [`plan_auto_hls`], [`run_auto_hls`] and [`install_auto_hls_outcome`] run in a
/// row on the calling thread, for the host tests that grade the steps' joint outcome (the Auto
/// watchdog's own fallback is a flight: [`execute_auto_hls_claim`]). No shipping caller is left, so
/// it is not built into the binary.
#[cfg(any(test, feature = "test-support"))]
pub fn fallback_auto_to_hls_for(
    ps: &mut PlaybackSession,
    expected: &WorkerTicket,
    measured_kbps: u32,
    offset_secs: i64,
) -> Option<String> {
    let plan = plan_auto_hls(ps, expected, measured_kbps, offset_secs)?;
    let outcome = run_auto_hls(&plx_base::task::OffFrame::for_test(), &plan);
    let installed = install_auto_hls_outcome(ps, plan, outcome)?;
    installed.retire_replaced();
    Some(installed.url())
}

/// The rejection the automatic Original-to-HLS fallback settles with: the Auto worker exited to
/// hand the action over, so the pump raises the producer's failure for its ordinary recovery.
pub(super) const AUTO_HLS_REJECTED: &str = "auto: synchronized Original fallback could not build HLS";

/// What a recovery dispatch did: the failure-path flights (the Original rollback's rebase, the
/// unopened source's HLS fallback) are Start-owned and have no "try again later".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryDispatch {
    /// A worker owns the PMS half; the drain ([`take_ready_recovery_flight`]) delivers the verdict.
    Flying { serial: u64 },
    /// Nothing to rebuild, another transaction holds the reducer, or the worker could not start:
    /// the failure stands.
    Refused,
}

/// The rollback's rebase gave up: said once, by whoever settles the refusal.
pub const ROLLBACK_REBASE_REFUSED: &str = "abr: restored HLS encoder but could not rebase it to the recovery position";

/// Start the flight for the rollback after a failed Original trial: rebase the restored HLS route
/// at `offset_ns` ([`plan_rebase`] with [`RebaseFor::Rollback`] on the frame thread, [`run_rebase`]
/// on a worker, [`install_rebase`] at the landing). The reducer holds `Preparing(serial)` until the
/// landing, which hands the pump [`RecoveryVerdict::Install`] to reload on or
/// [`RecoveryVerdict::Refused`] to fail on — the `Some`/`None` the synchronous rebase answered.
pub fn dispatch_rollback_rebase(ps: &mut PlaybackSession, offset_ns: i64) -> RecoveryDispatch {
    dispatch_rebase_recovery(ps, offset_ns, RebaseFor::Rollback, ROLLBACK_REBASE_REFUSED, false)
}

/// [`dispatch_rollback_rebase`] for a rollback NO ENGINE waits on: the Original trial's Load failed
/// while it was being constructed (the foreground restore's, or the pump's
/// `start_original_trial_reload`), so there is no Engine whose pump could drain the landing. The
/// flight is marked Engine-less ([`begin_engineless_flight`]): the app loop drains it, the HUD
/// reads `Resolving`, and an app-switch suspend drops it.
pub fn dispatch_engineless_rollback_rebase(ps: &mut PlaybackSession, offset_ns: i64) -> RecoveryDispatch {
    dispatch_rebase_recovery(ps, offset_ns, RebaseFor::Rollback, ROLLBACK_REBASE_REFUSED, true)
}

/// The rejection a refused cold-resume rebuild settles with (never logged as a rollback's).
pub(super) const RESUME_REBASE_REFUSED: &str = "resume: transcode could not be rebuilt at the saved position";

/// Start the flight of a COLD RESUME: the plan a resolve just landed is a transcode at offset 0, and
/// the saved position is owed before its first Load. Plan on the frame thread ([`plan_rebase`] with
/// [`RebaseFor::Resume`]), PMS on a worker ([`run_rebase`]), install at the landing
/// ([`install_rebase`]). The reducer holds `Preparing(serial)` — the start transaction the landing
/// left `Prepared` — until [`take_ready_recovery_flight`] hands back [`RecoveryVerdict::Install`]
/// (the Load starts at the offset) or [`RecoveryVerdict::Refused`] (the transaction is `Failed`).
/// [`RecoveryDispatch::Refused`] is the refusal said at the dispatch: nothing to rebuild, or the
/// worker could not start.
pub fn dispatch_resume_rebase(ps: &mut PlaybackSession, offset_ns: i64) -> RecoveryDispatch {
    dispatch_rebase_recovery(ps, offset_ns, RebaseFor::Resume, RESUME_REBASE_REFUSED, true)
}

/// The one dispatch of a rebuild that is owned by a start rather than by a claim or a seek: the
/// rollback's and the cold resume's. Both hold a recovery flight (`Preparing(serial)`) and land
/// through [`take_ready_recovery_flight`]; `owner` says which marker the reducer carries, and
/// `engineless` whether no Engine waits on the flight (a resume never has one).
fn dispatch_rebase_recovery(
    ps: &mut PlaybackSession,
    offset_ns: i64,
    owner: RebaseFor,
    refused: &'static str,
    engineless: bool,
) -> RecoveryDispatch {
    let Ok(plan) = plan_rebase(ps, offset_ns / 1_000_000_000, owner) else {
        return RecoveryDispatch::Refused;
    };
    let ticket = plan.route_start.expect("a recovery flight owns its start transaction");
    match owner {
        RebaseFor::Resume => begin_resume_flight(ticket.serial, offset_ns),
        _ if engineless => begin_engineless_flight(ticket.serial, offset_ns),
        _ => begin_recovery_flight(ticket.serial, offset_ns),
    }
    if spawn_flight(
        FlightOwner::Recovery(ticket),
        offset_ns,
        offset_ns,
        None,
        ClaimWork::Rebase(Box::new(plan)),
        RetranscodeFallback::RejectWith(refused),
    ) {
        RecoveryDispatch::Flying { serial: ticket.serial }
    } else {
        // The OS refused the thread: nothing was registered, so there is nothing to stop.
        release_rebase_start(owner, ticket);
        RecoveryDispatch::Refused
    }
}

/// The frame-thread half of the unopened-source fallback: reuse the exact contingency
/// [`crate::abr::bootstrap`] chose while it still owned the evidence.
///
/// An HTTP 4xx/5xx, connect refusal or demux open error before the first body byte is not a
/// zero-throughput observation. For Remote the rung came from the completed source probe; for
/// Local it remains the unknown-link fallback — source consumption is demand, not capacity.
fn plan_unopened_auto_hls(ps: &PlaybackSession, offset_secs: i64) -> Option<AutoHlsPlan> {
    let expected = worker_ticket();
    let watch = auto_original_watch(ps)?;
    if cur_rk(ps).is_empty() {
        return None;
    }
    let bootstrap_rung = ps.auto_bootstrap_rung;
    let rung = crate::abr::original_open_fallback_rung(
        bootstrap_rung,
        &watch.catalog,
        &crate::abr::AbrPolicy::measured(),
    );
    crate::player::log(&format!(
        "auto: Original source open failed without a throughput sample; reusing bootstrap {:?} as {}kbps {}x{} HLS",
        bootstrap_rung,
        rung.kbps(),
        rung.raster().0,
        rung.raster().1,
    ));
    // A source which never opened showed no Original picture, so its recovery is not a visible
    // switch for the anti-flap history.
    plan_auto_hls_for(
        ps,
        &expected,
        rung,
        offset_secs,
        false,
        crate::player::report::DeliveryReason::OriginalOpenRollback,
    )
}

/// Start the flight that replaces an Auto Original whose source never opened: plan on the frame
/// thread ([`plan_unopened_auto_hls`]), PMS on a worker ([`run_auto_hls`]), install at the landing
/// ([`install_auto_hls_outcome`]). Nothing is playing, so there is no Original to keep: the flight
/// is the only way forward, and [`RecoveryDispatch::Refused`] / [`RecoveryVerdict::Refused`] are the
/// refusal (the pump raises the failure).
pub fn dispatch_unopened_auto_hls(ps: &mut PlaybackSession, offset_secs: i64) -> RecoveryDispatch {
    let Some(plan) = plan_unopened_auto_hls(ps, offset_secs) else {
        return RecoveryDispatch::Refused;
    };
    let Some(ticket) = begin_recovery_flight_start() else {
        return RecoveryDispatch::Refused;
    };
    let offset_ns = offset_secs * 1_000_000_000;
    begin_recovery_flight(ticket.serial, offset_ns);
    if spawn_flight(
        FlightOwner::Recovery(ticket),
        offset_ns,
        offset_ns,
        None,
        ClaimWork::AutoHls(Box::new(plan)),
        RetranscodeFallback::RejectWith(AUTO_HLS_REJECTED),
    ) {
        RecoveryDispatch::Flying { serial: ticket.serial }
    } else {
        let _ = reject_route_start_preparation(ticket);
        RecoveryDispatch::Refused
    }
}

/// [`plan_unopened_auto_hls`], [`run_auto_hls`] and [`install_auto_hls_outcome`] run in a row on
/// the calling thread, for the host tests that grade the steps' joint outcome (the pump's own
/// fallback is a flight: [`dispatch_unopened_auto_hls`]). No shipping caller is left, so it is not
/// built into the binary.
#[cfg(any(test, feature = "test-support"))]
pub fn fallback_unopened_auto_to_hls(ps: &mut PlaybackSession, offset_secs: i64) -> Option<String> {
    let plan = plan_unopened_auto_hls(ps, offset_secs)?;
    let outcome = run_auto_hls(&plx_base::task::OffFrame::for_test(), &plan);
    let installed = install_auto_hls_outcome(ps, plan, outcome)?;
    installed.retire_replaced();
    Some(installed.url())
}

/// The segmentation every automatic HLS route is built with.
const AUTO_HLS_DELIVERY: plx_plex::plex::TranscodeDelivery =
    plx_plex::plex::TranscodeDelivery::FixedHls { seconds_per_segment: 2 };

/// **An Original-to-HLS fallback in three steps, so that the middle one — the only one that reaches
/// PMS — runs on a worker.** The frame thread plans ([`plan_auto_hls`]: every read of the session,
/// the ticket and the contract), the PMS half runs ([`run_auto_hls`]: the selection PUT and the
/// replacement encoder's `/decision`), and the frame thread installs the outcome
/// ([`install_auto_hls_outcome`]: the route commit, every write to the session, the anti-flap and
/// delivery bookkeeping). Both consumers are flights: the Auto watchdog's own fallback
/// ([`execute_auto_hls_claim`]) and the source that never opened ([`dispatch_unopened_auto_hls`]).
///
/// **Nothing of the attempt is written until the install.** The session keeps describing the live
/// Original while the worker runs, so a refused or stale landing has nothing to restore, and the
/// worker commits nothing: a landing that never installs stops ONLY the replacement encoder it
/// registered ([`discard_auto_hls`]), never the stream on screen.
pub(super) struct AutoHlsPlan {
    /// The route generation the fallback was planned against. The commit is gated on it.
    expected: WorkerTicket,
    rung: crate::abr::Rung,
    /// A starvation handoff is a visible mode switch and is charged to the anti-flap history; a
    /// source which never opened showed no Original picture, so its recovery is not.
    visible_switch: bool,
    reason: crate::player::report::DeliveryReason,
    route: AutoHlsRoute,
}

enum AutoHlsRoute {
    /// The no-Plex pipeline tier: the rung's playlist on the fixture server. No PMS.
    Fixture { base: String },
    Encode { inputs: RetranscodeClaimInputs, contract: plx_plex::plex::EncodeContract },
}

/// What the PMS half settled on.
pub(super) enum AutoHlsOutcome {
    Fixture { encoder: String, url: String },
    /// PMS accepted the replacement. NOT yet the route's: the install commits it.
    Prepared(PreparedEncode),
    /// PMS refused (or never answered); the replacement it may have registered is already stopped.
    Refused,
}

/// The frame-thread half of the fallback: decide whether one is allowed and capture what its PMS
/// half will need. `None` touches nothing but the anti-flap seed ([`PlaybackSession::auto_prior_kbps`],
/// which the controller reads whether or not the rebuild succeeds), exactly as before the split.
pub(super) fn plan_auto_hls(
    ps: &mut PlaybackSession,
    expected: &WorkerTicket,
    measured_kbps: u32,
    offset_secs: i64,
) -> Option<AutoHlsPlan> {
    if forced_direct_play(ps) { return None; }
    if !is_worker_ticket_current(expected) {
        return None;
    }
    // Publication already proved that this exact applied worker was Auto Original. The durable
    // picker may have moved while the accepted handoff waited on the main thread; consulting it
    // here relabelled the old applied event as the new (possibly refused) desire and killed the
    // only producer. Applied quality is reducer state and changes only on a committed user action.
    if applied_quality() != Quality::Auto || cur_rk(ps).is_empty() {
        return None;
    }
    let rung = crate::abr::original_fallback_rung(
        measured_kbps,
        &auto_catalog(ps),
        &crate::abr::AbrPolicy::measured(),
    );
    { let s = &mut *ps; s.auto_prior_kbps = measured_kbps };
    crate::player::log(&format!(
        "auto: Original became unsustainable at {measured_kbps}kbps; switching to {}kbps {}x{} HLS",
        rung.kbps(),
        rung.raster().0,
        rung.raster().1,
    ));
    plan_auto_hls_for(
        ps,
        expected,
        rung,
        offset_secs,
        true,
        crate::player::report::DeliveryReason::LinkFallback,
    )
}

/// [`plan_auto_hls`]'s second half, for a rung already chosen from the appropriate evidence.
fn plan_auto_hls_for(
    ps: &PlaybackSession,
    expected: &WorkerTicket,
    rung: crate::abr::Rung,
    offset_secs: i64,
    visible_switch: bool,
    reason: crate::player::report::DeliveryReason,
) -> Option<AutoHlsPlan> {
    let route = if ps.auto_fixture_base.is_empty() {
        AutoHlsRoute::Encode {
            inputs: prepare_retranscode_inputs(ps, expected, offset_secs)?,
            // Today's rebuild of the contract the session will carry once this lands: HLS at the
            // rung, never an enhanced remux (a re-encode is family `Other`).
            contract: re_encode_contract(ps, AUTO_HLS_DELIVERY, Some(rung.ceiling())),
        }
    } else {
        AutoHlsRoute::Fixture { base: ps.auto_fixture_base.clone() }
    };
    Some(AutoHlsPlan { expected: expected.clone(), rung, visible_switch, reason, route })
}

/// The PMS half of the fallback — everything that blocks on the server and nothing that touches a
/// `PlaybackSession` or the route, so it runs on a worker (`_off` is the proof: the frame thread
/// cannot mint one). It sends the selection PUT and registers the replacement encoder with
/// `/decision`; the COMMIT is [`install_auto_hls_outcome`]'s, on the frame thread.
pub(super) fn run_auto_hls(off: &plx_base::task::OffFrame, plan: &AutoHlsPlan) -> AutoHlsOutcome {
    match &plan.route {
        AutoHlsRoute::Fixture { base } => AutoHlsOutcome::Fixture {
            encoder: format!("auto-fixture-{}", plan.rung.kbps()),
            url: format!(
                "{}/{}/master.m3u8?X-Plex-Token=fixture-only",
                base.trim_end_matches('/'),
                plan.rung.kbps(),
            ),
        },
        AutoHlsRoute::Encode { inputs, contract } => {
            select_streams_for_encode(off, inputs);
            request_retranscode(off, inputs, *contract).map_or(AutoHlsOutcome::Refused, AutoHlsOutcome::Prepared)
        }
    }
}

enum LandedAutoHls {
    Fixture { encoder: String, url: String },
    Encoded(AppliedRetranscode),
}

/// What a landed fallback changed, for the caller that has to describe it.
pub(super) struct AutoHlsInstalled {
    rung: crate::abr::Rung,
    landed: LandedAutoHls,
}

impl AutoHlsInstalled {
    /// The fields the install wrote to the session, on a projection: the reducer's restore point of
    /// a claim must describe the stream the landing installed ([`advance_claim_snapshot`]). HLS is
    /// a full H.264/AAC encode: source FPS, Dolby Vision and E-AC3 JOC/Atmos belong to the Original
    /// elementary streams and may not survive this route transition.
    pub(super) fn apply_to_projection(&self, p: &mut AppliedRouteProjection) {
        p.auto_original_watched = false;
        p.contract.remux = false;
        p.contract.delivery = AUTO_HLS_DELIVERY;
        p.contract.ceiling = Some(self.rung.ceiling());
        p.stream_vcodec = "h264".into();
        p.stream_acodec = "aac".into();
        p.stream_fps = 0.0;
        p.stream_dovi = plx_data::metadata::Dovi::NONE;
        p.stream_dv_decision = plx_data::metadata::DvDecision::NONE;
        p.stream_immersive = false;
        match &self.landed {
            LandedAutoHls::Fixture { encoder, url } => {
                p.tsession = encoder.clone();
                p.url = url.clone();
            }
            LandedAutoHls::Encoded(applied) => apply_retranscode_outcome_to_projection(p, applied),
        }
    }

    /// The encoder the fallback replaced, with the server that owns it: still the stream on screen
    /// until the reload, so the caller retires it only after that reload
    /// ([`retire_superseded_encoder`]). `None` for a Direct Original and for the fixture.
    pub(super) fn superseded(&self) -> Option<(&'static plx_plex::plex::Client, String)> {
        match &self.landed {
            LandedAutoHls::Encoded(applied) if !applied.superseded.is_empty() => {
                Some((applied.client, applied.superseded.clone()))
            }
            _ => None,
        }
    }

    /// The replacement route's URL, for the host tests that run the steps in a row.
    #[cfg(any(test, feature = "test-support"))]
    fn url(&self) -> String {
        match &self.landed {
            LandedAutoHls::Fixture { url, .. } => url.clone(),
            LandedAutoHls::Encoded(applied) => applied.url.clone(),
        }
    }

    /// Stop the replaced encoder at once: for the caller that reloads onto the new stream next and
    /// has no flight to retire it after.
    pub(super) fn retire_replaced(&self) {
        if let Some((client, session)) = self.superseded() {
            stop_encoder_session(client, session);
        }
    }
}

/// The frame-thread half that lands a fallback: commit the replacement as the route's encoder
/// (gated on the plan's ticket), write the session projection and charge the switch. `None` is a
/// refusal and nothing was written: PMS said no, or the route moved while the worker ran (the
/// replacement it registered is stopped here — it never became anyone's).
pub(super) fn install_auto_hls_outcome(
    ps: &mut PlaybackSession,
    plan: AutoHlsPlan,
    outcome: AutoHlsOutcome,
) -> Option<AutoHlsInstalled> {
    let AutoHlsPlan { expected, rung, visible_switch, reason, route } = plan;
    let landed = match (route, outcome) {
        (AutoHlsRoute::Fixture { .. }, AutoHlsOutcome::Fixture { encoder, url }) => {
            replace_active_hls_for(&expected, &encoder, &url, rung, None)?;
            LandedAutoHls::Fixture { encoder, url }
        }
        (AutoHlsRoute::Encode { inputs, .. }, AutoHlsOutcome::Prepared(prepared)) => {
            match commit_retranscode(&inputs, prepared) {
                Ok(applied) => LandedAutoHls::Encoded(applied),
                Err(refused) => {
                    stop_encoder_session(inputs.client, refused.qsess);
                    return None;
                }
            }
        }
        (_, AutoHlsOutcome::Refused) => return None,
        (_, outcome) => {
            // Cannot happen: the outcome is built from the very plan it lands with.
            debug_assert!(false, "an auto-HLS outcome that does not belong to its plan");
            drop(outcome);
            return None;
        }
    };
    let installed = AutoHlsInstalled { rung, landed };
    {
        let mut projection = route_projection(ps);
        installed.apply_to_projection(&mut projection);
        install_route_projection(ps, &projection);
    }
    if let LandedAutoHls::Encoded(applied) = &installed.landed {
        log_enhancement_outcome(
            Some((ps.stream_vcodec.as_str(), ps.stream_acodec.as_str())),
            applied.enhancement,
            applied.contract.audio,
        );
    }
    // Counted only on the paths that really produce a replacement URL. A switch that failed to
    // build is not one the viewer saw, and the anti-flapping penalty prices what they SAW — the
    // pump turns a refusal here into a playback error, not into a mode change.
    if visible_switch {
        note_visible_switch(ps, ps.now_ms);
    }
    crate::player::report::note_delivery_requested_for(
        playback_trace_generation(),
        crate::player::report::DeliveryClass::Hls,
        crate::player::report::rung_quality_class(rung),
        reason,
    );
    Some(installed)
}

/// An outcome that will never install: stop ONLY the replacement its worker registered. The worker
/// committed nothing (the commit is [`install_auto_hls_outcome`]'s), so the encoder still on screen
/// is the route's and is left alone — unlike a claim's landing, whose worker already made its
/// replacement the route's encoder.
pub(super) fn discard_auto_hls(plan: AutoHlsPlan, outcome: AutoHlsOutcome) {
    if let (AutoHlsRoute::Encode { inputs, .. }, AutoHlsOutcome::Prepared(prepared)) = (plan.route, outcome) {
        stop_encoder_session(inputs.client, prepared.qsess);
    }
}

/// Main-thread half of HLS→Original recovery. The demux worker has already established, from
/// probes of the actual source file, that its uncertainty-discounted delivery estimate clears the
/// source's declared average consumption rate AND that the switch is worth its visible cost for the
/// playback that remains. Re-check the route and atomically retire the encoder identity before
/// changing any playback declaration.
#[cfg(any(test, feature = "test-support"))]
pub fn recover_auto_to_original(ps: &mut PlaybackSession, offset_secs: i64) -> Option<AutoOriginalReload> {
    let expected = worker_ticket();
    let cause = if quality() == Quality::Auto {
        RecoveryCause::Automatic
    } else {
        RecoveryCause::ManualOriginal
    };
    recover_auto_to_original_for(ps, &expected, offset_secs, cause)
}

/// Which Original route a recovery restores (issue #266).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecoveryFlavour {
    Direct,
    /// A codec-preserving remux carrying these enhancement params (`NONE` for a plain remux).
    Remux(plx_plex::plex::AudioEnhancements),
}

/// The enhancement a recovery to `candidate` should carry: the offer is evaluated against the
/// family of the route being BUILT (`candidate.feeds_part() ? Direct : Remux`), never the live one —
/// during an HLS recovery the live contract still says `FixedHls` (family `Other`, never offered)
/// and is only overwritten once the replacement is published.
fn recovery_route(ps: &PlaybackSession, candidate: &AutoOriginalCandidate) -> Option<EnhancementRoute> {
    // `pass`/`subtitle_effect`/`refused` are the live session's own facts, identical to `facts(ps)`;
    // only `base`/`carried` differ, because this candidate is not yet `ps.auto_original`/`cur_audio`.
    let facts = EnhancementFacts {
        base: Some(candidate),
        carried: candidate.audio.as_ref(),
        ..facts(ps)
    };
    // `feeds_part`, not `direct`: a candidate whose audio the server must convert is built as a
    // remux, so the offer is judged as one.
    let family = if candidate.feeds_part() {
        RouteFamily::Direct
    } else {
        RouteFamily::Remux
    };
    enhancement_route(&facts, family)
}

fn recovery_want(ps: &PlaybackSession, candidate: &AutoOriginalCandidate) -> plx_plex::plex::AudioEnhancements {
    desired_audio(crate::player::audio_enhancements(), recovery_route(ps, candidate).is_some())
}

/// **Pure: Direct only when the candidate direct-plays AND no enhancement is wanted.** An enhanced
/// Original is a remux by definition (M1), so a wanted enhancement turns a direct candidate into
/// the codec-preserving remux carrying the params — the same shape the resolve would have built.
pub(super) fn recovery_flavour(
    candidate: &AutoOriginalCandidate,
    want: plx_plex::plex::AudioEnhancements,
) -> RecoveryFlavour {
    if candidate.feeds_part() && !want.any() {
        RecoveryFlavour::Direct
    } else {
        RecoveryFlavour::Remux(want)
    }
}

/// **An Original recovery in three steps, so that the middle one — the only one that reaches PMS —
/// runs on a worker.** The frame thread plans ([`plan_original_recovery`]: every read of the
/// session, the ticket and the control state), the PMS half runs ([`run_original_recovery`]: the
/// Part admission probe, and the replacement remux's selection PUT + `/decision`), and the frame
/// thread installs the outcome ([`install_original_recovery`]: every write to the session and to the
/// `PendingOriginal` rollback). All three recoveries are flights ([`super::flight`]): an enhancement
/// release (`execute_retranscode_claim`'s `ClaimPrimary::ReleaseToDirect`), the viewer's Original
/// pick and the Auto watchdog's HLS-to-Original handoff (both [`execute_recover_original_claim`]).
/// This function is the three steps run in a row on the calling thread, and it exists only for the
/// host tests that grade the steps' joint outcome: no shipping caller is left, so it is not built
/// into the binary.
#[cfg(any(test, feature = "test-support"))]
pub fn recover_auto_to_original_for(
    ps: &mut PlaybackSession,
    expected: &WorkerTicket,
    offset_secs: i64,
    cause: RecoveryCause,
) -> Option<AutoOriginalReload> {
    let plan = plan_original_recovery(ps, expected, offset_secs, cause)?;
    let net = run_original_recovery(&plx_base::task::OffFrame::for_test(), &plan)?;
    install_original_recovery(ps, plan, net)
}

/// What an Original recovery's PMS half needs, owned: read off the session on the frame thread so
/// the worker that runs it never sees a `PlaybackSession` (see [`plan_original_recovery`]).
pub(super) struct OriginalRecoveryPlan {
    candidate: AutoOriginalCandidate,
    /// The route generation the recovery was planned against. Every commit below is gated on it.
    pub(super) expected: WorkerTicket,
    cause: RecoveryCause,
    offset_secs: i64,
    /// The Auto watchdog keeps watching an Original it put there itself, and an enhancement release
    /// inside an Auto playback — the viewer changed the audio, not the quality policy.
    watched: bool,
    flavour: RecoveryFlavour,
    /// Whether an enhancement this recovery carries is the subtitle-burning flavour.
    burn: bool,
    pub(super) client: Option<&'static plx_plex::plex::Client>,
    sid: ServerId,
    rk: String,
    part_id: i64,
    subtitle_sid: i64,
    /// The namespace the replacement encoder's session id is minted under.
    namespace: String,
    src_acodec: String,
}

impl OriginalRecoveryPlan {
    fn force_burn_for(&self, audio: plx_plex::plex::AudioEnhancements) -> bool {
        audio.any() && self.burn
    }

    /// Whether the PMS half will actually reach the server: a replacement remux's selection PUT and
    /// `/decision`, or a Part admission probe ([`admit_original_part`]). A direct trial the cause
    /// needs no admission for (an automatic recovery, whose Auto worker already sampled the Part)
    /// settles without a byte on the wire, so its flight lands within a frame and has no seconds
    /// to hold presentation for.
    fn reaches_pms(&self) -> bool {
        matches!(self.flavour, RecoveryFlavour::Remux(_))
            || (self.cause.needs_part_admission() && self.candidate.probe_part.starts_with('/'))
    }
}

/// The frame-thread half of an Original recovery: decide whether one is allowed and capture what
/// its PMS half will need. `None` is a refusal that touched nothing but the session's own live-HLS
/// mirror.
fn plan_original_recovery(
    ps: &mut PlaybackSession,
    expected: &WorkerTicket,
    offset_secs: i64,
    cause: RecoveryCause,
) -> Option<OriginalRecoveryPlan> {
    let automatic = cause == RecoveryCause::Automatic;
    if forced_direct_play(ps) { return None; }
    // One handoff owns both the unproven replacement and the retained client-side HLS route until
    // decoded frames commit it or an open failure restores that route snapshot. PMS-side HLS
    // cursor continuity is proved only by an actual later HLS response. Re-entering here would
    // replace that PendingOriginal, retire its only retained HLS identity, and leave the first
    // replacement ownerless while neither open had yet proved a frame.
    if original_recovery_pending() {
        return None;
    }
    let contract_allows = match cause {
        RecoveryCause::Automatic => applied_quality() == Quality::Auto,
        RecoveryCause::ManualOriginal => desired_quality() == Quality::Original,
        RecoveryCause::EnhancementReleased => {
            matches!(applied_quality(), Quality::Auto | Quality::Original)
        }
    };
    if !contract_allows || !is_transcoding(ps) {
        return None;
    }
    // The Auto watchdog keeps watching an Original it put there itself, and an enhancement
    // release inside an Auto playback — the viewer changed the audio, not the quality policy.
    let watched = automatic
        || (cause == RecoveryCause::EnhancementReleased && applied_quality() == Quality::Auto);
    let candidate = ps.auto_original.clone()?;
    let route = recovery_route(ps, &candidate);
    let flavour = recovery_flavour(&candidate, recovery_want(ps, &candidate));
    // The worker may have committed several HLS encoders since the main-thread plan was installed.
    // Mirror the physical route into the session before replacing it, otherwise the rollback pairs
    // the newest encoder id with the bootstrap URL/rung and reopens different media at a different
    // position.
    if !is_worker_ticket_current(expected) {
        return None;
    }
    let live_hls = sync_active_hls_to_session(ps);
    if live_hls
        .as_ref()
        .is_some_and(|(ticket, _)| ticket != expected)
    {
        return None;
    }
    if expected.encoder().is_empty() {
        return None;
    }
    // The replacement must have its own exact physical/resource identity. Reusing `sess()` can
    // equal the initial HLS encoder and would mutate the very rollback this handoff promises to
    // retain; a fresh child also makes a failed remux safe to stop without touching HLS.
    let logical_session = sess(ps);
    let namespace = if logical_session.is_empty() {
        expected.encoder().to_owned()
    } else {
        logical_session
    };
    Some(OriginalRecoveryPlan {
        flavour,
        burn: matches!(route, Some(EnhancementRoute::Burn)),
        client: cur_client(ps),
        sid: cur_sid(ps),
        rk: cur_rk(ps),
        part_id: cur_part_id(ps),
            subtitle_sid: cur_sub_sid(ps),
        namespace,
        src_acodec: ps.src_acodec.clone(),
        candidate,
        expected: expected.clone(),
        cause,
        offset_secs,
        watched,
    })
}

/// What an Original recovery's PMS half settled on.
pub(super) enum OriginalRecoveryNet {
    /// The candidate's raw Part opens as the trial; `EnhancementOutcome` is what that direct play
    /// settles on ([`recover_original_direct`]'s `outcome`).
    Direct(EnhancementOutcome),
    /// A remux replacement the PMS half registered AND committed as the route's active encoder.
    Remux(PreparedOriginalRemux),
}

/// What the PMS half of an Original remux produced, ready for [`install_original_remux`].
pub(super) struct PreparedOriginalRemux {
    pub(super) replacement: String,
    url: String,
    audio: plx_plex::plex::AudioEnhancements,
    force_burn: bool,
    vcodec: String,
    acodec: String,
    enhancement: EnhancementOutcome,
    /// The route's ticket as the PMS half's own commit left it; the install re-checks it, because
    /// when the PMS half ran on a worker the route may have moved again before the landing drained.
    ticket: WorkerTicket,
}

/// The PMS half of an Original recovery — everything that blocks on the server and nothing that
/// touches a `PlaybackSession`, so it runs on a worker (`_off` is the proof: the frame thread
/// cannot mint one). The only route state it writes is the remux's `replace_active_encoder_for`
/// commit, which is gated on `plan.expected`.
pub(super) fn run_original_recovery(
    off: &plx_base::task::OffFrame,
    plan: &OriginalRecoveryPlan,
) -> Option<OriginalRecoveryNet> {
    // A direct trial is only worth starting on a Part the server will actually serve this
    // identity. One the server refuses is reached as its codec-copy remux instead — the shape the
    // resolve builds whenever the server will not direct-play — rather than opened, failed and
    // rolled back to the route being left (see [`admit_original_part`]).
    let flavour = match plan.flavour {
        RecoveryFlavour::Direct => match admit_or_plain_remux(plan) {
            AdmitOrPlainRemux::Admitted => RecoveryFlavour::Direct,
            AdmitOrPlainRemux::PlainRemux => {
                RecoveryFlavour::Remux(plx_plex::plex::AudioEnhancements::NONE)
            }
        },
        remux => remux,
    };
    let RecoveryFlavour::Remux(audio) = flavour else {
        return Some(OriginalRecoveryNet::Direct(EnhancementOutcome::Off));
    };
    // `/decision` only registers the replacement. Just like a raw Part open, it does not prove
    // that Starfish can read and decode the resulting MKV. The install publishes the remux without
    // stopping the old HLS encoder, then puts both exact identities in PendingOriginal; decoded
    // frames retire HLS, while a failed open restores its client-side route snapshot and retires
    // this unproven remux. Only the next HLS response establishes PMS-side cursor continuity.
    match prepare_original_remux(off, plan, audio, plan.force_burn_for(audio), false)? {
        OriginalRemux::Prepared(prepared) => Some(OriginalRecoveryNet::Remux(prepared)),
        // The server will not apply the params and the candidate direct-plays: the plain
        // Original IS that direct play, exactly as the resolve's own fallback returns to it —
        // when the server will serve its Part; otherwise the plain remux of the same Original.
        OriginalRemux::RefusedToDirect => match admit_or_plain_remux(plan) {
            AdmitOrPlainRemux::Admitted => {
                Some(OriginalRecoveryNet::Direct(EnhancementOutcome::Refused))
            }
            AdmitOrPlainRemux::PlainRemux => {
                let none = plx_plex::plex::AudioEnhancements::NONE;
                match prepare_original_remux(off, plan, none, false, true)? {
                    OriginalRemux::Prepared(prepared) => Some(OriginalRecoveryNet::Remux(prepared)),
                    OriginalRemux::RefusedToDirect => None,
                }
            }
        },
    }
}

/// The frame-thread half that lands a settled Original recovery: take the rollback snapshot of the
/// route as it stands the instant before it is overwritten, then write the replacement and arm the
/// `PendingOriginal`. `None` when the route moved after the PMS half committed (a stale landing):
/// nothing was written, and an encoder the PMS half registered is stopped — the old one too, since
/// no `PendingOriginal` is going to own it.
pub(super) fn install_original_recovery(
    ps: &mut PlaybackSession,
    plan: OriginalRecoveryPlan,
    net: OriginalRecoveryNet,
) -> Option<AutoOriginalReload> {
    let automatic = plan.cause == RecoveryCause::Automatic;
    let mut rollback = snapshot_route(ps, plan.expected.encoder().to_owned(), plan.offset_secs);
    let prepared = match net {
        OriginalRecoveryNet::Direct(outcome) => {
            return recover_original_direct(
                ps,
                &plan.candidate,
                &plan.expected,
                rollback,
                plan.watched,
                plan.cause,
                outcome,
            );
        }
        OriginalRecoveryNet::Remux(prepared) => prepared,
    };
    if !is_worker_ticket_current(&prepared.ticket) {
        if let Some(client) = plan.client {
            stop_encoder_session(client, prepared.replacement);
            stop_encoder_session(client, plan.expected.encoder().to_owned());
        }
        return None;
    }
    install_original_remux(ps, &plan.candidate, plan.watched, &prepared);
    rollback.replacement_encoder = prepared.replacement;
    set_pending_original(ps, rollback, automatic);
    crate::player::clear_original_failure();
    crate::player::log(match plan.cause {
        RecoveryCause::Automatic => "auto: recovered Original remux; HLS encoder held pending frames",
        RecoveryCause::ManualOriginal => {
            "quality: Original restored remux; HLS encoder held pending frames"
        }
        RecoveryCause::EnhancementReleased => {
            "enhancement: released to Original remux; previous encoder held pending frames"
        }
    });
    crate::player::report::note_delivery_requested_for(
        playback_trace_generation(),
        crate::player::report::DeliveryClass::Remux,
        crate::player::report::QualityClass::Original,
        crate::player::report::DeliveryReason::OriginalRecovery,
    );
    Some(AutoOriginalReload::Remux)
}

/// Whether the server will serve an Original's raw Part to this playback right now.
pub(super) enum PartAdmission {
    Admitted,
    /// Why not, for the log: the HTTP status, or the transport failure that stood for one.
    Refused(String),
}

/// Bytes the admission reads before it lets go: enough to prove a body arrives, nothing more.
const PART_ADMISSION_BYTES: usize = 16 * 1024;
/// Header and body budget, each. The claim this answers runs in the recovery's PMS half beside a
/// `/decision` call, on the flight's worker, so it is bounded well under the API timeout that call
/// carries.
const PART_ADMISSION_BUDGET: std::time::Duration = std::time::Duration::from_millis(1500);

/// **Ask the server for the Original's Part on the exact identity the trial would open it on,
/// before the trial is started.** A recovery used to publish the raw Part as an unproven trial
/// and let the demuxer find out: PR 4's device run (PMS 1.43.4, issue #266) released an enhanced
/// remux to its direct play, the Part came back **503**, and the rollback restored the enhanced
/// route — the only route `PendingOriginal` holds — under a preference, a menu and a log line that
/// all said the enhancement was off. A manual Original and an enhancement release did not ask at
/// all. Automatic still does not ask here: the Auto watchdog already sampled this exact Part on
/// the same identity on its own worker thread before proposing the recovery
/// (`probe_original_while_hls_cancellable`), so a second main-thread admission would only add its
/// own budget as UI/feed block for a cause that already has its answer (`RecoveryCause::
/// needs_part_admission`).
///
/// What PMS keys that 503 on did not reproduce from a host against the same server with the
/// same identity (every Part GET after an enhanced remux decision, with the encoder held,
/// stopped physically, stopped with `closeResourceSession=1`, or abandoned, answered 200/206), so
/// this reads the answer rather than predicting it. **It does not re-register an MDE first:**
/// measured on that server, an MDE `/decision` on a session with a live encoder ends that encoder
/// (HLS segments 404, a later `start.mkv` without a fresh decision 400) — which is precisely the
/// route a failed trial must be able to return to.
///
/// A fixture candidate (`probe_part` not a server path) has nothing to ask and is admitted, and so
/// is a Part the server did not answer inside the budget, or one whose connection failed mid-body
/// after a known status (`ThroughputFailure::BodyRead`): the admission bounds how long the claim
/// can hold the pump, and only the server's OWN answer — a refusal status, or headers with no body
/// behind them — is a refusal; a transport failure is not.
fn admit_original_part(plan: &OriginalRecoveryPlan) -> PartAdmission {
    if !plan.cause.needs_part_admission() || !plan.candidate.probe_part.starts_with('/') {
        return PartAdmission::Admitted;
    }
    let Some(client) = plan.client else {
        return PartAdmission::Refused("no client for this server".into());
    };
    let url = client
        .direct_play_url(&plan.candidate.probe_part, plan.expected.encoder())
        .to_url();
    use crate::curlio::{OpenErr, ThroughputFailure};
    match crate::curlio::sample_throughput_result(
        &url,
        PART_ADMISSION_BYTES,
        PART_ADMISSION_BUDGET,
        PART_ADMISSION_BUDGET,
    ) {
        // Only the server's own answer refuses: a status it will not stream, or headers with no
        // body behind them.
        Err(ThroughputFailure::Open(OpenErr::Status(status))) => {
            PartAdmission::Refused(format!("HTTP {status}"))
        }
        Err(failure @ ThroughputFailure::NoBody { .. }) => {
            PartAdmission::Refused(format!("{failure:?}"))
        }
        // A body, a transport failure mid-read (`BodyRead`: the server answered with a status and
        // then the connection died before delivering it), or no answer inside the budget (a slow
        // server, no bounded client on this build): nothing here is the server's OWN refusal, so
        // the trial's own open — with its rollback — stays the judge, exactly as before this
        // admission existed.
        Ok(_) | Err(_) => PartAdmission::Admitted,
    }
}

/// [`admit_or_plain_remux`]'s answer: either the Part is admitted and the caller's own plan
/// stands, or it is not and the caller falls back to the plain (unenhanced) remux — the same
/// shape the resolve itself builds whenever the server will not serve the Part.
enum AdmitOrPlainRemux {
    Admitted,
    PlainRemux,
}

/// Ask the server whether it will serve the Original's Part, and log the shared refusal sentence
/// once for both call sites that reach a direct trial only to find it will not
/// ([`run_original_recovery`]'s initial `RecoveryFlavour::Direct` arm, and its retry after
/// [`prepare_original_remux`] itself reports `RefusedToDirect`).
fn admit_or_plain_remux(plan: &OriginalRecoveryPlan) -> AdmitOrPlainRemux {
    match admit_original_part(plan) {
        PartAdmission::Admitted => AdmitOrPlainRemux::Admitted,
        PartAdmission::Refused(why) => {
            crate::player::log(&format!(
                "{}: server refused the Original Part ({why}); restoring Original as a remux",
                plan.cause.log_tag()
            ));
            AdmitOrPlainRemux::PlainRemux
        }
    }
}

/// The direct-play half of an Original recovery (the candidate's raw Part). `outcome` is the
/// [`EnhancementOutcome`] this direct play settles on: `Off` when no enhancement was in play,
/// `Refused` when an enhanced remux was asked for first and the server would not apply it, so the
/// offer stays withdrawn for this playback (the resolve's and the preflight's `Refused`).
fn recover_original_direct(
    ps: &mut PlaybackSession,
    candidate: &AutoOriginalCandidate,
    expected: &WorkerTicket,
    rollback: PendingOriginal,
    watched: bool,
    cause: RecoveryCause,
    outcome: EnhancementOutcome,
) -> Option<AutoOriginalReload> {
    let automatic = cause == RecoveryCause::Automatic;
    let expected_encoder = expected.encoder();
    // The probe and the actual Part body must name the same exact Streaming Resource. A URL
    // left on the logical playback id can token-alias this HLS resource today, then fail a
    // later seek after cleanup because the alias choice is not durable.
    let source_url = if candidate.probe_part.starts_with('/') {
        let client = cur_client(ps)?;
        client
            .direct_play_url(&candidate.probe_part, expected_encoder)
            .to_url()
    } else {
        candidate.url.clone()
    };
    // Keep the exact id as a source-resource owner, but remove its HLS route projection. On
    // decoded frames confirmation stops only the physical encoder; final teardown takes this
    // id and performs the full resource close.
    replace_active_encoder_for(expected, expected_encoder)?;
    // **Taken before anything is overwritten.** A raw Part request has no replacement
    // encoder; the empty marker tells rollback there is nothing new to retire.
    { let s = &mut *ps; {
        s.url = source_url;
        s.tsession.clear();
        s.cur_contract = plx_plex::plex::EncodeContract::original(false, plx_plex::plex::AudioEnhancements::NONE);
        s.cur_enhancement = outcome;
        s.cur_auto_original_watched = watched;
        s.cur_audio = candidate.audio.clone();
        s.stream_vcodec = candidate.vcodec.clone();
        // A server-default candidate carries no track facts; the file's own default codec
        // (`src_acodec`, the resolve's source argument) is what that direct play decodes.
        s.stream_acodec = candidate
            .audio
            .as_ref()
            .map_or_else(|| s.src_acodec.clone(), |a| a.codec.clone());
        s.stream_fps = candidate.fps;
        s.stream_dovi = candidate.dovi;
        s.stream_dv_decision = candidate.dv_decision;
        s.stream_immersive = candidate.audio.as_ref().is_some_and(|a| a.immersive);
    } };
    crate::player::set_audio_track(
        candidate.audio.as_ref().map_or(-1, |a| a.ordinal),
    );
    crate::player::request_subtitle(candidate.subtitle_ordinal.unwrap_or(-1));
    set_pending_original(ps, rollback, automatic);
    // This is a new source attempt. A prior probe's typed failure explains the HLS route we
    // are leaving, not the replacement now being opened; a failure of this open republishes
    // its own exact status from the pump.
    crate::player::clear_original_failure();
    crate::player::log(match cause {
        RecoveryCause::Automatic => {
            "auto: recovered Original direct play; HLS encoder held pending frames"
        }
        RecoveryCause::ManualOriginal => {
            "quality: Original restored direct play; HLS encoder held pending frames"
        }
        RecoveryCause::EnhancementReleased => {
            "enhancement: released to Original direct play; remux encoder held pending frames"
        }
    });
    crate::player::report::note_delivery_requested_for(
        playback_trace_generation(),
        crate::player::report::DeliveryClass::Direct,
        crate::player::report::QualityClass::Original,
        crate::player::report::DeliveryReason::OriginalRecovery,
    );
    Some(AutoOriginalReload::Direct)
}

/// The route as it stood the instant before an Original recovery overwrote it, kept so the
/// recovery can be UNDONE.
///
/// **A recovery is not proven by the evidence that authorised it.** The demux worker probes the
/// source file and the probes clear the requirement; that is a claim about a byte range fetched
/// seconds ago, not about the fresh open the pipeline is about to perform. Device, 2026-08-29:
/// this server had already answered **503** to an Original probe forty seconds earlier while the
/// HLS segments beside it kept succeeding, and when the viewer then asked for Original by hand the
/// open failed the same way. The recovery had already cleared `tsession`, cleared the active
/// encoder and asked the server to stop the encoder — so the working stream the viewer had been
/// watching no longer existed, and the pump had nothing to do but raise the failure read-out.
///
/// So the two irreversible client steps are DEFERRED behind this: the explicit server-side stop,
/// and retiring the route snapshot. [`confirm_original_recovery`] performs them once the new source
/// has actually delivered frames; [`rollback_original_recovery`] restores that snapshot if it
/// never does. Restoration makes no claim about PMS's cursor until a new HLS response arrives.
struct PendingOriginal {
    /// Applied reducer state to restore if the unproven native Load never produces a frame.
    previous_applied_revision: u64,
    previous_applied_quality: Quality,
    previous_applied_projection: Option<AppliedRouteProjection>,
    /// Complete candidate projection at the instant the trial starts. A later user command may
    /// stage fields in Session while native frames are pending; first-frame commit must not bless
    /// those still-unapplied edits along with the candidate.
    candidate_projection: AppliedRouteProjection,
    /// The HLS encoder the recovery replaced. The client has not explicitly stopped it; PMS-side
    /// cursor continuity is deliberately not inferred from that fact.
    encoder: String,
    /// The new server encoder to retire if this handoff never produces a decoded frame. Empty for
    /// direct play, which opens the raw Part and creates no universal-transcoder replacement.
    replacement_encoder: String,
    /// Where to resume the restored route. Kept here rather than read back from `playpos_ns`
    /// because `teardown(for_reload=true)` zeroes that on the way into the reload being graded, so
    /// by the time the failure is detected the playhead no longer remembers where the film was.
    offset_secs: i64,
    /// The route as it stood the instant before this recovery overwrote it. Restored verbatim
    /// through [`install_route_projection`] on rollback, except `subtitle_sid`/`auto_original`:
    /// a client-side edit made while the trial owned the route is not part of what rollback
    /// undoes (it travels with `candidate_projection` instead — see
    /// `commit_in_place_route_projection`), so the restore site puts those two fields back the
    /// way it found them.
    previous: AppliedRouteProjection,
    /// A manual Original pick can adopt an automatic trial without issuing a second Load. The
    /// first decoded frame then transfers the applied contract to Manual and invalidates the
    /// Auto worker ticket which was captured when the trial started.
    adopted_by_user: bool,
    /// Anti-flap history prices visible mode changes, not requested Loads. An automatic Original
    /// trial earns this charge only when decoded frames commit it; rollback drops it unspent.
    charge_visible_switch_on_commit: bool,
    /// User commands made while neither the candidate nor its rollback route is yet authoritative.
    /// They travel with this exact transaction and are applied only after a replacement Engine is
    /// proven; a terminal failure drops them rather than leaking them into a later trial.
    deferred_quality: Option<Quality>,
    deferred_audio: Option<CarriedAudio>,
    /// Issue #266: the enhancement preference changed while this trial owned the route. The
    /// reconcile it asked for runs against whichever route the trial settles on — the candidate
    /// or its rollback — never against the unproven half-state in between.
    deferred_reconcile: bool,
}

#[derive(Default)]
pub struct DeferredOriginalEffects {
    quality: Option<Quality>,
    audio: Option<CarriedAudio>,
    reconcile: bool,
}

impl DeferredOriginalEffects {
    fn from_pending(pending: &mut PendingOriginal) -> Self {
        Self {
            quality: pending.deferred_quality.take(),
            audio: pending.deferred_audio.take(),
            reconcile: std::mem::take(&mut pending.deferred_reconcile),
        }
    }

    fn is_empty(&self) -> bool {
        self.quality.is_none() && self.audio.is_none() && !self.reconcile
    }
}

pub struct OriginalRollback {
    pub offset_ns: i64,
}

impl OriginalRollback {
    pub fn without_deferred(offset_ns: i64) -> Self {
        Self { offset_ns }
    }
}

fn snapshot_route(ps: &PlaybackSession, encoder: String, offset_secs: i64) -> PendingOriginal {
    let s = &*ps;
    PendingOriginal {
        previous_applied_revision: 0,
        previous_applied_quality: Quality::Original,
        previous_applied_projection: None,
        // Replaced atomically by `set_pending_original` after the candidate route is installed.
        candidate_projection: route_projection(ps),
        encoder,
        replacement_encoder: String::new(),
        offset_secs,
        previous: route_projection(s),
        adopted_by_user: false,
        charge_visible_switch_on_commit: false,
        deferred_quality: None,
        deferred_audio: None,
        deferred_reconcile: false,
    }
}

/// Install the way back. A displaced one is RETIRED rather than dropped: its encoder is still
/// running on somebody's server, and the route it belonged to is two recoveries stale.
fn set_pending_original(ps: &PlaybackSession, mut pending: PendingOriginal, automatic: bool) {
    let candidate_projection = route_projection(ps);
    pending.charge_visible_switch_on_commit = automatic;
    let displaced = {
        let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        // The candidate is staged in Session, but its Original Load is not yet proven. Retain the
        // current HLS applied projection as the rollback owner, enter OriginalTrial::Prepared, and
        // publish the candidate projection only after decoded-frame confirmation.
        pending.previous_applied_revision = control.applied_revision;
        pending.previous_applied_quality = control.applied_quality;
        pending.previous_applied_projection = control.applied_projection.clone();
        pending.candidate_projection = candidate_projection;
        if !automatic {
            control.applied_revision = control.desired_revision;
            control.applied_quality = control.desired_quality;
        }
        let serial = match control.phase {
            ControlPhase::Applying(serial)
            | ControlPhase::Prepared(serial)
            | ControlPhase::Starting(serial, _) => serial,
            _ => {
                control.next_action = next_generation(control.next_action);
                control.next_action
            }
        };
        control.phase = ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial));
        control.pending_original.replace(pending)
    };
    if let Some(old) = displaced {
        retire_replaced_encoder(ps, old.encoder);
    }
}

fn take_pending_original() -> Option<PendingOriginal> {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    control.pending_original.take()
}

/// Is an Original recovery still waiting to be proven? The pump asks before spending a frame on
/// either half below.
pub fn original_recovery_pending() -> bool {
    PLAYER_CONTROL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pending_original
        .is_some()
}

/// **The new source delivered.** Make the recovery permanent: drop the way back and ask the server
/// to stop the encoder that is still running behind it.
///
/// The pump calls this on decoded frames rather than on `loadCompleted`, because the question the
/// deferral exists to answer is whether the SOURCE delivers — and a Load the pipeline accepted is
/// an acknowledgement of a payload declaration, not of a byte having arrived.
pub fn confirm_original_recovery(ps: &mut PlaybackSession) {
    let current_projection = route_projection(ps);
    let (mut pending, serial, use_current_projection) = {
        let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        let ControlPhase::OriginalTrial(OriginalTrialPhase::AwaitingFrame(serial)) = control.phase
        else {
            return;
        };
        let Some(pending) = control.pending_original.take() else {
            return;
        };
        let use_current =
            control.desired_revision == control.applied_revision && control.pending_user.is_none();
        control.phase = ControlPhase::Completing(serial);
        (pending, serial, use_current)
    };
    // If no user contract was staged during the trial, immediate client-only edits (notably a
    // direct-play subtitle renderer change) are already applied and current Session is truthful.
    // Otherwise commit exactly the candidate snapshot and leave the staged Session fields for the
    // queued action; its rejection will restore this candidate rather than blessing the proposal.
    let mut committed_projection = if use_current_projection {
        current_projection
    } else {
        pending.candidate_projection.clone()
    };
    let deferred = DeferredOriginalEffects::from_pending(&mut pending);
    if pending.adopted_by_user {
        committed_projection.auto_original_watched = false;
    }
    {
        let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        if control.phase != ControlPhase::Completing(serial) {
            return;
        }
        if pending.adopted_by_user {
            control.applied_revision = control.desired_revision;
            control.applied_quality = Quality::Original;
        }
        control.applied_projection = Some(committed_projection);
    }
    if pending.charge_visible_switch_on_commit {
        note_visible_switch(ps, ps.now_ms);
    }
    if pending.replacement_encoder.is_empty() {
        crate::player::log(
            "abr: direct Original confirmed by decoded frames; stopping HLS encoder and retaining source resource",
        );
        retire_hls_encoder_keep_source(ps, pending.encoder);
    } else {
        crate::player::log(
            "abr: remux Original confirmed by decoded frames; retiring old HLS resource",
        );
        retire_replaced_encoder(ps, pending.encoder);
    }
    apply_deferred_original_effects(ps, deferred);
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if control.phase == ControlPhase::Completing(serial) {
        // This is the last reducer publication: workers cannot observe Stable before the applied
        // projection and every trial-attached user command have crossed into reducer ownership.
        control.phase = ControlPhase::Stable;
    }
}

/// **The new source never delivered.** Restore the HLS projection as a `Prepared` candidate and
/// return its offset. The caller must rebase it ([`dispatch_rollback_rebase`] on the pump's frames,
/// [`dispatch_engineless_rollback_rebase`] where there is no Engine to drive a flight), claim a fresh exact Load
/// attempt and settle that attempt before `Stable`; this bookkeeping operation alone says nothing
/// about PMS cursor continuity. Returns `None` when there is nothing pending, in which case every
/// failure in the pump still means exactly what it always did.
pub fn rollback_original_recovery(ps: &mut PlaybackSession) -> Option<OriginalRollback> {
    let (mut pending, trial_serial) = {
        let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        let serial = match control.phase {
            ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial))
            | ControlPhase::OriginalTrial(OriginalTrialPhase::Starting(serial, _))
            | ControlPhase::OriginalTrial(OriginalTrialPhase::AwaitingFrame(serial))
            | ControlPhase::OriginalTrial(OriginalTrialPhase::Failed(serial)) => serial,
            _ => return None,
        };
        (control.pending_original.take()?, serial)
    };
    let deferred = DeferredOriginalEffects::from_pending(&mut pending);
    let failed_replacement = pending.replacement_encoder.clone();
    let restored_hls = match (
        pending.previous.contract.delivery,
        pending
            .previous
            .contract
            .ceiling
            .and_then(crate::abr::Rung::from_ceiling),
    ) {
        (plx_plex::plex::TranscodeDelivery::FixedHls { .. }, Some(rung)) => Some(rung),
        _ => None,
    };
    // `subtitle_sid`/`auto_original` are not part of what a rollback undoes (see the doc comment
    // on `PendingOriginal::previous`): put back whatever the trial left there.
    let kept_subtitle_sid = ps.cur_sub_sid;
    let kept_subtitle_sidecar = ps.cur_sub_sidecar;
    let kept_subtitle_drawable = ps.cur_sub_client_drawable;
    let kept_auto_original = ps.auto_original.clone();
    install_route_projection(ps, &pending.previous);
    ps.cur_sub_sid = kept_subtitle_sid;
    ps.cur_sub_sidecar = kept_subtitle_sidecar;
    ps.cur_sub_client_drawable = kept_subtitle_drawable;
    ps.auto_original = kept_auto_original;
    if let Some(rung) = restored_hls {
        install_active_hls(&pending.encoder, &pending.previous.url, rung);
    } else {
        install_active_encoder(&pending.encoder);
    }
    {
        let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(
            control.phase,
            ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial))
                | ControlPhase::OriginalTrial(OriginalTrialPhase::Starting(serial, _))
                | ControlPhase::OriginalTrial(OriginalTrialPhase::AwaitingFrame(serial))
                | ControlPhase::OriginalTrial(OriginalTrialPhase::Failed(serial))
                if serial == trial_serial
        ) {
            return None;
        }
        control.applied_revision = pending.previous_applied_revision;
        control.applied_quality = pending.previous_applied_quality;
        control.applied_projection = pending.previous_applied_projection.clone();
        // Session + active route + applied contract are now one restored *candidate*. The held
        // HLS cursor still has to be rebased and Loaded; keep every worker blocked across that
        // preparation and let the matching physical Load attempt publish Stable only on success.
        control.next_action = next_generation(control.next_action);
        let serial = control.next_action;
        control.phase = ControlPhase::Prepared(serial);
        control.start_deferred = (!deferred.is_empty()).then_some((serial, deferred));
    }
    if !failed_replacement.is_empty() && failed_replacement != pending.encoder {
        retire_replaced_encoder(ps, failed_replacement);
    }
    crate::player::log(&format!(
        "abr: Original recovery failed to open; restored HLS encoder={} at {}s",
        pending.encoder, pending.offset_secs,
    ));
    Some(OriginalRollback {
        offset_ns: pending.offset_secs.max(0) * 1_000_000_000,
    })
}

/// **Abandon the way back without taking it**, for the paths that make it meaningless: a new item,
/// a teardown, or a quality change that supersedes the recovery. The encoder is retired, because
/// the route it belonged to is gone either way and leaving it running is a session leaked on
/// somebody's server.
pub fn drop_original_recovery(ps: &PlaybackSession) {
    if let Some(pending) = take_pending_original() {
        // The only caller is real teardown, immediately after `scrobble_stop` took the active
        // identity. During a direct handoff that identity IS `pending.encoder`, so scrobble owns
        // its one full stop/resource close. During a remux handoff the active identity is the new
        // replacement; scrobble closes that one and this branch still owes the held old HLS.
        if !pending.replacement_encoder.is_empty() {
            retire_replaced_encoder(ps, pending.encoder);
        }
    }
}

fn retire_replaced_encoder(ps: &PlaybackSession, encoder: String) {
    if encoder.is_empty() || !ps.auto_fixture_base.is_empty() {
        return;
    }
    let Some(client) = cur_client(ps) else { return };
    plx_base::task::spawn_small_or_inline(
        "abr-original-stop",
        const { &plx_base::task::BlockingLabel::new("encoder stop (worker thread refused)") },
        move || {
            let ok = client.transcode_stop(&encoder);
            crate::player::log(&format!(
                "abr: retired superseded encoder after Original handoff ok={}",
                ok as i32
            ));
        },
    );
}

/// The raw Part already exact-reuses `encoder`'s Streaming Resource. Stop only the physical HLS
/// producer; keeping the resource alive is what lets the current body and every later seek remain
/// admitted. [`scrobble_stop`] still owns the id in `ACTIVE_ENCODER` and closes it at teardown.
fn retire_hls_encoder_keep_source(ps: &PlaybackSession, encoder: String) {
    if encoder.is_empty() || !ps.auto_fixture_base.is_empty() {
        return;
    }
    let Some(client) = cur_client(ps) else { return };
    plx_base::task::spawn_small_or_inline(
        "abr-original-physical-stop",
        const { &plx_base::task::BlockingLabel::new("encoder physical stop (worker thread refused)") },
        move || {
            let ok = client.transcode_stop_physical(&encoder);
            crate::player::log(&format!(
                "abr: stopped HLS encoder while retaining Original resource ok={}",
                ok as i32
            ));
        },
    );
}
/// true while this playback is a server transcode (a live transcode session exists). Cheap
/// in-place check — the pump polls it every tick, so no String clone here.
pub fn is_transcoding(ps: &PlaybackSession) -> bool {
    !ps.tsession.is_empty()
}
/// Did the server REFUSE this item at `/decision`, before playback? Cheap in-place check —
/// `player::state()` derives `Error` from it on every frame of the player route.
pub fn play_refused(ps: &PlaybackSession) -> bool {
    ps.play_verdict.is_some()
}
/// The app/worker failed to produce a playable plan, as distinct from a PMS `/decision` refusal.
/// There is no Engine whose pump could publish Error, so `player::state()` derives it beside the
/// refusal case.
pub fn play_resolution_failed(ps: &PlaybackSession) -> bool {
    ps.resolve_failed
}
/// The refusal's own sentence for the read-out to quote — `None` when the server did not refuse,
/// `Some("")` when it refused without saying why. MAIN THREAD (see [`PlaybackSession::play_verdict`]).
///
/// Borrowed, not cloned: the read-out asks for this 2–3× on every frame of a failure (the HUD
/// caption, the read-out itself, and the diagnostics panel when it is open), and every one of them
/// only reads it. The borrow lives until the next main-thread write, which is `apply_plan` or
/// `request_play` — neither of which can run inside a frame's draw.
pub fn play_verdict(ps: &PlaybackSession) -> Option<&str> {
    ps.play_verdict.as_ref().map(PlayVerdict::text)
}
/// The verdict NUMBERS of a refusal the SERVER made at `/decision` — `None` when nothing was
/// refused, or when the refusal was the app's own (Direct Play off, Force Direct Play): those carry
/// no server codes because the server was never asked to decide. Numbers only; the sentence is
/// [`play_verdict`], and it never reaches a report. MAIN THREAD.
pub fn server_refusal_codes(ps: &PlaybackSession) -> Option<DecisionCodes> {
    match ps.play_verdict {
        Some(PlayVerdict::Server(_, codes)) => Some(codes),
        _ => None,
    }
}
/// Retire the refusal — "this playback request is withdrawn", the one thing besides a fresh
/// resolve that ends a verdict's life. [`request_play`] clears it because a NEW item is being
/// resolved; this is the other half, for leaving the player entirely.
///
/// Without it a refusal outlived the player: `player::state()` derives `Error` from this field
/// and takes no route, so a verdict left standing described the item the user walked away from —
/// on Home, in the Library, on any detail page — until they happened to start something else.
fn clear_play_verdict(ps: &mut PlaybackSession) {
    { let s = &mut *ps; {
        s.play_verdict = None;
        s.jail_load_blocked = false;
        s.resolve_failed = false;
        s.requested_resume_ns = 0;
    } }
}
/// Test-only twin of [`clear_play_verdict`], for a test whose assertion presumes "no session":
/// `player::state()` derives `Error` from these fields, and a test that exercised a refusal on the
/// SAME session value may have left one standing. It no longer needs `plx_base::testlock::serial()`
/// for this reason — since phase 9 a test owns its session outright and cannot leave a refusal in
/// anybody else's.
#[cfg(any(test, feature = "test-support"))]
pub fn clear_play_verdict_for_test(ps: &mut PlaybackSession) {
    clear_play_verdict(ps)
}
/// Test-only: leave `ps` exactly as a refusing [`apply_plan`] does — the verdict installed, no URL,
/// no encoder session. For asserting what the failure report says about a playback that never got
/// a route.
#[cfg(any(test, feature = "test-support"))]
pub fn refuse_for_test(ps: &mut PlaybackSession, verdict: PlayVerdict) {
    ps.play_verdict = Some(verdict);
}
/// Test-only: leave `ps` as a refusing [`apply_plan`] does after the SERVER refused a transcode —
/// the verdict and its codes installed, the attempted route's contract and the source file's
/// codecs recorded, and still no URL and no encoder session.
#[cfg(any(test, feature = "test-support"))]
pub fn refuse_by_server_for_test(
    ps: &mut PlaybackSession,
    sentence: &str,
    codes: DecisionCodes,
    remux: bool,
    hls: bool,
    src_vcodec: &str,
    src_acodec: &str,
) {
    ps.play_verdict = Some(PlayVerdict::Server(sentence.to_owned(), codes));
    ps.cur_contract.remux = remux;
    ps.cur_contract.delivery = if hls {
        plx_plex::plex::TranscodeDelivery::FixedHls { seconds_per_segment: 2 }
    } else {
        plx_plex::plex::TranscodeDelivery::ProgressiveMkv
    };
    ps.src_vcodec = src_vcodec.to_owned();
    ps.src_acodec = src_acodec.to_owned();
}
/// Test-only: install a live transcode route (`encoder` session, optional remux, optional
/// fixed-HLS delivery) the way a successful [`apply_plan`] leaves it.
#[cfg(any(test, feature = "test-support"))]
pub fn install_transcode_for_test(ps: &mut PlaybackSession, remux: bool, hls: bool) {
    ps.tsession = "test-encoder".to_owned();
    ps.cur_contract.remux = remux;
    ps.cur_contract.delivery = if hls {
        plx_plex::plex::TranscodeDelivery::FixedHls { seconds_per_segment: 2 }
    } else {
        plx_plex::plex::TranscodeDelivery::ProgressiveMkv
    };
}
/// select the subtitle to BURN into any transcode of the current item (0 = none). This
/// is the transcode path; direct-play uses the client renderer (player::request_subtitle).
pub fn set_subtitle(ps: &mut PlaybackSession, sid: i64) {
    { let s = &mut *ps; s.cur_sub_sid = sid }
}
/// the subtitle stream id currently burned into the transcode (0 = none).
pub fn cur_sub_sid(ps: &PlaybackSession) -> i64 {
    ps.cur_sub_sid
}
/// The subtitle-language preference this play resolved under (show pref, else account pref), as
/// a BCP-47 code — `metadata::sub_layout::sub_sections`'s "yours" grouping reads this, never a Plex
/// account type.
pub fn cur_sub_pref_lang(ps: &PlaybackSession) -> Option<&str> {
    ps.cur_sub_pref_lang.as_deref()
}
/// ratingKey of the currently-playing item (for /:/timeline progress reports).
pub fn cur_rk(ps: &PlaybackSession) -> String {
    ps.cur_rk.clone()
}
/// The server the currently-playing item came from — see [`PlaybackSession::cur_sid`]. MAIN THREAD.
///
/// A worker must be handed this by value at its spawn site, never call it: read on a worker it is
/// "whatever is playing now", which is the very race capturing the id was meant to end.
pub fn cur_sid(ps: &PlaybackSession) -> ServerId {
    ps.cur_sid
}
/// Test-only: install the playing item's server directly, returning the previous value to restore.
///
/// In production [`PlaybackSession::cur_sid`] has exactly one writer — `apply_plan` — and a `Plan` cannot
/// be built outside this module, so a suite elsewhere that needs "this is playing from the share"
/// sets it through here rather than widening `Plan` for a test. `player::playing_subscription` is
/// the reader that needs it: the failure read-out's Plex Pass claim is about the server the failing
/// item came from, and there is no other way to say which that is. It returns the previous value
/// because it began life as a swap of a crate global; since phase 9 the session is the caller's own
/// value, so putting it back is the caller's convenience rather than an obligation to other tests.
#[cfg(any(test, feature = "test-support"))]
pub fn swap_cur_sid_for_test(ps: &mut PlaybackSession, sid: ServerId) -> ServerId {
    { let s = &mut *ps; std::mem::replace(&mut s.cur_sid, sid) }
}
/// The `Client` for the currently-playing item's server, `None` before the first play (or after a
/// plan that never resolved). The main-thread twin of `client_for(env.sid)` on the resolve worker
/// — every in-playback PMS call in this file goes through one of the two, and none through
/// `client_opt()`, which answers with whatever server is CURRENT rather than the one playing.
fn cur_client(ps: &PlaybackSession) -> Option<&'static plx_plex::plex::Client> {
    plx_plex::plex::client_for(cur_sid(ps))
}
/// Projection: the wire id of the carried audio track, `0` for "server default / none" — the
/// pre-#266 shape of this accessor, kept so every existing caller that only ever wanted the id
/// keeps compiling against `cur_audio` unchanged.
pub fn cur_audio_sid(ps: &PlaybackSession) -> i64 {
    ps.cur_audio.as_ref().map_or(0, |a| a.sid)
}
/// The currently-playing item's Part id. Written once per item by `build_stream` from its own
/// `part` argument. In-playback callers (audio switch, subtitle toggle, retranscode) want this;
/// `build_stream` must pass its freshly-derived local instead, since this is not yet updated
/// for the item being started.
fn cur_part_id(ps: &PlaybackSession) -> i64 {
    ps.cur_part_id
}
/// The stable app-owned playback generation (and the first encoder's PMS session id).
pub fn sess(ps: &PlaybackSession) -> String {
    ps.sess.clone()
}
pub fn pq_id(ps: &PlaybackSession) -> String {
    ps.pq_id.clone()
}
pub fn pq_item_id(ps: &PlaybackSession) -> String {
    ps.pq_item_id.clone()
}
/// The streamed item's Media video/audio codec, so the player picks the H265 Load payload for a
/// native HEVC direct-play and the matching audio codec.
pub fn stream_vcodec(ps: &PlaybackSession) -> String {
    ps.stream_vcodec.clone()
}
pub fn stream_acodec(ps: &PlaybackSession) -> String {
    ps.stream_acodec.clone()
}
/// direct-play source video fps for the Load esInfo (0 = unknown/transcode → omit)
pub fn stream_fps(ps: &PlaybackSession) -> f64 {
    ps.stream_fps
}
/// The direct-played file's Dolby Vision layering, for the Load payload's `DolbyHdrInfo` node.
/// `Dovi::NONE` for anything the server is transcoding or remuxing, and for a DV file we refused
/// to declare — in every one of those cases the payload must say nothing.
pub fn stream_dovi(ps: &PlaybackSession) -> plx_data::metadata::Dovi {
    let s = &*ps;
    if s.stream_vcodec.eq_ignore_ascii_case("hevc") {
        s.stream_dovi
    } else {
        // Last-line consistency guard for dev declarations and future route mutations: the LG
        // payload cannot truthfully describe Dolby Vision on a non-HEVC elementary stream.
        plx_data::metadata::Dovi::NONE
    }
}
/// The capability and presentation frozen into this installed route. Unlike the raw DOVI metadata,
/// this is the value the Load payload must consume without consulting the live capability cache.
pub fn stream_dv_presentation(ps: &PlaybackSession) -> plx_data::metadata::DvPresentation {
    if ps.stream_vcodec.eq_ignore_ascii_case("hevc") {
        ps.stream_dv_decision.presentation
    } else {
        plx_data::metadata::DvPresentation::NotDv
    }
}

pub fn stream_dv_decision(ps: &PlaybackSession) -> plx_data::metadata::DvDecision {
    ps.stream_dv_decision
}

/// Server output (HLS encode, progressive encode or container remux) carries no client-frozen
/// source declaration. Centralizing the paired reset prevents a future route mutation from
/// clearing the raw metadata while leaving a stale `Declare` behind for Load.
fn clear_output_dv(session: &mut PlaybackSession) {
    session.stream_dovi = plx_data::metadata::Dovi::NONE;
    session.stream_dv_decision = plx_data::metadata::DvDecision::NONE;
}
/// Is the audio being fed a Dolby Atmos stream? — the Load payload's `contents.immersive` node.
/// See [`PlaybackSession::stream_immersive`].
pub fn stream_immersive(ps: &PlaybackSession) -> bool {
    let s = &*ps;
    // This pipeline's Atmos path is E-AC3 JOC.  AAC/AC3 are ordinary output even if a stale
    // source flag exists, so neither diagnostics nor Load may repeat that source-only claim.
    s.stream_acodec.eq_ignore_ascii_case("eac3") && s.stream_immersive
}
/// Override the audio codec used to build the Load payload — set by a native audio-track
/// switch to the chosen track's codec before the direct-play reload.
pub fn set_stream_acodec(ps: &mut PlaybackSession, codec: &str) {
    { let s = &mut *ps; s.stream_acodec = codec.to_owned() }
}
/// Record the streamed item's video+audio codec pair in one write (the Load-payload source of
/// truth) for route-policy tests that install a synthetic live HLS response.
#[cfg(any(test, feature = "test-support"))]
pub fn set_stream_codecs(ps: &mut PlaybackSession, vc: &str, ac: &str) {
    { let s = &mut *ps; {
        s.stream_vcodec = vc.to_owned();
        s.stream_acodec = ac.to_owned();
    } }
}

/// **The widest raster this session can put through the decoder**, for the Starfish Load's
/// `adaptiveStreaming` ceiling — `(0, 0)` when nobody said. Three routes, three answers:
///
/// * direct play / remux (`ProgressiveMkv`): the SOURCE's own coded size (`cur_src`), since the
///   file's elementary stream is what gets fed;
/// * a fixed quality's HLS: the source capped by that quality's [`plx_plex::plex::Ceiling`] — PMS
///   never upscales, so the smaller of the two is the largest picture it can send;
/// * Auto: the bounding box of every FEASIBLE actuator (`HlsActuatorCatalog::widest_feasible_raster`),
///   because a rung commit never re-issues `Load` and the controller may climb to the 4K point
///   inside the one declaration — a ceiling sized to the bootstrap rung (`plan.ceiling`, which is
///   the STARTING rung and not the maximum) would be exceeded on the first climb.
///
/// Main thread, like every other session read. Pure over the session it is given.
pub fn sink_max_raster(ps: &PlaybackSession) -> (u16, u16) {
    let s = &*ps;
    let clamp = |v: i64| u16::try_from(v).unwrap_or(u16::MAX);
    let source = (clamp(s.cur_src.1), clamp(s.cur_src.2));
    match s.cur_contract.delivery {
        plx_plex::plex::TranscodeDelivery::ProgressiveMkv => source,
        plx_plex::plex::TranscodeDelivery::FixedHls { .. } => {
            if applied_quality() == Quality::Auto {
                auto_catalog(ps).widest_feasible_raster()
            } else {
                match s.cur_contract.ceiling {
                    Some(c) => {
                        // 0 on either side means "nobody said"; the other side's number wins.
                        let axis = |src: u16, cap: i64| match (src, clamp(cap)) {
                            (0, c) => c,
                            (s, 0) => s,
                            (s, c) => s.min(c),
                        };
                        (axis(source.0, c.max_w), axis(source.1, c.max_h))
                    }
                    None => source,
                }
            }
        }
    }
}

/// The SOURCE raster for a stream the app did not select — the pipeline tier's
/// `plxnative-playurl` carries it as `source_raster`, the same field its Auto fixtures already
/// used to size the actuator catalog. One fact, one field: [`sink_max_raster`] reads it from the
/// same place a PMS-chosen item's dimensions land, so the synthetic tier declares exactly what the
/// production route would for a file of that size.
pub fn set_stream_source_raster(ps: &mut PlaybackSession, w: u16, h: u16) {
    { let s = &mut *ps; {
        s.cur_src.1 = i64::from(w);
        s.cur_src.2 = i64::from(h);
    } };
}

/// The whole Load-payload DECLARATION for a stream the app did not SELECT — the pipeline test
/// tier's `/tmp/plxnative-playurl` ([`crate::player::playurl::PlayUrl`]), whose entire point is that no PMS
/// chose anything and so `apply_plan` never runs.
///
/// ONE write for the same reason [`set_server_output_declaration`] is one write and [`apply_plan`]
/// is a single struct assignment: these five fields describe ONE stream, and a half-applied set is
/// a payload that describes nothing real — 4K HEVC declared with the default `""` audio, say,
/// which falls through the engine's `_ =>` arm to `"AC3"` and stalls the sink on a Dolby Digital
/// Plus track. Production route transitions likewise update the complete declaration together.
/// Four separate setters would be four ways to leave it half-written.
///
/// This touches neither `cur_rk`/`cur_sid` nor `tsession`, which is what keeps a URL-fed playback
/// free of Plex entirely: the `/:/timeline` reporter stays unspawned and `is_transcoding()` stays
/// false.
pub fn set_stream_declaration(
    ps: &mut PlaybackSession,
    vc: &str,
    ac: &str,
    fps: f64,
    dovi: plx_data::metadata::Dovi,
    immersive: bool,
) -> bool {
    set_stream_declaration_with_capability(
        ps,
        vc,
        ac,
        fps,
        dovi,
        immersive,
        plx_platform::devcaps::dv::capability(),
    )
}

fn set_stream_declaration_with_capability(
    ps: &mut PlaybackSession,
    vc: &str,
    ac: &str,
    fps: f64,
    dovi: plx_data::metadata::Dovi,
    immersive: bool,
    capability: plx_platform::devcaps::dv::DvCapability,
) -> bool {
    let decision = plx_data::metadata::DvDecision {
        capability,
        presentation: dovi.presentation(
            !plx_data::metadata::dv_withheld(),
            capability,
            vc.eq_ignore_ascii_case("hevc"),
        ),
    };
    if decision.presentation.refusal().is_some() {
        crate::player::log(&format!(
            "playurl: refusing Dolby Vision declaration capability={} presentation={}",
            decision.capability.label(),
            decision.presentation.label(),
        ));
        return false;
    }
    { let s = &mut *ps; {
        s.stream_vcodec = vc.to_owned();
        s.stream_acodec = ac.to_owned();
        s.stream_fps = fps;
        s.stream_dovi = dovi;
        s.stream_dv_decision = decision;
        s.stream_immersive = immersive;
    } }
    true
}

#[cfg(any(test, feature = "test-support"))]
pub fn set_stream_declaration_for_test(
    ps: &mut PlaybackSession,
    vc: &str,
    ac: &str,
    fps: f64,
    dovi: plx_data::metadata::Dovi,
    immersive: bool,
    capability: plx_platform::devcaps::dv::DvCapability,
) -> bool {
    set_stream_declaration_with_capability(ps, vc, ac, fps, dovi, immersive, capability)
}

// (`set_source_codecs` stood here: a two-line setter for `src_vcodec`/`src_acodec` whose one
// caller was `apply_plan`, which now installs them as part of its single assignment. The rule it
// carried survives on [`PlaybackSession::src_vcodec`] itself — those two are the FILE's codecs and
// `apply_decision_codecs`, which overwrites the stream pair with the transcode's output, must
// never touch them.)

/// Was this playback's transcode a container-only REMUX (codecs copied) rather than a re-encode?
/// Meaningless unless `is_transcoding()`. The diagnostics read-out's three-way Source row turns on
/// it: "the server touched the pixels" and "the server repackaged the bytes" are different facts
/// and only one of them can explain a decode problem.
pub fn is_remux(ps: &PlaybackSession) -> bool {
    ps.cur_contract.remux
}
/// The Plex Pass DSP the live route was ASKED for (issue #266) — `cur_contract.audio`, gated to
/// what the server DEMONSTRABLY applied: `NONE` unless `cur_enhancement` is `Applied`
/// (`Unverified`/`Refused`/`Off` say nothing was provably added to this stream, so a caller
/// quoting the params must not claim them). Read by the diagnostics Audio row's suffix, which used
/// to read the unfiltered ask and gate it on `cur_enhancement_label(ps) == Some("applied")` itself.
pub fn applied_audio_enhancements(ps: &PlaybackSession) -> plx_plex::plex::AudioEnhancements {
    if ps.cur_enhancement == EnhancementOutcome::Applied {
        ps.cur_contract.audio
    } else {
        plx_plex::plex::AudioEnhancements::NONE
    }
}
/// **Narrow diagnostics accessor** — deliberately not `pub fn cur_enhancement`, which would
/// widen [`EnhancementOutcome`] itself past `pub(super)` for one read-only caller. `None` for
/// `Off` (nothing to report); otherwise the wire word `app::diagnostics::route_line` appends as
/// `enh=<word>`.
pub fn cur_enhancement_label(ps: &PlaybackSession) -> Option<&'static str> {
    match ps.cur_enhancement {
        EnhancementOutcome::Off => None,
        EnhancementOutcome::Applied => Some("applied"),
        EnhancementOutcome::Unverified => Some("unverified"),
        EnhancementOutcome::Refused => Some("refused"),
    }
}
/// Did this playback forbid the server a video stream COPY? Read by the seek and audio-switch
/// rebuilds so the constraint survives them — see [`PlaybackSession::cur_contract`].
fn is_no_video_copy(ps: &PlaybackSession) -> bool {
    ps.cur_contract.no_video_copy
}
/// The quality ceiling THIS playback was resolved under — read by the two query rebuilds
/// ([`plan_rebase`], [`retranscode`]) so a rung picked mid-film cannot reshape the encode
/// already on screen. See [`PlaybackSession::cur_contract`].
fn cur_ceiling(ps: &PlaybackSession) -> Option<plx_plex::plex::Ceiling> {
    ps.cur_contract.ceiling
}
fn cur_delivery(ps: &PlaybackSession) -> plx_plex::plex::TranscodeDelivery {
    ps.cur_contract.delivery
}

/// Whether the live route is the segmented HLS transport. The player uses this at the Starfish
/// Load boundary: HLS must prime both elementary-stream lanes before starting the audio-master
/// clock, even on an ordinary play-from-zero where no seek rebase is pending.
pub fn is_segmented_hls(ps: &PlaybackSession) -> bool {
    matches!(
        cur_delivery(ps),
        plx_plex::plex::TranscodeDelivery::FixedHls { .. }
    )
}
pub fn source_vcodec(ps: &PlaybackSession) -> String {
    ps.src_vcodec.clone()
}

/// **Can the pixels of this playback's source reach the panel untouched?** See
/// [`PlaybackSession::cur_source_decodable`]. `false` is the state in which the quality ladder's
/// "Original" row is a promise the pipeline cannot keep.
///
/// Deliberately NOT `is_transcoding()`: that says what is happening now, and a fixed rung makes it
/// true of any source. This says what is POSSIBLE, which is the question a picker is asked.
pub fn source_decodable(ps: &PlaybackSession) -> bool {
    ps.cur_source_decodable
}
pub fn source_acodec(ps: &PlaybackSession) -> String {
    ps.src_acodec.clone()
}
/// pointers into the module-owned HUD buffers (valid for the whole frame draw_text uses them)
pub fn title_cptr(ps: &PlaybackSession) -> *const c_char {
    ps.title.as_ptr()
}
pub fn ctxline_cptr(ps: &PlaybackSession) -> *const c_char {
    ps.ctxline.as_ptr()
}
struct ScrobbleWork {
    client: Option<&'static plx_plex::plex::Client>,
    final_report: Option<(String, i64, i64)>,
    report_th: Option<std::thread::JoinHandle<()>>,
    session: String,
    play_queue_id: String,
    play_queue_item_id: String,
    audio_stream_id: i64,
    subtitle_stream_id: i64,
    transcode_session: String,
    timeline_stop: Option<TimelineStopCompletion>,
}

impl ScrobbleWork {
    fn run(mut self) {
        // The progress reporter's last `playing` POST attempt must finish BEFORE this `stopped`
        // attempt begins. Waiting here keeps that ordering off the SDL thread; the stop generation
        // was announced before this worker was spawned, so a replacement reporter waits without
        // blocking the old one.
        if let Some(t) = self.report_th.take() {
            plx_base::task::join("timeline", t);
        }
        if let Some((rk, t_ms, d_ms)) = self.final_report.take() {
            let ok = {
                let _effect = TIMELINE_EFFECT.lock().unwrap_or_else(|e| e.into_inner());
                self.client.is_some_and(|c| {
                    c.timeline(&plx_plex::plex::TimelineReport {
                        rating_key: &rk,
                        state: plx_plex::plex::TimelineState::Stopped,
                        time_ms: t_ms,
                        duration_ms: d_ms,
                        session: &self.session,
                        play_queue_id: &self.play_queue_id,
                        play_queue_item_id: &self.play_queue_item_id,
                        audio_stream_id: self.audio_stream_id,
                        subtitle_stream_id: self.subtitle_stream_id,
                    })
                })
            };
            plx_base::eventlog::log(&format!(
                "timeline stopped t={}s/{}s ok={}",
                t_ms / 1000,
                d_ms / 1000,
                ok as i32,
            ));
        }
        // This is the semantic publication boundary: old reporter joined, then old stopped was
        // attempted in the common effect lane. Wake replacement reporters before the unrelated
        // encoder-stop request, whose latency must not delay their progress heartbeat.
        if let Some(stop) = self.timeline_stop.take() {
            stop.finish();
        }
        if !self.transcode_session.is_empty() {
            let ok = self
                .client
                .is_some_and(|c| c.transcode_stop(&self.transcode_session));
            plx_base::eventlog::log(&format!("transcode stopped ok={}", ok as i32));
        }
    }
}

/// The end-of-playback PMS work, moved OFF the main thread: the `state=stopped` timeline report
/// (which commits the server-side resume point and watched state) and the server-side transcode
/// stop. Replaces the inline `report_timeline` + `stop_transcode` pair in `engine::teardown`.
///
/// Both ran inline on the SDL
/// thread — two blocking PMS round trips, each bounded by `CONNECT_TIMEOUT_MS` + `SO_RCVTIMEO`
/// (~17 s), on **100% of real stops**. That was the largest guaranteed main-loop park left in the
/// engine, and strictly bigger than the rare in-flight-POST window at the joins above it.
///
/// Everything the worker needs is read HERE, on the main thread, and the two fields a stop retires
/// are cleared here too: the [`Session`] is a `static mut`, and what keeps it sound is that the main
/// thread is its only writer. The worker gets owned copies and touches none of it — the same
/// capture the demux thread's `acodec` does, and for the same reason.
pub fn scrobble_stop(
    ps: &mut PlaybackSession,
    final_report: Option<(String, i64, i64)>,
    report_th: Option<std::thread::JoinHandle<()>>,
) {
    if preview_request(ps) {
        return;
    }
    let (logical_session, pq, pqi) = (sess(ps), pq_id(ps), pq_item_id(ps));
    let (aud, sub) = (cur_audio_sid(ps), cur_sub_sid(ps)); // the selection this playback reported under
    let tsession = take_active_encoder();
    let session = if tsession.is_empty() {
        logical_session
    } else {
        tsession.clone()
    };
    // The two fields THIS function retires (teardown clears the URL a few lines later, and that is
    // the whole of what a stop resets). A partial write rather than a whole-session reset because
    // the rest is still read after teardown returns — see `reset_session`'s doc for the reader that
    // would break.
    { let s = &mut *ps; {
        s.tsession.clear();
        s.cur_contract.remux = false;
    } };
    if final_report.is_none() && tsession.is_empty() && report_th.is_none() {
        return; // nothing to post and nobody to wait for
    }
    // The server this playback came FROM, not whichever one is current — the resume point and the
    // transcode session both live there, and by the time a stop runs the user may well have walked
    // back to a different source's Home.
    let client = cur_client(ps);
    // Serialise against a previous stop still in flight: these carry a position for a specific
    // item, and letting two race would let an older one land last. Normally free — the measured
    // baseline for a finished worker is 0 ms.
    drain_scrobble();
    let timeline_stop =
        (final_report.is_some() || report_th.is_some()).then(|| TIMELINE_STOP_FENCE.announce());
    let work = std::sync::Arc::new(std::sync::Mutex::new(Some(ScrobbleWork {
        client,
        final_report,
        report_th,
        session,
        play_queue_id: pq,
        play_queue_item_id: pqi,
        audio_stream_id: aud,
        subtitle_stream_id: sub,
        transcode_session: tsession,
        timeline_stop,
    })));
    let worker = work.clone();
    let join_generation = SCROBBLE_JOIN.reserve();
    let h = plx_base::task::spawn_small_keeping("scrobble", move || {
        let work = { worker.lock().unwrap_or_else(|e| e.into_inner()).take() };
        if let Some(work) = work {
            work.run();
        }
    });
    if let Some(handle) = h {
        SCROBBLE_JOIN.install(join_generation, handle);
    } else {
        // Thread refusal is extraordinarily rare, but dropping the old reporter handle and
        // opening the stop fence would recreate the exact new-before-old race. Pay the old
        // synchronous cost on this failure path and preserve ordering.
        let _block = plx_base::task::allow_blocking(
            const { &plx_base::task::BlockingLabel::new("scrobble stop (worker thread refused)") },
        );
        let work = { work.lock().unwrap_or_else(|e| e.into_inner()).take() };
        if let Some(work) = work {
            work.run();
        }
        SCROBBLE_JOIN.complete_spawn_refusal(join_generation);
    }
}

struct ScrobbleJoinState {
    generation: u64,
    completed: u64,
    spawn_pending: bool,
    joining: bool,
    handle: Option<(u64, std::thread::JoinHandle<()>)>,
}

struct ScrobbleJoin {
    state: std::sync::Mutex<ScrobbleJoinState>,
    changed: std::sync::Condvar,
}

impl ScrobbleJoin {
    const fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(ScrobbleJoinState {
                generation: 0,
                completed: 0,
                spawn_pending: false,
                joining: false,
                handle: None,
            }),
            changed: std::sync::Condvar::new(),
        }
    }

    /// Publish unfinished work before spawning it. A concurrent drain then waits for handle
    /// installation (or synchronous refusal completion) instead of seeing a false idle window.
    fn reserve(&self) -> u64 {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        debug_assert_eq!(state.completed, state.generation);
        debug_assert!(!state.spawn_pending && !state.joining && state.handle.is_none());
        state.generation = state
            .generation
            .checked_add(1)
            .expect("scrobble generation exhausted");
        state.spawn_pending = true;
        state.generation
    }

    fn install(&self, generation: u64, handle: std::thread::JoinHandle<()>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        debug_assert_eq!(state.generation, generation);
        debug_assert!(state.spawn_pending && state.handle.is_none());
        state.handle = Some((generation, handle));
        state.spawn_pending = false;
        self.changed.notify_all();
    }

    fn complete_spawn_refusal(&self, generation: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        debug_assert_eq!(state.generation, generation);
        state.completed = state.completed.max(generation);
        state.spawn_pending = false;
        self.changed.notify_all();
    }

    fn drain(&self) {
        loop {
            let (generation, handle) = {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if state.completed >= state.generation {
                        return;
                    }
                    if !state.spawn_pending && !state.joining {
                        if let Some((generation, handle)) = state.handle.take() {
                            state.joining = true;
                            break (generation, handle);
                        }
                    }
                    state = self.changed.wait(state).unwrap_or_else(|e| e.into_inner());
                }
            };
            plx_base::task::join("scrobble", handle);
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.completed = state.completed.max(generation);
            state.joining = false;
            self.changed.notify_all();
            // Another generation may have been reserved as soon as this one completed.
        }
    }
}

/// The final scrobble still in flight, with a shared completion barrier so every concurrent
/// drainer waits even after one of them has taken ownership of the JoinHandle.
static SCROBBLE_JOIN: ScrobbleJoin = ScrobbleJoin::new();

/// One ordered network-effect lane for progress publication. The route lease is revalidated only
/// after this lock is acquired, then `PLAYER_CONTROL` is released before I/O. Consequently an old
/// reporter either lands before the replacement or observes its stale epoch and sends nothing;
/// it can never validate first, stall, and land after the replacement report.
static TIMELINE_EFFECT: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct TimelineStopFenceState {
    announced: u64,
    completed: u64,
}

struct TimelineStopFence {
    state: std::sync::Mutex<TimelineStopFenceState>,
    changed: std::sync::Condvar,
}

impl TimelineStopFence {
    const fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(TimelineStopFenceState {
                announced: 0,
                completed: 0,
            }),
            changed: std::sync::Condvar::new(),
        }
    }

    fn announce(&'static self) -> TimelineStopCompletion {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.announced = state
            .announced
            .checked_add(1)
            .expect("timeline stop generation exhausted");
        TimelineStopCompletion {
            generation: state.announced,
            finished: false,
        }
    }

    fn announced(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .announced
    }

    fn wait(&self, required: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while state.completed < required {
            state = self.changed.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn complete(&self, generation: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if generation > state.completed {
            state.completed = generation;
            self.changed.notify_all();
        }
    }
}

static TIMELINE_STOP_FENCE: TimelineStopFence = TimelineStopFence::new();

struct TimelineStopCompletion {
    generation: u64,
    finished: bool,
}

impl TimelineStopCompletion {
    fn finish(mut self) {
        TIMELINE_STOP_FENCE.complete(self.generation);
        self.finished = true;
    }
}

impl Drop for TimelineStopCompletion {
    fn drop(&mut self) {
        if !self.finished {
            // Panic or refused worker must not strand every later reporter behind this stop.
            TIMELINE_STOP_FENCE.complete(self.generation);
        }
    }
}

/// Wait for pending [`scrobble_stop`] work to finish its ordered timeline/transcode attempts.
///
/// Production uses this before another stop, before Retry starts a replacement encoder, and at
/// `plex_run` exit. The exit wait matters because the process is about to die and a detached worker
/// dies with it; the other waits preserve ordering without putting routine BACK teardown on the
/// SDL thread.
pub fn drain_scrobble() {
    SCROBBLE_JOIN.drain();
}

/// Seek within a LIVE TRANSCODE by restarting it at a time offset — a transcode has no byte-Cues,
/// so a byte-Range seek can't work (docs/plex-api.md). Registers a fresh physical encoder, swaps
/// the route to its delivery-matched start endpoint with `offset={secs}`, then retires the old
/// exact key. The old stream stays live until the new decision has succeeded and the route
/// publication wins, so a failed seek cannot cut playback.
///
/// **There is no synchronous entry point.** A rebuild is three steps ([`plan_rebase`] on the frame
/// thread, [`run_rebase`] — the only one that reaches PMS — and [`install_rebase`] on the frame
/// thread), and every caller runs the middle one on a worker (`flight.rs`):
/// [`dispatch_transcode_seek`] for a seek, [`execute_adaptive_reload_claim`] for an adaptive
/// reload, [`dispatch_rollback_rebase`] / [`dispatch_engineless_rollback_rebase`] for the rollback
/// after a failed Original trial, [`dispatch_resume_rebase`] for the cold AND the foreground resume.
/// This composition of the three in a row exists for the host tests that grade the steps' joint
/// outcome, and is not built into the binary.
#[cfg(any(test, feature = "test-support"))]
pub fn transcode_seek(ps: &mut PlaybackSession, offset_secs: i64) -> Option<String> {
    let plan = plan_rebase(ps, offset_secs, RebaseFor::Inline).ok()?;
    let outcome = run_rebase(&plx_base::task::OffFrame::for_test(), &plan);
    install_rebase(ps, plan, outcome).map(|installed| installed.url)
}

/// Who a rebuild is for: the one fact that decides which part of the reducer it may touch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RebaseFor {
    /// The host tests' inline composition ([`transcode_seek`]). Shares a start transaction another
    /// owner may already hold.
    #[cfg(any(test, feature = "test-support"))]
    Inline,
    /// A pump seek flight: owns a FRESH start transaction (`Preparing`) from the plan until the
    /// landing installs it or discards it.
    Seek,
    /// A claimed `AdaptiveReload`: the claim's own `Applying(serial)` owns the phase, so the
    /// rebuild opens no start transaction of its own.
    Claim(u64),
    /// The rollback after a failed Original trial: rebases the restored HLS route at the recovery
    /// position. A recovery flight ([`begin_recovery_flight_start`]) takes over the `Prepared`
    /// transaction [`rollback_original_recovery`] left, and the landing settles it. (Dispatched
    /// over an Original trial's own transaction, it flies on [`OriginalTrialPhase::Preparing`] and
    /// leaves the trial's snapshot armed.)
    Rollback,
    /// A cold resume: the plan a resolve just landed is a transcode at offset 0 and the viewer is
    /// owed the saved position, so it is rebased there before its first Load. Takes over the
    /// `Prepared` start transaction the landing left ([`begin_recovery_flight_start`]); a refusal
    /// leaves that transaction `Failed`. The foreground restore of a
    /// session suspended mid-Original-trial resumes over the trial's own transaction (the trial
    /// keeps its rollback; a refusal leaves it `OriginalTrial(Failed)`).
    Resume,
}

/// Settle the start transaction a rebuild reserved but will not use. Every owner but a cold resume
/// gives it back ([`reject_route_start_preparation`]); a cold resume's was a landing's `Prepared`
/// transaction, which a refusal leaves `Failed`.
pub(super) fn release_rebase_start(owner: RebaseFor, ticket: RouteStartTransaction) {
    let _ = match owner {
        RebaseFor::Resume => abort_route_start(ticket, RouteStartResult::StartFailed),
        _ => reject_route_start_preparation(ticket),
    };
}

/// Why a rebuild was not planned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RebaseRefusal {
    /// Not now: another transaction holds the reducer. The caller retries on a later frame.
    Busy,
    /// Never for this playback (not a transcode, no server, PMS-side ticket moved): give up.
    Refused,
}

/// What a rebuild's PMS half needs, owned: read off the session on the frame thread so the worker
/// that runs it never sees a `PlaybackSession` (see [`plan_rebase`]).
pub(super) struct RebasePlan {
    pub(super) owner: RebaseFor,
    client: &'static plx_plex::plex::Client,
    rk: String,
    /// The route generation the rebuild was planned against. The commit is gated on it.
    expected: WorkerTicket,
    /// The encoder the route plays now; stopped once the replacement is the route's.
    previous: String,
    /// The physical encoder the PMS half registers, minted on the frame thread.
    replacement: String,
    offset_secs: i64,
    audio_sid: i64,
    subtitle_sid: i64,
    contract: plx_plex::plex::EncodeContract,
    /// The live HLS rung the route is on, when it is on one: the replacement keeps it.
    hls_rung: Option<crate::abr::Rung>,
    /// The start transaction a seek flight (or an inline caller) reserved. Settled by the install.
    pub(super) route_start: Option<RouteStartTransaction>,
}

/// What the PMS half settled on.
pub(super) enum RebaseOutcome {
    /// PMS accepted the replacement. NOT yet the route's: the install commits it, because the
    /// commit is a mutex operation on the frame thread's own reducer, not a server round trip.
    Prepared(PreparedRebase),
    /// PMS refused (or never answered); the replacement it may have registered is already stopped.
    Refused,
}

pub(super) struct PreparedRebase {
    pub(super) replacement: String,
    url: String,
}

/// What a landed rebuild changed, for the caller that has to describe it.
pub(super) struct RebaseInstalled {
    pub(super) url: String,
    replacement: String,
    hls_rung: Option<crate::abr::Rung>,
}

impl RebaseInstalled {
    /// The fields the install wrote to the session, on a projection: the reducer's restore point
    /// of a claim must describe the stream the landing installed ([`advance_claim_snapshot`]).
    pub(super) fn apply_to_projection(&self, p: &mut AppliedRouteProjection) {
        p.tsession = self.replacement.clone();
        p.url = self.url.clone();
        if let Some(rung) = self.hls_rung {
            p.contract.ceiling = Some(rung.ceiling());
        }
    }
}

impl RebasePlan {
    fn release_start(&self) {
        if let Some(ticket) = self.route_start {
            release_rebase_start(self.owner, ticket);
        }
    }
}

/// Reserve a start transaction ONLY from `Stable`, so that a seek flight owns the one it holds and
/// can hand the reducer back without taking anyone else's with it. [`begin_route_start`] shares an
/// existing `Preparing`/`Prepared`/`Starting` transaction, which is right for an inline caller and
/// wrong for a flight that will later reject "its" preparation.
fn begin_flight_start() -> Option<RouteStartTransaction> {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if control.phase != ControlPhase::Stable {
        return None;
    }
    control.next_action = next_generation(control.next_action);
    let serial = control.next_action;
    control.phase = ControlPhase::Preparing(serial);
    Some(RouteStartTransaction { serial })
}

/// Reserve the start transaction of a RECOVERY flight: the route of a start that failed is being
/// replaced while the Engine that failed waits. Unlike [`begin_flight_start`] it takes a
/// transaction a failed start already holds — the rollback's `Prepared(serial)` (its deferred
/// commands are keyed by that serial), or the `Starting`/`Failed` one of a source that never
/// opened — and `Stable` too, for a source that opened and then died before anything played; the
/// transaction becomes `Preparing(serial)` and the landing settles it exactly as a seek's.
///
/// **An Original trial's transaction is taken too, and the trial keeps what it owns.** A session
/// suspended mid-trial (its `Starting`/`AwaitingFrame` phase became `OriginalTrial(Prepared)` at the
/// suspend) is restored by the foreground machine's resume flight, which finds the trial's
/// `Prepared` (or `Failed`) transaction here. The flight moves it to
/// [`OriginalTrialPhase::Preparing`], the trial's own variant of the flight phase, rather than to an
/// ordinary `Preparing` — the phase is the only thing that changes; the rollback snapshot
/// (`pending_original`) is a separate field no flight writes, and every exit of the flight returns
/// the transaction to the trial (see [`prepare_route_start`], [`reject_route_start_preparation`],
/// [`abort_route_start`] and the suspend in [`begin_engine_teardown`]). `None` when any other
/// transaction holds the reducer — including a trial that is `Starting` or `AwaitingFrame`, whose
/// Engine is live and unproven.
fn begin_recovery_flight_start() -> Option<RouteStartTransaction> {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    let (serial, trial) = match control.phase {
        ControlPhase::Prepared(serial)
        | ControlPhase::Failed(serial)
        | ControlPhase::Starting(serial, _) => (serial, false),
        ControlPhase::Stable => {
            control.next_action = next_generation(control.next_action);
            (control.next_action, false)
        }
        ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial) | OriginalTrialPhase::Failed(serial)) => {
            (serial, true)
        }
        _ => return None,
    };
    control.phase = if trial {
        ControlPhase::OriginalTrial(OriginalTrialPhase::Preparing(serial))
    } else {
        ControlPhase::Preparing(serial)
    };
    Some(RouteStartTransaction { serial })
}

/// The frame-thread half of a transcode rebuild: decide whether one is allowed and capture what its
/// PMS half will need. A refusal touched nothing but the session's own live-HLS mirror (and, for an
/// owner that reserved one, a start transaction it has already handed back).
pub(super) fn plan_rebase(
    ps: &mut PlaybackSession,
    offset_secs: i64,
    owner: RebaseFor,
) -> Result<RebasePlan, RebaseRefusal> {
    use RebaseRefusal::{Busy, Refused};
    if forced_direct_play(ps) {
        return Err(Refused);
    }
    if transcode_session(ps).is_empty() {
        return Err(Refused);
    }
    // A claim's worker owns `Applying` and has no vocabulary for a rebuild replacing the encoder
    // out from under it: only the claim that IS the flight may plan one, and nothing else may
    // while one is outstanding. A plain seek asks again on a later frame; an inline caller is
    // refused.
    match owner {
        #[cfg(any(test, feature = "test-support"))]
        RebaseFor::Inline if flight_phase_open() => return Err(Refused),
        RebaseFor::Seek if flight_phase_open() => return Err(Busy),
        RebaseFor::Rollback | RebaseFor::Resume if flight_phase_open() => return Err(Refused),
        RebaseFor::Claim(serial) if !flight_phase_is(serial) => return Err(Refused),
        _ => {}
    }
    let rk = cur_rk(ps);
    if rk.is_empty() {
        return Err(Refused);
    }
    let client = cur_client(ps).ok_or(Refused)?;
    // A plain seek/foreground resume has no claimed RouteAction, but it still replaces the PMS
    // route and native Engine. Reserve the same start transaction before exposing any candidate
    // fields; an action already in Applying owns its own later Prepared edge.
    let route_start = match owner {
        #[cfg(any(test, feature = "test-support"))]
        RebaseFor::Inline => begin_route_start(),
        RebaseFor::Seek => Some(begin_flight_start().ok_or(Busy)?),
        RebaseFor::Rollback | RebaseFor::Resume => Some(begin_recovery_flight_start().ok_or(Refused)?),
        RebaseFor::Claim(_) => None,
    };
    let release = |route_start: Option<RouteStartTransaction>| {
        if let Some(ticket) = route_start {
            release_rebase_start(owner, ticket);
        }
    };
    let live_hls = sync_active_hls_to_session(ps);
    let expected = live_hls
        .as_ref()
        .map(|(ticket, _)| ticket.clone())
        .unwrap_or_else(worker_ticket);
    let previous = expected.encoder().to_owned();
    if previous.is_empty() {
        release(route_start);
        return Err(Refused);
    }
    let logical_session = sess(ps);
    let namespace = if logical_session.is_empty() { previous.as_str() } else { logical_session.as_str() };
    let replacement = next_encoder_session(namespace);
    Ok(RebasePlan {
        owner,
        client,
        rk,
        expected,
        previous,
        replacement,
        offset_secs,
        audio_sid: cur_audio_sid(ps),
        subtitle_sid: cur_sub_sid(ps),
        contract: ps.cur_contract,
        hls_rung: live_hls.as_ref().map(|(_, hls)| hls.rung),
        route_start,
    })
}

/// The PMS half of a transcode rebuild — everything that blocks on the server and nothing that
/// touches a `PlaybackSession` or the route, so it may run on a worker. It registers the
/// replacement encoder with `/decision` at the plan's offset; the COMMIT (the route naming the
/// replacement) is [`install_rebase`]'s, on the frame thread. A refusal stops the replacement it
/// may have registered: a lost response can still have created the key, and the old route is still
/// published, so only the uncommitted replacement is cleaned up.
pub(super) fn run_rebase(off: &plx_base::task::OffFrame, plan: &RebasePlan) -> RebaseOutcome {
    let sp = transcode_spec(
        &plan.rk,
        &plan.replacement,
        &plan.replacement,
        plx_plex::plex::TranscodeOffset::from_seconds(plan.offset_secs.max(0)),
        plan.audio_sid,
        plan.subtitle_sid,
        plan.contract,
    );
    let c = plan.client;
    let Some(decision) = c.transcode_decision(off, &sp) else {
        let _ = c.transcode_stop(&plan.replacement);
        return RebaseOutcome::Refused;
    };
    if refusal(&decision).is_some() {
        let _ = c.transcode_stop(&plan.replacement);
        return RebaseOutcome::Refused;
    }
    RebaseOutcome::Prepared(PreparedRebase {
        replacement: plan.replacement.clone(),
        url: c.transcode_start_url(&sp).to_url(),
    })
}

/// The frame-thread half that lands a rebuild: commit the replacement as the route's encoder
/// (gated on the plan's ticket), write the session projection, settle the start transaction the
/// plan reserved and retire the encoder it replaced. `None` is a refusal and nothing was written:
/// PMS said no, or the route moved while the worker ran (the replacement it registered is stopped
/// here — it never became anyone's), or the start transaction was lost.
pub(super) fn install_rebase(
    ps: &mut PlaybackSession,
    plan: RebasePlan,
    outcome: RebaseOutcome,
) -> Option<RebaseInstalled> {
    let RebaseOutcome::Prepared(prepared) = outcome else {
        plan.release_start();
        return None;
    };
    let PreparedRebase { replacement, url } = prepared;
    let published = if let Some(rung) = plan.hls_rung {
        // This is a NEW PMS response. Carrying the old decoded raster would turn the previous
        // session's observation into a claim about bytes nobody has opened yet; the new demux
        // publishes its own master declaration and decoded raster after the reload.
        replace_active_hls_for(&plan.expected, &replacement, &url, rung, None).is_some()
    } else {
        replace_active_encoder_for(&plan.expected, &replacement).is_some()
    };
    if !published {
        stop_encoder_session(plan.client, replacement);
        plan.release_start();
        return None;
    }
    {
        let s = &mut *ps;
        s.tsession = replacement.clone();
        s.url = url.clone();
        if let Some(rung) = plan.hls_rung {
            s.cur_contract.ceiling = Some(rung.ceiling());
        }
    }
    let installed = RebaseInstalled { url, replacement, hls_rung: plan.hls_rung };
    // A claim's snapshot is what `finish_route_action` publishes; every other owner publishes the
    // session as it now stands — except inside an Original trial, whose applied projection is the
    // retained route the rollback restores and which the candidate rebuilt here is not yet.
    if !matches!(plan.owner, RebaseFor::Claim(_)) && !carry_rebase_into_trial(&installed, &plan.previous) {
        publish_applied_route_projection(ps);
    }
    if let Some(ticket) = plan.route_start {
        if !prepare_route_start(ticket) {
            crate::player::log("seek: prepared PMS route lost its start transaction");
            return None;
        }
    }
    // The route now names the replacement; the caller will tear down the old demux immediately
    // and reopen this URL. Retire the old exact PMS key off the frame thread, just like an ABR
    // commit, so a slow `/stop` cannot freeze the seek UI.
    retire_previous_encoder(plan.client, plan.previous);
    Some(installed)
}

/// A rebuild that lands INSIDE an Original trial (a recovery flight over the trial's own
/// transaction, [`OriginalTrialPhase::Preparing`]) rebuilt the CANDIDATE: the stream on screen when
/// the trial's Load proves itself, and the encoder its rollback or teardown must stop. So the
/// snapshot follows the route instead of the applied projection, which stays the retained route's
/// until the first decoded frame commits the candidate (`confirm_original_recovery`): the
/// candidate projection takes the rebuilt URL and session, and the remux the trial owns
/// (`replacement_encoder`) is the rebuilt one. `false` when no trial owns the reducer.
fn carry_rebase_into_trial(installed: &RebaseInstalled, previous: &str) -> bool {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if !matches!(control.phase, ControlPhase::OriginalTrial(_)) {
        return false;
    }
    if let Some(pending) = control.pending_original.as_mut() {
        installed.apply_to_projection(&mut pending.candidate_projection);
        if pending.replacement_encoder == previous {
            pending.replacement_encoder = installed.replacement.clone();
        }
    }
    true
}

/// Stop the encoder a rebuild replaced, off the frame thread, and log it — the device harness
/// (`tests/run.py::op_seek_transcode`) reads `seek: retired previous encoder ok=1` as the proof
/// that a seek did not leave its predecessor running. If the OS refuses the worker the stop still
/// happens inline under the refused-thread exception.
fn retire_previous_encoder(client: &'static plx_plex::plex::Client, previous: String) {
    if previous.is_empty() {
        return;
    }
    plx_base::task::spawn_small_or_inline(
        "seek-stop",
        const { &plx_base::task::BlockingLabel::new("encoder stop (worker thread refused)") },
        move || {
            let ok = client.transcode_stop(&previous);
            crate::player::log(&format!("seek: retired previous encoder ok={}", ok as i32));
        },
    );
}

/// A rebuild landing that will never install: stop ONLY the replacement its worker registered. The
/// worker committed nothing (the commit is [`install_rebase`]'s), so the encoder still on screen
/// is the route's and is left alone — unlike a claim's landing, whose worker already made its
/// replacement the route's encoder.
pub(super) fn discard_rebase(plan: RebasePlan, outcome: RebaseOutcome) {
    if let RebaseOutcome::Prepared(prepared) = outcome {
        stop_encoder_session(plan.client, prepared.replacement);
    }
}

/// What [`dispatch_transcode_seek`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekDispatch {
    /// A worker owns the PMS half; the drain ([`take_ready_flight`]) delivers the verdict.
    Flying { serial: u64 },
    /// Another transaction holds the reducer; leave the seek target where it is and ask again.
    Busy,
    /// Nothing to rebuild or the worker could not start: abandon this seek.
    Refused,
}

/// Start the flight for a seek on a live transcode: plan on the frame thread, hand the PMS half to
/// a worker, and keep the reducer in `Preparing(serial)` — a start transaction this flight owns —
/// until the landing installs or discards it. The caller (the pump) holds presentation for the
/// flight.
pub fn dispatch_transcode_seek(ps: &mut PlaybackSession, target_ns: i64) -> SeekDispatch {
    let plan = match plan_rebase(ps, target_ns / 1_000_000_000, RebaseFor::Seek) {
        Ok(plan) => plan,
        Err(RebaseRefusal::Busy) => return SeekDispatch::Busy,
        Err(RebaseRefusal::Refused) => return SeekDispatch::Refused,
    };
    let ticket = plan.route_start.expect("a seek flight owns its start transaction");
    begin_flight(ticket.serial);
    if spawn_flight(
        FlightOwner::Start(ticket),
        target_ns,
        target_ns,
        None,
        ClaimWork::Rebase(Box::new(plan)),
        RetranscodeFallback::RejectWith(SEEK_REJECTED),
    ) {
        SeekDispatch::Flying { serial: ticket.serial }
    } else {
        // The OS refused the thread: nothing was registered, so there is nothing to stop.
        let _ = reject_route_start_preparation(ticket);
        SeekDispatch::Refused
    }
}

use plx_base::cbuf::set as set_c; // shared fixed-C-buffer write (the session's HUD title/ctxline)

// ---- the QUALITY ceiling: what the USER has asked this playback to come in under -------------

/// The playback-quality ladder: **Auto, Original, and a few fixed rungs**, and every rung is a ROUTING POLICY
/// before it is a parameter.
///
/// # Why this is not a bitrate field on [`plx_plex::plex::TranscodeSpec`]
///
/// That is the shape this began as, and it does nothing for the one file it exists for.
/// [`build_stream`] picks direct play → remux → re-encode BEFORE any spec is built, and only the
/// re-encode branch's query has ever carried `maxVideoBitrate`: direct play streams the file's own
/// bytes with no encoder anywhere to read a cap, and a remux copies the codecs and deliberately
/// sends no cap at all (a cap is exactly what would force the re-encode it exists to avoid). So a
/// 30 Mbit/s source on a 4 Mbit/s link — the case the whole feature is about — direct-plays
/// straight past a number set on the spec, and the user who picked "4 Mbps" sees no change
/// whatsoever. `plex::params`' own doc argued this out for a LINK ceiling long before there was a
/// user-chosen one, and the argument transfers unchanged.
///
/// So a rung is spent the way [`plx_plex::plex::link_policy`] spends the relay tier: **deny the two
/// flavors that ship the file at its own rate, leaving the one flavor whose whole point is that
/// the server picks the rate** ([`quality_policy`]). Only then does the rung's number reach the
/// wire, as [`plx_plex::plex::Ceiling`] on the re-encode query.
///
/// # The ladder, and why these rungs
///
/// A standard descending ladder, each rung pairing a rate with the frame that rate can actually
/// carry — a rung that halves the rate and keeps 4K asks the server for something it cannot make
/// look like anything.
///
/// **A rung is a CONTENT rate; the checklist's legs are LINK rates, and the two are not the same
/// number.** LG's #43 CASE1 exercises 512 Kbps / 1 Mbps / 7 Mbps / 17.5 Mbps, and the useful
/// question is which rung a user on each leg would pick — the one comfortably *below* it, since
/// the leg has to carry the stream plus everything else on the line:
///
/// | link leg | the rung that fits |
/// |---|---|
/// | 17.5 Mbit/s | `1080p · 8 Mbps` (`P1080High`'s 20 does NOT fit — it is the rung for an uncapped LAN) |
/// | 7 Mbit/s | `720p · 4 Mbps` |
/// | 1 Mbit/s | `480p · 720 kbps` |
/// | 512 Kbit/s | **nothing** — it is below this ladder's floor, and no rung here pretends otherwise |
///
/// That last row is the honest one and it is why this table exists: three of these rungs carried a
/// comment claiming to sit "under" a leg they are numerically above, which would have sent the
/// next person tuning them to trust a false justification.
///
/// **Original is the migration-safe default and must stay a pure no-op**, which is what the
/// regression test at the foot of this file pins: with `Original` selected, every routing
/// decision and every query byte is what it was before this type existed. Auto is a distinct
/// persisted mode, offered and restored only behind [`auto_quality_ready`]; its top state is an
/// unmodified Original on Local or a measured direct Remote, with fixed-session HLS as the
/// constrained-link path.
/// What the menu may offer in this build. Original and fixed ceilings are established playback
/// paths; Auto joins them only when [`auto_quality_ready`] says the adaptive path is complete.
pub fn available_quality_ladder() -> &'static [Quality] {
    quality_ladder_for(auto_quality_ready())
}

/// An in-memory index back to a rung — out of range is `Original`, never a neighbouring rung,
/// for the same reason the more menu resolves a press by row identity rather than position: the ladder can grow or shrink.
fn quality_from_index(i: u8) -> Quality {
    QUALITY_LADDER
        .get(i as usize)
        .copied()
        .unwrap_or(Quality::Original)
}

fn quality_index(q: Quality) -> u8 {
    QUALITY_LADDER.iter().position(|&r| r == q).unwrap_or(1) as u8
}

/// Install-wide preference; each resolve captures its own immutable mode.
static DIRECT_PLAY_MODE: AtomicU8 = AtomicU8::new(0);

pub fn direct_play_mode() -> DirectPlayMode {
    match DIRECT_PLAY_MODE.load(Ordering::Relaxed) {
        1 => DirectPlayMode::Forced,
        2 => DirectPlayMode::Disabled,
        _ => DirectPlayMode::Auto,
    }
}

pub fn restore_direct_play_mode(mode: DirectPlayMode) {
    #[cfg(any(test, feature = "test-support"))]
    plx_base::testlock::assert_held("direct-play preference");
    DIRECT_PLAY_MODE.store(match mode {
        DirectPlayMode::Auto => 0, DirectPlayMode::Forced => 1, DirectPlayMode::Disabled => 2,
    }, Ordering::Relaxed);
}

/// Blocking persistence seam; Settings dispatches it on the storage worker.
pub fn set_direct_play_mode(mode: DirectPlayMode) -> bool {
    let saved = plx_plex::plex::session::update_with_outcome(|s| Some(s.with_direct_play_mode(mode)))
        .is_some_and(|write| matches!(write.classify(),
            plx_plex::plex::session::async_persistence::CompletionOutcome::Durable(_)));
    if saved { restore_direct_play_mode(mode); plx_machine::idle::invalidate(); }
    saved
}

/// What the player does at an episode's credits when a successor is queued — install-wide, like
/// [`DIRECT_PLAY_MODE`]. Read by `appkit::player_hud::slot` every frame and by `finish_playback` at
/// the end of the stream.
static NEXT_EPISODE_MODE: AtomicU8 = AtomicU8::new(0); // NextEpisodeMode::Countdown's index

pub fn next_episode_mode() -> NextEpisodeMode {
    NextEpisodeMode::from_index(NEXT_EPISODE_MODE.load(Ordering::Relaxed))
}

pub fn restore_next_episode_mode(mode: NextEpisodeMode) {
    #[cfg(any(test, feature = "test-support"))]
    plx_base::testlock::assert_held("next episode preference");
    NEXT_EPISODE_MODE.store(mode.index(), Ordering::Relaxed);
}

/// Blocking persistence seam; Settings dispatches it on the storage worker. The live value changes
/// only once the write is durable, so a failed save claims nothing.
pub fn set_next_episode_mode(mode: NextEpisodeMode) -> bool {
    let saved = plx_plex::plex::session::update_with_outcome(|s| Some(s.with_next_episode_mode(mode)))
        .is_some_and(|write| matches!(write.classify(),
            plx_plex::plex::session::async_persistence::CompletionOutcome::Durable(_)));
    if saved {
        restore_next_episode_mode(mode);
        plx_machine::idle::invalidate();
        plx_telemetry::diag::event(plx_telemetry::diag::schema::DiagEvent::FeatureUsed {
            feature: plx_telemetry::diag::schema::Feature::NextEpisode(mode),
        });
    }
    saved
}

/// How far one Left/Right press jumps in the player and the trailer transport — install-wide,
/// like [`NEXT_EPISODE_MODE`]. Read per press by `appkit::player_hud::scrub_step_ns`.
static SKIP_INTERVAL: AtomicU8 = AtomicU8::new(1); // SkipInterval::Seconds10's index

pub fn skip_interval() -> SkipInterval {
    SkipInterval::from_index(SKIP_INTERVAL.load(Ordering::Relaxed))
}

pub fn restore_skip_interval(interval: SkipInterval) {
    #[cfg(any(test, feature = "test-support"))]
    plx_base::testlock::assert_held("skip interval preference");
    SKIP_INTERVAL.store(interval.index(), Ordering::Relaxed);
}

/// Blocking persistence seam; Settings dispatches it on the storage worker. The live value changes
/// only once the write is durable, so a failed save claims nothing.
pub fn set_skip_interval(interval: SkipInterval) -> bool {
    let saved = plx_plex::plex::session::update_with_outcome(|s| Some(s.with_skip_interval(interval)))
        .is_some_and(|write| matches!(write.classify(),
            plx_plex::plex::session::async_persistence::CompletionOutcome::Durable(_)));
    if saved {
        restore_skip_interval(interval);
        plx_machine::idle::invalidate();
        plx_telemetry::diag::event(plx_telemetry::diag::schema::DiagEvent::FeatureUsed {
            feature: plx_telemetry::diag::schema::Feature::SkipInterval(interval),
        });
    }
    saved
}

/// What OK does on a Continue Watching card — install-wide, like [`NEXT_EPISODE_MODE`]. Read by
/// every deck's press and draw (Home's and the Library's) and by the item menu when it builds its
/// rows; `DeckPress::press_plays` is the one question they all ask of it.
static DECK_PRESS: AtomicU8 = AtomicU8::new(0); // DeckPress::Details's index

pub fn deck_press() -> DeckPress {
    DeckPress::from_index(DECK_PRESS.load(Ordering::Relaxed))
}

pub fn restore_deck_press(mode: DeckPress) {
    #[cfg(any(test, feature = "test-support"))]
    plx_base::testlock::assert_held("deck press preference");
    DECK_PRESS.store(mode.index(), Ordering::Relaxed);
}

/// Blocking persistence seam; Settings dispatches it on the storage worker. The live value changes
/// only once the write is durable, so a failed save claims nothing.
pub fn set_deck_press(mode: DeckPress) -> bool {
    let saved = plx_plex::plex::session::update_with_outcome(|s| Some(s.with_deck_press(mode)))
        .is_some_and(|write| matches!(write.classify(),
            plx_plex::plex::session::async_persistence::CompletionOutcome::Durable(_)));
    if saved {
        restore_deck_press(mode);
        plx_machine::idle::invalidate();
    }
    saved
}

pub fn set_default_quality(q: Quality) -> bool {
    let q = supported_quality(q);
    let saved = plx_plex::plex::session::update_with_outcome(|s| Some(s.with_playback_quality(q)))
        .is_some_and(|write| matches!(write.classify(),
            plx_plex::plex::session::async_persistence::CompletionOutcome::Durable(_)));
    if saved { restore_quality(q); plx_machine::idle::invalidate(); }
    saved
}

/// The client-rendered subtitle caption's text size — install-wide, like [`DIRECT_PLAY_MODE`].
static SUBTITLE_SIZE: AtomicU8 = AtomicU8::new(1); // SubtitleSize::Medium's index

pub fn subtitle_size() -> SubtitleSize {
    SubtitleSize::from_index(SUBTITLE_SIZE.load(Ordering::Relaxed))
}

pub fn restore_subtitle_size(size: SubtitleSize) {
    #[cfg(any(test, feature = "test-support"))]
    plx_base::testlock::assert_held("subtitle size preference");
    SUBTITLE_SIZE.store(size.index(), Ordering::Relaxed);
}

/// **Pick a caption size: one optimistic operation, on the main thread.** The live atomic is
/// stored and the frame invalidated FIRST, so the caption changes on the next frame; then a
/// persist-only closure is retained for the shared storage worker. The closure writes the session
/// store and NEVER touches the atomic (the pattern of `player::set_subtitle_tone`), so no
/// completion can overwrite a newer live pick: live = the latest pick, durable writes are FIFO on
/// the worker, last submitted wins. `reply` (Settings) learns whether the write was durable; a
/// failure claims nothing about the next boot and republishes nothing.
pub fn select_subtitle_size(size: SubtitleSize, reply: Option<std::sync::mpsc::Sender<bool>>) {
    restore_subtitle_size(size);
    plx_machine::idle::invalidate();
    let _ = plx_base::storage_worker::submit_retained(move || {
        let saved = plx_plex::plex::session::update_with_outcome(|s| Some(s.with_subtitle_size(size)))
            .is_some_and(|write| matches!(write.classify(),
                plx_plex::plex::session::async_persistence::CompletionOutcome::Durable(_)));
        if !saved {
            plx_base::eventlog::log("subtitle size: durable write failed (live value kept for this session)");
        }
        if let Some(reply) = reply {
            let _ = reply.send(saved);
        }
        plx_machine::idle::invalidate();
    });
}

/// The client-rendered subtitle caption's vertical placement — install-wide, like
/// [`DIRECT_PLAY_MODE`]. Only the plain-text caption draw moves with this; image (PGS/VobSub)
/// captions and native ASS/SSA keep their own placement.
static SUBTITLE_POSITION: AtomicU8 = AtomicU8::new(0); // SubtitlePosition::Low's index

pub fn subtitle_position() -> SubtitlePosition {
    SubtitlePosition::from_index(SUBTITLE_POSITION.load(Ordering::Relaxed))
}

pub fn restore_subtitle_position(position: SubtitlePosition) {
    #[cfg(any(test, feature = "test-support"))]
    plx_base::testlock::assert_held("subtitle position preference");
    SUBTITLE_POSITION.store(position.index(), Ordering::Relaxed);
}

/// [`select_subtitle_size`] for the caption's vertical placement.
pub fn select_subtitle_position(position: SubtitlePosition, reply: Option<std::sync::mpsc::Sender<bool>>) {
    restore_subtitle_position(position);
    plx_machine::idle::invalidate();
    let _ = plx_base::storage_worker::submit_retained(move || {
        let saved = plx_plex::plex::session::update_with_outcome(|s| Some(s.with_subtitle_position(position)))
            .is_some_and(|write| matches!(write.classify(),
                plx_plex::plex::session::async_persistence::CompletionOutcome::Durable(_)));
        if !saved {
            plx_base::eventlog::log("subtitle position: durable write failed (live value kept for this session)");
        }
        if let Some(reply) = reply {
            let _ = reply.send(saved);
        }
        plx_machine::idle::invalidate();
    });
}

pub fn forced_direct_play(ps: &PlaybackSession) -> bool {
    ps.direct_play_mode == DirectPlayMode::Forced
}

pub fn audio_track_direct_plays(ps: &PlaybackSession, codec: &str, channels: i64) -> bool {
    audio_direct_plays(ps.direct_play_mode, codec, channels)
}

/// The user's current pick. An atomic rather than a field on [`Session`] because it OUTLIVES a
/// playback — it is a preference, not session state — and because `appkit::more_menu` reads it to draw
/// the checkmark while [`ResolveEnv::snapshot`] reads it to hand the worker a copy.
///
/// Seeded to Original even before the boot gate restores the session: no call path may turn a
/// missing preference into Auto simply because initialization order changed.
static QUALITY: AtomicU8 = AtomicU8::new(1);

/// The selected ceiling. Safe from any thread; the resolve worker gets a COPY through
/// [`ResolveEnv`] rather than reading this, per that struct's own rule.
pub fn quality() -> Quality {
    quality_from_index(QUALITY.load(Ordering::Relaxed))
}

/// Restore the persisted preference without writing it back. The distinction matters for legacy
/// sessions: their missing field resolves to Original but remains missing until the user makes an
/// explicit choice, so a future Auto-ready build still cannot reinterpret that old install.
pub fn restore_quality(q: Quality) {
    // `QUALITY` is a process global `quality()` reads without taking any lock, so a test that
    // writes it without `testlock::serial()` lands the write mid some OTHER test's read —
    // `on_deck_hevc_p5_preview_uses_the_selected_episodes_codec` flaked exactly this way. Same
    // guard as the plex server registry (`plex::servers::register_with_client_id`).
    #[cfg(any(test, feature = "test-support"))]
    plx_base::testlock::assert_held("the playback quality ceiling (restore_quality)");
    QUALITY.store(quality_index(supported_quality(q)), Ordering::Relaxed);
}

/// Select a rung. MAIN THREAD (it writes the session).
///
/// It binds every FUTURE resolve, and it also re-decides the playback already on screen — because
/// the ladder's only entry point is the player's own `…` menu, so a rung that bound nothing until
/// the next play would be a control that visibly does nothing everywhere it can be reached.
///
/// **The re-decision is the same one [`build_stream`] made**, re-asked with the new rung against
/// the numbers that resolve measured ([`PlaybackSession::cur_src`]) — not a blanket reload:
///
/// * Nothing playing, or the rung is the one already in force → the preference, and nothing else.
/// * The new rung still ADMITS this source and it is direct-playing → nothing to do. Picking
///   "1080p · 20 Mbps" while direct-playing a 5 Mbit/s file must not start an encoder.
/// * Otherwise the flavour on the wire is no longer the one this rung allows, so the session's
///   ceiling moves and the pump is asked for a fresh transcode at the current position. That is
///   `request_transcode_refresh` — the identical path a subtitle-burn change already takes
///   (`commit_subtitle_selection`), gated in `player::pump` on a session that is actually
///   `Playing`, so it is inert during a pre-roll.
///
/// This is a USER-initiated switch, and it is not the adaptive one: nothing here measures a link
/// or changes a rung on its own. `PlaybackSession::cur_ceiling`'s doc has the other half — a SEEK still
/// rebuilds from the stored ceiling, so only an explicit pick can move it mid-film.
fn persist_quality_choice(q: Quality) -> Quality {
    let q = supported_quality(q);
    crate::player::report::note_quality_selected_for(playback_trace_generation(), q);
    // See `restore_quality`: the same process global, the same lock requirement in tests.
    #[cfg(any(test, feature = "test-support"))]
    plx_base::testlock::assert_held("the playback quality ceiling (persist_quality_choice)");
    QUALITY.store(quality_index(q), Ordering::Relaxed);
    // A session write is a read-modify-write under the session lock: changing this preference
    // must not overwrite a roster refresh, a profile switch, or another profile's recents.
    let _ = plx_plex::plex::session::queue_update(move |s| {
        if s.playback_quality == Some(q) {
            None
        } else {
            Some(s.with_playback_quality(q))
        }
    });
    // The picker's checkmark moves on this and on nothing else — a settled popover presents no
    // frames, so without this the row would still read as the old rung until the next keypress.
    plx_machine::idle::invalidate();
    q
}

/// Persist a quality choice made from the terminal failure screen, without trying to mutate the
/// dead live route.  The caller starts a fresh resolve immediately; [`ResolveEnv`] will consume
/// this preference there.
pub fn set_quality_for_retry(q: Quality) {
    let _ = persist_quality_choice(q);
}

pub fn set_quality(ps: &mut PlaybackSession, q: Quality) {
    if forced_direct_play(ps) {
        let _ = persist_quality_choice(q);
        return;
    }

    let q = supported_quality(q);
    let unchanged = q == quality();
    // Hold the explicit user-staging phase across persistence, Session projection changes and
    // pending-action publication. Re-selecting the current row remains a true no-op.
    let _edit = (!unchanged).then(|| begin_user_quality_boundary(q));
    let q = persist_quality_choice(q);
    if unchanged {
        return;
    }
    if original_recovery_pending() {
        // The current Load has not produced a frame yet. Mutating its declaration or publishing
        // another encoder now would invalidate PendingOriginal's exact rollback identities. The
        // picker is already truthful because the preference was persisted above; the latest pick
        // is applied immediately after this handoff commits or rolls back.
        if q == Quality::Original {
            // Adopt the already-running automatic candidate. There is no reason to black-screen
            // through a second identical Load; first-frame confirmation transfers ownership to
            // this manual contract and revokes the Auto watchdog ticket.
            { let s = &mut *ps; s.cur_auto_original_watched = false };
            let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(pending) = control.pending_original.as_mut() {
                pending.adopted_by_user = true;
                pending.deferred_quality = None;
                pending.candidate_projection.auto_original_watched = false;
            }
        } else {
            let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(pending) = control.pending_original.as_mut() {
                pending.deferred_quality = Some(q);
            }
        }
        crate::player::log("quality: deferred until pending Original handoff is resolved");
        return;
    }
    apply_quality_choice(ps, q);
}

fn apply_quality_choice(ps: &mut PlaybackSession, q: Quality) {
    if forced_direct_play(ps) { return; }
    // A later non-Auto pick supersedes an Auto restart that the pump has not consumed yet. If the
    // live worker really was adaptive, the route comparison below schedules the symmetric restart
    // which removes its watchdog; if it was still Manual, this cancellation avoids a stale Auto
    // reload after the checkmark has already moved back.
    if q != Quality::Auto {
        crate::player::cancel_adaptive_reload();
    }
    // A worker-side ABR commit is the current route. Reconcile it before comparing or replacing
    // anything, so a menu action cannot make a decision against the bootstrap ceiling.
    let live_hls = sync_active_hls_to_session(ps);
    // An exact direct/remux candidate survives both fixed-rung and HLS transitions. An explicit
    // Original pick is an instruction to restore that native declaration now, not merely remove a
    // bitrate cap and start another encoder. Leave the current route intact until the pump owns
    // the same-position codec-changing reload.
    if q == Quality::Original && is_transcoding(ps) && ps.auto_original.is_some() {
        crate::player::log("quality: Original picked — restoring the native source");
        crate::player::request_original_recovery(ps);
        return;
    }
    if q == Quality::Auto && live_hls.is_some() {
        // The bytes/rung stay exactly where they are, but the running worker may have been born
        // while the picker was Manual Original (notably after an Original 500 rollback). Its HLS
        // controller then captured no Original candidate. Recreate only the worker at this exact
        // route so Auto means the same controller contract regardless of how HLS was reached.
        crate::player::log(
            "quality: Auto picked — retaining live HLS and refreshing its adaptive contract",
        );
        crate::player::request_adaptive_reload(ps);
        return;
    }
    let location = plx_plex::plex::client_for(cur_sid(ps)).and_then(|client| client.link());
    // A direct Remote already on screen is itself stronger evidence than a second prefix fetch:
    // selecting Auto must not start an encoder under a movie which is currently arriving as
    // Original. A fresh Remote play is measured in `build_stream`; an existing transcode has no
    // such original-file observation and stays on HLS.
    //
    // Link class cannot create a source candidate. Local normally needs no throughput proof, but
    // it still needs Original to be technically possible. `build_stream` records that fact as
    // `auto_original`: `None` means the source codec/container/audio combination already failed
    // feasibility. Treating Local alone as sufficient here turns a fixed-rung AV1 transcode into
    // progressive MKV when the user returns to Auto, so no HLS controller is rebuilt.
    let original_feasible = ps.auto_original.is_some();
    let auto_original = q == Quality::Auto
        && original_feasible
        && (location == Some(plx_plex::plex::probe::Location::Local)
            || (location == Some(plx_plex::plex::probe::Location::Remote)
                && (!is_transcoding(ps) || is_remux(ps))));
    let adaptive = auto_uses_hls(q, auto_original);
    let delivery = if adaptive {
        plx_plex::plex::TranscodeDelivery::FixedHls {
            seconds_per_segment: 2,
        }
    } else {
        plx_plex::plex::TranscodeDelivery::ProgressiveMkv
    };
    let starting_rung = adaptive.then(|| {
        crate::abr::hls_reentry_rung(
            cur_ceiling(ps).and_then(crate::abr::Rung::from_ceiling),
            auto_prior(ps),
            &auto_catalog(ps),
            &crate::abr::AbrPolicy::measured(),
        )
    });
    let ceiling = starting_rung
        .map(crate::abr::Rung::ceiling)
        .or_else(|| q.ceiling());
    if cur_rk(ps).is_empty() {
        return;
    }
    let route_unchanged = cur_ceiling(ps) == ceiling && cur_delivery(ps) == delivery;
    let watched_before = ps.cur_auto_original_watched;
    let (kbps, w, h) = ps.cur_src;
    let admits = quality_policy(q, auto_original, kbps, w, h).direct_play;
    { let s = &mut *ps; {
        s.cur_contract.ceiling = ceiling;
        s.cur_contract.delivery = delivery;
        // Supervision does not depend on where the server is: `auto_original` already says Auto
        // is going to run Original, and that is the whole question the watchdog asks. See the
        // field. It was assigned to an `auto_original_watched` binding first, which named nothing
        // the right-hand side did not already say — a leftover from the `auto_remote_original`
        // refactor, where the two really were different.
        s.cur_auto_original_watched = auto_original;
        if matches!(delivery, plx_plex::plex::TranscodeDelivery::FixedHls { .. }) {
            s.cur_contract.remux = false;
        }
    } };
    // The bytes, URL and decoder declaration may be identical while the worker contract is not.
    // `engine::start_bufferfeed` captures `auto_original_watch()` BY VALUE at spawn, so toggling
    // Manual Original <-> Auto Original underneath the existing thread can never start/stop the
    // controller. Replace only that worker/pipeline at the same movie position; this is not a PMS
    // rendition change and must not go through the transcode-refresh path.
    if route_unchanged && watched_before != auto_original {
        crate::player::log(&format!(
            "quality: {} picked — restarting the current source to {} adaptive supervision",
            q.label(),
            if auto_original { "enable" } else { "disable" },
        ));
        crate::player::request_adaptive_reload(ps);
        return;
    }
    if route_unchanged {
        commit_in_place_route_projection(ps, true);
        return;
    }
    if admits && !is_transcoding(ps) {
        // The bytes already satisfy the new ceiling, but the demux worker captured adaptive
        // supervision by value. Auto <-> Manual therefore still needs a same-URL worker restart;
        // otherwise the old Auto watchdog can publish a fallback after the picker says Manual.
        if watched_before != auto_original {
            crate::player::log(&format!(
                "quality: {} picked — keeping direct bytes and refreshing adaptive supervision",
                q.label(),
            ));
            crate::player::request_adaptive_reload(ps);
        } else {
            commit_in_place_route_projection(ps, true);
        }
        return; // the picture on screen already satisfies the new rung
    }
    crate::player::log(&format!(
        "quality: {} picked — source {kbps}kbps {w}x{h}; re-transcoding this playback{}",
        q.label(),
        starting_rung
            .map(|rung| format!(" at {}kbps HLS", rung.kbps()))
            .unwrap_or_default(),
    ));
    crate::player::request_transcode_refresh(ps);
}

/// **One bounded measurement of the actual file, as an observation and nothing more.** It reports
/// bytes, active duration and whether the target was reached, because all three decide how much
/// the measurement is worth: a 40 KiB read that finished instantly honestly reports a huge rate
/// and proves nothing. What it does NOT do is decide anything — [`crate::abr::bootstrap`] owns the
/// admission rule, so the policy is stated once and is host-testable without a network.
///
/// `None` means there is nothing to reason from (no source bitrate, or the transfer never
/// returned), which is deliberately distinct from a completed slow probe.
pub(super) fn measure_remote_original(url: &str, source_kbps: i64) -> Option<crate::abr::CapacityObservation> {
    let Some(plan) = remote_probe_plan(source_kbps) else {
        crate::player::log(
            "auto: remote Original unavailable — source bitrate is unknown; using HLS",
        );
        return None;
    };
    // `url` already names the playback's own identity. Direct play samples the Part; a remux
    // Original samples start.mkv. A throwaway `source-N` forces an exact miss and makes PMS run a
    // second AdHoc admission decision whose 500 says nothing about transport capacity.
    let sample = match crate::curlio::sample_throughput_result(
        url,
        plan.target_bytes,
        std::time::Duration::from_millis(plan.budget_ms),
        std::time::Duration::from_millis(plan.budget_ms),
    ) {
        Ok(sample) => sample,
        Err(failure) => {
            crate::player::log(&format!(
                "auto: remote Original preflight produced no capacity sample failure={failure:?}; using HLS"
            ));
            return None;
        }
    };
    let measured = sample.kbps();
    crate::player::log(&format!(
        "auto: remote Original probe source={source_kbps}kbps sample={}KiB/{}ms measured={measured}kbps complete={}",
        sample.bytes / 1024,
        sample.elapsed.as_millis(),
        sample.target_reached as i32,
    ));
    Some(crate::abr::CapacityObservation {
        kbps: u32::try_from(measured).unwrap_or(u32::MAX),
        bytes: u64::try_from(sample.bytes).unwrap_or(0),
        active_us: u64::try_from(sample.elapsed.as_micros()).unwrap_or(u64::MAX),
        completed: sample.target_reached,
    })
}

/// PMS 1.43 503s a Part GET after a transcode MDE. The Original we would actually play is a
/// codec-copy remux, so the Remote capacity sample has to be that `start.mkv`, under the same
/// session identity a direct Part probe uses.
///
/// Encoder spin-up stays inside the existing 4s probe budget; a miss fails toward HLS rather
/// than waiting longer. First-byte wait on this sample is the remux coming up, not a measure of
/// transport capacity.
///
/// Issue #266: with an enhanced `audio` this is the FIRST decision the params reach, so the
/// server's refusal (or silent ignoring) of them is caught here, not read as "no Original": the
/// probe re-asks once with `NONE` on the same session — replacing the enhanced registration
/// before any enhanced `start.mkv` is fetched — samples that plain remux, and reports
/// [`RemuxProbe::enhancement_refused`] so the play is built without the params and records it.
pub(super) fn measure_remote_remux(
    off: &plx_base::task::OffFrame,
    client: &plx_plex::plex::Client,
    rk: &str,
    session: &str,
    audio_stream_id: i64,
    subtitle_stream_id: i64,
    source_kbps: i64,
    // Issue #266: the DSP the play-path decision will carry on this same session, so the sample
    // is the remux that plays (`build_stream`'s `pre_audio`); `NONE` without Plex Pass.
    audio: plx_plex::plex::AudioEnhancements,
) -> RemuxProbe {
    // Never samples the Burn shape (M7): the bandwidth this probe measures is the uncapped remux's,
    // and `build_stream`'s own flavor decision (which reads `enhancement_route` directly) is what
    // actually forces a re-encode when a subtitle is being burned — see its doc for why.
    let spec_for = |audio| {
        transcode_spec_carrying_burn(
            rk,
            session,
            session,
            plx_plex::plex::TranscodeOffset::Fresh,
            audio_stream_id,
            subtitle_stream_id,
            false,
            enhanced_remux_contract(audio, false),
        )
    };
    let mut spec = spec_for(audio);
    let mut decision = client.transcode_decision(off, &spec);
    let mut enhancement_refused = false;
    if audio.any() && enhancement_fallback(decision.as_ref(), audio) == Fallback::Retry {
        // Nothing enhanced has been fetched yet: re-deciding on the same session replaces the
        // enhanced registration, exactly as the play path's own fallback does.
        note_enhancement_refused(" in remote remux preflight; fell back", audio);
        enhancement_refused = true;
        spec = spec_for(plx_plex::plex::AudioEnhancements::NONE);
        decision = client.transcode_decision(off, &spec);
    }
    let Some(decision) = decision else {
        crate::player::log("auto: remote remux preflight had no /decision; using HLS");
        return RemuxProbe { sample: None, enhancement_refused };
    };
    if refusal(&decision).is_some() {
        crate::player::log("auto: remote remux preflight refused by /decision; using HLS");
        return RemuxProbe { sample: None, enhancement_refused };
    }
    let sample = measure_remote_original(&client.transcode_start_url(&spec).to_url(), source_kbps);
    if sample.is_none() {
        // Same playback identity the HLS/remux that follows will register. closeResourceSession=1
        // would 503 that next start; physical-stop keeps the Streaming Resource.
        let _ = client.transcode_stop_physical(session);
    }
    RemuxProbe { sample, enhancement_refused }
}

/// What [`measure_remote_remux`] learned: the capacity sample (`None` = no usable sample, fall to
/// HLS), and whether the server refused or ignored the enhancement it was asked for — in which case
/// the sample is of the PLAIN remux and the play must be built without the params.
pub(super) struct RemuxProbe {
    pub(super) sample: Option<crate::abr::CapacityObservation>,
    pub(super) enhancement_refused: bool,
}

/// MDE handshake result. `None` from [`server_decision`] means the body was missing or unusable:
/// the caller must not Original, but remux is still allowed. `Part.decision=transcode` is a
/// container change, not a video-copy veto — see [`plx_plex::plex::MediaPart::video_forbids_copy`].
pub(super) struct MdeVerdict {
    pub(super) original: bool,
    pub(super) video_forbids_copy: bool,
}

/// Ask PMS whether `rk` should direct-play (`original`) or go through start.mkv. None when the
/// server returns no usable Media decision: the caller must not Original (PMS 1.43 503s a Part
/// without a registered decision) but may still remux or re-encode via a separate
/// `transcode_decision`. Registers the session as a side effect.
///
/// Takes the `Client` rather than looking one up: this runs on the resolve worker, and `rk` is only
/// an item on the server the caller resolved from this playback's captured `ServerId`.
pub(super) fn server_decision(
    c: &plx_plex::plex::Client,
    rk: &str,
    session: &str,
    audio_stream_id: i64,
    subtitle_stream_id: i64,
) -> Option<MdeVerdict> {
    let mc = match c.mde_decision(rk, session, audio_stream_id, subtitle_stream_id) {
        Some(mc) => mc,
        None => {
            // failed fetch OR unparseable (XML/truncated) body — no Original Part
            crate::player::log("decision: no/unparseable response -> no Original");
            return None;
        }
    };
    mde_verdict(&mc)
}

fn mde_verdict(mc: &plx_plex::plex::MediaContainer) -> Option<MdeVerdict> {
    if refusal(mc).is_some() { return None; }
    // Part.decision is the Original-vs-not verdict (Media/container carry none). Video copy
    // is the VIDEO stream's own decision — Part=transcode + video=copy is a remux.
    let part = match mc
        .metadata
        .first()
        .and_then(|m| m.media.first())
        .and_then(|md| md.part.first())
    {
        Some(p) => p,
        None => {
            crate::player::log(&format!(
                "decision: no media (general={:?}) -> no Original",
                mc.general_decision_code
            ));
            return None;
        }
    };
    let original = part.decision == "directplay";
    let video_forbids_copy = part.video_forbids_copy();
    let video = part
        .stream
        .iter()
        .find(|s| s.stream_type == 1)
        .map(|s| s.decision.as_str())
        .unwrap_or("-");
    crate::player::log(&format!(
        "decision: part={} video={video} general={:?} mde={:?} -> {}",
        part.decision,
        mc.general_decision_code,
        mc.mde_decision_code,
        if original {
            "DIRECT PLAY"
        } else if video_forbids_copy {
            "TRANSCODE"
        } else {
            "REMUX"
        }
    ));
    Some(MdeVerdict {
        original,
        video_forbids_copy,
    })
}

pub(super) fn forced_server_decision(
    c: &plx_plex::plex::Client, rk: &str, session: &str, audio: i64, sub: i64,
) -> Option<MdeVerdict> {
    mde_verdict(&c.mde_decision_forced(rk, session, audio, sub)?)
}

/// Select the audio + subtitle streams server-side for the current part before a
/// transcode. The transcoder encodes the part's SELECTED audio and, when a subtitle id is
/// non-zero, BURNS that subtitle (query-param subtitleStreamID does NOT suppress a
/// default-selected sub, only the PUT does). The direct-play profile advertises text and
/// client-rendered bitmap codecs; burn is still the remux/re-encode path when we PUT a
/// positive subtitle id. We PUT subtitleStreamID=0 to keep subs OFF (no burn), or the chosen
/// id to burn it; audioStreamID only when the user switched (else keep default).
///
/// `sid` names the server that owns `part` — the resolve worker passes the id it was given, and the
/// in-playback callers pass [`cur_sid`]. A `Part.id` is server-local, so a PUT sent to the wrong
/// one either 404s or, worse, re-selects streams on a stranger's part that happens to share the
/// number.
pub(super) fn put_selection(_off: &plx_base::task::OffFrame, sid: ServerId, part: i64, aud: i64, sub: i64) {
    if part <= 0 {
        return;
    }
    let c = match plx_plex::plex::client_for(sid) {
        Some(c) => c,
        None => return,
    };
    let st = c.select_streams(&plx_plex::plex::StreamSelection {
        part_id: part,
        audio_stream_id: aud,
        subtitle_stream_id: sub,
    });
    crate::player::log(&format!(
        "select streams: part={part} audio={aud} sub={sub} -> HTTP {st}"
    ));
}

struct QueuedSelection {
    sid: ServerId,
    part: i64,
    aud: i64,
    sub: i64,
}

/// The pending selection PUTs and whether a worker is draining them. One lock covers both, so the
/// worker deciding to exit and a `queue_put_selection` deciding whether to spawn one cannot
/// interleave: either the push lands before the worker's final empty check, or it sees the worker
/// already gone and spawns its own.
struct SelectionQueue {
    pending: std::collections::VecDeque<QueuedSelection>,
    worker_running: bool,
}

static SELECTION_QUEUE: std::sync::Mutex<SelectionQueue> =
    std::sync::Mutex::new(SelectionQueue { pending: std::collections::VecDeque::new(), worker_running: false });

fn selection_queue() -> std::sync::MutexGuard<'static, SelectionQueue> {
    SELECTION_QUEUE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Test-only: every queued PUT has been sent and no worker is still draining.
#[cfg(test)]
fn selection_queue_idle() -> bool {
    let queue = selection_queue();
    !queue.worker_running && queue.pending.is_empty()
}

/// Same PUT as [`put_selection`], run on a worker instead of the frame thread
/// (`commit_audio_selection`/`commit_subtitle_selection` are both reached from
/// `app/playback.rs::commit_track` inside the run-loop's `FrameScope`). Requests are drained
/// strictly FIFO by one serial worker at a time, never in parallel — PMS applies whichever PUT it
/// receives last, so two picks made in quick succession (audio, then subtitle) must land on the
/// server in the order the viewer made them, not in whatever order two independent threads happen
/// to finish their requests.
pub(super) fn queue_put_selection(sid: ServerId, part: i64, aud: i64, sub: i64) {
    if part <= 0 {
        return;
    }
    let spawn = {
        let mut queue = selection_queue();
        queue.pending.push_back(QueuedSelection { sid, part, aud, sub });
        !std::mem::replace(&mut queue.worker_running, true)
    };
    if spawn {
        spawn_selection_worker();
    }
}

fn spawn_selection_worker() {
    let spawned = plx_base::task::spawn_off_frame("put-selection", |off| loop {
        let req = {
            let mut queue = selection_queue();
            let Some(req) = queue.pending.pop_front() else {
                // Nothing left: retire under the same lock the push takes, so a racing
                // `queue_put_selection` either pushed before this check or spawns a fresh worker.
                queue.worker_running = false;
                break;
            };
            req
        };
        put_selection(off, req.sid, req.part, req.aud, req.sub);
    });
    if !spawned {
        // The OS refused the thread. Drop the running flag so a later `queue_put_selection` gets a
        // chance to retry, rather than leaving every future selection stranded in the queue behind
        // a flag nobody will ever clear.
        selection_queue().worker_running = false;
    }
}

/// What the queue told us, as owned data for `apply_plan` to install. `machine_id` is `""` when
/// the cached one is still good.
#[derive(Default)]
pub(super) struct QueueInfo {
    pub(super) machine_id: String,
    pub(super) id: String,
    pub(super) item_id: String,
    pub(super) up_next: Option<UpNext>,
    pub(super) rows: Vec<plx_plex::plex::QueueRow>,
}

/// The queued next episode. Main-thread only, and — like `metadata::playing()` (via
/// `MetadataView`, which hands out a `&'a` borrow of the owner's state) — it hands out a
/// reference the Up Next control reads across a frame, so `apply_plan` (main thread) staying its
/// only writer is what keeps that reference sound. A caller that STARTS the next episode must
/// clone first: `request_play` clears this before the new plan lands.
pub fn up_next(ps: &PlaybackSession) -> Option<&UpNext> {
    ps.up_next.as_ref()
}

/// The current playback's queue rows, in queue order, the row now playing among them — locate it
/// with `plex::queue_index_of(rows, pq_item_id().parse().unwrap_or(0), &cur_rk())`, which is the
/// ONE implementation of the identity rule (item id, rating key as the fallback). Empty until a
/// plan lands, and whenever the queue POST failed.
///
/// MAIN THREAD ONLY. Unlike [`up_next`] this lends the rows to a closure instead of handing out a
/// `&'static`, and that is deliberate: `request_play` FREES this Vec as its first act, so a
/// borrowed row used to start playback would be read after free — exactly the aliasing bug
/// `request_play_up_next`'s by-value signature exists to make unrepresentable. A caller that wants
/// to keep a row past the call clones it out (`with_queue(|q| q.get(i).cloned())`); the borrow
/// checker cannot police a `&'static`, but it does police this.
#[allow(dead_code)] // nothing reads the rows yet — the queue overlay that draws them is its own batch
pub fn with_queue<R>(ps: &PlaybackSession, f: impl FnOnce(&[plx_plex::plex::QueueRow]) -> R) -> R {
    f(&ps.queue)
}

/// Create a PlayQueue for `rk` so the session is a first-class, remote-controllable player and
/// the timeline can carry a real playQueueItemID. Best-effort: on failure the timeline still
/// works, just without the queue ids (and the player without an Up Next).
///
/// PURE: returns owned data for `apply_plan` to install.
///
/// **The machine id is THIS server's, three ways, in order.** It goes into
/// `uri=server://{machineIdentifier}/…`, so naming the wrong server is a POST that either fails or
/// builds a queue nobody asked for.
///   1. **The registry's own id for this client** — it is the key the server is filed under, so it
///      cannot belong to another one. Free, and refreshed whenever the slot is re-pointed.
///   2. `cached`, which `ResolveEnv` only fills in when the cache was learned from *this* server
///      (see [`PlaybackSession::machine_sid`]) — the `install(&Origin, token)` path registers with no id, so
///      for the session's own server rung 1 is empty and this is what saves a round trip.
///   3. `GET /identity`, whose answer travels back in `QueueInfo::machine_id` for `apply_plan` to
///      cache against this server.
pub(super) fn resolve_playqueue(
    c: &plx_plex::plex::Client,
    rk: &str,
    session: &str,
    cached: &str,
    continuous: bool,
) -> QueueInfo {
    let known = c.machine_id();
    // `mid` is the FETCHED id and nothing else: apply_plan's "" means "leave the cache alone", and
    // the first two rungs are already-known values with nothing to write back.
    let mid = if known.is_empty() && cached.is_empty() {
        c.machine_identity().unwrap_or_default()
    } else {
        String::new()
    };
    let effective = if !known.is_empty() {
        known
    } else if !mid.is_empty() {
        &mid
    } else {
        cached
    };
    if effective.is_empty() {
        crate::player::log("playqueue: no machineIdentifier (skip)");
        return QueueInfo::default();
    }
    match c.create_play_queue(effective, rk, session, continuous) {
        Some(q) => {
            let up_next = q.next.as_ref().and_then(up_next_of);
            crate::player::log(&format!(
                "playqueue: id={} item={} remaining={} rows={} next={}",
                q.id,
                q.selected_item_id,
                q.remaining,
                q.items.len(),
                up_next
                    .as_ref()
                    .map(|u| format!("S{}E{} {}", u.season, u.index, u.rk))
                    .unwrap_or_else(|| "-".into())
            ));
            QueueInfo {
                machine_id: mid,
                id: if q.id > 0 {
                    q.id.to_string()
                } else {
                    String::new()
                },
                item_id: if q.selected_item_id > 0 {
                    q.selected_item_id.to_string()
                } else {
                    String::new()
                },
                up_next,
                rows: q.items,
            }
        }
        None => {
            crate::player::log("playqueue: POST failed");
            QueueInfo {
                machine_id: mid,
                ..Default::default()
            }
        }
    }
}

impl ResolveEnv {
    /// Mark this resolve as a hero preview. A preview is direct play or nothing
    /// (`preview::accepts_direct_play`) and an enhancement is a remux by definition, so a preview
    /// never asks for one — without this, a Plex Pass viewer's opt-in would turn every trailer
    /// preview into a refused remux.
    pub(super) fn set_preview(&mut self, preview: bool) {
        self.preview = preview;
        if preview {
            self.audio_enhancements = plx_plex::plex::AudioEnhancements::NONE;
        }
    }

    /// MAIN THREAD ONLY.
    /// `sid` arrives BY VALUE from the caller, which is the whole point: the item being played
    /// carries the server it came from (`PmsMovie`/`UpNext`/`Detail` all hold one now), so a play
    /// raised off a merged shelf resolves against the server that shelf's row belongs to rather
    /// than whichever server happens to be current when the worker gets around to asking.
    fn snapshot(ps: &PlaybackSession, meta: plx_data::metadata::MetadataView<'_>, sid: ServerId, rk: &str) -> ResolveEnv {
        let s = ps;
        ResolveEnv {
            sid,
            // the cache only counts when it was learned from the server this play is against
            machine_id: if s.machine_sid == sid {
                s.machine_id.clone()
            } else {
                String::new()
            },
            audio_sid: cur_audio_sid(ps),
            sub_sid: cur_sub_sid(ps),
            subtitle_override: None,
            cached_item: meta.cached_playing(sid, rk),
            quality: quality(),
            direct_play_mode: direct_play_mode(),
            src_kbps: resolve_src_kbps(meta.current(), sid, rk),
            omit_queue_continuous: false,
            preview: false,
            audio_enhancements: crate::player::audio_enhancements(),
            pass: plx_plex::plex::serverinfo::subscription_of(sid),
            #[cfg(any(test, feature = "test-support"))]
            dv_capability: None,
        }
    }
}

pub fn playback_preview(d: &plx_data::metadata::Detail) -> Option<Preview> {
    playback_preview_with_capability(d, None)
}

fn playback_preview_with_capability(
    d: &plx_data::metadata::Detail,
    capability: Option<plx_platform::devcaps::dv::DvCapability>,
) -> Option<Preview> {
    // A SHOW's container carries no file of its own, so the page answers for the episode its Play
    // button would start — the one the hero is already about. Its frame size and audio list are
    // the show Detail's, which `fetch_item_streams` backfilled from that same episode.
    let (part, vcodec) = match d.on_deck.as_ref().filter(|_| d.part.is_empty()) {
        Some(ep) => (ep.part.as_str(), ep.vcodec.as_str()),
        None => (d.part.as_str(), d.vcodec.as_str()),
    };
    let presentation = match capability {
        Some(capability) => d.dovi.presentation(
            !plx_data::metadata::dv_withheld(),
            capability,
            vcodec.eq_ignore_ascii_case("hevc"),
        ),
        None => d.dovi.presentation_now(vcodec.eq_ignore_ascii_case("hevc")),
    };
    let mode = direct_play_mode();
    if mode == DirectPlayMode::Forced {
        return (!part.is_empty() && part_is_streamable(part)
            && video_feed_supported(vcodec, presentation)
            && d.audio.iter().any(|a| audio_direct_plays(mode, &a.codec, a.channels)))
            .then_some(Preview::DirectPlay);
    }
    let p = playback_preview_of(
        part,
        vcodec,
        d.width,
        d.height,
        presentation,
        &d.audio,
    )?;
    // The user's quality ceiling is the LAST gate `build_stream` applies, so it is the last one
    // here too — and it can only ever downgrade, never promote. Without this the facts row would
    // promise "Direct Play" for a source the rung is about to send to an encoder, which is the
    // exact mismatch this preview's doc says it exists to avoid. `d.bitrate`/`width`/`height` are
    // `Media[0]`'s, i.e. the same numbers `ResolveEnv` hands the resolve.
    let location = plx_plex::plex::client_for(d.sid).and_then(|client| client.link());
    // A detail page has not downloaded the file yet, so it reports what can preserve the source,
    // not a fictitious failed bandwidth result. Remote Auto is measured at Play. Relay remains a
    // conversion because its independent link policy refuses both original-rate flavors.
    let policy = direct_play_policy(mode, flavors_allowed(
        plx_plex::plex::link_policy(location),
        quality_policy(quality(), true, d.bitrate, d.width, d.height),
    ));
    Some(match p {
        Preview::DirectPlay if !policy.direct_play && policy.remux && mode == DirectPlayMode::Disabled => Preview::Remux,
        Preview::DirectPlay if !policy.direct_play => Preview::Converts,
        Preview::Remux if !policy.remux => Preview::Converts,
        _ => p,
    })
}

#[cfg(any(test, feature = "test-support"))]
pub fn playback_preview_with_capability_for_test(
    d: &plx_data::metadata::Detail,
    capability: plx_platform::devcaps::dv::DvCapability,
) -> Option<Preview> {
    playback_preview_with_capability(d, Some(capability))
}

static PLAY_GEN: AtomicU32 = AtomicU32::new(0);
static PLAY_BUSY: AtomicBool = AtomicBool::new(false);

/// Resolve workers (the detached `resolve` thread of [`request_play_inner`]) that have not
/// finished yet, retirement of an abandoned plan included.
///
/// [`cancel_play`] only withdraws the landing: the worker still runs its `build_stream` to the end
/// and then sends the `stop?closeResourceSession=1` for a plan nobody will play, to the server
/// named by the plan's `ServerId`. That is correct in production and a cross-test hazard in the
/// host suite, because `ServerId`s are reused after `reset_servers_for_test`: a worker a finished
/// test left behind resolved its stop against the NEXT test's fixture server, landing in that
/// fixture's request log as its first request (the `lifecycle_regression_tests` flake: "the
/// blocked resolve cannot clean up before it is released", 1 run in 300 alone, 1 in 15 under
/// load). [`wait_resolve_workers_for_test`] is how a test waits its own workers out.
static RESOLVE_WORKERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Counts one resolve worker from before it is spawned until its closure returns, panics, or is
/// dropped unspawned.
struct ResolveWorker;

impl ResolveWorker {
    fn enter() -> Self {
        RESOLVE_WORKERS.fetch_add(1, Ordering::SeqCst);
        ResolveWorker
    }
}

impl Drop for ResolveWorker {
    fn drop(&mut self) {
        RESOLVE_WORKERS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Wait up to `limit` for every resolve worker to finish; `false` if one is still running. A test
/// calls this after [`cancel_play`], while its fixture server can still answer the worker.
#[cfg(any(test, feature = "test-support"))]
pub fn wait_resolve_workers_for_test(limit: std::time::Duration) -> bool {
    let end = std::time::Instant::now() + limit;
    while RESOLVE_WORKERS.load(Ordering::SeqCst) != 0 {
        if std::time::Instant::now() >= end {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    true
}

struct PlayLanding {
    gen: u32,
    trace_generation: u32,
    /// Desired route contract captured before ResolveEnv was projected on the main thread.
    contract_revision: u64,
    plan: Plan,
    rk: String,
}
static PLAY_SLOT: Mutex<Option<PlayLanding>> = Mutex::new(None);
/// Resume intents are tagged with the resolve generation that owns them.  A BACK/cancel or a
/// later Play can therefore never donate an old movie's position to the next landing.
static PLAY_RESUME: Mutex<Option<(u32, i64)>> = Mutex::new(None);

fn retire_plan_resources(resources: AbandonedPlanResources) {
    let Some(client) = plx_plex::plex::client_for(resources.sid) else {
        return;
    };
    // Thread creation failure is rarer than cancellation and must not turn into a permanent
    // server allocation, so a refused worker stops them inline. The normal path keeps this
    // network work off the main thread.
    plx_base::task::spawn_small_or_inline(
        "resolve-abandoned-stop",
        const { &plx_base::task::BlockingLabel::new("abandoned plan stop (worker thread refused)") },
        move || {
            for identity in resources.identities {
                let _ = client.transcode_stop(&identity);
            }
        },
    );
}

/// Retire every exact PMS identity created by a resolve that will never be installed.
///
/// A cold Remote probe can admit a Streaming Resource under `Plan::sess` before the plan owns an
/// engine, and a transcode plan additionally owns `Plan::tsession`. Generation cancellation only
/// decides whether the UI may install the value; it does not make either server object disappear.
fn retire_abandoned_plan(plan: Plan) {
    if let Some(resources) = abandoned_plan_resources(&plan) {
        retire_plan_resources(resources);
    }
}

/// Trace generation owned by the plan that is actually installed. It deliberately remains the
/// outgoing generation while the next plan resolves, because that engine is still alive; its
/// workers carry the same token and are ignored by the newly reset report trace.
static ACTIVE_TRACE_GENERATION: AtomicU32 = AtomicU32::new(0);

pub fn playback_trace_generation() -> u32 {
    ACTIVE_TRACE_GENERATION.load(Ordering::SeqCst)
}

/// True while a resolve is in flight. The HUD's `PlaybackState::Resolving` is this OR a cold resume's
/// rebuild after the plan landed ([`resume_flight_outstanding`], when this is already false): ask
/// `player::state()`, not this, whether a start is still owed.
pub fn play_pending() -> bool {
    PLAY_BUSY.load(Ordering::SeqCst)
}

/// This session is a hero preview. The route backstop must not steal it onto the player.
pub fn is_preview(ps: &PlaybackSession) -> bool {
    ps.preview
}

pub fn clear_preview(ps: &mut PlaybackSession) {
    ps.preview = false;
}

/// Was the installed session resolved for a hero preview? Unlike [`is_preview`], which the
/// preview machinery clears as it retires the session, this is fixed at landing and only the next
/// landing replaces it — so no teardown bookkeeping can turn a trailer into a playback.
///
/// **The one gate on every account write a session makes**: the timeline lease
/// ([`begin_timeline_reporting`]) and the stop scrobble ([`scrobble_stop`]) both ask it, as do the
/// pump's failure arms before any Original→HLS rescue. Read from the INSTALLED session rather
/// than from `request`: a preview requested while a film's engine is still live replaces
/// `request` at the press, and that film's final stop must still be reported.
pub fn preview_request(ps: &PlaybackSession) -> bool {
    ps.preview || ps.resolved_as_preview
}

/// Attach the UI's resume point to the resolve currently in flight.
///
/// `request_play_*` is issued immediately before `app::start_playback`, so the latter knows the
/// position one call later than the former knows the generation.  Tagging here closes that seam:
/// a cancelled or superseded landing cannot consume a bare process-global resume value.
pub fn arm_play_resume(ps: &mut PlaybackSession, resume_ns: i64) -> bool {
    if resume_ns <= 0 || !play_pending() {
        return false;
    }
    let gen = PLAY_GEN.load(Ordering::SeqCst);
    *PLAY_RESUME.lock().unwrap_or_else(|e| e.into_inner()) = Some((gen, resume_ns));
    { let s = &mut *ps; s.requested_resume_ns = resume_ns };
    true
}

/// The server an item on a browsing surface came from. MAIN THREAD.
///
/// Today every surface is drawn from the CURRENT server, so this is `plex::current_server()` — and
/// reading it HERE, on the main thread, at the instant the user pressed Play, is the entire point:
/// from this line on the id travels by value and nothing downstream re-resolves it. When the stored
/// rows carry their own server (the shared-server data-model step), each call site passes `m.sid` /
/// `d.sid` instead and this helper goes away; it exists so there is exactly ONE line to change.
pub fn surface_sid() -> ServerId {
    plx_plex::plex::current_server()
}

/// MAIN THREAD, NON-BLOCKING. On acceptance, publishes the HUD strings immediately, supersedes an
/// in-flight resolve and spawns a worker; the caller flips the route this same frame. While a
/// PMS/native route transition owns the reducer, returns `false` without mutating request/session
/// state, and the caller must leave the current route alone.
///
/// `sid` is the server the ITEM came from, which the caller knows and this function must not guess:
/// with more than one source on Home, the item being started and the server currently being browsed
/// routinely differ, and every id in the playback protocol below (`rk`, the Part, the streams, the
/// PlayQueue, the resume point) belongs to the former.
pub fn request_play(
    ps: &mut PlaybackSession,
    meta: &mut plx_data::stores::metadata::MetadataStore,
    sid: ServerId,
    rk: &str,
    part: &str,
    vcodec: &str,
    acodec: &str,
    title: &str,
    ctx: &str,
) -> bool {
    request_play_inner(
        ps,
        meta,
        PlaybackRequest {
            sid,
            rk: rk.to_owned(),
            part: part.to_owned(),
            vcodec: vcodec.to_owned(),
            acodec: acodec.to_owned(),
            title: title.to_owned(),
            ctx: ctx.to_owned(),
            preview: false,
        },
        None,
        None,
        false,
    )
}

/// Same resolve door as [`request_play`], flagged as a preview. Does not push a route. Resume is
/// zero. The caller keeps the detail page mounted.
pub fn request_preview(
    ps: &mut PlaybackSession,
    meta: &mut plx_data::stores::metadata::MetadataStore,
    sid: ServerId,
    rk: &str,
    part: &str,
    vcodec: &str,
    acodec: &str,
    title: &str,
) -> bool {
    request_play_inner(
        ps,
        meta,
        PlaybackRequest {
            sid,
            rk: rk.to_owned(),
            part: part.to_owned(),
            vcodec: vcodec.to_owned(),
            acodec: acodec.to_owned(),
            title: title.to_owned(),
            ctx: plx_data::metadata::TRAILER_CONTEXT.to_owned(),
            preview: true,
        },
        None,
        None,
        false,
    )
}

/// Common async request transaction. A retry waits for the real stop's PMS work on THIS worker
/// before asking the server to start another encoder. Ordinary requests do not synchronously drain;
/// a replacement timeline lease still waits for any stop announced before its publication.
fn request_play_inner(
    ps: &mut PlaybackSession,
    meta: &mut plx_data::stores::metadata::MetadataStore,
    request: PlaybackRequest,
    retry: Option<RetryContext>,
    trace_generation: Option<u32>,
    drain_previous: bool,
) -> bool {
    let sid = request.sid;
    let rk = &request.rk;
    let part = &request.part;
    let title = &request.title;
    let ctx = &request.ctx;
    if part.is_empty() && rk.is_empty() {
        return false;
    }
    if !begin_playback_request() {
        crate::player::log(
            "playback request: route transition still owns the player; refusing overlapping resolve",
        );
        return false;
    }
    // **The playback funnel's denominator, minted HERE and not where the plan lands.** Every way
    // into playback comes through this one function, including the ones that go on to be refused at
    // `/decision` — and a refusal never reaches the engine, so anchoring the attempt any later
    // would have produced a `playback.failed` with no `playback.requested` before it: a funnel that
    // under-counts exactly the failure it exists to measure. It is after the empty-request guard
    // above, so a press that resolves to nothing is not an attempt.
    let trace_generation = if request.preview {
        0
    } else {
        trace_generation.unwrap_or_else(|| crate::player::report::requested(ps, sid))
    };
    // The fields a play REQUEST owns, as against the ones only a landing may install: the HUD
    // strings (published now, so the pre-roll has a title through the whole resolve) and the five
    // the OUTGOING item leaves behind. Everything else — url, session ids, codecs — stays as it is
    // until `apply_plan` replaces it, which is what lets a still-running playback keep answering
    // for itself while the next one resolves.
    { let s = &mut *ps; {
        s.request = Some(request.clone());
        s.requested_resume_ns = if request.preview {
            0
        } else {
            retry.map_or(0, |r| r.resume_ns.max(0))
        };
        // SAFETY: `s.title`/`s.ctxline` are exactly the fixed C buffers `set_c` is given the length
        // of, taken from the arrays themselves so the two can never disagree.
        unsafe {
            set_c(s.title.as_mut_ptr(), s.title.len(), title);
            set_c(s.ctxline.as_mut_ptr(), s.ctxline.len(), plx_data::metadata::context_label(ctx));
        }
        s.cur_audio = None;
        s.cur_sub_sid = 0;
        s.cur_sub_sidecar = false;
        s.cur_sub_client_drawable = false;
        s.side_subs_refused = false;
        // The outgoing item's enhancement outcome is not this one's; the landing installs its own.
        s.cur_enhancement = EnhancementOutcome::Off;
        // Retire the OUTGOING item's queue before its successor resolves: this names the episode
        // after the one that WAS playing, and leaving it up would offer the Up Next control a
        // stale "next" for the whole resolve window — including, when the user just started that
        // very episode from here, the one now on screen. The retained rows go with it, for the
        // same reason and because a fresh `Vec` also hands their strings back to the allocator.
        s.up_next = None;
        s.queue = Vec::new();
        // …and the PREVIOUS item's refusal, for the same reason and one more: `player::state()`
        // derives `Error` from it, so a verdict left standing would put the failure read-out over
        // the item now being resolved. `play_pending()` outranks it for this frame either way, but
        // a resolve that never lands (a refused spawn) would leave nothing else to clear it.
        s.play_verdict = None;
        s.jail_load_blocked = false;
        s.resolve_failed = false;
    } };
    // …and the outgoing item's track/marker/chapter store, for exactly the reason above: it stays
    // the PREVIOUS leaf's until this resolve lands. See `metadata::retire_playing_item`.
    if !request.preview {
        meta.run(plx_data::stores::metadata::MetadataCmd::RetirePlayingItem);
        reset_track_selection(request.sid, &request.rk, retry);
    }
    // Capture the reducer revision BEFORE projecting the environment. Both happen on the main
    // thread, so a later quality/track edit necessarily advances this revision after the snapshot
    // and makes the landing stale instead of installing an old plan beneath a new checkmark.
    let contract_revision = desired_contract_revision();
    // captured HERE, on the main thread, and moved into the worker — see ResolveEnv
    let mut env = ResolveEnv::snapshot(ps, meta.view(), sid, rk);
    env.omit_queue_continuous = plx_data::metadata::context_omits_queue_continuous(ctx);
    env.set_preview(request.preview);
    if let Some(retry) = retry {
        apply_retry_enhancement(&mut env, retry);
        // `request_play` resets the live selection because that is correct for a new item.  A
        // retry is the SAME item: override the fresh defaults with the selection captured before
        // that reset so a rescue does not silently turn subtitles/audio back to server default.
        env.direct_play_mode = retry.direct_play_mode;
        env.audio_sid = retry.audio_sid;
        env.sub_sid = retry.sub_sid;
        env.subtitle_override = Some(retry.sub_sid);
    }
    let gen = PLAY_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    if let Some(resume_ns) = retry.map(|r| r.resume_ns).filter(|ns| *ns > 0) {
        *PLAY_RESUME.lock().unwrap_or_else(|e| e.into_inner()) = Some((gen, resume_ns));
    }
    PLAY_BUSY.store(true, Ordering::SeqCst);
    let (rk, part, vc, ac) = (request.rk, request.part, request.vcodec, request.acodec);
    let worker = ResolveWorker::enter();
    let spawned = plx_base::task::spawn_off_frame("resolve", move |off| {
        let _worker = worker;
        if drain_previous {
            // The old attempt's `state=stopped` and transcode `/stop` were intentionally moved off
            // the SDL thread.  A user Retry must nevertheless preserve their ordering relative to
            // the replacement encoder, so pay that wait here, on the resolve worker.
            drain_scrobble();
        }
        // catch_unwind OUTSIDE the mailbox write, like load_season: a panicking resolve must still
        // land (as !ok) or PLAY_BUSY latches and the screen wedges on a spinner forever.
        let plan = std::panic::catch_unwind(|| build_stream(off, &rk, &part, &vc, &ac, &env))
            .unwrap_or_else(|_| Plan { direct_play_mode: env.direct_play_mode, ..Default::default() });
        let landing = PlayLanding {
            gen,
            trace_generation,
            contract_revision,
            plan,
            rk,
        };
        let abandoned = {
            let mut slot = PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner());
            if gen != PLAY_GEN.load(Ordering::SeqCst) {
                // Cancellation/supersession happened before publication. There may be no player
                // screen left to pump this landing later, so the worker owns its cleanup now.
                Some(landing.plan)
            } else if slot.as_ref().map(|old| old.gen < gen).unwrap_or(true) {
                // MONOTONE: a newer resolve replaces an older unconsumed landing, and takes over
                // the mailbox only after taking responsibility for that plan's server objects.
                slot.replace(landing).map(|old| old.plan)
            } else {
                // An even newer unconsumed landing already owns the slot.
                Some(landing.plan)
            }
        };
        if let Some(plan) = abandoned {
            retire_abandoned_plan(plan);
        }
    });
    if !spawned {
        // there is no worker, so nothing will ever land: releasing this is what keeps the screen
        // from wedging on a spinner that can never resolve
        PLAY_BUSY.store(false, Ordering::SeqCst);
        { let s = &mut *ps; s.resolve_failed = true };
        let mut resume = PLAY_RESUME.lock().unwrap_or_else(|e| e.into_inner());
        if resume.as_ref().is_some_and(|(owner, _)| *owner == gen) {
            *resume = None;
        }
        settle_failed_resolve_spawn(ps);
    }
    spawned
}

/// Clear the outgoing item's live track selection for a play request. A retry is the SAME item
/// resolved again with the subtitle it had (`RetryContext::sub_sid`, applied to the resolve env
/// below), so the timing offset tuned against that subtitle is carried with it. A genuinely NEW
/// request (no retry) instead resumes whatever this profile last tuned for THIS item, if anything
/// (`Session::subtitle_offset_for`, keyed by machine id + ratingKey, never by track — a track
/// CHANGE within one playback already zeros it, `commit_subtitle_selection`'s own doc). Either way
/// the landing re-clamps it once the subtitle is re-selected (`player::reclamp_subtitle_offset`),
/// since the reset has just deselected the sidecar whose range allows an advance.
fn reset_track_selection(sid: ServerId, rk: &str, retry: Option<RetryContext>) {
    crate::player::reset_audio_track();
    crate::player::reset_subtitle();
    match retry {
        Some(retry) => crate::player::restore_subtitle_offset(retry.sub_offset_ms),
        None => {
            if let Some(offset_ms) = remembered_subtitle_offset(sid, rk) {
                crate::player::restore_subtitle_offset(offset_ms);
            }
        }
    }
}

/// This profile's remembered subtitle sync-timing correction for `rk` on `sid`'s server, if this
/// television has ever tuned one — the read half of [`persist_subtitle_offset`]. `None` when the
/// server is not (yet) registered, which a resolve about to fail for the same reason would find
/// out anyway.
fn remembered_subtitle_offset(sid: ServerId, rk: &str) -> Option<i64> {
    let machine_id = plx_plex::plex::client_for(sid)?.machine_id();
    if machine_id.is_empty() {
        return None;
    }
    plx_plex::plex::session::peek().subtitle_offset_for(&plx_plex::plex::session::current_profile_key(), machine_id, rk)
}

/// Persist the viewer's subtitle sync-timing correction for the CURRENTLY PLAYING item, so the
/// next resume of this exact file starts from where they left it (`remembered_subtitle_offset`'s
/// own doc). Fire-and-forget on the storage worker, like every other in-player preference edit
/// (`player::set_subtitle_tone`) — the live atomic ([`crate::player::set_subtitle_offset`]) is
/// already applied by the time this queues, so a slow or lost write costs only the NEXT resume,
/// never this one.
pub fn persist_subtitle_offset(ps: &PlaybackSession, offset_ms: i64) {
    let Some(machine_id) = plx_plex::plex::client_for(cur_sid(ps)).map(|c| c.machine_id().to_string()) else {
        return;
    };
    if machine_id.is_empty() {
        return;
    }
    let rk = cur_rk(ps);
    let user = plx_plex::plex::session::current_profile_key();
    let wanted = (offset_ms != 0).then_some(offset_ms);
    plx_plex::plex::session::queue_update(move |current| {
        if current.subtitle_offset_for(&user, &machine_id, &rk) == wanted {
            return None;
        }
        let mut next = current.clone();
        next.set_subtitle_offset_for(&user, &machine_id, &rk, wanted);
        Some(next)
    });
}

/// Start a fresh resolve for the item whose terminal error is still on screen.
///
/// The caller owns Engine teardown; this module owns the immutable request descriptor, track
/// selection and generation-bound resume point.  Returning `false` is honest for URL/dev-trigger
/// playback, which never entered the Plex request funnel and therefore has no item to resolve.
pub fn can_retry_current_play(ps: &PlaybackSession) -> bool {
    ps.request.is_some()
}

/// Resume target not yet proven by a presented frame.  A refused retry keeps this so the next
/// quality choice can try again at the same point.
pub fn unpresented_resume_ns(ps: &PlaybackSession) -> i64 {
    ps.requested_resume_ns.max(0)
}

/// The replacement has shown a frame; from now on the live playhead, including a later backward
/// seek, is the only truthful retry position.
pub fn confirm_resume_presented(ps: &mut PlaybackSession) {
    if ps.requested_resume_ns > 0 {
        { let s = &mut *ps; s.requested_resume_ns = 0 };
    }
}

fn current_retry_context(ps: &PlaybackSession, resume_ns: i64) -> RetryContext {
    RetryContext {
        resume_ns: resume_ns.max(0),
        audio_sid: cur_audio_sid(ps),
        sub_sid: cur_sub_sid(ps),
        sub_offset_ms: crate::player::subtitle_offset_ms(),
        direct_play_mode: ps.direct_play_mode,
        suppress_enhancement: false,
    }
}

/// The context a START-FAILURE rescue resolves under: [`current_retry_context`], plus the
/// enhancement suppressed when the failed route carried one. Kept apart from the in-flight
/// contract-change re-resolve (which also builds a `RetryContext`), because that one is not a
/// failure and must keep the viewer's enhancement.
fn rescue_retry_context(ps: &PlaybackSession, resume_ns: i64) -> RetryContext {
    RetryContext {
        suppress_enhancement: ps.cur_contract.audio.any(),
        ..current_retry_context(ps, resume_ns)
    }
}

/// Apply a retry's enhancement decision to the resolve environment it is about to hand the worker.
fn apply_retry_enhancement(env: &mut ResolveEnv, retry: RetryContext) {
    if retry.suppress_enhancement {
        env.audio_enhancements = plx_plex::plex::AudioEnhancements::NONE;
    }
}

/// `direct_play` replaces the failed attempt's Direct Play mode for this resolve (the failure
/// read-out's *Switch to Auto and play*); `None` keeps it.
pub fn retry_current_play(
    ps: &mut PlaybackSession,
    meta: &mut plx_data::stores::metadata::MetadataStore,
    resume_ns: i64,
    direct_play: Option<DirectPlayMode>,
) -> bool {
    let Some(request) = ps.request.clone() else {
        crate::player::log("playback retry: no Plex request descriptor");
        return false;
    };
    crate::player::log(&format!(
        "playback retry: resolving item again at quality {:?}",
        quality(),
    ));
    let retry = retry_context_with(ps, resume_ns, direct_play);
    request_play_inner(ps, meta, request, Some(retry), None, true)
}

/// [`rescue_retry_context`] with the failed attempt's Direct Play mode optionally replaced — the
/// one place a retry can resolve under a different mode than the attempt it repeats.
fn retry_context_with(ps: &PlaybackSession, resume_ns: i64, direct_play: Option<DirectPlayMode>) -> RetryContext {
    let mut retry = rescue_retry_context(ps, resume_ns);
    if let Some(mode) = direct_play {
        retry.direct_play_mode = mode;
    }
    retry
}

/// ASYNC twins of `play_movie` / `play_episode`: identical HUD strings and inputs. On `true`, the
/// network work runs on a worker and the caller flips the route THIS frame; an empty or Busy request
/// returns `false` and leaves the current route alone. `app.rs` drains `pump_play` once a frame and
/// starts the engine when the plan lands.
///
/// `ctx` is the HUD's context line (`year · rating · runtime`). The caller formats it
/// (`app::playback::movie_ctx`) because the runtime string is `ui::fmt`'s and `route` sits below
/// `ui`.
pub fn request_play_movie(ps: &mut PlaybackSession, meta: &mut plx_data::stores::metadata::MetadataStore, m: &PmsMovie, ctx: &str) -> bool {
    if m.part.is_empty() {
        return false;
    }
    // **The ITEM's server, not the browsed one.** This passed `surface_sid()` — i.e. whichever
    // server happens to be current — while the row has carried its own `sid` since item identity
    // became a `(server, key)` pair. Starting a borrowed film therefore sent that film's
    // server-local `rk` and `Part.key` to OUR server, which is a different film or no film at all:
    // owner-reported as "playback from the other server just fails with no error, or starves".
    // Both symptoms are the same cause — our server either refuses the key (nothing to show) or
    // hands back a part that is not the one the pipeline was told to expect.
    //
    // `surface_sid()` stays as the fallback for a row with no server on it: rows built by host
    // tests, and any row parsed before a registry existed, carry `UNSET`.
    request_play(
        ps,
        meta,
        item_sid(m.sid),
        &m.rk,
        &m.part,
        &m.vcodec,
        &m.acodec,
        &m.title,
        ctx,
    )
}

/// The server an item's ids belong to: its own when it has one, else the browsed surface.
///
/// A row's `sid` is `UNSET` only before any server was registered (and in host tests, which build
/// rows by literal). Falling back to the surface there keeps the single-server app exactly as it
/// was, while never letting an item that DOES name its server be resolved against another.
pub fn item_sid(sid: ServerId) -> ServerId {
    if sid.is_set() {
        sid
    } else {
        surface_sid()
    }
}

/// Start the queued next episode. Takes the descriptor BY VALUE, and that is load-bearing rather
/// than stylistic: [`up_next`] hands out a `&'static`, `request_play` clears `up_next` as its
/// first act, and a `&UpNext` argument would therefore be pointing at a dropped `String` by the
/// time this reads it — an aliasing bug the borrow checker cannot see through a `'static`. Callers
/// clone (`route::up_next().cloned()`); the signature is what forces them to.
///
/// The HUD strings mirror the episode layout `draw_hud` uses once `now_playing` lands, so the
/// pre-roll doesn't change shape underneath the user when it does. `ctx` is the context line, the
/// episode kicker (`ui::fmt::episode_kicker(u.season, u.index, &u.ep_title)`): the caller formats it
/// before handing `u` over, because `route` sits below `ui`.
pub fn request_play_up_next(ps: &mut PlaybackSession, meta: &mut plx_data::stores::metadata::MetadataStore, u: UpNext, ctx: &str) -> bool {
    let title = if u.show_title.is_empty() {
        &u.ep_title
    } else {
        &u.show_title
    };
    // The successor comes out of the PlayQueue of the item now playing, so its server is that
    // item's — [`cur_sid`], not whatever surface is behind the player. Falls back to the browsing
    // surface only if nothing is playing, which the Up Next control cannot actually reach.
    let sid = if cur_sid(ps).is_set() {
        cur_sid(ps)
    } else {
        surface_sid()
    };
    request_play(ps, meta, sid, &u.rk, &u.part, &u.vcodec, &u.acodec, title, ctx)
}

/// Supersede an in-flight resolve (BACK during a load). The landing is dropped by generation.
pub fn cancel_play(ps: &mut PlaybackSession) {
    PLAY_GEN.fetch_add(1, Ordering::SeqCst);
    PLAY_BUSY.store(false, Ordering::SeqCst);
    let abandoned = PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner()).take();
    *PLAY_RESUME.lock().unwrap_or_else(|e| e.into_inner()) = None;
    // …and the refusal, because this is the statement that the withdrawn RESOLVE is over. Do not
    // clear the playback trace here: background suspend calls this to prevent a late plan landing,
    // then resumes the same playback without a new `requested`; only the true exit ritual ends the
    // attempt and clears it.
    clear_play_verdict(ps);
    cancel_playback_request(ps, has_url(ps));
    if let Some(landing) = abandoned {
        retire_abandoned_plan(landing.plan);
    }
}

/// What [`pump_play`] does with the next episode's still: start its fetch, nothing else. Installed
/// once at boot by `app` (`app::playback::warm_up_next_still`), because the texture cache it
/// warms is `ui`'s and `route` sits below `ui`. Unset, the landing warms nothing — which is also
/// what the host suite gets, where [`pump_play`] never calls it.
static UP_NEXT_STILL_WARM: std::sync::OnceLock<fn(ServerId, &str)> = std::sync::OnceLock::new();

/// Register the Up Next still prefetch ([`UP_NEXT_STILL_WARM`]). Called once, before the loop;
/// a second registration is ignored.
pub fn install_up_next_still_warm(warm: fn(ServerId, &str)) {
    let _ = UP_NEXT_STILL_WARM.set(warm);
}

/// MAIN THREAD, once a frame. Returns the generation-owned resume point when a playable fresh plan
/// was installed. `Some(0)` means start from the beginning; `None` means no playable landing. A
/// stale landing (and its resume) is dropped.
pub fn pump_play(ps: &mut PlaybackSession, meta: &mut plx_data::stores::metadata::MetadataStore) -> Option<i64> {
    let taken = PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(PlayLanding {
        gen,
        trace_generation,
        contract_revision,
        plan,
        rk,
    }) = taken
    else {
        return None;
    };
    if gen != PLAY_GEN.load(Ordering::SeqCst) {
        let mut resume = PLAY_RESUME.lock().unwrap_or_else(|e| e.into_inner());
        if resume.as_ref().is_some_and(|(owner, _)| *owner == gen) {
            *resume = None;
        }
        retire_abandoned_plan(plan);
        return None; // superseded while in flight
    }
    if contract_revision != desired_contract_revision() {
        PLAY_BUSY.store(false, Ordering::SeqCst);
        let resume_ns = {
            let mut resume = PLAY_RESUME.lock().unwrap_or_else(|e| e.into_inner());
            take_resume_for(&mut resume, gen)
        };
        let request = ps.request.clone();
        let retry = current_retry_context(ps, resume_ns);
        retire_abandoned_plan(plan);
        if let Some(request) = request {
            crate::player::log(
                "playback resolve: desired contract changed in flight; discarding and resolving the latest contract",
            );
            let _ = request_play_inner(ps, meta, request, Some(retry), Some(trace_generation), false);
        } else {
            cancel_playback_request(ps, has_url(ps));
        }
        return None;
    }
    PLAY_BUSY.store(false, Ordering::SeqCst);
    let ok = !plan.url.is_empty();
    // A refusing `/decision` is a real landing (its verdict must still be installed for the error
    // read-out), but it has no Engine and therefore no ACTIVE_ENCODER/scrobble owner. The cold
    // source probe or the decision itself may already have registered the logical resource.
    let refused_resources = (!ok).then(|| abandoned_plan_resources(&plan)).flatten();
    let resume_ns = {
        let mut resume = PLAY_RESUME.lock().unwrap_or_else(|e| e.into_inner());
        take_resume_for(&mut resume, gen)
    };
    // A preview's trace_generation is the placeholder 0 (request_play_inner never asked
    // report::requested() for a real one), so it must not overwrite the funnel's active
    // generation here — doing so would misattribute whatever telemetry a genuinely active,
    // non-preview resolve/trace is still using this counter for.
    if !is_preview(ps) {
        ACTIVE_TRACE_GENERATION.store(trace_generation, Ordering::SeqCst);
    }
    let _start = apply_plan(ps, meta, plan, &rk);
    if let Some(resources) = refused_resources {
        retire_plan_resources(resources);
    }
    // Warm the next episode's still NOW rather than at first draw. The URL has been known since
    // this plan resolved — tens of minutes before the credits — and the fetch is async, so touching
    // it here costs nothing and spares the control a skeleton for one image-transcode round trip at
    // exactly the moment it appears in front of the user. The prefetch itself is `app`'s
    // ([`install_up_next_still_warm`]): a texture warm is `ui`'s and `route` sits below it.
    //
    // It sits HERE, in the once-a-frame pump, rather than inside `apply_plan`: that function's
    // contract is that it is the sole WRITER OF THE SESSION, and a texture prefetch is not part of
    // it. Keeping the two apart also keeps the install reachable from the host suite —
    // the warm pulls in the poster cache and, through it, a GL call the dev Mac cannot link.
    // The host test binary has no GL symbols; this prefetch is visual-only and production-only.
    // Keeping it out of cfg(test) makes the generation/resource transaction above testable
    // without pretending a desktop unit test can exercise the poster texture path.
    #[cfg(not(any(test, feature = "test-support")))]
    if let Some(u) = up_next(ps) {
        if let Some(warm) = UP_NEXT_STILL_WARM.get() {
            warm(item_sid(cur_sid(ps)), &u.thumb);
        }
    }
    ok.then_some(resume_ns)
}

/// MAIN THREAD ONLY: the one place a resolved [`Session`] is installed, + the player's audio-track
/// request. Everything here was previously written from inside `build_stream`, i.e. from whatever
/// thread ran it.
///
/// **ONE assignment**, so the installed session is a value that can be read in one go rather than a
/// sequence of pokes whose end state has to be inferred. Three groups of fields are not the plan's
/// to set and are carried across it explicitly — the HUD strings, the `/identity` cache when this
/// plan learned no id, and the codec quartet when the plan resolved no video codec — and each says
/// below why it stays.
fn apply_plan(ps: &mut PlaybackSession, meta: &mut plx_data::stores::metadata::MetadataStore, plan: Plan, rk: &str) -> Option<RouteStartTransaction> {
    // ACTIVE_ENCODER is the final server-resource owner, even when there is no encoder. A raw
    // Part URL opens/adopts its Streaming Resource under the logical playback id; retaining that
    // id lets scrobble_stop exact-close it while PlaybackSession::tsession stays empty and Direct remains
    // truthfully distinguishable from a transcode. A refusing plan has no playable URL and leaves
    // its cleanup to pump_play's abandoned-resource owner instead.
    let active_encoder = if !plan.tsession.is_empty() {
        plan.tsession.clone()
    } else if !plan.url.is_empty() {
        plan.sess.clone()
    } else {
        String::new()
    };
    let resolve_failed = plan.url.is_empty() && plan.verdict.is_none();
    meta.run(plx_data::stores::metadata::MetadataCmd::InstallPlaying(
        plan.playing,
    ));
    // main thread only — `up_next()`/`with_queue()` lend out of this (see their docs). The rows
    // arrive already projected: the worker never retained a `Metadata` tree to install here.
    { let s = &mut *ps; {
        // The HUD strings belong to the REQUEST, not to the landing: `request_play` published them
        // synchronously at the press, and a plan resolving is not new information about the title.
        let (title, ctxline) = (s.title, s.ctxline);
        // The descriptor belongs to the REQUEST and was published before the resolve worker ran.
        // Carry it across the plan's whole-session assignment exactly like the HUD strings; a
        // refusing plan needs it most, and has no URL from which it could be reconstructed.
        let request = std::mem::take(&mut s.request);
        let requested_resume_ns = s.requested_resume_ns;
        // "" = this plan fetched no id — either one was already known, or it never got as far as
        // asking — so leave the cache alone (`Plan::machine_id` says the same). What IS cached is
        // cached AGAINST its server: the next playback only reuses it when it is playing from the
        // same one (`ResolveEnv::snapshot`). One global cache is how server A's fingerprint ends up
        // in a PlayQueue uri POSTed to server B.
        let (machine_id, machine_sid) = if plan.machine_id.is_empty() {
            (std::mem::take(&mut s.machine_id), s.machine_sid)
        } else {
            (plan.machine_id, plan.sid)
        };
        // A plan with no video codec leaves all four codec fields as they were — the same skip the
        // `if !plan.vcodec.is_empty()` guard here has always made. Every branch of `build_stream`
        // that reached a decision fills `vcodec` first (the REFUSING one included, since it is
        // filled before `/decision` is asked), so an empty one is the no-client exit, the default
        // `Plan` a panicking resolve lands, or a direct play whose caller named no codec — nothing
        // truer to put in the stream pair, which is the Load payload's source of truth. The four
        // move together because `stream_*` is what arrives and `src_*` is what the file is, and the
        // diagnostics read-out needs both.
        let (stream_vcodec, stream_acodec, src_vcodec, src_acodec) = if plan.vcodec.is_empty() {
            (
                std::mem::take(&mut s.stream_vcodec),
                std::mem::take(&mut s.stream_acodec),
                std::mem::take(&mut s.src_vcodec),
                std::mem::take(&mut s.src_acodec),
            )
        } else {
            (plan.vcodec, plan.acodec, plan.src_vcodec, plan.src_acodec)
        };
        let now_ms = s.now_ms;
        let preview = request.as_ref().is_some_and(|r| r.preview);
        *s = PlaybackSession {
            direct_play_mode: plan.direct_play_mode,
            jail_load_blocked: false,
            repair_status: plx_platform::tv::sandbox::State::Idle,
            request,
            requested_resume_ns,
            url: plan.url,
            tsession: plan.tsession,
            // Installed on EVERY landing, not only a refusing one: a plan that resolved is itself
            // the statement that the last refusal is over, and assigning unconditionally is what
            // makes that true without a second clear anyone can forget.
            play_verdict: plan.verdict,
            resolve_failed,
            // The APPLIED enhancement (I9): `contract.audio` is what the server's decision accepted
            // (a refused ask was rebuilt with `NONE`), and `enhancement` grades it.
            cur_contract: plan.contract,
            cur_enhancement: plan.enhancement,
            cur_src: plan.src_measure,
            cur_transport_kbps: plan.transport_kbps,
            cur_source_decodable: plan.source_decodable,
            cur_auto_original_watched: plan.auto_original_watched,
            auto_original: plan.auto_original,
            auto_fixture_base: String::new(),
            // A NEW playback starts a new switch history: the count exists to stop this film
            // flapping, and inheriting the last one's would price a first decision as a fourth.
            auto_switches: 0,
            auto_last_switch: None,
            auto_prior_kbps: plan.auto_prior_kbps,
            auto_bootstrap_rung: plan.auto_bootstrap_rung,
            // The two halves of the playing item's identity, installed together and by the same
            // writer — a ratingKey means nothing without the server it is a key ON. Everything
            // after this point (the track PUT, a transcode seek, the retranscode, the stop, and
            // the 10 s progress reporter engine.rs is about to spawn) resolves its server from it.
            cur_rk: rk.to_string(),
            cur_sid: plan.sid,
            // The carried track, as `build_stream` froze it from the fetched stream. `None` = the
            // plan names the server default, or its track list was never fetched.
            cur_audio: plan.audio,
            // the part/show-selected subtitle (0 = none), so the menu checkmark, the timeline report
            // and any later transcode of this item all agree with what the renderer is told below
            cur_sub_sid: plan.sub_sid,
            // Negative = an external sidecar the client draws itself (`sub_render_ordinal`'s own
            // convention); `None` (Off) never sets `sub_sid` either, so this reads `false` for it.
            cur_sub_sidecar: plan.sub_render_ordinal.is_some_and(|ord| ord < 0),
            // Any ordinal the plan carries is something the app's renderer draws.
            cur_sub_client_drawable: plan.sub_render_ordinal.is_some(),
            side_subs_refused: false,
            cur_sub_pref_lang: plan.sub_pref_lang,
            cur_part_id: plan.part_id,
            sess: plan.sess,
            machine_id,
            machine_sid,
            pq_id: plan.pq_id,
            pq_item_id: plan.pq_item_id,
            src_vcodec,
            src_acodec,
            stream_vcodec,
            stream_acodec,
            stream_fps: plan.fps,
            stream_dovi: plan.dovi,
            stream_dv_decision: plan.dv_decision,
            stream_immersive: plan.immersive,
            title,
            ctxline,
            up_next: plan.up_next,
            queue: plan.queue,
            // The frame tick is the MACHINE's, not the plan's: a landing replaces the session's
            // contents and must not rewind the stamp `Player::set_now` wrote this iteration.
            now_ms,
            preview,
            resolved_as_preview: preview,
        };
    } };
    if let (plx_plex::plex::TranscodeDelivery::FixedHls { .. }, Some(rung)) = (
        ps.cur_contract.delivery,
        ps
            .cur_contract
            .ceiling
            .and_then(crate::abr::Rung::from_ceiling),
    ) {
        install_active_hls(&active_encoder, &ps.url, rung);
    } else {
        install_active_encoder(&active_encoder);
    }
    // Restore the external renderer AND the route's stream identity before publishing the
    // start contract, so timeline reports and later audio/quality transcodes keep this pick.
    // Also over a plain remux: the server carries no subtitle there, so the app draws the sidecar
    // (`side_subs_allowed`). Every other transcode keeps the server's burn and no client sidecar.
    if plan.sub_render_ordinal.is_none()
        && (!is_transcoding(ps) || (ps.cur_contract.remux && original_remux_shape(ps)))
    {
        if let Some(id) = crate::player::sidecar::restore_server_selection(cur_sid(ps), meta.view()) {
            set_subtitle(ps, id);
            ps.cur_sub_sidecar = true;
            ps.cur_sub_client_drawable = true;
        }
    }
    let start = prepare_playback_landing(ps, !ps.url.is_empty());
    // Dev probe `plxnative-partprobe`: a settled server conversion of a real (non-preview) playback.
    #[cfg(feature = "devtriggers")]
    if !ps.url.is_empty() && is_transcoding(ps) && !ps.preview {
        if let Some(request) = ps.request.as_ref() {
            super::partprobe::arm(live_family(ps), ps.cur_sid, ps.cur_rk.clone(), request.part.clone(), ps.tsession.clone());
        }
    }
    // SHARED.desired_audio_idx is read by the DEMUX THREAD on every reopen — main thread only.
    if let Some(ord) = plan.feed_audio_ordinal {
        crate::player::set_audio_track(ord);
    }
    // `request_play` turned subtitles off; apply the resolved part/show selection
    // AFTER that reset (this lands a frame or more later, on the main thread, before the engine
    // starts — so the demuxer's per-block `desired_sub_idx` gate sees it from the first cue).
    if let Some(ord) = plan.sub_render_ordinal {
        crate::player::log(&format!(
            "server-selected subtitle: sid={} render_idx={ord}",
            plan.sub_sid
        ));
        crate::player::request_subtitle(ord);
    }
    // A retry or a remembered per-item correction carried its subtitle offset through the reset
    // (`reset_track_selection`); hold it to the range of the subtitle that actually landed, so an
    // advance never outlives its sidecar.
    crate::player::reclamp_subtitle_offset();
    // A landing is a DISCRETE change to what is on screen, so it owes the present gate a poke —
    // `plx_machine::idle::invalidate`'s call-site list is that module's correctness argument. The caller
    // (`app.rs`'s pump) invalidates only when `pump_play` returns TRUE, and a REFUSING plan returns
    // false by construction (empty url) while flipping the player from Resolving to Error. That it
    // still repainted was an accident of the player route bypassing the gate entirely; here it is
    // the rule instead.
    plx_machine::idle::invalidate();
    start
}

/// What [`prepare_original_remux`] produced (`None` from it = the recovery failed and the held
/// route is untouched).
enum OriginalRemux {
    /// The remux replacement's encoder session, registered and committed as the route's active
    /// encoder. The session is NOT yet written: [`install_original_remux`] does that, on the frame
    /// thread, and must not grade the outcome again.
    Prepared(PreparedOriginalRemux),
    /// The server would not apply the enhancement and the candidate direct-plays: nothing was
    /// published, and the plain Original is the direct play the caller now installs as `Refused`.
    RefusedToDirect,
}

/// Register and commit a codec-preserving Original remux without retiring `plan.expected`'s HLS
/// encoder — the PMS half only (see [`run_original_recovery`]); [`install_original_remux`] writes
/// the session. `PendingOriginal` owns the two-session commit/rollback after the install. `audio`
/// is the recovery's own [`recovery_flavour`] (issue #266): `NONE` for the plain remux, else the
/// params the enhanced Original carries. `known_refused` is set by the caller when this exact
/// playback already had an enhancement refused earlier in the same recovery attempt (the
/// direct-play retry that reaches here with `audio` forced to `NONE`) — the plain remux this call
/// now prepares carries no params of its own to refuse, but it exists only because the server would
/// not honour the ask, so it must still grade `Refused` rather than `Off`.
fn prepare_original_remux(
    off: &plx_base::task::OffFrame,
    plan: &OriginalRecoveryPlan,
    audio: plx_plex::plex::AudioEnhancements,
    force_burn: bool,
    known_refused: bool,
) -> Option<OriginalRemux> {
    let c = plan.client?;
    let rk = plan.rk.as_str();
    let expected = &plan.expected;
    let candidate = &plan.candidate;
    if rk.is_empty() || expected.encoder().is_empty() {
        return None;
    }
    let replacement = next_encoder_session(&plan.namespace);
    let subtitle = plan.subtitle_sid;
    let candidate_audio_sid = candidate.audio.as_ref().map_or(0, |a| a.sid);
    put_selection(off, plan.sid, plan.part_id, candidate_audio_sid, subtitle);
    let spec_for = |audio, force_burn| {
        transcode_spec(
            rk,
            &replacement,
            &replacement,
            plx_plex::plex::TranscodeOffset::from_seconds(plan.offset_secs.max(0)),
            candidate_audio_sid,
            subtitle,
            enhanced_remux_contract(audio, force_burn),
        )
    };
    let mut audio = audio;
    let mut force_burn = force_burn;
    let mut spec = spec_for(audio, force_burn);
    let mut decision = c.transcode_decision(off, &spec);
    let mut enhancement_refused = false;
    // A refused or ignored enhancement must not strand the recovery: the candidate's plain
    // Original is still the route this recovery exists to reach. Fall back once, exactly as the
    // resolve and the remote preflight do, and record `Refused` so no reconcile asks this server
    // again this playback. Nothing enhanced was fetched, so re-deciding on the same replacement
    // session replaces its registration; a direct candidate needs no remux at all.
    if enhancement_fallback(decision.as_ref(), audio) == Fallback::Retry {
        note_enhancement_refused(" in Original recovery; fell back", audio);
        if candidate.feeds_part() {
            let _ = c.transcode_stop(&replacement);
            return Some(OriginalRemux::RefusedToDirect);
        }
        enhancement_refused = true;
        audio = plx_plex::plex::AudioEnhancements::NONE;
        force_burn = false;
        spec = spec_for(audio, force_burn);
        decision = c.transcode_decision(off, &spec);
    }
    if let Some(reason) = decision.as_ref().and_then(refusal) {
        crate::player::log(&format!(
            "abr: Original remux decision refused{}",
            if reason.is_empty() {
                ""
            } else {
                ": server supplied a reason"
            },
        ));
        let _ = c.transcode_stop(&replacement);
        return None;
    }
    let (vcodec, acodec) = decision.as_ref().and_then(decision_codecs).unwrap_or_else(|| {
        (
            candidate.vcodec.clone(),
            remux_output_acodec(audio.any(), candidate.audio.as_ref(), &plan.src_acodec),
        )
    });
    let enhancement = if enhancement_refused || known_refused {
        EnhancementOutcome::Refused
    } else {
        classify_outcome(decision.as_ref(), candidate.audio.as_ref(), audio)
    };
    let url = c.transcode_start_url(&spec).to_url();
    let Some(ticket) = replace_active_encoder_for(expected, &replacement) else {
        let _ = c.transcode_stop(&replacement);
        return None;
    };
    Some(OriginalRemux::Prepared(PreparedOriginalRemux {
        replacement,
        url,
        audio,
        force_burn,
        vcodec,
        acodec,
        enhancement,
        ticket,
    }))
}

/// Write a prepared Original remux into the session — the frame-thread half of
/// [`prepare_original_remux`].
fn install_original_remux(
    ps: &mut PlaybackSession,
    candidate: &AutoOriginalCandidate,
    watched: bool,
    prepared: &PreparedOriginalRemux,
) {
    { let s = &mut *ps; {
        s.url = prepared.url.clone();
        s.tsession = prepared.replacement.clone();
        s.cur_contract = enhanced_remux_contract(prepared.audio, prepared.force_burn);
        s.cur_enhancement = prepared.enhancement;
        s.cur_auto_original_watched = watched;
        s.cur_audio = candidate.audio.clone();
        s.stream_vcodec = prepared.vcodec.clone();
        s.stream_acodec = prepared.acodec.clone();
        s.stream_fps = 0.0;
        clear_output_dv(s);
        s.stream_immersive = false;
    } };
    crate::player::log(&format!("decision output: v={} a={}", prepared.vcodec, prepared.acodec));
}

/// Re-transcode the current item (the session's `cur_rk`) at `offset_secs`, carrying the CURRENT
/// audio + subtitle selection (transcode_base). Used by an audio switch AND by a subtitle
/// (de)select while transcoding. Works from a direct-play OR transcode state — the result
/// is always a transcode (server always emits AC3, so the pipeline's Loaded codec is
/// unchanged). Sets `url` + `tsession`, runs /decision, and returns the new start.mkv URL
/// (the demux re-opens it from byte 0), or None.
///
/// Test-only: the whole of today's rebuild in a row on the calling thread. No shipping caller is
/// left (a claim's rebuild is a flight, the automatic fallback has its own three steps), so it is
/// built only to grade that a failed preparation publishes nothing.
#[cfg(test)]
fn retranscode_for(ps: &mut PlaybackSession, expected: &WorkerTicket, offset_secs: i64) -> Option<String> {
    let contract = plain_rebuild_precheck(ps, expected)?;
    let inputs = prepare_retranscode_inputs(ps, expected, offset_secs)?;
    let off = plx_base::task::OffFrame::for_test();
    select_streams_for_encode(&off, &inputs);
    match try_retranscode(&off, &inputs, contract) {
        RetranscodeWorkerOutcome::Applied(applied) => {
            install_retranscode_outcome(ps, &applied);
            stop_encoder_session(applied.client, applied.superseded);
            Some(applied.url)
        }
        RetranscodeWorkerOutcome::Refused => None,
    }
}

/// The ticket/HLS-sync check `retranscode_for` runs before building the "rebuild today's route"
/// contract — split out so the async claim worker (which never touches `PlaybackSession`) can run
/// it on the main thread once, up front, and hand the resulting [`plx_plex::plex::EncodeContract`]
/// into whichever attempt (primary or Legacy fallback) actually needs it.
fn plain_rebuild_precheck(ps: &mut PlaybackSession, expected: &WorkerTicket) -> Option<plx_plex::plex::EncodeContract> {
    if !is_worker_ticket_current(expected) {
        return None;
    }
    if matches!(
        cur_delivery(ps),
        plx_plex::plex::TranscodeDelivery::FixedHls { .. }
    ) {
        let live = sync_active_hls_to_session(ps);
        if live.as_ref().is_some_and(|(ticket, _)| ticket != expected) {
            return None;
        }
    }
    Some(retranscode_contract(ps))
}

/// The contract a plain rebuild of the current route asks for: today's delivery, ceiling and
/// video-copy rule with `remux: false`, and the enhancement run through the one offer predicate
/// (a re-encode is family `Other`, so NONE) — a pick mid-play never keeps params the offer no
/// longer covers.
///
/// **Two branches keep the route a remux instead of a re-encode.**
///   1. An ENHANCED remux stays one. A pick that leaves the offer standing (another capable track
///      the candidate can carry) makes [`enhancement_step`] `NotInvolved` because the params
///      already match, and its legacy reload lands here; rebuilding that as a re-encode would
///      silently drop the enhancement the viewer still has switched on.
///   2. A PLAIN remux ([`plain_rebuild_is_remux`]): the standing Original candidate on Auto/Original
///      whose carried audio the TV cannot decode. The server copies the video and converts only
///      that audio, so the rebuild asks for exactly that, with no enhancement params.
fn retranscode_contract(ps: &PlaybackSession) -> plx_plex::plex::EncodeContract {
    // A ceiling means a fixed rung was picked, and an enhanced remux is uncapped by definition:
    // keeping it would erase the cap the picker shows (I5).
    let route = enhancement_route(&facts(ps), enhancement_family(ps));
    let keep_enhanced = route.is_some()
        && ps.cur_contract.ceiling.is_none()
        && ps.cur_contract.audio.any()
        && want_live(ps).any();
    if keep_enhanced {
        return enhanced_remux_contract(want_live(ps), matches!(route, Some(EnhancementRoute::Burn)));
    }
    if plain_rebuild_is_remux(ps) {
        // Not the enhancement's remux (`NONE`, on purpose): the Legacy fallback contract is built
        // before a refusal is known, so carrying the Boost/Normalize params here would repeat the
        // ask the server just refused and reject the track pick itself. Wanted-and-offered
        // enhancement is already `enhancement_step`'s job.
        return enhanced_remux_contract(plx_plex::plex::AudioEnhancements::NONE, false);
    }
    re_encode_contract(ps, cur_delivery(ps), cur_ceiling(ps))
}

/// A re-encode of the current item at `delivery`/`ceiling`: never a remux, the video-copy rule
/// carried, and the enhancement run through the one offer predicate for family `Other` (so NONE).
/// [`retranscode_contract`] asks it for today's route; an automatic HLS fallback asks it for the
/// route it is about to become.
fn re_encode_contract(
    ps: &PlaybackSession,
    delivery: plx_plex::plex::TranscodeDelivery,
    ceiling: Option<plx_plex::plex::Ceiling>,
) -> plx_plex::plex::EncodeContract {
    plx_plex::plex::EncodeContract {
        remux: false,
        delivery,
        no_video_copy: is_no_video_copy(ps),
        ceiling,
        audio: desired_audio(
            crate::player::audio_enhancements(),
            enhancements_offered(&facts(ps), RouteFamily::Other),
        ),
    }
}

/// **Does a plain rebuild of this route stay the Original remux?** A standing Original candidate
/// on Auto/Original quality, no fixed rung, progressive MKV, video still copyable and no subtitle
/// on screen: the viewer is hearing a track the TV cannot decode, the server copies the video and
/// converts only that audio, and a rebuild (a seek, a track pick) must ask for exactly that
/// instead of a capped re-encode. No live-family term, so turning a burned subtitle off returns
/// to the remux. `cur_sub_sid == 0` is required: every transcode rebuild with a subtitle sends
/// `subtitles=burn`, which the remux shape does not honour.
///
/// Not a loop: the automatic Original -> HLS fallback writes a ceiling and `FixedHls` into the
/// contract before it rebuilds, a fixed rung fails `enhancement_quality`, and a refused remux is
/// rejected with the current stream retained rather than retried.
fn plain_rebuild_is_remux(ps: &PlaybackSession) -> bool {
    original_remux_shape(ps) && (ps.cur_sub_sid == 0 || side_subs_allowed(ps))
}

/// **Is the live session the Original remux's SHAPE**, whatever subtitle is on screen: a standing
/// Original candidate on Auto/Original quality, no fixed rung, progressive MKV, video still
/// copyable. [`plain_rebuild_is_remux`] adds the subtitle term; [`side_subs_allowed`] adds who
/// draws it.
fn original_remux_shape(ps: &PlaybackSession) -> bool {
    enhancement_quality()
        && ps.auto_original.is_some()
        && ps.cur_contract.ceiling.is_none()
        && cur_delivery(ps) == plx_plex::plex::TranscodeDelivery::ProgressiveMkv
        && !is_no_video_copy(ps)
        // A direct-played Dolby Vision Profile 5 never set `no_video_copy` (the declaration made
        // the direct play right); a copy of it, one container down, carries no declaration.
        && ps.auto_original.as_ref().is_some_and(|c| !c.dovi.base_layer_unusable())
}

/// Can the app read an EMBEDDED track from the remux's own stream (or a second demux of the
/// source) to draw it? Nothing can yet — only external sidecars, which the app fetches on its
/// own, are drawn over a remux — so an embedded subtitle on a remux is still the server's burn.
fn side_reader_admitted(_ps: &PlaybackSession) -> bool {
    false
}

/// [`side_subs_allowed`] for a prospective pick: a subtitle is chosen, the app can draw it, no
/// failure has retired the app's drawing for this playback, the route is the Original remux's
/// shape, and nothing but the app has to read the track (a sidecar, or an admitted reader).
fn side_subs_allowed_for(ps: &PlaybackSession, sid: i64, drawable: bool, sidecar: bool) -> bool {
    sid != 0
        && drawable
        && !ps.side_subs_refused
        && original_remux_shape(ps)
        && (sidecar || side_reader_admitted(ps))
}

/// **May the app draw the selected subtitle over a remux** instead of the server burning it?
fn side_subs_allowed(ps: &PlaybackSession) -> bool {
    side_subs_allowed_for(ps, ps.cur_sub_sid, ps.cur_sub_client_drawable, ps.cur_sub_sidecar)
}

/// Log the harness-readable pair every enhancement-graded decision produces, shared by
/// `install_retranscode_outcome` (a claim flight's install), `install_auto_hls_outcome` and
/// `route::plan`'s cold-start branch — the callers that classify a fresh `/decision` body and must
/// therefore agree on the exact wording.
/// `output_codecs` is `None` when no decision body was available to read a codec pair from (the
/// `decision output:` line is then skipped, matching the caller having nothing to report); the
/// `enhancement: applied ..` line is printed only when `outcome` is [`EnhancementOutcome::Applied`]
/// — a harness-readable statement of what actually took effect, distinct from the ask, so an
/// on-device case grading a live toggle (or a cold-start Burn) has one line to key on instead of
/// inferring the outcome from the codec/URL lines above.
pub(super) fn log_enhancement_outcome(
    output_codecs: Option<(&str, &str)>,
    outcome: EnhancementOutcome,
    audio: plx_plex::plex::AudioEnhancements,
) {
    if let Some((v, a)) = output_codecs {
        crate::player::log(&format!("decision output: v={v} a={a}"));
    }
    if matches!(outcome, EnhancementOutcome::Applied) {
        crate::player::log(&format!(
            "enhancement: applied boost={} loudness={}",
            i32::from(audio.boost_dialog),
            i32::from(audio.normalize_loudness),
        ));
    }
}

/// Everything the retranscode network half ([`request_retranscode`]) needs, owned so it runs on
/// a flight worker instead of the frame thread (see [`execute_retranscode_claim`]'s doc for the crash this exists to fix).
/// Captured once, on the main thread, from a `PlaybackSession` the worker never sees again.
#[derive(Clone)]
pub(super) struct RetranscodeClaimInputs {
    client: &'static plx_plex::plex::Client,
    sid: ServerId,
    rk: String,
    part_id: i64,
    audio_sid: i64,
    subtitle_sid: i64,
    namespace: String,
    offset_secs: i64,
    expected: WorkerTicket,
    src_vcodec: String,
    src_acodec: String,
    carried_audio: Option<CarriedAudio>,
}

/// What one accepted `/decision` attempt produced: the session the worker started and committed
/// as the route's encoder, and everything the main thread needs to install, retire or discard it.
pub(super) struct AppliedRetranscode {
    pub(super) qsess: String,
    url: String,
    vcodec: String,
    acodec: String,
    contract: plx_plex::plex::EncodeContract,
    enhancement: EnhancementOutcome,
    /// Returned by `replace_active_encoder_for`/`replace_active_hls_for` at the moment this
    /// worker actually committed the route. `take_ready_flight` re-checks it against
    /// the CURRENT route before applying: the worker's own ticket check ran before this commit,
    /// so a route change landing in the gap between the commit and the main thread draining the
    /// mailbox would otherwise go unnoticed.
    pub(super) ticket: WorkerTicket,
    /// The server that owns `qsess`, so a landing discarded as stale
    /// ([`discard_retranscode_claim_slot`]) can stop the session it just started.
    pub(super) client: &'static plx_plex::plex::Client,
    /// The encoder session the route was playing before this attempt replaced it (empty when
    /// there was none). NOT stopped by the attempt: the stream it feeds is still the one on
    /// screen until the landing reloads, so the caller retires it only after that reload
    /// (see [`retire_superseded_encoder`]).
    pub(super) superseded: String,
}

/// What one `/decision` attempt produced.
pub(super) enum RetranscodeWorkerOutcome {
    Applied(AppliedRetranscode),
    Refused,
}

/// The audio codec a codec-preserving remux is GUESSED to deliver when its `/decision` cannot be
/// read (the answer, when there is one, replaces this). An enhanced remux re-encodes the audio
/// (I4), and a carried track the profile cannot admit is converted by the server, both to the
/// profile's first target (`ac3`) — the source codec would describe bytes that never arrive
/// (silent audio). A plain remux of an admissible track copies it; with no carried track (the
/// server default) it falls back to the file's own default codec.
fn remux_output_acodec(
    enhanced: bool, carried: Option<&CarriedAudio>, src_acodec: &str,
) -> String {
    let carried = carried.filter(|a| !a.codec.is_empty());
    if enhanced || carried.is_some_and(|a| !plx_plex::plex::is_dp_audio_track(&a.codec, a.channels)) {
        "ac3".to_owned()
    } else {
        carried.map_or_else(|| src_acodec.to_owned(), |a| a.codec.clone())
    }
}

/// The fallback codecs a retranscode attempt falls back to, as a pure function of the owned
/// inputs plus whichever contract this attempt is building — needed twice now (a primary attempt
/// and, on refusal, a Legacy fallback attempt may build a different contract), so it is a function
/// rather than a one-shot local.
fn retranscode_fallback_codecs(
    inputs: &RetranscodeClaimInputs,
    contract: &plx_plex::plex::EncodeContract,
) -> (String, String) {
    if contract.remux {
        (
            inputs.src_vcodec.clone(),
            remux_output_acodec(contract.audio.any(), inputs.carried_audio.as_ref(), &inputs.src_acodec),
        )
    } else if matches!(contract.delivery, plx_plex::plex::TranscodeDelivery::FixedHls { .. }) {
        ("h264".to_owned(), "aac".to_owned())
    } else {
        (
            plx_platform::devcaps::caps().encode_vcodec().to_owned(),
            "ac3".to_owned(),
        )
    }
}

/// The PUT that drives the encode and the burn, sent once per claim BEFORE its first
/// [`try_retranscode`] attempt: every attempt of a claim shares `inputs`, so a fallback attempt
/// would send the identical PUT again. Skipped for a stale ticket, exactly as the attempt's own
/// gate refuses to start.
pub(super) fn select_streams_for_encode(off: &plx_base::task::OffFrame, inputs: &RetranscodeClaimInputs) {
    if is_worker_ticket_current(&inputs.expected) {
        put_selection(off, inputs.sid, inputs.part_id, inputs.audio_sid, inputs.subtitle_sid);
    }
}

/// What one accepted `/decision` answered, before the route has named it: the encoder session the
/// server registered, where it starts and what it will carry. The worker half of an attempt
/// ([`request_retranscode`]) produces it and touches no route state; [`commit_retranscode`] makes it
/// the route's, on whichever thread owns that decision.
pub(super) struct PreparedEncode {
    pub(super) qsess: String,
    url: String,
    vcodec: String,
    acodec: String,
    contract: plx_plex::plex::EncodeContract,
    enhancement: EnhancementOutcome,
}

/// One `/decision` attempt, gated on `inputs.expected` throughout — the network body of a
/// retranscode, run on a flight worker (`route::flight`) rather than the frame thread, less the
/// selection PUT ([`select_streams_for_encode`]) and less the commit ([`commit_retranscode`]).
/// Pure with respect
/// to `PlaybackSession` (never sees one) and to the route: everything it needs is in
/// `inputs`/`contract`, and everything it decides is returned rather than written. A refusal has
/// already stopped the session it registered.
pub(super) fn request_retranscode(
    off: &plx_base::task::OffFrame,
    inputs: &RetranscodeClaimInputs,
    contract: plx_plex::plex::EncodeContract,
) -> Option<PreparedEncode> {
    if !is_worker_ticket_current(&inputs.expected) {
        return None;
    }
    let plx_plex::plex::EncodeContract { audio, .. } = contract;
    let (fallback_vcodec, fallback_acodec) = retranscode_fallback_codecs(inputs, &contract);
    let qsess = next_encoder_session(&inputs.namespace);
    let sp = transcode_spec(
        &inputs.rk,
        &qsess,
        &qsess,
        plx_plex::plex::TranscodeOffset::from_seconds(inputs.offset_secs.max(0)),
        inputs.audio_sid,
        inputs.subtitle_sid,
        contract,
    );
    let Some(decision) = inputs.client.transcode_decision(off, &sp) else {
        let _ = inputs.client.transcode_stop(&qsess);
        return None;
    };
    // A live toggle the server refuses (or silently ignores — audio `copy` despite the params)
    // is a rejected action: the current stream is retained and the menu shows what plays.
    if enhancement_fallback(Some(&decision), audio) == Fallback::Retry {
        note_enhancement_refused("; current stream retained", audio);
        let _ = inputs.client.transcode_stop(&qsess);
        return None;
    }
    if let Some(reason) = refusal(&decision) {
        crate::player::log(&format!(
            "retranscode decision refused{}",
            if reason.is_empty() {
                ""
            } else {
                ": server supplied a reason"
            },
        ));
        let _ = inputs.client.transcode_stop(&qsess);
        return None;
    }
    let output_codecs = decision_codecs(&decision).unwrap_or((fallback_vcodec, fallback_acodec));
    Some(PreparedEncode {
        url: inputs.client.transcode_start_url(&sp).to_url(),
        vcodec: output_codecs.0,
        acodec: output_codecs.1,
        contract,
        enhancement: classify_outcome(Some(&decision), inputs.carried_audio.as_ref(), audio),
        qsess,
    })
}

/// Make a prepared encoder the route's, gated on `inputs.expected`: a concurrent ABR commit or a
/// teardown that won while the decision request was in flight hands the encoder back (`Err`) for
/// the caller to stop — a worker stops it inline, the frame thread through
/// [`stop_encoder_session`] — and nothing reloads onto a session which no longer belongs to this
/// playback generation.
pub(super) fn commit_retranscode(
    inputs: &RetranscodeClaimInputs,
    prepared: PreparedEncode,
) -> Result<AppliedRetranscode, PreparedEncode> {
    let replacement_ticket = match (
        prepared.contract.delivery,
        prepared.contract.ceiling.and_then(crate::abr::Rung::from_ceiling),
    ) {
        (plx_plex::plex::TranscodeDelivery::FixedHls { .. }, Some(rung)) => {
            replace_active_hls_for(&inputs.expected, &prepared.qsess, &prepared.url, rung, None)
        }
        _ => replace_active_encoder_for(&inputs.expected, &prepared.qsess),
    };
    let Some(ticket) = replacement_ticket else {
        return Err(prepared);
    };
    // The encoder this replaces keeps running: it still feeds the engine on screen (paused at the
    // claim offset, for a claimed retranscode) until the landing reloads onto `qsess`. Stopping it
    // here killed the live stream seconds early, and on the synchronous callers put one more
    // blocking round trip on the frame thread.
    let superseded = Some(inputs.expected.encoder())
        .filter(|old| !old.is_empty() && *old != prepared.qsess)
        .map(str::to_owned)
        .unwrap_or_default();
    // NEVER log the URL. `transcode_start_url` ends in `X-Plex-Token=…`, and this line is reached
    // by an ordinary audio-track switch — so the app's own support channel ("send us
    // /tmp/plxnative-events.log") was asking users to paste a live PMS credential into a public
    // issue thread. The rk, the track ids and the offset are the whole diagnostic value here; the
    // URL added nothing that is not derivable from them.
    crate::player::log(&format!(
        "retranscode rk={} audio={} sub={} offset={} -> transcode start",
        inputs.rk, inputs.audio_sid, inputs.subtitle_sid, inputs.offset_secs,
    ));
    let PreparedEncode { qsess, url, vcodec, acodec, contract, enhancement } = prepared;
    Ok(AppliedRetranscode {
        qsess,
        url,
        vcodec,
        acodec,
        contract,
        enhancement,
        ticket,
        client: inputs.client,
        superseded,
    })
}

/// [`request_retranscode`] then [`commit_retranscode`], both on a worker: the claim worker's whole
/// attempt. A refused commit stops its own encoder inline, which a worker may.
pub(super) fn try_retranscode(
    off: &plx_base::task::OffFrame,
    inputs: &RetranscodeClaimInputs,
    contract: plx_plex::plex::EncodeContract,
) -> RetranscodeWorkerOutcome {
    let Some(prepared) = request_retranscode(off, inputs, contract) else {
        return RetranscodeWorkerOutcome::Refused;
    };
    match commit_retranscode(inputs, prepared) {
        Ok(applied) => RetranscodeWorkerOutcome::Applied(applied),
        Err(refused) => {
            let _ = inputs.client.transcode_stop(&refused.qsess);
            RetranscodeWorkerOutcome::Refused
        }
    }
}

/// Snapshot everything [`try_retranscode`] needs off `ps`, or `None` when the claim has no PMS
/// work to do.
fn prepare_retranscode_inputs(
    ps: &PlaybackSession,
    expected: &WorkerTicket,
    offset_secs: i64,
) -> Option<RetranscodeClaimInputs> {
    if forced_direct_play(ps) {
        return None;
    }
    let client = cur_client(ps)?;
    let rk = cur_rk(ps);
    if rk.is_empty() || !is_worker_ticket_current(expected) {
        return None;
    }
    let logical = sess(ps);
    let namespace = if logical.is_empty() {
        format!("plxnative-{rk}")
    } else {
        logical
    };
    Some(RetranscodeClaimInputs {
        client,
        sid: cur_sid(ps),
        part_id: cur_part_id(ps),
        audio_sid: cur_audio_sid(ps),
        subtitle_sid: cur_sub_sid(ps),
        namespace,
        offset_secs,
        expected: expected.clone(),
        src_vcodec: ps.src_vcodec.clone(),
        src_acodec: ps.src_acodec.clone(),
        carried_audio: ps.cur_audio.clone(),
        rk,
    })
}

/// The fields a successful attempt changes, on a projection rather than on the session: the one
/// list [`install_retranscode_outcome`] writes to `ps` AND [`advance_claim_snapshot`] writes to the
/// claim's snapshot, so the reducer's restore point can never describe a stream the claim already
/// replaced.
pub(super) fn apply_retranscode_outcome_to_projection(p: &mut AppliedRouteProjection, applied: &AppliedRetranscode) {
    p.contract = applied.contract;
    p.enhancement = applied.enhancement;
    p.tsession = applied.qsess.clone();
    p.url = applied.url.clone();
    p.stream_vcodec = applied.vcodec.clone();
    p.stream_acodec = applied.acodec.clone();
    p.stream_fps = 0.0;
    p.stream_dovi = plx_data::metadata::Dovi::NONE;
    p.stream_dv_decision = plx_data::metadata::DvDecision::NONE;
    p.stream_immersive = false;
}

/// Install a successful attempt's session projection. Publishes `cur_contract` and
/// `cur_enhancement` only after PMS accepted it — the applied enhancement is never written at the
/// selection (I9) — the rule both the sync and worker
/// paths follow.
pub(super) fn install_retranscode_outcome(ps: &mut PlaybackSession, applied: &AppliedRetranscode) {
    let mut projection = route_projection(ps);
    apply_retranscode_outcome_to_projection(&mut projection, applied);
    install_route_projection(ps, &projection);
    let s = &*ps;
    // Logged HERE, where the outcome actually becomes the session's, and not in the worker: a
    // landing that goes stale before the drain must not print "enhancement: applied" for a route
    // that never took effect.
    log_enhancement_outcome(
        Some((s.stream_vcodec.as_str(), s.stream_acodec.as_str())),
        applied.enhancement,
        applied.contract.audio,
    );
}

// ---- issue #266: the live audio-enhancement state machine ------------------------------------
//
// The enhancement decorates the Original route: turning it on makes a Direct/Remux playback an
// enhanced remux, turning it off (or losing the offer — a subtitle, a track the server cannot
// analyse) returns it to the frozen `AutoOriginalCandidate`. Every mid-play change funnels through
// ONE reconcile which only ever queues `UserRouteIntent::Retranscode`; the claim then asks
// `enhancement_step` what "Retranscode" means NOW. Deciding at claim rather than at the click is
// what lets several quick picks and toggles coalesce into the route their final state describes,
// with the merge table left exactly as it was.

/// The family of the route playing NOW (see [`RouteFamily`]). Mid-play code and the menu pass
/// this; the resolve and recovery pass the family of the route they are building instead.
pub(super) fn live_family(ps: &PlaybackSession) -> RouteFamily {
    if !is_transcoding(ps) {
        RouteFamily::Direct
    } else if ps.cur_contract.remux
        && ps.cur_contract.delivery == plx_plex::plex::TranscodeDelivery::ProgressiveMkv
    {
        RouteFamily::Remux
    } else {
        RouteFamily::Other
    }
}

/// The offer's inputs, read from the live session alone — no metadata store: the carried track's
/// capability was frozen into `cur_audio` when it was picked, and the candidate carries its own
/// DV facts.
pub(super) fn facts(ps: &PlaybackSession) -> EnhancementFacts<'_> {
    EnhancementFacts {
        pass: plx_plex::plex::serverinfo::subscription_of(cur_sid(ps)),
        base: ps.auto_original.as_ref(),
        carried: ps.cur_audio.as_ref(),
        subtitle_effect: subtitle_effect_of(ps),
        refused: ps.cur_enhancement == EnhancementOutcome::Refused,
    }
}

/// What the picked subtitle does to the route: nothing, a sidecar the client draws itself, or an
/// embedded track the server would have to burn to keep. Two session fields, read in place.
fn subtitle_effect_of(ps: &PlaybackSession) -> SubtitleEffect {
    match ps.cur_sub_sid {
        0 => SubtitleEffect::None,
        _ if ps.cur_sub_sidecar || side_subs_allowed(ps) => SubtitleEffect::Sidecar,
        _ => SubtitleEffect::Embedded,
    }
}

/// The subtitle effect the Audio tab's NOTE reads (M7) — the same fact
/// [`menu_enhancement_availability`] folded into its `Offered`/`Disabled` answer, exposed
/// separately because the note's WORDING needs to tell "no subtitle" apart from "an unaffected
/// sidecar" even though both share the same [`EnhancementRoute::Remux`].
pub fn live_subtitle_effect(ps: &PlaybackSession) -> SubtitleEffect {
    subtitle_effect_of(ps)
}

/// **Who draws the subtitle on screen right now** — the one route fact the Subtitles menu reads
/// (Style and Timing apply only to a subtitle the app draws) and the one the player draws by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubtitlePresenter {
    /// No subtitle is selected.
    None,
    /// The app draws it (direct play): the viewer's Style and Timing reach it.
    Client,
    /// The app draws it OVER a remux: the server copies the video and converts the audio, the
    /// subtitle is not in the stream at all, and Style and Timing reach it as on direct play.
    ClientOverRemux,
    /// The server burns it into the picture: nothing the app has can reach it.
    ServerBurn(BurnReason),
}

impl SubtitlePresenter {
    /// Does the app draw this subtitle itself (so Style and Timing apply)?
    pub fn client_draws(self) -> bool {
        matches!(self, Self::Client | Self::ClientOverRemux)
    }
}

/// Why the server is the one drawing the subtitle — worded for the viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BurnReason {
    /// The server is converting the audio (a remux, or the Original-quality burn), and a remux
    /// stream cannot carry a subtitle.
    AudioConversion,
    /// Any other burn: a quality-limited re-encode, a picture subtitle that must be burned.
    Quality,
}

/// **The live [`SubtitlePresenter`]**, from the committed selection (`cur_sub_sid`, written for
/// every route by `commit_subtitle_selection`) and the live route. A transcode burns whatever is
/// selected and the client then draws nothing (`player_hud::draw_subtitles` is silenced by
/// [`subtitles_burned`] the same way) — except a sidecar over a remux, which the app draws
/// ([`SubtitlePresenter::ClientOverRemux`]); direct play draws it in the app. The reason is the
/// audio conversion when the live family is a remux or the burn is the enhancement's own or the
/// Original-quality one ([`enhancement_family`] reads exactly those as `Remux`), else quality.
pub fn subtitle_presenter(ps: &PlaybackSession) -> SubtitlePresenter {
    if ps.cur_sub_sid == 0 {
        SubtitlePresenter::None
    } else if !is_transcoding(ps) {
        SubtitlePresenter::Client
    } else if ps.cur_contract.remux
        && ps.cur_contract.delivery == plx_plex::plex::TranscodeDelivery::ProgressiveMkv
        && side_subs_allowed(ps)
    {
        SubtitlePresenter::ClientOverRemux
    } else if enhancement_family(ps) == RouteFamily::Remux {
        SubtitlePresenter::ServerBurn(BurnReason::AudioConversion)
    } else {
        SubtitlePresenter::ServerBurn(BurnReason::Quality)
    }
}

/// **Does the server burn the selected subtitle into the picture?** The gate every client-side
/// draw is silenced by: a transcode burns whatever is selected, except a remux the app draws the
/// subtitle over ([`SubtitlePresenter::ClientOverRemux`]).
pub fn subtitles_burned(ps: &PlaybackSession) -> bool {
    is_transcoding(ps) && subtitle_presenter(ps) != SubtitlePresenter::ClientOverRemux
}

/// **Is the live route the enhancement's own Burn (M7)?** A Burn forces `remux: false` to get PMS
/// to actually re-encode the video (burning text into pixels is not a codec copy), which is
/// exactly the contract shape of an ordinary `Other`-family re-encode picked for some unrelated
/// reason (a fixed rung, HLS, a relay). The two are told apart by the contract itself: only the
/// enhancement asks for a video-copying, no-ceiling, progressive-MKV re-encode carrying non-empty
/// `audio` params, whatever the server graded it (`Applied` or `Unverified`) and whether or not a
/// subtitle is still selected — a subtitle turned Off mid-play leaves this contract standing until
/// the rebuild lands, and that rebuild must keep the params.
pub fn live_is_own_burn(ps: &PlaybackSession) -> bool {
    ps.cur_contract.audio.any()
        && ps.cur_contract == enhanced_remux_contract(ps.cur_contract.audio, true)
}

/// **Is the live route Original quality with an embedded subtitle burned in?** The shape the
/// enhancement's own Burn asks for ([`enhanced_remux_contract`] with `force_burn`), standing with
/// or without the DSP params: a standing Original candidate on Auto/Original, a progressive MKV
/// re-encode with no ceiling and the video still copyable, and an embedded subtitle on screen —
/// the subtitle is the ONLY reason the video is re-encoded, nothing is downscaled. A fixed rung
/// (ceiling), HLS (`FixedHls`), a relay, a source that forbids a video copy (declared Dolby
/// Vision) or a sidecar the client draws itself all fail one of these terms and stay an
/// unrelated `Other`-family re-encode.
fn live_is_original_burn(ps: &PlaybackSession) -> bool {
    enhancement_quality()
        && ps.auto_original.is_some()
        && is_transcoding(ps)
        && !ps.cur_contract.remux
        && ps.cur_contract.delivery == plx_plex::plex::TranscodeDelivery::ProgressiveMkv
        && ps.cur_contract.ceiling.is_none()
        && !ps.cur_contract.no_video_copy
        && ps.cur_sub_sid != 0
        && !ps.cur_sub_sidecar
}

/// **The family the enhancement's own bookkeeping should read the live route as.** Identical to
/// [`live_family`] except a live Burn — which is wire-shaped `Other` (`remux: false`) — reads as
/// `Remux`: both the enhancement's own Burn and a plain Original-quality burn with no DSP params
/// (`live_is_original_burn`). Every predicate that asks "is the enhancement's route still
/// standing" must use this, not `live_family`, or an active Burn would appear "not offered" the
/// instant it took effect and get silently released back to the plain candidate. The track picks
/// (`commit_audio_selection`, `commit_subtitle_selection`) use it too; only `legacy_action`
/// still calls `live_family`, where a Burn genuinely needs re-encode-shaped handling.
fn enhancement_family(ps: &PlaybackSession) -> RouteFamily {
    if live_is_own_burn(ps) || live_is_original_burn(ps) {
        RouteFamily::Remux
    } else {
        live_family(ps)
    }
}

/// What the viewer's preference asks of the live route: an Original-family route (Direct or
/// Remux) is judged as the remux an enhancement would make it; anything else is never offered.
/// Under a fixed rung nothing is: the enhanced route is an uncapped remux, so wanting it there
/// would override the bitrate cap the viewer picked (I5) — the quality refresh already queued
/// rebuilds that route as a capped re-encode without the params.
fn want_live(ps: &PlaybackSession) -> plx_plex::plex::AudioEnhancements {
    if !enhancement_quality() {
        return plx_plex::plex::AudioEnhancements::NONE;
    }
    desired_audio(crate::player::audio_enhancements(), audio_enhancements_offered_live(ps))
}

/// The [`EnhancementRoute`] a live `Retranscode` would ask for right now — `None` when the
/// enhancement is not offered at all. `want_live`/`audio_enhancements_offered_live` collapse this
/// to a bool for the menu's toggle; `enhancement_step` needs the flavour too, since a subtitle
/// change can flip Remux↔Burn without changing the boost/loudness preference at all.
fn want_route_live(ps: &PlaybackSession) -> Option<EnhancementRoute> {
    if !enhancement_quality() {
        return None;
    }
    enhancement_route(&facts(ps), enhancement_family(ps))
}

/// Whether the quality preference admits an Original-family route at all — Auto (which may run
/// Original) or Original itself. A fixed rung is a bitrate cap, and the enhanced route is not.
fn enhancement_quality() -> bool {
    matches!(quality(), Quality::Auto | Quality::Original)
}

/// The shape an enhanced Original takes: a codec-preserving progressive-MKV remux, video copied,
/// no ceiling — or, when `force_burn` (M7's [`EnhancementRoute::Burn`]), the same delivery with
/// `remux: false` so PMS actually re-encodes the video and burns the requested subtitle into it
/// (a codec copy cannot alter pixels). `no_video_copy` stays `false` in both cases: I7 keeps a
/// declared-DV source (the one `no_video_copy` reason) out of the offer entirely before this is
/// ever reached.
fn enhanced_remux_contract(
    audio: plx_plex::plex::AudioEnhancements,
    force_burn: bool,
) -> plx_plex::plex::EncodeContract {
    plx_plex::plex::EncodeContract {
        remux: !force_burn,
        delivery: plx_plex::plex::TranscodeDelivery::ProgressiveMkv,
        no_video_copy: false,
        ceiling: None,
        audio,
    }
}

/// What a claimed `Retranscode` must do about the enhancement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnhancementStep {
    /// The enhancement has nothing to change: today's rebuild (or the displaced pick's own
    /// legacy reload) runs.
    NotInvolved,
    /// Rebuild as this remux — on with new params, or a remux candidate's plain remux.
    Remux(plx_plex::plex::EncodeContract),
    /// The enhancement is no longer wanted and the candidate direct-plays: return to it.
    ReleaseToDirect,
}

/// **Pure over session state.** See the table in the plan's §3: a wanted enhancement that differs
/// from the applied one (in preference OR in route flavour — M7's Remux↔Burn) builds the enhanced
/// remux; an applied one no longer wanted returns to the candidate (direct, or its plain remux);
/// everything else — including no candidate at all — is not the enhancement's business.
pub fn enhancement_step(ps: &PlaybackSession) -> EnhancementStep {
    // Under a fixed rung the rebuild is the quality refresh's own (a capped re-encode, which
    // drops the params by family); a release to the uncapped candidate would erase that cap.
    if !enhancement_quality() {
        return EnhancementStep::NotInvolved;
    }
    let want_route = want_route_live(ps);
    let want = desired_audio(crate::player::audio_enhancements(), want_route.is_some());
    let applied = ps.cur_contract.audio;
    let want_burn = matches!(want_route, Some(EnhancementRoute::Burn));
    let applied_burn = live_is_own_burn(ps);
    // A subtitle change while enhanced can leave the boost/loudness preference untouched and still
    // need a rebuild: turning off a burned subtitle must drop the route from Burn back to a plain
    // enhanced remux (M7), which `want == applied` alone would miss.
    if want.any() && (want != applied || want_burn != applied_burn) {
        return EnhancementStep::Remux(enhanced_remux_contract(want, want_burn));
    }
    if applied.any() && !want.any() {
        return match ps.auto_original.as_ref() {
            Some(candidate) if candidate.feeds_part() => EnhancementStep::ReleaseToDirect,
            // An embedded subtitle still on screen is kept by the plain burn, never dropped by a
            // plain remux (which cannot carry it, M4/M7).
            Some(_) => EnhancementStep::Remux(enhanced_remux_contract(
                plx_plex::plex::AudioEnhancements::NONE,
                subtitle_effect_of(ps) == SubtitleEffect::Embedded,
            )),
            None => EnhancementStep::NotInvolved,
        };
    }
    EnhancementStep::NotInvolved
}

/// **Bring the route in line with the enhancement preference.** Returns `true` when it queued the
/// `Retranscode` that will do so — the caller then skips its own legacy reload, and
/// `legacy_reloads` records that it did (`displaced_pick`), so a claim that cannot honour the
/// enhancement still performs the pick's own reload. A pending Original trial owns the route:
/// the reconcile is deferred to whichever route that trial settles on.
pub fn reconcile_enhancement(ps: &PlaybackSession, legacy_reloads: bool) -> bool {
    {
        let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pending) = control.pending_original.as_mut() {
            pending.deferred_reconcile = true;
            return false;
        }
    }
    if enhancement_step(ps) == EnhancementStep::NotInvolved {
        return false;
    }
    queue_user_route_intent(ps, UserRouteIntent::Retranscode, legacy_reloads);
    true
}

/// What the legacy commit branch would do NOW for the current selection — the reload a displaced
/// pick is owed. Mirrors `commit_audio_selection`'s own native/transcode split.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LegacyAction {
    Native { ordinal: i32, codec: String },
    Retranscode,
}

pub fn legacy_action(ps: &PlaybackSession) -> LegacyAction {
    match (live_family(ps), ps.cur_audio.as_ref()) {
        (RouteFamily::Direct, Some(a)) if audio_track_direct_plays(ps, &a.codec, a.channels) => {
            LegacyAction::Native {
                ordinal: a.ordinal,
                codec: a.codec.clone(),
            }
        }
        (RouteFamily::Direct, None) => {
            // Unreachable: a displaced pick always installed `cur_audio` first. Should it ever
            // happen, a rebuild carrying the server default is the reload that cannot mis-feed.
            debug_assert!(false, "enhancement: legacy_action Direct+None");
            crate::player::log("enhancement: legacy_action Direct+None");
            LegacyAction::Retranscode
        }
        _ => LegacyAction::Retranscode,
    }
}

/// The first thing a claimed `Retranscode` tries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimPrimary {
    ReleaseToDirect,
    Remux(plx_plex::plex::EncodeContract),
    /// The displaced pick's own legacy reload ([`legacy_action`]).
    Legacy,
    /// Today's rebuild, exactly: [`plain_rebuild_precheck`]'s contract, one [`try_retranscode`].
    Retranscode,
}

/// What a claim does when its primary effect is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimFallback {
    /// Run the displaced pick's legacy reload inside the same claimed action.
    Legacy,
    /// Settle as rejected; the current stream is retained.
    Reject,
}

impl ClaimFallback {
    /// What a failed claim owes: the displaced pick's legacy reload when there is one to honour,
    /// otherwise a plain rejection.
    fn owed(displaced_pick: bool) -> Self {
        if displaced_pick {
            Self::Legacy
        } else {
            Self::Reject
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dispatch {
    pub primary: ClaimPrimary,
    pub on_failure: ClaimFallback,
}

/// **Pure: the claim-time dispatch table** for a `Retranscode` (plan §6.1). Kept separate from the
/// effects so every cell is unit-testable without PMS or an Engine.
pub fn claim_dispatch(step: EnhancementStep, displaced_pick: bool) -> Dispatch {
    let owed = ClaimFallback::owed(displaced_pick);
    match step {
        EnhancementStep::ReleaseToDirect => Dispatch {
            primary: ClaimPrimary::ReleaseToDirect,
            on_failure: owed,
        },
        EnhancementStep::Remux(contract) => Dispatch {
            primary: ClaimPrimary::Remux(contract),
            on_failure: owed,
        },
        EnhancementStep::NotInvolved if displaced_pick => Dispatch {
            primary: ClaimPrimary::Legacy,
            on_failure: ClaimFallback::Reject,
        },
        EnhancementStep::NotInvolved => Dispatch {
            primary: ClaimPrimary::Retranscode,
            on_failure: ClaimFallback::Reject,
        },
    }
}

/// The physical half the pump performs after a claim's PMS half has run. The PMS half (this
/// module) never calls `finish_route_action` for a tail that reloads; the pump's tail does, once,
/// with the reload it is about to start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimTail {
    /// A transcode route was prepared: reload onto `url`.
    Retranscode,
    /// A rebuilt transcode of the SAME stream was installed ([`RebasePlan`], an `AdaptiveReload`):
    /// reload onto it at the claim offset.
    Adaptive,
    /// A native direct-play audio switch was staged (`desired_audio_idx`, payload codec).
    NativeAudio,
    /// An Original trial was staged; its own PendingOriginal owns commit/rollback.
    Original(AutoOriginalReload),
    /// Refused: settle Rejected with this log line.
    Rejected(&'static str),
}

/// Run the legacy reload a displaced pick is owed, inside the still-claimed action. No new intent
/// is queued, so the user contract is not advanced mid-claim.
fn run_legacy(ps: &mut PlaybackSession) -> Option<ClaimTail> {
    match legacy_action(ps) {
        LegacyAction::Native { ordinal, codec } => {
            crate::player::stage_native_audio(ps, ordinal, &codec);
            Some(ClaimTail::NativeAudio)
        }
        // The rebuild needs the very inputs the plan has just failed to capture (this is only
        // reached once `prepare_retranscode_inputs` refused them: no server, no item, a forced
        // direct play or a moved route), so it can only refuse too.
        LegacyAction::Retranscode => None,
    }
}

pub(super) const RETRANSCODE_REJECTED: &str =
    "route transition: user retranscode was rejected; current stream retained";
pub(super) const ENHANCEMENT_REJECTED: &str = "enhancement change rejected; current stream retained";
pub(super) const MANUAL_ORIGINAL_REJECTED: &str =
    "route transition: manual Original was rejected; current stream retained";
/// An automatic Original recovery the server refused. The Auto worker stopped to hand the action
/// over, so "retained" is the HLS ROUTE: the pump reopens it ([`ClaimTail::Rejected`]'s automatic arm).
pub(super) const AUTO_ORIGINAL_REJECTED: &str = "auto: Original recovery was rejected; reopening retained HLS";
pub(super) const ADAPTIVE_REJECTED: &str = "route transition: adaptive transcode reload was rejected";
pub(super) const SEEK_REJECTED: &str = "seek(transcode): rebuild failed";
pub(super) const LEGACY_REJECTED: &str =
    "enhancement: displaced pick's own reload was rejected; current stream retained";
pub(super) const RETRANSCODE_WORKER_PANICKED: &str =
    "route transition: retranscode worker panicked; current stream retained";

fn claim_fallback(ps: &mut PlaybackSession, fallback: ClaimFallback, reject: &'static str) -> ClaimTail {
    match fallback {
        ClaimFallback::Legacy => run_legacy(ps).unwrap_or(ClaimTail::Rejected(LEGACY_REJECTED)),
        ClaimFallback::Reject => ClaimTail::Rejected(reject),
    }
}

/// What the pump does with [`execute_retranscode_claim`]'s result.
#[derive(Debug)]
pub enum RetranscodeClaimDispatch {
    /// Resolved without reaching PMS: the pump may call `run_claim_tail` this same frame.
    Sync(ClaimTail),
    /// A worker now owns the PMS half; [`take_ready_flight`] collects the result on a
    /// later frame. `ControlPhase` stays `Applying` throughout (`claim_route_action` already
    /// refuses a new claim until then), so nothing else can race it. The pump holds presentation
    /// at the claim offset for the flight (`player::claim_hold`).
    Pending,
    /// [`Pending`](Self::Pending) for an AUTOMATIC flight whose worker never reaches PMS (a direct
    /// Original the Auto worker already sampled): it lands within a frame or two, so there are no
    /// seconds to hold and the stream is not paused for them.
    PendingNoHold,
}

/// PMS half of a claimed user `Retranscode` ("reconcile at claim").
///
/// This is the PLAN step of a **flight** (see [`super::flight`]'s `//!`): it runs on the frame
/// thread, captures owned inputs, and hands them to the claim worker; the worker, the landing
/// mailbox, the drain ([`take_ready_flight`]) and the stale-landing rules all live in
/// `flight.rs`.
///
/// **The freeze this exists to fix:** a live track pick, an enhancement toggle or a quality change
/// used to run `put_selection` (a synchronous PUT) and then `/decision` (a synchronous GET, up to
/// 15 s on stable, aborted at ~2 s by the dev threadcheck watchdog) right here, on the frame
/// thread — so the whole app froze while PMS was slow. Every arm that reaches PMS now runs on a
/// worker: `ClaimPrimary::Remux`/`Retranscode`, Legacy's own `LegacyAction::Retranscode`, and
/// `ClaimPrimary::ReleaseToDirect` — the last split three ways ([`plan_original_recovery`] here on
/// the frame thread, the Part admission probe and any replacement remux's `/decision` on the
/// worker, [`install_original_recovery`] when the drain lands it), because that arm also writes the
/// session and arms `PendingOriginal`, which no worker may. The viewer's Original pick and the
/// Auto watchdog's HLS-to-Original handoff are the same arm ([`execute_recover_original_claim`]); its
/// Original-to-HLS fallback is the same flight ([`execute_auto_hls_claim`]). No route-changing PMS
/// call is left on the frame thread (the foreground resume and every rollback are flights too: [`dispatch_resume_rebase`], [`dispatch_rollback_rebase`],
/// [`dispatch_engineless_rollback_rebase`]).
pub fn execute_retranscode_claim(
    ps: &mut PlaybackSession,
    action: &ClaimedRouteAction,
    offset_secs: i64,
    pending_seek: i64,
    user_target: i64,
) -> RetranscodeClaimDispatch {
    let dispatch = claim_dispatch(enhancement_step(ps), action.displaced_pick);
    plan_claim_flight(
        ps,
        action,
        FlightPlan { offset_secs, pending_seek, user_target },
        dispatch,
        OriginalAttempt { cause: RecoveryCause::EnhancementReleased, rejected: ENHANCEMENT_REJECTED },
    )
}

/// Where a claim's flight is flying to: the offset its PMS half builds at, and the seek state the
/// landing's tail crosses.
#[derive(Clone, Copy)]
struct FlightPlan {
    offset_secs: i64,
    pending_seek: i64,
    user_target: i64,
}

/// What a claim whose primary is `ClaimPrimary::ReleaseToDirect` tries: the recovery's cause, and
/// the line a refusal settles with.
#[derive(Clone, Copy)]
struct OriginalAttempt {
    cause: RecoveryCause,
    rejected: &'static str,
}

/// The ONE plan step of every claim whose PMS half is a flight: pick the work and the owed
/// fallback off the session, capture the owned inputs, begin the flight and hand it to a worker.
/// [`execute_retranscode_claim`] (an enhancement release, a track pick, a quality change) and
/// [`execute_recover_original_claim`] (the viewer's Original pick, the Auto watchdog's handoff)
/// differ only in the [`Dispatch`] they start from and in what a refused Original reports.
fn plan_claim_flight(
    ps: &mut PlaybackSession,
    action: &ClaimedRouteAction,
    flight: FlightPlan,
    mut dispatch: Dispatch,
    original: OriginalAttempt,
) -> RetranscodeClaimDispatch {
    let FlightPlan { offset_secs, pending_seek, user_target } = flight;
    let expected = action.ticket.clone();
    // A release with nothing it may try (a refusal the plan reads off the session alone) owes what
    // `claim_fallback` owed it: the displaced pick's own reload, as a Legacy primary, or a rejection.
    let mut recovery = None;
    if dispatch.primary == ClaimPrimary::ReleaseToDirect {
        recovery = plan_original_recovery(ps, &expected, offset_secs, original.cause);
        if recovery.is_none() {
            dispatch = match dispatch.on_failure {
                ClaimFallback::Legacy => Dispatch { primary: ClaimPrimary::Legacy, on_failure: ClaimFallback::Reject },
                ClaimFallback::Reject => {
                    return RetranscodeClaimDispatch::Sync(ClaimTail::Rejected(original.rejected));
                }
            };
        }
    }
    let holds_presentation = recovery
        .as_ref()
        .is_none_or(|plan| plan.cause != RecoveryCause::Automatic || plan.reaches_pms());
    let work = match dispatch.primary {
        ClaimPrimary::ReleaseToDirect => match recovery.take() {
            Some(plan) => ClaimWork::RecoverOriginal(Box::new(plan)),
            None => return RetranscodeClaimDispatch::Sync(ClaimTail::Rejected(original.rejected)),
        },
        ClaimPrimary::Legacy => match legacy_action(ps) {
            LegacyAction::Native { ordinal, codec } => {
                crate::player::stage_native_audio(ps, ordinal, &codec);
                return RetranscodeClaimDispatch::Sync(ClaimTail::NativeAudio);
            }
            LegacyAction::Retranscode => match plain_rebuild_precheck(ps, &expected) {
                Some(contract) => ClaimWork::Encode(contract),
                None => return RetranscodeClaimDispatch::Sync(ClaimTail::Rejected(LEGACY_REJECTED)),
            },
        },
        ClaimPrimary::Remux(contract) => ClaimWork::Encode(contract),
        ClaimPrimary::Retranscode => match plain_rebuild_precheck(ps, &expected) {
            Some(contract) => ClaimWork::Encode(contract),
            None => return RetranscodeClaimDispatch::Sync(ClaimTail::Rejected(RETRANSCODE_REJECTED)),
        },
    };
    let primary_reject = match dispatch.primary {
        ClaimPrimary::ReleaseToDirect => original.rejected,
        ClaimPrimary::Remux(_) => ENHANCEMENT_REJECTED,
        // A Legacy primary is the displaced pick's own reload, not the enhancement's and not
        // today's plain rebuild — its refusal keeps the label that names what it actually was.
        ClaimPrimary::Legacy => LEGACY_REJECTED,
        ClaimPrimary::Retranscode => RETRANSCODE_REJECTED,
    };
    // The fallback owed on refusal, precomputed the same way: `legacy_action`/the plain-rebuild
    // contract are pure reads of `ps` as it stands right now — the worker never gets another
    // chance to read it.
    let fallback = match dispatch.on_failure {
        ClaimFallback::Reject => RetranscodeFallback::RejectWith(primary_reject),
        ClaimFallback::Legacy => match legacy_action(ps) {
            LegacyAction::Native { ordinal, codec } => RetranscodeFallback::Native { ordinal, codec },
            LegacyAction::Retranscode => match plain_rebuild_precheck(ps, &expected) {
                Some(contract) => RetranscodeFallback::Retranscode(contract),
                None => RetranscodeFallback::RejectWith(LEGACY_REJECTED),
            },
        },
    };
    let Some(inputs) = prepare_retranscode_inputs(ps, &expected, offset_secs) else {
        return RetranscodeClaimDispatch::Sync(claim_fallback(ps, dispatch.on_failure, primary_reject));
    };
    let owner = begin_claim_flight(ps, action);
    if spawn_flight(owner, pending_seek, user_target, Some(inputs), work, fallback) {
        if holds_presentation {
            RetranscodeClaimDispatch::Pending
        } else {
            RetranscodeClaimDispatch::PendingNoHold
        }
    } else {
        // The OS refused the thread: settle exactly like a synchronous rejection. `run_claim_tail`'s
        // `Rejected` arm calls `finish_route_action`, which returns `ControlPhase` to `Stable` —
        // there is no stuck `Applying` the way an unspawned resolve needed `settle_failed_resolve_spawn`
        // to unstick, because `claim_route_action` already reserved this exact serial for us.
        RetranscodeClaimDispatch::Sync(ClaimTail::Rejected(primary_reject))
    }
}

/// The claim a flight carries to its landing: `action` plus the reducer's restore point as it
/// stands right now. A second edit that queues while the worker runs advances
/// `desired_revision`/`desired_quality`/`ps` right away (nothing stops it — the frame thread is
/// free), so `finish_route_action` must publish THIS snapshot rather than whatever those happen to
/// hold when the worker lands.
fn snapshotted_claim(ps: &PlaybackSession, action: &ClaimedRouteAction) -> ClaimedRouteAction {
    let (revision, quality) = {
        let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        (control.desired_revision, control.desired_quality)
    };
    let mut claim = action.clone();
    claim.claim_snapshot = Some(Box::new(ClaimSnapshot { revision, quality, projection: route_projection(ps) }));
    claim
}

/// Record the flight of `action` and return its owner: the claim with the reducer's restore point
/// as it stands now ([`snapshotted_claim`]), begun before the worker can land.
fn begin_claim_flight(ps: &PlaybackSession, action: &ClaimedRouteAction) -> FlightOwner {
    let owner = FlightOwner::Claim(snapshotted_claim(ps, action));
    begin_flight(owner.serial());
    owner
}

/// PMS half of a claimed AUTOMATIC `OriginalToHls`, the Auto watchdog's fallback from an Original
/// that cannot keep up: plan on the frame ([`plan_auto_hls`]), the PMS half on the claim worker
/// ([`run_auto_hls`]), the install at the drain ([`install_auto_hls_outcome`], landing as
/// [`ClaimTail::Retranscode`]) — the same flight as every other claim, owned by the automatic
/// claim, which never carries a displaced pick.
///
/// **Nothing is held for it** ([`RetranscodeClaimDispatch::PendingNoHold`]): the Original is what
/// plays until the landing replaces it, and only a refusal or a success changes that. A user pick
/// made meanwhile WAITS in `pending_user` and is claimed after the landing. A plan the session
/// refuses settles on the spot as [`AUTO_HLS_REJECTED`].
pub fn execute_auto_hls_claim(
    ps: &mut PlaybackSession,
    action: &ClaimedRouteAction,
    offset_secs: i64,
    conservative_kbps: u32,
    pending_seek: i64,
    user_target: i64,
) -> RetranscodeClaimDispatch {
    let Some(plan) = plan_auto_hls(ps, &action.ticket, conservative_kbps, offset_secs) else {
        return RetranscodeClaimDispatch::Sync(ClaimTail::Rejected(AUTO_HLS_REJECTED));
    };
    let owner = begin_claim_flight(ps, action);
    if spawn_flight(
        owner,
        pending_seek,
        user_target,
        None,
        ClaimWork::AutoHls(Box::new(plan)),
        RetranscodeFallback::RejectWith(AUTO_HLS_REJECTED),
    ) {
        RetranscodeClaimDispatch::PendingNoHold
    } else {
        RetranscodeClaimDispatch::Sync(ClaimTail::Rejected(AUTO_HLS_REJECTED))
    }
}

/// PMS half of a claimed user `AdaptiveReload` on a transcode: rebuild the encoder at the claim
/// offset so the fresh demux worker captures the adaptive contract ("quality: Auto picked —
/// retaining live HLS and refreshing its adaptive contract"). The same flight as a seek, owned by
/// the CLAIM: plan here ([`plan_rebase`] with [`RebaseFor::Claim`]), PMS on the claim worker
/// ([`run_rebase`]), install at the drain ([`install_rebase`], landing as [`ClaimTail::Adaptive`]).
///
/// [`RebaseFor::Claim`] is what lets the plan run inside the claim that is the flight: the
/// synchronous seek path refuses while any claim is in flight.
pub fn execute_adaptive_reload_claim(
    ps: &mut PlaybackSession,
    action: &ClaimedRouteAction,
    offset_secs: i64,
    pending_seek: i64,
    user_target: i64,
) -> RetranscodeClaimDispatch {
    let Ok(plan) = plan_rebase(ps, offset_secs, RebaseFor::Claim(action.serial())) else {
        return RetranscodeClaimDispatch::Sync(ClaimTail::Rejected(ADAPTIVE_REJECTED));
    };
    // Captured AFTER the plan, which mirrors a live HLS rung into the session: the snapshot is the
    // reducer's restore point and must describe the route the plan started from.
    let owner = begin_claim_flight(ps, action);
    if spawn_flight(
        owner,
        pending_seek,
        user_target,
        None,
        ClaimWork::Rebase(Box::new(plan)),
        RetranscodeFallback::RejectWith(ADAPTIVE_REJECTED),
    ) {
        RetranscodeClaimDispatch::Pending
    } else {
        RetranscodeClaimDispatch::Sync(ClaimTail::Rejected(ADAPTIVE_REJECTED))
    }
}

/// PMS half of a claimed `RecoverOriginal`, the viewer's own pick (`RecoveryCause::ManualOriginal`)
/// or the Auto watchdog's HLS-to-Original handoff (`RecoveryCause::Automatic`): plan on the frame
/// ([`plan_original_recovery`]), the PMS half on the claim worker ([`run_original_recovery`]), the
/// install at the drain ([`install_original_recovery`], landing as [`ClaimTail::Original`]) — the
/// same flight as an enhancement release.
///
/// A displaced pick merged under the claim (the merge table lets `RecoverOriginal` absorb a queued
/// `Retranscode`) is still owed its reload if Original is refused: [`ClaimFallback::owed`] picks
/// the owed fallback exactly as for a release, precomputed here and paid by the worker. An
/// automatic claim never carries a displaced pick, so a refusal is a plain rejection
/// ([`AUTO_ORIGINAL_REJECTED`]); the pump reopens the retained HLS for it.
pub fn execute_recover_original_claim(
    ps: &mut PlaybackSession,
    action: &ClaimedRouteAction,
    offset_secs: i64,
    cause: RecoveryCause,
    pending_seek: i64,
    user_target: i64,
) -> RetranscodeClaimDispatch {
    let dispatch = Dispatch {
        primary: ClaimPrimary::ReleaseToDirect,
        on_failure: ClaimFallback::owed(action.displaced_pick),
    };
    let rejected = match cause {
        RecoveryCause::Automatic => AUTO_ORIGINAL_REJECTED,
        RecoveryCause::ManualOriginal => MANUAL_ORIGINAL_REJECTED,
        RecoveryCause::EnhancementReleased => ENHANCEMENT_REJECTED,
    };
    plan_claim_flight(
        ps,
        action,
        FlightPlan { offset_secs, pending_seek, user_target },
        dispatch,
        OriginalAttempt { cause, rejected },
    )
}

/// C38: a `NativeAudioReload`/`AdaptiveReload` claim that took a displaced pick's marker (the
/// merge table let it replace the Retranscode that carried it). On a direct route the reload
/// feeds `desired_audio_idx`, which a displaced pick never wrote — store the picked track before
/// reloading. Every other case already rebuilds from `cur_audio`/`cur_sub_sid`.
pub fn honour_displaced_pick(ps: &mut PlaybackSession, displaced_pick: bool, arm: &str) {
    if !displaced_pick {
        return;
    }
    crate::player::log(&format!("enhancement: displaced pick on {arm}"));
    if is_transcoding(ps) {
        return;
    }
    let desired = crate::player::SHARED
        .desired_audio_idx
        .load(std::sync::atomic::Ordering::Relaxed);
    if let Some(a) = ps.cur_audio.clone() {
        if a.ordinal != desired {
            crate::player::stage_native_audio(ps, a.ordinal, &a.codec);
        }
    }
    debug_assert!(
        ps.cur_audio.as_ref().is_none_or(|a| a.ordinal
            == crate::player::SHARED.desired_audio_idx.load(std::sync::atomic::Ordering::Relaxed)),
        "the displaced pick is honoured before a direct reload",
    );
}

/// **Menu display truth.** While a user action is queued or in flight the rows show what the
/// preference is asking of the live route; once settled they show what the server APPLIED. A
/// claim-time rejection therefore shows what actually plays, and pressing the row again retries.
pub fn displayed_audio_enhancements(ps: &PlaybackSession) -> plx_plex::plex::AudioEnhancements {
    let in_flight = {
        let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        control.pending_user.is_some() || control.phase != ControlPhase::Stable
    };
    if in_flight {
        want_live(ps)
    } else {
        ps.cur_contract.audio
    }
}

/// **The Audio tab's one question, answered once.** `Hidden` only for the one case that stays
/// silent (no Plex Pass); every other reason the toggle cannot run right now comes back as
/// `Disabled(reason)` so the row stays visible with a plain-language note (owner direction,
/// 2026-09-29 — supersedes the earlier I1/I2 "absent, never greyed" reading of the enhancement
/// row itself; I1/I2's Plex-Pass gate is the one part of that reading that stands). Uses
/// [`enhancement_family`], not `live_family`, so an already-applied Burn reads as its own route
/// rather than as an unrelated `Other`-family re-encode.
pub fn menu_enhancement_availability(ps: &PlaybackSession) -> EnhancementAvailability {
    enhancement_availability(&facts(ps), enhancement_family(ps))
}

/// Are the rows offered at all — the toggle itself actionable? `true` only for
/// [`EnhancementAvailability::Offered`]; both `Hidden` and `Disabled` answer `false` here, since
/// callers that need to draw a `Disabled` row with its reason use [`menu_enhancement_availability`]
/// directly instead.
pub fn audio_enhancements_offered_live(ps: &PlaybackSession) -> bool {
    matches!(menu_enhancement_availability(ps), EnhancementAvailability::Offered(_))
}

/// **Test-only session builder for the Audio tab's enhancement rows (issue #266 PR 4).** Every
/// `PlaybackSession` field is private to this module by design (see `PlaybackSession::IDLE`'s own
/// doc), so `appkit::track_menu`'s tests — which live outside `route` and see only this module's
/// `pub` surface — cannot build one field-by-field the way this module's own tests do.
/// This is the one door: it drives every input `audio_enhancements_offered_live`/
/// `displayed_audio_enhancements` read (I1-I7), registers a throwaway server carrying the given
/// Plex Pass tristate, and returns the session plus that server's id so the caller can
/// `plx_plex::plex::reset_servers_for_test()` when done. Caller holds `plx_base::testlock::serial()`.
#[cfg(any(test, feature = "test-support"))]
pub struct EnhTestFixture {
    pub pass: plx_plex::plex::serverinfo::Subscription,
    /// `None` = still direct-playing (Direct family). `Some(true)` = a progressive-MKV remux
    /// (Remux family — a plain Original remux, or an already-applied enhancement). `Some(false)`
    /// = any other transcode shape (Other family — HLS, a fixed rung, a relay: I5 excludes all of
    /// them identically, so one shape stands for the group).
    pub remux: Option<bool>,
    /// `auto_original` present at all — `false` reproduces I5's forced-direct-play/fixed-rung/
    /// relay/non-Original-MDE exclusion, which is exactly "no candidate was ever computed".
    pub base_present: bool,
    /// The base route's own Dolby Vision declaration (I7).
    pub dv_declared: bool,
    /// The base route's own [`plx_data::metadata::Dovi::base_layer_unusable`] (Profile 5 / P7 with an
    /// enhancement layer) — a source the offer must refuse regardless of `dv_declared`, since a
    /// declaration only ever accompanies a USABLE base layer.
    pub dv_base_unusable: bool,
    /// `None` = server default audio, facts unknown (fails closed). `Some(capable)` = a known
    /// carried track with or without `canNormalizeLoudness`.
    pub carried_capable: Option<bool>,
    /// What is on screen (M7/I6): none, an external sidecar the client draws itself, or an
    /// embedded track the server would have to burn to keep — the same [`SubtitleEffect`]
    /// `facts(ps)` reads off `cur_sub_sid`/`cur_sub_sidecar`.
    pub subtitle_effect: SubtitleEffect,
    /// This playback's server already refused or ignored the params once.
    pub refused: bool,
    /// The server never answered (an unreachable decision — `classify_outcome`'s `Unverified`).
    /// Takes precedence over `applied.any()` alone, but `refused` wins if both are set.
    pub unverified: bool,
    /// The contract's own `audio` — what the live route has actually applied.
    pub applied: plx_plex::plex::AudioEnhancements,
    /// The applied route is a Burn (M7) — `cur_contract.remux` is `false` even though this is the
    /// enhancement's OWN route, exactly as [`live_is_own_burn`] reads it. Ignored unless `applied`
    /// carries a preference and `remux` is `Some(true)` (a Burn is still `ProgressiveMkv`).
    pub applied_burn: bool,
    /// Force `displayed_audio_enhancements`'s in-flight branch (a user edit queued, not yet
    /// settled), so it reads `want_live` instead of `cur_contract.audio`.
    pub in_flight: bool,
}

#[cfg(any(test, feature = "test-support"))]
impl Default for EnhTestFixture {
    fn default() -> Self {
        Self {
            pass: plx_plex::plex::serverinfo::Subscription::Yes,
            remux: Some(true),
            base_present: true,
            dv_declared: false,
            dv_base_unusable: false,
            carried_capable: Some(true),
            subtitle_effect: SubtitleEffect::None,
            refused: false,
            unverified: false,
            applied: plx_plex::plex::AudioEnhancements::NONE,
            applied_burn: false,
            in_flight: false,
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
fn test_original_candidate(subtitle_ordinal: Option<i32>) -> AutoOriginalCandidate {
    AutoOriginalCandidate {
        url: "https://example.invalid/source.mkv".into(),
        probe_part: "https://example.invalid/source.mkv".into(),
        direct: true,
        vcodec: "hevc".into(),
        fps: 23.976,
        dovi: plx_data::metadata::Dovi::NONE,
        dv_decision: plx_data::metadata::DvDecision::NONE,
        audio: Some(CarriedAudio {
            sid: 42,
            ordinal: 1,
            codec: "eac3".into(),
            channels: 6,
            can_normalize_loudness: false,
            immersive: true,
        }),
        audio_converted: false,
        subtitle_ordinal,
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn enhancement_test_session(route: EnhTestFixture) -> (PlaybackSession, ServerId) {
    let sid = plx_plex::plex::register_for_test("enh-menu-test", "127.0.0.1", 1, "token", "enh-menu-client");
    plx_plex::plex::serverinfo::store_for_test(sid, route.pass, "1.43.4");

    let mut ps = PlaybackSession::IDLE;
    ps.cur_sid = sid;
    ps.cur_sub_sid = if route.subtitle_effect == SubtitleEffect::None { 0 } else { 999 };
    ps.cur_sub_sidecar = route.subtitle_effect == SubtitleEffect::Sidecar;
    ps.cur_sub_client_drawable = ps.cur_sub_sidecar;
    ps.cur_enhancement = if route.refused {
        EnhancementOutcome::Refused
    } else if route.unverified {
        EnhancementOutcome::Unverified
    } else if route.applied.any() {
        EnhancementOutcome::Applied
    } else {
        EnhancementOutcome::Off
    };
    ps.cur_contract = plx_plex::plex::EncodeContract {
        remux: matches!(route.remux, Some(true)) && !(route.applied_burn && route.applied.any()),
        delivery: plx_plex::plex::TranscodeDelivery::ProgressiveMkv,
        no_video_copy: false,
        ceiling: None,
        audio: route.applied,
    };
    ps.tsession = if route.remux.is_some() { "enh-menu-test-session".to_owned() } else { String::new() };
    ps.cur_audio = route.carried_capable.map(|capable| CarriedAudio {
        sid: 501,
        ordinal: 1,
        codec: "ac3".into(),
        channels: 2,
        can_normalize_loudness: capable,
        immersive: false,
    });
    ps.auto_original = route.base_present.then(|| AutoOriginalCandidate {
        direct: route.remux.is_none(),
        dovi: if route.dv_base_unusable {
            plx_data::metadata::Dovi { present: true, profile: 5, bl_compat: 0, ..plx_data::metadata::Dovi::NONE }
        } else {
            plx_data::metadata::Dovi::NONE
        },
        dv_decision: if route.dv_declared {
            plx_data::metadata::DvDecision {
                capability: plx_platform::devcaps::dv::DvCapability::Supported,
                presentation: plx_data::metadata::DvPresentation::Declare(plx_data::metadata::DolbyHdrInfo {
                    profile_id: 8,
                    track_type: "single",
                    encryption_type: "clear",
                }),
            }
        } else {
            plx_data::metadata::DvDecision::NONE
        },
        audio: ps.cur_audio.clone(),
        ..test_original_candidate(None)
    });

    if route.in_flight {
        let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        control.pending_user = Some(UserRouteIntent::Retranscode);
        control.phase = ControlPhase::StagingUser(control.next_action);
    }

    (ps, sid)
}

// ---- selection commits: playback POLICY for the in-player track menu. The menu only reports
// what row was picked; whether that means a native stream switch, a server re-transcode, or a
// burn refresh is decided HERE, next to the codec sets and the transcode state it depends on. ----

/// Commit an audio-track pick: NATIVE switch (feed the chosen stream from the same direct-play
/// file — no transcode, keeps 4K HEVC) when the item direct-plays AND the target codec is
/// direct-playable; else a server re-transcode with that stream selected. `audio.ordinal` is the
/// CONTAINER audio ordinal (the menu converts its row via metadata::audio_ordinal); `audio` itself
/// is the frozen `CarriedAudio` snapshot (issue #266) the menu built via `CarriedAudio::from_stream`.
pub fn commit_audio_selection(ps: &mut PlaybackSession, audio: CarriedAudio) {
    if forced_direct_play(ps) && !audio_track_direct_plays(ps, &audio.codec, audio.channels) {
        ps.play_verdict = Some(PlayVerdict::Forced(ForcedFailure::AudioNeedsConversion));
        return;
    }
    if original_recovery_pending() {
        if let Some(pending) = PLAYER_CONTROL
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending_original
            .as_mut()
        {
            pending.deferred_audio = Some(audio);
        }
        crate::player::log("audio: deferred until pending Original handoff commits or rolls back");
        return;
    }
    // Audio always changes the decoder/server route contract, even when the eventual route stays
    // direct. Fence the old worker before publishing the selected stream or invalidating its
    // Original candidate; the queued reload below crosses its own action boundary afterwards.
    let _edit = begin_user_contract_boundary();
    // A standing Burn is wire-shaped `Other`, but it is an Original-family route: the pick must
    // retarget the candidate the release returns to, exactly as on a remux.
    let family = enhancement_family(ps);
    let direct_plays = audio_track_direct_plays(ps, &audio.codec, audio.channels);
    match family {
        // The recovery declaration captures one exact source/audio pairing. Once the user changes
        // that pairing while HLS is live, do not later resurrect the old track behind their back.
        // A new playback (or selecting Auto again from Original) can establish a fresh candidate.
        RouteFamily::Other => {
            if matches!(
                cur_delivery(ps),
                plx_plex::plex::TranscodeDelivery::FixedHls { .. }
            ) {
                ps.auto_original = None;
            }
        }
        // Issue #266: on the Original family the candidate is also the way back from an enhanced
        // remux, so it follows the pick instead (see `AutoOriginalCandidate::retarget_audio`).
        RouteFamily::Direct | RouteFamily::Remux => {
            if let Some(candidate) = ps.auto_original.as_mut() {
                if !candidate.retarget_audio(&audio, direct_plays) {
                    ps.auto_original = None;
                }
            }
        }
    }
    let stream_id = audio.sid;
    let native = family == RouteFamily::Direct && direct_plays;
    let (ordinal, codec) = (audio.ordinal, audio.codec.clone());
    { let s = &mut *ps; s.cur_audio = Some(audio) };
    if native {
        // record the pick: the timeline then reports the stream that actually plays, and a
        // later transcode event (subtitle burn refresh / transcode seek) keeps this track.
        // persist the USER's pick server-side (official-client behavior): /status/sessions'
        // selected-stream display keys on the part selection, not the timeline report. Only
        // user picks persist — the start-of-play auto-pick (eng preference) reports only. Reached
        // from `app/playback.rs::commit_track` inside the run-loop's `FrameScope`, so this PUT runs
        // on the serial selection worker instead of blocking the frame thread.
        queue_put_selection(cur_sid(ps), cur_part_id(ps), cur_audio_sid(ps), cur_sub_sid(ps));
    }
    // The new track may change what the enhancement offer says (capability, candidate). If the
    // route must change for that, the reconcile's Retranscode REPLACES this pick's own reload and
    // is marked as owing it, so a refused enhancement still switches the track.
    if reconcile_enhancement(ps, true) {
        return;
    }
    if native {
        crate::player::request_audio_track(ps, ordinal, &codec);
    } else {
        crate::player::request_audio_switch(ps, stream_id);
    }
}

/// Apply commands which were attached to one exact Original trial, after either the candidate or
/// its rollback Engine has really started. Consuming the value makes cross-trial leakage
/// impossible; dropping it on terminal start failure is the explicit cancellation edge.
pub fn apply_deferred_original_effects(ps: &mut PlaybackSession, mut effects: DeferredOriginalEffects) {
    if let Some(q) = effects.quality.take() {
        apply_quality_choice(ps, q);
    }
    if let Some(audio) = effects.audio.take() {
        commit_audio_selection(ps, audio);
    }
    if effects.reconcile {
        reconcile_enhancement(ps, false);
    }
}

/// Commit a subtitle pick (`sub_idx` -1 = Off): gate the client-side renderer (direct-play path)
/// and select the burn stream for any transcode of the item — refreshing a live transcode so the
/// server re-burns (or drops) it. `client_renderable` = the client can draw this pick itself (an
/// embedded ordinal or a sidecar), which is what lets the Original candidate carry it.
///
/// Deliberately NOT deferred behind a pending Original trial: the subtitle is client-rendered on
/// the candidate, and Off in particular must take effect the moment it is picked.
pub fn commit_subtitle_selection(
    ps: &mut PlaybackSession,
    sub_idx: i32,
    stream_id: i64,
    client_renderable: bool,
) {
    let transcoding = is_transcoding(ps);
    // A live plain remux whose new pick is Off or a subtitle the app draws over it keeps its
    // stream: the server never carried the subtitle, so nothing about the encode changes. An
    // embedded pick (a burn), or leaving a burned one, changes the route's shape and rebuilds.
    // Decided before the selection is written, from the pick itself.
    let in_place = transcoding
        && live_family(ps) == RouteFamily::Remux
        && (stream_id == 0
            || side_subs_allowed_for(ps, stream_id, client_renderable, sub_idx < 0));
    // Burned subtitles are part of the server/decoder contract, so revoke old worker evidence
    // before changing them. A direct-play subtitle is client-rendered and needs no reload; fencing
    // there would kill the valid Original watchdog while leaving the physical route untouched.
    let _edit = (transcoding && !in_place).then(begin_user_contract_boundary);
    // `enhancement_family`, read before `set_subtitle` below: a standing Burn is an Original-family
    // route whose candidate follows the pick, though it is wire-shaped `Other`.
    match enhancement_family(ps) {
        // As with audio, a non-Off subtitle may require server burn-in and is not interchangeable
        // with the direct declaration captured at playback start. Off is always safe to carry back.
        RouteFamily::Other => {
            if matches!(
                cur_delivery(ps),
                plx_plex::plex::TranscodeDelivery::FixedHls { .. }
            ) {
                let s = &mut *ps;
                if stream_id == 0 {
                    if let Some(candidate) = s.auto_original.as_mut() {
                        candidate.subtitle_ordinal = None;
                    }
                } else {
                    s.auto_original = None;
                }
            }
        }
        // Issue #266: the candidate follows the pick (see `retarget_subtitle`), so releasing an
        // enhanced remux shows the subtitle the viewer just chose, client-rendered.
        RouteFamily::Direct | RouteFamily::Remux => {
            let pick = (stream_id != 0).then_some(sub_idx);
            if let Some(candidate) = ps.auto_original.as_mut() {
                if !candidate.retarget_subtitle(pick, client_renderable) {
                    ps.auto_original = None;
                }
            }
        }
    }
    // A timing offset was tuned against the track that was showing; a DIFFERENT pick (another
    // track, a sidecar, or Off) starts at zero. Re-committing the same track — a subtitle OK
    // always republishes — keeps what the viewer found.
    if stream_id != ps.cur_sub_sid {
        crate::player::set_subtitle_offset(0);
    }
    crate::player::request_subtitle(sub_idx);
    set_subtitle(ps, stream_id);
    // `sub_idx` is `metadata::sub_render_ordinal`'s own convention: negative for an external
    // sidecar (and for Off, where `stream_id == 0` disambiguates), non-negative for an embedded
    // track. Issue #266 I6 reads this back through `facts` to tell a sidecar (unaffected by the
    // audio enhancement) apart from an embedded pick (needs a forced burn, M7).
    ps.cur_sub_sidecar = stream_id != 0 && sub_idx < 0;
    ps.cur_sub_client_drawable = stream_id != 0 && client_renderable;
    if !transcoding || in_place {
        // This is an immediate client-rendered change: unlike a burn/audio rebuild it is already
        // part of the applied stream contract. Publish projection + reporter tracks as one reducer
        // event so a later rejected action cannot restore the pre-subtitle snapshot.
        commit_in_place_route_projection(ps, false);
        // persist the pick server-side (and subs Off PUTs subtitleStreamID=0, clearing a
        // stale server-side selection that would otherwise burn on the next transcode). Reached
        // from `app/playback.rs::commit_track` inside the run-loop's `FrameScope`, so this PUT runs
        // on the serial selection worker instead of blocking the frame thread.
        queue_put_selection(cur_sid(ps), cur_part_id(ps), cur_audio_sid(ps), cur_sub_sid(ps));
    }
    // A subtitle turns the enhancement's offer off (I6) and Off may turn it back on. On direct
    // play a subtitle never reloads, so there is no pick of its own to displace.
    if reconcile_enhancement(ps, transcoding && !in_place) {
        return;
    }
    if transcoding && !in_place {
        crate::player::request_transcode_refresh(ps); // retranscode PUTs the selection itself
    }
}

/// Atomically publish the main-thread session fields a newly spawned timeline reporter may read.
/// Called at the reporter spawn site, before ownership crosses to its worker. The active encoder
/// remains in `PlayerControl`, so a later in-place ABR commit changes the wire session and this
/// projection under one lock without touching main-thread-only `Session`.
pub fn begin_timeline_reporting(ps: &PlaybackSession) -> Option<TimelineLease> {
    // A trailer never writes watch state, whatever became of its preview flag.
    if preview_request(ps) {
        return None;
    }
    let projection = TimelineProjection {
        sid: cur_sid(ps),
        rating_key: cur_rk(ps),
        logical_session: sess(ps),
        play_queue_id: pq_id(ps),
        play_queue_item_id: pq_item_id(ps),
        audio_stream_id: cur_audio_sid(ps),
        subtitle_stream_id: cur_sub_sid(ps),
    };
    if projection.rating_key.is_empty() || !projection.sid.is_set() {
        return None;
    }
    let required_stop = TIMELINE_STOP_FENCE.announced();
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    control.timeline = Some(projection);
    Some(TimelineLease {
        engine_epoch: control.engine_epoch,
        required_stop,
    })
}

/// Linearize one worker sample against route replacement and engine teardown. No network work is
/// done while the mutex is held. `None` permanently invalidates this reporter after its Engine is
/// retired; transient Applying/Resolving phases skip a hybrid report without borrowing Session.
fn timeline_snapshot(
    lease: &TimelineLease,
    state: plx_plex::plex::TimelineState,
    time_ms: i64,
    duration_ms: i64,
) -> Option<TimelineSnapshot> {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if control.engine_epoch != lease.engine_epoch {
        return None;
    }
    if !matches!(
        control.phase,
        ControlPhase::Stable | ControlPhase::OriginalTrial(OriginalTrialPhase::AwaitingFrame(_))
    ) {
        return None;
    }
    let projection = control.timeline.as_ref()?;
    let session = if control.active.id.is_empty() {
        projection.logical_session.clone()
    } else {
        control.active.id.clone()
    };
    Some(TimelineSnapshot {
        sid: projection.sid,
        rating_key: projection.rating_key.clone(),
        state,
        time_ms,
        duration_ms,
        session,
        play_queue_id: projection.play_queue_id.clone(),
        play_queue_item_id: projection.play_queue_item_id.clone(),
        audio_stream_id: projection.audio_stream_id,
        subtitle_stream_id: projection.subtitle_stream_id,
    })
}

/// POST one periodic timeline update for an exact [`TimelineLease`]. After waiting for the stop
/// fence captured by that lease, this serializes through `TIMELINE_EFFECT` and snapshots the
/// server, item, PlayQueue and selected tracks together from [`PlayerControl`]; a stale Engine lease
/// sends nothing. Final `Stopped` is emitted separately by [`ScrobbleWork`] from its owned teardown
/// snapshot.
pub fn report_timeline(
    lease: &TimelineLease,
    state: plx_plex::plex::TimelineState,
    t_ms: i64,
    d_ms: i64,
) -> bool {
    // This lease belongs to a replacement Engine only after every stop synchronously announced
    // before its publication has joined the old reporter and attempted old `stopped`. Old leases
    // captured an earlier generation and never wait on the stop worker which is joining them.
    TIMELINE_STOP_FENCE.wait(lease.required_stop);
    let _effect = TIMELINE_EFFECT.lock().unwrap_or_else(|e| e.into_inner());
    let Some(report) = timeline_snapshot(lease, state, t_ms, d_ms) else {
        return false;
    };
    let c = match plx_plex::plex::client_for(report.sid) {
        Some(c) => c,
        None => return true,
    };
    let ok = c.timeline(&plx_plex::plex::TimelineReport {
        rating_key: &report.rating_key,
        state: report.state,
        time_ms: report.time_ms,
        duration_ms: report.duration_ms,
        session: &report.session,
        play_queue_id: &report.play_queue_id,
        play_queue_item_id: &report.play_queue_item_id,
        audio_stream_id: report.audio_stream_id,
        subtitle_stream_id: report.subtitle_stream_id,
    });
    // FAILURES ONLY. The reporter thread logs `timeline <state> t=…s/…s` for every tick whichever
    // way the POST went (`player::threads`), so a report the server never took looks exactly like
    // one it did — for the whole length of a film, ten seconds at a time. The success half is
    // already on that line and this runs at 0.1 Hz, so only the silence needs a line of its own.
    if !ok {
        plx_base::eventlog::log(&format!(
            "timeline post failed rk={} state={} t={}s",
            report.rating_key,
            report.state.as_str(),
            report.time_ms / 1000,
        ));
    }
    true
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
#[path = "decision_test_support.rs"]
mod test_support;

#[cfg(all(test, feature = "hostsim"))]
#[path = "decision_flight_rig.rs"]
pub(crate) mod flight_rig;

#[cfg(all(test, feature = "hostsim"))]
#[path = "decision_trial_flight_tests.rs"]
mod trial_flight_tests;

#[cfg(test)]
#[path = "decision_next_episode_tests.rs"]
mod next_episode_tests;

#[cfg(test)]
#[path = "decision_skip_interval_tests.rs"]
mod skip_interval_tests;

#[cfg(test)]
#[path = "decision_deck_press_tests.rs"]
mod deck_press_tests;

#[cfg(test)]
#[path = "decision_resolve_route_tests.rs"]
mod resolve_route_tests;

#[cfg(test)]
#[path = "decision_plan_tests.rs"]
mod plan_tests;

#[cfg(test)]
#[path = "decision_quality_recovery_tests.rs"]
mod quality_recovery_tests;

#[cfg(test)]
#[path = "decision_timeline_tests.rs"]
mod timeline_tests;

#[cfg(test)]
#[path = "decision_direct_play_mode_tests.rs"]
mod direct_play_mode_tests;

#[cfg(test)]
#[path = "decision_subtitle_style_tests.rs"]
mod subtitle_style_tests;

#[cfg(test)]
#[path = "carried_audio_tests.rs"]
mod carried_audio_tests;

#[cfg(test)]
#[path = "plan_audio_enhancement_tests.rs"]
mod plan_audio_enhancement_tests;

#[cfg(test)]
#[path = "decision_audio_enhancement_tests.rs"]
mod audio_enhancement_tests;
