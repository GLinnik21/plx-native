#!/usr/bin/env python3
"""One nightly prerelease per UTC day, cut from `main` — plan, notes, the site's "latest nightly"
pointer, and retention. Stdlib only; the only subprocess this file ever shells out to is `git` (for
`plan`, over the checkout it already has) and `gh` (for `latest-json` and `prune`, over the network
the workflow already has a token for).

Nightly is a THIRD install beside stable and debug (`ci/flavor.py`'s `nightly` flavour): its own app
id, its own tile, its own sign-in, always a `RELEASE=1` build, reporting a dated version
(`X.Y.Z-nightly-YYYYMMDD`) rather than claiming to be a real release. This file owns the CI-only
half of that story — which day gets a build, what the release says, where the site finds the latest
one, and when an old one is deleted — none of which the Rust or Makefile side needs to know about.

Four subcommands, each independently testable as a pure function plus a thin CLI/subprocess shell:

  plan        — today's version/label/tag, the previous nightly tag, and whether to skip.
  notes       — render the release body (markdown, no hard wrapping, absolute links only).
  latest-json — what `plxnative.com/nightly/latest.json` serves; `{"available": false}` if none.
  prune       — delete nightly releases (and their tags) older than N days, keeping the newest.

`--selftest` runs the pure-logic tests below `make check` also runs (see `ci/flavor.py` for the
established shape of a same-file selftest); it needs no git repository and no network.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import version_rule  # noqa: E402  — ci/version_rule.py, the shared "next X.Y.Z" arithmetic

ROOT = Path(__file__).resolve().parent.parent

#: Every nightly tag this repository has ever cut carries this prefix, so it is also the filter
#: `latest-json` and `prune` apply to `gh api`'s release list — a canary (`canary/v...`) or a
#: stable release (`v...`) must never be mistaken for a nightly by either of them.
TAG_PREFIX = "nightly/v"


def _release_line_content() -> "str | None":
    """The tracked `RELEASE_LINE` marker's text, or `None` on trunk — same read as
    `ci/flavor.py::_release_line_content`, duplicated rather than imported because importing a
    module for one two-line file read would be the wrong direction of coupling; the two are kept
    in step by `ci/flavor.py`'s own selftest already asserting the shared `version_rule` arithmetic
    this file also relies on.
    """
    p = ROOT / "RELEASE_LINE"
    return p.read_text() if p.is_file() else None


def next_nightly_version() -> str:
    """The bare `X.Y.Z` a nightly cut today is built from — the exact number
    `ci/flavor.py::appinfo_for('nightly')` gives the PACKAGE, before the `-nightly-<date>` suffix
    that only the REPORTED version carries (see `rust-modules/build.rs::emit_version`)."""
    appinfo_version = json.loads((ROOT / "pkg/appinfo.json").read_text())["version"]
    triplet, err = version_rule.next_version_triplet(appinfo_version, _release_line_content())
    if err:
        raise SystemExit(f"::error::{err}")
    return "{}.{}.{}".format(*triplet)


def nightly_label(version: str, date: str) -> str:
    """`X.Y.Z-nightly-YYYYMMDD` — the one string this build reports everywhere (Sentry release,
    `X-Plex-Version`, the diagnostics panel) and the one a bug report names."""
    return f"{version}-nightly-{date}"


def nightly_tag(label: str) -> str:
    return f"{TAG_PREFIX}{label}"


def _run(args: "list[str]") -> str:
    return subprocess.run(args, capture_output=True, text=True, check=True).stdout


def _git(args: "list[str]") -> str:
    return _run(["git", *args]).strip()


def newest_nightly_tag() -> "str | None":
    """The most recently created `nightly/v*` tag, or `None` if this is the first nightly ever.

    By CREATORDATE, not by refname sort order: a tag's name embeds a version, not a date the tags
    themselves are guaranteed to agree on (a maintenance-line nightly's `X.Y.Z` does not compare
    the way a plain string sort would want), and creation order is the one thing that actually
    answers "which nightly came immediately before this one."
    """
    out = _run([
        "git", "for-each-ref", "--sort=-creatordate", "--format=%(refname:short)",
        f"refs/tags/{TAG_PREFIX}*",
    ])
    lines = [line for line in out.splitlines() if line.strip()]
    return lines[0] if lines else None


def changed_outside_site_docs(paths: "list[str]") -> bool:
    """True if any of `paths` (as `git diff --name-only` reports them) lies outside `site/` and
    `docs/` — the two trees a nightly build cannot possibly be affected by. Pure and
    selftest-covered on its own so the `plan` skip decision does not need a repository to test.
    """
    return any(not (p.startswith("site/") or p.startswith("docs/")) for p in paths if p.strip())


def cmd_plan(date: str, head: str) -> int:
    """Print `$GITHUB_OUTPUT`-shaped `key=value` lines: `version`, `label`, `tag`, `prev_tag`,
    `skip`, `reason`. Refuses (non-zero, `::error::`) if `tag` already exists — same-day rebuilds
    are refused by design; delete the release and its tag first if a genuine re-cut is wanted.
    """
    if not re.fullmatch(r"\d{8}", date):
        print(f"::error::--date must be YYYYMMDD, got {date!r}", file=sys.stderr)
        return 1

    version = next_nightly_version()
    label = nightly_label(version, date)
    tag = nightly_tag(label)

    if _git(["tag", "-l", tag]):
        print(f"::error::{tag} already exists — same-day nightly rebuilds are refused by design; "
              "delete the release and the tag first to force a re-cut", file=sys.stderr)
        return 1

    prev_tag = newest_nightly_tag()
    if prev_tag is None:
        skip, reason = False, "no previous nightly tag — first nightly"
    else:
        changed = [p for p in _git(["diff", "--name-only", f"{prev_tag}..{head}"]).splitlines() if p]
        if changed_outside_site_docs(changed):
            skip, reason = False, f"commit(s) outside site/ and docs/ since {prev_tag}"
        else:
            skip, reason = True, f"no commit outside site/ and docs/ since {prev_tag}"

    print(f"version={version}")
    print(f"label={label}")
    print(f"tag={tag}")
    print(f"prev_tag={prev_tag or ''}")
    print(f"skip={'true' if skip else 'false'}")
    print(f"reason={reason}")
    return 0


def render_notes(*, label: str, sha: str, prev_tag: "str | None", ipk: str, sha256: str, repo: str,
                  changes: "list[str]") -> str:
    """The release body. Markdown, no hard wrapping (each sentence stays on one line — a reader's
    browser wraps it, a `sed`-based diff of two notes should not have to fight line breaks that
    carry no meaning), and every link absolute so the body reads the same copied out of the page as
    it does on it.

    `changes` is the caller's `git log --first-parent --format='- %s' prev..sha -- . ':!site'
    ':!docs'` output, one already-formatted `- subject` line per entry (subjects only, because main
    is squash-only so each is a PR title) — passed in rather than shelled out to here so this
    function stays a pure string transform and is what `--selftest` actually exercises.
    """
    short_sha = sha[:7]
    commit_link = f"https://github.com/{repo}/commit/{sha}"
    latest_release_link = f"https://github.com/{repo}/releases/latest"

    if prev_tag:
        prev_label = prev_tag[len(TAG_PREFIX):]
        compare_link = f"https://github.com/{repo}/compare/{prev_tag}...{nightly_tag(label)}"
        changes_body = "\n".join(changes) if changes else "No changes outside `site/` and `docs/`."
        changes_section = (
            f"## Changes since {prev_label}\n\n"
            f"{changes_body}\n\n"
            f"[Compare {prev_label}...{label}]({compare_link}) · "
            f"[Latest stable release]({latest_release_link})"
        )
    else:
        changes_section = (
            "## Changes since\n\n"
            "First nightly.\n\n"
            f"[Latest stable release]({latest_release_link})"
        )

    return "\n\n".join([
        f"Automatic build of `main` at [`{short_sha}`]({commit_link}). It installs beside "
        "PlxNative as a separate app, \"PlxNative Nightly\", with its own launcher tile and its "
        "own sign-in — your regular PlxNative install is untouched.",

        "**This build has not been tested on a television.** It passed the same automated checks "
        "a release does — the ARM cross-build, the packaging gates, and the firmware loader "
        "compatibility check — but nobody has watched it play. Keep the stable app installed.",

        changes_section,

        "## Reporting problems\n\n"
        f"[Open an issue](https://github.com/{repo}/issues/new) and include the version string "
        f"`{label}`. If error reporting is enabled, reports from this build go to the project's "
        "development tracker, kept apart from reports the stable app sends.",

        "## Installing\n\n"
        f"Download `{ipk}` below and install it with "
        "[dev-manager-desktop](https://github.com/webosbrew/dev-manager-desktop) — no rooted "
        "television is needed. Nightly builds are not distributed through the Homebrew Channel and "
        "do not update automatically; the newest one is always linked from "
        "[plxnative.com/nightly/latest.json](https://plxnative.com/nightly/latest.json).\n\n"
        f"```\n{sha256}  {ipk}\n```\n\n"
        "This package bundles FFmpeg under LGPL-2.1-or-later; the complete corresponding source is "
        "attached below. Nightly builds are deleted after 30 days.",
    ])


def cmd_notes(args: argparse.Namespace) -> int:
    changes_raw = _git([
        "log", "--first-parent", "--format=- %s", f"{args.prev_tag}..{args.sha}",
        "--", ".", ":!site", ":!docs",
    ]) if args.prev_tag else ""
    changes = [line for line in changes_raw.splitlines() if line]
    print(render_notes(
        label=args.label, sha=args.sha, prev_tag=args.prev_tag or None,
        ipk=args.ipk, sha256=args.sha256, repo=args.repo, changes=changes,
    ))
    return 0


def _gh_json(args: "list[str]") -> object:
    return json.loads(_run(["gh", *args]))


def _nightly_releases(repo: str) -> "list[dict]":
    releases = _gh_json(["api", f"repos/{repo}/releases", "--paginate"])
    assert isinstance(releases, list)
    return [r for r in releases if r.get("tag_name", "").startswith(TAG_PREFIX)]


def pick_latest_release(releases: "list[dict]") -> "dict | None":
    """The newest nightly release, or `None`. Pure — takes the list `gh api` already returned,
    which is what `--selftest` feeds it with a canned fixture."""
    if not releases:
        return None
    return max(releases, key=lambda r: r["created_at"])


def latest_json_payload(release: dict, sha256: str) -> dict:
    """The `{version, tag, commit, date, ipk_url, sha256, release_url}` shape the site's
    `latest.json` serves, built from one release object (as `gh api` returns it) and the already-
    fetched sha256 text. Pure — split out from `cmd_latest_json` so `--selftest` can cover the
    field mapping without a network call.
    """
    tag = release["tag_name"]
    label = tag[len(TAG_PREFIX):]
    m = re.search(r"-nightly-(\d{8})$", label)
    date = m.group(1) if m else ""
    ipk_asset = next((a for a in release["assets"] if a["name"].endswith(".ipk")), None)
    if ipk_asset is None:
        raise SystemExit(f"::error::release {tag} has no .ipk asset")
    return {
        "version": label,
        "tag": tag,
        "commit": release["target_commitish"],
        "date": date,
        "ipk_url": ipk_asset["browser_download_url"],
        "sha256": sha256.split()[0],
        "release_url": release["html_url"],
    }


def cmd_latest_json(repo: str) -> int:
    """Newest nightly release's public facts, as JSON. `{"available": false}` and exit 0 — never a
    non-zero failure — when there is no nightly yet: the site must build before the first nightly
    exists, and a `latest.json` fetch failing the whole Pages build over that would be backwards.
    """
    releases = _nightly_releases(repo)
    release = pick_latest_release(releases)
    if release is None:
        print(json.dumps({"available": False}))
        return 0
    sha_asset = next((a for a in release["assets"] if a["name"] == "nightly.sha256"), None)
    if sha_asset is None:
        print(f"::warning::release {release['tag_name']} has no nightly.sha256 asset", file=sys.stderr)
        print(json.dumps({"available": False}))
        return 0
    with urllib.request.urlopen(sha_asset["browser_download_url"]) as resp:  # noqa: S310 — public asset
        sha256_text = resp.read().decode()
    print(json.dumps(latest_json_payload(release, sha256_text)))
    return 0


def select_prune_victims(releases: "list[dict]", days: int, now: datetime) -> "list[dict]":
    """Every nightly release older than `days`, except the single newest one — pure, so
    `--selftest` can cover the "always keep at least one" rule without `gh` or the clock.

    Keeping the newest unconditionally matters when nightly stops running for a stretch longer
    than the retention window: without it, a quiet month would prune the last nightly out from
    under `latest.json` and leave the site's link pointing at nothing.
    """
    if not releases:
        return []
    ordered = sorted(releases, key=lambda r: r["created_at"], reverse=True)
    newest, rest = ordered[0], ordered[1:]
    cutoff = now - timedelta(days=days)

    def _created(r: dict) -> datetime:
        return datetime.strptime(r["created_at"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)

    return [r for r in rest if _created(r) < cutoff]


def cmd_prune(repo: str, days: int, dry_run: bool) -> int:
    releases = _nightly_releases(repo)
    victims = select_prune_victims(releases, days, datetime.now(timezone.utc))
    if not victims:
        print(f"nothing to prune ({len(releases)} nightly release(s), none older than {days}d "
              "besides the newest)")
        return 0
    for r in victims:
        tag = r["tag_name"]
        if dry_run:
            print(f"would delete {tag} (created {r['created_at']})")
            continue
        subprocess.run(["gh", "release", "delete", tag, "--repo", repo, "--cleanup-tag", "-y"],
                        check=True)
        print(f"deleted {tag} and its tag")
    return 0


# ---------------------------------------------------------------------------------------------
# --selftest — the pure functions above, exercised with no repository and no network.
# ---------------------------------------------------------------------------------------------

def _selftest() -> int:
    fails = []

    def check(cond: bool, msg: str) -> None:
        print(f"  {'ok  ' if cond else 'FAIL'} — {msg}")
        if not cond:
            fails.append(msg)

    # version/label/tag arithmetic
    check(nightly_label("0.7.0", "20260919") == "0.7.0-nightly-20260919",
          "nightly_label joins version and date with -nightly-")
    check(nightly_tag("0.7.0-nightly-20260919") == "nightly/v0.7.0-nightly-20260919",
          "nightly_tag prefixes nightly/v")

    # the skip decision's pure half
    check(changed_outside_site_docs(["site/index.html", "docs/foo.md"]) is False,
          "site/ and docs/ only -> not outside")
    check(changed_outside_site_docs(["rust-modules/src/app.rs"]) is True,
          "an app file -> outside")
    check(changed_outside_site_docs([]) is False, "no changes -> not outside (skip)")
    check(changed_outside_site_docs(["site-plan.md"]) is True,
          "a look-alike path outside site/ (no trailing slash boundary) still counts as outside")

    # notes rendering
    body = render_notes(
        label="0.7.0-nightly-20260919", sha="abc1234def5678900000000000000000000000",
        prev_tag="nightly/v0.7.0-nightly-20260918", ipk="plxnative-v0.7.0-nightly-20260919.ipk",
        sha256="deadbeef" * 8, repo="GLinnik21/plx-native",
        changes=["- session: one in-memory cache owned by the session module (#136)"],
    )
    check("PlxNative Nightly" in body, "notes name the nightly tile")
    check("**This build has not been tested on a television.**" in body,
          "notes carry the untested-on-TV warning in bold")
    check("## Changes since 0.7.0-nightly-20260918" in body,
          "notes' changes header names the previous label, not the tag")
    check("session: one in-memory cache" in body, "notes carry the passed-in change lines")
    check("compare/nightly/v0.7.0-nightly-20260918...nightly/v0.7.0-nightly-20260919" in body,
          "notes link the compare view between the two tags")
    check(f"deadbeef{'deadbeef' * 7}  plxnative-v0.7.0-nightly-20260919.ipk" in body,
          "notes carry the sha256 code block with the exact ipk name")
    check("LGPL-2.1-or-later" in body and "deleted after 30 days" in body,
          "notes carry the LGPL notice and the retention promise")
    check("\n" not in body.split("## Changes since")[0].strip().split("\n\n")[0],
          "the first paragraph is not internally hard-wrapped")
    first_notes = render_notes(
        label="0.7.0-nightly-20260919", sha="0" * 40, prev_tag=None,
        ipk="plxnative-v0.7.0-nightly-20260919.ipk", sha256="0" * 64, repo="GLinnik21/plx-native",
        changes=[],
    )
    check("First nightly." in first_notes, "no prev_tag -> 'First nightly.'")

    # latest-json field mapping
    fake_release = {
        "tag_name": "nightly/v0.7.0-nightly-20260919",
        "target_commitish": "abc1234def5678900000000000000000000000",
        "html_url": "https://github.com/GLinnik21/plx-native/releases/tag/nightly%2Fv0.7.0-nightly-20260919",
        "assets": [
            {"name": "plxnative-v0.7.0-nightly-20260919.ipk",
             "browser_download_url": "https://example.invalid/ipk"},
            {"name": "nightly.sha256", "browser_download_url": "https://example.invalid/sha256"},
        ],
    }
    payload = latest_json_payload(fake_release, "deadbeef  plxnative-v0.7.0-nightly-20260919.ipk\n")
    check(payload["version"] == "0.7.0-nightly-20260919", "latest.json version is the bare label")
    check(payload["tag"] == "nightly/v0.7.0-nightly-20260919", "latest.json tag is the full ref")
    check(payload["date"] == "20260919", "latest.json date is pulled from the label")
    check(payload["sha256"] == "deadbeef", "latest.json sha256 is the hash only, not the filename")
    check(payload["ipk_url"] == "https://example.invalid/ipk", "latest.json ipk_url is the asset url")
    check(pick_latest_release([]) is None, "pick_latest_release([]) is None")
    older = {**fake_release, "tag_name": "nightly/v0.7.0-nightly-20260918", "created_at": "2026-09-18T03:00:00Z"}
    newer = {**fake_release, "created_at": "2026-09-19T03:00:00Z"}
    check(pick_latest_release([older, newer]) is newer,
          "pick_latest_release picks the newer created_at")

    # prune selection
    now = datetime(2026, 9, 19, tzinfo=timezone.utc)
    releases = [
        {"tag_name": "nightly/v0.7.0-nightly-20260919", "created_at": "2026-09-19T03:00:00Z"},
        {"tag_name": "nightly/v0.7.0-nightly-20260910", "created_at": "2026-09-10T03:00:00Z"},
        {"tag_name": "nightly/v0.7.0-nightly-20260801", "created_at": "2026-08-01T03:00:00Z"},
    ]
    victims = select_prune_victims(releases, 30, now)
    check([v["tag_name"] for v in victims] == ["nightly/v0.7.0-nightly-20260801"],
          "prune keeps everything within 30 days and the newest regardless")
    only_one = [releases[2]]
    check(select_prune_victims(only_one, 30, now) == [],
          "prune never deletes the last remaining nightly, however old")
    check(select_prune_victims([], 30, now) == [], "prune of an empty list deletes nothing")

    print()
    for f in fails:
        print(f"::error::{f}")
    return 1 if fails else 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd")

    p_plan = sub.add_parser("plan")
    p_plan.add_argument("--date", required=True)
    p_plan.add_argument("--head", required=True)

    p_notes = sub.add_parser("notes")
    p_notes.add_argument("--label", required=True)
    p_notes.add_argument("--sha", required=True)
    p_notes.add_argument("--prev-tag", default="")
    p_notes.add_argument("--ipk", required=True)
    p_notes.add_argument("--sha256", required=True)
    p_notes.add_argument("--repo", required=True)

    p_latest = sub.add_parser("latest-json")
    p_latest.add_argument("--repo", required=True)

    p_prune = sub.add_parser("prune")
    p_prune.add_argument("--repo", required=True)
    p_prune.add_argument("--days", type=int, default=30)
    p_prune.add_argument("--dry-run", action="store_true")

    ap.add_argument("--selftest", action="store_true")

    args = ap.parse_args()

    if args.selftest:
        print("== ci/nightly.py ==")
        return _selftest()

    if args.cmd == "plan":
        return cmd_plan(args.date, args.head)
    if args.cmd == "notes":
        return cmd_notes(args)
    if args.cmd == "latest-json":
        return cmd_latest_json(args.repo)
    if args.cmd == "prune":
        return cmd_prune(args.repo, args.days, args.dry_run)

    ap.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
