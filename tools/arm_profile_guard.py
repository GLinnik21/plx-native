#!/usr/bin/env python3
"""Refuse to grade frame timing on a binary built with the fast `tvdev` cargo profile.

A plain local `make deploy` compiles the ARM library with `[profile.tvdev]` (no LTO, 16 codegen
units; the Makefile's `ARM_PROFILE`), which is larger and slower in per-frame code than the shipped
`release` build. A frame-rate number measured on it reads as a regression that is not one, or
hides one that is, and nothing in the output says why. Every path that measures or grades
performance on the television therefore asks THIS module what profile the DEPLOYED binary came from
and stops if it is `tvdev`:

* `tests/run.py --fps / --fps-player / --graphics-profile` (and its `--build`, which builds with
  `ARM_PROFILE=release` for those runs),
* `tools/profile-graphics`, `tools/tv-sched-trace.sh`.

The record is a one-line file, `arm-profile`, that `make deploy` writes into the app directory next
to the binary (`release` or `tvdev`), so it describes what is actually installed, including a
binary deployed earlier by a plain `make deploy` or from another checkout. A directory with no such
file holds a binary deployed before the record existed (always `release`) or installed from a
package (CI builds, always `release`); that is reported as `unrecorded` and accepted, because the
alternative is refusing every run until the next deploy. Functional (non-timing) tests do not call
this and keep working on `tvdev`, which is the point of the profile.

    python3 tools/arm_profile_guard.py [--flavor debug]     # exit 1 on tvdev, prints the fix

Importable: `check(run, appdir)` takes any `run(command) -> object with .stdout` so each caller
keeps its own ssh helper, and nothing here touches the television by itself.
"""
from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path
from typing import Callable, Optional

ROOT = Path(__file__).resolve().parents[1]
RECORD = "arm-profile"          # the file `make deploy` writes in the app directory
PROFILES = ("release", "tvdev")
UNRECORDED = "unrecorded"
FIX = "make ARM_PROFILE=release deploy"


def parse_record(text: str) -> Optional[str]:
    """`release` / `tvdev` from the record's text, or None when it is empty or not one of those."""
    word = (text or "").split()
    return word[0].lower() if word and word[0].lower() in PROFILES else None


def refusal(profile: str, appid: str = "the deployed binary") -> Optional[str]:
    """The one-line refusal for a profile that must not be graded, else None."""
    if profile == "tvdev":
        return (f"refusing to grade timing: {appid} was built with the fast tvdev profile (no LTO), "
                f"which is not the shipped build. Rebuild with `{FIX}` "
                "(tests/run.py --build does that for timing runs) and run again.")
    return None


def read_deployed(run: Callable[[str], object], appdir: str) -> str:
    """The profile record on the television: `release`, `tvdev` or `unrecorded`.

    `run` executes a shell command there and returns an object with a `stdout` string. A missing
    file, an unreadable one and a garbled one are all `unrecorded`; only a clear `tvdev` refuses.
    """
    out = run(f"cat {appdir}/{RECORD} 2>/dev/null")
    return parse_record(getattr(out, "stdout", "") or "") or UNRECORDED


def check(run: Callable[[str], object], appdir: str, appid: str = "the deployed binary") -> tuple[str, Optional[str]]:
    """(profile label to record in the report, refusal message or None)."""
    profile = read_deployed(run, appdir)
    return profile, refusal(profile, appid)


def describe(profile: str) -> str:
    """The line a perf run prints and records."""
    if profile == UNRECORDED:
        return ("unrecorded (deployed before `make deploy` wrote the record, or installed from a "
                "package: both are release builds)")
    return profile


def _make_query(flavor: str) -> tuple[str, str, str]:
    proc = subprocess.run(["make", "-s", f"FLAVOR={flavor}", "print-tv", "print-appid", "print-appdir"],
                          cwd=ROOT, check=True, text=True, stdout=subprocess.PIPE)
    values = proc.stdout.splitlines()
    if len(values) != 3:
        raise SystemExit("cannot resolve TV/install paths from Makefile")
    return values[0], values[1], values[2]


def main(argv: Optional[list[str]] = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--flavor", default="debug")
    args = ap.parse_args(argv)
    tv, appid, appdir = _make_query(args.flavor)

    def run(command: str):
        return subprocess.run([str(ROOT / "tools" / "tv-ssh"), "ssh", f"root@{tv}", command],
                              text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)

    profile, message = check(run, appdir, appid)
    if message:
        print(f"arm-profile-guard: {message}", file=sys.stderr)
        return 1
    print(f"arm-profile-guard: {appid} binary profile: {describe(profile)}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
