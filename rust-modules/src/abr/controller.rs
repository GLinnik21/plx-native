use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Down,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsReason {
    SafeBudgetIncrease,
    UnsafeCurrentState,
    ProductionConstraint,
    BufferConstraint,
    /// **The terminal case: the downshift trigger fired and there is no rung below.** R12 — "a
    /// predicate with no action in the region that motivates it."
    ///
    /// It is not a new behaviour. `self.current.below()` returns `self.current` at the ladder
    /// floor, so the proposal was already skipped and `Stay` was already the answer; there is no
    /// other answer, because the escape this trigger exists to take does not exist at the bottom
    /// of the ladder. What was missing is that it said nothing. The line read `decision=stay
    /// reason=None`, identical to a healthy segment, and telling the two apart meant
    /// cross-referencing `current=` against the ladder floor by hand — on the one state where the
    /// controller has exhausted everything it can do and the picture is about to stop.
    LadderFloor,
    /// **The starvation horizon fired.** `T = B*R/(R - C) <= starvation_fallback_secs`: at the
    /// measured capacity, the reserve runs out inside the fallback window.
    ///
    /// It is a separate code from [`UnsafeCurrentState`](Self::UnsafeCurrentState) because the two
    /// answer different questions and can disagree. That one is `immediate_network < requirement`
    /// -- a rate comparison with no reserve in it, which fires on a 1% deficit against a full
    /// buffer. This one is the deadline: how long the do-nothing path has left. A log carrying
    /// only the first cannot tell a rung that is slightly too dear from one that is about to
    /// stop.
    StarvationHorizon,
    /// **The evidence supported a climb and N11's backoff was still holding it.**
    ///
    /// The one guard state on the UP path that is worth a code of its own, because it is the only
    /// one that says "yes, and not yet". `dwell=` on the same line reports the OTHER guard, which
    /// needs no code: a dwell that is holding returns before any target is selected, so there is
    /// no rung to name. This one has a rung, has selected it, and is refusing it.
    RejectBackoff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecisionReason {
    Hls(HlsReason),
}

/// The whole basis of one steady-state decision, for the event log. Every field here was an input
/// to the decision published beside it — which is the property that makes the line worth reading
/// six weeks later, and the reason it is assembled by the controller rather than re-derived at the
/// log site from whatever happened to be reachable.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ControllerTelemetry {
    pub(crate) current: Rung,
    pub(crate) safe_budget_kbps: u32,
    /// The EMERGENCY horizon — `T = B*R/(R - C)` on the MEASURED rate — as `edge=` on the steady
    /// line. `risk.starvation_seconds` beside it is the same formula on the conservative rate and
    /// is a planning quantity; this is the one the downshift trigger reads.
    pub(crate) emergency_horizon_secs: Option<u32>,
    /// What the model would pick for this link RIGHT NOW, ignoring the hysteresis that keeps the
    /// current rung. `None` while nothing is sustainable (a cold start, or a link that cannot
    /// carry the bottom of the ladder). The read-out's "current / optimal" pair.
    pub(crate) optimal: Option<HlsCandidate>,
    pub(crate) delivery: CapacityEstimate,
    pub(crate) production: ProductionEstimate,
    pub(crate) buffer: BufferEstimate,
    pub(crate) risk: CandidateRisk,
    pub(crate) pending: Option<Proposal>,
    pub(crate) reason: Option<DecisionReason>,
    /// **The §4 admission rule's shadow verdict on the current rung.** Decides nothing; it is here
    /// so the rule can be graded against the estimators beside it on a device before anything is
    /// moved onto it.
    pub(crate) window: AdmissionReadout,
    /// **The two operational guards that can hold an upshift back** (N10, N11), and the two
    /// estimator inputs that survived beside them. J5's three counters are gone; what replaced
    /// them is wall clock and recorded evidence, and both are reported here for the same reason
    /// the counters were: a log has to distinguish "the evidence did not support a climb" from
    /// "the evidence supported it and a guard was still holding".
    pub(crate) gates: GateCounters,
}

/// Guard state, for the log line only. **Nothing reads this to decide anything.**
///
/// It replaced J5's `stable`/`cooldown`/`on_rung`/`draining` quartet when I6 replaced the counters
/// those two reported. `on_rung` and `draining` survive unchanged — both are still real state,
/// and neither was ever a policy counter — while the two that WERE policy are now reported as
/// what they became: a wall-clock debt and a named refusal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct GateCounters {
    /// Wall milliseconds still owed before the UP path may propose again (N10). `0` is free.
    pub(crate) dwell_ms: u64,
    /// The rung the reject/backoff guard is currently refusing, in kbps (N11). `0` is nothing.
    pub(crate) blocked_kbps: u32,
    /// Samples taken since the current rung was committed. The first is a PMS cold start, which
    /// is the ONE sample count that survives anywhere in HLS policy — it feeds the production
    /// estimator's 1-vs-3 weight, and it is I3's cold-start predicate. It is not an adaptation
    /// gate any more (N9).
    pub(crate) on_rung: u8,
    /// Consecutive samples the reserve has been draining. **No longer a threshold** — N21 replaced
    /// `>= 8` on the production arm with `BufferEstimate::draining()`'s magnitude test — but still
    /// worth reading, because `starving()`'s second arm counts it.
    pub(crate) draining: u32,
}

/// **Why a candidate transaction was abandoned, in the only distinction the backoff guard needs.**
///
/// N11 asks the controller to record "the rejected rung and the reason". The reason matters for
/// exactly one decision: whether the failure says anything about the RUNG. Every reject site in
/// `ff.rs` already names itself to `tx.finish(...)`, so the vocabulary exists; this collapses it
/// to the one axis that changes behaviour, at the call site, where the knowledge is.
///
/// Getting this wrong in the permissive direction re-creates the livelock N11 exists to stop.
/// Getting it wrong in the strict direction blocks a perfectly good rung after a seek — which is
/// why `Circumstance` is not a courtesy: `reserve_unreadable` and `origin_changed` are statements
/// about the SESSION, and the transaction that follows them starts from different facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RejectCause {
    /// The RUNG itself failed — no playlist, no segment, a missed deadline, PMS refusing this
    /// rung's ceiling (`route::PrimeRefusal::Rung`, and only that one of `prime`'s four exits), a
    /// production ratio or acquisition window that would not admit it. Arms the backoff, because
    /// re-proposing the same rung against the same evidence buys the same answer at the same price
    /// — and arms it **only on an UP proposal**, because the block prices repeating a spend the
    /// controller chose to make, which a downshift is not.
    Candidate,
    /// The transaction was abandoned for a reason that says nothing about the rung: the origin
    /// moved, the reserve stopped being readable (a seek), or the session moved underneath the
    /// prime (`route::PrimeRefusal::Session` — the encoder changed, the client is gone, or the
    /// control-plane call never reached the server). Does not arm the backoff.
    Circumstance,
}

/// **What the last candidate reject cost, and the two independent things that release it** (N11).
///
/// Before this, `reject` recorded nothing at all: it set `cooldown = 1`, and the decrement runs
/// *before* the check, so `K = 1` has never blocked a single segment. Any stateless cost therefore
/// re-proposed on the very next sample, and each failed prime costs `E_tx` of unrefilled reserve —
/// a self-inflicted drain that repeats until something else moves.
///
/// **It refuses every upshift, not only the rung that failed, and that is a correction to N11 as
/// written.** N11 says "refuse to re-prime THAT rung", and the reason it gives is affordability:
/// "each failed prime costs ~4.6 s of unrefilled reserve against ~3.6 s of refill". Those two do
/// not match, and the test written to pin the guard is what exposed it — after a reject the
/// controller does not re-propose the same rung at all; the budget has moved, so it proposes a
/// NEIGHBOURING one, and a rung-keyed guard waves that through while the reserve pays for it just
/// the same. `E_tx` is spent by the ATTEMPT. The rung is recorded because the log needs it and
/// because the evidence test below is about it; it is not what the refusal is keyed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RejectBlock {
    rung: Rung,
    /// **The clock release**, at `now + refill_time(E_tx)`: the wall time this link needs to earn
    /// back what the failed attempt spent. `None` when the link had no surplus at reject time —
    /// then no amount of waiting repays it and only new evidence may release the block, which is
    /// [`crate::abr::plant::refill_time_ms`]'s `None` carried through rather than papered over.
    release_at_ms: Option<u64>,
    /// **The evidence release**, and it is not a chosen threshold. The failing budget was
    /// `slow * (1000 - unc)/1000`; the estimate's own uncertainty band is `slow * unc/1000`; so a
    /// budget that has moved past the whole band is a budget above `slow` — the raw rate the
    /// failing estimate believed. "Materially" is therefore the estimator's own statement of what
    /// it did not know, and no number is introduced to express it.
    evidence_kbps: u32,
}

impl RejectBlock {
    /// Is this block still refusing? **Both conditions must hold**, and either one alone releases
    /// it, because they are two independent sufficient reasons to try again: the link has repaid
    /// what the attempt spent, or the evidence has moved so far that the next attempt is not the
    /// same attempt. `release_at_ms == None` is a link with no measured surplus at reject time —
    /// nothing is ever repaid there, so only the evidence can release it, which is exactly what
    /// this expression says without a special case.
    fn holds(&self, now_ms: u64, safe_budget_kbps: u32) -> bool {
        let unpaid = self.release_at_ms.is_none_or(|at| now_ms < at);
        unpaid && safe_budget_kbps <= self.evidence_kbps
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Proposal {
    pub(crate) rung: Rung,
    pub(crate) direction: Direction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Stay,
    Prime(Proposal),
}

/// Integer-only estimator and transaction state. Current-session samples decide whether to
/// propose. Candidate-session measurements decide whether that proposal may commit.
pub(crate) struct Controller {
    pub(super) current: Rung,
    pending: Option<Proposal>,
    delivery: CapacityEstimate,
    production: ProductionEstimate,
    buffer: BufferEstimate,
    catalog: HlsActuatorCatalog,
    policy: AbrPolicy,
    samples_on_rung: u8,
    /// **Wall clock, supplied by the caller on every `observe`** — the controller owns no clock of
    /// its own and must not: `SegmentSample::total_fetch_us` is per-REQUEST elapsed and is duty
    /// cycled, running slow exactly when the reserve is full and the byte cap is idling the demux
    /// worker, which is the substitution N13 identifies as a defect elsewhere.
    now_ms: u64,
    /// **The instant the dwell guard EXPIRES, fixed when it is armed** (N10).
    ///
    /// It replaced `cooldown`, which counted SEGMENTS — and a segment is `bytes / C` of wall time,
    /// so an eight-segment guard was an unbounded amount of wall clock that got longer exactly as
    /// the link got worse. Any commit arms it, because any commit starts a PMS encoder session and
    /// this is an encoder-lifecycle guard; only the UP path is blocked by it, because a downshift
    /// is a recovery action and rate-limiting recovery is how a stall becomes a policy.
    ///
    /// It holds the deadline rather than the commit instant because `E_tx` is a function of the
    /// segment's media duration, and a guard's length must be decided by the transaction that
    /// armed it. Recomputing it each sample from the CURRENT `last_segment_ms` let a longer
    /// segment retroactively extend a running dwell — and HLS segment durations come off
    /// `#EXTINF`, i.e. a request PMS may answer differently at any point.
    dwell_until_ms: Option<u64>,
    /// The last observed segment's media duration, so `reject` can price `E_tx` without the sample
    /// that is no longer in scope by then. Zero until the first `observe`, and a zero-duration
    /// segment cannot arm a backoff, which is the correct reading of "no measurement".
    last_segment_ms: i64,
    /// **The reject/backoff guard's state** (N11). `None` is the ordinary case.
    reject_block: Option<RejectBlock>,
    last_reason: Option<DecisionReason>,
    last_safe_budget_kbps: u32,
    /// **The emergency deadline the last decision was taken against**, so the log carries the
    /// quantity that DECIDED rather than a neighbouring one that did not. `None` means the
    /// measured rate covers the current rung, which is the healthy state and not a missing
    /// reading.
    ///
    /// It is a second horizon beside `risk.starvation_seconds` on purpose and they can disagree by
    /// a factor of two: that one is computed on `conservative_kbps()` for the risk score and the
    /// mode comparison, this one on the measured rate for the eviction decision. Publishing only
    /// the planning horizon while deciding on the measured one is the exact shape of the trap
    /// `[[silent-instrument-trap]]` names -- a log full of plausible numbers, none of them the one
    /// that fired.
    last_emergency_horizon: Option<u32>,
    /// **Dev-only actuator pin — see [`Self::pinned_to`].** `None` in every production path.
    pin: Option<Rung>,
    /// **The transferred acquisition window** (`abr/window.rs`, specification §2a/§4).
    ///
    /// **It DECIDES.** [`Self::candidate_ready`] admits an upshift only if this window admits the
    /// candidate's worst-case byte count, and [`Self::largest_admissible`] will not propose one it
    /// would refuse.
    ///
    /// It shipped observe-only and was graded that way first — 68 device lines, 0 disagreements
    /// with the specification (`docs/measurements/j3a-window-shadow.md`) — before anything was
    /// moved onto it. That ordering is the reason to trust it, not a description of what it does.
    acquisitions: AcquisitionWindow,
    /// What the §4 rule WOULD have said about staying on the current rung, recomputed each sample.
    /// Read only by telemetry.
    last_window: AdmissionReadout,
}

impl Controller {
    /// Unknown links start at 480p/720 kbit/s. P240 remains available as the emergency floor.
    /// The active encoder already exists at `current` (including a runtime Original fallback
    /// chosen from its measured link), so the controller must begin from that exact wire state.
    ///
    /// `prior` is the bootstrap probe as an explicitly weak seed — the first segment then REFINES
    /// a measurement instead of starting from nothing, which is what stops the opening minute
    /// being spent re-deriving a number the app already paid for. `catalog` arrives with this
    /// playback's feasibility bounds already applied.
    pub(crate) fn starting_at(
        current: Rung,
        prior: Option<CapacityEstimate>,
        catalog: HlsActuatorCatalog,
    ) -> Self {
        Self {
            current,
            pending: None,
            delivery: prior.unwrap_or_default(),
            production: ProductionEstimate::default(),
            buffer: BufferEstimate::default(),
            catalog,
            policy: AbrPolicy::measured(),
            samples_on_rung: 0,
            now_ms: 0,
            dwell_until_ms: None,
            last_segment_ms: 0,
            reject_block: None,
            last_reason: None,
            last_safe_budget_kbps: 0,
            last_emergency_horizon: None,
            pin: None,
            acquisitions: AcquisitionWindow::default(),
            last_window: AdmissionReadout::default(),
        }
    }

    /// **Pin the controller to one actuator, for measurement only.** A builder rather than a
    /// parameter so no existing call site or test moves.
    ///
    /// Set from the `plxnative-abrpin` dev trigger, which is compiled out of a release build, so
    /// this is `None` in every shipped configuration and in every host test that does not ask for
    /// it. It exists for measurement step M4 (`docs/adaptive-playback-plan.md` §4), which has to
    /// hold one rung long enough to read a settled reserve at it — and which cannot use the
    /// playback-quality selector, because a non-Auto quality returns `None` from
    /// `route::hls_abr_control` before a controller is ever built, and because the quality ladder
    /// has no mid-1080p points.
    ///
    /// **What it does NOT do**: it changes no threshold, no budget and no risk term. Every
    /// estimator still updates on every segment, `telemetry()` still reports the model's real
    /// state, and the log lines still say what the model would have wanted. It short-circuits the
    /// DECISION only, and only after all measurement has been taken — so the numbers M4 reads are
    /// the numbers an unpinned run would have produced for that rung.
    pub(crate) fn pinned_to(mut self, pin: Option<Rung>) -> Self {
        self.pin = pin;
        self
    }

    pub(crate) fn current(&self) -> Rung {
        self.current
    }

    #[cfg(test)]
    pub(crate) fn pending(&self) -> Option<Proposal> {
        self.pending
    }

    /// **Test-only: `observe` with the fixture wall clock advanced by one segment.**
    ///
    /// `observe` takes absolute monotonic milliseconds (N10) and the fixtures in `tests.rs` are
    /// written around one steady state: a stream that is keeping up delivers `d` of content every
    /// `d` of wall clock. This advances the clock by exactly that and nothing else, so a test that
    /// does not care about time does not have to invent a number — and, more importantly, cannot
    /// silently leave the clock at zero, where the dwell deadline would never be reached and every
    /// post-commit upshift would be blocked by a guard the test never meant to exercise.
    ///
    /// **A test about a wall-clock guard must call `observe` directly.** Those are the tests where
    /// the cadence IS the subject, and expressing it through this fixture would assert the fixture.
    #[cfg(test)]
    pub(crate) fn observe_next(&mut self, sample: SegmentSample) -> Decision {
        let now = self.now_ms.saturating_add(u64::from(sample.media_duration_ms));
        self.observe(sample, now)
    }

    /// The controller's own wall clock, for a test that must CONTINUE it rather than invent a
    /// second origin. `observe_next` advances this by a segment per call, so by the time a fixture
    /// has driven the acquisition window to its admitting length the clock is already tens of
    /// seconds in; a test that then starts counting from 0 is not measuring the same timeline the
    /// controller is, and `dwell_remaining_ms`' `saturating_sub` reads the difference as zero
    /// elapsed until the second origin catches up.
    #[cfg(test)]
    pub(crate) fn clock_ms(&self) -> u64 {
        self.now_ms
    }

    #[cfg(test)]
    pub(crate) fn window_len(&self) -> usize {
        self.acquisitions.len()
    }

    pub(crate) fn catalog(&self) -> HlsActuatorCatalog {
        self.catalog
    }

    pub(crate) fn delivery(&self) -> CapacityEstimate {
        self.delivery
    }

    pub(crate) fn buffer(&self) -> BufferEstimate {
        self.buffer
    }

    /// A pause is wall-clock time with no measurement — the one gap where the estimate really has
    /// aged (backpressure is not: a full buffer stops the reader on purpose).
    pub(crate) fn on_resume(&mut self, paused_ms: u64) {
        self.delivery.age_ms(paused_ms, &self.policy);
        self.buffer = BufferEstimate::default();
        // `samples_on_rung` describes uninterrupted time on this rung, and a pause ends that.
        self.samples_on_rung = 0;
        // **The backoff block goes and the dwell instant stays, and the asymmetry is the whole
        // argument for a wall clock.** The block's evidence release is keyed on the estimate
        // `age_ms` has just widened, so the rate it recorded no longer describes anything; keeping
        // it would refuse a rung on a comparison against a number that has been retracted. The
        // dwell needs no such care — sixty seconds of pause really are sixty seconds during which
        // no encoder was started, so the guard has genuinely expired, which is exactly what a
        // segment counter could not represent.
        self.reject_block = None;
    }

    /// Everything one decision was made on, in one struct, for one event-log line. Assembled here
    /// rather than in `ff.rs` so the numbers logged are the numbers used.
    pub(crate) fn telemetry(&self) -> ControllerTelemetry {
        let current = self.catalog.candidate(self.current);
        ControllerTelemetry {
            current: self.current,
            safe_budget_kbps: self.last_safe_budget_kbps,
            emergency_horizon_secs: self.last_emergency_horizon,
            // The SAME selection [`Self::observe`]'s upshift arm makes, including the named
            // admission headroom — so the read-out cannot advertise an operating point the
            // controller would not actually choose. It is the answer to "what is this link worth",
            // which is a different question from "what is playing" and the one a viewer
            // photographing the panel is usually asking.
            optimal: self.catalog.best_sustainable(
                self.last_safe_budget_kbps
                    .saturating_mul(self.policy.upshift_admission_headroom_pm)
                    / 1_000,
                &self.production,
                current,
                &self.policy,
                self.buffer.buffered_ms,
            ),
            delivery: self.delivery,
            production: self.production,
            buffer: self.buffer,
            gates: GateCounters {
                dwell_ms: self.dwell_remaining_ms(),
                // The guard's EFFECT, not its storage: `0` means nothing is being refused right
                // now, which is the question a log line is asked. A block that has released is
                // indistinguishable from no block at all, and reporting it would read as a stuck
                // guard on exactly the segments where it had already got out of the way.
                blocked_kbps: self
                    .reject_block
                    .filter(|b| b.holds(self.now_ms, self.last_safe_budget_kbps))
                    .map(|b| b.rung.kbps())
                    .unwrap_or(0),
                on_rung: self.samples_on_rung,
                draining: self.buffer.draining_samples,
            },
            risk: candidate_risk(
                current,
                current,
                &self.delivery,
                &self.production,
                &self.buffer,
                &self.policy,
            ),
            pending: self.pending,
            reason: self.last_reason,
            window: self.last_window,
        }
    }

    /// **`now_ms` is ABSOLUTE monotonic wall clock, not a delta**, for the reason
    /// `OriginalRecovery::advance_to` already documents: segments do not arrive on a regular
    /// cadence, so a delta API would make every wall-clock guard depend on how often the caller
    /// happened to call — which is a property of the link, not of the guard. The production caller
    /// passes one `Instant`'s elapsed time for the whole playback (`ff.rs`).
    pub(crate) fn observe(&mut self, sample: SegmentSample, now_ms: u64) -> Decision {
        self.now_ms = now_ms;
        // **A reason describes THIS sample or there is none.** It was written on every path that
        // reached a conclusion and cleared on none, so any earlier return re-published the last
        // one that happened to be set. I6's dwell gate made that visible: it returns before a
        // target is selected, so the line read `dwell=1400ms reason=SafeBudgetIncrease` — the
        // reason belonging to the commit that ARMED the dwell, on a sample where no evaluation
        // took place. `HlsReason::RejectBackoff`'s doc already argues that the dwell needs no code
        // of its own ("a dwell that is holding returns before any target is selected, so there is
        // no rung to name"), and that argument only holds if the field is empty when it says so.
        self.last_reason = None;
        let ratio = sample.production_ratio_pm();
        let current_candidate = self.catalog.candidate(self.current);
        // A segment at a low rung on a fast link is too small to time; clamp what it may claim to
        // what it can actually support. See `CapacityObservation::clamped_to_evidence`.
        let observation = CapacityObservation {
            kbps: sample.network_kbps(),
            bytes: sample.bytes,
            active_us: sample.active_fetch_us,
            completed: true,
        }
        .clamped_to_evidence(current_candidate.expected_wire_kbps);
        let network = observation.kbps;
        if observation.is_collapse(&self.delivery) {
            self.delivery.collapse(network);
            // **The one place the acquisition window is cleared.** `window.rs` keeps history across
            // a rung COMMIT on purpose — the transfer bound carries a sample from one rung to
            // another by bytes — but a collapse is the other thing: the link this history describes
            // has stopped existing, and a bound built from it runs about 2x anti-conservative on a
            // swept link. This changes no decision today; the window is observed, not read.
            self.acquisitions.reset();
        }
        self.delivery.update(observation);
        let cold_start = self.samples_on_rung == 0;
        self.production
            .observe(ratio, current_candidate.production_load_pm, cold_start);
        self.buffer
            .update(sample.buffer.buffered_ms(), i64::from(sample.media_duration_ms));
        self.samples_on_rung = self.samples_on_rung.saturating_add(1);

        let draining = self.buffer.draining();
        let segment = i64::from(sample.media_duration_ms);
        self.last_segment_ms = segment;

        // **Computed HERE, above every early return, and only read below.** The budget is the
        // delivery estimate's conservative network rate, so its value is identical wherever
        // between here and the decision it is taken — nothing in between mutates that input.
        // Where it was computed mattered anyway, because three paths
        // return before reaching the decision: a transaction in flight, and both arms of the dev
        // pin. On a pinned run that is EVERY sample after the pin is reached, and the measured
        // consequence was that 397 of 527 `abr: steady` lines reported `safe=0kbps` — the central
        // quantity of the admission rule, unobservable on three quarters of the corpus, on
        // exactly the runs designed to characterise a rung.
        let safe_budget = hls_safe_budget(&self.delivery);
        self.last_safe_budget_kbps = safe_budget;
        // A block that has released is RETIRED, not merely re-tested every sample, and the reason
        // is not monotonicity — only the clock half is monotone; the budget can rise past
        // `evidence_kbps` and fall back under it. It is that the block describes ONE attempt and
        // its debt. Once any sufficient reason to try again has occurred, that debt is discharged;
        // a budget that falls afterwards is new information about the link, not the old refusal
        // coming back. Keeping it would let a single failed prime refuse climbs indefinitely
        // through a budget that merely wobbles.
        if self.reject_block.is_some_and(|block| !block.holds(now_ms, safe_budget)) {
            self.reject_block = None;
        }

        // **Observe the §4 window, and compute its verdict on the CURRENT rung for telemetry.**
        // The decision this window drives is not taken here — it is taken at the proposal
        // (`largest_admissible`) and at validation (`candidate_ready`), both of which query a
        // CANDIDATE's byte count rather than this one.
        //
        // Placed above every early return for the same reason `safe_budget` is: a pinned run
        // returns before the decision on every sample after the pin lands, and a quantity that is
        // only computed on the path it does not take is unobservable on exactly the runs meant to
        // characterise it.
        //
        // The query is the CURRENT rung's own byte count, so this answers "is what we are already
        // playing sustainable" — the one admission question that needs no size prediction and no
        // `sigma`. A candidate's query is `sigma * W_j * D / 8000`; that arrives with the decision.
        // The reserve here is the ESTIMATOR's — the last one actually observed — and not this
        // sample's, which may be `None`. The readout decides nothing and is graded offline, so a
        // one-segment-stale reserve is the right input for it; refusing to emit the line on the
        // samples where the audio lane is quiet would blind the grading to exactly the segments
        // after an open or a seek.
        self.acquisitions.observe(sample.bytes, sample.total_fetch_us());
        self.last_window = self.acquisitions.readout(
            sample.bytes,
            segment,
            self.buffer.buffered_ms,
            self.policy.admission,
        );

        if self.pending.is_some() {
            return Decision::Stay;
        }
        // **An unknowable reserve decides nothing.** `buffered_ms` is `None` for exactly one
        // situation — an A/V session whose audio lane has produced no timestamp since the open or
        // the seek — and every branch below is keyed on the reserve: the emergency trigger reads it
        // as a level, the upshift gate as a depth, and §4's condition (2) as the excess it has to
        // cover. There is no honest value to substitute, and the one that WAS substituted (zero,
        // i.e. "empty") turned a full reserve into a downshift trigger.
        //
        // Staying is not a policy choice dressed as a guard: it is the absence of one. The
        // estimators above have already taken this segment, so the evidence is kept; only the
        // decision waits, and it waits at most until the audio lane produces a timestamp, which is
        // bounded by the first audio AU of the current segment.
        let Some(buffered) = sample.buffer.buffered_ms() else {
            return Decision::Stay;
        };
        // The dev pin (`pinned_to`) short-circuits the decision and NOTHING above it: every
        // estimator has already taken this segment. Reaching the pinned rung goes through the
        // ordinary prime/validate/commit transaction, so a pinned run exercises the real transport
        // path rather than a shortcut into it.
        if let Some(pin) = self.pin {
            if self.current == pin {
                return Decision::Stay;
            }
            let direction = if pin.kbps() > self.current.kbps() { Direction::Up } else { Direction::Down };
            // Wait for a reserve the transaction can be paid out of. The requirement is
            // DIRECTIONAL: the six-segment figure is an upshift derivation (two deadline budgets
            // plus `candidate_ready`'s residual), and neither of those budgets applies going down
            // — there is no graded segment, and the warm-up is bounded by the reserve itself.
            // Charging it downward is unsatisfiable at the top of the ladder — 12 000 ms against a
            // `B_max(20000)` of ~5 421 ms — which silently cost the M4 census five of its seven
            // points. See PIN_MIN_RESERVE_SEGMENTS and PIN_MIN_RESERVE_SEGMENTS_DOWN.
            let required = match direction {
                Direction::Up => PIN_MIN_RESERVE_SEGMENTS,
                Direction::Down => PIN_MIN_RESERVE_SEGMENTS_DOWN,
            };
            if buffered < segment.saturating_mul(required) {
                return Decision::Stay;
            }
            let proposal = Proposal { rung: pin, direction };
            self.pending = Some(proposal);
            return Decision::Prime(proposal);
        }

        // Fast-down: either current-sustainability signal may fail. A JIT ratio around 1.0 is
        // merely real-time production; it forces a move only when the content reserve is draining.
        let immediate_network = network.min(self.delivery.fast_kbps);
        let current_risk = candidate_risk(
            current_candidate,
            current_candidate,
            &self.delivery,
            &self.production,
            &self.buffer,
            &self.policy,
        );
        // **A bare rate comparison, and NO LONGER A TRIGGER** (N4). It is true of a rung that is 1%
        // too dear against a completely full buffer, which is not an emergency — it is a reason not
        // to CLIMB, and it already is one: the same deficit narrows `safe_budget` a few lines down.
        // Keeping a state you are already buffered into and admitting a new one are different
        // decisions, and a reserve that is deep relative to the deficit is safe for a long time.
        //
        // Its other two uses survive verbatim, which is why it keeps a name. It SELECTS the
        // downshift target — a measured link collapse must not walk the ladder one oversized
        // encoder at a time — and it names the reason, because "the link is measurably below this
        // rung" is the actionable half of a downshift that fired for some other reason.
        let collapse_target = immediate_network < current_candidate.expected_wire_kbps;
        // **N21: a magnitude predicate, not a persistence count.** This required EIGHT consecutive
        // draining segments — about sixteen seconds at the 2 s segment this pipeline requests —
        // before a server falling behind could move the rung, while `starving()` two lines up
        // treats two as enough. It is now `draining()`, whose derivation is the 2026-08-25 device
        // finding recorded at `BufferEstimate::draining`: judge the travel, not the sign of it, and
        // not the number of samples it took.
        //
        // Stated as what it is rather than as a reconciliation: this drops the persistence
        // requirement ENTIRELY — an 8x increase in sensitivity on an immediate-downshift arm. It is
        // safe in the direction that matters because `production_risk` is itself a predicted-ratio
        // test against `production_max_pm`, so the conjunction still needs the server to be behind
        // AND the reserve to be measurably shrinking. If it proves too eager the recorded fallback
        // is `draining_samples >= 2`, matching `starving()`.
        let production_bad = current_risk.production_risk && self.buffer.draining();
        let buffer_bad = buffered < segment || self.buffer.starving();
        // **The deadline, and the one trigger cold start may not suppress.** `T = B*R/(R - C)`:
        // at the rate this link is delivering, the reserve empties in `T` seconds. N4's opening
        // complaint is that a horizon was computed and discarded unread; this is the reader.
        //
        // **`C` is the MEASURED rate here, not `conservative_kbps()`, and the difference is the
        // whole correctness of the exemption below.** Conservatism belongs to ADMISSION -- a rung
        // you have not tried might be dearer than you think, so plan against a lower bound. It
        // does not belong to EVICTION, where the claim is that the link in front of you cannot
        // carry what is already playing, and the evidence for that has to be observed rather than
        // discounted into existence. `conservative_kbps()` subtracts `uncertainty_pm` capped at
        // 500, and `uncertainty_pm` is exactly 500 on the first sample of every rung
        // (`CapacityEstimate::reset_confidence` after each commit) -- a 50% haircut. Compute this
        // horizon on that and a link delivering PRECISELY what the rung asks reads as a 2x deficit
        // and fires an emergency on the healthiest possible playback.
        //
        // **Why it is then exempt from the cold-start gate, structurally rather than by
        // preference.** `starvation_horizon` returns `None` whenever `C >= R`, so on a link that
        // covers the rung it cannot fire at all, however small the reserve is. The cold-start
        // artefact is a LEVEL -- the transaction just spent the reserve, so `B` is about one
        // segment -- and `B` appears only in the numerator, multiplied by the drain fraction. A
        // small `B` with no measured deficit is an INFINITE horizon. `buffered < segment` has no
        // such protection, which is precisely why the gate below is right for that disjunct and
        // wrong for this one.
        //
        // What the exemption costs, priced rather than asserted: at the cold-start floor of one
        // 2 s segment the predicate fires at a measured deficit of 10% or more, because
        // `2 s / 0.1 = 20 s`. A downshift transaction is ~0.7 s of that. At a full `B_max` it is
        // far slacker -- 8.7 s of reserve at P1080High needs a 44% deficit, and I5's stated
        // differential (5% at a full P1080High reserve) is 173 s of horizon, nowhere near.
        let emergency_horizon =
            starvation_horizon(buffered, current_candidate.expected_wire_kbps, immediate_network);
        self.last_emergency_horizon = emergency_horizon.seconds;
        let horizon_bad = emergency_horizon
            .seconds
            .is_some_and(|secs| secs <= self.policy.starvation_fallback_secs);
        // The first sample on a rung is the encoder's cold start and the reserve contains only the
        // segment that just arrived. It refines the estimators, but cannot establish a failing
        // steady state: on the measured baseline every playback otherwise downshifted here even on
        // a fast link, solely because `B <= D`. The second sample is live policy again, so a real
        // collapse waits one segment rather than being hidden.
        //
        // **"Waits one segment" is not a bounded wait, and that is what this gate cost.** A
        // segment is `bytes / C` of wall time, and `C` is precisely the quantity that has
        // collapsed. `pipe_abr_down_collapse` (2026-08-27) fired the gate on the first sample of
        // rung 14000 with `net=498kbps`, `buf=2210ms` and the controller's own `starve=2` on the
        // same line; the next sample was 58.3 seconds later, and the picture was frozen for 47 of
        // them. So the gate applies to the disjuncts whose evidence the cold start corrupts, and
        // the deadline runs whether or not this is the first sample.
        if horizon_bad || (!cold_start && (buffer_bad || production_bad)) {
            // A measured link collapse must not walk the ladder one oversized encoder at a time.
            // Select the best actuator that fits the new safe budget, still bounded below current.
            let target = if collapse_target || buffered < segment / 2 {
                self.catalog
                    .best_for_budget(self.delivery.conservative_kbps())
                    .map(|candidate| candidate.rung)
                    .unwrap_or(Rung::P240)
                    .min(self.current.below())
            } else {
                self.current.below()
            };
            if target != self.current {
                let proposal = Proposal { rung: target, direction: Direction::Down };
                self.pending = Some(proposal);
                self.last_reason = Some(DecisionReason::Hls(if horizon_bad {
                    HlsReason::StarvationHorizon
                } else if collapse_target {
                    HlsReason::UnsafeCurrentState
                } else if production_bad {
                    HlsReason::ProductionConstraint
                } else {
                    HlsReason::BufferConstraint
                }));
                return Decision::Prime(proposal);
            }
            // `target == self.current` here means EXACTLY the floor: `below()` is the identity
            // at the bottom rung, and the `best_for_budget` branch is clamped by `.min(below())`,
            // so at any other rung the target is strictly lower. See `HlsReason::LadderFloor`.
            self.last_reason = Some(DecisionReason::Hls(HlsReason::LadderFloor));
            return Decision::Stay;
        }

        // **N10's dwell, and N9's deleted gate.** What stood here was
        // `self.cooldown > 0 || self.samples_on_rung < 2` — two sample counts, one of which
        // ("wait three segments after an up commit, eight after a down") was an unbounded amount of
        // wall time, and the other of which forbade any adaptation at all on the first two samples
        // of a rung.
        //
        // `samples_on_rung` is gone from the decision and kept as an estimator input (N9). What
        // replaces the cooldown is one wall-clock interval, `E_tx` — the sum of the two deadlines
        // this transaction is already held to. Its meaning is exact and operational: do not start
        // another encoder session before the last one could have finished paying for itself. It is
        // NOT a quality preference and may never be made into one (N20).
        if self.dwell_remaining_ms() > 0 {
            return Decision::Stay;
        }
        // TWO independent constraints, deliberately not collapsed into one budget: the network has
        // to carry the bits AND the server has to produce them ahead of real time. This is what
        // refuses 4K on a fast link in front of a loaded PMS — the measured 4K point costs 4% more
        // wire and 110% more server, so a bitrate-only budget would wave it through.
        let upshift_admission_budget = safe_budget
            .saturating_mul(self.policy.upshift_admission_headroom_pm)
            / 1_000;
        let Some(target_candidate) = self.catalog.best_sustainable(
            upshift_admission_budget,
            &self.production,
            current_candidate,
            &self.policy,
            buffered,
        ) else {
            return Decision::Stay;
        };
        let target = target_candidate.rung;
        if target == self.current {
            return Decision::Stay;
        }
        if target < self.current {
            // The budget shrank without any current-state signal failing. Nothing is wrong with
            // what is playing, so this is not a downshift trigger — it is a reason not to climb.
            return Decision::Stay;
        }

        // **Selection must not propose what validation is certain to refuse.** Below `n` samples
        // `candidate_ready` cannot admit an upshift at all, and above it only admits one the
        // window can carry — so proposing outside that set is not merely wasted work, it is a
        // livelock: each proposal costs a real PMS encoder session and `E_tx` of unrefilled
        // playback, and nothing about the refusal was recorded, so the loop repeated forever. N11's
        // backoff is the other half of closing that; this is the half that never proposes it.
        //
        // **The same rule on both sides, with the best input each side has.** Validation has the
        // rendition's own declared rate; selection cannot — a rung's `BANDWIDTH` needs a PMS
        // encoder session to exist first (§3) — so it evaluates against the catalog's
        // `expected_wire_kbps`, which is approximate (+5.2% to +31.6%) and is why validation still
        // decides. This is one rule read twice, not two thresholds stacked: no new number appears,
        // `n` and `σ` are the rule's own.
        let Some(target) = self.largest_admissible(target, segment, buffered) else {
            return Decision::Stay;
        };
        if target <= self.current {
            return Decision::Stay;
        }

        // The network constraint already selected `target` from the stricter named admission
        // budget above. The remaining independent resource guards must pass simultaneously.
        // The target is selected directly from the actuator catalog, so 8 -> 14-class budgets
        // skip intermediate encoders.
        // **The upshift reserve gate, DERIVED from the reachable ceiling** — I3b(b), which the plan
        // left as a ruling and which `b_max_est_ms` makes decidable.
        //
        // It was a flat `3 * segment` = 6 000 ms. `B_max` falls as `1/R`, and at the top of the
        // ladder the byte caps top out at 5 852 ms with a SETTLED reserve well under that — so a
        // constant six seconds is a gate the plant cannot satisfy at exactly the rungs it is
        // guarding, which is R2's "the top of the ladder is unreachable for any guard of this
        // shape" seen from the control side. Phase 0 fixed the plant half; this is the other.
        //
        // `min` of the two, so nothing is loosened where the old number was reachable: below about
        // 14 000 kbps of video ES the ceiling term exceeds 6 000 ms and the constant still binds,
        // unchanged. Above it the gate becomes what the queue can actually hold a fraction of.
        // `alpha` is the same `buffer_reserve_fraction_pm` `B*` uses — one number for "how much of
        // the reachable ceiling we are willing to ask for", not a second one wearing a new name.
        //
        // Evaluated at the TARGET's rate, not the current one: the question is whether the reserve
        // will survive arriving there.
        let target_video_es = target_candidate
            .expected_wire_kbps
            .saturating_sub(self.policy.assumed_audio_kbps);
        let reachable_gate = crate::abr::plant::b_max_est_ms(
            target_video_es,
            self.policy.assumed_audio_kbps,
        )
        .saturating_mul(i64::from(self.policy.buffer_reserve_fraction_pm))
            / 1_000;
        let reserve_gate = segment.saturating_mul(3).min(reachable_gate);
        let all_good = self.production.ratio_pm <= self.policy.production_safe_pm
            && buffered >= reserve_gate
            && !draining;
        if !all_good {
            return Decision::Stay;
        }
        // **N11's backoff, checked against the rung selection actually made.** Placed here rather
        // than earlier so a blocked rung still costs a full evaluation and a full log line: the
        // question "would this climb have happened" stays answerable while the guard is holding.
        //
        // A blocked target is a STAY, not a walk down to the next rung. Dodging the block would be
        // a rung-walking rule — the unexplained per-decision step the design directive forbids —
        // and it would spend a transaction on a rung the evidence never selected.
        if self.reject_blocks() {
            self.last_reason = Some(DecisionReason::Hls(HlsReason::RejectBackoff));
            return Decision::Stay;
        }
        // **And there is no `stable_samples` here any more (N8).** Three consecutive samples on
        // which every conjunct above held was pure counting layered on a model that had already
        // passed every risk, budget, buffer and production condition, reset at seven separate
        // sites, and it was the dominant term in the opening seconds: counter spacing was exactly
        // five segments between successive upshifts and ten after a downshift.
        let proposal = Proposal { rung: target, direction: Direction::Up };
        self.pending = Some(proposal);
        self.last_reason = Some(DecisionReason::Hls(HlsReason::SafeBudgetIncrease));
        Decision::Prime(proposal)
    }

    /// **The highest rung at or below `ceiling` that §4 would admit**, or `None` if none would.
    ///
    /// Walks DOWN from the proposed target because the admission conditions are monotone in the
    /// query and the query is monotone in the rate, so the admissible set is a prefix of the ladder
    /// and the first rung that passes is the best one. Thirteen entries; no search is warranted.
    ///
    /// **This is not a rung-walking rule.** It does not step the ladder one rung per decision — it
    /// jumps straight to the largest rung the evidence supports, which on a fast link out of a low
    /// rung is many rungs at once. What it refuses is a jump the window cannot justify, and the
    /// refusal is the bound's, not a cap: `T_i(q) = A_i·max(1, q/b_i)` grows with the query, so a
    /// large enough jump fails condition (1) on its own arithmetic. A per-decision cap on the
    /// NUMBER of rungs would be the unexplained rule the design directive forbids; this is the
    /// same inequality the validation side evaluates, and it disappears entirely as the window
    /// accumulates evidence at larger byte counts.
    ///
    /// It uses the CATALOG rate, which is the only per-rung rate selection can see, and is
    /// therefore an estimate. `candidate_ready` re-runs the same rule on the rendition's own
    /// declared rate and is what actually decides.
    fn largest_admissible(&self, ceiling: Rung, segment: i64, buffered: i64) -> Option<Rung> {
        LADDER
            .iter()
            .rev()
            .copied()
            .filter(|rung| *rung <= ceiling)
            .find(|rung| {
                let declared_bps =
                    u64::from(self.catalog.candidate(*rung).expected_wire_kbps).saturating_mul(1_000);
                let query =
                    candidate_worst_case_bytes(declared_bps, segment, rung.size_spread_pm());
                query > 0
                    && self
                        .acquisitions
                        .admits(query, segment, buffered, self.policy.admission)
                        .is_some_and(Admission::admitted)
            })
    }

    /// **A candidate's GRADED segment is a link observation and enters the acquisition window.**
    ///
    /// The window is evidence about the LINK, not about a rung — that is the whole content of §2a's
    /// transfer bound, which carries a sample from one byte count to another. Excluding a real
    /// acquisition because of which rendition produced it would throw away the only direct
    /// measurement the transaction buys.
    ///
    /// **Only the graded one, and the exclusion is derived rather than chosen.** PMS's FixedSession
    /// HLS starts a fresh decoder and encoder for every candidate, so segment zero measures that
    /// cold start — a property of the server's session lifecycle, not of the link. `ff.rs` already
    /// says so where it fetches a second segment to grade. Feeding the warm-up would put a
    /// server-side startup cost into a distribution the rule reads as network capacity, which is
    /// the same category error as reading `control=` as a transfer.
    ///
    /// A downshift has no graded segment, so nothing enters from one. That is not a gap: a
    /// downshift is not gated on the window (see [`Self::candidate_ready`]).
    ///
    /// It landed separately from the verdict that reads it, so that the change in what the window
    /// CONTAINS could be measured on its own. Both are live now.
    pub(crate) fn observe_candidate(&mut self, sample: SegmentSample) {
        self.acquisitions.observe(sample.bytes(), sample.total_fetch_us());
    }

    /// **Candidate-session acceptance, and this is where §4's admission rule DECIDES.**
    ///
    /// It is the only point in a playback where the rule can be evaluated at all: a rung's
    /// `BANDWIDTH` cannot be read without first creating a PMS encoder session for it, so
    /// `declared_bps` exists here and nowhere else (§3). That makes this the rule's proper home
    /// rather than a compromise — the transaction has already fetched and graded a real segment at
    /// the candidate, and that segment is already in the window ([`Self::observe_candidate`]).
    ///
    /// **What this replaced.** Three stacked tests, none of which survives its own evidence:
    ///
    /// * `network_kbps >= candidate.expected_wire_kbps` — the CATALOG rate, which the plan's R1
    ///   killed: +5.2% to +31.6% error, item-dependent, and non-injective (rungs 18000 and 20000
    ///   both declare 16 150). It is replaced by `declared_bps`, the rendition's own.
    /// * `production_ratio_pm <= 800` — a bare 800, and structurally the SINGLE-OBSERVATION form
    ///   `A <= 0.8 D`, which the device corpus refutes at ~37% violation. It is replaced by a
    ///   window.
    /// * `buffered >= 2 * segment` — a reserve floor in segments, replaced by condition (2), which
    ///   asks the reserve to cover the excess this window actually contains.
    ///
    /// **A filling window refuses an upshift, and that needs no extra rule.** `admits` returns
    /// `None` below `n` samples, and "no evidence" is not "safe to climb" — the default has to be
    /// the conservative one, and it is the same answer the rule gives when it does have evidence
    /// and the evidence is bad. It costs the first `n` segments of a playback, which is `n·D` of
    /// media, and the alternative is committing an encoder on a guess.
    ///
    /// **A DOWNSHIFT is deliberately not gated on the rule** and keeps only the decodable-segment
    /// and one-segment-reserve tests. Measured reason, not caution: `pipe_abr_down_collapse` graded
    /// ZERO segments across 23, because a collapse resets the window and 19 samples at a 2 s segment
    /// is 38 s of media — longer than a collapse takes to resolve. A rule that is silent through the
    /// event is the wrong instrument for it, and §5 says so structurally: this is the trigger and
    /// the target, and a DEADLINE fires from the current reserve alone.
    ///
    /// The controller still does not mutate until `commit`.
    pub(crate) fn candidate_ready(
        &self,
        proposal: Proposal,
        sample: SegmentSample,
        declared_bps: u64,
    ) -> bool {
        if self.pending != Some(proposal) {
            return false;
        }
        // An unreadable reserve refuses. It is the same answer an empty reserve gets, and that is
        // not a coincidence to paper over: this test asks whether the transaction can be paid for,
        // and a reserve that cannot be read cannot be shown to cover anything. It differs from the
        // old zero in what it does NOT do — the controller no longer proposes on an unknown
        // reserve at all, so reaching here with `None` means the lane fell silent mid-transaction.
        let Some(buffered) = sample.buffer.buffered_ms() else {
            return false;
        };
        let segment = i64::from(sample.media_duration_ms);
        if buffered < segment {
            return false;
        }
        match proposal.direction {
            Direction::Down => true,
            Direction::Up => {
                let query = candidate_worst_case_bytes(
                    declared_bps,
                    segment,
                    proposal.rung.size_spread_pm(),
                );
                // **Two INDEPENDENT constraints, not two margins.** The window answers "can the
                // link carry this rendition"; this answers "can the SERVER produce it", which no
                // amount of link evidence can, because every sample in the window was produced by
                // a different encoder. Moving onto a JIT encoder already at or slower than real
                // time is unconditionally wrong whatever the link does.
                //
                // `production_max_pm` is the policy's own named threshold for exactly that, with a
                // stated product meaning. What it replaced was a bare `800` sitting unexplained
                // between this and `production_safe_pm` -- and structurally that 800 was the
                // SINGLE-OBSERVATION admission form `A <= 0.8 D`, which the device corpus refutes
                // at ~37% violation. The margin question is the window's; this is the disqualifier.
                //
                // A zero query is the most PERMISSIVE the rule can be -- every transfer factor
                // becomes 1 -- so an unreadable declared rate must refuse rather than fall through.
                sample.production_ratio_pm() < self.policy.production_max_pm
                    && query > 0
                    && self
                        .acquisitions
                        .admits(query, segment, buffered, self.policy.admission)
                        .is_some_and(Admission::admitted)
            }
        }
    }

    /// `now_ms` is the caller's clock **at the commit**, and it is a parameter rather than
    /// `self.now_ms` for a reason N10 depends on. `self.now_ms` is written only by `observe`, and
    /// on device a transaction runs `control.prime`, two playlist fetches, a warm-up fetch, a
    /// graded fetch and a feed between the `observe` that proposed and this call — so
    /// `self.now_ms` is the instant of the PROPOSAL. Anchoring the dwell there sets it expiring at
    /// `proposal + E_tx`, and `E_tx` is by construction the upper bound on the transaction's own
    /// duration: the guard would elapse at about the moment the transaction was guaranteed to be
    /// over, blocking roughly one sample instead of the interval N10 specifies. No host test could
    /// see it, because every fixture commits from the proposing `observe` with no clock advance,
    /// which reproduces exactly the anchor being corrected.
    pub(crate) fn commit(&mut self, proposal: Proposal, now_ms: u64) -> bool {
        if self.pending != Some(proposal) {
            return false;
        }
        self.current = proposal.rung;
        self.pending = None;
        self.samples_on_rung = 0;
        self.now_ms = self.now_ms.max(now_ms);
        // Both directions arm it; only the UP path reads it. See the field. The length is fixed
        // HERE, from the segment this transaction ran against.
        self.dwell_until_ms = Some(self.now_ms.saturating_add(self.upshift_dwell_ms()));
        // A rung that just committed is a rung that works. Whatever the last reject believed about
        // it has been answered by a transaction that finished, so the block is retired by evidence
        // of exactly the kind it was waiting for.
        if self.reject_block.map(|block| block.rung) == Some(proposal.rung) {
            self.reject_block = None;
        }
        true
    }

    /// **A reject now records what failed** (N11), where it recorded nothing and set a
    /// `cooldown = 1` that provably never blocked a segment.
    ///
    /// `cause` is the call site's own reading, and it decides whether a block is armed at all —
    /// see [`RejectCause`]. The block's two release conditions are computed HERE, from the state
    /// at the moment of failure, rather than re-derived later against numbers that have moved.
    pub(crate) fn reject(
        &mut self,
        proposal: Proposal,
        cause: RejectCause,
        now_ms: u64,
    ) -> bool {
        if self.pending != Some(proposal) {
            return false;
        }
        self.pending = None;
        self.now_ms = self.now_ms.max(now_ms);
        // **Only a DISCRETIONARY attempt arms the block, and a downshift is not one.** This guard
        // prices repeating a spend the controller chose to make; `dwell_until_ms`' doc already
        // draws the same line for the dwell — "a downshift is a recovery action and rate-limiting
        // recovery is how a stall becomes a policy" — and the argument is identical here, with a
        // sharper failure. `refill_time_ms` returns `None` whenever `safe_budget <= R_current`,
        // which IS the state a collapse-driven downshift is in, so a failed downshift armed a
        // block with no clock release at all; the only remaining exit is the budget exceeding the
        // raw rate the failing estimate believed, i.e. the link having to beat its own
        // pre-collapse reading. Between those, every upshift was refused indefinitely and playback
        // sat on the floor with a link that could carry several rungs. On the up path this cannot
        // arise: `best_sustainable` admits only `expected_wire <= 0.8 * safe`, so a surplus of at
        // least `0.25 * R` exists by construction and the block is bounded by `4 * E_tx`.
        //
        // It also settles the pricing: the cost below is `upshift_transaction_cost`, which is the
        // right ledger for the only direction that now reaches it. A downshift has no graded leg
        // and an unbounded warm-up, so charging it that sum was wrong twice over.
        if cause == RejectCause::Circumstance
            || proposal.direction != Direction::Up
            || self.last_segment_ms <= 0
        {
            return true;
        }
        // What the failed attempt spent: `E_tx`, the sum of the two deadlines it was held to —
        // the same quantity the dwell is armed for, asked for once.
        let cost_ms = i64::try_from(self.upshift_dwell_ms()).unwrap_or(i64::MAX);
        // What it takes to earn that back: the CURRENT rung is what playback keeps consuming while
        // the reserve refills, and the surplus is measured against the conservative budget rather
        // than the raw rate — this guard decides whether another attempt is affordable, and an
        // affordability question is answered on a lower bound.
        let current = self.catalog.candidate(self.current);
        let refill_ms = crate::abr::plant::refill_time_ms(
            cost_ms,
            current.expected_wire_kbps,
            hls_safe_budget(&self.delivery),
        );
        self.reject_block = Some(RejectBlock {
            rung: proposal.rung,
            release_at_ms: refill_ms
                .and_then(|ms| u64::try_from(ms).ok())
                .map(|ms| self.now_ms.saturating_add(ms)),
            evidence_kbps: self.delivery.slow_kbps,
        });
        true
    }

    /// `E_tx` for the segment currently in scope — the interval a commit arms the dwell for, and
    /// the debt a failed upshift owes. Zero when no segment has been measured, which is the
    /// correct reading of "no measurement" and cannot arm anything.
    ///
    /// **Saturate toward NOT blocking.** `upshift_transaction_cost` is bounded by `2.6 * d` today
    /// and this conversion cannot fail, but the safe failure of a guard is to let the decision
    /// through: `u64::MAX` here would be a dwell of 5.8e8 years, i.e. a permanent latch on every
    /// climb for the life of the demux, and it is one refactor away — the moment this cost is
    /// asked for a `Direction::Down`, `candidate_warmup_budget` returns `Duration::MAX` and
    /// `saturating_add` keeps it.
    fn upshift_dwell_ms(&self) -> u64 {
        if self.last_segment_ms <= 0 {
            return 0;
        }
        u64::try_from(
            crate::abr::viability::upshift_transaction_cost(
                std::time::Duration::from_millis(u64::try_from(self.last_segment_ms).unwrap_or(0)),
                &self.policy,
            )
            .as_millis(),
        )
        .unwrap_or(0)
    }

    /// Wall milliseconds still owed on the dwell guard (N10). `0` once the deadline has passed,
    /// and `0` before any commit — a controller that has started no encoder owes nothing.
    fn dwell_remaining_ms(&self) -> u64 {
        self.dwell_until_ms
            .map_or(0, |until| until.saturating_sub(self.now_ms))
    }

    /// Is a live reject block refusing this climb (N11)? See [`RejectBlock`] for why this takes no
    /// rung: the reserve pays `E_tx` for the ATTEMPT, whichever rung it was aimed at.
    fn reject_blocks(&self) -> bool {
        self.reject_block
            .is_some_and(|block| block.holds(self.now_ms, self.last_safe_budget_kbps))
    }
}
