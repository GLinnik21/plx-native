//! play_movie route selection (direct-play vs transcode) + the stream URL, transcode
//! session, and HUD strings — all private module state, held as ONE [`Session`] value. The
//! player engine reads the URL/session through the accessors here; ui::player_hud reads the
//! HUD strings through title_cptr()/ctxline_cptr().
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::Mutex;
use crate::pms::PmsMovie;
use crate::plex::ServerId;
use std::os::raw::c_char;
use std::ptr::{addr_of, addr_of_mut};

// ---- ONE playback session, as ONE value -----------------------------------------------------

/// Everything this module knows about the playback in progress, in one struct.
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
/// **MAIN THREAD ONLY.** That is what keeps a `static mut` sound here, and it is why
/// [`ResolveEnv`] exists: the resolve worker is handed owned copies and reads none of this. The
/// accessors that lend rather than copy — [`play_verdict`], [`up_next`] and [`with_queue`], plus
/// the raw pointers [`title_cptr`]/[`ctxline_cptr`] hand to `draw_text` — stay valid until the next
/// main-thread write, and that write is [`apply_plan`] or [`request_play`], neither of which can
/// run inside a frame's draw.
struct Session {
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
        url: String::new(),
        tsession: String::new(),
        play_verdict: None,
        cur_remux: false,
        cur_delivery: crate::plex::TranscodeDelivery::ProgressiveMkv,
        cur_no_video_copy: false,
        cur_ceiling: None,
        cur_src: (0, 0, 0),
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
pub(crate) fn set_url(s: &str) {
    session_mut(|x| x.url = s.to_owned())
}
pub(crate) fn clear_url() {
    session_mut(|x| x.url.clear())
}
pub(crate) fn transcode_session() -> String {
    session().tsession.clone()
}

/// Thread-safe physical encoder identity. `Session::tsession` remains the main-thread playback
/// classification bit; adaptive HLS can replace the server encoder from its demux worker without
/// racing that `static mut` state. Teardown atomically takes this value, so a late candidate can
/// never publish itself after the stop owner has retired the playback.
static ACTIVE_ENCODER: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

fn install_active_encoder(value: &str) {
    *ACTIVE_ENCODER.lock().unwrap_or_else(|e| e.into_inner()) = value.to_owned();
}

fn take_active_encoder() -> String {
    std::mem::take(&mut *ACTIVE_ENCODER.lock().unwrap_or_else(|e| e.into_inner()))
}

fn replace_active_encoder(expected: &str, replacement: &str) -> bool {
    let mut active = ACTIVE_ENCODER.lock().unwrap_or_else(|e| e.into_inner());
    if *active != expected {
        return false;
    }
    *active = replacement.to_owned();
    true
}

fn is_active_encoder(expected: &str) -> bool {
    *ACTIVE_ENCODER.lock().unwrap_or_else(|e| e.into_inner()) == expected
}

/// Owned, worker-safe inputs for HLS replacement sessions. Constructed on the main thread before
/// the demux worker starts; it never reads the mutable route session afterwards.
#[derive(Clone)]
pub(crate) struct HlsAbrControl {
    sid: ServerId,
    rating_key: String,
    logical_session: String,
    audio_stream_id: i64,
    subtitle_stream_id: i64,
    seconds_per_segment: u8,
}

pub(crate) struct PrimedHls {
    pub(crate) url: String,
    pub(crate) encoder_session: String,
}

impl HlsAbrControl {
    /// Register a distinct fixed-rendition encoder at the current content boundary. The old
    /// encoder remains active and readable; this only returns the candidate's master URL.
    pub(crate) fn prime(
        &self,
        expected_encoder: &str,
        proposal: crate::abr::Proposal,
        generation: u64,
        offset_secs: i64,
    ) -> Option<PrimedHls> {
        if !is_active_encoder(expected_encoder) {
            return None;
        }
        let client = crate::plex::client_for(self.sid)?;
        let encoder_session = format!("{}-abr-{generation}", self.logical_session);
        // PMS exposes the two session fields separately, but the overlap TV spike proved it
        // cannot prime a replacement while it shares the old X-Plex id: the first encoder dies
        // before the candidate produces segment zero. Couple both wire fields per encoder.
        let spec = transcode_spec(
            &self.rating_key,
            &encoder_session,
            &encoder_session,
            false,
            true,
            offset_secs.max(0),
            self.audio_stream_id,
            self.subtitle_stream_id,
            Some(proposal.rung.ceiling()),
            crate::plex::TranscodeDelivery::FixedHls {
                seconds_per_segment: self.seconds_per_segment,
            },
        );
        let decision = client.transcode_decision(&spec)?;
        if refusal(&decision).is_some() || !is_active_encoder(expected_encoder) {
            let _ = client.transcode_stop(&encoder_session);
            return None;
        }
        Some(PrimedHls {
            url: client.transcode_start_url(&spec).to_url(),
            encoder_session,
        })
    }

    /// Publish a successfully primed encoder. A concurrent teardown wins the compare, in which
    /// case the candidate is stopped and never becomes live. Retiring the old encoder is separate
    /// so the media worker can enqueue the primed AUs without blocking on that control request.
    pub(crate) fn commit(&self, expected_encoder: &str, candidate: &str) -> bool {
        let Some(client) = crate::plex::client_for(self.sid) else { return false };
        if !replace_active_encoder(expected_encoder, candidate) {
            let _ = client.transcode_stop(candidate);
            return false;
        }
        true
    }

    pub(crate) fn retire(&self, encoder: String) {
        let Some(client) = crate::plex::client_for(self.sid) else { return };
        let worker_encoder = encoder.clone();
        if crate::task::spawn_small_keeping("abr-stop", move || {
            let ok = client.transcode_stop(&worker_encoder);
            crate::player::log(&format!("abr: retired previous encoder ok={}", ok as i32));
        })
        .is_none()
        {
            // Thread refusal is extraordinarily remote, but leaving the old encoder running is a
            // server-side resource leak. The control-plane request is bounded; pay it here only on
            // that already-degraded path.
            let ok = client.transcode_stop(&encoder);
            crate::player::log(&format!("abr: synchronously retired previous encoder ok={}", ok as i32));
        }
    }

    pub(crate) fn abandon(&self, candidate: &str) {
        if let Some(client) = crate::plex::client_for(self.sid) {
            let _ = client.transcode_stop(candidate);
        }
    }
}

/// Main-thread capture immediately before spawning the HLS demux worker.
pub(crate) fn hls_abr_control() -> Option<(HlsAbrControl, String)> {
    if quality() != Quality::Auto {
        return None;
    }
    let seconds_per_segment = match cur_delivery() {
        crate::plex::TranscodeDelivery::FixedHls { seconds_per_segment } => seconds_per_segment,
        crate::plex::TranscodeDelivery::ProgressiveMkv => return None,
    };
    let encoder = ACTIVE_ENCODER.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if encoder.is_empty() {
        return None;
    }
    Some((
        HlsAbrControl {
            sid: cur_sid(),
            rating_key: cur_rk(),
            logical_session: sess(),
            audio_stream_id: cur_audio_sid(),
            subtitle_stream_id: cur_sub_sid(),
            seconds_per_segment,
        },
        encoder,
    ))
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
    session_mut(|s| s.play_verdict = None)
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
    session().stream_dovi
}
/// Is the audio being fed a Dolby Atmos stream? — the Load payload's `contents.immersive` node.
/// See [`Session::stream_immersive`].
pub(crate) fn stream_immersive() -> bool {
    session().stream_immersive
}
/// Override the audio codec used to build the Load payload — set by a native audio-track
/// switch to the chosen track's codec before the direct-play reload.
pub(crate) fn set_stream_acodec(codec: &str) {
    session_mut(|s| s.stream_acodec = codec.to_owned())
}
/// Record the streamed item's video+audio codec pair in one write (the Load-payload source of
/// truth) — outside `apply_decision_codecs`, the two fields are only ever set together.
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
/// ONE write for the reason [`set_stream_codecs`] is one write and [`apply_plan`] is a single
/// struct assignment: these five fields describe ONE stream, and a half-applied set is a payload
/// that describes nothing real — 4K HEVC declared with the default `""` audio, say, which falls
/// through the engine's `_ =>` arm to `"AC3"` and stalls the sink on a Dolby Digital Plus track.
/// Four separate setters would be four ways to leave it half-written.
///
/// `apply_plan` (the PMS path) is the only other writer of the last three; keep them in step.
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
    matches!(cur_delivery(), crate::plex::TranscodeDelivery::FixedHls { .. })
}
pub(crate) fn source_vcodec() -> String {
    session().src_vcodec.clone()
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
    offset_secs: i64,
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
        offset_secs,
        ceiling,
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
    let session = if tsession.is_empty() { logical_session } else { tsession.clone() };
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
    let Some(c) = cur_client() else { return };
    // Serialise against a previous stop still in flight: these carry a position for a specific
    // item, and letting two race would let an older one land last. Normally free — the measured
    // baseline for a finished worker is 0 ms.
    drain_scrobble();
    let h = crate::task::spawn_small_keeping("scrobble", move || {
        // The progress reporter's last `playing` POST must land BEFORE this `stopped` one, or the
        // server is left believing playback continues. That ordering is why teardown used to join
        // it — on the main thread. Waiting for it HERE keeps the guarantee and moves the cost off
        // the frame loop.
        if let Some(t) = report_th {
            crate::task::join("timeline", t);
        }
        if let Some((rk, t_ms, d_ms)) = final_report {
            let ok = c.timeline(&crate::plex::TimelineReport {
                rating_key: &rk,
                state: crate::plex::TimelineState::Stopped,
                time_ms: t_ms,
                duration_ms: d_ms,
                session: &session,
                play_queue_id: &pq,
                play_queue_item_id: &pqi,
                audio_stream_id: aud,
                subtitle_stream_id: sub,
            });
            // This POST is the resume point, and it is the LAST thing the app says about the item —
            // nothing re-reads the position afterwards, so `ok=0` is the only trace a lost one
            // leaves. The line printed unconditionally before, i.e. it asserted a write it had no
            // idea had happened. APPENDED, never reordered: the harness reads these by regex.
            crate::log(&format!("timeline stopped t={}s/{}s ok={}", t_ms / 1000, d_ms / 1000, ok as i32));
        }
        if !tsession.is_empty() {
            // The other half of the stop, and it had no outcome at all until now: the GET's result
            // was discarded (`get_void`), so a stop that never reached the server read exactly like
            // one that did — and what a lost one leaves behind is a server still ENCODING into a
            // session nothing will ever read again. Logged beside the timeline line above and in the
            // same `ok=` shape, because the two are one event: this worker is the whole of what a
            // stop says to the server.
            let ok = c.transcode_stop(&tsession);
            crate::log(&format!("transcode stopped ok={}", ok as i32));
        }
    });
    *SCROBBLE.lock().unwrap_or_else(|e| e.into_inner()) = h;
}

/// The final scrobble still in flight, if any.
static SCROBBLE: std::sync::Mutex<Option<std::thread::JoinHandle<()>>> = std::sync::Mutex::new(None);

/// Wait for a pending [`scrobble_stop`] to reach the server.
///
/// Called from exactly two places: the next stop (so two reports for different items cannot land
/// out of order), and `plex_run`'s exit — because the process is about to die and a detached
/// worker dies with it, which would silently drop the resume point the user just earned. Blocking
/// there is the same cost the old inline call paid, except it is now paid ONCE at exit instead of
/// on every BACK out of a movie.
pub(crate) fn drain_scrobble() {
    let h = SCROBBLE.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(h) = h {
        crate::task::join("scrobble", h);
    }
}

/// Seek within a LIVE TRANSCODE by restarting it at a time offset — a transcode has
/// no byte-Cues, so a byte-Range seek can't work (docs/plex-api.md). Stops the current
/// encoder, then re-registers (/decision) and re-points the stream at the delivery-matched
/// start endpoint with `offset={secs}`. Returns the new URL (the demux re-opens it from byte 0),
/// or None if this playback isn't a transcode. Blocks on two HTTP round-trips (like
/// play_movie's /decision), which is fine during a seek (the pipeline is flushed).
pub(crate) fn transcode_seek(offset_secs: i64) -> Option<String> {
    if transcode_session().is_empty() {
        return None;
    }
    let rk = cur_rk();
    if rk.is_empty() {
        return None;
    }
    let c = cur_client()?;
    // NB: do NOT explicitly /stop the old encoder here — the session id is reused, so a stop
    // would cut the stream the demux thread is still reading out from under it. The caller
    // (the pump) instead reloads onto this new start.mkv?&offset= (same session), which tears
    // the old engine down — dropping its connection, and with it the old transcode.
    // /decision is just a query and doesn't cut the streaming connection.
    let encoder = ACTIVE_ENCODER.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if encoder.is_empty() {
        return None;
    }
    let sp = transcode_spec(&rk, &encoder, &encoder, is_remux(), is_no_video_copy(), offset_secs.max(0),
                            cur_audio_sid(), cur_sub_sid(), cur_ceiling(), cur_delivery());
    // same session, same output codecs — no payload rebuild here, so the body is unused
    let _ = c.transcode_decision(&sp);
    let url = c.transcode_start_url(&sp).to_url();
    set_url(&url);
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
/// persisted mode, offered and restored only behind [`auto_quality_ready`]; the gate is now open
/// because its fixed-session HLS, candidate-prime and encoder-swap path has landed.
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
    if auto_ready { &QUALITY_LADDER } else { &QUALITY_LADDER[1..] }
}

/// What the menu may offer in this build. Original and fixed ceilings are established playback
/// paths; Auto joins them only when [`auto_quality_ready`] says the adaptive path is complete.
pub(crate) fn available_quality_ladder() -> &'static [Quality] {
    quality_ladder_for(auto_quality_ready())
}

fn supported_quality(q: Quality) -> Quality {
    if q == Quality::Auto && !auto_quality_ready() { Quality::Original } else { q }
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
        Some(crate::plex::Ceiling { max_kbps, max_w, max_h })
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
        QUALITY_LADDER.get(i as usize).copied().unwrap_or(Quality::Original)
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
pub(crate) fn set_quality(q: Quality) {
    let q = supported_quality(q);
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
    let delivery = if q == Quality::Auto {
        crate::plex::TranscodeDelivery::FixedHls { seconds_per_segment: 2 }
    } else {
        crate::plex::TranscodeDelivery::ProgressiveMkv
    };
    let ceiling = if q == Quality::Auto {
        Some(crate::plex::Ceiling { max_kbps: 720, max_w: 854, max_h: 480 })
    } else {
        q.ceiling()
    };
    if cur_rk().is_empty() || (cur_ceiling() == ceiling && cur_delivery() == delivery) {
        return;
    }
    let (kbps, w, h) = session().cur_src;
    let admits = quality_policy(q, kbps, w, h).direct_play;
    session_mut(|s| {
        s.cur_ceiling = ceiling;
        s.cur_delivery = delivery;
        if matches!(delivery, crate::plex::TranscodeDelivery::FixedHls { .. }) {
            s.cur_remux = false;
        }
    });
    if admits && !is_transcoding() {
        return; // the picture on screen already satisfies the new rung
    }
    crate::player::log(&format!(
        "quality: {} picked — source {kbps}kbps {w}x{h}; re-transcoding this playback",
        q.label()
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
/// * **Auto always selects the encoded HLS path**. Its initial 480p ceiling is installed by
///   `build_stream`; later ceilings are owned by the segment controller, never this policy.
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
fn quality_policy(q: Quality, src_kbps: i64, src_w: i64, src_h: i64) -> crate::plex::LinkPolicy {
    if q == Quality::Auto {
        return crate::plex::LinkPolicy { direct_play: false, remux: false };
    }
    match q.ceiling() {
        None => crate::plex::LinkPolicy::UNRESTRICTED,
        Some(c) if c.admits(src_kbps, src_w, src_h) => crate::plex::LinkPolicy::UNRESTRICTED,
        Some(_) => crate::plex::LinkPolicy { direct_play: false, remux: false },
    }
}

/// **Two ceilings mean the stricter one**, per flavor, and this is the only place the two are put
/// together. A ceiling can only ever REMOVE a flavor: a fast link cannot restore what a low rung
/// denied, and a high rung cannot restore what a relay denied.
///
/// A named function rather than two `&&`s inline at the decision site, so the composition the
/// tests grade is literally the composition [`build_stream`] runs — a re-implementation in a test
/// would agree with itself forever while the shipped path drifted.
fn flavors_allowed(link: crate::plex::LinkPolicy, quality: crate::plex::LinkPolicy) -> crate::plex::LinkPolicy {
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
    let part = match mc.metadata.first().and_then(|m| m.media.first()).and_then(|md| md.part.first()) {
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
    let streams = mc.metadata.first().and_then(|m| m.media.first()).and_then(|md| md.part.first())
        .map(|p| &p.stream)?;
    let (mut vc, mut ac) = (None, None);
    for s in streams {
        match s.stream_type {
            1 if vc.is_none() && !s.codec.is_empty() => vc = Some(s.codec.to_lowercase()),
            2 if ac.is_none() && !s.codec.is_empty() => ac = Some(s.codec.to_lowercase()),
            _ => {}
        }
    }
    match (vc, ac) { (Some(v), Some(a)) => Some((v, a)), _ => None }
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

fn apply_decision_codecs(mc: &crate::plex::MediaContainer) {
    if let Some((vc, ac)) = decision_codecs(mc) {
        set_stream_codecs(&vc, &ac);
        crate::player::log(&format!("decision output: v={vc} a={ac}"));
    }

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
    crate::player::log(&format!("select streams: part={part} audio={aud} sub={sub} -> HTTP {st}"));
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
                q.id, q.selected_item_id, q.remaining, q.items.len(),
                up_next.as_ref().map(|u| format!("S{}E{} {}", u.season, u.index, u.rk))
                    .unwrap_or_else(|| "-".into())
            ));
            QueueInfo {
                machine_id: mid,
                id: if q.id > 0 { q.id.to_string() } else { String::new() },
                item_id: if q.selected_item_id > 0 { q.selected_item_id.to_string() } else { String::new() },
                up_next,
                rows: q.items,
            }
        }
        None => {
            crate::player::log("playqueue: POST failed");
            QueueInfo { machine_id: mid, ..Default::default() }
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
            machine_id: if s.machine_sid == sid { s.machine_id.clone() } else { String::new() },
            audio_sid: cur_audio_sid(),
            sub_sid: cur_sub_sid(),
            cached_item: crate::metadata::cached_playing(sid, rk),
            quality: quality(),
            src_kbps: crate::metadata::current().filter(|d| detail_describes(d, sid, rk)).map_or(0, source_kbps),
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
        || d.on_deck.as_ref().is_some_and(|ep| crate::plex::same_item((d.sid, &ep.rk), (sid, rk)))
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
    pub machine_id: String,   // "" = leave the cached one alone
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
    /// The fixed quality ceiling this plan resolved under (`None` = [`Quality::Original`]; Auto
    /// begins at its explicit 480p bootstrap rung) — installed as
    /// [`Session::cur_ceiling`] so a seek or a track switch rebuilds the SAME query. Copied
    /// straight from `env.quality.ceiling()`, for the same reason `sid` is copied from the env:
    /// the worker must not re-read a preference the main thread can move underneath it.
    pub ceiling: Option<crate::plex::Ceiling>,
    /// What this plan MEASURED the source at — `(kbps, w, h)`, any of them `0` for "nobody said".
    /// Carried so [`set_quality`] can re-ask [`quality_policy`] for the item already playing when
    /// the user picks a different rung, instead of guessing. See [`Session::cur_src`].
    pub src_measure: (i64, i64, i64),
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
    plan.playing = env.cached_item.clone().or_else(|| crate::metadata::fetch_playing_item(env.sid, rk));
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
    let (src_w, src_h) = plan.playing.as_ref().map(|p| (p.width, p.height)).unwrap_or((0, 0));
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
    let tracks = plan.playing.as_ref().map(|p| p.audio.as_slice()).unwrap_or(&[]);
    let audio_sel = if rk.is_empty() { None } else { pick_dp_audio(tracks, acodec) };
    // What the CONNECTION to this server allows, beside what the pipeline can decode: a Plex
    // relay is a ~2 Mbit/s tunnel, so neither of the two flavors that ship the file's own bytes
    // (direct play, and the uncapped container remux) can be asked for over one. Unrestricted on
    // every other tier and on a server whose link nobody has recorded, which is all of them today.
    // The reasoning, and what is measured versus documented, is at `plex::link_policy`.
    let link = crate::plex::link_policy(client.link());
    // …and what the USER has asked for, on top of what the link allows. Same two flags, composed
    // by AND, so the STRICTER of the two always wins: a relay link cannot be loosened by picking a
    // high rung, and a low rung is not rescued by a fast link. The reasoning — and why a ceiling
    // has to arrive HERE, before a flavor is chosen, rather than as a number on the spec — is at
    // `quality_policy` and `Quality`.
    let quality = quality_policy(env.quality, env.src_kbps, src_w, src_h);
    let allowed = flavors_allowed(link, quality);
    // The ceiling and the source it was judged against ride EVERY plan, including the direct-play
    // one that returns below. That is not bookkeeping: `set_quality` re-asks this same question
    // when the user picks a rung mid-film, and `retranscode` — which a track switch reaches from a
    // DIRECT PLAY (`player/pump.rs`'s own comment says so) — spends `cur_ceiling` on the encode it
    // then starts. Setting these only on the transcode branch left both reading `None`/zero for
    // every direct play, so the first audio switch after picking "480p · 720 kbps" re-encoded at
    // 4K/60 Mbps: the rung silently discarded on the one path where an encoder was actually
    // running.
    if env.quality == Quality::Auto {
        plan.delivery = crate::plex::TranscodeDelivery::FixedHls { seconds_per_segment: 2 };
        plan.ceiling = Some(crate::plex::Ceiling { max_kbps: 720, max_w: 854, max_h: 480 });
    } else {
        plan.ceiling = env.quality.ceiling();
    }
    plan.src_measure = (env.src_kbps, src_w, src_h);
    if !quality.direct_play {
        // Worth its own line for the reason the Dolby Vision one above is: from the outside this
        // is an ordinary h264/AAC MKV going to the transcoder for no visible reason, and the two
        // numbers that explain it (the rung, and what the source measured) appear nowhere else in
        // the log. `0` for the bitrate means nobody measured it — see `ResolveEnv::src_kbps`.
        crate::player::log(&format!(
            "route: quality ceiling {} — source {}kbps {src_w}x{src_h}; denying direct play + remux, re-encoding",
            env.quality.label(),
            env.src_kbps
        ));
    }
    let directplay = if !allowed.direct_play {
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
                if aidx >= 0 { p.audio.get(aidx as usize) } else { p.audio.iter().find(|a| a.selected) }
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
                plan.playing.as_ref()
                    .map(|p| crate::metadata::audio_ordinal(&p.audio, aidx as usize))
                    .unwrap_or(aidx),
            );
        }
        // honour a subtitle the server already has selected for this part (chosen on another
        // client, or by this app in an earlier session) — free here, since the direct-play path
        // renders subtitles itself. apply_plan installs it on the main thread.
        let sub_sel = plan.playing.as_ref().and_then(|p| pick_dp_subtitle(&p.subs));
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
        let achosen = audio_sel.as_ref().map(|(_, c, _)| c.clone()).unwrap_or_else(|| acodec.to_string());
        plan.vcodec = vcodec.to_string();
        plan.acodec = achosen;
    } else if matches!(plan.delivery, crate::plex::TranscodeDelivery::FixedHls { .. }) {
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
        -1,
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
fn pick_dp_audio(tracks: &[crate::metadata::Stream], default_acodec: &str) -> Option<(i32, String, i64)> {
    let dp = crate::plex::is_dp_audio;
    if tracks.is_empty() {
        // no track info — fall back to the codec-default (or transcode if that isn't DP)
        return if dp(default_acodec) { Some((-1, default_acodec.to_string(), 0)) } else { None };
    }
    let pick = |i: usize| (i as i32, tracks[i].codec.to_lowercase(), tracks[i].id);
    // 1. the server's own current selection, when it is a real pick (differs from the file's
    //    default flag — see the doc) and direct-playable: honours a choice made elsewhere
    if let Some(i) = tracks.iter().position(|s| s.selected && !s.default && dp(&s.codec.to_lowercase())) {
        return Some(pick(i));
    }
    // 2. preferred-language, direct-playable
    if let Some(i) = tracks.iter().position(|s| dp(&s.codec.to_lowercase()) && s.lang_code == PREF_AUDIO_LANG) {
        return Some(pick(i));
    }
    // 3. the file's flagged default track, if direct-playable (explicit index)
    if let Some(i) = tracks.iter().position(|s| s.default && dp(&s.codec.to_lowercase())) {
        return Some(pick(i));
    }
    if dp(default_acodec) && !tracks.iter().any(|s| s.default) {
        // Media[0].audioCodec is DP but no stream carries the default flag — codec-match
        return Some((-1, default_acodec.to_string(), 0));
    }
    // 4. any direct-playable track (smart direct-play over a non-DP default)
    tracks.iter().position(|s| dp(&s.codec.to_lowercase())).map(pick)
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
    let p = playback_preview_of(part, vcodec, d.width, d.height, d.dovi.presentation_now(), &d.audio)?;
    // The user's quality ceiling is the LAST gate `build_stream` applies, so it is the last one
    // here too — and it can only ever downgrade, never promote. Without this the facts row would
    // promise "Direct Play" for a source the rung is about to send to an encoder, which is the
    // exact mismatch this preview's doc says it exists to avoid. `d.bitrate`/`width`/`height` are
    // `Media[0]`'s, i.e. the same numbers `ResolveEnv` hands the resolve.
    Some(if p != Preview::Converts && !quality_policy(quality(), d.bitrate, d.width, d.height).direct_play {
        Preview::Converts
    } else {
        p
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
    let audio = audio_streams.iter().any(|a| crate::plex::is_dp_audio(&a.codec));
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
static PLAY_SLOT: Mutex<Option<(u32, Plan, String)>> = Mutex::new(None);

/// True while a resolve is in flight — the HUD renders `PlaybackState::Resolving` from this.
pub(crate) fn play_pending() -> bool { PLAY_BUSY.load(Ordering::SeqCst) }

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

/// MAIN THREAD, NON-BLOCKING. Publishes the HUD strings immediately, supersedes any in-flight
/// resolve, and spawns a worker. The caller flips the route this same frame.
///
/// `sid` is the server the ITEM came from, which the caller knows and this function must not guess:
/// with more than one source on Home, the item being started and the server currently being browsed
/// routinely differ, and every id in the playback protocol below (`rk`, the Part, the streams, the
/// PlayQueue, the resume point) belongs to the former.
pub(crate) fn request_play(sid: ServerId, rk: &str, part: &str, vcodec: &str, acodec: &str, title: &str, ctx: &str) {
    if part.is_empty() && rk.is_empty() {
        return;
    }
    // The fields a play REQUEST owns, as against the ones only a landing may install: the HUD
    // strings (published now, so the pre-roll has a title through the whole resolve) and the five
    // the OUTGOING item leaves behind. Everything else — url, session ids, codecs — stays as it is
    // until `apply_plan` replaces it, which is what lets a still-running playback keep answering
    // for itself while the next one resolves.
    session_mut(|s| {
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
    });
    // …and the outgoing item's track/marker/chapter store, for exactly the reason above: it stays
    // the PREVIOUS leaf's until this resolve lands. See `metadata::retire_playing_item`.
    crate::metadata::retire_playing_item();
    crate::player::reset_audio_track();
    crate::player::reset_subtitle();
    // captured HERE, on the main thread, and moved into the worker — see ResolveEnv
    let env = ResolveEnv::snapshot(sid, rk);
    let gen = PLAY_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    PLAY_BUSY.store(true, Ordering::SeqCst);
    let (rk, part, vc, ac) = (rk.to_string(), part.to_string(), vcodec.to_string(), acodec.to_string());
    let spawned = crate::task::spawn_small("resolve", move || {
        // catch_unwind OUTSIDE the mailbox write, like load_season: a panicking resolve must still
        // land (as !ok) or PLAY_BUSY latches and the screen wedges on a spinner forever.
        let plan = std::panic::catch_unwind(|| build_stream(&rk, &part, &vc, &ac, &env))
            .unwrap_or_default();
        let mut slot = PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner());
        // MONOTONE: an older resolve landing late must never clobber a newer unconsumed one.
        if slot.as_ref().map(|(g, _, _)| *g < gen).unwrap_or(true) {
            *slot = Some((gen, plan, rk));
        }
    });
    if !spawned {
        // there is no worker, so nothing will ever land: releasing this is what keeps the screen
        // from wedging on a spinner that can never resolve
        PLAY_BUSY.store(false, Ordering::SeqCst);
    }
}

/// ASYNC twins of `play_movie` / `play_episode`: identical HUD strings and inputs, but the
/// network work runs on a worker and the caller flips the route THIS frame. `app.rs` drains
/// `pump_play` once a frame and starts the engine when the plan lands.
pub(crate) fn request_play_movie(m: &PmsMovie) {
    if m.part.is_empty() {
        return;
    }
    let rating = if m.rating.is_empty() { "NR" } else { &m.rating };
    let ctx = format!("{} \u{b7} {} \u{b7} {}", m.year, rating, crate::ui::fmt::dur_short(m.dur_ns / 1_000_000));
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
    request_play(item_sid(m.sid), &m.rk, &m.part, &m.vcodec, &m.acodec, &m.title, &ctx);
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
pub(crate) fn request_play_up_next(u: UpNext) {
    let ctx = crate::ui::fmt::episode_kicker(u.season, u.index, &u.ep_title);
    let title = if u.show_title.is_empty() { &u.ep_title } else { &u.show_title };
    // The successor comes out of the PlayQueue of the item now playing, so its server is that
    // item's — [`cur_sid`], not whatever surface is behind the player. Falls back to the browsing
    // surface only if nothing is playing, which the Up Next control cannot actually reach.
    let sid = if cur_sid().is_set() { cur_sid() } else { surface_sid() };
    request_play(sid, &u.rk, &u.part, &u.vcodec, &u.acodec, title, &ctx);
}

/// Supersede an in-flight resolve (BACK during a load). The landing is dropped by generation.
pub(crate) fn cancel_play() {
    PLAY_GEN.fetch_add(1, Ordering::SeqCst);
    PLAY_BUSY.store(false, Ordering::SeqCst);
    *PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    // …and the refusal, because this is the statement that the withdrawn request is over. It is
    // the ONLY place that can retire one: a refused plan builds no engine, so `scrobble_stop` —
    // where the rest of the session state is cleared — is never reached (teardown returns at
    // `engine_take`). Both callers are exactly "playback is being abandoned": `exit_player` and
    // the app-switch to background.
    clear_play_verdict();
}

/// MAIN THREAD, once a frame. Returns true when a fresh plan was installed and playback should
/// start. A stale landing (superseded) is dropped.
pub(crate) fn pump_play() -> bool {
    let taken = PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some((gen, plan, rk)) = taken else { return false };
    if gen != PLAY_GEN.load(Ordering::SeqCst) {
        return false; // superseded while in flight
    }
    PLAY_BUSY.store(false, Ordering::SeqCst);
    let ok = !plan.url.is_empty();
    apply_plan(plan, &rk);
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
    if let Some(u) = up_next() {
        crate::ui::widgets::warm_tex_on(item_sid(cur_sid()), &u.thumb, 480, 270, 0);
    }
    ok
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
fn apply_plan(plan: Plan, rk: &str) {
    let active_encoder = plan.tsession.clone();
    crate::metadata::install_playing(plan.playing);
    // main thread only — `up_next()`/`with_queue()` lend out of this (see their docs). The rows
    // arrive already projected: the worker never retained a `Metadata` tree to install here.
    session_mut(|s| {
        // The HUD strings belong to the REQUEST, not to the landing: `request_play` published them
        // synchronously at the press, and a plan resolving is not new information about the title.
        let (title, ctxline) = (s.title, s.ctxline);
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
            url: plan.url,
            tsession: plan.tsession,
            // Installed on EVERY landing, not only a refusing one: a plan that resolved is itself
            // the statement that the last refusal is over, and assigning unconditionally is what
            // makes that true without a second clear anyone can forget.
            play_verdict: plan.verdict,
            cur_remux: plan.remux,
            cur_delivery: plan.delivery,
            cur_no_video_copy: plan.no_video_copy,
            cur_ceiling: plan.ceiling,
            cur_src: plan.src_measure,
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
    install_active_encoder(&active_encoder);
    // SHARED.desired_audio_idx is read by the DEMUX THREAD on every reopen — main thread only.
    if let Some(ord) = plan.feed_audio_ordinal {
        crate::player::set_audio_track(ord);
    }
    // `request_play` turned subtitles off for the new item; turn the server's selection back on
    // AFTER that reset (this lands a frame or more later, on the main thread, before the engine
    // starts — so the demuxer's per-block `desired_sub_idx` gate sees it from the first cue).
    if let Some(ord) = plan.sub_render_ordinal {
        crate::player::log(&format!("server-selected subtitle: sid={} render_idx={ord}", plan.sub_sid));
        crate::player::request_subtitle(ord);
    }
    // A landing is a DISCRETE change to what is on screen, so it owes the present gate a poke —
    // `ui::idle::invalidate`'s call-site list is that module's correctness argument. The caller
    // (`app.rs`'s pump) invalidates only when `pump_play` returns TRUE, and a REFUSING plan returns
    // false by construction (empty url) while flipping the player from Resolving to Error. That it
    // still repainted was an accident of the player route bypassing the gate entirely; here it is
    // the rule instead.
    crate::ui::idle::invalidate();
}

/// Re-transcode the current item (the session's `cur_rk`) at `offset_secs`, carrying the CURRENT
/// audio + subtitle selection (transcode_base). Used by an audio switch AND by a subtitle
/// (de)select while transcoding. Works from a direct-play OR transcode state — the result
/// is always a transcode (server always emits AC3, so the pipeline's Loaded codec is
/// unchanged). Sets `url` + `tsession`, runs /decision, and returns the new start.mkv URL
/// (the demux re-opens it from byte 0), or None.
pub(crate) fn retranscode(offset_secs: i64) -> Option<String> {
    let c = cur_client()?;
    let rk = cur_rk();
    if rk.is_empty() {
        return None;
    }
    // NB: `tsession` becomes this synthetic marker while the transcoder QUERY keeps riding the
    // per-playback sess() — matching the shipped behavior (is_transcoding()/stop key off
    // `tsession`; the server session correlation stays on sess()). The two deliberately DISAGREE
    // from here on, which is why collecting the fields into one struct did not merge them.
    let session = format!("plxnative-{rk}");
    session_mut(|s| {
        s.cur_remux = false;
        s.tsession = session;
    });
    // the transcode output is the profile target's head (`Caps::encode_vcodec` — the ONE
    // definition, shared with build_stream's guess and profile_for) + AC3 — record it so a
    // pipeline RELOAD (audio switch) builds a Load payload matching the re-encoded stream. A
    // guess: apply_decision_codecs below replaces it with the server's actual output, but it
    // must still track devcaps, not the dev TV (issue #22's bug class).
    set_stream_codecs(crate::devcaps::caps().encode_vcodec(), "ac3");
    put_selection(cur_sid(), cur_part_id(), cur_audio_sid(), cur_sub_sid()); // drives the encode + burn
    let qsess = sess();
    let expected_encoder = ACTIVE_ENCODER.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let sp =
        transcode_spec(
            &rk,
            &qsess,
            &qsess,
            false,
            is_no_video_copy(),
            offset_secs.max(0),
            cur_audio_sid(),
            cur_sub_sid(),
            cur_ceiling(),
            cur_delivery(),
        );
    if let Some(mc) = c.transcode_decision(&sp) {
        apply_decision_codecs(&mc); // reload builds a fresh Load payload — match the real output
    }
    let url = c.transcode_start_url(&sp).to_url();
    if !replace_active_encoder(&expected_encoder, &qsess) {
        // A concurrent ABR commit or teardown won while the decision request was in flight. Do not
        // reload onto a session which no longer belongs to this playback generation.
        let _ = c.transcode_stop(&qsess);
        return None;
    }
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
    set_url(&url);
    // NEVER log the URL. `transcode_start_url` ends in `X-Plex-Token=…`, and this line is reached
    // by an ordinary audio-track switch — so the app's own support channel ("send us
    // /tmp/plxnative-events.log") was asking users to paste a live PMS credential into a public
    // issue thread. The rk, the track ids and the offset are the whole diagnostic value here; the
    // URL added nothing that is not derivable from them.
    crate::player::log(&format!(
        "retranscode rk={rk} audio={} sub={} offset={offset_secs} -> transcode start",
        cur_audio_sid(),
        cur_sub_sid()
    ));
    Some(url)
}

/// Switch the audio track: set the current source audio (&audioStreamID) and re-transcode
/// at the current position (which also (re)burns the current subtitle, if one is selected).
pub(crate) fn switch_audio(stream_id: i64, offset_secs: i64) -> Option<String> {
    session_mut(|s| s.cur_audio_sid = stream_id);
    // retranscode -> put_selection PUTs the audio (+ subtitle) selection server-side; the
    // transcoder encodes the part's SELECTED audio, and only a PUT changes it (a query-param
    // or GET is a no-op).
    retranscode(offset_secs)
}

// ---- selection commits: playback POLICY for the in-player track menu. The menu only reports
// what row was picked; whether that means a native stream switch, a server re-transcode, or a
// burn refresh is decided HERE, next to the codec sets and the transcode state it depends on. ----

/// Commit an audio-track pick: NATIVE switch (feed the chosen stream from the same direct-play
/// file — no transcode, keeps 4K HEVC) when the item direct-plays AND the target codec is
/// direct-playable; else a server re-transcode with that stream selected. `idx` is the
/// CONTAINER audio ordinal (the menu converts its row via metadata::audio_ordinal).
pub(crate) fn commit_audio_selection(idx: i32, codec: &str, stream_id: i64) {
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
        crate::player::request_audio_switch(stream_id);
    }
}

/// Commit a subtitle pick (`sub_idx` -1 = Off): gate the client-side renderer (direct-play path)
/// and select the burn stream for any transcode of the item — refreshing a live transcode so the
/// server re-burns (or drops) it.
pub(crate) fn commit_subtitle_selection(sub_idx: i32, stream_id: i64) {
    crate::player::request_subtitle(sub_idx);
    set_subtitle(stream_id);
    if is_transcoding() {
        crate::player::request_transcode_refresh(); // retranscode PUTs the selection itself
    } else {
        // persist the pick server-side (and subs Off PUTs subtitleStreamID=0, clearing a
        // stale server-side selection that would otherwise burn on the next transcode)
        put_selection(cur_sid(), cur_part_id(), cur_audio_sid(), cur_sub_sid());
    }
}

/// POST one /:/timeline progress report for `rk` to `sid`'s server, carrying this playback's
/// session + PlayQueue + selected-stream state — so /status/sessions shows the right track and the
/// Direct Play vs Transcode badge. The ONE timeline call site (the ~10s reporter thread and the
/// final state=stopped report both come through here).
///
/// **`sid` is an argument, and that is the whole point.** This runs on the reporter WORKER, once
/// every ten seconds for the life of a playback, and it used to resolve its server by calling
/// `client_opt()` — i.e. it asked, on each tick, "which server is the user browsing right now?".
/// The rk is a key on the server the item came from; posted to any other one it either 404s or
/// silently moves a stranger's resume point. Nothing about that was visible from the app: the host
/// suite has no runtime, and the device harness grades progress from the app's own heartbeat, not
/// from the server. (The POST is no longer fire-and-forget — it reports a lost report below — but
/// a report that LANDS on the wrong server is a success as far as that outcome can tell, so the
/// capture below is still the only thing making the right one land.) So the reporter captures the id at its spawn site
/// (`engine.rs`, beside the `rk` it already captured) and hands it back here.
pub(crate) fn report_timeline(sid: ServerId, rk: &str, state: crate::plex::TimelineState, t_ms: i64, d_ms: i64) {
    let c = match crate::plex::client_for(sid) {
        Some(c) => c,
        None => return,
    };
    let active = ACTIVE_ENCODER.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let session = if active.is_empty() { sess() } else { active };
    let (pq, pqi) = (pq_id(), pq_item_id());
    let ok = c.timeline(&crate::plex::TimelineReport {
        rating_key: rk,
        state,
        time_ms: t_ms,
        duration_ms: d_ms,
        session: &session,
        play_queue_id: &pq,
        play_queue_item_id: &pqi,
        audio_stream_id: cur_audio_sid(),
        subtitle_stream_id: cur_sub_sid(),
    });
    // FAILURES ONLY. The reporter thread logs `timeline <state> t=…s/…s` for every tick whichever
    // way the POST went (`player::threads`), so a report the server never took looks exactly like
    // one it did — for the whole length of a film, ten seconds at a time. The success half is
    // already on that line and this runs at 0.1 Hz, so only the silence needs a line of its own.
    if !ok {
        crate::log(&format!("timeline post failed rk={rk} state={} t={}s", state.as_str(), t_ms / 1000));
    }
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    /// `part_id_of` gates the server-side stream selection: `put_selection` returns early on
    /// `<= 0`, so a parse miss silently disables subtitle suppression and audio selection for
    /// the whole item — no error, no log line, just a burned-in subtitle nobody asked for.

    // ---- the QUALITY ceiling as a ROUTING policy --------------------------------------------
    //
    // These grade `flavors_allowed(link_policy(link), quality_policy(q, kbps, w, h))`, which is
    // the expression `build_stream` itself evaluates — not a re-derivation of it. `build_stream`
    // is unreachable from the host (it needs a `Client` and a PMS), and the composition is the
    // half that can silently go wrong, so it is the half that is factored out and pinned.

    /// A library file's shape, for readability at the call sites below: (kbps, w, h).
    const UHD_REMUX: (i64, i64, i64) = (60000, 3840, 2160); // a 60 Mbps 4K rip
    const HD_BIG: (i64, i64, i64) = (30000, 1920, 1080); // the case the whole feature is about
    const HD_SMALL: (i64, i64, i64) = (3000, 1280, 720); // a 3 Mbit/s 720p episode
    const UNMEASURED: (i64, i64, i64) = (0, 0, 0); // PMS said nothing (a play straight off a shelf)

    /// What `build_stream` computes, spelled once.
    fn allowed(link: Option<crate::plex::probe::Location>, q: Quality, src: (i64, i64, i64)) -> crate::plex::LinkPolicy {
        flavors_allowed(crate::plex::link_policy(link), quality_policy(q, src.0, src.1, src.2))
    }

    /// **GATE 1 — Original changes nothing, for any source, on any link.** It is the migration and
    /// readiness fallback: a ceiling that leaked into it would change every existing install.
    /// Note the unmeasured row in particular — `Ceiling::admits` fails CLOSED, and that rule must
    /// not be reachable at all without a fixed rung selected.
    #[test]
    fn original_leaves_routing_exactly_where_it_was_and_auto_selects_the_encoder() {
        for src in [UHD_REMUX, HD_BIG, HD_SMALL, UNMEASURED] {
            assert_eq!(
                quality_policy(Quality::Original, src.0, src.1, src.2),
                crate::plex::LinkPolicy::UNRESTRICTED,
                "Original must restrict nothing, and {src:?} is not an exception"
            );
            assert_eq!(
                quality_policy(Quality::Auto, src.0, src.1, src.2),
                crate::plex::LinkPolicy { direct_play: false, remux: false },
                "Auto must route every source through its fixed-session HLS owner"
            );
            // …and composed, on every link tier, Original is exactly what the link alone said.
            for link in [None, Some(crate::plex::probe::Location::Local),
                         Some(crate::plex::probe::Location::Remote),
                         Some(crate::plex::probe::Location::Relay)] {
                assert_eq!(allowed(link, Quality::Original, src), crate::plex::link_policy(link),
                    "Original changed the answer for link {link:?} on {src:?}");
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
        assert_eq!(quality_ladder_for(true)[..2], [Quality::Auto, Quality::Original]);
        assert!(auto_quality_ready(), "the integrated HLS prime/swap path owns production Auto");
        assert_eq!(supported_quality(Quality::Auto), Quality::Auto);
    }

    /// **GATE 2 — under-ceiling content keeps the fast paths.** Picking "1080p · 8 Mbps" must not
    /// send a 3 Mbit/s 720p episode to an encoder: there is nothing there for a transcode to fix,
    /// and doing it anyway would cost the server a job and the picture a generation. This is the
    /// assertion that stops the feature from degenerating into "a rung means always transcode".
    #[test]
    fn a_source_measured_under_the_ceiling_stays_direct_play_eligible() {
        let p = allowed(None, Quality::P1080, HD_SMALL);
        assert!(p.direct_play, "3 Mbps 720p is under 8 Mbps 1080p — nothing to fix");
        assert!(p.remux, "…and a container remux of it is under the ceiling too");
        // true right down the ladder, until the rung actually bites
        assert!(allowed(None, Quality::P720, HD_SMALL).direct_play, "3 Mbps 720p fits 4 Mbps 720p");
        assert!(!allowed(None, Quality::P720Low, HD_SMALL).direct_play, "…but not 2 Mbps");
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
        assert!(!allowed(None, Quality::P1080, HD_BIG).direct_play, "30 Mbps is over the 8 Mbps rung");
        assert!(!allowed(None, Quality::P1080, (4000, 3840, 2160)).direct_play, "4K is over a 1080p rung");
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
        assert!(!p.remux, "a remux is the same bytes at the same rate, one layer down");
        assert_eq!(p, crate::plex::LinkPolicy { direct_play: false, remux: false });
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
                    crate::plex::LinkPolicy { direct_play: false, remux: false },
                    "a relay was loosened by rung {q:?} on {src:?}"
                );
                // and on an unrestricted link the answer is the user's policy, unchanged
                for link in [None, Some(crate::plex::probe::Location::Local), Some(crate::plex::probe::Location::Remote)] {
                    assert_eq!(allowed(link, q, src), quality_policy(q, src.0, src.1, src.2),
                        "link {link:?} altered rung {q:?} on {src:?}");
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
            on_deck: Some(crate::metadata::Episode { rk: "205".into(), ..Default::default() }),
            ..Default::default()
        };
        assert!(detail_describes(&show, a, "100"), "its own key");
        assert!(detail_describes(&show, a, "205"), "the episode Play would actually start");
        assert!(!detail_describes(&show, a, "206"), "a different episode is not this one");
        // …and neither key may match across servers, or the ceiling judges the wrong file
        assert!(!detail_describes(&show, b, "100"));
        assert!(!detail_describes(&show, b, "205"));
        // a movie has no on-deck episode and must still answer for itself
        let movie = crate::metadata::Detail { sid: a, rk: "7".into(), ..Default::default() };
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
            video: Some(crate::metadata::Stream { bitrate: 7900, ..Default::default() }),
            ..Default::default()
        };
        assert_eq!(source_kbps(&with_video), 7900);
        // …which is what keeps it under an 8 Mbps rung its VIDEO does in fact fit
        assert!(quality_policy(Quality::P1080, source_kbps(&with_video), 1920, 1080).direct_play);
        assert!(!quality_policy(Quality::P1080, with_video.bitrate, 1920, 1080).direct_play,
            "the whole-file figure is what made the rung bite early — this is the bug, pinned");

        // no video record (a show with no episode backfill, an audio-only part) → whole-file
        let bare = crate::metadata::Detail { bitrate: 8540, ..Default::default() };
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
        crate::metadata::Stream { index, external, ..trk(id, "srt", lang, false) }
    }

    #[test]
    fn an_empty_track_list_falls_back_to_the_codec_default() {
        assert_eq!(pick_dp_audio(&[], "ac3").map(|(i, c, _)| (i, c)), Some((-1, "ac3".into())));
        assert!(pick_dp_audio(&[], "truehd").is_none(), "a non-direct-playable default must transcode");
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
        assert_eq!(pick_dp_audio(&tracks, "eac3"), Some((1, "eac3".into(), 10976)));
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
        let cases: [(&str, Vec<crate::metadata::Stream>, &str, Option<(i32, String, i64)>); 7] = [
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
                vec![server_selected(trk(1, "eac3", "rus", true)), trk(2, "eac3", "eng", false)],
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
                vec![server_selected(trk(1, "truehd", "eng", false)), trk(2, "ac3", "fra", false)],
                "truehd",
                Some((1, "ac3".into(), 2)),
            ),
            (
                "nothing direct-playable, selected or not → transcode",
                vec![server_selected(trk(1, "truehd", "eng", false)), trk(2, "dts", "rus", true)],
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
        let subs = [server_selected(sub(10, 3, "eng", true)), sub(11, 4, "rus", false)];
        assert_eq!(pick_dp_subtitle(&subs), None);
    }

    #[test]
    fn no_selected_subtitle_means_subtitles_stay_off() {
        assert_eq!(pick_dp_subtitle(&[]), None);
        let subs = [sub(10, 3, "eng", false), sub(11, 4, "rus", false)];
        assert_eq!(pick_dp_subtitle(&subs), None, "the file's own tracks are not an instruction");
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
        Dovi { present: true, profile: 5, bl_compat: 0, el_present: false, ..Dovi::NONE }
    }
    fn p7() -> Dovi {
        Dovi { present: true, profile: 7, bl_compat: 6, el_present: true, ..Dovi::NONE }
    }
    fn p8() -> Dovi {
        Dovi { present: true, profile: 8, bl_compat: 1, el_present: false, ..Dovi::NONE }
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
        assert!(video_direct_plays("hevc", 3840, 1602, no_dv().presentation(SILENT), &caps));
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
        assert!(video_direct_plays("hevc", 3840, 1602, dv, &caps), "a declared P5 is displayable");
        let n = dv.declared().expect("the payload must carry the node the gate was opened for");
        assert_eq!(n.profile_id, 5, "getInt, and the pipeline's -1 sentinel means no profile hint");
        assert_eq!(n.track_type, "single");
        assert_eq!(n.encryption_type, "clear");
        // ...and the size and codec halves of the gate are untouched by any of it
        assert!(!video_direct_plays("av1", 3840, 1602, dv, &caps));
        let small = crate::devcaps::Caps { hevc_max: (1920, 1088), ..caps.clone() };
        assert!(!video_direct_plays("hevc", 3840, 1602, dv, &small));
    }

    /// Profile 7 is dual-layer: the picture is split across a base and an enhancement layer, and
    /// the pipeline feeds ONE elementary stream. Caught by `el_present` alone — the live P7 item
    /// reports `bl_compat = 6`, so a compatibility-id test would wave it straight through.
    #[test]
    fn a_dual_layer_profile_7_source_does_not_direct_play() {
        let caps =
            crate::devcaps::Caps { hevc: true, hevc_max: (4096, 2176), vp9: false, audio: "eac3".into() };
        // and it is refused in BOTH worlds: no payload key can hand the pipeline a layer we do
        // not feed it, so arming the trigger must not open this gate the way it opens P5's
        for signal in [SILENT, DECLARED] {
            let dv = p7().presentation(signal);
            assert!(!video_direct_plays("hevc", 3840, 2160, dv, &caps), "signal={signal}");
            assert_eq!(dv.refusal(), Some("dual-layer"));
            assert_eq!(dv.declared(), None, "a layer we cannot feed must never be declared");
        }
        assert_ne!(p7().bl_compat, 0, "the fixture must keep the trap it was built to hold");
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
            assert!(video_direct_plays("hevc", 3840, 2160, no_dv().presentation(signal), &caps));
            assert!(video_direct_plays("h264", 1920, 1080, no_dv().presentation(signal), &caps));
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
        assert_eq!(p5().presentation(SILENT).declared(), None, "P5 stays behind the trigger");
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
        let bare = Dovi { present: true, profile: 0, bl_compat: 0, el_present: false, ..Dovi::NONE };
        assert!(!bare.base_layer_unusable(), "a compat id of 0 read out of a silent field is not a 0");
        // but an explicit enhancement layer is disqualifying even with no profile reported,
        // because that field says what it says regardless of what sits beside it
        let el_only = Dovi { present: true, profile: 0, bl_compat: 0, el_present: true, ..Dovi::NONE };
        assert!(el_only.base_layer_unusable());
        // and `present: false` overrides everything — no DV means no DV, whatever noise follows
        let contradictory = Dovi { present: false, profile: 5, bl_compat: 0, el_present: true, ..Dovi::NONE };
        assert!(!contradictory.base_layer_unusable());
        // The rule survives the declaration, in both settings: a bare `present` names no profile,
        // `getInt` has nothing to be given, and a node we cannot fill is not a reason to convict a
        // file that plays. It falls through to `NotDv` — plays as it always has, declares nothing.
        for signal in [SILENT, DECLARED] {
            assert_eq!(Dovi::default().presentation(signal), DvPresentation::NotDv);
            assert_eq!(bare.presentation(signal), DvPresentation::NotDv, "signal={signal}");
            assert_eq!(contradictory.presentation(signal), DvPresentation::NotDv);
            assert_eq!(el_only.presentation(signal), DvPresentation::Refuse("dual-layer"));
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
        let bare = Dovi { present: true, profile: 0, bl_compat: 0, el_present: false, ..Dovi::NONE };
        for d in [no_dv(), p5(), p7(), p8(), bare] {
            for signal in [SILENT, DECLARED] {
                let dv = d.presentation(signal);
                let plays = video_direct_plays("hevc", 3840, 1602, dv, &caps);
                assert_eq!(plays, dv.refusal().is_none(), "{d:?} signal={signal}");
                assert!(!(dv.refusal().is_some() && dv.declared().is_some()), "{d:?}");
                // and a refusal always implies the COPY refusal beside it — `build_stream`'s
                // `no_video_copy` reads `base_layer_unusable`, and its log line at the refusal
                // says "(no copy)" in so many words. If a shape could be refused while a copy of
                // it stayed permitted, the item would come back byte-identical from the server.
                if dv.refusal().is_some() {
                    assert!(d.base_layer_unusable(), "a refusal must also withdraw the copy: {d:?}");
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
                    assert!(!(n.track_type == "dual" && n.encryption_type == "all"), "dv-dual-svp");
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
        assert_eq!(p5().presentation(SILENT).refusal(), Some("no cross-compatible base layer"));
        for compat in [1, 2, 4] {
            let d = Dovi { present: true, profile: 8, bl_compat: compat, el_present: false, ..Dovi::NONE };
            assert!(!d.base_layer_unusable(), "P8 with a cross-compatible base layer (id {compat})");
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
        let aac = [crate::metadata::Stream { codec: "aac".into(), ..Default::default() }];
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
        assert!(!video_direct_plays("h264", 3840, 2160, no_dv().presentation(SILENT), &caps));
        assert!(!video_direct_plays("hevc", 3840, 2160, no_dv().presentation(SILENT), &caps));
        // one axis over is over (per-axis bound, not an area heuristic)
        assert!(!video_direct_plays("h264", 4096, 1080, no_dv().presentation(SILENT), &caps));
        // within the bound plays, exactly at it included (1088 IS the table's number)
        assert!(video_direct_plays("h264", 1920, 1088, no_dv().presentation(SILENT), &caps));
    }

    /// Unknown dimensions fail OPEN (0 = PMS never measured the file — not evidence of 4K, and
    /// yesterday's behavior for it), while the codec half keeps gating regardless.
    #[test]
    fn unknown_dimensions_fail_open_and_the_codec_half_still_gates() {
        let caps =
            crate::devcaps::Caps { hevc: false, hevc_max: (1920, 1088), vp9: false, audio: "aac".into() };
        assert!(video_direct_plays("h264", 0, 0, no_dv().presentation(SILENT), &caps));
        assert!(!video_direct_plays("hevc", 1280, 720, no_dv().presentation(SILENT), &caps), "no decoder row, no direct play");
        assert!(!video_direct_plays("av1", 1280, 720, no_dv().presentation(SILENT), &caps), "the pipeline cannot feed it at any size");
    }

    #[test]
    fn part_id_is_read_from_the_parts_segment() {
        assert_eq!(part_id_of("/library/parts/98765/1712345678/file.mkv"), 98765);
        assert_eq!(part_id_of("/library/parts/1/0/file.mp4"), 1);
        // a query string rides along on the real keys
        assert_eq!(part_id_of("/library/parts/42/17/file.mkv?download=0"), 42);
    }

    #[test]
    fn part_id_is_zero_when_there_is_no_parts_segment() {
        assert_eq!(part_id_of(""), 0);
        assert_eq!(part_id_of("/library/metadata/1234"), 0);
        assert_eq!(part_id_of("/library/parts"), 0, "trailing `parts` with no id");
        assert_eq!(part_id_of("/library/parts/notanumber/file.mkv"), 0);
    }

    /// The direct-play gate: MKV and MP4/M4V parts are fed to the demuxer untouched — everything
    /// else takes the remux branch. mp4 moved sides on 2026-08-11 (issue #22): the mkv-only gate
    /// dated from an unseekable AVIO, and on a server that cannot transcode it turned every mp4
    /// into a failure.
    #[test]
    fn mkv_and_mp4_parts_are_direct_playable() {
        assert!(part_is_streamable("/library/parts/1/2/movie.mkv"));
        assert!(part_is_streamable("/library/parts/1/2/movie.mkv?x=1"), "the query must not defeat it");
        assert!(part_is_streamable("/library/parts/1/2/movie.mp4"));
        assert!(part_is_streamable("/library/parts/1/2/movie.m4v"));
        assert!(!part_is_streamable("/library/parts/1/2/movie.mov"), "mov still remuxes");
        assert!(!part_is_streamable(""));
        assert!(!part_is_streamable("/library/parts/1/2/mkv.avi"), "the extension, not a substring");
        assert!(!part_is_streamable("/library/parts/1/2/mp4.avi"), "the extension, not a substring");
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
        fn item(vcodec: &str, part: &str, acodec: &str) -> crate::metadata::Detail {
            crate::metadata::Detail {
                vcodec: vcodec.to_string(),
                part: part.to_string(),
                width: 3840,
                height: 2160,
                audio: vec![crate::metadata::Stream { codec: acodec.to_string(), ..Default::default() }],
                ..Default::default()
            }
        }
        const MKV: &str = "/library/parts/1/2/file.mkv";
        const MOV: &str = "/library/parts/1/2/file.mov";
        // we pull the file ourselves — nothing on the server touches it
        assert_eq!(playback_preview(&item("h264", MKV, "aac")), Some(Preview::DirectPlay));
        // the container is one the buffer-feed demuxer cannot stream → the server REPACKAGES it
        assert_eq!(playback_preview(&item("h264", MOV, "aac")), Some(Preview::Remux));
        // …and so it does for a streamable container whose only audio track has to be converted
        assert_eq!(playback_preview(&item("h264", MKV, "truehd")), Some(Preview::Remux));
        // a codec the pipeline cannot decode at all is the only real re-encode
        assert_eq!(playback_preview(&item("vp9", MKV, "aac")), Some(Preview::Converts));
        // …including when the container and the audio would otherwise have been fine
        assert_eq!(playback_preview(&item("vp9", MOV, "truehd")), Some(Preview::Converts));
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
            serde_json::from_slice::<crate::plex::Envelope>(json).expect("parse").media_container
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
        assert_eq!(refusal(&general_only).as_deref(), Some("Neither direct play nor conversion is available."));

        // refused, and said nothing about why: still a stop, with no line to quote
        let silent = mc(br#"{"MediaContainer":{"generalDecisionCode":2000}}"#);
        assert_eq!(refusal(&silent).as_deref(), Some(""), "the CODE is the decision, not the text");

        // "Direct play not available; Conversion OK." — the ordinary transcode, which must proceed
        let ok = mc(br#"{"MediaContainer":{"generalDecisionCode":1001,"transcodeDecisionCode":1001,
            "transcodeDecisionText":"Direct play not available; Conversion OK."}}"#);
        assert!(refusal(&ok).is_none());

        // no verdict block at all (an older server, or a body we could not parse into one)
        assert!(refusal(&mc(br#"{"MediaContainer":{"size":1}}"#)).is_none(), "absent is not a refusal");
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
        crate::plex::reset_servers_for_test();
        reset_session();
        g
    }

    /// A `ServerId` naming a slot nothing is registered in — so `client_for` answers `None` and
    /// `build_stream` takes its no-client exit without opening a socket.
    fn unregistered_sid() -> ServerId {
        let id = ServerId::from_raw((crate::plex::MAX_SERVERS - 1) as u16);
        assert!(crate::plex::client_for(id).is_none(), "the test needs an EMPTY slot");
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
        assert_eq!(env.sid, sid, "the snapshot carries the id the request was made with");

        let plan = build_stream("rk-7", "/library/parts/5/1/f.mkv", "h264", "ac3", &env);
        assert_eq!(plan.sid, sid, "a plan that could not resolve still names its server");
        assert!(plan.url.is_empty(), "no client for that slot, so nothing resolved");
        assert_eq!(plan.part_id, 5, "…and the rest of the plan is built as usual");

        apply_plan(plan, "rk-7");
        assert_eq!(cur_sid(), sid, "the installed identity is the captured one");
        assert_eq!(cur_rk(), "rk-7", "and its other half");
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

        apply_plan(Plan { sid: a, machine_id: "MACHINE-A".into(), ..Default::default() }, "rk-a");
        assert_eq!(ResolveEnv::snapshot(a, "rk-a").machine_id, "MACHINE-A", "its own server reuses it");
        assert_eq!(
            ResolveEnv::snapshot(b, "rk-b").machine_id, "",
            "another server must re-ask rather than inherit A's fingerprint"
        );

        // …and an empty `machine_id` means "leave the cache alone", not "the cache is now B's"
        apply_plan(Plan { sid: b, ..Default::default() }, "rk-b");
        assert_eq!(ResolveEnv::snapshot(a, "rk-a").machine_id, "MACHINE-A");
        assert_eq!(ResolveEnv::snapshot(b, "rk-b").machine_id, "");
    }

    /// A one-shot loopback PMS: accepts ONE connection, hands its request line back down the
    /// channel, and answers 200 so the client's read terminates. Real sockets, like `stream.rs`'s
    /// own tests — which server a POST actually reached is the only thing the timeline routing can
    /// be graded on without a television.
    fn stub_pms() -> (i32, std::sync::mpsc::Receiver<String>, std::thread::JoinHandle<()>) {
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
        let a = crate::plex::register_for_test("route-test-A", "127.0.0.1", pa, "tok-a", "cid-route-test");
        let b = crate::plex::register_for_test("route-test-B", "127.0.0.1", pb, "tok-b", "cid-route-test");
        assert_ne!(a, b, "two servers, two slots");

        // an item from B starts playing, then the user walks back to their OWN server's Home
        apply_plan(Plan { sid: b, ..Default::default() }, "rk-b");
        assert!(crate::plex::set_current(a));
        assert_eq!(cur_sid(), b, "what is PLAYING does not move when the browsed server does");

        report_timeline(cur_sid(), "rk-b", crate::plex::TimelineState::Playing, 1_000, 2_000);
        let got = rx_b.recv_timeout(Duration::from_secs(5)).expect("B never received the report");
        assert!(got.contains("ratingKey=rk-b"), "B got something else: {got}");
        assert!(
            rx_a.recv_timeout(Duration::from_millis(300)).is_err(),
            "the current server must not receive another server's progress"
        );

        // control: the same call named at A does reach A, so the assertion above is about routing
        report_timeline(a, "rk-a", crate::plex::TimelineState::Stopped, 0, 2_000);
        let got = rx_a.recv_timeout(Duration::from_secs(5)).expect("A never received its own report");
        assert!(got.contains("ratingKey=rk-a"), "A got something else: {got}");

        ha.join().unwrap();
        hb.join().unwrap();
        // Hand the table back empty. Both stubs' ports close as this returns, so anything left
        // registered is a client that answers nothing — and `CURRENT` still points at one of them.
        // The session is idled with it for the same reason, one level up: it is still holding `b`
        // as the playing server, i.e. a `ServerId` into the table being emptied.
        crate::plex::reset_servers_for_test();
        reset_session();
    }
}
