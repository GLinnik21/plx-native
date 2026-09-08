//! `Instruments` — the frame's own measurement (restructure spec §8.4): the eight phase stamps
//! over the frame algorithm's steps, the per-second `worstframe=`/`worstprep=` peaks the heartbeat
//! carries, and the `FRAMEDROP` line. Lifted out of `app/run.rs` in phase 2 so the loop reads as
//! its algorithm and the instrument is one value with one owner (`App.instr`).
//!
//! The wire formats are the harness's contract (`tests/run.py`: `FRAMEDROP_RE`, `WORST_RE`,
//! `FPS_RE`) and are byte-identical to what the loop wrote before this module existed: the
//! FRAMEDROP fields in the spec's order (ingest results tick_drain navcommit prepare draw capture
//! swap), `worstframe=` LAST on the heartbeat. `coldopen`, `carried=` and `rec=` join when the
//! dispatcher runs the product loop (spec §8.4).
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

pub(crate) struct Instruments {
    armed: bool,
    thresh_ms: f64,
    perf_freq: f64,
    stamps: [u64; 9],
    /// Worst frame total this second (presented frames only), for the heartbeat peak.
    worst: f64,
    /// Worst prepare phase this second, timed on EVERY iteration (ungated by present).
    worst_prep: f64,
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
        }
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
    /// `FRAMEDROP` line when it crossed the threshold. `extra` is the loop's own trailing fields
    /// (upload counts, card stats, route, load, snap), appended verbatim.
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
        Some(format!(
            "FRAMEDROP total={total:.1} ingest={:.1} results={:.1} tick_drain={:.1} navcommit={:.1} prepare={:.1} draw={:.1} capture={:.1} swap={:.1} {}",
            self.span(Phase::Ingest),
            self.span(Phase::Results),
            self.span(Phase::TickDrain),
            self.span(Phase::NavCommit),
            self.span(Phase::Prepare),
            self.span(Phase::Draw),
            self.span(Phase::Capture),
            self.span(Phase::Swap),
            extra(),
        ))
    }

    /// The heartbeat's trailing fields — `worstframe=` stays LAST of the graded fields (the
    /// harness reads it there) — and the per-second reset. Empty when unarmed. `rec_us` is the
    /// recorder's spend this second (spec §5.3): it rides AFTER the graded fields, and its
    /// presence is what disqualifies the run's `fps=`/`worstframe=` in `tests/run.py`, exactly
    /// as the profiler triggers do — a recorder perturbs the pacing it feeds.
    pub(crate) fn heartbeat_tail(&mut self, rec_us: Option<u64>) -> String {
        let mut s = String::new();
        if self.armed {
            s = format!(" worstframe={:.1}ms worstprep={:.1}ms", self.worst, self.worst_prep);
            self.worst = 0.0;
            self.worst_prep = 0.0;
        }
        if let Some(us) = rec_us {
            s.push_str(&format!(" rec={us}us"));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unarmed_instrument_reads_zero_and_writes_nothing() {
        let mut i = Instruments::new(false, 22.0);
        i.mark(Phase::Top);
        i.mark(Phase::Ingest);
        i.mark(Phase::Prepare);
        i.note_prepare();
        i.skip_present_phases();
        assert!(i.frame_drop_line(&|| String::new()).is_none());
        assert_eq!(i.heartbeat_tail(None), "");
    }

    #[test]
    fn the_frame_drop_line_carries_the_eight_phases_in_the_specs_order_and_the_extra() {
        let mut i = Instruments::new(true, 0.0); // threshold 0: every frame is a drop
        i.perf_freq = 1000.0; // one tick per ms, so spans are readable
        i.stamps = [0, 1, 3, 6, 10, 15, 21, 28, 36];
        let line = i.frame_drop_line(&|| "route=home".into()).unwrap();
        assert_eq!(
            line,
            "FRAMEDROP total=36.0 ingest=1.0 results=2.0 tick_drain=4.0 navcommit=3.0 prepare=5.0 draw=6.0 capture=7.0 swap=8.0 route=home"
        );
        assert_eq!(i.heartbeat_tail(None), " worstframe=36.0ms worstprep=0.0ms");
        assert_eq!(i.heartbeat_tail(Some(17)), " worstframe=0.0ms worstprep=0.0ms rec=17us", "reset per second; rec= rides last");
    }
}
