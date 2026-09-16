//! Canonical Session record adapter.
//!
//! This module owns the distinction between the versioned canonical record and the legacy
//! filename candidates. Canonical helper loads, typed migration, and exact readback share this
//! adapter; no unavailable helper response authorizes a file-backend fallback.

use crate::storage::{
    CommitReceipt, CommitStage, Record, RecordKey, RecordState, RecordStore, StoreError,
};
use std::path::{Path, PathBuf};

use crate::storage::wire::ProtectionRequest;
use crate::storage::{
    client::{self, Load as HelperLoad},
    state::{self, Generation, MigrationProgress, Status},
    wire::{AuthLoad, CommitStatus, MigrationMutation, Response, WireMutation},
};

pub(crate) enum CanonicalRead {
    Missing,
    /// Raw file-record payload; it has not crossed the protected/public schema boundary.
    Data {
        revision: u64,
        payload: String,
    },
    /// Authenticated helper result. Consume this Session directly: serializing it into the
    /// flattened legacy file format would collide with opaque v1 extensions whose names became
    /// recognized Session fields in a newer client. Neither schema nor stored bytes change here.
    Opened {
        revision: u64,
        session: super::Session,
    },
    Cleared {
        revision: u64,
    },
    /// Preferences remain available while credentials and offline profile activation stay closed.
    Locked {
        revision: u64,
        public: super::Session,
        protection: Option<crate::storage::wire::ProtectionOutcome>,
    },
    /// An earlier canonical migration already owns these opaque bytes. Never search other files.
    Pending {
        revision: u64,
        envelope: String,
    },
    Blocked(StoreError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProtectionFailure {
    pub(crate) failure: crate::storage::wire::KeymanagerFailure,
    pub(crate) preservation: crate::storage::wire::AuthPreservation,
    pub(crate) db8_commit_verified: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CanonicalCommit {
    Durable {
        revision: u64,
        verified: bool,
        protection: Option<crate::storage::wire::ProtectionOutcome>,
    },
    Uncertain {
        stage: CommitStage,
        errno: i32,
    },
    Failed(StoreError),
    ProtectionFailed(ProtectionFailure),
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
pub(crate) fn load_legacy_json() -> CanonicalRead {
    load_legacy_json_at(root())
}

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

fn helper_error(error: client::ClientError) -> StoreError {
    match error {
        client::ClientError::Unavailable => StoreError::HelperUnavailable,
        client::ClientError::Authentication => StoreError::HelperAuthentication,
        client::ClientError::Protocol => StoreError::HelperProtocol,
        client::ClientError::Corrupt | client::ClientError::Invalid => StoreError::InvalidSchema,
    }
}

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

fn load_helper() -> CanonicalRead {
    load_helper_with(&mut client::NativeTransport)
}

pub(crate) fn load_helper_with(transport: &mut dyn client::Transport) -> CanonicalRead {
    match client::load_with(transport) {
        Ok(HelperLoad::Missing) => CanonicalRead::Missing,
        Ok(HelperLoad::Present(snapshot)) => {
            if snapshot.state.status == Status::Cleared {
                return CanonicalRead::Cleared {
                    revision: snapshot.state.revision,
                };
            }
            if snapshot.state.migrations.session.progress != MigrationProgress::Complete {
                return match snapshot.state.migrations.session.pending_import {
                    Some(envelope) => CanonicalRead::Pending {
                        revision: snapshot.state.revision,
                        envelope,
                    },
                    None => CanonicalRead::Missing,
                };
            }
            match snapshot.auth {
                AuthLoad::Plaintext { payload } => {
                    match super::join_canonical(&snapshot.state.public, &payload.0) {
                        Ok(session) => CanonicalRead::Opened {
                            revision: snapshot.state.revision,
                            session,
                        },
                        Err(()) => CanonicalRead::Blocked(StoreError::InvalidSchema),
                    }
                }
                AuthLoad::Locked { .. } => CanonicalRead::Locked {
                    revision: snapshot.state.revision,
                    public: super::public_session(&snapshot.state.public),
                    protection: snapshot.protection,
                },
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
            CanonicalRead::Data { revision, .. }
            | CanonicalRead::Opened { revision, .. }
            | CanonicalRead::Cleared { revision }
            | CanonicalRead::Locked { revision, .. }
            | CanonicalRead::Pending { revision, .. } => match revision.checked_add(1) {
                Some(revision) => revision,
                None => return CanonicalCommit::Failed(StoreError::InvalidSchema),
            },
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
            CanonicalRead::Data { revision, .. }
            | CanonicalRead::Opened { revision, .. }
            | CanonicalRead::Cleared { revision }
            | CanonicalRead::Locked { revision, .. }
            | CanonicalRead::Pending { revision, .. } => match revision.checked_add(1) {
                Some(revision) => revision,
                None => return CanonicalCommit::Failed(StoreError::InvalidSchema),
            },
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

pub(crate) fn migrate_session(
    source: &Path,
    session: &super::Session,
) -> (CanonicalCommit, PathBuf) {
    (
        commit_session(session, true, super::SaveAuthority::Routine),
        source.to_path_buf(),
    )
}

fn acl_only_envelope(envelope: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(envelope)
        .ok()
        .and_then(|value| value.get("format").and_then(str_value).map(str::to_owned))
        .as_deref()
        == Some("db8-acl-only-v1")
}

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

fn str_value(value: &serde_json::Value) -> Option<&str> {
    value.as_str()
}

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

fn expected(snapshot: &client::Snapshot) -> Option<(&str, state::Expected)> {
    Some((&snapshot.db_rev, snapshot.state.expected()))
}

fn commit_session(
    session: &super::Session,
    migration: bool,
    authority: super::SaveAuthority,
) -> CanonicalCommit {
    commit_session_with(
        session,
        migration,
        authority,
        crate::webos::info().major,
        &mut client::NativeTransport,
    )
}

pub(crate) fn commit_session_with(
    session: &super::Session,
    migration: bool,
    authority: super::SaveAuthority,
    major: u32,
    transport: &mut dyn client::Transport,
) -> CanonicalCommit {
    let mut public = match super::split_public(session) {
        Ok(public) => public,
        Err(()) => return CanonicalCommit::Failed(StoreError::InvalidSchema),
    };
    let loaded = match client::load_with(transport) {
        Ok(loaded) => loaded,
        Err(error) => return CanonicalCommit::Failed(helper_error(error)),
    };
    if let HelperLoad::Present(snapshot) = &loaded {
        preserve_unknown_preferences(&snapshot.state.public.preferences, &mut public.preferences);
    }
    let unchanged = matches!(&loaded, HelperLoad::Present(snapshot)
        if matches!(&snapshot.auth, AuthLoad::Plaintext {payload} if super::protected_matches(session, &payload.0)));
    let needs_protected = migration
        || (authority != super::SaveAuthority::PublicOnly
            && (!unchanged || authority == super::SaveAuthority::FreshReauthentication));
    let protected = if needs_protected {
        match super::split_canonical(session) {
            Ok((_, protected)) => protected,
            Err(()) => return CanonicalCommit::Failed(StoreError::InvalidSchema),
        }
    } else {
        String::new()
    };
    let may_fallback = migration || authority == super::SaveAuthority::FreshReauthentication;
    let mutation = match &loaded {
        HelperLoad::Missing if migration => WireMutation::AdvanceMigration {
            migration: MigrationMutation::SessionComplete {
                public: match serde_json::to_value(&public) {
                    Ok(value) => value,
                    Err(_) => return CanonicalCommit::Failed(StoreError::InvalidSchema),
                },
                auth_plaintext: crate::storage::wire::SecretString(protected),
                protection: protection_for_auth_write(major, true, false, false),
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
            protection: protection_for_auth_write(major, may_fallback, false, false),
        },
        HelperLoad::Present(_) if migration => WireMutation::AdvanceMigration {
            migration: MigrationMutation::SessionComplete {
                public: match serde_json::to_value(&public) {
                    Ok(value) => value,
                    Err(_) => return CanonicalCommit::Failed(StoreError::InvalidSchema),
                },
                auth_plaintext: crate::storage::wire::SecretString(protected),
                protection: protection_for_auth_write(major, true, false, false),
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
                        major,
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
    helper_commit_with(transport, expectation, operation, mutation)
}

pub(crate) fn commit_session_with_authority(
    session: &super::Session,
    authority: super::SaveAuthority,
) -> CanonicalCommit {
    commit_session(session, false, authority)
}

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

fn helper_commit(
    expected: Option<(&str, state::Expected)>,
    operation: Generation,
    mutation: WireMutation,
) -> CanonicalCommit {
    helper_commit_with(&mut client::NativeTransport, expected, operation, mutation)
}

fn helper_commit_with(
    transport: &mut dyn client::Transport,
    expected: Option<(&str, state::Expected)>,
    operation: Generation,
    mutation: WireMutation,
) -> CanonicalCommit {
    match client::commit_with(transport, expected, operation, mutation) {
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
        }) => CanonicalCommit::ProtectionFailed(ProtectionFailure {
            failure,
            preservation,
            db8_commit_verified,
        }),
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

    /// `bootstrap()` and `cleanup_after_confirmed_clear()` must search and sweep the pre-DB8
    /// canonical JSON wrapper identically on ARM, so both funnel through this one helper rather
    /// than each carrying its own `insert(0, path())`. This is callable unconditionally (it is
    /// pure path arithmetic), so a host test can prove the shared assembly directly instead of
    /// only trusting that the two ARM-gated call sites still agree.
    #[test]
    fn arm_candidate_assembly_puts_the_canonical_wrapper_first() {
        let mut candidates = vec![PathBuf::from("/some/legacy/auth.json")];
        insert_arm_canonical_wrapper(&mut candidates);
        assert_eq!(candidates[0], path());
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[1], PathBuf::from("/some/legacy/auth.json"));
    }
}

/// The authority-selected write used by the shared worker. ARM has exactly one authority: DB8.
pub(crate) fn write_session(
    session: &super::Session,
    authority: super::SaveAuthority,
) -> CanonicalCommit {
    #[cfg(all(
        target_os = "linux",
        target_arch = "arm",
        not(feature = "hostsim"),
        not(test)
    ))]
    {
        commit_session_with_authority(session, authority)
    }
    #[cfg(not(all(
        target_os = "linux",
        target_arch = "arm",
        not(feature = "hostsim"),
        not(test)
    )))]
    {
        let _ = authority;
        match serde_json::to_string(session) {
            Ok(payload) => commit_data(payload),
            Err(_) => CanonicalCommit::Failed(StoreError::InvalidSchema),
        }
    }
}

/// Legacy keys belong to exactly this identity. Missing metadata means Anonymous because that
/// was the only registration used before the field existed. Opening never tries another owner.
#[derive(Clone, Copy, Debug, Default, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LegacyIdentity {
    AppId,
    Named,
    #[default]
    Anonymous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LegacyOpenError {
    Unavailable,
    IdentityRefused,
    Authentication,
}

/// Resource adapter seam for the final legacy Keymanager identity bridge. Implementations must
/// request `identity` exactly, authenticate the envelope, and return plaintext only after success.
/// The envelope is deliberately not Debug and must not be logged or included in an error.
pub(crate) trait LegacyOpener {
    fn open(
        &mut self,
        identity: LegacyIdentity,
        sealed: &serde_json::Value,
    ) -> Result<Vec<u8>, LegacyOpenError>;
}

pub(crate) enum LegacySession {
    Data(super::Session),
    Cleared,
}

pub(crate) fn decode_legacy_session(
    bytes: &[u8],
    opener: &mut dyn LegacyOpener,
) -> Result<LegacySession, StoreError> {
    decode_legacy_at_depth(bytes, opener, 0)
}

fn decode_legacy_at_depth(
    bytes: &[u8],
    opener: &mut dyn LegacyOpener,
    depth: u8,
) -> Result<LegacySession, StoreError> {
    if depth > 2 {
        return Err(StoreError::InvalidSchema);
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| StoreError::InvalidSchema)?;
    if !value.is_object() {
        return Err(StoreError::InvalidSchema);
    }
    match value.get("format").and_then(serde_json::Value::as_str) {
        Some("plxnative-record") => {
            match crate::storage::parse_record(bytes, RecordKey::Session)?.state {
                RecordState::Data { payload } => {
                    decode_legacy_at_depth(payload.as_bytes(), opener, depth + 1)
                }
                RecordState::Cleared => Ok(LegacySession::Cleared),
            }
        }
        Some("plxnative-secure-session") => {
            if value.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
                return Err(StoreError::UnsupportedVersion);
            }
            let sealed = value
                .get("sealed")
                .filter(|v| v.is_object())
                .ok_or(StoreError::InvalidSchema)?;
            // Retain old unauthenticated/unknown algorithms; neither is a plaintext migration.
            if sealed.get("backend").and_then(serde_json::Value::as_str) != Some("keymanager3") {
                return Err(StoreError::UnknownFormat);
            }
            let identity = match sealed.get("identity") {
                None => LegacyIdentity::Anonymous,
                Some(identity) => serde_json::from_value(identity.clone())
                    .map_err(|_| StoreError::UnknownFormat)?,
            };
            let plaintext = opener.open(identity, sealed).map_err(|error| match error {
                LegacyOpenError::Unavailable => StoreError::HelperUnavailable,
                LegacyOpenError::IdentityRefused | LegacyOpenError::Authentication => {
                    StoreError::AuthLocked
                }
            })?;
            decode_legacy_at_depth(&plaintext, opener, depth + 1)
        }
        Some(_) => Err(StoreError::UnknownFormat),
        None if value.get("format").is_some() => Err(StoreError::UnknownFormat),
        None => serde_json::from_value(value)
            .map(LegacySession::Data)
            .map_err(|_| StoreError::InvalidSchema),
    }
}

/// Migration uses the same canonical reader/writer as ordinary operations. A blocked read can
/// never authorize a legacy import, and a historical operation receipt alone cannot retire it.
pub(crate) trait MigrationStore {
    fn load(&mut self) -> CanonicalRead;
    fn import(&mut self, session: &super::Session) -> CanonicalCommit;
    fn clear(&mut self) -> CanonicalCommit;
}

pub(crate) struct HelperMigration<'a> {
    pub(crate) transport: &'a mut dyn client::Transport,
    pub(crate) major: u32,
}
impl MigrationStore for HelperMigration<'_> {
    fn load(&mut self) -> CanonicalRead {
        load_helper_with(self.transport)
    }
    fn import(&mut self, session: &super::Session) -> CanonicalCommit {
        commit_session_with(
            session,
            true,
            super::SaveAuthority::Routine,
            self.major,
            self.transport,
        )
    }
    fn clear(&mut self) -> CanonicalCommit {
        let loaded = match client::load_with(self.transport) {
            Ok(value) => value,
            Err(error) => return CanonicalCommit::Failed(helper_error(error)),
        };
        let operation = match Generation::random() {
            Ok(operation) => operation,
            Err(_) => return CanonicalCommit::Failed(StoreError::HelperUnavailable),
        };
        helper_commit_with(
            self.transport,
            match &loaded {
                HelperLoad::Missing => None,
                HelperLoad::Present(snapshot) => expected(snapshot),
            },
            operation,
            WireMutation::ClearTenure {},
        )
    }
}

struct FileMigration;
impl MigrationStore for FileMigration {
    fn load(&mut self) -> CanonicalRead {
        load()
    }
    fn import(&mut self, session: &super::Session) -> CanonicalCommit {
        match serde_json::to_string(session) {
            Ok(payload) => commit_data(payload),
            Err(_) => CanonicalCommit::Failed(StoreError::InvalidSchema),
        }
    }
    fn clear(&mut self) -> CanonicalCommit {
        commit_cleared()
    }
}

pub(crate) struct Bootstrap {
    pub(crate) state: CanonicalRead,
    /// Runtime use must not label imported credentials saved before this receipt is durable.
    pub(crate) migration: Option<CanonicalCommit>,
    pub(crate) cleanup_failed: bool,
}

/// Files are inputs only. The caller chooses a canonical backend before entering this loop;
/// helper absence, timeout, corruption, locked auth and unknown schema never select FileMigration.
pub(crate) fn bootstrap_with(
    store: &mut dyn MigrationStore,
    opener: &mut dyn LegacyOpener,
    candidates: &[PathBuf],
) -> Bootstrap {
    let canonical = store.load();
    if let CanonicalRead::Pending { envelope, .. } = &canonical {
        return match decode_legacy_session(envelope.as_bytes(), opener) {
            Ok(legacy) => finish_import(store, legacy, None),
            Err(error) => blocked_bootstrap(error),
        };
    }
    if !matches!(canonical, CanonicalRead::Missing) {
        return Bootstrap {
            state: canonical,
            migration: None,
            cleanup_failed: false,
        };
    }
    for candidate in candidates {
        let bytes = match crate::storage::read_owned_bytes(candidate) {
            Ok(None) => continue,
            Ok(Some((bytes, true))) => bytes,
            Ok(Some((_, false))) => return blocked_bootstrap(StoreError::RecordUnsafeMode),
            Err(error) => return blocked_bootstrap(error),
        };
        let legacy = match decode_legacy_session(&bytes, opener) {
            Ok(value) => value,
            Err(error) => return blocked_bootstrap(error),
        };
        return finish_import(store, legacy, Some((candidate, &bytes)));
    }
    Bootstrap {
        state: CanonicalRead::Missing,
        migration: None,
        cleanup_failed: false,
    }
}

/// Compare each schema half without flattening opaque protected extensions into Session fields.
fn same_session_contents(expected: &super::Session, actual: &super::Session) -> bool {
    super::protected_fields_equal(expected, actual)
        && matches!((super::split_public(expected), super::split_public(actual)),
            (Ok(expected), Ok(actual)) if expected == actual)
}

fn finish_import(
    store: &mut dyn MigrationStore,
    legacy: LegacySession,
    source: Option<(&Path, &[u8])>,
) -> Bootstrap {
    let receipt = match &legacy {
        LegacySession::Data(session) => store.import(session),
        LegacySession::Cleared => store.clear(),
    };
    let readback = store.load();
    let exact = match (&legacy, &readback) {
        (LegacySession::Cleared, CanonicalRead::Cleared { .. }) => true,
        (
            LegacySession::Data(expected),
            CanonicalRead::Opened {
                session: actual, ..
            },
        ) => same_session_contents(expected, actual),
        (LegacySession::Data(expected), CanonicalRead::Data { payload, .. }) => {
            serde_json::from_str::<super::Session>(payload)
                .ok()
                .is_some_and(|actual| same_session_contents(expected, &actual))
        }
        _ => false,
    };
    let durable = matches!(receipt, CanonicalCommit::Durable { verified: true, .. });
    let cleanup_failed = durable
        && exact
        && source.is_some_and(|(candidate, bytes)| !retire_exact_candidate(candidate, bytes));
    let state = if durable && exact {
        readback
    } else {
        match receipt {
            CanonicalCommit::Failed(error) => CanonicalRead::Blocked(error),
            CanonicalCommit::ProtectionFailed(_) => CanonicalRead::Blocked(StoreError::AuthLocked),
            _ => CanonicalRead::Blocked(StoreError::HelperUnavailable),
        }
    };
    return Bootstrap {
        state,
        migration: Some(receipt),
        cleanup_failed,
    };
}

fn blocked_bootstrap(error: StoreError) -> Bootstrap {
    Bootstrap {
        state: CanonicalRead::Blocked(error),
        migration: None,
        cleanup_failed: false,
    }
}

fn retire_exact_candidate(path: &Path, expected: &[u8]) -> bool {
    if !matches!(crate::storage::read_owned_bytes(path), Ok(Some((ref current,true))) if current == expected)
    {
        return false;
    }
    if std::fs::remove_file(path).is_err() {
        return false;
    }
    path.parent()
        .and_then(|parent| std::fs::File::open(parent).ok())
        .is_some_and(|directory| directory.sync_all().is_ok())
}

/// The previous canonical JSON wrapper (`path()`, the pre-DB8 `session.json`) outranks older
/// bare candidates on ARM: it is a recognized migration source in its own right and must be
/// searched (on boot) and swept (on sign-out) ahead of the plain legacy files. Both call sites
/// that assemble an ARM candidate list must go through this one function so the two lists
/// cannot drift apart again.
fn insert_arm_canonical_wrapper(candidates: &mut Vec<PathBuf>) {
    candidates.insert(0, path());
}

pub(crate) fn bootstrap(opener: &mut dyn LegacyOpener) -> Bootstrap {
    #[cfg(not(test))]
    let candidates = crate::paths::session_migration_candidates();
    #[cfg(test)]
    let candidates = super::auth_paths();
    #[cfg(all(
        target_os = "linux",
        target_arch = "arm",
        not(feature = "hostsim"),
        not(test)
    ))]
    {
        let mut candidates = candidates;
        insert_arm_canonical_wrapper(&mut candidates);
        bootstrap_with(
            &mut HelperMigration {
                transport: &mut client::NativeTransport,
                major: crate::webos::info().major,
            },
            opener,
            &candidates,
        )
    }
    #[cfg(not(all(
        target_os = "linux",
        target_arch = "arm",
        not(feature = "hostsim"),
        not(test)
    )))]
    {
        bootstrap_with(&mut FileMigration, opener, &candidates)
    }
}

/// Called only after ClearTenure reports durable. Re-read the authority before removing legacy
/// residues; failure to retire a source remains visible but cannot reopen the cleared account.
pub(crate) fn cleanup_after_confirmed_clear() -> bool {
    if !matches!(load(), CanonicalRead::Cleared { .. }) {
        return false;
    }
    #[cfg(not(test))]
    let candidates = crate::paths::session_migration_candidates();
    #[cfg(test)]
    let candidates = super::auth_paths();
    #[cfg(all(
        target_os = "linux",
        target_arch = "arm",
        not(feature = "hostsim"),
        not(test)
    ))]
    let candidates = {
        let mut candidates = candidates;
        insert_arm_canonical_wrapper(&mut candidates);
        candidates
    };
    let mut complete = true;
    for candidate in candidates {
        match crate::storage::read_owned_bytes(&candidate) {
            Ok(None) => {}
            Ok(Some((bytes, true))) => {
                complete &= retire_exact_candidate(&candidate, &bytes);
            }
            _ => complete = false,
        }
    }
    complete
}
