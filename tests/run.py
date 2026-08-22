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
    ss = manifest.get("shared_server", {})
    stray = [f"{k}={v}" for k, v in
             [("pms.host", manifest["pms"].get("host")), ("tv", manifest["tv"])]
             + [(f"items.{k}", v) for k, v in items.items()]
             + ([("test_user.id", manifest["test_user"].get("id"))] if "test_user" in manifest else [])
             + [(f"shared_server.{k}", ss.get(k)) for k in ("machine_id", "name", "host")]
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
def triggers_for_case(case):
    """
    Map a case's operations -> the plxnative-* files to write in the TV's runtime root.
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
    for name, content in files:
        if content is None:
            parts.append(f"touch {RUNDIR}/{name}")
        else:
            # single-quote the content; rks / "0,6" / "mkv" never contain quotes
            parts.append(f"printf '%s' '{content}' > {RUNDIR}/{name}")
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
    """Leave the TV as we found it. Runs on EVERY exit — pass, fail, Ctrl-C, SIGTERM, crash.

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
    global _TEARDOWN_TV
    _TEARDOWN_TV = tv


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
    proc = subprocess.Popen(make_argv(["run-stream", f"TV={cfg['tv']}"]),
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                            text=True, bufsize=1,
                            # own process group: terminating `make` alone would orphan the
                            # sshpass/ssh child and leave the remote tail (and the app) attached.
                            start_new_session=True)
    lines, done = [], threading.Event()
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
    # And once more now the log is complete. The in-loop check cannot refuse an ABSENT boot line —
    # while a case is still running, "not there" and "not there yet" are the same thing.
    if not install_ok:
        require_install(list(lines), cfg)
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
    extras = []
    srv_json = shared_servers_json(cfg, scene)
    # A UI-tier scene normally needs no PMS token (it draws whatever the boot lands on), but a scene
    # about the SECOND server needs the first one's credentials to get past the boot gate to Home at
    # all — otherwise it sits on the QR sign-in screen and grades a route it never reached.
    if (is_player or srv_json) and cfg.get("inject_token"):
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


def run_fps_suite(scenes, cfg, token, include_player):
    tiers = {"ui"} | ({"player"} if include_player else set())
    scenes = fps_for_tiers(scenes, include_player)
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
    args = ap.parse_args()

    manifest = load_manifest()   # case definitions + the gitignored local overlay, merged
    # BEFORE anything else, including --list: every path this run uses hangs off it, and the
    # queries are offline and side-effect free (see make_query / the Makefile's PURE_QUERY).
    resolve_flavour(args, manifest)
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
            mark = "  [+2nd server]" if c.get("needs_shared_server") else ""
            print(f"{c['name']:32s} suite={case_suite(c):6s} rk={c['rk']:<5} {ops:20s} "
                  f"{', '.join(c.get('covers', []))}{mark}")
        for s in manifest.get("fps_scenes", []):
            tag = s["route"] + (f"/{s.get('overlay')}" if s.get("overlay") else "")
            gates = f"loop_floor={s['loop_floor']}"
            if s.get("fps_floor") is not None:
                gates += f" fps_floor={s['fps_floor']}"
            if s.get("fps_ceiling") is not None:
                gates += f" fps_ceiling={s['fps_ceiling']}"
            mark = "  [+2nd server]" if s.get("needs_shared_server") else ""
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

    # FPS regression suite — a separate path from the playback cases. UI-tier scenes need no video
    # (and no PMS token); --fps-player adds the info/menu scenes, which decode video as the test user.
    if args.fps or args.fps_player:
        if args.suite:
            sys.exit("--suite selects playback cases; the FPS scenes use --fps / --fps-player")
        include_player = args.fps_player
        scenes, _skipped = setup_shared(
            manifest, cfg, args,
            fps_for_tiers(manifest.get("fps_scenes", []), include_player), "scene")
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
        return run_fps_suite(scenes, cfg, token, include_player)

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
    print(f"install: {APPID} [{FLAVOUR}] — triggers and log under {RUNDIR}")
    print(f"test identity: {cfg['user_label']}  (playback + watch-history isolation)")

    # The second server, if anything selected needs one. AFTER the identity above and before the TV
    # is touched: it can exit, and an exit here still leaves an app nobody has driven yet alone.
    cases, shared_skipped = setup_shared(manifest, cfg, args, cases, "case")
    if not cases:
        sys.exit("every selected case needs a second server, and none is configured "
                 f"(see {MANIFEST_LOCAL})")

    acquire_tv_lock(cfg["tv"], f"tests/run.py ({len(cases)} cases) [{FLAVOUR}]")
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
    # A case that was never run is reported, never counted as a pass: an installation with no
    # friend's server must be able to see, in the summary, exactly what its matrix did not cover.
    for name in shared_skipped:
        print(f"  [SKIP] {name}  <- needs a second server; none configured in "
              f"{os.path.basename(MANIFEST_LOCAL)}")
    tail = f", {len(shared_skipped)} skipped" if shared_skipped else ""
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
