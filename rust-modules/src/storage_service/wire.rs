//! Private local protocol. No request can choose a DB8 owner, kind, ID, or arbitrary method.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, Read, Write};

pub const PROTOCOL: u32 = 1;
// A Load carries both the encoded canonical state and opened auth. Reserve room for
// both bounded values plus the response metadata; a maximum state must remain loadable.
pub const MAX_FRAME: usize = 2 * super::state::MAX_ENCODED_BYTES + 4096;
pub const SOCKET_NAME: &str = "storage.sock";
pub const DESCRIPTOR_NAME: &str = "rendezvous.json";

#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretString(pub String);
impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Expectation {
    Missing {},
    Present {
        db_rev: String,
        epoch: String,
        auth_generation: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectionRequest {
    KeymanagerRequired,
    /// Availability policy for complete fresh login/import payloads only.
    KeymanagerWithAclFallback,
    Db8AclOnlyExplicit,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    Session,
    Consent,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MigrationMutation {
    SessionPending {
        opaque_envelope: SecretString,
    },
    SessionComplete {
        public: Value,
        auth_plaintext: SecretString,
        protection: ProtectionRequest,
    },
    ConsentComplete {
        consent: Value,
    },
    CompleteEmpty {
        domain: Domain,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireMutation {
    UpdatePreferences {
        payload: Value,
    },
    UpdateConsent {
        payload: Value,
    },
    ReplaceAuth {
        public: Value,
        payload: SecretString,
        protection: ProtectionRequest,
    },
    AdvanceMigration {
        migration: MigrationMutation,
    },
    ClearTenure {},
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Hello {
        protocol: u32,
        nonce: String,
    },
    Load {},
    Commit {
        expected: Expectation,
        operation_id: String,
        digest: String,
        mutation: WireMutation,
    },
    Reconcile {
        operation_id: String,
        digest: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub keymanager: bool,
    pub db8: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitStatus {
    Committed,
    Conflict,
    Invalid,
    Unavailable,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileStatus {
    Applied,
    NotApplied,
    Unknown,
    Invalid,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Invalid,
    Unavailable,
    Timeout,
    Capability,
    Authentication,
    Protocol,
    Corrupt,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectionClass {
    Keymanager,
    Db8AclOnly,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeymanagerOperation {
    Seal,
    Open,
    Readback,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeymanagerStage {
    Validate,
    Generate,
    Begin,
    Finish,
    Roundtrip,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeymanagerFailureCategory {
    Unavailable,
    Timeout,
    ServiceRejected,
    InvalidResponse,
    Other,
}
/// Closed diagnostic vocabulary; no platform text or cryptographic material may enter it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeymanagerFailure {
    pub operation: KeymanagerOperation,
    pub stage: KeymanagerStage,
    pub code: ErrorCode,
    pub category: KeymanagerFailureCategory,
    pub service_code: Option<i32>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackReason {
    FreshLogin,
    LegacyImport,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackPhase {
    Seal,
    PostwriteReadbackRepair,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FallbackContext {
    pub reason: FallbackReason,
    pub phase: FallbackPhase,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthPreservation {
    Unchanged,
    Restored,
    Uncertain,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectionOutcome {
    pub class: ProtectionClass,
    pub fallback: Option<KeymanagerFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_context: Option<FallbackContext>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthLoad {
    None,
    Plaintext { payload: SecretString },
    Locked { code: ErrorCode },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Hello {
        protocol: u32,
        nonce: String,
        helper_generation: String,
        capabilities: Capabilities,
    },
    Loaded {
        db_rev: String,
        state: Value,
        auth: AuthLoad,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protection: Option<ProtectionOutcome>,
    },
    Missing,
    Commit {
        status: CommitStatus,
        db_rev: Option<String>,
        state: Option<Value>,
        applied: Option<super::state::Applied>,
        /// Exact candidate matched after this write; auth/migration writes additionally
        /// authenticated the stored auth. Public-only writes never open retained auth.
        /// Historical ledger replays carry a receipt but cannot authorize source cleanup.
        verified: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protection: Option<ProtectionOutcome>,
    },
    Reconcile {
        status: ReconcileStatus,
        db_rev: Option<String>,
        applied: Option<super::state::Applied>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protection: Option<ProtectionOutcome>,
    },
    Error {
        code: ErrorCode,
    },
    KeymanagerError {
        failure: KeymanagerFailure,
        preservation: AuthPreservation,
        /// The pending DB8 write was read back exactly; this is not an Applied auth receipt.
        #[serde(default)]
        db8_commit_verified: bool,
    },
}
impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StorageResponse([REDACTED])")
    }
}
macro_rules! redacted_debug {
    ($($ty:ty),+ $(,)?) => { $(impl std::fmt::Debug for $ty {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(concat!(stringify!($ty), "([REDACTED])"))
        }
    })+ };
}
redacted_debug!(Request, WireMutation, MigrationMutation);
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub protocol: u32,
    pub nonce: String,
    pub helper_generation: String,
    pub socket: String,
    pub pid: u32,
    pub uid: u32,
}

/// The operation digest is SHA-256 of these compact, recursively sorted JSON bytes.
/// Convert through Value's BTreeMap so field order on the wire cannot alter the digest.
pub fn request_digest_bytes(request: &Request) -> Result<Vec<u8>, ErrorCode> {
    check_frame_size(request)?;
    let mut value = serde_json::to_value(request).map_err(|_| ErrorCode::Invalid)?;
    if !matches!(request, Request::Commit { .. }) {
        return Err(ErrorCode::Invalid);
    }
    value
        .as_object_mut()
        .ok_or(ErrorCode::Invalid)?
        .remove("digest");
    serde_json::to_vec(&value).map_err(|_| ErrorCode::Invalid)
}

pub fn check_frame_size(value: &impl Serialize) -> Result<(), ErrorCode> {
    let bytes = serde_json::to_vec(value).map_err(|_| ErrorCode::Invalid)?;
    if bytes.len() > MAX_FRAME {
        return Err(ErrorCode::Invalid);
    }
    Ok(())
}

pub fn read_frame<T: serde::de::DeserializeOwned>(reader: &mut impl Read) -> io::Result<T> {
    let mut header = [0u8; 4];
    reader.read_exact(&mut header)?;
    let size = u32::from_be_bytes(header) as usize;
    if size == 0 || size > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame size"));
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame JSON"))
}
pub fn write_frame<T: Serialize>(writer: &mut impl Write, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame JSON"))?;
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame size"));
    }
    writer.write_all(&(bytes.len() as u32).to_be_bytes())?;
    writer.write_all(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversize_and_unknown_requests_are_rejected() {
        assert!(read_frame::<Request>(&mut &((MAX_FRAME as u32 + 1).to_be_bytes())[..]).is_err());
        assert!(serde_json::from_str::<Request>(r#"{"type":"load","owner":"other"}"#).is_err());
        assert!(serde_json::from_str::<Request>(r#"{"type":"raw_db8","method":"put"}"#).is_err());
    }
    #[test]
    fn protection_diagnostics_reject_arbitrary_text_and_unknown_fields() {
        let good = serde_json::json!({"class":"db8_acl_only","fallback":{
            "operation":"seal","stage":"generate","code":"timeout","category":"timeout","service_code":null
        }});
        assert!(serde_json::from_value::<ProtectionOutcome>(good.clone()).is_ok());
        for (field, value) in [
            ("operation", serde_json::json!("private-fixture")),
            ("stage", serde_json::json!("private-fixture")),
            ("service_code", serde_json::json!("private-fixture")),
            ("errorText", serde_json::json!("private-fixture")),
        ] {
            let mut bad = good.clone();
            bad["fallback"][field] = value;
            assert!(serde_json::from_value::<ProtectionOutcome>(bad).is_err());
        }
    }

    #[test]
    fn fallback_context_and_strict_failure_have_only_closed_fields() {
        let context =
            serde_json::json!({"reason":"legacy_import","phase":"postwrite_readback_repair"});
        assert!(serde_json::from_value::<FallbackContext>(context.clone()).is_ok());
        for key in ["reason", "phase", "errorText", "key_name"] {
            let mut invalid = context.clone();
            invalid[key] = serde_json::json!("secret-sentinel");
            assert!(serde_json::from_value::<FallbackContext>(invalid).is_err());
        }
        let response = Response::KeymanagerError {
            failure: KeymanagerFailure {
                operation: KeymanagerOperation::Seal,
                stage: KeymanagerStage::Generate,
                code: ErrorCode::Timeout,
                category: KeymanagerFailureCategory::Timeout,
                service_code: None,
            },
            preservation: AuthPreservation::Unchanged,
            db8_commit_verified: false,
        };
        let mut value = serde_json::to_value(response).unwrap();
        assert!(serde_json::from_value::<Response>(value.clone()).is_ok());
        value["preservation"] = serde_json::json!("secret-sentinel");
        assert!(serde_json::from_value::<Response>(value).is_err());
    }
    #[test]
    fn revision_never_crosses_float() {
        let expected = Expectation::Present {
            db_rev: u64::MAX.to_string(),
            epoch: "e".into(),
            auth_generation: "g".into(),
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &expected).unwrap();
        let Expectation::Present { db_rev, .. } = read_frame(&mut bytes.as_slice()).unwrap() else {
            panic!()
        };
        assert_eq!(db_rev.parse::<u64>().unwrap(), u64::MAX);
    }
    #[test]
    fn secret_debug_does_not_disclose_payload() {
        assert!(!format!("{:?}", SecretString("secret-fixture".into())).contains("secret-fixture"));
        let mutation = WireMutation::UpdatePreferences {
            payload: serde_json::json!({"token":"secret-fixture"}),
        };
        assert!(!format!("{mutation:?}").contains("secret-fixture"));
        let request = Request::Commit {
            expected: Expectation::Missing {},
            operation_id: "secret-fixture".into(),
            digest: String::new(),
            mutation,
        };
        assert!(!format!("{request:?}").contains("secret-fixture"));
        assert!(!format!(
            "{:?}",
            Request::Hello {
                protocol: PROTOCOL,
                nonce: "secret-fixture".into()
            }
        )
        .contains("secret-fixture"));
    }
    #[test]
    fn maximum_encoded_state_plus_opened_auth_fits_complete_load_frame() {
        use super::super::state::{CanonicalState, Flavor, Generation, MAX_ENCODED_BYTES};
        let mut state = CanonicalState::new(Flavor::Stable, Generation([1; 16]));
        let baseline = state.encode().unwrap().len();
        // NUL expands to six JSON bytes while staying within the logical-state bound.
        let padding = MAX_ENCODED_BYTES - baseline - 2;
        state.auth_envelope = Some(format!(
            "{}{}",
            "\0".repeat(padding / 6),
            "x".repeat(padding % 6)
        ));
        // Replacing null with a string adds two quotes but removes four null bytes.
        let actual = state.encode().unwrap().len();
        assert!(actual <= MAX_ENCODED_BYTES && MAX_ENCODED_BYTES - actual < 8);
        let response = Response::Loaded {
            db_rev: u64::MAX.to_string(),
            protection: None,
            state: serde_json::to_value(state).unwrap(),
            auth: AuthLoad::Plaintext {
                payload: SecretString("x".repeat(MAX_ENCODED_BYTES - 2)),
            },
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &response).unwrap();
        assert!(matches!(
            read_frame::<Response>(&mut bytes.as_slice()).unwrap(),
            Response::Loaded { .. }
        ));
    }
    #[test]
    fn complete_request_is_bounded_before_digest_work() {
        let request = Request::Commit {
            expected: Expectation::Missing {},
            operation_id: "02".repeat(16),
            digest: String::new(),
            mutation: WireMutation::ReplaceAuth {
                public: Value::Null,
                payload: SecretString("x".repeat(MAX_FRAME)),
                protection: ProtectionRequest::KeymanagerRequired,
            },
        };
        assert!(request_digest_bytes(&request).is_err());
        assert!(write_frame(&mut Vec::new(), &request).is_err());
    }
}
