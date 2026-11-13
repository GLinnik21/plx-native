//! When the URI backend may publish `Observation::Presented`, as ONE pure machine.
//!
//! **The rule** (ACB binding only; with no ACB binding nothing ever feeds this machine a
//! [`Event::PlaneShown`], so it never fires). The first picture of an attempt is claimed once, when
//! all three hold, in this order of observation:
//!
//! 1. the service has said `playing` ([`Event::ServicePlaying`]);
//! 2. the plane owner has seen the firmware complete BOTH the full-screen window and the PLAYING
//!    `setState` with event 1, each matched on its own task id ([`Event::PlaneShown`], from
//!    `tv::plane::Shown::Done`);
//! 3. the service's clock then ADVANCES: a [`Event::Clock`] sample strictly above the sample before
//!    it, observed after both (1) and (2).
//!
//! **Why that and no less.** Each alone is a state report or a dispatch result: `playing` is the
//! service's word, a completion says the plane accepted a window and a play state, and a clock can
//! tick over a black panel. Together, in this order, they are necessary-condition evidence that the
//! plane was told to show the stream and the stream then moved. It is not photon proof (the same
//! grade as the Feed rule, `playback-contract`'s `Observation::Presented`), and nothing here reads
//! pixels.
//!
//! **An advance needs two samples.** A resumed start reports its resume position as the first
//! `currentTime`; with no sample to compare against, that first sample proves nothing and is only
//! remembered. Each sample is compared with the one before it (not a frozen baseline), whether or
//! not the gate was armed when that earlier one arrived, so the answer does not depend on which
//! order the three conditions were met in. A seek that lands before the first picture
//! ([`Event::Seeked`]) forgets the remembered sample: its first report is a new position, not
//! movement.
//!
//! **Shape.** Pure, like `acb_bind`: no clock, no global, no IO. An event goes in, [`Fx`] come out;
//! the backend feeds the events and turns the effect into the observation and its log line.

#![deny(clippy::wildcard_enum_match_arm)]

/// What the backend observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The service accepted the first Play and reported `playing`.
    ServicePlaying,
    /// The plane owner: the window and PLAYING calls both completed with event 1.
    PlaneShown,
    /// The plane owner: one of those completed with another event. No picture is claimed after it.
    PlaneFailed,
    /// The service's `currentTime`, in milliseconds.
    Clock(u64),
    /// A seek landed: the next sample is a new position, not an advance of the old one.
    Seeked,
    /// A new attempt starts, or this one is closing. The machine is as new.
    Reset,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Fx {
    /// The rule holds: `clock_ms` is the sample that advanced past `from_ms`, the one before it.
    /// Emitted at most once per reset.
    Presented { from_ms: u64, clock_ms: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// Neither condition is met.
    Neither,
    /// The service has played; the plane has not completed.
    ServiceOnly,
    /// The plane has completed; the service has not played.
    PlaneOnly,
    /// Both are met: the next advancing sample is the picture.
    Armed,
    Presented,
    /// The plane reported a failure: no picture is claimed on this attempt.
    Refused,
}

pub struct PictureGate {
    stage: Stage,
    /// The previous `currentTime` sample, if any (kept in every stage).
    last_ms: Option<u64>,
}

impl Default for PictureGate {
    fn default() -> Self {
        Self::new()
    }
}

impl PictureGate {
    pub const fn new() -> Self {
        Self { stage: Stage::Neither, last_ms: None }
    }

    /// The machine still needs the plane owner's verdict: the backend asks the owner only while
    /// this is true, so a plane that has answered (or failed) is not polled again.
    pub fn wants_plane(&self) -> bool {
        matches!(self.stage, Stage::Neither | Stage::ServiceOnly)
    }

    pub fn step(&mut self, event: Event, fx: &mut Vec<Fx>) {
        self.transition(event, fx);
    }
    fn transition(&mut self, event: Event, fx: &mut Vec<Fx>) {
        match event {
            Event::Reset => { self.stage = Stage::Neither; self.last_ms = None; },
            Event::Seeked => self.last_ms = None,
            Event::PlaneFailed => {
                if self.stage != Stage::Presented {
                    self.stage = Stage::Refused;
                }
            }
            Event::ServicePlaying => {
                self.stage = match self.stage {
                    Stage::Neither => Stage::ServiceOnly,
                    Stage::PlaneOnly => Stage::Armed,
                    other @ (Stage::ServiceOnly | Stage::Armed | Stage::Presented | Stage::Refused) => other,
                };
            }
            Event::PlaneShown => {
                self.stage = match self.stage {
                    Stage::Neither => Stage::PlaneOnly,
                    Stage::ServiceOnly => Stage::Armed,
                    other @ (Stage::PlaneOnly | Stage::Armed | Stage::Presented | Stage::Refused) => other,
                };
            }
            Event::Clock(ms) => {
                let previous = self.last_ms.filter(|previous| ms > *previous);
                self.last_ms = Some(ms);
                if let (Stage::Armed, Some(from_ms)) = (self.stage, previous) {
                    self.stage = Stage::Presented;
                    fx.push(Fx::Presented { from_ms, clock_ms: ms });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(gate: &mut PictureGate, events: &[Event]) -> Vec<Fx> {
        let mut fx = Vec::new();
        for event in events {
            gate.step(*event, &mut fx);
        }
        fx
    }
    fn presented(from_ms: u64, clock_ms: u64) -> Vec<Fx> { vec![Fx::Presented { from_ms, clock_ms }] }

    #[test]
    fn the_first_advance_after_playing_and_the_plane_is_the_picture_in_either_order() {
        for first in [[Event::ServicePlaying, Event::PlaneShown], [Event::PlaneShown, Event::ServicePlaying]] {
            let mut gate = PictureGate::new();
            assert!(feed(&mut gate, &[Event::Clock(1_000)]).is_empty());
            assert!(feed(&mut gate, &first).is_empty(), "arming is not the picture: {first:?}");
            assert_eq!(feed(&mut gate, &[Event::Clock(1_040)]), presented(1_000, 1_040), "{first:?}");
        }
    }

    #[test]
    fn it_fires_once_and_then_stays_quiet() {
        let mut gate = PictureGate::new();
        feed(&mut gate, &[Event::Clock(0), Event::ServicePlaying, Event::PlaneShown]);
        assert_eq!(feed(&mut gate, &[Event::Clock(40)]), presented(0, 40));
        assert!(feed(&mut gate, &[Event::Clock(80), Event::Clock(120), Event::ServicePlaying, Event::PlaneShown]).is_empty());
    }

    #[test]
    fn an_advance_needs_two_samples_so_a_resumed_start_is_not_one() {
        let mut gate = PictureGate::new();
        feed(&mut gate, &[Event::ServicePlaying, Event::PlaneShown]);
        // The first report of a resumed start is its resume position: nothing to compare with.
        assert!(feed(&mut gate, &[Event::Clock(600_000)]).is_empty());
        assert_eq!(feed(&mut gate, &[Event::Clock(600_040)]), presented(600_000, 600_040));
    }

    #[test]
    fn the_sample_before_arming_is_what_the_first_one_after_it_is_compared_with() {
        let mut gate = PictureGate::new();
        feed(&mut gate, &[Event::Clock(600_000), Event::ServicePlaying, Event::PlaneShown]);
        assert_eq!(feed(&mut gate, &[Event::Clock(600_040)]), presented(600_000, 600_040));
    }

    #[test]
    fn a_clock_that_does_not_move_forward_is_not_an_advance() {
        let mut gate = PictureGate::new();
        feed(&mut gate, &[Event::ServicePlaying, Event::PlaneShown, Event::Clock(500)]);
        assert!(feed(&mut gate, &[Event::Clock(500), Event::Clock(400)]).is_empty(), "equal and backwards");
        assert_eq!(feed(&mut gate, &[Event::Clock(450)]), presented(400, 450), "forward of the previous sample");
    }

    #[test]
    fn neither_condition_alone_is_enough_while_the_other_is_the_positive_leg() {
        // The service plays and the clock runs, but the plane never completes.
        let mut gate = PictureGate::new();
        assert!(feed(&mut gate, &[Event::ServicePlaying, Event::Clock(0), Event::Clock(40), Event::Clock(80)]).is_empty());
        assert_eq!(feed(&mut gate, &[Event::PlaneShown, Event::Clock(120)]), presented(80, 120));
        // The plane completes and the clock runs, but the service never says playing.
        let mut gate = PictureGate::new();
        assert!(feed(&mut gate, &[Event::PlaneShown, Event::Clock(0), Event::Clock(40), Event::Clock(80)]).is_empty());
        assert_eq!(feed(&mut gate, &[Event::ServicePlaying, Event::Clock(120)]), presented(80, 120));
    }

    #[test]
    fn a_plane_failure_ends_the_claim_until_a_reset() {
        for before in [vec![], vec![Event::ServicePlaying], vec![Event::PlaneShown]] {
            let mut gate = PictureGate::new();
            feed(&mut gate, &before);
            feed(&mut gate, &[Event::PlaneFailed]);
            assert!(!gate.wants_plane());
            let late = feed(&mut gate, &[Event::ServicePlaying, Event::PlaneShown,
                Event::Clock(0), Event::Clock(40), Event::Clock(80)]);
            assert!(late.is_empty(), "{before:?}");
        }
        let mut gate = PictureGate::new();
        feed(&mut gate, &[Event::PlaneFailed, Event::Reset]);
        assert!(gate.wants_plane());
        assert_eq!(feed(&mut gate, &[Event::ServicePlaying, Event::PlaneShown, Event::Clock(0), Event::Clock(40)]),
            presented(0, 40), "a new attempt starts clean");
    }

    #[test]
    fn a_failure_after_the_picture_does_not_take_it_back() {
        let mut gate = PictureGate::new();
        feed(&mut gate, &[Event::ServicePlaying, Event::PlaneShown, Event::Clock(0)]);
        assert_eq!(feed(&mut gate, &[Event::Clock(40)]), presented(0, 40));
        assert!(feed(&mut gate, &[Event::PlaneFailed, Event::Clock(80)]).is_empty());
    }

    #[test]
    fn a_seek_forgets_the_sample_it_jumps_from() {
        let mut gate = PictureGate::new();
        feed(&mut gate, &[Event::ServicePlaying, Event::PlaneShown, Event::Clock(100)]);
        // The seek's target looks like an enormous advance of the old position; it is not one.
        assert!(feed(&mut gate, &[Event::Seeked, Event::Clock(600_000)]).is_empty());
        assert_eq!(feed(&mut gate, &[Event::Clock(600_040)]), presented(600_000, 600_040));
    }

    #[test]
    fn a_reset_forgets_everything() {
        let mut gate = PictureGate::new();
        feed(&mut gate, &[Event::ServicePlaying, Event::PlaneShown, Event::Clock(0), Event::Clock(40)]);
        feed(&mut gate, &[Event::Reset]);
        assert!(gate.wants_plane());
        assert!(feed(&mut gate, &[Event::Clock(80), Event::Clock(120)]).is_empty(), "the old conditions are gone");
        assert_eq!(feed(&mut gate, &[Event::ServicePlaying, Event::PlaneShown, Event::Clock(160)]), presented(120, 160));
    }

    #[test]
    fn the_plane_is_asked_for_only_until_it_has_answered() {
        let mut gate = PictureGate::new();
        assert!(gate.wants_plane());
        feed(&mut gate, &[Event::ServicePlaying]);
        assert!(gate.wants_plane(), "playing alone: the plane has not answered");
        feed(&mut gate, &[Event::PlaneShown]);
        assert!(!gate.wants_plane());
        let mut gate = PictureGate::new();
        feed(&mut gate, &[Event::PlaneShown]);
        assert!(!gate.wants_plane(), "answered before the service played");
    }
}
