#!/usr/bin/env python3
"""Which install a package is for — the ONE transform from the tracked descriptors to a flavour's.

Two builds of this app live on one television: `stable` is what users install
(`com.beb.plxnative` — the id in every release, every manifest and the webosbrew channel listing),
and `debug` is the day-to-day developer build beside it (`com.beb.plxnative.debug`), with its own
launcher tile, its own sign-in and its own `/tmp` root. The Makefile's FLAVOR block is the account
of why; this file is the part that has to be identical in three places at once.

**PATCH, DO NOT DUPLICATE.** `pkg/appinfo.json` has 14 fields and exactly TWO of them may differ
between flavours: `id` and `title`. The other twelve — `type`, `main`, `transparent`,
`requiredMemory`, `nativeLifeCycleInterfaceVersion`, `handlesRelaunch`, `splashBackground`,
`iconColor`, `vendor`, `version` and the two icon FILENAMES — are behaviour-critical and must never
drift. A second checked-in descriptor would drift on them the first time one was edited, and would
put the version in a fifth file that `ci/bump-version.py`, `ci/check-package.py` and
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

ROOT = Path(__file__).resolve().parent.parent

#: The app id users install. Everything else is this plus a dotted suffix.
STABLE_ID = "com.beb.plxnative"

#: The flavours the Makefile will accept. A typo here would mint a third registered app on the
#: television whose only symptom is a mystery tile, which is why the Makefile whitelists too.
FLAVORS = ("stable", "debug")


def app_id(flavor: str) -> str:
    """`com.beb.plxnative` for stable, `com.beb.plxnative.<flavour>` otherwise."""
    if flavor not in FLAVORS:
        raise SystemExit(f"unknown flavour {flavor!r} — one of: {', '.join(FLAVORS)}")
    return STABLE_ID if flavor == "stable" else f"{STABLE_ID}.{flavor}"


def appinfo_for(flavor: str, appinfo: dict | None = None) -> dict:
    """The tracked `pkg/appinfo.json`, re-pointed at `flavor`. Identity when flavor == stable.

    Only `id` and `title` move. The icon FIELDS deliberately do not: they name `icon.png` and
    `largeIcon.png`, and the badged artwork is staged over those basenames from `pkg/dev/` by the
    Makefile — so the flavour lives in the directory a file is read from and never in the name it
    is packaged under. `ci/check-package.py` grades the payload by basename, and appinfo's own
    fields have to match what is in the box.
    """
    a = dict(json.loads((ROOT / "pkg/appinfo.json").read_text()) if appinfo is None else appinfo)
    if flavor == "stable":
        return a
    a["id"] = app_id(flavor)
    # The launcher shows this under the tile. Two tiles reading `PlxNative` would be a coin flip
    # every time, and the badged icon only helps someone who is looking at the artwork rather than
    # at a list — `dev/listApps` and SAM's own dialogs show the title, not the icon.
    a["title"] = f"{a['title']} {flavor}"
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
          f"only id and title differ between flavours (also saw {sorted(moved - {'id', 'title'})})")

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
    # It also fails deliberately if a THIRD flavour is added: "stable or one higher" stops being a
    # rule at that point and somebody has to decide, in both languages.
    cap = (ROOT / "rust-modules/src/capture.rs").read_text()
    rs_stable = re.search(r"(?m)^const STABLE_PORT: u16 = (\d+);", cap)
    mk_port = re.search(r"(?m)^APPPORT\s*=\s*\$\(if \$\(filter stable,\$\(FLAVOR\)\),(\d+),(\d+)\)", mk)
    check(rs_stable is not None and mk_port is not None
          and rs_stable.group(1) == mk_port.group(1)
          and int(mk_port.group(2)) == int(mk_port.group(1)) + 1
          and "Some(_) => STABLE_PORT + 1," in cap,
          "capture port: Makefile APPPORT and capture::default_port agree (stable, stable+1)")
    check(len(FLAVORS) == 2,
          "the capture-port rule is 'stable, or one higher' — a third flavour needs a real rule "
          "in BOTH capture.rs and the Makefile")

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
    sys.exit(__doc__ and print(__doc__) or 2)
