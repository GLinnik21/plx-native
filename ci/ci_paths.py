#!/usr/bin/env python3
"""ci.yml's `paths` list (the `&ci_paths` anchor on its `push` trigger), evaluated with GitHub's rules.

One source of truth, two readers:

* ci/test_ci_workflows.py pins what the list must and must not trigger;
* ci.yml's `changes` job runs this as a program on a pull request's changed files, because the
  `pull_request` trigger carries NO `paths` filter any more (a filtered-out PR starts no run, so the
  required status checks never report and it can only be admin-merged). The job decides whether the
  heavy jobs run, using the very list `push` filters on.

The rules (docs.github.com, "Workflow syntax", filter pattern cheat sheet): patterns are evaluated
in order; a `!` pattern removes what an earlier pattern matched, a later positive pattern puts it
back; `*` matches any characters but `/`; `**` matches any characters including `/`, and `**/` also
matches no directory at all; `?` matches one character but `/`.

    ci_paths.py [--workflow FILE] < changed-files     prints `true` when any line triggers, else `false`

Reads one path per line on stdin. An empty list prints `true` (fail closed: nothing was learned).
Stdlib only: it runs on a bare runner before anything is installed.
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

WORKFLOW = Path(__file__).resolve().parent.parent / ".github/workflows/ci.yml"
ANCHOR = "paths: &ci_paths"


def patterns(workflow_text):
    """The `&ci_paths` list of a workflow's text, in order, quotes stripped."""
    lines = workflow_text.splitlines()
    start = next(i for i, line in enumerate(lines) if line.strip() == ANCHOR)
    out = []
    for line in lines[start + 1:]:
        stripped = line.strip()
        if stripped.startswith("#") or not stripped:
            continue
        if not stripped.startswith("- "):
            break
        out.append(stripped[2:].strip().strip("'\""))
    if not out:
        raise ValueError("the &ci_paths list is empty")
    return out


def regex(pattern):
    out, i = [], 0
    while i < len(pattern):
        if pattern.startswith("**/", i):
            out.append("(?:.*/)?")
            i += 3
        elif pattern.startswith("**", i):
            out.append(".*")
            i += 2
        elif pattern[i] == "*":
            out.append("[^/]*")
            i += 1
        elif pattern[i] == "?":
            out.append("[^/]")
            i += 1
        else:
            out.append(re.escape(pattern[i]))
            i += 1
    return re.compile("".join(out) + r"\Z")


def triggers(path, pats):
    """Whether `path` is included by the ordered include/exclude list `pats`."""
    included = False
    for pattern in pats:
        negative = pattern.startswith("!")
        if regex(pattern[1:] if negative else pattern).match(path):
            included = not negative
    return included


def gated(paths, pats):
    """True when any changed path triggers (or when there are none: fail closed)."""
    paths = [p for p in paths if p]
    return not paths or any(triggers(p, pats) for p in paths)


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--workflow", type=Path, default=WORKFLOW)
    args = ap.parse_args(argv)
    pats = patterns(args.workflow.read_text())
    print("true" if gated(sys.stdin.read().splitlines(), pats) else "false")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
