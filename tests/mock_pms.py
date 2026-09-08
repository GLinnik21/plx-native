#!/usr/bin/env python3
"""A SYNTHETIC Plex Media Server — enough of the PMS REST surface for the app to boot to Home,
browse a library, open a detail page, a season, a person, search, mark watched, and start (and
fail) a play, with NOT ONE household byte anywhere in it.

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

    python3 tests/mock_pms.py --port 32499              # serve until Ctrl-C
    python3 tests/mock_pms.py --port 32499 --selftest   # prove the shapes without an app
    python3 tests/mock_pms.py --port 32499 --movies 321 --rail-fixture  # multi-page A–Z rail

The app reaches it as any other server: `make sim-shot SIM_PMS=127.0.0.1 SIM_PORT=32499` with
any non-empty string in `$SIM_DIR/plxnative-token` (the token is accepted, never checked).
"""
import argparse
import hashlib
import json
import random
import struct
import sys
import threading
import time
import urllib.parse
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# ---------------------------------------------------------------- the closed alphabet -------

def sname(rng):
    """One name from the closed alphabet: `s` + 8 lowercase hex digits."""
    return "s%08x" % rng.getrandbits(32)


def swords(rng, n):
    return " ".join(sname(rng) for _ in range(n))


# ---------------------------------------------------------------- the generated library ----

class Library:
    """Two sections — movies (key 1) and shows (key 2) — with people, genres, collections,
    seasons, episodes, media parts, chapters, markers, watch state and blur colours. Every id is
    dense from 1 so the app's server-local integer assumptions hold."""

    def __init__(self, seed=1, movies=48, shows=6, seasons=2, episodes=6, rail_fixture=False):
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

    def search(self, query):
        q = query.lower()
        hubs = []
        for kind, title in (("movie", "movie"), ("show", "show"), ("episode", "episode")):
            rows = [it for it in self.items.values() if it["type"] == kind and q in it["title"].lower()]
            hubs.append({"title": title, "type": kind, "hubIdentifier": kind, "size": len(rows),
                         "Metadata": rows[:8]})
        people = [dict(t, type="actor", key=f"/library/sections/1/all?actor={t['id']}",
                       librarySectionID=1)
                  for t in self.people.values() if q in t["tag"].lower()]
        hubs.append({"title": "actor", "type": "actor", "hubIdentifier": "actor",
                     "size": len(people), "Directory": people[:8]})
        cols = [{"tag": c["tag"], "id": c["id"], "type": "collection", "librarySectionID": 1,
                 "key": f"/library/sections/1/all?collection={c['id']}", "reasonTitle": ""}
                for c in self.collections.values() if q in c["tag"].lower()]
        hubs.append({"title": "collection", "type": "collection", "hubIdentifier": "collection",
                     "size": len(cols), "Directory": cols})
        return hubs


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
                return j(self.container(Metadata=rows))
            rk = int(ids) if ids.isdigit() else -1
            sub = segs[3]
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
            if q.get("excludeContinueWatching") != "1":
                hubs[0]["Metadata"] = lib.continue_watching()[:12]
            for h in hubs:
                h["size"] = len(h["Metadata"])
            return j(self.container(Hub=hubs))
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
            return j(self.container(Hub=hubs))
        if p == "/hubs/search":
            return j(self.container(Hub=lib.search(q.get("query", ""))))
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
            rows = [dict(it, playQueueItemID=1)] if it else []
            return j(self.container(Metadata=rows, playQueueID=1, playQueueSelectedItemID=1,
                                    playQueueSelectedItemOffset=0, playQueueTotalCount=len(rows),
                                    playQueueVersion=1))
        if segs[:1] == ["playQueues"] and len(segs) == 2:
            return j(self.container(Metadata=[], playQueueID=int(segs[1]) if segs[1].isdigit() else 1))
        if p.startswith("/library/parts/"):
            # the media bytes: nothing here decodes, but the app's part probe must get a 200 with a
            # length so the route planner reaches its own (host-side) failure instead of a socket one
            return (200, "video/x-matroska", b"\x1a\x45\xdf\xa3" + b"\x00" * 60)
        if p.startswith("/video/:/transcode/universal/start"):
            # an HLS/transcode START: this server encodes nothing, so the honest answer is the one
            # a PMS gives for a session it cannot serve — the app's route planner then lands on
            # its failure read-out, which is the player screen a simulator can reach
            return (404, "text/plain", b"mock_pms: no transcoder")
        if p == "/video/:/transcode/universal/decision":
            return j(self.container(generalDecisionCode=1000, generalDecisionText="Direct play OK",
                                    mdeDecisionCode=1000, Metadata=[]))
        self.unknown.append(p)
        print(f"mock_pms: UNKNOWN {method} {path}", file=sys.stderr, flush=True)
        return j(self.container())


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "MockPMS/0"

    def log_message(self, fmt, *args):  # quiet by default; --verbose flips it
        if self.server.verbose:
            sys.stderr.write("mock_pms: " + fmt % args + "\n")

    def _do(self, method):
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


def serve(port, seed=1, host="127.0.0.1", verbose=False, movies=48, rail_fixture=False):
    """Start a mock PMS in a daemon thread; returns (server, pms). Loopback only by default: the
    app on the simulator is on this machine, and a LAN-facing listener would be one more thing
    the outbound guard has to reason about."""
    pms = MockPms(Library(seed=seed, movies=movies, rail_fixture=rail_fixture))
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
    assert not pms.unknown, pms.unknown
    for s_ in (srv, srv2, srv3, srv4):
        s_.shutdown()
    print("mock_pms selftest: ok")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--port", type=int, default=32499)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--movies", type=int, default=48, help="movie count, 0–1000 (keys must not overlap shows)")
    ap.add_argument("--rail-fixture", action="store_true", help="synthetic A/C/F/M/Z sort-title groups for rail tests")
    ap.add_argument("--verbose", action="store_true")
    ap.add_argument("--selftest", action="store_true")
    a = ap.parse_args()
    if not 0 <= a.movies <= 1000:
        ap.error("--movies must be between 0 and 1000")
    if a.selftest:
        selftest()
        return
    srv, pms = serve(a.port, seed=a.seed, host=a.host, verbose=a.verbose,
                     movies=a.movies, rail_fixture=a.rail_fixture)
    print(f"mock_pms: serving seed={a.seed} on http://{a.host}:{srv.server_address[1]}", flush=True)
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
