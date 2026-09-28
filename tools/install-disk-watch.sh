#!/bin/sh
# Install (or remove, with --uninstall) the per-user launchd agent that runs
# `tools/build-gc.sh --auto` hourly, so the pressure-driven reclaim does not depend on a Claude
# Code SessionEnd firing (`.claude/hooks/build-gc-auto.sh`) — a machine idle between sessions, or
# running some other tool entirely, still gets swept.
#
# MACOS ONLY. launchd is the platform's own scheduler and the one already running every other
# per-user background agent on this host; there is no reason to reimplement a poller. On any other
# OS this prints the cron line that does the same job and exits 0 — a non-zero exit here would
# make this the one setup step that fails a fresh Linux dev machine outright, for a feature that
# machine can still get by pasting one line into its own crontab.
#
# WHAT IT INSTALLS. `~/Library/LaunchAgents/com.plxnative.build-gc.plist`: `StartInterval 3600`
# (once an hour — `--auto`'s own lock and idle guard make a missed or doubled tick harmless, so
# there is no reason to poll faster), `RunAtLoad false` (do not fire the moment this plist loads;
# the first real run is the first scheduled tick, not login), and `LowPriorityIO` + `Nice` +
# `ProcessType Background` so an hourly `du` sweep across a fleet never contends with whatever the
# user is actually doing. It always launches the MAIN checkout's own `tools/build-gc.sh --auto` —
# resolved through `git rev-parse --git-common-dir`, the SAME lookup the SessionEnd hook uses, and
# for the same reason: running this installer FROM a lane must not bake that lane's own path into
# a plist that outlives it. `$0`'s own directory is the fallback, used only when git cannot answer
# (no repository, `git` missing) rather than a checkout this installer was run from at all.
#
# `launchctl bootstrap gui/$(id -u)` / `bootout`, not the deprecated `load`/`unload` — Apple's own
# guidance since 10.11, and the pair this script uses for BOTH install and uninstall makes the
# whole thing idempotent: bootout-then-bootstrap on install (so re-running after an edit picks up
# the new plist rather than leaving the old one loaded) and bootout-then-remove on uninstall.
#
# TIME MACHINE. If `tmutil destinationinfo` succeeds (a destination is configured — an unconfigured
# host prints nothing useful and would make this a false positive on every fresh machine), and
# `$PLX_FLEET_DIR` (default ~/plx-fleet — the external lane target dirs `tools/build-gc.sh`
# reclaims from) exists, exclude it with `tmutil addexclusion` (sticky, no `-p`, i.e. the exclusion
# itself is what persists, not a path-only entry Time Machine forgets across a rename) — backing up
# multi-gigabyte, fully rebuildable cargo output on an hourly cadence is exactly the kind of cost
# this whole feature exists to avoid, just paid to a backup volume instead of the boot disk.
#
# THIS SCRIPT NEVER RUNS ITSELF. Installing a hidden per-user background job is something the
# person running Claude Code must ask for, in their own words — an agent must never call this
# script on its own initiative (see AGENTS.md's Television and release safety section for the
# same rule applied to the TV: exceptional actions need the human to say so).
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
COMMON_DIR=$(cd "$ROOT" 2>/dev/null && git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || COMMON_DIR=""
if [ -n "$COMMON_DIR" ]; then MAIN=$(dirname "$COMMON_DIR"); else MAIN="$ROOT"; fi
LABEL=com.plxnative.build-gc
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
SCRIPT="$MAIN/tools/build-gc.sh"

UNINSTALL=
for a in "$@"; do
  case "$a" in
    --uninstall) UNINSTALL=1 ;;
    -h|--help)
      sed -n '2,/^set -eu/p' "$0" | grep '^#' | sed 's/^# \{0,1\}//'
      cat <<'USAGE'

usage: tools/install-disk-watch.sh [--uninstall]

  (no flag)    install the hourly launchd agent (macOS only; prints the cron equivalent and
               exits 0 on every other OS)
  --uninstall  bootout and remove it
USAGE
      exit 0 ;;
    *) echo "install-disk-watch: unknown argument $a (try --help)" >&2; exit 2 ;;
  esac
done

UID_N=$(id -u)
DOMAIN="gui/$UID_N"

cron_line() {
  echo "0 * * * * $SCRIPT --auto >/dev/null 2>&1"
}

if [ "$(uname -s 2>/dev/null)" != "Darwin" ]; then
  if [ -n "$UNINSTALL" ]; then
    echo "install-disk-watch: launchd is macOS-only; nothing to uninstall on $(uname -s 2>/dev/null || echo unknown)."
  else
    echo "install-disk-watch: launchd is macOS-only. Equivalent cron entry (crontab -e):"
    echo "  $(cron_line)"
  fi
  exit 0
fi

if [ -n "$UNINSTALL" ]; then
  launchctl bootout "$DOMAIN" "$PLIST" >/dev/null 2>&1 || true
  if [ -f "$PLIST" ]; then
    rm -f "$PLIST"
    echo "install-disk-watch: removed $PLIST"
  else
    echo "install-disk-watch: $LABEL was not installed"
  fi
  exit 0
fi

if [ ! -f "$SCRIPT" ]; then
  echo "install-disk-watch: $SCRIPT not found — run this from a checkout of the repository." >&2
  exit 1
fi

mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"

# Rewritten from scratch on every run rather than diffed: a plist this small is cheaper to
# regenerate than to patch, and regenerating means an edit to this script always takes effect on
# the next `make disk-watch`, not just on a machine that never had the old version installed.
cat > "$PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/sh</string>
    <string>$SCRIPT</string>
    <string>--auto</string>
  </array>
  <key>StartInterval</key>
  <integer>3600</integer>
  <key>RunAtLoad</key>
  <false/>
  <key>LowPriorityIO</key>
  <true/>
  <key>Nice</key>
  <integer>10</integer>
  <key>ProcessType</key>
  <string>Background</string>
  <key>StandardOutPath</key>
  <string>$HOME/Library/Logs/plxnative-build-gc-launchd.log</string>
  <key>StandardErrorPath</key>
  <string>$HOME/Library/Logs/plxnative-build-gc-launchd.log</string>
</dict>
</plist>
PLIST

# bootout-then-bootstrap: idempotent, and the only way an already-loaded job actually picks up an
# edited plist (`launchctl bootstrap` on a label that is already loaded fails rather than
# reloading it). A bootout of a job that was never loaded is a harmless no-op, hence `|| true`.
launchctl bootout "$DOMAIN" "$PLIST" >/dev/null 2>&1 || true
launchctl bootstrap "$DOMAIN" "$PLIST"
echo "install-disk-watch: installed $LABEL ($PLIST), runs tools/build-gc.sh --auto hourly"

FLEET_DIR=${PLX_FLEET_DIR:-$HOME/plx-fleet}
if command -v tmutil >/dev/null 2>&1 && tmutil destinationinfo >/dev/null 2>&1; then
  if [ -d "$FLEET_DIR" ]; then
    # Sticky, no `-p`, on $PLX_FLEET_DIR ITSELF, not on anything under it. A sticky exclusion is
    # an xattr on the item's own inode, which is exactly why it must land on the directory that
    # this script's `--lanes`/`--orphans` reclaim never removes: they delete each lane's target
    # dir INSIDE `$PLX_FLEET_DIR`, never the directory itself, so the inode carrying the xattr is
    # never recreated. `-p` (a path-based exclusion, tracked separately by Time Machine against
    # the path string rather than the inode) is the wrong tool here, not the safer one — it is
    # what you would want on a lane subdirectory that DOES get deleted and recreated by name, and
    # this script deliberately excludes the parent once instead of chasing every lane through it.
    if tmutil addexclusion "$FLEET_DIR" >/dev/null 2>&1; then
      echo "install-disk-watch: excluded $FLEET_DIR from Time Machine (it holds rebuildable fleet build trees)"
    fi
  fi
else
  echo "install-disk-watch: no Time Machine destination configured — skipped the exclusion"
fi
