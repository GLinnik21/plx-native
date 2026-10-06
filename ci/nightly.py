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

Five subcommands, each independently testable as a pure function plus a thin CLI/subprocess shell:

  plan        — today's version/label/tag, the previous nightly tag, and whether to skip.
  notes       — render the release body (markdown, no hard wrapping, absolute links only).
  latest-json — what `plxnative.com/nightly/latest.json` serves; `{"available": false}` if none.
  repo-json   — the Homebrew Channel repository at `plxnative.com/nightly/repo.json`: the index,
                the newest nightly's manifest and a description page; an empty index if none.
  prune       — delete nightly releases (and their tags) older than N days, keeping the newest.

`--selftest` runs the pure-logic tests below `make check` also runs (see `ci/flavor.py` for the
established shape of a same-file selftest); it needs no git repository and no network.
"""
from __future__ import annotations

import argparse
import html
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

#: The Homebrew Channel repository this file generates. A nightly is NOT in the webosbrew
#: catalogue (that listing is keyed to the stable app id), so Homebrew Channel users add this URL
#: under Settings > Add repository instead — `docs/nightly-builds.md` is the guide. The files live
#: on the project's own site because a release asset's URL carries the nightly's tag, which
#: changes every day, while the URL a user typed on a television must not.
SITE_ORIGIN = "https://plxnative.com"
REPO_JSON_URL = f"{SITE_ORIGIN}/nightly/repo.json"
GUIDE_URL = f"{SITE_ORIGIN}/nightly/"
PACKAGE_ID = "com.beb.plxnative.nightly"
#: The manifest `build-package.yml` generates for the nightly package and `publish` attaches to the
#: release under this name; also what is served beside `repo.json`.
MANIFEST_NAME = f"{PACKAGE_ID}.manifest.json"
#: Homebrew Channel shows this where there is no room for the long description (and falls back to
#: it when a package has no `fullDescriptionUrl`); the listing's limit is 80 characters.
SHORT_DESCRIPTION = "Daily build of PlxNative from main. Untested on a TV; installs beside stable."


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
    """The bare `X.Y.Z` a nightly cut today is built from — the number the REPORTED version
    `X.Y.Z-nightly-YYYYMMDD` is made of (see `rust-modules/build.rs::emit_version`). The PACKAGE's
    own version is different on purpose: the same `X.Y` with the cut date as its patch
    (`ci/flavor.py::appinfo_for('nightly')`), so that two nightlies are two versions to Homebrew
    Channel."""
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


def plan_decision(dry: bool, prev_tag: "str | None", changed: "list[str]") -> "tuple[bool, str]":
    """The `(skip, reason)` decision, isolated from git and the clock so `--selftest` can cover it
    directly. `changed` is `git diff --name-only prev_tag..head`'s output, meaningful only when
    `prev_tag` is not `None` and `dry` is false — the two cases that skip ever fires for.

    `dry` (a `pull_request` run, or `workflow_dispatch`'s `dry_run` input) NEVER skips: the whole
    point of a dry run is to prove the pipeline builds, so "nothing changed since the last real
    nightly" — which is exactly what a PR that only touches this workflow itself would see — must
    not be the thing that makes the check disappear silently.
    """
    if dry:
        return False, "dry run (pull_request or dry_run input) — always builds regardless of what changed"
    if prev_tag is None:
        return False, "no previous nightly tag — first nightly"
    if changed_outside_site_docs(changed):
        return False, f"commit(s) outside site/ and docs/ since {prev_tag}"
    return True, f"no commit outside site/ and docs/ since {prev_tag}"


def cmd_plan(date: str, head: str, dry: bool) -> int:
    """Print `$GITHUB_OUTPUT`-shaped `key=value` lines: `version`, `label`, `tag`, `prev_tag`,
    `skip`, `reason`. Refuses (non-zero, `::error::`) if `tag` already exists — same-day rebuilds
    are refused by design; delete the release and its tag first if a genuine re-cut is wanted.

    `dry` skips that refusal too: a dry run (`pull_request`, or `workflow_dispatch`'s `dry_run`)
    never publishes anything, so a tag that happens to already exist for today's real nightly is
    not this run's problem — it is proving the BUILD, not claiming the day.
    """
    if not re.fullmatch(r"\d{8}", date):
        print(f"::error::--date must be YYYYMMDD, got {date!r}", file=sys.stderr)
        return 1

    version = next_nightly_version()
    label = nightly_label(version, date)
    tag = nightly_tag(label)

    if not dry and _git(["tag", "-l", tag]):
        print(f"::error::{tag} already exists — same-day nightly rebuilds are refused by design; "
              "delete the release and the tag first to force a re-cut", file=sys.stderr)
        return 1

    prev_tag = newest_nightly_tag()
    changed = []
    if not dry and prev_tag is not None:
        changed = [p for p in _git(["diff", "--name-only", f"{prev_tag}..{head}"]).splitlines() if p]
    skip, reason = plan_decision(dry, prev_tag, changed)

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
            "## Changes\n\n"
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
        "**With Homebrew Channel:** under Settings, choose Add repository and enter "
        f"`{REPO_JSON_URL}`, then install \"PlxNative Nightly\" from the list. Later nightlies "
        f"appear there as updates. The [guide]({GUIDE_URL}) has the details.\n\n"
        f"**By hand:** download `{ipk}` below and install it with "
        "[dev-manager-desktop](https://github.com/webosbrew/dev-manager-desktop) — no rooted "
        "television is needed. A package installed this way does not update itself; the newest "
        "nightly is always linked from "
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


def _fail(msg: str) -> "SystemExit":
    return SystemExit(f"::error::{msg}")


def repo_manifest(manifest: dict, *, ipk_url: str, ipk_name: str, sha256: str, label: str) -> dict:
    """The manifest Homebrew Channel is pointed at: the one `build-package.yml` generated for the
    package, with `ipkUrl` made absolute. Its bare filename resolves against the manifest's own
    URL, and that is now `plxnative.com`, while the package is a release asset on GitHub.

    Everything Homebrew Channel enforces on the television is re-checked here against facts that
    do not come from the manifest, because a mismatch is invisible until it breaks a user's
    install (the hash is checked on the device and by nothing else):

      * `id` is the nightly app id — a manifest for another id would install over the wrong app.
      * `ipkHash.sha256` equals the `nightly.sha256` published beside the package.
      * `version` is `X.Y.<date>` with the label's own `X.Y` and date. The package version differs
        day to day by design (`ci/flavor.py::appinfo_for`), and a manifest that did not carry
        today's would offer no update, or offer one that installs yesterday's build.
      * `ipkUrl` names the release's own `.ipk` asset (`publish` rewrites it to that name), so the
        manifest and the download it hands the television describe the same file.

    Raises `SystemExit("::error::…")` on a mismatch: serving a manifest that cannot be installed
    is worse than failing the site build, which is visible the same day.
    """
    m = re.fullmatch(r"(\d+)\.(\d+)\.\d+-nightly-(\d{8})", label)
    if not m:
        raise _fail(f"label {label!r} is not X.Y.Z-nightly-YYYYMMDD")
    major, minor, date = m.groups()
    if manifest.get("id") != PACKAGE_ID:
        raise _fail(f"manifest id is {manifest.get('id')!r}, want {PACKAGE_ID!r}")
    want = sha256.split()[0] if sha256.split() else ""
    ipk_hash = manifest.get("ipkHash")
    have = ipk_hash.get("sha256", "") if isinstance(ipk_hash, dict) else ""
    if not want or have != want:
        raise _fail(f"manifest sha256 {have!r} != published nightly.sha256 {want!r}")
    version = str(manifest.get("version", ""))
    if version != f"{major}.{minor}.{date}":
        raise _fail(f"manifest version {version!r} is not {major}.{minor}.{date} "
                    f"(the label's X.Y and cut date)")
    if str(manifest.get("ipkUrl", "")).rsplit("/", 1)[-1] != ipk_name:
        raise _fail(f"manifest ipkUrl {manifest.get('ipkUrl')!r} does not name the release's "
                    f"package {ipk_name!r}")
    return {**manifest, "ipkUrl": ipk_url}


def repo_index(repo: str) -> dict:
    """`repo.json`: the index Homebrew Channel fetches once the user adds the repository. One
    package, pointing at the manifest and the description page staged beside it."""
    return {"packages": [{
        "id": PACKAGE_ID,
        "title": "PlxNative Nightly",
        "shortDescription": SHORT_DESCRIPTION,
        "fullDescriptionUrl": f"{SITE_ORIGIN}/nightly/description.html",
        # The nightly tile's own badged artwork, the one the manifest names too.
        "iconUri": f"https://raw.githubusercontent.com/{repo}/main/pkg/nightly/largeIcon.png",
        "manifestUrl": f"{SITE_ORIGIN}/nightly/{MANIFEST_NAME}",
    }]}


def description_html(*, label: str, sha: str, release_url: str, repo: str) -> str:
    """The long description Homebrew Channel fetches and renders as sanitised HTML — kept to
    `<p>`, `<strong>` and `<a>`, which is all it needs to show the build line and where the
    changes and the guide are. Every interpolated value is escaped."""
    esc = html.escape
    m = re.search(r"-nightly-(\d{8})$", label)
    date = m.group(1) if m else ""
    short = sha[:7]
    return "\n".join([
        "<p><strong>This build has not been tested on a television.</strong> It passed the same "
        "automated build and packaging checks a release does, but nobody has watched it play. "
        "Keep the regular PlxNative installed — this one installs beside it with its own tile "
        "and its own sign-in.</p>",
        f"<p>Build {esc(date)} &middot; version {esc(label)} &middot; commit "
        f'<a href="https://github.com/{esc(repo)}/commit/{esc(sha)}">{esc(short)}</a></p>',
        f'<p><a href="{esc(release_url, quote=True)}">What changed in this build</a> &middot; '
        f'<a href="{GUIDE_URL}">How nightlies work</a> &middot; '
        f'<a href="https://github.com/{esc(repo)}/issues/new">Report a problem</a> (include the '
        "version above).</p>",
        "<p>A new build is published most days that <code>main</code> changes, and appears here as "
        "an update. Builds are deleted after 30 days.</p>",
    ]) + "\n"


def pick_servable_release(releases: "list[dict]") -> "tuple[dict, dict, dict] | None":
    """`(release, sha256 asset, manifest asset)` for the NEWEST nightly that has both, or `None`.

    Newest-first, skipping any release without them rather than stopping at the newest: a Pages
    run that lands while `gh release create` is still uploading, or a nightly published before
    manifests existed, must not blank the repository on every installed television — the previous
    good nightly keeps being served until a newer one is complete. Pure, so `--selftest` covers
    the ordering without `gh`.
    """
    for release in sorted(releases, key=lambda r: r["created_at"], reverse=True):
        sha_asset = next((a for a in release["assets"] if a["name"] == "nightly.sha256"), None)
        manifest_asset = next((a for a in release["assets"] if a["name"] == MANIFEST_NAME), None)
        if sha_asset is not None and manifest_asset is not None:
            return release, sha_asset, manifest_asset
        print(f"::warning::release {release['tag_name']} lacks "
              f"{'nightly.sha256' if sha_asset is None else MANIFEST_NAME} — not served", file=sys.stderr)
    return None


def build_repo_files(release: "dict | None", sha256_text: str, manifest_text: "str | None",
                     repo: str) -> "dict[str, str]":
    """Every file `plxnative.com/nightly/` serves for the Homebrew Channel repository, as
    `{filename: text}`. Pure — takes what `gh api` and the asset downloads already returned, so
    `--selftest` covers it without a network.

    No servable nightly at all: an index with no packages and nothing else, rather than an error.
    The site has to build before the first nightly with a manifest exists, and an empty repository
    is the honest thing to show until then.
    """
    empty = {"repo.json": json.dumps({"packages": []}, indent=2) + "\n"}
    if release is None or manifest_text is None:
        return empty
    tag = release["tag_name"]
    label = tag[len(TAG_PREFIX):]
    ipk_asset = next((a for a in release["assets"] if a["name"].endswith(".ipk")), None)
    if ipk_asset is None:
        raise _fail(f"release {tag} has no .ipk asset")
    manifest = repo_manifest(
        json.loads(manifest_text), ipk_url=ipk_asset["browser_download_url"],
        ipk_name=ipk_asset["name"], sha256=sha256_text, label=label)
    return {
        "repo.json": json.dumps(repo_index(repo), indent=2) + "\n",
        MANIFEST_NAME: json.dumps(manifest, indent=2) + "\n",
        "description.html": description_html(
            label=label, sha=release["target_commitish"], release_url=release["html_url"],
            repo=repo),
    }


def _download(asset: dict) -> str:
    with urllib.request.urlopen(asset["browser_download_url"]) as resp:  # noqa: S310 — public asset
        return resp.read().decode()


def cmd_repo_json(repo: str, out_dir: str) -> int:
    """Write the Homebrew Channel repository for the newest SERVABLE nightly into `out_dir`. Like
    `latest-json`, no nightly (or none with a manifest yet) is exit 0 and an empty index; a
    manifest that DISAGREES with its own release is a failure (see `repo_manifest`)."""
    picked = pick_servable_release(_nightly_releases(repo))
    release = sha_text = manifest_text = None
    if picked is not None:
        release, sha_asset, manifest_asset = picked
        sha_text, manifest_text = _download(sha_asset), _download(manifest_asset)
    files = build_repo_files(release, sha_text or "", manifest_text, repo)
    out = Path(out_dir)
    out.mkdir(parents=True, exist_ok=True)
    for name, text in files.items():
        (out / name).write_text(text)
        print(f"wrote {out / name}")
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

    skip, reason = plan_decision(dry=True, prev_tag="nightly/v0.7.0-nightly-20260918", changed=[])
    check(skip is False and "dry run" in reason,
          "dry=True never skips, even with nothing changed since the previous nightly")
    skip, reason = plan_decision(dry=True, prev_tag=None, changed=["rust-modules/src/app.rs"])
    check(skip is False and "dry run" in reason, "dry=True never skips regardless of prev_tag/changed")
    skip, reason = plan_decision(dry=False, prev_tag=None, changed=[])
    check(skip is False and "first nightly" in reason, "no prev_tag, not dry -> first nightly, never skip")
    skip, reason = plan_decision(dry=False, prev_tag="nightly/v0.7.0-nightly-20260918", changed=["site/index.html"])
    check(skip is True, "not dry, prev_tag set, only site/ changed -> skip")
    skip, reason = plan_decision(dry=False, prev_tag="nightly/v0.7.0-nightly-20260918",
                                  changed=["rust-modules/src/app.rs"])
    check(skip is False, "not dry, prev_tag set, an app file changed -> do not skip")

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
    check("## Changes\n\n" in first_notes, "no prev_tag -> '## Changes' heading, not '## Changes since' with nothing after it")
    check("## Changes since" not in first_notes,
          "no prev_tag -> never renders the dangling '## Changes since' heading")

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

    # Homebrew Channel repository
    label = "0.8.0-nightly-20261006"
    good_manifest = {
        "id": PACKAGE_ID, "version": "0.8.20261006", "type": "native", "title": "PlxNative Nightly",
        "ipkUrl": "plxnative-v0.8.0-nightly-20261006.ipk",
        "ipkHash": {"sha256": "ab" * 32}, "iconUri": "https://example.invalid/i.png",
    }
    ipk_name = "plxnative-v0.8.0-nightly-20261006.ipk"
    ipk_url = "https://github.com/GLinnik21/plx-native/releases/download/x/plxnative-v0.8.0.ipk"
    out = repo_manifest(good_manifest, ipk_url=ipk_url, ipk_name=ipk_name,
                        sha256=("ab" * 32) + "  file.ipk\n", label=label)
    check(out["ipkUrl"] == ipk_url and out["version"] == "0.8.20261006",
          "repo manifest keeps the package's own fields and gets an ABSOLUTE ipkUrl")

    def refuses(manifest: dict, sha: str = "ab" * 32, lbl: str = label) -> bool:
        try:
            repo_manifest(manifest, ipk_url=ipk_url, ipk_name=ipk_name, sha256=sha, label=lbl)
        except SystemExit:
            return True
        return False
    check(refuses({**good_manifest, "id": "com.beb.plxnative"}),
          "a manifest for the STABLE id is refused (it would install over the wrong app)")
    check(refuses(good_manifest, sha="cd" * 32), "a manifest whose sha256 differs from nightly.sha256 is refused")
    check(refuses(good_manifest, sha=""), "an empty published checksum is refused, not matched against")
    check(refuses({**good_manifest, "version": "0.8.0"}),
          "a manifest whose version carries no date (the old per-cycle scheme) is refused")
    check(refuses({**good_manifest, "version": "0.8.20261005"}),
          "a manifest dated for a different day than the release is refused")
    check(refuses(good_manifest, lbl="0.8.0"), "a label without -nightly-YYYYMMDD is refused")
    check(refuses({**good_manifest, "version": "9.9.20261006"}),
          "a manifest with the right date but another major.minor is refused")
    check(refuses({**good_manifest, "ipkUrl": "something-else.ipk"}),
          "a manifest naming a different package file than the release's is refused")
    check(refuses({**good_manifest, "ipkHash": "not-a-dict"}) and refuses({k: v for k, v in good_manifest.items() if k != "ipkHash"}),
          "a missing or malformed ipkHash is refused with an error, not an AttributeError")
    # A maintenance-line label (0.6.2-nightly-D) heads for 0.6.2 but its package is 0.6.<D>:
    # the manifest must carry the label's own X.Y, not trunk's.
    maint = {**good_manifest, "version": "0.6.20261006"}
    check(not refuses(maint, lbl="0.6.2-nightly-20261006"),
          "a maintenance-line nightly's manifest (package 0.6.<date>) matches its 0.6.2 label")
    check(refuses(good_manifest, lbl="0.6.2-nightly-20261006"),
          "a trunk manifest (0.8.<date>) does not match a 0.6.2 maintenance-line label")

    index = repo_index("GLinnik21/plx-native")
    pkg = index["packages"][0]
    check(pkg["id"] == PACKAGE_ID and pkg["manifestUrl"] == f"{SITE_ORIGIN}/nightly/{MANIFEST_NAME}",
          "repo.json names the nightly id and the manifest staged beside it")
    check(len(pkg["shortDescription"]) <= 80, "shortDescription fits the listing's 80-character limit")
    check(pkg["fullDescriptionUrl"].startswith("https://") and pkg["iconUri"].startswith("https://"),
          "repo.json's description and icon URLs are absolute https")

    rel = {**fake_release, "html_url": "https://github.com/GLinnik21/plx-native/releases/tag/x",
           "assets": fake_release["assets"] + [{"name": MANIFEST_NAME,
                                                "browser_download_url": "https://example.invalid/m"}]}
    rel["assets"][0] = {"name": "plxnative-v0.8.0-nightly-20261006.ipk", "browser_download_url": ipk_url}
    rel["tag_name"] = "nightly/v" + label
    files = build_repo_files(rel, ("ab" * 32) + "  x\n", json.dumps(good_manifest), "GLinnik21/plx-native")
    check(set(files) == {"repo.json", MANIFEST_NAME, "description.html"},
          "a nightly with a manifest yields the index, the manifest and the description page")
    check(json.loads(files[MANIFEST_NAME])["ipkUrl"] == ipk_url, "the served manifest points at the release's .ipk")
    check("Build 20261006" in files["description.html"] and "version 0.8.0-nightly-20261006" in files["description.html"]
          and "abc1234" in files["description.html"] and "has not been tested on a television" in files["description.html"],
          "the description page carries the build line, the commit and the untested warning")
    check("<script" not in files["description.html"].lower(), "the description page has no scripts")
    hostile = description_html(label=label, sha="a" * 40, release_url='https://x/"><script>', repo="o/r")
    check("<script>" not in hostile, "description values are HTML-escaped")
    check(build_repo_files(None, "", None, "o/r") == {"repo.json": '{\n  "packages": []\n}\n'},
          "no nightly -> an empty index and nothing else")
    check(set(build_repo_files(rel, "", None, "o/r")) == {"repo.json"},
          "a newest nightly with no manifest asset -> an empty index, not an error")
    rel_no_ipk = {**rel, "assets": [a for a in rel["assets"] if not a["name"].endswith(".ipk")]}
    try:
        build_repo_files(rel_no_ipk, ("ab" * 32), json.dumps(good_manifest), "o/r")
        check(False, "a release with a manifest but no .ipk asset is an error")
    except SystemExit:
        check(True, "a release with a manifest but no .ipk asset is an error")

    # the newest SERVABLE nightly: an incomplete newest release must not blank the repository
    def mk(day: str, with_manifest: bool, with_sha: bool = True) -> dict:
        assets = [{"name": f"plxnative-v0.8.0-nightly-{day}.ipk", "browser_download_url": "https://x/ipk"}]
        if with_sha:
            assets.append({"name": "nightly.sha256", "browser_download_url": "https://x/sha"})
        if with_manifest:
            assets.append({"name": MANIFEST_NAME, "browser_download_url": "https://x/m"})
        return {"tag_name": f"nightly/v0.8.0-nightly-{day}", "created_at": f"2026-10-{day[-2:]}T03:00:00Z",
                "assets": assets}
    complete_old, partial_new = mk("20261005", True), mk("20261006", False)
    picked = pick_servable_release([complete_old, partial_new])
    check(picked is not None and picked[0] is complete_old,
          "a newest release still mid-upload (no manifest) is skipped for the previous complete one")
    picked = pick_servable_release([mk("20261005", True), mk("20261006", True)])
    check(picked is not None and picked[0]["tag_name"].endswith("20261006"),
          "when the newest release is complete it is the one served")
    check(pick_servable_release([mk("20261006", False), mk("20261005", True, with_sha=False)]) is None
          and pick_servable_release([]) is None,
          "no complete nightly at all -> nothing servable (an empty index)")

    # the install text in the notes
    check(REPO_JSON_URL in body and "Add repository" in body and "dev-manager-desktop" in body,
          "notes lead with the Homebrew Channel repository and keep the by-hand route")
    check("not distributed through the Homebrew Channel" not in body,
          "notes no longer claim nightlies are outside the Homebrew Channel")

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
    p_plan.add_argument("--dry", action="store_true",
                         help="never skip and never refuse an existing tag — this run will not publish")

    p_notes = sub.add_parser("notes")
    p_notes.add_argument("--label", required=True)
    p_notes.add_argument("--sha", required=True)
    p_notes.add_argument("--prev-tag", default="")
    p_notes.add_argument("--ipk", required=True)
    p_notes.add_argument("--sha256", required=True)
    p_notes.add_argument("--repo", required=True)

    p_latest = sub.add_parser("latest-json")
    p_latest.add_argument("--repo", required=True)

    p_repo = sub.add_parser("repo-json")
    p_repo.add_argument("--repo", required=True)
    p_repo.add_argument("--out-dir", required=True)

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
        return cmd_plan(args.date, args.head, args.dry)
    if args.cmd == "notes":
        return cmd_notes(args)
    if args.cmd == "latest-json":
        return cmd_latest_json(args.repo)
    if args.cmd == "repo-json":
        return cmd_repo_json(args.repo, args.out_dir)
    if args.cmd == "prune":
        return cmd_prune(args.repo, args.days, args.dry_run)

    ap.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
