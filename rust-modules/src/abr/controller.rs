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
    /// **Dev-only actuator pin — see [`Self::pinned_to`].** `None` in every production path.
    pin: Option<Rung>,
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
            pin: None,
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
        self.stable_samples = 0;
    }

    /// Everything one decision was made on, in one struct, for one event-log line. Assembled here
    /// rather than in `ff.rs` so the numbers logged are the numbers used.
    pub(crate) fn telemetry(&self) -> ControllerTelemetry {
        let current = self.catalog.candidate(self.current);
        ControllerTelemetry {
            current: self.current,
            safe_budget_kbps: self.last_safe_budget_kbps,
            // The SAME selection [`Self::observe`]'s upshift arm makes, four fifths of the budget
            // included — so the read-out cannot advertise an operating point the controller would
            // not actually choose. It is the answer to "what is this link worth", which is a
            // different question from "what is playing" and the one a viewer photographing the
            // panel is usually asking.
            optimal: self.catalog.best_sustainable(
                self.last_safe_budget_kbps * 4 / 5,
                &self.production,
                current,
                &self.policy,
            ),
            delivery: self.delivery,
            production: self.production,
            buffer: self.buffer,
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
        }
        self.delivery.update(observation);
        let cold_start = self.samples_on_rung == 0;
        self.production
            .observe(ratio, current_candidate.production_load_pm, cold_start);
        self.buffer
            .update(sample.buffer.buffered_ms(), i64::from(sample.media_duration_ms));
        self.samples_on_rung = self.samples_on_rung.saturating_add(1);

        let buffered = self.buffer.buffered_ms;
        let draining = self.buffer.draining();
        let segment = i64::from(sample.media_duration_ms);

        // **Computed HERE, above every early return, and only read below.** The budget is a pure
        // function of the four estimators, all of which this call has already updated, so its
        // value is identical wherever between here and the decision it is taken — nothing in
        // between mutates an input. Where it was computed mattered anyway, because three paths
        // return before reaching the decision: a transaction in flight, and both arms of the dev
        // pin. On a pinned run that is EVERY sample after the pin is reached, and the measured
        // consequence was that 397 of 527 `abr: steady` lines reported `safe=0kbps` — the central
        // quantity of the admission rule, unobservable on three quarters of the corpus, on
        // exactly the runs designed to characterise a rung.
        let safe_budget =
            hls_safe_budget(&self.delivery, &self.production, &self.buffer, &self.policy);
        self.last_safe_budget_kbps = safe_budget;

        if self.pending.is_some() {
            return Decision::Stay;
        }
        if self.cooldown > 0 {
            self.cooldown -= 1;
        }
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
            // plus `candidate_ready`'s residual), and both of those budgets are `None` going down.
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
        if buffer_bad || network_bad || production_bad {
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
                self.last_reason = Some(DecisionReason::Hls(if network_bad {
                    HlsReason::UnsafeCurrentState
                } else if production_bad {
                    HlsReason::ProductionConstraint
                } else {
                    HlsReason::BufferConstraint
                }));
                return Decision::Prime(proposal);
            }
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
        let Some(target_candidate) = self.catalog.best_sustainable(
            safe_budget * 4 / 5,
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

        // Upshift requires every resource signal to pass simultaneously. The target is selected
        // directly from the actuator catalog, so 8 -> 14-class budgets skip intermediate encoders.
        let all_good = safe_budget >= target_candidate.expected_wire_kbps
            && self.production.ratio_pm <= self.policy.production_safe_pm
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

    /// Candidate-session acceptance. Downshifts need a decodable complete segment and a surviving
    /// reserve; upshifts additionally require both network and JIT-production headroom at the
    /// candidate's actual rendition. The controller still does not mutate until `commit`.
    pub(crate) fn candidate_ready(&self, proposal: Proposal, sample: SegmentSample) -> bool {
        if self.pending != Some(proposal) {
            return false;
        }
        let buffered = sample.buffer.buffered_ms();
        let segment = i64::from(sample.media_duration_ms);
        if buffered < segment {
            return false;
        }
        match proposal.direction {
            Direction::Down => true,
            Direction::Up => {
                let candidate = self.catalog.candidate(proposal.rung);
                sample.network_kbps() >= candidate.expected_wire_kbps
                    && sample.production_ratio_pm() <= 800
                    && buffered >= segment.saturating_mul(2)
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

