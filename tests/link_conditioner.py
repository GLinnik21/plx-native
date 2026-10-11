#!/usr/bin/env python3
"""A network link conditioner for `tests/mock_pms.py`, modelled on macOS Network Link Conditioner.

The mock otherwise answers instantly on loopback, which hides every bug that needs a page to be
IN FLIGHT while the user keeps pressing a key: the sliding 24-card window only misbehaves when a
page landing and a key repeat coincide, and on loopback a page lands before the next key. This
module makes the mock slow the way a remote server is slow.

A profile is four numbers:

    latency_ms   fixed delay between receiving a request and starting to answer it
    jitter_ms    extra delay drawn uniformly from [0, jitter_ms] per request
    kbit         downlink in kilobit/s; the response BODY is written at this rate, so a 24-item
                 listing (~30 KB) and a poster JPEG (~15 KB) take realistically different times.
                 0 means unlimited.
    error_rate   the fraction of requests that fail, drawn per request: `reset` closes the
                 connection without a response (what a flaky WAN does), `503` answers a server error

and a request belongs to one CLASS, so listings and images can be conditioned separately:

    listing   every PMS API read (hubs, section listings, children, search, metadata)
    image     `/photo/:/transcode` and the `.../thumb`, `.../art` artwork paths
    (control  `/_mock/*` and media bytes are never conditioned)

Named profiles (`PROFILES`) are rough NLC presets, not measurements of any real network.

DETERMINISM. Every per-request draw (jitter, failure) is a pure function of (seed, class, the
request's path and query, how many times that same request has been seen), taken from a hash and
not from a shared RNG. Two runs with one seed therefore make the same request fail the same way
regardless of how the app's threads interleave, which a shared `random.Random` would not give.

TARGETED FAULTS. A random rate fails a given page only by luck, so a short list may be walked
without one failing and a retry assertion over it proves nothing. A `Fault` is the deterministic
counterpart: it fails the first `attempts` requests for EACH page (same path and same
`X-Plex-Container-Start`) of every endpoint whose path matches `path`, then lets that page through.
`min_start` leaves the head page (start 0) alone so the screen can open. `Conditioner.set_faults()`
replaces the rule set and forgets the attempts counted so far, so a test re-arms it between two
walks; `POST /_mock/link {"faults": [...]}` does the same at run time and `--link-fault` at start.

RUN TIME. `Conditioner.set()` replaces the profile of one class (or all) under a lock, so one test
can go fast -> slow -> fast. `tests/mock_pms.py` exposes it as `POST /_mock/link` (loopback only).
"""
import dataclasses
import hashlib
import re
import threading
import time


@dataclasses.dataclass(frozen=True)
class LinkProfile:
    latency_ms: float = 0.0
    jitter_ms: float = 0.0
    kbit: float = 0.0
    error_rate: float = 0.0
    error_kind: str = "reset"  # "reset" | "503"

    def validate(self):
        if self.latency_ms < 0 or self.jitter_ms < 0 or self.kbit < 0:
            raise ValueError("latency, jitter and kbit must not be negative")
        if not 0.0 <= self.error_rate <= 1.0:
            raise ValueError("error_rate must be between 0 and 1")
        if self.error_kind not in ("reset", "503"):
            raise ValueError("error_kind must be reset or 503")
        return self

    def to_json(self):
        return dataclasses.asdict(self)


PROFILES = {
    "none": LinkProfile(),
    "dsl": LinkProfile(latency_ms=50, jitter_ms=10, kbit=8000),
    "3g": LinkProfile(latency_ms=200, jitter_ms=50, kbit=780),
    "edge": LinkProfile(latency_ms=400, jitter_ms=100, kbit=240),
    "lossy": LinkProfile(latency_ms=100, jitter_ms=50, kbit=2000, error_rate=0.1),
    "very-bad": LinkProfile(latency_ms=800, jitter_ms=300, kbit=100, error_rate=0.2, error_kind="503"),
    # A house far from its server: fine bandwidth, long and uneven round trips. The owner's bug
    # was seen against this shape (a remote server, not a LAN one).
    "remote-wan": LinkProfile(latency_ms=250, jitter_ms=120, kbit=4000),
}

CLASSES = ("listing", "image")
ALL = "all"

_PROFILE_KEYS = {"latency_ms": float, "jitter_ms": float, "kbit": float, "error_rate": float,
                 "error_kind": str}
# Short spellings accepted in a spec string.
_ALIASES = {"latency": "latency_ms", "jitter": "jitter_ms", "kbit": "kbit", "error": "error_rate",
            "errors": "error_rate", "kind": "error_kind"}


def request_class(path):
    """`listing`, `image` or None (never conditioned: the control surface and media bytes)."""
    if path.startswith("/_mock/") or path.startswith("/library/parts/") \
            or path.startswith("/video/:/transcode/universal/"):
        return None
    if path == "/photo/:/transcode" or "/thumb" in path or path.endswith(("/art", "/composite")) \
            or "/art/" in path or "/composite/" in path:
        return "image"
    return "listing"


def make_profile(base, **overrides):
    """`base` (a LinkProfile or a profile name) with `overrides` applied, validated."""
    if isinstance(base, str):
        if base not in PROFILES:
            raise ValueError(f"unknown link profile {base!r} (known: {', '.join(sorted(PROFILES))})")
        base = PROFILES[base]
    fields = {}
    for key, value in overrides.items():
        key = _ALIASES.get(key, key)
        if key not in _PROFILE_KEYS:
            raise ValueError(f"unknown link parameter {key!r}")
        fields[key] = _PROFILE_KEYS[key](value)
    return dataclasses.replace(base, **fields).validate()


def parse_spec(spec):
    """`[class=]profile[:k=v,k=v]` -> (class_or_ALL, LinkProfile).

    `3g`, `image=edge`, `listing=dsl:latency=300,error=0.1`, `all=none`, or a bare override list
    on the default profile: `:kbit=500`. Raises ValueError on anything it does not understand."""
    klass = ALL
    head, _, tail = spec.partition(":")
    if "=" in head:
        klass, _, head = head.partition("=")
        if klass not in CLASSES + (ALL,):
            raise ValueError(f"unknown link class {klass!r} (listing, image or all)")
    overrides = {}
    for part in filter(None, tail.split(",")):
        key, eq, value = part.partition("=")
        if not eq:
            raise ValueError(f"link override {part!r} is not key=value")
        overrides[key.strip()] = value.strip()
    return klass, make_profile(head or "none", **overrides)


@dataclasses.dataclass(frozen=True)
class Fault:
    path: str                 # regex, searched against the request path
    attempts: int = 1         # how many requests for one page fail before it is let through
    kind: str = "reset"       # "reset" | "503"
    min_start: int = 1        # pages starting below this are never faulted (the head page)

    def validate(self):
        try:
            re.compile(self.path)
        except re.error as e:
            raise ValueError(f"fault path is not a regex: {e}")
        if self.attempts < 1 or self.min_start < 0:
            raise ValueError("fault attempts must be at least 1 and min_start not negative")
        if self.kind not in ("reset", "503"):
            raise ValueError("fault kind must be reset or 503")
        return self

    def to_json(self):
        return dataclasses.asdict(self)


def make_fault(path, **overrides):
    types = {"attempts": int, "kind": str, "min_start": int}
    for key in overrides:
        if key not in types:
            raise ValueError(f"unknown fault parameter {key!r}")
    return Fault(path=str(path), **{k: types[k](v) for k, v in overrides.items()}).validate()


def parse_fault(spec):
    """`REGEX[@k=v,k=v]` -> Fault, e.g. `^/library/sections/1/all$@attempts=2,kind=503`."""
    path, _, tail = spec.rpartition("@") if "@" in spec else (spec, "", "")
    overrides = {}
    for part in filter(None, tail.split(",")):
        key, eq, value = part.partition("=")
        if not eq:
            raise ValueError(f"fault override {part!r} is not key=value")
        overrides[key.strip()] = value.strip()
    return make_fault(path, **overrides)


class Plan:
    """What to do to ONE request: sleep `delay_s`, then either fail (`fail` = "reset" or "503")
    or write the body at `kbit` (0 = unthrottled)."""
    __slots__ = ("klass", "delay_s", "kbit", "fail")

    def __init__(self, klass, delay_s, kbit, fail):
        self.klass, self.delay_s, self.kbit, self.fail = klass, delay_s, kbit, fail

    def __repr__(self):
        return f"Plan({self.klass}, delay={self.delay_s * 1000:.0f}ms, kbit={self.kbit}, fail={self.fail})"


class Conditioner:
    def __init__(self, seed=1, profiles=None):
        self.seed = seed
        self._lock = threading.Lock()
        self._profiles = {c: PROFILES["none"] for c in CLASSES}
        self._seen = {}
        self._faults = ()
        self._fault_seen = {}
        for klass, profile in (profiles or {}).items():
            self._apply(klass, profile)

    def _apply(self, klass, profile):
        for c in (CLASSES if klass == ALL else (klass,)):
            self._profiles[c] = profile

    def set(self, klass, profile):
        """Replace one class's profile (or every class's) from now on, mid-run."""
        if klass not in CLASSES + (ALL,):
            raise ValueError(f"unknown link class {klass!r}")
        with self._lock:
            self._apply(klass, profile)

    def set_faults(self, faults):
        """Replace the targeted faults and zero their per-page attempt counts."""
        faults = tuple(f.validate() for f in faults)
        with self._lock:
            self._faults = faults
            self._fault_seen = {}

    def faults(self):
        with self._lock:
            return list(self._faults)

    def profiles(self):
        with self._lock:
            return dict(self._profiles)

    def active(self):
        return any(p != PROFILES["none"] for p in self.profiles().values())

    def _unit(self, klass, request, salt, nth):
        digest = hashlib.sha256(f"{self.seed}|{klass}|{request}|{nth}|{salt}".encode()).digest()
        return int.from_bytes(digest[:8], "big") / float(1 << 64)

    def _targeted(self, path, start):
        """The kind of the first Fault that fails this request, counting it; None if none does.
        Caller holds the lock."""
        try:
            start = int(start or 0)
        except ValueError:
            start = 0
        for i, fault in enumerate(self._faults):
            if start < fault.min_start or not re.search(fault.path, path):
                continue
            key = (i, path, start)
            seen = self._fault_seen.get(key, 0)
            self._fault_seen[key] = seen + 1
            if seen < fault.attempts:
                return fault.kind
        return None

    def plan(self, path, query="", start=None):
        """The Plan for one request; None for a class that is never conditioned. `start` is the
        page's `X-Plex-Container-Start` when it came as a header rather than in the query."""
        klass = request_class(path)
        if klass is None:
            return None
        request = f"{path}?{query}"
        if start is None:
            for part in query.split("&"):
                if part.startswith("X-Plex-Container-Start="):
                    start = part.partition("=")[2]
        with self._lock:
            profile = self._profiles[klass]
            nth = self._seen.get(request, 0)
            self._seen[request] = nth + 1
            targeted = self._targeted(path, start)
        if profile == PROFILES["none"]:
            return Plan(klass, 0.0, 0.0, targeted)
        delay_ms = profile.latency_ms + profile.jitter_ms * self._unit(klass, request, "jitter", nth)
        fail = targeted or (profile.error_kind if profile.error_rate and
                            self._unit(klass, request, "fail", nth) < profile.error_rate else None)
        return Plan(klass, delay_ms / 1000.0, profile.kbit, fail)


def transfer_seconds(nbytes, kbit):
    """How long `nbytes` take at `kbit` kilobit/s (0: no limit)."""
    return 0.0 if not kbit else nbytes * 8 / (kbit * 1000.0)


def write_throttled(write, data, kbit, clock=time.monotonic, sleep=time.sleep, tick_s=0.02):
    """Write `data` through `write(chunk)` no faster than `kbit` kilobit/s.

    Paced against a running deadline rather than sleeping a fixed amount per chunk, so a slow
    `write` does not stretch the transfer and a long body does not drift. Returns the seconds the
    transfer took by `clock`."""
    start = clock()
    if not kbit or not data:
        write(data)
        return clock() - start
    bytes_per_s = kbit * 1000.0 / 8
    chunk = max(1, int(bytes_per_s * tick_s))
    sent = 0
    while sent < len(data):
        piece = data[sent:sent + chunk]
        write(piece)
        sent += len(piece)
        wait = start + sent / bytes_per_s - clock()
        if wait > 0:
            sleep(wait)
    return clock() - start
