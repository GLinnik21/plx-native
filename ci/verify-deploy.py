#!/usr/bin/env python3
"""Compare the md5 of each locally-shipped file against the same file's md5 on the television.

Reads the REMOTE side from stdin — the exact text `md5sum <basenames...>` prints on the device's
busybox (`<hash>  <basename>` per line; a missing file instead produces a
`<basename>: No such file or directory` line from busybox's own stderr, which the caller merges
into this stream on purpose so a missing file is a REPORTED mismatch rather than a silently
absent line) — and takes the LOCAL side as file paths on argv, so the ssh round trip stays in the
Makefile beside the `$(SSH)`/`$(SCP)` helpers and this script never needs its own transport or
credentials.

This is the exact check that would have caught a stale `splash.png`: `pkg/splash.png` was in
`APP_FILES`, staged into every `.ipk`, and never scp'd by `deploy` — so a debug install kept
whatever launch image it was first installed with, forever, and nothing afterward asked whether
the device agreed with the source tree. `make deploy`'s last step now runs exactly this.
"""
from __future__ import annotations

import hashlib
import re
import sys
from pathlib import Path

# `<hash>  <name>`, busybox's plain form; a leading `*`/`./`  (GNU coreutils' binary-mode marker,
# or a mode where the name is written relative) is tolerated but not required, since which one a
# given `md5sum` build emits is not this script's business.
MD5_LINE = re.compile(r"^([0-9a-f]{32})\s+(?:[*]|\.[\\/])?(?P<name>.+)$")


def local_md5(path: Path) -> str:
    h = hashlib.md5()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def parse_remote(text: str) -> dict[str, str]:
    """basename -> md5, from busybox `md5sum`'s (stdout+stderr) text.

    Any line that is not `<hash>  <name>` is simply not added to the map — which is what turns a
    `No such file or directory` line into a MISSING verdict below, rather than a hash to compare.
    """
    out: dict[str, str] = {}
    for line in text.splitlines():
        m = MD5_LINE.match(line.strip())
        if m:
            out[m.group("name")] = m.group(1)
    return out


def check(local_paths: list[str], remote_text: str) -> list[str]:
    """Return one failure string per file that is missing locally, missing on the device, or
    whose hash disagrees. Empty list means the payload matches."""
    remote = parse_remote(remote_text)
    failures = []
    for arg in local_paths:
        path = Path(arg)
        name = path.name
        if not path.is_file():
            failures.append(f"{name}: not found locally at {arg}")
            continue
        got = remote.get(name)
        if got is None:
            failures.append(f"{name}: MISSING on the television")
            continue
        local = local_md5(path)
        if got != local:
            failures.append(f"{name}: MISMATCH — local {local} != device {got}")
    return failures


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        print("usage: verify-deploy.py <local-file>...  (remote md5sum text on stdin)", file=sys.stderr)
        return 2
    failures = check(argv[1:], sys.stdin.read())
    if failures:
        print("verify-deploy: the deployed payload does not match the source tree:", file=sys.stderr)
        for f in failures:
            print(f"  {f}", file=sys.stderr)
        return 1
    print(f"verify-deploy: {len(argv) - 1} files match")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
