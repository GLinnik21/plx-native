#!/usr/bin/env python3
"""Cases for the live-Watchlist-switch guard. `python3 .claude/hooks/watchlist-live-guard-test.py`.

Host-only: it imports the guard and asks it to classify command lines, and runs the hook once as a
process to check the exit-code contract. Half the cases are the false positives that matter: reading,
grepping, listing and removing the trigger, and commands that only mention it, must stay allowed.
Addresses are RFC 5737 documentation ranges.
"""
import importlib.util
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
HOOK = os.path.join(HERE, "watchlist-live-guard.py")
spec = importlib.util.spec_from_file_location("guard", HOOK)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)

T = "/tmp/com.beb.plxnative.debug/plxnative-watchlist-live"
BLOCK, ALLOW = True, False
CASES = [
    # --- must be blocked: every way of creating the file -------------------------
    (BLOCK, f"touch {T}"),
    (BLOCK, f"tools/tv-ssh ssh tv \"touch {T}\""),
    (BLOCK, f"ssh root@192.0.2.10 'touch {T}'"),
    (BLOCK, f": > {T}"),
    (BLOCK, f"echo 1 > {T}"),
    (BLOCK, f"printf '' >>{T}"),
    (BLOCK, f"echo 1 | tee {T}"),
    (BLOCK, f"cp /dev/null {T}"),
    (BLOCK, f"tools/tv-ssh ssh tv 'cd /tmp/com.beb.plxnative.debug && touch plxnative-watchlist-live'"),
    (BLOCK, f"python3 -c \"open('{T}', 'w').close()\""),
    (BLOCK, f"python3 -c \"import pathlib; pathlib.Path('{T}').touch()\""),
    (BLOCK, "tools/tv-session.sh up --screen watchlist --arm watchlist-live"),
    (BLOCK, "tools/tv-session.sh up --arm=watchlist-live"),
    (BLOCK, f"ls {T}; touch {T}"),
    # --- must be allowed: reads, removal, mentions ------------------------------
    (ALLOW, f"ls -la {T}"),
    (ALLOW, f"test -e {T} && echo on"),
    (ALLOW, f"tools/tv-ssh ssh tv \"rm -f {T}\""),
    (ALLOW, "grep -rn plxnative-watchlist-live rust-modules docs"),
    (ALLOW, "git log -S plxnative-watchlist-live --oneline"),
    (ALLOW, f"grep -c watchlist {T} > /tmp/count.txt"),
    (ALLOW, "touch /tmp/plxnative-discover"),
    (ALLOW, "tools/tv-session.sh up --screen watchlist --mock-plextv=8765"),
    (ALLOW, "git commit -m 'watchlist: the plxnative-watchlist-live switch'"),
    # --- the owner's prefix ------------------------------------------------------
    (ALLOW, f"PLX_WATCHLIST_LIVE_OWNER=1 tools/tv-ssh ssh tv \"touch {T}\""),
]


def run_hook(cmd):
    payload = json.dumps({"tool_name": "Bash", "tool_input": {"command": cmd}})
    return subprocess.run([sys.executable, HOOK], input=payload, capture_output=True, text=True).returncode


def main():
    fails = 0
    for want, cmd in CASES:
        got = guard.creates(cmd) is not None
        if got != want:
            fails += 1
            print(f"  FAIL  expected {'BLOCK' if want else 'ALLOW'}, got {'BLOCK' if got else 'ALLOW'}: {cmd}")
    # The process contract: 2 blocks, 0 allows, and a non-Bash or malformed payload allows.
    contract = [(run_hook(f"touch {T}"), 2), (run_hook(f"ls {T}"), 0)]
    other = subprocess.run([sys.executable, HOOK], input="not json", capture_output=True, text=True).returncode
    contract.append((other, 0))
    for got, want in contract:
        if got != want:
            fails += 1
            print(f"  FAIL  hook exit {got}, expected {want}")
    total = len(CASES) + len(contract)
    print(f"watchlist-live-guard: {total - fails}/{total} cases correct")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
