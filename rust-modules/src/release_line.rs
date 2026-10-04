// Pure helpers for the maintenance-line version rule that `rust-modules/build.rs::emit_version`
// applies at compile time.
//
// These two functions used to live entirely inside `build.rs`, with their own `#[cfg(test)] mod`
// beside them. That test module compiled and ran only if something invoked `rustc --test` on
// `build.rs` directly — cargo never builds a build script as a test target, so neither
// `cargo test --lib` (either feature pass `make check` runs) nor the PR gate ever executed those
// three assertions. The helpers are pure and have nothing host-specific about them (no file I/O,
// no `env!`), so they live here instead, where `cargo test --lib` compiles and runs them for
// real, and `build.rs` pulls in this exact source with `include!` — one definition, verified by
// the gate that was silently missing it, rather than two copies that could drift.
//
// Plain `//` rather than `//!` module docs, deliberately: `build.rs` textually `include!`s this
// file partway through its own `fn main`, where an inner doc comment (`//!`) is a hard error —
// it is only legal at the very start of a file or block. A regular comment compiles fine in
// both places this file is read from.
//
// `version_is_the_package_or_the_next_minor_dev` below exercises the end-to-end behavior (the
// emitted `PLX_VERSION` itself, as `plx_plex::plex::identity::version()` reports it once the
// application has handed it in); the tests above it are the unit-level half. That test lives here
// rather than in `plx_plex` because `PLX_VERSION` and `CARGO_PKG_VERSION` are the application
// crate's, not the layer's.

/// Parse a `RELEASE_LINE` file's content (`"X.Y"`, with or without a trailing newline) into its
/// two integers, or `None` for anything else. Malformed content degrades to "absent" rather than
/// failing the build — this file is hand-edited, and a bad edit should read as trunk, not as a
/// broken build for everyone on the line.
pub(crate) fn parse_release_line(content: &str) -> Option<(u64, u64)> {
    let line = content.trim();
    let (major, minor) = line.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// The next patch after `patch`, or a build failure — the maintenance-line half of
/// `build.rs::emit_version`'s arithmetic, split out so it is one thing to unit-test.
pub(crate) fn dev_patch(patch: u64, pkg: &str) -> u64 {
    patch
        .checked_add(1)
        .unwrap_or_else(|| panic!("Cargo.toml version {pkg:?} has no next patch"))
}

/// Whether `date` is the shape `PLX_NIGHTLY_DATE` must be — exactly 8 ASCII digits (`YYYYMMDD`) —
/// the nightly half of `build.rs::emit_version`'s arithmetic, split out for the same reason
/// `dev_patch` is: `cargo test --lib` runs this, a build script's own `#[cfg(test)]` module never
/// does. Not parsed into a real calendar date on purpose — `build.rs` only ever EMBEDS this string
/// verbatim into `PLX_VERSION`, it never computes with it, so validating the shape is the whole
/// contract and a bad shape (`"2026-09-19"`, `"1"`, empty) is exactly what must fail the build
/// rather than ship a malformed reported version silently.
pub(crate) fn is_nightly_date(date: &str) -> bool {
    date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_shape() {
        assert_eq!(parse_release_line("0.6\n"), Some((0, 6)));
        assert_eq!(parse_release_line("0.6"), Some((0, 6)));
        assert_eq!(parse_release_line("12.34"), Some((12, 34)));
    }

    #[test]
    fn malformed_content_is_absence_not_a_failure() {
        assert_eq!(parse_release_line(""), None);
        assert_eq!(parse_release_line("not-a-version"), None);
        assert_eq!(parse_release_line("0.6.1"), None);
    }

    #[test]
    fn dev_patch_increments() {
        assert_eq!(dev_patch(0, "0.6.0"), 1);
        assert_eq!(dev_patch(9, "0.6.9"), 10);
    }

    #[test]
    fn nightly_date_is_exactly_eight_digits() {
        assert!(is_nightly_date("20260919"));
        assert!(is_nightly_date("00000000"));
        assert!(!is_nightly_date(""));
        assert!(!is_nightly_date("2026919"));  // 7 digits
        assert!(!is_nightly_date("202609190"));  // 9 digits
        assert!(!is_nightly_date("2026-09-19"));  // not digits-only
        assert!(!is_nightly_date("2026091x"));
    }

    /// Read the same `RELEASE_LINE` marker `build.rs::release_line` reads, so this test's
    /// expectation tracks that function's behavior instead of assuming trunk unconditionally.
    /// `None` on trunk (no marker); `Some((major, minor))` on a maintenance branch.
    fn release_line() -> Option<(u32, u32)> {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo = manifest_dir.parent()?;
        let content = std::fs::read_to_string(repo.join("RELEASE_LINE")).ok()?;
        let line = content.trim();
        let (major, minor) = line.split_once('.')?;
        Some((major.parse().ok()?, minor.parse().ok()?))
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
    ///
    /// **On a maintenance line** — a tracked `RELEASE_LINE` marker at the repo root — the next
    /// thing cut from it is a PATCH on the same `major.minor`, never a minor bump the line will
    /// never make (`build.rs::emit_version`'s maintenance-line arm). This checkout carries such a
    /// marker while `release/v0.6` exists, so this test reads it the same way `build.rs` does
    /// rather than assuming trunk's rule unconditionally.
    #[test]
    fn version_is_the_package_or_the_next_minor_dev() {
        let pkg = env!("CARGO_PKG_VERSION");
        plx_plex::plex::identity::set_version(env!("PLX_VERSION"));
        let v = plx_plex::plex::identity::version();
        assert!(plx_plex::plex::identity::user_agent().contains(v));
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
                let expected = match release_line() {
                    Some((line_major, line_minor)) => {
                        assert_eq!(
                            (line_major, line_minor),
                            (n[0], n[1]),
                            "the RELEASE_LINE marker names this checkout's own major.minor"
                        );
                        format!("{}.{}.{}", n[0], n[1], n[2] + 1)
                    }
                    None => format!("{}.{}.0", n[0], n[1] + 1),
                };
                assert_eq!(
                    base,
                    expected,
                    "a developer build on trunk names the next MINOR with the patch reset; on a \
                     RELEASE_LINE maintenance branch it names the next PATCH on that line instead"
                );
            }
        }
    }
}
