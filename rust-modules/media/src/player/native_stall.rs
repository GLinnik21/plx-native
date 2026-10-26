//! player::native_stall — the watchdog for a native surround audio feed the TV's decoder swallows
//! without a word, and the evidence line about the size of the packets that were fed.
//!
//! A pipeline that cannot take a native Opus track usually says so (`type=18 num=200`,
//! which `player::mod` turns into a learned refusal). The LG C1's 6-channel Opus decoder does not:
//! given a packet of 1536 B or more it accepts the Load, takes a handful of access units and then
//! never produces a picture or an error. [`Watch`] tells that stall from a slow server by what OUR
//! side holds: both AU queues have data (the demuxer is not the bottleneck) while the pipeline has
//! accepted nothing new for [`NATIVE_AUDIO_STALL_MS`]. The pump turns a [`Step::Fire`] into the same
//! learned refusal and recovery flight as the loud failure.
//!
//! Pure state, no clock and no globals read here: the pump hands in a [`Probe`], so every rule is a
//! unit test.

use std::sync::Mutex;

/// How long the pipeline may accept no access unit, with both queues holding data and no picture
/// shown, before a native surround audio feed counts as swallowed. A clean native play shows its
/// first picture within about a second of the Load returning.
pub(super) const NATIVE_AUDIO_STALL_MS: u32 = 4_000;

/// What the pump knows this tick.
pub(super) struct Probe {
    /// The pump's millisecond clock.
    pub now: u32,
    /// Channel count of the natively fed audio track; 0 when the audio is not fed natively.
    pub channels: u32,
    /// `sf_load` returned for this session's epoch.
    pub load_returned: bool,
    /// Pictures the pipeline has shown this session.
    pub frames: i32,
    /// Access units the pipeline has ACCEPTED this session (video and audio).
    pub accepted: i64,
    pub audio_queue_bytes: i64,
    pub video_queue_bytes: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Step {
    Quiet,
    /// First tick of a candidate stall; the pump logs it once.
    Armed,
    Fire {
        /// Milliseconds since the watch armed.
        armed_ms: u32,
        /// Milliseconds the accepted count has not moved.
        stalled_ms: u32,
        accepted: i64,
    },
}

#[derive(Default)]
pub(super) struct Watch {
    armed: Option<Armed>,
    /// Fired, or a picture appeared: this session is done with the watchdog.
    spent: bool,
}

struct Armed {
    at: u32,
    progress_at: u32,
    accepted: i64,
}

impl Watch {
    pub(super) const fn new() -> Watch {
        Watch { armed: None, spent: false }
    }

    pub(super) fn step(&mut self, p: &Probe) -> Step {
        if self.spent {
            return Step::Quiet;
        }
        if p.frames > 0 {
            self.spent = true;
            self.armed = None;
            return Step::Quiet;
        }
        // Stereo is proven fine natively, so it is never learned from a stall.
        let candidate = p.channels > 2 && p.load_returned && p.audio_queue_bytes > 0 && p.video_queue_bytes > 0;
        if !candidate {
            self.armed = None;
            return Step::Quiet;
        }
        match &mut self.armed {
            None => {
                self.armed = Some(Armed { at: p.now, progress_at: p.now, accepted: p.accepted });
                Step::Armed
            }
            Some(a) if a.accepted != p.accepted => {
                a.accepted = p.accepted;
                a.progress_at = p.now;
                Step::Quiet
            }
            Some(a) => {
                let stalled_ms = p.now.wrapping_sub(a.progress_at);
                if stalled_ms < NATIVE_AUDIO_STALL_MS {
                    return Step::Quiet;
                }
                let step = Step::Fire { armed_ms: p.now.wrapping_sub(a.at), stalled_ms, accepted: p.accepted };
                self.spent = true;
                self.armed = None;
                step
            }
        }
    }
}

/// The first `SIZE_SAMPLE` natively fed audio access units' sizes: evidence for the per-packet size
/// limit of the C1's Opus decoder, logged once per play.
const SIZE_SAMPLE: usize = 200;

#[derive(Default)]
pub(super) struct Sizes {
    seen: Vec<u32>,
    logged: bool,
}

impl Sizes {
    pub(super) const fn new() -> Sizes {
        Sizes { seen: Vec::new(), logged: false }
    }

    /// Still collecting.
    pub(super) fn open(&self) -> bool {
        !self.logged && self.seen.len() < SIZE_SAMPLE
    }

    /// Record one accepted AU; the summary when it is the sample's last.
    pub(super) fn record(&mut self, len: u32) -> Option<String> {
        if self.logged || self.seen.len() >= SIZE_SAMPLE {
            return None;
        }
        self.seen.push(len);
        if self.seen.len() < SIZE_SAMPLE {
            return None;
        }
        self.logged = true;
        self.summary()
    }

    /// `n=.. max=..B p95=..B`, of whatever was seen so far.
    pub(super) fn summary(&self) -> Option<String> {
        if self.seen.is_empty() {
            return None;
        }
        let mut v = self.seen.clone();
        v.sort_unstable();
        let p95 = v[(v.len() * 95 / 100).min(v.len() - 1)];
        Some(format!("n={} max={}B p95={}B", v.len(), v[v.len() - 1], p95))
    }
}

pub(super) static WATCH: Mutex<Watch> = Mutex::new(Watch::new());
pub(super) static SIZES: Mutex<Sizes> = Mutex::new(Sizes::new());

/// A new session: forget the last one's watch and sizes.
pub(super) fn reset() {
    *WATCH.lock().unwrap_or_else(|e| e.into_inner()) = Watch::new();
    *SIZES.lock().unwrap_or_else(|e| e.into_inner()) = Sizes::new();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(now: u32) -> Probe {
        Probe { now, channels: 6, load_returned: true, frames: 0, accepted: 8, audio_queue_bytes: 200_000, video_queue_bytes: 1_400_000 }
    }

    #[test]
    fn a_stalled_surround_feed_arms_then_fires_after_the_window() {
        let mut w = Watch::new();
        assert_eq!(w.step(&probe(1_000)), Step::Armed);
        assert_eq!(w.step(&probe(2_000)), Step::Quiet);
        assert_eq!(w.step(&probe(1_000 + NATIVE_AUDIO_STALL_MS - 1)), Step::Quiet);
        assert_eq!(
            w.step(&probe(1_000 + NATIVE_AUDIO_STALL_MS)),
            Step::Fire { armed_ms: NATIVE_AUDIO_STALL_MS, stalled_ms: NATIVE_AUDIO_STALL_MS, accepted: 8 }
        );
        assert_eq!(w.step(&probe(60_000)), Step::Quiet, "once");
    }

    #[test]
    fn feed_progress_restarts_the_window() {
        let mut w = Watch::new();
        w.step(&probe(0));
        let mut p = probe(3_900);
        p.accepted = 9;
        assert_eq!(w.step(&p), Step::Quiet);
        p.now = 7_000;
        assert_eq!(w.step(&p), Step::Quiet, "3.1 s since the last accepted AU");
        p.now = 7_900;
        assert!(matches!(w.step(&p), Step::Fire { stalled_ms: 4_000, .. }));
    }

    #[test]
    fn an_empty_queue_is_a_slow_server_not_a_stall() {
        let mut w = Watch::new();
        w.step(&probe(0));
        let mut p = probe(10_000);
        p.video_queue_bytes = 0;
        assert_eq!(w.step(&p), Step::Quiet);
        let mut p = probe(20_000);
        p.audio_queue_bytes = 0;
        assert_eq!(w.step(&p), Step::Quiet);
        // The window restarts when the queues fill again.
        assert_eq!(w.step(&probe(21_000)), Step::Armed);
        assert_eq!(w.step(&probe(24_000)), Step::Quiet);
    }

    #[test]
    fn it_never_fires_after_a_picture() {
        let mut w = Watch::new();
        w.step(&probe(0));
        let mut p = probe(10_000);
        p.frames = 1;
        assert_eq!(w.step(&p), Step::Quiet);
        p.frames = 0; // a seek zeroes the count; the watch stays spent for the session
        assert_eq!(w.step(&p), Step::Quiet);
    }

    #[test]
    fn stereo_and_non_native_audio_are_never_watched() {
        for channels in [0, 1, 2] {
            let mut w = Watch::new();
            let mut p = probe(0);
            p.channels = channels;
            assert_eq!(w.step(&p), Step::Quiet);
            p.now = 30_000;
            assert_eq!(w.step(&p), Step::Quiet, "{channels} channels");
        }
    }

    #[test]
    fn nothing_arms_before_the_load_returned() {
        let mut w = Watch::new();
        let mut p = probe(0);
        p.load_returned = false;
        assert_eq!(w.step(&p), Step::Quiet);
        p.now = 30_000;
        assert_eq!(w.step(&p), Step::Quiet);
    }

    #[test]
    fn sizes_report_max_and_p95_once() {
        let mut s = Sizes::new();
        assert!(s.summary().is_none());
        for i in 0..SIZE_SAMPLE - 1 {
            assert!(s.record(1_000 + i as u32).is_none());
        }
        let line = s.record(2_300).expect("the sample is full");
        assert_eq!(line, "n=200 max=2300B p95=1190B");
        assert!(s.record(5_000).is_none(), "once per play");
    }
}
