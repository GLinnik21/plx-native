//! player::era_callbacks — the one diagnostic line for an era-chosen callback that never arrives.
//!
//! The pipeline's two frame counters sit above the point where libpf's callback numbering shifts by
//! two between webOS 4 and 5+ (46/47 against 48/49, [`super::sink_counter_kind`]). The app picks the
//! numbering from the firmware major, so a firmware that numbers them the other way is read as
//! "nothing arrived" and nothing says so. The displayed-frame counter is forwarded on the same
//! 200 ms libpf timer as the position tick (`engine.rs`, `PAYLOAD_H265`'s note), so a session that
//! has taken position ticks and never one displayed-frame callback is the symptom. The dropped
//! counter is not watched: libpf sends it only when non-zero.
//!
//! Pure state: the pump hands in a [`Probe`] of the session's counters, so every rule is a unit
//! test. One [`Watch`] lives on the Engine, so "once" is per session by construction.

use std::os::raw::c_int;

/// Position ticks (5 Hz, about 5 s of playback) a session must have taken, counted from the first
/// time the watch looks, before a missing displayed-frame callback is reported.
pub(super) const SINK_COUNTER_GRACE_TICKS: u32 = 25;

/// What the session's callbacks have been, this tick.
pub(super) struct Probe {
    /// Position ticks (`type=0`) this session.
    pub ticks: u32,
    /// Callbacks [`super::sink_counter_kind`] named as the displayed-frame counter this session.
    pub displayed: u32,
    /// Callbacks numbered 46 to 49 that it did NOT name for this major: the other era's numbers.
    pub other_numbering: u32,
    /// The firmware major the numbering was chosen from; 0 when it was unreadable.
    pub major: u32,
    /// The raw type [`super::sink_counter_kind`] reads as displayed for `major`.
    pub expected_type: c_int,
}

/// The facts the log line carries.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Missing {
    pub ticks: u32,
    pub major: u32,
    pub expected_type: c_int,
    pub other_numbering: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Step {
    Quiet,
    Missing(Missing),
}

#[derive(Default)]
pub(super) struct Watch {
    /// The session's tick count at the first look: the grace counts from there, so a counter that
    /// was not reset with this Engine cannot make the first look fire.
    first_ticks: Option<u32>,
    /// Reported, or the counter arrived: this session is done with the watch.
    spent: bool,
}

impl Watch {
    pub(super) const fn new() -> Watch {
        Watch { first_ticks: None, spent: false }
    }

    pub(super) fn step(&mut self, p: &Probe) -> Step {
        if self.spent {
            return Step::Quiet;
        }
        if p.displayed > 0 {
            self.spent = true;
            return Step::Quiet;
        }
        let first = *self.first_ticks.get_or_insert(p.ticks);
        let played = p.ticks.saturating_sub(first);
        if played < SINK_COUNTER_GRACE_TICKS {
            return Step::Quiet;
        }
        self.spent = true;
        Step::Missing(Missing {
            ticks: played,
            major: p.major,
            expected_type: p.expected_type,
            other_numbering: p.other_numbering,
        })
    }
}

/// The event-log line for [`Step::Missing`].
pub(super) fn line(m: &Missing) -> String {
    let major = if m.major == 0 {
        "unknown, read as before webOS 5".to_string()
    } else {
        m.major.to_string()
    };
    format!(
        "era callbacks: no displayed-frame counter (type={} for firmware major {major}) in {} position ticks; \
         {} callbacks numbered 46-49 that this numbering does not name",
        m.expected_type, m.ticks, m.other_numbering
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(ticks: u32) -> Probe {
        Probe { ticks, displayed: 0, other_numbering: 0, major: 4, expected_type: 47 }
    }

    #[test]
    fn a_played_session_with_no_displayed_counter_reports_once() {
        let mut w = Watch::new();
        assert_eq!(w.step(&probe(0)), Step::Quiet);
        assert_eq!(w.step(&probe(SINK_COUNTER_GRACE_TICKS - 1)), Step::Quiet);
        assert_eq!(
            w.step(&probe(SINK_COUNTER_GRACE_TICKS)),
            Step::Missing(Missing { ticks: SINK_COUNTER_GRACE_TICKS, major: 4, expected_type: 47, other_numbering: 0 })
        );
        assert_eq!(w.step(&probe(10 * SINK_COUNTER_GRACE_TICKS)), Step::Quiet, "once");
    }

    #[test]
    fn a_session_whose_counter_arrives_never_reports() {
        let mut w = Watch::new();
        assert_eq!(w.step(&probe(0)), Step::Quiet);
        let mut p = probe(3);
        p.displayed = 3;
        assert_eq!(w.step(&p), Step::Quiet);
        assert_eq!(w.step(&probe(1_000)), Step::Quiet, "it arrived once: the watch is spent");
    }

    #[test]
    fn a_session_that_never_played_reports_nothing() {
        let mut w = Watch::new();
        for _ in 0..1_000 {
            assert_eq!(w.step(&probe(0)), Step::Quiet, "no position tick, so nothing was played");
        }
    }

    #[test]
    fn the_grace_counts_from_the_first_look_not_from_a_stale_counter() {
        let mut w = Watch::new();
        assert_eq!(w.step(&probe(500)), Step::Quiet, "a counter already at 500 is not 500 ticks of THIS session");
        assert_eq!(w.step(&probe(500 + SINK_COUNTER_GRACE_TICKS - 1)), Step::Quiet);
        assert!(matches!(w.step(&probe(500 + SINK_COUNTER_GRACE_TICKS)), Step::Missing(_)));
    }

    #[test]
    fn the_line_names_the_expected_type_the_major_and_the_other_numbering() {
        let seen = Missing { ticks: 25, major: 6, expected_type: 49, other_numbering: 12 };
        let text = line(&seen);
        assert!(text.contains("type=49"), "{text}");
        assert!(text.contains("major 6"), "{text}");
        assert!(text.contains("12 callbacks numbered 46-49"), "{text}");
        let unknown = line(&Missing { major: 0, ..seen });
        assert!(unknown.contains("unknown, read as before webOS 5"), "{unknown}");
    }
}
