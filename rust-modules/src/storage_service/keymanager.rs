//! Owner-bound keymanager3 AES-256-GCM. Never export keys or choose an implicit fallback.
//! ParamSet/handle contract: LG Keymanager3 reference and exact 43.21.71 keymanager3
//! decompilation (keymanager-params/semantics/binding in the DB8 crypto evidence lab).
use super::wire::{
    ErrorCode, KeymanagerFailure, KeymanagerFailureCategory, KeymanagerOperation, KeymanagerStage,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub trait Rpc {
    fn call(&mut self, uri: &str, payload: &Value) -> Result<Value, ErrorCode>;
}
/// Retain only a closed stage and a bounded numeric service code. In particular, never retain
/// an LS2 reply/error string (services can echo keys and input data in their diagnostics).
struct DiagnosticRpc<'a, R> {
    inner: &'a mut R,
    stage: KeymanagerStage,
    service_code: Option<i32>,
    rejected: bool,
    failed: bool,
}
impl<R: Rpc> Rpc for DiagnosticRpc<'_, R> {
    fn call(&mut self, uri: &str, payload: &Value) -> Result<Value, ErrorCode> {
        let stage = if uri.ends_with("/generateKey") {
            Some(KeymanagerStage::Generate)
        } else if uri.ends_with("/begin") {
            Some(KeymanagerStage::Begin)
        } else if uri.ends_with("/finish") {
            Some(KeymanagerStage::Finish)
        } else {
            None
        };
        let result = self.inner.call(uri, payload);
        if let Some(stage) = stage.filter(|_| !self.failed) {
            self.stage = stage;
            if let Ok(reply) = &result {
                self.rejected = reply["returnValue"] == false;
                self.service_code = if self.rejected {
                    reply["errorCode"]
                        .as_i64()
                        .and_then(|n| i32::try_from(n).ok())
                } else {
                    None
                };
            }
            self.failed = result.is_err() || self.rejected;
        }
        result
    }
}
pub fn failure(
    operation: KeymanagerOperation,
    stage: KeymanagerStage,
    code: ErrorCode,
) -> KeymanagerFailure {
    KeymanagerFailure {
        operation,
        stage,
        code,
        category: match code {
            ErrorCode::Unavailable => KeymanagerFailureCategory::Unavailable,
            ErrorCode::Timeout => KeymanagerFailureCategory::Timeout,
            ErrorCode::Corrupt => KeymanagerFailureCategory::InvalidResponse,
            _ => KeymanagerFailureCategory::Other,
        },
        service_code: None,
    }
}
fn diagnose<R: Rpc, T>(
    bus: &mut R,
    operation: KeymanagerOperation,
    action: impl FnOnce(&mut DiagnosticRpc<'_, R>) -> Result<T, ErrorCode>,
) -> Result<T, KeymanagerFailure> {
    let mut traced = DiagnosticRpc {
        inner: bus,
        stage: KeymanagerStage::Validate,
        service_code: None,
        rejected: false,
        failed: false,
    };
    action(&mut traced).map_err(|code| {
        let mut evidence = failure(operation, traced.stage, code);
        evidence.service_code = traced.service_code;
        if traced.rejected {
            evidence.category = KeymanagerFailureCategory::ServiceRejected;
        }
        evidence
    })
}
pub fn seal_detailed(
    bus: &mut impl Rpc,
    name: &str,
    plaintext: &str,
    context: Value,
) -> Result<String, KeymanagerFailure> {
    diagnose(bus, KeymanagerOperation::Seal, |bus| {
        seal(bus, name, plaintext, context)
    })
}
pub fn unseal_detailed(
    bus: &mut impl Rpc,
    name: &str,
    encoded: &str,
    context: &Value,
    operation: KeymanagerOperation,
) -> Result<String, KeymanagerFailure> {
    diagnose(bus, operation, |bus| unseal(bus, name, encoded, context))
}
const URI: &str = "luna://com.webos.service.keymanager3";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub format: String,
    pub key_name: String,
    pub iv: String,
    pub ciphertext: String,
    pub context: Value,
}

fn checked(value: Value) -> Result<Value, ErrorCode> {
    match value["returnValue"].as_bool() {
        Some(true) => Ok(value),
        Some(false) => Err(ErrorCode::Capability),
        None => Err(ErrorCode::Corrupt),
    }
}
fn params(purpose: &[&str]) -> Value {
    json!({"type":"AES","mode":["GCM"],"purpose":purpose,"padding":["None"]})
}
fn generate_params() -> Value {
    let mut value = params(&["encrypt", "decrypt"]);
    value["size"] = json!(256);
    value
}
pub fn seal(
    bus: &mut impl Rpc,
    name: &str,
    plaintext: &str,
    context: Value,
) -> Result<String, ErrorCode> {
    // The caller supplies the freshly generated tenure key name explicitly. A collision is a
    // refusal, never permission to overwrite or use a different owner's key.
    if !valid_key_name(name) {
        return Err(ErrorCode::Invalid);
    }
    checked(bus.call(
        &format!("{URI}/generateKey"),
        &json!({"name":name,"params":generate_params()}),
    )?)?;
    let result = seal_created(bus, name, plaintext, context);
    if result.is_err() {
        // Only this successfully generated, not-yet-persisted generation is eligible.
        let _ = bus.call(&format!("{URI}/removeKey"), &json!({"name":name}));
    }
    result
}
fn seal_created(
    bus: &mut impl Rpc,
    name: &str,
    plaintext: &str,
    context: Value,
) -> Result<String, ErrorCode> {
    let begin = checked(bus.call(
        &format!("{URI}/begin"),
        &json!({"name":name,"params":params(&["encrypt"])}),
    )?)?;
    let handle = begin["handle"]
        .as_str()
        .filter(|h| h.parse::<u64>().is_ok())
        .ok_or(ErrorCode::Corrupt)?;
    let result = (|| {
        let iv = begin["iv"].as_str().ok_or(ErrorCode::Corrupt)?;
        if unbase64(iv)?.is_empty() {
            return Err(ErrorCode::Corrupt);
        }
        let inner = serde_json::to_vec(&json!({"context":context,"payload":plaintext}))
            .map_err(|_| ErrorCode::Invalid)?;
        let result = checked(bus.call(
            &format!("{URI}/finish"),
            &json!({"handle":handle,"data":base64(&inner)}),
        )?)?;
        let ciphertext = result["output"].as_str().ok_or(ErrorCode::Corrupt)?;
        if unbase64(ciphertext)?.len() < 16 {
            return Err(ErrorCode::Corrupt);
        }
        serde_json::to_string(&Envelope {
            format: "keymanager3-aes256-gcm-v1".into(),
            key_name: name.into(),
            iv: iv.into(),
            ciphertext: ciphertext.into(),
            context,
        })
        .map_err(|_| ErrorCode::Invalid)
    })();
    if result.is_err() {
        let _ = bus.call(&format!("{URI}/abort"), &json!({"handle":handle}));
    }
    result
}
pub fn unseal(
    bus: &mut impl Rpc,
    name: &str,
    encoded: &str,
    context: &Value,
) -> Result<String, ErrorCode> {
    let envelope: Envelope = serde_json::from_str(encoded).map_err(|_| ErrorCode::Corrupt)?;
    if !valid_key_name(name)
        || envelope.format != "keymanager3-aes256-gcm-v1"
        || envelope.key_name != name
        || &envelope.context != context
        || unbase64(&envelope.iv)?.is_empty()
        || unbase64(&envelope.ciphertext)?.len() < 16
    {
        return Err(ErrorCode::Corrupt);
    }
    let mut p = params(&["decrypt"]);
    p["iv"] = json!(envelope.iv);
    let begin = checked(bus.call(&format!("{URI}/begin"), &json!({"name":name,"params":p}))?)?;
    let handle = begin["handle"]
        .as_str()
        .filter(|h| h.parse::<u64>().is_ok())
        .ok_or(ErrorCode::Corrupt)?;
    let result = (|| {
        let result = checked(bus.call(
            &format!("{URI}/finish"),
            &json!({"handle":handle,"data":envelope.ciphertext}),
        )?)?;
        let inner: Value = serde_json::from_slice(&unbase64(
            result["output"].as_str().ok_or(ErrorCode::Corrupt)?,
        )?)
        .map_err(|_| ErrorCode::Corrupt)?;
        if inner["context"] != *context {
            return Err(ErrorCode::Corrupt);
        }
        inner["payload"]
            .as_str()
            .map(str::to_owned)
            .ok_or(ErrorCode::Corrupt)
    })();
    if result.is_err() {
        let _ = bus.call(&format!("{URI}/abort"), &json!({"handle":handle}));
    }
    result
}
pub fn available(bus: &mut impl Rpc) -> bool {
    // A read-only begin against a reserved, never generated name proves service dispatch and
    // the closed key-not-found error. No stored key or arbitrary platform value is read.
    match bus.call(
        &format!("{URI}/begin"),
        &json!({"name":"plxnative-capability-probe","params":params(&["encrypt"])}),
    ) {
        Ok(v) if v["returnValue"] == false && v["errorCode"] == -10001 => true,
        Ok(v) if v["returnValue"] == true => {
            if let Some(handle) = v["handle"].as_str() {
                let _ = bus.call(&format!("{URI}/abort"), &json!({"handle":handle}));
            }
            false
        }
        _ => false,
    }
}
fn valid_key_name(name: &str) -> bool {
    name.len() == 24
        && name.starts_with("a.")
        && name[2..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub fn remove_retired(bus: &mut impl Rpc, name: &str) {
    if valid_key_name(name) {
        let _ = bus.call(&format!("{URI}/removeKey"), &json!({"name":name}));
    }
}
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let v = (c[0] as u32) << 16
            | (c.get(1).copied().unwrap_or(0) as u32) << 8
            | c.get(2).copied().unwrap_or(0) as u32;
        out.push(ALPHABET[(v >> 18) as usize] as char);
        out.push(ALPHABET[((v >> 12) & 63) as usize] as char);
        out.push(if c.len() > 1 {
            ALPHABET[((v >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            ALPHABET[(v & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
pub fn unbase64(text: &str) -> Result<Vec<u8>, ErrorCode> {
    if text.len() % 4 != 0 {
        return Err(ErrorCode::Corrupt);
    }
    let mut out = Vec::new();
    for (i, c) in text.as_bytes().chunks(4).enumerate() {
        let mut v = 0u32;
        let mut padding = 0;
        for (j, b) in c.iter().enumerate() {
            let n = match b {
                b'A'..=b'Z' => b - b'A',
                b'a'..=b'z' => b - b'a' + 26,
                b'0'..=b'9' => b - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' if j >= 2 && i + 1 == text.len() / 4 => {
                    padding += 1;
                    0
                }
                _ => return Err(ErrorCode::Corrupt),
            };
            if padding > 0 && *b != b'=' {
                return Err(ErrorCode::Corrupt);
            }
            v = v << 6 | n as u32;
        }
        out.push((v >> 16) as u8);
        if padding < 2 {
            out.push((v >> 8) as u8);
        }
        if padding == 0 {
            out.push(v as u8);
        }
    }
    if base64(&out) != text {
        return Err(ErrorCode::Corrupt);
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_base64_and_invalid_padding() {
        for b in [b"".as_slice(), b"f", b"fo", b"foo", b"foob"] {
            assert_eq!(unbase64(&base64(b)).unwrap(), b);
        }
        for bad in ["Zg=Z", "====", "Zg=", "Zh=="] {
            assert!(unbase64(bad).is_err());
        }
    }
    struct Absent;
    impl Rpc for Absent {
        fn call(&mut self, _: &str, _: &Value) -> Result<Value, ErrorCode> {
            Err(ErrorCode::Unavailable)
        }
    }
    #[test]
    fn absent_service_is_failure_not_plaintext_fallback() {
        assert!(!available(&mut Absent));
        assert!(seal(&mut Absent, "a.AAAAAAAAAAAAAAAAAAAAAA", "secret", json!({})).is_err());
    }
    #[test]
    fn failure_evidence_accepts_only_bounded_numeric_service_codes() {
        for (raw, expected) in [
            (json!(-20030), Some(-20030)),
            (json!(i32::MIN), Some(i32::MIN)),
            (json!(i32::MAX), Some(i32::MAX)),
            (json!(i64::MAX), None),
            (json!(u64::MAX), None),
            (json!("private-fixture"), None),
            (json!(-1.5), None),
        ] {
            let mut rpc = Script {
                replies: vec![json!({"returnValue":false,"errorCode":raw,"errorText":"private-fixture","key_name":"private-fixture"})].into(),
                calls: vec![],
            };
            let evidence = seal_detailed(
                &mut rpc,
                "a.AAAAAAAAAAAAAAAAAAAAAA",
                "private-fixture",
                json!({}),
            )
            .unwrap_err();
            assert_eq!(evidence.service_code, expected);
            assert_eq!(
                evidence.category,
                KeymanagerFailureCategory::ServiceRejected
            );
            assert!(!serde_json::to_string(&evidence)
                .unwrap()
                .contains("private-fixture"));
            assert!(!format!("{evidence:?}").contains("private-fixture"));
        }
    }

    #[test]
    fn malformed_return_value_is_invalid_response_not_service_rejection() {
        for reply in [
            json!({}),
            json!({"returnValue":"false"}),
            json!({"returnValue":null}),
        ] {
            let mut rpc = Script {
                replies: vec![reply].into(),
                calls: vec![],
            };
            let failure = seal_detailed(&mut rpc, "a.AAAAAAAAAAAAAAAAAAAAAA", "fixture", json!({}))
                .unwrap_err();
            assert_eq!(failure.category, KeymanagerFailureCategory::InvalidResponse);
            assert_eq!(failure.code, ErrorCode::Corrupt);
            assert_eq!(failure.stage, KeymanagerStage::Generate);
            assert_eq!(failure.service_code, None);
        }
    }

    struct Script {
        replies: std::collections::VecDeque<Value>,
        calls: Vec<(String, Value)>,
    }
    impl Rpc for Script {
        fn call(&mut self, uri: &str, payload: &Value) -> Result<Value, ErrorCode> {
            self.calls.push((uri.into(), payload.clone()));
            Ok(self
                .replies
                .pop_front()
                .expect("unexpected keymanager call"))
        }
    }
    #[test]
    fn sealing_uses_generation_key_aes256_gcm_and_authenticated_context() {
        let mut rpc = Script {
            replies: vec![
                json!({"returnValue":true}),
                json!({"returnValue":true,"handle":"18446744073709551615","iv":base64(&[0;12])}),
                json!({"returnValue":true,"output":base64(&[7;32])}),
            ]
            .into(),
            calls: vec![],
        };
        let name = "a.AAAAAAAAAAAAAAAAAAAAAA";
        let context = json!({"generation":"fixture","epoch":7});
        let envelope = seal(&mut rpc, name, "secret-fixture", context.clone()).unwrap();
        assert_eq!(rpc.calls[0].1["name"], name);
        assert_eq!(rpc.calls[0].1["params"]["size"], 256);
        assert!(rpc.calls[1].1["params"].get("size").is_none());
        assert_eq!(rpc.calls[0].1["params"]["mode"], json!(["GCM"]));
        assert_eq!(rpc.calls[2].1["handle"], "18446744073709551615");
        assert!(rpc.calls[0].1["params"].get("mac_length").is_none());
        assert!(rpc.calls[2].1.get("aad").is_none());
        let inner: Value =
            serde_json::from_slice(&unbase64(rpc.calls[2].1["data"].as_str().unwrap()).unwrap())
                .unwrap();
        assert_eq!(inner, json!({"context":context,"payload":"secret-fixture"}));
        assert!(!envelope.contains("secret-fixture"));
        let count = rpc.calls.len();
        assert!(unseal(&mut rpc, name, &envelope, &json!({"epoch":8})).is_err());
        assert_eq!(rpc.calls.len(), count);
    }
    #[test]
    fn failed_finish_aborts_the_acquired_handle() {
        let mut rpc = Script {
            replies: vec![
                json!({"returnValue":true}),
                json!({"returnValue":true,"handle":"9","iv":base64(&[0;12])}),
                json!({"returnValue":false,"errorCode":-20030}),
                json!({"returnValue":true}),
                json!({"returnValue":true}),
            ]
            .into(),
            calls: vec![],
        };
        assert!(seal(&mut rpc, "a.AAAAAAAAAAAAAAAAAAAAAA", "secret", json!({})).is_err());
        assert_eq!(
            &rpc.calls[3],
            &(format!("{URI}/abort"), json!({"handle":"9"}))
        );
        assert_eq!(
            rpc.calls.last().unwrap(),
            &(
                format!("{URI}/removeKey"),
                json!({"name":"a.AAAAAAAAAAAAAAAAAAAAAA"})
            )
        );
    }

    #[test]
    fn authenticated_inner_context_must_match_even_when_outer_was_replaced() {
        let name = "a.AAAAAAAAAAAAAAAAAAAAAA";
        let outer = json!({"epoch":2});
        let inner = json!({"context":{"epoch":1},"payload":"secret"});
        let envelope = serde_json::to_string(&Envelope {
            format: "keymanager3-aes256-gcm-v1".into(),
            key_name: name.into(),
            iv: base64(&[1; 16]),
            ciphertext: base64(&[2; 32]),
            context: outer.clone(),
        })
        .unwrap();
        let mut rpc = Script {
            replies: vec![
                json!({"returnValue":true,"handle":"9"}),
                json!({"returnValue":true,"output":base64(&serde_json::to_vec(&inner).unwrap())}),
                json!({"returnValue":true}),
            ]
            .into(),
            calls: vec![],
        };
        assert!(unseal(&mut rpc, name, &envelope, &outer).is_err());
        assert!(rpc.calls[0].1["params"].get("size").is_none());
        assert_eq!(
            rpc.calls[1]
                .1
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["data", "handle"]
        );
        assert!(rpc.calls.iter().all(|(uri, _)| !uri.ends_with("removeKey")));
    }
}
