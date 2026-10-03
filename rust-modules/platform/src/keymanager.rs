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

use plx_base::b64;
use crate::tv::secure::{Backend, Sealed};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU8, Ordering};

const KEY_NAME: &str = "plxnative.session.v1";
const UNKNOWN: u8 = 0;
const MODERN: u8 = 1;
const UNAVAILABLE: u8 = 3;
static SELECTED: AtomicU8 = AtomicU8::new(UNKNOWN);

pub fn seal(plain: &[u8]) -> Option<Sealed> {
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
            plx_base::eventlog::log("session protection: keymanager3");
            return Some(sealed);
        }
    }
    SELECTED.store(UNAVAILABLE, Ordering::Relaxed);
    plx_base::eventlog::log("session protection: no usable key manager; using the 0600 file fallback");
    None
}

pub fn open(sealed: &Sealed) -> Option<Vec<u8>> {
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

pub fn remove(backend: &Backend, key: &str) {
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

#[cfg(any(test, feature = "test-support"))]
pub fn reset_for_test() {
    SELECTED.store(UNKNOWN, Ordering::Relaxed);
    plx_base::eventlog::log("session protection: host tests use the 0600 plaintext fixture");
}

// Synthetic LS2 transport for host persistence tests; never compiled into a device build.
#[cfg(any(test, feature = "test-support"))]
std::thread_local! {
    pub static RPC_FOR_TEST: std::cell::Cell<Option<fn(&str, &str) -> Result<String, ()>>> =
        const { std::cell::Cell::new(None) };
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
    let _block = plx_base::task::assert_may_block(const { &plx_base::task::BlockingLabel::new("keymanager round trip") });
    let mut client = platform::Client::new().ok()?;
    call_with(&mut client, uri, payload)
}

fn call_with(client: &mut platform::Client, uri: &str, payload: &Value) -> Option<Value> {
    let _block = plx_base::task::assert_may_block(const { &plx_base::task::BlockingLabel::new("keymanager round trip") });
    client
        .call(uri, &payload.to_string())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

#[cfg(any(feature = "hostsim", test, feature = "test-support"))]
mod platform {
    pub(super) struct Client;

    impl Client {
        pub(super) fn new() -> Result<Self, ()> {
            #[cfg(any(test, feature = "test-support"))]
            if super::RPC_FOR_TEST.with(|hook| hook.get().is_some()) { return Ok(Self); }
            Err(())
        }

        pub(super) fn call(&mut self, _uri: &str, _payload: &str) -> Result<String, ()> {
            #[cfg(any(test, feature = "test-support"))]
            if let Some(rpc) = super::RPC_FOR_TEST.with(|hook| hook.get()) { return rpc(_uri, _payload); }
            Err(())
        }
    }
}

#[cfg(all(not(feature = "hostsim"), not(any(test, feature = "test-support"))))]
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
                    plx_base::eventlog::log(&format!("keymanager: LS2 {e}"));
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
                    plx_base::eventlog::log(&format!(
                        "keymanager: no reply in {} ms — this client asks nothing more",
                        started.elapsed().as_millis()
                    ));
                    Err(())
                }
                Err(crate::webos::ls2::Fail::Setup { stage, detail, .. }) => {
                    plx_base::eventlog::log(&format!("keymanager: call failed stage={stage} ({detail})"));
                    Err(())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        b64, open, remove, reset_for_test, seal, Backend, Sealed, MODERN, RPC_FOR_TEST, SELECTED,
        UNKNOWN,
    };
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
        let _guard = plx_base::testlock::serial();
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

    /// The protocol round trip, over a synthetic LS2 transport that returns what it was given: it
    /// does not claim to test cryptography or firmware availability, only that `seal` and `open`
    /// walk generateKey, begin and finish in order and agree on the envelope.
    #[test]
    fn a_synthetic_keymanager3_round_trips_through_seal_and_open() {
        let _guard = plx_base::testlock::serial();
        reset_for_test();
        RPC_FOR_TEST.with(|hook| hook.set(Some(|uri, payload| {
            let payload: serde_json::Value = serde_json::from_str(payload).unwrap();
            let response = if uri.ends_with("/generateKey") {
                serde_json::json!({"returnValue":true})
            } else if uri.ends_with("/begin") {
                serde_json::json!({"returnValue":true,"handle":"synthetic","iv":"synthetic-iv"})
            } else if uri.ends_with("/finish") {
                serde_json::json!({"returnValue":true,"output":payload["data"]})
            } else { panic!("unexpected synthetic keymanager operation"); };
            Ok(response.to_string())
        })));
        let sealed = seal(b"secret").expect("the synthetic transport seals");
        assert_eq!(sealed.backend, Backend::Keymanager3);
        assert_eq!(sealed.key, super::KEY_NAME);
        assert_eq!(sealed.iv, "synthetic-iv");
        assert_eq!(open(&sealed).as_deref(), Some(&b"secret"[..]));
        RPC_FOR_TEST.with(|hook| hook.set(None));
        reset_for_test();
    }
}
