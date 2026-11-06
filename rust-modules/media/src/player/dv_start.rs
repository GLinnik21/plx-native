//! player::dv_start — the Dolby Vision landing guard (#1166): one pure state per Engine that takes the
//! `DolbyHdrInfo` node out of a play whose first picture the firmware's Dolby Vision splitter will not
//! present.
//!
//! The splitter (`libgstdvsplitter.so`, built only when a Load carries the node) of webOS 5.5.0-19 lets
//! only an HEVC IDR be the first picture after its init or a flush. A file whose only IDR is frame 0
//! and whose later random access points are CRAs therefore sits on Buffering for good after every seek
//! and every resume: the pipeline accepts everything and presents nothing. Dolby Vision Off plays the
//! same landings (no node, no splitter), and a Profile 8.1 base layer is HDR10, so the answer is to
//! withdraw the declaration for the rest of the play and reload once.
//!
//! Two triggers, one rule: a landing keyframe fed and then no picture.
//! - **The row** (`devcaps::DvFirstPicture::IdrOnly`, per firmware in `devcaps::EXCLUSIONS`): the
//!   landing keyframe is not an IDR ([`crate::aq::hevc_irap_not_idr`] on the access unit's `key`), so
//!   it is a prediction at the landing, before anything is fed: [`Guard::land`] answers
//!   [`Step::Withdraw`] and the keyframe is held back.
//! - **The watch**: for a build nobody has read, at least [`ACCEPTED_MIN_NS`] of video accepted past the
//!   landing and the pipeline's position counter (`SHARED.frames`) still zero for
//!   [`DV_FIRST_PICTURE_MS`] of running clock. [`Guard::step`] answers [`Step::Withdraw`].
//!
//! Which plays are guarded is the guard's [`Mode`], chosen once by `engine::dv_start_for` from the
//! route: [`Mode::Act`] (a declared direct play), [`Mode::ReportOnly`] (the node is sent but this change
//! has no rung for the route: a Profile 5 direct play, or a remux that copies a declared video) and
//! [`Mode::Inert`] (no node, or a trailer preview).
//!
//! Pure state, no clock and no globals read here: the pump hands in a [`Probe`] and the engine a
//! [`Landing`], so every rule is a unit test. One [`Guard`] lives on the Engine and is re-armed at every
//! landing ([`Guard::rearm`]: the Engine's start, and each in-place seek).

// The vocabulary lands before the code that reads it (the engine's landing hook and the pump's driver
// come in the next commit of this change, which deletes this attribute).
#![allow(dead_code)]

use plx_platform::devcaps::DvFirstPicture;
use std::os::raw::c_int;

/// How long the watch's clock may run, with a landing keyframe fed and no position update since, before
/// the declaration is withdrawn: the margin `native_stall::NATIVE_AUDIO_STALL_MS` keeps (a 4K HEVC
/// reopen, seek and first keyframe is about 700 ms).
pub(super) const DV_FIRST_PICTURE_MS: u32 = 4_000;

/// The video the pipeline must have accepted past the landing before the watch's clock may run: what
/// the prime holds Play for ([`super::engine`]'s `PRIME_NS`), so a slow server that under-feeds never
/// reads as a silent pipeline.
pub(super) const ACCEPTED_MIN_NS: i64 = super::engine::PRIME_NS;

/// What a play's guard does, decided once from the route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    /// Nothing: no node is sent, or the play is a trailer preview.
    Inert,
    /// The node is sent and this change has no rung for the route: say so, change nothing.
    ReportOnly,
    /// Withdraw the declaration.
    Act,
}

/// The landing keyframe, as the engine's `feed_stream` meets it: the first post-seek keyframe in the
/// rebase branch, or an Engine started with no seek armed, its first video keyframe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Landing {
    /// The keyframe's content pts (ns).
    pub kf_pts: i64,
    /// The access unit's `key` ([`crate::aq::AuNode`]): `0` is not a keyframe, [`crate::aq::AU_KEY`] a
    /// keyframe the splitter takes, any other value the HEVC NAL type of a non-IDR IRAP.
    pub key: c_int,
    /// An in-place seek of a live Engine (a native Seek or the Segment path), not an Engine's start.
    pub in_place: bool,
}

/// What the pump knows this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Probe {
    /// The pump's millisecond clock.
    pub now: u32,
    /// The landing keyframe was fed (`!eng.rebase_pending`).
    pub landed: bool,
    /// The prime has released Play (`!eng.prime_play`).
    pub prime_released: bool,
    /// Video accepted past the landing (`eng.max_fed_video_pts - eng.seek_base_pts`).
    pub accepted_ns: i64,
    /// The feed gate is open (`TX.feed_allowed()`).
    pub feed_open: bool,
    /// The viewer's clock runs, or a paused-seek preroll is open (not `TX.paused && !preroll`).
    pub clock_running: bool,
    /// The pipeline's position updates since the last landing (`SHARED.frames`, seek-scoped).
    pub frames: i32,
}

/// Why a withdrawal or a report was raised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Cause {
    /// The landing keyframe is not an IDR on a firmware whose splitter takes only an IDR first.
    Row,
    /// No position update followed the landing keyframe: how long the clock ran, and the video accepted.
    NoPositionUpdate { silent_ms: u32, accepted_ns: i64 },
}

/// A withdrawal the driver executes: withdraw on the route, then reload once without the node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Withdrawal {
    /// The landing keyframe's content pts (ns).
    pub kf_pts: i64,
    /// The landing keyframe's HEVC NAL type when it is a non-IDR IRAP.
    pub nal: Option<u8>,
    /// The seek target this guard was armed with: an Engine started with a seek armed, or the in-place
    /// seek's target (`None` for a plain start).
    pub armed_target: Option<i64>,
    /// An in-place seek of a live Engine ([`Landing::in_place`]).
    pub in_place: bool,
    pub cause: Cause,
}

/// A report-only guard's one line per landing and its one line for a silent pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Report {
    pub kf_pts: i64,
    pub nal: Option<u8>,
    /// The firmware row says the splitter takes only an IDR first.
    pub row: bool,
    pub cause: Cause,
}

/// What the guard asks the engine or the pump to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    Quiet,
    /// Log this line; nothing else changes.
    Report(Report),
    /// Withdraw the declaration and reload. From [`Guard::land`] it also stays pending
    /// ([`Guard::withdrawal_pending`]) until the driver takes it ([`Guard::take_withdrawal`]), and the
    /// engine holds the keyframe back meanwhile.
    Withdraw(Withdrawal),
}

pub(super) struct Guard {
    mode: Mode,
    /// What the attempt's caps say the set's splitter takes first.
    row: DvFirstPicture,
    armed_target: Option<i64>,
    /// A landing withdrawal not yet taken by the driver.
    pending: Option<Withdrawal>,
}

impl Guard {
    /// A guard that never does anything: the Engine literal's value when no node is sent.
    pub(super) const fn inert() -> Guard {
        Guard { mode: Mode::Inert, row: DvFirstPicture::AnyIrap, armed_target: None, pending: None }
    }

    /// A guard in `mode` for a set whose splitter takes `row` first, armed with `armed_target` (the
    /// seek an Engine started with, or `None`). `engine::dv_start_for` is the one production caller.
    pub(super) fn armed(mode: Mode, row: DvFirstPicture, armed_target: Option<i64>) -> Guard {
        Guard { mode, row, armed_target, pending: None }
    }

    pub(super) fn mode(&self) -> Mode {
        self.mode
    }

    /// Start a fresh landing: forget the last one's clock, report and pending withdrawal, and arm with
    /// `armed_target`, the in-place seek's target.
    pub(super) fn rearm(&mut self, armed_target: Option<i64>) {
        self.armed_target = armed_target;
        self.pending = None;
    }

    /// The landing keyframe `l` arrived. A guard in [`Mode::Act`] on a set whose splitter takes only an
    /// IDR answers [`Step::Withdraw`] for a non-IDR IRAP and holds it pending; a [`Mode::ReportOnly`]
    /// guard answers [`Step::Report`] for the same landing; every other landing starts the watch.
    pub(super) fn land(&mut self, _l: &Landing) -> Step {
        Step::Quiet
    }

    /// A withdrawal [`land`](Self::land) raised is waiting for the driver. While one is, the seek
    /// stuck-watchdog yields and both lane feeders hold.
    pub(super) fn withdrawal_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn take_withdrawal(&mut self) -> Option<Withdrawal> {
        self.pending.take()
    }

    /// One pump tick of the watch. Its clock runs only while the landing keyframe was fed, the prime has
    /// released Play, at least [`ACCEPTED_MIN_NS`] was accepted past it, the feed gate is open, the
    /// viewer's clock runs and `frames` is zero; any condition dropping resets it. It fires once per
    /// landing, at [`DV_FIRST_PICTURE_MS`].
    pub(super) fn step(&mut self, _p: &Probe) -> Step {
        Step::Quiet
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: i64 = 74_212_999_999;
    const KF: i64 = 71_780_000_000;
    const CRA: c_int = 21;
    const IDR: c_int = crate::aq::AU_KEY;

    fn landing(key: c_int) -> Landing {
        Landing { kf_pts: KF, key, in_place: true }
    }

    /// A tick at `now` ms with every condition of the watch's clock met and no position update.
    fn probe(now: u32) -> Probe {
        Probe {
            now,
            landed: true,
            prime_released: true,
            accepted_ns: 1_000_000_000,
            feed_open: true,
            clock_running: true,
            frames: 0,
        }
    }

    fn act(row: DvFirstPicture) -> Guard {
        Guard::armed(Mode::Act, row, Some(TARGET))
    }

    fn silent(now: u32) -> Cause {
        Cause::NoPositionUpdate { silent_ms: now, accepted_ns: 1_000_000_000 }
    }

    fn withdrawal(nal: Option<u8>, cause: Cause) -> Withdrawal {
        Withdrawal { kf_pts: KF, nal, armed_target: Some(TARGET), in_place: true, cause }
    }

    /// Step the watch at `times`, returning every answer that was not `Quiet`.
    fn fired(g: &mut Guard, times: &[u32]) -> Vec<(u32, Step)> {
        times.iter().filter_map(|&t| Some((t, g.step(&probe(t)))).filter(|(_, s)| *s != Step::Quiet)).collect()
    }

    #[test]
    fn dv_start_a_cra_landing_on_an_idr_only_set_withdraws_with_that_target() {
        let mut g = act(DvFirstPicture::IdrOnly);
        let w = withdrawal(Some(21), Cause::Row);
        assert_eq!(g.land(&landing(CRA)), Step::Withdraw(w));
        assert!(g.withdrawal_pending(), "held for the driver, which also stops the stuck-watchdog");
        assert_eq!(g.take_withdrawal(), Some(w));
        assert!(!g.withdrawal_pending() && g.take_withdrawal().is_none(), "taken once");
        // Every non-IDR IRAP type is the same rule; the reserved ones are withdrawn too.
        for t in [16, 17, 18, 22, 23] {
            let mut g = act(DvFirstPicture::IdrOnly);
            assert_eq!(g.land(&landing(t)), Step::Withdraw(withdrawal(Some(t as u8), Cause::Row)), "type {t}");
        }
        // A start with no seek armed has no target to carry.
        let mut plain = Guard::armed(Mode::Act, DvFirstPicture::IdrOnly, None);
        let l = Landing { kf_pts: KF, key: CRA, in_place: false };
        let w = Withdrawal { kf_pts: KF, nal: Some(21), armed_target: None, in_place: false, cause: Cause::Row };
        assert_eq!(plain.land(&l), Step::Withdraw(w));
    }

    #[test]
    fn dv_start_an_idr_landing_on_an_idr_only_set_is_fed_and_watched() {
        let mut g = act(DvFirstPicture::IdrOnly);
        assert_eq!(g.land(&landing(IDR)), Step::Quiet);
        assert!(!g.withdrawal_pending(), "an IDR is what the splitter takes");
        // ... and the watch still backstops it: nothing in the row says the picture will come.
        assert_eq!(fired(&mut g, &[0, 1_000, 3_999]), []);
        assert_eq!(g.step(&probe(4_000)), Step::Withdraw(withdrawal(None, silent(4_000))));
    }

    #[test]
    fn dv_start_with_no_row_a_cra_landing_is_fed_and_watched() {
        let mut g = act(DvFirstPicture::AnyIrap);
        assert_eq!(g.land(&landing(CRA)), Step::Quiet, "no row, no prediction");
        assert!(!g.withdrawal_pending());
        assert_eq!(fired(&mut g, &[0, 3_999]), []);
        assert_eq!(g.step(&probe(4_000)), Step::Withdraw(withdrawal(Some(21), silent(4_000))));
    }

    #[test]
    fn dv_start_the_watch_fires_once_at_4000_ms() {
        let mut g = act(DvFirstPicture::AnyIrap);
        g.land(&landing(IDR));
        assert_eq!(DV_FIRST_PICTURE_MS, 4_000);
        let all: Vec<u32> = (0..=12).map(|i| i * 1_000).collect();
        assert_eq!(fired(&mut g, &all), [(4_000, Step::Withdraw(withdrawal(None, silent(4_000))))]);
        assert!(!g.withdrawal_pending(), "a watch withdrawal is handed over by value, not held");
    }

    #[test]
    fn dv_start_a_position_update_ends_the_watch_for_the_landing() {
        let mut g = act(DvFirstPicture::AnyIrap);
        g.land(&landing(IDR));
        assert_eq!(fired(&mut g, &[0, 2_000, 3_999]), []);
        let update = |now| Probe { frames: 1, ..probe(now) };
        for t in [3_999, 4_000, 20_000] {
            assert_eq!(g.step(&update(t)), Step::Quiet, "frames > 0 at {t} ms");
        }
        // It stays ended even if the counter reads zero again before the next landing.
        assert_eq!(fired(&mut g, &[21_000, 30_000]), []);
    }

    #[test]
    fn dv_start_a_shut_gate_or_a_paused_viewer_never_trips_the_watch() {
        for shut in [
            (|p: Probe| Probe { feed_open: false, ..p }) as fn(Probe) -> Probe,
            |p| Probe { clock_running: false, ..p },
        ] {
            let mut g = act(DvFirstPicture::AnyIrap);
            g.land(&landing(IDR));
            for t in (0..=10_000).step_by(500) {
                assert_eq!(g.step(&shut(probe(t))), Step::Quiet, "{t} ms with the gate shut");
            }
            // The clock counts from the reopen, not from the first tick: 10 s of shut gate is not 4 s of silence.
            assert_eq!(g.step(&probe(10_500)), Step::Quiet);
            assert_eq!(g.step(&probe(14_499)), Step::Quiet);
            assert_eq!(g.step(&probe(14_500)), Step::Withdraw(withdrawal(None, silent(4_000))));
        }
    }

    #[test]
    fn dv_start_an_under_fed_landing_never_trips_the_watch() {
        let mut g = act(DvFirstPicture::AnyIrap);
        g.land(&landing(IDR));
        let thin = |now| Probe { accepted_ns: ACCEPTED_MIN_NS - 1, ..probe(now) };
        for t in (0..=10_000).step_by(1_000) {
            assert_eq!(g.step(&thin(t)), Step::Quiet, "{t} ms with 699 ms accepted");
        }
        // Counted from the moment 700 ms was accepted.
        assert_eq!(g.step(&Probe { accepted_ns: ACCEPTED_MIN_NS, ..probe(10_500) }), Step::Quiet);
        assert!(matches!(g.step(&probe(14_500)), Step::Withdraw(_)));
    }

    #[test]
    fn dv_start_an_unfed_landing_or_an_unreleased_prime_never_trips_the_watch() {
        // `rebase_pending`: the landing keyframe has not been fed (a flushed Engine waiting for its landing).
        let mut g = act(DvFirstPicture::AnyIrap);
        g.land(&landing(IDR));
        for t in (0..=10_000).step_by(1_000) {
            assert_eq!(g.step(&Probe { landed: false, ..probe(t) }), Step::Quiet, "landed=false at {t} ms");
        }
        // The prime has not released Play: only `native_stall` watches that window.
        let mut g = act(DvFirstPicture::AnyIrap);
        g.land(&landing(IDR));
        for t in (0..=10_000).step_by(1_000) {
            assert_eq!(g.step(&Probe { prime_released: false, ..probe(t) }), Step::Quiet, "prime at {t} ms");
        }
    }

    #[test]
    fn dv_start_a_report_only_guard_reports_and_never_withdraws() {
        let mut g = Guard::armed(Mode::ReportOnly, DvFirstPicture::IdrOnly, None);
        let l = Landing { kf_pts: KF, key: CRA, in_place: false };
        let landing_report = Report { kf_pts: KF, nal: Some(21), row: true, cause: Cause::Row };
        assert_eq!(g.land(&l), Step::Report(landing_report), "the landing is reported once");
        assert!(!g.withdrawal_pending());
        // Its watch reports once if no position update follows, and never acts.
        let report = Report { kf_pts: KF, nal: Some(21), row: true, cause: silent(4_000) };
        assert_eq!(fired(&mut g, &[0, 1_000, 3_999, 4_000, 5_000, 20_000]), [(4_000, Step::Report(report))]);
        assert!(!g.withdrawal_pending() && g.take_withdrawal().is_none());
        // With no row the landing is not reported (nothing predicts a skip), and the watch still is.
        let mut g = Guard::armed(Mode::ReportOnly, DvFirstPicture::AnyIrap, None);
        assert_eq!(g.land(&l), Step::Quiet);
        let report = Report { kf_pts: KF, nal: Some(21), row: false, cause: silent(4_000) };
        assert_eq!(fired(&mut g, &[0, 4_000, 9_000]), [(4_000, Step::Report(report))]);
    }

    #[test]
    fn dv_start_an_inert_guard_never_does_anything() {
        let mut g = Guard::inert();
        assert_eq!(g.mode(), Mode::Inert);
        assert_eq!(g.land(&landing(CRA)), Step::Quiet);
        assert!(!g.withdrawal_pending());
        let all: Vec<u32> = (0..=20).map(|i| i * 1_000).collect();
        assert_eq!(fired(&mut g, &all), []);
        // Armed in `Inert` mode (a play with no node) is the same.
        let mut g = Guard::armed(Mode::Inert, DvFirstPicture::IdrOnly, Some(TARGET));
        assert_eq!(g.land(&landing(CRA)), Step::Quiet);
        assert_eq!(fired(&mut g, &all), []);
    }

    #[test]
    fn dv_start_a_re_arm_starts_a_fresh_landing() {
        let mut g = act(DvFirstPicture::AnyIrap);
        g.land(&landing(IDR));
        assert!(matches!(g.step(&probe(4_000)), Step::Withdraw(_)), "the first landing fired");
        assert_eq!(fired(&mut g, &[5_000, 9_000]), [], "and is spent");
        // A new in-place seek: its own target, its own clock, its own single shot.
        let second = 98_000_000_000;
        g.rearm(Some(second));
        assert_eq!(fired(&mut g, &[10_000, 20_000]), [], "no landing yet, so no watch");
        g.land(&Landing { kf_pts: KF + 1, key: CRA, in_place: true });
        assert_eq!(fired(&mut g, &[30_000, 33_999]), []);
        let w = Withdrawal { kf_pts: KF + 1, nal: Some(21), armed_target: Some(second), in_place: true, cause: silent(4_000) };
        assert_eq!(g.step(&probe(34_000)), Step::Withdraw(w));
        // A re-arm also drops a withdrawal nobody took.
        let mut g = act(DvFirstPicture::IdrOnly);
        g.land(&landing(CRA));
        assert!(g.withdrawal_pending());
        g.rearm(None);
        assert!(!g.withdrawal_pending());
    }
}
