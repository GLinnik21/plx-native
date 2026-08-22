#!/usr/bin/env python3
"""keytable.py — the key ladder's behaviour, as a table you can diff.

`app.rs`'s key handler is ~676 lines and 19 arms, and no host test executes any of it: it lives
inside the SDL event loop. Its correctness is also ORDER-dependent by design — an earlier guard
subsumes a later one (`Route::Player{..} && transport_hidden()` swallows every key but BACK, so
the four overlay arms below it are unreachable while it holds). Reordering compiles, keeps the
suite green, and silently changes behaviour.

So before that ladder is touched, its behaviour has to be written down. This drives the SIMULATOR
through (screen x key) and records, for every press, the focus fingerprint before and after — i.e.
what the key actually did. The result is a golden table. Refactor, re-run, diff: an empty diff is
the evidence the refactor did not change behaviour, and a non-empty one names the arm that moved.

    tools/keytable.py --out tests/keytable.json          # record
    tools/keytable.py --check tests/keytable.json        # re-record and diff against it

Requires `make sim` to have been built. Never touches the television. Each screen gets its own
instance root, so several of these can run at once (that is the whole point of `SIM_DIR`).

Two triggers are armed in every root:
  * `plxnative-focus`  — the fingerprint itself (`crate::focusprobe`)
  * `plxnative-noidle` — the frame gate off. A settled screen stops presenting, and while the
    fingerprint is logged from the frame loop rather than from a present, a screen that never
    repaints also never redraws what a key changed. Arming it keeps the run honest and costs
    nothing here; on the device it would cost the idle measurement, which is why it is DIAG-exempt.
"""
import argparse
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SIM = os.path.join(REPO, "rust-modules", "target-sim", "debug", "plxnative-sim")

# The keys the ladder dispatches on. `okdown`/`okup` are the split halves — the only way to drive a
# press-and-hold past `press::LONG_MS`, which is a different arm from a tap and has to be recorded
# as one.
#
# `back` is NOT here, and the comment that used to explain why said it "is last in every script
# because at a screen's root it EXITS the app" — which described neither the list (there was no
# `back` in it to be last) nor, since 2026-08-21, the behaviour: a root BACK raises the exit alert
# (`ui::exit_alert`) instead of quitting. Adding it is now a real and worthwhile option — `home` is
# in SCREENS, and the ladder's whole BACK cascade is currently uncharacterised — but it costs a
# re-record of `tests/keytable.json`, and a script that ends on the alert has to answer it (`back`
# again, or arm `/tmp/plxnative-noexitconfirm`) before the next screen's run.
KEYS = ["up", "down", "left", "right", "ok", "play", "pause", "stop"]

# screen name -> (trigger file, trigger content or None)
SCREENS = {
    "home": (None, None),
    "library": ("plxnative-library", ""),
    "search": ("plxnative-search", "the"),
}

FOCUS_RE = re.compile(r"^focus .*$", re.M)


def pms_from_config():
    """Host and port out of the gitignored src/config.local.h, the same source `make sim` reads."""
    p = os.path.join(REPO, "src", "config.local.h")
    host, port = None, "32400"
    if os.path.exists(p):
        for line in open(p):
            m = re.match(r'\s*#define\s+PMS_HOST\s+"([^"]+)"', line)
            if m:
                host = m.group(1)
            m = re.match(r"\s*#define\s+PMS_PORT\s+(\d+)", line)
            if m:
                port = m.group(1)
    return host, port


def boot(root, screen, host, port, settle):
    """Start one simulator in its own instance root, armed, and wait for its first fingerprint."""
    shutil.rmtree(root, ignore_errors=True)
    os.makedirs(root, exist_ok=True)
    # the injected token: the simulator cannot sign in (libcurl does not bind on the host)
    tok = os.path.join(REPO, "src", "config.local.h")
    if os.path.exists(tok):
        for line in open(tok):
            m = re.match(r'\s*#define\s+PMS_TOKEN\s+"([^"]+)"', line)
            if m:
                open(os.path.join(root, "plxnative-token"), "w").write(m.group(1))
    open(os.path.join(root, "plxnative-focus"), "w").close()
    open(os.path.join(root, "plxnative-noidle"), "w").close()
    trig, content = SCREENS[screen]
    if trig:
        open(os.path.join(root, trig), "w").write(content or "")
    env = dict(os.environ, PLXNATIVE_RUNTIME_DIR=root, PLXNATIVE_APP_DIR=os.path.join(REPO, "pkg"))
    proc = subprocess.Popen([SIM, host, port], env=env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    log = os.path.join(root, "plxnative-events.log")
    for _ in range(int(settle * 10)):
        time.sleep(0.1)
        if os.path.exists(log) and FOCUS_RE.search(open(log, errors="replace").read()):
            time.sleep(settle / 2)  # let posters and hub data land before the first sample
            return proc, log
    proc.terminate()
    raise SystemExit(f"{screen}: no `focus` line after {settle}s — is the probe armed and the PMS up?")


def last_focus(log):
    ms = FOCUS_RE.findall(open(log, errors="replace").read())
    return ms[-1] if ms else None


def run_screen(screen, host, port, settle, pause):
    """Drive every key from one boot and return the transition list."""
    root = f"/tmp/keytable-{screen}"
    proc, log = boot(root, screen, host, port, settle)
    fifo = os.path.join(root, "plxnative-remote")
    rows = []
    try:
        # `<>` read-write: a write-only open on a FIFO with no reader blocks forever in open(2),
        # which looks exactly like a hung harness (`ui-sim`'s trap 1).
        fd = os.open(fifo, os.O_RDWR | os.O_NONBLOCK)
        for key in KEYS:
            before = last_focus(log)
            os.write(fd, (key + " ").encode())
            time.sleep(pause)
            after = last_focus(log)
            rows.append({
                "key": key,
                # the route the key was actually delivered to. A script drifts — an OK on Home opens
                # a detail page, and every key after it is a DIFFERENT screen's arm. That is a
                # faithful session trace rather than a defect, but the drift has to be READABLE or
                # the table's screen label lies about three quarters of its rows.
                "route": (before or "route=?").split()[1].split("=", 1)[1] if before else "?",
                "before": before,
                "after": after,
                # the honest verdict: a key that moved nothing is a RECORDED fact, not a gap —
                # most of the ladder's arms are no-ops on most screens and that is the behaviour
                "moved": before != after,
            })
        os.close(fd)
    finally:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
    return rows


def record(args):
    host, port = pms_from_config()
    if args.pms:
        host = args.pms
    if not host:
        raise SystemExit("no PMS host: pass --pms <numeric ip> (stream.rs has no DNS resolver)")
    table = {}
    for screen in args.screens:
        print(f"  {screen} …", flush=True)
        table[screen] = run_screen(screen, host, port, args.settle, args.pause)
    return table


def summarise(table):
    for screen, rows in table.items():
        moved = sum(1 for r in rows if r["moved"])
        print(f"\n{screen}: {moved}/{len(rows)} keys changed the fingerprint")
        for r in rows:
            if not r["moved"]:
                print(f"    [{r['route']:<8}] {r['key']:<6} —")
                continue
            b = dict(f.split("=", 1) for f in (r["before"] or "").split()[1:] if "=" in f)
            a = dict(f.split("=", 1) for f in (r["after"] or "").split()[1:] if "=" in f)
            diff = [f"{k}:{b.get(k,'-')}→{a[k]}" for k in a if b.get(k) != a[k]]
            print(f"    [{r['route']:<8}] {r['key']:<6} {' '.join(diff)}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", help="write the table here")
    ap.add_argument("--check", help="re-record and diff against this table; non-zero exit on any change")
    ap.add_argument("--screens", nargs="+", default=list(SCREENS), choices=list(SCREENS))
    ap.add_argument("--pms", help="PMS host (numeric IP); defaults to src/config.local.h")
    ap.add_argument("--settle", type=float, default=8.0, help="seconds to let a boot land")
    ap.add_argument("--pause", type=float, default=0.7, help="seconds between a key and its sample")
    args = ap.parse_args()

    if not os.path.exists(SIM):
        raise SystemExit(f"no simulator at {SIM} — run `make sim` first")

    table = record(args)
    summarise(table)

    if args.out:
        json.dump(table, open(args.out, "w"), indent=1, sort_keys=True)
        print(f"\nwrote {args.out}")
    if args.check:
        golden = json.load(open(args.check))
        bad = 0
        for screen, rows in table.items():
            old = {r["key"]: r for r in golden.get(screen, [])}
            for r in rows:
                o = old.get(r["key"])
                if o is None:
                    print(f"NEW  {screen}/{r['key']}")
                    bad += 1
                elif o["after"] != r["after"]:
                    print(f"DIFF {screen}/{r['key']}\n  was {o['after']}\n  now {r['after']}")
                    bad += 1
        print(f"\n{'CHANGED: ' + str(bad) if bad else 'unchanged'}")
        sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
