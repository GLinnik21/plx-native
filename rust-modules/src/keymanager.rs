//! Device-backed protection for the persisted Plex session.
//!
//! TV 24+ documents the public `com.webos.service.keymanager3` service. The version number alone
//! is not a capability test, so we probe the running firmware and its LS2 policy. Older firmware's
//! archival `com.palm.keymanager` AES-CFB interface is deliberately not used: it provides no
//! authenticated-encryption primitive, and ciphertext integrity is part of the storage contract.
//!
//! This module deliberately uses LS2 as an unprivileged in-app client — the process's one client
//! in `webos::ls2`, a plain anonymous `LSRegister`; the `LSRegisterApplicationService(NULL, app_id)`
//! it used until 2026-09-04 is refused by the hub on the dev set (`-1027 Invalid permissions`), so
//! every probe here failed at registration and never reached a service — and no root-only broker,
//! filesystem or HAL symbol. A normal SAM-launched app therefore follows the same unprivileged call path on
//! development and retail sets; the retail LS2 entitlement itself is capability-probed at runtime
//! and denial selects the mode-0600 fallback.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU8, Ordering};

const KEY_NAME: &str = "plxnative.session.v1";
const UNKNOWN: u8 = 0;
const MODERN: u8 = 1;
const UNAVAILABLE: u8 = 3;
static SELECTED: AtomicU8 = AtomicU8::new(UNKNOWN);

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Backend {
    Keymanager3,
    PalmKeymanager,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct Sealed {
    pub backend: Backend,
    pub key: String,
    pub iv: String,
    pub data: String,
}

pub(crate) fn seal(plain: &[u8]) -> Option<Sealed> {
    match SELECTED.load(Ordering::Relaxed) {
        MODERN => {
            if let Some(sealed) = modern_crypt(plain, None) {
                return Some(sealed);
            }
            SELECTED.store(UNKNOWN, Ordering::Relaxed);
        }
        UNAVAILABLE => return None,
        _ => {}
    }

    if modern_key_ready() {
        if let Some(sealed) = modern_crypt(plain, None) {
            SELECTED.store(MODERN, Ordering::Relaxed);
            crate::log("session protection: keymanager3");
            return Some(sealed);
        }
    }
    SELECTED.store(UNAVAILABLE, Ordering::Relaxed);
    crate::log("session protection: no usable key manager; using the 0600 file fallback");
    None
}

pub(crate) fn open(sealed: &Sealed) -> Option<Vec<u8>> {
    if sealed.key != KEY_NAME {
        return None;
    }
    match sealed.backend {
        Backend::Keymanager3 => modern_crypt(sealed.data.as_bytes(), Some(&sealed.iv)),
        // Kept only so an interim/pre-release envelope deserializes as locked instead of being
        // mistaken for plaintext. AES-CFB does not authenticate the file, so never open it.
        Backend::PalmKeymanager => None,
    }
    .and_then(|s| b64::decode(&s.data))
}

pub(crate) fn remove(backend: &Backend, key: &str) {
    if key != KEY_NAME {
        return;
    }
    let _ = match backend {
        Backend::Keymanager3 => call(
            "luna://com.webos.service.keymanager3/removeKey",
            &json!({"name": key}),
        ),
        Backend::PalmKeymanager => call(
            "luna://com.palm.keymanager/remove",
            &json!({"keyname": key}),
        ),
    };
    // `clear()` deletes the key and the file in one sign-out. A later sign-in in the same process
    // must run key creation again rather than trusting the now-stale backend cache.
    SELECTED.store(UNKNOWN, Ordering::Relaxed);
}

fn succeeded(v: &Value) -> bool {
    v.get("returnValue").and_then(Value::as_bool) == Some(true)
}

fn error_code(v: &Value) -> Option<i64> {
    v.get("errorCode").and_then(Value::as_i64)
}

fn modern_key_ready() -> bool {
    let Some(v) = call(
        "luna://com.webos.service.keymanager3/generateKey",
        &json!({
            "name": KEY_NAME,
            "params": {
                "type": "AES", "size": 256, "mode": ["GCM"],
                "purpose": ["encrypt", "decrypt"], "padding": ["None"]
            }
        }),
    ) else {
        return false;
    };
    succeeded(&v) || error_code(&v) == Some(-10002)
}

fn modern_crypt(input: &[u8], iv: Option<&str>) -> Option<Sealed> {
    // Keymanager3's operation handle belongs to this logical client operation. Keep one LS2
    // registration alive across begin → finish (and abort on failure) instead of assuming a
    // handle survives the caller disconnecting between two one-shot bus calls.
    let mut client = platform::Client::new().ok()?;
    let decrypt = iv.is_some();
    let purpose = if decrypt { "decrypt" } else { "encrypt" };
    let mut params = json!({
        "type": "AES", "mode": ["GCM"], "purpose": [purpose],
        "padding": ["None"], "mac_length": "128"
    });
    if let Some(iv) = iv {
        params["iv"] = Value::String(iv.to_string());
    }
    let begin = call_with(
        &mut client,
        "luna://com.webos.service.keymanager3/begin",
        &json!({"name": KEY_NAME, "params": params}),
    )?;
    if !succeeded(&begin) {
        return None;
    }
    let handle = begin.get("handle")?.as_str()?.to_string();
    let generated_iv = iv
        .map(str::to_string)
        .or_else(|| begin.get("iv")?.as_str().map(str::to_string));
    let Some(generated_iv) = generated_iv else {
        abort_modern(&mut client, &handle);
        return None;
    };
    let data = if decrypt {
        std::str::from_utf8(input).ok()?.to_string()
    } else {
        b64::encode(input)
    };
    let finish = call_with(
        &mut client,
        "luna://com.webos.service.keymanager3/finish",
        &json!({"handle": handle, "data": data}),
    );
    let Some(finish) = finish else {
        abort_modern(&mut client, &handle);
        return None;
    };
    if !succeeded(&finish) {
        abort_modern(&mut client, &handle);
        return None;
    }
    Some(Sealed {
        backend: Backend::Keymanager3,
        key: KEY_NAME.to_string(),
        iv: generated_iv,
        data: finish.get("output")?.as_str()?.to_string(),
    })
}

fn abort_modern(client: &mut platform::Client, handle: &str) {
    let _ = call_with(
        client,
        "luna://com.webos.service.keymanager3/abort",
        &json!({"handle": handle}),
    );
}

fn call(uri: &str, payload: &Value) -> Option<Value> {
    let mut client = platform::Client::new().ok()?;
    call_with(&mut client, uri, payload)
}

fn call_with(client: &mut platform::Client, uri: &str, payload: &Value) -> Option<Value> {
    client
        .call(uri, &payload.to_string())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

#[cfg(any(feature = "hostsim", test))]
mod platform {
    pub(super) struct Client;

    impl Client {
        pub(super) fn new() -> Result<Self, ()> {
            Err(())
        }

        pub(super) fn call(&mut self, _uri: &str, _payload: &str) -> Result<String, ()> {
            Err(())
        }
    }
}

#[cfg(all(not(feature = "hostsim"), not(test)))]
mod platform {
    use std::time::Duration;

    /// Keymanager3's budget. A key generation on a cold set is not a 600 ms affair, and this
    /// client never runs on the press path `webos::ls2::BUDGET` is sized for.
    const BUDGET: Duration = Duration::from_secs(4);

    /// One registration on the bus, kept alive for the length of a logical keymanager operation
    /// (`modern_crypt` needs begin → finish on ONE connection). The registration itself is the
    /// process-wide `webos::ls2` client — the shape it registers with and the reason are there.
    ///
    /// **A service that stalls once is not asked again on this client.** Registration succeeds on
    /// the dev set since 2026-09-04, which makes [`BUDGET`] REACHABLE from a synchronous session
    /// save for the first time, and `modern_crypt`'s begin → (finish | abort) is two calls: a
    /// keymanager3 that hangs on the first would otherwise cost two budgets on a path that holds
    /// the auth and session locks (Codex review, 2026-09-04). A timeout marks the client dead and
    /// every later call on it answers at once; `seal` then records the backend unavailable.
    pub(super) struct Client {
        registration: crate::webos::ls2::Registration,
        dead: bool,
    }

    impl Client {
        pub(super) fn new() -> Result<Self, ()> {
            crate::webos::ls2::register()
                .map(|registration| Self {
                    registration,
                    dead: false,
                })
                .map_err(|e| {
                    crate::log(&format!("keymanager: LS2 {e}"));
                })
        }

        pub(super) fn call(&mut self, uri: &str, payload: &str) -> Result<String, ()> {
            if self.dead {
                return Err(());
            }
            let started = std::time::Instant::now();
            match self.registration.call(uri, payload, BUDGET) {
                Ok(reply) => Ok(reply),
                Err(crate::webos::ls2::Fail::Timeout) => {
                    self.dead = true;
                    crate::log(&format!(
                        "keymanager: no reply in {} ms — this client asks nothing more",
                        started.elapsed().as_millis()
                    ));
                    Err(())
                }
                Err(crate::webos::ls2::Fail::Setup { stage, detail }) => {
                    crate::log(&format!("keymanager: call failed stage={stage} ({detail})"));
                    Err(())
                }
            }
        }
    }
}

mod b64 {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub(super) fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let n = (chunk[0] as u32) << 16
                | (*chunk.get(1).unwrap_or(&0) as u32) << 8
                | *chunk.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[(n >> (18 - i * 6)) as usize & 63] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    pub(super) fn decode(text: &str) -> Option<Vec<u8>> {
        if text.len() % 4 != 0 {
            return None;
        }
        let mut acc = 0u32;
        let mut bits = 0u32;
        let mut out = Vec::with_capacity(text.len() / 4 * 3);
        for ch in text.bytes() {
            if ch == b'=' {
                break;
            }
            let value = ALPHABET.iter().position(|&x| x == ch)? as u32;
            acc = (acc << 6) | value;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::{b64, open, remove, Backend, Sealed, MODERN, SELECTED, UNKNOWN};
    use std::sync::atomic::Ordering;

    #[test]
    fn base64_round_trips_binary_and_padding() {
        for bytes in [
            &b""[..],
            &b"a"[..],
            &b"ab"[..],
            &b"abc"[..],
            &[0, 255, 1, 2],
        ] {
            assert_eq!(b64::decode(&b64::encode(bytes)).as_deref(), Some(bytes));
        }
    }

    #[test]
    fn removing_the_key_invalidates_the_backend_cache() {
        let _guard = crate::testlock::serial();
        SELECTED.store(MODERN, Ordering::Relaxed);
        remove(&Backend::Keymanager3, super::KEY_NAME);
        assert_eq!(SELECTED.load(Ordering::Relaxed), UNKNOWN);
    }

    #[test]
    fn unauthenticated_legacy_ciphertext_is_never_opened() {
        let sealed = Sealed {
            backend: Backend::PalmKeymanager,
            key: super::KEY_NAME.into(),
            iv: "legacy-iv".into(),
            data: b64::encode(b"attacker-controlled ciphertext"),
        };
        assert!(open(&sealed).is_none());
    }
}
