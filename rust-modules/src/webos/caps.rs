//! The television's own Dolby Vision capability, read once from configd at boot.
//!
//! This is deliberately separate from [`super::probe`]. That probe reads stable nyx files; this
//! value is a service answer whose absence is meaningful and must remain `Unknown`. In particular,
//! an early render-thread read never initializes the cache (`devcaps::dv`'s, which this module
//! publishes into): the worker is the only publisher, and a failed or late answer cannot be
//! mistaken for an affirmative capability.

use crate::devcaps::dv::{DvCapability, DvProbe, ProbeSource};
use std::sync::OnceLock;
use std::time::Instant;

const KEY: &str = "tv.config.supportDolbyHDRContents";

static STARTED: OnceLock<()> = OnceLock::new();

plx_base::devtrig::latched_flag!(
    /// `/tmp/plxnative-dvcaps0` — force the boot's platform answer to unsupported.
    pub(crate) fn forced_unsupported = "dvcaps0";
);

plx_base::devtrig::latched_flag!(
    /// `/tmp/plxnative-dvcaps1` — force the boot's platform answer to supported.
    pub(crate) fn forced_supported = "dvcaps1";
);

fn override_capability(zero: bool, one: bool) -> Option<(DvCapability, bool)> {
    if zero {
        Some((DvCapability::Unsupported, one))
    } else if one {
        Some((DvCapability::Supported, false))
    } else {
        None
    }
}

fn publish(probe: DvProbe, started: Instant, code: Option<i64>, detail: Option<&str>) {
    crate::devcaps::dv::publish(probe);
    let elapsed = started.elapsed().as_millis();
    if probe.capability == DvCapability::Unknown {
        let code = code.map(|n| format!(" code={n}")).unwrap_or_default();
        let detail = detail
            .filter(|s| !s.is_empty())
            .map(|s| format!(" detail={s}"))
            .unwrap_or_default();
        plx_base::eventlog::log(&format!(
            "webos-caps: key={KEY} answer=unknown stage={}{}{} elapsed_ms={elapsed}",
            probe.reason, code, detail,
        ));
    } else {
        plx_base::eventlog::log(&format!(
            "webos-caps: key={KEY} answer={} source={} elapsed_ms={elapsed}",
            probe.capability.label(),
            probe.provenance(),
        ));
    }
    crate::ui::idle::invalidate();
}

/// Start the one boot probe. The registration and its private GLib context are both created and
/// dropped on this worker; no LS2 handle crosses a thread boundary and the app never joins it.
pub(crate) fn start_probe() {
    if STARTED.set(()).is_err() {
        return;
    }
    if plx_base::task::spawn("webos Dolby Vision capability", run_probe).is_none() {
        publish(
            DvProbe {
                capability: DvCapability::Unknown,
                source: ProbeSource::Failure,
                reason: "spawn",
            },
            Instant::now(),
            None,
            None,
        );
    }
}

fn run_probe() {
    let started = Instant::now();
    if let Some((capability, conflict)) =
        override_capability(forced_unsupported(), forced_supported())
    {
        if conflict {
            plx_base::eventlog::log("webos-caps: dvcaps0 and dvcaps1 both armed; dvcaps0 wins");
        }
        publish(
            DvProbe {
                capability,
                source: ProbeSource::Override,
                reason: if conflict {
                    "override-conflict"
                } else {
                    "override"
                },
            },
            started,
            None,
            None,
        );
        return;
    }
    run_transport(started);
}

#[cfg(all(
    target_arch = "arm",
    target_os = "linux",
    not(feature = "hostsim"),
    not(test)
))]
fn run_transport(started: Instant) {
    use std::time::Duration;

    let registration = match super::ls2::register() {
        Ok(registration) => registration,
        Err(super::ls2::RegisterFail::Setup {
            stage,
            detail,
            code,
        }) => {
            publish(
                DvProbe {
                    capability: DvCapability::Unknown,
                    source: ProbeSource::Failure,
                    reason: stage,
                },
                started,
                code.map(i64::from),
                Some(&detail),
            );
            return;
        }
    };
    let reply = registration.call(
        "luna://com.webos.service.config/getConfigs",
        r#"{"configNames":["tv.config.supportDolbyHDRContents"]}"#,
        Duration::from_millis(1500),
    );
    match reply {
        Ok(reply) => match parse_dv_reply(&reply) {
            Ok(capability) => publish(
                DvProbe {
                    capability,
                    source: ProbeSource::Configd,
                    reason: "configd",
                },
                started,
                None,
                None,
            ),
            Err(failure) => publish(
                DvProbe {
                    capability: DvCapability::Unknown,
                    source: ProbeSource::Failure,
                    reason: failure.stage(),
                },
                started,
                reply_error_code(&reply),
                None,
            ),
        },
        Err(super::ls2::Fail::Timeout) => publish(
            DvProbe {
                capability: DvCapability::Unknown,
                source: ProbeSource::Failure,
                reason: "timeout",
            },
            started,
            None,
            None,
        ),
        Err(super::ls2::Fail::Setup {
            stage,
            detail,
            code,
        }) => publish(
            DvProbe {
                capability: DvCapability::Unknown,
                source: ProbeSource::Failure,
                reason: stage,
            },
            started,
            code.map(i64::from),
            Some(&detail),
        ),
    }
}

#[cfg(not(all(
    target_arch = "arm",
    target_os = "linux",
    not(feature = "hostsim"),
    not(test)
)))]
fn run_transport(started: Instant) {
    publish(
        DvProbe {
            capability: DvCapability::Unknown,
            source: ProbeSource::Host,
            reason: "host",
        },
        started,
        None,
        None,
    );
}

#[cfg(all(
    target_arch = "arm",
    target_os = "linux",
    not(feature = "hostsim"),
    not(test)
))]
fn reply_error_code(reply: &str) -> Option<i64> {
    serde_json::from_str::<serde_json::Value>(reply)
        .ok()?
        .as_object()?
        .get("errorCode")?
        .as_i64()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(
    test,
    all(target_arch = "arm", target_os = "linux", not(feature = "hostsim"))
))]
enum ProbeFailure {
    Json,
    ReplyReturnValue,
    MissingKey,
    MissingConfigs,
    ValueType,
}

#[cfg(any(
    test,
    all(target_arch = "arm", target_os = "linux", not(feature = "hostsim"))
))]
impl ProbeFailure {
    #[cfg(all(
        target_arch = "arm",
        target_os = "linux",
        not(feature = "hostsim"),
        not(test)
    ))]
    const fn stage(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::ReplyReturnValue => "reply-returnValue",
            Self::MissingKey => "missing-key",
            Self::MissingConfigs => "missingConfigs",
            Self::ValueType => "value-type",
        }
    }
}

/// Strictly grade the one configd shape this decision is allowed to trust. A syntactically valid
/// service refusal is still uncertainty, and a contradictory `missingConfigs` cannot be rescued
/// by a boolean elsewhere in the object.
#[cfg(any(
    test,
    all(target_arch = "arm", target_os = "linux", not(feature = "hostsim"))
))]
fn parse_dv_reply(reply: &str) -> Result<DvCapability, ProbeFailure> {
    let value: serde_json::Value = serde_json::from_str(reply).map_err(|_| ProbeFailure::Json)?;
    let root = value.as_object().ok_or(ProbeFailure::Json)?;
    if root.get("returnValue").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(ProbeFailure::ReplyReturnValue);
    }
    if let Some(missing) = root.get("missingConfigs") {
        let missing = missing.as_array().ok_or(ProbeFailure::MissingConfigs)?;
        let mut target_missing = false;
        for item in missing {
            let item = item.as_str().ok_or(ProbeFailure::MissingConfigs)?;
            target_missing |= item == KEY;
        }
        if target_missing {
            return Err(ProbeFailure::MissingConfigs);
        }
    }
    let configs = root
        .get("configs")
        .and_then(serde_json::Value::as_object)
        .ok_or(ProbeFailure::MissingKey)?;
    let requested = configs.get(KEY).ok_or(ProbeFailure::MissingKey)?;
    let answer = requested.as_bool().ok_or(ProbeFailure::ValueType)?;
    Ok(if answer {
        DvCapability::Supported
    } else {
        DvCapability::Unsupported
    })
}

#[cfg(test)]
mod tests {
    use super::{override_capability, parse_dv_reply, DvCapability, ProbeFailure};

    #[test]
    fn dv_caps_override_precedence() {
        assert_eq!(override_capability(false, false), None);
        assert_eq!(
            override_capability(true, false),
            Some((DvCapability::Unsupported, false))
        );
        assert_eq!(
            override_capability(false, true),
            Some((DvCapability::Supported, false))
        );
        assert_eq!(
            override_capability(true, true),
            Some((DvCapability::Unsupported, true))
        );
    }

    #[test]
    fn dv_caps_reply_parser_real_shapes() {
        use DvCapability::{Supported, Unsupported};

        assert_eq!(
            parse_dv_reply(
                r#"{"returnValue":true,"configs":{"tv.config.supportDolbyHDRContents":true}}"#
            ),
            Ok(Supported)
        );
        assert_eq!(
            parse_dv_reply(
                r#"{"returnValue":true,"configs":{"tv.config.supportDolbyHDRContents":false}}"#
            ),
            Ok(Unsupported)
        );
        assert_eq!(
            parse_dv_reply(
                r#"{"returnValue":true,"configs":{"tv.config.supportDolbyHDRContents":true},"missingConfigs":[]}"#
            ),
            Ok(Supported)
        );
        assert_eq!(
            parse_dv_reply(
                r#"{"returnValue":true,"configs":{"tv.config.supportDolbyHDRContents":true},"missingConfigs":["unrelated.key"]}"#
            ),
            Ok(Supported)
        );
        assert_eq!(
            parse_dv_reply(
                r#"{"returnValue":true,"configs":{},"missingConfigs":["tv.config.supportDolbyHDRContents"]}"#
            ),
            Err(ProbeFailure::MissingConfigs)
        );
        assert_eq!(
            parse_dv_reply(
                r#"{"returnValue":true,"configs":{"tv.config.supportDolbyHDRContents":true},"missingConfigs":["tv.config.supportDolbyHDRContents"]}"#
            ),
            Err(ProbeFailure::MissingConfigs)
        );
        assert_eq!(
            parse_dv_reply(
                r#"{"returnValue":false,"configs":{"tv.config.supportDolbyHDRContents":true}}"#
            ),
            Err(ProbeFailure::ReplyReturnValue)
        );
        assert_eq!(parse_dv_reply("not json"), Err(ProbeFailure::Json));

        for reply in [
            r#"[]"#,
            r#"{}"#,
            r#"{"returnValue":null,"configs":{"tv.config.supportDolbyHDRContents":true}}"#,
            r#"{"returnValue":"true","configs":{"tv.config.supportDolbyHDRContents":true}}"#,
            r#"{"returnValue":1,"configs":{"tv.config.supportDolbyHDRContents":true}}"#,
        ] {
            assert!(parse_dv_reply(reply).is_err(), "{reply}");
        }
        for reply in [
            r#"{"returnValue":true}"#,
            r#"{"returnValue":true,"configs":null}"#,
            r#"{"returnValue":true,"configs":[]}"#,
            r#"{"returnValue":true,"configs":{}}"#,
        ] {
            assert_eq!(
                parse_dv_reply(reply),
                Err(ProbeFailure::MissingKey),
                "{reply}"
            );
        }
        for reply in [
            r#"{"returnValue":true,"configs":{"tv.config.supportDolbyHDRContents":null}}"#,
            r#"{"returnValue":true,"configs":{"tv.config.supportDolbyHDRContents":"true"}}"#,
            r#"{"returnValue":true,"configs":{"tv.config.supportDolbyHDRContents":1}}"#,
        ] {
            assert_eq!(
                parse_dv_reply(reply),
                Err(ProbeFailure::ValueType),
                "{reply}"
            );
        }
        for reply in [
            r#"{"returnValue":true,"configs":{"tv.config.supportDolbyHDRContents":true},"missingConfigs":null}"#,
            r#"{"returnValue":true,"configs":{"tv.config.supportDolbyHDRContents":true},"missingConfigs":{}}"#,
            r#"{"returnValue":true,"configs":{"tv.config.supportDolbyHDRContents":true},"missingConfigs":[1]}"#,
        ] {
            assert_eq!(
                parse_dv_reply(reply),
                Err(ProbeFailure::MissingConfigs),
                "{reply}"
            );
        }
    }
}
