//! The IMPURE half of the route split (spec §9, §15.2): session state, the synchronized
//! [`PlayerControl`], PMS/native I/O, and the encoder/scrobble/timeline machinery — everything
//! [`super::plan`] is not. `PlaybackSession` is the main-thread projection used to build URLs/payloads;
//! [`PLAYER_CONTROL`] is the synchronized authority for route ownership and route-changing
//! intents. The player engine reads the URL/session through the accessors here; ui::player_hud
//! reads the HUD strings through title_cptr()/ctxline_cptr(). This file is exempt from the
//! `wall` gate that `plan.rs` must pass — a network/adapter effect is allowed to read wall time —
//! but as of this split it still contains none: the one wall-clock field this module owned
//! (`PlaybackSession::auto_last_switch`) is now a frame-tick millisecond stamp, not an `Instant`.

use crate::plex::ServerId;
use crate::pms::PmsMovie;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::Mutex;
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
pub(crate) struct PlaybackSession {
    /// This playback was refused by the cached device sandbox preflight. No Engine exists.
    /// Cleared with the playback verdict on exit/reset, never a process-global error latch.
    pub(crate) jail_load_blocked: bool,
    /// Read-only publication of Player.repair for the HUD. Never authorizes a resource effect.
    pub(crate) repair_status: crate::webos::jail_repair::State,
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
    play_verdict: Option<String>,
    /// The resolve itself could not produce a plan (no client, worker panic, or worker spawn
    /// refusal).  Unlike `play_verdict`, this is OUR failure rather than a PMS sentence; it makes
    /// an empty plan terminal and retryable instead of falling back to an idle black frame.
    resolve_failed: bool,
    /// this playback's transcode flavor: true = container-only remux, false = re-encode. A seek or
    /// retranscode rebuilds the identical start.mkv query from (`cur_rk`, `sess`, the `cur_*_sid`
    /// pair, this flag) via plex::TranscodeSpec — replaces the old stored offset-free TBASE query
    /// string.
    cur_remux: bool,
    /// The coupled transcode profile/query/endpoint/demux contract. Stored with the flavor so a
    /// seek or track change cannot silently turn an HLS session back into progressive MKV.
    cur_delivery: crate::plex::TranscodeDelivery,
    /// This playback asked the server for a re-encode it may NOT satisfy with a stream copy —
    /// [`crate::plex::TranscodeSpec::no_video_copy`]. Stored for the same reason `cur_remux` is:
    /// a seek and an audio-track switch rebuild the start.mkv query from scratch, and a rebuild
    /// that dropped this would hand the server back the permission mid-playback, so the film
    /// would carry on in the wrong colours from the first seek.
    cur_no_video_copy: bool,
    /// The fixed QUALITY ceiling this playback was resolved under (`None` = Original, or Auto's
    /// dynamically owned policy once that path is ready), stored
    /// for exactly the reason `cur_remux` and `cur_no_video_copy` are: a seek
    /// ([`transcode_seek`]) and an audio switch ([`retranscode`]) rebuild the start.mkv query from
    /// scratch, and a rebuild that read the LIVE selection instead would change the encode's
    /// resolution mid-film while the Load payload built for the old one stayed configured.
    ///
    /// **[`set_quality`] is the ONE writer that may move it mid-film**, and that is the whole
    /// distinction: an explicit pick is new information about what the link can carry, while a
    /// seek is not, so a seek rebuilds from what is stored here and a pick replaces it (and asks
    /// the pump for a fresh transcode when the answer actually changed). Nothing measures a link
    /// or moves a rung on its own — the adaptive switch is not here.
    cur_ceiling: Option<crate::plex::Ceiling>,
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
    /// current audio/subtitle selection carried by any TRANSCODE of the current item
    /// (0 = server default / none). The subtitle is BURNED into the video (our client
    /// profile advertises no soft-sub support, so Plex's decision is burn); direct-play
    /// subtitles are separate (client-rendered from the demuxer, player::request_subtitle).
    cur_audio_sid: i64,
    cur_sub_sid: i64,
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
    stream_dovi: crate::metadata::Dovi,
    /// Capability and presentation frozen when `stream_dovi` entered this physical route. An
    /// audio switch, reload, Original recovery and rollback all copy this beside the raw record.
    stream_dv_decision: crate::metadata::DvDecision,
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
    queue: Vec<crate::plex::QueueRow>,
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
}

impl PlaybackSession {
    /// Nothing playing: what the module holds before the first play, and the value the static is
    /// born as. Every String empty, every id 0 or `UNSET`, both HUD buffers NUL.
    pub(crate) const IDLE: PlaybackSession = PlaybackSession {
        jail_load_blocked: false,
        repair_status: crate::webos::jail_repair::State::Idle,
        request: None,
        requested_resume_ns: 0,
        url: String::new(),
        tsession: String::new(),
        play_verdict: None,
        resolve_failed: false,
        cur_remux: false,
        cur_delivery: crate::plex::TranscodeDelivery::ProgressiveMkv,
        cur_no_video_copy: false,
        cur_ceiling: None,
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
        cur_audio_sid: 0,
        cur_sub_sid: 0,
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
        stream_dovi: crate::metadata::Dovi::NONE,
        stream_dv_decision: crate::metadata::DvDecision::NONE,
        stream_immersive: false,
        title: [0; 128],
        ctxline: [0; 96],
        up_next: None,
        queue: Vec::new(),
        now_ms: 0,
        preview: false,
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
    pub(crate) fn publication(&self) -> PlaybackSession {
        let PlaybackSession {
            jail_load_blocked,
            repair_status,
            request,
            requested_resume_ns,
            url,
            tsession,
            play_verdict,
            resolve_failed,
            cur_remux,
            cur_delivery,
            cur_no_video_copy,
            cur_ceiling,
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
            cur_audio_sid,
            cur_sub_sid,
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
        } = self;
        PlaybackSession {
            jail_load_blocked: *jail_load_blocked,
            repair_status: *repair_status,
            request: request.clone(),
            requested_resume_ns: *requested_resume_ns,
            url: url.clone(),
            tsession: tsession.clone(),
            play_verdict: play_verdict.clone(),
            resolve_failed: *resolve_failed,
            cur_remux: *cur_remux,
            cur_delivery: *cur_delivery,
            cur_no_video_copy: *cur_no_video_copy,
            cur_ceiling: *cur_ceiling,
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
            cur_audio_sid: *cur_audio_sid,
            cur_sub_sid: *cur_sub_sid,
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
    pub(crate) fn set_now(&mut self, now_ms: u32) {
        self.now_ms = now_ms;
    }
}

/// **A borrowable idle session**, for a test that builds a `Cx` and has no playback in it.
///
/// `AppViews::session` is a borrow, and `&PlaybackSession::IDLE` is a temporary that dies at the
/// end of the statement — this is the one long-lived idle value those fixtures share.
#[cfg(test)]
pub(crate) fn idle_session_for_test() -> &'static PlaybackSession {
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
pub(crate) fn url(ps: &PlaybackSession) -> String {
    ps.url.clone()
}
/// Is there a stream URL at all? The in-place twin of [`url`], for the callers that only want the
/// emptiness — [`is_transcoding`]'s idiom, and for the same reason: a universal-transcode
/// `start.mkv` URL is several hundred bytes, and the player route is exempt from the idle present
/// gate, so a `!url().is_empty()` in a draw is a heap allocation and a memcpy at ~60/s.
pub(crate) fn has_url(ps: &PlaybackSession) -> bool {
    !ps.url.is_empty()
}
/// Whole-file transport requirement captured by the playback resolve. Diagnostics uses it for
/// manual Original, where no adaptive controller exists to publish `dg_abr_kbps`.
pub(crate) fn transport_kbps(ps: &PlaybackSession) -> i64 {
    ps.cur_transport_kbps
}
pub(crate) fn set_url(ps: &mut PlaybackSession, s: &str) {
    ps.url = s.to_owned()
}
pub(crate) fn clear_url(ps: &mut PlaybackSession) {
    ps.url.clear()
}
pub(crate) fn transcode_session(ps: &PlaybackSession) -> String {
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
pub(crate) enum UserRouteIntent {
    Retranscode,
    NativeAudioReload,
    AdaptiveReload,
    RecoverOriginal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AutomaticRouteIntent {
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
pub(crate) enum AutomaticIntentResult {
    Accepted,
    Busy,
    Stale,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RouteIntent {
    User(UserRouteIntent),
    Automatic(AutomaticRouteIntent),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClaimedRouteAction {
    serial: u64,
    pub(crate) ticket: WorkerTicket,
    pub(crate) intent: RouteIntent,
}

/// Identity of one prepared route transaction.
///
/// PMS/Session preparation and native construction are two different external effects. A
/// transaction may mint multiple never-reused [`RouteStartAttempt`]s; the attempt, not this
/// transaction, prevents a late `Load` result from settling a retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RouteStartTransaction {
    serial: u64,
}

/// Synchronous result of the native half of a prepared route transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RouteStartResult {
    Started,
    NoRoute,
    StartFailed,
}

/// Native half of an Original handoff.  A successful `sf_load` only proves that the payload was
/// accepted; the old HLS route cannot be retired until the new source produces a decoded frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OriginalTrialPhase {
    Prepared(u64),
    Starting(u64, u64),
    AwaitingFrame(u64),
    Failed(u64),
}

/// The only three ways an external route effect may return to the synchronized owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RouteApplyResult {
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
struct AppliedRouteProjection {
    url: String,
    tsession: String,
    remux: bool,
    delivery: crate::plex::TranscodeDelivery,
    no_video_copy: bool,
    ceiling: Option<crate::plex::Ceiling>,
    auto_original_watched: bool,
    auto_original: Option<AutoOriginalCandidate>,
    audio_sid: i64,
    subtitle_sid: i64,
    stream_vcodec: String,
    stream_acodec: String,
    stream_fps: f64,
    stream_dovi: crate::metadata::Dovi,
    stream_dv_decision: crate::metadata::DvDecision,
    stream_immersive: bool,
}

fn route_projection(ps: &PlaybackSession) -> AppliedRouteProjection {
    let s = &*ps;
    AppliedRouteProjection {
        url: s.url.clone(),
        tsession: s.tsession.clone(),
        remux: s.cur_remux,
        delivery: s.cur_delivery,
        no_video_copy: s.cur_no_video_copy,
        ceiling: s.cur_ceiling,
        auto_original_watched: s.cur_auto_original_watched,
        auto_original: s.auto_original.clone(),
        audio_sid: s.cur_audio_sid,
        subtitle_sid: s.cur_sub_sid,
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
        s.cur_remux = projection.remux;
        s.cur_delivery = projection.delivery;
        s.cur_no_video_copy = projection.no_video_copy;
        s.cur_ceiling = projection.ceiling;
        s.cur_auto_original_watched = projection.auto_original_watched;
        s.auto_original = projection.auto_original.clone();
        s.cur_audio_sid = projection.audio_sid;
        s.cur_sub_sid = projection.subtitle_sid;
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
    let audio_stream_id = projection.audio_sid;
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
pub(crate) struct TimelineLease {
    engine_epoch: u64,
    /// Every stop announced before this Engine was published must finish its final old-playback
    /// timeline effect before this reporter may send the replacement playback's first update.
    required_stop: u64,
}

#[derive(Clone)]
struct TimelineSnapshot {
    sid: ServerId,
    rating_key: String,
    state: crate::plex::TimelineState,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResolveFallback {
    Idle,
    Stable,
    Failed(u64),
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
    pending_auto: Option<AutomaticRouteIntent>,
    /// Latest requested playhead which has not yet crossed a real media discontinuity.
    pending_seek_ns: Option<i64>,
    phase: ControlPhase,
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
    pending_auto: None,
    pending_seek_ns: None,
    phase: ControlPhase::Stable,
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
        (_, RecoverOriginal) => RecoverOriginal,
        (Some(RecoverOriginal), Retranscode) if preserve_original_recovery => RecoverOriginal,
        (Some(RecoverOriginal), newer) => newer,
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
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    let fallback = match control.phase {
        // A newer resolve may supersede an older worker, but it inherits the first resolve's
        // fallback. Re-snapshotting `Resolving` would lose the route hidden underneath it.
        ControlPhase::Resolving => None,
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
    #[cfg(test)]
    if let Some(start) = start {
        if let Some(attempt) = claim_route_start_attempt(start) {
            let _ = settle_route_start(ps, attempt, RouteStartResult::Started);
        }
    }
    #[cfg(not(test))]
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
    };
}

/// Settle the deterministic main-thread half of a resolve whose worker could not be spawned.
/// `request_play` deliberately leaves the outgoing URL installed while resolving; retain that
/// still-playable route instead of unconditionally landing the controller in `Idle`.
fn settle_failed_resolve_spawn(ps: &mut PlaybackSession) {
    cancel_playback_request(ps, has_url(ps));
}

/// Capture the complete ownership generation for a newly spawned media worker.
pub(crate) fn worker_ticket() -> WorkerTicket {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    worker_ticket_of(&control)
}

/// Publish an automatic route request iff all evidence still belongs to the current engine,
/// media position, user contract and semantic route. A refusal is ordinary supersession: the
/// worker keeps/abandons its local transaction as appropriate and no playback error is raised.
pub(crate) fn publish_automatic_route_intent(
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
pub(crate) fn request_user_route_intent(ps: &PlaybackSession, intent: UserRouteIntent) {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    // Setters already crossed an early boundary before publishing their projection. Queueing the
    // resulting route action is a second, independently reachable boundary: callers such as the
    // automatic-recovery UI can request an action directly, and both paths must fence old tickets.
    advance_user_contract_locked(&mut control);
    // Subtitle Off is the one Retranscode which does not invalidate an in-flight Original
    // recovery. The candidate is updated to carry `None`, and a failed Original open necessarily
    // rebases the restored HLS route through `transcode_seek`, which reads the current subtitle id.
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
pub(crate) fn note_user_seek_intent(position_ns: i64) {
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
pub(crate) fn commit_user_seek() -> bool {
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
pub(crate) fn reject_user_seek() {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    control.pending_seek_ns = None;
}

pub(crate) fn cancel_user_route_intent(intent: UserRouteIntent) {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if control.pending_user == Some(intent) {
        control.pending_user = None;
    }
}

/// Reserve one main-thread action. The mutex is released before PMS or native I/O; `serial`
/// prevents a completion from settling any later action by mistake.
pub(crate) fn claim_route_action() -> Option<ClaimedRouteAction> {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if control.phase != ControlPhase::Stable {
        return None;
    }
    let intent = if let Some(user) = control.pending_user.take() {
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
    Some(ClaimedRouteAction {
        serial,
        ticket,
        intent,
    })
}

/// Settle the PMS/projection half of a claimed action which did not enter the explicit Original
/// trial phase. A prepared candidate advances the applied contract but remains non-publishable in
/// `Prepared`; claiming a physical `Load` moves it to `Starting`, and only [`settle_route_start`]
/// may expose it as `Stable`. Refusal/cancellation restores the previous complete projection while
/// the phase still blocks workers, then publishes Stable.
pub(crate) fn finish_route_action(ps: &mut PlaybackSession, action: &ClaimedRouteAction, result: RouteApplyResult) {
    // The effect runs on the main thread and has finished mutating Session before this call. Take
    // its complete value now; workers never touch Session, and the mutex below decides whether
    // this particular action is still allowed to publish it.
    let candidate_projection = (result == RouteApplyResult::Prepared).then(|| route_projection(ps));
    let restore = {
        let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        if control.phase != ControlPhase::Applying(action.serial) {
            return;
        }
        if result == RouteApplyResult::Prepared {
            if matches!(action.intent, RouteIntent::User(_)) {
                control.applied_revision = control.desired_revision;
                control.applied_quality = control.desired_quality;
            }
            // Automatic actions retain the applied contract revision carried by their ticket.
            // Route identity has its own epoch; rebinding an automatic result to a later rejected
            // user desire would immediately revoke the worker which the automatic action created.
            control.applied_projection = candidate_projection;
            control.phase = ControlPhase::Prepared(action.serial);
            return;
        }
        control.applied_projection.clone()
    };
    if let Some(applied) = restore {
        install_route_projection(ps, &applied);
    }
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if control.phase == ControlPhase::Applying(action.serial) {
        control.phase = ControlPhase::Stable;
    }
}

/// Return the pending route-start transaction while the reducer is between preparation and a
/// `Load` result. The exact physical attempt is minted only by [`claim_route_start_attempt`]; a dev
/// fixture which prepares its route inside `start_bufferfeed` is covered too.
pub(crate) fn pending_route_start() -> Option<RouteStartTransaction> {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    match control.phase {
        ControlPhase::Preparing(serial)
        | ControlPhase::Prepared(serial)
        | ControlPhase::Starting(serial, _) => Some(RouteStartTransaction { serial }),
        ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial))
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
pub(crate) fn begin_route_start() -> Option<RouteStartTransaction> {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    match control.phase {
        ControlPhase::Preparing(serial)
        | ControlPhase::Prepared(serial)
        | ControlPhase::Starting(serial, _) => Some(RouteStartTransaction { serial }),
        ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial))
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
pub(crate) fn prepare_route_start(ticket: RouteStartTransaction) -> bool {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    match control.phase {
        ControlPhase::Preparing(serial) if serial == ticket.serial => {
            control.phase = ControlPhase::Prepared(serial);
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
pub(crate) fn claim_route_start_attempt(
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
pub(crate) fn reject_route_start_preparation(ticket: RouteStartTransaction) -> bool {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    control.phase = match control.phase {
        ControlPhase::Preparing(serial) if serial == ticket.serial => ControlPhase::Stable,
        ControlPhase::Prepared(serial) if serial == ticket.serial => ControlPhase::Failed(serial),
        ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial))
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
pub(crate) fn abort_route_start(ticket: RouteStartTransaction, result: RouteStartResult) -> bool {
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
        ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial))
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
pub(crate) fn settle_route_start(ps: &mut PlaybackSession, ticket: RouteStartAttempt, result: RouteStartResult) -> bool {
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
pub(crate) enum RouteStartStatus {
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
pub(crate) enum LiveEngineStartRelation {
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
pub(crate) fn classify_live_engine_start(existing: RouteStartAttempt) -> LiveEngineStartRelation {
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
        ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(serial))
        | ControlPhase::OriginalTrial(OriginalTrialPhase::Starting(serial, _)) => {
            LiveEngineStartRelation::Conflict(RouteStartTransaction { serial })
        }
        _ => LiveEngineStartRelation::NoPendingRoute,
    }
}

/// Observe one exact physical start without consuming another subsystem's result. There can only
/// be one live start owner; retaining the last completion is sufficient for the foreground
/// reducer to bridge the media-thread return into its next main-loop tick.
pub(crate) fn route_start_status(ticket: RouteStartAttempt) -> RouteStartStatus {
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
pub(crate) fn publish_route_start_result(ticket: RouteStartAttempt, result: RouteStartResult) {
    PLAYER_CONTROL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .start_results
        .push((ticket, result));
}

/// Main-thread publication point for every completed native Load call. Late/stale results are
/// intentionally drained too; [`settle_route_start`] rejects their exact serial.
pub(crate) fn drain_route_start_results(ps: &mut PlaybackSession) {
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
pub(crate) fn fail_current_engine() {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    let serial = match control.phase {
        ControlPhase::Preparing(serial)
        | ControlPhase::Prepared(serial)
        | ControlPhase::Starting(serial, _) => serial,
        ControlPhase::Stable | ControlPhase::Completing(_) => {
            control.next_action = next_generation(control.next_action);
            control.next_action
        }
        ControlPhase::StagingUser(serial) => serial,
        ControlPhase::OriginalTrial(trial) => match trial {
            OriginalTrialPhase::Prepared(serial)
            | OriginalTrialPhase::Starting(serial, _)
            | OriginalTrialPhase::AwaitingFrame(serial)
            | OriginalTrialPhase::Failed(serial) => serial,
        },
        ControlPhase::Failed(_) | ControlPhase::Idle | ControlPhase::Stopping => return,
        ControlPhase::Resolving | ControlPhase::Applying(_) => return,
    };
    control.engine_epoch = next_generation(control.engine_epoch);
    control.media_epoch = next_generation(control.media_epoch);
    control.pending_auto = None;
    control.start_deferred = None;
    control.timeline = None;
    control.phase = ControlPhase::Failed(serial);
}

/// Invalidate the workers belonging to the Engine being torn down. Pending explicit contracts are
/// retained; a reload can therefore never consume a quality/track request merely by resetting the
/// atomics that used to carry it.
pub(crate) fn begin_engine_teardown(for_reload: bool) {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    control.engine_epoch = next_generation(control.engine_epoch);
    control.media_epoch = next_generation(control.media_epoch);
    control.pending_auto = None;
    if !for_reload {
        control.desired_revision = next_generation(control.desired_revision);
        control.pending_user = None;
        control.pending_seek_ns = None;
        control.phase = ControlPhase::Stopping;
        control.timeline = None;
    } else {
        control.phase = match control.phase {
            ControlPhase::Starting(serial, _) => {
                // The physical attempt being torn down can still publish from its media thread.
                // Return the transaction to Prepared so a retry mints a fresh attempt; the old
                // result no longer matches even when both attempts open the same candidate URL.
                ControlPhase::Prepared(serial)
            }
            ControlPhase::OriginalTrial(OriginalTrialPhase::Starting(serial, _)) => {
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
}

/// The reducer's phase, for a log line. A refused start owner names the phase it was refused
/// from, because "refused" alone cannot be diagnosed from a device log.
pub(crate) fn control_phase_label() -> String {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    format!("{:?}", control.phase)
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
pub(crate) fn finish_engine_teardown() {
    let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if control.phase == ControlPhase::Stopping {
        control.phase = ControlPhase::Idle;
    }
}

#[cfg(test)]
pub(crate) fn pending_user_route_intent(intent: UserRouteIntent) -> bool {
    PLAYER_CONTROL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pending_user
        == Some(intent)
}

#[cfg(test)]
pub(crate) fn reset_player_control_for_test(ps: &PlaybackSession) {
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
    control.pending_auto = None;
    control.pending_seek_ns = None;
    control.phase = ControlPhase::Stable;
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
    let Some(client) = crate::plex::client_for(check.sid) else {
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
        let fallback = check.clone();
        if !crate::task::spawn_small("abr-cleanup", move || run_encoder_cleanup_check(check)) {
            run_encoder_cleanup_check(fallback);
        }
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
#[cfg(all(test, feature = "devtriggers"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActiveEncoderRefusal {
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
        applied.ceiling = Some(hls.rung.ceiling());
        applied.remux = false;
        control.applied_projection = Some(applied.clone());
        (ticket, hls)
    };
    { let s = &mut *ps; {
        s.url = active.1.url.clone();
        s.tsession = active.0.encoder().to_owned();
        // An adaptive commit changes the encoder, URL and requested rung, not the delivery
        // contract. Preserve the route's negotiated segment duration instead of fabricating one
        // here: seek/reload must carry the exact server contract that created this worker.
        s.cur_ceiling = Some(active.1.rung.ceiling());
        s.cur_remux = false;
    } };
    // The applied clone retained above intentionally excludes Session's staged user fields:
    // rejection combines the newest physical HLS route with the last accepted track contract.
    Some(active)
}

fn is_worker_ticket_current(expected: &WorkerTicket) -> bool {
    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    ticket_is_current(&control, expected)
}

/// Owned, worker-safe inputs for HLS replacement sessions. Constructed on the main thread before
/// the demux worker starts; it never reads the mutable route session afterwards.
#[derive(Clone)]
pub(crate) struct HlsAbrControl {
    trace_generation: u32,
    sid: ServerId,
    rating_key: String,
    logical_session: String,
    audio_stream_id: i64,
    subtitle_stream_id: i64,
    seconds_per_segment: u8,
    pub(crate) initial_rung: crate::abr::Rung,
    /// Response facts carried with the active physical route across seek/reload. They seed the
    /// worker's response state but never alter the request actuator above.
    pub(crate) initial_observed: Option<(crate::abr::ObservedHlsVariant, u32)>,
    fixture_base: String,
    /// Raw Part key in production; a complete URL only for the no-PMS fixture.  Runtime source
    /// measurements bind this Part to the exact active HLS resource instead of minting an AdHoc
    /// identity: PMS's token fallback makes a supposedly separate identity non-owning anyway.
    original_probe_part: String,
    original_source_kbps: u32,
    /// This playback's actuator set, with the device's decode bound and the source raster already
    /// applied — so the worker cannot propose a rendition that could never decode or that would
    /// only make PMS upscale.
    pub(crate) catalog: crate::abr::HlsActuatorCatalog,
    /// The startup probe as a weak prior for the live estimator, when there was one.
    pub(crate) prior: Option<crate::abr::CapacityEstimate>,
    /// Visible switches already spent, so anti-flapping survives the engine replacement that each
    /// switch performs.
    pub(crate) history: crate::abr::TransitionHistory,
    /// The source carries something no transcode can give back (Dolby Vision, Atmos). Makes
    /// Original's utility bonus about this file rather than about Original in general.
    pub(crate) original_features: crate::abr::SourceFeatures,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutoOriginalReload {
    Direct,
    Remux,
}

pub(crate) struct PrimedHls {
    pub(crate) url: String,
    pub(crate) encoder_session: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OriginalProbeResult {
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
pub(crate) enum OriginalProbeFailure {
    HttpStatus(i32),
    Deadline,
    Transport,
    NoBody,
    Other,
}

/// Why the final candidate ownership transaction did not publish. No arm performs cleanup: the
/// caller still owns the candidate on every refusal and must retire it outside AQ/ACTIVE locks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsCommitRefusal {
    Session,
    RouteMoved,
    TransitionRejected,
}

impl HlsAbrControl {
    pub(crate) fn trace_generation(&self) -> u32 {
        self.trace_generation
    }

    pub(crate) fn request_original_recovery(
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

    pub(crate) fn observe_active_variant(
        &self,
        expected: &WorkerTicket,
        variant: crate::abr::ObservedHlsVariant,
        evidence_kbps: u32,
    ) {
        observe_active_hls(expected, variant, evidence_kbps);
    }

    /// Whether every superseded/rejected physical encoder on this PMS has crossed the server's
    /// exact cleanup point, without starting a request.
    pub(crate) fn encoder_cleanup_ready(&self) -> bool {
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
    pub(crate) fn observe_encoder_cleanup(&self) -> bool {
        if self.fixture_base.is_empty() {
            drive_encoder_cleanup(self.sid)
        } else {
            true
        }
    }

    pub(crate) fn can_recover_original(&self) -> bool {
        self.has_original_candidate() && self.original_source_kbps > 0
    }

    /// Is there a source to go back TO, independent of whether its rate is known. Split out so the
    /// worker's disarm line can say WHICH of the two terms was missing — a plan that never carried
    /// a candidate and a candidate whose bitrate nobody published are different bugs, and the
    /// single boolean could not tell them apart.
    pub(crate) fn has_original_candidate(&self) -> bool {
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
    #[cfg(all(test, feature = "devtriggers"))]
    pub(crate) fn probe_original_while_hls(
        &self,
        expected: &WorkerTicket,
        plan: crate::abr::SourceProbePlan,
    ) -> OriginalProbeResult {
        self.probe_original_while_hls_cancellable(expected, plan, || false)
    }

    pub(crate) fn probe_original_while_hls_cancellable<F>(
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
            let Some(client) = crate::plex::client_for(self.sid) else {
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

    pub(crate) fn original_source_kbps(&self) -> u32 {
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
    pub(crate) fn prime(
        &self,
        expected: &WorkerTicket,
        proposal: crate::abr::Proposal,
        offset_micros: i64,
        deadline: Option<std::time::Instant>,
    ) -> Result<PrimedHls, PrimeRefusal> {
        self.prime_rung(expected, proposal.rung, offset_micros, deadline)
    }

    fn prime_rung(
        &self,
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
        let Some(client) = crate::plex::client_for(self.sid) else {
            return Err(PrimeRefusal::Session);
        };
        // PMS exposes the two session fields separately, but the overlap TV spike proved it
        // cannot prime a replacement while it shares the old X-Plex id: the first encoder dies
        // before the candidate produces segment zero. Couple both wire fields per encoder.
        let spec = transcode_spec(
            &self.rating_key,
            &encoder_session,
            &encoder_session,
            false,
            true,
            crate::plex::TranscodeOffset::from_micros(offset_micros),
            self.audio_stream_id,
            self.subtitle_stream_id,
            Some(rung.ceiling()),
            crate::plex::TranscodeDelivery::FixedHls {
                seconds_per_segment: self.seconds_per_segment,
            },
        );
        // The deadline-bearing path preserves the cause where it is issued. A completed HTTP
        // response (including malformed 2xx) and a transport failure are Control; only the timer
        // which actually stopped the request is Deadline. The active encoder is checked on the far
        // side of the request so a concurrent route change has priority over every one of them.
        let decision = match deadline {
            Some(at) => {
                let outcome = client.transcode_decision_until(&spec, at);
                classify_prime_decision(is_worker_ticket_current(expected), outcome)
            }
            None => {
                let decision = client.transcode_decision(&spec);
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
    pub(crate) fn commit_transition<T>(
        &self,
        expected: &WorkerTicket,
        candidate: &PrimedHls,
        proposal: crate::abr::Proposal,
        observed: (crate::abr::ObservedHlsVariant, u32),
        transition: impl FnOnce() -> Option<T>,
    ) -> Result<(T, WorkerTicket), HlsCommitRefusal> {
        if self.fixture_base.is_empty() && crate::plex::client_for(self.sid).is_none() {
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
    #[cfg(test)]
    pub(crate) fn commit(
        &self,
        expected: &WorkerTicket,
        candidate: &PrimedHls,
        proposal: crate::abr::Proposal,
        observed: (crate::abr::ObservedHlsVariant, u32),
    ) -> bool {
        self.commit_transition(expected, candidate, proposal, observed, || Some(()))
            .is_ok()
    }

    pub(crate) fn retire(&self, encoder: String) {
        if !self.fixture_base.is_empty() {
            return;
        }
        request_encoder_cleanup(self.sid, &encoder);
    }

    pub(crate) fn abandon(&self, candidate: &str) {
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
    let caps = crate::devcaps::caps();
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
            atmos: candidate.immersive,
        })
        .unwrap_or_default()
}

/// Main-thread capture immediately before spawning the HLS demux worker.
pub(crate) fn hls_abr_control(ps: &PlaybackSession) -> Option<(HlsAbrControl, WorkerTicket)> {
    let seconds_per_segment = match cur_delivery(ps) {
        crate::plex::TranscodeDelivery::FixedHls {
            seconds_per_segment,
        } => seconds_per_segment,
        crate::plex::TranscodeDelivery::ProgressiveMkv => return None,
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
pub(crate) struct AutoOriginalWatch {
    pub(crate) ticket: WorkerTicket,
    pub(crate) source_kbps: u32,
    pub(crate) catalog: crate::abr::HlsActuatorCatalog,
    pub(crate) history: crate::abr::TransitionHistory,
    pub(crate) features: crate::abr::SourceFeatures,
}

impl AutoOriginalWatch {
    pub(crate) fn request_hls_fallback(
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
    pub(crate) fn refresh_ticket_after_seek(&mut self) -> bool {
        let current = worker_ticket();
        if current.engine_epoch != self.ticket.engine_epoch || current.route != self.ticket.route {
            return false;
        }
        self.ticket = current;
        true
    }
}

pub(crate) fn auto_original_watch(ps: &PlaybackSession) -> Option<AutoOriginalWatch> {
    let s = &*ps;
    if applied_quality() != Quality::Auto
        || !s.cur_auto_original_watched
        || !matches!(
            s.cur_delivery,
            crate::plex::TranscodeDelivery::ProgressiveMkv
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
/// [`crate::dev::PlayUrl::auto_start_hls`] has the history — the alternative was declaring a
/// source rate no link could carry and relying on a starvation horizon that did not check whether
/// the reserve was draining.
pub(crate) fn arm_auto_fixture(
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
        s.cur_delivery = crate::plex::TranscodeDelivery::ProgressiveMkv;
        s.cur_ceiling = None;
        // **Saying the source raster out loud is load-bearing rather than cosmetic**: an unknown
        // one is treated as unbounded (`HlsActuatorCatalog::limited_to`), which makes the 4K
        // actuator feasible on every case. It was a hardcoded 1080p because
        // `tests/serve_fixtures.py` served no 22000 rung, so such a candidate would 404 and read
        // on the television as a rejected encoder — a fixture gap standing in for a policy, and
        // the thing that kept the plan's I9 blocked. The server answers 22000 now, so the caller
        // declares it (`dev::PlayUrl::source_raster`) and the default is still 1080p.
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
            acodec: "aac".into(),
            fps: 0.0,
            dovi: crate::metadata::Dovi::NONE,
            dv_decision: crate::metadata::DvDecision::NONE,
            immersive: false,
            audio_sid: 0,
            audio_ordinal: None,
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
    // hand the caller the playlist to open. See `dev::PlayUrl::auto_start_hls` for why this exists
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
        s.cur_remux = false;
        s.cur_delivery = crate::plex::TranscodeDelivery::FixedHls {
            seconds_per_segment: 2,
        };
        s.cur_ceiling = Some(rung.ceiling());
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
#[cfg(all(test, feature = "devtriggers"))]
pub(crate) fn fallback_auto_to_hls(ps: &mut PlaybackSession, measured_kbps: u32, offset_secs: i64) -> Option<String> {
    let expected = worker_ticket();
    fallback_auto_to_hls_for(ps, &expected, measured_kbps, offset_secs)
}

pub(crate) fn fallback_auto_to_hls_for(
    ps: &mut PlaybackSession,
    expected: &WorkerTicket,
    measured_kbps: u32,
    offset_secs: i64,
) -> Option<String> {
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
    install_auto_hls(
        ps,
        expected,
        rung,
        offset_secs,
        true,
        crate::player::report::DeliveryReason::LinkFallback,
    )
}

/// Replace an Auto Original route whose source request never opened.
///
/// An HTTP 4xx/5xx, connect refusal or demux open error before the first body byte is not a
/// zero-throughput observation. Reuse the exact contingency [`crate::abr::bootstrap`] chose while
/// it still owned the evidence. For Remote that rung came from the completed source probe; for
/// Local it remains the unknown-link fallback — source consumption is demand, not capacity.
pub(crate) fn fallback_unopened_auto_to_hls(ps: &mut PlaybackSession, offset_secs: i64) -> Option<String> {
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
    install_auto_hls(
        ps,
        &expected,
        rung,
        offset_secs,
        false,
        crate::player::report::DeliveryReason::OriginalOpenRollback,
    )
}

/// Commit the common Original→HLS route mutation after the caller has chosen a rung from the
/// appropriate evidence.  A starvation handoff is a visible mode switch and is charged to the
/// anti-flap history; a source which never opened showed no Original picture, so its recovery is
/// not charged as a switch the viewer saw.
fn install_auto_hls(
    ps: &mut PlaybackSession,
    expected: &WorkerTicket,
    rung: crate::abr::Rung,
    offset_secs: i64,
    visible_switch: bool,
    reason: crate::player::report::DeliveryReason,
) -> Option<String> {
    let fixture_base = ps.auto_fixture_base.clone();
    let previous = {
        let s = &*ps;
        (
            s.url.clone(),
            s.tsession.clone(),
            s.cur_auto_original_watched,
            s.cur_remux,
            s.cur_delivery,
            s.cur_ceiling,
            s.stream_vcodec.clone(),
            s.stream_acodec.clone(),
            s.stream_fps,
            s.stream_dovi,
            s.stream_dv_decision,
            s.stream_immersive,
        )
    };
    let restore = |ps: &mut PlaybackSession| {
        { let s = &mut *ps; {
            s.url = previous.0.clone();
            s.tsession = previous.1.clone();
            s.cur_auto_original_watched = previous.2;
            s.cur_remux = previous.3;
            s.cur_delivery = previous.4;
            s.cur_ceiling = previous.5;
            s.stream_vcodec = previous.6.clone();
            s.stream_acodec = previous.7.clone();
            s.stream_fps = previous.8;
            s.stream_dovi = previous.9;
            s.stream_dv_decision = previous.10;
            s.stream_immersive = previous.11;
        } };
    };
    { let s = &mut *ps; {
        s.cur_auto_original_watched = false;
        s.cur_remux = false;
        s.cur_delivery = crate::plex::TranscodeDelivery::FixedHls {
            seconds_per_segment: 2,
        };
        s.cur_ceiling = Some(rung.ceiling());
        // These five fields are one declaration of what the television is about to receive.
        // HLS is a full H.264/AAC encode: source FPS, Dolby Vision and E-AC3 JOC/Atmos belong to
        // the Original elementary streams and may not survive this route transition.
        s.stream_vcodec = "h264".into();
        s.stream_acodec = "aac".into();
        s.stream_fps = 0.0;
        clear_output_dv(s);
        s.stream_immersive = false;
    } };
    let finish = |ps: &mut PlaybackSession, url: String| {
        if visible_switch {
            note_visible_switch(ps, ps.now_ms);
        }
        crate::player::report::note_delivery_requested_for(
            playback_trace_generation(),
            crate::player::report::DeliveryClass::Hls,
            crate::player::report::QualityClass::from_rung(rung),
            reason,
        );
        Some(url)
    };
    if !fixture_base.is_empty() {
        let encoder = format!("auto-fixture-{}", rung.kbps());
        let url = format!(
            "{}/{}/master.m3u8?X-Plex-Token=fixture-only",
            fixture_base.trim_end_matches('/'),
            rung.kbps(),
        );
        if replace_active_hls_for(expected, &encoder, &url, rung, None).is_none() {
            restore(ps);
            return None;
        }
        { let s = &mut *ps; {
            s.url = url.clone();
            s.tsession = encoder.clone();
        } };
        return finish(ps, url);
    }
    // Counted only on the paths that really produce a replacement URL. A switch that failed to
    // build is not one the viewer saw, and the anti-flapping penalty prices what they SAW — the
    // pump turns a `None` here into a playback error, not into a mode change.
    match retranscode_for(ps, expected, offset_secs) {
        Some(url) => finish(ps, url),
        None => {
            restore(ps);
            None
        }
    }
}

/// Main-thread half of HLS→Original recovery. The demux worker has already established, from
/// probes of the actual source file, that its uncertainty-discounted delivery estimate clears the
/// source's declared average consumption rate AND that the switch is worth its visible cost for the
/// playback that remains. Re-check the route and atomically retire the encoder identity before
/// changing any playback declaration.
#[cfg(test)]
pub(crate) fn recover_auto_to_original(ps: &mut PlaybackSession, offset_secs: i64) -> Option<AutoOriginalReload> {
    let expected = worker_ticket();
    recover_auto_to_original_for(ps, &expected, offset_secs, quality() == Quality::Auto)
}

pub(crate) fn recover_auto_to_original_for(
    ps: &mut PlaybackSession,
    expected: &WorkerTicket,
    offset_secs: i64,
    automatic: bool,
) -> Option<AutoOriginalReload> {
    // One handoff owns both the unproven replacement and the retained client-side HLS route until
    // decoded frames commit it or an open failure restores that route snapshot. PMS-side HLS
    // cursor continuity is proved only by an actual later HLS response. Re-entering here would
    // replace that PendingOriginal, retire its only retained HLS identity, and leave the first
    // replacement ownerless while neither open had yet proved a frame.
    if original_recovery_pending() {
        return None;
    }
    let contract_allows = if automatic {
        applied_quality() == Quality::Auto
    } else {
        desired_quality() == Quality::Original
    };
    if !contract_allows || !is_transcoding(ps) {
        return None;
    }
    let candidate = ps.auto_original.clone()?;
    // The worker may have committed several HLS encoders since the main-thread plan was installed.
    // Snapshot the physical route before replacing it, otherwise the rollback pairs the newest
    // encoder id with the bootstrap URL/rung and reopens different media at a different position.
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
    let expected_encoder = expected.encoder().to_owned();
    if expected_encoder.is_empty() {
        return None;
    }
    let mut rollback = snapshot_route(ps, expected_encoder.clone(), offset_secs);
    if candidate.direct {
        // The probe and the actual Part body must name the same exact Streaming Resource. A URL
        // left on the logical playback id can token-alias this HLS resource today, then fail a
        // later seek after cleanup because the alias choice is not durable.
        let source_url = if candidate.probe_part.starts_with('/') {
            let client = cur_client(ps)?;
            client
                .direct_play_url(&candidate.probe_part, &expected_encoder)
                .to_url()
        } else {
            candidate.url.clone()
        };
        // Keep the exact id as a source-resource owner, but remove its HLS route projection. On
        // decoded frames confirmation stops only the physical encoder; final teardown takes this
        // id and performs the full resource close.
        if replace_active_encoder_for(expected, &expected_encoder).is_none() {
            return None;
        }
        // **Taken before anything is overwritten.** A raw Part request has no replacement
        // encoder; the empty marker tells rollback there is nothing new to retire.
        { let s = &mut *ps; {
            s.url = source_url;
            s.tsession.clear();
            s.cur_remux = false;
            s.cur_delivery = crate::plex::TranscodeDelivery::ProgressiveMkv;
            s.cur_no_video_copy = false;
            s.cur_ceiling = None;
            s.cur_auto_original_watched = automatic;
            s.cur_audio_sid = candidate.audio_sid;
            s.stream_vcodec = candidate.vcodec.clone();
            s.stream_acodec = candidate.acodec.clone();
            s.stream_fps = candidate.fps;
            s.stream_dovi = candidate.dovi;
            s.stream_dv_decision = candidate.dv_decision;
            s.stream_immersive = candidate.immersive;
        } };
        crate::player::set_audio_track(candidate.audio_ordinal.unwrap_or(-1));
        crate::player::request_subtitle(candidate.subtitle_ordinal.unwrap_or(-1));
        set_pending_original(ps, rollback, automatic);
        // This is a new source attempt. A prior probe's typed failure explains the HLS route we
        // are leaving, not the replacement now being opened; a failure of this open republishes
        // its own exact status from the pump.
        crate::player::clear_original_failure();
        crate::player::log(if automatic {
            "auto: recovered Original direct play; HLS encoder held pending frames"
        } else {
            "quality: Original restored direct play; HLS encoder held pending frames"
        });
        crate::player::report::note_delivery_requested_for(
            playback_trace_generation(),
            crate::player::report::DeliveryClass::Direct,
            crate::player::report::QualityClass::Original,
            crate::player::report::DeliveryReason::OriginalRecovery,
        );
        return Some(AutoOriginalReload::Direct);
    }

    // `/decision` only registers the replacement. Just like a raw Part open, it does not prove
    // that Starfish can read and decode the resulting MKV. Publish the remux without stopping the
    // old HLS encoder, then put both exact identities in PendingOriginal; decoded frames retire
    // HLS, while a failed open restores its client-side route snapshot and retires this unproven
    // remux. Only the next HLS response establishes PMS-side cursor continuity.
    let replacement = prepare_original_remux(ps, &candidate, expected, offset_secs, automatic)?;
    rollback.replacement_encoder = replacement;
    set_pending_original(ps, rollback, automatic);
    crate::player::clear_original_failure();
    crate::player::log(if automatic {
        "auto: recovered Original remux; HLS encoder held pending frames"
    } else {
        "quality: Original restored remux; HLS encoder held pending frames"
    });
    crate::player::report::note_delivery_requested_for(
        playback_trace_generation(),
        crate::player::report::DeliveryClass::Remux,
        crate::player::report::QualityClass::Original,
        crate::player::report::DeliveryReason::OriginalRecovery,
    );
    Some(AutoOriginalReload::Remux)
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
    url: String,
    tsession: String,
    cur_remux: bool,
    cur_delivery: crate::plex::TranscodeDelivery,
    cur_no_video_copy: bool,
    cur_ceiling: Option<crate::plex::Ceiling>,
    cur_auto_original_watched: bool,
    cur_audio_sid: i64,
    stream_vcodec: String,
    stream_acodec: String,
    stream_fps: f64,
    stream_dovi: crate::metadata::Dovi,
    stream_dv_decision: crate::metadata::DvDecision,
    stream_immersive: bool,
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
    deferred_audio: Option<(i32, String, i64)>,
}

#[derive(Default)]
pub(crate) struct DeferredOriginalEffects {
    quality: Option<Quality>,
    audio: Option<(i32, String, i64)>,
}

impl DeferredOriginalEffects {
    fn from_pending(pending: &mut PendingOriginal) -> Self {
        Self {
            quality: pending.deferred_quality.take(),
            audio: pending.deferred_audio.take(),
        }
    }

    fn is_empty(&self) -> bool {
        self.quality.is_none() && self.audio.is_none()
    }
}

pub(crate) struct OriginalRollback {
    pub(crate) offset_ns: i64,
}

impl OriginalRollback {
    pub(crate) fn without_deferred(offset_ns: i64) -> Self {
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
        url: s.url.clone(),
        tsession: s.tsession.clone(),
        cur_remux: s.cur_remux,
        cur_delivery: s.cur_delivery,
        cur_no_video_copy: s.cur_no_video_copy,
        cur_ceiling: s.cur_ceiling,
        cur_auto_original_watched: s.cur_auto_original_watched,
        cur_audio_sid: s.cur_audio_sid,
        stream_vcodec: s.stream_vcodec.clone(),
        stream_acodec: s.stream_acodec.clone(),
        stream_fps: s.stream_fps,
        stream_dovi: s.stream_dovi,
        stream_dv_decision: s.stream_dv_decision,
        stream_immersive: s.stream_immersive,
        adopted_by_user: false,
        charge_visible_switch_on_commit: false,
        deferred_quality: None,
        deferred_audio: None,
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
pub(crate) fn original_recovery_pending() -> bool {
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
pub(crate) fn confirm_original_recovery(ps: &mut PlaybackSession) {
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
/// return its offset. The caller must rebase it through `transcode_seek`, claim a fresh exact Load
/// attempt and settle that attempt before `Stable`; this bookkeeping operation alone says nothing
/// about PMS cursor continuity. Returns `None` when there is nothing pending, in which case every
/// failure in the pump still means exactly what it always did.
pub(crate) fn rollback_original_recovery(ps: &mut PlaybackSession) -> Option<OriginalRollback> {
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
        pending.cur_delivery,
        pending.cur_ceiling.and_then(crate::abr::Rung::from_ceiling),
    ) {
        (crate::plex::TranscodeDelivery::FixedHls { .. }, Some(rung)) => Some(rung),
        _ => None,
    };
    { let s = &mut *ps; {
        s.url = pending.url.clone();
        s.tsession = pending.tsession.clone();
        s.cur_remux = pending.cur_remux;
        s.cur_delivery = pending.cur_delivery;
        s.cur_no_video_copy = pending.cur_no_video_copy;
        s.cur_ceiling = pending.cur_ceiling;
        s.cur_auto_original_watched = pending.cur_auto_original_watched;
        s.cur_audio_sid = pending.cur_audio_sid;
        s.stream_vcodec = pending.stream_vcodec.clone();
        s.stream_acodec = pending.stream_acodec.clone();
        s.stream_fps = pending.stream_fps;
        s.stream_dovi = pending.stream_dovi;
        s.stream_dv_decision = pending.stream_dv_decision;
        s.stream_immersive = pending.stream_immersive;
    } };
    if let Some(rung) = restored_hls {
        install_active_hls(&pending.encoder, &pending.url, rung);
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
pub(crate) fn drop_original_recovery(ps: &PlaybackSession) {
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
    let worker = encoder.clone();
    if crate::task::spawn_small_keeping("abr-original-stop", move || {
        let ok = client.transcode_stop(&worker);
        crate::player::log(&format!(
            "abr: retired superseded encoder after Original handoff ok={}",
            ok as i32
        ));
    })
    .is_none()
    {
        let _ = client.transcode_stop(&encoder);
    }
}

/// The raw Part already exact-reuses `encoder`'s Streaming Resource. Stop only the physical HLS
/// producer; keeping the resource alive is what lets the current body and every later seek remain
/// admitted. [`scrobble_stop`] still owns the id in `ACTIVE_ENCODER` and closes it at teardown.
fn retire_hls_encoder_keep_source(ps: &PlaybackSession, encoder: String) {
    if encoder.is_empty() || !ps.auto_fixture_base.is_empty() {
        return;
    }
    let Some(client) = cur_client(ps) else { return };
    let worker = encoder.clone();
    if crate::task::spawn_small_keeping("abr-original-physical-stop", move || {
        let ok = client.transcode_stop_physical(&worker);
        crate::player::log(&format!(
            "abr: stopped HLS encoder while retaining Original resource ok={}",
            ok as i32
        ));
    })
    .is_none()
    {
        let _ = client.transcode_stop_physical(&encoder);
    }
}
/// true while this playback is a server transcode (a live transcode session exists). Cheap
/// in-place check — the pump polls it every tick, so no String clone here.
pub(crate) fn is_transcoding(ps: &PlaybackSession) -> bool {
    !ps.tsession.is_empty()
}
/// Did the server REFUSE this item at `/decision`, before playback? Cheap in-place check —
/// `player::state()` derives `Error` from it on every frame of the player route.
pub(crate) fn play_refused(ps: &PlaybackSession) -> bool {
    ps.play_verdict.is_some()
}
/// The app/worker failed to produce a playable plan, as distinct from a PMS `/decision` refusal.
/// There is no Engine whose pump could publish Error, so `player::state()` derives it beside the
/// refusal case.
pub(crate) fn play_resolution_failed(ps: &PlaybackSession) -> bool {
    ps.resolve_failed
}
/// The refusal's own sentence for the read-out to quote — `None` when the server did not refuse,
/// `Some("")` when it refused without saying why. MAIN THREAD (see [`PlaybackSession::play_verdict`]).
///
/// Borrowed, not cloned: the read-out asks for this 2–3× on every frame of a failure (the HUD
/// caption, the read-out itself, and the diagnostics panel when it is open), and every one of them
/// only reads it. The borrow lives until the next main-thread write, which is `apply_plan` or
/// `request_play` — neither of which can run inside a frame's draw.
pub(crate) fn play_verdict(ps: &PlaybackSession) -> Option<&str> {
    ps.play_verdict.as_deref()
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
/// SAME session value may have left one standing. It no longer needs `crate::testlock::serial()`
/// for this reason — since phase 9 a test owns its session outright and cannot leave a refusal in
/// anybody else's.
#[cfg(test)]
pub(crate) fn clear_play_verdict_for_test(ps: &mut PlaybackSession) {
    clear_play_verdict(ps)
}
/// select the subtitle to BURN into any transcode of the current item (0 = none). This
/// is the transcode path; direct-play uses the client renderer (player::request_subtitle).
pub(crate) fn set_subtitle(ps: &mut PlaybackSession, sid: i64) {
    { let s = &mut *ps; s.cur_sub_sid = sid }
}
/// the subtitle stream id currently burned into the transcode (0 = none).
pub(crate) fn cur_sub_sid(ps: &PlaybackSession) -> i64 {
    ps.cur_sub_sid
}
/// ratingKey of the currently-playing item (for /:/timeline progress reports).
pub(crate) fn cur_rk(ps: &PlaybackSession) -> String {
    ps.cur_rk.clone()
}
/// The server the currently-playing item came from — see [`PlaybackSession::cur_sid`]. MAIN THREAD.
///
/// A worker must be handed this by value at its spawn site, never call it: read on a worker it is
/// "whatever is playing now", which is the very race capturing the id was meant to end.
pub(crate) fn cur_sid(ps: &PlaybackSession) -> ServerId {
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
#[cfg(test)]
pub(crate) fn swap_cur_sid_for_test(ps: &mut PlaybackSession, sid: ServerId) -> ServerId {
    { let s = &mut *ps; std::mem::replace(&mut s.cur_sid, sid) }
}
/// The `Client` for the currently-playing item's server, `None` before the first play (or after a
/// plan that never resolved). The main-thread twin of `client_for(env.sid)` on the resolve worker
/// — every in-playback PMS call in this file goes through one of the two, and none through
/// `client_opt()`, which answers with whatever server is CURRENT rather than the one playing.
fn cur_client(ps: &PlaybackSession) -> Option<&'static crate::plex::Client> {
    crate::plex::client_for(cur_sid(ps))
}
pub(crate) fn cur_audio_sid(ps: &PlaybackSession) -> i64 {
    ps.cur_audio_sid
}
/// The currently-playing item's Part id. Written once per item by `build_stream` from its own
/// `part` argument. In-playback callers (audio switch, subtitle toggle, retranscode) want this;
/// `build_stream` must pass its freshly-derived local instead, since this is not yet updated
/// for the item being started.
fn cur_part_id(ps: &PlaybackSession) -> i64 {
    ps.cur_part_id
}
/// The stable app-owned playback generation (and the first encoder's PMS session id).
pub(crate) fn sess(ps: &PlaybackSession) -> String {
    ps.sess.clone()
}
pub(crate) fn pq_id(ps: &PlaybackSession) -> String {
    ps.pq_id.clone()
}
pub(crate) fn pq_item_id(ps: &PlaybackSession) -> String {
    ps.pq_item_id.clone()
}
/// The streamed item's Media video/audio codec, so the player picks the H265 Load payload for a
/// native HEVC direct-play and the matching audio codec.
pub(crate) fn stream_vcodec(ps: &PlaybackSession) -> String {
    ps.stream_vcodec.clone()
}
pub(crate) fn stream_acodec(ps: &PlaybackSession) -> String {
    ps.stream_acodec.clone()
}
/// direct-play source video fps for the Load esInfo (0 = unknown/transcode → omit)
pub(crate) fn stream_fps(ps: &PlaybackSession) -> f64 {
    ps.stream_fps
}
/// The direct-played file's Dolby Vision layering, for the Load payload's `DolbyHdrInfo` node.
/// `Dovi::NONE` for anything the server is transcoding or remuxing, and for a DV file we refused
/// to declare — in every one of those cases the payload must say nothing.
pub(crate) fn stream_dovi(ps: &PlaybackSession) -> crate::metadata::Dovi {
    let s = &*ps;
    if s.stream_vcodec.eq_ignore_ascii_case("hevc") {
        s.stream_dovi
    } else {
        // Last-line consistency guard for dev declarations and future route mutations: the LG
        // payload cannot truthfully describe Dolby Vision on a non-HEVC elementary stream.
        crate::metadata::Dovi::NONE
    }
}
/// The capability and presentation frozen into this installed route. Unlike the raw DOVI metadata,
/// this is the value the Load payload must consume without consulting the live capability cache.
pub(crate) fn stream_dv_presentation(ps: &PlaybackSession) -> crate::metadata::DvPresentation {
    if ps.stream_vcodec.eq_ignore_ascii_case("hevc") {
        ps.stream_dv_decision.presentation
    } else {
        crate::metadata::DvPresentation::NotDv
    }
}

pub(crate) fn stream_dv_decision(ps: &PlaybackSession) -> crate::metadata::DvDecision {
    ps.stream_dv_decision
}

/// Server output (HLS encode, progressive encode or container remux) carries no client-frozen
/// source declaration. Centralizing the paired reset prevents a future route mutation from
/// clearing the raw metadata while leaving a stale `Declare` behind for Load.
fn clear_output_dv(session: &mut PlaybackSession) {
    session.stream_dovi = crate::metadata::Dovi::NONE;
    session.stream_dv_decision = crate::metadata::DvDecision::NONE;
}
/// Is the audio being fed a Dolby Atmos stream? — the Load payload's `contents.immersive` node.
/// See [`PlaybackSession::stream_immersive`].
pub(crate) fn stream_immersive(ps: &PlaybackSession) -> bool {
    let s = &*ps;
    // This pipeline's Atmos path is E-AC3 JOC.  AAC/AC3 are ordinary output even if a stale
    // source flag exists, so neither diagnostics nor Load may repeat that source-only claim.
    s.stream_acodec.eq_ignore_ascii_case("eac3") && s.stream_immersive
}
/// Override the audio codec used to build the Load payload — set by a native audio-track
/// switch to the chosen track's codec before the direct-play reload.
pub(crate) fn set_stream_acodec(ps: &mut PlaybackSession, codec: &str) {
    { let s = &mut *ps; s.stream_acodec = codec.to_owned() }
}
/// Record the streamed item's video+audio codec pair in one write (the Load-payload source of
/// truth) for route-policy tests that install a synthetic live HLS response.
#[cfg(test)]
pub(crate) fn set_stream_codecs(ps: &mut PlaybackSession, vc: &str, ac: &str) {
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
/// * a fixed quality's HLS: the source capped by that quality's [`crate::plex::Ceiling`] — PMS
///   never upscales, so the smaller of the two is the largest picture it can send;
/// * Auto: the bounding box of every FEASIBLE actuator (`HlsActuatorCatalog::widest_feasible_raster`),
///   because a rung commit never re-issues `Load` and the controller may climb to the 4K point
///   inside the one declaration — a ceiling sized to the bootstrap rung (`plan.ceiling`, which is
///   the STARTING rung and not the maximum) would be exceeded on the first climb.
///
/// Main thread, like every other session read. Pure over the session it is given.
pub(crate) fn sink_max_raster(ps: &PlaybackSession) -> (u16, u16) {
    let s = &*ps;
    let clamp = |v: i64| u16::try_from(v).unwrap_or(u16::MAX);
    let source = (clamp(s.cur_src.1), clamp(s.cur_src.2));
    match s.cur_delivery {
        crate::plex::TranscodeDelivery::ProgressiveMkv => source,
        crate::plex::TranscodeDelivery::FixedHls { .. } => {
            if applied_quality() == Quality::Auto {
                auto_catalog(ps).widest_feasible_raster()
            } else {
                match s.cur_ceiling {
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
pub(crate) fn set_stream_source_raster(ps: &mut PlaybackSession, w: u16, h: u16) {
    { let s = &mut *ps; {
        s.cur_src.1 = i64::from(w);
        s.cur_src.2 = i64::from(h);
    } };
}

/// The whole Load-payload DECLARATION for a stream the app did not SELECT — the pipeline test
/// tier's `/tmp/plxnative-playurl` ([`crate::dev::PlayUrl`]), whose entire point is that no PMS
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
pub(crate) fn set_stream_declaration(
    ps: &mut PlaybackSession,
    vc: &str,
    ac: &str,
    fps: f64,
    dovi: crate::metadata::Dovi,
    immersive: bool,
) -> bool {
    set_stream_declaration_with_capability(
        ps,
        vc,
        ac,
        fps,
        dovi,
        immersive,
        crate::webos::caps::capability(),
    )
}

fn set_stream_declaration_with_capability(
    ps: &mut PlaybackSession,
    vc: &str,
    ac: &str,
    fps: f64,
    dovi: crate::metadata::Dovi,
    immersive: bool,
    capability: crate::webos::caps::DvCapability,
) -> bool {
    let decision = crate::metadata::DvDecision {
        capability,
        presentation: dovi.presentation(
            !crate::metadata::dv_withheld(),
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

#[cfg(test)]
pub(crate) fn set_stream_declaration_for_test(
    ps: &mut PlaybackSession,
    vc: &str,
    ac: &str,
    fps: f64,
    dovi: crate::metadata::Dovi,
    immersive: bool,
    capability: crate::webos::caps::DvCapability,
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
pub(crate) fn is_remux(ps: &PlaybackSession) -> bool {
    ps.cur_remux
}
/// Did this playback forbid the server a video stream COPY? Read by the seek and audio-switch
/// rebuilds so the constraint survives them — see [`PlaybackSession::cur_no_video_copy`].
fn is_no_video_copy(ps: &PlaybackSession) -> bool {
    ps.cur_no_video_copy
}
/// The quality ceiling THIS playback was resolved under — read by the two query rebuilds
/// ([`transcode_seek`], [`retranscode`]) so a rung picked mid-film cannot reshape the encode
/// already on screen. See [`PlaybackSession::cur_ceiling`].
fn cur_ceiling(ps: &PlaybackSession) -> Option<crate::plex::Ceiling> {
    ps.cur_ceiling
}
fn cur_delivery(ps: &PlaybackSession) -> crate::plex::TranscodeDelivery {
    ps.cur_delivery
}

/// Whether the live route is the segmented HLS transport. The player uses this at the Starfish
/// Load boundary: HLS must prime both elementary-stream lanes before starting the audio-master
/// clock, even on an ordinary play-from-zero where no seek rebase is pending.
pub(crate) fn is_segmented_hls(ps: &PlaybackSession) -> bool {
    matches!(
        cur_delivery(ps),
        crate::plex::TranscodeDelivery::FixedHls { .. }
    )
}
pub(crate) fn source_vcodec(ps: &PlaybackSession) -> String {
    ps.src_vcodec.clone()
}

/// **Can the pixels of this playback's source reach the panel untouched?** See
/// [`PlaybackSession::cur_source_decodable`]. `false` is the state in which the quality ladder's
/// "Original" row is a promise the pipeline cannot keep.
///
/// Deliberately NOT `is_transcoding()`: that says what is happening now, and a fixed rung makes it
/// true of any source. This says what is POSSIBLE, which is the question a picker is asked.
pub(crate) fn source_decodable(ps: &PlaybackSession) -> bool {
    ps.cur_source_decodable
}
pub(crate) fn source_acodec(ps: &PlaybackSession) -> String {
    ps.src_acodec.clone()
}
/// pointers into the module-owned HUD buffers (valid for the whole frame draw_text uses them)
pub(crate) fn title_cptr(ps: &PlaybackSession) -> *const c_char {
    ps.title.as_ptr()
}
pub(crate) fn ctxline_cptr(ps: &PlaybackSession) -> *const c_char {
    ps.ctxline.as_ptr()
}
struct ScrobbleWork {
    client: Option<&'static crate::plex::Client>,
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
            crate::task::join("timeline", t);
        }
        if let Some((rk, t_ms, d_ms)) = self.final_report.take() {
            let ok = {
                let _effect = TIMELINE_EFFECT.lock().unwrap_or_else(|e| e.into_inner());
                self.client.is_some_and(|c| {
                    c.timeline(&crate::plex::TimelineReport {
                        rating_key: &rk,
                        state: crate::plex::TimelineState::Stopped,
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
            crate::log(&format!(
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
            crate::log(&format!("transcode stopped ok={}", ok as i32));
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
pub(crate) fn scrobble_stop(
    ps: &mut PlaybackSession,
    final_report: Option<(String, i64, i64)>,
    report_th: Option<std::thread::JoinHandle<()>>,
) {
    if ps.preview {
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
        s.cur_remux = false;
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
    let h = crate::task::spawn_small_keeping("scrobble", move || {
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
            crate::task::join("scrobble", handle);
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
pub(crate) fn drain_scrobble() {
    SCROBBLE_JOIN.drain();
}

/// Seek within a LIVE TRANSCODE by restarting it at a time offset — a transcode has no byte-Cues,
/// so a byte-Range seek can't work (docs/plex-api.md). Registers a fresh physical encoder, swaps
/// the route to its delivery-matched start endpoint with `offset={secs}`, then retires the old
/// exact key. Returns the new URL (the demux re-opens it from byte 0), or `None` if this playback
/// is not a transcode or PMS refuses the replacement. The old stream stays live until the new
/// decision has succeeded and the route publication wins, so a failed seek cannot cut playback.
pub(crate) fn transcode_seek(ps: &mut PlaybackSession, offset_secs: i64) -> Option<String> {
    if transcode_session(ps).is_empty() {
        return None;
    }
    let rk = cur_rk(ps);
    if rk.is_empty() {
        return None;
    }
    let c = cur_client(ps)?;
    // A plain seek/foreground resume has no claimed RouteAction, but it still replaces the PMS
    // route and native Engine. Reserve the same start transaction before exposing any candidate
    // fields; an action already in Applying owns its own later Prepared edge and returns None here.
    let route_start = begin_route_start();
    let reject_preparation = || {
        if let Some(ticket) = route_start {
            let _ = reject_route_start_preparation(ticket);
        }
    };
    let live_hls = sync_active_hls_to_session(ps);
    let expected = live_hls
        .as_ref()
        .map(|(ticket, _)| ticket.clone())
        .unwrap_or_else(worker_ticket);
    let previous = expected.encoder().to_owned();
    if previous.is_empty() {
        reject_preparation();
        return None;
    }
    let logical_session = sess(ps);
    let namespace = if logical_session.is_empty() {
        previous.as_str()
    } else {
        logical_session.as_str()
    };
    let replacement = next_encoder_session(namespace);
    let sp = transcode_spec(
        &rk,
        &replacement,
        &replacement,
        is_remux(ps),
        is_no_video_copy(ps),
        crate::plex::TranscodeOffset::from_seconds(offset_secs.max(0)),
        cur_audio_sid(ps),
        cur_sub_sid(ps),
        cur_ceiling(ps),
        cur_delivery(ps),
    );
    let Some(decision) = c.transcode_decision(&sp) else {
        // A lost response may still have registered the key. The old route remains published;
        // clean up only the uncommitted replacement.
        let _ = c.transcode_stop(&replacement);
        reject_preparation();
        return None;
    };
    if refusal(&decision).is_some() {
        let _ = c.transcode_stop(&replacement);
        reject_preparation();
        return None;
    }
    let url = c.transcode_start_url(&sp).to_url();
    let replacement_published = if let Some((_, hls)) = live_hls.as_ref() {
        // This is a NEW PMS response. Carrying the old decoded raster would turn the previous
        // session's observation into a claim about bytes nobody has opened yet; the new demux
        // publishes its own master declaration and decoded raster after the reload.
        replace_active_hls_for(&expected, &replacement, &url, hls.rung, None).is_some()
    } else {
        replace_active_encoder_for(&expected, &replacement).is_some()
    };
    if !replacement_published {
        let _ = c.transcode_stop(&replacement);
        reject_preparation();
        return None;
    }
    { let s = &mut *ps; {
        s.tsession = replacement.clone();
        s.url = url.clone();
        if let Some((_, hls)) = live_hls.as_ref() {
            s.cur_ceiling = Some(hls.rung.ceiling());
        }
    } };
    publish_applied_route_projection(ps);
    if let Some(ticket) = route_start {
        if !prepare_route_start(ticket) {
            crate::player::log("seek: prepared PMS route lost its start transaction");
            return None;
        }
    }

    // The route now names the replacement; the caller will tear down the old demux immediately
    // and reopen this URL. Retire the old exact PMS key off the main thread, just like an ABR
    // commit, so a slow `/stop` cannot freeze the seek UI.
    let old = previous.clone();
    if crate::task::spawn_small_keeping("seek-stop", move || {
        let ok = c.transcode_stop(&old);
        crate::player::log(&format!("seek: retired previous encoder ok={}", ok as i32));
    })
    .is_none()
    {
        let ok = c.transcode_stop(&previous);
        crate::player::log(&format!(
            "seek: synchronously retired previous encoder ok={}",
            ok as i32
        ));
    }
    Some(url)
}

use crate::cbuf::set as set_c; // shared fixed-C-buffer write (the session's HUD title/ctxline)

// ---- the QUALITY ceiling: what the USER has asked this playback to come in under -------------

/// The playback-quality ladder: **Auto, Original, and a few fixed rungs**, and every rung is a ROUTING POLICY
/// before it is a parameter.
///
/// # Why this is not a bitrate field on [`crate::plex::TranscodeSpec`]
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
/// So a rung is spent the way [`crate::plex::link_policy`] spends the relay tier: **deny the two
/// flavors that ship the file at its own rate, leaving the one flavor whose whole point is that
/// the server picks the rate** ([`quality_policy`]). Only then does the rung's number reach the
/// wire, as [`crate::plex::Ceiling`] on the re-encode query.
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
pub(crate) fn available_quality_ladder() -> &'static [Quality] {
    quality_ladder_for(auto_quality_ready())
}

impl Quality {
    /// The bound this rung imposes. Original has none; Auto also has no fixed ceiling because its
    /// adaptive owner selects one dynamically once that path is ready.
    pub(crate) fn ceiling(self) -> Option<crate::plex::Ceiling> {
        let (max_kbps, max_w, max_h) = match self {
            Quality::Auto | Quality::Original => return None,
            Quality::P1080High => (20000, 1920, 1080),
            Quality::P1080 => (8000, 1920, 1080),
            Quality::P720 => (4000, 1280, 720),
            Quality::P720Low => (2000, 1280, 720),
            Quality::P480 => (720, 854, 480),
        };
        Some(crate::plex::Ceiling {
            max_kbps,
            max_w,
            max_h,
        })
    }

    /// The picker's row text. ONE string per rung rather than a label plus a trailing value,
    /// because the row already carries the picker's leading checkmark — `ui/table.rs`'s rule is
    /// that a mark says where you are and a word says what is set, and no row says both.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Quality::Auto => "Auto",
            Quality::Original => "Original",
            Quality::P1080High => "1080p \u{b7} 20 Mbps",
            Quality::P1080 => "1080p \u{b7} 8 Mbps",
            Quality::P720 => "720p \u{b7} 4 Mbps",
            Quality::P720Low => "720p \u{b7} 2 Mbps",
            Quality::P480 => "480p \u{b7} 720 kbps",
        }
    }

    /// An in-memory index back to a rung — out of range is `Original`, never a neighbouring rung,
    /// for the same reason `more_menu::action_at` refuses one: the ladder can grow or shrink.
    fn from_index(i: u8) -> Quality {
        QUALITY_LADDER
            .get(i as usize)
            .copied()
            .unwrap_or(Quality::Original)
    }

    fn index(self) -> u8 {
        QUALITY_LADDER.iter().position(|&r| r == self).unwrap_or(1) as u8
    }
}

/// The user's current pick. An atomic rather than a field on [`Session`] because it OUTLIVES a
/// playback — it is a preference, not session state — and because `ui::more_menu` reads it to draw
/// the checkmark while [`ResolveEnv::snapshot`] reads it to hand the worker a copy.
///
/// Seeded to Original even before the boot gate restores the session: no call path may turn a
/// missing preference into Auto simply because initialization order changed.
static QUALITY: AtomicU8 = AtomicU8::new(1);

/// The selected ceiling. Safe from any thread; the resolve worker gets a COPY through
/// [`ResolveEnv`] rather than reading this, per that struct's own rule.
pub(crate) fn quality() -> Quality {
    Quality::from_index(QUALITY.load(Ordering::Relaxed))
}

/// Restore the persisted preference without writing it back. The distinction matters for legacy
/// sessions: their missing field resolves to Original but remains missing until the user makes an
/// explicit choice, so a future Auto-ready build still cannot reinterpret that old install.
pub(crate) fn restore_quality(q: Quality) {
    QUALITY.store(supported_quality(q).index(), Ordering::Relaxed);
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
    QUALITY.store(q.index(), Ordering::Relaxed);
    // A session write is a read-modify-write under the session lock: changing this preference
    // must not overwrite a roster refresh, a profile switch, or another profile's recents.
    let _ = crate::plex::session::queue_update(move |s| {
        if s.playback_quality == Some(q) {
            None
        } else {
            Some(s.with_playback_quality(q))
        }
    });
    // The picker's checkmark moves on this and on nothing else — a settled popover presents no
    // frames, so without this the row would still read as the old rung until the next keypress.
    crate::ui::idle::invalidate();
    q
}

/// Persist a quality choice made from the terminal failure screen, without trying to mutate the
/// dead live route.  The caller starts a fresh resolve immediately; [`ResolveEnv`] will consume
/// this preference there.
pub(crate) fn set_quality_for_retry(q: Quality) {
    let _ = persist_quality_choice(q);
}

pub(crate) fn set_quality(ps: &mut PlaybackSession, q: Quality) {
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
    let location = crate::plex::client_for(cur_sid(ps)).and_then(|client| client.link());
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
        && (location == Some(crate::plex::probe::Location::Local)
            || (location == Some(crate::plex::probe::Location::Remote)
                && (!is_transcoding(ps) || is_remux(ps))));
    let adaptive = auto_uses_hls(q, auto_original);
    let delivery = if adaptive {
        crate::plex::TranscodeDelivery::FixedHls {
            seconds_per_segment: 2,
        }
    } else {
        crate::plex::TranscodeDelivery::ProgressiveMkv
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
        s.cur_ceiling = ceiling;
        s.cur_delivery = delivery;
        // Supervision does not depend on where the server is: `auto_original` already says Auto
        // is going to run Original, and that is the whole question the watchdog asks. See the
        // field. It was assigned to an `auto_original_watched` binding first, which named nothing
        // the right-hand side did not already say — a leftover from the `auto_remote_original`
        // refactor, where the two really were different.
        s.cur_auto_original_watched = auto_original;
        if matches!(delivery, crate::plex::TranscodeDelivery::FixedHls { .. }) {
            s.cur_remux = false;
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
pub(super) fn measure_remote_remux(
    client: &crate::plex::Client,
    rk: &str,
    session: &str,
    audio_stream_id: i64,
    subtitle_stream_id: i64,
    source_kbps: i64,
) -> Option<crate::abr::CapacityObservation> {
    let spec = transcode_spec(
        rk,
        session,
        session,
        true,
        false,
        crate::plex::TranscodeOffset::Fresh,
        audio_stream_id,
        subtitle_stream_id,
        None,
        crate::plex::TranscodeDelivery::ProgressiveMkv,
    );
    let Some(decision) = client.transcode_decision(&spec) else {
        crate::player::log("auto: remote remux preflight had no /decision; using HLS");
        return None;
    };
    if refusal(&decision).is_some() {
        crate::player::log("auto: remote remux preflight refused by /decision; using HLS");
        return None;
    }
    let sample = measure_remote_original(&client.transcode_start_url(&spec).to_url(), source_kbps);
    if sample.is_none() {
        // Same playback identity the HLS/remux that follows will register. closeResourceSession=1
        // would 503 that next start; physical-stop keeps the Streaming Resource.
        let _ = client.transcode_stop_physical(session);
    }
    sample
}

/// MDE handshake result. `None` from [`server_decision`] means the body was missing or unusable:
/// the caller must not Original, but remux is still allowed. `Part.decision=transcode` is a
/// container change, not a video-copy veto — see [`crate::plex::MediaPart::video_forbids_copy`].
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
    c: &crate::plex::Client,
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
pub(super) fn put_selection(sid: ServerId, part: i64, aud: i64, sub: i64) {
    if part <= 0 {
        return;
    }
    let c = match crate::plex::client_for(sid) {
        Some(c) => c,
        None => return,
    };
    let st = c.select_streams(&crate::plex::StreamSelection {
        part_id: part,
        audio_stream_id: aud,
        subtitle_stream_id: sub,
    });
    crate::player::log(&format!(
        "select streams: part={part} audio={aud} sub={sub} -> HTTP {st}"
    ));
}

/// What the queue told us, as owned data for `apply_plan` to install. `machine_id` is `""` when
/// the cached one is still good.
#[derive(Default)]
pub(super) struct QueueInfo {
    pub(super) machine_id: String,
    pub(super) id: String,
    pub(super) item_id: String,
    pub(super) up_next: Option<UpNext>,
    pub(super) rows: Vec<crate::plex::QueueRow>,
}

/// The queued next episode. Main-thread only, and — like `metadata::playing()` (via
/// `MetadataView`, which hands out a `&'a` borrow of the owner's state) — it hands out a
/// reference the Up Next control reads across a frame, so `apply_plan` (main thread) staying its
/// only writer is what keeps that reference sound. A caller that STARTS the next episode must
/// clone first: `request_play` clears this before the new plan lands.
pub(crate) fn up_next(ps: &PlaybackSession) -> Option<&UpNext> {
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
pub(crate) fn with_queue<R>(ps: &PlaybackSession, f: impl FnOnce(&[crate::plex::QueueRow]) -> R) -> R {
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
    c: &crate::plex::Client,
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
    /// MAIN THREAD ONLY.
    /// `sid` arrives BY VALUE from the caller, which is the whole point: the item being played
    /// carries the server it came from (`PmsMovie`/`UpNext`/`Detail` all hold one now), so a play
    /// raised off a merged shelf resolves against the server that shelf's row belongs to rather
    /// than whichever server happens to be current when the worker gets around to asking.
    fn snapshot(ps: &PlaybackSession, meta: crate::metadata::MetadataView<'_>, sid: ServerId, rk: &str) -> ResolveEnv {
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
            cached_item: meta.cached_playing(sid, rk),
            quality: quality(),
            src_kbps: resolve_src_kbps(meta.current(), sid, rk),
            omit_queue_continuous: false,
            preview: false,
            #[cfg(test)]
            dv_capability: None,
        }
    }
}

pub(crate) fn playback_preview(d: &crate::metadata::Detail) -> Option<Preview> {
    playback_preview_with_capability(d, None)
}

fn playback_preview_with_capability(
    d: &crate::metadata::Detail,
    capability: Option<crate::webos::caps::DvCapability>,
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
            !crate::metadata::dv_withheld(),
            capability,
            vcodec.eq_ignore_ascii_case("hevc"),
        ),
        None => d.dovi.presentation_now(vcodec.eq_ignore_ascii_case("hevc")),
    };
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
    let location = crate::plex::client_for(d.sid).and_then(|client| client.link());
    // A detail page has not downloaded the file yet, so it reports what can preserve the source,
    // not a fictitious failed bandwidth result. Remote Auto is measured at Play. Relay remains a
    // conversion because its independent link policy refuses both original-rate flavors.
    let policy = flavors_allowed(
        crate::plex::link_policy(location),
        quality_policy(quality(), true, d.bitrate, d.width, d.height),
    );
    Some(match p {
        Preview::DirectPlay if !policy.direct_play => Preview::Converts,
        Preview::Remux if !policy.remux => Preview::Converts,
        _ => p,
    })
}

#[cfg(test)]
pub(crate) fn playback_preview_with_capability_for_test(
    d: &crate::metadata::Detail,
    capability: crate::webos::caps::DvCapability,
) -> Option<Preview> {
    playback_preview_with_capability(d, Some(capability))
}

static PLAY_GEN: AtomicU32 = AtomicU32::new(0);
static PLAY_BUSY: AtomicBool = AtomicBool::new(false);
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
    let Some(client) = crate::plex::client_for(resources.sid) else {
        return;
    };
    let worker_ids = resources.identities.clone();
    if !crate::task::spawn_small("resolve-abandoned-stop", move || {
        for identity in worker_ids {
            let _ = client.transcode_stop(&identity);
        }
    }) {
        // Thread creation failure is rarer than cancellation and must not turn into a permanent
        // server allocation. The normal path above keeps this network work off the main thread.
        for identity in resources.identities {
            let _ = client.transcode_stop(&identity);
        }
    }
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

pub(crate) fn playback_trace_generation() -> u32 {
    ACTIVE_TRACE_GENERATION.load(Ordering::SeqCst)
}

/// True while a resolve is in flight — the HUD renders `PlaybackState::Resolving` from this.
pub(crate) fn play_pending() -> bool {
    PLAY_BUSY.load(Ordering::SeqCst)
}

/// This session is a hero preview. The route backstop must not steal it onto the player.
pub(crate) fn is_preview(ps: &PlaybackSession) -> bool {
    ps.preview
}

pub(crate) fn clear_preview(ps: &mut PlaybackSession) {
    ps.preview = false;
}

/// Attach the UI's resume point to the resolve currently in flight.
///
/// `request_play_*` is issued immediately before `app::start_playback`, so the latter knows the
/// position one call later than the former knows the generation.  Tagging here closes that seam:
/// a cancelled or superseded landing cannot consume a bare process-global resume value.
pub(crate) fn arm_play_resume(ps: &mut PlaybackSession, resume_ns: i64) -> bool {
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
pub(crate) fn surface_sid() -> ServerId {
    crate::plex::current_server()
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
pub(crate) fn request_play(
    ps: &mut PlaybackSession,
    meta: &mut crate::stores::metadata::MetadataStore,
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
pub(crate) fn request_preview(
    ps: &mut PlaybackSession,
    meta: &mut crate::stores::metadata::MetadataStore,
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
            ctx: crate::metadata::TRAILER_CONTEXT.to_owned(),
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
    meta: &mut crate::stores::metadata::MetadataStore,
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
            set_c(s.ctxline.as_mut_ptr(), s.ctxline.len(), ctx);
        }
        s.cur_audio_sid = 0;
        s.cur_sub_sid = 0;
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
        meta.run(crate::stores::metadata::MetadataCmd::RetirePlayingItem);
        crate::player::reset_audio_track();
        crate::player::reset_subtitle();
    }
    // Capture the reducer revision BEFORE projecting the environment. Both happen on the main
    // thread, so a later quality/track edit necessarily advances this revision after the snapshot
    // and makes the landing stale instead of installing an old plan beneath a new checkmark.
    let contract_revision = desired_contract_revision();
    // captured HERE, on the main thread, and moved into the worker — see ResolveEnv
    let mut env = ResolveEnv::snapshot(ps, meta.view(), sid, rk);
    env.omit_queue_continuous = crate::metadata::context_omits_queue_continuous(ctx);
    env.preview = request.preview;
    if let Some(retry) = retry {
        // `request_play` resets the live selection because that is correct for a new item.  A
        // retry is the SAME item: override the fresh defaults with the selection captured before
        // that reset so a rescue does not silently turn subtitles/audio back to server default.
        env.audio_sid = retry.audio_sid;
        env.sub_sid = retry.sub_sid;
    }
    let gen = PLAY_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    if let Some(resume_ns) = retry.map(|r| r.resume_ns).filter(|ns| *ns > 0) {
        *PLAY_RESUME.lock().unwrap_or_else(|e| e.into_inner()) = Some((gen, resume_ns));
    }
    PLAY_BUSY.store(true, Ordering::SeqCst);
    let (rk, part, vc, ac) = (request.rk, request.part, request.vcodec, request.acodec);
    let spawned = crate::task::spawn_small("resolve", move || {
        if drain_previous {
            // The old attempt's `state=stopped` and transcode `/stop` were intentionally moved off
            // the SDL thread.  A user Retry must nevertheless preserve their ordering relative to
            // the replacement encoder, so pay that wait here, on the resolve worker.
            drain_scrobble();
        }
        // catch_unwind OUTSIDE the mailbox write, like load_season: a panicking resolve must still
        // land (as !ok) or PLAY_BUSY latches and the screen wedges on a spinner forever.
        let plan = std::panic::catch_unwind(|| build_stream(&rk, &part, &vc, &ac, &env))
            .unwrap_or_default();
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

/// Start a fresh resolve for the item whose terminal error is still on screen.
///
/// The caller owns Engine teardown; this module owns the immutable request descriptor, track
/// selection and generation-bound resume point.  Returning `false` is honest for URL/dev-trigger
/// playback, which never entered the Plex request funnel and therefore has no item to resolve.
pub(crate) fn can_retry_current_play(ps: &PlaybackSession) -> bool {
    ps.request.is_some()
}

/// Resume target not yet proven by a presented frame.  A refused retry keeps this so the next
/// quality choice can try again at the same point.
pub(crate) fn unpresented_resume_ns(ps: &PlaybackSession) -> i64 {
    ps.requested_resume_ns.max(0)
}

/// The replacement has shown a frame; from now on the live playhead, including a later backward
/// seek, is the only truthful retry position.
pub(crate) fn confirm_resume_presented(ps: &mut PlaybackSession) {
    if ps.requested_resume_ns > 0 {
        { let s = &mut *ps; s.requested_resume_ns = 0 };
    }
}

fn current_retry_context(ps: &PlaybackSession, resume_ns: i64) -> RetryContext {
    RetryContext {
        resume_ns: resume_ns.max(0),
        audio_sid: cur_audio_sid(ps),
        sub_sid: cur_sub_sid(ps),
    }
}

pub(crate) fn retry_current_play(ps: &mut PlaybackSession, meta: &mut crate::stores::metadata::MetadataStore, resume_ns: i64) -> bool {
    let Some(request) = ps.request.clone() else {
        crate::player::log("playback retry: no Plex request descriptor");
        return false;
    };
    crate::player::log(&format!(
        "playback retry: resolving item again at quality {}",
        quality().label(),
    ));
    request_play_inner(ps, meta, request, Some(current_retry_context(ps, resume_ns)), None, true)
}

/// ASYNC twins of `play_movie` / `play_episode`: identical HUD strings and inputs. On `true`, the
/// network work runs on a worker and the caller flips the route THIS frame; an empty or Busy request
/// returns `false` and leaves the current route alone. `app.rs` drains `pump_play` once a frame and
/// starts the engine when the plan lands.
pub(crate) fn request_play_movie(ps: &mut PlaybackSession, meta: &mut crate::stores::metadata::MetadataStore, m: &PmsMovie) -> bool {
    if m.part.is_empty() {
        return false;
    }
    let rating = if m.rating.is_empty() { "NR" } else { &m.rating };
    let ctx = format!(
        "{} \u{b7} {} \u{b7} {}",
        m.year,
        rating,
        crate::ui::fmt::dur_short(m.dur_ns / 1_000_000)
    );
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
        &ctx,
    )
}

/// The server an item's ids belong to: its own when it has one, else the browsed surface.
///
/// A row's `sid` is `UNSET` only before any server was registered (and in host tests, which build
/// rows by literal). Falling back to the surface there keeps the single-server app exactly as it
/// was, while never letting an item that DOES name its server be resolved against another.
pub(crate) fn item_sid(sid: ServerId) -> ServerId {
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
/// pre-roll doesn't change shape underneath the user when it does.
pub(crate) fn request_play_up_next(ps: &mut PlaybackSession, meta: &mut crate::stores::metadata::MetadataStore, u: UpNext) -> bool {
    let ctx = crate::ui::fmt::episode_kicker(u.season, u.index, &u.ep_title);
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
    request_play(ps, meta, sid, &u.rk, &u.part, &u.vcodec, &u.acodec, title, &ctx)
}

/// Supersede an in-flight resolve (BACK during a load). The landing is dropped by generation.
pub(crate) fn cancel_play(ps: &mut PlaybackSession) {
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

/// MAIN THREAD, once a frame. Returns the generation-owned resume point when a playable fresh plan
/// was installed. `Some(0)` means start from the beginning; `None` means no playable landing. A
/// stale landing (and its resume) is dropped.
pub(crate) fn pump_play(ps: &mut PlaybackSession, meta: &mut crate::stores::metadata::MetadataStore) -> Option<i64> {
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
    // exactly the moment it appears in front of the user. `warm_tex`, not `resolve_tex`: this wants
    // the fetch and nothing else, and a slot warmed tens of minutes early must NOT be carrying the
    // evict-protection a draw takes (see `ui::tex::warm_on`). At the tile's OWN 480×270 —
    // `(server, path, w, h, png)` IS the store key, so a warm at any other size buys nothing.
    //
    // It sits HERE, in the once-a-frame pump, rather than inside `apply_plan`: that function's
    // contract is that it is the sole WRITER OF THE SESSION, and a texture prefetch is not part of
    // it. Keeping the two apart also keeps the install reachable from the host suite —
    // `warm_tex` pulls in the poster cache and, through it, a GL call the dev Mac cannot link.
    // The host test binary has no GL symbols; this prefetch is visual-only and production-only.
    // Keeping it out of cfg(test) makes the generation/resource transaction above testable
    // without pretending a desktop unit test can exercise the poster texture path.
    #[cfg(not(test))]
    if let Some(u) = up_next(ps) {
        crate::ui::widgets::warm_tex_on(item_sid(cur_sid(ps)), &u.thumb, 480, 270, 0);
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
fn apply_plan(ps: &mut PlaybackSession, meta: &mut crate::stores::metadata::MetadataStore, plan: Plan, rk: &str) -> Option<RouteStartTransaction> {
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
    meta.run(crate::stores::metadata::MetadataCmd::InstallPlaying(
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
            jail_load_blocked: false,
            repair_status: crate::webos::jail_repair::State::Idle,
            request,
            requested_resume_ns,
            url: plan.url,
            tsession: plan.tsession,
            // Installed on EVERY landing, not only a refusing one: a plan that resolved is itself
            // the statement that the last refusal is over, and assigning unconditionally is what
            // makes that true without a second clear anyone can forget.
            play_verdict: plan.verdict,
            resolve_failed,
            cur_remux: plan.remux,
            cur_delivery: plan.delivery,
            cur_no_video_copy: plan.no_video_copy,
            cur_ceiling: plan.ceiling,
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
            cur_audio_sid: plan.audio_sid,
            // the part/show-selected subtitle (0 = none), so the menu checkmark, the timeline report
            // and any later transcode of this item all agree with what the renderer is told below
            cur_sub_sid: plan.sub_sid,
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
        };
    } };
    if let (crate::plex::TranscodeDelivery::FixedHls { .. }, Some(rung)) = (
        ps.cur_delivery,
        ps
            .cur_ceiling
            .and_then(crate::abr::Rung::from_ceiling),
    ) {
        install_active_hls(&active_encoder, &ps.url, rung);
    } else {
        install_active_encoder(&active_encoder);
    }
    // Restore the external renderer AND the route's stream identity before publishing the
    // start contract, so timeline reports and later audio/quality transcodes keep this pick.
    if plan.sub_render_ordinal.is_none() && !is_transcoding(ps) {
        if let Some(id) = crate::player::sidecar::restore_server_selection(cur_sid(ps), meta.view()) {
            set_subtitle(ps, id);
        }
    }
    let start = prepare_playback_landing(ps, !ps.url.is_empty());
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
    // A landing is a DISCRETE change to what is on screen, so it owes the present gate a poke —
    // `ui::idle::invalidate`'s call-site list is that module's correctness argument. The caller
    // (`app.rs`'s pump) invalidates only when `pump_play` returns TRUE, and a REFUSING plan returns
    // false by construction (empty url) while flipping the player from Resolving to Error. That it
    // still repainted was an accident of the player route bypassing the gate entirely; here it is
    // the rule instead.
    crate::ui::idle::invalidate();
    start
}

/// Register and publish a codec-preserving Original remux without retiring `expected_hls`.
/// `PendingOriginal` owns the two-session commit/rollback after this returns.
fn prepare_original_remux(
    ps: &mut PlaybackSession,
    candidate: &AutoOriginalCandidate,
    expected: &WorkerTicket,
    offset_secs: i64,
    automatic: bool,
) -> Option<String> {
    let c = cur_client(ps)?;
    let rk = cur_rk(ps);
    let expected_hls = expected.encoder();
    if rk.is_empty() || expected_hls.is_empty() {
        return None;
    }
    // The replacement must have its own exact physical/resource identity. Reusing `sess()` can
    // equal the initial HLS encoder and would mutate the very rollback this handoff promises to
    // retain; a fresh child also makes a failed remux safe to stop without touching HLS.
    let logical_session = sess(ps);
    let namespace = if logical_session.is_empty() {
        expected_hls
    } else {
        logical_session.as_str()
    };
    let replacement = next_encoder_session(namespace);
    let subtitle = cur_sub_sid(ps);
    put_selection(cur_sid(ps), cur_part_id(ps), candidate.audio_sid, subtitle);
    let spec = transcode_spec(
        &rk,
        &replacement,
        &replacement,
        true,
        false,
        crate::plex::TranscodeOffset::from_seconds(offset_secs.max(0)),
        candidate.audio_sid,
        subtitle,
        None,
        crate::plex::TranscodeDelivery::ProgressiveMkv,
    );
    let decision = c.transcode_decision(&spec);
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
    let output_codecs = decision
        .as_ref()
        .and_then(decision_codecs)
        .unwrap_or_else(|| (candidate.vcodec.clone(), candidate.acodec.clone()));
    let url = c.transcode_start_url(&spec).to_url();
    if replace_active_encoder_for(expected, &replacement).is_none() {
        let _ = c.transcode_stop(&replacement);
        return None;
    }
    { let s = &mut *ps; {
        s.url = url;
        s.tsession = replacement.clone();
        s.cur_remux = true;
        s.cur_delivery = crate::plex::TranscodeDelivery::ProgressiveMkv;
        s.cur_no_video_copy = false;
        s.cur_ceiling = None;
        s.cur_auto_original_watched = automatic;
        s.cur_audio_sid = candidate.audio_sid;
        s.stream_vcodec = output_codecs.0.clone();
        s.stream_acodec = output_codecs.1.clone();
        s.stream_fps = 0.0;
        clear_output_dv(s);
        s.stream_immersive = false;
    } };
    crate::player::log(&format!(
        "decision output: v={} a={}",
        output_codecs.0, output_codecs.1
    ));
    Some(replacement)
}

/// Re-transcode the current item (the session's `cur_rk`) at `offset_secs`, carrying the CURRENT
/// audio + subtitle selection (transcode_base). Used by an audio switch AND by a subtitle
/// (de)select while transcoding. Works from a direct-play OR transcode state — the result
/// is always a transcode (server always emits AC3, so the pipeline's Loaded codec is
/// unchanged). Sets `url` + `tsession`, runs /decision, and returns the new start.mkv URL
/// (the demux re-opens it from byte 0), or None.
pub(crate) fn retranscode_for(ps: &mut PlaybackSession, expected: &WorkerTicket, offset_secs: i64) -> Option<String> {
    if !is_worker_ticket_current(expected) {
        return None;
    }
    if matches!(
        cur_delivery(ps),
        crate::plex::TranscodeDelivery::FixedHls { .. }
    ) {
        let live = sync_active_hls_to_session(ps);
        if live.as_ref().is_some_and(|(ticket, _)| ticket != expected) {
            return None;
        }
    }
    retranscode_as(ps, expected, offset_secs, false)
}

fn retranscode_as(ps: &mut PlaybackSession, expected: &WorkerTicket, offset_secs: i64, remux: bool) -> Option<String> {
    let c = cur_client(ps)?;
    let rk = cur_rk(ps);
    if rk.is_empty() || !is_worker_ticket_current(expected) {
        return None;
    }
    // Resolve every fallible recovery input before publishing the new route. A missing candidate
    // must leave the still-playing HLS session untouched, not strand it behind a remux marker.
    let remux_codecs = if remux {
        let s = &*ps;
        Some((
            s.src_vcodec.clone(),
            s.auto_original.as_ref()?.acodec.clone(),
        ))
    } else {
        None
    };
    // Snapshot the desired contract, but publish none of it before PMS has answered and the full
    // worker/action ticket still owns the route. This prevents a failed `/decision` from making
    // diagnostics claim the requested 22 Mbps while the old 1.1 Mbps encoder still serves bytes.
    let delivery = cur_delivery(ps);
    let ceiling = cur_ceiling(ps);
    let no_video_copy = is_no_video_copy(ps);
    let audio_sid = cur_audio_sid(ps);
    let subtitle_sid = cur_sub_sid(ps);
    let (fallback_vcodec, fallback_acodec) = if let Some((vcodec, acodec)) = remux_codecs {
        (vcodec, acodec)
    } else if matches!(delivery, crate::plex::TranscodeDelivery::FixedHls { .. }) {
        ("h264".to_owned(), "aac".to_owned())
    } else {
        (
            crate::devcaps::caps().encode_vcodec().to_owned(),
            "ac3".to_owned(),
        )
    };
    put_selection(cur_sid(ps), cur_part_id(ps), audio_sid, subtitle_sid); // drives encode + burn
    let logical = sess(ps);
    let namespace = if logical.is_empty() {
        format!("plxnative-{rk}")
    } else {
        logical
    };
    let qsess = next_encoder_session(&namespace);
    let sp = transcode_spec(
        &rk,
        &qsess,
        &qsess,
        remux,
        no_video_copy,
        crate::plex::TranscodeOffset::from_seconds(offset_secs.max(0)),
        audio_sid,
        subtitle_sid,
        ceiling,
        delivery,
    );
    let Some(decision) = c.transcode_decision(&sp) else {
        let _ = c.transcode_stop(&qsess);
        return None;
    };
    if let Some(reason) = refusal(&decision) {
        crate::player::log(&format!(
            "retranscode decision refused{}",
            if reason.is_empty() {
                ""
            } else {
                ": server supplied a reason"
            },
        ));
        let _ = c.transcode_stop(&qsess);
        return None;
    }
    let output_codecs = decision_codecs(&decision).unwrap_or((fallback_vcodec, fallback_acodec));
    let url = c.transcode_start_url(&sp).to_url();
    let replacement_installed = match (delivery, ceiling.and_then(crate::abr::Rung::from_ceiling)) {
        (crate::plex::TranscodeDelivery::FixedHls { .. }, Some(rung)) => {
            replace_active_hls_for(expected, &qsess, &url, rung, None).is_some()
        }
        _ => replace_active_encoder_for(expected, &qsess).is_some(),
    };
    if !replacement_installed {
        // A concurrent ABR commit or teardown won while the decision request was in flight. Do not
        // reload onto a session which no longer belongs to this playback generation.
        let _ = c.transcode_stop(&qsess);
        return None;
    }
    let expected_encoder = expected.encoder().to_owned();
    { let s = &mut *ps; {
        s.cur_remux = remux;
        s.tsession = qsess.clone();
        s.url = url.clone();
        s.stream_vcodec = output_codecs.0.clone();
        s.stream_acodec = output_codecs.1.clone();
        s.stream_fps = 0.0;
        clear_output_dv(s);
        s.stream_immersive = false;
    } };
    crate::player::log(&format!(
        "decision output: v={} a={}",
        output_codecs.0, output_codecs.1,
    ));
    if !expected_encoder.is_empty() && expected_encoder != qsess {
        let old = expected_encoder;
        let worker_old = old.clone();
        if crate::task::spawn_small_keeping("retranscode-stop", move || {
            let _ = c.transcode_stop(&worker_old);
        })
        .is_none()
        {
            let _ = c.transcode_stop(&old);
        }
    }
    // NEVER log the URL. `transcode_start_url` ends in `X-Plex-Token=…`, and this line is reached
    // by an ordinary audio-track switch — so the app's own support channel ("send us
    // /tmp/plxnative-events.log") was asking users to paste a live PMS credential into a public
    // issue thread. The rk, the track ids and the offset are the whole diagnostic value here; the
    // URL added nothing that is not derivable from them.
    crate::player::log(&format!(
        "retranscode rk={rk} audio={} sub={} offset={offset_secs} -> transcode start",
        audio_sid, subtitle_sid
    ));
    Some(url)
}

// ---- selection commits: playback POLICY for the in-player track menu. The menu only reports
// what row was picked; whether that means a native stream switch, a server re-transcode, or a
// burn refresh is decided HERE, next to the codec sets and the transcode state it depends on. ----

/// Commit an audio-track pick: NATIVE switch (feed the chosen stream from the same direct-play
/// file — no transcode, keeps 4K HEVC) when the item direct-plays AND the target codec is
/// direct-playable; else a server re-transcode with that stream selected. `idx` is the
/// CONTAINER audio ordinal (the menu converts its row via metadata::audio_ordinal).
pub(crate) fn commit_audio_selection(ps: &mut PlaybackSession, idx: i32, codec: &str, stream_id: i64) {
    if original_recovery_pending() {
        if let Some(pending) = PLAYER_CONTROL
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending_original
            .as_mut()
        {
            pending.deferred_audio = Some((idx, codec.to_owned(), stream_id));
        }
        crate::player::log("audio: deferred until pending Original handoff commits or rolls back");
        return;
    }
    // Audio always changes the decoder/server route contract, even when the eventual route stays
    // direct. Fence the old worker before publishing the selected stream or invalidating its
    // Original candidate; the queued reload below crosses its own action boundary afterwards.
    let _edit = begin_user_contract_boundary();
    // The recovery declaration captures one exact source/audio pairing. Once the user changes
    // that pairing while HLS is live, do not later resurrect the old track behind their back.
    // A new playback (or selecting Auto again from Original) can establish a fresh candidate.
    if matches!(
        cur_delivery(ps),
        crate::plex::TranscodeDelivery::FixedHls { .. }
    ) {
        ps.auto_original = None;
    }
    if !is_transcoding(ps) && crate::plex::is_dp_audio(codec) {
        // record the pick: the timeline then reports the stream that actually plays, and a
        // later transcode event (subtitle burn refresh / transcode seek) keeps this track
        { let s = &mut *ps; s.cur_audio_sid = stream_id };
        // persist the USER's pick server-side (official-client behavior): /status/sessions'
        // selected-stream display keys on the part selection, not the timeline report. Only
        // user picks persist — the start-of-play auto-pick (eng preference) reports only.
        put_selection(cur_sid(ps), cur_part_id(ps), cur_audio_sid(ps), cur_sub_sid(ps));
        crate::player::request_audio_track(ps, idx, codec);
    } else {
        { let s = &mut *ps; s.cur_audio_sid = stream_id };
        crate::player::request_audio_switch(ps, stream_id);
    }
}

/// Apply commands which were attached to one exact Original trial, after either the candidate or
/// its rollback Engine has really started. Consuming the value makes cross-trial leakage
/// impossible; dropping it on terminal start failure is the explicit cancellation edge.
pub(crate) fn apply_deferred_original_effects(ps: &mut PlaybackSession, mut effects: DeferredOriginalEffects) {
    if let Some(q) = effects.quality.take() {
        apply_quality_choice(ps, q);
    }
    if let Some((idx, codec, stream_id)) = effects.audio.take() {
        commit_audio_selection(ps, idx, &codec, stream_id);
    }
}

/// Commit a subtitle pick (`sub_idx` -1 = Off): gate the client-side renderer (direct-play path)
/// and select the burn stream for any transcode of the item — refreshing a live transcode so the
/// server re-burns (or drops) it.
pub(crate) fn commit_subtitle_selection(ps: &mut PlaybackSession, sub_idx: i32, stream_id: i64) {
    let transcoding = is_transcoding(ps);
    // Burned subtitles are part of the server/decoder contract, so revoke old worker evidence
    // before changing them. A direct-play subtitle is client-rendered and needs no reload; fencing
    // there would kill the valid Original watchdog while leaving the physical route untouched.
    let _edit = transcoding.then(begin_user_contract_boundary);
    // As with audio, a non-Off subtitle may require server burn-in and is not interchangeable
    // with the direct declaration captured at playback start. Off is always safe to carry back.
    if matches!(
        cur_delivery(ps),
        crate::plex::TranscodeDelivery::FixedHls { .. }
    ) {
        { let s = &mut *ps; {
            if stream_id == 0 {
                if let Some(candidate) = s.auto_original.as_mut() {
                    candidate.subtitle_ordinal = None;
                }
            } else {
                s.auto_original = None;
            }
        } };
    }
    crate::player::request_subtitle(sub_idx);
    set_subtitle(ps, stream_id);
    if transcoding {
        crate::player::request_transcode_refresh(ps); // retranscode PUTs the selection itself
    } else {
        // This is an immediate client-rendered change: unlike a burn/audio rebuild it is already
        // part of the applied stream contract. Publish projection + reporter tracks as one reducer
        // event so a later rejected action cannot restore the pre-subtitle snapshot.
        commit_in_place_route_projection(ps, false);
        // persist the pick server-side (and subs Off PUTs subtitleStreamID=0, clearing a
        // stale server-side selection that would otherwise burn on the next transcode)
        put_selection(cur_sid(ps), cur_part_id(ps), cur_audio_sid(ps), cur_sub_sid(ps));
    }
}

/// Atomically publish the main-thread session fields a newly spawned timeline reporter may read.
/// Called at the reporter spawn site, before ownership crosses to its worker. The active encoder
/// remains in `PlayerControl`, so a later in-place ABR commit changes the wire session and this
/// projection under one lock without touching main-thread-only `Session`.
pub(crate) fn begin_timeline_reporting(ps: &PlaybackSession) -> Option<TimelineLease> {
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
    state: crate::plex::TimelineState,
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
pub(crate) fn report_timeline(
    lease: &TimelineLease,
    state: crate::plex::TimelineState,
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
    let c = match crate::plex::client_for(report.sid) {
        Some(c) => c,
        None => return true,
    };
    let ok = c.timeline(&crate::plex::TimelineReport {
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
        crate::log(&format!(
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
