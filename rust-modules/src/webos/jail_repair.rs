//! User-confirmed repair for the k5lp/k3lp Developer Mode jail.
//!
//! No repair runs at boot. `request` is the sole production repair entry point and
//! exists for a confirmation action in the UI. An accepted attempt is retained for the life of
//! the process, including a timeout: the remote shell may still be running after LS2 gives up.

use crate::task::MainThread;
use serde_json::Value;
use std::path::Path;
use std::sync::Mutex;
#[cfg(all(not(feature = "hostsim"), not(test)))]
use std::time::Duration;

#[cfg(all(not(feature = "hostsim"), not(test)))]
const EXEC_URI: &str = "luna://org.webosbrew.hbchannel.service/exec";
#[cfg(all(not(feature = "hostsim"), not(test)))]
const BUDGET: Duration = Duration::from_secs(10);
const OK_MARKER: &str = "PLXNATIVE_JAIL_REPAIR_OK_74";
const NOT_ROOT_MARKER: &str = "PLXNATIVE_JAIL_REPAIR_NOT_ROOT_74";
const HBC_ABSENT_TEXT: &str = "Service does not exist: org.webosbrew.hbchannel.service.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    StartFailed,
    HbcUnavailable,
    NotRoot,
    CommandFailed,
    Timeout,
    Unreadable,
    Unsupported,
}

impl Failure {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::StartFailed => "Could not start the repair. Close and reopen PlxNative to try again.",
            Self::HbcUnavailable => "Homebrew Channel service is unavailable.",
            Self::NotRoot => "Homebrew Channel service is not running as root.",
            Self::CommandFailed => "The sandbox repair command failed.",
            Self::Timeout => "Repair outcome is unknown. Close and reopen the app to check again.",
            Self::Unreadable => "Repair completed, but this app cannot see /dev/rtkmem yet. Close and reopen the app.",
            Self::Unsupported => "Sandbox repair is not available on this device.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Idle,
    Running,
    Repaired,
    Failed(Failure),
}

struct Controller {
    state: Mutex<State>,
}

impl Controller {
    const fn new() -> Self {
        Self {
            state: Mutex::new(State::Idle),
        }
    }

    fn snapshot(&self) -> State {
        *self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn begin(&self, is_supported: bool) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if *state != State::Idle {
            return false;
        }
        if !is_supported {
            *state = State::Failed(Failure::Unsupported);
            return false;
        }
        *state = State::Running;
        true
    }

    fn finish(&self, result: Result<(), Failure>) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = match result {
            Ok(()) => State::Repaired,
            Err(failure) => State::Failed(failure),
        };
    }

    fn spawn_refused(&self) {
        self.finish(Err(Failure::StartFailed));
    }
}

static REPAIR: Controller = Controller::new();

#[cfg(feature = "devtriggers")]
crate::dev::latched_flag!(
    /// `/tmp/plxnative-jailrepair-probe` — one read-only HBC identity call for device verification.
    fn probe_armed = "jailrepair-probe";
);

/// Development-only proof that this jailed process can reach HBC's fixed exec endpoint. This is
/// intentionally separate from `request`: it neither checks nor alters repair state and its only
/// command is the read-only literal `id -u`.
#[cfg(feature = "devtriggers")]
pub(super) fn probe_if_armed() {
    if !probe_armed() {
        return;
    }
    if !crate::task::spawn_small("jail repair HBC probe", || {
        let result = probe_result(call_hbc(
            &serde_json::json!({ "command": "id -u" }).to_string(),
        ));
        crate::log(match result {
            ProbeResult::Root => "jail-repair-probe: root",
            ProbeResult::NotRoot => "jail-repair-probe: not_root",
            ProbeResult::Unavailable => "jail-repair-probe: unavailable",
        });
    }) {
        crate::log("jail-repair-probe: unavailable");
    }
}

#[cfg(not(feature = "devtriggers"))]
pub(super) fn probe_if_armed() {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(feature = "devtriggers")]
enum ProbeResult {
    Root,
    NotRoot,
    Unavailable,
}

#[cfg(feature = "devtriggers")]
fn probe_result(reply: Result<String, Failure>) -> ProbeResult {
    let Ok(reply) = reply else {
        return ProbeResult::Unavailable;
    };
    let Ok(value) = serde_json::from_str::<Value>(&reply) else {
        return ProbeResult::Unavailable;
    };
    if value.get("returnValue").and_then(Value::as_bool) != Some(true) {
        return ProbeResult::Unavailable;
    }
    match value.get("stdoutString").and_then(Value::as_str) {
        Some("0\n") => ProbeResult::Root,
        Some(stdout)
            if stdout.ends_with('\n')
                && stdout[..stdout.len() - 1]
                    .parse::<u32>()
                    .is_ok_and(|uid| uid != 0) =>
        {
            ProbeResult::NotRoot
        }
        Some(_) => ProbeResult::Unavailable,
        None => ProbeResult::Unavailable,
    }
}

pub(crate) fn snapshot() -> State {
    REPAIR.snapshot()
}

/// True only for the cached affected-SoC verdict whose current jail lacks `/dev/rtkmem`.
pub(crate) fn supported() -> bool {
    super::jail_blocks_native_video()
}

/// Start the one repair attempt this process permits. The token makes the user action originate
/// on the UI thread; it is intentionally not captured by the worker.
pub(crate) fn request(_mt: &MainThread) -> bool {
    if !REPAIR.begin(supported()) {
        return false;
    }
    if !crate::task::spawn_small("jail repair", || {
        let result = repair(
            |payload| call_hbc(payload),
            crate::paths::app_dir(),
            crate::paths::app_id(),
            || device_readable(RTKMEM),
        );
        crate::log(match result {
            Ok(()) => "jail-repair: repaired; close and reopen the app",
            Err(Failure::StartFailed) => "jail-repair: worker start failed; relaunch to try again",
            Err(Failure::HbcUnavailable) => "jail-repair: Homebrew Channel unavailable",
            Err(Failure::NotRoot) => "jail-repair: Homebrew Channel is not root",
            Err(Failure::CommandFailed) => "jail-repair: command failed",
            Err(Failure::Timeout) => {
                "jail-repair: timed out; remote outcome unknown, relaunch to check"
            }
            Err(Failure::Unreadable) => {
                "jail-repair: node still unreadable; close and reopen the app"
            }
            Err(Failure::Unsupported) => "jail-repair: unsupported",
        });
        REPAIR.finish(result);
    }) {
        REPAIR.spawn_refused();
        return false;
    }
    true
}

const RTKMEM: &str = "/dev/rtkmem";

#[cfg(all(not(feature = "hostsim"), not(test)))]
fn call_hbc(payload: &str) -> Result<String, Failure> {
    let registration = super::ls2::register().map_err(|_| Failure::HbcUnavailable)?;
    registration
        .call(EXEC_URI, payload, BUDGET)
        .map_err(|failure| match failure {
            super::ls2::Fail::Timeout => Failure::Timeout,
            super::ls2::Fail::Setup { .. } => Failure::HbcUnavailable,
        })
}

#[cfg(any(feature = "hostsim", test))]
fn call_hbc(_payload: &str) -> Result<String, Failure> {
    Err(Failure::HbcUnavailable)
}

fn device_readable(path: &str) -> bool {
    std::ffi::CString::new(path)
        .map(|p| unsafe { libc::access(p.as_ptr(), libc::R_OK) } == 0)
        .unwrap_or(false)
}

fn valid_id(id: &str) -> bool {
    let Some(suffix) = id.strip_prefix(crate::paths::STABLE_APP_ID) else {
        return false;
    };
    (suffix.is_empty() || (suffix.starts_with('.') && suffix.len() > 1))
        && suffix
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

fn validated_install<'a>(dir: &'a Path, id: &str) -> Option<&'a str> {
    if !valid_id(id) {
        return None;
    }
    let path = dir.to_str()?;
    let dev = format!("/media/developer/apps/usr/palm/applications/{id}");
    let homebrew = format!("/media/cryptofs/apps/usr/palm/applications/{id}");
    (path == dev || path == homebrew).then_some(path)
}

fn command(dir: &Path, id: &str) -> Result<String, Failure> {
    let dir = validated_install(dir, id).ok_or(Failure::Unsupported)?;
    // All interpolated bytes passed the strict id/path grammar above. Single quotes therefore
    // quote data rather than admitting shell syntax.
    Ok(format!(
        "if [ \"$(id -u)\" != 0 ]; then printf '{NOT_ROOT_MARKER}'; elif [ ! -c /dev/rtkmem ]; then exit 74; elif /usr/bin/jailer -t native -p '{dir}' -i '{id}' /bin/true >/dev/null 2>&1 && [ -c '/var/palm/jail/{id}/dev/rtkmem' ]; then printf '{OK_MARKER}'; else exit 74; fi"
    ))
}

fn repair(
    exec: impl FnOnce(&str) -> Result<String, Failure>,
    dir: &Path,
    id: &str,
    readable: impl FnOnce() -> bool,
) -> Result<(), Failure> {
    let command = command(dir, id)?;
    let payload = serde_json::json!({ "command": command }).to_string();
    let reply = exec(&payload)?;
    let value: Value = serde_json::from_str(&reply).map_err(|_| Failure::CommandFailed)?;
    if value.get("returnValue").and_then(Value::as_bool) != Some(true) {
        if value.get("errorCode").and_then(Value::as_i64) == Some(-1)
            && value.get("errorText").and_then(Value::as_str) == Some(HBC_ABSENT_TEXT)
        {
            return Err(Failure::HbcUnavailable);
        }
        return Err(Failure::CommandFailed);
    }
    match value.get("stdoutString").and_then(Value::as_str) {
        Some(OK_MARKER) if readable() => Ok(()),
        Some(OK_MARKER) => Err(Failure::Unreadable),
        Some(NOT_ROOT_MARKER) => Err(Failure::NotRoot),
        _ => Err(Failure::CommandFailed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn dir(id: &str) -> PathBuf {
        PathBuf::from(format!("/media/developer/apps/usr/palm/applications/{id}"))
    }
    fn reply(stdout: &str) -> String {
        serde_json::json!({"returnValue": true, "stdoutString": stdout, "stdoutBytes": "", "stderrString": "", "stderrBytes": ""}).to_string()
    }

    #[test]
    fn only_exact_success_marker_plus_fresh_local_read_is_success() {
        let id = crate::paths::STABLE_APP_ID;
        assert_eq!(
            repair(|_| Ok(reply(OK_MARKER)), &dir(id), id, || true),
            Ok(())
        );
        assert_eq!(
            repair(|_| Ok(reply(OK_MARKER)), &dir(id), id, || false),
            Err(Failure::Unreadable)
        );
        for output in [
            "",
            "ok",
            "PLXNATIVE_JAIL_REPAIR_OK_74\n",
            "PLXNATIVE_JAIL_REPAIR_OK_74 extra",
        ] {
            assert_eq!(
                repair(|_| Ok(reply(output)), &dir(id), id, || true),
                Err(Failure::CommandFailed)
            );
        }
    }

    #[test]
    fn service_errors_malformed_replies_and_no_root_fail_closed() {
        let id = crate::paths::STABLE_APP_ID;
        assert_eq!(
            repair(|_| Err(Failure::Timeout), &dir(id), id, || true),
            Err(Failure::Timeout)
        );
        for response in ["garbage".to_string(), "{}".to_string(), serde_json::json!({"returnValue": false, "errorText": "secret", "stdoutString": OK_MARKER}).to_string()] {
            assert_eq!(repair(|_| Ok(response), &dir(id), id, || true), Err(Failure::CommandFailed));
        }
        let absent = serde_json::json!({
            "returnValue": false,
            "errorCode": -1,
            "errorText": HBC_ABSENT_TEXT,
        })
        .to_string();
        assert_eq!(
            repair(|_| Ok(absent), &dir(id), id, || true),
            Err(Failure::HbcUnavailable)
        );
        let command_error = serde_json::json!({
            "returnValue": false,
            "errorCode": -1,
            "errorText": "Command failed: /usr/bin/jailer",
        })
        .to_string();
        assert_eq!(
            repair(|_| Ok(command_error), &dir(id), id, || true),
            Err(Failure::CommandFailed)
        );
        assert_eq!(
            repair(|_| Ok(reply(NOT_ROOT_MARKER)), &dir(id), id, || true),
            Err(Failure::NotRoot)
        );
    }

    #[test]
    fn unsafe_identity_or_install_path_never_reaches_the_executor() {
        let called = std::cell::Cell::new(false);
        for (path, id) in [
            (dir("com.evil"), "com.evil"),
            (dir("com.beb.plxnative.bad;id"), "com.beb.plxnative.bad;id"),
            (
                PathBuf::from("/tmp/com.beb.plxnative"),
                crate::paths::STABLE_APP_ID,
            ),
            (
                PathBuf::from(
                    "/media/developer/apps/usr/palm/applications/com.beb.plxnative.debug",
                ),
                crate::paths::STABLE_APP_ID,
            ),
        ] {
            assert_eq!(
                repair(
                    |_| {
                        called.set(true);
                        Ok(reply(OK_MARKER))
                    },
                    &path,
                    id,
                    || true
                ),
                Err(Failure::Unsupported)
            );
        }
        assert!(!called.get());
    }

    #[test]
    fn controller_is_single_flight_and_retains_every_terminal_result() {
        let c = Controller::new();
        assert!(c.begin(true));
        assert_eq!(c.snapshot(), State::Running);
        assert!(!c.begin(true));
        c.finish(Err(Failure::Timeout));
        assert_eq!(c.snapshot(), State::Failed(Failure::Timeout));
        assert!(
            !c.begin(true),
            "a timeout must never permit a second remote command"
        );

        let refused = Controller::new();
        assert!(refused.begin(true));
        refused.spawn_refused();
        assert_eq!(refused.snapshot(), State::Failed(Failure::StartFailed));
        assert!(
            !refused.begin(true),
            "a refused spawn still spends the process attempt"
        );

        let unsupported = Controller::new();
        assert!(!unsupported.begin(false));
        assert_eq!(unsupported.snapshot(), State::Failed(Failure::Unsupported));
        assert!(!unsupported.begin(true));

        for result in [
            Ok(()),
            Err(Failure::StartFailed),
            Err(Failure::HbcUnavailable),
            Err(Failure::NotRoot),
            Err(Failure::CommandFailed),
            Err(Failure::Timeout),
            Err(Failure::Unreadable),
        ] {
            let terminal = Controller::new();
            assert!(terminal.begin(true));
            terminal.finish(result);
            assert!(
                !terminal.begin(true),
                "every terminal result must spend the attempt"
            );
        }
    }

    #[test]
    fn command_is_fixed_and_contains_the_only_validated_install_arguments() {
        let id = "com.beb.plxnative.debug-1";
        let c = command(&dir(id), id).unwrap();
        assert!(c.contains("/usr/bin/jailer -t native -p '/media/developer/apps/usr/palm/applications/com.beb.plxnative.debug-1' -i 'com.beb.plxnative.debug-1' /bin/true"));
        assert!(c.contains("id -u"));
        assert!(c.contains("[ ! -c /dev/rtkmem ]"));
        assert!(c.contains("/var/palm/jail/com.beb.plxnative.debug-1/dev/rtkmem"));
    }

    #[test]
    #[cfg(feature = "devtriggers")]
    fn development_probe_accepts_only_the_fixed_id_response_and_cannot_form_a_repair() {
        assert_eq!(probe_result(Ok(reply("0\n"))), ProbeResult::Root);
        assert_eq!(probe_result(Ok(reply("1000\n"))), ProbeResult::NotRoot);
        assert_eq!(probe_result(Ok(reply(OK_MARKER))), ProbeResult::Unavailable);
        assert_eq!(probe_result(Ok(reply("wat\n"))), ProbeResult::Unavailable);
        assert_eq!(probe_result(Ok("{}".into())), ProbeResult::Unavailable);

        let payload = serde_json::json!({ "command": "id -u" }).to_string();
        assert_eq!(
            serde_json::from_str::<Value>(&payload).unwrap()["command"],
            "id -u"
        );
        assert!(!payload.contains("jailer"));
    }
}
