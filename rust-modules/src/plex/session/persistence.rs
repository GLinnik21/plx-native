//! Canonical Session record adapter.
//!
//! This module owns the distinction between the versioned canonical record and the legacy
//! filename candidates. It deliberately does not deserialize Session JSON: migration carries the
//! exact legacy UTF-8 payload into Record::data before the domain reader is allowed to rewrite it.

use crate::storage::{
    CommitReceipt, CommitStage, Record, RecordKey, RecordState, RecordStore, StoreError,
};
use std::path::{Path, PathBuf};

#[cfg(any(
    all(
        target_os = "linux",
        target_arch = "arm",
        not(feature = "hostsim"),
        not(test)
    ),
    test
))]
use crate::storage::wire::ProtectionRequest;
#[cfg(all(
    target_os = "linux",
    target_arch = "arm",
    not(feature = "hostsim"),
    not(test)
))]
use crate::storage::{
    client::{self, Load as HelperLoad},
    state::{self, Generation, MigrationProgress, Status},
    wire::{AuthLoad, CommitStatus, MigrationMutation, Response, WireMutation},
};

pub(crate) enum CanonicalRead {
    Missing,
    Data { revision: u64, payload: String },
    Cleared { revision: u64 },
    Blocked(StoreError),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CanonicalCommit {
    Durable {
        revision: u64,
        verified: bool,
        #[cfg(all(
            target_os = "linux",
            target_arch = "arm",
            not(feature = "hostsim"),
            not(test)
        ))]
        protection: Option<crate::storage::wire::ProtectionOutcome>,
    },
    Uncertain {
        stage: CommitStage,
        errno: i32,
    },
    Failed(StoreError),
}

pub(crate) fn root() -> PathBuf {
    crate::paths::persistent_state_root()
}

pub(crate) fn path() -> PathBuf {
    root().join("session.json")
}

pub(crate) fn cleanup_temporaries() -> Result<(), StoreError> {
    #[cfg(all(
        target_os = "linux",
        target_arch = "arm",
        not(feature = "hostsim"),
        not(test)
    ))]
    {
        // DB8 has no app-owned rename temporaries. Legacy paths are retired explicitly after the
        // helper has verified the committed destination; absence of the old app/state directory
        // is normal on a fresh install and must not turn sign-out into a cleanup failure.
        Ok(())
    }
    #[cfg(not(all(
        target_os = "linux",
        target_arch = "arm",
        not(feature = "hostsim"),
        not(test)
    )))]
    {
        store()?.cleanup(RecordKey::Session)
    }
}

fn store() -> Result<impl RecordStore, StoreError> {
    crate::paths::ensure_persistent_state_root().map_err(|error| StoreError::Io {
        stage: crate::storage::CommitStage::ParentOpen,
        errno: error.raw_os_error().unwrap_or(0),
    })?;
    crate::storage::open(root())
}

/// Read the versioned JSON record used by the pre-DB8 0.6.6 candidates without creating it.
/// Its wrapper is decoded here; callers must never feed the wrapper itself to `Session`.
#[cfg(all(
    target_os = "linux",
    target_arch = "arm",
    not(feature = "hostsim"),
    not(test)
))]
pub(crate) fn load_legacy_json() -> CanonicalRead {
    load_legacy_json_at(root())
}

#[cfg(any(
    all(
        target_os = "linux",
        target_arch = "arm",
        not(feature = "hostsim"),
        not(test)
    ),
    test
))]
fn load_legacy_json_at(root: PathBuf) -> CanonicalRead {
    match std::fs::symlink_metadata(&root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => CanonicalRead::Missing,
        Err(error) => CanonicalRead::Blocked(StoreError::Io {
            stage: CommitStage::ParentOpen,
            errno: error.raw_os_error().unwrap_or(0),
        }),
        Ok(metadata) if !metadata.is_dir() => CanonicalRead::Blocked(StoreError::RootNotDirectory),
        Ok(_) => {
            let store = match crate::storage::open(root) {
                Ok(store) => store,
                Err(error) => return CanonicalRead::Blocked(error),
            };
            match store.load(RecordKey::Session) {
                Ok(None) => CanonicalRead::Missing,
                Ok(Some(record)) => match record.state {
                    RecordState::Data { payload } => CanonicalRead::Data {
                        revision: record.revision,
                        payload,
                    },
                    RecordState::Cleared => CanonicalRead::Cleared {
                        revision: record.revision,
                    },
                },
                Err(error) => CanonicalRead::Blocked(error),
            }
        }
    }
}

pub(crate) fn load() -> CanonicalRead {
    #[cfg(all(
        target_os = "linux",
        target_arch = "arm",
        not(feature = "hostsim"),
        not(test)
    ))]
    {
        return load_helper();
    }
    #[cfg(not(all(
        target_os = "linux",
        target_arch = "arm",
        not(feature = "hostsim"),
        not(test)
    )))]
    {
        let store = match store() {
            Ok(store) => store,
            Err(error) => return CanonicalRead::Blocked(error),
        };
        match store.load(RecordKey::Session) {
            Ok(None) => CanonicalRead::Missing,
            Ok(Some(record)) => match record.state {
                RecordState::Data { payload } => CanonicalRead::Data {
                    revision: record.revision,
                    payload,
                },
                RecordState::Cleared => CanonicalRead::Cleared {
                    revision: record.revision,
                },
            },
            Err(error) => CanonicalRead::Blocked(error),
        }
    }
}

#[cfg(all(
    target_os = "linux",
    target_arch = "arm",
    not(feature = "hostsim"),
    not(test)
))]
fn helper_error(error: client::ClientError) -> StoreError {
    match error {
        client::ClientError::Unavailable => StoreError::HelperUnavailable,
        client::ClientError::Authentication => StoreError::HelperAuthentication,
        client::ClientError::Protocol => StoreError::HelperProtocol,
        client::ClientError::Corrupt | client::ClientError::Invalid => StoreError::InvalidSchema,
    }
}

#[cfg(any(
    test,
    all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"))
))]
fn helper_rejection(code: crate::storage::wire::ErrorCode) -> StoreError {
    use crate::storage::wire::ErrorCode;
    match code {
        ErrorCode::Unavailable | ErrorCode::Timeout | ErrorCode::Capability => {
            StoreError::HelperUnavailable
        }
        ErrorCode::Authentication => StoreError::HelperAuthentication,
        ErrorCode::Protocol => StoreError::HelperProtocol,
        ErrorCode::Invalid | ErrorCode::Corrupt => StoreError::InvalidSchema,
    }
}

#[cfg(all(
    target_os = "linux",
    target_arch = "arm",
    not(feature = "hostsim"),
    not(test)
))]
fn load_helper() -> CanonicalRead {
    match client::load() {
        Ok(HelperLoad::Missing) => CanonicalRead::Missing,
        Ok(HelperLoad::Present(snapshot)) => {
            if let Some(outcome) = snapshot
                .protection
                .filter(|outcome| outcome.fallback.is_some())
            {
                // The fallback reason is deliberately persisted in the ACL-only envelope. This
                // lets a later launch report it if the sign-in process ended before consent was
                // settled or before the original report reached Sentry.
                let _ = crate::telemetry::storage::report_keymanager_protection(outcome, true);
            }
            if snapshot.state.status == Status::Cleared {
                return CanonicalRead::Cleared {
                    revision: snapshot.state.revision,
                };
            }
            if snapshot.state.migrations.session.progress != MigrationProgress::Complete {
                return CanonicalRead::Missing;
            }
            match snapshot.auth {
                AuthLoad::Plaintext { payload } => {
                    match super::join_canonical(&snapshot.state.public, &payload.0) {
                        Ok(session) => {
                            let payload = match serde_json::to_string(&session) {
                                Ok(payload) => payload,
                                Err(_) => return CanonicalRead::Blocked(StoreError::InvalidSchema),
                            };
                            CanonicalRead::Data {
                                revision: snapshot.state.revision,
                                payload,
                            }
                        }
                        Err(()) => CanonicalRead::Blocked(StoreError::InvalidSchema),
                    }
                }
                AuthLoad::Locked { .. } => CanonicalRead::Blocked(StoreError::AuthLocked),
                AuthLoad::None => CanonicalRead::Blocked(StoreError::InvalidSchema),
            }
        }
        Err(error) => CanonicalRead::Blocked(helper_error(error)),
    }
}

fn commit(record: Record) -> CanonicalCommit {
    let revision = record.revision;
    let store = match store() {
        Ok(store) => store,
        Err(error) => return CanonicalCommit::Failed(error),
    };
    match store.commit(RecordKey::Session, &record) {
        Ok(CommitReceipt::Durable) => CanonicalCommit::Durable {
            revision,
            verified: true,
            #[cfg(all(
                target_os = "linux",
                target_arch = "arm",
                not(feature = "hostsim"),
                not(test)
            ))]
            protection: None,
        },
        Ok(CommitReceipt::Uncertain { stage, errno }) => {
            CanonicalCommit::Uncertain { stage, errno }
        }
        Err(error) => CanonicalCommit::Failed(error),
    }
}

pub(crate) fn commit_data(payload: String) -> CanonicalCommit {
    #[cfg(all(
        target_os = "linux",
        target_arch = "arm",
        not(feature = "hostsim"),
        not(test)
    ))]
    {
        let session = match serde_json::from_str::<super::Session>(&payload) {
            Ok(session) => session,
            Err(_) => return CanonicalCommit::Failed(StoreError::InvalidSchema),
        };
        return commit_session(&session, false, super::SaveAuthority::Routine);
    }
    #[cfg(not(all(
        target_os = "linux",
        target_arch = "arm",
        not(feature = "hostsim"),
        not(test)
    )))]
    {
        let revision = match load() {
            CanonicalRead::Data { revision, .. } | CanonicalRead::Cleared { revision } => {
                match revision.checked_add(1) {
                    Some(revision) => revision,
                    None => return CanonicalCommit::Failed(StoreError::InvalidSchema),
                }
            }
            CanonicalRead::Missing => 1,
            CanonicalRead::Blocked(error) => return CanonicalCommit::Failed(error),
        };
        commit(Record::data(revision, payload))
    }
}

pub(crate) fn commit_cleared() -> CanonicalCommit {
    #[cfg(all(
        target_os = "linux",
        target_arch = "arm",
        not(feature = "hostsim"),
        not(test)
    ))]
    {
        return commit_clear();
    }
    #[cfg(not(all(
        target_os = "linux",
        target_arch = "arm",
        not(feature = "hostsim"),
        not(test)
    )))]
    {
        let revision = match load() {
            CanonicalRead::Data { revision, .. } | CanonicalRead::Cleared { revision } => {
                match revision.checked_add(1) {
                    Some(revision) => revision,
                    None => return CanonicalCommit::Failed(StoreError::InvalidSchema),
                }
            }
            CanonicalRead::Missing => 1,
            CanonicalRead::Blocked(_) => 1,
        };
        commit(Record::cleared(revision))
    }
}

/// Exact-payload migration. The caller owns keymanager semantics and legacy cleanup; this helper
/// only commits the bytes and reports whether cleanup may proceed.
pub(crate) fn migrate_exact(source: &Path, payload: &[u8]) -> (CanonicalCommit, PathBuf) {
    let payload = match std::str::from_utf8(payload) {
        Ok(payload) => payload.to_owned(),
        Err(_) => {
            return (
                CanonicalCommit::Failed(StoreError::InvalidUtf8),
                source.to_path_buf(),
            )
        }
    };
    (commit_data(payload), source.to_path_buf())
}

#[cfg(all(
    target_os = "linux",
    target_arch = "arm",
    not(feature = "hostsim"),
    not(test)
))]
pub(crate) fn migrate_session(
    source: &Path,
    session: &super::Session,
) -> (CanonicalCommit, PathBuf) {
    (
        commit_session(session, true, super::SaveAuthority::Routine),
        source.to_path_buf(),
    )
}

#[cfg(all(
    target_os = "linux",
    target_arch = "arm",
    not(feature = "hostsim"),
    not(test)
))]
fn acl_only_envelope(envelope: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(envelope)
        .ok()
        .and_then(|value| value.get("format").and_then(str_value).map(str::to_owned))
        .as_deref()
        == Some("db8-acl-only-v1")
}

#[cfg(any(
    all(
        target_os = "linux",
        target_arch = "arm",
        not(feature = "hostsim"),
        not(test)
    ),
    test
))]
fn preserve_unknown_preferences(current: &serde_json::Value, next: &mut serde_json::Value) {
    let (Some(current), Some(next)) = (current.as_object(), next.as_object_mut()) else {
        return;
    };
    for (key, value) in current {
        if key != "playback_quality" && !next.contains_key(key) {
            next.insert(key.clone(), value.clone());
        }
    }
}

#[cfg(all(
    target_os = "linux",
    target_arch = "arm",
    not(feature = "hostsim"),
    not(test)
))]
fn str_value(value: &serde_json::Value) -> Option<&str> {
    value.as_str()
}

#[cfg(any(
    all(
        target_os = "linux",
        target_arch = "arm",
        not(feature = "hostsim"),
        not(test)
    ),
    test
))]
fn protection_for_auth_write(
    major: u32,
    may_fallback: bool,
    existing_acl_only: bool,
    existing_protected: bool,
) -> ProtectionRequest {
    if may_fallback {
        if (1..=4).contains(&major) {
            ProtectionRequest::Db8AclOnlyExplicit
        } else {
            ProtectionRequest::KeymanagerWithAclFallback
        }
    } else if existing_acl_only {
        ProtectionRequest::Db8AclOnlyExplicit
    } else if existing_protected {
        // Preserve the protection already earned by this record even if firmware classification
        // changes.  The OS-major policy is only a default for a new record; it must never turn a
        // routine refresh of healthy ciphertext into plaintext-at-rest.
        ProtectionRequest::KeymanagerRequired
    } else if (1..=4).contains(&major) {
        ProtectionRequest::Db8AclOnlyExplicit
    } else {
        // A routine refresh/new non-authenticated write on newer firmware fails closed. Only a
        // fresh login or an explicit legacy import may trade encryption for login durability.
        ProtectionRequest::KeymanagerRequired
    }
}

#[cfg(all(
    target_os = "linux",
    target_arch = "arm",
    not(feature = "hostsim"),
    not(test)
))]
fn expected(snapshot: &client::Snapshot) -> Option<(&str, state::Expected)> {
    Some((&snapshot.db_rev, snapshot.state.expected()))
}

#[cfg(all(
    target_os = "linux",
    target_arch = "arm",
    not(feature = "hostsim"),
    not(test)
))]
fn commit_session(
    session: &super::Session,
    migration: bool,
    authority: super::SaveAuthority,
) -> CanonicalCommit {
    let (mut public, protected) = match super::split_canonical(session) {
        Ok(parts) => parts,
        Err(()) => return CanonicalCommit::Failed(StoreError::InvalidSchema),
    };
    let loaded = match client::load() {
        Ok(loaded) => loaded,
        Err(error) => return CanonicalCommit::Failed(helper_error(error)),
    };
    if let HelperLoad::Present(snapshot) = &loaded {
        preserve_unknown_preferences(
            &snapshot.state.public.preferences,
            &mut public.preferences,
        );
    }
    let may_fallback = migration || authority == super::SaveAuthority::FreshReauthentication;
    let mutation = match &loaded {
        HelperLoad::Missing if migration => WireMutation::AdvanceMigration {
            migration: MigrationMutation::SessionComplete {
                public: match serde_json::to_value(&public) {
                    Ok(value) => value,
                    Err(_) => return CanonicalCommit::Failed(StoreError::InvalidSchema),
                },
                auth_plaintext: crate::storage::wire::SecretString(protected),
                protection: protection_for_auth_write(crate::webos::info().major, true, false, false),
            },
        },
        HelperLoad::Missing if authority == super::SaveAuthority::PublicOnly => {
            return CanonicalCommit::Failed(StoreError::InvalidSchema)
        }
        HelperLoad::Missing => WireMutation::ReplaceAuth {
            public: match serde_json::to_value(&public) {
                Ok(value) => value,
                Err(_) => return CanonicalCommit::Failed(StoreError::InvalidSchema),
            },
            payload: crate::storage::wire::SecretString(protected),
            protection: protection_for_auth_write(crate::webos::info().major, may_fallback, false, false),
        },
        HelperLoad::Present(snapshot) if migration => WireMutation::AdvanceMigration {
            migration: MigrationMutation::SessionComplete {
                public: match serde_json::to_value(&public) {
                    Ok(value) => value,
                    Err(_) => return CanonicalCommit::Failed(StoreError::InvalidSchema),
                },
                auth_plaintext: crate::storage::wire::SecretString(protected),
                protection: protection_for_auth_write(crate::webos::info().major, true, false, false),
            },
        },
        HelperLoad::Present(_) if authority == super::SaveAuthority::PublicOnly => {
            WireMutation::UpdatePreferences {
                payload: match serde_json::to_value(&public) {
                    Ok(value) => value,
                    Err(_) => return CanonicalCommit::Failed(StoreError::InvalidSchema),
                },
            }
        }
        HelperLoad::Present(snapshot) => {
            let unchanged = matches!(
                &snapshot.auth,
                AuthLoad::Plaintext { payload } if payload.0 == protected
            );
            if unchanged && authority != super::SaveAuthority::FreshReauthentication {
                WireMutation::UpdatePreferences {
                    payload: match serde_json::to_value(&public) {
                        Ok(value) => value,
                        Err(_) => return CanonicalCommit::Failed(StoreError::InvalidSchema),
                    },
                }
            } else if authority == super::SaveAuthority::FreshReauthentication
                || !matches!(snapshot.auth, AuthLoad::Locked { .. })
            {
                WireMutation::ReplaceAuth {
                    public: match serde_json::to_value(&public) {
                        Ok(value) => value,
                        Err(_) => return CanonicalCommit::Failed(StoreError::InvalidSchema),
                    },
                    payload: crate::storage::wire::SecretString(protected),
                    protection: protection_for_auth_write(
                        crate::webos::info().major,
                        may_fallback,
                        snapshot
                            .state
                            .auth_envelope
                            .as_deref()
                            .is_some_and(acl_only_envelope),
                        snapshot.state.auth_envelope.is_some(),
                    ),
                }
            } else {
                return CanonicalCommit::Failed(StoreError::AuthLocked);
            }
        }
    };
    let operation = match Generation::random() {
        Ok(operation) => operation,
        Err(_) => return CanonicalCommit::Failed(StoreError::HelperUnavailable),
    };
    let expectation = match &loaded {
        HelperLoad::Missing => None,
        HelperLoad::Present(snapshot) => expected(snapshot),
    };
    helper_commit(expectation, operation, mutation)
}

#[cfg(all(
    target_os = "linux",
    target_arch = "arm",
    not(feature = "hostsim"),
    not(test)
))]
pub(crate) fn commit_session_with_authority(
    session: &super::Session,
    authority: super::SaveAuthority,
) -> CanonicalCommit {
    commit_session(session, false, authority)
}

#[cfg(all(
    target_os = "linux",
    target_arch = "arm",
    not(feature = "hostsim"),
    not(test)
))]
fn commit_clear() -> CanonicalCommit {
    let loaded = match client::load() {
        Ok(loaded) => loaded,
        Err(error) => return CanonicalCommit::Failed(helper_error(error)),
    };
    let operation = match Generation::random() {
        Ok(operation) => operation,
        Err(_) => return CanonicalCommit::Failed(StoreError::HelperUnavailable),
    };
    let expectation = match &loaded {
        HelperLoad::Missing => None,
        HelperLoad::Present(snapshot) => expected(snapshot),
    };
    helper_commit(expectation, operation, WireMutation::ClearTenure {})
}

#[cfg(all(
    target_os = "linux",
    target_arch = "arm",
    not(feature = "hostsim"),
    not(test)
))]
fn helper_commit(
    expected: Option<(&str, state::Expected)>,
    operation: Generation,
    mutation: WireMutation,
) -> CanonicalCommit {
    match client::commit(expected, operation, mutation) {
        Ok(Response::Commit {
            status: CommitStatus::Committed,
            state: Some(value),
            applied: Some(applied),
            verified,
            protection,
            ..
        }) => match serde_json::to_vec(&value).ok().and_then(|bytes| {
            state::CanonicalState::decode(
                &bytes,
                if crate::paths::flavour() == Some("debug") {
                    state::Flavor::Debug
                } else {
                    state::Flavor::Stable
                },
            )
            .ok()
        }) {
            Some(_) => CanonicalCommit::Durable {
                revision: applied.revision,
                verified,
                protection,
            },
            None => CanonicalCommit::Failed(StoreError::InvalidSchema),
        },
        Ok(Response::Reconcile {
            status: crate::storage::wire::ReconcileStatus::Applied,
            applied: Some(applied),
            protection,
            ..
        }) => CanonicalCommit::Durable {
            revision: applied.revision,
            verified: false,
            protection,
        },
        Ok(Response::Commit {
            status: CommitStatus::Conflict,
            ..
        }) => CanonicalCommit::Failed(StoreError::Conflict),
        Ok(Response::Commit {
            status: CommitStatus::Unavailable,
            ..
        })
        | Ok(Response::Reconcile {
            status: crate::storage::wire::ReconcileStatus::Unknown,
            ..
        }) => CanonicalCommit::Uncertain {
            stage: CommitStage::Readback,
            errno: 0,
        },
        Ok(Response::Error { code }) => {
            crate::log(&format!(
                "session: storage helper rejected commit code={code:?}"
            ));
            CanonicalCommit::Failed(helper_rejection(code))
        }
        Ok(Response::KeymanagerError {
            failure,
            preservation,
            db8_commit_verified,
        }) => {
            let _ = crate::telemetry::storage::report_keymanager_fail_closed(
                failure,
                preservation,
                db8_commit_verified,
            );
            if preservation == crate::storage::wire::AuthPreservation::Uncertain {
                CanonicalCommit::Uncertain {
                    stage: CommitStage::Readback,
                    errno: 0,
                }
            } else {
                CanonicalCommit::Failed(StoreError::AuthLocked)
            }
        }
        Ok(response) => {
            let shape = match response {
                Response::Commit { .. } => "commit",
                Response::Reconcile { .. } => "reconcile",
                Response::Hello { .. } => "hello",
                Response::Loaded { .. } => "loaded",
                Response::Missing => "missing",
                Response::Error { .. } => "error",
                Response::KeymanagerError { .. } => "keymanager_error",
            };
            crate::log(&format!(
                "session: storage helper returned incomplete response shape={shape}"
            ));
            CanonicalCommit::Failed(StoreError::InvalidSchema)
        }
        Err(error) => CanonicalCommit::Failed(helper_error(error)),
    }
}

#[cfg(test)]
mod db8_policy_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn helper_timeout_is_unavailable_not_invalid_schema() {
        assert_eq!(
            helper_rejection(crate::storage::wire::ErrorCode::Timeout),
            StoreError::HelperUnavailable
        );
        assert_eq!(
            helper_rejection(crate::storage::wire::ErrorCode::Corrupt),
            StoreError::InvalidSchema
        );
    }

    #[test]
    fn old_firmware_uses_acl_directly_and_newer_firmware_requests_crypto_with_fallback() {
        assert!(matches!(
            protection_for_auth_write(4, true, false, false),
            ProtectionRequest::Db8AclOnlyExplicit
        ));
        for major in [0, 5, 9, 11] {
            assert!(matches!(
                protection_for_auth_write(major, true, false, false),
                ProtectionRequest::KeymanagerWithAclFallback
            ));
        }
    }

    #[test]
    fn fallback_is_limited_to_fresh_auth_or_import_on_new_firmware() {
        assert!(matches!(
            protection_for_auth_write(11, true, false, false),
            ProtectionRequest::KeymanagerWithAclFallback
        ));
        assert!(matches!(
            protection_for_auth_write(11, false, false, false),
            ProtectionRequest::KeymanagerRequired
        ));
        assert!(matches!(
            protection_for_auth_write(11, false, true, true),
            ProtectionRequest::Db8AclOnlyExplicit
        ));
        assert!(matches!(
            protection_for_auth_write(11, true, true, true),
            ProtectionRequest::KeymanagerWithAclFallback
        ));
        assert!(matches!(
            protection_for_auth_write(4, true, false, false),
            ProtectionRequest::Db8AclOnlyExplicit
        ));
        assert!(matches!(
            protection_for_auth_write(4, false, false, true),
            ProtectionRequest::KeymanagerRequired
        ));
    }

    #[test]
    fn a_public_preferences_rewrite_preserves_future_keys() {
        let current = serde_json::json!({
            "playback_quality": {"kind":"Original"},
            "future_preference": {"version": 2, "enabled": true}
        });
        let mut next = serde_json::json!({
            "playback_quality": {"kind":"Auto"}
        });

        preserve_unknown_preferences(&current, &mut next);

        assert_eq!(next["playback_quality"]["kind"], "Auto");
        assert_eq!(next["future_preference"], current["future_preference"]);
    }

    #[test]
    fn previous_json_wrapper_yields_its_nested_session_payload_and_tombstone() {
        let _serial = crate::testlock::serial();
        let root = std::env::temp_dir().join(format!(
            "plxnative-prior-session-wrapper-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = crate::storage::open(root.clone()).unwrap();
        let payload = r#"{"client_id":"legacy-client","account_token":"legacy-token"}"#;
        assert_eq!(
            store.commit(RecordKey::Session, &Record::data(7, payload.into())),
            Ok(CommitReceipt::Durable)
        );
        match load_legacy_json_at(root.clone()) {
            CanonicalRead::Data {
                revision,
                payload: actual,
            } => {
                assert_eq!(revision, 7);
                assert_eq!(actual, payload);
            }
            _ => panic!("the wrapper was not decoded as Session data"),
        }
        assert_eq!(
            store.commit(RecordKey::Session, &Record::cleared(8)),
            Ok(CommitReceipt::Durable)
        );
        assert!(matches!(
            load_legacy_json_at(root.clone()),
            CanonicalRead::Cleared { revision: 8 }
        ));
        drop(store);
        let _ = std::fs::remove_dir_all(root);
    }
}
