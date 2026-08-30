//! Sentry Native's crash-capture half, without its network half.
//!
//! The SDK is linked only on the television and is built with `SENTRY_TRANSPORT=none`. Its native
//! backend owns the fatal-signal handler and an out-of-process daemon: on a fault the daemon can
//! still read the stopped process, walk ARM frame chains and copy registers into a JSON event. It
//! then launches this same executable with the envelope path. [`plx_sentry_spool_external`] is
//! that executable's deliberately tiny alternate entry point; it moves the envelope into a
//! private pending directory and exits before the ordinary boot opens or truncates any log.
//!
//! A later healthy boot calls [`import_pending`]. It discards the SDK's envelope header (including
//! its DSN), accepts exactly one bounded event item, strips local directory names from module
//! paths, and appends the JSON body to the application's existing consent-aware durable spool.
//! The ordinary sender remains the only code that opens a Sentry connection. This division is
//! load-bearing: signal capture needs native code, while consent, retry and data minimisation must
//! keep one implementation.

use std::path::{Path, PathBuf};

const DATABASE_DIR: &str = "plxnative-sentry-db";
const PENDING_DIR: &str = "plxnative-sentry-pending";

/// Keep the capture backend alive until the app leaves `plex_run`.
pub(crate) struct Guard;

/// Enough identity to suppress the local fallback copy of a crash whose native envelope was
/// already durably queued. There is no timestamp in the async-signal-safe fallback record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CrashKey {
    pub build_id: String,
    pub signal: u32,
}

impl Drop for Guard {
    fn drop(&mut self) {
        stop();
        remove_database();
    }
}

/// Bring the capture backend into line with the currently published consent decision.
///
/// Boot imports pending envelopes before calling this. A withdrawal first restores the C crash
/// tracer that Sentry found installed ahead of it, then removes both native directories.
pub(crate) fn sync(c: &super::consent::Consent) -> Guard {
    let wanted = c.answered() && c.errors && super::sender::sentry_dsn().is_some();
    if wanted {
        start();
    } else {
        stop();
        purge_all();
    }
    Guard
}

/// Apply a consent change without manufacturing a second lifetime guard.
pub(crate) fn sync_change(c: &super::consent::Consent) {
    let wanted = c.answered() && c.errors && super::sender::sentry_dsn().is_some();
    if wanted {
        let _ = import_pending();
        start();
    } else {
        stop();
        purge_all();
    }
}

fn database_dir() -> PathBuf {
    crate::paths::in_runtime_dir(DATABASE_DIR)
}

fn pending_dir() -> PathBuf {
    crate::paths::in_runtime_dir(PENDING_DIR)
}

fn remove_database() {
    let _ = std::fs::remove_dir_all(database_dir());
}

fn purge_all() {
    remove_database();
    let _ = std::fs::remove_dir_all(pending_dir());
}

/// Is this the UUID-shaped filename the SDK gives an external event envelope?
fn envelope_filename(name: &str) -> bool {
    let Some(id) = name.strip_suffix(".envelope") else {
        return false;
    };
    normalise_event_id(id).is_some()
}

fn external_envelope_path(source: &Path) -> bool {
    external_envelope_path_in(source, &database_dir())
}

fn external_envelope_path_in(source: &Path, database: &Path) -> bool {
    source.parent() == Some(database.join("external").as_path())
        && source
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(envelope_filename)
}

/// Move one SDK-owned external envelope into the application's pending directory.
///
/// The path is an argument to a public executable, so it is treated as hostile. Only a regular
/// file immediately inside this install's exact SDK `external` directory is accepted; symlinks,
/// traversal and lookalike filenames leave the ordinary application entry point untouched.
fn spool_external(source: &Path) -> bool {
    spool_external_in(source, &database_dir(), &pending_dir())
}

fn spool_external_in(source: &Path, database: &Path, dest_dir: &Path) -> bool {
    if !external_envelope_path_in(source, database) {
        return false;
    }
    let Some(name) = source.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    if !envelope_filename(name) {
        return false;
    }
    let Ok(meta) = std::fs::symlink_metadata(source) else {
        return false;
    };
    if !meta.file_type().is_file() || meta.len() > super::queue::MAX_RECORD as u64 {
        return false;
    }

    if std::fs::create_dir_all(dest_dir).is_err() {
        return false;
    }
    let Ok(dest_meta) = std::fs::symlink_metadata(dest_dir) else {
        return false;
    };
    if !dest_meta.file_type().is_dir() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::set_permissions(dest_dir, std::fs::Permissions::from_mode(0o700)).is_err() {
            return false;
        }
    }
    let dest = dest_dir.join(name);
    // `rename` overwrites on Unix. A hard link gives us atomic no-clobber semantics instead; both
    // directories live below one runtime root, so they are necessarily on the same filesystem.
    if std::fs::hard_link(source, &dest).is_err() {
        return false;
    }
    let linked_regular = std::fs::symlink_metadata(&dest)
        .is_ok_and(|m| m.file_type().is_file() && m.len() <= super::queue::MAX_RECORD as u64);
    if !linked_regular {
        let _ = std::fs::remove_file(dest);
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600)).is_err() {
            let _ = std::fs::remove_file(dest);
            return false;
        }
    }
    if std::fs::remove_file(source).is_err() {
        let _ = std::fs::remove_file(dest);
        return false;
    }
    true
}

/// Alternate process entry used by Sentry's external reporter.
///
/// Returns 1 whenever the argument belongs to the SDK external-report directory. C then exits
/// immediately, whether the move succeeded or failed; returning 0 means this was an ordinary
/// invocation whose first argument merely happened to be a path.
///
/// # Safety
/// `path` must be null or point to a valid NUL-terminated string for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn plx_sentry_spool_external(
    path: *const std::os::raw::c_char,
) -> std::os::raw::c_int {
    if path.is_null() {
        return 0;
    }
    let bytes = std::ffi::CStr::from_ptr(path).to_bytes();
    #[cfg(unix)]
    let path = {
        use std::os::unix::ffi::OsStrExt;
        Path::new(std::ffi::OsStr::from_bytes(bytes))
    };
    #[cfg(not(unix))]
    let path = Path::new(std::str::from_utf8(bytes).unwrap_or(""));
    if !external_envelope_path(path) {
        return 0;
    }
    // Once recognised, this is never an ordinary launch — even disk-full, duplicate-name or
    // permission failure must exit rather than opening a second full UI process. The SDK retains
    // the source envelope and cleans stale external reports itself when the move fails.
    let _ = spool_external(path);
    1
}

/// Strip UUID punctuation and reject anything but exactly 128 lowercase-able hex bits.
fn normalise_event_id(id: &str) -> Option<String> {
    let valid = match id.len() {
        32 => id.bytes().all(|b| b.is_ascii_hexdigit()),
        36 => id.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        }),
        _ => false,
    };
    if !valid {
        return None;
    }
    let compact: String = id.chars().filter(|&c| c != '-').collect();
    if compact.len() != 32 || !compact.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(compact.to_ascii_lowercase())
}

fn basename(value: &mut serde_json::Value) {
    let Some(s) = value.as_str() else { return };
    let Some(name) = Path::new(s).file_name().and_then(|n| n.to_str()) else {
        *value = serde_json::Value::Null;
        return;
    };
    *value = serde_json::Value::String(name.to_string());
}

const TOP_FIELDS: &[&str] = &[
    "event_id",
    "timestamp",
    "platform",
    "level",
    "release",
    "environment",
    "dist",
    "sdk",
    "contexts",
    "exception",
    "threads",
    "debug_meta",
];
const SDK_FIELDS: &[&str] = &["name", "version"];
const OS_FIELDS: &[&str] = &["type", "name", "version", "build", "kernel_version"];
const WEBOS_FIELDS: &[&str] = &["type", "name", "release", "codename", "api"];
const EXCEPTION_CONTAINER_FIELDS: &[&str] = &["values"];
const EXCEPTION_FIELDS: &[&str] = &["type", "value", "mechanism", "stacktrace"];
const MECHANISM_FIELDS: &[&str] = &["type", "handled", "meta"];
const MECHANISM_META_FIELDS: &[&str] = &["signal"];
const SIGNAL_FIELDS: &[&str] = &["number", "name"];
const STACK_FIELDS: &[&str] = &["frames", "registers"];
const REGISTER_FIELDS: &[&str] = &[
    "r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10", "fp", "ip", "sp", "lr",
    "pc", "cpsr",
];
const FRAME_FIELDS: &[&str] = &[
    "instruction_addr",
    "symbol_addr",
    "function",
    "package",
    "filename",
    "lineno",
    "colno",
    "in_app",
    "trust",
    "addr_mode",
];
const THREADS_FIELDS: &[&str] = &["values"];
const THREAD_FIELDS: &[&str] = &["id", "name", "crashed", "current", "stacktrace"];
const DEBUG_META_FIELDS: &[&str] = &["images"];
const IMAGE_FIELDS: &[&str] = &[
    "type",
    "code_file",
    "debug_file",
    "code_id",
    "debug_id",
    "image_addr",
    "image_size",
    "image_vmaddr",
    "arch",
];

fn retain_fields(object: &mut serde_json::Map<String, serde_json::Value>, allowed: &[&str]) {
    object.retain(|key, _| allowed.contains(&key.as_str()));
}

fn sanitise_frames(stacktrace: &mut serde_json::Value) {
    let Some(stack) = stacktrace.as_object_mut() else {
        return;
    };
    retain_fields(stack, STACK_FIELDS);
    if let Some(registers) = stack
        .get_mut("registers")
        .and_then(serde_json::Value::as_object_mut)
    {
        retain_fields(registers, REGISTER_FIELDS);
    }
    let Some(frames) = stacktrace
        .get_mut("frames")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for frame in frames {
        let Some(frame) = frame.as_object_mut() else {
            continue;
        };
        retain_fields(frame, FRAME_FIELDS);
        if let Some(package) = frame.get_mut("package") {
            basename(package);
        }
        if let Some(filename) = frame.get_mut("filename") {
            basename(filename);
        }
    }
}

/// Remove host-local path prefixes from every native stack and debug image.
fn sanitise_event(event: &mut serde_json::Value, event_id: &str) {
    event["event_id"] = serde_json::Value::String(event_id.to_string());
    // An allowlist makes a future SDK scope change fail private: the crash channel is for machine
    // state, never account/request data or arbitrary tags/extras.
    if let Some(o) = event.as_object_mut() {
        retain_fields(o, TOP_FIELDS);
    }
    if let Some(contexts) = event
        .get_mut("contexts")
        .and_then(serde_json::Value::as_object_mut)
    {
        // The SDK creates a random trace/span pair even though this application does no tracing;
        // retaining only kernel + our fixed firmware context removes that identifier and any
        // future context by default. Hardware identity is deliberately not allowlisted.
        contexts.retain(|key, _| key == "os" || key == "webos");
        if let Some(os) = contexts
            .get_mut("os")
            .and_then(serde_json::Value::as_object_mut)
        {
            retain_fields(os, OS_FIELDS);
        }
        if let Some(webos) = contexts
            .get_mut("webos")
            .and_then(serde_json::Value::as_object_mut)
        {
            retain_fields(webos, WEBOS_FIELDS);
        }
    }
    if let Some(sdk) = event
        .get_mut("sdk")
        .and_then(serde_json::Value::as_object_mut)
    {
        retain_fields(sdk, SDK_FIELDS);
    }
    if let Some(exception) = event
        .get_mut("exception")
        .and_then(serde_json::Value::as_object_mut)
    {
        retain_fields(exception, EXCEPTION_CONTAINER_FIELDS);
    }

    if let Some(values) = event
        .pointer_mut("/exception/values")
        .and_then(serde_json::Value::as_array_mut)
    {
        for exception in values {
            let Some(exception) = exception.as_object_mut() else {
                continue;
            };
            retain_fields(exception, EXCEPTION_FIELDS);
            if let Some(mechanism) = exception
                .get_mut("mechanism")
                .and_then(serde_json::Value::as_object_mut)
            {
                retain_fields(mechanism, MECHANISM_FIELDS);
                if let Some(meta) = mechanism
                    .get_mut("meta")
                    .and_then(serde_json::Value::as_object_mut)
                {
                    retain_fields(meta, MECHANISM_META_FIELDS);
                    if let Some(signal) = meta
                        .get_mut("signal")
                        .and_then(serde_json::Value::as_object_mut)
                    {
                        retain_fields(signal, SIGNAL_FIELDS);
                    }
                }
            }
            if let Some(stack) = exception.get_mut("stacktrace") {
                sanitise_frames(stack);
            }
        }
    }
    if let Some(threads) = event
        .get_mut("threads")
        .and_then(serde_json::Value::as_object_mut)
    {
        retain_fields(threads, THREADS_FIELDS);
    }
    if let Some(values) = event
        .pointer_mut("/threads/values")
        .and_then(serde_json::Value::as_array_mut)
    {
        for thread in values {
            let Some(thread) = thread.as_object_mut() else {
                continue;
            };
            retain_fields(thread, THREAD_FIELDS);
            if let Some(stack) = thread.get_mut("stacktrace") {
                sanitise_frames(stack);
            }
        }
    }
    if let Some(debug_meta) = event
        .get_mut("debug_meta")
        .and_then(serde_json::Value::as_object_mut)
    {
        retain_fields(debug_meta, DEBUG_META_FIELDS);
    }
    if let Some(images) = event
        .pointer_mut("/debug_meta/images")
        .and_then(serde_json::Value::as_array_mut)
    {
        for image in images {
            let Some(image) = image.as_object_mut() else {
                continue;
            };
            retain_fields(image, IMAGE_FIELDS);
            if let Some(code_file) = image.get_mut("code_file") {
                basename(code_file);
            }
            if let Some(debug_file) = image.get_mut("debug_file") {
                basename(debug_file);
            }
        }
    }
}

/// A representative native crash built through the same path sanitizer as a real envelope.
///
/// Dynamic values are explicit placeholders: the preview is shown before consent, so it may not
/// manufacture an event id or inspect a pending crash merely to explain the schema.
pub(crate) fn preview_event() -> Vec<u8> {
    let mut event = serde_json::json!({
        "event_id": "<random id for this crash>",
        "timestamp": "<crash time>",
        "platform": "native",
        "level": "fatal",
        "release": concat!("plxnative@", env!("CARGO_PKG_VERSION")),
        "environment": super::sender::ENVIRONMENT,
        "dist": "<ELF build id>",
        "sdk": {"name": "plxnative", "version": "0.13.9"},
        "contexts": {
            "os": {"type": "os", "name": "Linux", "version": "<kernel release>",
                "build": "<kernel build suffix>", "kernel_version": "<kernel release>"},
            "webos": {"type": "webos", "name": "webOS TV", "release": "<webOS release>",
                "codename": "<webOS release codename>", "api": "<webOS API version>"}
        },
        "exception": {"values": [{
            "type": "SIGSEGV",
            "value": "Fatal crash: SIGSEGV",
            "mechanism": {"type": "signalhandler", "handled": false,
                "meta": {"signal": {"number": 11, "name": "SIGSEGV"}}},
            "stacktrace": {
                "frames": [
                    {"instruction_addr": "<caller address>", "symbol_addr": "<symbol address>",
                        "function": "<compiled function>", "package": "/private/app/plxnative",
                        "filename": "/private/source/example.rs", "lineno": "<source line>",
                        "colno": "<source column>", "in_app": true, "trust": "fp",
                        "addr_mode": "abs"},
                    {"instruction_addr": "<fault address>", "trust": "context", "package": "/private/app/plxnative"}
                ],
                "registers": {"r0": "<address>", "r1": "<address>", "r2": "<address>",
                    "r3": "<address>", "r4": "<address>", "r5": "<address>",
                    "r6": "<address>", "r7": "<address>", "r8": "<address>",
                    "r9": "<address>", "r10": "<address>", "fp": "<address>",
                    "ip": "<address>", "sp": "<address>", "lr": "<address>",
                    "pc": "<address>", "cpsr": "<flags>"}
            }
        }]},
        "threads": {"values": [{"id": "<thread id>", "name": "<internal thread label>",
            "crashed": false, "current": false,
            "stacktrace": {
                "frames": [{"instruction_addr": "<thread instruction address>",
                    "symbol_addr": "<symbol address>", "function": "<compiled function>",
                    "package": "/private/app/plxnative", "filename": "/private/source/example.rs",
                    "lineno": "<source line>", "colno": "<source column>", "in_app": true,
                    "trust": "context", "addr_mode": "abs"}],
                "registers": {"r0": "<address>", "r1": "<address>", "r2": "<address>",
                    "r3": "<address>", "r4": "<address>", "r5": "<address>",
                    "r6": "<address>", "r7": "<address>", "r8": "<address>",
                    "r9": "<address>", "r10": "<address>", "fp": "<address>",
                    "ip": "<address>", "sp": "<address>", "lr": "<address>",
                    "pc": "<address>", "cpsr": "<flags>"}
            }}]},
        "debug_meta": {"images": [{
            "type": "elf", "code_file": "/private/app/plxnative",
            "debug_file": "/private/app/plxnative.debug", "code_id": "<ELF code id>",
            "debug_id": "<ELF debug id>", "image_addr": "<load address>",
            "image_size": "<bytes>", "image_vmaddr": "<ELF virtual address>", "arch": "arm"
        }]}
    });
    sanitise_event(&mut event, "<random id for this crash>");
    serde_json::to_vec(&event).unwrap_or_default()
}

/// Parse one exact Sentry envelope into the event body our existing sender accepts.
fn event_from_envelope(bytes: &[u8]) -> Option<(String, Vec<u8>, Option<CrashKey>)> {
    if bytes.len() > super::queue::MAX_RECORD {
        return None;
    }
    let first_nl = bytes.iter().position(|&b| b == b'\n')?;
    let header: serde_json::Value = serde_json::from_slice(&bytes[..first_nl]).ok()?;
    let header_id = normalise_event_id(header.get("event_id")?.as_str()?)?;

    let after_header = &bytes[first_nl + 1..];
    let item_nl = after_header.iter().position(|&b| b == b'\n')?;
    let item: serde_json::Value = serde_json::from_slice(&after_header[..item_nl]).ok()?;
    if item.get("type").and_then(serde_json::Value::as_str) != Some("event") {
        return None;
    }
    let len = usize::try_from(item.get("length")?.as_u64()?).ok()?;
    if len == 0 || len > super::queue::MAX_RECORD {
        return None;
    }
    let payload_and_tail = &after_header[item_nl + 1..];
    let payload = payload_and_tail.get(..len)?;
    if !matches!(payload_and_tail.get(len..), Some([]) | Some([b'\n'])) {
        return None; // no attachments or hidden second item on this privacy-bounded channel
    }
    let mut event: serde_json::Value = serde_json::from_slice(payload).ok()?;
    if !event.is_object() {
        return None;
    }
    let payload_id = normalise_event_id(event.get("event_id")?.as_str()?)?;
    if payload_id != header_id {
        return None;
    }
    if event.get("platform").and_then(serde_json::Value::as_str) != Some("native") {
        return None;
    }
    sanitise_event(&mut event, &header_id);
    let crash_key = event
        .get("dist")
        .and_then(serde_json::Value::as_str)
        .filter(|id| id.len() == 40 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .zip(
            event
                .pointer("/exception/values/0/mechanism/meta/signal/number")
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| u32::try_from(n).ok()),
        )
        .map(|(build_id, signal)| CrashKey {
            build_id: build_id.to_ascii_lowercase(),
            signal,
        });
    let body = serde_json::to_vec(&event).ok()?;
    (body.len() <= super::queue::MAX_RECORD).then_some((header_id, body, crash_key))
}

/// Import every complete native envelope, deleting it only after the durable spool accepted it.
pub(crate) fn import_pending() -> Vec<CrashKey> {
    if !super::consent::allows_errors() || super::sender::sentry_dsn().is_none() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(pending_dir()) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    paths.sort();
    let mut queued = 0usize;
    let mut rejected = 0usize;
    let mut crash_keys = Vec::new();
    for path in paths {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !envelope_filename(name) {
            continue;
        }
        let parsed = std::fs::symlink_metadata(&path)
            .ok()
            .filter(|m| m.file_type().is_file() && m.len() <= super::queue::MAX_RECORD as u64)
            .and_then(|_| {
                use std::io::Read;
                let mut bytes = Vec::new();
                std::fs::File::open(&path)
                    .ok()?
                    .take(super::queue::MAX_RECORD as u64 + 1)
                    .read_to_end(&mut bytes)
                    .ok()?;
                (bytes.len() <= super::queue::MAX_RECORD).then_some(bytes)
            })
            .and_then(|b| event_from_envelope(&b));
        let Some((event_id, body, crash_key)) = parsed else {
            rejected += 1;
            let _ = std::fs::remove_file(&path);
            continue;
        };
        let accepted = super::spool::append(&super::queue::Record {
            category: super::queue::Category::Errors,
            dest: super::queue::Dest::Sentry,
            event_id,
            body,
        });
        if accepted {
            queued += 1;
            if let Some(key) = crash_key {
                crash_keys.push(key);
            }
            let _ = std::fs::remove_file(&path);
        }
    }
    if queued != 0 || rejected != 0 {
        crate::log(&format!(
            "telemetry: native crash envelopes queued={queued} rejected={rejected}"
        ));
    }
    crash_keys
}

#[cfg(all(target_os = "linux", target_arch = "arm"))]
mod sdk {
    use std::ffi::{c_char, c_int, c_void, CString};
    use std::sync::atomic::{AtomicBool, Ordering};

    static ACTIVE: AtomicBool = AtomicBool::new(false);

    extern "C" {
        fn sentry_options_new() -> *mut c_void;
        fn sentry_options_set_dsn(options: *mut c_void, value: *const c_char);
        fn sentry_options_set_database_path(options: *mut c_void, value: *const c_char);
        fn sentry_options_set_handler_path(options: *mut c_void, value: *const c_char);
        fn sentry_options_set_external_crash_reporter_path(
            options: *mut c_void,
            value: *const c_char,
        );
        fn sentry_options_set_release(options: *mut c_void, value: *const c_char);
        fn sentry_options_set_environment(options: *mut c_void, value: *const c_char);
        fn sentry_options_set_dist(options: *mut c_void, value: *const c_char);
        fn sentry_options_set_auto_session_tracking(options: *mut c_void, value: c_int);
        fn sentry_options_set_max_breadcrumbs(options: *mut c_void, value: usize);
        fn sentry_options_set_debug(options: *mut c_void, value: c_int);
        fn sentry_options_set_crash_reporting_mode(options: *mut c_void, value: c_int);
        fn sentry_init(options: *mut c_void) -> c_int;
        fn sentry_close() -> c_int;
        fn plx_sentry_set_webos_context(
            name: *const c_char,
            release: *const c_char,
            codename: *const c_char,
            api: *const c_char,
        );
    }

    fn cstring(value: impl Into<Vec<u8>>) -> Option<CString> {
        CString::new(value).ok()
    }

    pub(super) fn start() {
        if ACTIVE.load(Ordering::Acquire) {
            return;
        }
        let Some(dsn) = super::super::sender::sentry_dsn().and_then(cstring) else {
            return;
        };
        let Some(database) = cstring(super::database_dir().as_os_str().as_encoded_bytes()) else {
            return;
        };
        let Some(handler) = cstring(
            crate::paths::app_dir()
                .join("sentry-crash")
                .as_os_str()
                .as_encoded_bytes(),
        ) else {
            return;
        };
        let Ok(executable_path) = std::env::current_exe() else {
            return;
        };
        let Some(executable) = cstring(executable_path.as_os_str().as_encoded_bytes()) else {
            return;
        };
        let Some(release) = cstring(format!("plxnative@{}", env!("CARGO_PKG_VERSION"))) else {
            return;
        };
        let Some(environment) = cstring(super::super::sender::ENVIRONMENT) else {
            return;
        };
        let Some(dist) = cstring(super::super::sentry::build_id()) else {
            return;
        };
        let webos = crate::webos::info();
        let webos_name = cstring(webos.name.as_bytes());
        let webos_release = cstring(webos.release.as_bytes());
        let webos_codename = cstring(webos.codename.as_bytes());
        let webos_api = cstring(webos.api.as_bytes());
        let ptr = |value: &Option<CString>| {
            value
                .as_ref()
                .map_or(std::ptr::null(), |value| value.as_ptr())
        };

        unsafe {
            let options = sentry_options_new();
            if options.is_null() {
                return;
            }
            sentry_options_set_dsn(options, dsn.as_ptr());
            sentry_options_set_database_path(options, database.as_ptr());
            sentry_options_set_handler_path(options, handler.as_ptr());
            sentry_options_set_external_crash_reporter_path(options, executable.as_ptr());
            sentry_options_set_release(options, release.as_ptr());
            sentry_options_set_environment(options, environment.as_ptr());
            if !dist.as_bytes().is_empty() {
                sentry_options_set_dist(options, dist.as_ptr());
            }
            sentry_options_set_auto_session_tracking(options, 0);
            sentry_options_set_max_breadcrumbs(options, 0);
            sentry_options_set_debug(options, 0);
            sentry_options_set_crash_reporting_mode(options, 1); // NATIVE, no minidump
            if sentry_init(options) == 0 {
                plx_sentry_set_webos_context(
                    ptr(&webos_name),
                    ptr(&webos_release),
                    ptr(&webos_codename),
                    ptr(&webos_api),
                );
                ACTIVE.store(true, Ordering::Release);
                crate::log("telemetry: native ARM crash capture active");
            } else {
                // `sentry_init` takes ownership even when backend startup fails.
                crate::log(
                    "telemetry: native crash capture unavailable; C fallback remains active",
                );
            }
        }
    }

    pub(super) fn stop() {
        if ACTIVE.swap(false, Ordering::AcqRel) {
            unsafe {
                let _ = sentry_close();
            }
        }
    }
}

#[cfg(all(target_os = "linux", target_arch = "arm"))]
fn start() {
    sdk::start();
}

#[cfg(not(all(target_os = "linux", target_arch = "arm")))]
fn start() {}

#[cfg(all(target_os = "linux", target_arch = "arm"))]
fn stop() {
    sdk::stop();
}

#[cfg(not(all(target_os = "linux", target_arch = "arm")))]
fn stop() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_keys(value: &serde_json::Value, pointer: &str, allowed: &[&str]) {
        let object = value
            .pointer(pointer)
            .and_then(serde_json::Value::as_object)
            .unwrap();
        let mut actual: Vec<&str> = object.keys().map(String::as_str).collect();
        let mut expected = allowed.to_vec();
        actual.sort_unstable();
        expected.sort_unstable();
        assert_eq!(
            actual, expected,
            "preview drifted from sanitizer at {pointer}"
        );
    }

    fn envelope(event_id: &str, mut event: serde_json::Value) -> Vec<u8> {
        event["event_id"] = serde_json::Value::String(event_id.to_string());
        let body = serde_json::to_vec(&event).unwrap();
        format!(
            "{{\"dsn\":\"https://secret@example.invalid/1\",\"event_id\":\"{event_id}\"}}\n{{\"type\":\"event\",\"length\":{}}}\n{}",
            body.len(),
            String::from_utf8(body).unwrap()
        )
        .into_bytes()
    }

    #[test]
    fn only_a_uuid_envelope_name_is_accepted() {
        assert!(envelope_filename(
            "91ad1844-535b-4dac-89d9-5384165d703c.envelope"
        ));
        assert!(envelope_filename(
            "91ad1844535b4dac89d95384165d703c.envelope"
        ));
        for bad in [
            "event.envelope",
            "../event.envelope",
            "a.json",
            "0.envelope",
            "-91ad1844535b4dac89d95384165d703c.envelope",
            "91ad-1844-535b4dac89d95384165d703c.envelope",
        ] {
            assert!(!envelope_filename(bad), "accepted {bad}");
        }
    }

    #[test]
    fn the_preview_declares_every_allowlisted_native_field() {
        let value: serde_json::Value = serde_json::from_slice(&preview_event()).unwrap();
        assert_keys(&value, "", TOP_FIELDS);
        assert_keys(&value, "/sdk", SDK_FIELDS);
        assert_keys(&value, "/contexts/os", OS_FIELDS);
        assert_keys(&value, "/contexts/webos", WEBOS_FIELDS);
        assert_keys(&value, "/exception", EXCEPTION_CONTAINER_FIELDS);
        assert_keys(&value, "/exception/values/0", EXCEPTION_FIELDS);
        assert_keys(&value, "/exception/values/0/mechanism", MECHANISM_FIELDS);
        assert_keys(
            &value,
            "/exception/values/0/mechanism/meta",
            MECHANISM_META_FIELDS,
        );
        assert_keys(
            &value,
            "/exception/values/0/mechanism/meta/signal",
            SIGNAL_FIELDS,
        );
        assert_keys(&value, "/exception/values/0/stacktrace", STACK_FIELDS);
        assert_keys(
            &value,
            "/exception/values/0/stacktrace/registers",
            REGISTER_FIELDS,
        );
        assert_keys(
            &value,
            "/exception/values/0/stacktrace/frames/0",
            FRAME_FIELDS,
        );
        assert_keys(&value, "/threads", THREADS_FIELDS);
        assert_keys(&value, "/threads/values/0", THREAD_FIELDS);
        assert_keys(&value, "/threads/values/0/stacktrace", STACK_FIELDS);
        assert_keys(
            &value,
            "/threads/values/0/stacktrace/registers",
            REGISTER_FIELDS,
        );
        assert_keys(
            &value,
            "/threads/values/0/stacktrace/frames/0",
            FRAME_FIELDS,
        );
        assert_keys(&value, "/debug_meta", DEBUG_META_FIELDS);
        assert_keys(&value, "/debug_meta/images/0", IMAGE_FIELDS);
    }

    #[test]
    fn external_mode_moves_only_the_sdks_regular_envelope() {
        let _g = crate::testlock::serial();
        let root =
            std::env::temp_dir().join(format!("plxnative-sentry-test-{}", std::process::id()));
        let database = root.join("db");
        let external = database.join("external");
        let pending = root.join("pending");
        std::fs::create_dir_all(&external).unwrap();
        let name = "91ad1844-535b-4dac-89d9-5384165d703c.envelope";
        let source = external.join(name);
        std::fs::write(&source, b"event").unwrap();
        assert!(spool_external_in(&source, &database, &pending));
        assert!(!source.exists());
        assert_eq!(std::fs::read(pending.join(name)).unwrap(), b"event");

        let lookalike = root.join(name);
        std::fs::write(&lookalike, b"event").unwrap();
        assert!(!spool_external_in(&lookalike, &database, &pending));
        assert!(lookalike.exists(), "an unrecognised argument was touched");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_native_envelope_becomes_one_path_sanitised_event() {
        let id = "91ad1844-535b-4dac-89d9-5384165d703c";
        let bytes = envelope(
            id,
            serde_json::json!({
                "platform": "native",
                "level": "fatal",
                "dist": "11223344556677889900aabbccddeeff00112233",
                "user": {"id": "must-not-pass"},
                "request": {"url": "must-not-pass"},
                "extra": {"future_sdk_field": "must-not-pass"},
                "arbitrary": "must-not-pass",
                "sdk": {"name": "plxnative", "version": "0.13.9", "future": "must-not-pass"},
                "contexts": {
                    "os": {"name": "Linux", "future": "must-not-pass"},
                    "webos": {"type": "webos", "name": "webOS TV", "release": "4.10.2",
                        "codename": "goldilocks2-grampians", "api": "4.1.0",
                        "model": "must-not-pass", "future": "must-not-pass"}
                },
                "exception": {"values": [{
                    "mechanism": {"meta": {"signal": {"number": 11, "name": "SIGSEGV"}}},
                    "stacktrace": {"frames": [
                    {"instruction_addr": "0x1234", "package": "/media/developer/apps/usr/palm/applications/com.beb.plxnative/plxnative", "vars": "must-not-pass"},
                    {"instruction_addr": "0xf00", "filename": "/private/source/main.c"}
                ], "registers": {"pc": "0x1234", "sp": "0xbeef", "future": "must-not-pass"},
                    "future": "must-not-pass"}, "future": "must-not-pass"}]},
                "threads": {"future": "must-not-pass", "values": [{"id": 7,
                    "future": "must-not-pass", "stacktrace": {"frames": [
                    {"package": "/lib/libc-2.24.so"}
                ]}}]},
                "debug_meta": {"future": "must-not-pass", "images": [{
                    "type": "elf", "code_file": "/media/developer/apps/x/plxnative",
                    "image_addr": "0x10000", "image_size": 4096,
                    "debug_id": "44332211-6655-8877-9900-aabbccddeeff",
                    "future": "must-not-pass"
                }]}
            }),
        );
        let (event_id, body, crash_key) = event_from_envelope(&bytes).expect("valid envelope");
        assert_eq!(event_id, "91ad1844535b4dac89d95384165d703c");
        assert_eq!(
            crash_key,
            Some(CrashKey {
                build_id: "11223344556677889900aabbccddeeff00112233".to_string(),
                signal: 11,
            })
        );
        let text = String::from_utf8(body.clone()).unwrap();
        assert!(
            !text.contains("https://secret"),
            "DSN escaped the envelope header"
        );
        assert!(!text.contains("/media/") && !text.contains("/private/"));
        assert!(
            !text.contains("must-not-pass"),
            "identity/request scope escaped"
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v["exception"]["values"][0]["stacktrace"]["registers"]["sp"],
            "0xbeef"
        );
        assert_eq!(
            v["exception"]["values"][0]["stacktrace"]["frames"][0]["package"],
            "plxnative"
        );
        assert_eq!(v["debug_meta"]["images"][0]["code_file"], "plxnative");
        assert_eq!(v["debug_meta"]["images"][0]["image_size"], 4096);
        assert_eq!(v["contexts"]["webos"]["release"], "4.10.2");
        assert!(v["contexts"]["webos"].get("model").is_none());
    }

    #[test]
    fn malformed_or_multi_item_envelopes_are_not_partially_imported() {
        let id = "91ad1844535b4dac89d95384165d703c";
        let good = envelope(id, serde_json::json!({"platform": "native"}));
        let scalar =
            format!("{{\"event_id\":\"{id}\"}}\n{{\"type\":\"event\",\"length\":8}}\n\"native\"")
                .into_bytes();
        assert!(
            event_from_envelope(&scalar).is_none(),
            "a scalar event must not panic or import"
        );
        let mut extra = good.clone();
        extra.extend_from_slice(b"\n{\"type\":\"attachment\",\"length\":1}\nx");
        assert!(event_from_envelope(&extra).is_none());

        let mut wrong_id = good;
        let needle = id.as_bytes();
        let pos = wrong_id
            .windows(needle.len())
            .rposition(|w| w == needle)
            .unwrap();
        wrong_id[pos] = b'a';
        assert!(event_from_envelope(&wrong_id).is_none());
    }
}
