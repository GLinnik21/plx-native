#!/usr/bin/env python3
"""The PLATFORM build axis: which operating system a build targets.

`PLATFORM` in the Makefile is one of the values below, and the Makefile reads them from `--list`
the way it reads FLAVOR's from `ci/flavor.py --list`, so a mistyped `make PLATFORM=webso ...` is
refused at parse time instead of building something nobody asked for:

  webos    the LG webOS television (armv7, the `.ipk`): what every target built before the axis
           existed, and the default.
  desktop  the macOS and Windows/WSLg simulators and the macOS app (`sim-*`, `macapp`): the Makefile
           makes it the default for those goals.
  tizen    the Samsung Tizen television (the `.tpk`). Only the name exists yet; no goal builds it.

The axis is the TARGET OS and nothing else. It does not choose the compatibility axis: the Rust
target (`RUST_TARGET`), the sysroot (`SYSROOT`), a managed TFM, a firmware floor and the signing
path stay separate variables, each recorded once per tested configuration, so adding a platform never
changes what another one links. FLAVOR stays the install identity (`debug`, `nightly`, `stable`) and
is orthogonal to it.

THIS FILE IS NOT NAMED `platform.py`. Scripts in `ci/` (`mkmacapp.py`, `build-libass.py`,
`test-compat.py`) `import platform` for `system()` and `machine()`, and Python puts a script's own
directory first on `sys.path`, so a `ci/platform.py` would replace the standard library module for
every one of them. `ci/test_platform_axis.py` runs them from this directory to keep it that way.

It imports nothing but `sys`: the Makefile spawns it on every `make` invocation, as it does
`ci/flavor.py`, and each import adds to that bill.
"""
import sys

# Order is the order the Makefile's error message lists them in. The default is the Makefile's
# `PLATFORM ?= webos`, not a property of this list.
PLATFORMS = ("webos", "desktop", "tizen")

if __name__ == "__main__":
    if sys.argv[1:] == ["--list"]:
        print(" ".join(PLATFORMS))
        sys.exit(0)
    print(__doc__)
    sys.exit(2)
