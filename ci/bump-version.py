#!/usr/bin/env python3
"""The ONE place that knows where this project's version is written.

Four files carry it and they must never disagree — `ci/check-package.py` fails the build if they
do, and webosbrew's registry reads the version out of the archive, so a mismatch is a rejected
submission rather than a warning:

    pkg/appinfo.json            the SOURCE. The Makefile derives IPK_VERSION (and so the .ipk
                                filename) from it, and release.yml refuses to publish a tag that
                                disagrees with it.
    ipkroot/ctl/control         Version: — read by opkg and by webosbrew's repogen.
    rust-modules/Cargo.toml     what every version the app REPORTS is derived from —
                                X-Plex-Version, the telemetry release, the diagnostics panel.
                                `rust-modules/build.rs` publishes it as `PLX_VERSION`, exactly
                                for a RELEASE build and as the next MINOR plus `-dev` for
                                everything else, so a working tree stops claiming to be the
                                last release. That suffix exists only in the binary.
    rust-modules/Cargo.lock     cargo would fix this itself on the next build, but leaving it
                                stale means the very next `make` produces a dirty tree — which on
                                a release commit is exactly when you least want one.

Usage:
    ci/bump-version.py --current          print the current version and exit
    ci/bump-version.py patch|minor|major  compute the next version and write it

    ci/bump-version.py 1.2.3              write an explicit version
    ci/bump-version.py patch --dry-run    print what would change, write nothing
    ci/bump-version.py minor --line release/v1.3
                                          a RELEASE bump on a release line (release.yml, rc.yml)

Every release is cut from a release line, `release/vX.Y`, never from `main`
(`.github/workflows/cut-line.yml` makes the line and moves `main` to `X.Y.0`, so `main`'s builds
report the next minor while the line stabilizes). `--line` names the line this checkout is, and
refuses a version that line may not publish (`ci/version_rule.py::line_release_error`): a fresh
line, with no `RELEASE_LINE` marker yet, publishes `X.Y.0`, and this bump writes the marker so every
build of the line after it reports the next patch; a line with the marker publishes `X.Y.N`,
`N >= 1`. Without `--line` the script writes what it is told and nothing else, which is how
cut-line moves `main`.

Exactly three integers, always: LG rejects anything else, and `ci/check-package.py` asserts it.
There is deliberately no pre-release or build-metadata support — `1.0.0-rc1` is not installable
on a webOS TV, so accepting it here would only let it fail later. The `-dev` suffix a developer
build reports is not an exception to that: it is added by `rust-modules/build.rs` to the string
the RUNNING app reports and never written to any of the four files below.
"""
import argparse
import json
import re
import sys
from pathlib import Path

import version_rule

ROOT = Path(__file__).resolve().parent.parent
APPINFO = ROOT / "pkg/appinfo.json"
CONTROL = ROOT / "ipkroot/ctl/control"
CARGO_TOML = ROOT / "rust-modules/Cargo.toml"
CARGO_LOCK = ROOT / "rust-modules/Cargo.lock"
CRATE = "plxnative-modules"
RELEASE_LINE = ROOT / "RELEASE_LINE"

SEMVER = re.compile(r"^(\d+)\.(\d+)\.(\d+)$")


def current() -> str:
    v = json.loads(APPINFO.read_text())["version"]
    if not SEMVER.match(v):
        sys.exit(f"pkg/appinfo.json version {v!r} is not three integers")
    return v


def nextver(cur: str, spec: str) -> str:
    if SEMVER.match(spec):
        return spec
    major, minor, patch = (int(x) for x in cur.split("."))
    if spec == "major":
        return f"{major + 1}.0.0"
    if spec == "minor":
        return f"{major}.{minor + 1}.0"
    if spec == "patch":
        return f"{major}.{minor}.{patch + 1}"
    sys.exit(f"{spec!r} is not patch, minor, major or an X.Y.Z version")


def _sub_once(path: Path, pattern: str, repl: str, text: str) -> str:
    """Substitute exactly one occurrence, or fail loudly — a silent no-op here ships a package
    whose files disagree, and the failure would surface as a rejected submission much later."""
    out, n = re.subn(pattern, repl, text, count=1, flags=re.M)
    if n != 1:
        sys.exit(f"{path.relative_to(ROOT)}: expected 1 version match, found {n}")
    return out


def write(new: str, dry: bool) -> list[str]:
    changed = []
    edits = [
        (APPINFO, r'^(\s*"version"\s*:\s*")\d+\.\d+\.\d+(")', rf"\g<1>{new}\g<2>"),
        (CONTROL, r"^(Version:\s*)\d+\.\d+\.\d+$", rf"\g<1>{new}"),
        (CARGO_TOML, r'^(version\s*=\s*")\d+\.\d+\.\d+(")', rf"\g<1>{new}\g<2>"),
        # Cargo.lock repeats every dependency's version, so anchor on OUR package block only.
        (CARGO_LOCK, rf'^(name = "{CRATE}"\nversion = ")\d+\.\d+\.\d+(")', rf"\g<1>{new}\g<2>"),
    ]
    for path, pat, repl in edits:
        text = path.read_text()
        out = _sub_once(path, pat, repl, text)
        if out != text:
            changed.append(str(path.relative_to(ROOT)))
            if not dry:
                path.write_text(out)
    return changed


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("spec", nargs="?", help="patch | minor | major | X.Y.Z")
    ap.add_argument("--current", action="store_true", help="print the current version and exit")
    ap.add_argument("--dry-run", action="store_true", help="report changes without writing")
    ap.add_argument("--line", help="the release line (release/vX.Y) a release bump is made on")
    a = ap.parse_args()

    cur = current()
    if a.current:
        print(cur)
        return
    if not a.spec:
        ap.error("give patch, minor, major or an explicit X.Y.Z (or --current)")

    new = nextver(cur, a.spec)
    if tuple(map(int, new.split("."))) < tuple(map(int, cur.split("."))):
        # Only BACKWARDS is refused here. Going sideways is legitimate and is in fact the very
        # first release: appinfo.json already says 0.1.0, nothing has ever been tagged, and
        # `--version 0.1.0` means "ship what is already written". Refusing equality made the
        # first release structurally impossible — the guard was pointed at appinfo.json when the
        # thing that must not repeat is a TAG. Whether a version has already shipped is a
        # question about tags, so the workflow asks it there (`git rev-parse refs/tags/vX.Y.Z`),
        # where the answer actually lives.
        sys.exit(f"{new} is lower than the current {cur} — releases only move forwards")

    marker = None
    if a.line:
        marker = RELEASE_LINE.read_text() if RELEASE_LINE.exists() else None
        err = version_rule.line_release_error(new, a.line, marker, cur)
        if err:
            sys.exit(f"refusing {new} on {a.line}: {err}")

    changed = [] if new == cur else write(new, a.dry_run)
    if a.line and marker is None:
        # The line's first release: from this commit on it is a maintenance line, and every build
        # of it reports the next patch (`rust-modules/build.rs`, `ci/version_rule.py`).
        if not a.dry_run:
            RELEASE_LINE.write_text(a.line.removeprefix("release/v") + "\n")
        changed.append(str(RELEASE_LINE.relative_to(ROOT)))
    if not changed:
        print(f"{cur} unchanged — nothing to write")
        return
    verb = "would update" if a.dry_run else "updated"
    print(f"{cur} -> {new}  ({verb} {len(changed)} files)")
    for c in changed:
        print(f"  {c}")


if __name__ == "__main__":
    main()
