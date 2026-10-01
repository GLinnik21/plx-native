#!/usr/bin/env python3
"""No CI job, apt refresh or download may be able to hang for hours.

CI run 36904995113 sat 4 h 36 min on `apt-get update` (last log line: a `Get:5 https://...
InRelease`), because no job set `timeout-minutes` (GitHub's default is 360) and the update had no
`timeout` of its own. Both are silent until the day a mirror stalls, so both are pinned here:

* every job of every workflow sets an integer `timeout-minutes` (at most MAX_MINUTES). A job that
  calls a reusable workflow (`uses:`) cannot carry the key; its limit lives on the called
  workflow's jobs, which this test covers like any other;
* every `apt-get update` in a workflow or composite action runs under `timeout`;
* every `curl` in a workflow or composite action has `--max-time`.

Text-level on purpose, like ci/test_ci_split.py (no YAML library on a stock runner): the files are
ours and regular. The checkers take text, so the tests at the bottom can prove each one goes red on
a mutated copy.
"""
from __future__ import annotations

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GITHUB = ROOT / ".github"
# Generous ceiling: the largest limit in use is 50 minutes (the macOS simulator).
MAX_MINUTES = 120


def yaml_files():
    return sorted(list((GITHUB / "workflows").glob("*.yml")) + list((GITHUB / "actions").glob("*/action.yml")))


def job_problems(name, text):
    """Problems with the `jobs:` of one workflow's text (empty for a composite action)."""
    lines = text.splitlines()
    start = next((i for i, l in enumerate(lines) if l.rstrip() == "jobs:"), None)
    if start is None:
        return []
    jobs = {}  # id -> its body lines
    cur = None
    for line in lines[start + 1:]:
        if line and not line.startswith((" ", "#")):
            break  # the next top-level key ends `jobs:`
        m = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
        if m:
            cur = m.group(1)
            jobs[cur] = []
        elif cur is not None:
            jobs[cur].append(line)
    problems = []
    for job, body in jobs.items():
        if any(re.match(r"^    uses:", l) for l in body):
            continue  # a reusable-workflow call: GitHub rejects `timeout-minutes` here
        found = [re.match(r"^    timeout-minutes:\s*(\S+)", l) for l in body]
        found = [m for m in found if m]
        if not found:
            problems.append(f"{name}: job '{job}' has no timeout-minutes (GitHub's default is 360)")
        elif not found[0].group(1).isdigit() or not 1 <= int(found[0].group(1)) <= MAX_MINUTES:
            problems.append(f"{name}: job '{job}' timeout-minutes {found[0].group(1)!r} is not an integer in 1..{MAX_MINUTES}")
    return problems


def code_lines(text):
    """Non-comment lines, with their 1-based numbers."""
    return [(i, l) for i, l in enumerate(text.splitlines(), 1) if not l.lstrip().startswith("#")]


def apt_update_problems(name, text):
    return [
        f"{name}:{i}: `apt-get update` is not wrapped in `timeout`"
        for i, l in code_lines(text)
        if re.search(r"apt-get\s+update", l) and not re.search(r"\btimeout\s+\d+\s+(sudo\s+)?apt-get\s+update", l)
    ]


def curl_problems(name, text):
    return [
        f"{name}:{i}: `curl` has no --max-time"
        for i, l in code_lines(text)
        if re.search(r"(^|[\s;&|(])curl\s", l) and "--max-time" not in l
    ]


class CiTimeouts(unittest.TestCase):
    def test_every_job_has_a_timeout(self):
        problems = [p for f in yaml_files() for p in job_problems(f.name, f.read_text())]
        self.assertEqual(problems, [])

    def test_every_apt_update_is_bounded(self):
        problems = [p for f in yaml_files() for p in apt_update_problems(f.name, f.read_text())]
        self.assertEqual(problems, [])

    def test_every_curl_is_bounded(self):
        problems = [p for f in yaml_files() for p in curl_problems(f.name, f.read_text())]
        self.assertEqual(problems, [])

    def test_the_gates_see_something(self):
        # A parser that silently matched nothing would pass everything above.
        names = {f.name for f in yaml_files()}
        self.assertTrue({"ci.yml", "simulators.yml", "build-package.yml", "action.yml"} <= names)
        total = sum(len(re.findall(r"^    timeout-minutes:", f.read_text(), re.M)) for f in yaml_files())
        self.assertGreaterEqual(total, 18)


class CheckersGoRed(unittest.TestCase):
    """Each checker must fail on a mutated copy of a real file, or the gate above proves nothing."""

    def test_job_without_timeout_is_caught(self):
        text = (GITHUB / "workflows/ci.yml").read_text()
        self.assertEqual(job_problems("ci.yml", text), [])
        mutated = re.sub(r"^    timeout-minutes:.*\n", "", text, count=1, flags=re.M)
        self.assertEqual(len(job_problems("ci.yml", mutated)), 1)

    def test_non_numeric_or_huge_timeout_is_caught(self):
        text = (GITHUB / "workflows/ci.yml").read_text()
        for bad in ("${{ inputs.t }}", "360", "0"):
            mutated = re.sub(r"^(    timeout-minutes:).*$", r"\1 " + bad, text, count=1, flags=re.M)
            self.assertEqual(len(job_problems("ci.yml", mutated)), 1, bad)

    def test_reusable_workflow_call_is_exempt(self):
        text = (GITHUB / "workflows/nightly.yml").read_text()
        self.assertIn("    uses: ./.github/workflows/build-package.yml", text)
        self.assertEqual(job_problems("nightly.yml", text), [])

    def test_unwrapped_apt_update_is_caught(self):
        text = (GITHUB / "actions/apt-install/action.yml").read_text()
        self.assertEqual(apt_update_problems("apt-install", text), [])
        mutated = text.replace("timeout 120 sudo apt-get update", "sudo apt-get update")
        self.assertNotEqual(mutated, text)
        self.assertTrue(apt_update_problems("apt-install", mutated))
        # The shape that caused the 4 h 36 min stall.
        self.assertTrue(apt_update_problems("x", "        run: |\n          sudo apt-get update\n"))

    def test_unbounded_curl_is_caught(self):
        text = (GITHUB / "workflows/ci.yml").read_text()
        self.assertEqual(curl_problems("ci.yml", text), [])
        mutated = text.replace("--max-time 120 ", "")
        self.assertNotEqual(mutated, text)
        self.assertTrue(curl_problems("ci.yml", mutated))


if __name__ == "__main__":
    unittest.main()
