use super::*;

/// Auto's request ladder — **the ACTUATOR set, not a settings menu**.
///
/// Six of these are byte-for-byte aligned with `route::Quality`'s canonical fixed rungs, because a
/// user who picks "1080p · 8 Mbps" by hand and Auto arriving at the same operating point must send
/// the same request. The rest exist only for Auto: `P240` is an emergency floor, and the six
/// 1080p rungs between 6 and 18 Mbps are the resolution this controller needs to spend a measured
/// link instead of rounding it down to the next power of two — a 17.5 Mbit/s link that had to
/// choose between 8 and 20 Mbps spent 12 Mbit/s of it on nothing.
///
/// `Uhd` is the one entry whose REQUEST is not its output. See [`HlsActuatorCatalog`]: PMS holds
/// 1920x1080 up to a 21,750 kbps ask and switches to 3840x2160 at 22,000, so the request is the
/// actuator and the raster is the measured consequence. It is also the one rung
/// [`HlsActuatorCatalog::feasible`] can remove: no device decodes every raster.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Rung {
    P240,
    P480,
    P720Low,
    P720,
    P1080M6,
    P1080,
    P1080M10,
    P1080M12,
    P1080M14,
    P1080M16,
    P1080M18,
    P1080High,
    Uhd,
}

pub(crate) const LADDER: [Rung; 13] = [
    Rung::P240,
    Rung::P480,
    Rung::P720Low,
    Rung::P720,
    Rung::P1080M6,
    Rung::P1080,
    Rung::P1080M10,
    Rung::P1080M12,
    Rung::P1080M14,
    Rung::P1080M16,
    Rung::P1080M18,
    Rung::P1080High,
    Rung::Uhd,
];

/// **How much reserve the dev rung pin waits for before it transacts UPWARD.** Six segments.
///
/// A TOOL constant, not ABR policy: nothing outside [`Controller::pinned_to`] reads it and it
/// takes no part in any decision an unpinned build makes. It exists because a candidate
/// transaction runs inline on the demux worker and costs roughly 2.3 segments of unrefilled
/// playback (`candidate_warmup_budget` + `candidate_prime_budget`), while `candidate_ready`
/// requires two segments of reserve to still be there afterwards. Propose with less and the
/// transaction drains the reserve below its own acceptance test, the candidate is rejected, and
/// the pin re-proposes forever without ever landing — which is not a hypothetical: the closed-loop
/// plant reproduced exactly that livelock the first time this was written without a gate.
///
/// **Every clause of that derivation is an UPSHIFT argument, and applying it downward cost the M4
/// census four of its seven points (five in the corpus before it, where `pin_4000` additionally
/// ran at rung 6000 for a separate reason -- rungs 2000 and 4000 shared one clip).** A downshift
/// has no PRIME budget at all (there is no graded segment to hold to an acceptance threshold) and
/// its warm-up budget is the reserve itself rather than a multiple of the content duration
/// (`abr/viability.rs`), so neither of the two figures this constant sums applies going down.
/// The old note here said "pin UPWARD from the bootstrap rung, which is how measurement step
/// M4 is written" — but on an unshaped LAN `startup_rung` picks the ladder TOP, so every pin in
/// that census was a *downshift*, needing 12 000 ms of reserve at a rung whose reachable ceiling is
/// `B_max(20000) ≈ 5 421 ms`. Unsatisfiable by construction. Device-measured 2026-08-26 and only
/// noticed 2026-08-27: `pin_320`, `pin_2000`, `pin_10000` and `pin_16000` all ran at `rung=20000`
/// with byte lists identical to `pin_20000`'s, so the census recorded the top rung five times and
/// the corpus has three distinct clips of byte support rather than eleven.
///
/// See [`PIN_MIN_RESERVE_SEGMENTS_DOWN`] for what a downshift actually has to afford.
pub(crate) const PIN_MIN_RESERVE_SEGMENTS: i64 = 6;

/// **How much reserve the dev rung pin waits for before it transacts DOWNWARD.** Two segments.
///
/// Derived from what the transaction costs rather than reused from the upshift figure:
///
/// * **One segment** for the candidate's warm-up fetch. The acquisition-transfer bound
///   (`docs/adaptive-playback-spec.md` §2a) gives `A_j ≤ A_i` whenever the candidate's byte count
///   is lower, which a downshift's is by definition — so the warm-up cannot cost more than a
///   current-rung segment, and a rung being sustained costs at most `D`.
/// * **One segment** for the control plane. Three requests (`ff.rs`), measured on a real PMS at a
///   ~100 ms median with a 1 306 ms maximum (`docs/measurements/p2h-pms-ladder.md` §5), which one
///   media duration covers at every `D` the ladder serves.
///
/// No third term: the two deadline budgets that make up the upshift's 2.3 segments do not apply
/// in this direction — there is no graded segment, and the warm-up's budget is the reserve itself
/// (J3b) rather than a multiple of `D` — and `candidate_ready`'s two-segment residual is an
/// *upshift* acceptance test.
///
/// **The deadline makes this figure self-consistent rather than merely expected.** A downshift
/// warm-up is now bounded by the reserve it is spending, so a pin that waits for two segments
/// hands the fetch a budget of at most those two segments less the control plane — the gate and
/// the enforcement are the same quantity read at two moments, which is why neither needs a margin
/// against the other.
///
/// Two segments is 4 000 ms at `D = 2000`, inside `B_max` at every rung including the top, which is
/// the property the six-segment figure lacked and the whole reason this constant exists separately.
pub(crate) const PIN_MIN_RESERVE_SEGMENTS_DOWN: i64 = 2;

impl Rung {
    pub(crate) const fn kbps(self) -> u32 {
        match self {
            Rung::P240 => 320,
            Rung::P480 => 720,
            Rung::P720Low => 2_000,
            Rung::P720 => 4_000,
            Rung::P1080M6 => 6_000,
            Rung::P1080 => 8_000,
            Rung::P1080M10 => 10_000,
            Rung::P1080M12 => 12_000,
            Rung::P1080M14 => 14_000,
            Rung::P1080M16 => 16_000,
            Rung::P1080M18 => 18_000,
            Rung::P1080High => 20_000,
            Rung::Uhd => 22_000,
        }
    }

    /// **`σ` — how far above its DECLARED rate a rendition's segments can actually run**, per-mille.
    ///
    /// The admission rule's candidate query is `σ · W_j · D / 8000` (specification §3), and `σ` is a
    /// property of the encoder at that rung rather than a single constant. It is measured, not
    /// chosen: `max_observed(delivered/declared) × cross-item spread`, both factors classification
    /// (2) and the product (3), over 1 560 segments on three items in five windows.
    ///
    /// | rung | max observed | cross-item spread | `σ` |
    /// |---|---:|---:|---:|
    /// | 320 | 1.285 | *22.7% (borrowed)* | 1.577 |
    /// | 720 | 1.155 | 22.7% | 1.418 |
    /// | 2000 | 0.917 | 13.4% | 1.040 |
    /// | ≥ 4000 | 0.846 | 5.6% | 0.893 |
    ///
    /// **The ladder has two regimes and one constant would be wrong in both.** Above 4000 the
    /// declared rate is a genuine ceiling — 0 of 1 440 segments exceed `0.85·W_j`. Below it the
    /// encoder cannot go under a content-dependent **quality floor**, so a small enough target
    /// loses to it and the delivered rate overshoots; max `σ` decays monotonically across the whole
    /// ladder (1.285, 1.155, 0.917, 0.846 … 0.798), which is one curve crossing a threshold rather
    /// than a bound that holds and then breaks. Applying the floor regime's 1.577 everywhere would
    /// be a 1.77× haircut at exactly the rungs this controller exists to reach; applying 0.893
    /// everywhere would under-state the bottom three by up to 43%, in the permissive direction.
    ///
    /// **Rung 320's spread is BORROWED and that is the one number here that is not measured.** No
    /// second item reached it, so its cross-item variation is unknown; it takes 720's, the largest
    /// in the table and its neighbour in the same floor regime. Stated rather than smoothed over,
    /// because the floor regime is not merely higher but far more item-dependent (22.7% at 720
    /// against 5.6% above 4000), which is precisely why a shared constant is wrong down there.
    ///
    /// **This is a seed, not a guarantee**, and §2a is why that is survivable: the rule re-decides
    /// every segment against a window of real acquisitions, and `bytes=` is logged for every
    /// fetched segment, so a wrong `σ` is visible rather than silent.
    pub(crate) const fn size_spread_pm(self) -> u32 {
        match self {
            Rung::P240 => 1_577,
            Rung::P480 => 1_418,
            Rung::P720Low => 1_040,
            _ => 893,
        }
    }

    pub(crate) const fn raster(self) -> (u16, u16) {
        match self {
            Rung::P240 => (426, 240),
            Rung::P480 => (854, 480),
            Rung::P720Low | Rung::P720 => (1280, 720),
            Rung::P1080M6
            | Rung::P1080
            | Rung::P1080M10
            | Rung::P1080M12
            | Rung::P1080M14
            | Rung::P1080M16
            | Rung::P1080M18
            | Rung::P1080High => (1920, 1080),
            Rung::Uhd => (3840, 2160),
        }
    }

    /// The ladder entry whose REQUEST is exactly `kbps`, or `None`. Actuator identity by the
    /// number that goes on the wire as the ceiling, which is stable across catalog re-measurement
    /// — unlike `planning`/`expected_wire_kbps`, which moves when somebody probes the server, and
    /// unlike the UI quality enum, which has no mid-1080p points at all
    /// (`plex::session::PlaybackQuality`). Used only by the dev rung pin (I0-D).
    pub(crate) fn from_request_kbps(kbps: u32) -> Option<Rung> {
        LADDER.into_iter().find(|r| r.kbps() == kbps)
    }

    pub(crate) fn ceiling(self) -> crate::plex::Ceiling {
        let (width, height) = self.raster();
        crate::plex::Ceiling {
            max_kbps: i64::from(self.kbps()),
            max_w: i64::from(width),
            max_h: i64::from(height),
        }
    }

    fn index(self) -> usize {
        LADDER.iter().position(|r| *r == self).unwrap_or(0)
    }

    /// The next rung down, and **the identity at the floor** — which is what makes it R12's
    /// terminal case in one expression: `below() == self` IS "there is nowhere cheaper to
    /// run to", and `ff.rs`'s abort rule asks it that way rather than naming `P240`.
    pub(crate) fn below(self) -> Self {
        LADDER[self.index().saturating_sub(1)]
    }

    /// **R12's terminal case, named.** `below() == self` is the whole test, but it was written out
    /// at three sites in two files and twice in the negative — `below() != self` for "there IS
    /// somewhere to run to" reads as its own double negative, and a reader has to re-derive the
    /// identity-at-the-floor trick each time to see that the three are one predicate.
    pub(crate) fn at_floor(self) -> bool {
        self.below() == self
    }

    /// Recover the controller's starting rung from the exact ceiling stored in the playback
    /// route. Auto owns only these canonical values; an arbitrary/manual ceiling is not an ABR
    /// state and therefore has no answer here.
    pub(crate) fn from_ceiling(ceiling: crate::plex::Ceiling) -> Option<Self> {
        LADDER.iter().copied().find(|rung| rung.ceiling() == ceiling)
    }
}

/// One PMS operating point. `request_kbps` is what goes on the wire as the ceiling; the other two
/// are what the server was measured to DO with it, and they are separate fields precisely because
/// the request is not a promise in either direction. Read `expected_wire_kbps` with the
/// correction on [`HlsActuatorCatalog`] beside it: for 11 of 13 rungs it simply repeats
/// `request_kbps`, and the server declares 5%–32% less.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HlsCandidate {
    pub(crate) rung: Rung,
    pub(crate) request_kbps: u32,
    pub(crate) expected_wire_kbps: u32,
    /// Relative PMS transcoding work, per mille of the 1080p/20 Mbps point. **Two values are
    /// measured and the rest are an ordering assumption**, which matters when reading a refusal:
    /// the 1080p high point produced segments at a 0.21 production ratio and the 4K point at 0.44,
    /// i.e. **the wire cost rose 4% while the server's work roughly doubled** — so 1000 and 2100
    /// are evidence. Everything else is estimated from RASTER, with only a slight slope across
    /// bitrate at one raster, because that is where a video encoder's cost actually is: decode,
    /// scale and per-pixel analysis dominate, while rate control at a fixed size is nearly free.
    /// The 4K measurement supports exactly that reading — 2.1x the work for the same 1080p-class
    /// bitrate. Used only comparatively ("would this candidate cost the server more than the one
    /// now running"), never as a predicted absolute time.
    ///
    /// # What this table is indexed BY, settled 2026-08-28
    ///
    /// It is indexed by the TARGET RUNG, and M3's re-run shows that cannot be right for every
    /// source: the same rung costs 2.8x-4x differently depending on what it is transcoding FROM.
    /// `Uhd` measures 429 pm against a 4K source and 106 against a 1918x802 one; `P240` measures
    /// 57 against the 4K source and 161 against the smaller one — in the opposite direction. The
    /// reason is structural: PMS never upscales, so the raster it actually produces is
    /// `min(rung box, source)`, and against a small source the top eight rungs all clamp to the
    /// same output and cost the same. Measured spread on that source is **1.53x** against the
    /// 23.3x this table asserts.
    ///
    /// **The decision is to keep the target-only index and state what it describes**: the work of
    /// producing this rung *when the source can supply it*. Two reasons, and the second is the one
    /// that makes it safe rather than merely convenient.
    ///
    /// 1. Against a 4K source — the only class where the two empirical anchors were ever taken —
    ///    the table is good: `Uhd = 2100` measures a 2340-2416 residual (11-15%) and the whole
    ///    1080p block lands within 8%. Re-indexing would re-derive numbers that are already right
    ///    where they are used.
    /// 2. Where the index is wrong, it is wrong in the **inert** direction. On a source below the
    ///    rung's raster the real cost collapses to the source-raster cost, so the table
    ///    OVERSTATES — and an overstated production cost can only make the gate more reluctant to
    ///    climb, never less. There is no 4K work to refuse when the source is 1080p, so a gate
    ///    that never fires there is correct rather than broken.
    ///
    /// What that trades away is stated rather than hidden: on a small source the production
    /// constraint contributes nothing, so the admission rule is effectively network-only there.
    /// That is the status quo, it is safe, and it is not what the two-constraint rule exists for —
    /// the rule exists to refuse 4K on a fast link in front of a loaded PMS, which is precisely
    /// the case a 4K source produces and this table gets right.
    ///
    /// **The alternative, if this ever needs to be exact:** index by the raster PMS actually
    /// produces, `min(rung box, source)`, which the census shows the cost tracks far better than
    /// the rung does. It needs a per-source measurement this project can take (the census does it
    /// in one command) and a `source_raster` already threaded into the catalog by `limited_to`.
    /// It is not built because nothing measured needs it yet.
    pub(crate) production_load_pm: u32,
}

/// The compact actuator catalog: the fixed request values, and beside each one what this PMS was
/// measured to produce for it.
///
/// **The 4K entry is the reason this type exists rather than a bare `Rung::kbps()` call.** Measured
/// against the probe's Generic HLS / H.264 / AAC profile: a request of up to 21,750 kbps with a
/// 3840x2160 ceiling stays 1920x1080 and the decision tops out near 20,011 kbps, while 22,000
/// kbps flips the output to 3840x2160 advertised at about 20,895 kbps — and every request from 22
/// to 60 Mbps produced that same output. So asking for 20,895 does NOT get 4K, and asking for
/// 22,000 does not get 22 Mbit/s of bits. Both halves have to be stored, or the controller spends
/// a budget it does not have on a raster it did not ask for.
///
/// None of it is a claim about Plex in general — it is this server, this profile, this media
/// shape, taken by `tools/pms-hls-probe.py` (see `docs/pms-hls-protocol-probe.md`). A different
/// PMS may hold a different boundary, which is survivable exactly because the transaction in
/// [`Controller::candidate_ready`] grades the actual segment rather than trusting this table.
///
/// **`expected_wire_kbps` is WRONG for 12 of these 13 entries, and the reason is that it is a
/// PER-ITEM quantity stored as a per-server constant.** Swept across the whole ladder on three
/// library items (`docs/measurements/p2h-pms-ladder.md`, `tools/pms-rung-sweep.py`), the rate
/// PMS actually declares runs 5.2%–31.6% BELOW the request on every rung but the 4K one, and it
/// moves with the item: rung 720 declares 547 kbps on one film and 425 on two other titles.
/// Two entries are worse than merely stale. **The 20,011 above was taken with a 3840x2160
/// ceiling, and `Rung::P1080High::raster()` is (1920, 1080)** — under the request the app really
/// sends, that rung declares 16,150, so the table is 23.9% high. It is not the ceiling box that
/// moved it: requesting 20,000 with EITHER box returns 16,150, which was checked rather than
/// assumed. And **rungs 18000 and 20000 are the same encoder session** on a 1080p item — same
/// declared 16,150, and 39 of 40 segments byte-identical by sha256 — so the controller carries
/// two budgets and two production loads for one stream. All of it is over-estimation, hence
/// conservative for admission and not a live bug; the fix belongs with the admission rule, since
/// the transaction ALREADY fetches the true value and logs it (`ff.rs`'s
/// `hls: master one-variant bandwidth=`) before it decides anything.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HlsActuatorCatalog {
    candidates: [HlsCandidate; 13],
    /// Feasibility, not preference: the widest raster the DEVICE's own codec table admits. A
    /// candidate above it is removed before anything is scored, because no utility weight should
    /// have to outvote a decoder that cannot decode.
    raster_limit: (u16, u16),
    /// The source's own raster, for the smallest-sufficient-box rule in [`Self::admits`]. `(0, 0)`
    /// when the server did not say, which means "no bound".
    source: (u16, u16),
}

impl HlsActuatorCatalog {
    pub(crate) const fn measured() -> Self {
        const fn point(rung: Rung, expected_wire_kbps: u32, production_load_pm: u32) -> HlsCandidate {
            HlsCandidate {
                rung,
                request_kbps: rung.kbps(),
                expected_wire_kbps,
                production_load_pm,
            }
        }
        Self {
            candidates: [
                point(Rung::P240, 320, 90),
                point(Rung::P480, 720, 180),
                // **300 and 350 are MEASURED (M3 re-run, 2026-08-28), replacing 420 and 900.**
                // Both were refuted by that census against a 4K source at the falsification rule
                // this table was published with: `P720Low` measured a 296 residual (29.5% off) and
                // `P1080M6` measured 346 (61.6% off), reproducibly across both pacing legs. See
                // `docs/measurements/m3-production-census.md`.
                //
                // `P1080M6` is the entry worth understanding rather than just correcting, because
                // it is the whole indexing defect in one row: against a 4K source PMS produces
                // **1280x720** for it — the same raster it produces for `P720` — at the same
                // measured 112 pm, while this table asserted 900 against `P720`'s 450. Two rungs,
                // identical output, identical cost, a 2x ratio between them. That is the bounding
                // box being read as a target, which the `raster()` doc below already warns about.
                point(Rung::P720Low, 2_000, 300),
                point(Rung::P720, 4_000, 450),
                point(Rung::P1080M6, 6_000, 350),
                point(Rung::P1080, 8_000, 930),
                point(Rung::P1080M10, 10_000, 950),
                point(Rung::P1080M12, 12_000, 970),
                point(Rung::P1080M14, 14_000, 980),
                point(Rung::P1080M16, 16_000, 990),
                point(Rung::P1080M18, 18_000, 995),
                point(Rung::P1080High, 20_011, 1_000),
                point(Rung::Uhd, 20_895, 2_100),
            ],
            raster_limit: (u16::MAX, u16::MAX),
            source: (0, 0),
        }
    }

    /// **The source raster this catalog was bounded by**, `(0, 0)` for "nobody said" — the same
    /// reading `limited_to` gives it, and the reading `covers_source` already depends on.
    ///
    /// N14 asked for a `source_raster` field on `ModeInputs`, "threaded through `HlsAbrControl` to
    /// the worker — that one does cross a thread". It does not: `route::auto_catalog` builds this
    /// catalog from `session().cur_src` on the main thread and `HlsAbrControl` already carries the
    /// whole catalog to the worker, so the raster has been on the worker's stack all along, one
    /// accessor away. Adding a parallel field would have been a second copy of one fact, free to
    /// disagree with the bound it describes.
    pub(crate) fn source_raster(&self) -> (u16, u16) {
        self.source
    }

    /// Restrict the catalog to rasters this playback can actually use. Both bounds matter and they
    /// are different questions: `device` is what the SoC decodes (`devcaps`, the television's own
    /// table), and `source` is the picture that exists — asking PMS to UPSCALE a 1080p master to
    /// 4K buys nothing and costs the measured 2.1x of server work, so a candidate wider than the
    /// source is infeasible rather than merely unattractive.
    /// **A zero on either axis means NOBODY SAID, and is treated as unbounded** — not as a
    /// forbidden zero-pixel picture. PMS omits source dimensions often enough that the other
    /// reading would empty the catalog and park Auto on whatever the floor happens to be, which is
    /// the opposite of what a missing field justifies. (This is the mirror image of
    /// `plex::Ceiling::admits`, where an unmeasured source fails CLOSED — deliberately: there, `0`
    /// is being asked to honour an explicit user instruction, and here it is being asked to
    /// forbid a device capability nobody has contradicted.)
    pub(crate) fn limited_to(mut self, device: (u16, u16), source: (u16, u16)) -> Self {
        fn axis(value: u16) -> u16 {
            if value == 0 { u16::MAX } else { value }
        }
        self.raster_limit = (axis(device.0), axis(device.1));
        self.source = source;
        self
    }

    /// Does the DEVICE decode this raster? A hard bound, and the only unconditional one.
    fn decodable(&self, candidate: HlsCandidate) -> bool {
        let (width, height) = candidate.rung.raster();
        width <= self.raster_limit.0 && height <= self.raster_limit.1
    }

    /// Is this box big enough that PMS would not scale the source down at all?
    fn covers_source(&self, candidate: HlsCandidate) -> bool {
        let (width, height) = candidate.rung.raster();
        self.source.0 > 0 && self.source.1 > 0 && width >= self.source.0 && height >= self.source.1
    }

    /// **A rung's raster is a BOUNDING BOX, not a target**, and reading it as a target is a bug
    /// this shipped for one afternoon: PMS fits the source inside the box and never upscales, so
    /// the per-axis test that seemed obvious — box must not exceed the source on either axis —
    /// threw away every 1080p rung for a 1918x802 scope film, which is to say for most films.
    /// Measured on the television against a real library item: Auto capped at 4 Mbps / 720p on a
    /// gigabit LAN, and the log looked healthy while it did it.
    ///
    /// The rule that survives both readings is **the smallest sufficient box**: keep every box that
    /// actually constrains the source (those are real quality steps), keep the smallest box that
    /// covers it (that is "do not scale at all"), and drop the larger ones — a bigger box buys the
    /// same picture and, for the 4K point, would price it with a production load measured on an
    /// output this source cannot produce.
    fn admits(&self, candidate: HlsCandidate) -> bool {
        if !self.decodable(candidate) {
            return false;
        }
        if !self.covers_source(candidate) {
            return true;
        }
        // Dominance is compared on the BOX, never on the bitrate: the six 1080p rungs share one
        // raster and differ only in bits, so a bitrate comparison here would keep the cheapest of
        // them and silently delete the other five.
        let (width, height) = candidate.rung.raster();
        !self.candidates.iter().any(|other| {
            let (other_w, other_h) = other.rung.raster();
            let strictly_smaller =
                other_w <= width && other_h <= height && (other_w < width || other_h < height);
            strictly_smaller && self.decodable(*other) && self.covers_source(*other)
        })
    }

    /// Every candidate this playback may move to, cheapest first. The current rung is deliberately
    /// NOT filtered by [`Self::candidate`] — a state already running has to remain describable
    /// even when a later feasibility bound would exclude it.
    pub(crate) fn feasible(&self) -> impl Iterator<Item = HlsCandidate> + '_ {
        self.candidates.iter().copied().filter(move |c| self.admits(*c))
    }

    pub(crate) fn candidate(self, rung: Rung) -> HlsCandidate {
        self.candidates
            .iter()
            .copied()
            .find(|candidate| candidate.rung == rung)
            .unwrap_or(HlsCandidate {
                rung,
                request_kbps: rung.kbps(),
                expected_wire_kbps: rung.kbps(),
                production_load_pm: 1_000,
            })
    }

    /// The best FEASIBLE actuator whose measured output fits the budget — chosen directly, so a
    /// jump from 8 Mbps to a 15 Mbit/s budget primes the 14 Mbps encoder once instead of walking
    /// 10, 12, 14 and paying three encoder creations for one move.
    pub(crate) fn best_for_budget(&self, safe_budget_kbps: u32) -> Option<HlsCandidate> {
        self.feasible()
            .filter(|candidate| candidate.expected_wire_kbps <= safe_budget_kbps)
            .max_by_key(|candidate| candidate.expected_wire_kbps)
    }

    /// The same selection with the PMS production estimate applied as a second, independent
    /// constraint (see [`ProductionEstimate::predicted_ratio_pm`]). This is what stops a fast link
    /// in front of a loaded server from committing 4K: the network says yes and the server's own
    /// measured cadence says it would fall behind real time.
    pub(crate) fn best_sustainable(
        &self,
        safe_budget_kbps: u32,
        production: &ProductionEstimate,
        current: HlsCandidate,
        policy: &AbrPolicy,
        buffered_ms: i64,
    ) -> Option<HlsCandidate> {
        self.feasible()
            .filter(|candidate| candidate.expected_wire_kbps <= safe_budget_kbps)
            .filter(|candidate| {
                production
                    .predicted_ratio_pm(*candidate, current, policy)
                    .is_none_or(|ratio| ratio <= policy.production_safe_pm)
            })
            // **N3's refill filter — a THIRD independent constraint, in its own units.** The
            // budget above is bits per second, production is a cadence, and this is a reserve: a
            // candidate that would leave the buffer short of its own target has to leave room to
            // close that shortfall inside `H`, so the rate it may claim shrinks in proportion.
            // Per candidate rather than as one scalar, because `R` appears on both sides of the
            // algebra and a single budget compared against every rung is not well defined.
            //
            // **The two rates handed to it are PLANNING rates and are labelled so here**, per N17.
            // `expected_wire_kbps` is what the rendition was asked for, not what it delivered —
            // the catalog's error is +5.2% to +31.6% — and the audio lane uses
            // `assumed_audio_kbps` because no per-lane ES measurement exists for a rung that has
            // not been played. Only `B_max_est`'s geometry is measured; the rates it is evaluated
            // at are not, and the day `SegmentSample::media_kbps` is carried per rung this becomes
            // a measurement.
            .filter(|candidate| {
                let video_es = candidate
                    .expected_wire_kbps
                    .saturating_sub(policy.assumed_audio_kbps);
                super::plant::refill_admits(
                    candidate.expected_wire_kbps,
                    video_es,
                    policy.assumed_audio_kbps,
                    buffered_ms,
                    safe_budget_kbps,
                    policy,
                )
            })
            .max_by_key(|candidate| candidate.expected_wire_kbps)
    }
}

