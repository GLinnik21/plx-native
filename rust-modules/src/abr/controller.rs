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
    /// **The three counters that can hold an upshift back, and could not be seen.** J5 proposes
    /// replacing all three with derived guards; nothing can say what they COST until a log
    /// distinguishes "the evidence did not support a climb" from "the evidence supported it and a
    /// counter was still counting". Every field of `abr: steady` beside these is an estimator or a
    /// derived quantity; these are the raw state, and they are here for exactly one increment.
    pub(crate) gates: GateCounters,
}

/// Raw counter state, for the log line only. **Nothing reads this to decide anything** — it is the
/// instrumentation half of J5, landed before the policy half so the policy change has a baseline
/// to be measured against rather than an argument to be judged by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct GateCounters {
    /// Consecutive samples on which every `all_good` conjunct held. An upshift needs three.
    pub(crate) stable: u8,
    /// Samples still to be skipped after a commit or a reject.
    pub(crate) cooldown: u8,
    /// Samples taken since the current rung was committed. The first is a PMS cold start.
    pub(crate) on_rung: u8,
    /// Consecutive samples the reserve has been draining. The production trigger needs eight.
    /// **`u32` where the other three are `u8`**, because it is `BufferEstimate`'s own field and
    /// widening it here would be a second copy of that type's decision.
    pub(crate) draining: u32,
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
    stable_samples: u8,
    cooldown: u8,
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
            stable_samples: 0,
            cooldown: 0,
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
        // These counters describe uninterrupted time on this rung. A pause deliberately ages the
        // delivery estimate, so carrying the lifecycle guards across it would combine stale
        // evidence with a state that claims continuity.
        self.samples_on_rung = 0;
        self.stable_samples = 0;
        self.cooldown = 0;
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
            ),
            delivery: self.delivery,
            production: self.production,
            buffer: self.buffer,
            gates: GateCounters {
                stable: self.stable_samples,
                cooldown: self.cooldown,
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

    pub(crate) fn observe(&mut self, sample: SegmentSample) -> Decision {
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
        if self.cooldown > 0 {
            self.cooldown -= 1;
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
            self.stable_samples = 0;
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
        let network_bad = immediate_network < current_candidate.expected_wire_kbps;
        let production_bad =
            current_risk.production_risk && self.buffer.draining_samples >= 8;
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
        if horizon_bad || (!cold_start && (buffer_bad || network_bad || production_bad)) {
            self.stable_samples = 0;
            // A measured link collapse must not walk the ladder one oversized encoder at a time.
            // Select the best actuator that fits the new safe budget, still bounded below current.
            let target = if network_bad || buffered < segment / 2 {
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
                } else if network_bad {
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

        if self.cooldown > 0 || self.samples_on_rung < 2 {
            self.stable_samples = 0;
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
        ) else {
            self.stable_samples = 0;
            return Decision::Stay;
        };
        let target = target_candidate.rung;
        if target == self.current {
            self.stable_samples = 0;
            return Decision::Stay;
        }
        if target < self.current {
            // The budget shrank without any current-state signal failing. Nothing is wrong with
            // what is playing, so this is not a downshift trigger — it is a reason not to climb.
            self.stable_samples = 0;
            return Decision::Stay;
        }

        // **Selection must not propose what validation is certain to refuse.** Below `n` samples
        // `candidate_ready` cannot admit an upshift at all, and above it only admits one the
        // window can carry — so proposing outside that set is not merely wasted work, it is a
        // livelock: each proposal costs a real PMS encoder session and ~3 s of unrefilled
        // playback, the reject clears `stable_samples`, and the loop repeats forever.
        //
        // **The same rule on both sides, with the best input each side has.** Validation has the
        // rendition's own declared rate; selection cannot — a rung's `BANDWIDTH` needs a PMS
        // encoder session to exist first (§3) — so it evaluates against the catalog's
        // `expected_wire_kbps`, which is approximate (+5.2% to +31.6%) and is why validation still
        // decides. This is one rule read twice, not two thresholds stacked: no new number appears,
        // `n` and `σ` are the rule's own.
        let Some(target) = self.largest_admissible(target, segment, buffered) else {
            self.stable_samples = 0;
            return Decision::Stay;
        };
        if target <= self.current {
            self.stable_samples = 0;
            return Decision::Stay;
        }

        // The network constraint already selected `target` from the stricter named admission
        // budget above. The remaining independent resource guards must pass simultaneously.
        // The target is selected directly from the actuator catalog, so 8 -> 14-class budgets
        // skip intermediate encoders.
        let all_good = self.production.ratio_pm <= self.policy.production_safe_pm
            && buffered >= segment.saturating_mul(3)
            && !draining;
        if !all_good {
            self.stable_samples = 0;
            return Decision::Stay;
        }
        self.stable_samples = self.stable_samples.saturating_add(1);
        if self.stable_samples < 3 {
            return Decision::Stay;
        }
        self.stable_samples = 0;
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

    pub(crate) fn commit(&mut self, proposal: Proposal) -> bool {
        if self.pending != Some(proposal) {
            return false;
        }
        self.current = proposal.rung;
        self.pending = None;
        self.samples_on_rung = 0;
        self.stable_samples = 0;
        self.cooldown = match proposal.direction {
            Direction::Down => 8,
            Direction::Up => 3,
        };
        true
    }

    pub(crate) fn reject(&mut self, proposal: Proposal) -> bool {
        if self.pending != Some(proposal) {
            return false;
        }
        self.pending = None;
        self.stable_samples = 0;
        self.cooldown = 1;
        true
    }
}
