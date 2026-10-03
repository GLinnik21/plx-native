//! The television's own Dolby Vision capability: the type, and the cache the port's boot probe
//! publishes into (`webos::caps`).
//!
//! An early render-thread read never initializes the cache: the port's worker is the only
//! publisher, and a failed or late answer cannot be mistaken for an affirmative capability.

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DvCapability {
    Unknown,
    Supported,
    Unsupported,
}

impl DvCapability {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
        }
    }

    /// The one-word answer for the diagnostics header, in the UI language.
    pub(crate) fn compact_display(self) -> &'static str {
        match self {
            Self::Supported => crate::i18n::msg::browse_diagnostics_dv_yes(),
            Self::Unsupported => crate::i18n::msg::browse_diagnostics_dv_no(),
            Self::Unknown => "?",
        }
    }

    /// [`Self::label`] in the UI language, for the diagnostics read-out. The log keeps `label`.
    pub(crate) fn display(self) -> &'static str {
        match self {
            Self::Supported => crate::i18n::msg::browse_diagnostics_dv_supported(),
            Self::Unsupported => crate::i18n::msg::browse_diagnostics_dv_unsupported(),
            Self::Unknown => crate::i18n::msg::browse_diagnostics_unknown(),
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

    /// Capability and provenance for the screen. The source names (`configd`, `host`) and failure
    /// stages are technical identifiers and stay as written; only the override is a word.
    pub(crate) fn full_state(self) -> String {
        let source = match self.source {
            ProbeSource::Override => crate::i18n::msg::browse_diagnostics_dv_forced(),
            _ => self.provenance(),
        };
        format!("{} · {source}", self.capability.display())
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
pub(crate) fn probe() -> DvProbe {
    RESULT.get()
}

pub(crate) fn capability() -> DvCapability {
    probe().capability
}

/// The boot probe's answer. First write wins, like the `OnceLock` it lands in.
pub(crate) fn publish(probe: DvProbe) {
    RESULT.publish(probe);
}

#[cfg(test)]
mod tests {
    use super::{DvCache, DvCapability, DvProbe, ProbeSource};

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
