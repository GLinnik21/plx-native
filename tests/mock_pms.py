#!/usr/bin/env python3
"""A SYNTHETIC Plex Media Server — enough of the PMS REST surface for the app to boot to Home,
browse a library, open a detail page, a season, a person, search, mark watched, and start a play,
with NOT ONE household byte anywhere in it.

Why it exists (restructure spec §5.6): every committed replay fixture and every focus-fingerprint
flow is recorded against THIS server, on the simulator, so the recording can be committed to a
public repository without a scrub pass that cannot recognise a title. Every string a real PMS
would fill with a title, a name, a summary or a tag is drawn here from one CLOSED ALPHABET —
`s` + eight lowercase hex digits (`s3fa90c12`) — plus the protocol's own constants (`movie`,
`h264`, `home.continue`, …). `tests/test_harness.py` verifies committed fixtures against exactly
that alphabet, which is decidable; "does this look like a title" is not.

What it is NOT: a PMS emulator. It answers the endpoints `plex/{library,hubs,transcoder,
timeline}.rs` request (`docs/plex-openapi.json` is the spec they follow) with the FIELDS
`plex/models.rs` deserialises, and nothing else. An unknown path gets an empty container and a
line on stderr naming it, so extending it is one function. Numbers go on the wire as numbers;
the models fold either form.

Deterministic: the library is generated from `--seed` and never from the clock, so two servers
started with one seed answer byte-identically, which is what a fixture recorded against one and
replayed against the other needs.

`--media DIR` adds two fixed verification items backed only by the synthetic files made by
`tests/fixtures/make_fixtures.py --only mockverify`. Their stream records are derived from those
files with ffprobe at startup, never from a household library or account. Without `--media`, the
library and the old deliberately undecodable part response are unchanged.

`--extra-media FILE` (repeatable) adds one movie per arbitrary file outside that fixture set —
e.g. a hand-generated Dolby Vision asset — with its DOVI*/colorTrc/bitDepth/container fields read
from ffprobe exactly like `--media`, at a fixed ratingKey block (990001+) that cannot collide with
the generated library or the `--media` verification ids. The ratingKey-to-file mapping is printed
at startup.

    python3 tests/mock_pms.py --port 32499              # serve until Ctrl-C
    python3 tests/mock_pms.py --port 32499 --selftest   # prove the shapes without an app
    python3 tests/mock_pms.py --port 32499 --movies 321 --rail-fixture  # multi-page A–Z rail
    python3 tests/mock_pms.py --host 0.0.0.0 --media /path/to/mockverify  # account-free TV set

`--catalog tests/demo_library/catalog.json` is the one exception to the closed alphabet, and a
deliberate one: the DEMO LIBRARY behind the documentation screenshots (`make screenshots`). It
serves real titles, credits and synopses of openly licensed and public-domain films, with the
artwork `tools/demo_library.py derive` built from the pinned sources in
`tests/demo_library/assets.json`. Its clock is pinned (`now` in the catalog), so it is as
deterministic as a seeded library; `--hero SLUG` moves one film to the front of Continue Watching,
which is the home hero's first slot. An episode marked `stand_in` plays: its media is a black,
silent file of the episode's catalog length that `derive` made (never the work itself), which is
enough for the simulator's clock sink to run the player over it. And a `continuous=1` PlayQueue
of an episode carries the rest of its show after it, as PMS's does, so Up Next has a successor.
Nothing recorded against it may be committed as a fixture — the harness's alphabet check would
refuse it, correctly.

Every mode also answers the four plex.tv calls of the QR sign-in (`/api/v2/pins`, the poll, the
QR image, and `/api/v2/user`) with a fixed demo code, for an app booted with
`plxnative-plextv=http://127.0.0.1:<port>`: the sign-in screen can then be driven and captured
without touching plex.tv. The poll stays pending unless `--authorize-after N` (implied, N=2, by
`--plaintext-only-lan`) links the code on the Nth poll with a synthetic account token, so a
sign-in completes end to end without any real account.
`GET`/`PUT /api/v2/user/profile` also reads and edits an in-memory synthetic profile;
`/api/v2/user` returns the same preferences for playback warmup. Changes last only until the
mock stops, and PUTs are recorded in the mock write log. Ordinary mode advertises itself through
`/api/v2/resources` and supplies one synthetic Demo owner in the Home roster, so mock QR sign-in
can reach Settings without a pre-seeded session.

The app reaches it as any other server: `make sim-shot SIM_PMS=127.0.0.1 SIM_PORT=32499` with
any non-empty string in `$SIM_DIR/plxnative-token` (the token is accepted, never checked).

`--plaintext-only-lan` reproduces Sentry PLX-NATIVE-10: a person's OWN server (`owned=true`) on
their LAN, whose owner never turned on port forwarding, so every HTTPS route plex.tv could name
for it — a `*.plex.direct` connection AND relay — fails, while the plaintext local address still
answers. It makes `/api/v2/resources` return exactly ONE resource, no relay connection at all,
and two connections: an `https://<dashed-ip>.<hash>.plex.direct:<port>` one with `local=true` that
FAILS (a real TCP listener that accepts and immediately closes, i.e. a genuine TLS handshake
failure — or, with `--insecure-fail-mode unreachable`, a port nothing listens on at all), and a
plain `http://<advertise-ip>:<port>` one with `local=true` that is THIS SAME server, answering
`/identity` tokenless. `--advertise-ip` sets the LAN address the resources row carries (default:
this host's detected primary LAN IPv4, else `127.0.0.1` with a startup warning — 127.0.0.1 is
loopback, which the app's own eligibility check classifies as NOT a LAN address, so the consent
path this mode exists to exercise will never trigger with that fallback; pass a real address).
Bind with `--host 0.0.0.0` so a device other than this one can actually reach it. Expected app
behaviour: a "Connect without encryption?" consent offer, never a dead end.

The full reproduction needs a developer build to take the STORE credential policy — every
`devtriggers` build otherwise lets a token ride plaintext and never needs the consent — and a QR
sign-in against this mock (it authorizes the code by itself, on the 2nd poll):

    1. python3 tests/mock_pms.py --host 0.0.0.0 --plaintext-only-lan --advertise-ip <HOST-LAN-IP>
       (the LAN plaintext leg the app connects to directly).
    2. `plex_tv()` in rust-modules/plex/src/plex/account.rs accepts only a `127.0.0.1`/`localhost`
       trigger — the trigger carries the account token, and that restriction is deliberate and
       must stay. So make plex.tv loopback ON THE DEVICE with a reverse tunnel from the host
       (hold the TV lock first):
         ssh -N -R 32499:127.0.0.1:32499 root@<TV-HOST>
    3. in the install's runtime root, arm (empty files unless shown):
         plxnative-storepolicy                       store HttpsOnly policy (dev builds only)
         plxnative-login                             boot to the QR sign-in
         plxnative-plextv = http://127.0.0.1:32499
    4. launch. The events log shows, in order:
         - the sign-in read-out "Couldn't sign in" with the reason "Your Plex server is on this
           network but can't be reached securely. Select Connect to connect without encryption."
           and the primary *Connect*
         - *Connect* opens "Connect without encryption?" seated on *Not now*; its *Connect* logs
           `plaintext: user allowed an unencrypted connection on this network` and retries
         - the retry mints the grant (`security: consented plaintext credentials for one server at …`),
           then `security: plaintext PMS credentials sent under a consented grant` — the token
           rides http://<HOST-LAN-IP>:32499 under it — Home loads from this mock
         - Settings → Unencrypted connections shows the server ON; turning it off logs
           `settings: unencrypted connections turned off for one server` and withdraws the grant
           at once
Loopback (the simulator) is never LAN-eligible, so steps 3–4 on the Mac stop at the ineligible
read-out; the consent flow itself needs the TV and a real LAN address.

    # TV (replace 192.168.0.10 with the host's actual LAN IPv4; never a real private address from
    # a gitignored file — hold the TV lock for the device commands below):
    python3 tests/mock_pms.py --host 0.0.0.0 --plaintext-only-lan --advertise-ip 192.168.0.10
    # `plxnative-plextv` only ever accepts a loopback address (see step 2 above), so make plex.tv
    # loopback ON THE DEVICE with a reverse tunnel from the host, then arm the loopback trigger:
    # ssh -N -R 32499:127.0.0.1:32499 root@<TV-HOST>
    # plxnative-plextv=http://127.0.0.1:32499

    # Simulator (loopback is both plex.tv and the LAN address it advertises; still exercises the
    # HTTPS-fails / plaintext-answers shape, just not the loopback-ineligibility warning):
    python3 tests/mock_pms.py --plaintext-only-lan --advertise-ip 127.0.0.1
    # plxnative-plextv=http://127.0.0.1:32499

Issue #266 (Boost Dialog / Normalize Loudness) fixtures and flags, every one default-off except
the loudness attribute itself:

`--no-plex-pass` answers `/` and `/identity` with `myPlexSubscription=false` (the version string
is untouched — `tools/mock-guest.py` keys off it). Every generated and verification audio stream
otherwise carries `canNormalizeLoudness="1"` (PMS string-encodes booleans on the wire); pass
`--no-loudness-analysis` to omit the key entirely, modelling a server that never ran loudness
analysis on the source.

`/video/:/transcode/universal/decision` answers a live `boostDialog=1` or `normalizeLoudness=1`
like measured PMS 1.43.4 with Plex Pass (M1/M2 in the design plan): the Part's decision becomes
"transcode", the video stream stays "copy", and the audio stream becomes "transcode" with
`codec=ac3` at the SAME channel count as the source — whether the request shape was a remux
(`directPlay=0&directStream=1`) or an MDE probe (`directPlay=1&directStreamAudio=1`); a param of
`0` or absent is byte-identical to today. `--refuse-enhancements` answers instead with the
server-refusal shape (`generalDecisionCode=2000`, a `transcodeDecisionCode`/`transcodeDecisionText`
naming the enhancement, `route/plan.rs::refusal` reads this). `--ignore-enhancements` accepts the
params but leaves the audio decision "copy" (an old server that does not act on them). Both can
also be flipped live, mid-run, with `POST /_mock/config` — JSON body
`{"refuse_enhancements": true, "ignore_enhancements": false, "plex_pass": false}` — so one test
case can resolve once, then refuse only from the next decision on.

Five fixed fixture movies (own section id 3, so ordinary section/rail counts and hashes over
sections 1/2 are untouched, but each is reachable directly at its ratingKey) exercise the offering
and fallback rules:
  - 960001 `ac3_2ch_capable` — AC3 2.0.
  - 960002 `aac51_capable` — AAC 5.1.
  - 960003 `dv_p8` — Dolby Vision profile 8 (`DOVIPresent`/`DOVIProfile` etc. on the video stream).
  - 960004 `default_embedded_srt` — a default embedded SRT subtitle.
  - 960005 `server_selected_external_srt` — an external SRT stream, `selected=true`, `key` set,
    `codec="srt"`.

`GET /_mock/requests` returns every request `handle()` routed — `{"method", "path", "query",
"session"}`, with any query key containing "token" stripped — as a JSON array, in arrival order;
`DELETE /_mock/requests` clears it. (Raw ranged media-byte GETs under `/library/parts/<id>`
bypass `handle()`, as before, and stay on the older `pms.requests` list only.)

`--transcode-fixture PATH` serves that file, with the same Range/206 handling as `--media`'s part
bytes, for every `/video/:/transcode/universal/start...` request in place of the 404 a session
with no server-side transcoder otherwise returns. It is for exercising the TV's playback pipeline
against a real container; it claims no DSP realism for the enhancement itself.
"""
import argparse
import hashlib
import json
import os
import pathlib
import random
import re
import socket
import struct
import subprocess
import sys
import threading
import time
import urllib.parse
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))  # demo_library.qr

VERIFY_SHOW_RK = 900001
VERIFY_SEASON_RK = 900002
V1_RATING_KEY = 900003
V2_RATING_KEY = 900004
VERIFY_PARTS = {V1_RATING_KEY: 900101, V2_RATING_KEY: 900102}
VERIFY_SIDECAR_ID = 901009
# Subtitle search (`MockPms.subtitle_search`): candidate and installed stream ids live in their own
# bands so neither can collide with a generated or verification stream.
SUB_CANDIDATE_BASE, SUB_INSTALLED_BASE = 8_100_000, 8_200_000
# The languages the mock agent "finds" subtitles in: 2-letter query -> (languageCode, language).
# Any other well-formed code is an empty answer — the not-found case, which is not a failure.
SUB_SEARCH_LANGS = {"en": ("eng", "English"), "nl": ("nld", "Nederlands"), "es": ("spa", "Español")}
# --extra-media ids. Generated movies are 1001..(1000+movies<=1000) => max 2000; shows/seasons/
# episodes nest as rk*10+n from 2001, so a default-sized run never reaches six digits; VERIFY_*
# stops at 901009. 990001+ sits clear of all three, with a wide gap before the next round number
# so a deliberately huge --shows run remains this module's problem to notice, not to silently hit.
EXTRA_MEDIA_RK_BASE = 990001
EXTRA_MEDIA_PART_ID_BASE = 991001
# #266 enhancement fixtures (module doc): own librarySectionID (3, never registered in
# `Library.sections`), so they never appear in a section/rail listing or count, but are always
# present and reachable directly by ratingKey. 960001..960005 sits clear of the generated range
# (max ~205026 for a --shows-heavy run, see the EXTRA_MEDIA comment above) and of EXTRA_MEDIA/VERIFY.
ENHANCEMENT_SECTION_ID = 3
ENHANCEMENT_PART_ID_BASE = 961001
ENHANCEMENT_AC3_2CH_RK = 960001
ENHANCEMENT_AAC_51_RK = 960002
ENHANCEMENT_DV_P8_RK = 960003
ENHANCEMENT_DEFAULT_SRT_RK = 960004
ENHANCEMENT_EXTERNAL_SRT_RK = 960005
VERIFY_PREFS = [
    {"id": "audioLanguage", "value": "de"},
    {"id": "subtitleLanguage", "value": "en"},
    {"id": "subtitleMode", "value": "2"},
]
# `--plaintext-only-lan` (PLX-NATIVE-10). Not a name — the fixed 32-hex label a real
# `*.plex.direct` hostname carries ahead of the dashed IP; the closed alphabet governs the
# synthetic library's titles/tags, not a protocol constant like this or "h264".
PLEX_DIRECT_HASH = "0123456789abcdef0123456789abcdef"

# ---------------------------------------------------------------- the closed alphabet -------

def sname(rng):
    """One name from the closed alphabet: `s` + 8 lowercase hex digits."""
    return "s%08x" % rng.getrandbits(32)


def swords(rng, n):
    return " ".join(sname(rng) for _ in range(n))


# The shortest query PMS answers: a one-character query came back with every hub empty (measured
# against PMS 1.43.3; the app's `search::MIN_QUERY` never sends one).
SEARCH_MIN_QUERY = 2


def search_words(text):
    return re.findall(r"[0-9a-z]+", text.lower())


def search_matcher(query):
    """What `/hubs/search` counts as a hit: every word of the query begins a word of the name.
    "sp" finds "Spring" and "Sprite Fright"; "in" finds neither "Spring" nor "Sintel".

    The WORD-PREFIX rule is an assumption, not a measurement: docs/pms-api.md §3b probed the
    response shape, not the matching, and the spec only says PMS "looks for partial matches" and
    spell-checks. It is the conservative reading — a mid-word match would make figures show hits a
    real server may not return. Spell-checking is not modelled; the related results are, in
    `Library.search`."""
    want = search_words(query)
    if len(query.strip()) < SEARCH_MIN_QUERY or not want:
        return lambda name: False

    def hits(name):
        words = search_words(name)
        return all(any(w.startswith(q) for w in words) for q in want)
    return hits


# ---------------------------------------------------------------- the generated library ----

class Library:
    """Two sections — movies (key 1) and shows (key 2) — with people, genres, collections,
    seasons, episodes, media parts, chapters, markers, watch state and blur colours. Generated
    ids are dense from 1; the opt-in verification ids are fixed and deliberately conspicuous."""

    def __init__(self, seed=1, movies=48, shows=6, seasons=2, episodes=6, rail_fixture=False,
                 media=None, extra_media=None, loudness_analysis=True):
        # Movie keys start at 1001; shows start at 2001. Refuse a fixture that would silently
        # overwrite a film with a show instead of exercising the requested listing size.
        if not 0 <= movies <= 1000:
            raise ValueError("movies must be between 0 and 1000")
        # #266: whether a generated/verification audio stream carries canNormalizeLoudness.
        self.loudness_analysis = loudness_analysis
        rng = random.Random(seed)
        self.seed = seed
        self.machine = "s%08x" % rng.getrandbits(32) + "s%08x" % rng.getrandbits(32)
        self.friendly = sname(rng)
        self.items = {}  # rk -> dict (the wire item, mutable for watch state)
        self.people = {}  # id -> tag dict
        self.genres = {}  # id -> tag dict
        self.collections = {}
        self.sections = [
            {"key": "1", "type": "movie", "title": sname(rng), "uuid": sname(rng)},
            {"key": "2", "type": "show", "title": sname(rng), "uuid": sname(rng)},
        ]
        for i in range(1, 21):
            self.people[i] = {"id": i, "tag": sname(rng), "tagKey": sname(rng),
                              "thumb": f"/library/metadata/people/{i}/thumb/1"}
        for i in range(1, 9):
            self.genres[i] = {"id": i, "tag": sname(rng), "filter": f"genre={i}"}
        for i in range(1, 4):
            self.collections[i] = {"id": i, "ratingKey": 50000 + i, "tag": sname(rng)}
        rk = 1000
        part = 1
        for _ in range(movies):
            rk += 1
            part += 1
            self.items[rk] = self._movie(rng, rk, part)
        rk = 2000
        for _ in range(shows):
            rk += 1
            show = self._show(rng, rk)
            self.items[rk] = show
            for s in range(1, seasons + 1):
                srk = rk * 10 + s
                season = self._season(rng, srk, show, s)
                self.items[srk] = season
                for e in range(1, episodes + 1):
                    erk = srk * 10 + e
                    part += 1
                    self.items[erk] = self._episode(rng, erk, season, show, e, part)
        self._add_empty_collection(sname(rng))
        # watch state: a third watched, a sixth in progress (the Continue Watching deck)
        for i, it in enumerate(sorted(self.items.values(), key=lambda x: x["ratingKey"])):
            if it["type"] not in ("movie", "episode"):
                continue
            if i % 3 == 0:
                it["viewCount"] = 1
                it["lastViewedAt"] = 1_700_000_000 + i
            elif i % 6 == 1:
                it["viewOffset"] = it["duration"] // 3
                it["lastViewedAt"] = 1_700_100_000 + i
        self._roll_up()
        self._add_enhancement_fixtures()
        if media is not None or extra_media:
            self.media_files = {}
            self.sidecars = {}
            self.verification_streams = {}
            self.media_content_type = {}
        if media is not None:
            self._add_verification_media(pathlib.Path(media))
        if extra_media:
            self._add_extra_media([pathlib.Path(p) for p in extra_media])
        if rail_fixture:
            # Plex's sort title can differ from its displayed title. Only this explicit mode
            # gives it a prefix; ordinary seeded fixtures keep every generated byte unchanged.
            # Gaps between letters exercise directory-based movement, not a hardcoded A–Z list.
            for section in self.sections:
                rows = sorted((it for it in self.items.values()
                               if it["librarySectionID"] == int(section["key"])
                               and it["type"] in ("movie", "show")), key=lambda it: it["ratingKey"])
                for index, it in enumerate(rows):
                    it["titleSort"] = "ACFMZ"[index % 5] + " " + it["titleSort"]

    def _probe(self, path):
        if not path.is_file():
            raise ValueError(f"verification media is missing: {path}")
        try:
            raw = subprocess.check_output([
                "ffprobe", "-v", "error", "-show_format", "-show_streams", "-of", "json",
                str(path),
            ], stderr=subprocess.STDOUT)
            return json.loads(raw)
        except FileNotFoundError as e:
            raise ValueError("ffprobe is required with --media/--extra-media") from e
        except (subprocess.CalledProcessError, json.JSONDecodeError) as e:
            detail = getattr(e, "output", b"").decode("utf-8", "replace").strip()
            raise ValueError(f"ffprobe failed for {path}: {detail or e}") from e

    # The container a real PMS would name, and the wire Content-Type for it. Only the two
    # containers this fixture set ever deals in — extend here, never with a per-caller default.
    _CONTAINER_CONTENT_TYPE = {"mkv": "video/x-matroska", "mp4": "video/mp4"}

    @staticmethod
    def _resolution_label(width, height):
        """The (videoResolution, displayTitle-prefix) pair PMS derives from the DECODED frame
        size — e.g. `("4k", "4K")` — never a hardcoded 1080p regardless of the source file.
        Thresholds follow PMS's own videoResolution buckets (sd/720/1080/4k)."""
        if width >= 3800 or height >= 2000:
            return "4k", "4K"
        if height >= 1000:
            return "1080", "1080p"
        if height >= 576:
            return "720", "720p"
        return "sd", "SD"

    @staticmethod
    def _dovi_wire(stream):
        """The eight `DOVI*` keys (docs/pms-api.md; spellings verified live against a real PMS
        2026-08-21), derived from ffprobe's "DOVI configuration record" side-data entry on this
        VIDEO stream. Returns {} — sending none of the keys — when no such side-data is present,
        which is the shape `metadata::Dovi`'s never-convict-on-silence rule expects from an
        ordinary HDR10/SDR file; DOVIPresent is the only key the app actually gates on, so it is
        the only one whose absence must mean "no Dolby Vision" rather than "the server didn't
        say"."""
        for sd in stream.get("side_data_list") or []:
            if sd.get("side_data_type") != "DOVI configuration record":
                continue
            major = int(sd.get("dv_version_major", 0))
            minor = int(sd.get("dv_version_minor", 0))
            return {
                "DOVIPresent": True,
                "DOVIProfile": int(sd.get("dv_profile", 0)),
                "DOVIBLCompatID": int(sd.get("dv_bl_signal_compatibility_id", 0)),
                "DOVIELPresent": bool(sd.get("el_present_flag", 0)),
                "DOVILevel": int(sd.get("dv_level", 0)),
                "DOVIVersion": f"{major}.{minor}",
                "DOVIBLPresent": bool(sd.get("bl_present_flag", 0)),
                "DOVIRPUPresent": bool(sd.get("rpu_present_flag", 0)),
            }
        return {}

    def _stream_wire(self, stream, sid):
        kind = {"video": 1, "audio": 2, "subtitle": 3}.get(stream.get("codec_type"))
        if kind is None:
            return None
        tags = {k.lower(): str(v) for k, v in (stream.get("tags") or {}).items()}
        disp = stream.get("disposition") or {}
        lang = tags.get("language", "")
        codec = stream.get("codec_name", "")
        out = {
            "id": sid, "streamType": kind, "codec": codec,
            "index": int(stream.get("index", 0)),
            "default": bool(disp.get("default", 0)),
            "selected": bool(disp.get("default", 0)),
        }
        if lang:
            out.update(language=lang, languageCode=lang)
        if tags.get("title"):
            out["title"] = tags["title"]
        if kind == 1:
            width, height = int(stream.get("width", 0)), int(stream.get("height", 0))
            _, label = Library._resolution_label(width, height)
            pix_fmt = stream.get("pix_fmt", "")
            bit_depth = 12 if "p12" in pix_fmt else 10 if "p10" in pix_fmt else 8
            out.update(width=width, height=height,
                       frameRate=float(stream.get("avg_frame_rate", "0/1").split("/")[0] or 0) /
                       max(1.0, float(stream.get("avg_frame_rate", "0/1").split("/")[-1] or 1)),
                       displayTitle=f"{label} ({codec.upper()})", bitDepth=bit_depth)
            color_trc = stream.get("color_transfer", "")
            if color_trc:
                out["colorTrc"] = color_trc
            out.update(Library._dovi_wire(stream))
        elif kind == 2:
            out.update(channels=int(stream.get("channels", 0)),
                       audioChannelLayout=stream.get("channel_layout", ""),
                       displayTitle=f"{lang or 'und'} ({codec.upper()})")
            self._maybe_loudness(out)
        else:
            out["displayTitle"] = f"{lang or 'und'} ({codec.upper()})"
            out["forced"] = bool(disp.get("forced", 0))
        return out

    def _verified_media(self, path, rk, part_id):
        info = self._probe(path)
        streams = []
        for n, stream in enumerate(info.get("streams", []), start=1):
            wire = self._stream_wire(stream, part_id * 10 + n)
            if wire is not None:
                streams.append(wire)
        video = next((s for s in streams if s["streamType"] == 1), None)
        audio = next((s for s in streams if s["streamType"] == 2), None)
        if video is None or audio is None:
            raise ValueError(f"verification media needs video and audio streams: {path}")
        duration = int(round(float(info.get("format", {}).get("duration", 0)) * 1000))
        if duration <= 0:
            raise ValueError(f"verification media has no positive duration: {path}")
        size = path.stat().st_size
        bitrate = int(round(size * 8 / max(1, duration)))
        container = path.suffix.lstrip(".").lower()
        content_type = self._CONTAINER_CONTENT_TYPE.get(container)
        if content_type is None:
            raise ValueError(f"unsupported container {container!r} for verification media: {path}")
        video_resolution, _ = self._resolution_label(video.get("width", 0), video.get("height", 0))
        part = {
            "id": part_id, "key": f"/library/parts/{part_id}/1/file.{container}",
            "duration": duration, "file": f"/synthetic/{path.name}", "size": size,
            "container": container, "Stream": streams,
        }
        media = {
            "id": rk, "duration": duration, "bitrate": bitrate,
            "width": video.get("width", 0), "height": video.get("height", 0),
            "aspectRatio": round(video.get("width", 0) / max(1, video.get("height", 1)), 3),
            "audioChannels": audio.get("channels", 0), "audioCodec": audio["codec"],
            "videoCodec": video["codec"], "videoResolution": video_resolution,
            "container": container, "videoFrameRate": "24p", "Part": [part],
        }
        self.media_files[part_id] = path
        self.media_content_type[part_id] = content_type
        self.verification_streams[part_id] = streams
        return media, duration

    def _add_verification_media(self, media_dir):
        """Add the two opt-in items without perturbing the seeded library's random sequence."""
        v1_media, v1_duration = self._verified_media(
            media_dir / "mockverify-v1.mkv", V1_RATING_KEY, VERIFY_PARTS[V1_RATING_KEY])
        v2_media, v2_duration = self._verified_media(
            media_dir / "mockverify-v2.mkv", V2_RATING_KEY, VERIFY_PARTS[V2_RATING_KEY])
        sidecar = media_dir / "mockverify-v2.eng.srt"
        side_info = self._probe(sidecar)
        side_stream = next((s for s in side_info.get("streams", [])
                            if s.get("codec_type") == "subtitle"), None)
        if side_stream is None:
            raise ValueError(f"verification sidecar is not a subtitle: {sidecar}")
        side_wire = self._stream_wire(side_stream, VERIFY_SIDECAR_ID)
        side_wire.update(index=max(s["index"] for s in v2_media["Part"][0]["Stream"]) + 1,
                         language="eng", languageCode="eng", external=True, selected=True,
                         default=False, key=f"/library/streams/{VERIFY_SIDECAR_ID}",
                         displayTitle="eng (external SRT)")
        v2_media["Part"][0]["Stream"].append(side_wire)
        self.sidecars[VERIFY_SIDECAR_ID] = sidecar

        show = self._base(random.Random(149), VERIFY_SHOW_RK, "show", self.sections[1])
        show.update(contentRating="TV-14", duration=v1_duration, childCount=1, leafCount=1,
                    viewedLeafCount=0, Genre=[], Role=[])
        season = self._base(random.Random(150), VERIFY_SEASON_RK, "season", self.sections[1])
        season.update(index=1, parentRatingKey=str(VERIFY_SHOW_RK), parentKey=show["key"],
                      parentTitle=show["title"], parentThumb=show["thumb"], childCount=1,
                      leafCount=1, viewedLeafCount=0)
        episode = self._base(random.Random(151), V1_RATING_KEY, "episode", self.sections[1])
        episode.update(index=1, parentIndex=1, parentRatingKey=str(VERIFY_SEASON_RK),
                       parentKey=season["key"], parentTitle=season["title"],
                       grandparentRatingKey=str(VERIFY_SHOW_RK), grandparentKey=show["key"],
                       grandparentTitle=show["title"], grandparentThumb=show["thumb"],
                       grandparentArt=show["art"], duration=v1_duration, contentRating="TV-14",
                       Media=[v1_media], Director=[], Writer=[], Role=[], Chapter=[], Marker=[])
        movie = self._base(random.Random(116), V2_RATING_KEY, "movie", self.sections[0])
        movie.update(duration=v2_duration, contentRating="PG", studio="s00000116",
                     tagline="s00000116", Media=[v2_media], Genre=[], Director=[], Writer=[],
                     Role=[], Country=[], Chapter=[], Marker=[], Rating=[])
        self.items.update({VERIFY_SHOW_RK: show, VERIFY_SEASON_RK: season,
                           V1_RATING_KEY: episode, V2_RATING_KEY: movie})

    def _add_extra_media(self, paths):
        """--extra-media: one movie per arbitrary file, at a fixed id block (EXTRA_MEDIA_RK_BASE+)
        that never overlaps the generated library or the --media verification ids. Each file's
        DOVI*/colorTrc/bitDepth/container come from `_verified_media` exactly like a --media item
        — this is the same probe, not a parallel one."""
        self.extra_media_files = {}
        for n, path in enumerate(paths):
            if not path.is_file():
                raise ValueError(f"--extra-media file is missing: {path}")
            rk = EXTRA_MEDIA_RK_BASE + n
            part_id = EXTRA_MEDIA_PART_ID_BASE + n
            media, duration = self._verified_media(path, rk, part_id)
            # Native subtitle verification can exercise the same script as an embedded stream
            # and as a sidecar. Only explicit sibling files of --extra-media are exposed.
            streams = media["Part"][0]["Stream"]
            for extension in ("ass", "ssa", "srt", "vtt"):
                sidecar = path.with_suffix("." + extension)
                if not sidecar.is_file():
                    continue
                sid = part_id * 100 + len(streams) + 1
                streams.append({"id": sid, "streamType": 3, "codec": extension,
                                "index": len(streams), "external": True, "selected": False,
                                "language": "eng", "languageCode": "eng",
                                "key": f"/library/streams/{sid}",
                                "displayTitle": f"eng (external {extension.upper()})"})
                self.sidecars[sid] = sidecar
            rng = random.Random(rk)
            movie = self._base(rng, rk, "movie", self.sections[0])
            movie.update(duration=duration, contentRating="NR", studio=sname(rng),
                         tagline=sname(rng), Media=[media], Genre=[], Director=[], Writer=[],
                         Role=[], Country=[], Chapter=[], Marker=[], Rating=[])
            self.items[rk] = movie
            self.extra_media_files[rk] = path

    # --- item builders -------------------------------------------------------------------

    def _blur(self, rng):
        return {k: "#%06x" % rng.getrandbits(24)
                for k in ("topLeft", "topRight", "bottomRight", "bottomLeft")}

    def _tags(self, rng, pool, n, role=False):
        picks = rng.sample(sorted(pool), min(n, len(pool)))
        out = []
        for i in picks:
            t = dict(pool[i])
            if role:
                t["role"] = sname(rng)
            out.append(t)
        return out

    def _maybe_loudness(self, audio):
        """PMS string-encodes this boolean on the wire (#266)."""
        if self.loudness_analysis:
            audio["canNormalizeLoudness"] = "1"
        return audio

    def _media(self, rng, rk, part, duration, *, acodec="ac3", channels=6,
               audio_display="English (AC3 5.1)", video_extra=None, sub=None):
        """A movie's Media block. The generated library calls this with no keyword arguments,
        which must keep producing exactly the original literal shape (`_media`'s pre-#266 output,
        byte for byte). `_add_enhancement_fixtures` calls it with `acodec`/`channels` set and an
        explicit `audio_display` for the five fixed #266 fixture movies (`ENHANCEMENT_*_RK`),
        folded in here rather than kept as a separate `_enhancement_movie_media` because the two
        builders differed only in these knobs. `video_extra`: an optional dict merged onto the
        video stream (e.g. the DV_P8 fixture's Dolby Vision fields) — note the top-level
        `videoCodec` stays the literal `"h264"` even when `video_extra` overrides the stream's own
        `codec`, matching both builders' pre-fold behaviour. `sub`: `None` (an ordinary embedded
        SRT, not selected/default), `"embedded_default"` (default+selected embedded SRT), or
        `"external_selected"` (a server-selected external SRT sidecar stream, `key` set, never
        actually fetchable — no sidecar file backs it, matching a movie fixture this module never
        claims to be a probed file)."""
        video = {"id": part * 10 + 1, "streamType": 1, "codec": "h264", "index": 0,
                 "width": 1920, "height": 1080, "displayTitle": "1080p (H.264)"}
        if video_extra:
            video.update(video_extra)
        audio = {"id": part * 10 + 2, "streamType": 2, "codec": acodec, "index": 1,
                 "channels": channels, "language": "en", "languageCode": "eng",
                 "displayTitle": audio_display, "selected": True}
        self._maybe_loudness(audio)
        streams = [video, audio]
        embedded_sub = {"id": part * 10 + 3, "streamType": 3, "codec": "srt", "index": 2,
                        "language": "en", "languageCode": "eng", "displayTitle": "English (SRT)"}
        if sub == "embedded_default":
            embedded_sub.update(default=True, selected=True)
        streams.append(embedded_sub)
        if sub == "external_selected":
            sid = part * 100 + len(streams) + 1
            streams.append({
                "id": sid, "streamType": 3, "codec": "srt", "index": len(streams),
                "external": True, "selected": True, "default": False,
                "language": "eng", "languageCode": "eng", "key": f"/library/streams/{sid}",
                "displayTitle": "English (external SRT)",
            })
        return [{
            "id": rk, "duration": duration, "bitrate": 8000, "width": 1920, "height": 1080,
            "aspectRatio": 1.78, "audioChannels": channels, "audioCodec": acodec,
            "videoCodec": "h264", "videoResolution": "1080", "container": "mkv",
            "videoFrameRate": "24p", "videoProfile": "high",
            "Part": [{
                "id": part, "key": f"/library/parts/{part}/{1_700_000_000 + part}/file.mkv",
                "duration": duration, "file": f"/{sname(rng)}/{sname(rng)}.mkv",
                "size": 4_000_000_000, "container": "mkv", "videoProfile": "high",
                "Stream": streams,
            }],
        }]

    def _add_enhancement_fixtures(self):
        """The five fixed #266 fixture movies named in the module doc, on their own
        `librarySectionID` (`ENHANCEMENT_SECTION_ID`) that is never added to `self.sections` —
        so they are invisible to every section/rail listing, count and firstCharacter query, and
        the existing generated-data hashes and letter-count tests are unaffected — while staying
        reachable, always, directly by `/library/metadata/<ratingKey>` and `/decision`."""
        section = {"key": str(ENHANCEMENT_SECTION_ID), "title": "s00000003"}
        specs = [
            (ENHANCEMENT_AC3_2CH_RK, "ac3", 2, None, None),
            (ENHANCEMENT_AAC_51_RK, "aac", 6, None, None),
            (ENHANCEMENT_DV_P8_RK, "hevc", 6, {
                "codec": "hevc",
                "DOVIPresent": True, "DOVIProfile": 8, "DOVIBLCompatID": 1,
                "DOVIELPresent": False, "DOVILevel": 6, "DOVIVersion": "1.0",
                "DOVIBLPresent": True, "DOVIRPUPresent": True,
            }, None),
            (ENHANCEMENT_DEFAULT_SRT_RK, "ac3", 6, None, "embedded_default"),
            (ENHANCEMENT_EXTERNAL_SRT_RK, "ac3", 6, None, "external_selected"),
        ]
        for n, (rk, acodec, channels, video_extra, sub) in enumerate(specs, start=1):
            part = ENHANCEMENT_PART_ID_BASE + n
            rng = random.Random(rk)
            it = self._base(rng, rk, "movie", section)
            dur = 100 * 60_000
            it.update({
                "duration": dur, "contentRating": "PG-13", "studio": sname(rng),
                "tagline": swords(rng, 4), "originallyAvailableAt": f"{it['year']}-03-14",
                "rating": 7.0, "audienceRating": 7.0, "Genre": [], "Director": [], "Writer": [],
                "Role": [], "Country": [], "Chapter": [], "Marker": [], "Rating": [],
                "Media": self._media(
                    rng, rk, part, dur, acodec=acodec, channels=channels,
                    audio_display=f"English ({acodec.upper()} {channels}ch)",
                    video_extra=video_extra, sub=sub),
            })
            self.items[rk] = it

    def _base(self, rng, rk, kind, section):
        return {
            "ratingKey": str(rk), "key": f"/library/metadata/{rk}", "guid": f"plex://{kind}/{sname(rng)}",
            "type": kind, "title": sname(rng), "titleSort": sname(rng),
            "librarySectionTitle": section["title"], "librarySectionID": int(section["key"]),
            "librarySectionKey": f"/library/sections/{section['key']}",
            "summary": swords(rng, 24), "year": 1990 + rng.randrange(35),
            "thumb": f"/library/metadata/{rk}/thumb/1", "art": f"/library/metadata/{rk}/art/1",
            "addedAt": 1_690_000_000 + rk, "updatedAt": 1_690_000_000 + rk,
            "UltraBlurColors": self._blur(rng),
        }

    def _movie(self, rng, rk, part):
        it = self._base(rng, rk, "movie", self.sections[0])
        dur = (80 + rng.randrange(80)) * 60_000
        it.update({
            "duration": dur, "contentRating": "PG-13", "studio": sname(rng),
            "tagline": swords(rng, 4), "originallyAvailableAt": f"{it['year']}-03-14",
            "rating": 6.0 + rng.randrange(40) / 10.0, "audienceRating": 6.0 + rng.randrange(40) / 10.0,
            "ratingImage": "rottentomatoes://image.rating.ripe",
            "audienceRatingImage": "rottentomatoes://image.rating.upright",
            "Media": self._media(rng, rk, part, dur),
            "Genre": self._tags(rng, self.genres, 2),
            "Director": self._tags(rng, self.people, 1),
            "Writer": self._tags(rng, self.people, 1),
            "Role": self._tags(rng, self.people, 5, role=True),
            "Country": [{"tag": sname(rng)}],
            "Chapter": [{"index": i + 1, "startTimeOffset": i * dur // 6,
                         "endTimeOffset": (i + 1) * dur // 6, "tag": sname(rng)} for i in range(6)],
            "Marker": [{"type": "credits", "startTimeOffset": dur - 120_000, "endTimeOffset": dur,
                        "final": True}],
            "Rating": [{"image": "imdb://image.rating", "value": 7.1, "type": "audience"}],
        })
        if rng.randrange(3) == 0:
            c = self.collections[1 + rng.randrange(len(self.collections))]
            it["Collection"] = [{"tag": c["tag"], "id": c["id"]}]
        return it

    def _show(self, rng, rk):
        it = self._base(rng, rk, "show", self.sections[1])
        it.update({
            "contentRating": "TV-14", "studio": sname(rng), "duration": 45 * 60_000,
            "originallyAvailableAt": f"{it['year']}-09-21", "childCount": 0, "leafCount": 0,
            "viewedLeafCount": 0,
            "Genre": self._tags(rng, self.genres, 2),
            "Role": self._tags(rng, self.people, 6, role=True),
        })
        return it

    def _season(self, rng, rk, show, n):
        it = self._base(rng, rk, "season", self.sections[1])
        it.update({
            "title": sname(rng), "index": n, "parentRatingKey": show["ratingKey"],
            "parentKey": show["key"], "parentTitle": show["title"], "parentThumb": show["thumb"],
            "leafCount": 0, "viewedLeafCount": 0,
        })
        return it

    def _episode(self, rng, rk, season, show, n, part):
        it = self._base(rng, rk, "episode", self.sections[1])
        dur = (40 + rng.randrange(20)) * 60_000
        it.update({
            "index": n, "parentIndex": season["index"], "parentRatingKey": season["ratingKey"],
            "parentKey": season["key"], "parentTitle": season["title"],
            "grandparentRatingKey": show["ratingKey"], "grandparentKey": show["key"],
            "grandparentTitle": show["title"], "grandparentThumb": show["thumb"],
            "grandparentArt": show["art"], "duration": dur, "contentRating": "TV-14",
            "originallyAvailableAt": f"{show['year']}-{season['index']:02d}-{n:02d}",
            "Media": self._media(rng, rk, part, dur),
            "Director": self._tags(rng, self.people, 1),
            "Writer": self._tags(rng, self.people, 1),
            "Role": self._tags(rng, self.people, 3, role=True),
            "Chapter": [], "Marker": [
                {"type": "intro", "startTimeOffset": 30_000, "endTimeOffset": 90_000},
                {"type": "credits", "startTimeOffset": dur - 60_000, "endTimeOffset": dur,
                 "final": True}],
        })
        return it

    # --- derived state ---------------------------------------------------------------------

    def _roll_up(self):
        """leafCount/viewedLeafCount/childCount on seasons and shows, from the episodes."""
        for it in self.items.values():
            if it["type"] in ("show", "season"):
                it["leafCount"] = 0
                it["viewedLeafCount"] = 0
                it["childCount"] = 0
        for it in self.items.values():
            if it["type"] != "episode":
                continue
            for pk in (it["parentRatingKey"], it["grandparentRatingKey"]):
                p = self.items[int(pk)]
                p["leafCount"] += 1
                p["viewedLeafCount"] += 1 if it.get("viewCount", 0) > 0 else 0
        for it in self.items.values():
            if it["type"] == "season":
                self.items[int(it["parentRatingKey"])]["childCount"] += 1
            if it["type"] == "show":
                pass

    # --- queries ---------------------------------------------------------------------------

    def section_items(self, key, q):
        kind = {"1": "movie", "2": "show"}.get(key)
        t = q.get("type")
        if t:
            kind = {"1": "movie", "2": "show", "3": "season", "4": "episode"}.get(t, kind)
        rows = [it for it in self.items.values() if it["type"] == kind
                and it["librarySectionID"] == int(key)]
        if q.get("unwatched") == "1":
            rows = [it for it in rows if not self._watched(it)]
        g = q.get("genre")
        if g:
            rows = [it for it in rows if any(str(t["id"]) == g for t in it.get("Genre", []))]
        a = q.get("actor")
        if a:
            rows = [it for it in rows if any(str(t["id"]) == a for t in
                    it.get("Role", []) + it.get("Director", []) + it.get("Writer", []))]
        c = q.get("collection")
        if c:
            rows = [it for it in rows if any(str(t["id"]) == c for t in it.get("Collection", []))]
        sort = q.get("sort", "titleSort")
        field, _, direction = sort.partition(":")
        keyf = {
            "titleSort": lambda it: it["titleSort"],
            "addedAt": lambda it: it["addedAt"],
            "originallyAvailableAt": lambda it: it.get("originallyAvailableAt", ""),
            "year": lambda it: it["year"],
            "rating": lambda it: it.get("rating", 0.0),
            "lastViewedAt": lambda it: it.get("lastViewedAt", 0),
            "random": lambda it: hashlib.md5(it["ratingKey"].encode()).hexdigest(),
        }.get(field, lambda it: it["titleSort"])
        rows.sort(key=keyf, reverse=(direction == "desc"))
        return rows

    def first_characters(self, key, q=None):
        """Counts in the unfiltered ascending titleSort order, exactly the rail's query —
        of the section's collections for `type=18`, as PMS answers `firstCharacter?type=18`."""
        counts = {}
        rows = (sorted(self.collection_metadata(key), key=lambda c: c["titleSort"].lower())
                if (q or {}).get("type") == "18" else self.section_items(key, {}))
        for item in rows:
            letter = item["titleSort"][0].upper()
            counts[letter] = counts.get(letter, 0) + 1
        return [{"key": letter.lower(), "title": letter, "size": count}
                for letter, count in counts.items()]

    def _watched(self, it):
        if it["type"] in ("movie", "episode"):
            return it.get("viewCount", 0) > 0
        return it.get("leafCount", 0) > 0 and it.get("viewedLeafCount", 0) >= it["leafCount"]

    def children(self, rk):
        it = self.items.get(rk)
        if not it:
            return []
        if it["type"] == "show":
            return sorted((x for x in self.items.values() if x["type"] == "season"
                           and x["parentRatingKey"] == it["ratingKey"]), key=lambda x: x["index"])
        if it["type"] == "season":
            return sorted((x for x in self.items.values() if x["type"] == "episode"
                           and x["parentRatingKey"] == it["ratingKey"]), key=lambda x: x["index"])
        return []

    def leaves(self, rk):
        it = self.items.get(rk)
        if not it or it["type"] != "show":
            return []
        return sorted((x for x in self.items.values() if x["type"] == "episode"
                       and x["grandparentRatingKey"] == it["ratingKey"]),
                      key=lambda x: (x["parentIndex"], x["index"]))

    def continue_watching(self):
        rows = [it for it in self.items.values() if it.get("viewOffset", 0) > 0]
        rows.sort(key=lambda it: -it.get("lastViewedAt", 0))
        return rows

    def recent(self, kind, n=12):
        rows = [it for it in self.items.values() if it["type"] == kind]
        rows.sort(key=lambda it: -it["addedAt"])
        return rows[:n]

    def person_media(self, pid):
        return [it for it in self.items.values() if it["type"] in ("movie", "show") and any(
            t["id"] == pid for t in it.get("Role", []) + it.get("Director", []) + it.get("Writer", []))]

    def composite(self, rk, width, height):
        """The server's automatic collection poster (`/library/collections/<rk>/composite/<stamp>`):
        a 2x2 of its first members' posters, as JPEG; None (a 404) for a collection with none."""
        c = self.collection_by_rating_key(int(rk)) if str(rk).isdigit() else None
        paths = [self.images.get(int(it["ratingKey"]), {}).get("thumb")
                 for it in (self.collection_rows(c["id"]) if c else [])]
        paths = [p for p in paths if p is not None][:4]
        if not paths:
            return None
        paths = (paths * 4)[:4]
        try:
            w, h = max(2, min(int(width), 3840)), max(2, min(int(height), 2160))
        except (TypeError, ValueError):
            w, h = 400, 600
        key = ("composite", str(rk), w, h)
        cw, ch = w // 2, h // 2
        scale = "".join(f"[{i}]scale={cw}:{ch}:force_original_aspect_ratio=increase,crop={cw}:{ch}[t{i}];"
                        for i in range(4))
        with self._lock:
            if key not in self._scaled:
                self._scaled[key] = subprocess.check_output([
                    "ffmpeg", "-v", "error", "-threads", "1",
                    *[a for p in paths for a in ("-i", str(p))],
                    "-filter_complex", scale + "[t0][t1][t2][t3]xstack=inputs=4:layout=0_0|w0_0|0_h0|w0_h0",
                    "-frames:v", "1", "-q:v", "3", "-bitexact", "-f", "image2pipe", "-vcodec", "mjpeg", "-"])
            return "image/jpeg", self._scaled[key]

    def _add_empty_collection(self, tag):
        """A collection with no members: the case a client's neutral collection tile draws."""
        cid = len(self.collections) + 1
        self.collections[cid] = {"id": cid, "ratingKey": 50000 + cid, "tag": tag}

    def collection_rows(self, cid):
        rows = [it for it in self.items.values()
                if any(c["id"] == cid for c in it.get("Collection", []))]
        rows.sort(key=lambda it: -it["addedAt"])
        return rows

    def collection_metadata(self, section):
        if int(section) != 1:
            return []
        rows = []
        for c in self.collections.values():
            members = [it for it in self.collection_rows(c["id"])
                       if it["librarySectionID"] == int(section)]
            updated = max((it.get("updatedAt", 0) for it in members), default=1_700_000_000)
            rk = c["ratingKey"]
            rows.append({
                "ratingKey": str(rk), "key": f"/library/collections/{rk}/children",
                "type": "collection", "title": c["tag"], "titleSort": c["tag"],
                "index": c["id"], "childCount": len(members),
                "updatedAt": updated, "librarySectionID": int(section), "smart": 0,
            })
            # A collection with members has the server's automatic composite; the EMPTY one has
            # no artwork at all, which is the case a client's neutral collection tile draws.
            if members:
                rows[-1]["thumb"] = f"/library/collections/{rk}/composite/{updated}"
        return rows

    def collection_by_rating_key(self, rk):
        return next((c for c in self.collections.values() if c["ratingKey"] == rk), None)

    def search(self, query, limit=3, include_collections=False):
        """`/hubs/search` as hubs. Each hub holds at most `limit` rows and its `size` is the number
        it holds, as measured (docs/pms-api.md: "`limit` caps each hub separately", 3 when absent,
        and `Hub.size` is the rows returned).

        The movie hub also carries RELATED results, which the spec documents for this endpoint:
        "for a genre match, it may return movies in that genre, or for an actor match, movies with
        that actor", each marked with `reason` (the hub the match came from), `reasonTitle` and
        `reasonID`. The mock returns exactly those two relations — a genre whose name the query
        matches brings that genre's movies, and a person it matches brings the movies they act in —
        after the direct title hits, in library order. The order and the choice of which relations
        a real server applies are assumptions: the spec names these two examples and says the hubs
        are ordered "based on quality", which the mock does not try to model. Shows, episodes and
        the other hubs hold direct hits only.

        `include_collections` is `includeCollections=1`, measured live (docs/pms-api.md §2b): the
        collection hub's rows move from tag-shaped `Directory[]` rows (tag `id`, `count`, `key`; no
        ratingKey, no thumb) to the collections' full `Metadata[]` rows — the same rows
        `/library/sections/{s}/collections` lists, plus a `score`."""
        hits = search_matcher(query)
        limit = max(1, int(limit))
        genres = [g for g in self.genres.values() if hits(g["tag"])]
        matched = [t for t in self.people.values() if hits(t["tag"])]

        def related(it):
            for g in genres:
                if any(t["id"] == g["id"] for t in it.get("Genre", [])):
                    return {"reason": "genre", "reasonTitle": g["tag"], "reasonID": g["id"]}
            for p in matched:
                if any(t["id"] == p["id"] for t in it.get("Role", [])):
                    return {"reason": "actor", "reasonTitle": p["tag"], "reasonID": p["id"]}
            return None

        hubs = []
        for kind in ("movie", "show", "episode"):
            items = [it for it in self.items.values() if it["type"] == kind]
            rows = [it for it in items if hits(it["title"])]
            if kind == "movie":
                rows += [dict(it, **why) for it in items
                         if not hits(it["title"]) and (why := related(it))]
            rows = rows[:limit]
            hubs.append({"title": kind, "type": kind, "hubIdentifier": kind, "size": len(rows),
                         "Metadata": rows})
        people = [dict(t, type="actor", key=f"/library/sections/1/all?actor={t['id']}",
                       librarySectionID=1) for t in matched][:limit]
        hubs.append({"title": "actor", "type": "actor", "hubIdentifier": "actor",
                     "size": len(people), "Directory": people})
        if include_collections:
            cols = [dict(row, score="0.90000") for row in self.collection_metadata(1)
                    if hits(row["title"])][:limit]
            container = "Metadata"
        else:
            cols = [{"tag": c["tag"], "id": c["id"], "type": "collection", "librarySectionID": 1,
                     "key": f"/library/sections/1/all?collection={c['id']}", "reasonTitle": ""}
                    for c in self.collections.values() if hits(c["tag"])][:limit]
            container = "Directory"
        hubs.append({"title": "collection", "type": "collection", "hubIdentifier": "collection",
                     "size": len(cols), container: cols})
        return hubs


# ---------------------------------------------------------------- the demo catalog --------

DEMO_PIN_ID = 1790000001
DEMO_PIN_CODE = "DEMO"
# The account token `--authorize-after` links the demo code with: synthetic, in the harness's own
# token alphabet (`ALPHABET_TOKEN`), and accepted — never checked — by every endpoint here.
DEMO_ACCOUNT_TOKEN = "s" + hashlib.sha1(b"mock-pms-demo-account").hexdigest()[:8]
# What the demo sign-in QR encodes: the page a person types a code into. A scan of a screenshot
# lands on plex.tv's own link page, which asks for a code this server invented — harmless.
DEMO_QR_TEXT = b"https://plex.tv/link"


def demo_cache_dir():
    env = os.environ.get("PLXNATIVE_DEMO_CACHE")
    return pathlib.Path(env) if env else pathlib.Path.home() / ".cache" / "plxnative-demo"


class CatalogLibrary(Library):
    """The demo library (`--catalog`): the same wire shapes and the same queries as the generated
    library, built from `tests/demo_library/catalog.json` instead of a seed. Keys are dense and
    fixed by catalog ORDER — movies 101.., shows 201.., a season `show*10+n`, an episode
    `season*10+n` — so a scene manifest can name an item by key and the key never moves unless
    the catalog does."""

    def __init__(self, catalog_path, cache=None, hero=None, loudness_analysis=True):
        self.loudness_analysis = loudness_analysis
        catalog_path = pathlib.Path(catalog_path)
        cat = json.loads(catalog_path.read_text())
        assets = json.loads((catalog_path.parent / "assets.json").read_text())["assets"]
        self.cache = pathlib.Path(cache) if cache else demo_cache_dir()
        self.derived = self.cache / "derived"
        if not self.derived.is_dir():
            raise ValueError(f"no derived demo artwork in {self.derived}: "
                             "run `python3 tools/demo_library.py derive` first")
        self.catalog = cat
        self.seed = 0
        self.now = int(cat["now"])
        self.machine = cat["server"]["machineIdentifier"]
        self.friendly = cat["server"]["friendlyName"]
        self.items, self.people, self.genres, self.collections = {}, {}, {}, {}
        self.sections = [dict(s, uuid=f"demo-section-{s['key']}") for s in cat["sections"]]
        self.media_files, self.sidecars, self.verification_streams, self.media_content_type = {}, {}, {}, {}
        self.images = {}  # rk -> {"thumb"|"art": derived file}
        self.by_slug = {}  # "slug" / "slug/season/episode" -> rk
        self._scaled = {}
        self._lock = threading.Lock()
        for n, name in enumerate(cat["collections"], start=1):
            self.collections[n] = {"id": n, "ratingKey": 50000 + n, "tag": name}
        coll_id = {c["tag"]: c["id"] for c in self.collections.values()}

        def person(name):
            for p in self.people.values():
                if p["tag"] == name:
                    return p
            pid = len(self.people) + 1
            self.people[pid] = {"id": pid, "tag": name, "tagKey": f"demo-person-{pid}"}
            return self.people[pid]

        def genre(name):
            for g in self.genres.values():
                if g["tag"] == name:
                    return g
            gid = len(self.genres) + 1
            self.genres[gid] = {"id": gid, "tag": name, "filter": f"genre={gid}"}
            return self.genres[gid]

        def credits(rec):
            return {
                "Genre": [dict(genre(g)) for g in rec.get("genres", [])],
                "Director": [dict(person(n)) for n in rec.get("directors", [])],
                "Writer": [dict(person(n)) for n in rec.get("writers", [])],
                "Role": [dict(person(c["name"]), role=c["role"]) for c in rec.get("cast", [])],
            }

        def base(rk, kind, section, rec, slug):
            key = slug.replace("/", "_")
            it = {
                "ratingKey": str(rk), "key": f"/library/metadata/{rk}", "guid": f"plex://{kind}/demo-{key}",
                "type": kind, "title": rec["title"], "titleSort": rec.get("titleSort", rec["title"]),
                "librarySectionTitle": section["title"], "librarySectionID": int(section["key"]),
                "librarySectionKey": f"/library/sections/{section['key']}",
                "summary": rec.get("summary", ""),
            }
            if "year" in rec:
                it["year"] = rec["year"]
            images = {}
            for role, name in (("thumb", "poster"), ("art", "art"), ("thumb", "thumb")):
                path = self.derived / key / f"{name}.jpg"
                if name in rec and path.is_file():
                    images[role] = path
                    it[role] = f"/library/metadata/{rk}/{role}/{self.now}"
                elif name in rec:
                    raise ValueError(f"{slug}: derived {name} is missing ({path}); rerun "
                                     "`python3 tools/demo_library.py derive`")
            if "logo" in rec:
                # The item's clearLogo, which the app asks for by path
                # (`/library/metadata/<rk>/clearLogo`), as it does of a real server.
                path = self.derived / key / "logo.png"
                if not path.is_file():
                    raise ValueError(f"{slug}: derived logo is missing ({path}); rerun "
                                     "`python3 tools/demo_library.py derive`")
                images["clearLogo"] = path
            if "art" in images:
                it["UltraBlurColors"] = self._blur_of(images["art"])
            self.images[rk] = images
            self.by_slug[slug] = rk
            return it

        movies, shows = self.sections[0], self.sections[1]
        part = 100
        for n, m in enumerate(cat["movies"]):
            rk = 101 + n
            part += 1
            it = base(rk, "movie", movies, m, m["id"])
            dur = m["minutes"] * 60_000
            it.update(studio=m.get("studio", ""), originallyAvailableAt=m["originallyAvailableAt"],
                      duration=dur, Chapter=[], Marker=[], **credits(m))
            if m.get("collections"):
                it["Collection"] = [{"tag": c, "id": coll_id[c]} for c in m["collections"]]
            if "media" in m:
                a = assets[m["media"]]
                path = self.cache / "src" / f"{m['media']}.{a['url'].rsplit('.', 1)[-1].lower()}"
                media, dur = self._verified_media(path, rk, part)
                it.update(duration=dur, Media=[media])
            else:
                it["Media"] = self._demo_media(rk, part, dur, f"/media/Movies/{m['title']} ({m['year']})")
            it["Chapter"] = self._chapters(m.get("chapters", []), dur, m["id"])
            self.items[rk] = it
        for n, s in enumerate(cat["shows"]):
            rk = 201 + n
            show = base(rk, "show", shows, s, s["id"])
            show.update(studio=s.get("studio", ""), originallyAvailableAt=f"{s['year']}-01-01",
                        duration=0, childCount=0, leafCount=0, viewedLeafCount=0, **credits(s))
            self.items[rk] = show
            for season in s["seasons"]:
                srk = rk * 10 + season["index"]
                se = base(srk, "season", shows, {"title": f"Season {season['index']}"}, f"{s['id']}/{season['index']}")
                se.update(index=season["index"], parentRatingKey=show["ratingKey"], parentKey=show["key"],
                          parentTitle=show["title"], parentThumb=show.get("thumb", ""), thumb=show.get("thumb", ""),
                          art=show.get("art", ""), leafCount=0, viewedLeafCount=0)
                self.images[srk] = self.images[rk]
                self.items[srk] = se
                for e in season["episodes"]:
                    erk = srk * 1000 + e["index"]  # episode numbers run past 9 (Hubblecast 133)
                    part += 1
                    ep = base(erk, "episode", shows, e, f"{s['id']}/{season['index']}/{e['index']}")
                    dur = e["minutes"] * 60_000
                    ep.update(index=e["index"], parentIndex=season["index"], parentRatingKey=se["ratingKey"],
                              parentKey=se["key"], parentTitle=se["title"], grandparentRatingKey=show["ratingKey"],
                              grandparentKey=show["key"], grandparentTitle=show["title"],
                              grandparentThumb=show.get("thumb", ""), grandparentArt=show.get("art", ""),
                              year=int(e["date"][:4]), originallyAvailableAt=e["date"], duration=dur,
                              Media=self._demo_media(erk, part, dur, f"/media/TV/{s['title']}/S{season['index']:02d}E{e['index']:02d}"),
                              Director=[], Writer=[], Role=[], Chapter=[], Marker=[])
                    if e.get("stand_in"):
                        slug = f"{s['id']}/{season['index']}/{e['index']}"
                        path = self.derived / slug.replace("/", "_") / "stand-in.mp4"
                        if not path.is_file():
                            raise ValueError(f"{slug}: derived stand-in is missing ({path}); rerun "
                                             "`python3 tools/demo_library.py derive`")
                        media, dur = self._verified_media(path, erk, part)
                        ep.update(duration=dur, Media=[media])
                    if "art" in self.images[rk]:
                        self.images[erk]["art"] = self.images[rk]["art"]
                    self.items[erk] = ep
        self._add_empty_collection("Empty Collection")
        self._add_name_fitting_collections()
        self._pin_clock(hero or cat["hero"])
        self._roll_up()
        for it in self.items.values():
            if it["type"] == "show":
                it["duration"] = max((x["duration"] for x in self.items.values()
                                      if x["type"] == "episode" and x["grandparentRatingKey"] == it["ratingKey"]),
                                     default=0)

    def continuous_queue(self, it):
        """The rows of a `continuous=1` PlayQueue started on `it`: an episode and every episode of
        its show after it, in (season, episode) order, as PMS answers; anything else, itself."""
        if it["type"] != "episode":
            return [it]
        show = [x for x in self.items.values()
                if x["type"] == "episode" and x["grandparentRatingKey"] == it["grandparentRatingKey"]]
        show.sort(key=lambda x: (x["parentIndex"], x["index"]))
        return show[show.index(it):]

    def _pin_clock(self, hero):
        """addedAt, lastViewedAt, viewOffset and viewCount from the catalog and its pinned `now` —
        never from the wall clock. `hero` goes to the head of Continue Watching: the home hero pool
        opens with the deck, so that is its first slot."""
        cat = self.catalog
        if hero not in self.by_slug:
            raise ValueError(f"--hero {hero!r} is not in the catalog")
        for i, slug in enumerate(cat["added_order"]):
            it = self.items[self.by_slug[slug]]
            added = self.now - 86_400 * (2 + 3 * i)
            it["addedAt"] = it["updatedAt"] = added
            if it["type"] == "show":
                for x in self.items.values():
                    if x.get("grandparentRatingKey") == it["ratingKey"] or x.get("parentRatingKey") == it["ratingKey"]:
                        x["addedAt"] = x["updatedAt"] = added + x.get("index", 0) * 60
        deck = [dict(c) for c in cat["continue_watching"]]
        if all(c["item"] != hero for c in deck):
            deck.insert(0, {"item": hero, "progress": 0.42})
        deck.sort(key=lambda c: c["item"] != hero)  # stable: the hero first, the rest in order
        for i, c in enumerate(deck):
            it = self.items[self.by_slug[c["item"]]]
            it["viewOffset"] = int(it["duration"] * c["progress"]) // 1000 * 1000
            it["lastViewedAt"] = self.now - 600 - 3_600 * i
        for i, slug in enumerate(cat["watched"]):
            it = self.items[self.by_slug[slug]]
            it["viewCount"] = 1
            it["lastViewedAt"] = self.now - 86_400 * (10 + i)

    @staticmethod
    def _chapters(marks, duration, slug):
        """`[{"start": "m:ss", "title": …}, …]` from the catalog → PMS's `Chapter[]`: each chapter
        ends where the next begins, the last at the end of the file. The first must start at 0:00
        and the starts must rise, as they do on a real file."""
        def ms(stamp):
            m, s = stamp.split(":")
            return (int(m) * 60 + int(s)) * 1000
        starts = [ms(c["start"]) for c in marks]
        if marks and (starts[0] != 0 or starts != sorted(set(starts)) or starts[-1] >= duration):
            raise ValueError(f"{slug}: chapter starts must begin at 0:00, rise, and end inside the film")
        ends = starts[1:] + [duration]
        return [{"id": n, "index": n, "tag": c["title"], "startTimeOffset": a, "endTimeOffset": b}
                for n, (c, a, b) in enumerate(zip(marks, starts, ends), start=1)]

    @staticmethod
    def _blur_of(path):
        """The four UltraBlur corner colours PMS would compute, taken from the backdrop itself: a
        2×2 area-average of the image, darkened the way PMS's own colours are."""
        raw = subprocess.check_output(["ffmpeg", "-v", "error", "-i", str(path), "-vf",
                                       "scale=2:2:flags=area", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        px = [tuple(int(c * 0.55) for c in raw[i:i + 3]) for i in range(0, 12, 3)]
        tl, tr, bl, br = px
        return {k: "#%02x%02x%02x" % v for k, v in
                (("topLeft", tl), ("topRight", tr), ("bottomRight", br), ("bottomLeft", bl))}

    def _demo_media(self, rk, part, duration, stem):
        media = self._media(random.Random(rk), rk, part, duration)
        media[0]["Part"][0]["file"] = stem + ".mkv"
        return media

    def image(self, url, width, height):
        """`(content type, bytes)` for an artwork URL (`/library/metadata/<rk>/<thumb|art|clearLogo>
        [/<n>]`), scaled like PMS's photo transcoder with minSize=1 (cover the box, keep the aspect)
        — a clearLogo as a transparent PNG, the rest as JPEG; None when the item has no such image
        — a 404, as for a real item without art."""
        segs = [s for s in urllib.parse.urlsplit(url).path.split("/") if s]
        if len(segs) >= 4 and segs[:2] == ["library", "collections"] and segs[3] == "composite":
            return self.composite(segs[2], width, height)
        if len(segs) < 4 or segs[:2] != ["library", "metadata"] or not segs[2].isdigit():
            return None
        path = self.images.get(int(segs[2]), {}).get(segs[3])
        if path is None:
            return None
        png = path.suffix == ".png"
        ctype = "image/png" if png else "image/jpeg"
        try:
            w, h = max(1, min(int(width), 3840)), max(1, min(int(height), 2160))
        except (TypeError, ValueError):
            return ctype, path.read_bytes()
        key = (str(path), w, h)
        encode = (["-pix_fmt", "rgba", "-f", "image2pipe", "-vcodec", "png"] if png else
                  ["-q:v", "3", "-f", "image2pipe", "-vcodec", "mjpeg"])
        with self._lock:
            if key not in self._scaled:
                self._scaled[key] = subprocess.check_output([
                    "ffmpeg", "-v", "error", "-threads", "1", "-i", str(path), "-vf",
                    f"scale={w}:{h}:force_original_aspect_ratio=increase:flags=lanczos",
                    "-bitexact", *encode, "-"])
            return ctype, self._scaled[key]

    # Collections whose names exercise the collection tile's name fitting
    # (`ui/collection_tile.rs::fit_name`): one short line, a balanced three, and one too long for
    # three lines at either size, which steps down and elides. Demo-only: the generated library
    # keeps its closed alphabet.
    NAME_FITTING_COLLECTIONS = (
        ("Shorts", 0),
        ("Blender Studio Anniversary Collection", 1),
        ("The Complete Blender Foundation Open Movie Projects Archive Collection", 2),
    )

    def _add_name_fitting_collections(self):
        movies = sorted((it for it in self.items.values() if it["type"] == "movie"),
                        key=lambda it: int(it["ratingKey"]))
        for tag, offset in self.NAME_FITTING_COLLECTIONS:
            cid = len(self.collections) + 1
            self.collections[cid] = {"id": cid, "ratingKey": 50000 + cid, "tag": tag}
            for it in movies[offset * 3:offset * 3 + 3]:
                it.setdefault("Collection", []).append({"tag": tag, "id": cid})

    def collection_metadata(self, section):
        """The base listing, plus each collection's member order as PMS states it
        (`collectionSort`: 0 release date, 2 custom) — custom where the catalog orders it."""
        orders = getattr(self, "catalog", {}).get("collection_order", {})
        return [dict(row, collectionSort="2" if row["title"] in orders else "0")
                for row in super().collection_metadata(section)]

    def collection_rows(self, cid):
        """A collection's members (Home's shelf and the library's alike): in the catalog's
        `collection_order` for it when it has one — a real server's custom collection order —
        otherwise newest addition first."""
        rows = super().collection_rows(cid)
        order = getattr(self, "catalog", {}).get("collection_order", {}).get(
            self.collections[cid]["tag"])
        if order:
            rank = {self.by_slug[slug]: n for n, slug in enumerate(order)}
            rows.sort(key=lambda it: rank[int(it["ratingKey"])])
        return rows

    def section_collection_hubs(self, section, kind):
        """The catalog's collections in one library, as the shelves a real server lists after
        Recently Added: `custom.collection.<section>.<ratingKey>.<ratingKey>` (a live probe: the
        promoted hub's id is the collection's RATING key, not its tag id), in the catalog's order."""
        hubs = []
        unpromoted = {tag for tag, _ in self.NAME_FITTING_COLLECTIONS}
        for c in self.collections.values():
            if c["tag"] in unpromoted:
                continue  # listed in the library's Collections, not promoted as a shelf
            rows = [it for it in self.collection_rows(c["id"]) if it["librarySectionID"] == section]
            if rows:
                hubs.append({"title": c["tag"], "type": kind, "size": len(rows),
                             "hubIdentifier": f"custom.collection.{section}.{c['ratingKey']}.{c['ratingKey']}",
                             "key": f"/library/collections/{c['ratingKey']}/children", "Metadata": rows[:12]})
        return hubs

    def home_hubs(self):
        """`/hubs` after Continue Watching: the catalog's shelves, in its order. A collection shelf
        is published the way a real server promotes one — `custom.collection.<section>.<rk>.<rk>`
        keyed by the collection's member listing — so its heading links to the collection page."""
        out = []
        for h in self.catalog["hubs"]:
            if "recent" in h:
                rows = self.recent(h["recent"], 12)
                ident, key = h["hubIdentifier"], f"/hubs/demo/{h['hubIdentifier']}"
            else:
                coll = next(c for c in self.collections.values() if c["tag"] == h["collection"])
                rows = self.collection_rows(coll["id"])
                section = rows[0]["librarySectionID"] if rows else 1
                rk = coll["ratingKey"]
                ident = f"custom.collection.{section}.{rk}.{rk}"
                key = f"/library/collections/{rk}/children"
            out.append({"title": h["title"], "type": h["type"], "hubIdentifier": ident,
                        "key": key, "size": len(rows), "Metadata": rows})
        return out


# ---------------------------------------------------------------- PNG ----------------------

def flat_png(w, h, rgb):
    """A solid-colour PNG, pure stdlib. Small (one filter byte + a run per row, deflated)."""
    w = max(1, min(int(w), 1920))
    h = max(1, min(int(h), 1920))
    row = b"\x00" + bytes(rgb) * w
    raw = row * h

    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))


def colour_for(path):
    h = hashlib.sha1(path.encode()).digest()
    return (64 + h[0] % 128, 64 + h[1] % 128, 64 + h[2] % 128)


# ---------------------------------------------------------------- PLX-NATIVE-10 (insecure LAN) --

def detect_lan_ip():
    """This host's primary LAN IPv4, best-effort. A UDP socket's `connect` only performs a routing
    lookup — the kernel picks the outbound interface/source address for that destination — and
    never actually sends a packet on `SOCK_DGRAM`, so this needs no network access and reaches
    nothing. Returns `None` on a host with no route at all (offline, a sandboxed CI runner)."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("10.255.255.255", 1))
        return s.getsockname()[0]
    except OSError:
        return None
    finally:
        s.close()


def open_insecure_fail_listener(fail_mode):
    """The failing HTTPS route `--plaintext-only-lan` advertises. `"handshake"`: a real TCP
    listener that accepts every connection and immediately closes it without writing a byte, so a
    TLS ClientHello gets no ServerHello back — an actual, reproducible handshake failure rather
    than a guess about how some firewall treats an unused port. `"unreachable"`: no listener at
    all; the chosen port is picked and released, so nothing is bound there and a connection
    attempt gets whatever the OS/network gives an address nobody is listening on (a real
    ECONNREFUSED here; a silent timeout across a real LAN). Returns `(port, listener_socket)`;
    `listener_socket` is `None` for `"unreachable"` and must be `.close()`d by the caller once the
    accept-and-drop thread is no longer needed."""
    if fail_mode not in ("handshake", "unreachable"):
        raise ValueError(f"unknown fail_mode {fail_mode!r}")
    if fail_mode == "unreachable":
        probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        probe.bind(("0.0.0.0", 0))
        port = probe.getsockname()[1]
        probe.close()
        return port, None
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("0.0.0.0", 0))
    listener.listen(8)
    port = listener.getsockname()[1]

    def accept_and_drop():
        while True:
            try:
                conn, _ = listener.accept()
            except OSError:
                return  # the listener was closed under us: shutting down, not a bug
            conn.close()

    threading.Thread(target=accept_and_drop, name="mock-pms-insecure-fail", daemon=True).start()
    return port, listener


def plaintext_only_lan_resources(lib, ip, http_port, fail_port, access_token):
    """The exact `/api/v2/resources` PLX-NATIVE-10 needs: ONE owned server, no relay connection at
    all, and two connections — the failing `https://…plex.direct` one and the plaintext LAN one,
    which is this same mock (`ip`:`http_port`). Field names match what
    `rust-modules/plex/src/plex/account.rs`'s `Resource`/`Connection` deserialize (`clientIdentifier`,
    `provides`, `owned`, `accessToken`, `httpsRequired`, `publicAddressMatches`,
    `connections[{protocol,address,port,uri,local,relay,IPv6}]`); the top-level shape is a bare
    JSON array, not a `MediaContainer` — plex.tv's envelope, not a PMS one."""
    dashed = ip.replace(".", "-")
    https_uri = f"https://{dashed}.{PLEX_DIRECT_HASH}.plex.direct:{fail_port}"
    return [{
        "name": lib.friendly,
        "clientIdentifier": lib.machine,
        "provides": "server",
        "owned": True,
        "accessToken": access_token,
        "sourceTitle": None,
        "ownerId": 0,
        "home": False,
        "presence": True,
        "publicAddressMatches": True,
        "httpsRequired": False,
        "connections": [
            {"protocol": "https", "address": ip, "port": fail_port, "uri": https_uri,
             "local": True, "relay": False, "IPv6": False},
            {"protocol": "http", "address": ip, "port": http_port,
             "uri": f"http://{ip}:{http_port}", "local": True, "relay": False, "IPv6": False},
        ],
    }]


# ---------------------------------------------------------------- the server ----------------

class MockPms:
    def section_pad_hubs(self, key, kind, count, rows=12):
        """`count` synthetic hubs for `/hubs/sections/<key>` (#412), after the library's own.

        Plain ones are `mock.section.<key>.shelf.<i>`, listed by `/hubs/mock/section/<key>/shelf/<i>`.
        `section_hubs_linked` of them (spread evenly through the run; movie section only, where the
        collections live) are promoted collections the way a real server publishes one,
        `custom.collection.<section>.<rk>.<n>` keyed `/library/collections/<rk>/children`, which the
        app links the shelf heading to (`promoted_collection_link`) and the mock resolves. The
        last identifier segment is the hub's own number, not the rating key a server repeats there,
        so every hub keeps a distinct identity while the few collections the library has are reused.
        """
        lib = self.lib
        recent = lib.recent(kind, rows)
        collections = [c for c in lib.collections.values()
                       if kind == "movie" and any(it["librarySectionID"] == int(key)
                                                  for it in lib.collection_rows(c["id"]))]
        linked = self.section_hubs_linked if collections else 0
        hubs, made = [], 0
        for i in range(1, count + 1):
            if linked and i * linked // count > (i - 1) * linked // count:
                c = collections[made % len(collections)]
                made += 1
                members = [it for it in lib.collection_rows(c["id"])
                           if it["librarySectionID"] == int(key)][:rows]
                hubs.append({"title": c["tag"], "type": kind, "size": len(members),
                             "hubIdentifier": f"custom.collection.{key}.{c['ratingKey']}.{i}",
                             "key": f"/library/collections/{c['ratingKey']}/children",
                             "Metadata": members})
            else:
                hubs.append({"title": f"Mock Shelf {i}", "type": kind, "size": len(recent),
                             "hubIdentifier": f"mock.section.{key}.shelf.{i}",
                             "key": f"/hubs/mock/section/{key}/shelf/{i}",
                             "more": False, "Metadata": recent})
        return hubs

    def __init__(self, lib):
        self.lib = lib
        # `--home-hubs N` (#395): `/hubs` answers exactly N hubs; 0 keeps the library's own.
        self.home_hubs = 0
        # `--section-hubs N` (#412): `/hubs/sections/<id>` answers exactly N hubs; 0 keeps the
        # library's own. `--section-hubs-linked M` makes M of the padding hubs promoted collections.
        self.section_hubs = 0
        self.section_hubs_linked = 0
        self.lock = threading.Lock()
        self.requests = []  # (path, status) in arrival order, for the harness
        self.unknown = []
        self.writes = []
        # Set by `serve` after binding the listener; ordinary sign-in advertises only this mock.
        self.account_resource = None
        # The specialized TLS-failure reproduction overrides ordinary resource discovery.
        # Set by `serve(..., plaintext_only_lan=True)` to ip/http_port/fail_port/access_token.
        self.plaintext_only_lan = None
        # `--authorize-after N`: the pin poll links the demo code on the Nth poll; `None` never.
        self.authorize_after = None
        # `--home-users N`: how many profiles the Home roster lists (the first is the Demo owner).
        self.home_users = 1
        self.pin_polls = 0
        self.user_profile = {"autoSelectAudio": True, "defaultAudioLanguage": "en",
                             "defaultSubtitleLanguage": "en", "autoSelectSubtitle": 1,
                             "defaultSubtitleForced": 0, "defaultSubtitleAccessibility": 0}
        # #266: myPlexSubscription on `/`/`/identity`; `serve(plex_pass=False)` or
        # `POST /_mock/config {"plex_pass": false}` flips it live.
        self.plex_pass = True
        # #266: `/decision` behaviour for a live boostDialog/normalizeLoudness param — both
        # runtime-togglable through `POST /_mock/config`, so one test case can resolve once and
        # only then start refusing/ignoring.
        self.refuse_enhancements = False
        self.ignore_enhancements = False
        # `--transcode-fixture PATH`: serve this file (Range/206 supported) for every
        # /video/:/transcode/universal/start* request instead of the plain 404.
        self.transcode_fixture = None
        # `--decision-delay-ms N`: hold every transcode decision/start request this long before
        # answering, so a flight's PMS half is observably in flight. 0 answers at once.
        self.decision_delay_ms = 0
        # Every request `handle()` routed, in arrival order: {"method","path","query","session"}.
        # Query keys naming a token are stripped, never just redacted — never send a real value.
        self.request_log = []
        # subtitle search: rk -> {candidate key -> row} for the latest search; id sequences
        self.sub_candidates, self.sub_candidate_seq, self.sub_install_seq = {}, 0, 0
        self.sub_downloaded = set()  # stream ids the search flow installed and PMS has not yet discarded

    @staticmethod
    def safe_path(path):
        """A request target fit for stderr: useful query, never an auth token."""
        u = urllib.parse.urlsplit(path)
        pairs = urllib.parse.parse_qsl(u.query, keep_blank_values=True)
        clean = [(k, "<redacted>" if "token" in k.lower() else v) for k, v in pairs]
        return urllib.parse.urlunsplit(("", "", u.path, urllib.parse.urlencode(clean), ""))

    def note_write(self, method, path, body=b""):
        safe = self.safe_path(path)
        with self.lock:
            self.writes.append((method, safe, body))
        print(f"mock_pms: WRITE {method} {safe}", file=sys.stderr, flush=True)

    # --- subtitle search & download (`docs/pms-api.md` §8) -----------------------------------

    def subtitle_search(self, lib, rk, method, q, j):
        """The agent-backed subtitle search, modelled on the measured PMS shape.

        GET answers `Stream[]` candidates for a 2-letter `language` (a 3-letter one is the 500
        the real server gives; an item with no Media is the 404). Candidate keys are
        `/library/streams/<id>` and only valid for the LATEST search of that item, as on PMS.
        PUT `key=<candidate>` answers 200 with an empty body and appends an external SRT
        stream with a NEW id to the item's first part, backed by a tiny sidecar body; a stale
        or unknown key is accepted and does nothing. The install is immediate here — PMS's is
        asynchronous, which the client's poll already covers. Nothing here reaches a real
        server: this is the only place the download flow may be exercised end to end."""
        it = lib.items.get(rk)
        parts = (it or {}).get("Media", [{}])[0].get("Part") if it else None
        if not parts:
            return j(self.container(), 404)
        if method == "GET":
            lang = q.get("language", "")
            if len(lang) != 2 or not lang.isalpha():
                return (500, "text/plain", b"mock_pms: language must be 2 letters")
            code = SUB_SEARCH_LANGS.get(lang.lower())
            rows = []
            with self.lock:
                self.sub_candidates[rk] = {}
                for n in range(2 if code else 0):
                    self.sub_candidate_seq += 1
                    cid = SUB_CANDIDATE_BASE + self.sub_candidate_seq
                    row = {"id": cid, "key": f"/library/streams/{cid}", "streamType": 3,
                           "codec": "srt", "format": "srt", "language": code[1],
                           "languageCode": code[0], "providerTitle": "OpenSubtitles",
                           "score": str(2000 - 500 * n), "title": f"mock.release.{lang}.{n + 1}",
                           "hearingImpaired": n == 1}
                    self.sub_candidates[rk][row["key"]] = row
                    rows.append(row)
            return j(self.container(Stream=rows))
        if method == "PUT":
            with self.lock:
                row = self.sub_candidates.get(rk, {}).pop(q.get("key", ""), None)
                if row:
                    self.sub_install_seq += 1
                    sid = SUB_INSTALLED_BASE + self.sub_install_seq
                    self.sub_downloaded.add(sid)
                    streams = parts[0].setdefault("Stream", [])
                    streams.append({
                        "id": sid, "streamType": 3, "codec": "srt", "format": "srt",
                        "language": row["language"], "languageCode": row["languageCode"],
                        "key": f"/library/streams/{sid}", "title": row["title"],
                        "displayTitle": row["language"],
                        "extendedDisplayTitle": f"{row['title']} ({row['language']} SRT)",
                        "index": max((s.get("index", 0) for s in streams), default=0) + 1,
                        "selected": True})
                    for other in streams:
                        if other.get("streamType") == 3 and other["id"] != sid:
                            other.pop("selected", None)
                    if getattr(lib, "sidecars", None) is None:
                        lib.sidecars = {}  # a generated library without --media has none yet
                    lib.sidecars[sid] = (b"1\n00:00:01,000 --> 00:00:04,000\n"
                                         + row["title"].encode() + b"\n")
            return (200, "text/plain", b"")
        return (405, "text/plain", b"")

    def discard_deselected_downloads(self, lib, part, selected):
        """PMS drops a subtitle the search flow downloaded as soon as the part's subtitle selection
        moves to another stream or to 0 (measured 2026-10-06, `docs/pms-api.md`): the stream leaves
        the part, its `/library/streams/<id>` answers 404 and selecting it again is a 400."""
        with self.lock:
            for stream in list(part.get("Stream", [])):
                sid = stream["id"]
                if stream["streamType"] == 3 and sid in self.sub_downloaded and sid != selected:
                    part["Stream"].remove(stream)
                    self.sub_downloaded.discard(sid)
                    if not hasattr(lib, "sub_discarded"):
                        lib.sub_discarded = set()
                    lib.sub_discarded.add(sid)
                    getattr(lib, "sidecars", {}).pop(sid, None)

    # --- containers ------------------------------------------------------------------------

    def container(self, **kw):
        mc = {"size": 0, "allowSync": False, "identifier": "com.plexapp.plugins.library",
              "mediaTagPrefix": "/system/bundle/media/flags/", "mediaTagVersion": 1}
        mc.update(kw)
        for k in ("Metadata", "Directory", "Hub"):
            if k in mc:
                mc["size"] = len(mc[k])
        return {"MediaContainer": mc}

    def sort_meta(self, kind=None):
        if kind == "18":
            # PMS's collection listing declares ONE sort (probed on 1.43): titleSort.
            return {"Type": [{"key": "/library/sections/1/all?type=18", "type": "collection",
                              "title": "Collections", "active": True,
                              "Sort": [{"key": "titleSort", "defaultDirection": "asc",
                                        "descKey": "titleSort:desc", "title": "Title"}]}]}
        return {"Type": [{"key": "/library/sections/1/all?type=1", "type": "movie", "title": "movie",
                          "active": True,
                          "Sort": [{"key": "titleSort", "defaultDirection": "asc", "title": "Title"},
                                   {"key": "addedAt", "defaultDirection": "desc", "title": "Date Added"},
                                   {"key": "originallyAvailableAt", "defaultDirection": "desc", "title": "Release Date"},
                                   {"key": "rating", "defaultDirection": "desc", "title": "Critic Rating"},
                                   {"key": "lastViewedAt", "defaultDirection": "desc", "title": "Date Viewed"},
                                   {"key": "random", "defaultDirection": "asc", "title": "Randomly"}]}]}

    def handle(self, method, path, body=b"", headers=None):
        """Returns (status, content_type, bytes)."""
        u = urllib.parse.urlsplit(path)
        p = u.path
        q = {k: v[-1] for k, v in urllib.parse.parse_qs(u.query, keep_blank_values=True).items()}
        headers = headers or {}
        lib = self.lib
        j = lambda obj, status=200: (status, "application/json", json.dumps(obj).encode())
        segs = [s for s in p.split("/") if s]

        # Never a token value, even in the in-memory log: strip any query key naming one.
        logged_query = {k: v for k, v in q.items() if "token" not in k.lower()}
        with self.lock:
            self.request_log.append({
                "method": method, "path": p, "query": logged_query,
                "session": headers.get("X-Plex-Session-Identifier"),
            })

        if p == "/_mock/requests":
            if method == "GET":
                with self.lock:
                    return j(list(self.request_log))
            if method == "DELETE":
                with self.lock:
                    self.request_log.clear()
                return j({"ok": True})
            return j({"error": "method not allowed"}, 405)
        if p == "/_mock/config":
            if method != "POST":
                return j({"error": "method not allowed"}, 405)
            try:
                patch = json.loads(body or b"{}")
            except json.JSONDecodeError:
                return j({"error": "invalid JSON body"}, 400)
            if not isinstance(patch, dict):
                return j({"error": "body must be a JSON object"}, 400)
            allowed = {"refuse_enhancements", "ignore_enhancements", "plex_pass"}
            unknown = set(patch) - allowed
            if unknown:
                return j({"error": f"unknown config key(s): {sorted(unknown)}"}, 400)
            with self.lock:
                for key, value in patch.items():
                    setattr(self, key, bool(value))
                current = {k: getattr(self, k) for k in allowed}
            return j(current)

        def paged(rows):
            start = int(q.get("X-Plex-Container-Start",
                              headers.get("X-Plex-Container-Start", 0)))
            size = int(q.get("X-Plex-Container-Size",
                             headers.get("X-Plex-Container-Size", len(rows))))
            return rows[start:start + size], {"totalSize": len(rows), "offset": start}

        write_path = (p in ("/:/timeline", "/:/scrobble", "/:/unscrobble", "/:/progress",
                            "/actions/removeFromContinueWatching", "/status/sessions/close",
                            "/video/:/transcode/universal/stop", "/playQueues")
                      or p.startswith("/library/parts/")
                      or (segs[:1] == ["playQueues"] and len(segs) == 2)
                      or (method == "PUT" and len(segs) == 4 and segs[:2] == ["library", "metadata"]
                          and segs[3] == "subtitles"))
        if write_path and not (method in ("GET", "HEAD") and p.startswith("/library/parts/")):
            self.note_write(method, path, body)

        # plex.tv's QR sign-in, for `plxnative-plextv` (see the module doc). Pending — the state
        # the sign-in screen is captured in — unless `--authorize-after N` links it on the Nth poll.
        if p == "/api/v2/pins" and method == "POST":
            return j({"id": DEMO_PIN_ID, "code": DEMO_PIN_CODE, "expiresIn": 1800, "authToken": None,
                      "qr": ""}, 201)
        if p == f"/api/v2/pins/{DEMO_PIN_ID}":
            with self.lock:
                self.pin_polls += 1
                linked = self.authorize_after is not None and self.pin_polls >= self.authorize_after
            return j({"id": DEMO_PIN_ID, "code": DEMO_PIN_CODE, "expiresIn": 1800,
                      "authToken": DEMO_ACCOUNT_TOKEN if linked else None})
        if p == f"/api/v2/pins/qr/{DEMO_PIN_CODE}":
            import demo_library.qr as qr
            return (200, "image/png", qr.png(qr.encode(DEMO_QR_TEXT, "M"), scale=8, border=0, plex_style=True))
        if p == "/api/v2/user":
            with self.lock:
                profile = dict(self.user_profile)
            return j({"id": 1, "uuid": "demo-user", "username": "demo", "title": "Demo",
                      "friendlyName": "Demo", "email": "demo@example.invalid", "thumb": "",
                      "profile": profile,
                      "subscription": {"active": True, "status": "Active", "plan": "lifetime"}})
        if p == "/api/v2/user/profile":
            if method == "PUT":
                if body:
                    return j({"error": "profile PUT requires an empty body"}, 400)
                patch = {}
                for key, value in q.items():
                    if key in ("defaultAudioLanguage", "defaultSubtitleLanguage"):
                        patch[key] = value
                    elif key == "autoSelectAudio" and value in ("true", "false", "1", "0"):
                        patch[key] = value in ("true", "1")
                    elif key in ("autoSelectSubtitle", "defaultSubtitleForced"):
                        limit = 2 if key == "autoSelectSubtitle" else 3
                        if not value.isdigit() or int(value) > limit:
                            return j({"error": "invalid profile preference"}, 400)
                        patch[key] = int(value)
                    else:
                        return j({"error": "unknown profile preference"}, 400)
                self.note_write(method, path, body)
                with self.lock:
                    self.user_profile.update(patch)
            elif method not in ("GET", "HEAD"):
                return j({"error": "method not allowed"}, 405)
            with self.lock:
                return j(dict(self.user_profile))
        if p == "/api/v2/home/users":
            return j({"users": home_roster(self.home_users)})
        if method == "POST" and p.startswith("/api/v2/home/users/") and p.endswith("/switch"):
            who = next((u for u in home_roster(self.home_users)
                        if p == f"/api/v2/home/users/{u['uuid']}/switch"), None)
            if who is not None:
                return j({"id": who["id"], "uuid": who["uuid"], "title": who["title"],
                          "authToken": DEMO_ACCOUNT_TOKEN})
        if p == "/api/v2/resources":
            if self.plaintext_only_lan is not None:
                cfg = self.plaintext_only_lan
                return j(plaintext_only_lan_resources(
                    lib, cfg["ip"], cfg["http_port"], cfg["fail_port"], cfg["access_token"]))
            return j([self.account_resource] if self.account_resource is not None else [])
        catalog = isinstance(lib, CatalogLibrary)
        if p == "/" or p == "/identity":
            return j(self.container(machineIdentifier=lib.machine, friendlyName=lib.friendly,
                                    version="1.41.0.0000-synthetic",
                                    myPlexSubscription=self.plex_pass,
                                    platform="Linux", myPlex=True))
        if p == "/library/sections":
            return j(self.container(Directory=[dict(s, agent="tv.plex.agents.movie",
                                                    scanner="Plex Movie", language="en-US",
                                                    art="/:/resources/movie-fanart.jpg",
                                                    composite=f"/library/sections/{s['key']}/composite/1",
                                                    refreshing=False, allowSync=False)
                                               for s in lib.sections]))
        if len(segs) == 4 and segs[:2] == ["library", "sections"] and segs[3] == "all":
            if q.get("type") == "18":
                rows = lib.collection_metadata(segs[2])
                field, _, direction = q.get("sort", "titleSort").partition(":")
                keyf = {
                    "titleSort": lambda row: row["titleSort"],
                    "updatedAt": lambda row: row["updatedAt"],
                    "childCount": lambda row: row["childCount"],
                    "random": lambda row: hashlib.md5(row["ratingKey"].encode()).hexdigest(),
                }.get(field, lambda row: row["titleSort"])
                rows.sort(key=keyf, reverse=(direction == "desc"))
            else:
                rows = lib.section_items(segs[2], q)
            page, extra = paged(rows)
            if q.get("includeMeta") == "1":
                extra["Meta"] = self.sort_meta(q.get("type"))
            return j(self.container(Metadata=page, **extra))
        if len(segs) == 4 and segs[:2] == ["library", "sections"]:
            d = segs[3]
            if d == "collections":
                rows, extra = paged(lib.collection_metadata(segs[2]))
                return j(self.container(Metadata=rows, **extra))
            if d == "genre":
                return j(self.container(Directory=[{"key": str(g["id"]), "title": g["tag"],
                                                    "fastKey": f"/library/sections/{segs[2]}/all?genre={g['id']}"}
                                                   for g in lib.genres.values()]))
            if d == "firstCharacter":
                return j(self.container(Directory=lib.first_characters(segs[2], q)))
            return j(self.container(Directory=[]))
        if len(segs) >= 3 and segs[:2] == ["library", "metadata"]:
            ids = segs[2]
            if len(segs) == 3:
                rows = [lib.items[int(x)] for x in ids.split(",") if x.isdigit() and int(x) in lib.items]
                if not rows and ids.isdigit():
                    coll = lib.collection_by_rating_key(int(ids))
                    rows = [row for row in lib.collection_metadata(1)
                            if coll and int(row["ratingKey"]) == coll["ratingKey"]]
                if not rows:
                    return j(self.container(Metadata=[]), 404)
                if q.get("includePreferences") == "1" and len(rows) == 1 \
                        and int(ids) == VERIFY_SHOW_RK and getattr(lib, "media_files", None):
                    rows = [dict(rows[0], Preferences={"Setting": list(VERIFY_PREFS)})]
                return j(self.container(Metadata=rows))
            rk = int(ids) if ids.isdigit() else -1
            sub = segs[3]
            if sub == "tree" and rk == VERIFY_SHOW_RK and getattr(lib, "media_files", None):
                return j(self.container(Setting=list(VERIFY_PREFS)))
            if sub == "children":
                return j(self.container(Metadata=lib.children(rk)))
            if sub == "allLeaves":
                return j(self.container(Metadata=lib.leaves(rk)))
            if sub == "subtitles":
                return self.subtitle_search(lib, rk, method, q, j)
            if sub == "related":
                it = lib.items.get(rk)
                pool = [x for x in lib.items.values() if it and x["type"] == it["type"] and x is not it]
                hubs = [{"title": "related", "type": it["type"] if it else "movie",
                         "hubIdentifier": "related", "size": min(8, len(pool)), "Metadata": pool[:8]}]
                # A member movie also gets one `collection.related.{section}.{n}` hub per
                # collection (a live probe): titled with the collection, keyed by the section's
                # TAG-id filter, listing EVERY member — the movie itself included.
                tags = it.get("Collection", []) if it and it["type"] == "movie" else []
                for n, tag in enumerate(tags, 1):
                    members = lib.collection_rows(tag["id"])
                    sec = it.get("librarySectionID", 1)
                    hubs.append({
                        "title": tag["tag"], "type": "movie", "size": len(members),
                        "hubIdentifier": f"collection.related.{sec}.{n}",
                        "key": f"/library/sections/{sec}/all?type=1&tagId={tag['id']}"
                               "&sort=originallyAvailableAt,year:nullsLast",
                        "Metadata": members})
                return j(self.container(Hub=hubs))
            if catalog:
                img = lib.image(p, q.get("width"), q.get("height"))
                return (200, *img) if img else (404, "text/plain", b"no image")
            if sub in ("thumb", "art"):
                png = flat_png(q.get("width", 250), q.get("height", 375), colour_for(p))
                return (200, "image/png", png)
            return j(self.container())
        if len(segs) == 4 and segs[:2] == ["library", "collections"] \
                and segs[3] == "children":
            rk = int(segs[2]) if segs[2].isdigit() else -1
            coll = lib.collection_by_rating_key(rk)
            rows = lib.collection_rows(coll["id"]) if coll else []
            page, extra = paged(rows)
            return j(self.container(Metadata=page, **extra), 200 if coll else 404)
        if segs[:2] == ["library", "people"] and len(segs) == 4 and segs[3] == "media":
            pid = int(segs[2]) if segs[2].isdigit() else -1
            return j(self.container(Metadata=lib.person_media(pid)))
        if p == "/hubs" or p == "/hubs/promoted":
            hubs = [{"title": "home.continue", "type": "mixed", "hubIdentifier": "home.continue",
                     "key": "/hubs/continueWatching", "size": 0, "Metadata": []},
                    {"title": "home.movies.recent", "type": "movie", "hubIdentifier": "home.movies.recent",
                     "key": "/library/sections/1/recentlyAdded", "Metadata": lib.recent("movie")},
                    {"title": "home.television.recent", "type": "episode",
                     "hubIdentifier": "home.television.recent",
                     "key": "/library/sections/2/recentlyAdded", "Metadata": lib.recent("episode")}]
            if catalog:
                hubs = hubs[:1] + lib.home_hubs()
                hubs[0]["title"] = "Continue Watching"
            if q.get("excludeContinueWatching") != "1":
                hubs[0]["Metadata"] = lib.continue_watching()[:12]
            if self.home_hubs:
                # #395: pad Home with synthetic shelves up to exactly `--home-hubs` hubs.
                rows = lib.recent("movie", int(q.get("count", 12)))
                for i in range(1, self.home_hubs - len(hubs) + 1):
                    hubs.append({"title": f"Mock Shelf {i}", "type": "movie",
                                 "hubIdentifier": f"mock.shelf.{i}", "key": f"/hubs/mock/shelf/{i}",
                                 "more": False, "Metadata": rows})
            for h in hubs:
                h["size"] = len(h["Metadata"])
            return j(self.container(Hub=hubs))
        if len(segs) == 4 and segs[:3] == ["hubs", "mock", "shelf"] and self.home_hubs:
            page, extra = paged(lib.recent("movie", 1000))
            return j(self.container(Metadata=page, **extra))
        if catalog and p == "/hubs/continueWatching":
            rows = lib.continue_watching()[:int(q.get("count", 12))]
            return j(self.container(Hub=[{"title": "Continue Watching", "type": "mixed",
                                          "hubIdentifier": "home.continue", "key": "/hubs/continueWatching",
                                          "size": len(rows), "Metadata": rows}]))
        if p == "/hubs/continueWatching":
            rows = lib.continue_watching()[:int(q.get("count", 12))]
            return j(self.container(Hub=[{"title": "home.continue", "type": "mixed",
                                          "hubIdentifier": "home.continue", "key": "/hubs/continueWatching",
                                          "size": len(rows), "Metadata": rows}]))
        if len(segs) == 3 and segs[:2] == ["hubs", "sections"]:
            key = segs[2]
            kind = {"1": "movie", "2": "show"}.get(key, "movie")
            cw = [it for it in lib.continue_watching() if it["librarySectionID"] == int(key)]
            hubs = [{"title": f"{kind}.inprogress.{key}", "type": kind, "hubIdentifier": f"{kind}.inprogress.{key}",
                     "size": len(cw), "Metadata": cw[:12]},
                    {"title": f"{kind}.recentlyadded.{key}", "type": kind,
                     "hubIdentifier": f"{kind}.recentlyadded.{key}", "size": 12,
                     "Metadata": lib.recent(kind)}]
            if catalog:
                # The identifiers stay; the titles are the ones a real server shows, because a
                # catalog run is photographed and an identifier on screen is a mock showing through.
                hubs[0]["title"] = "Continue Watching"
                hubs[1]["title"] = "Recently Added Movies" if kind == "movie" else "Recently Added TV"
                # A real server lists the library's collections as shelves of their own after
                # these (docs/pms-api.md, 3a), so the catalog's do the same.
                hubs += lib.section_collection_hubs(int(key), kind)
            if self.section_hubs:
                # #412: pad the section's shelves up to exactly `--section-hubs` hubs.
                hubs = hubs[:self.section_hubs]
                hubs += self.section_pad_hubs(key, kind, self.section_hubs - len(hubs))
            return j(self.container(Hub=hubs))
        if len(segs) == 6 and segs[:3] == ["hubs", "mock", "section"] and segs[4] == "shelf" \
                and self.section_hubs:
            kind = {"1": "movie", "2": "show"}.get(segs[3], "movie")
            page, extra = paged(lib.recent(kind, 1000))
            return j(self.container(Metadata=page, **extra))
        if p == "/hubs/search":
            lim = q.get("limit", "")
            return j(self.container(Hub=lib.search(q.get("query", ""), int(lim) if lim.isdigit() else 3,
                                                   include_collections=q.get("includeCollections") == "1")))
        if p == "/photo/:/transcode" and catalog:
            img = lib.image(q.get("url", ""), q.get("width"), q.get("height"))
            return (200, *img) if img else (404, "text/plain", b"no image")
        if p == "/photo/:/transcode":
            src = q.get("url", "")
            png = flat_png(q.get("width", 250), q.get("height", 375), colour_for(src))
            return (200, "image/png", png)
        if p in ("/:/scrobble", "/:/unscrobble"):
            rk = q.get("key", "")
            with self.lock:
                it = lib.items.get(int(rk)) if rk.isdigit() else None
                if it:
                    if p == "/:/scrobble":
                        it["viewCount"] = it.get("viewCount", 0) + 1
                        it.pop("viewOffset", None)
                        it["lastViewedAt"] = int(time.time())
                    else:
                        it.pop("viewCount", None)
                        it.pop("viewOffset", None)
                    lib._roll_up()
            return j(self.container())
        if p in ("/:/timeline", "/:/progress", "/actions/removeFromContinueWatching",
                 "/status/sessions/close", "/video/:/transcode/universal/stop"):
            if p == "/:/timeline":
                rk = q.get("key", "").rsplit("/", 1)[-1]
                t = q.get("time")
                with self.lock:
                    it = lib.items.get(int(rk)) if rk.isdigit() else None
                    if it and t and t.isdigit():
                        it["viewOffset"] = int(t)
                        it["lastViewedAt"] = int(time.time())
            return j(self.container())
        if p == "/playQueues":
            uri = q.get("uri", "")
            rk = uri.rsplit("/", 1)[-1]
            it = lib.items.get(int(rk)) if rk.isdigit() else None
            # Only the demo library follows `continuous=1` past the item: the seeded library's
            # queue of one is what the harness's recorded cases were taken against.
            if it is None:
                queue = []
            elif q.get("continuous") == "1" and isinstance(lib, CatalogLibrary):
                queue = lib.continuous_queue(it)
            else:
                queue = [it]
            rows = [dict(x, playQueueItemID=n) for n, x in enumerate(queue, start=1)]
            return j(self.container(Metadata=rows, playQueueID=1, playQueueSelectedItemID=1,
                                    playQueueSelectedItemOffset=0, playQueueTotalCount=len(rows),
                                    playQueueVersion=1))
        if segs[:1] == ["playQueues"] and len(segs) == 2:
            return j(self.container(Metadata=[], playQueueID=int(segs[1]) if segs[1].isdigit() else 1))
        if p.startswith("/library/parts/"):
            part_id = int(segs[2]) if len(segs) > 2 and segs[2].isdigit() else -1
            if method == "PUT":
                streams = getattr(lib, "verification_streams", {}).get(part_id, [])
                if q.get("subtitleStreamID", "").isdigit():
                    # Selecting a downloaded stream PMS already discarded is the 400 it answers.
                    wanted = int(q["subtitleStreamID"])
                    with self.lock:
                        if wanted in getattr(lib, "sub_discarded", set()):
                            return j(self.container(), 400)
                for kind, key in ((2, "audioStreamID"), (3, "subtitleStreamID")):
                    if key not in q:
                        continue
                    selected = int(q[key]) if q[key].isdigit() else 0
                    for stream in streams:
                        if stream["streamType"] == kind:
                            stream["selected"] = stream["id"] == selected
                    # The external stream is appended to the item's part, not the probe list.
                    for item in lib.items.values():
                        for media in item.get("Media", []):
                            for part in media.get("Part", []):
                                if part.get("id") == part_id:
                                    for stream in part.get("Stream", []):
                                        if stream["streamType"] == kind:
                                            stream["selected"] = stream["id"] == selected
                                    if kind == 3:
                                        self.discard_deselected_downloads(lib, part, selected)
                return j(self.container())
            # the media bytes: nothing here decodes, but the app's part probe must get a 200 with a
            # length so the route planner reaches its own (host-side) failure instead of a socket one
            return (200, "video/x-matroska", b"\x1a\x45\xdf\xa3" + b"\x00" * 60)
        if len(segs) == 3 and segs[:2] == ["library", "streams"]:
            sid = int(segs[2]) if segs[2].isdigit() else -1
            if sid in getattr(lib, "sub_discarded", set()):
                return (404, "text/plain", b"")
            sidecar = getattr(lib, "sidecars", {}).get(sid)
            if sidecar is not None:
                body = sidecar if isinstance(sidecar, bytes) else sidecar.read_bytes()
                return (200, "application/x-subrip", body)
        if p.startswith("/video/:/transcode/universal/start"):
            # an HLS/transcode START: this server encodes nothing, so the honest answer is the one
            # a PMS gives for a session it cannot serve — the app's route planner then lands on
            # its failure read-out, which is the player screen a simulator can reach
            return (404, "text/plain", b"mock_pms: no transcoder")
        if p == "/video/:/transcode/universal/decision":
            wanted = q.get("path", "").rsplit("/", 1)[-1]
            rk = int(wanted) if wanted.isdigit() else -1
            item = lib.items.get(rk)
            part_id = (item.get("Media", [{}])[0].get("Part", [{}])[0].get("id")
                       if item else None)
            # #266: a LIVE boostDialog=1 or normalizeLoudness=1 — a `0` or absent param is
            # byte-identical to the branches below, untouched. Measured against PMS 1.43.4 with
            # Plex Pass (design plan M1/M2): whether the request shape was the remux one
            # (directPlay=0&directStream=1) or the MDE probe (directPlay=1&directStreamAudio=1),
            # the Part becomes a transcode — video stays "copy", audio becomes "transcode" at
            # `ac3`, same channel count as the source — so both shapes are modelled identically
            # here rather than branching on directPlay/directStream at all.
            enhancement = q.get("boostDialog") == "1" or q.get("normalizeLoudness") == "1"
            if item is not None and part_id is not None and enhancement:
                if self.refuse_enhancements:
                    # The refusal shape `route/plan.rs::refusal` reads: `generalDecisionCode`
                    # 2000, with the cause in `transcodeDecisionText` (falling back to
                    # `generalDecisionText` only when the server sends no transcode text).
                    return j(self.container(
                        generalDecisionCode=2000,
                        generalDecisionText="Neither direct play nor conversion is available.",
                        transcodeDecisionCode=4020,
                        transcodeDecisionText="Server declined the requested audio enhancement.",
                        mdeDecisionCode=2000, Metadata=[]))
                row = json.loads(json.dumps(item))
                part = row["Media"][0]["Part"][0]
                part["decision"] = "transcode"
                for stream in part["Stream"]:
                    if stream["streamType"] != 2:
                        stream["decision"] = "copy"
                    elif self.ignore_enhancements:
                        # An old/ignoring server: accepts the params, audio decision stays copy.
                        stream["decision"] = "copy"
                    else:
                        stream["decision"] = "transcode"
                        stream["codec"] = "ac3"
                return j(self.container(generalDecisionCode=1000, generalDecisionText="Transcode OK",
                                        mdeDecisionCode=1000, transcodeDecisionCode=1000,
                                        Metadata=[row]))
            # Any item backed by a REAL probed file (--media or --extra-media) answers direct
            # play, the same way a PMS does for a file its own caps accept — not just the two
            # fixed verification ids. `media_files` is the one place that distinguishes "this rk
            # has real bytes behind it" from a purely synthetic generated item.
            if part_id is not None and part_id in getattr(lib, "media_files", {}):
                # MDE answers with the same measured item plus only its decision fields.
                row = json.loads(json.dumps(item))
                part = row["Media"][0]["Part"][0]
                part["decision"] = "directplay"
                for stream in part["Stream"]:
                    stream["decision"] = "copy"
                return j(self.container(generalDecisionCode=1000, mdeDecisionCode=1000,
                                        generalDecisionText="Direct play OK", Metadata=[row]))
            return j(self.container(generalDecisionCode=1000, generalDecisionText="Direct play OK",
                                    mdeDecisionCode=1000, Metadata=[]))
        self.unknown.append(p)
        print(f"mock_pms: UNKNOWN {method} {path}", file=sys.stderr, flush=True)
        return j(self.container())


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "MockPMS/0"

    def log_message(self, fmt, *args):
        pass  # _do logs a redacted request path; BaseHTTPRequestHandler would log tokens.

    def _serve_file_ranged(self, method, path, content_type):
        """A byte range GET/HEAD of `path`, real `Range`/206 handling included — the same helper
        `/library/parts/<id>` verification bytes and `--transcode-fixture` both serve through, so
        the start.mkv fixture gets exactly the Range semantics the part-probe path already
        proved. Returns the status actually sent."""
        size = path.stat().st_size
        start, end, status = 0, max(0, size - 1), 200
        raw_range = self.headers.get("Range")
        if raw_range:
            match = re.fullmatch(r"bytes=(\d*)-(\d*)", raw_range.strip())
            if not match or (not match.group(1) and not match.group(2)):
                self.send_response(416)
                self.send_header("Content-Range", f"bytes */{size}")
                self.send_header("Content-Length", "0")
                self.end_headers()
                return 416
            if match.group(1):
                start = int(match.group(1))
                end = min(int(match.group(2)) if match.group(2) else end, end)
            else:
                suffix = int(match.group(2))
                start = max(0, size - suffix)
            if start >= size or end < start:
                self.send_response(416)
                self.send_header("Content-Range", f"bytes */{size}")
                self.send_header("Content-Length", "0")
                self.end_headers()
                return 416
            status = 206
        length = end - start + 1
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(length))
        self.send_header("Accept-Ranges", "bytes")
        if status == 206:
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.send_header("X-Plex-Protocol", "1.0")
        self.end_headers()
        if method != "HEAD":
            with path.open("rb") as src:
                src.seek(start)
                left = length
                while left:
                    block = src.read(min(left, 256 * 1024))
                    if not block:
                        break
                    self.wfile.write(block)
                    left -= len(block)
        return status

    def _media(self, method):
        u = urllib.parse.urlsplit(self.path)
        segs = [s for s in u.path.split("/") if s]
        if len(segs) < 3 or segs[:2] != ["library", "parts"] or not segs[2].isdigit():
            return False
        part_id = int(segs[2])
        path = getattr(self.server.pms.lib, "media_files", {}).get(part_id)
        if path is None:
            return False
        content_type = getattr(self.server.pms.lib, "media_content_type", {}).get(
            part_id, "video/x-matroska")
        status = self._serve_file_ranged(method, path, content_type)
        with self.server.pms.lock:
            self.server.pms.requests.append((self.path, status))
        return True

    def _transcode_fixture(self, method):
        u = urllib.parse.urlsplit(self.path)
        if not u.path.startswith("/video/:/transcode/universal/start"):
            return False
        path = getattr(self.server.pms, "transcode_fixture", None)
        if path is None:
            return False
        container = path.suffix.lstrip(".").lower()
        content_type = Library._CONTAINER_CONTENT_TYPE.get(container, "video/x-matroska")
        status = self._serve_file_ranged(method, path, content_type)
        with self.server.pms.lock:
            self.server.pms.requests.append((self.path, status))
        return True

    def _do(self, method):
        if self.server.verbose:
            print(f"mock_pms: REQUEST {method} {self.server.pms.safe_path(self.path)}",
                  file=sys.stderr, flush=True)
        delay_ms = self.server.pms.decision_delay_ms
        if delay_ms and urllib.parse.urlsplit(self.path).path.startswith(
                ("/video/:/transcode/universal/decision", "/video/:/transcode/universal/start")):
            time.sleep(delay_ms / 1000)
        if method in ("GET", "HEAD") and (self._media(method) or self._transcode_fixture(method)):
            return
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n) if n else b""
        status, ctype, data = self.server.pms.handle(method, self.path, body, self.headers)
        with self.server.pms.lock:
            self.server.pms.requests.append((self.path, status))
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("X-Plex-Protocol", "1.0")
        self.end_headers()
        if method != "HEAD":
            self.wfile.write(data)

    def do_GET(self):
        self._do("GET")

    def do_HEAD(self):
        self._do("HEAD")

    def do_POST(self):
        self._do("POST")

    def do_PUT(self):
        self._do("PUT")

    def do_DELETE(self):
        self._do("DELETE")


class Server(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def handle_error(self, request, client_address):
        """A client that hangs up mid-body (a player stopping, a process exiting) is ordinary for a
        media server, not an error worth a traceback; anything else still gets one."""
        if isinstance(sys.exc_info()[1], ConnectionError):
            return
        super().handle_error(request, client_address)


HOME_USER_NAMES = ("Demo", "Alex", "Sam", "Kid", "Guest", "Family", "Pat", "Robin")


def home_roster(n):
    """The synthetic Home roster plex.tv's `/api/v2/home/users` answers: the Demo owner first
    (uuid `demo-user`, what the sign-in and `/api/v2/user` name), then `n - 1` more profiles."""
    return [{"id": i + 1, "uuid": "demo-user" if i == 0 else f"demo-user-{i + 1}",
             "title": HOME_USER_NAMES[i % len(HOME_USER_NAMES)] + ("" if i < len(HOME_USER_NAMES) else str(i)),
             "thumb": "", "admin": i == 0, "restricted": i > 0, "protected": False}
            for i in range(n)]


def serve(port, seed=1, host="127.0.0.1", verbose=False, movies=48, rail_fixture=False,
          media=None, extra_media=None, catalog=None, catalog_cache=None, hero=None,
          plaintext_only_lan=False, advertise_ip=None, insecure_fail_mode="handshake",
          authorize_after=None, plex_pass=True, loudness_analysis=True,
          refuse_enhancements=False, ignore_enhancements=False, transcode_fixture=None,
          home_hubs=0, section_hubs=0, section_hubs_linked=0, decision_delay_ms=0,
          home_users=1):
    """Start a mock PMS in a daemon thread; returns (server, pms). Loopback only by default: the
    app on the simulator is on this machine, and a LAN-facing listener would be one more thing
    the outbound guard has to reason about. `catalog` serves the demo library instead of a seed.

    `plaintext_only_lan=True` additionally answers `/api/v2/resources` (PLX-NATIVE-10; see the
    module doc) and opens the failing-HTTPS listener `insecure_fail_mode` names. `advertise_ip`
    picks the LAN address that resources row carries; `None` tries `detect_lan_ip()` and falls
    back to `"127.0.0.1"` with a stderr warning if nothing routes. The chosen address and the
    fail listener's port end up on `pms.plaintext_only_lan`; the fail listener itself (`None` for
    `insecure_fail_mode="unreachable"`) is on `srv.insecure_fail_listener`, for the caller to
    `.close()` alongside `srv.shutdown()`.

    `authorize_after=N` links the QR sign-in's demo code on the Nth pin poll (`None`: never, except
    that `plaintext_only_lan` implies 2 so its reproduction signs in by itself).

    #266: `plex_pass=False`/`loudness_analysis=False` set the starting state of
    `pms.plex_pass`/`Library.loudness_analysis`; `refuse_enhancements`/`ignore_enhancements` set
    the starting state of the same-named `pms.*` flags — all four are still flippable afterwards,
    live, through `POST /_mock/config`. `transcode_fixture` is the file every
    `/video/:/transcode/universal/start*` request serves instead of the plain 404, Range/206
    included; `None` (default) leaves that endpoint unchanged. `decision_delay_ms` sleeps that long
    before answering a transcode decision or start request (`0`, the default, answers at once)."""
    if transcode_fixture is not None and not pathlib.Path(transcode_fixture).is_file():
        raise ValueError(f"--transcode-fixture file is missing: {transcode_fixture}")
    if catalog is not None:
        lib = CatalogLibrary(catalog, cache=catalog_cache, hero=hero,
                             loudness_analysis=loudness_analysis)
    else:
        lib = Library(seed=seed, movies=movies, rail_fixture=rail_fixture, media=media,
                      extra_media=extra_media, loudness_analysis=loudness_analysis)
    pms = MockPms(lib)
    pms.plex_pass = plex_pass
    pms.home_hubs = home_hubs
    pms.section_hubs = section_hubs
    pms.section_hubs_linked = section_hubs_linked
    pms.refuse_enhancements = refuse_enhancements
    pms.ignore_enhancements = ignore_enhancements
    pms.transcode_fixture = pathlib.Path(transcode_fixture) if transcode_fixture else None
    pms.decision_delay_ms = decision_delay_ms
    if authorize_after is None and plaintext_only_lan:
        authorize_after = 2
    pms.authorize_after = authorize_after
    pms.home_users = home_users
    srv = Server((host, port), Handler)
    srv.pms = pms
    srv.verbose = verbose
    srv.insecure_fail_listener = None
    account_ip = advertise_ip or ("127.0.0.1" if host in ("0.0.0.0", "::") else host)
    account_port = srv.server_address[1]
    pms.account_resource = {
        "name": lib.friendly, "clientIdentifier": lib.machine, "provides": "server",
        "owned": True, "home": False, "ownerId": 1, "presence": True,
        "accessToken": "s" + hashlib.sha1(lib.machine.encode()).hexdigest()[:8],
        "httpsRequired": False, "publicAddressMatches": True,
        "connections": [{"protocol": "http", "address": account_ip, "port": account_port,
                         "uri": f"http://{account_ip}:{account_port}", "local": True,
                         "relay": False, "IPv6": False}],
    }
    if plaintext_only_lan:
        ip = advertise_ip
        if ip is None:
            ip = detect_lan_ip()
            if ip is None:
                ip = "127.0.0.1"
                print("mock_pms: WARNING --plaintext-only-lan found no routable LAN IPv4 to "
                      "advertise; falling back to 127.0.0.1, which the app classifies as "
                      "loopback, NOT LAN-eligible — PLX-NATIVE-10's consent path will never "
                      "trigger against this fallback. Pass --advertise-ip with a real LAN "
                      "address.", file=sys.stderr, flush=True)
        fail_port, listener = open_insecure_fail_listener(insecure_fail_mode)
        srv.insecure_fail_listener = listener
        pms.plaintext_only_lan = {
            "ip": ip, "http_port": srv.server_address[1], "fail_port": fail_port,
            "fail_mode": insecure_fail_mode,
            "access_token": "s" + hashlib.sha1(lib.machine.encode()).hexdigest()[:8],
        }
    t = threading.Thread(target=srv.serve_forever, name="mock-pms", daemon=True)
    t.start()
    return srv, pms


# ---------------------------------------------------------------- selftest ----------------

ALPHABET_TOKEN = "s[0-9a-f]{8}"


def selftest():
    import re
    import tempfile
    import urllib.request
    srv, pms = serve(0, seed=7)
    port = srv.server_address[1]
    base = f"http://127.0.0.1:{port}"

    def get(path):
        with urllib.request.urlopen(base + path, timeout=5) as r:
            return r.status, r.headers.get("Content-Type"), r.read()

    def jget(path):
        s, ct, b = get(path)
        assert s == 200 and ct == "application/json", (path, s, ct)
        return json.loads(b)["MediaContainer"]

    resources = json.loads(get("/api/v2/resources")[2])
    assert len(resources) == 1 and resources[0]["clientIdentifier"] == pms.lib.machine
    assert resources[0]["connections"][0]["uri"] == base
    users = json.loads(get("/api/v2/home/users")[2])["users"]
    user = json.loads(get("/api/v2/user")[2])
    assert len(users) == 1 and users[0]["uuid"] == user["uuid"] == "demo-user"
    switch = urllib.request.Request(base + "/api/v2/home/users/demo-user/switch", data=b"", method="POST")
    with urllib.request.urlopen(switch, timeout=5) as reply:
        switched = json.load(reply)
    assert switched["id"] == user["id"] and switched["uuid"] == user["uuid"]
    assert switched["authToken"] == DEMO_ACCOUNT_TOKEN
    pms.home_users = 4
    roster = json.loads(get("/api/v2/home/users")[2])["users"]
    assert [u["uuid"] for u in roster] == ["demo-user", "demo-user-2", "demo-user-3", "demo-user-4"]
    other = urllib.request.Request(base + "/api/v2/home/users/demo-user-3/switch", data=b"", method="POST")
    with urllib.request.urlopen(other, timeout=5) as reply:
        assert json.load(reply)["uuid"] == "demo-user-3"
    pms.home_users = 1

    # Account preferences live only in this mock. Empty-body PUT changes just named keys,
    # including an empty language, and /user serves the same state to the playback cache.
    profile = json.loads(get("/api/v2/user/profile")[2])
    update = urllib.request.Request(base + "/api/v2/user/profile?defaultAudioLanguage=&autoSelectAudio=true&defaultSubtitleForced=3",
                                    data=b"", method="PUT")
    with urllib.request.urlopen(update, timeout=5) as reply:
        changed = json.load(reply)
    assert changed["defaultAudioLanguage"] == "" and changed["autoSelectAudio"] is True
    assert changed["defaultSubtitleForced"] == 3
    assert changed["defaultSubtitleLanguage"] == profile["defaultSubtitleLanguage"]
    assert json.loads(get("/api/v2/user")[2])["profile"] == changed
    assert pms.writes[-1][0] == "PUT" and pms.writes[-1][2] == b""

    secs = jget("/library/sections")["Directory"]
    assert [s["key"] for s in secs] == ["1", "2"]
    page = jget("/library/sections/1/all?includeMeta=1&sort=titleSort:asc&X-Plex-Container-Start=0&X-Plex-Container-Size=10")
    assert page["size"] == 10 and page["totalSize"] == 48 and page["Meta"]["Type"][0]["Sort"]
    titles = [m["titleSort"] for m in page["Metadata"]]
    assert titles == sorted(titles), "titleSort:asc really sorts"
    rk = page["Metadata"][0]["ratingKey"]
    item = jget(f"/library/metadata/{rk}?includeChapters=1&includeMarkers=1")["Metadata"][0]
    assert item["Media"][0]["Part"][0]["key"].startswith("/library/parts/")
    assert item["Chapter"] and item["Marker"][0]["final"] is True
    shows = jget("/library/sections/2/all")["Metadata"]
    seasons = jget(f"/library/metadata/{shows[0]['ratingKey']}/children")["Metadata"]
    eps = jget(f"/library/metadata/{seasons[0]['ratingKey']}/children")["Metadata"]
    assert len(seasons) == 2 and len(eps) == 6 and eps[0]["grandparentTitle"] == shows[0]["title"]
    assert len(jget(f"/library/metadata/{shows[0]['ratingKey']}/allLeaves")["Metadata"]) == 12
    hubs = jget("/hubs?count=12&excludeContinueWatching=1")["Hub"]
    assert [h["hubIdentifier"] for h in hubs][0] == "home.continue" and hubs[0]["Metadata"] == []
    cw = jget("/hubs/continueWatching?count=12")["Hub"][0]
    assert cw["Metadata"] and all(m["viewOffset"] > 0 for m in cw["Metadata"])
    sh = jget("/hubs/sections/1?count=12")["Hub"]
    assert sh[0]["hubIdentifier"] == "movie.inprogress.1"
    pid = item["Role"][0]["id"]
    assert jget(f"/library/people/{pid}/media")["Metadata"]
    res = jget(f"/hubs/search?query={item['title'][:3]}&limit=8")["Hub"]
    assert any(h["type"] == "movie" and h["Metadata"] for h in res)
    s, ct, b = get(f"/photo/:/transcode?width=250&height=375&minSize=1&url=%2Flibrary%2Fmetadata%2F{rk}%2Fthumb%2F1")
    assert s == 200 and ct == "image/png" and b[:8] == b"\x89PNG\r\n\x1a\n"
    # watch state round trip
    jget(f"/:/scrobble?key={rk}&identifier=com.plexapp.plugins.library")
    assert jget(f"/library/metadata/{rk}")["Metadata"][0]["viewCount"] == 1
    jget(f"/:/unscrobble?key={rk}&identifier=com.plexapp.plugins.library")
    assert "viewCount" not in jget(f"/library/metadata/{rk}")["Metadata"][0]
    # subtitle search & download: 3-letter -> 500, unknown language -> empty, a PUT installs a
    # NEW selected external stream whose sidecar is fetchable, and a candidate key is single-use
    def send(path, method="GET"):
        req = urllib.request.Request(base + path, data=b"" if method == "PUT" else None, method=method)
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                return r.status, r.read()
        except urllib.error.HTTPError as e:
            return e.code, e.read()
    assert send(f"/library/metadata/{rk}/subtitles?language=nld&hearingImpaired=0&forced=0")[0] == 500
    assert jget(f"/library/metadata/{rk}/subtitles?language=fi&hearingImpaired=0&forced=0").get("Stream") == []
    found = jget(f"/library/metadata/{rk}/subtitles?language=nl&hearingImpaired=0&forced=0")["Stream"]
    assert len(found) == 2 and isinstance(found[0]["score"], str) and found[0]["languageCode"] == "nld"
    before = {s["id"] for s in jget(f"/library/metadata/{rk}")["Metadata"][0]["Media"][0]["Part"][0]["Stream"]}
    assert send(f"/library/metadata/{rk}/subtitles?key={found[0]['key']}", "PUT") == (200, b"")
    assert send(f"/library/metadata/{rk}/subtitles?key={found[0]['key']}", "PUT") == (200, b"")
    after = jget(f"/library/metadata/{rk}")["Metadata"][0]["Media"][0]["Part"][0]["Stream"]
    added = [s for s in after if s["id"] not in before]
    assert len(added) == 1 and added[0]["selected"] and added[0]["codec"] == "srt", added
    assert added[0]["id"] != found[0]["id"], "an install is a NEW stream, not the candidate's id"
    s, _, b = get(added[0]["key"])
    assert s == 200 and found[0]["title"].encode() in b
    assert any(m == "PUT" and "/subtitles?" in w for m, w, _ in pms.writes)
    # PMS discards a DOWNLOADED subtitle the moment the part's selection moves off it (measured
    # 2026-10-06, docs/pms-api.md): its stream then 404s and selecting it again is a 400.
    part_id = jget(f"/library/metadata/{rk}")["Metadata"][0]["Media"][0]["Part"][0]["id"]
    assert send(f"/library/parts/{part_id}?allParts=1&subtitleStreamID={added[0]['id']}", "PUT")[0] == 200
    assert send(added[0]["key"])[0] == 200, "re-selecting the same stream keeps it"
    assert send(f"/library/parts/{part_id}?allParts=1&subtitleStreamID=0", "PUT")[0] == 200
    assert send(added[0]["key"])[0] == 404, "deselected -> the downloaded stream is gone"
    assert send(f"/library/parts/{part_id}?allParts=1&subtitleStreamID={added[0]['id']}", "PUT")[0] == 400
    gone = {s["id"] for s in jget(f"/library/metadata/{rk}")["Metadata"][0]["Media"][0]["Part"][0]["Stream"]}
    assert added[0]["id"] not in gone and before <= gone
    # ... and so is moving it onto an EMBEDDED stream (the second measured sequence, 2026-10-06: a
    # playback start that re-picked the embedded language match instead of keeping the download)
    again = jget(f"/library/metadata/{rk}/subtitles?language=nl&hearingImpaired=0&forced=0")["Stream"]
    assert send(f"/library/metadata/{rk}/subtitles?key={again[1]['key']}", "PUT") == (200, b"")
    streams = jget(f"/library/metadata/{rk}")["Metadata"][0]["Media"][0]["Part"][0]["Stream"]
    second = [x for x in streams if x["id"] not in gone and x["streamType"] == 3]
    embedded = [x for x in streams if x["streamType"] == 3 and not x.get("key") and x["id"] in gone]
    assert len(second) == 1 and second[0]["selected"], streams
    if embedded:
        assert send(f"/library/parts/{part_id}?allParts=1&subtitleStreamID={embedded[0]['id']}", "PUT")[0] == 200
        assert send(second[0]["key"])[0] == 404, "moved onto an embedded track -> the download is gone"
    # the closed alphabet: every title-shaped string obeys it
    tok = re.compile(f"^{ALPHABET_TOKEN}$")
    for it in pms.lib.items.values():
        for k in ("title", "titleSort", "studio"):
            if it.get(k):
                assert tok.match(it[k]), (k, it[k])
        for w in it["summary"].split():
            assert tok.match(w), w
        for t in it.get("Role", []):
            assert tok.match(t["tag"]) and tok.match(t["role"]) and tok.match(t["tagKey"])
    # determinism: two FRESH servers with one seed answer byte-identically (the first server above
    # has been scrobbled, so it is compared to nothing — a write is supposed to change its answer)
    srv2, pms2 = serve(0, seed=7)
    srv4, _ = serve(0, seed=7)
    p2 = srv2.server_address[1]
    with urllib.request.urlopen(f"http://127.0.0.1:{p2}/library/sections/1/all", timeout=5) as r:
        b2 = r.read()
    with urllib.request.urlopen(f"http://127.0.0.1:{srv4.server_address[1]}/library/sections/1/all", timeout=5) as r:
        assert r.read() == b2
    # a different seed is a different library
    srv3, _ = serve(0, seed=8)
    with urllib.request.urlopen(f"http://127.0.0.1:{srv3.server_address[1]}/library/sections/1/all", timeout=5) as r:
        assert r.read() != b2

    # Opt-in verification surface. Tiny valid files keep this endpoint test quick; the fixture
    # generator separately proves its shipping 150-second layouts.
    tmp = tempfile.TemporaryDirectory()
    media = pathlib.Path(tmp.name)
    cue = "1\n00:00:00,000 --> 00:00:01,000\nSIDECAR SELFTEST\n"
    (media / "v1.srt").write_text(cue)
    (media / "mockverify-v2.eng.srt").write_text(cue)
    (media / "v2.srt").write_text(cue)
    base_ff = ["ffmpeg", "-y", "-v", "error", "-f", "lavfi", "-i",
               "color=size=64x64:rate=24:duration=2"]
    subprocess.check_call(base_ff + [
        "-f", "lavfi", "-i", "sine=frequency=330:duration=2", "-f", "lavfi", "-i",
        "sine=frequency=220:duration=2", "-i", str(media / "v1.srt"), "-map", "0:v",
        "-map", "1:a", "-map", "2:a", "-map", "3:s", "-c:v", "libx264", "-c:a", "ac3",
        "-c:s", "srt", "-metadata:s:a:0", "language=deu", "-disposition:a:0", "0",
        "-metadata:s:a:1", "language=eng", "-disposition:a:1", "default",
        "-metadata:s:s:0", "language=eng", "-disposition:s:0", "0", "-t", "2",
        str(media / "mockverify-v1.mkv")])
    subprocess.check_call(base_ff + [
        "-f", "lavfi", "-i", "sine=frequency=262:duration=2", "-i", str(media / "v2.srt"),
        "-map", "0:v", "-map", "1:a", "-map", "2:s", "-c:v", "libx264", "-c:a", "aac",
        "-c:s", "srt", "-metadata:s:a:0", "language=eng", "-disposition:a:0", "default",
        "-metadata:s:s:0", "language=eng", "-disposition:s:0", "0", "-t", "2",
        str(media / "mockverify-v2.mkv")])
    media_srv, media_pms = serve(0, seed=7, media=media)
    media_base = f"http://127.0.0.1:{media_srv.server_address[1]}"
    prefs = json.load(urllib.request.urlopen(
        media_base + f"/library/metadata/{VERIFY_SHOW_RK}?includePreferences=1"))
    assert prefs["MediaContainer"]["Metadata"][0]["Preferences"]["Setting"] == VERIFY_PREFS
    tree = json.load(urllib.request.urlopen(
        media_base + f"/library/metadata/{VERIFY_SHOW_RK}/tree"))
    assert tree["MediaContainer"]["Setting"] == VERIFY_PREFS
    part = f"/library/parts/{VERIFY_PARTS[V1_RATING_KEY]}/1/file.mkv"
    request = urllib.request.Request(media_base + part, headers={"Range": "bytes=1-3"})
    with urllib.request.urlopen(request) as response:
        assert response.status == 206 and response.read() == (media / "mockverify-v1.mkv").read_bytes()[1:4]
        assert response.headers["Accept-Ranges"] == "bytes"
        assert response.headers["Content-Range"].startswith("bytes 1-3/")
    request = urllib.request.Request(media_base + part, method="HEAD")
    with urllib.request.urlopen(request) as response:
        assert response.status == 200 and response.headers["Accept-Ranges"] == "bytes"
    for suffix in ("?encoding=utf-8&format=srt", "?encoding=utf-8", ""):
        with urllib.request.urlopen(
                media_base + f"/library/streams/{VERIFY_SIDECAR_ID}{suffix}") as response:
            assert response.read().startswith(b"1\n00:00:00")
    put = urllib.request.Request(
        media_base + f"/library/parts/{VERIFY_PARTS[V2_RATING_KEY]}?allParts=1&subtitleStreamID=0",
        method="PUT")
    urllib.request.urlopen(put).read()
    assert media_pms.writes and media_pms.writes[-1][0] == "PUT"
    assert not pms.unknown, pms.unknown
    assert not media_pms.unknown, media_pms.unknown
    for s_ in (srv, srv2, srv3, srv4, media_srv):
        s_.shutdown()
        s_.server_close()
    tmp.cleanup()

    _selftest_plaintext_only_lan()
    _selftest_section_hubs()
    print("mock_pms selftest: ok")


def _teardown(srv):
    srv.shutdown()
    srv.server_close()
    if srv.insecure_fail_listener is not None:
        srv.insecure_fail_listener.close()


def _selftest_section_hubs():
    """#412: `--section-hubs N` pads `/hubs/sections/1` to exactly N hubs, `--section-hubs-linked M`
    makes M of them promoted collections, and each hub's own key lists its items."""
    import urllib.request

    srv, _ = serve(0, seed=7, section_hubs=20, section_hubs_linked=5)
    try:
        base = f"http://127.0.0.1:{srv.server_address[1]}"

        def jget(path):
            with urllib.request.urlopen(base + path, timeout=5) as r:
                return json.loads(r.read())["MediaContainer"]

        hubs = jget("/hubs/sections/1")["Hub"]
        assert len(hubs) == 20, len(hubs)
        linked = [h for h in hubs if h["hubIdentifier"].startswith("custom.collection.1.")]
        assert len(linked) == 5, len(linked)
        assert len({h["hubIdentifier"] for h in hubs}) == 20
        for h in hubs[2:]:
            assert h["Metadata"], h["hubIdentifier"]
            assert jget(h["key"])["Metadata"], h["key"]
    finally:
        srv.shutdown()
        srv.server_close()


def _selftest_plaintext_only_lan():
    """PLX-NATIVE-10: exactly two connections and no relay, the https one really fails a TLS
    handshake (or is really unreachable, in the other fail mode), and the http one really answers
    `/identity` tokenless with the resource's own machineIdentifier. `advertise_ip="127.0.0.1"` is
    explicit here, so this is hermetic on a sandboxed CI runner with no real LAN route — it is
    testing the mock's wire shapes, not the app's loopback-ineligibility rule."""
    import ssl

    srv, pms = serve(0, seed=7, plaintext_only_lan=True, advertise_ip="127.0.0.1")
    try:
        cfg = pms.plaintext_only_lan
        http_port = srv.server_address[1]
        assert cfg["ip"] == "127.0.0.1" and cfg["http_port"] == http_port

        with urllib.request.urlopen(f"http://127.0.0.1:{http_port}/api/v2/resources", timeout=5) as r:
            resources = json.loads(r.read())
        assert isinstance(resources, list) and len(resources) == 1, resources
        res = resources[0]
        assert res["provides"] == "server" and res["owned"] is True
        assert res["clientIdentifier"] == pms.lib.machine
        assert res["httpsRequired"] is False and res["publicAddressMatches"] is True
        conns = res["connections"]
        assert len(conns) == 2, conns
        assert not any(c["relay"] for c in conns), "no relay connection at all"
        https = next(c for c in conns if c["protocol"] == "https")
        http = next(c for c in conns if c["protocol"] == "http")
        assert https["local"] is True and http["local"] is True
        assert https["uri"] == f"https://127-0-0-1.{PLEX_DIRECT_HASH}.plex.direct:{cfg['fail_port']}"
        assert https["address"] == "127.0.0.1" and https["port"] == cfg["fail_port"]
        assert http["address"] == "127.0.0.1" and http["port"] == http_port
        assert http["uri"] == f"http://127.0.0.1:{http_port}"

        # the https route really fails a TLS handshake: a real ClientHello against a listener
        # that accepts and immediately closes, never a guess from the JSON alone.
        raised = None
        try:
            raw = socket.create_connection(("127.0.0.1", cfg["fail_port"]), timeout=5)
            try:
                ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
                ctx.check_hostname = False
                ctx.verify_mode = ssl.CERT_NONE
                ctx.wrap_socket(raw, server_hostname="127.0.0.1").close()
            finally:
                raw.close()
        except (ssl.SSLError, OSError) as e:
            raised = e
        assert raised is not None, "the advertised https route must fail a real TLS handshake"

        # the plaintext local connection answers /identity tokenless.
        with urllib.request.urlopen(f"http://127.0.0.1:{http_port}/identity", timeout=5) as r:
            identity = json.loads(r.read())["MediaContainer"]
        assert identity["machineIdentifier"] == pms.lib.machine

        # the mode signs in by itself: the demo code links on the 2nd poll, with the synthetic
        # account token (T2 — the reproduction needs no real account).
        def poll():
            with urllib.request.urlopen(f"http://127.0.0.1:{http_port}/api/v2/pins/{DEMO_PIN_ID}",
                                        timeout=5) as r:
                return json.loads(r.read())["authToken"]
        assert poll() is None, "the first poll is still pending"
        assert poll() == DEMO_ACCOUNT_TOKEN
        assert re.fullmatch(ALPHABET_TOKEN, DEMO_ACCOUNT_TOKEN)
    finally:
        _teardown(srv)

    # the selectable "unreachable" fail mode: nothing is listening on the advertised port at all.
    srv2, pms2 = serve(0, seed=7, plaintext_only_lan=True, advertise_ip="127.0.0.1",
                       insecure_fail_mode="unreachable")
    try:
        assert srv2.insecure_fail_listener is None
        fail_port = pms2.plaintext_only_lan["fail_port"]
        try:
            socket.create_connection(("127.0.0.1", fail_port), timeout=5).close()
            raised = False
        except OSError:
            raised = True
        assert raised, "an unreachable fail port must refuse or time out, never accept"
    finally:
        _teardown(srv2)


def main():
    # The PLX-NATIVE-10 reproduction (module doc) is the help's epilog, so `--help` carries it.
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0],
                                 epilog=__doc__[__doc__.index("`--plaintext-only-lan` reproduces"):],
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", type=int, default=32499)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--movies", type=int, default=48, help="movie count, 0–1000 (keys must not overlap shows)")
    ap.add_argument("--rail-fixture", action="store_true", help="synthetic A/C/F/M/Z sort-title groups for rail tests")
    ap.add_argument("--media", type=pathlib.Path,
                    help="opt-in mockverify directory; ffprobe derives its stream metadata")
    ap.add_argument("--extra-media", type=pathlib.Path, action="append", default=[],
                    help="opt-in arbitrary media file (repeatable); each becomes one movie, "
                         "ratingKey assigned from EXTRA_MEDIA_RK_BASE and printed at startup")
    ap.add_argument("--catalog", type=pathlib.Path,
                    help="serve the demo library (tests/demo_library/catalog.json) instead of a seed")
    ap.add_argument("--catalog-cache", type=pathlib.Path,
                    help="the demo library cache (default $PLXNATIVE_DEMO_CACHE or ~/.cache/plxnative-demo)")
    ap.add_argument("--home-hubs", type=int, default=0, metavar="N",
                    help="#395: make /hubs answer exactly N hubs (synthetic 'Mock Shelf i' rows pad "
                         "the library's own); 0 (default) leaves them as they are")
    ap.add_argument("--section-hubs", type=int, default=0, metavar="N",
                    help="#412: make /hubs/sections/<id> answer exactly N hubs (synthetic 'Mock Shelf i' "
                         "rows pad the library's own); 0 (default) leaves them as they are")
    ap.add_argument("--section-hubs-linked", type=int, default=0, metavar="M",
                    help="with --section-hubs: make M of the padding hubs promoted collections "
                         "(custom.collection.<section>.<rk>.<n>), whose headings link to the collection")
    ap.add_argument("--hero", help="with --catalog: the film at the head of Continue Watching (the hero)")
    ap.add_argument("--plaintext-only-lan", action="store_true",
                    help="PLX-NATIVE-10: /api/v2/resources answers with ONE owned server, no "
                         "relay, whose only HTTPS route (a plex.direct connection) fails before "
                         "the app's own logic runs, and whose plaintext http://<advertise-ip>:"
                         "<port> connection is this same server, answering /identity tokenless. "
                         "TV: `python3 tests/mock_pms.py --host 0.0.0.0 --plaintext-only-lan "
                         "--advertise-ip 192.168.0.10`, then point the device's plex.tv trigger "
                         "at http://192.168.0.10:<port>. Simulator: `python3 tests/mock_pms.py "
                         "--plaintext-only-lan --advertise-ip 127.0.0.1`. Expect the app to offer "
                         "\"Connect without encryption?\", never a dead end. The full reproduction "
                         "(store policy trigger, QR sign-in, expected log lines) follows below.")
    ap.add_argument("--advertise-ip",
                    help="with --plaintext-only-lan: the LAN IPv4 the resources row advertises; "
                         "default: this host's detected primary LAN IPv4, else 127.0.0.1 WITH A "
                         "STARTUP WARNING — 127.0.0.1 is loopback, which the app classifies as "
                         "NOT LAN-eligible, so the consent path never triggers against that "
                         "fallback")
    ap.add_argument("--insecure-fail-mode", choices=("handshake", "unreachable"), default="handshake",
                    help="with --plaintext-only-lan: how the advertised HTTPS route fails — "
                         "'handshake' (default) accepts and immediately closes, a real TLS "
                         "handshake failure; 'unreachable' advertises a port nothing listens on")
    ap.add_argument("--authorize-after", type=int, metavar="N",
                    help="link the QR sign-in's demo code on the Nth pin poll with a synthetic "
                         "account token (default: never; --plaintext-only-lan implies 2)")
    ap.add_argument("--home-users", type=int, default=1, metavar="N",
                    help="how many profiles the plex.tv Home roster lists, 1-16 (default 1: the "
                         "Demo owner only; more make a stored-session boot open the picker)")
    ap.add_argument("--no-plex-pass", action="store_true",
                    help="#266: / and /identity answer myPlexSubscription=false (the version "
                         "string is untouched — tools/mock-guest.py depends on it)")
    ap.add_argument("--no-loudness-analysis", action="store_true",
                    help="#266: omit canNormalizeLoudness from every audio stream — by default "
                         "it is sent as \"1\" (PMS string-encodes it), modelling a server that "
                         "never analysed the source for loudness")
    ap.add_argument("--refuse-enhancements", action="store_true",
                    help="#266: a live boostDialog=1/normalizeLoudness=1 on /decision gets the "
                         "server-refusal shape (generalDecisionCode=2000) instead of a transcode "
                         "decision; also flippable live via POST /_mock/config")
    ap.add_argument("--ignore-enhancements", action="store_true",
                    help="#266: a live boostDialog=1/normalizeLoudness=1 is accepted but the "
                         "audio decision stays \"copy\" — an old server that does not act on the "
                         "params; also flippable live via POST /_mock/config")
    ap.add_argument("--transcode-fixture", type=pathlib.Path,
                    help="#266: serve this file (Range/206 supported) for every "
                         "/video/:/transcode/universal/start* request instead of the plain 404")
    ap.add_argument("--decision-delay-ms", type=int, default=0, metavar="N",
                    help="sleep N ms before answering a /video/:/transcode/universal/decision or "
                         "start.* request, so a route flight is observably in flight (default 0)")
    ap.add_argument("--verbose", action="store_true")
    ap.add_argument("--selftest", action="store_true")
    a = ap.parse_args()
    if a.hero and a.catalog is None:
        ap.error("--hero needs --catalog")
    if a.home_hubs < 0:
        ap.error("--home-hubs must not be negative")
    if a.section_hubs < 0 or a.section_hubs_linked < 0:
        ap.error("--section-hubs and --section-hubs-linked must not be negative")
    if a.section_hubs_linked > a.section_hubs:
        ap.error("--section-hubs-linked must not exceed --section-hubs")
    if a.decision_delay_ms < 0:
        ap.error("--decision-delay-ms must not be negative")
    if not 0 <= a.movies <= 1000:
        ap.error("--movies must be between 0 and 1000")
    if (a.advertise_ip or a.insecure_fail_mode != "handshake") and not a.plaintext_only_lan:
        ap.error("--advertise-ip/--insecure-fail-mode need --plaintext-only-lan")
    if not 1 <= a.home_users <= 16:
        ap.error("--home-users must be between 1 and 16")
    if a.authorize_after is not None and a.authorize_after < 1:
        ap.error("--authorize-after must be at least 1")
    if a.selftest:
        selftest()
        return
    try:
        srv, pms = serve(a.port, seed=a.seed, host=a.host, verbose=a.verbose,
                         movies=a.movies, rail_fixture=a.rail_fixture, media=a.media,
                         extra_media=a.extra_media, catalog=a.catalog, catalog_cache=a.catalog_cache,
                         hero=a.hero, plaintext_only_lan=a.plaintext_only_lan,
                         advertise_ip=a.advertise_ip, insecure_fail_mode=a.insecure_fail_mode,
                         authorize_after=a.authorize_after, plex_pass=not a.no_plex_pass,
                         loudness_analysis=not a.no_loudness_analysis,
                         refuse_enhancements=a.refuse_enhancements,
                         ignore_enhancements=a.ignore_enhancements,
                         transcode_fixture=a.transcode_fixture, home_hubs=a.home_hubs,
                         section_hubs=a.section_hubs,
                         section_hubs_linked=a.section_hubs_linked,
                         decision_delay_ms=a.decision_delay_ms, home_users=a.home_users)
    except ValueError as e:
        ap.error(str(e))
    what = f"catalog={a.catalog}" if a.catalog else f"seed={a.seed}"
    print(f"mock_pms: serving {what} on http://{a.host}:{srv.server_address[1]}", flush=True)
    if a.media is not None:
        trigger = {"name": pms.lib.friendly, "machine_id": pms.lib.machine,
                   "host": "<TV-REACHABLE-HOST>", "port": srv.server_address[1],
                   "token": "mock-pms", "v1_rating_key": str(V1_RATING_KEY),
                   "v2_rating_key": str(V2_RATING_KEY)}
        print(json.dumps(trigger, separators=(",", ":")), flush=True)
    if a.plaintext_only_lan:
        cfg = pms.plaintext_only_lan
        print(f"mock_pms: plaintext-only-lan resources: machine={pms.lib.machine} "
              f"https(fails, {cfg['fail_mode']})=https://<dashed-ip>.{PLEX_DIRECT_HASH}"
              f".plex.direct:{cfg['fail_port']} http(answers)=http://{cfg['ip']}:{cfg['http_port']}",
              flush=True)
    for rk, path in sorted(getattr(pms.lib, "extra_media_files", {}).items()):
        print(f"mock_pms: extra-media rk={rk} file={path}", flush=True)
    try:
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        pass
    finally:
        srv.shutdown()
        if srv.insecure_fail_listener is not None:
            srv.insecure_fail_listener.close()
        if pms.unknown:
            print("mock_pms: unknown paths seen: " + ", ".join(sorted(set(pms.unknown))), file=sys.stderr)


if __name__ == "__main__":
    main()
