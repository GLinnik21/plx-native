#!/usr/bin/env bash
#
# wake-tv.sh — wake the LG dev TV (49SM9000PLA) from standby via Wake-on-LAN and
# wait until SSH answers; or put it INTO standby for testing the cycle.
#
#   wake-tv.sh            wake + wait (fast no-op if already up).  Exit 0 = ssh up.
#   wake-tv.sh standby    ask webOS to power off to standby (clean, resumable by WoL).
#   wake-tv.sh status     one reachability probe, prints UP/DOWN.  Exit 0 = up.
#
# Config resolution (nothing about your network is baked into this file):
#   TV_HOST — $TV_HOST, else $TV, else the Makefile's TV default.
#   TV_MAC  — $TV_MAC, else the gitignored .tv-mac cache, else looked up from the ARP
#             table while the TV is reachable and cached there for next time.
#   TV_USER (root)  WAKE_TIMEOUT (180 s)
#
# Notes from live use (see SKILL.md Gotchas):
#  - The TV auto-drops to standby after a few idle minutes; every automation session
#    starts here. Wake typically takes 15-60 s, occasionally ~2-3 min — hence the
#    generous default timeout and the WoL resend every ~20 s while polling.
#  - macOS has no `wakeonlan` out of the box; python3 broadcasts the magic packet.
#  - SSH auth: tools/tv-ssh tries the installed key first and falls back to sshpass only when
#    the set refuses the key, so a machine without one still works.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
MAC_CACHE="$REPO/.tv-mac"

# Host: explicit env wins, else the gitignored .tv-host — the same file the Makefile's TV and
# tools/' TV_HOST fall back to, so there is one place to change and none of it is in the repo.
# NB this used to scrape `TV = …` out of `make -pn`, which broke the moment TV stopped being a
# literal: `make -p` prints a recursive variable's UNEXPANDED DEFINITION, so the scrape returned
# the `$(shell cat .tv-host)` text and the wake failed with a DNS error that read like the TV was
# gone. The last-resort ask is now `make -s print-tv`, a real echo recipe that prints the expanded
# value and cannot regress that way (and is exempt from the Makefile's parse-time configuration
# stamp, so asking cannot delete a build).
TV_HOST="${TV_HOST:-${TV:-}}"
[ -n "$TV_HOST" ] || TV_HOST="$(cat "$REPO/.tv-host" 2>/dev/null || true)"
[ -n "$TV_HOST" ] || TV_HOST="$(make -s -C "$REPO" print-tv 2>/dev/null | head -1)"
[ -n "$TV_HOST" ] || { echo "ERROR: no TV host — put its IP in .tv-host, or set TV_HOST=<ip>." >&2; exit 2; }

TV_USER="${TV_USER:-root}"
WAKE_TIMEOUT="${WAKE_TIMEOUT:-180}"

# MAC (needed only for the magic packet): env -> cache -> ARP while the TV is up.
TV_MAC="${TV_MAC:-}"
[ -n "$TV_MAC" ] && [ -f "$MAC_CACHE" ] || true
[ -n "$TV_MAC" ] || TV_MAC="$(cat "$MAC_CACHE" 2>/dev/null || true)"
learn_mac() {
  # normalize the ARP form (a:b:c:1:2:3) to zero-padded octets
  local m
  m="$(arp -n "$TV_HOST" 2>/dev/null | grep -oE '([0-9a-f]{1,2}:){5}[0-9a-f]{1,2}' | head -1)" || true
  [ -n "$m" ] || return 1
  m="$(python3 -c "import sys;print(':'.join(f'{int(x,16):02x}' for x in sys.argv[1].split(':')))" "$m")"
  printf '%s' "$m" > "$MAC_CACHE"
  TV_MAC="$m"
}

# Through tools/tv-ssh: the key first, `sshpass` only if the set refuses it, a fast failure while
# the set is still asleep (which is the answer `up` wants), and no address on any line it prints.
TVSSH="$REPO/tools/tv-ssh"
export PLX_TV_ADDR="$TV_HOST" PLX_TV_SSH_TIMEOUT=5
up() { "$TVSSH" ssh "${TV_USER}@${TV_HOST}" true 2>/dev/null; }

send_wol() {
  python3 - "$TV_MAC" "$TV_HOST" <<'PY'
import socket, sys
mac = bytes(int(x, 16) for x in sys.argv[1].split(":"))
pkt = b"\xff" * 6 + mac * 16
# subnet broadcast (derived from the TV's /24) + global broadcast, port 9
subnet = ".".join(sys.argv[2].split(".")[:3]) + ".255"
for bcast in (subnet, "255.255.255.255"):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    s.sendto(pkt, (bcast, 9))
    s.close()
PY
}

case "${1:-wake}" in
  status)
    if up; then
      [ -n "$TV_MAC" ] || learn_mac || true
      echo "TV: UP"
    else echo "TV: DOWN"; exit 1; fi
    ;;
  standby)
    # Clean standby via the webOS power service. On THIS webOS 4.5 build the method is
    # power/powerOff (power/turnOff -> "Unknown method"). luna-send silently no-ops
    # without a controlling TTY, so it runs under `script -qc` ON the TV.
    up || { echo "TV is already down."; exit 0; }
    # ServerAlive: powerOff drops the link while the ssh session is still open — without
    # keepalives the session hangs on TCP for minutes. With them it errors out in ~6s,
    # which is fine (the call already fired); the poll below is the real confirmation.
    "$TVSSH" ssh -o ServerAliveInterval=3 -o ServerAliveCountMax=2 "${TV_USER}@${TV_HOST}" \
      'script -qc "luna-send -n 1 luna://com.webos.service.tvpower/power/powerOff '\''{\"reason\":\"remoteKey\"}'\''" /dev/null' \
      >/dev/null 2>&1 || true
    # confirm it actually dropped
    for _ in $(seq 1 10); do up || { echo "TV: standby."; exit 0; }; sleep 2; done
    echo "ERROR: TV still answers after turnOff." >&2; exit 1
    ;;
  wake)
    if up; then
      [ -n "$TV_MAC" ] || learn_mac || true   # cache it now, while we still can
      echo "TV: already up."; exit 0
    fi
    if [ -z "$TV_MAC" ]; then
      echo "ERROR: no MAC for the magic packet, and the TV is unreachable so it cannot be" >&2
      echo "       learned from ARP. Set TV_MAC=<aa:bb:cc:dd:ee:ff> once (it is then cached" >&2
      echo "       in .tv-mac), or wake the TV by hand and re-run to cache it." >&2
      exit 2
    fi
    echo "Waking the TV..."
    t0=$(date +%s)
    send_wol
    while :; do
      if up; then
        echo "TV: UP after $(( $(date +%s) - t0 ))s."
        exit 0
      fi
      elapsed=$(( $(date +%s) - t0 ))
      if [ "$elapsed" -ge "$WAKE_TIMEOUT" ]; then
        echo "ERROR: TV did not answer within ${WAKE_TIMEOUT}s. Is it on mains power?" >&2
        exit 1
      fi
      # resend the magic packet every ~20s — a single packet is occasionally missed
      [ $(( elapsed % 20 )) -lt 3 ] && send_wol
      sleep 3
    done
    ;;
  *)
    echo "usage: wake-tv.sh [wake|standby|status]" >&2; exit 2
    ;;
esac
