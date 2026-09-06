//! Boot-time helpers of the app core: the desktop window size, the panic logger, the replay
//! budget, the direct-server trigger, the cursor and the loop-rate tick. Moved out of `app.rs`
//! verbatim in phase 1a of the UI restructure (a pure move; `pub(super)` widening only).

use super::*;

/// The window size a DESKTOP should open at, in points: the authored 1920x1080 canvas divided by
/// the smallest whole number that fits the usable display area.
///
/// **An exact divisor, never a best fit.** `surface::scale` will letterbox any drawable it is
/// given, so an arbitrary size would *work* — it would just be soft, because every glyph and icon
/// mask in this app is rasterized for a 1:1 surface (`gfx::snap`, and the crispness contract in
/// `theme.rs`) and a fractional scale resamples all of it. 1/1, 1/2 and 1/3 keep whole texels whole.
///
/// The television is untouched by any of this: it takes the panel, and the canvas IS the surface.
/// A Mac is the case the `surface` doc was written against — 1920x1080 exceeds the usable area of
/// every laptop display Apple ships, so asking for it flatly would put the title bar above the
/// screen and the bottom of the interface under the Dock.
///
/// Falls back to the canvas size if SDL cannot answer, which is the behaviour this replaced.
#[cfg(feature = "hostsim")]
pub(super) fn desktop_window_size() -> (c_int, c_int) {
    // `PLXNATIVE_WIN=<w>x<h>` overrides the fit entirely — `make sim-shot SIM_W=1920 SIM_H=1080`.
    // It exists because the fit below is chosen for a HUMAN looking at a window, and a screenshot
    // is not that: on a 1x display the divisor lands on 2 and every shot comes back 960x540, which
    // is half the canvas the UI is authored at. A hairline, a 1px edge-sheen and a snapped glyph
    // are exactly the things that do not survive that, so a shot taken to JUDGE the interface has
    // to be asked for at full size. Off-screen edges are fine for a headless grab: the drawable is
    // the window's own framebuffer, not the part of it the compositor happens to show.
    if let Some(v) = std::env::var_os("PLXNATIVE_WIN") {
        let v = v.to_string_lossy().to_lowercase();
        if let Some((w, h)) = v.split_once('x') {
            if let (Ok(w), Ok(h)) = (w.trim().parse::<c_int>(), h.trim().parse::<c_int>()) {
                if w > 0 && h > 0 {
                    return (w, h);
                }
            }
        }
    }
    let mut r = [0 as c_int; 4]; // SDL_Rect: x, y, w, h
    let ok = unsafe { SDL_GetDisplayUsableBounds(0, r.as_mut_ptr()) } == 0;
    let (uw, uh) = (r[2], r[3]);
    if !ok || uw <= 0 || uh <= 0 {
        return (SCR_W, SCR_H);
    }
    // A little headroom under the usable bounds: a window flush against them reads as a fullscreen
    // that went wrong rather than as a deliberate size.
    for div in 1..=3 {
        let (w, h) = (SCR_W / div, SCR_H / div);
        if w <= (uw as f32 * 0.95) as c_int && h <= (uh as f32 * 0.95) as c_int {
            return (w, h);
        }
    }
    (SCR_W / 3, SCR_H / 3)
}

/// Log every Rust panic (message + source location + thread) to the event log AND the
/// persistent crash log BEFORE it unwinds. A panic that crosses an extern "C" boundary
/// (e.g. libav calling ff::read_cb/seek_cb) aborts the process (SIGABRT) — by then the
/// message is gone, so capturing it here is the only way to see WHAT panicked. Pairs with
/// `src/crashtrace.c` — not `main.c`, which the tracer left in 2026-08-29 — whose re-raise buys SAM
/// a real `WIFSIGNALED` status and **nothing else**: `core_pattern` on this firmware is the bare
/// string `core` and `RLIMIT_CORE` is 0, so no core is written and no crashd report is ever
/// generated. `crashtrace.c` says so itself. Two deliberate SIGSEGVs produced the signal status and
/// an empty `/var/log/reports/librdx/`.
///
/// The line this hook writes is also the crash channel's PANIC input: `telemetry::crashreport`
/// reads the log on the next launch, hashes the message and sends the location only.
pub(super) fn install_panic_logger() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let loc = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "?".into());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic>".into());
        let cur = std::thread::current();
        let thread = cur.name().unwrap_or("?");
        let line = format!("*** RUST PANIC [{thread}] at {loc}: {msg}");
        log(&line);
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&crate::paths::in_runtime_dir("plxnative-crash.log"))
        {
            let _ = writeln!(f, "{line}");
        }
        default(info); // preserve default behaviour (stderr -> plxnative-stderr.log)
    }));
}

/// Hide the Magic Remote's on-screen pointer. A webOS-only concept: there is no such cursor to
/// hide on a desktop, and `SDL_webOSCursorVisibility` exists in no SDL but LG's fork.
///
/// One door rather than a branch at each of the five call sites, so the platform question is
/// asked once and the call sites read the same on both.
#[inline]
pub(super) unsafe fn hide_cursor() {
    #[cfg(not(feature = "hostsim"))]
    {
        SDL_webOSCursorVisibility(0);
    }
}
/// Advance the once-per-second LOOP-RATE window: bump `iters_ct` and, when a full second has
/// elapsed, recompute `loop_shown`, reset the window, and return `true` so the caller logs the
/// heartbeat with its own route/overlay tag. Shared by the player and home/detail draw paths.
///
/// This counts **loop iterations, not frames**. Since the present gate (`ui::idle`) landed the two
/// are different numbers, and conflating them is the single most reliable way to misread this app:
/// a settled screen runs the loop at the `IDLE_POLL_MS` rate while swapping nothing. The frame
/// count lives beside it in the heartbeat as `fps=`, from `ui::idle::take_presents`.
pub(super) fn loop_tick(iters_ct: &mut i32, loop_t: &mut u32, loop_shown: &mut i32, now: u32) -> bool {
    *iters_ct += 1;
    if now.wrapping_sub(*loop_t) < 1000 {
        return false;
    }
    *loop_shown = (*iters_ct as f32 * 1000.0 / now.wrapping_sub(*loop_t) as f32 + 0.5) as i32;
    *iters_ct = 0;
    *loop_t = now;
    true
}
/// How many times a finished `plxnative-playurl` playback may start itself AGAIN — the
/// `/tmp/plxnative-replay` trigger's content, as a number (LG App Self Checklist #46).
///
/// A named function rather than a closure at the one call site, for `note_global_press`'s reason
/// one step removed: the call site is inside the SDL event loop, which no host test can enter, and
/// this half touches no SDL at all. Splitting it puts the parsing under `make check` instead of
/// leaving it gradeable only by a television — which matters more here than it looks, because
/// EVERY value this returns is a plausible one and a misparse is invisible on the panel: 0 reads
/// as "the replay arm is missing from this binary" and 2 reads as a loop.
///
/// `None` (no file) is 0 — the one-shot behaviour every other boot has always had. An EMPTY file
/// is 1, which is the whole idiom of this trigger surface (`touch` it and get the obvious thing).
/// An explicit `0` is honoured, so a script can arm the file and turn it off without deleting it.
/// Anything unparseable is 1 rather than 0: this file is armed by hand, and answering a typo with
/// "silently do nothing" is how a green run comes to mean the opposite of what it says.
pub(super) fn replay_budget(raw: Option<&str>) -> u32 {
    match raw {
        None => 0,
        Some(s) => match s.trim() {
            "" => 1,
            t => t.parse::<u32>().unwrap_or(1),
        },
    }
}

#[cfg(test)]
mod replay_budget_tests {
    #[test]
    fn absent_is_one_shot_and_empty_is_one_replay() {
        assert_eq!(
            super::replay_budget(None),
            0,
            "no trigger must change nothing"
        );
        assert_eq!(super::replay_budget(Some("")), 1);
        assert_eq!(super::replay_budget(Some("  ")), 1);
    }

    #[test]
    fn a_number_is_honoured_including_a_deliberate_zero() {
        assert_eq!(super::replay_budget(Some("1")), 1);
        assert_eq!(super::replay_budget(Some("3")), 3);
        assert_eq!(super::replay_budget(Some(" 2 ")), 2);
        // An armed-but-off file, so a script can stop replaying without deleting the trigger.
        assert_eq!(super::replay_budget(Some("0")), 0);
    }

    #[test]
    fn a_typo_replays_once_rather_than_silently_doing_nothing() {
        // The file is armed by hand. Answering `-1` or `one` with 0 would make the case fail as
        // "the app never re-entered the player", i.e. as a missing feature rather than a typo.
        for bad in ["one", "-1", "1.5", "999999999999999999999"] {
            assert_eq!(super::replay_budget(Some(bad)), 1, "{bad}");
        }
    }
}

/// Resolve the server half of a direct dev-screen request.
///
/// An absent trigger preserves the historical `plxnative-play=<rk>` contract and uses the current
/// server. Once an explicit slot was written, however, failure is terminal: rating keys are local
/// to one PMS, so falling back could open a different item on another server.
pub(super) fn resolve_direct_server(
    requested: Option<Result<u16, String>>,
    current: crate::plex::ServerId,
    registered: impl Fn(crate::plex::ServerId) -> bool,
) -> Result<crate::plex::ServerId, String> {
    let Some(slot) = requested else {
        return Ok(current);
    };
    let raw = slot?;
    let sid = crate::plex::ServerId::from_raw(raw);
    registered(sid)
        .then_some(sid)
        .ok_or_else(|| format!("server slot {raw} is not registered"))
}

pub(super) fn direct_trigger_server() -> Result<crate::plex::ServerId, String> {
    resolve_direct_server(
        crate::dev::server_slot(),
        crate::plex::current_server(),
        |sid| crate::plex::client_for(sid).is_some(),
    )
}

// ---- boot, and the loop's own between-frame state ---------------------------------------------
/// Which screen the boot gate landed on — see the gate itself in `plex_run`, which is where the
/// order of its four cases is argued.
pub(super) enum BootTo {
    Home,
    Login,
    Profiles,
}
