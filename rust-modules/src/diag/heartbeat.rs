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
//! Two things here are armed and the rest is not, and the split is the measurement's cost: the
//! eight phases and the two peaks need a clock read per phase, so they hide behind
//! `plxnative-framedrop`; every other field is a counter somebody already keeps, so it prints in
//! every build. The one §8.4 names and this module does NOT have is `allocs=` — see
//! `heartbeat_tail`'s doc for why a counting allocator was not invented to fill it.
//!
//! Armed by `plxnative-framedrop[=<ms>]`; unarmed, every stamp is the frame's origin and every
//! phase reads 0.0 — the counter is never read, so an unarmed frame pays nothing.

#[cfg(not(test))]
extern "C" {
    fn SDL_GetPerformanceCounter() -> u64;
    fn SDL_GetPerformanceFrequency() -> u64;
}
// The host test binary links no SDL: the tests set the stamps and the frequency directly.
#[cfg(test)]
#[allow(non_snake_case)] // the SDL name, so the call sites read the same in both builds
unsafe fn SDL_GetPerformanceCounter() -> u64 {
    0
}
#[cfg(test)]
#[allow(non_snake_case)]
unsafe fn SDL_GetPerformanceFrequency() -> u64 {
    1000
}

/// The eight phases, as stamp indices: `mark(Phase::X)` stamps the END of phase X.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub(crate) enum Phase {
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
pub(crate) fn now_us() -> u64 {
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
pub(crate) struct FrameCounters {
    pub(crate) uploads: u32,
    pub(crate) upload_px: u64,
    pub(crate) cards: u32,
    pub(crate) cards_off: u32,
}

/// The heartbeat's frame-plan fields (spec §8.4), gathered by the loop once a second and printed
/// after `worstprep=` — before `rec=`/`sim=1`, so `tests/run.py`'s `FPS_RE` and `WORST_RE` anchors
/// are untouched. Plain scalars rather than the frame plan's own types: this module is `diag/`
/// and has no business naming `ui::frame::budget::Class`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct HeartbeatFields {
    /// Effects the dispatcher carried into the next frame at the last frame of this second.
    pub(crate) carried: usize,
    /// Deliveries dropped since the previous heartbeat (an undeliverable addressee, a full lane).
    pub(crate) dropped: u32,
    /// `Budget` takes admitted / refused since the previous heartbeat, and the solo class if one
    /// was admitted in that window.
    pub(crate) admitted: u32,
    pub(crate) refused: u32,
    pub(crate) solo: Option<&'static str>,
    /// Glyph-cache entries evicted while still inside their hot window (`text::take_evicted_hot`).
    pub(crate) evicted_hot: u32,
}

pub(crate) struct Instruments {
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
}

impl Instruments {
    /// `armed` is the trigger's presence; `thresh_ms` its content (default 22).
    pub(crate) fn new(armed: bool, thresh_ms: f64) -> Self {
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
        }
    }

    /// This frame's counters, REPLACING the last frame's — see [`FrameCounters`] for why that
    /// word is the whole point. Called once per presented frame, before [`Self::frame_drop_line`].
    pub(crate) fn note_frame_counters(&mut self, c: FrameCounters) {
        self.counters = c;
    }

    fn ms(&self, ticks: u64) -> f64 {
        ticks as f64 * 1000.0 / self.perf_freq
    }

    /// Stamp the end of a phase. Unarmed, a phase inherits the origin so every span reads 0.
    pub(crate) fn mark(&mut self, phase: Phase) {
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

    /// A frame that does not present has no draw/capture/swap: those phases END where prepare did.
    pub(crate) fn skip_present_phases(&mut self) {
        let p = self.stamps[Phase::Prepare as usize];
        self.stamps[Phase::Draw as usize] = p;
        self.stamps[Phase::Capture as usize] = p;
        self.stamps[Phase::Swap as usize] = p;
    }

    /// After `mark(Prepare)`: fold this iteration's prepare span into the per-second peak.
    pub(crate) fn note_prepare(&mut self) {
        if !self.armed {
            return;
        }
        let prep = self.ms(
            self.stamps[Phase::Prepare as usize].wrapping_sub(self.stamps[Phase::TickDrain as usize]),
        );
        if prep > self.worst_prep {
            self.worst_prep = prep;
        }
    }

    fn span(&self, phase: Phase) -> f64 {
        let i = phase as usize;
        self.ms(self.stamps[i].wrapping_sub(self.stamps[i - 1]))
    }

    /// At the iteration's tail of a PRESENTED frame: fold the total into the peak and return the
    /// `FRAMEDROP` line when it crossed the threshold. The four per-frame counters come from
    /// [`Self::note_frame_counters`] — this frame's, not the accumulation since the last drop
    /// line — and `extra` is the loop's own trailing fields (route, load, snap), appended
    /// verbatim after them.
    pub(crate) fn frame_drop_line(&mut self, extra: &dyn Fn() -> String) -> Option<String> {
        if !self.armed {
            return None;
        }
        let total = self.ms(self.stamps[Phase::Swap as usize].wrapping_sub(self.stamps[Phase::Top as usize]));
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

    /// The heartbeat's trailing fields, and the per-second reset. The WIRE ORDER is a contract:
    ///
    /// ```text
    /// … fps=<n> [load= snap= period=] [worstframe= worstprep=] carried= dropped= budget= evicted_hot= [rec=] [sim=1]
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
    /// * `rec_us` is the recorder's spend this second (spec §5.3): it rides after all of them,
    ///   and its presence is what disqualifies the run's `fps=`/`worstframe=` in `tests/run.py`,
    ///   exactly as the profiler triggers do — a recorder perturbs the pacing it feeds.
    ///
    /// There is deliberately NO `allocs=` field, which §8.4 names for `hostsim`. This crate has
    /// no `GlobalAlloc` and phase 11 did not add one: a counting allocator is a whole-process
    /// instrument with its own correctness argument, and inventing one to fill a heartbeat field
    /// would be the more expensive half of the work with none of the evidence.
    pub(crate) fn heartbeat_tail(&mut self, f: HeartbeatFields, rec_us: Option<u64>) -> String {
        let mut s = String::new();
        if self.armed {
            s = format!(" worstframe={:.1}ms worstprep={:.1}ms", self.worst, self.worst_prep);
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
pub(crate) struct ColdOpens {
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
    pub(crate) fn mounted(&mut self, id: u32, screen: &'static str, now_ms: u32) {
        if self.pending.iter().any(|p| p.id == id) {
            return;
        }
        if self.pending.len() >= COLD_PENDING_MAX {
            self.pending.remove(0);
        }
        self.pending.push(Pending { id, screen, at_ms: now_ms });
    }

    /// A body was unmounted without ever drawing: forget it rather than report it late.
    pub(crate) fn unmounted(&mut self, id: u32) {
        self.pending.retain(|p| p.id != id);
    }

    /// The prepare window opened: remember the budget's refusal count to difference at the draw.
    pub(crate) fn note_prepare(&mut self, refused: u32) {
        self.refused_at_prepare = refused;
    }

    /// A body drew this frame, on a frame whose prepare pass ran. `refused_now` is the budget's
    /// refusal count at the draw; the line is queued for [`Self::take_lines`].
    pub(crate) fn drawn(&mut self, id: u32, now_ms: u32, refused_now: u32) {
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

    pub(crate) fn take_lines(&mut self) -> Vec<String> {
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

    #[test]
    fn the_frame_drop_line_carries_the_eight_phases_in_the_specs_order_and_the_extra() {
        let mut i = Instruments::new(true, 0.0); // threshold 0: every frame is a drop
        i.perf_freq = 1000.0; // one tick per ms, so spans are readable
        i.stamps = [0, 1, 3, 6, 10, 15, 21, 28, 36];
        i.note_frame_counters(FrameCounters { uploads: 2, upload_px: 187_500, cards: 9, cards_off: 1 });
        let line = i.frame_drop_line(&|| "route=home".into()).unwrap();
        assert_eq!(
            line,
            "FRAMEDROP total=36.0 ingest=1.0 results=2.0 tick_drain=4.0 navcommit=3.0 prepare=5.0 \
             draw=6.0 capture=7.0 swap=8.0 up=2 px=187500 cards=9 off=1 route=home"
        );
        assert_eq!(
            i.heartbeat_tail(HeartbeatFields::default(), None),
            " worstframe=36.0ms worstprep=0.0ms carried=0 dropped=0 budget=0/0 evicted_hot=0"
        );
        assert_eq!(
            i.heartbeat_tail(HeartbeatFields::default(), Some(17)),
            " worstframe=0.0ms worstprep=0.0ms carried=0 dropped=0 budget=0/0 evicted_hot=0 rec=17us",
            "reset per second; rec= rides last"
        );
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
        i.note_frame_counters(FrameCounters { uploads: 9, upload_px: 843_750, cards: 40, cards_off: 3 });
        assert!(i.frame_drop_line(&|| "route=detail".into()).is_none());
        // frame 2: nothing uploaded, and slow — the line must not inherit frame 1's nine
        i.stamps = [0, 1, 2, 3, 4, 5, 40, 41, 42];
        i.note_frame_counters(FrameCounters { uploads: 0, upload_px: 0, cards: 12, cards_off: 0 });
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
        assert!(c.take_lines().is_empty(), "another body's draw is not this mount's");
        // …and the frame it does draw on: 40 ms later, budget refused nothing
        c.note_prepare(3);
        c.drawn(7, 1_040, 3);
        assert_eq!(c.take_lines(), vec!["coldopen screen=detail ms=40 prepared=true".to_string()]);
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
        assert_eq!(c.take_lines(), vec!["coldopen screen=home ms=0 prepared=false".to_string()]);
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
