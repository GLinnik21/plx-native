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

#[cfg(test)]
std::thread_local! {
    pub(super) static READ_FOR_TEST: std::cell::Cell<Option<fn() -> CanonicalRead>> =
        const { std::cell::Cell::new(None) };
}

pub(crate) fn load() -> CanonicalRead {
    #[cfg(test)]
    if let Some(read) = READ_FOR_TEST.with(|hook| hook.get()) {
        return read();
    }
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

/// Import an already decoded migration input through the same backend used by bootstrap.
pub(crate) fn migrate_session(session: &super::Session) -> CanonicalCommit {
    #[cfg(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test)))]
    {
        commit_session(session, true, super::SaveAuthority::Routine)
    }
    #[cfg(not(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(test))))]
    {
        write_session(session, super::SaveAuthority::Routine)
    }
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
    helper_commit_for_app_with(transport, expectation, operation, mutation)
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
    helper_commit_for_app_with(&mut client::NativeTransport, expected, operation, mutation)
}

fn helper_commit_for_app_with(
    transport: &mut dyn client::Transport,
    expected: Option<(&str, state::Expected)>,
    operation: Generation,
    mutation: WireMutation,
) -> CanonicalCommit {
    let flavor = match client::flavor() {
        Ok(flavor) => flavor,
        Err(error) => return CanonicalCommit::Failed(helper_error(error)),
    };
    helper_commit_with(transport, flavor, expected, operation, mutation)
}

fn helper_commit_with(
    transport: &mut dyn client::Transport,
    flavor: state::Flavor,
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
        }) => match serde_json::to_vec(&value)
            .ok()
            .and_then(|bytes| state::CanonicalState::decode(&bytes, flavor).ok())
        {
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

    /// Point the canonical persistence root at a directory of this test's own; restore the
    /// process-shared default on drop. This is a sibling of `session.rs`'s own
    /// `TempCanonicalRoot` (declared private, inside that module's own `#[cfg(test)] mod tests`,
    /// so not reachable from this module's sibling test mod) built the identical way, against the
    /// same `crate::paths::redirect_persistent_state_root_for_test` seam.
    struct TempPersistenceRoot {
        dir: PathBuf,
    }

    impl TempPersistenceRoot {
        fn new(tag: &str) -> TempPersistenceRoot {
            let dir = std::env::temp_dir().join(format!(
                "plxnative-persistence-cleanup-outcome-{}-{tag}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            crate::paths::redirect_persistent_state_root_for_test(Some(dir.clone()));
            TempPersistenceRoot { dir }
        }
    }

    impl Drop for TempPersistenceRoot {
        fn drop(&mut self) {
            crate::paths::redirect_persistent_state_root_for_test(None);
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Point the LEGACY sweep candidate `cleanup_after_confirmed_clear()`'s `#[cfg(test)]`
    /// `super::auth_paths()` resolves at a scratch file of this test's own, and restore the
    /// process-shared default (`session::redirect_for_test(None)`, i.e. `fallback_file()`) on
    /// drop — same RAII shape as [`TempPersistenceRoot`] above, one module over.
    ///
    /// This exists because `session::auth_paths()` under `#[cfg(test)]` returns exactly ONE
    /// candidate: the file a redirect names, or else `session::fallback_file()` — a
    /// process-`OnceLock` scratch path shared by the WHOLE test binary. A test that leaves
    /// `session::redirect_for_test(None)` in place therefore points the legacy sweep at that
    /// SAME shared file every other unredirected test in the process also reads, writes or has
    /// residue in — see the RED note on the test below, where planting a stray candidate there
    /// really does flip this test's own verdict.
    ///
    /// Deliberately narrower than `session.rs`'s own `TempSession`: this redirects only the
    /// legacy-candidate lookup (`session::redirect_for_test`), nothing else, so it can be paired
    /// with [`TempPersistenceRoot`] (the CANONICAL root) to exercise both halves of
    /// `cleanup_after_confirmed_clear()` — the canonical read via `load()`/`commit_cleared()` and
    /// the legacy sweep — each against its own isolated storage, with no shared state left in
    /// play. Do not also apply a `TempSession`-style redirect in a test using this guard; the two
    /// would fight over the same `TEST_FILE` global.
    struct TempLegacyCandidate {
        dir: PathBuf,
    }

    impl TempLegacyCandidate {
        fn new(tag: &str) -> TempLegacyCandidate {
            let dir = std::env::temp_dir().join(format!(
                "plxnative-persistence-legacy-candidate-{}-{tag}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a writable temp dir");
            super::super::redirect_for_test(Some(dir.join("auth.json")));
            TempLegacyCandidate { dir }
        }
    }

    impl Drop for TempLegacyCandidate {
        fn drop(&mut self) {
            super::super::redirect_for_test(None);
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// AUTH-09 Finding B regression: `ClearCleanupOutcome::AuthorityNotConfirmed` is a genuinely
    /// distinct outcome from `ClearCleanupOutcome::Confirmed` — the two failure/success modes a
    /// bare `bool` used to conflate before the AUTH-09 contract freeze (commit 4a9c8a27).
    ///
    /// **Three similarly-named types exist in this crate and must not be conflated; spelled out
    /// fully here so a future reader cannot confuse them:**
    /// - `crate::plex::session::persistence::ClearCleanupOutcome::AuthorityNotConfirmed` — THIS
    ///   enum, in THIS module: the verdict of one `cleanup_after_confirmed_clear()` call, i.e.
    ///   whether the canonical authority read back `Cleared` before any legacy sweep was
    ///   attempted. This is the type this test exercises.
    /// - `crate::plex::session::ClearOutcome::AuthorityNotConfirmed` — a DIFFERENT enum, declared
    ///   in the parent module `session.rs`, naming `clear()`'s verdict for its *whole* call.
    ///   `clear()`'s match on this module's `ClearCleanupOutcome` maps this exact variant onto
    ///   that one (see `session.rs`'s `clear()`), but they are two distinct types in two
    ///   different modules that merely share a variant name.
    /// - `crate::plex::session::async_persistence::ClearOutcome` — an unrelated, private STRUCT
    ///   in `async_persistence.rs` with the same short type name, used for the coordinator's
    ///   clear-completion bookkeeping. It shares nothing with either enum above beyond the name.
    ///
    /// **Reachability, investigated for this package:** `grep -rn
    /// inject_next_commit_failure_for_test rust-modules/src` finds exactly one fault-injection
    /// seam in this crate, and it is commit-stage-only — it makes a `commit()` call report
    /// failure, which routes into `CanonicalCommit`'s own failure variants (`Uncertain`/`Failed`/
    /// `ProtectionFailed`), never into a *load-side* discrepancy. There is no seam anywhere in the
    /// storage layer that can make `load()` disagree with a `commit_cleared()` that just landed —
    /// i.e. no way to reproduce, from a host test through the production `clear()` entry point,
    /// the genuine race `AuthorityNotConfirmed` exists to describe (a commit reported durable, but
    /// the read-back at cleanup time disagrees, e.g. because a concurrent re-login already
    /// overwrote it). A true end-to-end `clear() -> ClearOutcome::AuthorityNotConfirmed` is
    /// therefore NOT reachable here. This test instead proves the narrower, but still real and
    /// non-trivial, distinction directly against `cleanup_after_confirmed_clear()`: an authority
    /// that has never confirmed `Cleared` at all (a fresh, never-committed root — exactly what
    /// `load()` reports before any commit ever reaches it) versus one that genuinely has. No
    /// log-capture assertion is used; this crate has no log-capture mechanism.
    ///
    /// **RED state:** simulated, not historically observed — this is a new test locking in
    /// behavior that was already correct at this commit, not a regression that was ever shipped.
    /// Confirmed red by temporarily commenting out the early `return
    /// ClearCleanupOutcome::AuthorityNotConfirmed;` line at the top of
    /// `cleanup_after_confirmed_clear()` (so a fresh/`Missing` root falls through into the sweep
    /// loop instead of returning early), running only this test via `cargo +nightly test --lib
    /// plex::session::persistence::db8_policy_tests::cleanup_after_confirmed_clear_distinguishes_an_unconfirmed_authority_from_a_confirmed_one`,
    /// and observing the first assertion fail — the fall-through sweep finds zero candidates,
    /// trivially "completes", and reports `Confirmed` where the test asserts
    /// `AuthorityNotConfirmed` — then reverting the mutation. This was actually run live with
    /// cargo, not reasoned through: red observed on the mutated source, green after reverting.
    ///
    /// **Isolation, added separately (review Finding 3 on this lineage):** this test used to call
    /// `super::super::redirect_for_test(None)`, which points the legacy sweep at
    /// `session::fallback_file()` — a process-`OnceLock` path shared by the whole test binary —
    /// rather than at a fixture of its own. `TempPersistenceRoot` isolates the CANONICAL root but
    /// deliberately does not touch that lookup, so this test's own `Confirmed` assertion was
    /// deciding itself against whatever any other unredirected test in the process happened to
    /// leave at that shared path. **RED observed live, not simulated:** with the original
    /// `redirect_for_test(None)` restored and, immediately before the first assertion, a stray
    /// file written to `super::super::fallback_file()` with permissive (group/other-writable)
    /// mode bits — plausible residue, since `crate::storage::read_owned_bytes` treats such a mode
    /// as untrusted — `cargo +nightly test --lib
    /// plex::session::persistence::db8_policy_tests::cleanup_after_confirmed_clear_distinguishes_an_unconfirmed_authority_from_a_confirmed_one`
    /// failed the SECOND assertion: `left: LegacyRetireFailed, right: Confirmed`. That is this
    /// test's own verdict, not a different test's, so the experiment discriminates the isolation
    /// bug directly rather than merely tripping something else. [`TempLegacyCandidate`] (added
    /// alongside this test) removes the hazard by giving the legacy candidate a scratch file of
    /// its own; with it in place the identical stray-planting experiment against
    /// `super::super::fallback_file()` no longer touches this test's verdict, because
    /// `auth_paths()` under `#[cfg(test)]` never resolves there once redirected. The planting
    /// edit itself was reverted before this test was left in its final shape.
    #[test]
    fn cleanup_after_confirmed_clear_distinguishes_an_unconfirmed_authority_from_a_confirmed_one() {
        let _serial = crate::testlock::serial();
        let _root = TempPersistenceRoot::new("clear-cleanup-outcome");
        let _legacy = TempLegacyCandidate::new("clear-cleanup-outcome");

        // Fresh root: nothing has ever been committed, so the authority cannot read back
        // `Cleared` yet — this is the "unconfirmed" premise `AuthorityNotConfirmed` exists for.
        assert!(
            matches!(load(), CanonicalRead::Missing),
            "setup: a freshly redirected root must read back Missing, or the two assertions \
             below do not isolate what they claim to"
        );
        assert_eq!(
            cleanup_after_confirmed_clear(),
            ClearCleanupOutcome::AuthorityNotConfirmed,
            "an authority that has never confirmed Cleared must be reported distinctly from one \
             that has, not silently treated as Confirmed"
        );

        // Now make the premise true: commit an actual durable Cleared record, and confirm the
        // cleanup step's verdict flips to the other, genuinely distinct, variant.
        assert!(
            matches!(commit_cleared(), CanonicalCommit::Durable { .. }),
            "setup: the seed commit_cleared() must land durably or the Confirmed case below \
             proves nothing"
        );
        assert!(
            matches!(load(), CanonicalRead::Cleared { .. }),
            "setup: the authority must read back Cleared after a durable commit_cleared()"
        );
        assert_eq!(
            cleanup_after_confirmed_clear(),
            ClearCleanupOutcome::Confirmed,
            "a confirmed Cleared authority with no unswept legacy candidate must report \
             Confirmed, distinct from AuthorityNotConfirmed"
        );
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
    /// A neutralized sign-out candidate; neither an import nor a canonical clear request.
    Missing,
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
    let mut value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| StoreError::InvalidSchema)?;
    if value.is_null() {
        return Ok(LegacySession::Missing);
    }
    if !value.is_object() {
        return Err(StoreError::InvalidSchema);
    }
    value
        .as_object_mut()
        .unwrap()
        .remove(super::FALLBACK_MARKER);
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
        helper_commit_for_app_with(
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
    let fallback_revoked = super::fallback_revoked_at(candidates);
    for candidate in candidates {
        let bytes = match crate::storage::read_owned_bytes(candidate) {
            Ok(None) => continue,
            Ok(Some((bytes, true))) => bytes,
            Ok(Some((_, false))) => return blocked_bootstrap(StoreError::RecordUnsafeMode),
            Err(error) => return blocked_bootstrap(error),
        };
        if fallback_revoked && super::marked_fallback(&bytes) { continue; }
        let legacy = match decode_legacy_session(&bytes, opener) {
            Ok(LegacySession::Missing) => continue,
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
        LegacySession::Missing => return Bootstrap {
            state: CanonicalRead::Missing,
            migration: None,
            cleanup_failed: false,
        },
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

/// The result of [`cleanup_after_confirmed_clear`], distinguishing the two failure modes that a
/// bare `bool` used to conflate (AUTH-09 Finding B): the authority read-back disagreeing with the
/// caller's premise that the clear already landed durably, versus the read-back confirming
/// `Cleared` but a recognized legacy migration candidate surviving the sweep. The two are not
/// interchangeable — the first means the clear itself may not be trustworthy yet (or a concurrent
/// re-login already overwrote it), the second means the clear is real and only a residue file is
/// left over. `session::ClearOutcome` (session.rs) carries a distinct variant for the first case
/// specifically so `erase()`'s `matches!(.., Durable { .. })` check cannot accidentally match it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClearCleanupOutcome {
    /// The authority read back `Cleared` and every recognized legacy migration candidate was
    /// retired (removed or neutralized with a synced tombstone), or was already absent.
    Confirmed,
    /// The authority did NOT read back `Cleared` at the moment of this call — the caller's premise
    /// that the clear already committed durably could not be confirmed here. No legacy sweep was
    /// attempted.
    AuthorityNotConfirmed,
    /// The authority read back `Cleared`, but at least one recognized legacy migration candidate
    /// could not be retired (an unreadable, non-regular, or otherwise un-removable file). The
    /// clear itself is real, but a marked fallback residue could reopen if the helper later
    /// becomes unreachable. Report this failure and retry retirement on the next sign-out.
    LegacyRetireFailed,
}

/// Called only after ClearTenure reports durable. Re-read the authority before removing legacy
/// residues. Failed retirement remains visible: canonical Cleared shadows a residue only while
/// the authority can answer, so a marked fallback must also be removed or neutralized.
pub(crate) fn cleanup_after_confirmed_clear() -> ClearCleanupOutcome {
    if !matches!(load(), CanonicalRead::Cleared { .. }) {
        return ClearCleanupOutcome::AuthorityNotConfirmed;
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
    let mut complete = super::retry_pending_retirements_locked();
    for candidate in candidates {
        match crate::storage::read_owned_bytes(&candidate) {
            Ok(None) => {}
            Ok(Some((bytes, true))) => {
                // Preserve the exact-source fence used by migration retirement.
                complete &= matches!(crate::storage::read_owned_bytes(&candidate),
                    Ok(Some((ref current, true))) if current == &bytes)
                    && super::retire_session_candidate(&candidate);
            }
            _ => complete = false,
        }
    }
    if complete {
        ClearCleanupOutcome::Confirmed
    } else {
        ClearCleanupOutcome::LegacyRetireFailed
    }
}

#[cfg(test)]
mod commit_flavor_tests {
    use super::*;

    fn commit_response(response_flavor: state::Flavor) -> CanonicalCommit {
        let state = state::CanonicalState::new(response_flavor, Generation([1; 16]));
        let mut transport = |request| {
            assert!(matches!(
                request,
                crate::storage::wire::Request::Commit { .. }
            ));
            Ok(Response::Commit {
                status: CommitStatus::Committed,
                db_rev: Some("1".into()),
                state: Some(serde_json::to_value(&state).unwrap()),
                applied: Some(state::Applied {
                    revision: 7,
                    epoch: state.epoch,
                    auth_generation: state.auth_generation,
                }),
                verified: true,
                protection: None,
            })
        };
        helper_commit_with(
            &mut transport,
            state::Flavor::Nightly,
            None,
            Generation([2; 16]),
            WireMutation::ClearTenure {},
        )
    }

    #[test]
    fn nightly_commit_accepts_nightly_state() {
        assert!(matches!(
            commit_response(state::Flavor::Nightly),
            CanonicalCommit::Durable {
                revision: 7,
                verified: true,
                protection: None
            }
        ));
    }

    #[test]
    fn nightly_commit_rejects_stable_state() {
        assert!(matches!(
            commit_response(state::Flavor::Stable),
            CanonicalCommit::Failed(StoreError::InvalidSchema)
        ));
    }
}
