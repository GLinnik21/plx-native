//! The command API: a FIFO-only surface for an agent (an LLM over SSH, a test harness, an
//! operator's script) to drive and read the app.
//!
//! There is no HTTP server. The surface is the devtrigger FIFO's token vocabulary plus
//! two file-backed reads, and it exists exactly where the FIFO does: `remote::Remote::open`
//! gates the whole transport on `devtriggers`, so a stable television
//! (`--no-default-features`) has no FIFO and therefore no API — nothing to reach, nothing
//! bound. The debug and nightly televisions, the host builds and the simulator all have it.
//!
//! The transport (see `remote.rs`): the FIFO is the remote file in the app's runtime directory. The
//! READS land in the instance root as files an agent fetches over SSH:
//! - `state` — a FIFO token: dump the last frame's snapshot to `state.json`
//!   (the last frame's snapshot, plus `up_ms`).
//! - `shot` — a FIFO token: `shot-<N>.png`, a screenshot of the app's own UI plane
//!   (shot.rs). NOT the video overlay: the overlay lives on a hardware plane the read-back
//!   cannot see, so during playback the video area reads back black.
//!
//! The WRITEs are QUEUED, not applied. Every write is a token that enters the frame loop's
//! single input path (`run::ingest` → `run::ingress_token`), the same path as the FIFO's
//! own tokens; its effect lands on the next presented frame (~50 ms). A token proves
//! nothing — re-write `state` (and, for the visual, `shot`) and read the files back until
//! they say what you expected.
//!
//! The `api:` verbs, performed on the frame thread by [`handle`] (all of them, by
//! construction, go through the media layer's own commit functions — the same ones the
//! track menu's picks land in — so nothing here re-implements a playback decision):
//! - `api:play:<rk>`  — the headless play ritual, SELF-CONTAINED: when nothing is staged
//!   yet, the rk is resolved from PMS on a WORKER thread (the fetch may block, the frame
//!   may not — [`stage_play`]), parked in `PENDING_PLAYS` WITH THE PLAY'S GENERATION, and
//!   an internal `api:playstaged:<rk>` token re-posted; the drain performs the ritual —
//!   `route::request_play_movie` (+ the detail request) then `playback::start_playback`,
//!   exactly what the dev live-play scenario does — ONLY while that generation is still
//!   the latest. A stop, or a newer play, that bumped it in between drops the stale ritual:
//!   a fetch that lands late cannot start playback after a Stop, or run its ritual over a
//!   newer request. A worker that fails (no PMS server selected yet, PMS has no such item,
//!   no playable part) logs the reason and posts nothing — re-write the token to retry.
//! - `api:seek:<ms>`  — `player::request_seek(ms * 1_000_000)`.
//! - `api:audio:<sid>` — the `playing()` item's audio `Stream` whose `id == sid`, through
//!   `route::commit_audio_selection` with the panel's `audio_ordinal` mapping.
//! - `api:sub:<sid>`  — the subtitle `Stream` whose `id == sid`, through the track menu's
//!   own commit, `route::commit_subtitle_and_sidecar`: the track's DEMUXER ordinal
//!   (`metadata::sub_render_ordinal` — −1 for an external pick) and its sidecar (key +
//!   codec) when the stream is sidecar-renderable, which `player::sidecar` fetches and
//!   draws. No viewer memo is remembered (an agent's pick).
//! - `api:quality:<rung>` — the `…` menu's own quality commit: `apply_more_action` with
//!   `SetQuality` — the routing question re-asked for the stream on screen, which reloads only
//!   when the answer changed (the live switch while the engine runs); a terminal engine gets
//!   the persist-then-fresh-retry path instead, the failure screen's Retry at the new rung.
//!   The rung is a persisted-quality spelling: `auto`, `original`, `1080p_20_mbps`,
//!   `1080p_8_mbps`, `720p_4_mbps`, `720p_2_mbps`, `480p_720_kbps`.
//! - `api:profile:<index>[:<pin>|-]` — `SelectProfile` on the session machine: the same
//!   commit the "who's watching" roster's rows land in. The outcome (Switching → Ready, or the
//!   flow's error line) is readable in the state file's `auth` section.
//! - `api:pause` / `api:resume` — the transport key's effect performed directly on the frame
//!   thread: `transport::set_transport_paused` (the value the key carries, the commit the
//!   player page's own PAUSE/PLAY arms land in), plus the HUD linger a key press would extend.
//!   They go direct because the bare-key spelling (`pause`/`play`
//!   FIFO tokens) rides SDL key synthesis, which this television build never delivers to the
//!   event loop (measured: the pre-existing devtrigger FIFO is inert the same way).
//! - `api:stop` — the one leave-playback ritual the STOP key performs, by the same direct path:
//!   `playback::exit_player` (supersede the play, close the overlays, stop the buffer feed,
//!   return to the origin page, arm the Continue-Watching refresh).
//!
//! Everything else the FIFO speaks (keys, `ck:`/`pm:`/`pd:`/`pu:`, `txt:`, `wheel:`, …)
//! passes straight through the dispatcher — the API can speak the whole FIFO vocabulary.
//!
//! Finding content: this API does not browse the library. To find a ratingKey: pull the
//! `X-Plex-Token=...` parameter out of the state file's `url` (present while anything has
//! played) and query the Plex Media Server yourself; the items' `ratingKey` field is the
//! `rk` for `api:play:<rk>`.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde_json::json;

use plx_base::eventlog::log;

/// A command on the frame loop's single input path: a free-form DEV token (the FIFO, the
/// lab channel, replay — `ingress_token` performs it unchanged), or a TYPED request from
/// the UDS control socket ([`crate::api_sock`]) carrying the client's request id — whose
/// response is written back over the socket by the server thread, not to a log line.
pub(crate) enum IngestToken {
    Free(String),
    Api(crate::api_sock::Request),
}
/// Commands the frame loop has not yet performed: `run::ingest` calls [`take_pending`]
/// once per frame and feeds every entry to its dispatcher — the single input path both
/// token families share. Fed by the roster auto-default (in [`take_pending`]), the
/// play-staging worker, and the UDS server thread (`crate::api_sock`); the FIFO's own
/// tokens enter the same path via `remote::drain_fifo`.
static PENDING: Mutex<VecDeque<IngestToken>> = Mutex::new(VecDeque::new());
/// Staged plays: the rk a token asked for → the `PmsMovie` fetched from PMS on a worker
/// thread, WITH THE GENERATION OF THE PLAY REQUEST IT BELONGS TO, consumed (removed) by
/// [`handle_play_staged`] on the frame thread. A play whose staging failed is never
/// parked here, so a drained `api:play:<rk>` with no entry is a fresh request to stage
/// again.
static PENDING_PLAYS: Mutex<Option<HashMap<String, (plx_data::pms::PmsMovie, u64)>>> = Mutex::new(None);
/// The play-intent generation: monotonically increasing; every `api:play` request and
/// every `api:stop` bumps it (recording which kind). A staged play performs its ritual
/// only while its generation is still the latest — a stop, or a newer play, in between
/// makes it stale: the PMS fetch runs on a worker, and a late landing must not start a
/// play after a Stop, nor run its ritual over a newer request.
static PLAY_INTENT: Mutex<(u64, bool)> = Mutex::new((0, false)); // (generation, last was a stop)
/// The last frame's snapshot, republished by `run::report`; the `state` FIFO token dumps
/// it to `state.json`. `None` until the first frame (the app is booting).
static SNAPSHOT: Mutex<Option<String>> = Mutex::new(None);
/// The debug-flavour roster default: the "who's watching" screen is answered with the FIRST
/// user (this household's admin) exactly once per app launch. `true` after the pick has been
/// posted — or stays `false` for the whole launch when the flow never reached a roster.
static PROFILE_DEFAULTED: AtomicBool = AtomicBool::new(false);
/// The app's start instant: the state file's `up_ms` field — the liveness the old `/health`
/// used to answer, now riding the snapshot.
static UP_SINCE: OnceLock<Instant> = OnceLock::new();

/// Boot init: the surface is the FIFO itself, so there is nothing to start — this line is
/// the boot-log answer to "which door does this build have".
pub(crate) fn init() {
    log("api: FIFO-only surface (devtrig builds) — tokens `api:*`, `state`, `shot`; no HTTP listener");
}

/// Command tokens the frame loop has not yet performed: `run::ingest` calls this once per
/// frame and feeds every token to `ingress_token` — the single input path.
///
/// The debug-flavour roster default is posted HERE, on the command path — not from
/// [`publish`], the snapshot's construction, which stays a pure read: profile selection
/// is an explicit commit, not a side effect of the state file's publication.
pub(crate) unsafe fn take_pending(app: &crate::app::App) -> Vec<IngestToken> {
    // The debug-flavour roster default (the user's rule): the "who's watching" screen is
    // answered with the FIRST user — this household's admin — exactly once per app launch,
    // so a debug install never sits on the picker. The flow's own phase change (Profiles →
    // Switching) ends the condition; the flag keeps a refused pick from re-posting every
    // frame. An explicit `api:profile:<index>` token still reaches every other user.
    if plx_base::paths::flavour() == Some("debug") {
        let snap = app.bridge.views().auth.0;
        if snap.phase == plx_session::auth::Phase::Profiles
            && !snap.users.is_empty()
            && !PROFILE_DEFAULTED.swap(true, Ordering::Relaxed)
        {
            // The user's own name stays OUT of the event log: the scrub gate bans the `.title`
            // spelling in a log call (household identity is scrubbed at write time anyway).
            log(&format!(
                "api: roster: auto-selecting first user (user 1 of {})",
                snap.users.len()
            ));
            push("api:profile:0:-".to_string());
        }
    }
    let mut guard = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_empty() {
        return Vec::new();
    }
    guard.drain(..).collect()
}

/// Post one DEV token to the frame loop. `tok` is bounded (an `api:` verb spelling,
/// never a body).
fn push(tok: String) {
    let mut guard = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if guard.len() >= 64 {
        log("api: command queue full, dropping");
        return;
    }
    guard.push_back(IngestToken::Free(tok));
}

/// The UDS server thread's half: post one TYPED request for the frame loop's drain.
/// Bounded exactly like [`push`] — a flooded socket cannot stall the frame thread; the
/// overflow answers with the structured `queue_full` error (the caller reports it).
pub(crate) fn push_req(req: crate::api_sock::Request) -> bool {
    let mut guard = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if guard.len() >= 64 {
        return false;
    }
    guard.push_back(IngestToken::Api(req));
    true
}

/// The `state` FIFO token (dispatched by `events::dispatch_remote_token`): write the last
/// frame's snapshot to `state.json` in the instance root, where an agent reads it over SSH.
/// A token before the first frame logs and writes nothing — there is no snapshot yet.
pub(crate) fn state_file() {
    let Some(json) = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        log("api: state: no frame yet — nothing to dump");
        return;
    };
    let path = plx_base::paths::in_runtime_dir("state.json");
    match std::fs::write(&path, json.as_bytes()) {
        Ok(()) => log(&format!("api: state: wrote {} bytes to {}", json.len(), path.display())),
        Err(e) => log(&format!("api: state: could not write {}: {e}", path.display())),
    }
}

/// The `api:play:<rk>` self-contained door: resolve the item from PMS on a WORKER thread
/// (the fetch blocks on the network — the frame thread must never), park the parsed item
/// WITH THE REQUEST'S GENERATION, and re-post the internal `api:playstaged` token so the
/// frame's next drain performs the ritual — if this generation is still the latest then.
/// A worker that lands after a newer play request for the same rk posts nothing (the
/// newer one owns the entry); every other failure is a logged no-post, and the agent
/// re-writes the token to retry. The client is the NON-PANICKING accessor — before
/// sign-in no server is selected yet, and a worker must not take the app down with a
/// panic.
/// Returns whether the worker was accepted (a refused spawn is the only failure the
/// CALLER can see here — the fetch's own failures are logged no-posts by the worker).
fn stage_play(rk: String, gen: u64) -> bool {
    let refused_msg = format!("api: play rk {rk}: the worker was refused — retry the token");
    let worker = plx_base::task::spawn("api-play-stage", move || {
        let Some(client) = plx_plex::plex::client_opt() else {
            log(&format!(
                "api: play rk {rk}: no PMS server selected yet (sign-in pending) — retry the token"
            ));
            return;
        };
        let Some(meta) = client.metadata(&rk) else {
            log(&format!("api: play rk {rk}: PMS has no such item — check the rk"));
            return;
        };
        let sid = plx_media::route::surface_sid();
        let pmm = plx_data::pms::parse_item(&meta, sid);
        if pmm.part.is_empty() {
            log(&format!("api: play rk {rk}: item has no playable part"));
            return;
        }
        {
            let mut guard = PENDING_PLAYS.lock().unwrap_or_else(|e| e.into_inner());
            let map = guard.get_or_insert_with(HashMap::new);
            if map.get(&rk).is_some_and(|(_, staged)| *staged > gen) {
                // A newer request for this rk already owns the entry: this landing is
                // superseded. No overwrite, no re-post — the newer one's token carries it.
                log(&format!("api: play rk {rk}: landing superseded by a newer play — dropped"));
                return;
            }
            map.insert(rk.clone(), (pmm, gen));
        }
        log(&format!("api: play rk {rk}: item staged (gen {gen})"));
        push(format!("api:playstaged:{rk}"));
    });
    if worker.is_none() {
        log(&refused_msg);
        false
    } else {
        true
    }
}

// ---------------------------------------------------------------------------
// Frame-thread half
// ---------------------------------------------------------------------------

/// Perform an `api:` verb. Called from `run::ingress_token` with the prefix already
/// peeled. Returns whether the frame loop may consider the token dispatched (the
/// passthrough path returns what the dispatcher said; the `api:` verbs answer their
/// own acceptance here).
pub(crate) unsafe fn handle(app: &mut crate::app::App, fr: &mut crate::app::run::Frame, spec: &str) -> bool {
    let (kind, arg) = match spec.split_once(':') {
        Some(pair) => pair,
        None => (spec, ""),
    };
    match kind {
        "play" => handle_play(app, fr, arg),
        "playstaged" => handle_play_staged(app, fr, arg),
        "pause" => handle_transport(app, fr, false),
        "resume" => handle_transport(app, fr, true),
        "stop" => handle_stop(app, fr),
        "seek" => match arg.parse::<i64>() {
            Ok(ms) if ms >= 0 => {
                plx_media::player::request_seek(
                    &mut app.player.session,
                    &mut app.adapters.player,
                    ms * 1_000_000,
                );
                true
            }
            _ => false,
        },
        "audio" => handle_track(app, fr, arg, true),
        "sub" => handle_track(app, fr, arg, false),
        "quality" => handle_quality(app, fr, arg),
        "profile" => handle_profile(app, fr, arg),
        _ => false,
    }
}

/// The frame thread's half of a TYPED UDS request: the same handlers the FIFO's tokens
/// reach (the typed surface is a CONVERSATION over the command set, not a second command
/// set), and the response over the socket instead of a log line — the outcome in the
/// action's own vocabulary, or a structured error naming the reason.
pub(crate) unsafe fn execute_request(
    app: &mut crate::app::App,
    fr: &mut crate::app::run::Frame,
    req: crate::api_sock::Request,
) {
    use crate::api_sock::{ApiError, Outcome, Reply, Verb};
    let id = req.id;
    let st = plx_media::player::state(&app.player.session);
    let no_engine = Reply::err(ApiError::NoEngine { state: state_str(st) });
    match req.verb {
        Verb::Play { rk } => {
            let accepted = handle_play(app, fr, &rk);
            crate::api_sock::reply(
                id,
                if accepted {
                    Reply::ok(Outcome::PlayStaged { rk })
                } else {
                    Reply::err(ApiError::WorkerRefused)
                },
            );
        }
        Verb::Seek { ms } => {
            // The seek target is a `i64` of NANOSECONDS: a ms beyond ~292 years is not a
            // seek (and a blind `as i64` would wrap negative — the FIFO's own guard
            // rejects negatives for exactly this reason).
            let ns = (ms as i64).checked_mul(1_000_000);
            if is_seated(st) {
                if let Some(ns) = ns {
                    plx_media::player::request_seek(
                        &mut app.player.session,
                        &mut app.adapters.player,
                        ns,
                    );
                    crate::api_sock::reply(id, Reply::ok(Outcome::SeekCommitted { ms }));
                } else {
                    crate::api_sock::reply(id, Reply::err(ApiError::Internal {
                        reason: "the seek target overflows nanoseconds".to_string(),
                    }));
                }
            } else {
                crate::api_sock::reply(id, no_engine);
            }
        }
        Verb::Audio { sid } => {
            let ok = handle_track(app, fr, &sid.to_string(), true);
            crate::api_sock::reply(
                id,
                if ok {
                    Reply::ok(Outcome::TrackCommitted { track: "audio", sid })
                } else {
                    Reply::err(ApiError::NoTrack { sid })
                },
            );
        }
        Verb::Sub { sid } => {
            let ok = handle_track(app, fr, &sid.to_string(), false);
            crate::api_sock::reply(
                id,
                if ok {
                    Reply::ok(Outcome::TrackCommitted { track: "sub", sid })
                } else {
                    Reply::err(ApiError::NoTrack { sid })
                },
            );
        }
        Verb::Quality { rung } => {
            let ok = handle_quality(app, fr, &rung);
            crate::api_sock::reply(
                id,
                if ok {
                    Reply::ok(Outcome::QualityCommitted { rung })
                } else {
                    Reply::err(ApiError::UnknownRung { rung })
                },
            );
        }
        Verb::Profile { index } => {
            commit_profile(app, index, None);
            crate::api_sock::reply(id, Reply::ok(Outcome::ProfileCommitted { index }));
        }
        Verb::Pause => {
            let ok = handle_transport(app, fr, false);
            crate::api_sock::reply(
                id,
                if ok { Reply::ok(Outcome::Paused) } else { no_engine },
            );
        }
        Verb::Resume => {
            let ok = handle_transport(app, fr, true);
            crate::api_sock::reply(
                id,
                if ok { Reply::ok(Outcome::Resumed) } else { no_engine },
            );
        }
        Verb::Stop => {
            // Idempotent: an idle session has nothing to leave, and "stopped" is still
            // the true answer to "is it playing".
            handle_stop(app, fr);
            crate::api_sock::reply(id, Reply::ok(Outcome::Stopped));
        }
        Verb::State => {
            let snap = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let r = match snap
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            {
                Some(snapshot) => Reply::ok(Outcome::State { snapshot }),
                None => Reply::err(ApiError::NoSnapshot),
            };
            crate::api_sock::reply(id, r);
        }
        Verb::Shot => {
            crate::shot::request();
            crate::api_sock::reply(id, Reply::ok(Outcome::ShotRequested));
        }
    }
}

/// The playback state's snapshot vocabulary: the same strings the state file's `state`
/// field and the structured `no_engine` error answer share.
fn state_str(st: plx_media::player::PlaybackState) -> &'static str {
    match st {
        plx_media::player::PlaybackState::Idle => "idle",
        plx_media::player::PlaybackState::Resolving => "resolving",
        plx_media::player::PlaybackState::Connecting => "connecting",
        plx_media::player::PlaybackState::Buffering => "buffering",
        plx_media::player::PlaybackState::Seeking => "seeking",
        plx_media::player::PlaybackState::Playing => "playing",
        plx_media::player::PlaybackState::Paused => "paused",
        plx_media::player::PlaybackState::Error => "error",
    }
}

/// The seated-engine question the transport verbs (and the typed seek) ask before they
/// touch the engine: `sink().pause` and `request_seek` are Starfish calls, and a session
/// without an engine (idle, resolving, connecting, errored) has nothing for them to
/// reach.
fn is_seated(st: plx_media::player::PlaybackState) -> bool {
    matches!(
        st,
        plx_media::player::PlaybackState::Buffering
            | plx_media::player::PlaybackState::Paused
            | plx_media::player::PlaybackState::Seeking
            | plx_media::player::PlaybackState::Playing
    )
}

/// A FRESH `api:play` request: bump the play-intent generation (a stop, or a later play,
/// bumps again and supersedes this one) and stage from PMS on a worker — the re-posted
/// `api:playstaged` token performs the ritual on a later drain, only if this generation
/// is still the latest then.
unsafe fn handle_play(
    _app: &mut crate::app::App,
    _fr: &mut crate::app::run::Frame,
    rk: &str,
) -> bool {
    let gen = {
        let mut intent = PLAY_INTENT.lock().unwrap_or_else(|e| e.into_inner());
        intent.0 = intent.0.saturating_add(1);
        intent.1 = false;
        intent.0
    };
    log(&format!("api: play rk {rk}: nothing staged yet — resolving from PMS (worker)"));
    stage_play(rk.to_string(), gen)
}

/// The STAGED half of `api:play` — the headless play ritual, the same three calls the dev
/// live-play scenario makes, in the same order: request the item (which owns the resolve),
/// ask for its detail (the HUD's context), then seat the engine. It runs only while the
/// play's generation is still the latest: a stop or a newer play that bumped it in
/// between drops the ritual here — logged — and the play never starts.
unsafe fn handle_play_staged(
    app: &mut crate::app::App,
    _fr: &mut crate::app::run::Frame,
    rk: &str,
) -> bool {
    let Some((pmm, gen)) = PENDING_PLAYS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .and_then(|map| map.remove(rk))
    else {
        log(&format!(
            "api: play rk {rk}: staged item gone (superseded or stopped) — dropped"
        ));
        return false;
    };
    let (cur, is_stop) = *PLAY_INTENT.lock().unwrap_or_else(|e| e.into_inner());
    if cur != gen {
        log(&format!(
            "api: play rk {rk}: stale — {} intervened; the ritual is dropped",
            if is_stop { "a stop" } else { "a newer play" }
        ));
        return false;
    }
    let requested = plx_media::route::request_play_movie(
        &mut app.player.session,
        app.bridge.metadata_mut(),
        crate::app::playback::start_caps(),
        &pmm,
        &crate::app::playback::movie_ctx(&pmm),
        None,
    );
    if requested {
        app.bridge.metadata_mut().run(
            plx_data::stores::metadata::MetadataCmd::RequestDetail {
                sid: pmm.sid,
                rk: pmm.rk.clone(),
            },
        );
        crate::app::playback::start_playback(
            &mut app.player.session,
            &mut app.adapters.player,
            0,
            crate::app::playback::Origin::Here,
            plx_screens::player::input::HUD_HEADLESS_MS,
            None,
            &mut app.pages,
            &mut app.bridge,
        );
    }
    requested
}

/// `api:pause` / `api:resume` — the transport key's effect, performed directly on the frame
/// thread (the bare-key spelling rides SDL synthesis, which this TV build does not deliver —
/// see the module doc). `play` is the key's `Transport` payload: `true` = the PLAY key
/// (resume), `false` = the PAUSE key, so the commit value is `!play`. Seated-engine guard:
/// `sink().pause` is a Starfish call and a session with no engine (idle, resolving,
/// connecting, errored) has nothing for it to reach — those answer a log line and a false,
/// never a guess at an engine. EXPLICIT PAUSED PASSES the guard: a viewer-paused engine is
/// seated, and `resume` is exactly what it is waiting for.
unsafe fn handle_transport(
    app: &mut crate::app::App,
    fr: &mut crate::app::run::Frame,
    play: bool,
) -> bool {
    let verb = if play { "resume" } else { "pause" };
    let st = plx_media::player::state(&app.player.session);
    if !matches!(
        st,
        plx_media::player::PlaybackState::Buffering
            | plx_media::player::PlaybackState::Paused
            | plx_media::player::PlaybackState::Seeking
            | plx_media::player::PlaybackState::Playing
    ) {
        log(&format!("api: {verb}: no seated engine (state {st:?})"));
        return false;
    }
    let ok = plx_media::player::transport::set_transport_paused(
        &mut app.player.session,
        &mut app.adapters.player,
        !play,
    );
    if !ok {
        log(&format!("api: {verb}: transport commit refused"));
    }
    if let Some(player) = crate::app::bridge::player_mut(&mut app.pages) {
        player.hud.extend(fr.now, plx_screens::player::input::HUD_LINGER_MS);
        player.publish();
    }
    ok
}

/// `api:stop` — the one leave-playback ritual the STOP key performs, by the same direct path:
/// `playback::exit_player` (supersede the play, close the overlays, stop the buffer feed,
/// return to the origin page, arm the Continue-Watching refresh). An idle session has nothing
/// to leave. The stop bumps the play-intent generation and clears the staging table FIRST —
/// even an idle stop: an in-flight PMS fetch for a play request may land after this stop,
/// and the stale check in [`handle_play_staged`] drops it then.
unsafe fn handle_stop(app: &mut crate::app::App, _fr: &mut crate::app::run::Frame) -> bool {
    {
        let mut intent = PLAY_INTENT.lock().unwrap_or_else(|e| e.into_inner());
        intent.0 = intent.0.saturating_add(1);
        intent.1 = true;
    }
    PENDING_PLAYS.lock().unwrap_or_else(|e| e.into_inner()).take();
    let st = plx_media::player::state(&app.player.session);
    if matches!(st, plx_media::player::PlaybackState::Idle) {
        log("api: stop: nothing to leave (state Idle)");
        return false;
    }
    crate::app::playback::exit_player(
        &mut app.player,
        &mut app.adapters.player,
        &mut app.repause_at,
        &mut app.refresh_hubs,
        &mut app.pages,
    );
    true
}

/// `api:audio:<sid>` / `api:sub:<sid>` — find the `playing()` item's track row by PMS
/// stream id and commit it through the media layer's own selection functions (the bare
/// `_selection` commits the ladder uses: no viewer memo is remembered, which is right
/// for an agent).
unsafe fn handle_track(
    app: &mut crate::app::App,
    _fr: &mut crate::app::run::Frame,
    arg: &str,
    audio: bool,
) -> bool {
    let Ok(sid) = arg.parse::<i64>() else {
        return false;
    };
    let views = app.bridge.views();
    let Some(item) = views.metadata.playing() else {
        return false;
    };
    if audio {
        let Some(i) = item.audio.iter().position(|s| s.id == sid) else {
            return false;
        };
        let ord = plx_data::metadata::audio_ordinal(&item.audio, i);
        plx_media::route::commit_audio_selection(
            &mut app.player.session,
            plx_media::route::CarriedAudio::from_stream(&item.audio[i], ord),
        )
    } else {
        let Some(i) = item.subs.iter().position(|s| s.id == sid) else {
            return false;
        };
        let s = &item.subs[i];
        // The track menu's own commit, field for field: the DEMUXER ordinal (the client
        // renderer's input — −1 for an external pick, which the embedded renderer cannot
        // draw), the sidecar the stream names (key + codec — the external pick
        // `player::sidecar` fetches and draws), and no viewer memo (an agent's pick
        // remembers nothing, the ladder's raw commit path).
        let ridx = plx_data::metadata::sub_render_ordinal(&item.subs, i);
        let sidecar = s.sidecar_renderable().then(|| (s.key.clone(), s.codec.clone()));
        plx_media::route::commit_subtitle_and_sidecar(
            &mut app.player.session,
            ridx,
            s.id,
            sidecar.as_ref().map(|(k, _)| k.clone()),
            sidecar.map(|(_, c)| c).unwrap_or_default(),
            None,
        );
        true
    }
}

/// `api:quality:<rung>` — the \u2026 menu\u2019s own quality commit, performed through the menu\u2019s own
/// function: `apply_more_action` re-asks the routing question for the stream on screen and
/// reloads only if the answer changed (the live switch while the engine runs); a terminal
/// engine gets the persist-then-fresh-retry path instead — the failure screen\u2019s Retry at the
/// new rung.
unsafe fn handle_quality(
    app: &mut crate::app::App,
    _fr: &mut crate::app::run::Frame,
    arg: &str,
) -> bool {
    let Ok(q) = serde_json::from_value::<plx_media::route::Quality>(serde_json::Value::String(arg.to_string())) else {
        log(&format!("api: quality: unknown rung {arg}"));
        return false;
    };
    crate::app::playback::apply_more_action(
        &mut app.player.session,
        &mut app.adapters.player,
        &mut app.bridge,
        plx_appkit::more_menu::Action::SetQuality(q),
        &mut app.pages.present,
    );
    log(&format!("api: quality: rung set to {arg}"));
    true
}

/// `api:profile:<index>[:<pin>|-]` — commit a roster pick through the session machine\u2019s own
/// command (the same commit the roster\u2019s rows land in). The pick moves the flow to Switching;
/// its outcome is visible in the state file's `auth` section (phase, error).
unsafe fn handle_profile(
    app: &mut crate::app::App,
    _fr: &mut crate::app::run::Frame,
    arg: &str,
) -> bool {
    let Some((idx, pin)) = arg.split_once(':') else {
        return false;
    };
    let Ok(index) = idx.parse::<usize>() else {
        return false;
    };
    let pin = (pin != "-").then(|| pin.to_string());
    commit_profile(app, index, pin);
    true
}

/// The profile commit the FIFO's `api:profile` token and the typed `profile` request both
/// reach: the session machine's own command (the same commit the roster's rows land in).
/// The pick moves the flow to Switching; its outcome is visible in the state file's
/// `auth` section (phase, error).
unsafe fn commit_profile(app: &mut crate::app::App, index: usize, pin: Option<String>) {
    log(&format!(
        "api: profile: select index {index} (pin {})",
        if pin.is_some() { "given" } else { "none" }
    ));
    crate::app::bridge::execute_session_command(
        &mut app.pages,
        plx_session::auth::SessionCmd::SelectProfile { index, pin },
    );
}

/// Republish the snapshot, called from `run::report` once per frame. The string form is
/// what the `state` FIFO token dumps verbatim to `state.json`; building it here (frame
/// thread) means no other thread ever borrows the app.
pub(crate) unsafe fn publish(app: &mut crate::app::App, fr: &mut crate::app::run::Frame) {
    let ps = &app.player.session;
    let st = plx_media::player::state(ps);
    let state_name = state_str(st);
    let views = app.bridge.views();
    // This frame's auth section: the sign-in flow's phase, the offered "who's watching"
    // roster (title, PIN flag, admin flag) and the flow's error line — the state file's
    // `auth` section.
    let auth = {
        let snap = views.auth.0;
        json!({
            "phase": format!("{:?}", snap.phase).to_lowercase(),
            "error": if snap.error.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(snap.error.to_string())
            },
            "profiles": snap.users.iter().enumerate().map(|(i, u)| {
                json!({
                    "index": i,
                    "title": u.title,
                    "pin": u.protected,
                    "admin": u.admin,
                })
            }).collect::<Vec<_>>(),
        })
    };
    let now = views.metadata.now_playing();
    let playing = views.metadata.playing();
    let tracks = |item: Option<&plx_data::metadata::PlayingItem>, audio: bool| -> Vec<serde_json::Value> {
        let Some(item) = item else {
            return Vec::new();
        };
        let list: &Vec<plx_data::metadata::Stream> = if audio { &item.audio } else { &item.subs };
        list.iter().enumerate().map(|(i, s)| {
            let mut o = json!({
                "sid": s.id,
                "ord": plx_data::metadata::audio_ordinal(&list, i),
                "codec": s.codec,
                "lang": s.lang,
                "title": s.title,
                "selected": s.selected,
            });
            if audio {
                o["ch"] = json!(s.channels);
                o["layout"] = json!(s.layout);
                o["kbits"] = json!(s.bitrate);
                o["default"] = json!(s.default);
            } else {
                o["external"] = json!(s.external);
                o["forced"] = json!(s.forced);
            }
            o
        }).collect()
    };
    let verdict = plx_media::route::play_verdict(ps).map(|s| s.to_string());
    let mut obj = json!({
        "t": fr.now,
        "up_ms": UP_SINCE.get().map(|t| t.elapsed().as_millis() as u64).unwrap_or(0),
        "state": state_name,
        "state_id": st as u8,
        "playing": plx_media::player::is_playing(ps),
        // The viewer's pause — `true` while the transport is paused for the viewer (a claim
        // hold's own pause is NOT the viewer's and stays out of this lens).
        "paused": plx_media::player::transport::viewer_paused(ps),
        "pos_ms": plx_media::player::playpos_ns(ps).unwrap_or(0) / 1_000_000,
        "dur_ms": plx_media::player::duration_ns(ps).unwrap_or(0) / 1_000_000,
        "title": now.map(|n| n.title.clone()).unwrap_or_default(),
        "ep_title": now.map(|n| n.ep_title.clone()).unwrap_or_default(),
        "rk": playing.map(|p| p.rk.clone()).unwrap_or_default(),
        "kind": playing.map(|p| p.kind.clone()).unwrap_or_default(),
        "url": plx_media::route::url(ps),
        "transcode": plx_media::route::transcode_session(ps),
        "kbps": plx_media::route::transport_kbps(ps),
        "audio_sid": plx_media::route::cur_audio_sid(ps),
        "verdict": verdict,
        "audio": tracks(playing, true),
        "subs": tracks(playing, false),
    });
    obj["auth"] = auth;
    let json = obj.to_string();
    *SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(json);
    let _ = UP_SINCE.get_or_init(Instant::now);
}
