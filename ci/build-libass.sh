#!/bin/sh
# HOST=1 builds the same pinned stack for the desktop simulator.
set -eu
exec python3 "$(dirname "$0")/build-libass.py" "$@"
