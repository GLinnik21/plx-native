#!/usr/bin/env python3
"""
A Range-capable static file server, for the player-PIPELINE test tier.

Why this file exists at all, when Python ships `http.server`: **`SimpleHTTPRequestHandler` does not
implement `Range`, and the failure is silent and total.** The app's demuxer seeks by closing the
socket and re-opening with `Range: bytes=<target>-` (`ff.rs::seek_cb`), and `stream.rs` accepts any
2xx — so a server that ignores the header answers `200` with the file *from byte zero*, the AVIO
layer believes it is positioned at `target`, and every subsequent byte is offset garbage. There is
no error anywhere: the demuxer reports a corrupt bitstream, or hangs looking for a start code, and
the case reads as a player bug. That is the single most important thing this file does, and it is
why `Range` is not optional here (§4 of the spec below).

THE SPEC — what the app actually requires of a server (all read out of the code, not assumed):

  1. Request line is `GET <abs-path> HTTP/1.1` with `Host:`, the `User-Agent` boot installs and
     `Accept: */*` (`stream.rs::http_open`). Media GETs omit `Connection: close` so HTTP/1.1
     keep-alive is the default; sequential HLS/playlist GETs to the same peer reuse the fd.
     Range seeks still close from the client (`ff.rs::seek_cb`). No pipelining.
  2. Only the status code, `Content-Length:` and `Transfer-Encoding: chunked` are parsed
     (`stream.rs`, the block after the header read). Everything else we send is ignored, so extra
     headers are free but never load-bearing.
  3. Any 2xx is success (`status < 200 || status >= 300` is the rejection). `206` is therefore
     accepted, and is what a Range response must be.
  4. `Range: bytes=<n>-` MUST be honoured with `206` + a body starting at byte n. Open-ended only:
     the demuxer never sends an end offset. See the note above for what ignoring it does.
  5. `Content-Length` MUST be the length of THIS response's body (`size - n` on a 206), not the
     file size. `stream.rs` counts `consumed` per connection and stops at `content_length`; the
     total file size is captured once, at the FIRST open, into `AvioState.size` (`ff.rs:1845`) and
     is what `AVSEEK_SIZE` answers. Sending the full size on a 206 makes the reader wait for bytes
     that never come — a 15 s `SO_RCVTIMEO` stall per seek.
  6. First byte promptly: `SO_RCVTIMEO` is 15 s and the header read is a single blocking `recv`.
  7. `HEAD` is answered because it is free and makes the server debuggable by hand; the app never
     sends one.

Run it directly to serve a directory, or import `serve()` from the harness — `tests/run.py` starts
one per pipeline-tier run and stops it in teardown.

    ./tests/serve_fixtures.py --root tests/fixtures/out --port 8020

macOS trap, and it costs an afternoon every time: the **application firewall silently drops the
TV's connections to an ad-hoc python listener** — no refusal, no log line, the TV's open just reads
empty. The same shape bit `tools/netcond.py` (verified 2026-08-11). The GUI prompt "allow incoming
connections?" must be accepted once per python binary, so start this ONCE with a human at the
keyboard before going headless, and read "the server logged nothing" as the firewall rather than as
a quiet television. `--selftest` fetches from itself over the LAN address and says which it is.
"""
import argparse
import io
import os
import re
import socket
import socketserver
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler

# `bytes=<n>-` and `bytes=<n>-<m>`. The app only ever sends the open-ended form, but a closed range
# is what a browser sends when someone points one at this server to check a fixture by hand, and
# answering it wrongly there would look like a server bug while debugging a real one.
RE_RANGE = re.compile(r"^bytes=(\d+)-(\d*)$")
# Every rung `abr::LADDER` can propose, so a controller that skips intermediate encoders cannot
# land on a 404 and have it graded as a rejected candidate.
#
# **22000 is the 4K point and it used to be absent**, on the reasoning that it is feasible only for
# a UHD source and this pack's clip is 1080p. That was true and it was also the thing keeping the
# plan's I9 blocked: `route::arm_auto_fixture` hardcoded a 1080p source raster BECAUSE this rung
# 404'd, so `admits` deleted Uhd on every `auto_network` case, so the two entries the production
# table calls empirical were the two no case could reach — a fixture gap standing in for a policy.
# It is answered now, by a real 4K clip. A 1080p-source case still never requests it (the catalog
# deletes it), so nothing about the existing cases changes; a case that declares
# `source_raster: [3840, 2160]` can reach it.
ABR_RUNGS = "320|720|2000|4000|6000|8000|10000|12000|14000|16000|18000|20000|22000"
RE_ABR_PLAYLIST = re.compile(rf"^/__abr/({ABR_RUNGS})/(master|media)\.m3u8$")
RE_ABR_SEGMENT = re.compile(rf"^/__abr/({ABR_RUNGS})/segment\.ts$")
# One master listing every rung as its own variant, for a player that picks between them itself
# (the TV's native HLS controller in URI mode) rather than one the app steers rung by rung.
RE_ABR_LADDER = re.compile(r"^/__abr/ladder/master\.m3u8$")
RE_SEQUENCE = re.compile(r"(?:^|&)sequence=(\d+)")
RE_ABR_GENERATION = re.compile(r"(?:^|&)fixtureGeneration=(\d+)")
# What a rung asks PMS for is a BITRATE CEILING and the raster is the consequence; the controller's
# acceptance test is `hls_raster_within` — the decoded picture must FIT the rung's raster, not equal
# it — so several rungs legitimately share a raster.
#
# They must NOT share a FILE, and that is a change from this pack's first shape. Every rung from
# 6000 up used to serve one `pipe_abr_1080p.ts`, which is sufficient for grading which rung a
# controller chose and useless for measuring what that rung COSTS: the reachable buffer ceiling is
# `queue_bytes / media_rate` (`docs/adaptive-playback-plan.md` §0.1), so a pack that delivers the
# same bytes for 6 Mbit/s and 18 Mbit/s reports the same reserve at both and measurement step M4
# has nothing to read. The mid/high rungs now serve rate-targeted clips of their own
# (`make_fixtures.py`'s `vbr` key), one per rung.
#
# WHAT IS SYNTHETIC HERE, precisely: the MEDIA is real — really encoded at that bitrate, really
# demuxed, really fed through the same path as any other segment. What is synthetic is that a
# rung's clip is generated rather than produced by a PMS transcoder, so it carries no JIT
# production latency; `network_profile` shapes the transport and nothing here models an encoder
# falling behind. Production cadence is measurement step M3's job, against a real PMS.
ABR_FIXTURE = {
    "320": "pipe_abr_240p.ts", "720": "pipe_abr_480p.ts",
    "2000": "pipe_abr_720p.ts", "4000": "pipe_abr_720p_4m.ts",
    "6000": "pipe_abr_1080p_6m.ts", "8000": "pipe_abr_1080p_8m.ts",
    "10000": "pipe_abr_1080p_10m.ts", "12000": "pipe_abr_1080p_12m.ts",
    "14000": "pipe_abr_1080p_14m.ts", "16000": "pipe_abr_1080p_16m.ts",
    "18000": "pipe_abr_1080p_18m.ts", "20000": "pipe_abr_1080p_20m.ts",
    "22000": "pipe_abr_4k_22m.ts",
}
ABR_RASTER = {
    "320": "426x240", "720": "854x480", "2000": "1280x720", "4000": "1280x720",
    "6000": "1920x1080", "8000": "1920x1080", "10000": "1920x1080",
    "12000": "1920x1080", "14000": "1920x1080", "16000": "1920x1080",
    "18000": "1920x1080", "20000": "1920x1080",
    # The only rung in this pack above 1080p, and the only one whose raster is the POINT rather
    # than a consequence: `hls_raster_within` grades the decoded picture against the rung's box, so
    # a 22000 rung serving a 1080p clip would be admitted and would prove nothing about Uhd.
    "22000": "3840x2160",
}
# What a rung falls back to when its own clip has not been generated yet. See `_resolve`.
ABR_FALLBACK = "pipe_abr_1080p.ts"
# ...and the raster that fallback actually carries. A rung declaring anything else must NOT take
# it: the acceptance test is a bounding box, so a smaller picture is admitted and the case passes
# having proved nothing.
ABR_RASTER_FALLBACK = "1920x1080"
assert set(ABR_FIXTURE) == set(ABR_RUNGS.split("|")) == set(ABR_RASTER), (
    "a rung the route can request but this server cannot answer reads on the TV as a rejected "
    "candidate, which is indistinguishable from a real refusal"
)
# Every rung, lowest first: the order the ladder master lists its variants in.
ABR_LADDER = tuple(sorted(ABR_FIXTURE, key=int))
# `?continuous=1` on a master, a media playlist or a segment: serve the rung's parts as ONE
# continuous stream (see "Continuous timestamps" below) instead of six independent programs.
RE_ABR_CONTINUOUS = re.compile(r"(?:^|&)continuous=1(?:&|$)")
# `?top=<rung>` on the ladder master: list only the rungs at or below it.
RE_ABR_TOP = re.compile(r"(?:^|&)top=([^&]*)")


def ladder_rungs(query):
    """The rungs the ladder master lists for `query` (the part after `?`), lowest first.

    `top=<rung>` keeps the rungs whose number is at most `<rung>`, which must itself be a rung this
    server answers: `top=7000` would otherwise silently list the same ladder as `top=6000` and read
    as a typo that worked. ValueError for anything else, which the handler turns into a 400 and the
    harness into a skip.
    """
    found = RE_ABR_TOP.search(query or "")
    if not found:
        return ABR_LADDER
    top = found.group(1)
    if top not in ABR_FIXTURE:
        raise ValueError(f"unknown rung top={top!r} (rungs: {', '.join(ABR_LADDER)})")
    return tuple(rung for rung in ABR_LADDER if int(rung) <= int(top))


def abr_playlist_rungs(fixture):
    """The rungs whose clips a manifest `fixture` needs when it names a playlist this server writes.

    `fixture` is the manifest string, query included (`__abr/ladder/master.m3u8?top=6000`). `None`
    means it names a plain file under the pack root instead. Raises ValueError for a ladder whose
    `top` is not a rung.
    """
    path, _, query = fixture.partition("?")
    if RE_ABR_LADDER.match("/" + path):
        return ladder_rungs(query)
    found = RE_ABR_PLAYLIST.match("/" + path)
    return (found.group(1),) if found else None


# ---- Continuous timestamps ----
#
# Why `?continuous=1` exists. Every part of a rung (`pipe_abr_*.ts`, `_01` .. `_05`) is its own
# MPEG-TS program cut by the generator, so EVERY part's video starts at PTS 1.483 s and its audio
# at about 1.40-1.46 s, and the playlist carries no `#EXT-X-DISCONTINUITY`. The app's feed path
# reads each segment on its own and does not care. A native HLS player that places segments on a
# timeline by their PTS sees the clock jump backwards at every segment, and whether a stall on a
# high-bitrate rung is that or a real pipeline limit cannot be told apart from a fixture that
# restarts its clock. So a flagged request serves the SAME bytes with every timestamp moved
# forward by the real duration of all the sequences before it, which is what a PMS transcoder's
# one continuous segmented stream looks like.
#
# What is rewritten, and what is not. PCR (and OPCR, which no part carries) in the adaptation
# field, and the PTS and DTS in each PES header. Nothing changes length, so Range, HEAD and
# Content-Length are exactly the unflagged ones. NOT rewritten, so a flagged stream is still not a
# PMS's one continuous stream in two ways:
#   * continuity counters: each part starts its own at 0, so the counter jumps at every join;
#   * the audio cut: the shift is the part's VIDEO length (48 frames, exactly 2.0 s in the pack),
#     and a part's audio is not trimmed to it. Measured on the 20000 rung: inside a cycle each
#     segment's first audio PTS lands one AAC frame (21.3 ms) after the previous segment's last,
#     so it abuts; where the six-part cycle wraps (sequence 6, 12, 18 ...: part `_05` into part
#     0) `_05`'s audio runs about 32 ms past its 2.0 s, so the next segment's first audio PTS is
#     10.7 ms BEHIND the last one's (13.472667 s, then 13.462000 s).
TS_PACKET = 188
TICKS_PER_S = 90_000          # PTS, DTS and the PCR base run at 90 kHz
PTS_MODULUS = 1 << 33         # 33-bit PTS/DTS/PCR base: wraps after 26.5 hours
# stream_id values whose PES packet has no optional header (so no PTS): program stream map,
# padding, private_stream_2, ECM, EMM, DSM-CC, H.222.1 type E and the program stream directory.
_PES_WITHOUT_HEADER = frozenset((0xBC, 0xBE, 0xBF, 0xF0, 0xF1, 0xF2, 0xF8, 0xFF))


class TsTimeline:
    """Where one TS file's timestamps sit, and how long it plays: found once, patched per request.

    `duration` is in 90 kHz ticks. The four lists are byte offsets into the file: the 6-byte PCR /
    OPCR fields and the 5-byte PTS / DTS fields.
    """
    __slots__ = ("duration", "pcr", "opcr", "pts", "dts")

    def __init__(self, duration, pcr, opcr, pts, dts):
        self.duration, self.pcr, self.opcr, self.pts, self.dts = duration, pcr, opcr, pts, dts


def _read_pts(buf, at):
    """The 33-bit value of the 5-byte PTS/DTS field at `at` (ISO 13818-1 2.4.3.7)."""
    return (((buf[at] >> 1) & 0x07) << 30 | buf[at + 1] << 22 | (buf[at + 2] >> 1) << 15
            | buf[at + 3] << 7 | buf[at + 4] >> 1)


def _write_pts(buf, at, value):
    """Store a 33-bit value, keeping the 4-bit prefix (0010/0011/0001) and the three marker bits."""
    value %= PTS_MODULUS
    buf[at] = (buf[at] & 0xF1) | ((value >> 30) & 0x07) << 1
    buf[at + 1] = (value >> 22) & 0xFF
    buf[at + 2] = (buf[at + 2] & 0x01) | ((value >> 15) & 0x7F) << 1
    buf[at + 3] = (value >> 7) & 0xFF
    buf[at + 4] = (buf[at + 4] & 0x01) | (value & 0x7F) << 1


def _read_pcr_base(buf, at):
    """The 33-bit base of the 6-byte PCR/OPCR at `at`: 33 base bits, 6 reserved, 9 extension."""
    return buf[at] << 25 | buf[at + 1] << 17 | buf[at + 2] << 9 | buf[at + 3] << 1 | buf[at + 4] >> 7


def _write_pcr_base(buf, at, base):
    """Store the base, leaving the 6 reserved bits and the 9-bit extension byte-for-byte alone."""
    base %= PTS_MODULUS
    buf[at] = (base >> 25) & 0xFF
    buf[at + 1] = (base >> 17) & 0xFF
    buf[at + 2] = (base >> 9) & 0xFF
    buf[at + 3] = (base >> 1) & 0xFF
    buf[at + 4] = (buf[at + 4] & 0x7F) | (base & 1) << 7


def scan_ts(data):
    """Find every timestamp in an MPEG-TS file and measure how long it plays.

    Duration is the span of the VIDEO PTS plus one frame: a part's last frame is displayed for a
    frame, so first-to-last PTS alone is short by one (47 of 48 frames at 24 fps reads 1.958 s for
    a 2.000 s part, which is what ffprobe's stream `duration` says). A frame is the most common
    positive gap between sorted video PTS values (B-frame reordering shuffles packet order, not
    the sorted set), so 24, 25 and 30 fps parts need no frame rate declared.

    ValueError names a file this cannot honestly shift: lost sync, a PES header split across two
    TS packets, or fewer than two video frames to measure from.
    """
    size = len(data)
    if not size or size % TS_PACKET:
        raise ValueError(f"not a whole number of {TS_PACKET}-byte TS packets ({size} bytes)")
    pcr, opcr, pts, dts, video = [], [], [], [], []
    for pkt in range(0, size, TS_PACKET):
        if data[pkt] != 0x47:
            raise ValueError(f"lost TS sync at byte {pkt}")
        end = pkt + TS_PACKET
        control = (data[pkt + 3] >> 4) & 3
        at = pkt + 4
        if control & 2:                       # adaptation field
            field_end = at + 1 + data[at]
            if field_end > end:
                raise ValueError(f"adaptation field overruns its packet at byte {pkt}")
            flags, cursor = (data[at + 1] if data[at] else 0), at + 2    # length 0: no flags byte
            if flags & 0x10:                  # PCR
                if cursor + 6 > field_end:
                    raise ValueError(f"PCR overruns its adaptation field at byte {pkt}")
                pcr.append(cursor)
                cursor += 6
            if flags & 0x08:                  # OPCR
                if cursor + 6 > field_end:
                    raise ValueError(f"OPCR overruns its adaptation field at byte {pkt}")
                opcr.append(cursor)
            at = field_end
        # A PES starts in a packet with payload_unit_start set whose payload begins 00 00 01.
        # PSI tables start with a pointer byte instead, so a section never matches.
        if not (control & 1 and data[pkt + 1] & 0x40) or at + 9 > end or data[at:at + 3] != b"\0\0\1":
            continue
        stream_id = data[at + 3]
        if stream_id in _PES_WITHOUT_HEADER:
            continue
        marks = data[at + 7] >> 6             # PTS_DTS_flags: 2 = PTS, 3 = PTS and DTS
        if marks == 1:
            raise ValueError(f"forbidden PTS_DTS_flags at byte {pkt}")
        if not marks:
            continue
        if at + 9 + (5 if marks == 2 else 10) > end:
            raise ValueError(f"PES header split across TS packets at byte {pkt}")
        pts.append(at + 9)
        if marks == 3:
            dts.append(at + 14)
        if 0xE0 <= stream_id <= 0xEF:
            video.append(_read_pts(data, at + 9))
    # Signed distance from the first frame, so a part that straddles the 2^33 wrap still sorts.
    half = PTS_MODULUS // 2
    ordered = sorted({(value - video[0] + half) % PTS_MODULUS - half for value in video})
    if len(ordered) < 2:
        raise ValueError(f"{len(ordered)} distinct video PTS: too few to measure a duration from")
    gaps = [b - a for a, b in zip(ordered, ordered[1:])]
    frame = sorted(set(gaps), key=lambda gap: (-gaps.count(gap), gap))[0]
    return TsTimeline(ordered[-1] - ordered[0] + frame, pcr, opcr, pts, dts)


def shift_ts(data, timeline, ticks):
    """`data` with every timestamp `timeline` found moved forward by `ticks` (90 kHz), as new bytes.

    Same length, so every byte offset a Range request names is still the same place. Wraps at
    2^33 like the fields do.
    """
    out = bytearray(data)
    for at in timeline.pts + timeline.dts:
        _write_pts(out, at, _read_pts(out, at) + ticks)
    for at in timeline.pcr + timeline.opcr:
        _write_pcr_base(out, at, _read_pcr_base(out, at) + ticks)
    return bytes(out)


class _Refused(Exception):
    """A request this server answers with an explicit status instead of falling through to a 404."""

    def __init__(self, code, why):
        super().__init__(why)
        self.code, self.why = code, why


class FixtureHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "plxfixtures/1.0"
    # The default logs to stderr with a timestamp; ours go through the server's sink so a harness
    # run can attribute every open and every seek to a case.
    def log_message(self, fmt, *args):  # noqa: A003  (BaseHTTPRequestHandler's name)
        self.server.note(f"{self.address_string()} {fmt % args}")

    def setup(self):
        super().setup()
        self.server.count_accept()

    def handle(self):
        # A TV seek or teardown RSTs a kept-alive socket while this loop is blocked in
        # readline() for the next request. Mid-body already catches that in `_serve`;
        # without this, every such case prints an uncaught ConnectionResetError.
        try:
            super().handle()
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            self.server.note("client reset keep-alive")

    def finish(self):
        # socketserver.BaseRequestHandler.__init__ calls finish() in a `finally`, so it runs
        # whether handle() above returned cleanly or was already caught — it is a SECOND,
        # independent place the same reset can surface. StreamRequestHandler.finish() only
        # guards its own wfile.flush() against socket.error; the wfile/rfile close() calls
        # right after it are unguarded, and on a socket the peer already RST'd, closing the
        # wrapped file objects can still raise. Uncaught there, it reaches
        # socketserver.ThreadingMixIn's process_request_thread and prints exactly the
        # traceback this file otherwise goes out of its way to avoid.
        try:
            super().finish()
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            self.server.note("client reset keep-alive (finish)")

    def _resolve(self):
        """Map the request target to a real file under the root, or None.

        Path traversal is refused by resolving and re-checking containment rather than by filtering
        `..` out of the string — the filter form is the one that keeps being wrong.
        """
        target = self.path.split("?", 1)[0].split("#", 1)[0]
        seg = RE_ABR_SEGMENT.match(target)
        rel = self._abr_segment_file(seg) if seg else target.lstrip("/")
        if rel is None:
            return None
        if seg and self._contained(rel) is None:
            if self._abr_continuous():
                # The fallback below is for an OLD pack's unflagged cases. A flagged stream is new,
                # and the 1080p stand-in has neither this rung's parts nor their durations, so any
                # offset computed for it would be a plausible wrong timeline.
                self.server.note(f"!! {rel} missing — a continuous-timestamp segment never takes "
                                 f"the {ABR_FALLBACK} fallback. Re-run `make fixtures-pipeline`.")
                return None
            # A pack built before the per-rung ladder existed has only `pipe_abr_1080p.ts`. Serve
            # it rather than 404: a 404 on a rung reads to the controller as a REJECTED CANDIDATE,
            # which is indistinguishable from a real refusal and would make an out-of-date fixture
            # pack look like a controller bug. Said out loud every time, because the fallback also
            # silently defeats measurement step M4 — the whole point of the per-rung clips is that
            # the rungs deliver different bitrates.
            if ABR_RASTER[seg.group(1)] != ABR_RASTER_FALLBACK:
                # **A rung whose RASTER is the point may not fall back.** `hls_raster_within`
                # grades the decoded picture against the rung's BOX rather than equality, so
                # serving 1080p at the 4K rung would be ADMITTED and the case would pass having
                # proved nothing — a false pass with no symptom, which is worse than the rejected
                # candidate the fallback exists to avoid. 404 instead, and say why: a missing
                # fixture is an operator error and reads as one.
                self.server.note(f"!! {rel} missing and rung {seg.group(1)} is "
                                 f"{ABR_RASTER[seg.group(1)]} — REFUSING the "
                                 f"{ABR_RASTER_FALLBACK} fallback, which would be admitted by the "
                                 f"raster box test and pass the case for the wrong reason. "
                                 f"Re-run `make fixtures-pipeline`.")
                return None
            self.server.note(f"!! {rel} missing — falling back to pipe_abr_1080p.ts; rung "
                             f"{seg.group(1)} will NOT deliver its own bitrate. "
                             f"Re-run `make fixtures-pipeline`.")
            rel = ABR_FALLBACK
        return self._contained(rel)

    def _contained(self, rel):
        """The real path of `rel` under the root if it names a regular file there, else None.

        Containment is checked on the resolved path BEFORE any filesystem call that touches it, so
        a request target never reaches `isfile`/`open` unchecked. The root itself is a directory,
        never a servable file, so only a strict descendant qualifies.
        """
        full = os.path.realpath(os.path.join(self.server.root, rel))
        if not full.startswith(self.server.root + os.sep):
            return None
        return full if os.path.isfile(full) else None

    def _abr_segment_file(self, seg):
        """The clip for `segment.ts?sequence=N` at this rung — **N is not ignored any more**.

        It was, until 2026-08-26: every one of the 90 advertised segments resolved to one file,
        so a rung delivered the same bytes for the whole playback. 593 segments logged in the P1
        device run carried exactly TEN distinct byte sizes, one per fixture file, which made
        `bytes` an exact function of `rung` and left the transport model with ten data points to
        fit. `docs/measurements/p1-transaction-anatomy.md` §6 is the measurement.

        The generator now cuts each rung into `pipe_abr_*.ts` plus `pipe_abr_*_01.ts` .. `_05.ts`
        and this cycles through them, so a rung delivers six different sizes. Segment 0 keeps the
        unsuffixed name, which is what lets an OLD pack keep working: if no `_01` exists the list
        is one long and the behaviour is exactly what it was.
        """
        chosen = self._abr_segment_parts(seg)
        if chosen is None:
            return None
        parts, sequence = chosen
        return parts[sequence % len(parts)]

    def _abr_segment_parts(self, seg):
        """`(parts, sequence)` for a segment request: the rung's cycling clip list, and N.

        The rung is the EFFECTIVE one, after the case's response profile. `None` when a
        `fixtureGeneration` names no master this server issued.
        """
        requested = seg.group(1)
        effective = self.server.abr_response_rung(requested, self._abr_generation())
        if effective is None:
            return None
        parts = self.server.abr_parts(ABR_FIXTURE[effective])
        # The sequence lives in the QUERY, which `_resolve` has already stripped off the path it
        # matched — so it is read from `self.path` here rather than from a capture group.
        found = RE_SEQUENCE.search(self._query())
        return parts, int(found.group(1)) if found else 0

    def _query(self):
        return self.path.split("?", 1)[1] if "?" in self.path else ""

    def _abr_continuous(self):
        """Whether this request carries `continuous=1`: timestamps that run on across segments."""
        return bool(RE_ABR_CONTINUOUS.search(self._query()))

    def _abr_generation(self):
        found = RE_ABR_GENERATION.search(self._query())
        return int(found.group(1)) if found else None

    def _fail(self, code, why):
        self.server.note(f"-> {code} {why} ({self.path})")
        body = f"{code} {why}\n".encode()
        self.send_response(code)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)
        self.close_connection = True

    def do_HEAD(self):
        self._serve(body=False)

    def do_GET(self):
        self._serve(body=True)

    def _abr_variant(self, requested, base="", continuous=False):
        """The `#EXT-X-STREAM-INF` entry a fresh master for `requested` declares, two lines.

        One allocation of a response generation per call (`begin_abr_response`), so the entry's
        BANDWIDTH/RESOLUTION and the `fixtureGeneration` its child URI carries are the rendition
        the case's response profile maps this request to, and nothing else. `base` prefixes the
        child URI: empty for the single-rung master, whose playlist sits beside it, and the rung's
        own directory for the ladder master, whose playlists do not. `continuous` appends
        `continuous=1` after the generation, so the flag rides master -> media -> segment.
        """
        generation, effective = self.server.begin_abr_response(requested)
        params = []
        if generation is not None:
            params.append(f"fixtureGeneration={generation}")
        if continuous:
            params.append("continuous=1")
        child = f"{base}media.m3u8" + (f"?{'&'.join(params)}" if params else "")
        return (f"#EXT-X-STREAM-INF:BANDWIDTH={int(effective) * 1000},"
                f"RESOLUTION={ABR_RASTER[effective]}\n"
                f"{child}\n")

    def _abr_playlist(self):
        target = self.path.split("?", 1)[0].split("#", 1)[0]
        if RE_ABR_LADDER.match(target):
            # The ladder exists for a player that fetches the ladder itself, and such a player
            # places segments by their timestamps, so its children are ALWAYS the continuous
            # ones. `top=` lists only the rungs up to it.
            try:
                rungs = ladder_rungs(self._query())
            except ValueError as why:
                raise _Refused(400, str(why))
            # Absolute child paths: a relative `media.m3u8` would resolve under `/__abr/ladder/`,
            # which holds nothing. Each entry is exactly what that rung's own master declares, so
            # a response profile composes per rung.
            variants = "".join(self._abr_variant(rung, f"/__abr/{rung}/", continuous=True)
                               for rung in rungs)
            return ("#EXTM3U\n#EXT-X-VERSION:3\n" + variants).encode("utf-8")
        match = RE_ABR_PLAYLIST.match(target)
        if not match:
            return None
        requested, kind = match.groups()
        continuous = self._abr_continuous()
        if kind == "master":
            text = "#EXTM3U\n#EXT-X-VERSION:3\n" + self._abr_variant(requested, continuous=continuous)
        else:
            generation = self._abr_generation()
            effective = self.server.abr_response_rung(requested, generation)
            if effective is None:
                return None
            suffix = "" if generation is None else f"&fixtureGeneration={generation}"
            if continuous:
                suffix += "&continuous=1"
            if continuous:
                # The MEASURED duration of the part each sequence serves, because the media's
                # timestamps advance by exactly that and a playlist that says otherwise disagrees
                # with it. TARGETDURATION is the longest one rounded, as the HLS spec defines it.
                ticks = self._abr_part_ticks(effective)
                lengths = [f"{each / TICKS_PER_S:.6f}" for each in ticks]
                target = max(round(each / TICKS_PER_S) for each in ticks)
            else:
                lengths, target = ["2.0"], 2     # what the generator cuts, and what this always said
            rows = ["#EXTM3U", "#EXT-X-VERSION:3", f"#EXT-X-TARGETDURATION:{target}",
                    "#EXT-X-MEDIA-SEQUENCE:0"]
            for sequence in range(90):
                # Each backing file is an independent MPEG-TS program with an IDR and in-band
                # SPS/PPS at its head, matching the measured PMS segment contract.
                rows.extend((f"#EXTINF:{lengths[sequence % len(lengths)]},",
                             f"segment.ts?sequence={sequence}{suffix}"))
            rows.append("#EXT-X-ENDLIST")
            text = "\n".join(rows) + "\n"
        return text.encode("utf-8")

    def _abr_part_ticks(self, rung):
        """The measured duration of each of `rung`'s parts, in 90 kHz ticks, in cycling order."""
        try:
            return [self.server.abr_timeline(part).duration
                    for part in self.server.abr_parts(ABR_FIXTURE[rung])]
        except OSError as why:
            self.server.note(f"!! rung {rung}'s clip is missing for a continuous playlist: {why}")
            raise _Refused(404, f"rung {rung} has no clip here")
        except ValueError as why:
            self.server.note(f"!! cannot measure rung {rung}'s parts for a continuous playlist: {why}")
            raise _Refused(500, f"rung {rung}'s clip cannot be measured: {why}")

    def _continuous_body(self, path):
        """The whole body of a flagged ABR segment at `path`, timestamps moved on, or None if unflagged.

        Built per request from the file's cached timestamp positions: the clip read into memory
        (a few MB) and a few hundred fields patched, not re-parsed. Offset is the measured duration
        of every earlier sequence, walking the SAME parts list `_abr_segment_file` cycles.
        """
        seg = RE_ABR_SEGMENT.match(self.path.split("?", 1)[0])
        if not seg or not self._abr_continuous():
            return None
        parts, sequence = self._abr_segment_parts(seg)
        offset = self.server.abr_continuous_offset(parts, sequence)
        with open(path, "rb") as f:
            return shift_ts(f.read(), self.server.abr_timeline(parts[sequence % len(parts)]), offset)

    def _serve(self, body):
        try:
            playlist = self._abr_playlist()
        except _Refused as refusal:
            return self._fail(refusal.code, refusal.why)
        if playlist is not None:
            self.send_response(200)
            self.send_header("Content-Type", "application/vnd.apple.mpegurl")
            self.send_header("Content-Length", str(len(playlist)))
            self.end_headers()
            if body:
                self.server.count(False)
                self.server.write_body(self.wfile, playlist)
            return
        path = self._resolve()
        if path is None:
            return self._fail(404, "Not Found")
        size = os.path.getsize(path)
        start, end = 0, size - 1
        partial = False
        raw = self.headers.get("Range")
        if raw:
            m = RE_RANGE.match(raw.strip())
            if not m:
                # 416 rather than a silent 200: an unsatisfiable range that answers 200 is exactly
                # the corruption this whole file exists to prevent, and a loud refusal is the only
                # honest answer when we cannot tell what was asked for.
                return self._fail(416, f"Requested Range Not Satisfiable ({raw!r})")
            start = int(m.group(1))
            if m.group(2):
                end = min(int(m.group(2)), size - 1)
            if start >= size:
                return self._fail(416, f"Requested Range Not Satisfiable (start {start} >= {size})")
            partial = True
        length = end - start + 1
        shifted = None
        if body:
            # Before the status line: a clip this cannot measure must be a 500, not a 200 that dies.
            try:
                shifted = self._continuous_body(path)
            except (OSError, ValueError) as why:
                return self._fail(500, f"cannot shift timestamps: {why}")
        self.send_response(206 if partial else 200)
        self.send_header("Content-Type", "video/x-matroska")
        # §5: the length of THIS body. On a 206 that is size-start, never size.
        self.send_header("Content-Length", str(length))
        self.send_header("Accept-Ranges", "bytes")
        if partial:
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.end_headers()
        if not body:
            return
        self.server.count(partial)
        # The request-indexed rate is resolved ONCE, here, from this response's own segment index —
        # so the whole body runs at one rate even if the schedule would move under it. Only media
        # segments are counted and only they can be overridden; a playlist carries no media.
        rate_override = None
        segment = RE_ABR_SEGMENT.match(self.path.split("?", 1)[0])
        if segment:
            self.server.count_rung_segment(segment.group(1))
            rate_override = self.server.segment_rate_kbps(self.server.count_segment())
        try:
            with (open(path, "rb") if shifted is None else io.BytesIO(shifted)) as f:
                f.seek(start)
                left = length
                while left > 0:
                    chunk = f.read(min(self.server.chunk_size(), left))
                    if not chunk:
                        break
                    self.server.write_body(self.wfile, chunk, rate_override)
                    left -= len(chunk)
        except (BrokenPipeError, ConnectionResetError):
            # The demuxer closes mid-body on every seek and on teardown. That is the protocol here,
            # not an error, and printing a traceback for it would bury the real ones.
            self.server.note(f"client closed mid-body ({os.path.basename(path)} @{start})")


class FixtureServer(socketserver.ThreadingTCPServer):
    daemon_threads = True
    # Keep-alive handlers sit in readline() until the TV RSTs. Do not wait them out on shutdown.
    block_on_close = False
    allow_reuse_address = True

    def __init__(self, root, port, sink=None, bind="0.0.0.0"):
        self.root = os.path.realpath(root)
        self.sink = sink
        self.lock = threading.Lock()
        # Two counters, not a log: every request is already narrated through `note()`, and
        # `stats()` is the only reader — it wants totals. A list of per-request tuples grew for
        # the life of the run and read as though something consulted it.
        self.n_opens = self.n_ranged = self.n_accepts = 0
        self.rate_profile = []
        self.rate_started = None
        # The REQUEST-indexed schedule, and the count it is indexed by. See `set_segment_profile`.
        self.segment_profile = []
        self.n_segments = 0
        # Media-segment responses begun, by the rung the REQUEST named: which rung of a ladder the
        # player fetched. A running total, never reset; a reader takes the difference of two
        # `segment_rungs()` snapshots, as it does for `stats()`.
        self.rung_segments = {}
        # Per-case PMS response mapping. The request path remains the actuator the app sent;
        # each fresh master gets a generation whose media/segments remain pinned to the rendition
        # that master declared. This is what lets an old underfilled session keep playing while a
        # same-actuator refresh independently returns a better picture.
        self.abr_response_profile = {}
        self.abr_response_counts = {}
        self.abr_response_generations = {}
        # The instant the shared link next falls idle. See `write_body`.
        self.link_free_at = None
        self._abr_parts = {}
        # Per clip file (relative name): its `TsTimeline`, found once. See `abr_timeline`.
        self._abr_timelines = {}
        super().__init__((bind, port), FixtureHandler)

    def abr_parts(self, base):
        """The ordered clip list for one rung: `[base, base_01, base_02, ...]`, cached.

        Discovered from disk rather than declared, so an OLD pack -- which has only the
        unsuffixed file -- yields a one-element list and behaves exactly as it did before the
        split existed. That is the whole backward-compatibility story, and it is why the
        generator gives segment 0 the unsuffixed name.
        """
        with self.lock:
            cached = self._abr_parts.get(base)
            if cached is not None:
                return cached
        stem, _, ext = base.rpartition(".")
        parts = [base]
        i = 1
        while os.path.isfile(os.path.join(self.root, f"{stem}_{i:02d}.{ext}")):
            parts.append(f"{stem}_{i:02d}.{ext}")
            i += 1
        with self.lock:
            self._abr_parts[base] = parts
        return parts

    def abr_timeline(self, rel):
        """The `TsTimeline` of the clip `rel` under the root: parsed on first use, then cached.

        ValueError when the file is not a TS this can shift; OSError when it is absent.
        """
        with self.lock:
            cached = self._abr_timelines.get(rel)
        if cached is not None:
            return cached
        with open(os.path.join(self.root, rel), "rb") as f:
            found = scan_ts(f.read())
        with self.lock:
            self._abr_timelines[rel] = found
        return found

    def abr_continuous_offset(self, parts, sequence):
        """Ticks (90 kHz) between the start of the stream and the start of `sequence`.

        The sum of the MEASURED durations of every earlier sequence, cycling `parts` the way
        `_abr_segment_file` does: whole cycles plus the parts of this one already played. Never
        `sequence * 2.0`: a part need not be 2.0 s.
        """
        durations = [self.abr_timeline(part).duration for part in parts]
        cycles, within = divmod(sequence, len(parts))
        return cycles * sum(durations) + sum(durations[:within])

    def set_network_profile(self, profile):
        """Install one case-local wall-clock rate schedule: [{until_s, kbps}, ...].

        The clock begins on the first response body, not when the harness starts the app, so SSH,
        boot and Load latency cannot consume the fast leg before the television opens the file.
        Empty restores the ordinary unshaped fixture server.
        """
        cleaned = []
        for leg in profile or []:
            until_s, kbps = float(leg["until_s"]), int(leg["kbps"])
            if until_s <= 0 or kbps <= 0 or (cleaned and until_s <= cleaned[-1][0]):
                raise ValueError(f"invalid network-profile leg: {leg!r}")
            cleaned.append((until_s, kbps))
        with self.lock:
            self.rate_profile = cleaned
            self.rate_started = None
            self.link_free_at = None

    def set_segment_profile(self, profile):
        """Install a rate schedule keyed to the MEDIA SEGMENT COUNT: [{from_segment, kbps}, ...].

        # Why a second shaper, when `set_network_profile` already shapes the link

        `network_profile` is keyed to the wall clock, and there is one behaviour it structurally
        cannot produce: a rate that falls DURING a transfer whose target was chosen from the rate
        before it. That is not a corner case — it is the only condition under which a candidate
        transfer deadline can fire at all, and the reason is worth stating because it is not
        obvious. The controller picks its downshift target from the rate it just measured, so on a
        steady link the target is by construction affordable: a rung is admitted only if its
        `expected_wire_kbps` fits the measured budget, and one segment of it therefore fetches in
        about one segment of time. A transfer only becomes unaffordable when the rate underneath
        it drops after the choice was made.

        With a wall-clock cliff, whether that happens is a PHASE relationship — the cliff has to
        land near the end of a segment fetch, so that the measurement stays high and the next fetch
        runs slow. `pipe_abr_down_collapse` produced exactly that once in three runs of the same
        case (36 156 ms of fetch against a 5 793 ms reserve), and the two runs that did not bracket
        the change it was supposed to grade. A test that enters its own state one time in three
        cannot grade anything.

        Keyed to the segment COUNT instead, the same event is exact: the controller measures
        segment `from_segment - 1` at the fast rate, decides, and its candidate warm-up IS segment
        `from_segment`, served slow. No clock, no phase, no luck.

        **Counted per media-segment RESPONSE, not per `?sequence=`**, because sequence numbering
        restarts at zero for each rung — so a candidate's warm-up is always `sequence=0` and could
        never be selected by sequence. Playlists do not count; they carry no media.

        Legs apply from `from_segment` onward, last match wins, and an empty list restores the
        ordinary behaviour. It COMPOSES with `network_profile`: this one wins where it applies,
        which is what lets a case shape the run-up on the clock and the critical fetch by index.
        """
        cleaned = []
        for leg in profile or []:
            start, kbps = int(leg["from_segment"]), int(leg["kbps"])
            if start < 0 or kbps <= 0 or (cleaned and start <= cleaned[-1][0]):
                raise ValueError(f"invalid segment-profile leg: {leg!r}")
            cleaned.append((start, kbps))
        with self.lock:
            self.segment_profile = cleaned
            self.n_segments = 0
            self.link_free_at = None

    def set_abr_response_profile(self, profile):
        """Map fresh master requests to completed renditions for one case.

        Shape: ``{"22000": ["2000", "22000"]}`` means the first fresh 22 Mbps
        session declares and serves the 2 Mbps/720p fixture, while the second and every later
        session declares and serves the real 22 Mbps/4K fixture. Request and response values are
        ladder actuators, not inferred rates. An empty profile leaves every existing case byte-for-
        byte unchanged.
        """
        if profile is None:
            profile = {}
        if not isinstance(profile, dict):
            raise ValueError(f"invalid ABR response profile: {profile!r}")
        cleaned = {}
        for requested, responses in profile.items():
            requested = str(requested)
            if requested not in ABR_FIXTURE or not isinstance(responses, list) or not responses:
                raise ValueError(f"invalid ABR response profile entry: {requested!r}: {responses!r}")
            converted = [str(response) for response in responses]
            if any(response not in ABR_FIXTURE for response in converted):
                raise ValueError(f"invalid ABR response profile entry: {requested!r}: {responses!r}")
            cleaned[requested] = tuple(converted)
        with self.lock:
            self.abr_response_profile = cleaned
            self.abr_response_counts = {}
            self.abr_response_generations = {}

    def begin_abr_response(self, requested):
        """Allocate one fresh response generation, or preserve the ordinary identity mapping."""
        with self.lock:
            responses = self.abr_response_profile.get(requested)
            if responses is None:
                return None, requested
            count = self.abr_response_counts.get(requested, 0)
            generation = count + 1
            effective = responses[min(count, len(responses) - 1)]
            self.abr_response_counts[requested] = generation
            self.abr_response_generations[(requested, generation)] = effective
            return generation, effective

    def abr_response_rung(self, requested, generation):
        """Rendition attached to one master's child resources; unknown generations fail closed."""
        if generation is None:
            return requested
        with self.lock:
            return self.abr_response_generations.get((requested, generation))

    def count_segment(self):
        """One media-segment response has begun. Returns its 0-based index."""
        with self.lock:
            index = self.n_segments
            self.n_segments += 1
            return index

    def count_rung_segment(self, rung):
        """One media-segment response has begun for the rung (`"720"`) its request path named."""
        with self.lock:
            self.rung_segments[rung] = self.rung_segments.get(rung, 0) + 1

    def segment_rungs(self):
        """Media-segment responses begun so far, `{rung: count}`, lowest rung first.

        Keyed by the rung in the request path, which is the player's choice; the rendition a
        response profile served for it is the profile's business. A separate reader from
        `stats()`, whose three values `evaluate_pipeline` zips.
        """
        with self.lock:
            return {rung: self.rung_segments[rung] for rung in sorted(self.rung_segments, key=int)}

    def segment_rate_kbps(self, index):
        """The request-indexed rate for the segment at `index`, or `None` if none applies."""
        with self.lock:
            match = None
            for start, kbps in self.segment_profile:
                if index >= start:
                    match = kbps
            return match

    def _rate_kbps(self):
        with self.lock:
            if not self.rate_profile:
                return None
            now = time.monotonic()
            if self.rate_started is None:
                self.rate_started = now
            elapsed = now - self.rate_started
            return next((kbps for until_s, kbps in self.rate_profile if elapsed < until_s),
                        self.rate_profile[-1][1])

    def rate_windows(self):
        """The injected schedule as absolute monotonic intervals: `[(start, end, kbps), ...]`.

        This is the PLANT's own account of what it did to the link, and it is the only admissible
        source for "when was the link degraded" — the harness must not infer a dip from the app's
        own observations, because a metric derived from the behaviour under test cannot grade it.

        `[]` while unshaped, or before the first response body has started the phase clock (the
        clock deliberately begins there rather than at app launch, so ssh, boot and Load latency
        cannot consume the first leg). The last leg extends to infinity, reported as `None`.
        """
        with self.lock:
            if not self.rate_profile or self.rate_started is None:
                return []
            base, out, prev = self.rate_started, [], 0.0
            for until_s, kbps in self.rate_profile:
                out.append((base + prev, base + until_s, kbps))
                prev = until_s
            start, _, kbps = out[-1]
            out[-1] = (start, None, kbps)
            return out

    def dip_windows(self):
        """The degraded intervals of the injected schedule: every leg below the fastest one.

        A profile whose legs are all equal has no dip and returns `[]` — which is the right answer
        for a flat-link case, and is reported as an absence rather than as a number.
        """
        legs = self.rate_windows()
        if not legs:
            return []
        peak = max(kbps for _, _, kbps in legs)
        return [(a, b, kbps) for a, b, kbps in legs if kbps < peak]

    def chunk_size(self):
        shaped = self._rate_kbps() is not None or bool(self.segment_profile)
        return 64 * 1024 if shaped else 262144

    def write_body(self, stream, data, rate_override=None):
        """Write one chunk, then hold the SHARED link for as long as those bytes would occupy it.

        `rate_override` is the request-indexed rate from `set_segment_profile`, resolved once when
        the response began. It WINS over the wall-clock profile: a case that uses both is saying
        "shape the run-up on the clock and this particular fetch by index", and the index is the
        more specific statement. Resolved per response rather than per chunk on purpose — the
        whole point is that one transfer runs at one rate the controller did not choose from.

        **This used to sleep per writer**, so two concurrent transfers each got the full nominal
        rate and the aggregate was N times the link. That is not a small error: it is why every
        Original-probe measurement taken on this tier was inadmissible, because a probe runs
        BESIDE the segment stream and the two together were measured at 1.89x the rate the profile
        asked for. A link is shared; a shaper that is not shared is not a link.

        The model is a serial link with no queue, expressed as a virtual clock: `link_free_at` is
        the instant the wire next falls idle, a chunk starts when the wire is free (or now, if it
        already is) and occupies it for exactly `bytes * 8 / rate` seconds. Total time is then
        total bytes over the rate no matter how the writers interleave, which is the defining
        property, and it needs no bucket depth or burst allowance -- there is no constant here to
        justify because there is no constant.

        Interleaving is at CHUNK granularity, which is what a real link does with two flows
        anyway. The rate is sampled per chunk, so a profile leg that changes mid-transfer applies
        from the next chunk rather than retroactively.
        """
        stream.write(data)
        stream.flush()
        kbps = rate_override if rate_override is not None else self._rate_kbps()
        if kbps is None:
            return
        occupancy = len(data) * 8 / (kbps * 1000.0)
        with self.lock:
            now = time.monotonic()
            # `max(now, ...)` is what stops an idle link banking credit, and what lets the clock
            # recover if a leg drops the rate while a long chunk is already in flight.
            starts = now if self.link_free_at is None else max(now, self.link_free_at)
            self.link_free_at = starts + occupancy
            release = self.link_free_at
        delay = release - time.monotonic()
        if delay > 0:
            time.sleep(delay)

    def note(self, msg):
        if self.sink:
            self.sink(msg)

    def count(self, partial):
        with self.lock:
            self.n_opens += 1
            self.n_ranged += bool(partial)

    def count_accept(self):
        with self.lock:
            self.n_accepts += 1

    def stats(self):
        """(total opens, range opens, TCP accepts).

        The harness asserts on opens/ranges: a seek case that never produced a range
        open never reached the demuxer's seek path at all, which is a different
        failure from a seek that landed in the wrong place. Accepts are a separate
        counter; keep-alive means sequential GETs share one TCP connection.
        """
        with self.lock:
            return self.n_opens, self.n_ranged, self.n_accepts


def default_root():
    """Where `make fixtures-pipeline` writes the pack, and where the harness looks for it.

    One definition, imported by `tests/run.py` — the expression lived in both files, and the
    failure mode of letting them drift is the quiet one: the harness serves one directory while
    the standalone server defaults to another. NOT inside the repo, and the generator enforces
    that from its own side by refusing an `--out` under the repo root: this repository is public
    and `.gitignore` as the only defence against committing media has been got wrong here before.
    """
    return os.path.join(
        os.environ.get("FIXTURES_OUT") or os.path.expanduser("~/plxnative-fixtures"), "pipeline")


def lan_ip(peer=None):
    """This machine's address as the TV will reach it.

    A UDP `connect` picks the interface the routing table would use without sending anything.
    `gethostbyname(gethostname())` is the obvious alternative and is wrong on macOS — it answers
    127.0.0.1 on a machine with a perfectly good LAN address, and the app cannot resolve names
    anyway (`stream.rs` takes a dotted quad only), so a loopback answer here produces a URL the
    television will never connect to.

    When `peer` is the television, connect to THAT rather than 8.8.8.8: a VPN default route
    otherwise advertises a utun address the set cannot route to, the app logs `HTTP request
    failed`, and the harness reports 0 bodies — the same shape as the macOS firewall trap.
    """
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect((peer, 9) if peer else ("8.8.8.8", 53))
        return s.getsockname()[0]
    finally:
        s.close()


def serve(root, port=0, sink=None, peer=None):
    """Start a server on its own thread. Returns (server, url_base). Port 0 picks a free one."""
    srv = FixtureServer(root, port, sink=sink)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, f"http://{lan_ip(peer)}:{srv.server_address[1]}"


def _selftest(root, port):
    """Prove the three things that are silently broken otherwise: the listener is reachable on the
    LAN address (not just loopback — the firewall trap), Range yields 206 with the right
    Content-Length, and the ranged body really starts at the requested offset."""
    import http.client
    names = sorted(f for f in os.listdir(root) if not f.startswith("."))
    if not names:
        print(f"selftest: nothing in {root} to serve", file=sys.stderr)
        return 1
    srv, base = serve(root, port)
    host = base.split("//", 1)[1]
    name = names[0]
    ok = True
    try:
        c = http.client.HTTPConnection(host, timeout=5)
        c.request("GET", f"/{name}", headers={"Connection": "close"})
        r = c.getresponse()
        whole = r.read()
        size = len(whole)
        print(f"  full   GET /{name} -> {r.status} {size} bytes")
        ok &= r.status == 200
        off = size // 2
        c = http.client.HTTPConnection(host, timeout=5)
        c.request("GET", f"/{name}", headers={"Range": f"bytes={off}-", "Connection": "close"})
        r = c.getresponse()
        part = r.read()
        clen = int(r.getheader("Content-Length", -1))
        print(f"  ranged GET /{name} bytes={off}- -> {r.status} "
              f"Content-Length={clen} got={len(part)} Content-Range={r.getheader('Content-Range')}")
        ok &= r.status == 206
        ok &= clen == size - off == len(part)
        ok &= part == whole[off:]
        if not ok:
            print("  FAIL: the ranged body is not the tail of the whole body", file=sys.stderr)
    finally:
        srv.shutdown()
    print(f"selftest: {'OK' if ok else 'FAILED'} on {base} (reachable from the TV at this address)")
    return 0 if ok else 1


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1],
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    root = default_root()
    ap.add_argument("--root", default=root,
                    help=f"directory to serve (default $FIXTURES_OUT/pipeline, i.e. {root})")
    ap.add_argument("--port", type=int, default=8020, help="TCP port (0 = pick a free one)")
    ap.add_argument("--selftest", action="store_true",
                    help="prove Range works and the LAN address is reachable, then exit")
    a = ap.parse_args()
    if not os.path.isdir(a.root):
        print(f"no such directory: {a.root}", file=sys.stderr)
        return 2
    if a.selftest:
        return _selftest(a.root, a.port)
    srv, base = serve(a.root, a.port, sink=lambda m: print(m, flush=True))
    print(f"serving {a.root}")
    print(f"  {base}/<file>   (this is the address to put in plxnative-url)")
    print("  ^C to stop")
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        srv.shutdown()
        print("\nstopped")
    return 0


if __name__ == "__main__":
    sys.exit(main())
