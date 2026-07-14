#!/usr/bin/env python3
"""
On-device regression harness for the webOS Plex player (plex-native-poc).

For each case in manifest.json this driver:
  1. closes the running app on the TV (luna-send closeByAppId + fuser -k, via `make kill`);
  2. (if the case sets a viewOffset) seeds the item's resume point server-side via
     PUT /:/progress -- AFTER the close, so a live timeline_thread can't re-scrobble over it;
  3. clears every /tmp/plxnative-* trigger on the TV, then writes only the ones this case needs;
  4. runs `make run TV=<tv> RUN_SECS=<n>`, which relaunches the app, waits, and cats
     /tmp/plxnative-events.log back;
  5. filters the `smp_cb type=43 num=0 str=` flood and evaluates the per-op assertions;
  6. records PASS/FAIL with the failing evidence line.

Security: the PMS X-Plex-Token is read from src/config.local.h at runtime and is NEVER
printed, logged, or written to any file. The TV ssh creds already live in the committed
Makefile, so we shell out to `make` / sshpass for device I/O.

Usage:
  ./tests/run.py --list                 # list cases and what they cover
  ./tests/run.py --build                # cargo + make + make deploy, then run all cases
  ./tests/run.py --filter morning       # run only cases whose name contains "morning"
  ./tests/run.py                        # run every case (assumes app already deployed)

Exit code is nonzero if any selected case fails.

No third-party deps -- Python 3 stdlib only (macOS system python3 is fine).
"""

import argparse
import json
import os
import re
import subprocess
import sys
import urllib.parse
import urllib.request

# ---------------------------------------------------------------------------
# Paths / constants
# ---------------------------------------------------------------------------
TESTS_DIR = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(TESTS_DIR)
MANIFEST = os.path.join(TESTS_DIR, "manifest.json")
CONFIG_LOCAL_H = os.path.join(REPO_ROOT, "src", "config.local.h")

# reference list of the dev triggers the app reads (apply_triggers now GLOB-clears /tmp/plxnative-*,
# so this no longer has to be exhaustive — it's kept for humans / grep)
ALL_TRIGGERS = [
    "plxnative-detail", "plxnative-detailplay", "plxnative-detailsec", "plxnative-detailcol",
    "plxnative-autoseek", "plxnative-menupick", "plxnative-menu", "plxnative-noaudio", "plxnative-demux",
    "plxnative-grid", "plxnative-autoplay", "plxnative-h265", "plxnative-playidx", "plxnative-url",
    "plxnative-play", "plxnative-ffprobe", "plxnative-token",
    # UI/FPS scenes (plxnative-profile MUST be cleared — a stale one glFinish-tanks FPS and false-fails)
    "plxnative-detailosc", "plxnative-info", "plxnative-chapters", "plxnative-profile",
    # boot-flow triggers (heroidx pins the hero + bypasses the who's-watching picker; pickuser forces it)
    "plxnative-heroidx", "plxnative-pickuser",
]

# the type=43 spam filter (mirrors: grep -vaE "smp_cb type=43 num=0 str=$")
TYPE43_SPAM = re.compile(r"smp_cb type=43 num=0 str=\s*$")


# ---------------------------------------------------------------------------
# Token (never printed)
# ---------------------------------------------------------------------------
def read_token():
    """Extract PMS_TOKEN "..." from the gitignored src/config.local.h."""
    try:
        with open(CONFIG_LOCAL_H, "r") as f:
            txt = f.read()
    except OSError as e:
        sys.exit(f"cannot read {CONFIG_LOCAL_H}: {e}\n"
                 f"(this file is gitignored and holds the PMS token; create it locally)")
    m = re.search(r'#define\s+PMS_TOKEN\s+"([^"]+)"', txt)
    if not m:
        sys.exit(f"no PMS_TOKEN macro found in {CONFIG_LOCAL_H}")
    return m.group(1)


CID = "plxnative-test-harness"  # stable X-Plex-Client-Identifier for the plex.tv calls below


def _pms_machine_id(host, port, admin_token):
    """The server's machineIdentifier (needed to look up its shared_servers on plex.tv)."""
    url = f"http://{host}:{port}/?X-Plex-Token={admin_token}"
    req = urllib.request.Request(url, headers={"Accept": "application/json"})
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.load(resp)["MediaContainer"]["machineIdentifier"]


def fetch_managed_user_token(admin_token, host, port, user_id):
    """Resolve a Plex Home managed user's PER-SERVER access token from the owner's
    shared_servers list (keyed by userID), so test playback runs as that user and its watch
    history never touches the owner's real account. Uses only the admin token already on hand --
    no new secret is stored. Returns the token string (never printed) or exits on failure."""
    import xml.etree.ElementTree as ET
    mid = _pms_machine_id(host, port, admin_token)
    url = f"https://plex.tv/api/servers/{mid}/shared_servers"
    req = urllib.request.Request(url, headers={
        "X-Plex-Token": admin_token, "X-Plex-Client-Identifier": CID})
    with urllib.request.urlopen(req, timeout=20) as resp:
        root = ET.fromstring(resp.read())
    for s in root.findall("SharedServer"):
        if s.get("userID") == str(user_id):
            tok = s.get("accessToken")
            if tok:
                return tok
            sys.exit(f"managed user {user_id} is shared but has no accessToken")
    sys.exit(f"managed user {user_id} has no shared_servers entry on this server "
             f"(share the libraries with it first, or run with --owner)")


# ---------------------------------------------------------------------------
# Device I/O
# ---------------------------------------------------------------------------
def ssh(tv, remote_cmd, timeout=30):
    """Run a command on the TV. Mirrors the Makefile's committed sshpass creds."""
    cmd = [
        "sshpass", "-p", "alpine", "ssh",
        "-o", "StrictHostKeyChecking=no",
        "-o", "UserKnownHostsFile=/dev/null",
        "-o", "ConnectTimeout=8",
        f"root@{tv}", remote_cmd,
    ]
    return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)


def make(target_args, timeout, capture=True):
    """Invoke a make target from the repo root (absolute cwd so nothing drifts)."""
    cmd = ["make", "-s", "-C", REPO_ROOT] + target_args
    return subprocess.run(cmd, capture_output=capture, text=True, timeout=timeout)


def pms_put_progress(host, port, rk, time_ms, token):
    """Seed an item's resume point (viewOffset) via PUT /:/progress. Token never printed."""
    q = urllib.parse.urlencode({
        "key": rk,
        "identifier": "com.plexapp.plugins.library",
        "time": str(time_ms),
        "state": "stopped",
        "X-Plex-Token": token,
    })
    url = f"http://{host}:{port}/:/progress?{q}"
    redacted = url.replace(token, "<token>")
    req = urllib.request.Request(url, method="PUT")
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            print(f"    progress set: {redacted} -> {resp.status}")
            return True
    except Exception as e:
        print(f"    WARN: progress PUT failed ({redacted}): {e}")
        return False


# ---------------------------------------------------------------------------
# Trigger derivation
# ---------------------------------------------------------------------------
def triggers_for_case(case):
    """
    Map a case's operations -> the /tmp/plxnative-* files to write on the TV.
    Returns a list of (filename, content-or-None) pairs; None => `touch` (empty marker).
    """
    files = [("plxnative-play", case["rk"])]  # the robust play trigger (fetches any rk)
    for op in case["operations"]:
        kind = op["op"]
        if kind == "seek":
            files.append(("plxnative-autoseek", None))          # touch -> one seek to 140s
        elif kind == "audio_switch":
            files.append(("plxnative-menupick", f'{op["tab"]},{op["row"]}'))
        elif kind == "subtitle":
            files.append(("plxnative-menupick", f'{op["tab"]},{op["row"]}'))
            if op.get("demux") == "mkv":
                # Optional: force the legacy mkv.rs demuxer (H264-only) to regression-test that
                # path specifically. The DEFAULT libavformat demuxer (ff.rs) now emits `sub cue
                # [..]` lines too, so subtitle cases no longer need this — omit `demux` to run on
                # the default and cover HEVC/mp4 as well.
                files.append(("plxnative-demux", "mkv"))
        # "play" and "resume" need no extra trigger (resume rides the seeded viewOffset).
    return files


def apply_triggers(tv, files):
    """Clear every /tmp/plxnative-* trigger (sparing the *.log files), then create the ones this case
    needs, in one ssh round-trip. GLOB-based, not an enumerated list, so a newly-added app trigger can
    never bleed between scenes — a stale plxnative-novsync would uncap vsync and false-PASS an FPS
    scene, a stale plxnative-press/-login would derail a home scene. ALL_TRIGGERS above is now just a
    human reference of the known triggers."""
    # wipe every trigger, keeping only the append-only logs (events/stderr/crash)
    parts = ['for f in /tmp/plxnative-*; do case "$f" in *.log) ;; *) rm -f "$f";; esac; done']
    for name, content in files:
        if content is None:
            parts.append(f"touch /tmp/{name}")
        else:
            # single-quote the content; rks / "0,6" / "mkv" never contain quotes
            parts.append(f"printf '%s' '{content}' > /tmp/{name}")
    ssh(tv, "; ".join(parts))


# ---------------------------------------------------------------------------
# Log parsing
# ---------------------------------------------------------------------------
def filter_log(raw):
    """Drop the type=43 flood; return the surviving lines as a list."""
    return [ln for ln in raw.splitlines() if not TYPE43_SPAM.search(ln)]


RE_DECISION = re.compile(r"decision: part=.* -> (DIRECT PLAY|TRANSCODE)")
RE_CODEC = re.compile(r"ff: v=#0 codec_id=(\d+)\s+(\d+)x(\d+)")
RE_TIMELINE = re.compile(r"timeline playing t=(\d+)s/")
RE_SUBCUE = re.compile(r'sub cue \[\d+\.\.\d+ms\]\s+"(.*)"')
# image (PGS/VobSub) subtitle cue: the demuxer decoded a bitmap display-set for the selected
# track and pushed it to the render store (ff.rs decode_bitmap_cue). Distinct from the text
# `sub cue` signal — image subs carry no text, only geometry.
RE_IMGCUE = re.compile(r"image cue \[\d+ms\]\s+\d+x\d+ at \d+,\d+")
# the `stream: host=.. path=<url>` line the demux logs when it opens the media URL -- the
# ground truth of direct-play (`/library/parts/..`) vs transcode (`/transcode/universal/..`).
# This is the ONLY universal decision signal: the smart/fast direct-play path in build_stream
# short-circuits and never logs a `decision:` line (that is emitted only when server_decision
# runs), so keying solely on `decision:` false-FAILs every plain direct-play case.
# NON-greedy up to the FIRST path= — the transcode URL is
# `path=/video/:/transcode/..start.mkv?path=%2Flibrary..`, and a greedy .* would grab the
# inner (URL-encoded) query param instead of the real request path.
RE_STREAM_PATH = re.compile(r"stream:.*?path=(\S+)")
# the media URL carries the secret X-Plex-Token; strip it from anything we print/log.
RE_TOKEN = re.compile(r"(X-Plex-Token=)[^&\s]+")


def redact(s):
    """Never let the PMS token reach stdout: replace any X-Plex-Token=<v> with <token>."""
    return RE_TOKEN.sub(r"\1<token>", s)


def codec_ids(lines):
    """All (codec_id, width, height) from `ff: v=#0` lines, in order."""
    out = []
    for ln in lines:
        m = RE_CODEC.search(ln)
        if m:
            out.append((int(m.group(1)), int(m.group(2)), int(m.group(3)), ln))
    return out


def timeline_secs(lines):
    """All `timeline playing t=<S>s` values in order."""
    return [(int(m.group(1)), ln) for ln in lines for m in [RE_TIMELINE.search(ln)] if m]


def find(lines, needle):
    for ln in lines:
        if needle in ln:
            return ln
    return None


# ---------------------------------------------------------------------------
# Assertions -- each returns (ok: bool, evidence: str)
# ---------------------------------------------------------------------------
def a_decision(lines, expected):
    want = "DIRECT PLAY" if expected == "directplay" else "TRANSCODE"
    # Primary signal: the actual URL the demux opened. This is emitted on EVERY playback,
    # unlike `decision:` which the smart direct-play fast path skips. `/library/parts/..`
    # is a raw file GET (direct play); `/transcode/universal/..` is a server transcode.
    for ln in lines:
        m = RE_STREAM_PATH.search(ln)
        if m:
            path = m.group(1)
            if "/transcode/" in path:
                got = "TRANSCODE"
            elif "/library/parts/" in path:
                got = "DIRECT PLAY"
            else:
                continue  # a subtitle/other stream line -- not the media decision
            return (got == want), f"stream path -> {got} (want {want}) :: {redact(ln.strip())}"
    # Fallback: the explicit `decision:` line (only server_decision emits it -- transcode
    # items and the local-heuristic path).
    for ln in lines:
        mm = RE_DECISION.search(ln)
        if mm:
            got = mm.group(1)
            return (got == want), f"decision={got} (want {want}) :: {redact(ln.strip())}"
    return False, "no `stream: ... path=` or `decision: ... ->` line found"


def a_codec(lines, expected_id, min_width):
    cs = codec_ids(lines)
    if not cs:
        return False, "no `ff: v=#0 codec_id=` line found"
    cid, w, h, ln = cs[0]
    ok = (cid == expected_id) and (w >= min_width)
    return ok, f"codec_id={cid} {w}x{h} (want id={expected_id} w>={min_width}) :: {ln.strip()}"


def a_no_error(lines):
    for ln in lines:
        if "smp_cb type=18" in ln or "Playing error" in ln:
            return False, f"error surfaced :: {ln.strip()}"
    return True, "no `smp_cb type=18` / `Playing error`"


def a_video_bound(lines):
    ln = find(lines, "setMediaVideoData sent")
    return (ln is not None), (ln.strip() if ln else "no `setMediaVideoData sent` (video plane never bound)")


def a_timeline_climb(lines, min_climb):
    ts = timeline_secs(lines)
    if len(ts) < 2:
        return False, f"only {len(ts)} `timeline playing t=` line(s); need >=2 that climb"
    lo = min(t for t, _ in ts)
    hi = max(t for t, _ in ts)
    ok = (hi - lo) >= min_climb
    return ok, f"timeline {lo}s..{hi}s (climb {hi-lo}s, need >={min_climb}s) over {len(ts)} reports"


# ---- per-op assertions ----
def op_seek_inplace(lines, target_s):
    started = find(lines, "seek(ff in-place)")
    if started is None:
        return False, "no `seek(ff in-place)` line (in-place seek did not fire)"
    seg_ok = any("sendSegment=1" in ln for ln in lines if "in-place seek:" in ln)
    if not seg_ok:
        ln = find(lines, "in-place seek:")
        return False, f"in-place seek lacked sendSegment=1 :: {ln.strip() if ln else 'no in-place seek: line'}"
    if find(lines, "reload_at: fresh Load"):
        return False, "in-place seek fell back to a reload (`reload_at: fresh Load` present)"
    ts = timeline_secs(lines)
    reached = max((t for t, _ in ts), default=-1)
    if reached < target_s - 6:
        return False, f"timeline reached only {reached}s, expected >= ~{target_s}s after seek"
    return True, f"in-place seek OK; reached {reached}s :: {started.strip()}"


def op_seek_transcode(lines, target_s):
    # transcode seeks now RELOAD the pipeline (reload_transcode: fresh Load = correct GStreamer
    # segment, no stale-segment artifacts). Older builds flushed+refed (`seek(transcode)`) or fell
    # back to reload_at.
    hit = find(lines, "reload_transcode: fresh Load at offset") or find(lines, "seek(transcode)") \
        or find(lines, "reload_at: fresh Load at %ds" % target_s)
    if hit is None:
        return False, "no transcode-seek signal (reload_transcode / seek(transcode) / reload_at) present"
    ts = timeline_secs(lines)
    reached = max((t for t, _ in ts), default=-1)
    if reached < target_s - 6:
        return False, f"timeline reached only {reached}s, expected >= ~{target_s}s after seek"
    return True, f"transcode/reload seek OK; reached {reached}s :: {hit.strip()}"


def op_audio_native(lines):
    hit = find(lines, "audio switch (native)")
    if hit is None:
        return False, "no `audio switch (native)` line (switch was not native)"
    cs = codec_ids(lines)
    if not cs:
        return False, "no ff codec line after switch"
    last_id = cs[-1][0]
    if last_id != 174:
        return False, f"codec after native switch = {last_id}, expected 174 (should stay HEVC) :: {cs[-1][3].strip()}"
    return True, f"native switch OK; codec stayed 174 :: {hit.strip()}"


def op_audio_transcode(lines):
    re_t = find(lines, "re-transcode:")
    rl_t = find(lines, "reload_transcode:")
    if re_t is None or rl_t is None:
        return False, f"missing transcode-switch logs (re-transcode={bool(re_t)} reload_transcode={bool(rl_t)})"
    cs = codec_ids(lines)
    # the transcode target is HEVC (keeps 4K + HDR10), so an audio-forced re-transcode
    # re-encodes the video to HEVC (174). NB: a future "audio-only transcode" that COPIES the
    # video would leave it at the source codec instead — update this expectation if that lands.
    if cs and cs[-1][0] != 174:
        return False, f"codec after transcode switch = {cs[-1][0]}, expected 174 (HEVC target) :: {cs[-1][3].strip()}"
    return True, f"transcode switch OK :: {rl_t.strip()}"


def op_subtitle(lines):
    for ln in lines:
        m = RE_SUBCUE.search(ln)
        if m and m.group(1).strip():
            return True, f"sub cue rendered :: {ln.strip()}"
    return False, "no `sub cue [..] \"text\"` line with non-empty text"


def op_image_subtitle(lines):
    """PGS/VobSub: the demuxer must decode a bitmap display-set for the selected track and log
    an `image cue` line (which the renderer composites over the video as a GL texture)."""
    for ln in lines:
        if RE_IMGCUE.search(ln):
            return True, f"image sub cue decoded + stored :: {ln.strip()}"
    return False, "no `image cue [..] WxH at X,Y` line (PGS/VobSub bitmap not decoded)"


def op_resume_directplay(lines, offset_s):
    ts = timeline_secs(lines)
    if not ts:
        return False, "no `timeline playing t=` line to check resume position"
    first_s, ln = ts[0]
    floor = int(offset_s * 0.6)
    ok = first_s >= floor
    return ok, f"first timeline t={first_s}s (want >= {floor}s, offset {offset_s}s) :: {ln.strip()}"


def op_resume_transcode(lines, offset_s):
    hit = None
    for ln in lines:
        m = re.search(r"resume\(transcode\): restart at offset (\d+)s", ln)
        if m:
            hit = (int(m.group(1)), ln)
            break
    if hit is None:
        return False, "no `resume(transcode): restart at offset <s>s` line"
    got, ln = hit
    if abs(got - offset_s) > max(15, offset_s * 0.1):
        return False, f"resume offset {got}s != expected ~{offset_s}s :: {ln.strip()}"
    ts = timeline_secs(lines)
    first_s = ts[0][0] if ts else -1
    if first_s < int(offset_s * 0.6):
        return False, f"first timeline t={first_s}s not near offset {offset_s}s"
    return True, f"transcode resume OK at {got}s; first timeline {first_s}s :: {ln.strip()}"


# ---------------------------------------------------------------------------
# Case execution
# ---------------------------------------------------------------------------
def uses_mkv_demux(case):
    """True if a case forces the legacy mkv.rs demuxer (subtitle cases) — it's the only demuxer
    that emits `sub cue` lines, but it doesn't log the libavformat `ff: codec_id` line."""
    return any(op.get("demux") == "mkv" for op in case.get("operations", []))


def evaluate(case, lines):
    """Run every assertion for a case. Returns (passed, [(label, ok, evidence)])."""
    exp = case["expect"]
    results = []

    # base assertions (every case)
    results.append(("decision", *a_decision(lines, exp["decision"])))
    # The legacy mkv.rs demuxer (subtitle cases) doesn't emit `ff: v=#0 codec_id=`; skip the
    # ffmpeg-codec check there — video_bound + timeline_climb still prove the frame decoded.
    if not uses_mkv_demux(case):
        results.append(("codec", *a_codec(lines, exp["codec_id"], exp["min_video_width"])))
    if exp.get("require_video_bound", True):
        results.append(("video_bound", *a_video_bound(lines)))
    results.append(("timeline_climb", *a_timeline_climb(lines, exp.get("min_timeline_climb_s", 12))))
    if exp.get("no_playing_error", True):
        results.append(("no_error", *a_no_error(lines)))

    # per-operation assertions
    for op in case["operations"]:
        k = op["op"]
        if k == "seek" and op.get("mode") == "inplace":
            results.append(("seek_inplace", *op_seek_inplace(lines, op.get("target_s", 140))))
        elif k == "seek":
            results.append(("seek_transcode", *op_seek_transcode(lines, op.get("target_s", 140))))
        elif k == "audio_switch" and op.get("mode") == "native":
            results.append(("audio_native", *op_audio_native(lines)))
        elif k == "audio_switch":
            results.append(("audio_transcode", *op_audio_transcode(lines)))
        elif k == "subtitle" and op.get("image"):
            results.append(("image_subtitle", *op_image_subtitle(lines)))
        elif k == "subtitle":
            results.append(("subtitle", *op_subtitle(lines)))
        elif k == "resume" and op.get("mode") == "transcode":
            results.append(("resume_transcode", *op_resume_transcode(lines, op.get("offset_s", 600))))
        elif k == "resume":
            results.append(("resume_directplay", *op_resume_directplay(lines, op.get("offset_s", 600))))

    passed = all(ok for _, ok, _ in results)
    return passed, results


def run_case(case, cfg, token, verbose):
    name = case["name"]
    tv = cfg["tv"]
    run_secs = case.get("run_secs", 60)
    print(f"\n=== {name}  (rk={case['rk']}, {case.get('title','')}) ===")
    print(f"    covers: {', '.join(case.get('covers', []))}")

    # 1. close the app first (so a live timeline_thread can't overwrite the viewOffset)
    make(["kill", f"TV={tv}"], timeout=40)

    # 2. seed the resume point AFTER the close
    setup = case.get("setup", {})
    if "viewOffset_ms" in setup:
        pms_put_progress(cfg["pms"]["host"], cfg["pms"]["port"], case["rk"],
                         setup["viewOffset_ms"], token)

    # 3. clear + set triggers
    files = triggers_for_case(case)
    apply_triggers(tv, files)
    shown = ", ".join(n + ("=" + c if c is not None else "") for n, c in files)
    print(f"    triggers: {shown}")

    # 3b. inject the effective PMS token so the APP itself plays (and scrobbles) as this user.
    # Written in its own ssh round-trip so the token value never reaches stdout; plxnative-token was
    # just cleared by apply_triggers (it's in ALL_TRIGGERS). Always required: the binary carries
    # no baked token — /tmp/plxnative-token is the only way an automated run gets PMS access.
    if cfg.get("inject_token"):
        ssh(tv, f"printf '%s' '{token}' > /tmp/plxnative-token")
        print(f"    plxnative-token: <{cfg['user_label']}, redacted>")

    # 4. run + fetch log
    print(f"    make run RUN_SECS={run_secs} ...")
    try:
        proc = make(["run", f"TV={tv}", f"RUN_SECS={run_secs}"], timeout=run_secs + 90)
    except subprocess.TimeoutExpired:
        print("    FAIL: make run timed out")
        return False, [("harness", False, "make run timed out")], []
    lines = filter_log(proc.stdout + "\n" + proc.stderr)

    # 5. evaluate
    passed, results = evaluate(case, lines)
    for label, ok, evidence in results:
        mark = "PASS" if ok else "FAIL"
        if ok and not verbose:
            print(f"      [{mark}] {label}")
        else:
            print(f"      [{mark}] {label}: {redact(evidence)}")  # never leak the token
    print(f"    => {'PASS' if passed else 'FAIL'}")
    return passed, results, lines


# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------
def do_build(tv):
    print("=== BUILD: cargo zigbuild -> make -> make deploy ===")
    rust_dir = os.path.join(REPO_ROOT, "rust-modules")
    env = dict(os.environ)
    env["PATH"] = os.path.expanduser("~/.cargo/bin") + os.pathsep + env.get("PATH", "")
    env["RUSTFLAGS"] = "-C target-cpu=cortex-a53 -C target-feature=-neon"
    cargo = subprocess.run(
        ["cargo", "+nightly", "zigbuild", "-Z", "build-std=std,panic_unwind",
         "--release", "--target", "arm-unknown-linux-gnueabi.2.24"],
        cwd=rust_dir, env=env, timeout=1200)
    if cargo.returncode != 0:
        sys.exit("cargo build failed")
    if make(["all"], timeout=600, capture=False).returncode != 0:
        sys.exit("make (link) failed")
    if make(["deploy", f"TV={tv}"], timeout=180, capture=False).returncode != 0:
        sys.exit("make deploy failed")
    print("=== BUILD OK ===")


# ---------------------------------------------------------------------------
# FPS regression suite (UI perf gate — separate from the playback cases above).
# The app logs a once/sec `FPS=<n> route=<home|detail|player> [overlay=<info|chapters|menu|none>]`
# heartbeat. Each scene sets its plxnative-* triggers, runs the app profiler-OFF, then asserts the steady
# framerate for that screen stays above a floor. This is the automated form of the by-hand FPS
# hunting that found the hero / cast+about / info-panel regressions.
# ---------------------------------------------------------------------------
FPS_RE = re.compile(r"FPS=(\d+) route=(\w+)(?: overlay=(\w+))?")


def parse_fps(lines, route, overlay):
    """The FPS heartbeat samples (ints) whose route (+overlay, if the scene pins one) match."""
    out = []
    for ln in lines:
        m = FPS_RE.search(ln)
        if not m or m.group(2) != route:
            continue
        if overlay and (m.group(3) or "none") != overlay:
            continue
        out.append(int(m.group(1)))
    return out


def fps_stats(vals):
    s = sorted(vals)
    n = len(s)
    # The gate is the 2nd-lowest sample: it tolerates ONE transient dip (a mid-run texture upload or
    # GC pause) while a *sustained* regression — every sample low — still fails.
    robust_min = s[1] if n >= 2 else (s[0] if s else 0)
    return {"n": n, "min": s[0] if s else 0, "median": s[n // 2] if n else 0, "robust_min": robust_min}


def run_fps_scene(scene, cfg, token):
    name = scene["name"]
    tv = cfg["tv"]
    route = scene["route"]
    overlay = scene.get("overlay")  # None for home/detail
    floor = scene["floor"]
    warmup = scene.get("warmup_s", 5)
    run_secs = scene.get("run_secs", 18)
    is_player = scene.get("tier", "ui") == "player"
    tag = route + (f"/{overlay}" if overlay else "")
    print(f"\n=== fps:{name}  (route={tag}, floor {floor}fps) ===")

    make(["kill", f"TV={tv}"], timeout=40)
    files = []
    for tname, tval in scene.get("triggers", {}).items():
        if tval is True:
            files.append((tname, None))
        elif tval == "$rk":
            files.append((tname, str(scene["rk"])))
        else:
            files.append((tname, str(tval)))
    apply_triggers(tv, files)  # clears every plxnative-* (incl. plxnative-profile) then writes this scene's
    # player-tier scenes actually decode video, so inject the test-user token (its own ssh round-trip,
    # so the value never reaches stdout) exactly like the playback cases do.
    if is_player and cfg.get("inject_token"):
        ssh(tv, f"printf '%s' '{token}' > /tmp/plxnative-token")
    shown = ", ".join(n + ("=" + c if c is not None else "") for n, c in files)
    print(f"    triggers: {shown or '(none)'}   run {run_secs}s, skip first {warmup} sample(s)")

    try:
        proc = make(["run", f"TV={tv}", f"RUN_SECS={run_secs}"], timeout=run_secs + 90)
    except subprocess.TimeoutExpired:
        print("    [FAIL] make run timed out")
        return False, "make run timed out"
    lines = filter_log(proc.stdout + "\n" + proc.stderr)

    alls = parse_fps(lines, route, overlay)
    samples = alls[warmup:]  # heartbeat is ~1/sec, so drop the first `warmup` matching samples
    st = fps_stats(samples)

    # False-negative guard: too few post-warmup samples means the scene never really reached this
    # screen (app crash, or a detail/play trigger that didn't open — e.g. an rk not in the home
    # catalog). That is a FAIL, never a vacuous pass.
    if st["n"] < 5:
        msg = (f"only {st['n']} post-warmup FPS samples for route={tag} (need >= 5) — scene never "
               f"entered this screen? ({len(alls)} total matched before warmup)")
        print(f"    [FAIL] {msg}")
        return False, msg

    ok = st["robust_min"] >= floor
    detail = f"robust_min={st['robust_min']}fps (min={st['min']}, median={st['median']}, n={st['n']}) vs floor {floor}"
    print(f"    [{'PASS' if ok else 'FAIL'}] {detail}")
    return ok, detail


def run_fps_suite(scenes, cfg, token, include_player):
    tiers = {"ui"} | ({"player"} if include_player else set())
    scenes = [s for s in scenes if s.get("tier", "ui") in tiers]
    if not scenes:
        sys.exit("no FPS scenes defined in the manifest for the selected tier(s)")
    print(f"=== FPS regression suite: {len(scenes)} scene(s), tiers={sorted(tiers)} ===")
    results = []
    for s in scenes:
        try:
            ok, detail = run_fps_scene(s, cfg, token)
        except Exception as e:  # keep the batch going
            ok, detail = False, f"ERROR: {e}"
            print(f"    [FAIL] ERROR: {e}")
        results.append((s["name"], ok, detail))

    print("\n" + "=" * 72 + "\nFPS SUMMARY\n" + "=" * 72)
    nfail = sum(1 for _, ok, _ in results if not ok)
    for name, ok, detail in results:
        print(f"  [{'PASS' if ok else 'FAIL'}] fps:{name}   {detail}")
    print(f"\n{len(results) - nfail} passed, {nfail} failed of {len(results)}")
    return 0 if nfail == 0 else 1


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------
def main():
    ap = argparse.ArgumentParser(description="webOS Plex player on-device regression harness")
    ap.add_argument("--build", action="store_true", help="cargo + make + make deploy before running")
    ap.add_argument("--filter", default=None, help="run only cases whose name contains this substring")
    ap.add_argument("--list", action="store_true", help="list cases and exit")
    ap.add_argument("--tv", default=None, help="override TV IP (default from manifest)")
    ap.add_argument("--verbose", action="store_true", help="print evidence for passing assertions too")
    ap.add_argument("--owner", action="store_true",
                    help="run as the config.local.h OWNER token (default: run as the manifest "
                         "test_user, e.g. Guest, so watch history stays off your real account)")
    ap.add_argument("--fps", action="store_true",
                    help="run the FPS regression suite (UI tier: home/detail, no video needed)")
    ap.add_argument("--fps-player", action="store_true",
                    help="FPS suite INCLUDING player-tier scenes (info/menu — needs playback, slower)")
    args = ap.parse_args()

    with open(MANIFEST) as f:
        manifest = json.load(f)
    cfg = {
        "tv": args.tv or manifest.get("tv", "192.168.0.114"),
        "pms": manifest.get("pms", {"host": "192.168.0.3", "port": 32400}),
    }
    cases = manifest["cases"]
    if args.filter:
        cases = [c for c in cases if args.filter in c["name"]]

    if args.list:
        for c in manifest["cases"]:
            ops = "+".join(o["op"] for o in c["operations"])
            print(f"{c['name']:32s} rk={c['rk']:<5} {ops:20s} {', '.join(c.get('covers', []))}")
        for s in manifest.get("fps_scenes", []):
            tag = s["route"] + (f"/{s.get('overlay')}" if s.get("overlay") else "")
            print(f"fps:{s['name']:28s} tier={s.get('tier','ui'):6s} {tag:16s} floor={s['floor']}")
        return 0

    # FPS regression suite — a separate path from the playback cases. UI-tier scenes need no video
    # (and no PMS token); --fps-player adds the info/menu scenes, which decode video as the test user.
    if args.fps or args.fps_player:
        include_player = args.fps_player
        token = None
        if include_player:
            admin_token = read_token()
            test_user = manifest.get("test_user")
            if args.owner or not test_user:
                token, cfg["inject_token"] = admin_token, True  # no baked token in the binary
            else:
                token = fetch_managed_user_token(admin_token, cfg["pms"]["host"],
                                                 cfg["pms"]["port"], test_user["id"])
                cfg["inject_token"] = True
        if args.build:
            do_build(cfg["tv"])
        return run_fps_suite(manifest.get("fps_scenes", []), cfg, token, include_player)

    if not cases:
        sys.exit(f"no cases match --filter {args.filter!r}")

    admin_token = read_token()  # owner token from config.local.h; never printed

    # Resolve the identity every case plays as. Default = the manifest test_user (Guest), so
    # playback + timeline scrobbles land on that user's history and the owner's real account
    # stays clean. --owner opts back into the owner token. Neither token is ever printed.
    test_user = manifest.get("test_user")
    if args.owner or not test_user:
        token = admin_token
        cfg["user_label"] = "owner (config.local.h)"
        cfg["inject_token"] = True  # the binary has NO baked token; every identity is injected
        if not args.owner:
            print("NOTE: no test_user in manifest -> running as OWNER (history WILL be affected)")
    else:
        token = fetch_managed_user_token(admin_token, cfg["pms"]["host"], cfg["pms"]["port"],
                                         test_user["id"])
        cfg["user_label"] = f'{test_user.get("title", "managed")} (id={test_user["id"]})'
        cfg["inject_token"] = True
    print(f"test identity: {cfg['user_label']}  (playback + watch-history isolation)")

    if args.build:
        do_build(cfg["tv"])

    summary = []
    for c in cases:
        try:
            passed, results, _ = run_case(c, cfg, token, args.verbose)
        except Exception as e:  # keep the batch going; record the failure
            print(f"    ERROR running {c['name']}: {e}")
            passed, results = False, [("harness", False, str(e))]
        fails = [label for label, ok, _ in results if not ok]
        summary.append((c["name"], passed, fails, c.get("known_gap")))

    # final table. A case tagged `known_gap` that fails is XFAIL (a documented, expected gap —
    # not a regression); it does NOT fail the suite. If it unexpectedly passes it is XPASS.
    print("\n" + "=" * 72)
    print("SUMMARY")
    print("=" * 72)
    npass = real_fail = nxfail = 0
    for name, passed, fails, gap in summary:
        if passed:
            npass += 1
            mark = "XPASS" if gap else "PASS"
            detail = "  (known gap unexpectedly passes)" if gap else ""
        elif gap:
            nxfail += 1
            mark = "XFAIL"
            detail = "  <- " + ", ".join(fails) + f"  (known gap: {gap})"
        else:
            real_fail += 1
            mark = "FAIL"
            detail = "  <- " + ", ".join(fails)
        print(f"  [{mark}] {name}{detail}")
    print(f"\n{npass} passed, {real_fail} failed, {nxfail} known-gap of {len(summary)}")
    return 0 if real_fail == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
