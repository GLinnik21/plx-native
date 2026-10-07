#!/bin/sh
# SessionStart hook: give a Claude Code CLOUD session the full git history `make check` needs.
#
# Cloud sessions start from a SHALLOW clone, and `tools/abr-calibrate-plant.py` (run by
# `make check-python-rest`) refuses one outright: it orders measurement captures by
# `git log --diff-filter=A`, which a shallow clone cannot answer. CI checks out with
# `fetch-depth: 0` for the same reason (.github/workflows/ci.yml, host-python). Without this,
# five of its tests error in every cloud session for a reason that has nothing to do with the code.
#
# Only in the cloud (`CLAUDE_CODE_REMOTE=true`, set by the harness there): a local checkout is
# never shallow by accident, and a SessionStart hook must not start a network fetch on a laptop.
# CONTRACT: exit 0 always. A failed fetch costs those tests, not the session; it says so on stderr.
[ "${CLAUDE_CODE_REMOTE:-}" = "true" ] || exit 0
cd "${CLAUDE_PROJECT_DIR:-.}" 2>/dev/null || exit 0
[ "$(git rev-parse --is-shallow-repository 2>/dev/null)" = "true" ] || exit 0
git fetch -q --unshallow origin \
  || echo "cloud-unshallow: git fetch --unshallow failed; make check's capture-chronology tests will error" >&2
exit 0
