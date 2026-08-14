#!/usr/bin/env python3
"""
On-device regression harness for the webOS Plex player (plex-native-poc).

The matrix is split in two: manifest.json holds the installation-independent case definitions,
and the gitignored manifest.local.json (see manifest.local.json.example) maps each case's
symbolic `item` key to a ratingKey on this server and supplies the PMS host, TV address and
test user. load_manifest merges them and fails with the missing key if the overlay is absent.

For each case in manifest.json this driver:
  1. closes the running app on the TV (luna-send closeByAppId + fuser -k, via `make kill`);
  2. establishes the item's resume point server-side -- AFTER the close, so a live
     timeline_thread can't re-scrobble over it. ALWAYS clears it first (/:/unscrobble), then
     seeds `setup.viewOffset_ms` if the case declares one. The offset lives on the SERVER and
     outlives the run, and the app reports progress every 10s while playing, so an unreset case
     inherits whatever the last case -- or the last RUN -- left on that rk, and `resume_ns`
     resumes anything past 10s. One item is shared by five cases and another by three, so
     leaving it implicit turned "play from the start" into "resume from somewhere", varying with
     suite order and run history. Do not make the reset conditional again;
  3. clears every /tmp/plxnative-* trigger on the TV, then writes only the ones this case needs;
  4. runs `make run-stream TV=<tv>`, which relaunches the app and tails
     /tmp/plxnative-events.log live;
  5. filters the `smp_cb type=43 num=0 str=` flood and evaluates the per-op assertions
     CONTINUOUSLY as lines arrive, stopping the case the moment it passes — the manifest's
     run_secs is the cap, not the runtime (see stream_case for why that is sound, and
     --no-early to turn it off);
  6. records PASS/FAIL with the failing evidence line.

Security: the PMS X-Plex-Token is read from src/config.local.h at runtime and is NEVER
printed, logged, or written to any file. The TV ssh creds already live in the committed
Makefile, so we shell out to `make` / sshpass for device I/O.

Usage:
  ./tests/run.py --list                 # list cases and what they cover
  ./tests/run.py --build                # cargo + make + make deploy, then run all cases
  ./tests/run.py --filter marker        # run only cases whose name contains "marker"
  ./tests/run.py                        # run every case (assumes app already deployed)

Exit code is nonzero if any selected case fails.

No third-party deps -- Python 3 stdlib only (macOS system python3 is fine).
"""

import argparse
import json
import os
import re
import signal
import subprocess
import sys
import threading
import time
import urllib.parse
import urllib.request

# ---------------------------------------------------------------------------
# Paths / constants
# ---------------------------------------------------------------------------
TESTS_DIR = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(TESTS_DIR)
MANIFEST = os.path.join(TESTS_DIR, "manifest.json")
MANIFEST_LOCAL = os.path.join(TESTS_DIR, "manifest.local.json")
MANIFEST_LOCAL_EXAMPLE = MANIFEST_LOCAL + ".example"
CONFIG_LOCAL_H = os.path.join(REPO_ROOT, "src", "config.local.h")

# reference list of the dev triggers the app reads (apply_triggers now GLOB-clears /tmp/plxnative-*,
# so this no longer has to be exhaustive — it's kept for humans / grep)
ALL_TRIGGERS = [
    "plxnative-detail", "plxnative-detailplay", "plxnative-detailsec", "plxnative-detailcol",
    "plxnative-autoseek", "plxnative-menupick", "plxnative-menu", "plxnative-noaudio",
    "plxnative-grid", "plxnative-autoplay", "plxnative-h265", "plxnative-playidx", "plxnative-url",
    "plxnative-play", "plxnative-ffprobe", "plxnative-token",
    # UI/FPS scenes (plxnative-profile MUST be cleared — a stale one glFinish-tanks FPS and false-fails)
    "plxnative-detailosc", "plxnative-info", "plxnative-chapters", "plxnative-profile",
    # boot-flow triggers (heroidx pins the hero + bypasses the who's-watching picker; pickuser forces it)
    "plxnative-heroidx", "plxnative-pickuser",
    # itemmenu snaps into the grid and opens the press-and-hold card context menu (route=itemmenu)
    "plxnative-itemmenu",
]

# the type=43 spam filter (mirrors: grep -vaE "smp_cb type=43 num=0 str=$")
TYPE43_SPAM = re.compile(r"smp_cb type=43 num=0 str=\s*$")


# ---------------------------------------------------------------------------
# Token (never printed)
# ---------------------------------------------------------------------------
# Manifest + local overlay
#
# manifest.json is installation-INDEPENDENT: it holds the case definitions (names, operations,
# assertions, run_secs, triggers), and every field that would name a particular server, TV or
# library item lives in the gitignored manifest.local.json instead. A case says which SHAPE of
# item it needs (`item`, a symbolic key like `movie_h264_ac3_1080p`); the overlay's `items` map
# turns that into the ratingKey on this machine's server. Resolution happens once, here, and
# writes the concrete value back as `rk`, so every consumer below (the play trigger, the
# unscrobble/seed, the fps scenes' "$rk") is unchanged and still reads case["rk"].
#
# The symbolic key is not just anonymisation: it is what keeps "five cases share one item"
# visible in the tracked file, which is the fact run_case's reset comment depends on.
# ---------------------------------------------------------------------------
def _die_no_overlay(extra=""):
    sys.exit(f"missing test configuration: {MANIFEST_LOCAL}\n"
             f"  This file is gitignored — it maps the manifest's symbolic item keys to the\n"
             f"  ratingKeys on YOUR server, and carries the PMS host, TV address and test user.\n"
             f"  Create it with:  cp {MANIFEST_LOCAL_EXAMPLE} {MANIFEST_LOCAL}\n"
             f"  then replace every <placeholder> in it.{extra}")


def _resolve_items(entries, items, what):
    """Turn each entry's symbolic `item` key into the concrete `rk` the rest of the runner uses."""
    for e in entries:
        key = e.get("item")
        if key is None:
            continue                      # an fps scene that needs no library item
        if key not in items:
            _die_no_overlay(f"\n  (no `items` entry for {key!r}, needed by {what} {e['name']!r})")
        e["rk"] = str(items[key])


def load_manifest():
    with open(MANIFEST) as f:
        manifest = json.load(f)
    try:
        with open(MANIFEST_LOCAL) as f:
            local = json.load(f)
    except FileNotFoundError:
        _die_no_overlay()
    except ValueError as e:
        sys.exit(f"{MANIFEST_LOCAL} is not valid JSON: {e}")

    for field in ("pms", "tv"):
        if field not in local:
            _die_no_overlay(f"\n  (no {field!r} block)")
        manifest[field] = local[field]
    # test_user is optional by design: leaving it out runs as the owner (with a warning).
    if "test_user" in local:
        manifest["test_user"] = local["test_user"]

    items = local.get("items", {})
    _resolve_items(manifest["cases"], items, "case")
    _resolve_items(manifest.get("fps_scenes", []), items, "fps scene")
    # The two places an item key appears NESTED rather than as a case's own `item`: the successor
    # a credits marker auto-advances into is both reset server-side and asserted on by ratingKey.
    def rk_of(key, case_name, field):
        if key not in items:
            _die_no_overlay(f"\n  (no `items` entry for {key!r}, needed by case "
                            f"{case_name!r} {field})")
        return str(items[key])

    for c in manifest["cases"]:
        setup = c.get("setup", {})
        if "also_reset" in setup:
            setup["also_reset"] = [rk_of(k, c["name"], "setup.also_reset")
                                   for k in setup["also_reset"]]
        for op in c.get("operations", []):
            if op.get("expect_up_next"):
                op["expect_up_next"] = rk_of(op["expect_up_next"], c["name"], "expect_up_next")

    # A copied-but-unedited template fails LOUDLY here rather than as a 404 from plex.tv or as a
    # case that never plays: every value in the example is bracketed, and none is ever legitimate.
    stray = [f"{k}={v}" for k, v in
             [("pms.host", manifest["pms"].get("host")), ("tv", manifest["tv"])]
             + [(f"items.{k}", v) for k, v in items.items()]
             + ([("test_user.id", manifest["test_user"].get("id"))] if "test_user" in manifest else [])
             if isinstance(v, str) and v.startswith("<")]
    if stray:
        sys.exit(f"{MANIFEST_LOCAL} still holds template placeholders: {', '.join(stray)}\n"
                 f"  Replace each with the value for your own server / TV / library.")
    return manifest


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


def pms_unscrobble(host, port, rk, token):
    """Clear an item's resume point (and watched flag) via /:/unscrobble. Token never printed.

    This is the ONLY thing that actually resets a viewOffset. `PUT /:/progress?time=0` looks like
    it should and returns 200, but PMS ignores it and the old offset survives -- verified against
    the live server (so does time=1). Do not "simplify" this back into a progress PUT.
    """
    q = urllib.parse.urlencode({
        "key": rk,
        "identifier": "com.plexapp.plugins.library",
        "X-Plex-Token": token,
    })
    url = f"http://{host}:{port}/:/unscrobble?{q}"
    redacted = url.replace(token, "<token>")
    try:
        with urllib.request.urlopen(urllib.request.Request(url), timeout=15) as resp:
            print(f"    progress reset: {redacted} -> {resp.status}")
            return True
    except Exception as e:
        print(f"    WARN: unscrobble failed ({redacted}): {e}")
        return False


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
        if kind == "seek" and op.get("mode") == "rapid":
            # seek SCRIPT: comma-separated steps fired one per ~300ms — absolute seconds or
            # tap-relative +N/-N (vs the last requested target). Exercises seek coalescing.
            files.append(("plxnative-autoseek", op["script"]))
        elif kind == "seek":
            files.append(("plxnative-autoseek", None))          # touch -> one seek to 140s
        elif kind == "skip":
            files.append(("plxnative-marker", op["marker"]))
        elif kind == "marker":
            # jump to 5s before the named server marker, so the skip/Up Next control row is
            # reachable in seconds instead of 50 minutes into an episode
            files.append(("plxnative-marker", op["marker"]))
        elif kind == "audio_switch":
            files.append(("plxnative-menupick", f'{op["tab"]},{op["row"]}'))
        elif kind == "subtitle":
            files.append(("plxnative-menupick", f'{op["tab"]},{op["row"]}'))
        # "play" and "resume" need no extra trigger (resume rides the seeded viewOffset).
    return files


def key_inject_for_case(case):
    """(log-pattern, remote-token) for a case that presses a key MID-RUN, or None.

    The app mkfifos /tmp/plxnative-remote and drains it every frame, so a token written while it
    runs replays through the real key handler. Keying the write to a LOG LINE rather than to a
    wall-clock delay is what makes it deterministic: the press lands the moment the control is
    actually on screen, however long the resolve took.
    """
    for op in case["operations"]:
        if op["op"] == "skip":
            return (f"marker offer: {op['marker'].capitalize()}", op.get("press", "ok"))
    return None


def apply_triggers(tv, files, extra=None):
    """Clear every /tmp/plxnative-* trigger (sparing the *.log files), then create the ones this case
    needs, in one ssh round-trip. GLOB-based, not an enumerated list, so a newly-added app trigger can
    never bleed between scenes — a stale plxnative-novsync would uncap vsync and false-PASS an FPS
    scene, a stale plxnative-press/-login would derail a home scene. ALL_TRIGGERS above is now just a
    human reference of the known triggers.

    `extra` is a raw shell command appended to the same round-trip, for a trigger whose VALUE must
    not reach stdout (the PMS token) and so cannot go through the printed `files` list.
    """
    # wipe every trigger, keeping only the append-only logs (events/stderr/crash)
    parts = ['for f in /tmp/plxnative-*; do case "$f" in *.log) ;; *) rm -f "$f";; esac; done']
    for name, content in files:
        if content is None:
            parts.append(f"touch /tmp/{name}")
        else:
            # single-quote the content; rks / "0,6" / "mkv" never contain quotes
            parts.append(f"printf '%s' '{content}' > /tmp/{name}")
    if extra:
        parts.append(extra)
    ssh(tv, "; ".join(parts))


# ---------------------------------------------------------------------------
# Log parsing
# ---------------------------------------------------------------------------
def filter_log(raw):
    """Drop the type=43 flood; return the surviving lines as a list."""
    return [ln for ln in raw.splitlines() if not TYPE43_SPAM.search(ln)]


RE_DECISION = re.compile(r"decision: part=.* -> (DIRECT PLAY|TRANSCODE)")
# Grades the codec NAME, not the raw AV_CODEC_ID: that enum renumbers between FFmpeg
# majors (H264 is 28 / 27, HEVC 174 / 173 / 172 across n3.3 / 6 / 9), so asserting the
# number quietly made this suite a test of which FFmpeg the app happened to be using.
RE_CODEC = re.compile(r"ff: v=#0 codec=(\S+) codec_id=(\d+)\s+(\d+)x(\d+)")
RE_TIMELINE = re.compile(r"timeline playing t=(\d+)s/")
# the 1Hz media position carried on the render heartbeat (app.rs). Anchored to loop= so it can
# never pick up a `pos=` that some other line grows later. loop= and NOT fps=, because `pos=` sits
# between them on the line and because a paused-but-alive player still emits loop=.
RE_POS = re.compile(r"loop=\d+ .*?\bpos=(\d+)s")
# requests a single applied seek MERGED (pump.rs take_coalesced). Present on BOTH paths that
# apply a coalesced target — the normal `seek(in-place)` and the stuck-watchdog `retry reopen` —
# which is exactly why op_seek_rapid keys on it instead of on either line. See that docstring.
RE_COALESCED = re.compile(r"\bcoalesced=(\d+)")
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
    """All (codec_name, width, height) from `ff: v=#0` lines, in order."""
    out = []
    for ln in lines:
        m = RE_CODEC.search(ln)
        if m:
            out.append((m.group(1), int(m.group(3)), int(m.group(4)), ln))
    return out


def timeline_secs(lines):
    """All `timeline playing t=<S>s` values in order."""
    return [(int(m.group(1)), ln) for ln in lines for m in [RE_TIMELINE.search(ln)] if m]


def playpos_secs(lines):
    """All `pos=<S>s` values from the once/sec render heartbeat, in order.

    Same SHARED.playpos_ns the /:/timeline reporter posts, but sampled at 1 Hz instead of
    every 10s (app.rs). Density is the whole point: seeing a 15s climb through 10s samples
    needs ~30s of playback, so the sparse signal charged every case roughly double its real
    floor. The app only emits the field while genuinely presenting frames, which is what
    keeps a direct-play resume's pre-roll 0 out of the series (see player::is_playing).
    """
    return [(int(m.group(1)), ln) for ln in lines for m in [RE_POS.search(ln)] if m]


def progress_secs(lines):
    """The densest available media-position series: the 1Hz heartbeat, else the 10s timeline.

    The fallback keeps every position assertion working against a log with no `pos=` field —
    an older binary, or one built before the heartbeat carried it.
    """
    return playpos_secs(lines) or timeline_secs(lines)


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


def a_codec(lines, expected, min_width):
    cs = codec_ids(lines)
    if not cs:
        return False, "no `ff: v=#0 codec=` line found"
    name, w, h, ln = cs[0]
    ok = (name == expected) and (w >= min_width)
    return ok, f"codec={name} {w}x{h} (want {expected} w>={min_width}) :: {ln.strip()}"


def a_no_error(lines):
    for ln in lines:
        if "smp_cb type=18" in ln or "Playing error" in ln:
            return False, f"error surfaced :: {ln.strip()}"
    return True, "no `smp_cb type=18` / `Playing error`"


def a_video_bound(lines):
    ln = find(lines, "setMediaVideoData sent")
    return (ln is not None), (ln.strip() if ln else "no `setMediaVideoData sent` (video plane never bound)")


def a_timeline_climb(lines, min_climb):
    """Media position advanced by >= min_climb seconds. Read from the densest signal available.

    This is the floor on every case and it is a real one: playback is 1x realtime, so a 15s
    climb can never cost less than 15s of wall clock. What the dense signal removes is only
    the SAMPLING tax on top of it.
    """
    dense = playpos_secs(lines)  # scanned once — evaluate() now runs twice a second
    ts = dense or timeline_secs(lines)
    if len(ts) < 2:
        return False, f"only {len(ts)} media-position sample(s); need >=2 that climb"
    lo = min(t for t, _ in ts)
    hi = max(t for t, _ in ts)
    ok = (hi - lo) >= min_climb
    src = "heartbeat pos=" if dense else "timeline t="
    return ok, f"{src} {lo}s..{hi}s (climb {hi-lo}s, need >={min_climb}s) over {len(ts)} samples"


def a_timeline_post(lines):
    """At least one /:/timeline progress report reached PMS.

    a_timeline_climb reads the 1Hz heartbeat now, so this is what keeps the SERVER-side
    reporting path (threads::timeline_thread -> route::report_timeline -> viewOffset and
    watched state) covered. Without it, switching the climb assertion to the denser local
    signal would have quietly dropped that coverage.
    """
    ts = timeline_secs(lines)
    if not ts:
        return False, "no `timeline playing t=` line — progress never reported to PMS"
    return True, f"{len(ts)} timeline report(s) to PMS, last t={ts[-1][0]}s"


# ---- per-op assertions ----
def _reached_target(lines, target_s):
    """Shared seek-op tail: the max reported media second, or an error string if it
    never climbed to ~target (the -6s tolerance covers keyframe snap + report cadence).

    Reads the dense heartbeat when present: confirming a seek landed used to wait up to a
    full 10s timeline cadence. max() over the series is indifferent to the extra samples.
    """
    ts = progress_secs(lines)
    reached = max((t for t, _ in ts), default=-1)
    if reached < target_s - 6:
        return reached, f"timeline reached only {reached}s, expected >= ~{target_s}s after seek"
    return reached, None


def op_seek_inplace(lines, target_s):
    started = find(lines, "seek(in-place)")
    if started is None:
        return False, "no `seek(in-place)` line (in-place seek did not fire)"
    seg_ok = any("sendSegment=1" in ln for ln in lines if "in-place seek:" in ln)
    if not seg_ok:
        ln = find(lines, "in-place seek:")
        return False, f"in-place seek lacked sendSegment=1 :: {ln.strip() if ln else 'no in-place seek: line'}"
    if find(lines, "reload_at: fresh Load"):
        return False, "in-place seek fell back to a reload (`reload_at: fresh Load` present)"
    reached, err = _reached_target(lines, target_s)
    if err:
        return False, err
    return True, f"in-place seek OK; reached {reached}s :: {started.strip()}"


def op_seek_rapid(lines, final_s):
    """Rapid tap-burst seek: request_seek()s land while an earlier seek is still resolving, so
    the pump COALESCES — it keeps only the newest target and applies it once the in-flight seek
    anchors. Assert the merge actually happened, that no seek escalated out of in-place, that
    playback re-anchored near the final target and kept ADVANCING, and that the audio lane
    resumed (the historical rapid-back-tap bug was 10+ s of post-burst silence).

    DO NOT go back to counting `seek(in-place)` lines. That is what this assertion used to do
    (">= 2 fired, so coalescing was exercised") and it measured PMS latency, not merging:

      * a tap that arrives mid-seek is applied by the next pump tick after the in-flight seek
        anchors — which logs `seek(in-place)`;
      * unless the anchor takes longer than SEEK_STUCK_MS (1200ms), in which case the
        stuck-watchdog applies it instead — it swaps the coalesced target out of TX.seek_to_ns
        (pump.rs) and logs `in-place stuck → retry reopen`, so the second `seek(in-place)`
        never comes.

    Both are the coalescer working. Which one runs is decided by whether a reopen+av_seek+first
    keyframe beats 1200ms, i.e. by the network — so the seek count swung 1..5 run to run on
    identical code, and every value below 2 was scored as a failure. That was the wandering seek
    tier. The pump now reports `coalesced=<n>` on both paths (n = requests this seek merged),
    which states the invariant directly and cannot be raced.
    """
    taps = [ln for ln in lines if "autoseek: step" in ln]
    if not taps:
        return False, "no `autoseek: step` lines — the seek script never ran (nothing was tested)"
    # Every line either path emits for an applied seek, in order; the burst's tail starts at the
    # last one, since a watchdog retry can be the final seek of the burst.
    idxs = [i for i, ln in enumerate(lines) if "coalesced=" in ln]
    if not idxs:
        return False, f"{len(taps)} taps fired but the pump applied no seek (no `coalesced=` line)"
    merged = sum(int(m.group(1)) for ln in lines for m in [RE_COALESCED.search(ln)] if m)
    if merged < 1:
        return False, (
            f"{len(taps)} taps produced {len(idxs)} seeks, none of which merged a request "
            f"(all `coalesced=0`) — the burst outran nothing, so coalescing went untested. "
            f"Tighten `gap=` in the case's seek script."
        )
    if find(lines, "reload_at: fresh Load"):
        return False, "burst escalated to a reload (`reload_at: fresh Load` — stuck-watchdog gave up on in-place)"
    seg = sum(1 for ln in lines if "in-place seek:" in ln and "sendSegment=1" in ln)
    if seg < 1:
        return False, "no seek re-anchored the segment (`in-place seek: … sendSegment=1` absent)"
    tail = lines[idxs[-1]:]
    ts = progress_secs(tail)
    if len(ts) < 2:
        return False, f"only {len(ts)} position report(s) after the last seek; playback did not demonstrably resume"
    lo = min(t for t, _ in ts)
    hi = max(t for t, _ in ts)
    if hi < final_s - 8:
        return False, f"post-burst position peaked at {hi}s, expected ~{final_s}s (seek landed wrong / stalled)"
    if hi - lo < 8:
        return False, f"post-burst position {lo}s..{hi}s climbed only {hi - lo}s (need >=8s of real playback)"
    if not any("feed a#" in ln for ln in tail):
        return False, "no `feed a#` after the last seek — audio lane never resumed (silent playback)"
    return True, (
        f"{len(taps)} taps → {len(idxs)} pump seeks, {merged} request(s) coalesced away "
        f"(sendSegment ok on {seg}); post-burst position {lo}s..{hi}s; audio alive"
    )


def op_seek_transcode(lines, target_s):
    # transcode seeks now RELOAD the pipeline (reload_transcode: fresh Load = correct GStreamer
    # segment, no stale-segment artifacts). Older builds flushed+refed (`seek(transcode)`) or fell
    # back to reload_at.
    hit = find(lines, "reload_transcode: fresh Load at offset") or find(lines, "seek(transcode)") \
        or find(lines, "reload_at: fresh Load at %ds" % target_s)
    if hit is None:
        return False, "no transcode-seek signal (reload_transcode / seek(transcode) / reload_at) present"
    reached, err = _reached_target(lines, target_s)
    if err:
        return False, err
    return True, f"transcode/reload seek OK; reached {reached}s :: {hit.strip()}"


def op_audio_native(lines):
    hit = find(lines, "audio switch (native)")
    if hit is None:
        return False, "no `audio switch (native)` line (switch was not native)"
    cs = codec_ids(lines)
    if not cs:
        return False, "no ff codec line after switch"
    last = cs[-1][0]
    if last != "hevc":
        return False, f"codec after native switch = {last}, expected hevc (should not change) :: {cs[-1][3].strip()}"
    return True, f"native switch OK; codec stayed hevc :: {hit.strip()}"


def op_audio_transcode(lines):
    re_t = find(lines, "re-transcode:")
    rl_t = find(lines, "reload_transcode:")
    if re_t is None or rl_t is None:
        return False, f"missing transcode-switch logs (re-transcode={bool(re_t)} reload_transcode={bool(rl_t)})"
    cs = codec_ids(lines)
    # The audio-forced re-transcode must not cost the VIDEO anything: since the transcode
    # target became a chain (hevc,h264 — issue #22, 2026-08-11), the server direct-streams
    # (copies) an h264/hevc source and re-encodes only the audio, so the codec after the
    # switch is the codec before it. The old expectation here was the inverse — video
    # re-encoded to hevc, the only member of the then single-entry target — and this
    # assertion's own NB predicted today's flip. A copy-incapable path (source over the
    # profile caps) would legitimately re-encode, but this case's item is fixed h264 1080p.
    if len(cs) >= 2 and cs[-1][0] != cs[0][0]:
        return False, (f"video was RE-ENCODED across the audio switch ({cs[0][0]} -> {cs[-1][0]}); "
                       f"expected a copy :: {cs[-1][3].strip()}")
    return True, f"transcode switch OK (video copied, {cs[-1][0] if cs else '?'}) :: {rl_t.strip()}"


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
# Streaming run — grade the event log AS IT ARRIVES, stop as soon as a case passes.
#
# The old path slept a fixed run_secs (60/70/75/90 per case — 1190s, ~20 min of pure
# sleep across the 18 cases) and only graded afterwards. Almost every case satisfies its
# assertions well before that. What a case can NOT beat is `min_timeline_climb_s` seconds
# of real playback, because playback is 1x realtime — that is the floor, and it is a
# coverage decision, not waste. So run_secs stops being the runtime and becomes the CAP:
# a FAILING case still costs exactly what it costs today; a passing one costs what it needs.
#
# Why grading a prefix is sound: almost every assertion is monotone once satisfied — it looks
# for a line that has appeared, or for a max/climb that only grows. The exceptions are the two
# ABSENCE checks, which start out true and can only flip the other way:
#   * a_no_error            — `smp_cb type=18` / `Playing error`
#   * op_seek_rapid         — `reload_at: fresh Load` (stuck-watchdog gave up on in-place)
# So early exit can never turn a FAIL into a PASS on the evidence *seen*; what it can do is
# stop before evidence that would have failed the case arrives. SETTLE_S keeps watching for a
# moment after the last assertion flips, and --no-early restores the full fixed window.
# For the rapid-seek cases the structure already covers most of that risk: op_seek_rapid wants
# >=8s of position CLIMB after the last seek, so that much post-burst playback has to elapse
# before the case can pass at all — well past when the watchdog would have escalated.
# (Note the old window was itself arbitrary — an error at 70s of a 60s case was never caught.)
SETTLE_S = 2.0
EVAL_EVERY_S = 0.5
# How long the app gets to produce its FIRST log line before we give up and grade an empty log.
# Generous on purpose: boot is ~5-10s, and a TV that is merely slow should fail on its
# assertions, not on a harness timeout that looks identical to a total regression.
BOOT_GRACE_S = 45.0


def failed_for_good(case, lines):
    """The reason a case can no longer pass however long it runs, or None if it still might.

    Returned as a STRING, not a bool, because stopping early can change which failure the case
    reports: assertions are written to name the FIRST thing missing, and cutting a case short can
    leave an earlier check unsatisfied for no reason but the clock, so the reported evidence would
    point at a symptom of the early exit rather than at the real disqualifier. Surfacing the
    settled reason next to the verdict keeps the true cause instead of throwing it away.

    Only the two ABSENCE checks can conclude this, and it is the mirror image of the rule that
    limits early PASS: once the disqualifying line exists it never un-exists, so the rest of the
    cap is pure waste. Every OTHER failure means "the line has not appeared YET" — exactly what
    more time could still fix — so those must run to the cap. This matters because a red suite is
    the one you iterate against: seek_rapid_hevc_4k burns its full 75s to reach a verdict
    that is already settled ~25s in.
    """
    if case["expect"].get("no_playing_error", True):
        ok, evidence = a_no_error(lines)
        if not ok:
            return evidence
    # `reload_at: fresh Load` disqualifies only a RAPID seek (op_seek_rapid wants everything to
    # stay in-place). op_seek_transcode accepts that very line as a valid signal — so scope it.
    rapid = any(o["op"] == "seek" and o.get("mode") == "rapid" for o in case.get("operations", []))
    if rapid and find(lines, "reload_at: fresh Load"):
        return "burst escalated to a reload (`reload_at: fresh Load`) — verdict cannot change"
    return None


def _drain(stream, sink, done):
    """Reader thread: filter the type=43 flood at the door so the grader never re-scans it."""
    try:
        for raw in stream:
            ln = raw.rstrip("\n")
            if not TYPE43_SPAM.search(ln):
                sink.append(ln)
    finally:
        done.set()


# The remote command `make run-stream` ends in — unique enough to identify our own ssh clients.
RUN_STREAM_MARK = "tail -F -n +1 /tmp/plxnative-events.log"


def _run_stream_pids():
    """PIDs of every local ssh/sshpass process carrying a run-stream tail."""
    out = subprocess.run(["ps", "-Ao", "pid,command"], capture_output=True, text=True).stdout
    pids = set()
    for ln in out.splitlines():
        if RUN_STREAM_MARK in ln and " ps -Ao" not in ln:
            try:
                pids.add(int(ln.split(None, 1)[0]))
            except ValueError:
                pass
    return pids


def teardown(tv):
    """Leave the TV as we found it. Runs on EVERY exit — pass, fail, Ctrl-C, SIGTERM, crash.

    Three things outlive the harness otherwise, and none of them are cosmetic:

    * THE APP KEEPS RUNNING. Nothing closes it at the end — `make kill` only runs at the START
      of the next case, so the last case's playback just carries on. It keeps a PMS session
      open, and its timeline reporter keeps posting progress every 10s, so the next run
      inherits a resume point on that rk — the exact contamination ee07506 removed between
      cases, reintroduced at the seam between runs. It is also what "I see FPS tests running"
      looks like from the outside, long after the suite printed its summary.
    * THE INJECTED TOKEN STAYS in /tmp/plxnative-token: a real per-server PMS access token,
      world-readable, on a device with a rooted sshd and a committed password. The normal path
      did clear it; every abnormal one left it.
    * ssh CLIENTS. Per-case reaping covers a case that ends normally, but not the harness dying
      between cases.

    Never raises: a teardown that throws on an unreachable TV would mask the real failure (and
    on Ctrl-C would replace the user's interrupt with a traceback about ssh).
    """
    try:
        make(["kill", f"TV={tv}"], timeout=40)
    except Exception:
        pass
    try:
        apply_triggers(tv, [])
    except Exception:
        pass
    for pid in _run_stream_pids():
        try:
            os.kill(pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass


# The TV to clean on exit, or None while the harness has not driven one yet. Armed at the point we
# commit to driving the TV — NOT at startup — so `--list`, a bad `--filter` and a missing token all
# exit without closing an app the user may be watching.
_TEARDOWN_TV = None


def arm_teardown(tv):
    global _TEARDOWN_TV
    _TEARDOWN_TV = tv


def stream_case(case, cfg, cap_s, early=True, inject=None):
    """Launch via `make run-stream` and grade the log as it streams.

    Returns (lines, elapsed_s, stopped_early, settled_reason). Lines come back already filtered;
    settled_reason is non-None only when the case was cut short by an already-decided failure.
    """
    # Snapshot BEFORE launching so the teardown can kill exactly the clients this case started.
    # killpg is not sufficient on its own: sshpass forks ssh into its OWN process group (measured:
    # make pgid=P, sshpass pid=A pgid=P, ssh pid=B pgid=B ppid=A), so the group signal never
    # reaches B, and B reparents to init holding an ssh connection and a remote `tail -F`. These
    # accumulate one per case — 125 of them piled up against the TV in a single session before
    # this was noticed, which is real load on a device whose dropbear has a connection limit.
    pre_pids = _run_stream_pids()
    proc = subprocess.Popen(["make", "-s", "run-stream", f"TV={cfg['tv']}"],
                            cwd=REPO_ROOT, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                            text=True, bufsize=1,
                            # own process group: terminating `make` alone would orphan the
                            # sshpass/ssh child and leave the remote tail (and the app) attached.
                            start_new_session=True)
    lines, done = [], threading.Event()
    threading.Thread(target=_drain, args=(proc.stdout, lines, done), daemon=True).start()

    injected = inject is None
    started = time.monotonic()
    # cap_s is APP runtime, so the clock starts at the app's first log line — not here. Anchoring
    # it at ssh-start instead would silently shorten every case by the close+launch overhead
    # (BOOT_SH's own `sleep 2` plus connect), which `make run RUN_SECS=` spent BEFORE it started
    # counting. That is a couple of seconds off exactly the cases that run to the cap.
    deadline = None
    passed_since = None
    stopped_early = False
    settled = None
    try:
        while True:
            time.sleep(EVAL_EVERY_S)
            ended = done.is_set()  # sample BEFORE grading, so we grade what arrived last
            now = time.monotonic()
            if deadline is None:
                if lines:
                    deadline = now + cap_s
                elif now - started >= BOOT_GRACE_S:
                    break  # app never wrote a line — grade the empty log, same as a dead TV
            elif now >= deadline:
                break
            if not injected and any(inject[0] in l for l in lines):
                injected = True
                # the FIFO has a live reader (the app drains it per frame), so this returns at once
                ssh(cfg["tv"], f"printf '{inject[1]}\\n' > /tmp/plxnative-remote", timeout=15)
                print(f"    injected key '{inject[1]}' on '{inject[0]}'")
            if early:
                snap = list(lines)
                ok, _ = evaluate(case, snap)
                if not ok:
                    passed_since = None
                    settled = failed_for_good(case, snap)
                    if settled:
                        stopped_early = True
                        break  # verdict is settled — don't burn the rest of the cap
                elif passed_since is None:
                    passed_since = now
                elif now - passed_since >= SETTLE_S:
                    stopped_early = True
                    break
            if ended:
                break  # ssh hung up (TV asleep / connection lost) — grade what we got
    finally:
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
        except (ProcessLookupError, PermissionError):
            pass
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        # Reap the ssh clients killpg could not reach (see pre_pids). SIGKILL, not SIGTERM —
        # sshpass/ssh demonstrably survive the TERM the group already sent them. Only pids that
        # appeared while this case ran are touched, so a stray unrelated session is left alone.
        for pid in _run_stream_pids() - pre_pids:
            try:
                os.kill(pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
    return list(lines), time.monotonic() - started, stopped_early, settled


# ---------------------------------------------------------------------------
# Case execution
# ---------------------------------------------------------------------------
def op_marker_offer(lines, kind):
    """The control row OFFERED the named segment.

    Proves the whole server-marker path against the live server: `?includeMarkers=1` came back,
    `convert_markers` kept the segment, the playhead landed inside it, and `player_hud::slot_for`
    handed the row to a stand-in. The host suite covers the precedence in isolation; only the
    device can show that real `Marker[]` data drives it.
    """
    want = f"marker offer: {kind}"
    hit = [l for l in lines if want in l]
    if not hit:
        seen = [l.strip() for l in lines if "marker" in l.lower()][-3:]
        return False, f"no '{want}' line; nearest marker lines: {seen or 'none'}"
    return True, hit[-1].strip()


def op_skip_marker(lines, kind, min_pos_after, min_climb_after):
    """The skip was offered, PRESSING it landed, and playback carried on past the segment.

    This is the press path end to end on the real device: the marker came off the wire, the control
    row offered it, a key written to the remote FIFO replayed through the real handler, the seek
    executed, and the playhead kept climbing past the segment afterwards.

    What it does NOT pin is the consumed-marker latch (`metadata::mark_skipped`), and that is worth
    stating because the obvious reading of "offered exactly once" suggests otherwise. Verified by
    negative control: with `mark_skipped` commented out this case still PASSES. Two reasons —
    `app.rs`'s `last_offer` is sticky, so a re-offered segment never logs a second line for the
    count to catch; and on this episode `av_seek_frame`'s AVSEEK_FLAG_BACKWARD keyframe happens to
    land PAST the marker, so the regression does not even reproduce here. Pinning it needs a signal
    for "the row is offering again" plus content whose keyframe lands inside the segment.
    """
    offers = [l for l in lines if f"marker offer: {kind}" in l]
    if not offers:
        return False, f"no 'marker offer: {kind}' line — the control row never offered the segment"
    if len(offers) > 1:
        return False, f"segment offered {len(offers)}x — it came back after the skip: {offers[-1].strip()}"
    ts = [t for t, _ in playpos_secs(lines) if t >= min_pos_after]
    if not ts:
        return False, f"offered once, but the playhead never reached {min_pos_after}s (the skip did not land)"
    climb = max(ts) - min(ts)
    if climb < min_climb_after:
        return False, (f"offered once and skipped to >={min_pos_after}s, but only {climb}s of play "
                       f"after it (need >={min_climb_after}s for the no-recurrence check to mean anything)")
    return True, f"offered 1x, skipped past {min_pos_after}s, {climb}s of play after with no re-offer"


def op_up_next(lines, expect_rk):
    """The credits marker handed off to the queued episode, and that episode actually played.

    Three links in one assertion, because each is worthless without the next: the `continuous=1`
    PlayQueue named a successor, the countdown started it, and the pipeline installed ITS item
    store (i.e. it really began playing, not just resolved).
    """
    queued = [l for l in lines if "playqueue:" in l and "next=" in l and "next=-" not in l]
    if not queued:
        return False, "no playqueue line naming a successor (next=...)"
    started = [l for l in lines if "up next:" in l and f"rk={expect_rk}" in l]
    if not started:
        return False, f"successor queued but never started (want 'up next: ... rk={expect_rk}'): {queued[-1].strip()}"
    playing = [l for l in lines if "playing item:" in l and f"rk={expect_rk}" in l]
    if not playing:
        return False, f"started but its item store never landed: {started[-1].strip()}"
    return True, f"{queued[-1].strip()} | {started[-1].strip()}"


def evaluate(case, lines):
    """Run every assertion for a case. Returns (passed, [(label, ok, evidence)])."""
    exp = case["expect"]
    results = []

    # base assertions (every case)
    results.append(("decision", *a_decision(lines, exp["decision"])))
    results.append(("codec", *a_codec(lines, exp["codec"], exp["min_video_width"])))
    if exp.get("require_video_bound", True):
        results.append(("video_bound", *a_video_bound(lines)))
    results.append(("timeline_climb", *a_timeline_climb(lines, exp.get("min_timeline_climb_s", 12))))
    results.append(("timeline_post", *a_timeline_post(lines)))
    if exp.get("no_playing_error", True):
        results.append(("no_error", *a_no_error(lines)))

    # per-operation assertions
    for op in case["operations"]:
        k = op["op"]
        if k == "seek" and op.get("mode") == "rapid":
            results.append(("seek_rapid", *op_seek_rapid(lines, op["final_s"])))
        elif k == "seek" and op.get("mode") == "inplace":
            results.append(("seek_inplace", *op_seek_inplace(lines, op.get("target_s", 140))))
        elif k == "seek":
            results.append(("seek_transcode", *op_seek_transcode(lines, op.get("target_s", 140))))
        elif k == "skip":
            results.append(("skip_marker", *op_skip_marker(
                lines, op["marker"].capitalize(),
                op.get("min_pos_after_s", 100), op.get("min_climb_after_s", 8))))
        elif k == "marker" and op.get("expect_up_next"):
            results.append(("marker_offer", *op_marker_offer(lines, "Credits")))
            results.append(("up_next", *op_up_next(lines, op["expect_up_next"])))
        elif k == "marker":
            results.append(("marker_offer", *op_marker_offer(lines, op["marker"].capitalize())))
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

    # 1. close the app first (so a live timeline_thread can't overwrite the viewOffset).
    # ONLY needed when this case seeds one: that race is the entire purpose of the close, and
    # `make run-stream` closes the app again anyway right before it relaunches. Skipping the
    # redundant round-trip (it carries its own `sleep 2`) saves ~3s on the 14 cases that seed
    # nothing. Ordering still holds for the ones that do — the previous case's app is killed
    # here, before the seed below.
    # EVERY case seeds, including the ones that mean "from the start" (viewOffset_ms 0). The
    # resume point is SERVER-side and persists across cases and across whole runs: the app's
    # timeline reporter posts progress every 10s while playing, so a case that just played an item
    # for 20s leaves a 20s offset on it, and `metadata::resume_ns` resumes anything past 10s.
    # Left implicit, five different cases share one item and three share another, so "play from the
    # start" silently became "resume from wherever the previous case stopped" — a different code
    # path than the one under test, varying with suite order and with the PREVIOUS run's ending.
    # Seeding unconditionally is what makes a case's starting position a property of the manifest
    # instead of of history.
    setup = case.get("setup", {})
    offset_ms = setup.get("viewOffset_ms", 0)
    # The close is what makes the seed stick: the PREVIOUS case's app is still running, and its
    # timeline_thread would re-scrobble over the value we are about to write.
    make(["kill", f"TV={tv}"], timeout=40)

    # 2. establish the resume point AFTER the close. Reset first, ALWAYS: unscrobble is the only
    # call that actually clears a viewOffset (a time=0 progress PUT returns 200 and changes
    # nothing -- see pms_unscrobble), and resetting even before a seed keeps the starting state a
    # function of the manifest alone rather than of whatever was there before.
    pms_unscrobble(cfg["pms"]["host"], cfg["pms"]["port"], case["rk"], token)
    if offset_ms > 0:
        pms_put_progress(cfg["pms"]["host"], cfg["pms"]["port"], case["rk"], offset_ms, token)
    # A case that AUTO-ADVANCES plays a second item this run, and that item's resume point is
    # server state exactly like the first one's -- left alone, the successor starts wherever a
    # previous run stopped it, which is the same history-dependence `setup.viewOffset_ms` exists
    # to kill. Declared per case rather than inferred, so the manifest still says what it starts from.
    for rk in setup.get("also_reset", []):
        pms_unscrobble(cfg["pms"]["host"], cfg["pms"]["port"], rk, token)

    # 3. clear + set triggers, and inject the effective PMS token in the SAME round-trip.
    # The token rides `extra=` rather than `files` so its value never reaches stdout — the only
    # reason it used to need a round-trip of its own. Ordering is what matters and is preserved:
    # plxnative-token is cleared by the glob wipe that opens the command, and rewritten after it.
    # Always required — the binary carries no baked token, so /tmp/plxnative-token is the only way
    # an automated run gets PMS access.
    files = triggers_for_case(case)
    tok_cmd = f"printf '%s' '{token}' > /tmp/plxnative-token" if cfg.get("inject_token") else None
    apply_triggers(tv, files, extra=tok_cmd)
    shown = ", ".join(n + ("=" + c if c is not None else "") for n, c in files)
    print(f"    triggers: {shown}")
    if tok_cmd:
        print(f"    plxnative-token: <{cfg['user_label']}, redacted>")

    # 4. run + grade the log as it streams (run_secs is the cap, not the runtime)
    early = not cfg.get("no_early")
    print(f"    run-stream (cap {run_secs}s{'' if early else ', early exit off'}) ...")
    lines, elapsed, stopped_early, settled = stream_case(
        case, cfg, run_secs, early=early, inject=key_inject_for_case(case))

    # 5. evaluate
    passed, results = evaluate(case, lines)
    for label, ok, evidence in results:
        mark = "PASS" if ok else "FAIL"
        if ok and not verbose:
            print(f"      [{mark}] {label}")
        else:
            print(f"      [{mark}] {label}: {redact(evidence)}")  # never leak the token
    how = f"{elapsed:.0f}s" + (f" (early, cap {run_secs}s)" if stopped_early else f" (full cap {run_secs}s)")
    print(f"    => {'PASS' if passed else 'FAIL'} in {how}")
    if settled:
        # the true disqualifier — which is NOT always the assertion printed above (see
        # failed_for_good), so losing it would point a reader at a symptom instead of the cause.
        print(f"       stopped early — settled: {redact(settled)}")
    return passed, results, lines


# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------
def do_build(tv):
    # The Makefile owns the whole build (it drives cargo +nightly with the load-bearing
    # cortex-a9 flags itself) — shelling out to it keeps run.py from drifting a second
    # copy of the toolchain invocation (the old hand-rolled zigbuild here did exactly that).
    print("=== BUILD: make -> make deploy ===")
    if make(["all"], timeout=1200, capture=False).returncode != 0:
        sys.exit("make failed")
    if make(["deploy", f"TV={tv}"], timeout=180, capture=False).returncode != 0:
        sys.exit("make deploy failed")
    print("=== BUILD OK ===")


# ---------------------------------------------------------------------------
# FPS regression suite (UI perf gate — separate from the playback cases above).
# The app logs a once/sec `loop=<n> route=<home|detail|player> [overlay=<info|chapters|menu|none>]
# fps=<n>` heartbeat, carrying TWO rates that must never be confused:
#
#   loop=  LOOP ITERATIONS per second. Liveness only. A settled screen still reports ~62 while
#          swapping nothing, so this CANNOT see a frozen animation. Graded by `loop_floor`.
#   fps=   FRAMES actually swapped per second — the real frame rate, moved by `ui::idle`'s present
#          gate. Graded by `fps_floor` (it must keep animating) and `fps_ceiling` (it must stop).
#
# RENAMED 2026-08-01 and the old name was REUSED: what these fields are called today is the reverse
# of what a pre-rename log says. `FPS=` in an old log is this `loop=`; old `pres=` is this `fps=`.
# Both regexes below therefore fail to match an old log outright, which is the intended loud
# failure — a scene must never grade a loop rate as if it were a frame rate.
#
# Each scene sets its plxnative-* triggers, runs the app profiler-OFF, then asserts its gates. This
# is the automated form of the by-hand FPS hunting that found the hero / cast+about / info-panel
# regressions.
# ---------------------------------------------------------------------------
LOOP_RE = re.compile(r"loop=(\d+) route=(\w+)(?: overlay=(\w+))?")

# The desktop simulator (`make sim`) emits the SAME heartbeat, tagged `sim=1` (app.rs's SIM_TAG).
# Its numbers describe a Mac's GPU, driver and compositor; every gate in this file is calibrated to
# the SM9000's Mali. Refusing the sample is the enforcement — a disclaimer in a doc is a comment,
# and the whole point of the tag is that a log gets pasted between agents and into issues.
SIM_RE = re.compile(r"\bsim=1\b")


def reject_simulator(lines):
    """Abort rather than grade a simulator log against device-calibrated gates."""
    if any(SIM_RE.search(ln) for ln in lines):
        raise SystemExit(
            "refusing to grade: this log carries `sim=1`, so it came from the desktop simulator "
            "(make sim), not a television. Its frame rates are about a Mac. Run the scene on the "
            "device — see the tv-session skill."
        )


def parse_loop(lines, route, overlay):
    """The per-second LOOP-ITERATION counts whose route (+overlay, if the scene pins one) match."""
    reject_simulator(lines)
    out = []
    for ln in lines:
        m = LOOP_RE.search(ln)
        if not m or m.group(2) != route:
            continue
        if overlay and (m.group(3) or "none") != overlay:
            continue
        out.append(int(m.group(1)))
    return out


# `fps=<n>` — frames actually SWAPPED in that second, which is what `ui::idle`'s present gate moves.
# Deliberately a SECOND regex rather than a group on LOOP_RE: the field is newer than the heartbeat,
# and a scene graded on a log from a build without it must fail as "no samples" rather than silently
# match zero and read as a spectacular pass.
FPS_RE = re.compile(r"\bloop=\d+ route=(\w+)(?: overlay=(\w+))?.*?\bfps=(\d+)")


def parse_fps(lines, route, overlay):
    """The per-second PRESENTED-FRAME counts whose route (+overlay) match."""
    reject_simulator(lines)
    out = []
    for ln in lines:
        m = FPS_RE.search(ln)
        if not m or m.group(1) != route:
            continue
        if overlay and (m.group(2) or "none") != overlay:
            continue
        out.append(int(m.group(3)))
    return out


def rate_stats(vals):
    s = sorted(vals)
    n = len(s)
    # The gate is the 2nd-lowest sample: it tolerates ONE transient dip (a mid-run texture upload or
    # GC pause) while a *sustained* regression — every sample low — still fails.
    robust_min = s[1] if n >= 2 else (s[0] if s else 0)
    # DRIFT — the one thing sorting destroys, and the only signature a thermal ramp has.
    # `s = sorted(vals)` above throws sample ORDER away, so before this a monotone 60->53 decay and
    # a flat 53 produced byte-identical output: a screen that is simply expensive and a SoC that is
    # heating up were indistinguishable in every run this harness has ever done. `drift` is
    # (last third mean - first third mean); it is REPORTED, never asserted, because a single
    # 18-36s scene is far too short to gate on — see tests/README.md on the thermal hypothesis.
    # A real thermal soak needs one scene held for tens of minutes.
    third = max(1, n // 3)
    head = sum(vals[:third]) / float(third) if n else 0.0
    tail = sum(vals[-third:]) / float(third) if n else 0.0
    return {"n": n, "min": s[0] if s else 0, "median": s[n // 2] if n else 0,
            "robust_min": robust_min, "head": head, "tail": tail, "drift": tail - head}


def run_fps_scene(scene, cfg, token):
    name = scene["name"]
    tv = cfg["tv"]
    route = scene["route"]
    overlay = scene.get("overlay")  # None for home/detail
    loop_floor = scene["loop_floor"]
    warmup = scene.get("warmup_s", 5)
    run_secs = scene.get("run_secs", 18)
    is_player = scene.get("tier", "ui") == "player"
    tag = route + (f"/{overlay}" if overlay else "")
    print(f"\n=== fps:{name}  (route={tag}, loop_floor {loop_floor}/s) ===")

    make(["kill", f"TV={tv}"], timeout=40)
    files = []
    for tname, tval in scene.get("triggers", {}).items():
        if tval is True:
            files.append((tname, None))
        elif tval == "$rk":
            files.append((tname, str(scene["rk"])))
        else:
            files.append((tname, str(tval)))
    # clears every plxnative-* (incl. plxnative-profile) then writes this scene's. Player-tier
    # scenes actually decode video, so they need the test-user token too — appended to the same
    # round-trip via extra= so its value stays off stdout, exactly like the playback cases.
    tok_cmd = (f"printf '%s' '{token}' > /tmp/plxnative-token"
               if is_player and cfg.get("inject_token") else None)
    apply_triggers(tv, files, extra=tok_cmd)
    shown = ", ".join(n + ("=" + c if c is not None else "") for n, c in files)
    print(f"    triggers: {shown or '(none)'}   run {run_secs}s, skip first {warmup} sample(s)")

    try:
        proc = make(["run", f"TV={tv}", f"RUN_SECS={run_secs}"], timeout=run_secs + 90)
    except subprocess.TimeoutExpired:
        print("    [FAIL] make run timed out")
        return False, "make run timed out"
    lines = filter_log(proc.stdout + "\n" + proc.stderr)

    alls = parse_loop(lines, route, overlay)
    samples = alls[warmup:]  # heartbeat is ~1/sec, so drop the first `warmup` matching samples
    st = rate_stats(samples)

    # False-negative guard: too few post-warmup samples means the scene never really reached this
    # screen (app crash, or a detail/play trigger that didn't open — e.g. an rk not in the home
    # catalog). That is a FAIL, never a vacuous pass.
    if st["n"] < 5:
        msg = (f"only {st['n']} post-warmup loop= samples for route={tag} (need >= 5) — scene never "
               f"entered this screen? ({len(alls)} total matched before warmup)")
        print(f"    [FAIL] {msg}")
        return False, msg

    ok = st["robust_min"] >= loop_floor
    detail = (f"robust_min={st['robust_min']} loop/s (min={st['min']}, median={st['median']}, "
              f"n={st['n']}) vs loop_floor {loop_floor}")
    # Reported, not asserted — a decaying series is the thermal signature, and a scene this short
    # cannot gate it. A drift more negative than a couple per second is worth a real soak.
    detail += f" | drift={st['drift']:+.1f} ({st['head']:.0f}->{st['tail']:.0f})"

    # `fps_floor`: for a scene whose whole point is that an ANIMATION keeps running. Graded on
    # `fps=` because `loop=` counts loop iterations and cannot see a frozen animator — the trap that
    # let a frozen route dip and a stopped spinner both ship.
    #
    # Graded on the MEDIAN, deliberately NOT on the 2nd-lowest the way `loop_floor` and
    # `fps_ceiling` are. Under the present gate a frame rate is intermittent BY DESIGN — that is the
    # whole feature — so on a scene that bounces rather than animates continuously (the `*-nav` pair
    # reverse every 1400ms, of which only ~210ms is fading) a 1-second heartbeat window can land
    # entirely inside the settled gap and legitimately read 0. Measured: home-detail-nav ran min=0,
    # median=15 with a perfectly healthy fade, and even the oscillator scenes are bursty now that
    # the settle predicate lets them idle between steps (home-grid min=11, median=41).
    # The median answers the question actually being asked — "is this screen animating at all, at
    # rate" — while a FROZEN animator reads ~0.5/s, the keepalive alone, which no threshold in this
    # range can confuse with a healthy one. Fill-rate regressions are `loop_floor`'s job, not this.
    f_floor = scene.get("fps_floor")
    if f_floor is not None:
        presf = parse_fps(lines, route, overlay)[warmup:]
        if len(presf) < 5:
            msg = (f"only {len(presf)} post-warmup fps= samples for route={tag} (need >= 5) — a "
                   f"build without the fps= field, or the scene never reached this screen")
            print(f"    [FAIL] {msg}")
            return False, msg
        sf = sorted(presf)
        med = sf[len(sf) // 2]
        ok = ok and med >= f_floor
        detail += (f" | median={med} fps (min={sf[0]}, max={sf[-1]}, n={len(presf)}) "
                   f"vs fps_floor {f_floor}")

    # `fps_ceiling`: the INVERSE assertion — this scene wants the app to stop presenting.
    # `loop_floor` still guards loop liveness so a wedged loop cannot pass by presenting nothing;
    # this adds the ceiling on `fps=` (actual swaps). Graded on the 2nd-HIGHEST sample, the mirror
    # of robust_min's 2nd-lowest, so one late poster landing waking the gate is tolerated while a
    # sustained repaint still fails.
    ceiling = scene.get("fps_ceiling")
    if ceiling is not None:
        pres = parse_fps(lines, route, overlay)[warmup:]
        if len(pres) < 5:
            msg = (f"only {len(pres)} post-warmup fps= samples for route={tag} (need >= 5) — a "
                   f"build without the fps= field, or the scene never reached this screen")
            print(f"    [FAIL] {msg}")
            return False, msg
        sp = sorted(pres, reverse=True)
        robust_max = sp[1]
        ok_c = robust_max <= ceiling
        detail += (f" | robust_max={robust_max} fps (max={sp[0]}, median={sorted(pres)[len(pres)//2]}, "
                   f"n={len(pres)}) vs fps_ceiling {ceiling}")
        ok = ok and ok_c

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
    return 0 if nfail == 0 else 1  # the TV is cleaned by main()'s teardown, on every exit path


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------
# ---- case suites -----------------------------------------------------------------------
# The playback cases are two suites, DERIVED from each case's own shape rather than tagged (a
# field drifts the moment someone adds a case; `operations` cannot):
#   codec — operations is just [play]. Asserts the DECISION (direct-play vs remux vs transcode)
#           and the Load payload's codecs. Never drives the transport, so it cannot see a seek,
#           teardown or threading regression. Run it when route.rs or plex/ changes.
#   logic — also seeks / resumes / switches audio / renders subtitles: the engine, pump and
#           demuxer. Spans the codec matrix on its own (movie_h264_ac3_1080p = H264 direct-play,
#           episode_hevc_4k_hdr10_eac3 + movie_hevc_4k_pgs_subs = 4K HEVC, movie_av1_no_dp_audio
#           + movie_h264_ac3_many_audio = transcode), which is what makes it a safe default for
#           player work. It drops decision BREADTH only:
#           Dolby Vision, MP4/sidecar, AAC, smart-DP's TrueHD-default -> AC3-sibling.
# NB `tier` is a DIFFERENT axis, on fps_scenes (ui|player) — do not merge the two vocabularies.
def case_suite(case):
    ops = {o["op"] for o in case.get("operations", [])}
    return "codec" if ops <= {"play"} else "logic"


def main():
    ap = argparse.ArgumentParser(description="webOS Plex player on-device regression harness")
    ap.add_argument("--build", action="store_true", help="cargo + make + make deploy before running")
    ap.add_argument("--filter", default=None, help="run only cases whose name contains this substring")
    ap.add_argument("--suite", default=None, choices=["logic", "codec"],
                    help="run only one suite: 'logic' (seek/resume/audio/subtitle — the engine and "
                         "pump; still covers h264-dp, 4k-hevc-dp and transcode) or 'codec' (the "
                         "play-only decision + Load-payload cases). Default: every case. "
                         "NB distinct from fps_scenes' ui|player 'tier'.")
    ap.add_argument("--list", action="store_true", help="list cases and exit")
    ap.add_argument("--tv", default=None, help="override TV IP (default from manifest.local.json)")
    ap.add_argument("--verbose", action="store_true", help="print evidence for passing assertions too")
    ap.add_argument("--no-early", action="store_true",
                    help="don't stop a case as soon as it passes — run the full manifest run_secs. "
                         "Slower by design: it widens the window for a LATE `Playing error` to show "
                         "up, which early exit trades away for speed.")
    ap.add_argument("--owner", action="store_true",
                    help="run as the config.local.h OWNER token (default: run as the overlay's "
                         "test_user, so watch history stays off your real account)")
    ap.add_argument("--fps", action="store_true",
                    help="run the FPS regression suite (UI tier: home/detail, no video needed)")
    ap.add_argument("--fps-player", action="store_true",
                    help="FPS suite INCLUDING player-tier scenes (info/menu — needs playback, slower)")
    args = ap.parse_args()

    manifest = load_manifest()   # case definitions + the gitignored local overlay, merged
    cfg = {
        "tv": args.tv or manifest["tv"],
        "pms": manifest["pms"],
        "no_early": args.no_early,
    }
    cases = manifest["cases"]
    if args.suite:
        cases = [c for c in cases if case_suite(c) == args.suite]
    if args.filter:
        cases = [c for c in cases if args.filter in c["name"]]

    if args.list:
        for c in manifest["cases"]:
            ops = "+".join(o["op"] for o in c["operations"])
            print(f"{c['name']:32s} suite={case_suite(c):6s} rk={c['rk']:<5} {ops:20s} "
                  f"{', '.join(c.get('covers', []))}")
        for s in manifest.get("fps_scenes", []):
            tag = s["route"] + (f"/{s.get('overlay')}" if s.get("overlay") else "")
            gates = f"loop_floor={s['loop_floor']}"
            if s.get("fps_floor") is not None:
                gates += f" fps_floor={s['fps_floor']}"
            if s.get("fps_ceiling") is not None:
                gates += f" fps_ceiling={s['fps_ceiling']}"
            print(f"fps:{s['name']:28s} tier={s.get('tier','ui'):6s} {tag:16s} {gates}")
        return 0

    # FPS regression suite — a separate path from the playback cases. UI-tier scenes need no video
    # (and no PMS token); --fps-player adds the info/menu scenes, which decode video as the test user.
    if args.fps or args.fps_player:
        if args.suite:
            sys.exit("--suite selects playback cases; the FPS scenes use --fps / --fps-player")
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
        arm_teardown(cfg["tv"])
        if args.build:
            do_build(cfg["tv"])
        return run_fps_suite(manifest.get("fps_scenes", []), cfg, token, include_player)

    if not cases:
        sys.exit(f"no cases match --filter {args.filter!r} / --suite {args.suite!r}")

    admin_token = read_token()  # owner token from config.local.h; never printed

    # Resolve the identity every case plays as. Default = the overlay's test_user, so
    # playback + timeline scrobbles land on that user's history and the owner's real account
    # stays clean. --owner opts back into the owner token. Neither token is ever printed.
    test_user = manifest.get("test_user")
    if args.owner or not test_user:
        token = admin_token
        cfg["user_label"] = "owner (config.local.h)"
        cfg["inject_token"] = True  # the binary has NO baked token; every identity is injected
        if not args.owner:
            print("NOTE: no test_user in manifest.local.json -> running as OWNER "
                  "(history WILL be affected)")
    else:
        token = fetch_managed_user_token(admin_token, cfg["pms"]["host"], cfg["pms"]["port"],
                                         test_user["id"])
        cfg["user_label"] = f'{test_user.get("title", "managed")} (id={test_user["id"]})'
        cfg["inject_token"] = True
    print(f"test identity: {cfg['user_label']}  (playback + watch-history isolation)")

    arm_teardown(cfg["tv"])
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
    # SIGTERM has to UNWIND rather than kill the interpreter outright, or `kill <harness pid>`
    # leaves exactly the state teardown() exists to prevent. SIGINT already raises
    # KeyboardInterrupt, which the same finally catches; a `sys.exit(...)` inside main raises
    # SystemExit, which also runs the finally on its way out (and keeps its own exit status).
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    code = 1
    try:
        code = main()
    except KeyboardInterrupt:
        print("\ninterrupted — closing the app and clearing the TV")
        code = 130
    finally:
        if _TEARDOWN_TV:
            teardown(_TEARDOWN_TV)
    sys.exit(code)
