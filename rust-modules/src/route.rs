//! play_movie route selection (direct-play vs transcode) + the stream URL, transcode session, and
//! HUD strings. Main-thread projection state is held as ONE [`Session`] value; synchronized route
//! ownership and route-changing intents live in [`PLAYER_CONTROL`]. The player engine reads the
//! URL/session through the accessors here; ui::player_hud reads the HUD strings through
//! title_cptr()/ctxline_cptr().
use crate::plex::ServerId;
use crate::pms::PmsMovie;
use std::os::raw::c_char;
use std::ptr::{addr_of, addr_of_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::Mutex;

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RetryContext {
    resume_ns: i64,
    audio_sid: i64,
    sub_sid: i64,
}

/// Everything the main thread needs to resolve and render the playback in progress, in one struct.
///
/// Every field below was its own `static mut`, and the SHAPE was the hazard rather than any one of
/// them: [`apply_plan`] installed all of them but the two HUD buffers, [`request_play`] owned those
/// two and the five the outgoing item leaves behind, and a dozen small functions each poked one or
/// two more on the side — so "what a session IS" was written down nowhere and no writer could be
/// read against the whole. The failure that shape produced is still documented at the line that
/// fixed it, in [`build_stream`] — the part id was published by the CALLER after the resolve
/// returned, so `put_selection`, which runs INSIDE the resolve, addressed the PREVIOUS item's part,
/// and the server-default subtitle that PUT exists to suppress was burned into the transcode
/// instead.
///
/// Route ownership and worker/main route transitions are deliberately not fields here: the
/// synchronized [`PLAYER_CONTROL`] is their authority. **MAIN THREAD ONLY.** That is what keeps a
/// `static mut` sound here, and it is why
/// [`ResolveEnv`] exists: the resolve worker is handed owned copies and reads none of this. The
/// accessors that lend rather than copy — [`play_verdict`], [`up_next`] and [`with_queue`], plus
/// the raw pointers [`title_cptr`]/[`ctxline_cptr`] hand to `draw_text` — stay valid until the next
/// main-thread write, and that write is [`apply_plan`] or [`request_play`], neither of which can
/// run inside a frame's draw.
struct Session {
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
    /// same argument [`Session::cur_remux`] carries for the neighbouring question, and the reason
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
    auto_last_switch: Option<std::time::Instant>,
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
    /// The direct-played file's own Dolby Vision layering, carried for one consumer: the Load
    /// payload's `DolbyHdrInfo` node ([`crate::metadata::Dovi::presentation`]). Rides the session
    /// exactly like `stream_fps` and for the same reason — an audio-track switch tears the engine
    /// down and rebuilds the payload from here, and a payload that lost the node mid-film would
    /// put the rest of the picture up in the wrong colours.
    ///
    /// `Dovi::NONE` on every transcode and remux: what arrives then is the server's output, and
    /// the only DV file that reaches those paths is one we refused to declare in the first place.
    stream_dovi: crate::metadata::Dovi,
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
}

impl Session {
    /// Nothing playing: what the module holds before the first play, and the value the static is
    /// born as. Every String empty, every id 0 or `UNSET`, both HUD buffers NUL.
    const IDLE: Session = Session {
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
        stream_immersive: false,
        title: [0; 128],
        ctxline: [0; 96],
        up_next: None,
        queue: Vec::new(),
    };
}

static mut SESSION: Session = Session::IDLE;

/// Read the session. MAIN THREAD.
///
/// `&'static` because several accessors lend part of it out across a frame (see [`Session`]); the
/// borrow is only sound for as long as no main-thread write lands, which is the same rule those
/// accessors' own docs state and the same one that held while these were separate statics.
fn session() -> &'static Session {
    // `addr_of!`, never `&SESSION`: a shared reference to a `static mut` is the thing this module
    // has always routed around, and the raw pointer is what keeps that true for the whole struct.
    unsafe { &*addr_of!(SESSION) }
}

/// Write the session. MAIN THREAD.
///
/// Scoped to a closure so the `&mut` cannot outlive the statement that took it — `f` is
/// `FnOnce(&mut Session) -> R` with an elided (i.e. universally quantified) lifetime, so nothing
/// borrowed from the session can leave through `R` either. That is what keeps the within-thread
/// hazard narrow: an exclusive borrow alive while a [`session`] borrow still is. (The cross-thread
/// one is answered by MAIN THREAD, as it was for the statics this replaced.)
fn session_mut<R>(f: impl FnOnce(&mut Session) -> R) -> R {
    f(unsafe { &mut *addr_of_mut!(SESSION) })
}

/// Put the module back to [`Session::IDLE`] — the whole session at once, HUD buffers and the
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
fn reset_session() {
    session_mut(|s| *s = Session::IDLE);
}

// ---- accessors: the player reads the URL/session; the HUD reads the title/ctxline ----
// Their signatures and meanings are the module's whole public surface — `app.rs`, `player/` and
// `ui/` call them heavily — so collecting the state behind them changed the BODIES only.
pub(crate) fn url() -> String {
    session().url.clone()
}
/// Is there a stream URL at all? The in-place twin of [`url`], for the callers that only want the
/// emptiness — [`is_transcoding`]'s idiom, and for the same reason: a universal-transcode
/// `start.mkv` URL is several hundred bytes, and the player route is exempt from the idle present
/// gate, so a `!url().is_empty()` in a draw is a heap allocation and a memcpy at ~60/s.
pub(crate) fn has_url() -> bool {
    !session().url.is_empty()
}
/// Whole-file transport requirement captured by the playback resolve. Diagnostics uses it for
/// manual Original, where no adaptive controller exists to publish `dg_abr_kbps`.
pub(crate) fn transport_kbps() -> i64 {
    session().cur_transport_kbps
}
pub(crate) fn set_url(s: &str) {
    session_mut(|x| x.url = s.to_owned())
}
pub(crate) fn clear_url() {
    session_mut(|x| x.url.clear())
}
pub(crate) fn transcode_session() -> String {
    session().tsession.clone()
}

/// Thread-safe active PMS resource identity. While transcoding it names the coupled physical
/// encoder/Streaming Resource; while direct-playing it names the logical Streaming Resource only.
/// `Session::tsession` remains the main-thread playback classification bit, so owning a direct
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

struct ActiveEncoderState {
    /// Monotone semantic route generation. The PMS id may deliberately stay unchanged while the
    /// route changes from HLS to direct Original, so the id alone is not an ownership token.
    epoch: u64,
    id: String,
    hls: Option<ActiveHlsRoute>,
}

/// One worker's right to observe or replace the active route. Both fields are required: `encoder`
/// addresses PMS, while `epoch` distinguishes semantic routes which intentionally reuse that
/// exact Streaming Resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RouteLease {
    epoch: u64,
    encoder: String,
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
    route: RouteLease,
    engine_epoch: u64,
    media_epoch: u64,
    applied_revision: u64,
}

impl WorkerTicket {
    pub(crate) fn encoder(&self) -> &str {
        self.route.encoder()
    }
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

/// Identity of one physical `sf_load` attempt inside a prepared route transaction. Attempts are
/// never reused: a late result from A cannot settle retry B even though both open the same URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RouteStartAttempt {
    serial: u64,
    attempt: u64,
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
    stream_immersive: bool,
}

fn route_projection() -> AppliedRouteProjection {
    let s = session();
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
        stream_immersive: s.stream_immersive,
    }
}

fn install_route_projection(projection: &AppliedRouteProjection) {
    session_mut(|s| {
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
        s.stream_immersive = projection.stream_immersive;
    });
}

/// Publish the main-thread projection which now belongs to the physical route.  Capture before
/// taking `PLAYER_CONTROL`: session access is main-thread-only, while workers only need the owned
/// clone behind the mutex.
fn publish_applied_route_projection() {
    let projection = route_projection();
    PLAYER_CONTROL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .applied_projection = Some(projection);
}

/// Commit a contract change which requires no PMS/native route replacement.  This is still a
/// reducer event: otherwise a later rejected action restores the older snapshot and silently
/// undoes the already-visible quality/subtitle choice.
fn commit_in_place_route_projection(quality_contract: bool) {
    let projection = route_projection();
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
struct PlayerControl {
    active: ActiveEncoderState,
    engine_epoch: u64,
    media_epoch: u64,
    /// Latest user-visible contract edit. It fences automatic publication through pending/phase,
    /// but is not a worker credential.
    desired_revision: u64,
    /// Quality preference represented by `desired_revision`. The process-wide picker is only the
    /// durable user preference; it cannot also describe bytes PMS has not accepted yet.
    desired_quality: Quality,
    /// Revision represented by the physical route and therefore carried by WorkerTicket.
    applied_revision: u64,
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

fn next_route_epoch(epoch: u64) -> u64 {
    let next = epoch.wrapping_add(1);
    if next == 0 {
        1
    } else {
        next
    }
}

fn lease_of(active: &ActiveEncoderState) -> RouteLease {
    RouteLease {
        epoch: active.epoch,
        encoder: active.id.clone(),
    }
}

fn advance_route(active: &mut ActiveEncoderState) {
    active.epoch = next_route_epoch(active.epoch);
}

fn next_generation(value: u64) -> u64 {
    let next = value.wrapping_add(1);
    if next == 0 {
        1
    } else {
        next
    }
}

fn worker_ticket_of(control: &PlayerControl) -> WorkerTicket {
    WorkerTicket {
        route: lease_of(&control.active),
        engine_epoch: control.engine_epoch,
        media_epoch: control.media_epoch,
        applied_revision: control.applied_revision,
    }
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

fn ticket_is_current(control: &PlayerControl, ticket: &WorkerTicket) -> bool {
    ticket == &worker_ticket_of(control)
}

fn automatic_ticket(intent: &AutomaticRouteIntent) -> &WorkerTicket {
    match intent {
        AutomaticRouteIntent::OriginalToHls { ticket, .. }
        | AutomaticRouteIntent::HlsToOriginal { ticket, .. } => ticket,
    }
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
        // Stopping is observable only while the synchronous main-thread teardown owns the loop;
        // by the time another UI request can run there is no live Engine left to preserve.
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
fn prepare_playback_landing(playable: bool) -> Option<RouteStartTransaction> {
    let projection = playable.then(route_projection);
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
fn settle_plan_start_in_unit_test(start: Option<RouteStartTransaction>) {
    #[cfg(test)]
    if let Some(start) = start {
        if let Some(attempt) = claim_route_start_attempt(start) {
            let _ = settle_route_start(attempt, RouteStartResult::Started);
        }
    }
    #[cfg(not(test))]
    let _ = start;
}

/// Settle a resolve which will never produce a landing (cancelled or failed to spawn). Restore the
/// exact phase hidden by `Resolving`: URL presence cannot distinguish a live Stable route from a
/// failed candidate which merely retains its cleanup projection. A late worker cannot apply
/// because PLAY_GEN owns that separate mailbox.
fn cancel_playback_request(_playable: bool) {
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
        install_route_projection(&applied);
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
fn settle_failed_resolve_spawn() {
    cancel_playback_request(has_url());
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
pub(crate) fn request_user_route_intent(intent: UserRouteIntent) {
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
        quality() == Quality::Original && session().auto_original.is_some();
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
pub(crate) fn finish_route_action(action: &ClaimedRouteAction, result: RouteApplyResult) {
    // The effect runs on the main thread and has finished mutating Session before this call. Take
    // its complete value now; workers never touch Session, and the mutex below decides whether
    // this particular action is still allowed to publish it.
    let candidate_projection = (result == RouteApplyResult::Prepared).then(route_projection);
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
        install_route_projection(&applied);
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
        ControlPhase::Failed(_) => {
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
pub(crate) fn settle_route_start(ticket: RouteStartAttempt, result: RouteStartResult) -> bool {
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
        apply_deferred_original_effects(effects);
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
pub(crate) fn drain_route_start_results() {
    let results = {
        let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut control.start_results)
    };
    for (ticket, result) in results {
        let _ = settle_route_start(ticket, result);
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

#[cfg(test)]
pub(crate) fn pending_user_route_intent(intent: UserRouteIntent) -> bool {
    PLAYER_CONTROL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pending_user
        == Some(intent)
}

#[cfg(test)]
pub(crate) fn reset_player_control_for_test() {
    let projection = route_projection();
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
static ENCODER_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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

fn next_encoder_generation() -> u64 {
    ENCODER_GENERATION.fetch_add(1, Ordering::Relaxed) + 1
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

#[cfg(test)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(test)]
pub(crate) enum ActiveEncoderRefusal {
    RouteMoved,
}

/// Run one bounded publication while ACTIVE still names `expected`.
///
/// Production enters this under the AU queue mutex, fixing the global order at AQ -> ACTIVE. The
/// callback executes before ACTIVE is released; a check which returned `bool` and published later
/// would reopen a gap for seek/retranscode to retire this worker between those two operations.
#[cfg(test)]
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

#[cfg(test)]
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
fn sync_active_hls_to_session() -> Option<(WorkerTicket, ActiveHlsRoute)> {
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
            .unwrap_or_else(route_projection);
        applied.url = hls.url.clone();
        applied.tsession = ticket.encoder().to_owned();
        applied.ceiling = Some(hls.rung.ceiling());
        applied.remux = false;
        control.applied_projection = Some(applied.clone());
        (ticket, hls)
    };
    session_mut(|s| {
        s.url = active.1.url.clone();
        s.tsession = active.0.encoder().to_owned();
        // An adaptive commit changes the encoder, URL and requested rung, not the delivery
        // contract. Preserve the route's negotiated segment duration instead of fabricating one
        // here: seek/reload must carry the exact server contract that created this worker.
        s.cur_ceiling = Some(active.1.rung.ceiling());
        s.cur_remux = false;
    });
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

/// Everything needed to restore Auto's zero-video-encode state after HLS. `url` is the cold-start
/// playback target; `probe_part` is the raw Part key used to bind runtime measurement and direct
/// playback to the exact live HLS Streaming Resource. `direct` says whether the Part itself is
/// playable or whether PMS must container-remux it while copying the video.
#[derive(Clone)]
struct AutoOriginalCandidate {
    url: String,
    probe_part: String,
    direct: bool,
    vcodec: String,
    acodec: String,
    fps: f64,
    dovi: crate::metadata::Dovi,
    immersive: bool,
    audio_sid: i64,
    audio_ordinal: Option<i32>,
    subtitle_ordinal: Option<i32>,
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

fn source_probe_sample_outcome(
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

/// Why the final candidate ownership transaction did not publish. No arm performs cleanup: the
/// caller still owns the candidate on every refusal and must retire it outside AQ/ACTIVE locks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsCommitRefusal {
    Session,
    RouteMoved,
    TransitionRejected,
}

fn classify_prime_decision(
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
    #[cfg(test)]
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
fn auto_catalog() -> crate::abr::HlsActuatorCatalog {
    let caps = crate::devcaps::caps();
    let device = (
        u16::try_from(caps.hevc_max.0).unwrap_or(u16::MAX),
        u16::try_from(caps.hevc_max.1).unwrap_or(u16::MAX),
    );
    let (_, width, height) = session().cur_src;
    let source = (
        u16::try_from(width).unwrap_or(u16::MAX),
        u16::try_from(height).unwrap_or(u16::MAX),
    );
    crate::abr::HlsActuatorCatalog::measured().limited_to(device, source)
}

/// Visible switches spent so far, aged. Read on the main thread at worker spawn; the worker
/// advances it with its own clock from there.
fn auto_history() -> crate::abr::TransitionHistory {
    let s = session();
    crate::abr::TransitionHistory {
        visible_switches: s.auto_switches,
        since_last_ms: s
            .auto_last_switch
            .map(|at| u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX)),
    }
}

/// Record that the viewer just saw a mode change. Called by BOTH halves of the transaction, which
/// is the point: a fallback and a recovery are equally visible, and it is their ALTERNATION that
/// the penalty exists to price.
fn note_visible_switch() {
    session_mut(|s| {
        s.auto_switches = s.auto_switches.saturating_add(1);
        s.auto_last_switch = Some(std::time::Instant::now());
    });
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
fn auto_prior() -> Option<crate::abr::CapacityEstimate> {
    let carried = crate::player::SHARED.abr_seed();
    carried.or_else(|| {
        let kbps = session().auto_prior_kbps;
        (kbps > 0).then(|| crate::abr::CapacityEstimate::from_prior(kbps))
    })
}

/// Does this playback's source carry something a transcode cannot give back? Dolby Vision and
/// Atmos are the two that matter here, and both are recorded on the Original candidate rather than
/// inferred from the stream now playing (which, mid-HLS, is a re-encode of them).
fn auto_original_features() -> crate::abr::SourceFeatures {
    session()
        .auto_original
        .as_ref()
        .map(|candidate| crate::abr::SourceFeatures {
            dv: candidate.dovi.profile > 0,
            atmos: candidate.immersive,
        })
        .unwrap_or_default()
}

/// Main-thread capture immediately before spawning the HLS demux worker.
pub(crate) fn hls_abr_control() -> Option<(HlsAbrControl, WorkerTicket)> {
    let seconds_per_segment = match cur_delivery() {
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
        .then(|| session().auto_original.as_ref())
        .flatten();
    Some((
        HlsAbrControl {
            trace_generation: playback_trace_generation(),
            sid: cur_sid(),
            rating_key: cur_rk(),
            logical_session: sess(),
            audio_stream_id: cur_audio_sid(),
            subtitle_stream_id: cur_sub_sid(),
            seconds_per_segment,
            initial_rung: live
                .as_ref()
                .map(|(_, hls)| hls.rung)
                .or_else(|| cur_ceiling().and_then(crate::abr::Rung::from_ceiling))
                .unwrap_or(crate::abr::Rung::P480),
            initial_observed: live.as_ref().and_then(|(_, hls)| hls.observed),
            fixture_base: session().auto_fixture_base.clone(),
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
                    let s = session();
                    u32::try_from(s.cur_transport_kbps)
                        .ok()
                        .filter(|&kbps| kbps > 0)
                        .or_else(|| u32::try_from(s.cur_src.0).ok().filter(|&kbps| kbps > 0))
                })
                .unwrap_or(0),
            catalog: auto_catalog(),
            prior: auto_prior(),
            history: auto_history(),
            original_features: auto_original_features(),
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

pub(crate) fn auto_original_watch() -> Option<AutoOriginalWatch> {
    let s = session();
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
        catalog: auto_catalog(),
        history: auto_history(),
        features: auto_original_features(),
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
    original_url: &str,
    source_kbps: u32,
    hls_base: &str,
    start_hls: bool,
    source_raster: (u16, u16),
) -> Option<String> {
    session_mut(|s| {
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
            immersive: false,
            audio_sid: 0,
            audio_ordinal: None,
            subtitle_ordinal: None,
        });
        s.auto_fixture_base = hls_base.trim_end_matches('/').to_owned();
    });
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
        settle_plan_start_in_unit_test(prepare_playback_landing(true));
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
    session_mut(|s| {
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
    });
    install_active_hls(&encoder, &url, rung);
    crate::player::log(&format!(
        "auto fixture: starting in {}kbps {}x{} HLS (no Original phase)",
        rung.kbps(),
        rung.raster().0,
        rung.raster().1,
    ));
    settle_plan_start_in_unit_test(prepare_playback_landing(true));
    Some(url)
}

/// Main-thread half of the progressive watchdog transaction. The demux worker has stopped at a
/// packet boundary and published its CONSERVATIVE delivery estimate — not the last window's raw
/// rate, which is one sample of a distribution; atomically move the route to the best HLS state
/// that estimate sustains, then build the replacement encoder at the current movie position. The
/// caller performs the fresh Starfish Load only when this returns a URL.
#[cfg(test)]
pub(crate) fn fallback_auto_to_hls(measured_kbps: u32, offset_secs: i64) -> Option<String> {
    let expected = worker_ticket();
    fallback_auto_to_hls_for(&expected, measured_kbps, offset_secs)
}

pub(crate) fn fallback_auto_to_hls_for(
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
    if applied_quality() != Quality::Auto || cur_rk().is_empty() {
        return None;
    }
    let rung = crate::abr::original_fallback_rung(
        measured_kbps,
        &auto_catalog(),
        &crate::abr::AbrPolicy::measured(),
    );
    session_mut(|s| s.auto_prior_kbps = measured_kbps);
    crate::player::log(&format!(
        "auto: Original became unsustainable at {measured_kbps}kbps; switching to {}kbps {}x{} HLS",
        rung.kbps(),
        rung.raster().0,
        rung.raster().1,
    ));
    install_auto_hls(
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
pub(crate) fn fallback_unopened_auto_to_hls(offset_secs: i64) -> Option<String> {
    let expected = worker_ticket();
    let watch = auto_original_watch()?;
    if cur_rk().is_empty() {
        return None;
    }
    let bootstrap_rung = session().auto_bootstrap_rung;
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
    expected: &WorkerTicket,
    rung: crate::abr::Rung,
    offset_secs: i64,
    visible_switch: bool,
    reason: crate::player::report::DeliveryReason,
) -> Option<String> {
    let fixture_base = session().auto_fixture_base.clone();
    let previous = {
        let s = session();
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
            s.stream_immersive,
        )
    };
    let restore = || {
        session_mut(|s| {
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
            s.stream_immersive = previous.10;
        });
    };
    session_mut(|s| {
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
        s.stream_dovi = crate::metadata::Dovi::NONE;
        s.stream_immersive = false;
    });
    let finish = |url: String| {
        if visible_switch {
            note_visible_switch();
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
            restore();
            return None;
        }
        session_mut(|s| {
            s.url = url.clone();
            s.tsession = encoder.clone();
        });
        return finish(url);
    }
    // Counted only on the paths that really produce a replacement URL. A switch that failed to
    // build is not one the viewer saw, and the anti-flapping penalty prices what they SAW — the
    // pump turns a `None` here into a playback error, not into a mode change.
    match retranscode_for(expected, offset_secs) {
        Some(url) => finish(url),
        None => {
            restore();
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
pub(crate) fn recover_auto_to_original(offset_secs: i64) -> Option<AutoOriginalReload> {
    let expected = worker_ticket();
    recover_auto_to_original_for(&expected, offset_secs, quality() == Quality::Auto)
}

pub(crate) fn recover_auto_to_original_for(
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
    if !contract_allows || !is_transcoding() {
        return None;
    }
    let candidate = session().auto_original.clone()?;
    // The worker may have committed several HLS encoders since the main-thread plan was installed.
    // Snapshot the physical route before replacing it, otherwise the rollback pairs the newest
    // encoder id with the bootstrap URL/rung and reopens different media at a different position.
    if !is_worker_ticket_current(expected) {
        return None;
    }
    let live_hls = sync_active_hls_to_session();
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
    let mut rollback = snapshot_route(expected_encoder.clone(), offset_secs);
    if candidate.direct {
        // The probe and the actual Part body must name the same exact Streaming Resource. A URL
        // left on the logical playback id can token-alias this HLS resource today, then fail a
        // later seek after cleanup because the alias choice is not durable.
        let source_url = if candidate.probe_part.starts_with('/') {
            let client = cur_client()?;
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
        session_mut(|s| {
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
            s.stream_immersive = candidate.immersive;
        });
        crate::player::set_audio_track(candidate.audio_ordinal.unwrap_or(-1));
        crate::player::request_subtitle(candidate.subtitle_ordinal.unwrap_or(-1));
        set_pending_original(rollback, automatic);
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
    let replacement = prepare_original_remux(&candidate, expected, offset_secs, automatic)?;
    rollback.replacement_encoder = replacement;
    set_pending_original(rollback, automatic);
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

fn snapshot_route(encoder: String, offset_secs: i64) -> PendingOriginal {
    let s = session();
    PendingOriginal {
        previous_applied_revision: 0,
        previous_applied_quality: Quality::Original,
        previous_applied_projection: None,
        // Replaced atomically by `set_pending_original` after the candidate route is installed.
        candidate_projection: route_projection(),
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
        stream_immersive: s.stream_immersive,
        adopted_by_user: false,
        charge_visible_switch_on_commit: false,
        deferred_quality: None,
        deferred_audio: None,
    }
}

/// Install the way back. A displaced one is RETIRED rather than dropped: its encoder is still
/// running on somebody's server, and the route it belonged to is two recoveries stale.
fn set_pending_original(mut pending: PendingOriginal, automatic: bool) {
    let candidate_projection = route_projection();
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
        retire_replaced_encoder(old.encoder);
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
pub(crate) fn confirm_original_recovery() {
    let current_projection = route_projection();
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
        note_visible_switch();
    }
    if pending.replacement_encoder.is_empty() {
        crate::player::log(
            "abr: direct Original confirmed by decoded frames; stopping HLS encoder and retaining source resource",
        );
        retire_hls_encoder_keep_source(pending.encoder);
    } else {
        crate::player::log(
            "abr: remux Original confirmed by decoded frames; retiring old HLS resource",
        );
        retire_replaced_encoder(pending.encoder);
    }
    apply_deferred_original_effects(deferred);
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
pub(crate) fn rollback_original_recovery() -> Option<OriginalRollback> {
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
    session_mut(|s| {
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
        s.stream_immersive = pending.stream_immersive;
    });
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
        retire_replaced_encoder(failed_replacement);
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
pub(crate) fn drop_original_recovery() {
    if let Some(pending) = take_pending_original() {
        // The only caller is real teardown, immediately after `scrobble_stop` took the active
        // identity. During a direct handoff that identity IS `pending.encoder`, so scrobble owns
        // its one full stop/resource close. During a remux handoff the active identity is the new
        // replacement; scrobble closes that one and this branch still owes the held old HLS.
        if !pending.replacement_encoder.is_empty() {
            retire_replaced_encoder(pending.encoder);
        }
    }
}

fn retire_replaced_encoder(encoder: String) {
    if encoder.is_empty() || !session().auto_fixture_base.is_empty() {
        return;
    }
    let Some(client) = cur_client() else { return };
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
fn retire_hls_encoder_keep_source(encoder: String) {
    if encoder.is_empty() || !session().auto_fixture_base.is_empty() {
        return;
    }
    let Some(client) = cur_client() else { return };
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
pub(crate) fn is_transcoding() -> bool {
    !session().tsession.is_empty()
}
/// Did the server REFUSE this item at `/decision`, before playback? Cheap in-place check —
/// `player::state()` derives `Error` from it on every frame of the player route.
pub(crate) fn play_refused() -> bool {
    session().play_verdict.is_some()
}
/// The app/worker failed to produce a playable plan, as distinct from a PMS `/decision` refusal.
/// There is no Engine whose pump could publish Error, so `player::state()` derives it beside the
/// refusal case.
pub(crate) fn play_resolution_failed() -> bool {
    session().resolve_failed
}
/// The refusal's own sentence for the read-out to quote — `None` when the server did not refuse,
/// `Some("")` when it refused without saying why. MAIN THREAD (see [`Session::play_verdict`]).
///
/// Borrowed, not cloned: the read-out asks for this 2–3× on every frame of a failure (the HUD
/// caption, the read-out itself, and the diagnostics panel when it is open), and every one of them
/// only reads it. The borrow lives until the next main-thread write, which is `apply_plan` or
/// `request_play` — neither of which can run inside a frame's draw.
pub(crate) fn play_verdict() -> Option<&'static str> {
    session().play_verdict.as_deref()
}
/// Retire the refusal — "this playback request is withdrawn", the one thing besides a fresh
/// resolve that ends a verdict's life. [`request_play`] clears it because a NEW item is being
/// resolved; this is the other half, for leaving the player entirely.
///
/// Without it a refusal outlived the player: `player::state()` derives `Error` from this field
/// and takes no route, so a verdict left standing described the item the user walked away from —
/// on Home, in the Library, on any detail page — until they happened to start something else.
fn clear_play_verdict() {
    session_mut(|s| {
        s.play_verdict = None;
        s.resolve_failed = false;
        s.requested_resume_ns = 0;
    })
}
/// select the subtitle to BURN into any transcode of the current item (0 = none). This
/// is the transcode path; direct-play uses the client renderer (player::request_subtitle).
pub(crate) fn set_subtitle(sid: i64) {
    session_mut(|s| s.cur_sub_sid = sid)
}
/// the subtitle stream id currently burned into the transcode (0 = none).
pub(crate) fn cur_sub_sid() -> i64 {
    session().cur_sub_sid
}
/// ratingKey of the currently-playing item (for /:/timeline progress reports).
pub(crate) fn cur_rk() -> String {
    session().cur_rk.clone()
}
/// The server the currently-playing item came from — see [`Session::cur_sid`]. MAIN THREAD.
///
/// A worker must be handed this by value at its spawn site, never call it: read on a worker it is
/// "whatever is playing now", which is the very race capturing the id was meant to end.
pub(crate) fn cur_sid() -> ServerId {
    session().cur_sid
}
/// Test-only: install the playing item's server directly, returning the previous value to restore.
///
/// In production [`Session::cur_sid`] has exactly one writer — `apply_plan` — and a `Plan` cannot
/// be built outside this module, so a suite elsewhere that needs "this is playing from the share"
/// sets it through here rather than widening `Plan` for a test. `player::playing_subscription` is
/// the reader that needs it: the failure read-out's Plex Pass claim is about the server the failing
/// item came from, and there is no other way to say which that is. Callers must hold
/// `crate::testlock::serial()` and put the previous value back: this is a crate global.
#[cfg(test)]
pub(crate) fn swap_cur_sid_for_test(sid: ServerId) -> ServerId {
    session_mut(|s| std::mem::replace(&mut s.cur_sid, sid))
}
/// The `Client` for the currently-playing item's server, `None` before the first play (or after a
/// plan that never resolved). The main-thread twin of `client_for(env.sid)` on the resolve worker
/// — every in-playback PMS call in this file goes through one of the two, and none through
/// `client_opt()`, which answers with whatever server is CURRENT rather than the one playing.
fn cur_client() -> Option<&'static crate::plex::Client> {
    crate::plex::client_for(cur_sid())
}
pub(crate) fn cur_audio_sid() -> i64 {
    session().cur_audio_sid
}
/// The currently-playing item's Part id. Written once per item by `build_stream` from its own
/// `part` argument. In-playback callers (audio switch, subtitle toggle, retranscode) want this;
/// `build_stream` must pass its freshly-derived local instead, since this is not yet updated
/// for the item being started.
fn cur_part_id() -> i64 {
    session().cur_part_id
}
/// The stable app-owned playback generation (and the first encoder's PMS session id).
pub(crate) fn sess() -> String {
    session().sess.clone()
}
pub(crate) fn pq_id() -> String {
    session().pq_id.clone()
}
pub(crate) fn pq_item_id() -> String {
    session().pq_item_id.clone()
}
/// The streamed item's Media video/audio codec, so the player picks the H265 Load payload for a
/// native HEVC direct-play and the matching audio codec.
pub(crate) fn stream_vcodec() -> String {
    session().stream_vcodec.clone()
}
pub(crate) fn stream_acodec() -> String {
    session().stream_acodec.clone()
}
/// direct-play source video fps for the Load esInfo (0 = unknown/transcode → omit)
pub(crate) fn stream_fps() -> f64 {
    session().stream_fps
}
/// The direct-played file's Dolby Vision layering, for the Load payload's `DolbyHdrInfo` node.
/// `Dovi::NONE` for anything the server is transcoding or remuxing, and for a DV file we refused
/// to declare — in every one of those cases the payload must say nothing.
pub(crate) fn stream_dovi() -> crate::metadata::Dovi {
    let s = session();
    if s.stream_vcodec.eq_ignore_ascii_case("hevc") {
        s.stream_dovi
    } else {
        // Last-line consistency guard for dev declarations and future route mutations: the LG
        // payload cannot truthfully describe Dolby Vision on a non-HEVC elementary stream.
        crate::metadata::Dovi::NONE
    }
}
/// Is the audio being fed a Dolby Atmos stream? — the Load payload's `contents.immersive` node.
/// See [`Session::stream_immersive`].
pub(crate) fn stream_immersive() -> bool {
    let s = session();
    // This pipeline's Atmos path is E-AC3 JOC.  AAC/AC3 are ordinary output even if a stale
    // source flag exists, so neither diagnostics nor Load may repeat that source-only claim.
    s.stream_acodec.eq_ignore_ascii_case("eac3") && s.stream_immersive
}
/// Override the audio codec used to build the Load payload — set by a native audio-track
/// switch to the chosen track's codec before the direct-play reload.
pub(crate) fn set_stream_acodec(codec: &str) {
    session_mut(|s| s.stream_acodec = codec.to_owned())
}
/// Record the streamed item's video+audio codec pair in one write (the Load-payload source of
/// truth) for route-policy tests that install a synthetic live HLS response.
#[cfg(test)]
pub(crate) fn set_stream_codecs(vc: &str, ac: &str) {
    session_mut(|s| {
        s.stream_vcodec = vc.to_owned();
        s.stream_acodec = ac.to_owned();
    })
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
    vc: &str,
    ac: &str,
    fps: f64,
    dovi: crate::metadata::Dovi,
    immersive: bool,
) {
    session_mut(|s| {
        s.stream_vcodec = vc.to_owned();
        s.stream_acodec = ac.to_owned();
        s.stream_fps = fps;
        s.stream_dovi = dovi;
        s.stream_immersive = immersive;
    })
}

// (`set_source_codecs` stood here: a two-line setter for `src_vcodec`/`src_acodec` whose one
// caller was `apply_plan`, which now installs them as part of its single assignment. The rule it
// carried survives on [`Session::src_vcodec`] itself — those two are the FILE's codecs and
// `apply_decision_codecs`, which overwrites the stream pair with the transcode's output, must
// never touch them.)

/// Was this playback's transcode a container-only REMUX (codecs copied) rather than a re-encode?
/// Meaningless unless `is_transcoding()`. The diagnostics read-out's three-way Source row turns on
/// it: "the server touched the pixels" and "the server repackaged the bytes" are different facts
/// and only one of them can explain a decode problem.
pub(crate) fn is_remux() -> bool {
    session().cur_remux
}
/// Did this playback forbid the server a video stream COPY? Read by the seek and audio-switch
/// rebuilds so the constraint survives them — see [`Session::cur_no_video_copy`].
fn is_no_video_copy() -> bool {
    session().cur_no_video_copy
}
/// The quality ceiling THIS playback was resolved under — read by the two query rebuilds
/// ([`transcode_seek`], [`retranscode`]) so a rung picked mid-film cannot reshape the encode
/// already on screen. See [`Session::cur_ceiling`].
fn cur_ceiling() -> Option<crate::plex::Ceiling> {
    session().cur_ceiling
}
fn cur_delivery() -> crate::plex::TranscodeDelivery {
    session().cur_delivery
}

/// Whether the live route is the segmented HLS transport. The player uses this at the Starfish
/// Load boundary: HLS must prime both elementary-stream lanes before starting the audio-master
/// clock, even on an ordinary play-from-zero where no seek rebase is pending.
pub(crate) fn is_segmented_hls() -> bool {
    matches!(
        cur_delivery(),
        crate::plex::TranscodeDelivery::FixedHls { .. }
    )
}
pub(crate) fn source_vcodec() -> String {
    session().src_vcodec.clone()
}

/// **Can the pixels of this playback's source reach the panel untouched?** See
/// [`Session::cur_source_decodable`]. `false` is the state in which the quality ladder's
/// "Original" row is a promise the pipeline cannot keep.
///
/// Deliberately NOT `is_transcoding()`: that says what is happening now, and a fixed rung makes it
/// true of any source. This says what is POSSIBLE, which is the question a picker is asked.
pub(crate) fn source_decodable() -> bool {
    session().cur_source_decodable
}
pub(crate) fn source_acodec() -> String {
    session().src_acodec.clone()
}
/// pointers into the module-owned HUD buffers (valid for the whole frame draw_text uses them)
pub(crate) fn title_cptr() -> *const c_char {
    session().title.as_ptr()
}
pub(crate) fn ctxline_cptr() -> *const c_char {
    session().ctxline.as_ptr()
}
/// This playback's universal-transcoder spec, rebuilt from the module state (rk + session are
/// borrowed from the caller's locals; audio/subtitle ride the CURRENT selection) — so every
/// (re)start of the item's transcode carries identical params.
///
/// `ceiling` is an ARGUMENT rather than a read of [`quality`], for the same reason `remux` and
/// `no_video_copy` are: [`build_stream`] runs on the resolve worker and must take it from
/// [`ResolveEnv`], while [`retranscode`] runs on the main thread and reads the live selection. A
/// read inside here would be a `static` touched from a worker.
fn transcode_spec<'a>(
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
    final_report: Option<(String, i64, i64)>,
    report_th: Option<std::thread::JoinHandle<()>>,
) {
    let (logical_session, pq, pqi) = (sess(), pq_id(), pq_item_id());
    let (aud, sub) = (cur_audio_sid(), cur_sub_sid()); // the selection this playback reported under
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
    session_mut(|s| {
        s.tsession.clear();
        s.cur_remux = false;
    });
    if final_report.is_none() && tsession.is_empty() && report_th.is_none() {
        return; // nothing to post and nobody to wait for
    }
    // The server this playback came FROM, not whichever one is current — the resume point and the
    // transcode session both live there, and by the time a stop runs the user may well have walked
    // back to a different source's Home.
    let client = cur_client();
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
pub(crate) fn transcode_seek(offset_secs: i64) -> Option<String> {
    if transcode_session().is_empty() {
        return None;
    }
    let rk = cur_rk();
    if rk.is_empty() {
        return None;
    }
    let c = cur_client()?;
    // A plain seek/foreground resume has no claimed RouteAction, but it still replaces the PMS
    // route and native Engine. Reserve the same start transaction before exposing any candidate
    // fields; an action already in Applying owns its own later Prepared edge and returns None here.
    let route_start = begin_route_start();
    let reject_preparation = || {
        if let Some(ticket) = route_start {
            let _ = reject_route_start_preparation(ticket);
        }
    };
    let live_hls = sync_active_hls_to_session();
    let expected = live_hls
        .as_ref()
        .map(|(ticket, _)| ticket.clone())
        .unwrap_or_else(worker_ticket);
    let previous = expected.encoder().to_owned();
    if previous.is_empty() {
        reject_preparation();
        return None;
    }
    let logical_session = sess();
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
        is_remux(),
        is_no_video_copy(),
        crate::plex::TranscodeOffset::from_seconds(offset_secs.max(0)),
        cur_audio_sid(),
        cur_sub_sid(),
        cur_ceiling(),
        cur_delivery(),
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
    session_mut(|s| {
        s.tsession = replacement.clone();
        s.url = url.clone();
        if let Some((_, hls)) = live_hls.as_ref() {
            s.cur_ceiling = Some(hls.rung.ceiling());
        }
    });
    publish_applied_route_projection();
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

fn quality_ladder_for(auto_ready: bool) -> &'static [Quality] {
    if auto_ready {
        &QUALITY_LADDER
    } else {
        &QUALITY_LADDER[1..]
    }
}

/// What the menu may offer in this build. Original and fixed ceilings are established playback
/// paths; Auto joins them only when [`auto_quality_ready`] says the adaptive path is complete.
pub(crate) fn available_quality_ladder() -> &'static [Quality] {
    quality_ladder_for(auto_quality_ready())
}

fn supported_quality(q: Quality) -> Quality {
    if q == Quality::Auto && !auto_quality_ready() {
        Quality::Original
    } else {
        q
    }
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
/// the numbers that resolve measured ([`Session::cur_src`]) — not a blanket reload:
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
/// or changes a rung on its own. `Session::cur_ceiling`'s doc has the other half — a SEEK still
/// rebuilds from the stored ceiling, so only an explicit pick can move it mid-film.
fn persist_quality_choice(q: Quality) -> Quality {
    let q = supported_quality(q);
    crate::player::report::note_quality_selected_for(playback_trace_generation(), q);
    QUALITY.store(q.index(), Ordering::Relaxed);
    // A session write is a read-modify-write under the session lock: changing this preference
    // must not overwrite a roster refresh, a profile switch, or another profile's recents.
    let _ = crate::plex::session::update(|s| {
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

pub(crate) fn set_quality(q: Quality) {
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
            session_mut(|s| s.cur_auto_original_watched = false);
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
    apply_quality_choice(q);
}

fn apply_quality_choice(q: Quality) {
    // A later non-Auto pick supersedes an Auto restart that the pump has not consumed yet. If the
    // live worker really was adaptive, the route comparison below schedules the symmetric restart
    // which removes its watchdog; if it was still Manual, this cancellation avoids a stale Auto
    // reload after the checkmark has already moved back.
    if q != Quality::Auto {
        crate::player::cancel_adaptive_reload();
    }
    // A worker-side ABR commit is the current route. Reconcile it before comparing or replacing
    // anything, so a menu action cannot make a decision against the bootstrap ceiling.
    let live_hls = sync_active_hls_to_session();
    // An exact direct/remux candidate survives both fixed-rung and HLS transitions. An explicit
    // Original pick is an instruction to restore that native declaration now, not merely remove a
    // bitrate cap and start another encoder. Leave the current route intact until the pump owns
    // the same-position codec-changing reload.
    if q == Quality::Original && is_transcoding() && session().auto_original.is_some() {
        crate::player::log("quality: Original picked — restoring the native source");
        crate::player::request_original_recovery();
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
        crate::player::request_adaptive_reload();
        return;
    }
    let location = crate::plex::client_for(cur_sid()).and_then(|client| client.link());
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
    let original_feasible = session().auto_original.is_some();
    let auto_original = q == Quality::Auto
        && original_feasible
        && (location == Some(crate::plex::probe::Location::Local)
            || (location == Some(crate::plex::probe::Location::Remote)
                && (!is_transcoding() || is_remux())));
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
            cur_ceiling().and_then(crate::abr::Rung::from_ceiling),
            auto_prior(),
            &auto_catalog(),
            &crate::abr::AbrPolicy::measured(),
        )
    });
    let ceiling = starting_rung
        .map(crate::abr::Rung::ceiling)
        .or_else(|| q.ceiling());
    if cur_rk().is_empty() {
        return;
    }
    let route_unchanged = cur_ceiling() == ceiling && cur_delivery() == delivery;
    let watched_before = session().cur_auto_original_watched;
    let (kbps, w, h) = session().cur_src;
    let admits = quality_policy(q, auto_original, kbps, w, h).direct_play;
    session_mut(|s| {
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
    });
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
        crate::player::request_adaptive_reload();
        return;
    }
    if route_unchanged {
        commit_in_place_route_projection(true);
        return;
    }
    if admits && !is_transcoding() {
        // The bytes already satisfy the new ceiling, but the demux worker captured adaptive
        // supervision by value. Auto <-> Manual therefore still needs a same-URL worker restart;
        // otherwise the old Auto watchdog can publish a fallback after the picker says Manual.
        if watched_before != auto_original {
            crate::player::log(&format!(
                "quality: {} picked — keeping direct bytes and refreshing adaptive supervision",
                q.label(),
            ));
            crate::player::request_adaptive_reload();
        } else {
            commit_in_place_route_projection(true);
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
    crate::player::request_transcode_refresh();
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
fn quality_policy(
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

fn auto_uses_hls(q: Quality, auto_original: bool) -> bool {
    q == Quality::Auto && !auto_original
}

/// The shared source plan owns both the finite object and its conservation deadline. Keep this
/// narrow wrapper for the route tests and for converting Plex's signed bitrate into ABR units.
fn remote_probe_plan(source_kbps: i64) -> Option<crate::abr::SourceProbePlan> {
    crate::abr::source_probe_plan(
        u32::try_from(source_kbps).ok()?,
        crate::abr::PROBE_BUDGET_MS,
    )
}

#[cfg(test)]
fn remote_probe_target_bytes(source_kbps: i64) -> Option<usize> {
    remote_probe_plan(source_kbps).map(|plan| plan.target_bytes)
}

/// **One bounded measurement of the actual file, as an observation and nothing more.** It reports
/// bytes, active duration and whether the target was reached, because all three decide how much
/// the measurement is worth: a 40 KiB read that finished instantly honestly reports a huge rate
/// and proves nothing. What it does NOT do is decide anything — [`crate::abr::bootstrap`] owns the
/// admission rule, so the policy is stated once and is host-testable without a network.
///
/// `None` means there is nothing to reason from (no source bitrate, or the transfer never
/// returned), which is deliberately distinct from a completed slow probe.
fn measure_remote_original(
    client: &crate::plex::Client,
    part_key: &str,
    logical_session: &str,
    source_kbps: i64,
) -> Option<crate::abr::CapacityObservation> {
    let Some(plan) = remote_probe_plan(source_kbps) else {
        crate::player::log(
            "auto: remote Original unavailable — source bitrate is unknown; using HLS",
        );
        return None;
    };
    // Use the playback's own identity. If the bounded GET establishes a Streaming Resource, the
    // winning route — Original or the initial HLS encoder — exact-reuses it. A throwaway
    // `source-N` forces an exact miss and makes PMS run a second AdHoc admission decision whose
    // 500 says nothing about transport capacity.
    let url = client.direct_play_url(part_key, logical_session).to_url();
    let sample = match crate::curlio::sample_throughput_result(
        &url,
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

/// **Two ceilings mean the stricter one**, per flavor, and this is the only place the two are put
/// together. A ceiling can only ever REMOVE a flavor: a fast link cannot restore what a low rung
/// denied, and a high rung cannot restore what a relay denied.
///
/// A named function rather than two `&&`s inline at the decision site, so the composition the
/// tests grade is literally the composition [`build_stream`] runs — a re-implementation in a test
/// would agree with itself forever while the shipped path drifted.
fn flavors_allowed(
    link: crate::plex::LinkPolicy,
    quality: crate::plex::LinkPolicy,
) -> crate::plex::LinkPolicy {
    crate::plex::LinkPolicy {
        direct_play: link.direct_play && quality.direct_play,
        remux: link.remux && quality.remux,
    }
}

/// Ask PMS whether `rk` should direct-play (Some(true) → serve the raw Part) or transcode
/// (Some(false) → start.mkv). None when the server returns no usable Media decision, so the
/// caller falls back to the local codec test. Registers the session as a side effect.
///
/// Takes the `Client` rather than looking one up: this runs on the resolve worker, and `rk` is only
/// an item on the server the caller resolved from this playback's captured `ServerId`.
fn server_decision(c: &crate::plex::Client, rk: &str, session: &str) -> Option<bool> {
    let mc = match c.mde_decision(rk, session) {
        Some(mc) => mc,
        None => {
            // failed fetch OR unparseable (XML/truncated) body — keep the fallback visible
            // in the event log, like the old raw-body scan did
            crate::player::log("decision: no/unparseable response -> local heuristic");
            return None;
        }
    };
    // Part.decision is the authoritative verdict (Media/container carry none)
    let part = match mc
        .metadata
        .first()
        .and_then(|m| m.media.first())
        .and_then(|md| md.part.first())
    {
        Some(p) => p,
        None => {
            crate::player::log(&format!(
                "decision: no media (general={:?}) -> local heuristic",
                mc.general_decision_code
            ));
            return None;
        }
    };
    let direct = part.decision == "directplay";
    crate::player::log(&format!(
        "decision: part={} general={:?} mde={:?} -> {}",
        part.decision,
        mc.general_decision_code,
        mc.mde_decision_code,
        if direct { "DIRECT PLAY" } else { "TRANSCODE" }
    ));
    Some(direct)
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
fn decision_codecs(mc: &crate::plex::MediaContainer) -> Option<(String, String)> {
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
const DECISION_UNPLAYABLE: i64 = 2000;

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
fn refusal(mc: &crate::plex::MediaContainer) -> Option<String> {
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

/// Select the audio + subtitle streams server-side for the current part before a
/// transcode. The transcoder encodes the part's SELECTED audio and BURNS its SELECTED
/// subtitle (our client profile advertises no soft-sub support, so Plex's decision is
/// always burn) — a query-param subtitleStreamID does NOT suppress a default-selected
/// sub, only the PUT does. So we PUT subtitleStreamID=0 to keep subs OFF (no burn), or
/// the chosen id to burn it; audioStreamID only when the user switched (else keep default).
///
/// `sid` names the server that owns `part` — the resolve worker passes the id it was given, and the
/// in-playback callers pass [`cur_sid`]. A `Part.id` is server-local, so a PUT sent to the wrong
/// one either 404s or, worse, re-selects streams on a stranger's part that happens to share the
/// number.
fn put_selection(sid: ServerId, part: i64, aud: i64, sub: i64) {
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

/// Fresh opaque session id per playback. Reads the kernel UUID (the TV is Linux); falls
/// back to a ratingKey + monotonic-counter token if that read fails.
fn new_sess(rk: &str) -> String {
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

/// What the queue told us, as owned data for `apply_plan` to install. `machine_id` is `""` when
/// the cached one is still good.
#[derive(Default)]
struct QueueInfo {
    machine_id: String,
    id: String,
    item_id: String,
    up_next: Option<UpNext>,
    rows: Vec<crate::plex::QueueRow>,
}

/// The queued next episode. Main-thread only, and — like `metadata::playing()` — it hands out a
/// `&'static` the Up Next control reads across a frame, so `apply_plan` (main thread) staying its
/// only writer is what keeps that reference sound. A caller that STARTS the next episode must
/// clone first: `request_play` clears this before the new plan lands.
pub(crate) fn up_next() -> Option<&'static UpNext> {
    session().up_next.as_ref()
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
pub(crate) fn with_queue<R>(f: impl FnOnce(&[crate::plex::QueueRow]) -> R) -> R {
    f(&session().queue)
}

/// Build the Up Next descriptor from a queue row. Episodes only: `continuous=1` on a movie
/// returns just the movie itself (verified live — total count 1), and "up next" is a show idea.
/// The gate belongs HERE, on the one-item control — the retained row list is deliberately not
/// episode-gated, because a queue list has to be able to show whatever the queue holds.
fn up_next_of(r: &crate::plex::QueueRow) -> Option<UpNext> {
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
///      (see [`Session::machine_sid`]) — the `install(&Origin, token)` path registers with no id, so
///      for the session's own server rung 1 is empty and this is what saves a round trip.
///   3. `GET /identity`, whose answer travels back in `QueueInfo::machine_id` for `apply_plan` to
///      cache against this server.
fn resolve_playqueue(c: &crate::plex::Client, rk: &str, session: &str, cached: &str) -> QueueInfo {
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
    match c.create_play_queue(effective, rk, session) {
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
}

impl ResolveEnv {
    /// MAIN THREAD ONLY.
    /// `sid` arrives BY VALUE from the caller, which is the whole point: the item being played
    /// carries the server it came from (`PmsMovie`/`UpNext`/`Detail` all hold one now), so a play
    /// raised off a merged shelf resolves against the server that shelf's row belongs to rather
    /// than whichever server happens to be current when the worker gets around to asking.
    fn snapshot(sid: ServerId, rk: &str) -> ResolveEnv {
        let s = session();
        ResolveEnv {
            sid,
            // the cache only counts when it was learned from the server this play is against
            machine_id: if s.machine_sid == sid {
                s.machine_id.clone()
            } else {
                String::new()
            },
            audio_sid: cur_audio_sid(),
            sub_sid: cur_sub_sid(),
            cached_item: crate::metadata::cached_playing(sid, rk),
            quality: quality(),
            src_kbps: crate::metadata::current()
                .filter(|d| detail_describes(d, sid, rk))
                .map_or(0, source_kbps),
        }
    }
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
fn detail_describes(d: &crate::metadata::Detail, sid: ServerId, rk: &str) -> bool {
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
fn source_kbps(d: &crate::metadata::Detail) -> i64 {
    match d.video.as_ref().map(|v| v.bitrate) {
        Some(b) if b > 0 => b,
        _ => d.bitrate,
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
    auto_original: Option<AutoOriginalCandidate>,
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
fn build_stream(rk: &str, part: &str, vcodec: &str, acodec: &str, env: &ResolveEnv) -> Plan {
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
    if !rk.is_empty() {
        let q = resolve_playqueue(client, rk, &session, &env.machine_id);
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
    // capability profile. Falls back to the local codec test if the server returns no usable
    // decision; the local-sample/demo path (rk empty) skips the decision entirely.
    // Server-adjudicated (Phase 2). HEVC now direct-plays (Phase 3 demuxer + native decode);
    // the guard that forced non-h264 to transcode is gone.
    // Smart direct-play: the video decodes natively (H264/HEVC) AND some audio track is
    // direct-playable (AAC/AC3/E-AC3) — even if the DEFAULT track isn't. We own the demuxer, so
    // we direct-play the raw file and FEED a direct-playable track (e.g. a 4K HEVC item: TrueHD
    // default + an AC3 track → native 4K HEVC + AC3, no transcode — beats the server's
    // video-downscaling transcode). Falls back to the server /decision (then the local codec
    // test) when the video isn't direct-playable or NO audio track is (TrueHD/DTS-only → transcode).
    // The video gate consults the DEVICE's own decoder table (devcaps), not this codebase's
    // memory of the dev TV: "the panel decodes HEVC" was the last dev-environment claim still
    // asserted as universal (issue #22's bug class — docs/plex-pass-audit.md, closing section).
    // This is belt-and-braces with the profile — a no-hevc profile means PMS should never
    // *offer* hevc direct-play, but the smart-DP branch below can bypass the server's /decision
    // entirely, so the local gate must agree with the profile on BOTH axes it asserts: the codec
    // AND the width/height bound. Codec agreement alone left the resolution half open — the
    // profile's `*`-scoped limitation makes PMS transcode a 4K source down for a 1080p-bounded
    // SoC, but a branch that never asks the server never meets the limitation, so a 4K file with
    // any AAC/AC3 track (nearly every file has one) direct-played straight onto the bounded
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
    let mut directplay = if !allowed.direct_play {
        false
    } else if !video_dp {
        // The buffer-feed pipeline only decodes what the Load payload declares — H264/H265,
        // and H265 only on a SoC whose table lists the decoder (devcaps). Anything else
        // (AV1/VP9/MPEG-2/…) MUST transcode: we can't feed it even if the server's /decision
        // says directplay (it adjudicates the panel's decoders, not our payload). This gate is
        // why the local sample path (rk empty) is the only other non-transcode case. A source
        // exceeding the device's width/height bound lands here too, and deliberately on the
        // RE-ENCODE side of the branch below (a remux would copy the too-big pixels verbatim);
        // its /decision carries the profile's own bound, so PMS scales the video down.
        false
    } else if !streamable {
        false // non-MKV container → remux (the transcode branch copies the source codecs)
    } else if audio_sel.is_some() {
        true
    } else if rk.is_empty() {
        false
    } else {
        server_decision(client, rk, &session).unwrap_or_else(|| crate::plex::is_dp_audio(acodec))
    };

    // A container-only remux also preserves the original video and avoids the GPU, so it belongs
    // to Auto's Original state and must pass the same remote bandwidth gate as direct play.
    let remux_candidate = video_dp && allowed.remux && !no_video_copy;
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
    let decision = match (env.quality, link_kind) {
        (Quality::Auto, Some(link)) => {
            // The probe is the only expensive input, so it is only taken where it can change the
            // answer: a direct Remote with a feasible Original. Local needs no proof and Relay
            // cannot be talked into carrying a remux.
            let probe = (link == crate::abr::LinkKind::Remote && original_feasible)
                .then(|| measure_remote_original(&client, part, &session, source_transport_kbps))
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
    // link cannot carry.
    let remux = video_dp && allowed.remux && !no_video_copy;
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
    // Carry the picked SOURCE track into the server-side selection (put_selection +
    // &audioStreamID on the transcode query): the remux copies — and the re-encode encodes —
    // the CHOSEN track instead of the part default. The demuxer is NOT pointed at a source
    // ordinal here (the old set_audio_track(aidx) indexed the SERVER's output, whose stream
    // layout is the transcoder's, not the source's) — the payload-codec match finds the lane.
    if let Some((_, _, asid)) = &audio_sel {
        plan.audio_sid = *asid;
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
    put_selection(env.sid, plan.part_id, env.audio_sid, env.sub_sid); // audio/subtitle selection drives the encode/remux + burn
    let sp = transcode_spec(
        rk,
        &session,
        &session,
        remux,
        no_video_copy,
        crate::plex::TranscodeOffset::Fresh,
        env.audio_sid,
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
const PREF_AUDIO_LANG: &str = "eng";

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
fn pick_dp_audio(
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
fn pick_dp_subtitle(subs: &[crate::metadata::Stream]) -> Option<(i64, i32)> {
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

/// PURE: the local direct-play VIDEO test — the codec, the source's stated frame size and its
/// Dolby Vision layering must ALL clear what this device and this pipeline can actually show.
///
/// The codec half: h264 unconditionally (every webOS SoC decodes it), hevc only when the table
/// lists the decoder — anything else the pipeline cannot feed at all. The resolution half is the
/// local agreement with the profile's `*`-scoped `video.width`/`video.height` limitation: the
/// profile makes PMS transcode a 4K source down for a 1080p-bounded SoC, but the smart-DP branch
/// never asks PMS, so without this test a 4K file with one direct-playable audio track was fed
/// verbatim to a decoder whose table says 1920x1088 — the wrong-side failure devcaps' own doc
/// names (issue #22's over-claim class), invisible on the dev TV, whose bound is 4096x2176.
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
fn video_direct_plays(
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
pub(crate) fn playback_preview(d: &crate::metadata::Detail) -> Option<Preview> {
    // A SHOW's container carries no file of its own, so the page answers for the episode its Play
    // button would start — the one the hero is already about. Its frame size and audio list are
    // the show Detail's, which `fetch_item_streams` backfilled from that same episode.
    let (part, vcodec) = match d.on_deck.as_ref().filter(|_| d.part.is_empty()) {
        Some(ep) => (ep.part.as_str(), ep.vcodec.as_str()),
        None => (d.part.as_str(), d.vcodec.as_str()),
    };
    let p = playback_preview_of(
        part,
        vcodec,
        d.width,
        d.height,
        d.dovi.presentation_now(),
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
fn part_is_streamable(part_key: &str) -> bool {
    let name = part_key.rsplit('/').next().unwrap_or(part_key);
    let name = name.split('?').next().unwrap_or(name);
    name.ends_with(".mkv") || name.ends_with(".mp4") || name.ends_with(".m4v")
}

/// Extract the numeric Part id from a Plex part key (/library/parts/{id}/…/file.mkv).
fn part_id_of(part_key: &str) -> i64 {
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

struct AbandonedPlanResources {
    sid: ServerId,
    identities: Vec<String>,
}

fn abandoned_plan_resources(plan: &Plan) -> Option<AbandonedPlanResources> {
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

fn take_resume_for(pending: &mut Option<(u32, i64)>, gen: u32) -> i64 {
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

/// Attach the UI's resume point to the resolve currently in flight.
///
/// `request_play_*` is issued immediately before `app::start_playback`, so the latter knows the
/// position one call later than the former knows the generation.  Tagging here closes that seam:
/// a cancelled or superseded landing cannot consume a bare process-global resume value.
pub(crate) fn arm_play_resume(resume_ns: i64) -> bool {
    if resume_ns <= 0 || !play_pending() {
        return false;
    }
    let gen = PLAY_GEN.load(Ordering::SeqCst);
    *PLAY_RESUME.lock().unwrap_or_else(|e| e.into_inner()) = Some((gen, resume_ns));
    session_mut(|s| s.requested_resume_ns = resume_ns);
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
    sid: ServerId,
    rk: &str,
    part: &str,
    vcodec: &str,
    acodec: &str,
    title: &str,
    ctx: &str,
) -> bool {
    request_play_inner(
        PlaybackRequest {
            sid,
            rk: rk.to_owned(),
            part: part.to_owned(),
            vcodec: vcodec.to_owned(),
            acodec: acodec.to_owned(),
            title: title.to_owned(),
            ctx: ctx.to_owned(),
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
    let trace_generation =
        trace_generation.unwrap_or_else(|| crate::player::report::requested(sid));
    // The fields a play REQUEST owns, as against the ones only a landing may install: the HUD
    // strings (published now, so the pre-roll has a title through the whole resolve) and the five
    // the OUTGOING item leaves behind. Everything else — url, session ids, codecs — stays as it is
    // until `apply_plan` replaces it, which is what lets a still-running playback keep answering
    // for itself while the next one resolves.
    session_mut(|s| {
        s.request = Some(request.clone());
        s.requested_resume_ns = retry.map_or(0, |r| r.resume_ns.max(0));
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
        s.resolve_failed = false;
    });
    // …and the outgoing item's track/marker/chapter store, for exactly the reason above: it stays
    // the PREVIOUS leaf's until this resolve lands. See `metadata::retire_playing_item`.
    crate::metadata::retire_playing_item();
    crate::player::reset_audio_track();
    crate::player::reset_subtitle();
    // Capture the reducer revision BEFORE projecting the environment. Both happen on the main
    // thread, so a later quality/track edit necessarily advances this revision after the snapshot
    // and makes the landing stale instead of installing an old plan beneath a new checkmark.
    let contract_revision = desired_contract_revision();
    // captured HERE, on the main thread, and moved into the worker — see ResolveEnv
    let mut env = ResolveEnv::snapshot(sid, rk);
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
        session_mut(|s| s.resolve_failed = true);
        let mut resume = PLAY_RESUME.lock().unwrap_or_else(|e| e.into_inner());
        if resume.as_ref().is_some_and(|(owner, _)| *owner == gen) {
            *resume = None;
        }
        settle_failed_resolve_spawn();
    }
    spawned
}

/// Start a fresh resolve for the item whose terminal error is still on screen.
///
/// The caller owns Engine teardown; this module owns the immutable request descriptor, track
/// selection and generation-bound resume point.  Returning `false` is honest for URL/dev-trigger
/// playback, which never entered the Plex request funnel and therefore has no item to resolve.
pub(crate) fn can_retry_current_play() -> bool {
    session().request.is_some()
}

/// Resume target not yet proven by a presented frame.  A refused retry keeps this so the next
/// quality choice can try again at the same point.
pub(crate) fn unpresented_resume_ns() -> i64 {
    session().requested_resume_ns.max(0)
}

/// The replacement has shown a frame; from now on the live playhead, including a later backward
/// seek, is the only truthful retry position.
pub(crate) fn confirm_resume_presented() {
    if session().requested_resume_ns > 0 {
        session_mut(|s| s.requested_resume_ns = 0);
    }
}

fn current_retry_context(resume_ns: i64) -> RetryContext {
    RetryContext {
        resume_ns: resume_ns.max(0),
        audio_sid: cur_audio_sid(),
        sub_sid: cur_sub_sid(),
    }
}

pub(crate) fn retry_current_play(resume_ns: i64) -> bool {
    let Some(request) = session().request.clone() else {
        crate::player::log("playback retry: no Plex request descriptor");
        return false;
    };
    crate::player::log(&format!(
        "playback retry: resolving item again at quality {}",
        quality().label(),
    ));
    request_play_inner(request, Some(current_retry_context(resume_ns)), None, true)
}

/// ASYNC twins of `play_movie` / `play_episode`: identical HUD strings and inputs. On `true`, the
/// network work runs on a worker and the caller flips the route THIS frame; an empty or Busy request
/// returns `false` and leaves the current route alone. `app.rs` drains `pump_play` once a frame and
/// starts the engine when the plan lands.
pub(crate) fn request_play_movie(m: &PmsMovie) -> bool {
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
pub(crate) fn request_play_up_next(u: UpNext) -> bool {
    let ctx = crate::ui::fmt::episode_kicker(u.season, u.index, &u.ep_title);
    let title = if u.show_title.is_empty() {
        &u.ep_title
    } else {
        &u.show_title
    };
    // The successor comes out of the PlayQueue of the item now playing, so its server is that
    // item's — [`cur_sid`], not whatever surface is behind the player. Falls back to the browsing
    // surface only if nothing is playing, which the Up Next control cannot actually reach.
    let sid = if cur_sid().is_set() {
        cur_sid()
    } else {
        surface_sid()
    };
    request_play(sid, &u.rk, &u.part, &u.vcodec, &u.acodec, title, &ctx)
}

/// Supersede an in-flight resolve (BACK during a load). The landing is dropped by generation.
pub(crate) fn cancel_play() {
    PLAY_GEN.fetch_add(1, Ordering::SeqCst);
    PLAY_BUSY.store(false, Ordering::SeqCst);
    let abandoned = PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner()).take();
    *PLAY_RESUME.lock().unwrap_or_else(|e| e.into_inner()) = None;
    // …and the refusal, because this is the statement that the withdrawn RESOLVE is over. Do not
    // clear the playback trace here: background suspend calls this to prevent a late plan landing,
    // then resumes the same playback without a new `requested`; only the true exit ritual ends the
    // attempt and clears it.
    clear_play_verdict();
    cancel_playback_request(has_url());
    if let Some(landing) = abandoned {
        retire_abandoned_plan(landing.plan);
    }
}

/// MAIN THREAD, once a frame. Returns the generation-owned resume point when a playable fresh plan
/// was installed. `Some(0)` means start from the beginning; `None` means no playable landing. A
/// stale landing (and its resume) is dropped.
pub(crate) fn pump_play() -> Option<i64> {
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
        let request = session().request.clone();
        let retry = current_retry_context(resume_ns);
        retire_abandoned_plan(plan);
        if let Some(request) = request {
            crate::player::log(
                "playback resolve: desired contract changed in flight; discarding and resolving the latest contract",
            );
            let _ = request_play_inner(request, Some(retry), Some(trace_generation), false);
        } else {
            cancel_playback_request(has_url());
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
    ACTIVE_TRACE_GENERATION.store(trace_generation, Ordering::SeqCst);
    let _start = apply_plan(plan, &rk);
    if let Some(resources) = refused_resources {
        retire_plan_resources(resources);
    }
    // Warm the next episode's still NOW rather than at first draw. The URL has been known since
    // this plan resolved — tens of minutes before the credits — and the fetch is async, so touching
    // it here costs nothing and spares the control a skeleton for one image-transcode round trip at
    // exactly the moment it appears in front of the user. `warm_tex`, not `resolve_tex`: this wants
    // the fetch and nothing else, and a slot warmed tens of minutes early must NOT be carrying the
    // evict-protection a draw takes (see `posters::poster_warm`). At the tile's OWN 480×270 —
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
    if let Some(u) = up_next() {
        crate::ui::widgets::warm_tex_on(item_sid(cur_sid()), &u.thumb, 480, 270, 0);
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
fn apply_plan(plan: Plan, rk: &str) -> Option<RouteStartTransaction> {
    // ACTIVE_ENCODER is the final server-resource owner, even when there is no encoder. A raw
    // Part URL opens/adopts its Streaming Resource under the logical playback id; retaining that
    // id lets scrobble_stop exact-close it while Session::tsession stays empty and Direct remains
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
    crate::metadata::install_playing(plan.playing);
    // main thread only — `up_next()`/`with_queue()` lend out of this (see their docs). The rows
    // arrive already projected: the worker never retained a `Metadata` tree to install here.
    session_mut(|s| {
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
        *s = Session {
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
            // the server-selected subtitle (0 = none), so the menu checkmark, the timeline report
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
            stream_immersive: plan.immersive,
            title,
            ctxline,
            up_next: plan.up_next,
            queue: plan.queue,
        };
    });
    if let (crate::plex::TranscodeDelivery::FixedHls { .. }, Some(rung)) = (
        session().cur_delivery,
        session()
            .cur_ceiling
            .and_then(crate::abr::Rung::from_ceiling),
    ) {
        install_active_hls(&active_encoder, &session().url, rung);
    } else {
        install_active_encoder(&active_encoder);
    }
    let start = prepare_playback_landing(!session().url.is_empty());
    // SHARED.desired_audio_idx is read by the DEMUX THREAD on every reopen — main thread only.
    if let Some(ord) = plan.feed_audio_ordinal {
        crate::player::set_audio_track(ord);
    }
    // `request_play` turned subtitles off for the new item; turn the server's selection back on
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
    candidate: &AutoOriginalCandidate,
    expected: &WorkerTicket,
    offset_secs: i64,
    automatic: bool,
) -> Option<String> {
    let c = cur_client()?;
    let rk = cur_rk();
    let expected_hls = expected.encoder();
    if rk.is_empty() || expected_hls.is_empty() {
        return None;
    }
    // The replacement must have its own exact physical/resource identity. Reusing `sess()` can
    // equal the initial HLS encoder and would mutate the very rollback this handoff promises to
    // retain; a fresh child also makes a failed remux safe to stop without touching HLS.
    let logical_session = sess();
    let namespace = if logical_session.is_empty() {
        expected_hls
    } else {
        logical_session.as_str()
    };
    let replacement = next_encoder_session(namespace);
    let subtitle = cur_sub_sid();
    put_selection(cur_sid(), cur_part_id(), candidate.audio_sid, subtitle);
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
    session_mut(|s| {
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
        s.stream_dovi = crate::metadata::Dovi::NONE;
        s.stream_immersive = false;
    });
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
pub(crate) fn retranscode_for(expected: &WorkerTicket, offset_secs: i64) -> Option<String> {
    if !is_worker_ticket_current(expected) {
        return None;
    }
    if matches!(
        cur_delivery(),
        crate::plex::TranscodeDelivery::FixedHls { .. }
    ) {
        let live = sync_active_hls_to_session();
        if live.as_ref().is_some_and(|(ticket, _)| ticket != expected) {
            return None;
        }
    }
    retranscode_as(expected, offset_secs, false)
}

fn retranscode_as(expected: &WorkerTicket, offset_secs: i64, remux: bool) -> Option<String> {
    let c = cur_client()?;
    let rk = cur_rk();
    if rk.is_empty() || !is_worker_ticket_current(expected) {
        return None;
    }
    // Resolve every fallible recovery input before publishing the new route. A missing candidate
    // must leave the still-playing HLS session untouched, not strand it behind a remux marker.
    let remux_codecs = if remux {
        let s = session();
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
    let delivery = cur_delivery();
    let ceiling = cur_ceiling();
    let no_video_copy = is_no_video_copy();
    let audio_sid = cur_audio_sid();
    let subtitle_sid = cur_sub_sid();
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
    put_selection(cur_sid(), cur_part_id(), audio_sid, subtitle_sid); // drives encode + burn
    let logical = sess();
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
    session_mut(|s| {
        s.cur_remux = remux;
        s.tsession = qsess.clone();
        s.url = url.clone();
        s.stream_vcodec = output_codecs.0.clone();
        s.stream_acodec = output_codecs.1.clone();
        s.stream_fps = 0.0;
        s.stream_dovi = crate::metadata::Dovi::NONE;
        s.stream_immersive = false;
    });
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
pub(crate) fn commit_audio_selection(idx: i32, codec: &str, stream_id: i64) {
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
        cur_delivery(),
        crate::plex::TranscodeDelivery::FixedHls { .. }
    ) {
        session_mut(|s| s.auto_original = None);
    }
    if !is_transcoding() && crate::plex::is_dp_audio(codec) {
        // record the pick: the timeline then reports the stream that actually plays, and a
        // later transcode event (subtitle burn refresh / transcode seek) keeps this track
        session_mut(|s| s.cur_audio_sid = stream_id);
        // persist the USER's pick server-side (official-client behavior): /status/sessions'
        // selected-stream display keys on the part selection, not the timeline report. Only
        // user picks persist — the start-of-play auto-pick (eng preference) reports only.
        put_selection(cur_sid(), cur_part_id(), cur_audio_sid(), cur_sub_sid());
        crate::player::request_audio_track(idx, codec);
    } else {
        session_mut(|s| s.cur_audio_sid = stream_id);
        crate::player::request_audio_switch(stream_id);
    }
}

/// Apply commands which were attached to one exact Original trial, after either the candidate or
/// its rollback Engine has really started. Consuming the value makes cross-trial leakage
/// impossible; dropping it on terminal start failure is the explicit cancellation edge.
pub(crate) fn apply_deferred_original_effects(mut effects: DeferredOriginalEffects) {
    if let Some(q) = effects.quality.take() {
        apply_quality_choice(q);
    }
    if let Some((idx, codec, stream_id)) = effects.audio.take() {
        commit_audio_selection(idx, &codec, stream_id);
    }
}

/// Commit a subtitle pick (`sub_idx` -1 = Off): gate the client-side renderer (direct-play path)
/// and select the burn stream for any transcode of the item — refreshing a live transcode so the
/// server re-burns (or drops) it.
pub(crate) fn commit_subtitle_selection(sub_idx: i32, stream_id: i64) {
    let transcoding = is_transcoding();
    // Burned subtitles are part of the server/decoder contract, so revoke old worker evidence
    // before changing them. A direct-play subtitle is client-rendered and needs no reload; fencing
    // there would kill the valid Original watchdog while leaving the physical route untouched.
    let _edit = transcoding.then(begin_user_contract_boundary);
    // As with audio, a non-Off subtitle may require server burn-in and is not interchangeable
    // with the direct declaration captured at playback start. Off is always safe to carry back.
    if matches!(
        cur_delivery(),
        crate::plex::TranscodeDelivery::FixedHls { .. }
    ) {
        session_mut(|s| {
            if sub_idx < 0 {
                if let Some(candidate) = s.auto_original.as_mut() {
                    candidate.subtitle_ordinal = None;
                }
            } else {
                s.auto_original = None;
            }
        });
    }
    crate::player::request_subtitle(sub_idx);
    set_subtitle(stream_id);
    if transcoding {
        crate::player::request_transcode_refresh(); // retranscode PUTs the selection itself
    } else {
        // This is an immediate client-rendered change: unlike a burn/audio rebuild it is already
        // part of the applied stream contract. Publish projection + reporter tracks as one reducer
        // event so a later rejected action cannot restore the pre-subtitle snapshot.
        commit_in_place_route_projection(false);
        // persist the pick server-side (and subs Off PUTs subtitleStreamID=0, clearing a
        // stale server-side selection that would otherwise burn on the next transcode)
        put_selection(cur_sid(), cur_part_id(), cur_audio_sid(), cur_sub_sid());
    }
}

/// Atomically publish the main-thread session fields a newly spawned timeline reporter may read.
/// Called at the reporter spawn site, before ownership crosses to its worker. The active encoder
/// remains in `PlayerControl`, so a later in-place ABR commit changes the wire session and this
/// projection under one lock without touching main-thread-only `Session`.
pub(crate) fn begin_timeline_reporting() -> Option<TimelineLease> {
    let projection = TimelineProjection {
        sid: cur_sid(),
        rating_key: cur_rk(),
        logical_session: sess(),
        play_queue_id: pq_id(),
        play_queue_item_id: pq_item_id(),
        audio_stream_id: cur_audio_sid(),
        subtitle_stream_id: cur_sub_sid(),
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
mod tests {
    use super::*;

    /// Most route tests install a projection without constructing a native Engine. Make that
    /// synthetic boundary explicit in the fixture layer so production `apply_plan` and tests of
    /// the start reducer both retain the real `Prepared -> Starting -> result` semantics.
    fn apply_plan(plan: Plan, rk: &str) {
        settle_plan_start_in_unit_test(super::apply_plan(plan, rk));
    }

    fn settle_pending_native_start(result: RouteStartResult) -> RouteStartAttempt {
        let transaction = pending_route_start().expect("prepared native start transaction");
        let attempt = claim_route_start_attempt(transaction).expect("physical Load attempt");
        assert!(settle_route_start(attempt, result));
        attempt
    }

    fn rollback_seconds() -> Option<i64> {
        rollback_original_recovery().map(|rollback| rollback.offset_ns / 1_000_000_000)
    }

    fn test_original_candidate(subtitle_ordinal: Option<i32>) -> AutoOriginalCandidate {
        AutoOriginalCandidate {
            url: "https://example.invalid/source.mkv".into(),
            probe_part: "https://example.invalid/source.mkv".into(),
            direct: true,
            vcodec: "hevc".into(),
            acodec: "eac3".into(),
            fps: 23.976,
            dovi: crate::metadata::Dovi::NONE,
            immersive: true,
            audio_sid: 42,
            audio_ordinal: Some(1),
            subtitle_ordinal,
        }
    }

    #[test]
    fn a_user_contract_requested_during_resolve_survives_the_landing() {
        let _g = fresh_registry();
        begin_playback_request();
        request_user_route_intent(UserRouteIntent::Retranscode);

        assert!(
            claim_route_action().is_none(),
            "pre-roll cannot consume a route rebuild"
        );
        settle_plan_start_in_unit_test(prepare_playback_landing(true));

        let action = claim_route_action().expect("the landed Engine inherits the explicit request");
        assert_eq!(
            action.intent,
            RouteIntent::User(UserRouteIntent::Retranscode)
        );
        finish_route_action(&action, RouteApplyResult::Prepared);
    }

    #[test]
    fn a_cancelled_resolve_has_an_explicit_terminal_phase() {
        let _g = fresh_registry();
        begin_playback_request();
        cancel_playback_request(false);
        assert!(
            claim_route_action().is_none(),
            "an empty cancelled resolve lands Idle"
        );

        reset_player_control_for_test();
        begin_playback_request();
        request_user_route_intent(UserRouteIntent::AdaptiveReload);
        cancel_playback_request(true);
        let action = claim_route_action().expect("the retained route lands Stable");
        assert_eq!(
            action.intent,
            RouteIntent::User(UserRouteIntent::AdaptiveReload)
        );
        finish_route_action(&action, RouteApplyResult::Prepared);
    }

    #[test]
    fn cancelling_resolve_restores_failed_even_when_its_projection_has_a_url() {
        let _g = fresh_registry();
        reset_session();
        session_mut(|s| {
            s.url = "https://example.invalid/failed-candidate.mkv".into();
            s.cur_audio_sid = 17;
        });
        let failed_projection = route_projection();
        {
            let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
            control.applied_projection = Some(failed_projection);
            control.phase = ControlPhase::Failed(73);
        }

        begin_playback_request();
        session_mut(|s| {
            s.url = "https://example.invalid/incoming.mkv".into();
            s.cur_audio_sid = 0;
        });
        cancel_playback_request(true);

        let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(control.phase, ControlPhase::Failed(73));
        assert_eq!(url(), "https://example.invalid/failed-candidate.mkv");
        assert_eq!(cur_audio_sid(), 17);
        drop(control);
        reset_session();
        reset_player_control_for_test();
    }

    #[test]
    fn a_new_load_attempt_supersedes_the_old_observer_and_rejects_its_late_results() {
        let _g = fresh_registry();
        reset_player_control_for_test();
        let transaction = begin_route_start().expect("route start transaction");
        assert!(prepare_route_start(transaction));
        let first = claim_route_start_attempt(transaction).expect("first Load attempt");
        assert_eq!(
            classify_live_engine_start(first),
            LiveEngineStartRelation::CurrentAttempt,
            "rediscovering the Engine for the same Load is idempotent",
        );

        begin_engine_teardown(true);
        let second = claim_route_start_attempt(transaction).expect("replacement Load attempt");
        assert_ne!(first.attempt, second.attempt);
        assert_eq!(
            classify_live_engine_start(first),
            LiveEngineStartRelation::Conflict(transaction),
        );
        assert_eq!(
            classify_live_engine_start(second),
            LiveEngineStartRelation::CurrentAttempt,
        );
        assert_eq!(
            route_start_status(first),
            RouteStartStatus::Superseded(second),
        );
        assert_eq!(route_start_status(second), RouteStartStatus::Pending);

        assert!(!settle_route_start(first, RouteStartResult::Started));
        assert!(!settle_route_start(first, RouteStartResult::StartFailed));
        assert_eq!(route_start_status(second), RouteStartStatus::Pending);
        assert!(settle_route_start(second, RouteStartResult::Started));
        assert_eq!(route_start_status(second), RouteStartStatus::Started);
        assert_eq!(
            PLAYER_CONTROL
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .phase,
            ControlPhase::Stable,
        );
        reset_player_control_for_test();
    }

    #[test]
    fn backgrounding_an_unproven_original_rearms_frame_proof_on_a_new_load() {
        let _g = fresh_registry();
        reset_session();
        session_mut(|s| {
            s.url = "http://fixture.invalid/hls/master.m3u8".into();
            s.tsession = "foreground-held-hls".into();
            s.cur_delivery = crate::plex::TranscodeDelivery::FixedHls {
                seconds_per_segment: 2,
            };
            s.cur_ceiling = Some(crate::abr::Rung::P480.ceiling());
        });
        install_active_hls(
            "foreground-held-hls",
            "http://fixture.invalid/hls/master.m3u8",
            crate::abr::Rung::P480,
        );
        reset_player_control_for_test();
        let pending = snapshot_route("foreground-held-hls".into(), 44);
        session_mut(|s| {
            s.url = "https://example.invalid/source.mkv".into();
            s.tsession.clear();
            s.cur_delivery = crate::plex::TranscodeDelivery::ProgressiveMkv;
            s.cur_ceiling = None;
        });
        set_pending_original(pending, true);

        let first = settle_pending_native_start(RouteStartResult::Started);
        assert!(matches!(
            PLAYER_CONTROL
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .phase,
            ControlPhase::OriginalTrial(OriginalTrialPhase::AwaitingFrame(_)),
        ));
        begin_engine_teardown(true);
        let transaction = pending_route_start().expect("Original transaction re-prepared");
        let second = claim_route_start_attempt(transaction).expect("replacement Original Load");
        assert_ne!(first, second);
        assert_eq!(
            route_start_status(first),
            RouteStartStatus::Superseded(second),
        );
        assert!(!settle_route_start(first, RouteStartResult::Started));
        assert!(settle_route_start(second, RouteStartResult::Started));
        assert!(matches!(
            PLAYER_CONTROL
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .phase,
            ControlPhase::OriginalTrial(OriginalTrialPhase::AwaitingFrame(_)),
        ));

        assert_eq!(rollback_seconds(), Some(44));
        settle_pending_native_start(RouteStartResult::Started);
        reset_session();
        install_active_encoder("");
        reset_player_control_for_test();
    }

    #[test]
    fn resolve_cannot_hide_a_live_start_transaction() {
        let _g = fresh_registry();
        reset_player_control_for_test();
        let transaction = begin_route_start().expect("route start transaction");
        assert!(prepare_route_start(transaction));
        let attempt = claim_route_start_attempt(transaction).expect("physical Load attempt");
        let before_revision = desired_contract_revision();

        assert!(!begin_playback_request());
        assert_eq!(desired_contract_revision(), before_revision);
        assert_eq!(route_start_status(attempt), RouteStartStatus::Pending);
        assert!(settle_route_start(attempt, RouteStartResult::Started));
        assert_eq!(route_start_status(attempt), RouteStartStatus::Started);

        reset_player_control_for_test();
    }

    #[test]
    fn accepted_original_load_stays_in_trial_until_a_frame_or_rollback() {
        let _g = fresh_registry();
        reset_session();
        session_mut(|s| {
            s.url = "http://fixture.invalid/hls/master.m3u8".into();
            s.tsession = "held-hls".into();
            s.cur_delivery = crate::plex::TranscodeDelivery::FixedHls {
                seconds_per_segment: 2,
            };
            s.cur_ceiling = Some(crate::abr::Rung::P480.ceiling());
        });
        install_active_hls(
            "held-hls",
            "http://fixture.invalid/hls/master.m3u8",
            crate::abr::Rung::P480,
        );
        reset_player_control_for_test();
        let pending = snapshot_route("held-hls".into(), 31);
        session_mut(|s| {
            s.url = "https://example.invalid/source.mkv".into();
            s.tsession.clear();
            s.cur_delivery = crate::plex::TranscodeDelivery::ProgressiveMkv;
            s.cur_ceiling = None;
        });
        set_pending_original(pending, true);

        let attempt = settle_pending_native_start(RouteStartResult::Started);
        assert_eq!(route_start_status(attempt), RouteStartStatus::Started);
        assert!(matches!(
            PLAYER_CONTROL
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .phase,
            ControlPhase::OriginalTrial(OriginalTrialPhase::AwaitingFrame(_)),
        ));
        assert!(
            claim_route_action().is_none(),
            "Load acceptance is not frame proof"
        );

        assert_eq!(rollback_seconds(), Some(31));
        settle_pending_native_start(RouteStartResult::Started);
        reset_session();
        install_active_encoder("");
        reset_player_control_for_test();
    }

    #[test]
    fn automatic_publication_is_busy_for_the_whole_staged_user_edit() {
        let _g = fresh_registry();
        reset_player_control_for_test();
        install_active_encoder("staging-owner");
        let ticket = worker_ticket();
        let edit = begin_user_quality_boundary(Quality::P720);

        let intent = || AutomaticRouteIntent::HlsToOriginal {
            ticket: ticket.clone(),
            evidence_kbps: 40_000,
            position_ns: 12_000_000_000,
        };
        assert_eq!(
            publish_automatic_route_intent(intent()),
            AutomaticIntentResult::Busy,
        );
        drop(edit);
        assert_eq!(
            publish_automatic_route_intent(intent()),
            AutomaticIntentResult::Accepted,
        );
        reset_player_control_for_test();
    }

    #[test]
    fn a_failed_resolve_spawn_preserves_the_old_playable_route() {
        let _g = fresh_registry();
        session_mut(|s| s.url = "https://example.invalid/still-playing.mkv".into());
        begin_playback_request();
        request_user_route_intent(UserRouteIntent::AdaptiveReload);

        settle_failed_resolve_spawn();

        let action = claim_route_action().expect("the retained route must return to Stable");
        assert_eq!(
            action.intent,
            RouteIntent::User(UserRouteIntent::AdaptiveReload),
        );
        finish_route_action(&action, RouteApplyResult::Prepared);
        reset_session();
        reset_player_control_for_test();
    }

    #[test]
    fn a_failed_resolve_spawn_without_an_old_url_lands_idle() {
        let _g = fresh_registry();
        PLAYER_CONTROL
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .phase = ControlPhase::Idle;
        begin_playback_request();

        settle_failed_resolve_spawn();

        assert!(claim_route_action().is_none());
        assert_eq!(
            PLAYER_CONTROL
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .phase,
            ControlPhase::Idle,
        );
        reset_player_control_for_test();
    }

    #[test]
    fn quality_changed_during_resolve_cannot_land_the_old_contract() {
        let _g = fresh_registry();
        reset_session();
        reset_player_control_for_test();
        begin_playback_request();
        let old_contract = desired_contract_revision();
        let gen = 41;
        PLAY_GEN.store(gen, Ordering::SeqCst);
        PLAY_BUSY.store(true, Ordering::SeqCst);
        *PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(PlayLanding {
            gen,
            trace_generation: 7,
            contract_revision: old_contract,
            plan: Plan {
                url: "https://example.invalid/old-contract.mkv".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P480.ceiling()),
                ..Default::default()
            },
            rk: "rk-old-contract".into(),
        });

        // This is the reducer half of a quality edit after ResolveEnv was snapshotted.
        begin_user_contract_boundary();
        assert_eq!(pump_play(), None);
        assert!(
            url().is_empty(),
            "the stale plan must never become the applied URL"
        );
        assert_ne!(cur_ceiling(), Some(crate::abr::Rung::P480.ceiling()));

        PLAY_BUSY.store(false, Ordering::SeqCst);
        reset_player_control_for_test();
        reset_session();
    }

    #[test]
    fn a_seek_revokes_automatic_evidence_without_erasing_the_user_contract() {
        let _g = fresh_registry();
        install_active_hls(
            "seek-owner",
            "http://fixture.invalid/live.m3u8",
            crate::abr::Rung::P480,
        );
        let before_seek = worker_ticket();
        request_user_route_intent(UserRouteIntent::AdaptiveReload);
        note_user_seek_intent(90_000_000_000);

        assert!(pending_user_route_intent(UserRouteIntent::AdaptiveReload));
        assert_eq!(
            publish_automatic_route_intent(AutomaticRouteIntent::HlsToOriginal {
                ticket: before_seek.clone(),
                evidence_kbps: 50_000,
                position_ns: 90_000_000_000,
            }),
            AutomaticIntentResult::Busy,
        );
        let action = claim_route_action().expect("seek and quality coalesce into the user action");
        assert_eq!(
            action.intent,
            RouteIntent::User(UserRouteIntent::AdaptiveReload)
        );
        finish_route_action(&action, RouteApplyResult::Prepared);
        assert!(commit_user_seek());
        assert_ne!(worker_ticket(), before_seek);
    }

    #[test]
    fn a_seek_retargets_an_accepted_handoff_instead_of_erasing_its_only_producer() {
        let _g = fresh_registry();
        install_active_encoder("direct-owner");
        let worker = worker_ticket();
        assert_eq!(
            publish_automatic_route_intent(AutomaticRouteIntent::OriginalToHls {
                ticket: worker,
                conservative_kbps: 4_000,
                position_ns: 12_000_000_000,
            }),
            AutomaticIntentResult::Accepted,
        );

        note_user_seek_intent(90_000_000_000);
        let action =
            claim_route_action().expect("the accepted handoff still owns the stopped worker");
        assert_eq!(action.ticket, worker_ticket());
        assert!(matches!(
            action.intent,
            RouteIntent::Automatic(AutomaticRouteIntent::OriginalToHls {
                position_ns: 90_000_000_000,
                ..
            })
        ));
        finish_route_action(&action, RouteApplyResult::Prepared);
        assert!(commit_user_seek());
    }

    #[test]
    fn rejected_transcode_seek_preserves_hls_worker_authority() {
        let _g = fresh_registry();
        install_active_hls(
            "seek-refusal-owner",
            "http://fixture.invalid/live.m3u8",
            crate::abr::Rung::P480,
        );
        let worker = worker_ticket();
        note_user_seek_intent(90_000_000_000);
        assert_eq!(
            publish_automatic_route_intent(AutomaticRouteIntent::HlsToOriginal {
                ticket: worker.clone(),
                evidence_kbps: 50_000,
                position_ns: 12_000_000_000,
            }),
            AutomaticIntentResult::Busy,
        );

        reject_user_seek();
        assert_eq!(worker_ticket(), worker);
        assert_eq!(
            publish_automatic_route_intent(AutomaticRouteIntent::HlsToOriginal {
                ticket: worker,
                evidence_kbps: 50_000,
                position_ns: 12_000_000_000,
            }),
            AutomaticIntentResult::Accepted,
        );
        reset_player_control_for_test();
    }

    #[test]
    fn a_rejected_user_action_leaves_the_accepted_handoff_owned_for_the_next_tick() {
        let _g = fresh_registry();
        install_active_encoder("direct-owner");
        assert_eq!(
            publish_automatic_route_intent(AutomaticRouteIntent::OriginalToHls {
                ticket: worker_ticket(),
                conservative_kbps: 4_000,
                position_ns: 12_000_000_000,
            }),
            AutomaticIntentResult::Accepted,
        );
        request_user_route_intent(UserRouteIntent::Retranscode);

        let user = claim_route_action().expect("user action has priority");
        assert_eq!(user.intent, RouteIntent::User(UserRouteIntent::Retranscode));
        finish_route_action(&user, RouteApplyResult::Rejected);

        let automatic = claim_route_action().expect("the stopped producer's handoff was not lost");
        assert!(matches!(automatic.intent, RouteIntent::Automatic(_)));
        finish_route_action(&automatic, RouteApplyResult::Prepared);
    }

    #[test]
    fn rejected_user_action_preserves_old_applied_auto_handoff_without_rebinding_it_to_desired() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        reset_player_control_for_test();
        install_active_encoder("direct-auto-owner");
        let applied = worker_ticket();
        assert_eq!(applied_quality(), Quality::Auto);
        assert_eq!(
            publish_automatic_route_intent(AutomaticRouteIntent::OriginalToHls {
                ticket: applied.clone(),
                conservative_kbps: 4_000,
                position_ns: 12_000_000_000,
            }),
            AutomaticIntentResult::Accepted,
        );

        begin_user_quality_boundary(Quality::P720);
        request_user_route_intent(UserRouteIntent::Retranscode);
        let user = claim_route_action().expect("the newer explicit contract has priority");
        assert_eq!(user.intent, RouteIntent::User(UserRouteIntent::Retranscode));
        finish_route_action(&user, RouteApplyResult::Rejected);

        assert_eq!(
            applied_quality(),
            Quality::Auto,
            "PMS refusal changed the policy which owns the unchanged physical stream",
        );
        let automatic =
            claim_route_action().expect("the stopped Auto producer retained its handoff");
        assert_eq!(
            automatic.ticket, applied,
            "old Auto evidence was rebound to a desired contract it never observed",
        );
        assert!(matches!(
            automatic.intent,
            RouteIntent::Automatic(AutomaticRouteIntent::OriginalToHls { .. })
        ));
        finish_route_action(&automatic, RouteApplyResult::Prepared);
        assert_eq!(
            applied_quality(),
            Quality::Auto,
            "an automatic route transition must not commit the rejected Fixed preference",
        );
        assert_eq!(
            worker_ticket(),
            applied,
            "automatic completion rebound its worker to the rejected desired revision",
        );

        restore_quality(Quality::Original);
        reset_player_control_for_test();
    }

    #[test]
    fn rejected_route_effect_restores_the_whole_applied_projection() {
        let _g = fresh_registry();
        let previous = route_projection();
        session_mut(|s| {
            s.url = "http://fixture.invalid/applied-480.m3u8".into();
            s.tsession = "applied-480".into();
            s.cur_remux = false;
            s.cur_delivery = crate::plex::TranscodeDelivery::FixedHls {
                seconds_per_segment: 2,
            };
            s.cur_no_video_copy = true;
            s.cur_ceiling = Some(crate::abr::Rung::P480.ceiling());
            s.cur_auto_original_watched = false;
            s.cur_audio_sid = 17;
            s.cur_sub_sid = 23;
            s.stream_vcodec = "h264".into();
            s.stream_acodec = "aac".into();
            s.stream_fps = 0.0;
            s.stream_dovi = crate::metadata::Dovi::NONE;
            s.stream_immersive = false;
        });
        restore_quality(Quality::Auto);
        reset_player_control_for_test();

        begin_user_quality_boundary(Quality::P1080High);
        session_mut(|s| {
            s.url = "http://fixture.invalid/not-yet-applied-4k.m3u8".into();
            s.tsession = "not-yet-applied-4k".into();
            s.cur_delivery = crate::plex::TranscodeDelivery::ProgressiveMkv;
            s.cur_no_video_copy = false;
            s.cur_ceiling = Some(crate::abr::Rung::Uhd.ceiling());
            s.cur_auto_original_watched = true;
            s.cur_audio_sid = 99;
            s.cur_sub_sid = 101;
            s.stream_vcodec = "hevc".into();
            s.stream_acodec = "eac3".into();
            s.stream_fps = 23.976;
            s.stream_immersive = true;
        });
        request_user_route_intent(UserRouteIntent::Retranscode);
        let action = claim_route_action().expect("staged user route");
        finish_route_action(&action, RouteApplyResult::Rejected);

        let restored = route_projection();
        assert_eq!(restored.url, "http://fixture.invalid/applied-480.m3u8");
        assert_eq!(restored.tsession, "applied-480");
        assert_eq!(
            restored.delivery,
            crate::plex::TranscodeDelivery::FixedHls {
                seconds_per_segment: 2,
            },
        );
        assert_eq!(restored.ceiling, Some(crate::abr::Rung::P480.ceiling()));
        assert_eq!(restored.audio_sid, 17);
        assert_eq!(restored.subtitle_sid, 23);
        assert_eq!(restored.stream_vcodec, "h264");
        assert_eq!(restored.stream_acodec, "aac");
        assert!(!restored.auto_original_watched);
        assert!(!restored.stream_immersive);

        install_route_projection(&previous);
        install_active_encoder("");
        restore_quality(Quality::Original);
        reset_player_control_for_test();
    }

    #[test]
    fn hls_commit_during_a_staged_user_contract_merges_only_physical_fields() {
        let _g = fresh_registry();
        let previous = route_projection();
        session_mut(|s| {
            s.url = "http://fixture.invalid/old-480.m3u8".into();
            s.tsession = "old-480".into();
            s.cur_delivery = crate::plex::TranscodeDelivery::FixedHls {
                seconds_per_segment: 2,
            };
            s.cur_ceiling = Some(crate::abr::Rung::P480.ceiling());
            s.cur_audio_sid = 17;
            s.cur_sub_sid = 23;
            s.stream_vcodec = "h264".into();
            s.stream_acodec = "aac".into();
        });
        restore_quality(Quality::Auto);
        install_active_hls(
            "old-480",
            "http://fixture.invalid/old-480.m3u8",
            crate::abr::Rung::P480,
        );
        reset_player_control_for_test();
        let worker = worker_ticket();

        begin_user_contract_boundary();
        session_mut(|s| s.cur_audio_sid = 99);
        request_user_route_intent(UserRouteIntent::Retranscode);
        assert!(replace_active_hls_for(
            &worker,
            "new-720",
            "http://fixture.invalid/new-720.m3u8",
            crate::abr::Rung::P720,
            None,
        )
        .is_some());
        sync_active_hls_to_session().expect("physical HLS commit");

        let action = claim_route_action().expect("staged audio rebuild");
        finish_route_action(&action, RouteApplyResult::Rejected);
        let restored = route_projection();
        assert_eq!(restored.url, "http://fixture.invalid/new-720.m3u8");
        assert_eq!(restored.tsession, "new-720");
        assert_eq!(restored.ceiling, Some(crate::abr::Rung::P720.ceiling()));
        assert_eq!(
            restored.audio_sid, 17,
            "unaccepted track leaked into applied route"
        );
        assert_eq!(restored.subtitle_sid, 23);

        install_route_projection(&previous);
        restore_quality(Quality::Original);
        reset_player_control_for_test();
    }

    #[test]
    fn rejected_user_retranscode_keeps_the_physical_worker_authorized() {
        let _g = fresh_registry();
        install_active_hls(
            "still-serving-hls",
            "http://fixture.invalid/live.m3u8",
            crate::abr::Rung::P480,
        );
        let physical_worker = worker_ticket();

        request_user_route_intent(UserRouteIntent::Retranscode);
        let action = claim_route_action().expect("user application");
        finish_route_action(&action, RouteApplyResult::Rejected);

        assert_eq!(
            worker_ticket(),
            physical_worker,
            "a refused desired route must not revoke the unchanged applied worker"
        );
        assert_eq!(
            publish_automatic_route_intent(AutomaticRouteIntent::HlsToOriginal {
                ticket: physical_worker,
                evidence_kbps: 50_000,
                position_ns: 12_000_000_000,
            }),
            AutomaticIntentResult::Accepted,
            "the retained HLS worker must resume adaptive publication after refusal"
        );
        reset_player_control_for_test();
    }

    #[test]
    fn pinning_the_live_auto_hls_rung_fences_its_worker_before_projection_changes() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                url: "http://fixture.invalid/4000/master.m3u8".into(),
                sess: "logical-auto".into(),
                tsession: "encoder-auto-720".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P720.ceiling()),
                src_measure: (22_000, 3_840, 2_160),
                auto_original: Some(test_original_candidate(None)),
                ..Default::default()
            },
            "rk-auto-720",
        );
        let outgoing = worker_ticket();
        assert_eq!(
            publish_automatic_route_intent(AutomaticRouteIntent::HlsToOriginal {
                ticket: outgoing.clone(),
                evidence_kbps: 50_000,
                position_ns: 90_000_000_000,
            }),
            AutomaticIntentResult::Accepted,
        );

        set_quality(Quality::P720);

        assert_eq!(quality(), Quality::P720);
        assert_eq!(
            cur_delivery(),
            crate::plex::TranscodeDelivery::ProgressiveMkv,
        );
        assert_eq!(
            publish_automatic_route_intent(AutomaticRouteIntent::HlsToOriginal {
                ticket: outgoing,
                evidence_kbps: 60_000,
                position_ns: 91_000_000_000,
            }),
            AutomaticIntentResult::Busy,
            "the old Auto worker is paused while the desired pin is applying, but remains the applied owner until commit",
        );
        let user = claim_route_action().expect("pinning HLS queues a manual transcode");
        assert_eq!(user.intent, RouteIntent::User(UserRouteIntent::Retranscode));
        finish_route_action(&user, RouteApplyResult::Rejected);
        let automatic = claim_route_action()
            .expect("the already accepted handoff remains owned after the user action");
        assert!(matches!(automatic.intent, RouteIntent::Automatic(_)));
        finish_route_action(&automatic, RouteApplyResult::Prepared);

        restore_quality(Quality::Original);
        reset_session();
        reset_player_control_for_test();
    }

    #[test]
    fn reselecting_the_exact_quality_does_not_fence_the_current_worker() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                url: "http://fixture.invalid/4000/master.m3u8".into(),
                tsession: "encoder-auto-720".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P720.ceiling()),
                src_measure: (22_000, 3_840, 2_160),
                auto_original: Some(test_original_candidate(None)),
                ..Default::default()
            },
            "rk-auto-720",
        );
        let before = worker_ticket();

        set_quality(Quality::Auto);

        assert_eq!(worker_ticket(), before);
        assert!(
            claim_route_action().is_none(),
            "an identical Auto selection must not restart or re-fence its live HLS worker",
        );

        restore_quality(Quality::Original);
        reset_session();
        reset_player_control_for_test();
    }

    #[test]
    fn a_pending_retranscode_cannot_be_weakened_into_a_native_reload() {
        let _g = fresh_registry();
        request_user_route_intent(UserRouteIntent::Retranscode);
        request_user_route_intent(UserRouteIntent::NativeAudioReload);

        let action = claim_route_action().expect("merged user obligation");
        assert_eq!(
            action.intent,
            RouteIntent::User(UserRouteIntent::Retranscode)
        );
        finish_route_action(&action, RouteApplyResult::Prepared);
    }

    #[test]
    fn subtitle_off_keeps_a_pending_original_recovery() {
        let _g = fresh_registry();
        restore_quality(Quality::Original);
        apply_plan(
            Plan {
                url: "http://fixture.invalid/4000/master.m3u8".into(),
                tsession: "encoder-subtitle-off".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P720.ceiling()),
                sub_sid: 77,
                auto_original: Some(test_original_candidate(Some(3))),
                ..Default::default()
            },
            "rk-subtitle-off",
        );
        request_user_route_intent(UserRouteIntent::RecoverOriginal);

        commit_subtitle_selection(-1, 0);

        assert_eq!(cur_sub_sid(), 0);
        assert_eq!(
            session()
                .auto_original
                .as_ref()
                .and_then(|candidate| candidate.subtitle_ordinal),
            None,
            "the retained source declaration must carry subtitles Off",
        );
        let action = claim_route_action().expect("Original recovery remains the owned action");
        assert_eq!(
            action.intent,
            RouteIntent::User(UserRouteIntent::RecoverOriginal),
        );
        finish_route_action(&action, RouteApplyResult::Prepared);

        reset_session();
        reset_player_control_for_test();
        crate::player::reset_subtitle();
    }

    #[test]
    fn a_direct_subtitle_change_keeps_the_original_watchdog_ticket_current() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                url: "https://example.invalid/source.mkv".into(),
                delivery: crate::plex::TranscodeDelivery::ProgressiveMkv,
                transport_kbps: 22_000,
                auto_original_watched: true,
                auto_original: Some(test_original_candidate(None)),
                ..Default::default()
            },
            "rk-direct-subtitle",
        );
        let watchdog = worker_ticket();

        commit_subtitle_selection(2, 88);

        assert_eq!(worker_ticket(), watchdog);
        assert!(auto_original_watch().is_some());
        assert!(
            claim_route_action().is_none(),
            "client-rendered subtitles do not replace the direct media route",
        );

        reset_session();
        reset_player_control_for_test();
        restore_quality(Quality::Original);
        crate::player::reset_subtitle();
    }

    #[test]
    fn subtitle_on_invalidates_a_pending_original_recovery() {
        let _g = fresh_registry();
        restore_quality(Quality::Original);
        apply_plan(
            Plan {
                url: "http://fixture.invalid/4000/master.m3u8".into(),
                tsession: "encoder-subtitle-on".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P720.ceiling()),
                auto_original: Some(test_original_candidate(None)),
                ..Default::default()
            },
            "rk-subtitle-on",
        );
        request_user_route_intent(UserRouteIntent::RecoverOriginal);

        commit_subtitle_selection(2, 88);

        assert!(session().auto_original.is_none());
        let action = claim_route_action().expect("the burned subtitle needs HLS retranscode");
        assert_eq!(
            action.intent,
            RouteIntent::User(UserRouteIntent::Retranscode)
        );
        finish_route_action(&action, RouteApplyResult::Prepared);

        reset_session();
        reset_player_control_for_test();
        crate::player::reset_subtitle();
    }

    #[test]
    fn audio_change_invalidates_a_pending_original_recovery() {
        let _g = fresh_registry();
        restore_quality(Quality::Original);
        apply_plan(
            Plan {
                url: "http://fixture.invalid/4000/master.m3u8".into(),
                tsession: "encoder-audio-change".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P720.ceiling()),
                auto_original: Some(test_original_candidate(None)),
                ..Default::default()
            },
            "rk-audio-change",
        );
        request_user_route_intent(UserRouteIntent::RecoverOriginal);

        commit_audio_selection(1, "aac", 99);

        assert!(session().auto_original.is_none());
        let action = claim_route_action().expect("the new audio track needs HLS retranscode");
        assert_eq!(
            action.intent,
            RouteIntent::User(UserRouteIntent::Retranscode)
        );
        finish_route_action(&action, RouteApplyResult::Prepared);

        reset_session();
        reset_player_control_for_test();
        crate::player::reset_audio_track();
    }

    #[test]
    fn an_original_trial_is_busy_not_stale_to_its_new_watchdog() {
        let _g = fresh_registry();
        install_active_encoder("original-owner");
        let ticket = worker_ticket();
        PLAYER_CONTROL
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .phase = ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(1));

        assert_eq!(
            publish_automatic_route_intent(AutomaticRouteIntent::OriginalToHls {
                ticket,
                conservative_kbps: 4_000,
                position_ns: 12_000_000_000,
            }),
            AutomaticIntentResult::Busy,
        );
        reset_player_control_for_test();
    }

    #[test]
    fn teardown_invalidates_the_worker_before_it_can_publish() {
        let _g = fresh_registry();
        install_active_hls(
            "teardown-owner",
            "http://fixture.invalid/live.m3u8",
            crate::abr::Rung::P480,
        );
        let outgoing = worker_ticket();
        begin_engine_teardown(false);

        assert_eq!(
            publish_automatic_route_intent(AutomaticRouteIntent::HlsToOriginal {
                ticket: outgoing,
                evidence_kbps: 50_000,
                position_ns: 90_000_000_000,
            }),
            AutomaticIntentResult::Stale,
        );
        assert!(
            claim_route_action().is_none(),
            "Stopping owns the transition boundary"
        );
    }

    #[test]
    fn a_route_commit_between_automatic_publication_and_claim_discards_the_stale_action() {
        let _g = fresh_registry();
        install_active_hls(
            "auto-owner",
            "http://fixture.invalid/live.m3u8",
            crate::abr::Rung::P480,
        );
        let outgoing = worker_ticket();
        assert_eq!(
            publish_automatic_route_intent(AutomaticRouteIntent::HlsToOriginal {
                ticket: outgoing.clone(),
                evidence_kbps: 50_000,
                position_ns: 90_000_000_000,
            }),
            AutomaticIntentResult::Accepted,
        );
        assert!(
            replace_active_encoder_for(&outgoing, "new-owner").is_some(),
            "the candidate wins before the main thread claims the automatic handoff",
        );

        assert!(
            claim_route_action().is_none(),
            "the queued evidence belongs to the retired route and is discarded",
        );
    }

    #[test]
    fn a_claimed_route_action_fences_worker_candidate_commits() {
        let _g = fresh_registry();
        install_active_hls(
            "action-owner",
            "http://fixture.invalid/live.m3u8",
            crate::abr::Rung::P480,
        );
        let worker = worker_ticket();
        request_user_route_intent(UserRouteIntent::Retranscode);
        let action = claim_route_action().expect("user action claimed");

        assert_eq!(
            replace_active_hls_with(
                &worker,
                "candidate-owner",
                "http://fixture.invalid/candidate.m3u8",
                crate::abr::Rung::P720Low,
                None,
                || Some(()),
            ),
            Err(ActiveHlsCommitRefusal::RouteMoved),
            "Applying is the exclusive route-mutation phase",
        );
        finish_route_action(&action, RouteApplyResult::Prepared);
    }

    #[test]
    fn a_route_change_wins_over_an_expired_control_snapshot() {
        assert!(matches!(
            classify_prime_decision(false, crate::plex::JsonDeadlineOutcome::Deadline),
            Err(PrimeRefusal::Session),
        ));
    }

    /// Regression for the worker handoff race: a boolean ownership check followed by a mailbox
    /// store let seek replace ACTIVE in between. The callback door must both reject an already
    /// moved route without touching the mailbox and hold ACTIVE throughout an accepted store.
    #[test]
    fn source_recovery_publication_is_atomic_with_route_ownership() {
        let _g = fresh_registry();
        let owner = "source-recovery-owner";
        install_active_hls(
            owner,
            "http://fixture.invalid/live.m3u8",
            crate::abr::Rung::P480,
        );
        let owner_lease = active_route_lease();
        let superseded = RouteLease {
            epoch: owner_lease.epoch,
            encoder: "superseded-owner".into(),
        };
        let mailbox = std::sync::atomic::AtomicI64::new(0);

        assert_eq!(
            with_active_route(&superseded, || { mailbox.store(49_041, Ordering::Release) }),
            Err(ActiveEncoderRefusal::RouteMoved),
        );
        assert_eq!(
            mailbox.load(Ordering::Acquire),
            0,
            "a superseded worker cannot publish its Original handoff",
        );

        assert_eq!(
            with_active_route(&owner_lease, || {
                assert!(matches!(
                    PLAYER_CONTROL.try_lock(),
                    Err(std::sync::TryLockError::WouldBlock),
                ));
                mailbox.store(49_041, Ordering::Release);
            }),
            Ok(()),
        );
        assert_eq!(mailbox.load(Ordering::Acquire), 49_041);
        assert!(
            replace_active_encoder(owner, "seek-owner"),
            "the route may move only after the publication callback releases ACTIVE",
        );
        install_active_encoder("");
    }

    /// A semantic route change can keep the same PMS resource id: direct Original deliberately
    /// retains the HLS Streaming Resource while dropping its HLS projection. Comparing only the
    /// id therefore admits an outgoing HLS worker after ownership has changed (same-id ABA).
    #[test]
    fn a_same_id_route_change_invalidates_the_outgoing_worker() {
        let _g = fresh_registry();
        let owner = "same-resource-owner";
        install_active_hls(
            owner,
            "http://fixture.invalid/live.m3u8",
            crate::abr::Rung::P480,
        );
        let outgoing_owner = active_route_lease();

        assert!(
            replace_active_encoder(owner, owner),
            "direct Original keeps the exact PMS resource id",
        );
        assert_eq!(
            with_active_route(&outgoing_owner, || ()),
            Err(ActiveEncoderRefusal::RouteMoved),
            "the old worker lease must not survive a same-id semantic route change",
        );
        install_active_encoder("");
    }

    #[test]
    fn prime_refusals_follow_the_issued_cause_not_the_clock_at_return() {
        let response = |status, body: &[u8]| crate::plex::JsonDeadlineOutcome::Response {
            reply: crate::http::Reply {
                status,
                body: body.to_vec(),
            },
            parsed: None,
        };
        assert!(matches!(
            classify_prime_decision(true, response(500, b"nope")),
            Err(PrimeRefusal::Control),
        ));
        assert!(matches!(
            classify_prime_decision(true, response(200, b"not-json")),
            Err(PrimeRefusal::Control),
        ));
        assert!(matches!(
            classify_prime_decision(true, crate::plex::JsonDeadlineOutcome::Transport),
            Err(PrimeRefusal::Control),
        ));
        assert!(matches!(
            classify_prime_decision(true, crate::plex::JsonDeadlineOutcome::Deadline),
            Err(PrimeRefusal::Deadline),
        ));
        assert!(matches!(
            classify_prime_decision(false, response(500, b"nope")),
            Err(PrimeRefusal::Session),
        ));
        assert!(matches!(
            classify_prime_decision(false, crate::plex::JsonDeadlineOutcome::Transport),
            Err(PrimeRefusal::Session),
        ));
    }

    #[test]
    fn a_resume_intent_belongs_to_exactly_one_resolve_generation() {
        let mut pending = Some((41, 3_600_000_000_000));
        assert_eq!(take_resume_for(&mut pending, 40), 0);
        assert_eq!(pending, Some((41, 3_600_000_000_000)));
        assert_eq!(take_resume_for(&mut pending, 41), 3_600_000_000_000);
        assert_eq!(pending, None);
    }

    #[test]
    fn abandoned_resolves_retire_the_streaming_resources_they_created() {
        use std::io::{BufRead, BufReader, Write};

        let _g = fresh_registry();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind cleanup server");
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port() as i32;
        let (tx, rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            let mut requests = Vec::new();
            while std::time::Instant::now() < deadline && requests.len() < 2 {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        let mut request = String::new();
                        let mut reader = BufReader::new(socket.try_clone().expect("clone socket"));
                        reader
                            .read_line(&mut request)
                            .expect("read cleanup request");
                        socket
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .expect("cleanup response");
                        requests.push(request);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept cleanup request: {error}"),
                }
            }
            tx.send(requests).unwrap();
        });
        let sid = crate::plex::register_for_test(
            "stale-resolve",
            "127.0.0.1",
            port,
            "token",
            "stale-resolve-client",
        );

        PLAY_GEN.store(2, Ordering::SeqCst);
        *PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(PlayLanding {
            gen: 1,
            trace_generation: 1,
            contract_revision: desired_contract_revision(),
            plan: Plan {
                sid,
                sess: "abandoned-logical-resource".into(),
                url: "https://example.invalid/source.mkv".into(),
                ..Default::default()
            },
            rk: "abandoned-rk".into(),
        });

        assert_eq!(
            pump_play(),
            None,
            "the superseded plan may not be installed"
        );

        PLAY_GEN.store(3, Ordering::SeqCst);
        *PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(PlayLanding {
            gen: 3,
            trace_generation: 2,
            contract_revision: desired_contract_revision(),
            plan: Plan {
                sid,
                sess: "refused-logical-resource".into(),
                verdict: Some("server refused this route".into()),
                ..Default::default()
            },
            rk: "refused-rk".into(),
        });
        assert_eq!(pump_play(), None, "a refusal has no playable URL");
        assert!(
            play_refused(),
            "its server verdict still reaches the error read-out"
        );

        let requests = rx.recv().expect("cleanup observations");
        server.join().unwrap();
        assert_eq!(
            requests.len(),
            2,
            "both ownerless resources need an exact close: {requests:?}"
        );
        for session in ["abandoned-logical-resource", "refused-logical-resource"] {
            let request = requests
                .iter()
                .find(|request| request.contains(&format!("session={session}")))
                .unwrap_or_else(|| panic!("missing cleanup for {session}: {requests:?}"));
            assert!(
                request.contains("/video/:/transcode/universal/stop?"),
                "{request}"
            );
            assert!(request.contains("closeResourceSession=1"), "{request}");
        }

        crate::plex::reset_servers_for_test();
        reset_session();
    }

    #[test]
    fn a_refused_retry_keeps_its_position_and_full_request_for_the_next_quality() {
        let _g = crate::testlock::serial();
        reset_session();
        let request = PlaybackRequest {
            sid: ServerId::UNSET,
            rk: "episode-42".into(),
            part: "/library/parts/987/1700000000/file.mkv".into(),
            vcodec: "hevc".into(),
            acodec: "eac3".into(),
            title: "Episode".into(),
            ctx: "S01 E02".into(),
        };
        session_mut(|s| {
            s.request = Some(request.clone());
            s.requested_resume_ns = 3_600_000_000_000;
            s.cur_audio_sid = 17;
            s.cur_sub_sid = 23;
        });

        assert_eq!(
            current_retry_context(3_600_000_000_000),
            RetryContext {
                resume_ns: 3_600_000_000_000,
                audio_sid: 17,
                sub_sid: 23,
            },
            "a rescue must not silently restore the server-default tracks",
        );

        apply_plan(
            Plan {
                verdict: Some("temporary refusal".into()),
                ..Default::default()
            },
            "episode-42",
        );

        assert_eq!(session().request.as_ref(), Some(&request));
        assert_eq!(unpresented_resume_ns(), 3_600_000_000_000);
        confirm_resume_presented();
        assert_eq!(unpresented_resume_ns(), 0);
        reset_session();
        install_active_encoder("");
    }

    /// `part_id_of` gates the server-side stream selection: `put_selection` returns early on
    /// `<= 0`, so a parse miss silently disables subtitle suppression and audio selection for
    /// the whole item — no error, no log line, just a burned-in subtitle nobody asked for.

    // ---- the QUALITY ceiling as a ROUTING policy --------------------------------------------
    //
    // These grade `flavors_allowed(link_policy(link), quality_policy(q, auto_original, …))`, which is
    // the expression `build_stream` itself evaluates — not a re-derivation of it. `build_stream`
    // is unreachable from the host (it needs a `Client` and a PMS), and the composition is the
    // half that can silently go wrong, so it is the half that is factored out and pinned.

    /// A library file's shape, for readability at the call sites below: (kbps, w, h).
    const UHD_REMUX: (i64, i64, i64) = (60000, 3840, 2160); // a 60 Mbps 4K rip
    const HD_BIG: (i64, i64, i64) = (30000, 1920, 1080); // the case the whole feature is about
    const HD_SMALL: (i64, i64, i64) = (3000, 1280, 720); // a 3 Mbit/s 720p episode
    const UNMEASURED: (i64, i64, i64) = (0, 0, 0); // PMS said nothing (a play straight off a shelf)

    /// A stop acknowledgement is not a release event. The ledger must coalesce concurrent
    /// checks, retain present/unknown sessions, retry a stop only when it was not accepted, and
    /// release one server independently only after physical absence and exact logical close.
    #[test]
    fn encoder_cleanup_ledger_releases_only_on_exact_absence() {
        let sid_a = ServerId::from_raw(1);
        let sid_b = ServerId::from_raw(2);
        let mut ledger = EncoderCleanupLedger::default();

        assert!(ledger.remember(sid_a, "candidate-a"));
        assert!(
            !ledger.remember(sid_a, "candidate-a"),
            "one physical key, one cleanup owner"
        );
        assert!(!ledger.is_clear(sid_a));
        assert!(
            ledger.is_clear(sid_b),
            "another PMS has independent resource accounting"
        );

        let first = ledger.take_unchecked(sid_a);
        assert_eq!(first.len(), 1);
        assert!(
            first[0].stop_needed,
            "the first worker must issue the one requested stop"
        );
        assert!(
            ledger.take_unchecked(sid_a).is_empty(),
            "an in-flight ping is single-owner"
        );
        ledger.finish(
            first.into_iter().next().unwrap(),
            Some(true),
            Some(true),
            None,
        );

        let ping = ledger.take_unchecked(sid_a);
        assert_eq!(ping.len(), 1);
        assert!(
            !ping[0].stop_needed,
            "accepted stop is polled with ping, not re-enqueued"
        );
        ledger.finish(ping.into_iter().next().unwrap(), None, None, None);
        assert!(
            !ledger.is_clear(sid_a),
            "transport uncertainty is not absence"
        );

        let absent = ledger.take_unchecked(sid_a);
        ledger.finish(absent.into_iter().next().unwrap(), Some(false), None, None);
        assert!(
            !ledger.is_clear(sid_a),
            "404 ping cannot prove that the separately-owned Streaming Resource was released",
        );

        let close = ledger.take_unchecked(sid_a);
        assert_eq!(close.len(), 1);
        assert!(
            close[0].physical_absent,
            "a known-absent encoder must not be pinged again"
        );
        assert!(
            !close[0].stop_needed,
            "a known-absent encoder must not be stopped again"
        );
        ledger.finish(
            close.into_iter().next().unwrap(),
            Some(false),
            None,
            Some(true),
        );
        assert!(
            ledger.is_clear(sid_a),
            "only the logical close completes exact cleanup"
        );

        assert!(ledger.remember(sid_a, "candidate-retry"));
        let failed_stop = ledger.take_unchecked(sid_a).into_iter().next().unwrap();
        ledger.finish(failed_stop, Some(true), Some(false), None);
        let retry = ledger.take_unchecked(sid_a).into_iter().next().unwrap();
        assert!(
            retry.stop_needed,
            "an unaccepted stop must be retried from later media evidence"
        );
    }

    /// The live Mandalorian regression, reproduced at the PMS resource boundary.  A raw Part GET
    /// first exact-looks up the supplied Streaming Resource and only enters AdHoc MDE when that
    /// lookup (and its token alias fallback) misses.  For this file AdHoc rejects Original at
    /// `99_341 > 92_000` kbps and PMS 1.43.4 turns the missing decision code into HTTP 500.  The
    /// previous implementation CAUSED that path by stopping and exact-closing the live HLS
    /// resource before measuring the Part.
    ///
    /// Pin the opposite client ordering: the finite source read borrows the exact active HLS
    /// identity and no control-plane request precedes or follows it. This proves the local route
    /// remains selected; it deliberately does not infer PMS-side cursor continuity.
    #[test]
    fn source_probe_reuses_live_hls_resource_instead_of_entering_adhoc_mde() {
        use std::io::{BufRead, BufReader, Write};

        let _g = fresh_registry();
        if !crate::net::global_init() || !crate::curlio::available() {
            return;
        }
        const SOURCE_KBPS: u32 = 25_264;
        let source_plan = crate::abr::source_probe_plan(SOURCE_KBPS, crate::abr::PROBE_BUDGET_MS)
            .expect("a measured source has a finite probe plan");
        assert_eq!(
            source_plan.budget_ms, 1_000,
            "the source body and PMS control plane must exercise different horizons",
        );
        let probe_bytes = source_plan.target_bytes;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port() as i32;
        let (tx, rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept source request");
            let mut reader = BufReader::new(socket.try_clone().expect("clone socket"));
            let mut first = String::new();
            reader.read_line(&mut first).expect("request line");
            let mut headers = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("request header");
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                headers.push(line);
            }
            tx.send((first, headers)).expect("publish request");
            write!(
                socket,
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                probe_bytes - 1,
                probe_bytes * 2,
                probe_bytes,
            )
            .expect("source headers");
            socket
                .write_all(&vec![0x55; probe_bytes])
                .expect("source body");
        });

        let sid = crate::plex::register_for_test(
            "probe-lifecycle",
            "127.0.0.1",
            port,
            "tok",
            "cid-probe-lifecycle",
        );
        let rung = crate::abr::Rung::P480;
        let active = "probe-live";
        install_active_hls(active, "http://fixture.invalid/old.m3u8", rung);
        let active_route = worker_ticket();
        let control = HlsAbrControl {
            trace_generation: 0,
            sid,
            rating_key: "1".into(),
            logical_session: "probe-logical".into(),
            audio_stream_id: 0,
            subtitle_stream_id: 0,
            seconds_per_segment: 2,
            initial_rung: rung,
            initial_observed: None,
            fixture_base: String::new(),
            original_probe_part: "/library/parts/1/file.mkv".into(),
            original_source_kbps: SOURCE_KBPS,
            catalog: crate::abr::HlsActuatorCatalog::measured(),
            prior: None,
            history: crate::abr::TransitionHistory::default(),
            original_features: crate::abr::SourceFeatures::default(),
        };
        let result = control.probe_original_while_hls(&active_route, source_plan);
        assert!(
            matches!(result, OriginalProbeResult::Measured(sample) if sample.target_reached),
            "the finite source response is the measurement: {result:?}",
        );

        let request = rx.recv().expect("captured source request");
        assert!(
            request.0.starts_with("GET /library/parts/1/file.mkv?"),
            "{:?}",
            request.0
        );
        assert!(
            request.0.contains("X-Plex-Session-Identifier=probe-live"),
            "the finite read must exact-reuse the active HLS Streaming Resource: {:?}",
            request.0,
        );
        assert!(
            request
                .1
                .iter()
                .any(|line| line.to_ascii_lowercase().starts_with("range: bytes=0-")),
            "the source experiment must be one finite HTTP response",
        );
        assert_eq!(
            active_encoder(),
            active,
            "a measurement cannot replace the selected client-side HLS route",
        );

        server.join().unwrap();
        install_active_encoder("");
        crate::plex::reset_servers_for_test();
        reset_session();
    }

    /// PMS 1.43.4 turns an AdHoc bandwidth refusal into 500.  That status is a request failure,
    /// not a zero-rate sample, and an optional source check must never make the live HLS route
    /// fatal or replace it.
    #[test]
    fn a_rejected_original_probe_keeps_hls_and_produces_no_capacity_observation() {
        use std::io::{BufRead, BufReader, Write};

        let _g = fresh_registry();
        if !crate::net::global_init() || !crate::curlio::available() {
            return;
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port() as i32;
        let (tx, rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept source request");
            let mut reader = BufReader::new(socket.try_clone().expect("clone socket"));
            let mut first = String::new();
            reader.read_line(&mut first).expect("request line");
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("request header");
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            tx.send(first).expect("publish request");
            socket
                .write_all(
                    b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("server refusal");
        });

        let sid = crate::plex::register_for_test(
            "probe-bodyless",
            "127.0.0.1",
            port,
            "tok",
            "cid-probe-bodyless",
        );
        let rung = crate::abr::Rung::P480;
        let active = "probe-bodyless-live";
        install_active_hls(active, "http://fixture.invalid/old.m3u8", rung);
        let active_route = worker_ticket();
        let control = HlsAbrControl {
            trace_generation: 0,
            sid,
            rating_key: "1".into(),
            logical_session: "probe-bodyless-logical".into(),
            audio_stream_id: 0,
            subtitle_stream_id: 0,
            seconds_per_segment: 2,
            initial_rung: rung,
            initial_observed: None,
            fixture_base: String::new(),
            original_probe_part: "/library/parts/1/file.mkv".into(),
            original_source_kbps: 320,
            catalog: crate::abr::HlsActuatorCatalog::measured(),
            prior: None,
            history: crate::abr::TransitionHistory::default(),
            original_features: crate::abr::SourceFeatures::default(),
        };
        let plan = crate::abr::source_probe_plan(320, crate::abr::PROBE_BUDGET_MS).unwrap();
        let result = control.probe_original_while_hls(&active_route, plan);
        assert_eq!(
            result,
            OriginalProbeResult::Failed {
                outcome: crate::player::report::TraceOutcome::ServerState,
                failure: OriginalProbeFailure::HttpStatus(500),
            },
            "HTTP 500 stays exact for the panel and is never a zero-rate observation",
        );
        let request = rx.recv().expect("captured source request");
        assert!(request.contains("X-Plex-Session-Identifier=probe-bodyless-live"));
        assert_eq!(
            active_encoder(),
            active,
            "the rejected check leaves the client-side HLS route selected",
        );

        server.join().unwrap();
        install_active_encoder("");
        crate::plex::reset_servers_for_test();
        reset_session();
    }

    #[test]
    fn a_partial_source_body_is_not_traced_as_a_successful_measurement() {
        use crate::player::report::TraceOutcome;
        let sample = |target_reached| crate::curlio::ThroughputSample {
            bytes: 64 * 1024,
            elapsed: std::time::Duration::from_millis(500),
            target_reached,
        };
        assert_eq!(
            source_probe_sample_outcome(sample(false)),
            TraceOutcome::Inconclusive,
            "a right-censored non-empty prefix cannot claim the requested sample completed",
        );
        assert_eq!(
            source_probe_sample_outcome(sample(true)),
            TraceOutcome::Succeeded,
        );
    }

    /// The worker may finish a bounded response after a concurrent quality change has installed a
    /// different HLS resource.  Bytes charged to the old identity are not evidence for the new
    /// route: keep the replacement intact and discard the completed sample.
    #[test]
    fn a_source_sample_from_a_superseded_hls_resource_is_discarded() {
        use std::io::{BufRead, BufReader, Write};

        let _g = fresh_registry();
        if !crate::net::global_init() || !crate::curlio::available() {
            return;
        }
        let plan = crate::abr::source_probe_plan(320, crate::abr::PROBE_BUDGET_MS).unwrap();
        let probe_bytes = plan.target_bytes;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port() as i32;
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept source request");
            let mut reader = BufReader::new(socket.try_clone().expect("clone socket"));
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("request line or header");
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            install_active_hls(
                "probe-new",
                "http://fixture.invalid/new.m3u8",
                crate::abr::Rung::P720,
            );
            write!(
                socket,
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                probe_bytes - 1,
                probe_bytes * 2,
                probe_bytes,
            )
            .expect("source headers");
            socket
                .write_all(&vec![0x55; probe_bytes])
                .expect("source body");
        });

        let sid = crate::plex::register_for_test(
            "probe-stale",
            "127.0.0.1",
            port,
            "tok",
            "cid-probe-stale",
        );
        let active = "probe-old";
        install_active_hls(
            active,
            "http://fixture.invalid/old.m3u8",
            crate::abr::Rung::P480,
        );
        let active_route = worker_ticket();
        let control = HlsAbrControl {
            trace_generation: 0,
            sid,
            rating_key: "1".into(),
            logical_session: "probe-logical".into(),
            audio_stream_id: 0,
            subtitle_stream_id: 0,
            seconds_per_segment: 2,
            initial_rung: crate::abr::Rung::P480,
            initial_observed: None,
            fixture_base: String::new(),
            original_probe_part: "/library/parts/1/file.mkv".into(),
            original_source_kbps: 320,
            catalog: crate::abr::HlsActuatorCatalog::measured(),
            prior: None,
            history: crate::abr::TransitionHistory::default(),
            original_features: crate::abr::SourceFeatures::default(),
        };

        assert_eq!(
            control.probe_original_while_hls(&active_route, plan),
            OriginalProbeResult::Stale,
        );
        assert_eq!(
            active_encoder(),
            "probe-new",
            "the concurrent replacement wins"
        );

        server.join().unwrap();
        install_active_encoder("");
        crate::plex::reset_servers_for_test();
        reset_session();
    }

    /// Cold Auto measures the Part under the playback's durable logical owner.  The bounded read
    /// must not manufacture a `source-N` identity or exact-close the resource before the selected
    /// Original/HLS route can reuse it.
    #[test]
    fn cold_source_preflight_uses_the_playback_identity_and_does_not_close_it() {
        use std::io::{BufRead, BufReader, Write};

        let _g = fresh_registry();
        if !crate::net::global_init() || !crate::curlio::available() {
            return;
        }
        let plan = crate::abr::source_probe_plan(320, crate::abr::PROBE_BUDGET_MS).unwrap();
        let probe_bytes = plan.target_bytes;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port() as i32;
        let (tx, rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept cold source request");
            let mut reader = BufReader::new(socket.try_clone().expect("clone socket"));
            let mut requests = Vec::new();
            let mut first = String::new();
            reader.read_line(&mut first).expect("request line");
            requests.push(first);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("request header");
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                requests.push(line);
            }
            write!(
                socket,
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                probe_bytes - 1,
                probe_bytes * 2,
                probe_bytes,
            )
            .expect("source headers");
            socket
                .write_all(&vec![0x55; probe_bytes])
                .expect("source body");
            drop(socket);

            listener.set_nonblocking(true).unwrap();
            for _ in 0..50 {
                match listener.accept() {
                    Ok((socket, _)) => {
                        let mut extra = String::new();
                        BufReader::new(socket)
                            .read_line(&mut extra)
                            .expect("extra request line");
                        requests.push(extra);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(4));
                    }
                    Err(error) => panic!("accept extra request: {error}"),
                }
            }
            tx.send(requests).expect("publish cold request set");
        });

        let sid = crate::plex::register_for_test(
            "probe-cold",
            "127.0.0.1",
            port,
            "tok",
            "cid-probe-cold",
        );
        let client = crate::plex::client_for(sid).expect("test server installed");
        let sample =
            measure_remote_original(client, "/library/parts/1/file.mkv", "cold-logical", 320)
                .expect("completed cold sample");
        assert!(sample.completed);

        let requests = rx.recv().expect("captured cold requests");
        assert_eq!(
            requests
                .iter()
                .filter(|line| line.starts_with("GET ") || line.starts_with("POST "))
                .count(),
            1,
            "the preflight is one bounded Part request and no exact-close: {requests:?}",
        );
        assert!(requests[0].starts_with("GET /library/parts/1/file.mkv?"));
        assert!(requests[0].contains("X-Plex-Session-Identifier=cold-logical"));
        assert!(
            requests
                .iter()
                .any(|line| line.to_ascii_lowercase().starts_with("range: bytes=0-")),
            "the logical resource is sampled with one finite response",
        );

        server.join().unwrap();
        crate::plex::reset_servers_for_test();
        reset_session();
    }

    /// What `build_stream` computes, spelled once.
    fn allowed(
        link: Option<crate::plex::probe::Location>,
        q: Quality,
        src: (i64, i64, i64),
    ) -> crate::plex::LinkPolicy {
        let auto_original = q == Quality::Auto && link == Some(crate::plex::probe::Location::Local);
        flavors_allowed(
            crate::plex::link_policy(link),
            quality_policy(q, auto_original, src.0, src.1, src.2),
        )
    }

    /// **GATE 1 — Original changes nothing, for any source, on any link.** It is the migration and
    /// readiness fallback: a ceiling that leaked into it would change every existing install.
    /// Note the unmeasured row in particular — `Ceiling::admits` fails CLOSED, and that rule must
    /// not be reachable at all without a fixed rung selected.
    #[test]
    fn original_is_unchanged_and_auto_original_is_an_explicit_measured_state() {
        for src in [UHD_REMUX, HD_BIG, HD_SMALL, UNMEASURED] {
            assert_eq!(
                quality_policy(Quality::Original, false, src.0, src.1, src.2),
                crate::plex::LinkPolicy::UNRESTRICTED,
                "Original must restrict nothing, and {src:?} is not an exception"
            );
            assert_eq!(
                quality_policy(Quality::Auto, false, src.0, src.1, src.2),
                crate::plex::LinkPolicy {
                    direct_play: false,
                    remux: false
                },
                "Auto without a positive Original measurement must use HLS"
            );
            assert_eq!(
                quality_policy(Quality::Auto, true, src.0, src.1, src.2),
                crate::plex::LinkPolicy::UNRESTRICTED,
                "Auto's proven Original state must not start an encoder"
            );
            // …and composed, on every link tier, Original is exactly what the link alone said.
            for link in [
                None,
                Some(crate::plex::probe::Location::Local),
                Some(crate::plex::probe::Location::Remote),
                Some(crate::plex::probe::Location::Relay),
            ] {
                assert_eq!(
                    allowed(link, Quality::Original, src),
                    crate::plex::link_policy(link),
                    "Original changed the answer for link {link:?} on {src:?}"
                );
            }
        }
        // Neither mode carries a fixed ceiling. The parameter half of Original's claim remains
        // the transcoder test that a `None` ceiling produces the pre-ceiling literals.
        assert_eq!(Quality::Auto.ceiling(), None);
        assert_eq!(Quality::Original.ceiling(), None);
    }

    #[test]
    fn auto_is_available_only_on_the_positive_readiness_side() {
        assert_eq!(quality_ladder_for(false).first(), Some(&Quality::Original));
        assert!(!quality_ladder_for(false).contains(&Quality::Auto));
        assert_eq!(quality_ladder_for(true), &QUALITY_LADDER);
        assert_eq!(
            quality_ladder_for(true)[..2],
            [Quality::Auto, Quality::Original]
        );
        assert!(
            auto_quality_ready(),
            "the integrated HLS prime/swap path owns production Auto"
        );
        assert_eq!(supported_quality(Quality::Auto), Quality::Auto);
    }

    /// The cold-start admission rule now lives in `abr::bootstrap`, and this grades the composition
    /// this file is responsible for: a curl sample turned into an observation, and the LINK CLASS
    /// deciding whether the probe is consulted at all.  The boundary is conservation, not an
    /// arbitrary headroom multiplier: a completed prefix is sustainable exactly when its arrival
    /// rate is at least the source consumption rate.
    #[test]
    fn remote_original_uses_the_completed_source_conservation_test() {
        let policy = crate::abr::AbrPolicy::measured();
        let catalog = crate::abr::HlsActuatorCatalog::measured();
        let observation = |bytes: u64, ms: u64, complete: bool| crate::abr::CapacityObservation {
            kbps: u32::try_from(
                crate::curlio::ThroughputSample {
                    bytes,
                    elapsed: std::time::Duration::from_millis(ms),
                    target_reached: complete,
                }
                .kbps(),
            )
            .unwrap(),
            bytes: bytes as u64,
            active_us: ms * 1_000,
            completed: complete,
        };
        let fast = observation(1_000_000, 500, true);
        assert_eq!(fast.kbps, 16_000);
        let go = |source, probe| {
            crate::abr::bootstrap(
                crate::abr::LinkKind::Remote,
                true,
                source,
                Some(probe),
                &catalog,
                &policy,
            )
            .original
        };
        assert!(go(10_000, fast));
        assert!(
            go(10_000, observation(1_000_000, 800, true)),
            "a completed 12.5 Mbit/s prefix sustains a 10 Mbit/s source without a hidden margin"
        );
        assert!(
            !go(10_000, observation(1_000_000, 801, true)),
            "a completed prefix just below 10 Mbit/s does not sustain that source"
        );
        assert!(
            !go(10_000, observation(1_000_000, 500, false)),
            "a truncated probe proves a floor"
        );
        assert!(
            !go(0, fast),
            "an unknown source bitrate cannot be reasoned about"
        );
        // ...and neither of the other two link classes consults a probe at all.
        for link in [crate::abr::LinkKind::Local, crate::abr::LinkKind::Relay] {
            let decision = crate::abr::bootstrap(link, true, 10_000, None, &catalog, &policy);
            assert_eq!(decision.original, link == crate::abr::LinkKind::Local);
        }
    }

    #[test]
    fn remote_probe_samples_one_second_but_has_strict_memory_bounds() {
        assert_eq!(remote_probe_target_bytes(0), None);
        assert_eq!(
            remote_probe_target_bytes(720),
            Some(crate::abr::SOURCE_PROBE_MIN_BYTES),
        );
        assert_eq!(remote_probe_target_bytes(8_000), Some(1_000_000));
        assert_eq!(
            remote_probe_target_bytes(200_000),
            Some(crate::abr::SOURCE_PROBE_MAX_BYTES),
        );
    }

    /// **GATE 2 — under-ceiling content keeps the fast paths.** Picking "1080p · 8 Mbps" must not
    /// send a 3 Mbit/s 720p episode to an encoder: there is nothing there for a transcode to fix,
    /// and doing it anyway would cost the server a job and the picture a generation. This is the
    /// assertion that stops the feature from degenerating into "a rung means always transcode".
    #[test]
    fn a_source_measured_under_the_ceiling_stays_direct_play_eligible() {
        let p = allowed(None, Quality::P1080, HD_SMALL);
        assert!(
            p.direct_play,
            "3 Mbps 720p is under 8 Mbps 1080p — nothing to fix"
        );
        assert!(
            p.remux,
            "…and a container remux of it is under the ceiling too"
        );
        // true right down the ladder, until the rung actually bites
        assert!(
            allowed(None, Quality::P720, HD_SMALL).direct_play,
            "3 Mbps 720p fits 4 Mbps 720p"
        );
        assert!(
            !allowed(None, Quality::P720Low, HD_SMALL).direct_play,
            "…but not 2 Mbps"
        );
    }

    /// **GATE 3 — over-ceiling loses DIRECT PLAY, and this is the whole point.** A 30 Mbit/s 1080p
    /// file is the case a bitrate field on `TranscodeSpec` cannot touch: direct play streams the
    /// file's own bytes and no encoder ever reads the number. Refusing the flavor is the only
    /// thing that makes a cap mean anything.
    ///
    /// Both axes refuse independently — over on RATE alone (the 1080p file against a 1080p rung)
    /// and over on FRAME alone (a 4K source against a 1080p rung, at a rate the rung allows).
    #[test]
    fn a_source_over_the_ceiling_is_refused_direct_play() {
        assert!(
            !allowed(None, Quality::P1080, HD_BIG).direct_play,
            "30 Mbps is over the 8 Mbps rung"
        );
        assert!(
            !allowed(None, Quality::P1080, (4000, 3840, 2160)).direct_play,
            "4K is over a 1080p rung"
        );
        // …and the unmeasured source fails CLOSED, which is the rule that makes a rung mean
        // something on a play from a shelf that never loaded a detail page.
        assert!(!allowed(None, Quality::P1080, UNMEASURED).direct_play,
            "an unmeasured source cannot be PROVEN under the ceiling, so it takes the branch that applies one");
    }

    /// **GATE 4 — over-ceiling loses the REMUX too**, and this is the half a "force a transcode"
    /// instinct leaves behind, because a remux *feels* like a concession already. It is not: it
    /// copies the codecs and its query deliberately carries no cap, so it is the same 30 Mbit/s
    /// one container down. `link_policy` states this for the relay; a user ceiling inherits it
    /// unchanged, and what survives is the re-encode.
    #[test]
    fn a_source_over_the_ceiling_is_refused_the_remux_as_well() {
        let p = allowed(None, Quality::P1080, HD_BIG);
        assert!(
            !p.remux,
            "a remux is the same bytes at the same rate, one layer down"
        );
        assert_eq!(
            p,
            crate::plex::LinkPolicy {
                direct_play: false,
                remux: false
            }
        );
        // A 4K remux — the flavor that exists to keep 4K/HDR intact — is exactly what a low rung
        // has to refuse, or the rung buys nothing on the biggest files in the library.
        assert!(!allowed(None, Quality::P720, UHD_REMUX).remux);
    }

    /// **GATE 6 — the link's policy and the user's compose to the STRICTER, per flavor.** A relay
    /// must not be loosened by picking a high rung (the tunnel is 2 Mbit/s whatever the user
    /// thinks), and a low rung must not be loosened by a fast LAN link. Graded as a full product
    /// of both axes rather than one example, because a `||` typed for a `&&` passes any single
    /// case that happens to agree.
    #[test]
    fn a_relay_link_and_a_user_ceiling_compose_to_the_stricter_of_the_two() {
        for q in QUALITY_LADDER {
            for src in [UHD_REMUX, HD_BIG, HD_SMALL, UNMEASURED] {
                // relay denies both, and NOTHING a user can pick gives either back
                assert_eq!(
                    allowed(Some(crate::plex::probe::Location::Relay), q, src),
                    crate::plex::LinkPolicy {
                        direct_play: false,
                        remux: false
                    },
                    "a relay was loosened by rung {q:?} on {src:?}"
                );
                // and on an unrestricted link the answer is the user's policy, unchanged
                for link in [
                    None,
                    Some(crate::plex::probe::Location::Local),
                    Some(crate::plex::probe::Location::Remote),
                ] {
                    let auto_original =
                        q == Quality::Auto && link == Some(crate::plex::probe::Location::Local);
                    assert_eq!(
                        allowed(link, q, src),
                        quality_policy(q, auto_original, src.0, src.1, src.2),
                        "link {link:?} altered rung {q:?} on {src:?}"
                    );
                }
            }
        }
    }

    // ---- the two reads that FEED the ceiling: which detail describes the leaf, and at what rate

    /// **Press Play on a SHOW page and the detail's `rk` is the show's, not the episode's.** An
    /// rk-only test therefore missed on the commonest path in the app, `src_kbps` fell to 0, and
    /// `Ceiling::admits` fails closed — so with any rung selected every episode in the library
    /// lost direct play, while `playback_preview` (reading the same `Detail`'s numbers directly)
    /// still promised Direct Play for it. Two answers to one question.
    ///
    /// The server half is graded on both arms: a ratingKey names an item only within one server.
    #[test]
    fn the_loaded_detail_describes_its_own_key_and_its_on_deck_episodes() {
        let a = crate::plex::ServerId::from_raw(1);
        let b = crate::plex::ServerId::from_raw(2);
        let show = crate::metadata::Detail {
            sid: a,
            rk: "100".into(),
            on_deck: Some(crate::metadata::Episode {
                rk: "205".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(detail_describes(&show, a, "100"), "its own key");
        assert!(
            detail_describes(&show, a, "205"),
            "the episode Play would actually start"
        );
        assert!(
            !detail_describes(&show, a, "206"),
            "a different episode is not this one"
        );
        // …and neither key may match across servers, or the ceiling judges the wrong file
        assert!(!detail_describes(&show, b, "100"));
        assert!(!detail_describes(&show, b, "205"));
        // a movie has no on-deck episode and must still answer for itself
        let movie = crate::metadata::Detail {
            sid: a,
            rk: "7".into(),
            ..Default::default()
        };
        assert!(detail_describes(&movie, a, "7"));
        assert!(!detail_describes(&movie, a, "100"));
    }

    /// **The ceiling is spent as `maxVideoBitrate`, so it must be judged against the VIDEO rate.**
    /// `Detail::bitrate` is the whole-file figure — video plus every audio track — and comparing
    /// that against a video-only cap makes each rung bite about one AC-3 track early. The video
    /// stream's own number is preferred where PMS sent one; the whole-file figure is the fallback,
    /// which is the conservative direction and so the right one.
    #[test]
    fn the_source_rate_is_the_video_streams_own_where_the_server_gave_one() {
        let with_video = crate::metadata::Detail {
            bitrate: 8540, // 7900 video + a 640 kbps AC-3 track
            video: Some(crate::metadata::Stream {
                bitrate: 7900,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(source_kbps(&with_video), 7900);
        // …which is what keeps it under an 8 Mbps rung its VIDEO does in fact fit
        assert!(
            quality_policy(Quality::P1080, false, source_kbps(&with_video), 1920, 1080).direct_play
        );
        assert!(
            !quality_policy(Quality::P1080, false, with_video.bitrate, 1920, 1080).direct_play,
            "the whole-file figure is what made the rung bite early — this is the bug, pinned"
        );

        // no video record (a show with no episode backfill, an audio-only part) → whole-file
        let bare = crate::metadata::Detail {
            bitrate: 8540,
            ..Default::default()
        };
        assert_eq!(source_kbps(&bare), 8540);
        // a video record PMS gave no bitrate for is not a measurement of 0 — fall back
        let unmeasured_stream = crate::metadata::Detail {
            bitrate: 8540,
            video: Some(crate::metadata::Stream::default()),
            ..Default::default()
        };
        assert_eq!(source_kbps(&unmeasured_stream), 8540);
        // nothing said at all stays 0, which `Ceiling::admits` fails closed on
        assert_eq!(source_kbps(&crate::metadata::Detail::default()), 0);
    }

    // ---- pick_dp_audio: the direct-play audio selection ladder ------------------------------
    // Never host-testable before: it read `metadata::playing()`'s `&'static` store. Making it
    // take the tracks explicitly (step 6 of docs/async-model-decision.md) turned the ladder into
    // a pure function, and these pin the order the comments claim.

    fn trk(id: i64, codec: &str, lang: &str, default: bool) -> crate::metadata::Stream {
        // `..Default::default()` for the rest, which is what that derive is FOR (see the comment
        // above `metadata::Stream`): this ladder is about id / codec / language / default, and a
        // fixture that spells out the technical fields it does not read would have to be revisited
        // every time the Track-information panel learns another one.
        crate::metadata::Stream {
            id,
            index: id,
            lang_code: lang.into(),
            codec: codec.into(),
            channels: 2,
            default,
            ..Default::default()
        }
    }

    /// Mark a track as the server's CURRENT pick (PMS `Stream.selected`) — the flag a pick made
    /// on a phone / Plex Web / another TV arrives on.
    fn server_selected(mut s: crate::metadata::Stream) -> crate::metadata::Stream {
        s.selected = true;
        s
    }

    /// A subtitle stream, spelled out because the ordinal maths depends on `index` (container
    /// order, which PMS may report out of document order) and on `external` (sidecars are not in
    /// the container at all, so the client renderer cannot count them).
    fn sub(id: i64, index: i64, lang: &str, external: bool) -> crate::metadata::Stream {
        crate::metadata::Stream {
            index,
            external,
            ..trk(id, "srt", lang, false)
        }
    }

    #[test]
    fn an_empty_track_list_falls_back_to_the_codec_default() {
        assert_eq!(
            pick_dp_audio(&[], "ac3").map(|(i, c, _)| (i, c)),
            Some((-1, "ac3".into()))
        );
        assert!(
            pick_dp_audio(&[], "truehd").is_none(),
            "a non-direct-playable default must transcode"
        );
    }

    #[test]
    fn english_wins_over_the_files_default_track() {
        // The Office ships a Russian "kubik" track flagged default; we must not open in it.
        let tracks = [trk(1, "ac3", "rus", true), trk(2, "ac3", "eng", false)];
        assert_eq!(pick_dp_audio(&tracks, "ac3"), Some((1, "ac3".into(), 2)));
    }

    #[test]
    fn the_flagged_default_wins_when_no_english_track_is_direct_playable() {
        let tracks = [trk(1, "ac3", "deu", false), trk(2, "ac3", "fra", true)];
        assert_eq!(pick_dp_audio(&tracks, "ac3"), Some((1, "ac3".into(), 2)));
    }

    #[test]
    fn smart_dp_takes_a_playable_sibling_over_a_non_playable_default() {
        // A 4K HEVC item: TrueHD default + an AC3 sibling — direct-play beats the server's
        // video-downscaling transcode.
        let tracks = [trk(1, "truehd", "eng", true), trk(2, "ac3", "eng", false)];
        assert_eq!(pick_dp_audio(&tracks, "truehd"), Some((1, "ac3".into(), 2)));
    }

    #[test]
    fn no_direct_playable_track_means_transcode() {
        let tracks = [trk(1, "truehd", "eng", true), trk(2, "dts", "eng", false)];
        assert!(pick_dp_audio(&tracks, "truehd").is_none());
    }

    // ---- rung 1: the selection the SERVER already holds --------------------------------------
    // `Stream.selected` is the part's current pick — what `put_selection` writes and what a pick
    // made on a phone / Plex Web / another TV shows up as. We wrote it for a long time and never
    // read it, so our own ladder silently overwrote every cross-client choice on the next play.
    // The shapes below are the ones the live server actually serves (probed per-identity while
    // this landed), which is where the two gates on the rung come from.

    #[test]
    fn the_servers_selected_track_outranks_the_english_preference() {
        // A user picks the second Russian dub on their phone. English is still the
        // FIRST direct-playable track, so the old ladder handed back English on every play.
        let tracks = [
            trk(2693, "ac3", "rus", true),
            server_selected(trk(2694, "ac3", "rus", false)),
            trk(2695, "ac3", "eng", false),
        ];
        assert_eq!(pick_dp_audio(&tracks, "ac3"), Some((1, "ac3".into(), 2694)));
    }

    #[test]
    fn a_selection_that_only_echoes_the_files_default_does_not_beat_english() {
        // THE gate that keeps the English rung alive. PMS reports a selected audio stream on
        // every part — for one nobody has touched it is just the container's default flag coming
        // back (The Morning Show: the Russian default reads `selected`). Treating that as a
        // choice would reinstate exactly the foreign-dub-on-open bug rung 2 exists to prevent.
        let tracks = [
            server_selected(trk(10975, "eac3", "rus", true)),
            trk(10976, "eac3", "eng", false),
        ];
        assert_eq!(
            pick_dp_audio(&tracks, "eac3"),
            Some((1, "eac3".into(), 10976))
        );
    }

    #[test]
    fn a_selected_track_that_cannot_direct_play_falls_through_to_the_ladder() {
        // A live shape off the server: it holds the English DTS track (a real pick — it is
        // not the file default), which this pipeline cannot decode. Honouring it would force a
        // whole-video transcode for one audio track, so the ladder runs on instead.
        let tracks = [
            trk(2663, "ac3", "rus", true),
            server_selected(trk(2669, "dca", "eng", false)),
            trk(2673, "ac3", "eng", false),
        ];
        assert_eq!(pick_dp_audio(&tracks, "dca"), Some((2, "ac3".into(), 2673)));
    }

    /// The whole ladder, rung by rung, with the selected flag switched on and off — the order is
    /// the contract, and every row here is a shape the live server actually serves.
    #[test]
    fn the_audio_ladder_walks_its_rungs_in_order() {
        let cases: [(
            &str,
            Vec<crate::metadata::Stream>,
            &str,
            Option<(i32, String, i64)>,
        ); 7] = [
            (
                "rung 1: a real server pick wins even against English",
                vec![
                    trk(1, "eac3", "rus", true),
                    server_selected(trk(2, "eac3", "deu", false)),
                    trk(3, "eac3", "eng", false),
                ],
                "eac3",
                Some((1, "eac3".into(), 2)),
            ),
            (
                "rung 1 needs a real pick: the default echoed back is not one",
                vec![
                    server_selected(trk(1, "eac3", "rus", true)),
                    trk(2, "eac3", "eng", false),
                ],
                "eac3",
                Some((1, "eac3".into(), 2)),
            ),
            (
                "rung 1 is skipped when the pick can't direct-play, not obeyed by transcoding",
                vec![
                    trk(1, "ac3", "rus", true),
                    server_selected(trk(2, "dca", "eng", false)),
                    trk(3, "ac3", "eng", false),
                ],
                "ac3",
                Some((2, "ac3".into(), 3)), // rung 2 (English) still applies
            ),
            (
                "rung 2: no selection at all → the English preference, as before",
                vec![trk(1, "ac3", "rus", true), trk(2, "ac3", "eng", false)],
                "ac3",
                Some((1, "ac3".into(), 2)),
            ),
            (
                "rung 3: no English → the file's flagged default",
                vec![trk(1, "ac3", "deu", false), trk(2, "ac3", "fra", true)],
                "ac3",
                Some((1, "ac3".into(), 2)),
            ),
            (
                "rung 4: a selected non-DP track with only a foreign DP sibling — smart-DP",
                vec![
                    server_selected(trk(1, "truehd", "eng", false)),
                    trk(2, "ac3", "fra", false),
                ],
                "truehd",
                Some((1, "ac3".into(), 2)),
            ),
            (
                "nothing direct-playable, selected or not → transcode",
                vec![
                    server_selected(trk(1, "truehd", "eng", false)),
                    trk(2, "dts", "rus", true),
                ],
                "truehd",
                None,
            ),
        ];
        for (what, tracks, acodec, want) in cases {
            assert_eq!(pick_dp_audio(&tracks, acodec), want, "{what}");
        }
    }

    // ---- pick_dp_subtitle: the read-back half of put_selection -------------------------------

    #[test]
    fn the_selected_subtitle_resolves_to_the_renderers_embedded_ordinal() {
        // Document order is NOT container order and a sidecar sits in the middle of the list:
        // the renderer counts only embedded streams, sorted on PMS `Stream.index` — the same
        // identifier space the track menu commits (metadata::sub_render_ordinal).
        let subs = [
            sub(10, 7, "fra", true),  // sidecar — not in the container, not counted
            sub(11, 3, "rus", false), // embedded, container-first
            server_selected(sub(12, 4, "eng", false)),
        ];
        assert_eq!(pick_dp_subtitle(&subs), Some((12, 1)));
    }

    #[test]
    fn an_external_selected_subtitle_is_left_off() {
        // A sidecar can only be shown by a server burn; forcing a transcode to obey a stored
        // flag is not a trade the user asked for, so the direct-play path leaves subs off.
        let subs = [
            server_selected(sub(10, 3, "eng", true)),
            sub(11, 4, "rus", false),
        ];
        assert_eq!(pick_dp_subtitle(&subs), None);
    }

    #[test]
    fn no_selected_subtitle_means_subtitles_stay_off() {
        assert_eq!(pick_dp_subtitle(&[]), None);
        let subs = [sub(10, 3, "eng", false), sub(11, 4, "rus", false)];
        assert_eq!(
            pick_dp_subtitle(&subs),
            None,
            "the file's own tracks are not an instruction"
        );
    }

    #[test]
    fn a_selection_with_no_stream_id_is_left_off_rather_than_half_applied() {
        // id and ordinal travel together: the id is what the menu checkmark and the timeline
        // report key on, so an id-less stream would render subtitles while the menu said Off.
        let subs = [server_selected(sub(0, 3, "eng", false))];
        assert_eq!(pick_dp_subtitle(&subs), None);
    }

    // ---- video_direct_plays: the local codec + resolution + Dolby Vision direct-play gate ----

    use crate::metadata::{Dovi, DvPresentation};

    /// The two settings of the `/tmp/plxnative-dv` trigger, named so every assertion below says
    /// which world it is in. `DECLARED` is the armed one — the pipeline is told the stream is
    /// Dolby Vision — and `SILENT` is a build (or a boot) that sends no node, which is also what
    /// `RELEASE=1` compiles in today.
    const DECLARED: bool = true;
    const SILENT: bool = false;

    /// An ordinary non-DV file: every DOVI field absent, which is what PMS sends for one.
    fn no_dv() -> Dovi {
        Dovi::default()
    }
    /// The four real shapes, spelled exactly as the dev server reports them (probed live
    /// 2026-08-21 by sweeping all 540 movies and episodes on the dev PMS: 28 carry Dolby Vision,
    /// 8 movies and 20 episodes — the numbers are not invented, and `p7`'s `bl_compat: 6` in
    /// particular is why an `== 0` test is not enough).
    fn p5() -> Dovi {
        Dovi {
            present: true,
            profile: 5,
            bl_compat: 0,
            el_present: false,
            ..Dovi::NONE
        }
    }
    fn p7() -> Dovi {
        Dovi {
            present: true,
            profile: 7,
            bl_compat: 6,
            el_present: true,
            ..Dovi::NONE
        }
    }
    fn p8() -> Dovi {
        Dovi {
            present: true,
            profile: 8,
            bl_compat: 1,
            el_present: false,
            ..Dovi::NONE
        }
    }

    /// **The bug this gate exists for.** Profile 5 is single-layer IPT-PQ with no HDR10 fallback,
    /// so feeding its base layer to an ordinary HEVC decoder produces a picture in visibly wrong
    /// colours — and nothing else in the ladder can see that: the codec is `hevc` (fine), the
    /// frame size clears the dev TV's bound (fine), the container is mp4, which has direct-played
    /// since 2026-08-11 (fine). Every gate passes and the user gets a broken picture.
    #[test]
    fn a_profile_5_source_does_not_direct_play_undeclared() {
        let caps = crate::devcaps::Caps {
            hevc: true,
            hevc_max: (4096, 2176), // the dev TV's own bound — this must fail on SIZE grounds nowhere
            vp9: false,
            audio: "aac,ac3,eac3".into(),
        };
        // the live P5 item's own shape: 3840x1602 hevc, well inside the bound
        assert!(
            !video_direct_plays("hevc", 3840, 1602, p5().presentation(SILENT), &caps),
            "IPT-PQ has no HDR10 base layer"
        );
        // and it is the DV fields doing it, not the size or the codec: the same file without them
        // direct-plays, which is exactly the behaviour that shipped the wrong colours
        assert!(video_direct_plays(
            "hevc",
            3840,
            1602,
            no_dv().presentation(SILENT),
            &caps
        ));
    }

    /// **The inversion, and the reason the refusal above is now conditional.** Declaring the
    /// stream — one `DolbyHdrInfo` node in the Load payload — is what makes the pipeline set
    /// `dolby-vision=TRUE` on the caps it builds, and a Profile 5 shown in Dolby Vision mode is
    /// the correct picture rather than the wrong one. So the same file, same size, same codec,
    /// direct-plays once we are willing to say what it is; the refusal was never about the
    /// decoder, only about our own silence.
    #[test]
    fn declaring_dolby_vision_inverts_the_profile_5_refusal() {
        let caps = crate::devcaps::Caps {
            hevc: true,
            hevc_max: (4096, 2176),
            vp9: false,
            audio: "aac,ac3,eac3".into(),
        };
        let dv = p5().presentation(DECLARED);
        assert!(
            video_direct_plays("hevc", 3840, 1602, dv, &caps),
            "a declared P5 is displayable"
        );
        let n = dv
            .declared()
            .expect("the payload must carry the node the gate was opened for");
        assert_eq!(
            n.profile_id, 5,
            "getInt, and the pipeline's -1 sentinel means no profile hint"
        );
        assert_eq!(n.track_type, "single");
        assert_eq!(n.encryption_type, "clear");
        // ...and the size and codec halves of the gate are untouched by any of it
        assert!(!video_direct_plays("av1", 3840, 1602, dv, &caps));
        let small = crate::devcaps::Caps {
            hevc_max: (1920, 1088),
            ..caps.clone()
        };
        assert!(!video_direct_plays("hevc", 3840, 1602, dv, &small));
    }

    /// Profile 7 is dual-layer: the picture is split across a base and an enhancement layer, and
    /// the pipeline feeds ONE elementary stream. Caught by `el_present` alone — the live P7 item
    /// reports `bl_compat = 6`, so a compatibility-id test would wave it straight through.
    #[test]
    fn a_dual_layer_profile_7_source_does_not_direct_play() {
        let caps = crate::devcaps::Caps {
            hevc: true,
            hevc_max: (4096, 2176),
            vp9: false,
            audio: "eac3".into(),
        };
        // and it is refused in BOTH worlds: no payload key can hand the pipeline a layer we do
        // not feed it, so arming the trigger must not open this gate the way it opens P5's
        for signal in [SILENT, DECLARED] {
            let dv = p7().presentation(signal);
            assert!(
                !video_direct_plays("hevc", 3840, 2160, dv, &caps),
                "signal={signal}"
            );
            assert_eq!(dv.refusal(), Some("dual-layer"));
            assert_eq!(
                dv.declared(),
                None,
                "a layer we cannot feed must never be declared"
            );
        }
        assert_ne!(
            p7().bl_compat,
            0,
            "the fixture must keep the trap it was built to hold"
        );
    }

    /// **Profile 8.1 must be UNAFFECTED**, and so must every file with no DOVI record at all.
    /// P8's base layer IS an HDR10 stream, so ignoring the RPU costs the dynamic metadata and
    /// nothing else — the 21-case on-device suite includes a passing P8 case (`dp_hevc_eac3_dovi_p8`)
    /// and this change must not move it.
    #[test]
    fn profile_8_and_plain_files_are_unaffected() {
        let caps = crate::devcaps::Caps {
            hevc: true,
            hevc_max: (4096, 2176),
            vp9: false,
            audio: "aac,ac3,eac3".into(),
        };
        for signal in [SILENT, DECLARED] {
            assert!(
                video_direct_plays("hevc", 3840, 2160, p8().presentation(signal), &caps),
                "HDR10-compatible base layer (signal={signal})"
            );
            assert!(video_direct_plays(
                "hevc",
                3840,
                2160,
                no_dv().presentation(signal),
                &caps
            ));
            assert!(video_direct_plays(
                "h264",
                1920,
                1080,
                no_dv().presentation(signal),
                &caps
            ));
            assert_eq!(p8().presentation(signal).refusal(), None);
            assert_eq!(no_dv().presentation(signal).refusal(), None);
        }
        // A file with no Dolby Vision at all declares nothing however the trigger is set — the
        // node is a statement about the stream, not a mode the app is in.
        assert_eq!(no_dv().presentation(DECLARED).declared(), None);
        // P8 declares in BOTH settings, and that is deliberate: its base layer is HDR10 either
        // way, so the node costs nothing and adds the dynamic metadata the RPU carries. The
        // trigger reaches only the profile whose declaration is not yet free — P5, measured to
        // lose two frames every ~40 s on this set. `SILENT` here is the half that would silently
        // regress if the gate were ever rewritten as a bare `signal &&`.
        for signal in [SILENT, DECLARED] {
            assert_eq!(
                p8().presentation(signal).declared().map(|n| n.profile_id),
                Some(8),
                "a cross-compatible base layer declares without the trigger: signal={signal}"
            );
        }
        assert_eq!(
            p5().presentation(SILENT).declared(),
            None,
            "P5 stays behind the trigger"
        );
    }

    /// **Silence must not convict.** Every field of `Dovi` is 0 both when the server omits it and
    /// when the file simply is not Dolby Vision, so a bare `bl_compat == 0` test would refuse
    /// direct play for the entire library. Two guards keep that from happening, and this drives
    /// both: `present` gates the whole question, and a KNOWN profile gates the compat-id test.
    /// The direction is deliberate — a false refusal costs 4K and HDR10 on a file that played
    /// perfectly, and on a Pass-less server (issue #22) it costs playback outright.
    #[test]
    fn an_unreported_dolby_vision_record_refuses_nothing() {
        // the shape every ordinary SDR file has: no DV at all, so bl_compat 0 means nothing
        assert!(!Dovi::default().base_layer_unusable());
        // `DOVIPresent` and nothing else — an older or quieter server. Not enough to convict.
        let bare = Dovi {
            present: true,
            profile: 0,
            bl_compat: 0,
            el_present: false,
            ..Dovi::NONE
        };
        assert!(
            !bare.base_layer_unusable(),
            "a compat id of 0 read out of a silent field is not a 0"
        );
        // but an explicit enhancement layer is disqualifying even with no profile reported,
        // because that field says what it says regardless of what sits beside it
        let el_only = Dovi {
            present: true,
            profile: 0,
            bl_compat: 0,
            el_present: true,
            ..Dovi::NONE
        };
        assert!(el_only.base_layer_unusable());
        // and `present: false` overrides everything — no DV means no DV, whatever noise follows
        let contradictory = Dovi {
            present: false,
            profile: 5,
            bl_compat: 0,
            el_present: true,
            ..Dovi::NONE
        };
        assert!(!contradictory.base_layer_unusable());
        // The rule survives the declaration, in both settings: a bare `present` names no profile,
        // `getInt` has nothing to be given, and a node we cannot fill is not a reason to convict a
        // file that plays. It falls through to `NotDv` — plays as it always has, declares nothing.
        for signal in [SILENT, DECLARED] {
            assert_eq!(Dovi::default().presentation(signal), DvPresentation::NotDv);
            assert_eq!(
                bare.presentation(signal),
                DvPresentation::NotDv,
                "signal={signal}"
            );
            assert_eq!(contradictory.presentation(signal), DvPresentation::NotDv);
            assert_eq!(
                el_only.presentation(signal),
                DvPresentation::Refuse("dual-layer")
            );
        }
    }

    /// **The gate and the payload are one predicate, and this is the property that says so.**
    /// Every shape the server can report, in both trigger settings: whatever the answer, direct
    /// play is allowed exactly when a node will be sent or there was no Dolby Vision to declare,
    /// and refused exactly when there is Dolby Vision we are not declaring. The pair that must
    /// never occur is a direct play with an undeclared DV stream — that IS the wrong-colours bug —
    /// and its mirror, a refusal carrying a node nobody will ever send.
    #[test]
    fn the_direct_play_gate_and_the_payload_node_can_never_disagree() {
        let caps = crate::devcaps::Caps {
            hevc: true,
            hevc_max: (4096, 2176),
            vp9: false,
            audio: "aac,ac3,eac3".into(),
        };
        let bare = Dovi {
            present: true,
            profile: 0,
            bl_compat: 0,
            el_present: false,
            ..Dovi::NONE
        };
        for d in [no_dv(), p5(), p7(), p8(), bare] {
            for signal in [SILENT, DECLARED] {
                let dv = d.presentation(signal);
                let plays = video_direct_plays("hevc", 3840, 1602, dv, &caps);
                assert_eq!(plays, dv.refusal().is_none(), "{d:?} signal={signal}");
                assert!(
                    !(dv.refusal().is_some() && dv.declared().is_some()),
                    "{d:?}"
                );
                // and a refusal always implies the COPY refusal beside it — `build_stream`'s
                // `no_video_copy` reads `base_layer_unusable`, and its log line at the refusal
                // says "(no copy)" in so many words. If a shape could be refused while a copy of
                // it stayed permitted, the item would come back byte-identical from the server.
                if dv.refusal().is_some() {
                    assert!(
                        d.base_layer_unusable(),
                        "a refusal must also withdraw the copy: {d:?}"
                    );
                }
                // **The one that matters, and it is now unconditional.** A direct-played Dolby
                // Vision stream is a DECLARED one — in either trigger setting, for every shape.
                // It reads as a strengthening and it is one: while the trigger gated every
                // declaration this could only be asserted as `== signal`, which quietly permitted
                // the wrong-colours pair for any profile the trigger happened to be off for. Now
                // the only undeclared DV is refused DV, so the implication holds outright.
                if plays && d.present && d.profile > 0 {
                    assert!(dv.declared().is_some(), "{d:?} signal={signal}");
                }
                if let Some(n) = dv.declared() {
                    assert_eq!(n.profile_id, d.profile);
                    // `trackType:"dual"` with `encryptionType:"all"` is what sets the pipeline's
                    // `dv-dual-svp` secure-video-path flag, which this app cannot satisfy. No
                    // input may produce that pair.
                    assert!(
                        !(n.track_type == "dual" && n.encryption_type == "all"),
                        "dv-dual-svp"
                    );
                }
            }
        }
    }

    /// The three profiles, through the predicate itself rather than the gate, including the
    /// 8.2 (SDR base) and 8.4 (HLG base) variants: their base layers are ordinary displayable
    /// pictures, so they direct-play like 8.1 and only the compat id tells them apart.
    #[test]
    fn base_layer_usability_by_profile() {
        assert!(p5().base_layer_unusable());
        assert!(p7().base_layer_unusable());
        assert!(!p8().base_layer_unusable());
        assert_eq!(
            p5().presentation(SILENT).refusal(),
            Some("no cross-compatible base layer")
        );
        for compat in [1, 2, 4] {
            let d = Dovi {
                present: true,
                profile: 8,
                bl_compat: compat,
                el_present: false,
                ..Dovi::NONE
            };
            assert!(
                !d.base_layer_unusable(),
                "P8 with a cross-compatible base layer (id {compat})"
            );
        }
    }

    /// The detail page's preview must agree with what Play will do, or the facts row promises a
    /// direct play the route then refuses. A P5 item reads `Converts` — which is the honest
    /// answer, since a real re-encode is exactly what the server has to do to make it displayable.
    ///
    /// It is a client-side PREDICTION and stops there: `Preview` has no "this server cannot do it"
    /// state, and on the dev PMS a Profile 5 conversion is exactly what comes back refused. The
    /// page says what the route will ASK for; whether the server can answer is the read-out's
    /// question, not this one's.
    #[test]
    fn the_preview_calls_a_profile_5_item_a_conversion() {
        let aac = [crate::metadata::Stream {
            codec: "aac".into(),
            ..Default::default()
        }];
        let part = "/library/parts/1/2/movie.mp4";
        assert_eq!(
            playback_preview_of(part, "hevc", 1920, 1080, p5().presentation(SILENT), &aac),
            Some(Preview::Converts),
            "the server must re-encode it — a container remux would copy the same wrong pixels"
        );
        // the identical item without the DV record is a plain direct play, so the preview is
        // reading the new field and not something else that happens to differ
        assert_eq!(
            playback_preview_of(part, "hevc", 1920, 1080, no_dv().presentation(SILENT), &aac),
            Some(Preview::DirectPlay)
        );
        assert_eq!(
            playback_preview_of(part, "hevc", 1920, 1080, p8().presentation(SILENT), &aac),
            Some(Preview::DirectPlay)
        );
        // and the page must follow the inversion, or the facts row promises a conversion the
        // route no longer performs — the preview reads the same predicate the gate does
        assert_eq!(
            playback_preview_of(part, "hevc", 1920, 1080, p5().presentation(DECLARED), &aac),
            Some(Preview::DirectPlay)
        );
    }

    // ---- video_direct_plays: the local codec + resolution direct-play gate -------------------

    /// The RESOLUTION half of the gate (issue #22's over-claim class): the smart-DP branch never
    /// asks PMS, so the profile's `*`-scoped width/height limitation cannot save a 4K source from
    /// direct-playing onto a 1080p-bounded decoder — the client must refuse it locally. Invisible
    /// on the dev TV (bound 4096x2176); this drives the gate with the reviewer-class caps.
    #[test]
    fn a_source_beyond_the_device_bound_does_not_direct_play() {
        let caps = crate::devcaps::Caps {
            hevc: true,
            hevc_max: (1920, 1088),
            vp9: false,
            audio: "aac,ac3,eac3".into(),
        };
        // the codec agrees; the frame size must still refuse — on either codec
        assert!(!video_direct_plays(
            "h264",
            3840,
            2160,
            no_dv().presentation(SILENT),
            &caps
        ));
        assert!(!video_direct_plays(
            "hevc",
            3840,
            2160,
            no_dv().presentation(SILENT),
            &caps
        ));
        // one axis over is over (per-axis bound, not an area heuristic)
        assert!(!video_direct_plays(
            "h264",
            4096,
            1080,
            no_dv().presentation(SILENT),
            &caps
        ));
        // within the bound plays, exactly at it included (1088 IS the table's number)
        assert!(video_direct_plays(
            "h264",
            1920,
            1088,
            no_dv().presentation(SILENT),
            &caps
        ));
    }

    /// Unknown dimensions fail OPEN (0 = PMS never measured the file — not evidence of 4K, and
    /// yesterday's behavior for it), while the codec half keeps gating regardless.
    #[test]
    fn unknown_dimensions_fail_open_and_the_codec_half_still_gates() {
        let caps = crate::devcaps::Caps {
            hevc: false,
            hevc_max: (1920, 1088),
            vp9: false,
            audio: "aac".into(),
        };
        assert!(video_direct_plays(
            "h264",
            0,
            0,
            no_dv().presentation(SILENT),
            &caps
        ));
        assert!(
            !video_direct_plays("hevc", 1280, 720, no_dv().presentation(SILENT), &caps),
            "no decoder row, no direct play"
        );
        assert!(
            !video_direct_plays("av1", 1280, 720, no_dv().presentation(SILENT), &caps),
            "the pipeline cannot feed it at any size"
        );
    }

    #[test]
    fn part_id_is_read_from_the_parts_segment() {
        assert_eq!(
            part_id_of("/library/parts/98765/1712345678/file.mkv"),
            98765
        );
        assert_eq!(part_id_of("/library/parts/1/0/file.mp4"), 1);
        // a query string rides along on the real keys
        assert_eq!(part_id_of("/library/parts/42/17/file.mkv?download=0"), 42);
    }

    #[test]
    fn part_id_is_zero_when_there_is_no_parts_segment() {
        assert_eq!(part_id_of(""), 0);
        assert_eq!(part_id_of("/library/metadata/1234"), 0);
        assert_eq!(
            part_id_of("/library/parts"),
            0,
            "trailing `parts` with no id"
        );
        assert_eq!(part_id_of("/library/parts/notanumber/file.mkv"), 0);
    }

    /// The direct-play gate: MKV and MP4/M4V parts are fed to the demuxer untouched — everything
    /// else takes the remux branch. mp4 moved sides on 2026-08-11 (issue #22): the mkv-only gate
    /// dated from an unseekable AVIO, and on a server that cannot transcode it turned every mp4
    /// into a failure.
    #[test]
    fn mkv_and_mp4_parts_are_direct_playable() {
        assert!(part_is_streamable("/library/parts/1/2/movie.mkv"));
        assert!(
            part_is_streamable("/library/parts/1/2/movie.mkv?x=1"),
            "the query must not defeat it"
        );
        assert!(part_is_streamable("/library/parts/1/2/movie.mp4"));
        assert!(part_is_streamable("/library/parts/1/2/movie.m4v"));
        assert!(
            !part_is_streamable("/library/parts/1/2/movie.mov"),
            "mov still remuxes"
        );
        assert!(!part_is_streamable(""));
        assert!(
            !part_is_streamable("/library/parts/1/2/mkv.avi"),
            "the extension, not a substring"
        );
        assert!(
            !part_is_streamable("/library/parts/1/2/mp4.avi"),
            "the extension, not a substring"
        );
    }

    /// The preview's THIRD answer, which is the one the UI hangs a Plex Pass claim on.
    ///
    /// While `Preview` had two values, everything that was not a direct play collapsed into
    /// `Converts` — and `detail::play_note` read that as "the server re-encodes the picture", which
    /// is false for the two cases below: `build_stream` answers both of them with
    /// `plan.remux = video_dp`, i.e. ask Plex to copy the codecs into MKV. So a 4K HDR HEVC file in
    /// a `.mov`, and any mkv whose only fault is an audio track that must be converted, drew
    /// "HDR → SDR · tone-mapping needs \[PLEX PASS\]" on a proven-Pass-less server while the picture
    /// arrived HDR10 intact. This grades the SPLIT; the truth table it feeds is `detail.rs`'s.
    ///
    /// The device table is `Caps::assumed` here (nothing in the host suite calls `devcaps::probe`),
    /// so h264 at 3840×2160 clears the codec and resolution gates and the container/audio halves
    /// are what move.
    #[test]
    fn the_preview_tells_a_container_remux_apart_from_a_re_encode() {
        let _g = fresh_registry();
        restore_quality(Quality::Original);
        fn item(vcodec: &str, part: &str, acodec: &str) -> crate::metadata::Detail {
            crate::metadata::Detail {
                vcodec: vcodec.to_string(),
                part: part.to_string(),
                width: 3840,
                height: 2160,
                audio: vec![crate::metadata::Stream {
                    codec: acodec.to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }
        }
        const MKV: &str = "/library/parts/1/2/file.mkv";
        const MOV: &str = "/library/parts/1/2/file.mov";
        // we pull the file ourselves — nothing on the server touches it
        assert_eq!(
            playback_preview(&item("h264", MKV, "aac")),
            Some(Preview::DirectPlay)
        );
        // the container is one the buffer-feed demuxer cannot stream → the server REPACKAGES it
        assert_eq!(
            playback_preview(&item("h264", MOV, "aac")),
            Some(Preview::Remux)
        );
        // …and so it does for a streamable container whose only audio track has to be converted
        assert_eq!(
            playback_preview(&item("h264", MKV, "truehd")),
            Some(Preview::Remux)
        );
        // a codec the pipeline cannot decode at all is the only real re-encode
        assert_eq!(
            playback_preview(&item("vp9", MKV, "aac")),
            Some(Preview::Converts)
        );
        // …including when the container and the audio would otherwise have been fine
        assert_eq!(
            playback_preview(&item("vp9", MOV, "truehd")),
            Some(Preview::Converts)
        );
        // nothing playable loaded (a show still resolving its episode) answers nothing at all
        assert_eq!(playback_preview(&item("h264", "", "aac")), None);
    }

    /// The pre-flight refusal, graded off a real `/decision` body. Four properties, and each one is
    /// a way the old "parse it and only log it" behaviour went wrong:
    ///   * a `2000` verdict IS a refusal, and it hands back the TRANSCODE sentence — the one that
    ///     names the cause — rather than the general text that merely restates the code;
    ///   * a healthy decision (`1001`, "conversion OK") is not one, or every transcode in the
    ///     library would stop;
    ///   * a body with no verdict at all is not one either — absent is not a refusal, and it is
    ///     what an older server and every failed/unparseable fetch look like;
    ///   * a refusal with no sentence still refuses. The CODE is the decision; the text is only
    ///     the human line, and a server that stays quiet must not thereby become playable.
    #[test]
    fn a_2000_decision_is_a_refusal_and_quotes_the_reason_the_server_named() {
        fn mc(json: &[u8]) -> crate::plex::MediaContainer {
            serde_json::from_slice::<crate::plex::Envelope>(json)
                .expect("parse")
                .media_container
        }
        // the live PMS 1.43.3 answer for a VP9 source
        let refused = mc(br#"{"MediaContainer":{"generalDecisionCode":2000,
            "generalDecisionText":"Neither direct play nor conversion is available.",
            "transcodeDecisionCode":4007,
            "transcodeDecisionText":"Cannot convert this item. Implementation for video encoder 'vp9' not found."}}"#);
        assert_eq!(
            refusal(&refused).as_deref(),
            Some("Cannot convert this item. Implementation for video encoder 'vp9' not found."),
            "the transcode sentence names the cause; the general one only restates the code"
        );

        // only the general sentence came back — quote that instead of nothing
        let general_only = mc(br#"{"MediaContainer":{"generalDecisionCode":"2000",
            "generalDecisionText":"Neither direct play nor conversion is available."}}"#);
        assert_eq!(
            refusal(&general_only).as_deref(),
            Some("Neither direct play nor conversion is available.")
        );

        // refused, and said nothing about why: still a stop, with no line to quote
        let silent = mc(br#"{"MediaContainer":{"generalDecisionCode":2000}}"#);
        assert_eq!(
            refusal(&silent).as_deref(),
            Some(""),
            "the CODE is the decision, not the text"
        );

        // "Direct play not available; Conversion OK." — the ordinary transcode, which must proceed
        let ok = mc(
            br#"{"MediaContainer":{"generalDecisionCode":1001,"transcodeDecisionCode":1001,
            "transcodeDecisionText":"Direct play not available; Conversion OK."}}"#,
        );
        assert!(refusal(&ok).is_none());

        // no verdict block at all (an older server, or a body we could not parse into one)
        assert!(
            refusal(&mc(br#"{"MediaContainer":{"size":1}}"#)).is_none(),
            "absent is not a refusal"
        );
    }

    // ---- the playing item's SERVER: captured once, carried by value ---------------------------
    // A ratingKey, a Part id, a Stream id, a playQueueID and a resume point are all keys on ONE
    // server. Every PMS call in this file used to find its server by asking which one was current
    // at the instant of the call, so an item borrowed from a shared source was resolved, queued,
    // PUT, stopped and — every ten seconds — reported to whichever server the user had since
    // wandered off to. None of that is observable from inside the app, which is why these exist.

    /// Take the crate-wide serialization lock, empty the server registry AND idle the session, so
    /// each test below starts from "nothing installed" and, more importantly, LEAVES the table that
    /// way. A test that registers a loopback server and walks off owes the next one an empty table:
    /// its ports close when it returns, so what it leaves behind is a `client_opt()` that answers
    /// `Some` with a client nothing will ever answer.
    ///
    /// The session goes with it because it holds a `ServerId` INTO that table: a leftover `cur_sid`
    /// names a slot the next test is about to re-fill with a different server, and `machine_id` is
    /// a cache keyed on exactly that id — which `the_machine_id_cache_is_scoped_to_the_server_that_taught_it`
    /// then reads. `reset_session` is the whole-session write, and this is what it is for.
    fn fresh_registry() -> std::sync::MutexGuard<'static, ()> {
        let g = crate::testlock::serial();
        // These are process-global route transactions, not Session fields. A host test has no
        // Engine pump to spend them, so leaving either behind makes a later loopback server see a
        // stop for an encoder from a completely different case.
        let _ = take_pending_original();
        // Establish the idle projection before resetting the reducer: its applied snapshot must
        // describe this test's empty route, not the previous test's final encoder.  Quality is
        // part of the same baseline; cases which need Auto opt in after this boundary and then
        // land a route explicitly.
        reset_session();
        restore_quality(Quality::Original);
        crate::player::reset_route_requests_for_test();
        crate::plex::reset_servers_for_test();
        crate::player::clear_original_failure();
        g
    }

    /// A `ServerId` naming a slot nothing is registered in — so `client_for` answers `None` and
    /// `build_stream` takes its no-client exit without opening a socket.
    fn unregistered_sid() -> ServerId {
        let id = ServerId::from_raw((crate::plex::MAX_SERVERS - 1) as u16);
        assert!(
            crate::plex::client_for(id).is_none(),
            "the test needs an EMPTY slot"
        );
        id
    }

    /// The identity round trip: `request_play`'s captured id reaches `cur_sid` unchanged, through
    /// `ResolveEnv` (the main-thread snapshot) and `Plan` (the worker's output).
    ///
    /// Graded on the resolve that FAILS, deliberately — it is the exit `build_stream` takes first,
    /// before any network, and a plan that carries no server is one `apply_plan` cannot install an
    /// honest `cur_sid` from. Every richer exit builds on the same field.
    #[test]
    fn a_plan_round_trips_the_server_the_request_captured() {
        let _g = fresh_registry();
        let sid = unregistered_sid();

        let env = ResolveEnv::snapshot(sid, "rk-7");
        assert_eq!(
            env.sid, sid,
            "the snapshot carries the id the request was made with"
        );

        let plan = build_stream("rk-7", "/library/parts/5/1/f.mkv", "h264", "ac3", &env);
        assert_eq!(
            plan.sid, sid,
            "a plan that could not resolve still names its server"
        );
        assert!(
            plan.url.is_empty(),
            "no client for that slot, so nothing resolved"
        );
        assert_eq!(
            plan.part_id, 5,
            "…and the rest of the plan is built as usual"
        );

        apply_plan(plan, "rk-7");
        assert_eq!(cur_sid(), sid, "the installed identity is the captured one");
        assert_eq!(cur_rk(), "rk-7", "and its other half");
    }

    /// **A plan that never reached the codec gate must not claim the source is undecodable.**
    ///
    /// `Plan::source_decodable` is a `bool`, `bool::default()` is `false`, and `false` is a CLAIM —
    /// the quality menu renders it as "Converts on server" on the Original row. `build_stream` has
    /// an exit (no client for the server slot) that returns before the gate runs at all, so the
    /// value has to be `true` on the way in and be overwritten by evidence, not the other way
    /// round.
    ///
    /// Differential: with the initializer's explicit `true` removed this fails, and the failure is
    /// a line of copy asserting something about a file nobody opened.
    #[test]
    fn a_plan_that_never_resolved_makes_no_claim_about_the_source() {
        let _g = fresh_registry();
        let sid = unregistered_sid();
        let env = ResolveEnv::snapshot(sid, "rk-7");

        let plan = build_stream("rk-7", "/library/parts/5/1/f.mkv", "h264", "ac3", &env);
        assert!(
            plan.url.is_empty(),
            "the test needs the exit that precedes the codec gate"
        );
        assert!(
            plan.source_decodable,
            "nobody looked at this file, so nothing may be said about it",
        );
        apply_plan(plan, "rk-7");
        assert!(
            source_decodable(),
            "and the session carries the same silence"
        );
    }

    /// The gate's verdict reaches the session, and it is the SOURCE codec that decides it.
    ///
    /// Category: policy plumbing. It is one boolean, but it is the boolean a line of user-visible
    /// copy is drawn from, and the two ends are in different modules — the menu asks
    /// `route::source_decodable()` and never evaluates `video_direct_plays` itself, deliberately,
    /// because a second evaluation could disagree with the routing decision it describes.
    #[test]
    fn the_codec_gates_verdict_is_what_the_quality_menu_reads() {
        let caps = crate::devcaps::Caps::assumed();
        let dv = crate::metadata::Dovi::default().presentation_now();
        // The two ends of the gate, at a UHD raster this device's table admits.
        assert!(
            video_direct_plays("hevc", 3840, 2160, dv, &caps),
            "the raster is not what refuses a 4K item here — `hevc_max` admits it",
        );
        assert!(
            !video_direct_plays("av1", 3840, 2160, dv, &caps),
            "and the codec is: the pipeline cannot feed AV1 at any size",
        );

        let _g = fresh_registry();
        session_mut(|s| s.cur_source_decodable = false);
        assert!(
            !source_decodable(),
            "the menu reads the session, not the gate"
        );
        session_mut(|s| s.cur_source_decodable = true);
        assert!(source_decodable());
    }

    /// The pipeline tier's entry into HLS, both ways round. Differential against the old seam,
    /// which had ONE way in: declare an Original source rate no link could carry and let the
    /// starvation horizon fire on a reserve that was visibly filling. That entry stopped working
    /// when the horizon started requiring an observed drain, and it should have — the reserve was
    /// growing, so nothing was starving. This is the honest replacement.
    /// The raster every pre-2026-08-28 `auto_network` case had hardcoded into the function.
    const HD: (u16, u16) = (1_920, 1_080);

    /// **The declared source raster reaches the catalog, and that is what makes the Uhd rung
    /// reachable at all.**
    ///
    /// Differential, and it is the plan's I9 blocker stated as a test: with a 1080p source
    /// `limited_to` deletes the 4K actuator, so every `auto_network` case that ever ran could not
    /// select the one rung whose `production_load_pm` the table calls empirical. Before this the
    /// raster was a literal inside `arm_auto_fixture`, so the 4K leg was unreachable by construction
    /// rather than by policy — `tests/serve_fixtures.py` served no 22000 rung and the literal was
    /// there to keep candidates off a 404.
    #[test]
    fn a_declared_4k_source_makes_the_uhd_actuator_feasible() {
        let _lock = crate::testlock::serial();
        let uhd_feasible = |raster: (u16, u16)| {
            arm_auto_fixture(
                "http://host/clip.mp4",
                900_000,
                "http://host/__abr",
                true,
                raster,
            );
            auto_catalog()
                .feasible()
                .any(|candidate| candidate.rung == crate::abr::Rung::Uhd)
        };
        assert!(
            !uhd_feasible(HD),
            "a 1080p source must not admit the 4K actuator"
        );
        assert!(uhd_feasible((3_840, 2_160)), "a 4K source must");
    }

    #[test]
    fn the_fixture_can_start_in_hls_instead_of_provoking_a_starvation() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);

        // Without the flag the fixture arms an Original and returns nothing to open.
        assert_eq!(
            arm_auto_fixture(
                "http://host/clip.mp4",
                900_000,
                "http://host/__abr",
                false,
                HD
            ),
            None,
        );
        assert!(matches!(
            cur_delivery(),
            crate::plex::TranscodeDelivery::ProgressiveMkv
        ));
        assert!(
            auto_original_watch().is_some(),
            "…and it is WATCHED, which is what the transition case grades",
        );

        // With it, the post-fallback state is installed directly and the playlist comes back.
        let url = arm_auto_fixture(
            "http://host/clip.mp4",
            900_000,
            "http://host/__abr/",
            true,
            HD,
        )
        .expect("a fixture that starts in HLS hands back the playlist to open");
        assert!(
            url.starts_with("http://host/__abr/720/master.m3u8"),
            "{url}"
        );
        assert!(matches!(
            cur_delivery(),
            crate::plex::TranscodeDelivery::FixedHls {
                seconds_per_segment: 2
            }
        ));
        assert_eq!(cur_ceiling(), Some(crate::abr::Rung::P480.ceiling()));
        assert!(
            auto_original_watch().is_none(),
            "there is no Original under it to watch",
        );
        let (control, _) = hls_abr_control().expect("the direct HLS fixture has a controller");
        assert!(
            !control.has_original_candidate() && !control.can_recover_original(),
            "and a loopback source probe cannot escape the HLS-only test",
        );

        restore_quality(Quality::Original);
        install_active_encoder("");
        reset_session();
    }

    /// A source request that returns an HTTP error before its first body byte did not measure a
    /// slow link.  Falling back as though it measured 0 kbps throws away the exact evidence which
    /// admitted Original and opens at the emergency floor; on the incident server that left Auto
    /// at 720/1100 kbps while a manual Original played smoothly.
    ///
    /// The remote case carries the rung its completed source probe selected. The local case
    /// deliberately carries bootstrap's unknown-link fallback: Local admitted Original without a
    /// measurement, and source demand must not be relabelled as capacity after the open fails.
    #[test]
    fn an_unopened_auto_original_reuses_admission_evidence_instead_of_inventing_zero_rate() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);

        arm_auto_fixture(
            "http://host/clip.mp4",
            10_000,
            "http://host/__abr",
            false,
            HD,
        );
        session_mut(|s| s.auto_bootstrap_rung = Some(crate::abr::Rung::P1080M12));
        let remote =
            fallback_unopened_auto_to_hls(0).expect("the refused source falls back to HLS");
        assert!(
            remote.contains("/__abr/12000/"),
            "the completed probe's decision is retained: {remote}"
        );

        reset_session();
        restore_quality(Quality::Auto);
        arm_auto_fixture(
            "http://host/clip.mp4",
            28_000,
            "http://host/__abr",
            false,
            HD,
        );
        let local =
            fallback_unopened_auto_to_hls(0).expect("a local refused source also falls back");
        assert!(
            local.contains("/__abr/720/"),
            "unknown capacity keeps bootstrap's honest floor: {local}"
        );

        restore_quality(Quality::Original);
        install_active_encoder("");
        reset_session();
    }

    /// A route declaration describes the elementary streams arriving at the television, not the
    /// file PMS started from.  An Original Dolby Vision + Atmos source that falls back to HLS is
    /// re-encoded as H.264 + AAC, so carrying its source-only Dolby flags across the handoff makes
    /// diagnostics lie and (for `immersive`) tells the system player that AAC contains Atmos.
    #[test]
    fn an_original_to_hls_handoff_drops_source_only_dolby_declarations() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);

        arm_auto_fixture(
            "http://host/dovi-atmos.mkv",
            28_000,
            "http://host/__abr",
            false,
            (3_840, 2_160),
        );
        set_stream_declaration("hevc", "eac3", 23.976, p8(), true);

        let hls = fallback_auto_to_hls(8_000, 120).expect("the watched Original falls back");
        assert!(
            hls.contains("/__abr/"),
            "the fixture produced an HLS route: {hls}"
        );
        assert_eq!(stream_vcodec(), "h264");
        assert_eq!(stream_acodec(), "aac");
        assert_eq!(
            stream_fps(),
            0.0,
            "an encoded output must not inherit source FPS metadata"
        );
        assert_eq!(
            session().stream_dovi,
            crate::metadata::Dovi::NONE,
            "the route must retire the source's Dolby Vision declaration, not merely hide it",
        );
        assert!(
            !session().stream_immersive,
            "the route must retire the source E-AC3 JOC/Atmos declaration, not merely hide it",
        );
        assert_eq!(stream_dovi(), crate::metadata::Dovi::NONE);
        assert!(!stream_immersive());

        restore_quality(Quality::Original);
        install_active_encoder("");
        reset_session();
    }

    /// **RE-EXPRESSED 2026-08-27**, name and message both. It read
    /// `only_a_measured_remote_auto_original_arms_the_progressive_watchdog` and asserted "HLS and
    /// Local Original do not use this watchdog" — which was true of the code and is the defect
    /// `docs/measurements/local-original-blind.md` measured. What the watchdog needs is a
    /// MEASURED SOURCE RATE and a progressive delivery; where the server sits is not part of it.
    #[test]
    fn a_measured_auto_original_arms_the_progressive_watchdog_wherever_the_server_is() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                url: "https://example.invalid/source.mkv".into(),
                transport_kbps: 28_000,
                auto_original_watched: true,
                ..Default::default()
            },
            "rk-auto",
        );
        assert_eq!(auto_original_watch().map(|w| w.source_kbps), Some(28_000));
        session_mut(|s| s.cur_auto_original_watched = false);
        assert!(
            auto_original_watch().is_none(),
            "HLS owns its own controller and needs no watchdog"
        );
        restore_quality(Quality::Original);
        reset_session();
    }

    /// **The differential for the LOCAL blindness.** A local server, Auto, a direct-playable
    /// source with a measured transport rate: `build_stream` must arm the watchdog. Against the
    /// old route this fails on the last assertion — `plan.auto_original_watched` was
    /// `Auto && Location::Remote && auto_original`, so a LAN playback ran unsupervised and a link
    /// that turned out not to carry the source produced 8-25% of real time for the rest of the
    /// film with no `abr:` line anywhere in the log.
    #[test]
    fn a_local_auto_original_is_supervised_exactly_like_a_remote_one() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        let sid = crate::plex::register_for_test(
            "machine-local-watch",
            "<peer-host-1>.example.invalid",
            32400,
            "token",
            "test-client-id",
        );
        crate::plex::client_for(sid)
            .expect("server installed")
            .set_link(crate::plex::probe::Location::Local);
        apply_plan(
            Plan {
                sid,
                url: "https://example.invalid/source.mkv".into(),
                transport_kbps: 10_634,
                delivery: crate::plex::TranscodeDelivery::ProgressiveMkv,
                auto_original_watched: true,
                ..Default::default()
            },
            "rk-local-original",
        );
        let watch = auto_original_watch().expect("a local Auto Original is still watched");
        assert_eq!(
            watch.source_kbps, 10_634,
            "and it is watched against the MEASURED source"
        );
        restore_quality(Quality::Original);
        reset_session();
        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn hls_controller_starts_at_the_rung_the_runtime_fallback_selected() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                tsession: "encoder-1".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P720Low.ceiling()),
                ..Default::default()
            },
            "rk-auto",
        );
        let (control, encoder) = hls_abr_control().expect("Auto HLS control");
        assert_eq!(control.initial_rung, crate::abr::Rung::P720Low);
        assert_eq!(encoder.encoder(), "encoder-1");
        restore_quality(Quality::Original);
        reset_session();
        install_active_encoder("");
        crate::player::reset_route_requests_for_test();
    }

    /// **The incident this pins killed a playback from inside the client and read as a server
    /// fault.** A switch commits, so the live encoder is `<sess>-abr-1`; a scrub reloads the demux
    /// worker, which used to restart its generation counter at 0 while `transcode_seek` kept the
    /// session id; the next transaction primed a candidate named `<sess>-abr-1` — the live session.
    /// Rollback then stopped it via `abandon`, commit via `retire(previous)`, and the demuxer saw a
    /// run of 404s it correctly reports as "not produced in time".
    ///
    /// The assertion is deliberately about the NAME rather than about any one exit: both exits are
    /// safe exactly when a candidate can never be called what the live encoder is called.
    #[test]
    fn a_candidate_is_never_named_after_the_encoder_it_would_replace() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                // `commit_transition` is deliberately closed outside a landed Engine. This fixture
                // exercises a live HLS replacement, so make the synthetic plan playable rather than
                // weakening the production `Stable` gate to accommodate an idle test session.
                url: "http://fixture.invalid/12000/master.m3u8".into(),
                // Two DIFFERENT fields, and their divergence is what makes the collision
                // possible: `sess` is the candidate namespace (`logical_session`), `tsession`
                // seeds the live encoder. Before any switch they agree, as they do on the wire.
                sess: "sess-42".into(),
                tsession: "sess-42".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P1080M12.ceiling()),
                ..Default::default()
            },
            "rk-auto",
        );
        // The fixture base is what lets `prime` answer without a client; the naming it exercises
        // is the same line the live path takes.
        session_mut(|s| s.auto_fixture_base = "http://fixture.invalid".into());
        let (control, first_encoder) = hls_abr_control().expect("Auto HLS control");
        let proposal = crate::abr::Proposal {
            rung: crate::abr::Rung::P1080M6,
            direction: crate::abr::Direction::Down,
        };

        let primed = control
            .prime(&first_encoder, proposal, 0, None)
            .expect("the fixture path primes");
        let candidate = primed.encoder_session.clone();
        assert_ne!(
            candidate,
            first_encoder.encoder(),
            "a candidate may not be the live encoder",
        );
        // The switch commits: the candidate is now what the playback is reading.
        let raster = proposal.rung.raster();
        let observed = crate::abr::ObservedHlsVariant::new(
            u64::from(proposal.rung.kbps()) * 1_000,
            i32::from(raster.0),
            i32::from(raster.1),
        )
        .unwrap();
        let rejected = control.commit_transition(
            &first_encoder,
            &primed,
            proposal,
            (observed, 20_000),
            || None::<()>,
        );
        assert_eq!(rejected, Err(HlsCommitRefusal::TransitionRejected));
        assert_eq!(
            active_encoder(),
            first_encoder.encoder(),
            "a rejected local/controller transition must leave the process route untouched",
        );
        assert!(
            control.commit(&first_encoder, &primed, proposal, (observed, 20_000)),
            "the commit swap takes",
        );
        let transition_called = std::cell::Cell::new(false);
        let moved = control.commit_transition(
            &first_encoder,
            &primed,
            proposal,
            (observed, 20_000),
            || {
                transition_called.set(true);
                Some(())
            },
        );
        assert_eq!(moved, Err(HlsCommitRefusal::RouteMoved));
        assert!(
            !transition_called.get(),
            "a superseded worker may not mutate its controller/local state",
        );

        // Reconstructing the worker around the live route is the state a seek creates. The seek
        // now publishes a fresh physical encoder first; the important property here is still that
        // a fresh worker cannot restart a local counter and collide with whichever id is live.
        let (control, live) = hls_abr_control().expect("Auto HLS control survives the reload");
        assert_eq!(
            live.encoder(),
            candidate,
            "the seek carries the committed encoder, as it must",
        );
        assert_eq!(
            control.initial_rung, proposal.rung,
            "a seek must rebuild the controller at the rung the live encoder actually serves, \
             not at the stale bootstrap ceiling stored before the worker committed",
        );
        assert_eq!(
            control.initial_observed,
            Some((observed, 20_000)),
            "seek/reload must carry the delivered response separately from its request rung",
        );

        let after_seek = control
            .prime(&live, proposal, 890_000_000, None)
            .expect("the fixture path primes")
            .encoder_session;
        assert_ne!(
            after_seek,
            live.encoder(),
            "the first post-seek candidate must not be named after the live session",
        );

        restore_quality(Quality::Original);
        reset_session();
        install_active_encoder("");
    }

    /// A seek is a new Universal Transcoder start, even when it asks for the same rung and codecs.
    /// PMS keys the physical encoder by the exact opaque `session`; re-registering that key can
    /// resurrect or mutate a stale resource and was observed in the server archive as the same
    /// `abr-N` starting twice.  The replacement must therefore be registered under a fresh key,
    /// published atomically, and the old exact key stopped only after that publication succeeds.
    #[test]
    fn a_transcode_seek_swaps_to_a_fresh_physical_session_and_retires_the_old_one() {
        use std::io::{BufRead, BufReader, Write};
        use std::time::Duration;

        let _g = fresh_registry();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port() as i32;
        let (tx, rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().expect("accept decision/stop");
                let mut request = String::new();
                BufReader::new(&socket)
                    .read_line(&mut request)
                    .expect("request line");
                tx.send(request.clone()).expect("publish request");
                let body = if request.contains("/decision?") {
                    br#"{"MediaContainer":{"generalDecisionCode":1000,"mdeDecisionCode":1000}}"#
                        .as_slice()
                } else {
                    b"".as_slice()
                };
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len(),
                )
                .expect("response headers");
                socket.write_all(body).expect("response body");
            }
        });

        let sid = crate::plex::register_for_test(
            "seek-session-test",
            "127.0.0.1",
            port,
            "token",
            "seek-client",
        );
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                sid,
                sess: "playback-seek".into(),
                tsession: "playback-seek-abr-old".into(),
                url: "http://127.0.0.1/old/master.m3u8".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P720Low.ceiling()),
                ..Default::default()
            },
            "42",
        );

        let new_url = transcode_seek(300).expect("accepted seek decision");
        let decision = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("PMS never received the seek decision");
        let new_encoder = decision
            .split("session=")
            .nth(1)
            .and_then(|tail| tail.split('&').next())
            .expect("decision has a physical session");
        assert_ne!(
            new_encoder, "playback-seek-abr-old",
            "a seek must not re-register the physical session it is replacing",
        );
        assert!(
            new_url.contains(&format!("session={new_encoder}")),
            "{new_url}"
        );
        assert_eq!(transcode_session(), new_encoder);
        assert_eq!(active_encoder(), new_encoder);

        let stop = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the old physical session was not retired");
        assert!(stop.contains("/stop?"), "{stop}");
        assert!(stop.contains("session=playback-seek-abr-old"), "{stop}");
        server.join().unwrap();

        restore_quality(Quality::Original);
        reset_session();
        install_active_encoder("");
        crate::plex::reset_servers_for_test();
    }

    /// A `/decision` response is preparation, not publication. PMS can close the connection or
    /// return an unparseable body after registering the proposed resource; neither outcome may
    /// rewrite Session/ACTIVE to the requested rung while the old encoder is still on screen.
    #[test]
    fn a_failed_retranscode_decision_leaves_the_live_route_unchanged() {
        use std::io::{BufRead, BufReader, Write};
        use std::time::Duration;

        let _g = fresh_registry();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port() as i32;
        let (tx, rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().expect("accept decision/cleanup");
                let mut request = String::new();
                BufReader::new(&socket)
                    .read_line(&mut request)
                    .expect("request line");
                tx.send(request.clone()).expect("publish request");
                let body = if request.contains("/decision?") {
                    b"this is not a MediaContainer".as_slice()
                } else {
                    b"".as_slice()
                };
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len(),
                )
                .expect("response headers");
                socket.write_all(body).expect("response body");
            }
        });

        let sid = crate::plex::register_for_test(
            "failed-retranscode-test",
            "127.0.0.1",
            port,
            "token",
            "failed-retranscode-client",
        );
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                sid,
                sess: "logical-playback".into(),
                tsession: "live-encoder".into(),
                url: "http://127.0.0.1/live/master.m3u8".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P720Low.ceiling()),
                vcodec: "h264".into(),
                acodec: "aac".into(),
                ..Default::default()
            },
            "42",
        );
        let expected = worker_ticket();
        let before = (
            url(),
            transcode_session(),
            stream_vcodec(),
            stream_acodec(),
            cur_ceiling(),
            cur_delivery(),
        );

        assert_eq!(retranscode_for(&expected, 90), None);
        assert_eq!(worker_ticket(), expected, "the semantic route did not move");
        assert_eq!(
            (
                url(),
                transcode_session(),
                stream_vcodec(),
                stream_acodec(),
                cur_ceiling(),
                cur_delivery(),
            ),
            before,
            "a failed preparation must publish none of the requested declaration",
        );

        let decision = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("PMS never received decision");
        assert!(decision.contains("/decision?"), "{decision}");
        let cleanup = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the uncommitted resource was not cleaned up");
        assert!(cleanup.contains("/stop?"), "{cleanup}");
        server.join().unwrap();

        restore_quality(Quality::Original);
        reset_session();
        install_active_encoder("");
        crate::plex::reset_servers_for_test();
    }

    /// Exact user sequence from the device trace: Auto HLS commits a replacement encoder, a
    /// manual Original open fails, the held HLS route is restored, then the user selects Auto.
    /// Every boundary must retain the encoder/rung/URL that was ACTUALLY on screen.  Before this
    /// regression the worker updated only `ACTIVE_ENCODER`; the main-thread route still named the
    /// bootstrap URL and ceiling, so rollback reopened old media and Auto restarted at 720 kbps.
    #[test]
    fn failed_original_then_auto_keeps_the_live_adaptive_route() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                url: "http://fixture.invalid/720/master.m3u8?offset=100".into(),
                sess: "sess-live".into(),
                tsession: "encoder-bootstrap".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P480.ceiling()),
                transport_kbps: 28_000,
                auto_original: Some(AutoOriginalCandidate {
                    url: "https://example.invalid/source.mkv".into(),
                    probe_part: "https://example.invalid/source.mkv".into(),
                    direct: true,
                    vcodec: "hevc".into(),
                    acodec: "eac3".into(),
                    fps: 23.976,
                    dovi: crate::metadata::Dovi::NONE,
                    immersive: true,
                    audio_sid: 42,
                    audio_ordinal: Some(1),
                    subtitle_ordinal: None,
                }),
                ..Default::default()
            },
            "rk-auto",
        );
        set_stream_codecs("h264", "aac");
        session_mut(|s| s.auto_fixture_base = "http://fixture.invalid".into());

        let (control, bootstrap) = hls_abr_control().expect("the Auto worker owns HLS");
        let proposal = crate::abr::Proposal {
            rung: crate::abr::Rung::Uhd,
            direction: crate::abr::Direction::Up,
        };
        let primed = control
            .prime(&bootstrap, proposal, 140_000_000, None)
            .expect("fixture candidate");
        let raster = proposal.rung.raster();
        let observed = crate::abr::ObservedHlsVariant::new(
            u64::from(proposal.rung.kbps()) * 1_000,
            i32::from(raster.0),
            i32::from(raster.1),
        )
        .unwrap();
        assert!(control.commit(&bootstrap, &primed, proposal, (observed, 20_000)));

        // The picker changed before the pump performed the codec-changing handoff. Exercise the
        // same claim boundary as the pump: a persisted checkmark alone is deliberately not an
        // applied route contract.
        set_quality(Quality::Original);
        let original = claim_route_action().expect("the manual Original action is explicit");
        assert_eq!(
            original.intent,
            RouteIntent::User(UserRouteIntent::RecoverOriginal),
        );
        assert_eq!(
            recover_auto_to_original_for(&original.ticket, 142, false),
            Some(AutoOriginalReload::Direct),
        );
        assert_eq!(rollback_seconds(), Some(142));
        assert_eq!(
            url(),
            primed.url,
            "rollback must reopen the live candidate URL, never the bootstrap URL it replaced",
        );
        let (restored, restored_encoder) = hls_abr_control()
            .expect("failed manual Original still needs the adaptive HLS controller");
        assert_eq!(restored_encoder.encoder(), primed.encoder_session);
        assert_eq!(restored.initial_rung, proposal.rung);

        set_quality(Quality::Auto);
        assert_eq!(cur_ceiling(), Some(proposal.rung.ceiling()));
        assert!(
            !crate::player::pending_transcode_refresh(),
            "Auto must adopt the already-live adaptive route instead of rebuilding at 720 kbps",
        );
        assert!(
            crate::player::pending_adaptive_reload(),
            "the retained HLS worker must recapture Auto's Original-recovery contract",
        );

        reset_session();
        restore_quality(Quality::Original);
        install_active_encoder("");
        crate::player::reset_audio_track();
        crate::player::reset_subtitle();
    }

    #[test]
    fn hls_recovery_restores_the_exact_direct_source_and_rearms_its_watchdog() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                url: "https://example.invalid/hls/master.m3u8".into(),
                tsession: "encoder-1".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P1080High.ceiling()),
                transport_kbps: 28_000,
                auto_original: Some(AutoOriginalCandidate {
                    url: "https://example.invalid/source.mkv".into(),
                    probe_part: "https://example.invalid/source.mkv".into(),
                    direct: true,
                    vcodec: "hevc".into(),
                    acodec: "eac3".into(),
                    fps: 23.976,
                    dovi: crate::metadata::Dovi::NONE,
                    immersive: true,
                    audio_sid: 42,
                    audio_ordinal: Some(1),
                    subtitle_ordinal: Some(2),
                }),
                ..Default::default()
            },
            "rk-auto",
        );
        assert_eq!(
            recover_auto_to_original(120),
            Some(AutoOriginalReload::Direct)
        );
        assert_eq!(
            auto_history().visible_switches,
            0,
            "an unproven Original Load is not a switch the viewer has seen",
        );
        assert_eq!(url(), "https://example.invalid/source.mkv");
        assert!(!is_transcoding());
        assert_eq!(
            cur_delivery(),
            crate::plex::TranscodeDelivery::ProgressiveMkv
        );
        assert_eq!(cur_ceiling(), None);
        assert_eq!(stream_vcodec(), "hevc");
        assert_eq!(stream_acodec(), "eac3");
        assert_eq!(auto_original_watch().map(|w| w.source_kbps), Some(28_000));
        assert_eq!(crate::player::desired_sub_idx(), 2);
        restore_quality(Quality::Original);
        reset_session();
        install_active_encoder("");
        crate::player::reset_audio_track();
        crate::player::reset_subtitle();
    }

    /// **The evidence that authorises a recovery is not the evidence that it WORKED, and the
    /// recovery was spending the old route before finding out.** (Device, 2026-08-29 — the
    /// reported failure, in its own sequence.)
    ///
    /// `recover_auto_to_original` cleared `tsession`, cleared the active encoder and asked the
    /// server to stop the HLS encoder, and only then did the pump open the source URL. On that
    /// television the source URL failed — the same server had answered **503** to an Original
    /// probe forty seconds earlier while the HLS segments beside it kept succeeding — and by then
    /// the working stream had been dismantled. The viewer asked for Original by hand and got the
    /// failure read-out, on a film that had been playing.
    ///
    /// So both irreversible steps are deferred until frames prove the new source, and the old
    /// route is kept whole until then. This is the "kept whole" half; the pump wiring that spends
    /// or restores it is `player/pump.rs`.
    ///
    /// Differential by construction: against the recovery as it stood, the first assertion fails —
    /// nothing was kept, so there was nothing to roll back to.
    #[test]
    fn a_recovery_that_never_opens_can_still_go_back_to_the_encoder_it_replaced() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                url: "https://example.invalid/hls/master.m3u8".into(),
                tsession: "encoder-1".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P1080High.ceiling()),
                transport_kbps: 28_000,
                auto_original: Some(AutoOriginalCandidate {
                    url: "https://example.invalid/source.mkv".into(),
                    probe_part: "https://example.invalid/source.mkv".into(),
                    direct: true,
                    vcodec: "hevc".into(),
                    acodec: "eac3".into(),
                    fps: 23.976,
                    dovi: crate::metadata::Dovi::NONE,
                    immersive: true,
                    audio_sid: 42,
                    audio_ordinal: Some(1),
                    subtitle_ordinal: Some(2),
                }),
                ..Default::default()
            },
            "rk-auto",
        );
        // What a live HLS route declares to the pipeline. `apply_plan` leaves these to the
        // decision, so the test states them — they are half of what a rollback has to put back:
        // reloading the m3u8 while the Load payload still says `hevc` is a refusal, not a recovery.
        set_stream_codecs("h264", "aac");
        assert_eq!(
            recover_auto_to_original(120),
            Some(AutoOriginalReload::Direct)
        );
        assert_eq!(
            url(),
            "https://example.invalid/source.mkv",
            "the route did commit"
        );
        assert_eq!(stream_vcodec(), "hevc", "…declaration and all");
        assert!(
            original_recovery_pending(),
            "the encoder is still running on the server and the old route is still known —              nothing here has been proven yet",
        );

        // …and the source never opens. The pump asks for the old route back rather than raising
        // the failure read-out on a stream that was working a moment ago.
        assert_eq!(
            rollback_seconds(),
            Some(120),
            "reload the old route where the film is"
        );
        assert_eq!(
            url(),
            "https://example.invalid/hls/master.m3u8",
            "and it is the old route"
        );
        assert!(is_transcoding(), "the HLS session id is back");
        assert_eq!(
            active_encoder(),
            "encoder-1",
            "and so is the encoder identity the ABR controller steers by — re-installed rather              than re-requested, because it was never stopped",
        );
        assert!(
            matches!(
                cur_delivery(),
                crate::plex::TranscodeDelivery::FixedHls { .. }
            ),
            "the delivery shape must come back with it, or the demuxer reads an m3u8 as an mkv",
        );
        assert_eq!(cur_ceiling(), Some(crate::abr::Rung::P1080High.ceiling()));
        assert_eq!(
            stream_vcodec(),
            "h264",
            "the HLS payload declaration, not the source's"
        );
        assert!(!original_recovery_pending(), "and the way back is spent");

        restore_quality(Quality::Original);
        reset_session();
        install_active_encoder("");
        crate::player::reset_audio_track();
        crate::player::reset_subtitle();
    }

    /// A codec-preserving Original remux has the same proof boundary as direct play: a successful
    /// `/decision` only registered a route; it did not prove that the new MKV can deliver a decoded
    /// frame.  Keep the working HLS encoder until that frame arrives. If the remux never opens,
    /// restore HLS and retire the unproven replacement rather than the stream the viewer had.
    #[test]
    fn a_remux_recovery_keeps_hls_until_frames_and_rolls_back_the_replacement() {
        use std::io::{BufRead, BufReader, Write};

        let _g = fresh_registry();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port() as i32;
        let (pre_tx, pre_rx) = std::sync::mpsc::channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel();
        let (post_tx, post_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            fn request(socket: &mut std::net::TcpStream) -> String {
                let mut reader = BufReader::new(socket.try_clone().expect("clone socket"));
                let mut first = String::new();
                reader.read_line(&mut first).expect("request line");
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).expect("request header");
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                }
                first
            }
            fn poll(listener: &std::net::TcpListener, rounds: usize, requests: &mut Vec<String>) {
                for _ in 0..rounds {
                    match listener.accept() {
                        Ok((mut socket, _)) => {
                            requests.push(request(&mut socket));
                            socket
                                .write_all(
                                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                )
                                .expect("control response");
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(4));
                        }
                        Err(error) => panic!("accept control request: {error}"),
                    }
                }
            }

            let (mut socket, _) = listener.accept().expect("accept remux decision");
            let first = request(&mut socket);
            assert!(first.contains("/decision?"), "{first}");
            let body = br#"{"MediaContainer":{"generalDecisionCode":1000,"mdeDecisionCode":1000}}"#;
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len(),
            )
            .expect("decision headers");
            socket.write_all(body).expect("decision body");
            drop(socket);

            listener.set_nonblocking(true).unwrap();
            let mut before_rollback = vec![first];
            poll(&listener, 75, &mut before_rollback);
            pre_tx
                .send(before_rollback)
                .expect("publish pre-frame requests");
            go_rx.recv().expect("begin rollback observation");
            let mut after_rollback = Vec::new();
            poll(&listener, 125, &mut after_rollback);
            post_tx
                .send(after_rollback)
                .expect("publish rollback requests");
        });

        let sid = crate::plex::register_for_test(
            "remux-recovery",
            "127.0.0.1",
            port,
            "token",
            "remux-client",
        );
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                sid,
                sess: "remux-logical".into(),
                url: "http://fixture.invalid/hls/master.m3u8".into(),
                tsession: "remux-hls".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P1080High.ceiling()),
                src_vcodec: "hevc".into(),
                src_acodec: "eac3".into(),
                vcodec: "h264".into(),
                acodec: "aac".into(),
                transport_kbps: 28_000,
                auto_original: Some(AutoOriginalCandidate {
                    url: "http://fixture.invalid/source.mkv".into(),
                    probe_part: "/library/parts/1/file.mkv".into(),
                    direct: false,
                    vcodec: "hevc".into(),
                    acodec: "eac3".into(),
                    fps: 23.976,
                    dovi: crate::metadata::Dovi::NONE,
                    immersive: false,
                    audio_sid: 42,
                    audio_ordinal: Some(1),
                    subtitle_ordinal: None,
                }),
                ..Default::default()
            },
            "42",
        );

        assert_eq!(
            recover_auto_to_original(120),
            Some(AutoOriginalReload::Remux),
        );
        let repeated = recover_auto_to_original(121);
        let replacement = active_encoder();
        let pending_before_frames = original_recovery_pending();
        let pre = pre_rx.recv().expect("captured pre-frame requests");
        go_tx.send(()).unwrap();
        let rollback = rollback_seconds();
        let post = post_rx.recv().expect("captured rollback requests");
        server.join().unwrap();

        assert!(
            pending_before_frames,
            "a decision is not decoded-frame proof"
        );
        assert_eq!(
            repeated, None,
            "an unconfirmed handoff owns the route until frames commit or failure rolls it back",
        );
        assert_eq!(
            pre.iter().filter(|line| line.contains("/stop?")).count(),
            0,
            "the working HLS encoder must remain alive before remux frames: {pre:?}",
        );
        assert_eq!(rollback, Some(120));
        assert_eq!(
            active_encoder(),
            "remux-hls",
            "rollback restores the exact old route"
        );
        assert!(
            post.iter().any(|line| {
                line.contains("/stop?") && line.contains(&format!("session={replacement}"))
            }),
            "rollback retires the unproven remux resource: {post:?}",
        );

        restore_quality(Quality::Original);
        reset_session();
        install_active_encoder("");
        crate::plex::reset_servers_for_test();
        crate::player::reset_audio_track();
        crate::player::reset_subtitle();
    }

    /// **A playback that HAS a way back to Original must be able to LOOK for it.** (Device,
    /// 2026-08-30 — the twenty minutes at 480p.)
    ///
    /// `can_recover_original` is `!probe_part.is_empty() && original_source_kbps > 0`, and the
    /// second term is filled from `Session::cur_transport_kbps`, whose own doc explains the zero
    /// as *"PMS did not provide one and disables the watchdog fail-safely"*. That reasoning is
    /// sound for the PROGRESSIVE WATCHDOG it was written for, which compares a live socket against
    /// that number and cannot do its job without it. It is imported here by accident: the HLS
    /// RECOVERY gate does not compare anything against it up front — its whole purpose is to spend
    /// a bounded probe finding out what the source actually costs.
    ///
    /// So a missing whole-file bitrate silently deletes the feature. `ff.rs` builds
    /// `OriginalRecovery` only when this returns true, and `probe_due` — the one thing that logs a
    /// REASON — is inside it. The device log is the shape of that: after the user returned to Auto
    /// mid-film there is not one `abr: probe withheld`, not one `abr: checking actual Original`,
    /// and not one `abr: mode` in the remaining ~1 600 lines. The recovery did not decide against
    /// probing; it was never constructed, and nothing said so.
    ///
    /// The fallback is `cur_src.0`, the video rate, which is the same quantity minus audio and is
    /// what the menu already shows the user. Wrong by the audio track, and being wrong by an audio
    /// track is not comparable to the feature being absent.
    ///
    /// Differential by construction: against unmodified code the first assertion fails.
    #[test]
    fn a_missing_whole_file_bitrate_must_not_silently_delete_original_recovery() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                url: "https://example.invalid/hls/master.m3u8".into(),
                tsession: "encoder-1".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P480.ceiling()),
                // PMS said what the VIDEO runs at and did not say what the whole file does. That
                // is an ordinary answer, not a broken one.
                src_measure: (23_920, 3_840, 2_160),
                transport_kbps: 0,
                auto_original: Some(AutoOriginalCandidate {
                    url: "https://example.invalid/source.mkv".into(),
                    probe_part: "https://example.invalid/source.mkv".into(),
                    direct: true,
                    vcodec: "hevc".into(),
                    acodec: "eac3".into(),
                    fps: 23.976,
                    dovi: crate::metadata::Dovi::NONE,
                    immersive: true,
                    audio_sid: 42,
                    audio_ordinal: Some(1),
                    subtitle_ordinal: None,
                }),
                ..Default::default()
            },
            "rk-auto",
        );
        let (control, _) = hls_abr_control().expect("Auto HLS control");
        assert!(
            control.can_recover_original(),
            "the candidate exists and its probe URL is known — a missing whole-file bitrate is a              reason to go and measure the source, which is what the probe DOES, and not a reason              to remove the only path back to it",
        );
        assert!(
            control.original_source_kbps() > 0,
            "and the gate needs a requirement to score against, or `source_requirement_kbps` is              zero and every link looks sufficient",
        );

        restore_quality(Quality::Original);
        reset_session();
        install_active_encoder("");
    }

    /// The other half: a recovery that DOES open is made permanent, and the way back is spent
    /// rather than left to be taken by some later failure on a route it no longer describes.
    ///
    /// Both halves have to be pinned together, because the failure mode of a one-sided fix is
    /// silent. A `confirm` that forgot to clear the slot would leave a stale rollback armed for
    /// the rest of the film: the next unrelated demux failure would find it, restore an HLS route
    /// whose encoder the server had long since reaped, and reload onto a dead URL — a worse
    /// outcome than the failure read-out this whole change exists to avoid.
    #[test]
    fn a_recovery_that_opens_spends_the_way_back_rather_than_leaving_it_armed() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                url: "https://example.invalid/hls/master.m3u8".into(),
                tsession: "encoder-1".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P1080High.ceiling()),
                transport_kbps: 28_000,
                auto_original: Some(AutoOriginalCandidate {
                    url: "https://example.invalid/source.mkv".into(),
                    probe_part: "https://example.invalid/source.mkv".into(),
                    direct: true,
                    vcodec: "hevc".into(),
                    acodec: "eac3".into(),
                    fps: 23.976,
                    dovi: crate::metadata::Dovi::NONE,
                    immersive: true,
                    audio_sid: 42,
                    audio_ordinal: Some(1),
                    subtitle_ordinal: None,
                }),
                ..Default::default()
            },
            "rk-auto",
        );
        assert_eq!(
            recover_auto_to_original(120),
            Some(AutoOriginalReload::Direct)
        );
        assert!(original_recovery_pending());

        // Frames arrived — the pump's own test for this, and the reason it is frames and not
        // `loadCompleted`.
        settle_pending_native_start(RouteStartResult::Started);
        confirm_original_recovery();
        assert!(!original_recovery_pending(), "the recovery is permanent");
        assert_eq!(
            auto_history().visible_switches,
            1,
            "the first decoded frame commits exactly one visible HLS-to-Original switch",
        );
        assert_eq!(
            url(),
            "https://example.invalid/source.mkv",
            "and the route is the new one"
        );
        assert!(!is_transcoding());
        assert_eq!(
            active_encoder(),
            "encoder-1",
            "the physical encoder is stopped, but its exact Streaming Resource identity remains \
             the owner of the direct body until playback teardown",
        );
        assert!(
            rollback_original_recovery().is_none(),
            "a spent way back may not be taken by a later, unrelated failure",
        );

        restore_quality(Quality::Original);
        reset_session();
        install_active_encoder("");
        crate::player::reset_audio_track();
        crate::player::reset_subtitle();
    }

    #[test]
    fn manual_original_adopts_one_running_trial_and_revokes_its_auto_ticket_on_frame() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                url: "https://example.invalid/hls/master.m3u8".into(),
                tsession: "adopt-hls".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P1080High.ceiling()),
                transport_kbps: 28_000,
                auto_original: Some(test_original_candidate(None)),
                ..Default::default()
            },
            "rk-adopt-original",
        );
        let hls_worker = worker_ticket();
        assert_eq!(
            recover_auto_to_original_for(&hls_worker, 120, true),
            Some(AutoOriginalReload::Direct),
        );
        let trial_worker = worker_ticket();
        let attempts_before = PLAYER_CONTROL
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .next_start_attempt;

        set_quality(Quality::Original);
        assert!(original_recovery_pending());
        assert!(is_worker_ticket_current(&trial_worker));
        assert!(PLAYER_CONTROL
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending_original
            .as_ref()
            .is_some_and(|pending| pending.adopted_by_user),);

        settle_pending_native_start(RouteStartResult::Started);
        confirm_original_recovery();
        let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            control.next_start_attempt,
            attempts_before
                .checked_add(1)
                .expect("test Load attempt identity exhausted"),
        );
        assert_eq!(control.phase, ControlPhase::Stable);
        assert_eq!(control.applied_quality, Quality::Original);
        drop(control);
        assert!(
            !is_worker_ticket_current(&trial_worker),
            "the Auto candidate worker may not publish after manual adoption commits",
        );
        assert!(!original_recovery_pending());

        restore_quality(Quality::Original);
        reset_session();
        install_active_encoder("");
        reset_player_control_for_test();
        crate::player::reset_audio_track();
        crate::player::reset_subtitle();
    }

    /// A quality pick can arrive after Starfish accepted the replacement Load but before that
    /// replacement produced its first frame. The held HLS route is still the only proven route in
    /// that interval, so the pick must wait behind the Original commit boundary instead of
    /// mutating the transaction underneath its rollback snapshot.
    #[test]
    fn a_quality_change_waits_for_an_original_handoff_to_commit() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                url: "https://example.invalid/hls/master.m3u8".into(),
                tsession: "quality-hls".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P1080High.ceiling()),
                transport_kbps: 28_000,
                auto_original: Some(AutoOriginalCandidate {
                    url: "https://example.invalid/source.mkv".into(),
                    probe_part: "https://example.invalid/source.mkv".into(),
                    direct: true,
                    vcodec: "hevc".into(),
                    acodec: "eac3".into(),
                    fps: 23.976,
                    dovi: crate::metadata::Dovi::NONE,
                    immersive: true,
                    audio_sid: 42,
                    audio_ordinal: Some(1),
                    subtitle_ordinal: None,
                }),
                ..Default::default()
            },
            "rk-quality-handoff",
        );
        assert_eq!(
            recover_auto_to_original(120),
            Some(AutoOriginalReload::Direct),
        );

        set_quality(Quality::P480);
        assert_eq!(
            quality(),
            Quality::P480,
            "the preference and checkmark move now"
        );
        assert!(
            original_recovery_pending(),
            "the first-frame proof still owns the route"
        );
        assert_eq!(
            cur_delivery(),
            crate::plex::TranscodeDelivery::ProgressiveMkv,
            "the pending source declaration may not be rewritten before its first frame",
        );
        assert!(
            !crate::player::pending_transcode_refresh(),
            "the pump must not replace either half of an unconfirmed transaction",
        );

        settle_pending_native_start(RouteStartResult::Started);
        confirm_original_recovery();
        assert!(!original_recovery_pending());
        assert_eq!(
            cur_ceiling(),
            Some(crate::abr::Rung::P480.ceiling()),
            "the deferred pick applies as soon as decoded frames commit Original",
        );
        assert!(crate::player::pending_transcode_refresh());

        let staged = claim_route_action().expect("deferred fixed-rung effect");
        finish_route_action(&staged, RouteApplyResult::Rejected);
        assert_eq!(
            cur_delivery(),
            crate::plex::TranscodeDelivery::ProgressiveMkv,
            "rejecting the deferred effect must restore the Original candidate which produced frames",
        );
        assert_eq!(cur_ceiling(), None);
        assert_eq!(url(), "https://example.invalid/source.mkv");

        restore_quality(Quality::Original);
        reset_session();
        install_active_encoder("");
        crate::player::reset_route_requests_for_test();
    }

    #[test]
    fn a_quality_change_survives_an_original_handoff_rollback() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                url: "https://example.invalid/hls/master.m3u8".into(),
                tsession: "quality-rollback-hls".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P1080High.ceiling()),
                auto_original: Some(AutoOriginalCandidate {
                    url: "https://example.invalid/source.mkv".into(),
                    probe_part: "https://example.invalid/source.mkv".into(),
                    direct: true,
                    vcodec: "hevc".into(),
                    acodec: "eac3".into(),
                    fps: 23.976,
                    dovi: crate::metadata::Dovi::NONE,
                    immersive: false,
                    audio_sid: 0,
                    audio_ordinal: None,
                    subtitle_ordinal: None,
                }),
                ..Default::default()
            },
            "rk-quality-rollback",
        );
        assert_eq!(
            recover_auto_to_original(120),
            Some(AutoOriginalReload::Direct),
        );
        set_quality(Quality::P480);

        assert_eq!(rollback_seconds(), Some(120));
        assert_eq!(
            cur_delivery(),
            crate::plex::TranscodeDelivery::FixedHls {
                seconds_per_segment: 2
            },
            "failure first restores the one route that was proven to play",
        );
        assert_eq!(cur_ceiling(), Some(crate::abr::Rung::P1080High.ceiling()));
        assert!(!crate::player::pending_transcode_refresh());

        // Deferred commands belong to the exact rollback Load, not to thread creation. Only its
        // accepted native result releases the next transaction.
        settle_pending_native_start(RouteStartResult::Started);
        assert_eq!(cur_ceiling(), Some(crate::abr::Rung::P480.ceiling()));
        assert!(crate::player::pending_transcode_refresh());

        restore_quality(Quality::Original);
        reset_session();
        install_active_encoder("");
        crate::player::reset_route_requests_for_test();
    }

    #[test]
    fn a_failed_rollback_load_discards_trial_effects_before_the_next_trial() {
        let _g = fresh_registry();
        crate::player::reset_route_requests_for_test();
        reset_session();
        restore_quality(Quality::Auto);
        session_mut(|s| {
            s.url = "http://fixture.invalid/hls/master.m3u8".into();
            s.tsession = "rollback-owner".into();
            s.cur_delivery = crate::plex::TranscodeDelivery::FixedHls {
                seconds_per_segment: 2,
            };
            s.cur_ceiling = Some(crate::abr::Rung::P1080High.ceiling());
        });
        install_active_hls(
            "rollback-owner",
            "http://fixture.invalid/hls/master.m3u8",
            crate::abr::Rung::P1080High,
        );
        reset_player_control_for_test();

        let first = snapshot_route("rollback-owner".into(), 41);
        session_mut(|s| {
            s.url = "https://example.invalid/first-source.mkv".into();
            s.tsession.clear();
            s.cur_delivery = crate::plex::TranscodeDelivery::ProgressiveMkv;
            s.cur_ceiling = None;
        });
        set_pending_original(first, true);
        set_quality(Quality::P480);
        assert_eq!(rollback_seconds(), Some(41));
        settle_pending_native_start(RouteStartResult::StartFailed);
        {
            let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
            assert!(matches!(control.phase, ControlPhase::Failed(_)));
            assert!(control.start_deferred.is_none());
        }
        assert!(
            !crate::player::pending_transcode_refresh(),
            "a terminal rollback Load may not release its deferred quality edit",
        );

        // A later, independent Original trial and successful rollback carry no residue from the
        // failed transaction, even though the durable picker still remembers the user's choice.
        let second = snapshot_route("rollback-owner".into(), 52);
        session_mut(|s| {
            s.url = "https://example.invalid/second-source.mkv".into();
            s.tsession.clear();
            s.cur_delivery = crate::plex::TranscodeDelivery::ProgressiveMkv;
            s.cur_ceiling = None;
        });
        set_pending_original(second, true);
        assert_eq!(rollback_seconds(), Some(52));
        settle_pending_native_start(RouteStartResult::Started);
        {
            let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
            assert_eq!(control.phase, ControlPhase::Stable);
            assert!(control.start_deferred.is_none());
            assert!(control.pending_user.is_none());
        }
        assert!(!crate::player::pending_transcode_refresh());

        restore_quality(Quality::Original);
        reset_session();
        install_active_encoder("");
        reset_player_control_for_test();
        crate::player::reset_route_requests_for_test();
    }

    #[test]
    fn audio_selected_during_original_trial_uses_the_route_that_actually_lands() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                url: "https://example.invalid/hls/master.m3u8".into(),
                tsession: "audio-rollback-hls".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P1080High.ceiling()),
                audio_sid: 7,
                auto_original: Some(AutoOriginalCandidate {
                    url: "https://example.invalid/source.mkv".into(),
                    probe_part: "https://example.invalid/source.mkv".into(),
                    direct: true,
                    vcodec: "hevc".into(),
                    acodec: "eac3".into(),
                    fps: 23.976,
                    dovi: crate::metadata::Dovi::NONE,
                    immersive: false,
                    audio_sid: 42,
                    audio_ordinal: Some(1),
                    subtitle_ordinal: None,
                }),
                ..Default::default()
            },
            "rk-audio-rollback",
        );
        assert_eq!(
            recover_auto_to_original(120),
            Some(AutoOriginalReload::Direct)
        );

        commit_audio_selection(2, "aac", 99);
        assert!(
            !pending_user_route_intent(UserRouteIntent::NativeAudioReload),
            "the temporary Direct actuator must not escape the Original trial",
        );
        assert_eq!(rollback_seconds(), Some(120));
        settle_pending_native_start(RouteStartResult::Started);
        assert_eq!(cur_audio_sid(), 99);
        assert!(
            pending_user_route_intent(UserRouteIntent::Retranscode),
            "after HLS rollback the same semantic pick must be applied by retranscode",
        );

        restore_quality(Quality::Original);
        reset_session();
        install_active_encoder("");
        crate::player::reset_route_requests_for_test();
    }

    #[test]
    fn an_installed_cold_direct_route_closes_its_logical_resource_at_teardown() {
        use std::io::{BufRead, BufReader, Write};

        let _g = fresh_registry();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port() as i32;
        let (tx, rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            let mut requests = Vec::new();
            while std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        let mut first = String::new();
                        BufReader::new(socket.try_clone().expect("clone socket"))
                            .read_line(&mut first)
                            .expect("request line");
                        requests.push(first);
                        socket
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .expect("stop response");
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept cold-direct cleanup: {error}"),
                }
            }
            tx.send(requests).unwrap();
        });
        let sid = crate::plex::register_for_test(
            "cold-direct-owner",
            "127.0.0.1",
            port,
            "token",
            "cold-direct-client",
        );
        apply_plan(
            Plan {
                sid,
                sess: "cold-direct-logical".into(),
                url: "http://fixture.invalid/library/parts/1/file.mkv".into(),
                vcodec: "h264".into(),
                acodec: "aac".into(),
                ..Default::default()
            },
            "42",
        );
        assert!(
            !is_transcoding(),
            "resource ownership must not relabel Direct as a transcode"
        );
        scrobble_stop(None, None);
        drain_scrobble();
        let requests = rx.recv().expect("cold-direct cleanup observation");
        server.join().unwrap();

        assert_eq!(
            requests.len(),
            1,
            "one installed resource has one final owner: {requests:?}"
        );
        assert!(
            requests[0].contains("session=cold-direct-logical"),
            "{}",
            requests[0]
        );
        assert!(
            requests[0].contains("closeResourceSession=1"),
            "{}",
            requests[0]
        );

        reset_session();
        install_active_encoder("");
        crate::plex::reset_servers_for_test();
    }

    /// Runtime direct recovery borrows the exact active HLS Streaming Resource. A decoded frame
    /// proves the current HTTP body, but PMS checks the resource's terminated flag again on every
    /// later Range GET. Therefore confirmation stops only the physical HLS encoder, retains that
    /// exact resource identity in the direct URL, and closes it only at final playback teardown.
    #[test]
    fn a_confirmed_direct_recovery_remains_seekable_after_hls_is_retired() {
        use std::io::{BufRead, BufReader, Write};

        let _g = fresh_registry();
        if !crate::net::global_init() || !crate::curlio::available() {
            return;
        }
        let plan = crate::abr::source_probe_plan(320, crate::abr::PROBE_BUDGET_MS).unwrap();
        let bytes = plan.target_bytes;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port() as i32;
        let (stop_tx, stop_rx) = std::sync::mpsc::channel();
        let (all_tx, all_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            let mut resource_closed = false;
            for index in 0..4 {
                let (mut socket, _) = listener.accept().expect("accept direct lifecycle request");
                let mut reader = BufReader::new(socket.try_clone().expect("clone socket"));
                let mut first = String::new();
                reader.read_line(&mut first).expect("request line");
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).expect("request header");
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                }
                requests.push(first.clone());
                if index == 1 || index == 3 {
                    assert!(first.contains("/stop?"), "{first}");
                    resource_closed |= first.contains("closeResourceSession=1");
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .expect("stop response");
                    if index == 1 {
                        stop_tx.send(first).expect("publish stop request");
                    }
                } else if resource_closed {
                    socket
                        .write_all(
                            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .expect("terminated resource response");
                } else {
                    write!(
                        socket,
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes - 1,
                        bytes * 2,
                        bytes,
                    )
                    .expect("source headers");
                    socket.write_all(&vec![0x55; bytes]).expect("source body");
                }
            }
            all_tx.send(requests).expect("publish direct lifecycle");
        });

        let sid = crate::plex::register_for_test(
            "direct-recovery",
            "127.0.0.1",
            port,
            "token",
            "direct-client",
        );
        let client = crate::plex::client_for(sid).expect("test server installed");
        let logical_url = client
            .direct_play_url("/library/parts/1/file.mkv", "direct-logical")
            .to_url();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                sid,
                sess: "direct-logical".into(),
                url: "http://fixture.invalid/hls/master.m3u8".into(),
                tsession: "direct-hls".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P480.ceiling()),
                transport_kbps: 320,
                auto_original: Some(AutoOriginalCandidate {
                    url: logical_url,
                    probe_part: "/library/parts/1/file.mkv".into(),
                    direct: true,
                    vcodec: "h264".into(),
                    acodec: "aac".into(),
                    fps: 24.0,
                    dovi: crate::metadata::Dovi::NONE,
                    immersive: false,
                    audio_sid: 0,
                    audio_ordinal: None,
                    subtitle_ordinal: None,
                }),
                ..Default::default()
            },
            "42",
        );

        assert_eq!(
            recover_auto_to_original(120),
            Some(AutoOriginalReload::Direct),
        );
        let direct_url = url();
        let initial = crate::curlio::sample_throughput_result(
            &direct_url,
            bytes,
            std::time::Duration::from_secs(4),
            std::time::Duration::from_secs(4),
        );
        assert!(initial.is_ok(), "the first direct body opens: {initial:?}");
        settle_pending_native_start(RouteStartResult::Started);
        confirm_original_recovery();
        let stop = stop_rx.recv().expect("captured HLS retirement");
        let reopened = crate::curlio::sample_throughput_result(
            &direct_url,
            bytes,
            std::time::Duration::from_secs(4),
            std::time::Duration::from_secs(4),
        );
        assert_eq!(
            active_encoder(),
            "direct-hls",
            "teardown still owns the resource identity"
        );
        scrobble_stop(None, None);
        drain_scrobble();
        let requests = all_rx.recv().expect("captured direct lifecycle");
        server.join().unwrap();

        assert!(
            direct_url.contains("X-Plex-Session-Identifier=direct-hls"),
            "the actual direct body must exact-reuse the resource the probe measured: {direct_url}",
        );
        assert!(
            stop.contains("closeResourceSession=0"),
            "confirmation retires the encoder without terminating the source resource: {stop}",
        );
        assert!(
            reopened.is_ok(),
            "a later Range/seek must still open: {reopened:?}"
        );
        assert_eq!(
            active_encoder(),
            "",
            "final teardown spends the retained owner"
        );
        assert_eq!(requests.len(), 4);
        let stops: Vec<_> = requests
            .iter()
            .filter(|line| line.contains("/stop?"))
            .collect();
        assert_eq!(
            stops.len(),
            2,
            "one physical retirement and one final close: {requests:?}"
        );
        assert!(stops[0].contains("session=direct-hls"), "{}", stops[0]);
        assert!(stops[0].contains("closeResourceSession=0"), "{}", stops[0]);
        assert!(stops[1].contains("session=direct-hls"), "{}", stops[1]);
        assert!(stops[1].contains("closeResourceSession=1"), "{}", stops[1]);

        restore_quality(Quality::Original);
        reset_session();
        install_active_encoder("");
        crate::plex::reset_servers_for_test();
        crate::player::reset_audio_track();
        crate::player::reset_subtitle();
    }

    /// BACK while a direct recovery is still awaiting frames has one resource owner, not two:
    /// `scrobble_stop` takes the retained active identity and performs the final exact close.
    /// Dropping PendingOriginal must only forget its rollback in this branch, or PMS receives two
    /// concurrent stop/close requests for the same resource.
    #[test]
    fn stopping_a_pending_direct_recovery_closes_its_resource_once() {
        use std::io::{BufRead, BufReader, Write};

        let _g = fresh_registry();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port() as i32;
        let (tx, rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for _ in 0..250 {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        let mut reader = BufReader::new(socket.try_clone().expect("clone socket"));
                        let mut first = String::new();
                        reader.read_line(&mut first).expect("request line");
                        loop {
                            let mut line = String::new();
                            reader.read_line(&mut line).expect("request header");
                            if line == "\r\n" || line.is_empty() {
                                break;
                            }
                        }
                        requests.push(first);
                        socket
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .expect("stop response");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(4));
                    }
                    Err(error) => panic!("accept stop: {error}"),
                }
            }
            tx.send(requests).expect("publish stop requests");
        });

        let sid = crate::plex::register_for_test(
            "direct-pending-stop",
            "127.0.0.1",
            port,
            "token",
            "direct-stop-client",
        );
        let candidate_url = crate::plex::client_for(sid)
            .unwrap()
            .direct_play_url("/library/parts/1/file.mkv", "direct-stop-logical")
            .to_url();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                sid,
                sess: "direct-stop-logical".into(),
                url: "http://fixture.invalid/hls/master.m3u8".into(),
                tsession: "direct-stop-hls".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P480.ceiling()),
                auto_original: Some(AutoOriginalCandidate {
                    url: candidate_url,
                    probe_part: "/library/parts/1/file.mkv".into(),
                    direct: true,
                    vcodec: "h264".into(),
                    acodec: "aac".into(),
                    fps: 24.0,
                    dovi: crate::metadata::Dovi::NONE,
                    immersive: false,
                    audio_sid: 0,
                    audio_ordinal: None,
                    subtitle_ordinal: None,
                }),
                ..Default::default()
            },
            "42",
        );
        assert_eq!(
            recover_auto_to_original(120),
            Some(AutoOriginalReload::Direct),
        );
        assert!(original_recovery_pending());

        scrobble_stop(None, None);
        drop_original_recovery();
        drain_scrobble();
        let requests = rx.recv().expect("captured teardown stops");
        server.join().unwrap();
        let stops: Vec<_> = requests
            .iter()
            .filter(|line| line.contains("/stop?"))
            .collect();
        assert_eq!(
            stops.len(),
            1,
            "one retained resource has one final owner and one exact close: {requests:?}",
        );

        restore_quality(Quality::Original);
        reset_session();
        install_active_encoder("");
        crate::plex::reset_servers_for_test();
        crate::player::reset_audio_track();
        crate::player::reset_subtitle();
    }

    #[test]
    fn direct_recovery_without_its_server_keeps_hls_instead_of_using_a_logical_alias() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                sid: unregistered_sid(),
                sess: "missing-logical".into(),
                url: "http://fixture.invalid/hls/master.m3u8".into(),
                tsession: "missing-hls".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P480.ceiling()),
                auto_original: Some(AutoOriginalCandidate {
                    url: "http://missing.invalid/source.mkv?X-Plex-Session-Identifier=missing-logical".into(),
                    probe_part: "/library/parts/1/file.mkv".into(),
                    direct: true,
                    vcodec: "h264".into(),
                    acodec: "aac".into(),
                    fps: 24.0,
                    dovi: crate::metadata::Dovi::NONE,
                    immersive: false,
                    audio_sid: 0,
                    audio_ordinal: None,
                    subtitle_ordinal: None,
                }),
                ..Default::default()
            },
            "42",
        );

        assert_eq!(recover_auto_to_original(120), None);
        assert_eq!(url(), "http://fixture.invalid/hls/master.m3u8");
        assert_eq!(active_encoder(), "missing-hls");
        assert!(!original_recovery_pending());

        restore_quality(Quality::Original);
        reset_session();
        install_active_encoder("");
    }

    #[test]
    fn manually_picking_original_restores_native_dolby_vision_instead_of_retranscoding() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                url: "https://example.invalid/hls/master.m3u8".into(),
                tsession: "encoder-1".into(),
                delivery: crate::plex::TranscodeDelivery::FixedHls {
                    seconds_per_segment: 2,
                },
                ceiling: Some(crate::abr::Rung::P1080High.ceiling()),
                transport_kbps: 28_000,
                auto_original: Some(AutoOriginalCandidate {
                    url: "https://example.invalid/source.mkv".into(),
                    probe_part: "https://example.invalid/source.mkv".into(),
                    direct: true,
                    vcodec: "hevc".into(),
                    acodec: "eac3".into(),
                    fps: 23.976,
                    dovi: p8(),
                    immersive: true,
                    audio_sid: 42,
                    audio_ordinal: Some(1),
                    subtitle_ordinal: None,
                }),
                ..Default::default()
            },
            "rk-auto",
        );

        crate::player::note_original_failure(crate::player::ABR_FAILURE_ORIGINAL_HTTP, 503);

        set_quality(Quality::Original);
        assert_eq!(quality(), Quality::Original);
        assert!(
            matches!(
                cur_delivery(),
                crate::plex::TranscodeDelivery::FixedHls { .. }
            ),
            "the pump owns the pending codec-changing reload; the menu must not pre-mutate it"
        );
        assert_eq!(
            recover_auto_to_original(120),
            Some(AutoOriginalReload::Direct)
        );
        assert_eq!(url(), "https://example.invalid/source.mkv");
        assert_eq!(stream_vcodec(), "hevc");
        assert_eq!(
            stream_dovi(),
            p8(),
            "the native Load must regain its Dolby Vision declaration"
        );
        assert_eq!(
            crate::player::SHARED
                .abr_failure_kind
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "the new Original attempt supersedes the old probe's failure",
        );
        assert!(!is_transcoding());
        assert!(
            auto_original_watch().is_none(),
            "manual Original is not adaptive after the jump"
        );

        reset_session();
        install_active_encoder("");
        crate::player::reset_audio_track();
        crate::player::reset_subtitle();
    }

    /// Local Auto begins on Original, but it must retain the same source candidate as Remote:
    /// after the user selects any fixed rung, Manual Original needs the exact direct/remux
    /// declaration to return to. Without it the Local route has no recovery target and asks for
    /// one more encoder.
    #[test]
    fn local_auto_preserves_the_candidate_needed_to_leave_a_fixed_rung() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        let sid = crate::plex::register_for_test(
            "machine-local",
            "<peer-host-1>.example.invalid",
            32400,
            "token",
            "test-client-id",
        );
        crate::plex::client_for(sid)
            .expect("server installed")
            .set_link(crate::plex::probe::Location::Local);
        apply_plan(
            Plan {
                sid,
                url: "https://example.invalid/source.mkv".into(),
                delivery: crate::plex::TranscodeDelivery::ProgressiveMkv,
                ceiling: None,
                src_measure: (6_381, 3_832, 2_152),
                auto_original: Some(AutoOriginalCandidate {
                    url: "https://example.invalid/source.mkv".into(),
                    probe_part: "https://example.invalid/source.mkv".into(),
                    direct: true,
                    vcodec: "hevc".into(),
                    acodec: "aac".into(),
                    fps: 25.0,
                    dovi: crate::metadata::Dovi::NONE,
                    immersive: false,
                    audio_sid: 14_778,
                    audio_ordinal: Some(0),
                    subtitle_ordinal: None,
                }),
                ..Default::default()
            },
            "rk-local-original",
        );

        assert!(
            session().auto_original.is_some(),
            "the route contract must be testable without network by asserting the plan/candidate path"
        );

        reset_session();
        restore_quality(Quality::Original);
        crate::plex::reset_servers_for_test();
    }

    /// Returning from a fixed rung to Auto on a Local server must not confuse "the link needs no
    /// proof" with "the source needs no feasibility check". Differential against the old route:
    /// Local alone set `auto_original = true`, selected progressive MKV, and left this AV1-shaped
    /// playback without an HLS controller after the reload.
    #[test]
    fn local_auto_keeps_hls_when_original_is_infeasible() {
        let _g = fresh_registry();
        restore_quality(Quality::P720);
        let sid = crate::plex::register_for_test(
            "machine-local-infeasible",
            "<peer-host-1>.example.invalid",
            32400,
            "token",
            "test-client-id",
        );
        crate::plex::client_for(sid)
            .expect("server installed")
            .set_link(crate::plex::probe::Location::Local);
        apply_plan(
            Plan {
                sid,
                tsession: "encoder-fixed".into(),
                delivery: crate::plex::TranscodeDelivery::ProgressiveMkv,
                ceiling: Some(Quality::P720.ceiling().expect("fixed rung")),
                src_measure: (16_357, 3_840, 1_608),
                auto_original: None,
                ..Default::default()
            },
            "rk-local-infeasible",
        );
        install_active_encoder("encoder-fixed");

        set_quality(Quality::Auto);

        assert_eq!(quality(), Quality::Auto);
        assert!(
            matches!(
                cur_delivery(),
                crate::plex::TranscodeDelivery::FixedHls { .. }
            ),
            "Auto must rebuild the HLS controller when no native source candidate exists",
        );
        assert_eq!(
            cur_ceiling(),
            Some(crate::abr::Rung::P720.ceiling()),
            "handing a playing 4 Mbps route to Auto must not first replace it with 720 kbps",
        );
        assert!(crate::player::pending_transcode_refresh());

        reset_session();
        restore_quality(Quality::Original);
        install_active_encoder("");
        crate::plex::reset_servers_for_test();
    }

    /// The exact remote-control sequence from a 4K Original session: Manual 1080p replaces it
    /// with a capped encoder, and Manual Original must use the preserved source candidate to
    /// return to direct play. The bad old route kept the progressive-transcode flavor and asked
    /// for one more encoder refresh, because the recovery branch only recognized Fixed HLS.
    #[test]
    fn manual_original_after_a_fixed_rung_returns_to_the_native_source() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        apply_plan(
            Plan {
                url: "https://example.invalid/source.mkv".into(),
                tsession: String::new(),
                delivery: crate::plex::TranscodeDelivery::ProgressiveMkv,
                ceiling: None,
                src_measure: (6_381, 3_832, 2_152),
                transport_kbps: 6_381,
                auto_original: Some(AutoOriginalCandidate {
                    url: "https://example.invalid/source.mkv".into(),
                    probe_part: "https://example.invalid/source.mkv".into(),
                    direct: true,
                    vcodec: "hevc".into(),
                    acodec: "aac".into(),
                    fps: 25.0,
                    dovi: crate::metadata::Dovi::NONE,
                    immersive: false,
                    audio_sid: 14_778,
                    audio_ordinal: Some(0),
                    subtitle_ordinal: None,
                }),
                ..Default::default()
            },
            "rk-manual-original",
        );
        install_active_encoder("");

        set_quality(Quality::P1080High);
        assert_eq!(quality(), Quality::P1080High);
        assert_eq!(
            cur_ceiling(),
            Some(crate::plex::Ceiling {
                max_kbps: 20_000,
                max_w: 1920,
                max_h: 1080
            }),
            "a 3832x2152 source cannot fit the 1080p rung, so this pick legitimately starts a cap"
        );
        assert!(
            crate::player::pending_transcode_refresh(),
            "the fixed rung still asks the pump for an encoder reload"
        );
        // The route state the pump owns after that first transition lands.
        session_mut(|s| {
            s.tsession = "encoder-1080".into();
            s.cur_remux = false;
            s.cur_no_video_copy = false;
        });
        install_active_encoder("encoder-1080");

        set_quality(Quality::Original);
        assert_eq!(quality(), Quality::Original);
        assert!(
            !crate::player::pending_transcode_refresh(),
            "Original must not build another capped encoder"
        );
        assert_eq!(
            cur_delivery(),
            crate::plex::TranscodeDelivery::ProgressiveMkv,
            "the pending recovery owns the route; the pump will perform the native reload"
        );
        assert_eq!(
            recover_auto_to_original(27),
            Some(AutoOriginalReload::Direct),
            "the pump must restore the exact direct source at the current position"
        );
        assert_eq!(url(), "https://example.invalid/source.mkv");
        assert_eq!(stream_vcodec(), "hevc");
        assert_eq!(stream_acodec(), "aac");
        assert!(!is_transcoding());
        assert_eq!(cur_ceiling(), None);
        assert_eq!(
            cur_delivery(),
            crate::plex::TranscodeDelivery::ProgressiveMkv
        );
        assert!(
            auto_original_watch().is_none(),
            "manual Original is not adaptive"
        );

        reset_session();
        install_active_encoder("");
        crate::player::reset_audio_track();
        crate::player::reset_subtitle();
    }

    /// Manual Original and Auto Original use the same URL and decoder declaration, but not the
    /// same demux worker: Auto's worker owns an `OriginalModeController`. Merely changing the route
    /// flag leaves the already-running Manual worker alive with the `None` it captured at spawn,
    /// which is the photographed `Auto · controller idle / no adaptive session` state.
    #[test]
    fn original_to_auto_restarts_the_worker_to_arm_the_watchdog() {
        let _g = fresh_registry();
        restore_quality(Quality::Original);
        let sid = crate::plex::register_for_test(
            "machine-local-original-auto",
            "<peer-host-1>.example.invalid",
            32400,
            "token",
            "test-client-id",
        );
        crate::plex::client_for(sid)
            .expect("server installed")
            .set_link(crate::plex::probe::Location::Local);
        apply_plan(
            Plan {
                sid,
                url: "https://example.invalid/source.mkv".into(),
                delivery: crate::plex::TranscodeDelivery::ProgressiveMkv,
                ceiling: None,
                src_measure: (23_920, 3_840, 2_160),
                transport_kbps: 23_920,
                auto_original: Some(AutoOriginalCandidate {
                    url: "https://example.invalid/source.mkv".into(),
                    probe_part: "https://example.invalid/source.mkv".into(),
                    direct: true,
                    vcodec: "hevc".into(),
                    acodec: "eac3".into(),
                    fps: 24.0,
                    dovi: crate::metadata::Dovi::NONE,
                    immersive: true,
                    audio_sid: 42,
                    audio_ordinal: Some(0),
                    subtitle_ordinal: None,
                }),
                ..Default::default()
            },
            "rk-original-auto",
        );
        assert!(
            auto_original_watch().is_none(),
            "Manual Original has no watchdog"
        );

        set_quality(Quality::Auto);
        assert!(
            crate::player::pending_adaptive_reload(),
            "the live Manual worker must be replaced so it can capture that watchdog"
        );
        assert!(
            auto_original_watch().is_none(),
            "the old Manual worker may not be relabelled before the reload is claimed",
        );
        let action = claim_route_action().expect("the adaptive worker reload is explicit");
        assert_eq!(
            action.intent,
            RouteIntent::User(UserRouteIntent::AdaptiveReload),
        );
        finish_route_action(&action, RouteApplyResult::Prepared);
        assert!(
            auto_original_watch().is_some(),
            "the committed Auto route enables the watchdog for the replacement worker",
        );
        assert!(
            !crate::player::pending_transcode_refresh(),
            "the source and decoder declaration did not change, so this is not a new encode"
        );

        reset_session();
        restore_quality(Quality::Original);
        crate::player::reset_route_requests_for_test();
        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn auto_to_an_admitting_fixed_rung_restarts_the_worker_to_remove_the_watchdog() {
        let _g = fresh_registry();
        restore_quality(Quality::Auto);
        let sid = crate::plex::register_for_test(
            "machine-auto-fixed-direct",
            "<peer-host-1>.example.invalid",
            32400,
            "token",
            "test-client-id",
        );
        crate::plex::client_for(sid)
            .expect("server installed")
            .set_link(crate::plex::probe::Location::Local);
        apply_plan(
            Plan {
                sid,
                url: "https://example.invalid/source.mkv".into(),
                delivery: crate::plex::TranscodeDelivery::ProgressiveMkv,
                src_measure: (3_000, 1_280, 720),
                transport_kbps: 3_256,
                auto_original_watched: true,
                auto_original: Some(AutoOriginalCandidate {
                    url: "https://example.invalid/source.mkv".into(),
                    probe_part: "https://example.invalid/source.mkv".into(),
                    direct: true,
                    vcodec: "h264".into(),
                    acodec: "aac".into(),
                    fps: 24.0,
                    dovi: crate::metadata::Dovi::NONE,
                    immersive: false,
                    audio_sid: 42,
                    audio_ordinal: Some(0),
                    subtitle_ordinal: None,
                }),
                ..Default::default()
            },
            "rk-auto-fixed-direct",
        );
        assert!(auto_original_watch().is_some());

        set_quality(Quality::P1080High);

        assert!(
            auto_original_watch().is_none(),
            "the manual rung has no Auto watchdog"
        );
        assert!(
            crate::player::pending_adaptive_reload(),
            "same direct bytes still need a new non-adaptive worker",
        );
        assert!(
            !crate::player::pending_transcode_refresh(),
            "the 3 Mbps 720p source already satisfies the 20 Mbps fixed rung",
        );

        reset_session();
        restore_quality(Quality::Original);
        crate::player::reset_route_requests_for_test();
        crate::plex::reset_servers_for_test();
    }

    /// Manual Original is not Auto, but it is still a zero-encode route and must be recoverable
    /// after the user temporarily selects a fixed rung. This is the Depeche Mode shape: Original
    /// direct-play → 480p burned-subtitle transcode → Original.
    #[test]
    fn manual_original_after_a_fixed_rung_with_a_subtitle_returns_to_direct_play() {
        let _g = fresh_registry();
        restore_quality(Quality::Original);
        apply_plan(
            Plan {
                url: "https://example.invalid/source.mkv".into(),
                tsession: String::new(),
                delivery: crate::plex::TranscodeDelivery::ProgressiveMkv,
                ceiling: None,
                src_measure: (6_381, 3_832, 2_152),
                transport_kbps: 6_381,
                auto_original: Some(AutoOriginalCandidate {
                    url: "https://example.invalid/source.mkv".into(),
                    probe_part: "https://example.invalid/source.mkv".into(),
                    direct: true,
                    vcodec: "hevc".into(),
                    acodec: "aac".into(),
                    fps: 25.0,
                    dovi: crate::metadata::Dovi::NONE,
                    immersive: false,
                    audio_sid: 14_778,
                    audio_ordinal: Some(0),
                    subtitle_ordinal: Some(3),
                }),
                ..Default::default()
            },
            "rk-manual-original-subtitle",
        );
        install_active_encoder("");

        set_quality(Quality::P480);
        session_mut(|s| {
            s.tsession = "encoder-480".into();
            s.cur_remux = false;
            s.cur_no_video_copy = false;
        });
        install_active_encoder("encoder-480");

        set_quality(Quality::Original);
        assert_eq!(
            cur_delivery(),
            crate::plex::TranscodeDelivery::ProgressiveMkv,
            "Original requests the native reload; it must not build another capped encoder"
        );
        assert_eq!(
            recover_auto_to_original(938),
            Some(AutoOriginalReload::Direct),
            "a burned fixed rung must still return to the exact direct source"
        );
        assert_eq!(url(), "https://example.invalid/source.mkv");
        assert_eq!(stream_vcodec(), "hevc");
        assert_eq!(stream_acodec(), "aac");
        assert!(!is_transcoding());
        assert_eq!(
            crate::player::desired_sub_idx(),
            3,
            "the subtitle returns to client rendering"
        );

        reset_session();
        install_active_encoder("");
        crate::player::reset_audio_track();
        crate::player::reset_subtitle();
    }

    /// The `/identity` cache is keyed to the server that taught it. It feeds
    /// `uri=server://{machineIdentifier}/…` on the PlayQueue POST, so one cached globally is
    /// server A's fingerprint sent to server B — naming a machine B has never heard of, on a POST
    /// that is best-effort and therefore fails silently.
    #[test]
    fn the_machine_id_cache_is_scoped_to_the_server_that_taught_it() {
        let _g = fresh_registry();
        let a = ServerId::from_raw((crate::plex::MAX_SERVERS - 2) as u16);
        let b = ServerId::from_raw((crate::plex::MAX_SERVERS - 1) as u16);
        assert!(crate::plex::client_for(a).is_none() && crate::plex::client_for(b).is_none());

        apply_plan(
            Plan {
                sid: a,
                machine_id: "MACHINE-A".into(),
                ..Default::default()
            },
            "rk-a",
        );
        assert_eq!(
            ResolveEnv::snapshot(a, "rk-a").machine_id,
            "MACHINE-A",
            "its own server reuses it"
        );
        assert_eq!(
            ResolveEnv::snapshot(b, "rk-b").machine_id,
            "",
            "another server must re-ask rather than inherit A's fingerprint"
        );

        // …and an empty `machine_id` means "leave the cache alone", not "the cache is now B's"
        apply_plan(
            Plan {
                sid: b,
                ..Default::default()
            },
            "rk-b",
        );
        assert_eq!(ResolveEnv::snapshot(a, "rk-a").machine_id, "MACHINE-A");
        assert_eq!(ResolveEnv::snapshot(b, "rk-b").machine_id, "");
    }

    /// A one-shot loopback PMS: accepts ONE connection, hands its request line back down the
    /// channel, and answers 200 so the client's read terminates. Real sockets, like `stream.rs`'s
    /// own tests — which server a POST actually reached is the only thing the timeline routing can
    /// be graded on without a television.
    fn stub_pms() -> (
        i32,
        std::sync::mpsc::Receiver<String>,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{BufRead, BufReader, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port() as i32;
        let (tx, rx) = std::sync::mpsc::channel();
        let h = std::thread::spawn(move || {
            if let Some(Ok(s)) = l.incoming().next() {
                let mut line = String::new();
                let _ = BufReader::new(&s).read_line(&mut line);
                let mut s = s;
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
                let _ = tx.send(line);
            }
        });
        (port, rx, h)
    }

    fn ordered_stub_pms(
        label: &'static str,
        tx: std::sync::mpsc::Sender<(&'static str, String)>,
    ) -> (i32, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port() as i32;
        let handle = std::thread::spawn(move || {
            if let Some(Ok(socket)) = listener.incoming().next() {
                let mut line = String::new();
                let _ = BufReader::new(&socket).read_line(&mut line);
                let _ = tx.send((label, line));
                let mut socket = socket;
                let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            }
        });
        (port, handle)
    }

    /// **The one that had to ship in the same commit as `cur_sid`.** The `/:/timeline` report runs
    /// on a worker every ten seconds and used to read the current server fresh on each tick — the
    /// only place in the playback path with no capture at all. Split from the rest, the app
    /// resolves correctly, plays correctly, and quietly writes the resume point of a friend's film
    /// onto your own server for as long as it plays.
    ///
    /// Two servers on loopback, the item playing from B, the user browsing A: the POST must land on
    /// B. The closing report to A is the control — it proves the two stubs are distinguishable, so
    /// "A heard nothing" is a fact about the routing and not about a listener that never worked.
    #[test]
    fn the_timeline_reaches_the_server_the_item_came_from_not_the_current_one() {
        use std::time::Duration;
        let _g = fresh_registry();
        let (pa, rx_a, ha) = stub_pms();
        let (pb, rx_b, hb) = stub_pms();
        // `register_for_test`, not the public `register`: the latter resolves the device id through
        // `session::load`, which mints and PERSISTS a uuid on a host that has no session file.
        let a = crate::plex::register_for_test(
            "route-test-A",
            "127.0.0.1",
            pa,
            "tok-a",
            "cid-route-test",
        );
        let b = crate::plex::register_for_test(
            "route-test-B",
            "127.0.0.1",
            pb,
            "tok-b",
            "cid-route-test",
        );
        assert_ne!(a, b, "two servers, two slots");

        // an item from B starts playing, then the user walks back to their OWN server's Home
        apply_plan(
            Plan {
                sid: b,
                url: "https://example.invalid/b.mkv".into(),
                sess: "timeline-session-b".into(),
                ..Default::default()
            },
            "rk-b",
        );
        assert!(crate::plex::set_current(a));
        assert_eq!(
            cur_sid(),
            b,
            "what is PLAYING does not move when the browsed server does"
        );

        let lease_b = begin_timeline_reporting().expect("B timeline lease");
        assert!(report_timeline(
            &lease_b,
            crate::plex::TimelineState::Playing,
            1_000,
            2_000,
        ));
        let got = rx_b
            .recv_timeout(Duration::from_secs(5))
            .expect("B never received the report");
        assert!(
            got.contains("ratingKey=rk-b"),
            "B got something else: {got}"
        );
        assert!(
            rx_a.recv_timeout(Duration::from_millis(300)).is_err(),
            "the current server must not receive another server's progress"
        );

        // control: a complete A projection reaches A, so the assertion above is about routing.
        apply_plan(
            Plan {
                sid: a,
                url: "https://example.invalid/a.mkv".into(),
                sess: "timeline-session-a".into(),
                ..Default::default()
            },
            "rk-a",
        );
        let lease_a = begin_timeline_reporting().expect("A timeline lease");
        assert!(report_timeline(
            &lease_a,
            crate::plex::TimelineState::Stopped,
            0,
            2_000,
        ));
        let got = rx_a
            .recv_timeout(Duration::from_secs(5))
            .expect("A never received its own report");
        assert!(
            got.contains("ratingKey=rk-a"),
            "A got something else: {got}"
        );

        ha.join().unwrap();
        hb.join().unwrap();
        // Hand the table back empty. Both stubs' ports close as this returns, so anything left
        // registered is a client that answers nothing — and `CURRENT` still points at one of them.
        // The session is idled with it for the same reason, one level up: it is still holding `b`
        // as the playing server, i.e. a `ServerId` into the table being emptied.
        crate::plex::reset_servers_for_test();
        reset_session();
    }

    #[test]
    fn replacement_timeline_waits_for_the_announced_old_stop_boundary() {
        use std::time::Duration;

        let _g = fresh_registry();
        drain_scrobble();
        reset_session();
        reset_player_control_for_test();
        let (order_tx, order_rx) = std::sync::mpsc::channel();
        let (old_port, old_server) = ordered_stub_pms("old", order_tx.clone());
        let (new_port, new_server) = ordered_stub_pms("new", order_tx);
        let old_sid = crate::plex::register_for_test(
            "timeline-stop-old",
            "127.0.0.1",
            old_port,
            "old-token",
            "timeline-client",
        );
        let new_sid = crate::plex::register_for_test(
            "timeline-stop-new",
            "127.0.0.1",
            new_port,
            "new-token",
            "timeline-client",
        );
        apply_plan(
            Plan {
                sid: old_sid,
                sess: "logical-old".into(),
                ..Default::default()
            },
            "rk-old-stop",
        );
        // The test isolates timeline ordering; avoid adding a second transcode-stop request to the
        // one-shot old PMS after its stopped report.
        install_active_encoder("");

        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        struct ReleaseOnDrop(Option<std::sync::mpsc::Sender<()>>);
        impl Drop for ReleaseOnDrop {
            fn drop(&mut self) {
                if let Some(tx) = self.0.take() {
                    let _ = tx.send(());
                }
            }
        }
        let mut release = ReleaseOnDrop(Some(release_tx));
        let old_reporter = std::thread::spawn(move || {
            let _ = release_rx.recv();
        });
        scrobble_stop(
            Some(("rk-old-stop".into(), 11_000, 20_000)),
            Some(old_reporter),
        );

        apply_plan(
            Plan {
                sid: new_sid,
                url: "https://example.invalid/new.mkv".into(),
                sess: "logical-new".into(),
                ..Default::default()
            },
            "rk-new-playing",
        );
        let lease = begin_timeline_reporting().expect("replacement reporter lease");
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let replacement = std::thread::spawn(move || {
            let sent = report_timeline(&lease, crate::plex::TimelineState::Playing, 1_000, 20_000);
            let _ = done_tx.send(sent);
        });

        assert!(
            order_rx.recv_timeout(Duration::from_millis(250)).is_err(),
            "replacement playing escaped before the old reporter/stop boundary",
        );
        release.0.take().unwrap().send(()).unwrap();
        let (first_label, old) = order_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("old stopped report never reached PMS");
        assert_eq!(
            first_label, "old",
            "replacement report arrived before stopped"
        );
        assert!(
            old.contains("ratingKey=rk-old-stop"),
            "wrong old report: {old}"
        );
        assert!(
            old.contains("state=stopped"),
            "old report was not stopped: {old}"
        );
        let (second_label, new) = order_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("replacement playing report never reached PMS");
        assert_eq!(second_label, "new", "old server received an extra request");
        assert!(
            new.contains("ratingKey=rk-new-playing"),
            "wrong new report: {new}"
        );
        assert!(
            new.contains("state=playing"),
            "new report was not playing: {new}"
        );
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(1)), Ok(true));

        replacement.join().unwrap();
        drain_scrobble();
        old_server.join().unwrap();
        new_server.join().unwrap();
        crate::plex::reset_servers_for_test();
        reset_session();
        install_active_encoder("");
        reset_player_control_for_test();
    }

    #[test]
    fn every_concurrent_scrobble_drain_waits_for_the_same_taken_handle() {
        use std::time::Duration;

        let join = std::sync::Arc::new(ScrobbleJoin::new());
        let generation = join.reserve();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = release_rx.recv();
        });
        join.install(generation, worker);

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let first_join = join.clone();
        let first_done = done_tx.clone();
        let first = std::thread::spawn(move || {
            first_join.drain();
            let _ = first_done.send(1);
        });
        let second_join = join.clone();
        let second = std::thread::spawn(move || {
            second_join.drain();
            let _ = done_tx.send(2);
        });

        assert!(
            done_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "a drainer escaped after another thread took the JoinHandle",
        );
        release_tx.send(()).unwrap();
        let mut completed = [
            done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        ];
        completed.sort_unstable();
        assert_eq!(completed, [1, 2]);
        first.join().unwrap();
        second.join().unwrap();
    }

    #[test]
    fn timeline_lease_cannot_cross_engine_teardown() {
        let _g = crate::testlock::serial();
        reset_player_control_for_test();
        reset_session();
        apply_plan(
            Plan {
                sid: ServerId::from_raw(0),
                url: "https://example.invalid/old.mkv".into(),
                sess: "logical-old".into(),
                pq_id: "pq-old".into(),
                pq_item_id: "pqi-old".into(),
                audio_sid: 7,
                sub_sid: 9,
                ..Default::default()
            },
            "rk-old",
        );
        install_active_encoder("wire-old");
        let old = begin_timeline_reporting().expect("old reporter");
        let before = timeline_snapshot(&old, crate::plex::TimelineState::Playing, 1_000, 2_000)
            .expect("old projection");
        assert_eq!(before.rating_key, "rk-old");
        assert_eq!(before.session, "wire-old");
        assert_eq!((before.audio_stream_id, before.subtitle_stream_id), (7, 9));

        begin_engine_teardown(true);
        assert!(
            timeline_snapshot(&old, crate::plex::TimelineState::Playing, 1_500, 2_000).is_none(),
            "an old reporter must not sample any field after its Engine is retired"
        );
        reset_player_control_for_test();
        reset_session();
    }
}
