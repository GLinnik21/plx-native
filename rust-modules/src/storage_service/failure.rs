//! Closed, payload-free helper diagnostics shared by the app and helper executable.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    Unsupported, RuntimeAbsent, RuntimeInvalid, SocketAbsent, SocketInvalid, Connect,
    PeerCredentials, PeerUidMismatch, DescriptorInvalid, SocketTimeout, HelloRejected,
    Wire, ActivationContext, ActivationRegister, ActivationAttach, ActivationCall,
    ActivationSent, BusContext, BusRegister, BusAttach, BusCall, BusPayload, BusTimeout, BusCancel,
    Db8, LoadInvalid, LoadUnavailable, LoadTimeout, LoadAuthentication, LoadProtocol,
    LoadCorrupt, Unknown,
    RequestSize, ResponseSize, RequestDigest, DigestMismatch, ReconcileId, ReconcileLoad, ReconcileLedger, OperationId, LoadBeforeCommit, Ledger, ExpectedDbRev, ExpectedEpoch, ExpectedAuthGeneration, Prepare, Apply, Encode, EncodeJson, ReadbackLedger, OpenReadback, CommitResponse, PlaintextMismatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detail {
    pub stage: Stage,
    pub code: Option<i32>,
}
impl Detail {
    pub const fn new(stage: Stage, code: Option<i32>) -> Self { Self { stage, code } }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperFailure {
    pub observed: Detail,
    pub start_timeout: bool,
    pub activation: Option<Detail>,
    pub helper: Option<Detail>,
    #[serde(default)]
    pub wire: Option<super::ErrorCode>,
}
impl HelperFailure {
    pub fn new(stage: Stage, code: Option<i32>) -> Self {
        Self { observed: Detail::new(stage, code), start_timeout: false, activation: None, helper: None, wire: None }
    }
    #[allow(dead_code)] // App read-out; the helper shares the diagnostic schema only.
    pub fn line(self) -> String {
        let detail = self.helper.unwrap_or(self.observed);
        let fallback = serde_json::to_value(detail.stage).unwrap();
        let name = match detail.stage {
            Stage::RuntimeAbsent => "no runtime dir",
            Stage::RuntimeInvalid => "runtime invalid",
            Stage::SocketAbsent => "no socket",
            Stage::SocketInvalid => "socket invalid",
            Stage::Connect if detail.code == Some(libc::ECONNREFUSED) => "connect refused",
            Stage::Connect => "connect failed",
            Stage::PeerUidMismatch => "peer uid mismatch",
            Stage::DescriptorInvalid => "descriptor invalid",
            Stage::HelloRejected => "hello rejected",
            _ => fallback.as_str().unwrap(),
        };
        let phase = if self.start_timeout { "start-timeout" } else { "helper" };
        let code = detail.code.map_or(String::new(), |n| format!(" ({n})"));
        format!("storage: {phase} · {name}{code}")
    }
}

#[allow(dead_code)] // ARM LS2 activation; also tested without a platform bus.
pub fn activation_failure(stage: &str, code: Option<i32>) -> Detail {
    Detail::new(match stage {
        "register" => Stage::ActivationRegister,
        "attach" => Stage::ActivationAttach,
        "call" => Stage::ActivationCall,
        _ => Stage::ActivationContext,
    }, code)
}

thread_local! {
    static LAST: std::cell::Cell<Option<HelperFailure>> = const { std::cell::Cell::new(None) };
}
pub fn last() -> Option<HelperFailure> { LAST.with(|slot| slot.get()) }
pub fn clear() { LAST.with(|slot| slot.set(None)); }
pub fn remember(stage: Stage, code: Option<i32>) {
    LAST.with(|slot| slot.set(Some(HelperFailure::new(stage, code))));
}
#[allow(dead_code)] // App-side enrichment; helper records a single stage.
pub fn update(f: impl FnOnce(&mut HelperFailure)) {
    LAST.with(|slot| {
        if let Some(mut failure) = slot.get() { f(&mut failure); slot.set(Some(failure)); }
    });
}

/// Legacy helper files have only one of these fixed names. Never forward arbitrary text.
pub fn parse_last_error(bytes: &[u8]) -> Option<Detail> {
    if bytes.len() > 4096 { return None; }
    if let Ok(detail) = serde_json::from_slice::<Detail>(bytes) { return Some(detail); }
    let stage = match bytes {
        b"load_invalid" => Stage::LoadInvalid,
        b"load_unavailable" => Stage::LoadUnavailable,
        b"load_timeout" => Stage::LoadTimeout,
        b"load_authentication" => Stage::LoadAuthentication,
        b"load_protocol" => Stage::LoadProtocol,
        b"load_corrupt" => Stage::LoadCorrupt,
        b"request_size" => Stage::RequestSize,
        b"response_size" => Stage::ResponseSize,
        b"request_digest" => Stage::RequestDigest,
        b"digest_mismatch" => Stage::DigestMismatch,
        b"reconcile_id" => Stage::ReconcileId,
        b"reconcile_load" => Stage::ReconcileLoad,
        b"reconcile_ledger" => Stage::ReconcileLedger,
        b"operation_id" => Stage::OperationId,
        b"load_before_commit" => Stage::LoadBeforeCommit,
        b"ledger" => Stage::Ledger,
        b"expected_db_rev" => Stage::ExpectedDbRev,
        b"expected_epoch" => Stage::ExpectedEpoch,
        b"expected_auth_generation" => Stage::ExpectedAuthGeneration,
        b"prepare" => Stage::Prepare,
        b"apply" => Stage::Apply,
        b"encode" => Stage::Encode,
        b"encode_json" => Stage::EncodeJson,
        b"readback_ledger" => Stage::ReadbackLedger,
        b"open_readback" => Stage::OpenReadback,
        b"commit_response" => Stage::CommitResponse,
        b"plaintext_mismatch" => Stage::PlaintextMismatch,
        _ => return None,
    };
    Some(Detail::new(stage, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_failure_activation_keeps_only_the_stage_and_numeric_code() {
        for (input, stage) in [("register", Stage::ActivationRegister),
            ("attach", Stage::ActivationAttach), ("call", Stage::ActivationCall),
            ("glib-context", Stage::ActivationContext), ("private-fixture", Stage::ActivationContext)] {
            assert_eq!(activation_failure(input, Some(-13)), Detail::new(stage, Some(-13)));
            assert!(!serde_json::to_string(&activation_failure(input, None)).unwrap().contains("private-fixture"));
        }
    }

    #[test]
    fn helper_failure_file_only_accepts_closed_stages_and_numbers() {
        for (stage, code) in [
            (Stage::BusRegister, Some(-1)), (Stage::BusAttach, Some(-2)),
            (Stage::BusCall, Some(-3)), (Stage::BusPayload, None),
            (Stage::BusTimeout, None), (Stage::Db8, Some(-3963)),
        ] {
            let detail = Detail::new(stage, code);
            assert_eq!(parse_last_error(&serde_json::to_vec(&detail).unwrap()), Some(detail));
        }
        assert_eq!(parse_last_error(b"load_unavailable"), Some(Detail::new(Stage::LoadUnavailable, None)));
        assert!(parse_last_error(br#"{"stage":"db8","code":-1,"token":"secret"}"#).is_none());
        assert!(parse_last_error(br#"{"stage":"private-host","code":-1}"#).is_none());
        assert!(parse_last_error(&vec![b'a'; 4097]).is_none());
    }
}
