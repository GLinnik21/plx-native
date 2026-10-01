#!/usr/bin/env python3
"""Is the `frame.ui` phase GPU-bound, or waiting with the GPU idle?

  analyze-hwcnt-wait.py plxnative-hwcnt.jsonl [--phase frame.ui] [--discard 60]
  analyze-hwcnt-wait.py --self-test

Each JSONL `phase` row is one frame's phase bracketed by two glFinish calls, with the Mali
counters' delta over it (`--arm hwcnt=frame.ui`). This prints, per wall-time bucket, the GPU's own
active cycles (GPU_ACTIVE, JS0 = fragment jobs, JS1 = vertex/tiler jobs; whole-GPU, so the
compositor's jobs in the same window are included). The reading:
  * wall grows and the active cycles grow with it        -> the phase is GPU-bound;
  * wall grows and the active cycles stay flat (or ~0)   -> the GPU sat idle while the frame
    thread waited: a buffer held by the compositor / vsync, not GPU work.
The serialized wall is a calibration only (glFinish changes pacing): compare cycles, never fps.
Run tools/analyze-hwcnt.py on the same file for the full counter summary.
"""
import argparse
import collections
import contextlib
import io
import json
import os
import statistics
import tempfile

BUCKET_MS = 4
LAST_BUCKET_MS = 32
# Word index of each counter in the job-manager block (the first of the 64-word blocks), the same
# indices tools/analyze-hwcnt.py's SPECS use.
JM = {"GPU_ACTIVE": 6, "JS0_ACTIVE": 10, "JS1_ACTIVE": 18}


def load_rows(path, phase, discard):
    with open(path) as f:
        rows = [r for r in map(json.loads, (l for l in f if l.strip()))
                if r.get("type") == "phase" and r.get("name") == phase]
    return rows[discard:]


def report(path, rows, phase):
    buckets = collections.defaultdict(list)
    for r in rows:
        wall = r["serialized_wall_ns"] / 1e6
        key = min(int(wall // BUCKET_MS) * BUCKET_MS, LAST_BUCKET_MS)
        buckets[key].append((wall, *[r["interval"][w] for w in JM.values()]))
    print(f"{path}: {len(rows)} samples of {phase}")
    print(f"{'wall ms':>9} {'n':>6} " + " ".join(f"{k + ' p50':>16}" for k in JM)
          + "   GPU_ACTIVE cycles per wall-ms")
    for b in sorted(buckets):
        v = buckets[b]
        med = [statistics.median(x[i] for x in v) for i in range(1, len(JM) + 1)]
        per = statistics.median(x[1] / x[0] for x in v if x[0] > 0)
        print(f"{b:>4}-{b + BUCKET_MS:<4} {len(v):>6} " + " ".join(f"{m:>16.0f}" for m in med)
              + f"   {per:.0f}")


def self_test():
    def row(wall_ms, gpu, name="frame.ui"):
        interval = [0] * 64
        interval[JM["GPU_ACTIVE"]] = gpu
        return json.dumps({"type": "phase", "name": name,
                           "serialized_wall_ns": int(wall_ms * 1e6), "interval": interval})
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, "hwcnt.jsonl")
        with open(path, "w") as f:
            f.write(json.dumps({"type": "info"}) + "\n")
            f.write(row(1.0, 999) + "\n")                       # discarded
            for _ in range(3):
                f.write(row(5.0, 5000) + "\n")                  # 4-8 ms bucket, busy GPU
            for _ in range(2):
                f.write(row(30.0, 0) + "\n")                    # 28-32 ms bucket, idle GPU: waiting, not working
            f.write(row(50.0, 0) + "\n")                        # the last bucket takes everything from 32 ms up
            f.write(row(5.0, 777, name="other") + "\n")         # other phase, ignored
        rows = load_rows(path, "frame.ui", 1)
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            report(path, rows, "frame.ui")
        assert load_rows(path, "frame.ui", 0)[0]["interval"][6] == 999
    out = buf.getvalue().splitlines()
    assert "6 samples of frame.ui" in out[0], out
    assert out[2].split()[:2] == ["4-8", "3"] and out[2].split()[2] == "5000", out[2]
    assert out[2].split()[-1] == "1000", out[2]
    assert out[3].split()[:2] == ["28-32", "2"] and out[3].split()[-1] == "0", out[3]
    assert out[4].split()[:2] == ["32-36", "1"], out[4]
    print("analyze-hwcnt-wait self-test: ok")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("jsonl", nargs="?")
    ap.add_argument("--phase", default="frame.ui")
    ap.add_argument("--discard", type=int, default=60, help="warm-up rows to drop")
    ap.add_argument("--self-test", action="store_true", help="run against synthetic rows and exit")
    a = ap.parse_args()
    if a.self_test:
        return self_test()
    if not a.jsonl:
        ap.error("the JSONL path is required")
    rows = load_rows(a.jsonl, a.phase, a.discard)
    if not rows:
        raise SystemExit("no matching phase rows")
    report(a.jsonl, rows, a.phase)


if __name__ == "__main__":
    main()
