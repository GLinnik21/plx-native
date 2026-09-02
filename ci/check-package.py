#!/usr/bin/env python3
"""Packaging-metadata assertions. Stdlib only, no NDK, no TV — runs anywhere.

The registry reads metadata straight out of the .ipk (webosbrew's repogen/ipk_file.py reads
Package/Version/Installed-Size from the control file, then appinfo.json), so any disagreement
between the three places the version is written is a submission failure rather than a warning.
"""
import json
import re
import struct
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import flavor  # noqa: E402  — ci/flavor.py, which DECIDES a flavour's id and title

ROOT = Path(__file__).resolve().parent.parent
FAILURES: list[str] = []

def check(cond: bool, msg: str) -> None:
    if cond:
        print(f"  ok — {msg}")
    else:
        FAILURES.append(msg)
        print(f"  FAIL — {msg}")


def build_configuration(stamp: str) -> "str | None":
    """Decode `pkg/.build-config` into "dev", "release", or None for anything else.

    THE FEATURE FLAGS ARE ONE FIELD OF SEVERAL, and reading the stamp as a whole string is what
    silently switched every gate that depends on this off. `RUST_CFG` is `features:$(RUST_FEATFLAGS)`,
    then `+symbols` when SYMBOLS=1, then always `+tel:<hash>` — so the real stamp for an ordinary
    dev build is `features:+tel:98c4b7d3`, which equals neither of the two literals this was once
    written against. It matched when it was written; the telemetry field was added later, and from
    that day the answer was None for EVERY build this project makes. Nothing failed — the callers
    print "SKIP — neither shipped configuration" and move on — so the dev-trigger gate, the
    dev-only-library gate and the reported-version gate all graded nothing on every CI run. Exactly
    the defect class DEV_WITNESS is commented against, a witness that cannot fail, reached by
    another route. The release cut carries `SYMBOLS=1`, which adds a THIRD field, so repairing the
    two literals by hand would have left the release job ungraded anyway.

    Only the FEATURE half is decoded, and matched WHOLE rather than by substring: the "shots"
    recipe in the Makefile's header is `--no-default-features --features devtriggers`, which a
    substring test would grade as a release build and then fail for carrying exactly the surface it
    asked for. `+tel:` and `+symbols` are optional so a stamp written by an older Makefile still
    decodes, and the feature flags are lazy so they cannot swallow a trailing field.
    """
    fields = re.fullmatch(r"features:(?P<flags>.*?)(?:\+symbols)?(?:\+tel:[0-9a-f]+)?", stamp.strip())
    if not fields:
        return None
    return {"": "dev", "--no-default-features": "release"}.get(fields.group("flags").strip())


def _selftest() -> int:
    """Prove the decoder against every stamp the Makefile can actually write.

    It is here rather than in a comment because this function has already been wrong for months
    without anything going red, and because the stamps it must decode are produced by make
    variables that no Python test can otherwise see. `make check` runs it, beside `flavor.py`'s.
    """
    cases = {
        # what the Makefile writes today, per documented configuration
        "features:+tel:98c4b7d37a4c": "dev",
        "features:--no-default-features+tel:98c4b7d37a4c": "release",
        "features:--no-default-features+symbols+tel:98c4b7d37a4c": "release",   # the release cut
        "features:+symbols+tel:98c4b7d37a4c": "dev",
        # older stamps, from before the telemetry and symbols fields existed
        "features:": "dev",
        "features:--no-default-features": "release",
        # configurations that are neither shipped one, and must SAY so rather than be graded
        "features: --features lab-diagnostics+tel:abc123def456": None,          # LAB=1
        "features:--no-default-features --features lab-diagnostics+tel:abc123def456": None,
        "features:--no-default-features --features devtriggers+tel:abc123def456": None,  # shots
        # nothing to read
        "": None,
        "garbage": None,
    }
    bad = 0
    for stamp, want in cases.items():
        got = build_configuration(stamp)
        if got != want:
            bad += 1
            print(f"  FAIL — {stamp!r} decoded {got!r}, want {want!r}")
    print(f"check-package: build_configuration {len(cases) - bad}/{len(cases)} stamps correct")
    return 1 if bad else 0


if "--selftest" in sys.argv:
    sys.exit(_selftest())

# ---- the two release documents -----------------------------------------------------------------
#
# `docs/release-notes/vX.Y.Z.md` is the body CI publishes, written for a television owner.
# `docs/release-audits/vX.Y.Z.md` is the evidence, written for a reviewer and completed by
# `ci/gen-release-audit.py` from the artifact. The standards are the README in each directory; the
# checks below are the half a command can decide, which is where every check in this project lives
# — a checklist is only as good as the person following it, and the release that skipped every gate
# skipped them BECAUSE it was done by hand.

# The split landed 2026-08-29, and on the same day v0.1.0 through v0.5.0 were rewritten to it as a
# deliberate, dated exercise — so there is no historical exemption and every check below applies to
# every note and every audit in the two directories. The gate that used to sit here (a version
# floor, skipping the structural checks for pre-standard notes) is gone rather than left at a value
# nothing can reach: an exemption nobody can trip is one nobody maintains, and the next standard
# change can reintroduce it against the version it actually needs.

SENTINELS = ("__IPK_SHA256__", "__COMMIT__", "__RUN_URL__", "__IPK_SIZE__", "__INSTALLED_SIZE__")
# The one a note cannot do without: it is the only tamper check a user of an unsigned binary has.
REQUIRED_SENTINEL = "__IPK_SHA256__"
FFMPEG_TARBALL_SHA = "7f607a00dd0d28a729d5a4811205812eef01cf6ef6155025febb6f36a9062d52"
AUDIT_BEGIN = "<!-- BEGIN GENERATED"
AUDIT_END = "<!-- END GENERATED -->"
# Sections that were in the note and are now the audit's. Named so a regression to the old shape
# fails rather than being noticed in review, or not.
MOVED_SECTIONS = (
    "## Package facts",
    "## Source for the bundled FFmpeg",
    "## Checking what you downloaded",
    "## Still the same scope",
)
AUTHORED_AUDIT_SECTIONS = (
    "## Device test evidence",
    "## External compatibility reports known at release time",
    "## Compatibility tiers, and what moved",
    "## Known issues at release, with provenance",
    "## Release configuration",
)
# Emoji, not punctuation: the em dash, the ellipsis and the arrow this repo writes everywhere are
# deliberately outside these ranges.
EMOJI = re.compile("[\U0001F000-\U0001FAFF\u2600-\u27BF\u2B00-\u2BFF\uFE0F]")
MD_LINK = re.compile(r"\[[^\]]*\]\(([^)]+)\)")
# A GitHub @mention. Anchored so an email address (`a@users.noreply.github.com`) and a path-like
# `@rpath` are not mentions; a bare `@name` at a word boundary is.
MENTION = re.compile(r"(?<![A-Za-z0-9._%+/-])@([A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?)\b")


# ---- which webOS versions a release document may name ------------------------------------------
#
# THE "webOS 26" GATE. A published note once asserted support for a webOS 26 that does not exist,
# and until 2026-08-29 this was a plain `version in evidence` substring test over the text of
# `fwcompat.py` and `webos5-port.md`. **That test could not catch its own motivating defect**: both
# documents are full of dates, and `"26" in "2026-08-11"` is True. Widening the corpus made it
# worse rather than better — `docs/webos10-lab-report.md` quotes, in order to refute it, somebody
# else's gloss "= webOS 25 and 26", so the corpus contains the exact string being guarded against.
#
# So the evidence is ENUMERATED instead, and a named version is accepted only when it is a
# component-wise prefix of something in it: `webOS 10` passes on 10.2.0, `webOS 4.5` passes as LG's
# marketing number, `webOS 26` and `webOS 25` fail.
#
# This list is a transcription and transcriptions rot in this repo — so the failure message names
# the authority rather than this file. `tools/fwcompat.py` grades against a downloaded inventory
# and prints the releases it holds; when it grows one, that output is what corrects this line.
FW_MATRIX = ("1.2.0", "1.4.0", "2.2.3", "3.4.0", "3.9.2", "4.4.2", "4.10.0", "5.3.1",
             "6.4.0", "7.4.0", "8.3.0", "9.2.0", "10.2.0", "11.2.0")
# Real numbers outside that matrix, each with the evidence that makes it real.
FW_EXTRA = {
    "4.5": "LG's marketing number for the dev set's generation",
    "4.10.2": "what the dev television itself reports (docs/webos5-port.md)",
    "6.5.2": "the reviewer's set in issue #22",
    "10.3.1": "the rented Cloud Lab set (docs/webos10-resource-allocation.md)",
}


def unknown_firmwares(text: str) -> list:
    """Versions named after the literal "webOS " that this repo has no evidence for.

    Two ways to be known, because the notes legitimately use both. A version is evidence-backed
    when it is a component-wise PREFIX of a release in the lists above (`webOS 10` on 10.2.0), or
    when it is a GENERATION BOUNDARY — `N.0` for a major that exists — which is how a range is
    written: "every other firmware from webOS 4.0 up", "webOS 5.0 replaced the library that binds
    the video plane". Neither admits a major nobody has: 25 and 26 fail on both.
    """
    known = tuple(FW_MATRIX) + tuple(FW_EXTRA)
    majors = {k.split(".")[0] for k in known}
    out = []
    for v in sorted(set(re.findall(r"webOS (\d+(?:\.\d+)*)", text))):
        parts = v.split(".")
        prefix = any(k == v or k.startswith(v + ".") for k in known)
        boundary = parts[0] in majors and all(c == "0" for c in parts[1:])
        if not (prefix or boundary):
            out.append(v)
    return out


def _strip_noise(text: str) -> list:
    """(line number, line) with fenced code and HTML comments removed.

    Both are invisible to the reader — a fence is verbatim and a comment is deleted by the
    renderer — so neither can be hard-wrapped prose and neither carries a link a reader can click.
    """
    out, fence, comment = [], None, False
    for n, line in enumerate(text.splitlines(), start=1):
        s = line.strip()
        if fence is None and s.startswith("```"):
            fence = s[: len(s) - len(s.lstrip("`"))]
            continue
        if fence is not None:
            if s.startswith(fence):
                fence = None
            continue
        if comment:
            if "-->" in line:
                comment = False
            continue
        if s.startswith("<!--"):
            comment = "-->" not in line
            continue
        out.append((n, line))
    return out


# A line that opens a new block rather than continuing the previous one. Everything else following
# a non-blank line is a hard-wrapped continuation.
_BLOCK_START = re.compile(r"^\s*(?:[-*+>|#]|\d+[.)]\s|\[\^|!\[)")


def hard_wrapped(text: str) -> list:
    """Line numbers where a paragraph was split across source lines.

    Release notes are read RENDERED, in a browser, at a width nobody controls, so a hard wrap
    buys nothing and costs a re-flow on every edit. One source line per paragraph or list item.
    """
    lines = _strip_noise(text)
    bad = []
    for (pn, prev), (n, cur) in zip(lines, lines[1:]):
        if n != pn + 1 or not prev.strip() or not cur.strip():
            continue
        if _BLOCK_START.match(cur):
            continue
        bad.append(n)
    return bad


def lint_note(path) -> None:
    """Grade one release note against the standard in `docs/release-notes/README.md`."""
    body = path.read_text()
    missing = REQUIRED_SENTINEL not in body
    check(not missing, f"{path.name} carries the {REQUIRED_SENTINEL} sentinel CI substitutes")
    # A sentinel CI does not know about would publish literally, in the one field a reader checks.
    unknown = sorted({s for s in re.findall(r"__[A-Z0-9_]+__", body) if s not in SENTINELS})
    check(not unknown, f"{path.name} uses only sentinels CI substitutes"
                       + (f" (unknown: {', '.join(unknown)})" if unknown else ""))
    # A literal 64-hex string beside the sentinel is either a stale hash from a previous release or
    # one somebody typed, and both are the defect class this whole standard exists to end.
    stray = [h for h in re.findall(r"\b[0-9a-f]{64}\b", body) if h != FFMPEG_TARBALL_SHA]
    check(not stray, f"no hand-typed package hash in {path.name}"
                     + (f" (found {stray[0][:12]}…)" if stray else ""))
    # A past note asserted support for a "webOS 26" that does not exist. This is that gate.
    unknown_fw = unknown_firmwares(body)
    check(not unknown_fw, f"every webOS version {path.name} names has evidence in the repo"
                          + (f" (no evidence for {', '.join(unknown_fw)})" if unknown_fw else ""))
    wrapped = hard_wrapped(body)
    check(not wrapped, f"{path.name} does not hard-wrap prose (one source line per paragraph)"
                       + (f" (lines {', '.join(map(str, wrapped[:6]))})" if wrapped else ""))
    # A release body is NOT rendered relative to the repository, so a relative link is dead for
    # every reader of the thing this file becomes.
    rel = sorted({u for u in MD_LINK.findall(body)
                  if not u.startswith(("http://", "https://", "#", "mailto:"))})
    check(not rel, f"{path.name} links absolutely (a release body resolves no repo-relative path)"
                   + (f" (relative: {', '.join(rel[:3])})" if rel else ""))
    emoji = sorted(set(EMOJI.findall(body)))
    check(not emoji, f"{path.name} carries no emoji" + (f" (found {' '.join(emoji)})" if emoji else ""))
    # AN @MENTION IN A RELEASE BODY MAKES GITHUB LIST THAT PERSON AS A CONTRIBUTOR TO THE RELEASE.
    # These notes name testers and bug reporters — people who contributed neither code nor the
    # release — and it also attributes our claims to them on their own profile. Five published
    # releases carried one before anybody noticed (`mentions_count=1` on each). Link the profile.
    mentions = sorted(set(MENTION.findall(body)))
    check(not mentions, f"{path.name} uses profile links rather than @mentions"
                        + (f" (found @{', @'.join(mentions)} — GitHub would list them as release "
                           "contributors; write [name](https://github.com/name))" if mentions else ""))
    moved = [h for h in MOVED_SECTIONS if h in body]
    check(not moved, f"{path.name} does not carry the audit's sections"
                     + (f" ({', '.join(moved)} → docs/release-audits/)" if moved else ""))


def lint_audit(path) -> None:
    """Grade one release audit's AUTHORED half. The generated half grades itself by existing."""
    body = path.read_text()
    has_markers = AUDIT_BEGIN in body and AUDIT_END in body
    check(has_markers, f"{path.name} carries the generated-block markers CI fills")
    authored = body.split(AUDIT_BEGIN)[0] if AUDIT_BEGIN in body else body
    absent = [h for h in AUTHORED_AUDIT_SECTIONS if h not in authored]
    check(not absent, f"{path.name} carries the authored sections"
                      + (f" (missing {'; '.join(absent)})" if absent else ""))
    # Same firmware gate as the note, on the half a person wrote. The generated half is derived
    # from the tool's own output and cannot invent a release.
    # Same rule as the note, for a weaker reason: an audit is not a release body, so it creates no
    # release contributor — but it does notify and mis-attribute, and one rule across both
    # directories is one rule to remember.
    mentions = sorted(set(MENTION.findall(authored)))
    check(not mentions, f"{path.name} uses profile links rather than @mentions"
                        + (f" (found @{', @'.join(mentions)})" if mentions else ""))
    unknown = unknown_firmwares(authored)
    check(not unknown, f"every webOS version {path.name}'s authored half names has evidence"
                       + (f" (no evidence for {', '.join(unknown)})" if unknown else ""))


# Lint one document without a package, which is how a note or an audit is graded while it is
# being written — before the version is bumped and long before anything is built.
if len(sys.argv) > 2 and sys.argv[1] in ("--lint-note", "--lint-audit"):
    target = Path(sys.argv[2])
    print(f"== {sys.argv[1][7:]} {target} ==")
    (lint_note if sys.argv[1] == "--lint-note" else lint_audit)(target)
    for f in FAILURES:
        print(f"::error::{f}")
    print("\n" + ("all assertions passed" if not FAILURES else "FAILURES above"))
    sys.exit(1 if FAILURES else 0)



def png_size(p: Path) -> tuple[int, int]:
    return struct.unpack(">II", p.read_bytes()[16:24])


# ---- the localized descriptors ----------------------------------------------------------------
#
# `resources/<locale>/appinfo.json` gives the launcher tile and the store listing a title and
# description per television language. LG documents exactly two properties in one — "In the
# appinfo.json file for localization, you should fill appDescription and title properties. All
# other properties are kept the same as the top-level appinfo.json file" — and requires UTF-8
# WITHOUT BOM for non-Latin text. Both are gated below, and neither is cosmetic:
#
#   * a THIRD property in one of these files is `pkg/appinfo.json` duplicated. The version would
#     then live in a file `ci/bump-version.py`, `release.yml`'s tag guard and every version
#     assertion in THIS file have never heard of — the failure `ci/flavor.py`'s "PATCH, DO NOT
#     DUPLICATE" exists to prevent, arrived at from the other direction.
#   * a BOM makes the file unparseable to a strict JSON reader while looking identical in every
#     editor and in `git diff`. LG asks for its absence by name, so this is their rule, not ours.
#
# This runs against the TRACKED tree in every invocation, including the one that has no package
# staged — the tracked-only branch below is what `release.yml`'s `prepare` runs before a tag
# exists, and a translation is exactly the kind of thing that gets edited by hand.
LOCALE_RE = re.compile(r"^[a-z]{2,3}(-[A-Z][a-z]{3})?(-([A-Z]{2}|[0-9]{3}))?$")
LOCALIZED_KEYS = {"title", "appDescription"}


def check_tracked_resources(expect_title: str) -> list:
    """Grade `pkg/resources/`. `expect_title` is the title these must carry. Returns the locales."""
    src = ROOT / "pkg" / "resources"
    if not src.is_dir():
        check(False, "pkg/resources/ exists — the localized appinfo tree (LG checklist #41)")
        return []
    locales = sorted(p.name for p in src.iterdir() if p.is_dir())
    check(bool(locales), f"pkg/resources/ carries at least one locale (saw {len(locales)})")
    english = json.loads((ROOT / "pkg/appinfo.json").read_text())["appDescription"]
    for loc in locales:
        f = src / loc / "appinfo.json"
        check(LOCALE_RE.fullmatch(loc) is not None,
              f"pkg/resources/{loc} is a locale tag (language[-Script][-REGION])")
        if not f.is_file():
            check(False, f"pkg/resources/{loc}/appinfo.json exists")
            continue
        raw = f.read_bytes()
        check(not raw.startswith(b"\xef\xbb\xbf"),
              f"{loc}/appinfo.json is UTF-8 without BOM (LG's stated requirement)")
        try:
            d = json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as e:
            check(False, f"{loc}/appinfo.json is valid UTF-8 JSON ({e})")
            continue
        check(set(d) == LOCALIZED_KEYS,
              f"{loc}/appinfo.json carries exactly {sorted(LOCALIZED_KEYS)} (saw {sorted(d)})")
        # The title is the BRAND, not a string to translate: TRADEMARKS.md reserves it, and LG's
        # own listing shows it verbatim. A translated one here would also silently un-badge a
        # flavoured install's tile, which is why `mkipk.stage_resources` reapplies the suffix
        # rather than trusting whatever is written in the tree.
        check(d.get("title") == expect_title,
              f"{loc}/appinfo.json title is the untranslated brand ({expect_title!r})")
        desc = d.get("appDescription", "")
        check(bool(desc.strip()) and desc != english,
              f"{loc}/appinfo.json appDescription is present and actually translated")
        # The English sentence is a trademark disclaimer. A translation that transliterated the
        # mark ("플렉스") would both lose the disclaimer's force and misuse it, and no reader of
        # this repository is placed to catch that by eye in twelve languages.
        check("Plex" in desc, f"{loc}/appinfo.json names Plex verbatim (the disclaimer's subject)")
    return locales


def font_family(p: Path) -> str:
    """nameID 1 out of a TrueType `name` table, without fontTools."""
    b = p.read_bytes()
    (numtables,) = struct.unpack(">H", b[4:6])
    for i in range(numtables):
        off = 12 + 16 * i
        if b[off:off + 4] == b"name":
            toff, = struct.unpack(">I", b[off + 8:off + 12])
            count, stroff = struct.unpack(">HH", b[toff + 2:toff + 6])
            for r in range(count):
                ro = toff + 6 + 12 * r
                pid, _enc, _lang, nid, ln, no = struct.unpack(">HHHHHH", b[ro:ro + 12])
                if nid == 1:
                    raw = b[toff + stroff + no: toff + stroff + no + ln]
                    # platform 0 (Unicode) and 3 (Windows) are both UTF-16BE; 1 (Mac) is not.
                    enc = "latin-1" if pid == 1 else "utf-16-be"
                    return raw.decode(enc, "replace")
    return "?"


# WHICH INSTALL was packaged — read off the STAGE, not assumed.
#
# An installed app's identity is spelled FOUR times in the archive and they must all agree: the
# staged `applications/<dir>` name, that directory's `appinfo.json` `id`,
# `packages/<id>/packageinfo.json`, and the control file's `Package:`. The directory is also what
# the running binary reads to learn which install it is (`paths::app_id`), so a mismatch is a
# process that identifies as something its own descriptor denies. Since two ids can be packaged
# from this tree now, taking the id
# from the tracked `pkg/appinfo.json` would grade a debug package against the stable id and every
# path below would go VACUOUS: an empty `rglob` prints nothing, fails nothing, and reports success.
# That is the exact defect class the build-machine-path scan's own comment names, and it is how a
# missing `packageinfo.json` hid for months. So: derive, then assert all four agree with each other.
APPS = ROOT / "ipkroot/data/usr/palm/applications"
staged = sorted(p.name for p in APPS.glob("*")) if APPS.is_dir() else []

print("== packaged identity ==")

# NOTHING STAGED IS NOT A FAILURE BY ITSELF — it depends on who is asking, and getting that wrong
# breaks the release before it starts. Four jobs run this script and only TWO of them have built
# anything first: `release.yml`'s `prepare` calls it before the tag exists, precisely so the four
# version files are proven to agree while failing is still cheap, and `guard` does the same after
# the tag. On a fresh checkout neither `ipkroot/data` nor `pkg/*.ipk` exists — both are derived and
# neither is tracked — so a hard failure here means `prepare` exits 1 and NO RELEASE IS EVER CUT.
# That is a regression this file shipped with: the pre-two-install version skipped the package half
# when nothing was built, which is why v0.3.0 could be published at all.
#
# The fix is not to go back to skipping everywhere, which is how a build job could grade a package
# it never made. It is to let the CALLER say which it is. `REQUIRE_PACKAGE=1` is set by the two
# post-build steps, so a missing package there is still a hard error — strictly stronger than the
# behaviour this replaces, which had no way to demand one at all.
REQUIRE_PACKAGE = os.environ.get("REQUIRE_PACKAGE") == "1"
if len(staged) != 1 and not REQUIRE_PACKAGE:
    print(f"  SKIP — no package staged ({staged or 'nothing'}); grading the TRACKED files only.")
    print("         Set REQUIRE_PACKAGE=1 to make this a failure (the post-build CI steps do).")
    # ...but "skip" must not mean "grade nothing", which is what `prepare` would then be doing while
    # its own comment claims it proves the version files agree. Everything below this point reads
    # `appinfo` out of the STAGE, so none of it is reachable — yet the three files that carry the
    # version are all TRACKED and all present on a bare checkout. This is that agreement, and it is
    # the assertion `prepare` exists to make before it writes a tag.
    print("== version agreement (tracked files) ==")
    tracked_appinfo = json.loads((ROOT / "pkg/appinfo.json").read_text())
    tracked_control = dict(
        line.split(": ", 1)
        for line in (ROOT / "ipkroot/ctl/control").read_text().splitlines() if ": " in line
    )
    tracked_cargo = re.search(r'^version = "([^"]+)"', (ROOT / "rust-modules/Cargo.toml").read_text(), re.M)
    v = tracked_appinfo["version"]
    check(re.fullmatch(r"\d+\.\d+\.\d+", v) is not None, f"pkg/appinfo.json version is X.Y.Z ({v})")
    check(tracked_control.get("Version") == v, f'control Version == appinfo version ({v})')
    check(tracked_cargo is not None and tracked_cargo.group(1) == v,
          f'Cargo.toml version == appinfo version ({v})')
    check(tracked_appinfo["id"] == flavor.STABLE_ID,
          f'pkg/appinfo.json id is the stable id ({flavor.STABLE_ID})')
    check(tracked_control.get("Package") == flavor.STABLE_ID,
          f'control Package is the stable id ({flavor.STABLE_ID})')
    # Tracked too, and editable by hand in twelve languages — so graded on the cheap path as well,
    # not only after a cross-build has produced a package.
    print("== localized appinfo (tracked) ==")
    check_tracked_resources(tracked_appinfo["title"])
    # The release note and the release audit are TRACKED files too, so they are graded on this
    # cheap path as well — which is the one `release.yml`'s `prepare` runs BEFORE it writes a tag.
    # Catching a missing note here costs a job; catching it after the build costs a tag that
    # points at a release nobody can publish.
    if tracked_appinfo["id"] == flavor.STABLE_ID:
        print("== release documents (tracked) ==")
        _note = ROOT / f"docs/release-notes/v{v}.md"
        _audit = ROOT / f"docs/release-audits/v{v}.md"
        check(_note.exists(), f"docs/release-notes/v{v}.md exists (CI publishes the body from it)")
        if _note.exists():
            lint_note(_note)
        check(_audit.exists(), f"docs/release-audits/v{v}.md exists (the authored half)")
        if _audit.exists():
            lint_audit(_audit)
    for f in FAILURES:
        print(f"::error::{f}")
    print(f"\n{'all tracked-file assertions passed' if not FAILURES else 'FAILURES above'}")
    sys.exit(1 if FAILURES else 0)

check(len(staged) == 1,
      f"exactly one application directory is staged (saw {staged or 'none — run `make ipk` first'})")
if len(staged) != 1:
    # Everything below reads through this id. Continuing would grade nothing and say so cheerfully.
    for f in FAILURES:
        print(f"::error::{f}")
    sys.exit(1)
PACKAGED_ID = staged[0]
FLAVOR = next((f for f in flavor.FLAVORS if flavor.app_id(f) == PACKAGED_ID), None)
check(FLAVOR is not None, f"the staged id is a known flavour ({PACKAGED_ID})")
IS_STABLE = FLAVOR == "stable"
# The staged payload directory — written down ONCE, here, because everything below reads through
# it: the descriptor, the build-machine-path scan, the binary and the icons.
PAYLOAD = APPS / PACKAGED_ID
print(f"  -- grading the {FLAVOR or '?'} package: {PACKAGED_ID}")

print("== version / id consistency ==")
appinfo = json.loads((PAYLOAD / "appinfo.json").read_text())
# The tracked control file names the STABLE package; the flavoured one is assembled in memory by
# `mkipk.py` at package time (as `Installed-Size` already was). Grade through the same transform,
# rather than widening the equality into something that cannot fail.
control = dict(
    line.split(": ", 1)
    for line in flavor.control_for((ROOT / "ipkroot/ctl/control").read_text(), FLAVOR or "stable").splitlines()
    if ": " in line
)
check(appinfo["id"] == PACKAGED_ID,
      f'staged appinfo id == the directory it sits in ({PACKAGED_ID})')
check(appinfo["id"] == control["Package"],
      f'appinfo id == control Package ({appinfo["id"]})')
# ...and the STABLE package's descriptor must be the tracked file, unchanged. This is the gate
# that makes the whole flavour mechanism safe for the released artifact: if the transform were
# ever anything but the identity for `stable`, the .ipk's sha256 would move and every published
# manifest hash — the entire integrity story, since nothing here is code-signed — would be wrong.
if IS_STABLE:
    check(appinfo == json.loads((ROOT / "pkg/appinfo.json").read_text()),
          "the stable package's appinfo.json is the tracked file, unchanged")
check(appinfo["version"] == control["Version"],
      f'appinfo version == control Version ({appinfo["version"]})')
# Cargo.toml is the FOURTH witness, and the one with a user-visible consequence: the diagnostics
# read-out prints `plex::identity::VERSION`, which is derived from this number, and that panel is
# designed to be photographed into a bug report. A bump that missed Cargo.toml would ship a package
# labelled 0.2.1 whose own on-screen version says 0.2.0 — precisely the disagreement `identity`
# exists to make impossible, and nothing checked it until a release nearly went out that way.
# (Derived, not copied: `rust-modules/build.rs` reports the next minor with a `-dev` suffix for
# anything but a RELEASE build, which the binary check further down grades on the bytes.)
cargo = (ROOT / "rust-modules/Cargo.toml").read_text()
m = re.search(r'^version = "([^"]+)"', cargo, re.M)
check(m is not None and m.group(1) == appinfo["version"],
      f'Cargo.toml version == appinfo version ({appinfo["version"]})')

# No build machine's directory layout may ship inside the package.
#
# This exists because it happened: v0.2.1 went out with the maintainer's working directory baked
# into all three bundled FFmpeg libraries — FFmpeg records its whole configure invocation in
# libavutil — and with it the reproducibility claim in the release notes, on the one number a user
# has to check an unsigned download. `ci/check-elf.sh` only ever scanned `pkg/plxnative`, so
# nothing looked at the libraries beside it.
#
# The pattern is ANCHORED on a non-path character so ordinary URL fragments do not trip it: the
# app talks to plex.tv's `/api/v2/home/users`, which is not a build path.
# A build-machine path anywhere in the payload.
#
# Two calibration bugs are baked into the shape below, both found by running this against a release
# KNOWN to be dirty rather than assuming it worked:
#
#   * matching per-BLOB and allowing anything containing "webos-ndk" passes the very file it was
#     written for. FFmpeg records its whole configure invocation as ONE string, so the unavoidable
#     `--cross-prefix=/…/webos-ndk/…` sits beside the offending `--prefix=/…/plex-native-poc/…`
#     and one allowed token vouches for the other. v0.2.1's libraries pass that test.
#   * tokenising to fix it, without keeping the leading boundary, makes plex.tv's own
#     `/api/v2/home/users` read as `/home/users` and fails every build.
#
# So: extract each path WITH its boundary character, drop the boundary, and allow per PATH.
HOSTPATH = re.compile(rb"(?:^|[^A-Za-z0-9/_.-])(/(?:Users|home)/[A-Za-z0-9_./+-]+)")
# The NDK's own location cannot be removed — `--cross-prefix` must be absolute (the wrapper gcc
# dies when invoked through PATH), so it rides in FFmpeg's recorded configure string. It is
# identical on every CI runner, which is the reason releases must be BUILT by CI.
ALLOWED_PATH = re.compile(rb"webos-ndk|^/home/runner/")

# A missing payload directory is a HARD failure, not an empty loop. `check` only ever prints for
# something it was given, so an absent stage used to print nothing at all here — no ok, no FAIL —
# and the section that exists to keep a maintainer's working directory out of three FFmpeg
# libraries (v0.2.1, which shipped exactly that) would have reported success by saying nothing.
check(PAYLOAD.is_dir(), f"the staged payload directory exists ({PAYLOAD.relative_to(ROOT)})")
for member in sorted(PAYLOAD.rglob("*")) if PAYLOAD.is_dir() else []:
    if not member.is_file():
        continue
    dirty = sorted({m for m in HOSTPATH.findall(member.read_bytes()) if not ALLOWED_PATH.search(m)})
    # Labelled by PATH inside the payload, not basename: since the localized descriptors landed
    # there are THIRTEEN files called `appinfo.json` in here (the top level plus one per locale),
    # and a basename cannot say which one is dirty — nor which twelve of the thirteen identical
    # `ok` lines to stop reading.
    check(not dirty,
          f"{member.relative_to(PAYLOAD)} carries no build-machine path"
          + (f" (saw {dirty[0].decode(errors='replace')})" if dirty else ""))

# ---- the RELEASE is coherent, not just the package -------------------------------------------
#
# These live here rather than in a skill or a checklist for one reason: a skill is advisory and a
# gate is not. Every defect found in v0.2.1 got out because the gates were skipped — publishing by
# hand skipped them wholesale — so the response to that cannot itself be something a person has to
# remember to run.

# ...and only for the package that is actually released. A developer flavour has no release note,
# no published hash and no channel listing, and grading it against those would be a wall of
# failures for an artifact that is doing exactly what it should. SAID OUT LOUD rather than skipped
# silently, because a gate that quietly does not run is the defect this section exists to end.
note = ROOT / f"docs/release-notes/v{appinfo['version']}.md"
audit = ROOT / f"docs/release-audits/v{appinfo['version']}.md"
if not IS_STABLE:
    print(f"  SKIP — release coherence is not graded for the {FLAVOR} flavour (it is never published)")
else:
    # CI publishes the release body from the note, so a missing one means a release with no notes.
    check(note.exists(), f"docs/release-notes/v{appinfo['version']}.md exists (CI publishes the body from it)")
    # ...and the audit is the other half of the same release: its authored sections are written
    # and reviewed BEFORE the build, and `ci/gen-release-audit.py` fills its generated block from
    # the artifact during the release run.
    check(audit.exists(),
          f"docs/release-audits/v{appinfo['version']}.md exists "
          f"(the authored half — copy docs/release-audits/TEMPLATE.md)")

if IS_STABLE and note.exists():
    lint_note(note)
if IS_STABLE and audit.exists():
    lint_audit(audit)

# WHICH CONFIGURATION produced what is in pkg/, which every gate below that says "RELEASE build"
# needs. The Makefile writes this stamp at PARSE time, and it is the same witness `release.yml`
# greps to prove RELEASE=1 took.
#
# `build_configuration` above decodes it, and its docstring carries the trap: the feature flags are
# one FIELD of the stamp, not the whole of it, and reading it whole left every gate below graded on
# nothing for months. Only the two configurations this project actually ships decode; anything else
# (LAB, the shots recipe, an unreadable stamp) says so out loud instead of being graded.
#
# NB the stamp moves at make PARSE time, so any bare `make <target>` after a RELEASE=1 build flips
# it to dev while ipkroot still holds the release binary. Both workflows build, package and check
# in one shot so they never see that; a by-hand run on a stale tree can, and the disagreement it
# then reports is true — repackage before believing anything else about that tree.
_stamp = ROOT / "pkg/.build-config"
BUILD = build_configuration(_stamp.read_text() if _stamp.exists() else "")

# THIRD-PARTY-NOTICES must name exactly the libraries that ship. RELEASE=1 drops swscale, and the
# notices claimed it for two releases — an LGPL document describing a file that is not in the box.
#
# The grade is against the DISTRIBUTED set, which is not what `pkg/` holds: `ci.yml` packages a DEV
# build deliberately (a PR artifact you can sideload with the /tmp trigger surface on) and a dev
# build stages one library more. Grading pkg/ verbatim against a document written for the release
# payload is what turned every push to main red from 2026-08-10 to 2026-08-12 — the notices were
# corrected and the gate added in the same commit, and only the release job ever built the
# configuration the pair describes. Subtracting the dev-only set keeps ONE rule for both
# configurations, with no build-flag sniffing: a new library still has to be documented, and a
# documented one that stopped shipping still fails. Whether a RELEASE build really dropped them is
# the separate, narrower check below.
DEV_ONLY_SONAMES = {"libswscale-plx.so.10"}   # the dev capture stream's scaler; RELEASE=1 drops it
shipped = {p.name for p in (ROOT / "pkg").glob("*.so.*")}
if shipped:
    named = set(re.findall(r"`(lib[a-z]+-plx\.so\.\d+)`", (ROOT / "THIRD-PARTY-NOTICES.md").read_text()))
    distributed = shipped - DEV_ONLY_SONAMES
    check(distributed == named,
          "THIRD-PARTY-NOTICES names exactly the distributed libraries"
          + (f" (shipped-not-named={sorted(distributed-named)} named-not-shipped={sorted(named-distributed)})"
             if distributed != named else ""))
    # ...and a RELEASE build must carry none of the dev-only ones at all, which is the half the
    # subtraction above cannot see.
    if BUILD == "release":
        extra = sorted(shipped & DEV_ONLY_SONAMES)
        check(not extra, "a RELEASE build ships none of the dev-only libraries"
                         + (f" (found {', '.join(extra)})" if extra else ""))

# A dev build carries the /tmp trigger surface. `RELEASE=1` must be on EVERY make invocation, and
# any make without it deletes the release artifacts at parse time — so this is worth asserting on
# the bytes rather than trusting the command line that produced them.
#
# The witness has to be a string only a `devtriggers` build emits, and almost none are: `dev.rs`
# composes every trigger path as `paths::in_runtime_dir(format!("plxnative-{name}"))` — a bare name
# joined to a root resolved at RUNTIME, which since the flavour split is not even always `/tmp` — so
# no full trigger path is a literal anywhere. The previous witness here was b"plxnative-autoplay" and it matched NOTHING —
# in EITHER configuration — so from the day it was written this printed "ok — the packaged binary
# is a RELEASE build" over CI's dev build on every run, while release.yml's stamp grep carried the
# property alone. `dev.rs`'s DIAG list is the one place the full names are literals, it is
# `#[cfg(feature = "devtriggers")]`, and `plxnative-noidle` is not one of the four logs `main.c`
# writes unconditionally. Measured on the two shipped artifacts — published v0.3.0 .ipk: 0
# occurrences; CI's dev .ipk for 8827d32c: 2.
#
# GRADED FROM BOTH SIDES, which is the repair for the defect class rather than for the one string:
# a witness that cannot fail is not a gate. The dev leg asserts the marker is still emitted, so the
# day DIAG is renamed CI fails on the next push instead of quietly going vacuous again.
DEV_WITNESS = b"plxnative-noidle"
binary = PAYLOAD / "plxnative"
check(binary.exists(), f"the staged payload carries the binary ({binary.name})")

# THE ID IS THE RULE, and it is graded whatever the stamp says — note the `if IS_STABLE`
# below sits BESIDE the `BUILD` branch, never inside it.
#
# `com.beb.plxnative` is what a user installs, so a dev-featured binary under it ships the whole
# /tmp trigger surface, the world-writable `plxnative-remote` FIFO and the `:8910` listener to the
# public. The Makefile's `release-guard` refuses to BUILD that; this is the same rule on the bytes,
# which is the half that survives someone reaching for the documented `ALLOW_DEV_ON_STABLE=1`
# hatch and forgetting.
#
# It must not sit under `if BUILD:` — `BUILD` is `None` for any stamp that is neither shipped
# configuration, and the Makefile itself documents a third (`RUST_FEATFLAGS="--no-default-features
# --features devtriggers"`, the README-screenshot recipe). Nested, that combination would satisfy
# `release-guard` (RELEASE is non-empty), print "SKIP — neither shipped configuration", and package
# a dev-trigger binary under the released id on a green run. "This package carries no dev-trigger
# surface" is a property of the BYTES and needs no stamp to grade.
# THE GNU BUILD ID, which nothing else in this repo would notice the loss of.
#
# It is the only identifier that survives `strip` into the binary a user runs, and therefore the
# only thing that can match a separated `pkg/plxnative.debug` back to a crash reported from a
# television. Dropping `-Wl,--build-id=sha1` from the link would break every future symbolication
# silently: the package builds, installs, runs and crashes exactly as before, and the failure
# surfaces months later as a debug file that matches nothing — by which time the build that
# produced the crash is gone.
#
# Matched as the NOTE STRUCTURE rather than by shelling out to readelf, so this stays stdlib-only
# and does not need the NDK on the runner: namesz=4, descsz=20 (sha1), type=3 (NT_GNU_BUILD_ID),
# then the name "GNU\0". Little-endian, which every target this project has ever had is.
BUILD_ID_NOTE = b"\x04\x00\x00\x00\x14\x00\x00\x00\x03\x00\x00\x00GNU\x00"

if binary.exists():
    blob = binary.read_bytes()
    check(BUILD_ID_NOTE in blob,
          "the packaged binary carries a GNU build id (-Wl,--build-id=sha1 is still on the link)")
    has_dev = DEV_WITNESS in blob
    if IS_STABLE:
        check(not has_dev,
              f"the {PACKAGED_ID} package carries no dev-trigger surface — that id is what users install")
    if BUILD == "release":
        check(not has_dev, "the packaged binary is a RELEASE build (no dev triggers compiled in)")
    elif BUILD:
        check(has_dev, "the packaged binary is the DEV build the stamp records — which is also what"
                       f" proves `{DEV_WITNESS.decode()}` still witnesses the trigger surface")
    else:
        print("  SKIP — pkg/.build-config is neither shipped configuration; not grading the binary")

    # WHICH VERSION THE BINARY SAYS IT IS, which the four tracked files above cannot answer.
    #
    # They are the version this package was CUT from; `rust-modules/build.rs` decides what the
    # binary REPORTS, and for anything but `RELEASE=1` that is the next minor with a `-dev` suffix
    # (`0.5.0` published, `0.6.0-dev` in the tree). That exists so a developer build stops
    # impersonating the last release in X-Plex-Version, in the Sentry release and on the
    # diagnostics panel — and it means a version string now has a way to be wrong that no file
    # comparison can see: a package for the stable id whose binary reports a version no release
    # will ever carry, or, once this rule exists, a developer build that silently stopped saying so.
    #
    # Graded from BOTH sides for the reason DEV_WITNESS is: a witness that cannot fail is not a
    # gate, and the string is compiled in from an env var, i.e. from something a build can lose.
    # The suffix cannot be read off appinfo.json (LG takes three integers, so it never gets there),
    # so it is recomputed here from the same arithmetic build.rs uses.
    #
    # MATCHED WITH THE `plxnative@` PREFIX, not as a bare number, and that is the difference
    # between grading `PLX_VERSION` and grading whatever digits happen to be in .rodata: the About
    # page and any release note text carry the version too, so a bare-number search was satisfiable
    # by a page the version mechanism never touched. `telemetry::{crashreport,native,playback}`
    # compose `concat!("plxnative@", env!("PLX_VERSION"))` in every configuration — telemetry is
    # ungated on purpose — so this witnesses the emitted value itself.
    # The next MINOR with the patch reset, which is what `build.rs` emits and why: trunk is where
    # features land, so the next release cut from it is a minor (or a major, which nothing here can
    # predict); a patch comes off an existing minor's own line.
    _major, _minor, _ = (int(x) for x in appinfo["version"].split("."))
    DEV_VERSION = f"plxnative@{_major}.{_minor + 1}.0-dev".encode()
    #
    # The id is a rule of its own here too, so it sits BESIDE the stamp branch rather than inside
    # it: whatever configuration produced it, the package users install may not claim a version no
    # release will ever carry.
    says_dev = DEV_VERSION in blob
    if IS_STABLE:
        check(not says_dev,
              f"the {PACKAGED_ID} binary reports a released version, not {DEV_VERSION.decode()}"
              " (build.rs adds the suffix unless PLX_RELEASE is set — RELEASE=1 exports it)")
        check(f'plxnative@{appinfo["version"]}'.encode() in blob,
              f'the {PACKAGED_ID} binary reports the packaged version ({appinfo["version"]})')
    # ...and the configuration is the other half. A `RELEASE=1` build of ANY flavour reports the
    # exact version — `make FLAVOR=debug RELEASE=1 ipk` is a real combination, the submission
    # candidate is built that way — so the suffix is graded against the stamp, not against the id.
    if BUILD == "release":
        check(not says_dev,
              f"the RELEASE binary reports {appinfo['version']} exactly, not {DEV_VERSION.decode()}")
    elif BUILD == "dev":
        check(says_dev,
              f"the dev binary says it is one ({DEV_VERSION.decode()}) — which is also what proves"
              " the suffix still reaches the bytes")

# The checksum file has to verify where a USER stands: they download it beside the .ipk, so a
# `pkg/` prefix in the line makes `shasum -a 256 -c` fail for everyone. It did, through v0.2.1.
sha_file = ROOT / "pkg/ipk.sha256"
if not IS_STABLE:
    print(f"  SKIP — ipk.sha256 is a released asset name; the {FLAVOR} flavour does not write it")
elif sha_file.exists():
    check(not any(l.split("  ")[-1].startswith("pkg/") for l in sha_file.read_text().splitlines() if l.strip()),
          "ipk.sha256 carries the bare filename, so `shasum -c` works beside the .ipk")

# The Makefile derives IPK_VERSION from appinfo.json, so the built filename is the fourth witness.
# Scoped to THIS flavour's id: two flavours' artifacts can sit in pkg/ side by side, and the
# `_arm.ipk` suffix in the pattern is what keeps `com.beb.plxnative_*` from also matching
# `com.beb.plxnative.debug_*` (the dot is not a `_`, but a bare prefix test would still match).
built = sorted((ROOT / "pkg").glob(f"{PACKAGED_ID}_*_arm.ipk"))
if built:
    check(len(built) == 1, f"exactly one built {PACKAGED_ID} ipk in pkg/ (saw {[p.name for p in built]})")
    m = re.fullmatch(rf"{re.escape(PACKAGED_ID)}_([0-9][0-9.]*)_arm\.ipk", built[0].name)
    check(m is not None and m.group(1) == appinfo["version"],
          f"built ipk filename carries the appinfo version ({built[0].name})")
else:
    print(f"  SKIP — no built {PACKAGED_ID} ipk in pkg/ (run `make ipk` first)")
check(re.fullmatch(r"\d+\.\d+\.\d+", appinfo["version"]) is not None,
      "version is exactly three integers (LG requirement)")
check(appinfo["type"] == "native", 'appinfo type == "native"')
check(not appinfo["id"].startswith(("com.palm", "com.webos", "com.lge", "com.palmdts")),
      "app id avoids LG's reserved prefixes")
# The crate version is a THIRD copy of the same number: plex/identity.rs sends it to both Plex
# services as X-Plex-Version (through `PLX_VERSION`, which `build.rs` derives from it), so a build
# whose Cargo.toml disagreed with appinfo.json would report a version no release ever had.
cargo_ver = re.search(r'^version\s*=\s*"([^"]+)"', (ROOT / "rust-modules/Cargo.toml").read_text(), re.M)
check(cargo_ver is not None and cargo_ver.group(1) == appinfo["version"],
      f'rust-modules/Cargo.toml version == appinfo version ({appinfo["version"]})')

# Control-file provenance. None of this is read by opkg, and that is the point: it is what a
# human — a webosbrew reviewer, or a user running `opkg info` — sees about who ships this and
# under what terms. The Maintainer assertion exists because the field held a personal Gmail that
# travelled inside every distributed .ipk, and nothing would have caught its return.
check("Homepage" in control, "control declares a Homepage")

# The three fields webosbrew's ipk-verify reads to decide whether a package was built by a webOS
# packager. Any one missing and every submission report carries ":warning: This package looks
# hand-rolled. Please build it with `ares-package`." — the check itself still PASSES, so nothing
# fails and the warning simply rides along on the PR forever. The heuristic is presence-only
# (dev-toolbox-cli, common/ipk/src/ipk.rs: `PACKAGER_FIELDS.iter().any(|f| control.get(f).is_none())`).
#
# Installed-Size is written by mkipk.py at build time, so only the two static ones are asserted
# here. Values taken from what ares-package 2.4.0 itself emits, except the packager string, which
# names OUR packager rather than copying theirs — claiming to be ares when we are not would be the
# dishonest way to silence a warning. See mkipk.py's header for why we do not use ares-package.
for field in ("webOS-Package-Format-Version", "webOS-Packager-Version"):
    check(field in control, f"control declares {field}")
check(control.get("License") == "MIT", f'control License == MIT (saw {control.get("License")!r})')
check("@users.noreply.github.com" in control["Maintainer"] or "@gmail.com" not in control["Maintainer"],
      f'control Maintainer is not a personal mailbox ({control["Maintainer"]})')

print("== icons ==")
# Graded on the STAGED artwork, so a badged debug tile is checked as thoroughly as the release one
# — the icons are the only payload files whose SOURCE differs per flavour, which makes them the
# only ones a per-flavour bug could reach. (They are staged under the canonical basenames, which is
# itself what appinfo's `icon`/`largeIcon` fields and the payload gate below both require.)
check(png_size(PAYLOAD / "icon.png") == (80, 80), "icon.png is 80x80")
check(png_size(PAYLOAD / "largeIcon.png") == (130, 130), "largeIcon.png is 130x130")
# `iconColor` paints the launcher tile BEHIND the icon, so a disagreement draws the icon as a
# hard-edged rectangle floating in a differently-coloured tile. Shipped that way until 2026-08-02
# (gold tile, black icon) and invisible in every file — it only exists once the system composites.
# The corner pixel is the icon's own background; anything within a couple of levels is the same
# colour to the eye and to a PNG optimiser.
corner = None
try:
    from PIL import Image
    corner = Image.open(PAYLOAD / "largeIcon.png").convert("RGB").getpixel((1, 1))
except ImportError:
    print("  SKIP — Pillow absent; cannot compare iconColor against the icon background")
if corner is not None:
    want = appinfo["iconColor"].lstrip("#")
    declared = tuple(int(want[i:i + 2], 16) for i in (0, 2, 4))
    check(max(abs(a - b) for a, b in zip(corner, declared)) <= 2,
          f"iconColor {appinfo['iconColor']} matches the icon's own background rgb{corner}")

check(png_size(PAYLOAD / "splash.png") == (1920, 1080),
      "splash.png is exactly 1920x1080 (splashBackground accepts no other size)")
# The badged set is a tracked artwork source (`tools/mkicons.py --out-dir=pkg/dev --badge=DEV`), so
# it is graded whether or not this run happens to be packaging it — otherwise a regression in it
# would only ever be found by whoever next built a debug package.
if (ROOT / "pkg/dev").is_dir():
    check(png_size(ROOT / "pkg/dev/icon.png") == (80, 80), "pkg/dev/icon.png is 80x80")
    check(png_size(ROOT / "pkg/dev/largeIcon.png") == (130, 130), "pkg/dev/largeIcon.png is 130x130")
    if corner is not None:
        # Same rule as above, and the same failure it prevents: iconColor paints the launcher tile
        # BEHIND the icon, so a badge that changed the tile's own background without moving
        # iconColor would draw the debug icon as a hard-edged rectangle in a differently-coloured
        # tile. The badge is a BOTTOM bar for this reason — pixel (1,1) is untouched, so one
        # iconColor stays correct for both flavours.
        dbg_corner = Image.open(ROOT / "pkg/dev/largeIcon.png").convert("RGB").getpixel((1, 1))
        check(max(abs(a - b) for a, b in zip(dbg_corner, declared)) <= 2,
              f"the badged tile keeps iconColor {appinfo['iconColor']} at its corner rgb{dbg_corner}")
check(appinfo.get("splashBackground") == "splash.png",
      "appinfo declares splashBackground: splash.png")

print("== shipped fonts ==")
# Landed 2026-08-01: Inter (SIL OFL 1.1). This is now a REAL gate, not an XFAIL — its job is to
# stop Monotype Arial coming back through a stale local copy, which is exactly how it would
# return (the files are named appfont*.ttf, so nothing about the filename reveals the swap).
ALLOWED = {"Inter", "Arimo", "Roboto", "Noto Sans", "Source Sans 3", "Noto Sans CJK KR"}
for f in ("pkg/appfont.ttf", "pkg/appfont-bold.ttf", "pkg/appfont-cjk.ttf"):
    fam = font_family(ROOT / f)
    check(fam in ALLOWED, f"{f} family={fam!r} is redistributable (allowed: {sorted(ALLOWED)})")
# The fallback face, checked for PRESENCE separately: the payload assertion below only runs when
# an .ipk has been built, and this file is the difference between a Korean library rendering and
# rendering as tofu. The floor is deliberately crude — a subsetted or truncated stand-in is caught
# properly by `fontcov.rs`'s cmap gate in `make check`; this only catches "it is not there".
_cjk = ROOT / "pkg/appfont-cjk.ttf"
check(_cjk.exists() and _cjk.stat().st_size > 15_000_000,
      "pkg/appfont-cjk.ttf present and whole — the CJK fallback face (see rust-modules/src/fontcov.rs)")
check((ROOT / "pkg/OFL.txt").exists(),
      "pkg/OFL.txt present — the OFL requires the licence to travel with the font")
# One OFL text covers both faces: Inter's and Noto CJK's licence bodies are byte-identical after
# their differing copyright headers, and the per-font copyright notices live in THIRD-PARTY-NOTICES
# and in each font's own name table (ID 0/7/13/14, asserted by tools/cut-noto-cjk.py).
check("Noto Sans CJK" in (ROOT / "THIRD-PARTY-NOTICES.md").read_text(),
      "THIRD-PARTY-NOTICES.md attributes Noto Sans CJK (OFL 1.1 §2 wants the notice to travel)")

print("== compliance artifacts ==")
# LGPL-2.1 §6 requires the notice AND the licence text to travel with the BINARY, so these are
# payload rather than repo decoration — a copy on GitHub does not discharge it for someone who
# received only the .ipk. release.yml's legal-gate refuses to publish without the first two.
for f in ("LICENSE", "TRADEMARKS.md", "THIRD-PARTY-NOTICES.md"):
    check((ROOT / f).exists(), f"{f} present")
# LICENSE must stay VERBATIM MIT. GitHub's `licensee` matches it against known licence texts by
# similarity, and this file previously carried the trademark reservation appended below the grant —
# which pushed it under the threshold, so the repository reported its licence as "Other". That
# misrepresents the terms in the one place most people look. The reservation lives in TRADEMARKS.md
# now; this assertion is what stops it drifting back.
_lic = (ROOT / "LICENSE").read_text()
check(_lic.rstrip().endswith("SOFTWARE."),
      "LICENSE is verbatim MIT (no appended text — it would read as 'Other' on GitHub)")
check("TRADEMARK" not in _lic.upper(), "LICENSE carries no trademark reservation (see TRADEMARKS.md)")
NEEDED_LICENCES = {
    "LGPL-2.1.txt": "FFmpeg, GLib, glibc — dynamically linked, §6 notice duty",
    "MIT.txt": "Feather/Heroicons and the MIT-elected Rust crates",
    "Apache-2.0.txt": "Material Icons, moxcms, pxfm, compiler_builtins",
    "LLVM-exception.txt": "compiler_builtins",
    "Unicode-3.0.txt": "the Unicode tables inside Rust core",
    "Zlib.txt": "nanosvg — vendored and compiled into the binary",
}
for name, why in NEEDED_LICENCES.items():
    p = ROOT / "licenses" / name
    check(p.exists() and p.stat().st_size > 200, f"licenses/{name} — {why}")

print("== localized appinfo (staged) ==")
# The tracked half first, against the TRACKED title — `stage_resources` reapplies the flavour
# suffix, so the file in the tree carries the bare brand name whatever is being packaged.
tracked_locales = check_tracked_resources(json.loads((ROOT / "pkg/appinfo.json").read_text())["title"])
staged_res = PAYLOAD / "resources"
staged_locales = sorted(p.name for p in staged_res.iterdir() if p.is_dir()) if staged_res.is_dir() else []
# A SET EQUALITY, not a subset. A locale added to the tree and never staged is the ipk-vs-deploy
# divergence that shipped a fontless package for months, and a locale left in the stage after being
# deleted from the tree is a translation nobody can find the source of.
check(staged_locales == tracked_locales,
      f"the staged locales are exactly the tracked ones ({len(tracked_locales)}: "
      f"{' '.join(tracked_locales) or 'none'})")
for loc in staged_locales:
    staged_loc = json.loads((staged_res / loc / "appinfo.json").read_text(encoding="utf-8"))
    check(set(staged_loc) == LOCALIZED_KEYS,
          f"staged {loc}/appinfo.json carries exactly {sorted(LOCALIZED_KEYS)} (saw {sorted(staged_loc)})")
    # THE flavour assertion, and the only one that can fail for the debug package alone: a localized
    # descriptor overrides the top-level one, so a tile reading `PlxNative debug` in English and
    # `PlxNative` in Korean is two installs that cannot be told apart on a Korean set.
    check(staged_loc.get("title") == appinfo["title"],
          f'staged {loc}/appinfo.json title == the staged top-level title ({appinfo["title"]!r})')
    if loc in tracked_locales:
        tracked_loc = json.loads((ROOT / "pkg/resources" / loc / "appinfo.json").read_text(encoding="utf-8"))
        check(staged_loc.get("appDescription") == tracked_loc.get("appDescription"),
              f"staged {loc}/appinfo.json appDescription is the tracked translation, unchanged")

print("== ipk payload ==")
expected = {
    "plxnative", "sentry-crash", "appinfo.json", "icon.png", "largeIcon.png", "splash.png",
    # appfont-cjk.ttf is the fallback face. Its absence is not a cosmetic loss: every Korean,
    # Japanese and Chinese title in the library becomes tofu, which is LG checklist #6 and #48.
    "appfont.ttf", "appfont-bold.ttf", "appfont-cjk.ttf", "OFL.txt",
    "THIRD-PARTY-NOTICES.md", "LICENSE", "TRADEMARKS.md", *NEEDED_LICENCES,
}
data_tar = ROOT / "ipkroot/data.tar.gz"
if data_tar.exists():
    import tarfile
    with tarfile.open(data_tar) as t:
        members = [m for m in t.getmembers() if m.isfile()]
        names = {Path(m.name).name for m in members}
        modes = {Path(m.name).name: m.mode & 0o777 for m in members}
        paths = {m.name.lstrip("./") for m in members}
        owners = {(m.uname, m.gname) for m in members}
    check(expected <= names, f"payload carries all {len(expected)} app files")
    check(modes.get("plxnative") == 0o755,
          "native app is executable by its jailed runtime uid")
    check(modes.get("sentry-crash") == 0o755,
          "native crash handler is executable in the archive")
    # **The simulator's Mach-O FFmpeg lives in pkg/ too, and must never be in the package.** It
    # cannot get there today — `APP_FILES` is an explicit list, not a glob — but "cannot" is a
    # property of one Makefile line, and what it guards against is 2 MB of unrunnable arm64 shipped
    # to a 32-bit television inside an archive whose sha256 is a published release asset. Graded
    # against the ARCHIVE, never against pkg/, which is expected to hold them.
    dylibs = sorted(n for n in names if n.endswith(".dylib"))
    check(not dylibs, "payload carries no host .dylib"
                      + (f" (found {', '.join(dylibs)})" if dylibs else ""))
    # The Makefile's own comment records the ipk once shipping WITHOUT the fonts, silently
    # rendering the whole theme::size ladder in DroidSans.
    check(owners <= {("root", "root"), ("", "")},
          f"payload is not owned by the developer's account (saw {sorted(owners)})")
    # webOS's *package* descriptor, distinct from the app's appinfo.json. Absent from every ipk
    # built before 2026-08-02 and undetectable from the dev loop, which scp's into an app dir the
    # TV already has registered. Without it `appinstalld` unpacks nothing.
    check(f'usr/palm/packages/{appinfo["id"]}/packageinfo.json' in paths,
          "payload carries usr/palm/packages/<id>/packageinfo.json")
    # ...and the localized descriptors, asserted BY PATH. The `expected` set above is basenames, so
    # it cannot see these at all: `resources/ko/appinfo.json` contributes the basename
    # `appinfo.json`, which the top-level descriptor already supplies. A resources tree that never
    # reached the archive would pass every other assertion in this file.
    # **A LAB SESSION FILE IS NEVER IN A PACKAGE THAT IS NOT A LAB PACKAGE, AND NEVER ON THE
    # STABLE ID AT ALL.** `pkg/lab.json` carries a live bearer secret and an endpoint on the
    # developer's own router (`docs/lab-diagnostics.md`); it reaches the payload only through
    # `make LAB=1`, whose `LAB_FILES` adds it. The failure this catches is a leftover: the file is
    # written by `tools/plxnative-lab start` and is not removed by anything, so the NEXT ordinary
    # `make ipk` in that tree would ship it if the Makefile's condition were ever loosened — and
    # the artifact would look completely normal. `LAB` is read from the environment because that
    # is how `make` was invoked; the assertion is one-directional on purpose (a lab build MAY
    # contain it, nothing else may).
    LAB = bool(os.environ.get("LAB"))
    has_lab = "lab.json" in names
    check(not has_lab or LAB,
          "payload carries lab.json only in a LAB=1 build (a live session secret otherwise)")
    check(not (has_lab and IS_STABLE),
          "the stable id never carries a lab session file")
    missing = [loc for loc in tracked_locales
               if f'usr/palm/applications/{appinfo["id"]}/resources/{loc}/appinfo.json' not in paths]
    check(not missing,
          f"payload carries resources/<locale>/appinfo.json for all {len(tracked_locales)} locales"
          + (f" (missing {' '.join(missing)})" if missing else ""))
else:
    print("  SKIP — ipkroot/data.tar.gz absent (run `make ipk` first)")

print("== ar container ==")
# `ar rcD` (GNU) terminates short member names with '/', which appinstalld rejects outright:
# "Failed to extract package", error_code -5, before a single file is unpacked. Nothing else in
# the pipeline notices — webosbrew-ipk-verify reads such an archive happily.
if built:
    blob = built[0].read_bytes()
    check(blob[:8] == b"!<arch>\n", "starts with the ar global header")
    members, off = [], 8
    while off + 60 <= len(blob):
        name = blob[off:off + 16].decode("latin-1").rstrip()
        size = int(blob[off + 48:off + 58].decode("latin-1").strip() or 0)
        members.append(name)
        off += 60 + size + (size % 2)
    check(members == ["debian-binary", "control.tar.gz", "data.tar.gz"],
          f"members are the three bare names in order (saw {members})")

print()
if FAILURES:
    for f in FAILURES:
        print(f"::error::{f}")
    sys.exit(1)
print("all packaging assertions passed")
