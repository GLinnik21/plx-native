#!/usr/bin/env python3
"""Per-test outcomes of one CI test job, from libtest's own output, as a small JSON file.

usage: tools/ci-test-results.py --log FILE --job NAME --out FILE

The host unit jobs of ci.yml tee `make check-cargo-unit-*` into a log. Every test binary prints, under a
`Running unittests` header, one `test <name> ... ok|FAILED|ignored` line per test (stable libtest has no JSON
format: `-Z unstable-options --format json` is nightly-only, so the text lines are the machine-readable
form). tools/cargo-test-parallel.py prints the header as `Running unittests (target/debug/deps/plx_ui-1a2b)
[plx_ui, 3.2s]` and plain cargo as `Running unittests src/main.rs (target/debug/deps/plxnative_storage-1a2b)`;
both name the package, and a test is keyed `package::test_path` because a path is only unique per crate.

The file is `{"v": 1, "job", "failed": [key...], "passed": [key...]}`, both sorted. `ignored` tests are
left out (they did not run). A key that appears as both passed and failed within the one log (the same name
run twice) counts as failed. The job's attempt is not in the file: the workflow puts it in the artifact name
(`test-results-<job slug>-<run_attempt>`), which is what tools/ci-history.py reads.

WHY ALL THE PASSES, NOT ONLY THE FAILURES. A flaky test is one that failed in one attempt of a run and
passed in another, and "passed in attempt 2" can only be told from "did not run in attempt 2" if the pass
list is there. It is ~6000 short names, a few tens of kilobytes zipped, in a 14-day artifact; the history
file on the `ci-metrics` branch stores only the tests that were seen both ways.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys
from pathlib import Path

VERSION = 1
# `Running unittests (target/debug/deps/plx_ui-0123abcd) [plx_ui, 3.2s]`  (tools/cargo-test-parallel.py)
PARALLEL_HEADER = re.compile(r"^\s*Running unittests \(.*\) \[([^,\]]+),")
# `Running unittests src/main.rs (target/debug/deps/plxnative_storage-0123abcd)`  (cargo itself)
CARGO_HEADER = re.compile(r"^\s*Running unittests \S+ \((.+)\)\s*$")
TEST_LINE = re.compile(r"^test (\S.*?) \.\.\. (ok|FAILED|ignored)\b")
HASH_SUFFIX = re.compile(r"-[0-9a-f]{8,}$")


def package_of(line: str) -> str | None:
    m = PARALLEL_HEADER.match(line)
    if m:
        return m.group(1)
    m = CARGO_HEADER.match(line)
    return HASH_SUFFIX.sub("", os.path.basename(m.group(1))) if m else None


def parse(log: str) -> tuple[set[str], set[str]]:
    """(passed, failed) keys of every test line in `log`."""
    passed, failed = set(), set()
    package = "unknown"
    for line in log.splitlines():
        header = package_of(line)
        if header is not None:
            package = header
            continue
        m = TEST_LINE.match(line)
        if not m or m.group(2) == "ignored":
            continue
        (failed if m.group(2) == "FAILED" else passed).add(f"{package}::{m.group(1)}")
    return passed - failed, failed


def document(log: str, job: str) -> dict:
    passed, failed = parse(log)
    return {"v": VERSION, "job": job, "failed": sorted(failed), "passed": sorted(passed)}


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--log", required=True)
    ap.add_argument("--job", required=True, help="the job's name, as the history file will show it")
    ap.add_argument("--out", required=True)
    args = ap.parse_args(argv)
    doc = document(Path(args.log).read_text(encoding="utf-8", errors="replace"), args.job)
    Path(args.out).write_text(json.dumps(doc, separators=(",", ":")) + "\n", encoding="utf-8")
    print(f"ci-test-results: {len(doc['passed'])} passed, {len(doc['failed'])} failed -> {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
