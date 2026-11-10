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

`featflags()` is the general query under it: any of the Makefile's configurations, dev (`release=False`,
which adds `devtools,devtriggers,threadcheck`) or release, for one `PLAYBACK_BACKENDS` value. A caller
that has to RECOGNISE a build (`ci/check-package.py` decoding `pkg/.build-config`) asks for both
configurations of every backend in `PACKAGED_BACKENDS` rather than spelling the flag strings.
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
# The `PLAYBACK_BACKENDS` values a television package can be built with: the Makefile's other value,
# `none`, is refused by the Makefile at parse time for any goal that builds, as is `ci/check-elf.sh`'s artifact mode.
# `ci/test_arm_profile.py` fails when the Makefile gains or loses a `PLAYBACK_FEATURES_<name>`.
PACKAGED_BACKENDS = ("both", "uri", "feed")


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


def featflags(root: Path = ROOT, *, release: bool, backends: "str | None" = None) -> list[str]:
    """The cargo feature flags the Makefile builds the ARM library with, as a token list.

    `release` is `RELEASE=1` (the shipping set: `--no-default-features` plus the backends) or its
    absence (the dev set, which adds `devtools`, `devtriggers` and `threadcheck`). `backends` is one
    `PLAYBACK_BACKENDS` value, or None to follow the environment exactly as the build does.

    `make -s print-playback-config` is a pure query (it resets no build stamp and needs no NDK).
    Every other input of `RUST_FEATFLAGS` is pinned on the command line, which outranks both the
    environment and an inherited `MAKEFLAGS`: a caller running inside `make ipk` (the Makefile runs
    `ci/check-package.py` itself) would otherwise ask about the build that is being graded instead of
    the configuration it means, a LAB build would derive a feature set that already carries
    `lab-diagnostics`, a `CURLSTACK=1` build one that carries `--features http3-curlstack` (which
    its stamp's feature field leaves out: `+curlstack` is a field of its own), and an inherited
    `RC=<n>` with `RELEASE=` empty trips the Makefile's own parse-time refusal. `MAKEFLAGS` (and its jobserver) is dropped for the same reason.

    Raises ValueError when make cannot answer, so a caller reports it instead of guessing a feature set."""
    query = ["RELEASE=1" if release else "RELEASE=", "LAB=", "RC=", "CURLSTACK="]
    if backends is not None:
        query.append(f"PLAYBACK_BACKENDS={backends}")
    cmd = ["make", "-s", "--no-print-directory", "-C", str(root), *query, "print-playback-config"]
    env = {k: v for k, v in os.environ.items() if k not in ("MAKEFLAGS", "MFLAGS", "MAKELEVEL")}
    proc = subprocess.run(cmd, capture_output=True, text=True, env=env)
    shown = "`" + " ".join(cmd[1:]) + "`"
    if proc.returncode != 0:
        raise ValueError(f"{shown} failed: " + proc.stderr.strip()[-400:])
    for line in proc.stdout.splitlines():
        if line.startswith("features="):
            return shlex.split(line[len("features="):])
    raise ValueError(f"{shown} printed no `features=` line")


def shipping_featflags(root: Path = ROOT) -> list[str]:
    """The cargo feature flags `make RELEASE=1` builds the shipping ARM library with, from the Makefile.

    Follows `PLAYBACK_BACKENDS` in the environment exactly as the build does (see `featflags`)."""
    return featflags(root, release=True)
