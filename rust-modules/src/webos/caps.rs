//! The television's own Dolby Vision capability, read once from configd at boot.
//!
//! This is deliberately separate from [`super::probe`]. That probe reads stable nyx files; this
//! value is a service answer whose absence is meaningful and must remain `Unknown`. In particular,
//! an early render-thread read never initializes the cache: the worker is the only publisher, and
//! a failed or late answer cannot be mistaken for an affirmative capability.

use std::sync::OnceLock;
use std::time::Instant;

const KEY: &str = "tv.config.supportDolbyHDRContents";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DvCapability {
    Unknown,
    Supported,
    Unsupported,
}

impl DvCapability {
    pub(crate) const fn compact(self) -> &'static str {
        match self {
            Self::Supported => "yes",
            Self::Unsupported => "no",
            Self::Unknown => "?",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Configd is native-ARM-only; host/release checks still render the other sources.
pub(crate) enum ProbeSource {
    Configd,
    Override,
    Host,
    Failure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DvProbe {
    pub(crate) capability: DvCapability,
    pub(crate) source: ProbeSource,
    pub(crate) reason: &'static str,
}

impl DvProbe {
    const PENDING: Self = Self {
        capability: DvCapability::Unknown,
        source: ProbeSource::Failure,
        reason: "pending",
    };

    pub(crate) const fn provenance(self) -> &'static str {
        match self.source {
            ProbeSource::Configd => "configd",
            ProbeSource::Override => "forced",
            ProbeSource::Host => "host",
            ProbeSource::Failure => self.reason,
        }
    }

    pub(crate) fn full_state(self) -> String {
        format!("{} · {}", self.capability.label(), self.provenance())
    }
}

struct DvCache(OnceLock<DvProbe>);

impl DvCache {
    const fn new() -> Self {
        Self(OnceLock::new())
    }

    fn get(&self) -> DvProbe {
        self.0.get().copied().unwrap_or(DvProbe::PENDING)
    }

    fn publish(&self, probe: DvProbe) {
        let _ = self.0.set(probe);
    }
}

static RESULT: DvCache = DvCache::new();
static STARTED: OnceLock<()> = OnceLock::new();

/// Cached result only. This performs no registration, filesystem access, wait or initialization.
pub(crate) fn probe() -> DvProbe {
    RESULT.get()
}

pub(crate) fn capability() -> DvCapability {
    probe().capability
}

crate::dev::latched_flag!(
    /// `/tmp/plxnative-dvcaps0` — force the boot's platform answer to unsupported.
    pub(crate) fn forced_unsupported = "dvcaps0";
);

crate::dev::latched_flag!(
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
    RESULT.publish(probe);
    let elapsed = started.elapsed().as_millis();
    if probe.capability == DvCapability::Unknown {
        let code = code.map(|n| format!(" code={n}")).unwrap_or_default();
        let detail = detail
            .filter(|s| !s.is_empty())
            .map(|s| format!(" detail={s}"))
            .unwrap_or_default();
        crate::log(&format!(
            "webos-caps: key={KEY} answer=unknown stage={}{}{} elapsed_ms={elapsed}",
            probe.reason, code, detail,
        ));
    } else {
        crate::log(&format!(
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
    if crate::task::spawn("webos Dolby Vision capability", run_probe).is_none() {
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
            crate::log("webos-caps: dvcaps0 and dvcaps1 both armed; dvcaps0 wins");
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
    use super::{
        override_capability, parse_dv_reply, DvCache, DvCapability, DvProbe, ProbeFailure,
        ProbeSource,
    };

    #[test]
    fn early_caps_read_does_not_initialize_cache() {
        let cache = DvCache::new();
        assert_eq!(cache.get().capability, DvCapability::Unknown);
        assert_eq!(cache.get().reason, "pending");
        cache.publish(DvProbe {
            capability: DvCapability::Supported,
            source: ProbeSource::Configd,
            reason: "configd",
        });
        assert_eq!(cache.get().capability, DvCapability::Supported);
    }

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
    fn dv_caps_getters_are_frame_safe() {
        crate::metadata::prewarm_dv_latches();
        let cache = DvCache::new();
        let frame = crate::task::FrameScope::enter();
        assert_eq!(cache.get().capability, DvCapability::Unknown);
        let _ = super::capability();
        let dovi = crate::metadata::Dovi {
            present: true,
            profile: 8,
            bl_compat: 1,
            el_present: false,
            ..crate::metadata::Dovi::NONE
        };
        assert_eq!(
            dovi.presentation(true, DvCapability::Unknown, true),
            crate::metadata::DvPresentation::NotDv,
        );
        drop(frame);
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
