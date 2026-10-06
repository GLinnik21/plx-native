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
between flavours — `id` and `title` — except for `nightly`, which also moves `version` (to
`<next X.Y>.<cut date>`, see `appinfo_for`). The other thirteen (twelve for nightly) — `type`,
`main`, `transparent`, `requiredMemory`, `nativeLifeCycleInterfaceVersion`, `handlesRelaunch`,
`splashBackground`, `iconColor`, `vendor`, `appDescription` and the two icon FILENAMES, plus
`version` for every flavour but nightly — are behaviour-critical and must never drift. A second
checked-in descriptor would drift on them the first time one was edited, and would put the version
in a fifth file that `ci/bump-version.py`, `ci/check-package.py` and `release.yml`'s tag guard all
already read. The selftest asserts the set of moved keys is exactly `{id, title}` for debug and
`{id, title, version}` for nightly, so widening either is a decision somebody has to make on
purpose.

**THE STABLE TRANSFORM IS THE IDENTITY, and that is asserted rather than intended** (`--selftest`,
run by `make check`). It is the whole mechanical guarantee that adding a second identity cannot
perturb the artifact whose sha256 every user's television verifies at install time: if the stable
descriptors come out byte-identical to the tracked files, the package built from them is the
package that was always built.

The install list and storage identities live in `ci/install-identities.json`; `build.rs`
generates the Rust storage schema from it. The stable app id also anchors `paths::STABLE_APP_ID`
and `APPID_STABLE` in the Makefile; `--selftest` checks those existing path conventions against
the manifest.
"""
from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import version_rule  # noqa: E402  — ci/version_rule.py, the shared "next X.Y.Z" arithmetic

ROOT = Path(__file__).resolve().parent.parent

# The packaging boundary owns the install identities. build.rs generates the Rust Flavor
# schema and lookups from this same data: adding an install must also reach both storage peers.
INSTALL_IDENTITIES = json.loads((ROOT / "ci/install-identities.json").read_text())
FLAVORS = tuple(identity["name"] for identity in INSTALL_IDENTITIES)
STABLE_ID = next(identity["app_id"] for identity in INSTALL_IDENTITIES
                 if identity["name"] == "stable")


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
    return next(identity["app_id"] for identity in INSTALL_IDENTITIES
                if identity["name"] == flavor)


def _nightly_date(nightly_date: "str | None") -> str:
    """The `YYYYMMDD` a nightly package is cut for: the argument, else the `PLX_NIGHTLY_DATE` the
    Makefile exports for `FLAVOR=nightly` (the SAME variable `rust-modules/build.rs` dates the
    reported version by, so the package and the binary inside it cannot disagree about the day).
    No fallback to "today": a nightly package with a guessed date would be a different package
    from the binary it carries, so absence is an error."""
    date = nightly_date if nightly_date is not None else os.environ.get("PLX_NIGHTLY_DATE", "")
    if not re.fullmatch(r"[0-9]{8}", date):
        raise SystemExit(
            "a nightly package's version carries its cut date: pass nightly_date or set "
            f"PLX_NIGHTLY_DATE=YYYYMMDD (got {date!r})")
    return date


def appinfo_for(flavor: str, nightly_date: "str | None" = None) -> dict:
    """The tracked `pkg/appinfo.json`, re-pointed at `flavor`. Identity when flavor == stable.

    Only `id`, `title` and — for `nightly` ONLY — `version` move. The icon FIELDS deliberately do
    not: they name `icon.png` and `largeIcon.png`, and the badged artwork is staged over those
    basenames from `pkg/dev/` (or `pkg/nightly/`) by the Makefile — so the flavour lives in the directory a file is
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
    # NIGHTLY ONLY: the package version itself moves — to the next minor (or next patch, on a
    # maintenance line) with the CUT DATE as its patch (`version_rule.nightly_package_triplet`),
    # e.g. `0.7.20260919`. Two properties are wanted and both come from that. It is ahead of
    # whatever stable is at, or LG's installer could never move a nightly install forward from the
    # stable it was cut after. And it differs every day, because Homebrew Channel offers an
    # update only when the manifest's version STRING differs from the installed one — a version
    # that stayed `0.7.0` for the whole cycle would never be offered between two nightlies. It is
    # still three integers (LG's installer takes nothing else), which is why the date is the patch
    # and not a suffix. `ci/check-package.py`'s `--selftest` is what keeps this the ONLY flavour
    # allowed to move `version` — see its `moved` assertion.
    if flavor == "nightly":
        triplet, err = version_rule.nightly_package_triplet(
            a["version"], _release_line_content(), _nightly_date(nightly_date))
        if err:
            raise SystemExit(err)
        a["version"] = "{}.{}.{}".format(*triplet)
    return a


def control_for(text: str, flavor: str, nightly_date: "str | None" = None) -> str:
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
        version = appinfo_for(flavor, nightly_date)["version"]
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
    nightly = appinfo_for("nightly", "20260919")
    check(nightly["id"] == f"{STABLE_ID}.nightly",
          f'nightly appinfo id == {STABLE_ID}.nightly (got {nightly["id"]})')
    check(nightly["title"] == f'{tracked_appinfo["title"]} Nightly',
          f'nightly appinfo title is "{tracked_appinfo["title"]} Nightly" (got {nightly["title"]!r})')
    _tracked_major, _tracked_minor, _ = (int(x) for x in tracked_appinfo["version"].split("."))
    _release_line = _release_line_content()
    _want_major_minor = (f"{_tracked_major}.{_tracked_minor + 1}" if _release_line is None
                         else f"{_tracked_major}.{_tracked_minor}")
    check(nightly["version"] == f"{_want_major_minor}.20260919",
          "nightly appinfo version is the next minor (next patch's line on a maintenance branch) "
          f'with the cut date as its patch (got {nightly["version"]!r})')
    check(re.fullmatch(r"\d+\.\d+\.\d+", nightly["version"]) is not None,
          "nightly appinfo version is still exactly three integers — all LG's installer accepts")
    nightly_next = appinfo_for("nightly", "20260920")
    check(nightly_next["version"] != nightly["version"]
          and int(nightly_next["version"].rsplit(".", 1)[1]) > int(nightly["version"].rsplit(".", 1)[1]),
          "two nightlies cut on different days carry different, increasing package versions "
          "(Homebrew Channel only offers an update when the version string differs)")
    for bad in ("2026091", "202609190", "2026-09-1", "", "abcdefgh", "２０２６０９１９"):
        try:
            appinfo_for("nightly", bad)
        except SystemExit:
            check(True, f"nightly package refuses a malformed date {bad!r}")
        else:
            check(False, f"nightly package refuses a malformed date {bad!r}")
    _saved_env = os.environ.pop("PLX_NIGHTLY_DATE", None)
    try:
        try:
            appinfo_for("nightly")
        except SystemExit:
            check(True, "nightly package with no date argument and no PLX_NIGHTLY_DATE is an error")
        else:
            check(False, "nightly package with no date argument and no PLX_NIGHTLY_DATE is an error")
        os.environ["PLX_NIGHTLY_DATE"] = "20260919"
        check(appinfo_for("nightly") == nightly,
              "PLX_NIGHTLY_DATE (what the Makefile exports) supplies the date when none is passed")
    finally:
        os.environ.pop("PLX_NIGHTLY_DATE", None)
        if _saved_env is not None:
            os.environ["PLX_NIGHTLY_DATE"] = _saved_env
    check(appinfo_for("stable", "20260919") == tracked_appinfo and
          appinfo_for("debug", "20260919") == dbg,
          "a date argument changes nothing for stable and debug")
    nightly_moved = {k for k in tracked_appinfo if nightly.get(k) != tracked_appinfo[k]}
    check(nightly_moved == {"id", "title", "version"},
          "only id, title and version differ between nightly and stable (also saw "
          f"{sorted(nightly_moved - {'id', 'title', 'version'})})")
    nightly_control = control_for(tracked_control, "nightly", "20260919")
    check(f"Package: {STABLE_ID}.nightly" in nightly_control,
          "nightly control Package is the nightly id")
    check(f"Version: {nightly['version']}" in nightly_control,
          "nightly control Version is the bumped nightly package version")

    # The same string, in three languages that cannot see each other.
    rust = (ROOT / "rust-modules/base/src/paths.rs").read_text()
    check(f'STABLE_APP_ID: &str = "{STABLE_ID}"' in rust,
          "rust-modules/base/src/paths.rs STABLE_APP_ID agrees")
    mk = (ROOT / "Makefile").read_text()
    check(re.search(rf"(?m)^APPID_STABLE\s*=\s*{re.escape(STABLE_ID)}\s*$", mk) is not None,
          "Makefile APPID_STABLE agrees")
    mk_flavors = re.search(r"(?m)^FLAVORS\s*:=\s*(.+)$", mk)
    check(mk_flavors is not None and mk_flavors.group(1) == "$(shell python3 ci/flavor.py --list)",
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
    if sys.argv[1:] == ["--list"]:
        print(" ".join(FLAVORS))
        sys.exit(0)
    if sys.argv[1:2] == ["--selftest"]:
        print("== flavour transform ==")
        sys.exit(_selftest())
    if len(sys.argv) == 2:
        print(app_id(sys.argv[1]))
        sys.exit(0)
    print(__doc__)
    sys.exit(2)
