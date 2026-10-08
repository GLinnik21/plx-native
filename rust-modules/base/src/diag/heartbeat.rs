//! `Instruments` — the frame's own measurement (restructure spec §8.4): the eight phase stamps
//! over the frame algorithm's steps, the per-second `worstframe=`/`worstprep=` peaks the heartbeat
//! carries, and the `FRAMEDROP` line. Lifted out of `app/run.rs` in phase 2 so the loop reads as
//! its algorithm and the instrument is one value with one owner (`App.instr`).
//!
//! The wire formats are the harness's contract (`tests/run.py`: `FRAMEDROP_RE`, `WORST_RE`,
//! `FPS_RE`) and are byte-identical to what the loop wrote before this module existed: the
//! FRAMEDROP fields in the spec's order (ingest results tick_drain navcommit prepare draw capture
//! swap), `worstframe=` LAST of the graded fields on the heartbeat. Phase 11 added the rest of
//! §8.4: `coldopen` (its own line, once per screen mount, UNARMED), and `carried=`/`dropped=`/
//! `budget=`/`evicted_hot=` after `worstprep=` — see [`Instruments::heartbeat_tail`] for the wire
//! order and why nothing may be inserted ahead of `fps=` or `worstframe=`.
//!
//! Phase timing and present-to-present pacing are armed, and the split is the measurement's
//! cost: the eight phases and the two peaks need a clock read per phase, so they hide behind
//! `plxnative-framedrop`; every other field is a counter somebody already keeps, so it prints in
//! every build. The one §8.4 names and this module does NOT have is `allocs=` — see
//! `heartbeat_tail`'s doc for why a counting allocator was not invented to fill it.
//!
//! Armed by `plxnative-framedrop[=<ms>]`; unarmed, every stamp is the frame's origin and every
//! phase reads 0.0 — the counter is never read, so an unarmed frame pays nothing.

#[cfg(not(any(test, feature = "test-support")))]
extern "C" {
    fn SDL_GetPerformanceCounter() -> u64;
    fn SDL_GetPerformanceFrequency() -> u64;
}
// The host test binary links no SDL: the tests set the stamps and the frequency directly.
#[cfg(any(test, feature = "test-support"))]
#[allow(non_snake_case)] // the SDL name, so the call sites read the same in both builds
unsafe fn SDL_GetPerformanceCounter() -> u64 {
    0
}
#[cfg(any(test, feature = "test-support"))]
#[allow(non_snake_case)]
unsafe fn SDL_GetPerformanceFrequency() -> u64 {
    1000
}

/// The eight phases, as stamp indices: `mark(Phase::X)` stamps the END of phase X.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub enum Phase {
    /// The iteration's origin (stamp 0), taken at the loop top.
    Top = 0,
    Ingest = 1,
    Results = 2,
    NavCommit = 3,
    TickDrain = 4,
    Prepare = 5,
    Draw = 6,
    Capture = 7,
    Swap = 8,
}

/// Microseconds on the performance counter — the frame budget's clock (`ui::frame::Budget`).
pub fn now_us() -> u64 {
    // SAFETY: SDL is initialised before the loop; no arguments, no memory of ours.
    let (t, f) = unsafe { (SDL_GetPerformanceCounter(), SDL_GetPerformanceFrequency()) };
    if f == 0 {
        return 0;
    }
    t / (f / 1_000_000).max(1)
}

/// The per-FRAME counters the `FRAMEDROP` line carries beside the eight phases: GL uploads and
/// their pixels (`app::adapters::poster`), card composites and how many of those were culled
/// (`gfx`). All four are read-and-reset atomics owned by the module that increments them.
///
/// **They are a FRAME's counts, and until phase 11 they were not.** Their only drain was the
/// `extra()` closure of [`Instruments::frame_drop_line`], which runs AFTER the threshold check —
/// so on a healthy frame nothing drained them, and the numbers printed beside the next slow frame
/// covered every frame since the last slow one. A `FRAMEDROP` line with `up=9` on it therefore
/// said "nine uploads since the previous drop line", which reads as this frame's cost and is not
/// (`docs/measurements/tv-session-5-2026-09-10.md`, the 2026-09-10 reading, was taken that way).
/// The loop drains them on EVERY presented frame now and hands the result to
/// [`Instruments::note_frame_counters`]; `frame_drop_line` only formats what it was given.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FrameCounters {
    pub uploads: u32,
    pub upload_px: u64,
    pub cards: u32,
    pub cards_off: u32,
}

/// The heartbeat's frame-plan fields (spec §8.4), gathered by the loop once a second and printed
/// after `worstprep=` — before `rec=`/`sim=1`, so `tests/run.py`'s `FPS_RE` and `WORST_RE` anchors
/// are untouched. Plain scalars rather than the frame plan's own types: this module is `diag/`
/// and has no business naming `ui::frame::budget::Class`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct HeartbeatFields {
    /// Effects the dispatcher carried into the next frame at the last frame of this second.
    pub carried: usize,
    /// Deliveries dropped since the previous heartbeat (an undeliverable addressee, a full lane).
    pub dropped: u32,
    /// `Budget` takes admitted / refused since the previous heartbeat, and the solo class if one
    /// was admitted in that window.
    pub admitted: u32,
    pub refused: u32,
    pub solo: Option<&'static str>,
    /// Glyph-cache entries evicted while still inside their hot window (`text::take_evicted_hot`).
    pub evicted_hot: u32,
}

/// Presented-frame cadence, including pacing/scheduler time outside the measured CPU phases.
/// The fixed histogram rounds up to integer milliseconds; bin 256 holds every interval over
/// 255ms. A percentile in that overflow bin reports the observed maximum as an upper bound.
/// Exact threshold counters use the performance counter directly, before histogram rounding.
struct FramePacing {
    histogram: [u64; 257],
    n: u64,
    gt16: u64,
    gt33: u64,
    gt50: u64,
    gt100: u64,
    max_ms: f64,
    /// Poster-upload frames (see [`POSTER_UPLOAD_MAX_PX`]), those whose interval was at least
    /// [`UPLOAD_LATE_MS`], and the uploads on them.
    pup_frames: u64,
    pup_late: u64,
    pup_uploads: u64,
}

/// A presented frame is a POSTER-upload frame when it uploaded something and the mean upload was
/// at most this many pixels (a 250x375 poster is 93 750; a 1280x720 backdrop is 921 600). The
/// same split the television analysis used, so a backdrop's multi-frame landing never counts as a
/// poster burst's lateness.
pub const POSTER_UPLOAD_MAX_PX: u64 = 120_000;

/// A frame interval at or over this many milliseconds is "late" for the poster-upload share
/// (`pupf_ge22=`): the next refresh slot after a 16.7 ms frame plus a 5 ms hitch.
pub const UPLOAD_LATE_MS: u64 = 22;

impl Default for FramePacing {
    fn default() -> Self {
        Self {
            histogram: [0; 257],
            n: 0,
            gt16: 0,
            gt33: 0,
            gt50: 0,
            gt100: 0,
            max_ms: 0.0,
            pup_frames: 0,
            pup_late: 0,
            pup_uploads: 0,
        }
    }
}

impl FramePacing {
    fn note(&mut self, ticks: u64, frequency: u64) {
        if frequency == 0 {
            return;
        }
        let elapsed_ms = ticks as f64 * 1000.0 / frequency as f64;
        self.n += 1;
        self.histogram[(elapsed_ms.ceil() as usize).min(256)] += 1;
        self.max_ms = self.max_ms.max(elapsed_ms);
        // Strictly over 1/60s, 1/30s, 1/20s and 1/10s. Integer cross-products distinguish an
        // exact boundary from one tick above it, independent of floating point rounding.
        let ticks = ticks as u128;
        let frequency = frequency as u128;
        self.gt16 += u64::from(ticks * 60 > frequency);
        self.gt33 += u64::from(ticks * 30 > frequency);
        self.gt50 += u64::from(ticks * 20 > frequency);
        self.gt100 += u64::from(ticks * 10 > frequency);
    }

    /// Fold the interval just noted into the poster-upload tally when this frame uploaded posters.
    fn note_uploads(&mut self, ticks: u64, frequency: u64, c: FrameCounters) {
        if frequency == 0 || c.uploads == 0 || c.upload_px / u64::from(c.uploads) > POSTER_UPLOAD_MAX_PX {
            return;
        }
        self.pup_frames += 1;
        self.pup_uploads += u64::from(c.uploads);
        // `ticks / frequency >= UPLOAD_LATE_MS / 1000`, in integers.
        self.pup_late += u64::from(u128::from(ticks) * 1000 >= u128::from(frequency) * u128::from(UPLOAD_LATE_MS));
    }

    fn percentile(&self, percent: u64) -> f64 {
        if self.n == 0 {
            return 0.0;
        }
        let rank = (self.n * percent).div_ceil(100);
        let mut count = 0;
        for (ms, n) in self.histogram.iter().enumerate() {
            count += n;
            if count >= rank {
                return if ms == 256 { self.max_ms } else { ms as f64 };
            }
        }
        self.max_ms
    }

    fn tail(&self) -> String {
        let mut s = format!(
            " frame_n={} frame_gt16={} frame_gt33={} frame_gt50={} frame_gt100={} frame_max={:.1}ms frame_p95={:.1}ms frame_p99={:.1}ms",
            self.n, self.gt16, self.gt33, self.gt50, self.gt100, self.max_ms,
            self.percentile(95), self.percentile(99),
        );
        // Present only in a second that uploaded posters, so its presence is the event and every
        // other heartbeat is byte-for-byte what it was.
        if self.pup_frames > 0 {
            s.push_str(&format!(
                " pupf={} pupf_ge22={} pup={}",
                self.pup_frames, self.pup_late, self.pup_uploads
            ));
        }
        s
    }
}

pub struct Instruments {
    armed: bool,
    thresh_ms: f64,
    perf_freq: f64,
    stamps: [u64; 9],
    /// Worst frame total this second (presented frames only), for the heartbeat peak.
    worst: f64,
    /// Worst prepare phase this second, timed on EVERY iteration (ungated by present).
    worst_prep: f64,
    /// THIS frame's counters, replaced (never accumulated) once per presented frame.
    counters: FrameCounters,
    /// None after startup or a skipped present, so intentional idle gaps are not samples.
    previous_present: Option<u64>,
    pacing: FramePacing,
    /// `plxnative-framering`: write slow frames with their context instead of every frame.
    ring: Option<FrameRing>,
}

/// **The frame-drop detector's context ring** (`/tmp/plxnative-framering[=<ms>]`).
///
/// Reading a slow frame needs the frames around it — the one before it committed late or did not,
/// the one after it is short or is not, and a compositor callback for frame N arrives during
/// frame N+1 — so a pacing investigation used to arm `framedrop=0.01` and log every frame. That is
/// ~60 scrub + open + append + close round trips a second ON THE FRAME THREAD, in the phase being
/// measured. With the ring armed every frame's line is still formatted, but it is only HELD: the
/// last [`Self::BEFORE`] lines stay in memory, and they reach the log when a frame crosses `slow_ms`,
/// together with that frame and the [`Self::AFTER`] frames that follow it.
pub struct FrameRing {
    slow_ms: f64,
    held: std::collections::VecDeque<String>,
    /// Frames still owed to the log after the last slow one.
    after: usize,
}

impl FrameRing {
    /// Frames of context written before a slow frame.
    const BEFORE: usize = 4;
    /// Frames of context written after it.
    const AFTER: usize = 3;

    pub fn new(slow_ms: f64) -> Self {
        Self { slow_ms, held: std::collections::VecDeque::with_capacity(Self::BEFORE + 1), after: 0 }
    }

    /// Offer one presented frame's line. Returns the lines to write NOW, oldest first: nothing for
    /// an ordinary frame, the held context plus the frame for a slow one, the frame alone while
    /// the context after a slow one is still owed.
    pub fn offer(&mut self, total_ms: f64, line: String) -> Vec<String> {
        if total_ms >= self.slow_ms {
            self.after = Self::AFTER;
            let mut out: Vec<String> = self.held.drain(..).collect();
            out.push(line);
            return out;
        }
        if self.after > 0 {
            self.after -= 1;
            return vec![line];
        }
        if self.held.len() == Self::BEFORE {
            self.held.pop_front();
        }
        self.held.push_back(line);
        Vec::new()
    }
}

impl Instruments {
    /// `armed` is the trigger's presence; `thresh_ms` its content (default 22).
    pub fn new(armed: bool, thresh_ms: f64) -> Self {
        Self {
            armed,
            thresh_ms,
            // SAFETY: a plain SDL query with no preconditions; SDL is initialised before the
            // loop that owns this value exists.
            perf_freq: unsafe { SDL_GetPerformanceFrequency() } as f64,
            stamps: [0; 9],
            worst: 0.0,
            worst_prep: 0.0,
            counters: FrameCounters::default(),
            previous_present: None,
            pacing: FramePacing::default(),
            ring: None,
        }
    }

    /// Arm the context ring ([`FrameRing`]): every presented frame formats its line, so the
    /// threshold drops to zero, and `slow_ms` decides which of them reach the log.
    pub fn arm_ring(&mut self, slow_ms: f64) {
        if self.armed {
            self.thresh_ms = 0.0;
            self.ring = Some(FrameRing::new(slow_ms));
        }
    }

    /// This frame's counters, REPLACING the last frame's — see [`FrameCounters`] for why that
    /// word is the whole point. Called once per presented frame, before [`Self::frame_drop_line`].
    pub fn note_frame_counters(&mut self, c: FrameCounters) {
        self.counters = c;
    }

    fn ms(&self, ticks: u64) -> f64 {
        ticks as f64 * 1000.0 / self.perf_freq
    }

    /// Stamp the end of a phase. Unarmed, a phase inherits the origin so every span reads 0.
    pub fn mark(&mut self, phase: Phase) {
        let i = phase as usize;
        self.stamps[i] = if self.armed {
            // SAFETY: as `new`.
            unsafe { SDL_GetPerformanceCounter() }
        } else if i == 0 {
            0
        } else {
            self.stamps[0]
        };
    }

    /// Initialize draw/capture/swap to the end of prepare before the present decision. This
    /// runs on every iteration; a later present replaces the stamps and preserves cadence.
    pub fn seed_present_phases(&mut self) {
        let p = self.stamps[Phase::Prepare as usize];
        self.stamps[Phase::Draw as usize] = p;
        self.stamps[Phase::Capture as usize] = p;
        self.stamps[Phase::Swap as usize] = p;
    }

    /// An iteration that actually skips presenting has no draw/capture/swap. Its idle gap must
    /// not become a pacing sample when presenting resumes; call only after that decision.
    pub fn skip_present_phases(&mut self) {
        self.seed_present_phases();
        self.previous_present = None;
    }

    /// An iteration whose present the capture fence deferred. Same stamps as a skip, but the
    /// cadence is NOT broken: `previous_present` stays, so the next presented frame's interval
    /// (and its pacing sample, `frame_max`) spans the deferred iterations. Deliberate: a fence
    /// hold is a real late picture for the panel, which `skip_present_phases` used to hide.
    pub fn defer_present_phases(&mut self) {
        self.seed_present_phases();
    }

    /// After `mark(Prepare)`: fold this iteration's prepare span into the per-second peak.
    pub fn note_prepare(&mut self) {
        if !self.armed {
            return;
        }
        let prep = self.ms(self.stamps[Phase::Prepare as usize]
            .wrapping_sub(self.stamps[Phase::TickDrain as usize]));
        if prep > self.worst_prep {
            self.worst_prep = prep;
        }
    }

    fn span(&self, phase: Phase) -> f64 {
        let i = phase as usize;
        self.ms(self.stamps[i].wrapping_sub(self.stamps[i - 1]))
    }

    /// The just-finished frame's total (Top→Swap), or `0.0` unarmed. The narrow read the
    /// stress-bench oscillators (`dev::scenarios::bench_frame_tick`) need without duplicating
    /// what [`Self::frame_drop_line`] already computes — that method's `total` local, pulled out
    /// so a caller that wants the number and not the threshold/log-line behaviour can have it.
    /// No side effect, unlike `frame_drop_line`: it does not fold into `self.worst` and may be
    /// called on every iteration, presented or not — an unpresented frame's Draw/Capture/Swap
    /// stamps equal Prepare's (`skip_present_phases`), so its "total" reads as the prepare-only
    /// cost, which is why callers that want presented-frame timing gate on `Frame::present`
    /// themselves rather than trusting this alone.
    pub fn last_frame_ms(&self) -> f64 {
        if !self.armed {
            return 0.0;
        }
        self.ms(self.stamps[Phase::Swap as usize].wrapping_sub(self.stamps[Phase::Top as usize]))
    }

    /// The just-presented frame's present-to-present interval in ms: its Swap stamp minus the
    /// previous presented frame's. `None` unarmed, and for the first frame after startup or a
    /// skipped present (an idle gap is not a cadence sample — `skip_present_phases`). Read it
    /// BEFORE [`Self::frame_drop_line`], which advances the previous-present stamp.
    pub fn present_interval_ms(&self) -> Option<f64> {
        if !self.armed {
            return None;
        }
        let previous = self.previous_present?;
        Some(self.ms(self.stamps[Phase::Swap as usize].wrapping_sub(previous)))
    }

    /// At the iteration's tail of a PRESENTED frame: fold the total into the peak and return the
    /// `FRAMEDROP` line when it crossed the threshold. The four per-frame counters come from
    /// [`Self::note_frame_counters`] — this frame's, not the accumulation since the last drop
    /// line — and `extra` is the loop's own trailing fields (route, load, snap), appended
    /// verbatim after them.
    pub fn frame_drop_line(&mut self, extra: &dyn Fn() -> String) -> Option<String> {
        if !self.armed {
            return None;
        }
        let present = self.stamps[Phase::Swap as usize];
        if let Some(previous) = self.previous_present.replace(present) {
            let ticks = present.wrapping_sub(previous);
            self.pacing.note(ticks, self.perf_freq as u64);
            self.pacing.note_uploads(ticks, self.perf_freq as u64, self.counters);
        }
        let total = self
            .ms(self.stamps[Phase::Swap as usize].wrapping_sub(self.stamps[Phase::Top as usize]));
        if total > self.worst {
            self.worst = total;
        }
        if total <= self.thresh_ms {
            return None;
        }
        // Printed in the frame ALGORITHM's order (spec §8.4), which on the legacy loop is not the
        // order they ran: navcommit runs before tick_drain there.
        let c = self.counters;
        Some(format!(
            "FRAMEDROP total={total:.1} ingest={:.1} results={:.1} tick_drain={:.1} navcommit={:.1} prepare={:.1} draw={:.1} capture={:.1} swap={:.1} up={} px={} cards={} off={} {}",
            self.span(Phase::Ingest),
            self.span(Phase::Results),
            self.span(Phase::TickDrain),
            self.span(Phase::NavCommit),
            self.span(Phase::Prepare),
            self.span(Phase::Draw),
            self.span(Phase::Capture),
            self.span(Phase::Swap),
            c.uploads,
            c.upload_px,
            c.cards,
            c.cards_off,
            extra(),
        ))
    }

    /// [`Self::frame_drop_line`] through the context ring when one is armed: the lines to write
    /// for this presented frame, oldest first. Without a ring, that method's line or nothing.
    pub fn frame_drop_lines(&mut self, extra: &dyn Fn() -> String) -> Vec<String> {
        let total = self.last_frame_ms();
        let Some(line) = self.frame_drop_line(extra) else {
            return Vec::new();
        };
        match self.ring.as_mut() {
            Some(ring) => ring.offer(total, line),
            None => vec![line],
        }
    }

    /// The heartbeat's trailing fields, and the per-second reset. The WIRE ORDER is a contract:
    ///
    /// ```text
    /// … fps=<n> [load= snap= period=] [worstframe= worstprep=] carried= dropped= budget= evicted_hot= [frame_n= frame_gt16= frame_gt33= frame_gt50= frame_gt100= frame_max= frame_p95= frame_p99= [pupf= pupf_ge22= pup=]] [rec=] [sim=1]
    /// ```
    ///
    /// * `worstframe=`/`worstprep=` are the ARMED pair (`plxnative-framedrop`) and stay LAST of
    ///   the graded fields — `tests/run.py`'s `WORST_RE` anchors on `worstframe=` and `FPS_RE` on
    ///   `fps=`, both with a lazy `.*?` in front, so a field may be appended after them and never
    ///   inserted between `loop=`/`route=`/`overlay=`.
    /// * The four frame-plan fields (spec §8.4) print UNARMED, on every heartbeat, because none
    ///   of them costs a measurement: they are counters four owners already keep. `budget=` is
    ///   `admitted/refused`, with `/solo:<class>` appended only when a solo take was admitted in
    ///   the second — a field that is usually absent, so its presence is the event.
    /// * The ARMED `frame_*` summary measures intervals between consecutive presented swaps.
    ///   It includes pacing delays outside the CPU phases, excludes gaps across a skipped
    ///   present, and resets each heartbeat while preserving the previous present timestamp.
    ///   `gt16` and `gt33` mean strictly over 1000/60ms and 1000/30ms. Percentiles round up to
    ///   whole milliseconds; an overflow percentile (>255ms) reports the maximum interval.
    /// * `rec_us` is the recorder's spend this second (spec §5.3): it rides after all of them,
    ///   and its presence is what disqualifies the run's `fps=`/`worstframe=` in `tests/run.py`,
    ///   exactly as the profiler triggers do — a recorder perturbs the pacing it feeds.
    ///
    /// There is deliberately NO `allocs=` field, which §8.4 names for `hostsim`. This crate has
    /// no `GlobalAlloc` and phase 11 did not add one: a counting allocator is a whole-process
    /// instrument with its own correctness argument, and inventing one to fill a heartbeat field
    /// would be the more expensive half of the work with none of the evidence.
    pub fn heartbeat_tail(&mut self, f: HeartbeatFields, rec_us: Option<u64>) -> String {
        let mut s = String::new();
        if self.armed {
            s = format!(
                " worstframe={:.1}ms worstprep={:.1}ms",
                self.worst, self.worst_prep
            );
            self.worst = 0.0;
            self.worst_prep = 0.0;
        }
        s.push_str(&format!(
            " carried={} dropped={} budget={}/{}{} evicted_hot={}",
            f.carried,
            f.dropped,
            f.admitted,
            f.refused,
            match f.solo {
                Some(class) => format!("/solo:{class}"),
                None => String::new(),
            },
            f.evicted_hot,
        ));
        if self.armed {
            s.push_str(&self.pacing.tail());
            self.pacing = FramePacing::default();
        }
        if let Some(us) = rec_us {
            s.push_str(&format!(" rec={us}us"));
        }
        s
    }
}

// ---------------------------------------------------------------------------------------------
// coldopen (spec §8.4)
// ---------------------------------------------------------------------------------------------

/// **`coldopen screen=<name> ms=<n> prepared=<bool>`, once per mount, on every build.**
///
/// The definition, which is the whole of it:
///
/// * the clock starts on the frame that executed `Fx::Mount` for that body — `Dispatcher::mount`,
///   the same event `FrameReport::mounted` reports;
/// * it stops on the FIRST frame that body both PREPARED and DREW. A body that mounts and is
///   unmounted without ever drawing produces no line at all, and its pending record is dropped
///   when the unmount lands;
/// * `ms` is the difference of those two frames' tick clocks. A mount and a first draw in ONE
///   iteration is `ms=0`, and that is the honest reading: the instrument measures how long a
///   screen took to get on screen, not how long the frame it landed in took (`worstframe=` and
///   `FRAMEDROP` are that);
/// * `prepared` is whether the frame it drew on refused NOTHING it asked for — the `Budget`'s
///   refusal count taken at the opening of that frame's prepare window and read again at the
///   draw. It is the frame's verdict rather than the screen's, deliberately: the budget admits
///   per CLASS across the whole frame, so "did this screen get everything" is not a question the
///   budget can answer and pretending otherwise would put a per-screen word on a per-frame fact.
///
/// It is UNARMED — no trigger, no threshold — because it is one line per screen mount, and the
/// scene it re-bases (`cold-open`) could not be graded from `FRAMEDROP` without censoring itself:
/// the detector only prints above the threshold `tests/run.py` armed it with, so every cold open
/// FASTER than the ceiling left no line, and a run of them was graded as passes with no samples
/// at all (`tests/run.py::grade_frame_ceilings`, and the "no-line PASS, 208.5, no-line PASS"
/// reading in `docs/measurements/tv-session-5-2026-09-10.md`).
///
/// **One route is structurally invisible to it: the player.** Its page still answers
/// `FocusSource::Legacy`, so `bridge::page_owned` is false there and the loop draws that page
/// itself rather than through `Dispatcher::draw_with` — the pending record is never matched and
/// no line is emitted. That ends when the player's focus moves onto the engine; nothing else is
/// needed here.
#[derive(Default)]
pub struct ColdOpens {
    pending: Vec<Pending>,
    /// The `Budget`'s refusal count as this frame's prepare window opened.
    refused_at_prepare: u32,
    /// Lines waiting for the loop's report block to log them.
    out: Vec<String>,
}

struct Pending {
    id: u32,
    screen: &'static str,
    at_ms: u32,
}

/// A mount that never draws holds a record; the cap stops a pathological session growing it
/// without bound. Deep enough for a page plus every surface a route can stack over it.
const COLD_PENDING_MAX: usize = 16;

impl ColdOpens {
    /// A body mounted this frame. `InstanceId`s are never reused, so one id can be pending once.
    pub fn mounted(&mut self, id: u32, screen: &'static str, now_ms: u32) {
        if self.pending.iter().any(|p| p.id == id) {
            return;
        }
        if self.pending.len() >= COLD_PENDING_MAX {
            self.pending.remove(0);
        }
        self.pending.push(Pending {
            id,
            screen,
            at_ms: now_ms,
        });
    }

    /// A body was unmounted without ever drawing: forget it rather than report it late.
    pub fn unmounted(&mut self, id: u32) {
        self.pending.retain(|p| p.id != id);
    }

    /// The prepare window opened: remember the budget's refusal count to difference at the draw.
    pub fn note_prepare(&mut self, refused: u32) {
        self.refused_at_prepare = refused;
    }

    /// A body drew this frame, on a frame whose prepare pass ran. `refused_now` is the budget's
    /// refusal count at the draw; the line is queued for [`Self::take_lines`].
    pub fn drawn(&mut self, id: u32, now_ms: u32, refused_now: u32) {
        let Some(i) = self.pending.iter().position(|p| p.id == id) else {
            return;
        };
        let p = self.pending.remove(i);
        let prepared = refused_now == self.refused_at_prepare;
        self.out.push(format!(
            "coldopen screen={} ms={} prepared={}",
            p.screen,
            now_ms.wrapping_sub(p.at_ms),
            prepared
        ));
    }

    pub fn take_lines(&mut self) -> Vec<String> {
        std::mem::take(&mut self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unarmed_instrument_reads_zero_and_writes_no_frame_drop_line() {
        let mut i = Instruments::new(false, 22.0);
        i.mark(Phase::Top);
        i.mark(Phase::Ingest);
        i.mark(Phase::Prepare);
        i.note_prepare();
        i.skip_present_phases();
        assert!(i.frame_drop_line(&|| String::new()).is_none());
        // the frame-plan fields are NOT the armed pair: they print on every heartbeat
        assert_eq!(
            i.heartbeat_tail(HeartbeatFields::default(), None),
            " carried=0 dropped=0 budget=0/0 evicted_hot=0"
        );
    }

    /// **The ring writes a slow frame with the frames around it, and nothing else.** Logging
    /// every frame put ~60 file appends a second on the frame thread, inside the measurement.
    #[test]
    fn the_context_ring_writes_only_slow_frames_and_their_neighbours() {
        let mut ring = FrameRing::new(17.0);
        let mut written = Vec::new();
        // ten ordinary frames, a slow one, five ordinary, then quiet
        for n in 0..10 {
            written.extend(ring.offer(16.2, format!("f{n}")));
        }
        assert!(written.is_empty(), "ordinary frames reached the log: {written:?}");
        written.extend(ring.offer(22.4, "slow".into()));
        assert_eq!(written, ["f6", "f7", "f8", "f9", "slow"], "the slow frame with BEFORE frames of context");
        written.clear();
        for n in 10..16 {
            written.extend(ring.offer(16.2, format!("f{n}")));
        }
        assert_eq!(written, ["f10", "f11", "f12"], "AFTER frames follow it, then the ring holds again");
        // a second slow frame does not repeat what was already written
        written.clear();
        written.extend(ring.offer(30.0, "slow2".into()));
        assert_eq!(written, ["f13", "f14", "f15", "slow2"]);

        // Through the instrument: an armed ring makes every frame a candidate, an unarmed
        // instrument stays silent, and without a ring the threshold decides as before.
        let mut i = Instruments::new(true, 22.0);
        i.arm_ring(17.0);
        i.perf_freq = 1000.0;
        i.stamps = [0, 1, 2, 3, 4, 5, 6, 7, 16];
        assert!(i.frame_drop_lines(&String::new).is_empty(), "a 16 ms frame is held, not written");
        i.stamps = [0, 1, 2, 3, 4, 5, 6, 7, 21];
        let lines = i.frame_drop_lines(&String::new);
        assert_eq!(lines.len(), 2, "the held frame and the slow one: {lines:?}");
        assert!(lines[0].starts_with("FRAMEDROP total=16.0") && lines[1].starts_with("FRAMEDROP total=21.0"));
        let mut unarmed = Instruments::new(false, 22.0);
        unarmed.arm_ring(17.0);
        assert!(unarmed.frame_drop_lines(&String::new).is_empty());
        let mut plain = Instruments::new(true, 22.0);
        plain.perf_freq = 1000.0;
        plain.stamps = [0, 1, 2, 3, 4, 5, 6, 7, 21];
        assert!(plain.frame_drop_lines(&String::new).is_empty(), "21 ms is under the 22 ms threshold");
    }

    #[test]
    fn the_frame_drop_line_carries_the_eight_phases_in_the_specs_order_and_the_extra() {
        let mut i = Instruments::new(true, 0.0); // threshold 0: every frame is a drop
        i.perf_freq = 1000.0; // one tick per ms, so spans are readable
        i.stamps = [0, 1, 3, 6, 10, 15, 21, 28, 36];
        i.note_frame_counters(FrameCounters {
            uploads: 2,
            upload_px: 187_500,
            cards: 9,
            cards_off: 1,
        });
        let line = i.frame_drop_line(&|| "route=home".into()).unwrap();
        assert_eq!(
            line,
            "FRAMEDROP total=36.0 ingest=1.0 results=2.0 tick_drain=4.0 navcommit=3.0 prepare=5.0 \
             draw=6.0 capture=7.0 swap=8.0 up=2 px=187500 cards=9 off=1 route=home"
        );
        assert_eq!(
            i.heartbeat_tail(HeartbeatFields::default(), None),
            " worstframe=36.0ms worstprep=0.0ms carried=0 dropped=0 budget=0/0 evicted_hot=0 \
             frame_n=0 frame_gt16=0 frame_gt33=0 frame_gt50=0 frame_gt100=0 frame_max=0.0ms frame_p95=0.0ms frame_p99=0.0ms"
        );
        assert_eq!(
            i.heartbeat_tail(HeartbeatFields::default(), Some(17)),
            " worstframe=0.0ms worstprep=0.0ms carried=0 dropped=0 budget=0/0 evicted_hot=0 \
             frame_n=0 frame_gt16=0 frame_gt33=0 frame_gt50=0 frame_gt100=0 frame_max=0.0ms frame_p95=0.0ms frame_p99=0.0ms rec=17us",
            "reset per second; rec= rides last"
        );
    }

    fn present_at(i: &mut Instruments, swap: u64) {
        i.stamps = [swap - 2; 9];
        // prepare_window seeds these phases on every iteration, including presented frames.
        i.seed_present_phases();
        i.stamps[Phase::Swap as usize] = swap;
        assert!(
            i.frame_drop_line(&String::new).is_none(),
            "short CPU phases need no FRAMEDROP line"
        );
    }

    /// The poster-upload tally (`pupf=`/`pupf_ge22=`/`pup=`): only POSTER-sized upload frames
    /// count, a frame is late at 22 ms exactly, and a second with none prints nothing extra.
    #[test]
    fn poster_upload_frames_are_tallied_by_interval_and_a_backdrop_frame_is_not_one() {
        let c = |uploads, upload_px| FrameCounters { uploads, upload_px, ..FrameCounters::default() };
        let mut i = Instruments::new(true, 22.0);
        present_at(&mut i, 100); // first present: no interval yet
        i.note_frame_counters(c(2, 187_500));
        present_at(&mut i, 122); // 22 ms, two posters: late (the boundary counts)
        i.note_frame_counters(c(1, 93_750));
        present_at(&mut i, 138); // 16 ms, one poster: on time
        i.note_frame_counters(c(1, 921_600));
        present_at(&mut i, 188); // a backdrop: not a poster frame whatever its interval
        i.note_frame_counters(c(0, 0));
        present_at(&mut i, 238); // no upload at all
        let tail = i.heartbeat_tail(HeartbeatFields::default(), None);
        assert!(tail.ends_with(" pupf=2 pupf_ge22=1 pup=3"), "{tail}");
        let quiet = i.heartbeat_tail(HeartbeatFields::default(), None);
        assert!(!quiet.contains("pupf="), "reset per second, absent when nothing uploaded: {quiet}");
    }

    #[test]
    fn pacing_measures_actual_swap_intervals_including_time_outside_cpu_work() {
        let mut i = Instruments::new(true, 22.0);
        present_at(&mut i, 100);
        present_at(&mut i, 150);
        let tail = i.heartbeat_tail(HeartbeatFields::default(), None);
        assert!(tail.contains(" worstframe=2.0ms "), "{tail}");
        assert!(tail.contains(" frame_n=1 frame_gt16=1 frame_gt33=1 frame_gt50=0 frame_gt100=0 frame_max=50.0ms frame_p95=50.0ms frame_p99=50.0ms"), "{tail}");
        let mut unarmed = Instruments::new(false, 22.0);
        present_at(&mut unarmed, 100);
        present_at(&mut unarmed, 150);
        assert_eq!(unarmed.pacing.n, 0);
        assert_eq!(unarmed.previous_present, None);
        assert!(!unarmed
            .heartbeat_tail(HeartbeatFields::default(), None)
            .contains("frame_"));
    }

    #[test]
    fn skipped_present_excludes_idle_gap_without_discarding_prior_samples() {
        let mut i = Instruments::new(true, 22.0);
        present_at(&mut i, 100);
        present_at(&mut i, 116);
        i.skip_present_phases();
        present_at(&mut i, 10_000);
        present_at(&mut i, 10_016);
        let tail = i.heartbeat_tail(HeartbeatFields::default(), None);
        assert!(
            tail.contains(
                " frame_n=2 frame_gt16=0 frame_gt33=0 frame_gt50=0 frame_gt100=0 frame_max=16.0ms"
            ),
            "{tail}"
        );
    }

    #[test]
    fn a_deferred_present_keeps_the_cadence_and_an_idle_skip_breaks_it() {
        let mut deferred = Instruments::new(true, 22.0);
        present_at(&mut deferred, 100);
        deferred.defer_present_phases();
        deferred.defer_present_phases();
        present_at(&mut deferred, 147);
        assert_eq!(deferred.pacing.n, 1, "the interval across a deferral is a pacing sample");
        let tail = deferred.heartbeat_tail(HeartbeatFields::default(), None);
        assert!(tail.contains("frame_n=1 frame_gt16=1 frame_gt33=1"), "{tail}");
        assert!(tail.contains("frame_max=47.0ms"), "{tail}");

        let mut idle = Instruments::new(true, 22.0);
        present_at(&mut idle, 100);
        idle.skip_present_phases();
        present_at(&mut idle, 147);
        assert_eq!(idle.pacing.n, 0, "an idle gap is not a sample");
    }

    #[test]
    fn prepare_seeding_preserves_present_cadence_and_only_actual_idle_breaks_it() {
        let mut i = Instruments::new(true, 22.0);
        // The same sequence as run.rs: prepare_window seeds every iteration, then a present
        // replaces its swap stamp. Three consecutive presents therefore give two intervals.
        for swap in [100, 117, 134] {
            present_at(&mut i, swap);
        }
        assert_eq!(i.pacing.n, 2);
        assert_eq!(i.previous_present, Some(134));
        i.stamps[Phase::Prepare as usize] = 140;
        i.seed_present_phases();
        assert_eq!(i.previous_present, Some(134), "seeding alone is not idle");
        assert_eq!(&i.stamps[Phase::Draw as usize..], &[140, 140, 140]);
        // Only the actual !fr.present branch marks the cadence discontinuity.
        i.skip_present_phases();
        assert_eq!(i.previous_present, None);
        present_at(&mut i, 10_000);
        assert_eq!(
            i.pacing.n, 2,
            "first present after idle only anchors the next interval"
        );
        present_at(&mut i, 10_017);
        assert_eq!(i.pacing.n, 3);
        assert_eq!(i.pacing.max_ms, 17.0);
    }

    #[test]
    fn pacing_thresholds_are_strict_and_histogram_overflow_keeps_long_stalls() {
        let mut i = Instruments::new(true, 22.0);
        i.perf_freq = 60_000.0;
        let mut stamp = 100;
        present_at(&mut i, stamp);
        // Exact 1/60, 1/30, 1/20, 1/10 second boundaries and one tick above each, then 500ms.
        for interval in [1000, 1001, 2000, 2001, 3000, 3001, 6000, 6001, 30_000] {
            stamp += interval;
            present_at(&mut i, stamp);
        }
        assert_eq!(i.pacing.histogram[256], 1);
        let tail = i.heartbeat_tail(HeartbeatFields::default(), None);
        assert!(tail.contains(" frame_n=9 frame_gt16=8 frame_gt33=6 frame_gt50=4 frame_gt100=2 frame_max=500.0ms frame_p95=500.0ms frame_p99=500.0ms"), "{tail}");
    }

    #[test]
    fn pacing_percentiles_use_nearest_rank_and_round_up_to_integer_ms() {
        let mut pacing = FramePacing::default();
        for _ in 0..95 {
            pacing.note(10_100, 1_000_000);
        }
        for _ in 0..3 {
            pacing.note(30_000, 1_000_000);
        }
        pacing.note(100_000, 1_000_000);
        pacing.note(200_000, 1_000_000);
        assert_eq!(pacing.percentile(95), 11.0);
        assert_eq!(pacing.percentile(99), 100.0);
        assert_eq!(pacing.percentile(100), 200.0);
    }

    #[test]
    fn heartbeat_resets_pacing_samples_but_preserves_consecutive_present_cadence() {
        let mut i = Instruments::new(true, 22.0);
        present_at(&mut i, 100);
        present_at(&mut i, 116);
        assert!(i
            .heartbeat_tail(HeartbeatFields::default(), None)
            .contains(" frame_n=1 "));
        assert_eq!(i.previous_present, Some(116));
        assert_eq!(i.pacing.n, 0);
        present_at(&mut i, 166);
        let tail = i.heartbeat_tail(HeartbeatFields::default(), None);
        assert!(
            tail.contains(
                " frame_n=1 frame_gt16=1 frame_gt33=1 frame_gt50=0 frame_gt100=0 frame_max=50.0ms"
            ),
            "{tail}"
        );
        let empty = i.heartbeat_tail(HeartbeatFields::default(), None);
        assert!(empty.contains(" frame_n=0 frame_gt16=0 frame_gt33=0 frame_gt50=0 frame_gt100=0 frame_max=0.0ms frame_p95=0.0ms frame_p99=0.0ms"), "{empty}");
    }

    /// The defect this fixed: the counters were drained by the `extra()` closure, which runs only
    /// AFTER the threshold check — so a slow frame's `up=`/`cards=` covered every frame since the
    /// previous slow one. The loop hands each presented frame's own counts to
    /// `note_frame_counters` now; two frames apart, the second line carries only the second's.
    #[test]
    fn a_frame_drop_line_carries_this_frames_counters_and_not_the_last_ones() {
        let mut i = Instruments::new(true, 10.0);
        i.perf_freq = 1000.0;
        // frame 1: nine uploads, but a 5 ms frame — under the threshold, so NO line
        i.stamps = [0, 1, 1, 1, 1, 2, 4, 4, 5];
        i.note_frame_counters(FrameCounters {
            uploads: 9,
            upload_px: 843_750,
            cards: 40,
            cards_off: 3,
        });
        assert!(i.frame_drop_line(&|| "route=detail".into()).is_none());
        // frame 2: nothing uploaded, and slow — the line must not inherit frame 1's nine
        i.stamps = [0, 1, 2, 3, 4, 5, 40, 41, 42];
        i.note_frame_counters(FrameCounters {
            uploads: 0,
            upload_px: 0,
            cards: 12,
            cards_off: 0,
        });
        let line = i.frame_drop_line(&|| "route=detail".into()).unwrap();
        assert!(line.contains(" up=0 px=0 cards=12 off=0 "), "{line}");
    }

    #[test]
    fn a_mount_produces_exactly_one_cold_open_line_at_its_first_prepared_and_drawn_frame() {
        let mut c = ColdOpens::default();
        c.mounted(7, "detail", 1_000);
        // a frame that prepared and drew SOMETHING ELSE says nothing about instance 7
        c.note_prepare(0);
        c.drawn(4, 1_000, 0);
        assert!(
            c.take_lines().is_empty(),
            "another body's draw is not this mount's"
        );
        // …and the frame it does draw on: 40 ms later, budget refused nothing
        c.note_prepare(3);
        c.drawn(7, 1_040, 3);
        assert_eq!(
            c.take_lines(),
            vec!["coldopen screen=detail ms=40 prepared=true".to_string()]
        );
        // never a second line for the same instance
        c.note_prepare(3);
        c.drawn(7, 1_200, 3);
        assert!(c.take_lines().is_empty(), "one line per mount");
    }

    #[test]
    fn a_refusal_inside_the_first_drawn_frames_prepare_window_makes_it_unprepared() {
        let mut c = ColdOpens::default();
        c.mounted(2, "home", 500);
        c.note_prepare(11);
        c.drawn(2, 500, 12); // one take refused between the window opening and the draw
        assert_eq!(
            c.take_lines(),
            vec!["coldopen screen=home ms=0 prepared=false".to_string()]
        );
    }

    #[test]
    fn a_body_unmounted_before_it_ever_drew_reports_nothing() {
        let mut c = ColdOpens::default();
        c.mounted(5, "item_menu", 10);
        c.unmounted(5);
        c.note_prepare(0);
        c.drawn(5, 90, 0);
        assert!(c.take_lines().is_empty());
    }

    #[test]
    fn pending_cold_opens_are_bounded() {
        let mut c = ColdOpens::default();
        for id in 0..(COLD_PENDING_MAX as u32 + 4) {
            c.mounted(id, "home", id);
        }
        assert_eq!(c.pending.len(), COLD_PENDING_MAX);
        // the OLDEST were dropped: id 0 no longer reports, the newest still does
        c.note_prepare(0);
        c.drawn(0, 100, 0);
        assert!(c.take_lines().is_empty());
        c.drawn(COLD_PENDING_MAX as u32 + 3, 100, 0);
        assert_eq!(c.take_lines().len(), 1);
    }
}
