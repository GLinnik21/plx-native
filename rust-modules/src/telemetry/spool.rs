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
    resolve_from(crate::paths::telemetry_spool_candidates())
}

fn resolve_from(cands: Vec<PathBuf>) -> Option<PathBuf> {
    // Not `Path::exists()`: that follows symlinks and checks neither ownership nor file type, so a
    // peer-planted name at the first candidate would capture the spool for the whole process (every
    // later reader/writer refuses it on the owned-regular check, or blocks on it if it is a FIFO,
    // and this `OnceLock` never reconsiders). Select on the same owned-regular predicate the readers
    // use instead (review finding, 2026-09-10).
    if let Some(p) = cands
        .iter()
        .find(|p| crate::plex::session::read_owned_regular_trusted(p).is_some())
    {
        return Some(p.clone());
    }
    // Nothing yet: take the first candidate whose directory will accept a write. Probing by writing
    // is the only honest test — `/media/internal` exists on a set where it is not writable by us.
    cands
        .into_iter()
        .find(|p| crate::plex::session::write_atomic(p, b""))
}

/// Every record on disk, oldest first. A missing file is an empty queue — that is what a first boot
/// looks like, not an error.
pub(crate) fn read() -> Vec<Record> {
    let _g = lock();
    read_locked()
}

fn read_locked() -> Vec<Record> {
    let Some(p) = path() else { return Vec::new() };
    let Some((bytes, trust)) = crate::plex::session::read_owned_regular_trusted(&p) else {
        return Vec::new();
    };
    if !trust.content_trusted() {
        // Write-widened: another uid could have appended or replaced records, so nothing here is
        // provably ours to send. Count what is there only for the log, then truncate — never keep
        // a byte of it. `ON_DISK` is reset to 0 so the next append's fast path does not believe a
        // stale count.
        let discarded = queue::decode_all(&bytes).records.len();
        if discarded > 0 {
            crate::log(&format!(
                "telemetry: discarded {discarded} spool records after an untrusted mode"
            ));
        }
        let _ = crate::plex::session::write_atomic(&p, &[]);
        ON_DISK.store(0, std::sync::atomic::Ordering::Relaxed);
        return Vec::new();
    }
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
    let _g = lock();
    append_locked(r)
}

/// Add a record only if `allowed` is still true while the spool is exclusively owned. `None`
/// means consent refused the append; `Some(false)` is an actual spool failure. Withdrawal first
/// publishes the new decision and then takes this same lock to purge, so every race has one of two
/// safe orders: the record is refused, or it is appended first and the purge removes it.
pub(crate) fn append_if(r: &Record, allowed: impl FnOnce() -> bool) -> Option<bool> {
    let _g = lock();
    if !allowed() {
        return None;
    }
    Some(append_locked(r))
}

fn append_locked(r: &Record) -> bool {
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

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

    // `resolve`/compaction creates the file through `write_atomic`, so ordinary append only opens
    // an existing object. O_NOFOLLOW plus fstat closes the fixed-name symlink/TOCTOU path in the
    // shared runtime directory; the fd, rather than a second pathname lookup, is what is checked.
    // O_NONBLOCK: without it, a peer-planted FIFO at this fixed name blocks `open(2)` before the
    // `is_file()` check below ever runs, wedging the frame loop that calls `append`. No-op for a
    // genuine regular file on Linux (review finding, 2026-09-10; twin fix in `plex::session`).
    let opened = std::fs::OpenOptions::new()
        .append(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&p);
    let Ok(mut f) = opened else {
        crate::log("telemetry: could not open the spool for append");
        return false;
    };
    let Ok(meta) = f.metadata() else { return false };
    if !meta.file_type().is_file() || meta.uid() != unsafe { libc::geteuid() } {
        crate::log("telemetry: refused an unsafe spool file");
        return false;
    }
    // A widened MODE on a file that is still ours and still regular is repaired in place rather
    // than refused — see `session::repair_owned_mode`'s doc for why, and
    // `docs/measurements/credential-storage-native-apps-2026-09-10.md` for the device finding
    // that made the old refusal a silent, permanent telemetry outage. **The mode is always
    // repaired, but a write-widened file's CONTENT is never trusted** — another uid could have
    // appended a forged record — so that case truncates instead of appending onto it. This stays
    // the cheap "one write(2)" path in the ordinary (trusted) case: only an untrusted mode pays for
    // reading the file back, and that is a rare event, not the steady state this path is sized for.
    let trust = crate::plex::session::repair_owned_mode(&f, &meta, &p);
    if !trust.content_trusted() {
        use std::io::{Read, Seek, SeekFrom};
        let mut existing = Vec::new();
        let _ = f.seek(SeekFrom::Start(0));
        let _ = Read::by_ref(&mut f).take(4 * 1024 * 1024).read_to_end(&mut existing);
        let discarded = queue::decode_all(&existing).records.len();
        if discarded > 0 {
            crate::log(&format!(
                "telemetry: discarded {discarded} spool records after an untrusted mode"
            ));
        }
        return write_locked(std::slice::from_ref(r));
    }
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
        crate::log(&format!(
            "telemetry: spool over cap, dropped {dropped} oldest records"
        ));
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
///
/// **`Category::OneOff` is never named here, and that is deliberate, not an omission.** A one-off
/// record's consent was the single press that queued it, not either standing switch, so there is
/// no decision here for it to be withdrawn BY — see that variant's doc.
pub(crate) fn purge_withdrawn(c: &super::consent::Consent) -> bool {
    if c.errors && c.usage {
        return true;
    }
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
        crate::log(&format!(
            "telemetry: withdrawal purged {} queued records",
            before - all.len()
        ));
    }
    // Always rewrite and sync, even when the decoded queue was empty. An empty read is not proof
    // that no stale file exists at another candidate; the resolved canonical spool must carry the
    // cutoff before a new opt-in can become effective.
    write_locked(&all)
}

/// **Destroy EVERY queued record, `Category::OneOff` included.**
///
/// [`purge_withdrawn`] deliberately spares a one-off record — its consent was the single press
/// that queued it, not a standing switch a withdrawal can name — but sign-out and Delete all
/// local data are not a withdrawal of that press, they are an erasure of this television's local
/// data, and PRIVACY.md/`legal.rs`'s PRIVACY const both promise sign-out removes "any queued
/// report" and that Delete all local data "removes all of it". Called only from
/// `telemetry::forget_with_receipt()`, never from the ordinary consent-change path `purge_withdrawn` guards.
pub(crate) fn purge_all_local() -> bool {
    let _g = lock();
    let all = read_locked();
    let n = all.len();
    if write_locked(&Vec::new()) {
        if n != 0 {
            crate::log(&format!("telemetry: local erasure purged {n} queued records"));
        }
        true
    } else {
        false
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
///
/// **Also forgets [`ON_DISK`].** It is a per-PROCESS count that is sound in production because
/// `path()` is a `OnceLock` and the file never moves — but a test moves it, and a stale non-
/// `UNKNOWN` count from whichever spool test ran last makes the very next `append` skip
/// compaction and try to open a file that was never created at this new path (`append_locked`'s
/// fast path only opens, it never creates). Every test in this module already did this itself via
/// `Scratch::new`; centralising it here is what let the first test OUTSIDE this module
/// (`telemetry::tests::flush_drops_a_spooled_record_the_current_consent_no_longer_allows`) call
/// `set_test_path` directly and still see its append actually land.
#[cfg(test)]
pub(crate) fn set_test_path(p: Option<PathBuf>) {
    *TEST_PATH.lock().unwrap_or_else(|e| e.into_inner()) = p;
    ON_DISK.store(UNKNOWN, std::sync::atomic::Ordering::Relaxed);
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
            let p =
                std::env::temp_dir().join(format!("plx-spool-{}-{tag}.bin", std::process::id()));
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

    #[test]
    fn conditional_append_rechecks_permission_before_the_record_exists() {
        let _g = crate::testlock::serial();
        let _s = Scratch::new("conditional");

        assert_eq!(append_if(&rec("withdrawn"), || false), None);
        assert!(
            ids().is_empty(),
            "a withdrawn event reached the durable spool"
        );
        assert_eq!(append_if(&rec("allowed"), || true), Some(true));
        assert_eq!(ids(), vec!["allowed".to_string()]);
    }

    #[test]
    fn resolver_uses_state_after_external_failure_but_prefers_an_existing_state_spool() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let dir = std::env::temp_dir().join(format!("plx-spool-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let state_dir = dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let external = dir.join("missing-parent/telemetry-spool.bin");
        let state = state_dir.join("telemetry-spool.bin");

        assert_eq!(resolve_from(vec![external.clone(), state.clone()]), Some(state.clone()));
        assert!(!external.exists());
        assert_eq!(std::fs::metadata(&state).unwrap().permissions().mode() & 0o777, 0o600);

        // Once state exists, it outranks a merely writable earlier directory so queued records
        // cannot be stranded in the old file.
        std::fs::create_dir_all(external.parent().unwrap()).unwrap();
        assert_eq!(resolve_from(vec![external.clone(), state.clone()]), Some(state.clone()));
        assert!(!external.exists());
        let _ = std::fs::remove_dir_all(dir);
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
        assert_eq!(
            got,
            want,
            "{} of {N} producers' records survived",
            got.len()
        );
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

    #[test]
    fn a_precreated_spool_symlink_cannot_redirect_telemetry_bytes() {
        use std::os::unix::fs::symlink;
        let _g = crate::testlock::serial();
        let s = Scratch::new("symlink");
        let victim = s.0.with_extension("victim");
        std::fs::write(&victim, b"unchanged").unwrap();
        symlink(&victim, &s.0).unwrap();

        assert!(!append(&rec("secret-event")));
        assert_eq!(std::fs::read(&victim).unwrap(), b"unchanged");

        let _ = std::fs::remove_file(victim);
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
        assert_eq!(
            got.last().map(|r| r.event_id.clone()),
            Some(format!("e{}", queue::MAX_RECORDS + 39))
        );
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
        append(&Record {
            category: Category::Errors,
            ..rec("a-crash")
        });

        let mut c = crate::telemetry::consent::Consent::default();
        c.errors = true; // still consented
        c.usage = false; // just withdrawn
        purge_withdrawn(&c);

        assert_eq!(ids(), vec!["a-crash".to_string()]);
    }

    /// **A `OneOff` record is never purged by a withdrawal, even when BOTH standing switches are
    /// off** — its consent was the one press that queued it, not either switch, so there is no
    /// decision here to withdraw it by. See `Category::OneOff`'s doc and `purge_withdrawn`'s.
    #[test]
    fn a_withdrawal_never_touches_a_one_off_record() {
        let _g = crate::testlock::serial();
        let _s = Scratch::new("withdraw-oneoff");

        append(&Record {
            category: Category::OneOff,
            ..rec("signin-one-off")
        });
        append(&Record {
            category: Category::Errors,
            ..rec("a-crash")
        });

        let mut c = crate::telemetry::consent::Consent::default();
        c.errors = false; // withdrawn
        c.usage = false; // withdrawn
        purge_withdrawn(&c);

        assert_eq!(ids(), vec!["signin-one-off".to_string()]);
    }

    /// **Unlike a withdrawal, a LOCAL ERASURE (sign-out, Delete all local data) takes the `OneOff`
    /// record too.** `telemetry::forget_with_receipt()` calls this instead of `purge_withdrawn`, and the two
    /// must stay opposite: PRIVACY.md promises sign-out removes "any queued report" and Delete all
    /// local data "removes all of it", which a one-off record surviving either would contradict.
    #[test]
    fn a_local_erasure_purges_a_one_off_record_too() {
        let _g = crate::testlock::serial();
        let _s = Scratch::new("erase-oneoff");

        append(&Record {
            category: Category::OneOff,
            ..rec("signin-one-off")
        });
        append(&Record {
            category: Category::Errors,
            ..rec("a-crash")
        });

        purge_all_local();

        assert!(ids().is_empty());
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

    /// **A spool widened to 0777 after this process already knows its record count is REPAIRED,
    /// not refused.** Measured on the device (2026-09-10, `docs/measurements/credential-storage-
    /// native-apps-2026-09-10.md`): the debug install's spool was found at 0777 in the shared
    /// `/media/developer` namespace, and the pre-hardening `append_locked` refused every append to
    /// such a file forever, which is telemetry dying silently rather than loudly. Ownership (our
    /// uid, a regular file) is still the line that must never be crossed — only the MODE is
    /// self-healing.
    #[test]
    fn an_append_to_a_world_writable_spool_repairs_the_mode_and_discards_the_old_record() {
        let _g = crate::testlock::serial();
        let s = Scratch::new("repair");

        // First append: compaction runs (ON_DISK starts UNKNOWN every process), creating the file
        // fresh at 0600 and learning the on-disk count — the ordinary case.
        append(&rec("already-here"));

        // Something in the shared namespace widens the mode mid-session (a peer devmode app, or —
        // per the archaeology in the measurement doc above — an unexplained external actor; this
        // repo's own code has never written this file at anything but 0600). 0o777 carries WRITE
        // bits, so another uid could have rewritten the pre-existing record — its content is no
        // longer trustworthy, unlike the mode alone.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&s.0, std::fs::Permissions::from_mode(0o777)).unwrap();

        // A SECOND append takes the fast direct-append path (ON_DISK is now known), which is the
        // one that used to refuse outright on a bad mode, and later (pre-trust-policy) kept
        // whatever was already there across the repair.
        assert!(
            append(&rec("second")),
            "append refused a file this process owns — telemetry died silently"
        );

        assert_eq!(
            ids(),
            vec!["second".to_string()],
            "a write-widened mode makes the pre-existing record untrusted — it must be discarded, \
             not kept across the repair"
        );

        let mode = std::fs::metadata(&s.0).expect("spool").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the widened mode was not repaired: {mode:o}");
    }

    /// **The READ path has the same rule and had no test** (review finding, 2026-09-10). The two
    /// write-widened branches in this module are not one: the append side runs on the frame loop
    /// and the read side is what a FLUSH calls, and a flush is the only one of the two that can
    /// put a record on the network. A forged record reaching `read()` is therefore the worse half
    /// — bytes another uid could have written, sent under this television's own reporting
    /// identifier — and until now nothing graded it. `an_append_to_a_world_writable_spool_…`
    /// cannot: by the time its own `ids()` runs, the append has already repaired the mode to 0600,
    /// so the branch here is never entered.
    #[test]
    fn a_flush_read_of_a_world_writable_spool_discards_every_record_and_truncates() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let s = Scratch::new("read-repair");

        append(&rec("already-here"));
        // Nothing in this process writes after this point: the read path is on its own.
        std::fs::set_permissions(&s.0, std::fs::Permissions::from_mode(0o666)).unwrap();

        assert!(
            ids().is_empty(),
            "a flush must never pick up records another uid could have written"
        );
        assert_eq!(
            std::fs::read(&s.0).expect("spool"),
            Vec::<u8>::new(),
            "and not a byte of them is left to be read again by the next flush"
        );
        assert_eq!(
            ON_DISK.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "the append fast path must not go on believing a count from before the truncation"
        );
        let mode = std::fs::metadata(&s.0).expect("spool").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the widened mode was not repaired: {mode:o}");

        // And the spool is usable again straight away: this is a discard, not a permanent break.
        assert!(append(&rec("after")));
        assert_eq!(ids(), vec!["after".to_string()]);
    }

    /// The read-only-widened twin, and the reason the distinction is worth carrying on the read
    /// path too: `0o644` grants no write bit, so the records are still provably this process's own
    /// and a flush keeps them. Discarding here would throw away real reports over a disclosure
    /// problem the mode repair has already closed.
    #[test]
    fn a_flush_read_of_a_read_only_widened_spool_keeps_every_record() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let s = Scratch::new("read-repair-readable");

        append(&rec("already-here"));
        std::fs::set_permissions(&s.0, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert_eq!(ids(), vec!["already-here".to_string()]);
        let mode = std::fs::metadata(&s.0).expect("spool").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the widened mode was not repaired: {mode:o}");
    }

    #[test]
    fn an_append_to_a_read_only_widened_spool_repairs_the_mode_and_keeps_the_old_record() {
        let _g = crate::testlock::serial();
        let s = Scratch::new("repair-readable");

        append(&rec("already-here"));

        // 0644 widens group/other READ only — no write bit. The content is still provably ours.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&s.0, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert!(append(&rec("second")));

        assert_eq!(
            ids(),
            vec!["already-here".to_string(), "second".to_string()],
            "a read-only widened mode must not cost the pre-existing record"
        );

        let mode = std::fs::metadata(&s.0).expect("spool").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the widened mode was not repaired: {mode:o}");
    }
}
