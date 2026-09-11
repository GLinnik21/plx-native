//! Canonical Session record adapter.
//!
//! This module owns the distinction between the versioned canonical record and the legacy
//! filename candidates. It deliberately does not deserialize Session JSON: migration carries the
//! exact legacy UTF-8 payload into Record::data before the domain reader is allowed to rewrite it.

use crate::storage::{
    CommitReceipt, CommitStage, Record, RecordKey, RecordState, RecordStore, StoreError,
};
use std::path::{Path, PathBuf};

pub(crate) enum CanonicalRead {
    Missing,
    Data { revision: u64, payload: String },
    Cleared { revision: u64 },
    Blocked(StoreError),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CanonicalCommit {
    Durable { revision: u64 },
    Uncertain { stage: CommitStage, errno: i32 },
    Failed(StoreError),
}

pub(crate) fn root() -> PathBuf {
    crate::paths::persistent_state_root()
}

pub(crate) fn path() -> PathBuf {
    root().join("session.json")
}

pub(crate) fn cleanup_temporaries() -> Result<(), StoreError> {
    store()?.cleanup(RecordKey::Session)
}

fn store() -> Result<impl RecordStore, StoreError> {
    crate::paths::ensure_persistent_state_root().map_err(|error| StoreError::Io {
        stage: crate::storage::CommitStage::ParentOpen,
        errno: error.raw_os_error().unwrap_or(0),
    })?;
    crate::storage::open(root())
}

pub(crate) fn load() -> CanonicalRead {
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

fn commit(record: Record) -> CanonicalCommit {
    let revision = record.revision;
    let store = match store() {
        Ok(store) => store,
        Err(error) => return CanonicalCommit::Failed(error),
    };
    match store.commit(RecordKey::Session, &record) {
        Ok(CommitReceipt::Durable) => CanonicalCommit::Durable { revision },
        Ok(CommitReceipt::Uncertain { stage, errno }) => {
            CanonicalCommit::Uncertain { stage, errno }
        }
        Err(error) => CanonicalCommit::Failed(error),
    }
}

pub(crate) fn commit_data(payload: String) -> CanonicalCommit {
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

pub(crate) fn commit_cleared() -> CanonicalCommit {
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
