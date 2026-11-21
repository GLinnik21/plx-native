//! The television's own Dolby Vision capability, read once from configd at boot.
//!
//! This is deliberately separate from [`super::probe`]. That probe reads stable nyx files; this
//! value is a service answer whose absence is meaningful and must remain `Unknown`. In particular,
//! an early render-thread read never initializes the cache (`devcaps::dv`'s, which this module
//! publishes into): the worker is the only publisher, and a failed or late answer cannot be
//! mistaken for an affirmative capability.

use plx_os::devcaps::dv::{self, DvCapability, DvProbe, ProbeSource};
use std::sync::OnceLock;
use std::time::Instant;

#[cfg(any(test, all(target_arch = "arm", target_os = "linux")))]
const KEY: &str = dv::CONFIG_KEY;

static STARTED: OnceLock<()> = OnceLock::new();

// The boot worker publishes DvProbe only (`dv::publish_logged`, which every platform's probe ends
// in). Bridge captures the typed probe before present admission and owns the Page damage when this
// diagnostic fact changes. The dev triggers that force the answer (`dvcaps0`, `dvcaps1`) are
// `dv::publish_forced`'s, so a platform other than this one honours them the same way.

/// Start the one boot probe. The registration and its private GLib context are both created and
/// dropped on this worker; no LS2 handle crosses a thread boundary and the app never joins it.
pub fn start_probe() {
    if STARTED.set(()).is_err() {
        return;
    }
    if plx_base::task::spawn("dv-caps-probe", run_probe).is_none() {
        dv::publish_logged(
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
    if dv::publish_forced(started) {
        return;
    }
    run_transport(started);
}

#[cfg(all(
    target_arch = "arm",
    target_os = "linux",
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
            dv::publish_logged(
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
            Ok(capability) => dv::publish_logged(
                platform_gate(
                    DvProbe {
                        capability,
                        source: ProbeSource::Configd,
                        reason: "configd",
                    },
                    plx_os::tv::device::info().major,
                ),
                started,
                None,
                None,
            ),
            Err(failure) => dv::publish_logged(
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
        Err(super::ls2::Fail::Timeout) => dv::publish_logged(
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
        }) => dv::publish_logged(
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
    not(test)
)))]
fn run_transport(started: Instant) {
    dv::publish_logged(
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
    all(target_arch = "arm", target_os = "linux")
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
    all(target_arch = "arm", target_os = "linux")
))]
impl ProbeFailure {
    #[cfg(all(
        target_arch = "arm",
        target_os = "linux",
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

/// **configd's "supported" is not enough on webOS 3.** A Starfish Load carrying our
/// `DolbyHdrInfo` node never starts its clock there: the pipeline takes every access unit and
/// presents none, and the player sits at 0:00. Device-measured 2026-10-14 on an LG OLED C7
/// (webOS 3.9.3, m16p) with a Profile 8.1 film: configd answers `true`, the declared Load
/// wedged, and the same build with the declaration removed played. So a webOS 3 set reports
/// Dolby Vision as unsupported to the route, which plays a cross-compatible base layer (Profile
/// 8.1) as HDR and refuses one that has none (Profile 5) — the same answer an unsupported set
/// gets. `major == 0` is an unknown version and keeps configd's answer; webOS 4+ is untouched.
#[cfg(any(
    test,
    all(target_arch = "arm", target_os = "linux")
))]
fn platform_gate(probe: DvProbe, webos_major: u32) -> DvProbe {
    if probe.capability == DvCapability::Supported && (1..=3).contains(&webos_major) {
        return DvProbe {
            capability: DvCapability::Unsupported,
            source: ProbeSource::Platform,
            reason: "webos3",
        };
    }
    probe
}

/// Strictly grade the one configd shape this decision is allowed to trust. A syntactically valid
/// service refusal is still uncertainty, and a contradictory `missingConfigs` cannot be rescued
/// by a boolean elsewhere in the object.
#[cfg(any(
    test,
    all(target_arch = "arm", target_os = "linux")
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
    use super::{parse_dv_reply, platform_gate, DvCapability, ProbeFailure};
    use plx_os::devcaps::dv::{DvProbe, ProbeSource};

    fn configd(capability: DvCapability) -> DvProbe {
        DvProbe {
            capability,
            source: ProbeSource::Configd,
            reason: "configd",
        }
    }

    /// The C7 (webOS 3.9.3): configd says supported, and a declared Load never starts its clock.
    #[test]
    fn webos3_supported_is_reported_unsupported() {
        let gated = platform_gate(configd(DvCapability::Supported), 3);
        assert_eq!(gated.capability, DvCapability::Unsupported);
        assert_eq!(gated.source, ProbeSource::Platform);
        assert_eq!(gated.provenance(), "webos3");
    }

    #[test]
    fn webos4_and_later_keep_configd() {
        for major in [4, 5, 6, 10, 25] {
            assert_eq!(
                platform_gate(configd(DvCapability::Supported), major),
                configd(DvCapability::Supported)
            );
        }
    }

    #[test]
    fn an_unknown_version_or_a_non_affirmative_answer_is_untouched() {
        assert_eq!(
            platform_gate(configd(DvCapability::Supported), 0),
            configd(DvCapability::Supported)
        );
        assert_eq!(
            platform_gate(configd(DvCapability::Unsupported), 3),
            configd(DvCapability::Unsupported)
        );
        assert_eq!(
            platform_gate(configd(DvCapability::Unknown), 3),
            configd(DvCapability::Unknown)
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
