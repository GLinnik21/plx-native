#!/usr/bin/env python3
"""Take the Watchlist ui-sim captures named by tests/scenarios/watchlist_captures.json.

The ONE capture recipe (the scenario's `comment` repeats it):

  1. start tests/mock_pms.py in this process (loopback, `authorize_after=2`) and reset its Discover
     list to the capture's `mock` mode with `POST /_mock/discover {"list": <mode>}`;
  2. a fresh instance root with `plxnative-plextv` and `plxnative-discover` aimed at the mock;
     a prep boot with `plxnative-login` signs in through the app's own QR flow (the stored mock
     session is the account; no `token` trigger is armed);
  3. arm the capture's `triggers`, boot, wait `boot_wait_s`, send each `keys` entry through
     `plxnative-remote` (`hold` is okdown, 1.2 s, okup), check `expect_focus`, then `shot`;
     A capture REACHES its target by an accessibility-id token (tools/a11y.py, the app's devtriggers
     tokens) or by a boot trigger: `focus:<id>` moves focus to the element the id names (`nav.watchlist`,
     `watchlist.grid.card.7`, `home.shelf.2.card.0`) and `press:<id>` focuses it and presses OK, so no
     direction key stands between the boot and the target in any capture. What stays a key is a key whose
     subject is the key itself: `hold`, a timing (`ok@0.12`), `back`; and `ck:<x>,<y>` pointer clicks,
     which `w6/detail-offline.png` alone uses as its walk. An id is checked against the grammar before
     anything boots. A press of an element that acts on a title it names (`a11y.SUBJECT_BEARING` is the one
     list) NAMES that title, `press:<id>@<ratingKey>`, and the app refuses it when
     another title is there, so a hero that moved on or a menu opened on another card fails the capture
     instead of acting on it: `press:itemmenu.watchlist_remove@2001` (The Long Meridian, the mock's second
     Watchlist title), `press:home.hero.watchlist@200622` (the first Continue Watching episode of the
     `catalog` mock) and `press:detail.hero.watchlist@1001` (the Detail page of Harbor Lights, the
     `first_movie_off` mock's first movie). The one bare title press is `watchlist.grid.card.7`, a coming-soon card with no key.
     A key `mock:<json>` POSTs <json> to /_mock/discover mid-walk (`{"pms":"fail"}` makes the PMS side
     answer 503: once a library read fails and the list is read again, a title matched there reads
     ServerOffline); `lib:<json>` POSTs <json> to /_mock/library (a TEST-ONLY library add/remove plus the
     `timeline` frame the feed carries for it: W-notify); `sleep:<s>` waits <s> seconds; any other key, `ck:<x>,<y>` (a click at logical
     1920x1080 pixels) included, goes to the FIFO as it is;
     `expect_focus` names the full id the app must have focused when the walk is done: the driver asks
     for a `state` snapshot (`a11y.expect_focus`) and fails the capture, naming both ids, when it
     differs; a walk that ends in a TIMED key is checked before that key (its shot must follow it within
     the wait, and a `state` round trip is longer). A capture whose boot trigger decides what is focused
     (an opener row: `account-menu-new.png`, `item-menu-new.png`) or whose pointer clicks do
     (`w6/detail-offline.png`) may leave it out. A refused id (`a11y: refused ...` in the event log)
     fails the capture whatever the focus;
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

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import a11y  # noqa: E402  (tools/a11y.py: the by-id tokens and the `state` read-back)

ROOT = pathlib.Path(__file__).resolve().parent.parent
SCENARIO = ROOT / "tests" / "scenarios" / "watchlist_captures.json"  # --scenario overrides
SIGN_IN_DONE = "hubs: landed"  # the event-log line after which the sign-in's session is on disk
FOCUS_WAIT_S = 8  # how long `expect_focus` waits for the app to answer with the focus it wants


class Failure(Exception):
    pass


def step_kind(token):
    """How the driver sends one `keys` entry: `hold`, `mock`, `lib`, `sleep`, `byid` (`focus:<id>`,
    `press:<id>[@<ratingKey>]`) or `key` (everything else: a key name, `ck:<x>,<y>`, `ok@0.12`).
    The `@` of a by-id token is a subject's, never a timing's: only a `key` carries `key@<wait>`."""
    if token == "hold":
        return "hold"
    for kind in ("mock", "lib", "sleep"):
        if token.startswith(kind + ":"):
            return kind
    if token.startswith(("focus:", "press:")):
        return "byid"
    return "key"


def check_steps(file, keys):
    """Fail, before anything boots, on a by-id entry the app would refuse as malformed."""
    for token in keys:
        if step_kind(token) == "byid":
            try:
                a11y.parse_token(token)
            except ValueError as e:
                raise Failure(f"{file}: {e}")


def expect_position(keys):
    """Where in the walk `expect_focus` is checked: after the last entry, or BEFORE it when the last is a
    timed key (`ok@0.12`): that capture's shot must follow the key within the wait, and the `state` round
    trip is longer than a flip."""
    if keys and step_kind(keys[-1]) == "key" and "@" in keys[-1]:
        return len(keys) - 1
    return len(keys)


def check_focus(runtime, spec):
    """Fail the capture unless the app has the focus `spec["expect_focus"]` names (a no-op without one)."""
    want = spec.get("expect_focus")
    if not want:
        return
    try:
        a11y.expect_focus(runtime, want, timeout=FOCUS_WAIT_S)
    except a11y.FocusMismatch as e:
        events = runtime / "plxnative-events.log"
        refused = a11y.refusals(events.read_text(errors="replace")) if events.exists() else []
        raise Failure(f"{spec['file']}: {e}" + (f"; the app refused: {'; '.join(refused)}" if refused else ""))
    except TimeoutError as e:
        raise Failure(f"{spec['file']}: {e} (a RELEASE=1 build has no state: token, or the app is stalled)")


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
    proc = log = None
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

            keys = spec["keys"]
            at = expect_position(keys)
            for i, key in enumerate(keys):
                if i == at:
                    check_focus(runtime, spec)
                kind = step_kind(key)
                if kind == "hold":
                    send("okdown")
                    time.sleep(1.2)
                    send("okup")
                elif kind == "mock":
                    # `mock:{"pms":"fail"}`: POST that body to /_mock/discover mid-walk (the PMS goes
                    # away after the Watchlist matched a title on it: the server-offline state).
                    post(port, "/_mock/discover", json.loads(key[5:]))
                    continue
                elif kind == "lib":
                    # `lib:{"op":"add","title":"Northbound Tide"}`: POST that body to /_mock/library, a
                    # TEST-ONLY library change plus the `timeline` frame the feed would carry for it
                    # (the W-notify scenario: the tile flips without leaving the tab).
                    post(port, "/_mock/library", json.loads(key[4:]))
                    continue
                elif kind == "sleep":
                    time.sleep(float(key[6:]))
                    continue
                elif kind == "byid":
                    # `focus:<id>` / `press:<id>[@<ratingKey>]`: the app resolves the id at its ordered
                    # delivery position, so it acts after every token written before it.
                    send(key)
                else:
                    # `key@0.3` waits 0.3 s instead of key_wait_s before the next key (or the shot):
                    # the way to catch a spring mid-flight.
                    key, _, wait = key.partition("@")
                    send(key)
                    time.sleep(float(wait) if wait else scenario["key_wait_s"])
                    continue
                time.sleep(scenario["key_wait_s"])
            if at == len(keys):
                check_focus(runtime, spec)
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
        events = runtime / "plxnative-events.log"
        refused = a11y.refusals(events.read_text(errors="replace")) if events.exists() else []
        if refused:  # the picture is of a state the walk did not reach
            raise Failure(f"{spec['file']}: the app refused a by-id token: {'; '.join(refused)}")
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
        if log is not None:
            log.close()  # the console file the sim wrote to: a leaked handle is a ResourceWarning in a later test
        if proc is not None:
            # The run's event log (a panic, a frame guard's catch) and console are kept beside the
            # capture as <file stem>.log / .out. A failed capture (a focus mismatch, a missing shot) gets here
            # BEFORE the shot's own mkdir, so a `w6/x.png` capture makes its directory first: without it the
            # copy raises FileNotFoundError, which hides the Failure and ends the whole run.
            (out_dir / spec["file"]).parent.mkdir(parents=True, exist_ok=True)
            stem = spec["file"][:-len(".png")]
            for name, ext in (("plxnative-events.log", ".log"), ("sim.out", ".out")):
                if (runtime / name).is_file():
                    shutil.copy(runtime / name, out_dir / (stem + ext))
        srv.shutdown()
        srv.server_close()  # the listening socket too: a run of 25 captures leaves none behind
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
    for spec in scenario["captures"]:
        try:
            check_steps(spec["file"], spec["keys"])
            if spec.get("expect_focus") and not a11y.valid_id(spec["expect_focus"]):
                raise Failure(f"{spec['file']}: expect_focus {spec['expect_focus']!r} is not an accessibility id")
        except Failure as e:
            print(f"FAIL {e}", file=sys.stderr)
            return 1
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
