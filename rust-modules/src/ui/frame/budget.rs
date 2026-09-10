/// ≈25 % of the measured ~7.8 ms discretionary headroom on a Home frame.
pub const PREPARE_MAX_US: u64 = 2000;

/// The ceiling a SOLO frame runs under (§8.1). A class whose worst case cannot fit
/// [`PREPARE_MAX_US`] is not ordinary prepare work: it is admitted at most once per frame, only
/// as that frame's FIRST take, and it then owns the frame — every other take is refused. The
/// frame it lands on is a long one by construction; the point of the rule is that it is long
/// ALONE, rather than long on top of three poster uploads.
pub const SOLO_MAX_US: u64 = 8000;

/// A solo frame logs one line. An oscillator sweeping a shelf of backdrops would otherwise put a
/// line on the event log every few frames — the log is the primary debugging surface and a
/// diagnostic that drowns it is worse than none — so the line is rate-limited to at most one per
/// second of the budget's own frame clock. The count is not lost: `admitted`/`refused` and the
/// `solo` field of [`FrameStats`] ride the heartbeat, which is where a rate belongs.
const SOLO_LOG_GAP_US: u64 = 1_000_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    /// One decoded poster's GL upload (a 250x375 poster is ≈375 KB). Quota 3 per frame.
    Poster,
    /// The GL upload + `warm_tex` pair for a LARGE decoded image — a 1280x720 backdrop (≈3.7 MB)
    /// or a hero logo (up to 1.44 MB; `app/adapters/poster.rs`'s request boxes). A SOLO class:
    /// its worst case is over [`PREPARE_MAX_US`], so it takes the frame to itself.
    Residency,
}

impl Class {
    /// The device-measured worst case. Two of them, and they are not the same KIND of number:
    ///
    /// * `Poster` — **[?] still unmeasured cleanly; TV session 7 legs 2-4 tried and the attempt is
    ///   the finding.** 400 µs remains the placeholder — **not applied by choice**, because the
    ///   naive derivation this session ran breaks the admission system it would feed. Derivation
    ///   attempted: every `FRAMEDROP` line across legs 2/3/4 (both A and B passes, ~2400 lines)
    ///   filtered to `up=1 cards=1` with `px` matching a poster's decoded size (90000-93750, the
    ///   240x375/250x375 dims this variant's own doc names) — 16 clean, single-upload, single-card
    ///   samples, `prepare=` 7.8-26.4 ms (median 13.1 ms; excluding `page-panel`, the one scene
    ///   with its own per-frame glass/blur source pass as a plausible confound, still 7.8-18.1 ms,
    ///   n=8, median 12.0 ms — not a page-panel artifact). An `up=0` baseline sanity-checked it:
    ///   median 0.0 ms of `prepare=` across ~2500 no-upload frames, so the nonzero values are not
    ///   baseline noise.
    ///
    ///   **Why the raw number was not pinned here.** 26400 µs is over not only [`PREPARE_MAX_US`]
    ///   but [`SOLO_MAX_US`] as well (8000 µs) — and [`Class::is_solo`]'s admission rule for a solo
    ///   class is `elapsed + worst_us() <= SOLO_MAX_US` with **no forward-progress escape** (see
    ///   [`Budget::take`]'s doc, rule 2: "not first means wait for a frame of your own"). A class
    ///   whose OWN `worst_us()` already exceeds `SOLO_MAX_US`, independent of `elapsed`, is refused
    ///   on every frame, forever — setting this constant to the raw measurement would make the app
    ///   stop loading posters entirely, which was caught only by running the existing test suite
    ///   (`a_class_over_the_ceiling_is_a_solo_frame` and the two per-frame-quota tests all fail,
    ///   the first by design — `Poster` really would become solo — the other two because the
    ///   admission path a solo `Poster` takes has no working case left at this magnitude). That is
    ///   strong evidence for reading (b) below, not a green light to apply the number anyway.
    ///
    ///   Two readings of the data, left open rather than picked under a TV-session deadline: (a)
    ///   the placeholder undersold the real GL-upload-plus-decode cost and `Poster` genuinely
    ///   belongs in the solo tier, which would also mean its quota (currently 3) and every screen's
    ///   assumption of batched poster admission need redesigning together, not a one-line constant
    ///   edit; (b) `prepare=` times more than the upload itself (layout/hit-test/animation resolve
    ///   for the WHOLE frame the upload happened to land on, not the upload in isolation), so 7.8-
    ///   26.4 ms is an upper bound on the upload proper rather than a clean isolate of it, and the
    ///   true `Poster` cost may still be well under [`PREPARE_MAX_US`]. Settling which reading is
    ///   right needs an isolated micro-benchmark (one upload with nothing else changing on the
    ///   page, e.g. a scene that seeds exactly one new poster and nothing else per frame) rather
    ///   than another heartbeat grep across mixed scenes — flagged for whoever picks this up next,
    ///   with the raw samples and this reasoning rather than a number that was never cleanly
    ///   isolated.
    /// * `Residency` — **[M-dev]** 6 ms, device-measured 2026-09-02 with `plxnative-framedrop`:
    ///   a 1280x720 backdrop landing cost 6 ms in the pump and 116 ms in the NEXT frame's draw
    ///   without the `warm_tex` that now follows it (`gfx.rs`'s `warm_tex`, whose doc is the
    ///   record). That 6 ms is the pair this class prices.
    pub const fn worst_us(self) -> u64 {
        match self {
            Class::Poster => 400,
            Class::Residency => 6000,
        }
    }

    const fn quota(self) -> u8 {
        match self {
            Class::Poster => 3,
            Class::Residency => 1,
        }
    }

    /// A class whose worst case cannot fit the ordinary ceiling is not ordinary prepare work
    /// (§8.1). Derived from [`worst_us`](Self::worst_us) rather than declared, so a class cannot
    /// be given a worst case over the ceiling and quietly keep sharing frames.
    pub const fn is_solo(self) -> bool {
        self.worst_us() > PREPARE_MAX_US
    }

    /// The word the `budget solo=` line and the heartbeat use.
    pub const fn name(self) -> &'static str {
        match self {
            Class::Poster => "poster",
            Class::Residency => "residency",
        }
    }
}

/// What the heartbeat reads (`admitted=`, `refused=`, `solo=`), drained by
/// [`Budget::take_frame_stats`]. The counters accumulate ACROSS frames until they are taken, so
/// a once-per-second heartbeat reports the second rather than whichever frame it happened to
/// land on; `solo` is "a solo take was admitted since the last read", and names the class.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FrameStats {
    pub admitted: u32,
    pub refused: u32,
    pub solo: Option<Class>,
}

pub struct Budget {
    frame_start_us: u64,
    poster_left: u8,
    residency_left: u8,
    queued: bool,
    /// Takes admitted in THIS frame: the forward-progress escape and the solo-first rule read it.
    window_admitted: u32,
    /// This frame's solo take, if one was admitted.
    solo: Option<Class>,
    /// Since the last [`take_frame_stats`](Budget::take_frame_stats).
    admitted: u32,
    refused: u32,
    stats_solo: Option<Class>,
    /// The frame clock of the last `budget solo=` line, for the rate limit.
    solo_logged_us: Option<u64>,
    /// `plxnative-nobudget`: the budget as it was BEFORE phase 11 — quota only, no time ceiling,
    /// no solo rule, `Residency` spending the `Poster` quota. The A/B control leg.
    relaxed: bool,
}

impl Budget {
    pub fn new() -> Self {
        Self {
            frame_start_us: 0,
            poster_left: 0,
            residency_left: 0,
            queued: false,
            window_admitted: 0,
            solo: None,
            admitted: 0,
            refused: 0,
            stats_solo: None,
            solo_logged_us: None,
            relaxed: false,
        }
    }

    /// The `plxnative-nobudget` control leg: admission as it was before phase 11 — the `Poster`
    /// quota of 3 per frame and nothing else. The time ceiling is off, the solo rule is off, and
    /// `Residency` is treated as `Poster` (before phase 11 there was no such class: every upload,
    /// backdrop included, spent one of the three). It exists so a device A/B measures THIS
    /// change and not the difference between two builds.
    pub fn pre_phase_11() -> Self {
        Self {
            relaxed: true,
            ..Self::new()
        }
    }

    /// Opens a frame: quotas reset, the clock origin recorded, the solo latch cleared.
    ///
    /// **The origin is the start of the PREPARE WINDOW, not the top of the iteration.** It was
    /// the loop top until phase 11, which made the ceiling a test of how long ingest, the result
    /// landings, the nav commit and the tick drain had already taken — on a cold-open frame
    /// (~62 ms) the ceiling refused every upload and only the forward-progress escape below let
    /// ONE through, so the quota of three was silently a quota of one on exactly the frames that
    /// had the most textures waiting.
    pub fn begin_frame(&mut self, now_us: u64) {
        self.frame_start_us = now_us;
        self.poster_left = Class::Poster.quota();
        self.residency_left = Class::Residency.quota();
        self.window_admitted = 0;
        self.solo = None;
    }

    fn left(&mut self, class: Class) -> &mut u8 {
        match class {
            Class::Poster => &mut self.poster_left,
            Class::Residency => &mut self.residency_left,
        }
    }

    fn refuse(&mut self) -> bool {
        self.refused += 1;
        false
    }

    fn admit(&mut self, class: Class) {
        *self.left(class) -= 1;
        self.window_admitted += 1;
        self.admitted += 1;
    }

    /// Admission. The caller reads the clock BEFORE every take and hands it in — one
    /// `SDL_GetPerformanceCounter` per take, never a stale value — and is admitted only if the
    /// class's worst case still fits the ceiling and its quota is not spent.
    ///
    /// Three rules on top of that, in the order they apply:
    ///
    /// 1. **A solo frame is spent.** Once a solo class has been admitted this frame, every
    ///    further take is refused: that is what makes the raised ceiling safe.
    /// 2. **A solo class is admitted only as the frame's FIRST take**, under [`SOLO_MAX_US`].
    ///    Not first means "wait for a frame of your own", not "shrink" — there is no shrinking a
    ///    texture upload. It gets no forward-progress escape, which is the one place this could
    ///    in principle stall: a queue head that is over the raised ceiling on every frame would
    ///    never upload while `has_queued_work` kept forcing the frame. It cannot sustain itself
    ///    on the loop as written — the only thing that spends milliseconds between `begin_frame`
    ///    and this take is the drain of newly DECODED images, which empties each frame, so the
    ///    frame after a slow one opens with an elapsed of microseconds and admits. TV session 7
    ///    is where that argument gets a measurement rather than a derivation.
    /// 3. **Forward progress** for the classes that fit: the FIRST take of a frame is always
    ///    admitted, so a queue cannot stall behind a ceiling it can never satisfy.
    pub fn take(&mut self, class: Class, now_us: u64) -> bool {
        if self.relaxed {
            // pre-phase-11: one pool of three, no clock at all.
            if self.poster_left == 0 {
                return self.refuse();
            }
            self.poster_left -= 1;
            self.window_admitted += 1;
            self.admitted += 1;
            return true;
        }
        if self.solo.is_some() {
            return self.refuse();
        }
        if *self.left(class) == 0 {
            return self.refuse();
        }
        let elapsed = now_us.saturating_sub(self.frame_start_us);
        if class.is_solo() {
            if self.window_admitted != 0 || elapsed + class.worst_us() > SOLO_MAX_US {
                return self.refuse();
            }
            self.admit(class);
            self.solo = Some(class);
            self.stats_solo = Some(class);
            self.note_solo(class);
            return true;
        }
        if elapsed + class.worst_us() <= PREPARE_MAX_US || self.window_admitted == 0 {
            self.admit(class);
            true
        } else {
            self.refuse()
        }
    }

    /// One `budget solo=<class>` line per solo frame, rate-limited to one per
    /// [`SOLO_LOG_GAP_US`] of the frame clock.
    fn note_solo(&mut self, class: Class) {
        let now = self.frame_start_us;
        let due = match self.solo_logged_us {
            None => true,
            Some(t) => now.saturating_sub(t) >= SOLO_LOG_GAP_US,
        };
        if due {
            self.solo_logged_us = Some(now);
            crate::log(&format!("budget solo={}", class.name()));
        }
    }

    /// Whether prepare work is waiting — the second term of §3.3 step 8's present decision.
    pub fn has_queued_work(&self) -> bool {
        self.queued
    }

    /// Reported by whoever holds a queue (`TexCache`, through `ui::tex::note_queued`) before the
    /// present decision reads it.
    pub fn note_queued(&mut self, queued: bool) {
        self.queued = queued;
    }

    /// This frame's solo take, if one was admitted.
    pub fn solo(&self) -> Option<Class> {
        self.solo
    }

    pub fn admitted(&self) -> u32 {
        self.admitted
    }

    pub fn refused(&self) -> u32 {
        self.refused
    }

    /// Drain the counters for the heartbeat (§8.4). Once per report, not once per frame — see
    /// [`FrameStats`].
    pub fn take_frame_stats(&mut self) -> FrameStats {
        let s = FrameStats {
            admitted: self.admitted,
            refused: self.refused,
            solo: self.stats_solo,
        };
        self.admitted = 0;
        self.refused = 0;
        self.stats_solo = None;
        s
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_poster_quota_is_three_per_frame_and_the_ceiling_refuses_late_work() {
        let mut b = Budget::new();
        b.begin_frame(1000);
        assert!(b.take(Class::Poster, 1000));
        assert!(b.take(Class::Poster, 1100));
        assert!(b.take(Class::Poster, 1200));
        assert!(!b.take(Class::Poster, 1300), "quota spent");
        b.begin_frame(5000);
        assert!(b.take(Class::Poster, 5000 + PREPARE_MAX_US), "the first take always fits");
        assert!(!b.take(Class::Poster, 5000 + PREPARE_MAX_US), "the second does not");
        assert_eq!(b.refused(), 2, "the counters accumulate until they are taken");
        let s = b.take_frame_stats();
        assert_eq!((s.admitted, s.refused, s.solo), (4, 2, None));
        assert_eq!(b.take_frame_stats(), FrameStats::default(), "taking drains them");
    }

    /// Deliverable 2's property, and the defect it replaces. **The red was SIMULATED**: the
    /// placement lives in `app/run.rs`'s frame loop, which no host test drives, so the old
    /// behaviour is reproduced here by opening the frame where the old loop opened it.
    #[test]
    fn the_poster_quota_is_three_on_a_frame_whose_earlier_phases_cost_60ms() {
        // A cold-open iteration: ingest, the result landings, the nav commit and the tick drain
        // have already cost 60 ms when the prepare window opens.
        const ITERATION_TOP_US: u64 = 0;
        const PREPARE_WINDOW_US: u64 = 60_000;

        // phase 11: the frame is opened at the start of the prepare window.
        let mut b = Budget::new();
        b.begin_frame(PREPARE_WINDOW_US);
        assert!(b.take(Class::Poster, PREPARE_WINDOW_US + 20));
        assert!(b.take(Class::Poster, PREPARE_WINDOW_US + 440));
        assert!(b.take(Class::Poster, PREPARE_WINDOW_US + 860));
        assert_eq!(b.take_frame_stats().admitted, 3, "the quota is three");

        // before it: opened at the top of the iteration, so the ceiling was already 58 ms past
        // and only the forward-progress escape got a texture through.
        let mut old = Budget::new();
        old.begin_frame(ITERATION_TOP_US);
        assert!(old.take(Class::Poster, PREPARE_WINDOW_US + 20), "the escape admits one");
        assert!(!old.take(Class::Poster, PREPARE_WINDOW_US + 440), "and only one");
        let s = old.take_frame_stats();
        assert_eq!((s.admitted, s.refused), (1, 1), "a quota of three that was really one");
    }

    /// §15.1.
    #[test]
    fn a_class_over_the_ceiling_is_a_solo_frame() {
        assert!(Class::Residency.is_solo() && !Class::Poster.is_solo());
        assert!(Class::Residency.worst_us() > PREPARE_MAX_US);

        // admitted as the frame's first take, at the raised ceiling; the frame is then spent
        let mut b = Budget::new();
        b.begin_frame(0);
        assert!(b.take(Class::Residency, 100));
        assert_eq!(b.solo(), Some(Class::Residency));
        assert!(!b.take(Class::Poster, 200), "after a solo take the frame is spent");
        assert!(!b.take(Class::Residency, 300), "including a second solo take");
        let s = b.take_frame_stats();
        assert_eq!((s.admitted, s.refused, s.solo), (1, 2, Some(Class::Residency)));

        // a Residency take that is NOT the frame's first is refused, and takes no solo latch
        let mut b = Budget::new();
        b.begin_frame(0);
        assert!(b.take(Class::Poster, 0));
        assert!(!b.take(Class::Residency, 100), "a solo class goes first or not at all");
        assert_eq!(b.solo(), None);

        // the raised ceiling is a ceiling: SOLO_MAX_US, and no forward-progress escape past it
        let mut b = Budget::new();
        b.begin_frame(0);
        assert!(!b.take(Class::Residency, SOLO_MAX_US - Class::Residency.worst_us() + 1));
        assert_eq!(b.solo(), None);
        b.begin_frame(0);
        assert!(b.take(Class::Residency, SOLO_MAX_US - Class::Residency.worst_us()));

        // the next frame is ordinary again
        b.begin_frame(10_000);
        assert!(b.take(Class::Poster, 10_000));
        assert_eq!(b.solo(), None);
    }

    /// The A/B control leg (`plxnative-nobudget`).
    #[test]
    fn the_pre_phase_11_budget_is_quota_only() {
        let mut b = Budget::pre_phase_11();
        b.begin_frame(0);
        // a clock reading a hundred milliseconds past the ceiling admits all the same
        assert!(b.take(Class::Poster, 100_000));
        assert!(b.take(Class::Residency, 100_000), "Residency spends the Poster quota");
        assert!(b.take(Class::Poster, 100_000));
        assert!(!b.take(Class::Poster, 100_000), "the quota is the only bound");
        assert_eq!(b.solo(), None, "no solo rule");
        let s = b.take_frame_stats();
        assert_eq!((s.admitted, s.refused, s.solo), (3, 1, None));
    }
}
