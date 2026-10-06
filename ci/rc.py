#!/usr/bin/env python3
"""Release candidates: plan the next `rc/vX.Y.Z-rc.N`, and render its prerelease body. Stdlib only;
the one subprocess it runs is `git`, over the checkout the workflow already has.

A release candidate is the STABLE package (`com.beb.plxnative`, the id users install) of a version
that has not shipped yet, built exactly the way `.github/workflows/release.yml` builds the release
and published as a GitHub prerelease. Three things set it apart from the release, and nothing else:

  * the binary REPORTS `X.Y.Z-rc.N` (`rust-modules/build.rs`'s `PLX_CHANNEL=rc` arm, the Makefile's
    `RC=N`), while the package keeps the plain `X.Y.Z` — webOS installs nothing else;
  * its tag lives under `rc/` (`rc/v0.8.0-rc.1`), so release.yml's `push: tags: ['v*']` trigger and
    every `refs/tags/v*` question about shipped releases never see it;
  * it is a prerelease, never `latest`, so the Homebrew Channel — which resolves the manifest
    through `releases/latest` — never offers it, and no manifest is attached at all.

The candidate's tag points at a commit that is NOT on any branch: the version bump the release will
make, committed on top of the line's head and pushed as a tag only. Promoting it is release.yml's
`candidate` input, which refuses unless its own bump produces the identical tree — so the release
ships exactly the source the candidate was tested from.

Subcommands:

  plan   — the candidate number, label, tag, the previous candidate of the same version, and the
           last stable release, as `$GITHUB_OUTPUT` lines.
  notes  — the prerelease body (markdown, no hard wrapping, absolute links only).

`--selftest` runs the pure-logic tests `make check` also runs; it needs no repository and no network.
"""
from __future__ import annotations

import argparse
import re
import subprocess
import sys

#: Every candidate tag carries this prefix — the filter `plan` lists existing candidates with, and
#: the reason a candidate is never mistaken for a release (`v…`) or a nightly (`nightly/v…`).
TAG_PREFIX = "rc/v"

_VERSION = re.compile(r"(\d+)\.(\d+)\.(\d+)")
_LINE = re.compile(r"release/v(\d+)\.(\d+)")


def rc_label(version: str, n: int) -> str:
    return f"{version}-rc.{n}"


def rc_tag(label: str) -> str:
    return f"{TAG_PREFIX}{label}"


def line_rule_error(version: str, line: str) -> "str | None":
    """The version/line rule release.yml's `prepare` enforces, asked BEFORE a candidate is cut so a
    candidate can never be one the release would refuse: trunk (`main`) cuts `X.Y.0`; a maintenance
    line `release/vX.Y` cuts `X.Y.N` with `N >= 1`. `None` when the pair is one release.yml accepts.
    """
    v = _VERSION.fullmatch(version)
    if not v:
        return f"{version!r} is not X.Y.Z"
    major, minor, patch = (int(x) for x in v.groups())
    if line == "main":
        return None if patch == 0 else (
            f"refusing a candidate for {version} from main — trunk releases end in .0; a patch "
            "belongs to its own release/vX.Y line")
    m = _LINE.fullmatch(line)
    if not m:
        return f"line {line!r} is neither main nor release/vX.Y"
    if (int(m.group(1)), int(m.group(2))) != (major, minor) or patch < 1:
        return (f"refusing a candidate for {version} from {line} — a maintenance line publishes "
                f"{m.group(1)}.{m.group(2)}.N with N>=1")
    return None


def next_rc(version: str, existing_tags: "list[str]") -> "tuple[int, str | None]":
    """The next candidate number for `version` and the newest existing candidate tag of that same
    version (or `None`). Numbering is per version and never reuses a number: a run that failed after
    pushing its tag burns that number, the same way a release tag is never re-cut silently.
    """
    pat = re.compile(re.escape(TAG_PREFIX + version) + r"-rc\.([1-9][0-9]*)")
    numbers = sorted(int(m.group(1)) for t in existing_tags if (m := pat.fullmatch(t)))
    if not numbers:
        return 1, None
    return numbers[-1] + 1, rc_tag(rc_label(version, numbers[-1]))


def last_release(release_tags: "list[str]") -> "str | None":
    """The highest plain `vX.Y.Z` tag — the release a candidate's changes are listed against."""
    parsed = [(tuple(int(x) for x in m.groups()), t)
              for t in release_tags if (m := re.fullmatch(r"v(\d+)\.(\d+)\.(\d+)", t))]
    return max(parsed)[1] if parsed else None


def _git(args: "list[str]") -> str:
    return subprocess.run(["git", *args], check=True, capture_output=True, text=True).stdout


def cmd_plan(version: str, line: str, dry: bool) -> int:
    err = line_rule_error(version, line)
    if err and not dry:
        print(f"::error::{err}", file=sys.stderr)
        return 1
    tags = _git(["tag", "--list"]).split()
    if f"v{version}" in tags and not dry:
        print(f"::error::v{version} is already released — a candidate must name a version that has "
              "not shipped yet", file=sys.stderr)
        return 1
    n, prev = next_rc(version, tags)
    label = rc_label(version, n)
    print(f"version={version}")
    print(f"rc={n}")
    print(f"label={label}")
    print(f"tag={rc_tag(label)}")
    print(f"prev_tag={prev or ''}")
    print(f"base_tag={last_release(tags) or ''}")
    return 0


def render_notes(*, label: str, sha: str, base_tag: "str | None", prev_tag: "str | None",
                 ipk: str, sha256: str, repo: str, changes: "list[str]") -> str:
    """The prerelease body. `changes` is the caller's `git log --first-parent --format='- %s'
    base..sha` output (main is squash-only, so each line is a PR title), passed in so this stays a
    pure string transform `--selftest` can exercise.
    """
    version = label.split("-rc.")[0]
    tag = rc_tag(label)
    commit_link = f"https://github.com/{repo}/commit/{sha}"
    if base_tag:
        compare = f"https://github.com/{repo}/compare/{base_tag}...{tag}"
        body = "\n".join(changes) if changes else "No changes outside `site/` and `docs/`."
        changes_section = (f"## Changes since {base_tag}\n\n{body}\n\n"
                           f"[Compare {base_tag}...{label}]({compare})")
    else:
        changes_section = "## Changes\n\nFirst release."
    if prev_tag:
        changes_section += (f" · [Changes since the previous candidate]"
                            f"(https://github.com/{repo}/compare/{prev_tag}...{tag})")

    return "\n\n".join([
        f"Release candidate {label} of PlxNative {version}, built from "
        f"[`{sha[:7]}`]({commit_link}). If nothing turns up, the same source ships as {version}.",

        "**It installs as PlxNative itself, not as a separate app.** It replaces the regular "
        "PlxNative on your television, the same way an update does; PlxNative Nightly and other "
        "separate installs are left alone. It is not offered in the Homebrew Channel, and because "
        f"it carries the package version {version}, the Channel will not offer the final {version} "
        "as an update over it either: reinstall the final release by hand once it is out.",

        changes_section,

        "## Reporting problems\n\n"
        f"[Open an issue](https://github.com/{repo}/issues/new) and include the version string "
        f"`{label}`. If error reporting is enabled, reports from this build are filed under "
        f"`plxnative@{label}`, apart from any release.",

        "## Installing\n\n"
        f"Download `{ipk}` below and install it with "
        "[dev-manager-desktop](https://github.com/webosbrew/dev-manager-desktop).\n\n"
        f"```\n{sha256}  {ipk}\n```\n\n"
        "This package bundles FFmpeg under LGPL-2.1-or-later; the complete corresponding source is "
        "attached below.",
    ])


def cmd_notes(args: argparse.Namespace) -> int:
    changes_raw = _git([
        "log", "--first-parent", "--format=- %s", f"{args.base_tag}..{args.sha}",
        "--", ".", ":!site", ":!docs",
    ]) if args.base_tag else ""
    changes = [line for line in changes_raw.splitlines() if line]
    print(render_notes(
        label=args.label, sha=args.sha, base_tag=args.base_tag or None,
        prev_tag=args.prev_tag or None, ipk=args.ipk, sha256=args.sha256, repo=args.repo,
        changes=changes,
    ))
    return 0


def _selftest() -> int:
    fails = []

    def check(cond: bool, msg: str) -> None:
        print(f"  {'ok  ' if cond else 'FAIL'} — {msg}")
        if not cond:
            fails.append(msg)

    check(rc_tag(rc_label("0.8.0", 2)) == "rc/v0.8.0-rc.2", "label and tag shape")

    check(line_rule_error("0.8.0", "main") is None, "main cuts X.Y.0")
    check(line_rule_error("0.8.1", "main") is not None, "main refuses a patch")
    check(line_rule_error("0.6.2", "release/v0.6") is None, "a line cuts its own X.Y.N, N>=1")
    check(line_rule_error("0.6.0", "release/v0.6") is not None, "a line refuses .0")
    check(line_rule_error("0.7.1", "release/v0.6") is not None, "a line refuses another minor")
    check(line_rule_error("0.8", "main") is not None, "two components is not a version")
    check(line_rule_error("0.8.0", "release/0.8") is not None, "a malformed line name")

    check(next_rc("0.8.0", []) == (1, None), "the first candidate is rc.1")
    tags = ["v0.7.0", "rc/v0.8.0-rc.1", "rc/v0.8.0-rc.2", "rc/v0.7.0-rc.5",
            "nightly/v0.8.0-nightly-20261001", "rc/v0.8.0-rc.10x"]
    check(next_rc("0.8.0", tags) == (3, "rc/v0.8.0-rc.2"),
          "numbering continues per version and ignores other versions and malformed tags")
    check(next_rc("0.8.0", ["rc/v0.8.0-rc.9", "rc/v0.8.0-rc.10"]) == (11, "rc/v0.8.0-rc.10"),
          "numbering is numeric, not lexical")
    check(next_rc("0.8.0", ["rc/v0.8.0-rc.1", "rc/v0.8.00-rc.4"]) == (2, "rc/v0.8.0-rc.1"),
          "another version that shares a prefix is not this one")

    check(last_release(["v0.6.1", "v0.10.0", "v0.9.3", "rc/v0.11.0-rc.1", "nightly/v1.0.0-x"]) == "v0.10.0",
          "the last release is the highest plain vX.Y.Z, compared numerically")
    check(last_release(["rc/v0.1.0-rc.1"]) is None, "no release yet")

    body = render_notes(label="0.8.0-rc.2", sha="abc1234def" + "0" * 30, base_tag="v0.7.0",
                        prev_tag="rc/v0.8.0-rc.1", ipk="plxnative-v0.8.0-rc.2.ipk",
                        sha256="deadbeef" * 8, repo="GLinnik21/plx-native",
                        changes=["- Home: something (#471)"])
    check("replaces the regular PlxNative" in body, "notes say it replaces the stable install")
    check("will not offer the final 0.8.0" in body, "notes warn the final is not offered over it")
    check("/compare/v0.7.0...rc/v0.8.0-rc.2" in body, "notes compare against the last release")
    check("/compare/rc/v0.8.0-rc.1...rc/v0.8.0-rc.2" in body, "notes link the previous candidate")
    check(f"{'deadbeef' * 8}  plxnative-v0.8.0-rc.2.ipk" in body, "notes carry a shasum -c line")
    check("- Home: something (#471)" in body, "notes list the changes")
    first = render_notes(label="0.8.0-rc.1", sha="0" * 40, base_tag=None, prev_tag=None,
                         ipk="x.ipk", sha256="0" * 64, repo="o/r", changes=[])
    check("previous candidate" not in first and "First release." in first,
          "a first candidate with no earlier release renders without either link")

    print()
    for f in fails:
        print(f"::error::{f}")
    return 1 if fails else 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd")

    p_plan = sub.add_parser("plan")
    p_plan.add_argument("--version", required=True)
    p_plan.add_argument("--line", required=True)
    p_plan.add_argument("--dry", action="store_true",
                        help="skip the line rule and the already-released check — this run will not publish")

    p_notes = sub.add_parser("notes")
    p_notes.add_argument("--label", required=True)
    p_notes.add_argument("--sha", required=True)
    p_notes.add_argument("--base-tag", default="")
    p_notes.add_argument("--prev-tag", default="")
    p_notes.add_argument("--ipk", required=True)
    p_notes.add_argument("--sha256", required=True)
    p_notes.add_argument("--repo", required=True)

    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()

    if args.selftest:
        print("== ci/rc.py ==")
        return _selftest()
    if args.cmd == "plan":
        return cmd_plan(args.version, args.line, args.dry)
    if args.cmd == "notes":
        return cmd_notes(args)
    ap.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
