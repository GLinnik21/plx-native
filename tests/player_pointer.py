#!/usr/bin/env python3
"""Offline grader for docs/testing-player-pointer.md; never connects to a device.

The protocol's explicit FIFO edge markers bracket commits, so a missing release or a seek
while held fails even when the film continues playing. Captures prove HUD presentation;
this grader proves input/seek ordering, not pixels or FPS.
"""
import argparse
from pathlib import Path
import re
import unittest


MARKERS = (
    "remote: pointer move 1400,870",
    "remote: click 1400,870",
    "remote: pointer move 1400,890",
    "remote: click 1400,890",
    "remote: pointer down 900,870",
    "remote: pointer move 1200,700",
    "remote: pointer move 700,700",
    "remote: pointer up 700,700",
    "remote: pointer up 700,700",  # deliberately repeated release
    "remote: pointer move 701,700",  # end of the no-duplicate observation window
)
COMMIT = re.compile(r"scrub: pointer commit ns=(\d+)")
NATIVE_SEEK = re.compile(r"seek\(in-place\): av_seek t=(\d+)")


def grade(lines):
    """Return failures for one complete protocol run; empty means the log contract passed."""
    indices = []
    cursor = 0
    for marker in MARKERS:
        found = next((i for i in range(cursor, len(lines)) if marker in lines[i]), None)
        if found is None:
            return [f"missing ordered edge: {marker}"]
        indices.append(found)
        cursor = found + 1
    failures = []
    phases = (
        ("HUD reveal", 0, 1, 0),
        ("click at 870", 1, 2, 1),
        ("second HUD reveal", 2, 3, 0),
        ("click at 890", 3, 4, 1),
        ("held drag", 4, 7, 0),
        ("release outside band", 7, 8, 1),
        ("duplicate release", 8, 9, 0),
    )
    committed = []
    for name, begin, end, expected in phases:
        window = lines[indices[begin]:indices[end]]
        commits = [int(m[1]) for line in window if (m := COMMIT.search(line))]
        seeks = [int(m[1]) for line in window if (m := NATIVE_SEEK.search(line))]
        if len(commits) != expected:
            failures.append(f"{name}: {len(commits)} pointer commits; expected {expected}")
        if len(seeks) != expected:
            failures.append(f"{name}: {len(seeks)} native seeks; expected {expected}")
        if commits != seeks:
            failures.append(f"{name}: requested {commits}, native seeks {seeks}")
        committed.extend(commits)
    if len(committed) == 3 and not (committed[0] == committed[1] > committed[2] > 0):
        failures.append("equal-x clicks must agree and the leftward drag must seek backward")
    if any(COMMIT.search(line) or NATIVE_SEEK.search(line) for line in lines[indices[-1]:]):
        failures.append("seek repeated after the final observation marker")
    return failures


class GraderTests(unittest.TestCase):
    @staticmethod
    def good():
        lines = []
        for i, marker in enumerate(MARKERS):
            lines.append(marker)
            if i in (1, 3, 7):
                ns = 75_000_000_000 if i != 7 else 35_000_000_000
                lines += [f"scrub: pointer commit ns={ns}", f"seek(in-place): av_seek t={ns} coalesced=0"]
        return lines

    def test_complete_protocol(self):
        self.assertEqual(grade(self.good()), [])

    def test_missing_primitive_does_not_pass_as_no_premature_seek(self):
        self.assertTrue(grade(["remote: unknown token 'pd:900,870'"]))

    def test_seek_while_held_is_rejected(self):
        lines = self.good()
        at = lines.index(MARKERS[5])
        lines[at:at] = ["scrub: pointer commit ns=500", "seek(in-place): av_seek t=500 coalesced=0"]
        self.assertTrue(any("held drag" in why for why in grade(lines)))

    def test_duplicate_and_wrong_native_target_are_rejected(self):
        lines = self.good()
        lines.append("scrub: pointer commit ns=35000000000")
        self.assertTrue(grade(lines))
        lines = self.good()
        lines[3] = "seek(in-place): av_seek t=100 coalesced=0"
        self.assertTrue(grade(lines))

    def test_no_seek_after_release_is_rejected(self):
        self.assertTrue(grade([line for line in self.good() if not COMMIT.search(line)]))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("log", nargs="?", type=Path)
    parser.add_argument("--selftest", action="store_true")
    args = parser.parse_args()
    if args.selftest:
        unittest.main(argv=[__file__])
    elif args.log:
        failures = grade(args.log.read_text().splitlines())
        for failure in failures:
            print(f"FAIL: {failure}")
        if not failures:
            print("PASS: both clicks, held drag, release outside band, no premature/duplicate seek")
        raise SystemExit(bool(failures))
    else:
        parser.error("supply a saved log or --selftest")
