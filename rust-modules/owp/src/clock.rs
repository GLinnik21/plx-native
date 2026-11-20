#![deny(clippy::wildcard_enum_match_arm)]

use std::collections::{BTreeMap, VecDeque};

pub const CLOCK_WINDOW: usize = 8;
pub const LARGEST_RTT_MS: i64 = 1000;
pub const CLOCK_STEP_SLACK_MS: i64 = 100;
pub const FORGET_PING_AFTER_MS: i64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub offset_ms: i64,
    pub rtt_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockEvent {
    PingSent { id: i64, at_ms: i64 },
    PongReceived { id: i64, server_time: i64, at_ms: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PongOutcome {
    Accepted,
    Stepped,
    Ignored,
}

#[derive(Debug)]
pub struct ClockSync {
    sent: BTreeMap<i64, i64>,
    window: VecDeque<Sample>,
    best: Option<Sample>,
}

impl Default for ClockSync {
    fn default() -> ClockSync {
        ClockSync::new()
    }
}

fn steps(best: Option<Sample>, sample: Sample) -> bool {
    match best {
        Some(best) => (sample.offset_ms - best.offset_ms).abs() > (sample.rtt_ms + best.rtt_ms) / 2 + CLOCK_STEP_SLACK_MS,
        None => false,
    }
}

impl ClockSync {
    pub fn new() -> ClockSync {
        ClockSync { sent: BTreeMap::new(), window: VecDeque::new(), best: None }
    }

    pub fn best(&self) -> Option<Sample> {
        self.best
    }

    pub fn step(&mut self, event: ClockEvent, outcomes: &mut Vec<PongOutcome>) {
        self.transition(event, outcomes);
    }

    fn transition(&mut self, event: ClockEvent, outcomes: &mut Vec<PongOutcome>) {
        match event {
            ClockEvent::PingSent { id, at_ms } => {
                self.sent.retain(|_, sent_at| at_ms - *sent_at <= FORGET_PING_AFTER_MS);
                self.sent.insert(id, at_ms);
            }
            ClockEvent::PongReceived { id, server_time, at_ms } => {
                let Some(sent_at) = self.sent.remove(&id) else {
                    outcomes.push(PongOutcome::Ignored);
                    return;
                };
                let rtt_ms = at_ms - sent_at;
                if !(0..=LARGEST_RTT_MS).contains(&rtt_ms) {
                    outcomes.push(PongOutcome::Ignored);
                    return;
                }
                let sample = Sample { offset_ms: server_time - (sent_at + rtt_ms / 2), rtt_ms };
                let stepped = steps(self.best, sample);
                if stepped {
                    self.window.clear();
                }
                self.window.push_back(sample);
                if self.window.len() > CLOCK_WINDOW {
                    self.window.pop_front();
                }
                let mut best: Option<Sample> = None;
                for kept in self.window.iter() {
                    match best {
                        Some(current) if current.rtt_ms <= kept.rtt_ms => {}
                        Some(_) | None => best = Some(*kept),
                    }
                }
                self.best = best;
                outcomes.push(if stepped { PongOutcome::Stepped } else { PongOutcome::Accepted });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ping_unanswered_for_longer_than_the_forget_time_is_dropped() {
        let mut clock = ClockSync::new();
        let mut outcomes = Vec::new();
        clock.step(ClockEvent::PingSent { id: 1, at_ms: 0 }, &mut outcomes);
        clock.step(ClockEvent::PingSent { id: 2, at_ms: FORGET_PING_AFTER_MS }, &mut outcomes);
        assert_eq!(clock.sent.keys().copied().collect::<Vec<_>>(), [1, 2]);
        clock.step(ClockEvent::PingSent { id: 3, at_ms: FORGET_PING_AFTER_MS + 1 }, &mut outcomes);
        assert_eq!(clock.sent.keys().copied().collect::<Vec<_>>(), [2, 3]);
        clock.step(ClockEvent::PongReceived { id: 1, server_time: 0, at_ms: FORGET_PING_AFTER_MS + 2 }, &mut outcomes);
        assert_eq!(outcomes, [PongOutcome::Ignored]);
    }
}
