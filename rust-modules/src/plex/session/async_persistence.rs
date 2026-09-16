//! Nonblocking Session persistence admission and completion tracking.
//!
//! The production executor is the application's one bounded FIFO in [`crate::storage_worker`];
//! the local executors below exist only to prove lifecycle and revision behavior without a TV.

use super::persistence::{self, CanonicalCommit, ProtectionFailure};
use super::{SaveAuthority, Session};
use crate::storage::wire::{AuthPreservation, ProtectionOutcome};
use crate::storage::{CommitStage, StoreError};
use crate::storage_worker::SubmitError;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, OnceLock};

static CACHE: std::sync::RwLock<Option<Session>> = std::sync::RwLock::new(None);
static LOCKED_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
const NOT_LOCKED: u8 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersistOutcome {
    PersistedPlaintext,
    PersistedSealed,
    WriteFailed,
}
impl PersistOutcome {
    pub(crate) fn persisted(self) -> bool {
        self != Self::WriteFailed
    }
}

/// What a durable write is FOR. The caller uses this to refuse treating a background refresh as
/// proof that a login was saved; it is carried on admission and on the completion so the owner
/// never has to infer it from the outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum PersistencePurpose {
    Discovery,
    Final,
    Profile,
    Background,
}

impl PersistencePurpose {
    /// A background refresh is maintenance, never evidence that credentials reached disk.
    pub(crate) fn proves_saved_login(self) -> bool {
        matches!(self, Self::Discovery | Self::Final | Self::Profile)
    }
}
#[derive(Clone, Copy)]
enum ClearDurability {
    Durable,
    Uncertain,
    Failed,
}
#[derive(Clone, Copy)]
struct ClearOutcome {
    durability: ClearDurability,
    cleanup_failed: bool,
}

/// Ready snapshot publication is a load-completion step, never a draw-time disk read.
/// The owning adapter must finish bootstrap before admitting writes.
pub(crate) fn install_loaded(session: Session, locked: bool) -> Result<(), AdmissionError> {
    let state = coordinator()
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if state.revision != 0 || state.requires_fresh {
        return Err(AdmissionError {
            revision: None,
            failure: AdmissionFailure::Revoked,
        });
    }
    *CACHE.write().unwrap_or_else(|e| e.into_inner()) = Some(session);
    LOCKED_STATE.store(u8::from(locked), std::sync::atomic::Ordering::Release);
    Ok(())
}

pub(crate) fn snapshot() -> Option<Session> {
    CACHE.read().unwrap_or_else(|e| e.into_inner()).clone()
}

type Job = Box<dyn FnOnce() + Send + 'static>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    Write {
        outcome: PersistOutcome,
        verified: bool,
        protection: Option<ProtectionOutcome>,
    },
    Clear {
        cleanup_failed: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    Admission(SubmitError),
    Persistence(PersistOutcome),
    Storage(StoreError),
    Protection(ProtectionFailure),
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
    ProtectionUncertain(ProtectionFailure),
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
    purpose: PersistencePurpose,
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

    pub(crate) fn purpose(&self) -> PersistencePurpose {
        self.purpose
    }

    /// Attach owner-side correlation to this receipt's resolved verdict. `None` until the worker
    /// has actually answered, so a caller can never mistake pending for durable.
    pub(crate) fn typed(&self, correlation: PersistenceCorrelation) -> Option<PersistenceCompletion> {
        self.resolved.map(|completion| PersistenceCompletion {
            req: correlation.req,
            epoch: correlation.epoch,
            arrival: correlation.arrival,
            revision: self.revision,
            purpose: self.purpose,
            outcome: completion.outcome,
        })
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
    ProtectionUncertain(ProtectionFailure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub(crate) latest_revision: u64,
    pub(crate) latest: Option<LatestStatus>,
    pub(crate) durable_revision: Option<u64>,
    pub(crate) revocation_floor: Option<u64>,
}

/// Serializable projection of an [`AdmissionFailure`]. `SubmitError` belongs to the storage
/// worker's transport vocabulary and is deliberately not serialized into owner state, so the
/// owner-side typed rejection carries this closed, replay-safe enum instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum RejectionKind {
    /// Bounded FIFO had no capacity, or the executor was stopped/failed to start.
    Capacity,
    /// The same edit was already admitted; nothing new was enqueued.
    Duplicate,
    /// Protected fields were locked by storage protection.
    Locked,
    /// A public-only edit tried to change protected credentials.
    PublicOnly,
    /// No loaded snapshot to edit.
    Uninitialized,
    /// The process tenure was revoked; a fresh authorization is required.
    Revoked,
    /// The revision counter cannot advance.
    RevisionExhausted,
}

impl From<AdmissionFailure> for RejectionKind {
    fn from(failure: AdmissionFailure) -> Self {
        match failure {
            AdmissionFailure::Queue(_) => Self::Capacity,
            AdmissionFailure::Uninitialized => Self::Uninitialized,
            AdmissionFailure::Locked => Self::Locked,
            AdmissionFailure::Revoked => Self::Revoked,
            AdmissionFailure::RevisionExhausted => Self::RevisionExhausted,
        }
    }
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

/// Owner-side identity of one persistence decision. The completion repeats it so a late result
/// can be fenced by request, epoch, arrival AND revision before it activates anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PersistenceCorrelation {
    pub(crate) req: u32,
    pub(crate) epoch: u64,
    pub(crate) arrival: u64,
}

/// Typed answer to "was this authority current, and was durability even requested?". Replaces a
/// boolean that conflated currency with persistence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersistenceAdmission {
    /// The permit was not current: wrong epoch, wrong request/arrival, or an endpoint lifecycle
    /// that no longer matches. Nothing was written and no revision was consumed.
    StaleAuthority,
    /// Authority was current and the commit asked for no durable write (registry-only).
    AcceptedRegistryOnly,
    /// Authority was current and a durable write was admitted and enqueued. NOT yet durable.
    Admitted { revision: u64, purpose: PersistencePurpose },
    /// Authority was current but the queue/guards refused the write.
    Rejected { revision: Option<u64>, failure: AdmissionFailure },
}

impl PersistenceAdmission {
    pub(crate) fn accepted(self) -> bool {
        !matches!(self, Self::StaleAuthority | Self::Rejected { .. })
    }
    pub(crate) fn admitted_revision(self) -> Option<u64> {
        match self {
            Self::Admitted { revision, .. } => Some(revision),
            _ => None,
        }
    }
}

/// A typed durability verdict for one admitted revision, fenced by owner correlation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PersistenceCompletion {
    pub(crate) req: u32,
    pub(crate) epoch: u64,
    pub(crate) arrival: u64,
    pub(crate) revision: u64,
    pub(crate) purpose: PersistencePurpose,
    pub(crate) outcome: CompletionOutcome,
}

impl PersistenceCompletion {
    /// True only when this completion still describes the current authority and revision. A stale
    /// completion must not activate registry/profile, clear a newer error, or free another
    /// request's admission credit.
    pub(crate) fn acts_on(self, current: PersistenceCorrelation, latest_revision: u64) -> bool {
        self.req == current.req
            && self.epoch == current.epoch
            && self.arrival == current.arrival
            && self.revision == latest_revision
    }
    #[cfg(test)]
    fn with_req(mut self, req: u32) -> Self { self.req = req; self }
    #[cfg(test)]
    fn with_epoch(mut self, epoch: u64) -> Self { self.epoch = epoch; self }
    #[cfg(test)]
    fn with_arrival(mut self, arrival: u64) -> Self { self.arrival = arrival; self }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommitDetail {
    Durable,
    Uncertain { stage: CommitStage, errno: i32 },
    Failed(StoreError),
}

#[derive(Clone, Copy)]
enum DiskOutcome {
    ProtectionFailed(ProtectionFailure),
    Write {
        outcome: PersistOutcome,
        verified: bool,
        protection: Option<ProtectionOutcome>,
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
            Self::ProtectionFailed(evidence) => {
                if evidence.preservation == AuthPreservation::Uncertain {
                    CompletionOutcome::ProtectionUncertain(evidence)
                } else {
                    CompletionOutcome::Failed(Failure::Protection(evidence))
                }
            }
            Self::Write {
                outcome,
                verified,
                protection,
                commit,
            } => match commit {
                Some(CommitDetail::Uncertain { stage, errno }) => {
                    CompletionOutcome::Uncertain { stage, errno }
                }
                Some(CommitDetail::Failed(error)) => {
                    CompletionOutcome::Failed(Failure::Storage(error))
                }
                Some(CommitDetail::Durable) | None if outcome.persisted() => {
                    CompletionOutcome::Durable(Operation::Write {
                        outcome,
                        verified,
                        protection,
                    })
                }
                _ => CompletionOutcome::Failed(Failure::Persistence(outcome)),
            },
            Self::Clear { outcome, commit } => match outcome.durability {
                ClearDurability::Durable => match commit {
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
                ClearDurability::Uncertain => match commit {
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
                ClearDurability::Failed => match commit {
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
    dirty_authority: SaveAuthority,
    latest: Option<Arc<Mutex<LatestStatus>>>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            revision: 0,
            durable_revision: None,
            revocation_floor: None,
            requires_fresh: false,
            dirty_authority: SaveAuthority::PublicOnly,
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
        self.admit_with(
            executor,
            SaveAuthority::Routine,
            PersistencePurpose::Background,
            |current| Ok(edit(current)),
            persist,
        )
    }

    fn admit_with(
        &self,
        executor: &dyn Submitter,
        authority: SaveAuthority,
        purpose: PersistencePurpose,
        edit: impl FnOnce(&Session) -> Result<Option<Session>, AdmissionFailure>,
        persist: impl FnOnce(Session, SaveAuthority) -> DiskOutcome + Send + 'static,
    ) -> Result<Option<Receipt>, AdmissionError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.requires_fresh && authority != SaveAuthority::FreshReauthentication {
            return Err(AdmissionError {
                revision: None,
                failure: AdmissionFailure::Revoked,
            });
        }
        if authority == SaveAuthority::Routine
            && LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed) != NOT_LOCKED
        {
            return Err(AdmissionError {
                revision: None,
                failure: AdmissionFailure::Locked,
            });
        }
        let current = CACHE.read().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(current) = current.filter(|session| {
            authority == SaveAuthority::FreshReauthentication || !session.client_id.is_empty()
        }) else {
            return Err(AdmissionError {
                revision: None,
                failure: AdmissionFailure::Uninitialized,
            });
        };
        let Some(next) = edit(&current).map_err(|failure| AdmissionError {
            revision: None,
            failure,
        })?
        else {
            return Ok(None);
        };
        if authority == SaveAuthority::PublicOnly && !super::protected_fields_equal(&current, &next)
        {
            return Err(AdmissionError {
                revision: None,
                failure: AdmissionFailure::Locked,
            });
        }
        if authority == SaveAuthority::FreshReauthentication && next.account_token.is_empty() {
            return Err(AdmissionError {
                revision: None,
                failure: AdmissionFailure::Revoked,
            });
        }
        let revision = next_revision(&mut state)?;
        let command_snapshot = next.clone();
        let receipt = self.submit_locked(&mut state, revision, purpose, executor, move || {
            persist(command_snapshot, authority)
        })?;
        if authority == SaveAuthority::FreshReauthentication {
            state.requires_fresh = false;
            LOCKED_STATE.store(NOT_LOCKED, std::sync::atomic::Ordering::Release);
        }
        *CACHE.write().unwrap_or_else(|e| e.into_inner()) = Some(next);
        Ok(Some(receipt))
    }

    /// ONE short consistent operation: check the caller's permit (the `edit` closure is the
    /// owner's fenced patch application), merge against the LATEST in-memory snapshot, and enqueue.
    /// No I/O and no recursion happen here; the worker performs durability.
    fn admit_typed_with(
        &self,
        executor: &dyn Submitter,
        authority: SaveAuthority,
        purpose: PersistencePurpose,
        writes_durable: bool,
        edit: impl FnOnce(&Session) -> Result<Option<Session>, AdmissionFailure>,
        persist: impl FnOnce(Session, SaveAuthority) -> DiskOutcome + Send + 'static,
    ) -> PersistenceAdmission {
        // A registry-only commit consumes no persistence revision and performs no durable write.
        if !writes_durable {
            return PersistenceAdmission::AcceptedRegistryOnly;
        }
        if let Err(error) = self.admit_with(executor, authority, purpose, edit, persist) {
            // An authority refusal (not-current permit) is categorically different from capacity,
            // duplicate, lock, or public-only refusal. Both used to be one `false`.
            if matches!(error.failure, AdmissionFailure::Revoked) {
                return PersistenceAdmission::StaleAuthority;
            }
            return PersistenceAdmission::Rejected {
                revision: error.revision,
                failure: error.failure,
            };
        }
        PersistenceAdmission::Admitted { revision: self.status().latest_revision, purpose }
    }

    fn update_ordinary(
        &self,
        executor: &dyn Submitter,
        authority: SaveAuthority,
        edit: impl FnOnce(&Session) -> Option<Session>,
    ) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.requires_fresh
            || (authority != SaveAuthority::PublicOnly
                && LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed) != NOT_LOCKED)
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
        if authority == SaveAuthority::PublicOnly && !super::protected_fields_equal(&current, &next)
        {
            return false;
        }
        let Ok(revision) = next_revision(&mut state) else {
            return false;
        };
        // Keep the latest dirty intent even if bounded FIFO admission fails. Retry reads this
        // snapshot, and preserves Routine authority if an earlier dirty protected edit exists.
        if authority == SaveAuthority::Routine {
            state.dirty_authority = authority;
        }
        let authority = state.dirty_authority;
        *CACHE.write().unwrap_or_else(|e| e.into_inner()) = Some(next.clone());
        let command = next;
        let receipt = self.submit_locked(&mut state, revision, PersistencePurpose::Background,
            executor, move || {
            execute_write(command, authority)
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
        let authority = state.dirty_authority;
        let receipt = self.submit_locked(&mut state, revision, PersistencePurpose::Background,
            executor, move || {
            execute_write(snapshot, authority)
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
        state.dirty_authority = SaveAuthority::PublicOnly;
        install_ordinary_receipt(0, None);
        // Runtime revocation is immediate even when the bounded queue cannot accept durability.
        *CACHE.write().unwrap_or_else(|e| e.into_inner()) = Some(Session::default());
        self.submit_locked(&mut state, revision, PersistencePurpose::Background, executor, persist)
    }

    fn submit_locked(
        &self,
        state: &mut State,
        revision: u64,
        purpose: PersistencePurpose,
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
            purpose,
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
            if self.revision == state.revision {
                state.dirty_authority = SaveAuthority::PublicOnly;
            }
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
            CompletionOutcome::Durable(_)
            | CompletionOutcome::Superseded
            | CompletionOutcome::ProtectionUncertain(_) => {}
        }
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = match outcome {
            CompletionOutcome::Durable(_) => LatestStatus::Durable,
            CompletionOutcome::Uncertain { stage, errno } => {
                LatestStatus::Uncertain { stage, errno }
            }
            CompletionOutcome::Failed(failure) => LatestStatus::Failed(failure),
            CompletionOutcome::ProtectionUncertain(evidence) => {
                LatestStatus::ProtectionUncertain(evidence)
            }
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
pub(crate) fn reset_for_test() {
    *ORDINARY_RECEIPT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    ORDINARY_REVISION.store(0, std::sync::atomic::Ordering::Release);
    *coordinator()
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = State::default();
}

pub(crate) fn update(
    edit: impl FnOnce(&Session) -> Option<Session>,
) -> Result<Option<Receipt>, AdmissionError> {
    coordinator().update(&EXECUTOR, edit, execute_write)
}

/// Validate the owner's permit, merge its patch with the latest snapshot, and enqueue under
/// one short coordinator lock. The closure must not perform I/O or call this module recursively.
pub(crate) fn admit(
    authority: SaveAuthority,
    permit_and_edit: impl FnOnce(&Session) -> Result<Option<Session>, AdmissionFailure>,
) -> Result<Option<Receipt>, AdmissionError> {
    coordinator().admit_with(&EXECUTOR, authority, PersistencePurpose::Background,
        permit_and_edit, execute_write)
}

/// Typed variant of [`admit`]. Reports whether the permit was current, whether durability was
/// even requested, and — when it was — the admitted revision and purpose.
pub(crate) fn admit_typed(
    authority: SaveAuthority,
    purpose: PersistencePurpose,
    writes_durable: bool,
    permit_and_edit: impl FnOnce(&Session) -> Result<Option<Session>, AdmissionFailure>,
) -> PersistenceAdmission {
    coordinator().admit_typed_with(&EXECUTOR, authority, purpose, writes_durable,
        permit_and_edit, execute_write)
}

/// Admit a routine preference/roster edit and retain one receipt for explicit failure polling.
/// Replacing a pending receipt stays bounded: the newer admitted snapshot already contains the
/// older edit, and the coordinator's latest cell becomes the relevant durability verdict.
pub(crate) fn update_ordinary(edit: impl FnOnce(&Session) -> Option<Session>) -> bool {
    coordinator().update_ordinary(&EXECUTOR, SaveAuthority::PublicOnly, edit)
}

pub(crate) fn update_protected_ordinary(edit: impl FnOnce(&Session) -> Option<Session>) -> bool {
    coordinator().update_ordinary(&EXECUTOR, SaveAuthority::Routine, edit)
}

/// Main-loop hook: poll the one retained ordinary receipt and publish/log its terminal result.
/// No wait and no disk work; auth never calls this while holding its activation gate.
pub(crate) fn poll_ordinary() -> Status {
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

pub(crate) fn ordinary_revision() -> u64 {
    ORDINARY_REVISION.load(std::sync::atomic::Ordering::Acquire)
}

pub(crate) fn retry_ordinary(expected_revision: u64) -> bool {
    coordinator().retry_ordinary(expected_revision, &EXECUTOR)
}

pub(crate) fn clear() -> Result<Receipt, AdmissionError> {
    clear_after(|| true)
}

/// One shared FIFO operation: resource cleanup runs before the account-wide ClearTenure.
/// Consent may delegate to this receipt; it must not announce independent durability.
///
/// **Not yet wired to the live sign-out path, and that is a decision, not an oversight.**
/// `plex::session::clear()` is the entry point `app::adapters::session::erase()` actually calls,
/// and it stays a synchronous, [`super::IO`]-locked implementation — `commit_cleared` then, on a
/// durable commit, `persistence::cleanup_after_confirmed_clear` — for the same reason `save()`'s
/// live write path is still synchronous: routing sign-out through this coordinator would admit it
/// into the same FIFO ordering as queued ordinary writes, which is the right eventual shape but
/// needs the main loop polling a `Receipt` the way `poll_ordinary` does for writes, not a caller
/// that blocks on it (`Receipt::wait_blocking` is `#[cfg(test)]`-only by design: "production
/// UI/auth callers poll"). That polling integration is a separate, larger change than this
/// package's scope. Until it lands, `clear`/`clear_after` remain exercised only by this module's
/// own tests — deliberately kept rather than deleted, since `execute_clear`'s sequencing (clear,
/// then sweep legacy candidates only once the clear is confirmed durable) is the shape the live
/// path independently converged on, and the next package that adds the async handoff should
/// delegate to this rather than re-derive it.
pub(crate) fn clear_after(
    cleanup: impl FnOnce() -> bool + Send + 'static,
) -> Result<Receipt, AdmissionError> {
    coordinator().clear(&EXECUTOR, move || {
        let cleanup_failed = !cleanup();
        let mut result = execute_clear();
        if let DiskOutcome::Clear { outcome, .. } = &mut result {
            outcome.cleanup_failed |= cleanup_failed;
        }
        result
    })
}

pub(crate) fn start_bootstrap(
    opener: Box<dyn persistence::LegacyOpener + Send>,
) -> Result<crate::storage_worker::TypedTicket<persistence::Bootstrap>, SubmitError> {
    crate::storage_worker::submit(move || {
        let mut opener = opener;
        persistence::bootstrap(&mut *opener)
    })
}

pub(crate) fn status() -> Status {
    coordinator().status()
}

fn execute_write(snapshot: Session, authority: SaveAuthority) -> DiskOutcome {
    match persistence::write_session(&snapshot, authority) {
        CanonicalCommit::Durable {
            protection,
            verified,
            ..
        } => DiskOutcome::Write {
            protection,
            verified,
            outcome: if protection.is_some_and(|outcome| {
                outcome.class == crate::storage::wire::ProtectionClass::Keymanager
            }) {
                PersistOutcome::PersistedSealed
            } else {
                PersistOutcome::PersistedPlaintext
            },
            commit: Some(CommitDetail::Durable),
        },
        CanonicalCommit::Uncertain { stage, errno } => DiskOutcome::Write {
            outcome: PersistOutcome::WriteFailed,
            verified: false,
            protection: None,
            commit: Some(CommitDetail::Uncertain { stage, errno }),
        },
        CanonicalCommit::ProtectionFailed(evidence) => DiskOutcome::ProtectionFailed(evidence),
        CanonicalCommit::Failed(error) => DiskOutcome::Write {
            outcome: PersistOutcome::WriteFailed,
            verified: false,
            protection: None,
            commit: Some(CommitDetail::Failed(error)),
        },
    }
}

fn execute_clear() -> DiskOutcome {
    let (durability, commit) = match persistence::commit_cleared() {
        CanonicalCommit::Durable { .. } => (ClearDurability::Durable, CommitDetail::Durable),
        CanonicalCommit::Uncertain { stage, errno } => (
            ClearDurability::Uncertain,
            CommitDetail::Uncertain { stage, errno },
        ),
        CanonicalCommit::Failed(error) => (ClearDurability::Failed, CommitDetail::Failed(error)),
        CanonicalCommit::ProtectionFailed(evidence) => {
            return DiskOutcome::ProtectionFailed(evidence)
        }
    };
    let cleanup_failed = matches!(durability, ClearDurability::Durable)
        && !persistence::cleanup_after_confirmed_clear();
    DiskOutcome::Clear {
        outcome: ClearOutcome {
            durability,
            cleanup_failed,
        },
        commit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage_worker::{SubmitErrorGeneric, Writer};
    use std::sync::atomic::{AtomicBool, Ordering};

    impl Coordinator {
        /// Lifecycle fixtures submit a complete synthetic value through the production guards.
        /// No production snapshot-replacement API exists beside permit-and-merge admission.
        fn admit_snapshot(
            &self,
            executor: &dyn Submitter,
            snapshot: Session,
            authority: SaveAuthority,
            persist: impl FnOnce(Session, SaveAuthority) -> DiskOutcome + Send + 'static,
        ) -> Result<Receipt, AdmissionError> {
            self.admit_with(executor, authority, PersistencePurpose::Final,
                |_| Ok(Some(snapshot)), persist)
                .map(|receipt| receipt.expect("fixture always proposes a change"))
        }
    }

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

    fn durable(snapshot: Session, _: SaveAuthority) -> DiskOutcome {
        let _ = snapshot;
        DiskOutcome::Write {
            outcome: PersistOutcome::PersistedPlaintext,
            verified: true,
            protection: None,
            commit: Some(CommitDetail::Durable),
        }
    }

    fn durable_clear() -> DiskOutcome {
        DiskOutcome::Clear {
            outcome: ClearOutcome {
                durability: ClearDurability::Durable,
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
        assert_eq!(snapshot().unwrap_or_default().client_id, "accepted");
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
        assert_eq!(snapshot().unwrap_or_default().client_id, "newer");
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
            .admit_snapshot(
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
        assert!(snapshot().unwrap_or_default().client_id.is_empty());
        let fresh_order = order.clone();
        let mut fresh_snapshot = session("fresh");
        fresh_snapshot.account_token = "fresh-account".into();
        let fresh = coordinator
            .admit_snapshot(
                &executor,
                fresh_snapshot,
                SaveAuthority::FreshReauthentication,
                move |snapshot, authority| {
                    fresh_order.lock().unwrap().push("write-fresh");
                    durable(snapshot, authority)
                },
            )
            .unwrap();
        assert_eq!(snapshot().unwrap_or_default().client_id, "fresh");
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
            CompletionOutcome::Durable(Operation::Write { .. })
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
        assert_eq!(snapshot().unwrap_or_default().client_id, "kept");
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
        assert!(snapshot().unwrap_or_default().client_id.is_empty());
        assert_eq!(coordinator.status().durable_revision, None);
        assert!(matches!(
            coordinator.admit_snapshot(
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
            .admit_snapshot(
                &executor,
                session("empty-fresh"),
                SaveAuthority::FreshReauthentication,
                durable,
            )
            .unwrap_err();
        assert_eq!(error.failure, AdmissionFailure::Revoked);
        assert!(snapshot().unwrap_or_default().account_token.is_empty());
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
            .admit_snapshot(
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
            .admit_snapshot(
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
        assert!(snapshot().unwrap_or_default().client_id.is_empty());
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
            .admit_snapshot(
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
            .admit_snapshot(
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
    fn refused_ordinary_edit_retains_dirty_snapshot_for_explicit_retry() {
        let _serial = crate::testlock::serial();
        install(session("dirty"));
        let coordinator = Coordinator::new();
        assert!(!coordinator.update_ordinary(
            &Refusing(SubmitError::Full),
            SaveAuthority::PublicOnly,
            |current| {
                let mut next = current.clone();
                next.auto_sign_in = true;
                Some(next)
            }
        ));
        assert!(
            snapshot().unwrap().auto_sign_in,
            "queue refusal must not erase a user's dirty preference"
        );
    }

    #[test]
    fn public_ordinary_edit_is_allowed_when_auth_is_locked() {
        let _serial = crate::testlock::serial();
        install(session("locked-public"));
        LOCKED_STATE.store(1, Ordering::Release);
        let coordinator = Coordinator::new();
        // Refusing executor proves admission reached the FIFO without doing I/O here.
        coordinator.update_ordinary(
            &Refusing(SubmitError::Full),
            SaveAuthority::PublicOnly,
            |current| {
                let mut next = current.clone();
                next.auto_sign_in = true;
                Some(next)
            },
        );
        assert_eq!(
            coordinator.status().latest,
            Some(LatestStatus::Failed(Failure::Admission(SubmitError::Full)))
        );
        assert!(snapshot().unwrap().auto_sign_in);
    }

    #[test]
    fn rejected_owner_permit_does_not_enqueue_or_publish_a_patch() {
        let _serial = crate::testlock::serial();
        install(session("permitted-snapshot"));
        let coordinator = Coordinator::new();
        let result = coordinator.admit_with(
            &Refusing(SubmitError::Full),
            SaveAuthority::Routine,
            PersistencePurpose::Final,
            |_| Err(AdmissionFailure::Revoked),
            durable,
        );
        assert!(matches!(
            result,
            Err(AdmissionError {
                revision: None,
                failure: AdmissionFailure::Revoked
            })
        ));
        assert_eq!(coordinator.status().latest_revision, 0);
        assert_eq!(snapshot().unwrap().client_id, "permitted-snapshot");
    }
    /// ACCEPTANCE SPEC 1/5. An admission is authority currency, not durability, and the typed
    /// admission must be able to say so. Registry-only commits request no durable write and must
    /// therefore never be reported as an admitted persistence revision.
    #[test]
    fn typed_admission_separates_registry_only_from_a_durable_revision() {
        let _serial = crate::testlock::serial();
        install(session("client"));
        let coordinator = Coordinator::new();
        let executor = WriterExecutor::start(1);

        // (a) authority current, no durable write requested -> NOT an admitted revision.
        let registry_only = coordinator.admit_typed_with(
            &executor,
            SaveAuthority::Routine,
            PersistencePurpose::Profile,
            false,
            |_| Ok(None),
            durable,
        );
        assert_eq!(
            registry_only,
            PersistenceAdmission::AcceptedRegistryOnly,
            "a registry-only commit must be typed as accepted without persistence"
        );
        assert_eq!(coordinator.status().durable_revision, None);
        assert_eq!(
            coordinator.status().latest_revision,
            0,
            "a registry-only commit consumes no persistence revision"
        );

        // (b) a durable write requested -> ADMITTED, still not durable. The write is gated so
        // the verdict cannot race the assertion: admission must not imply durability.
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let admitted = coordinator.admit_typed_with(
            &executor,
            SaveAuthority::Routine,
            PersistencePurpose::Final,
            true,
            |_| Ok(Some(session("durable-edit"))),
            move |snapshot, authority| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                durable(snapshot, authority)
            },
        );
        let PersistenceAdmission::Admitted { revision, purpose } = admitted else {
            panic!("a requested durable write must be reported as admitted, got {admitted:?}");
        };
        assert_eq!(purpose, PersistencePurpose::Final, "the purpose must survive admission");
        assert_eq!(revision, 1);
        entered_rx.recv().unwrap();
        assert_eq!(
            coordinator.status().durable_revision, None,
            "enqueue is not durability"
        );
        assert_eq!(
            coordinator.status().latest, Some(LatestStatus::Pending),
            "an admitted revision reports Pending, never Durable, before the worker answers"
        );

        // (c) a stale authority is its own typed answer, never a queue refusal.
        let stale = coordinator.admit_typed_with(
            &executor,
            SaveAuthority::Routine,
            PersistencePurpose::Final,
            true,
            |_| Err(AdmissionFailure::Revoked),
            durable,
        );
        assert_eq!(stale, PersistenceAdmission::StaleAuthority);
        assert_eq!(
            coordinator.status().latest_revision,
            1,
            "a stale authority must not consume a revision"
        );
        release_tx.send(()).unwrap();
        assert!(matches!(
            coordinator.status().latest_revision,
            1
        ));

        // (d) capacity refusal carries its own typed rejection.
        let refused = Coordinator::new().admit_typed_with(
            &Refusing(SubmitError::Full),
            SaveAuthority::Routine,
            PersistencePurpose::Background,
            true,
            |_| Ok(Some(session("refused"))),
            durable,
        );
        assert_eq!(
            refused,
            PersistenceAdmission::Rejected {
                revision: Some(1),
                failure: AdmissionFailure::Queue(SubmitError::Full),
            }
        );
    }

    /// ACCEPTANCE SPEC 5. A stale completion must not be mistaken for the current operation's
    /// durability verdict: the typed completion carries request/epoch/arrival/revision so the
    /// owner can fence it before it activates anything.
    #[test]
    fn typed_completion_is_fenced_by_request_epoch_and_revision() {
        let current = PersistenceCorrelation { req: 7, epoch: 3, arrival: 12 };
        let completion = PersistenceCompletion {
            req: 7,
            epoch: 3,
            arrival: 12,
            revision: 4,
            purpose: PersistencePurpose::Final,
            outcome: CompletionOutcome::Durable(Operation::Write {
                outcome: PersistOutcome::PersistedPlaintext,
                verified: true,
                protection: None,
            }),
        };
        assert!(completion.acts_on(current, 4), "the current revision is actionable");
        assert!(!completion.acts_on(current, 5), "a newer revision supersedes this completion");
        assert!(
            !completion
                .with_arrival(11)
                .acts_on(current, 4),
            "a wrong arrival must not clear a newer error"
        );
        assert!(
            !completion.with_epoch(4).acts_on(current, 4),
            "a stale epoch must not activate registry or profile"
        );
        assert!(
            !completion.with_req(8).acts_on(current, 4),
            "a completion must not free another request's admission credit"
        );
    }

    /// ACCEPTANCE SPEC 2. FreshReauthentication is the only key to a cleared/locked tenure and
    /// must not be obtainable through an ordinary retry: the retry path re-writes at most the
    /// dirty Routine/PublicOnly authority and can never mint fresh authority.
    #[test]
    fn ordinary_retry_can_never_acquire_fresh_reauthentication() {
        let _serial = crate::testlock::serial();
        install(session("signed-in"));
        let coordinator = Coordinator::new();
        let executor = WriterExecutor::start(3);
        coordinator.update_ordinary(&executor, SaveAuthority::Routine, |current| {
            let mut next = current.clone();
            next.account_token = "dirty".into();
            Some(next)
        });
        let dirty_authority = coordinator.state.lock().unwrap().dirty_authority;
        assert_eq!(dirty_authority, SaveAuthority::Routine,
            "an ordinary edit records Routine, never fresh authority");
        // A clear revokes the process tenure and demands fresh authority.
        let clear = coordinator.clear(&executor, durable_clear).unwrap();
        clear.wait_blocking();
        assert!(coordinator.state.lock().unwrap().requires_fresh);
        let revision = {
            let state = coordinator.state.lock().unwrap();
            state.revision
        };
        // The ordinary retry path stays closed...
        assert!(!coordinator.retry_ordinary(revision, &executor),
            "a retry must not re-acquire fresh authority");
        // ...and only an explicit fresh authorization is admitted.
        let mut fresh = session("fresh");
        fresh.account_token = "fresh-token".into();
        assert!(coordinator
            .admit_snapshot(&executor, fresh, SaveAuthority::FreshReauthentication, durable)
            .is_ok());
        assert!(!coordinator.state.lock().unwrap().requires_fresh);
    }

    /// ACCEPTANCE SPEC 3. A background refresh must never be usable as proof that a login was
    /// saved; discovery/final/profile may be.
    #[test]
    fn background_purpose_cannot_prove_a_saved_login() {
        assert!(!PersistencePurpose::Background.proves_saved_login());
        assert!(PersistencePurpose::Discovery.proves_saved_login());
        assert!(PersistencePurpose::Final.proves_saved_login());
        assert!(PersistencePurpose::Profile.proves_saved_login());
    }

    #[test]
    fn snapshot_admission_cannot_bypass_public_only_or_locked_auth_guards() {
        let _serial = crate::testlock::serial();
        for authority in [SaveAuthority::PublicOnly, SaveAuthority::Routine] {
            install(session("before"));
            LOCKED_STATE.store(
                u8::from(authority == SaveAuthority::Routine),
                Ordering::Release,
            );
            let coordinator = Coordinator::new();
            let mut changed = session("before");
            changed.account_token = "unauthorized-change".into();
            let executor = WriterExecutor::start(1);
            let result = coordinator.admit_snapshot(&executor, changed, authority, durable);
            assert!(matches!(
                result,
                Err(AdmissionError {
                    failure: AdmissionFailure::Locked,
                    ..
                })
            ));
            assert_eq!(coordinator.status().latest_revision, 0);
            assert!(snapshot().unwrap().account_token.is_empty());
        }
    }
}
