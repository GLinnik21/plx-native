//! **The spool file: one owner, and the races that owner exists to make impossible.**
//!
//! [`queue`](super::queue) is the pure half — framing, caps, acknowledgement, all of it bytes in
//! and records out. This is the impure half: which file, who may touch it, and in what order. The
//! split is the same one `queue`'s own doc argues for, and it is what lets the interesting failures
//! (a power cut mid-write, an append racing a compaction) be *tested* instead of reasoned about.
//!
//! # There is exactly ONE writer, because the obvious cheap fix is a worse bug
//!
//! Queuing an event used to be a read-modify-write of the whole file, on the frame loop, per event.
//! The obvious repair is to append instead — and appending, on its own, silently LOSES records:
//! the flush worker rewrites via temp+rename, so an appender that opened the file before the rename
//! writes into an inode nobody will ever read again, and the record disappears when the fd closes.
//! No error, no log, nothing to grep for.
//!
//! So every read, append and rewrite goes through [`LOCK`], and the flush's commit **re-reads the
//! file under that lock** rather than writing back the snapshot it sent from. Both halves are
//! needed and they fix different halves of the same race: the lock keeps an append from landing in
//! an orphaned inode, and the re-read keeps a record appended *during* the network send — which is
//! the whole point of a send being slow — from being erased by a commit that predates it.
//!
//! # The lock is not held across the network
//!
//! A flush can block for [`sender`](super::sender)'s whole timeout. Holding the mutex across that
//! would park the main thread on the next event for twelve seconds, which is the freeze this module
//! is trying to avoid, arriving by the door marked "correctness". The protocol is therefore three
//! steps — snapshot under the lock, send with it released, commit under it again — and
//! [`commit_retiring`] is what makes the third step safe.
//!
//! # One path per process
//!
//! The candidate list is a SEARCH ORDER, for [`crate::paths`]' reason: which of the two `/media`
//! directories is writable depends on the jail profile. But a search order applied independently to
//! reads and writes is a split brain — read the file that exists, write the first that accepts, and
//! on a set where those differ every record written is invisible to the next read. Resolved once,
//! cached, preferring a spool that already exists over one that merely could.

use super::queue::{self, Record};
use std::path::PathBuf;
use std::sync::Mutex;

/// The one owner. See the module doc: every read, append and rewrite is taken under this, and it is
/// never held across a network send.
static LOCK: Mutex<()> = Mutex::new(());

/// Take the lock, ignoring poisoning.
///
/// A poisoned mutex here means some earlier caller panicked while holding it, which for this module
/// means at worst a partially compacted spool — recoverable by construction, since a torn tail is
/// exactly what the framing is designed to survive. Refusing to queue telemetry for the rest of the
/// process because of it would turn a recoverable file into a permanent outage of the mechanism
/// whose entire job is to report that something went wrong.
fn lock() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where the spool lives, resolved once. `None` if nowhere is writable, which is a real outcome on
/// a jail profile we have not met yet and must degrade to "queue nothing" rather than to a panic.
fn path() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(p) = test_path() {
        return Some(p);
    }
    static PATH: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    PATH.get_or_init(resolve).clone()
}

/// Prefer a spool that already exists — a reinstall or a jail change can move which candidate is
/// writable, and picking a fresh empty file over one holding a crash report is the one ordering
/// that loses the report this whole module exists to keep.
fn resolve() -> Option<PathBuf> {
    let cands = crate::paths::telemetry_spool_candidates();
    if let Some(p) = cands.iter().find(|p| p.exists()) {
        return Some(p.clone());
    }
    // Nothing yet: take the first candidate whose directory will accept a write. Probing by writing
    // is the only honest test — `/media/internal` exists on a set where it is not writable by us.
    cands.into_iter().find(|p| crate::plex::session::write_atomic(p, b""))
}

/// Every record on disk, oldest first. A missing file is an empty queue — that is what a first boot
/// looks like, not an error.
pub(crate) fn read() -> Vec<Record> {
    let _g = lock();
    read_locked()
}

fn read_locked() -> Vec<Record> {
    let Some(p) = path() else { return Vec::new() };
    let Ok(bytes) = std::fs::read(&p) else { return Vec::new() };
    let d = queue::decode_all(&bytes);
    if d.dropped_bytes > 0 {
        // Expected after a power cut, and worth one line either way: a non-zero count after a CLEAN
        // shutdown means something worse than a torn write.
        crate::log(&format!(
            "telemetry: spool recovered {} records, {} bytes discarded",
            d.records.len(),
            d.dropped_bytes
        ));
    }
    d.records
}

/// How many records are on disk, as far as this process knows — the count kept by the last
/// compaction plus every append since. [`UNKNOWN`] until this process has compacted once, which is
/// what makes the first append of a boot read the file it inherited rather than trusting a counter
/// that knows nothing about the last run.
static ON_DISK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(UNKNOWN);
const UNKNOWN: usize = usize::MAX;

/// Add one record.
///
/// **One `write(2)` in the ordinary case**, and no `fsync`. This is called from the frame loop, and
/// what it has to survive is the process dying — a SAM kill, a SIGSEGV, somebody pulling the plug
/// on a frozen picture — which the write alone already does, because the record is in the page
/// cache and the kernel outlives the process. `fsync` buys durability across a POWER cut, and
/// paying a synchronous disk round trip on every route change to narrow that window is the wrong
/// trade for this data. The compaction below syncs, and so does every flush.
///
/// It used to be a read-modify-write of the whole spool, per event, on that same thread.
pub(crate) fn append(r: &Record) -> bool {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let _g = lock();
    let Some(p) = path() else { return false };
    let Some(frame) = queue::encode(r) else {
        // Dropped where there is a caller to blame, rather than becoming a frame no reader accepts.
        crate::log("telemetry: record over the per-record cap, dropped");
        return false;
    };

    if needs_compaction(&p, frame.len()) {
        let mut all = read_locked();
        all.push(r.clone());
        return write_locked(&all);
    }

    // 0600 on create for the reason `write_atomic` does it: `/tmp` on this television is 1777 and
    // shared with every other app on the set. An existing file keeps its own mode, which the
    // compaction path — the only thing that creates this file fresh — has already set.
    let opened = std::fs::OpenOptions::new().append(true).create(true).mode(0o600).open(&p);
    let Ok(mut f) = opened else {
        crate::log("telemetry: could not open the spool for append");
        return false;
    };
    if f.write_all(&frame).is_err() {
        return false;
    }
    ON_DISK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    true
}

/// Whether this append has to go the slow way. Three reasons, and the first is the one that is easy
/// to leave out: a counter says nothing about the file this process INHERITED, so the first append
/// of every boot compacts and thereby learns what is actually there.
fn needs_compaction(p: &std::path::Path, incoming: usize) -> bool {
    let on_disk = ON_DISK.load(std::sync::atomic::Ordering::Relaxed);
    if on_disk == UNKNOWN || on_disk >= queue::MAX_RECORDS {
        return true;
    }
    let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0) as usize;
    len.saturating_add(incoming) > queue::MAX_BYTES
}

/// Replace the file with `records`, trimmed to the caps.
fn write_locked(records: &[Record]) -> bool {
    let Some(p) = path() else { return false };
    let (kept, dropped) = queue::trim(records.to_vec());
    if dropped > 0 {
        // Never silent. A queue that discards without saying so is a queue whose numbers are wrong
        // in a direction nobody can see.
        crate::log(&format!("telemetry: spool over cap, dropped {dropped} oldest records"));
    }
    let bytes: Vec<u8> = kept.iter().filter_map(queue::encode).flatten().collect();
    let ok = crate::plex::session::write_atomic(&p, &bytes);
    if ok {
        ON_DISK.store(kept.len(), std::sync::atomic::Ordering::Relaxed);
    }
    ok
}

/// Drop the records a flush finished with, keeping everything else — **including whatever was
/// queued while that flush was on the network.**
///
/// It takes only the acknowledged ids and **re-reads the file** rather than being handed the
/// snapshot the flush sent from, and that is the entire fix. A send is slow by construction (a
/// socket with [`sender`](super::sender)'s twelve-second ceiling), so the window between snapshot
/// and commit is exactly when the app is most likely to queue something — a route change, a fault
/// record from the crash channel. Writing the snapshot back minus what was accepted erases every
/// one of them, silently: the file is well formed, the framing intact, the caps held, the sender's
/// accounting correct. Only the count is wrong, and only against a number nobody has.
pub(crate) fn commit_retiring(retired: &[String]) {
    let _g = lock();
    let keep = queue::ack(read_locked(), retired);
    if !write_locked(&keep) {
        crate::log("telemetry: could not persist the spool to ANY candidate path");
    }
}

/// **Destroy every record belonging to a category that is now off.**
///
/// Called the moment a decision is recorded, not left to the flush. The flush does retire a record
/// whose category no longer has consent — `sender::allowed` asks per record — but a flush only runs
/// in a build that HAS an endpoint and only when something spawns it, so in a release with no
/// PostHog key, or on a set that never reaches the internet, a withdrawal would leave what it
/// withdrew sitting on the disk indefinitely. A withdrawal has to be an act, not a policy applied
/// at some later send.
///
/// Per category, never wholesale: the two switches are independent, and turning off usage must not
/// discard crash reports somebody is still consenting to.
pub(crate) fn purge_withdrawn(c: &super::consent::Consent) {
    let _g = lock();
    let mut all = read_locked();
    let before = all.len();
    if !c.errors {
        all = queue::purge(all, queue::Category::Errors);
    }
    if !c.usage {
        all = queue::purge(all, queue::Category::Usage);
    }
    if all.len() != before {
        crate::log(&format!("telemetry: withdrawal purged {} queued records", before - all.len()));
        write_locked(&all);
    }
}

#[cfg(test)]
static TEST_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

#[cfg(test)]
fn test_path() -> Option<PathBuf> {
    TEST_PATH.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Point this process's spool at `p` for the duration of a test.
///
/// There is one spool per process by design, so without this every test in the suite would share
/// one file under the build directory — the cross-test pollution `crate::testlock` exists for,
/// arriving by a path nobody would think to grep. Callers hold [`crate::testlock::serial`].
#[cfg(test)]
pub(crate) fn set_test_path(p: Option<PathBuf>) {
    *TEST_PATH.lock().unwrap_or_else(|e| e.into_inner()) = p;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::queue::{Category, Dest};

    /// A scratch spool path, unique per test, removed on drop. Everything here holds
    /// [`crate::testlock::serial`] as well — the path override, `LOCK` and the spool file itself
    /// are all process-global, so two of these running at once would grade each other's file.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("plx-spool-{}-{tag}.bin", std::process::id()));
            let _ = std::fs::remove_file(&p);
            set_test_path(Some(p.clone()));
            // `ON_DISK` is a per-PROCESS count, which is sound in production because `path()` is a
            // `OnceLock` and the file never moves. A test moves it, so the counter has to be
            // forgotten with it or the next test inherits a claim about a file that is gone.
            ON_DISK.store(UNKNOWN, std::sync::atomic::Ordering::Relaxed);
            Scratch(p)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            set_test_path(None);
        }
    }

    fn rec(id: &str) -> Record {
        Record {
            category: Category::Usage,
            dest: Dest::PostHog,
            event_id: id.to_string(),
            body: b"{}".to_vec(),
        }
    }

    fn ids() -> Vec<String> {
        read().into_iter().map(|r| r.event_id).collect()
    }

    /// **The record queued while a flush was on the network must survive that flush's commit.**
    ///
    /// This is the whole reason the spool has one owner. The flush protocol is three steps —
    /// snapshot, send, commit — and the send is slow *by construction*: it is a socket with a
    /// twelve-second timeout. Anything the app does in that window (a route change, a crash) queues
    /// a record the snapshot has never seen, and a commit that writes the snapshot back minus what
    /// it sent erases it.
    ///
    /// Silent, and invisible to every other assertion: the file is well-formed, the framing is
    /// intact, the caps hold, the sender's accounting is correct. Only the count is wrong, and only
    /// against a number nobody has.
    #[test]
    fn a_record_queued_while_a_flush_was_sending_survives_the_commit() {
        let _g = crate::testlock::serial();
        let _s = Scratch::new("flushwindow");

        append(&rec("already-here"));

        // The flush takes its snapshot...
        let snapshot = read();
        assert_eq!(snapshot.len(), 1);

        // ...and while it is on the network, the app queues another.
        append(&rec("queued-mid-flush"));

        // The server accepted the one that was sent.
        commit_retiring(&["already-here".to_string()]);

        assert_eq!(
            ids(),
            vec!["queued-mid-flush".to_string()],
            "the record queued during the send was erased by the commit"
        );
    }

    /// **Parallel producers do not lose or duplicate.** The frame loop, the flush worker and (from
    /// the crash channel) a fault handler all queue; a read-modify-write with no owner drops
    /// whichever of two interleaved writers finished first.
    #[test]
    fn parallel_producers_yield_one_record_each() {
        let _g = crate::testlock::serial();
        let _s = Scratch::new("parallel");

        const N: usize = 24;
        std::thread::scope(|s| {
            for i in 0..N {
                s.spawn(move || append(&rec(&format!("r{i}"))));
            }
        });

        let mut got = ids();
        got.sort();
        let mut want: Vec<String> = (0..N).map(|i| format!("r{i}")).collect();
        want.sort();
        assert_eq!(got, want, "{} of {N} producers' records survived", got.len());
    }

    /// **A torn tail costs the tail and nothing else.** The framing's whole promise, asserted at
    /// the FILE level rather than over a byte slice: `queue`'s own tests grade `decode_all`, and
    /// this grades that the reader in front of it does not turn a survivable half-write into an
    /// empty queue.
    #[test]
    fn a_torn_frame_leaves_every_earlier_record_intact() {
        let _g = crate::testlock::serial();
        let s = Scratch::new("torn");

        append(&rec("first"));
        append(&rec("second"));

        // A write that stopped in the middle of framing a third.
        let mut bytes = std::fs::read(&s.0).expect("spool");
        let whole = queue::encode(&rec("third")).expect("frame");
        bytes.extend_from_slice(&whole[..whole.len() / 2]);
        std::fs::write(&s.0, &bytes).expect("torn write");

        assert_eq!(ids(), vec!["first".to_string(), "second".to_string()]);
    }

    /// **The cap holds with nothing draining it.** A television that cannot reach the internet for
    /// a week still queues on every route change, and the partition it writes to is 615 MB shared
    /// with every other app on the set. The cap is enforced on the way IN, not only by a flush that
    /// may never run.
    #[test]
    fn the_cap_applies_with_nothing_draining_the_spool() {
        let _g = crate::testlock::serial();
        let _s = Scratch::new("cap");

        for i in 0..(queue::MAX_RECORDS + 40) {
            append(&rec(&format!("e{i}")));
        }

        let got = read();
        assert!(
            got.len() <= queue::MAX_RECORDS,
            "{} records on disk, cap is {}",
            got.len(),
            queue::MAX_RECORDS
        );
        // Oldest-first, so the newest survive — a fresh report is worth more than a stale one.
        assert_eq!(got.last().map(|r| r.event_id.clone()), Some(format!("e{}", queue::MAX_RECORDS + 39)));
    }

    /// **A withdrawal destroys what it withdrew, and only that.** The two switches are independent,
    /// so turning off usage must not discard a crash report somebody is still consenting to send.
    ///
    /// It is asserted here rather than left to the flush because the flush is conditional: it needs
    /// a build with an endpoint and something to spawn it. A release with no PostHog key, or a
    /// television that never reaches the internet, would otherwise keep the withdrawn records
    /// indefinitely — the one outcome a withdrawal exists to prevent.
    #[test]
    fn a_withdrawal_purges_its_own_category_and_leaves_the_other() {
        let _g = crate::testlock::serial();
        let _s = Scratch::new("withdraw");

        append(&rec("a-usage"));
        append(&Record { category: Category::Errors, ..rec("a-crash") });

        let mut c = crate::telemetry::consent::Consent::default();
        c.errors = true; // still consented
        c.usage = false; // just withdrawn
        purge_withdrawn(&c);

        assert_eq!(ids(), vec!["a-crash".to_string()]);
    }

    /// **0600.** The spool holds no credential, but it holds what a person consented to send and
    /// nothing more — and `/tmp` on this television is world-readable and shared with every other
    /// app. The mode is the one property of this file that no other test would notice losing.
    #[test]
    fn the_spool_is_not_readable_by_other_users() {
        let _g = crate::testlock::serial();
        let s = Scratch::new("mode");
        append(&rec("one"));

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&s.0).expect("spool").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "spool mode is {mode:o}");
    }
}
