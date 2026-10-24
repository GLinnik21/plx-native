#!/usr/bin/env python3
"""Take the Watchlist ui-sim captures named by tests/scenarios/watchlist_captures.json.

The ONE capture recipe (the scenario's `comment` repeats it):

  1. start tests/mock_pms.py in this process (loopback, `authorize_after=2`) and reset its Discover
     list to the capture's `mock` mode with `POST /_mock/discover {"list": <mode>}`;
  2. a fresh instance root with `plxnative-plextv` and `plxnative-discover` aimed at the mock;
     a prep boot with `plxnative-login` signs in through the app's own QR flow (the stored mock
     session is the account; no `token` trigger is armed);
  3. arm the capture's `triggers`, boot, wait `boot_wait_s`, send each key through
     `plxnative-remote` (`hold` is okdown, 1.2 s, okup), then `shot`;
     A key `mock:<json>` POSTs <json> to /_mock/discover mid-walk (`{"pms":"fail"}` makes the PMS side
     answer 503: once a library read fails and the list is read again, a title matched there reads
     ServerOffline); `lib:<json>` POSTs <json> to /_mock/library (a TEST-ONLY library add/remove plus the
     `timeline` frame the feed carries for it: W-notify); `sleep:<s>` waits <s> seconds; any other key, `ck:<x>,<y>` (a click at logical
     1920x1080 pixels) included, goes to the FIFO as it is;
     A capture with `unreachable` aims the plex.tv and Discover triggers at a closed loopback port
     AFTER the sign-in, so the stored mock session is the account but the service cannot answer
     (the "plex.tv unreachable" state);
  4. move the instance root's shot-N.png to `<out>/<file>`. A capture with `expect_write` fails
     unless the mock logged that write, and one with `expect_log` unless the app's event log holds
     that text.

Nothing leaves the machine: no plex.tv, no real server, no television. The PNGs are scratch
output and are never committed.

  python3 tools/watchlist_captures.py --sim <path to plxnative-sim> [--out DIR] [--only FILE ...]
"""
import argparse
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
SCENARIO = ROOT / "tests" / "scenarios" / "watchlist_captures.json"  # --scenario overrides
SIGN_IN_DONE = "hubs: landed"  # the event-log line after which the sign-in's session is on disk


class Failure(Exception):
    pass


def sim_env(runtime):
    """This machine's environment minus every PLXNATIVE_* variable, plus the instance root."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("PLXNATIVE_")}
    env.update({"PLXNATIVE_RUNTIME_DIR": str(runtime), "PLXNATIVE_APP_DIR": str(ROOT / "pkg"),
                "PLXNATIVE_WIN": "1920x1080"})
    return env


def post(port, path, obj):
    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", data=json.dumps(obj).encode(), method="POST")
    with urllib.request.urlopen(req, timeout=5) as r:
        return json.load(r)


def require_stand_in(events):
    """Fail the capture unless the boot's own event log says Discover is the mock's stand-in."""
    sys.path.insert(0, str(ROOT / "tests"))
    import mock_pms
    problem = mock_pms.discover_stand_in_problem(events.read_text(errors="replace") if events.exists() else "")
    if problem:
        raise Failure(problem)


def sign_in(sim, runtime, port, timeout=90):
    (runtime / "plxnative-login").write_text("")
    log = open(runtime / "prep.out", "w")
    proc = subprocess.Popen([str(sim), "127.0.0.1", str(port)], stdout=log, stderr=subprocess.STDOUT,
                            env=sim_env(runtime))
    events = runtime / "plxnative-events.log"
    try:
        t0 = time.monotonic()
        while True:
            if proc.poll() is not None:
                raise Failure(f"the sign-in boot exited {proc.returncode} (see {runtime / 'prep.out'})")
            if events.exists() and SIGN_IN_DONE in events.read_text(errors="replace"):
                break
            if time.monotonic() - t0 > timeout:
                raise Failure(f"the mock sign-in did not finish within {timeout}s")
            time.sleep(0.5)
        time.sleep(1.5)  # the session's last revision is written a moment after the hubs land
        require_stand_in(events)
    finally:
        proc.kill()
        proc.wait()
        log.close()
    if not (runtime / "state" / "session.json").is_file():
        raise Failure("the mock sign-in left no state/session.json")
    for name in ("plxnative-login", "plxnative-events.log", "plxnative-diag.log", "plxnative-remote"):
        (runtime / name).unlink(missing_ok=True)


def capture(sim, spec, scenario, out_dir):
    sys.path.insert(0, str(ROOT / "tests"))
    import mock_pms

    srv, pms = mock_pms.serve(0, authorize_after=2)
    port = srv.server_address[1]
    runtime = pathlib.Path(tempfile.mkdtemp(prefix="plxnative-wl-"))
    proc = None
    try:
        post(port, "/_mock/discover", dict({"list": spec["mock"]}, **spec.get("mock_extra", {})))
        for name in ("plextv", "discover"):
            (runtime / f"plxnative-{name}").write_text(f"http://127.0.0.1:{port}")
        sign_in(sim, runtime, port)
        pms.writes.clear()
        if spec.get("unreachable"):
            for name in ("plextv", "discover"):
                (runtime / f"plxnative-{name}").write_text("http://127.0.0.1:1")
        for name, value in spec["triggers"].items():
            (runtime / f"plxnative-{name}").write_text(value)
        log = open(runtime / "sim.out", "w")
        proc = subprocess.Popen([str(sim), "127.0.0.1", str(port)], stdout=log, stderr=subprocess.STDOUT,
                                env=sim_env(runtime))
        time.sleep(scenario["boot_wait_s"])
        require_stand_in(runtime / "plxnative-events.log")  # before a single key is sent
        fifo = runtime / "plxnative-remote"
        for _ in range(50):
            if fifo.exists():
                break
            time.sleep(0.2)
        else:
            raise Failure("the simulator never opened plxnative-remote")
        fd = os.open(fifo, os.O_RDWR)  # read-write: a write-only open blocks if the app is gone
        try:
            def send(tok):
                os.write(fd, (tok + " ").encode())

            for key in spec["keys"]:
                if key == "hold":
                    send("okdown")
                    time.sleep(1.2)
                    send("okup")
                elif key.startswith("mock:"):
                    # `mock:{"pms":"fail"}`: POST that body to /_mock/discover mid-walk (the PMS goes
                    # away after the Watchlist matched a title on it: the server-offline state).
                    post(port, "/_mock/discover", json.loads(key[5:]))
                    continue
                elif key.startswith("lib:"):
                    # `lib:{"op":"add","title":"Northbound Tide"}`: POST that body to /_mock/library, a
                    # TEST-ONLY library change plus the `timeline` frame the feed would carry for it
                    # (the W-notify scenario: the tile flips without leaving the tab).
                    post(port, "/_mock/library", json.loads(key[4:]))
                    continue
                elif key.startswith("sleep:"):
                    time.sleep(float(key[6:]))
                    continue
                else:
                    # `key@0.3` waits 0.3 s instead of key_wait_s before the next key (or the shot):
                    # the way to catch a spring mid-flight.
                    key, _, wait = key.partition("@")
                    send(key)
                    time.sleep(float(wait) if wait else scenario["key_wait_s"])
                    continue
                time.sleep(scenario["key_wait_s"])
            send("shot")
            shot, last = None, -1
            for _ in range(40):  # the file appears before it is written: wait for a non-empty, steady size
                time.sleep(0.5)
                shots = sorted(runtime.glob("shot-*.png"))
                size = shots[-1].stat().st_size if shots else 0
                if size and size == last:
                    shot = shots[-1]
                    break
                last = size
            if shot is None:
                raise Failure("no complete shot-N.png appeared")
        finally:
            os.close(fd)
        (out_dir / spec["file"]).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(shot, out_dir / spec["file"])
        want_log = spec.get("expect_log")
        if want_log and want_log not in (runtime / "plxnative-events.log").read_text(errors="replace"):
            raise Failure(f"{spec['file']}: the event log has no `{want_log}`")
        want = spec.get("expect_write")
        if want:
            method, _, path = want.partition(" ")
            if not any(w[0] == method and path in w[1] for w in pms.writes):
                raise Failure(f"{spec['file']}: the mock logged no `{want}` (writes: {[w[:2] for w in pms.writes]})")
    finally:
        if proc is not None:
            proc.kill()
            proc.wait()
            # The run's event log (a panic, a frame guard's catch) and console are kept beside the
            # capture as <file stem>.log / .out.
            stem = spec["file"][:-len(".png")]
            for name, ext in (("plxnative-events.log", ".log"), ("sim.out", ".out")):
                if (runtime / name).is_file():
                    shutil.copy(runtime / name, out_dir / (stem + ext))
        srv.shutdown()
        shutil.rmtree(runtime, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--sim", required=True, help="the built plxnative-sim (hostsim)")
    ap.add_argument("--out", help="output directory (default: the scenario's output_dir)")
    ap.add_argument("--scenario", default=str(SCENARIO), help="a scenario file (default: the committed one)")
    ap.add_argument("--only", nargs="*", default=[], help="capture file names to take (default: all)")
    args = ap.parse_args()
    scenario = json.loads(pathlib.Path(args.scenario).read_text())
    out_dir = pathlib.Path(args.out or scenario["output_dir"])
    out_dir.mkdir(parents=True, exist_ok=True)
    failed = []
    for spec in scenario["captures"]:
        if args.only and spec["file"] not in args.only:
            continue
        try:
            capture(pathlib.Path(args.sim), spec, scenario, out_dir)
            print(f"ok   {spec['file']}  ({spec['board']})")
        except Failure as e:
            failed.append(spec["file"])
            print(f"FAIL {spec['file']}: {e}", file=sys.stderr)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
