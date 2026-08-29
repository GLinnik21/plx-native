#!/usr/bin/env python3
"""
On-device regression harness for the webOS Plex player (plex-native-poc).

The matrix is split in two: manifest.json holds the installation-independent case definitions,
and the gitignored manifest.local.json (see manifest.local.json.example) maps each case's
symbolic `item` key to a ratingKey on this server and supplies the PMS host, TV address and
test user. load_manifest merges them and fails with the missing key if the overlay is absent.

The matrix is a SUPERSET of what any one library can exercise, so an `item` key the overlay
cannot resolve -- absent, or left as the example's `<ratingKey>` placeholder -- SKIPS the cases
and fps scenes that need it and runs the rest, printing what it gave up. Only the values no run
of any size can proceed without (pms.host, tv, test_user.id, a malformed shared_server) are still
fatal. Before 2026-08-22 a single unmappable shape killed all 21 cases, which meant the suite ran
for exactly one library in the world -- see tests/README.md, "Running it against your own library".

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
  3. clears every plxnative-* trigger in THIS install's runtime root, then writes only the
     ones this case needs;
  4. runs `make run-stream TV=<tv> FLAVOR=<f>`, which relaunches the app and tails its event
     log live;
  5. filters the `smp_cb type=43 num=0 str=` flood and evaluates the per-op assertions
     CONTINUOUSLY as lines arrive, stopping the case the moment it passes — the manifest's
     run_secs is the cap, not the runtime (see stream_case for why that is sound, and
     --no-early to turn it off);
  6. records PASS/FAIL with the failing evidence line.

Two builds can sit on one television -- com.beb.plxnative, the app users install, and
com.beb.plxnative.debug beside it -- each with its own app directory, its own SAM id and its own
runtime root (/tmp for stable, /tmp/<app id> for a flavoured install). Every run here drives
exactly ONE of them: the flavour comes from --flavor, else the overlay's `flavour` key, else the
Makefile's own default, and every path that follows is ASKED FOR rather than restated
(`make -s print-rundir` / `print-eventlog`). See resolve_flavour.

Security: the PMS X-Plex-Token is read from src/config.local.h at runtime and is NEVER
printed, logged, or written to any file. The TV ssh creds already live in the committed
Makefile, so we shell out to `make` / sshpass for device I/O. A run that also needs a SECOND
server (a friend's shared one) resolves that server's own access token from plex.tv with the
same owner token -- again storing nothing; see resolve_shared_server.

Usage:
  ./tests/run.py --list                 # list cases and what they cover
  ./tests/run.py --build                # cargo + make + make deploy, then run all cases
  ./tests/run.py --filter marker        # run only cases whose name contains "marker"
  ./tests/run.py --flavor stable        # drive the OTHER install (see check_install first)
  ./tests/run.py                        # run every case (assumes app already deployed)

Exit code is nonzero if any selected case fails.

No third-party deps -- Python 3 stdlib only (macOS system python3 is fine).
"""

import argparse
import atexit
import functools
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
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
TV_HOST_FILE = os.path.join(REPO_ROOT, ".tv-host")

sys.path.insert(0, TESTS_DIR)
from serve_fixtures import serve, default_root as serve_fixtures_default_root  # noqa: E402  (needs TESTS_DIR on the path first)

# Where `make fixtures-pipeline` puts the generated pack. NOT inside the repo, and not merely by
# convention: the generator REFUSES an --out under the repo root, because this repository is
# public and .gitignore as the only defence against committing media has already been got wrong
# here once. The env var is the same one the Makefile reads, so `FIXTURES_OUT=... make
# fixtures-pipeline` and `FIXTURES_OUT=... ./tests/run.py` agree without a flag.
FIXTURES_ROOT = serve_fixtures_default_root()

# ---------------------------------------------------------------------------
# WHICH INSTALL this run drives. All four are resolved once, by resolve_flavour(), before anything
# is touched — and every one of them is ANSWERED BY THE MAKEFILE rather than computed here.
#
# That is the whole point: the flavour this harness resolved and the flavour it kills, launches and
# greps for are then one value and cannot disagree. Closing install A while launching install B
# reproduces SAM's stale-"running" no-op, and the run afterwards grades the other app's log — the
# "plausible wrong data" failure, not a clean one.
#
# (The Makefile spells the variable FLAVOR; this repo's prose, the Rust half and the overlay key
# spell the word flavour. Both spellings are deliberate, so neither side is being quoted wrong.)
FLAVOUR = None          # "stable" | "debug"
APPID = None            # com.beb.plxnative[.<flavour>]
RUNDIR = None           # the runtime root: /tmp for stable, /tmp/<app id> for a flavoured install
EVENTLOG = None         # <RUNDIR>/plxnative-events.log
RUN_STREAM_MARK = None  # the remote command text — see _run_stream_pids()

# reference list of the dev triggers the app reads (apply_triggers now GLOB-clears the runtime
# root's plxnative-*, so this no longer has to be exhaustive — it's kept for humans / grep)
ALL_TRIGGERS = [
    "plxnative-detail", "plxnative-detailplay", "plxnative-detailsec", "plxnative-detailcol",
    "plxnative-autoseek", "plxnative-menupick", "plxnative-menu", "plxnative-noaudio",
    "plxnative-grid", "plxnative-autoplay", "plxnative-h265", "plxnative-playidx", "plxnative-url",
    "plxnative-play", "plxnative-ffprobe", "plxnative-token", "plxnative-servers",
    # UI/FPS scenes (both profiler triggers MUST be cleared; either invalidates production pacing)
    "plxnative-detailosc", "plxnative-info", "plxnative-chapters", "plxnative-profile",
    "plxnative-hwcnt", "plxnative-glassboth", "plxnative-glasshz",
    # the track's material and the instruments that override or narrate it. `flattabs` is the one
    # that MUST be cleared: it swaps the shipped material for the flat capsule, so a leftover turns
    # every glass assertion into a measurement of something else.
    "plxnative-tabglassdim", "plxnative-flattabs", "plxnative-groundlog",
    # boot-flow triggers (heroidx pins the hero + bypasses the who's-watching picker; pickuser forces it)
    "plxnative-heroidx", "plxnative-pickuser",
    # itemmenu snaps into the grid and opens the press-and-hold card context menu (route=itemmenu)
    "plxnative-itemmenu",
    # playurl is the synthetic tier's entry; replay is how many times a FINISHED one restarts (#46)
    "plxnative-playurl", "plxnative-replay", "plxnative-gstlog", "plxnative-quality",
    "plxnative-qualityswitch",
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


def _item_rk(items, key):
    """The ratingKey for a symbolic item key, or None when this installation has nothing of that
    shape. An UNEDITED `<ratingKey>` placeholder reads exactly like an absent key -- both say "I
    have no such item" -- because the dominant path for anyone who is not the maintainer is to copy
    the example and fill in the few shapes they can actually find. Reading only the ABSENT branch
    would move the death from here to the stray-placeholder guard below and change nothing."""
    v = items.get(key)
    if v is None or (isinstance(v, str) and v.startswith("<")):
        return None
    return str(v)


def _item_missing_reason(items, key):
    return (f"no `items` entry for {key!r}" if key not in items
            else f"`items.{key}` is still the template placeholder")


def _resolve_items(entries, items):
    """Turn each entry's symbolic `item` key into the concrete `rk` the rest of the runner uses.

    An unresolvable key marks the entry `skip` and leaves `rk` ABSENT, rather than killing the run:
    the matrix is a superset of what any one library can exercise, and a library that covers 14 of
    the 21 shapes should run those 14 and say what it gave up. Leaving `rk` absent rather than
    setting a sentinel is deliberate -- every consumer of case["rk"] sits behind the skip filter in
    main(), so a partition that is ever wrong fails with a KeyError naming the case instead of
    quietly driving the television at ratingKey 0."""
    for e in entries:
        key = e.get("item")
        if key is None:
            continue                      # an fps scene that needs no library item
        rk = _item_rk(items, key)
        if rk is None:
            e["skip"] = _item_missing_reason(items, key)
            continue
        e["rk"] = rk


def load_manifest(pipeline_only=False, tv_override=None, for_listing=False):
    """Merge the tracked matrix with the gitignored overlay.

    `pipeline_only` is what makes requirement-1 true — that a stranger can run the pipeline tier
    with nothing configured. That tier talks to no PMS, holds no token and names no ratingKey, so
    every Plex-shaped key here stops being required: the overlay may be absent ENTIRELY, in which
    case the TV address is read from the repo's own gitignored `.tv-host` (the same file the
    Makefile and `tools/` fall back to). A TV address is the one thing that still cannot be
    guessed, and is the only thing this path can die for.

    `for_listing` drops **even that**, and it is the difference between `--list` being offline and
    only claiming to be. `--list` prints which cases a set of flags selects; it opens no socket,
    arms no trigger and touches no television, which is exactly why `DefaultTier` in
    `tests/test_harness.py` spawns the real CLI to ask what a bare command runs. On any machine
    with no `.tv-host` and no overlay — a fresh clone, a fleet worktree, and **the CI runner** —
    the requirements below fired first and `--list` exited 1 with empty stdout, so those two tests
    failed on every push. They had been red on `main` since 2026-08-22 and were about to be
    inherited, identically, by every lane of a parallel fleet: eleven red PRs in which a real
    failure would have been indistinguishable from the background.
    """
    with open(MANIFEST) as f:
        manifest = json.load(f)
    local = {}
    try:
        with open(MANIFEST_LOCAL) as f:
            local = json.load(f)
    except FileNotFoundError:
        if not (pipeline_only or for_listing):
            _die_no_overlay()
    except ValueError as e:
        # Still fatal for a listing: a malformed overlay is a typo to fix, not an absent one to
        # work around, and silently listing the wrong thing is the failure mode this whole
        # function is shaped to avoid.
        sys.exit(f"{MANIFEST_LOCAL} is not valid JSON: {e}")

    if pipeline_only:
        # `tv` from the overlay if it is there, else .tv-host, else nothing to drive.
        tv = tv_override or local.get("tv")
        if not tv and os.path.isfile(TV_HOST_FILE):
            with open(TV_HOST_FILE) as f:
                tv = f.read().strip() or None
        if not tv and not for_listing:
            sys.exit(f"--pipeline needs a TV address: put one in {TV_HOST_FILE} (one line, an IP "
                     f"or hostname), or a `tv` key in {MANIFEST_LOCAL}, or pass --tv")
        if tv:
            manifest["tv"] = tv
        if "flavour" in local:
            manifest["flavour"] = local["flavour"]
        return manifest

    for field in ("pms", "tv"):
        if field not in local:
            if for_listing:
                continue    # nothing downstream of a listing dials either of them
            _die_no_overlay(f"\n  (no {field!r} block)")
        manifest[field] = local[field]
    # WHICH INSTALL to drive, and optional: absent means the Makefile's own default. An
    # installation that always tests one build says so once, here, instead of typing --flavor on
    # every invocation. Not validated here on purpose — the Makefile owns the whitelist and its
    # parse-time $(error) names both the bad value and the allowed set on the first query.
    if "flavour" in local:
        manifest["flavour"] = local["flavour"]
    # test_user is optional by design: leaving it out runs as the owner (with a warning).
    if "test_user" in local:
        manifest["test_user"] = local["test_user"]
    # shared_server is optional the same way: an installation with no second server simply omits it,
    # and any case that needs one is SKIPPED (not failed) with the reason printed. What is NOT
    # optional is naming the server well enough to look up on plex.tv -- a block with neither
    # machine_id nor name resolves to nothing, and would otherwise fail later as a mystery.
    if "shared_server" in local:
        ss = local["shared_server"]
        if not (ss.get("machine_id") or ss.get("name")):
            _die_no_overlay("\n  (`shared_server` needs a `machine_id` (preferred) or a `name` to "
                            "match against your plex.tv resources)")
        manifest["shared_server"] = ss

    items = local.get("items", {})
    _resolve_items(manifest["cases"], items)
    _resolve_items(manifest.get("fps_scenes", []), items)
    # The two places an item key appears NESTED rather than as a case's own `item`: the successor
    # a credits marker auto-advances into is both reset server-side and asserted on by ratingKey.
    # An unresolvable one skips ITS OWN case and nothing else -- before this, a library with no
    # "the episode after that one" killed all 21 cases over the one case that names it.
    for c in manifest["cases"]:
        setup = c.get("setup", {})
        if "also_reset" in setup:
            resolved = [(k, _item_rk(items, k)) for k in setup["also_reset"]]
            for k, rk in resolved:
                if rk is None:
                    c.setdefault("skip", _item_missing_reason(items, k) + " (setup.also_reset)")
            setup["also_reset"] = [rk for _, rk in resolved if rk is not None]
        for op in c.get("operations", []):
            key = op.get("expect_up_next")
            if key:
                rk = _item_rk(items, key)
                if rk is None:
                    c.setdefault("skip", _item_missing_reason(items, key) + " (expect_up_next)")
                else:
                    op["expect_up_next"] = rk

    # A copied-but-unedited template fails LOUDLY here rather than as a 404 from plex.tv or as a
    # case that never plays: every value in the example is bracketed, and none is ever legitimate.
    # `items.*` is deliberately NOT in this list any more: a bracketed ratingKey is the honest
    # answer of somebody whose library has nothing of that shape, and it now skips the cases that
    # need it. The values below stay fatal because no run of any size can proceed without them.
    ss = manifest.get("shared_server", {})
    # `.get` on both, for `for_listing`'s sake: with no overlay at all neither key was ever
    # installed above, and a listing must still print. A real run cannot reach here without them —
    # the `_die_no_overlay` calls above are what guarantee that, which is why this stays a lookup
    # and not a second requirement check.
    stray = [f"{k}={v}" for k, v in
             [("pms.host", manifest.get("pms", {}).get("host")), ("tv", manifest.get("tv"))]
             + ([("test_user.id", manifest["test_user"].get("id"))] if "test_user" in manifest else [])
             + [(f"shared_server.{k}", ss.get(k)) for k in ("machine_id", "name", "host")]
             if isinstance(v, str) and v.startswith("<")]
    if stray:
        sys.exit(f"{MANIFEST_LOCAL} still holds template placeholders: {', '.join(stray)}\n"
                 f"  Replace each with the value for your own server / TV / library.")
    return manifest


# ---------------------------------------------------------------------------
def partition_skips(entries):
    """Split `entries` into (runnable, [(name, reason), …]).

    The counterpart to `_resolve_items` / `_resolve_fixtures`, which are what set the `skip` key.
    It must happen before ANY consumer subscripts `rk` or `path`, because a skipped entry carries
    neither — deliberately, so a partition that is ever wrong raises KeyError naming the entry
    instead of driving the television at some sentinel. Held as (name, reason) pairs so a summary
    can say which shape an installation is missing, not merely how many.
    """
    return ([e for e in entries if not e.get("skip")],
            [(e["name"], e["skip"]) for e in entries if e.get("skip")])


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
# The SECOND server (a friend's shared server) — resolved, never stored
# ---------------------------------------------------------------------------
# A shared server is a separate authority: its own machineIdentifier and its own per-(user,server)
# accessToken, and the account token gets a 401 from it. So a run that has to reach two servers
# needs two credentials, which one plxnative-token file cannot carry -- that is what
# plxnative-servers (app: dev::servers) exists for.
#
# The overlay NAMES the server; it never holds its token. plex.tv's resource list is keyed by the
# owner's account token, which the harness already reads from src/config.local.h for everything
# else, and each entry carries that account's accessToken for that server. Same bargain as
# fetch_managed_user_token: one secret in one gitignored place, everything else derived at runtime.
def _plextv_resources(admin_token):
    """Every server this account can reach — owned AND shared with it — from plex.tv."""
    url = "https://plex.tv/api/v2/resources?includeHttps=1&includeRelay=1"
    req = urllib.request.Request(url, headers={
        "Accept": "application/json",
        "X-Plex-Token": admin_token,
        "X-Plex-Client-Identifier": CID})
    with urllib.request.urlopen(req, timeout=20) as resp:
        return json.load(resp)


def _server_resources(res):
    return [r for r in res if "server" in (r.get("provides") or "")]


def _is_ipv4(addr):
    parts = (addr or "").split(".")
    return len(parts) == 4 and all(p.isdigit() and 0 <= int(p) <= 255 for p in parts)


def pick_connection(hit):
    """Best (address, port, how) plex.tv offers for this server, or None.

    `local` does NOT mean "on the TV's network" for a server someone SHARED with you -- it means
    on the OWNER's. A real shared server in this account advertises `10.9.9.5:32400 local=true`
    (the friend's LAN, unroutable from here) alongside its public address, so ranking `local` first
    the way the app's own sign-in does would hand the TV an address it can never reach and turn a
    credentials test into a mystery timeout. Hence: for an OWNED server local wins; for a shared one
    the public address does, and `local` is demoted below it.

    Within a rank, a dotted quad beats a hostname, because the app's media transport (stream.rs)
    has no DNS at all. Relay last: it is TLS-only. Either way the caller PRINTS what it picked --
    `shared_server.host` in the overlay is the override, and for most shares it is the right answer.
    """
    owned = bool(hit.get("owned"))
    conns = [c for c in (hit.get("connections") or []) if c.get("address")]
    if not conns:
        return None

    def rank(c):
        if c.get("relay"):
            tier = 3
        elif c.get("local"):
            tier = 0 if owned else 2
        else:
            tier = 1
        return (tier, 0 if _is_ipv4(c["address"]) else 1)

    best = min(conns, key=rank)
    how = "relay" if best.get("relay") else ("LAN" if best.get("local") else "remote")
    return best["address"], int(best.get("port") or 32400), f"{how}, {best.get('protocol', '?')}"


def resolve_shared_server(admin_token, spec):
    """Turn the overlay's `shared_server` block into the credentials the TV needs.

    Returns {name, machine_id, host, port, token}; the token is resolved from plex.tv and is NEVER
    printed. Exits, naming what it could not resolve, on anything unresolvable -- an unreachable
    plex.tv, a server no longer shared with this account, or a server with no address the TV could
    use. A silent fallback here would run the case against ONE server and pass it.
    """
    want_mid, want_name = spec.get("machine_id"), spec.get("name")
    try:
        res = _server_resources(_plextv_resources(admin_token))
    except Exception as e:
        sys.exit(f"cannot resolve the second server: plex.tv /resources failed ({e})\n"
                 f"  (this call needs internet; the rest of the harness is LAN-only)")

    def matches(r):
        if want_mid:
            return r.get("clientIdentifier") == want_mid
        return (r.get("name") or "").lower() == (want_name or "").lower()

    hit = next((r for r in res if matches(r)), None)
    if hit is None:
        # The ONE place a share's name and machineIdentifier still reach stdout, and deliberately:
        # they are the answer to the question this exit asks, which is "put one of these in your
        # gitignored overlay". It is a fatal exit before anything runs, not a run transcript — but
        # it is still pasteable, so it says so.
        known = "\n".join(f"    {r.get('name')!r}  machine_id={r.get('clientIdentifier')}  "
                          f"{'owned' if r.get('owned') else 'shared with you'}" for r in res)
        sys.exit(f"shared_server {want_mid or want_name!r} is not in this account's plex.tv "
                 f"resources (is it still shared with you?). Servers this account can reach:\n"
                 f"{known or '    (none)'}\n"
                 f"  (a 'shared with you' row names somebody else's machine — copy what you need "
                 f"into {os.path.basename(MANIFEST_LOCAL)}, don't paste this list into an issue.)")

    token = hit.get("accessToken")
    if not token:
        sys.exit(f"shared_server {hit.get('name')!r} has no accessToken on plex.tv — the share was "
                 f"probably revoked; re-accept it, or drop the block from {MANIFEST_LOCAL}")

    host, port = spec.get("host"), spec.get("port")
    if not host:
        best = pick_connection(hit)
        if best is None:
            sys.exit(f"shared_server {hit.get('name')!r} advertises no connection at all on plex.tv."
                     f"\n  Set `shared_server.host` (and `port`) in {MANIFEST_LOCAL} to an address "
                     f"the TV can route to.")
        host, port, how = best
        # Loud on purpose. The app streams over plain HTTP to a numeric IP (stream.rs: no DNS, no
        # TLS), so anything but a LAN address may resolve, be picked, and still be unreachable from
        # the TV — which would read as "the second server's credentials did not work".
        if how.split(",")[0] != "LAN":
            print(f"    NOTE: plex.tv offers no LAN address for {hit.get('name')!r} — using "
                  f"{host}:{port} ({how}). The TV's transport is plain HTTP to a numeric IP; set "
                  f"`shared_server.host`/`port` in {os.path.basename(MANIFEST_LOCAL)} if it cannot "
                  f"reach that.")
    return {
        "name": spec.get("name") or hit.get("name") or "shared",
        "machine_id": hit.get("clientIdentifier") or want_mid or "",
        # The OWNER's plex.tv handle, straight off the resource. It is what every browsing surface
        # says out loud (the Sources list, the Library chip), and it is also what tells the app this
        # is somebody else's server at all: empty means owned. Public, unlike the token below.
        "handle": hit.get("sourceTitle") or "",
        "host": host,
        "port": int(port or 32400),
        "token": token,
    }


def shared_servers_json(cfg, entry):
    """The plxnative-servers payload for this case/scene, or None if it wants one server.

    Opt-in, never blanket: a case declares `needs_shared_server` in manifest.json (or the whole run
    passes --shared-server). Injecting a second server into every case would change what Home shows
    for cases that say nothing about it.
    """
    srv = cfg.get("shared_server")
    if not srv:
        return None
    if not (cfg.get("shared_all") or entry.get("needs_shared_server")):
        return None
    return json.dumps([srv], separators=(",", ":"))


def sh_squote(s):
    """POSIX single-quote a string for the one-line ssh command (names can contain apostrophes)."""
    return "'" + s.replace("'", "'\\''") + "'"


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


def make_argv(target_args):
    """The `make` command line every invocation in this file uses — FLAVOR included.

    The flavour is threaded HERE and not at the call sites, of which there are eight across four
    functions (kill x3, all, deploy, run, run-stream, and stream_case's Popen). One of them would
    be forgotten, and a `make kill` that forgot it closes the app users installed while this run
    drives the developer one — then grades a log nobody wrote. There is nothing to forget if the
    wrapper owns it.
    """
    return ["make", "-s", "-C", REPO_ROOT] + target_args + [f"FLAVOR={FLAVOUR}"]


def tv_awake(tv, timeout=8):
    """Does the television answer ssh at all. One probe, no lease, no side effects."""
    try:
        return ssh(tv, "true", timeout=timeout).returncode == 0
    except Exception:
        return False


def require_tv(tv, name):
    """**A sleeping television is not a test result.** Abort the run instead of grading it.

    This set drops to standby on its own (`.claude/skills/wake-tv/`), and when it does mid-suite
    EVERY assertion of every remaining case fails at once — `stream_path`, `load_decl`, `codec`,
    `video_bound`, `pos_climb`, `server_wire` — because the app never launched and the log is a
    boot banner. Measured 2026-08-28: a 19-case ABR tier reported **14 failures** that way, four
    cases in, and the shape of it is indistinguishable at a glance from a catastrophic regression
    in the code under test. It cost a full re-run to tell apart, and it is exactly the "plausible
    wrong data" the repo's one-television rule warns about — the damage is not a clean failure.

    So the run stops at the first case it cannot reach, naming the television rather than the
    change. It does NOT wake the set itself: waking is a lease-holding operation with its own
    skill, and a harness that silently resurrects the hardware hides how often this happens.
    """
    if tv_awake(tv):
        return
    sys.exit(
        f"\nTELEVISION UNREACHABLE before `{name}` — stopping rather than grading.\n"
        f"  Every assertion would fail at once and would read as a code regression.\n"
        f"  Wake it and re-run:  .claude/skills/wake-tv/wake-tv.sh\n"
    )


def make(target_args, timeout, capture=True):
    """Invoke a make target from the repo root (absolute cwd so nothing drifts)."""
    return subprocess.run(make_argv(target_args), capture_output=capture, text=True,
                          timeout=timeout)


def make_query(goals, flavour=None):
    """Ask the Makefile for derived values (`make -s print-<x> …`) — the only supported way.

    SEVERAL GOALS GO IN ONE INVOCATION, which is why this takes a tuple: the Makefile's query
    targets compose, its PURE_QUERY guard is satisfied as long as EVERY goal is a query, and one
    make start-up is cheaper than one per value — `resolve_flavour` was parsing a 900-line Makefile
    four times at harness start. `tools/stream-screen.py` already worked this way; this is the
    same helper, not a second one. Returns a list, one line per goal, in the order asked.

    NEVER `make -p` / `make -pn`, which is the obvious-looking alternative and is a trap: it prints
    a recursive variable's UNEXPANDED DEFINITION, so `TV` comes back as the literal
    `$(strip $(shell cat .tv-host ...))`, every ssh built from it fails, and the tool reports an
    unreachable television that is awake and answering (tools/tv-session.sh documents the same trap
    from the shell side). These goals are real echo recipes of real values, and the Makefile's
    PURE_QUERY guard keeps a query free of side effects.
    """
    goals = [goals] if isinstance(goals, str) else list(goals)
    cmd = ["make", "-s", "-C", REPO_ROOT, *goals] + ([f"FLAVOR={flavour}"] if flavour else [])
    shown = " ".join(cmd)
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.TimeoutExpired) as e:
        sys.exit(f"cannot ask the Makefile which install to drive (`{shown}`): {e}")
    if p.returncode != 0:
        # A bad flavour lands here: the Makefile's parse-time $(error) names the value and the
        # whitelist, which is a better message than anything this file could invent.
        sys.exit(f"`{shown}` failed:\n{(p.stderr or p.stdout).strip()}")
    out = p.stdout.strip().splitlines()
    if len(out) != len(goals):
        sys.exit(f"`{shown}` printed {len(out)} lines for {len(goals)} goals — is {REPO_ROOT} this repo?")
    return out[0] if len(goals) == 1 else out


def resolve_flavour(args, manifest):
    """Decide which install this run drives, then ask the Makefile for everything that follows.

    Order: --flavor, else the overlay's `flavour` key, else the Makefile's own default (which is
    `debug`, deliberately — the dangerous id has to be typed). Nothing below is computed from the
    flavour here. The app id, the runtime root and the event log come back from the same Makefile
    the launch will use, so this harness cannot arm triggers in one root and grade a log in
    another — and a future change to the naming rule reaches the harness for free.
    """
    global FLAVOUR, APPID, RUNDIR, EVENTLOG, RUN_STREAM_MARK
    # ONE invocation for all four. `print-flavor` comes back first and answers the default when
    # neither --flavor nor the overlay named one, so the old ask-then-ask-again round trip is gone.
    FLAVOUR = args.flavor or manifest.get("flavour") or ""
    FLAVOUR, APPID, RUNDIR, EVENTLOG = make_query(
        ("print-flavor", "print-appid", "print-rundir", "print-eventlog"), FLAVOUR or None)
    # The tail `make run-stream` ends in. _run_stream_pids() matches it against `ps` to find this
    # harness's OWN ssh clients, so it has to be the text the Makefile actually runs: a copy that
    # stops matching reaps nothing, and every case then leaks an ssh client holding a remote
    # `tail -F` (125 of them piled up against the TV in one session the last time that happened).
    RUN_STREAM_MARK = f"tail -F -n +1 {EVENTLOG}"
    return FLAVOUR


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
def triggers_for_case(case, url_base=None):
    """
    Map a case's operations -> the plxnative-* files to write in the TV's runtime root.
    Returns a list of (filename, content-or-None) pairs; None => `touch` (empty marker).

    `url_base` selects the TIER, and it selects only the entry trigger: a pipeline case names a
    generated file served over HTTP from this machine and declares its own Load payload
    (`plxnative-playurl`), an integration case names a ratingKey on the PMS
    (`plxnative-play`). Everything after that — the seek scripts especially — is the same
    machinery driving the same engine, which is why this is one function with two heads rather
    than two functions that would drift apart at the first new operation.
    """
    if url_base is not None:
        # JSON, whole-file — dev::PlayUrl. `separators` drops the spaces `json.dumps` would put
        # after ':' and ',': they are legal JSON and the parser takes them, but this string is
        # about to be printed in the case header and pasted into issues, and short is legible.
        # Today's fields contain no apostrophe (a URL from `lan_ip()` plus codec names plus
        # numbers), and apply_triggers still shell-quotes the complete value rather than making
        # that incidental property the remote command's security boundary.
        spec = dict(case.get("declare", {}))
        spec["url"] = f"{url_base}/{case['fixture']}"
        files = [("plxnative-playurl", json.dumps(spec, separators=(",", ":")))]
        files.append(("plxnative-stats", None))
        auto = case.get("auto_network")
        if auto:
            spec["auto_source_kbps"] = int(auto["source_kbps"])
            spec["auto_hls_base"] = f"{url_base}/__abr"
            # Enter HLS directly rather than by provoking a starvation. Declared per case: the one
            # case that GRADES the Original->HLS transition must not skip it. See
            # `dev::PlayUrl::auto_start_hls` — the old entry relied on the starvation horizon
            # firing while the reserve was filling, which stopped being possible on 2026-08-27.
            if auto.get("start_hls"):
                spec["auto_start_hls"] = True
            # **The SOURCE raster, which is what decides whether the 4K actuator is feasible.**
            # `route::arm_auto_fixture` defaults to 1080p, so omitting this leaves every existing
            # case byte-identical; declaring [3840, 2160] is the only way a case can reach the Uhd
            # rung, and until `tests/serve_fixtures.py` answered 22000 there was no such way at
            # all. `[w, h]`.
            if auto.get("source_raster"):
                spec["source_raster"] = [int(v) for v in auto["source_raster"]]
            files[0] = ("plxnative-playurl", json.dumps(spec, separators=(",", ":")))
            files.append(("plxnative-quality", "auto"))
            # Pin Auto's ladder to one actuator for the whole case, by REQUEST rate. Measurement
            # step M4 needs a rung held long enough to read a settled reserve at it, and the
            # playback-quality selector cannot do it: a non-Auto quality returns `None` from
            # `route::hls_abr_control` before a controller exists, and the quality ladder has no
            # mid-1080p points. Absent => ordinary Auto.
            if case.get("abr_pin"):
                files.append(("plxnative-abrpin", str(int(case["abr_pin"]))))
            # The A/B selector increments I5 and I6 need. It must come through the manifest and
            # not be armed by hand: `apply_triggers` wipes every plxnative-* in the runtime root
            # before each case, so a hand-armed trigger cannot survive into the case it is meant
            # to switch. Inert today — nothing reads it until a second policy path exists.
            if case.get("abr_policy"):
                files.append(("plxnative-abrpolicy", str(case["abr_policy"])))
        # `plxnative-replay=<n>` — how many times a FINISHED playback restarts itself (LG #46).
        # Keyed off `expect.replays`, so the number the app is TOLD and the number the harness
        # GRADES are one statement rather than two that nothing keeps in step. Absent => the
        # trigger is not written at all, which is the one-shot behaviour every other case wants.
        n = case.get("expect", {}).get("replays", 0)
        if n:
            files.append(("plxnative-replay", str(n)))
    else:
        # Pin the routing contract instead of inheriting whatever the person last selected on the
        # television. The established PMS matrix grades direct-play/remux/progressive transcode,
        # so its default is Original; a future adaptive case opts in with `"quality": "auto"`.
        # This trigger is an in-memory override and never changes the persisted user preference.
        files = [
            ("plxnative-play", case["rk"]),  # the robust play trigger (fetches any rk)
            ("plxnative-quality", case.get("quality", "original")),
            ("plxnative-stats", None),
        ]
    gst_debug = case.get("gst_trace", {}).get("debug")
    if gst_debug:
        files.append(("plxnative-gstlog", gst_debug))
    for op in case["operations"]:
        kind = op["op"]
        if kind == "seek" and op.get("mode") == "rapid":
            # seek SCRIPT: comma-separated steps fired one per ~300ms — absolute seconds or
            # tap-relative +N/-N (vs the last requested target). Exercises seek coalescing.
            files.append(("plxnative-autoseek", op["script"]))
        elif kind == "seek":
            # The TARGET, always, on both tiers — because the manifest already declares it and
            # `evaluate` already grades against it (`op.get("target_s", 140)`), so writing an empty
            # marker instead meant "where the case seeks" and "where the case asserts it seeked"
            # were two independent statements of the same number that nothing kept in step.
            #
            # This was briefly a pipeline-only arm, on the theory that making the server tier write
            # its target would swap an app code path. That theory was WRONG and the app says so:
            # `dev::read` returns Some("") for an empty file, and app.rs splits on ',', drops empty
            # tokens, then does `if steps.is_empty() { steps.push("140") }` — so an empty file and
            # the content "140" converge to a byte-identical `steps == ["140"]` before any seek
            # logic runs. There is no second path to preserve. The app's empty-file default
            # survives as a by-hand affordance, exercised by no case, which is its correct status.
            #
            # `delay_ms` is what makes a seek reachable from an ABR case. The app fires the first
            # step at a fixed ~12 s after the player route is entered, and an ABR transaction needs
            # tens of seconds of samples before it can COMMIT — so without a delay every seek in
            # this suite lands before the controller has ever switched, and the state either side
            # of a seek-after-a-switch is untestable. It is not the same quantity as `gap_ms` (the
            # cadence BETWEEN rapid steps) and the app parses them as two tokens for that reason;
            # expressing the wait as a gap would need a throwaway first seek, which would then be
            # in the log this case grades.
            target = str(op.get("target_s", 140))
            delay_ms = op.get("delay_ms")
            files.append((
                "plxnative-autoseek",
                f"delay={int(delay_ms)},{target}" if delay_ms else target,
            ))
        elif kind == "skip":
            files.append(("plxnative-marker", op["marker"]))
        elif kind == "marker":
            # jump to 5s before the named server marker, so the skip/Up Next control row is
            # reachable in seconds instead of 50 minutes into an episode
            files.append(("plxnative-marker", op["marker"]))
        elif kind == "quality_switch":
            # The rungs to switch to WHILE IT PLAYS, in the app's own wire vocabulary — the same
            # strings `plxnative-quality` accepts and `quality: switch → …` prints, so the case
            # states each rung once. `gap_ms` only appears when there is more than one step,
            # because with one there is no cadence to state.
            steps = op["to"] if isinstance(op["to"], list) else [op["to"]]
            gap = f'gap={op["gap_ms"]},' if len(steps) > 1 else ""
            files.append(("plxnative-qualityswitch", gap + ",".join(steps)))
        elif kind == "audio_switch":
            files.append(("plxnative-menupick", f'{op["tab"]},{op["row"]}'))
        elif kind == "subtitle":
            files.append(("plxnative-menupick", f'{op["tab"]},{op["row"]}'))
        # "play" and "resume" need no extra trigger (resume rides the seeded viewOffset).
    return files


def key_inject_for_case(case):
    """(log-pattern, remote-token) for a case that presses a key MID-RUN, or None.

    The app mkfifos plxnative-remote in its runtime root and drains it every frame, so a token
    written while it runs replays through the real key handler. Keying the write to a LOG LINE
    rather than to a wall-clock delay is what makes it deterministic: the press lands the moment
    the control is actually on screen, however long the resolve took.
    """
    for op in case["operations"]:
        if op["op"] == "skip":
            return (f"marker offer: {op['marker'].capitalize()}", op.get("press", "ok"))
    return None


def apply_triggers(tv, files, extra=None):
    """Clear every plxnative-* trigger in THIS install's runtime root (sparing the *.log files),
    then create the ones this case needs, in one ssh round-trip. GLOB-based, not an enumerated list,
    so a newly-added app trigger can never bleed between scenes — a stale plxnative-novsync would
    uncap vsync and false-PASS an FPS scene, a stale plxnative-press/-login would derail a home
    scene. ALL_TRIGGERS above is now just a human reference of the known triggers.

    The glob is scoped to RUNDIR, which is what keeps the two installs out of each other's way: the
    stable root is /tmp itself, and `/tmp/plxnative-*` cannot match the flavoured root beside it
    (`/tmp/com.beb.plxnative.debug` — the separator is a DOT for exactly this reason, since a
    directory called `plxnative-debug` would read as an armed trigger to `dev::any_trigger_present`
    and silently suppress the other install's who's-watching picker).

    `extra` is a raw shell command — or a list of them — appended to the same round-trip, for a
    trigger whose VALUE must not reach stdout (a PMS token) and so cannot go through the printed
    `files` list. Both credential triggers ride it: plxnative-token and plxnative-servers.
    """
    # The root has to EXIST and be world-writable before anything is written into it. Two uids
    # write here and neither can be made to go second: this ssh is ROOT and arms triggers before the
    # app has ever booted, while the app runs jailed under its own uid and creates its logs here.
    # Whoever creates the directory sets its mode — and umask masks mkdir's, hence the explicit
    # chmod — so an owner-only mode locks the other one out. A root-owned event log the app cannot
    # write stays 0 bytes, which every assertion in this file reports as "no line found", i.e.
    # exactly like a total regression. A no-op for the stable flavour, whose root is /tmp (1777).
    parts = [f"mkdir -p {RUNDIR} && chmod 1777 {RUNDIR}"]
    # wipe every trigger, keeping only the append-only logs (events/stderr/crash)
    parts.append(f'for f in {RUNDIR}/plxnative-*; do case "$f" in *.log) ;; *) rm -f "$f";; esac; done')
    # GST_DEBUG_FILE_OVERWRITE is honoured only once libpf installs its logger. Remove the old
    # trace here as well, before launch, so an app that fails before that point cannot be graded
    # against the previous case's perfectly plausible per-frame log.
    if any(name == "plxnative-gstlog" for name, _ in files):
        parts.append(f"rm -f {RUNDIR}/plxnative-gst.log")
    for name, content in files:
        if content is None:
            parts.append(f"touch {RUNDIR}/{name}")
        else:
            parts.append(f"printf '%s' {sh_squote(content)} > {RUNDIR}/{name}")
    for cmd in ([extra] if isinstance(extra, str) else (extra or [])):
        if cmd:
            parts.append(cmd)
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
# The Load payload's DECLARATION, as `player::engine` writes it once per streamed playback:
#   load: v=H264 a="AC3" fps=24.000 dv=present:0 P0/0 el:0 atmos:0
# This is the only place an event log says what the app told the television the stream WAS, as
# opposed to what the demuxer found in it. The two are independent, and the pipeline tier's whole
# reason for existing is that the first one can now be set without a PMS.
RE_LOAD = re.compile(r'load: v=(\S+) a="([^"]*)" fps=([\d.]+) '
                     r'dv=present:(\d) P(-?\d+)/(-?\d+) el:(\d) atmos:(\d)')
# The audio LANE the demuxer actually fed, off the same `ff: v=#0 …` line: `a=#<index>`.
RE_ALANE = re.compile(r"ff: v=#0 .*\ba=#(-?\d+)")
# The RATIONAL the Load payload's esInfo actually carries — `engine::fps_rational`'s output, as
# opposed to the float that went into it. Emitted on every playback that declares a non-zero rate.
RE_ESINFO = re.compile(r"esInfo: videoFps (\d+/\d+)")
# GStreamer's monotonic debug timestamp. GST is used only for lxvideosink's per-picture cadence;
# Starfish type 4 below is the coded-resolution authority.
RE_GST_CLOCK = re.compile(r"(?<!\d)(\d+):(\d{2}):(\d{2})\.(\d{1,9})(?!\d)")
# Starfish callback type 4 is the decoder's source-info event. Unlike LG's GST caps logger, it
# reports every coded-size transition on the measured firmware (including a return to an earlier
# size), so it is the authoritative dynamic-resolution signal for this gate.
RE_SMP_SOURCE_INFO = re.compile(r"smp_cb type=4 num=-?\d+ str=(\{.*\})")
# The audio feeder logs the first four attempts and every 200th thereafter. reply=O is Starfish
# accepting a compressed audio AU: not proof a loudspeaker made sound, but the strongest event-log
# readiness fact available and exactly what the badly interleaved first fixture never reached.
RE_AUDIO_FEED = re.compile(r"\bfeed a#(\d+)\s+sz=(\d+)\s+fed=(-?\d+)\s+reply=(.)\s+qbytes=(\d+)")
# the media URL carries the secret X-Plex-Token; strip it from anything we print/log.
RE_TOKEN = re.compile(r"(X-Plex-Token=)[^&\s]+")


def redact(s):
    """Never let the PMS token reach stdout: replace any X-Plex-Token=<v> with <token>."""
    return RE_TOKEN.sub(r"\1<token>", s)


# ---------------------------------------------------------------------------
# Saying which server, without saying WHOSE
# ---------------------------------------------------------------------------
# The app already holds itself to a contract here -- `dev.rs`'s `DevServer::describe`, and the
# `describe_redacts_everything_identifying_about_someone_elses_server` test that pins it: a SHARED
# server names nothing that identifies it or its owner (no name, no handle, no address, no
# machineIdentifier), only a stable non-reversible `ref=` so two lines about one server can be
# correlated inside a single log. An OWNED server is the user's own machine and is unchanged.
#
# This harness prints to stdout, and a harness transcript is exactly as pasteable into an issue or a
# PR body as an event log -- four PR bodies leaked these fields on 2026-08-14 and had to be redacted
# after the fact, which a public repository does not really allow. So the two formatters below are
# the same contract on this side of the wire, deliberately producing the SAME text the event log
# does, so a transcript line and a device line about one server read as one server.
def server_ref(machine_id):
    """`dev.rs`'s `DevServer::reference`, byte for byte: FNV-1a over the machineIdentifier, low 24
    bits, six hex digits. Not a cryptographic choice -- a legibility one. Matching the app's
    arithmetic exactly is the point: the harness and the TV then agree on one tag per server, and
    `dev.rs`'s `the_shared_reference_is_the_same_tag_the_harness_prints` pins the whole line to a
    literal so the two copies cannot drift."""
    h = 0xCBF29CE484222325
    for b in (machine_id or "").encode():
        h = ((h ^ b) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{h & 0xFFFFFF:06x}"


def describe_server(s):
    """One RESOLVED server, for stdout. Redacted iff it is somebody else's (see above).

    `handle` is the owner's plex.tv handle (`sourceTitle`), and empty means owned -- the same field,
    with the same meaning, that `DevServer::describe` branches on.
    """
    if s.get("handle"):
        port_set = "true" if int(s.get("port") or 0) > 0 else "false"
        return f"SHARED ref={server_ref(s.get('machine_id'))} port_set={port_set}"
    return f"{s.get('name')!r} @ {s.get('host')}:{s.get('port')} (machine_id={s.get('machine_id')})"


def describe_spec(spec):
    """The overlay's `shared_server` BLOCK, before anything is resolved (`--list`).

    Offline there is no `sourceTitle` to branch on, so ownership is unknown -- and the stricter rule
    is the safe one to guess. The `ref=` is the same tag a real run will print when the block names
    a `machine_id`, so `--list` and the run correlate; matched-by-name has nothing to hash.
    """
    mid = spec.get("machine_id")
    return f"SHARED ref={server_ref(mid)}" if mid else "SHARED ref=? (matched by name)"


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


def a_codec(lines, expected, min_width, size=None):
    """What the DEMUXER found: the codec, and how big the picture is.

    `size` ("1920x1080") is an EXACT assertion and is what the resolution x codec matrix
    (LG App Self Checklist #50/#51) actually grades — `min_width` cannot tell 720x480 from
    720x576, and a matrix answered with "at least 1900 wide" is the "pieces are covered" answer
    that item is asking us to stop giving. Read out of `AVCodecParameters::width/height`, i.e.
    the CROPPED dimensions the container declares, so 1080 is 1080 and not the coded 1088.
    The two are independent: a case may declare either, both, or neither.
    """
    cs = codec_ids(lines)
    if not cs:
        return False, "no `ff: v=#0 codec=` line found"
    name, w, h, ln = cs[0]
    ok = (name == expected) and (w >= min_width)
    want = f"{expected} w>={min_width}"
    if size:
        ok = ok and (f"{w}x{h}" == size)
        want = f"{expected} {size}"
    return ok, f"codec={name} {w}x{h} (want {want}) :: {ln.strip()}"


def a_no_error(lines):
    for ln in lines:
        if "smp_cb type=18" in ln or "Playing error" in ln:
            return False, f"error surfaced :: {ln.strip()}"
    return True, "no `smp_cb type=18` / `Playing error`"


def a_video_bound(lines):
    ln = find(lines, "setMediaVideoData sent")
    return (ln is not None), (ln.strip() if ln else "no `setMediaVideoData sent` (video plane never bound)")


def a_timeline_climb(lines, min_climb, dense_only=False):
    """Media position advanced by >= min_climb seconds. Read from the densest signal available.

    This is the floor on every case and it is a real one: playback is 1x realtime, so a 15s
    climb can never cost less than 15s of wall clock. What the dense signal removes is only
    the SAMPLING tax on top of it.

    `dense_only` refuses the 10 s `/:/timeline` fallback, and the synthetic tier passes it: a
    URL-fed playback has no ratingKey, so the reporter thread is never spawned and the fallback
    can never fire there. Silently accepting an absent dense signal is how a broken climb
    assertion reads as a pass, so that tier says out loud that it will not.
    """
    dense = playpos_secs(lines)  # scanned once — evaluate() now runs twice a second
    ts = dense if dense_only else (dense or timeline_secs(lines))
    if len(ts) < 2:
        return False, f"only {len(ts)} media-position sample(s); need >=2 that climb"
    lo = min(t for t, _ in ts)
    hi = max(t for t, _ in ts)
    ok = (hi - lo) >= min_climb
    src = "heartbeat pos=" if dense else "timeline t="
    # The RATE rides along, reported on every case and asserted by none of them: a climb bound
    # says the film got somewhere, and says nothing about how long it took. `playback_rate`'s
    # docstring has the mechanism — a reserve cannot see a slow film, because the reserve is
    # measured against the playhead that slowed. Absent on the sparse fallback, which is 10 s
    # apart and cannot carry a rate.
    rate = ""
    if dense:
        mean_pm, worst_pm, _n, legs = playback_rate(lines)
        if mean_pm is not None:
            rate = f"; {mean_pm}pm of real time (worst 10s window {worst_pm}pm over {legs} leg(s))"
    return ok, f"{src} {lo}s..{hi}s (climb {hi-lo}s, need >={min_climb}s) over {len(ts)} samples{rate}"


def a_play_rate(lines, floor_pm):
    """The film ran at speed. `floor_pm` is per mille of real time; 1000 is exact.

    Declared per case rather than defaulted, because the honest floor is a property of what the
    case is doing to the link: a profile that deliberately collapses the link to below the ladder
    floor MUST run slow, and a case on a link that carries the rung must not. There is no one
    number, and a default here would be a guess wearing a bound.

    Reads the worst window rather than the mean: a film that crawls for thirty seconds and then
    replays at 2x to catch up has a mean of 1000 and is exactly the defect.
    """
    mean_pm, worst_pm, beats, legs = playback_rate(lines)
    if worst_pm is None:
        return False, f"no usable media-clock series ({beats} beat(s), {legs} leg(s))"
    return worst_pm >= floor_pm, (
        f"worst 10s window {worst_pm}pm of real time (mean {mean_pm}pm over {legs} leg(s), "
        f"{beats} beats), want >= {floor_pm}pm")


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


# ---- pipeline-tier assertions ----
#
# Four, and each exists because the integration-tier assertion next to it asks a question the
# pipeline tier cannot answer, or fails to ask one it must.
def a_stream_path(lines, fixture, hls_entry=False):
    """The demuxer opened THIS case's stream.

    `hls_entry` is a case that starts in HLS (`auto_network.start_hls`): the first thing opened is
    the fixture server's own ABR playlist, never the clip the case names, so the fixture filename
    is the wrong thing to compare. What still has to hold is the property this assertion exists
    for — that the stream came from THIS case's fixture root and not from a stale
    `plxnative-play=<rk>` pointing at a library item — and `/__abr/<rung>/master.m3u8` says so.

    `a_decision` cannot be reused and must not be relaxed to cover this: it classifies the opened
    path as DIRECT PLAY or TRANSCODE by matching `/library/parts/` or `/transcode/`, and
    `continue`s on anything else — so a local path falls through both arms and off the end as
    "no line found". Its two substrings are the whole content of the direct-play-vs-transcode
    decision and are exactly right for the tier that grades one.

    What this catches instead is a case grading a stream it was never pointed at: a stale
    `plxnative-play=<rk>` from a by-hand session fires from its own branch in `app.rs` and would
    play a LIBRARY ITEM through a pipeline case. `apply_triggers` glob-wipes before every case, so
    that needs a hand-armed trigger to reach — but the failure is silent and the assertion is one
    line.
    """
    for ln in lines:
        m = RE_STREAM_PATH.search(ln)
        if m:
            path = m.group(1).split("?", 1)[0]
            got = os.path.basename(path)
            if hls_entry:
                ok = got == "master.m3u8" and "/__abr/" in path
                return ok, f"opened {path!r} (want the case's own /__abr/…/master.m3u8) :: {ln.strip()}"
            return (got == fixture), f"opened {got!r} (want {fixture!r}) :: {ln.strip()}"
    return False, "no `stream: ... path=` line — the demuxer never opened anything"


def a_load_decl(lines, exp):
    """The Load payload declared what this case said it would.

    THE assertion that makes the declaration path mean anything, and the reason is a false PASS
    waiting to happen: the engine maps an unrecognised audio codec through a `_ =>` arm to `"AC3"`
    and a non-"hevc" video codec to the H264 payload — so a trigger that never got read at all
    (unplumbed, misparsed, wiped by a stale glob) produces EXACTLY the right payload for the AC-3
    baseline case, which then passes and proves nothing. That is why the tier carries cases whose
    expected `load_audio` is "AC3 PLUS" and "AAC": they cannot be reached by accident.
    """
    hit = next((m for ln in lines for m in [RE_LOAD.search(ln)] if m), None)
    if hit is None:
        return False, "no `load:` line — nothing was declared (or the binary predates it)"
    line = hit.string.strip()
    v, a, fps, present, prof, blc, _el, atmos = hit.groups()
    # "P8/1" — profile/bl_compat, or "none". One token because the two are only ever meaningful
    # together: a profile with the wrong compatibility id describes a different file.
    dv = "none" if present == "0" else f"P{prof}/{blc}"
    checks = [("load_video", "v", v), ("load_audio", "a", a), ("load_dovi", "dv", dv),
              ("load_atmos", "atmos", atmos == "1")]
    got = []
    for key, label, actual in checks:
        if key not in exp:
            continue
        want = bool(exp[key]) if key == "load_atmos" else exp[key]
        got.append(f"{label}={actual!r}")
        if actual != want:
            return False, f"declared {label}={actual!r}, want {want!r} :: {line}"
    if "load_fps" in exp:
        # The float the conversion started from. Tolerance is what three decimals can express.
        want_fps = float(exp["load_fps"])
        got.append(f"fps={fps}")
        if abs(float(fps) - want_fps) > 0.005:
            return False, f"declared fps={fps}, want {want_fps:.3f} :: {line}"
    if "load_fps_rational" in exp:
        # ...and the RATIONAL it produced, which is the thing the frame-rate fixtures exist for.
        # `engine::fps_rational` has one branch for the 1001-denominator broadcast rates and
        # another for the integers, and grading only the float above would grade the INPUT to that
        # split — leaving the branch a fixture was built to reach unobserved. It also pins the two
        # copies of the broadcast-rate table (this repo has one in Rust and one in the generator's
        # `_rate_arg`) against each other at the only place they can meet: the wire.
        rat = next((m.group(1) for ln in lines for m in [RE_ESINFO.search(ln)] if m), None)
        if rat is None:
            return False, f"no `esInfo: videoFps` line — the rate never reached the payload :: {line}"
        got.append(f"esInfo={rat}")
        if rat != exp["load_fps_rational"]:
            return False, f"esInfo videoFps {rat}, want {exp['load_fps_rational']} :: {line}"
    if not got:
        return False, f"case declares no load_* expectation to grade :: {line}"
    return True, f"declared {' '.join(got)} :: {line}"


def a_audio_lane(lines, want_idx):
    """The demuxer fed the audio stream the declaration named.

    `ff::audio_stream_matching` picks the first audio stream whose codec equals the Load payload's
    audio codec — deliberately, because `av_find_best_stream` picks "highest quality" and on an
    8-track file chose DTS over the AC-3 the payload declared, which leaves the audio ES
    unconfigured and, with audioSync, wedges the video forever. That selection is a pure function
    of the declaration and the file, which is exactly what this tier can vary one at a time: one
    fixture with three audio codecs, two cases declaring different ones, two different lanes.
    Nothing in the integration tier isolates it — there the declaration comes from the same PMS
    metadata the file was scanned into, so the two can only ever agree.
    """
    hit = next((m for ln in lines for m in [RE_ALANE.search(ln)] if m), None)
    if hit is None:
        return False, "no `ff: v=#0 … a=#` line — no audio lane was chosen"
    got = int(hit.group(1))
    return got == want_idx, (f"fed audio lane a=#{got} (want a=#{want_idx}) :: "
                             f"{hit.string.strip()}")


def a_load_count(lines, want):
    """An adaptive coded-size change must stay inside one Starfish Load/session."""
    got = sum(bool(RE_LOAD.search(ln)) for ln in lines)
    return got == want, f"saw {got} Load declaration(s), want exactly {want}"


# The fallback line carries its whole basis since 2026-08-25 — the reason code, the rate, the
# requirement it was measured against, and the reserve — so this reads the two fields it grades and
# reports the reason, which is the field that says WHICH rule fired.
RE_AUTO_FALLBACK = re.compile(
    r"auto: Original -> HLS (\w+) measured=(\d+)kbps safe=(\d+)kbps need=(\d+)kbps buf=(-?\d+)ms")
RE_ABR_UP = re.compile(r"abr: committed Up to (\d+)kbps (\d+)x(\d+)")
RE_AUTO_RECOVERY_REQUEST = re.compile(r"abr: source sustainable again at (\d+)kbps; requesting Original")
RE_AUTO_RECOVERED = re.compile(r"auto: recovered Original (direct play|remux)")


def a_auto_network_recovery(lines, max_fallback_kbps, min_recovered_kbps):
    """One offline TV session saw Original collapse, HLS carry the film, then Original return.

    `min_recovered_kbps` grades the SOURCE PROBE that justified the return, not a rung the HLS
    ladder had to reach first. Requiring the top rung was the old gate's shape and it measured the
    wrong resource: PMS producing 20 Mbit/s of H.264 says the server can encode, and says nothing
    about whether the link can carry the remux. An upshift is still reported when one happened,
    because it is useful context — but the probe can legitimately fire from a middle rung, and on
    this fixture it usually does (the ladder needs five segments to climb, the probe gate three).
    """
    fallback = next(((i, RE_AUTO_FALLBACK.search(line)) for i, line in enumerate(lines)
                     if RE_AUTO_FALLBACK.search(line)), None)
    if fallback is None:
        return False, "no Original -> HLS fallback line"
    index, match = fallback
    reason, measured, buffered = match.group(1), int(match.group(2)), int(match.group(5))
    if measured > max_fallback_kbps:
        return False, (f"fallback measured {measured}kbps, want <= {max_fallback_kbps}kbps "
                       f"for the shaped leg :: {match.string.strip()}")
    upshifts = [int(m.group(1)) for line in lines[index + 1:]
                for m in [RE_ABR_UP.search(line)] if m]
    requested = next(((i, m) for i, line in enumerate(lines[index + 1:], start=index + 1)
                      for m in [RE_AUTO_RECOVERY_REQUEST.search(line)] if m), None)
    if requested is None:
        return False, (f"Original fell at {measured}kbps/{buffered}ms ({reason}) and HLS reached "
                       f"{max(upshifts, default=0)}kbps, but Original was never requested again")
    source_kbps = int(requested[1].group(1))
    if source_kbps < min_recovered_kbps:
        return False, (f"the recovery probe measured only {source_kbps}kbps "
                       f"(want >= {min_recovered_kbps}) :: {requested[1].string.strip()}")
    recovered = next((m for line in lines[requested[0] + 1:]
                      for m in [RE_AUTO_RECOVERED.search(line)] if m), None)
    if recovered is None:
        return False, "Original was requested after recovery, but the route never committed it"
    return True, (f"Original fell at {measured}kbps/{buffered}ms ({reason}); HLS reached "
                  f"{max(upshifts, default=0)}kbps; source probe {source_kbps}kbps; "
                  f"recovered {recovered.group(1)}")


# The DECISION-INDEPENDENT per-segment line (`ff.rs::log_hls_abr_sample`, plan I0-A). Every
# statistic below is read from THIS and never from `abr: steady`, which the app emits only on
# `Decision::Stay` — so the segments it omits are exactly the ones where the reserve was lowest,
# and a minimum read from it cannot see the trough it is named for. Worse, it is an order
# statistic over a sample whose membership the policy under test controls: a policy that commits
# more often observes less, and reads as an improvement.
RE_ABR_SAMPLE = re.compile(
    r"abr: sample current=(\d+)kbps media=(\d+)kbps net=(\d+)kbps buf=(\S+) "
    r"vbuf=(-?\d+)ms abuf=(\S+) dur=(\d+)ms prod=(\d+)pm n=(\d+) decision=(\S+) "
    r"target=(\d+)kbps"
)
# The re-seed after a fresh controller is built (plan I0-G) and the switch-history state the
# worker starts from (plan I0-H). Both are characterisation surfaces: I0 reports them, I4 and I8
# change what they say.
RE_ABR_SEED = re.compile(
    r"abr: seed rung=(\d+)kbps prior=(\S+) slow=(\d+)kbps fast=(\d+)kbps unc=(\d+)pm n=(\d+) pin=(\S+)"
)
RE_ABR_HISTORY = re.compile(r"abr: history switches=(\d+) since_last=(\S+) advanced=(\d+)ms")

# **The mode comparison, whole** — the one line that answers "why did Auto choose this" for the
# Original/HLS decision, as opposed to the rung decision `abr: steady` covers. Both utilities are
# decomposed because a total explains nothing: "Original lost" is not a diagnosis, "Original lost
# 40 of quality to 60 of transition cost at scale 66pm" is.
RE_ABR_MODE = re.compile(
    r"abr: mode chose=(\S+) why=(\S+) vs_hls=(\d+)kbps scale=(-?\d+)pm "
    r"win\[q=(-?\d+) f=(-?\d+) r=(-?\d+) s=(-?\d+) t=(-?\d+) tot=(-?\d+)\] "
    r"lose\[q=(-?\d+) f=(-?\d+) r=(-?\d+) s=(-?\d+) t=(-?\d+) tot=(-?\d+)\]"
)
MODE_FIELDS = (
    "chose", "why", "vs_hls_kbps", "scale_pm",
    "win_quality", "win_features", "win_risk", "win_server", "win_transition", "win_total",
    "lose_quality", "lose_features", "lose_risk", "lose_server", "lose_transition", "lose_total",
)


def abr_modes(lines):
    """Every Original-vs-HLS comparison the run made, decomposed, in order."""
    return _parsed(RE_ABR_MODE, MODE_FIELDS, lines)

RE_ABR_STEADY = re.compile(r"abr: steady current=(\d+)kbps")
# The two operational guards and the two estimator inputs beside them. A separate regex from
# `RE_ABR_STEADY` above rather than an extension of it: that one is deliberately a one-field prefix
# match used to count decisions, and widening it would couple every existing count to fields that
# move whenever the guards do.
#
# `stable=`/`cool=` were here until 2026-08-28 and are GONE, with the counters they reported (I6,
# N8/N10). A log predating that will not match this regex at all, which is the intended failure:
# `dwell=` is wall clock and `cool=` was a segment count, so silently reading one as the other
# would compare two different quantities across the very change they measure.
RE_ABR_GATES = re.compile(
    r"abr: steady current=(\d+)kbps .*?"
    r"dwell=(\d+)ms block=(\d+)kbps onrung=(\d+) draining=(\d+) reason=(\S+)"
)
GATE_FIELDS = ("current_kbps", "dwell_ms", "blocked_kbps", "on_rung", "draining", "reason")


def abr_gates(lines):
    """Every `abr: steady` line's guard state, in order.

    These are the only fields on that line that are not an estimator or a derived quantity, and
    they answer the question a counter baseline used to: a `stay` with every `all_good` conjunct
    holding and a non-zero `dwell=` or `block=` is a climb the evidence supported and a guard
    declined. Both are now durations and rates rather than sample counts, so the same reading works
    whatever the segment size turns out to be.
    """
    return _parsed(RE_ABR_GATES, GATE_FIELDS, lines)
# `declared` is the CANDIDATE RENDITION'S OWN RATE, off its master playlist -- the only per-rung
# rate this app can obtain that is not the catalog's `expected_wire_kbps` (the input the plan's R1
# killed: +5.2% to +31.6% error, item-dependent, and non-injective). `-1` on every exit path that
# never fetched a master, which is not zero. Differencing it against `to_kbps` on a captured trace
# is the catalog's error, measured, with no extra instrumentation.
#
# `graded_bytes` pairs with `graded=` to make the ONE observation a transaction adds to the
# controller's acquisition window replayable. Without it a trace cannot reconstruct that window:
# every `abr: window` line describes a CURRENT-stream segment, and `observe_candidate` adds a
# sample none of them mentions -- which reads to a replayer as the app miscounting.
#
# The whole transaction, one line per proposal on every exit path. `decided` is the DECISION cost
# and `feed` is the post-commit backpressure that used to be inside it; `control` is the sum of
# `prime` + `master` + `media`, which are three separate requests and move for different reasons.
# Every Option field prints `none` rather than a zero, because "not reached" and "took no time"
# are different facts and a zero cannot say which.
_MS = r"(-?\d+|none)"
RE_ABR_TX = re.compile(
    r"abr: tx (\w+) (\d+)->(\d+)kbps outcome=(\S+) "
    r"decided=" + _MS + r"ms total=(-?\d+)ms control=" + _MS + r"ms "
    r"prime=" + _MS + r"ms master=" + _MS + r"ms media=" + _MS + r"ms "
    r"warmup=" + _MS + r"ms graded=" + _MS + r"ms warmup_dl=" + _MS + r"ms "
    r"buf_start=" + _MS + r"ms buf_decided=" + _MS + r"ms feed=" + _MS + r"ms "
    r"buf_fed=" + _MS + r"ms buf_end=" + _MS + r"ms cur_acq_before=(-?\d+)ms "
    r"net=(\d+)kbps fast=(\d+)kbps slow=(\d+)kbps unc=(\d+)pm declared=(-?\d+)kbps "
    r"graded_bytes=(-?\d+)"
)
TX_FIELDS = (
    "direction", "from_kbps", "to_kbps", "outcome",
    "decided_ms", "total_ms", "control_ms", "prime_ms", "master_ms", "media_ms",
    "warmup_ms", "graded_ms", "warmup_dl_ms", "buf_start_ms", "buf_decided_ms", "feed_ms",
    "buf_fed_ms",
    "buf_end_ms", "cur_acq_before_ms", "net_kbps", "fast_kbps", "slow_kbps", "unc_pm",
    "declared_kbps", "graded_bytes",
)

# One line per acquired segment. `open_ms` is the successful open only (a NotReady retry is
# counted by `not_ready` instead), `ttfb_ms` is open-to-first-body-byte -- which for a JIT
# encoder IS the production term, and is -1 when no byte ever arrived.
RE_HLS_SEGMENT = re.compile(
    r"hls: segment=(\d+) bytes=(\d+) raster=(\d+)x(\d+) v=(\d+) a=(\d+) "
    r"tail_skew_ms=(-?\d+) audio_pts_recovered=(\d+) not_ready=(\d+) "
    r"open_ms=(\d+) ttfb_ms=(-?\d+) open_probe_ms=(\d+) first_au_ms=(\d+) total_ms=(\d+)"
)
SEGMENT_FIELDS = (
    "sequence", "bytes", "width", "height", "video_packets", "audio_packets",
    "tail_skew_ms", "audio_pts_recovered", "not_ready",
    "open_ms", "ttfb_ms", "open_probe_ms", "first_au_ms", "total_ms",
)


def _parsed(pattern, fields, lines, numeric=True):
    """Every matching line as a dict keyed by `fields`. `none` becomes None, never 0."""
    out = []
    for line in lines:
        m = pattern.search(line)
        if not m:
            continue
        row = {}
        for name, raw in zip(fields, m.groups()):
            if raw == "none":
                row[name] = None
            elif numeric and (raw.isdigit() or (raw[:1] == "-" and raw[1:].isdigit())):
                row[name] = int(raw)
            else:
                row[name] = raw
        out.append(row)
    return out


# ---------------------------------------------------------------------------
# The LINK CONDITIONER — a real server over a link this harness controls.
#
# The synthetic tier shapes its own fixture server, which is why it can grade a rung. The server
# tier could not: it plays a real item off a real PMS over whatever the LAN happens to be doing,
# so an Auto assertion there would grade the afternoon. `tools/netcond.py` closes that — one
# shared token bucket, live-switchable under an open transfer — and this is its lifecycle.
#
# **The binary decides whether any of this is reachable, and the harness can only read that.**
# The app's primary server is `plex_run(PMS_HOST, PMS_PORT)` (`src/main.c`) plus the injected
# `plxnative-token`; `plxnative-servers` is strictly ADDITIVE and cannot move the primary. So the
# link is conditioned only if the DEPLOYED BINARY was built with `PMS_PORT` pointing at the proxy
# rather than at the server. When it was not, a case that declares `link_profile` SKIPS with that
# reason spelled out — it must never run unconditioned and report a pass, because "Auto held a
# rung it could afford" and "Auto was never squeezed" produce the same green.
# ---------------------------------------------------------------------------
CONFIG_LOCAL_H = os.path.join(REPO_ROOT, "src", "config.local.h")


def compiled_pms_endpoint():
    """`(host, port)` the deployed binary talks to, or `(None, None)`.

    Read from the gitignored `src/config.local.h`, which is the ONLY statement of it — `app.h`
    includes that file when present and falls back to a placeholder host on port 32400.
    """
    try:
        with open(CONFIG_LOCAL_H, encoding="utf-8") as fh:
            text = fh.read()
    except OSError:
        return None, None
    host = re.search(r'#define\s+PMS_HOST\s+"([^"]*)"', text)
    port = re.search(r"#define\s+PMS_PORT\s+(\d+)", text)
    return (host.group(1) if host else None, int(port.group(1)) if port else None)


class LinkConditioner:
    """Owns one `tools/netcond.py` for the run, and steers it per case.

    Started ONCE for the whole server tier rather than per case, because the binary points at its
    port unconditionally: an unconditioned case still has to reach the PMS through it. It runs in
    `pass` until a case names a profile, and is returned to `pass` when that case ends — a mode
    left armed would silently shape every case after it, which is the same class of bug as a dev
    trigger left behind.

    An ALREADY-RUNNING proxy on that port is adopted for forwarding and refused for steering: we
    cannot know its control file or its allowlist, and writing to the wrong one conditions nothing
    while reporting that it did. `usable` is what a case's skip decision reads.
    """

    def __init__(self, listen, target, allow_client, control=None):
        self.listen, self.target, self.allow_client = listen, target, allow_client
        self.control = control or os.path.join(
            tempfile.gettempdir(), f"netcond-run-{os.getpid()}.mode")
        self.proc = None
        self.usable = False
        self.why = "not started"
        self.legs = []          # [(monotonic, mode)] — what was applied, WHEN
        self._timer = None
        self._stop = threading.Event()

    def start(self):
        if self.listen is None:
            self.why = ("the deployed binary names no PMS port — src/config.local.h is missing "
                        "or has no PMS_PORT")
            return
        # netcond binds 0.0.0.0, so "direct" is decided by the PORT alone: the compiled host IS
        # the PMS host by construction (`self.target[0]` is where both come from). The second
        # conjunct used to be `self.target[0] == self.host_of_listen()`, and `host_of_listen`
        # returned `self.target[0]` — `x == x`, a named method and a clause that read as a real
        # guard and expressed nothing.
        if self.listen == self.target[1]:
            self.why = (f"the binary talks to the server DIRECTLY on :{self.listen}; rebuild with "
                        f"PMS_PORT set to a proxy port to condition the link")
            return
        with open(self.control, "w") as fh:
            fh.write("pass")
        argv = [sys.executable, os.path.join(REPO_ROOT, "tools", "netcond.py"),
                "--listen", str(self.listen),
                "--target", f"{self.target[0]}:{self.target[1]}",
                "--control", self.control, "--mode", "pass"]
        if self.allow_client:
            argv += ["--allow-client", self.allow_client]
        self.proc = subprocess.Popen(argv, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                                     text=True, start_new_session=True)
        # A bind failure is immediate and is the ONE outcome that must not be read as success:
        # the app still reaches the PMS through whoever holds the port, so every case passes and
        # every conditioned case grades an unshaped link.
        time.sleep(1.0)
        if self.proc.poll() is not None:
            err = (self.proc.stderr.read() or "").strip().splitlines()
            self.proc = None
            self.why = (f"could not bind :{self.listen} — another netcond is probably already "
                        f"running; stop it so this run can steer one"
                        + (f" ({err[-1]})" if err else ""))
            return
        self.usable = True
        self.why = ""

    def arm(self, profile, started_at):
        """Apply `profile` — a list of `{"at_s": <offset>, "mode": "<netcond mode>"}` — from
        `started_at` (the app's first log line). Leg 0 is applied immediately at that instant.

        A thread rather than inline sleeps because `stream_case` owns the foreground for the whole
        case; and offsets are from PLAYBACK start, so a leg lands the same distance into the film
        however long the launch took.
        """
        self.disarm()
        self._stop = threading.Event()
        legs = sorted(profile, key=lambda leg: leg.get("at_s", 0))

        def drive():
            for leg in legs:
                wait = started_at + float(leg.get("at_s", 0)) - time.monotonic()
                if wait > 0 and self._stop.wait(wait):
                    return
                if self._stop.is_set():
                    return
                self.apply(leg["mode"])

        self._timer = threading.Thread(target=drive, daemon=True)
        self._timer.start()

    def apply(self, mode):
        with open(self.control, "w") as fh:
            fh.write(mode)
        self.legs.append((time.monotonic(), mode))
        print(f"    link: {mode}", flush=True)

    def disarm(self):
        """Stop the schedule and put the link back. Called at the END of every conditioned case."""
        self._stop.set()
        if self._timer:
            self._timer.join(timeout=2)
            self._timer = None
        if self.usable and self.legs and self.legs[-1][1] != "pass":
            self.apply("pass")

    def close(self):
        self.disarm()
        if self.proc:
            try:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGTERM)
            except (ProcessLookupError, PermissionError):
                pass
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
            self.proc = None
        try:
            os.unlink(self.control)
        except OSError:
            pass


def early_exit_allowed(case, cfg):
    """(may this case stop as soon as every assertion holds, why not) -- the NON-monotone check.

    Early exit is sound only because assertions are monotone once satisfied: more time cannot
    un-satisfy them. Two kinds of bound break that and must run the full cap.

    `gst_trace`: GST_DEBUG_FILE is not tailed by run-stream, so those assertions can only be
    graded after the run; an event-log-only partial grade must not end the case first.

    `abr_shape`: its `max_commits` bound COUNTS EVENTS over whatever window it was given, so
    stopping early does not merely observe less -- it scores lower, in the passing direction. The
    same binary scored 7 rung changes on a full window while PASSING a 5-bound on an early-exit
    run of the same case (I1, 2026-08-26), then 8 on a later full window. A bound whose value
    depends on when grading stopped cannot be graded early in either direction.

    `link_profile`: a case that conditions the link has LEGS, and the interesting one is never
    the first. Stopping as soon as the opening assertions hold ends the case before the squeeze
    it exists for — observed on the first run of `auto_link_squeeze`, which passed in 70 s of a
    150 s profile whose second leg starts at 50 s.

    `min_play_rate_pm`: the same shape from the other side. It grades the WORST window of the
    media clock, so it is satisfied by every prefix that has not met the slow leg yet, and a case
    that stopped early would report the rate of its healthy opening. Both tiers ask it now, which
    is why `run_case` calls this function too -- it did not until 2026-08-27, and the server cases
    are exactly where a slow film was reported.
    """
    exp = case.get("expect") or {}
    if cfg.get("no_early"):
        return False, ""                      # the operator already said so; no need to explain
    if case.get("gst_trace"):
        return False, "gst_trace assertions are graded only after the run"
    if "abr_shape" in exp:
        return False, "abr_shape carries a commit COUNT, which is window-length sensitive"
    if "min_play_rate_pm" in exp:
        return False, "min_play_rate_pm grades the WORST window, which a prefix cannot settle"
    if case.get("link_profile"):
        return False, "a link_profile has legs; the last one must actually run"
    return True, ""


# **The section 4 admission rule's shadow verdict, one line per segment.** It decides nothing in
# the app -- the whole point of the increment that added it is that the rule can be graded against
# the estimators beside it on real device traces before anything is moved onto it.
#
# A line of its own rather than fields appended to `abr: sample`, so a trace taken with the shadow
# running is still parsed byte-for-byte by `RE_ABR_SAMPLE` and is therefore comparable against a
# baseline captured before it existed. Do NOT fold these fields into that regex.
#
# `verdict=filling` is the ordinary state for the first `n` segments of every playback, and
# `demand`/`supply`/`excess`/`bound` are `-1` there -- "not computed", not zero.
#
# `reset` is a cumulative, monotone count of window resets (one per delivery collapse). It is what
# makes `have` dropping back to 1 mid-playback ATTRIBUTABLE: without it, a legitimate regime-change
# reset and a window that lost its history for some other reason are the same two lines.
RE_ABR_WINDOW = re.compile(
    r"abr: window current=(\d+)kbps verdict=(\S+) have=(\d+)/(\d+) eps=(\d+)pm clamp=(\d+) "
    r"bound=(-?\d+)ms demand=(-?\d+)ms supply=(-?\d+)ms excess=(-?\d+)ms "
    r"sus=(\d+) sur=(\d+) reset=(\d+) bytes=(\d+) dur=(\d+)ms"
)
WINDOW_FIELDS = (
    "current_kbps", "verdict", "have", "want", "eps_pm", "clamp",
    "bound_ms", "demand_ms", "supply_ms", "excess_ms",
    "sustainable", "survivable", "resets", "bytes", "dur_ms",
)


def abr_windows(lines):
    """Every `abr: window` as a dict, in order. One per segment, whatever was decided."""
    return _parsed(RE_ABR_WINDOW, WINDOW_FIELDS, lines)


def abr_transactions(lines):
    """Every `abr: tx` as a dict. One per proposal, commit or reject.

    **A line that does not match is REPORTED, not dropped in silence.** `RE_ABR_TX` names the
    current field set exactly, so a capture from an earlier instrumentation generation fails to
    match it entirely — and the corpus under `docs/measurements/` is append-only and spans several.
    That is not hypothetical: `decided=` meant a different quantity before the transaction leg
    split (it included the post-commit feed), and reading the two generations as one distribution
    is what produced the retracted "true upshift cost is 9 563 ms" in
    `docs/measurements/i2-transaction-cost.md` — and, on 2026-08-27, a `Down` summary that
    understated its own headline. Silence here reads as "there were no transactions", which is
    indistinguishable from a feature that never ran.
    """
    rows = _parsed(RE_ABR_TX, TX_FIELDS, lines)
    seen = sum(1 for line in lines if "abr: tx " in line)
    if seen > len(rows):
        print(f"note: {seen - len(rows)} of {seen} `abr: tx` line(s) did not match the current "
              "field set — an older instrumentation generation, excluded from every statistic "
              "below", file=sys.stderr)
    return rows


def hls_segments(lines):
    """Every `hls: segment` as a dict. One per acquired segment, both streams during a
    transaction -- so a caller separating current from candidate must do it by sequence."""
    return _parsed(RE_HLS_SEGMENT, SEGMENT_FIELDS, lines)

RE_ABR_COMMIT = re.compile(r"abr: committed (Up|Down) to (\d+)kbps (\d+)x(\d+)")
# The DECODED raster, appended to the same line since 2026-08-28. Separate pattern rather
# than a widened `RE_ABR_COMMIT` so a log predating it still parses as it always did — and
# so a reader cannot mistake the bounding box for an observation by reading one regex.
RE_ABR_COMMIT_OUT = re.compile(
    r"abr: committed (?:Up|Down) to \d+kbps \d+x\d+ out=(\d+)x(\d+)")


def abr_samples(lines):
    """Every `abr: sample` as a dict, in order. One per segment, whatever was decided.

    `at` is the harness monotonic clock when the line ARRIVED, present only when `lines` came from
    `stream_case` (a `StampedLines`); `None` otherwise, and every consumer treats that as "cannot
    be placed on the shaper's timeline" rather than as zero.

    `fetch_ms` is derived, not logged: `media x dur / net` is the transfer's own duration, which
    with `at` gives the SPAN a segment occupied. That span is what gets intersected with an
    injected shaper leg.
    """
    stamps = getattr(lines, "stamps", None)
    out = []
    for i, line in enumerate(lines):
        m = RE_ABR_SAMPLE.search(line)
        if not m:
            continue
        media, net, dur = int(m.group(2)), int(m.group(3)), int(m.group(7))
        out.append({
            "current_kbps": int(m.group(1)), "media_kbps": media,
            "net_kbps": net,
            # `buf=` joined `abuf=` in being optional: the app prints `none` when the playable
            # reserve is not knowable this segment (an A/V session whose audio lane has produced
            # no timestamp since the open or the seek). It used to print a fabricated `0`, which
            # every reader here — `abr_min_buf_ms` above all — read as a reserve that hit bottom.
            "buf_ms": None if m.group(4) == "none" else int(m.group(4).rstrip("ms")),
            "vbuf_ms": int(m.group(5)),
            "abuf_ms": None if m.group(6) == "none" else int(m.group(6).rstrip("ms")),
            "dur_ms": dur,
            "prod_pm": int(m.group(8)), "n": int(m.group(9)),
            "decision": m.group(10), "target_kbps": int(m.group(11)),
            "at": (stamps[i] if stamps and i < len(stamps) else None),
            "fetch_ms": (media * dur / net) if net else 0.0,
        })
    return out


def abr_min_buf_ms(samples):
    """The lowest reserve the pipeline actually reached, in ms. `None` with no samples.

    This is the controller-visible playable reserve — `min(video, audio) - playback`, the same
    quantity the decision path used, taken from the same segment. It is not recomputed here and
    there is deliberately no second notion of "buffer" in this harness.

    Segments whose reserve was `none` are EXCLUDED rather than read as zero. That is the same
    decision the controller makes on them (it declines to decide), and it is the difference
    between a `min_buf_ms` bound that grades the reserve and one that grades how often the audio
    lane was quiet — the latter fires hardest on exactly the first segment after every seek.
    """
    return min((s["buf_ms"] for s in samples if s["buf_ms"] is not None), default=None)


def abr_binding_lane(samples):
    """Which lane held the reserve down, counted over the run: 'video', 'audio' or 'n/a'.

    `buf = min(video, audio)`, and which one binds moves with the rung — the 8 MiB video queue
    against a multi-Mbit stream versus the 1 MiB audio queue at ~192 kbps. A `buf=` alone cannot
    say which ceiling was hit, and the answer decides whether a measured reserve confirms or
    refutes the model in `docs/adaptive-playback-plan.md` §0.1.
    """
    video = sum(1 for s in samples if s["abuf_ms"] is None or s["vbuf_ms"] <= s["abuf_ms"])
    audio = sum(1 for s in samples if s["abuf_ms"] is not None and s["abuf_ms"] < s["vbuf_ms"])
    if not samples:
        return "n/a"
    return f"video {video}/{len(samples)}, audio {audio}/{len(samples)}"


def abr_dip_max_kbps(samples, dip_windows):
    """The highest media rate the pipeline SUSTAINED while the injected link was degraded.

    Returns `(kbps, note)` or `(None, reason)`.

    Two independence rules, and the metric is worthless without either:

    * The WINDOW comes from the shaper's own schedule (`FixtureServer.dip_windows`), never from
      the app's observations. An earlier version of this found the dip by looking for samples
      whose delivered rate fell below half the run's peak — which put an uncalibrated 0.5 in the
      middle of a measurement, and made the window a function of the behaviour being measured.
    * The VALUE is observed delivery (`media=`, bytes over content duration), never the rung the
      controller chose. A metric derived from the decision under test cannot grade it.

    A sample counts when its TRANSFER SPAN — `[at - fetch_ms, at]` — intersects a degraded leg,
    which is the "overlapping" sense: a segment half of which crossed the bad link was affected by
    it. `at` is arrival at the harness and so lags the app by the ssh hop, always in the same
    direction; the note reports how many samples landed in the window so a suspicious count is
    visible rather than silently changing the answer.
    """
    if not dip_windows:
        return None, "no degraded leg in the injected profile"
    if not samples:
        return None, "no `abr: sample` line to attribute"
    if all(s["at"] is None for s in samples):
        return None, "log lines carry no arrival stamp (not a stream_case run)"
    hit = []
    for s in samples:
        if s["at"] is None:
            continue
        span = (s["at"] - s["fetch_ms"] / 1000.0, s["at"])
        for a, b, _ in dip_windows:
            if span[1] >= a and (b is None or span[0] <= b):
                hit.append(s)
                break
    if not hit:
        return None, f"no segment overlapped the {len(dip_windows)} degraded leg(s)"
    legs = "/".join(str(k) for _, _, k in dip_windows)
    return max(s["media_kbps"] for s in hit), f"{len(hit)} segment(s) over leg(s) {legs}kbps"


# **Above this a forward jump is a RELOCATION, not a delivery.** A seek, a reload or an Up Next
# advance moves the media clock by minutes; a queue running dry moves it by one segment. The
# fixtures' segments are 2 s and the longest legitimate lump is therefore bounded by the largest
# segment duration any fixture uses, not by a taste about judder — `hls: segment` carries the real
# figure and nothing in either pack exceeds 4 s. Ten leaves room for a pack with longer segments
# while staying far below the smallest seek any case performs (`plxnative-autoseek`'s default is
# 140 s, and the shortest scripted step is 10 s ABSOLUTE from a position well past it).
LUMP_SEEK_S = 10


def _runs(series, is_bad):
    """`(runs, hits)` — the lengths of every maximal run of CONSECUTIVE bad beats, and the total.

    `abr_stalls` and `playback_lumpiness` ask the same question of the same `pos=` series and
    differ only in what makes a beat bad, so the walk is here once. Both had their own copy,
    including the trailing flush — the half that is easy to fix in one and forget in the other.
    """
    runs, cur, hits = [], 0, 0
    for before, after in zip(series, series[1:]):
        if is_bad(before, after):
            cur += 1
            hits += 1
        elif cur:
            runs.append(cur)
            cur = 0
    if cur:
        runs.append(cur)
    return runs, hits


def abr_stalls(lines):
    """`(max_continuous_s, total_s, samples)` of REAL playback stall, from the 1 Hz `pos=` series.

    A stall is the media clock failing to advance between two consecutive heartbeats. It is NOT
    the starvation horizon, not `buffered < threshold`, and not the controller's `starving()`
    boolean — those are the model's opinion about the future, and this is what happened.

    RESOLUTION IS +/-1 s AND CANNOT BE BETTER FROM THIS SOURCE: `pos=` is integer seconds on a
    once-per-second heartbeat (`RE_POS`), so a sub-second gap is invisible and a 1 s reading may be
    rounding. Quote it as a floor on stall duration, never as a precise one.
    """
    series = [p for p, _ in playpos_secs(lines)]
    if len(series) < 2:
        return None, None, len(series)
    runs, _ = _runs(series, lambda before, after: after <= before)
    return (max(runs) if runs else 0), sum(runs), len(series)


def playback_lumpiness(lines):
    """`(lumpy_beats, longest_run, beats)` — the clock arriving in SEGMENT-SIZED lumps.

    A third failure shape, and the only one of the three that every other metric on this line
    reads as healthy. `abr_stalls` grades the clock STOPPING and `playback_rate` grades it
    advancing too SLOWLY; this grades it advancing in the right amount at the wrong granularity —
    `2,0,2,0,2,0` instead of `1,1,1,1,1,1`. The mean rate over that is exactly 1000 pm, no beat
    is a stall longer than one, and the picture is visibly juddering.

    Device-measured (`pipe_abr_down_outrun`, 2026-08-28): after three downshift attempts drained
    the reserve to 168 ms, the media clock ran `123 125 125 127 129 129 131 133 133 …` for ~30
    beats. `abr_stalls` scored max=8 total=13 — all of it the earlier true stall — and
    `playback_rate` scored ~1000 pm. Both were right and both were blind: playback was running
    straight off the network one 2 s segment at a time, because the queue was empty and each
    arrival advanced the clock by a whole segment.

    A beat is LUMPY when the clock moved by 2 s or more in one 1 Hz beat — i.e. it delivered at
    least a whole extra segment's worth in the time one second of media should have taken. That
    threshold is not a tuning knob: 2 is the smallest integer step this ±1 s source can
    distinguish from real time at all (a true 1 s advance may read 0 or 1, never 2).

    A seek moves the clock discontinuously and is not lumpiness; `playback_rate`'s leg splitting
    is the wrong tool here because a lump IS a forward step, so this instead ignores any step
    larger than `LUMP_SEEK_S`, above which a jump is a relocation rather than a delivery.
    """
    series = [p for p, _ in playpos_secs(lines)]
    if len(series) < 2:
        return None, None, len(series)
    runs, lumpy = _runs(series, lambda before, after: 2 <= after - before <= LUMP_SEEK_S)
    return lumpy, (max(runs) if runs else 0), len(series)


def playback_rate(lines, window=10):
    """Media seconds per WALL second — whether the film is running at speed.

    Returns `(mean_pm, worst_pm, beats, legs)` in per mille of real time, or `(None, None, n, 0)`
    when there is not enough of a series to say. 1000 is real time; 670 is the picture crawling.

    **This is the only thing here that can see a slow film.** Every other metric on this line is a
    RESERVE, and a reserve is media time measured against the playhead — so when the playhead
    itself slows down, the reserve stops draining, `slope` goes quiet, and `min_buf_ms` reads
    healthy while the picture runs at two thirds speed. `max_stall_s` cannot see it either: a
    stall is the clock not advancing at all, and this is the clock advancing too slowly.
    Measured on the corpus 2026-08-27: `pipe_abr_band_20000` holds 670 for ~30 s with `buf`
    parked at 2.2 s, `slope` decaying to −29 ms/s and every buffer signal reading fine.

    The wall clock is the HEARTBEAT COUNT: `app.rs`'s `loop_tick` emits one line per wall second
    whatever the frame rate, so beats are seconds. Arrival stamps are used instead when `lines`
    carries them (a live `StampedLines`), because they measure wall time directly and survive a
    beat the ssh hop dropped; a log read back off disk has none and falls back to the count.

    RESOLUTION: `pos=` is INTEGER seconds (`RE_POS`), so a single beat is ±1 s and only a window
    is meaningful. Over `window` beats the quantization is ±1000/(window−1) pm — ±111 pm at the
    default 10 — which is why nothing shorter is reported and why a bound below ~900 is the only
    kind this can carry honestly.

    A seek or a reload moves the media clock discontinuously and is NOT a rate. Legs are split on
    any backward step, and on a forward step larger than the wall gap plus one second of slack;
    each leg is measured on its own and legs shorter than `window` are dropped.
    """
    stamps = getattr(lines, "stamps", None)
    beats = []
    for i, ln in enumerate(lines):
        m = RE_POS.search(ln)
        if m:
            beats.append((int(m.group(1)),
                          stamps[i] if stamps and i < len(stamps) else float(len(beats))))
    if len(beats) < window:
        return None, None, len(beats), 0
    legs, cur = [], []
    for b in beats:
        if cur:
            d_media, d_wall = b[0] - cur[-1][0], b[1] - cur[-1][1]
            if d_media < 0 or d_media > d_wall + 1.0:
                legs.append(cur)
                cur = []
        cur.append(b)
    legs.append(cur)
    legs = [l for l in legs if len(l) >= window]
    if not legs:
        return None, None, len(beats), 0
    worst, media, wall = None, 0.0, 0.0
    for leg in legs:
        media += leg[-1][0] - leg[0][0]
        wall += leg[-1][1] - leg[0][1]
        for i in range(len(leg) - window + 1):
            a, b = leg[i], leg[i + window - 1]
            span = b[1] - a[1]
            if span <= 0:
                continue
            r = int(round(1000 * (b[0] - a[0]) / span))
            if worst is None or r < worst:
                worst = r
    mean = int(round(1000 * media / wall)) if wall > 0 else None
    return mean, worst, len(beats), len(legs)


def abr_raster_changes(lines):
    """Commits whose raster differs from the previous commit's.

    Eight of the thirteen rungs share 1920x1080 and are eventless to a viewer; four of the twelve
    adjacent steps cross a raster band and are a different class of event. Counting commits alone
    cannot tell those apart.

    **Counts the DECODED raster when the log carries one** (`out=`, appended 2026-08-28), and the
    catalog raster otherwise. That is not a formatting preference — the two disagree, measured:
    against a 4K source PMS produces 1280x720 for BOTH `P720` and `P1080M6`, and against a 1080p
    source it produces 1918x802 for every rung from `P1080M6` up
    (`docs/measurements/m3-production-census.md`). So on the catalog reading a `P720 -> P1080M6`
    commit is a raster change and a viewer sees nothing, while `P1080High -> Uhd` on a 1080p item
    reads as a change into 4K that never happened. A rung's raster is a BOUNDING BOX, not a
    target, and this counter is the plan's device co-grader — it was grading the intent.

    A log with no `out=` falls back to the box and is scored exactly as before, so the two are not
    mixed within one series: whichever the FIRST commit offers is used for all of them.

    **It returns WHICH reading it used, and the caller prints it.** Two series answer to one name
    here, and this file states the doctrine twice elsewhere — a scene "must never grade a loop rate
    as if it were a frame rate", a renamed field "must fail as 'no samples' rather than silently
    match zero". A silent whole-series fallback is the same hazard: `0x0` is not hypothetical
    (`ff.rs` writes it whenever a candidate produced no output, which a stall-aborted one does), so
    a CURRENT build's log can revert to bounding-box semantics with nothing saying so — while the
    failure message's `trail` goes on showing catalog rasters either way.
    """
    observed = [f"{m.group(1)}x{m.group(2)}"
                for line in lines for m in [RE_ABR_COMMIT_OUT.search(line)] if m]
    catalog = [f"{m.group(3)}x{m.group(4)}"
               for line in lines for m in [RE_ABR_COMMIT.search(line)] if m]
    # A decoded 0x0 is "the commit fed nothing measurable", not an observation; fall back whole
    # rather than letting one such entry manufacture two spurious transitions around itself.
    decoded = bool(observed) and len(observed) == len(catalog) and "0x0" not in observed
    rasters = observed if decoded else catalog
    return (sum(1 for a, b in zip(rasters, rasters[1:]) if a != b),
            "decoded" if decoded else "catalog")


def report_and_record(cfg, name, passed, results, lines, elapsed, run_secs, early, settled,
                     verbose):
    """Save the log, print the verdict, print the characterisation. Both tiers, one tail.

    CHARACTERISATION is printed for any case that ran the HLS controller and is never graded:
    these are the observations increment I1 has to record about UNMODIFIED HEAD, and later
    increments are expected to change what they say. Asserting them would pin today's behaviour as
    desirable, which is exactly what I0 must not do.

    The server tier needs it MORE, not less: an `abr: sample` there came from a real PMS ladder
    over a real link, and the app truncates its event log every launch. `--save-logs` was a silent
    no-op on that tier until 2026-08-27, so every server-tier ABR observation ever taken was read
    once off a terminal and then destroyed by the next case. The fix was copied into the second
    runner rather than shared, which set up the same failure one move ahead — the next
    characterisation surface added to one tail and not the other.
    """
    saved = save_case_log(cfg, name, lines)
    if saved:
        print(f"    log saved: {saved} ({len(lines)} lines)")
    report_case(passed, results, elapsed, run_secs, early, settled, verbose)
    notes = abr_characterisation(lines)
    if notes:
        print("    characterisation (recorded, not graded):")
        for note in notes:
            print(f"       {note}")


def abr_characterisation(lines):
    """The baseline observations I1 has to record, as printable text. Never graded.

    * **PLAYBACK HEALTH — whether the film actually ran** (see below; added 2026-08-28);
    * the FIRST segment: its reserve, decision and target (plan §0.3(1) predicts a downshift on
      every playback, on any link, because one segment of reserve trips `starving()`);
    * the controller SEED after a fresh construction, which a seek forces (plan §7.G);
    * the switch-history state and the elapsed time actually passed to its decay (plan §7.H).

    **Why playback health is here rather than only in an assertion's evidence.** `report_case`
    prints an assertion's evidence only when it FAILS (or under `--verbose`), and `max_stall_s` /
    `play_rate_pm` ride on `abr_shape`'s evidence string — so a case that stalled for three
    seconds and still satisfied every bound it declared printed nothing about the stall at all.
    The quantities were computed, returned, and discarded. Most cases declare no stall bound and
    none declares one on the rate, so "passed" has never meant "did not stall".

    That is the same defect as `visited` reading `abr: steady` alone and as a sleeping television
    grading as a regression: an instrument that exists and is not read. A stall is a fact about
    the playback, not an opinion of the model, and no bound may hide one — so it prints on every
    case, pass or fail, and stays UNGRADED for I0's reason (asserting it would pin today's
    behaviour as desirable).
    """
    out = []
    stall_max, stall_total, beats = abr_stalls(lines)
    if stall_max is not None:
        mean_pm, worst_pm, _n, legs = playback_rate(lines)
        rate = "n/a" if mean_pm is None else f"{mean_pm}pm mean / {worst_pm}pm worst over {legs} leg(s)"
        # +/-1 s and a floor, never a precise duration: `pos=` is integer seconds on a 1 Hz
        # heartbeat, so a sub-second stall is invisible. Said at the point of reading.
        # Three shapes, printed together because each is blind to the other two: the clock
        # STOPPING, the clock running SLOW, and the clock arriving in segment-sized LUMPS at a
        # correct mean rate. The third was invisible until 2026-08-28 — `pipe_abr_down_outrun`
        # ran ~30 beats of `2,0,2,0` with max_stall=8 and rate~1000pm, both reading healthy.
        lumpy, lump_run, _ = playback_lumpiness(lines)
        out.append(
            f"playback: max_stall={stall_max}s (total {stall_total}s over {beats} beats, +/-1s) "
            f"rate={rate} lumpy={lumpy} beat(s), longest run {lump_run}"
        )
    samples = abr_samples(lines)
    if samples:
        first = samples[0]
        first_buf = "none" if first["buf_ms"] is None else f"{first['buf_ms']}ms"
        # The count of segments whose playable reserve was not knowable — an A/V session whose
        # audio lane had produced no timestamp. It is reported because it is the ONLY way this
        # tier can say whether that path was reached at all: it decides nothing (the controller
        # declines to decide on those segments), so no assertion moves when it changes, and a
        # zero here means R11's branch is untested on device rather than tested and passing.
        unreadable = sum(1 for s in samples if s["buf_ms"] is None)
        out.append(f"unreadable reserve: {unreadable}/{len(samples)} segment(s)")
        out.append(f"first segment: buf={first_buf} current={first['current_kbps']}kbps "
                   f"decision={first['decision']} target={first['target_kbps']}kbps")
    for m in [RE_ABR_SEED.search(ln) for ln in lines]:
        if m:
            out.append(f"seed: rung={m.group(1)}kbps prior={m.group(2)} slow={m.group(3)}kbps "
                       f"unc={m.group(5)}pm n={m.group(6)} pin={m.group(7)}")
    for m in [RE_ABR_HISTORY.search(ln) for ln in lines]:
        if m:
            out.append(f"history: switches={m.group(1)} since_last={m.group(2)} "
                       f"advanced={m.group(3)}ms")
    return out


def a_abr_shape(lines, spec, dip_windows=()):
    """Grade the RUNGS a shaped link produced, rather than only the Original<->HLS transition.

    `a_auto_network_recovery` above grades one event. This grades the whole trajectory, which is
    what a bad-network profile is actually testing, and it is the only assertion here that can fail
    on the controller being TOO EAGER: a link that carries 4 Mbit/s and a client that spends the
    whole film reaching for 20 both "play", and only a ceiling can tell them apart.

    Four independent bounds, each optional, each answering one question a profile poses:

    * ``ceiling_kbps`` -- no rung above this was ever active. The overreach guard.
    * ``floor_kbps`` -- some rung at or above this WAS active. The under-reach guard: a controller
      that parks on 320 Kbps forever also never stalls, and would pass every other assertion here.
    * ``max_commits`` -- at most this many committed rung changes. The FLAP guard, and the one that
      grades the decaying transition penalty: an oscillating link must not produce a commit per
      oscillation, because each one is a visible quality change to the person watching.
    * ``settle_min_kbps`` / ``settle_max_kbps`` -- where it ENDED. Bounds rather than a value,
      because the ladder has 13 points and a link between two of them may legitimately settle on
      either; a profile that wants an exact rung sets both to it.

    Read from ``abr: steady`` AND from the commits, in log order. Steady alone is not enough and
    steady alone is what this did, which made both bounds under-report.

    ``abr: steady`` is emitted only on ``Decision::Stay`` (``ff.rs``), so a rung the controller
    commits to and then immediately proposes to leave — or commits to near the end of a case —
    produces no steady line at all and was INVISIBLE here. Measured 2026-08-28:
    ``pipe_abr_seek_flat`` logged ``tx Up 2000->10000kbps outcome=committed`` and then ended
    (that transaction alone took 22.4 s, 20.6 s of it feed backpressure), so ``visited`` was
    ``{720, 2000}``, ``max`` was 2000, and ``floor_kbps: 8000`` failed a case that had reached
    10000.

    **The same gap is a FALSE PASS on the other side**, which is the half that matters more:
    ``ceiling_kbps`` is the overreach guard the manifest calls "the assertion no position climb can
    stand in for", and a rung reached and left inside one segment cleared it by not being looked at.

    Commits alone would not do either — the STARTING rung is not a commit, and a case whose whole
    point is "it never moved" would have nothing to read. So it is the union, ordered by position
    in the log, which is what "which rung was the controller on, over time" actually means.
    ``visited[-1]`` is then the last rung it was on, which is what ``settle`` wants.
    """
    marks = []
    for i, line in enumerate(lines):
        m = RE_ABR_STEADY.search(line)
        if m:
            marks.append((i, int(m.group(1))))
        c = RE_ABR_COMMIT.search(line)
        if c:
            marks.append((i, int(c.group(2))))
    visited = [kbps for _, kbps in marks]
    if not visited:
        return False, "no `abr: steady` line — the HLS controller never ran"
    commits = [(m.group(1), int(m.group(2)), f"{m.group(3)}x{m.group(4)}")
               for line in lines for m in [RE_ABR_COMMIT.search(line)] if m]
    trail = " -> ".join(f"{d[0].lower()}{kbps}" for d, kbps, _ in
                        ((c[0], c[1], c[2]) for c in commits)) or "no changes"
    story = (f"rungs {min(visited)}..{max(visited)}kbps, settled {visited[-1]}kbps, "
             f"{len(commits)} change(s): {trail}")

    # The observed metrics (plan I0-A). REPORTED ALWAYS, asserted only where a case names a bound
    # — and no case names one in increment I0, deliberately: these exist so I1 can record what
    # HEAD does, and a bound written before that baseline exists would be a number somebody
    # guessed. Each comes from observed playback, never from the model under test.
    samples = abr_samples(lines)
    min_buf = abr_min_buf_ms(samples)
    dip_kbps, dip_note = abr_dip_max_kbps(samples, dip_windows)
    stall_max, stall_total, beats = abr_stalls(lines)
    rate_mean, rate_worst, _rate_beats, rate_legs = playback_rate(lines)
    rasters, raster_src = abr_raster_changes(lines)
    story += (f" | min_buf_ms={min_buf if min_buf is not None else 'n/a'}"
              f" dip_max_kbps={dip_kbps if dip_kbps is not None else 'n/a'} ({dip_note})"
              f" max_stall_s={stall_max if stall_max is not None else 'n/a'}"
              f" (total {stall_total if stall_total is not None else 'n/a'}s over {beats} beats,"
              f" +/-1s) play_rate_pm={rate_mean if rate_mean is not None else 'n/a'}"
              f"/worst{rate_worst if rate_worst is not None else 'n/a'} over {rate_legs} leg(s)"
              f" raster_changes={rasters} ({raster_src}) lane[{abr_binding_lane(samples)}]"
              f" segments={len(samples)}")
    if not samples:
        story += " | WARNING: no `abr: sample` line — every metric above is blind"

    floor_buf = spec.get("min_buf_ms")
    if floor_buf is not None and (min_buf is None or min_buf < floor_buf):
        return False, f"reserve fell to {min_buf}ms, want >= {floor_buf}ms :: {story}"
    stall_cap = spec.get("max_stall_s")
    if stall_cap is not None and (stall_max is None or stall_max > stall_cap):
        return False, f"stalled {stall_max}s, want <= {stall_cap}s :: {story}"
    # The film ran SLOW. Separate from `max_stall_s` on purpose: that grades the clock stopping,
    # this grades it advancing at the wrong speed, and a reserve-derived metric can see neither.
    rate_floor = spec.get("min_play_rate_pm")
    if rate_floor is not None and (rate_worst is None or rate_worst < rate_floor):
        return False, (f"played at {rate_worst}pm of real time (mean {rate_mean}pm), "
                       f"want >= {rate_floor}pm :: {story}")
    raster_cap = spec.get("raster_changes_max")
    if raster_cap is not None and rasters > raster_cap:
        return False, f"{rasters} raster change(s), want <= {raster_cap} :: {story}"

    ceiling = spec.get("ceiling_kbps")
    if ceiling is not None and max(visited) > ceiling:
        return False, f"reached {max(visited)}kbps on a link graded for <= {ceiling}kbps :: {story}"
    floor = spec.get("floor_kbps")
    if floor is not None and max(visited) < floor:
        return False, f"never reached {floor}kbps on a link that carries it :: {story}"
    cap = spec.get("max_commits")
    if cap is not None and len(commits) > cap:
        return False, f"{len(commits)} rung changes, want <= {cap} :: {story}"
    lo, hi = spec.get("settle_min_kbps"), spec.get("settle_max_kbps")
    if lo is not None and visited[-1] < lo:
        return False, f"settled at {visited[-1]}kbps, want >= {lo}kbps :: {story}"
    if hi is not None and visited[-1] > hi:
        return False, f"settled at {visited[-1]}kbps, want <= {hi}kbps :: {story}"
    return True, story


def a_audio_feed_ready(lines):
    """At least one compressed audio AU reached and was accepted by Starfish."""
    attempts = [m for line in lines for m in [RE_AUDIO_FEED.search(line)] if m]
    accepted = [m for m in attempts if m.group(4) == "O"]
    if accepted:
        last = accepted[-1]
        return True, (f"Starfish accepted audio feed a#{last.group(1)} "
                      f"({last.group(2)} bytes, pts={int(last.group(3)) / 1e9:.3f}s)")
    if attempts:
        replies = "".join(m.group(4) for m in attempts)
        return False, f"audio feed was attempted but no logged AU was accepted (replies={replies})"
    return False, "no `feed a#… reply=O` line — the audio lane never reached Starfish"


def _source_info_shape(node):
    """Find the `video.width/height` pair inside a type-4 JSON envelope."""
    if not isinstance(node, dict):
        return None
    video = node.get("video")
    if isinstance(video, dict):
        try:
            return f"{int(video['width'])}x{int(video['height'])}"
        except (KeyError, TypeError, ValueError):
            pass
    for value in node.values():
        shape = _source_info_shape(value)
        if shape:
            return shape
    return None


def starfish_resolution_sequence(lines):
    """Collapsed WxH sequence from valid Starfish source-info callbacks."""
    got = []
    for line in lines:
        match = RE_SMP_SOURCE_INFO.search(line)
        if not match:
            continue
        try:
            shape = _source_info_shape(json.loads(match.group(1)))
        except json.JSONDecodeError:
            continue
        if shape and (not got or got[-1] != shape):
            got.append(shape)
    return got


def a_starfish_resolution_sequence(lines, want):
    """The decoder itself must observe every in-band coded-size transition."""
    got = starfish_resolution_sequence(lines)
    return got == want, (f"Starfish source-info {' -> '.join(got) if got else '<none>'}; "
                         f"want exactly {' -> '.join(want)}")


def a_no_reload(lines):
    """Reject either reload path even when a later session recovers and otherwise passes."""
    bad = next((ln for ln in lines
                if "reload_at:" in ln or "reload_transcode:" in ln), None)
    return bad is None, ("no reload path entered" if bad is None else
                         f"session reloaded :: {bad.strip()}")


def a_reload_ceiling(lines, limit):
    """Bound the number of fresh `Load`s — the assertion `no_reload` cannot express.

    A mode-switching Auto case legitimately reloads: once to leave Original when the link stops
    covering the source, once to come back when it recovers. Zero is therefore the wrong gate and
    absent is no gate at all, which is the state that let the Original flap ship — the controller
    left and re-entered Original repeatedly on a link that was carrying the film, and every
    existing assertion (climb, play rate, no error) was satisfied throughout, because each
    individual reload is brief and the film keeps advancing.

    A reload is a visible blink: it is a fresh Starfish `Load`, the picture goes and comes back.
    So this counts what the viewer counts.
    """
    reloads = [ln for ln in lines if "reload_at:" in ln or "reload_transcode:" in ln]
    n = len(reloads)
    where = " | ".join(ln.strip()[:90] for ln in reloads[:6]) or "<none>"
    return n <= limit, f"{n} reload(s), ceiling {limit} :: {where}"


def gst_clock_ms(line):
    """GStreamer debug's H:MM:SS.nanoseconds clock as milliseconds, or None."""
    m = RE_GST_CLOCK.search(line)
    if not m:
        return None
    hours, minutes, seconds = (int(m.group(i)) for i in range(1, 4))
    nanos = int(m.group(4).ljust(9, "0"))
    return ((hours * 60 + minutes) * 60 + seconds) * 1000.0 + nanos / 1_000_000.0


def gst_frame_times(lines, pattern):
    """Sorted unique per-picture sink timestamps selected by a case-owned regex."""
    selector = re.compile(pattern, re.IGNORECASE)
    return sorted({stamp for line in lines if selector.search(line)
                   for stamp in [gst_clock_ms(line)] if stamp is not None})


def _nearest_rank(values, percentile):
    if not values:
        return None
    ordered = sorted(values)
    rank = max(1, int((percentile * len(ordered) + 99) // 100))
    return ordered[min(rank, len(ordered)) - 1]


def a_gst_resolution_trace(lines, exp, trace):
    """Bound presentation gaps around the fixture's known content-time transitions.

    This deliberately grades LG's own pipeline clock, not the app's 5 Hz position heartbeat.
    Source-info callbacks grade the exact resolution sequence separately; LG's GST caps logger did
    not repeat the returning 720p caps event on the measured firmware. The fixture owns its 8/16 s
    boundaries, so the first presented-picture clock plus those content offsets is a stronger and
    firmware-independent cadence reference.
    """
    try:
        frames = gst_frame_times(lines, trace["frame_pattern"])
    except (KeyError, re.error) as e:
        return False, f"invalid GST trace selector: {e}"

    if len(frames) < 3:
        return False, f"only {len(frames)} per-picture GST timestamp(s)"

    gaps = [(frames[i - 1], frames[i], frames[i] - frames[i - 1])
            for i in range(1, len(frames))]
    try:
        boundary_times = [frames[0] + float(seconds) * 1000.0
                          for seconds in exp["resolution_boundaries_s"]]
    except (KeyError, TypeError, ValueError) as e:
        return False, f"invalid resolution boundary declaration: {e}"
    window = float(exp.get("gst_boundary_window_ms", 1000))
    first_max = float(exp.get("gst_first_frame_max_ms", 125))
    boundary_gaps = []
    for boundary in boundary_times:
        after = next((t for t in frames if t >= boundary), None)
        if after is None:
            return False, f"no presented picture after content boundary at {boundary:.3f} ms"
        if after - boundary > first_max:
            return False, (f"first picture after {boundary:.3f} ms content boundary took "
                           f"{after - boundary:.3f} ms, limit {first_max:g} ms")
        near = [gap for left, right, gap in gaps
                if right >= boundary - window and left <= boundary + window]
        if not near:
            return False, f"no picture intervals around content boundary at {boundary:.3f} ms"
        boundary_gaps.append(max(near))

    baseline = [gap for left, right, gap in gaps
                if all(right < boundary - window or left > boundary + window
                       for boundary in boundary_times)]
    baseline_p99 = _nearest_rank(baseline, 99)
    if baseline_p99 is None:
        # A short captured trace can consist entirely of the two boundary windows. The declared
        # source rate is still a conservative reference, while the absolute floor remains binding.
        baseline_p99 = 1000.0 / float(exp.get("load_fps", 24.0))
    limit = max(float(exp.get("gst_boundary_gap_floor_ms", 120)),
                baseline_p99 * float(exp.get("gst_boundary_gap_p99_multiplier", 3.0)))
    worst = max(boundary_gaps, default=0.0)
    if worst > limit:
        return False, (f"boundary picture gap {worst:.3f} ms exceeds {limit:.3f} ms "
                       f"(off-boundary p99 {baseline_p99:.3f} ms)")
    return True, (f"GST {len(frames)} pictures; boundary max {worst:.3f} ms <= {limit:.3f} ms "
                  f"(off-boundary p99 "
                  f"{baseline_p99:.3f} ms)")


def a_server_wire(delta, min_opens, min_range, exact_opens=None, exact_range=None):
    """What the fixture SERVER saw — the one assertion no log line can give.

    A seek that never reaches the demuxer's `seek_cb` never issues a `Range` request, and from the
    app's side that is indistinguishable from a seek that landed: the pump logs its intent either
    way. Counting range opens on the wire is the direct defence, and it is also what would catch a
    server (a hand-substituted `python3 -m http.server`, say) answering 200 to a Range request —
    the silent corruption this tier's server exists to make impossible.
    """
    opens, ranges = delta
    if exact_opens is not None and opens != exact_opens:
        return False, f"the fixture server served {opens} body/bodies, want exactly {exact_opens}"
    if exact_range is not None and ranges != exact_range:
        return False, f"the fixture server saw {ranges} ranged request(s), want exactly {exact_range}"
    if opens < min_opens:
        return False, f"the fixture server served {opens} body/bodies, need >={min_opens}"
    if ranges < min_range:
        return False, (f"{ranges} ranged (206) request(s), need >={min_range} — the seek never "
                       f"reached the demuxer's Range reopen")
    return True, f"server saw {opens} open(s), {ranges} ranged"


def a_replayed(lines, want):
    """The finished stream STARTED AGAIN, and the second run really played — LG #46's second half.

    Three signals, and each is worthless without the other two:

      * `replay: starting the finished stream again` exactly `want` times (`app.rs`, at the EOS
        site). COUNTED, not merely found: a replay that fires more often than the case armed is a
        loop, and a loop satisfies every other assertion here while meaning the opposite.
      * at least `want + 1` `load:` lines. `engine::start_bufferfeed` writes one per SESSION and
        `teardown` clears the URL, so a second line is what says `dev::playurl()` was re-read and
        the payload rebuilt — the thing that distinguishes a real restart from a pipeline that
        never tore down.
      * the media position FELL and then climbed again. A replay that resumed where the first run
        ended would produce both lines above and no second viewing; the drop is the only evidence
        that the stream restarted rather than continued.

    The position series is the same `pos=` heartbeat every other case reads, deliberately: a binary
    that stopped emitting it fails here the way it fails everywhere else, instead of passing this
    case by having nothing left to contradict.

    NB the replay COUNT is an equality, which makes this the third exception to the early-exit
    soundness rule above `SETTLE_S` — see that note for what it can and cannot see, and why the
    real defence against a runaway loop is `replay_left` in `app.rs` rather than anything here.
    """
    fired = [ln for ln in lines if "replay: starting the finished stream again" in ln]
    if len(fired) != want:
        why = ("the app never re-entered the player — is `plxnative-replay` armed, and does this "
               "binary carry the replay arm at all?" if not fired
               else "a replay that fires more often than it was asked to is a loop")
        return False, f"{len(fired)} `replay:` line(s), want {want} — {why}"
    loads = [ln for ln in lines if RE_LOAD.search(ln)]
    if len(loads) < want + 1:
        return False, (f"{len(loads)} `load:` line(s) for {want} replay(s) — the second playback "
                       f"never rebuilt its payload, so the pipeline was never restarted")
    ts = [t for t, _ in playpos_secs(lines)]
    if len(ts) < 2:
        return False, f"only {len(ts)} media-position sample(s); a replay cannot be seen in them"
    # The DROP, as the deepest fall anywhere in the series — so a case replaying twice still reads
    # as one number, and a single late sample cannot hide it. The INDEX is kept too, because the
    # climb below has to be measured from there.
    peak, drop, at = ts[0], 0, 0
    for i, t in enumerate(ts):
        peak = max(peak, t)
        if peak - t > drop:
            drop, at = peak - t, i
    if drop < 5:
        return False, (f"the media position never fell (peak {max(ts)}s, deepest drop {drop}s) — "
                       f"the `replay:` line fired but playback carried on from where it was")
    # ...and it CLIMBED after falling, which is the second viewing rather than a restart that
    # stalled at the join.
    #
    # Anchored at the deepest DROP, not at the global floor. The floor form is the shape this
    # shipped with and it was a FALSE PASS: the floor is a VALUE, and viewing 2 only reaches
    # viewing 1's minimum value by coincidence — the `pos=` heartbeat is 1 Hz and free-running, so
    # viewing 1 logging `pos=0s` while viewing 2's first sample lands at `pos=1s` puts the anchor
    # back in viewing 1 and measures VIEWING 1'S OWN CLIMB. `[0,5,10,19,1]` then read as "fell 18s
    # then climbed 19s" and passed with the second viewing having produced one sample and zero
    # seconds of playback — which is precisely the near-miss named below, in the sample ordering
    # the field will actually produce, and the state the harness normally grades because it exits
    # the moment every assertion passes.
    tail = ts[at:]
    climb = max(tail) - min(tail)
    if climb < 5:
        return False, (f"the position fell {drop}s but only climbed {climb}s afterwards — the "
                       f"replay restarted and then did not play")
    return True, (f"{want} replay(s), {len(loads)} `load:` line(s), position fell {drop}s then "
                  f"climbed {climb}s over {len(ts)} samples")


def a_finished(lines):
    """The stream ran OUT, and the app left the player instead of freezing on the last frame.

    LG App Self Checklist #46 is "replay after completion", and this is its first half — the
    completion. Two links, in order, because either alone is satisfiable by something that is not
    a finish:

      * `EOS reached: playpos=Ns/Ms → ended` (pump.rs) — the producer hit file EOF AND the pipeline
        played out to within a second of the duration. Not merely "the socket closed": the pump
        gates the flag on `eos_pushed && pos >= dur - 1s`, so a truncated transfer does not reach
        it.
      * `stop_bufferfeed: torn down` (engine.rs::teardown) AFTER that line — `app.rs` calls
        `finish_playback` on `player::ended()`, which with nothing queued is `exit_player`. The
        ORDER is the assertion: this app tears the engine down on every stop, including the
        harness's own close at the end of a case, so an unordered match would pass on a clip that
        never ended at all.

    "After" is a SEARCH FROM `eos`, not a comparison against the first teardown in the log, and
    the difference is a false regression rather than a nicety: `teardown` writes that same line on
    a `for_reload` stop too — a seek that escalated to `reload_at`, or an app-switch suspend — so a
    first-match index can sit BEFORE the EOS while every teardown that matters comes after it. The
    comparison form fails such a case for its whole `run_secs`, with the evidence reading "the
    player froze on the last frame", which is exactly the reading this assertion exists to avoid.

    What it deliberately does NOT claim is the SECOND half of #46 — that the same content can then
    be started again. That is [`a_replayed`]'s job and `pipe_replay_after_eos`'s, and the two stay
    apart because the failures are different: a stream that never ends, and a stream that ends and
    cannot be restarted. One case would report either as the other.
    """
    eos = next((i for i, ln in enumerate(lines) if "EOS reached" in ln), None)
    if eos is None:
        ts = progress_secs(lines)
        far = max((t for t, _ in ts), default=-1)
        return False, (f"no `EOS reached` line — the stream never ran out (deepest position "
                       f"{far}s). A fixture longer than this case's cap cannot end inside it.")
    torn = next((i for i, ln in enumerate(lines) if i > eos
                 and "stop_bufferfeed: torn down" in ln), None)
    if torn is None:
        return False, (f"reached EOS but never tore the engine down after it — the player froze on "
                       f"the last frame :: {lines[eos].strip()}")
    return True, f"{lines[eos].strip()} -> {lines[torn].strip()}"


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


def op_quality_switch(lines, steps):
    """A mid-playback quality change must LAND, and playback must survive it.

    This is the thing a person does at the television that no boot override can reach:
    `plxnative-quality` decides what a playback STARTS as and is read once, while switching a
    running stream re-asks the routing question against a picture already on screen, reloads if the
    answer moved, and — on the way out of Auto — tears down a live ABR controller.

    Three assertions, and the middle one is the only one that grades what a PIN MEANS:

    * every requested rung produced its `quality: switch → <rung>` line, in order;
    * **Auto's controller stops when Auto stops**, and — only if this run showed it running at all
      — resumes when Auto does. `route::hls_abr_control` returns `None` for any non-Auto quality,
      so the `abr: sample` line, one per acquired segment, must cease after a pin. That is a
      derived consequence of the routing policy rather than a restatement of it: nothing tells the
      controller about the switch, it simply is not constructed. A pin that left it deciding would
      be a stream still being adapted under a user who asked for a fixed rung.

      The RESUME half is conditional, and self-calibrating rather than assumed. `hls_abr_control`
      also requires HLS delivery and a live encoder, so Auto on an item this server DIRECT-PLAYS
      runs no controller and never will — asserting one would fail on somebody's library for a
      reason that is not a defect. So it is asserted exactly when the run itself demonstrated the
      capability: if `abr: sample` appeared before the first switch, Auto adapts this item here,
      and switching back must restore that. If it never appeared, the case still grades the switch
      landing and playback surviving, and says so.
    * the position keeps climbing across the whole sequence, since a reload that never re-primes
      would otherwise read as a successful switch.

    `no_playing_error` is asserted separately by every case that uses this, so it is not repeated
    here.
    """
    marks = [(i, ln) for i, ln in enumerate(lines) if "quality: switch → " in ln]
    got = [ln.split("quality: switch → ", 1)[1].split(" ", 1)[0].strip() for _, ln in marks]
    if got != list(steps):
        return False, f"switched to {got}, want {list(steps)}"

    # Did Auto adapt this item on this server AT ALL, before anything was switched? Everything
    # about the resume half hangs on it, and it is read from the run rather than assumed.
    adapted_before = sum(1 for ln in lines[:marks[0][0]] if RE_ABR_SAMPLE.search(ln))

    # Segment-level ABR activity, sliced by where each switch landed. A quality request is logged
    # before `set_quality`, while the old demux worker may still finish an in-flight acquisition.
    # Grade the replacement stream after its fresh-Load boundary, not that teardown interval.
    for n, (idx, _) in enumerate(marks):
        upto = marks[n + 1][0] if n + 1 < len(marks) else len(lines)
        requested = lines[idx + 1:upto]
        reload_at = next(
            (i for i, ln in enumerate(requested)
             if "reload_transcode: fresh Load" in ln or "reload_at: fresh Load" in ln),
            None,
        )
        window = requested[reload_at + 1:] if reload_at is not None else requested
        samples = sum(1 for ln in window if RE_ABR_SAMPLE.search(ln))
        if steps[n] == "auto":
            if adapted_before and samples == 0:
                return False, (f"Auto adapted this item before the switches ({adapted_before} "
                               f"`abr: sample`) but not after switching back to auto "
                               f"(0 in {len(window)} line(s))")
        else:
            if adapted_before and reload_at is None:
                return False, (f"pinning {steps[n]} requested no replacement Load after an active "
                               "Auto controller — the fixed route did not land")
            if samples:
                return False, (f"after pinning {steps[n]} the ABR controller kept deciding "
                               f"({samples} `abr: sample` line(s) after the fresh Load) — the "
                               "replacement stream is still being adapted")

    after = lines[marks[0][0]:]
    ts = progress_secs(after)
    if len(ts) < 2 or ts[-1][0] <= ts[0][0]:
        return False, (f"position did not advance across the switches "
                       f"({len(ts)} sample(s), {ts[0][0] if ts else '?'}s..{ts[-1][0] if ts else '?'}s)")
    adapt = (f"Auto adapted ({adapted_before} sample(s)) before the first switch"
             if adapted_before else
             "Auto did NOT adapt this item here (direct play or progressive) — the resume half "
             "of this assertion is not graded")
    return True, (f"switched {' -> '.join(got)}; position advanced "
                  f"{ts[-1][0] - ts[0][0]}s over {len(ts)} sample(s); {adapt}")


def op_seek_refused(lines, target_s):
    """A seek the app CANNOT serve must be refused cleanly, and playback must survive the refusal.

    Reaching this path is structural rather than incidental, and only this tier can. A transcode
    seek restarts the encode at a new `&offset`, which `route::transcode_seek` builds from a PMS
    ratingKey and client — and a `plxnative-playurl` playback has neither, so every seek during
    Auto on the pipeline tier is refused. That makes it the one place the REFUSAL path is
    observable at all; on the server tier the seek succeeds and this branch never runs.

    What is graded is the survival, because that is what failed. `player::request_seek` arms
    `SHARED.seeking` and, before 2026-08-27, only a successful prime→Play ever disarmed it — so a
    refused seek left `PlaybackState::Seeking` latched forever: a spinner over the picture, the
    playhead frozen at the target, and `pos=` (gated on `is_playing()`) absent from every
    subsequent heartbeat. Measured before the fix: the position froze at 5 s while 37 further
    segments were acquired, four rung commits landed and the loop held 60 fps for 84 more seconds.

    So the assertion is DIFFERENTIAL by construction: it requires the position series to advance
    AFTER the refusal line, which is exactly what a latched spinner prevents. `target_s` is unused
    and named only so the signature matches its siblings — a refused seek reaches no target, and
    asserting one would assert the bug.
    """
    _ = target_s
    fired = find(lines, "autoseek: step")
    if fired is None:
        return False, "the seek was never requested (`autoseek: step` absent)"
    refused = find(lines, "seek(transcode): rebuild failed")
    if refused is None:
        return False, ("the seek was NOT refused — this tier cannot rebuild a transcode, so either "
                       "the app grew a playurl seek path or the case is no longer on Auto")
    after = lines[lines.index(refused) + 1:]
    ts = progress_secs(after)
    if len(ts) < 2:
        return False, (f"only {len(ts)} position sample(s) after the refusal — the read-out is "
                       "latched (`SHARED.seeking` never disarmed); see player::abandon_seek")
    climb = ts[-1][0] - ts[0][0]
    if climb <= 0:
        return False, f"position did not advance after the refusal ({ts[0][0]}s..{ts[-1][0]}s)"
    return True, f"refused cleanly; position advanced {climb}s over {len(ts)} sample(s) after it"


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
    # The claim is that a NATIVE audio switch costs the video nothing -- same stream, same decoder,
    # no reload -- so grade the codec against the one this run OPENED WITH, never against a literal.
    # This read `!= "hevc"` until 2026-08-22, which was a fact about the maintainer's library and
    # not about the player: map an h264 episode to this case's shape and a perfectly native switch
    # failed as `codec after native switch = h264`. `op_audio_transcode` already grades its own
    # (harder) version of the same question this way.
    if cs[-1][0] != cs[0][0]:
        return False, (f"codec changed across the native switch ({cs[0][0]} -> {cs[-1][0]}); "
                       f"a native switch must not reload the video :: {cs[-1][3].strip()}")
    return True, f"native switch OK; codec stayed {cs[-1][0]} :: {hit.strip()}"


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
# ...and, since 2026-08-23, one COUNT that is an equality rather than a floor:
#   * a_replayed            — exactly N `replay:` lines (more than N is a loop)
# It is the same shape as the two above and it is here because this file asks a third exception to
# be reasoned about rather than added. What it cannot see: an app that replays FOREVER fires its
# next `replay:` line only after another full viewing (~20 s on the short clip), and the case can
# satisfy everything else at ~28 s, so an early exit two seconds later stops before the evidence
# arrives. The primary defence is therefore app-side and not here — `replay_left` is decremented
# before each re-arm and its parsing is host-tested (`app::replay_budget_tests`) — and `--no-early`
# is how to go looking for a loop deliberately. Deliberately NOT closed by forcing this one case to
# burn its whole 100 s cap: that is ~1.5 min on every suite run to guard three lines whose bound is
# already gradeable by `make check`, and the trade is worth re-taking only if that bound moves.
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


class StampedLines(list):
    """The log as a plain list of strings, plus WHEN each line reached the harness.

    A `list` subclass rather than a second parallel structure, so every existing caller — and
    there are dozens — keeps treating the log as the list of strings it has always been, while
    `.stamps[i]` carries the harness monotonic clock reading for `self[i]`.

    It exists to place an app observation on the SHAPER's timeline. The fixture server runs in
    this process, so its phase clock and these stamps are the same `time.monotonic()`; that is
    what lets a segment be attributed to an injected leg without asking the controller anything.

    The stamp is ARRIVAL, not emission: it lags the app by the ssh/`tail -F` hop. Sub-second
    against legs measured in seconds, and the direction is knowable — arrival is always later —
    but it is a real limit and every consumer says so.
    """

    def __init__(self, items=(), stamps=()):
        super().__init__(items)
        self.stamps = list(stamps)

    def append(self, item):
        self.stamps.append(time.monotonic())
        super().append(item)

    def snapshot(self):
        """A copy carrying its stamps. `list(x)` would silently drop them."""
        return StampedLines(list(self), list(self.stamps))


def _drain(stream, sink, done):
    """Reader thread: filter the type=43 flood at the door so the grader never re-scans it."""
    try:
        for raw in stream:
            ln = raw.rstrip("\n")
            if not TYPE43_SPAM.search(ln):
                sink.append(ln)
    finally:
        done.set()


def _run_stream_pids():
    """PIDs of every local ssh/sshpass process carrying a run-stream tail.

    RUN_STREAM_MARK is the tail `make run-stream` ends in, and it is DERIVED (resolve_flavour) from
    `make -s print-eventlog` rather than written out here — the two installs tail two different
    files, and a mark that stops matching the remote command text reaps nothing at all.
    """
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
    """Leave the TV as we found it. Runs on EVERY exit — pass, fail, Ctrl-C, SIGTERM, crash,
    and whether this file was run as a program or imported and called.

    Three things outlive the harness otherwise, and none of them are cosmetic:

    * THE APP KEEPS RUNNING. Nothing closes it at the end — `make kill` only runs at the START
      of the next case, so the last case's playback just carries on. It keeps a PMS session
      open, and its timeline reporter keeps posting progress every 10s, so the next run
      inherits a resume point on that rk — the exact contamination ee07506 removed between
      cases, reintroduced at the seam between runs. It is also what "I see FPS tests running"
      looks like from the outside, long after the suite printed its summary.
    * THE INJECTED TOKENS STAY in the runtime root's plxnative-token and plxnative-servers: real
      per-(user,server) PMS access tokens -- and the second file carries someone ELSE'S server's
      too -- world-readable, on a device with a rooted sshd and a committed password. The normal
      path did clear them; every abnormal one left them. Both are covered because the wipe is a
      GLOB over <runtime root>/plxnative-*, which is exactly why a new credential trigger needs
      no change here; a credential file named anything else would need one.
    * ssh CLIENTS. Per-case reaping covers a case that ends normally, but not the harness dying
      between cases.

    All three are scoped to the install this run drove and to nothing else: `make kill` carries
    the flavour (so `closeByAppId` names this id, and `fuser -k` is INODE-scoped on this app dir —
    a name-based kill would take the other install down with it), and the trigger wipe is a glob
    inside this flavour's runtime root. A run against the developer build must leave the app users
    installed exactly as it found it, running or not.

    Never raises: a teardown that throws on an unreachable TV would mask the real failure (and
    on Ctrl-C would replace the user's interrupt with a traceback about ssh).

    IDEMPOTENT, because two things arm it — the atexit hook in `arm_teardown` and the `__main__`
    wrapper — and on the ordinary path both fire. Doing the work twice is only wasteful, but the
    second `make kill` prints over the summary the user is reading, and a reader who sees the
    cleanup banner twice reasonably wonders which run it belonged to.
    """
    global _TEARDOWN_DONE
    if _TEARDOWN_DONE:
        return
    _TEARDOWN_DONE = True
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
_TEARDOWN_DONE = False

# Whether THIS run took the television's lock (as opposed to inheriting one the operator or an
# outer `tv-lock.sh with` already held). Only a lock we took is a lock we release: dropping an
# inherited one would hand the set away in the middle of somebody's larger session.
_LOCK_TAKEN = False
TVLOCK = os.path.join(REPO_ROOT, "tools", "tv-lock.sh")


def acquire_tv_lock(tv, why):
    """Take the television's lock, or refuse to run.

    The whole suite is one long exclusive session: it closes the app between cases, wipes the
    runtime root, injects tokens and grades a log it assumes only it is writing. A second job on
    the set during any of that does not produce a clean failure — it produces a plausible wrong
    one (a bogus timeline_climb, an fps sample taken while another lane's deploy was landing),
    which is indistinguishable from a real regression when you read the summary.

    A lease this lane already holds is inherited rather than re-taken, so
    `tools/tv-lock.sh with -- ./tests/run.py` and a hand-held session both work unchanged.
    """
    global _LOCK_TAKEN
    if not os.access(TVLOCK, os.X_OK):
        return
    env = dict(os.environ, TV=tv)
    held = subprocess.run([TVLOCK, "status"], env=env, capture_output=True, text=True)
    if "HELD BY THIS LANE" in held.stdout:
        print("TV lock: already held by this lane — inheriting it")
        return
    # `--wait 0`: a harness that blocks for ten minutes inside somebody's tool call is worse than
    # one that says who has the set and exits. `--wait <s>` is the caller's choice, not ours.
    r = subprocess.run([TVLOCK, "acquire", "--why", why, "--as", "tests/run.py"], env=env)
    if r.returncode != 0:
        sys.exit("refusing to run: the television is held by another lane (see above). "
                 "Wait for it (tools/tv-lock.sh acquire --wait 540), or run host-side work "
                 "meanwhile (make check, make sim).")
    _LOCK_TAKEN = True


def release_tv_lock(tv):
    """Give the set back. Never raises — it runs in the same finally as teardown."""
    if not _LOCK_TAKEN:
        return
    try:
        subprocess.run([TVLOCK, "release"], env=dict(os.environ, TV=tv),
                       capture_output=True, timeout=30)
    except Exception:
        pass


def arm_teardown(tv):
    """Commit to driving `tv`, and guarantee the cleanup from THIS MOMENT ON, whatever happens.

    The atexit hook is the guarantee, and it is not redundant with the `__main__` wrapper below.
    That wrapper only runs when this file is the program; anything that does `import run;
    run.main()` -- a wrapper script, tests/test_harness.py, an agent probing one exit path -- got
    no cleanup at all, and the failure is invisible: the run prints its summary, exits 0, and
    leaves the app playing with a live per-(user,server) PMS token sitting world-readable in the
    runtime root of a device with a rooted sshd and a published password. That is the one outcome
    teardown() exists to prevent, and it happened here on 2026-08-22.

    Both paths stay: atexit does not run for SIGTERM (the wrapper's handler converts it to an
    unwind) and prints after the interpreter has begun shutting down, so the wrapper keeps giving
    the ordinary path its output in the right place. `teardown` is idempotent, so both firing is
    fine and neither firing is not possible."""
    global _TEARDOWN_TV
    _TEARDOWN_TV = tv
    atexit.register(lambda: teardown(_TEARDOWN_TV) if _TEARDOWN_TV else None)


# The app's first log line, written before anything can fail (app.rs's plex_run): which install
# produced this log, and whether it was built with the dev triggers at all.
RE_INSTALL = re.compile(r"install: id=(\S+) flavour=\S+ .*\bfeatures=(\S+)")


def check_install(lines, cfg):
    """Refuse to grade a log that did not come from the install this run drove.

    Returns True once the boot line has been seen and matched, False while it has not arrived yet;
    aborts the WHOLE run otherwise — not the case, for the reason below.

    Nothing else can answer this question. Both binaries are named `plxnative`, so `pidof` matches
    both on this busybox set, and `pkg/plxnative` is a path every flavour and every configuration
    writes, so an md5 against the local build proves only that SOME build matches. This one line is
    the only witness, which is why its absence is also a refusal (see require_install).

    The un-caught failure is what makes this worth an abort. A RELEASE build reads no triggers at
    all — `devtriggers` is compiled out, so `dev::read` is None at COMPILE time — which means the
    injected PMS token is ignored, the app has no session, and it parks on the who's-watching
    picker having played nothing. Every assertion then fails as "the line has not appeared YET",
    which `failed_for_good` deliberately never settles, so every case burns its full run_secs and
    the summary reads like a catastrophic regression — for a build that is working perfectly.
    Grading the OTHER install's log is the same failure, quieter.
    """
    for ln in lines:
        m = RE_INSTALL.search(ln)
        if not m:
            continue
        got, feats = m.group(1), m.group(2)
        if got != APPID:
            raise SystemExit(
                f"WRONG INSTALL: the app that booted logs id={got}, but this run drives "
                f"{cfg['appid']} (flavour {FLAVOUR}). Every path this harness used belongs to "
                f"{cfg['appid']} — the triggers, the injected token, {EVENTLOG} — so the whole "
                f"run would grade a log it never armed. Deploy the flavour you meant "
                f"(`make FLAVOR={FLAVOUR} install` once, then `make FLAVOR={FLAVOUR} deploy`), or "
                f"select the other one with --flavor.")
        if feats != "dev":
            raise SystemExit(
                f"RELEASE BUILD on {got}: its boot line says features={feats}, so the whole "
                f"`devtriggers` surface was compiled out and it reads NOTHING under {RUNDIR} — not "
                f"the injected PMS token, not one trigger this harness wrote. It boots to the "
                f"who's-watching picker and plays nothing, and every case would burn its full cap "
                f"failing as if the player were broken. Ship a dev build to this install: "
                f"`make FLAVOR={FLAVOUR} deploy` without RELEASE=1.\n"
                f"  NB the STABLE install is normally a release build by design — `make deploy` "
                f"refuses a dev build on that id unless ALLOW_DEV_ON_STABLE=1 — so `--flavor "
                f"stable` lands here unless you deliberately put a dev build there.")
        return True
    return False


def require_install(lines, cfg):
    """check_install for a COMPLETE log, where an absent boot line is itself a refusal."""
    if check_install(lines, cfg) or not lines:
        return  # an empty log is graded by the assertions themselves ("no line found")
    raise SystemExit(
        f"no `install:` boot line in {EVENTLOG}. The deployed binary predates it (app.rs's "
        f"plex_run writes it first, before anything can fail), so nothing in this log says which "
        f"of the two installs produced it — and an unattributable log is exactly what this check "
        f"exists to refuse. `make FLAVOR={FLAVOUR} deploy` ships one that carries it.")


def stream_case(case, cfg, cap_s, early=True, inject=None, evaluator=None, on_start=None):
    """Launch via `make run-stream` and grade the log as it streams.

    Returns (lines, elapsed_s, stopped_early, settled_reason). Lines come back already filtered;
    settled_reason is non-None only when the case was cut short by an already-decided failure.

    `evaluator` is how the two tiers share this function: it is (case, lines) -> (passed, results),
    defaulting to the integration `evaluate`. It has to be a parameter rather than a branch,
    because the early-exit poll below runs the SAME grading the verdict uses — a poll that graded
    something else would either stop a case that had not passed or run one that had. (This was a
    hardcoded `evaluate` at first, and a pipeline case died in it with `KeyError: 'decision'` on
    the first poll: there is no PMS decision here to be.)
    """
    # Snapshot BEFORE launching so the teardown can kill exactly the clients this case started.
    # killpg is not sufficient on its own: sshpass forks ssh into its OWN process group (measured:
    # make pgid=P, sshpass pid=A pgid=P, ssh pid=B pgid=B ppid=A), so the group signal never
    # reaches B, and B reparents to init holding an ssh connection and a remote `tail -F`. These
    # accumulate one per case — 125 of them piled up against the TV in a single session before
    # this was noticed, which is real load on a device whose dropbear has a connection limit.
    pre_pids = _run_stream_pids()
    proc = subprocess.Popen(make_argv(["run-stream", f"TV={cfg['tv']}"]),
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                            text=True, bufsize=1,
                            # own process group: terminating `make` alone would orphan the
                            # sshpass/ssh child and leave the remote tail (and the app) attached.
                            start_new_session=True)
    lines, done = StampedLines(), threading.Event()
    threading.Thread(target=_drain, args=(proc.stdout, lines, done), daemon=True).start()

    injected = inject is None
    install_ok = False
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
            # As early as possible: a wrong or release install disqualifies the whole run, and
            # finding that out at the first case costs one cap instead of the entire suite's.
            if not install_ok:
                install_ok = check_install(list(lines), cfg)
            if deadline is None:
                if lines:
                    deadline = now + cap_s
                    # The app is alive and its clock has started. Anything scheduled against
                    # PLAYBACK time — the link conditioner's legs — is anchored here and not at
                    # ssh start, for the same reason `cap_s` is: the close+launch overhead is a
                    # couple of seconds and it is not playback.
                    if on_start:
                        on_start(now)
                elif now - started >= BOOT_GRACE_S:
                    break  # app never wrote a line — grade the empty log, same as a dead TV
            elif now >= deadline:
                break
            if not injected and any(inject[0] in l for l in lines):
                injected = True
                # the FIFO has a live reader (the app drains it per frame), so this returns at once
                ssh(cfg["tv"], f"printf '{inject[1]}\\n' > {RUNDIR}/plxnative-remote", timeout=15)
                print(f"    injected key '{inject[1]}' on '{inject[0]}'")
            if early:
                snap = list(lines)
                ok, _ = (evaluator or evaluate)(case, snap)
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
    # And once more now the log is complete. The in-loop check cannot refuse an ABSENT boot line —
    # while a case is still running, "not there" and "not there yet" are the same thing.
    if not install_ok:
        require_install(list(lines), cfg)
    return lines.snapshot(), time.monotonic() - started, stopped_early, settled


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

    # Base assertions. Adaptive cases deliberately omit a fixed decision/codec: those are outputs
    # of the real server and link, while their integration contract is stated by their operations.
    if "decision" in exp:
        results.append(("decision", *a_decision(lines, exp["decision"])))
    if "codec" in exp:
        results.append(("codec", *a_codec(
            lines, exp["codec"], exp.get("min_video_width", 0), exp.get("video_size"))))
    if exp.get("require_video_bound", True):
        results.append(("video_bound", *a_video_bound(lines)))
    results.append(("timeline_climb", *a_timeline_climb(lines, exp.get("min_timeline_climb_s", 12))))
    if "max_reloads" in exp:
        results.append(("reload_ceiling", *a_reload_ceiling(lines, exp["max_reloads"])))
    if "min_play_rate_pm" in exp:
        results.append(("play_rate", *a_play_rate(lines, exp["min_play_rate_pm"])))
    results.append(("timeline_post", *a_timeline_post(lines)))
    if exp.get("no_playing_error", True):
        results.append(("no_error", *a_no_error(lines)))

    # per-operation assertions
    for op in case["operations"]:
        k = op["op"]
        if k == "seek" and op.get("mode") == "rapid":
            results.append(("seek_rapid", *op_seek_rapid(lines, op["final_s"])))
        elif k == "seek" and op.get("mode") == "refused":
            results.append(("seek_refused", *op_seek_refused(lines, op.get("target_s", 140))))
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
        elif k == "quality_switch":
            steps = op["to"] if isinstance(op["to"], list) else [op["to"]]
            results.append(("quality_switch", *op_quality_switch(lines, steps)))
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


def report_case(passed, results, elapsed, run_secs, stopped_early, settled, verbose):
    """Print one case's verdict. Shared by both tiers, and `redact()` is why.

    This was copied from `run_case` into the synthetic runner and the copy dropped the
    redaction — harmless for a tier that injects no token, but the point of having ONE printer
    is that the property cannot be lost by a copy. The evidence strings are arbitrary log lines
    and opened URLs; `redact()` is a no-op on anything that carries no `X-Plex-Token=`.
    """
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


def run_case(case, cfg, token, verbose, cond=None):
    name = case["name"]
    tv = cfg["tv"]
    run_secs = case.get("run_secs", 60)
    print(f"\n=== {name}  (rk={case['rk']}, {case.get('title','')}) ===")
    print(f"    covers: {', '.join(case.get('covers', []))}")
    require_tv(tv, name)

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
    # Always required — the binary carries no baked token, so plxnative-token in the runtime root
    # is the only way an automated run gets PMS access.
    files = triggers_for_case(case)
    extras = []
    if cfg.get("inject_token"):
        extras.append(f"printf '%s' '{token}' > {RUNDIR}/plxnative-token")
    # …and, for a case that declares it needs one, the SECOND server's credentials — same rules:
    # value never on stdout, cleared by the glob wipe above and again by teardown().
    srv_json = shared_servers_json(cfg, case)
    if srv_json:
        extras.append(f"printf '%s' {sh_squote(srv_json)} > {RUNDIR}/plxnative-servers")
    apply_triggers(tv, files, extra=extras)
    shown = ", ".join(n + ("=" + c if c is not None else "") for n, c in files)
    print(f"    triggers: {shown}")
    if cfg.get("inject_token"):
        print(f"    plxnative-token: <{cfg['user_label']}, redacted>")
    if srv_json:
        # said the way the APP says it (`describe_server`): a share is a `ref=` tag and nothing
        # else. The token was never printed here and still is not.
        print(f"    plxnative-servers: <{describe_server(cfg['shared_server'])}, token redacted>")

    # 4. run + grade the log as it streams (run_secs is the cap, not the runtime)
    early, why = early_exit_allowed(case, cfg)
    if not early and why:
        print(f"    early exit disabled: {why}")
    print(f"    run-stream (cap {run_secs}s{'' if early else ', early exit off'}) ...")
    profile = case.get("link_profile")
    on_start = None
    if profile and cond and cond.usable:
        on_start = lambda at: cond.arm(profile, at)
    try:
        lines, elapsed, stopped_early, settled = stream_case(
            case, cfg, run_secs, early=early, inject=key_inject_for_case(case),
            on_start=on_start)
    finally:
        # ALWAYS, and before the next case is set up: a mode left armed shapes every case after
        # this one, and nothing downstream would say so.
        if on_start:
            cond.disarm()

    # 5. evaluate
    passed, results = evaluate(case, lines)
    report_and_record(cfg, name, passed, results, lines, elapsed, run_secs, stopped_early,
                      settled, verbose)
    return passed, results, lines


# ---------------------------------------------------------------------------
# The PIPELINE tier — the player, with no Plex behind it at all.
#
# The 21 cases above drive `plxnative-play=<ratingKey>`, i.e. the whole chain: plex.tv auth, the
# PMS `/decision`, library metadata, the PlayQueue, markers, resume, the timeline reporter. That
# is the right shape for what they grade — SELECTION — and it is also why they need somebody's
# library and cannot run for anyone else.
#
# These cases drive `plxnative-playurl`: a generated file served off this machine by
# `tests/serve_fixtures.py`, plus the Load-payload declaration to play it with. What runs is the
# same engine, byte for byte, from `stream.rs`'s raw-socket GET through `ff.rs`'s demux over the
# custom AVIO, the two-lane AU queues, the Starfish `Feed()` pump and the ACB bind — only the
# CHOOSING is bypassed. No token, no ratingKey, no library, no sharing, nothing to configure but a
# TV address.
#
# What this tier CANNOT prove, and must never be read as proving: that the declaration it feeds is
# the declaration a real item would produce. It writes those five route fields itself, so the whole
# metadata -> plan -> apply_plan half is bypassed and a regression there passes here green. Nor
# does it reach resume, markers, Up Next, the timeline reporter, subtitle or audio-track SELECTION,
# or any transcode. It is the tier that separates "the player is broken" from "the library layer
# is broken" — not a replacement for either.
# ---------------------------------------------------------------------------
@functools.lru_cache(maxsize=None)
def _probe_fixture(path):
    """`(duration_s, [(codec_type, codec_name), ...])` for one fixture, or None.

    ffprobe rather than the generator's `fixtures.json`, deliberately: the file on disk is the
    thing the television will actually play, and a sidecar record can be stale, hand-edited, or
    written by a version of the generator that spelled its fields differently. None (no ffprobe on
    this machine, an unreadable file) disables every check built on it rather than failing them —
    refusing to run a suite because a *diagnostic* is unavailable is the wrong trade.

    Cached by PATH because SEVERAL cases share one fixture — a seek case beside its play case, an
    audio-lane pair on the multi-audio clip — so `--list` would otherwise ffprobe the same file
    twice. Stated as the property rather than as a count: this line read "three of the twelve" and
    was wrong on both halves even before the resolution matrix widened the denominator (it was four
    of twelve then, and is four of nineteen now). The caching is right at any of those numbers.
    The streams come back in container order, which is the order `ff.rs` walks when it matches the
    declared audio codec, so the list index IS the `a=#<n>` the log reports.
    """
    try:
        out = subprocess.run(
            ["ffprobe", "-v", "error", "-of", "json",
             "-show_entries", "format=duration:stream=codec_type,codec_name", path],
            capture_output=True, text=True, timeout=20)
        doc = json.loads(out.stdout)
        dur = float(doc["format"]["duration"])
        return dur, [(st.get("codec_type"), st.get("codec_name")) for st in doc.get("streams", [])]
    except (OSError, ValueError, KeyError, subprocess.SubprocessError):
        return None


def _declaration_mismatch(case, streams):
    """Why this case's `declare` does not describe the file it names, or None.

    The tier's single premise is that the declaration is honest about the media, and until this
    existed nothing checked it: the pack's generator verifies `declare` against what it built, but
    the harness sends the manifest's copy, and the two are authored separately. The three cases
    that deliberately override a shape's declaration to select another audio lane are exactly the
    ones the generator's own check cannot cover — and they are the ones carrying a hand-counted
    stream index.

    A mismatch is a SKIP, not a failure, for the same reason a too-short pack is: the fixture is
    wrong, the player is not, and a case that fails here would point squarely at the player.
    """
    dec = case.get("declare", {})
    vid = [c for t, c in streams if t == "video"]
    aud = [(i, c) for i, (t, c) in enumerate(streams) if t == "audio"]
    if dec.get("vcodec") and vid and dec["vcodec"] != vid[0]:
        return f"declares vcodec {dec['vcodec']!r}, the file's video stream is {vid[0]!r}"
    if dec.get("acodec") and aud and dec["acodec"] not in [c for _, c in aud]:
        return (f"declares acodec {dec['acodec']!r}, which no audio stream carries "
                f"({', '.join(c for _, c in aud)})")
    want_idx = case.get("expect", {}).get("audio_stream_index")
    if want_idx is not None and dec.get("acodec"):
        # `ff.rs::audio_stream_matching` feeds the FIRST audio stream whose codec matches, so the
        # expected index is derivable — computed here from ffprobe rather than trusted from the
        # manifest, which makes the hand-counted number an assertion instead of a premise.
        first = next((i for i, c in aud if c == dec["acodec"]), None)
        if first != want_idx:
            return (f"expects audio_stream_index {want_idx}, but the first {dec['acodec']!r} "
                    f"stream in this file is #{first}")
    return None


def _case_depth_s(case):
    """The deepest media position this case needs to reach, in seconds.

    Every seek op already DECLARES where it ends up — `final_s` for a rapid burst, `target_s`
    otherwise — and those are the same fields the assertions grade against, so this reads them
    rather than re-deriving anything. It briefly parsed a rapid burst's `script` to find the
    deepest step, which was both dead (no synthetic case declares a rapid seek) and wrong (it
    read a tap-relative `+10` as an absolute 10).
    """
    depth = case.get("expect", {}).get("min_pos_climb_s", 0)
    for op in case.get("operations", []):
        if op.get("op") == "seek":
            depth = max(depth, op.get("final_s", op.get("target_s", 140)))
    return depth


def _resolve_fixtures(cases, root):
    """The skip channel, for media instead of for library items.

    Same contract as `_resolve_items`: a case that cannot run gets `skip` and NO `path`, so a
    partition that is ever wrong raises KeyError naming the case rather than driving the TV at
    some default. Three ways to be unrunnable, and they are different answers: the pack was never
    built, it was built SHORTER than this case seeks, or — for the one case that wants the stream
    to run OUT — it was built LONGER than the case can play through. The last two are the quiet
    ones: a pack regenerated at `--secs 30` while the manifest still seeks to 40 s makes the seek
    assertion fail as though the player had regressed, and the same regeneration at `--secs 300`
    would do it to `a_finished`.
    """
    for c in cases:
        name = c.get("fixture")
        if not name:
            c["skip"] = "case declares no `fixture`"
            continue
        path = os.path.join(root, name)
        if not os.path.isfile(path):
            c["skip"] = f"no fixture {name!r} in {root} — run `make fixtures-pipeline`"
            continue
        if c.get("auto_network"):
            abr_names = ["pipe_abr_240p.ts", "pipe_abr_480p.ts",
                         "pipe_abr_720p.ts", "pipe_abr_1080p.ts"]
            missing = [n for n in abr_names if not os.path.isfile(os.path.join(root, n))]
            if missing:
                c["skip"] = ("Auto network profile is missing segment fixtures "
                             f"{', '.join(missing)} — run `make fixtures-pipeline`")
                continue
        probe = _probe_fixture(path)
        if probe is not None:
            dur, streams = probe
            depth = _case_depth_s(c)
            if depth and depth > dur * 0.8:
                c["skip"] = (f"{name} is {dur:.0f}s but this case needs {depth:.0f}s — regenerate "
                             f"the pack longer (`make fixtures-pipeline FIXTURES_ARGS=--secs=<n>`)")
                continue
            # ...and the opposite bound, for `reaches_eos` alone. That case has to boot the app,
            # join the stream and then play the WHOLE clip at 1x inside its cap — ONCE PER VIEWING,
            # so a case that also replays needs the clip `replays + 1` times over. The clip must be
            # comfortably shorter than the cap rather than merely shorter: 0.6 leaves 40% of the
            # window for close+launch and one pre-roll per viewing, against a boot-to-playing
            # measured well under 15 s. A `--secs`-regenerated pack is the realistic way to trip
            # this, and it must skip — `a_finished` failing on a 300 s clip reads as the app
            # freezing on the last frame, and `a_replayed` failing on one reads as the replay arm
            # being absent from the binary.
            exp = c.get("expect", {})
            if exp.get("reaches_eos"):
                cap = c.get("run_secs", 30)
                views = 1 + exp.get("replays", 0)
                if dur * views > cap * 0.6:
                    c["skip"] = (f"{name} is {dur:.0f}s and this case must play it to the END "
                                 f"{views}x within a {cap}s cap — rebuild the pack at its declared "
                                 f"length (`make fixtures-pipeline`, no --secs/--quick)")
                    continue
            why = _declaration_mismatch(c, streams)
            if why:
                c["skip"] = f"{name} does not match this case: {why} — regenerate the pack"
                continue
        c["path"] = path


def evaluate_pipeline(case, lines, srv_delta, gst_lines=None):
    """Every assertion for a pipeline case. Returns (passed, [(label, ok, evidence)]).

    A separate function rather than a flag threaded through `evaluate`, for one concrete reason:
    `evaluate` calls `a_timeline_post` UNCONDITIONALLY, with no `expect` key that could switch it
    off, and that assertion can never hold here. Splitting keeps the integration tier's behaviour
    bit-identical instead of making it depend on a flag it never sets.
    """
    exp = case["expect"]
    results = [("stream_path", *a_stream_path(
        lines, case["fixture"],
        hls_entry=bool((case.get("auto_network") or {}).get("start_hls"))))]
    results.append(("load_decl", *a_load_decl(lines, exp)))
    if "load_count_exact" in exp:
        results.append(("load_count", *a_load_count(lines, exp["load_count_exact"])))
    if "auto_fallback_max_kbps" in exp:
        results.append(("auto_network", *a_auto_network_recovery(
            lines, exp["auto_fallback_max_kbps"], exp["abr_recovery_min_kbps"])))
    if "abr_shape" in exp:
        # The shaper's own schedule, stashed on the case by `run_pipeline_case` before
        # grading. The dip window must come from the PLANT, never from the app's observations.
        results.append(("abr_shape", *a_abr_shape(
            lines, exp["abr_shape"], case.get("_dip_windows", ()))))
    if exp.get("require_audio_feed_ready"):
        results.append(("audio_feed", *a_audio_feed_ready(lines)))
    if exp.get("no_reload"):
        results.append(("no_reload", *a_no_reload(lines)))
    if "codec" in exp:
        results.append(("codec", *a_codec(lines, exp["codec"], exp.get("min_video_width", 0),
                                          exp.get("video_size"))))
    if exp.get("require_video_bound", True):
        results.append(("video_bound", *a_video_bound(lines)))
    results.append(("pos_climb", *a_timeline_climb(lines, exp.get("min_pos_climb_s", 8),
                                               dense_only=True)))
    if "audio_stream_index" in exp:
        results.append(("audio_lane", *a_audio_lane(lines, exp["audio_stream_index"])))
    if exp.get("reaches_eos"):
        results.append(("finished", *a_finished(lines)))
    if exp.get("replays"):
        results.append(("replayed", *a_replayed(lines, exp["replays"])))
    if exp.get("no_playing_error", True):
        results.append(("no_error", *a_no_error(lines)))
    if "starfish_resolution_sequence" in exp:
        results.append(("starfish_resolution", *a_starfish_resolution_sequence(
            lines, exp["starfish_resolution_sequence"])))
    if "resolution_boundaries_s" in exp:
        results.append(("gst_cadence", *a_gst_resolution_trace(
            gst_lines or [], exp, case.get("gst_trace", {}))))

    for op in case.get("operations", []):
        if op["op"] == "quality_switch":
            steps = op["to"] if isinstance(op["to"], list) else [op["to"]]
            results.append(("quality_switch", *op_quality_switch(lines, steps)))
        elif op["op"] == "seek" and op.get("mode") == "rapid":
            results.append(("seek_rapid", *op_seek_rapid(lines, op["final_s"])))
        elif op["op"] == "seek" and op.get("mode") == "refused":
            results.append(("seek_refused", *op_seek_refused(lines, op.get("target_s", 140))))
        elif op["op"] == "seek":
            results.append(("seek_inplace", *op_seek_inplace(lines, op.get("target_s", 140))))

    # Last, so its evidence sits next to the seek assertion it corroborates.
    results.append(("server_wire", *a_server_wire(
        srv_delta, exp.get("server_opens_min", 1), exp.get("server_range_opens_min", 0),
        exp.get("server_opens_exact"), exp.get("server_range_opens_exact"))))
    return all(ok for _, ok, _ in results), results


def pull_runtime_log(tv, name):
    """Read a case-owned diagnostic after playback; an absent/unreadable trace grades empty."""
    proc = ssh(tv, f"cat {RUNDIR}/{name}", timeout=30)
    return proc.stdout.splitlines() if proc.returncode == 0 else []


def save_case_log(cfg, name, lines):
    """Persist one case's event log, if --save-logs asked for it.

    The app truncates `plxnative-events.log` at every launch and every case relaunches, so the
    only copy of a trace is the one the harness holds in memory. Increment I2 needs those traces
    (transaction records, per-segment acquisition) and re-running a device case to recover a log
    costs a television lease.
    """
    outdir = cfg.get("save_logs")
    if not outdir:
        return None
    os.makedirs(outdir, exist_ok=True)
    path = os.path.join(outdir, f"{name}.log")
    with open(path, "w") as f:
        f.write("\n".join(lines) + "\n")
    return path


def run_pipeline_case(case, cfg, srv, url_base, verbose):
    """One pipeline case. A sibling of `run_case` with every PMS step removed.

    Kept from it: the `make kill` that opens (the app is a singleton on the television, and the
    previous case's is still up), `apply_triggers`' single round-trip that recreates the runtime
    root 1777 and glob-wipes stale triggers, and `stream_case` unchanged — the same early-exit
    grading, the same install check, the same killpg-plus-ssh-reap in its `finally`.
    Dropped: the unscrobble, the viewOffset seed, `also_reset`, and the token injection. There is
    no ratingKey here to reset and no identity to be.
    """
    name = case["name"]
    tv = cfg["tv"]
    run_secs = case.get("run_secs", 30)
    print(f"\n=== {name}  ({case['fixture']}, {case.get('title','')}) ===")
    print(f"    covers: {', '.join(case.get('covers', []))}")

    require_tv(tv, name)
    make(["kill", f"TV={tv}"], timeout=40)
    srv.set_network_profile(case.get("network_profile"))
    # **The REQUEST-indexed shaper, which existed and was never armed.**
    # `serve_fixtures.set_segment_profile` has been implemented and covered by
    # `test_harness.py` since it was written, but this runner only ever called its
    # wall-clock sibling — so `pipe_abr_down_outrun`, the ONLY case that can make a
    # candidate transfer deadline fire, declared a `segment_profile` the run silently
    # ignored, streamed over an unshaped link and could not pass by construction. Both are
    # reset per case (each setter clears when given None), and they COMPOSE — the
    # request-indexed one wins where it applies.
    srv.set_segment_profile(case.get("segment_profile"))
    files = triggers_for_case(case, url_base=url_base)
    apply_triggers(tv, files)
    print("    triggers: " + ", ".join(n + ("=" + c if c is not None else "") for n, c in files))

    before = srv.stats()

    def grade(c, ls):
        """The wire counters are read LIVE, so the early-exit poll grades the same
        `server_wire` the verdict will — a case cannot stop early on a seek whose Range reopen
        has not happened yet."""
        # Re-read every poll, not once: the shaper's phase clock does not start until the first
        # response body, so at the moment this case was set up there were no windows to read.
        c["_dip_windows"] = srv.dip_windows()
        now = srv.stats()
        return evaluate_pipeline(c, ls, (now[0] - before[0], now[1] - before[1]))

    early, why = early_exit_allowed(case, cfg)
    if not early and why:
        print(f"    early exit disabled: {why}")
    print(f"    run-stream (cap {run_secs}s{'' if early else ', early exit off'}) ...")
    lines, elapsed, stopped_early, settled = stream_case(case, cfg, run_secs, early=early,
                                                         evaluator=grade)
    after = srv.stats()
    delta = (after[0] - before[0], after[1] - before[1])

    gst_lines = pull_runtime_log(tv, "plxnative-gst.log") if case.get("gst_trace") else None
    if gst_lines is not None:
        print(f"    GST trace: {len(gst_lines)} line(s)")
    # The shaper's schedule, for the FINAL grade. `grade()` above also sets it, but only the
    # early-exit poll calls `grade()` — under `--no-early` it never runs, and the dip metric then
    # reported "no degraded leg" on cases that plainly had one (observed 2026-08-26 on
    # pipe_abr_brief_dropout and pipe_abr_oscillating_link). Captured here so the verdict path and
    # the poll path see the same windows whichever way the case ended.
    case["_dip_windows"] = srv.dip_windows()
    passed, results = evaluate_pipeline(case, lines, delta, gst_lines=gst_lines)
    report_and_record(cfg, name, passed, results, lines, elapsed, run_secs, stopped_early,
                      settled, verbose)
    if delta[0] == 0:
        # Every case that reaches the television opens at least one body. Zero means the TV never
        # connected AT ALL, and on macOS the overwhelmingly likely reason is not the app: the
        # application firewall drops connections to an ad-hoc python listener silently — no
        # refusal, no log line, the app's open just reads empty. Every assertion then fails as
        # "no line found", which is precisely what a total regression looks like. Say so once.
        print("       NOTE: the fixture server saw NO request from the TV. If this repeats for "
              "every case, it is almost certainly the macOS application firewall — accept the "
              "'allow incoming connections' prompt for this python once, with a human present.")
    return passed, results, lines


def run_pipeline_suite(cases, cfg, srv, url_base, verbose, skipped=()):
    summary = []
    for c in cases:
        try:
            passed, results, _ = run_pipeline_case(c, cfg, srv, url_base, verbose)
        except Exception as e:  # keep the batch going; record the failure
            print(f"    ERROR running {c['name']}: {e}")
            passed, results = False, [("harness", False, str(e))]
        summary.append((c["name"], passed, [l for l, ok, _ in results if not ok]))

    print("\n" + "=" * 72)
    print("SUMMARY  (pipeline tier — no Plex)")
    print("=" * 72)
    npass = nfail = 0
    for nm, passed, fails in summary:
        npass, nfail = (npass + 1, nfail) if passed else (npass, nfail + 1)
        print(f"  [{'PASS' if passed else 'FAIL'}] {nm}" + ("" if passed else "  <- " + ", ".join(fails)))
    for nm, reason in skipped:
        print(f"  [SKIP] {nm}  <- {reason}")
    tail = f", {len(skipped)} skipped" if skipped else ""
    print(f"\n{npass} passed, {nfail} failed of {len(summary)}{tail}")
    return 0 if nfail == 0 else 1


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


def fps_scene_needs_token(scene, has_shared_server=False):
    """Whether this scene must cross the signed-in boot gate.

    A fresh debug install has no stored session. Every route-specific FPS scene except the login
    spinner therefore needs the same temporary test identity as the player tier; otherwise the app
    correctly lands on QR sign-in and the harness misreports zero samples for Home/Detail/etc. A
    shared-server scene also needs the primary credential even if a future one intentionally uses
    the login route.
    """
    return has_shared_server or scene.get("route") != "login"


def run_fps_scene(scene, cfg, token):
    name = scene["name"]
    tv = cfg["tv"]
    route = scene["route"]
    overlay = scene.get("overlay")  # None for home/detail
    loop_floor = scene["loop_floor"]
    warmup = scene.get("warmup_s", 5)
    run_secs = scene.get("run_secs", 18)
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
    # Player FPS baselines were calibrated on the established Original route. Pin that route just
    # as the server matrix does; otherwise a persisted Auto choice turns this into an HLS encoder
    # benchmark and makes the number describe a different workload. Future adaptive FPS scenes
    # opt in explicitly with `"quality": "auto"`.
    if scene.get("tier") == "player":
        files.append(("plxnative-quality", scene.get("quality", "original")))
    # clears every plxnative-* (incl. plxnative-profile) then writes this scene's. Player-tier
    # scenes actually decode video, so they need the test-user token too — appended to the same
    # round-trip via extra= so its value stays off stdout, exactly like the playback cases.
    extras = []
    srv_json = shared_servers_json(cfg, scene)
    # Route-specific scenes need to cross the signed-in boot gate even on a fresh debug install.
    # The login spinner is the one deliberate exception: its contract is the signed-out route.
    # A scene about the SECOND server needs the primary credential for the same reason.
    if fps_scene_needs_token(scene, bool(srv_json)) and cfg.get("inject_token"):
        extras.append(f"printf '%s' '{token}' > {RUNDIR}/plxnative-token")
    if srv_json:
        extras.append(f"printf '%s' {sh_squote(srv_json)} > {RUNDIR}/plxnative-servers")
    apply_triggers(tv, files, extra=extras)
    shown = ", ".join(n + ("=" + c if c is not None else "") for n, c in files)
    print(f"    triggers: {shown or '(none)'}   run {run_secs}s, skip first {warmup} sample(s)")
    if srv_json:
        # same redaction as the playback cases — see `describe_server`.
        print(f"    plxnative-servers: <{describe_server(cfg['shared_server'])}, token redacted>")

    try:
        proc = make(["run", f"TV={tv}", f"RUN_SECS={run_secs}"], timeout=run_secs + 90)
    except subprocess.TimeoutExpired:
        print("    [FAIL] make run timed out")
        return False, "make run timed out"
    lines = filter_log(proc.stdout + "\n" + proc.stderr)
    # Same refusal as the playback cases, and it matters at least as much here: a scene graded
    # against the wrong install's log, or against a release build that never read its triggers,
    # fails on the <5-samples guard and reads as "the app never reached this screen".
    require_install(lines, cfg)

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


def fps_for_tiers(scenes, include_player):
    """The scenes this run will actually execute. One definition, because main() has to know it too
    — it decides from the SELECTED scenes whether a second server is needed, and resolving one for a
    player-tier scene that `--fps` was never going to run is a plex.tv round-trip for nothing."""
    tiers = {"ui"} | ({"player"} if include_player else set())
    return [s for s in scenes if s.get("tier", "ui") in tiers]


def run_fps_suite(scenes, cfg, token, include_player, skipped=()):
    # `scenes` arrives already tier-filtered and already known non-empty: main() does both, and
    # its bail has to happen there anyway, BEFORE arm_teardown commits to driving the television.
    # A second filter-and-bail here was dead code that someone would keep maintaining.
    tiers = {"ui"} | ({"player"} if include_player else set())
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
    for name, reason in skipped:
        print(f"  [SKIP] fps:{name}   <- {reason}")
    tail = f", {len(skipped)} skipped" if skipped else ""
    print(f"\n{len(results) - nfail} passed, {nfail} failed of {len(results)}{tail}")
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


def setup_shared(manifest, cfg, args, entries, what):
    """Resolve the second server if this run needs it. Returns (entries_to_run, skipped_names).

    Three states, and they are deliberately different:
      * nothing wants a second server  -> no plex.tv call at all (the common case stays LAN-only);
      * something wants one and the overlay describes it -> resolve now, loudly, before the TV is
        touched, so a revoked share fails as itself instead of as an empty screen mid-suite;
      * something wants one and the overlay has no `shared_server` -> SKIP those entries with the
        reason printed. Not a failure: an installation with no friend's server is a normal
        installation, and the rest of the matrix is still meaningful there. `--shared-server` is
        the exception -- it was asked for explicitly, so it exits instead of silently doing nothing.
    """
    cfg["shared_all"] = args.shared_server
    named = [e for e in entries if e.get("needs_shared_server")]
    if not (args.shared_server or named):
        return entries, []
    spec = manifest.get("shared_server")
    if not spec:
        msg = (f"no `shared_server` block in {MANIFEST_LOCAL} — see manifest.local.json.example "
               f"(it names a server shared with your account; the token is resolved from plex.tv)")
        if args.shared_server:
            sys.exit(f"--shared-server: {msg}")
        print(f"NOTE: {msg}\n      skipping {len(named)} {what}(s) that need a second server: "
              f"{', '.join(e['name'] for e in named)}")
        return [e for e in entries if not e.get("needs_shared_server")], [e["name"] for e in named]
    cfg["shared_server"] = resolve_shared_server(read_token(), spec)
    # The header line of the whole run, and the one most likely to be pasted somewhere. `handle`
    # decides: a share is a `ref=` tag (`describe_server`), your own second machine is unchanged.
    print(f"second server: {describe_server(cfg['shared_server'])} "
          f"— access token resolved from plex.tv, never printed")
    return entries, []


def main():
    # LIVE OUTPUT. This suite drives a television for ten to twenty minutes, and Python
    # block-buffers stdout the moment it is not a tty — so piping it to a file or a log viewer
    # showed NOTHING until the run ended, which is indistinguishable from a hung harness against a
    # TV that is doing exactly what it was asked to. Line buffering costs nothing here (the output
    # is a few hundred lines) and makes progress visible wherever it is sent.
    try:
        sys.stdout.reconfigure(line_buffering=True)
        sys.stderr.reconfigure(line_buffering=True)
    except AttributeError:  # pragma: no cover — pre-3.7
        pass
    ap = argparse.ArgumentParser(description="webOS Plex player on-device regression harness")
    ap.add_argument("--build", action="store_true", help="cargo + make + make deploy before running")
    ap.add_argument("--filter", default=None, help="run only cases whose name contains this substring")
    ap.add_argument("--suite", default=None, choices=["logic", "codec"],
                    help="run only one suite: 'logic' (seek/resume/audio/subtitle — the engine and "
                         "pump; still covers h264-dp, 4k-hevc-dp and transcode) or 'codec' (the "
                         "play-only decision + Load-payload cases). Default: every case. "
                         "NB distinct from fps_scenes' ui|player 'tier'.")
    ap.add_argument("--list", action="store_true", help="list cases and exit")
    ap.add_argument("--save-logs", metavar="DIR", default=None,
                    help="write each case's full event log to DIR/<case>.log. The app truncates "
                         "its log every launch and each case overwrites the previous one, so a "
                         "trace not captured here is gone when the run ends.")
    ap.add_argument("--tv", default=None, help="override TV IP (default from manifest.local.json)")
    # Deliberately NOT `choices=`: the Makefile's FLAVORS list is the one whitelist, and a second
    # copy here would be the copy that goes stale. A bad value fails on the first `make -s print-*`
    # with the Makefile's own parse-time error, which names the allowed set.
    ap.add_argument("--flavor", default=None, metavar="stable|debug",
                    help="which INSTALL to drive (default: the overlay's `flavour` key, else the "
                         "Makefile's own default). The two builds have separate app ids, separate "
                         "runtime roots and separate sign-ins; everything this run touches is "
                         "derived from this one choice")
    ap.add_argument("--verbose", action="store_true", help="print evidence for passing assertions too")
    ap.add_argument("--no-early", action="store_true",
                    help="don't stop a case as soon as it passes — run the full manifest run_secs. "
                         "Slower by design: it widens the window for a LATE `Playing error` to show "
                         "up, which early exit trades away for speed.")
    ap.add_argument("--owner", action="store_true",
                    help="run as the config.local.h OWNER token (default: run as the overlay's "
                         "test_user, so watch history stays off your real account)")
    ap.add_argument("--shared-server", action="store_true",
                    help="inject the overlay's `shared_server` credentials into EVERY case/scene of "
                         "this run, not just the ones declaring needs_shared_server. For bringing "
                         "up a second-server screen by hand; exits if the overlay has no such block")
    ap.add_argument("--fps", action="store_true",
                    help="run the FPS regression suite (UI tier: home/detail, no video needed)")
    ap.add_argument("--fps-player", action="store_true",
                    help="FPS suite INCLUDING player-tier scenes (info/menu — needs playback, slower)")
    # THE DEFAULT TIER. `--server` opts into the library-backed one; `--pipeline` is accepted and
    # redundant, kept because it is what every recipe written between this tier landing and the
    # inversion says, and because naming the default explicitly is never wrong.
    ap.add_argument("--server", "--integration", dest="server", action="store_true",
                    help="run the SERVER tier instead of the default synthetic one: the 21 "
                         "library-backed cases driven through the whole Plex chain (plex.tv auth, "
                         "/decision, direct-play vs transcode, markers, resume, timeline). Needs a "
                         "PMS, a token and a filled-in manifest.local.json")
    ap.add_argument("--pipeline", action="store_true",
                    help="run the synthetic pipeline tier — the DEFAULT, so this flag is a no-op "
                         "kept for the recipes that name it")
    ap.add_argument("--fixtures-dir", default=None, metavar="DIR",
                    help=f"where the generated pipeline pack lives (default $FIXTURES_OUT/pipeline, "
                         f"i.e. {FIXTURES_ROOT})")
    ap.add_argument("--fixtures-port", type=int, default=0, metavar="PORT",
                    help="port for the fixture HTTP server (default: pick a free one). Pin it when "
                         "a firewall rule names a port")
    args = ap.parse_args()

    # WHICH TIER. The synthetic pipeline tier is the DEFAULT (2026-08-22); the library-backed one
    # is `--server`. The inversion is deliberate and it is about what a bare `./tests/run.py`
    # should mean: the default has to be the thing that runs for everybody, needs no credentials,
    # touches nobody's watch history, and answers "is the PLAYER broken" — which is the question
    # asked far more often than "is my library's metadata right". The server tier costs ten to
    # twenty minutes, a PMS, a token and an overlay; making that the price of typing the obvious
    # command meant most people could not type it at all.
    #
    # The FPS scenes are on the server side of the line whatever else is asked: they navigate a
    # real signed-in Home, so without a token they grade a QR screen.
    server_tier = args.server or args.fps or args.fps_player
    if args.pipeline and server_tier:
        # `--pipeline` is a no-op naming the default, so combining it with anything that selects
        # the other tier is not a preference to resolve — it is two contradictory instructions, and
        # silently honouring one of them is how somebody watches a suite grade the tier they did
        # not ask for and believe the result.
        sys.exit("--pipeline names the DEFAULT tier and cannot be combined with "
                 "--server/--fps/--fps-player, which select the server tier. Pick one.")
    manifest = load_manifest(pipeline_only=not server_tier, tv_override=args.tv, for_listing=args.list)
    # BEFORE anything else, including --list: every path this run uses hangs off it, and the
    # queries are offline and side-effect free (see make_query / the Makefile's PURE_QUERY).
    resolve_flavour(args, manifest)
    cfg = {
        # `.get`, not `[…]`: under `--list` the overlay and `.tv-host` are both optional, so there
        # may be no address at all. Nothing on a listing path dials it — and a RUN cannot reach
        # here without one, because `load_manifest` still exits for that case.
        "tv": args.tv or manifest.get("tv", ""),
        # server-tier only: the synthetic tier talks to no PMS and `load_manifest` does not
        # synthesize the key for it.
        "pms": manifest.get("pms", {}),
        "no_early": args.no_early,
        "save_logs": args.save_logs,
    }
    cases = manifest["cases"]
    if args.suite:
        cases = [c for c in cases if case_suite(c) == args.suite]
    if args.filter:
        cases = [c for c in cases if args.filter in c["name"]]
    # Partition BEFORE anything reads case["rk"]: a skipped case has none. Held as (name, reason)
    # pairs so the summary can say which shape this library is missing, not merely how many.
    cases, item_skipped = partition_skips(cases)

    # Both the listing and the run below need these, and they were computed identically in each.
    fixtures_root = args.fixtures_dir or FIXTURES_ROOT
    pipeline_cases = manifest.get("pipeline_cases", [])

    if args.list and not server_tier:
        # A separate listing, not a filtered one: these cases share no key with the integration
        # matrix (a `fixture` and a `declare`, never an `item` or an `rk`), and the columns that
        # matter — which file, what it declares, whether the pack holds it — have no counterpart
        # in the other table.
        root, pcases = fixtures_root, pipeline_cases
        _resolve_fixtures(pcases, root)
        for c in pcases:
            d = c.get("declare", {})
            decl = f"{d.get('vcodec','?')}/{d.get('acodec','?')}"
            if d.get("dovi", {}).get("profile"):
                decl += f"+DV{d['dovi']['profile']}"
            if d.get("atmos"):
                decl += "+atmos"
            # Only when it is not the 24p every other fixture runs at — a column that repeats
            # the same number eleven times hides the two rows that are the point of it.
            if abs(float(d.get("fps", 24.0)) - 24.0) > 0.01:
                decl += f"@{d['fps']:g}"
            ops = "+".join(o["op"] for o in c.get("operations", [])) or "play"
            # The RESOLUTION, which six of these cases exist to vary (#50/#51) and which was
            # otherwise readable only by opening the manifest — the listing showed the codec pair
            # and the filename, and half the filenames do not carry the raster.
            size = c.get("expect", {}).get("video_size", "-")
            mark = f"  [SKIP: {c['skip']}]" if c.get("skip") else ""
            print(f"{c['name']:30s} {decl:22s} {size:11s} {ops:8s} {c.get('fixture','?')}{mark}")
        print(f"\nfixtures: {root}")
        print(f"install:  {APPID} [{FLAVOUR}] — triggers and log under {RUNDIR}")
        print("\nThe pipeline tier needs no PMS, no token and no manifest.local.json — only a TV "
              "address and\na pack built by `make fixtures-pipeline`.")
        return 0

    if args.list:
        for c in manifest["cases"]:
            ops = "+".join(o["op"] for o in c["operations"])
            mark = "  [+2nd server]" if c.get("needs_shared_server") else ""
            mark += f"  [SKIP: {c['skip']}]" if c.get("skip") else ""
            print(f"{c['name']:32s} suite={case_suite(c):6s} rk={c.get('rk', 'SKIP'):<5} "
                  f"{ops:20s} {', '.join(c.get('covers', []))}{mark}")
        for s in manifest.get("fps_scenes", []):
            tag = s["route"] + (f"/{s.get('overlay')}" if s.get("overlay") else "")
            gates = f"loop_floor={s['loop_floor']}"
            if s.get("fps_floor") is not None:
                gates += f" fps_floor={s['fps_floor']}"
            if s.get("fps_ceiling") is not None:
                gates += f" fps_ceiling={s['fps_ceiling']}"
            mark = "  [+2nd server]" if s.get("needs_shared_server") else ""
            mark += f"  [SKIP: {s['skip']}]" if s.get("skip") else ""
            print(f"fps:{s['name']:28s} tier={s.get('tier','ui'):6s} {tag:16s} {gates}{mark}")
        print(f"\ninstall: {APPID} [{FLAVOUR}] — runtime root {RUNDIR}")
        # Offline, so this reports what the OVERLAY says — nothing is resolved and plex.tv is not
        # called. It is still the answer to "will the [+2nd server] entries above actually run".
        # `describe_spec` rather than `describe_server` for exactly that reason: with no resolved
        # `sourceTitle` there is no way to tell a share from your own second machine here, and the
        # stricter reading is the safe one to guess.
        ss = manifest.get("shared_server")
        if ss:
            print(f"\nsecond server: {describe_spec(ss)} — configured; its access token "
                  f"is resolved from plex.tv at run time")
        else:
            print(f"\nsecond server: not configured in {os.path.basename(MANIFEST_LOCAL)} — any "
                  f"[+2nd server] entry above is SKIPPED (and --shared-server exits)")
        return 0

    # PIPELINE tier — the player with no Plex behind it. Placed here, after --list and before
    # everything that reads a token, because none of what follows applies: no identity to resolve,
    # no second server, no ratingKey to reset.
    if not server_tier:
        if args.suite:
            sys.exit("--suite selects among the SERVER tier's cases; pass --server with it. "
                     "(The synthetic tier is the default and has no logic/codec split.)")
        if args.owner or args.shared_server:
            sys.exit("--owner / --shared-server are identities on a PMS; the default tier talks to "
                     "no server. Pass --server if you meant the library-backed cases.")
        root, pcases = fixtures_root, pipeline_cases
        if args.filter:
            pcases = [c for c in pcases if args.filter in c["name"]]
        _resolve_fixtures(pcases, root)
        pcases, pskipped = partition_skips(pcases)
        # Bail BEFORE the server is started and before arm_teardown: arming commits to driving the
        # television, and its cleanup closes the app on EVERY exit path including this one. A run
        # with nothing to grade must not close an app somebody is watching.
        if not pcases:
            sys.exit(f"no pipeline case can run (fixtures dir: {root})"
                     + (":\n  " + "\n  ".join(f"{n}  <- {r}" for n, r in pskipped)
                        if pskipped else f" — --filter {args.filter!r} matched nothing"))
        srv, url_base = serve(root, port=args.fixtures_port,
                              sink=(lambda m: print(f"      [srv] {m}")) if args.verbose else None)
        # LIFO: registered after the server and BEFORE arm_teardown, so on the way out the TV is
        # cleaned FIRST and the bytes are pulled second. The other order stops serving an app that
        # is still playing, which turns every interrupted run into a demux failure in the log.
        atexit.register(srv.shutdown)
        print(f"install: {APPID} [{FLAVOUR}] — triggers and log under {RUNDIR}")
        print(f"fixtures: {root}")
        print(f"serving:  {url_base}  (the TV fetches from this address)")
        # The television is a mutex for this tier too, and MORE so than for the others: this is
        # the default suite now, so it is the one somebody runs without thinking. It closes the
        # app between cases, wipes the runtime root and grades a log it assumes only it is
        # writing — every reason the server tier takes the lock applies here unchanged.
        #
        # Immediately before `arm_teardown`, matching the other two call sites and for a reason:
        # `teardown` is what releases the lease, so any statement between taking it and arming
        # that is a window where a failure leaks the television.
        acquire_tv_lock(cfg["tv"], f"tests/run.py ({len(pcases)} synthetic cases) [{FLAVOUR}]")
        arm_teardown(cfg["tv"])
        if args.build:
            do_build(cfg["tv"])
        return run_pipeline_suite(pcases, cfg, srv, url_base, args.verbose, pskipped)

    # FPS regression suite — a separate path from the playback cases. UI-tier scenes need no video
    # (and no PMS token); --fps-player adds the info/menu scenes, which decode video as the test user.
    if args.fps or args.fps_player:
        if args.suite:
            sys.exit("--suite selects playback cases; the FPS scenes use --fps / --fps-player")
        include_player = args.fps_player
        selected = fps_for_tiers(manifest.get("fps_scenes", []), include_player)
        # Same partition as the playback cases, and for a sharper reason: run_fps_scene's "$rk"
        # substitution reads scene["rk"] directly, and the KeyError landed in the batch's blanket
        # `except` as `[FAIL] ERROR: 'rk'` -- a false FAILURE, indistinguishable from a regression.
        selected, fps_skipped = partition_skips(selected)
        scenes, _skipped = setup_shared(manifest, cfg, args, selected, "scene")
        # Bail BEFORE read_token() and arm_teardown(): arming commits to driving the television,
        # and its cleanup closes the app on every exit path -- including this one. A run with
        # nothing left to grade must not close an app somebody is watching (the same reason
        # `--list` and a no-match `--filter` return before this point).
        if not scenes:
            sys.exit("no FPS scene left to run"
                     + (":\n  " + "\n  ".join(f"{n}  <- {r}" for n, r in fps_skipped)
                        if fps_skipped else f" for tier(s) {'ui+player' if include_player else 'ui'}"))
        token = None
        # A second-server scene needs the FIRST server's token too, whatever its tier: without it
        # the app boots to QR sign-in and the scene grades a screen it never reached.
        if include_player or cfg.get("shared_server"):
            admin_token = read_token()
            test_user = manifest.get("test_user")
            if args.owner or not test_user:
                token, cfg["inject_token"] = admin_token, True  # no baked token in the binary
            else:
                token = fetch_managed_user_token(admin_token, cfg["pms"]["host"],
                                                 cfg["pms"]["port"], test_user["id"])
                cfg["inject_token"] = True
        print(f"install: {APPID} [{FLAVOUR}] — triggers and log under {RUNDIR}")
        acquire_tv_lock(cfg["tv"], f"tests/run.py --fps ({len(scenes)} scenes) [{FLAVOUR}]")
        arm_teardown(cfg["tv"])
        if args.build:
            do_build(cfg["tv"])
        return run_fps_suite(scenes, cfg, token, include_player, fps_skipped)

    if not cases:
        # "matched nothing" and "matched, then skipped for want of an item" are different answers,
        # and printing the first for the second is how a filter reads as a typo it is not.
        if item_skipped:
            sys.exit(f"every case matching --filter {args.filter!r} / --suite {args.suite!r} is "
                     f"skipped for want of a library item:\n  "
                     + "\n  ".join(f"{n}  <- {r}" for n, r in item_skipped))
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
    print(f"install: {APPID} [{FLAVOUR}] — triggers and log under {RUNDIR}")
    print(f"test identity: {cfg['user_label']}  (playback + watch-history isolation)")

    # The second server, if anything selected needs one. AFTER the identity above and before the TV
    # is touched: it can exit, and an exit here still leaves an app nobody has driven yet alone.
    cases, shared_skipped = setup_shared(manifest, cfg, args, cases, "case")
    if not cases:
        sys.exit("no selected case can run: "
                 + (f"{len(item_skipped)} skipped for want of a library item, " if item_skipped else "")
                 + f"{len(shared_skipped)} for want of a second server "
                 + f"(see {MANIFEST_LOCAL})")

    acquire_tv_lock(cfg["tv"], f"tests/run.py ({len(cases)} cases) [{FLAVOUR}]")
    arm_teardown(cfg["tv"])
    if args.build:
        do_build(cfg["tv"])

    # The link conditioner, started ONCE for the tier — the binary points at its port for every
    # case, conditioned or not, so it cannot be a per-case resource. `usable` decides whether a
    # case that names a `link_profile` runs or skips; nothing else changes.
    _host, listen = compiled_pms_endpoint()
    cond = LinkConditioner(listen, (cfg["pms"]["host"], cfg["pms"]["port"]), cfg["tv"])
    cond.start()
    atexit.register(cond.close)
    shaped, link_skipped = [c for c in cases if c.get("link_profile")], []
    if shaped:
        if cond.usable:
            print(f"link conditioner: :{listen} -> PMS, {len(shaped)} shaped case(s)")
        else:
            print(f"link conditioner UNAVAILABLE — {cond.why}")
            for c in shaped:
                print(f"    SKIP {c['name']}: needs a conditioned link")
            cases = [c for c in cases if not c.get("link_profile")]
            link_skipped = shaped

    summary = []
    for c in cases:
        try:
            passed, results, _ = run_case(c, cfg, token, args.verbose, cond=cond)
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
    # A case that was never run is reported, never counted as a pass: an installation missing a
    # media shape -- or a friend's server -- must be able to see, in the summary, exactly what its
    # matrix did not cover. The bare pass count is the number that gets quoted; it must never be
    # quotable without the skips beside it.
    for name, reason in item_skipped:
        print(f"  [SKIP] {name}  <- {reason}")
    for name in shared_skipped:
        print(f"  [SKIP] {name}  <- needs a second server; none configured in "
              f"{os.path.basename(MANIFEST_LOCAL)}")
    for c in link_skipped:
        print(f"  [SKIP] {c['name']}  <- needs a conditioned link: {cond.why}")
    nskip = len(shared_skipped) + len(item_skipped) + len(link_skipped)
    tail = f", {nskip} skipped" if nskip else ""
    print(f"\n{npass} passed, {real_fail} failed, {nxfail} known-gap of {len(summary)}{tail}")
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
            # AFTER teardown, never before: the close, the trigger wipe and the token clear are
            # still device work, and handing the lock back first would let the next lane start
            # driving an app this one is still closing.
            release_tv_lock(_TEARDOWN_TV)
    sys.exit(code)
