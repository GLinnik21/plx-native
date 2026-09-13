//! User-confirmed repair for the k5lp/k3lp Developer Mode jail.
//!
//! No repair runs at boot. PlayerAdapter calls `execute` only after Player.repair accepts
//! explicit confirmation. That app-owned attempt survives every screen and session reset,
//! including timeout: the remote shell may still run after LS2 gives up.

use serde_json::Value;
use std::path::Path;
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
    // The simulator has no LS2 timeout; development fixtures and tests still construct it.
    #[cfg_attr(
        all(feature = "hostsim", not(feature = "devtriggers"), not(test)),
        expect(dead_code)
    )]
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

/// Called only by the PlayerAdapter worker after the owner accepts explicit confirmation.
pub(crate) fn execute() -> Result<(), Failure> {
    repair(
        call_hbc,
        crate::paths::app_dir(),
        crate::paths::app_id(),
        || device_readable(RTKMEM),
    )
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
    fn command_is_fixed_and_contains_the_only_validated_install_arguments() {
        let id = "com.beb.plxnative.debug-1";
        let c = command(&dir(id), id).unwrap();
        assert!(c.contains("/usr/bin/jailer -t native -p '/media/developer/apps/usr/palm/applications/com.beb.plxnative.debug-1' -i 'com.beb.plxnative.debug-1' /bin/true"));
        assert!(c.contains("id -u"));
        assert!(c.contains("[ ! -c /dev/rtkmem ]"));
        assert!(c.contains("/var/palm/jail/com.beb.plxnative.debug-1/dev/rtkmem"));
    }
}
