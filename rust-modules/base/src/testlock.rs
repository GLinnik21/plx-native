//! One lock for every test that touches a process-global.
//!
//! Some remaining async seams are process-wide by construction — metadata's `static mut CURRENT`, route's play
//! mailbox, the player's SHARED block — so tests in DIFFERENT modules contend on the same
//! state and `cargo test` threads them. A per-module mutex cannot see that: the season and
//! detail mailboxes are two test functions in one file, but the season generation also moves
//! under `pump_detail` (which calls `supersede_season`).
//!
//! Hold the guard for the whole test. Poison is stepped over so a failing test reports ITS
//! assertion instead of dragging every later one down with a poison panic.
//!
//! **The lock also RECORDS who holds it, and that half is what makes the rule enforceable.**
//! A mutex nobody is obliged to take is a convention, and a convention that is broken in one
//! test out of two thousand does not fail — it hands some *other* module's test a wiped store
//! at a rate low enough to read as flakiness. That is exactly how
//! `app::chrome`'s `four_libraries_on_two_servers_publish_two_type_destinations` failed: its
//! own `browse::reset()` + `seed_two_source_table_for_test()` are adjacent statements, so the
//! table could only have been emptied by another thread, and the only thread that can run
//! beside a lock holder is one that never took the lock.
//!
//! So [`serial`] publishes the holding thread in [`OWNER`], and [`assert_held`] — called from
//! every crate-global mutator a test can reach — turns "somebody wrote this without the lock"
//! from an intermittent failure in a bystander into a deterministic panic in the culprit,
//! naming the culprit. Add the call to any new global store; do not add a retry anywhere.
//!
//! **This SUPERSEDES the static allowlist approach spec §14/§15.2 used earlier in the
//! restructure (`ci/allow/statics-migration.txt` and friends) for the stores specifically.**
//! That DoD criterion scopes a bare `static mut` OUT of `screens`/`ui` engine code — it asks
//! "does a SCREEN still own process-wide state a second instance would corrupt", and an
//! allowlisted exception there is a debt with a name and a phase number. A store's data module
//! (`pms`, `metadata`, `search`) is a different question entirely: those remaining
//! compatibility stores keep process-wide statics **by design** — `docs/stores-as-machines.md`
//! §1 is explicit about that temporary shape. Browse, Person and ViewState have since moved to
//! per-`Bridge` physical owners, and their owner gates reject a restored process global. For
//! each remaining compatibility store, an allowlist entry would therefore be a permanent
//! fixture wearing a temporary label. What such a store genuinely owes is not "stop being
//! global" but "a test that mutates you holds the one lock every other
//! test mutating you also holds", which is exactly what [`assert_held`] enforces at every
//! mutator a test can reach, rather than at the `static` declaration site. Put differently: the
//! allowlist answers "is this global allowed to exist", the assertion answers "was this write to
//! it safe" — a store answers yes to the first question unconditionally, so only the second one
//! applies to it.
//!
//! **Phase 12 / D5 closed the coverage this claim depends on** (2026-09-10): each remaining
//! process-global store's `apply`/`run` funnel asserts (`stores::hubs::run`,
//! `stores::metadata::run` and `stores::search::run`), as does every
//! `_for_test` installer that touches shared state. Browse, Person and ViewState fixtures instead own
//! explicit stores; Browse's helpers cover source, pin,
//! table, item, letter and query seeds without selecting process Browse state. Helpers that
//! also touch the shared session or server registry assert the same lock. The remaining global
//! installers include `metadata.rs`
//! (`install_for_test`, `set_current_for_test`, `begin_detail_for_test`,
//! `land_detail_for_test`), `search.rs` (`publish_shelves_for_test`,
//! `debounce_elapsed_for_test`) and `pms.rs`'s own eleven sites — and so does every entry point of
//! the server registry a test can reach: `plex::servers::register_with_client_id` (and the
//! `register_lazy` seam it and its sibling test constructors share), `revoke_all`,
//! `set_current` and `reset_for_test`. A three-run `make check` plus a ten-run
//! `--test-threads 16` stress at this coverage level turned up no test that had been writing a
//! store without the lock — which says the existing call sites were already disciplined, not
//! that the assertion was unnecessary: it is what keeps that true as the suite grows, instead of
//! relying on every future PR remembering the convention on its own.
use std::sync::atomic::{AtomicU64, Ordering};

static GLOBALS: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// The [`ticket`] of the thread currently inside a [`serial`] guard, or [`NOBODY`].
static OWNER: AtomicU64 = AtomicU64::new(NOBODY);
const NOBODY: u64 = 0;

/// This thread's identity as a plain integer. `ThreadId` has no stable numeric projection on
/// the pinned toolchain, and the value only has to be unique and comparable, so it is minted
/// from a counter on first use and kept for the thread's life.
fn ticket() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    thread_local! {
        static MINE: u64 = NEXT.fetch_add(1, Ordering::Relaxed);
    }
    MINE.with(|mine| *mine)
}

/// The guard [`serial`] hands back. Its own `Drop` runs BEFORE its field's, so the owner is
/// cleared while the mutex is still held — no window in which a second thread has the lock and
/// this one still claims it.
pub struct Serial(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

impl Drop for Serial {
    fn drop(&mut self) {
        // Workers the test started (a flight, a season or detail fetch) finish before the lock is
        // released: one that outlived it would write the next holder's globals.
        crate::task::drain_workers_for_test();
        crate::storage_worker::drain_for_test();
        OWNER.store(NOBODY, Ordering::SeqCst);
    }
}

/// Take the lock, or PANIC if this thread already holds it.
///
/// The re-entrancy check is not a nicety. [`GLOBALS`] is a plain mutex, so a test that takes
/// the guard and then calls a helper which takes it again — `screens::detail`'s `install()` is
/// exactly such a helper, and two trailer-scrub tests did this on 2026-09-18 — does not fail.
/// It *hangs*, holding the one lock the whole suite queues on, so every other serial test in
/// the run wedges behind it at 0% CPU with no output and no failure to read. That cost an hour
/// of wall clock and was indistinguishable from a slow build from the outside. A deadlock and
/// a panic are the same bug; only one of them names itself.
pub fn serial() -> Serial {
    assert!(
        !held(),
        "testlock::serial() taken twice on one thread — the second take would deadlock the \
         whole suite. Hold the guard a helper (e.g. detail's `install`) already returned \
         instead of taking a second one."
    );
    let guard = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    OWNER.store(ticket(), Ordering::SeqCst);
    Serial(guard)
}

thread_local! {
    /// Set by [`adopt_current_thread`]; see its doc.
    static ADOPTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Treat the CALLING thread as if it too held the [`serial`] guard — for a test that
/// deliberately spawns a WORKER thread to race a real *production* lock (not this one)
/// against the thread that took `serial()`, e.g. proving that `plex::servers`' own `WRITE`
/// mutex serializes a repoint against an in-flight commit. That worker still touches the same
/// crate globals `serial()` exists to protect, so [`assert_held`] must not wave it through for
/// free — but it is not a bystander test either, so the ordinary per-thread [`held`] check
/// (correctly) refuses it.
///
/// Call this as the FIRST statement inside the spawned closure, and only while the spawning
/// thread's [`Serial`] guard is still alive for the rest of the adopted thread's work — nothing
/// here clears the flag early or checks that the spawner is still holding it, so an adopted
/// thread that outlives the guard's `Drop` (e.g. a leaked worker still running after the test
/// function has returned) would silently keep reporting `held() == true` with nobody actually
/// holding [`GLOBALS`], which is a quieter failure than the one this whole module exists to
/// catch. A thread that calls this NEVER needs to call [`serial`] itself — doing so would
/// deadlock against the spawning thread's still-held [`GLOBALS`] lock.
pub fn adopt_current_thread() {
    ADOPTED.with(|a| a.set(true));
}

/// Does THIS thread hold the lock (or was it explicitly [`adopt`](adopt_current_thread)ed by
/// one that does)? Not "is it held" — an UNADOPTED foreign holder is the failure.
pub fn held() -> bool {
    OWNER.load(Ordering::SeqCst) == ticket() || ADOPTED.with(|a| a.get())
}

/// Refuse a write to a crate global from a thread that does not hold the lock.
///
/// `what` names the store, because the panic is read by whoever wrote the offending test and
/// the useful half is "which global" — the thread name libtest prints already says which test.
#[track_caller]
pub fn assert_held(what: &str) {
    assert!(
        held(),
        "{what} was written without crate::testlock::serial(). It is a process global: \
         without the lock this write lands in the middle of some other module's test and \
         fails THAT one, intermittently. Take the guard for the whole test body (see \
         lib.rs::testlock)."
    );
}

#[cfg(test)]
mod tests {
    /// **Ownership is per THREAD, which is the whole discriminating property.**
    ///
    /// "Is the mutex locked" cannot answer this question: while a test holds the guard the
    /// mutex is locked for everybody, so a bystander thread asking that would be told yes and
    /// would go on to write the global it had no right to. The lock records WHO, and a second
    /// thread — the shape every one of these bugs has taken — is told no.
    #[test]
    fn a_second_thread_is_never_mistaken_for_the_holder() {
        assert!(!super::held(), "a thread that never took the guard holds nothing");
        let guard = super::serial();
        assert!(super::held());
        // …and while THIS thread holds it, another one still does not.
        assert!(
            !std::thread::spawn(super::held).join().expect("probe thread"),
            "a foreign thread must not inherit this one's claim"
        );
        drop(guard);
        assert!(!super::held(), "the claim ends with the guard, not after it");
    }

    /// A second take on one thread must PANIC, not block.
    ///
    /// Graded on a spawned thread so the failure mode under test cannot take the suite's own
    /// lock down with it, and because that is the shape the bug has: a test takes the guard,
    /// then calls a helper that takes it again. Without this assertion the inner take blocks
    /// forever holding the lock every other serial test queues on — a silent, output-free
    /// hang. `join` returning an `Err` is the panic; a `join` that never returns would itself
    /// be the regression, which is why nothing here has a timeout to get wrong.
    #[test]
    fn taking_the_guard_twice_on_one_thread_panics_instead_of_hanging() {
        let attempt = std::thread::spawn(|| {
            let _guard = super::serial();
            let _second = super::serial(); // the deadlock this assertion replaces
        })
        .join();
        assert!(attempt.is_err(), "the re-entrant take must panic");
    }

    /// **The lock is not released while a worker the test started is still running.** A test that
    /// returns right after its last assertion (a season fetch cancelled by `Clear`, a flight whose
    /// landing nobody drains) leaves a `spawn_small` worker alive, and that worker would write the NEXT
    /// holder's globals, which reads as a flake in whichever bystander it hit; the guard's drop waits for it.
    #[test]
    fn releasing_the_guard_waits_for_a_worker_the_test_left_running() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let finished = Arc::new(AtomicBool::new(false));
        {
            let _guard = super::serial();
            let flag = Arc::clone(&finished);
            assert!(crate::task::spawn_small("lingering test worker", move || {
                std::thread::sleep(std::time::Duration::from_millis(150));
                flag.store(true, Ordering::SeqCst);
            }));
            assert!(!finished.load(Ordering::SeqCst), "sanity: the worker is still sleeping");
        }
        assert!(
            finished.load(Ordering::SeqCst),
            "the guard released the lock while the test's worker was still running"
        );
    }
}
