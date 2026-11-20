//! The television's own Dolby Vision capability: the type, the cache a platform's boot probe
//! publishes into (the webOS port's is `webos::caps`, the desktop platform's its own), and the dev
//! triggers that force the answer, which every platform's probe honours the same way.
//!
//! An early render-thread read never initializes the cache: the platform's probe is the only
//! publisher, and a failed or late answer cannot be mistaken for an affirmative capability.

use std::sync::OnceLock;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DvCapability {
    Unknown,
    Supported,
    Unsupported,
}

impl DvCapability {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
        }
    }

    /// The one-word answer for the diagnostics header.
    pub fn compact_display(self) -> &'static str {
        match self {
            Self::Supported => crate::i18n::msg::browse_diagnostics_dv_yes(),
            Self::Unsupported => crate::i18n::msg::browse_diagnostics_dv_no(),
            Self::Unknown => "?",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Configd is native-ARM-only; host/release checks still render the other sources.
pub enum ProbeSource {
    Configd,
    /// configd answered, and a platform rule overrode it (`webos::caps::platform_gate`); the
    /// probe's `reason` names the rule.
    Platform,
    Override,
    Host,
    Failure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DvProbe {
    pub capability: DvCapability,
    pub source: ProbeSource,
    pub reason: &'static str,
}

impl DvProbe {
    const PENDING: Self = Self {
        capability: DvCapability::Unknown,
        source: ProbeSource::Failure,
        reason: "pending",
    };

    pub const fn provenance(self) -> &'static str {
        match self.source {
            ProbeSource::Configd => "configd",
            ProbeSource::Override => "forced",
            ProbeSource::Host => "host",
            ProbeSource::Platform | ProbeSource::Failure => self.reason,
        }
    }

    /// Capability and provenance for the screen. The source names (`configd`, `host`) and failure
    /// stages are technical identifiers and stay as written; only the override is a word.
    pub fn full_state(self) -> String {
        let source = match self.source {
            ProbeSource::Override => crate::i18n::msg::browse_diagnostics_dv_forced(),
            _ => self.provenance(),
        };
        format!("{} · {source}", self.capability.label())
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

/// Cached result only. This performs no registration, filesystem access, wait or initialization.
pub fn probe() -> DvProbe {
    RESULT.get()
}

pub fn capability() -> DvCapability {
    probe().capability
}

/// The boot probe's answer. First write wins, like the `OnceLock` it lands in.
pub fn publish(probe: DvProbe) {
    RESULT.publish(probe);
}

/// The configd key the webOS probe asks for, and the one every probe line names.
pub const CONFIG_KEY: &str = "tv.config.supportDolbyHDRContents";

plx_base::devtrig::latched_flag!(
    /// `/tmp/plxnative-dvcaps0` — force the boot's platform answer to unsupported.
    fn forced_unsupported = "dvcaps0";
);

plx_base::devtrig::latched_flag!(
    /// `/tmp/plxnative-dvcaps1` — force the boot's platform answer to supported.
    fn forced_supported = "dvcaps1";
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

/// Publish a probe's answer and log it, one `webos-caps:` line, with the time the probe took. A
/// platform's probe ends in this call whatever it learned, so every answer leaves the same line.
pub fn publish_logged(probe: DvProbe, started: Instant, code: Option<i64>, detail: Option<&str>) {
    publish(probe);
    let elapsed = started.elapsed().as_millis();
    if probe.capability == DvCapability::Unknown {
        let code = code.map(|n| format!(" code={n}")).unwrap_or_default();
        let detail = detail
            .filter(|s| !s.is_empty())
            .map(|s| format!(" detail={s}"))
            .unwrap_or_default();
        plx_base::eventlog::log(&format!(
            "webos-caps: key={CONFIG_KEY} answer=unknown stage={}{}{} elapsed_ms={elapsed}",
            probe.reason, code, detail,
        ));
    } else {
        plx_base::eventlog::log(&format!(
            "webos-caps: key={CONFIG_KEY} answer={} source={} elapsed_ms={elapsed}",
            probe.capability.label(),
            probe.provenance(),
        ));
    }
}

/// The dev triggers' answer, if one is armed: `dvcaps0` forces unsupported and `dvcaps1` supported,
/// `dvcaps0` winning when both are (and saying so). Published and logged like any probe answer, and
/// answers whether it did, so the probe stops there instead of asking its platform.
pub fn publish_forced(started: Instant) -> bool {
    let Some((capability, conflict)) = override_capability(forced_unsupported(), forced_supported()) else {
        return false;
    };
    if conflict {
        plx_base::eventlog::log("webos-caps: dvcaps0 and dvcaps1 both armed; dvcaps0 wins");
    }
    publish_logged(
        DvProbe {
            capability,
            source: ProbeSource::Override,
            reason: if conflict { "override-conflict" } else { "override" },
        },
        started,
        None,
        None,
    );
    true
}

#[cfg(test)]
mod tests {
    use super::{override_capability, DvCache, DvCapability, DvProbe, ProbeSource};

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
    fn dv_caps_getters_are_frame_safe() {
        let cache = DvCache::new();
        let frame = plx_base::task::FrameScope::enter();
        assert_eq!(cache.get().capability, DvCapability::Unknown);
        let _ = super::capability();
        drop(frame);
    }
}
