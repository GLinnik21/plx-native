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

/// The app version, derived from `Cargo.toml` rather than written as a literal — `pkg/appinfo.json`
/// (which is the single source for the ipk's version) and this must not be able to disagree, and
/// the two literals this replaces were already stale in opposite directions before the first
/// release.
///
/// **A release reports the package version exactly; anything else reports the next patch with a
/// `-dev` suffix** — `0.5.0` published, `0.5.1-dev` in the tree. `rust-modules/build.rs` is where
/// that rule is written and why. Every other surface that reports a version — the telemetry
/// release, the lab snapshot, the usage context — reads the same `PLX_VERSION`, so they cannot
/// drift apart.
pub(crate) const VERSION: &str = env!("PLX_VERSION");

pub(crate) const PLATFORM: &str = "webOS";

/// The OS version — the REAL one, read off the set at boot ([`crate::webos`]), because the app
/// runs on webOS 4 through 11 now and a literal is wrong on every set but one. This was
/// `const … = "4.5"` while the app was packaged `>=4.0, <5.0`; the webosbrew reviewer flagged it
/// reporting 4.5 from a 6.5.2 television (issue #22). PMS augments our named Generic profile
/// from the X-Plex-Client-Profile-Extra we send, so the version is informational today — but it
/// is also how a server-side profile could ever distinguish firmware generations, and a false
/// one poisons that forever. The fallback when `os_info.json` is unreadable keeps the literal
/// this replaces — the exact claim every release so far has made — rather than inventing an
/// empty-string case no server has ever been shown. Safe by boot order: `webos::probe()` is the
/// first call in `plex_run`, before SDL exists, so no PMS request can precede the read.
pub(crate) fn platform_version() -> &'static str {
    let r = &crate::webos::info().release;
    if r.is_empty() {
        "4.5"
    } else {
        r
    }
}

/// The UI language inherited by this native process, as a safe BCP-47-shaped tag.
///
/// webOS's authoritative setting is `localeInfo.locales.UI`, but this native app has no LS2
/// settings client. The honest source it already inherits is the process locale environment; the
/// host simulator inherits the same thing from its shell. POSIX precedence is `LC_ALL`, then
/// `LC_MESSAGES`, then `LANG`. If the launcher supplies none (or explicitly supplies the neutral
/// `C`/`POSIX` locale), the identity omits `X-Plex-Language` instead of inventing English.
pub(crate) fn language() -> Option<&'static str> {
    static LANGUAGE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    LANGUAGE
        .get_or_init(|| {
            for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
                let Some(raw) = std::env::var_os(key) else {
                    continue;
                };
                if raw.is_empty() {
                    continue;
                }
                // A present higher-precedence locale decides the answer, including `C` (None).
                return raw.to_str().and_then(locale_language_tag);
            }
            None
        })
        .as_deref()
}

/// POSIX locale (`sr_RS.UTF-8@latin`) to the language tag PMS expects (`sr-RS`). Strict ASCII
/// validation is also the header/query-injection boundary for an inherited environment value.
fn locale_language_tag(raw: &str) -> Option<String> {
    let base = raw.trim().split(['.', '@']).next()?;
    if base.eq_ignore_ascii_case("C") || base.eq_ignore_ascii_case("POSIX") {
        return None;
    }
    let parts: Vec<&str> = base.split(['-', '_']).collect();
    let language = *parts.first()?;
    if !(2..=8).contains(&language.len()) || !language.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }

    let mut out = language.to_ascii_lowercase();
    for part in parts.into_iter().skip(1) {
        if part.is_empty() || part.len() > 8 || !part.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return None;
        }
        out.push('-');
        if part.len() == 4 && part.bytes().all(|b| b.is_ascii_alphabetic()) {
            let mut chars = part.chars();
            out.extend(chars.next()?.to_uppercase());
            out.push_str(&chars.as_str().to_ascii_lowercase());
        } else if part.len() == 2 && part.bytes().all(|b| b.is_ascii_alphabetic()) {
            out.push_str(&part.to_ascii_uppercase());
        } else {
            out.push_str(&part.to_ascii_lowercase());
        }
    }
    Some(out)
}

/// Device CLASS — what kind of thing this is. Generic on purpose: this app runs on any rooted
/// webOS 4.x panel, not on the model it was developed against.
pub(crate) const DEVICE: &str = "LG webOS TV";
pub(crate) const MODEL: &str = "LG webOS TV";

/// Who MADE the hardware. The one field in this file that is a fact about the panel rather than a
/// claim about this app, and it is safe to state because it is not a choice: the binary is
/// cross-compiled for LG's webOS, links LG's `libplayerAPIs`, and starts on nothing else.
///
/// It rides the **plex.tv** headers only. That surface is the account's authorized-device list,
/// where a user picks their television out of a column of them and revokes it — the place the
/// vendor is worth reading. PMS is told what it acts on instead (`Client::playback_identity`), and
/// it acts on the codec profile, not on who built the set. See that method's doc for the split.
pub(crate) const VENDOR: &str = "LG";

/// The FRIENDLY name, which is what a user actually reads in plex.tv's device list and in the
/// server's Now Playing. Names the app rather than a room, so it is true on every install and
/// distinguishable from an official client sharing the same TV.
///
/// **A flavoured install says so.** Two builds can sit on one television now
/// ([`crate::paths::app_id`]) and they hold separate session files, so each mints its own
/// `X-Plex-Client-Identifier` and each appears as its own authorized device. Without the suffix
/// the account grows two entries spelled identically, and revoking "the one on the TV" is a
/// coin flip. The shipped app's name is unchanged, which matters because it is already in every
/// existing user's device list — a rename there would read as a new, unknown device.
pub(crate) fn device_name() -> &'static str {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| match crate::paths::flavour() {
        None => "PlxNative (LG TV)".to_string(),
        Some(f) => format!("PlxNative {f} (LG TV)"),
    })
}

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
        for s in [
            super::PRODUCT,
            super::DEVICE,
            super::device_name(),
            super::MODEL,
            &super::user_agent(),
        ] {
            let low = s.to_ascii_lowercase();
            assert!(
                !low.starts_with("plex for"),
                "{s:?} uses Plex's own first-party naming pattern"
            );
            assert!(
                !low.starts_with("plexfor"),
                "{s:?} uses Plex's own first-party naming pattern"
            );
        }
    }

    /// The version must track the package, not a literal that goes stale the day it is written —
    /// and a build that is NOT a release must not report the published version.
    ///
    /// A release commit leaves every tracked file at the version it just published, so every
    /// developer build after it reported that exact number: to `X-Plex-Version` on the account's
    /// authorized-devices list, to Sentry as `plxnative@X.Y.Z`, and on the diagnostics panel that
    /// is designed to be photographed into a bug report. Nothing downstream could tell the shipped
    /// binary from a working tree. So a non-release build names the version it is working TOWARDS,
    /// suffixed — `0.5.0` published, `0.6.0-dev` in the tree, the next MINOR because trunk is where
    /// features land and a patch release is cut from an existing minor's own line rather than from
    /// here — and that string is produced by `rust-modules/build.rs`, the only place the rule is
    /// written.
    #[test]
    fn version_is_the_package_or_the_next_minor_dev() {
        let pkg = env!("CARGO_PKG_VERSION");
        let v = super::VERSION;
        assert!(super::user_agent().contains(v));
        // The same input `build.rs` decides on: set by the Makefile for `RELEASE=1` and by
        // nothing else, so an ordinary `make check` compiles the developer answer.
        let release = matches!(option_env!("PLX_RELEASE"), Some(s) if !s.is_empty());
        match v.strip_suffix("-dev") {
            None => {
                assert!(release, "a non-release build reports the published version {v:?}");
                assert_eq!(v, pkg, "a release build reports the package version exactly");
            }
            Some(base) => {
                assert!(!release, "a RELEASE build must report {pkg:?} exactly, not {v:?}");
                let n: Vec<u32> = pkg
                    .split('.')
                    .map(|p| p.parse().expect("the package version is three integers"))
                    .collect();
                assert_eq!(n.len(), 3, "the package version is three integers");
                assert_eq!(
                    base,
                    format!("{}.{}.0", n[0], n[1] + 1),
                    "a developer build names the next MINOR with the patch reset — trunk is where \
                     features land, and a patch release is cut from a minor's own line, not here"
                );
            }
        }
    }

    /// The developer's own panel must not be reported as every user's hardware.
    #[test]
    fn no_specific_model_is_asserted() {
        for s in [
            super::DEVICE,
            super::MODEL,
            super::device_name(),
            super::VENDOR,
        ] {
            assert!(
                !s.contains("49SM9000"),
                "{s:?} names the author's television"
            );
        }
    }

    /// The vendor is the panel's, and it is the ONE identity field that is not this app's to
    /// choose — the binary starts on LG's webOS and on nothing else. Pinned so that "make the
    /// identity honest" can never be read as a reason to blank it: an empty header value is a
    /// claim too, and a wrong one.
    #[test]
    fn the_vendor_names_the_hardware_this_binary_runs_on() {
        assert_eq!(super::VENDOR, "LG");
        assert!(!super::VENDOR.to_ascii_lowercase().starts_with("plex"));
    }

    /// The shipped app's device name must not move — it is already in every existing user's
    /// authorized-device list, and a rename there reads as a new, unknown device. A flavoured
    /// install must move, or the two separate sign-ins are indistinguishable in that list.
    #[test]
    fn only_a_flavoured_install_renames_the_device() {
        let name = super::device_name();
        match crate::paths::flavour() {
            None => assert_eq!(name, "PlxNative (LG TV)"),
            Some(f) => assert!(name.contains(f), "{name:?} does not name the {f} install"),
        }
        assert!(name.starts_with("PlxNative"));
    }

    /// On the host there is no `/var/run/nyx/os_info.json`, so this exercises exactly the
    /// unreadable-file path a television would hit: the fallback is the literal every release
    /// so far reported, not an empty string no server has ever been shown. (The real-version
    /// path can only be seen on a set — issue #22's reviewer saw "4.5" from webOS 6.5.2, which
    /// is the bug this function fixes.)
    #[test]
    fn unknown_firmware_falls_back_to_the_old_literal() {
        assert_eq!(super::platform_version(), "4.5");
    }

    #[test]
    fn a_process_locale_becomes_a_safe_plex_language_tag() {
        assert_eq!(
            super::locale_language_tag("en_US.UTF-8"),
            Some("en-US".into())
        );
        assert_eq!(
            super::locale_language_tag("mn_Cyrl_MN.UTF-8"),
            Some("mn-Cyrl-MN".into())
        );
        assert_eq!(super::locale_language_tag("pt-BR"), Some("pt-BR".into()));
        assert_eq!(super::locale_language_tag("C.UTF-8"), None);
        assert_eq!(super::locale_language_tag("POSIX"), None);
        assert_eq!(
            super::locale_language_tag("en_US\r\nX-Plex-Token: stolen"),
            None
        );
    }
}
