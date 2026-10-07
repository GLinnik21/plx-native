//! **The frame dump** (the demo video's S5b): the simulator renders a storyboard from VIRTUAL time
//! and writes every frame it declares clean, as raw RGB24 for `ffmpeg -c:v ffv1 -level 3 -g 1`, one
//! xxh3 per frame into `frames.tsv`, and a summary into `dump.json`. `tools/site_video.py render`
//! is the launcher that owns the pipe, the pinned ffmpeg, the sentinel scan and `render.json`.
//! Simulator only: this file compiles under `hostsim` + `devtriggers` and nowhere else.
//!
//! **Armed by `PLXNATIVE_DUMP=<dir>`** (where `frames.tsv` and `dump.json` go), read once at the
//! first loop iteration. The rest, all optional:
//!
//! | variable | meaning |
//! |---|---|
//! | `PLXNATIVE_DUMP_FRAMES=<n>` | written frames to produce (default 180) |
//! | `PLXNATIVE_DUMP_PREROLL=<n>` | virtual frames run and NOT written first (default [`DEFAULT_PREROLL`], 60), so the boot settles and a hero's reveal can finish |
//! | `PLXNATIVE_DUMP_OUT=<path>` | where the RGB24 frames go: a file or a FIFO (unset: hash only, nothing is written) |
//! | `PLXNATIVE_DUMP_MAX_HOLD_MS=<ms>` | the fail-closed wall-clock bound on a stretch without progress (default 60000) |
//! | `PLXNATIVE_DUMP_NO_HOLD=1` | write every frame whatever the debt (a negative-test switch, never a render) |
//! | `PLXNATIVE_DUMP_ALLOW_ABSENT=1` | do not fail on a settled-absent hero logo or a never-resolving card (a proof switch) |
//!
//! **The clock.** `clock::set_replay(ORIGIN_MS + T(v))`, `T(v) = round(v * 1000 / 60)`, where `v` is
//! the virtual frame in flight. The origin is a constant far above any tick the boot stamped, so a
//! "how long since" sum reads a long time ago on every run instead of whatever the boot took.
//! `app.t0` and `app.prev` are set to it, so the first iteration's `dt` is 0 like every held one.
//!
//! **One iteration**, in the order `landgate` and `plx_gfx::dump` require: the landing takes (the
//! loop's own, which wait in dump mode), then [`pre_draw`] (the FULL text prewarm drain, then the
//! debt sample), then the draw, then [`after_draw`], which decides.
//!
//! **The image hold.** A frame is written only when nothing says the picture is still in flux:
//! no placeholder was drawn (`placeholder::take`), no upload is queued (`tex::has_pending`), the
//! poster source is idle (`tex::source_idle`), the page shown is not a held image
//! (`Dispatcher::held_page_image`) and no landing claim is open (`Bridge::open_claims`). Otherwise
//! there are two exits, and neither writes. While a placeholder was drawn, an upload is queued, the
//! poster source is busy or a claim is open, the iteration HOLDS: it repeats at the SAME `T(v)` (so
//! `dt` is 0) and `v` does not advance. If the only debt is a held page image, it ADVANCES: `v`
//! moves on without a write, because a frozen clock never releases that image (it ends by virtual
//! time) and holding on it would never finish; `dump.json` counts these as `advances`. **An
//! advance is a boot crutch and nothing else.** It is a cut: the held image is the whole page
//! transition, so skipping it deletes the fade and leaves a flat frame. It is therefore allowed
//! only inside the unwritten preroll (`v < preroll`, [`check_advance`]); at `v >= preroll` the run
//! FAILS CLOSED, naming the virtual frame. Residency is paint state, not logical state, so how long
//! a hold lasts is not meant to change a written frame; the run-twice comparison of `frames.tsv`
//! is the only check of that, and it has been shown for a still Home scene only.
//!
//! **What is and is not shown (read this before trusting a render).** Determinism is shown for a
//! STILL Home scene and its reveal only. A page push is UNSUPPORTED: it currently jump-cuts, and
//! the driver refuses it after the preroll. Any `down` on Home deadlocks on the card-admission
//! `dt == 0` gap (S5b-2). The Detail page is not yet deterministic: two runs differ by 1/255 in
//! the Resume button's box and the cause is under investigation. `Bridge::open_claims` covers
//! Hubs and Browse discovery only; the session-adapter drains are unprobed. No CI job builds
//! `site-video-sim` yet, so Linux is unverified, and a render from a Mac is a non-canonical
//! preview.
//!
//! **Fail closed, never fail soft.** A stretch of `PLXNATIVE_DUMP_MAX_HOLD_MS` without a virtual
//! frame completing ends the process (exit 3, after writing `dump.json`) naming the debt reasons,
//! the placeholder entries and the open claims. So does a hero logo that settled as a miss
//! (`placeholder::Frame::absent`), and a card that can never resolve (an empty key while nothing
//! else is pending), unless `ALLOW_ABSENT` says the proof accepts them. At the end the driver
//! asserts `Gate::unconverted_takes()` is empty. The exit is `_exit`, not `exit`: libc's `atexit`
//! handlers race the sign-in worker (`shot::maybe_capture`'s account of the same crash).
//!
//! **The seam S5c uses.** The storyboard interpreter belongs in [`iteration_begin`], after the clock is
//! set and before the loop ingests: it injects keys at a virtual frame (as `dev::scenarios` calls
//! `bridge::script_key`) and reads `after: "landed"` off [`Dump`]'s state (`holds_run == 0`, the last
//! sample clean, no open claim). It also owns `hero_pool.logged` and `opened_rating_keys`, which
//! `render.json` carries empty until then. Nothing here assumes the script is "hold".
//!
//! **What this PR does NOT do (S5b-2):** the dump overrides for card admission under `dt == 0`, the
//! poster retry backoff and evict cooldown under a held clock, `frame::Budget`'s cap, failing fast
//! on an image failure, and the negative tests with mock delays. S5c interprets the storyboard; here
//! the whole script is "hold the booted scene for N frames".

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{BufWriter, Write};
use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use plx_ui::placeholder::{self, Entry, Reason};

use crate::app::App;

/// The preroll when none is asked for: virtual frames run and not written first. Measured: Home's
/// boot spends about 10 `Advance`s on its first paint, so one second of virtual time covers it with
/// room; a `--preroll` too small to cover the boot makes the run fail closed ([`check_advance`]).
pub(crate) const DEFAULT_PREROLL: u32 = 60;
/// The frame rate every written frame is spaced at.
pub(crate) const FPS: u32 = 60;
/// The virtual clock's origin. See the module doc.
pub(crate) const ORIGIN_MS: u32 = 1_000_000;
/// The frame size `tools/site_video.py` requires.
const WIDTH: i32 = 1920;
const HEIGHT: i32 = 1080;

/// `T(v) = round(v * 1000 / 60)` in whole milliseconds. No tie exists (`1000 v mod 60` is never 30),
/// so integer rounding is exact.
pub(crate) fn virtual_ms(v: u64) -> u32 {
    ((v * 1000 + (FPS as u64) / 2) / FPS as u64) as u32
}

/// What one iteration observed about whether the picture is still in flux.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Sample {
    /// `placeholder::take().count`: placeholders the draw showed.
    pub placeholders: u32,
    /// `tex::has_pending()`: an upload or an eviction notice is queued.
    pub tex_pending: bool,
    /// `!tex::source_idle()`: the poster source has work out.
    pub source_busy: bool,
    /// `Dispatcher::held_page_image()` is `Some`: the page shown is a captured image.
    pub page_image: bool,
    /// Landing claims still open ([`crate::app::bridge::Bridge::open_claims`]).
    pub claims: Vec<&'static str>,
}

/// Why this frame must be held, in a fixed order; empty means the frame may be written.
pub(crate) fn hold_reasons(s: &Sample) -> Vec<String> {
    let mut why = Vec::new();
    if s.placeholders > 0 {
        why.push("placeholder".to_string());
    }
    if s.tex_pending {
        why.push("tex-pending".to_string());
    }
    if s.source_busy {
        why.push("source-busy".to_string());
    }
    if s.page_image {
        why.push("page-image".to_string());
    }
    for c in &s.claims {
        why.push(format!("claim:{c}"));
    }
    why
}

/// May virtual frame `v` take an [`Action::Advance`]? Only inside the unwritten preroll: an advance
/// is a cut, so one that reaches a written frame would put a hard cut into the master. `Err` is the
/// message the run fails with.
pub(crate) fn check_advance(v: u64, preroll: u32) -> Result<(), String> {
    if v < preroll as u64 {
        return Ok(());
    }
    Err(format!(
        "virtual frame {v} (preroll {preroll}): the only debt is a held page image, which would be \
         skipped as a cut; a page push is unsupported until S5b-2, and a boot slower than the preroll \
         needs a larger PLXNATIVE_DUMP_PREROLL"
    ))
}

/// What one iteration does with the frame it just drew.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Clean: read it back (after the preroll) and move to the next virtual frame.
    Write,
    /// Debt that only WAITING clears: repeat at the same `T(v)`, dt 0, write nothing.
    Hold,
    /// The only debt is a held page image, which ends by virtual TIME (`PAGE_QUIESCENCE_HOLD_MAX_MS`)
    /// or by quiescence, and neither can happen while the clock is frozen: a repeat at `dt == 0`
    /// would hold forever (measured: Home's first paint). Advance `v`, write nothing. The image's
    /// captured placeholders are re-counted on it, so `placeholders` is ignored here: they belong
    /// to the image, and the live draw that replaces it is judged afresh.
    Advance,
}

/// The decision, a pure function of the sample.
pub(crate) fn decide(s: &Sample) -> Action {
    let waiting = s.tex_pending || s.source_busy || !s.claims.is_empty();
    if s.page_image && !waiting {
        return Action::Advance;
    }
    if hold_reasons(s).is_empty() {
        Action::Write
    } else {
        Action::Hold
    }
}

/// An entry that will never resolve: a card with no picture key at all.
fn never_resolves(e: &Entry) -> bool {
    e.key.is_empty() && matches!(e.reason, Reason::CardSkeleton | Reason::CardGround)
}

/// `reason:key` for a message, deduplicated in draw order.
fn describe(entries: &[Entry]) -> String {
    let mut seen: Vec<String> = Vec::new();
    for e in entries {
        let s = format!("{}:{}", e.reason.name(), if e.key.is_empty() { "<empty>" } else { &e.key });
        if !seen.contains(&s) {
            seen.push(s);
        }
    }
    seen.join(", ")
}

/// One `frames.tsv` row, LF-terminated.
pub(crate) fn tsv_row(n: u32, t_ms: u32, hash: u64, debt: u32, holds: u32) -> String {
    format!("{n}\t{t_ms}\t{hash:016x}\t{debt}\t{holds}\n")
}

/// The header `tools/site_video.py` demands, byte for byte.
pub(crate) const TSV_HEADER: &str = "n\tt_ms\txxh3\tdebt\tholds\n";

/// JSON string escaping for the summary (only what a message can hold).
fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// What the run reports about itself, written as `dump.json`.
#[derive(Default)]
pub(crate) struct Summary {
    pub frames: u32,
    pub preroll: u32,
    pub iterations: u64,
    pub holds: u64,
    /// Virtual frames advanced and not written because only a page image was in the way.
    pub advances: u64,
    pub frames_with_debt: u32,
    pub hold_reasons: BTreeMap<String, u64>,
    pub unconverted_takes: Vec<(u32, u64)>,
    pub wall_ms: u128,
    pub width: i32,
    pub height: i32,
    pub failed: Option<String>,
}

impl Summary {
    pub(crate) fn to_json(&self) -> String {
        let reasons = self
            .hold_reasons
            .iter()
            .map(|(k, v)| format!("{}:{v}", json_str(k)))
            .collect::<Vec<_>>()
            .join(",");
        let unconverted = self
            .unconverted_takes
            .iter()
            .map(|(o, n)| format!("[{o},{n}]"))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"schema\":1,\"frames\":{},\"preroll\":{},\"iterations\":{},\"holds\":{},\"advances\":{},\
             \"frames_with_debt\":{},\"hold_reasons\":{{{reasons}}},\"unconverted_takes\":[{unconverted}],\
             \"clock_origin_ms\":{ORIGIN_MS},\"wall_ms\":{},\"width\":{},\"height\":{},\"failed\":{}}}\n",
            self.frames, self.preroll, self.iterations, self.holds, self.advances, self.frames_with_debt, self.wall_ms,
            self.width, self.height,
            self.failed.as_deref().map(json_str).unwrap_or_else(|| "null".to_string()),
        )
    }
}

struct Cfg {
    dir: std::path::PathBuf,
    frames: u32,
    preroll: u32,
    out: Option<std::path::PathBuf>,
    max_hold: Duration,
    no_hold: bool,
    allow_absent: bool,
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}

impl Cfg {
    fn from_env() -> Option<Cfg> {
        let dir = std::path::PathBuf::from(std::env::var_os("PLXNATIVE_DUMP")?);
        Some(Cfg {
            dir,
            frames: env_u32("PLXNATIVE_DUMP_FRAMES", 180),
            preroll: env_u32("PLXNATIVE_DUMP_PREROLL", DEFAULT_PREROLL),
            out: std::env::var_os("PLXNATIVE_DUMP_OUT").map(Into::into),
            max_hold: Duration::from_millis(env_u32("PLXNATIVE_DUMP_MAX_HOLD_MS", 60_000) as u64),
            no_hold: std::env::var_os("PLXNATIVE_DUMP_NO_HOLD").is_some(),
            allow_absent: std::env::var_os("PLXNATIVE_DUMP_ALLOW_ABSENT").is_some(),
        })
    }
}

/// Is the dump asked for? Read before SDL exists: the swap interval and the pacer depend on it.
pub(crate) fn requested() -> bool {
    static R: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *R.get_or_init(|| std::env::var_os("PLXNATIVE_DUMP").is_some())
}

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Is a dump running (armed at the first iteration)? The loop forces a present while it is.
pub(crate) fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

struct Dump {
    cfg: Cfg,
    /// The virtual frame in flight.
    v: u64,
    written: u32,
    /// Held repeats since the last virtual frame completed.
    holds_run: u32,
    started: Instant,
    last_progress: Instant,
    last_why: String,
    sink: Option<BufWriter<std::fs::File>>,
    tsv: BufWriter<std::fs::File>,
    sum: Summary,
    pre: Sample,
    done: bool,
}

thread_local! {
    static DUMP: RefCell<Option<Dump>> = const { RefCell::new(None) };
}

impl Dump {
    fn start(app: &mut App, cfg: Cfg) -> Dump {
        std::fs::create_dir_all(&cfg.dir).expect("framedump: cannot create PLXNATIVE_DUMP");
        let tsv = std::fs::File::create(cfg.dir.join("frames.tsv")).expect("framedump: cannot create frames.tsv");
        let mut tsv = BufWriter::new(tsv);
        tsv.write_all(TSV_HEADER.as_bytes()).expect("framedump: frames.tsv");
        // Opening a FIFO for writing blocks until the reader opens it: the launcher does.
        let sink = cfg.out.as_ref().map(|p| {
            BufWriter::with_capacity(
                1 << 20,
                std::fs::OpenOptions::new().write(true).open(p).expect("framedump: cannot open PLXNATIVE_DUMP_OUT"),
            )
        });
        plx_gfx::dump::arm();
        placeholder::arm();
        // The takes wait for their answers up to this long; the hold timeout is the real bound.
        app.bridge.landgate().arm_dump_strict(cfg.max_hold);
        app.t0 = ORIGIN_MS;
        app.prev = ORIGIN_MS;
        ACTIVE.store(true, Ordering::Relaxed);
        plx_base::eventlog::log(&format!(
            "framedump: armed, {} frames after {} preroll, out={:?}, no_hold={}, allow_absent={}",
            cfg.frames, cfg.preroll, cfg.out, cfg.no_hold, cfg.allow_absent
        ));
        let now = Instant::now();
        Dump {
            v: 0,
            written: 0,
            holds_run: 0,
            started: now,
            last_progress: now,
            last_why: String::new(),
            sink,
            tsv,
            sum: Summary { frames: cfg.frames, preroll: cfg.preroll, width: WIDTH, height: HEIGHT, ..Summary::default() },
            pre: Sample::default(),
            done: false,
            cfg,
        }
    }

    fn finish_summary(&mut self, app_gate_unconverted: Vec<(u32, u64)>) {
        self.sum.unconverted_takes = app_gate_unconverted;
        self.sum.wall_ms = self.started.elapsed().as_millis();
        let _ = self.tsv.flush();
        if let Some(s) = self.sink.as_mut() {
            let _ = s.flush();
        }
        let _ = std::fs::write(self.cfg.dir.join("dump.json"), self.sum.to_json());
    }

    /// End the process now, writing what is known. Never returns.
    fn fail(&mut self, why: String) -> ! {
        let msg = format!("framedump: FAILED at virtual frame {} (written {}): {why}", self.v, self.written);
        plx_base::eventlog::log(&msg);
        eprintln!("{msg}");
        self.sum.failed = Some(why);
        self.finish_summary(Vec::new());
        // SAFETY: `_exit` takes no memory of ours; see the module doc for why not `exit`.
        unsafe { libc::_exit(3) }
    }
}

/// Top of every loop iteration, before anything reads the clock. The first call arms the dump.
pub(crate) fn iteration_begin(app: &mut App) {
    DUMP.with(|cell| {
        let mut g = cell.borrow_mut();
        if g.is_none() {
            let Some(cfg) = Cfg::from_env() else { return };
            *g = Some(Dump::start(app, cfg));
        }
        let d = g.as_mut().expect("armed above");
        d.sum.iterations += 1;
        if d.last_progress.elapsed() > d.cfg.max_hold {
            let why = format!(
                "no virtual frame completed in {} ms; last hold reasons [{}]; open claims {:?}",
                d.cfg.max_hold.as_millis(), d.last_why, app.bridge.open_claims()
            );
            d.fail(why);
        }
        crate::app::clock::set_replay(ORIGIN_MS + virtual_ms(d.v));
        placeholder::reset();
    });
}

/// After the loop's takes and before the draw: the FULL text prewarm drain (the ui callers gate it
/// on wall time, so the driver does not rely on them), then the debt sample's pre-draw half.
/// Called on the presenting side, with the GL context current.
pub(crate) fn pre_draw(app: &App) {
    DUMP.with(|cell| {
        let mut g = cell.borrow_mut();
        let Some(d) = g.as_mut() else { return };
        // The budget is ignored in dump mode (`plx_gfx::dump`); a constant clock cannot spend one.
        plx_gfx::text::drain_prewarm(u64::MAX, || 0);
        plx_gfx::text::drain_background_prewarm(u64::MAX, || 0);
        plx_ui::panel_motion::PanelMotion::drain_queued_text(|| 0);
        d.pre = Sample {
            placeholders: 0,
            tex_pending: plx_ui::tex::has_pending(),
            source_busy: !plx_ui::tex::source_idle(),
            page_image: false,
            claims: app.bridge.open_claims(),
        };
    });
}

/// After the draw, at the seam `shot::maybe_capture` holds, before the swap: decide, and write.
/// Returns `true` when the run is complete and the loop must stop.
#[must_use]
pub(crate) fn after_draw(app: &App, vx: c_int, vy: c_int, vw: c_int, vh: c_int) -> bool {
    DUMP.with(|cell| {
        let mut g = cell.borrow_mut();
        let Some(d) = g.as_mut() else { return false };
        if d.done {
            return true;
        }
        let frame = placeholder::take();
        let post = Sample {
            placeholders: frame.count,
            tex_pending: d.pre.tex_pending || plx_ui::tex::has_pending(),
            source_busy: d.pre.source_busy || !plx_ui::tex::source_idle(),
            page_image: app.pages.held_page_image().is_some(),
            claims: {
                let mut c = d.pre.claims.clone();
                for n in app.bridge.open_claims() {
                    if !c.contains(&n) {
                        c.push(n);
                    }
                }
                c
            },
        };
        if !d.cfg.allow_absent && !frame.absent.is_empty() {
            let why = format!("a settled-absent placeholder is on screen (no clearLogo will arrive): {}", describe(&frame.absent));
            d.fail(why);
        }
        let why = hold_reasons(&post);
        let action = if d.cfg.no_hold { Action::Write } else { decide(&post) };
        if action == Action::Advance {
            if let Err(why) = check_advance(d.v, d.cfg.preroll) {
                d.fail(why);
            }
            d.sum.advances += 1;
            *d.sum.hold_reasons.entry("page-image(advance)".into()).or_default() += 1;
            d.last_why = format!("page-image(advance); entries [{}]", describe(&frame.entries));
            d.v += 1;
            d.holds_run = 0;
            d.last_progress = Instant::now();
            return false;
        }
        if action == Action::Hold {
            d.holds_run += 1;
            d.sum.holds += 1;
            for r in &why {
                *d.sum.hold_reasons.entry(r.clone()).or_default() += 1;
            }
            d.last_why = format!("{}; entries [{}]", why.join(","), describe(&frame.entries));
            let stuck_on_nothing = !post.tex_pending && !post.source_busy && !post.page_image
                && post.claims.is_empty() && !frame.entries.is_empty() && frame.entries.iter().all(never_resolves);
            if stuck_on_nothing && !d.cfg.allow_absent {
                let why = format!("a card that can never resolve (empty art key) and nothing else pending: {}", describe(&frame.entries));
                d.fail(why);
            }
            return false;
        }
        // The frame is clean: it counts as a virtual frame whether or not it is written.
        // `frames.tsv`'s `t_ms` is `T(n)` of the WRITTEN index, the output timeline, which is what
        // `tools/site_video.py` gates. After the preroll no advance can happen ([`check_advance`]),
        // so written frame `n` is virtual frame `preroll + n`.
        let t = virtual_ms(d.written as u64);
        if d.v >= d.cfg.preroll as u64 {
            if vw != WIDTH || vh != HEIGHT {
                let why = format!("the viewport is {vw}x{vh}, the master is {WIDTH}x{HEIGHT} (set PLXNATIVE_WIN=1920x1080)");
                d.fail(why);
            }
            let rgb = crate::shot::read_flipped(vx, vy, vw, vh, 3);
            let hash = xxhash_rust::xxh3::xxh3_64(&rgb);
            if let Some(s) = d.sink.as_mut() {
                if let Err(e) = s.write_all(&rgb) {
                    let why = format!("the frame sink refused frame {}: {e}", d.written);
                    d.fail(why);
                }
            }
            let row = tsv_row(d.written, t, hash, frame.count, d.holds_run);
            d.tsv.write_all(row.as_bytes()).expect("framedump: frames.tsv");
            if frame.count > 0 {
                d.sum.frames_with_debt += 1;
            }
            d.written += 1;
        }
        d.v += 1;
        d.holds_run = 0;
        d.last_progress = Instant::now();
        if d.written >= d.cfg.frames {
            let unconverted: Vec<(u32, u64)> =
                app.bridge.landgate().unconverted_takes().into_iter().map(|(o, n)| (o.0, n)).collect();
            if !unconverted.is_empty() {
                d.fail(format!("plain (unconverted) landing takes under dump mode: {unconverted:?}"));
            }
            d.finish_summary(unconverted);
            plx_base::eventlog::log(&format!(
                "framedump: done, {} frames, {} holds over {} iterations in {} ms",
                d.written, d.sum.holds, d.sum.iterations, d.sum.wall_ms
            ));
            d.done = true;
            return true;
        }
        false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t_of_n_is_the_rounded_sixtieth_of_a_second() {
        assert_eq!(virtual_ms(0), 0);
        assert_eq!(virtual_ms(1), 17);
        assert_eq!(virtual_ms(2), 33);
        assert_eq!(virtual_ms(3), 50);
        assert_eq!(virtual_ms(59), 983);
        assert_eq!(virtual_ms(60), 1000);
        assert_eq!(virtual_ms(600), 10_000);
    }

    #[test]
    fn t_of_n_matches_the_float_round_for_ten_minutes_of_frames() {
        for n in 0..36_000u64 {
            let want = ((n as f64) * 1000.0 / 60.0).round() as u32;
            assert_eq!(virtual_ms(n), want, "frame {n}");
        }
    }

    #[test]
    fn consecutive_frames_are_16_or_17_ms_apart() {
        for n in 0..3600u64 {
            let d = virtual_ms(n + 1) - virtual_ms(n);
            assert!(d == 16 || d == 17, "frame {n}: {d} ms");
        }
    }

    #[test]
    fn a_clean_sample_is_written() {
        assert!(hold_reasons(&Sample::default()).is_empty());
    }

    #[test]
    fn every_kind_of_debt_holds_and_names_itself() {
        let one = |s: Sample| hold_reasons(&s);
        assert_eq!(one(Sample { placeholders: 3, ..Default::default() }), ["placeholder"]);
        assert_eq!(one(Sample { tex_pending: true, ..Default::default() }), ["tex-pending"]);
        assert_eq!(one(Sample { source_busy: true, ..Default::default() }), ["source-busy"]);
        assert_eq!(one(Sample { page_image: true, ..Default::default() }), ["page-image"]);
        assert_eq!(one(Sample { claims: vec!["hubs"], ..Default::default() }), ["claim:hubs"]);
    }

    #[test]
    fn reasons_come_in_a_fixed_order_and_accumulate() {
        let s = Sample { placeholders: 1, tex_pending: true, source_busy: true, page_image: true, claims: vec!["hubs", "browse"] };
        assert_eq!(hold_reasons(&s), ["placeholder", "tex-pending", "source-busy", "page-image", "claim:hubs", "claim:browse"]);
    }

    #[test]
    fn debt_that_only_waiting_clears_holds_and_a_clean_sample_writes() {
        assert_eq!(decide(&Sample::default()), Action::Write);
        for s in [
            Sample { placeholders: 1, ..Default::default() },
            Sample { tex_pending: true, ..Default::default() },
            Sample { source_busy: true, ..Default::default() },
            Sample { claims: vec!["hubs"], ..Default::default() },
        ] {
            assert_eq!(decide(&s), Action::Hold, "{s:?}");
        }
    }

    #[test]
    fn a_held_page_image_alone_advances_time_because_a_frozen_clock_never_releases_it() {
        let image = Sample { page_image: true, ..Default::default() };
        assert_eq!(decide(&image), Action::Advance);
        // Its captured placeholders are the image's own, re-counted on it: still an advance.
        assert_eq!(decide(&Sample { placeholders: 7, ..image.clone() }), Action::Advance);
    }

    #[test]
    fn a_page_image_with_something_still_in_flight_holds_instead() {
        let image = Sample { page_image: true, ..Default::default() };
        assert_eq!(decide(&Sample { tex_pending: true, ..image.clone() }), Action::Hold);
        assert_eq!(decide(&Sample { source_busy: true, ..image.clone() }), Action::Hold);
        assert_eq!(decide(&Sample { claims: vec!["browse"], ..image }), Action::Hold);
    }

    #[test]
    fn an_advance_is_allowed_only_inside_the_preroll() {
        assert!(check_advance(0, 60).is_ok());
        assert!(check_advance(59, 60).is_ok());
        let at = check_advance(60, 60).unwrap_err();
        assert!(at.contains("virtual frame 60") && at.contains("unsupported until S5b-2"), "{at}");
        assert!(check_advance(61, 60).is_err());
        assert!(check_advance(500, 60).is_err());
        // With no preroll there is no unwritten stretch at all: even a boot-time advance fails.
        assert!(check_advance(0, 0).is_err());
    }

    #[test]
    fn the_default_preroll_covers_the_measured_boot_advances_with_room() {
        // Home's boot spends about 10 advances on its first paint; the default must be well past.
        assert!(DEFAULT_PREROLL >= 30);
        assert!(check_advance(10, DEFAULT_PREROLL).is_ok());
    }

    #[test]
    fn a_count_of_one_and_a_count_of_many_hold_alike() {
        let a = hold_reasons(&Sample { placeholders: 1, ..Default::default() });
        let b = hold_reasons(&Sample { placeholders: 9000, ..Default::default() });
        assert_eq!(a, b, "the count is zero/non-zero only");
    }

    #[test]
    fn only_a_card_with_no_key_never_resolves() {
        let e = |reason, key: &str| Entry { reason, key: key.into() };
        assert!(never_resolves(&e(Reason::CardSkeleton, "")));
        assert!(never_resolves(&e(Reason::CardGround, "")));
        assert!(!never_resolves(&e(Reason::CardSkeleton, "/library/metadata/1/thumb/9")));
        assert!(!never_resolves(&e(Reason::DetailSpinner, "")), "a spinner has no key and will clear");
        assert!(!never_resolves(&e(Reason::WorkingReadout, "")));
    }

    #[test]
    fn describe_deduplicates_and_marks_the_empty_key() {
        let e = |reason, key: &str| Entry { reason, key: key.into() };
        let s = describe(&[e(Reason::CardSkeleton, "a"), e(Reason::CardSkeleton, "a"), e(Reason::CardGround, "")]);
        assert_eq!(s, "card-skeleton:a, card-ground:<empty>");
    }

    #[test]
    fn a_tsv_row_is_the_contract_s_five_columns() {
        assert_eq!(tsv_row(7, 117, 0xabc, 0, 4), "7\t117\t0000000000000abc\t0\t4\n");
        assert_eq!(TSV_HEADER, "n\tt_ms\txxh3\tdebt\tholds\n");
    }

    #[test]
    fn the_summary_is_valid_json_with_the_failure_escaped() {
        let mut s = Summary { frames: 3, preroll: 1, width: 1920, height: 1080, ..Summary::default() };
        s.hold_reasons.insert("claim:hubs".into(), 2);
        s.unconverted_takes.push((1, 5));
        s.failed = Some("a \"quoted\"\nline".into());
        let j = s.to_json();
        assert!(j.contains("\"hold_reasons\":{\"claim:hubs\":2}"), "{j}");
        assert!(j.contains("\"unconverted_takes\":[[1,5]]"), "{j}");
        assert!(j.contains("\"failed\":\"a \\\"quoted\\\"\\nline\""), "{j}");
        assert!(j.ends_with("}\n"));
        let ok = Summary::default().to_json();
        assert!(ok.contains("\"failed\":null"));
    }
}
