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

use crate::telemetry::storage::StorageStage;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

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

/// The stage and (when the failure reached a service reply) numeric `errorCode` of the most
/// recent seal/open refusal or missing-field reply THIS PROCESS has seen — issue #76's storage
/// telemetry reads this after a `seal`/`open` failure to attach a real stage and code to a handled
/// report, rather than re-deriving one from a log line. `None` on a process that has never seen a
/// service refusal (including a process with no key manager at all, since that path never reaches
/// a service call). Cleared on [`remove`]: a stale refusal from a previous account's key must never
/// be attached to a report about a fresh one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LastRefusal {
    pub stage: StorageStage,
    pub error_code: Option<i64>,
}

static LAST_REFUSAL: Mutex<Option<LastRefusal>> = Mutex::new(None);

pub(crate) fn last_refusal() -> Option<LastRefusal> {
    *LAST_REFUSAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn set_last_refusal(stage: StorageStage, error_code: Option<i64>) {
    *LAST_REFUSAL.lock().unwrap_or_else(|e| e.into_inner()) = Some(LastRefusal { stage, error_code });
}

/// Map a `log_refusal`/`log_missing_field` call site's own `method` label to the telemetry stage
/// vocabulary. `None` for a method this module does not (yet) classify — today that is nothing,
/// since every caller passes one of these five labels.
fn stage_for_method(method: &str) -> Option<StorageStage> {
    match method {
        "generateKey" => Some(StorageStage::GenerateKey),
        "begin(encrypt)" => Some(StorageStage::BeginEncrypt),
        "begin(decrypt)" => Some(StorageStage::BeginDecrypt),
        "finish(encrypt)" => Some(StorageStage::FinishEncrypt),
        "finish(decrypt)" => Some(StorageStage::FinishDecrypt),
        _ => None,
    }
}

/// Record a call that got NO reply to classify: the client is dead (its budget ran out —
/// `NoReply`) or the call never got that far (a failed registration, or an LS2 setup failure —
/// `Unreachable`). Published to the global exactly like a refusal, and to `local` for the caller
/// that decides something on it (`open_checked`'s marker gate). Issue #76, second review: the
/// first review treated these as "no evidence", and the trace under a stalled service showed the
/// cost — no marker, so every launch re-pays the budget, and no report, so nothing ever says so.
fn note_unanswered(client: Option<&platform::Client>, local: &mut Option<LastRefusal>) {
    let stage = if client.is_some_and(platform::Client::is_dead) {
        StorageStage::NoReply
    } else {
        StorageStage::Unreachable
    };
    set_last_refusal(stage, None);
    *local = Some(LastRefusal {
        stage,
        error_code: None,
    });
}

pub(crate) fn seal(plain: &[u8]) -> Option<Sealed> {
    match SELECTED.load(Ordering::Relaxed) {
        // Trust the fast path rather than re-verifying every save: the round trip already ran
        // once, at promotion below, and a `seal` that pays a second LS2 registration plus two
        // more budgeted calls on every credential write is a cost this path (the SDL main thread,
        // under the auth and session locks) cannot absorb for free (Codex review 2026-09-04;
        // issue #76 review). It is NOT what catches a backend whose key is unusable from a
        // DIFFERENT launch or registration than the one that sealed it — `plex::session`'s
        // `LOCKED_STATE` does, from the read side, which is the only side that can see a launch
        // boundary at all.
        MODERN => {
            if let Some(sealed) = modern_crypt(plain, None, &mut None) {
                return Some(sealed);
            }
            SELECTED.store(UNKNOWN, Ordering::Relaxed);
        }
        UNAVAILABLE => return None,
        _ => {}
    }

    if modern_key_ready() {
        if let Some(sealed) = modern_crypt(plain, None, &mut None) {
            if round_trips(&sealed, plain) {
                SELECTED.store(MODERN, Ordering::Relaxed);
                log("session protection: keymanager3");
                return Some(sealed);
            }
            SELECTED.store(UNAVAILABLE, Ordering::Relaxed);
            return None;
        }
    }
    SELECTED.store(UNAVAILABLE, Ordering::Relaxed);
    log("session protection: no usable key manager; using the 0600 file fallback");
    None
}

/// Prove a just-sealed envelope actually opens before `seal` hands it back to be persisted.
/// Mirrors what [`open`] will do later — same call shape, new registration — because a backend
/// that answers `encrypt` but not `decrypt` (or answers with a shape [`open`] cannot parse) is
/// exactly the failure this exists to catch before it reaches disk.
fn round_trips(sealed: &Sealed, plain: &[u8]) -> bool {
    match open(sealed) {
        Some(bytes) if bytes == plain => return true,
        // `open` returned bytes, but not the ones this call just sealed — a genuine mismatch, as
        // opposed to a service refusal `open` (via `log_refusal`/`log_missing_field`) has already
        // recorded a more specific stage and code for.
        Some(_) => set_last_refusal(StorageStage::RoundtripMismatch, None),
        None => {}
    }
    log(
        "session protection: keymanager3 sealed but could not open its own envelope — using the 0600 file fallback",
    );
    false
}

pub(crate) fn open(sealed: &Sealed) -> Option<Vec<u8>> {
    open_checked(sealed).0
}

/// [`open`], plus — alongside the result — whatever refusal or missing-field reply THIS SPECIFIC
/// call saw, straight from the call itself rather than re-read from the [`LAST_REFUSAL`] global
/// afterward.
///
/// **Issue #76 review: `LAST_REFUSAL` is a process-global**, written by `seal`, `open` and
/// `round_trips` from however many threads call them, and read long after the fact by
/// `save_locked`'s seal-failure report. A caller that needs to know "did THIS attempt specifically
/// see a refusal" — `plex::session::read_locked`'s cross-launch marker gate, which must never
/// treat a bare timeout/registration failure (no service reached at all) as the proven refusal it
/// requires — cannot get that answer safely from the shared global: another thread's unrelated
/// `open`/`seal` call can write over it between this call returning and the global being read
/// (measured: an early version of this fix cleared `LAST_REFUSAL` at `open`'s own entry to scope
/// it, and that turned every `open` call anywhere in the process into a writer of the shared
/// global, which then intermittently raced a `testlock::serial()`-held `keymanager.rs` unit test
/// that never touches this file at all — six-of-six clean once the entry clear was replaced by
/// this call-local return value instead). This function is therefore the one to reach for when the
/// answer decides something (a marker write); the bare [`open`]/[`last_refusal`] pair remains for
/// callers that only want the STANDING fact (a report's error-code enrichment, a preview).
pub(crate) fn open_checked(sealed: &Sealed) -> (Option<Vec<u8>>, Option<LastRefusal>) {
    if sealed.key != KEY_NAME {
        return (None, None);
    }
    let mut local = None;
    let plain = match sealed.backend {
        Backend::Keymanager3 => modern_crypt(sealed.data.as_bytes(), Some(&sealed.iv), &mut local),
        // Kept only so an interim/pre-release envelope deserializes as locked instead of being
        // mistaken for plaintext. AES-CFB does not authenticate the file, so never open it.
        Backend::PalmKeymanager => None,
    }
    .and_then(|s| b64::decode(&s.data));
    (plain, local)
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
    // A stale refusal from the account that just signed out must never be attached to a report
    // about the one that signs in next.
    *LAST_REFUSAL.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn succeeded(v: &Value) -> bool {
    v.get("returnValue").and_then(Value::as_bool) == Some(true)
}

fn error_code(v: &Value) -> Option<i64> {
    v.get("errorCode").and_then(Value::as_i64)
}

fn error_text(v: &Value) -> String {
    v.get("errorText")
        .and_then(Value::as_str)
        .unwrap_or("no errorText")
        .to_string()
}

/// Route through here rather than `crate::log` directly so a test can capture what this module
/// says without a real event-log file — see the `tests` module below.
#[cfg(not(test))]
fn log(m: &str) {
    crate::log(m);
}
#[cfg(test)]
fn log(m: &str) {
    tests::capture(m);
}

/// Which `(method, errorCode)` refusals and `(method, field)` missing-field replies this PROCESS
/// has already logged — a `Mutex`, not a `thread_local!`, because `seal`/`open` run on more than
/// one thread (the SDL main thread via a synchronous session save, and each auth worker
/// `task::spawn_small` starts fresh for a sign-in or roster refresh) and a per-thread cache would
/// dedupe only within one thread, re-logging the same refusal once per worker. A television's
/// keymanager3 answers the same shape call after call (a token is re-sealed on every profile
/// switch), so without a process-wide cache a bad or absent service would fill the primary event
/// log with the same line every few seconds.
static LOGGED_ONCE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn logged_once() -> &'static Mutex<HashSet<String>> {
    LOGGED_ONCE.get_or_init(|| Mutex::new(HashSet::new()))
}

fn log_once(key: String, message: String) {
    let first = logged_once()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key);
    if first {
        log(&message);
    }
}

/// Log a service-refused reply exactly once per distinct `(method, errorCode)` — never the
/// request or reply body, since that is where `data`/`iv`/`handle`/`output` live.
///
/// `errorText` is an arbitrary string the SERVICE chose, not one this module controls, and three of
/// the calls that can refuse (`finish(encrypt)`/`finish(decrypt)`/`begin(decrypt)`) carry sensitive
/// bytes in their own REQUEST — the base64 session plaintext/ciphertext on the two `finish` calls,
/// the GCM IV on `begin(decrypt)` — a service that echoes any of its input back in an error string
/// must never be able to turn this refusal line into a leak. Both `finish` call sites and
/// `begin(decrypt)`'s own call site pass `""` here unconditionally rather than relying on
/// `sanitize_error_text` alone: its base64-run screen (24 characters) is well past a 16-character
/// base64 IV, so it cannot see that one on its own.
fn log_refusal(method: &str, code: i64, text: &str) {
    if let Some(stage) = stage_for_method(method) {
        set_last_refusal(stage, Some(code));
    }
    let safe = sanitize_error_text(text);
    log_once(
        format!("refusal:{method}:{code}"),
        if safe.is_empty() {
            format!("keymanager: {method} refused errorCode={code}")
        } else {
            format!("keymanager: {method} refused errorCode={code} ({safe})")
        },
    );
}

/// Bound `errorText` to a short human sentence and refuse anything shaped like the secret it must
/// never carry. keymanager3's documented errors ("key not found", "iv not set", …) are a handful
/// of words; nothing legitimate needs more than [`MAX_ERROR_TEXT`] characters or a long run of
/// base64 alphabet.
fn sanitize_error_text(text: &str) -> String {
    const MAX_ERROR_TEXT: usize = 64;
    if has_long_base64_run(text) {
        return String::new();
    }
    text.chars().take(MAX_ERROR_TEXT).collect()
}

/// True if `text` contains an unbroken run of base64-alphabet characters long enough to be a
/// fragment of encoded session bytes rather than a word in a human sentence.
fn has_long_base64_run(text: &str) -> bool {
    const RUN: usize = 24; // ~18 decoded bytes — already past any plausible error word
    let mut run = 0usize;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=' {
            run += 1;
            if run >= RUN {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// Log a reply that came back `returnValue:true` but missing the one field the caller needed —
/// a shape keymanager3's own reference documents but this module has never observed on real
/// hardware (the dev set predates the service entirely).
fn log_missing_field(method: &str, field: &str) {
    if let Some(stage) = stage_for_method(method) {
        set_last_refusal(stage, None);
    }
    log_once(
        format!("missing:{method}:{field}"),
        format!("keymanager: {method} reply had no {field}"),
    );
}

fn modern_key_ready() -> bool {
    let Ok(mut client) = platform::Client::new() else {
        note_unanswered(None, &mut None);
        return false;
    };
    let Some(v) = call_with(
        &mut client,
        "luna://com.webos.service.keymanager3/generateKey",
        &json!({
            "name": KEY_NAME,
            "params": {
                "type": "AES", "size": 256, "mode": ["GCM"],
                "purpose": ["encrypt", "decrypt"], "padding": ["None"]
            }
        }),
    ) else {
        note_unanswered(Some(&client), &mut None);
        return false;
    };
    if succeeded(&v) {
        return true;
    }
    match error_code(&v) {
        Some(-10002) => true, // "key already exists" — an earlier session already created it
        // The LS2 HUB answering for a service that is not on this firmware — every set before
        // webOS 24, the dev set included. Measured on the webOS 4.10 dev set (2026-09-10,
        // `luna-send` as root): `errorCode:-1`, `"Service does not exist: com.webos.service.
        // keymanager3."`, in under 2 ms. That is the ABSENCE of a service, not a refusal by one,
        // so it publishes no `last_refusal` (the seal-failure report would otherwise fire from
        // every such install's first save) and is logged as what it is, once.
        Some(-1) if service_absent(&error_text(&v)) => {
            log_once(
                "absent".to_string(),
                "keymanager: keymanager3 is not on this firmware; using the 0600 file".to_string(),
            );
            false
        }
        Some(code) => {
            log_refusal("generateKey", code, &error_text(&v));
            false
        }
        None => false,
    }
}

/// The hub's own wording for a service nobody registered — `ls-hubd`'s reply, not the service's,
/// which is why it is matched by text: `-1` alone is also what a REAL keymanager3 uses for an
/// unknown method (measured against the legacy `com.webos.service.keymanager` the same day:
/// `-1`, `Unknown method "generateKey" for category "/"`).
fn service_absent(error_text: &str) -> bool {
    error_text.starts_with("Service does not exist")
}

/// `local` is filled in alongside the global `LAST_REFUSAL` at every `log_refusal`/
/// `log_missing_field` call below, so a caller that needs to know THIS call's own outcome (rather
/// than the shared global's, which another thread can write over) has a race-free answer —
/// `open_checked`'s doc has the reasoning. Pass `&mut None` when the caller only wants `LAST_REFUSAL`
/// updated as a side effect and does not need the value back (both `seal` call sites).
fn modern_crypt(
    input: &[u8],
    iv: Option<&str>,
    local: &mut Option<LastRefusal>,
) -> Option<Sealed> {
    // Keymanager3's operation handle belongs to this logical client operation. Keep one LS2
    // registration alive across begin → finish (and abort on failure) instead of assuming a
    // handle survives the caller disconnecting between two one-shot bus calls.
    let Ok(mut client) = platform::Client::new() else {
        note_unanswered(None, local);
        return None;
    };
    let decrypt = iv.is_some();
    let purpose = if decrypt { "decrypt" } else { "encrypt" };
    let begin_method = if decrypt { "begin(decrypt)" } else { "begin(encrypt)" };
    let finish_method = if decrypt { "finish(decrypt)" } else { "finish(encrypt)" };
    // Mirrors `stage_for_method`'s own mapping so `local` always agrees with what `log_refusal`/
    // `log_missing_field` just published to the global — computed once here rather than at each
    // of the four call sites below.
    let begin_stage = stage_for_method(begin_method);
    let finish_stage = stage_for_method(finish_method);
    // Every field here is in keymanager3's own published ParamSet
    // (webostv.developer.lge.com/develop/references/keymanager3): `type`, `mode`, `purpose` and
    // `padding` on generateKey/begin, plus `iv` on a decrypt begin. There is no `mac_length`
    // field documented anywhere in that ParamSet — the default MAC length applies to both
    // directions of a GCM operation, and the app used to send one anyway.
    let mut params = json!({
        "type": "AES", "mode": ["GCM"], "purpose": [purpose], "padding": ["None"]
    });
    if let Some(iv) = iv {
        params["iv"] = Value::String(iv.to_string());
    }
    let Some(begin) = call_with(
        &mut client,
        "luna://com.webos.service.keymanager3/begin",
        &json!({"name": KEY_NAME, "params": params}),
    ) else {
        note_unanswered(Some(&client), local);
        return None;
    };
    if !succeeded(&begin) {
        if let Some(code) = error_code(&begin) {
            // `begin(decrypt)`'s own REQUEST carries the IV (`params.iv`, set above) — a service
            // that echoed it back in `errorText` would need only sanitize_error_text's 24-char
            // base64-run screen to fail (a 12-byte GCM IV is 16 base64 characters), so this call's
            // text is dropped unconditionally, exactly like both `finish` calls already are.
            // `begin(encrypt)`'s request carries no such field.
            let text = if decrypt { "" } else { &error_text(&begin) };
            log_refusal(begin_method, code, text);
            *local = begin_stage.map(|stage| LastRefusal { stage, error_code: Some(code) });
        }
        return None;
    }
    let Some(handle) = begin.get("handle").and_then(Value::as_str).map(str::to_string) else {
        log_missing_field(begin_method, "handle");
        *local = begin_stage.map(|stage| LastRefusal { stage, error_code: None });
        return None;
    };
    let generated_iv = iv
        .map(str::to_string)
        .or_else(|| begin.get("iv").and_then(Value::as_str).map(str::to_string));
    let Some(generated_iv) = generated_iv else {
        log_missing_field(begin_method, "iv");
        *local = begin_stage.map(|stage| LastRefusal { stage, error_code: None });
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
        note_unanswered(Some(&client), local);
        abort_modern(&mut client, &handle);
        return None;
    };
    if !succeeded(&finish) {
        if let Some(code) = error_code(&finish) {
            // `finish`'s own REQUEST is `{"handle": handle, "data": data}` — the base64 session
            // plaintext or ciphertext. `errorCode` alone (-20030 verification failed, -20052 iv
            // not set, -10001 key not found, …) is the whole diagnostic; `errorText` on this call
            // is never logged at all, `sanitize_error_text`'s bound notwithstanding — the request
            // it is answering is exactly the shape a payload-echoing reply would leak.
            log_refusal(finish_method, code, "");
            *local = finish_stage.map(|stage| LastRefusal { stage, error_code: Some(code) });
        }
        abort_modern(&mut client, &handle);
        return None;
    }
    let Some(output) = finish.get("output").and_then(Value::as_str).map(str::to_string) else {
        log_missing_field(finish_method, "output");
        *local = finish_stage.map(|stage| LastRefusal { stage, error_code: None });
        abort_modern(&mut client, &handle);
        return None;
    };
    Some(Sealed {
        backend: Backend::Keymanager3,
        key: KEY_NAME.to_string(),
        iv: generated_iv,
        data: output,
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

/// Test-only bridge into the private scripted backend, for another module's tests
/// (`plex::session`'s issue #76 coverage) that need a keymanager3 double without duplicating one.
/// `mod platform` below is not `pub`, so its `pub(crate)` script hooks are otherwise unreachable
/// outside this file — Rust visibility follows the whole path, not just the leaf item. Also resets
/// the cached backend selection and the log-dedup cache, the same clean slate `keymanager`'s own
/// tests share, so a caller does not have to know those exist to get a predictable `seal`/`open`.
#[cfg(test)]
pub(crate) fn arm_for_test(entries: Vec<(&'static str, Result<Value, ()>)>) {
    SELECTED.store(UNKNOWN, Ordering::Relaxed);
    logged_once().lock().unwrap_or_else(|e| e.into_inner()).clear();
    platform::script_for_test(entries);
    *LAST_REFUSAL.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Test-only: undo [`arm_for_test`] — `Client::new` goes back to refusing (the default every
/// non-scripted test relies on), and the backend/log caches are reset again so a later scripted or
/// unscripted call in the same process starts clean.
#[cfg(test)]
pub(crate) fn disarm_for_test() {
    SELECTED.store(UNKNOWN, Ordering::Relaxed);
    logged_once().lock().unwrap_or_else(|e| e.into_inner()).clear();
    platform::reset_for_test();
    *LAST_REFUSAL.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Test-only bridge for the same reason [`arm_for_test`] is one: `plex::session`'s issue #76
/// marker coverage needs to assert that a save gated on the persisted refused-storage marker never
/// even reaches the scripted backend — a call list that is not merely `Err`-free but genuinely
/// EMPTY is the only thing that tells "never asked" apart from "asked and refused".
#[cfg(test)]
pub(crate) fn calls_for_test() -> Vec<(String, String)> {
    platform::calls_for_test()
}

#[cfg(any(feature = "hostsim", test))]
mod platform {
    /// `dead` mirrors the real client's: a scripted `Err(())` reply IS the timeout shape.
    pub(super) struct Client {
        dead: bool,
    }

    /// Test-only scripted backend. There is no keymanager3 anywhere off-device — the dev TV
    /// predates the service and the simulator has no LS2 bus at all — so `Client` answers
    /// `Err(())` unconditionally UNLESS a test has armed a script, in which case it plays that
    /// script back instead. A hostsim (non-test) build never arms one, so its behaviour is
    /// unchanged: no keymanager3, same as always.
    #[cfg(test)]
    mod script {
        use serde_json::Value;
        use std::cell::RefCell;
        use std::collections::{HashMap, VecDeque};

        thread_local! {
            /// Queued replies per LUNA method (the URI's last path segment: `generateKey`,
            /// `begin`, `finish`, `abort`, `removeKey`), consumed FIFO — so a test can queue a
            /// `begin` success for the encrypt half of an operation and a `begin` refusal for the
            /// decrypt half, in the order those calls actually happen.
            static REPLIES: RefCell<HashMap<&'static str, VecDeque<Result<Value, ()>>>> =
                RefCell::new(HashMap::new());
            /// Every `(uri, payload)` this client sent, in order — lets a test assert on the
            /// REQUEST shape, not only the reply (e.g. that `begin` no longer sends `mac_length`).
            static CALLS: RefCell<Vec<(String, String)>> = RefCell::new(Vec::new());
        }

        pub(super) fn armed() -> bool {
            REPLIES.with(|r| !r.borrow().is_empty())
        }

        pub(super) fn set(entries: Vec<(&'static str, Result<Value, ()>)>) {
            REPLIES.with(|r| {
                let mut r = r.borrow_mut();
                r.clear();
                for (method, reply) in entries {
                    r.entry(method).or_default().push_back(reply);
                }
            });
            CALLS.with(|c| c.borrow_mut().clear());
        }

        pub(super) fn reset() {
            REPLIES.with(|r| r.borrow_mut().clear());
            CALLS.with(|c| c.borrow_mut().clear());
        }

        pub(super) fn record_call(uri: &str, payload: &str) {
            CALLS.with(|c| c.borrow_mut().push((uri.to_string(), payload.to_string())));
        }

        pub(super) fn calls() -> Vec<(String, String)> {
            CALLS.with(|c| c.borrow().clone())
        }

        pub(super) fn next_reply(uri: &str) -> Result<Value, ()> {
            let method = uri.rsplit('/').next().unwrap_or("");
            REPLIES.with(|r| {
                r.borrow_mut()
                    .get_mut(method)
                    .and_then(VecDeque::pop_front)
                    .unwrap_or(Err(()))
            })
        }
    }

    /// Test-only: script the reply keymanager3 gives to each LUNA method. Replaces any previous
    /// script and clears the recorded call log.
    #[cfg(test)]
    pub(crate) fn script_for_test(entries: Vec<(&'static str, Result<serde_json::Value, ()>)>) {
        script::set(entries);
    }

    /// Test-only: clear the script — `Client::new` goes back to refusing, the default every
    /// non-scripted test relies on.
    #[cfg(test)]
    pub(crate) fn reset_for_test() {
        script::reset();
    }

    /// Test-only: every request this client sent, in call order.
    #[cfg(test)]
    pub(crate) fn calls_for_test() -> Vec<(String, String)> {
        script::calls()
    }

    impl Client {
        pub(super) fn new() -> Result<Self, ()> {
            #[cfg(test)]
            if script::armed() {
                return Ok(Self { dead: false });
            }
            Err(())
        }

        pub(super) fn is_dead(&self) -> bool {
            self.dead
        }

        #[allow(unused_variables)]
        pub(super) fn call(&mut self, uri: &str, payload: &str) -> Result<String, ()> {
            #[cfg(test)]
            {
                if self.dead {
                    return Err(());
                }
                script::record_call(uri, payload);
                let reply = script::next_reply(uri).map(|v| v.to_string());
                if reply.is_err() {
                    self.dead = true;
                }
                return reply;
            }
            #[cfg(not(test))]
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

        pub(super) fn is_dead(&self) -> bool {
            self.dead
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
    use super::{
        b64, open, platform, remove, seal, Backend, Sealed, StorageStage, MODERN, SELECTED,
        UNAVAILABLE, UNKNOWN,
    };
    use serde_json::json;
    use std::cell::RefCell;
    use std::sync::atomic::Ordering;

    thread_local! {
        static CAPTURED: RefCell<Vec<String>> = RefCell::new(Vec::new());
    }

    /// Called by [`super::log`] instead of writing a real event-log file. A brand-new test
    /// thread's thread-local storage starts empty, but every test below resets it explicitly —
    /// the fixture is what a scripted round trip actually asserts on, not an assumption about the
    /// test harness's thread reuse policy.
    pub(super) fn capture(m: &str) {
        CAPTURED.with(|c| c.borrow_mut().push(m.to_string()));
    }

    fn captured() -> Vec<String> {
        CAPTURED.with(|c| c.borrow().clone())
    }

    /// Shared setup for every scripted-backend test: a clean slate for the backend cache, the
    /// log-dedup cache, the captured lines and the scripted client — held under `testlock::serial`
    /// because `SELECTED` and the log dedup cache are process globals other keymanager tests also
    /// touch.
    fn reset() {
        SELECTED.store(UNKNOWN, Ordering::Relaxed);
        super::logged_once().lock().unwrap_or_else(|e| e.into_inner()).clear();
        CAPTURED.with(|c| c.borrow_mut().clear());
        platform::reset_for_test();
        *super::LAST_REFUSAL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

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

    fn generate_key_ok() -> (&'static str, Result<serde_json::Value, ()>) {
        ("generateKey", Ok(json!({"returnValue": true})))
    }

    /// (a) A backend that seals fine but cannot open what it just sealed must not be trusted —
    /// `seal` returns `None`, the backend is marked unavailable, and the refusal is logged once.
    #[test]
    fn seal_refuses_a_backend_that_cannot_open_its_own_envelope() {
        let _guard = crate::testlock::serial();
        reset();
        let plain = b"issue-76 plaintext";
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc", "iv": b64::encode(b"0123456789ab")})),
            ),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(b"ciphertext-placeholder")})),
            ),
            (
                "begin",
                Ok(json!({
                    "returnValue": false,
                    "errorCode": -10001,
                    "errorText": "key not found"
                })),
            ),
        ]);

        let result = seal(plain);

        assert!(result.is_none(), "a backend that cannot open its own envelope must not be trusted");
        assert_eq!(SELECTED.load(Ordering::Relaxed), UNAVAILABLE);
        let lines = captured();
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.contains("begin(decrypt) refused errorCode=-10001"))
                .count(),
            1,
            "expected exactly one refusal line, got: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("could not open its own envelope")),
            "expected the round-trip failure line, got: {lines:?}"
        );
        platform::reset_for_test();
    }

    /// (b) A backend that genuinely round-trips is trusted: `seal` returns `Some`, and `open`
    /// recovers the original bytes from that envelope.
    #[test]
    fn seal_trusts_a_backend_that_round_trips() {
        let _guard = crate::testlock::serial();
        reset();
        let plain = b"issue-76 plaintext";
        let iv = b64::encode(b"0123456789ab");
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc", "iv": iv})),
            ),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(b"ciphertext-placeholder")})),
            ),
            ("begin", Ok(json!({"returnValue": true, "handle": "h-dec"}))),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(plain)})),
            ),
        ]);

        let sealed = seal(plain).expect("a round-tripping backend must be trusted");
        assert_eq!(SELECTED.load(Ordering::Relaxed), MODERN);
        assert_eq!(sealed.backend, Backend::Keymanager3);

        // `open` mints its own fresh registration, exactly like a later boot would.
        platform::script_for_test(vec![(
            "begin",
            Ok(json!({"returnValue": true, "handle": "h-dec2"})),
        ), (
            "finish",
            Ok(json!({"returnValue": true, "output": b64::encode(plain)})),
        )]);
        assert_eq!(open(&sealed).as_deref(), Some(&plain[..]));
        platform::reset_for_test();
    }

    /// (c) A `finish` reply that succeeded but carries no `output` is a missing-field failure,
    /// not a silent `None` — it must be logged, and `seal` must still refuse cleanly.
    #[test]
    fn seal_logs_a_finish_reply_with_no_output() {
        let _guard = crate::testlock::serial();
        reset();
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc", "iv": b64::encode(b"0123456789ab")})),
            ),
            ("finish", Ok(json!({"returnValue": true}))),
        ]);

        assert!(seal(b"issue-76 plaintext").is_none());
        assert_eq!(SELECTED.load(Ordering::Relaxed), UNAVAILABLE);
        let lines = captured();
        assert!(
            lines
                .iter()
                .any(|l| l == "keymanager: finish(encrypt) reply had no output"),
            "expected the missing-field line, got: {lines:?}"
        );
        platform::reset_for_test();
    }

    /// (c2) Issue #76 review: `finish`'s own request carries the base64 session plaintext or
    /// ciphertext, so a service that echoes any of its input back in `errorText` must never see
    /// that string reach the log — not truncated, not partially, not at all. Only `errorCode` may
    /// appear.
    #[test]
    fn finish_refusal_never_logs_errortext_even_if_the_service_echoes_the_request() {
        let _guard = crate::testlock::serial();
        reset();
        let leaked_secret = b64::encode(b"X-Plex-Token=super-secret-account-token-value");
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc", "iv": b64::encode(b"0123456789ab")})),
            ),
            (
                "finish",
                Ok(json!({
                    "returnValue": false,
                    "errorCode": -20030,
                    "errorText": format!("verification failed for data={leaked_secret}")
                })),
            ),
        ]);

        assert!(seal(b"issue-76 plaintext").is_none());
        let lines = captured();
        assert!(
            lines.iter().any(|l| l == "keymanager: finish(encrypt) refused errorCode=-20030"),
            "expected a code-only refusal line, got: {lines:?}"
        );
        assert!(
            lines.iter().all(|l| !l.contains(&leaked_secret)),
            "the service's echoed payload must never reach the log: {lines:?}"
        );
        platform::reset_for_test();
    }

    /// (c3) `begin`'s request carries no secret, so its `errorText` may still be logged — but a
    /// reply shaped like it is carrying one (a long base64-alphabet run) is dropped outright, and
    /// an ordinary one is bounded rather than trusted to stay short forever.
    #[test]
    fn begin_refusal_drops_a_long_base64_looking_errortext_but_keeps_an_ordinary_one() {
        let _guard = crate::testlock::serial();
        reset();
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({
                    "returnValue": false,
                    "errorCode": -10001,
                    "errorText": "key not found"
                })),
            ),
        ]);
        assert!(seal(b"issue-76 plaintext").is_none());
        assert!(
            captured()
                .iter()
                .any(|l| l == "keymanager: begin(encrypt) refused errorCode=-10001 (key not found)"),
            "an ordinary short errorText is kept: {:?}",
            captured()
        );
        platform::reset_for_test();

        reset();
        let base64_shaped = b64::encode(b"this looks exactly like an encoded secret payload");
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({
                    "returnValue": false,
                    "errorCode": -10001,
                    "errorText": base64_shaped
                })),
            ),
        ]);
        assert!(seal(b"issue-76 plaintext").is_none());
        let lines = captured();
        assert!(
            lines.iter().any(|l| l == "keymanager: begin(encrypt) refused errorCode=-10001"),
            "a base64-shaped errorText is dropped to code-only: {lines:?}"
        );
        assert!(
            lines.iter().all(|l| !l.contains(&base64_shaped)),
            "must never log the base64-shaped text: {lines:?}"
        );
        platform::reset_for_test();
    }

    /// (f) Issue #76 review: once a backend is trusted (`SELECTED == MODERN`), `seal` must not pay
    /// a second decrypt round trip on every later save — that doubles keymanager3's LS2 budget on
    /// a path that holds the auth and session locks. Script only ONE decrypt reply (consumed by
    /// the promotion's `round_trips` check) and two encrypts; if the second `seal` attempted to
    /// re-verify, the exhausted decrypt queue would make `open` fail and `seal` return `None`.
    #[test]
    fn seal_does_not_re_verify_on_the_cached_modern_fast_path() {
        let _guard = crate::testlock::serial();
        reset();
        let plain = b"issue-76 plaintext";
        let iv = b64::encode(b"0123456789ab");
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc1", "iv": iv.clone()})),
            ),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(b"ciphertext-1")})),
            ),
            // The promotion's own round-trip verify — the only decrypt this test scripts.
            ("begin", Ok(json!({"returnValue": true, "handle": "h-dec"}))),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(plain)})),
            ),
            // A second encrypt for the fast-path call below. No second decrypt is scripted.
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc2", "iv": iv})),
            ),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(b"ciphertext-2")})),
            ),
        ]);

        assert!(seal(plain).is_some(), "the promotion round trip succeeds");
        assert_eq!(SELECTED.load(Ordering::Relaxed), MODERN);

        let second = seal(plain);
        assert!(
            second.is_some(),
            "the fast path must not fail merely because no second decrypt was scripted"
        );
        assert_eq!(second.unwrap().data, b64::encode(b"ciphertext-2"));
        platform::reset_for_test();
    }

    /// (d) `begin`'s request never carries the undocumented `mac_length` field.
    #[test]
    fn begin_request_carries_no_mac_length() {
        let _guard = crate::testlock::serial();
        reset();
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc", "iv": b64::encode(b"0123456789ab")})),
            ),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(b"ciphertext-placeholder")})),
            ),
        ]);

        let _ = seal(b"issue-76 plaintext");

        let begin_calls: Vec<_> = platform::calls_for_test()
            .into_iter()
            .filter(|(uri, _)| uri.ends_with("/begin"))
            .collect();
        assert!(!begin_calls.is_empty(), "expected at least one begin call");
        for (_, payload) in begin_calls {
            assert!(
                !payload.contains("mac_length"),
                "begin request still carries mac_length: {payload}"
            );
        }
        platform::reset_for_test();
    }

    /// (e) Removing the key resets the backend cache (kept from the original suite, now
    /// exercised through the shared `reset` fixture too).
    #[test]
    fn remove_resets_selected_via_shared_fixture() {
        let _guard = crate::testlock::serial();
        reset();
        SELECTED.store(MODERN, Ordering::Relaxed);
        remove(&Backend::Keymanager3, super::KEY_NAME);
        assert_eq!(SELECTED.load(Ordering::Relaxed), UNKNOWN);
    }

    /// (g) Issue #76 storage telemetry: a `begin(decrypt)` refusal reached through `seal`'s own
    /// round-trip proof publishes the exact stage and service error code — the shape
    /// `seal_refuses_a_backend_that_cannot_open_its_own_envelope` above exercises without asserting
    /// on it.
    #[test]
    fn last_refusal_publishes_the_stage_and_code_of_a_begin_decrypt_refusal() {
        let _guard = crate::testlock::serial();
        reset();
        assert_eq!(super::last_refusal(), None, "clean slate");
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc", "iv": b64::encode(b"0123456789ab")})),
            ),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(b"ciphertext-placeholder")})),
            ),
            (
                "begin",
                Ok(json!({"returnValue": false, "errorCode": -10001, "errorText": "key not found"})),
            ),
        ]);

        assert!(seal(b"issue-76 plaintext").is_none());
        assert_eq!(
            super::last_refusal(),
            Some(super::LastRefusal {
                stage: StorageStage::BeginDecrypt,
                error_code: Some(-10001),
            })
        );
        platform::reset_for_test();
    }

    /// (h) A missing-field reply (no `errorCode` at all) publishes its stage with no code, rather
    /// than being invisible to the telemetry wiring.
    #[test]
    fn last_refusal_records_a_missing_field_with_no_error_code() {
        let _guard = crate::testlock::serial();
        reset();
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc", "iv": b64::encode(b"0123456789ab")})),
            ),
            ("finish", Ok(json!({"returnValue": true}))), // no output
        ]);

        assert!(seal(b"issue-76 plaintext").is_none());
        assert_eq!(
            super::last_refusal(),
            Some(super::LastRefusal {
                stage: StorageStage::FinishEncrypt,
                error_code: None,
            })
        );
        platform::reset_for_test();
    }

    /// (m) A firmware with NO keymanager3 at all — every set before webOS 24, the dev set
    /// included — is answered by the LS2 hub itself, at once, with `errorCode:-1` and
    /// `"Service does not exist: com.webos.service.keymanager3."` (measured on the webOS 4.10 dev
    /// set, 2026-09-10, `luna-send` as root). That is the ABSENCE of a service, not a refusal by
    /// one: it must publish no `last_refusal` (or every such install's first save would send a
    /// StorageError), and it is not evidence for anything.
    #[test]
    fn an_absent_service_is_not_a_refusal() {
        let _guard = crate::testlock::serial();
        reset();
        platform::script_for_test(vec![(
            "generateKey",
            Ok(json!({
                "returnValue": false, "errorCode": -1,
                "errorText": "Service does not exist: com.webos.service.keymanager3."
            })),
        )]);
        assert!(seal(b"issue-76 plaintext").is_none());
        assert_eq!(SELECTED.load(Ordering::Relaxed), UNAVAILABLE);
        assert_eq!(super::last_refusal(), None, "an absent service refused nothing");
        platform::reset_for_test();
    }

    /// (i) A genuine round-trip MISMATCH — `open` succeeds but hands back different bytes than
    /// what was sealed, never a service refusal — publishes `RoundtripMismatch` with no code, since
    /// no service reply carried an `errorCode` for this failure.
    #[test]
    fn last_refusal_records_a_genuine_roundtrip_mismatch() {
        let _guard = crate::testlock::serial();
        reset();
        let plain = b"issue-76 plaintext";
        let iv = b64::encode(b"0123456789ab");
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc", "iv": iv})),
            ),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(b"ciphertext-placeholder")})),
            ),
            ("begin", Ok(json!({"returnValue": true, "handle": "h-dec"}))),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(b"not the original plaintext")})),
            ),
        ]);

        assert!(seal(plain).is_none());
        assert_eq!(
            super::last_refusal(),
            Some(super::LastRefusal {
                stage: StorageStage::RoundtripMismatch,
                error_code: None,
            })
        );
        platform::reset_for_test();
    }

    /// (j) `remove` (sign-out) clears the last refusal — a stale verdict about the account that
    /// just left must never be attached to a report about the one that signs in next.
    #[test]
    fn removing_the_key_clears_the_last_refusal() {
        let _guard = crate::testlock::serial();
        reset();
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": false, "errorCode": -10001, "errorText": "key not found"})),
            ),
        ]);
        assert!(seal(b"plain").is_none());
        assert!(super::last_refusal().is_some());
        platform::reset_for_test();

        remove(&Backend::Keymanager3, super::KEY_NAME);
        assert_eq!(super::last_refusal(), None);
    }
}
