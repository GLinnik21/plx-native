//! Boot-time helpers of the app core: the desktop window size, the panic logger, the replay
//! budget, the direct-server trigger, the cursor and the loop-rate tick. Moved out of `app.rs`
//! verbatim in phase 1a of the UI restructure (a pure move; `pub(crate)` widening only).

use super::*;

/// What a DESKTOP window is asked to be, before the display's pixel density is known.
#[cfg(any(feature = "hostsim", test))]
#[derive(Clone, Copy)]
enum WindowAsk {
    /// `PLXNATIVE_WIN=<w>x<h>`: a DRAWABLE of exactly this many pixels, on any display.
    Drawable(c_int, c_int),
    /// No override: the largest whole fraction of the canvas whose window fits this usable display
    /// area (`SDL_GetDisplayUsableBounds`, in points).
    Fit(c_int, c_int),
}

/// How many drawable pixels a window point is worth on this display, as the integer pair SDL
/// reported for a window that exists: `window_w` points wide, `drawable_w` pixels wide.
///
/// Integers, never a float multiplied back: 960 points of a 2x window must come out as exactly
/// 1920 pixels, and a ratio that is not a whole number (a 150% Windows display) must round the
/// same way on the way out as on the way in.
#[cfg(any(feature = "hostsim", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Density {
    window_w: c_int,
    drawable_w: c_int,
}

#[cfg(any(feature = "hostsim", test))]
impl Density {
    /// One pixel per point: every 1x display, and what is assumed until a window has been measured.
    const UNIT: Density = Density { window_w: 1, drawable_w: 1 };

    /// The measured pair. A drawable that is not larger than the window (a 1x display) is
    /// [`Density::UNIT`]: a drawable never has fewer pixels than points, and a window is never
    /// grown to make up for one that seems to.
    fn measured(window_w: c_int, drawable_w: c_int) -> Density {
        if window_w <= 0 || drawable_w <= window_w {
            Density::UNIT
        } else {
            Density { window_w, drawable_w }
        }
    }

    /// The window size, in points, whose drawable is `px` pixels: the nearest whole number of
    /// points, and one fewer if rounding up would make the drawable LARGER than asked (never
    /// larger than the canvas is the contract; a pixel short is a letterbox, a pixel over is not).
    fn points(self, px: c_int) -> c_int {
        let (w, d) = (i64::from(self.window_w), i64::from(self.drawable_w));
        let pt = ((i64::from(px) * w + d / 2) / d).max(1) as c_int;
        if pt > 1 && self.pixels(pt) > px {
            pt - 1
        } else {
            pt
        }
    }

    /// The drawable, in pixels, of a window `pt` points wide.
    fn pixels(self, pt: c_int) -> c_int {
        let (w, d) = (i64::from(self.window_w), i64::from(self.drawable_w));
        ((i64::from(pt) * d + w / 2) / w) as c_int
    }
}

/// `PLXNATIVE_WIN`'s value: `<w>x<h>`, both positive. Anything else is no override.
#[cfg(any(feature = "hostsim", test))]
fn parse_window_override(v: &str) -> Option<(c_int, c_int)> {
    let (w, h) = v.split_once(['x', 'X'])?;
    match (w.trim().parse::<c_int>(), h.trim().parse::<c_int>()) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => Some((w, h)),
        _ => None,
    }
}

/// The window a DESKTOP should open at, in points, for a display of this pixel `density`: the
/// pure arithmetic of the size, so the Retina cases are testable without a Retina display.
///
/// **The drawable is what is sized; the window follows from it.** The UI is authored at a 1920x1080
/// canvas and every glyph, icon mask and poster is rasterized for 1:1 texels against it
/// (`gfx::snap`, the crispness contract in `theme.rs`), so a drawable LARGER than the canvas is
/// not a sharper picture: it is the same atlas stretched, with every drawable-sized buffer (the
/// page snapshot, the glass sources) four times the television's, which is how a 2x display once
/// breached the render-set ceiling on every frame (`ui/src/frame/render_set.rs`).
///
/// * `Drawable` is that drawable, exactly. The fit below is chosen for a HUMAN looking at a
///   window, and a screenshot is not that: on a 1x display its divisor lands on 2 and every shot
///   comes back 960x540, half the canvas the UI is authored at, so a shot taken to JUDGE the
///   interface asks for full size (`make sim-shot SIM_W=1920 SIM_H=1080`). Off-screen edges are
///   fine for a headless grab: the drawable is the window's own framebuffer.
/// * `Fit` is **an exact divisor, never a best fit**: the largest of 1/1, 1/2 and 1/3 of the canvas
///   whose window, in points, fits the usable area. `surface::scale` will letterbox any drawable it
///   is given, so an arbitrary size would *work*, but a fractional scale resamples all of the above.
///   The bounds are in points, so the window is `canvas / div / density` of them.
///
/// The television asks for the canvas itself (1:1) unless the owner picked a smaller Interface
/// resolution in Settings on webOS 1-3 (`requested_drawable`), in which case the compositor fills the panel.
/// A Mac is the case the `surface` doc was written against — 1920x1080 exceeds the usable area of
/// every laptop display Apple ships, so asking for it flatly would put the title bar above the
/// screen and the bottom of the interface under the Dock.
#[cfg(any(feature = "hostsim", test))]
fn plan_desktop_window(ask: WindowAsk, density: Density) -> (c_int, c_int) {
    let points = |(w, h): (c_int, c_int)| (density.points(w), density.points(h));
    match ask {
        WindowAsk::Drawable(w, h) => points((w, h)),
        WindowAsk::Fit(uw, uh) => {
            // A little headroom under the usable bounds: a window flush against them reads as a
            // fullscreen that went wrong rather than as a deliberate size.
            let (max_w, max_h) = ((uw as f32 * 0.95) as c_int, (uh as f32 * 0.95) as c_int);
            let fits = |div| {
                let (w, h) = points((SCR_W / div, SCR_H / div));
                w <= max_w && h <= max_h
            };
            let div = (1..3).find(|&div| fits(div)).unwrap_or(3);
            points((SCR_W / div, SCR_H / div))
        }
    }
}

/// `SDL_WINDOWPOS_CENTERED` on display 0.
#[cfg(feature = "hostsim")]
const WINDOWPOS_CENTERED: c_int = 0x2FFF_0000u32 as c_int;

/// What this process is asked for: `PLXNATIVE_WIN` if it holds a size, else the fit to the display,
/// or the canvas itself if SDL cannot say how big the display is.
#[cfg(feature = "hostsim")]
fn desktop_window_ask() -> WindowAsk {
    if let Some((w, h)) = std::env::var_os("PLXNATIVE_WIN").and_then(|v| parse_window_override(&v.to_string_lossy())) {
        return WindowAsk::Drawable(w, h);
    }
    let mut r = [0 as c_int; 4]; // SDL_Rect: x, y, w, h
    let ok = unsafe { SDL_GetDisplayUsableBounds(0, r.as_mut_ptr()) } == 0 && r[2] > 0 && r[3] > 0;
    if ok {
        WindowAsk::Fit(r[2], r[3])
    } else {
        WindowAsk::Drawable(SCR_W, SCR_H)
    }
}

/// Size the just-created desktop `win` for the pixel density of the display it opened on.
///
/// `SDL_GetDisplayDPI` is unreliable on macOS, so the density is MEASURED instead: the window was
/// opened at the 1x answer of [`plan_desktop_window`], and `SDL_GL_GetDrawableSize` over
/// `SDL_GetWindowSize` says how many pixels that bought per point. On a 1x display nothing happens.
/// On a 2x display the window is resized (and put back in the middle of the display, since it was
/// centred at the old size) to the points that give the drawable that was asked for:
/// `PLXNATIVE_WIN=1920x1080` is a 960x540-point window, not a 1920x1080-point one with a 3840x2160
/// drawable. Called before `surface::probe`, which reads the drawable once and for good.
#[cfg(feature = "hostsim")]
unsafe fn fit_window_to_density(win: *mut c_void, ask: WindowAsk) {
    let ((ww, wh), (dw, dh)) = plx_base::surface::window_and_drawable_size(win);
    let density = Density::measured(ww, dw);
    if density == Density::UNIT {
        return;
    }
    let (pw, ph) = plan_desktop_window(ask, density);
    if (pw, ph) == (ww, wh) {
        return;
    }
    SDL_SetWindowSize(win, pw, ph);
    SDL_SetWindowPosition(win, WINDOWPOS_CENTERED, WINDOWPOS_CENTERED);
    let ((nw, nh), (gw, gh)) = plx_base::surface::window_and_drawable_size(win);
    log(&format!(
        "surface: {:.3} pixels per point (a {ww}x{wh} point window read back a {dw}x{dh} drawable); \
         resized to {pw}x{ph} points, which now reads {nw}x{nh} points / {gw}x{gh} drawable",
        f64::from(dw) / f64::from(ww)
    ));
}

#[cfg(test)]
mod desktop_window_tests {
    use super::WindowAsk::{Drawable, Fit};
    use super::{parse_window_override, plan_desktop_window, Density, WindowAsk, SCR_H, SCR_W};

    /// A window of 1920 points whose drawable reads back 3840 pixels: macOS Retina, the case this
    /// module exists for. (macOS always reports a whole backing factor, even in a "scaled" mode.)
    fn retina() -> Density {
        Density::measured(1920, 3840)
    }

    /// The window the planner opens, in points, and the drawable that window gives.
    fn plan(ask: WindowAsk, density: Density) -> ((i32, i32), (i32, i32)) {
        let points = plan_desktop_window(ask, density);
        (points, (density.pixels(points.0), density.pixels(points.1)))
    }

    /// `PLXNATIVE_WIN=1920x1080` on a 1x display is the 1920x1080 window it always was.
    #[test]
    fn a_1x_display_gives_the_override_as_asked() {
        assert_eq!(plan(Drawable(1920, 1080), Density::UNIT), ((1920, 1080), (1920, 1080)));
    }

    /// THE BUG: `PLXNATIVE_WIN=1920x1080` on a 2x display was a 1920x1080-POINT window, whose
    /// drawable is 3840x2160 (`scale=2.000`) and which breached the render-set ceiling on every
    /// frame of a Library scroll. The override is the DRAWABLE, so the window is half of it.
    #[test]
    fn the_override_is_the_drawable_on_a_2x_display() {
        assert_eq!(plan(Drawable(1920, 1080), retina()), ((960, 540), (1920, 1080)));
    }

    /// A 2x laptop (usable ~1512x945 points): 960x540 points, a 1:1 drawable. What it always got,
    /// by the coincidence of div=2 meeting a 2x density; now by arithmetic.
    #[test]
    fn a_2x_laptop_fits_960x540_points_for_a_1_to_1_drawable() {
        assert_eq!(plan(Fit(1512, 945), retina()), ((960, 540), (1920, 1080)));
    }

    /// THE SECOND HOLE: a 5K/4K display "looking like 2560x1440" has usable ~2560x1415 points, so
    /// the fit in points took div=1 -- a 1920x1080-point window, a 3840x2160 drawable -- with no
    /// override at all. The fit is of the DRAWABLE, measured in the points it occupies.
    #[test]
    fn a_large_2x_display_gives_the_same_1_to_1_drawable_without_any_override() {
        assert_eq!(plan(Fit(2560, 1415), retina()), ((960, 540), (1920, 1080)));
    }

    /// Nothing changes on a 1x display: the divisor lands where it did, on every size.
    #[test]
    fn a_1x_display_fits_exactly_as_it_did() {
        // a 1080p display's usable area under a menu bar and a Dock: div=2
        assert_eq!(plan(Fit(1920, 1050), Density::UNIT), ((960, 540), (960, 540)));
        // a display big enough for the canvas itself (div=1) needs 5% headroom: 1920 / 0.95 = 2021.05
        assert_eq!(plan(Fit(2022, 1140), Density::UNIT), ((1920, 1080), (1920, 1080)));
        assert_eq!(plan(Fit(2020, 1140), Density::UNIT), ((960, 540), (960, 540)));
        // a small one: div=3
        assert_eq!(plan(Fit(100, 100), Density::UNIT), ((640, 360), (640, 360)));
    }

    /// A density that is not a whole number (a 150% Windows display, a fractional Wayland scale):
    /// the window is the drawable over the ratio, exactly where that is exact.
    #[test]
    fn a_fractional_density_is_divided_out_too() {
        let d150 = Density::measured(1280, 1920);
        assert_eq!(plan(Drawable(1920, 1080), d150), ((1280, 720), (1920, 1080)));
        // 1280x720 points is over 95% of a 1280x800 display, so the half (960x540 px) wins
        assert_eq!(plan(Fit(1280, 800), d150), ((640, 360), (960, 540)));
    }

    /// Whatever the ratio, the window never gives a drawable LARGER than the one asked for: a
    /// pixel short is a letterbox, a pixel over is a texture that is not the canvas.
    #[test]
    fn the_drawable_is_never_larger_than_asked_on_any_density() {
        for (window_w, drawable_w) in [(1000, 1100), (1000, 1250), (1000, 1333), (1000, 1750), (1000, 2000), (1000, 2500), (1000, 3000), (1920, 2880), (1366, 2049)] {
            let d = Density::measured(window_w, drawable_w);
            for (w, h) in [(1920, 1080), (960, 540), (640, 360), (1280, 720)] {
                let (_, got) = plan(Drawable(w, h), d);
                assert!(got.0 <= w && got.1 <= h, "{window_w}->{drawable_w} asked {w}x{h}: {got:?}");
                // and no more than a point short of it, in either axis
                let point = (drawable_w + window_w - 1) / window_w;
                assert!(w - got.0 < point && h - got.1 < point, "{window_w}->{drawable_w} asked {w}x{h}: {got:?}");
            }
            for (uw, uh) in [(1280, 720), (1512, 945), (1920, 1050), (2560, 1415), (3840, 2100)] {
                let (_, got) = plan(Fit(uw, uh), d);
                assert!(got.0 <= SCR_W && got.1 <= SCR_H, "{window_w}->{drawable_w} usable {uw}x{uh}: {got:?}");
            }
        }
    }

    /// A drawable that is not larger than the window is no HiDPI at all, however it got that way:
    /// a window is never grown to cover for a measurement that failed.
    #[test]
    fn a_drawable_no_larger_than_the_window_is_unit_density() {
        assert_eq!(Density::measured(1920, 1920), Density::UNIT);
        assert_eq!(Density::measured(0, 1920), Density::UNIT);
        assert_eq!(Density::measured(1920, 960), Density::UNIT);
    }

    #[test]
    fn the_override_parses_a_positive_size_and_nothing_else() {
        assert_eq!(parse_window_override("1920x1080"), Some((1920, 1080)));
        assert_eq!(parse_window_override(" 640X360 "), Some((640, 360)));
        assert_eq!(parse_window_override("1920 x 1080"), Some((1920, 1080)));
        for bad in ["", "1920", "x", "0x1080", "1920x0", "-1x5", "axb", "1920x1080x2"] {
            assert_eq!(parse_window_override(bad), None, "{bad:?}");
        }
    }
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
/// The record this hook writes (the panic line and its `time:` line) is also the crash channel's
/// PANIC input: `telemetry::crashreport` reads the log on the next launch, hashes the message and
/// sends the location and the bounded crash time, never the message.
pub(crate) fn install_panic_logger() {
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
        let _ = record_panic_in_crash_log(
            &plx_base::paths::in_runtime_dir(plx_base::paths::runtime_file::CRASH),
            &line,
            plx_telemetry::telemetry::crashreport::now_secs(),
        );
        default(info); // preserve default behaviour (stderr -> plxnative-stderr.log)
    }));
}

/// Append the panic's record to the crash log at `path` (`plxnative-crash.log`, which `src/main.c`
/// opens and the signal tracer appends to).
///
/// **Two disciplines, both the event log's** (the crash log is group-readable for the same Dev Mode
/// reason, `eventlog::LOG_MODE`). The text passes [`plx_base::eventlog::scrub::scrub_local`]: a panic
/// message is whatever the failing call was formatting, and the raw line used to go to this file
/// while only the event-log copy ([`log`]) was redacted. And the open is
/// [`plx_base::eventlog::open_log_append`]'s (`O_NOFOLLOW`, a regular file this uid owns with ONE
/// link, 0640, never truncated) rather than a bare `OpenOptions` at the umask's mode, so a name
/// planted in the shared `/tmp` is refused instead of written through. `Err` is for the hook to
/// ignore: a panic hook has nowhere to report a failure to.
///
/// **The record is two lines in ONE write:** the panic line and the `time: <unix seconds>` line the
/// C tracer also writes under a signal record ([`plx_telemetry::telemetry::crashreport::crash_time_record`]
/// is the one spelling). The crash importer sends it as the event's `timestamp`, so the report says
/// when the panic happened rather than when the next launch found it. `0` (a clock that reads
/// nothing) writes no time line.
fn record_panic_in_crash_log(path: &std::path::Path, line: &str, unix_secs: u64) -> std::io::Result<()> {
    let mut record = plx_base::eventlog::scrub::scrub_local(line);
    if unix_secs > 0 {
        record.push('\n');
        record.push_str(&plx_telemetry::telemetry::crashreport::crash_time_record(unix_secs));
    }
    plx_base::eventlog::append_record(path, &record)
}

#[cfg(test)]
mod panic_log_tests {
    use super::record_panic_in_crash_log;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

    const LINE: &str = "*** RUST PANIC [demux] at src/ff.rs:1204: bad url https://192-168-1-50.0123456789abcdef0123456789abcdef.plex.direct:32400/x?X-Plex-Token=SECRETTOKEN";

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("plx-panic-log-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    /// The crash log is group-readable (a Dev Mode user fetches it), so the panic text a hook writes
    /// to it passes the same redaction every event-log line does: a panic message can embed whatever
    /// the failing call was formatting, including a URL.
    #[test]
    fn the_panic_text_is_scrubbed_like_every_event_log_line() {
        let dir = scratch("scrub");
        let path = dir.join("crash.log");
        record_panic_in_crash_log(&path, LINE, 1_750_000_000).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(!written.contains("SECRETTOKEN"), "{written}");
        assert!(!written.contains("192-168-1-50") && !written.contains("plex.direct"), "{written}");
        assert!(written.starts_with("*** RUST PANIC [demux] at src/ff.rs:1204: "), "the record's shape survives: {written}");
        assert!(
            written.ends_with("\ntime: 1750000000\n") && written.matches('\n').count() == 2,
            "the panic line, then its time line, newline-terminated: {written}"
        );
        // And the crash report parser still reads the scrubbed record, with the time it was given.
        let reports = plx_telemetry::telemetry::crashreport::parse(&written);
        assert_eq!(reports.len(), 1, "{written}");
        assert!(
            matches!(&reports[0], plx_telemetry::telemetry::crashreport::Report::Panic(p) if p.crash_time == Some(1_750_000_000)),
            "{reports:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same open discipline as the event log's sink ([`plx_base::eventlog::open_log_append`]):
    /// 0640 even under a restrictive umask, appended to, never truncated.
    #[test]
    fn the_crash_log_is_0640_and_appended_to() {
        let dir = scratch("mode");
        let path = dir.join("crash.log");
        std::fs::write(&path, b"*** SIGNAL 11 earlier\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        record_panic_in_crash_log(&path, "*** RUST PANIC [t] at src/a.rs:1: boom", 1_750_000_000).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o7777, 0o640);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "*** SIGNAL 11 earlier\n*** RUST PANIC [t] at src/a.rs:1: boom\ntime: 1750000000\n"
        );
        let fresh = dir.join("fresh.log");
        record_panic_in_crash_log(&fresh, "*** RUST PANIC [t] at src/a.rs:1: boom", 1_750_000_000).unwrap();
        assert_eq!(std::fs::metadata(&fresh).unwrap().mode() & 0o7777, 0o640);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A planted symlink or a second hard link at the crash log's name is somebody else's file:
    /// refused, and it keeps its content and mode.
    #[test]
    fn a_symlinked_or_hard_linked_crash_log_is_refused_and_the_other_file_is_untouched() {
        let dir = scratch("refuse");
        let victim = dir.join("auth.json");
        std::fs::write(&victim, b"session").unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o600)).unwrap();

        let linked = dir.join("symlinked.log");
        symlink(&victim, &linked).unwrap();
        assert!(record_panic_in_crash_log(&linked, LINE, 1_750_000_000).is_err());

        let hard = dir.join("hardlinked.log");
        std::fs::hard_link(&victim, &hard).unwrap();
        assert!(record_panic_in_crash_log(&hard, LINE, 1_750_000_000).is_err());

        let meta = std::fs::metadata(&victim).unwrap();
        assert_eq!(meta.mode() & 0o7777, 0o600, "never widened");
        assert_eq!(std::fs::read(&victim).unwrap(), b"session", "never written to");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// **Hand `plex` the two answers it cannot name.** The Plex layer sits beneath `route` and
/// `telemetry`, so what it needs from them reaches it as hooks installed here, once, first thing in
/// [`enter_application`] — before the controlled/replay split and before anything can load the
/// session:
///
/// * `plex::session::install_auto_quality_ready` — `route::auto_quality_ready`, the gate a fresh
///   session record's default quality is seeded from (unset, `plex` reads `true`, the gate's value);
/// * `plex::session::install_account_clear_cleanup` — `telemetry::cleanup_after_account_clear`, the
///   telemetry/consent legacy-file sweep a durable sign-out runs. It exists only where the sweep does
///   (ARM, not the simulator, not a test build); unset, `plex` reads it as already retired.
pub(crate) fn install_plex_seams() {
    plx_plex::plex::session::install_auto_quality_ready(plx_media::route::auto_quality_ready);
    #[cfg(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test)))]
    plx_plex::plex::session::install_account_clear_cleanup(plx_telemetry::telemetry::cleanup_after_account_clear);
}

#[cfg(test)]
mod relaunch_tap_tests {
    use super::slot_holds;

    #[test]
    fn relaunch_slot_must_hold_libhelpers() {
        assert!(slot_holds(0x1000, Some(0x1000)), "the slot holds libhelpers' function");
        assert!(!slot_holds(0x1000, Some(0x1001)), "a different value, even by the Thumb bit");
        assert!(!slot_holds(0x1000, None), "libhelpers is not mapped or lacks the symbol");
        assert!(!slot_holds(0, None));
    }

    #[test]
    fn the_slot_must_be_word_aligned() {
        use super::slot_aligned;
        let word = core::mem::align_of::<usize>();
        assert!(slot_aligned(0x1000) && slot_aligned(word));
        assert!(!slot_aligned(0x1000 + 1) && !slot_aligned(word + word / 2));
    }
}

#[cfg(test)]
mod seam_order_tests {
    /// `plx_plex` reports the version it is handed (`plex::identity::set_version`) and, in a
    /// shipping build, panics when asked for it earlier, so the hand-in has to be the first thing
    /// the one entry point does: nothing before it can build a header or a `User-Agent`. It also
    /// has to hand in `PLX_VERSION` itself, the string `ci/check-package.py` grades in the binary
    /// (as `plxnative@` + version, see the telemetry hand-in below), and no second production site
    /// may hand in anything else. Reads the source, like the tests
    /// below, because no host test can run `enter_application`.
    #[test]
    fn the_plex_version_is_handed_in_first_and_only_there() {
        let hand_in = concat!("plx_plex::plex::identity::", "set_version(env!(\"PLX_VERSION\"));");
        let source = include_str!("mod.rs");
        let body = source
            .split_once("fn enter_application(")
            .expect("enter_application")
            .1
            .split_once('{')
            .expect("enter_application's body")
            .1;
        let first = body
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("//"))
            .expect("a first statement");
        assert_eq!(first, hand_in, "set_version must be enter_application's first statement");
        // `plx_telemetry` reports the release it is handed the same way (and also panics when asked
        // earlier in a shipping build). It is composed here with `concat!`, not by the telemetry
        // crate, because `ci/check-package.py` greps the packaged binary for that exact string.
        let release = concat!("plx_telemetry::telemetry::", "set_release(concat!(\"plxnative@\", env!(\"PLX_VERSION\")));");
        let second = body
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("//"))
            .nth(1)
            .expect("a second statement");
        assert_eq!(second, release, "set_release must be enter_application's second statement");
        // The screens crate cannot see `PLX_BUILD_SHA` either (About and Source print it), and in a
        // shipping build panics when asked before it is handed in: third, one site.
        let sha = concat!("plx_screens::legal::", "set_build_sha(env!(\"PLX_BUILD_SHA\"));");
        let third = body
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("//"))
            .nth(2)
            .expect("a third statement");
        assert_eq!(third, sha, "set_build_sha must be enter_application's third statement");
        assert_eq!(
            source.matches(concat!("legal::", "set_build_sha(")).count(),
            1,
            "one production hand-in of the build's commit"
        );
        assert_eq!(
            source.matches(concat!("telemetry::", "set_release(")).count(),
            1,
            "one production hand-in of the release"
        );
        assert_eq!(
            source.matches(concat!("identity::", "set_version(")).count(),
            1,
            "one production hand-in; a second could name a different version"
        );
        assert_eq!(
            source.matches(concat!("enter_application(", "pms_host,pms_port)")).count(),
            1,
            "enter_application has one caller, run_application"
        );
    }

    /// `enter_application` installs the player's error-trace eraser before telemetry first loads a
    /// decision (`pre_boot_diagnostics` -> `telemetry::boot`, which already erases through it) and
    /// before anything can play. It was installed by `player::report::requested`, which a
    /// detail-page preview never passes, so a failed preview's trace outlived a withdrawal. This
    /// reads the source, like the diagnostics boot-order test below, because no host test can
    /// run `enter_application`; what the eraser reaches is graded by `player::report`'s
    /// `a_failed_preview_trace_is_erased_through_telemetrys_hook`.
    #[test]
    fn the_trace_eraser_is_installed_before_telemetry_loads_or_anything_plays() {
        let source = include_str!("mod.rs");
        let body = source
            .split_once("fn enter_application(")
            .expect("enter_application")
            .1
            .split_once("/// The ten-line public skeleton")
            .expect("end of enter_application")
            .0;
        let eraser = body
            .find("plx_media::player::report::install_trace_eraser();")
            .expect("enter_application must install telemetry's error-trace eraser");
        let telemetry = body
            .find(".then(pre_boot_diagnostics)")
            .expect("telemetry's boot load");
        let boot = body.find("boot(pms_host").expect("application boot");
        assert!(
            eraser < telemetry && eraser < boot,
            "the eraser must be installed before telemetry loads and before anything can play"
        );
    }

    /// The storage diagnostics worker probes `app_dir()` on its own thread, so it starts only after the
    /// live identity preamble has written the first two event-log lines. Reads the source like the
    /// test above, for the same reason: no host test can run `enter_application`. It lived in
    /// `plx_platform::storage::diagnostics` until that crate stopped being able to see this file.
    #[test]
    fn worker_start_follows_the_live_boot_identity_preamble() {
        let source = include_str!("mod.rs");
        assert_eq!(
            source
                .matches("plx_platform::storage::diagnostics::start(")
                .count(),
            1,
            "every start site must stay behind the identity preamble"
        );
        let pre_boot = source
            .split_once("fn pre_boot_diagnostics")
            .expect("pre_boot_diagnostics")
            .1
            .split_once("unsafe fn run_and_shutdown")
            .expect("end of pre_boot_diagnostics")
            .0;
        let install = pre_boot.find("\"install: id=").expect("install line");
        let app_dir = pre_boot
            .find("plx_base::paths::app_dir_line()")
            .expect("appdir line");
        assert!(install < app_dir);
        let body = source
            .split_once("fn enter_application")
            .expect("enter_application")
            .1
            .split_once("/// The ten-line public skeleton")
            .expect("end of enter_application")
            .0;
        let identity = body
            .find(".then(pre_boot_diagnostics)")
            .expect("identity preamble");
        let diagnostics = body
            .find("plx_platform::storage::diagnostics::start(")
            .expect("diagnostics start");
        let boot = body.find("boot(pms_host").expect("application boot");
        assert!(identity < diagnostics && diagnostics < boot);
    }

    /// `construct` hands the transport its `User-Agent` before `net::global_init` makes HTTPS
    /// available. `net` reads it only once a request has passed `available()`, so with this order
    /// no request can leave without one; the other order sends the first ones bare.
    #[test]
    fn the_user_agent_is_installed_before_libcurl_is_made_available() {
        // The needles are split with `concat!` so this test's own text, which `include_str!`
        // reads too and which sits above `construct`, can never be what they match.
        let source = include_str!("boot.rs");
        let body = source
            .split_once(concat!("pub(crate) unsafe fn ", "construct("))
            .expect("construct")
            .1;
        let user_agent = body
            .find(concat!("plx_net::net::", "set_user_agent("))
            .expect("construct must install the User-Agent");
        let init = body.find(concat!("plx_net::net::", "global_init()")).expect("construct's libcurl init");
        assert!(user_agent < init, "set_user_agent must come before net::global_init");
    }

    /// SDL reads `SDL_WEBOS_CURSOR_SLEEP_TIME` in `SDL_Init`, so the hint set after it does nothing.
    #[test]
    fn the_cursor_sleep_hint_is_set_before_sdl_init() {
        // Split with `concat!` for the reason the test above gives.
        let source = include_str!("boot.rs");
        let body = source
            .split_once(concat!("pub(crate) unsafe fn ", "construct("))
            .expect("construct")
            .1;
        let hint = body
            .find(concat!("webos3_cursor_", "sleep_hint()"))
            .expect("construct must set the cursor sleep hint");
        let init = body.find(concat!("SDL_Init(", "SDL_INIT_VIDEO)")).expect("construct's SDL_Init");
        assert!(hint < init, "the cursor sleep hint must come before SDL_Init");
    }

    /// The Interface resolution is read from the stored session, so the session has to be read
    /// BEFORE `SDL_CreateWindow` (the one place the window size is applied) and the window request
    /// has to come from it. Reads the source for the reason the tests above give.
    #[test]
    fn the_stored_session_is_read_before_the_window_is_created() {
        let source = include_str!("boot.rs");
        let body = source
            .split_once(concat!("pub(crate) unsafe fn ", "construct("))
            .expect("construct")
            .1;
        let load = body
            .find(concat!("plx_plex::plex::session::", "load()"))
            .expect("construct reads the stored session");
        let request = body
            .find(concat!("requested_drawable(", "&session)"))
            .expect("the window request is derived from the session");
        let window = body.find(concat!("SDL_CreateWindow(", "title")).expect("construct creates the window");
        assert!(load < request && request < window, "session read, then request, then SDL_CreateWindow");
    }

    /// Automatic is the native canvas on every set and asks for no smaller window. The other
    /// options ask for exactly their size.
    #[test]
    fn the_setting_maps_to_the_requested_window() {
        use plx_plex::plex::session::UiResolution as R;
        for major in [1, 2, 3] {
            assert_eq!(super::drawable_for_setting(major, R::Auto), None);
            assert_eq!(super::drawable_for_setting(major, R::Hd900), Some((1600, 900)));
            assert_eq!(super::drawable_for_setting(major, R::Sd540), Some((960, 540)));
        }
        assert_eq!(super::drawable_for_setting(3, R::Hd720), Some((1280, 720)));
        // webOS 4+, or a set that never said, keeps the canvas whatever a record carried over holds
        for major in [0, 4, 5, 6, 22] {
            for mode in R::LADDER {
                assert_eq!(super::drawable_for_setting(major, mode), None, "major {major} {mode:?}");
            }
        }
        // a record that predates the setting, or holds a spelling this build does not know, is Automatic
        let old: plx_plex::plex::session::Session = serde_json::from_str(r#"{"client_id":"c"}"#).unwrap();
        assert_eq!(super::drawable_for_setting(3, old.ui_resolution()), None);
        let unknown: plx_plex::plex::session::Session =
            serde_json::from_str(r#"{"client_id":"c","ui_resolution":"4320p"}"#).unwrap();
        assert_eq!(super::drawable_for_setting(3, unknown.ui_resolution()), None);
        // the retired explicit 1080p reads as Automatic
        let retired: plx_plex::plex::session::Session =
            serde_json::from_str(r#"{"client_id":"c","ui_resolution":"1080p"}"#).unwrap();
        assert_eq!(super::drawable_for_setting(3, retired.ui_resolution()), None);
    }

    /// A persisted size the set will not give must never brick launch: a refused smaller window is
    /// retried ONCE at the native canvas, and a refused native window has nowhere left to fall.
    #[test]
    fn a_refused_smaller_window_retries_once_at_the_native_canvas() {
        assert_eq!(super::window_fallback((1600, 900)), Some((super::SCR_W, super::SCR_H)));
        assert_eq!(super::window_fallback((960, 540)), Some((super::SCR_W, super::SCR_H)));
        assert_eq!(super::window_fallback((super::SCR_W, super::SCR_H)), None, "the native canvas has no fallback");
    }

    /// `surface::probe` reads the drawable once, at boot, for good: the desktop window has to be
    /// sized for the display's pixel density BEFORE it, or a 2x display keeps the 3840x2160
    /// drawable the 1x size gave it. Reads the source for the reason the tests above give.
    #[test]
    fn the_desktop_window_is_fitted_to_the_density_before_the_drawable_is_probed() {
        let source = include_str!("boot.rs");
        let body = source.split_once(concat!("pub(crate) unsafe fn ", "construct(")).expect("construct").1;
        let create = body.find(concat!("SDL_CreateWindow(", "title")).expect("creates the window");
        let fit = body.find(concat!("fit_window_to_", "density(win")).expect("fits the window to the density");
        let probe = body.find(concat!("surface::", "probe(win)")).expect("probes the drawable");
        assert!(create < fit && fit < probe, "create the window, fit it to the density, then probe the drawable");
    }

    /// `construct` frees a half-made window before it retries (a leaked one would keep the display).
    #[test]
    fn construct_destroys_a_half_made_window_before_the_retry() {
        let source = include_str!("boot.rs");
        let body = source.split_once(concat!("pub(crate) unsafe fn ", "construct(")).expect("construct").1;
        let create = body.find(concat!("SDL_CreateWindow(", "title")).expect("creates the window");
        let destroy = body.find(concat!("SDL_DestroyWindow(", "win)")).expect("destroys a half-made window");
        let retry = body.find(concat!("window_fallback(", "(ww_req")).expect("asks for the fallback size");
        assert!(create < destroy && destroy < body[create..].find("probe(win)").map(|i| i + create).unwrap(),
            "the half-made window is destroyed inside the creation loop, before `surface::probe`");
        assert!(retry < create, "the retry is armed before the first attempt");
    }
}

/// Hide the Magic Remote's on-screen pointer. A webOS-only concept: there is no such cursor to
/// hide on a desktop, and `SDL_webOSCursorVisibility` exists in no SDL but LG's fork.
///
/// One door rather than a branch at each caller, so the platform question is asked once
/// and the callers read the same on LG's extension and on the transparent-cursor path.
///
/// Resolved at runtime rather than linked: webOS 3.4 ships SDL 2.0.3, which has none of LG's
/// `SDL_webOS*` extensions, and a strong reference there is a lazily-bound PLT slot that kills
/// the process on the first D-pad press (`tools/fwcompat.py --release 3.4.0`).
///
/// **Without LG's extension the pointer is SDL's own cursor, and it is swapped for a transparent
/// one rather than switched off.** webOS 3.9.3 ships SDL 2.0.4 with no `SDL_webOS*` export at all,
/// and that SDL draws the pointer itself (its `.rodata` carries `/usr/share/im/cursorType*.png`
/// and `wl_starfish_pointer`). With nothing to call, every hide was a no-op and the pointer stayed
/// up until LG's ~5-minute idle timeout, issue #642. `SDL_ShowCursor(SDL_DISABLE)` does hide it
/// there, but measured on that set it also stops pointer motion reaching the app until the pointer
/// re-enters the window (opening and closing the Settings overlay), so no motion could bring it
/// back. With a transparent cursor the motion keeps arriving, and [`show_cursor_after_motion`] puts
/// the previous cursor back on the next one. Every SDL call is dlsym'd, so the binary gains no
/// import.
#[inline]
pub(crate) unsafe fn hide_cursor() {
    #[cfg(not(feature = "hostsim"))]
    {
        hide_through(cursor_door(), &SHOWN_CURSOR);
    }
}

/// True where the pointer is hidden by swapping in a transparent cursor rather than by LG's call,
/// so the system's pointer stays up and the remote's OK arrives as a click (`input::click_is_ok`).
/// False on the host simulator, where a click is always a real one.
pub(crate) unsafe fn cursor_is_transparent() -> bool {
    #[cfg(not(feature = "hostsim"))]
    {
        matches!(cursor_door(), CursorDoor::Sdl { .. })
    }
    #[cfg(feature = "hostsim")]
    {
        false
    }
}

/// Bring the pointer back after [`hide_cursor`] swapped in the transparent cursor. Call on pointer
/// motion while the cursor is hidden. A no-op wherever LG's extension exists: there the
/// compositor shows its own pointer on motion, as it always has.
#[inline]
pub(crate) unsafe fn show_cursor_after_motion() {
    #[cfg(not(feature = "hostsim"))]
    {
        show_through(cursor_door(), &SHOWN_CURSOR);
    }
}

/// [`hide_cursor`] on a given door, the cursor it replaced kept in `shown`. Split out so the
/// bookkeeping is gradeable on the host with a door of fake functions.
#[cfg(not(feature = "hostsim"))]
unsafe fn hide_through(door: CursorDoor, shown: &std::sync::atomic::AtomicUsize) {
    match door {
        CursorDoor::Lg(f) => {
            f(0);
        }
        CursorDoor::Sdl { set, get, def, blank } => {
            let cur = get() as usize;
            if cur != blank {
                let keep = if cur != 0 { cur } else { def() as usize };
                if keep == 0 {
                    return; // nothing to put back, so the pointer would never return: leave it
                }
                shown.store(keep, std::sync::atomic::Ordering::Relaxed);
            }
            set(blank as *mut std::ffi::c_void);
        }
        CursorDoor::None => {}
    }
}

/// [`show_cursor_after_motion`] on a given door.
#[cfg(not(feature = "hostsim"))]
unsafe fn show_through(door: CursorDoor, shown: &std::sync::atomic::AtomicUsize) {
    if let CursorDoor::Sdl { set, .. } = door {
        let shown = shown.load(std::sync::atomic::Ordering::Relaxed);
        if shown != 0 {
            set(shown as *mut std::ffi::c_void);
        }
    }
}
/// The window to ask for when it is not the 1920x1080 canvas, `None` for the canvas (1:1): whatever
/// `/tmp/plxnative-drawable=<W>x<H>` says when that trigger is armed (debug builds only; it
/// outranks the setting and is bounded to 640x360..=1920x1080 so a typo cannot ask for nonsense),
/// else, on webOS 1-3 only, the install-wide Settings > System > Interface resolution choice.
/// **Automatic is the canvas on every set**, and webOS 4+ is always the canvas; a smaller window is
/// only ever a webOS 1-3 owner's pick (or the debug trigger).
///
/// What was measured, with the heartbeat framedrop A/B (not `profile-tv` counters, so no cause is
/// claimed): on a 2016 55UH650V (webOS 3.4.3, Mali-T760) at 1280x720, average 46.9 -> 54.5 fps and
/// frames over 33 ms from 7.1% to 0.5%; on a 2017 OLED C7V (webOS 3.9.3, Mali-T820) frames over
/// 16.7 ms from 36% at 1080p to 2% at 1280x720 and 16% at 1600x900. The same owner found 720p text
/// "disturbingly soft" on the C7V, which is why this is opt-in rather than a webOS-release rule.
/// `surface::probe` reads back what webOS granted and `scale`/`viewport` fit the canvas into it.
#[cfg(not(feature = "hostsim"))]
fn requested_drawable(session: &plx_plex::plex::session::Session) -> Option<(c_int, c_int)> {
    if let Some(v) = plx_base::devtrig::read("drawable") {
        let parsed = v.trim().split_once('x').and_then(|(w, h)| {
            Some((w.trim().parse::<c_int>().ok()?, h.trim().parse::<c_int>().ok()?))
        });
        let ok = parsed.filter(|&(w, h)| (640..=SCR_W).contains(&w) && (360..=SCR_H).contains(&h));
        log(&format!("surface: drawable trigger {:?} -> {:?}", v.trim(), ok));
        if ok.is_some() {
            return ok;
        }
    }
    let chosen = session.ui_resolution();
    let major = plx_platform::tv::device::info().major;
    let size = drawable_for_setting(major, chosen);
    log(&format!("surface: webOS major {major}, Interface resolution {chosen:?} -> {size:?}"));
    size
}

/// Where to retry when the set refuses a window of size `asked`: the native canvas, unless that is
/// what was asked, which has nowhere left to fall back to.
#[cfg(any(not(feature = "hostsim"), test))]
fn window_fallback(asked: (c_int, c_int)) -> Option<(c_int, c_int)> {
    (asked != (SCR_W, SCR_H)).then_some((SCR_W, SCR_H))
}

/// The window an Interface resolution choice asks for on firmware `major`: `None` for Automatic,
/// the native canvas, and `None` on every set the setting is not offered on (webOS 4+ or unknown,
/// [`plx_platform::tv::device::offers_ui_resolution`]) whatever is stored, so a record carried over
/// from another set cannot shrink the interface where the owner has no way to change it back.
#[cfg(any(not(feature = "hostsim"), test))]
fn drawable_for_setting(major: u32, chosen: plx_plex::plex::session::UiResolution) -> Option<(c_int, c_int)> {
    if plx_platform::tv::device::offers_ui_resolution(major) { chosen.size() } else { None }
}


/// **webOS 3.x: shorten LG's own idle hide from about five minutes to five seconds.** Without
/// LG's extension, LG's SDL hides its pointer by itself after `SDL_WEBOS_CURSOR_SLEEP_TIME` of
/// stillness, and its default is about five minutes (timed on a 3.9.3 set, issue #642), where
/// every other app on that television hides it within seconds. Measured on the same set: the value
/// is milliseconds, `5000` hides a still pointer after about five seconds, and a deliberate
/// movement brings it back. Set only where `SDL_webOSCursorVisibility` is absent, so webOS 4+
/// keeps its SDL's own behaviour. Must run before `SDL_Init`, which reads the hint.
#[cfg(not(feature = "hostsim"))]
unsafe fn webos3_cursor_sleep_hint() {
    if let Some(ms) = cursor_sleep_hint(self_sym("SDL_webOSCursorVisibility").is_some()) {
        SDL_SetHint(c"SDL_WEBOS_CURSOR_SLEEP_TIME".as_ptr(), ms.as_ptr());
        log("cursor: SDL_WEBOS_CURSOR_SLEEP_TIME=5000 (no SDL_webOS* in this SDL)");
    }
}
#[cfg(feature = "hostsim")]
unsafe fn webos3_cursor_sleep_hint() {}

/// The idle-hide time to ask SDL for, in milliseconds as a string; `None` where LG's extension is
/// present and its SDL keeps its own behaviour.
#[cfg(not(feature = "hostsim"))]
fn cursor_sleep_hint(has_lg_extension: bool) -> Option<&'static std::ffi::CStr> {
    (!has_lg_extension).then_some(c"5000")
}

/// A symbol of the running process (the system SDL), resolved at runtime; `None` if absent or NULL.
#[cfg(not(feature = "hostsim"))]
fn self_sym(name: &str) -> Option<*mut std::ffi::c_void> {
    plx_base::dynlib::Handle::self_handle().sym(name).filter(|p| !p.is_null())
}

/// **A warm launch's parameters, read where LG's SDL reads its `registerApp` replies.** SAM sends a
/// relaunch's parameters only to the `registerApp` subscriber, which is LG's SDL; its handler
/// (`callback_RegistNativeApp`) raises the window and never reads `parameters`. It fetches every
/// reply's payload through `HELPER_HLunaServiceMessage`, an exported pointer slot that
/// `LoadWEBOSBASELibrary` fills from `libhelpers.so.2` inside `SDL_Init` and that the handler
/// reloads on every call. Wrapping that slot hands a relaunch payload to `tv::launch` and returns
/// it unchanged, so SDL still raises the window. No new Luna registration: SAM refuses the app's
/// own (`tv::launch`). The firmware inventories have the slot from webOS 7.4 on; where it is missing
/// or empty (webOS 4.x to 6.x), nothing changes and callers keep closing the app first.
/// The tap installs only when the slot holds the address `libhelpers.so.2` (already mapped,
/// `RTLD_NOLOAD`) exports as `HLunaServiceMessage` ([`slot_holds`]); any other value is left alone.
#[cfg(not(feature = "hostsim"))]
unsafe fn tap_relaunch_payloads() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let Some(slot) = self_sym("HELPER_HLunaServiceMessage") else {
        log("relaunch tap: no HELPER_HLunaServiceMessage in this SDL");
        return;
    };
    // An atomic reference to a misaligned address is undefined behaviour: refuse a symbol that is
    // not a word-aligned slot before forming it.
    if !slot_aligned(slot as usize) {
        log("relaunch tap: slot misaligned \u{2014} not installed");
        return;
    }
    // SDL's Luna thread may read the slot while it is replaced: store it atomically, after the
    // function it forwards to is published.
    let slot = &*(slot as *const AtomicUsize);
    let wrapped = wrapped_message as *const () as usize;
    let current = slot.load(Ordering::Acquire);
    if current == 0 || current == wrapped {
        log(if current == 0 { "relaunch tap: HELPER_HLunaServiceMessage is empty" }
            else { "relaunch tap: already installed" });
        return;
    }
    // The slot is only ours to wrap while it holds libhelpers' own `HLunaServiceMessage`: another
    // value (a different SDL build, a slot of some other type) would be called as a function
    // pointer it is not.
    let expected = plx_base::dynlib::Handle::open_loaded(&["libhelpers.so.2"])
        .and_then(|(h, _)| h.sym("HLunaServiceMessage"))
        .filter(|p| !p.is_null())
        .map(|p| p as usize);
    if !slot_holds(current, expected) {
        log("relaunch tap: slot does not hold libhelpers' HLunaServiceMessage \u{2014} not installed");
        return;
    }
    REAL_MESSAGE.store(current, Ordering::Release);
    MAIN_THREAD.store(libc::pthread_self() as usize, Ordering::Relaxed);
    slot.store(wrapped, Ordering::Release);
    log("relaunch tap: installed");
}
#[cfg(feature = "hostsim")]
unsafe fn tap_relaunch_payloads() {}

/// Does the SDL slot hold exactly the function `libhelpers.so.2` exports as `HLunaServiceMessage`?
/// The Thumb bit is compared as `dlsym` and the slot both carry it. `expected` is `None` when
/// libhelpers is not mapped or lacks the symbol, and then nothing holds.
#[cfg(any(test, not(feature = "hostsim")))]
fn slot_holds(current: usize, expected: Option<usize>) -> bool {
    expected == Some(current)
}

/// Is `addr` aligned for the `AtomicUsize` the slot is read and written as?
#[cfg(any(test, not(feature = "hostsim")))]
fn slot_aligned(addr: usize) -> bool {
    addr % core::mem::align_of::<usize>() == 0
}

#[cfg(not(feature = "hostsim"))]
type LunaMessageFn = unsafe extern "C" fn(*mut std::ffi::c_void) -> *const std::ffi::c_char;
#[cfg(not(feature = "hostsim"))]
static REAL_MESSAGE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "hostsim"))]
static MAIN_THREAD: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// SDL's payload read, passed through. Every Luna reply SDL subscribed to comes through here, so
/// anything that is not a relaunch costs one substring scan and nothing else.
#[cfg(not(feature = "hostsim"))]
unsafe extern "C" fn wrapped_message(msg: *mut std::ffi::c_void) -> *const std::ffi::c_char {
    let real = REAL_MESSAGE.load(std::sync::atomic::Ordering::Acquire);
    if real == 0 {
        return std::ptr::null();
    }
    let real: LunaMessageFn = std::mem::transmute(real);
    let p = real(msg);
    if !p.is_null() {
        let payload = std::ffi::CStr::from_ptr(p).to_string_lossy();
        if payload.contains("\"relaunch\"") {
            let main = libc::pthread_self() as usize == MAIN_THREAD.load(std::sync::atomic::Ordering::Relaxed);
            log(&format!("relaunch tap: {} bytes on the {} thread, parameters={}",
                payload.len(), if main { "main" } else { "other" }, payload.contains("\"parameters\"")));
            plx_platform::tv::launch::offer(&payload);
        }
    }
    p
}

/// The cursor [`hide_cursor`] replaced with the transparent one, for the motion that restores it.
#[cfg(not(feature = "hostsim"))]
static SHOWN_CURSOR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(not(feature = "hostsim"))]
type CursorVisibilityFn = unsafe extern "C" fn(c_int) -> c_int;
#[cfg(not(feature = "hostsim"))]
type SetCursorFn = unsafe extern "C" fn(*mut std::ffi::c_void);
#[cfg(not(feature = "hostsim"))]
type GetCursorFn = unsafe extern "C" fn() -> *mut std::ffi::c_void;
#[cfg(not(feature = "hostsim"))]
type CreateCursorFn =
    unsafe extern "C" fn(*const u8, *const u8, c_int, c_int, c_int, c_int) -> *mut std::ffi::c_void;

/// Which call hides the pointer on this firmware, resolved once: LG's extension when the system
/// SDL has it (webOS 4+), else a transparent SDL cursor (webOS 3.x).
#[cfg(not(feature = "hostsim"))]
#[derive(Clone, Copy)]
enum CursorDoor {
    Lg(CursorVisibilityFn),
    /// `SDL_SetCursor` / `SDL_GetCursor` / `SDL_GetDefaultCursor`, and the transparent cursor.
    Sdl { set: SetCursorFn, get: GetCursorFn, def: GetCursorFn, blank: usize },
    None,
}

/// Resolved on the first hide, after `SDL_Init`, because `SDL_CreateCursor` needs the video
/// subsystem.
#[cfg(not(feature = "hostsim"))]
fn cursor_door() -> CursorDoor {
    static DOOR: std::sync::OnceLock<CursorDoor> = std::sync::OnceLock::new();
    *DOOR.get_or_init(|| {
        use std::ffi::c_void;
        use std::mem::transmute;
        // SAFETY: each symbol is transmuted to the signature SDL2's public API (or, for
        // `SDL_webOSCursorVisibility`, LG's fork) gives it, and is called on the main thread.
        let door = unsafe {
            if let Some(f) = self_sym("SDL_webOSCursorVisibility") {
                CursorDoor::Lg(transmute::<*mut c_void, CursorVisibilityFn>(f))
            } else if let (Some(set), Some(get), Some(def), Some(create)) = (
                self_sym("SDL_SetCursor"),
                self_sym("SDL_GetCursor"),
                self_sym("SDL_GetDefaultCursor"),
                self_sym("SDL_CreateCursor"),
            ) {
                // 8x1 with every data and mask bit clear: SDL's transparent pixel throughout.
                let bits = [0u8; 1];
                let create = transmute::<*mut c_void, CreateCursorFn>(create);
                let blank = create(bits.as_ptr(), bits.as_ptr(), 8, 1, 0, 0);
                if blank.is_null() {
                    CursorDoor::None
                } else {
                    CursorDoor::Sdl {
                        set: transmute::<*mut c_void, SetCursorFn>(set),
                        get: transmute::<*mut c_void, GetCursorFn>(get),
                        def: transmute::<*mut c_void, GetCursorFn>(def),
                        blank: blank as usize,
                    }
                }
            } else {
                CursorDoor::None
            }
        };
        log(match door {
            CursorDoor::Lg(_) => "cursor: SDL_webOSCursorVisibility",
            CursorDoor::Sdl { .. } => "cursor: transparent SDL cursor (no SDL_webOS* in this SDL)",
            CursorDoor::None => "cursor: no way to hide the pointer",
        });
        door
    })
}
#[cfg(all(test, not(feature = "hostsim")))]
mod cursor_door_tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    const BLANK: usize = 0x99;

    thread_local! {
        static CURRENT: Cell<usize> = const { Cell::new(0) };
        static DEFAULT: Cell<usize> = const { Cell::new(0) };
        static SET: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
        static LG: RefCell<Vec<c_int>> = const { RefCell::new(Vec::new()) };
    }

    unsafe extern "C" fn set(c: *mut c_void) {
        SET.with(|s| s.borrow_mut().push(c as usize));
        CURRENT.with(|cur| cur.set(c as usize));
    }
    unsafe extern "C" fn get() -> *mut c_void {
        CURRENT.with(Cell::get) as *mut c_void
    }
    unsafe extern "C" fn def() -> *mut c_void {
        DEFAULT.with(Cell::get) as *mut c_void
    }
    unsafe extern "C" fn lg(visible: c_int) -> c_int {
        LG.with(|l| l.borrow_mut().push(visible));
        0
    }

    /// A fresh door with SDL's current cursor and default cursor as given (0 = NULL).
    fn sdl(current: usize, default: usize) -> CursorDoor {
        CURRENT.with(|c| c.set(current));
        DEFAULT.with(|d| d.set(default));
        SET.with(|s| s.borrow_mut().clear());
        CursorDoor::Sdl { set, get, def, blank: BLANK }
    }
    fn installed() -> Vec<usize> {
        SET.with(|s| s.borrow().clone())
    }

    #[test]
    fn hiding_swaps_in_the_blank_cursor_and_motion_puts_the_previous_one_back() {
        let shown = AtomicUsize::new(0);
        let door = sdl(0x10, 0x20);
        unsafe {
            hide_through(door, &shown);
            assert_eq!(installed(), [BLANK]);
            show_through(door, &shown);
        }
        assert_eq!(installed(), [BLANK, 0x10]);
    }

    #[test]
    fn with_no_current_cursor_the_default_one_is_what_comes_back() {
        let shown = AtomicUsize::new(0);
        let door = sdl(0, 0x20);
        unsafe {
            hide_through(door, &shown);
            show_through(door, &shown);
        }
        assert_eq!(installed(), [BLANK, 0x20]);
    }

    #[test]
    fn hiding_again_while_hidden_keeps_the_cursor_to_restore() {
        let shown = AtomicUsize::new(0);
        let door = sdl(0x10, 0x20);
        unsafe {
            hide_through(door, &shown);
            hide_through(door, &shown); // `get` now answers the blank cursor
            show_through(door, &shown);
        }
        assert_eq!(installed(), [BLANK, BLANK, 0x10]);
    }

    /// With no current and no default cursor there is nothing to put back, so the pointer must not
    /// be hidden: it would never come back.
    #[test]
    fn with_nothing_to_restore_the_cursor_is_left_alone() {
        let shown = AtomicUsize::new(0);
        let door = sdl(0, 0);
        unsafe {
            hide_through(door, &shown);
            show_through(door, &shown);
        }
        assert_eq!(installed(), Vec::<usize>::new());
        assert_eq!(shown.load(Relaxed), 0);
    }

    #[test]
    fn lgs_extension_is_asked_to_hide_and_motion_leaves_it_to_the_compositor() {
        let shown = AtomicUsize::new(0);
        LG.with(|l| l.borrow_mut().clear());
        unsafe {
            hide_through(CursorDoor::Lg(lg), &shown);
            show_through(CursorDoor::Lg(lg), &shown);
        }
        assert_eq!(LG.with(|l| l.borrow().clone()), [0]);
    }

    #[test]
    fn the_idle_hide_hint_is_asked_for_only_where_lgs_extension_is_absent() {
        assert_eq!(cursor_sleep_hint(false), Some(c"5000"));
        assert_eq!(cursor_sleep_hint(true), None);
    }
}


/// Advance the once-per-second LOOP-RATE window: bump `iters_ct` and, when a full second has
/// elapsed, recompute `loop_shown`, reset the window, and return `true` so the caller logs the
/// heartbeat with its own route/overlay tag. Shared by the player and home/detail draw paths.
///
/// This counts **loop iterations, not frames**. Since the present gate (`plx_machine::idle`) landed the two
/// are different numbers, and conflating them is the single most reliable way to misread this app:
/// a settled screen runs the loop at the `IDLE_POLL_MS` rate while swapping nothing. The frame
/// count lives beside it in the heartbeat as `fps=`, from `plx_machine::idle::take_presents`.
pub(crate) fn loop_tick(iters_ct: &mut i32, loop_t: &mut u32, loop_shown: &mut i32, now: u32) -> bool {
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
pub(crate) fn replay_budget(raw: Option<&str>) -> u32 {
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
pub(crate) fn resolve_direct_server(
    requested: Option<Result<u16, String>>,
    current: plx_plex::plex::ServerId,
    registered: impl Fn(plx_plex::plex::ServerId) -> bool,
) -> Result<plx_plex::plex::ServerId, String> {
    let Some(slot) = requested else {
        return Ok(current);
    };
    let raw = slot?;
    let sid = plx_plex::plex::ServerId::from_raw(raw);
    registered(sid)
        .then_some(sid)
        .ok_or_else(|| format!("server slot {raw} is not registered"))
}

#[cfg(test)]
mod direct_server_tests {
    //! The direct-screen server rule, beside the function that decides it. It stood in
    //! `app/mod.rs`'s `route_tests` — a module named for a concept D1 deleted — with a doc that
    //! called it a "route-classification rule"; it never was one. Pure, parallel, no global.
    use super::resolve_direct_server;

    #[test]
    fn an_explicit_direct_screen_server_never_falls_back_to_current() {
        let current = plx_plex::plex::ServerId::from_raw(0);
        let secondary = plx_plex::plex::ServerId::from_raw(1);
        assert_eq!(
            resolve_direct_server(None, current, |_| false),
            Ok(current),
            "an absent selector preserves the historical current-server contract"
        );
        assert_eq!(
            resolve_direct_server(Some(Ok(1)), current, |sid| sid == secondary),
            Ok(secondary)
        );
        let missing = resolve_direct_server(Some(Ok(2)), current, |sid| sid == secondary)
            .expect_err("an explicit missing slot must not become current");
        assert!(missing.contains("slot 2"), "{missing}");
        assert_eq!(
            resolve_direct_server(Some(Err("bad selector".into())), current, |_| true),
            Err("bad selector".into())
        );
    }
}

pub(crate) fn direct_trigger_server() -> Result<plx_plex::plex::ServerId, String> {
    resolve_direct_server(
        crate::dev::server_slot(),
        plx_plex::plex::current_server(),
        |sid| plx_plex::plex::client_for(sid).is_some(),
    )
}

// ---- boot, and the loop's own between-frame state ---------------------------------------------
/// Which screen the boot gate landed on — see the gate itself in `plex_run`, which is where the
/// order of its four cases is argued.
pub(crate) enum BootTo {
    Home,
    Login,
    Profiles,
}

/// Pure capture boundary shared by actual boot and owner bootstrap regression fixtures.
pub(crate) fn captured_session_for_boot(saved: plx_plex::plex::session::Session,
    dev_primary: Option<plx_plex::plex::session::ServerRef>,
    extras: Vec<plx_plex::plex::session::SourceRef>) -> plx_session::auth::SessionInit {
    plx_session::auth::SessionInit::captured_boot(saved, dev_primary, extras)
}

fn captured_dev_sources(servers: &[crate::dev::DevServer]) -> Vec<plx_plex::plex::session::SourceRef> {
    servers.iter().filter(|s| s.usable()).filter_map(|s| {
        let origin = s.origin()?;
        Some(plx_plex::plex::session::SourceRef { machine_id: s.machine_id.clone(), name: s.name.clone(),
            address: s.resolve_pin().map_or_else(|| s.host.clone(), |pin| pin.addr().to_string()),
            port: s.port, origin_url: origin.base(), token: s.token.clone(),
            shared_by: s.handle.clone(), owned: s.handle.is_empty(), tier: s.tier, ..Default::default() })
    }).collect()
}

    // Everything that has to happen when the server `plex::client()` answers with CHANGES —
    // whether because a new identity signed in or because the user walked into another source.
    // EVERY store below is keyed to whichever server was current when it was filled, and none
    // of them carries a server in its keys, so leaving one behind means server A's ratingKeys
    // being fetched from server B: the same catalog index opening a different film.
pub(super) fn activate_server_owned(
    bridge: &mut super::bridge::Bridge,
) -> plx_data::stores::EndpointRefreshSet {
    bridge.browse_run(plx_data::stores::browse::BrowseCmd::Reset);
    bridge.refresh_browse_directory();
    bridge.search_run(plx_data::stores::search::SearchCmd::Reset);
    let _ = bridge.hubs_run(plx_data::stores::hubs::HubsCmd::Reset).changed;
    bridge.person_run(plx_data::stores::person::PersonCmd::Reset);
    bridge.collection_run(plx_data::stores::collection::CollectionCmd::Reset);
    // before the stores reset: a stop the previous identity left in flight must not land on this one
    plx_media::route::retire_pending_stops();
    let mut endpoints = bridge.viewstate_run(plx_data::stores::viewstate::ViewStateCmd::Reset).endpoints;
    bridge.metadata_run(plx_data::stores::metadata::MetadataCmd::Reset);
    endpoints.merge(bridge.hubs_run(plx_data::stores::hubs::HubsCmd::RefetchHubs).endpoints);
    endpoints.merge(bridge.browse_discover_pump().endpoints);
    log("pms: catalog activation queued");
    endpoints
}

    // Install the PMS client (the read layer AND the playback path) as the CURRENT server,
    // then fetch the catalog. Used by the boot gate and again when a login resolves; a later
    // call for the same address just swaps the token (profile switch).
    // Takes an ORIGIN and not a `(host, port)` pair: the pair cannot say `https`, and the host
    // a certificate is issued for is the `plex.direct` NAME rather than the address behind it
    // (`plex::origin`). Discovery and persisted sessions may supply either scheme; the client
    // routes control and media requests through the matching transport.
pub(super) fn install_pms_owned(
    bridge: &mut super::bridge::Bridge,
    origin: &plx_plex::plex::Origin,
    address: &str,
    token: &str,
    tier: Option<plx_plex::plex::probe::Location>,
    pin: Option<&plx_plex::plex::ResolvePin>,
    install: &plx_session::auth::owner::ReadyInstall,
) -> plx_data::stores::EndpointRefreshSet {
    if let plx_session::auth::owner::ReadyInstall::PrimaryAndExtras(extras) = install {
        plx_session::auth::install_captured_registry(origin, address, token, tier, pin, extras, None);
    }
    activate_server_owned(bridge)
}
/// Everything before the loop: SDL and the window, GL, text, the poster workers, the boot gate
/// (login / token / session / picker), every dev trigger read once, and the `App` literal —
/// `plex_run`'s former body up to `while app.running`, moved verbatim in phase 1b-ii. An early
/// exit is the process exit code `plex_run` returns.
///
/// `mt`, THE main-thread token, is minted once by `plex_run` itself (`plx_base::task::MainThread::
/// assume()` — that function IS the SDL main thread) and MOVES into `App.adapters.player` here,
/// from where a `&mut PlayerAdapter` is the proof it is still held: the ACB/Starfish seam takes
/// `&MainThread` (which is `!Send`, so `task::spawn` rejects any closure that captured one), and
/// the native session slot takes the adapter itself. See `task::MainThread` and `player::adapter`.
pub(crate) unsafe fn boot(
    pms_host: *const c_char,
    pms_port: c_int,
    mt: plx_base::task::MainThread,
    preflight: super::bootstrap::Preflight,
) -> Result<App, c_int> {
    // The Up Next still prefetch `route::pump_play` asks for is `ui`'s, which `route` may not name,
    // so it is handed over here, before any playback can land. Replay boots take it too.
    plx_media::route::install_up_next_still_warm(super::playback::warm_up_next_still);
    // The dev frame probe's clock is `platform`'s, which `gfx` does not depend on.
    plx_gfx::gfx::install_clear_probe(
        plx_platform::tv::window::frame_probe_waiting,
        plx_platform::tv::window::frame_probe_acquired,
    );
    let initial = match &preflight {
        super::bootstrap::Preflight::Live => None,
        super::bootstrap::Preflight::Record => {
            let host = std::ffi::CStr::from_ptr(pms_host).to_string_lossy();
            match super::bootstrap::Initial::capture_home(&host, pms_port) {
                Ok(initial) => Some(initial),
                Err(reason) => { log(&format!("rec: REFUSED — {reason}")); return Err(1); }
            }
        }
        super::bootstrap::Preflight::Replay { initial, .. } => Some((initial.clone(), None)),
    };
    match initial {
        Some((initial, deferred)) => App::from_init(initial, preflight, pms_host, pms_port, mt, deferred),
        None => construct(pms_host, pms_port, mt, preflight, None, None),
    }
}

pub(super) fn apply_deferred_capture(rec: &mut super::recorder::Recplay,
    gate: &plx_machine::landgate::Gate,
    deferred: plx_plex::plex::session::DeferredLoad) -> Result<(), &'static str> {
    if let Err(reason) = deferred.apply() {
        rec.abort_startup(gate)?;
        return Err(reason);
    }
    Ok(())
}

/// **Boot's restore**: the mirrors from the record, then the ten live per-profile cells for the seat
/// the process starts with, `Unseated` (no profile is published yet), through the one
/// `route::restore_profile_rows` the seat trigger (`bridge::SeatWatch`) calls at every seat change.
/// Every cell then holds the fallback (the frozen install-wide key, then the default), which is the
/// value boot always set. Split out of [`construct`] so the cells it sets can be graded without an
/// SDL window (`run.rs`' `lifecycle_regression_tests`).
///
/// Restored before any route can resolve a stream. A legacy file with no quality resolves to
/// Original; a new file can choose Auto only through route's explicit readiness gate
/// (session::load records that decision once). The dev quality override is honoured unless the
/// harness is `controlled`.
pub(super) fn restore_player_preferences(session: &plx_plex::plex::session::Session, controlled: bool) {
    plx_media::route::restore_install_rows(session);
    plx_media::route::restore_profile_rows(session, &plx_plex::plex::session::Seat::Unseated,
        plx_media::route::Overrides {
            quality: if controlled { None } else { crate::dev::playback_quality_override() },
        });
}

/// **The stored session resumed, as boot does it**: `ResumeStored` makes the owner seat the stored
/// profile (`release_held_handoff` publishes it, which is what the seat trigger sees) and emit
/// `Ready`; the handoff is returned for the caller to install. The same function the stored boot
/// runs, so a host test grades the boot and not a copy of it.
pub(super) fn resume_stored(pages: &mut plx_ui::dispatch::Dispatcher<super::bridge::AppHost>,
    bridge: &mut super::bridge::Bridge, tap: &mut dyn plx_ui::dispatch::Tap<super::bridge::AppHost>)
    -> Option<plx_session::auth::ReadyCreds> {
    super::bridge::execute_session_command(pages, plx_session::auth::SessionCmd::ResumeStored);
    pages.frame_with(bridge, plx_machine::machine::Tick::default(), Vec::new(), Vec::new(), tap, false);
    bridge.take_session_ready()
}

pub(crate) unsafe fn construct(
    pms_host: *const c_char, pms_port: c_int, mt: plx_base::task::MainThread,
    preflight: super::bootstrap::Preflight, initial: Option<super::bootstrap::Initial>,
    deferred: Option<plx_plex::plex::session::DeferredLoad>,
) -> Result<App, c_int> {
    let controlled = preflight.controlled();
    if let Some(initial) = &initial {
        plx_data::stores::tape::init(initial.person_credits(), preflight.replay());
        initial.home.restore(&mt).map_err(|_| 1)?;
        plx_plex::plex::Client::restore_generation_seed(initial.primary_client).map_err(|_| 1)?;
    }
    SDL_SetMainReady();
    // DEAD END, measured 2026-07-31 — do not re-try this. The obvious answer to "a parked TV
    // should blank itself" is to stop inhibiting the platform screensaver here (and re-allow it
    // per route, since webOS BACKGROUNDS the app to run one and `0x103` suspends the
    // buffer-feed, so it could never be on during playback). It does not work, for a reason
    // upstream of this app: the TV's SDL 2.0.4 fork carries the
    // `SDL_VIDEO_ALLOW_SCREENSAVER` hint STRING but implements no wayland idle-inhibit
    // (`strings libSDL2-2.0.so.0` finds no `idle_inhibit`/`suspend_screensaver` symbol), so
    // this call and `SDL_EnableScreenSaver` are both no-ops. Soaked 34 min on Home with the
    // TV's own `screenSaverEnabled: on`: no screensaver, no `LIFECYCLE: background`, CPU flat,
    // our UI still at full brightness on the panel. webOS does not blank a foreground native
    // app, and nothing reachable from SDL changes that. The line stays because it costs
    // nothing and states the intent; it is not what keeps the screensaver away.
    SDL_SetHint(c"SDL_VIDEO_ALLOW_SCREENSAVER".as_ptr(), c"0".as_ptr());
    webos3_cursor_sleep_hint();
    if SDL_Init(SDL_INIT_VIDEO) != 0 {
        log("SDL_Init failed");
        return Err(1);
    }
    tap_relaunch_payloads();
    {
        let d = SDL_GetCurrentVideoDriver();
        if !d.is_null() {
            log(&format!(
                "video driver: {}",
                std::ffi::CStr::from_ptr(d).to_string_lossy()
            ));
        }
    }
    // The television has a real GLES2 driver (a shim over libmali). macOS has none at all —
    // Apple ships desktop GL only, capped at 4.1 core — so asking for ES here fails context
    // creation outright. 4.1 core is the closest thing that exists, and it is a superset for
    // everything this renderer does: a real VBO (never client arrays) and RGBA/UNSIGNED_BYTE
    // textures, both core-profile-legal. The shader sources are adapted at compile time by
    // `gfx::glsl_preamble`, which reads the driver's GLSL version rather than assuming.
    if cfg!(feature = "hostsim") {
        SDL_GL_SetAttribute(A_CTX_PROFILE_MASK, CTX_PROFILE_CORE);
        SDL_GL_SetAttribute(A_CTX_MAJOR, 4);
        SDL_GL_SetAttribute(A_CTX_MINOR, 1);
    } else {
        SDL_GL_SetAttribute(A_CTX_PROFILE_MASK, CTX_PROFILE_ES);
        SDL_GL_SetAttribute(A_CTX_MAJOR, 2);
        SDL_GL_SetAttribute(A_CTX_MINOR, 0);
    }
    // full 32-bit RGBA so the video plane shows through
    SDL_GL_SetAttribute(A_RED, 8);
    SDL_GL_SetAttribute(A_GREEN, 8);
    SDL_GL_SetAttribute(A_BLUE, 8);
    SDL_GL_SetAttribute(A_ALPHA, 8);
    SDL_GL_SetAttribute(A_BUFFER_SIZE, 32);
    // ...and NO depth or stencil, which SDL would otherwise give us anyway: its defaults are
    // 16 bits of depth and 0 of stencil, and asking for neither had simply never been written
    // down. **This renderer has no use for either.** There is no `GL_DEPTH_TEST`, no
    // `glDepthFunc`, no `glDepthMask` and no `glClear(GL_DEPTH_BUFFER_BIT)` anywhere in the
    // crate — every screen is painter's-algorithm 2-D, drawn back to front — and the one
    // scissor user (`gfx::clip_set`) is a scissor, not a stencil.
    //
    // On a TILER this is not merely 4 MB of address space. Midgard allocates the depth buffer
    // per tile alongside colour and, unless the driver proves it dead, RESOLVES it to memory at
    // end-of-frame: 1920x1080x2 bytes written per presented frame for a buffer nothing ever
    // reads. `system.rs` logs what the config actually came back with — a request is not a
    // grant, and the only honest confirmation is `FB bits: … depth=0`.
    SDL_GL_SetAttribute(A_DEPTH, 0);
    SDL_GL_SetAttribute(A_STENCIL, 0);
    // **The stored session is read HERE, before the window.** The window size is a setting
    // (Settings > System > Interface resolution, install-wide: `Session::ui_resolution`), and
    // `SDL_CreateWindow` below is the only moment it can be applied, so the record has to be read
    // first. `peek_settled` would not do: it only reads the cache, which the session owner fills
    // after boot, so its first call answers "not yet". The seams `load` consults (`install_plex_seams`) are installed by
    // `enter_application`, before `boot`. A replay or captured boot brings its own session.
    let session = match &initial {
        Some(initial) => initial.session.persisted.clone(),
        None => plx_plex::plex::session::load(),
    };
    // The television is placed at 0,0 at exactly canvas size (unless a webOS 1-3 owner picked a smaller
    // Interface resolution, `requested_drawable`) and takes the panel. A desktop
    // window is centred (`SDL_WINDOWPOS_CENTERED`) at whatever fits — see `plan_desktop_window`.
    // The density is not known until a window exists, so this is the 1x answer; `fit_window_to_density`
    // below corrects it on a display where that was wrong.
    #[cfg(feature = "hostsim")]
    let desktop_ask = desktop_window_ask();
    #[cfg(feature = "hostsim")]
    let (wx, wy, ww_req, wh_req) = {
        let (w, h) = plan_desktop_window(desktop_ask, Density::UNIT);
        (WINDOWPOS_CENTERED, WINDOWPOS_CENTERED, w, h)
    };
    #[cfg(not(feature = "hostsim"))]
    let (wx, wy, ww_req, wh_req) = {
        let (w, h) = requested_drawable(&session).unwrap_or((SCR_W, SCR_H));
        (0, 0, w, h)
    };
    // The title is furniture a television never draws (no window manager, no decoration) and
    // the first thing a desktop shows, so the two builds spell it differently: the device keeps
    // the process-shaped name every log, `pidof` recipe and skill already uses.
    #[cfg(feature = "hostsim")]
    let title = c"PlxNative";
    #[cfg(not(feature = "hostsim"))]
    let title = c"plxnative";
    // A frame dump's drawable is exactly the window's size in points: it is the one desktop window
    // that does not ask for ALLOW_HIGHDPI (0x2000), so its window is the 1920x1080 points of the
    // canonical Linux host on a Retina display too, and `fit_window_to_density` finds nothing to
    // correct. (Absent from every shipping build.)
    #[cfg(all(feature = "hostsim", feature = "devtriggers"))]
    let window_flags = if crate::dev::framedump::requested() { SDL_WINDOW_FLAGS & !0x2000 } else { SDL_WINDOW_FLAGS };
    #[cfg(not(all(feature = "hostsim", feature = "devtriggers")))]
    let window_flags = SDL_WINDOW_FLAGS;
    // **A stored size the set will not give must never brick launch.** If the window or its GL
    // context is refused at a smaller size the owner chose, free whatever was made and retry ONCE
    // at the native canvas; only a refused native window is fatal. On a desktop the request is the
    // desktop's own size, not a setting, so there is no retry there.
    #[cfg(not(feature = "hostsim"))]
    let mut fallback = window_fallback((ww_req, wh_req));
    #[cfg(feature = "hostsim")]
    let mut fallback: Option<(c_int, c_int)> = None;
    let (mut ww_try, mut wh_try) = (ww_req, wh_req);
    let win = loop {
        let win = SDL_CreateWindow(title.as_ptr(), wx, wy, ww_try, wh_try, window_flags);
        if win.is_null() {
            log(&format!("CreateWindow failed at {ww_try}x{wh_try}"));
        } else {
            let ctx = SDL_GL_CreateContext(win);
            if !ctx.is_null() {
                break win;
            }
            log(&format!("GL ctx failed at {ww_try}x{wh_try}"));
            SDL_DestroyWindow(win);
        }
        let Some((w, h)) = fallback.take() else { return Err(1); };
        log(&format!("surface: retrying the window at the native {w}x{h}"));
        (ww_try, wh_try) = (w, h);
    };
    // Correct the size for the display's density now: `surface::probe` below reads the drawable once.
    #[cfg(feature = "hostsim")]
    fit_window_to_density(win, desktop_ask);
    plx_base::surface::probe(win);
    // vsync on → the frame rate locks to the panel refresh. `/tmp/plxnative-novsync` uncaps it so
    // `fps=` reports the true GPU render rate. WSLg's X11/GLX swap accepts interval 1 without
    // blocking, so the software budget in `run` follows the same switch.
    // A frame dump never waits on the display: every iteration is a render, at swap interval 0 and
    // with the Linux software pacer off (`vsync_enabled` also gates `wslg_frame_pacing`).
    #[cfg(all(feature = "hostsim", feature = "devtriggers"))]
    let dumping = crate::dev::framedump::requested();
    #[cfg(not(all(feature = "hostsim", feature = "devtriggers")))]
    let dumping = false;
    let vsync_enabled = !dumping && (controlled || !crate::dev::scenarios::novsync_armed());
    SDL_GL_SetSwapInterval(if vsync_enabled { 1 } else { 0 });
    #[cfg(all(feature = "hostsim", target_os = "linux"))]
    let wslg_frame_pacing = vsync_enabled
        && std::env::var_os("WSL_DISTRO_NAME").is_some()
        && std::env::var("SDL_VIDEODRIVER").as_deref() == Ok("x11");
    {
        let r = glGetString(GL_RENDERER);
        let v = glGetString(GL_VERSION);
        let renderer = (!r.is_null()).then(|| std::ffi::CStr::from_ptr(r).to_string_lossy());
        if let (Some(renderer), false) = (&renderer, v.is_null()) {
            log(&format!("GL: {} / {}", renderer, std::ffi::CStr::from_ptr(v).to_string_lossy()));
        }
        // A CPU rasterizer's frame time is not a main-thread hang (`task::runtime_check`).
        #[cfg(feature = "threadcheck")]
        plx_base::task::runtime_check::note_renderer(renderer.as_deref());
    }
    // The system on-screen keyboard, PROBED — see `crate::textinput`'s module doc. Both facts
    // on this line are preconditions that fail in complete silence, and nothing in this tree
    // had ever read either of them:
    //   support= `SDL_HasScreenKeyboardSupport` — does this firmware's SDL have a panel at all.
    //   focus=   `SDL_WINDOW_INPUT_FOCUS` — `SDL_StartTextInput` shows the panel only
    //            `if (SDL_GetKeyboardFocus())`. Clear, and it enables text events, returns
    //            void, and no panel appears.
    //   active=  whether text events are already on. It is 1 on a desktop and 0 here, because
    //            SDL only auto-starts text input on platforms with NO screen keyboard — which
    //            is precisely why `textinput` tracks its own started flag instead of this one.
    // A `focus=0` HERE is not yet a verdict: the flag arrives with the wayland keyboard
    // `enter`, which needs the event loop below. `textinput::start` logs it again at the
    // moment the field asks for the panel, which is the reading that decides anything.
    // What EGL this set has — extension string, swap behaviour, buffer age. One boot-time
    // read, logged and used for nothing: `docs/egl-partial-update-and-damage.md` is what it
    // was for. Deliberately NOT a new link dependency; see `egl.rs`'s module doc for why
    // `-lEGL` would kill the process at exec() on the very firmwares this app runs on.
    // No platform carve-out: the probe asks EGL nothing unless an EGL context is current on this
    // thread (`egl::current_with`), which is what makes it safe on a GLX-backed Linux simulator.
    plx_gfx::egl::probe();
    crate::textinput::bind(win);
    // …and the same handshake for the ROOT press: `tv::home::go_home`'s fallback leg minimizes
    // this window, and the window is created here, a long way from where BACK is decided.
    plx_platform::tv::window::bind_window(win);
    let wflags = SDL_GetWindowFlags(win);
    log(&format!(
        "keyboard: support={} active={} focus={} winflags=0x{wflags:x}",
        SDL_HasScreenKeyboardSupport(),
        SDL_IsTextInputActive(),
        i32::from(wflags & SDL_WINDOW_INPUT_FOCUS != 0)
    ));

    plx_platform::tv::window::grab(win);
    // EXPERIMENT (`/tmp/plxnative-opaque`), no-op without the trigger: build the full-surface
    // wl_region once, so `opaque_route` below can declare the UI plane opaque on every screen
    // that has nothing behind it. See `system.rs`'s section on it.
    plx_platform::tv::window::arm_opaque_region();
    plx_gfx::gfx::init_gl();
    plx_gfx::text::init_text();
    plx_gfx::gfx::init_image();
    plx_gfx::gfx::init_blur();
    // The transport takes the client's `User-Agent` as a value (it names no Plex layer): hand it
    // over before any request can be made, so the first one already carries it.
    if plx_net::net::set_user_agent(plx_plex::plex::identity::user_agent()).is_err() {
        plx_base::eventlog::log("net: the client's User-Agent holds a control character; none is sent");
    }
    // One-time libcurl bind + init (main thread) before any threaded HTTPS call. A false here
    // means this device has no libcurl we can bind, so plex.tv sign-in will not work — the app
    // still runs, and `net::global_init` has already said so in the event log.
    let _ = plx_net::net::global_init();
    // Drain whatever the LAST session left behind, on a worker — and **after `global_init`,
    // which is the whole reason this line is here and not beside `telemetry::boot()` 170 lines
    // up.** It was there first, and the end-to-end run showed why that was wrong: the worker
    // reached `post_ca` before libcurl was bound, `net::available()` was false, every record
    // came back Keep, and the log read `holding 5 records` immediately ABOVE `net: bound
    // libcurl`. So the first flush of every launch failed, always, and the failure was
    // indistinguishable from a television with no network. Worse than the lost flush: curl's
    // own init is documented as not thread-safe, and a worker that got there first would have
    // been doing it off the main thread.
    //
    // Boot is the right cadence for a television. Sessions are long, and the reports most worth
    // having are about how one ENDED — a crash is the end, so the record was written by a
    // process that no longer exists and this is the first moment anything can send it. A record
    // queued during THIS session goes out at the next launch, or sooner if a consent change
    // flushes.
    if !preflight.controlled() { plx_telemetry::telemetry::flush_soon(); }

    // NO token is compiled into this binary. PMS access comes from the signed-in session,
    // or — for automated runs only (the regression harness, headless captures) — from the
    // /tmp/plxnative-token dev trigger. The value is NEVER logged (only that one is in effect).
    let dev_token = match &initial {
        Some(initial) => {
            let plx_session::auth::owner::BootstrapAuthority::DevPms { primary, .. } = &initial.session.authority
                else { unreachable!("preflight validated authority") };
            primary.token.clone()
        }
        _ => crate::dev::scenarios::dev_token(),
    };
    // dev: /tmp/plxnative-servers — credentials for a SECOND (third, …) server, so an automated
    // run can reach a friend's SHARED server beside the one above. A shared server is its own
    // authority: its own machineIdentifier, its own per-(user,server) access token, and a 401
    // for anybody else's — which is precisely what ONE `plxnative-token` cannot express, and
    // why no two-source state could be graded headlessly before this.
    //
    // ADDITIVE and nothing more. The primary is still `plxnative-token` (or the stored session)
    // against the configured host/port (or the explicit dev-only pms-origin fixture override),
    // so an ordinary run that names one server behaves as before. `dev::servers()` is the accessor — memoized, so the harness's
    // /tmp wipe cannot change what this boot was handed — and `dev::DevServer` is the shape.
    //
    // It is NOT on the DIAG exemption list (`dev.rs`), deliberately: unlike a log or the anim
    // overlay, this file names a host AND the token to trust it with, so it must mark the boot
    // automated and skip the who's-watching picker exactly as `plxnative-token` does. A run
    // that landed on the picker instead of Home would grade the wrong screen.
    //
    // Tokens are never logged: `DevServer` has no `Debug`, and `describe()` prints all of it
    // except the token.
    let dev_servers = if controlled { Ok(Vec::new()) } else { crate::dev::servers() };
    match &dev_servers {
        // `_e`: the only reader is the gated log line (see `dev.rs` on why literals are gated).
        Err(_e) => {
            #[cfg(feature = "devtriggers")]
            log(&format!("servers: /tmp/plxnative-servers IGNORED — not valid JSON: {_e}"));
        }
        Ok(v) if !v.is_empty() => {
            let usable = v.iter().filter(|s| s.usable()).count();
            for (i, s) in v.iter().enumerate() {
                let creds = if s.usable() {
                    "ok"
                } else {
                    "MISSING (empty host/port or token)"
                };
                log(&format!("servers: #{i} {} creds={creds}", s.describe()));
            }
            log(&format!(
                "servers: {} extra server(s) injected, {usable} usable",
                v.len()
            ));
        }
        Ok(_) => {}
    }
    let host_s = std::ffi::CStr::from_ptr(pms_host)
        .to_string_lossy()
        .into_owned();

    // UI infra + poster workers always come up — the owned login/profiles screens use them too.
    //
    // **No `ui::login::init()` / `ui::profiles::init()` here any more (phase 6 retirement).**
    // Both calls used to allocate the legacy `Scene` statics `ui::login`/`ui::profiles` drew
    // from; `screens::login::LoginScreen`/`screens::profiles::ProfilesScreen` are owned
    // `Screen`s constructed fresh by `AppMounter::mount` on each entry (`app/bridge.rs`) and
    // read no module global at all, so nothing left in the boot path calls into either legacy
    // module's `scene()` and the `.expect("…::init not called")` guard those calls carried is no
    // longer reachable. BOTH legacy modules are deleted outright: `ui/login.rs`'s one surviving
    // reader (`screens::login::LoginScreen::resync`'s `delete_leftovers` count) moved to `auth`,
    // and `ui/profiles.rs`'s last reader was a single `const` — `screens::onboard.rs`'s
    // `CRUMB_PROFILES`, the breadcrumb over the Favourites editor — which now reads
    // `screens::profiles::TITLE`, the owned screen's own title. Worth remembering when the next
    // family is retired: what kept 1,250 dead lines in the tree was not a hard dependency but one
    // string constant nobody had moved, and it was invisible because the file still compiled.
    if !preflight.controlled() {
        super::adapters::poster::init();
        crate::capture::init(); // dev live UI capture stream
    }

    // Any dev trigger under /tmp marks the boot as automated (the harness token override,
    // autoplay/detail captures, playback-path knobs): those runs need a deterministic Home,
    // so the boot who's-watching picker is skipped. Pure diagnostics (the logs, the profiler,
    // the anim overlay) don't count as automation.
    // The scan itself, and the DIAG exemption list that decides what does NOT count as
    // automation, live in `dev::any_trigger_present` — together with the `plxnative-anim.log`
    // bug that list was rewritten for. It is the one dev-trigger surface that names no file,
    // so it is also the one a release build had to be taught about explicitly.
    let automated_boot = || controlled || crate::dev::any_trigger_present();

    // Boot gate. Order matters:
    //  1. /tmp/plxnative-login forces the QR login screen (to exercise the flow on demand).
    //  2. /tmp/plxnative-token (the harness / headless runs) beats the stored session — automation
    //     must run as the injected test identity no matter who is signed in on the TV.
    //  3. A stored session (offline-capable LAN server) → Home. A multi-user Plex Home roster
    //     still goes through the who's-watching picker first on interactive boots, unless
    //     Automatically Sign In is on and a profile is already seated (that skip is an explicit
    //     opt-in, PIN included). Automated boots and a roster of one still skip the picker.
    //  4. Nothing → the QR sign-in flow (no credentials are compiled in — like a real client).
    // The destination itself is [`BootTo`], at module scope with the rest of the vocabulary.
    //
    // dev: /tmp/plxnative-pickuser=<index> — force the boot picker even on an automated boot and
    // auto-select that roster tile once it's up (headless exercise of the who's-watching flow).
    let pick_user: Option<usize> = if controlled { None } else { crate::dev::scenarios::pickuser_index() };
    // The remembered server keys of the session this boot runs on, handed over before any request
    // can leave: a captured session was never READ through the cache that projects them.
    plx_plex::plex::session::project_server_keys(&session, false);
    // dev: /tmp/plxnative-tls-selftest — exercises the wrong-clock TLS fallback on both planes with
    // no account (a no-op without the trigger; absent in shipping builds). Armed HERE, after the
    // projection above, and not beside `global_init`: the projection replaces the key table
    // wholesale, and the self-test's pin is filed under a machine no session holds, so a projection
    // landing while round 1's first handshake is in flight would wipe it and the round be refused.
    #[cfg(feature = "devtriggers")]
    crate::dev::scenarios::tls_selftest::arm_at_boot();
    // dev: /tmp/plxnative-toast=<text> — asks the TV's notification service for a system toast,
    // once plain and once as the app, and logs each full outcome (`toast-probe …`).
    #[cfg(feature = "devtriggers")]
    crate::dev::scenarios::toast_probe::arm_at_boot();
    // dev: /tmp/plxnative-clockfact=nokey|keychanged|engaged[:<year>] — plants a wrong-clock fact
    // (`net::keypin`) so the Home/Library read-out and the toast can be looked at with no
    // television. Armed after the projection above, whose sign-out arm only clears bound hosts.
    #[cfg(feature = "devtriggers")]
    crate::dev::scenarios::clock_fact::arm_at_boot();
    // dev: /tmp/plxnative-hintoff — the hold hint taught for this run, on every screen, read
    // once here on the frame thread (the hint's latches are thread-local). The site demo film is
    // shot with it: the capsule is a one-time teaching overlay, and a film has no viewer to teach.
    #[cfg(feature = "devtriggers")]
    if plx_base::devtrig::flag("hintoff") {
        plx_ui::hold_hint::teach_all_for_dev();
    }
    // dev: /tmp/plxnative-hintfresh — this run starts with the hold hint untaught, though the install
    // was taught: the seed below is skipped, so the hint can be looked at again. Nothing on disk is
    // un-written; a read showing then writes taught again (an idempotent write).
    // The hold hint offers itself once per INSTALL: seed what an earlier run taught, before frame one.
    #[cfg(feature = "devtriggers")]
    let hint_fresh = plx_base::devtrig::flag("hintfresh");
    #[cfg(not(feature = "devtriggers"))]
    let hint_fresh = false;
    if !hint_fresh {
        super::hint_taught::seed(session.hold_hint_taught());
    }
    #[cfg(not(test))]
    plx_platform::i18n::initialize(session.language, controlled);
    let forced_login = !controlled && crate::dev::scenarios::login_forced();
    let dev_primary = (!forced_login && !dev_token.is_empty()).then(|| {
        let origin = crate::dev::scenarios::pms_origin()
            .unwrap_or_else(|| plx_plex::plex::Origin::http(&host_s, pms_port));
        plx_plex::plex::session::ServerRef {
            address: origin.host().to_owned(), port: i64::from(origin.port()),
            origin_url: origin.base(), token: dev_token.clone(),
            tier: Some(plx_plex::plex::probe::configured_tier(origin.host())), ..Default::default()
        }
    });
    let session_init = match &initial {
        Some(initial) => initial.session.clone(),
        None => captured_session_for_boot(session.clone(), dev_primary,
            captured_dev_sources(&dev_servers.unwrap_or_default())),
    };
    let mut bridge = if controlled {
        super::bridge::Bridge::controlled_home(plx_base::diag::heartbeat::now_us,
            initial.as_ref().expect("controlled initialization"), &mt, preflight.replay())
    } else {
        super::bridge::Bridge::new(plx_base::diag::heartbeat::now_us, session_init,
            plx_telemetry::telemetry::consent::current().unwrap_or_default(), &mt)
    };
    // Construct the one dispatcher before bootstrap commands; move this same queue into App.
    let mut pages = plx_ui::dispatch::Dispatcher::with_transition(Box::new(
        plx_ui::containers::transition::PageDip::new(),
    ));
    restore_player_preferences(&session, controlled);
    // dev: /tmp/plxnative-audioenh=off|boost|loudness — force the enhancement preference right after
    // the restore. Nobody is seated yet, so this sets the live cell alone (the door refuses to save
    // for nobody), which is all a dev-token boot, which never seats, gets. A boot that seats a
    // profile runs the arm again, writing THAT profile's record, at the seat change
    // (`bridge::SeatWatch`). See `dev::scenarios::arm_audio_enhancements`'s own doc.
    if !controlled { crate::dev::scenarios::arm_audio_enhancements(); }
    let primary_binding = initial.as_ref().map(|initial| initial.primary_client);
    let activate_session = |bridge: &mut super::bridge::Bridge,
        pages: &mut plx_ui::dispatch::Dispatcher<super::bridge::AppHost>,
        rec: &mut super::recorder::Recplay| {
        if forced_login {
        super::bridge::execute_session_command(pages, plx_session::auth::SessionCmd::StartLogin);
        pages.frame_with(bridge, plx_machine::machine::Tick::default(), Vec::new(), Vec::new(),
            rec, false);
        #[cfg(feature = "devtriggers")]
        log("boot: /tmp/plxnative-login — starting QR login");
        BootTo::Login
    } else if !dev_token.is_empty() {
        // `Origin::http` names the assumption out loud: the host and port compiled into the
        // C shim are a plaintext address, with no scheme to read off them.
        //
        // The tier is classified from that address rather than left `None`. There is no
        // plex.tv connection list on this path to read a `local` flag off, and `None` means
        // "nothing has said" — which left every automated run unable to reach Auto's Original
        // bootstrap, since `abr::bootstrap` is only consulted once a tier exists. See
        // `probe::configured_tier` for why address shape is honest enough here.
        let tier = plx_plex::plex::probe::configured_tier(&host_s);
        log(&format!(
            "boot: dev token — link={tier:?} (classified from the configured address)"
        ));
        super::bridge::execute_session_command(pages, plx_session::auth::SessionCmd::ActivateDevBootstrap);
        pages.frame_with(bridge, plx_machine::machine::Tick::default(), Vec::new(), Vec::new(),
            rec, false);
        if let Some(ready) = bridge.take_session_ready() {
            if controlled {
                if bridge.bind_primary(primary_binding.expect("controlled initial binding")).is_err() {
                    log("bootstrap: primary resource binding refused");
                    return BootTo::Login;
                }
                use plx_data::stores::{StoreCmd, StoreWork};
                use plx_machine::machine::{Fx, MachineId};
                use plx_screens::registry::AppFx;
                for cmd in [
                    StoreCmd::Browse(plx_data::stores::browse::BrowseCmd::Reset),
                    StoreCmd::Hubs(plx_data::stores::hubs::HubsCmd::Reset),
                    StoreCmd::Hubs(plx_data::stores::hubs::HubsCmd::RefetchHubs),
                ] { pages.emit(MachineId::Nav, Fx::App(AppFx::Store(cmd.store(), cmd))); }
                pages.emit(MachineId::Nav, Fx::App(AppFx::StoreWork(StoreWork::BrowseDiscovery)));
                pages.frame_with(bridge, plx_machine::machine::Tick::default(), Vec::new(), Vec::new(), rec, false);
            } else {
                let endpoints = install_pms_owned(bridge, &ready.origin, &ready.address,
                    &ready.token, ready.tier, ready.pin.as_ref(), &ready.install);
                super::bridge::execute_endpoint_outcomes(pages, endpoints);
            }
            BootTo::Home
        } else { BootTo::Login }
    } else if session.can_go_local() {
        if session.boot_shows_picker(automated_boot(), pick_user.is_some()) {
            // The Session owner installs the avatar read client and retained grants, publishes
            // the captured profile, then owns the picker/refresh flows. It does NOT issue the
            // Ready handoff before a viewer is selected. Boot BACK therefore retains its
            // protected/unknown-profile refusal policy rather than silently entering Home.
            super::bridge::execute_session_command(pages,
                plx_session::auth::SessionCmd::StartSwitch(plx_session::auth::Picker::Boot));
            pages.frame_with(bridge, plx_machine::machine::Tick::default(), Vec::new(), Vec::new(),
                rec, false);
            log("boot: stored session — who's watching");
            BootTo::Profiles
        } else {
            // The owner restores the granted roster and publishes WHO is watching before
            // install_pms can read any per-profile stores. Stored boot does not re-save its
            // credentials. This is the normal bounded dispatcher drain over the same owner
            // and queue later moved into App, not a recursive bootstrap reducer.
            if let Some(ready) = resume_stored(pages, bridge, rec) {
                let endpoints = install_pms_owned(bridge, &ready.origin, &ready.address,
                    &ready.token, ready.tier, ready.pin.as_ref(), &ready.install);
                super::bridge::execute_endpoint_outcomes(pages, endpoints);
                // Refresh only AFTER installing the captured primary, so a fast accepted
                // endpoint observation cannot be overwritten by that older boot snapshot.
                super::bridge::execute_session_command(pages, plx_session::auth::SessionCmd::RefreshRoster);
                pages.frame_with(bridge, plx_machine::machine::Tick::default(), Vec::new(), Vec::new(),
                    rec, false);
                if session.auto_sign_in()
                    && session.home_users.len() > 1
                    && session.seated_in_roster()
                    && !automated_boot()
                    && pick_user.is_none()
                {
                    log("boot: stored session — auto sign-in");
                } else {
                    log("boot: stored session — local server (offline-capable)");
                }
                BootTo::Home
            } else {
                super::bridge::execute_session_command(pages, plx_session::auth::SessionCmd::StartLogin);
                log("boot: stored session could not be activated — starting QR sign-in");
                BootTo::Login
            }
        }
    } else {
        super::bridge::execute_session_command(pages, plx_session::auth::SessionCmd::StartLogin);
        pages.frame_with(bridge, plx_machine::machine::Tick::default(), Vec::new(), Vec::new(),
            rec, false);
        log("boot: no session — starting QR sign-in");
        BootTo::Login
    }
    };
    // Controlled Home constructs its App before executing the same bootstrap command path.
    let boot_to = if controlled { BootTo::Home } else {
        activate_session(&mut bridge, &mut pages, &mut super::recorder::Recplay::Off)
    };
    if !controlled {
        plx_media::player::acb_init(&mt);
        plx_media::ff::boot(); // Live playback resource boot; controlled Home cannot invoke ABI probes.
    }
                       // dev: /tmp/plxnative-logintest validates the plex.tv account path end-to-end on the device — a
                       // real typed create_pin() through the libcurl transport + DTO deserialize. Logs only the
                       // public pin id + code length + that authToken is still null (never a token/secret).
    if !controlled { crate::dev::scenarios::arm_logintest(); }
    // dev: the animation-diagnostic overlay is OFF by default; /tmp/plxnative-anim enables it (its
    // trace goes to /tmp/plxnative-anim.log, a separate stream from the main event log)
    if !controlled { crate::dev::scenarios::arm_anim(); }
    // dev: /tmp/plxnative-stillclock=<ms> holds every free-running animation clock (spinners)
    // still, so a screenshot of a waiting screen settles on one deterministic frame.
    if !controlled { crate::dev::scenarios::arm_stillclock(); }
    // dev: profile is asynchronous EXT_disjoint_timer_query timing; hwcnt is the serialized
    // direct Mali counter-attribution run. Their content names ONE phase (empty = frame.ui).
    // Combining them would perturb the timer result, so fail closed when both are present.
    // dev: /tmp/plxnative-glassload is the backdrop-glass LOAD DIAL — a sweep of glass-surface
    // count, size and refresh cadence that cycles its own steps inside one launch, so legs are
    // interleaved by construction. /tmp/plxnative-navblur is the blurred-route-transition
    // prototype. Both live in `ui::glassload`; both are absent from a release build.
    // The plan is built HERE rather than in the `App` literal below so the dial is armed at the
    // point in boot it always was: `configure` logs what it will run, and that line's position in
    // the event log is what a sweep is read against.
    let mut glass = plx_ui::frame::glass::GlassPlan::new();
    if !controlled {
        crate::dev::scenarios::arm_glassload(&mut glass);
        crate::dev::scenarios::arm_navblur(&mut glass);
    }
    // dev: the two OVERDRAW surfaces (`ui::overdraw`, docs/backdrop-blur-profiling.md Part 5).
    // `plxnative-overdraw` arms the CPU-side per-draw-class ledger — how much screen-visible
    // quad area this app submits, per primitive family, per frame. It is not billed for the
    // wayland compositor's work and is not `glFinish`-serialised, which is what the GPU's
    // global FRAG_QUADS_RAST cannot say. `plxnative-drawmask=<classes>` REFUSES every draw of
    // the named classes, so a whole-frame `frame.ui` A/B against the unmasked control prices
    // that class as the frame sees it; `all` draws nothing and is therefore the compositor
    // floor. A masked leg is a broken picture on purpose.
    if !controlled {
        crate::dev::scenarios::arm_overdraw();
        crate::dev::scenarios::arm_drawmask();
        crate::dev::scenarios::arm_cardsplit();
    }
    // dev: /tmp/plxnative-heroground — draw the hero's photograph and BOTH of its scrim fields
    // in one pass instead of the art plus four blended gradient quads over it. Absent, the
    // shipped four-quad path draws, which is what makes this an A/B on one binary.
    if !controlled { crate::dev::scenarios::arm_heroground(); }
    // dev: /tmp/plxnative-nobudget — the frame budget's A/B CONTROL leg (spec §8.1). Read here
    // with the other boot triggers; applied to the one `Budget` below, once the tree exists.
    let nobudget = !controlled && crate::dev::scenarios::nobudget_armed();
    if !controlled { crate::dev::scenarios::arm_profile_hwcnt(); }
    // dev: /tmp/plxnative-cpuprof — the render thread's OWN time per phase, every phase at
    // once, no glFinish. The one mode that can see a frame the frame-drop detector reports as
    // all `draw=` and no `swap=`; the two GPU modes above are blind to it by construction.
    if !controlled { crate::dev::scenarios::arm_cpuprof(); }
    // dev: /tmp/plxnative-noidle turns the whole-frame present gate (plx_machine::idle) OFF, so a still
    // screen goes back to repainting at panel rate. It is a DIAG trigger (see the list above)
    // precisely so an A/B costs one file and does not also change which screen you boot to —
    // and so that if a frame ever looks wrong on the panel, ruling this feature out is one
    // `rm` rather than a redeploy.
    if !controlled {
        plx_ui::testpat::boot();
        plx_media::player::seed_dev_track_names();
    }
    if let Some(initial) = &initial {
        plx_machine::idle::set_enabled(!plx_base::devtrig::listed(&initial.triggers, "noidle"));
    } else { crate::dev::scenarios::arm_noidle(); }
    // dev: /tmp/plxnative-detailosc (read once at boot, like the other triggers) makes the detail scroll
    // perpetually swing hero<->bottom so the FPS heartbeat samples the transition, not the ends.
    let detail_osc = !controlled && crate::dev::scenarios::detailosc_armed();
    // dev: /tmp/plxnative-homeosc — perpetually sweep the home grid focus DOWN to the bottom then
    // UP to the top (~3s each way, one row per 350ms), so a headless run reproduces the top↔bottom
    // vertical-scroll judder for the frame-drop detector / retui profiler.
    let home_osc = !controlled && crate::dev::scenarios::homeosc_armed();
    let home_osc_last = 0u32;
    // dev: the two Home transition scenes the old home-hero/home-grid pair could not see.
    // `heroosc` continuously pages the real carousel; `homefoldosc` alternates the real
    // hero↔first-shelf snap. Their intervals overlap the spring lifetime so the FPS heartbeat
    // samples motion rather than the efficient idle gaps at either end.
    let hero_osc = !controlled && crate::dev::scenarios::heroosc_armed();
    let hero_osc_last = 0u32;
    let home_fold_osc = !controlled && crate::dev::scenarios::homefoldosc_armed();
    let home_fold_osc_last = 0u32;
    let home_fold_down = true;
    // dev: /tmp/plxnative-libosc — the Library twin of homeosc: sweep the browse grid focus
    // down↔up perpetually for the library_scroll FPS scene.
    let lib_osc = !controlled && crate::dev::scenarios::libosc_armed();
    let lib_osc_last = 0u32;
    // dev: /tmp/plxnative-libswitch — exercise EVERY Library switch on a timer (tab switch,
    // sort menu open/move/close, unwatched on/off, filter open/close) for the library_switch
    // FPS scene, so the re-query + popover paths are perf-gated, not just the scroll.
    let lib_switch = !controlled && crate::dev::scenarios::libswitch_armed();
    let lib_switch_last = 0u32;
    let lib_switch_step = 0u32;
    // dev: /tmp/plxnative-searchosc — the Search twin of homeosc/libosc: sweep the result
    // shelves' focus down↔up perpetually for the `fps:search-type` scene. It does NOT reach the
    // screen on its own — pair it with `/tmp/plxnative-search=<query>`, and with a query the
    // library actually matches, or there are no shelves to sweep and the scene grades nothing.
    let search_osc = !controlled && crate::dev::scenarios::searchosc_armed();
    let search_osc_last = 0u32;
    // dev: /tmp/plxnative-settings=<root|home|privacy|legal|playback|picker-quality> opens the Settings modal (and,
    // optionally, one of its real child panels) once Home is available. `settingsosc` turns
    // that settled modal into a continuous render-throughput scene: it alternates the focused
    // row and explicitly keeps the present gate awake. Without the latter an efficient,
    // completely healthy modal intentionally reports ~0 fps after its springs settle, which
    // cannot grade the screen's fill cost. The paired settings-idle scene omits the oscillator
    // and guards the inverse contract.
    let settings_boot = if controlled {
        initial.as_ref().and_then(|initial| initial.settings.clone())
    } else {
        crate::dev::scenarios::settings_boot_value()
    };
    let settings_osc = !controlled && crate::dev::scenarios::settingsosc_armed();
    let settings_osc_last = 0u32;
    let settings_osc_down = true;
    // dev: /tmp/plxnative-modalosc — with `plxnative-settings=root`, OPEN and DISMISS the
    // Settings modal every 1500 ms through the same `open`/`on_back` the chip and BACK use, so
    // `fps:modal-ramp` grades the appear/disappear RAMP (host snapshot, scrim, ground) under
    // `worst_ceiling_ms` rather than a settled modal. It reverses on a clock because the ramp
    // itself has no end the app reports.
    let modal_osc = !controlled && crate::dev::scenarios::modalosc_armed();
    let modal_osc_last = 0u32;
    // dev: /tmp/plxnative-legaldoc — with `plxnative-settings=legal`, press OK on the Legal
    // index ONCE so the boot lands on a pushed DOCUMENT (the reader over the frozen ground),
    // which no boot trigger reached before: `fps:legal-document`.
    let legal_doc = !controlled && crate::dev::scenarios::legaldoc_armed();
    let legal_doc_tried = false;
    // dev: /tmp/plxnative-alert — with `plxnative-settings=privacy`, open the "Delete all local
    // data?" DECISION ALERT once the privacy panel is up. It is the one shared yes/no alert in
    // the app and nothing headless could reach it: `fps:decision-alert`. Opening it is all this
    // does — nothing is deleted, and Cancel is what a BACK would press.
    let alert_boot = !controlled && crate::dev::scenarios::alert_armed();
    let alert_tried = false;
    // …and the DOWN presses that walk to the delete row before the OK that opens it. A count
    // rather than an index: the row is the LAST of the privacy table, whose length is that
    // screen's own business, and DOWN at the last row of a document with no action band is an
    // `Outcome::Edge` — i.e. idempotent — so any count comfortably past the table's length lands
    // exactly on it whatever the table becomes. One press per frame, for the reason every other
    // oscillator here steps at a human cadence: a whole table walked inside one frame would be a
    // slow frame, and this scene's gates (`worst_ceiling_ms`, `stall_ceiling_ms`) include warmup.
    let alert_step = 0u8;
    // The profile menu freezes its host and uses one cached backdrop. Drive the menu's own
    // TableView for a strict FPS scene; reusing `homeosc` would now correctly move nothing and
    // would grade the idle keepalive rather than the popover.
    let account_osc = !controlled && crate::dev::scenarios::acctosc_armed();
    let account_osc_last = 0u32;
    let account_osc_down = true;
    // First-run route oscillators keep their real focus models moving so the device FPS suite
    // grades the composition rather than a settled screen that correctly stops presenting.
    let consent_osc = !controlled && crate::dev::scenarios::consentosc_armed();
    let consent_osc_last = 0u32;
    let consent_osc_down = true;
    let onboard_osc = !controlled && crate::dev::scenarios::onboardosc_armed();
    let onboard_osc_last = 0u32;
    let onboard_osc_right = true;
    // dev: /tmp/plxnative-navosc — bounce the ROUTE on a timer, so the page cross-fade
    // (`ui::nav`) is FPS-gated like every other motion in the app. These are the only scenes
    // that change route, and therefore the only ones that sample a whole-screen cascade alpha
    // over both screens' full draw. 1400 ms matches `libswitch`: long enough that the ~225 ms
    // transition is measured against a settled screen on either side.
    //
    // EMPTY file = Home↔the first library section (the `home-library-nav` scene, whose two
    // pages share the top tab bar). A `<ratingKey>` = Home↔that item's DETAIL page instead
    // (`home-detail-nav`) — the arm phase 2 added, and a genuinely different cost: no shared
    // chrome, a hero backdrop and an ambient wash on the far side, and a real teardown at the
    // floor. Both bounce through the SAME `nav_open`/`nav_back` the interactive presses use, so
    // the scene measures the transition rather than an imitation of it.
    let nav_osc_rk = if controlled { None } else { crate::dev::scenarios::navosc_value() };
    let nav_osc = nav_osc_rk.is_some();
    let nav_osc_rk = nav_osc_rk.unwrap_or_default();
    let nav_osc_last = 0u32;

    // dev: /tmp/plxnative-pushbench[=<n>[,<ratingKey>]] — the counted, deterministic stress
    // benchmark twin of `navosc` (spec: `docs/agent-reference.md`'s fps-scene section). Its
    // Detail leg reuses `navosc`'s own ratingKey when the bench's own trigger carries none, so
    // `plxnative-navosc=<rk>` alone is enough to point both oscillators at the same item.
    let push_bench = (!controlled)
        .then(crate::dev::scenarios::pushbench_value)
        .flatten()
        .map(|(n, rk)| {
            let rk = if rk.is_empty() { nav_osc_rk.clone() } else { rk };
            crate::dev::scenarios::bench::PushBench::new(n, rk)
        });
    // dev: /tmp/plxnative-modalbench[=<n>[,<ratingKey>]] — the modal-ramp twin of the above, same
    // n,rk shape. Its item-menu leg reuses `navosc`'s ratingKey ONLY when the bench's own trigger
    // carries none, exactly like the push leg above — see `modalbench_value`'s doc for why a
    // scene that wants the item menu but not navosc's own competing bounce sets its own rk here
    // instead. See `ModalBench::new`'s doc for the two modal Styles it deliberately leaves out of
    // the rotation.
    let modal_bench = (!controlled)
        .then(crate::dev::scenarios::modalbench_value)
        .flatten()
        .map(|(n, rk)| {
            let rk = if rk.is_empty() { nav_osc_rk.clone() } else { rk };
            crate::dev::scenarios::bench::ModalBench::new(n, rk)
        });
    // dev: /tmp/plxnative-deepbench[=<depth>[,<ratingKey>]] — the DEEP-stack twin of the two
    // above: pushes `depth` pages with no pop in between (rotating Detail/Person — never Library,
    // see `bench::DeepBench::targets`'s doc), then pops all the way back to the root one page at a
    // time. Same empty-ratingKey resolution against `navosc`'s own value as the two legs above.
    let deep_bench = (!controlled)
        .then(crate::dev::scenarios::deepbench_value)
        .flatten()
        .map(|(depth, rk)| {
            let rk = if rk.is_empty() { nav_osc_rk.clone() } else { rk };
            crate::dev::scenarios::bench::DeepBench::new(depth, rk)
        });
    // dev: /tmp/plxnative-herobench[=<n>[,<period_ms>]] — flips the real Home hero carousel back and
    // forth over three titles (`bench::HeroBench`), one `bench:` line per two flips.
    let hero_bench = (!controlled)
        .then(crate::dev::scenarios::herobench_value)
        .flatten()
        .map(|(n, period, target)| crate::dev::scenarios::bench::HeroBench::new(n, period, target));
    // dev: /tmp/plxnative-holdbench[=<n>[,<period_ms>[,tap]]] — a REAL press-and-hold (or press-and-
    // cancel) on the focused card, cycle after cycle (`bench::PressBench`).
    let hold_bench = (!controlled)
        .then(crate::dev::scenarios::holdbench_value)
        .flatten()
        .map(|(n, period, kind)| crate::dev::scenarios::bench::PressBench::new(n, period, kind));

    // dev: /tmp/plxnative-framedrop — the FRAME-DROP DETECTOR. When present, each frame is timed with
    // the high-res perf counter (pump / draw / swap, NO glFinish so it doesn't perturb the pipeline),
    // and any frame whose total exceeds a threshold (ms; file content overrides the 22ms default) is
    // logged with its phase breakdown + GL texture-upload count — so a scroll judder shows *what* stalled
    // (high `pump`+`up` ⇒ synchronous poster uploads; high `swap` with low pump/draw ⇒ GPU fill).
    let framedrop = if controlled { None } else { crate::dev::scenarios::framedrop_value() };
    let framedrop_on = framedrop.is_some();
    let framedrop_thresh: f64 = framedrop
        .and_then(|s| s.parse().ok())
        .filter(|v: &f64| *v > 0.0)
        .unwrap_or(22.0);
    let mut instr = plx_base::diag::heartbeat::Instruments::new(framedrop_on, framedrop_thresh);
    if let Some(slow_ms) = crate::dev::scenarios::framering_ms().filter(|_| !controlled) {
        instr.arm_ring(slow_ms);
    }
    if framedrop_on {
        plx_base::diag::spans::arm();
    }

    let last_input = initial.as_ref().map_or_else(clock::now, |initial| initial.clock_start);
    let t0 = last_input;
    let loop_t = t0;
    let iters_ct = 0i32;
    let loop_shown = 0i32;
    // The dev number painted in the top-right corner. Unlike `loop_shown`, this is a real
    // presentation rate: the same completed-window value the heartbeat publishes as `fps=`.
    // It is updated only when that heartbeat drains PRESENTS, so pixels and logs cannot
    // disagree by observing two different counters. On a settled screen the number changes
    // only when the ordinary keepalive next buys a frame; the diagnostic must never defeat
    // the present gate merely to repaint itself.
    #[cfg(feature = "devtools")]
    let fps_shown = 0i32;
    // (media ns, SDL ticks) at the previous heartbeat, for `play=` below. `None` while
    // nothing is presenting, so the first beat of a playback reports no rate rather than a
    // fabricated one.
    let play_prev: Option<(i64, u32)> = None;
    let running = true;
    // Dev-only panel proof: advance a red/green counter phase only after SDL_GL_SwapWindow
    // returns. Hold each colour for 30 swaps: per-buffer alternation blends yellow at 60 Hz,
    // while this ~2 Hz change is human-visible and still freezes immediately with presentation.
    #[cfg(feature = "devtools")]
    let buffer_flip_count = 0u8;

    // All that is left of `HeldKey` on the loop's side (phase 10) — see `App::down_sym`.
    let down_sym = 0u32;
    // Item 13: rate-limits a hardware auto-repeat forwarded into the Settings family, which is
    // owned by the dispatcher since phase 5b — so the gate is applied at the loop's hand-over,
    // to the DIRECTIONS only. See `run`'s auto-repeat arm for why the OK edges go through
    // ungated, and `on_auto_repeat`'s doc for the one thing left on the legacy side (the
    // deferred press's liveness beat, and nothing else since phase 12).
    let modal_repeat = RepeatGate::IDLE;
    let marker_tried = false; // dev: the /tmp/plxnative-marker jump has been resolved
    let player = plx_media::player::machine::Player::new();
    // The token stops being an argument here and becomes a field. `PlayerAdapter::new` CONSUMES
    // it, so the adapter is the only thing in the process that holds one, and a `&mut` to it is
    // what every native-session call now asks for. See `player::adapter`.
    let adapters = super::Adapters {
        player: plx_media::player::adapter::PlayerAdapter::new(mt),
    };
    let repause_at = 0i64;
    // ui::press click state: a grid-card OK is deferred (press-in on down, activate on the
    // spring-back after key-up) so `ok_armed` marks "a press is in flight, commit it from the
    // per-frame loop when press::take_commit fires". Only ever set on Home's grid.
    let ok_armed = false;
    // Which route name was last REPORTED as an event. Not `route` itself: several `Route`
    // values share one name (every `Route::Player` is "player"), and an overlay
    // opening is not a screen change.
    let last_route_reported: &'static str = "";
    let press_tried = false; // dev: /tmp/plxnative-press fires one simulated grid-card press
    let press_release_at = 0u32; // …and the tick at which that simulated press releases
    let itemmenu_tried = false; // dev: /tmp/plxnative-itemmenu opens the card context menu once
    let acct_tried = false; // dev: /tmp/plxnative-acct opens the profile menu once
    let ptr = Pointer::IDLE;

    // Initial route from the boot gate: Login when we have no usable creds, Profiles for the
    // boot who's-watching picker, else Home.
    //
    // …and Home is intercepted by the first-run question when this profile has never been
    // asked it and the roster holds a source at all — the account holder or a managed profile, one
    // server or several (`plex::pins::asks`; the screen skips the one shape with nothing to choose,
    // `screens::onboard`). It belongs HERE as
    // well as on the login path, because a single-Plex-Home-user account never meets the
    // picker at all: the two paths into Home are the picker's `take_ready` and this gate, and
    // a question asked on only one of them is a question half the accounts never see.
    // `install_pms` above has already registered the stored roster, so "is there a source"
    // has a real answer by this line. An AUTOMATED boot is exempt for the reason the picker is
    // — a harness run must land on a deterministic Home.
    //
    // dev: `/tmp/plxnative-firstrun` forces it — a screen that is by definition asked once is
    // otherwise unreachable the moment you have answered it, and the two-source roster it
    // needs comes from `/tmp/plxnative-servers`, which marks the boot automated. Both halves
    // are why looking at this screen headlessly requires a trigger of its own.
    bridge.refresh_browse_directory();
    let ask_first_run = || !controlled && (crate::dev::scenarios::firstrun_armed()
        || (!automated_boot()
            && plx_data::stores::browse::onboard::asks(bridge.browse_directory())));
    // The sign-in's telemetry question is PRESENTED on the container tree, and the tree lives on
    // the `App` this function is still assembling — so this boot arm records that it owes the
    // question and `maybe_ask_consent` is called once the struct exists, a few dozen lines down.
    // Deferring it changes nothing about when it is ASKED: the surface would not have drawn until
    // the loop's first frame either way, and the route below is a page underneath it.
    let mut owes_consent_question = false;
    let route = match boot_to {
        // **Both Home arms ask, and the shared call is the point.** This is the one boot that
        // has no earlier hook — an install already signed in, either never asked or asked
        // against an older policy — and the sign-in's question has to come before every
        // per-profile step, the Home-sources wizard included. Asking only in the second arm
        // (which is what shipped for an hour) meant a stored session that still owed the
        // sources answer walked Onboard → Home and was never asked at all.
        BootTo::Home => {
            owes_consent_question = true;
            if ask_first_run() {
                log("boot: asking which sources feed Home");
                // No `enter()`: the first-run editor is an OWNED screen, and naming the route is
                // the whole of mounting it — `bridge`'s mounter builds `OnboardScreen::first_run`
                // when the tree follows this route on the loop's first NAV COMMIT.
                AppArg::Onboard
            } else {
                AppArg::Home
            }
        }
        // Both of these enter the `Route::Login | Route::Profiles` block below, which asks as
        // soon as the account is authorized — earlier than here, and before the picker.
        BootTo::Login => AppArg::Login,
        BootTo::Profiles => AppArg::Profiles,
    };
    // (dev: /tmp/plxnative-acct used to open the profile menu HERE, beside a
    // `route = Route::Account { over: BarHost::Home }`. The menu is a `ModalStack` surface since
    // phase 10 and the container does not exist yet at this point in the boot, so the trigger is
    // an ordinary per-frame arm — `dev::scenarios::acct_arm`, beside `itemmenu_arm`.)
    // Home is the product landing after the credential gates; its Hero / Continue Watching
    // rows own resume. Never override this route from an old last-page bookmark. The cleanup is
    // intentionally unconditional so automated and ordinary upgrades retire the same state.
    crate::coldstart::retire();
    // (`play_from`, the BACK trail and `nav_pending` were three run-loop locals here — the page
    // the live session returns to, the pages behind the one on screen, and the route change a fade
    // is carrying. All three are the container's since restructure phase 12: `PlayerScreen::origin`
    // is an `EntryId`, the `NavStack` IS the history, and a pending op lives on it under a
    // `PageDip`.)

    let auto_tried = false;
    // dev: `/tmp/plxnative-replay[=N]` — how many times a finished `plxnative-playurl`
    // playback may be started AGAIN (LG App Self Checklist #46, "replay after completion").
    //
    // A COUNTER re-arming `auto_tried`, rather than the latch being lifted: `auto_tried` also
    // guards the `autoplay`+`playidx` arm below, which does a `request_play_movie` +
    // `load_detail_now`, so an unconditionally re-armable latch would re-fetch a catalog item
    // on every player exit and loop a real playback forever. Bounded and opt-in instead — an
    // absent file is 0, which leaves every existing boot byte-identical, and every pipeline
    // case but the one that asks for a replay is untouched.
    //
    // Why the app needs this at all: the synthetic tier boots with NO Plex session, so after a
    // stream ends there is no detail page, no Play control and no key path back into the
    // player. Everything else was already in place — `teardown` clears the URL and `ended` on a
    // real stop, and `engine::start_bufferfeed` re-reads `player::playurl::playurl()` whenever
    // `route::url()` is empty — so a replay is a second trip through the entry below.
    let replay_left: u32 = if controlled { 0 } else { replay_budget(crate::dev::scenarios::replay_trigger_value().as_deref()) };
    let grid_tried = false;
    let settings_tried = settings_boot.is_none();
    let seek_tried = false;
    // /tmp/plxnative-autoseek seek script (see the parse site): pending steps, the tick of
    // the last fired step, the gap between steps, and the last REQUESTED target (the base
    // for "+10"/"-10" tap-relative steps, like taps on the HUD's frozen scrub playhead).
    let seek_script: Vec<String> = Vec::new();
    let seek_script_at = 0u32;
    let seek_gap_ms = 300u32;
    let seek_script_last = 0i64;
    // /tmp/plxnative-qualityswitch: the rungs still to switch to, the tick of the last one
    // fired, and the gap between them. Same shape as the seek script above, for the same
    // reason — a person changing quality mid-playback does it more than once.
    let quality_script: Vec<plx_plex::plex::session::PlaybackQuality> = Vec::new();
    let quality_script_at = 0u32;
    let quality_gap_ms = 0u32;
    let quality_tried = false;
    let quality_playing_since: Option<u32> = None;
    let detail_tried = false;
    let collection_tried = false;
    let play_tried = false;
    let menu_tried = false;
    let menupick_tried = false;
    let menupick_target = None;
    let pause_tried = false;
    // `/tmp/plxnative-autopause`: an authored Pause edge, plus the optional Resume edge which
    // owns the same script. External effects retry until the synchronized player state machine
    // accepts them; a busy native transition cannot silently consume the test operation.
    let pause_script: Option<(u32, Option<u32>, Option<u32>)> = None;
    let pause_resume_at: Option<u32> = None;
    let prev = 0u32;
    // Home data refresh, armed on every player exit (Stop/BACK/EOS): the hubs are refetched once
    // the stop report has settled (`route::timeline_stop_landed`), so the `stopped` timeline POST
    // lands first — Continue Watching then shows the new resume point / next episode instead of
    // the state from boot.
    let refresh_hubs = false;

    let ev = [0u8; 128];
    // dev/testing remote: drain any tokens written to /tmp/plxnative-remote and push
    // them as synthetic key events BEFORE the poll loop, so they're consumed this frame
    // by the ONE real key handler (see crate::remote / tools/stream-screen.py).
    let remote = crate::remote::Remote::open();
    // A LAB package may opt into the outbound long-poll command channel. Start only now: curl
    // has been initialised and, unlike the earlier boot/discovery work, the SDL loop below is
    // ready to dispatch a delivered command within one frame. Compile-time no-op otherwise.
    if !controlled { crate::lab::start_control(); }
    let mut app = App {
        last_input,
        loop_t,
        iters_ct,
        loop_shown,
        #[cfg(feature = "devtools")]
        fps_shown,
        play_prev,
        running,
        window_activity: super::window_activity::WindowActivity::new(),
        #[cfg(feature = "devtools")]
        buffer_flip_count,
        down_sym,
        modal_repeat,
        diagnostics: crate::app::diagnostics::Diagnostics::default(),
        player,
        adapters,
        repause_at,
        decoder_parked: false,
        ok_armed,
        last_route_reported,
        ptr,
        menu_play_await: None,
        deeplink: Default::default(),
        prev,
        refresh_hubs,
        return_hold: Default::default(),
        plaintext_upgrade: Default::default(),
        reread: Default::default(),
        ev,
        remote,
        win,
        #[cfg(all(feature = "hostsim", target_os = "linux"))]
        wslg_frame_pacing,
        t0,
        instr,
        measure_fault_logged: false,
        input: plx_ui::input::Input::new(),
        rec: super::recorder::Recplay::Off,
        boot_initial: initial,
        telemetry_guard: None,
        present: plx_machine::present::Present::new(),
        glass,
        // **The application's page stack runs the route DIP** (§6.2). It ran `Immediate` until
        // phase 12 while `ui::nav` held a second fader and the loop applied its own route change
        // at THAT floor; `PageDip` is the same schedule in the container that owns the op.
        pages,
        inputs: Vec::new(),
        bridge,
        // Every dev-trigger arm's own state (spec: `dev/scenarios.rs`'s module doc).
        scenarios: crate::dev::scenarios::Scenarios {
            #[cfg(feature = "devtriggers")]
            poster_gate: Default::default(),
            pick_user,
            home_osc_last,
            hero_osc_last,
            home_fold_osc_last,
            home_fold_down,
            lib_osc_last,
            lib_switch_last,
            lib_switch_step,
            search_osc_last,
            settings_osc_last,
            settings_osc_down,
            modal_osc_last,
            legal_doc_tried,
            alert_tried,
            alert_step,
            account_osc_last,
            account_osc_down,
            consent_osc_last,
            consent_osc_down,
            onboard_osc_last,
            onboard_osc_right,
            nav_osc_last,
            push_bench,
            modal_bench,
            deep_bench,
            hero_bench,
            hold_bench,
            marker_tried,
            press_tried,
            press_release_at,
            itemmenu_tried,
            acct_tried,
            acct_rest: None,
            shots: Default::default(),
            auto_tried,
            replay_left,
            grid_tried,
            settings_tried,
            seek_tried,
            seek_script,
            seek_script_at,
            seek_gap_ms,
            seek_script_last,
            quality_script,
            quality_script_at,
            quality_gap_ms,
            quality_tried,
            quality_playing_since,
            detail_tried,
            collection_tried,
            content_boot: None,
            play_tried,
            play_await: None,
            menu_tried,
            menupick_tried,
            menupick_target,
            subtiming: Default::default(),
            submenu_osc: Default::default(),
            more_osc: Default::default(),
            pause_tried,
            pause_script,
            pause_resume_at,
            dev: crate::dev::scenarios::DevFlags {
                detail_osc,
                home_osc,
                hero_osc,
                home_fold_osc,
                lib_osc,
                lib_switch,
                search_osc,
                settings_boot,
                settings_osc,
                modal_osc,
                legal_doc,
                alert_boot,
                account_osc,
                consent_osc,
                onboard_osc,
                nav_osc,
                nav_osc_rk,
                nobudget,
            },
        },
    };
    // dev: /tmp/plxnative-nobudget — put the ONE frame budget (the tree's, spec §2.2) into its
    // pre-phase-11 shape for the A/B's control leg. The tree exists now, which is why this is
    // here rather than beside the trigger read.
    //
    // The log line is gated on its own: the flag reaches here through a struct field, and the
    // optimizer does not always prove that field `false` in a release build, so the literal
    // shipped (`ci/check-package.py` failed on it). See `dev.rs`'s module doc.
    if app.scenarios.dev.nobudget {
        app.pages.budget = plx_ui::frame::Budget::pre_phase_11();
        #[cfg(feature = "devtriggers")]
        plx_base::eventlog::log("budget: pre-phase-11 admission (quota only) by /tmp/plxnative-nobudget");
    }
    // dev: /tmp/plxnative-watchlist-live — the owner's switch that lets a debug build write to the
    // real plex.tv Watchlist; latched here, and logged once when it is on.
    plx_plex::plex::watchlist::announce_live_writes();
    // dev: /tmp/plxnative-discover (or the plextv redirect) — resolve where Discover lives NOW, so
    // the event log says whether this boot's person and Watchlist reads reach a stand-in, the real
    // provider, or nothing, before any request is made.
    plx_plex::plex::account::announce_discover();
    if controlled {
        let initial = app.snapshot_init().expect("controlled constructor retains initial inputs");
        initial.validate().map_err(|reason| { log(&format!("rec: REFUSED — {reason}")); 1 })?;
        let replay = preflight.replay();
        app.rec = super::recorder::Recplay::controlled(preflight, &initial)
            .map_err(|reason| { log(&format!("rec: REFUSED — {reason}")); 1 })?;
        app.rec.arm_landgate(app.bridge.landgate());
        log("bootstrap: captured pre-effect initial state");
        if let Some(deferred) = deferred {
            apply_deferred_capture(&mut app.rec, app.bridge.landgate(), deferred)
                .map_err(|reason| { log(&format!("rec: REFUSED — {reason}")); 1 })?;
            log("bootstrap: captured session persistence applied");
        }
        if !replay {
            app.telemetry_guard = Some(plx_telemetry::telemetry::activate_initial(initial.consent.clone()));
        }
        if !replay { super::adapters::poster::init(); }
        app.rec.tick(initial.clock_start, 0.0);
        app.rec.prepare_resources(&mut app.bridge);
        if !matches!(activate_session(&mut app.bridge, &mut app.pages, &mut app.rec), BootTo::Home) {
            log("bootstrap: controlled Home activation refused");
            return Err(1);
        }
        let tree = app.pages.state_hash();
        app.rec.resource_requests(app.bridge.take_resource_requests());
        if let Some(reason) = app.rec.failure().or_else(|| app.bridge.controlled_failure()) {
            log(&format!("bootstrap: REFUSED — {reason}"));
            return Err(1);
        }
        super::run::recorder_end_frame(&mut app.rec, &app.bridge, &app.input.press,
            "home", "", "bootstrap", tree);
        if let Some(ms) = app.rec.clock_start() {
            super::clock::set_replay(ms);
            app.t0 = ms;
            app.loop_t = ms;
            app.last_input = ms;
        }
    }
    // **ROOT THE TREE AT THE BOOT PAGE.** The `route` chosen above used to be a field of `App`
    // that `bridge::sync_page` mirrored onto the container on the loop's first frame; there is no
    // mirror, so the boot says what it wants once, here, the moment the tree exists. A `Root` on
    // an empty stack mints the first entry, which is what makes this a hard CUT — there is no
    // outgoing screen to dip.
    if controlled {
        app.pages.emit(plx_machine::machine::MachineId::Nav,
            plx_machine::machine::Fx::Nav(plx_machine::machine::NavOp::Root(route)));
    } else {
        super::bridge::nav_root(&mut app.pages, route);
    }
    if controlled { log(&format!("bootstrap: root-request tree={:016x}", app.pages.state_hash())); }
    // …the deferred half of the `BootTo::Home` arm above: the tree exists now, so the question can
    // be presented. Idempotent and cheap (`should_show` is false once a decision is recorded and
    // on any automated boot), so the flag is the only thing carrying the decision forward.
    if owes_consent_question {
        if let Some(initial) = &app.boot_initial {
            // The same policy, evaluated over captured consent and automation inputs. A
            // recplay trigger alone intentionally does not grant live automation authority.
            // Preflight refuses the still-unsupported first-run surface before resource boot.
            if plx_screens::consent::should_show(&initial.consent, initial.automated) {
                log("bootstrap: REFUSED — unsupported initial consent route");
                return Err(1);
            }
        } else {
            maybe_ask_consent(&mut app.pages);
        }
    }
    Ok(app)
}
