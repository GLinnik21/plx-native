//! player — the buffer-feed video engine (was src/playback.c). THREADING: everything
//! here except sf_on_event/acb_on_event runs on the SDL main thread. Those two are
//! #[no_mangle] and run on the StarfishMediaAPIs library thread; they touch ONLY
//! `SHARED`. All other cross-thread state is in shared.rs (atomics + Mutex); the
//! Engine (engine.rs) is main-thread-confined. Design: docs/engine-port-design.md.
//!
//! "Runs on the SDL main thread" is a **compile error to violate** for the two things where it
//! matters — the ACB/Starfish seam and the `ENGINE` slot. Both take a [`MainThread`] token,
//! which `plex_run` mints once and passes down; it is `!Send`, so a closure that captured one
//! cannot be handed to `task::spawn`. The exceptions are the honest ones: the two callbacks
//! above are `extern "C"` entry points *from* the library thread and touch only `SHARED`, and
//! `threads::load_thread` calls `sf_load` off-main by design (see `ffi`).
#![allow(non_upper_case_globals)]
pub(crate) mod engine;
mod ffi;
mod pump;
mod shared;
pub(crate) mod threads;

use crate::task::MainThread;
use shared::{Shared, SubBitmap, SubCue, Transport};
/// one rect of an image-subtitle display set — the demuxer builds them, the HUD draws them
pub(crate) use shared::SubRect;
pub(crate) use shared::TrackNames;

/// `/tmp/plxnative-tracknames[=<audio>;<subs>]` — **stand in for the container's own track names**,
/// which nothing off-device can read.
///
/// It exists for the same reason `/tmp/plxnative-personbio` does, and the shape of the problem is
/// identical: the data comes from a source no automated or host run can reach, so without a seed
/// every headless look at the screen shows the degenerate state. Here the source is the DEMUXER —
/// `ff::track_names` publishes these when it opens a part — and the desktop simulator has no
/// demuxer at all (the bundled FFmpeg is ARM, and `player::ffi`'s host arm has no video path), so
/// the picker there can only ever draw what PMS sent. Which, for the MP4 this exists to fix, is a
/// column of identical language names.
///
/// Pipe-separated within a list, `;` between the two lists, audio first:
/// `Дубляж|Original;Forced|Full`. Either side may be empty (`;Forced|Full` seeds subtitles alone).
/// An EMPTY file seeds the real nine-track sample this was built against, because that is the shape
/// that exercises the case: six same-language rows PMS reports identically, which no shorter list
/// demonstrates.
///
/// **It seeds the real store and stubs nothing else** — the same `SHARED.track_names` the demuxer
/// writes, read back through the same `ui::track_menu::track_name` precedence. So a seeded
/// screenshot verifies the ROW, honestly; what it cannot verify is the FFI read that fills the
/// store on a television. Compiled out of a release build with every other trigger (`dev::read` is
/// a compile-time `None`), so a shipped binary cannot be made to show a name that is not the
/// file's.
pub(crate) fn seed_dev_track_names() {
    let Some(spec) = crate::dev::read("tracknames") else { return };
    // The real subtitle names of a nine-track MP4 whose PMS record carries none — the file this
    // whole path was written against. Audio is left empty on purpose: one list is enough to
    // demonstrate, and seeding audio too would hide the `audio_descriptor` fallback the real menu
    // still uses for a track the container does not name.
    const SAMPLE: &str = ";Форс. iTunes|Форс. Jaskier песни|Форс. Red Head Sound песни|Полные iTunes|Полные Jaskier|Полные stirloo|Full|Full SDH|Повнi iTunes";
    let spec = spec.trim().to_string();
    let spec = if spec.is_empty() { SAMPLE } else { spec.as_str() };
    let list = |s: &str| -> Vec<String> {
        if s.is_empty() {
            Vec::new()
        } else {
            s.split('|').map(|p| p.trim().to_string()).collect()
        }
    };
    // `split_once(';')`, so a name may not contain a `;` and everything after the first one is the
    // subtitle list — a seed is a diagnostic, not a format, and the alternative is quoting rules.
    let (a, sub) = spec.split_once(';').unwrap_or(("", spec));
    let (audio, subs) = (list(a), list(sub));
    crate::log(&format!(
        "player: DEV track names seeded (a={} s={}) — /tmp/plxnative-tracknames",
        audio.len(),
        subs.len()
    ));
    *SHARED.track_names.lock().unwrap() = TrackNames { audio, subs };
}
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_long};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering::Relaxed};

pub(crate) static SHARED: Shared = Shared::new();
pub(crate) static TX: Transport = Transport::new();
static ACB_OK: AtomicBool = AtomicBool::new(false); // was the g_acb availability flag

// Diagnostics-only Auto state. These codes cross the demux/UI thread boundary through atomics;
// named constants keep the writer and the photograph formatter from growing separate vocabularies.
pub(crate) const ABR_MODE_ORIGINAL: u8 = 1;
pub(crate) const ABR_MODE_HLS: u8 = 2;
pub(crate) const ABR_ACTION_STEADY: u8 = 1;
pub(crate) const ABR_ACTION_PRIME_DOWN: u8 = 2;
pub(crate) const ABR_ACTION_PRIME_UP: u8 = 3;
pub(crate) const ABR_ACTION_COMMIT_DOWN: u8 = 4;
pub(crate) const ABR_ACTION_COMMIT_UP: u8 = 5;
pub(crate) const ABR_ACTION_REJECT_DOWN: u8 = 6;
pub(crate) const ABR_ACTION_REJECT_UP: u8 = 7;
pub(crate) const ABR_ACTION_PROBE_ORIGINAL: u8 = 8;
pub(crate) const ABR_ACTION_RECOVER_ORIGINAL: u8 = 9;
/// Why the controller last moved (or declined to) — `crate::abr::HlsReason` as a code, so the
/// read-out can name the CONSTRAINT that bound rather than only the action it produced. `0` is
/// "nothing has decided yet", which is a real state at the top of a playback and not a fault.
pub(crate) const ABR_WHY_NONE: u8 = 0;
pub(crate) const ABR_WHY_SAFE_BUDGET: u8 = 1;
pub(crate) const ABR_WHY_UNSAFE_STATE: u8 = 2;
pub(crate) const ABR_WHY_PRODUCTION: u8 = 3;
pub(crate) const ABR_WHY_BUFFER: u8 = 4;
/// The downshift trigger fired and there is no rung below — the ladder floor. Distinct from the
/// three constraint codes above because it names the ABSENCE of an action rather than the
/// constraint that chose one: nothing the controller can do will improve this playback.
pub(crate) const ABR_WHY_LADDER_FLOOR: u8 = 5;
/// The starvation horizon fired: at the measured capacity the reserve empties inside the fallback
/// window. Distinct from [`ABR_WHY_UNSAFE_STATE`] because that one is a rate comparison with no
/// reserve in it — it is true of a rung that is 1% too dear against a full buffer — while this one
/// is a DEADLINE and is the code a reader sees on the way to a stall.
pub(crate) const ABR_WHY_STARVATION: u8 = 6;
// Kodi in-place seek (flush + reopen + re-anchor the decode position + sendSegmentEvent, NO
// reload/decoder re-init → no HDR-mode popup, no A/V-resync glitch). On webOS<11 (this 4.5)
// setTimeToDecode returns 0, so feed_stream falls back to the content-info path
// (loadSpi_getInfo + setContentInfo(ptsToDecode) — the same path the official app uses).
// Cleared to false if the pipeline can't be reached (sf_send_segment == 0), which drops seeks
// back to the robust reload-per-seek path.
//
// SCOPE: this is a **per-session probe, not a device-capability latch**, and
// `engine::start_bufferfeed` re-arms it to true for every new session. What `sf_send_segment`
// reports is whether `sf_pipeline()` could reach the CustomPipeline behind the CURRENT
// StarfishMediaAPIs object: `SMP_READY()` (constructed by `sf_load`, cleared by `sf_destroy`)
// plus two non-null shared_ptr hops, `g_smp+0x4c` -> `player+0x04` (src/starfish.c). Every one
// of those is a property of the live object this session builds and teardown destructs; none of
// them says anything about what this panel's pipeline can do. `sendSegmentEvent` itself returns
// void, so a 0 here NEVER means "the segment event was rejected", only "there was nothing to
// call it on" — a liveness/timing condition by construction.
// Latching it for the process was therefore a bug with a very long tail: one teardown-window
// race downgraded every later seek of every later item to a ~1 s reload until the app was
// restarted. Re-arming per session is self-healing rather than oscillating, too: the fallback
// the clear selects (`reload_at`) builds a fresh Starfish object, so the exact condition that
// produced the 0 cannot survive into the session that re-arms — and if a fresh session really
// can't reach its pipeline either, it re-clears after one seek and stays on the reload path.
// It remains a static rather than an `Engine` field only because `pump.rs` reads it without an
// Engine borrow at hand; its LIFETIME is the Engine's, since `start_bufferfeed` is the sole
// constructor and the flag is only ever read while a session is live.
pub(crate) static INPLACE_SEEK_OK: AtomicBool = AtomicBool::new(true);
static PTYPE: AtomicI32 = AtomicI32::new(10); // g_ptype (PLAYER_TYPE_MSE)

// ---- API app.rs calls (were extern "C" fns in playback.h) ----
pub(crate) use engine::{acb_init, resume_at, start_bufferfeed, stop_bufferfeed, suspend_bufferfeed};
pub(crate) use pump::pump;
pub(crate) use shared::PlaybackState;
pub(crate) fn pause(mt: &MainThread) {
    unsafe { ffi::sf_pause(mt); }
    acb_mirror_playstate(mt, false);
} // playback_pause
pub(crate) fn resume(mt: &MainThread) {
    unsafe { ffi::sf_play(mt); }
    acb_mirror_playstate(mt, true);
} // playback_resume

/// Kodi parity: mirror the ACB PLAYSTATE on transport pause/resume (the pipeline Pause/Play alone
/// leaves the app-owned sink's ACB state stale). Only once the plane is bound — firing
/// setState(PAUSED/PLAYING) before setMediaId/LOADED would corrupt the bind ordering.
fn acb_mirror_playstate(mt: &MainThread, playing: bool) {
    if !ACB_OK.load(Relaxed) {
        return;
    }
    if !engine::engine(mt).is_some_and(|e| e.stage >= shared::Stage::Bound) {
        return;
    }
    unsafe {
        if playing {
            ffi::acb_resume(mt);
        } else {
            ffi::acb_pause(mt);
        }
    }
}

// ---- transport accessors app.rs / player_hud.rs call ----
pub(crate) fn is_started() -> bool { TX.started.load(Relaxed) }
pub(crate) fn playpos_ns() -> i64 { SHARED.playpos_ns.load(Relaxed) }
pub(crate) fn frames() -> i32 { SHARED.frames.load(Relaxed) }
/// True once this SESSION has presented at least one frame. Deliberately NOT `frames() > 0`: the
/// pump zeroes `frames` as part of applying a seek (`pump.rs`), so that expression reads "no
/// picture" for the whole of every seek. Cleared only by `reset_session` — i.e. by a real stop or
/// a reload, both of which do blank the video plane. See [`shared::Shared::seen_frame`].
pub(crate) fn seen_frame() -> bool { SHARED.seen_frame.load(Relaxed) }
pub(crate) fn duration_ns() -> i64 { SHARED.duration_ns.load(Relaxed) }
pub(crate) fn seek_pending() -> i64 { TX.seek_to_ns.load(Relaxed) }
/// true once the pipeline has drained to true end-of-stream (see pump's EOS check). app.rs polls
/// this to tear the player down at the credits.
pub(crate) fn ended() -> bool { SHARED.ended.load(Relaxed) }
pub(crate) fn request_seek(ns: i64) {
    SHARED.ended.store(false, Relaxed); // seeking back from the end un-ends the stream
    SHARED.seeking.store(true, Relaxed); // HUD: spinner + freeze the playhead until it lands
    SHARED.seek_display_ns.store(ns, Relaxed);
    TX.seek_to_ns.store(ns, Relaxed);
    // Count the request even though the target it carries may be overwritten before the pump
    // ever sees it — that overwrite IS the coalescing, and this is the only place it's countable.
    TX.seek_reqs.fetch_add(1, Relaxed);
}
/// **The seek was ABANDONED — put the playhead back on reality.**
///
/// [`request_seek`] sets `SHARED.seeking`, and until 2026-08-27 exactly ONE place ever cleared it:
/// the successful prime→Play in `engine::feed_stream`. Every path that gives UP on a seek
/// therefore leaked the flag — and that flag is what `pump::set_state` reads to publish
/// `PlaybackState::Seeking`, which means a spinner over the picture, the playhead frozen at
/// `seek_display_ns`, and `is_playing()` false, **for the rest of the playback**, while the
/// pipeline goes on fetching and presenting underneath.
///
/// Device-measured 2026-08-27 (`docs/measurements/j3e-logs/pipe_abr_seek_flat.log`): a transcode
/// seek whose rebuild returned `None` froze the read-out at `pos=5s` while 37 further segments
/// were acquired, four rung commits landed, and the loop held 60 fps for another 84 seconds. The
/// stream was fine; only the app's account of it was stuck.
///
/// A flag set by the requester and cleared only on the success path is a wedge waiting to happen.
/// This exists so every give-up path can say so in one word, rather than each remembering a store
/// — which is the arrangement that failed. `seek_display_ns` goes back to `-1` with it, since the
/// HUD reads that as "no seek target" and a stale one would keep the frozen playhead after the
/// spinner cleared.
pub(crate) fn abandon_seek() {
    SHARED.seeking.store(false, Relaxed);
    SHARED.seek_display_ns.store(-1, Relaxed);
}
/// true while a seek is resolving (request → reopen/reload → prime → Play): the HUD shows a
/// spinner and freezes the playhead at `seek_display_ns` instead of wobbling through the reopen.
pub(crate) fn loading() -> bool { state().is_busy() }
/// true only while the pipeline is actually presenting frames — not resolving, connecting,
/// buffering or seeking. app.rs gates the heartbeat's `pos=` field on this: on a **direct-play**
/// resume `resume_at` only arms the seek (it does not seed `playpos_ns`, unlike the transcode
/// branch), so the position reads 0 until the first decoded frame lands at the resume offset.
/// Logging that pre-roll 0 would show the harness a 0→600 step and read as 600s of "climb"
/// inside one second — a false PASS on `min_timeline_climb_s`.
pub(crate) fn is_playing() -> bool { matches!(state(), shared::PlaybackState::Playing) }
/// The derived playback state — the ONE thing the HUD renders from. See `PlaybackState`.
pub(crate) fn state() -> shared::PlaybackState {
    // Resolving is DERIVED here rather than stored: the pump owns `pb_state` but only runs once
    // an engine exists, which is false for the whole resolve window. Deriving in the one reader
    // keeps a single writer instead of poking the state in from the frame loop.
    if crate::route::play_pending() {
        return shared::PlaybackState::Resolving;
    }
    // …and so is the PRE-FLIGHT refusal, for exactly the same reason: `/decision` answers before a
    // byte of video moves, so the plan fails with no URL, no engine is ever built, and the pump
    // that owns `pb_state` never runs. Deriving it in the one reader keeps a single writer — the
    // alternative is poking `Error` into the player's state from the frame loop. It sits BELOW the
    // resolve check because a fresh resolve is the thing that retires the last verdict.
    if crate::route::play_refused() {
        return shared::PlaybackState::Error;
    }
    shared::PlaybackState::from_u8(SHARED.pb_state.load(Relaxed))
}

/// The `Error` state's wording, shaped by WHY — issue #22's lesson: `ff: no video stream` was
/// technically true and cost the reviewer a full server-side investigation that the sentence
/// "the server sent audio only" would have ended. Pure so every arm is host-testable; the two
/// wrappers below feed it the globals (main thread only — `route::is_transcoding` reads
/// main-thread state).
///
/// `sub` is `plex::serverinfo`'s Plex Pass tristate, an explicit parameter for the same purity.
/// It sharpens the WORDING of the transcode arm and nothing more — and on a KNOWN-free server it
/// appends the subscription as a support FACT, never as the cause. The distinction is the
/// codebase's own audit: docs/plex-pass-audit.md row 1 says h264 encoding is free everywhere,
/// and the profile's target chain ends in h264 precisely so a free server always HAS a usable
/// video target — so reaching this arm on one means something ELSE failed (a source the server's
/// ffmpeg cannot decode, transcoding disabled server-side), and "it cannot encode video without
/// Plex Pass" was asserting a cause the profile had already ruled out, pointing the user at a
/// purchase that would fix nothing. Known-true or unknown keeps the neutral wording alone: a
/// subscription the app cannot prove absent must never even be named. (And wording is as far as
/// subscription state may ever reach into playback — see `serverinfo::subscription`'s doc for
/// why it is not a routing input.)
///
/// The (no-video, not-transcoding) arm — an audio-only DIRECT-PLAYED file — is worded for the
/// file because that is the truth there: route only direct-plays when PMS metadata names an
/// h264/hevc video track, so reaching it means the file disagrees with its own metadata.
/// One error, three surfaces — the HUD caption, the diagnostics panel's verdict line, and the
/// full-screen read-out (`Player Screen.dc.html`, the spec that superseded the retired
/// `Plex Pass Awareness.dc.html` for this screen) — shaped in ONE place so
/// they can never disagree about what happened. `no_pass` is the read-out's cue to draw the
/// filled PLEX PASS capsule: a support FACT beside the reason, never the cause (see the arm
/// comment above).
pub(crate) struct ErrorShape {
    pub caption: &'static std::ffi::CStr,
    /// the diagnostics panel's verdict suffix ("" = no reason known) — includes the
    /// subscription fact in words, because the panel is plain text
    pub panel: &'static str,
    /// the read-out's reason line — sentence case, subscription fact NOT baked in (the
    /// read-out states it as its own line, with the capsule)
    pub readout: &'static str,
    /// The SERVER's own sentence, quoted VERBATIM under the reason ("" = none). The one field
    /// here whose text is not OURS: it arrives at runtime off `/decision` and is reproduced
    /// unedited — not sentence-cased, not re-worded — since its wording is the server's. Only the
    /// pre-flight arm ever fills it; every other arm's reason is something the app worked out
    /// itself.
    ///
    /// A `Cow`, and borrowed in practice: `route::play_verdict` hands out a `&'static str` off the
    /// main-thread static, and this whole shape is rebuilt 2–3× per frame while a read-out is up
    /// (HUD caption, read-out, diagnostics panel), two of those only to read a `&'static` field.
    ///
    /// **It deliberately does NOT reach `panel`.** The diagnostics panel is a PHOTOGRAPH — its
    /// module doc bans URLs, paths and item titles from it, and a PMS decision sentence is
    /// untrusted free text that can carry a filename. So the viewer's own screen quotes the
    /// server and the shared support surface names only what the app decided; the asymmetry is
    /// the redaction rule, not an oversight.
    pub detail: std::borrow::Cow<'static, str>,
    /// true only when the failure is the audio-only transcode AND the server is known to have
    /// no Plex Pass — the one case the capsule appears
    pub no_pass: bool,
}
fn error_shape(
    no_video: bool,
    transcoding: bool,
    sub: crate::plex::serverinfo::Subscription,
    verdict: Option<&'static str>,
) -> ErrorShape {
    let no_pass = sub == crate::plex::serverinfo::Subscription::No;
    // FIRST, because it is the earliest thing that can fail and the most certain thing we can say:
    // the server adjudicated the request at `/decision` and refused BOTH lanes before any of the
    // signals below could exist (no engine ran, so `no_video` is simply false here). The two lines
    // it produces are different KINDS of sentence — ours states what happened, mapped from the
    // decision CODE; the server's is quoted verbatim beneath it.
    //
    // `no_pass` is FALSE on this arm on purpose, and it stays false on a server we KNOW has no Plex
    // Pass and even when the encoder the server names happens to be HEVC. The server told us the
    // cause; naming a subscription beside it would be exactly the speculation the tristate rule
    // forbids — and it would be wrong twice over, since the arm is reachable for any source the
    // server's own ffmpeg cannot decode, which no subscription changes.
    if let Some(v) = verdict {
        return ErrorShape {
            caption: c"Playback failed — the server cannot play or convert this file",
            // The panel's line is ours and static; the server's sentence rides on `detail`, whose
            // surface (the full-screen read-out) is the one that can hold a whole sentence.
            panel: "the server refused the item at /decision — it can neither direct play nor convert it",
            readout: "The server cannot play or convert this file",
            detail: std::borrow::Cow::Borrowed(v),
            no_pass: false,
        };
    }
    match (no_video, transcoding) {
        (true, true) => ErrorShape {
            caption: c"Playback failed — server sent audio only",
            panel: if no_pass {
                "server sent audio only — it found no usable video transcode target (server has no Plex Pass)"
            } else {
                "server sent audio only — it found no usable video transcode target"
            },
            readout: "The server sent audio only — it found no usable video transcode target",
            detail: std::borrow::Cow::Borrowed(""),
            no_pass,
        },
        (true, false) => ErrorShape {
            caption: c"Playback failed — no video in the file",
            panel: "the stream carries no video track",
            readout: "This file has no video track",
            detail: std::borrow::Cow::Borrowed(""),
            no_pass: false,
        },
        _ => ErrorShape {
            caption: c"Playback failed",
            panel: "",
            readout: "",
            detail: std::borrow::Cow::Borrowed(""),
            no_pass: false,
        },
    }
}
/// The Plex Pass claim that applies to THIS failure: the subscription of the server the PLAYING
/// item came from.
///
/// **Never `serverinfo::subscription()`**, which answers for whichever server is *current* — and
/// `current` stays pinned to the primary while a borrowed film plays (`plex::servers`' own rule:
/// browsing a share does not re-point it). Both polarities are wrong and both are silent: a film
/// borrowed from a Pass-less share loses the "(server has no Plex Pass)" clause and the read-out's
/// capsule, which is exactly the support fact issue #22 was reported without; and our own Pass-less
/// server would put that capsule on a failure that came from a friend's Pass'd machine, asserting a
/// fact about a server that has nothing to do with it. `serverinfo::subscription_of` exists for
/// this, and `route::cur_sid` is the playing item's own server — captured once at `request_play`
/// and installed by `apply_plan`, which runs before either signal that can flip the state to
/// `Error` (the plan's own refusal and the engine it would otherwise have started).
///
/// MAIN THREAD, like every other reader of `route`'s playback state.
fn playing_subscription() -> crate::plex::serverinfo::Subscription {
    crate::plex::serverinfo::subscription_of(crate::route::cur_sid())
}

/// The live [`ErrorShape`] for `PlaybackState::Error` (main thread — `route::is_transcoding` and
/// `route::play_verdict` read main-thread state).
pub(crate) fn error_now() -> ErrorShape {
    if let Some(arm) = failtest_arm() {
        return arm;
    }
    error_shape(
        SHARED.demux_no_video.load(Relaxed),
        crate::route::is_transcoding(),
        playing_subscription(),
        crate::route::play_verdict(),
    )
}

/// A sample PMS refusal, for the `verdict` variant of the dev trigger below. Real wording: this is
/// the sentence a server emits when the only encoder our profile asked for is one it does not have,
/// which is issue #22's failure with the pre-#22 single-entry target chain.
const FAILTEST_VERDICT: &str = "Cannot convert this item. Implementation for video encoder 'hevc' not found.";

/// dev: `/tmp/plxnative-failtest=<arm>` — force one variant of the failure read-out.
///
/// The read-out is the one screen in the app that **cannot be reached on purpose**: it needs a
/// server that refuses, which is exactly the state a working setup does not have. It is also the
/// screen most designed to be looked at — `Player Screen.dc.html` shapes it to survive a phone
/// photograph, because a maintainer triaging a report from someone else's television is its whole
/// audience. So the arms are selectable, and there is no other way to grade them on a panel.
///
/// Arms: `verdict` (the pre-flight refusal, with the server's own sentence quoted), `audio` (the
/// audio-only transcode — pair with `/tmp/plxnative-nopass` for the PLEX PASS capsule), `novideo`
/// (an audio-only file that direct-played), `none` (a failure with no reason known). It feeds
/// [`error_shape`] rather than short-circuiting it, so what is photographed is the real resolver.
///
/// `player_hud::busy` has the other half — the state itself — for the same reason.
///
/// The subscription comes from [`playing_subscription`], the same reader the real path uses, so the
/// arm being photographed is the real resolver on real state. `/tmp/plxnative-nopass` is what makes
/// the capsule reachable and it applies to EVERY server, so the pairing the root `CLAUDE.md`
/// documents is unaffected — but note the arm still has to be looked at from the player route,
/// i.e. after a play, which is when `route::cur_sid` names a server at all.
fn failtest_arm() -> Option<ErrorShape> {
    let arm = crate::dev::read("failtest")?;
    let sub = playing_subscription();
    Some(match arm.trim() {
        "audio" => error_shape(true, true, sub, None),
        "novideo" => error_shape(true, false, sub, None),
        "none" => error_shape(false, false, sub, None),
        _ => error_shape(false, true, sub, Some(FAILTEST_VERDICT)),
    })
}
/// HUD caption for `PlaybackState::Error` (main thread).
pub(crate) fn error_caption() -> &'static std::ffi::CStr {
    error_now().caption
}
/// The same answer for the diagnostics panel's verdict line ("" = no reason known).
pub(crate) fn error_reason() -> &'static str {
    error_now().panel
}
/// Test-only: drive the derived playback state, returning the previous raw value to restore.
///
/// `pb_state` is the pump's field and `shared` is a private module, so a host test that needs the
/// app in a given state — `app.rs`'s HUD-visibility pair, which pins the bug that made the `…` disc
/// unreachable while stalled — sets it through here rather than widening the module for a test.
/// Callers must hold `crate::testlock::serial()`: this is a crate global.
#[cfg(test)]
pub(crate) fn swap_state_for_test(s: shared::PlaybackState) -> u8 {
    let prev = SHARED.pb_state.load(Relaxed);
    SHARED.pb_state.store(s as u8, Relaxed);
    prev
}
#[cfg(test)]
pub(crate) fn restore_state_for_test(raw: u8) {
    SHARED.pb_state.store(raw, Relaxed);
}

// ---- diagnostics --------------------------------------------------------------------------

pub(crate) use engine::aq_caps;
/// The two feed-ahead throttles, as milliseconds.
///
/// **Not test-only any more, and the reason is the point of N3.** These were `#[cfg(test)]` because
/// only `abr::sim`'s plant read them, to pin half of `B_max` against the pipeline's own values.
/// `abr::plant::b_max_est_ms` now computes the reachable reserve inside the CONTROLLER, from these
/// and `aq_caps()` at run time rather than from a transcription — which is what makes `B*` a
/// property of the plant instead of a number somebody chose. `sim.rs` still keeps its own copy by
/// value, deliberately, so the plant grading the controller is not the controller agreeing with
/// itself.
pub(crate) use engine::feed_leads_ms;
pub(crate) use ffi::{VP_ACB, VP_EXPORTED, VP_NONE};

/// One consistent read of everything the on-screen diagnostics overlay shows (`ui::stats`).
///
/// A struct rather than twenty accessors for one reason: the panel must not tell a story that
/// never happened. Sampled field-by-field across a frame it could report "no frames" beside a
/// callback count taken 16 ms later, and the whole point is that a maintainer trusts the
/// photograph. One call, one instant, main thread.
///
/// Everything here is a MIRROR — see `Shared`'s diagnostics block. Nothing in the playback state
/// machine may read a `Diag` back.
///
/// `Default` is the never-started session — all zero, no window, nothing fed — which is both the
/// honest pre-playback reading and what the host tests build, since the real [`diag`] reaches
/// `starfish.c` symbols that do not exist on the dev Mac.
#[derive(Default)]
pub(crate) struct Diag {
    pub vp_mode: c_int,
    pub window_id: String,
    pub acb_ok: bool,
    pub place_rv: i32,
    pub placed_w: i32,
    pub placed_h: i32,
    pub stage: u8,
    pub load_completed: bool,
    pub load_failed: bool,
    pub cb_count: u32,
    pub pushed_any: bool,
    pub fed_v: i64,
    pub fed_a: i64,
    pub frames: i32,
    pub seen_frame: bool,
    pub aq_video: i64,
    pub aq_audio: i64,
    pub fed_v_pts: i64,
    pub fed_a_pts: i64,
    pub load_v: u8,
    pub load_a: u8,
    pub feed_state: u8,
    pub cb_err: i32,
    pub cb_err_at: u32,
    pub http_status: i32,
    pub net_rx: i64,
    pub load_at: u32,
    pub frame_at: u32,
    pub video_w: i32,
    pub video_h: i32,
    pub pos_ns: i64,
    pub dur_ns: i64,
    pub abr_mode: u8,
    pub abr_kbps: i64,
    pub abr_net_kbps: i64,
    pub abr_buffer_ms: i64,
    pub abr_ratio_pm: i64,
    pub abr_action: u8,
    pub abr_target_kbps: i64,
    pub abr_bad_windows: u8,
    pub abr_safe_kbps: i64,
    pub abr_optimal_kbps: i64,
    pub abr_unc_pm: i64,
    pub abr_samples: i64,
    pub abr_slope_ms_per_s: i64,
    pub abr_starve_secs: i64,
    pub abr_pred_pm: i64,
    pub abr_risk: i64,
    pub abr_why: u8,
}

impl Diag {
    pub fn vp_mode_str(&self) -> &'static str {
        match self.vp_mode {
            VP_EXPORTED => "exported window (webOS 5+)",
            VP_ACB => "ACB (webOS 4)",
            _ => "NONE — no video path",
        }
    }
    /// What the Load payload named as the video codec, or `—` before one was built.
    pub fn load_v_str(&self) -> &'static str {
        match self.load_v {
            1 => "H264",
            2 => "H265",
            _ => "—",
        }
    }
    /// …and the audio codec. `needAudio:false` is its own answer, not an absence.
    pub fn load_a_str(&self) -> &'static str {
        match self.load_a {
            1 => "AC3",
            2 => "AC3 PLUS",
            3 => "AAC",
            _ => "NONE (needAudio:false)",
        }
    }
    /// Why the VIDEO feeder is where it is. The video lane specifically: the picture is what a
    /// user complains about, and a two-lane string overflows the value column.
    ///
    /// `queue empty` vs `BufferFull` is the row's whole point — a dead PRODUCER and a dead SINK
    /// look identical from every other field on the panel and want opposite fixes.
    ///
    /// The throttle state is worded as what it IS, not as what it is waiting for. It was
    /// "waiting for a frame", which is the literal truth and reads as a stall — and it is the
    /// state a healthy playback sits in most of the time, because the feeder deliberately stays
    /// within `MAX_FEED_AHEAD_NS` of the presented position. The first person to see the panel in
    /// the wild asked why playback was stuck; it was not.
    pub fn feed_state_str(&self) -> &'static str {
        match self.feed_state {
            1 => "accepting",
            2 => "BufferFull (sink is full)",
            3 => "REFUSED",
            4 => "holding ~1.6 s ahead",
            5 => "queue empty (no data)",
            _ => "— nothing fed yet",
        }
    }
    /// Only an outright refusal is a fault. BufferFull is the steady state under the feed-ahead
    /// throttle, and the throttle and an empty queue are ordinary moments in a healthy stream.
    pub fn feed_is_fault(&self) -> bool {
        self.feed_state == 3
    }
}

pub(crate) fn diag() -> Diag {
    let (fed_v, fed_a) = engine::fed_totals();
    // `vp_window_id` hands back the seam's own static buffer — never NULL, "" when no window was
    // created — so this is a copy of a bounded char[64], not a borrow with a lifetime to reason about.
    let window_id = unsafe { std::ffi::CStr::from_ptr(ffi::vp_window_id()) }
        .to_string_lossy()
        .into_owned();
    Diag {
        vp_mode: ffi::vp_mode(),
        window_id,
        acb_ok: ACB_OK.load(Relaxed),
        place_rv: SHARED.dg_place_rv.load(Relaxed),
        placed_w: SHARED.dg_placed_w.load(Relaxed),
        placed_h: SHARED.dg_placed_h.load(Relaxed),
        stage: SHARED.dg_stage.load(Relaxed),
        load_completed: SHARED.load_completed.load(Relaxed),
        load_failed: SHARED.load_failed.load(Relaxed),
        cb_count: SHARED.dg_cb_count.load(Relaxed),
        pushed_any: crate::ff::pushed_any(),
        fed_v,
        fed_a,
        frames: SHARED.frames.load(Relaxed),
        seen_frame: SHARED.seen_frame.load(Relaxed),
        aq_video: SHARED.dg_aq_video.load(Relaxed),
        aq_audio: SHARED.dg_aq_audio.load(Relaxed),
        fed_v_pts: SHARED.dg_fed_v_pts.load(Relaxed),
        fed_a_pts: SHARED.dg_fed_a_pts.load(Relaxed),
        load_v: SHARED.dg_load_v.load(Relaxed),
        load_a: SHARED.dg_load_a.load(Relaxed),
        feed_state: SHARED.dg_feed_state.load(Relaxed),
        cb_err: SHARED.dg_cb_err.load(Relaxed),
        cb_err_at: SHARED.dg_cb_err_at.load(Relaxed),
        http_status: SHARED.dg_http_status.load(Relaxed),
        net_rx: SHARED.dg_net_rx.load(Relaxed),
        load_at: SHARED.dg_load_at.load(Relaxed),
        frame_at: SHARED.dg_frame_at.load(Relaxed),
        video_w: SHARED.video_w.load(Relaxed),
        video_h: SHARED.video_h.load(Relaxed),
        pos_ns: SHARED.playpos_ns.load(Relaxed),
        dur_ns: SHARED.duration_ns.load(Relaxed),
        abr_mode: SHARED.dg_abr_mode.load(Relaxed),
        abr_kbps: SHARED.dg_abr_kbps.load(Relaxed),
        abr_net_kbps: SHARED.dg_abr_net_kbps.load(Relaxed),
        abr_buffer_ms: SHARED.dg_abr_buffer_ms.load(Relaxed),
        abr_ratio_pm: SHARED.dg_abr_ratio_pm.load(Relaxed),
        abr_action: SHARED.dg_abr_action.load(Relaxed),
        abr_target_kbps: SHARED.dg_abr_target_kbps.load(Relaxed),
        abr_bad_windows: SHARED.dg_abr_bad_windows.load(Relaxed),
        abr_safe_kbps: SHARED.dg_abr_safe_kbps.load(Relaxed),
        abr_optimal_kbps: SHARED.dg_abr_optimal_kbps.load(Relaxed),
        abr_unc_pm: SHARED.dg_abr_unc_pm.load(Relaxed),
        abr_samples: SHARED.dg_abr_samples.load(Relaxed),
        abr_slope_ms_per_s: SHARED.dg_abr_slope_ms_per_s.load(Relaxed),
        abr_starve_secs: SHARED.dg_abr_starve_secs.load(Relaxed),
        abr_pred_pm: SHARED.dg_abr_pred_pm.load(Relaxed),
        abr_risk: SHARED.dg_abr_risk.load(Relaxed),
        abr_why: SHARED.dg_abr_why.load(Relaxed),
    }
}

pub(crate) fn seek_display_ns() -> i64 { SHARED.seek_display_ns.load(Relaxed) }
/// The playhead the user INTENDS, which is not always the one being published: while a seek is
/// still resolving (request → reopen → prime → Play) `playpos_ns` keeps reporting the PRE-seek
/// spot, so anything snapshotting "where are we?" inside that window snapshots the position the
/// user just left. The rule — an in-flight seek target wins, else the published position — used to
/// be open-coded at each reader that remembered it and was simply MISSING at the one that did not
/// (the OS-background save; see `app::intended_pos`). This is that rule, once.
///
/// Use it at every reader that means "where the user is". Keep the raw `playpos_ns` only where the
/// PUBLISHED position is the point: the re-pause gate (already behind `seek_pending() < 0`) and the
/// heartbeat's `pos=`, which `tests/run.py` grades real playback progress from — feeding it an
/// intended position would let a seek that never lands read as playback that climbed.
///
/// `ui/player_hud.rs` deliberately does NOT call this: it needs the same outer two rungs with the
/// live scrub preview between them, so its expression is a superset rather than a caller.
pub(crate) fn intended_pos_ns() -> i64 {
    let t = seek_display_ns();
    if loading() && t >= 0 { t } else { playpos_ns() }
}
/// request an audio-track switch (Plex audioStreamID); the pump forces a fresh
/// transcode with that source audio at the current position next tick.
pub(crate) fn request_audio_switch(sid: i64) {
    SHARED.pending_audio_sid.store(sid, Relaxed);
    SHARED.sub_cues.lock().unwrap().clear(); // the fresh transcode carries no embedded subs
}
/// request a NATIVE audio-track switch (direct-play, NO transcode): feed the 0-based `audio_idx`
/// audio stream from the same MKV with codec `codec`. The pump reloads direct-play at the current
/// position next tick (switch_audio_native). Used when the item direct-plays and the target track
/// is a direct-playable codec (aac/ac3/eac3).
pub(crate) fn request_audio_track(audio_idx: i32, codec: &str) {
    crate::route::set_stream_acodec(codec); // the reload's Load payload uses this audio codec
    SHARED.pending_audio_idx.store(audio_idx, Relaxed);
    SHARED.sub_cues.lock().unwrap().clear();
}
/// reset to the default (best) audio stream — called on a new item so a prior track choice
/// does not leak across items (desired_audio_idx persists across seeks, not across items).
pub(crate) fn reset_audio_track() {
    SHARED.desired_audio_idx.store(-1, Relaxed);
}
/// reset the subtitle selection to Off — called on a NEW item. Like desired_audio_idx, the
/// subtitle selection PERSISTS across seeks/reloads (it is no longer cleared in reset_session),
/// so a reload-based seek (transcode, or the direct-play reload fallback) keeps the chosen sub
/// instead of silently turning subtitles off.
pub(crate) fn reset_subtitle() {
    SHARED.desired_sub_idx.store(-1, Relaxed);
}
/// select the audio stream index the demuxer feeds at the FIRST Load (before start_bufferfeed) —
/// used by the decision to direct-play a non-default direct-playable track (e.g. an AC3 track on
/// a TrueHD-default item). -1 = default/best.
pub(crate) fn set_audio_track(idx: i32) {
    SHARED.desired_audio_idx.store(idx, Relaxed);
}
/// request a re-transcode at the current position with the CURRENT audio + subtitle —
/// used when a subtitle is (de)selected while already transcoding, so the server
/// re-burns (or drops) it. No-op-ish if not transcoding (the caller gates on that).
pub(crate) fn request_transcode_refresh() {
    SHARED.pending_retranscode.store(true, Relaxed);
    SHARED.sub_cues.lock().unwrap().clear(); // burned/absent in the fresh transcode
}

/// Whether a route change has scheduled an encoder rebuild. Test-visible so route policy can be
/// graded independently of the pump's frame timing.
#[cfg(test)]
pub(crate) fn pending_transcode_refresh() -> bool {
    SHARED.pending_retranscode.load(Relaxed)
}

/// Request the main-thread HLS→Original pipeline replacement. Used by an explicit Original pick;
/// the adaptive worker publishes through the same atomic after its source probes pass.
pub(crate) fn request_original_recovery() {
    SHARED.auto_recover_kbps.store(1, std::sync::atomic::Ordering::Release);
    SHARED.pending_retranscode.store(false, Relaxed);
    SHARED.sub_cues.lock().unwrap().clear();
}

// ---- client-rendered subtitles (direct-play only; a transcode carries no subs) ----
/// selected subtitle track index (-1 = off); the demuxer reads this per block.
pub(crate) fn desired_sub_idx() -> i32 { SHARED.desired_sub_idx.load(Relaxed) }
/// select a subtitle track by index (-1 = off). Does NOT clear the cue store: the demuxer
/// pushes cues for EVERY text track regardless of selection, so the buffered region's cues for
/// the newly-selected track are already present and the switch shows immediately. Clearing here
/// would reintroduce the ~10-20s buffer-gap delay (the demuxer runs well ahead of the playhead).
/// A new item / transcode re-point clears the store via reset_session / the pump.
pub(crate) fn request_subtitle(idx: i32) {
    SHARED.desired_sub_idx.store(idx, Relaxed);
    if idx < 0 {
        // subs Off: free the image-cue RGBA store now (the demuxer also stops decoding new
        // bitmap cues while off — see ff.rs's desired_sub_idx gate)
        SHARED.sub_bitmaps.lock().unwrap().clear();
    }
}
/// push a ready (already-clean) subtitle cue into the shared store, tagged with its 0-based
/// track index (the demux pushes for every text track).
/// Bounded by TIME rather than a fixed count: since every track is pushed regardless of
/// selection, drop cues already well behind the playhead and keep a generous forward window
/// (the demuxer reads ~10-20s ahead). A hard cap guards against a runaway.
pub(crate) fn push_subtitle_text(track: i32, start_ns: i64, end_ns: i64, text: String) {
    if text.is_empty() {
        return;
    }
    let mut cues = SHARED.sub_cues.lock().unwrap();
    let floor = SHARED.playpos_ns.load(Relaxed) - 2_000_000_000;
    cues.retain(|c| c.end_ns >= floor);
    if cues.len() >= 512 {
        cues.remove(0);
    }
    cues.push(SubCue { track, start_ns, end_ns, text });
}
/// demux (D-thread) pushes a subtitle cue (content-time ns) for track `track`. Called for
/// EVERY text track so a mid-play switch is instant; only the selected track's cues are logged.
pub(crate) fn push_subtitle_cue(track: i32, start_ns: i64, end_ns: i64, payload: &[u8], is_ass: bool) {
    let text = sub_text(payload, is_ass);
    if text.is_empty() {
        return;
    }
    if track == SHARED.desired_sub_idx.load(Relaxed) {
        log(&format!("sub cue [{}..{}ms] {:?}", start_ns / 1_000_000, end_ns / 1_000_000,
            text.chars().take(34).collect::<String>()));
    }
    push_subtitle_text(track, start_ns, end_ns, text);
}
/// the selected track's subtitle text active at `now_ns`, or None (also None when off).
pub(crate) fn active_subtitle(now_ns: i64) -> Option<String> {
    let sel = SHARED.desired_sub_idx.load(Relaxed);
    if sel < 0 {
        return None;
    }
    let cues = SHARED.sub_cues.lock().unwrap();
    cues.iter()
        .rev()
        .find(|c| c.track == sel && now_ns >= c.start_ns && now_ns < c.end_ns)
        .map(|c| c.text.clone())
}

/// Image-subtitle store (PGS/VobSub). The demux (D) thread decodes the SELECTED track's
/// bitmaps and pushes them here; the renderer (M) reads the active one for the playpos. A new
/// display-set supersedes any still-open cue on the same track (PGS signals the end via a later
/// CLEAR or a superseding set, both handled here). Bounded by time like the text store.
///
/// `cw`/`ch` are the stream's authoring canvas (0 = the decoder never declared one) and every
/// rect's coords are relative to it — the renderer scales the whole set into the video rect, so
/// a 720×480 VobSub and a 1920×1080 PGS land the same size on screen.
pub(crate) fn push_subtitle_bitmap(track: i32, start_ns: i64, cw: i32, ch: i32, rects: Vec<SubRect>) {
    if rects.is_empty() {
        return;
    }
    let mut v = SHARED.sub_bitmaps.lock().unwrap();
    for c in v.iter_mut() {
        if c.track == track && c.end_ns == i64::MAX {
            c.end_ns = start_ns; // this set replaces the one still showing
        }
    }
    let floor = SHARED.playpos_ns.load(Relaxed) - 2_000_000_000;
    v.retain(|c| c.end_ns >= floor);
    v.push(SubBitmap { track, start_ns, end_ns: i64::MAX, cw, ch, rects });
    // Hard RAM ceiling: decoding ALL image tracks means several are buffered at once, so bound
    // the store by total RGBA bytes (not count). ~24 MB is comfortable headroom on the direct-play
    // path. A multi-rect display set counts as the sum of its rects, which is why the budget is
    // bytes and not cue count — and which is what made the eviction ORDER start to matter.
    //
    // `v` is in demux (increasing-pts) order and the time-retain above has already dropped
    // everything more than 2s behind the playhead, so `v[0]` is the cue AT or just behind the
    // playhead — the one about to be drawn — while the tail is the demuxer's 10-20s read-ahead.
    // Evicting index 0 (what this did) therefore blanks the subtitle the viewer is reading and
    // keeps cues they have not reached. So: drop a cue the playhead has already passed first,
    // since it can never be shown again; only when none is left does the FAR END of the
    // read-ahead go, because that cue is at least not on screen yet.
    const BUDGET: usize = 24 * 1024 * 1024;
    let mut total: usize = v.iter().map(|c| c.bytes()).sum();
    let now = SHARED.playpos_ns.load(Relaxed);
    while total > BUDGET && v.len() > 1 {
        let i = v.iter().position(|c| c.end_ns <= now).unwrap_or(v.len() - 1);
        total -= v[i].bytes();
        v.remove(i);
    }
}
/// A CLEAR display-set (num_rects==0): close the currently-open cue on this track at `end_ns`.
pub(crate) fn close_subtitle_bitmap(track: i32, end_ns: i64) {
    let mut v = SHARED.sub_bitmaps.lock().unwrap();
    for c in v.iter_mut() {
        if c.track == track && c.end_ns == i64::MAX {
            c.end_ns = end_ns;
        }
    }
}
/// Cheap per-frame lookup: the `start_ns` key of the selected track's image cue active at
/// `now_ns`, or None. The renderer only re-uploads its GL texture when this key changes.
pub(crate) fn active_bitmap_key(now_ns: i64) -> Option<i64> {
    let sel = SHARED.desired_sub_idx.load(Relaxed);
    if sel < 0 {
        return None;
    }
    let v = SHARED.sub_bitmaps.lock().unwrap();
    v.iter()
        .rev()
        .find(|c| c.track == sel && now_ns >= c.start_ns && now_ns < c.end_ns)
        .map(|c| c.start_ns)
}
/// Fetch (canvas_w, canvas_h, rects) for the selected track's display set with this `start_ns`
/// key. Clones the bitmaps once (only when the renderer sees a new key), so the per-frame path
/// stays cheap.
pub(crate) fn bitmap_by_key(key: i64) -> Option<(i32, i32, Vec<SubRect>)> {
    let sel = SHARED.desired_sub_idx.load(Relaxed);
    let v = SHARED.sub_bitmaps.lock().unwrap();
    v.iter()
        .rev()
        .find(|c| c.track == sel && c.start_ns == key)
        .map(|c| (c.cw, c.ch, c.rects.clone()))
}
/// extract displayable text from a subtitle block (SRT = raw UTF-8; ASS = the field
/// after the 8th comma), stripping tags/override codes and normalizing line breaks.
fn sub_text(payload: &[u8], is_ass: bool) -> String {
    let raw = String::from_utf8_lossy(payload);
    let s = if is_ass {
        raw.splitn(9, ',').nth(8).unwrap_or("").to_string()
    } else {
        raw.into_owned()
    };
    let mut out = String::with_capacity(s.len());
    let mut ch = s.chars().peekable();
    while let Some(c) = ch.next() {
        match c {
            '<' => while let Some(x) = ch.next() { if x == '>' { break; } },   // <i></i>
            '{' => while let Some(x) = ch.next() { if x == '}' { break; } },   // {\an8}
            '\\' => match ch.peek() {
                Some('N') | Some('n') => { ch.next(); out.push('\n'); }
                Some('h') => { ch.next(); out.push(' '); }
                _ => out.push('\\'),
            },
            '\r' => {}
            _ => out.push(c),
        }
    }
    out.trim().to_string()
}

pub(crate) use crate::log; // event-log sink (crate-wide single copy in lib.rs)

fn find(h: &[u8], n: &[u8]) -> bool {
    !n.is_empty() && h.windows(n.len()).any(|w| w == n)
}
/// bytes between `prefix` and the next `term`, or None if `prefix` absent.
fn between(h: &[u8], prefix: &[u8], term: u8) -> Option<Vec<u8>> {
    let start = h.windows(prefix.len()).position(|w| w == prefix)? + prefix.len();
    let rest = &h[start..];
    let end = rest.iter().position(|&b| b == term).unwrap_or(rest.len());
    Some(rest[..end].to_vec())
}

/// Monotonic milliseconds, from an origin fixed at the first call.
///
/// Not SDL ticks: this is read on the pipeline's own callback thread, and the value is only ever
/// differenced, so a private origin is enough and owes SDL nothing.
fn vclock_ms() -> u32 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis() as u32
}

/// **The pipeline's `FRAMEREADY` cadence since the last call**: ticks received, and the worst gap
/// between two consecutive ones in milliseconds. Draining, like
/// [`crate::ui::idle::take_presents`] — the heartbeat is the one caller, once a second.
///
/// **This is a liveness signal, not a frame rate.** See [`sf_on_event`]: the healthy reading on
/// every codec, resolution and container measured so far is `5` and `201`, because the tick is
/// ~5 Hz regardless of the stream's frame rate. What it is good for is the opposite question —
/// whether the pipeline still thinks it is running. A steady `5 / 201` through a picture the
/// viewer says is stuttering is a real and useful finding: it rules the fault OUT of everything
/// this process can see, and sends the search to the display side.
pub(crate) fn vplane_take() -> (u32, u32) {
    (SHARED.dg_vpres_ct.swap(0, Relaxed), SHARED.dg_vpres_gap.swap(0, Relaxed))
}

/// pipeline event on the LIBRARY thread. type 0 = `PF_EVENT_TYPE_FRAMEREADY` (num = fed pts).
///
/// **`FRAMEREADY` is NOT one callback per decoded frame on this firmware, and reading it that way
/// is how a stutter investigation gets the wrong answer.** Kodi's Starfish path treats it as one
/// picture per event, which is where the old "frame presented" gloss here came from. Measured on
/// webOS 4.10.2 (2026-08-21), it is a **~5 Hz position tick**: a 1080p H264 direct play, a 4K HEVC
/// direct play and a visibly stuttering Dolby Vision direct play all deliver it 5 times a second,
/// 201 ms apart, to the millisecond. So [`frames`](crate::player::shared::Shared::frames) counts
/// TICKS, not frames — which is what `pump`'s `frames >= 2` gate really means (≈400 ms of
/// playback, not two pictures) and what `ui::stats` really shows.
///
/// The consequence for diagnosis: this callback can say the pipeline still believes it is
/// presenting, and cannot say the picture is smooth. The video plane's real cadence is not
/// observable from this process at all — the evidence for that lives in the TV's own kernel log
/// (`kad-hdr`).
/// Panic-guarded (unwinding into C is UB); touches only SHARED.
#[no_mangle]
pub extern "C" fn sf_on_event(ty: c_int, num: i64, s: *const c_char) {
    let _ = catch_unwind(AssertUnwindSafe(|| sf_on_event_inner(ty, num, s)));
}
fn sf_on_event_inner(ty: c_int, num: i64, s: *const c_char) {
    if ty != 0 {
        // The diagnostics census, beside the log line that already records every event. A COUNT is
        // what the read-out needs: "Load completed and then nothing ever called us" is the sharpest
        // symptom the stuck-buffering reports could carry, and it is invisible in a log the user
        // cannot reach. Kept here rather than in the `ty` dispatch below so an event we do not
        // handle still counts — an unhandled callback is still the pipeline talking.
        let n = SHARED.dg_cb_count.fetch_add(1, Relaxed) + 1;
        // Latch the FIRST error, with the callback index it arrived at. Sticky, because a later
        // healthy callback must not erase the one event that explains the session — and the index
        // separates "refused immediately" from "died after a long healthy run". Only `ty == 18`:
        // it is the one value this project has ever acted on, and it sits below the 0x1c point
        // where the numbering shifts between webOS 4 and 5+, so it means the same on both. Naming
        // any higher type would be a confident lie on the firmware we cannot test.
        if ty == 18 && SHARED.dg_cb_err.load(Relaxed) == 0 {
            SHARED.dg_cb_err.store(ty, Relaxed);
            SHARED.dg_cb_err_at.store(n, Relaxed);
        }
        let preview = if s.is_null() { String::new() } else {
            unsafe { CStr::from_ptr(s) }.to_string_lossy().chars().take(1400).collect()
        };
        log(&format!("smp_cb type={ty} num={num} str={preview}"));
    }
    if ty == 0 {
        // a POSITION UPDATE — map fed pts -> real content position.
        //
        // **It is not one per presented frame, and `vtick`/`vgap` cannot see a dropped frame.**
        // Measured 2026-08-21 across every Profile 5 run: `vtick=5 vgap=201ms`, unvarying, on
        // clean and visibly stuttering playback alike. The pipeline emits this at 5 Hz — it is a
        // position report, not a vsync. This comment used to say "a frame was PRESENTED" and the
        // `dg_vpres_*` block below still describes a cadence probe; both were written from the
        // callback's NAME rather than from its rate, and an instrument that reads a flat 201 ms
        // through the fault it exists to catch is worse than no instrument, because it is quoted.
        //
        // The real per-frame cadence is only observable from LG's own tracing — `GST_DEBUG=
        // dualsequencer:6` via `/tmp/plxnative-gstlog`, whose `push_dual` and `lxvideosink`
        // timestamps give one line per frame. That was long avoided as perturbing; it is not, at
        // level 6: the same scene measured 123 LUT misses uninstrumented and 122 with the trace
        // running. Level 9 IS perturbing and is what that reputation came from.
        //
        // `pres_fed` below is still sound — the feed-ahead throttle wants a position, and a 200 ms
        // granularity against a 1.6 s budget is ample. Only the TIMING half was wrong.
        let t = vclock_ms();
        let prev = SHARED.dg_vpres_at.swap(t, Relaxed);
        SHARED.dg_vpres_ct.fetch_add(1, Relaxed);
        if prev != 0 {
            SHARED.dg_vpres_gap.fetch_max(t.saturating_sub(prev), Relaxed);
        }
        SHARED.frames.fetch_add(1, Relaxed);
        SHARED.seen_frame.store(true, Relaxed); // session-scoped: unlike `frames`, a seek won't clear it
        SHARED.pres_fed.store(num, Relaxed); // raw fed pts, for the feed-ahead throttle
        SHARED.playpos_ns
            .store(num - SHARED.pts_shift.load(Relaxed) + SHARED.disp_base.load(Relaxed), Relaxed);
    }
    if s.is_null() {
        return;
    }
    let b = unsafe { CStr::from_ptr(s) }.to_bytes();

    {
        let mut mid = SHARED.media_id.lock().unwrap();
        if mid.is_none() {
            if let Some(id) = between(b, b"\"context\":\"", b'"').or_else(|| between(b, b"\"mediaId\":\"", b'"')) {
                if let Ok(c) = std::ffi::CString::new(id.clone()) {
                    log(&format!("SMP context/mediaId={}", String::from_utf8_lossy(&id)));
                    *mid = Some(c);
                }
            }
        }
    }

    if !SHARED.load_completed.load(Relaxed) && (find(b, b"loadCompleted") || find(b, b"\"loaded\"")) {
        SHARED.load_completed.store(true, Relaxed);
        log("SMP loadCompleted");
    }

    {
        // capture the WHOLE sourceInfo envelope VERBATIM (byte-for-byte + NUL), never re-encoded
        let mut si = SHARED.source_info.lock().unwrap();
        if si.is_none() && find(b, b"\"video\":") && find(b, b"\"context\":") {
            let mut v = Vec::with_capacity(b.len() + 1);
            v.extend_from_slice(b);
            v.push(0);
            log(&format!("SMP sourceInfoRaw captured ({} bytes)", b.len()));
            *si = Some(v);
        }
    }
}

#[no_mangle]
pub extern "C" fn acb_on_event(ev: c_long, reply: *const c_char) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let r = if reply.is_null() { String::new() } else {
            unsafe { CStr::from_ptr(reply) }.to_string_lossy().into_owned()
        };
        log(&format!("acb_cb ev={ev} reply={r}"));
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A seek that is given up on must not leave the spinner armed forever.**
    ///
    /// `request_seek` sets `SHARED.seeking`, and the ONLY place that cleared it was the successful
    /// prime→Play. `pump::set_state` publishes `PlaybackState::Seeking` from that flag ahead of
    /// every other arm — deliberately, since the frames on the panel during a seek are the
    /// pre-seek ones — so a leaked flag means a permanent spinner, a playhead frozen at the target
    /// and `is_playing()` false, while the pipeline plays on underneath. Device-measured: 84
    /// seconds of exactly that, through 37 segment acquisitions and four rung commits.
    ///
    /// Differential: against the code before `abandon_seek` existed there was nothing to call, and
    /// both assertions below fail on the state `request_seek` leaves.
    ///
    /// Takes the crate lock because `SHARED` is a process-wide global and other modules' tests
    /// read the playback state.
    #[test]
    fn an_abandoned_seek_disarms_the_spinner_and_the_frozen_playhead() {
        let _guard = crate::testlock::serial();
        let was_seeking = SHARED.seeking.load(Relaxed);
        let was_display = SHARED.seek_display_ns.load(Relaxed);

        request_seek(40_000_000_000);
        assert!(SHARED.seeking.load(Relaxed), "the requester arms the spinner");
        assert_eq!(seek_display_ns(), 40_000_000_000, "and freezes the playhead at the target");

        abandon_seek();
        assert!(!SHARED.seeking.load(Relaxed), "giving up must disarm it");
        assert_eq!(seek_display_ns(), -1, "a stale target would keep the playhead frozen");

        SHARED.seeking.store(was_seeking, Relaxed);
        SHARED.seek_display_ns.store(was_display, Relaxed);
    }

    /// Issue #22: the error must NAME an audio-only stream, and name the right party. On a
    /// transcode it is the server's doing — and on a KNOWN-free server the reason appends the
    /// subscription as a FACT, never as the cause: the audit's row 1 says h264 encoding is free
    /// everywhere and the profile's target chain ends in h264, so a missing Pass cannot be WHY
    /// the video is gone, and asserting it would point the user at a purchase that fixes nothing
    /// (the confident wrong answer this arm exists to prevent). Known-true or UNKNOWN keeps the
    /// neutral wording alone: a subscription the app cannot prove absent is never even named.
    /// Direct-played, the fault is the file's whatever the subscription says. Drives the pure
    /// shape only, so no globals move and nothing here can race the HUD test that reads the real
    /// (default-false) flags.
    #[test]
    fn an_audio_only_stream_is_blamed_on_whoever_sent_it() {
        use crate::plex::serverinfo::Subscription as Sub;
        // transcode on a known-free server: the Pass appears as a parenthetical fact on the
        // panel, as the capsule flag for the read-out…
        let e = error_shape(true, true, Sub::No, None);
        assert!(e.caption.to_str().unwrap().contains("server sent audio only"), "{:?}", e.caption);
        assert!(e.panel.contains("no usable video transcode target"), "{}", e.panel);
        assert!(e.panel.contains("server has no Plex Pass"), "{}", e.panel);
        assert!(e.no_pass, "the read-out draws the capsule from this flag");
        // …and never as a cause — h264 encoding is free everywhere (audit row 1). The read-out
        // reason carries no Pass words at all: the capsule line states the fact separately.
        assert!(!e.panel.contains("cannot encode"), "causation may not be asserted: {}", e.panel);
        assert!(!e.readout.contains("Plex Pass"), "the capsule, not prose, names the Pass: {}", e.readout);
        assert!(e.detail.is_empty(), "only the server's own verdict fills the detail line");
        // known-Pass'd or never-heard-from: today's wording, and no Pass blame anywhere in it
        for sub in [Sub::Yes, Sub::Unknown] {
            let e = error_shape(true, true, sub, None);
            assert!(e.caption.to_str().unwrap().contains("server sent audio only"), "{:?}", e.caption);
            assert!(e.panel.contains("no usable video transcode target"), "{} ({sub:?})", e.panel);
            assert!(!e.panel.contains("Plex Pass"), "an unproven subscription must not be blamed ({sub:?})");
            assert!(!e.no_pass, "the capsule may not appear on an unproven subscription ({sub:?})");
        }
        for sub in [Sub::Unknown, Sub::No, Sub::Yes] {
            let e = error_shape(true, false, sub, None);
            assert!(e.caption.to_str().unwrap().contains("no video in the file"), "{:?}", e.caption);
            assert!(e.panel.contains("no video track"), "direct play blames the file, not the server");
            assert!(!e.no_pass, "an audio-only FILE is not a subscription story");
            for transcoding in [false, true] {
                let e = error_shape(false, transcoding, sub, None);
                assert_eq!(e.caption.to_str().unwrap(), "Playback failed", "no reason may be invented");
                assert_eq!(e.panel, "");
                assert_eq!(e.readout, "");
                assert!(e.detail.is_empty());
                assert!(!e.no_pass);
            }
        }
    }

    /// The PRE-FLIGHT arm: `/decision` refused the item before a byte of video moved, so the reason
    /// is the SERVER's and not our inference. Three things are asserted and each was a way to get
    /// this wrong. (1) The reason line is ours and fixed, while the detail is the server's sentence
    /// **verbatim** — not sentence-cased, not re-worded, because its wording is not ours and it is
    /// the line a maintainer photographs. (2) The capsule NEVER appears here, on any subscription
    /// state, including a proven-free server and including a verdict that names HEVC — the server
    /// named the cause, so naming a subscription beside it is the speculation the tristate rule
    /// forbids. (3) The arm OUTRANKS the demux-derived ones: nothing was ever demuxed, so a stale
    /// `no_video` from a previous session must not re-word a refusal.
    #[test]
    fn a_refused_decision_quotes_the_server_and_never_names_a_subscription() {
        use crate::plex::serverinfo::Subscription as Sub;
        const VP9: &str = "Cannot convert this item. Implementation for video encoder 'vp9' not found.";
        for sub in [Sub::Unknown, Sub::No, Sub::Yes] {
            // graded with `no_video`/`transcoding` BOTH set — the arm that would otherwise win
            let e = error_shape(true, true, sub, Some(VP9));
            assert_eq!(e.readout, "The server cannot play or convert this file", "({sub:?})");
            assert_eq!(e.detail, VP9, "the server's sentence is reproduced unedited ({sub:?})");
            assert!(!e.no_pass, "no capsule on this arm, ever — the server named the cause ({sub:?})");
            assert!(!e.readout.contains("Plex Pass") && !e.panel.contains("Plex Pass"), "({sub:?})");
            assert!(e.caption.to_str().unwrap().starts_with("Playback failed"), "{:?}", e.caption);
        }
        // an HEVC verdict on a server PROVEN to have no Pass is the temptation, and still no capsule
        let e = error_shape(false, true, Sub::No, Some("Implementation for video encoder 'hevc' not found."));
        assert!(!e.no_pass);
        // a server that refused without saying why: the reason still lands, the quote line does not
        let e = error_shape(false, true, Sub::No, Some(""));
        assert_eq!(e.readout, "The server cannot play or convert this file");
        assert!(e.detail.is_empty(), "an empty verdict draws no quote line at all");
    }

    /// **The subscription the read-out states is the FAILING ITEM's server's, not the current
    /// one's** — the wiring the pure shape above cannot see, because it takes the tristate as an
    /// argument.
    ///
    /// The two are routinely different: `plex::servers` keeps `current` pinned to the primary while
    /// a borrowed film plays, so `serverinfo::subscription()` here answered for OUR server on every
    /// failure of a share's item. Both polarities are silent and wrong. A film borrowed from a
    /// Pass-less share dropped the "(server has no Plex Pass)" clause and the read-out's capsule —
    /// the exact support fact issue #22 was reported without, on the exact configuration (someone
    /// else's free server) that produced it. And with the primary free and the share Pass'd, the
    /// capsule appeared on a failure nothing about a subscription explains, which is the confident
    /// wrong answer `error_shape`'s tristate rule exists to prevent.
    ///
    /// Registry, subscription slots and `route`'s playing identity are all crate globals, so this
    /// holds `testlock::serial()` and puts every one of them back on the way OUT — the discipline
    /// `serverinfo`'s own multi-server test states, and the reason its `Fresh` guard has a `Drop`.
    #[test]
    fn the_failure_read_out_states_the_playing_items_server_not_the_current_one() {
        use crate::plex::serverinfo::{store_for_test, Subscription as Sub};
        struct Fresh {
            _g: std::sync::MutexGuard<'static, ()>,
            sid: crate::plex::ServerId,
        }
        impl Drop for Fresh {
            fn drop(&mut self) {
                crate::route::swap_cur_sid_for_test(self.sid);
                crate::plex::reset_servers_for_test();
            }
        }
        let g = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let _fresh = Fresh { _g: g, sid: crate::route::swap_cur_sid_for_test(crate::plex::ServerId::UNSET) };

        let reg = |m: &str, host: &str| crate::plex::register_for_test(m, host, 32400, "tok", "cid");
        let (ours, theirs) = (reg("mach-A", "10.0.0.1"), reg("mach-B", "10.0.0.2"));
        // the slot arrays outlive `reset_servers_for_test` — start from the boot state explicitly
        store_for_test(ours, Sub::Unknown, "");
        store_for_test(theirs, Sub::Unknown, "");
        // our own server has a Plex Pass; the friend's share does not
        store_for_test(ours, Sub::Yes, "1.43.3.10861-cd85035e7");
        store_for_test(theirs, Sub::No, "1.32.0.6918-free");
        // …and browsing a share does NOT re-point `current`, which is the whole trap
        assert!(crate::plex::set_current(ours));

        crate::route::swap_cur_sid_for_test(theirs);
        assert_eq!(playing_subscription(), Sub::No, "the borrowed film's own server is the one that failed");
        let e = error_shape(true, true, playing_subscription(), None);
        assert!(e.no_pass, "so the read-out draws the capsule…");
        assert!(e.panel.contains("server has no Plex Pass"), "…and the panel states the fact: {}", e.panel);

        // the inverse polarity: playing from OUR Pass'd server while `current` sits on the share
        assert!(crate::plex::set_current(theirs));
        crate::route::swap_cur_sid_for_test(ours);
        assert_eq!(playing_subscription(), Sub::Yes, "the current server's answer is not this item's");
        assert!(!error_shape(true, true, playing_subscription(), None).no_pass, "no capsule may be invented");

        // before the first play there is no playing server, and "we have not heard" is the honest
        // answer — never slot 0's, and never a blamed subscription
        crate::route::swap_cur_sid_for_test(crate::plex::ServerId::UNSET);
        assert_eq!(playing_subscription(), Sub::Unknown);
        assert!(!error_shape(true, true, playing_subscription(), None).no_pass);
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> SubRect {
        SubRect { x, y, w, h, rgba: vec![0u8; (w * h * 4) as usize] }
    }

    /// The image-subtitle store, exercised as a display SET rather than a single bitmap. Three
    /// invariants moved when multi-rect landed and none of them is observable on the host except
    /// here: every rect of a set survives the round trip under ONE key (so a two-line PGS cue is
    /// not silently halved); a later set still closes the one still showing; and the RAM ceiling
    /// counts a set's rects together, so a multi-rect cue cannot smuggle bytes past the budget.
    ///
    /// Takes the crate-wide `testlock` — `SHARED` is a process-global the whole player shares.
    #[test]
    fn an_image_display_set_round_trips_whole_and_is_superseded_as_a_unit() {
        let _g = crate::testlock::serial();
        SHARED.sub_bitmaps.lock().unwrap().clear();
        SHARED.playpos_ns.store(0, Relaxed);
        SHARED.desired_sub_idx.store(0, Relaxed);

        // a two-rect set (dialogue plus a sign), authored on a DVD canvas
        push_subtitle_bitmap(0, 1_000, 720, 480, vec![rect(60, 400, 600, 60), rect(100, 20, 200, 40)]);
        let key = active_bitmap_key(1_500).expect("the set should be active at its start");
        let (cw, ch, rects) = bitmap_by_key(key).expect("the active key must resolve");
        assert_eq!((cw, ch), (720, 480), "the authoring canvas travels with the set");
        assert_eq!(rects.len(), 2, "BOTH rects must survive — rect 0 only was the bug");
        assert_eq!((rects[1].x, rects[1].y), (100, 20));

        // the next set closes the open one AT ITS OWN START — a display set stays up until the
        // one that replaces it begins, so the handover is seamless and never double-shows
        push_subtitle_bitmap(0, 5_000, 720, 480, vec![rect(60, 400, 600, 60)]);
        assert_eq!(active_bitmap_key(4_999), Some(1_000), "the first set holds right up to the handover");
        assert_eq!(active_bitmap_key(5_000), Some(5_000), "and the second takes over on that exact ns");

        // an empty set is not a cue: it must not land and must not close what is showing
        push_subtitle_bitmap(0, 6_000, 720, 480, Vec::new());
        assert_eq!(active_bitmap_key(5_500), Some(5_000));

        // The byte budget charges a set for ALL its rects (2 x 4 MB here), and — the part that
        // only matters once a set can be big — it must not evict the cue the viewer is READING.
        // The playhead sits inside the 5_000 cue, so that one has to survive four 8 MB sets
        // arriving from the demuxer's read-ahead; what goes is the far end of that read-ahead.
        SHARED.playpos_ns.store(5_500, Relaxed);
        for i in 0..4 {
            push_subtitle_bitmap(0, 10_000 + i, 720, 480, vec![rect(0, 0, 1024, 1024), rect(0, 0, 1024, 1024)]);
        }
        let v = SHARED.sub_bitmaps.lock().unwrap();
        let total: usize = v.iter().map(|c| c.bytes()).sum();
        assert!(total <= 24 * 1024 * 1024, "the store stayed inside its ceiling ({total} bytes)");
        assert!(v.iter().any(|c| c.start_ns == 5_000), "the cue under the playhead was not evicted");
        drop(v);
        assert_eq!(active_bitmap_key(5_500), Some(5_000), "and it is still the one on screen");

        // leave the globals as they were found — `desired_sub_idx` deliberately survives a reset
        // (shared.rs), so a test that leaves it selected changes what the NEXT one sees
        SHARED.sub_bitmaps.lock().unwrap().clear();
        SHARED.desired_sub_idx.store(-1, Relaxed);
    }
}
