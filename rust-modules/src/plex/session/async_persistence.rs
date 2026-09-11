//! Nonblocking Session persistence admission and completion tracking.
//!
//! The production executor is the application's one bounded FIFO in [`crate::storage_worker`];
//! the local executors below exist only to prove lifecycle and revision behavior without a TV.

use super::{
    ClearOutcome, PersistOutcome, SaveAuthority, Session, CACHE, LOCKED_STATE, NOT_LOCKED,
};
use crate::storage::{CommitStage, StoreError};
use crate::storage_worker::SubmitError;
use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, OnceLock};

type Job = Box<dyn FnOnce() + Send + 'static>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    Write(PersistOutcome),
    Clear { cleanup_failed: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    Admission(SubmitError),
    Persistence(PersistOutcome),
    Storage(StoreError),
    WorkerDropped,
    Superseded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionOutcome {
    Durable(Operation),
    Uncertain {
        stage: CommitStage,
        errno: i32,
    },
    Failed(Failure),
    /// The disk callback may have run, but a later admitted clear revoked this process tenure.
    Superseded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Completion {
    pub(crate) revision: u64,
    pub(crate) outcome: CompletionOutcome,
}

#[derive(Debug)]
pub(crate) struct Receipt {
    revision: u64,
    result: Option<Receiver<Completion>>,
    resolved: Option<Completion>,
    status: Arc<Mutex<LatestStatus>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Poll {
    Pending { revision: u64 },
    Complete(Completion),
}

impl Receipt {
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// Poll this operation without waiting for persistence. Resolved outcomes remain readable.
    pub(crate) fn poll(&mut self) -> Poll {
        if let Some(completion) = self.resolved {
            return Poll::Complete(completion);
        }
        let Some(result) = self.result.as_ref() else {
            return Poll::Complete(self.disconnected());
        };
        match result.try_recv() {
            Ok(completion) => {
                self.result = None;
                self.resolved = Some(completion);
                Poll::Complete(completion)
            }
            Err(mpsc::TryRecvError::Empty) => Poll::Pending {
                revision: self.revision,
            },
            Err(mpsc::TryRecvError::Disconnected) => Poll::Complete(self.disconnected()),
        }
    }

    fn disconnected(&mut self) -> Completion {
        let completion = Completion {
            revision: self.revision,
            outcome: CompletionOutcome::Failed(Failure::WorkerDropped),
        };
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) =
            LatestStatus::Failed(Failure::WorkerDropped);
        self.result = None;
        self.resolved = Some(completion);
        completion
    }

    /// Tests and background callers may wait; production UI/auth callers poll.
    #[cfg(test)]
    pub(crate) fn wait_blocking(mut self) -> Completion {
        if let Some(completion) = self.resolved {
            return completion;
        }
        match self
            .result
            .take()
            .expect("an unresolved receipt owns a result channel")
            .recv()
        {
            Ok(completion) => completion,
            Err(_) => self.disconnected(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LatestStatus {
    Pending,
    Durable,
    Uncertain { stage: CommitStage, errno: i32 },
    Failed(Failure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub(crate) latest_revision: u64,
    pub(crate) latest: Option<LatestStatus>,
    pub(crate) durable_revision: Option<u64>,
    pub(crate) revocation_floor: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdmissionFailure {
    Queue(SubmitError),
    Uninitialized,
    Locked,
    Revoked,
    RevisionExhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AdmissionError {
    pub(crate) revision: Option<u64>,
    pub(crate) failure: AdmissionFailure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommitDetail {
    Durable,
    Uncertain { stage: CommitStage, errno: i32 },
    Failed(StoreError),
}

thread_local! {
    static LAST_COMMIT: RefCell<Option<CommitDetail>> = const { RefCell::new(None) };
}

pub(super) fn reset_commit_detail() {
    LAST_COMMIT.with(|slot| *slot.borrow_mut() = None);
}

fn note_commit_detail(detail: Result<(), CommitDetail>) {
    LAST_COMMIT.with(|slot| {
        *slot.borrow_mut() = Some(match detail {
            Ok(()) => CommitDetail::Durable,
            Err(detail) => detail,
        });
    });
}

pub(super) fn note_commit_durable() {
    note_commit_detail(Ok(()));
}

pub(super) fn note_commit_uncertain(stage: CommitStage, errno: i32) {
    note_commit_detail(Err(CommitDetail::Uncertain { stage, errno }));
}

pub(super) fn note_commit_failed(error: StoreError) {
    note_commit_detail(Err(CommitDetail::Failed(error)));
}

fn take_commit_detail() -> Option<CommitDetail> {
    LAST_COMMIT.with(|slot| slot.borrow_mut().take())
}

#[derive(Clone, Copy)]
enum DiskOutcome {
    Write {
        outcome: PersistOutcome,
        commit: Option<CommitDetail>,
    },
    Clear {
        outcome: ClearOutcome,
        commit: CommitDetail,
    },
}

impl DiskOutcome {
    fn classify(self) -> CompletionOutcome {
        match self {
            Self::Write { outcome, commit } => match commit {
                Some(CommitDetail::Uncertain { stage, errno }) => {
                    CompletionOutcome::Uncertain { stage, errno }
                }
                Some(CommitDetail::Failed(error)) => {
                    CompletionOutcome::Failed(Failure::Storage(error))
                }
                Some(CommitDetail::Durable) | None if outcome.persisted() => {
                    CompletionOutcome::Durable(Operation::Write(outcome))
                }
                _ => CompletionOutcome::Failed(Failure::Persistence(outcome)),
            },
            Self::Clear { outcome, commit } => match outcome.durability {
                super::ClearDurability::Durable => match commit {
                    CommitDetail::Durable => CompletionOutcome::Durable(Operation::Clear {
                        cleanup_failed: outcome.cleanup_failed,
                    }),
                    CommitDetail::Uncertain { stage, errno } => {
                        CompletionOutcome::Uncertain { stage, errno }
                    }
                    CommitDetail::Failed(error) => {
                        CompletionOutcome::Failed(Failure::Storage(error))
                    }
                },
                super::ClearDurability::Uncertain => match commit {
                    CommitDetail::Uncertain { stage, errno } => {
                        CompletionOutcome::Uncertain { stage, errno }
                    }
                    CommitDetail::Failed(error) => {
                        CompletionOutcome::Failed(Failure::Storage(error))
                    }
                    CommitDetail::Durable => {
                        CompletionOutcome::Failed(Failure::Persistence(PersistOutcome::WriteFailed))
                    }
                },
                super::ClearDurability::Failed => match commit {
                    CommitDetail::Failed(error) => {
                        CompletionOutcome::Failed(Failure::Storage(error))
                    }
                    _ => {
                        CompletionOutcome::Failed(Failure::Persistence(PersistOutcome::WriteFailed))
                    }
                },
            },
        }
    }
}

#[derive(Clone)]
struct State {
    revision: u64,
    durable_revision: Option<u64>,
    revocation_floor: Option<u64>,
    requires_fresh: bool,
    latest: Option<Arc<Mutex<LatestStatus>>>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            revision: 0,
            durable_revision: None,
            revocation_floor: None,
            requires_fresh: false,
            latest: None,
        }
    }
}

struct Coordinator {
    state: Arc<Mutex<State>>,
}

impl Coordinator {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    fn status(&self) -> Status {
        let (latest_revision, durable_revision, revocation_floor, latest) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                state.revision,
                state.durable_revision,
                state.revocation_floor,
                state.latest.clone(),
            )
        };
        Status {
            latest_revision,
            latest: latest.map(|status| *status.lock().unwrap_or_else(|e| e.into_inner())),
            durable_revision,
            revocation_floor,
        }
    }

    fn update(
        &self,
        executor: &dyn Submitter,
        edit: impl FnOnce(&Session) -> Option<Session>,
        persist: impl FnOnce(Session, SaveAuthority) -> DiskOutcome + Send + 'static,
    ) -> Result<Option<Receipt>, AdmissionError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.requires_fresh {
            return Err(AdmissionError {
                revision: None,
                failure: AdmissionFailure::Revoked,
            });
        }
        if LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed) != NOT_LOCKED {
            return Err(AdmissionError {
                revision: None,
                failure: AdmissionFailure::Locked,
            });
        }
        let current = CACHE.read().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(current) = current.filter(|session| !session.client_id.is_empty()) else {
            return Err(AdmissionError {
                revision: None,
                failure: AdmissionFailure::Uninitialized,
            });
        };
        let Some(next) = edit(&current) else {
            return Ok(None);
        };
        let revision = next_revision(&mut state)?;
        let command_snapshot = next.clone();
        let receipt = self.submit_locked(&mut state, revision, executor, move || {
            persist(command_snapshot, SaveAuthority::Routine)
        })?;
        *CACHE.write().unwrap_or_else(|e| e.into_inner()) = Some(next);
        Ok(Some(receipt))
    }

    fn update_ordinary(
        &self,
        executor: &dyn Submitter,
        edit: impl FnOnce(&Session) -> Option<Session>,
    ) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.requires_fresh
            || LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed) != NOT_LOCKED
        {
            return false;
        }
        let current = CACHE.read().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(current) = current.filter(|session| !session.client_id.is_empty()) else {
            return false;
        };
        let Some(next) = edit(&current) else {
            return false;
        };
        let Ok(revision) = next_revision(&mut state) else {
            return false;
        };
        let command = next.clone();
        let receipt = self.submit_locked(&mut state, revision, executor, move || {
            execute_write(command, SaveAuthority::Routine)
        });
        match receipt {
            Ok(receipt) => {
                *CACHE.write().unwrap_or_else(|e| e.into_inner()) = Some(next);
                install_ordinary_receipt(revision, Some(receipt));
                true
            }
            Err(_) => {
                install_ordinary_receipt(revision, None);
                false
            }
        }
    }

    fn replace(
        &self,
        executor: &dyn Submitter,
        snapshot: Session,
        authority: SaveAuthority,
        persist: impl FnOnce(Session, SaveAuthority) -> DiskOutcome + Send + 'static,
    ) -> Result<Receipt, AdmissionError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.requires_fresh
            && (authority != SaveAuthority::FreshReauthentication
                || snapshot.account_token.is_empty())
        {
            return Err(AdmissionError {
                revision: None,
                failure: AdmissionFailure::Revoked,
            });
        }
        let revision = next_revision(&mut state)?;
        let published = snapshot.clone();
        let receipt = self.submit_locked(&mut state, revision, executor, move || {
            persist(snapshot, authority)
        })?;
        if authority == SaveAuthority::FreshReauthentication {
            state.requires_fresh = false;
        }
        *CACHE.write().unwrap_or_else(|e| e.into_inner()) = Some(published);
        Ok(receipt)
    }

    fn retry_ordinary(&self, expected_revision: u64, executor: &dyn Submitter) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.revision != expected_revision || state.requires_fresh {
            return false;
        }
        let Some(snapshot) = CACHE.read().unwrap_or_else(|e| e.into_inner()).clone() else {
            return false;
        };
        let Ok(revision) = next_revision(&mut state) else {
            return false;
        };
        let receipt = self.submit_locked(&mut state, revision, executor, move || {
            execute_write(snapshot, SaveAuthority::Routine)
        });
        match receipt {
            Ok(receipt) => {
                install_ordinary_receipt(revision, Some(receipt));
                true
            }
            Err(_) => {
                install_ordinary_receipt(revision, None);
                false
            }
        }
    }

    fn clear(
        &self,
        executor: &dyn Submitter,
        persist: impl FnOnce() -> DiskOutcome + Send + 'static,
    ) -> Result<Receipt, AdmissionError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let revision = next_revision(&mut state)?;
        state.revocation_floor = Some(revision);
        state.requires_fresh = true;
        install_ordinary_receipt(0, None);
        // Runtime revocation is immediate even when the bounded queue cannot accept durability.
        *CACHE.write().unwrap_or_else(|e| e.into_inner()) = Some(Session::default());
        self.submit_locked(&mut state, revision, executor, persist)
    }

    fn submit_locked(
        &self,
        state: &mut State,
        revision: u64,
        executor: &dyn Submitter,
        persist: impl FnOnce() -> DiskOutcome + Send + 'static,
    ) -> Result<Receipt, AdmissionError> {
        let (reply, result) = mpsc::channel();
        let status = Arc::new(Mutex::new(LatestStatus::Pending));
        let mut guard = CompletionGuard {
            state: self.state.clone(),
            status: status.clone(),
            revision,
            armed: true,
        };
        let job = Box::new(move || {
            let outcome = if guard.superseded_before_disk() {
                guard.finish_outcome(CompletionOutcome::Superseded)
            } else {
                guard.finish_disk(persist())
            };
            let _ = reply.send(Completion { revision, outcome });
        });
        state.latest = Some(status.clone());
        if let Err(error) = executor.submit(job) {
            *status.lock().unwrap_or_else(|e| e.into_inner()) =
                LatestStatus::Failed(Failure::Admission(error));
            return Err(AdmissionError {
                revision: Some(revision),
                failure: AdmissionFailure::Queue(error),
            });
        }
        Ok(Receipt {
            revision,
            result: Some(result),
            resolved: None,
            status,
        })
    }
}

fn next_revision(state: &mut State) -> Result<u64, AdmissionError> {
    let Some(revision) = state.revision.checked_add(1) else {
        return Err(AdmissionError {
            revision: None,
            failure: AdmissionFailure::RevisionExhausted,
        });
    };
    state.revision = revision;
    Ok(revision)
}

struct CompletionGuard {
    state: Arc<Mutex<State>>,
    status: Arc<Mutex<LatestStatus>>,
    revision: u64,
    armed: bool,
}

impl CompletionGuard {
    fn superseded_before_disk(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .revocation_floor
            .is_some_and(|floor| self.revision < floor)
    }

    fn finish_disk(&mut self, disk: DiskOutcome) -> CompletionOutcome {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let outcome = if state
            .revocation_floor
            .is_some_and(|floor| self.revision < floor)
        {
            CompletionOutcome::Superseded
        } else {
            disk.classify()
        };
        if matches!(outcome, CompletionOutcome::Durable(_)) {
            state.durable_revision = Some(
                state
                    .durable_revision
                    .map_or(self.revision, |old| old.max(self.revision)),
            );
        }
        drop(state);
        self.finish_outcome(outcome)
    }

    fn finish_outcome(&mut self, outcome: CompletionOutcome) -> CompletionOutcome {
        match outcome {
            CompletionOutcome::Uncertain { stage, errno } => crate::log(&format!(
                "session: async persistence uncertain revision={} stage={stage:?} errno={errno}",
                self.revision
            )),
            CompletionOutcome::Failed(failure) => crate::log(&format!(
                "session: async persistence failed revision={} failure={failure:?}",
                self.revision
            )),
            CompletionOutcome::Durable(_) | CompletionOutcome::Superseded => {}
        }
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = match outcome {
            CompletionOutcome::Durable(_) => LatestStatus::Durable,
            CompletionOutcome::Uncertain { stage, errno } => {
                LatestStatus::Uncertain { stage, errno }
            }
            CompletionOutcome::Failed(failure) => LatestStatus::Failed(failure),
            CompletionOutcome::Superseded => LatestStatus::Failed(Failure::Superseded),
        };
        self.armed = false;
        outcome
    }
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // This per-operation cell is independent of the admission mutex. It therefore cannot
        // self-deadlock when a Full submit drops the boxed job synchronously, and a queued job
        // dropped by a panicking worker cannot strand the public status at Pending.
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) =
            LatestStatus::Failed(Failure::WorkerDropped);
    }
}

trait Submitter: Send + Sync {
    fn submit(&self, job: Job) -> Result<(), SubmitError>;
}

struct SharedExecutor;

impl Submitter for SharedExecutor {
    fn submit(&self, job: Job) -> Result<(), SubmitError> {
        crate::storage_worker::submit(move || job()).map(|ticket| drop(ticket))
    }
}

static COORDINATOR: OnceLock<Coordinator> = OnceLock::new();
static EXECUTOR: SharedExecutor = SharedExecutor;
static ORDINARY_RECEIPT: Mutex<Option<Receipt>> = Mutex::new(None);
static ORDINARY_REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Called only while the coordinator state mutex is held, including clear. That lock makes
/// revision publication + receipt replacement one ordering decision across concurrent producers.
fn install_ordinary_receipt(revision: u64, receipt: Option<Receipt>) {
    *ORDINARY_RECEIPT.lock().unwrap_or_else(|e| e.into_inner()) = receipt;
    ORDINARY_REVISION.store(revision, std::sync::atomic::Ordering::Release);
}

fn coordinator() -> &'static Coordinator {
    COORDINATOR.get_or_init(Coordinator::new)
}

#[cfg(test)]
pub(super) fn reset_for_test() {
    *ORDINARY_RECEIPT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    ORDINARY_REVISION.store(0, std::sync::atomic::Ordering::Release);
    *coordinator()
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = State::default();
}

pub(super) fn update(
    edit: impl FnOnce(&Session) -> Option<Session>,
) -> Result<Option<Receipt>, AdmissionError> {
    coordinator().update(&EXECUTOR, edit, execute_write)
}

/// Admit a routine preference/roster edit and retain one receipt for explicit failure polling.
/// Replacing a pending receipt stays bounded: the newer admitted snapshot already contains the
/// older edit, and the coordinator's latest cell becomes the relevant durability verdict.
pub(super) fn update_ordinary(edit: impl FnOnce(&Session) -> Option<Session>) -> bool {
    coordinator().update_ordinary(&EXECUTOR, edit)
}

/// Main-loop hook: poll the one retained ordinary receipt and publish/log its terminal result.
/// No wait and no disk work; auth never calls this while holding its activation gate.
pub(super) fn poll_ordinary() -> Status {
    let receipt = ORDINARY_RECEIPT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some(mut receipt) = receipt {
        match receipt.poll() {
            Poll::Pending { .. } => {
                let revision = receipt.revision();
                let mut slot = ORDINARY_RECEIPT.lock().unwrap_or_else(|e| e.into_inner());
                if slot
                    .as_ref()
                    .is_none_or(|current| current.revision() < revision)
                {
                    *slot = Some(receipt);
                }
            }
            Poll::Complete(_) => {}
        }
    }
    status()
}

pub(super) fn ordinary_revision() -> u64 {
    ORDINARY_REVISION.load(std::sync::atomic::Ordering::Acquire)
}

pub(super) fn retry_ordinary(expected_revision: u64) -> bool {
    coordinator().retry_ordinary(expected_revision, &EXECUTOR)
}

pub(super) fn replace(
    snapshot: Session,
    authority: SaveAuthority,
) -> Result<Receipt, AdmissionError> {
    coordinator().replace(&EXECUTOR, snapshot, authority, execute_write)
}

pub(super) fn clear() -> Result<Receipt, AdmissionError> {
    coordinator().clear(&EXECUTOR, execute_clear)
}

pub(super) fn status() -> Status {
    coordinator().status()
}

fn execute_write(snapshot: Session, authority: SaveAuthority) -> DiskOutcome {
    reset_commit_detail();
    let outcome = super::save_with_authority_disk(&snapshot, authority);
    DiskOutcome::Write {
        outcome,
        commit: take_commit_detail(),
    }
}

fn execute_clear() -> DiskOutcome {
    reset_commit_detail();
    let outcome = super::clear_outcome_disk();
    let commit = take_commit_detail().unwrap_or(CommitDetail::Failed(StoreError::InvalidSchema));
    DiskOutcome::Clear { outcome, commit }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage_worker::{SubmitErrorGeneric, Writer};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct WriterExecutor {
        writer: Writer<Job, ()>,
    }

    impl WriterExecutor {
        fn start(capacity: usize) -> Self {
            Self {
                writer: Writer::start("session persistence test", capacity, |job: Job| job())
                    .unwrap(),
            }
        }
    }

    impl Submitter for WriterExecutor {
        fn submit(&self, job: Job) -> Result<(), SubmitError> {
            match self.writer.submit(job) {
                Ok(()) => Ok(()),
                Err(SubmitErrorGeneric::Full(_)) => Err(SubmitError::Full),
                Err(SubmitErrorGeneric::Stopped(_)) => Err(SubmitError::Stopped),
            }
        }
    }

    struct Refusing(SubmitError);

    impl Submitter for Refusing {
        fn submit(&self, _job: Job) -> Result<(), SubmitError> {
            Err(self.0)
        }
    }

    struct ConcurrentExecutor;

    impl Submitter for ConcurrentExecutor {
        fn submit(&self, job: Job) -> Result<(), SubmitError> {
            std::thread::Builder::new()
                .spawn(job)
                .map(|_| ())
                .map_err(|_| SubmitError::StartFailed)
        }
    }

    fn session(client: &str) -> Session {
        Session {
            client_id: client.into(),
            ..Session::default()
        }
    }

    fn b64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let n = (chunk[0] as u32) << 16
                | (*chunk.get(1).unwrap_or(&0) as u32) << 8
                | *chunk.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[(n >> (18 - i * 6)) as usize & 63] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    fn durable(snapshot: Session, _: SaveAuthority) -> DiskOutcome {
        let _ = snapshot;
        DiskOutcome::Write {
            outcome: PersistOutcome::PersistedPlaintext,
            commit: Some(CommitDetail::Durable),
        }
    }

    fn durable_clear() -> DiskOutcome {
        DiskOutcome::Clear {
            outcome: ClearOutcome {
                durability: super::super::ClearDurability::Durable,
                cleanup_failed: false,
            },
            commit: CommitDetail::Durable,
        }
    }

    fn install(snapshot: Session) {
        *CACHE.write().unwrap_or_else(|e| e.into_inner()) = Some(snapshot);
        LOCKED_STATE.store(NOT_LOCKED, Ordering::Relaxed);
    }

    #[test]
    fn admission_and_peek_do_not_wait_for_delayed_disk() {
        let _serial = crate::testlock::serial();
        install(session("before"));
        let coordinator = Coordinator::new();
        let executor = WriterExecutor::start(1);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut receipt = coordinator
            .update(
                &executor,
                |current| {
                    let mut next = current.clone();
                    next.client_id = "accepted".into();
                    Some(next)
                },
                move |snapshot, authority| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    durable(snapshot, authority)
                },
            )
            .unwrap()
            .unwrap();
        entered_rx.recv().unwrap();
        assert_eq!(super::super::peek().client_id, "accepted");
        assert_eq!(
            receipt.poll(),
            Poll::Pending {
                revision: receipt.revision()
            }
        );
        release_tx.send(()).unwrap();
        assert!(matches!(
            receipt.wait_blocking().outcome,
            CompletionOutcome::Durable(_)
        ));
    }

    #[test]
    fn rapid_field_edits_compose_from_the_latest_snapshot_and_finish_newest() {
        let _serial = crate::testlock::serial();
        install(session("client"));
        let coordinator = Coordinator::new();
        let executor = WriterExecutor::start(2);
        let persisted = Arc::new(Mutex::new(Vec::new()));
        let first_saved = persisted.clone();
        let first = coordinator
            .update(
                &executor,
                |current| {
                    let mut next = current.clone();
                    next.account_token = "account".into();
                    Some(next)
                },
                move |snapshot, authority| {
                    first_saved.lock().unwrap().push(snapshot.clone());
                    durable(snapshot, authority)
                },
            )
            .unwrap()
            .unwrap();
        let second_saved = persisted.clone();
        let second = coordinator
            .update(
                &executor,
                |current| {
                    let mut next = current.clone();
                    next.user.title = "viewer".into();
                    Some(next)
                },
                move |snapshot, authority| {
                    second_saved.lock().unwrap().push(snapshot.clone());
                    durable(snapshot, authority)
                },
            )
            .unwrap()
            .unwrap();
        first.wait_blocking();
        second.wait_blocking();
        let saved = persisted.lock().unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[1].account_token, "account");
        assert_eq!(saved[1].user.title, "viewer");
        assert_eq!(coordinator.status().latest, Some(LatestStatus::Durable));
    }

    #[test]
    fn stale_completion_cannot_clobber_newer_snapshot_or_status() {
        let _serial = crate::testlock::serial();
        install(session("initial"));
        let coordinator = Coordinator::new();
        let (release_tx, release_rx) = mpsc::channel();
        let first = coordinator
            .update(
                &ConcurrentExecutor,
                |current| {
                    let mut next = current.clone();
                    next.client_id = "older".into();
                    Some(next)
                },
                move |snapshot, authority| {
                    release_rx.recv().unwrap();
                    durable(snapshot, authority)
                },
            )
            .unwrap()
            .unwrap();
        let second = coordinator
            .update(
                &ConcurrentExecutor,
                |current| {
                    let mut next = current.clone();
                    next.client_id = "newer".into();
                    Some(next)
                },
                durable,
            )
            .unwrap()
            .unwrap();
        second.wait_blocking();
        release_tx.send(()).unwrap();
        first.wait_blocking();
        assert_eq!(super::super::peek().client_id, "newer");
        let status = coordinator.status();
        assert_eq!(status.latest_revision, 2);
        assert_eq!(status.latest, Some(LatestStatus::Durable));
        assert_eq!(status.durable_revision, Some(2));
    }

    #[test]
    fn write_clear_fresh_is_fifo_and_pre_clear_receipt_is_superseded() {
        let _serial = crate::testlock::serial();
        install(session("old"));
        let coordinator = Coordinator::new();
        let executor = WriterExecutor::start(3);
        let order = Arc::new(Mutex::new(Vec::new()));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first_order = order.clone();
        let first = coordinator
            .replace(
                &executor,
                session("old-write"),
                SaveAuthority::Routine,
                move |snapshot, authority| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    first_order.lock().unwrap().push("write-old");
                    durable(snapshot, authority)
                },
            )
            .unwrap();
        entered_rx.recv().unwrap();
        let stale_ran = Arc::new(AtomicBool::new(false));
        let stale_ran_job = stale_ran.clone();
        let stale = coordinator
            .update(
                &executor,
                |current| {
                    let mut next = current.clone();
                    next.user.title = "must-not-persist".into();
                    Some(next)
                },
                move |snapshot, authority| {
                    stale_ran_job.store(true, Ordering::Release);
                    durable(snapshot, authority)
                },
            )
            .unwrap()
            .unwrap();
        let clear_order = order.clone();
        let clear = coordinator
            .clear(&executor, move || {
                clear_order.lock().unwrap().push("clear");
                durable_clear()
            })
            .unwrap();
        assert!(super::super::peek().client_id.is_empty());
        let fresh_order = order.clone();
        let mut fresh_snapshot = session("fresh");
        fresh_snapshot.account_token = "fresh-account".into();
        let fresh = coordinator
            .replace(
                &executor,
                fresh_snapshot,
                SaveAuthority::FreshReauthentication,
                move |snapshot, authority| {
                    fresh_order.lock().unwrap().push("write-fresh");
                    durable(snapshot, authority)
                },
            )
            .unwrap();
        assert_eq!(super::super::peek().client_id, "fresh");
        release_tx.send(()).unwrap();
        assert_eq!(first.wait_blocking().outcome, CompletionOutcome::Superseded);
        assert_eq!(stale.wait_blocking().outcome, CompletionOutcome::Superseded);
        assert!(!stale_ran.load(Ordering::Acquire));
        assert!(matches!(
            clear.wait_blocking().outcome,
            CompletionOutcome::Durable(Operation::Clear { .. })
        ));
        assert!(matches!(
            fresh.wait_blocking().outcome,
            CompletionOutcome::Durable(Operation::Write(_))
        ));
        assert_eq!(
            &*order.lock().unwrap(),
            &["write-old", "clear", "write-fresh"]
        );
    }

    #[test]
    fn queue_refusal_is_explicit_and_does_not_publish_rejected_edit() {
        let _serial = crate::testlock::serial();
        install(session("kept"));
        let coordinator = Coordinator::new();
        let error = coordinator
            .update(
                &Refusing(SubmitError::Full),
                |current| {
                    let mut next = current.clone();
                    next.client_id = "rejected".into();
                    Some(next)
                },
                durable,
            )
            .unwrap_err();
        assert_eq!(error.revision, Some(1));
        assert_eq!(error.failure, AdmissionFailure::Queue(SubmitError::Full));
        assert_eq!(super::super::peek().client_id, "kept");
        assert_eq!(
            coordinator.status().latest,
            Some(LatestStatus::Failed(Failure::Admission(SubmitError::Full)))
        );
    }

    #[test]
    fn clear_refusal_keeps_memory_revoked_without_claiming_durability() {
        let _serial = crate::testlock::serial();
        install(session("signed-in"));
        let coordinator = Coordinator::new();
        let error = coordinator
            .clear(&Refusing(SubmitError::StartFailed), durable_clear)
            .unwrap_err();
        assert_eq!(error.revision, Some(1));
        assert!(super::super::peek().client_id.is_empty());
        assert_eq!(coordinator.status().durable_revision, None);
        assert!(matches!(
            coordinator.replace(
                &Refusing(SubmitError::Full),
                session("stale"),
                SaveAuthority::Routine,
                durable
            ),
            Err(AdmissionError {
                failure: AdmissionFailure::Revoked,
                ..
            })
        ));
    }

    #[test]
    fn empty_fresh_authority_cannot_reopen_a_cleared_tenure() {
        let _serial = crate::testlock::serial();
        install(session("signed-in"));
        let coordinator = Coordinator::new();
        let executor = WriterExecutor::start(2);
        let clear = coordinator.clear(&executor, durable_clear).unwrap();
        let error = coordinator
            .replace(
                &executor,
                session("empty-fresh"),
                SaveAuthority::FreshReauthentication,
                durable,
            )
            .unwrap_err();
        assert_eq!(error.failure, AdmissionFailure::Revoked);
        assert!(super::super::peek().account_token.is_empty());
        assert!(matches!(
            clear.wait_blocking().outcome,
            CompletionOutcome::Durable(Operation::Clear { .. })
        ));
    }

    #[test]
    fn rejected_clear_still_prevents_an_older_queued_write_from_running() {
        let _serial = crate::testlock::serial();
        install(session("signed-in"));
        let coordinator = Coordinator::new();
        let executor = WriterExecutor::start(1);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let running = coordinator
            .replace(
                &executor,
                session("already-running"),
                SaveAuthority::Routine,
                move |snapshot, authority| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    durable(snapshot, authority)
                },
            )
            .unwrap();
        entered_rx.recv().unwrap();
        let queued_ran = Arc::new(AtomicBool::new(false));
        let queued_ran_job = queued_ran.clone();
        let queued = coordinator
            .replace(
                &executor,
                session("queued"),
                SaveAuthority::Routine,
                move |snapshot, authority| {
                    queued_ran_job.store(true, Ordering::Release);
                    durable(snapshot, authority)
                },
            )
            .unwrap();
        let error = coordinator.clear(&executor, durable_clear).unwrap_err();
        assert_eq!(error.failure, AdmissionFailure::Queue(SubmitError::Full));
        assert!(super::super::peek().client_id.is_empty());
        release_tx.send(()).unwrap();
        assert_eq!(
            running.wait_blocking().outcome,
            CompletionOutcome::Superseded
        );
        assert_eq!(
            queued.wait_blocking().outcome,
            CompletionOutcome::Superseded
        );
        assert!(!queued_ran.load(Ordering::Acquire));
        assert_eq!(coordinator.status().durable_revision, None);
        assert_eq!(
            coordinator.status().latest,
            Some(LatestStatus::Failed(Failure::Admission(SubmitError::Full)))
        );
    }

    #[test]
    fn asynchronously_dropped_job_disconnects_receipt_and_clears_pending_status() {
        struct DropLater {
            sent: Mutex<Option<mpsc::Sender<Job>>>,
        }
        impl Submitter for DropLater {
            fn submit(&self, job: Job) -> Result<(), SubmitError> {
                self.sent
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .send(job)
                    .unwrap();
                Ok(())
            }
        }

        let _serial = crate::testlock::serial();
        install(session("before-drop"));
        let coordinator = Coordinator::new();
        let (tx, rx) = mpsc::channel::<Job>();
        let executor = DropLater {
            sent: Mutex::new(Some(tx)),
        };
        let receipt = coordinator
            .replace(
                &executor,
                session("accepted"),
                SaveAuthority::Routine,
                durable,
            )
            .unwrap();
        assert_eq!(coordinator.status().latest, Some(LatestStatus::Pending));
        let admission_held = coordinator.state.lock().unwrap();
        drop(rx.recv().unwrap());
        drop(admission_held);
        assert_eq!(
            receipt.wait_blocking().outcome,
            CompletionOutcome::Failed(Failure::WorkerDropped)
        );
        assert_eq!(
            coordinator.status().latest,
            Some(LatestStatus::Failed(Failure::WorkerDropped))
        );
    }

    #[test]
    fn shared_executor_runs_persistence_off_the_admitting_thread() {
        let _serial = crate::testlock::serial();
        install(session("shared"));
        let coordinator = Coordinator::new();
        let caller = std::thread::current().id();
        let ran = Arc::new(AtomicBool::new(false));
        let ran_job = ran.clone();
        let receipt = coordinator
            .replace(
                &SharedExecutor,
                session("shared-worker"),
                SaveAuthority::Routine,
                move |snapshot, authority| {
                    assert_ne!(std::thread::current().id(), caller);
                    ran_job.store(true, Ordering::Release);
                    durable(snapshot, authority)
                },
            )
            .unwrap();
        assert!(matches!(
            receipt.wait_blocking().outcome,
            CompletionOutcome::Durable(_)
        ));
        assert!(ran.load(Ordering::Acquire));
        crate::storage_worker::drain_for_test();
    }

    #[test]
    fn blocked_shared_disk_does_not_block_flag_reads_or_a_second_ui_admission() {
        let _serial = crate::testlock::serial();
        crate::storage_worker::drain_for_test();
        let dir = std::env::temp_dir().join(format!(
            "plxnative-session-blocked-ui-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        super::super::redirect_for_test(Some(dir.join("auth.json")));
        crate::keymanager::disarm_for_test();
        super::super::save(&session("blocked-client"));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let blocker = crate::storage_worker::submit(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        })
        .unwrap();
        entered_rx.recv().unwrap();
        let began = std::time::Instant::now();
        assert!(update_ordinary(|current| {
            let mut next = current.clone();
            next.user.title = "first-ui-event".into();
            Some(next)
        }));
        assert!(update_ordinary(|current| {
            let mut next = current.clone();
            next.server.name = "settings-event".into();
            Some(next)
        }));
        assert_eq!(super::super::snapshot().user.title, "first-ui-event");
        assert_eq!(super::super::snapshot().server.name, "settings-event");
        assert_eq!(status().latest, Some(LatestStatus::Pending));
        assert!(began.elapsed() < std::time::Duration::from_millis(250));
        release_tx.send(()).unwrap();
        blocker.wait_blocking().unwrap();
        crate::storage_worker::drain_for_test();
        let _ = poll_ordinary();
        super::super::redirect_for_test(None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn concurrent_ordinary_producers_retain_the_newest_revision_receipt() {
        let _serial = crate::testlock::serial();
        crate::storage_worker::drain_for_test();
        let dir = std::env::temp_dir().join(format!(
            "plxnative-session-ordinary-race-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        super::super::redirect_for_test(Some(dir.join("auth.json")));
        crate::keymanager::disarm_for_test();
        super::super::save(&session("ordinary-race"));
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let first_barrier = barrier.clone();
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            update_ordinary(|current| {
                let mut next = current.clone();
                next.user.title = "first".into();
                Some(next)
            })
        });
        let second_barrier = barrier.clone();
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            update_ordinary(|current| {
                let mut next = current.clone();
                next.server.name = "second".into();
                Some(next)
            })
        });
        barrier.wait();
        assert!(first.join().unwrap());
        assert!(second.join().unwrap());
        let status = status();
        assert_eq!(ordinary_revision(), status.latest_revision);
        assert_eq!(super::super::snapshot().user.title, "first");
        assert_eq!(super::super::snapshot().server.name, "second");
        crate::storage_worker::drain_for_test();
        let _ = poll_ordinary();
        super::super::redirect_for_test(None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn shared_worker_writes_canonical_session_and_a_cold_reopen_reads_it() {
        let _serial = crate::testlock::serial();
        crate::storage_worker::drain_for_test();
        let dir = std::env::temp_dir().join(format!(
            "plxnative-session-async-canonical-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        super::super::redirect_for_test(Some(dir.join("auth.json")));
        crate::keymanager::disarm_for_test();
        let coordinator = Coordinator::new();
        let mut snapshot = session("canonical-client");
        snapshot.account_token = "canonical-account".into();
        let receipt = coordinator
            .replace(
                &SharedExecutor,
                snapshot,
                SaveAuthority::Routine,
                execute_write,
            )
            .unwrap();
        assert!(matches!(
            receipt.wait_blocking().outcome,
            CompletionOutcome::Durable(Operation::Write(_))
        ));
        crate::storage_worker::drain_for_test();
        super::super::redirect_for_test(Some(dir.join("auth.json")));
        let reopened = super::super::load();
        assert_eq!(reopened.client_id, "canonical-client");
        assert_eq!(reopened.account_token, "canonical-account");
        crate::storage_worker::drain_for_test();
        super::super::redirect_for_test(None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn in_flight_cold_worker_prevents_a_second_sync_reader_crossing_the_boundary() {
        let _serial = crate::testlock::serial();
        crate::storage_worker::drain_for_test();
        let dir = std::env::temp_dir().join(format!(
            "plxnative-session-cold-owner-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join("auth.json");
        super::super::redirect_for_test(Some(legacy.clone()));
        let mut stored = session("cold-owner");
        stored.account_token = "cold-secret".into();
        std::fs::write(&legacy, serde_json::to_vec(&stored).unwrap()).unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let blocker = crate::storage_worker::submit(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        })
        .unwrap();
        entered_rx.recv().unwrap();
        let mut receipt = super::super::start_load().unwrap();
        assert!(super::super::load().account_token.is_empty());
        assert!(super::super::peek().account_token.is_empty());
        assert!(matches!(receipt.poll(), super::super::LoadPoll::Pending));
        release_tx.send(()).unwrap();
        blocker.wait_blocking().unwrap();
        crate::storage_worker::drain_for_test();
        match receipt.poll() {
            super::super::LoadPoll::Ready(loaded) => {
                assert_eq!(loaded.account_token, "cold-secret")
            }
            _ => panic!("the owning cold worker did not publish Ready"),
        }
        super::super::redirect_for_test(None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn dropped_or_refused_cold_job_releases_ownership_for_retry() {
        let _serial = crate::testlock::serial();
        crate::storage_worker::drain_for_test();
        super::super::COLD_LOAD_ACTIVE.store(false, Ordering::Release);
        let queued = super::super::prepare_cold_load().unwrap();
        assert!(super::super::COLD_LOAD_ACTIVE.load(Ordering::Acquire));
        assert!(matches!(
            super::super::prepare_cold_load(),
            Err(crate::storage_worker::SubmitError::Full)
        ));
        drop(queued);
        assert!(!super::super::COLD_LOAD_ACTIVE.load(Ordering::Acquire));
        let retry = super::super::prepare_cold_load().expect("retry owns the released boundary");
        drop(retry);
        assert!(!super::super::COLD_LOAD_ACTIVE.load(Ordering::Acquire));
    }

    #[test]
    fn shared_worker_seals_and_reopens_with_the_worker_scoped_keymanager_script() {
        struct Disarm;
        impl Drop for Disarm {
            fn drop(&mut self) {
                crate::storage_worker::drain_for_test();
                crate::keymanager::disarm_for_test();
            }
        }

        let _serial = crate::testlock::serial();
        let _disarm = Disarm;
        crate::storage_worker::drain_for_test();
        let dir = std::env::temp_dir().join(format!(
            "plxnative-session-async-sealed-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        super::super::redirect_for_test(Some(dir.join("auth.json")));
        super::super::write_proven_marker(crate::keymanager::Identity::Anonymous);
        let mut snapshot = session("sealed-client");
        snapshot.account_token = "sealed-account".into();
        let plain = serde_json::to_vec_pretty(&snapshot).unwrap();
        crate::keymanager::arm_worker_for_test(vec![
            ("generateKey", Ok(serde_json::json!({"returnValue": true}))),
            (
                "begin",
                Ok(serde_json::json!({
                    "returnValue": true, "handle": "h-enc", "iv": "MDEyMzQ1Njc4OWFi"
                })),
            ),
            (
                "finish",
                Ok(serde_json::json!({"returnValue": true, "output": "Y2lwaGVydGV4dA=="})),
            ),
            (
                "begin",
                Ok(serde_json::json!({"returnValue": true, "handle": "h-dec"})),
            ),
            (
                "finish",
                Ok(serde_json::json!({"returnValue": true, "output": b64(&plain)})),
            ),
        ]);
        let coordinator = Coordinator::new();
        let receipt = coordinator
            .replace(
                &SharedExecutor,
                snapshot,
                SaveAuthority::Routine,
                execute_write,
            )
            .unwrap();
        assert_eq!(
            receipt.wait_blocking().outcome,
            CompletionOutcome::Durable(Operation::Write(PersistOutcome::PersistedSealed))
        );
        assert_eq!(crate::keymanager::calls_for_test().len(), 5);

        crate::keymanager::arm_worker_for_test(vec![
            (
                "begin",
                Ok(serde_json::json!({"returnValue": true, "handle": "h-open"})),
            ),
            (
                "finish",
                Ok(serde_json::json!({"returnValue": true, "output": b64(&plain)})),
            ),
        ]);
        super::super::redirect_for_test(Some(dir.join("auth.json")));
        let mut load = super::super::start_load().unwrap();
        crate::storage_worker::drain_for_test();
        match load.poll() {
            super::super::LoadPoll::Ready(reopened) => {
                assert_eq!(reopened.client_id, "sealed-client");
                assert_eq!(reopened.account_token, "sealed-account");
            }
            _ => panic!("sealed worker load did not resolve ready"),
        }
        assert_eq!(crate::keymanager::calls_for_test().len(), 2);
        super::super::redirect_for_test(None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn uncertainty_keeps_stage_and_errno_and_is_not_durable() {
        let _serial = crate::testlock::serial();
        install(session("uncertain"));
        let coordinator = Coordinator::new();
        let receipt = coordinator
            .replace(
                &ConcurrentExecutor,
                session("uncertain-write"),
                SaveAuthority::Routine,
                |_, _| DiskOutcome::Write {
                    outcome: PersistOutcome::WriteFailed,
                    commit: Some(CommitDetail::Uncertain {
                        stage: CommitStage::ParentSync,
                        errno: libc::EIO,
                    }),
                },
            )
            .unwrap();
        assert_eq!(
            receipt.wait_blocking().outcome,
            CompletionOutcome::Uncertain {
                stage: CommitStage::ParentSync,
                errno: libc::EIO,
            }
        );
        let status = coordinator.status();
        assert_eq!(status.durable_revision, None);
        assert_eq!(
            status.latest,
            Some(LatestStatus::Uncertain {
                stage: CommitStage::ParentSync,
                errno: libc::EIO,
            })
        );
    }
}
