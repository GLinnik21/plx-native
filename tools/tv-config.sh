#!/usr/bin/env bash
# tools/tv-config.sh -- the ONE place that knows where the dev television's address and its
# Wake-on-LAN MAC live. Every reader (the Makefile's TV, tools/tv-ssh, tools/tv-lock.sh,
# capture-screen.sh, tv-sched-trace.sh, stream-screen.py, tests/run.py, the wake-tv skill, ...)
# asks this instead of carrying its own copy of the lookup.
#
#   tools/tv-config.sh host          # the address (one line, no newline), or nothing
#   tools/tv-config.sh mac           # the Wake-on-LAN MAC, or nothing
#   tools/tv-config.sh dir           # the per-user directory below (whether or not it exists yet)
#   tools/tv-config.sh set-mac VALUE # persist a MAC in the per-user directory (dir 700, file 600)
#
# `host` and `mac` print nothing and still exit 0 when nothing is configured, so a `$(...)` under
# `set -e` needs no `|| true`; the caller decides what "no TV" means. The ENVIRONMENT is the
# caller's business, not this script's (tv-ssh honours PLX_TV_ADDR/TV/TV_HOST, wake-tv TV_HOST/TV/
# TV_MAC, ...): it answers only "what do the files say", in this order, first non-empty wins:
#
#   1. `.tv-host` / `.tv-mac` in THIS checkout (gitignored; the per-checkout override),
#   2. the same file in the MAIN checkout (a linked worktree has none of its own; setups that keep
#      the file only there keep working),
#   3. `tv-host` / `tv-mac` in the per-USER directory
#         ${PLX_TV_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/plxnative}
#      -- set it ONCE per machine and every checkout and worktree sees it. That is the reason it
#      exists: the address and MAC are properties of the television and of this person's network,
#      not of a checkout, and copying a file into each worktree goes stale the day the TV's DHCP
#      lease or the file changes in one place only. `PLX_TV_CONFIG_DIR` mainly lets a test point
#      the lookup at a temp dir.
#
# A value has all whitespace stripped, and an empty (or whitespace-only) file counts as absent, so
# it falls through to the next place. `PLX_TV_NO_HOST_FILE=1` switches every file off (the escape
# hatch ci/test_tv_ssh.py uses to assert the "no TV configured" path on a machine that has one).
# The user directory holds the same private values as `.tv-host`/`.tv-mac`: the outbound-guard hook
# and the source bundle treat `tv-host`/`tv-mac` as private names (`.claude/hooks/outbound-guard.py`,
# `ci/source_bundle.py`).
#
# Graded without a television by ci/test_tv_config.py (a throwaway repo + linked worktree).

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

user_dir() {
  if [ -n "${PLX_TV_CONFIG_DIR:-}" ]; then printf '%s' "$PLX_TV_CONFIG_DIR"
  elif [ -n "${XDG_CONFIG_HOME:-}" ]; then printf '%s/plxnative' "$XDG_CONFIG_HOME"
  elif [ -n "${HOME:-}" ]; then printf '%s/.config/plxnative' "$HOME"
  fi
}

strip() { tr -d ' \t\r\n'; }

# lookup NAME: NAME is `host` or `mac`; the repo files are `.tv-NAME`, the user file is `tv-NAME`.
lookup() {
  local name="$1" v dir common
  [ -z "${PLX_TV_NO_HOST_FILE:-}" ] || return 0
  v="$(cat "$REPO/.tv-$name" 2>/dev/null | strip)"
  if [ -z "$v" ]; then
    # `--git-common-dir` is the MAIN checkout's .git from anywhere in the worktree family; it is
    # relative to the checkout (plain `.git`) in the main one, absolute in a linked worktree.
    common="$(git -C "$REPO" rev-parse --git-common-dir 2>/dev/null)"
    case "$common" in /*) ;; ?*) common="$REPO/$common" ;; esac
    [ -z "$common" ] || v="$(cat "$common/../.tv-$name" 2>/dev/null | strip)"
  fi
  if [ -z "$v" ]; then
    dir="$(user_dir)"
    [ -z "$dir" ] || v="$(cat "$dir/tv-$name" 2>/dev/null | strip)"
  fi
  printf '%s' "$v"
}

case "${1:-}" in
  host|mac) lookup "$1" ;;
  dir) user_dir ;;
  set-mac)
    v="$(printf '%s' "${2:-}" | strip)"
    dir="$(user_dir)"
    [ -n "$v" ] || { echo "tv-config: set-mac needs a MAC address" >&2; exit 2; }
    [ -n "$dir" ] || { echo "tv-config: no per-user config directory (HOME is unset)" >&2; exit 1; }
    (umask 077; mkdir -p "$dir" && printf '%s\n' "$v" > "$dir/tv-mac") || exit 1
    chmod 600 "$dir/tv-mac" 2>/dev/null || true
    ;;
  *) echo "usage: tv-config.sh host | mac | dir | set-mac VALUE" >&2; exit 2 ;;
esac
