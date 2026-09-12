//! **Telemetry: the decision, the spool, and the worker that drains it.**
//!
//! The pieces have one ordering. [`consent`] and its storage are the part that has to be right
//! before anything can be collected or sent and are answerable entirely on the host. [`sentry`]
//! and [`posthog`] are the wire FORMATS; [`playback`] is the closed handled-error schema. Their
//! failures are silent 400s from a server that explains nothing, so they are pinned to tests while
//! there is still no network to hide behind. [`queue`] is the framing and caps, pure; [`spool`] is
//! the file those bytes live in and the one owner every read and write goes through; [`sender`] is
//! the socket, and the place the credential split decides which project this build reports to.
//!
//! **Ungated**, like `diag::scrub` and `diag::schema`, and for the reason both of those record: the
//! guarantees here are the tests — that no identifier exists before an opt-in, that withdrawal
//! destroys what it withdrew, that the event path fails closed on an ANSWERED "no" (an unanswered
//! decision defers the sign-in family instead, bounded, in memory — see `diag::defer`), that a
//! record queued while a flush was on the network is not erased by that flush's commit — and a
//! test behind a feature the default gate does not build is a test that never runs.
pub(crate) mod consent;
pub(crate) mod crashreport;
pub(crate) mod native;
pub(crate) mod window;
pub(crate) mod oneoff;
pub(crate) mod persistence;
pub(crate) mod playback;
pub(crate) mod posthog;
pub(crate) mod queue;
pub(crate) mod sender;
pub(crate) mod sentry;
pub(crate) mod signin;
pub(crate) mod spool;
pub(crate) mod storage;

use consent::Consent;

#[derive(Clone)]
pub(crate) struct BootReady {
    consent: Consent,
}

#[derive(Clone)]
pub(crate) enum BootPoll {
    Pending,
    Ready(BootReady),
    Failed,
}

pub(crate) struct BootReceipt {
    ticket: Option<crate::storage_worker::TypedTicket<BootReady>>,
    resolved: Option<BootPoll>,
}

impl BootReceipt {
    pub(crate) fn poll(&mut self) -> BootPoll {
        if let Some(result) = self.resolved.clone() {
            return result;
        }
        let Some(ticket) = self.ticket.as_ref() else {
            return BootPoll::Failed;
        };
        match ticket.try_recv() {
            Ok(ready) => {
                let result = BootPoll::Ready(ready);
                self.ticket = None;
                self.resolved = Some(result.clone());
                result
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => BootPoll::Pending,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.ticket = None;
                self.resolved = Some(BootPoll::Failed);
                BootPoll::Failed
            }
        }
    }
}

fn load_boot() -> BootReady {
    let consent = load();
    // These are all filesystem/spool operations. They precede publication so diagnostics from a
    // prior process are queued under the stored decision without ever blocking the SDL thread.
    let native_crashes = native::prepare_boot(&consent);
    crashreport::report_pending_for(&consent, &native_crashes);
    BootReady { consent }
}

pub(crate) fn start_boot() -> Result<BootReceipt, crate::storage_worker::SubmitError> {
    crate::storage_worker::submit(load_boot).map(|ticket| BootReceipt {
        ticket: Some(ticket),
        resolved: None,
    })
}

#[derive(Clone)]
pub(crate) struct Activated {
    consent: Consent,
}

#[derive(Clone)]
pub(crate) enum ActivationPoll {
    Pending,
    Ready(Activated),
    Failed,
}

pub(crate) struct ActivationReceipt {
    ticket: Option<crate::storage_worker::TypedTicket<Result<Activated, ()>>>,
    resolved: Option<ActivationPoll>,
}

impl ActivationReceipt {
    pub(crate) fn poll(&mut self) -> ActivationPoll {
        if let Some(result) = self.resolved.clone() {
            return result;
        }
        let Some(ticket) = self.ticket.as_ref() else {
            return ActivationPoll::Failed;
        };
        match ticket.try_recv() {
            Ok(Ok(ready)) => {
                let result = ActivationPoll::Ready(ready);
                self.ticket = None;
                self.resolved = Some(result.clone());
                result
            }
            Ok(Err(())) => {
                self.ticket = None;
                self.resolved = Some(ActivationPoll::Failed);
                ActivationPoll::Failed
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => ActivationPoll::Pending,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.ticket = None;
                self.resolved = Some(ActivationPoll::Failed);
                ActivationPoll::Failed
            }
        }
    }
}

fn activate_boot(ready: BootReady) -> Activated {
    let c = ready.consent;
    // Logged because the alternative is a silent behavioural difference between two televisions.
    // No identifier in the line: it is the one field here worth not putting in a log that gets
    // pasted into issue threads, and its PRESENCE is the only fact worth stating anyway.
    let presence = |id: &Option<String>| if id.is_some() { "yes" } else { "none" };
    crate::log(&format!(
        "telemetry: answered={} errors={} usage={} id={} errors_id={}",
        c.answered(),
        c.errors,
        c.usage,
        presence(&c.install_id),
        presence(&c.errors_id)
    ));
    consent::install(c.clone());
    if !c.errors {
        crate::player::report::clear_error_trace();
    }
    // **Which destinations this build can actually reach**, once, at boot. A decision of `usage=true`
    // in a build with no PostHog key sends nothing, and every other line in this log looks
    // identical either way — `diag::event` returns before the queue, correctly and silently. This
    // is the line that says whether telemetry is WIRED, as against merely consented to, and it
    // names no endpoint: which projects those are is a release-audit fact, not a per-boot one.
    crate::log(&format!(
        "telemetry: env={} sentry={} posthog={}",
        sender::ENVIRONMENT,
        if sender::has_sentry() { "yes" } else { "no" },
        if sender::has_posthog() { "yes" } else { "no" }
    ));
    // Session's cold read ran behind the consent read on the same FIFO. Any storage/sign-in report
    // it found therefore deferred while consent was unpublished; resolve that bounded batch now,
    // on this worker, before the app leaves its boot gate.
    crate::diag::replay_deferred();
    storage::replay_deferred();
    #[cfg(feature = "devtriggers")]
    crate::dev::run_storage_diagnostics();
    Activated { consent: c }
}

pub(crate) fn start_activation(
    ready: BootReady,
) -> Result<ActivationReceipt, crate::storage_worker::SubmitError> {
    crate::storage_worker::submit(move || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| activate_boot(ready))).map_err(
            |_| {
                // Publication precedes deferred replay so its normal gates can run. If that replay
                // panics, close authority again before reporting activation failure to the UI.
                consent::install(Consent::default());
                crate::log("telemetry: boot activation failed; reporting remains off");
            },
        )
    })
    .map(|ticket| ActivationReceipt {
        ticket: Some(ticket),
        resolved: None,
    })
}

/// Configure only the native process-lifecycle backend on the main thread. Disk migration,
/// crash import, deferred replay and spool work have already completed on the shared worker.
pub(crate) fn finish_boot(ready: Activated) -> native::Guard {
    let c = ready.consent;
    // The SDK starts only after the worker has safely queued old fallback records. Its guard lives
    // for the whole app and restores the C tracer on clean exit.
    native::sync_prepared(&c)
}

/// The first candidate that exists and parses. Same search-order shape as the session file, and
/// for the same reason: which of the two `/media` directories is writable depends on the jail
/// profile, so the answer cannot be a literal.
fn load() -> Consent {
    persistence::load(&candidates())
}

/// Where the decision lives: `paths::telemetry_candidates()`, until a test redirects it to a file
/// of its own. Same shape and same reason as `session::redirect_for_test`: every real candidate is
/// either a device path that does not exist on the dev Mac or the directory the test binary runs
/// from, so a test that writes through the real list leaves a consent file in `target/`.
#[cfg(not(test))]
fn candidates() -> Vec<std::path::PathBuf> {
    crate::paths::telemetry_candidates()
}

#[cfg(test)]
static TEST_FILE: std::sync::Mutex<Option<Vec<std::path::PathBuf>>> = std::sync::Mutex::new(None);

#[cfg(test)]
fn candidates() -> Vec<std::path::PathBuf> {
    match TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        Some(paths) => paths,
        None => crate::paths::telemetry_candidates(),
    }
}

/// Point this module's decision file at `p`, or back at the real search order with `None`. The
/// caller holds `crate::testlock::serial()` for the whole test: this is a crate global.
#[cfg(test)]
pub(crate) fn redirect_for_test(p: Option<std::path::PathBuf>) {
    crate::storage_worker::drain_for_test();
    let root = p.as_ref().and_then(|path| path.parent()).map(std::path::Path::to_path_buf);
    *TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()) = p.map(|p| vec![p]);
    persistence::redirect_root_for_test(root);
}

#[cfg(test)]
fn redirect_for_test_multi(paths: Vec<std::path::PathBuf>) {
    crate::storage_worker::drain_for_test();
    let root = paths
        .iter()
        .filter_map(|path| path.parent())
        .find(|path| path.exists())
        .map(std::path::Path::to_path_buf);
    *TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(paths);
    persistence::redirect_root_for_test(root);
}

#[cfg(test)]
fn load_from(candidates: &[std::path::PathBuf]) -> Consent {
    crate::storage_worker::drain_for_test();
    let root = candidates
        .iter()
        .filter_map(|path| path.parent())
        .find(|path| path.exists())
        .map(std::path::Path::to_path_buf);
    persistence::redirect_root_for_test(root);
    persistence::load(candidates)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersistenceState {
    Pending,
    Delegated,
    Durable,
    Uncertain,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueueFailure {
    Full,
    Stopped,
    StartFailed,
    Disconnected,
    OperationPanicked,
    CutoffFailed,
    CanonicalWriteFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RevisionStatus {
    pub(crate) revision: u64,
    pub(crate) write: PersistenceState,
    pub(crate) cleanup: persistence::CleanupResult,
    pub(crate) failure: Option<QueueFailure>,
}

impl RevisionStatus {
    pub(crate) fn cleanup_failed(self) -> bool {
        self.cleanup == persistence::CleanupResult::Failed
    }

    const fn pending(revision: u64) -> Self {
        Self {
            revision,
            write: PersistenceState::Pending,
            cleanup: persistence::CleanupResult::NotAttempted,
            failure: None,
        }
    }

    const fn failed(revision: u64, failure: QueueFailure) -> Self {
        Self {
            revision,
            write: PersistenceState::Failed,
            cleanup: persistence::CleanupResult::NotAttempted,
            failure: Some(failure),
        }
    }
}

struct Coordinator {
    next_revision: u64,
    latest_revision: u64,
    latest: Option<std::sync::Arc<std::sync::Mutex<RevisionStatus>>>,
    /// A refused Forget cannot run its disk/native purge. The next accepted enable carries this
    /// generation into its prospective cutoff, so stale native envelopes cannot cross tenures.
    tenure_cleanup: Option<u64>,
}

static PERSISTENCE: std::sync::Mutex<Coordinator> = std::sync::Mutex::new(Coordinator {
    next_revision: 0,
    latest_revision: 0,
    latest: None,
    tenure_cleanup: None,
});

pub(crate) struct ConsentReceipt {
    revision: u64,
    ticket: Option<crate::storage_worker::TypedTicket<RevisionStatus>>,
    resolved: Option<RevisionStatus>,
    status: std::sync::Arc<std::sync::Mutex<RevisionStatus>>,
}

impl ConsentReceipt {
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// Poll one operation without waiting for the disk worker. A disconnected ticket is a failed
    /// operation, never a Pending state that survives forever.
    pub(crate) fn poll(&mut self) -> RevisionStatus {
        if let Some(status) = self.resolved {
            return status;
        }
        let Some(ticket) = self.ticket.as_ref() else {
            return RevisionStatus::failed(self.revision, QueueFailure::Disconnected);
        };
        match ticket.try_recv() {
            Ok(status) => {
                self.ticket = None;
                self.resolved = Some(status);
                status
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => RevisionStatus::pending(self.revision),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                let status = RevisionStatus::failed(self.revision, QueueFailure::Disconnected);
                *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status;
                self.ticket = None;
                self.resolved = Some(status);
                status
            }
        }
    }

    #[cfg(test)]
    fn wait_blocking(mut self) -> RevisionStatus {
        if let Some(status) = self.resolved {
            return status;
        }
        let status = match self.ticket.take().expect("an unresolved receipt has a ticket").wait_blocking() {
            Ok(status) => status,
            Err(_) => RevisionStatus::failed(self.revision, QueueFailure::Disconnected),
        };
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status;
        self.resolved = Some(status);
        status
    }
}

pub(crate) fn latest_persistence_status() -> RevisionStatus {
    let latest = PERSISTENCE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .latest
        .clone();
    latest
        .map(|status| *status.lock().unwrap_or_else(|e| e.into_inner()))
        .unwrap_or(RevisionStatus {
            revision: 0,
            write: PersistenceState::Failed,
            cleanup: persistence::CleanupResult::NotAttempted,
            failure: None,
        })
}

struct CompletionGuard {
    status: std::sync::Arc<std::sync::Mutex<RevisionStatus>>,
    complete: bool,
}

impl CompletionGuard {
    fn finish(mut self, status: RevisionStatus) -> RevisionStatus {
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status;
        self.complete = true;
        status
    }
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        if self.complete {
            return;
        }
        let revision = self
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .revision;
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) =
            RevisionStatus::failed(revision, QueueFailure::Disconnected);
    }
}

#[derive(Clone)]
enum Operation {
    Record {
        decision: Consent,
        enabling_errors: bool,
        enabling_usage: bool,
        tenure_cleanup: Option<u64>,
    },
    Forget,
}

type PersistJob = Box<dyn FnOnce() -> persistence::PersistOutcome + Send + 'static>;
type WorkerJob = Box<dyn FnOnce() -> RevisionStatus + Send + 'static>;

fn submit_operation(
    decision: Consent,
    forgetting: bool,
    persist: PersistJob,
    submit: impl FnOnce(
        WorkerJob,
    ) -> Result<crate::storage_worker::TypedTicket<RevisionStatus>, crate::storage_worker::SubmitError>,
) -> ConsentReceipt {
    submit_operation_expected(None, decision, forgetting, persist, submit)
        .expect("an unconditional operation is always admitted to the domain coordinator")
}

fn submit_operation_expected(
    expected_revision: Option<u64>,
    decision: Consent,
    forgetting: bool,
    persist: PersistJob,
    submit: impl FnOnce(
        WorkerJob,
    ) -> Result<crate::storage_worker::TypedTicket<RevisionStatus>, crate::storage_worker::SubmitError>,
) -> Option<ConsentReceipt> {
    let mut coordinator = PERSISTENCE.lock().unwrap_or_else(|e| e.into_inner());
    if expected_revision.is_some_and(|expected| coordinator.latest_revision != expected) {
        return None;
    }
    let Some(revision) = coordinator.next_revision.checked_add(1) else {
        let status = RevisionStatus::failed(u64::MAX, QueueFailure::Stopped);
        let cell = std::sync::Arc::new(std::sync::Mutex::new(status));
        coordinator.latest_revision = status.revision;
        coordinator.latest = Some(cell.clone());
        return Some(ConsentReceipt {
            revision: status.revision,
            ticket: None,
            resolved: Some(status),
            status: cell,
        });
    };
    coordinator.next_revision = revision;
    let (enabling_errors, enabling_usage) = consent::request(decision.clone());
    if forgetting {
        coordinator.tenure_cleanup = Some(revision);
    }
    let operation = if forgetting {
        Operation::Forget
    } else {
        Operation::Record {
            decision,
            enabling_errors,
            enabling_usage,
            tenure_cleanup: coordinator.tenure_cleanup,
        }
    };
    let status = std::sync::Arc::new(std::sync::Mutex::new(RevisionStatus::pending(revision)));
    coordinator.latest_revision = revision;
    coordinator.latest = Some(status.clone());
    let guard = CompletionGuard {
        status: status.clone(),
        complete: false,
    };
    let job: WorkerJob = Box::new(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if !apply_worker_cutoff(&operation) {
                return RevisionStatus {
                    revision,
                    write: PersistenceState::Failed,
                    cleanup: persistence::CleanupResult::Failed,
                    failure: Some(QueueFailure::CutoffFailed),
                };
            }
            let outcome = persist();
            let cleanup = apply_worker_effects(revision, &operation);
            status_from_outcome(revision, outcome, cleanup)
        }));
        let status = result.unwrap_or_else(|_| {
            RevisionStatus::failed(revision, QueueFailure::OperationPanicked)
        });
        guard.finish(status)
    });
    let receipt = match submit(job) {
        Ok(ticket) => ConsentReceipt {
            revision,
            ticket: Some(ticket),
            resolved: None,
            status,
        },
        Err(error) => {
            let failure = match error {
                crate::storage_worker::SubmitError::Full => QueueFailure::Full,
                crate::storage_worker::SubmitError::Stopped => QueueFailure::Stopped,
                crate::storage_worker::SubmitError::StartFailed => QueueFailure::StartFailed,
            };
            let status = RevisionStatus::failed(revision, failure);
            *coordinator
                .latest
                .as_ref()
                .expect("admission installed a status")
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = status;
            ConsentReceipt {
                revision,
                ticket: None,
                resolved: Some(status),
                status: coordinator
                    .latest
                    .as_ref()
                    .expect("admission installed a status")
                    .clone(),
            }
        }
    };
    Some(receipt)
}

fn status_from_outcome(
    revision: u64,
    outcome: persistence::PersistOutcome,
    operation_cleanup: bool,
) -> RevisionStatus {
    let write = match outcome.write {
        persistence::PersistResult::Delegated => PersistenceState::Delegated,
        persistence::PersistResult::Durable => PersistenceState::Durable,
        persistence::PersistResult::Uncertain => PersistenceState::Uncertain,
        persistence::PersistResult::Failed | persistence::PersistResult::NotAttempted => {
            PersistenceState::Failed
        }
    };
    RevisionStatus {
        revision,
        write,
        cleanup: if operation_cleanup {
            outcome.cleanup
        } else {
            persistence::CleanupResult::Failed
        },
        failure: (write == PersistenceState::Failed).then_some(QueueFailure::CanonicalWriteFailed),
    }
}

/// Called on the persistence worker after the shared account tombstone is durable.
#[cfg(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test)))]
pub(crate) fn cleanup_after_account_clear() -> bool {
    persistence::cleanup_after_combined_clear(&candidates())
        != persistence::CleanupResult::Failed
}

fn is_latest(revision: u64) -> bool {
    PERSISTENCE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .latest_revision
        == revision
}

fn publish_effective_if_latest(revision: u64, decision: &Consent) -> bool {
    let coordinator = PERSISTENCE.lock().unwrap_or_else(|e| e.into_inner());
    if coordinator.latest_revision != revision {
        return false;
    }
    consent::publish_effective(decision.clone());
    true
}

fn clear_tenure_cleanup(generation: u64) {
    let mut coordinator = PERSISTENCE.lock().unwrap_or_else(|e| e.into_inner());
    if coordinator.tenure_cleanup == Some(generation) {
        coordinator.tenure_cleanup = None;
    }
}

fn sync_native_if_latest(revision: u64, decision: &Consent) -> bool {
    if !is_latest(revision) {
        return true;
    }
    let mut complete = native::sync_change(decision);
    // Native reconfiguration must not hold the coordinator. A No can therefore race the call,
    // but its effective runtime gate is already closed. Reconcile once after the call so a rejected
    // No cannot leave a stale enable active indefinitely; an accepted No also follows in FIFO.
    if !is_latest(revision) {
        let effective = consent::effective().unwrap_or_default();
        if !effective.errors {
            complete &= native::sync_change(&effective);
        }
    }
    complete
}

/// Prospective enable cutoff. This must finish before the Yes record is committed: after a crash,
/// boot trusts that durable record immediately and imports old diagnostics under it.
fn apply_worker_cutoff(operation: &Operation) -> bool {
    let Operation::Record {
        enabling_errors,
        enabling_usage,
        tenure_cleanup,
        ..
    } = operation
    else {
        return true;
    };
    if !*enabling_errors && !*enabling_usage {
        return true;
    }
    let mut complete = if tenure_cleanup.is_some() {
        spool::purge_runtime_all() & native::sync_change(&Consent::default())
    } else {
        let cutoff = Consent {
            errors: !*enabling_errors,
            usage: !*enabling_usage,
            ..Default::default()
        };
        spool::purge_before_opt_in(&cutoff)
    };
    if *enabling_errors {
        complete &= crashreport::discard_pending_before_opt_in();
        complete &= native::sync_change(&Consent::default());
    }
    if !complete {
        crate::log("telemetry: prospective consent cutoff failed; enable remains inactive");
        return false;
    }
    if let Some(generation) = tenure_cleanup {
        clear_tenure_cleanup(*generation);
    }
    if *enabling_errors {
        crate::plex::session::retry_dropped_stages();
        storage::retry_dropped_sign_in();
    }
    true
}

fn apply_worker_effects(revision: u64, operation: &Operation) -> bool {
    match operation {
        Operation::Record {
            decision,
            enabling_errors: _,
            enabling_usage: _,
            tenure_cleanup: _,
        } => {
            // A withdrawal's purge is ordered even when a later decision has already arrived.
            let mut complete = spool::purge_withdrawn(decision);
            if publish_effective_if_latest(revision, decision) {
                crate::diag::replay_deferred();
                storage::replay_deferred();
                complete &= sync_native_if_latest(revision, decision);
                flush_soon();
            }
            complete
        }
        Operation::Forget => {
            let complete = spool::purge_all_local() & native::sync_change(&Consent::default());
            if complete {
                clear_tenure_cleanup(revision);
            }
            complete
        }
    }
}

/// Queue a decision without ever turning Pending into a durability claim. Requested state is
/// visible to Settings immediately; withdrawals are effective immediately, while enables wait for
/// their prospective cutoff on the shared bounded worker.
pub(crate) fn record_with_receipt(c: Consent) -> ConsentReceipt {
    let legacy = candidates();
    let root = persistence::operation_root();
    let persisted = c.clone();
    let receipt = submit_operation(
        c.clone(),
        false,
        Box::new(move || persistence::record_at(&persisted, &legacy, root)),
        |job| crate::storage_worker::submit(job),
    );
    if !c.errors {
        crate::player::report::clear_error_trace();
    }
    receipt
}

/// Retry a UI decision only while it is still the coordinator's latest operation. A sign-out or
/// newer choice invalidates the old alert atomically with admission, so it cannot restore a
/// departed account's consent or identifiers.
pub(crate) fn retry_record_with_receipt(
    expected_revision: u64,
    c: Consent,
) -> Option<ConsentReceipt> {
    let legacy = candidates();
    let root = persistence::operation_root();
    let persisted = c.clone();
    submit_operation_expected(
        Some(expected_revision),
        c,
        false,
        Box::new(move || persistence::record_at(&persisted, &legacy, root)),
        |job| crate::storage_worker::submit(job),
    )
}

/// **End the signed-in account's tenure over telemetry.** The decision returns to *unanswered*
/// and is PUBLISHED FIRST — before any file I/O — so from that instant every STANDING producer's
/// gate answers no and the sender's per-record revision check picks up no further standing
/// record; this is what lets `PRIVACY.md` say that no further report of a category is picked up
/// after a sign-out. ONE record the sender had already passed through that check may still go
/// out — the check-to-POST gap has no cancellation — and the policy says exactly that, no more.
/// The one-off sign-in report is not a standing producer and answers no gate at all; what actually
/// removes it here is the purge below, same as everything else queued. Then both identifiers are
/// gone with it, and the canonical consent record receives a durable cleared state. Legacy
/// candidates and queued records are cleaned up on a best-effort basis; a cleanup failure is
/// retained as an explicit persistence outcome, not treated as proof that the record was absent.
/// The worker attempts to purge every queued record, standing or one-off
/// (`spool::purge_all_local`), and to stop the native capture backend and remove its pending
/// envelopes. The receipt reports cleanup independently from the cleared record's durability. A
/// missing legacy file is not interpreted as "never asked" once a canonical cleared record exists.
///
/// ONE mechanism with two consumers, both behind `auth::forget_account`: the two Sign out doors
/// (account menu, who's-watching pill) and Delete all local data.
///
/// Why sign-out ends it: consent is given by the person who signed the television in, and it
/// authorises nothing about the next person to sign in through the QR flow. Until 2026-09-04 the
/// decision and both identifiers deliberately outlived the sign-in ("so that a decision you have
/// already made is not put to you again"), which meant account B was never asked and every report
/// B caused went out under A's consent and A's identifiers. A managed-profile switch is not a
/// sign-out and keeps the decision. An external candidate may survive uninstall; app-local state
/// is removed with the app.
///
/// The crash MARK (`paths::telemetry_crashmark_candidates`) is deliberately left alone: it records
/// how much of the crash log has been read — a fact about the log, not about anybody — and the
/// next opt-in watermarks the log again through `crashreport::discard_pending_before_opt_in`.
pub(crate) fn forget_with_receipt() -> ConsentReceipt {
    let c = Consent::default();
    let legacy = candidates();
    let root = persistence::operation_root();
    let receipt = submit_operation(
        c,
        true,
        Box::new(move || persistence::forget_at(&legacy, root)),
        |job| crate::storage_worker::submit(job),
    );
    // These are memory-only and belong to the departing tenure. Disk, spool and native work stay
    // on the persistence worker; the effective consent snapshot above has already closed gates.
    crate::player::report::clear_error_trace();
    // Issue #76's report lane: what the signed-out account's own sign-in save did with its session
    // file is a fact ABOUT that account, and belongs to it — the same lifetime the consent decision
    // and the crash-report id already have here. See `signin::forget_storage_outcome`.
    signin::forget_storage_outcome();
    oneoff::forget();
    storage::forget();
    receipt
}

#[cfg(test)]
fn newly_enables_errors(previous: Option<&Consent>, next: &Consent) -> bool {
    let effectively_allows_errors = |c: &Consent| c.answered() && c.errors;
    effectively_allows_errors(next) && previous.is_none_or(|c| !effectively_allows_errors(c))
}

// ---- the spool, and the one worker that drains it ---------------------------------------------

/// Guards against two flushes at once. A spool is a read-modify-write of one file, so two workers
/// racing would have the second write back a list that does not know what the first acknowledged —
/// re-sending records that were accepted, which is the duplicate-issue failure `event_id` reuse
/// exists to prevent, arriving by a different door.
static FLUSHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// At most one sleeping retry worker. Manual flushes may happen while it waits; the eventual wake
/// is harmless, while spawning one sleeper per flush would consume this small device's thread cap.
static RETRY_SCHEDULED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Drain the spool on a worker thread.
///
/// **Never the main loop and never signal context.** A flush opens a socket and can block for
/// [`sender`]'s whole timeout; on the main thread that is a visibly frozen interface, and from a
/// signal handler it is neither async-signal-safe nor able to finish.
///
/// Returns immediately. A refused spawn is a return value rather than a panic — `task::spawn_small`
/// exists because `thread::spawn` panics on EAGAIN and killed this app once.
pub(crate) fn flush_soon() {
    // Unit tests may deliberately configure the DEV endpoints through Makefile environment
    // variables, but their spool path is a process-global test seam. A worker that outlives the
    // serial test guard can then wake in a later test and drain that later test's fixture. Tests
    // exercise `flush_now` synchronously where needed; never launch a background sender against a
    // movable test path. Shipping builds retain the asynchronous flush behavior below.
    if cfg!(test) {
        #[cfg(test)]
        TEST_FLUSH_REQUESTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }

    use std::sync::atomic::Ordering;
    if !sender::configured() {
        return; // nothing in this build to send to — see `sender`'s module doc
    }
    // A decision must EXIST — nothing loaded means nothing consented.
    let Some(c) = consent::effective() else { return };
    // …but deliberately no `if !c.any() { return }`. A withdrawal is exactly when the spool most
    // needs draining: `flush_now` retires a record whose category is now off without sending it,
    // so the purge rides the same path instead of needing one of its own. Returning early here
    // would leave records on disk that nobody has consented to, which is the opposite of what a
    // withdrawal is for. With both switches off the flush loads the spool, retires everything and
    // writes back an empty file, sending nothing.
    let _ = c.any();
    let decision_revision = consent::revision();
    if FLUSHING.swap(true, Ordering::AcqRel) {
        return; // one at a time — see FLUSHING
    }
    let ok = crate::task::spawn_small("telemetry", move || {
        let retry = flush_now(&c, decision_revision);
        FLUSHING.store(false, Ordering::Release);
        if let Some(seconds) = retry {
            if RETRY_SCHEDULED.swap(true, Ordering::AcqRel) {
                return;
            }
            // A retry is an actual schedule, not merely a number in a log. This worker owns no
            // spool lock and has a small stack; when it wakes it goes through FLUSHING again.
            let scheduled = crate::task::spawn_small("telemetry-retry", move || {
                std::thread::sleep(std::time::Duration::from_secs(seconds));
                RETRY_SCHEDULED.store(false, Ordering::Release);
                flush_soon();
            });
            if !scheduled {
                RETRY_SCHEDULED.store(false, Ordering::Release);
            }
        }
    });
    if !ok {
        FLUSHING.store(false, Ordering::Release);
    }
}

#[cfg(test)]
static TEST_FLUSH_REQUESTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// The flush itself, on the worker.
fn flush_now(c: &consent::Consent, decision_revision: u32) -> Option<u64> {
    let all = spool::read();
    if all.is_empty() {
        return None;
    }
    // Records that leave the spool, whether because a server took them or because nobody consents
    // to them any more. One list, because `queue::ack` asks one question — is this record still
    // ours to keep — and the two reasons for "no" need no distinction downstream.
    let mut retired: Vec<String> = Vec::new();
    let (newly_retired, dropped_by_consent, retry) = process_records(
        &all,
        c,
        || consent::revision() == decision_revision,
        sender::send_one,
    );
    retired.extend(newly_retired);
    if let Some(s) = retry {
        crate::log(&format!(
            "telemetry: holding {} records, ~{s}s",
            all.len() - retired.len()
        ));
    }
    // **Records queued under an earlier decision, re-checked against the CURRENT one at flush
    // time.** `sender::allowed` already answers this per record above (`process_records`); the log
    // line is what the 2026-09-10 TV telemetry proof's negative control had to work around by
    // moving a stale spool aside by hand instead of being able to read that they were dropped.
    if dropped_by_consent > 0 {
        crate::log(&format!(
            "telemetry: dropped {dropped_by_consent} spooled record(s) the current consent no longer allows"
        ));
    }
    if !retired.is_empty() {
        spool::commit_retiring(&retired);
        crate::log(&format!(
            "telemetry: flushed {} of {} record(s)",
            retired.len(),
            all.len()
        ));
    }
    retry
}

/// Process each destination as an independent logical lane. A dead/rate-limited Sentry endpoint
/// cannot prevent a later PostHog record from being attempted, or vice versa.
/// Returns `(retired ids, how many of those were dropped rather than sent because the CURRENT
/// consent no longer allows their category, the retry hold)`. The middle value is what lets the
/// flush log a consent-drop distinctly from an ordinary send/hopeless retirement — see
/// `flush_now`'s doc and the 2026-09-10 TV telemetry proof's negative control, which had no way to
/// read that a stale spooled record would be dropped rather than sent on the next flush.
fn process_records(
    all: &[queue::Record],
    c: &consent::Consent,
    mut still_current: impl FnMut() -> bool,
    mut send: impl FnMut(&queue::Record) -> (sender::Verdict, Option<u64>),
) -> (Vec<String>, usize, Option<u64>) {
    let mut retired = Vec::new();
    let mut dropped_by_consent = 0usize;
    let mut retry: Option<u64> = None;
    'destinations: for dest in [queue::Dest::Sentry, queue::Dest::PostHog] {
        for r in all.iter().filter(|r| r.dest == dest) {
            // Never carry an old consent snapshot through a withdrawal, a sign-out or a quick
            // off→on cycle. A request that already passed this check may finish because the socket
            // API has no cancellation; PRIVACY.md ("Your choices") states that narrow in-flight
            // boundary explicitly — it did not until 2026-09-04, while this comment said it did.
            if !still_current() {
                break 'destinations;
            }
            // Per record, against its own category — a spool written before a withdrawal (or
            // inherited from disk under a decision this process never lived through as a `record`
            // call, so `spool::purge_withdrawn` never ran against it) can still hold records of a
            // category that is now off. `r.category == OneOff` always answers true here — see
            // `sender::allowed`'s doc — so a one-off report is never counted as a consent-drop.
            if !sender::allowed(r, c) {
                retired.push(r.event_id.clone());
                dropped_by_consent += 1;
                continue;
            }
            match send(r) {
                (sender::Verdict::Done, _) | (sender::Verdict::Hopeless, _) => {
                    retired.push(r.event_id.clone())
                }
                // Stop this lane only. The failure applies to later records for the same endpoint,
                // but says nothing about the independent service in the other lane.
                (sender::Verdict::Keep, hold) => {
                    let s = hold.unwrap_or(sender::DEFAULT_HOLD_S);
                    retry = Some(retry.map_or(s, |old| old.min(s)));
                    break;
                }
            }
        }
    }
    (retired, dropped_by_consent, retry)
}

/// 16 bytes of `/dev/urandom` as lowercase hex — the ONLY way a consent identifier (the analytics
/// `install_id` or the crash-report `errors_id`) is ever produced.
///
/// Reads the device directly rather than taking a dependency: this crate has no RNG, and the one
/// property that matters is that the value is not derived from anything about this television or
/// this account. A read failure yields `None`, and [`consent::apply`] then records that channel as
/// off rather than inventing a fallback — a "random" identifier built from a clock or a MAC is
/// exactly the identifier this design refuses.
pub(crate) fn mint_id() -> Option<String> {
    let mut buf = [0u8; 16];
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .ok()?
        .read_exact(&mut buf)
        .ok()?;
    Some(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Does `s` have the shape [`mint_id`] produces — 32 lowercase hex characters, nothing else?
///
/// The native importer uses it to decide whether a `user.id` the crash daemon captured is OUR
/// crash-report id or something a future SDK scope put there: anything that is not this shape is
/// dropped with the rest of the user object.
pub(crate) fn is_minted_id(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_boot_receipt_is_pending_while_worker_storage_is_delayed_then_ready() {
        let executor = crate::storage_worker::Executor::start(1).unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let ticket = executor
            .submit(move || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                BootReady {
                    consent: Consent::default(),
                }
            })
            .unwrap();
        entered_rx.recv().unwrap();
        let mut receipt = BootReceipt {
            ticket: Some(ticket),
            resolved: None,
        };
        assert!(matches!(receipt.poll(), BootPoll::Pending));
        release_tx.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            match receipt.poll() {
                BootPoll::Pending => {
                    assert!(std::time::Instant::now() < deadline, "boot receipt stayed pending");
                    std::thread::yield_now();
                }
                BootPoll::Ready(_) => break,
                BootPoll::Failed => panic!("accepted cold boot disconnected"),
            }
        }
    }

    #[test]
    fn saved_opt_in_replays_a_session_storage_error_before_boot_activation_is_ready() {
        let _g = crate::testlock::serial();
        crate::storage_worker::drain_for_test();
        let saved = consent::current();
        storage::forget();
        consent::install(Consent::default());
        let report = storage::report_error_with_candidate_reads(
            storage::StorageErrorContext {
                stage: storage::StorageStage::Unreachable,
                service_error_code: None,
                class: storage::SessionStorageClass::SecureUnavailable,
                refused_marker: false,
                key_outcome: None,
                registered_with_app_id: false,
                registered_with_name: false,
                sealed_identity: None,
            },
            Some("app_dir:open_failed:5".into()),
        );
        assert_eq!(report, storage::ReportOutcome::Deferred);
        assert_eq!(storage::deferred_len_for_test(), 1);
        let opted_in = Consent {
            asked_version: consent::POLICY_VERSION,
            errors: true,
            errors_scope: consent::ERRORS_SCOPE,
            errors_id: Some("e".repeat(32)),
            ..Default::default()
        };
        let mut receipt = start_activation(BootReady { consent: opted_in }).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            match receipt.poll() {
                ActivationPoll::Pending => {
                    assert!(std::time::Instant::now() < deadline, "activation stayed pending");
                    std::thread::yield_now();
                }
                ActivationPoll::Ready(_) => break,
                ActivationPoll::Failed => panic!("activation worker disconnected"),
            }
        }
        assert_eq!(storage::deferred_len_for_test(), 0);
        assert!(consent::allows_errors_at(consent::ERRORS_SCOPE));
        storage::forget();
        consent::install(saved.unwrap_or_default());
    }

    fn outcome(
        write: persistence::PersistResult,
        cleanup: persistence::CleanupResult,
    ) -> persistence::PersistOutcome {
        persistence::PersistOutcome { write, cleanup }
    }

    fn usage_decision(on: bool, id: char) -> Consent {
        Consent {
            asked_version: consent::POLICY_VERSION,
            usage: on,
            usage_scope: if on { consent::USAGE_SCOPE } else { 0 },
            install_id: on.then(|| id.to_string().repeat(32)),
            ..Default::default()
        }
    }

    struct AsyncReset {
        dir: std::path::PathBuf,
        saved: Option<Consent>,
    }

    impl AsyncReset {
        fn new(name: &str) -> Self {
            crate::storage_worker::drain_for_test();
            let dir = std::env::temp_dir().join(format!(
                "plxnative-consent-async-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            spool::set_test_path(Some(dir.join("spool.bin")));
            *crashreport::TEST_ROOT.lock().unwrap() = Some(dir.clone());
            Self {
                dir,
                saved: consent::current(),
            }
        }
    }

    impl Drop for AsyncReset {
        fn drop(&mut self) {
            crate::storage_worker::drain_for_test();
            spool::set_test_path(None);
            *spool::TEST_LEGACY.lock().unwrap() = None;
            *crashreport::TEST_ROOT.lock().unwrap() = None;
            redirect_for_test(None);
            consent::install(self.saved.take().unwrap_or_default());
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn caller_returns_pending_while_persistence_is_blocked() {
        let _g = crate::testlock::serial();
        let _reset = AsyncReset::new("nonblocking");
        consent::install(Consent::default());
        let flushes_before = TEST_FLUSH_REQUESTS.load(std::sync::atomic::Ordering::Relaxed);
        let executor = crate::storage_worker::Executor::start(2).unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut receipt = submit_operation(
            usage_decision(true, 'a'),
            false,
            Box::new(move || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                outcome(
                    persistence::PersistResult::Durable,
                    persistence::CleanupResult::Complete,
                )
            }),
            |job| executor.submit(job),
        );
        entered_rx.recv().unwrap();
        assert!(consent::current().is_some_and(|c| c.usage));
        assert!(!consent::allows_usage(), "enable became effective before its cutoff/write");
        assert_eq!(receipt.poll().write, PersistenceState::Pending);
        assert_eq!(latest_persistence_status().write, PersistenceState::Pending);
        release_tx.send(()).unwrap();
        let status = receipt.wait_blocking();
        assert_eq!(status.write, PersistenceState::Durable);
        assert!(consent::allows_usage());
        assert!(
            TEST_FLUSH_REQUESTS.load(std::sync::atomic::Ordering::Relaxed) > flushes_before,
            "effective publication did not schedule the deferred-record flush"
        );
    }

    #[test]
    fn a_newer_withdrawal_stays_effective_after_an_older_enable_completes() {
        let _g = crate::testlock::serial();
        let _reset = AsyncReset::new("stale-enable");
        consent::install(Consent::default());
        let executor = crate::storage_worker::Executor::start(3).unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first = submit_operation(
            usage_decision(true, 'b'),
            false,
            Box::new(move || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                outcome(
                    persistence::PersistResult::Durable,
                    persistence::CleanupResult::Complete,
                )
            }),
            |job| executor.submit(job),
        );
        entered_rx.recv().unwrap();
        let second = submit_operation(
            usage_decision(false, 'x'),
            false,
            Box::new(|| {
                outcome(
                    persistence::PersistResult::Durable,
                    persistence::CleanupResult::Complete,
                )
            }),
            |job| executor.submit(job),
        );
        assert!(!consent::allows_usage(), "withdrawal did not close the runtime gate immediately");
        release_tx.send(()).unwrap();
        assert_eq!(first.wait_blocking().write, PersistenceState::Durable);
        assert!(!consent::allows_usage(), "old enable published over the newer withdrawal");
        let second_revision = second.revision();
        assert_eq!(second.wait_blocking().write, PersistenceState::Durable);
        assert!(!consent::allows_usage());
        assert_eq!(latest_persistence_status().revision, second_revision);
    }

    #[test]
    fn queue_full_is_a_visible_failure_and_never_runs_rejected_work() {
        let _g = crate::testlock::serial();
        let _reset = AsyncReset::new("full");
        consent::install(Consent::default());
        let executor = crate::storage_worker::Executor::start(1).unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first = executor
            .submit(move || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
            .unwrap();
        entered_rx.recv().unwrap();
        let queued = executor.submit(|| ()).unwrap();
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ran_job = ran.clone();
        let mut rejected = submit_operation(
            usage_decision(true, 'c'),
            false,
            Box::new(move || {
                ran_job.store(true, std::sync::atomic::Ordering::Release);
                outcome(
                    persistence::PersistResult::Durable,
                    persistence::CleanupResult::Complete,
                )
            }),
            |job| executor.submit(job),
        );
        let status = rejected.poll();
        assert_eq!(status.write, PersistenceState::Failed);
        assert_eq!(status.failure, Some(QueueFailure::Full));
        assert_eq!(latest_persistence_status(), status);
        assert!(!ran.load(std::sync::atomic::Ordering::Acquire));
        assert!(!consent::allows_usage(), "a rejected enable acquired authority");
        release_tx.send(()).unwrap();
        first.wait_blocking().unwrap();
        queued.wait_blocking().unwrap();
    }

    #[test]
    fn failed_prospective_cutoff_withholds_enable_and_skips_the_yes_write() {
        let _g = crate::testlock::serial();
        let reset = AsyncReset::new("cutoff-failure");
        consent::install(Consent::default());
        spool::set_test_path(Some(reset.dir.join("missing-parent/spool.bin")));
        let executor = crate::storage_worker::Executor::start(1).unwrap();
        let wrote_yes = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wrote_yes_job = wrote_yes.clone();
        let status = submit_operation(
            usage_decision(true, 'f'),
            false,
            Box::new(move || {
                wrote_yes_job.store(true, std::sync::atomic::Ordering::Release);
                outcome(
                    persistence::PersistResult::Durable,
                    persistence::CleanupResult::Complete,
                )
            }),
            |job| executor.submit(job),
        )
        .wait_blocking();
        assert_eq!(status.write, PersistenceState::Failed);
        assert_eq!(status.failure, Some(QueueFailure::CutoffFailed));
        assert!(!wrote_yes.load(std::sync::atomic::Ordering::Acquire));
        assert!(consent::current().is_some_and(|c| c.usage));
        assert!(!consent::allows_usage());
    }

    #[test]
    fn enabling_error_reports_does_not_depend_on_legacy_spool_storage() {
        let _g = crate::testlock::serial();
        let reset = AsyncReset::new("errors-without-legacy-spool");
        consent::install(Consent::default());
        // A regular file used as the parent deterministically models an inaccessible legacy
        // directory, including when tests run as root. The runtime queue remains writable.
        let blocked = reset.dir.join("legacy-parent");
        std::fs::write(&blocked, b"not a directory").unwrap();
        *spool::TEST_LEGACY.lock().unwrap() = Some(vec![blocked.join("spool.bin")]);
        assert!(spool::append(&queue::Record {
            category: queue::Category::Errors,
            dest: queue::Dest::Sentry,
            event_id: "pre-consent-error".into(),
            body: b"{}".to_vec(),
        }));
        let executor = crate::storage_worker::Executor::start(1).unwrap();
        let wrote_db8_consent = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wrote_db8_consent_job = wrote_db8_consent.clone();
        let decision = Consent {
            asked_version: consent::POLICY_VERSION,
            errors: true,
            errors_scope: consent::ERRORS_SCOPE,
            errors_id: Some("e".repeat(32)),
            ..Default::default()
        };

        let status = submit_operation(
            decision,
            false,
            Box::new(move || {
                wrote_db8_consent_job.store(true, std::sync::atomic::Ordering::Release);
                outcome(
                    persistence::PersistResult::Durable,
                    persistence::CleanupResult::Complete,
                )
            }),
            |job| executor.submit(job),
        )
        .wait_blocking();

        assert_eq!(status.failure, None);
        assert!(wrote_db8_consent.load(std::sync::atomic::Ordering::Acquire));
        assert!(consent::allows_errors());
        assert_eq!(status.write, PersistenceState::Durable);
        assert_eq!(status.cleanup, persistence::CleanupResult::Complete,
            "an inert legacy queue must not surface as a failed current consent action");
        assert!(spool::read().iter().all(|record| record.event_id != "pre-consent-error"),
            "publishing prospective consent must not revive an old active-queue record");
    }

    #[test]
    fn persistence_failure_is_not_confused_with_pending_and_cleanup_is_independent() {
        let _g = crate::testlock::serial();
        let _reset = AsyncReset::new("outcomes");
        consent::install(Consent::default());
        let executor = crate::storage_worker::Executor::start(2).unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut failed = submit_operation(
            usage_decision(false, 'x'),
            false,
            Box::new(move || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                outcome(
                    persistence::PersistResult::Failed,
                    persistence::CleanupResult::NotAttempted,
                )
            }),
            |job| executor.submit(job),
        );
        entered_rx.recv().unwrap();
        assert_eq!(failed.poll().write, PersistenceState::Pending);
        release_tx.send(()).unwrap();
        assert_eq!(failed.wait_blocking().write, PersistenceState::Failed);

        let cleanup_failed = submit_operation(
            usage_decision(false, 'x'),
            false,
            Box::new(|| {
                outcome(
                    persistence::PersistResult::Durable,
                    persistence::CleanupResult::Failed,
                )
            }),
            |job| executor.submit(job),
        )
        .wait_blocking();
        assert_eq!(cleanup_failed.write, PersistenceState::Durable);
        assert_eq!(cleanup_failed.cleanup, persistence::CleanupResult::Failed);

        let uncertain = submit_operation(
            usage_decision(false, 'x'),
            false,
            Box::new(|| {
                outcome(
                    persistence::PersistResult::Uncertain,
                    persistence::CleanupResult::NotAttempted,
                )
            }),
            |job| executor.submit(job),
        )
        .wait_blocking();
        assert_eq!(uncertain.write, PersistenceState::Uncertain);
        assert_eq!(uncertain.cleanup, persistence::CleanupResult::NotAttempted);
    }

    #[test]
    fn queued_disconnect_marks_latest_failed_even_while_admission_mutex_is_contended() {
        let _g = crate::testlock::serial();
        let _reset = AsyncReset::new("disconnect");
        consent::install(Consent::default());
        let executor = crate::storage_worker::Executor::start(1).unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let doomed_worker = executor
            .submit(move || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                panic!("injected worker death");
            })
            .unwrap();
        entered_rx.recv().unwrap();
        let receipt = submit_operation(
            usage_decision(false, 'x'),
            false,
            Box::new(|| {
                outcome(
                    persistence::PersistResult::Durable,
                    persistence::CleanupResult::Complete,
                )
            }),
            |job| executor.submit(job),
        );
        let coordinator = PERSISTENCE.lock().unwrap_or_else(|e| e.into_inner());
        release_tx.send(()).unwrap();
        assert!(doomed_worker.wait_blocking().is_err());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            let status = *receipt.status.lock().unwrap_or_else(|e| e.into_inner());
            if status.write == PersistenceState::Failed {
                assert_eq!(status.failure, Some(QueueFailure::Disconnected));
                break;
            }
            assert!(std::time::Instant::now() < deadline, "dropped job stayed Pending");
            std::thread::yield_now();
        }
        drop(coordinator);
        assert_eq!(latest_persistence_status().write, PersistenceState::Failed);
    }

    #[test]
    fn ordered_yes_no_yes_is_durable_on_reopen() {
        let _g = crate::testlock::serial();
        let reset = AsyncReset::new("ordered-reopen");
        consent::install(Consent::default());
        let legacy = reset.dir.join("telemetry.json");
        redirect_for_test(Some(legacy.clone()));
        let yes_a = usage_decision(true, 'd');
        let no = usage_decision(false, 'x');
        let yes_b = usage_decision(true, 'e');
        assert_eq!(record_with_receipt(yes_a).wait_blocking().write, PersistenceState::Durable);
        assert_eq!(record_with_receipt(no).wait_blocking().write, PersistenceState::Durable);
        assert_eq!(record_with_receipt(yes_b.clone()).wait_blocking().write, PersistenceState::Durable);
        assert_eq!(load_from(&[legacy]), yes_b);
    }

    #[test]
    fn only_an_errors_off_to_on_transition_discards_preconsent_crashes() {
        let state = |errors| Consent {
            asked_version: consent::POLICY_VERSION,
            errors,
            usage: false,
            install_id: None,
            errors_id: None,
            ..Default::default()
        };
        assert!(newly_enables_errors(None, &state(true)));
        assert!(newly_enables_errors(Some(&state(false)), &state(true)));
        assert!(!newly_enables_errors(Some(&state(true)), &state(true)));
        assert!(!newly_enables_errors(Some(&state(true)), &state(false)));

        let stale_yes = Consent {
            asked_version: consent::POLICY_VERSION.saturating_sub(1),
            errors: true,
            usage: false,
            install_id: None,
            errors_id: None,
            ..Default::default()
        };
        assert!(
            newly_enables_errors(Some(&stale_yes), &state(true)),
            "a stale-policy boolean is not current authorization; the new answer must watermark crashes accumulated while consent failed closed",
        );
    }

    #[test]
    fn a_held_destination_does_not_block_the_other_destination() {
        let record = |id: &str, category, dest| queue::Record {
            category,
            dest,
            event_id: id.into(),
            body: b"{}".to_vec(),
        };
        let all = vec![
            record("s1", queue::Category::Errors, queue::Dest::Sentry),
            record("p1", queue::Category::Usage, queue::Dest::PostHog),
        ];
        let c = consent::Consent {
            asked_version: consent::POLICY_VERSION,
            errors: true,
            usage: true,
            install_id: Some("id".into()),
            errors_id: Some("eid".into()),
            ..Default::default()
        };
        let mut attempted = Vec::new();
        let (retired, dropped_by_consent, retry) = process_records(
            &all,
            &c,
            || true,
            |r| {
                attempted.push(r.event_id.clone());
                if r.dest == queue::Dest::Sentry {
                    (sender::Verdict::Keep, Some(7))
                } else {
                    (sender::Verdict::Done, None)
                }
            },
        );
        assert_eq!(attempted, vec!["s1", "p1"]);
        assert_eq!(retired, vec!["p1"]);
        assert_eq!(dropped_by_consent, 0);
        assert_eq!(retry, Some(7));
    }

    /// **The flush re-checks consent, not just append time.** The 2026-09-10 TV telemetry proof
    /// had to move a stale spool aside by hand for its negative control — three records queued by
    /// an EARLIER session sat in the spool under a decision that no longer covered them, and
    /// nothing in the flush path proved they would be dropped rather than sent on the next flush.
    /// A record whose CURRENT consent no longer allows its category must never reach `send`, must
    /// be retired (removed from the spool) anyway, and must be logged as a consent-drop distinct
    /// from an ordinary send.
    #[test]
    fn process_records_drops_a_record_the_current_consent_no_longer_allows() {
        let record = |id: &str, category, dest| queue::Record {
            category,
            dest,
            event_id: id.into(),
            body: b"{}".to_vec(),
        };
        let all = vec![record(
            "stale-error",
            queue::Category::Errors,
            queue::Dest::Sentry,
        )];
        let no_consent = consent::Consent {
            asked_version: consent::POLICY_VERSION,
            errors: false,
            usage: false,
            ..Default::default()
        };
        let mut attempted = Vec::new();
        let (retired, dropped_by_consent, retry) = process_records(
            &all,
            &no_consent,
            || true,
            |r| {
                attempted.push(r.event_id.clone());
                (sender::Verdict::Done, None)
            },
        );
        assert!(attempted.is_empty(), "a disallowed record must never reach send");
        assert_eq!(retired, vec!["stale-error"]);
        assert_eq!(dropped_by_consent, 1);
        assert_eq!(retry, None);
    }

    /// **End-to-end through `flush_now` and the real spool**: a record queued while consent
    /// allowed it, flushed once the CURRENT consent no longer does, leaves the spool empty and
    /// logs the drop — the exact scenario the negative control worked around by hand. Mirrors
    /// `spool::tests::a_withdrawal_purges_its_own_category_and_leaves_the_other` in spirit, but at
    /// the FLUSH boundary rather than the withdrawal-time purge, which is a separate, already-
    /// covered mechanism (`record`'s call to `spool::purge_withdrawn`).
    #[test]
    fn flush_drops_a_spooled_record_the_current_consent_no_longer_allows() {
        let _g = crate::testlock::serial();
        let dir =
            std::env::temp_dir().join(format!("plxnative-flush-consent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let saved = consent::current();
        spool::set_test_path(Some(dir.join("spool.bin")));

        spool::append(&queue::Record {
            category: queue::Category::Errors,
            dest: queue::Dest::Sentry,
            event_id: "stale-error".into(),
            body: b"{}".to_vec(),
        });
        assert_eq!(spool::read().len(), 1, "the record must be on disk before the flush");

        let no_consent = consent::Consent {
            asked_version: consent::POLICY_VERSION,
            errors: false,
            usage: false,
            ..Default::default()
        };
        consent::install(no_consent.clone());

        let logged = crate::with_test_log(|p| {
            let retry = flush_now(&no_consent, consent::revision());
            assert!(retry.is_none());
            assert!(
                spool::read().is_empty(),
                "a record the current consent no longer allows must not survive the flush"
            );
            std::fs::read_to_string(p).unwrap_or_default()
        });
        assert!(
            logged.contains(
                "telemetry: dropped 1 spooled record(s) the current consent no longer allows"
            ),
            "missing the consent-drop log line: {logged}"
        );

        spool::set_test_path(None);
        if let Some(c) = saved {
            consent::install(c);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The identifier is 32 hex characters and two mints differ. Not a randomness test — it is a
    /// test that the SOURCE is the device and not a constant, which is the failure that would make
    /// every install share one id and nobody notice.
    #[test]
    fn a_minted_identifier_is_random_hex() {
        let Some(a) = mint_id() else { return }; // no /dev/urandom: nothing to assert
        assert_eq!(a.len(), 32, "16 bytes as hex");
        assert!(a
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert!(is_minted_id(&a), "the shape check rejects what the mint produces");
        let b = mint_id().expect("second read");
        assert_ne!(a, b, "two mints produced the same identifier");
    }

    /// The shape check is exact: length, case and alphabet. It is what stands between a future SDK
    /// scope value and the wire, so a near miss must not pass.
    #[test]
    fn the_id_shape_check_is_exact() {
        assert!(is_minted_id(&"0".repeat(32)));
        assert!(is_minted_id("0123456789abcdef0123456789abcdef"));
        for bad in [
            "",
            "0123456789ABCDEF0123456789abcdef",
            "0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdef0",
            "0123456789abcdef0123456789abcdeg",
            "0123456789abcdef-0123456789abcde",
            "id:0123456789abcdef0123456789abcd",
        ] {
            assert!(!is_minted_id(bad), "accepted {bad:?}");
        }
    }

    /// **Ending the tenure leaves neither identifier in the snapshot and no file on disk.** The
    /// snapshot is the half a producer on the render thread reads, so a report queued after the
    /// sign-out must find nothing to attach; the file is the half the next boot reads.
    /// `auth::tests::signing_out_leaves_no_consent_and_no_identifier_for_the_next_account` grades
    /// the same thing through the real sign-out tail.
    #[test]
    fn forgetting_the_tenure_clears_both_identifiers_and_the_file() {
        /// The redirects and the snapshot, handed back on drop, so a failed assertion cannot leave
        /// the next test writing into this directory.
        struct Redirects {
            dir: std::path::PathBuf,
            saved: Option<Consent>,
        }
        impl Drop for Redirects {
            fn drop(&mut self) {
                spool::set_test_path(None);
                redirect_for_test(None);
                if let Some(c) = self.saved.take() {
                    consent::install(c);
                }
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }
        let _g = crate::testlock::serial();
        let dir = std::env::temp_dir().join(format!("plxnative-forget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _redirects = Redirects {
            dir: dir.clone(),
            saved: consent::current(),
        };
        let file = dir.join("telemetry.json");
        let canonical = dir.join("consent.json");
        redirect_for_test(Some(file.clone()));
        spool::set_test_path(Some(dir.join("spool.jsonl")));
        assert_eq!(
            record_with_receipt(consent::apply(&Consent::default(), true, true, || {
                Some("f".repeat(32))
            }))
            .wait_blocking()
            .write,
            PersistenceState::Durable
        );
        assert!(canonical.exists());
        assert!(consent::errors_id().is_some() && consent::allows_usage());
        assert_eq!(
            forget_with_receipt().wait_blocking().write,
            PersistenceState::Durable
        );
        let after = consent::current().expect("a default decision is published, not none");
        assert!(!after.any() && !after.answered());
        assert!(after.install_id.is_none() && after.errors_id.is_none());
        assert!(consent::errors_id().is_none() && !consent::allows_errors());
        assert!(canonical.exists(), "the canonical cleared tombstone was removed");
    }

    #[test]
    fn consent_falls_back_to_packaged_state_and_forget_removes_every_copy() {
        use std::os::unix::fs::PermissionsExt;
        struct Reset {
            dir: std::path::PathBuf,
            saved: Option<Consent>,
        }
        impl Drop for Reset {
            fn drop(&mut self) {
                spool::set_test_path(None);
                redirect_for_test(None);
                if let Some(c) = self.saved.take() { consent::install(c); }
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }
        let _g = crate::testlock::serial();
        let dir = std::env::temp_dir()
            .join(format!("plxnative-consent-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let state = dir.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let external = dir.join("missing-parent/telemetry.json");
        let state_decision = state.join("telemetry.json");
        let canonical = state.join("consent.json");
        let state_spool = state.join("telemetry-spool.bin");
        let _reset = Reset { dir: dir.clone(), saved: consent::current() };
        redirect_for_test_multi(vec![external.clone(), state_decision.clone()]);
        spool::set_test_path(Some(state_spool.clone()));

        let chosen = consent::apply(&Consent::default(), true, true, || Some("s".repeat(32)));
        assert_eq!(
            record_with_receipt(chosen.clone()).wait_blocking().write,
            PersistenceState::Durable
        );
        assert!(!external.exists(), "legacy candidates are not write targets");
        assert_eq!(load_from(&[external.clone(), state_decision.clone()]), chosen);
        assert_eq!(
            std::fs::metadata(&canonical).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(spool::append(&queue::Record {
            category: queue::Category::OneOff,
            dest: queue::Dest::Sentry,
            event_id: "state-oneoff".into(),
            body: b"{}".to_vec(),
        }));

        // A second candidate copy must be erased too, not only the winner used by `record`.
        std::fs::create_dir_all(external.parent().unwrap()).unwrap();
        std::fs::write(&external, serde_json::to_vec(&chosen).unwrap()).unwrap();
        assert_eq!(
            forget_with_receipt().wait_blocking().write,
            PersistenceState::Durable
        );
        assert!(!external.exists() && canonical.exists());
        assert!(spool::read().is_empty());

    }

    #[test]
    fn conflicting_trusted_consent_candidates_fail_closed() {
        let _g = crate::testlock::serial();
        let dir = std::env::temp_dir()
            .join(format!("plxnative-consent-conflict-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let higher = dir.join("higher.json");
        let lower = dir.join("lower.json");
        let yes = consent::apply(&Consent::default(), true, true, || Some("y".repeat(32)));
        let no = consent::apply(&Consent::default(), false, false, || Some("n".repeat(32)));
        std::fs::write(&higher, serde_json::to_vec(&yes).unwrap()).unwrap();
        std::fs::write(&lower, serde_json::to_vec(&no).unwrap()).unwrap();
        let loaded = load_from(&[higher, lower]);
        assert!(!loaded.any() && !loaded.answered());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn identical_trusted_consent_duplicates_are_accepted() {
        let _g = crate::testlock::serial();
        let dir = std::env::temp_dir()
            .join(format!("plxnative-consent-identical-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.json");
        let b = dir.join("b.json");
        let yes = consent::apply(&Consent::default(), true, true, || Some("i".repeat(32)));
        let bytes = serde_json::to_vec(&yes).unwrap();
        std::fs::write(&a, &bytes).unwrap();
        std::fs::write(&b, &bytes).unwrap();
        assert_eq!(load_from(&[a, b]), yes);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fallback_no_cannot_be_overridden_by_a_readonly_higher_yes() {
        use std::os::unix::fs::PermissionsExt;
        struct Reset(std::path::PathBuf, Option<Consent>);
        impl Drop for Reset {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
                redirect_for_test(None);
                if let Some(c) = self.1.take() { consent::install(c); }
                let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
            }
        }
        let _g = crate::testlock::serial();
        let dir = std::env::temp_dir()
            .join(format!("plxnative-consent-readonly-{}", std::process::id()));
        let higher_dir = dir.join("higher");
        let state = dir.join("state/telemetry.json");
        std::fs::create_dir_all(&higher_dir).unwrap();
        std::fs::create_dir_all(state.parent().unwrap()).unwrap();
        let higher = higher_dir.join("telemetry.json");
        let yes = consent::apply(&Consent::default(), true, true, || Some("h".repeat(32)));
        std::fs::write(&higher, serde_json::to_vec(&yes).unwrap()).unwrap();
        std::fs::set_permissions(&higher, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&higher_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let _reset = Reset(higher_dir.clone(), consent::current());
        redirect_for_test_multi(vec![higher.clone(), state.clone()]);
        persistence::redirect_root_for_test(Some(state.parent().unwrap().to_path_buf()));
        let no = consent::apply(&Consent::default(), false, false, || Some("n".repeat(32)));
        assert_eq!(
            record_with_receipt(no).wait_blocking().write,
            PersistenceState::Durable
        );
        std::fs::set_permissions(&higher_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        let canonical = state.parent().unwrap().join("consent.json");
        assert!(higher.exists() && canonical.exists(), "the legacy source or canonical decision vanished");
        persistence::redirect_root_for_test(Some(state.parent().unwrap().to_path_buf()));
        let loaded = persistence::load(&[higher, state]);
        assert!(!loaded.any() && loaded.answered(), "old Yes was re-enabled: {loaded:?}");
    }

    #[test]
    fn a_successful_decision_write_sweeps_an_owned_lower_duplicate() {
        let _g = crate::testlock::serial();
        let dir = std::env::temp_dir()
            .join(format!("plxnative-consent-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let higher = dir.join("higher.json");
        let lower = dir.join("lower.json");
        std::fs::write(&lower, b"{}").unwrap();
        let saved = consent::current();
        redirect_for_test_multi(vec![higher.clone(), lower.clone()]);
        assert_eq!(
            record_with_receipt(consent::apply(&Consent::default(), false, false, || {
                Some("s".repeat(32))
            }))
            .wait_blocking()
            .write,
            PersistenceState::Durable
        );
        assert!(dir.join("consent.json").exists());
        assert!(!lower.exists(), "a durable canonical write should clean stale legacy sources");
        redirect_for_test(None);
        if let Some(c) = saved { consent::install(c); }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// An unreadable or corrupt file is the DEFAULT decision, never a partial one — a file we
    /// cannot understand is not consent.
    #[test]
    fn an_unparsable_file_is_not_consent() {
        let c: Consent = serde_json::from_slice(b"{ not json").unwrap_or_default();
        assert!(!c.any() && !c.answered());
    }

    #[test]
    fn a_symlink_cannot_supply_telemetry_consent() {
        use std::os::unix::fs::symlink;
        let _g = crate::testlock::serial();
        let dir =
            std::env::temp_dir().join(format!("plxnative-consent-symlink-{}", std::process::id()));
        let _ = std::fs::create_dir(&dir);
        let victim = dir.join("attacker.json");
        let candidate = dir.join("consent.json");
        let _ = std::fs::remove_file(&candidate);
        std::fs::write(
            &victim,
            format!(
                r#"{{"asked_version":{},"errors":true,"usage":true}}"#,
                consent::POLICY_VERSION
            ),
        )
        .unwrap();
        symlink(&victim, &candidate).unwrap();

        let loaded = load_from(&[candidate.clone()]);
        assert!(!loaded.any() && !loaded.answered());

        let _ = std::fs::remove_file(candidate);
        let _ = std::fs::remove_file(victim);
        let _ = std::fs::remove_dir(dir);
    }

    /// **A write-widened consent file is not this person's decision — it is discarded like a
    /// symlink or a corrupt file, not merely repaired.** Seeded here as a stored "yes/yes" (the
    /// worst case: a forged `usage: true`/`errors: true` reaching further consent than anyone here
    /// actually granted). Read-only widening (`0o644`) is a disclosure problem and is covered
    /// separately below — this test is specifically the write-widened (`0o666`) case.
    #[test]
    fn a_write_widened_consent_file_is_treated_as_unanswered_and_reset_to_0600() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let dir = std::env::temp_dir()
            .join(format!("plxnative-consent-widened-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("telemetry.json");
        std::fs::write(
            &file,
            format!(
                r#"{{"asked_version":{},"errors":true,"usage":true,"install_id":"{}","errors_id":"{}"}}"#,
                consent::POLICY_VERSION,
                "a".repeat(32),
                "b".repeat(32)
            ),
        )
        .unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666)).unwrap();

        let loaded = load_from(&[file.clone()]);
        assert!(
            !loaded.errors && !loaded.usage,
            "both categories must come back unanswered, not the forged yes/yes"
        );
        assert!(!loaded.answered() && !loaded.any());
        assert!(
            loaded.install_id.is_none() && loaded.errors_id.is_none(),
            "a forged file's identifiers are not ours and must be discarded with it"
        );

        let mode = std::fs::metadata(dir.join("consent.json")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the cleared barrier must be private: {mode:o}");
        assert!(file.exists(), "the untrusted legacy source remains stale and ignored");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The read-only-widened twin: `0o644` carries no write bit, so the stored decision is still
    /// provably this person's own and must load exactly as written.
    #[test]
    fn a_read_only_widened_consent_file_still_loads_as_written() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let dir = std::env::temp_dir()
            .join(format!("plxnative-consent-readable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("telemetry.json");
        std::fs::write(
            &file,
            format!(r#"{{"asked_version":{},"errors":true,"usage":false}}"#, consent::POLICY_VERSION),
        )
        .unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();

        let loaded = load_from(&[file.clone()]);
        assert!(loaded.errors && !loaded.usage && loaded.answered());

        let mode = std::fs::metadata(dir.join("consent.json")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the widened mode was not repaired: {mode:o}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A canonical record is terminal: replaying a stale legacy candidate cannot override it.
    #[test]
    fn a_replayed_older_valid_consent_file_is_accepted() {
        let _g = crate::testlock::serial();
        let dir =
            std::env::temp_dir().join(format!("plxnative-consent-replay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("telemetry.json");

        let old = format!(r#"{{"asked_version":{},"errors":false,"usage":false}}"#, consent::POLICY_VERSION);
        std::fs::write(&file, &old).unwrap();
        let old_bytes = std::fs::read(&file).unwrap();

        // A later decision supersedes it.
        std::fs::write(
            &file,
            format!(r#"{{"asked_version":{},"errors":true,"usage":true}}"#, consent::POLICY_VERSION),
        )
        .unwrap();
        let current = load_from(&[file.clone()]);
        assert!(current.errors && current.usage);

        // The replay: the old bytes come back, same owner, same mode (an existing path's mode is
        // untouched by an ordinary overwrite).
        std::fs::write(&file, &old_bytes).unwrap();

        let replayed = load_from(&[file.clone()]);
        assert!(replayed.errors && replayed.usage);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file written by a FUTURE build, carrying fields this one does not know, still parses —
    /// and a file missing fields still parses. Both matter on a device that can be downgraded by a
    /// reinstall while the file survives it.
    #[test]
    fn the_stored_shape_tolerates_version_skew() {
        let older: Consent = serde_json::from_slice(br#"{"asked_version":1,"usage":true}"#)
            .expect("a file with fewer fields still parses");
        assert!(older.usage && !older.errors && older.install_id.is_none());
        let newer: Consent =
            serde_json::from_slice(br#"{"asked_version":1,"usage":true,"a_field_from_later":7}"#)
                .expect("a file with more fields still parses");
        assert!(newer.usage);
    }
}
