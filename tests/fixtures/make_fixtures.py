#!/usr/bin/env python3
"""Synthesize the media SHAPES `tests/manifest.json` needs, out of nothing but ffmpeg.

WHY THIS EXISTS. `tests/run.py` grades the player against nine symbolic *item shapes*
(`movie_h264_ac3_1080p`, `episode_hevc_4k_hdr10_eac3`, …) and `tests/manifest.local.json`
maps each to a ratingKey on whatever PMS the contributor owns. That mapping is the whole
barrier to entry: the shapes include a TrueHD default with an AC-3 sibling, a Dolby Vision
profile 8.1 base layer, a PGS bitmap subtitle track and an eight-track audio file with
English DTS at ordinal 6. Nobody has all of that lying around, and two of them (TrueHD,
Dolby Vision) have no freely-licensed example anywhere in the world. So a contributor
either owns an exotic library or cannot run the suite at all. This script removes that:
it builds every shape from lavfi sources, lays them out in two Plex-scannable trees, and
writes `fixtures.json` describing exactly what it proved about each file.

WHAT A GREEN RUN ON THIS MEDIA DOES AND DOES NOT CLAIM. These files are synthetic. They
exercise the same code paths — route decision, demux, Starfish payload, ACB bind, timeline
— but a real remux carries encoder quirks, odd GOP structures, container edge cases and
metadata this generator will never produce. Read a pass here as "no regression against the
shapes the suite names", never as "plays the world's media". `tests/README.md` makes the
same distinction about `--fps` numbers; it applies with more force here.

TWO TIERS, ONE GENERATOR (`--tier`). Everything above and below describes the INTEGRATION
pack: the two Plex-scannable trees `tests/manifest.local.json` maps shape keys into.
`--tier pipeline` builds a second, much smaller pack for the player-PIPELINE tier, which
has no Plex in it at all — flat 60 s clips served off the dev Mac by
`tests/serve_fixtures.py` and played through `/tmp/plxnative-playurl`, which carries the
URL *and* the Load payload declaration. Both packs share every builder, every trap below
and `verify()`; what differs is the table (`PIPE_SHAPES`), the on-disk layout, and the fact
that nothing in that table is derived from Plex's watched threshold, its marker detector or
a case's seek depth — because none of those exist down there. `PIPE_SHAPES`' own comment is
where that tier's shapes, and the three it deliberately cannot contain, are argued.

--------------------------------------------------------------------------------------
THE TRAPS THIS SCRIPT IS BUILT AROUND. Every one of them fails SILENTLY — the command
succeeds, the file exists, and it is the wrong shape. That is the failure mode worth
fearing here, because a wrong fixture makes the harness fail as if the PLAYER regressed.
Which is also why `verify()` is not optional: every property a shape CLAIMS is read back
out of the finished file with ffprobe, and a mismatch is a hard error naming both sides.

 1. `-t <n>` IS AN INPUT OPTION and must precede its own `-i`. Written after `-i` it binds
    to the NEXT input; written last it becomes an output option. On an unbounded lavfi
    source that means ffmpeg encodes FOREVER at 100% CPU. Every lavfi input below gets its
    own `-t` in front of it, audio graphs additionally carry `duration=` inside the filter,
    and the muxes that mix bounded and unbounded inputs (the PGS one — a `.sup` is not a
    timed input) also carry a trailing output `-t`. Measured: a 20 s video muxed with a
    60 s `.sup` produced a 58-second file.

 2. THE AUTO-INSERTED SCALER OVERWRITES color_primaries/color_trc when it converts lavfi's
    rgb24 to yuv420p10le. `-color_primaries bt2020 -color_trc smpte2084` alone yields
    `bt2020nc/unknown/unknown` and Plex will not label the file HDR. The colour stamp must
    ride `setparams` AFTER an explicit `format=` in the same filter chain, which is why
    every video chain here ends `…,format=<pixfmt>,setparams=…`.

 3. THE MATROSKA MUXER IS NOT THE ONE EATING THE COLOUR STAMP — trap 2 is. This entry
    used to read "ffmpeg's mkv muxer drops the transfer characteristic for HLG:
    `smpte2084` survives, `arib-std-b67` does not", and the conclusion drawn from it was
    that HLG must be muxed with `mkvmerge`. RE-MEASURED on the same ffmpeg 8.1.2, using
    the chain shape below: HLG SURVIVES the mkv mux — `mkvinfo` shows the container's
    `Colour transfer: 18` and ffprobe reads back `arib-std-b67`. What actually loses the
    transfer is `-color_trc` written as an OUTPUT option with no `setparams`, and that
    form loses `color_primaries` too, which is the tell: a muxer that special-cased one
    transfer value would not also drop the primaries. So nothing here needs mkvmerge for
    a colour reason, and whoever builds the `library_gaps` HLG clip needs `setparams`,
    not mkvtoolnix. The Dolby Vision shape goes through `mkvmerge` for an unrelated
    reason: ffmpeg's matroska muxer will not write the DV configuration record from a raw
    `.hevc`, and that record is precisely what the app reads back
    (`ff: … dovi=P8 bl_compat=1`).

 4. PMS EXCLUDES FILES WITH "sample" IN THE NAME under 300 MB. Nothing here is ever named
    sample*; the two raw ES intermediates are `_bl.hevc` / `_bl_rpu.hevc` inside `.work/`,
    which is deleted, and outside the library trees regardless.

 5. `testsrc` (v1) IS A NEAR-STATIC PATTERN: ~181 kbit/s at 1080p CRF18. `testsrc2` at
    identical settings is ~7,830 kbit/s — and real Creative-Commons footage measured 7,560,
    i.e. 3% BELOW testsrc2. So testsrc2 is the base here, everywhere. Bitrate is not
    cosmetic: `seek_rapid_h264` and `seek_rapid_hevc_4k` assert `coalesced=n>=1`, which
    needs a seek still resolving ~50 ms after it was issued. Against a 181 kbit/s source
    every seek in the burst completes before the next tap and nothing ever coalesces — the
    case passes its other assertions and covers the merge path zero times. So the two
    shapes carrying a rapid-seek case declare a `min_mbit` FLOOR, and `verify()` asserts
    it out of the finished file. It is a floor and not a comment because a CRF is not a
    bitrate: this file once carried a comment claiming those two shapes "get the higher
    bitrates", while the H.264 seek shape sat at exactly the same CRF as the two shapes
    with no seek case at all.

 6. `-preset ultrafast` SILENTLY FORCES H.264 CONSTRAINED BASELINE. The H.264 shapes use
    `veryfast` with an explicit `-profile:v high`. (x265 has no such coupling, so the HEVC
    shapes do use `ultrafast` — it is the difference between an 18-minute run and an hour.)

 7. lavfi SOURCES ARE rgb24. Without an explicit `format=yuv420p` libx264 picks yuv444p and
    reports "High 4:4:4 Predictive", which is not what a library file looks like and is not
    what the TV decodes.

 8. THE lavfi DEMUXER REJECTS NAMED OUTPUT PADS. A graph handed to `-f lavfi -i` must end
    on an UNLABELLED pad (or `[out0]`); a trailing `[a]` fails with "Invalid outpad name".
    The per-channel audio graphs below therefore end bare on the `join`.

 9. PLEX MARKS AN ITEM WATCHED PAST ~90% AND DROPS THE viewOffset THE HARNESS JUST SEEDED.
    `subtitle_text_srt` seeds 843 s, so its item must run past 843/0.9 = 937 s or the case
    silently becomes a play-from-zero test. Every duration below is the deepest seek /
    resume / marker depth its cases reach, divided by 0.9, then rounded up with margin.
    And nothing is ever shorter than 60 s even where the cases would allow 30: an item that
    hits EOF inside its `run_secs` fires the finish → Up Next → auto-advance chain, which
    contaminates `no_playing_error` and the teardown assertions of a case that was only
    ever about playing.

10. ffmpeg HAS A PGS DECODER AND NO PGS ENCODER, and refuses text→bitmap conversion
    outright ("Subtitle encoding currently only possible from text to text or bitmap to
    bitmap"). So this file contains a small HDMV PGS writer (`pgs_build`, below) that
    authors the `.sup` directly. It is NOT a conformant Blu-ray authoring tool — single
    window, single object, one palette, epoch-start per cue — but ffmpeg's own PGS decoder
    reads it back as `num_rects=1` per cue and 0 per clear, which is exactly the display-set
    stream `subtitle_image_pgs` grades. No external tool (SUPer, avs2bdnxml, BDSup2Sub) is
    needed.

--------------------------------------------------------------------------------------
TWO IDEAS BORROWED FROM iwalton3/stdjflib, both about making a WRONG FILE VISIBLE.

 * THE STREAM LAYOUT IS BURNED INTO THE PICTURE. `testsrc2` already draws a running
    timecode and frame counter top-left (verified — that is a property of the source, not
    of `drawtext`, which this Homebrew ffmpeg does not even have: it is built without
    libfreetype, so there is no `drawtext` and no `subtitles` filter). What testsrc2 cannot
    say is which shape you are looking at, so `banner_png()` rasterises the shape key and
    its full stream layout with a built-in 5x7 font into an RGBA PNG and `overlay` composites
    it TOP-centre. A media-source mix-up in the harness then shows up in a screen capture
    without anyone consulting a manifest. The banner text is generated FROM THE SAME
    declarative spec that `verify()` asserts, so the two cannot drift.
    TOP-centre, and not the bottom-centre it sat at until a verifier photographed the
    result: at the bottom the plate lands in the band the app draws SUBTITLES in and the
    band the PGS cues are authored in, so on the two shapes whose whole case is "is a
    subtitle on the screen" the two texts overprinted each other. The top-LEFT corner is
    testsrc2's own timecode box (measured: about 160x48 px at 1080p), so the plate starts
    at H/12, below it.

 * EVERY AUDIO CHANNEL GETS ITS OWN PITCH. A 5.1 track is six sine sources joined with
    `join=inputs=6:channel_layout=5.1:map=…`, at musically distinct frequencies (LFE two
    octaves down), so a bad downmix or a swapped channel map is AUDIBLE rather than merely
    plausible. `map=` IS LOAD-BEARING and was missing: with no map, `join` does NOT consume
    its inputs in declaration order — each mono sine's own layout is `FC`, so input 0 claims
    the CENTRE channel and the rest fill in around it. Measured on the shipped file: the
    intended FL/FR/FC/LFE/BL/BR pitches came out rotated as FC/FL/FR/LFE/BL/BR. That still
    sounds like six distinct pitches, so nothing looks wrong — it just makes the file
    useless as the channel-ORDER reference it is documented to be, and `verify()` asserts
    channel COUNT, which cannot see it.

--------------------------------------------------------------------------------------
ORDERING IS PART OF THE SPEC — the cases assert track POSITIONS, not just presence.
`tests/README.md`: audio-tab row = the metadata audio index in file order; subtitle-tab
row 0 is *Off* and row r is subtitle index r-1. So:

 * `audio_switch_transcode` picks audio row 6 on `movie_h264_ac3_many_audio` and expects a
   transcode, so index 6 is the ENGLISH DTS track (DTS is outside the direct-play set).
 * `audio_switch_native` picks audio row 0 on `episode_hevc_4k_hdr10_eac3` and expects a
   NATIVE switch, and its title says "foreign default + eng … eng auto-picked at start".
   So index 0 is a German E-AC-3 track carrying the default disposition and index 1 is the
   English one: the route auto-picks English at start, row 0 switches to the German track,
   and both being E-AC-3 keeps the switch native.
 * `subtitle_text_srt` picks subtitle row 3 = index 2 and expects English cues, so the four
   text tracks are ordered [rus forced, rus, eng, eng-SDH] — the same order as the real
   library item the case was written against.
 * `subtitle_image_pgs` picks subtitle row 1 = index 0, so the PGS track is index 0.

WHAT THIS DOES NOT SOLVE, stated here rather than discovered later: intro/credits MARKERS.
Plex's detector needs Plex Pass, ignores intros under 20 s, and will not detect an intro
that ends past the halfway point. The episode pair does carry an identical 130 s intro
segment over a 300 s episode (on by default; `--no-markers` turns it off), plus a 40 s
black credits tail, placed to satisfy both constraints — but whether Plex's analyser fires
on synthetic content is not something this script can assert, and the three `marker_*`
cases stay a real-library concern. See README.md.

WHICH MODALITY THE INTRO HAS TO BE IDENTICAL IN, because the first version of this got it
exactly backwards and shipped: Plex fingerprints AUDIO and looks for the stretch that
MATCHES between episodes of a season. The original pair differed only by a `hue` filter,
i.e. in video, and its audio was one constant unmodulated tone from end to end — so the
whole 300 s matched, the candidate ran past the halfway point, and there was nothing for a
detector to bound. Meanwhile the intro WINDOW was the one stretch that differed, because
the burned-in layout banner reads `S01E01` vs `S01E02` inside it. Both halves are now the
other way round: the audio is a shared 3-tone intro melody and then a per-episode pitch
after `intro_end` (and a distinct low bed under the credits tail), and the layout banner is
only drawn AFTER `intro_end`, so the intro window really is frame-identical. `verify_pair`
asserts all three out of the finished files rather than trusting the filter graph.
"""

import argparse
import json
import os
import re
import shutil
import struct
import subprocess
import sys
import time
import zlib
from pathlib import Path

GEN_VERSION = "1.0.0"
FPS = 24


def shape_fps(spec):
    """This shape's frame rate. `FPS` (24) unless `video.fps` says otherwise.

    An axis rather than a constant since 2026-08-22, and the reason is a gap nothing else could
    see: every fixture in both packs ran at 24p while the television's own capability table
    (`/etc/umediaserver/device_codec_capability_config.json`) claims 60 for H.264 and 120 for
    H.265 — so the rate the Load payload declares (`esInfo videoFps`, built by
    `engine.rs::fps_rational`) had exactly one value in the whole suite. `fps_rational` has
    distinct branches for the integer rates and for the 1001-denominator broadcast ones, and
    neither had ever been reached with anything but 24.

    Note what does NOT vary with it: the GOP stays 48 frames (`venc_args`, `X265_GOP`). At 60p
    that is 0.8 s rather than 2 s, which is denser than a real file — fine here, and better for
    a seek case, but it is why a 60p fixture is not a size model for real 60p content.
    """
    return spec.get("video", {}).get("fps", FPS)


def _rate_arg(fps):
    """A frame rate as ffmpeg/mkvmerge want it: an exact rational for the 1001-denominator
    broadcast rates, a plain number otherwise.

    Passing 59.94 as a decimal is the trap: lavfi parses it, rounds, and emits 60 — so the clip
    that exists to exercise the 60000/1001 path would be built at the integer rate and pass every
    check, because `verify()` compares the DECLARED fps against the file and both would say 60.
    """
    for exact in (23.976, 29.97, 47.952, 59.94, 119.88):
        if abs(fps - exact) < 0.02:
            return "%d/1001" % round(exact * 1001)
    return "%g" % fps
REPO_ROOT = Path(__file__).resolve().parents[2]

# ---------------------------------------------------------------------------------------
# A 5x7 bitmap font. Five bits per row, bit 4 leftmost. Uppercase only by design: the
# banner is read at a glance off a 1080p screen capture, and mixed case at this cell size
# is a smear. Unknown characters render as a space rather than raising.
# ---------------------------------------------------------------------------------------
FONT = {
    'A': (0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11),
    'B': (0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E),
    'C': (0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E),
    'D': (0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E),
    'E': (0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F),
    'F': (0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10),
    'G': (0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F),
    'H': (0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11),
    'I': (0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E),
    'J': (0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C),
    'K': (0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11),
    'L': (0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F),
    'M': (0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11),
    'N': (0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11),
    'O': (0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E),
    'P': (0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10),
    'Q': (0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D),
    'R': (0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11),
    'S': (0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E),
    'T': (0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04),
    'U': (0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E),
    'V': (0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04),
    'W': (0x11, 0x11, 0x11, 0x15, 0x15, 0x1B, 0x11),
    'X': (0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11),
    'Y': (0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04),
    'Z': (0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F),
    '0': (0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E),
    '1': (0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E),
    '2': (0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F),
    '3': (0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E),
    '4': (0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02),
    '5': (0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E),
    '6': (0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E),
    '7': (0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08),
    '8': (0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E),
    '9': (0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C),
    ' ': (0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00),
    '.': (0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C),
    ',': (0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C, 0x08),
    '-': (0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00),
    '_': (0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1F),
    ':': (0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00),
    '/': (0x01, 0x02, 0x02, 0x04, 0x08, 0x08, 0x10),
    '+': (0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00),
    '(': (0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02),
    ')': (0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08),
    '#': (0x0A, 0x0A, 0x1F, 0x0A, 0x1F, 0x0A, 0x0A),
    '=': (0x00, 0x00, 0x1F, 0x00, 0x1F, 0x00, 0x00),
    '*': (0x00, 0x15, 0x0E, 0x1F, 0x0E, 0x15, 0x00),
}
GLYPH_W, GLYPH_H = 5, 7


def _png(width, height, rgba_rows):
    """Write an RGBA PNG with the stdlib only (zlib + struct). No PIL, no pip."""
    raw = b"".join(b"\x00" + bytes(r) for r in rgba_rows)

    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9))
            + chunk(b"IEND", b""))


def banner_png(path, lines, scale):
    """Rasterise the shape's stream layout to an RGBA PNG for `overlay`.

    White text on a translucent black plate. Drawn at `scale` so the banner is legible at
    both 1080p and 4K without the encoder ever seeing a resampled bitmap.
    """
    lines = [ln.upper() for ln in lines]
    cell_w = (GLYPH_W + 1) * scale
    line_h = (GLYPH_H + 3) * scale
    pad = 4 * scale
    cols = max(len(ln) for ln in lines)
    w = cols * cell_w + 2 * pad
    h = len(lines) * line_h + 2 * pad
    # plate: black at 60% — dark enough to read against testsrc2's saturated bars, light
    # enough that the encoder still spends bits on the moving picture underneath.
    rows = [bytearray(b"\x00\x00\x00\x99" * w) for _ in range(h)]
    for li, line in enumerate(lines):
        oy = pad + li * line_h + scale
        for ci, ch in enumerate(line):
            g = FONT.get(ch, FONT[' '])
            ox = pad + ci * cell_w
            for gy in range(GLYPH_H):
                bits = g[gy]
                for gx in range(GLYPH_W):
                    if not (bits >> (GLYPH_W - 1 - gx)) & 1:
                        continue
                    for sy in range(scale):
                        row = rows[oy + gy * scale + sy]
                        base = (ox + gx * scale) * 4
                        for sx in range(scale):
                            o = base + sx * 4
                            row[o:o + 4] = b"\xff\xff\xff\xff"
    path.write_bytes(_png(w, h, rows))
    return path


# ---------------------------------------------------------------------------------------
# Text subtitles. Self-describing: every cue names its own track index, language and
# timestamp, so a screen capture says which track is selected without a manifest.
# ---------------------------------------------------------------------------------------
SUB_STEP, SUB_HOLD = 10, 8


def sub_cue_times(duration):
    """The cue schedule, shared by the SRT writer, the PGS writer and verify().

    One function rather than three copies of `range(0, dur - hold, step)`, because what
    verify() has to assert about a subtitle track is not "it has cues" but "it has cues
    WHERE THE CASE PLAYS" — `subtitle_text_srt` seeds 843 s and `subtitle_image_pgs` seeds
    600 s, and a track whose only cue is at t=0 satisfies every other check in this file
    while failing on the television as `no sub cue`, which reads as a demuxer regression.
    """
    cues = list(range(0, max(0, int(duration) - SUB_HOLD), SUB_STEP))
    # A clip shorter than one cue's hold has to BUILD anyway. `--secs 8` or less made this
    # schedule EMPTY, `write_srt` then wrote a zero-byte .srt, and ffmpeg refused it as an
    # input — so the three subtitle-bearing shapes died with `Invalid data found when
    # processing input`, naming a file in .work rather than the length that caused it.
    # (Unreachable before `--secs`: `--quick` was pinned at 20 s.) One cue at t=0 keeps the
    # media well-formed at any length, and because verify() reads this same schedule it then
    # grades exactly what was written. Such a clip is development media either way — the
    # cue-coverage rule this function exists for only means anything at declared length.
    return cues or ([0] if int(duration) >= 1 else [])


def write_srt(path, idx, label, duration, step=SUB_STEP, hold=SUB_HOLD):
    def ts(t):
        return "%02d:%02d:%02d,000" % (t // 3600, t % 3600 // 60, t % 60)

    out = []
    for n, t in enumerate(sub_cue_times(duration), start=1):
        # The hold is CLAMPED into the clip. At declared length this is a no-op (the schedule
        # never places a cue later than dur - hold), but the floor cue that keeps a very short
        # `--secs` build alive would otherwise end past EOF — and a cue ending at 8 s inside a
        # 2 s clip sets the MATROSKA DURATION to 8 s, which verify() then reports as a
        # duration mismatch on a file whose video is exactly the length that was asked for.
        end = t + max(1, min(hold, int(duration) - t))
        out.append("%d\n%s --> %s\nS%d %s @ %ds\n" % (n, ts(t), ts(end), idx, label, t))
    path.write_text("\n".join(out), encoding="utf-8")
    return path


# ---------------------------------------------------------------------------------------
# HDMV PGS (.sup) writer — see trap 10. Segment framing is
#   'PG' | pts:u32be | dts:u32be | type:u8 | size:u16be | payload
# with 0x14 PDS, 0x15 ODS, 0x16 PCS, 0x17 WDS, 0x80 END. One display set opens each cue
# and a composition-state-0 set with no objects clears it, which is what makes ffmpeg's
# decoder report num_rects 1 then 0 — the shape `subtitle_image_pgs` grades.
# The authoring canvas stays 1920x1080 even for a 4K video, exactly as a UHD disc does;
# the app scales from the canvas it is told about (`image cue … canvas=WxH`).
# ---------------------------------------------------------------------------------------
def _pgs_render(text, scale=6, pad=12):
    cw, ch, gap = GLYPH_W, GLYPH_H, 1
    tw = len(text) * (cw + gap) * scale
    w, h = tw + 2 * pad, ch * scale + 2 * pad
    rows = [[2] * w for _ in range(h)]          # palette index 2 = translucent plate
    for ci, c in enumerate(text.upper()):
        g = FONT.get(c, FONT[' '])
        ox = pad + ci * (cw + gap) * scale
        for gy in range(ch):
            for gx in range(cw):
                if (g[gy] >> (cw - 1 - gx)) & 1:
                    for sy in range(scale):
                        r = rows[pad + gy * scale + sy]
                        for sx in range(scale):
                            r[ox + gx * scale + sx] = 1
    return w, h, rows


def _pgs_rle(w, h, rows):
    out = bytearray()
    for y in range(h):
        row, x = rows[y], 0
        while x < w:
            c, n = row[x], 1
            while x + n < w and row[x + n] == c and n < 16383:
                n += 1
            if c == 0:
                out += bytes([0x00, n]) if n <= 63 else bytes([0x00, 0x40 | (n >> 8), n & 0xFF])
            elif n <= 63:
                out += bytes([c]) * n if n <= 2 else bytes([0x00, 0x80 | n, c])
            else:
                out += bytes([0x00, 0xC0 | (n >> 8), n & 0xFF, c])
            x += n
        out += b"\x00\x00"
    return bytes(out)


def _pgs_seg(pts90, stype, payload):
    return b"PG" + struct.pack(">IIBH", pts90 & 0xFFFFFFFF, 0, stype, len(payload)) + payload


def pgs_build(path, duration, canvas=(1920, 1080), step=SUB_STEP, hold=SUB_HOLD):
    vw, vh = canvas
    out = bytearray()
    comp = 0
    n = 0
    for t in sub_cue_times(duration):
        n += 1
        w, h, rows = _pgs_render("PGS S0 ENG @%ds" % t)
        rle = _pgs_rle(w, h, rows)
        x = max(0, min((vw - w) // 2, vw - w))
        # 80% down the canvas, where a real subtitle sits. Safe only because the layout
        # banner moved to the TOP band: while the banner was bottom-centre the two
        # overlapped exactly, on the one shape whose case is about reading a bitmap
        # subtitle off the screen.
        y = max(0, min(int(vh * 0.80) - h // 2, vh - h))
        pcs = struct.pack(">HHBHBBBB", vw, vh, 0x10, comp, 0x80, 0x00, 0, 1) \
            + struct.pack(">HBBHH", 0, 0, 0x00, x, y)
        wds = struct.pack(">BBHHHH", 1, 0, x, y, w, h)
        pal = {0: (16, 128, 128, 0), 1: (235, 128, 128, 255), 2: (16, 128, 128, 160)}
        pds = struct.pack(">BB", 0, 0)
        for idx, (Y, Cr, Cb, A) in pal.items():
            pds += struct.pack(">BBBBB", idx, Y, Cr, Cb, A)
        body = struct.pack(">HH", w, h) + rle
        ods = struct.pack(">HBB", 0, 0, 0xC0) + struct.pack(">I", len(body))[1:] + body
        pts = int(t * 90000)
        out += _pgs_seg(pts, 0x16, pcs) + _pgs_seg(pts, 0x17, wds)
        out += _pgs_seg(pts, 0x14, pds) + _pgs_seg(pts, 0x15, ods) + _pgs_seg(pts, 0x80, b"")
        comp += 1
        # Clamped into the clip, for the reason write_srt() gives: at declared length this is
        # the hold unchanged, and on a floor-cue build it keeps the clear inside the file.
        pts_end = int((t + max(1, min(hold, int(duration) - t))) * 90000)
        clear = struct.pack(">HHBHBBBB", vw, vh, 0x10, comp, 0x00, 0x00, 0, 0)
        out += _pgs_seg(pts_end, 0x16, clear) + _pgs_seg(pts_end, 0x17, wds) + _pgs_seg(pts_end, 0x80, b"")
        comp += 1
    path.write_bytes(bytes(out))
    return n


# ---------------------------------------------------------------------------------------
# Process plumbing
# ---------------------------------------------------------------------------------------
class Fail(Exception):
    pass


def run(argv, quiet=True):
    p = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if p.returncode != 0:
        tail = p.stderr.decode("utf-8", "replace").strip().splitlines()[-20:]
        raise Fail("%s failed (rc=%d)\n  %s\n  %s"
                   % (Path(argv[0]).name, p.returncode, " ".join(argv[:14]) + " …",
                      "\n  ".join(tail) or "(no stderr)"))
    return p.stdout.decode("utf-8", "replace")


def probe(path, extra=None):
    argv = ["ffprobe", "-v", "error", "-show_format", "-show_streams", "-of", "json"]
    argv += extra or []
    argv += [str(path)]
    return json.loads(run(argv))


def first_frame_side_data(path):
    """Side-data types on the first video frame — how in-band HDR10 SEI is proven.

    Mastering-display and content-light metadata written by x265 (`repeat-headers=1`) live
    in the BITSTREAM, not in a container element, so `-show_streams` reports nothing at all
    for them. Only decoding a frame surfaces them; this is the difference between "we asked
    for HDR10" and "the file carries it".
    """
    try:
        j = json.loads(run(["ffprobe", "-v", "error", "-select_streams", "v:0",
                            "-read_intervals", "%+#1", "-show_frames", "-of", "json", str(path)]))
        return [sd.get("side_data_type", "") for sd in (j["frames"][0].get("side_data_list") or [])]
    except Exception:
        return []


def tool_version(name, args=None):
    exe = shutil.which(name)
    if not exe:
        return None
    try:
        out = run([exe] + (args or ["--version"]))
    except Fail:
        return "present"
    return out.strip().splitlines()[0] if out.strip() else "present"


def ffmpeg_encoders():
    try:
        out = run(["ffmpeg", "-hide_banner", "-encoders"])
    except (Fail, FileNotFoundError):
        return set()
    names = set()
    for line in out.splitlines():
        m = re.match(r"^\s[VAS][\.A-Z]{5}\s+(\S+)", line)
        if m:
            names.add(m.group(1))
    return names


# ---------------------------------------------------------------------------------------
# lavfi source construction
# ---------------------------------------------------------------------------------------
# Musically distinct ratios so a swapped channel or a bad downmix is AUDIBLE. LFE sits two
# octaves below the front-left so it is unmistakable even through a phone speaker.
CH_RATIOS = {2: (1.0, 1.5), 6: (1.0, 1.26, 1.5, 0.25, 2.0, 2.52)}
CH_LAYOUT = {2: "stereo", 6: "5.1"}
# join's `map=` — see the module docstring. Without it join fills the output layout in its
# own order (every mono input's layout is FC, so input 0 lands on CENTRE), which rotates
# the channel map silently.
CH_MAP = {2: "0.0-FL|1.0-FR",
          6: "0.0-FL|1.0-FR|2.0-FC|3.0-LFE|4.0-BL|5.0-BR"}


def audio_graph(channels, base_hz, duration, segs=None):
    """A lavfi graph: one sine per channel, joined into a real multichannel layout.

    NB the graph ends on the bare `join` — the lavfi DEMUXER rejects a named output pad
    ("Invalid outpad name 'a'"), which is the one way this differs from a `-filter_complex`
    written for the same purpose. `duration=` inside each sine is what actually bounds the
    source; the `-t` in front of `-i` is the second belt (see trap 1).

    `segs` is an optional list of `(pitch_multiplier, seconds)` spliced end to end with
    `concat`, which is how the marker-bearing episodes get an audio track with STRUCTURE:
    a shared intro melody, a per-episode body pitch, a distinct credits bed. Plex's marker
    detector fingerprints audio, so this — not the picture — is the modality the intro has
    to be identical in and the body has to differ in.
    """
    ratios = CH_RATIOS[channels]
    segs = segs or [(1.0, duration)]
    parts = []
    for i, r in enumerate(ratios):
        if len(segs) == 1:
            parts.append("sine=frequency=%d:sample_rate=48000:duration=%s[c%d]"
                         % (round(base_hz * r * segs[0][0]), _g(segs[0][1]), i))
            continue
        labs = []
        for j, (mult, secs) in enumerate(segs):
            labs.append("c%ds%d" % (i, j))
            parts.append("sine=frequency=%d:sample_rate=48000:duration=%s[%s]"
                         % (round(base_hz * r * mult), _g(secs), labs[-1]))
        parts.append("".join("[%s]" % x for x in labs)
                     + "concat=n=%d:v=0:a=1[c%d]" % (len(segs), i))
    parts.append("".join("[c%d]" % i for i in range(channels))
                 + "join=inputs=%d:channel_layout=%s:map=%s"
                 % (channels, CH_LAYOUT[channels], CH_MAP[channels]))
    return ";".join(parts)


def _g(x):
    """A number for a filter argument, without a trailing `.0` that reads as sloppiness."""
    return ("%g" % float(x))


X265_MD = ("master-display=G(8500,39850)B(6550,2300)R(35400,14600)"
           "WP(15635,16450)L(10000000,50):max-cll=1000,400")
X265_HDR = ("hdr10=1:repeat-headers=1:" + X265_MD +
            ":colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:range=limited")
X265_GOP = "keyint=48:min-keyint=48:scenecut=0:log-level=none"

CHAIN_SDR = "format=yuv420p,setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv"
CHAIN_PQ = ("format=yuv420p10le,setparams=color_primaries=bt2020:color_trc=smpte2084"
            ":colorspace=bt2020nc:range=tv")


def venc_args(v):
    c = v["codec"]
    if c == "h264":
        # veryfast, NOT ultrafast: ultrafast silently forces Constrained Baseline (trap 6).
        return ["-c:v", "libx264", "-profile:v", "high", "-level", "4.0",
                "-preset", "veryfast", "-crf", str(v["crf"]),
                "-g", "48", "-keyint_min", "48", "-sc_threshold", "0"]
    if c == "hevc":
        xp = (X265_HDR + ":" + X265_GOP) if v.get("hdr") else X265_GOP
        if v.get("x265_extra"):
            xp += ":" + v["x265_extra"]
        a = ["-c:v", "libx265", "-profile:v", "main10" if v.get("hdr") else "main",
             "-preset", "ultrafast", "-crf", str(v["crf"]), "-x265-params", xp]
        if v.get("tag"):
            a += ["-tag:v", v["tag"]]
        return a
    if c == "av1":
        return ["-c:v", "libsvtav1", "-preset", "12", "-crf", str(v["crf"]), "-g", "48"]
    raise Fail("no encoder recipe for video codec %r" % c)


AENC = {
    "ac3": ["ac3"], "eac3": ["eac3"], "aac": ["aac"], "truehd": ["truehd"],
    "dts": ["dca"], "vorbis": ["vorbis"], "opus": ["libopus"], "flac": ["flac"],
}


# ---------------------------------------------------------------------------------------
# Human-readable stream layout — burned into the picture, written into the container's
# metadata, and recorded in fixtures.json. Derived from the SAME spec that verify()
# asserts, so the burn-in cannot drift away from what the file actually is.
# ---------------------------------------------------------------------------------------
def layout_lines(key, spec, dur, ep=None):
    v = spec["video"]
    name = key if ep is None else "%s S01E%02d" % (key, ep)
    lines = ["PLXTEST FIXTURE * " + name.replace("_", " ")]
    # The Dolby Vision shape's picture is a plain HDR10 base layer, so without this line
    # its burn-in was byte-identical to the two HDR10 shapes' — on the one file where a
    # mix-up is least recoverable from the picture itself.
    hdr = "SDR BT709"
    if spec.get("dovi"):
        hdr = "DOVI P8.1 (HDR10 BL) PQ BT2020"
    elif v.get("hdr"):
        hdr = "HDR10 PQ BT2020"
    lines.append("V0 %s %s %s %gFPS %ds" % (v["codec"].upper(), v["size"], hdr,
                                            v.get("fps", FPS), dur))
    for i, a in enumerate(spec.get("audio", [])):
        lines.append("A%d %s %s %s%s"
                     % (i, a["codec"].upper(), CH_LAYOUT[a["ch"]].upper(), a["lang"].upper(),
                        " DEFAULT" if a.get("default") else ""))
    for i, s in enumerate(spec.get("subs", [])):
        lines.append("S%d %s %s%s%s"
                     % (i, s["codec"].upper(), s["lang"].upper(),
                        " FORCED" if s.get("forced") else "",
                        " SIDECAR" if s.get("sidecar") else ""))
    lines.append("SHAPE KEY: " + (spec["extra_key"] if ep == 2 and spec.get("extra_key") else key))
    return lines


def fit_scale(lines, vw, vh):
    """Pick the largest font scale whose banner stays under 90% width and 28% height."""
    cols = max(len(x) for x in lines)
    for scale in range(max(2, vh // 300), 0, -1):
        w = cols * (GLYPH_W + 1) * scale + 8 * scale
        h = len(lines) * (GLYPH_H + 3) * scale + 8 * scale
        if w <= vw * 0.90 and h <= vh * 0.28:
            return scale
    return 1


# =======================================================================================
# THE SHAPE TABLE
#
# `duration` is the deepest depth the shape's cases reach (setup.viewOffset_ms, seek
# target_s/final_s, resume offset_s, skip min_pos_after_s + min_climb_after_s, expect.
# min_timeline_climb_s), divided by 0.9 for Plex's watched threshold (trap 9), rounded up
# with margin, and floored at 1.5x the longest `run_secs` of any case that names the shape
# — never at 60 flat, which is what four shapes carried while their cases capped at exactly
# `run_secs: 60`. Zero margin is invisible on the PASSING path (the harness ends a case the
# moment its assertions are satisfied) and stacks a spurious finish -> Up Next -> teardown
# failure on top of a real one whenever a case FAILS or runs under `--no-early`, which is
# the hardest kind of harness output to read. `rate` is encode-seconds per second of
# output measured on an Apple-silicon Mac with this Homebrew ffmpeg — it exists only to
# print an honest ETA, and it is the number to re-measure if the ETA drifts.
#
# `crf` is chosen per shape against the size budget: the whole set is ~2.5 GB. The two
# shapes carrying a rapid-seek case (movie_h264_ac3_1080p, episode_hevc_4k_hdr10_eac3)
# additionally declare `min_mbit`, a measured floor verify() reads back out of the finished
# file — because seek COALESCING is what those cases grade and a thin stream never
# coalesces (trap 5). This comment used to claim those two "get the higher bitrates" while
# the H.264 one sat at the same CRF as the two shapes with no seek case at all; a CRF is
# not a bitrate, which is why the floor is asserted now instead of narrated.
# =======================================================================================
SHAPES = {
    "movie_h264_ac3_1080p": {
        "kind": "movie", "library_title": "PlxTest H264 AC3 1080p (2001)", "ext": "mkv",
        "duration": 1080, "rate": 0.06, "min_mbit": 5.5,
        # 1080 s because subtitle_text_srt seeds viewOffset 843 s: 843/0.9 = 937 s floor.
        # crf 20, not the 25 this shape shipped with: at 25 it measured 3.7 Mbit/s of
        # video, half the testsrc2 reference trap 5 is written against, on the shape that
        # carries BOTH rapid-seek cases' H.264 half.
        "video": {"codec": "h264", "size": "1920x1080", "crf": 20},
        "audio": [{"codec": "ac3", "ch": 6, "lang": "eng", "br": "448k", "pitch": 220,
                   "default": True, "title": "AC-3 5.1 English"}],
        # Order is the spec: subtitle_text_srt picks row 3 = index 2 and expects English.
        "subs": [{"codec": "subrip", "lang": "rus", "forced": True, "title": "Russian (Forced)"},
                 {"codec": "subrip", "lang": "rus", "title": "Russian"},
                 {"codec": "subrip", "lang": "eng", "title": "English"},
                 {"codec": "subrip", "lang": "eng", "title": "English (SDH)"}],
    },
    "episode_hevc_4k_hdr10_eac3": {
        "kind": "episode", "library_title": "PlxTest HDR Show (2010)", "ext": "mkv",
        "episodes": 2, "extra_key": "episode_hevc_4k_hdr10_eac3_next",
        "duration": 300, "rate": 0.65, "min_mbit": 5.0,
        "intro": (0, 130), "credits": 40,
        # 300 s: seek_rapid_hevc_4k lands at 160 s (160/0.9 = 178) and the marker cases want
        # an intro ending >= 100 s AND before the midpoint, i.e. anywhere in (100, 150).
        # 130 sits in the MIDDLE of that window on purpose. It was 105 first, which satisfies
        # both constraints on paper and leaves five seconds of margin against a detector whose
        # boundary precision nobody here has measured -- Plex matches audio fingerprints and is
        # free to trim its match. Land at 98 and `marker_intro_press` (min_pos_after_s: 100)
        # fails while every piece of evidence on screen points at the app's skip-intro latch.
        # There is nothing to buy with the margin, so do not spend it.
        # `credits` is the black-plate + distinct-audio-bed tail marker_credits_up_next
        # needs: Plex derives a `final` credits marker from a credits-LIKE tail, and
        # testsrc2 runs full-brightness with an unchanging tone to the last frame, so
        # before this there was nothing on disk for that case at all.
        "video": {"codec": "hevc", "size": "3840x2160", "crf": 30, "hdr": True},
        # index 0 is the FOREIGN default and index 1 English — see the ordering note in the
        # module docstring: audio_switch_native picks row 0 and expects a native switch.
        "audio": [{"codec": "eac3", "ch": 6, "lang": "deu", "br": "384k", "pitch": 330,
                   "default": True, "title": "E-AC-3 5.1 Deutsch"},
                  {"codec": "eac3", "ch": 6, "lang": "eng", "br": "384k", "pitch": 220,
                   "title": "E-AC-3 5.1 English"}],
        "subs": [],
    },
    "movie_hevc_4k_hdr10_truehd": {
        "kind": "movie", "library_title": "PlxTest HEVC 4K HDR10 TrueHD (2002)", "ext": "mkv",
        "duration": 90, "rate": 0.58,
        "video": {"codec": "hevc", "size": "3840x2160", "crf": 30, "hdr": True},
        # The smart-direct-play shape: a TrueHD default the TV cannot take, with an AC-3
        # sibling it can. No freely-licensed TrueHD exists anywhere; ffmpeg's own encoder
        # (experimental, hence the global -strict -2) is the only way to get this shape.
        "audio": [{"codec": "truehd", "ch": 6, "lang": "eng", "pitch": 220,
                   "default": True, "title": "TrueHD 5.1 English"},
                  {"codec": "ac3", "ch": 6, "lang": "eng", "br": "448k", "pitch": 220,
                   "title": "AC-3 5.1 English"}],
        "subs": [],
    },
    "movie_hevc_4k_dovi_p8": {
        "kind": "movie", "library_title": "PlxTest HEVC 4K DoVi P8 (2003)", "ext": "mkv",
        "duration": 90, "rate": 0.60, "builder": "dovi", "dovi": True,
        "tools": ["dovi_tool", "mkvmerge"],
        "video": {"codec": "hevc", "size": "3840x2160", "crf": 30, "hdr": True},
        "audio": [{"codec": "eac3", "ch": 6, "lang": "eng", "br": "384k", "pitch": 262,
                   "default": True, "title": "E-AC-3 5.1 English"}],
        # `dp_hevc_eac3_dovi_p8` covers `embedded-srt-many` and its title says "many
        # embedded SRT", so the many-track text stack belongs HERE. It was built onto
        # movie_h264_ac3_1080p instead (whose own covers says only `embedded-srt`), which
        # left a declared coverage axis with no file behind it — invisibly, because the
        # case asserts nothing about subtitles. Nothing here asserts an ORDER; the stack
        # mirrors the h264 shape's so a reader comparing the two is not surprised.
        "subs": [{"codec": "subrip", "lang": "rus", "forced": True, "title": "Russian (Forced)"},
                 {"codec": "subrip", "lang": "rus", "title": "Russian"},
                 {"codec": "subrip", "lang": "eng", "title": "English"},
                 {"codec": "subrip", "lang": "eng", "title": "English (SDH)"}],
    },
    "movie_hevc_aac_mp4": {
        "kind": "movie", "library_title": "PlxTest HEVC AAC MP4 (2004)", "ext": "mp4",
        "duration": 90, "rate": 0.14,
        "video": {"codec": "hevc", "size": "1920x1080", "crf": 26, "tag": "hvc1"},
        # Stereo, and NO `title`. Two deliberate differences from every other shape here.
        # Stereo because this case is the mov-demuxer/ADTS-reframing path and a real-world
        # mp4 is usually 2.0 — 5.1 AAC coverage lives on episode_h264_aac — while
        # `devcaps::audio_has` ignores the manufacturer table's channel count, so a set
        # that advertises AAC at 2 channels would still be told to direct-play a 5.1 track
        # and nothing in this repo would notice. No title because mp4 does not carry a
        # per-track title through this path at all: the spec used to claim one, the file
        # never had one, and verify() asserted neither.
        "audio": [{"codec": "aac", "ch": 2, "lang": "eng", "br": "192k", "pitch": 294,
                   "default": True}],
        # Sidecar, not embedded: this shape's case is the mov-demuxer-over-HTTP path and
        # Plex serves the sidecar as its own stream.
        "subs": [{"codec": "subrip", "lang": "eng", "sidecar": True, "title": "English"}],
    },
    "episode_h264_aac": {
        "kind": "episode", "library_title": "PlxTest SDR Show (2011)", "ext": "mkv",
        "episodes": 1, "duration": 90, "rate": 0.06,
        "video": {"codec": "h264", "size": "1920x1080", "crf": 25},
        "audio": [{"codec": "aac", "ch": 6, "lang": "eng", "br": "256k", "pitch": 196,
                   "default": True, "title": "AAC 5.1 English"}],
        "subs": [],
    },
    "movie_h264_ac3_many_audio": {
        "kind": "movie", "library_title": "PlxTest Many Audio (2005)", "ext": "mkv",
        # 120 s, not 90: audio_switch_transcode caps at run_secs 70 and the floor is 1.5x
        # the longest run_secs of any case naming the shape.
        "duration": 120, "rate": 0.08,
        "video": {"codec": "h264", "size": "1920x1080", "crf": 25},
        # EIGHT tracks, and the ORDER is asserted: audio_switch_transcode picks row 6 and
        # expects a transcode, so index 6 is English DTS (outside the direct-play set).
        "audio": [
            {"codec": "ac3", "ch": 6, "lang": "eng", "br": "448k", "pitch": 200,
             "default": True, "title": "AC-3 5.1 English"},
            {"codec": "eac3", "ch": 6, "lang": "eng", "br": "384k", "pitch": 250,
             "title": "E-AC-3 5.1 English Commentary"},
            {"codec": "aac", "ch": 2, "lang": "eng", "br": "256k", "pitch": 300,
             "title": "AAC 2.0 English"},
            {"codec": "ac3", "ch": 6, "lang": "spa", "br": "448k", "pitch": 350,
             "title": "AC-3 5.1 Espanol"},
            {"codec": "ac3", "ch": 6, "lang": "fra", "br": "448k", "pitch": 400,
             "title": "AC-3 5.1 Francais"},
            {"codec": "aac", "ch": 2, "lang": "deu", "br": "256k", "pitch": 450,
             "title": "AAC 2.0 Deutsch"},
            {"codec": "dts", "ch": 6, "lang": "eng", "pitch": 500,
             "title": "DTS 5.1 English"},
            {"codec": "vorbis", "ch": 2, "lang": "jpn", "pitch": 550,
             "title": "Vorbis 2.0 Japanese"},
        ],
        "subs": [],
    },
    "movie_av1_no_dp_audio": {
        "kind": "movie", "library_title": "PlxTest AV1 Opus (2006)", "ext": "mkv",
        "duration": 780, "rate": 0.30,
        # 780 s: resume_transcode seeds 600 s (600/0.9 = 667 floor) and then plays on.
        # Deliberately SDR: the client never demuxes this file — the server transcodes it —
        # so an HDR claim here would be a property nothing verifies end to end.
        "video": {"codec": "av1", "size": "3840x2160", "crf": 50},
        "audio": [{"codec": "opus", "ch": 6, "lang": "eng", "br": "256k", "pitch": 620,
                   "default": True, "title": "Opus 5.1 English"}],
        "subs": [],
    },
    "movie_hevc_4k_pgs_subs": {
        "kind": "movie", "library_title": "PlxTest HEVC 4K PGS Subs (2007)", "ext": "mkv",
        "duration": 780, "rate": 0.65,
        "video": {"codec": "hevc", "size": "3840x2160", "crf": 34, "hdr": True},
        "audio": [{"codec": "eac3", "ch": 6, "lang": "eng", "br": "384k", "pitch": 700,
                   "default": True, "title": "E-AC-3 5.1 English"}],
        # index 0, because subtitle_image_pgs picks row 1 (row 0 is Off).
        "subs": [{"codec": "hdmv_pgs_subtitle", "lang": "eng", "title": "English (PGS)"}],
    },
}

def aliases(shapes):
    """The second output key of a multi-episode shape -> the shape key itself, so `--only`
    takes either. Per TABLE rather than a module constant: the pipeline tier has no episode
    pairs and therefore no aliases, and a constant built from `SHAPES` would have quietly
    offered the integration tier's names to a run that cannot build them."""
    return {s["extra_key"]: k for k, s in shapes.items() if s.get("extra_key")}


QUICK_SECS = 20


# =======================================================================================
# THE PIPELINE TIER — a second, much smaller table, for a suite tier with NO PLEX IN IT.
#
# `tests/serve_fixtures.py` serves a directory of these clips off the dev Mac, and
# `/tmp/plxnative-playurl` hands the app one URL plus the Load payload DECLARATION to play
# it with (`dev::PlayUrl` -> `route::set_stream_declaration`). Everything from the socket
# down is then the same code a real playback runs — `stream.rs`' GET, `ff.rs`' AVIO and
# demux, the `aq.rs` queues, the pump's `Feed()`, the ACB bind — with the server, the
# `/decision`, the PlayQueue, the timeline and the watched state all absent. So this tier
# grades the pipeline and the payload the engine builds for it, and nothing above them.
#
# WHY A SECOND TABLE AND NOT MORE ROWS IN `SHAPES`. Every constant up there is derived from
# something that does not exist down here. A duration is the deepest seek/resume/marker
# depth a Plex CASE reaches divided by Plex's 90% watched threshold (trap 9); nothing here
# seeds a viewOffset, marks anything watched, or has an Up Next to fire, so the only length
# constraint left is "do not hit EOF while a case is still asserting". The LAYOUT differs
# for the same kind of reason: the Plex trees exist for a SCANNER, and the thing reading
# these is a static file server (see `out_paths`).
#
# WHAT THIS TIER DELIBERATELY DOES NOT CONTAIN, written down so nobody adds it back as an
# oversight — each of the three looks like an obvious gap and is not:
#   * TrueHD / Atmos. The correct LG Load-payload audio string for TrueHD does not exist
#     anywhere in this repo, so the shape could not be DECLARED — and the declaration is
#     this tier's entire subject. A `declare` that is a guess grades a guess.
#   * PGS and SRT subtitles. The demuxer would carry them, but the `sub cue` / `image cue`
#     log lines a case would grade sit behind `desired_sub_idx`, which only the Plex path
#     ever writes — so a subtitle case down here would assert nothing at all.
#   * AV1. There is no Load payload video codec for it.
#
# `declare` IS THE TRIGGER'S PLAYBACK HALF, recorded verbatim into fixtures.json so the
# harness writes `/tmp/plxnative-playurl` FROM the pack instead of restating it, and read
# back against the finished file by verify(). A declaration that drifts from its own media
# is precisely the fault this tier cannot otherwise see: the app builds the payload the
# declaration asked for, never looks at the container, and the case then fails pointing
# squarely at the player.
# =======================================================================================
# 60 s, and the reason is trap 9's second half rather than any depth: a clip that hits EOF
# inside a case's window ends the session under the assertions' feet. Every assertion this
# tier can make — the `load:` payload line, the first `Feed`, the ACB bind, `a=#<idx>` —
# lands in the first seconds, so nothing here wants length for its own sake, and 60 s is
# what keeps the whole pack to minutes of encoding.
PIPE_SECS = 60

PIPE_SHAPES = {
    "pipe_h264_ac3_1080p": {
        # The baseline: GET -> demux -> AU queues -> Feed -> ACB bind, an H264 payload and
        # LG's plain "AC3". Every other shape here varies one axis of this one.
        "kind": "clip", "ext": "mkv",
        "duration": PIPE_SECS, "rate": 0.06,
        "declare": {"vcodec": "h264", "acodec": "ac3", "fps": float(FPS), "atmos": False},
        # crf 20 is movie_h264_ac3_1080p's, carried across so the socket and the queues see
        # a real library file's wire rate rather than a near-static trickle (trap 5) — it
        # measured 7.24 Mbit/s at 60 s. No `min_mbit` floor: up there the floor exists for
        # the two rapid-seek CASES, and this tier has no such case today — add the floor
        # with the case that needs it, not before it.
        "video": {"codec": "h264", "size": "1920x1080", "crf": 20},
        "audio": [{"codec": "ac3", "ch": 6, "lang": "eng", "br": "448k", "pitch": 220,
                   "default": True, "title": "AC-3 5.1 English"}],
        "subs": [],
    },
    "pipe_hevc_eac3_4k_hdr10": {
        # H265 payload selection AND the "AC3 PLUS" naming trap in one file: `eac3` is the
        # FFmpeg spelling the trigger carries, `AC3 PLUS` is what the engine must hand
        # Starfish, and the rename lives in engine.rs where nothing else can check it.
        "kind": "clip", "ext": "mkv",
        "duration": PIPE_SECS, "rate": 0.65,
        # The AC-3 track beside it is not decoration: `H265 + AC3` is one of the six Load payload
        # combinations the player can direct-play, and it was the LAST one this tier could not
        # reach — covered on the server tier only, by the TrueHD-default-falls-back-to-its-AC-3-
        # sibling case, i.e. by a library shape almost nobody owns. Two tracks in one file make
        # it a declaration away instead of a fixture away. `declare` still names the E-AC-3 lane;
        # the case that wants the other one overrides `acodec` and reads a=#2.
        "declare": {"vcodec": "hevc", "acodec": "eac3", "fps": float(FPS), "atmos": False},
        "video": {"codec": "hevc", "size": "3840x2160", "crf": 30, "hdr": True},
        "audio": [{"codec": "eac3", "ch": 6, "lang": "eng", "br": "384k", "pitch": 330,
                   "default": True, "title": "E-AC-3 5.1 English"},
                  {"codec": "ac3", "ch": 6, "lang": "eng", "br": "448k", "pitch": 220,
                   "title": "AC-3 5.1 English"}],
        "subs": [],
    },
    "pipe_hevc_eac3_4k_dovi_p8": {
        # The `option.externalStreamingInfo.contents.DolbyHdrInfo` splice, end to end on the
        # hardware. Profile 8.1 is the one DV shape that can be synthesized at all (see
        # build_dovi), and the DECLARATION is what puts the node in the payload, so this is
        # the only fixture in the repo that reaches that code with no PMS decision behind it.
        "kind": "clip", "ext": "mkv",
        "duration": PIPE_SECS, "rate": 0.60, "builder": "dovi", "dovi": True,
        "tools": ["dovi_tool", "mkvmerge"],
        "declare": {"vcodec": "hevc", "acodec": "eac3", "fps": float(FPS), "atmos": False,
                    "dovi": {"profile": 8, "bl_compat": 1, "el_present": False}},
        "video": {"codec": "hevc", "size": "3840x2160", "crf": 30, "hdr": True},
        "audio": [{"codec": "eac3", "ch": 6, "lang": "eng", "br": "384k", "pitch": 262,
                   "default": True, "title": "E-AC-3 5.1 English"}],
        "subs": [],
    },
    "pipe_hevc_aac_mp4": {
        # A second CONTAINER through the AVIO — the mov demuxer seeks differently from
        # matroska's, and a seek here is a socket close and a `Range:` re-open — plus the
        # payload's AAC arm, which additionally turns on ff.rs' ADTS reframing (mp4 carries
        # raw AAC; LG's decoder needs the frame header).
        "kind": "clip", "ext": "mp4",
        "duration": PIPE_SECS, "rate": 0.14,
        "declare": {"vcodec": "hevc", "acodec": "aac", "fps": float(FPS), "atmos": False},
        "video": {"codec": "hevc", "size": "1920x1080", "crf": 26, "tag": "hvc1"},
        # Stereo and no per-track title, for movie_hevc_aac_mp4's reasons: mp4 does not carry
        # a track title through this path at all, so a declared one would be a claim nothing
        # reads back.
        "audio": [{"codec": "aac", "ch": 2, "lang": "eng", "br": "192k", "pitch": 294,
                   "default": True}],
        "subs": [],
    },
    "pipe_h264_aac_mp4": {
        # H.264 in mp4 — the single most ordinary file in the world, and until 2026-08-22 the
        # one direct-play combination NEITHER tier touched. Both packs reached mp4 only through
        # an HEVC shape, so `part_is_streamable`'s mp4 arm, the mov demuxer's AVCC->Annex-B path
        # and the AAC/ADTS reframe had never been exercised together with the H264 payload.
        "kind": "clip", "ext": "mp4",
        "duration": PIPE_SECS, "rate": 0.06,
        "declare": {"vcodec": "h264", "acodec": "aac", "fps": float(FPS), "atmos": False},
        "video": {"codec": "h264", "size": "1920x1080", "crf": 21},
        # Stereo, no track title: mp4 carries neither through this path (see pipe_hevc_aac_mp4).
        "audio": [{"codec": "aac", "ch": 2, "lang": "eng", "br": "192k", "pitch": 233,
                   "default": True}],
        "subs": [],
    },
    "pipe_h264_1080p5994": {
        # The FRAME-RATE axis, broadcast half. 59.94 is not a round number by accident: it is
        # the only rate here that reaches `engine.rs::fps_rational`'s 1001-denominator branch,
        # which converts it to 60000/1001 for the Load payload's `esInfo videoFps`. Every other
        # fixture in both packs is 24p, so that branch — and every rational branch beside it —
        # had never run against a real stream. The television's own capability table claims 60
        # for H.264, so this is inside the hardware envelope, not at its edge.
        "kind": "clip", "ext": "mkv",
        "duration": PIPE_SECS, "rate": 0.13,
        "declare": {"vcodec": "h264", "acodec": "ac3", "fps": 59.94, "atmos": False},
        "video": {"codec": "h264", "size": "1920x1080", "crf": 22, "fps": 59.94},
        "audio": [{"codec": "ac3", "ch": 6, "lang": "eng", "br": "448k", "pitch": 220,
                   "default": True, "title": "AC-3 5.1 English"}],
        "subs": [],
    },
    "pipe_hevc_4k_60fps": {
        # The frame-rate axis at the hardware's stated bound: the device table's `HEVC` row is
        # 4096x2176 at 60, and this is 3840x2160 at exactly 60 — the most demanding thing this
        # panel claims to decode, in the codec it claims it for. Integer rate on purpose, so the
        # pair with pipe_h264_1080p5994 covers both sides of `fps_rational`'s split.
        #
        # NB the app never BOUNDS frame rate: `devcaps` reads the table's width/height and
        # explicitly ignores `maxFrameRate`, so the profile sent to PMS carries no fps
        # limitation at all. This fixture cannot test that gap — nothing synthetic can, because
        # the gap is on the server-decision side — but it does establish that the pipeline half
        # survives the rate, which is the half that would be blamed first.
        "kind": "clip", "ext": "mkv",
        "duration": PIPE_SECS, "rate": 1.35,
        "declare": {"vcodec": "hevc", "acodec": "eac3", "fps": 60.0, "atmos": False},
        "video": {"codec": "hevc", "size": "3840x2160", "crf": 32, "hdr": True, "fps": 60},
        "audio": [{"codec": "eac3", "ch": 6, "lang": "eng", "br": "384k", "pitch": 330,
                   "default": True, "title": "E-AC-3 5.1 English"}],
        "subs": [],
    },
    "pipe_multiaudio_1080p": {
        # Audio-LANE selection by declared codec. `ff.rs::audio_stream_matching` walks the
        # streams and feeds the FIRST whose `avcodec_get_name` matches the declaration, so
        # three tracks in three codecs — all `eng`, so nothing but the codec can be doing the
        # choosing — turn one file into three cases, graded on `a=#<idx>` in the `ff:` line.
        # There is exactly one video stream (verify() asserts that), and the order is
        # asserted too, so audio ordinal i is stream index i+1: ac3 -> a=#1, eac3 -> a=#2,
        # aac -> a=#3. `declare` names the ac3 lane; a case wanting another one overrides
        # `acodec` and reads the index out of this record's `audio` array.
        "kind": "clip", "ext": "mkv",
        "duration": PIPE_SECS, "rate": 0.08,
        "declare": {"vcodec": "h264", "acodec": "ac3", "fps": float(FPS), "atmos": False},
        "video": {"codec": "h264", "size": "1920x1080", "crf": 25},
        "audio": [{"codec": "ac3", "ch": 6, "lang": "eng", "br": "448k", "pitch": 200,
                   "default": True, "title": "AC-3 5.1 English"},
                  {"codec": "eac3", "ch": 6, "lang": "eng", "br": "384k", "pitch": 250,
                   "title": "E-AC-3 5.1 English"},
                  {"codec": "aac", "ch": 2, "lang": "eng", "br": "256k", "pitch": 300,
                   "title": "AAC 2.0 English"}],
        "subs": [],
    },
}

# Which tier a shape belongs to, stamped once here rather than written into every literal:
# it is a property of the TABLE, and a shape that disagreed with the table it lives in would
# mislabel its own record. verify() copies it into fixtures.json, which is what tells a
# reader — and `tests/run.py` — which suite tier a pack can be pointed at. Note this is NOT
# derivable from the duration: the integration table has 90 s shapes.
# Stamped below, from TIERS itself — a second hand-written pairing of tier name to shape table
# is one registry a third tier gets added to and another it gets forgotten in.


def shape_duration(spec, secs=None):
    """The length to BUILD this shape at: what it declares, or this run's override.

    The override is `--secs` (`--quick` is that same override at QUICK_SECS, under the name
    the Makefile and the README already use). It is a property of the RUN, never of the
    shape: `spec["duration"]` stays the length the shape is DEFINED at, which is what
    verify() grades a file's `quick` flag against — so a deliberately shortened clip still
    says so in fixtures.json, whichever tier it came from.
    """
    return spec["duration"] if secs is None else secs


def out_paths(root, key, spec):
    """Where this shape's file(s) go — one layout per tier.

    (The `quick` parameter this carried until the pipeline tier landed was never read:
    length has never been part of a path, and a shortened clip deliberately overwrites the
    full-length file it is a short copy of.)
    """
    # A clip is flat by definition: the Plex tree exists for a SCANNER, and nothing scans the
    # pipeline pack — tests/serve_fixtures.py serves a directory. (`flat` was a second key set on
    # exactly the shapes whose kind was already "clip", and read only here.)
    if spec["kind"] == "clip":
        # The pipeline tier. `tests/serve_fixtures.py` serves a DIRECTORY — the trees below
        # exist for a Plex scanner and this tier has none — and the file is named for the
        # shape key, so a `plxnative-playurl` URL says which shape it is playing
        # (`…/pipe_hevc_eac3_4k_dovi_p8.mkv`) without a lookup. The `pipe_` prefix and the
        # separate root are also what keep the two tiers off one path: main()'s "longer on
        # disk, keep it" rule would otherwise hand a 60 s pipeline run the full-length
        # integration file and rebuild nothing, and report it as `already correct`.
        return [(key, root / ("%s.%s" % (key, spec["ext"])))]
    # Plex-scannable layout. Movies and shows MUST be separate libraries; the scanners
    # assume the two content types live apart, and a mixed root matches badly or not at all.
    n = spec.get("episodes", 1)
    if spec["kind"] == "movie":
        t = spec["library_title"]
        return [(key, root / "Movies" / t / ("%s.%s" % (t, spec["ext"])))]
    show = spec["library_title"]
    out = []
    for ep in range(1, n + 1):
        k = key if ep == 1 else spec["extra_key"]
        out.append((k, root / "TV Shows" / show / "Season 01"
                    / ("%s - s01e%02d.%s" % (show, ep, spec["ext"]))))
    return out


def shape_index(shapes):
    """Output key -> (shape key, spec, episode number). The reverse of out_paths, so a
    record read back out of an existing fixtures.json can be re-verified without knowing
    which run wrote it. Built from the RUN'S table, not from every table there is: each tier
    keeps its own fixtures.json beside its own media, and a key from the other one is a key
    that does not belong in this document."""
    idx = {}
    for k, spec in shapes.items():
        for out_key, _ in out_paths(Path("."), k, spec):
            ep = None
            if spec["kind"] == "episode" and spec.get("episodes", 1) > 1:
                ep = 1 if out_key == k else 2
            idx[out_key] = (k, spec, ep)
    return idx


# Measured wire rate per shape (Mbit/s, video+audio) — used only for the up-front size
# estimate. Re-measure alongside `rate` if the CRFs above ever move, and take the sample
# from a FULL-LENGTH build: a 20-second clip is I-frame-heavy and reads 20-30% high (the
# 4K HDR10 episode measured 9.1 Mbit/s over 20 s and 7.6 over its real 300). These numbers
# therefore run slightly conservative, which is the safe direction for a disk-space warning.
MBIT = {
    "movie_h264_ac3_1080p": 7.50, "episode_hevc_4k_hdr10_eac3": 8.80,
    "movie_hevc_4k_hdr10_truehd": 11.0, "movie_hevc_4k_dovi_p8": 9.80,
    "movie_hevc_aac_mp4": 5.00, "episode_h264_aac": 4.60,
    "movie_h264_ac3_many_audio": 8.10, "movie_av1_no_dp_audio": 4.40,
    "movie_hevc_4k_pgs_subs": 5.50,
}

# The pipeline tier's estimate. ONE of these is measured: `pipe_h264_ac3_1080p` built at its
# full 60 s came out at 7.24 Mbit/s (54.3 MB) on an Apple-silicon Mac with this ffmpeg, and
# the same shape at 5 s read 7.19 — so at these lengths that shape shows none of the
# short-clip inflation the comment above records, which was measured on the 4K HDR10 x265
# shape and should not be assumed to carry to a different encoder. The other four numbers are
# the integration table's, carried across from the shape each pipeline clip is a short copy
# of (named beside each) and NOT re-measured; re-measure the first time an estimate here is
# visibly wrong. The multi-audio clip borrows the eight-track shape's number and so
# over-estimates by the five tracks it does not have, which is the safe direction for a
# disk-space warning.
PIPE_MBIT = {
    "pipe_h264_ac3_1080p": 7.24,        # measured, from a full 60 s build
    "pipe_hevc_eac3_4k_hdr10": 8.80,    # = episode_hevc_4k_hdr10_eac3 (hevc 4K crf 30 HDR)
    "pipe_hevc_eac3_4k_dovi_p8": 9.80,  # = movie_hevc_4k_dovi_p8
    "pipe_hevc_aac_mp4": 5.00,          # = movie_hevc_aac_mp4         (hevc 1080p crf 26)
    "pipe_h264_aac_mp4": 6.40,          # measured, from a full 60 s build
    "pipe_h264_1080p5994": 9.48,        # measured; NB 2.5x the frames cost only 1.3x the bits
    "pipe_hevc_4k_60fps": 9.40,         # measured, at crf 32 (crf 30 would be well over 15)
    "pipe_multiaudio_1080p": 8.10,      # = movie_h264_ac3_many_audio, minus five tracks
}

# The two tiers as data: which table, which wire-rate estimate, and which subdirectory of
# `--out` the pack lives in. `subdir` is what keeps the packs apart on disk — the flat
# `pipe_` names and the Plex trees could share a root without a filename collision, but
# fixtures.json could not, and that document is read as ground truth by everything
# downstream.
TIERS = {
    "integration": {"shapes": SHAPES, "mbit": MBIT, "subdir": None},
    "pipeline": {"shapes": PIPE_SHAPES, "mbit": PIPE_MBIT, "subdir": "pipeline"},
}

for _tier, _t in TIERS.items():          # see the note above the tables
    for _spec in _t["shapes"].values():
        _spec["tier"] = _tier


# ---------------------------------------------------------------------------------------
# Builders
# ---------------------------------------------------------------------------------------
# The marker segments' audio, as pitch multipliers. The intro melody is SHARED by every
# episode (that identity is the thing Plex's fingerprinter looks for) and has three steps
# rather than one held tone, because a constant sine gives an aligner nothing to lock onto.
# The body pitch is per-episode, so the match STOPS at intro_end instead of running the
# whole episode and past the halfway point where Plex discards the candidate.
INTRO_MELODY = (1.0, 1.335, 1.189)
BODY_PITCH = {1: 1.0, 2: 1.19}
CREDITS_PITCH = 0.5


def marker_plan(ctx, spec, dur):
    """`(intro_end, credits_start)` in seconds — or `(None, None)` for a shape/run with no
    markers.

    MARKERS ONLY AT THE SHAPE'S FULL DECLARED LENGTH. The guard used to read `not
    ctx.quick`, which said the same thing by accident for as long as QUICK_SECS was the only
    other length there was. `intro` and `credits` are ABSOLUTE seconds against the declared
    duration, so at any shorter build they are not approximate but broken, and broken
    SILENTLY: at 60 s the episode pair's `intro: (0, 130)` puts intro_end past the end of the
    clip, so the banner overlay's `enable='gte(t,130)'` never fires — which verify() cannot
    see, because it compares the container's description TAG and never the pixels — while
    `audio_segments` goes on to ask lavfi for a body segment of `60 - 40 - 130 = -110`
    seconds. Markers are a Plex-detector concern
    to the last inch — a tier with no Plex in it never wants them, and no pipeline shape
    declares an `intro` either.
    """
    if not (ctx.markers and spec.get("intro") and dur >= spec["duration"]):
        return None, None
    credits = spec.get("credits") or 0
    return spec["intro"][1], (dur - credits if credits else None)


def audio_segments(spec, dur, ep, intro_end, credits_start):
    """The `segs` list for `audio_graph` — see INTRO_MELODY. None when there are no markers."""
    if intro_end is None:
        return None
    step = intro_end / float(len(INTRO_MELODY))
    segs = [(m, step) for m in INTRO_MELODY]
    body_end = credits_start or dur
    segs.append((BODY_PITCH.get(ep or 1, 1.0), body_end - intro_end))
    if credits_start:
        segs.append((CREDITS_PITCH, dur - credits_start))
    return segs


def container_title(key, spec):
    """The container's own `title` tag: the library title where there is one, since that is
    what a Plex scanner matches on, and the shape key for a flat pipeline clip, which has no
    library, no scanner and nothing for a title to be matched against."""
    return spec.get("library_title") or key


def _assets(ctx, key, spec, dur, ep, work):
    """Banner PNGs and subtitle assets for one output file."""
    v = spec["video"]
    vw, vh = (int(x) for x in v["size"].split("x"))
    lines = layout_lines(key, spec, dur, ep)
    scale = fit_scale(lines, vw, vh)
    tag = "%s%s" % (key, "_e%d" % ep if ep else "")
    banner = banner_png(work / ("banner_%s.png" % tag), lines, scale)
    intro = None
    if marker_plan(ctx, spec, dur)[0] is not None:
        intro = banner_png(work / ("intro_%s.png" % key),
                           ["INTRO SEGMENT", "IDENTICAL IN EVERY EPISODE",
                            "SKIP-INTRO MARKER BAIT"], scale + 2)
    return lines, banner, intro


def build_generic(ctx, key, spec, dur, ep, dest, work, video_only=False):
    """One ffmpeg invocation: lavfi video + banner overlay + N audio graphs + M sub files.

    Input order is fixed and the maps are computed from it, because ffmpeg's stream
    specifiers are positional and a hand-counted `-map 4:a` is exactly the kind of thing
    that silently produces the wrong track ORDER — which is the one property several cases
    assert (see the module docstring).
    """
    v = spec["video"]
    lines, banner, intro = _assets(ctx, key, spec, dur, ep, work)
    intro_end, credits_start = marker_plan(ctx, spec, dur)
    segs = audio_segments(spec, dur, ep, intro_end, credits_start)

    argv = ["ffmpeg", "-y", "-v", "error", "-nostdin"]
    # -t BEFORE -i, always (trap 1).
    # `rate=` takes a rational, so 59.94 goes in as 60000/1001 rather than as a float that
    # lavfi would round to 60 — the whole point of that shape is the 1001-denominator branch of
    # `engine.rs::fps_rational`, which a rounded 60 would never reach.
    argv += ["-t", str(dur), "-f", "lavfi",
             "-i", "testsrc2=size=%s:rate=%s" % (v["size"], _rate_arg(v.get("fps", FPS)))]
    argv += ["-i", str(banner)]
    idx = 2
    intro_idx = None
    if intro is not None:
        argv += ["-i", str(intro)]
        intro_idx = idx
        idx += 1

    a_idx, s_idx = [], []
    if not video_only:
        for a in spec.get("audio", []):
            argv += ["-t", str(dur), "-f", "lavfi",
                     "-i", audio_graph(a["ch"], a["pitch"], dur, segs)]
            a_idx.append(idx)
            idx += 1
        for i, s in enumerate(spec.get("subs", [])):
            if s.get("sidecar"):
                continue
            if s["codec"] == "subrip":
                p = write_srt(work / ("sub_%s_%d.srt" % (key, i)), i,
                              "%s %s" % (s["lang"].upper(), s.get("title", "")), dur)
            else:
                p = work / ("sub_%s_%d.sup" % (key, i))
                pgs_build(p, dur)
            argv += ["-i", str(p)]
            s_idx.append((idx, i, s))
            idx += 1

    # ---- filter chain. overlay's default eof_action is `repeat`, which is what lets a
    # single-frame PNG input persist for the whole clip without -loop/-framerate games.
    # TOP-centre at H/12: below testsrc2's own timecode box, and clear of both the band the
    # app renders subtitles in and the band pgs_build authors its cues in. And gated OFF
    # during the intro window — the banner's first line carries `S01E01` vs `S01E02`, so
    # while it was drawn there the intro was the one stretch of the two episodes that was
    # NOT identical, which is precisely backwards for a detector looking for a match.
    fc = "[0:v][1:v]overlay=(W-w)/2:H/12"
    if intro_end is not None:
        fc += ":enable='gte(t,%d)'" % intro_end
        fc += "[bg];[bg][%d:v]overlay=(W-w)/2:(H-h)/2:enable='lt(t,%d)'" % (intro_idx, intro_end)
    if ep == 2:
        # Episode 2 must not be a byte-identical twin of episode 1, or "which episode is
        # this" becomes unanswerable from a capture. Tint AFTER the intro only, so the
        # intro segment stays the identical stretch Plex's detector is looking for. NB the
        # tint is for the HUMAN reading a capture; the episodes' real difference is in the
        # AUDIO (see audio_segments), which is what Plex actually fingerprints.
        fc += ",hue=h=140:enable='gte(t,%d)'" % (intro_end or 0)
    if credits_start is not None:
        # A credits-like tail: black picture under a distinct audio bed. Plex derives the
        # `final` credits marker marker_credits_up_next needs from exactly this, and
        # testsrc2 runs full-brightness colour bars to the last frame.
        fc += (",drawbox=x=0:y=0:w=iw:h=ih:color=black@1.0:t=fill:enable='gte(t,%d)'"
               % credits_start)
    # format= BEFORE setparams=, or the auto-scaler eats the colour stamp (trap 2).
    fc += "," + (CHAIN_PQ if v.get("hdr") else CHAIN_SDR) + "[v]"
    argv += ["-filter_complex", fc, "-map", "[v]"]

    for i in a_idx:
        argv += ["-map", "%d:a" % i]
    for i, _, _ in s_idx:
        argv += ["-map", "%d:s" % i]

    argv += venc_args(v)
    if video_only:
        argv += ["-an", "-sn", "-f", "hevc", "-t", str(dur), str(dest)]
        run(argv)
        return lines

    # ffmpeg writes an explicit FlagDefault=0 on the video track unless told otherwise, so
    # every ffmpeg-muxed shape here disagreed with the mkvmerge-muxed one (and with every
    # real library file). Nothing in the app reads the video default flag today — this is
    # consistency, not a fix — but the asymmetry was invisible because verify() checked the
    # flag on audio streams only. It checks the video one now too.
    argv += ["-disposition:v:0", "default"]
    for n, a in enumerate(spec.get("audio", [])):
        argv += ["-c:a:%d" % n] + AENC[a["codec"]]
        if a.get("br"):
            argv += ["-b:a:%d" % n, a["br"]]
        argv += ["-ac:a:%d" % n, str(a["ch"])]
        argv += ["-metadata:s:a:%d" % n, "language=" + a["lang"]]
        argv += ["-metadata:s:a:%d" % n, "title=" + a.get("title", "")]
        # Dispositions are set EXPLICITLY on every track, including the zeros: ffmpeg marks
        # the first stream of a type default on its own, and a second "default" audio track
        # changes which one the route auto-picks.
        argv += ["-disposition:a:%d" % n, "default" if a.get("default") else "0"]

    embedded = [s for s in spec.get("subs", []) if not s.get("sidecar")]
    if embedded:
        # One `-c:s` for the whole output, so a shape mixing text and bitmap tracks would
        # need a per-stream codec list. No shape does today; fail loudly rather than
        # transcode a .sup into subrip (which ffmpeg refuses anyway — see trap 10).
        kinds = {x["codec"] == "subrip" for x in embedded}
        if len(kinds) != 1:
            raise Fail("shape %r mixes text and bitmap subtitle tracks; build_generic "
                       "emits a single -c:s" % key)
        argv += ["-c:s", "srt" if embedded[0]["codec"] == "subrip" else "copy"]
    for n, s in enumerate(embedded):
        argv += ["-metadata:s:s:%d" % n, "language=" + s["lang"]]
        argv += ["-metadata:s:s:%d" % n, "title=" + s.get("title", "")]
        argv += ["-disposition:s:%d" % n, "forced" if s.get("forced") else "0"]

    desc = "\n".join(lines)
    argv += ["-metadata", "title=" + container_title(key, spec),
             "-metadata", "description=" + desc,
             "-metadata", "comment=" + desc]
    if spec["ext"] == "mp4":
        argv += ["-movflags", "+faststart"]
    # -strict -2 once, globally: truehd, dca and the native vorbis encoder are all marked
    # experimental in ffmpeg and refuse to run without it.
    argv += ["-strict", "-2"]
    # A trailing output -t as well: a .sup is not a timed input, so without this the mux
    # runs to the LONGEST stream and the container duration comes out wrong (measured: a
    # 20 s video muxed with a 60 s .sup produced a 58 s file).
    argv += ["-t", str(dur), str(dest)]
    dest.parent.mkdir(parents=True, exist_ok=True)
    run(argv)

    # A Plex sidecar is "<video basename>.<lang>.srt" and must carry the video's FINAL
    # basename, which is the library title — not the shape key it was generated under.
    for s in spec.get("subs", []):
        if s.get("sidecar"):
            lang2 = SIDECAR_LANG.get(s["lang"], s["lang"])
            write_srt(dest.parent / (dest.stem + "." + lang2 + ".srt"),
                      0, s["lang"].upper() + " SIDECAR", dur)
    return lines


def build_dovi(ctx, key, spec, dur, ep, dest, work):
    """Dolby Vision profile 8.1, synthesized end to end — no donor file involved.

    8.1 means the base layer IS plain HDR10, which is why this can be built at all: encode
    the HDR10 base, have `dovi_tool generate` author an RPU from a JSON description, inject
    it, and let mkvmerge write the container's DV configuration record. ffmpeg's matroska
    muxer will NOT write that record from a raw .hevc, and it is exactly what the app reads
    back (`ff: … dovi=P8 bl_compat=1`) and what `dp_hevc_eac3_dovi_p8` is the regression
    guard for. x265 needs aud/hrd/vbv here so the injected RPU NALs land in a conformant
    access-unit structure.
    """
    v = dict(spec["video"], x265_extra="aud=1:hrd=1:vbv-maxrate=50000:vbv-bufsize=50000")
    bl = work / ("%s_bl.hevc" % key)
    lines = build_generic(ctx, key, dict(spec, video=v), dur, ep, bl, work, video_only=True)

    cfg = work / ("%s_dovi.json" % key)
    cfg.write_text(json.dumps({
        "cm_version": "V40", "profile": "8.1", "length": int(dur * shape_fps(spec)),
        "level5": {"active_area_left_offset": 0, "active_area_right_offset": 0,
                   "active_area_top_offset": 0, "active_area_bottom_offset": 0},
        "level6": {"max_display_mastering_luminance": 1000,
                   "min_display_mastering_luminance": 1,
                   "max_content_light_level": 1000,
                   "max_frame_average_light_level": 400},
    }))
    rpu = work / ("%s_rpu.bin" % key)
    run(["dovi_tool", "generate", "-j", str(cfg), "-o", str(rpu)])
    bl_rpu = work / ("%s_bl_rpu.hevc" % key)
    run(["dovi_tool", "inject-rpu", "-i", str(bl), "--rpu-in", str(rpu), "-o", str(bl_rpu)])

    intro_end, credits_start = marker_plan(ctx, spec, dur)
    segs = audio_segments(spec, dur, ep, intro_end, credits_start)
    a = spec["audio"][0]
    mka = work / ("%s_audio.mka" % key)
    run(["ffmpeg", "-y", "-v", "error", "-nostdin",
         "-t", str(dur), "-f", "lavfi", "-i", audio_graph(a["ch"], a["pitch"], dur, segs)]
        + ["-c:a"] + AENC[a["codec"]] + (["-b:a", a["br"]] if a.get("br") else [])
        + ["-ac", str(a["ch"]), "-strict", "-2", "-t", str(dur), str(mka)])

    # The description tag, which this builder used to skip entirely: build_generic writes
    # it, but the DV path returns from build_generic at the video_only branch long before
    # that code runs, so the ONE file whose picture is hardest to tell apart from its
    # neighbours was also the one with no machine-readable layout in it. mkvmerge writes
    # global tags from an XML document; ffprobe reads them back as format.tags.DESCRIPTION,
    # which is what verify() now compares against layout_lines().
    tags = work / ("%s_tags.xml" % key)
    desc = "\n".join(lines)
    tags.write_text('<?xml version="1.0" encoding="UTF-8"?>\n<Tags><Tag>\n'
                    + "".join("<Simple><Name>%s</Name><String>%s</String></Simple>\n"
                              % (n, _xml_escape(desc)) for n in ("DESCRIPTION", "COMMENT"))
                    + "</Tag></Tags>\n", encoding="utf-8")

    dest.parent.mkdir(parents=True, exist_ok=True)
    argv = ["mkvmerge", "-q", "-o", str(dest),
            "--title", container_title(key, spec), "--global-tags", str(tags),
            "--default-duration", "0:%sfps" % _rate_arg(shape_fps(spec)), "--language", "0:und",
            "--default-track", "0:yes", str(bl_rpu),
            "--language", "0:" + a["lang"], "--track-name", "0:" + a.get("title", ""),
            "--default-track", "0:yes", str(mka)]
    srts = []
    for i, s in enumerate(spec.get("subs", [])):
        if s.get("sidecar"):
            continue
        if s["codec"] != "subrip":
            raise Fail("build_dovi muxes text subtitles only; %r wants %r" % (key, s["codec"]))
        p = write_srt(work / ("sub_%s_%d.srt" % (key, i)), i,
                      "%s %s" % (s["lang"].upper(), s.get("title", "")), dur)
        srts.append(p)
        argv += ["--language", "0:" + s["lang"], "--track-name", "0:" + s.get("title", ""),
                 "--forced-track", "0:%s" % ("yes" if s.get("forced") else "no"),
                 "--default-track", "0:no", str(p)]
    run(argv)
    for p in (bl, bl_rpu, mka, rpu, cfg, tags, *srts):
        p.unlink(missing_ok=True)
    return lines


def _xml_escape(s):
    return (s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
             .replace('"', "&quot;"))


# ---------------------------------------------------------------------------------------
# Self-verification. NOT optional, and not a formality: a generator that silently emits the
# wrong shape is worse than no generator at all, because the harness then fails as if the
# PLAYER regressed and the evidence all points at the app. Everything a shape CLAIMS is
# read back out of the finished file; a mismatch names both sides and is fatal.
# ---------------------------------------------------------------------------------------
VCODEC_NAME = {"h264": "h264", "hevc": "hevc", "av1": "av1"}
ACODEC_NAME = {"ac3": "ac3", "eac3": "eac3", "aac": "aac", "truehd": "truehd",
               "dts": "dts", "vorbis": "vorbis", "opus": "opus", "flac": "flac"}


def _rate(s):
    """ffprobe's rational frame rate ("24/1") as a float; 0.0 when it says nothing."""
    try:
        n, d = (s or "0/0").split("/")
        return float(n) / float(d) if float(d) else 0.0
    except (ValueError, ZeroDivisionError):
        return 0.0


def verify(key, spec, dur, path, ep=None):
    problems, rec = [], {}
    if not path.exists():
        return {}, ["missing file %s" % path]
    j = probe(path)
    fmt = j.get("format", {})
    rec["size_bytes"] = int(fmt.get("size") or path.stat().st_size)

    measured = float(fmt.get("duration") or 0.0)
    rec["duration_s"] = round(measured, 3)
    # A property of the FILE, not of the invocation that happened to look at it — which is
    # what makes the document's top-level flag derivable rather than a per-run stamp.
    rec["quick"] = measured < spec["duration"] * 0.9
    # Which suite tier the pack is for, out of the table this shape came from — and NOT
    # derivable from anything else in the record: a 60 s file is a full-length pipeline clip
    # and a stunted integration one, and `quick` above can only tell those apart once you
    # already know which table to compare it against.
    rec["tier"] = spec["tier"]
    tol = max(2.0, dur * 0.03)
    if abs(measured - dur) > tol:
        # DUR_LONGER_MARK: main() reads this exact prefix to tell "the file on disk is
        # LONGER than this run asked for" from every other duration mismatch, because the
        # two want opposite handling — a short file is wrong, a long one is a full-length
        # build a later `--quick` run must not silently shorten.
        problems.append("duration %.2fs, wanted %ds (+-%.1f)" % (measured, dur, tol))

    if measured > 0:
        rec["bitrate_mbit"] = round(rec["size_bytes"] * 8 / measured / 1e6, 2)
        floor = spec.get("min_mbit")
        if floor and rec["bitrate_mbit"] < floor:
            problems.append("%.2f Mbit/s, wanted at least %.2f — a rapid-seek case grades "
                            "seek COALESCING and a thin stream never coalesces (trap 5)"
                            % (rec["bitrate_mbit"], floor))

    # The burned-in layout, read back out of the container rather than restated from the
    # spec. fixtures.json used to record `layout` from layout_lines() alone, so it asserted
    # a description the file did not necessarily have — and one builder did not write one
    # at all. Tag case differs by container (matroska uppercases), hence the fold.
    tags = {k.lower(): v for k, v in (fmt.get("tags") or {}).items()}
    # Against the MEASURED length, not the requested one: the layout's duration field is
    # whatever the file was built at, and grading a full-length file during a --quick run
    # must produce exactly ONE complaint (the duration), which is what main() keys the
    # "longer than asked, keep it" rule on.
    want_desc = "\n".join(layout_lines(key, spec, int(round(measured)) or dur, ep))
    got_desc = tags.get("description") or tags.get("comment")
    rec["description_ok"] = (got_desc or "").strip() == want_desc.strip()
    if not rec["description_ok"]:
        problems.append("container description does not match the burned-in layout "
                        "(got %r)" % ((got_desc or "")[:60]))

    streams = j.get("streams", [])
    vs = [s for s in streams if s["codec_type"] == "video"]
    aus = [s for s in streams if s["codec_type"] == "audio"]
    ss = [s for s in streams if s["codec_type"] == "subtitle"]

    v = spec["video"]
    want_w, want_h = (int(x) for x in v["size"].split("x"))
    if len(vs) != 1:
        problems.append("expected 1 video stream, found %d" % len(vs))
    else:
        s = vs[0]
        rec["video"] = {"codec": s.get("codec_name"), "width": s.get("width"),
                        "height": s.get("height"), "pix_fmt": s.get("pix_fmt"),
                        "color_primaries": s.get("color_primaries"),
                        "color_transfer": s.get("color_transfer"),
                        "color_space": s.get("color_space"),
                        "codec_tag": s.get("codec_tag_string"),
                        "default": (s.get("disposition") or {}).get("default"),
                        "profile": s.get("profile")}
        if v.get("tag") and s.get("codec_tag_string") != v["tag"]:
            # `hvc1` vs `hev1` decides whether some players will touch the file at all, and
            # it is one `-tag:v` away from wrong. A hev1 copy of this shape verified clean.
            problems.append("codec tag %r, wanted %r" % (s.get("codec_tag_string"), v["tag"]))
        if not (s.get("disposition") or {}).get("default"):
            problems.append("video track is not flagged default (real library files are)")
        want_codec = VCODEC_NAME[v["codec"]]
        if s.get("codec_name") != want_codec:
            problems.append("video codec %r, wanted %r" % (s.get("codec_name"), want_codec))
        if (s.get("width"), s.get("height")) != (want_w, want_h):
            problems.append("video %sx%s, wanted %s" % (s.get("width"), s.get("height"), v["size"]))
        want_pix = "yuv420p10le" if v.get("hdr") else "yuv420p"
        if s.get("pix_fmt") != want_pix:
            hint = (" (trap 7: lavfi is rgb24 and libx264 picks yuv444p when nothing in the "
                    "chain says format=)" if "444" in (s.get("pix_fmt") or "") else "")
            problems.append("pix_fmt %r, wanted %r%s" % (s.get("pix_fmt"), want_pix, hint))
        if v["codec"] == "h264" and "Baseline" in (s.get("profile") or ""):
            problems.append("H.264 profile %r — -preset ultrafast forced Constrained "
                            "Baseline (trap 6)" % s.get("profile"))
        if v.get("hdr"):
            for field, want in (("color_transfer", "smpte2084"),
                                ("color_primaries", "bt2020"),
                                ("color_space", "bt2020nc")):
                if s.get(field) != want:
                    problems.append("%s %r, wanted %r (trap 2: the auto-scaler overwrites "
                                    "these unless setparams runs after format=)"
                                    % (field, s.get(field), want))
            sd = first_frame_side_data(path)
            rec["video"]["frame_side_data"] = sd
            for want in ("Mastering display metadata", "Content light level metadata"):
                if want not in sd:
                    problems.append("no in-band %s (x265 repeat-headers=1 did not land)" % want)
        if spec.get("dovi"):
            dv = [x for x in (s.get("side_data_list") or [])
                  if "DOVI" in (x.get("side_data_type") or "")]
            if not dv:
                problems.append("no DOVI configuration record in the container "
                                "(ffmpeg's matroska muxer will not write one — mux with mkvmerge)")
            else:
                rec["video"]["dovi"] = {"profile": dv[0].get("dv_profile"),
                                        "bl_compat": dv[0].get("dv_bl_signal_compatibility_id"),
                                        "el_present": dv[0].get("el_present_flag"),
                                        "bl_present": dv[0].get("bl_present_flag"),
                                        "rpu_present": dv[0].get("rpu_present_flag")}
                if dv[0].get("dv_profile") != 8:
                    problems.append("dv_profile %r, wanted 8" % dv[0].get("dv_profile"))
                if dv[0].get("dv_bl_signal_compatibility_id") != 1:
                    problems.append("dv_bl_signal_compatibility_id %r, wanted 1 (8.1 means "
                                    "an HDR10-compatible base layer)"
                                    % dv[0].get("dv_bl_signal_compatibility_id"))
                # `metadata::Dovi::base_layer_unusable()` has THREE disqualifiers —
                # el_present, profile 5, bl_compat 0 — and this block asserted two of them.
                # An el_present=1 file makes the route refuse direct play, so
                # dp_hevc_eac3_dovi_p8 fails on `decision` with every piece of evidence
                # pointing at metadata::Dovi while the fixture verified clean.
                if dv[0].get("el_present_flag"):
                    problems.append("el_present_flag=1 — a dual-layer base layer, which "
                                    "route.rs refuses to direct-play (base_layer_unusable)")
                if not dv[0].get("bl_present_flag"):
                    problems.append("bl_present_flag=0 — no base layer in the record")
                if not dv[0].get("rpu_present_flag"):
                    problems.append("rpu_present_flag=0 — the configuration record claims "
                                    "no RPU, so this is not Dolby Vision")
            # ...and the record is a CLAIM the container makes. The RPU NALs are what the
            # decoder sees: assert they are in the bitstream too.
            sd = rec.get("video", {}).get("frame_side_data") or first_frame_side_data(path)
            rec["video"]["frame_side_data"] = sd
            if "Dolby Vision RPU Data" not in sd:
                problems.append("no in-band Dolby Vision RPU Data on the first frame "
                                "(dovi_tool inject-rpu did not land)")

    want_audio = spec.get("audio", [])
    rec["audio"] = [{"codec": s.get("codec_name"), "channels": s.get("channels"),
                     "language": (s.get("tags") or {}).get("language"),
                     "title": (s.get("tags") or {}).get("title"),
                     "default": (s.get("disposition") or {}).get("default")} for s in aus]
    if len(aus) != len(want_audio):
        problems.append("%d audio streams, wanted %d" % (len(aus), len(want_audio)))
    else:
        for i, (got, w) in enumerate(zip(aus, want_audio)):
            want_name = ACODEC_NAME[w["codec"]]
            if got.get("codec_name") != want_name:
                problems.append("audio[%d] codec %r, wanted %r — ORDER is asserted by the "
                                "track-menu cases" % (i, got.get("codec_name"), want_name))
            if got.get("channels") != w["ch"]:
                problems.append("audio[%d] %r channels, wanted %d" % (i, got.get("channels"), w["ch"]))
            lang = (got.get("tags") or {}).get("language")
            if lang != w["lang"]:
                problems.append("audio[%d] language %r, wanted %r" % (i, lang, w["lang"]))
            dflt = bool((got.get("disposition") or {}).get("default"))
            if dflt != bool(w.get("default")):
                problems.append("audio[%d] default=%s, wanted %s" % (i, dflt, bool(w.get("default"))))
            # Track titles are checked for MATROSKA only: mp4 does not carry a per-track
            # title through this path, which is why the mp4 shape declares none. A spec
            # field nothing reads back is a claim, and this file had three of them.
            if spec["ext"] != "mp4" and w.get("title"):
                title = (got.get("tags") or {}).get("title")
                if title != w["title"]:
                    problems.append("audio[%d] title %r, wanted %r" % (i, title, w["title"]))

    want_subs = [s for s in spec.get("subs", []) if not s.get("sidecar")]
    rec["subtitles"] = [{"codec": s.get("codec_name"),
                         "language": (s.get("tags") or {}).get("language"),
                         "title": (s.get("tags") or {}).get("title"),
                         "forced": (s.get("disposition") or {}).get("forced")} for s in ss]
    want_cues = sub_cue_times(dur)
    if len(ss) != len(want_subs):
        problems.append("%d subtitle streams, wanted %d" % (len(ss), len(want_subs)))
    else:
        for i, (got, w) in enumerate(zip(ss, want_subs)):
            if got.get("codec_name") != w["codec"]:
                problems.append("subtitle[%d] codec %r, wanted %r"
                                % (i, got.get("codec_name"), w["codec"]))
            lang = (got.get("tags") or {}).get("language")
            if lang != w["lang"]:
                problems.append("subtitle[%d] language %r, wanted %r" % (i, lang, w["lang"]))
            if bool((got.get("disposition") or {}).get("forced")) != bool(w.get("forced")):
                problems.append("subtitle[%d] forced flag mismatch" % i)
            if spec["ext"] != "mp4" and w.get("title"):
                title = (got.get("tags") or {}).get("title")
                if title != w["title"]:
                    problems.append("subtitle[%d] title %r, wanted %r" % (i, title, w["title"]))
            if w["codec"] == "subrip":
                # WHERE the cues are, not merely that the track exists. This was the one
                # vacuous corner left in verify(): a stunted SRT with a single cue at t=0
                # kept its codec, language and forced flag and verified clean, while
                # `subtitle_text_srt` — which seeds 843 s — failed on the television as
                # `no sub cue`, i.e. as a demuxer regression. One ffprobe per track.
                try:
                    pk = json.loads(run(["ffprobe", "-v", "error", "-select_streams", "s:%d" % i,
                                         "-show_packets", "-show_entries", "packet=pts_time",
                                         "-of", "json", str(path)]))
                    pts = sorted(float(p["pts_time"]) for p in pk.get("packets", [])
                                 if p.get("pts_time") not in (None, "N/A"))
                except (Fail, ValueError):
                    pts = []
                rec["subtitles"][i]["cue_count"] = len(pts)
                rec["subtitles"][i]["last_cue_s"] = round(pts[-1], 2) if pts else None
                if len(pts) < len(want_cues):
                    problems.append("subtitle[%d] carries %d cues, wanted %d"
                                    % (i, len(pts), len(want_cues)))
                elif want_cues and pts[-1] < want_cues[-1] - 1.0:
                    problems.append("subtitle[%d] last cue at %.1fs, wanted one at %ds — a "
                                    "track with no cue where the case PLAYS fails on device "
                                    "as `no sub cue`" % (i, pts[-1], want_cues[-1]))
            if w["codec"] == "hdmv_pgs_subtitle":
                # Presence is not enough for a hand-written .sup: prove ffmpeg's own PGS
                # decoder produces display sets out of it (num_rects 1 on a cue, 0 on the
                # clear). This is the property `subtitle_image_pgs` actually grades.
                try:
                    f = json.loads(run(["ffprobe", "-v", "error", "-select_streams", "s:%d" % i,
                                        "-show_frames", "-of", "json", str(path)]))
                    frames = f.get("frames", [])
                    rects = [fr.get("num_rects", 0) for fr in frames]
                    cues = sorted(float(fr.get("pts_time") or 0) for fr in frames
                                  if fr.get("num_rects"))
                except (Fail, ValueError):
                    rects, cues = [], []
                rec["subtitles"][i]["decoded_display_sets"] = len(rects)
                rec["subtitles"][i]["decoded_cues"] = len(cues)
                rec["subtitles"][i]["last_cue_s"] = round(cues[-1], 2) if cues else None
                if not any(rects):
                    problems.append("subtitle[%d] PGS decoded 0 rects — the .sup did not "
                                    "survive the mux" % i)
                # Same coverage rule as the text tracks, and for the same reason:
                # `subtitle_image_pgs` seeds 600 s and only ever looks from there on, so
                # "some display set exists" is not the property it grades.
                elif len(cues) < len(want_cues):
                    problems.append("subtitle[%d] PGS decoded %d cues, wanted %d"
                                    % (i, len(cues), len(want_cues)))
                elif want_cues and cues[-1] < want_cues[-1] - 1.0:
                    problems.append("subtitle[%d] last PGS cue at %.1fs, wanted one at %ds"
                                    % (i, cues[-1], want_cues[-1]))

    # ---- THE DECLARATION (pipeline tier). What `/tmp/plxnative-playurl` will tell the
    # television this stream IS, read back against what it actually is. The app builds its
    # Load payload from the declaration and never consults the container, so a declaration
    # that has drifted from its own media is invisible everywhere but the panel — an H264
    # payload over an HEVC elementary stream, or an audio lane that matches nothing — and
    # the case then fails pointing squarely at the player.
    dec = spec.get("declare")
    if dec:
        rec["declare"] = dict(dec)
        if vs:
            if vs[0].get("codec_name") != dec["vcodec"]:
                problems.append("declares vcodec %r, the file is %r"
                                % (dec["vcodec"], vs[0].get("codec_name")))
            # The payload's esInfo frame rate — the engine prints it as `fps=` on the `load:`
            # line to three decimals, so a clip built at another rate reads there as a
            # payload bug rather than as a fixture that was never 24p.
            got_fps = _rate(vs[0].get("avg_frame_rate") or vs[0].get("r_frame_rate"))
            if dec.get("fps") and abs(got_fps - dec["fps"]) > 0.01:
                problems.append("declares fps %.3f, the file runs at %.3f"
                                % (dec["fps"], got_fps))
        # `ff.rs::audio_stream_matching` feeds the FIRST audio stream whose
        # `avcodec_get_name` equals the declared codec, case-insensitively — so the
        # declaration is also the lane SELECTOR, and the stream index it lands on is exactly
        # what `a=#<idx>` in the `ff:` line reports. Recorded, because on the multi-audio
        # shape that index is the assertion, and deriving it from the same probe that just
        # graded the track order is how the harness avoids counting streams by hand.
        want_a = (dec.get("acodec") or "").lower()
        lane = next((i for i, s in enumerate(streams)
                     if s.get("codec_type") == "audio"
                     and (s.get("codec_name") or "").lower() == want_a), None)
        if want_a and lane is None:
            problems.append("declares acodec %r and no audio stream is that codec (have %s) "
                            "— the app would silently fall back to av_find_best_stream"
                            % (dec["acodec"], ", ".join(str(s.get("codec_name")) for s in aus)))
        rec["declare_audio_stream"] = lane
        # All three DV fields the trigger carries, against the container's own record. A
        # declared profile the media does not have is the one way the DolbyHdrInfo splice can
        # be exercised with nothing behind it — the payload node would be built and the
        # television would be told about a base layer that is not there.
        want_dv, got_dv = dec.get("dovi") or {}, (rec.get("video") or {}).get("dovi") or {}
        for f in ("profile", "bl_compat", "el_present"):
            if int(want_dv.get(f) or 0) != int(got_dv.get(f) or 0):
                problems.append("declares dovi %s=%d, the container's DV record says %d"
                                % (f, int(want_dv.get(f) or 0), int(got_dv.get(f) or 0)))
        if dec.get("atmos"):
            # No shape in this tier can carry Atmos — PIPE_SHAPES says why TrueHD is absent —
            # so this is a declaration with no media under it.
            problems.append("declares atmos, and no shape in this tier carries it")

    for s in spec.get("subs", []):
        if s.get("sidecar"):
            lang2 = SIDECAR_LANG.get(s["lang"], s["lang"])
            side = path.parent / (path.stem + "." + lang2 + ".srt")
            rec.setdefault("sidecars", []).append(side.name)
            if not side.exists():
                problems.append("sidecar %s missing" % side.name)
    return rec, problems


SIDECAR_LANG = {"eng": "en", "rus": "ru", "spa": "es", "fra": "fr", "deu": "de"}


# ---------------------------------------------------------------------------------------
# The marker segments, verified ACROSS the episode pair. verify() grades one file against
# its spec and structurally cannot see this: "the intro is identical in both episodes and
# the body is not" is a property of the PAIR, and it is the property three cases depend on.
# It is also the one this generator got backwards on its first pass — differing in video,
# identical in audio, which is the opposite of what Plex's fingerprinter reads.
# ---------------------------------------------------------------------------------------
def _seg_md5(path, kind, start, secs):
    """md5 of DECODED video or audio over a window — content, not container bytes."""
    argv = ["ffmpeg", "-v", "error", "-nostdin", "-ss", _g(start), "-t", _g(secs), "-i", str(path)]
    argv += ["-map", "0:v:0", "-an"] if kind == "v" else ["-map", "0:a:0", "-vn"]
    return run(argv + ["-f", "md5", "-"]).strip()


def _mean_luma(path, at):
    """Mean luma of one frame, 0-255. Cheap black-frame test with no filter dependencies."""
    raw = subprocess.run(["ffmpeg", "-v", "error", "-nostdin", "-ss", _g(at), "-i", str(path),
                          "-frames:v", "1", "-vf", "scale=64:36", "-pix_fmt", "gray",
                          "-f", "rawvideo", "-"], stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE).stdout
    return (sum(raw) / len(raw)) if raw else -1.0


def verify_markers(ctx, spec, dur, paths):
    """(rec, problems) for the intro/credits segments of an episode PAIR."""
    intro_end, credits_start = marker_plan(ctx, spec, dur)
    if intro_end is None or len(paths) < 2:
        return {}, []
    p1, p2 = paths[0], paths[1]
    rec, problems = {"intro_end_s": intro_end, "credits_start_s": credits_start}, []

    mid = max(0.0, intro_end / 2.0 - 1.5)
    a_intro = [_seg_md5(p, "a", mid, 3.0) for p in (p1, p2)]
    rec["intro_audio_identical"] = a_intro[0] == a_intro[1]
    if not rec["intro_audio_identical"]:
        problems.append("the intro audio DIFFERS between the two episodes — Plex "
                        "fingerprints audio and looks for the matching stretch, so nothing "
                        "would be detected")

    a_body = [_seg_md5(p, "a", intro_end + 10, 3.0) for p in (p1, p2)]
    rec["body_audio_differs"] = a_body[0] != a_body[1]
    if not rec["body_audio_differs"]:
        problems.append("the audio AFTER the intro is identical in both episodes — the "
                        "match then runs past the halfway point, which Plex discards")

    v_intro = [_seg_md5(p, "v", mid, 2.0) for p in (p1, p2)]
    rec["intro_video_identical"] = v_intro[0] == v_intro[1]
    if not rec["intro_video_identical"]:
        problems.append("the intro PICTURE differs between the episodes — something "
                        "per-episode (the layout banner names S01E01/S01E02) is being drawn "
                        "inside the intro window")

    if credits_start:
        luma = [_mean_luma(p, credits_start + 10) for p in (p1, p2)]
        rec["credits_mean_luma"] = [round(x, 1) for x in luma]
        if max(luma) > 12.0:
            problems.append("the credits tail is not black (mean luma %.1f) — Plex derives "
                            "a `final` credits marker from a credits-LIKE tail" % max(luma))
        a_cred = _seg_md5(p1, "a", credits_start + 10, 3.0)
        rec["credits_audio_differs"] = a_cred != a_body[0]
        if not rec["credits_audio_differs"]:
            problems.append("the credits tail carries the same audio bed as the body")
    return rec, problems


# ---------------------------------------------------------------------------------------
# Preflight. Missing OPTIONAL tooling SKIPS its shapes with a printed reason and never
# aborts — the same skip channel `tests/run.py` uses for an unmapped item, and the house
# rule: a partial set that says what it is beats an all-or-nothing failure.
# ---------------------------------------------------------------------------------------
EXPERIMENTAL_ENC = ("this ffmpeg has no %s encoder (experimental; a build can omit it).\n"
                    "            ffmpeg -hide_banner -encoders | grep -E ' truehd| dca| vorbis'\n"
                    "            Homebrew's ffmpeg normally has all three.")

BREW = {
    "ffmpeg": "brew install ffmpeg",
    "libx264": "brew install ffmpeg          # libx264 ships with it",
    "libx265": "brew install ffmpeg          # libx265 ships with it",
    "libsvtav1": "brew install ffmpeg          # libsvtav1 ships with it",
    "libopus": "brew install ffmpeg          # libopus ships with it",
    "ac3": "brew install ffmpeg", "eac3": "brew install ffmpeg",
    "aac": "brew install ffmpeg",
    # truehd/dca/vorbis are EXPERIMENTAL encoders a build can be configured without, and
    # anyone reading this line already has ffmpeg — "brew install ffmpeg" was the thing
    # that just failed them. Name the check instead.
    "truehd": EXPERIMENTAL_ENC % "truehd",
    "dca": EXPERIMENTAL_ENC % "dca",
    "vorbis": EXPERIMENTAL_ENC % "vorbis",
    "flac": "brew install ffmpeg",
    "mkvmerge": "brew install mkvtoolnix",
    "dovi_tool": "brew install dovi_tool",
}


def shape_requirements(key, spec):
    encs = set()
    encs.add({"h264": "libx264", "hevc": "libx265", "av1": "libsvtav1"}[spec["video"]["codec"]])
    for a in spec.get("audio", []):
        encs.add(AENC[a["codec"]][0])
    return sorted(encs), list(spec.get("tools", []))


def preflight(keys, shapes):
    print("== preflight")
    have_ffmpeg = shutil.which("ffmpeg") and shutil.which("ffprobe")
    if not have_ffmpeg:
        print("  ffmpeg/ffprobe: MISSING — nothing can be built.\n     %s" % BREW["ffmpeg"])
        raise SystemExit(2)
    encoders = ffmpeg_encoders()
    versions = {
        "ffmpeg": (tool_version("ffmpeg", ["-version"]) or "").replace("ffmpeg version ", ""),
        "mkvmerge": tool_version("mkvmerge"),
        "dovi_tool": tool_version("dovi_tool"),
        "SUPer": tool_version("supercli") or tool_version("SUPer"),
    }
    print("  ffmpeg      %s" % versions["ffmpeg"])
    for t in ("mkvmerge", "dovi_tool"):
        print("  %-11s %s" % (t, versions[t] or "MISSING"))
    print("  %-11s %s" % ("SUPer",
                          (versions["SUPer"] or "absent") +
                          "  (not required — this script writes PGS itself; see trap 10)"))

    plan, skipped = [], []
    for k in keys:
        spec = shapes[k]
        need_enc, need_tool = shape_requirements(k, spec)
        miss_e = [e for e in need_enc if e not in encoders]
        miss_t = [t for t in need_tool if not shutil.which(t)]
        if miss_e or miss_t:
            why = ", ".join(miss_e + miss_t)
            fix = sorted({BREW.get(x, "install " + x) for x in (miss_e + miss_t)})
            skipped.append((k, why, fix))
        else:
            plan.append(k)
    if skipped:
        print("\n  SKIPPING %d shape(s) for missing tooling:" % len(skipped))
        for k, why, fix in skipped:
            print("    %-32s needs %s" % (k, why))
            for f in fix:
                print("        %s" % f)
    return plan, skipped, versions


def fmt_secs(s):
    return "%d:%02d" % (int(s) // 60, int(s) % 60)


def main(argv=None):
    ap = argparse.ArgumentParser(
        description="Generate the media shapes tests/manifest.json needs, from lavfi.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="integration tier: point two Plex libraries (Movies, TV Shows) at <out> with "
               "the 'Personal Media' agents, then map the shape keys from fixtures.json into "
               "tests/manifest.local.json. pipeline tier: no Plex — serve <out>/pipeline with "
               "tests/serve_fixtures.py. See README.md.")
    ap.add_argument("--tier", choices=sorted(TIERS), default="integration",
                    help="which pack to build. `integration` (the default, so every existing "
                         "invocation is unchanged) is the Plex-scannable set the on-device "
                         "suite maps; `pipeline` is the flat %ds clips the player-pipeline "
                         "tier plays over HTTP with no Plex anywhere. They land in different "
                         "directories and keep separate fixtures.json documents."
                         % PIPE_SECS)
    ap.add_argument("--out", default=os.environ.get("FIXTURES_OUT")
                    or str(Path.home() / "plxnative-fixtures"),
                    help="output root (default: $FIXTURES_OUT, else ~/plxnative-fixtures). "
                         "Never inside the repo. `--tier pipeline` writes to <out>/pipeline.")
    # `--quick` and `--secs` are one knob under two names, hence the exclusive group: given
    # both, the loser would silently decide the length of a 20-minute build.
    length = ap.add_mutually_exclusive_group()
    length.add_argument("--quick", action="store_true",
                        help="build every shape at ~%ds so the run takes a couple of minutes. "
                             "Shapes are structurally correct but TOO SHORT for the suite's "
                             "seeks and resumes — development only." % QUICK_SECS)
    length.add_argument("--secs", type=int, metavar="N",
                        help="build every shape at N seconds instead of the length it "
                             "declares. The general form of --quick, and it carries the same "
                             "warning: anything shorter than the shape declares is recorded "
                             "`quick` in fixtures.json and is not suite-valid.")
    # `append`, not a bare string: `--only A --only B` is a natural thing to type and the
    # single-value form silently kept only B. Each value is still a comma list.
    ap.add_argument("--only", action="append", default=[],
                    help="shape keys, comma-separated and/or repeated (see --list)")
    ap.add_argument("--list", action="store_true", help="list the shapes and exit")
    ap.add_argument("--force", action="store_true",
                    help="rebuild even if the output verifies (also the only way to SHORTEN "
                         "an existing full-length file with --quick/--secs)")
    ap.add_argument("--keep-work", action="store_true", help="keep intermediates in <out>/.work")
    ap.add_argument("--no-markers", "--no-intro", dest="markers", action="store_false",
                    default=True,
                    help="do not splice the shared intro segment and black credits tail "
                         "into the episode pair")
    args = ap.parse_args(argv)
    if args.secs is not None and args.secs < 1:
        print("--secs must be at least 1", file=sys.stderr)
        return 2

    tier = TIERS[args.tier]
    shapes = tier["shapes"]
    # One name for "the length this run builds at", None meaning "whatever each shape says".
    secs = QUICK_SECS if args.quick else args.secs

    if args.list:
        label = Path("<out>") / tier["subdir"] if tier["subdir"] else Path("<out>")
        print("%-34s %-8s %-9s %s" % ("shape", "kind", "duration", "output"))
        for k, spec in shapes.items():
            for kk, pth in out_paths(label, k, spec):
                print("%-34s %-8s %8ds  %s"
                      % (kk, spec["kind"], shape_duration(spec, secs), pth))
        # ...and the tooling report, because `--list` is what the README sends a newcomer to FIRST
        # and "which shapes can this machine actually build" is the only question they have at that
        # point. Without this the table above reads as ten happy rows to somebody with no
        # `dovi_tool`, who then learns otherwise twenty minutes into a build.
        print()
        preflight(list(shapes), shapes)
        return 0

    root = Path(args.out).expanduser()
    if tier["subdir"] and root.name != tier["subdir"]:
        # A subdirectory rather than a second `--out`: the two packs are generated by the same
        # command and read by the same people, and one place to point at is one place to
        # delete. Nothing scans it — a Plex library is pointed at <out>/Movies and
        # <out>/TV Shows, so a sibling directory of clips is invisible to the scanner.
        #
        # The `root.name != subdir` half makes the append IDEMPOTENT, and that is not
        # defensive habit — this seam has two sides and they disagreed. `make
        # fixtures-pipeline` passes `$(FIXTURES_OUT)/pipeline`, while `tests/run.py
        # --pipeline` and `tests/serve_fixtures.py` both default their root to
        # `$FIXTURES_OUT/pipeline`. Appending blind wrote the pack to
        # `<out>/pipeline/pipeline`: still outside the repo, so the guard below stayed quiet,
        # and every pipeline case then skipped with "no fixture … in <out>/pipeline — run
        # `make fixtures-pipeline`" — the suite telling you to re-run the command that had
        # just succeeded, with nothing on either side erroring. Accepting BOTH spellings of
        # the same directory is what keeps that from depending on which lane lands last.
        root = root / tier["subdir"]
    root = root.resolve()
    try:
        root.relative_to(REPO_ROOT)
    except ValueError:
        pass
    else:
        print("refusing to write media inside the repository (%s).\n"
              "Pick an --out outside %s." % (root, REPO_ROOT), file=sys.stderr)
        return 2

    wanted = list(shapes)
    if args.only:
        alias = aliases(shapes)
        wanted = []
        for chunk in args.only:
            for name in [x.strip() for x in chunk.split(",") if x.strip()]:
                k = alias.get(name, name)
                if k not in shapes:
                    print("unknown shape %r (see --list)" % name, file=sys.stderr)
                    return 2
                if k not in wanted:
                    wanted.append(k)

    plan, skipped, versions = preflight(wanted, shapes)
    if not plan and not skipped:
        print("\nnothing to build.")
        return 2

    total_secs = sum(shape_duration(shapes[k], secs) * shapes[k]["rate"]
                     * shapes[k].get("episodes", 1) for k in plan)
    total_bytes = sum(shape_duration(shapes[k], secs) * tier["mbit"][k] / 8 * 1e6
                      * shapes[k].get("episodes", 1) for k in plan)
    tag = " [QUICK]" if args.quick else (" [%ds]" % secs if secs is not None else "")
    print("\n== plan: %d shape(s)%s%s -> %s"
          % (len(plan), tag, "" if args.tier == "integration" else " [%s]" % args.tier, root))
    print("   estimated encode time ~%s, estimated size ~%.2f GB"
          % (fmt_secs(total_secs), total_bytes / 1e9))
    # An EMPTY plan is not an error and does not return early: everything asked for was
    # skipped for missing optional tooling, which is the documented skip contract ("it
    # never aborts") — and the run still has work to do, because the records those shapes
    # left in an earlier fixtures.json have to be re-checked and dropped if they no longer
    # describe what is on disk. Returning here (which is what this did) left a document
    # claiming `dovi: {profile: 8}` over a file somebody had since replaced, on the machine
    # least able to notice. It also used to exit 1 while the same condition with one
    # buildable shape beside it exited 0.
    if not plan:
        print("   (every shape asked for was skipped — re-checking the existing records)")
    if args.quick:
        print("   QUICK: every clip is %ds. Structurally right, but every seek, resume and\n"
              "   marker depth the suite asserts is DEEPER than that, and a %ds item hits EOF\n"
              "   inside a case's run_secs and fires the finish -> Up Next chain. Do not point\n"
              "   the harness at a --quick set and read the result as a player verdict."
              % (QUICK_SECS, QUICK_SECS))
    elif secs is not None and any(secs < shapes[k]["duration"] for k in plan):
        # The same warning for the general knob, and it has to be said for BOTH tiers: down
        # in the pipeline tier there is no Up Next to fire, but a clip that reaches EOF still
        # ends the session while a case is still reading the log.
        print("   %ds clips: shorter than the length these shapes declare, so every one of\n"
              "   them is recorded `quick` in fixtures.json. Development media — a clip that\n"
              "   hits EOF inside a case's window ends the session under it." % secs)

    work = root / ".work"
    work.mkdir(parents=True, exist_ok=True)
    manifest_path = root / "fixtures.json"
    doc = {}
    if manifest_path.exists():
        try:
            doc = json.loads(manifest_path.read_text())
        except ValueError:
            doc = {}
    shapes_doc = doc.get("shapes", {})

    ctx = args_ctx(args)
    built, reused, failed, marker_bad = [], [], [], []
    t0 = time.time()
    for k in plan:
        spec = shapes[k]
        dur = shape_duration(spec, secs)
        for out_key, dest in out_paths(root, k, spec):
            ep = None
            if spec["kind"] == "episode" and spec.get("episodes", 1) > 1:
                ep = 1 if out_key == k else 2
            label = "%-34s %5ds" % (out_key, dur)
            if dest.exists() and not args.force:
                rec, problems = verify(k, spec, dur, dest, ep)
                # LONGER than this run asked for is not a defect: it is a full-length file
                # and a later `--quick` run must not silently shorten it. Before this, a
                # `--quick` iteration on one shape rebuilt the whole finished set at 20 s,
                # and the README's own "a quick set is not suite-valid" warning then applied
                # to media the contributor believed was full length. `--force` still shortens.
                longer = ((rec.get("duration_s") or 0) > dur
                          and problems and all(p.startswith("duration ") for p in problems))
                if not problems or longer:
                    print("  skip  %s  (%s)"
                          % (label, "already correct" if not problems else
                             "%.0fs on disk, longer than asked — kept" % rec["duration_s"]))
                    rec["path"] = str(dest.relative_to(root))
                    rec["layout"] = layout_lines(k, spec, int(round(rec["duration_s"])), ep)
                    if spec.get("library_title"):
                        rec["library_title"] = spec["library_title"]
                    rec["kind"] = spec["kind"]
                    shapes_doc[out_key] = rec
                    reused.append(out_key)
                    continue
                print("  redo  %s  (%s)" % (label, problems[0]))
            print("  build %s ..." % label, end="", flush=True)
            ts = time.time()
            try:
                if spec.get("builder") == "dovi":
                    lines = build_dovi(ctx, k, spec, dur, ep, dest, work)
                else:
                    lines = build_generic(ctx, k, spec, dur, ep, dest, work)
            except Fail as e:
                print(" FAILED")
                print("        %s" % str(e).replace("\n", "\n        "))
                # Drop any record a PREVIOUS run left for this key: fixtures.json is read
                # as ground truth by whatever fills manifest.local.json, and a stale entry
                # describing a file that is now half-written is worse than no entry.
                shapes_doc.pop(out_key, None)
                failed.append((out_key, str(e).splitlines()[0]))
                continue
            rec, problems = verify(k, spec, dur, dest, ep)
            if problems:
                print(" WRONG SHAPE")
                for p in problems:
                    print("        - %s" % p)
                shapes_doc.pop(out_key, None)
                failed.append((out_key, problems[0]))
                continue
            print(" ok  %s  %.0f MB" % (fmt_secs(time.time() - ts), rec["size_bytes"] / 1e6))
            rec["path"] = str(dest.relative_to(root))
            rec["layout"] = lines
            # Only where there IS one. A flat pipeline clip has no library and no title for a
            # scanner to match, and a record carrying an invented one would read as a shape
            # somebody could point a Plex library at.
            if spec.get("library_title"):
                rec["library_title"] = spec["library_title"]
            rec["kind"] = spec["kind"]
            shapes_doc[out_key] = rec
            built.append(out_key)

        # The intro/credits segments are a property of the PAIR, so they are graded once
        # both episodes are on disk — see verify_markers. A failure here does NOT drop the
        # records: the files are still correct fixtures for the shape's other five cases.
        if marker_plan(ctx, spec, dur)[0] is not None:
            paths = [d for _, d in out_paths(root, k, spec)]
            if all(p.exists() for p in paths):
                mrec, mprobs = verify_markers(ctx, spec, dur, paths)
                mrec["ok"] = not mprobs
                for out_key, _ in out_paths(root, k, spec):
                    if out_key in shapes_doc:
                        shapes_doc[out_key]["markers"] = mrec
                if mprobs:
                    print("  MARKERS %-32s the intro/credits premise is broken:" % k)
                    for p in mprobs:
                        print("        - %s" % p)
                    marker_bad.append(k)
                else:
                    print("  marker %-32s intro shared, body distinct, credits black" % k)

    if not args.keep_work:
        shutil.rmtree(work, ignore_errors=True)

    # ---- Records this run did not touch. fixtures.json accumulates ACROSS runs — a
    # `--only` run, or a run on a machine without dovi_tool, leaves every other key in the
    # document — and until now those were copied through verbatim under a fresh
    # `generated_at`, never re-checked and not even tested for the file still existing. The
    # document is what a resolver maps into tests/manifest.local.json, so a carried-forward
    # record pointing at a deleted or replaced file maps a shape key onto media that is not
    # that shape, and the harness then fails as if the player had regressed. Re-verify each
    # one (~50 ms) and drop the ones that no longer hold. That also covers the tool-SKIPPED
    # shapes, whose whole point is that their file may be stale from an earlier build.
    touched = set(built) | set(reused) | {x for x, _ in failed}
    index = shape_index(shapes)
    for out_key in sorted(shapes_doc):
        if out_key in touched:
            continue
        ent = index.get(out_key)
        if ent is None:
            print("  drop  %-34s (no such shape any more)" % out_key)
            shapes_doc.pop(out_key, None)
            continue
        kk, spec2, ep2 = ent
        dest2 = root / shapes_doc[out_key].get("path", "")
        if not shapes_doc[out_key].get("path") or not dest2.exists():
            print("  drop  %-34s (file is gone)" % out_key)
            shapes_doc.pop(out_key, None)
            continue
        # Graded at ITS OWN length — the length the RECORD says this file was built at, so
        # a shortened clip carried through a full-length run is still correct and vice versa,
        # and a file somebody has since REPLACED still fails on the duration it no longer has.
        #
        # This was a two-valued derivation (`QUICK_SECS if measured < declared*0.9 else
        # declared`), which is exactly as many lengths as the script used to have. It was the
        # single most load-bearing line for adding a third: a 60 s pipeline record that this
        # run never asked about was graded at 20 s, failed on `duration 60.02s, wanted 20s
        # (+-2.0)`, and was dropped — silently, out of a document nobody had touched, on the
        # only path that never prints what it built.
        dur2 = shapes_doc[out_key].get("duration_s")
        if not dur2:
            # A record from before duration_s existed, or one hand-edited: ask the file.
            try:
                dur2 = float(probe(dest2)["format"]["duration"])
            except (Fail, KeyError, ValueError):
                dur2 = spec2["duration"]
        rec2, probs2 = verify(kk, spec2, dur2, dest2, ep2)
        if probs2:
            print("  drop  %-34s (%s)" % (out_key, probs2[0]))
            shapes_doc.pop(out_key, None)
            continue
        rec2["path"] = shapes_doc[out_key]["path"]
        rec2["layout"] = layout_lines(kk, spec2, round(rec2["duration_s"]), ep2)
        if spec2.get("library_title"):
            rec2["library_title"] = spec2["library_title"]
        rec2["kind"] = spec2["kind"]
        if "markers" in shapes_doc[out_key]:
            rec2["markers"] = shapes_doc[out_key]["markers"]
        shapes_doc[out_key] = rec2

    # `quick` is a property of each FILE (verify() derives it from the measured duration),
    # never of the invocation. Stamped per-run over a cross-run document it could say
    # `false` over a set that was still 20 s clips — the single machine-readable warning
    # that a set is not suite-valid, positively lying.
    quicks = {bool(r.get("quick")) for r in shapes_doc.values()}
    quick_flag = "mixed" if len(quicks) > 1 else (quicks.pop() if quicks else bool(args.quick))

    doc = {
        "generator": "tests/fixtures/make_fixtures.py",
        "generator_version": GEN_VERSION,
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        # Which SUITE TIER this pack is for. Per-record as well (verify() stamps it from the
        # shape's own table), because the two packs keep separate documents today and a
        # top-level word alone would not survive anyone merging them.
        "tier": args.tier,
        "quick": quick_flag,
        "skipped": {k: why for k, why, _ in skipped},
        "tool_versions": versions,
        "synthetic": True,
        "_comment": ("Shape key -> the file that shape was built as, plus every property "
                     "verify() read back out of it. A resolver fills tests/manifest.local.json "
                     "'items' from the keys here. Synthetic media: a green suite run against "
                     "it is a no-regression claim about the shapes the suite names, not a "
                     "claim about real-world media. Every record here was verified against "
                     "the file on disk during the run that wrote this document, including "
                     "the ones the run did not rebuild. `quick` per shape means that file is "
                     "SHORTER than its shape declares (a --quick or --secs development clip) "
                     "and is NOT suite-valid; the top-level `quick` "
                     "is true/false/\"mixed\" over the whole set. `tier` says which suite "
                     "tier the pack serves: `integration` records map to ratingKeys in "
                     "tests/manifest.local.json, while a `pipeline` record is a file served "
                     "over HTTP by tests/serve_fixtures.py and its `declare` block is the "
                     "playback half of the /tmp/plxnative-playurl trigger, verified here "
                     "against the media it describes."),
        "shapes": shapes_doc,
    }
    manifest_path.write_text(json.dumps(doc, indent=2, sort_keys=False) + "\n")

    print("\n== done in %s" % fmt_secs(time.time() - t0))
    print("   built %d, reused %d, failed %d, skipped %d"
          % (len(built), len(reused), len(failed), len(skipped)))
    for k, why in failed:
        print("   FAILED %-32s %s" % (k, why))
    for k in marker_bad:
        print("   MARKER %-32s files are usable; the three marker_* cases are not" % k)
    for k, why, _ in skipped:
        print("   SKIP   %-32s needs %s" % (k, why))
    if quick_flag == "mixed":
        print("   NB this set MIXES quick and full-length files (see the per-shape `quick`\n"
              "      flag in fixtures.json). It is not suite-valid as it stands.")
    print("   manifest: %s" % manifest_path)
    if built or reused:
        if args.tier == "pipeline":
            # No Plex, no scan, no mapping step: the next thing that touches these files is a
            # static file server on this Mac. The port is the one serve_fixtures.py documents
            # as its own default; the harness picks its own.
            print("\n   Next: serve this directory to the television —")
            print("     ./tests/serve_fixtures.py --root %s --port 8020" % root)
            print("   Each case then arms /tmp/plxnative-playurl with a URL into that server")
            print("   plus the `declare` block recorded beside the file in fixtures.json.")
            print("   Start it ONCE with a human at the keyboard: the macOS application")
            print("   firewall drops the TV's connections to a new python listener silently.")
        else:
            print("\n   Next: add TWO Plex libraries with the 'Personal Media' agents —")
            print("     Movies    -> %s" % (root / "Movies"))
            print("     TV Shows  -> %s" % (root / "TV Shows"))
            print("   then map the keys from fixtures.json into tests/manifest.local.json.")
    return 1 if (failed or marker_bad) else 0


class args_ctx:
    """The build-time knob the builders need, without passing argparse around.

    It was two until the length knob became `--secs`: `marker_plan` used to ask `not
    ctx.quick`, and now decides from the LENGTH it is handed, which is the only form of the
    question that stays right at a third length."""
    def __init__(self, args):
        self.markers = args.markers


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
