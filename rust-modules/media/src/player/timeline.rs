//! One playback attempt's periodic PMS reporter. The adapter owns this resource; the backend
//! publishes normalized content-time samples to it. The worker never reads SHARED, TX, FEED_SHARED, FEED_TX or an
//! Engine, and the existing route lease still rejects a retired or hybrid projection.
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::route::{RouteStartAttempt, TimelineLease};

const REPORT_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct TimelineSnapshot {
    pub state: plx_plex::plex::TimelineState,
    pub time_ms: i64,
    pub duration_ms: i64,
}

impl TimelineSnapshot {
    /// Backend clocks are nanoseconds. A missing duration is not a reportable sample.
    pub fn from_content_ns(time_ns: i64, duration_ns: i64, paused: bool) -> Option<Self> {
        (duration_ns > 0).then_some(Self {
            state: if paused { plx_plex::plex::TimelineState::Paused } else { plx_plex::plex::TimelineState::Playing },
            time_ms: time_ns / 1_000_000,
            duration_ms: duration_ns / 1_000_000,
        })
    }

    pub fn from_feed(readout: &super::readout::PlayerReadout, duration_ns: i64) -> Option<Self> {
        if readout.unconfirmed_seek_target().is_some() {
            return None;
        }
        Self::from_content_ns(readout.raw().playpos_ns, duration_ns, readout.paused())
    }
}

struct MailboxState {
    stopped: bool,
    latest: Option<TimelineSnapshot>,
}

/// A reporter's private stop and sample mailbox. Resetting a later Engine cannot clear it.
pub(super) struct ReportStop {
    state: Mutex<MailboxState>,
    changed: Condvar,
}

impl ReportStop {
    fn new() -> Arc<Self> {
        Arc::new(Self { state: Mutex::new(MailboxState { stopped: false, latest: None }), changed: Condvar::new() })
    }

    fn publish(&self, snapshot: Option<TimelineSnapshot>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.stopped { state.latest = snapshot; }
    }

    fn stop(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.stopped = true;
        self.changed.notify_all();
    }

    /// Consume the latest sample once per interval. `None` means stopped; `Some(None)`
    /// means no fresh evidence arrived, so a stalled pump cannot repeat an old playhead.
    fn wait_sample(&self, interval: Duration) -> Option<Option<TimelineSnapshot>> {
        let deadline = Instant::now() + interval;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if state.stopped { return None; }
            let now = Instant::now();
            if now >= deadline { return Some(state.latest.take()); }
            state = self.changed.wait_timeout(state, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
}

pub(super) struct TimelineRunner {
    attempt: RouteStartAttempt,
    stop: Arc<ReportStop>,
    thread: JoinHandle<()>,
}

impl TimelineRunner {
    pub fn start(attempt: RouteStartAttempt, lease: TimelineLease) -> Option<Self> {
        let stop = ReportStop::new();
        let worker = stop.clone();
        let thread = plx_base::task::spawn("timeline", move || timeline_thread(lease, worker))?;
        Some(Self { attempt, stop, thread })
    }

    pub fn attempt(&self) -> RouteStartAttempt { self.attempt }

    pub fn publish(&self, attempt: RouteStartAttempt, snapshot: Option<TimelineSnapshot>) {
        if self.attempt == attempt { self.stop.publish(snapshot); }
    }

    /// The handle goes to route::scrobble_stop on a real stop so final `stopped` is ordered
    /// after a possibly in-flight periodic POST, without joining on the frame thread.
    pub fn stop(self) -> JoinHandle<()> {
        self.stop.stop();
        self.thread
    }
}

fn timeline_thread(lease: TimelineLease, stop: Arc<ReportStop>) {
    loop {
        let Some(sample) = stop.wait_sample(REPORT_INTERVAL) else { return; };
        let Some(sample) = sample else { continue; };
        if !crate::route::report_timeline(&lease, sample.state, sample.time_ms, sample.duration_ms) {
            return;
        }
        super::log(&format!("timeline {} t={}s/{}s", sample.state.as_str(), sample.time_ms / 1000, sample.duration_ms / 1000));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seek_reports_no_progress_until_the_replacement_reports_its_position() {
        use super::super::readout::{PlayerReadout, ReadoutEvent};
        use plx_playback_contract::feed::FeedRaw;

        const TARGET: i64 = 2_118_000_000_000;
        const DURATION: i64 = 7_836_454_000_000;
        let mut readout = PlayerReadout::IDLE;
        readout.step(ReadoutEvent::SeekRequested { target_ns: TARGET, display_ns: TARGET });
        readout.step(ReadoutEvent::SeekTaken);
        readout.step(ReadoutEvent::FactsIngested(FeedRaw {
            playpos_ns: 9_467_000_000, frames: 3, ..FeedRaw::IDLE
        }));
        assert!(TimelineSnapshot::from_feed(&readout, DURATION).is_none());
        assert_eq!(readout.unconfirmed_seek_target(), Some(TARGET));

        readout.step(ReadoutEvent::SessionReset);
        readout.step(ReadoutEvent::ReloadSeek { target_ns: TARGET });
        readout.step(ReadoutEvent::FactsIngested(FeedRaw { playpos_ns: TARGET, ..FeedRaw::IDLE }));
        assert!(TimelineSnapshot::from_feed(&readout, DURATION).is_none(), "the seeded target is not a report");
        assert_eq!(readout.unconfirmed_seek_target(), Some(TARGET), "stopping here still saves the seek target");

        readout.step(ReadoutEvent::FactsIngested(FeedRaw {
            playpos_ns: TARGET - 5_000_000_000, frames: 1, ..FeedRaw::IDLE
        }));
        assert_eq!(readout.unconfirmed_seek_target(), None);
        assert_eq!(TimelineSnapshot::from_feed(&readout, DURATION).unwrap().time_ms, 2_113_000);
    }

    #[test]
    fn mailbox_keeps_the_latest_normalized_sample_and_stop_is_sticky() {
        let mail = ReportStop::new();
        let first = TimelineSnapshot::from_content_ns(4_000_000_000, 40_000_000_000, false).unwrap();
        let last = TimelineSnapshot::from_content_ns(9_000_000_000, 40_000_000_000, true).unwrap();
        mail.publish(Some(first));
        mail.publish(Some(last));
        assert!(mail.state.lock().unwrap().latest == Some(last));
        mail.publish(None);
        assert!(mail.state.lock().unwrap().latest.is_none(), "missing duration clears the prior sample");
        mail.publish(Some(last));
        assert!(mail.wait_sample(Duration::ZERO).is_some_and(|sample| sample == Some(last)));
        assert!(mail.wait_sample(Duration::ZERO).is_some_and(|sample| sample.is_none()),
            "no new backend evidence must not repeat the prior playhead");
        mail.publish(Some(last));
        mail.stop();
        mail.publish(Some(first));
        assert!(mail.state.lock().unwrap().latest == Some(last));
        assert!(mail.wait_sample(Duration::ZERO).is_none());
        assert!(TimelineSnapshot::from_content_ns(0, 0, false).is_none());
    }
}
