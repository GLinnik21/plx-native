//! Fixed-ID DB8 CAS with typed state transitions and operation-ledger reconciliation.
use super::{
    keymanager::{self, Rpc},
    wire::*,
};
use crate::state::{
    self, Applied, CanonicalState, Flavor, Generation, Mutation, Operation, OperationStatus,
    StateError,
};
use serde_json::{json, Value};

static LAST_ERROR_STAGE: std::sync::Mutex<&'static str> = std::sync::Mutex::new("none");

fn remember_stage(stage: &'static str) {
    *LAST_ERROR_STAGE.lock().unwrap_or_else(|e| e.into_inner()) = stage;
}

pub fn last_error_stage() -> &'static str {
    *LAST_ERROR_STAGE.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(all(target_os = "linux", target_arch = "arm"))]
fn stage_error(stage: &'static str, error: ErrorCode) -> ErrorCode {
    remember_stage(stage);
    use std::ffi::CString;
    if let Ok(message) = CString::new(format!("plxstorage stage={stage} code={error:?}")) {
        unsafe {
            libc::syslog(
                libc::LOG_USER | libc::LOG_ERR,
                b"%s\0".as_ptr().cast(),
                message.as_ptr(),
            );
        }
    }
    error
}

#[cfg(not(all(target_os = "linux", target_arch = "arm")))]
fn stage_error(_stage: &'static str, error: ErrorCode) -> ErrorCode {
    remember_stage(_stage);
    error
}

fn load_stage(error: ErrorCode) -> ErrorCode {
    let stage = match error {
        ErrorCode::Invalid => "load_invalid",
        ErrorCode::Unavailable | ErrorCode::Capability => "load_unavailable",
        ErrorCode::Timeout => "load_timeout",
        ErrorCode::Authentication => "load_authentication",
        ErrorCode::Protocol => "load_protocol",
        ErrorCode::Corrupt => "load_corrupt",
    };
    stage_error(stage, error)
}

pub struct Backend<R> {
    pub rpc: R,
    pub flavor: Flavor,
    pub service: String,
    pending_cleanup: Vec<RetiredKey>,
    // Request-local closed evidence, consumed by dispatch. Never retain payloads/service text.
    keymanager_error: Option<(KeymanagerFailure, AuthPreservation)>,
}
struct RetiredKey {
    operation: Generation,
    digest: String,
    generation: Generation,
    name: String,
}
pub fn generation(text: &str) -> Result<Generation, ErrorCode> {
    if text.len() != 32
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(ErrorCode::Invalid);
    }
    let mut bytes = [0; 16];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).map_err(|_| ErrorCode::Invalid)?;
    }
    Ok(Generation(bytes))
}
fn exact_revision(text: &str) -> Result<u64, ErrorCode> {
    let value = text.parse::<u64>().map_err(|_| ErrorCode::Invalid)?;
    if value.to_string() != text {
        return Err(ErrorCode::Invalid);
    }
    Ok(value)
}
fn translate(error: StateError) -> ErrorCode {
    match error {
        StateError::RandomUnavailable => ErrorCode::Unavailable,
        _ => ErrorCode::Invalid,
    }
}
impl<R: Rpc> Backend<R> {
    pub fn new(rpc: R, flavor: Flavor, service: String) -> Self {
        Self {
            rpc,
            flavor,
            service,
            pending_cleanup: Vec::new(),
            keymanager_error: None,
        }
    }
    pub fn setup(&mut self) -> Result<(), ErrorCode> {
        // The registered service is both the DB8 caller and the private kind owner. This exact
        // identity passed the packaged helper probe on webOS 4; changing owner to the parent app
        // is rejected when the helper later repeats putKind for the existing kind.
        let result = self.rpc.call(
            "luna://com.palm.db/putKind",
            &json!({"id":format!("{}:1",self.service),"owner":self.service,"private":true,"indexes":[]}),
        )?;
        if result["returnValue"] == true {
            Ok(())
        } else {
            Err(ErrorCode::Unavailable)
        }
    }
    pub fn load(&mut self) -> Result<Option<(u64, CanonicalState)>, ErrorCode> {
        let reply = self.rpc.call(
            "luna://com.palm.db/get",
            &json!({"ids":[self.flavor.object_id()]}),
        )?;
        if reply["returnValue"] != true {
            return Err(ErrorCode::Unavailable);
        }
        let objects = reply["results"].as_array().ok_or(ErrorCode::Corrupt)?;
        if objects.is_empty() {
            return Ok(None);
        }
        if objects.len() != 1 {
            return Err(ErrorCode::Corrupt);
        }
        let mut object = objects[0].clone();
        let fields = object.as_object_mut().ok_or(ErrorCode::Corrupt)?;
        if fields.remove("_kind") != Some(json!(format!("{}:1", self.service))) {
            return Err(ErrorCode::Corrupt);
        }
        let rev = fields
            .remove("_rev")
            .and_then(|v| v.as_u64())
            .ok_or(ErrorCode::Corrupt)?;
        let bytes = if fields.contains_key("state") {
            // Keep the canonical JSON opaque: older DB8 adds _id to objects in arrays.
            // The wrapper has a closed schema and must address the same fixed ID as its state.
            if fields.len() != 2 || fields.get("_id") != Some(&json!(self.flavor.object_id())) {
                return Err(ErrorCode::Corrupt);
            }
            fields
                .get("state")
                .and_then(Value::as_str)
                .ok_or(ErrorCode::Corrupt)?
                .as_bytes()
                .to_vec()
        } else {
            // Recover the earlier flattened candidate once. Only ledger entries have a closed
            // object schema inside an array; never strip _id from arbitrary application Values.
            // A successful later CAS rewrites this record using the opaque wrapper.
            if let Some(entries) = fields.get_mut("operations").and_then(Value::as_array_mut) {
                for entry in entries {
                    if let Some(fields) = entry.as_object_mut() {
                        if let Some(id) = fields.remove("_id") {
                            if !matches!(id.as_str(), Some(id) if !id.is_empty()) {
                                return Err(ErrorCode::Corrupt);
                            }
                        }
                    }
                }
            }
            serde_json::to_vec(&object).map_err(|_| ErrorCode::Corrupt)?
        };
        let state = CanonicalState::decode(&bytes, self.flavor).map_err(|_| ErrorCode::Corrupt)?;
        Ok(Some((rev, state)))
    }
    pub fn dispatch(&mut self, request: Request) -> Response {
        remember_stage("none");
        self.keymanager_error = None;
        let result = check_frame_size(&request)
            .map_err(|error| stage_error("request_size", error))
            .and_then(|_| self.try_dispatch(request));
        match result.and_then(|response| {
            check_frame_size(&response).map_err(|error| stage_error("response_size", error))?;
            Ok(response)
        }) {
            Ok(r) => r,
            Err(code) => {
                if let Some((failure, preservation)) = self.keymanager_error.take() {
                    return Response::KeymanagerError {
                        failure,
                        preservation,
                        db8_commit_verified: false,
                    };
                }
                if last_error_stage() == "none" {
                    remember_stage("dispatch");
                }
                Response::Error { code }
            }
        }
    }
    fn try_dispatch(&mut self, request: Request) -> Result<Response, ErrorCode> {
        if let Request::Commit { digest, .. } = &request {
            let bytes = request_digest_bytes(&request)
                .map_err(|error| stage_error("request_digest", error))?;
            if state::digest_bytes(&bytes) != *digest {
                return Err(stage_error("digest_mismatch", ErrorCode::Invalid));
            }
        }
        match request {
            Request::Load {} => match self.load().map_err(load_stage)? {
                None => Ok(Response::Missing),
                Some((rev, state)) => {
                    let opened = if state.auth_envelope.is_some() {
                        self.authenticate_current_auth(&state).map(Some)
                    } else {
                        Ok(None)
                    };
                    let auth = match opened {
                        Ok(Some(payload)) => AuthLoad::Plaintext {
                            payload: SecretString(payload),
                        },
                        Ok(None) => AuthLoad::None,
                        Err(code) => AuthLoad::Locked { code },
                    };
                    Ok(Response::Loaded {
                        db_rev: rev.to_string(),
                        protection: protection_outcome(&state),
                        state: serde_json::to_value(state).map_err(|_| ErrorCode::Invalid)?,
                        auth,
                    })
                }
            },
            Request::Reconcile {
                operation_id,
                digest,
            } => {
                let id = generation(&operation_id)
                    .map_err(|error| stage_error("reconcile_id", error))?;
                let result = self
                    .load()
                    .map_err(|error| stage_error("reconcile_load", error))?;
                if let Some((rev, state)) = &result {
                    if let Some(pending) = &state.pending_auth {
                        if pending.operation.id == id {
                            if pending.operation.digest != digest {
                                return Err(ErrorCode::Invalid);
                            }
                            return match self.finish_pending(*rev, state.clone())? {
                                Response::Commit {
                                    status: CommitStatus::Committed,
                                    db_rev,
                                    applied,
                                    protection,
                                    ..
                                } => Ok(Response::Reconcile {
                                    status: ReconcileStatus::Applied,
                                    db_rev,
                                    applied,
                                    protection,
                                }),
                                response @ Response::KeymanagerError { .. } => Ok(response),
                                _ => Ok(Response::Reconcile {
                                    status: ReconcileStatus::Unknown,
                                    db_rev: Some(rev.to_string()),
                                    applied: None,
                                    protection: None,
                                }),
                            };
                        }
                    }
                }
                let (status, db_rev, applied, protection) = match result {
                    Some((rev, state)) => {
                        let (status, applied) = match state
                            .operation_status(id, &digest)
                            .map_err(|error| stage_error("reconcile_ledger", translate(error)))?
                        {
                            OperationStatus::Applied(applied) => {
                                if replay_needs_auth(&state, id)
                                    && self.authenticate_current_auth(&state).is_err()
                                {
                                    (ReconcileStatus::Unknown, None)
                                } else {
                                    self.cleanup_confirmed(id, &digest, &state);
                                    (ReconcileStatus::Applied, Some(applied))
                                }
                            }
                            OperationStatus::Unknown => (ReconcileStatus::Unknown, None),
                        };
                        let protection = applied.as_ref().and_then(|_| protection_outcome(&state));
                        (status, Some(rev.to_string()), applied, protection)
                    }
                    None => (ReconcileStatus::Unknown, None, None, None),
                };
                Ok(Response::Reconcile {
                    status,
                    db_rev,
                    applied,
                    protection,
                })
            }
            Request::Commit {
                expected,
                operation_id,
                digest,
                mutation,
            } => self.commit(expected, operation_id, digest, mutation),
            Request::Hello { .. } => Err(ErrorCode::Protocol),
        }
    }
    fn commit(
        &mut self,
        expected: Expectation,
        operation_id: String,
        digest: String,
        mutation: WireMutation,
    ) -> Result<Response, ErrorCode> {
        let id = generation(&operation_id).map_err(|error| stage_error("operation_id", error))?;
        let loaded = self
            .load()
            .map_err(|error| stage_error("load_before_commit", error))?;
        if let Some((rev, state)) = &loaded {
            if let Some(pending) = &state.pending_auth {
                if pending.operation.id == id {
                    if pending.operation.digest != digest {
                        return Err(ErrorCode::Invalid);
                    }
                    return self.finish_pending(*rev, state.clone());
                }
            }
            if let OperationStatus::Applied(applied) = state
                .operation_status(id, &digest)
                .map_err(|error| stage_error("ledger", translate(error)))?
            {
                if let Some(expected_plaintext) = fresh_plaintext(&mutation) {
                    let opened = self.authenticate_current_auth(state);
                    let authenticated = match opened {
                        Ok(opened) => {
                            applied.auth_generation != state.auth_generation
                                || typed_plaintext_equal(Some(&opened), expected_plaintext)
                        }
                        Err(_) => false,
                    };
                    if !authenticated {
                        return receipt(CommitStatus::Unavailable, Some(*rev), None);
                    }
                }
                self.cleanup_confirmed(id, &digest, state);
                return committed(*rev, state, applied, false);
            }
        }
        let (revision, state) = match (&expected, loaded) {
            (Expectation::Missing {}, None) => (
                None,
                CanonicalState::new(self.flavor, Generation::random().map_err(translate)?),
            ),
            (
                Expectation::Present {
                    db_rev,
                    epoch,
                    auth_generation,
                },
                Some((rev, state)),
            ) if exact_revision(db_rev)
                .map_err(|error| stage_error("expected_db_rev", error))?
                == rev
                && exact_revision(epoch)
                    .map_err(|error| stage_error("expected_epoch", error))?
                    == state.epoch
                && generation(auth_generation)
                    .map_err(|error| stage_error("expected_auth_generation", error))?
                    == state.auth_generation =>
            {
                (Some(rev), state)
            }
            _ => return receipt(CommitStatus::Conflict, None, None),
        };
        let plaintext = fresh_plaintext(&mutation).map(str::to_owned);
        let fallback_reason = match &mutation {
            WireMutation::AdvanceMigration {
                migration: MigrationMutation::SessionComplete { .. },
            } => FallbackReason::LegacyImport,
            _ => FallbackReason::FreshLogin,
        };
        let strict = matches!(
            &mutation,
            WireMutation::ReplaceAuth {
                protection: ProtectionRequest::KeymanagerRequired,
                ..
            }
        );
        let allow_fallback = matches!(
            &mutation,
            WireMutation::ReplaceAuth {
                protection: ProtectionRequest::KeymanagerWithAclFallback,
                ..
            } | WireMutation::AdvanceMigration {
                migration: MigrationMutation::SessionComplete {
                    protection: ProtectionRequest::KeymanagerWithAclFallback,
                    ..
                }
            }
        );
        let authenticate_readback = matches!(
            mutation,
            WireMutation::ReplaceAuth { .. } | WireMutation::AdvanceMigration { .. }
        );
        let prepared = self
            .prepare(&state, mutation)
            .map_err(|error| stage_error("prepare", error))?;
        let created_key = prepared_key(&prepared);
        let operation = Operation {
            id,
            digest: digest.clone(),
            expected: state.expected(),
            mutation: prepared,
        };
        let (mut next, applied) = match state.apply_verified_digest(&operation) {
            Ok(applied) => applied,
            Err(error) => {
                if let Some(name) = &created_key {
                    keymanager::remove_retired(&mut self.rpc, name);
                }
                return if error == StateError::Conflict {
                    receipt(CommitStatus::Conflict, revision, None)
                } else {
                    Err(stage_error("apply", translate(error)))
                };
            }
        };
        if let Some(plaintext) = plaintext.as_deref() {
            next.operations
                .last_mut()
                .ok_or(ErrorCode::Corrupt)?
                .verification = Some(state::ReceiptVerification::Auth {
                plaintext_digest: Some(plaintext_digest(plaintext)),
            });
        }
        if strict {
            return self.stage_strict(
                revision,
                state,
                operation,
                plaintext.as_deref().ok_or(ErrorCode::Invalid)?,
            );
        }
        let encoded = String::from_utf8(
            next.encode()
                .map_err(|error| stage_error("encode", translate(error)))
                .inspect_err(|_| {
                    if let Some(name) = &created_key {
                        keymanager::remove_retired(&mut self.rpc, name);
                    }
                })?,
        )
        .map_err(|_| stage_error("encode_json", ErrorCode::Invalid))?;
        let mut object = json!({
            "_id": self.flavor.object_id(),
            "_kind": format!("{}:1", self.service),
            "state": encoded
        });
        if let Some(rev) = revision {
            object["_rev"] = json!(rev);
        }
        self.remember_retired(&state, &next, id, &digest);
        let put = self
            .rpc
            .call("luna://com.palm.db/put", &json!({"objects":[object]}));
        if matches!(&put, Ok(reply) if reply["returnValue"] == false) {
            // An explicit DB8 rejection proves this newly created generation was not installed.
            // A lost/malformed acknowledgement never supplies that proof.
            if let Some(name) = &created_key {
                keymanager::remove_retired(&mut self.rpc, name);
            }
            self.pending_cleanup
                .retain(|entry| entry.operation != id || entry.digest != digest);
        }
        // Includes create-ID collision, stale _rev, and lost acknowledgement. Never force a
        // retry: reread the exact ID and let the operation ledger establish what happened.
        match self.load() {
            Ok(Some((rev, actual))) => {
                match actual
                    .operation_status(id, &digest)
                    .map_err(|error| stage_error("readback_ledger", translate(error)))?
                {
                    OperationStatus::Applied(original) => {
                        let mut verified = actual == next && original == applied;
                        if verified && authenticate_readback {
                            if allow_fallback && created_key.is_some() {
                                let expected = plaintext.as_deref().ok_or(ErrorCode::Invalid)?;
                                let context = serde_json::to_value(actual.auth_context())
                                    .map_err(|_| ErrorCode::Invalid)?;
                                let opened = keymanager::unseal_detailed(
                                    &mut self.rpc,
                                    &actual.auth_generation.key_name(),
                                    actual.auth_envelope.as_deref().ok_or(ErrorCode::Corrupt)?,
                                    &context,
                                    KeymanagerOperation::Readback,
                                );
                                let failure = match opened {
                                    Ok(opened)
                                        if typed_plaintext_equal(Some(&opened), expected) =>
                                    {
                                        None
                                    }
                                    Ok(_) => Some(keymanager::failure(
                                        KeymanagerOperation::Readback,
                                        KeymanagerStage::Roundtrip,
                                        ErrorCode::Corrupt,
                                    )),
                                    Err(failure) => Some(failure),
                                };
                                if let Some(failure) = failure {
                                    return self.retry_readback_acl(
                                        rev,
                                        actual,
                                        original,
                                        expected,
                                        failure,
                                        fallback_reason,
                                        id,
                                        &digest,
                                    );
                                }
                            } else {
                                match self.open_auth(&actual) {
                                    Ok(opened) => {
                                        if let Some(expected) = plaintext.as_deref() {
                                            if !typed_plaintext_equal(opened.as_deref(), expected) {
                                                return Err(ErrorCode::Corrupt);
                                            }
                                        }
                                    }
                                    Err(error) if plaintext.is_some() => {
                                        return Err(stage_error("open_readback", error))
                                    }
                                    Err(_) => verified = false,
                                }
                            }
                        }
                        if !verified
                            && plaintext.is_some()
                            && self.authenticate_current_auth(&actual).is_err()
                        {
                            return receipt(CommitStatus::Unavailable, Some(rev), None);
                        }
                        self.cleanup_confirmed(id, &digest, &actual);
                        if verified {
                            self.cleanup_cancelled_pending(&state, &actual);
                        }
                        committed(rev, &actual, original, verified)
                            .map_err(|error| stage_error("commit_response", error))
                    }
                    OperationStatus::Unknown => receipt(
                        if Some(rev) != revision {
                            CommitStatus::Conflict
                        } else {
                            CommitStatus::Unavailable
                        },
                        Some(rev),
                        None,
                    ),
                }
            }
            _ => receipt(CommitStatus::Unavailable, None, None),
        }
    }
    fn authenticate_current_auth(&mut self, state: &CanonicalState) -> Result<String, ErrorCode> {
        let opened = self.open_auth(state)?.ok_or(ErrorCode::Authentication)?;
        // A successfully decoded but incorrect plaintext is not authentication proof. This
        // digest survives helper restarts, rejected ACL repairs, and later public operations.
        if let Some(expected) = state.operations.iter().rev().find_map(|entry| {
            if entry.result.auth_generation != state.auth_generation {
                return None;
            }
            match &entry.verification {
                Some(state::ReceiptVerification::Auth {
                    plaintext_digest: Some(digest),
                }) => Some(digest),
                _ => None,
            }
        }) {
            if plaintext_digest(&opened) != *expected {
                return Err(ErrorCode::Corrupt);
            }
        }
        Ok(opened)
    }
    /// The first write is known to be committed but failed authentication. Repair only that
    /// exact DB8 revision, retaining its generation and ledger receipt. A concurrent writer or
    /// uncertain repair cannot authorize durability or key/source cleanup.
    fn retry_readback_acl(
        &mut self,
        rev: u64,
        actual: CanonicalState,
        applied: Applied,
        plaintext: &str,
        failure: KeymanagerFailure,
        reason: FallbackReason,
        id: Generation,
        digest: &str,
    ) -> Result<Response, ErrorCode> {
        let created_key = actual.auth_generation.key_name();
        let mut repaired = actual;
        let context =
            serde_json::to_value(repaired.auth_context()).map_err(|_| ErrorCode::Invalid)?;
        repaired.auth_envelope = Some(acl_envelope(
            context,
            plaintext,
            Some((
                failure,
                FallbackContext {
                    reason,
                    phase: FallbackPhase::PostwriteReadbackRepair,
                },
            )),
        ));
        let encoded = String::from_utf8(repaired.encode().map_err(translate)?)
            .map_err(|_| ErrorCode::Invalid)?;
        let object = json!({"_id":self.flavor.object_id(),"_kind":format!("{}:1", self.service),"_rev":rev,"state":encoded});
        let _ = self
            .rpc
            .call("luna://com.palm.db/put", &json!({"objects":[object]}));
        match self.load() {
            Ok(Some((revision, current))) if current == repaired => {
                let opened = self.open_auth(&current)?;
                if !typed_plaintext_equal(opened.as_deref(), plaintext) {
                    return Err(ErrorCode::Corrupt);
                }
                self.cleanup_confirmed(id, digest, &current);
                keymanager::remove_retired(&mut self.rpc, &created_key);
                committed(revision, &current, applied, true)
            }
            Ok(Some((revision, _))) => receipt(
                if revision != rev {
                    CommitStatus::Conflict
                } else {
                    CommitStatus::Unavailable
                },
                Some(revision),
                None,
            ),
            _ => receipt(CommitStatus::Unavailable, None, None),
        }
    }
    /// Phase one stores only a pending encrypted operation. Old auth remains active across
    /// helper exit/power loss. No Applied receipt exists until phase two promotes it.
    fn stage_strict(
        &mut self,
        revision: Option<u64>,
        mut active: CanonicalState,
        operation: Operation,
        plaintext: &str,
    ) -> Result<Response, ErrorCode> {
        let name = prepared_key(&operation.mutation).ok_or(ErrorCode::Invalid)?;
        let prior = active.clone();
        active.pending_auth = Some(state::PendingAuth {
            operation,
            plaintext_digest: plaintext_digest(plaintext),
        });
        if let Err(error) = active.encode() {
            keymanager::remove_retired(&mut self.rpc, &name);
            return Err(translate(error));
        }
        let put = self.put_state(revision, &active);
        if matches!(&put, Ok(reply) if reply["returnValue"] == false) {
            keymanager::remove_retired(&mut self.rpc, &name);
        }
        match self.load() {
            Ok(Some((rev, staged))) if staged == active => {
                self.cleanup_cancelled_pending(&prior, &staged);
                self.finish_pending(rev, staged)
            }
            _ => receipt(CommitStatus::Unavailable, None, None),
        }
    }
    fn put_state(
        &mut self,
        revision: Option<u64>,
        state: &CanonicalState,
    ) -> Result<Value, ErrorCode> {
        let encoded = String::from_utf8(state.encode().map_err(translate)?)
            .map_err(|_| ErrorCode::Invalid)?;
        let mut object = json!({"_id":self.flavor.object_id(),"_kind":format!("{}:1",self.service),"state":encoded});
        if let Some(rev) = revision {
            object["_rev"] = json!(rev);
        }
        self.rpc
            .call("luna://com.palm.db/put", &json!({"objects":[object]}))
    }
    /// Resume from persisted evidence alone. Authenticate the staged bytes first, then promote
    /// them and their original ledger receipt in one exact-revision CAS. A timeout never proves
    /// cancellation; both generations remain until the final candidate is read back exactly.
    fn finish_pending(&mut self, rev: u64, staged: CanonicalState) -> Result<Response, ErrorCode> {
        let pending = staged.pending_auth.as_ref().ok_or(ErrorCode::Invalid)?;
        let (candidate, applied) = staged
            .pending_candidate()
            .map_err(translate)?
            .ok_or(ErrorCode::Invalid)?;
        let context =
            serde_json::to_value(candidate.auth_context()).map_err(|_| ErrorCode::Invalid)?;
        let opened = keymanager::unseal_detailed(
            &mut self.rpc,
            &candidate.auth_generation.key_name(),
            candidate
                .auth_envelope
                .as_deref()
                .ok_or(ErrorCode::Corrupt)?,
            &context,
            KeymanagerOperation::Readback,
        );
        let failure = match opened {
            Ok(plaintext) if plaintext_digest(&plaintext) == pending.plaintext_digest => None,
            Ok(_) => Some(keymanager::failure(
                KeymanagerOperation::Readback,
                KeymanagerStage::Roundtrip,
                ErrorCode::Corrupt,
            )),
            Err(failure) => Some(failure),
        };
        if let Some(failure) = failure {
            return Ok(Response::KeymanagerError {
                failure,
                preservation: AuthPreservation::Unchanged,
                db8_commit_verified: true,
            });
        }
        let _ = self.put_state(Some(rev), &candidate);
        match self.load() {
            Ok(Some((revision, actual))) if actual == candidate => {
                self.remember_retired(
                    &staged,
                    &actual,
                    pending.operation.id,
                    &pending.operation.digest,
                );
                self.cleanup_confirmed(pending.operation.id, &pending.operation.digest, &actual);
                committed(revision, &actual, applied, true)
            }
            _ => receipt(CommitStatus::Unavailable, None, None),
        }
    }
    fn cleanup_cancelled_pending(&mut self, old: &CanonicalState, current: &CanonicalState) {
        let Some(pending) = &old.pending_auth else {
            return;
        };
        let Some(name) = prepared_key(&pending.operation.mutation) else {
            return;
        };
        if !references_key(current, &name) {
            keymanager::remove_retired(&mut self.rpc, &name);
        }
    }
    fn remember_retired(
        &mut self,
        old: &CanonicalState,
        next: &CanonicalState,
        id: Generation,
        digest: &str,
    ) {
        if old.auth_generation == next.auth_generation {
            return;
        }
        let Some(encoded) = &old.auth_envelope else {
            return;
        };
        let Ok(envelope) = serde_json::from_str::<keymanager::Envelope>(encoded) else {
            return;
        };
        let name = old.auth_generation.key_name();
        if envelope.format != "keymanager3-aes256-gcm-v1" || envelope.key_name != name {
            return;
        }
        if self
            .pending_cleanup
            .iter()
            .any(|entry| entry.operation == id && entry.digest == digest)
        {
            return;
        }
        // Cleanup is best effort. Keep at most the operation-ledger window; eviction or a helper
        // restart may leave an orphan, but neither authorizes guessing another generation's key.
        if self.pending_cleanup.len() >= state::LEDGER_CAPACITY {
            self.pending_cleanup.remove(0);
        }
        self.pending_cleanup.push(RetiredKey {
            operation: id,
            digest: digest.into(),
            generation: old.auth_generation,
            name,
        });
    }
    fn cleanup_confirmed(&mut self, id: Generation, digest: &str, current: &CanonicalState) {
        let Some(index) = self
            .pending_cleanup
            .iter()
            .position(|entry| entry.operation == id && entry.digest == digest)
        else {
            return;
        };
        let entry = &self.pending_cleanup[index];
        // Treat opaque retained/import envelopes conservatively: a reference prevents cleanup,
        // even when a future envelope format cannot be decoded by this helper.
        if current.auth_generation == entry.generation || references_key(current, &entry.name) {
            return;
        }
        let retired = self.pending_cleanup.remove(index);
        keymanager::remove_retired(&mut self.rpc, &retired.name);
    }
    fn prepare(
        &mut self,
        state: &CanonicalState,
        mutation: WireMutation,
    ) -> Result<Mutation, ErrorCode> {
        Ok(match mutation {
            WireMutation::UpdatePreferences { payload } => Mutation::UpdatePreferences {
                public: serde_json::from_value(payload).map_err(|_| ErrorCode::Invalid)?,
            },
            WireMutation::UpdateConsent { payload } => {
                let c: state::ConsentPayload =
                    serde_json::from_value(payload).map_err(|_| ErrorCode::Invalid)?;
                Mutation::UpdateConsent {
                    consent: c.consent,
                    scopes: c.scopes,
                    ids: c.ids,
                }
            }
            WireMutation::ReplaceAuth {
                public,
                payload,
                protection,
            } => {
                let public = serde_json::from_value(public).map_err(|_| ErrorCode::Invalid)?;
                let auth_generation = Generation::random().map_err(translate)?;
                let auth_envelope = self.protect(
                    state,
                    auth_generation,
                    &payload.0,
                    protection,
                    FallbackReason::FreshLogin,
                )?;
                Mutation::ReplaceAuth {
                    auth_generation,
                    auth_envelope,
                    public,
                }
            }
            WireMutation::ClearTenure {} => Mutation::ClearTenure {
                auth_generation: Generation::random().map_err(translate)?,
            },
            WireMutation::AdvanceMigration { migration } => {
                use state::{MigrationAdvance as A, MigrationDomain as D};
                let (domain, payload) = match migration {
                    MigrationMutation::SessionPending { opaque_envelope } => (
                        D::Session,
                        A::SessionPending {
                            opaque_envelope: opaque_envelope.0,
                        },
                    ),
                    MigrationMutation::SessionComplete {
                        public,
                        auth_plaintext,
                        protection,
                    } => {
                        // Validate public data before generating any persistent key.
                        let public =
                            serde_json::from_value(public).map_err(|_| ErrorCode::Invalid)?;
                        if state.migrations.session.progress == state::MigrationProgress::Complete {
                            return Err(ErrorCode::Invalid);
                        }
                        let generation = Generation::random().map_err(translate)?;
                        let envelope = self.protect(
                            state,
                            generation,
                            &auth_plaintext.0,
                            protection,
                            FallbackReason::LegacyImport,
                        )?;
                        (
                            D::Session,
                            A::SessionComplete {
                                public,
                                auth: state::ProtectedAuth {
                                    auth_generation: generation,
                                    envelope,
                                },
                            },
                        )
                    }
                    MigrationMutation::ConsentComplete { consent } => (
                        D::Consent,
                        A::ConsentComplete {
                            consent: serde_json::from_value(consent)
                                .map_err(|_| ErrorCode::Invalid)?,
                        },
                    ),
                    MigrationMutation::CompleteEmpty { domain } => {
                        let d = match domain {
                            Domain::Session => D::Session,
                            Domain::Consent => D::Consent,
                        };
                        (d, A::CompleteEmpty { domain: d })
                    }
                };
                Mutation::AdvanceMigration { domain, payload }
            }
        })
    }
    fn protect(
        &mut self,
        state: &CanonicalState,
        generation: Generation,
        plaintext: &str,
        protection: ProtectionRequest,
        reason: FallbackReason,
    ) -> Result<String, ErrorCode> {
        if serde_json::to_vec(plaintext)
            .map_err(|_| ErrorCode::Invalid)?
            .len()
            > state::MAX_ENCODED_BYTES
        {
            return Err(ErrorCode::Invalid);
        }
        let mut context = state.auth_context();
        context.auth_generation = generation;
        let context = serde_json::to_value(context).map_err(|_| ErrorCode::Invalid)?;
        match protection {
            ProtectionRequest::KeymanagerRequired => {
                keymanager::seal_detailed(&mut self.rpc, &generation.key_name(), plaintext, context)
                    .map_err(|failure| {
                        self.keymanager_error = Some((failure, AuthPreservation::Unchanged));
                        failure.code
                    })
            }
            ProtectionRequest::KeymanagerWithAclFallback => match keymanager::seal_detailed(
                &mut self.rpc,
                &generation.key_name(),
                plaintext,
                context.clone(),
            ) {
                Ok(envelope) => Ok(envelope),
                Err(failure) => Ok(acl_envelope(
                    context,
                    plaintext,
                    Some((
                        failure,
                        FallbackContext {
                            reason,
                            phase: FallbackPhase::Seal,
                        },
                    )),
                )),
            },
            ProtectionRequest::Db8AclOnlyExplicit => Ok(acl_envelope(context, plaintext, None)),
        }
    }
    fn open_auth(&mut self, state: &CanonicalState) -> Result<Option<String>, ErrorCode> {
        let Some(envelope) = &state.auth_envelope else {
            return Ok(None);
        };
        let context = serde_json::to_value(state.auth_context()).map_err(|_| ErrorCode::Invalid)?;
        let value: Value = serde_json::from_str(envelope).map_err(|_| ErrorCode::Corrupt)?;
        if value["format"] == "db8-acl-only-v1" {
            if value["context"] != context {
                return Err(stage_error("plaintext_mismatch", ErrorCode::Corrupt));
            }
            return value["plaintext"]
                .as_str()
                .ok_or(ErrorCode::Corrupt)
                .and_then(bounded_plaintext)
                .map(Some);
        }
        keymanager::unseal(
            &mut self.rpc,
            &state.auth_generation.key_name(),
            envelope,
            &context,
        )
        .and_then(|plaintext| bounded_plaintext(&plaintext))
        .map(Some)
    }
}
fn references_key(state: &CanonicalState, name: &str) -> bool {
    if state.auth_generation.key_name() == name {
        return true;
    }
    let pending =
        state
            .pending_auth
            .as_ref()
            .and_then(|pending| match &pending.operation.mutation {
                Mutation::ReplaceAuth { auth_envelope, .. } => Some(auth_envelope),
                _ => None,
            });
    [
        state.auth_envelope.as_ref(),
        state.migrations.session.pending_import.as_ref(),
        state.migrations.consent.pending_import.as_ref(),
        pending,
    ]
    .into_iter()
    .flatten()
    .any(|value| value.contains(name))
}
fn fresh_plaintext(mutation: &WireMutation) -> Option<&str> {
    match mutation {
        WireMutation::ReplaceAuth { payload, .. } => Some(&payload.0),
        WireMutation::AdvanceMigration {
            migration: MigrationMutation::SessionComplete { auth_plaintext, .. },
        } => Some(&auth_plaintext.0),
        _ => None,
    }
}
fn plaintext_digest(plaintext: &str) -> String {
    let canonical = serde_json::from_str::<Value>(plaintext)
        .ok()
        .and_then(|value| serde_json::to_vec(&value).ok());
    state::digest_bytes(canonical.as_deref().unwrap_or(plaintext.as_bytes()))
}
fn replay_needs_auth(state: &CanonicalState, id: Generation) -> bool {
    let Some(index) = state.operations.iter().position(|entry| entry.id == id) else {
        return false;
    };
    match &state.operations[index].verification {
        Some(state::ReceiptVerification::Public) => false,
        Some(state::ReceiptVerification::Auth { .. }) => true,
        // Compatibility for old ledgers: equal consecutive generations prove a retained-auth
        // write. An unclassified first entry cannot establish that proof while auth is locked.
        None if index > 0 => {
            state.operations[index - 1].result.auth_generation
                != state.operations[index].result.auth_generation
        }
        None => true,
    }
}
fn acl_envelope(
    context: Value,
    plaintext: &str,
    failure: Option<(KeymanagerFailure, FallbackContext)>,
) -> String {
    let mut envelope = json!({"format":"db8-acl-only-v1","context":context,"plaintext":plaintext});
    if let Some((failure, context)) = failure {
        envelope["fallback"] = json!(failure);
        envelope["fallback_context"] = json!(context);
    }
    envelope.to_string()
}
fn protection_outcome(state: &CanonicalState) -> Option<ProtectionOutcome> {
    let envelope: Value = serde_json::from_str(state.auth_envelope.as_ref()?).ok()?;
    let class = match envelope["format"].as_str()? {
        "keymanager3-aes256-gcm-v1" => ProtectionClass::Keymanager,
        "db8-acl-only-v1" => ProtectionClass::Db8AclOnly,
        _ => return None,
    };
    Some(ProtectionOutcome {
        class,
        fallback: serde_json::from_value(envelope["fallback"].clone()).ok(),
        fallback_context: serde_json::from_value(envelope["fallback_context"].clone()).ok(),
    })
}
fn bounded_plaintext(plaintext: &str) -> Result<String, ErrorCode> {
    if serde_json::to_vec(plaintext)
        .map_err(|_| ErrorCode::Corrupt)?
        .len()
        > state::MAX_ENCODED_BYTES
    {
        return Err(ErrorCode::Corrupt);
    }
    Ok(plaintext.to_owned())
}
fn typed_plaintext_equal(actual: Option<&str>, expected: &str) -> bool {
    let Some(actual) = actual else { return false };
    match (
        serde_json::from_str::<Value>(actual),
        serde_json::from_str::<Value>(expected),
    ) {
        (Ok(actual), Ok(expected)) => actual == expected,
        _ => actual == expected,
    }
}
fn prepared_key(mutation: &Mutation) -> Option<String> {
    let (generation, encoded) = match mutation {
        Mutation::ReplaceAuth {
            auth_generation,
            auth_envelope,
            ..
        } => (auth_generation, auth_envelope),
        Mutation::AdvanceMigration {
            payload: state::MigrationAdvance::SessionComplete { auth, .. },
            ..
        } => (&auth.auth_generation, &auth.envelope),
        _ => return None,
    };
    let envelope: keymanager::Envelope = serde_json::from_str(encoded).ok()?;
    let name = generation.key_name();
    (envelope.format == "keymanager3-aes256-gcm-v1" && envelope.key_name == name).then_some(name)
}
fn committed(
    rev: u64,
    state: &CanonicalState,
    applied: Applied,
    verified: bool,
) -> Result<Response, ErrorCode> {
    Ok(Response::Commit {
        status: CommitStatus::Committed,
        db_rev: Some(rev.to_string()),
        state: Some(serde_json::to_value(state).map_err(|_| ErrorCode::Invalid)?),
        applied: Some(applied),
        verified,
        protection: protection_outcome(state),
    })
}
fn receipt(
    status: CommitStatus,
    rev: Option<u64>,
    state: Option<&CanonicalState>,
) -> Result<Response, ErrorCode> {
    Ok(Response::Commit {
        status,
        db_rev: rev.map(|v| v.to_string()),
        state: state
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| ErrorCode::Invalid)?,
        applied: None,
        verified: false,
        protection: state.and_then(protection_outcome),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    // Reproduce the old device's physical representation without ever interpreting strings.
    fn add_db8_array_ids(value: &mut Value) {
        match value {
            Value::Array(entries) => {
                for entry in entries {
                    if let Some(object) = entry.as_object_mut() {
                        object.entry("_id").or_insert(json!("B3K8aZ"));
                    }
                    add_db8_array_ids(entry);
                }
            }
            Value::Object(fields) => {
                for value in fields.values_mut() {
                    add_db8_array_ids(value);
                }
            }
            _ => {}
        }
    }
    struct Mock {
        replies: VecDeque<Result<Value, ErrorCode>>,
        calls: Vec<(String, Value)>,
    }
    impl Rpc for Mock {
        fn call(&mut self, uri: &str, payload: &Value) -> Result<Value, ErrorCode> {
            self.calls.push((uri.into(), payload.clone()));
            let reply = self.replies.pop_front().expect("unexpected RPC");
            if matches!(&reply, Ok(Value::Null)) && uri == "luna://com.palm.db/get" {
                let mut object = self
                    .calls
                    .iter()
                    .rev()
                    .find(|(uri, _)| uri == "luna://com.palm.db/put")
                    .unwrap()
                    .1["objects"][0]
                    .clone();
                object["_rev"] = json!(object["_rev"].as_u64().unwrap_or(0) + 1);
                add_db8_array_ids(&mut object);
                return Ok(json!({"returnValue":true,"results":[object]}));
            }
            reply
        }
    }
    fn backend(replies: Vec<Result<Value, ErrorCode>>) -> Backend<Mock> {
        Backend::new(
            Mock {
                replies: replies.into(),
                calls: vec![],
            },
            Flavor::Stable,
            "com.beb.plxnative.storage".into(),
        )
    }
    fn record(rev: u64) -> Value {
        let state = CanonicalState::new(Flavor::Stable, Generation([1; 16]));
        let mut value = serde_json::to_value(state).unwrap();
        value["_rev"] = json!(rev);
        value["_kind"] = json!("com.beb.plxnative.storage:1");
        json!({"returnValue":true,"results":[value]})
    }
    fn commit(expected: Expectation) -> Request {
        let mut r = Request::Commit {
            expected,
            operation_id: "02".repeat(16),
            digest: String::new(),
            mutation: WireMutation::ClearTenure {},
        };
        let hash = state::digest_bytes(&request_digest_bytes(&r).unwrap());
        if let Request::Commit { digest, .. } = &mut r {
            *digest = hash;
        }
        r
    }
    fn device_record() -> (Value, CanonicalState) {
        let initial = CanonicalState::new(Flavor::Stable, Generation([1; 16]));
        let operation = Operation::new(
            Generation([3; 16]),
            initial.expected(),
            Mutation::AdvanceMigration {
                domain: state::MigrationDomain::Session,
                payload: state::MigrationAdvance::CompleteEmpty {
                    domain: state::MigrationDomain::Session,
                },
            },
        )
        .unwrap();
        let current = initial.apply(&operation).unwrap().0;
        let mut object = serde_json::to_value(&current).unwrap();
        object["_kind"] = json!("com.beb.plxnative.storage:1");
        object["_rev"] = json!(9);
        object["operations"][0]["_id"] = json!("B3K8aZ");
        (json!({"returnValue":true,"results":[object]}), current)
    }
    fn wrapped_record(state: &CanonicalState, rev: u64) -> Value {
        json!({"returnValue":true,"results":[{
            "_id":state.id,
            "_kind":"com.beb.plxnative.storage:1",
            "_rev":rev,
            "state":String::from_utf8(state.encode().unwrap()).unwrap()
        }]})
    }
    #[test]
    fn old_db8_ledger_ids_load_and_next_commit_rewrites_an_opaque_document() {
        let (record, current) = device_record();
        assert_eq!(
            backend(vec![Ok(record.clone())]).load().unwrap(),
            Some((9, current))
        );
        let mut b = backend(vec![
            Ok(record),
            Ok(json!({"returnValue":true})),
            Ok(Value::Null),
        ]);
        assert!(matches!(
            b.dispatch(changing_request(WireMutation::UpdateConsent {
                payload: json!({"consent":true,"scopes":[{"name":"scope"}],"ids":[{"_id":"application-id"}]})
            })),
            Response::Commit { status: CommitStatus::Committed, verified: true, .. }
        ));
        let written = &b.rpc.calls[1].1["objects"][0];
        assert_eq!(written["_rev"], 9);
        assert_eq!(written.as_object().unwrap().len(), 4);
        let decoded = CanonicalState::decode(
            written["state"].as_str().unwrap().as_bytes(),
            Flavor::Stable,
        )
        .unwrap();
        assert_eq!(decoded.operations.len(), 2);
        assert_eq!(decoded.public.scopes, json!([{"name":"scope"}]));
        assert_eq!(decoded.public.ids, json!([{"_id":"application-id"}]));
    }
    #[test]
    fn opaque_document_roundtrips_arbitrary_nested_ids_and_escaped_strings() {
        let mut current = device_record().1;
        current.public.preferences =
            json!({"list":[{"_id":"application-id","text":"\"\\\n\u{0000}телевизор"}]});
        current.auth_envelope = Some(" future-format: \"\\\n ".into());
        assert_eq!(
            backend(vec![Ok(wrapped_record(&current, u64::MAX))])
                .load()
                .unwrap(),
            Some((u64::MAX, current))
        );
    }
    #[test]
    fn opaque_wrapper_rejects_unknown_fields_collisions_tombstones_and_invalid_state() {
        let current = device_record().1;
        let good = wrapped_record(&current, 9);
        let mut malformed = Vec::new();
        for (key, value) in [
            ("unexpected", json!(true)),
            ("_del", json!(true)),
            ("_id", json!("other-object")),
            ("_kind", json!("other:1")),
            ("_rev", json!("9")),
            ("state", json!(current)),
            ("state", Value::Null),
            ("state", json!("invalid-json")),
        ] {
            let mut record = good.clone();
            record["results"][0][key] = value;
            malformed.push(record);
        }
        for key in ["_id", "_kind", "_rev", "state"] {
            let mut record = good.clone();
            record["results"][0].as_object_mut().unwrap().remove(key);
            malformed.push(record);
        }
        for (pointer, value) in [
            ("/_id", json!("other-object")),
            ("/schema", json!(99)),
            ("/flavor", json!("debug")),
            ("/operations/0/_id", json!("B3K8aZ")),
            ("/unexpected", json!(true)),
        ] {
            let mut state = serde_json::to_value(&current).unwrap();
            // Unknown fields do not exist yet, so assign through their parent object.
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            state.pointer_mut(parent).unwrap()[key] = value;
            let mut record = good.clone();
            record["results"][0]["state"] = json!(state.to_string());
            malformed.push(record);
        }
        for record in malformed {
            assert!(matches!(
                backend(vec![Ok(record)]).load(),
                Err(ErrorCode::Corrupt)
            ));
        }
    }
    #[test]
    fn flattened_recovery_strips_only_ledger_ids_and_keeps_strict_validation() {
        let (mut record, mut expected) = device_record();
        let preferences = json!({"list":[{"_id":"keep-this","_rev":4,"unknown":true}]});
        record["results"][0]["public"]["preferences"] = preferences.clone();
        expected.public.preferences = preferences;
        assert_eq!(
            backend(vec![Ok(record.clone())]).load().unwrap(),
            Some((9, expected))
        );
        // The canonical decoder itself stays strict, including when used by the app.
        let mut raw = record["results"][0].clone();
        raw.as_object_mut().unwrap().remove("_kind");
        raw.as_object_mut().unwrap().remove("_rev");
        assert!(
            CanonicalState::decode(&serde_json::to_vec(&raw).unwrap(), Flavor::Stable).is_err()
        );
        for (pointer, value) in [
            ("/_del", json!(true)),
            ("/unknown", json!(true)),
            ("/operations/0/unknown", json!(true)),
            ("/operations/0/_id", json!(42)),
            ("/operations/0/result/_id", json!("not-an-array-element")),
            ("/operations/0/digest", json!("invalid")),
        ] {
            let mut malformed = record.clone();
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            malformed["results"][0].pointer_mut(parent).unwrap()[key] = value;
            assert!(matches!(
                backend(vec![Ok(malformed)]).load(),
                Err(ErrorCode::Corrupt)
            ));
        }
    }
    #[test]
    fn maximum_canonical_state_fits_escaped_db8_wrapper_and_overflow_is_rejected() {
        let mut current = CanonicalState::new(Flavor::Stable, Generation([1; 16]));
        let padding = state::MAX_ENCODED_BYTES - current.encode().unwrap().len() + 2;
        current.auth_envelope = Some(format!(
            "{}{}",
            "\0".repeat(padding / 6),
            "x".repeat(padding % 6)
        ));
        assert_eq!(current.encode().unwrap().len(), state::MAX_ENCODED_BYTES);
        let record = wrapped_record(&current, u64::MAX);
        assert!(serde_json::to_vec(&record).unwrap().len() < MAX_FRAME);
        assert_eq!(
            backend(vec![Ok(record.clone())]).load().unwrap(),
            Some((u64::MAX, current))
        );
        let mut oversized = record;
        let encoded = oversized["results"][0]["state"]
            .as_str()
            .unwrap()
            .to_owned()
            + " ";
        oversized["results"][0]["state"] = json!(encoded);
        assert!(matches!(
            backend(vec![Ok(oversized)]).load(),
            Err(ErrorCode::Corrupt)
        ));
    }
    #[test]
    fn fixed_id_get_and_exact_u64_revision() {
        let mut b = backend(vec![Ok(record(u64::MAX))]);
        assert_eq!(b.load().unwrap().unwrap().0, u64::MAX);
        assert_eq!(
            b.rpc.calls[0],
            (
                "luna://com.palm.db/get".into(),
                json!({"ids":["plxstate.stable"]})
            )
        );
    }

    #[test]
    fn kind_namespace_and_acl_owner_are_the_registered_service() {
        let mut b = backend(vec![Ok(json!({"returnValue":true}))]);
        b.setup().unwrap();
        assert_eq!(
            b.rpc.calls[0],
            (
                "luna://com.palm.db/putKind".into(),
                json!({
                    "id":"com.beb.plxnative.storage:1",
                    "owner":"com.beb.plxnative.storage",
                    "private":true,
                    "indexes":[]
                })
            )
        );
    }
    #[test]
    fn get_failure_is_not_missing_and_wrong_owner_is_corrupt() {
        let mut b = backend(vec![Ok(json!({"returnValue":false,"errorCode":-3963}))]);
        assert!(b.load().is_err());
        let mut wrong = record(1);
        wrong["results"][0]["_kind"] = json!("other:1");
        assert!(backend(vec![Ok(wrong)]).load().is_err());
    }
    #[test]
    fn stale_revision_never_reaches_put() {
        let mut b = backend(vec![Ok(record(9))]);
        let response = b.dispatch(commit(Expectation::Present {
            db_rev: "8".into(),
            epoch: "0".into(),
            auth_generation: "01".repeat(16),
        }));
        assert!(matches!(
            response,
            Response::Commit {
                status: CommitStatus::Conflict,
                ..
            }
        ));
        assert_eq!(b.rpc.calls.len(), 1);
    }
    #[test]
    fn initial_fixed_id_collision_rereads_without_force_or_overwrite() {
        let mut b = backend(vec![
            Ok(json!({"returnValue":true,"results":[]})),
            Ok(json!({"returnValue":false,"errorCode":-3961})),
            Ok(record(9)),
        ]);
        assert!(matches!(
            b.dispatch(commit(Expectation::Missing {})),
            Response::Commit {
                status: CommitStatus::Conflict,
                ..
            }
        ));
        assert_eq!(b.rpc.calls.len(), 3);
        assert_eq!(b.rpc.calls[1].0, "luna://com.palm.db/put");
        assert_eq!(b.rpc.calls[1].1["objects"][0]["_id"], "plxstate.stable");
        assert!(b.rpc.calls[1].1["objects"][0].get("_rev").is_none());
        assert!(b.rpc.calls[1].1.get("force").is_none());
        assert_eq!(b.rpc.calls[2].0, "luna://com.palm.db/get");
    }
    #[test]
    fn a_valid_update_carries_the_exact_read_revision() {
        let rev = 9_007_199_254_740_993u64;
        let mut b = backend(vec![
            Ok(record(rev)),
            Ok(json!({"returnValue":true,"results":[{"id":"plxstate.stable","rev":rev+1}]})),
            Ok(Value::Null),
        ]);
        let response = b.dispatch(commit(Expectation::Present {
            db_rev: rev.to_string(),
            epoch: "0".into(),
            auth_generation: "01".repeat(16),
        }));
        assert!(matches!(
            response,
            Response::Commit {
                status: CommitStatus::Committed,
                ..
            }
        ));
        assert_eq!(b.rpc.calls[1].1["objects"][0]["_rev"].as_u64(), Some(rev));
    }
    #[test]
    fn digest_is_checked_before_any_storage_or_crypto_call() {
        let mut request = commit(Expectation::Missing {});
        if let Request::Commit { digest, .. } = &mut request {
            *digest = "0".repeat(64);
        }
        let mut b = backend(vec![]);
        assert!(matches!(
            b.dispatch(request),
            Response::Error {
                code: ErrorCode::Invalid
            }
        ));
        assert!(b.rpc.calls.is_empty());
    }

    #[test]
    fn explicit_fresh_auth_installs_new_public_payload_after_clear() {
        let original = CanonicalState::new(Flavor::Stable, Generation([1; 16]));
        let clear = Operation::new(
            Generation([2; 16]),
            original.expected(),
            Mutation::ClearTenure {
                auth_generation: Generation([3; 16]),
            },
        )
        .unwrap();
        let cleared = original.apply(&clear).unwrap().0;
        let public = state::PublicPayload {
            client_id: Some("new-client".into()),
            profile: json!({"id":"new-profile"}),
            consent: json!("must-not-be-restored"),
            ..Default::default()
        };
        let mut b = backend(vec![]);
        let prepared = b
            .prepare(
                &cleared,
                WireMutation::ReplaceAuth {
                    public: serde_json::to_value(public).unwrap(),
                    payload: SecretString("fixture".into()),
                    protection: ProtectionRequest::Db8AclOnlyExplicit,
                },
            )
            .unwrap();
        let operation = Operation::new(Generation([4; 16]), cleared.expected(), prepared).unwrap();
        let next = cleared.apply(&operation).unwrap().0;
        assert_eq!(next.public.client_id.as_deref(), Some("new-client"));
        assert_eq!(next.public.profile, json!({"id":"new-profile"}));
        assert_eq!(next.public.consent, cleared.public.consent);
        assert!(b.rpc.calls.is_empty());
    }

    fn protected_record() -> Value {
        let mut value = record(9);
        value["results"][0]["auth_envelope"] =
            json!(serde_json::to_string(&keymanager::Envelope {
                format: "keymanager3-aes256-gcm-v1".into(),
                key_name: Generation([1; 16]).key_name(),
                iv: "aXY=".into(),
                ciphertext: "Y2lwaGVy".into(),
                context: json!({})
            })
            .unwrap());
        value
    }
    #[test]
    fn confirmed_clear_removes_old_key_after_db8_readback() {
        let mut b = backend(vec![
            Ok(protected_record()),
            Ok(json!({"returnValue":true,"results":[{"id":"plxstate.stable","rev":10}]})),
            Ok(Value::Null),
            Err(ErrorCode::Unavailable),
        ]);
        assert!(matches!(
            b.dispatch(commit(Expectation::Present {
                db_rev: "9".into(),
                epoch: "0".into(),
                auth_generation: "01".repeat(16)
            })),
            Response::Commit {
                status: CommitStatus::Committed,
                ..
            }
        ));
        assert_eq!(
            b.rpc.calls.last().unwrap(),
            &(
                "luna://com.webos.service.keymanager3/removeKey".into(),
                json!({"name":Generation([1;16]).key_name()})
            )
        );
        assert_eq!(b.rpc.calls[1].0, "luna://com.palm.db/put");
    }
    #[test]
    fn engine_conflict_is_a_typed_commit_conflict() {
        let mut request = commit(Expectation::Present {
            db_rev: "9".into(),
            epoch: "0".into(),
            auth_generation: "01".repeat(16),
        });
        if let Request::Commit { mutation, .. } = &mut request {
            *mutation = WireMutation::UpdatePreferences {
                payload: serde_json::to_value(state::PublicPayload::default()).unwrap(),
            };
        }
        let hash = state::digest_bytes(&request_digest_bytes(&request).unwrap());
        if let Request::Commit { digest, .. } = &mut request {
            *digest = hash;
        }
        let mut b = backend(vec![Ok(record(9))]);
        assert!(matches!(
            b.dispatch(request),
            Response::Commit {
                status: CommitStatus::Conflict,
                ..
            }
        ));
        assert_eq!(b.rpc.calls.len(), 1);
    }

    struct CleanupRpc {
        record: Value,
        lose_ack: bool,
        fail_readback: bool,
        persist: bool,
        wrote: bool,
        removed: Vec<String>,
    }
    impl Rpc for CleanupRpc {
        fn call(&mut self, uri: &str, payload: &Value) -> Result<Value, ErrorCode> {
            match uri {
                "luna://com.palm.db/get" => {
                    if self.wrote && self.fail_readback {
                        self.fail_readback = false;
                        return Err(ErrorCode::Unavailable);
                    }
                    Ok(self.record.clone())
                }
                "luna://com.palm.db/put" => {
                    self.wrote = true;
                    if self.persist {
                        let mut object = payload["objects"][0].clone();
                        object["_rev"] =
                            json!(self.record["results"][0]["_rev"].as_u64().unwrap() + 1);
                        add_db8_array_ids(&mut object);
                        self.record = json!({"returnValue":true,"results":[object]});
                    }
                    if self.lose_ack {
                        Err(ErrorCode::Unavailable)
                    } else {
                        Ok(
                            json!({"returnValue":true,"results":[{"id":"plxstate.stable","rev":10}]}),
                        )
                    }
                }
                "luna://com.webos.service.keymanager3/removeKey" => {
                    assert!(self.wrote, "key removed before DB8 write");
                    self.removed.push(payload["name"].as_str().unwrap().into());
                    Ok(json!({"returnValue":true}))
                }
                _ => panic!("unexpected RPC method"),
            }
        }
    }
    fn cleanup_backend() -> Backend<CleanupRpc> {
        Backend::new(
            CleanupRpc {
                record: protected_record(),
                lose_ack: false,
                fail_readback: false,
                persist: true,
                wrote: false,
                removed: vec![],
            },
            Flavor::Stable,
            "com.beb.plxnative.storage".into(),
        )
    }
    fn changing_request(mutation: WireMutation) -> Request {
        let mut request = commit(Expectation::Present {
            db_rev: "9".into(),
            epoch: "0".into(),
            auth_generation: "01".repeat(16),
        });
        if let Request::Commit {
            mutation: target, ..
        } = &mut request
        {
            *target = mutation;
        }
        let hash = state::digest_bytes(&request_digest_bytes(&request).unwrap());
        if let Request::Commit { digest, .. } = &mut request {
            *digest = hash;
        }
        request
    }
    fn fresh_auth() -> WireMutation {
        WireMutation::ReplaceAuth {
            public: serde_json::to_value(state::PublicPayload::default()).unwrap(),
            payload: SecretString("fixture".into()),
            protection: ProtectionRequest::Db8AclOnlyExplicit,
        }
    }
    struct CryptoRpc {
        db: CleanupRpc,
        generated: Vec<String>,
        removed: Vec<String>,
        plaintext: std::collections::HashMap<String, String>,
        active_key: String,
        decrypt: bool,
        reject_put: bool,
        wrong_plaintext: bool,
        changed_readback: bool,
        failure: Option<(&'static str, Result<Value, ErrorCode>)>,
        readback_failure: Option<(&'static str, ErrorCode)>,
        readback_fail_once: bool,
        put_revisions: Vec<u64>,
        repair_failure: Option<RepairFailure>,
    }
    #[derive(Clone, Copy)]
    enum RepairFailure {
        Rejected,
        Uncertain,
        Concurrent,
    }
    impl Rpc for CryptoRpc {
        fn call(&mut self, uri: &str, payload: &Value) -> Result<Value, ErrorCode> {
            if let Some((method, error)) = self.readback_failure {
                if self.db.wrote && uri.ends_with(method) && uri.contains("keymanager3") {
                    if self.readback_fail_once {
                        self.readback_failure = None;
                    }
                    return Err(error);
                }
            }
            if uri == "luna://com.palm.db/put" {
                let expected = payload["objects"][0]["_rev"].as_u64().unwrap();
                self.put_revisions.push(expected);
                assert_eq!(
                    expected,
                    self.db.record["results"][0]["_rev"].as_u64().unwrap(),
                    "every write must carry its observed CAS revision"
                );
                if self.put_revisions.len() == 2 {
                    match self.repair_failure {
                        Some(RepairFailure::Rejected) => {
                            return Ok(json!({"returnValue":false,"errorCode":-3961}))
                        }
                        Some(RepairFailure::Uncertain) => {
                            self.db.lose_ack = true;
                            self.db.fail_readback = true;
                        }
                        Some(RepairFailure::Concurrent) => {
                            self.db.record["results"][0]["_rev"] = json!(expected + 1);
                            return Ok(json!({"returnValue":false,"errorCode":-3961}));
                        }
                        None => {}
                    }
                }
            }
            if let Some((method, result)) = &self.failure {
                if uri.ends_with(method) && uri.contains("keymanager3") {
                    return result.clone();
                }
            }
            match uri {
                "luna://com.webos.service.keymanager3/generateKey" => {
                    self.generated
                        .push(payload["name"].as_str().unwrap().into());
                    Ok(json!({"returnValue":true}))
                }
                "luna://com.webos.service.keymanager3/begin" => {
                    self.active_key = payload["name"].as_str().unwrap().to_owned();
                    self.decrypt = payload["params"]["purpose"][0] == "decrypt";
                    Ok(json!({"returnValue":true,"handle":"123","iv":"aXY="}))
                }
                "luna://com.webos.service.keymanager3/finish" => {
                    if self.decrypt {
                        let mut inner: Value =
                            serde_json::from_str(&self.plaintext[&self.active_key]).unwrap();
                        if self.wrong_plaintext {
                            inner["payload"] = json!("wrong-auth");
                        }
                        Ok(
                            json!({"returnValue":true,"output":keymanager::base64(&serde_json::to_vec(&inner).unwrap())}),
                        )
                    } else {
                        let plaintext = String::from_utf8(
                            keymanager::unbase64(payload["data"].as_str().unwrap()).unwrap(),
                        )
                        .unwrap();
                        self.plaintext.insert(self.active_key.clone(), plaintext);
                        Ok(json!({"returnValue":true,"output":keymanager::base64(&[7; 32])}))
                    }
                }
                "luna://com.webos.service.keymanager3/removeKey" => {
                    self.removed.push(payload["name"].as_str().unwrap().into());
                    Ok(json!({"returnValue":true}))
                }
                "luna://com.webos.service.keymanager3/abort" => Ok(json!({"returnValue":true})),
                "luna://com.palm.db/put" if self.reject_put => {
                    self.db.wrote = true;
                    Ok(json!({"returnValue":false,"errorCode":-3961}))
                }
                "luna://com.palm.db/get" if self.db.wrote && self.changed_readback => {
                    let mut record = self.db.record.clone();
                    let mut state: Value =
                        serde_json::from_str(record["results"][0]["state"].as_str().unwrap())
                            .unwrap();
                    state["public"]["preferences"] = json!({"changed":true});
                    record["results"][0]["state"] = json!(state.to_string());
                    Ok(record)
                }
                _ => self.db.call(uri, payload),
            }
        }
    }
    fn crypto_backend() -> Backend<CryptoRpc> {
        Backend::new(
            CryptoRpc {
                db: cleanup_backend().rpc,
                generated: vec![],
                removed: vec![],
                plaintext: Default::default(),
                active_key: String::new(),
                decrypt: false,
                reject_put: false,
                wrong_plaintext: false,
                changed_readback: false,
                failure: None,
                readback_failure: None,
                readback_fail_once: false,
                put_revisions: vec![],
                repair_failure: None,
            },
            Flavor::Stable,
            "com.beb.plxnative.storage".into(),
        )
    }
    fn protected_auth(public: state::PublicPayload) -> WireMutation {
        WireMutation::ReplaceAuth {
            public: serde_json::to_value(public).unwrap(),
            payload: SecretString("{\"token\":\"fixture\"}".into()),
            protection: ProtectionRequest::KeymanagerRequired,
        }
    }
    fn fallback_auth() -> WireMutation {
        let mut mutation = protected_auth(Default::default());
        if let WireMutation::ReplaceAuth { protection, .. } = &mut mutation {
            *protection = ProtectionRequest::KeymanagerWithAclFallback;
        }
        mutation
    }
    /// A local deadline does not cancel the remote write. The first reread sees the old
    /// object; the next reread sees the late commit, as can happen across LS2 timeout/cancel.
    struct LateDbRpc {
        inner: CryptoRpc,
        pending: Option<Value>,
        read_old_once: bool,
    }
    impl Rpc for LateDbRpc {
        fn call(&mut self, uri: &str, payload: &Value) -> Result<Value, ErrorCode> {
            if uri == "luna://com.palm.db/put" {
                self.pending = Some(payload.clone());
                return Err(ErrorCode::Timeout);
            }
            if uri == "luna://com.palm.db/get" && self.pending.is_some() {
                if self.read_old_once {
                    self.read_old_once = false;
                } else {
                    let pending = self.pending.take().unwrap();
                    self.inner.call("luna://com.palm.db/put", &pending)?;
                }
            }
            self.inner.call(uri, payload)
        }
    }
    #[test]
    fn late_db8_commit_after_timeout_reconciles_same_operation_without_new_key_or_early_cleanup() {
        let inner = crypto_backend();
        let mut b = Backend::new(
            LateDbRpc {
                inner: inner.rpc,
                pending: None,
                read_old_once: true,
            },
            inner.flavor,
            inner.service,
        );
        let request = changing_request(protected_auth(Default::default()));
        let Request::Commit {
            operation_id,
            digest,
            ..
        } = &request
        else {
            panic!()
        };
        let reconcile = Request::Reconcile {
            operation_id: operation_id.clone(),
            digest: digest.clone(),
        };
        assert!(matches!(
            b.dispatch(request.clone()),
            Response::Commit {
                status: CommitStatus::Unavailable,
                ..
            }
        ));
        assert_eq!(b.rpc.inner.generated.len(), 1);
        assert!(b.rpc.inner.removed.is_empty());
        assert!(matches!(
            b.dispatch(reconcile),
            Response::Reconcile {
                status: ReconcileStatus::Applied,
                ..
            }
        ));
        assert!(matches!(
            b.dispatch(request),
            Response::Commit {
                status: CommitStatus::Committed,
                ..
            }
        ));
        assert_eq!(
            b.rpc.inner.generated.len(),
            1,
            "retry must use ledger, never fresh auth mutation"
        );
        assert!(!b.rpc.inner.removed.contains(&b.rpc.inner.generated[0]));
    }
    struct LateGenerateRpc {
        inner: CryptoRpc,
    }
    impl Rpc for LateGenerateRpc {
        fn call(&mut self, uri: &str, payload: &Value) -> Result<Value, ErrorCode> {
            let result = self.inner.call(uri, payload);
            if uri.ends_with("/generateKey") {
                Err(ErrorCode::Timeout)
            } else {
                result
            }
        }
    }
    #[test]
    fn generate_success_with_lost_reply_keeps_uncertain_key_and_commits_acl_once() {
        let inner = crypto_backend();
        let mut b = Backend::new(
            LateGenerateRpc { inner: inner.rpc },
            inner.flavor,
            inner.service,
        );
        let request = changing_request(fallback_auth());
        assert!(matches!(
            b.dispatch(request.clone()),
            Response::Commit {
                verified: true,
                protection: Some(ProtectionOutcome {
                    fallback: Some(KeymanagerFailure {
                        code: ErrorCode::Timeout,
                        ..
                    }),
                    ..
                }),
                ..
            }
        ));
        assert_eq!(b.rpc.inner.generated.len(), 1);
        assert!(
            !b.rpc.inner.removed.contains(&b.rpc.inner.generated[0]),
            "timeout did not prove generate was cancelled"
        );
        assert!(matches!(
            b.dispatch(request),
            Response::Commit {
                status: CommitStatus::Committed,
                ..
            }
        ));
        assert_eq!(b.rpc.inner.generated.len(), 1);
    }
    fn followup_request(b: &mut Backend<CryptoRpc>, id: u8, mutation: WireMutation) -> Request {
        let (rev, state) = b.load().unwrap().unwrap();
        let mut request = Request::Commit {
            expected: Expectation::Present {
                db_rev: rev.to_string(),
                epoch: state.epoch.to_string(),
                auth_generation: state
                    .auth_generation
                    .0
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect(),
            },
            operation_id: format!("{id:02x}").repeat(16),
            digest: String::new(),
            mutation,
        };
        let digest = state::digest_bytes(&request_digest_bytes(&request).unwrap());
        if let Request::Commit { digest: target, .. } = &mut request {
            *target = digest;
        }
        request
    }
    #[test]
    fn fresh_login_falls_back_on_keymanager_failures_with_closed_evidence() {
        for (method, failure, stage, category, service_code) in [
            (
                "/generateKey",
                Err(ErrorCode::Unavailable),
                KeymanagerStage::Generate,
                KeymanagerFailureCategory::Unavailable,
                None,
            ),
            (
                "/generateKey",
                Err(ErrorCode::Timeout),
                KeymanagerStage::Generate,
                KeymanagerFailureCategory::Timeout,
                None,
            ),
            (
                "/generateKey",
                Ok(json!({"returnValue":false,"errorCode":-42,"errorText":"private-fixture"})),
                KeymanagerStage::Generate,
                KeymanagerFailureCategory::ServiceRejected,
                Some(-42),
            ),
            (
                "/begin",
                Ok(json!({"returnValue":false,"errorCode":-43})),
                KeymanagerStage::Begin,
                KeymanagerFailureCategory::ServiceRejected,
                Some(-43),
            ),
            (
                "/finish",
                Ok(json!({"returnValue":false,"errorCode":-44})),
                KeymanagerStage::Finish,
                KeymanagerFailureCategory::ServiceRejected,
                Some(-44),
            ),
            (
                "/begin",
                Err(ErrorCode::Timeout),
                KeymanagerStage::Begin,
                KeymanagerFailureCategory::Timeout,
                None,
            ),
            (
                "/finish",
                Err(ErrorCode::Timeout),
                KeymanagerStage::Finish,
                KeymanagerFailureCategory::Timeout,
                None,
            ),
        ] {
            let mut b = crypto_backend();
            b.rpc.failure = Some((method, failure));
            let request = changing_request(fallback_auth());
            let Request::Commit {
                operation_id,
                digest,
                ..
            } = request.clone()
            else {
                panic!()
            };
            let response = b.dispatch(request);
            let Response::Commit {
                status: CommitStatus::Committed,
                verified: true,
                protection: Some(outcome),
                ..
            } = response
            else {
                panic!("fresh login did not survive {method}");
            };
            assert_eq!(outcome.class, ProtectionClass::Db8AclOnly);
            assert_eq!(
                outcome.fallback_context,
                Some(FallbackContext {
                    reason: FallbackReason::FreshLogin,
                    phase: FallbackPhase::Seal
                })
            );
            let evidence = outcome.fallback.unwrap();
            assert_eq!(evidence.operation, KeymanagerOperation::Seal);
            assert_eq!(evidence.stage, stage);
            assert_eq!(evidence.category, category);
            assert_eq!(evidence.service_code, service_code);
            assert!(!serde_json::to_string(&outcome)
                .unwrap()
                .contains("private-fixture"));
            assert!(
                matches!(b.dispatch(Request::Load {}), Response::Loaded { auth: AuthLoad::Plaintext { .. }, protection: Some(loaded), .. } if loaded == outcome)
            );
            assert!(
                matches!(b.dispatch(Request::Reconcile { operation_id, digest }), Response::Reconcile { status: ReconcileStatus::Applied, protection: Some(reconciled), .. } if reconciled == outcome)
            );
        }
    }
    #[test]
    fn fresh_login_roundtrip_failure_retries_acl_at_observed_revision() {
        let mut b = crypto_backend();
        b.rpc.wrong_plaintext = true;
        assert!(matches!(
            b.dispatch(changing_request(fallback_auth())),
            Response::Commit {
                status: CommitStatus::Committed,
                verified: true,
                protection: Some(ProtectionOutcome {
                    class: ProtectionClass::Db8AclOnly,
                    fallback: Some(KeymanagerFailure {
                        stage: KeymanagerStage::Roundtrip,
                        ..
                    }),
                    ..
                }),
                ..
            }
        ));
        assert!(b.rpc.removed.contains(&b.rpc.generated[0]));
        assert_eq!(b.rpc.put_revisions, [9, 10]);
        let state = b.load().unwrap().unwrap().1;
        assert_eq!(
            state.operations.len(),
            1,
            "repair preserves the original operation receipt"
        );
        assert_eq!(state.revision, 1);
        assert_eq!(
            protection_outcome(&state).unwrap().fallback_context,
            Some(FallbackContext {
                reason: FallbackReason::FreshLogin,
                phase: FallbackPhase::PostwriteReadbackRepair
            })
        );
    }
    #[test]
    fn fresh_login_keeps_successful_keymanager_protection() {
        let mut b = crypto_backend();
        assert!(matches!(
            b.dispatch(changing_request(fallback_auth())),
            Response::Commit {
                status: CommitStatus::Committed,
                verified: true,
                protection: Some(ProtectionOutcome {
                    class: ProtectionClass::Keymanager,
                    fallback: None,
                    ..
                }),
                ..
            }
        ));
    }
    #[test]
    fn readback_timeout_and_unavailable_fall_back_without_regenerating_keys() {
        for (method, code, stage) in [
            ("/begin", ErrorCode::Unavailable, KeymanagerStage::Begin),
            ("/begin", ErrorCode::Timeout, KeymanagerStage::Begin),
            ("/finish", ErrorCode::Timeout, KeymanagerStage::Finish),
        ] {
            let mut b = crypto_backend();
            b.rpc.readback_failure = Some((method, code));
            assert!(matches!(b.dispatch(changing_request(fallback_auth())),
                Response::Commit { status: CommitStatus::Committed, verified: true, protection: Some(ProtectionOutcome { class: ProtectionClass::Db8AclOnly, fallback: Some(failure), .. }), .. }
                if failure.operation == KeymanagerOperation::Readback && failure.stage == stage && failure.code == code));
            assert_eq!(b.rpc.generated.len(), 1);
            assert_eq!(b.rpc.put_revisions, [9, 10]);
        }
    }
    #[test]
    fn unconfirmed_readback_repair_never_claims_durability_or_removes_keys() {
        for failure in [
            RepairFailure::Rejected,
            RepairFailure::Uncertain,
            RepairFailure::Concurrent,
        ] {
            let mut b = crypto_backend();
            b.rpc.wrong_plaintext = true;
            b.rpc.repair_failure = Some(failure);
            assert!(matches!(
                b.dispatch(changing_request(fallback_auth())),
                Response::Commit {
                    status: CommitStatus::Unavailable | CommitStatus::Conflict,
                    verified: false,
                    ..
                }
            ));
            assert!(b.rpc.removed.is_empty());
            assert!(b.rpc.db.removed.is_empty());
        }
    }
    #[test]
    fn rejected_auth_repair_cannot_become_applied_by_reconcile_after_restart() {
        for wrong_plaintext in [false, true] {
            let mut b = crypto_backend();
            b.rpc.wrong_plaintext = wrong_plaintext;
            if !wrong_plaintext {
                b.rpc.readback_failure = Some(("/begin", ErrorCode::Unavailable));
            }
            b.rpc.repair_failure = Some(RepairFailure::Rejected);
            let request = changing_request(fallback_auth());
            let Request::Commit {
                operation_id,
                digest,
                ..
            } = request.clone()
            else {
                panic!()
            };
            assert!(matches!(
                b.dispatch(request),
                Response::Commit {
                    status: CommitStatus::Unavailable,
                    ..
                }
            ));
            let mut restarted = Backend::new(b.rpc, Flavor::Stable, b.service);
            assert!(matches!(
                restarted.dispatch(Request::Load {}),
                Response::Loaded {
                    auth: AuthLoad::Locked { .. },
                    ..
                }
            ));
            assert!(matches!(
                restarted.dispatch(Request::Reconcile {
                    operation_id,
                    digest
                }),
                Response::Reconcile {
                    status: ReconcileStatus::Unknown,
                    applied: None,
                    ..
                }
            ));
            assert!(restarted.rpc.removed.is_empty());
        }
    }
    #[test]
    fn rejected_auth_repair_cannot_become_committed_by_duplicate_replay() {
        for wrong_plaintext in [false, true] {
            let mut b = crypto_backend();
            b.rpc.wrong_plaintext = wrong_plaintext;
            if !wrong_plaintext {
                b.rpc.readback_failure = Some(("/begin", ErrorCode::Unavailable));
            }
            b.rpc.repair_failure = Some(RepairFailure::Rejected);
            let request = changing_request(fallback_auth());
            assert!(matches!(
                b.dispatch(request.clone()),
                Response::Commit {
                    status: CommitStatus::Unavailable,
                    ..
                }
            ));
            let mut restarted = Backend::new(b.rpc, Flavor::Stable, b.service);
            assert!(matches!(
                restarted.dispatch(request),
                Response::Commit {
                    status: CommitStatus::Unavailable,
                    applied: None,
                    verified: false,
                    ..
                }
            ));
            assert!(restarted.rpc.removed.is_empty());
        }
    }
    #[test]
    fn auth_receipts_recover_only_after_plaintext_is_readable_or_acl_repair_is_found() {
        for repair in [RepairFailure::Rejected, RepairFailure::Uncertain] {
            let mut b = crypto_backend();
            b.rpc.wrong_plaintext = true;
            b.rpc.repair_failure = Some(repair);
            let request = changing_request(fallback_auth());
            let Request::Commit {
                operation_id,
                digest,
                ..
            } = request.clone()
            else {
                panic!()
            };
            assert!(matches!(
                b.dispatch(request.clone()),
                Response::Commit {
                    status: CommitStatus::Unavailable,
                    ..
                }
            ));
            // A rejected repair recovers through a working Keymanager; a lost repair reply
            // recovers through the actual ACL document. Both require fresh readback proof.
            b.rpc.wrong_plaintext = false;
            let mut restarted = Backend::new(b.rpc, Flavor::Stable, b.service);
            assert!(matches!(
                restarted.dispatch(Request::Reconcile {
                    operation_id,
                    digest
                }),
                Response::Reconcile {
                    status: ReconcileStatus::Applied,
                    applied: Some(_),
                    ..
                }
            ));
            assert!(matches!(
                restarted.dispatch(request),
                Response::Commit {
                    status: CommitStatus::Committed,
                    verified: false,
                    ..
                }
            ));
        }
    }
    #[test]
    fn public_receipts_reconcile_and_replay_while_retained_auth_is_locked() {
        let mut b = crypto_backend();
        assert!(matches!(
            b.dispatch(changing_request(fallback_auth())),
            Response::Commit {
                status: CommitStatus::Committed,
                ..
            }
        ));
        let request = followup_request(
            &mut b,
            3,
            WireMutation::UpdatePreferences {
                payload: serde_json::to_value(state::PublicPayload::default()).unwrap(),
            },
        );
        let Request::Commit {
            operation_id,
            digest,
            ..
        } = request.clone()
        else {
            panic!()
        };
        b.rpc.readback_failure = Some(("/begin", ErrorCode::Unavailable));
        assert!(matches!(
            b.dispatch(request.clone()),
            Response::Commit {
                status: CommitStatus::Committed,
                verified: true,
                ..
            }
        ));
        let mut restarted = Backend::new(b.rpc, Flavor::Stable, b.service);
        assert!(matches!(
            restarted.dispatch(Request::Reconcile {
                operation_id: operation_id.clone(),
                digest: digest.clone()
            }),
            Response::Reconcile {
                status: ReconcileStatus::Applied,
                ..
            }
        ));
        assert!(matches!(
            restarted.dispatch(request.clone()),
            Response::Commit {
                status: CommitStatus::Committed,
                verified: false,
                ..
            }
        ));
        // Old schemas carry no marker. Equal adjacent generations still prove retained-auth.
        let mut state: Value = serde_json::from_str(
            restarted.rpc.db.record["results"][0]["state"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        for entry in state["operations"].as_array_mut().unwrap() {
            entry.as_object_mut().unwrap().remove("verification");
        }
        restarted.rpc.db.record["results"][0]["state"] = json!(state.to_string());
        assert!(matches!(
            restarted.dispatch(Request::Reconcile {
                operation_id,
                digest
            }),
            Response::Reconcile {
                status: ReconcileStatus::Applied,
                ..
            }
        ));
        assert!(matches!(
            restarted.dispatch(request),
            Response::Commit {
                status: CommitStatus::Committed,
                verified: false,
                ..
            }
        ));
    }
    #[test]
    fn historical_auth_receipts_require_current_auth_readability_after_replacement() {
        let mut b = crypto_backend();
        let original = changing_request(fallback_auth());
        let Request::Commit {
            operation_id,
            digest,
            ..
        } = original.clone()
        else {
            panic!()
        };
        assert!(matches!(
            b.dispatch(original.clone()),
            Response::Commit {
                status: CommitStatus::Committed,
                ..
            }
        ));
        let replacement = followup_request(&mut b, 3, fallback_auth());
        assert!(matches!(
            b.dispatch(replacement),
            Response::Commit {
                status: CommitStatus::Committed,
                ..
            }
        ));
        b.rpc.readback_failure = Some(("/begin", ErrorCode::Unavailable));
        let mut restarted = Backend::new(b.rpc, Flavor::Stable, b.service);
        assert!(matches!(
            restarted.dispatch(Request::Reconcile {
                operation_id,
                digest
            }),
            Response::Reconcile {
                status: ReconcileStatus::Unknown,
                applied: None,
                ..
            }
        ));
        assert!(matches!(
            restarted.dispatch(original),
            Response::Commit {
                status: CommitStatus::Unavailable,
                applied: None,
                ..
            }
        ));
    }
    #[test]
    fn legacy_auth_receipt_with_locked_auth_is_conservative_after_restart() {
        let mut b = crypto_backend();
        let request = changing_request(fallback_auth());
        let Request::Commit {
            operation_id,
            digest,
            ..
        } = request.clone()
        else {
            panic!()
        };
        assert!(matches!(
            b.dispatch(request.clone()),
            Response::Commit {
                status: CommitStatus::Committed,
                ..
            }
        ));
        let mut state: Value =
            serde_json::from_str(b.rpc.db.record["results"][0]["state"].as_str().unwrap()).unwrap();
        state["operations"][0]
            .as_object_mut()
            .unwrap()
            .remove("verification");
        b.rpc.db.record["results"][0]["state"] = json!(state.to_string());
        b.rpc.readback_failure = Some(("/begin", ErrorCode::Unavailable));
        let mut restarted = Backend::new(b.rpc, Flavor::Stable, b.service);
        assert!(matches!(
            restarted.dispatch(Request::Reconcile {
                operation_id,
                digest
            }),
            Response::Reconcile {
                status: ReconcileStatus::Unknown,
                applied: None,
                ..
            }
        ));
        assert!(matches!(
            restarted.dispatch(request),
            Response::Commit {
                status: CommitStatus::Unavailable,
                applied: None,
                ..
            }
        ));
    }
    #[test]
    fn strict_refresh_failure_preserves_existing_ciphertext() {
        let mut b = crypto_backend();
        assert!(matches!(
            b.dispatch(changing_request(protected_auth(Default::default()))),
            Response::Commit { verified: true, .. }
        ));
        let request = followup_request(&mut b, 0x76, protected_auth(Default::default()));
        b.rpc.db.wrote = false;
        b.rpc.removed.clear();
        let original = b.rpc.db.record.clone();
        b.rpc.failure = Some(("/generateKey", Err(ErrorCode::Unavailable)));
        let response = serde_json::to_value(b.dispatch(request)).unwrap();
        assert_eq!(response["type"], "keymanager_error");
        assert_eq!(response["failure"]["category"], "unavailable");
        assert_eq!(response["failure"]["stage"], "generate");
        assert!(!b.rpc.db.wrote);
        assert_eq!(b.rpc.db.record, original);
        assert!(b.rpc.removed.is_empty());
    }
    #[test]
    fn strict_failures_keep_exact_closed_cause_and_existing_ciphertext() {
        for (method, result, category, code) in [
            (
                "/generateKey",
                Err(ErrorCode::Timeout),
                KeymanagerFailureCategory::Timeout,
                ErrorCode::Timeout,
            ),
            (
                "/generateKey",
                Ok(json!({"returnValue":false,"errorCode":-42,"errorText":"never-send-this"})),
                KeymanagerFailureCategory::ServiceRejected,
                ErrorCode::Capability,
            ),
            (
                "/begin",
                Ok(json!({"returnValue":true,"handle":false})),
                KeymanagerFailureCategory::InvalidResponse,
                ErrorCode::Corrupt,
            ),
        ] {
            let mut b = crypto_backend();
            assert!(matches!(
                b.dispatch(changing_request(protected_auth(Default::default()))),
                Response::Commit { verified: true, .. }
            ));
            let request = followup_request(&mut b, 0x77, protected_auth(Default::default()));
            let old = b.load().unwrap().unwrap().1;
            b.rpc.failure = Some((method, result));
            let response = b.dispatch(request);
            assert!(
                matches!(response, Response::KeymanagerError { failure, preservation: AuthPreservation::Unchanged, db8_commit_verified: false } if failure.category == category && failure.code == code)
            );
            assert_eq!(b.load().unwrap().unwrap().1, old);
            assert!(!serde_json::to_string(&response)
                .unwrap()
                .contains("never-send-this"));
        }
    }
    #[test]
    fn strict_pending_auth_failure_keeps_active_state_unchanged() {
        let mut b = crypto_backend();
        assert!(matches!(
            b.dispatch(changing_request(protected_auth(Default::default()))),
            Response::Commit { verified: true, .. }
        ));
        let old = b.load().unwrap().unwrap().1;
        let request = followup_request(&mut b, 0x77, protected_auth(Default::default()));
        b.rpc.db.wrote = false;
        b.rpc.readback_failure = Some(("/begin", ErrorCode::Timeout));
        let response = serde_json::to_value(b.dispatch(request)).unwrap();
        let mut staged = b.load().unwrap().unwrap().1;
        assert!(staged.pending_auth.take().is_some());
        assert_eq!(staged, old, "candidate never replaced active state");
        assert_eq!(response["type"], "keymanager_error");
        assert_eq!(response["preservation"], "unchanged");
        assert_eq!(response["failure"]["code"], "timeout");
        assert_eq!(response["db8_commit_verified"], true);
        b.rpc.readback_failure = None;
        let mut restarted = Backend::new(b.rpc, Flavor::Stable, b.service);
        assert!(matches!(
            restarted.dispatch(Request::Load {}),
            Response::Loaded {
                auth: AuthLoad::Plaintext { .. },
                ..
            }
        ));
    }
    #[test]
    fn strict_transient_pending_failure_keeps_old_auth_and_duplicate_resumes_without_new_key() {
        let mut b = crypto_backend();
        assert!(matches!(
            b.dispatch(changing_request(protected_auth(Default::default()))),
            Response::Commit { verified: true, .. }
        ));
        let old = b.load().unwrap().unwrap().1;
        let request = followup_request(&mut b, 0x78, protected_auth(Default::default()));
        b.rpc.db.wrote = false;
        b.rpc.removed.clear();
        b.rpc.put_revisions.clear();
        b.rpc.readback_failure = Some(("/begin", ErrorCode::Timeout));
        b.rpc.readback_fail_once = true;
        assert!(matches!(
            b.dispatch(request.clone()),
            Response::KeymanagerError {
                failure: KeymanagerFailure {
                    operation: KeymanagerOperation::Readback,
                    code: ErrorCode::Timeout,
                    ..
                },
                preservation: AuthPreservation::Unchanged,
                db8_commit_verified: true,
            }
        ));
        let mut active = b.load().unwrap().unwrap().1;
        assert!(active.pending_auth.take().is_some());
        assert_eq!(active, old);
        assert_eq!(b.rpc.put_revisions.len(), 1);
        assert!(b.rpc.removed.is_empty());
        let mut restarted = Backend::new(b.rpc, Flavor::Stable, b.service);
        assert!(matches!(
            restarted.dispatch(Request::Load {}),
            Response::Loaded {
                auth: AuthLoad::Plaintext { .. },
                protection: Some(ProtectionOutcome {
                    class: ProtectionClass::Keymanager,
                    ..
                }),
                ..
            }
        ));
        assert!(matches!(
            restarted.dispatch(request),
            Response::Commit { verified: true, .. }
        ));
        assert_eq!(
            restarted.rpc.generated.len(),
            2,
            "one seed and one staged refresh; restart never regenerates candidate"
        );
        assert!(restarted.load().unwrap().unwrap().1.pending_auth.is_none());
    }
    struct ExitAfterPut {
        inner: CryptoRpc,
        armed: bool,
    }
    impl Rpc for ExitAfterPut {
        fn call(&mut self, uri: &str, payload: &Value) -> Result<Value, ErrorCode> {
            let result = self.inner.call(uri, payload);
            if self.armed && uri == "luna://com.palm.db/put" {
                self.armed = false;
                panic!("simulated helper exit after first persisted write");
            }
            result
        }
    }
    #[test]
    fn helper_exit_after_first_strict_put_keeps_old_auth_active_on_restart() {
        let mut b = crypto_backend();
        assert!(matches!(
            b.dispatch(changing_request(protected_auth(Default::default()))),
            Response::Commit { verified: true, .. }
        ));
        let old = b.load().unwrap().unwrap().1;
        let request = followup_request(&mut b, 0x7a, protected_auth(Default::default()));
        let mut crashing = Backend::new(
            ExitAfterPut {
                inner: b.rpc,
                armed: true,
            },
            b.flavor,
            b.service,
        );
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || crashing.dispatch(request)
        ))
        .is_err());
        let mut restarted = Backend::new(crashing.rpc.inner, crashing.flavor, crashing.service);
        let actual = restarted.load().unwrap().unwrap().1;
        assert_eq!(
            actual.auth_generation, old.auth_generation,
            "first write must not publish unverified auth"
        );
        assert_eq!(actual.auth_envelope, old.auth_envelope);
        assert!(matches!(
            restarted.dispatch(Request::Load {}),
            Response::Loaded {
                auth: AuthLoad::Plaintext { .. },
                ..
            }
        ));
    }
    #[derive(Clone, Copy, Debug)]
    enum BoundaryFault {
        BeforePut,
        AfterPut,
        LostReply,
        LostReplyAndReadback,
    }
    struct BoundaryRpc {
        inner: CryptoRpc,
        phase: usize,
        puts: usize,
        fault: BoundaryFault,
        fired: bool,
    }
    impl Rpc for BoundaryRpc {
        fn call(&mut self, uri: &str, payload: &Value) -> Result<Value, ErrorCode> {
            if uri == "luna://com.palm.db/put" {
                self.puts += 1;
                if self.puts == self.phase && !self.fired {
                    self.fired = true;
                    if matches!(self.fault, BoundaryFault::BeforePut) {
                        panic!("simulated exit before CAS");
                    }
                    let result = self.inner.call(uri, payload);
                    return match self.fault {
                        BoundaryFault::AfterPut => panic!("simulated exit after CAS"),
                        BoundaryFault::LostReply => Err(ErrorCode::Timeout),
                        BoundaryFault::LostReplyAndReadback => {
                            self.inner.db.fail_readback = true;
                            Err(ErrorCode::Timeout)
                        }
                        BoundaryFault::BeforePut => result,
                    };
                }
            }
            self.inner.call(uri, payload)
        }
    }
    #[test]
    fn strict_two_phase_restart_matrix_never_activates_unverified_auth_or_invents_applied() {
        for phase in [1, 2] {
            for fault in [
                BoundaryFault::BeforePut,
                BoundaryFault::AfterPut,
                BoundaryFault::LostReply,
                BoundaryFault::LostReplyAndReadback,
            ] {
                let mut b = crypto_backend();
                assert!(matches!(
                    b.dispatch(changing_request(protected_auth(Default::default()))),
                    Response::Commit { verified: true, .. }
                ));
                let old = b.load().unwrap().unwrap().1;
                let mut mutation = protected_auth(Default::default());
                if let WireMutation::ReplaceAuth { payload, .. } = &mut mutation {
                    payload.0 = "{\"token\":\"next-fixture\"}".into();
                }
                let request = followup_request(&mut b, 0x7b, mutation);
                let Request::Commit {
                    operation_id,
                    digest,
                    ..
                } = &request
                else {
                    panic!()
                };
                let reconcile = Request::Reconcile {
                    operation_id: operation_id.clone(),
                    digest: digest.clone(),
                };
                let operation = generation(operation_id).unwrap();
                b.rpc.removed.clear();
                let mut interrupted = Backend::new(
                    BoundaryRpc {
                        inner: b.rpc,
                        phase,
                        puts: 0,
                        fault,
                        fired: false,
                    },
                    b.flavor,
                    b.service,
                );
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    interrupted.dispatch(request.clone())
                }));
                let mut restarted =
                    Backend::new(interrupted.rpc, interrupted.flavor, interrupted.service);
                let active = restarted.load().unwrap().unwrap().1;
                let before_promotion = matches!(fault, BoundaryFault::BeforePut)
                    || (phase == 1
                        && matches!(
                            fault,
                            BoundaryFault::AfterPut | BoundaryFault::LostReplyAndReadback
                        ));
                if before_promotion {
                    assert_eq!(active.auth_envelope, old.auth_envelope, "{phase} {fault:?}");
                    assert_eq!(active.auth_generation, old.auth_generation);
                    assert_eq!(
                        active.operation_status(operation, digest).unwrap(),
                        OperationStatus::Unknown
                    );
                    assert!(restarted.rpc.inner.removed.is_empty());
                } else {
                    assert_ne!(active.auth_generation, old.auth_generation);
                    assert!(matches!(
                        active.operation_status(operation, digest).unwrap(),
                        OperationStatus::Applied(_)
                    ));
                    assert!(active.pending_auth.is_none());
                }
                assert!(matches!(
                    restarted.dispatch(Request::Load {}),
                    Response::Loaded {
                        auth: AuthLoad::Plaintext { .. },
                        ..
                    }
                ));
                let reconciled = restarted.dispatch(reconcile);
                if phase == 1 && matches!(fault, BoundaryFault::BeforePut) {
                    assert!(matches!(
                        reconciled,
                        Response::Reconcile {
                            status: ReconcileStatus::Unknown,
                            applied: None,
                            ..
                        }
                    ));
                } else {
                    assert!(
                        matches!(
                            reconciled,
                            Response::Reconcile {
                                status: ReconcileStatus::Applied,
                                ..
                            }
                        ),
                        "{phase} {fault:?}"
                    );
                }
                assert!(matches!(
                    restarted.dispatch(request),
                    Response::Commit {
                        status: CommitStatus::Committed,
                        ..
                    }
                ));
                let final_state = restarted.load().unwrap().unwrap().1;
                assert!(final_state.pending_auth.is_none());
                assert_eq!(final_state.operations.len(), old.operations.len() + 1);
                assert_eq!(
                    restarted.rpc.inner.generated.len(),
                    if phase == 1 && matches!(fault, BoundaryFault::BeforePut) {
                        3
                    } else {
                        2
                    }
                );
                assert!(!restarted
                    .rpc
                    .inner
                    .removed
                    .contains(&final_state.auth_generation.key_name()));
            }
        }
    }

    #[test]
    fn public_decision_and_clear_cancel_pending_atomically_without_promoting_it() {
        for decision in 0..3 {
            let clear = decision == 2;
            let mut b = crypto_backend();
            assert!(matches!(
                b.dispatch(changing_request(protected_auth(Default::default()))),
                Response::Commit { verified: true, .. }
            ));
            let old = b.load().unwrap().unwrap().1;
            let request = followup_request(&mut b, 0x7c, protected_auth(Default::default()));
            let mut interrupted = Backend::new(
                ExitAfterPut {
                    inner: b.rpc,
                    armed: true,
                },
                b.flavor,
                b.service,
            );
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || interrupted.dispatch(request)
            ))
            .is_err());
            let mut restarted = Backend::new(
                interrupted.rpc.inner,
                interrupted.flavor,
                interrupted.service,
            );
            let staged = restarted.load().unwrap().unwrap().1;
            let pending = staged.pending_auth.as_ref().unwrap();
            let cancelled_name = prepared_key(&pending.operation.mutation).unwrap();
            let pending_reconcile = Request::Reconcile {
                operation_id: pending
                    .operation
                    .id
                    .0
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
                digest: pending.operation.digest.clone(),
            };
            let mutation = if clear {
                WireMutation::ClearTenure {}
            } else if decision == 1 {
                WireMutation::UpdateConsent {
                    payload: json!({"consent":false,"scopes":[],"ids":{}}),
                }
            } else {
                WireMutation::UpdatePreferences {
                    payload: serde_json::to_value(state::PublicPayload {
                        preferences: json!({"chosen":true}),
                        ..old.public.clone()
                    })
                    .unwrap(),
                }
            };
            let change = followup_request(&mut restarted, 0x7d, mutation);
            assert!(matches!(
                restarted.dispatch(change),
                Response::Commit {
                    status: CommitStatus::Committed,
                    verified: true,
                    ..
                }
            ));
            let current = restarted.load().unwrap().unwrap().1;
            assert!(current.pending_auth.is_none());
            assert!(restarted.rpc.removed.contains(&cancelled_name));
            if clear {
                assert_eq!(current.status, state::Status::Cleared);
                assert!(current.auth_envelope.is_none());
            } else {
                assert_eq!(current.auth_envelope, old.auth_envelope);
                if decision == 1 {
                    assert_eq!(current.public.consent, json!(false));
                } else {
                    assert_eq!(current.public.preferences, json!({"chosen":true}));
                }
            }
            assert!(matches!(
                restarted.dispatch(pending_reconcile),
                Response::Reconcile {
                    status: ReconcileStatus::Unknown,
                    applied: None,
                    ..
                }
            ));
        }
    }
    #[test]
    fn strict_unconfirmed_promotion_retains_all_possibly_referenced_keys() {
        for failure in [
            RepairFailure::Rejected,
            RepairFailure::Uncertain,
            RepairFailure::Concurrent,
        ] {
            let mut b = crypto_backend();
            assert!(matches!(
                b.dispatch(changing_request(protected_auth(Default::default()))),
                Response::Commit { verified: true, .. }
            ));
            let request = followup_request(&mut b, 0x79, protected_auth(Default::default()));
            b.rpc.db.wrote = false;
            b.rpc.removed.clear();
            b.rpc.put_revisions.clear();
            b.rpc.repair_failure = Some(failure);
            assert!(matches!(
                b.dispatch(request),
                Response::Commit {
                    status: CommitStatus::Unavailable,
                    verified: false,
                    ..
                }
            ));
            assert!(
                b.rpc.removed.is_empty(),
                "uncertain promotion cannot retire either key"
            );
        }
    }
    #[test]
    fn complete_session_import_can_fall_back_and_authenticates_acl_readback() {
        let mut b = crypto_backend();
        b.rpc.failure = Some(("/generateKey", Err(ErrorCode::Unavailable)));
        let response = b.dispatch(changing_request(WireMutation::AdvanceMigration {
            migration: MigrationMutation::SessionComplete {
                public: serde_json::to_value(state::PublicPayload::default()).unwrap(),
                auth_plaintext: SecretString("import-fixture".into()),
                protection: ProtectionRequest::KeymanagerWithAclFallback,
            },
        }));
        assert!(matches!(
            response,
            Response::Commit {
                status: CommitStatus::Committed,
                verified: true,
                ..
            }
        ));
        let current = b.load().unwrap().unwrap().1;
        assert_eq!(
            current.migrations.session.progress,
            state::MigrationProgress::Complete
        );
        assert_eq!(
            protection_outcome(&current).unwrap().fallback_context,
            Some(FallbackContext {
                reason: FallbackReason::LegacyImport,
                phase: FallbackPhase::Seal
            })
        );
        assert_eq!(
            b.open_auth(&current).unwrap().as_deref(),
            Some("import-fixture")
        );
    }
    #[test]
    fn invalid_migration_public_data_does_not_generate_a_key() {
        let mut b = crypto_backend();
        assert!(matches!(
            b.dispatch(changing_request(WireMutation::AdvanceMigration {
                migration: MigrationMutation::SessionComplete {
                    public: json!({"unknown":"invalid"}),
                    auth_plaintext: SecretString("fixture".into()),
                    protection: ProtectionRequest::KeymanagerRequired,
                }
            })),
            Response::Error {
                code: ErrorCode::Invalid
            }
        ));
        assert!(b.rpc.generated.is_empty());
    }
    #[test]
    fn candidate_validation_failure_removes_only_the_unpersisted_key() {
        let mut b = crypto_backend();
        assert!(matches!(
            b.dispatch(changing_request(protected_auth(state::PublicPayload {
                preferences: json!("x".repeat(state::MAX_LOGICAL_BYTES)),
                ..Default::default()
            }))),
            Response::Error {
                code: ErrorCode::Invalid
            }
        ));
        assert_eq!(b.rpc.generated.len(), 1);
        assert_eq!(b.rpc.removed, b.rpc.generated);
        assert!(!b.rpc.db.wrote);
    }
    #[test]
    fn definite_rejection_removes_new_key_but_uncertain_outcome_retains_it() {
        let mut rejected = crypto_backend();
        rejected.rpc.reject_put = true;
        assert!(!matches!(
            rejected.dispatch(changing_request(protected_auth(Default::default()))),
            Response::Commit {
                status: CommitStatus::Committed,
                ..
            }
        ));
        assert_eq!(rejected.rpc.generated.len(), 1);
        assert_eq!(rejected.rpc.removed, rejected.rpc.generated);
        assert!(rejected.pending_cleanup.is_empty());

        let mut uncertain = crypto_backend();
        uncertain.rpc.db.lose_ack = true;
        uncertain.rpc.db.fail_readback = true;
        assert!(matches!(
            uncertain.dispatch(changing_request(protected_auth(Default::default()))),
            Response::Commit {
                status: CommitStatus::Unavailable,
                verified: false,
                ..
            }
        ));
        assert_eq!(uncertain.rpc.generated.len(), 1);
        assert!(uncertain.rpc.removed.is_empty());
    }
    #[test]
    fn migration_cleanup_proof_requires_state_and_authenticated_plaintext_readback() {
        let mut b = crypto_backend();
        assert!(matches!(
            b.dispatch(changing_request(protected_auth(Default::default()))),
            Response::Commit {
                status: CommitStatus::Committed,
                verified: true,
                applied: Some(_),
                ..
            }
        ));
        assert!(
            b.rpc.decrypt,
            "must reopen persisted auth before verification"
        );

        let mut mismatch = crypto_backend();
        mismatch.rpc.wrong_plaintext = true;
        assert!(matches!(
            mismatch.dispatch(changing_request(protected_auth(Default::default()))),
            Response::KeymanagerError {
                failure: KeymanagerFailure {
                    code: ErrorCode::Corrupt,
                    ..
                },
                preservation: AuthPreservation::Unchanged,
                db8_commit_verified: true,
            }
        ));
        assert!(
            mismatch.rpc.removed.is_empty(),
            "persisted new key and retired old key are retained on failed verification"
        );

        let mut changed = crypto_backend();
        changed.rpc.changed_readback = true;
        assert!(matches!(
            changed.dispatch(changing_request(protected_auth(Default::default()))),
            Response::Commit {
                status: CommitStatus::Unavailable,
                verified: false,
                applied: None,
                ..
            }
        ));

        let mut unreadable = crypto_backend();
        unreadable.rpc.db.fail_readback = true;
        assert!(matches!(
            unreadable.dispatch(changing_request(protected_auth(Default::default()))),
            Response::Commit {
                status: CommitStatus::Unavailable,
                verified: false,
                ..
            }
        ));
        assert!(unreadable.rpc.removed.is_empty());
    }
    #[test]
    fn readback_compares_typed_auth_json_instead_of_property_order() {
        assert!(typed_plaintext_equal(
            Some("{\"b\":2,\"a\":1}"),
            "{\"a\":1,\"b\":2}"
        ));
        assert!(!typed_plaintext_equal(Some("{\"a\":2}"), "{\"a\":1}"));
    }
    #[test]
    fn locked_public_settings_preserve_auth_exactly_without_keymanager_calls() {
        let mut b = backend(vec![]);
        let initial = CanonicalState::new(Flavor::Stable, Generation([1; 16]));
        let envelope = format!(
            " {} \n",
            serde_json::to_string(&keymanager::Envelope {
                format: "keymanager3-aes256-gcm-v1".into(),
                key_name: Generation([1; 16]).key_name(),
                iv: "aXY=".into(),
                ciphertext: keymanager::base64(&[7; 32]),
                context: serde_json::to_value(initial.auth_context()).unwrap(),
            })
            .unwrap()
        );
        let mut stored = record(9);
        stored["results"][0]["auth_envelope"] = json!(envelope);
        stored["results"][0]["migrations"]["session"]["progress"] = json!("Complete");
        b.rpc.replies = vec![
            Ok(stored),
            Ok(json!({"returnValue":true,"results":[{"id":"plxstate.stable","rev":10}]})),
            Ok(Value::Null),
        ]
        .into();
        let response = b.dispatch(changing_request(WireMutation::UpdatePreferences {
            payload: serde_json::to_value(state::PublicPayload {
                preferences: json!({"volume":42}),
                ..Default::default()
            })
            .unwrap(),
        }));
        let Response::Commit {
            status: CommitStatus::Committed,
            state: Some(actual),
            ..
        } = response
        else {
            panic!()
        };
        assert_eq!(actual["auth_envelope"], json!(envelope));
        assert_eq!(actual["public"]["preferences"], json!({"volume":42}));
        assert!(b
            .rpc
            .calls
            .iter()
            .all(|(uri, _)| uri.starts_with("luna://com.palm.db/")));
    }
    #[test]
    fn replay_and_reconcile_report_original_applied_after_intervening_operation() {
        let initial = CanonicalState::new(Flavor::Stable, Generation([1; 16]));
        let request = commit(Expectation::Missing {});
        let Request::Commit {
            operation_id,
            digest,
            ..
        } = &request
        else {
            panic!()
        };
        let operation = Operation {
            id: generation(operation_id).unwrap(),
            digest: digest.clone(),
            expected: initial.expected(),
            mutation: Mutation::ClearTenure {
                auth_generation: Generation([3; 16]),
            },
        };
        let (current, original) = initial.apply_verified_digest(&operation).unwrap();
        let later = Operation::new(
            Generation([4; 16]),
            current.expected(),
            Mutation::ReplaceAuth {
                auth_generation: Generation([5; 16]),
                auth_envelope: "opaque-later-auth".into(),
                public: state::PublicPayload::default(),
            },
        )
        .unwrap();
        let current = current.apply(&later).unwrap().0;
        let wrapped = wrapped_record(&current, 20);
        let mut object = serde_json::to_value(current).unwrap();
        object["_rev"] = json!(20);
        object["_kind"] = json!("com.beb.plxnative.storage:1");
        add_db8_array_ids(&mut object);
        let record = json!({"returnValue":true,"results":[object]});
        let reconcile = Request::Reconcile {
            operation_id: operation_id.clone(),
            digest: digest.clone(),
        };
        for record in [record, wrapped] {
            let mut b = backend(vec![Ok(record.clone()), Ok(record)]);
            for response in [b.dispatch(request.clone()), b.dispatch(reconcile.clone())] {
                assert_eq!(
                    serde_json::to_value(response).unwrap()["applied"],
                    serde_json::to_value(original).unwrap()
                );
            }
            assert_eq!(b.rpc.calls.len(), 2, "replay must never write");
        }
    }
    #[test]
    fn evicted_operation_is_unknown_and_cannot_be_replayed_as_a_new_write() {
        let mut current = CanonicalState::new(Flavor::Stable, Generation([1; 16]));
        let mut request = Request::Commit {
            expected: Expectation::Missing {},
            operation_id: "02".repeat(16),
            digest: String::new(),
            mutation: WireMutation::UpdateConsent {
                payload: json!({"consent":true,"scopes":null,"ids":null}),
            },
        };
        let hash = state::digest_bytes(&request_digest_bytes(&request).unwrap());
        let Request::Commit { digest, .. } = &mut request else {
            panic!()
        };
        *digest = hash.clone();
        for id in 2..=(state::LEDGER_CAPACITY as u8 + 2) {
            let operation = Operation {
                id: Generation([id; 16]),
                digest: hash.clone(),
                expected: current.expected(),
                mutation: Mutation::UpdateConsent {
                    consent: json!(true),
                    scopes: Value::Null,
                    ids: Value::Null,
                },
            };
            current = current.apply_verified_digest(&operation).unwrap().0;
        }
        let mut object = serde_json::to_value(current).unwrap();
        object["_rev"] = json!(20);
        object["_kind"] = json!("com.beb.plxnative.storage:1");
        let record = json!({"returnValue":true,"results":[object]});
        let mut b = backend(vec![Ok(record.clone()), Ok(record)]);
        assert!(matches!(
            b.dispatch(request),
            Response::Commit {
                status: CommitStatus::Conflict,
                applied: None,
                verified: false,
                ..
            }
        ));
        assert!(matches!(
            b.dispatch(Request::Reconcile {
                operation_id: "02".repeat(16),
                digest: hash
            }),
            Response::Reconcile {
                status: ReconcileStatus::Unknown,
                applied: None,
                ..
            }
        ));
        assert_eq!(b.rpc.calls.len(), 2);
    }
    #[test]
    fn all_auth_changing_mutations_cleanup_after_lost_ack_is_reconciled() {
        for mutation in [
            WireMutation::ClearTenure {},
            fresh_auth(),
            WireMutation::AdvanceMigration {
                migration: MigrationMutation::SessionComplete {
                    public: serde_json::to_value(state::PublicPayload::default()).unwrap(),
                    auth_plaintext: SecretString("fixture".into()),
                    protection: ProtectionRequest::Db8AclOnlyExplicit,
                },
            },
        ] {
            let mut b = cleanup_backend();
            b.rpc.lose_ack = true;
            assert!(matches!(
                b.dispatch(changing_request(mutation)),
                Response::Commit {
                    status: CommitStatus::Committed,
                    ..
                }
            ));
            assert_eq!(b.rpc.removed, vec![Generation([1; 16]).key_name()]);
        }
    }
    #[test]
    fn later_reconcile_cleans_only_after_applied_is_proven() {
        let mut b = cleanup_backend();
        b.rpc.lose_ack = true;
        b.rpc.fail_readback = true;
        let request = changing_request(WireMutation::ClearTenure {});
        let Request::Commit {
            operation_id,
            digest,
            ..
        } = &request
        else {
            panic!()
        };
        let reconcile = Request::Reconcile {
            operation_id: operation_id.clone(),
            digest: digest.clone(),
        };
        assert!(matches!(
            b.dispatch(request),
            Response::Commit {
                status: CommitStatus::Unavailable,
                ..
            }
        ));
        assert!(b.rpc.removed.is_empty());
        assert!(matches!(
            b.dispatch(reconcile),
            Response::Reconcile {
                status: ReconcileStatus::Applied,
                ..
            }
        ));
        assert_eq!(b.rpc.removed, vec![Generation([1; 16]).key_name()]);
    }
    #[test]
    fn unconfirmed_writes_and_acl_only_or_still_referenced_keys_are_never_removed() {
        let mut failed = cleanup_backend();
        failed.rpc.persist = false;
        failed.rpc.lose_ack = true;
        assert!(matches!(
            failed.dispatch(changing_request(WireMutation::ClearTenure {})),
            Response::Commit {
                status: CommitStatus::Unavailable,
                ..
            }
        ));
        assert!(failed.rpc.removed.is_empty());
        let mut acl = cleanup_backend();
        acl.rpc.record["results"][0]["auth_envelope"] =
            json!("{\"format\":\"db8-acl-only-v1\",\"plaintext\":\"fixture\"}");
        assert!(matches!(
            acl.dispatch(changing_request(WireMutation::ClearTenure {})),
            Response::Commit {
                status: CommitStatus::Committed,
                ..
            }
        ));
        assert!(acl.rpc.removed.is_empty());
        let mut referenced = cleanup_backend();
        referenced.rpc.record["results"][0]["migrations"]["consent"]["pending_import"] =
            json!(Generation([1; 16]).key_name());
        assert!(matches!(
            referenced.dispatch(changing_request(fresh_auth())),
            Response::Commit {
                status: CommitStatus::Committed,
                ..
            }
        ));
        assert!(referenced.rpc.removed.is_empty());
        let mut unchanged = cleanup_backend();
        unchanged.rpc.record["results"][0]["migrations"]["session"]["progress"] = json!("Complete");
        assert!(matches!(
            unchanged.dispatch(changing_request(WireMutation::UpdatePreferences {
                payload: serde_json::to_value(state::PublicPayload::default()).unwrap()
            })),
            Response::Commit {
                status: CommitStatus::Committed,
                ..
            }
        ));
        assert!(unchanged.rpc.removed.is_empty());
    }
}
