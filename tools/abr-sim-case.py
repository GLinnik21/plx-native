#!/usr/bin/env python3
"""Run one ABR pipeline case on the DESKTOP SIMULATOR — no television, no lock, N at once.

    tools/abr-sim-case.py pipe_abr_down_outrun [seconds]
    tools/abr-sim-case.py --list

**Why this exists.** The television is a mutex and the ABR tier costs ~40 minutes of it, because
every `abr_shape` case forces `--no-early` (a commit COUNT is not monotone, so a case cannot stop
when its assertions are satisfied). Since 2026-08-28 `make sim` builds a HOST copy of the bundled
FFmpeg, so `ff.rs` demuxes here: everything between the socket and the decoder runs on the Mac —
both AVIO transports, the HLS demux, the AU queues and their byte-cap backpressure, the feed-ahead
throttle, `ff.rs`'s rung transactions, seek and PTS rebase. This arms a real case against a real
`serve_fixtures.py` and lets the controller drive it.

It reads the case out of `tests/manifest.json` rather than restating it, so the declaration, the
fixture and the request-indexed `segment_profile` are the SAME ones the device tier arms. That is
the whole point: a divergence between the two tiers should come from the tier, not from two
hand-written copies of a case drifting apart.

**What it CANNOT tell you**, and the reasons are structural rather than temporary:

* **Nothing decodes.** `plxnative-clocksink` accepts access units, discards them, and advances a
  presentation clock at real time clamped to the last fed PTS. That is a faithful plant for the
  reserve the controller reads and it is not a decoder. Anything about LG's decoder — resource
  allocation, the Load payload's Dolby declaration, raster excursions, frame pacing — is
  device-only. `docs/webos10-resource-allocation.md` is what that blindness costs.
* **No number here is a device measurement.** Every heartbeat carries `sim=1` precisely so a
  pasted log cannot be mistaken for one, and this Mac's link to loopback is not a television's
  link to a PMS.
* **It does not grade.** It runs the case and prints where the log is; `tests/run.py` owns the
  assertions. Grading here would be a second copy of `a_abr_shape` free to disagree with the first.

**Traps, both cost time before they are known.** `SDL_VIDEODRIVER=dummy` fails with
`CreateWindow failed` — the simulator needs a real GL context, so a small `PLXNATIVE_WIN` is used
instead of hiding the window. And `serve()` returns `(server, url_base)`, not a server.
"""
import json
import os
import shutil
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, "tests"))
from run import triggers_for_case  # noqa: E402
from serve_fixtures import serve, default_root  # noqa: E402

# `PLXNATIVE_SIM_BIN` overrides which simulator is driven, which is what makes an A/B possible:
# point it at a simulator built from another commit and this still supplies HEAD's manifest,
# HEAD's fixtures and HEAD's shaper, so the ONLY thing that differs between the two legs is the
# app. Reconstructing an old policy by hand would grade a strawman; a checkout cannot.
SIM_BIN = os.environ.get(
    "PLXNATIVE_SIM_BIN",
    os.path.join(REPO, "rust-modules", "target-sim", "debug", "plxnative-sim"))


def cases():
    with open(os.path.join(REPO, "tests", "manifest.json")) as fh:
            # `pipe_auto_*` as well as `pipe_abr_*`: `pipe_auto_original_slow_recover` is the ONLY
        # case that starts in Original mode, and so the only one that reaches the Original -> HLS
        # handoff and the recovery back. A `pipe_abr` filter excludes it by name, which is how it
        # went unrun through a whole day of tiers.
        return [c for c in json.load(fh)["pipeline_cases"]
                if c["name"].startswith(("pipe_abr", "pipe_auto"))]


def main():
    argv = sys.argv[1:]
    if not argv or argv[0] in ("-h", "--help", "--list"):
        for c in cases():
            print(f"  {c['name']:<34} run_secs={c.get('run_secs', 60)}")
        return 0 if argv and argv[0] == "--list" else 1

    name = argv[0]
    case = next((c for c in cases() if c["name"] == name), None)
    if case is None:
        print(f"no such ABR case: {name} (try --list)", file=sys.stderr)
        return 2
    secs = int(argv[1]) if len(argv) > 1 else int(case.get("run_secs", 60))
    if not os.path.exists(SIM_BIN):
        print(f"no simulator at {SIM_BIN} — run `make sim` first", file=sys.stderr)
        return 2

    srv, base = serve(default_root(), port=0)
    # BOTH shapers, because the two cases that matter most use different ones and arming only one
    # silently streams over an unshaped link — which is how `pipe_abr_down_outrun` could not pass
    # by construction until 2026-08-28. `segment_profile` is request-indexed (exact, needed where a
    # transfer must go unaffordable AFTER the choice was made); `network_profile` is wall-clock
    # (what `pipe_auto_original_slow_recover`'s Original -> HLS -> Original arc is written against).
    srv.set_segment_profile(case.get("segment_profile"))
    srv.set_network_profile(case.get("network_profile"))
    srv.set_abr_response_profile(case.get("abr_response_profile"))

    # One instance root PER CASE, so several of these run side by side — which is the capability
    # the television does not have and the reason this file is worth its length.
    tag = os.environ.get("PLXNATIVE_SIM_TAG", "head")
    root = os.path.join("/tmp", "plxnative-sim-abr", tag, name)
    shutil.rmtree(root, ignore_errors=True)
    os.makedirs(root, exist_ok=True)
    write = lambda n, v: open(os.path.join(root, n), "w").write(v)  # noqa: E731
    # **The harness derives the trigger set; this does not restate it.** The module doc above
    # promises exactly that for the case, and the trigger derivation is the same promise one level
    # down — a hand-written copy of `triggers_for_case` lived here and had ALREADY drifted, because
    # it never read `case["operations"]`. `pipe_abr_seek_flat` is the one ABR case with a
    # non-`play` operation, so on this tier it ran without ever seeking: a case that shares a name
    # with the device tier's and grades a different thing, which is the precise failure the doc
    # says this file exists to avoid.
    for fname, content in triggers_for_case(case, url_base=base):
        write(fname, content or "")
    # The one trigger that is genuinely THIS tier's rather than the harness's: nothing decodes on a
    # Mac, so the clock sink stands in for LG's decoder. `tests/run.py` has no reason to know it.
    write("plxnative-clocksink", "")

    print(f"{name}: {secs}s, fixtures on {base}, root {root}")
    if case.get("segment_profile"):
        print(f"  segment_profile={case['segment_profile']}")
    env = dict(os.environ, PLXNATIVE_RUNTIME_DIR=root,
               PLXNATIVE_APP_DIR=os.path.join(REPO, "pkg"), PLXNATIVE_WIN="640x360")
    proc = subprocess.Popen([SIM_BIN, "127.0.0.1", "32400"], env=env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        proc.wait(timeout=secs)
    except subprocess.TimeoutExpired:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
    finally:
        srv.shutdown()

    log = os.path.join(root, "plxnative-events.log")
    if not os.path.exists(log):
        print("no event log — the simulator did not boot", file=sys.stderr)
        return 1
    with open(log, errors="replace") as fh:
        lines = fh.read().splitlines()
    tx = [ln for ln in lines if "abr: tx " in ln]
    print(f"\n{log}  ({len(lines)} lines, {sum('abr:' in l for l in lines)} abr:)")
    for ln in tx:
        print("  " + ln[ln.index("abr: tx "):][:132])
    return 0


if __name__ == "__main__":
    sys.exit(main())
