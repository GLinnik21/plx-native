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
//! | `PLXNATIVE_DUMP_LAND_LAG_MS=<ms>` | how long, in VIRTUAL time, a request is out before its answer lands (default 0: the next execution of its site). A page whose read takes a second to answer is filmed with its loading frames by setting this to about that second; see "Landings" below |
//! | `PLXNATIVE_DUMP_NO_HOLD=1` | write every frame whatever the debt (a negative-test switch, never a render) |
//! | `PLXNATIVE_DUMP_ALLOW_ABSENT=1` | do not fail on a settled-absent hero logo, a never-resolving card or a failed art request (a proof switch) |
//! | `PLXNATIVE_DUMP_KEYS=<v>:<key>,...` | press `up`/`down`/`left`/`right`/`ok`/`back`: the down edge at the first iteration of virtual frame `<v>`, the up edge [`KEY_UP_AFTER`] frames later (the storyboard's compiled key schedule, `tests/video/feel.json`, arrives through this variable). `hold` is OK held for [`HOLD_UP_AFTER`] frames (the long press that opens an item's context menu) |
//! | `PLXNATIVE_DUMP_ALLOW_OVERLAY=1` | do not fail when a modal or popover is open. A MOTION-REVIEW switch: the underlay's dim field is cadenced by presented iterations, so such a frame is not guaranteed to be hold-independent (the hold-injection gate does not cover it) and the render is never a deliverable |
//! | `PLXNATIVE_DUMP_EXTRA_HOLDS=<k>` or `r<seed>` | force `k` (or a seeded 0..=3) EXTRA held repeats on every clean virtual frame: the hold-injection gate's switch |
//!
//! **The clock.** `clock::set_replay(ORIGIN_MS + T(v))`, `T(v) = round(v * 1000 / 60)`, where `v` is
//! the virtual frame in flight. The origin is a constant far above any tick the boot stamped, so a
//! "how long since" sum reads a long time ago on every run instead of whatever the boot took.
//! `app.t0` and `app.prev` are set to it, so the first iteration's `dt` is 0 like every held one.
//!
//! **One iteration**, in the order `landgate` and `plx_gfx::dump` require: the landing takes (the
//! loop's own, which never sleep in dump mode: see "Landings"), then [`pre_draw`] (the FULL text
//! prewarm drain, then the debt sample), then the draw, then [`after_draw`], which decides.
//!
//! **The hold rule.** A frame is written only when nothing says the picture is still in flux: no
//! placeholder was drawn (`placeholder::take`; bar a store's loading state while the landing lag holds its answer out, see "Landings"), no upload is queued (`tex::has_pending`), the poster
//! source is idle (`tex::source_idle`), no landing claim is open (`Bridge::open_claims`), no landing
//! take is waiting on an answer that is still out (`owed:<store>`, see "Landings") and the
//! dispatcher carries no work into its next iteration (`Dispatcher::work_carried`: a pressed key's
//! effect, such as a nav commit, lands one iteration after the press, so without this the frame an
//! effect shows on depends on how many iterations the frames before it took). Otherwise the
//! iteration HOLDS: it repeats at the SAME `T(v)` (so `dt` is 0) and `v` does not advance. There is
//! no other exit, and in particular no skipping of virtual time.
//!
//! **Settling.** A frame with none of that debt is still not written until a clean iteration at the
//! same `T(v)` reproduces the picture of the clean one before it (the hash of the read-back; hold
//! reason `settling`). Some state advances per ITERATION and not per millisecond (a first draw that
//! only measures, a layout that needs a second pass, an underlay latched before a late
//! landing), and no stepper audit finds all of it; running each frame to its fixed point makes what
//! is written independent of how many iterations came before it. The cost is one more iteration and
//! one more read-back per virtual frame. The product itself shows such a state for a few frames
//! first (measured: a cast headshot that appears 40 frames late behind a held page image); the dump
//! shows the settled picture of every frame.
//!
//! **A pushed page is filmed LIVE under the dip, never as a held image** (`plx_gfx::dump`'s one page
//! rule). The product paints a `PageDip` page as a captured image for the whole transition, and
//! keeps that image up past the dip while the destination's layout moves; a dump with that image
//! could only film a cut (the destination's springs start at its mount and run unseen under an image
//! captured at the floor, and the image is then replaced by the page already mid-flight), so in a
//! dump `PageImage::plan` re-captures the page on EVERY frame of the dip (`step_filmed`) and the
//! page is simply live when the dip ends. The capture is a live draw, so its placeholders are the
//! frame's debt like any other draw's ([`Sample::page_image`] is a recorded fact that stays false in
//! a dump): a capture frame with debt holds, and [`plx_gfx::dump::held_repeat`] makes `plan` return
//! the SAME paint on every repeat, so the page is re-captured until it is clean and only then
//! written. A push therefore films as 70 ms out over the live source page, two consecutive alpha-0
//! frames (the out ramp clamps to 0, then `DipPhase::Hold`, the same two the product shows), then
//! 140 ms in over the live destination, its entrance springs and its `Xfade` fade-in playing, then
//! the same page live. `PageDip::tick` likewise does not leave its one-frame `Hold` on a repeat.
//!
//! **Why a hold cannot change a written frame, and the gate that checks it.** Settling is the
//! guarantee; the rest keeps the fixed point the one the product reaches. Per-iteration steppers were
//! made to count virtual frames: the ground probes are synchronous per call (`plx_gfx::dump`), the
//! page image and dip step once per virtual frame (above), card art admission never declines for
//! motion (`card_motion::declines_request`), the upload budget admits on its quotas alone
//! (`frame::Budget::take`), the poster store has no eviction cooldown and records a failed fetch
//! instead of waiting out a retry on a clock that is held still, and a held repeat judges whether a
//! spring is at rest (`idle::settled`, which multiplies by `dt`) with the `dt` of the virtual
//! frame's first pass ([`idle_dt`]), so `page_quiescent` does not flip on `dt == 0`. The springs a
//! pushed page starts are therefore stepped once per virtual frame and written as they move, never
//! on a wall-clock event. The check is `PLXNATIVE_DUMP_EXTRA_HOLDS`:
//! `tools/site_video.py hold-gate` renders the scene with no extra holds and with seeded extra holds
//! on every frame and requires `frames.tsv` columns 1-4 equal.
//!
//! **What is and is not shown (read this before trusting a render).** Determinism is shown for the
//! scenes the gate was run on: Home held, then `right`, `down`, `ok` (a push to Detail), a four second
//! hold and `back` (the pop), run twice, under CPU load and with seeded extra holds (columns 1-4 of
//! `frames.tsv` and the master identical), the site film (`tests/video/feel.json`: hero paging, a
//! Detail from a shelf card, the Movies library; `hold-gate --storyboard`) and a Detail page booted at a rating key; a scene with a modal
//! or popover is refused (its dim field is still cadenced: see `after_draw`). `Bridge::open_claims`
//! covers Hubs and Browse discovery; for the stores and the session adapter see "Claims" below. The Linux
//! llvmpipe render (`.github/workflows/site-video.yml`: run twice and hold-gated per dispatch) is the canonical one; a
//! render from a Mac is a non-canonical preview.
//!
//! **Landings.** The loop's landing takes (`landgate::Gate::take_owed` and `take_all_owed`) never
//! sleep in a dump: a sleep on the frame thread is what `plx_base::task::assert_may_block` refuses,
//! and a page whose read took longer than a frame held the loop on one sleeping take, so the
//! 2 s hang watchdog aborted the dump and the page's loading frames were never drawn. The driver arms
//! the PACED form (`Gate::arm_dump_paced`) instead. A take whose answer is still out returns nothing
//! and names its store (`Gate::take_starved`); the iteration then HOLDS on `owed:<store>` like on any
//! other debt (it repeats at the same `T(v)`, sleeping [`PACE_SLEEP_US`] between repeats so a worker
//! can run) and the next repeat takes again. The landing frame is therefore still a function of the
//! request and not of the worker, because the picture is never written while a take is starved.
//! `Fetch` stores need no `open_claims` entry for this, and Home's hubs and the Browse discovery
//! (which have one) are held by the claim as before.
//!
//! **The landing lag** (`PLXNATIVE_DUMP_LAND_LAG_MS`, off by default) is how a slow page is filmed
//! with the frames its user would see. Waiting for an answer, however it is done, writes no frame
//! while it is out, so a page whose read takes a second would show the loading state for one frame
//! and then the loaded page. With a lag of `L` virtual frames the gate holds an answer back until
//! `L` frames after the frame its request was first seen open (`Gate::dump_frame` is the driver's
//! clock for it), whatever the worker did meanwhile, and the stores it is holding back are not debt
//! (`Gate::take_lagged`), so those `L` frames are drawn, held only by what else they owe, and written:
//! the loading state, for the time the dump chose and not the network. The landing frame is the
//! request's frame plus `L`: deterministic, and equal for a fast worker and a slow one (a worker
//! slower than `L` frames holds the picture on `owed:<store>` at that frame). With no lag a take lands
//! on the next execution of its site, which is what every storyboard's rests were tuned on.
//!
//! **A lagged page's loading placeholders are not debt either** ([`owed_placeholders`]). The page of a
//! lagged store draws its loading state (the Working read-out, a spinner, the Detail's "Loading"
//! title: `Reason::is_loading_state`), and virtual time advances only when a frame is written, so
//! holding on that placeholder would never reach the frame the lag waits for and the run would end
//! on `PLXNATIVE_DUMP_MAX_HOLD_MS` naming `placeholder`. While a store is lagged this iteration
//! those placeholders, and only those, are excused (`Entry::is_loading_state`: the loading reasons, and a
//! card tile whose key is EMPTY, the Collection header's thumb before the collection lands, which no
//! poster fetch can answer); a card's art with a key, a logo or a backdrop still hold, and so does any
//! draw past the entry cap. They are still COUNTED where it matters: the written
//! frame's `debt` column and `frames_with_debt` record the loading frames the film holds, so
//! `site_video.py gates` rejects a lagged render as a deliverable, which is right: it is a capture
//! of a loading state, never the demo. With no lag armed nothing is excused and the "no placeholders
//! in a demo video" rule holds as written.
//!
//! **Claims.** Session-adapter drains and stores outside `open_claims` cannot change what is drawn
//! without a placeholder or a claim: every request-driven landing either paints a placeholder until
//! it lands (card art, spinners, logos, the Detail spinner), or is a landing take that holds the
//! picture itself (above). Anything that does not is a gap no predicate covers: the storyboard fixes
//! its rests in frames, and `site_video.py gates` measures whether each beat had come to rest by the
//! next key (`storyboard/rest`).
//!
//! **Fail closed, never fail soft.** A stretch of `PLXNATIVE_DUMP_MAX_HOLD_MS` without a virtual
//! frame completing ends the process (exit 3, after writing `dump.json`) naming the debt reasons,
//! the placeholder entries and the open claims. So does a hero logo that settled as a miss
//! (`placeholder::Frame::absent`), a card that can never resolve (an empty key while nothing else
//! is pending) and an art request that FAILED (a 404, bytes that do not decode, a refused connect:
//! the card would hold on its placeholder to the timeout; the run ends at once naming the key),
//! unless `ALLOW_ABSENT` says the proof accepts them. So does a modal or popover opening, unless `ALLOW_OVERLAY` (`site_video.py render --allow-overlay`)
//! marks a motion review, which is never a deliverable; so does any
//! PANIC on the loop's thread (a stretch that never gets its answer is the `max_hold` exit above,
//! and its message names `owed:<store>`): a panic hook writes `dump.json` with the message and exits 3, so a failed run always leaves a
//! valid summary. The launcher renames a failed run's master to `master.partial.mkv`. At the end the driver
//! asserts `Gate::unconverted_takes()` is empty. The exit is `_exit`, not `exit`: libc's `atexit`
//! handlers race the sign-in worker (`shot::maybe_capture`'s account of the same crash).
//!
//! **The storyboard** is `tests/video/feel.json` (positions and durations, never titles), compiled by
//! `tools/site_video.py` (`compile_storyboard`) to `PLXNATIVE_DUMP_KEYS` and a frame count. The key
//! schedule is the driver's whole seam: this file injects a key at a virtual frame in [`iteration_begin`]
//! (after the clock is set, before the loop ingests, as `dev::scenarios` calls `bridge::script_key`) and
//! knows nothing of beats. There is no `landed` predicate: a rest is a fixed number of frames, and the
//! launcher decodes the master afterwards and records in `render.json` (`storyboard.rest`) how far each
//! beat's last 0.2 s still moved. The launcher also records what the film showed: `hero_pool` (every
//! hero page the film showed, read from the app's event log: the pinned page's `home: hero pinned at
//! slot 0` line plus one `home: hero page slot N rk=R` per page turn, checked by `gates` against the
//! `demo_library.py hero-report` pass set and `not_in_video`) and `opened_rating_keys` (every title page opened, read from the mock's request log), which `gates`
//! checks against the catalog's `not_in_video` flag. A storyboard render also writes the
//! `plxnative-hintoff` trigger (`app::boot` teaches the hold hint on every screen, a one-time
//! teaching overlay a film has no viewer for) and records `hold_hint: suppressed`. Nothing here assumes
//! the script is "hold".
//!
//! **What this does NOT do:** the negative tests with mock delays. The script is `PLXNATIVE_DUMP_KEYS` or
//! "hold the booted scene for N frames".

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{BufWriter, Write};
use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use plx_machine::machine::{Key, Tick};
use plx_ui::placeholder::{self, Entry, Frame, Reason};

use crate::app::App;

/// Virtual frames between a scripted key's down edge and its up edge: 100 ms, a tap.
const KEY_UP_AFTER: u64 = 6;

/// Virtual frames a scripted `hold` keeps OK down: 750 ms, past the long-press threshold
/// (`ui::press::LONG_MS`, 500 ms) and under the hold ceiling (1000 ms), so the item context menu
/// opens while the key is still down and the release then springs the press back.
const HOLD_UP_AFTER: u64 = 45;

/// The preroll when none is asked for: virtual frames run and not written first, so the boot's
/// first paint settles and a hero's reveal (a spring of well under a second) finishes before the
/// first written frame. It is a choice of what the film starts on, not a safety bound: nothing fails
/// when it is short, the early frames just show the boot (a preroll of 0 writes Home's first paint,
/// held until it is clean, as frame 0).
pub(crate) const DEFAULT_PREROLL: u32 = 60;
/// The frame rate every written frame is spaced at.
pub(crate) const FPS: u32 = 60;
/// How long a held iteration sleeps when a landing take is still waiting on its worker: long enough
/// that the wait does not burn a core, far under a frame, and nowhere near the hang watchdog's 2 s.
const PACE_SLEEP_US: u64 = 500;
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

/// `PLXNATIVE_DUMP_LAND_LAG_MS` as virtual frames, rounded UP so a lag never lands before it was asked.
pub(crate) fn land_lag_frames(ms: u32) -> u64 {
    (ms as u64 * FPS as u64).div_ceil(1000)
}

/// What one iteration observed about whether the picture is still in flux.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Sample {
    /// Placeholders the draw showed that are DEBT ([`owed_placeholders`]: `placeholder::take().count`,
    /// less a loading state the landing lag is deliberately holding out).
    pub placeholders: u32,
    /// `tex::has_pending()`: an upload or an eviction notice is queued.
    pub tex_pending: bool,
    /// `!tex::source_idle()`: the poster source has work out.
    pub source_busy: bool,
    /// `Dispatcher::held_page_image()` is `Some`: the page shown is a captured image. A RECORDED FACT,
    /// never a hold reason: see the module doc.
    pub page_image: bool,
    /// Landing claims still open ([`crate::app::bridge::Bridge::open_claims`]).
    pub claims: Vec<&'static str>,
    /// The dispatcher still carries work into its next iteration (`Dispatcher::work_carried`).
    pub carried: bool,
    /// Landing takes whose answer is still out (`Gate::take_starved`), by store name.
    pub starved: Vec<String>,
}

impl Sample {
    /// The whole iteration's sample: `self` is the pre-draw half (the takes and the text drain have
    /// run), `after` what was observed once the draw was done (its `placeholders` field is ignored:
    /// the draw's [`Frame`] is the count, read through [`owed_placeholders`]), and `lagged` the stores
    /// the landing lag held back this iteration. The one place the driver combines them, so a test
    /// of the hold rule runs the code the driver runs.
    pub(crate) fn with_draw(&self, after: Sample, frame: &Frame, lagged: &[String]) -> Sample {
        let mut claims = self.claims.clone();
        for c in after.claims {
            if !claims.contains(&c) {
                claims.push(c);
            }
        }
        let mut starved = self.starved.clone();
        for n in after.starved {
            if !starved.contains(&n) {
                starved.push(n);
            }
        }
        Sample {
            placeholders: owed_placeholders(frame, lagged),
            tex_pending: self.tex_pending || after.tex_pending,
            source_busy: self.source_busy || after.source_busy,
            page_image: after.page_image,
            claims,
            carried: after.carried,
            starved,
        }
    }
}

/// The placeholders a draw shows that are DEBT. All of them, unless the landing lag is holding a
/// store's answer back this iteration (`lagged` non-empty): the page is then deliberately showing
/// its loading state, so the placeholders that ARE a loading state ([`Frame::loading_state`]) are the
/// picture the dump was asked to film and not something to wait out. Every other placeholder
/// (a card's art, a logo, a backdrop) stays debt, and so does a draw past the entry cap, whose
/// reason is unknown. With no lag armed `lagged` is always empty, so no placeholder is ever excused
/// and the "no placeholders in a demo video" rule holds as written.
pub(crate) fn owed_placeholders(frame: &Frame, lagged: &[String]) -> u32 {
    if lagged.is_empty() {
        frame.count
    } else {
        frame.count - frame.loading_state()
    }
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
    for c in &s.claims {
        why.push(format!("claim:{c}"));
    }
    if s.carried {
        why.push("carried-work".to_string());
    }
    for n in &s.starved {
        why.push(format!("owed:{n}"));
    }
    why
}

/// What one iteration does with the frame it just drew.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Clean: read it back (after the preroll) and move to the next virtual frame.
    Write,
    /// Debt that only WAITING clears: repeat at the same `T(v)`, dt 0, write nothing.
    Hold,
}

/// The decision, a pure function of the sample.
pub(crate) fn decide(s: &Sample) -> Action {
    if hold_reasons(s).is_empty() {
        Action::Write
    } else {
        Action::Hold
    }
}

/// How many EXTRA held repeats a clean virtual frame gets (the hold-injection gate).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExtraHolds {
    /// `<k>`: exactly `k` on every frame.
    Fixed(u32),
    /// `r<seed>`: 0..=3, a pure function of the seed and the virtual frame.
    Seeded(u64),
}

impl ExtraHolds {
    pub(crate) fn parse(spec: &str) -> Result<ExtraHolds, String> {
        if let Some(seed) = spec.strip_prefix('r') {
            seed.parse().map(ExtraHolds::Seeded).map_err(|_| format!("PLXNATIVE_DUMP_EXTRA_HOLDS={spec:?}: want <k> or r<seed>"))
        } else {
            spec.parse().map(ExtraHolds::Fixed).map_err(|_| format!("PLXNATIVE_DUMP_EXTRA_HOLDS={spec:?}: want <k> or r<seed>"))
        }
    }

    pub(crate) fn for_frame(self, v: u64) -> u32 {
        match self {
            ExtraHolds::Fixed(k) => k,
            ExtraHolds::Seeded(seed) => {
                // splitmix64 over (seed, v): a fixed pure mix, so a seed names one injection pattern.
                let mut x = v.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ seed.wrapping_mul(0xD1B5_4A32_D192_ED03);
                x ^= x >> 30;
                x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
                x ^= x >> 27;
                x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
                x ^= x >> 31;
                (x % 4) as u32
            }
        }
    }
}

/// `PLXNATIVE_DUMP_KEYS`: `<v>:<key>,...` sorted by frame (stable, so two keys on one frame keep
/// their order).
pub(crate) fn parse_keys(spec: &str) -> Result<Vec<(u64, Key, u64)>, String> {
    let mut out = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (v, name) = part.split_once(':').ok_or_else(|| format!("PLXNATIVE_DUMP_KEYS: {part:?} is not <frame>:<key>"))?;
        let v = v.parse::<u64>().map_err(|_| format!("PLXNATIVE_DUMP_KEYS: {part:?}: the frame is not a number"))?;
        let mut up_after = KEY_UP_AFTER;
        let key = match name {
            "hold" => {
                up_after = HOLD_UP_AFTER;
                Key::Ok
            }
            "up" => Key::Up,
            "down" => Key::Down,
            "left" => Key::Left,
            "right" => Key::Right,
            "ok" => Key::Ok,
            "back" => Key::Back,
            other => return Err(format!("PLXNATIVE_DUMP_KEYS: {other:?} is not up/down/left/right/ok/back/hold")),
        };
        out.push((v, key, up_after));
    }
    out.sort_by_key(|&(v, _, _)| v);
    Ok(out)
}

/// A scripted key that cannot be pressed AND released inside the run is an error at start, not a
/// key that silently never happens: the run is `preroll + frames` virtual frames (the last is
/// `preroll + frames - 1`), the down edge is on frame `v` and the up edge on `v + KEY_UP_AFTER`.
pub(crate) fn check_key_schedule(keys: &[(u64, Key, u64)], preroll: u32, frames: u32) -> Result<(), String> {
    let total = preroll as u64 + frames as u64;
    for &(v, key, up_after) in keys {
        if v + up_after >= total {
            return Err(format!(
                "PLXNATIVE_DUMP_KEYS: {key:?} at virtual frame {v} cannot come up before the run ends \
                 (its up edge is frame {}, the last frame is {}: {preroll} preroll + {frames} frames)",
                v + up_after, total.saturating_sub(1)
            ));
        }
    }
    Ok(())
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
            "{{\"schema\":1,\"frames\":{},\"preroll\":{},\"iterations\":{},\"holds\":{},\
             \"frames_with_debt\":{},\"hold_reasons\":{{{reasons}}},\"unconverted_takes\":[{unconverted}],\
             \"clock_origin_ms\":{ORIGIN_MS},\"wall_ms\":{},\"width\":{},\"height\":{},\"failed\":{}}}\n",
            self.frames, self.preroll, self.iterations, self.holds, self.frames_with_debt, self.wall_ms,
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
    /// Virtual frames a landing is held back after its request ([`land_lag_frames`]).
    land_lag: u64,
    no_hold: bool,
    allow_absent: bool,
    keys: Vec<(u64, Key, u64)>,
    allow_overlay: bool,
    extra_holds: ExtraHolds,
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}

impl Cfg {
    fn from_env() -> Option<Cfg> {
        let dir = std::path::PathBuf::from(std::env::var_os("PLXNATIVE_DUMP")?);
        let cfg = Cfg {
            dir,
            frames: env_u32("PLXNATIVE_DUMP_FRAMES", 180),
            preroll: env_u32("PLXNATIVE_DUMP_PREROLL", DEFAULT_PREROLL),
            out: std::env::var_os("PLXNATIVE_DUMP_OUT").map(Into::into),
            max_hold: Duration::from_millis(env_u32("PLXNATIVE_DUMP_MAX_HOLD_MS", 60_000) as u64),
            land_lag: land_lag_frames(env_u32("PLXNATIVE_DUMP_LAND_LAG_MS", 0)),
            no_hold: std::env::var_os("PLXNATIVE_DUMP_NO_HOLD").is_some(),
            allow_absent: std::env::var_os("PLXNATIVE_DUMP_ALLOW_ABSENT").is_some(),
            allow_overlay: std::env::var_os("PLXNATIVE_DUMP_ALLOW_OVERLAY").is_some(),
            keys: std::env::var("PLXNATIVE_DUMP_KEYS").map_or_else(|_| Ok(Vec::new()), |s| parse_keys(&s)).unwrap_or_else(|e| panic!("framedump: {e}")),
            extra_holds: std::env::var("PLXNATIVE_DUMP_EXTRA_HOLDS")
                .map_or(Ok(ExtraHolds::Fixed(0)), |s| ExtraHolds::parse(&s))
                .unwrap_or_else(|e| panic!("framedump: {e}")),
        };
        check_key_schedule(&cfg.keys, cfg.preroll, cfg.frames).unwrap_or_else(|e| panic!("framedump: {e}"));
        Some(cfg)
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

thread_local! {
    /// The `dt` the idle gate judged the last NEW virtual frame with.
    static IDLE_DT: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
}

/// The `dt` the idle gate's rest test ([`plx_machine::idle::frame_begin_judging_rest_with`]) uses this iteration.
///
/// A spring is "at rest" when its position is near its target AND `velocity * dt` is under the
/// rest threshold (`idle::settled`), so a held repeat at `dt == 0` reads every spring that is
/// still creeping toward its target as settled and flips `page_quiescent`, whose answer decides the
/// `at_rest` latch of the page's underlay (a dump never holds a page image, so it no longer picks
/// between an image and the live page). Left alone, the frame a run writes would depend on whether
/// a repeat followed the first pass of its virtual frame. A
/// repeat therefore judges rest with the `dt` of the first pass: the springs themselves still
/// stand still (`fr.dt == 0`), only the verdict on them is repeated, so it is the same on every
/// iteration of a virtual frame. Outside a dump this is not called.
pub(crate) fn idle_dt(dt: f32) -> f32 {
    if plx_gfx::dump::held_repeat() {
        IDLE_DT.with(std::cell::Cell::get)
    } else {
        IDLE_DT.with(|c| c.set(dt));
        dt
    }
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
    /// The stores whose answer this iteration's takes held back for the lag: their claims are not
    /// debt ([`Gate::take_lagged`](plx_machine::landgate::Gate::take_lagged)).
    lagged: Vec<String>,
    done: bool,
    /// Index into `cfg.keys` of the next key to press.
    keys_next: usize,
    /// Key releases owed: (virtual frame to deliver on, key), in due order.
    ups: Vec<(u64, Key)>, // (due virtual frame, key), kept sorted by due
    /// The picture of the last CLEAN iteration of this virtual frame (pixels, hash): a frame is
    /// written once a clean iteration reproduces it ([`Action::Hold`], "settling").
    settle: Option<(Vec<u8>, u64)>,
    /// Extra holds spent on this virtual frame (the hold-injection gate).
    extras_run: u32,
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
        install_panic_hook();
        placeholder::arm();
        // The takes never sleep: a take whose answer is out holds the iteration, and the hold
        // timeout is the bound on how long (the module doc's "Landings").
        app.bridge.landgate().arm_dump_paced(cfg.land_lag);
        app.t0 = ORIGIN_MS;
        app.prev = ORIGIN_MS;
        ACTIVE.store(true, Ordering::Relaxed);
        plx_base::eventlog::log(&format!(
            "framedump: armed, {} frames after {} preroll, out={:?}, no_hold={}, allow_absent={}, land_lag={} frames",
            cfg.frames, cfg.preroll, cfg.out, cfg.no_hold, cfg.allow_absent, cfg.land_lag
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
            lagged: Vec::new(),
            done: false,
            keys_next: 0,
            ups: Vec::new(),
            settle: None,
            extras_run: 0,
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
        // `exit_now`, not `exit`: see the module doc.
        plx_base::process::exit_now(3)
    }
}

/// A panic on the loop's thread ends the run through [`Dump::fail`]: `dump.json` carries the message
/// (a landgate take names the mailbox it was owed) and the frame sink is flushed, instead of the
/// process dying with only a log line. A panic while the dump is already borrowed (inside
/// `after_draw`) falls through to the previous hook.
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = DUMP.try_with(|cell| {
            if let Ok(mut g) = cell.try_borrow_mut() {
                if let Some(d) = g.as_mut() {
                    d.fail(format!("panic: {info}"));
                }
            }
        });
        prev(info);
    }));
}

/// `Bridge::open_claims` without the stores the landing lag is deliberately holding back this
/// iteration: their request is out on purpose, and holding the picture for it would stop the
/// virtual frames the lag counts.
fn open_claims_but(app: &App, lagged: &[String]) -> Vec<&'static str> {
    let mut claims = app.bridge.open_claims();
    claims.retain(|c| !lagged.iter().any(|l| l == c));
    claims
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
        // A repeat of the same virtual frame: the per-iteration steppers stand still (`held_repeat`).
        plx_gfx::dump::set_held_repeat(d.holds_run > 0);
        crate::app::clock::set_replay(ORIGIN_MS + virtual_ms(d.v));
        // The landing clock: a landing is due on a virtual frame, never on a count of iterations.
        app.bridge.landgate().dump_frame(d.v);
        // The storyboard's compiled key schedule (`site_video.py compile_storyboard`): a key goes down at the FIRST iteration of its virtual frame, so a
        // repeat never presses it twice, and comes up [`KEY_UP_AFTER`] virtual frames later, as a
        // finger leaves a remote. Both edges at one instant would make the press machine's commit
        // (`ui::press`: the release is read only once the press has armed, which happens in the
        // iteration AFTER the pre-pass sees the up edge) depend on how many iterations a virtual
        // frame took.
        if d.holds_run == 0 {
            let at = Tick { ms: ORIGIN_MS + virtual_ms(d.v), dt_us: 0 };
            while d.keys_next < d.cfg.keys.len() && d.cfg.keys[d.keys_next].0 <= d.v {
                let (_, key, up_after) = d.cfg.keys[d.keys_next];
                d.keys_next += 1;
                plx_base::eventlog::log(&format!("framedump: key {key:?} at virtual frame {}", d.v));
                app.inputs.push(crate::app::bridge::script_edge(key, plx_machine::machine::Edge::Down, at));
                let due = d.v + up_after;
                let at_idx = d.ups.partition_point(|&(t, _)| t <= due);
                d.ups.insert(at_idx, (due, key));
            }
            while let Some(&(due, key)) = d.ups.first() {
                if due > d.v { break; }
                d.ups.remove(0);
                app.inputs.push(crate::app::bridge::script_edge(key, plx_machine::machine::Edge::Up, at));
            }
        }
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
        // The takes ran before this: which of them held an answer back for the lag, and which are
        // still waiting on a worker.
        d.lagged = app.bridge.landgate().take_lagged();
        d.pre = Sample {
            placeholders: 0,
            tex_pending: plx_ui::tex::has_pending(),
            source_busy: !plx_ui::tex::source_idle(),
            page_image: false,
            claims: open_claims_but(app, &d.lagged),
            carried: false,
            starved: app.bridge.landgate().take_starved(),
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
        let after = Sample {
            placeholders: 0,
            tex_pending: plx_ui::tex::has_pending(),
            source_busy: !plx_ui::tex::source_idle(),
            page_image: app.pages.held_page_image().is_some(),
            claims: open_claims_but(app, &d.lagged),
            carried: app.pages.work_carried(),
            starved: app.bridge.landgate().take_starved(),
        };
        let post = d.pre.with_draw(after, &frame, &d.lagged);
        if !d.cfg.allow_absent && !frame.absent.is_empty() {
            let why = format!("a settled-absent placeholder is on screen (no clearLogo will arrive): {}", describe(&frame.absent));
            d.fail(why);
        }
        if !d.cfg.allow_absent {
            if let Some(why) = crate::app::adapters::poster::dump_failure() {
                d.fail(why);
            }
        }
        if app.pages.overlay_open() && !d.cfg.allow_overlay {
            d.fail("a modal or popover is open: its dim field is sampled on a cadence of presented iterations \
                    and is not yet synchronous in a dump (gfx::field_kick), so the frame would depend on the holds"
                .to_string());
        }
        let mut why = hold_reasons(&post);
        let mut action = if d.cfg.no_hold { Action::Write } else { decide(&post) };
        if action == Action::Hold {
            d.settle = None;
        } else if !d.cfg.no_hold {
            // SETTLING: a clean frame is written once a clean iteration at the same `T(v)` reproduces
            // the picture of the one before it. State that advances per iteration rather than per
            // millisecond (a layout that needs a second pass, a first draw that only measures) is
            // therefore run to its fixed point, and the fixed point does not depend on how many
            // iterations the holds before it happened to take. The cost is one more iteration and
            // one more read-back per virtual frame.
            let px = crate::shot::read_flipped(vx, vy, vw, vh, 3);
            let hash = xxhash_rust::xxh3::xxh3_64(&px);
            let again = d.settle.as_ref().is_some_and(|(_, h)| *h == hash);
            d.settle = Some((px, hash));
            if !again {
                action = Action::Hold;
                why.push("settling".to_string());
            } else if d.extras_run < d.cfg.extra_holds.for_frame(d.v) {
                // The hold-injection gate: a settled frame is held `extra` more times.
                d.extras_run += 1;
                action = Action::Hold;
                why.push("extra-hold".to_string());
            }
        }
        if action == Action::Hold {
            d.holds_run += 1;
            d.sum.holds += 1;
            for r in &why {
                *d.sum.hold_reasons.entry(r.clone()).or_default() += 1;
            }
            d.last_why = format!("{}; entries [{}]", why.join(","), describe(&frame.entries));
            if !post.starved.is_empty() {
                // A landing take is waiting on a worker: let it run before the next repeat polls.
                std::thread::sleep(Duration::from_micros(PACE_SLEEP_US));
            }
            let stuck_on_nothing = !post.tex_pending && !post.source_busy
                && post.claims.is_empty() && !frame.entries.is_empty() && frame.entries.iter().all(never_resolves);
            if stuck_on_nothing && !d.cfg.allow_absent {
                let why = format!("a card that can never resolve (empty art key) and nothing else pending: {}", describe(&frame.entries));
                d.fail(why);
            }
            return false;
        }
        // The frame is clean: it counts as a virtual frame whether or not it is written.
        // `frames.tsv`'s `t_ms` is `T(n)` of the WRITTEN index, the output timeline, which is what
        // `tools/site_video.py` gates. Virtual time is never skipped, so written frame `n` is
        // virtual frame `preroll + n`.
        let t = virtual_ms(d.written as u64);
        if d.v >= d.cfg.preroll as u64 {
            if vw != WIDTH || vh != HEIGHT {
                let why = format!("the viewport is {vw}x{vh}, the master is {WIDTH}x{HEIGHT} (the dump asks for a 1:1 drawable; set PLXNATIVE_WIN=1920x1080 and run on a display that can give one)");
                d.fail(why);
            }
            let (rgb, hash) = match d.settle.take() {
                Some(settled) => settled,
                None => {
                    let rgb = crate::shot::read_flipped(vx, vy, vw, vh, 3);
                    let hash = xxhash_rust::xxh3::xxh3_64(&rgb);
                    (rgb, hash)
                }
            };
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
        d.extras_run = 0;
        d.settle = None;
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
        assert_eq!(one(Sample { claims: vec!["hubs"], ..Default::default() }), ["claim:hubs"]);
        assert_eq!(one(Sample { carried: true, ..Default::default() }), ["carried-work"]);
        assert_eq!(one(Sample { starved: vec!["collection".to_string()], ..Default::default() }), ["owed:collection"]);
    }

    #[test]
    fn reasons_come_in_a_fixed_order_and_accumulate() {
        let s = Sample { placeholders: 1, tex_pending: true, source_busy: true, page_image: true,
            claims: vec!["hubs", "browse"], carried: true, starved: vec!["collection".to_string(), "person".to_string()] };
        assert_eq!(hold_reasons(&s), ["placeholder", "tex-pending", "source-busy", "claim:hubs", "claim:browse", "carried-work",
            "owed:collection", "owed:person"]);
    }

    #[test]
    fn debt_that_only_waiting_clears_holds_and_a_clean_sample_writes() {
        assert_eq!(decide(&Sample::default()), Action::Write);
        for s in [
            Sample { placeholders: 1, ..Default::default() },
            Sample { tex_pending: true, ..Default::default() },
            Sample { source_busy: true, ..Default::default() },
            Sample { claims: vec!["hubs"], ..Default::default() },
            Sample { carried: true, ..Default::default() },
            Sample { starved: vec!["collection".to_string()], ..Default::default() },
        ] {
            assert_eq!(decide(&s), Action::Hold, "{s:?}");
        }
    }

    #[test]
    fn a_held_page_image_is_a_reported_fact_and_never_a_hold_reason() {
        let image = Sample { page_image: true, ..Default::default() };
        assert_eq!(decide(&image), Action::Write, "an image with no debt is a frame like any other");
        assert!(hold_reasons(&image).is_empty());
        // its debt (re-counted on it by the dispatcher) holds like any other
        assert_eq!(decide(&Sample { placeholders: 7, ..image.clone() }), Action::Hold);
        assert_eq!(decide(&Sample { tex_pending: true, ..image.clone() }), Action::Hold);
        assert_eq!(decide(&Sample { source_busy: true, ..image.clone() }), Action::Hold);
        assert_eq!(decide(&Sample { claims: vec!["browse"], ..image }), Action::Hold);
    }

    /// The lag is asked for in milliseconds and runs in virtual frames, rounded up.
    #[test]
    fn the_landing_lag_is_whole_virtual_frames_rounded_up() {
        assert_eq!(land_lag_frames(0), 0, "no lag: the next execution of the site");
        assert_eq!(land_lag_frames(1), 1);
        assert_eq!(land_lag_frames(16), 1);
        assert_eq!(land_lag_frames(17), 2);
        assert_eq!(land_lag_frames(1000), 60);
        assert_eq!(land_lag_frames(1555), 94, "a 1.5 s listing is filmed for at least its 1.5 s");
        assert!(land_lag_frames(1555) as u32 * 1000 >= 1555 * FPS);
    }

    #[test]
    fn extra_holds_parse_and_are_a_pure_function_of_the_frame() {
        assert_eq!(ExtraHolds::parse("0"), Ok(ExtraHolds::Fixed(0)));
        assert_eq!(ExtraHolds::parse("3"), Ok(ExtraHolds::Fixed(3)));
        assert_eq!(ExtraHolds::parse("r17"), Ok(ExtraHolds::Seeded(17)));
        assert!(ExtraHolds::parse("x").is_err() && ExtraHolds::parse("r").is_err() && ExtraHolds::parse("-1").is_err());
        assert!((0..500).all(|v| ExtraHolds::Fixed(0).for_frame(v) == 0));
        assert!((0..500).all(|v| ExtraHolds::Fixed(2).for_frame(v) == 2));
        let a: Vec<u32> = (0..500).map(|v| ExtraHolds::Seeded(7).for_frame(v)).collect();
        let b: Vec<u32> = (0..500).map(|v| ExtraHolds::Seeded(7).for_frame(v)).collect();
        let c: Vec<u32> = (0..500).map(|v| ExtraHolds::Seeded(8).for_frame(v)).collect();
        assert_eq!(a, b, "a seed names one pattern");
        assert_ne!(a, c, "another seed names another");
        assert!(a.iter().all(|&k| k <= 3));
        for k in 0..=3 {
            assert!(a.contains(&k), "all of 0..=3 occur in 500 frames (missing {k})");
        }
    }

    #[test]
    fn keys_parse_in_frame_order_and_refuse_nonsense() {
        assert_eq!(parse_keys("200:ok, 100:right,100:down").unwrap(), [(100, Key::Right, 6), (100, Key::Down, 6), (200, Key::Ok, 6)]);
        assert_eq!(parse_keys("10:hold").unwrap(), [(10, Key::Ok, HOLD_UP_AFTER)]);
        assert_eq!(parse_keys("").unwrap(), []);
        assert!(parse_keys("100").is_err());
        assert!(parse_keys("x:ok").is_err());
        assert!(parse_keys("100:menu").is_err());
    }

    #[test]
    fn a_key_that_cannot_come_up_inside_the_run_is_refused_at_start() {
        // 60 preroll + 100 frames: virtual frames 0..=159, so the last down edge is 153 (up on 159).
        let keys = |v| vec![(v, Key::Ok, KEY_UP_AFTER)];
        assert!(check_key_schedule(&keys(153), 60, 100).is_ok());
        assert!(check_key_schedule(&keys(154), 60, 100).unwrap_err().contains("up edge is frame 160"));
        assert!(check_key_schedule(&keys(159), 60, 100).is_err(), "on the last frame");
        assert!(check_key_schedule(&keys(160), 60, 100).is_err(), "past the last frame");
        assert!(check_key_schedule(&[(114, Key::Ok, HOLD_UP_AFTER)], 60, 100).is_ok());
        assert!(check_key_schedule(&[(115, Key::Ok, HOLD_UP_AFTER)], 60, 100).is_err(), "a hold needs its longer tail");
        assert!(check_key_schedule(&[], 60, 100).is_ok());
        assert!(check_key_schedule(&[(10, Key::Ok, 6), (500, Key::Back, 6)], 0, 100).is_err(), "any key counts, not the first");
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
        assert!(!s.to_json().contains("advances"));
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

#[cfg(test)]
#[path = "framedump_lag_tests.rs"]
mod lag_tests;
