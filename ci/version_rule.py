"""The one "what version comes next" rule, shared by every Python consumer that needs it.

`rust-modules/build.rs::emit_version` (mirrored for tests in `rust-modules/src/release_line.rs`)
is the canonical definition: on trunk, a build that is not `RELEASE=1` reports the next MINOR
after the tracked version, patch reset to zero; on a maintenance line (a tracked `RELEASE_LINE`
marker) it reports the next PATCH on that same line instead.

Three places now need exactly that arithmetic:
  * `rust-modules/build.rs` / `rust-modules/src/release_line.rs` — the Rust definition, which
    computes the `-dev` suffix AND (this change) the nightly `X.Y.Z` a reported
    `X.Y.Z-nightly-YYYYMMDD` is built from.
  * `ci/check-package.py::expected_dev_version` — recomputes the same thing to grade the `-dev`
    string a dev package's binary reports.
  * `ci/flavor.py::appinfo_for` — uses the SAME major.minor as a **nightly package's own**
    `version` field, with the cut date as the patch (`nightly_package_triplet`; never `-dev`: LG's
    installer takes three integers and nothing else). A nightly ipk is the only flavour whose
    *package* version moves.

One Python implementation, imported by the two Python consumers, is what keeps that from
drifting into a fourth copy the way the id and the port already had to be guarded against.
"""
from __future__ import annotations


def parse_release_line(content: str) -> "tuple[int, int] | None":
    """Mirror `rust-modules/src/release_line.rs::parse_release_line` exactly: `"X.Y"` (with or
    without a trailing newline) into its two integers, or `None` for anything else that is not
    that shape — `"0.6.1"` included, since splitting on the FIRST `.` leaves `"6.1"` for the minor
    half and that does not parse as one integer either. Malformed content degrades to "absent"
    (trunk) rather than a build failure, because `RELEASE_LINE` is hand-edited and a bad edit
    should read as trunk for everyone on the checkout, not as a broken gate.
    """
    line = content.strip()
    if "." not in line:
        return None
    major, _, minor = line.partition(".")
    try:
        return int(major), int(minor)
    except ValueError:
        return None


def next_version_triplet(
    tracked_version: str, release_line_content: "str | None"
) -> "tuple[tuple[int, int, int] | None, str | None]":
    """The `(major, minor, patch)` this checkout's tree is heading TOWARDS, or an error.

    Trunk (`release_line_content is None` — no tracked `RELEASE_LINE`, matching
    `release_line()`'s "absent means trunk", which also covers a marker present but malformed)
    is the next MINOR with the patch reset: `(0, 7, 0)` after `0.6.x`, because trunk is where
    features land and the next thing cut from it is a minor, never a patch of a line trunk is not
    on.

    A maintenance line (`RELEASE_LINE` present and parsing as `X.Y`) is instead the next PATCH on
    that same line: `(0, 6, 2)` after `0.6.1`, because trunk's "next minor" question does not
    apply to a line that will never cut one.

    Returns `(triplet_or_None, error_or_None)`. The only error is a MIS-CUT line: `RELEASE_LINE`
    names a `major.minor` that disagrees with `tracked_version`'s — left over from the wrong
    branch, or the version bumped without moving the marker — either way this checkout is not
    actually floating patches for the line it claims to be on, and reporting a plausible-looking
    next version for it would be worse than refusing.
    """
    major, minor, patch = (int(x) for x in tracked_version.split("."))
    line = parse_release_line(release_line_content) if release_line_content is not None else None
    if line is None:
        return (major, minor + 1, 0), None
    line_major, line_minor = line
    if (line_major, line_minor) != (major, minor):
        return None, (
            f"RELEASE_LINE names {line_major}.{line_minor} but the tracked version is at "
            f"{major}.{minor}.{patch} — mis-cut line (RELEASE_LINE's X.Y must equal the tracked "
            "version's major.minor)"
        )
    return (line_major, line_minor, patch + 1), None


def nightly_package_triplet(
    tracked_version: str, release_line_content: "str | None", date: str
) -> "tuple[tuple[int, int, int] | None, str | None]":
    """The `(major, minor, date)` a nightly PACKAGE carries as its own `version`.

    The `major.minor` is `next_version_triplet`'s, so a nightly still sits ahead of the stable it
    was cut after. The PATCH is the cut date read as an integer (`20260919`), which is what makes
    two nightlies different packages: Homebrew Channel decides an update exists by comparing the
    installed `appinfo.json` version with the manifest's as plain strings, so a package version
    that only moved with the tracked release (`0.7.0` for every day of the cycle) would never offer
    an update between two nightlies. It stays three integers because that is all LG's installer
    accepts (`ci/check-package.py` grades exactly that), which is also why the date is not a
    `+suffix` the way the REPORTED `X.Y.Z-nightly-YYYYMMDD` carries it.

    Strictly increasing day over day, and same-day rebuilds are refused by `ci/nightly.py plan`,
    so no two published nightlies share a package version.

    Returns `(triplet_or_None, error_or_None)`: the errors are `next_version_triplet`'s mis-cut
    line, or a `date` that is not exactly 8 ASCII digits.
    """
    if len(date) != 8 or not date.isascii() or not date.isdigit():
        return None, f"nightly date must be exactly 8 digits (YYYYMMDD), got {date!r}"
    triplet, err = next_version_triplet(tracked_version, release_line_content)
    if triplet is None:
        return None, err
    return (triplet[0], triplet[1], int(date)), None
