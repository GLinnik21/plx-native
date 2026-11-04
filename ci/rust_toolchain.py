"""The repository's ONE Rust toolchain pin, for the Python tools and tests that must name it, and the
ONE answer to "which cargo features does the shipping ARM build pass".

The pin is `channel` in `rust-modules/rust-toolchain.toml`; the Makefile reads the same line into
`RUST_TOOLCHAIN`, and the `.github/actions/rust-toolchain` action installs it. A Python caller asks
here instead of restating a version, so moving the pin is a one-line change that reaches all of them.

    from rust_toolchain import pinned, shipping_featflags

`pinned()` honours a `RUST_TOOLCHAIN` in the environment first, exactly as the Makefile's `?=` does
(the GPL source rebuild links its isolated toolchain as `source-rebuild` and passes it that way).

`shipping_featflags()` is the same idea for the feature set: the Makefile's `RUST_FEATFLAGS` under
`RELEASE=1` (`--no-default-features` plus the playback backends, `PLAYBACK_BACKENDS`, as explicit
`--features`), asked of `make -s print-playback-config` rather than restated. A caller that wrote a bare
`--no-default-features` once described a build with no playback backend at all (neither backend crate
nor `dolby_vision` and the rest of play-feed's third-party tree, all of which the release links).
"""
from __future__ import annotations

import os
import re
import shlex
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TOOLCHAIN_FILE = ROOT / "rust-modules" / "rust-toolchain.toml"
TARGET = "armv7-unknown-linux-gnueabi"


def pinned_channel(root: Path = ROOT) -> str:
    """The `channel` of the checkout at `root`, ignoring the environment."""
    text = (Path(root) / "rust-modules" / "rust-toolchain.toml").read_text()
    match = re.search(r'^channel\s*=\s*"([^"]+)"', text, re.M)
    if not match:
        raise ValueError("rust-modules/rust-toolchain.toml has no `channel`")
    return match.group(1)


def pinned(root: Path = ROOT) -> str:
    """The toolchain a build should use: $RUST_TOOLCHAIN if set, else the file's `channel`."""
    return os.environ.get("RUST_TOOLCHAIN", "").strip() or pinned_channel(root)


def shipping_featflags(root: Path = ROOT) -> list[str]:
    """The cargo feature flags `make RELEASE=1` builds the shipping ARM library with, from the Makefile.

    `make -s print-playback-config` is a pure query (it resets no build stamp and needs no NDK); it
    follows `PLAYBACK_BACKENDS` in the environment exactly as the build does. Raises ValueError when
    make cannot answer, so a caller reports it instead of guessing a feature set."""
    proc = subprocess.run(["make", "-s", "-C", str(root), "RELEASE=1", "print-playback-config"],
                          capture_output=True, text=True)
    if proc.returncode != 0:
        raise ValueError("`make -s RELEASE=1 print-playback-config` failed: " + proc.stderr.strip()[-400:])
    for line in proc.stdout.splitlines():
        if line.startswith("features="):
            return shlex.split(line[len("features="):])
    raise ValueError("`make -s RELEASE=1 print-playback-config` printed no `features=` line")
