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

    /// Recover the controller's starting rung from the exact ceiling stored in the playback
    /// route. Auto owns only these canonical values; an arbitrary/manual ceiling is not an ABR
    /// state and therefore has no answer here.
    pub(crate) fn from_ceiling(ceiling: crate::plex::Ceiling) -> Option<Self> {
        LADDER.iter().copied().find(|rung| rung.ceiling() == ceiling)
    }
}

/// A runtime Original failure is not an unknown-link bootstrap: the direct transfer has just
/// measured the link. Start the replacement at the highest rung with 35% measured headroom, so a
/// 4 Mbit/s cap enters at 2 Mbit/s/720p rather than needlessly flashing the 240p emergency floor.
pub(crate) fn original_fallback_rung(measured_kbps: u32) -> Rung {
    sustainable_rung(measured_kbps, 135)
}

/// The same admission rule at startup and during HLS recovery: the measured source prefix must
/// complete and carry the whole-file average with 35% peak/headroom reserve. The live watchdog
/// owns later link collapses, so startup need not reserve a second, redundant 50% margin.
pub(crate) fn original_sustainable(source_kbps: u32, measured_kbps: u32, complete: bool) -> bool {
    source_kbps > 0
        && complete
        && u64::from(measured_kbps).saturating_mul(1_000)
            >= u64::from(source_kbps).saturating_mul(1_350)
}

const ORIGINAL_RECOVERY_TOP_SAMPLES: u8 = 3;
const ORIGINAL_RECOVERY_GOOD_PROBES: u8 = 2;

/// Slow, explicit HLS→Original gate. HLS throughput and PMS encoder cadence are not evidence for
/// the differently-shaped source request, so this object merely schedules probes after the top
/// rung has built a safe buffer and then requires two successful actual-source measurements.
pub(crate) struct OriginalRecovery {
    source_kbps: u32,
    top_samples: u8,
    good_probes: u8,
}

impl OriginalRecovery {
    pub(crate) fn new(source_kbps: u32) -> Option<Self> {
        (source_kbps > 0).then_some(Self { source_kbps, top_samples: 0, good_probes: 0 })
    }

    pub(crate) fn probe_due(&mut self, current: Rung, sample: SegmentSample) -> bool {
        let healthy = current == Rung::P1080High
            && sample.buffer.buffered_ms() >= i64::from(sample.media_duration_ms);
        if !healthy {
            self.top_samples = 0;
            self.good_probes = 0;
            return false;
        }
        self.top_samples = self.top_samples.saturating_add(1);
        if self.top_samples < ORIGINAL_RECOVERY_TOP_SAMPLES {
            return false;
        }
        self.top_samples = 0;
        true
    }

    pub(crate) fn observe_probe(&mut self, measured_kbps: u32, complete: bool) -> bool {
        if original_sustainable(self.source_kbps, measured_kbps, complete) {
            self.good_probes = self.good_probes.saturating_add(1);
        } else {
            self.good_probes = 0;
        }
        self.good_probes >= ORIGINAL_RECOVERY_GOOD_PROBES
    }
}

const ORIGINAL_WINDOW_US: u64 = 750_000;
const ORIGINAL_LOW_BUFFER_MS: i64 = 3_500;
const ORIGINAL_REQUIRED_HEADROOM_PM: u64 = 1_100;
const ORIGINAL_BAD_WINDOWS: u8 = 2;

/// One completed runtime-Original measurement window. The demuxer logs the exact evidence and
/// publishes `measured_kbps` to the main thread when `fallback` becomes true.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OriginalObservation {
    pub(crate) measured_kbps: u32,
    pub(crate) buffered_ms: i64,
    pub(crate) bad_windows: u8,
    pub(crate) fallback: bool,
}

/// Hysteresis for Auto's Original state.
///
/// The transfer counters contain successful body-read time only, while `buffered_ms` is content
/// time in the one normalized movie timeline. Either signal alone lies: a shaped socket can look
/// slow while a large reserve makes it harmless, and a temporarily small queue can occur while a
/// fast reader is refilling it. Two complete 750 ms windows must say both "below the source's
/// requirement" and "less than 3.5 seconds remain" before the expensive encoder transition.
pub(crate) struct OriginalWatchdog {
    source_kbps: u32,
    last_bytes: u64,
    last_active_us: u64,
    bad_windows: u8,
}

impl OriginalWatchdog {
    pub(crate) fn new(source_kbps: u32) -> Option<Self> {
        (source_kbps > 0).then_some(Self {
            source_kbps,
            last_bytes: 0,
            last_active_us: 0,
            bad_windows: 0,
        })
    }

    pub(crate) fn reset(&mut self, bytes: u64, active_us: u64) {
        self.last_bytes = bytes;
        self.last_active_us = active_us;
        self.bad_windows = 0;
    }

    pub(crate) fn observe(
        &mut self,
        bytes: u64,
        active_us: u64,
        buffered_ms: Option<i64>,
    ) -> Option<OriginalObservation> {
        if bytes < self.last_bytes || active_us < self.last_active_us {
            self.reset(bytes, active_us);
            return None;
        }
        let active_delta = active_us - self.last_active_us;
        if active_delta < ORIGINAL_WINDOW_US {
            return None;
        }
        let byte_delta = bytes - self.last_bytes;
        self.last_bytes = bytes;
        self.last_active_us = active_us;
        let Some(buffered_ms) = buffered_ms else {
            self.bad_windows = 0;
            return None;
        };
        if byte_delta == 0 {
            self.bad_windows = 0;
            return None;
        }
        let measured_kbps = (byte_delta.saturating_mul(8_000) / active_delta)
            .min(u64::from(u32::MAX)) as u32;
        let rate_bad = u64::from(measured_kbps).saturating_mul(1_000)
            < u64::from(self.source_kbps).saturating_mul(ORIGINAL_REQUIRED_HEADROOM_PM);
        let buffer_bad = buffered_ms <= ORIGINAL_LOW_BUFFER_MS;
        if rate_bad && buffer_bad {
            self.bad_windows = self.bad_windows.saturating_add(1);
        } else {
            self.bad_windows = 0;
        }
        Some(OriginalObservation {
            measured_kbps,
            buffered_ms,
            bad_windows: self.bad_windows,
            fallback: self.bad_windows >= ORIGINAL_BAD_WINDOWS,
        })
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

    pub(crate) fn network_kbps(self) -> u32 {
        (self.bytes.saturating_mul(8_000) / self.active_fetch_us)
            .min(u64::from(u32::MAX)) as u32
    }

    pub(crate) fn media_duration_ms(self) -> u32 {
        self.media_duration_ms
    }

    /// Per-mille total acquisition time / content duration. This includes PMS JIT production and
    /// TTFB; a two-second segment arriving in 1.9 seconds has almost no production headroom even
    /// if its response body crosses the LAN quickly.
    pub(crate) fn production_ratio_pm(self) -> u32 {
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

/// A new PMS encoder's first segment includes decoder/encoder cold start and is not a
/// steady-state production sample. Give that one warm-up segment a bounded 1.5 content-duration
/// window, then apply [`candidate_prime_budget`] to the following segment before committing the
/// encoder. The proposal gate already requires at least three segments of reserve, so the warm-up
/// plus the graded segment still fits inside the buffer available when an upshift starts.
/// Downshifts keep their established recovery behavior and do not acquire a deadline here.
pub(crate) fn candidate_warmup_budget(
    proposal: Proposal,
    media_duration: std::time::Duration,
) -> Option<std::time::Duration> {
    if proposal.direction == Direction::Down {
        return None;
    }
    let micros = media_duration.as_micros().saturating_mul(3) / 2;
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
    production_bad_windows: u8,
    last_buffer_ms: Option<i64>,
}

impl Controller {
    /// Unknown links start at 480p/720 kbit/s. P240 remains available as the emergency floor.
    #[cfg(test)]
    pub(crate) fn bootstrap() -> Self {
        Self::starting_at(Rung::P480)
    }

    /// The active encoder already exists at `current` (including a runtime Original fallback
    /// chosen from its measured link), so the controller must begin from that exact wire state.
    pub(crate) fn starting_at(current: Rung) -> Self {
        Self {
            current,
            pending: None,
            fast_network_kbps: 0,
            slow_network_kbps: 0,
            slow_ratio_pm: 0,
            samples_on_rung: 0,
            up_good: 0,
            cooldown: 0,
            production_bad_windows: 0,
            last_buffer_ms: None,
        }
    }

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
        if ratio > 1_100 && draining {
            self.production_bad_windows = self.production_bad_windows.saturating_add(1);
        } else {
            self.production_bad_windows = 0;
        }
        let production_bad = self.production_bad_windows >= 3;
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
        self.production_bad_windows = 0;
        self.cooldown = match proposal.direction {
            Direction::Down => 8,
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
        self.production_bad_windows = 0;
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
    fn original_watchdog_requires_two_slow_low_buffer_windows() {
        let mut watchdog = OriginalWatchdog::new(28_000).unwrap();
        let bytes_per_window = 375_000; // 4,000 kbit/s over 750 ms
        assert!(watchdog.observe(bytes_per_window, 749_999, Some(3_000)).is_none());
        let first = watchdog
            .observe(bytes_per_window, ORIGINAL_WINDOW_US, Some(3_000))
            .unwrap();
        assert_eq!(first.measured_kbps, 4_000);
        assert!(!first.fallback, "one shaped window is jitter, not a mode switch");
        let second = watchdog
            .observe(bytes_per_window * 2, ORIGINAL_WINDOW_US * 2, Some(2_500))
            .unwrap();
        assert!(second.fallback, "the sustained 4 Mbit/s cap cannot carry a 28 Mbit/s file");
    }

    #[test]
    fn original_watchdog_ignores_slow_reads_while_the_content_reserve_is_healthy() {
        let mut watchdog = OriginalWatchdog::new(28_000).unwrap();
        let bytes = 375_000;
        for window in 1..=4 {
            let observation = watchdog
                .observe(
                    bytes * window,
                    ORIGINAL_WINDOW_US * window,
                    Some(8_000),
                )
                .unwrap();
            assert!(!observation.fallback);
        }
    }

    #[test]
    fn a_recovered_window_clears_original_fallback_hysteresis() {
        let mut watchdog = OriginalWatchdog::new(10_000).unwrap();
        let slow_bytes = 375_000;
        assert!(!watchdog
            .observe(slow_bytes, ORIGINAL_WINDOW_US, Some(2_000))
            .unwrap()
            .fallback);
        let fast_bytes = slow_bytes + 1_875_000; // 20 Mbit/s for the next window
        assert!(!watchdog
            .observe(fast_bytes, ORIGINAL_WINDOW_US * 2, Some(2_000))
            .unwrap()
            .fallback);
        assert!(!watchdog
            .observe(fast_bytes + slow_bytes, ORIGINAL_WINDOW_US * 3, Some(2_000))
            .unwrap()
            .fallback);
    }

    #[test]
    fn measured_runtime_fallback_avoids_an_unnecessarily_low_bootstrap() {
        assert_eq!(original_fallback_rung(512), Rung::P240);
        assert_eq!(original_fallback_rung(4_000), Rung::P720Low);
        assert_eq!(original_fallback_rung(7_000), Rung::P720);
        assert_eq!(Rung::from_ceiling(Rung::P720Low.ceiling()), Some(Rung::P720Low));
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
        assert_eq!(controller.observe(sample(20_000, 1_200, 6_000)), Decision::Stay);
        assert_eq!(controller.observe(sample(20_000, 1_200, 4_000)), Decision::Stay);
        assert_eq!(controller.observe(sample(20_000, 1_200, 2_000)),
            Decision::Prime(Proposal { rung: Rung::P240, direction: Direction::Down }));
    }

    #[test]
    fn original_recovery_requires_two_spaced_actual_source_probes() {
        let mut gate = OriginalRecovery::new(28_000).unwrap();
        for n in 1..=ORIGINAL_RECOVERY_TOP_SAMPLES {
            assert_eq!(
                // A real-time JIT encoder must not block escape to the GPU-free Original path.
                gate.probe_due(Rung::P1080High, sample(60_000, 2_000, 10_000)),
                n == ORIGINAL_RECOVERY_TOP_SAMPLES,
            );
        }
        assert!(!gate.observe_probe(60_000, true));
        for n in 1..=ORIGINAL_RECOVERY_TOP_SAMPLES {
            assert_eq!(
                gate.probe_due(Rung::P1080High, sample(60_000, 500, 10_000)),
                n == ORIGINAL_RECOVERY_TOP_SAMPLES,
            );
        }
        assert!(gate.observe_probe(60_000, true));
    }

    #[test]
    fn original_recovery_resets_below_the_top_or_after_a_failed_probe() {
        let mut gate = OriginalRecovery::new(28_000).unwrap();
        for _ in 0..ORIGINAL_RECOVERY_TOP_SAMPLES {
            assert!(!gate.probe_due(Rung::P1080, sample(60_000, 500, 10_000)));
        }
        for _ in 1..ORIGINAL_RECOVERY_TOP_SAMPLES {
            assert!(!gate.probe_due(Rung::P1080High, sample(60_000, 500, 10_000)));
        }
        assert!(gate.probe_due(Rung::P1080High, sample(60_000, 500, 10_000)));
        assert!(!gate.observe_probe(60_000, true));
        assert!(!gate.observe_probe(30_000, true));
        assert!(!gate.observe_probe(60_000, false));
    }

    #[test]
    fn a_downshift_holds_long_enough_to_avoid_immediate_top_rung_flapping() {
        let mut controller = Controller::starting_at(Rung::P1080High);
        let Decision::Prime(down) = controller.observe(sample(12_000, 500, 8_000)) else {
            panic!("the collapsed link must propose a downshift")
        };
        assert!(controller.commit(down));
        for _ in 0..9 {
            assert_eq!(controller.observe(sample(60_000, 400, 10_000)), Decision::Stay);
        }
        assert_eq!(
            controller.observe(sample(60_000, 400, 10_000)),
            Decision::Prime(Proposal { rung: Rung::P1080High, direction: Direction::Up }),
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
        assert_eq!(
            candidate_prime_budget(up, media),
            Some(std::time::Duration::from_micros(1_601_600))
        );
        assert_eq!(
            candidate_warmup_budget(up, media),
            Some(std::time::Duration::from_micros(3_003_000))
        );
        assert_eq!(candidate_prime_budget(down, media), None);
        assert_eq!(candidate_warmup_budget(down, media), None);
    }

    #[test]
    fn lg_network_legs_settle_on_sustainable_rungs() {
        assert_eq!(settle_link(512), Rung::P240);
        assert_eq!(settle_link(1_000), Rung::P480);
        assert_eq!(settle_link(7_000), Rung::P720);
        assert_eq!(settle_link(17_500), Rung::P1080);
    }
}
