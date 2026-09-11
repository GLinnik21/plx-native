//! Canonical consent persistence and the one-time legacy migration.
//!
//! The record backend owns framing and filesystem durability. This adapter owns the consent JSON
//! payload, revision choice, and the rule that a present canonical record is terminal: legacy
//! files are consulted only when `consent.json` is genuinely absent.

use super::consent::Consent;
#[cfg(test)]
use crate::storage::JsonStore;
use crate::storage::{Record, RecordKey, RecordState, RecordStore, StoreError};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersistResult {
    NotAttempted,
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

fn root() -> PathBuf {
    crate::paths::persistent_state_root()
}

/// Snapshot the canonical destination before an asynchronous operation is queued. In production
/// it is stable for the process; tests redirect it, so resolving it on the worker would let an old
/// operation write into a later fixture.
pub(super) fn operation_root() -> PathBuf {
    root()
}

fn store() -> Result<impl RecordStore, StoreError> {
    crate::storage::open(root())
}

fn store_at(root: PathBuf) -> Result<impl RecordStore, StoreError> {
    crate::storage::open(root)
}

fn cleanup_canonical(store: &impl RecordStore) -> CleanupResult {
    if store.cleanup(RecordKey::Consent).is_ok() {
        CleanupResult::Complete
    } else {
        CleanupResult::Failed
    }
}

fn remove_legacy_sources(paths: impl IntoIterator<Item = PathBuf>) -> CleanupResult {
    let mut parents = std::collections::BTreeSet::new();
    let mut result = CleanupResult::Complete;
    for path in paths {
        match std::fs::remove_file(&path) {
            Ok(()) => {
                if let Some(parent) = path.parent() {
                    parents.insert(parent.to_path_buf());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => result = CleanupResult::Failed,
        }
    }
    for parent in parents {
        match std::fs::File::open(parent).and_then(|directory| directory.sync_all()) {
            Ok(()) => {}
            Err(_) => result = CleanupResult::Failed,
        }
    }
    result
}

/// Load canonical consent, falling back to trusted legacy candidates only when canonical is
/// absent. A malformed, inaccessible, future, or otherwise present canonical record is terminal.
pub(crate) fn load(legacy: &[PathBuf]) -> Consent {
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

/// Write a canonical cleared tombstone, then remove stale legacy copies best-effort.
pub(super) fn forget_at(legacy: &[PathBuf], canonical_root: PathBuf) -> PersistOutcome {
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

#[cfg(test)]
mod tests {
    use super::*;
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
