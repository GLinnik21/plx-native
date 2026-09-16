//! Canonical consent persistence and the one-time legacy migration.
//!
//! The record backend owns framing and filesystem durability. This adapter owns the consent JSON
//! payload, revision choice, and the rule that a present canonical record is terminal: legacy
//! files are consulted only when `consent.json` is genuinely absent.

use super::consent::Consent;
use crate::storage::{Record, RecordKey, RecordState, RecordStore};
#[cfg(not(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test))))]
use crate::storage::StoreError;
use std::path::{Path, PathBuf};

#[cfg(any(
    all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test)),
    test
))]
use crate::storage::{
    client::{self, Load as HelperLoad},
    state::{self, Generation, MigrationProgress},
    wire::{CommitStatus, Domain, MigrationMutation, Response, WireMutation},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // `Delegated` is produced only by the shipping webOS adapter.
pub(crate) enum PersistResult {
    NotAttempted,
    /// Durability belongs to the immediately-following atomic Session `ClearTenure` operation.
    Delegated,
    Durable,
    Uncertain,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CleanupResult {
    NotAttempted,
    Complete,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PersistOutcome {
    pub(crate) write: PersistResult,
    pub(crate) cleanup: CleanupResult,
}

static LAST_OUTCOME: std::sync::Mutex<PersistOutcome> = std::sync::Mutex::new(PersistOutcome {
    write: PersistResult::Failed,
    cleanup: CleanupResult::NotAttempted,
});

fn publish(outcome: PersistOutcome) {
    *LAST_OUTCOME.lock().unwrap_or_else(|e| e.into_inner()) = outcome;
}

#[cfg(test)]
pub(crate) fn last_outcome() -> PersistOutcome {
    *LAST_OUTCOME.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
pub(crate) fn redirect_root_for_test(root: Option<PathBuf>) {
    crate::paths::redirect_persistent_state_root_for_test(root);
}

/// Which thread most recently ran the blocking disk/storage-helper work in [`record`] or
/// [`forget`]. Exists only to let a test prove the *caller* of those functions never blocks on
/// them directly — see `app::adapters::consent`'s `commit_live`/`forget_live` off-thread tests.
#[cfg(test)]
static LAST_CALL_THREAD: std::sync::Mutex<Option<std::thread::ThreadId>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
pub(crate) fn last_call_thread() -> Option<std::thread::ThreadId> {
    *LAST_CALL_THREAD.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
fn note_call_thread() {
    *LAST_CALL_THREAD.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::thread::current().id());
}

fn root() -> PathBuf {
    crate::paths::persistent_state_root()
}

/// Snapshot the canonical destination before an asynchronous operation is queued. In production
/// it is stable for the process; tests redirect it, so resolving it on the worker would let an old
/// operation write into a later fixture.
/// Persist a decision to the canonical record and, once that is durable, retire the legacy files.
///
/// This is blocking disk/storage-helper I/O — a genuine round trip on the television. Callers
/// must run it off the frame thread (`crate::storage_worker`), never inline from a dispatch path;
/// see `app::adapters::consent::commit_live`.
pub(crate) fn record(consent: &Consent) -> PersistOutcome {
    #[cfg(test)]
    note_call_thread();
    record_at(consent, &super::resource_candidates(), root())
}

/// End the account's tenure over consent: a canonical Cleared tombstone, so a later load cannot
/// resurrect the previous decision from the canonical record or from a reappeared legacy file.
///
/// Blocking, for the same reason as [`record`]; see `app::adapters::consent::forget_live`.
pub(crate) fn forget() -> PersistOutcome {
    #[cfg(test)]
    note_call_thread();
    forget_at(&super::resource_candidates(), root())
}

#[cfg(not(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test))))]
fn store() -> Result<impl RecordStore, StoreError> {
    crate::storage::open(root())
}

#[cfg(not(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test))))]
fn store_at(root: PathBuf) -> Result<impl RecordStore, StoreError> {
    crate::storage::open(root)
}

#[cfg(not(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test))))]
fn cleanup_canonical(store: &impl RecordStore) -> CleanupResult {
    if store.cleanup(RecordKey::Consent).is_ok() {
        CleanupResult::Complete
    } else {
        CleanupResult::Failed
    }
}

fn cleanup_failure_message(stage: &str, path: &Path, error: &std::io::Error) -> String {
    format!(
        "telemetry: consent cleanup failed stage={stage} errno={} target={}",
        error.raw_os_error().unwrap_or(0),
        path.file_name().unwrap_or_default().to_string_lossy()
    )
}

fn log_cleanup_failure(stage: &str, path: &Path, error: &std::io::Error) {
    crate::log(&cleanup_failure_message(stage, path, error));
}

/// Sweep telemetry/consent's own legacy candidates once the shared account ClearTenure DB8
/// mutation has been confirmed durable. [`forget_at`]'s ARM branch defers exactly this cleanup
/// here rather than doing it inline, because the immediately-following session ClearTenure is the
/// one atomic DB8 revocation for both domains and legacy sources must survive until THAT commit is
/// confirmed, not merely queued.
#[cfg(any(test, all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"))))]
pub(crate) fn cleanup_after_combined_clear(legacy: &[PathBuf]) -> CleanupResult {
    remove_legacy_sources(legacy.iter().cloned())
}

fn remove_legacy_sources(paths: impl IntoIterator<Item = PathBuf>) -> CleanupResult {
    let mut parents = std::collections::BTreeSet::new();
    let mut result = CleanupResult::Complete;
    for path in paths {
        match crate::storage::remove_file_or_prove_absent(&path) {
            Ok(crate::storage::RemoveDisposition::Removed) => {
                if let Some(parent) = path.parent() {
                    parents.insert(parent.to_path_buf());
                }
            }
            Ok(crate::storage::RemoveDisposition::Absent) => {}
            Err(error) => {
                log_cleanup_failure("remove_legacy", &path, &error);
                result = CleanupResult::Failed;
            }
        }
    }
    for parent in parents {
        match std::fs::File::open(&parent).and_then(|directory| directory.sync_all()) {
            Ok(()) => {}
            Err(error) => {
                log_cleanup_failure("sync_legacy_parent", &parent, &error);
                result = CleanupResult::Failed;
            }
        }
    }
    result
}

/// Load canonical consent, falling back to trusted legacy candidates only when canonical is
/// absent. A malformed, inaccessible, future, or otherwise present canonical record is terminal.
pub(crate) fn load(legacy: &[PathBuf]) -> Consent {
    #[cfg(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test)))]
    {
        return load_helper(&mut client::NativeTransport, legacy);
    }
    #[cfg(not(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test))))]
    {
    #[cfg(not(test))]
    if crate::paths::ensure_persistent_state_root().is_err() {
        publish(PersistOutcome {
            write: PersistResult::Failed,
            cleanup: CleanupResult::NotAttempted,
        });
        return Consent::default();
    }
    let canonical_root = root();
    let canonical_exists = match std::fs::symlink_metadata(&canonical_root) {
        Ok(meta) if meta.file_type().is_dir() => true,
        Ok(_) | Err(_) => {
            publish(PersistOutcome {
                write: PersistResult::Failed,
                cleanup: CleanupResult::NotAttempted,
            });
            return Consent::default();
        }
    };
    if canonical_exists {
        let Ok(store) = store() else {
            publish(PersistOutcome {
                write: PersistResult::Failed,
                cleanup: CleanupResult::NotAttempted,
            });
            return Consent::default();
        };
        return match store.load(RecordKey::Consent) {
            Ok(None) => load_legacy_and_migrate(&store, legacy),
            Ok(Some(Record {
                state: RecordState::Cleared,
                ..
            })) => {
                publish(PersistOutcome {
                    write: PersistResult::NotAttempted,
                    cleanup: cleanup_canonical(&store),
                });
                Consent::default()
            }
            Ok(Some(Record {
                state: RecordState::Data { payload },
                ..
            })) => {
                let cleanup = cleanup_canonical(&store);
                match serde_json::from_str::<Consent>(&payload) {
                    Ok(consent) => {
                        publish(PersistOutcome {
                            write: PersistResult::NotAttempted,
                            cleanup,
                        });
                        super::consent::migrate_loaded(consent)
                    }
                    Err(_) => {
                        publish(PersistOutcome {
                            write: PersistResult::Failed,
                            cleanup,
                        });
                        Consent::default()
                    }
                }
            }
            Err(_) => {
                publish(PersistOutcome {
                    write: PersistResult::Failed,
                    cleanup: cleanup_canonical(&store),
                });
                Consent::default()
            }
        };
    }
    let Ok(store) = store() else {
        publish(PersistOutcome {
            write: PersistResult::Failed,
            cleanup: CleanupResult::NotAttempted,
        });
        return Consent::default();
    };
    load_legacy_and_migrate(&store, legacy)
    }
}

#[cfg(any(
    all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test)),
    test
))]
fn helper_expected(snapshot: &client::Snapshot) -> Option<(&str, state::Expected)> {
    Some((&snapshot.db_rev, snapshot.state.expected()))
}

#[cfg(any(
    all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test)),
    test
))]
#[derive(Clone, Copy)]
struct HelperCommit {
    result: PersistResult,
    verified: bool,
    /// The helper refused a stale read revision; nothing was written.
    conflict: bool,
}

#[cfg(any(
    all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test)),
    test
))]
fn helper_commit(
    transport: &mut dyn client::Transport,
    loaded: &HelperLoad,
    mutation: WireMutation,
) -> HelperCommit {
    let operation = match Generation::random() {
        Ok(operation) => operation,
        Err(_) => {
            return HelperCommit {
                result: PersistResult::Failed,
                verified: false,
                conflict: false,
            }
        }
    };
    let expected = match loaded {
        HelperLoad::Missing => None,
        HelperLoad::Present(snapshot) => helper_expected(snapshot),
    };
    match client::commit_with(transport, expected, operation, mutation) {
        Ok(Response::Commit {
            status: CommitStatus::Committed,
            applied: Some(_),
            verified,
            ..
        }) => HelperCommit {
            result: PersistResult::Durable,
            verified,
            conflict: false,
        },
        Ok(Response::Reconcile {
            status: crate::storage::wire::ReconcileStatus::Applied,
            applied: Some(_),
            ..
        }) => HelperCommit {
            result: PersistResult::Durable,
            verified: false,
            conflict: false,
        },
        Ok(Response::Commit {
            status: CommitStatus::Unavailable,
            ..
        })
        | Ok(Response::Reconcile {
            status: crate::storage::wire::ReconcileStatus::Unknown,
            ..
        }) => HelperCommit {
            result: PersistResult::Uncertain,
            verified: false,
            conflict: false,
        },
        Ok(Response::Commit {
            status: CommitStatus::Conflict,
            ..
        }) => HelperCommit {
            result: PersistResult::Failed,
            verified: false,
            conflict: true,
        },
        _ => HelperCommit {
            result: PersistResult::Failed,
            verified: false,
            conflict: false,
        },
    }
}

#[cfg(any(
    all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test)),
    test
))]
fn legacy_consent(legacy: &[PathBuf]) -> Result<Option<(Consent, Vec<PathBuf>)>, ()> {
    let mut found: Option<(Consent, Vec<PathBuf>)> = None;
    for path in legacy {
        match read_legacy(path) {
            LegacyRead::Missing => {}
            LegacyRead::Valid(bytes, candidate) => {
                // The bytes were read and trust-checked by `read_legacy`; the DB8 migration stores
                // the typed split, while the legacy JSON backend below preserves the exact bytes.
                let _ = bytes.len();
                if let Some((existing, sources)) = &mut found {
                    if existing != &candidate {
                        return Err(());
                    }
                    sources.push(path.clone());
                } else {
                    found = Some((candidate, vec![path.clone()]));
                }
            }
            LegacyRead::Untrusted | LegacyRead::Invalid => return Err(()),
        }
    }
    Ok(found)
}

#[cfg(any(
    all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test)),
    test
))]
enum PreviousCanonical {
    Missing,
    Data(Consent),
    Cleared,
    Blocked,
}

/// Decode the complete `plxnative-record` wrapper used by the JSON canonical store that preceded
/// DB8. A corrupt/future record is terminal and is never treated like an absent legacy source.
#[cfg(any(
    all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test)),
    test
))]
fn previous_canonical_consent() -> PreviousCanonical {
    previous_canonical_consent_at(root())
}

#[cfg(any(
    all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test)),
    test
))]
fn previous_canonical_consent_at(root: PathBuf) -> PreviousCanonical {
    match std::fs::symlink_metadata(&root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => PreviousCanonical::Missing,
        Err(_) => PreviousCanonical::Blocked,
        Ok(metadata) if !metadata.is_dir() => PreviousCanonical::Blocked,
        Ok(_) => {
            let Ok(store) = crate::storage::open(root) else {
                return PreviousCanonical::Blocked;
            };
            match store.load(RecordKey::Consent) {
                Ok(None) => PreviousCanonical::Missing,
                Ok(Some(Record {
                    state: RecordState::Cleared,
                    ..
                })) => PreviousCanonical::Cleared,
                Ok(Some(Record {
                    state: RecordState::Data { payload },
                    ..
                })) => serde_json::from_str::<Consent>(&payload)
                    .map(super::consent::migrate_loaded)
                    .map(PreviousCanonical::Data)
                    .unwrap_or(PreviousCanonical::Blocked),
                Err(_) => PreviousCanonical::Blocked,
            }
        }
    }
}

#[cfg(any(
    all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test)),
    test
))]
fn load_helper(transport: &mut dyn client::Transport, legacy: &[PathBuf]) -> Consent {
    let loaded = match client::load_with(transport) {
        Ok(loaded) => loaded,
        Err(_) => {
            publish(PersistOutcome {
                write: PersistResult::Failed,
                cleanup: CleanupResult::NotAttempted,
            });
            return Consent::default();
        }
    };
    if let HelperLoad::Present(snapshot) = &loaded {
        if snapshot.state.migrations.consent.progress == MigrationProgress::Complete {
            let slots = state::ConsentPayload {
                consent: snapshot.state.public.consent.clone(),
                scopes: snapshot.state.public.scopes.clone(),
                ids: snapshot.state.public.ids.clone(),
            };
            let consent = if slots.consent.is_null() && slots.scopes.is_null() && slots.ids.is_null()
            {
                Consent::default()
            } else {
                match super::consent::join_canonical(&slots) {
                    Ok(consent) => super::consent::migrate_loaded(consent),
                    Err(()) => {
                        publish(PersistOutcome {
                            write: PersistResult::Failed,
                            cleanup: CleanupResult::NotAttempted,
                        });
                        return Consent::default();
                    }
                }
            };
            // A verified DB8 migration makes the old files non-authoritative, but it does not
            // prove that their earlier unlink succeeded.  Retry retirement on every cold load so
            // a temporary jail/filesystem failure cannot leave an account decision behind while
            // later launches falsely advertise cleanup as complete.
            let cleanup = remove_legacy_sources(
                std::iter::once(root().join("consent.json")).chain(legacy.iter().cloned()),
            );
            publish(PersistOutcome {
                write: PersistResult::NotAttempted,
                cleanup,
            });
            return consent;
        }
    }
    let previous_path = root().join("consent.json");
    let previous = previous_canonical_consent();
    if matches!(previous, PreviousCanonical::Blocked) {
        publish(PersistOutcome {
            write: PersistResult::Failed,
            cleanup: CleanupResult::NotAttempted,
        });
        return Consent::default();
    }
    if matches!(previous, PreviousCanonical::Cleared) {
        let commit = helper_commit(
            transport,
            &loaded,
            WireMutation::AdvanceMigration {
                migration: MigrationMutation::CompleteEmpty {
                    domain: Domain::Consent,
                },
            },
        );
        let cleanup = if commit.result == PersistResult::Durable && commit.verified {
            remove_legacy_sources(
                std::iter::once(previous_path).chain(legacy.iter().cloned()),
            )
        } else {
            CleanupResult::NotAttempted
        };
        publish(PersistOutcome {
            write: commit.result,
            cleanup,
        });
        return Consent::default();
    }
    let found = match previous {
        PreviousCanonical::Data(consent) => Some((consent, vec![previous_path])),
        PreviousCanonical::Missing => match legacy_consent(legacy) {
            Ok(found) => found,
            Err(()) => {
                // Invalid, untrusted and temporarily unreadable legacy sources are deliberately
                // unresolved.  Recording CompleteEmpty here would make a transient EACCES (or a
                // conflicting second candidate) permanent and the valid decision would never be
                // reconsidered on a later launch.
                publish(PersistOutcome {
                    write: PersistResult::Failed,
                    cleanup: CleanupResult::NotAttempted,
                });
                return Consent::default();
            }
        },
        PreviousCanonical::Cleared | PreviousCanonical::Blocked => unreachable!(),
    };
    let (consent, sources, mutation) = match found {
        Some((consent, sources)) => {
            let slots = match super::consent::split_canonical(&consent) {
                Ok(slots) => slots,
                Err(()) => {
                    publish(PersistOutcome {
                        write: PersistResult::Failed,
                        cleanup: CleanupResult::NotAttempted,
                    });
                    return Consent::default();
                }
            };
            let value = match serde_json::to_value(slots) {
                Ok(value) => value,
                Err(_) => {
                    publish(PersistOutcome {
                        write: PersistResult::Failed,
                        cleanup: CleanupResult::NotAttempted,
                    });
                    return Consent::default();
                }
            };
            (
                consent,
                sources,
                WireMutation::AdvanceMigration {
                    migration: MigrationMutation::ConsentComplete { consent: value },
                },
            )
        }
        None => (
            Consent::default(),
            Vec::new(),
            WireMutation::AdvanceMigration {
                migration: MigrationMutation::CompleteEmpty {
                    domain: Domain::Consent,
                },
            },
        ),
    };
    let commit = helper_commit(transport, &loaded, mutation);
    let cleanup = if commit.result == PersistResult::Durable && commit.verified {
        remove_legacy_sources(sources)
    } else {
        CleanupResult::NotAttempted
    };
    publish(PersistOutcome {
        write: commit.result,
        cleanup,
    });
    consent
}

enum LegacyRead {
    Missing,
    Valid(Vec<u8>, Consent),
    Untrusted,
    Invalid,
}

fn read_legacy(path: &Path) -> LegacyRead {
    match crate::storage::read_owned_bytes(path) {
        Ok(None) => LegacyRead::Missing,
        Ok(Some((bytes, trusted))) if trusted => {
            let Ok(consent) = serde_json::from_slice::<Consent>(&bytes) else {
                return LegacyRead::Invalid;
            };
            LegacyRead::Valid(bytes, super::consent::migrate_loaded(consent))
        }
        Ok(Some(_)) => LegacyRead::Untrusted,
        Err(_) => LegacyRead::Invalid,
    }
}

#[cfg(not(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test))))]
fn load_legacy_and_migrate(store: &impl RecordStore, legacy: &[PathBuf]) -> Consent {
    let mut found: Option<(Consent, Vec<(PathBuf, Vec<u8>)>)> = None;
    for path in legacy {
        let loaded = match read_legacy(path) {
            LegacyRead::Valid(bytes, candidate) => Some((bytes, candidate)),
            LegacyRead::Missing => None,
            LegacyRead::Untrusted => {
                let barrier = store.commit(RecordKey::Consent, &Record::cleared(1));
                let write = match barrier {
                    Ok(crate::storage::CommitReceipt::Durable) => PersistResult::Durable,
                    Ok(crate::storage::CommitReceipt::Uncertain { .. }) => PersistResult::Uncertain,
                    Err(_) => PersistResult::Failed,
                };
                let cleanup = if write == PersistResult::Durable {
                    if store.cleanup(RecordKey::Consent).is_ok() {
                        CleanupResult::Complete
                    } else {
                        CleanupResult::Failed
                    }
                } else {
                    CleanupResult::NotAttempted
                };
                publish(PersistOutcome { write, cleanup });
                return Consent::default();
            }
            LegacyRead::Invalid => {
                publish(PersistOutcome {
                    write: PersistResult::Failed,
                    cleanup: CleanupResult::NotAttempted,
                });
                return Consent::default();
            }
        };
        let Some((bytes, candidate)) = loaded else {
            continue;
        };
        if let Some((old, sources)) = &mut found {
            if old != &candidate {
                publish(PersistOutcome {
                    write: PersistResult::Failed,
                    cleanup: CleanupResult::NotAttempted,
                });
                return Consent::default();
            }
            sources.push((path.clone(), bytes));
        } else {
            found = Some((candidate, vec![(path.clone(), bytes)]));
        }
    }
    let Some((consent, sources)) = found else {
        publish(PersistOutcome {
            write: PersistResult::NotAttempted,
            cleanup: cleanup_canonical(store),
        });
        return Consent::default();
    };
    let Some((_, payload)) = sources.first() else {
        return consent;
    };
    let Ok(payload) = String::from_utf8(payload.clone()) else {
        publish(PersistOutcome {
            write: PersistResult::Failed,
            cleanup: CleanupResult::NotAttempted,
        });
        return Consent::default();
    };
    let record = Record::data(1, payload);
    let write = match store.commit(RecordKey::Consent, &record) {
        Ok(crate::storage::CommitReceipt::Durable) => PersistResult::Durable,
        Ok(crate::storage::CommitReceipt::Uncertain { .. }) => PersistResult::Uncertain,
        Err(_) => PersistResult::Failed,
    };
    if write != PersistResult::Durable {
        publish(PersistOutcome {
            write,
            cleanup: CleanupResult::NotAttempted,
        });
        return consent;
    }
    let temp_cleanup = if store.cleanup(RecordKey::Consent).is_ok() {
        CleanupResult::Complete
    } else {
        CleanupResult::Failed
    };
    let source_cleanup = remove_legacy_sources(sources.into_iter().map(|(path, _)| path));
    let cleanup =
        if temp_cleanup == CleanupResult::Failed || source_cleanup == CleanupResult::Failed {
            CleanupResult::Failed
        } else if temp_cleanup == CleanupResult::NotAttempted {
            source_cleanup
        } else {
            CleanupResult::Complete
        };
    publish(PersistOutcome {
        write: PersistResult::Durable,
        cleanup,
    });
    consent
}

/// Persist a typed decision canonically and retire stale legacy copies after a durable commit.
#[cfg(test)]
pub(crate) fn record_with_legacy(consent: &Consent, legacy: &[PathBuf]) -> PersistResult {
    record_at(consent, legacy, root()).write
}

pub(super) fn record_at(
    consent: &Consent,
    legacy: &[PathBuf],
    canonical_root: PathBuf,
) -> PersistOutcome {
    #[cfg(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test)))]
    {
        let _ = canonical_root;
        return record_helper(&mut client::NativeTransport, consent, legacy);
    }
    #[cfg(not(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test))))]
    {
    let store = match store_at(canonical_root) {
        Ok(store) => store,
        Err(_) => {
            let outcome = PersistOutcome {
                write: PersistResult::Failed,
                cleanup: CleanupResult::NotAttempted,
            };
            publish(outcome);
            return outcome;
        }
    };
    let revision = match store.load(RecordKey::Consent) {
        Ok(Some(record)) => match record.revision.checked_add(1) {
            Some(revision) => revision,
            None => {
                let outcome = PersistOutcome {
                    write: PersistResult::Failed,
                    cleanup: CleanupResult::NotAttempted,
                };
                publish(outcome);
                return outcome;
            }
        },
        Ok(None) => 1,
        Err(StoreError::InvalidSchema) => 1,
        Err(
            StoreError::UnknownFormat
            | StoreError::UnsupportedVersion
            | StoreError::DomainKeyMismatch,
        ) => {
            let outcome = PersistOutcome {
                write: PersistResult::Failed,
                cleanup: CleanupResult::NotAttempted,
            };
            publish(outcome);
            return outcome;
        }
        Err(_) => {
            let outcome = PersistOutcome {
                write: PersistResult::Failed,
                cleanup: CleanupResult::NotAttempted,
            };
            publish(outcome);
            return outcome;
        }
    };
    let payload = match serde_json::to_string(consent) {
        Ok(payload) => payload,
        Err(_) => {
            let outcome = PersistOutcome {
                write: PersistResult::Failed,
                cleanup: CleanupResult::NotAttempted,
            };
            publish(outcome);
            return outcome;
        }
    };
    let result = match store.commit(RecordKey::Consent, &Record::data(revision, payload)) {
        Ok(crate::storage::CommitReceipt::Durable) => PersistResult::Durable,
        Ok(crate::storage::CommitReceipt::Uncertain { .. }) => PersistResult::Uncertain,
        Err(_) => PersistResult::Failed,
    };
    let temp_cleanup = if result == PersistResult::Durable {
        if store.cleanup(RecordKey::Consent).is_ok() {
            CleanupResult::Complete
        } else {
            CleanupResult::Failed
        }
    } else {
        CleanupResult::NotAttempted
    };
    let source_cleanup = if result == PersistResult::Durable {
        remove_legacy_sources(legacy.iter().cloned())
    } else {
        CleanupResult::NotAttempted
    };
    let cleanup =
        if temp_cleanup == CleanupResult::Failed || source_cleanup == CleanupResult::Failed {
            CleanupResult::Failed
        } else if temp_cleanup == CleanupResult::NotAttempted {
            source_cleanup
        } else {
            CleanupResult::Complete
        };
    let outcome = PersistOutcome {
        write: result,
        cleanup,
    };
    publish(outcome);
    outcome
    }
}

#[cfg(any(
    all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test)),
    test
))]
fn record_helper(
    transport: &mut dyn client::Transport,
    consent: &Consent,
    legacy: &[PathBuf],
) -> PersistOutcome {
    let loaded = match client::load_with(transport) {
        Ok(loaded) => loaded,
        Err(_) => {
            let outcome = PersistOutcome {
                write: PersistResult::Failed,
                cleanup: CleanupResult::NotAttempted,
            };
            publish(outcome);
            return outcome;
        }
    };
    let slots = match super::consent::split_canonical(consent) {
        Ok(slots) => slots,
        Err(()) => {
            let outcome = PersistOutcome {
                write: PersistResult::Failed,
                cleanup: CleanupResult::NotAttempted,
            };
            publish(outcome);
            return outcome;
        }
    };
    let payload = match serde_json::to_value(slots) {
        Ok(payload) => payload,
        Err(_) => {
            let outcome = PersistOutcome {
                write: PersistResult::Failed,
                cleanup: CleanupResult::NotAttempted,
            };
            publish(outcome);
            return outcome;
        }
    };
    let mut commit = helper_commit(
        transport,
        &loaded,
        WireMutation::UpdateConsent {
            payload: payload.clone(),
        },
    );
    if commit.conflict {
        // Session commits run on their own worker, so one can land between this read and write.
        // The consent update replaces only its own slots: retry once at the fresh revision, but
        // only within the same tenure. A sign-out or fresh sign-in moved the epoch/auth
        // generation, and this decision must not be written into the next account's record.
        if let Ok(reloaded) = client::load_with(transport) {
            let tenure = |load: &HelperLoad| match load {
                HelperLoad::Missing => None,
                HelperLoad::Present(snapshot) => Some(snapshot.state.expected()),
            };
            if tenure(&loaded) == tenure(&reloaded) {
                commit = helper_commit(transport, &reloaded, WireMutation::UpdateConsent { payload });
            }
        }
    }
    let cleanup = if commit.result == PersistResult::Durable && commit.verified {
        remove_legacy_sources(legacy.iter().cloned())
    } else {
        CleanupResult::NotAttempted
    };
    let outcome = PersistOutcome {
        write: commit.result,
        cleanup,
    };
    publish(outcome);
    outcome
}

/// Write a canonical cleared tombstone, then remove stale legacy copies best-effort.
pub(super) fn forget_at(legacy: &[PathBuf], canonical_root: PathBuf) -> PersistOutcome {
    #[cfg(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test)))]
    {
        let _ = canonical_root;
        // Session's immediately-following ClearTenure is the one atomic DB8 revocation for both
        // domains. Consent has already been unpublished before this worker job runs. Legacy
        // sources remain until that one transaction is confirmed durable.
        let _ = legacy;
        let outcome = PersistOutcome {
            write: PersistResult::Delegated,
            cleanup: CleanupResult::NotAttempted,
        };
        publish(outcome);
        return outcome;
    }
    #[cfg(not(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test))))]
    {
    let store = match store_at(canonical_root) {
        Ok(store) => store,
        Err(_) => {
            let outcome = PersistOutcome {
                write: PersistResult::Failed,
                cleanup: CleanupResult::NotAttempted,
            };
            publish(outcome);
            return outcome;
        }
    };
    let revision = match store.load(RecordKey::Consent).ok().flatten() {
        Some(record) => match record.revision.checked_add(1) {
            Some(revision) => revision,
            None => {
                let outcome = PersistOutcome {
                    write: PersistResult::Failed,
                    cleanup: CleanupResult::NotAttempted,
                };
                publish(outcome);
                return outcome;
            }
        },
        None => 1,
    };
    let result = match store.commit(RecordKey::Consent, &Record::cleared(revision)) {
        Ok(crate::storage::CommitReceipt::Durable) => PersistResult::Durable,
        Ok(crate::storage::CommitReceipt::Uncertain { .. }) => PersistResult::Uncertain,
        Err(_) => PersistResult::Failed,
    };
    let temp_cleanup = if result == PersistResult::Durable {
        if store.cleanup(RecordKey::Consent).is_ok() {
            CleanupResult::Complete
        } else {
            CleanupResult::Failed
        }
    } else {
        CleanupResult::NotAttempted
    };
    let source_cleanup = remove_legacy_sources(legacy.iter().cloned());
    let cleanup =
        if temp_cleanup == CleanupResult::Failed || source_cleanup == CleanupResult::Failed {
            CleanupResult::Failed
        } else if temp_cleanup == CleanupResult::NotAttempted {
            source_cleanup
        } else {
            temp_cleanup
        };
    let outcome = PersistOutcome {
        write: result,
        cleanup,
    };
    publish(outcome);
    outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::JsonStore;
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;

    struct Fixture {
        dir: PathBuf,
        root: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "plxnative-consent-persistence-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            let root = dir.join("state");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            redirect_root_for_test(Some(root.clone()));
            Self { dir, root }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            redirect_root_for_test(None);
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn legacy_path(f: &Fixture, name: &str) -> PathBuf {
        f.dir.join(name)
    }

    fn old_yes() -> Consent {
        Consent {
            asked_version: 4,
            errors: true,
            usage: true,
            errors_id: Some("e".repeat(32)),
            install_id: Some("u".repeat(32)),
            errors_scope: 4,
            usage_scope: 4,
            errors_declined_scope: 2,
            usage_declined_scope: 3,
            extensions: BTreeMap::from([(
                "future_field".to_owned(),
                serde_json::json!({"opaque": "value"}),
            )]),
        }
    }

    /// Copilot review on PR #105, finding 7. `release/v0.6` swept telemetry/consent's own legacy
    /// files (`telemetry::cleanup_after_account_clear` / `persistence::cleanup_after_combined_clear`)
    /// once the shared account ClearTenure DB8 mutation was confirmed durable on ARM sign-out; the
    /// 0.7 forward-port dropped both the function and its only caller entirely (`forget_at`'s ARM
    /// branch still says it relies on this sweep, but nothing performed it — confirmed absent by
    /// grep against this tree before this fix). ARM's `forget_at` is compiled out on host
    /// (`not(all(target_os = "linux", target_arch = "arm", ...))`), so the real defect cannot be
    /// observed red on this platform; this test pins the restored sweep function directly rather
    /// than through the ARM-only call site. `cleanup_after_combined_clear` did not exist before
    /// this fix, so the red here is simulated (a compile failure against the old tree), not an
    /// observed runtime failure.
    #[test]
    fn cleanup_after_combined_clear_removes_every_legacy_candidate() {
        let _g = crate::testlock::serial();
        let fixture = Fixture::new("combined-clear");
        let a = legacy_path(&fixture, "a-telemetry.json");
        let b = legacy_path(&fixture, "b-telemetry.json");
        std::fs::write(&a, b"{}").unwrap();
        std::fs::write(&b, b"{}").unwrap();
        let missing = legacy_path(&fixture, "missing-telemetry.json");

        let result = cleanup_after_combined_clear(&[a.clone(), b.clone(), missing]);

        assert_eq!(result, CleanupResult::Complete);
        assert!(!a.exists());
        assert!(!b.exists());
    }

    #[test]
    fn genuine_cleanup_diagnostic_names_stage_errno_and_safe_target() {
        let error = std::io::Error::from_raw_os_error(libc::EROFS);
        assert_eq!(
            cleanup_failure_message(
                "remove_legacy",
                Path::new("/media/internal/telemetry.json"),
                &error,
            ),
            format!(
                "telemetry: consent cleanup failed stage=remove_legacy errno={} target=telemetry.json",
                libc::EROFS
            )
        );
    }

    #[test]
    fn migration_preserves_ids_declines_and_exact_legacy_payload() {
        let _g = crate::testlock::serial();
        let fixture = Fixture::new("migration");
        let source = legacy_path(&fixture, "telemetry.json");
        let bytes = serde_json::to_vec(&old_yes()).unwrap();
        std::fs::write(&source, &bytes).unwrap();
        let loaded = load(std::slice::from_ref(&source));
        assert_eq!(
            loaded.errors_id.as_deref(),
            Some("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee")
        );
        assert_eq!(
            loaded.install_id.as_deref(),
            Some("uuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuu")
        );
        assert_eq!(loaded.errors_declined_scope, 2);
        assert_eq!(
            loaded.extensions.get("future_field"),
            old_yes().extensions.get("future_field")
        );
        let store = JsonStore::new(fixture.root.clone()).unwrap();
        let Some(Record {
            state: RecordState::Data { payload },
            ..
        }) = store.load(RecordKey::Consent).unwrap()
        else {
            panic!("migration did not write canonical data");
        };
        assert_eq!(payload.as_bytes(), bytes.as_slice());
        assert!(
            !source.exists(),
            "legacy source survived a durable migration"
        );
    }

    #[test]
    fn previous_json_wrapper_yields_nested_consent_and_tombstone() {
        let _g = crate::testlock::serial();
        let fixture = Fixture::new("prior-wrapper");
        let store = JsonStore::new(fixture.root.clone()).unwrap();
        assert_eq!(
            store.commit(
                RecordKey::Consent,
                &Record::data(4, serde_json::to_string(&old_yes()).unwrap()),
            ),
            Ok(crate::storage::CommitReceipt::Durable)
        );
        match previous_canonical_consent_at(fixture.root.clone()) {
            PreviousCanonical::Data(consent) => assert_eq!(consent, old_yes()),
            _ => panic!("the wrapper was not decoded as Consent data"),
        }
        assert_eq!(
            store.commit(RecordKey::Consent, &Record::cleared(5)),
            Ok(crate::storage::CommitReceipt::Durable)
        );
        assert!(matches!(
            previous_canonical_consent_at(fixture.root.clone()),
            PreviousCanonical::Cleared
        ));
    }

    #[test]
    fn canonical_cleared_tombstone_beats_an_old_legacy_yes_after_reboot() {
        let _g = crate::testlock::serial();
        let fixture = Fixture::new("cleared");
        let source = legacy_path(&fixture, "telemetry.json");
        std::fs::write(&source, serde_json::to_vec(&old_yes()).unwrap()).unwrap();
        let store = JsonStore::new(fixture.root.clone()).unwrap();
        assert_eq!(
            store.commit(RecordKey::Consent, &Record::cleared(9)),
            Ok(crate::storage::CommitReceipt::Durable)
        );
        let loaded = load(std::slice::from_ref(&source));
        assert!(!loaded.any() && !loaded.answered());
        assert!(
            source.exists(),
            "stale legacy source should be ignored, not selected"
        );
    }

    #[test]
    fn corrupt_canonical_record_blocks_legacy_fallback() {
        let _g = crate::testlock::serial();
        let fixture = Fixture::new("corrupt");
        let source = legacy_path(&fixture, "telemetry.json");
        std::fs::write(&source, serde_json::to_vec(&old_yes()).unwrap()).unwrap();
        std::fs::write(fixture.root.join("consent.json"), b"not-json").unwrap();
        let loaded = load(std::slice::from_ref(&source));
        assert!(!loaded.any() && !loaded.answered());
        assert!(source.exists());
    }

    #[test]
    fn failed_migration_write_keeps_the_legacy_source() {
        let _g = crate::testlock::serial();
        let fixture = Fixture::new("failed");
        let source = legacy_path(&fixture, "telemetry.json");
        std::fs::write(&source, serde_json::to_vec(&old_yes()).unwrap()).unwrap();
        std::fs::set_permissions(&fixture.root, std::fs::Permissions::from_mode(0o500)).unwrap();
        let _ = load(std::slice::from_ref(&source));
        assert!(source.exists());
    }

    #[test]
    fn writable_legacy_yes_is_barriered_off_across_two_launches() {
        let _g = crate::testlock::serial();
        let fixture = Fixture::new("widened-two-launches");
        let source = legacy_path(&fixture, "telemetry.json");
        std::fs::write(&source, serde_json::to_vec(&old_yes()).unwrap()).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o666)).unwrap();

        let first = load(std::slice::from_ref(&source));
        assert!(!first.any() && first.install_id.is_none() && first.errors_id.is_none());
        let second = load(std::slice::from_ref(&source));
        assert!(!second.any() && !second.answered());
        assert!(
            source.exists(),
            "the untrusted source remains stale and ignored"
        );
        assert!(matches!(last_outcome().write, PersistResult::NotAttempted));
    }

    #[test]
    fn failed_untrusted_barrier_leaves_writable_source_untrusted_on_next_launch() {
        let _g = crate::testlock::serial();
        let fixture = Fixture::new("widened-barrier-failure");
        let source = legacy_path(&fixture, "telemetry.json");
        std::fs::write(&source, serde_json::to_vec(&old_yes()).unwrap()).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o666)).unwrap();
        std::fs::set_permissions(&fixture.root, std::fs::Permissions::from_mode(0o500)).unwrap();

        let first = load(std::slice::from_ref(&source));
        assert!(!first.any() && first.install_id.is_none());
        assert_eq!(
            std::fs::metadata(&source).unwrap().permissions().mode() & 0o777,
            0o666
        );
        let second = load(std::slice::from_ref(&source));
        assert!(!second.any() && second.install_id.is_none());
        assert_eq!(last_outcome().write, PersistResult::Failed);
        assert!(!fixture.root.join("consent.json").exists());
        std::fs::set_permissions(&fixture.root, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn conflicting_trusted_legacy_decisions_fail_closed() {
        let _g = crate::testlock::serial();
        let fixture = Fixture::new("conflict");
        let first = legacy_path(&fixture, "first.json");
        let second = legacy_path(&fixture, "second.json");
        std::fs::write(&first, serde_json::to_vec(&old_yes()).unwrap()).unwrap();
        let mut no = old_yes();
        no.usage = false;
        no.install_id = None;
        std::fs::write(&second, serde_json::to_vec(&no).unwrap()).unwrap();
        let loaded = load(&[first, second]);
        assert!(!loaded.any() && !loaded.answered());
    }
}

/// In-place upgrades from published 0.6 decisions, through both shipping paths: the webOS DB8
/// helper (the real backend behind a synthetic DB8 RPC) and the host JSON store.
///
/// `tests/fixtures/persistence/generated` was written by the tagged releases' own code
/// (`generated/generators.patch`); the per-tag files beside it are 0.6.6's schema fixtures.
#[cfg(test)]
mod upgrade_tests {
    use super::*;
    use crate::storage::wire::{ErrorCode, Request};
    use serde_json::Value;
    use std::os::unix::fs::PermissionsExt;

    const DB8_066: &str =
        include_str!("../../../tests/fixtures/persistence/generated/v0.6.6-db8-record.json");
    const JSON_STORE_066: &str =
        include_str!("../../../tests/fixtures/persistence/generated/v0.6.6-json-store/consent.json");
    const GENERATED_DECLINE_065: &str = include_str!(
        "../../../tests/fixtures/persistence/generated/v0.6.5-errors-yes-declined-extension.consent.json"
    );

    fn published() -> Vec<(&'static str, &'static str)> {
        macro_rules! fx {
            ($p:literal) => {
                ($p, include_str!(concat!("../../../tests/fixtures/persistence/", $p)))
            };
        }
        vec![
            fx!("v0.6.0/consent-yes.json"),
            fx!("v0.6.0/consent-no.json"),
            fx!("v0.6.1/consent-yes.json"),
            fx!("v0.6.1/consent-no.json"),
            fx!("v0.6.2/consent-yes.json"),
            fx!("v0.6.2/consent-no.json"),
            fx!("v0.6.3/consent-yes.json"),
            fx!("v0.6.3/consent-no.json"),
            fx!("v0.6.4/consent-yes.json"),
            fx!("v0.6.4/consent-no.json"),
            fx!("v0.6.5/consent-yes.json"),
            fx!("v0.6.5/consent-no.json"),
            fx!("generated/v0.6.0-errors-yes.consent.json"),
            fx!("generated/v0.6.0-no.consent.json"),
            fx!("generated/v0.6.5-both-yes.consent.json"),
            fx!("generated/v0.6.5-errors-yes-declined-extension.consent.json"),
        ]
    }

    #[derive(Default)]
    struct Db8 {
        record: Option<Value>,
        puts: usize,
        refuse_puts: bool,
    }
    impl crate::storage::keymanager::Rpc for Db8 {
        fn call(&mut self, uri: &str, payload: &Value) -> Result<Value, ErrorCode> {
            match uri {
                "luna://com.palm.db/get" => Ok(
                    serde_json::json!({"returnValue":true,"results":self.record.iter().cloned().collect::<Vec<_>>()}),
                ),
                "luna://com.palm.db/put" if self.refuse_puts => Err(ErrorCode::Unavailable),
                "luna://com.palm.db/put" => {
                    let mut next = payload["objects"][0].clone();
                    self.puts += 1;
                    let rev = self.record.as_ref().and_then(|r| r["_rev"].as_u64()).unwrap_or(0) + 1;
                    next["_rev"] = serde_json::json!(rev);
                    self.record = Some(next);
                    Ok(serde_json::json!({"returnValue":true,"results":[{"id":"plxstate.stable","rev":rev}]}))
                }
                other => panic!("unexpected DB8/Keymanager call {other}"),
            }
        }
    }

    fn backend(record: Option<Value>) -> crate::storage::backend::Backend<Db8> {
        crate::storage::backend::Backend::new(
            Db8 {
                record,
                ..Default::default()
            },
            crate::storage::state::Flavor::Stable,
            "com.beb.plxnative.storage".into(),
        )
    }

    fn db8_066() -> Value {
        serde_json::from_str(DB8_066).unwrap()
    }

    fn helper_load(b: &mut crate::storage::backend::Backend<Db8>, legacy: &[PathBuf]) -> Consent {
        let mut transport = |request: Request| Ok(b.dispatch(request));
        load_helper(&mut transport, legacy)
    }

    fn helper_record(b: &mut crate::storage::backend::Backend<Db8>, consent: &Consent) -> PersistOutcome {
        let mut transport = |request: Request| Ok(b.dispatch(request));
        record_helper(&mut transport, consent, &[])
    }

    struct Dir(PathBuf);
    impl Dir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("plx-consent-upgrade-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("state")).unwrap();
            std::fs::set_permissions(dir.join("state"), std::fs::Permissions::from_mode(0o700)).unwrap();
            redirect_root_for_test(Some(dir.join("state")));
            Self(dir)
        }
        fn legacy(&self, bytes: &str) -> PathBuf {
            let path = self.0.join("telemetry.json");
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            path
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            redirect_root_for_test(None);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Every stored field a 0.6 build wrote, compared by name so a 0.6.0 file's absent scope
    /// fields are compared against the backfill `migrate_loaded` defines rather than skipped.
    fn assert_preserved(label: &str, actual: &Consent, legacy: &str) {
        let expected = super::super::consent::migrate_loaded(serde_json::from_str(legacy).unwrap());
        assert_eq!(actual, &expected, "{label}: decision, identifiers, scopes and declines");
        let raw: Value = serde_json::from_str(legacy).unwrap();
        assert_eq!(actual.errors, raw["errors"] == true, "{label}: errors choice");
        assert_eq!(actual.usage, raw["usage"] == true, "{label}: usage choice");
        assert_eq!(actual.errors_id.as_deref(), raw["errors_id"].as_str(), "{label}: errors id");
        assert_eq!(actual.install_id.as_deref(), raw["install_id"].as_str(), "{label}: usage id");
        assert!(actual.answered(), "{label}: a stored 0.6 answer (Yes or No) is an answer");
        assert!(!super::super::consent::should_ask(actual, false), "{label}: not re-asked");
    }

    #[test]
    fn every_published_06_decision_survives_a_db8_import_and_the_next_launch() {
        let _g = crate::testlock::serial();
        for (label, bytes) in published() {
            let dir = Dir::new("db8-matrix");
            let source = dir.legacy(bytes);
            let mut b = backend(None);
            let first = helper_load(&mut b, std::slice::from_ref(&source));
            assert_preserved(label, &first, bytes);
            assert_eq!(last_outcome().write, PersistResult::Durable, "{label}: import committed");
            assert!(!source.exists(), "{label}: source retired only after the verified import");
            let puts = b.rpc.puts;
            let reopened = helper_load(&mut b, std::slice::from_ref(&source));
            assert_eq!(reopened, first, "{label}: next launch reads DB8, not a legacy file");
            assert_eq!(b.rpc.puts, puts, "{label}: a completed import is not repeated");
        }
    }

    #[test]
    fn every_published_06_decision_survives_the_host_store_migration_and_a_rewrite() {
        let _g = crate::testlock::serial();
        for (label, bytes) in published() {
            let dir = Dir::new("host-matrix");
            let source = dir.legacy(bytes);
            let first = load(std::slice::from_ref(&source));
            assert_preserved(label, &first, bytes);
            assert!(!source.exists(), "{label}: host source retired after a durable commit");
            assert_eq!(record_with_legacy(&first, &[]), PersistResult::Durable);
            assert_eq!(load(&[]), first, "{label}: canonical reopen");
        }
    }

    #[test]
    fn a_066_db8_record_reopens_every_consent_field_without_writing() {
        let _g = crate::testlock::serial();
        let _dir = Dir::new("db8-066");
        let mut b = backend(Some(db8_066()));
        let loaded = helper_load(&mut b, &[]);
        assert_preserved("0.6.6 DB8", &loaded, GENERATED_DECLINE_065);
        assert_eq!(b.rpc.puts, 0);
    }

    #[test]
    fn a_066_host_store_record_reopens_every_consent_field() {
        let _g = crate::testlock::serial();
        let dir = Dir::new("json-066");
        std::fs::write(dir.0.join("state/consent.json"), JSON_STORE_066).unwrap();
        std::fs::set_permissions(dir.0.join("state/consent.json"), std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_preserved("0.6.6 JSON store", &load(&[]), GENERATED_DECLINE_065);
    }

    /// The regression this module exists for: 0.7 wrote a five-field decision stamped with its own
    /// policy version, silently dropping the accepted/declined scopes and downgrading 6 to 4.
    #[test]
    fn a_07_settings_answer_over_a_066_record_keeps_scopes_declines_and_policy_version() {
        let _g = crate::testlock::serial();
        let _dir = Dir::new("db8-edit");
        let mut b = backend(Some(db8_066()));
        let loaded = helper_load(&mut b, &[]);
        let next = super::super::consent::apply(&loaded, true, true, || Some("f".repeat(32)));
        assert_eq!(helper_record(&mut b, &next).write, PersistResult::Durable);
        let reopened = helper_load(&mut b, &[]);
        assert_eq!(reopened, next);
        assert_eq!(reopened.asked_version, 6, "policy version is never downgraded");
        assert_eq!(reopened.errors_id, loaded.errors_id, "a kept category keeps its id");
        assert_eq!((reopened.errors_scope, reopened.errors_declined_scope), (4, 6));
        assert!(reopened.usage && reopened.install_id.as_deref() == Some(&*"f".repeat(32)));
    }

    #[test]
    fn a_refused_db8_import_keeps_the_legacy_source_and_the_next_launch_completes_it() {
        let _g = crate::testlock::serial();
        let dir = Dir::new("db8-interrupted");
        let source = dir.legacy(GENERATED_DECLINE_065);
        let mut b = backend(None);
        b.rpc.refuse_puts = true;
        let first = helper_load(&mut b, std::slice::from_ref(&source));
        assert_ne!(last_outcome().write, PersistResult::Durable);
        assert!(source.exists(), "an unconfirmed import must not retire its only source");
        assert_preserved("interrupted launch", &first, GENERATED_DECLINE_065);
        b.rpc.refuse_puts = false;
        let second = helper_load(&mut b, std::slice::from_ref(&source));
        assert_preserved("recovered launch", &second, GENERATED_DECLINE_065);
        assert!(!source.exists());
        assert_eq!(helper_load(&mut b, &[]), second);
    }

    /// Commit `interfering` through the same helper immediately before the first consent commit
    /// reaches it, the way a Session worker commit can land between consent's read and write.
    fn record_racing(
        b: &mut crate::storage::backend::Backend<Db8>,
        consent: &Consent,
        interfering: fn(&crate::storage::client::Snapshot) -> crate::storage::wire::WireMutation,
    ) -> PersistOutcome {
        let mut raced = false;
        let mut transport = |request: Request| {
            if !raced && matches!(request, Request::Commit { .. }) {
                raced = true;
                let mut inner = |r: Request| Ok(b.dispatch(r));
                let crate::storage::client::Load::Present(snapshot) =
                    crate::storage::client::load_with(&mut inner).unwrap()
                else {
                    panic!("fixture is present")
                };
                let other = crate::storage::client::commit_with(
                    &mut inner,
                    Some((&snapshot.db_rev, snapshot.state.expected())),
                    crate::storage::state::Generation::random().unwrap(),
                    interfering(&snapshot),
                );
                assert!(matches!(
                    other,
                    Ok(crate::storage::wire::Response::Commit { applied: Some(_), .. })
                ));
            }
            Ok(b.dispatch(request))
        };
        record_helper(&mut transport, consent, &[])
    }

    #[test]
    fn a_session_commit_landing_mid_write_does_not_drop_the_decision() {
        let _g = crate::testlock::serial();
        let _dir = Dir::new("db8-race");
        let mut b = backend(Some(db8_066()));
        let loaded = helper_load(&mut b, &[]);
        let next = super::super::consent::apply(&loaded, false, false, || None);
        let outcome = record_racing(&mut b, &next, |snapshot| {
            let mut public = snapshot.state.public.clone();
            public.preferences["playback_quality"] = "original".into();
            crate::storage::wire::WireMutation::UpdatePreferences {
                payload: serde_json::to_value(public).unwrap(),
            }
        });
        assert_eq!(outcome.write, PersistResult::Durable, "a same-tenure conflict is retried");
        assert_eq!(helper_load(&mut b, &[]), next, "the withdrawal survives the next launch");
        let mut transport = |request: Request| Ok(b.dispatch(request));
        let crate::storage::client::Load::Present(snapshot) =
            crate::storage::client::load_with(&mut transport).unwrap()
        else {
            panic!("present")
        };
        assert_eq!(snapshot.state.public.preferences["playback_quality"], "original");
    }

    #[test]
    fn a_decision_racing_a_sign_out_is_not_written_into_the_cleared_tenure() {
        let _g = crate::testlock::serial();
        let _dir = Dir::new("db8-race-clear");
        let mut b = backend(Some(db8_066()));
        let loaded = helper_load(&mut b, &[]);
        let next = super::super::consent::apply(&loaded, true, true, || Some("f".repeat(32)));
        let outcome = record_racing(&mut b, &next, |_| crate::storage::wire::WireMutation::ClearTenure {});
        assert_ne!(outcome.write, PersistResult::Durable);
        let after = helper_load(&mut b, &[]);
        assert!(!after.answered() && after.errors_id.is_none() && after.install_id.is_none());
    }

    #[test]
    fn a_decision_racing_a_fresh_sign_in_is_not_written_into_the_new_account() {
        let _g = crate::testlock::serial();
        let _dir = Dir::new("db8-race-signin");
        let mut b = backend(Some(db8_066()));
        let loaded = helper_load(&mut b, &[]);
        let next = super::super::consent::apply(&loaded, true, true, || Some("f".repeat(32)));
        let outcome = record_racing(&mut b, &next, |snapshot| {
            let account = crate::plex::session::Session {
                client_id: "synthetic-client-id".into(),
                account_token: "synthetic-next-account".into(),
                ..Default::default()
            };
            let (mut public, protected) = crate::plex::session::split_canonical(&account).unwrap();
            public.consent = snapshot.state.public.consent.clone();
            public.scopes = snapshot.state.public.scopes.clone();
            public.ids = snapshot.state.public.ids.clone();
            crate::storage::wire::WireMutation::ReplaceAuth {
                public: serde_json::to_value(public).unwrap(),
                payload: crate::storage::wire::SecretString(protected),
                protection: crate::storage::wire::ProtectionRequest::Db8AclOnlyExplicit,
            }
        });
        assert_ne!(outcome.write, PersistResult::Durable);
        let after = helper_load(&mut b, &[]);
        assert_ne!(after.install_id.as_deref(), Some(&*"f".repeat(32)), "not written after the tenure moved");
    }

    #[test]
    fn a_db8_sign_out_outranks_a_reappeared_legacy_yes() {
        let _g = crate::testlock::serial();
        let dir = Dir::new("db8-cleared");
        let mut b = backend(Some(db8_066()));
        let mut transport = |request: Request| Ok(b.dispatch(request));
        let crate::storage::client::Load::Present(snapshot) =
            crate::storage::client::load_with(&mut transport).unwrap()
        else {
            panic!("fixture is present")
        };
        let cleared = crate::storage::client::commit_with(
            &mut transport,
            Some((&snapshot.db_rev, snapshot.state.expected())),
            crate::storage::state::Generation::random().unwrap(),
            crate::storage::wire::WireMutation::ClearTenure {},
        );
        assert!(matches!(
            cleared,
            Ok(crate::storage::wire::Response::Commit { applied: Some(_), .. })
        ));
        let source = dir.legacy(GENERATED_DECLINE_065);
        let after = helper_load(&mut b, std::slice::from_ref(&source));
        assert!(!after.any() && !after.answered() && after.errors_id.is_none());
        assert!(!source.exists(), "a stale source is retired, never imported, after sign-out");
    }
}
