//! The product backend's `uri-trace:` stream (devtriggers only): every supported service event of
//! an attempt, as the probe's `uriprobe: t=<ms> ev=...` would have written it, so the harness reads
//! a production run with the graders it reads a probe run with. The words come from
//! [`event_text`](crate::event_text) (one fn for both); the stamp is milliseconds since the
//! attempt's start on the backend's injected clock. Pure: a release log never carries these lines
//! (a 1 Hz clock has no place there), and no payload, URL or identity reaches them, only the
//! whitelisted projection `parse_event` already built.
use crate::event_text::{event_text, unparsed_text, CLOCK_LOG_EVERY_MS};
use crate::uri_protocol::{Ev, Message};

/// What one attempt has already written, so `currentTime` is thinned the way the probe thins it:
/// the first report, the first move, then a moved position at most every [`CLOCK_LOG_EVERY_MS`].
pub(crate) struct Trace {
    started_ms: u64,
    last_time: Option<i64>,
    moved_once: bool,
    logged_ms: Option<u64>,
    unparsed_time_logged: bool,
}

impl Trace {
    pub(crate) fn new(started_ms: u64) -> Self {
        Self { started_ms, last_time: None, moved_once: false, logged_ms: None,
            unparsed_time_logged: false }
    }

    /// The lines for one message that arrived at `now_ms`, in event order.
    pub(crate) fn lines(&mut self, message: &Message, now_ms: u64) -> Vec<String> {
        let stamp = now_ms.saturating_sub(self.started_ms);
        let mut out = Vec::new();
        for seen in &message.seen {
            let first_report = if seen.ev == Ev::CurrentTime {
                match self.clock_report(message, now_ms) {
                    Some(first_report) => first_report,
                    None => continue,
                }
            } else {
                false
            };
            out.push(format!("uri-trace: t={stamp} {}", event_text(message, seen, first_report)));
        }
        if message.seen.is_empty() {
            if let Some(name) = &message.unknown {
                out.push(format!("uri-trace: t={stamp} {}", unparsed_text(name)));
            }
        }
        out
    }

    /// Whether this message's `currentTime` is written, and if so whether as a first report that
    /// has not moved. The same thinning as the probe's `current_time`.
    fn clock_report(&mut self, message: &Message, now_ms: u64) -> Option<bool> {
        let Some(value) = message.current_time.filter(|_| message.normalized.contains(&Ev::CurrentTime)) else {
            // Reported in no readable shape: said once, never again.
            return (!std::mem::replace(&mut self.unparsed_time_logged, true)).then_some(false);
        };
        let moved = self.last_time.is_some_and(|last| last != value);
        self.last_time = Some(value);
        let first_report = if moved {
            let first = !self.moved_once;
            self.moved_once = true;
            let due = self.logged_ms.is_none_or(|at| now_ms.saturating_sub(at) >= CLOCK_LOG_EVERY_MS);
            if !(first || due) { return None; }
            false
        } else if !self.moved_once && self.logged_ms.is_none() {
            true
        } else {
            return None;
        };
        self.logged_ms = Some(now_ms);
        Some(first_report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uri_protocol::parse_event;

    fn at(trace: &mut Trace, now_ms: u64, payload: &str) -> Vec<String> {
        trace.lines(&parse_event(payload), now_ms)
    }
    fn time(value: i64) -> String { format!(r#"{{"currentTime":{{"currentTime":{value},"mediaId":"_m1"}}}}"#) }

    #[test]
    fn events_are_stamped_from_the_attempts_start_in_the_probes_words() {
        let mut trace = Trace::new(1_000);
        assert_eq!(at(&mut trace, 1_250, r#"{"mediaId":"_m1","loadCompleted":true}"#),
            ["uri-trace: t=250 ev=loadCompleted"]);
        assert_eq!(at(&mut trace, 1_300, r#"{"playing":true}"#), ["uri-trace: t=300 ev=playing"]);
        // `remainingTime` rides on the bufferRange line, as the drain grader reads it.
        assert_eq!(at(&mut trace, 2_000, r#"{"mediaId":"_m1","bufferRange":{"beginTime":0,"endTime":58,"remainingTime":12,"percent":40}}"#),
            ["uri-trace: t=1000 ev=bufferRange beginTime=0 endTime=58 remainingTime=12 percent=40"]);
        assert_eq!(at(&mut trace, 2_100, r#"{"bufferingStart":true}"#), ["uri-trace: t=1100 ev=bufferingStart"]);
        assert_eq!(at(&mut trace, 2_200, r#"{"fooBar":{"uri":"x"}}"#), ["uri-trace: t=1200 ev=unknown (unparsed)"]);
    }

    #[test]
    fn the_clock_is_logged_when_it_first_moves_then_once_a_second_like_the_probe() {
        let mut trace = Trace::new(0);
        assert_eq!(at(&mut trace, 10, &time(40)), ["uri-trace: t=10 ev=currentTime value=40 (first report)"]);
        assert!(at(&mut trace, 20, &time(40)).is_empty(), "an unmoved repeat");
        assert_eq!(at(&mut trace, 30, &time(900)), ["uri-trace: t=30 ev=currentTime value=900"], "the first move");
        assert!(at(&mut trace, 600, &time(1500)).is_empty(), "moved, but under a second on");
        assert_eq!(at(&mut trace, 1_030, &time(2000)), ["uri-trace: t=1030 ev=currentTime value=2000"]);
    }

    #[test]
    fn an_unreadable_clock_is_said_once() {
        let mut trace = Trace::new(0);
        assert_eq!(at(&mut trace, 10, r#"{"currentTime":{"mediaId":"_m1"}}"#), ["uri-trace: t=10 ev=currentTime (unparsed)"]);
        assert!(at(&mut trace, 20, r#"{"currentTime":"soon"}"#).is_empty());
        assert_eq!(at(&mut trace, 30, &time(0)), ["uri-trace: t=30 ev=currentTime value=0 (first report)"]);
    }
}
