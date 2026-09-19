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
// `crate::plex::identity`'s `version_is_the_package_or_the_next_minor_dev` test exercises the
// end-to-end behavior (the emitted `PLX_VERSION` itself); this module is the unit-level half.

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
}
