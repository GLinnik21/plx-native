//! Who this client says it is — ONE set of constants for both Plex services.
//!
//! There are two independent transports here and they used to each hardcode their own identity:
//! [`account`](super::account) talks to plex.tv over libcurl and sends `X-Plex-*` as HEADERS, while
//! [`client`](super::client) talks to the PMS and sends them as QUERY PARAMETERS. Nothing kept the
//! two in step, and they had drifted on every field that mattered:
//!
//! | field       | plex.tv said        | the PMS said     |
//! |-------------|---------------------|------------------|
//! | Product     | `Plex for webOS`    | `PlxNative`      |
//! | Version     | `1.0`               | `0.1.0`          |
//! | Device      | `LG TV`             | `webOS`          |
//! | Device-Name | `Plex (LG webOS)`   | `Living Room TV` |
//! | Model       | `LG webOS TV`       | `49SM9000PLA`    |
//!
//! Two of those were release blockers rather than untidiness.
//!
//! **`Plex for webOS` is impersonation.** `Plex for <platform>` is Plex's own documented
//! first-party naming pattern (their sample value is `Plex for Roku`), and it appeared verbatim in
//! the authorized-devices list of the account this app signs into — on a platform where an official
//! Plex app exists. A server owner had no way to tell an unofficial client from Plex's own. This is
//! an unofficial client and must say so; that is also what makes the referential use of the Plex
//! name elsewhere defensible.
//!
//! **`49SM9000PLA` is the author's television.** It was accurate for exactly one device on earth
//! and reported as fact by every install.
//!
//! `Living Room TV` was the third: not an impersonation, but a claim about a room this app cannot
//! see, and identical across every install, so two users of the same shared server appeared under
//! one name.
//!
//! The client IDENTIFIER is deliberately not here — it is per-install, not per-build. It is minted
//! once from `/dev/urandom` and persisted by [`session`](super::session).

/// The product name. Unique, and not `Plex …` anything.
pub(crate) const PRODUCT: &str = "PlxNative";

/// The app version, from `Cargo.toml` rather than a literal — `pkg/appinfo.json` (which is the
/// single source for the ipk's version) and this must not be able to disagree, and the two
/// literals this replaces were already stale in opposite directions before the first release.
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) const PLATFORM: &str = "webOS";

/// The OS version. Still a literal: the app is packaged for `>=4.0, <5.0` and nothing reads the
/// real version off the device yet, so this is honest about the target and wrong in the third
/// decimal on a 4.0 or 4.4 panel. An LS2 call would fix it; it is not worth a release blocker.
pub(crate) const PLATFORM_VERSION: &str = "4.5";

/// Device CLASS — what kind of thing this is. Generic on purpose: this app runs on any rooted
/// webOS 4.x panel, not on the model it was developed against.
pub(crate) const DEVICE: &str = "LG webOS TV";
pub(crate) const MODEL: &str = "LG webOS TV";

/// The FRIENDLY name, which is what a user actually reads in plex.tv's device list and in the
/// server's Now Playing. Names the app rather than a room, so it is true on every install and
/// distinguishable from an official client sharing the same TV.
pub(crate) const DEVICE_NAME: &str = "PlxNative (LG TV)";

/// What this client offers the network. It plays; it is not a controller and not a server.
pub(crate) const PROVIDES: &str = "player";

/// The HTTP `User-Agent` for the libcurl transport (plex.tv + discover.provider.plex.tv).
///
/// This was `PlexForWebOS/1.0 (LG webOS)` — the same impersonation as the product string and
/// arguably the worse one, since it is what lands in Plex's own server logs, and it named no
/// version that has ever existed.
pub(crate) fn user_agent() -> String {
    format!("{PRODUCT}/{VERSION} ({DEVICE})")
}

#[cfg(test)]
mod tests {
    /// The point of the module: an unofficial client must not present as a first-party one.
    /// `Plex for <platform>` is Plex's documented pattern for its OWN apps.
    #[test]
    fn identity_never_claims_to_be_plex() {
        for s in [super::PRODUCT, super::DEVICE, super::DEVICE_NAME, super::MODEL, &super::user_agent()] {
            let low = s.to_ascii_lowercase();
            assert!(!low.starts_with("plex for"), "{s:?} uses Plex's own first-party naming pattern");
            assert!(!low.starts_with("plexfor"), "{s:?} uses Plex's own first-party naming pattern");
        }
    }

    /// The version must track the package, not a literal that goes stale the day it is written.
    #[test]
    fn version_comes_from_cargo() {
        assert_eq!(super::VERSION, env!("CARGO_PKG_VERSION"));
        assert!(super::user_agent().contains(super::VERSION));
    }

    /// The developer's own panel must not be reported as every user's hardware.
    #[test]
    fn no_specific_model_is_asserted() {
        for s in [super::DEVICE, super::MODEL, super::DEVICE_NAME] {
            assert!(!s.contains("49SM9000"), "{s:?} names the author's television");
        }
    }
}
