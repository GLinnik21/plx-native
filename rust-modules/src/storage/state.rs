//! Canonical state transitions, independent of DB8 transport and domain adapters.
//!
//! A successful transition is a candidate, not a durability receipt. The transport must CAS
//! the DB8 revision it read, then reconcile uncertain writes using the operation ledger.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

pub(crate) const FORMAT: &str = "plxnative-state";
pub(crate) const SCHEMA: u32 = 1;
pub(crate) const MAX_LOGICAL_BYTES: usize = 256 * 1024;
pub(crate) const MAX_ENCODED_BYTES: usize = 512 * 1024;
pub(crate) const LEDGER_CAPACITY: usize = 16;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Flavor {
    Stable,
    Debug,
}

impl Flavor {
    pub(crate) fn object_id(self) -> &'static str {
        match self {
            Self::Stable => "plxstate.stable",
            Self::Debug => "plxstate.debug",
        }
    }
}

/// Full random 128 bits, without UUID version/variant bit truncation.
#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Generation(pub(crate) [u8; 16]);

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Expected {
    pub(crate) epoch: u64,
    pub(crate) auth_generation: Generation,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) enum Status {
    Active,
    Cleared,
}

/// Opaque domain-owned public data. Auth ciphertext has a separate, typed slot.
#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicPayload {
    pub(crate) preferences: Value,
    pub(crate) client_id: Option<String>,
    pub(crate) profile: Value,
    pub(crate) pins: Value,
    pub(crate) recents: Value,
    pub(crate) consent: Value,
    pub(crate) scopes: Value,
    pub(crate) ids: Value,
    pub(crate) account_extensions: Value,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) enum MigrationDomain {
    Session,
    Consent,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) enum MigrationProgress {
    #[default]
    Pending,
    Complete,
}

#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DomainMigration {
    pub(crate) progress: MigrationProgress,
    /// An opaque import reference/envelope, never an unprotected credential.
    pub(crate) pending_import: Option<String>,
}

#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Migrations {
    pub(crate) session: DomainMigration,
    pub(crate) consent: DomainMigration,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConsentPayload {
    pub(crate) consent: Value,
    pub(crate) scopes: Value,
    pub(crate) ids: Value,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProtectedAuth {
    pub(crate) auth_generation: Generation,
    pub(crate) envelope: String,
}

/// A migration completion installs its domain in the same atomic candidate.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum MigrationAdvance {
    SessionPending {
        opaque_envelope: String,
    },
    SessionComplete {
        public: PublicPayload,
        auth: ProtectedAuth,
    },
    ConsentComplete {
        consent: ConsentPayload,
    },
    CompleteEmpty {
        domain: MigrationDomain,
    },
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CanonicalState {
    pub(crate) format: String,
    pub(crate) schema: u32,
    pub(crate) flavor: Flavor,
    #[serde(rename = "_id")]
    pub(crate) id: String,
    pub(crate) revision: u64,
    pub(crate) epoch: u64,
    pub(crate) auth_generation: Generation,
    pub(crate) status: Status,
    pub(crate) public: PublicPayload,
    /// Preserve exactly, including unknown crypto formats and whitespace.
    pub(crate) auth_envelope: Option<String>,
    pub(crate) migrations: Migrations,
    pub(crate) operations: Vec<LedgerEntry>,
    /// Prepared strict refresh. Active auth/public/ledger remain authoritative until promotion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pending_auth: Option<PendingAuth>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingAuth {
    pub(crate) operation: Operation,
    pub(crate) plaintext_digest: String,
}

/// This context is stable across settings revisions and is the input to auth metadata/AAD.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuthContext {
    pub(crate) format: String,
    pub(crate) schema: u32,
    pub(crate) flavor: Flavor,
    pub(crate) epoch: u64,
    pub(crate) auth_generation: Generation,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum Mutation {
    UpdatePreferences {
        public: PublicPayload,
    },
    UpdateConsent {
        consent: Value,
        scopes: Value,
        ids: Value,
    },
    ReplaceAuth {
        auth_generation: Generation,
        auth_envelope: String,
        public: PublicPayload,
    },
    AdvanceMigration {
        domain: MigrationDomain,
        payload: MigrationAdvance,
    },
    ClearTenure {
        auth_generation: Generation,
    },
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Operation {
    pub(crate) id: Generation,
    pub(crate) digest: String,
    pub(crate) expected: Expected,
    pub(crate) mutation: Mutation,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Applied {
    pub(crate) revision: u64,
    pub(crate) epoch: u64,
    pub(crate) auth_generation: Generation,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LedgerEntry {
    pub(crate) id: Generation,
    pub(crate) digest: String,
    pub(crate) result: Applied,
    /// Older records did not identify auth writes. Never infer successful authentication
    /// merely from their presence in the operation ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) verification: Option<ReceiptVerification>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum ReceiptVerification {
    Public,
    /// The helper adds a canonical plaintext digest before persisting the candidate. It is
    /// local verification material, never a diagnostic or telemetry value.
    Auth {
        plaintext_digest: Option<String>,
    },
}

/// Absence never proves NotApplied: the operation may have been evicted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OperationStatus {
    Applied(Applied),
    Unknown,
}

/// Closed, payload-free diagnostics; no serde error text escapes the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StateError {
    Invalid,
    UnsupportedFormat,
    UnsupportedSchema,
    FlavorMismatch,
    IdCollision,
    LogicalTooLarge,
    EncodedTooLarge,
    Conflict,
    Exhausted,
    RandomUnavailable,
}

macro_rules! redacted_debug {
    ($($ty:ty),+ $(,)?) => { $(impl fmt::Debug for $ty {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(concat!(stringify!($ty), " { <redacted> }"))
        }
    })+ };
}
redacted_debug!(
    Generation,
    PublicPayload,
    DomainMigration,
    Migrations,
    CanonicalState,
    AuthContext,
    Mutation,
    Operation,
    LedgerEntry,
    ReceiptVerification,
    ConsentPayload,
    ProtectedAuth,
    MigrationAdvance,
    PendingAuth
);

impl Generation {
    pub(crate) fn random() -> Result<Self, StateError> {
        use std::io::Read;
        let mut bytes = [0; 16];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut bytes))
            .map_err(|_| StateError::RandomUnavailable)?;
        Ok(Self(bytes))
    }

    pub(crate) fn key_name(self) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut name = String::with_capacity(24);
        name.push_str("a.");
        for chunk in self.0.chunks(3) {
            let bits = ((chunk[0] as u32) << 16)
                | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
                | chunk.get(2).copied().unwrap_or(0) as u32;
            for shift in [18, 12, 6, 0].into_iter().take(chunk.len() + 1) {
                name.push(ALPHABET[((bits >> shift) & 63) as usize] as char);
            }
        }
        name
    }
}

impl Operation {
    pub(crate) fn new(
        id: Generation,
        expected: Expected,
        mutation: Mutation,
    ) -> Result<Self, StateError> {
        let mut operation = Self {
            id,
            digest: String::new(),
            expected,
            mutation,
        };
        operation.digest = operation.computed_digest()?;
        Ok(operation)
    }

    /// Domain-separated SHA-256 of the complete typed request (except its digest).
    /// JSON maps are recursively sorted; transport whitespace/property order is irrelevant.
    pub(crate) fn computed_digest(&self) -> Result<String, StateError> {
        let value = serde_json::to_value((FORMAT, SCHEMA, self.id, self.expected, &self.mutation))
            .map_err(|_| StateError::Invalid)?;
        check_logical_size(&value)?;
        let bytes = serde_json::to_vec(&sorted_json(value)).map_err(|_| StateError::Invalid)?;
        if bytes.len() > MAX_ENCODED_BYTES {
            return Err(StateError::EncodedTooLarge);
        }
        Ok(digest_bytes(&bytes))
    }
}

impl CanonicalState {
    pub(crate) fn new(flavor: Flavor, auth_generation: Generation) -> Self {
        Self {
            format: FORMAT.into(),
            schema: SCHEMA,
            flavor,
            id: flavor.object_id().into(),
            revision: 0,
            epoch: 0,
            auth_generation,
            status: Status::Active,
            public: PublicPayload::default(),
            auth_envelope: None,
            migrations: Migrations::default(),
            operations: Vec::new(),
            pending_auth: None,
        }
    }
    pub(crate) fn expected(&self) -> Expected {
        Expected {
            epoch: self.epoch,
            auth_generation: self.auth_generation,
        }
    }
    pub(crate) fn auth_context(&self) -> AuthContext {
        AuthContext {
            format: self.format.clone(),
            schema: self.schema,
            flavor: self.flavor,
            epoch: self.epoch,
            auth_generation: self.auth_generation,
        }
    }
    pub(crate) fn decode(bytes: &[u8], flavor: Flavor) -> Result<Self, StateError> {
        if bytes.len() > MAX_ENCODED_BYTES {
            return Err(StateError::EncodedTooLarge);
        }
        let state: Self = serde_json::from_slice(bytes).map_err(|_| StateError::Invalid)?;
        if state.flavor != flavor {
            return Err(StateError::FlavorMismatch);
        }
        state.validate()?;
        Ok(state)
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>, StateError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| StateError::Invalid)?;
        if bytes.len() > MAX_ENCODED_BYTES {
            return Err(StateError::EncodedTooLarge);
        }
        Ok(bytes)
    }

    /// Reconstruct exactly the prepared transition, without recording it as active/Applied.
    /// The stored digest binds the original authenticated wire request, not random ciphertext.
    pub(crate) fn pending_candidate(&self) -> Result<Option<(Self, Applied)>, StateError> {
        let Some(pending) = &self.pending_auth else {
            return Ok(None);
        };
        if !valid_digest(&pending.plaintext_digest)
            || !matches!(pending.operation.mutation, Mutation::ReplaceAuth { .. })
            || self
                .operations
                .iter()
                .any(|entry| entry.id == pending.operation.id)
        {
            return Err(StateError::Invalid);
        }
        let mut active = self.clone();
        active.pending_auth = None;
        let (mut candidate, applied) = active.apply_verified_digest(&pending.operation)?;
        candidate
            .operations
            .last_mut()
            .ok_or(StateError::Invalid)?
            .verification = Some(ReceiptVerification::Auth {
            plaintext_digest: Some(pending.plaintext_digest.clone()),
        });
        Ok(Some((candidate, applied)))
    }

    fn validate(&self) -> Result<(), StateError> {
        if self.format != FORMAT {
            return Err(StateError::UnsupportedFormat);
        }
        if self.schema != SCHEMA {
            return Err(StateError::UnsupportedSchema);
        }
        if self.id != self.flavor.object_id() {
            return Err(StateError::IdCollision);
        }
        if self.operations.len() > LEDGER_CAPACITY {
            return Err(StateError::Invalid);
        }
        if self.pending_auth.is_some() {
            if self.status == Status::Cleared {
                return Err(StateError::Invalid);
            }
            self.pending_candidate()?;
        }
        for (index, entry) in self.operations.iter().enumerate() {
            if !valid_digest(&entry.digest)
                || entry.result.revision == 0
                || entry.result.revision > self.revision
                || entry.result.epoch > self.epoch
                || self.operations[..index]
                    .iter()
                    .any(|older| older.id == entry.id)
                || (index > 0
                    && self.operations[index - 1].result.revision >= entry.result.revision)
            {
                return Err(StateError::Invalid);
            }
            if matches!(&entry.verification,
                Some(ReceiptVerification::Auth { plaintext_digest: Some(digest) }) if !valid_digest(digest))
            {
                return Err(StateError::Invalid);
            }
        }
        for migration in [&self.migrations.session, &self.migrations.consent] {
            if migration.progress == MigrationProgress::Complete
                && migration.pending_import.is_some()
            {
                return Err(StateError::Invalid);
            }
        }
        if self.status == Status::Cleared
            && (self.auth_envelope.is_some()
                || self.public.client_id.is_some()
                || !self.public.profile.is_null()
                || !self.public.pins.is_null()
                || !self.public.recents.is_null()
                || !self.public.consent.is_null()
                || !self.public.scopes.is_null()
                || !self.public.ids.is_null()
                || !self.public.account_extensions.is_null()
                || self.migrations.session.progress != MigrationProgress::Complete
                || self.migrations.consent.progress != MigrationProgress::Complete)
        {
            return Err(StateError::Invalid);
        }
        check_logical_size(&serde_json::to_value(self).map_err(|_| StateError::Invalid)?)
    }

    pub(crate) fn operation_status(
        &self,
        id: Generation,
        digest: &str,
    ) -> Result<OperationStatus, StateError> {
        self.validate()?;
        if !valid_digest(digest) {
            return Err(StateError::Invalid);
        }
        match self.operations.iter().find(|entry| entry.id == id) {
            Some(entry) if entry.digest == digest => Ok(OperationStatus::Applied(entry.result)),
            Some(_) => Err(StateError::Invalid),
            None => Ok(OperationStatus::Unknown),
        }
    }

    /// Validate and advance a clone. The caller must persist the whole candidate atomically.
    /// A replay returns its original receipt without advancing state, including after logout.
    pub(crate) fn apply(&self, operation: &Operation) -> Result<(Self, Applied), StateError> {
        if operation.computed_digest()? != operation.digest {
            return Err(StateError::Invalid);
        }
        self.apply_verified_digest(operation)
    }

    /// Trusted helper boundary ONLY: the helper has recomputed this digest over the complete
    /// canonical plaintext request before preparing its encrypted envelope. Random sealing
    /// nonces must not alter operation identity. Never call this with an unverified wire digest.
    pub(crate) fn apply_verified_digest(
        &self,
        operation: &Operation,
    ) -> Result<(Self, Applied), StateError> {
        self.validate()?;
        if !valid_digest(&operation.digest) {
            return Err(StateError::Invalid);
        }
        if let OperationStatus::Applied(result) =
            self.operation_status(operation.id, &operation.digest)?
        {
            return Ok((self.clone(), result));
        }
        if operation.expected != self.expected() {
            return Err(StateError::Conflict);
        }
        if matches!(&self.pending_auth, Some(pending) if pending.operation.id == operation.id) {
            // Only the helper's pending-candidate promotion may consume this reserved identity.
            return Err(StateError::Invalid);
        }
        if self.pending_auth.is_some()
            && !matches!(
                operation.mutation,
                Mutation::UpdatePreferences { .. }
                    | Mutation::UpdateConsent { .. }
                    | Mutation::ClearTenure { .. }
                    | Mutation::ReplaceAuth { .. }
            )
        {
            return Err(StateError::Conflict);
        }
        if matches!(operation.mutation, Mutation::UpdatePreferences { .. })
            && (self.migrations.session.progress != MigrationProgress::Complete
                || self.auth_envelope.is_none())
        {
            return Err(StateError::Conflict);
        }
        if self.status == Status::Cleared
            && !matches!(
                operation.mutation,
                Mutation::ReplaceAuth { .. } | Mutation::ClearTenure { .. }
            )
        {
            return Err(StateError::Conflict);
        }
        let mut next = self.clone();
        // A separately authenticated public decision or logout supersedes the pending refresh.
        // Its CAS cancels the candidate and prevents a delayed promotion at the old DB8 rev.
        next.pending_auth = None;
        match &operation.mutation {
            Mutation::UpdatePreferences { public } => {
                // Session owns these fields. Consent has a separate mutation and cannot be
                // overwritten by a stale session snapshot.
                next.public.preferences = public.preferences.clone();
                next.public.client_id = public.client_id.clone();
                next.public.profile = public.profile.clone();
                next.public.pins = public.pins.clone();
                next.public.recents = public.recents.clone();
                next.public.account_extensions = public.account_extensions.clone();
            }
            Mutation::UpdateConsent {
                consent,
                scopes,
                ids,
            } => {
                next.public.consent = consent.clone();
                next.public.scopes = scopes.clone();
                next.public.ids = ids.clone();
                // A current user decision supersedes every legacy consent candidate just as an
                // explicit sign-in supersedes a pending Session import.
                next.migrations.consent = DomainMigration {
                    progress: MigrationProgress::Complete,
                    pending_import: None,
                };
            }
            Mutation::ReplaceAuth {
                auth_generation,
                auth_envelope,
                public,
            } => {
                self.check_fresh_generation(*auth_generation)?;
                if auth_envelope.is_empty() {
                    return Err(StateError::Invalid);
                }
                next.auth_generation = *auth_generation;
                next.auth_envelope = Some(auth_envelope.clone());
                next.public = public.clone();
                // Authentication owns Session data, never the independently committed Consent.
                next.public.consent = self.public.consent.clone();
                next.public.scopes = self.public.scopes.clone();
                next.public.ids = self.public.ids.clone();
                next.status = Status::Active;
                // Explicit sign-in supersedes any legacy Session candidate. Consent migration
                // remains independently pending until its own completion or a tenure clear.
                next.migrations.session = DomainMigration {
                    progress: MigrationProgress::Complete,
                    pending_import: None,
                };
            }
            Mutation::AdvanceMigration { domain, payload } => {
                let current = match domain {
                    MigrationDomain::Session => &self.migrations.session,
                    MigrationDomain::Consent => &self.migrations.consent,
                };
                if current.progress == MigrationProgress::Complete {
                    return Err(StateError::Invalid);
                }
                let mut completed = true;
                match (domain, payload) {
                    (
                        MigrationDomain::Session,
                        MigrationAdvance::SessionPending { opaque_envelope },
                    ) => {
                        if opaque_envelope.is_empty() {
                            return Err(StateError::Invalid);
                        }
                        next.migrations.session.pending_import = Some(opaque_envelope.clone());
                        completed = false;
                    }
                    (
                        MigrationDomain::Session,
                        MigrationAdvance::SessionComplete { public, auth },
                    ) => {
                        self.check_fresh_generation(auth.auth_generation)?;
                        if auth.envelope.is_empty() {
                            return Err(StateError::Invalid);
                        }
                        next.public = public.clone();
                        // Session owns no consent: retain any independently committed import.
                        next.public.consent = self.public.consent.clone();
                        next.public.scopes = self.public.scopes.clone();
                        next.public.ids = self.public.ids.clone();
                        next.auth_generation = auth.auth_generation;
                        next.auth_envelope = Some(auth.envelope.clone());
                        next.status = Status::Active;
                    }
                    (MigrationDomain::Consent, MigrationAdvance::ConsentComplete { consent }) => {
                        next.public.consent = consent.consent.clone();
                        next.public.scopes = consent.scopes.clone();
                        next.public.ids = consent.ids.clone();
                    }
                    (
                        actual,
                        MigrationAdvance::CompleteEmpty {
                            domain: payload_domain,
                        },
                    ) if actual == payload_domain => {}
                    _ => return Err(StateError::Invalid),
                }
                if completed {
                    let migration = match domain {
                        MigrationDomain::Session => &mut next.migrations.session,
                        MigrationDomain::Consent => &mut next.migrations.consent,
                    };
                    migration.progress = MigrationProgress::Complete;
                    migration.pending_import = None;
                }
            }
            Mutation::ClearTenure { auth_generation } => {
                self.check_fresh_generation(*auth_generation)?;
                next.epoch = next.epoch.checked_add(1).ok_or(StateError::Exhausted)?;
                next.auth_generation = *auth_generation;
                next.status = Status::Cleared;
                next.auth_envelope = None;
                next.public = PublicPayload {
                    preferences: self.public.preferences.clone(),
                    ..PublicPayload::default()
                };
                let complete = DomainMigration {
                    progress: MigrationProgress::Complete,
                    pending_import: None,
                };
                next.migrations = Migrations {
                    session: complete.clone(),
                    consent: complete,
                };
            }
        }
        next.revision = next.revision.checked_add(1).ok_or(StateError::Exhausted)?;
        let result = Applied {
            revision: next.revision,
            epoch: next.epoch,
            auth_generation: next.auth_generation,
        };
        next.operations.push(LedgerEntry {
            id: operation.id,
            digest: operation.digest.clone(),
            result,
            verification: Some(
                if matches!(
                    operation.mutation,
                    Mutation::ReplaceAuth { .. }
                        | Mutation::AdvanceMigration {
                            payload: MigrationAdvance::SessionComplete { .. },
                            ..
                        }
                ) {
                    ReceiptVerification::Auth {
                        plaintext_digest: None,
                    }
                } else {
                    ReceiptVerification::Public
                },
            ),
        });
        if next.operations.len() > LEDGER_CAPACITY {
            next.operations.remove(0);
        }
        // Validate the complete candidate including its ledger and encoded JSON expansion.
        next.encode()?;
        Ok((next, result))
    }

    fn check_fresh_generation(&self, generation: Generation) -> Result<(), StateError> {
        // Randomness is supplied by Generation::random at the adapter boundary. Reject known
        // collisions too; the bounded ledger is not an alternative to a cryptographic RNG.
        if generation == self.auth_generation
            || matches!(self.pending_auth.as_ref().map(|pending| &pending.operation.mutation), Some(Mutation::ReplaceAuth { auth_generation, .. }) if *auth_generation == generation)
            || self
                .operations
                .iter()
                .any(|entry| entry.result.auth_generation == generation)
        {
            return Err(StateError::Invalid);
        }
        Ok(())
    }
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Logical JSON bytes count UTF-8 content before escaping, with JSON structural overhead.
/// The bound includes metadata, public data, the protected envelope, imports, and the ledger.
fn check_logical_size(value: &Value) -> Result<(), StateError> {
    fn count(value: &Value, left: &mut usize) -> Result<(), StateError> {
        fn consume(left: &mut usize, n: usize) -> Result<(), StateError> {
            *left = left.checked_sub(n).ok_or(StateError::LogicalTooLarge)?;
            Ok(())
        }
        match value {
            Value::Null => consume(left, 4),
            Value::Bool(value) => consume(left, if *value { 4 } else { 5 }),
            Value::Number(value) => consume(left, value.to_string().len()),
            Value::String(value) => {
                consume(left, 2)?;
                consume(left, value.len())
            }
            Value::Array(values) => {
                consume(left, 2 + values.len().saturating_sub(1))?;
                for value in values {
                    count(value, left)?;
                }
                Ok(())
            }
            Value::Object(values) => {
                consume(left, 2 + values.len().saturating_sub(1))?;
                for (key, value) in values {
                    consume(left, 3)?;
                    consume(left, key.len())?;
                    count(value, left)?;
                }
                Ok(())
            }
        }
    }
    let mut remaining = MAX_LOGICAL_BYTES;
    count(value, &mut remaining)
}

fn sorted_json(value: Value) -> Value {
    match value {
        Value::Object(values) => {
            let sorted: std::collections::BTreeMap<_, _> = values
                .into_iter()
                .map(|(key, value)| (key, sorted_json(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(values) => Value::Array(values.into_iter().map(sorted_json).collect()),
        value => value,
    }
}

/// Shared SHA-256 encoding for the helper's canonical plaintext wire operation.
pub(crate) fn digest_bytes(input: &[u8]) -> String {
    sha256(input)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// SHA-256 (FIPS 180-4), used only for bounded operation identity, not authentication.
fn sha256(input: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&((input.len() as u64) * 8).to_be_bytes());
    for block in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (slot, word) in w.iter_mut().zip(block.chunks_exact(4)) {
            *slot = u32::from_be_bytes(word.try_into().expect("four-byte SHA word"));
        }
        for i in 16..64 {
            let x = w[i - 15];
            let y = w[i - 2];
            let s0 = x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3);
            let s1 = y.rotate_right(17) ^ y.rotate_right(19) ^ (y >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(majority);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut output = [0; 32];
    for (slot, word) in output.chunks_exact_mut(4).zip(h) {
        slot.copy_from_slice(&word.to_be_bytes());
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn generation(n: u8) -> Generation {
        Generation([n; 16])
    }
    fn bootstrap() -> CanonicalState {
        CanonicalState::new(Flavor::Stable, generation(1))
    }
    fn initial() -> CanonicalState {
        let mut state = bootstrap();
        state.auth_envelope = Some("protected-fixture".into());
        state.migrations.session.progress = MigrationProgress::Complete;
        state
    }
    fn operation(state: &CanonicalState, id: u8, mutation: Mutation) -> Operation {
        Operation::new(generation(id), state.expected(), mutation).unwrap()
    }
    fn preferences() -> Mutation {
        Mutation::UpdatePreferences {
            public: PublicPayload {
                preferences: json!({"volume": 30}),
                ..PublicPayload::default()
            },
        }
    }

    #[test]
    fn pending_auth_is_bounded_validated_redacted_and_not_an_applied_receipt() {
        let old = initial();
        let pending = PendingAuth {
            operation: operation(
                &old,
                2,
                Mutation::ReplaceAuth {
                    auth_generation: generation(3),
                    auth_envelope: "secret-candidate-fixture".into(),
                    public: PublicPayload::default(),
                },
            ),
            plaintext_digest: "a".repeat(64),
        };
        let mut staged = old.clone();
        staged.pending_auth = Some(pending.clone());
        assert_eq!(
            staged
                .operation_status(generation(2), &pending.operation.digest)
                .unwrap(),
            OperationStatus::Unknown
        );
        let decoded = CanonicalState::decode(&staged.encode().unwrap(), Flavor::Stable).unwrap();
        assert_eq!(decoded.auth_envelope, old.auth_envelope);
        assert_eq!(decoded.auth_generation, old.auth_generation);
        assert!(decoded.operations.is_empty());
        assert!(!format!("{decoded:?} {pending:?}").contains("secret-candidate-fixture"));
        let (candidate, applied) = decoded.pending_candidate().unwrap().unwrap();
        assert!(candidate.pending_auth.is_none());
        assert_eq!(candidate.auth_generation, generation(3));
        assert_eq!(
            candidate
                .operation_status(generation(2), &pending.operation.digest)
                .unwrap(),
            OperationStatus::Applied(applied)
        );
        for variant in 0..4 {
            let mut bad = staged.clone();
            let pending = bad.pending_auth.as_mut().unwrap();
            match variant {
                0 => pending.operation.expected.epoch += 1,
                1 => pending.operation.mutation = preferences(),
                2 => pending.plaintext_digest = "invalid".into(),
                _ => pending.operation.digest = "invalid".into(),
            }
            assert!(bad.encode().is_err());
        }
        assert_eq!(
            staged.apply(&operation(&staged, 2, preferences())),
            Err(StateError::Invalid)
        );
    }

    #[test]
    fn consent_bootstraps_before_auth_but_old_tenure_cannot_restore_it() {
        let state = bootstrap();
        let consent = Mutation::UpdateConsent {
            consent: json!(true),
            scopes: json!([]),
            ids: json!({}),
        };
        let stale = operation(&state, 2, consent.clone());
        let (consented, _) = state.apply(&stale).unwrap();
        assert_eq!(consented.public.consent, json!(true));
        assert_eq!(
            consented.migrations.session.progress,
            MigrationProgress::Pending
        );
        assert!(consented.auth_envelope.is_none());
        let (cleared, _) = consented
            .apply(&operation(
                &consented,
                3,
                Mutation::ClearTenure {
                    auth_generation: generation(4),
                },
            ))
            .unwrap();
        let stale = Operation::new(generation(5), state.expected(), consent.clone()).unwrap();
        assert_eq!(cleared.apply(&stale), Err(StateError::Conflict));
        assert_eq!(
            cleared.apply(&operation(&cleared, 6, consent)),
            Err(StateError::Conflict)
        );
    }

    #[test]
    fn exact_ids_and_key_names() {
        assert_eq!(Flavor::Stable.object_id().as_bytes(), b"plxstate.stable");
        assert_eq!(Flavor::Debug.object_id().as_bytes(), b"plxstate.debug");
        assert_eq!(Flavor::Stable.object_id().len(), 15);
        assert_eq!(Flavor::Debug.object_id().len(), 14);
        assert_eq!(Generation([0; 16]).key_name(), "a.AAAAAAAAAAAAAAAAAAAAAA");
        assert_eq!(Generation([255; 16]).key_name(), "a._____________________w");
    }

    #[test]
    fn public_updates_retain_unknown_envelope_and_crypto_context_exactly() {
        let mut state = initial();
        state.auth_envelope = Some(" {\"future_crypto\": 9, \"ciphertext\":\"secret\"} \n".into());
        let context = state.auth_context();
        let (updated, result) = state.apply(&operation(&state, 2, preferences())).unwrap();
        assert_eq!(result.revision, 1);
        assert_eq!(updated.auth_envelope, state.auth_envelope);
        assert_eq!(updated.auth_context(), context);
        let consent = Mutation::UpdateConsent {
            consent: json!(true),
            scopes: json!(["analytics"]),
            ids: json!({"analytics":"id"}),
        };
        let (updated, _) = updated.apply(&operation(&updated, 3, consent)).unwrap();
        assert_eq!(updated.auth_envelope, state.auth_envelope);
        assert_eq!(updated.auth_context(), context);
        assert_eq!(
            updated.migrations.consent.progress,
            MigrationProgress::Complete,
            "an explicit current decision must supersede every legacy consent candidate"
        );
        assert_eq!(
            CanonicalState::decode(&updated.encode().unwrap(), Flavor::Stable).unwrap(),
            updated
        );
    }
    #[test]
    fn legacy_ledgers_decode_and_new_writes_add_only_their_verification_marker() {
        let state = initial();
        let (current, _) = state.apply(&operation(&state, 2, preferences())).unwrap();
        let mut legacy = serde_json::to_value(&current).unwrap();
        legacy["operations"][0]
            .as_object_mut()
            .unwrap()
            .remove("verification");
        let old =
            CanonicalState::decode(&serde_json::to_vec(&legacy).unwrap(), Flavor::Stable).unwrap();
        assert!(old.operations[0].verification.is_none());
        let (next, _) = old.apply(&operation(&old, 3, preferences())).unwrap();
        assert!(next.operations[0].verification.is_none());
        assert!(matches!(
            next.operations[1].verification,
            Some(ReceiptVerification::Public)
        ));
        assert_eq!(next.auth_envelope, old.auth_envelope);
        assert_eq!(
            CanonicalState::decode(&next.encode().unwrap(), Flavor::Stable).unwrap(),
            next
        );
    }
    #[test]
    fn auth_receipt_plaintext_digest_is_validated_and_debug_redacted() {
        let state = initial();
        let (mut current, _) = state.apply(&operation(&state, 2, preferences())).unwrap();
        current.operations[0].verification = Some(ReceiptVerification::Auth {
            plaintext_digest: Some("private-fixture".into()),
        });
        assert!(current.encode().is_err());
        assert!(!format!("{:?}", current.operations[0].verification).contains("private-fixture"));
        current.operations[0].verification = Some(ReceiptVerification::Auth {
            plaintext_digest: Some("0".repeat(64)),
        });
        assert!(current.encode().is_ok());
    }

    #[test]
    fn consent_does_not_finish_session_import() {
        let state = bootstrap();
        let pending = Mutation::AdvanceMigration {
            domain: MigrationDomain::Session,
            payload: MigrationAdvance::SessionPending {
                opaque_envelope: "protected-import".into(),
            },
        };
        let (state, _) = state.apply(&operation(&state, 2, pending)).unwrap();
        let complete = Mutation::AdvanceMigration {
            domain: MigrationDomain::Consent,
            payload: MigrationAdvance::ConsentComplete {
                consent: ConsentPayload {
                    consent: json!(true),
                    scopes: json!(["analytics"]),
                    ids: json!({"analytics":"id"}),
                },
            },
        };
        let (state, _) = state.apply(&operation(&state, 3, complete)).unwrap();
        assert_eq!(
            state.migrations.session.progress,
            MigrationProgress::Pending
        );
        assert_eq!(
            state.migrations.session.pending_import.as_deref(),
            Some("protected-import")
        );
        assert_eq!(
            state.migrations.consent.progress,
            MigrationProgress::Complete
        );
    }

    #[test]
    fn logout_tombstone_clears_account_and_rejects_every_stale_mutation() {
        let mut state = initial();
        state.public = PublicPayload {
            preferences: json!({"volume":10}),
            client_id: Some("client".into()),
            profile: json!(1),
            pins: json!([1]),
            recents: json!([1]),
            consent: json!(true),
            scopes: json!([1]),
            ids: json!([1]),
            account_extensions: json!({"account":1}),
        };
        state.auth_envelope = Some("secret".into());
        state.migrations.session.pending_import = Some("pending".into());
        state.migrations.session.progress = MigrationProgress::Pending;
        let mutations = [
            preferences(),
            Mutation::UpdateConsent {
                consent: json!(true),
                scopes: Value::Null,
                ids: Value::Null,
            },
            Mutation::ReplaceAuth {
                auth_generation: generation(3),
                auth_envelope: "secret".into(),
                public: PublicPayload::default(),
            },
            Mutation::AdvanceMigration {
                domain: MigrationDomain::Session,
                payload: MigrationAdvance::CompleteEmpty {
                    domain: MigrationDomain::Session,
                },
            },
            Mutation::ClearTenure {
                auth_generation: generation(4),
            },
        ];
        let stale: Vec<_> = mutations
            .into_iter()
            .enumerate()
            .map(|(i, m)| operation(&state, i as u8 + 10, m))
            .collect();
        let (cleared, _) = state
            .apply(&operation(
                &state,
                2,
                Mutation::ClearTenure {
                    auth_generation: generation(2),
                },
            ))
            .unwrap();
        assert_eq!(cleared.status, Status::Cleared);
        assert_eq!(cleared.epoch, 1);
        assert_ne!(cleared.auth_generation, state.auth_generation);
        assert_eq!(cleared.auth_envelope, None);
        assert_eq!(
            cleared.public,
            PublicPayload {
                preferences: state.public.preferences,
                ..PublicPayload::default()
            }
        );
        assert_eq!(cleared.migrations.session.pending_import, None);
        assert_eq!(
            cleared.migrations.session.progress,
            MigrationProgress::Complete
        );
        assert_eq!(
            cleared.migrations.consent.progress,
            MigrationProgress::Complete
        );
        for request in stale {
            assert_eq!(cleared.apply(&request).unwrap_err(), StateError::Conflict);
        }
        assert_eq!(
            CanonicalState::decode(&cleared.encode().unwrap(), Flavor::Stable).unwrap(),
            cleared
        );
    }

    #[test]
    fn ledger_idempotence_collision_digest_verification_and_eviction() {
        let state = initial();
        let first = operation(&state, 2, preferences());
        let (mut state, receipt) = state.apply(&first).unwrap();
        assert_eq!(state.apply(&first).unwrap(), (state.clone(), receipt));
        let collision = operation(
            &state,
            2,
            Mutation::UpdatePreferences {
                public: PublicPayload {
                    preferences: json!(99),
                    ..PublicPayload::default()
                },
            },
        );
        assert_eq!(state.apply(&collision).unwrap_err(), StateError::Invalid);
        let mut forged = operation(&state, 3, preferences());
        forged.digest = "0".repeat(64);
        assert_eq!(state.apply(&forged).unwrap_err(), StateError::Invalid);
        for id in 3..=19 {
            state = state
                .apply(&operation(&state, id, preferences()))
                .unwrap()
                .0;
        }
        assert_eq!(state.operations.len(), LEDGER_CAPACITY);
        assert_eq!(
            state.operation_status(first.id, &first.digest),
            Ok(OperationStatus::Unknown)
        );
    }

    #[test]
    fn collision_schema_flavor_and_size_validation() {
        let state = initial();
        let bytes = state.encode().unwrap();
        assert_eq!(
            CanonicalState::decode(&bytes, Flavor::Debug).unwrap_err(),
            StateError::FlavorMismatch
        );
        let mut bad = state.clone();
        bad.id = "other".into();
        assert_eq!(bad.encode().unwrap_err(), StateError::IdCollision);
        bad = state.clone();
        bad.format = "other".into();
        assert_eq!(bad.encode().unwrap_err(), StateError::UnsupportedFormat);
        bad = state.clone();
        bad.schema += 1;
        assert_eq!(bad.encode().unwrap_err(), StateError::UnsupportedSchema);
        assert_eq!(
            CanonicalState::decode(&vec![b' '; MAX_ENCODED_BYTES + 1], Flavor::Stable).unwrap_err(),
            StateError::EncodedTooLarge
        );
        bad = state;
        bad.auth_envelope = Some("x".repeat(MAX_LOGICAL_BYTES + 1));
        assert_eq!(bad.encode().unwrap_err(), StateError::LogicalTooLarge);
    }

    #[test]
    fn closed_public_mutation_cannot_smuggle_an_envelope_field() {
        assert!(serde_json::from_value::<Mutation>(
            json!({"type":"UpdatePreferences", "preferences":{}, "auth_envelope":"secret"})
        )
        .is_err());
        assert!(serde_json::from_value::<Mutation>(json!({"type":"UpdateConsent", "consent":null,"scopes":null,"ids":null,"auth_envelope":"secret"})).is_err());
    }

    #[test]
    fn debug_never_discloses_payloads() {
        let mut state = initial();
        state.auth_envelope = Some("SECRET_SENTINEL".into());
        state.public.preferences = json!("SECRET_SENTINEL");
        let op = operation(
            &state,
            2,
            Mutation::UpdatePreferences {
                public: PublicPayload {
                    preferences: json!("SECRET_SENTINEL"),
                    ..PublicPayload::default()
                },
            },
        );
        assert!(!format!("{state:?} {op:?} {:?}", state.public).contains("SECRET_SENTINEL"));
    }

    #[test]
    fn sha256_nist_known_vectors() {
        for (input, expected) in [
            (
                "",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                "abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
        ] {
            let actual = digest_bytes(input.as_bytes());
            assert_eq!(actual, expected);
        }
        let million = digest_bytes(&vec![b'a'; 1_000_000]);
        assert_eq!(
            million,
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn digest_is_canonical_and_binds_both_guards_and_mutation() {
        let state = initial();
        let a = operation(
            &state,
            2,
            Mutation::UpdatePreferences {
                public: PublicPayload {
                    preferences: serde_json::from_str(r#"{"b":{"z":2,"a":1},"a":0}"#).unwrap(),
                    ..PublicPayload::default()
                },
            },
        );
        let b = operation(
            &state,
            2,
            Mutation::UpdatePreferences {
                public: PublicPayload {
                    preferences: serde_json::from_str(r#"{ "a": 0, "b": {"a":1,"z":2} }"#).unwrap(),
                    ..PublicPayload::default()
                },
            },
        );
        assert_eq!(a.digest, b.digest);
        let mut changed = a.clone();
        changed.expected.epoch += 1;
        assert_ne!(a.digest, changed.computed_digest().unwrap());
        changed = a.clone();
        changed.expected.auth_generation = generation(8);
        assert_ne!(a.digest, changed.computed_digest().unwrap());
        changed = a.clone();
        changed.id = generation(8);
        assert_ne!(a.digest, changed.computed_digest().unwrap());
        changed = a.clone();
        changed.mutation = preferences();
        assert_ne!(a.digest, changed.computed_digest().unwrap());
        assert_eq!(state.apply(&changed).unwrap_err(), StateError::Invalid);
    }

    #[test]
    fn helper_verified_plaintext_digest_survives_random_sealing_nonce() {
        let state = initial();
        let mut first = operation(
            &state,
            2,
            Mutation::ReplaceAuth {
                auth_generation: generation(3),
                auth_envelope: "sealed-with-nonce-one".into(),
                public: PublicPayload::default(),
            },
        );
        // Stand-in for the helper's independently verified canonical plaintext request digest.
        first.digest = sha256(b"canonical-plaintext-operation")
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(state.apply(&first).unwrap_err(), StateError::Invalid);
        let (state, receipt) = state.apply_verified_digest(&first).unwrap();
        let mut retry = first.clone();
        if let Mutation::ReplaceAuth { auth_envelope, .. } = &mut retry.mutation {
            *auth_envelope = "sealed-with-nonce-two".into();
        }
        assert_eq!(
            state.apply_verified_digest(&retry).unwrap(),
            (state.clone(), receipt)
        );
        assert_eq!(
            state.auth_envelope.as_deref(),
            Some("sealed-with-nonce-one")
        );
    }

    #[test]
    fn generations_and_epochs_independently_reject_stale_writers() {
        let state = initial();
        let mut stale = operation(&state, 2, preferences());
        let replace = operation(
            &state,
            3,
            Mutation::ReplaceAuth {
                auth_generation: generation(4),
                auth_envelope: "new-envelope".into(),
                public: PublicPayload::default(),
            },
        );
        let (replaced, _) = state.apply(&replace).unwrap();
        assert_eq!(replaced.epoch, state.epoch);
        assert_eq!(replaced.apply(&stale).unwrap_err(), StateError::Conflict);
        let (cleared, _) = replaced
            .apply(&operation(
                &replaced,
                5,
                Mutation::ClearTenure {
                    auth_generation: generation(6),
                },
            ))
            .unwrap();
        stale.expected.auth_generation = cleared.auth_generation;
        stale.digest = stale.computed_digest().unwrap();
        assert_eq!(cleared.apply(&stale).unwrap_err(), StateError::Conflict);
        // A newly signed-in account must not accept the old account's queued setting write.
        let login = operation(
            &cleared,
            7,
            Mutation::ReplaceAuth {
                auth_generation: generation(8),
                auth_envelope: "second-account".into(),
                public: PublicPayload::default(),
            },
        );
        let (active, _) = cleared.apply(&login).unwrap();
        assert_eq!(active.status, Status::Active);
        assert_eq!(active.epoch, cleared.epoch);
        assert_eq!(active.apply(&stale).unwrap_err(), StateError::Conflict);
    }

    #[test]
    fn cleared_state_accepts_safe_clear_retry_or_new_auth_and_replays_are_inert() {
        let state = initial();
        let setting = operation(&state, 2, preferences());
        let (state, _) = state.apply(&setting).unwrap();
        let clear = operation(
            &state,
            3,
            Mutation::ClearTenure {
                auth_generation: generation(4),
            },
        );
        let (cleared, receipt) = state.apply(&clear).unwrap();
        assert_eq!(cleared.apply(&clear).unwrap(), (cleared.clone(), receipt));
        assert_eq!(cleared.apply(&setting).unwrap().0, cleared);
        let retry = operation(
            &cleared,
            8,
            Mutation::ClearTenure {
                auth_generation: generation(10),
            },
        );
        let (recleared, _) = cleared.apply(&retry).unwrap();
        assert_eq!(recleared.status, Status::Cleared);
        assert!(recleared.auth_envelope.is_none());
        for mutation in [
            preferences(),
            Mutation::UpdateConsent {
                consent: json!(true),
                scopes: Value::Null,
                ids: Value::Null,
            },
            Mutation::AdvanceMigration {
                domain: MigrationDomain::Session,
                payload: MigrationAdvance::SessionPending {
                    opaque_envelope: "old-envelope".into(),
                },
            },
        ] {
            assert_eq!(
                cleared
                    .apply(&operation(&cleared, 9, mutation))
                    .unwrap_err(),
                StateError::Conflict
            );
        }
    }

    #[test]
    fn session_completion_is_atomic_and_preserves_independent_consent() {
        let state = bootstrap();
        let consent = Mutation::AdvanceMigration {
            domain: MigrationDomain::Consent,
            payload: MigrationAdvance::ConsentComplete {
                consent: ConsentPayload {
                    consent: json!({"analytics":true}),
                    scopes: json!(["scope"]),
                    ids: json!({"analytics":"id"}),
                },
            },
        };
        let (state, _) = state.apply(&operation(&state, 2, consent)).unwrap();
        let complete = Mutation::AdvanceMigration {
            domain: MigrationDomain::Session,
            payload: MigrationAdvance::SessionComplete {
                public: PublicPayload {
                    client_id: Some("client".into()),
                    ..PublicPayload::default()
                },
                auth: ProtectedAuth {
                    auth_generation: generation(4),
                    envelope: "protected".into(),
                },
            },
        };
        let (completed, _) = state.apply(&operation(&state, 3, complete)).unwrap();
        assert_eq!(completed.auth_envelope.as_deref(), Some("protected"));
        assert_eq!(completed.public.client_id.as_deref(), Some("client"));
        assert_eq!(completed.public.consent, state.public.consent);
        assert_eq!(completed.public.scopes, state.public.scopes);
        assert_eq!(completed.public.ids, state.public.ids);
        assert_eq!(
            completed.migrations.session.progress,
            MigrationProgress::Complete
        );
        assert_eq!(
            completed.migrations.consent.progress,
            MigrationProgress::Complete
        );
        let invalid = Mutation::AdvanceMigration {
            domain: MigrationDomain::Session,
            payload: MigrationAdvance::CompleteEmpty {
                domain: MigrationDomain::Consent,
            },
        };
        let initial = bootstrap();
        assert_eq!(
            initial.apply(&operation(&initial, 5, invalid)).unwrap_err(),
            StateError::Invalid
        );
    }

    #[test]
    fn exact_logical_boundary_and_json_expansion_are_independently_bounded() {
        let mut state = initial();
        state.auth_envelope = Some(String::new());
        let overhead = state.encode().unwrap().len();
        state.auth_envelope = Some("a".repeat(MAX_LOGICAL_BYTES - overhead));
        assert_eq!(state.encode().unwrap().len(), MAX_LOGICAL_BYTES);
        state.auth_envelope.as_mut().unwrap().push('a');
        assert_eq!(state.encode().unwrap_err(), StateError::LogicalTooLarge);
        state.auth_envelope = Some("\u{0000}".repeat(90_000));
        assert_eq!(state.encode().unwrap_err(), StateError::EncodedTooLarge);
        state.auth_envelope = Some("\u{0000}".repeat(60_000));
        let bytes = state.encode().unwrap();
        assert!(bytes.len() > MAX_LOGICAL_BYTES);
        assert_eq!(
            CanonicalState::decode(&bytes, Flavor::Stable).unwrap(),
            state
        );
    }

    #[test]
    fn malformed_ledger_and_counter_overflow_fail_without_mutation() {
        let state = initial();
        let (state, _) = state.apply(&operation(&state, 2, preferences())).unwrap();
        let mut bad = state.clone();
        bad.operations.push(bad.operations[0].clone());
        assert_eq!(bad.encode().unwrap_err(), StateError::Invalid);
        bad = state.clone();
        bad.operations[0].result.revision += 1;
        assert_eq!(bad.encode().unwrap_err(), StateError::Invalid);
        bad = state.clone();
        bad.operations[0].digest = "sensitive-bad-digest".into();
        assert_eq!(bad.encode().unwrap_err(), StateError::Invalid);
        bad = initial();
        bad.revision = u64::MAX;
        assert_eq!(
            bad.apply(&operation(&bad, 3, preferences())).unwrap_err(),
            StateError::Exhausted
        );
        assert_eq!(bad.revision, u64::MAX);
        bad = initial();
        bad.epoch = u64::MAX;
        assert_eq!(
            bad.apply(&operation(
                &bad,
                4,
                Mutation::ClearTenure {
                    auth_generation: generation(9)
                }
            ))
            .unwrap_err(),
            StateError::Exhausted
        );
        assert_eq!(bad.epoch, u64::MAX);
    }

    #[test]
    fn fresh_auth_rejects_current_or_recent_generation_reuse() {
        let state = initial();
        let same = operation(
            &state,
            2,
            Mutation::ClearTenure {
                auth_generation: state.auth_generation,
            },
        );
        assert_eq!(state.apply(&same).unwrap_err(), StateError::Invalid);
        let (state, _) = state.apply(&operation(&state, 2, preferences())).unwrap();
        let (state, _) = state
            .apply(&operation(
                &state,
                3,
                Mutation::ReplaceAuth {
                    auth_generation: generation(4),
                    auth_envelope: "cipher".into(),
                    public: PublicPayload::default(),
                },
            ))
            .unwrap();
        assert_eq!(
            state
                .apply(&operation(
                    &state,
                    5,
                    Mutation::ReplaceAuth {
                        auth_generation: generation(1),
                        auth_envelope: "cipher2".into(),
                        public: PublicPayload::default()
                    }
                ))
                .unwrap_err(),
            StateError::Invalid
        );
    }

    #[test]
    fn bootstrap_session_preferences_wait_for_auth_and_session_completion() {
        let state = bootstrap();
        assert_eq!(
            state
                .apply(&operation(&state, 2, preferences()))
                .unwrap_err(),
            StateError::Conflict
        );
        let import = Mutation::AdvanceMigration {
            domain: MigrationDomain::Consent,
            payload: MigrationAdvance::ConsentComplete {
                consent: ConsentPayload {
                    consent: json!(true),
                    scopes: Value::Null,
                    ids: Value::Null,
                },
            },
        };
        let (state, _) = state.apply(&operation(&state, 3, import)).unwrap();
        assert_eq!(
            state.migrations.session.progress,
            MigrationProgress::Pending
        );
        assert_eq!(state.auth_envelope, None);
        assert_eq!(state.public.consent, json!(true));
        assert_eq!(
            state
                .apply(&operation(&state, 4, preferences()))
                .unwrap_err(),
            StateError::Conflict
        );
        let login = Mutation::ReplaceAuth {
            auth_generation: generation(5),
            auth_envelope: "protected-login".into(),
            public: PublicPayload::default(),
        };
        let (state, _) = state.apply(&operation(&state, 5, login)).unwrap();
        assert_eq!(
            state.migrations.session.progress,
            MigrationProgress::Complete
        );
        assert!(state.apply(&operation(&state, 6, preferences())).is_ok());
        let mut missing_auth = state;
        missing_auth.auth_envelope = None;
        assert_eq!(
            missing_auth
                .apply(&operation(&missing_auth, 7, preferences()))
                .unwrap_err(),
            StateError::Conflict
        );
    }

    #[test]
    fn replace_auth_preserves_consent_import_and_cannot_restore_cleared_consent() {
        let state = bootstrap();
        let import = Mutation::AdvanceMigration {
            domain: MigrationDomain::Consent,
            payload: MigrationAdvance::ConsentComplete {
                consent: ConsentPayload {
                    consent: json!({"analytics": true}),
                    scopes: json!(["imported-scope"]),
                    ids: json!({"analytics": "imported-id"}),
                },
            },
        };
        let (imported, _) = state.apply(&operation(&state, 2, import)).unwrap();
        // Deliberately conflicting Consent fields in the Session payload must have no effect.
        let public = PublicPayload {
            client_id: Some("new-client".into()),
            preferences: json!({"volume": 20}),
            consent: json!({"analytics": false}),
            scopes: json!(["stale-scope"]),
            ids: json!({"analytics": "stale-id"}),
            ..PublicPayload::default()
        };
        let login = Mutation::ReplaceAuth {
            auth_generation: generation(3),
            auth_envelope: "protected-login".into(),
            public: public.clone(),
        };
        let (active, _) = imported.apply(&operation(&imported, 3, login)).unwrap();
        assert_eq!(active.public.consent, imported.public.consent);
        assert_eq!(active.public.scopes, imported.public.scopes);
        assert_eq!(active.public.ids, imported.public.ids);
        assert_eq!(active.public.client_id, public.client_id);
        assert_eq!(active.public.preferences, public.preferences);
        assert_eq!(active.migrations.consent, imported.migrations.consent);

        let clear = Mutation::ClearTenure {
            auth_generation: generation(4),
        };
        let (cleared, _) = active.apply(&operation(&active, 4, clear)).unwrap();
        let login = Mutation::ReplaceAuth {
            auth_generation: generation(5),
            auth_envelope: "protected-new-tenure".into(),
            public,
        };
        let (active, _) = cleared.apply(&operation(&cleared, 5, login)).unwrap();
        assert_eq!(active.status, Status::Active);
        assert!(active.public.consent.is_null());
        assert!(active.public.scopes.is_null());
        assert!(active.public.ids.is_null());
        assert_eq!(active.migrations.consent, cleared.migrations.consent);
    }
}
