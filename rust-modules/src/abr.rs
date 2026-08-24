//! Client-managed adaptive-quality policy for Plex's measured fixed-rendition HLS sessions.
//!
//! The PMS probe proved that one HLS encoder session has one fixed rendition. A quality move is
//! therefore a transaction: propose a rung, prime a separately named encoder, then commit only
//! after that candidate has delivered a decodable segment with enough headroom. A rejected prime
//! leaves this controller's current rung untouched.
//!
//! All presentation values are normalized [`MediaTimeMs`] values. Raw FFmpeg PTS, stream time
//! bases, segment-local offsets and discontinuity counters never cross this boundary.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct MediaTimeMs(pub(crate) i64);

impl MediaTimeMs {
    pub(crate) fn saturating_since(self, earlier: Self) -> i64 {
        self.0.saturating_sub(earlier.0).max(0)
    }
}

/// Auto's request ladder. P240 is an emergency floor, not a settings row or startup quality.
/// Every other value is byte-for-byte aligned with `route::Quality`'s canonical fixed rungs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Rung {
    P240,
    P480,
    P720Low,
    P720,
    P1080,
    P1080High,
}

pub(crate) const LADDER: [Rung; 6] = [
    Rung::P240,
    Rung::P480,
    Rung::P720Low,
    Rung::P720,
    Rung::P1080,
    Rung::P1080High,
];

impl Rung {
    pub(crate) const fn kbps(self) -> u32 {
        match self {
            Rung::P240 => 320,
            Rung::P480 => 720,
            Rung::P720Low => 2_000,
            Rung::P720 => 4_000,
            Rung::P1080 => 8_000,
            Rung::P1080High => 20_000,
        }
    }

    pub(crate) const fn raster(self) -> (u16, u16) {
        match self {
            Rung::P240 => (426, 240),
            Rung::P480 => (854, 480),
            Rung::P720Low | Rung::P720 => (1280, 720),
            Rung::P1080 | Rung::P1080High => (1920, 1080),
        }
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

    fn below(self) -> Self {
        LADDER[self.index().saturating_sub(1)]
    }

    fn above(self) -> Self {
        LADDER[(self.index() + 1).min(LADDER.len() - 1)]
    }
}

/// Tail timestamps from the demuxer after normalization. `audio_expected` distinguishes genuinely
/// silent media from an A/V session whose audio lane has not produced a timestamp yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BufferSnapshot {
    pub(crate) playback: MediaTimeMs,
    pub(crate) video_tail: MediaTimeMs,
    pub(crate) audio_tail: Option<MediaTimeMs>,
    pub(crate) audio_expected: bool,
}

impl BufferSnapshot {
    pub(crate) fn buffered_ms(self) -> i64 {
        let tail = match (self.audio_expected, self.audio_tail) {
            (true, None) => return 0,
            (_, Some(audio)) => audio.min(self.video_tail),
            (false, None) => self.video_tail,
        };
        tail.saturating_since(self.playback)
    }
}

/// Validated timing for one completed segment. Invalid/zero timing is absence of evidence, never
/// infinite bandwidth or perfect production.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SegmentSample {
    bytes: u64,
    active_fetch_us: u64,
    total_fetch_us: u64,
    media_duration_ms: u32,
    pub(crate) buffer: BufferSnapshot,
}

impl SegmentSample {
    pub(crate) fn new(
        bytes: u64,
        active_fetch_us: u64,
        total_fetch_us: u64,
        media_duration_ms: u32,
        buffer: BufferSnapshot,
    ) -> Option<Self> {
        (bytes > 0
            && active_fetch_us > 0
            && total_fetch_us >= active_fetch_us
            && media_duration_ms > 0)
            .then_some(Self {
                bytes,
                active_fetch_us,
                total_fetch_us,
                media_duration_ms,
                buffer,
            })
    }

    fn network_kbps(self) -> u32 {
        (self.bytes.saturating_mul(8_000) / self.active_fetch_us)
            .min(u64::from(u32::MAX)) as u32
    }

    /// Per-mille total acquisition time / content duration. This includes PMS JIT production and
    /// TTFB; a two-second segment arriving in 1.9 seconds has almost no production headroom even
    /// if its response body crosses the LAN quickly.
    fn production_ratio_pm(self) -> u32 {
        (self.total_fetch_us.saturating_mul(1_000)
            / u64::from(self.media_duration_ms).saturating_mul(1_000))
            .min(u64::from(u32::MAX)) as u32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Down,
    Up,
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

/// An upshift candidate that cannot deliver one complete segment inside the same production
/// headroom threshold used by [`Controller::candidate_ready`] can never be committed. Give the
/// transport that exact budget so it returns to the active encoder before the playback reserve
/// drains. Downshifts have no such deadline: they are the recovery path when the current rung is
/// already unsustainable.
pub(crate) fn candidate_prime_budget(
    proposal: Proposal,
    media_duration: std::time::Duration,
) -> Option<std::time::Duration> {
    if proposal.direction == Direction::Down {
        return None;
    }
    let micros = media_duration.as_micros().saturating_mul(4) / 5;
    Some(std::time::Duration::from_micros(
        micros.min(u128::from(u64::MAX)) as u64,
    ))
}

/// Integer-only estimator and transaction state. Current-session samples decide whether to
/// propose. Candidate-session measurements decide whether that proposal may commit.
pub(crate) struct Controller {
    current: Rung,
    pending: Option<Proposal>,
    fast_network_kbps: u32,
    slow_network_kbps: u32,
    slow_ratio_pm: u32,
    samples_on_rung: u8,
    up_good: u8,
    cooldown: u8,
    last_buffer_ms: Option<i64>,
}

impl Controller {
    /// Unknown links start at 480p/720 kbit/s. P240 remains available as the emergency floor.
    pub(crate) fn bootstrap() -> Self {
        Self {
            current: Rung::P480,
            pending: None,
            fast_network_kbps: 0,
            slow_network_kbps: 0,
            slow_ratio_pm: 0,
            samples_on_rung: 0,
            up_good: 0,
            cooldown: 0,
            last_buffer_ms: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn current(&self) -> Rung {
        self.current
    }

    #[cfg(test)]
    pub(crate) fn pending(&self) -> Option<Proposal> {
        self.pending
    }

    pub(crate) fn observe(&mut self, sample: SegmentSample) -> Decision {
        let network = sample.network_kbps();
        let ratio = sample.production_ratio_pm();
        self.fast_network_kbps = ewma(self.fast_network_kbps, network, 1, 2);
        self.slow_network_kbps = ewma(self.slow_network_kbps, network, 1, 8);
        self.slow_ratio_pm = ewma(self.slow_ratio_pm, ratio, 1, 8);
        self.samples_on_rung = self.samples_on_rung.saturating_add(1);

        let buffered = sample.buffer.buffered_ms();
        let previous_buffer = self.last_buffer_ms.replace(buffered);
        let draining = previous_buffer.is_some_and(|old| buffered + 250 < old);
        let segment = i64::from(sample.media_duration_ms);

        if self.pending.is_some() {
            return Decision::Stay;
        }
        if self.cooldown > 0 {
            self.cooldown -= 1;
        }

        // Fast-down: a failure of either current-sustainability signal is enough. A JIT ratio
        // around 1.0 by itself is merely "encoder is real-time"; it becomes a downshift signal
        // only when the content buffer is actually draining.
        let immediate_network = network.min(self.fast_network_kbps);
        let network_bad = immediate_network < self.current.kbps().saturating_mul(11) / 10;
        let production_bad = ratio > 1_100 && draining;
        if buffered < segment || network_bad || production_bad {
            self.up_good = 0;
            // A measured link collapse must not walk the ladder one oversized encoder at a time:
            // on 512 Kbit/s, merely priming a 4 Mbit/s two-second rung takes ~16 seconds and drains
            // the reserve before it can commit. Jump to the highest rendition the new link can
            // sustain. Encoder pressure without a network failure remains a one-rung move.
            let target = if network_bad || buffered < segment / 2 {
                sustainable_rung(immediate_network, 120).min(self.current.below())
            } else {
                self.current.below()
            };
            if target != self.current {
                let proposal = Proposal { rung: target, direction: Direction::Down };
                self.pending = Some(proposal);
                return Decision::Prime(proposal);
            }
            return Decision::Stay;
        }

        if self.cooldown > 0 || self.samples_on_rung < 2 {
            self.up_good = 0;
            return Decision::Stay;
        }
        let target = self.current.above();
        if target == self.current {
            self.up_good = 0;
            return Decision::Stay;
        }

        // Slow-up: every signal must pass simultaneously, using the deliberately slower network
        // and production estimates. Stable JIT~=1.0 blocks an upshift but does not force a drop.
        let all_good = self.slow_network_kbps >= target.kbps().saturating_mul(135) / 100
            && self.slow_ratio_pm <= 750
            && buffered >= segment.saturating_mul(3)
            && !draining;
        if !all_good {
            self.up_good = 0;
            return Decision::Stay;
        }
        self.up_good = self.up_good.saturating_add(1);
        if self.up_good < 3 {
            return Decision::Stay;
        }
        self.up_good = 0;
        let proposal = Proposal { rung: target, direction: Direction::Up };
        self.pending = Some(proposal);
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
                sample.network_kbps() >= proposal.rung.kbps().saturating_mul(135) / 100
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
        self.up_good = 0;
        self.cooldown = match proposal.direction {
            Direction::Down => 1,
            Direction::Up => 3,
        };
        self.last_buffer_ms = None;
        true
    }

    pub(crate) fn reject(&mut self, proposal: Proposal) -> bool {
        if self.pending != Some(proposal) {
            return false;
        }
        self.pending = None;
        self.up_good = 0;
        self.cooldown = 1;
        true
    }
}

fn ewma(old: u32, new: u32, new_weight: u64, denominator: u64) -> u32 {
    if old == 0 {
        new
    } else {
        ((u64::from(old) * (denominator - new_weight) + u64::from(new) * new_weight)
            / denominator)
            .min(u64::from(u32::MAX)) as u32
    }
}

fn sustainable_rung(network_kbps: u32, margin_percent: u32) -> Rung {
    LADDER
        .iter()
        .copied()
        .rev()
        .find(|rung| rung.kbps().saturating_mul(margin_percent) / 100 <= network_kbps)
        .unwrap_or(Rung::P240)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(network_kbps: u32, ratio_pm: u32, buffered_ms: i64) -> SegmentSample {
        let media_ms = 2_000;
        let total_us = u64::from((media_ms * ratio_pm / 1_000).max(1)) * 1_000;
        let active_us = total_us.min(200_000);
        let bytes = u64::from(network_kbps) * active_us / 8_000;
        SegmentSample::new(
            bytes,
            active_us,
            total_us,
            media_ms,
            BufferSnapshot {
                playback: MediaTimeMs(10_000),
                video_tail: MediaTimeMs(10_000 + buffered_ms),
                audio_tail: Some(MediaTimeMs(10_000 + buffered_ms)),
                audio_expected: true,
            },
        )
        .unwrap()
    }

    fn prime_up(controller: &mut Controller) -> Proposal {
        for _ in 0..4 {
            if let Decision::Prime(proposal) = controller.observe(sample(20_000, 200, 10_000)) {
                return proposal;
            }
        }
        panic!("no proposal")
    }

    fn settle_link(network_kbps: u32) -> Rung {
        let mut controller = Controller::bootstrap();
        for _ in 0..80 {
            if let Decision::Prime(proposal) =
                controller.observe(sample(network_kbps, 400, 10_000))
            {
                let candidate = sample(network_kbps, 400, 12_000);
                if controller.candidate_ready(proposal, candidate) {
                    controller.commit(proposal);
                } else {
                    controller.reject(proposal);
                }
            }
        }
        controller.current()
    }

    #[test]
    fn zero_or_inconsistent_measurements_are_not_infinite_headroom() {
        let b = BufferSnapshot {
            playback: MediaTimeMs(0),
            video_tail: MediaTimeMs(5_000),
            audio_tail: None,
            audio_expected: false,
        };
        assert!(SegmentSample::new(1, 0, 1, 2_000, b).is_none());
        assert!(SegmentSample::new(1, 2, 1, 2_000, b).is_none());
        assert!(SegmentSample::new(0, 1, 1, 2_000, b).is_none());
        assert!(SegmentSample::new(1, 1, 1, 0, b).is_none());
    }

    #[test]
    fn audio_expected_without_a_tail_has_no_buffer_but_silent_video_does() {
        let mut b = BufferSnapshot {
            playback: MediaTimeMs(1_000),
            video_tail: MediaTimeMs(5_000),
            audio_tail: None,
            audio_expected: true,
        };
        assert_eq!(b.buffered_ms(), 0);
        b.audio_expected = false;
        assert_eq!(b.buffered_ms(), 4_000);
    }

    #[test]
    fn realtime_jit_blocks_upshift_without_forcing_a_downshift() {
        let mut controller = Controller::bootstrap();
        for _ in 0..10 {
            assert_eq!(controller.observe(sample(20_000, 1_000, 10_000)), Decision::Stay);
        }
        assert_eq!(controller.current(), Rung::P480);
    }

    #[test]
    fn a_proposal_does_not_mutate_current_until_candidate_commit() {
        let mut controller = Controller::bootstrap();
        let proposal = prime_up(&mut controller);
        assert_eq!(proposal.rung, Rung::P720Low);
        assert_eq!(controller.current(), Rung::P480);
        assert_eq!(controller.pending(), Some(proposal));
        assert!(controller.candidate_ready(proposal, sample(20_000, 200, 12_000)));
        assert!(controller.commit(proposal));
        assert_eq!(controller.current(), Rung::P720Low);
    }

    #[test]
    fn rejected_candidate_preserves_current_and_clears_pending() {
        let mut controller = Controller::bootstrap();
        let proposal = prime_up(&mut controller);
        assert!(!controller.candidate_ready(proposal, sample(2_100, 950, 12_000)));
        assert!(controller.reject(proposal));
        assert_eq!(controller.current(), Rung::P480);
        assert_eq!(controller.pending(), None);
    }

    #[test]
    fn startup_does_not_issue_back_to_back_encoder_swaps() {
        let mut controller = Controller::bootstrap();
        let proposal = prime_up(&mut controller);
        controller.commit(proposal);
        for _ in 0..3 {
            assert_eq!(controller.observe(sample(20_000, 200, 12_000)), Decision::Stay);
        }
    }

    #[test]
    fn a_single_slow_network_sample_jumps_to_the_measured_sustainable_rung() {
        let mut controller = Controller::bootstrap();
        controller.current = Rung::P720;
        let decision = controller.observe(sample(1_000, 400, 8_000));
        assert_eq!(
            decision,
            Decision::Prime(Proposal { rung: Rung::P480, direction: Direction::Down })
        );
        assert_eq!(controller.current(), Rung::P720);
    }

    #[test]
    fn a_runtime_collapse_from_the_top_does_not_prime_oversized_intermediate_rungs() {
        let mut controller = Controller::bootstrap();
        controller.current = Rung::P1080High;
        assert_eq!(
            controller.observe(sample(512, 1_000, 8_000)),
            Decision::Prime(Proposal { rung: Rung::P240, direction: Direction::Down })
        );
    }

    #[test]
    fn draining_jit_session_downshifts_but_stable_jit_does_not() {
        let mut controller = Controller::bootstrap();
        assert_eq!(controller.observe(sample(20_000, 1_200, 8_000)), Decision::Stay);
        assert_eq!(
            controller.observe(sample(20_000, 1_200, 6_000)),
            Decision::Prime(Proposal { rung: Rung::P240, direction: Direction::Down })
        );
    }

    #[test]
    fn the_auto_ladder_matches_fixed_quality_wire_values() {
        assert_eq!(Rung::P480.kbps(), 720);
        assert_eq!(Rung::P720Low.kbps(), 2_000);
        assert_eq!(Rung::P720.kbps(), 4_000);
        assert_eq!(Rung::P1080.kbps(), 8_000);
        assert_eq!(Rung::P1080High.kbps(), 20_000);
        assert_eq!(Rung::P1080High.raster(), (1920, 1080));
    }

    #[test]
    fn only_upshift_primes_receive_the_exact_acceptance_budget() {
        let media = std::time::Duration::from_millis(2_002);
        let up = Proposal { rung: Rung::P720Low, direction: Direction::Up };
        let down = Proposal { rung: Rung::P240, direction: Direction::Down };
        assert_eq!(candidate_prime_budget(up, media), Some(std::time::Duration::from_micros(1_601_600)));
        assert_eq!(candidate_prime_budget(down, media), None);
    }

    #[test]
    fn lg_network_legs_settle_on_sustainable_rungs() {
        assert_eq!(settle_link(512), Rung::P240);
        assert_eq!(settle_link(1_000), Rung::P480);
        assert_eq!(settle_link(7_000), Rung::P720);
        assert_eq!(settle_link(17_500), Rung::P1080);
    }
}
