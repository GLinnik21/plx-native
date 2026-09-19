#!/usr/bin/env python3
"""Which install a package is for — a transform from the tracked descriptors to a flavour's.

Three builds of this app live on one television: `stable` is what users install
(`com.beb.plxnative` — the id in every release, every manifest and the webosbrew channel listing),
`debug` is the day-to-day developer build beside it (`com.beb.plxnative.debug`), with its own
launcher tile, its own sign-in and its own `/tmp` root, and `nightly` (`com.beb.plxnative.nightly`)
is a third install beside both — always a `RELEASE=1` build (no dev triggers, ever), with its own
tile ("PlxNative Nightly"), its own sign-in and its own `/tmp` root, that additionally carries a
PACKAGE version ahead of the tracked one (see `appinfo_for`'s nightly arm) and a dated REPORTED
version (`rust-modules/build.rs::emit_version`'s `PLX_CHANNEL=nightly` arm). The Makefile's FLAVOR
block is the account of why; this file is the part that has to be identical in three places at
once.

**PATCH, DO NOT DUPLICATE.** `pkg/appinfo.json` has 15 fields and exactly TWO of them may differ
between flavours: `id` and `title`. The other thirteen — `type`, `main`, `transparent`,
`requiredMemory`, `nativeLifeCycleInterfaceVersion`, `handlesRelaunch`, `splashBackground`,
`iconColor`, `vendor`, `version`, `appDescription` and the two icon FILENAMES — are
behaviour-critical and must never drift. A second checked-in descriptor would drift on them the
first time one was edited, and would put the version in a fifth file that `ci/bump-version.py`, `ci/check-package.py` and
`release.yml`'s tag guard all already read. The selftest asserts the set of moved keys is exactly
`{id, title}`, so widening it is a decision somebody has to make on purpose.

**THE STABLE TRANSFORM IS THE IDENTITY, and that is asserted rather than intended** (`--selftest`,
run by `make check`). It is the whole mechanical guarantee that adding a second identity cannot
perturb the artifact whose sha256 every user's television verifies at install time: if the stable
descriptors come out byte-identical to the tracked files, the package built from them is the
package that was always built.

The id is spelled in three languages that cannot see each other — here, `paths::STABLE_APP_ID` in
Rust, and `APPID_STABLE` in the Makefile. `--selftest` reads the other two and compares, because
"three copies of one string" is only safe while something checks.
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import version_rule  # noqa: E402  — ci/version_rule.py, the shared "next X.Y.Z" arithmetic

ROOT = Path(__file__).resolve().parent.parent

#: The app id users install. Everything else is this plus a dotted suffix.
STABLE_ID = "com.beb.plxnative"

#: The flavours the Makefile will accept. A typo here would mint a fourth registered app on the
#: television whose only symptom is a mystery tile, which is why the Makefile whitelists too.
FLAVORS = ("stable", "debug", "nightly")


def _release_line_content() -> "str | None":
    """The tracked `RELEASE_LINE` marker's text, or `None` when this checkout is trunk.

    Read fresh rather than cached: this module is imported once per process by short-lived CI/make
    invocations, never a long-running one, so there is no staleness window to guard against.
    """
    p = ROOT / "RELEASE_LINE"
    return p.read_text() if p.is_file() else None


def app_id(flavor: str) -> str:
    """`com.beb.plxnative` for stable, `com.beb.plxnative.<flavour>` otherwise."""
    if flavor not in FLAVORS:
        raise SystemExit(f"unknown flavour {flavor!r} — one of: {', '.join(FLAVORS)}")
    return STABLE_ID if flavor == "stable" else f"{STABLE_ID}.{flavor}"


def appinfo_for(flavor: str) -> dict:
    """The tracked `pkg/appinfo.json`, re-pointed at `flavor`. Identity when flavor == stable.

    Only `id`, `title` and — for `nightly` ONLY — `version` move. The icon FIELDS deliberately do
    not: they name `icon.png` and `largeIcon.png`, and the badged artwork is staged over those
    basenames from `pkg/dev/` by the Makefile — so the flavour lives in the directory a file is
    read from and never in the name it is packaged under. `ci/check-package.py` grades the payload
    by basename, and appinfo's own fields have to match what is in the box.
    """
    a = dict(json.loads((ROOT / "pkg/appinfo.json").read_text()))
    if flavor == "stable":
        return a
    a["id"] = app_id(flavor)
    # The launcher shows this under the tile. Two tiles reading `PlxNative` would be a coin flip
    # every time, and the badged icon only helps someone who is looking at the artwork rather than
    # at a list — `dev/listApps` and SAM's own dialogs show the title, not the icon. Nightly's own
    # title is a product decision ("PlxNative Nightly", capitalised) rather than the bare lowercase
    # suffix debug uses, so it is spelled out rather than titlecased generically.
    suffix = "Nightly" if flavor == "nightly" else flavor
    a["title"] = f"{a['title']} {suffix}"
    # NIGHTLY ONLY: the package version itself moves to the next minor (or next patch, on a
    # maintenance line) — the SAME arithmetic `rust-modules/build.rs::emit_version` uses for a
    # `-dev` build's REPORTED version, reused here via `ci/version_rule.py` rather than
    # re-derived. A nightly install has to carry a package version LG's installer treats as newer
    # than whatever stable is at, or it could never upgrade itself between nightly builds cut on
    # the same tracked version. `ci/check-package.py`'s `--selftest` is what keeps this the ONLY
    # flavour allowed to move `version` — see its `moved` assertion.
    if flavor == "nightly":
        triplet, err = version_rule.next_version_triplet(a["version"], _release_line_content())
        if err:
            raise SystemExit(err)
        a["version"] = "{}.{}.{}".format(*triplet)
    return a


def control_for(text: str, flavor: str) -> str:
    """The tracked control file's text with `Package:` re-pointed at `flavor`.

    Assembled in memory and never written back to `ipkroot/ctl/control`, for the same reason
    `mkipk.py` assembles `Installed-Size` that way: a tracked file rewritten per flavour makes
    every `make ipk` dirty the worktree and invites committing whichever value happened to be last
    — a value that is then wrong for the other flavour.
    """
    if flavor == "stable":
        return text
    out, n = re.subn(r"(?m)^Package: .*$", f"Package: {app_id(flavor)}", text, count=1)
    if n != 1:
        raise SystemExit("control file has no Package: line to re-point")
    if flavor == "nightly":
        # The one flavour whose PACKAGE version itself moves (see `appinfo_for`) — the control
        # file's `Version:` field has to move with it, or the archive's own two version witnesses
        # (control vs appinfo) would disagree, which `ci/check-package.py` already grades.
        version = appinfo_for(flavor)["version"]
        out, n = re.subn(r"(?m)^Version: .*$", f"Version: {version}", out, count=1)
        if n != 1:
            raise SystemExit("control file has no Version: line to re-point")
    return out


def _selftest() -> int:
    """Assert the stable transform is the identity, and that all three spellings of the id agree."""
    fails = []

    def check(cond: bool, msg: str) -> None:
        print(f"  {'ok  ' if cond else 'FAIL'} — {msg}")
        if not cond:
            fails.append(msg)

    tracked_appinfo = json.loads((ROOT / "pkg/appinfo.json").read_text())
    tracked_control = (ROOT / "ipkroot/ctl/control").read_text()

    # THE guarantee: nothing about the released package moves.
    check(appinfo_for("stable") == tracked_appinfo,
          "appinfo_for('stable') is the identity — the released descriptor cannot move")
    check(control_for(tracked_control, "stable") == tracked_control,
          "control_for('stable') is the identity — the released control file cannot move")
    check(tracked_appinfo["id"] == STABLE_ID,
          f"pkg/appinfo.json id == STABLE_ID ({STABLE_ID})")

    # ...and a flavoured one really is a different app, on every witness webOS reads.
    dbg = appinfo_for("debug")
    check(dbg["id"] == f"{STABLE_ID}.debug", f'debug appinfo id == {STABLE_ID}.debug (got {dbg["id"]})')
    check(dbg["title"] != tracked_appinfo["title"],
          f'debug appinfo title differs from stable ({dbg["title"]!r})')
    check(f"Package: {STABLE_ID}.debug" in control_for(tracked_control, "debug"),
          "debug control Package is the debug id")
    # Everything else must be untouched. A drifted `requiredMemory` or `transparent` is a
    # behaviour change that would show up only on the television, on the flavour nobody releases.
    moved = {k for k in tracked_appinfo if dbg.get(k) != tracked_appinfo[k]}
    check(moved == {"id", "title"},
          f"only id and title differ between debug and stable (also saw {sorted(moved - {'id', 'title'})})")

    # Nightly is a different app too, AND its package version moves ahead of the tracked one —
    # the one flavour allowed to widen the `moved` set, asserted explicitly rather than by relaxing
    # the debug check above.
    nightly = appinfo_for("nightly")
    check(nightly["id"] == f"{STABLE_ID}.nightly",
          f'nightly appinfo id == {STABLE_ID}.nightly (got {nightly["id"]})')
    check(nightly["title"] == f'{tracked_appinfo["title"]} Nightly',
          f'nightly appinfo title is "{tracked_appinfo["title"]} Nightly" (got {nightly["title"]!r})')
    _tracked_major, _tracked_minor, _ = (int(x) for x in tracked_appinfo["version"].split("."))
    check(nightly["version"] == f"{_tracked_major}.{_tracked_minor + 1}.0",
          f'nightly appinfo version is the next minor on trunk (got {nightly["version"]!r})')
    nightly_moved = {k for k in tracked_appinfo if nightly.get(k) != tracked_appinfo[k]}
    check(nightly_moved == {"id", "title", "version"},
          "only id, title and version differ between nightly and stable (also saw "
          f"{sorted(nightly_moved - {'id', 'title', 'version'})})")
    nightly_control = control_for(tracked_control, "nightly")
    check(f"Package: {STABLE_ID}.nightly" in nightly_control,
          "nightly control Package is the nightly id")
    check(f"Version: {nightly['version']}" in nightly_control,
          "nightly control Version is the bumped nightly package version")

    # The same string, in three languages that cannot see each other.
    rust = (ROOT / "rust-modules/src/paths.rs").read_text()
    check(f'STABLE_APP_ID: &str = "{STABLE_ID}"' in rust,
          "rust-modules/src/paths.rs STABLE_APP_ID agrees")
    mk = (ROOT / "Makefile").read_text()
    check(re.search(rf"(?m)^APPID_STABLE\s*=\s*{re.escape(STABLE_ID)}\s*$", mk) is not None,
          "Makefile APPID_STABLE agrees")
    mk_flavors = re.search(r"(?m)^FLAVORS\s*=\s*(.+)$", mk)
    check(mk_flavors is not None and tuple(mk_flavors.group(1).split()) == FLAVORS,
          f"Makefile FLAVORS agrees ({' '.join(FLAVORS)})")

    # The capture listener's port is the one value spelled in BOTH Rust and make with no shared
    # source, because a shell cannot call into the binary and the binary cannot read the Makefile.
    # Two installs binding one port fails silently on both sides, so the agreement gets a gate.
    # NIGHTLY WAS THE THIRD FLAVOUR "stable, or one higher" warned about: it needed a real,
    # explicit decision in both languages rather than a wildcard arm, which is why the Makefile's
    # rule and `capture::default_port`'s match are both now three named cases rather than two.
    cap = (ROOT / "rust-modules/src/capture.rs").read_text()
    rs_stable = re.search(r"(?m)^const STABLE_PORT: u16 = (\d+);", cap)
    mk_port = re.search(
        r"(?m)^APPPORT\s*=\s*\$\(if \$\(filter stable,\$\(FLAVOR\)\),(\d+),"
        r"\$\(if \$\(filter nightly,\$\(FLAVOR\)\),(\d+),(\d+)\)\)", mk)
    check(rs_stable is not None and mk_port is not None
          and rs_stable.group(1) == mk_port.group(1)
          and int(mk_port.group(3)) == int(mk_port.group(1)) + 1
          and int(mk_port.group(2)) == int(mk_port.group(1)) + 2
          and 'Some("debug") => STABLE_PORT + 1,' in cap
          and 'Some("nightly") => STABLE_PORT + 2,' in cap,
          "capture port: Makefile APPPORT and capture::default_port agree "
          "(stable, debug=+1, nightly=+2)")
    check(len(FLAVORS) == 3,
          "the capture-port rule now names debug and nightly explicitly — a FOURTH flavour needs "
          "a real decision in both capture.rs and the Makefile, the same way nightly just did")

    # The seven query targets are the FIRST targets in the Makefile, and make takes the first
    # target it sees as the default goal. That made a bare `make` print the flavour and exit 0
    # having built nothing — a failure with no failing exit code, so `make && make deploy` shipped
    # whatever binary happened to be sitting in pkg/. This asserts the same rule make applies:
    # an explicit .DEFAULT_GOAL if there is one, otherwise the first target in the file.
    goal = re.search(r"(?m)^\.DEFAULT_GOAL\s*:?=\s*(\S+)", mk)
    if goal is None:
        first = re.search(r"(?m)^([A-Za-z0-9_.%/][^=\n]*?):(?!=)", mk)
        goal_name = first.group(1).strip() if first else "<none>"
    else:
        goal_name = goal.group(1)
    check(goal_name == "all",
          f"a bare `make` builds the binary (default goal is {goal_name!r}, want 'all')")

    # THE RUNTIME ROOT, the last flavour rule spelled in two languages with nothing comparing them.
    # `Makefile`'s RUNDIR and `paths::resolve_runtime_dir` must agree that stable is bare /tmp and a
    # flavour is /tmp/<app id>. On a divergence the APP writes its triggers, its FIFO and its three
    # logs into one root while `tests/run.py`, `tv-session.sh`, `crash-report.sh` and
    # `stream-screen.py` read the other — both sides silent, and every assertion downstream reports
    # "no line found", which this repository documents as indistinguishable from a total
    # regression. It cannot be caught on the television either: the harness's `install:` check can
    # only fire once it has found the log it is looking for.
    mk_rundir = re.search(
        r"(?m)^RUNDIR\s*=\s*\$\(if \$\(filter stable,\$\(FLAVOR\)\),(\S+),(\S+)\)", mk)
    check(mk_rundir is not None
          and mk_rundir.group(1) == "/tmp"
          and mk_rundir.group(2) == "/tmp/$(APPID)",
          "Makefile RUNDIR: stable is bare /tmp, a flavour is /tmp/<app id>")
    check('const DEFAULT_RUNTIME_DIR: &str = "/tmp"' in rust
          and "if app_id == STABLE_APP_ID {" in rust
          and "return PathBuf::from(DEFAULT_RUNTIME_DIR);" in rust
          and "Path::new(DEFAULT_RUNTIME_DIR).join(app_id)" in rust,
          "paths::resolve_runtime_dir spells the same rule as the Makefile's RUNDIR")

    print()
    for f in fails:
        print(f"::error::{f}")
    return 1 if fails else 0


if __name__ == "__main__":
    if sys.argv[1:2] == ["--selftest"]:
        print("== flavour transform ==")
        sys.exit(_selftest())
    if len(sys.argv) == 2:
        print(app_id(sys.argv[1]))
        sys.exit(0)
    print(__doc__)
    sys.exit(2)
