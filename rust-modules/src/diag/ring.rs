//! The bounded diagnostic ring — **the log this app already writes, kept in memory**.
//!
//! There is no second logging system here and no new call site anywhere: `crate::log` is the ONE
//! sink every diagnostic line in the app passes through, and this taps it one line below
//! `redact_tokens`. So the ring is by construction a strict subset of `plxnative-events.log`, with
//! the shipped credential backstop already applied, and a module that starts logging tomorrow is
//! in the snapshot with no change here.
//!
//! # Two caps, not one
//!
//! A record count alone is not a bound: one `feed` line is ~40 bytes and one Load payload line can
//! be several hundred, so 4000 records is anywhere between 160 KB and megabytes. Both caps are
//! enforced on every insert and the older half of the ring is evicted until both hold. The device
//! declares a `requiredMemory` budget in `appinfo.json` and shares one `/tmp`-less jail with the
//! video pipeline; an unbounded debug buffer is exactly the sort of thing that turns a reproducible
//! bug into an unreproducible OOM.
//!
//! **Eviction is counted and reported.** `dropped` rides in the envelope, because "the window was
//! too small" and "nothing happened before the bug" are the same silence otherwise — and the whole
//! promise of this feature is that pressing the button after reproducing a fault still captures
//! what led to it.
//!
//! # Cost on the paths it sits on
//!
//! One `Mutex` lock, one `String` allocation and a `VecDeque::push_back` per logged line. The log
//! is written a few times a second at most and never per frame (`crate::log`'s own note), and the
//! lock is never held across an I/O call: [`take`] clones the whole buffer out under the lock and
//! serialises outside it.
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Record ceiling. A few minutes of an ordinary session, and rather more of a settled screen —
/// which writes one heartbeat line a second.
pub(crate) const MAX_RECORDS: usize = 4000;
/// Byte ceiling over the record TEXT. 768 KiB compresses to well under the receiver's 4 MiB body
/// cap even in the pathological all-unique case.
pub(crate) const MAX_BYTES: usize = 768 * 1024;

#[derive(Clone)]
pub(crate) struct Rec {
    /// milliseconds since [`start_clock`] — the same monotonic base the heartbeat's own timestamps
    /// come from, so a record correlates with `loop=`/`fps=`/`pos=` lines by eye.
    pub t_ms: u32,
    pub msg: String,
}

#[derive(Default)]
struct Ring {
    recs: VecDeque<Rec>,
    bytes: usize,
    /// evicted since the last [`take`] — reported, never silently absorbed
    dropped: u64,
}

static RING: Mutex<Option<Ring>> = Mutex::new(None);
static START: OnceLock<Instant> = OnceLock::new();

/// Start the uptime clock. Called from `lab::boot`; idempotent, and [`t_ms`] starts it anyway if a
/// line is logged before boot reaches this module (several are).
pub(crate) fn start_clock() {
    let _ = START.get_or_init(Instant::now);
}

/// Milliseconds since process start, saturating at `u32::MAX` (49 days — no session is that long,
/// and a saturating cast is still an honest reading rather than a wrapped one).
pub(crate) fn t_ms() -> u32 {
    START.get_or_init(Instant::now).elapsed().as_millis().min(u32::MAX as u128) as u32
}

/// Append one already-redacted line.
///
/// Poison is stepped over rather than propagated: a panic in some other thread while it held this
/// lock must not turn every subsequent `crate::log` call into a second panic — the log is how the
/// first one gets diagnosed.
pub(crate) fn record(line: &str) {
    let t = t_ms();
    let mut g = RING.lock().unwrap_or_else(|e| e.into_inner());
    let r = g.get_or_insert_with(Ring::default);
    r.bytes += line.len();
    r.recs.push_back(Rec { t_ms: t, msg: line.to_string() });
    while r.recs.len() > MAX_RECORDS || r.bytes > MAX_BYTES {
        match r.recs.pop_front() {
            Some(old) => {
                r.bytes -= old.msg.len();
                r.dropped += 1;
            }
            None => break,
        }
    }
}

/// A consistent read of the whole ring, plus how many records were evicted since the last call.
///
/// **The ring is NOT cleared.** Two snapshots taken a minute apart deliberately overlap: a tester
/// pressing the button twice is asking "and what about now", not "show me only what is new", and
/// an upload that failed in transit must not have taken the evidence with it.
pub(crate) fn take() -> (Vec<Rec>, u64) {
    let mut g = RING.lock().unwrap_or_else(|e| e.into_inner());
    let Some(r) = g.as_mut() else { return (Vec::new(), 0) };
    let dropped = std::mem::take(&mut r.dropped);
    (r.recs.iter().cloned().collect(), dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests share one process-global ring, so they hold the crate lock and start from empty.
    fn reset() {
        let mut g = RING.lock().unwrap_or_else(|e| e.into_inner());
        *g = Some(Ring::default());
    }

    #[test]
    fn records_come_back_in_order_with_their_timestamps() {
        let _lk = crate::testlock::serial();
        reset();
        record("first");
        record("second");
        let (recs, dropped) = take();
        assert_eq!(recs.iter().map(|r| r.msg.as_str()).collect::<Vec<_>>(), ["first", "second"]);
        assert_eq!(dropped, 0);
        assert!(recs[1].t_ms >= recs[0].t_ms, "the clock is monotonic");
    }

    /// The RECORD cap evicts oldest-first and counts what it dropped.
    #[test]
    fn the_record_cap_evicts_the_oldest_and_counts_it() {
        let _lk = crate::testlock::serial();
        reset();
        for i in 0..MAX_RECORDS + 10 {
            record(&format!("line {i}"));
        }
        let (recs, dropped) = take();
        assert_eq!(recs.len(), MAX_RECORDS);
        assert_eq!(dropped, 10);
        assert_eq!(recs[0].msg, format!("line {}", 10), "the oldest ten went");
    }

    /// …and the BYTE cap does too, which a record count alone cannot express: 4000 records is
    /// anywhere from 160 KB to several megabytes depending on which lines they are.
    #[test]
    fn the_byte_cap_binds_before_the_record_cap_on_long_lines() {
        let _lk = crate::testlock::serial();
        reset();
        let fat = "x".repeat(8 * 1024);
        for _ in 0..200 {
            record(&fat);
        }
        let (recs, dropped) = take();
        assert!(recs.len() < MAX_RECORDS, "the byte cap bound first, not the record cap");
        assert!(recs.len() * fat.len() <= MAX_BYTES);
        assert!(dropped > 0, "and it said so");
    }

    /// `dropped` is a delta since the last read, and a read does NOT empty the ring — a second
    /// press must still see the history the first one uploaded.
    #[test]
    fn dropped_is_a_delta_and_a_take_does_not_drain() {
        let _lk = crate::testlock::serial();
        reset();
        for i in 0..MAX_RECORDS + 5 {
            record(&format!("{i}"));
        }
        assert_eq!(take().1, 5);
        assert_eq!(take().1, 0, "already reported");
        assert_eq!(take().0.len(), MAX_RECORDS, "still there for the next snapshot");
    }
}
