"""The repository's ONE Rust toolchain pin, for the Python tools and tests that must name it.

The pin is `channel` in `rust-modules/rust-toolchain.toml`; the Makefile reads the same line into
`RUST_TOOLCHAIN`, and the `.github/actions/rust-toolchain` action installs it. A Python caller asks
here instead of restating a version, so moving the pin is a one-line change that reaches all of them.

    from rust_toolchain import pinned, codegen_flags

`pinned()` honours a `RUST_TOOLCHAIN` in the environment first, exactly as the Makefile's `?=` does
(the GPL source rebuild links its isolated toolchain as `source-rebuild` and passes it that way).
"""
from __future__ import annotations

import os
import re
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
