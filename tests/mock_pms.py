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
without touching plex.tv.

The app reaches it as any other server: `make sim-shot SIM_PMS=127.0.0.1 SIM_PORT=32499` with
any non-empty string in `$SIM_DIR/plxnative-token` (the token is accepted, never checked).
"""
import argparse
import hashlib
import json
import os
import pathlib
import random
import re
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
# --extra-media ids. Generated movies are 1001..(1000+movies<=1000) => max 2000; shows/seasons/
# episodes nest as rk*10+n from 2001, so a default-sized run never reaches six digits; VERIFY_*
# stops at 901009. 990001+ sits clear of all three, with a wide gap before the next round number
# so a deliberately huge --shows run remains this module's problem to notice, not to silently hit.
EXTRA_MEDIA_RK_BASE = 990001
EXTRA_MEDIA_PART_ID_BASE = 991001
VERIFY_PREFS = [
    {"id": "audioLanguage", "value": "de"},
    {"id": "subtitleLanguage", "value": "en"},
    {"id": "subtitleMode", "value": "2"},
]

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
                 media=None, extra_media=None):
        # Movie keys start at 1001; shows start at 2001. Refuse a fixture that would silently
        # overwrite a film with a show instead of exercising the requested listing size.
        if not 0 <= movies <= 1000:
            raise ValueError("movies must be between 0 and 1000")
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
            self.collections[i] = {"id": i, "tag": sname(rng)}
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

    @staticmethod
    def _stream_wire(stream, sid):
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

    def _media(self, rng, rk, part, duration):
        return [{
            "id": rk, "duration": duration, "bitrate": 8000, "width": 1920, "height": 1080,
            "aspectRatio": 1.78, "audioChannels": 6, "audioCodec": "ac3", "videoCodec": "h264",
            "videoResolution": "1080", "container": "mkv", "videoFrameRate": "24p",
            "videoProfile": "high",
            "Part": [{
                "id": part, "key": f"/library/parts/{part}/{1_700_000_000 + part}/file.mkv",
                "duration": duration, "file": f"/{sname(rng)}/{sname(rng)}.mkv",
                "size": 4_000_000_000, "container": "mkv", "videoProfile": "high",
                "Stream": [
                    {"id": part * 10 + 1, "streamType": 1, "codec": "h264", "index": 0,
                     "width": 1920, "height": 1080, "displayTitle": "1080p (H.264)"},
                    {"id": part * 10 + 2, "streamType": 2, "codec": "ac3", "index": 1,
                     "channels": 6, "language": "en", "languageCode": "eng",
                     "displayTitle": "English (AC3 5.1)", "selected": True},
                    {"id": part * 10 + 3, "streamType": 3, "codec": "srt", "index": 2,
                     "language": "en", "languageCode": "eng", "displayTitle": "English (SRT)"},
                ],
            }],
        }]

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

    def first_characters(self, key):
        """Counts in the unfiltered ascending titleSort order, exactly the rail's query."""
        counts = {}
        for item in self.section_items(key, {}):
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

    def search(self, query, limit=3):
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
        the other hubs hold direct hits only."""
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
        cols = [{"tag": c["tag"], "id": c["id"], "type": "collection", "librarySectionID": 1,
                 "key": f"/library/sections/1/all?collection={c['id']}", "reasonTitle": ""}
                for c in self.collections.values() if hits(c["tag"])][:limit]
        hubs.append({"title": "collection", "type": "collection", "hubIdentifier": "collection",
                     "size": len(cols), "Directory": cols})
        return hubs


# ---------------------------------------------------------------- the demo catalog --------

DEMO_PIN_ID = 1790000001
DEMO_PIN_CODE = "DEMO"
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

    def __init__(self, catalog_path, cache=None, hero=None):
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
            self.collections[n] = {"id": n, "tag": name}
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

    def collection_rows(self, cid):
        """A collection's members (Home's shelf and the library's alike): in the catalog's
        `collection_order` for it when it has one — a real server's custom collection order —
        otherwise newest addition first."""
        rows = [it for it in self.items.values()
                if any(c["id"] == cid for c in it.get("Collection", []))]
        rows.sort(key=lambda it: -it["addedAt"])
        order = self.catalog.get("collection_order", {}).get(self.collections[cid]["tag"])
        if order:
            rank = {self.by_slug[slug]: n for n, slug in enumerate(order)}
            rows.sort(key=lambda it: rank[int(it["ratingKey"])])
        return rows

    def section_collection_hubs(self, section, kind):
        """The catalog's collections in one library, as the shelves a real server lists after
        Recently Added: `custom.collection.<section>.<id>.<id>`, in the catalog's order."""
        hubs = []
        for c in self.collections.values():
            rows = [it for it in self.collection_rows(c["id"]) if it["librarySectionID"] == section]
            if rows:
                hubs.append({"title": c["tag"], "type": kind, "size": len(rows),
                             "hubIdentifier": f"custom.collection.{section}.{c['id']}.{c['id']}",
                             "key": f"/library/collections/{c['id']}/children", "Metadata": rows[:12]})
        return hubs

    def home_hubs(self):
        """`/hubs` after Continue Watching: the catalog's shelves, in its order."""
        out = []
        for h in self.catalog["hubs"]:
            if "recent" in h:
                rows = self.recent(h["recent"], 12)
            else:
                rows = self.collection_rows(
                    next(c["id"] for c in self.collections.values() if c["tag"] == h["collection"]))
            out.append({"title": h["title"], "type": h["type"], "hubIdentifier": h["hubIdentifier"],
                        "key": f"/hubs/demo/{h['hubIdentifier']}", "Metadata": rows})
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


# ---------------------------------------------------------------- the server ----------------

class MockPms:
    def __init__(self, lib):
        self.lib = lib
        self.lock = threading.Lock()
        self.requests = []  # (path, status) in arrival order, for the harness
        self.unknown = []
        self.writes = []

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

    # --- containers ------------------------------------------------------------------------

    def container(self, **kw):
        mc = {"size": 0, "allowSync": False, "identifier": "com.plexapp.plugins.library",
              "mediaTagPrefix": "/system/bundle/media/flags/", "mediaTagVersion": 1}
        mc.update(kw)
        for k in ("Metadata", "Directory", "Hub"):
            if k in mc:
                mc["size"] = len(mc[k])
        return {"MediaContainer": mc}

    def sort_meta(self):
        return {"Type": [{"key": "/library/sections/1/all?type=1", "type": "movie", "title": "movie",
                          "active": True,
                          "Sort": [{"key": "titleSort", "defaultDirection": "asc", "title": "Title"},
                                   {"key": "addedAt", "defaultDirection": "desc", "title": "Date Added"},
                                   {"key": "originallyAvailableAt", "defaultDirection": "desc", "title": "Release Date"},
                                   {"key": "rating", "defaultDirection": "desc", "title": "Critic Rating"},
                                   {"key": "lastViewedAt", "defaultDirection": "desc", "title": "Date Viewed"},
                                   {"key": "random", "defaultDirection": "asc", "title": "Randomly"}]}]}

    def handle(self, method, path, body=b""):
        """Returns (status, content_type, bytes)."""
        u = urllib.parse.urlsplit(path)
        p = u.path
        q = {k: v[-1] for k, v in urllib.parse.parse_qs(u.query, keep_blank_values=True).items()}
        lib = self.lib
        j = lambda obj, status=200: (status, "application/json", json.dumps(obj).encode())
        segs = [s for s in p.split("/") if s]

        write_path = (p in ("/:/timeline", "/:/scrobble", "/:/unscrobble", "/:/progress",
                            "/actions/removeFromContinueWatching", "/status/sessions/close",
                            "/video/:/transcode/universal/stop", "/playQueues")
                      or p.startswith("/library/parts/")
                      or (segs[:1] == ["playQueues"] and len(segs) == 2))
        if write_path and not (method in ("GET", "HEAD") and p.startswith("/library/parts/")):
            self.note_write(method, path, body)

        # plex.tv's QR sign-in, for `plxnative-plextv` (see the module doc). Never linked: the poll
        # stays pending, which is the state the sign-in screen is captured in.
        if p == "/api/v2/pins" and method == "POST":
            return j({"id": DEMO_PIN_ID, "code": DEMO_PIN_CODE, "expiresIn": 1800, "authToken": None,
                      "qr": ""}, 201)
        if p == f"/api/v2/pins/{DEMO_PIN_ID}":
            return j({"id": DEMO_PIN_ID, "code": DEMO_PIN_CODE, "expiresIn": 1800, "authToken": None})
        if p == f"/api/v2/pins/qr/{DEMO_PIN_CODE}":
            import demo_library.qr as qr
            return (200, "image/png", qr.png(qr.encode(DEMO_QR_TEXT, "M"), scale=8, border=0, plex_style=True))
        if p == "/api/v2/user":
            return j({"id": 1, "uuid": "demo-user", "username": "demo", "title": "Demo",
                      "friendlyName": "Demo", "email": "demo@example.invalid", "thumb": "",
                      "subscription": {"active": True, "status": "Active", "plan": "lifetime"}})
        catalog = isinstance(lib, CatalogLibrary)
        if p == "/" or p == "/identity":
            return j(self.container(machineIdentifier=lib.machine, friendlyName=lib.friendly,
                                    version="1.41.0.0000-synthetic", myPlexSubscription=True,
                                    platform="Linux", myPlex=True))
        if p == "/library/sections":
            return j(self.container(Directory=[dict(s, agent="tv.plex.agents.movie",
                                                    scanner="Plex Movie", language="en-US",
                                                    art="/:/resources/movie-fanart.jpg",
                                                    composite=f"/library/sections/{s['key']}/composite/1",
                                                    refreshing=False, allowSync=False)
                                               for s in lib.sections]))
        if len(segs) == 4 and segs[:2] == ["library", "sections"] and segs[3] == "all":
            rows = lib.section_items(segs[2], q)
            start = int(q.get("X-Plex-Container-Start", 0))
            size = int(q.get("X-Plex-Container-Size", len(rows)))
            page = rows[start:start + size]
            extra = {"totalSize": len(rows), "offset": start}
            if q.get("includeMeta") == "1":
                extra["Meta"] = self.sort_meta()
            return j(self.container(Metadata=page, **extra))
        if len(segs) == 4 and segs[:2] == ["library", "sections"]:
            d = segs[3]
            if d == "genre":
                return j(self.container(Directory=[{"key": str(g["id"]), "title": g["tag"],
                                                    "fastKey": f"/library/sections/{segs[2]}/all?genre={g['id']}"}
                                                   for g in lib.genres.values()]))
            if d == "firstCharacter":
                return j(self.container(Directory=lib.first_characters(segs[2])))
            return j(self.container(Directory=[]))
        if len(segs) >= 3 and segs[:2] == ["library", "metadata"]:
            ids = segs[2]
            if len(segs) == 3:
                rows = [lib.items[int(x)] for x in ids.split(",") if x.isdigit() and int(x) in lib.items]
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
            if sub == "related":
                it = lib.items.get(rk)
                pool = [x for x in lib.items.values() if it and x["type"] == it["type"] and x is not it]
                return j(self.container(Hub=[{"title": "related", "type": it["type"] if it else "movie",
                                              "hubIdentifier": "related", "size": min(8, len(pool)),
                                              "Metadata": pool[:8]}]))
            if catalog:
                img = lib.image(p, q.get("width"), q.get("height"))
                return (200, *img) if img else (404, "text/plain", b"no image")
            if sub in ("thumb", "art"):
                png = flat_png(q.get("width", 250), q.get("height", 375), colour_for(p))
                return (200, "image/png", png)
            return j(self.container())
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
            for h in hubs:
                h["size"] = len(h["Metadata"])
            return j(self.container(Hub=hubs))
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
            return j(self.container(Hub=hubs))
        if p == "/hubs/search":
            lim = q.get("limit", "")
            return j(self.container(Hub=lib.search(q.get("query", ""), int(lim) if lim.isdigit() else 3)))
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
                return j(self.container())
            # the media bytes: nothing here decodes, but the app's part probe must get a 200 with a
            # length so the route planner reaches its own (host-side) failure instead of a socket one
            return (200, "video/x-matroska", b"\x1a\x45\xdf\xa3" + b"\x00" * 60)
        if len(segs) == 3 and segs[:2] == ["library", "streams"]:
            sid = int(segs[2]) if segs[2].isdigit() else -1
            sidecar = getattr(lib, "sidecars", {}).get(sid)
            if sidecar is not None:
                return (200, "application/x-subrip", sidecar.read_bytes())
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
        size = path.stat().st_size
        start, end, status = 0, max(0, size - 1), 200
        raw_range = self.headers.get("Range")
        if raw_range:
            import re
            match = re.fullmatch(r"bytes=(\d*)-(\d*)", raw_range.strip())
            if not match or (not match.group(1) and not match.group(2)):
                self.send_response(416)
                self.send_header("Content-Range", f"bytes */{size}")
                self.send_header("Content-Length", "0")
                self.end_headers()
                return True
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
                return True
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
        with self.server.pms.lock:
            self.server.pms.requests.append((self.path, status))
        return True

    def _do(self, method):
        if self.server.verbose:
            print(f"mock_pms: REQUEST {method} {self.server.pms.safe_path(self.path)}",
                  file=sys.stderr, flush=True)
        if method in ("GET", "HEAD") and self._media(method):
            return
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n) if n else b""
        status, ctype, data = self.server.pms.handle(method, self.path, body)
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


def serve(port, seed=1, host="127.0.0.1", verbose=False, movies=48, rail_fixture=False,
          media=None, extra_media=None, catalog=None, catalog_cache=None, hero=None):
    """Start a mock PMS in a daemon thread; returns (server, pms). Loopback only by default: the
    app on the simulator is on this machine, and a LAN-facing listener would be one more thing
    the outbound guard has to reason about. `catalog` serves the demo library instead of a seed."""
    if catalog is not None:
        lib = CatalogLibrary(catalog, cache=catalog_cache, hero=hero)
    else:
        lib = Library(seed=seed, movies=movies, rail_fixture=rail_fixture, media=media,
                      extra_media=extra_media)
    pms = MockPms(lib)
    srv = Server((host, port), Handler)
    srv.pms = pms
    srv.verbose = verbose
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
    print("mock_pms selftest: ok")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
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
    ap.add_argument("--hero", help="with --catalog: the film at the head of Continue Watching (the hero)")
    ap.add_argument("--verbose", action="store_true")
    ap.add_argument("--selftest", action="store_true")
    a = ap.parse_args()
    if a.hero and a.catalog is None:
        ap.error("--hero needs --catalog")
    if not 0 <= a.movies <= 1000:
        ap.error("--movies must be between 0 and 1000")
    if a.selftest:
        selftest()
        return
    try:
        srv, pms = serve(a.port, seed=a.seed, host=a.host, verbose=a.verbose,
                         movies=a.movies, rail_fixture=a.rail_fixture, media=a.media,
                         extra_media=a.extra_media, catalog=a.catalog, catalog_cache=a.catalog_cache,
                         hero=a.hero)
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
    for rk, path in sorted(getattr(pms.lib, "extra_media_files", {}).items()):
        print(f"mock_pms: extra-media rk={rk} file={path}", flush=True)
    try:
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        pass
    finally:
        srv.shutdown()
        if pms.unknown:
            print("mock_pms: unknown paths seen: " + ", ".join(sorted(set(pms.unknown))), file=sys.stderr)


if __name__ == "__main__":
    main()
