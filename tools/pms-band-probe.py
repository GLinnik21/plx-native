#!/usr/bin/env python3
"""Measure how a real PMS answers the "count at a band edge" probes of a Library A-Z rail.

A value-sort rail (Critic rating, Duration, Date added, Unplayed, ...) would not load the library.
It would ask the server for the COUNT of items on the high side of each band edge,

    GET /library/sections/{k}/all?type=T&<field>>>=E        (Container-Start 0, Container-Size 0)

which returns no rows and a `totalSize`. In a descending listing that count is the offset of the
first item below E, so band offsets are the counts themselves. Whether that holds is a property
of the server, not of the spec: an unknown filter key is silently IGNORED (every count equals the
library total), a float edge may be cast to an integer, `>>=` may be inclusive, and items with no
value may sort anywhere. This tool measures each of those, per family, against the real paged
listing, and prints a verdict per family.

## What it is allowed to do (read-only by construction)

* There is ONE request function, `Pms.get`. It issues `GET` only, and refuses any path that is not
  the server root, `/library/sections`, or `/library/sections/{id}/{all|genre|year|decade|
  firstCharacter}`. Nothing under `/:/` (scrobble, timeline, rate), no refresh/analyze/empty-trash
  and no `/library/metadata` route is reachable, so nothing can change a library, a watch state or
  a collection. `--selftest` proves the guard without a server.
* The token is read from a file (default: the saved Guest token), sent as the `X-Plex-Token`
  header only, and never printed or written. The owner token file is refused by name.
* Requests are spaced by `--delay-ms` and capped by `--max-requests`.
* Rows are reduced the moment they are parsed to their numeric sort fields, their ids and indexes and
  the first glyph of their sort title (the letters check uses it); no title, name or path is printed
  or written, and the server's identity is never kept, printed or written. The one-glyph letter keys
  with their sizes may be printed or written. Libraries are labelled by kind and order only ("movie#1").

## Verdict per family (the first column of the table)

VERIFIED      every edge probe's count equals the number of items on the high side of the edge,
              AND the real listing, read at `Start=lead+c-1, Size=2` for the first and the last
              edge only, straddles the edge there.
OFF-BY-ONE    `>>=E` behaves as an inclusive `>=E`.
CAST-TO-INT   a fractional edge is truncated before it is compared (9.51 acts as 9).
IGNORED       every count equals the library total: the field is not a filter key here.
UNSUPPORTED   an HTTP error, or an empty answer where items exist.
UNSTABLE      the same probe answered differently when repeated.
ROUNDED       counts disagree with the listing's values only within +-0.05: the server compares finer
              stored values than the listing displays (0.1-rounded), so no edge is exact.
MISMATCH      a count matches none of the above hypotheses (the detail column says which edge).
UNTESTABLE    this library has fewer than two distinct values for the field, so no edge can
              separate the hypotheses; the detail column says what was still observed.

The `ops` column lists the other operators at one value present in the data, each `ok` or `X`
(wrong), and `nulls` where `<<=`/`<=` also counted the items that have no value. `nulls desc/asc`
say where the no-value items sit in the real listings; `nullsLast d/a` where `:nullsLast` puts them.

## `--floor-and-negative-only` (a second, smaller run)

Two questions the family run does not ask, answered with a few dozen more counts:

* FLOOR: what a probe at ZERO counts. For each of movie rating, duration, viewCount and show
  unviewedLeafCount, viewCount it asks `>>=-1`, `>>=0` and `>=0`, and reports them beside the
  listing's own tallies (no value / zero / above zero). The listing hides a stored zero as "no
  value" for some fields, so the probes decide: `>=0` minus `>>=0` is the items that STORE zero.
* NEGATIVE DATE EDGES: whether a negative literal on a date field is an absolute epoch (pre-1970),
  is ignored, or is read as a RELATIVE date (`-N` is relative-date syntax on these fields). Seven
  probes per field (`originallyAvailableAt`, `addedAt`), each judged against what every reading
  predicts; `reading` lists the readings the count is consistent with.

Counts only: no title, name or date of any item is printed (only how many items are older than 1970).

## `--types 3,4` (a third run: the SEASONS and EPISODES listings of the show libraries)

For `type=3` / `type=4` of each show library it measures: the count probe against the unfiltered
listing; the `Sort[]` the server declares (`includeMeta=1`, key names only); whether
`sort=<key>:asc|desc` is accepted and orders the rows, for titleSort, originallyAvailableAt,
rating, audienceRating, addedAt and lastViewedAt (a titleSort listing is also checked for being
grouped by show and for what its order is keyed on); the same family table as above (edge counts,
straddles, null placement, operators) for the five fields a row can carry; and the
`/firstCharacter?type=T` letters index against the titleSort order (do the prefix sums of the
bucket sizes land on the listing's real letter boundaries?). A row keeps the FIRST GLYPH of its sort
title (as `ch`, in every run) for the letters check, and a letter bucket's key is a single glyph;
no title is printed or written. Libraries stay labelled "show#1".

Usage:  tools/pms-band-probe.py [--base URL] [--token-file PATH] [--json PATH] [--only a,b]
                                [--floor-and-negative-only] [--types 3,4 [--letters-under-filters]]

## `--types 3,4 --letters-under-filters` (a fourth, small run)

Whether `/firstCharacter?type=T&<terms>` counts under the grid's own terms: for each type it asks the
`Size=0` count and the letters index with no term, with the application's Unwatched key (`unwatchedLeaves=1`
for seasons, `unwatched=1` for episodes) and, when the type's `/genre` list has a value that is neither empty
nor the whole type, with that genre alone and with both, and prints whether the letters' sizes SUM to the
count of the same terms. Sums, bucket numbers and totals only; no letter, per-letter size or id.
"""

import argparse
import calendar
import datetime
import json
import math
import os
import re
import statistics
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

DEFAULT_BASE = "http://127.0.0.1:32400"
DEFAULT_TOKEN_FILE = "~/.config/plxnative/guest-token"
OWNER_TOKEN_NAMES = {"pms-token"}

# The only paths `Pms.get` will request. Anything else is a programming error, not a request.
ALLOWED_PATH = re.compile(
    r"/|/library/sections|/library/sections/[0-9]+/(?:all|genre|year|decade|firstCharacter)"
)

OPS = (">=", "<<=", "<=", "=")  # beside the strictly-greater `>>=` every edge probe uses
RELATIVE_FORMS = ("-12h", "-30d", "-3w", "-4w", "-1mon", "-1mo", "-6mon", "-6m", "-1y", "-2y")
NUMERIC_FIELDS = (
    "rating", "audienceRating", "duration", "addedAt", "lastViewedAt", "year", "viewCount",
    "leafCount", "viewedLeafCount",
)
PAGE = 200  # rows per page when a whole listing is read


class ProbeDown(Exception):
    """The server did not answer at all (not an HTTP error status)."""


class RequestCap(Exception):
    """`--max-requests` was reached."""


class Reply:
    """One answer, reduced to numbers. `rows` are numeric field dicts; `dirs` are `{key, size}`."""

    def __init__(self, status, container, ms):
        self.status = status
        self.ms = ms
        container = container or {}
        self.total = container.get("totalSize")
        self.size = container.get("size")
        self.offset = container.get("offset")
        self.version = container.get("version")
        self.rows = [reduce_item(item) for item in container.get("Metadata") or []]
        self.dirs = [reduce_dir(entry) for entry in container.get("Directory") or []]
        self.types = [entry.get("type") for entry in container.get("Directory") or []]
        self.meta_types = reduce_meta(container.get("Meta"))


def reduce_item(item):
    """One listing row reduced to its numeric sort fields, its ids and indexes and the first glyph of
    its sort title (`ch`); the rest of the title is never kept."""
    row = {k: item[k] for k in NUMERIC_FIELDS if isinstance(item.get(k), (int, float))}
    for source, name in (("ratingKey", "rk"), ("grandparentRatingKey", "gp"), ("parentRatingKey", "pk"),
                         ("index", "idx"), ("parentIndex", "pidx")):
        try:
            row[name] = int(item[source])
        except (KeyError, TypeError, ValueError):
            pass
    glyph = first_glyph(item)
    if glyph:
        row["ch"] = glyph
    released = item.get("originallyAvailableAt")
    if isinstance(released, str):
        try:
            day = datetime.date.fromisoformat(released[:10])
        except ValueError:
            day = None
        if day is not None:
            row["oa_utc"] = calendar.timegm(day.timetuple())
            row["oa_local"] = int(time.mktime(day.timetuple()))
    return row


def first_glyph(item):
    """The first character of a row's sort title, upper-cased: the only piece of a title ever kept."""
    text = item.get("titleSort") or item.get("title")
    return text.upper()[:1] if isinstance(text, str) and text else None


def reduce_meta(meta):
    """`Meta.Type[]` reduced to its type name, whether it is the active one, and its Sort/Filter key names."""
    out = []
    for entry in (meta or {}).get("Type") or []:
        sorts = [s for s in entry.get("Sort") or [] if isinstance(s, dict)]
        out.append({
            "type": entry.get("type"), "active": bool(entry.get("active")),
            "sorts": [s.get("key") for s in sorts],
            "sort_desc_keys": [s.get("descKey") for s in sorts],
            "sort_default_direction": [s.get("defaultDirection") for s in sorts],
            "filters": [f.get("filter") for f in entry.get("Filter") or [] if isinstance(f, dict)],
        })
    return out


def reduce_dir(entry):
    """A directory row reduced to its key and (when present) its `size`; the title is dropped."""
    out = {"key": entry.get("key")}
    if "size" in entry:
        try:
            out["size"] = int(entry["size"])
        except (TypeError, ValueError):
            pass
    return out


class Pms:
    """A read-only client: every request in this tool goes through `get`."""

    def __init__(self, base, token, delay_ms, max_requests):
        self.base = base.rstrip("/")
        self.token = token
        self.delay = delay_ms / 1000.0
        self.max_requests = max_requests
        self.requests = 0
        self.count_ms = []   # Size=0 probes
        self.page_ms = []    # row-carrying pages
        self.dir_ms = []

    @staticmethod
    def check(path):
        """Raise ValueError unless `path` is a read-only route this tool may request."""
        route, _, query = path.partition("?")
        if "/:/" in path or "X-Plex-Token" in query:
            raise ValueError("refused route: %s" % route)
        if not ALLOWED_PATH.fullmatch(route):
            raise ValueError("refused route: %s" % route)

    def get(self, path, start=None, size=None, kind="page"):
        """GET `path`, optionally paged by header. Returns a `Reply`; never raises on HTTP errors."""
        self.check(path)
        if self.requests >= self.max_requests:
            raise RequestCap("request cap %d reached" % self.max_requests)
        if self.requests:
            time.sleep(self.delay)
        self.requests += 1
        headers = {"Accept": "application/json", "X-Plex-Token": self.token}
        if start is not None:
            headers["X-Plex-Container-Start"] = str(start)
        if size is not None:
            headers["X-Plex-Container-Size"] = str(size)
        request = urllib.request.Request(self.base + path, headers=headers, method="GET")
        began = time.monotonic()
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                status, body = response.status, response.read()
        except urllib.error.HTTPError as error:
            status, body = error.code, b""
        except (urllib.error.URLError, OSError) as error:
            raise ProbeDown("no answer from the server (%s)" % type(error).__name__) from None
        ms = (time.monotonic() - began) * 1000.0
        container = None
        if status == 200 and body:
            try:
                container = json.loads(body).get("MediaContainer")
            except ValueError:
                container = None
        reply = Reply(status, container, ms)
        if status == 200:
            if kind == "count":
                self.count_ms.append(ms)
            elif kind == "dir":
                self.dir_ms.append(ms)
            else:
                self.page_ms.append(ms)
        return reply

    # -- the three request shapes the probes are built from -------------------------------------

    def listing_path(self, section, ptype, terms=(), sort=None):
        parts = ["type=%d" % ptype] + list(terms)
        if sort:
            parts.append("sort=" + urllib.parse.quote(sort, safe=":,."))
        return "/library/sections/%d/all?%s" % (section, "&".join(parts))

    def count(self, section, ptype, terms=()):
        """`Container-Size: 0` probe: no rows, `totalSize` is the answer."""
        return self.get(self.listing_path(section, ptype, terms), 0, 0, kind="count")

    def read_all(self, section, ptype, terms=(), sort=None):
        """A whole listing as numeric rows in server order. Returns (rows, total, status)."""
        path = self.listing_path(section, ptype, terms, sort)
        rows, total, start = [], 0, 0
        while True:
            reply = self.get(path, start, PAGE)
            if reply.status != 200:
                return [], 0, reply.status
            rows.extend(reply.rows)
            total = reply.total or 0
            start += PAGE
            if start >= total or not reply.rows:
                return rows, total, 200


def term(key, op, value):
    """One filter term with the key side URL-encoded: `rating%3E%3E=9.5`."""
    return urllib.parse.quote(key + op.rstrip("="), safe=".") + "=" + format_number(value)


def format_number(value):
    if isinstance(value, float):
        return ("%.4f" % value).rstrip("0").rstrip(".")
    return str(value)


# -- the ground truth: what a filter SHOULD answer, computed from the listing's own values --------

HOLDS = {
    ">>=": lambda x, e: x > e,
    ">=": lambda x, e: x >= e,
    "<<=": lambda x, e: x < e,
    "<=": lambda x, e: x <= e,
    "=": lambda x, e: x == e,
    "!=": lambda x, e: x != e,
}


def expect(values, op, edge):
    """How many non-null `values` satisfy `op edge` (a NULL satisfies no comparison)."""
    holds = HOLDS[op]
    return sum(1 for x in values if x is not None and holds(x, edge))


def placement(values):
    """Where the null values sit in a listing: (count, 'none'|'first'|'last'|'all'|'mixed')."""
    nulls = [i for i, x in enumerate(values) if x is None]
    n, k = len(values), len(nulls)
    if not k:
        return 0, "none"
    if k == n:
        return k, "all"
    if nulls == list(range(k)):
        return k, "first"
    if nulls == list(range(n - k, n)):
        return k, "last"
    return k, "mixed"


def ordered(values, descending):
    """True when the non-null values never go the wrong way (ties allowed)."""
    nn = [x for x in values if x is not None]
    pairs = zip(nn, nn[1:])
    return all(a >= b for a, b in pairs) if descending else all(a <= b for a, b in pairs)


def classify(count, total, values, edge, kind):
    """What one `>>=edge` answer means: ok | inclusive | int-cast | ignored | other."""
    strict = expect(values, ">>=", edge)
    if count == strict:
        return "ok"
    if count == expect(values, ">=", edge):
        return "inclusive"
    if kind == "float" and count == expect(values, ">>=", math.trunc(edge)):
        return "int-cast"
    if count == total:
        return "ignored"
    return "other"


def pick_edges(distinct, wanted=4):
    """Data-driven edge values: spread over the distinct values, never the minimum."""
    candidates = distinct[1:]
    if len(candidates) <= wanted:
        return list(candidates)
    picks = []
    for q in ((0.15, 0.4, 0.65, 0.9) if wanted > 2 else (0.3, 0.75)):
        v = distinct[int(round(q * (len(distinct) - 1)))]
        if v != distinct[0] and v not in picks:
            picks.append(v)
    return picks


class Family:
    """One band family: a filter key, the sort that orders by it, and how to read its value off a row."""

    def __init__(self, name, ptype, key, sort, delta, value, kind="int", alts=(), variants=None, low=None):
        self.name, self.ptype, self.key, self.sort = name, ptype, key, sort
        self.delta, self.kind, self.alts = delta, kind, tuple(alts)
        # a family that orders :asc (and answers `<<=`) by a DIFFERENT value than :desc (and `>>=`)
        self.low = low
        # name -> row -> number|None. A date family offers two readings of the same stored day.
        self.variants = variants or {"": value}

    def edge(self, v):
        return round(v - self.delta, 4) if self.kind == "float" else v - 1


def getter(field):
    return lambda row: row.get(field)


def unviewed(row):
    if "leafCount" not in row:
        return None
    return row["leafCount"] - row.get("viewedLeafCount", 0)


def movie_families():
    return [
        Family("rating", 1, "rating", "rating", 0.01, getter("rating"), "float"),
        Family("audienceRating", 1, "audienceRating", "audienceRating", 0.01, getter("audienceRating"), "float"),
        Family("duration", 1, "duration", "duration", 1, getter("duration")),
        Family("addedAt", 1, "addedAt", "addedAt", 1, getter("addedAt")),
        Family("lastViewedAt", 1, "lastViewedAt", "lastViewedAt", 1, getter("lastViewedAt")),
        Family("year", 1, "year", "year", 1, getter("year")),
        Family("originallyAvailableAt", 1, "originallyAvailableAt", "originallyAvailableAt", 1, None,
               variants={"utc-midnight": getter("oa_utc"), "local-midnight": getter("oa_local")}),
        Family("viewCount", 1, "viewCount", "viewCount", 1, getter("viewCount")),
    ]


def show_families(episode_newest, episode_oldest):
    def newest_episode(row):
        return episode_newest.get(row.get("rk"))

    def oldest_episode(row):
        return episode_oldest.get(row.get("rk"))

    return [
        Family("addedAt", 2, "addedAt", "addedAt", 1, getter("addedAt"), alts=("show.addedAt",)),
        Family("lastViewedAt", 2, "lastViewedAt", "lastViewedAt", 1, getter("lastViewedAt"), alts=("show.lastViewedAt",)),
        Family("year", 2, "year", "year", 1, getter("year"), alts=("show.year",)),
        Family("originallyAvailableAt", 2, "originallyAvailableAt", "originallyAvailableAt", 1, None,
               variants={"utc-midnight": getter("oa_utc"), "local-midnight": getter("oa_local")}),
        Family("audienceRating", 2, "audienceRating", "audienceRating", 0.01, getter("audienceRating"), "float"),
        Family("rating", 2, "rating", "rating", 0.01, getter("rating"), "float"),
        Family("unviewedLeafCount", 2, "unviewedLeafCount", "unviewedLeafCount", 1, unviewed),
        Family("viewedLeafCount", 2, "viewedLeafCount", "viewedLeafCount", 1, getter("viewedLeafCount")),
        Family("viewCount", 2, "viewCount", "viewCount", 1, getter("viewCount"), alts=("show.viewCount",)),
        Family("episode.addedAt", 2, "episode.addedAt", "episode.addedAt", 1, newest_episode, low=oldest_episode),
    ]


# -- measuring one family -------------------------------------------------------------------------

def measure_family(pms, fam, section, terms, full):
    """Run every probe for one family on one library; returns a result dict of numbers and verdicts.

    `full` adds the ascending listing, the straddle reads, the operator variants and the spelling
    checks; a filter-term run (`full=False`) only checks the edge counts against that listing.
    """
    out = {"family": fam.name, "key": fam.key, "verdict": None, "detail": []}
    detail = out["detail"]
    path = pms.listing_path(section, fam.ptype, terms, fam.sort + ":desc")
    desc_rows, total, status = pms.read_all(section, fam.ptype, terms, fam.sort + ":desc")
    if status != 200:
        # The sort itself is refused: read the values unsorted so the filter can still be judged.
        out["sort_status"] = status
        desc_rows, total, status = pms.read_all(section, fam.ptype, terms)
        path = None
    out["total"] = total
    primary = next(iter(fam.variants))
    values = {name: [fn(row) for row in desc_rows] for name, fn in fam.variants.items()}
    low_values = [(fam.low or fam.variants[primary])(row) for row in desc_rows]
    base = values[primary]
    nonnull = [x for x in base if x is not None]
    distinct = sorted(set(nonnull))
    out.update(nonnull=len(nonnull), distinct=len(distinct))

    if path is not None:
        out["desc_nulls"], out["desc_placement"] = placement(base)
        out["desc_sorted"] = ordered(base, True)
        if not out["desc_sorted"]:
            detail.append("the :desc listing is not in value order")
    if full and path is not None:
        asc_rows, _, asc_status = pms.read_all(section, fam.ptype, terms, fam.sort + ":asc")
        if asc_status == 200:
            asc = [(fam.low or fam.variants[primary])(row) for row in asc_rows]
            out["asc_nulls"], out["asc_placement"] = placement(asc)
            out["asc_sorted"] = ordered(asc, False)
            if not out["asc_sorted"]:
                detail.append("the :asc listing is not in value order")
            elif fam.low:
                detail.append(":asc orders by the OLDEST value, :desc by the NEWEST")
        if out["desc_nulls"]:
            for direction in ("desc", "asc"):
                tail = pms.read_all(section, fam.ptype, terms, "%s:%s:nullsLast" % (fam.sort, direction))
                out[direction + "_nullsLast"] = (
                    placement([fam.variants[primary](r) for r in tail[0]])[1] if tail[2] == 200 else "HTTP %d" % tail[2])

    if full or len(distinct) < 2:
        # nulls inside a probe: `>>= (min - 1 unit)` should count exactly the non-null items.
        floor = fam.edge(distinct[0]) if distinct else -1
        reply = pms.count(section, fam.ptype, list(terms) + [term(fam.key, ">>=", floor)])
        if reply.status != 200:
            out["verdict"] = "UNSUPPORTED"
            detail.append("HTTP %d on the first probe" % reply.status)
            return out
        out["nonnull_probe"] = reply.total
        if reply.total == total and len(nonnull) < total:
            detail.append("a probe below every value also counts the %d no-value items" % (total - len(nonnull)))

    if len(distinct) < 2:
        out["verdict"] = "UNTESTABLE"
        detail.append("%d distinct value%s among %d items" % (len(distinct), "" if len(distinct) == 1 else "s", total))
        if distinct:
            above = pms.count(section, fam.ptype, list(terms) + [term(fam.key, ">>=", distinct[0])])
            out["probe_at_only_value"] = above.total
            detail.append("`>>=` at the only value answered %s (items above it: 0)" % above.total)
        return out

    edges = pick_edges(distinct, 4 if full else 2)
    results = []
    for v in edges:
        e = fam.edge(v)
        reply = pms.count(section, fam.ptype, list(terms) + [term(fam.key, ">>=", e)])
        if reply.status != 200:
            out["verdict"] = "UNSUPPORTED"
            detail.append("HTTP %d at edge" % reply.status)
            return out
        c = reply.total
        tags = {name: classify(c, total, vals, e, fam.kind) for name, vals in values.items()}
        results.append({"edge": e, "count": c, "ms": round(reply.ms, 1),
                        "expected": {name: expect(vals, ">>=", e) for name, vals in values.items()}, "tags": tags})
    out["edges"] = results

    # a date family: keep the reading of the stored day that the server actually agrees with
    best = max(values, key=lambda name: sum(1 for r in results if r["tags"][name] == "ok"))
    out["reading"] = best
    tags = [r["tags"][best] for r in results]
    counts = [r["count"] for r in results]
    expected_best = [r["expected"][best] for r in results]

    if all(t == "ignored" for t in tags) or (all(c == total for c in counts) and any(x != total for x in expected_best)):
        out["verdict"] = "IGNORED"
    elif all(c == 0 for c in counts) and any(x > 0 for x in expected_best):
        out["verdict"] = "UNSUPPORTED"
        detail.append("empty answers where items exist")
    elif "int-cast" in tags:
        out["verdict"] = "CAST-TO-INT"
    elif "inclusive" in tags:
        out["verdict"] = "OFF-BY-ONE"
        detail.append("`>>=E` counted items equal to E")
    elif "other" in tags:
        bad = [r for r in results if r["tags"][best] == "other"]
        within = [expect(values[best], ">>=", r["edge"] + 0.05) <= r["count"] <= expect(values[best], ">>=", r["edge"] - 0.05)
                  for r in bad]
        if fam.kind == "float" and all(within):
            out["verdict"] = "ROUNDED"
            detail.append("the listing shows 0.1-rounded values; every count agrees within +-0.05")
        else:
            out["verdict"] = "MISMATCH"
            detail.append("edge/count/expected: %s" % [(r["edge"], r["count"], r["expected"][best]) for r in bad][:3])
    elif "ignored" in tags:
        out["verdict"] = "MISMATCH"
        detail.append("some edges answered the whole library")
    else:
        out["verdict"] = "VERIFIED"
    if fam.kind == "float":
        cast_differs = sum(1 for r in results if expect(values[best], ">>=", r["edge"]) != expect(values[best], ">>=", math.trunc(r["edge"])))
        out["float_discriminating_edges"] = cast_differs

    if not full:
        return out

    # the real listing straddles each edge where the count says it does
    if path is not None and out["verdict"] == "VERIFIED" and out["desc_placement"] != "mixed":
        lead = out["desc_nulls"] if out["desc_placement"] == "first" else 0
        valfn = fam.variants[best]
        straddles = []
        for r in (results[0], results[-1]):
            c = r["count"]
            start = lead + max(c - 1, 0)
            page = pms.get(path, start, 2 if c else 1)
            vs = [valfn(row) for row in page.rows]
            e = r["edge"]
            if c == 0:
                ok = not vs or vs[0] is None or vs[0] <= e
            else:
                ok = bool(vs) and vs[0] is not None and vs[0] > e and (len(vs) < 2 or vs[1] is None or vs[1] <= e)
            straddles.append(ok)
        out["straddles"] = straddles
        if not all(straddles):
            out["verdict"] = "MISMATCH"
            detail.append("the paged listing does not straddle the edge at offset count")

    # the ascending listing: its offsets are the descending counts mirrored (total - c) when nulls
    # lead and :asc is the reverse order; a family whose :asc orders by another value is probed with `<<=`
    if out["verdict"] == "VERIFIED" and "asc_sorted" in out and out["asc_sorted"] and out.get("asc_placement") in ("none", "first"):
        asc_path = pms.listing_path(section, fam.ptype, terms, fam.sort + ":asc")
        r = results[len(results) // 2]
        if fam.low:
            low_distinct = sorted({x for x in low_values if x is not None})
            e = low_distinct[len(low_distinct) // 2]
            c = pms.count(section, fam.ptype, list(terms) + [term(fam.key, "<<=", e)]).total
            vs = [fam.low(row) for row in pms.get(asc_path, max(c - 1, 0), 2 if c else 1).rows]
            ok = (c == 0 or (bool(vs) and (vs[0] is None or vs[0] < e))) and (len(vs) < 2 or (vs[1] is not None and vs[1] >= e))
        else:
            e = r["edge"]
            c = total - r["count"]
            vs = [fam.variants[best](row) for row in pms.get(asc_path, max(c - 1, 0), 2 if c else 1).rows]
            ok = (c == 0 or (bool(vs) and (vs[0] is None or vs[0] <= e))) and (len(vs) < 2 or (vs[1] is not None and vs[1] > e))
        out["asc_straddle"] = ok
        if not ok:
            out["verdict"] = "MISMATCH"
            detail.append("the :asc listing does not straddle the mirrored edge")

    # a repeat of the first probe: the same question must give the same answer
    again = pms.count(section, fam.ptype, list(terms) + [term(fam.key, ">>=", results[0]["edge"])])
    if again.total != results[0]["count"]:
        out["verdict"] = "UNSTABLE"
        detail.append("repeat answered %s then %s" % (results[0]["count"], again.total))

    # the other operators, at one value that exists in the data. `<<=`/`<=` answer from the LOW value.
    mid = distinct[len(distinct) // 2]
    nulls = total - len(nonnull)
    ops = {}
    for op in OPS:
        reply = pms.count(section, fam.ptype, list(terms) + [term(fam.key, op, mid)])
        low_side = op in ("<<=", "<=")
        want = expect(low_values if low_side else values[best], op, mid)
        if reply.status != 200:
            ops[op] = "HTTP %d" % reply.status
        elif reply.total == want:
            ops[op] = "ok"
        elif low_side and nulls and reply.total == want + nulls:
            ops[op] = "ok+nulls"
        elif reply.total == total and want != total:
            ops[op] = "ignored"
        else:
            ops[op] = "got %s want %d" % (reply.total, want)
    out["ops"] = ops

    # an alternative key spelling must answer like the plain one
    spellings = {}
    for alt in fam.alts:
        reply = pms.count(section, fam.ptype, list(terms) + [term(alt, ">>=", results[len(results) // 2]["edge"])])
        spellings[alt] = "HTTP %d" % reply.status if reply.status != 200 else (
            "same" if reply.total == results[len(results) // 2]["count"] else "differs (%s)" % reply.total)
    if spellings:
        out["spellings"] = spellings
    return out


def float_edge_test(pms, fam, section, rows):
    """Probe v-0.01, v, v+0.01 around fractional values: float comparison or integer truncation?"""
    values = [fam.variants[next(iter(fam.variants))](r) for r in rows]
    total = len(values)
    fractional = sorted({x for x in values if x is not None and x != math.floor(x)})
    if not fractional:
        return {"family": fam.name, "skipped": "no fractional value in this library"}
    picks = []
    for target in (9.5, 7.5):
        near = min((x for x in fractional if x not in picks), key=lambda x: abs(x - target), default=None)
        if near is not None:
            picks.append(near)
    probes = []
    for v in picks:
        for e in (round(v - 0.01, 4), v, round(v + 0.01, 4)):
            reply = pms.count(section, fam.ptype, [term(fam.key, ">>=", e)])
            probes.append({
                "edge_offset_from_value": round(e - v, 2),
                "value_bucket": "%d.x" % math.floor(v),
                "count": reply.total, "float": expect(values, ">>=", e),
                "int_cast": expect(values, ">>=", math.trunc(e)),
            })
    float_hits = sum(1 for p in probes if p["count"] == p["float"])
    cast_hits = sum(1 for p in probes if p["count"] == p["int_cast"])
    discriminating = sum(1 for p in probes if p["float"] != p["int_cast"])
    return {"family": fam.name, "probes": probes, "discriminating": discriminating, "float_hits": float_hits,
            "cast_hits": cast_hits, "of": len(probes), "total": total}


def relative_date_test(pms, fam, section, rows):
    """Relative forms (`-30d`, `-1y`, ...): accepted? Is the implied cutoff the nominal one?

    The count c of a window fixes the cutoff's age to [age of the oldest item inside, age of the
    newest item outside); a form is `consistent` when a nominal reading of its unit falls there.
    """
    values = sorted((v for v in (fam.variants[""](r) for r in rows) if v is not None), reverse=True)
    out = {}
    for form in RELATIVE_FORMS:
        now = time.time()
        reply = pms.count(section, fam.ptype, [term(fam.key, ">>=", form)])
        if reply.status != 200:
            out[form] = "HTTP %d" % reply.status
            continue
        c = reply.total
        lo = (now - values[c - 1]) / 86400.0 if c else 0.0
        hi = (now - values[c]) / 86400.0 if c < len(values) else float("inf")
        readings = [name for name, days in nominal_readings(form) if lo - 0.01 <= days < hi + 0.01]
        out[form] = {"count": c, "cutoff_age_days": [round(lo, 1), None if hi == float("inf") else round(hi, 1)],
                     "consistent_with": readings}
    return out


def nominal_readings(form):
    """[(reading, days)] a relative form could mean; `m` is minutes in the spec but may be read as months."""
    match = re.fullmatch(r"-([0-9]+)(h|d|w|mon|mo|m|y)", form)
    if not match:
        return []
    n, unit = int(match.group(1)), match.group(2)
    if unit == "m":
        return [("minutes", n / 1440.0), ("months", 30.44 * n)]
    days = {"h": n / 24.0, "d": float(n), "w": 7.0 * n, "mon": 30.44 * n, "mo": 30.44 * n, "y": 365.25 * n}[unit]
    return [({"h": "hours", "d": "days", "w": "weeks", "y": "years"}.get(unit, "months"), days)]


# -- directories, request shapes and filter terms -------------------------------------------------

def directories(pms, section):
    out = {}
    for name in ("year", "decade", "firstCharacter"):
        reply = pms.get("/library/sections/%d/%s" % (section, name), 0, 1000, kind="dir")
        entry = {"status": reply.status, "ms": round(reply.ms, 1)}
        if reply.status == 200:
            entry["buckets"] = len(reply.dirs)
            entry["rows_carry_size"] = bool(reply.dirs) and all("size" in d for d in reply.dirs)
            if entry["rows_carry_size"]:
                entry["sum_of_sizes"] = sum(d["size"] for d in reply.dirs)
            if name == "decade":
                keys = [d["key"] for d in reply.dirs]
                entry["keys"] = keys
        out[name] = entry
    return out


def decade_sum(pms, section, ptype, keys):
    """Sum of `decade=K` counts over the directory's keys, to compare with the library total."""
    sums, counted = 0, 0
    for key in keys:
        reply = pms.count(section, ptype, ["decade=%s" % urllib.parse.quote(str(key), safe="")])
        if reply.status == 200 and isinstance(reply.total, int):
            sums += reply.total
            counted += 1
    return {"buckets_probed": counted, "sum": sums}


def request_shapes(pms, section):
    """Size=0 spelled three ways, and the documented trap (Size without Start in the query)."""
    out = {}
    base = "/library/sections/%d/all?type=1" % section
    header = pms.get(base, 0, 0, kind="count")
    out["header Start=0 Size=0"] = {"status": header.status, "size": header.size, "total": header.total,
                                     "offset": header.offset, "ms": round(header.ms, 1)}
    query = pms.get(base + "&X-Plex-Container-Start=0&X-Plex-Container-Size=0", kind="count")
    out["query Start=0&Size=0"] = {"status": query.status, "size": query.size, "total": query.total, "ms": round(query.ms, 1)}
    lone = pms.get(base + "&X-Plex-Container-Size=0", kind="count")
    out["query Size=0 only"] = {"status": lone.status, "size": lone.size, "total": lone.total, "rows": len(lone.rows)}
    hdr_lone = pms.get(base, None, 0, kind="count")
    out["header Size=0 only"] = {"status": hdr_lone.status, "size": hdr_lone.size, "total": hdr_lone.total, "rows": len(hdr_lone.rows)}
    return out


def pick_genre(pms, section, ptype, total):
    """One genre id whose count is neither empty nor the whole library, or None."""
    reply = pms.get("/library/sections/%d/genre" % section, 0, 1000, kind="dir")
    best = None
    for entry in reply.dirs[:5]:
        key = entry["key"]
        if key is None:
            continue
        count = pms.count(section, ptype, ["genre=%s" % urllib.parse.quote(str(key), safe="")]).total
        if isinstance(count, int) and 3 <= count < total:
            if best is None or abs(count - total / 2) < abs(best[1] - total / 2):
                best = (str(key), count)
    return best[0] if best else None


def filter_terms_test(pms, section, ptype, families, genre, unwatched_term, extra_terms):
    """Do the probes honour the grid's own terms? Counts under each term set vs that listing."""
    sets = [("unwatched", [unwatched_term])]
    if genre:
        g = "genre=%s" % urllib.parse.quote(genre, safe="")
        sets += [("genre", [g]), ("genre+unwatched", [g, unwatched_term])]
    sets += [(name, [t]) for name, t in extra_terms]
    out = []
    for label, terms in sets:
        page = pms.count(section, ptype, terms)
        entry = {"terms": label, "page_totalSize": page.total, "families": []}
        for fam in families:
            res = measure_family(pms, fam, section, terms, full=False)
            entry["families"].append({k: res.get(k) for k in ("family", "verdict", "total", "nonnull", "edges", "detail")})
        out.append(entry)
    return out


# -- the floor and the negative date edges (the second run) ----------------------------------------

FLOOR_FAMILIES = {"movie": ("rating", "duration", "viewCount"), "show": ("unviewedLeafCount", "viewCount")}
FLOOR_PROBES = ((">>=", -1), (">>=", 0), (">=", 0))
NEGATIVE_FIELDS = ("originallyAvailableAt", "addedAt")
# 1960-01-01 UTC and one second before it, one second before 1970, and 1970-01-01 itself
NEGATIVE_PROBES = ((">=", -315619200), (">>=", -315619201), (">=", -1), (">>=", -1),
                   ("<<=", -1), ("<=", -315619200), (">=", 0))


def reading_of(count, absolute, relative, total):
    """Which hypotheses a negative-edge count is consistent with, as a '/'-joined string."""
    names = [name for name, expected in (("absolute", absolute), ("relative-seconds", relative),
                                         ("ignored", total), ("zero", 0)) if count == expected]
    return "/".join(names) or "other"


def floor_test(pms, fam, section, rows):
    """What `>>=-1`, `>>=0` and `>=0` count, beside what the listing shows for the same items."""
    values = [fam.variants[next(iter(fam.variants))](r) for r in rows]
    listed = [x for x in values if x is not None]
    out = {"family": fam.name, "total": len(values),
           "listing": {"no_value": len(values) - len(listed), "zero": sum(1 for x in listed if x == 0),
                       "above_zero": sum(1 for x in listed if x > 0)},
           "counts": {}}
    for op, edge in FLOOR_PROBES:
        reply = pms.count(section, fam.ptype, [term(fam.key, op, edge)])
        out["counts"]["%s%s" % (op, edge)] = reply.total if reply.status == 200 else "HTTP %d" % reply.status
    c = out["counts"]
    if all(isinstance(v, int) for v in c.values()):
        out["stored_zero"] = c[">=0"] - c[">>=0"]
        out["no_stored_value"] = out["total"] - c[">=0"]
        out["negative_literal_is_absolute"] = c[">>=-1"] == c[">=0"]
        out["strict_zero_matches_listing_above_zero"] = c[">>=0"] == out["listing"]["above_zero"]
    return out


def negative_edge_test(pms, fam, section, rows):
    """Is a negative literal on a date field an absolute epoch, ignored, or a relative date?"""
    key = "oa_utc" if fam.name == "originallyAvailableAt" else fam.name
    values = [r.get(key) for r in rows]
    present = [v for v in values if v is not None]
    nulls, total, now = len(values) - len(present), len(values), time.time()
    out = {"family": fam.name, "total": total, "no_value": nulls,
           "before_1970": sum(1 for v in present if v < 0), "probes": []}
    for op, edge in NEGATIVE_PROBES:
        reply = pms.count(section, fam.ptype, [term(fam.key, op, edge)])
        entry = {"term": "%s%s%s" % (fam.key, op, edge), "status": reply.status}
        if reply.status == 200:
            low = op in ("<<=", "<=")  # these also count the items with no value
            absolute = expect(present, op, edge) + (nulls if low else 0)
            relative = expect(present, op, now + edge) + (nulls if low else 0)
            entry.update(count=reply.total, if_absolute=absolute, if_relative_seconds=relative,
                         reading=reading_of(reply.total, absolute, relative, total))
        out["probes"].append(entry)
    return out


def floor_and_negative_library(pms, section, kind, label):
    ptype = 1 if kind == "movie" else 2
    rows, total, _ = pms.read_all(section, ptype)
    print("\n#### %s: %d items" % (label, total))
    families = movie_families() if kind == "movie" else show_families({}, {})
    lib = {"label": label, "kind": kind, "items": total}
    lib["floor"] = [floor_test(pms, f, section, rows) for f in families if f.name in FLOOR_FAMILIES[kind]]
    lib["negative_edges"] = [negative_edge_test(pms, f, section, rows) for f in families if f.name in NEGATIVE_FIELDS]
    return lib


# -- seasons and episodes (the `--types` run) -------------------------------------------------------

TYPE_NAMES = {3: "season", 4: "episode"}
TYPE_FAMILIES = ("rating", "audienceRating", "addedAt", "lastViewedAt", "originallyAvailableAt")
SORT_KEYS = ("titleSort", "originallyAvailableAt", "rating", "audienceRating", "addedAt", "lastViewedAt")
SORT_FIELD = {"titleSort": None, "originallyAvailableAt": "oa_utc"}  # every other sort reads its own name
PRESENCE_FIELDS = ("rating", "audienceRating", "addedAt", "lastViewedAt", "oa_utc", "viewCount", "year")


def type_families(ptype):
    """The five fields a season or episode row can carry, as the movie families, pointed at `ptype`."""
    out = []
    for fam in movie_families():
        if fam.name in TYPE_FAMILIES:
            fam.ptype = ptype
            out.append(fam)
    return out


def bucket(glyph):
    """The letters-index bucket a first glyph is expected in: a letter is its own, anything else is '#'."""
    return glyph if glyph and glyph.isalpha() else "#"


def runs(ids):
    """(runs of equal neighbours, distinct ids): the two are equal exactly when the listing is grouped."""
    return (1 + sum(1 for a, b in zip(ids, ids[1:]) if a != b) if ids else 0), len(set(ids))


def prefix_match(buckets, glyphs):
    """Lay the directory's buckets end to end over the listing: (rows in their bucket, rows, first bad key).

    `buckets` is `[(key, size)]` in directory order, `glyphs` the first glyph of each listing row in
    listing order. A bucket is right when every row in its slice `[prefix, prefix + size)` belongs to it.
    """
    pos, matched, first_bad = 0, 0, None
    for key, size in buckets:
        hit = sum(1 for g in glyphs[pos:pos + size] if bucket(g) == key)
        matched += hit
        if hit != size and first_bad is None:
            first_bad = key
        pos += size
    return matched, len(glyphs), first_bad


def tally(glyphs):
    """{bucket: rows} for a list of first glyphs."""
    counts = {}
    for g in glyphs:
        counts[bucket(g)] = counts.get(bucket(g), 0) + 1
    return counts


def sort_test(pms, section, ptype, key, default_ids):
    """Is `sort=<key>:asc|desc` accepted, and does it order the rows by that field? -> (result, rows by direction)"""
    field = SORT_FIELD.get(key, key)
    out, rows_by, series = {"sort": key}, {}, {}
    for direction in ("asc", "desc"):
        rows, total, status = pms.read_all(section, ptype, [], "%s:%s" % (key, direction))
        entry = {"status": status}
        if status == 200:
            rows_by[direction] = rows
            entry.update(total=total, order_changed=[r.get("rk") for r in rows] != default_ids)
            if field:
                values = [r.get(field) for r in rows]
                nulls, place = placement(values)
                entry.update(nulls=nulls, placement=place, distinct=len({v for v in values if v is not None}),
                             ordered=ordered(values, direction == "desc"))
                series[direction] = [v for v in values if v is not None]
        out[direction] = entry
    if len(series) == 2:
        out["desc_is_reverse_of_asc"] = series["asc"] == series["desc"][::-1]
    entries = [out["asc"], out["desc"]]
    if any(e["status"] != 200 for e in entries):
        out["verdict"] = "REFUSED (HTTP %s/%s)" % (out["asc"]["status"], out["desc"]["status"])
    elif not field:
        out["verdict"] = "ACCEPTED"
    elif max(e["distinct"] for e in entries) < 2:
        out["verdict"] = "ACCEPTED, untestable (%d distinct)" % max(e["distinct"] for e in entries)
    elif all(e["ordered"] for e in entries):
        out["verdict"] = "ACCEPTED, ORDERS"
    else:
        out["verdict"] = "ACCEPTED, NOT ORDERED"
    return out, rows_by


def title_keying(rows, ptype, show_order, show_glyph, descending=False):
    """What a Title-ordered listing of seasons/episodes is keyed on: the row's own title, or its show's?"""
    group = "gp" if ptype == 4 else "pk"
    ids = [r.get(group) for r in rows]
    n_runs, n_groups = runs(ids)
    seen = [i for k, i in enumerate(ids) if k == 0 or ids[k - 1] != i]
    wanted = [i for i in (show_order[::-1] if descending else show_order) if i in set(seen)]
    index_key = (lambda r: (r.get("pidx", -1), r.get("idx", -1))) if ptype == 4 else (lambda r: (r.get("idx", -1),))
    bad_index_runs, start = 0, 0
    while start < len(rows):
        end = start
        while end < len(rows) and ids[end] == ids[start]:
            end += 1
        keys = [index_key(r) for r in rows[start:end]]
        bad_index_runs += 0 if keys == sorted(keys) else 1
        start = end
    return {
        "group_runs": n_runs, "groups": n_groups, "grouped_by_show": n_runs == n_groups,
        "group_order_is_show_titleSort_order": seen == wanted,
        "own_glyph_monotone": ordered([bucket(r.get("ch")) for r in rows], descending),
        "show_glyph_monotone": ordered([bucket(show_glyph.get(i)) for i in ids], descending),
        "runs_not_in_index_order": bad_index_runs,
        "rows_without_show_id": sum(1 for i in ids if i is None),
    }


def letters_index(pms, section, ptype, terms=()):
    """`/firstCharacter?type=T[&terms]` as [(key, size)] in directory order, plus whether the sizes sum to the total."""
    path = "/library/sections/%d/firstCharacter?%s" % (section, "&".join(["type=%d" % ptype] + list(terms)))
    reply = pms.get(path, 0, 1000, kind="dir")
    out = {"status": reply.status}
    if reply.status == 200:
        out["buckets"] = [(d["key"], d["size"]) for d in reply.dirs if "size" in d and d.get("key") is not None]
        out["sum_of_sizes"] = sum(size for _, size in out["buckets"])
    return out


def letters_vs_order(buckets, rows, show_glyph, ptype, descending=False):
    """Do the directory's buckets, laid end to end, land on the letter boundaries of a Title-ordered listing?

    Judged by the row's OWN first glyph and by its SHOW's; a descending listing meets the buckets reversed.
    """
    group = "gp" if ptype == 4 else "pk"
    walk = buckets[::-1] if descending else buckets
    out = {}
    for name, glyphs in (("own", [r.get("ch") for r in rows]), ("show", [show_glyph.get(r.get(group)) for r in rows])):
        matched, n, first_bad = prefix_match(walk, glyphs)
        out[name] = {"rows_in_bucket": matched, "of": n, "first_bad_bucket": first_bad,
                     "bucket_sizes_equal_directory": tally(glyphs) == dict(buckets)}
    return out


def letters_filter_terms(pms, section, ptype, buckets):
    """A `firstCharacter=<key>` filter term must count what the directory says (or be seen to be ignored)."""
    counted = {key: pms.count(section, ptype, ["firstCharacter=%s" % urllib.parse.quote(str(key), safe="")]).total
               for key, _ in buckets}
    return {"agree": sum(1 for k, n in buckets if counted[k] == n), "of": len(buckets),
            "disagree": {k: (n, counted[k]) for k, n in buckets if counted[k] != n}}


# The terms the application sends with a Seasons / Episodes listing under its Unwatched switch
# (`SecState::query_filters`): the show-level key for seasons, the plain one for episodes.
APP_UNWATCHED = {3: "unwatchedLeaves=1", 4: "unwatched=1"}


def pick_type_genre(pms, section, ptype, total):
    """One genre id of this type's `/genre` list whose listing count is neither empty nor the whole type, or None."""
    reply = pms.get("/library/sections/%d/genre?type=%d" % (section, ptype), 0, 1000, kind="dir")
    out = {"status": reply.status, "values": len(reply.dirs), "id": None}
    for entry in reply.dirs[:5]:
        if entry["key"] is None:
            continue
        term_ = "genre=%s" % urllib.parse.quote(str(entry["key"]), safe="")
        count = pms.count(section, ptype, [term_]).total
        if isinstance(count, int) and 0 < count < total:
            out["id"] = term_
            break
    return out


def measure_letters_under_filters(pms, section, label, ptypes):
    """Do the letters of `/firstCharacter?type=T&<terms>` sum to the SAME terms' `Size=0` count?

    The Library asks the letters with the grid's own terms and trusts the sum to equal the listing it shows
    (a letter jump is a prefix sum). If a filter is ignored by the directory the sums differ. Counts only:
    the sums, the bucket numbers and the totals; no key, size per letter or id is printed.
    """
    lib = {"label": label, "kind": "show", "types": {}}
    print("\n#### %s: letters under the grid's terms" % label)
    for ptype in ptypes:
        total = pms.count(section, ptype).total
        genre = pick_type_genre(pms, section, ptype, total or 0)
        # The app's Unwatched key matches the whole type on a library nothing was watched in, which cannot tell a
        # directory that honours a term from one that ignores it. So the same key with `=0` (the watched ones, a
        # subset or nothing) and a `year=` term (a real episode / season filter, a proper subset) are asked too:
        # they are not terms the app sends, they tell whether the DIRECTORY follows a filter term at all.
        sets = [("none", []), ("unwatched", [APP_UNWATCHED[ptype]]), ("unwatched=0", [APP_UNWATCHED[ptype][:-1] + "0"])]
        years = {}
        for row in pms.read_all(section, ptype)[0]:
            if isinstance(row.get("year"), (int, float)):
                years[int(row["year"])] = years.get(int(row["year"]), 0) + 1
        if years:
            sets.append(("year (subset)", ["year=%d" % max(years, key=years.get)]))
        if genre["id"]:
            sets += [("genre", [genre["id"]]), ("genre+unwatched", [genre["id"], APP_UNWATCHED[ptype]])]
        rows = []
        for name, terms in sets:
            count = pms.count(section, ptype, terms)
            letters = letters_index(pms, section, ptype, terms)
            rows.append({"terms": name, "count_status": count.status, "listing_total": count.total,
                         "letters_status": letters["status"], "buckets": len(letters.get("buckets", [])),
                         "sum_of_sizes": letters.get("sum_of_sizes"),
                         "sum_equals_total": letters.get("sum_of_sizes") == count.total})
        lib["types"][str(ptype)] = {"name": TYPE_NAMES[ptype], "type_total": total,
                                    "genre_values": genre["values"], "genre_picked": bool(genre["id"]), "sets": rows}
        print("== type=%d (%s): %d in the type; genre list status %s with %d values, one picked: %s" % (
            ptype, TYPE_NAMES[ptype], total or 0, genre["status"], genre["values"], bool(genre["id"])))
        print(table(["terms", "listing total", "letters sum", "buckets", "sum == total"],
                    [[r["terms"], r["listing_total"], r["sum_of_sizes"], r["buckets"], r["sum_equals_total"]] for r in rows]))
    return lib


def measure_types_library(pms, section, label, ptypes):
    """The seasons / episodes measurements for one show library; prints the family tables as it goes."""
    shows, show_total, _ = pms.read_all(section, 2, [], "titleSort:asc")
    show_order = [r["rk"] for r in shows if "rk" in r]
    show_glyph = {r["rk"]: r.get("ch") for r in shows if "rk" in r}
    lib = {"label": label, "kind": "show", "shows": show_total, "types": {}}
    print("\n#### %s: %d shows" % (label, show_total))
    for ptype in ptypes:
        res = lib["types"][str(ptype)] = {"name": TYPE_NAMES[ptype]}
        counts_before, pages_before = len(pms.count_ms), len(pms.page_ms)
        probe = pms.count(section, ptype)
        rows, total, status = pms.read_all(section, ptype)
        default_ids = [r.get("rk") for r in rows]
        res["count_probe"] = {"status": probe.status, "totalSize": probe.total, "size": probe.size,
                              "rows_returned": len(probe.rows), "offset": probe.offset}
        res["unfiltered"] = {"status": status, "totalSize": total, "rows_read": len(rows)}
        res["presence"] = {name: sum(1 for r in rows if r.get(name) is not None) for name in PRESENCE_FIELDS}
        res["presence"]["of"] = len(rows)
        meta = pms.get("/library/sections/%d/all?type=%d&includeMeta=1" % (section, ptype), 0, 0, kind="count")
        entries = [m for m in meta.meta_types if m["type"] == TYPE_NAMES[ptype]]
        declared = entries[0] if entries else {}
        res["declared"] = {"status": meta.status, "types_in_meta": [m["type"] for m in meta.meta_types],
                           "sorts": declared.get("sorts"), "sort_desc_keys": declared.get("sort_desc_keys"),
                           "sort_default_direction": declared.get("sort_default_direction"),
                           "filters": declared.get("filters")}
        print("\n== type=%d (%s): count probe totalSize=%s size=%s rows=%d; unfiltered read %d rows (totalSize %s)"
              % (ptype, TYPE_NAMES[ptype], probe.total, probe.size, len(probe.rows), len(rows), total))
        print("-- declared sorts (key, descKey, defaultDirection): %s" % list(zip(
            res["declared"]["sorts"] or [], res["declared"]["sort_desc_keys"] or [],
            res["declared"]["sort_default_direction"] or [])))
        print("-- declared filters: %s" % (res["declared"]["filters"],))
        print("-- field presence: %s" % json.dumps(res["presence"], sort_keys=True))

        res["sorts"], plain_title = [], {}
        for key in SORT_KEYS:
            sres, rows_by = sort_test(pms, section, ptype, key, default_ids)
            res["sorts"].append(sres)
            if key == "titleSort":
                plain_title = rows_by
        # the server's own Title sort is the first declared key (a composite for seasons and episodes)
        title_key = (res["declared"]["sorts"] or [None])[0]
        title_desc = (res["declared"]["sort_desc_keys"] or [None])[0]
        orders = {"default order": (rows, False), "sort=titleSort:asc": (plain_title.get("asc"), False),
                  "sort=titleSort:desc": (plain_title.get("desc"), True)}
        if title_key and title_key != "titleSort":
            for name, sort_by, descending in (("declared Title key", title_key, False), ("declared Title descKey", title_desc, True),
                                              ("declared Title key + ':desc'", title_key + ":desc", True)):
                got, _, st = pms.read_all(section, ptype, [], sort_by)
                orders[name] = (got if st == 200 else None, descending)
                res["sorts"].append({"sort": name, "verdict": "ACCEPTED" if st == 200 else "REFUSED (HTTP %d)" % st,
                                     "asc": {"status": st}, "desc": {"status": st}})
        print(table(["sort", "verdict", "asc nulls", "desc nulls", "distinct", "order changed a/d", "desc=rev(asc)"], [
            [s["sort"], s["verdict"],
             "%s(%s)" % (s["asc"].get("placement", "-"), s["asc"].get("nulls", "-")),
             "%s(%s)" % (s["desc"].get("placement", "-"), s["desc"].get("nulls", "-")),
             s["asc"].get("distinct", "-"),
             "%s/%s" % (s["asc"].get("order_changed", "-"), s["desc"].get("order_changed", "-")),
             s.get("desc_is_reverse_of_asc", "-")] for s in res["sorts"]]))

        letters = res["letters"] = letters_index(pms, section, ptype)
        letters["listing_total"] = total
        letters["sum_equals_total"] = letters.get("sum_of_sizes") == total
        print("-- letters index: %s" % json.dumps(letters, sort_keys=True))
        res["title_orders"] = {}
        for name, (got, descending) in orders.items():
            if got is None:
                res["title_orders"][name] = None
                continue
            entry = res["title_orders"][name] = {"keying": title_keying(got, ptype, show_order, show_glyph, descending)}
            if letters["status"] == 200:
                entry["letters"] = letters_vs_order(letters["buckets"], got, show_glyph, ptype, descending)
            print("-- Title order [%s]: %s" % (name, json.dumps(entry, sort_keys=True)))
        if letters["status"] == 200:
            res["letters"]["filter_term_counts"] = letters_filter_terms(pms, section, ptype, letters["buckets"])
            print("-- firstCharacter filter term: %s" % json.dumps(res["letters"]["filter_term_counts"], sort_keys=True))
            if "control_shows" not in lib:  # once per library: the same term on the shows listing, as a control
                control = letters_index(pms, section, 2)
                lib["control_shows"] = (letters_filter_terms(pms, section, 2, control["buckets"])
                                        if control["status"] == 200 else {"status": control["status"]})
                print("-- firstCharacter filter term on type=2 (control): %s" % json.dumps(lib["control_shows"], sort_keys=True))

        res["families"] = [measure_family(pms, fam, section, [], True) for fam in type_families(ptype)]
        print(family_table("%s type=%d" % (label, ptype), res["families"]))
        for fam_res in res["families"]:
            print("-- %s: probe below every value=%s; edges(edge,count,expected)=%s" % (
                fam_res["family"], fam_res.get("nonnull_probe"),
                [(e["edge"], e["count"], e["expected"]) for e in fam_res.get("edges") or []]))
        # one full page and one two-row read, timed on their own (the medians below mix every page size)
        full = pms.get(pms.listing_path(section, ptype), 0, PAGE)
        two = pms.get(pms.listing_path(section, ptype), max(min(100, total - 2), 0), 2)
        res["paged_read_ms"] = {"%d rows" % len(full.rows): round(full.ms, 1), "%d rows (second)" % len(two.rows): round(two.ms, 1)}
        counts, pages = pms.count_ms[counts_before:], pms.page_ms[pages_before:]
        res["ms"] = {"count_probes": {"n": len(counts), "median": round(statistics.median(counts), 1) if counts else None},
                     "row_pages": {"n": len(pages), "median": round(statistics.median(pages), 1) if pages else None,
                                   "max": round(max(pages), 1) if pages else None}}
        print("-- cost: %s; timed reads %s" % (json.dumps(res["ms"], sort_keys=True), json.dumps(res["paged_read_ms"], sort_keys=True)))
    return lib


# -- reporting ------------------------------------------------------------------------------------

def table(headers, rows):
    widths = [max(len(str(x)) for x in col) for col in zip(headers, *rows)]
    line = lambda cells: "  ".join(str(c).ljust(w) for c, w in zip(cells, widths)).rstrip()
    return "\n".join([line(headers), line(["-" * w for w in widths])] + [line(r) for r in rows])


def where(res, which):
    if res.get(which + "_placement") is None:
        return "-"
    n = res[which + "_nulls"]
    return "none" if not n else "%s(%d)" % (res[which + "_placement"], n)


def family_table(label, results):
    rows = []
    for res in results:
        edges = res.get("edges") or []
        ops = res.get("ops") or {}
        ok_ops = " ".join("%s:%s" % (op, "ok" if v == "ok" else "nulls" if v == "ok+nulls" else "X") for op, v in ops.items()) if ops else "-"
        rows.append([
            res["family"], res["verdict"], "%s/%s/%s" % (res.get("total", "-"), res.get("nonnull", "-"), res.get("distinct", "-")),
            where(res, "desc"), where(res, "asc"),
            "%s/%s" % (res.get("desc_nullsLast", "-"), res.get("asc_nullsLast", "-")),
            "%d/%d" % (sum(1 for r in edges if r["tags"][res["reading"]] == "ok"), len(edges)) if edges else "-",
            (",".join("y" if s else "N" for s in res["straddles"]) + (" asc:%s" % ("y" if res["asc_straddle"] else "N") if "asc_straddle" in res else ""))
            if res.get("straddles") else "-",
            ok_ops, "; ".join(res["detail"])[:70],
        ])
    return "== %s\n%s" % (label, table(
        ["family", "verdict", "n/nonnull/distinct", "nulls desc", "nulls asc", "nullsLast d/a", "edges ok", "straddle", "ops", "detail"], rows))


def measure_library(pms, section, kind, label, only, families_only):
    """Every measurement for one movie or show library; prints the family table as it goes."""
    ptype = 1 if kind == "movie" else 2
    rows, total, _ = pms.read_all(section, ptype)
    lib = {"label": label, "kind": kind, "items": total}
    print("\n#### %s: %d items" % (label, total))
    episode_newest, episode_oldest = {}, {}
    if kind == "show":
        eps, ep_total, _ = pms.read_all(section, 4)
        for ep in eps:
            if "gp" in ep and "addedAt" in ep:
                episode_newest[ep["gp"]] = max(episode_newest.get(ep["gp"], 0), ep["addedAt"])
                episode_oldest[ep["gp"]] = min(episode_oldest.get(ep["gp"], ep["addedAt"]), ep["addedAt"])
        lib["episodes"] = ep_total
    families = movie_families() if kind == "movie" else show_families(episode_newest, episode_oldest)
    if only:
        families = [f for f in families if f.name in only]
    if not families_only:
        if kind == "movie":
            lib["request_shapes"] = request_shapes(pms, section)
        # canaries: a key the server does not know must answer the whole library (the IGNORED symptom)
        lib["canary"] = {key: pms.count(section, ptype, [term(key, ">>=", 100)]).total
                         for key in (("leafCount",) if kind == "show" else ("bogusfield",))}
    results = [measure_family(pms, fam, section, [], True) for fam in families]
    lib["families"] = results
    print(family_table(label, results))
    if families_only:
        return lib
    lib["year_vs_release_year_disagree"] = sum(
        1 for r in rows if "year" in r and "oa_utc" in r
        and datetime.datetime.fromtimestamp(r["oa_utc"], datetime.timezone.utc).year != r["year"])
    lib["float_edge_tests"] = [float_edge_test(pms, fam, section, rows) for fam in families
                               if fam.kind == "float" and fam.name in ("rating", "audienceRating")]
    for fam in families:
        if fam.name == "addedAt" and kind == "movie":
            lib["relative_dates"] = relative_date_test(pms, fam, section, rows)
    lib["directories"] = directories(pms, section)
    decade_keys = lib["directories"]["decade"].get("keys") or []
    if decade_keys and kind == "movie":
        lib["decade_sum"] = decade_sum(pms, section, ptype, decade_keys)
    unwatched = "unwatched=1" if kind == "movie" else "unwatchedLeaves=1"
    extras = [("inProgress", "inProgress=1")] if kind == "movie" else []
    wanted = {"rating", "addedAt"} if kind == "movie" else {"unviewedLeafCount"}
    subset = [f for f in families if f.name in wanted]
    if subset:
        genre = pick_genre(pms, section, ptype, total)
        lib["filter_terms"] = filter_terms_test(pms, section, ptype, subset, genre, unwatched, extras)
    return lib


def print_extras(lib):
    """The parts of a library's result that are not one-row-per-family, as compact JSON lines."""
    compact = lambda value: json.dumps(value, sort_keys=True, separators=(",", ":"))
    for name in ("request_shapes", "canary", "float_edge_tests", "relative_dates", "directories", "decade_sum",
                 "year_vs_release_year_disagree"):
        if name in lib:
            print("-- %s: %s" % (name, compact(lib[name])))
    for entry in lib.get("filter_terms", []):
        print("-- terms %s: page totalSize=%s" % (entry["terms"], entry["page_totalSize"]))
        for fam in entry["families"]:
            edges = [(e["edge"], e["count"], e["expected"]) for e in fam.get("edges") or []]
            print("     %s: %s (listing total %s) edges(edge,count,expected)=%s %s" % (
                fam["family"], fam["verdict"], fam["total"], compact(edges), "; ".join(fam.get("detail") or [])))


def print_floor_and_negative(lib):
    compact = lambda value: json.dumps(value, sort_keys=True, separators=(",", ":"))
    for entry in lib["floor"]:
        print("-- floor %s" % compact(entry))
    for entry in lib["negative_edges"]:
        print("-- negative-edges %s" % compact(entry))


def selftest():
    """Offline checks of the read-only guard and of the verdict arithmetic; touches no server."""
    for bad in ("/:/scrobble?key=1", "/:/timeline", "/library/metadata/1", "/library/sections/1/refresh",
                "/library/sections/1/emptyTrash", "/library/sections/1/analyze", "/library/sections/1/all?X-Plex-Token=x",
                "/library/sections/x/all", "/library/clients", "/playlists", "//evil/library/sections"):
        try:
            Pms.check(bad)
        except ValueError:
            continue
        raise SystemExit("guard accepted %s" % bad)
    for good in ("/", "/library/sections", "/library/sections/1/all?type=1", "/library/sections/12/firstCharacter",
                 "/library/sections/2/year", "/library/sections/2/genre"):
        Pms.check(good)
    assert term("rating", ">>=", 9.49) == "rating%3E%3E=9.49"
    assert term("episode.addedAt", ">>=", 5) == "episode.addedAt%3E%3E=5"
    assert term("addedAt", "!=", 7) == "addedAt%21=7" and term("year", "=", 7) == "year=7"
    assert term("rating", "<<=", 7.5) == "rating%3C%3C=7.5" and term("rating", ">=", 7) == "rating%3E=7"
    vals = [9.5, 9.5, 9.0, 8.1, None, 7.0]
    assert expect(vals, ">>=", 9.49) == 2 and expect(vals, ">>=", 9.5) == 0 and expect(vals, ">=", 9.5) == 2
    assert classify(2, 6, vals, 9.49, "float") == "ok"
    assert classify(6, 6, vals, 9.49, "float") == "ignored"
    assert classify(2, 6, vals, 9.5, "float") == "inclusive"
    assert classify(4, 6, [9.7, 9.6, 9.5, 9.2, 8.0, None], 9.51, "float") == "int-cast"
    assert placement([None, None, 1, 2]) == (2, "first") and placement([1, 2, None]) == (1, "last")
    assert placement([1, None, 2]) == (1, "mixed") and placement([1, 2]) == (0, "none")
    assert nominal_readings("-6m")[0] == ("minutes", 6 / 1440.0) and nominal_readings("-2y")[0][0] == "years"
    assert ordered([3, 2, None, 2], True) and not ordered([1, 3], True)
    assert pick_edges([1, 2, 3]) == [2, 3] and 1 not in pick_edges(list(range(1, 40)))
    assert term("viewCount", ">>=", -1) == "viewCount%3E%3E=-1" and term("rating", ">=", 0) == "rating%3E=0"
    assert reading_of(10, 10, 4, 12) == "absolute" and reading_of(12, 10, 4, 12) == "ignored"
    assert reading_of(0, 10, 4, 12) == "zero" and reading_of(4, 10, 4, 12) == "relative-seconds"
    assert reading_of(7, 10, 4, 12) == "other" and reading_of(12, 12, 4, 12) == "absolute/ignored"
    # the `--types` run reaches only routes the guard already allowed; its extra routes stay refused
    for good in ("/library/sections/3/firstCharacter?type=4", "/library/sections/3/all?type=3&includeMeta=1",
                 "/library/sections/3/all?type=4&firstCharacter=A"):
        Pms.check(good)
    for bad in ("/library/sections/3/firstCharacter/extra", "/library/sections/3/all/1", "/library/sections/3/collection",
                "/library/sections/3/folder", "/library/sections/3/firstCharacter?type=4&X-Plex-Token=x",
                "/library/sections/3/all?type=4&url=/:/scrobble", "/library/sections/3/../../:/scrobble"):
        try:
            Pms.check(bad)
        except ValueError:
            continue
        raise SystemExit("guard accepted %s" % bad)
    assert first_glyph({"titleSort": "alpha", "title": "Zulu"}) == "A" and first_glyph({"title": "beta"}) == "B"
    assert first_glyph({}) is None and first_glyph({"title": ""}) is None
    assert reduce_item({"title": "secret", "titleSort": "secret", "parentRatingKey": "7", "index": 2, "parentIndex": 1}) == {
        "pk": 7, "idx": 2, "pidx": 1, "ch": "S"}
    meta = reduce_meta({"Type": [{"type": "episode", "active": True, "Filter": [{"filter": "genre", "title": "y"}],
                                  "Sort": [{"key": "titleSort", "descKey": "titleSort:desc", "defaultDirection": "asc",
                                            "title": "x"}]}]})
    assert meta == [{"type": "episode", "active": True, "sorts": ["titleSort"], "sort_desc_keys": ["titleSort:desc"],
                     "sort_default_direction": ["asc"], "filters": ["genre"]}]
    assert bucket("A") == "A" and bucket("7") == "#" and bucket(None) == "#" and bucket("Ж") == "Ж"
    assert runs([1, 1, 2, 2, 3]) == (3, 3) and runs([1, 2, 1]) == (3, 2) and runs([]) == (0, 0)
    letters = ["1", "A", "A", "B", "C", "C", "C"]
    assert prefix_match([("#", 1), ("A", 2), ("B", 1), ("C", 3)], letters) == (7, 7, None)
    assert prefix_match([("#", 1), ("A", 3), ("B", 0), ("C", 3)], letters) == (6, 7, "A")
    assert tally(letters) == {"#": 1, "A": 2, "B": 1, "C": 3}
    assert [f.ptype for f in type_families(4)] == [4] * len(TYPE_FAMILIES) and type_families(3)[0].ptype == 3
    print("selftest ok")


def main():
    parser = argparse.ArgumentParser(description="Measure a real PMS's band-edge count probes (read-only).")
    parser.add_argument("--base", default=DEFAULT_BASE, help="server base URL")
    parser.add_argument("--token-file", default=DEFAULT_TOKEN_FILE,
                        help="file holding the Guest token (never the owner token); sent as a header only")
    parser.add_argument("--delay-ms", type=int, default=100, help="pause between requests")
    parser.add_argument("--max-requests", type=int, default=400, help="hard cap on requests")
    parser.add_argument("--only", default="", help="comma-separated family names to run (default: all)")
    parser.add_argument("--families-only", action="store_true",
                        help="skip request-shape, canary, float, relative-date, directory and filter-term sections")
    parser.add_argument("--json", default=None, help="also write the numbers here (a path OUTSIDE the repository)")
    parser.add_argument("--floor-and-negative-only", action="store_true",
                        help="run only the zero-floor and negative-date-edge checks (a few dozen counts)")
    parser.add_argument("--types", default="",
                        help="comma-separated listing types of the SHOW libraries to measure instead (3 = seasons, "
                             "4 = episodes): count probe, sorts, ranges, letters index")
    parser.add_argument("--letters-under-filters", action="store_true",
                        help="with --types: only ask, per type, whether the /firstCharacter sizes sum to the Size=0 count "
                             "under the grid's own Unwatched and genre terms (a few dozen requests)")
    parser.add_argument("--selftest", action="store_true", help="check the guard and the arithmetic offline")
    args = parser.parse_args()
    if args.selftest:
        selftest()
        return

    ptypes = [int(t) for t in args.types.split(",") if t.strip().isdigit()]
    if args.letters_under_filters and not args.types:
        parser.error("--letters-under-filters needs --types")
    if args.types and (not ptypes or any(t not in TYPE_NAMES for t in ptypes)):
        parser.error("--types takes 3 (seasons) and/or 4 (episodes)")
    token_path = os.path.expanduser(args.token_file)
    if os.path.basename(token_path) in OWNER_TOKEN_NAMES:
        parser.error("refusing the owner token; use the Guest token file")
    try:
        with open(token_path) as handle:
            token = handle.read().strip()
    except OSError as error:
        parser.error("cannot read the token file (%s)" % type(error).__name__)
    pms = Pms(args.base, token, args.delay_ms, args.max_requests)
    only = {name for name in args.only.split(",") if name}
    report = {"libraries": []}

    try:
        root = pms.get("/")
        if root.status != 200:
            print("server root answered HTTP %d; stopping" % root.status)
            return 1
        report["version"] = (root.version or "").split("-")[0]
        listing = pms.get("/library/sections")
        if listing.status != 200:
            print("GET /library/sections answered HTTP %d for this token; stopping" % listing.status)
            return 1
        ordinals = {}
        sections = []
        for entry, kind in zip(listing.dirs, listing.types):
            if kind in ("movie", "show"):
                ordinals[kind] = ordinals.get(kind, 0) + 1
                sections.append((int(entry["key"]), kind, "%s#%d" % (kind, ordinals[kind])))
        print("PMS %s; %d request-able libraries (%s)" % (report["version"], len(sections), ", ".join(s[2] for s in sections)))

        for section, kind, label in sections:
            if ptypes:
                if kind == "show":
                    measure = measure_letters_under_filters if args.letters_under_filters else measure_types_library
                    report["libraries"].append(measure(pms, section, label, ptypes))
                continue
            if args.floor_and_negative_only:
                lib = floor_and_negative_library(pms, section, kind, label)
                report["libraries"].append(lib)
                print_floor_and_negative(lib)
                continue
            lib = measure_library(pms, section, kind, label, only, args.families_only)
            report["libraries"].append(lib)
            print_extras(lib)
    except ProbeDown as error:
        print("STOPPED: %s" % error)
        return 1
    except RequestCap as error:
        print("STOPPED: %s" % error)

    report["requests"] = pms.requests
    for name, series in (("Size=0 probes", pms.count_ms), ("row pages", pms.page_ms), ("directory reads", pms.dir_ms)):
        if series:
            report["ms_" + name] = {"n": len(series), "median": round(statistics.median(series), 1),
                                    "max": round(max(series), 1)}
    print("\nrequests: %d; %s" % (pms.requests, "; ".join(
        "%s n=%d median=%.1f ms max=%.1f ms" % (k[3:], v["n"], v["median"], v["max"]) for k, v in report.items() if k.startswith("ms_"))))
    if args.json:
        with open(args.json, "w") as handle:
            json.dump(report, handle, indent=1, sort_keys=True)
            handle.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
