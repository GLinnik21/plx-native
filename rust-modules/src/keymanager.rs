//! Device-backed protection for the persisted Plex session.
//!
//! TV 24+ documents the public `com.webos.service.keymanager3` service. The version number alone
//! is not a capability test, so we probe the running firmware and its LS2 policy. Older firmware's
//! archival `com.palm.keymanager` AES-CFB interface is deliberately not used: it provides no
//! authenticated-encryption primitive, and ciphertext integrity is part of the storage contract.
//!
//! This module deliberately uses LS2 as an *application* (`LSRegisterApplicationService`) and no
//! root-only broker, filesystem or HAL symbol. A normal SAM-launched app therefore follows the
//! same unprivileged call path on development and retail sets; the retail LS2 entitlement itself
//! is capability-probed at runtime and denial selects the mode-0600 fallback.

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
    use std::ffi::{CStr, CString};
    use std::os::raw::{c_char, c_int, c_void};
    use std::time::{Duration, Instant};

    #[repr(C)]
    struct LSError {
        error_code: c_int,
        message: *mut c_char,
        file: *const c_char,
        line: c_int,
        func: *const c_char,
        padding: *mut c_void,
        magic: libc::c_ulong,
    }

    #[derive(Default)]
    struct Reply {
        payload: Option<String>,
    }

    extern "C" {
        fn LSErrorInit(error: *mut LSError) -> bool;
        fn LSErrorFree(error: *mut LSError);
        fn LSRegisterApplicationService(
            name: *const c_char,
            app_id: *const c_char,
            handle: *mut *mut c_void,
            error: *mut LSError,
        ) -> bool;
        fn LSGmainContextAttach(
            handle: *mut c_void,
            context: *mut c_void,
            error: *mut LSError,
        ) -> bool;
        fn LSCallOneReply(
            handle: *mut c_void,
            uri: *const c_char,
            payload: *const c_char,
            callback: extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> bool,
            context: *mut c_void,
            token: *mut libc::c_ulong,
            error: *mut LSError,
        ) -> bool;
        fn LSCallCancel(handle: *mut c_void, token: libc::c_ulong, error: *mut LSError) -> bool;
        fn LSMessageGetPayload(message: *mut c_void) -> *const c_char;
        fn LSUnregister(handle: *mut c_void, error: *mut LSError) -> bool;
        fn g_main_context_new() -> *mut c_void;
        fn g_main_context_iteration(context: *mut c_void, may_block: c_int) -> c_int;
        fn g_main_context_unref(context: *mut c_void);
    }

    extern "C" fn on_reply(
        _handle: *mut c_void,
        message: *mut c_void,
        context: *mut c_void,
    ) -> bool {
        if context.is_null() || message.is_null() {
            return true;
        }
        let payload = unsafe { LSMessageGetPayload(message) };
        if !payload.is_null() {
            unsafe {
                (*(context as *mut Reply)).payload =
                    Some(CStr::from_ptr(payload).to_string_lossy().into_owned());
            }
        }
        true
    }

    pub(super) struct Client {
        handle: *mut c_void,
        context: *mut c_void,
        error: LSError,
    }

    impl Client {
        pub(super) fn new() -> Result<Self, ()> {
            let app_id = CString::new(crate::paths::app_id()).map_err(|_| ())?;
            let mut error: LSError = unsafe { std::mem::zeroed() };
            unsafe {
                LSErrorInit(&mut error);
            }
            let context = unsafe { g_main_context_new() };
            if context.is_null() {
                unsafe { LSErrorFree(&mut error) };
                return Err(());
            }
            let mut handle = std::ptr::null_mut();
            let registered = unsafe {
                LSRegisterApplicationService(
                    std::ptr::null(),
                    app_id.as_ptr(),
                    &mut handle,
                    &mut error,
                )
            };
            if !registered || handle.is_null() {
                unsafe {
                    LSErrorFree(&mut error);
                    g_main_context_unref(context);
                }
                return Err(());
            }
            reset_error(&mut error);
            if !unsafe { LSGmainContextAttach(handle, context, &mut error) } {
                reset_error(&mut error);
                unsafe {
                    LSUnregister(handle, &mut error);
                    LSErrorFree(&mut error);
                    g_main_context_unref(context);
                }
                return Err(());
            }
            Ok(Self {
                handle,
                context,
                error,
            })
        }

        pub(super) fn call(&mut self, uri: &str, payload: &str) -> Result<String, ()> {
            let uri = CString::new(uri).map_err(|_| ())?;
            let payload = CString::new(payload).map_err(|_| ())?;
            reset_error(&mut self.error);
            let mut reply = Reply::default();
            let mut token = 0;
            let called = unsafe {
                LSCallOneReply(
                    self.handle,
                    uri.as_ptr(),
                    payload.as_ptr(),
                    on_reply,
                    &mut reply as *mut Reply as *mut c_void,
                    &mut token,
                    &mut self.error,
                )
            };
            if called {
                let until = Instant::now() + Duration::from_secs(4);
                while reply.payload.is_none() && Instant::now() < until {
                    unsafe {
                        g_main_context_iteration(self.context, 0);
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            if reply.payload.is_none() && token != 0 {
                reset_error(&mut self.error);
                unsafe {
                    LSCallCancel(self.handle, token, &mut self.error);
                }
            }
            reply.payload.ok_or(())
        }
    }

    impl Drop for Client {
        fn drop(&mut self) {
            reset_error(&mut self.error);
            unsafe {
                LSUnregister(self.handle, &mut self.error);
                LSErrorFree(&mut self.error);
                g_main_context_unref(self.context);
            }
        }
    }

    fn reset_error(error: &mut LSError) {
        unsafe {
            LSErrorFree(error);
            *error = std::mem::zeroed();
            LSErrorInit(error);
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
