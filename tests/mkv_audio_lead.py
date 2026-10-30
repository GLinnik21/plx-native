#!/usr/bin/env python3
"""Rewrite an interleaved Matroska file so every audio block with timestamp < LEAD_S is stored
BEFORE the first video block (file order), as issue #600's muxer did. Presentation timestamps
are untouched: the first video block is still ts=0, it is just stored after LEAD_S of audio.

usage: mkv_audio_lead.py IN.mkv OUT.mkv LEAD_S [skew]

With the optional `skew` mode the lead is CONSTANT for the whole file instead of a prefix: every
audio block is written next to the video block LEAD_S earlier (audio at pts t sits beside video at
pts t-LEAD_S), so after the audio-only prefix the two interleave with the audio still LEAD_S ahead
for the rest of the file. That is the layout a muxer keeps once it has started writing audio early.
Drops SeekHead/Cues/Tags (they would point at the old offsets); the Segment is written with an
unknown size, as a live muxer would. Only SimpleBlock and BlockGroup cluster children are kept.
"""
import struct, sys

def rd_id(b, p):
    first = b[p]; n = 1
    while n <= 4 and not (first & (0x80 >> (n - 1))): n += 1
    return int.from_bytes(b[p:p+n], "big"), p + n

def rd_size(b, p):
    first = b[p]; n = 1
    while n <= 8 and not (first & (0x80 >> (n - 1))): n += 1
    v = first & ((0x80 >> (n - 1)) - 1)
    for i in range(1, n): v = (v << 8) | b[p+i]
    unknown = v == (1 << (7 * n)) - 1
    return (None if unknown else v), p + n

def wr_size(n):  # 8-byte size vint
    return bytes([0x01]) + n.to_bytes(7, "big")

def elem(eid, payload):
    return eid.to_bytes((eid.bit_length() + 7) // 8, "big") + wr_size(len(payload)) + payload

def children(b, p, end):
    while p < end:
        eid, q = rd_id(b, p); sz, q = rd_size(b, q)
        # Never past the parent: a child of a small element (a CRC-32's four checksum bytes parsed as
        # EBML) must not claim a size that runs on into its siblings.
        e = end if sz is None else min(q + sz, end)
        yield eid, p, q, e
        p = e

SEGMENT, CLUSTER, TIMESTAMP, SIMPLEBLOCK, BLOCKGROUP, BLOCK = 0x18538067, 0x1F43B675, 0xE7, 0xA3, 0xA0, 0xA1
TRACKS, TRACK_ENTRY = 0x1654AE6B, 0xAE
KEEP_TOP = {0x1549A966: "Info", TRACKS: "Tracks", 0x1043A770: "Chapters", 0x1941A469: "Attachments"}

def block_track_rel(b, q):  # q = start of Block payload
    tr, r = rd_size(b, q)
    rel = struct.unpack(">h", b[r:r+2])[0]
    return tr, r, rel

def main(src, dst, lead_s, skew=False):
    b = open(src, "rb").read()
    out = bytearray()
    blocks = []  # (abs_ts, track, raw element bytes, offset of rel ts within raw)
    tracks_types = {}
    for eid, p, q, e in children(b, 0, len(b)):
        if eid != SEGMENT:
            out += b[p:e]; continue  # EBML header
        seg_hdr = SEGMENT.to_bytes(4, "big") + bytes([0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])
        out += seg_hdr
        for cid, cp, cq, ce in children(b, q, e):
            if cid in KEEP_TOP:
                out += b[cp:ce]
                if cid == TRACKS:  # TrackEntry: TrackNumber 0xD7, TrackType 0x83
                    for tid, tp, tq, te in children(b, cq, ce):
                        # Tracks also holds a CRC-32 (0xBF) when ffmpeg wrote it: its checksum bytes
                        # are not EBML, and read as fields they can look like a number of any size.
                        if tid != TRACK_ENTRY: continue
                        num = typ = None
                        for fid, fp, fq, fe in children(b, tq, te):
                            if fid == 0xD7: num = int.from_bytes(b[fq:fe], "big")
                            if fid == 0x83: typ = int.from_bytes(b[fq:fe], "big")
                        tracks_types[num] = typ
            elif cid == CLUSTER:
                cts = 0
                for kid, kp, kq, ke in children(b, cq, ce):
                    if kid == TIMESTAMP: cts = int.from_bytes(b[kq:ke], "big")
                    elif kid == SIMPLEBLOCK:
                        tr, r, rel = block_track_rel(b, kq)
                        blocks.append((cts + rel, tr, bytes(b[kp:ke]), r - kp))
                    elif kid == BLOCKGROUP:
                        for gid, gp, gq, ge in children(b, kq, ke):
                            if gid == BLOCK:
                                tr, r, rel = block_track_rel(b, gq)
                                blocks.append((cts + rel, tr, bytes(b[kp:ke]), r - kp))
    VIDEO, AUDIO = 1, 2
    lead_ms = int(lead_s * 1000)
    is_a = lambda t: tracks_types.get(t) == AUDIO
    phase1 = [x for x in blocks if is_a(x[1]) and x[0] < lead_ms]
    phase2 = [x for x in blocks if not (is_a(x[1]) and x[0] < lead_ms)]
    if skew:
        # Merge two sequences, never re-sort a track's own blocks: a track keeps its ORIGINAL file
        # order (decode order; video with B-frames is NOT in pts order, and a decoder fed pts order
        # fails with "Could not find ref with POC"). The merge key is monotonic within each track:
        # the running maximum of the track's pts, minus the lead for audio. The sort is stable, so
        # equal keys (and a B-frame whose pts is below the running maximum) keep original order.
        phase1 = []
        run = {}
        keyed = []
        for x in blocks:
            a = is_a(x[1])
            run[x[1]] = max(run.get(x[1], x[0]), x[0])
            keyed.append((run[x[1]] - (lead_ms if a else 0), x))
        phase2 = [x for _, x in sorted(keyed, key=lambda k: k[0])]
    def emit(group):
        cts = min(x[0] for x in group)
        body = elem(TIMESTAMP, cts.to_bytes(4, "big"))
        for ts, tr, raw, off in group:
            raw = bytearray(raw); raw[off:off+2] = struct.pack(">h", ts - cts); body += raw
        out.extend(elem(CLUSTER, body))
    for seq in (phase1, phase2):
        group = []
        for x in seq:
            # A cluster's block timestamps are int16 offsets from its own, so audio and video that
            # are `lead` apart can never share one: a cluster holds one track class at a time.
            if group and (len(group) >= 64 or max(abs(x[0] - g[0]) for g in group[:1]) > 2000
                          or is_a(x[1]) != is_a(group[0][1])):
                emit(group); group = []
            group.append(x)
        if group: emit(group)
    open(dst, "wb").write(out)
    print(f"blocks={len(blocks)} audio-first={len(phase1)} ({sum(len(x[2]) for x in phase1)} B) then {len(phase2)}; tracks={tracks_types}")

if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2], float(sys.argv[3]), len(sys.argv) > 4 and sys.argv[4] == "skew")
