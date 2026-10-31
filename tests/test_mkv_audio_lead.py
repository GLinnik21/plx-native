"""`tests/mkv_audio_lead.py` reading the Tracks element of a file whose CRC-32 happens to look like a
TrackNumber (0xD7) or TrackType (0x83) field followed by a large size.

ffmpeg's Matroska muxer starts Tracks with a CRC-32 child (id 0xBF, four checksum bytes). The script
used to treat every child of Tracks as a TrackEntry, so it parsed those four checksum bytes as EBML
fields; the checksum depends on the fixture, whose picture is random noise, so about one run in a
hundred it began `D7` or `83` plus a size vint reaching far past the CRC element, `int.from_bytes`
swallowed that slice, and the closing `print` of the track table died with "Exceeds the limit (4300
digits) for integer string conversion" on Python 3.11+ (CI run 40728922069, the macOS FFmpeg job's
late-audio tests; passed on re-run). Host only: a synthetic file, no ffmpeg, no cargo.

A second class of input: an integer field wider than EBML's 8-byte maximum (a TrackNumber, a
TrackType, a Cluster Timestamp). No muxer writes one, but the script reads those fields as plain
integers, so one would reach the summary `print` as a huge integer. The script refuses such a field
with a message instead (by width: zero padding up to 8 bytes is fine).

A third: a Cluster timestamp whose VALUE cannot be written back. `emit` writes each new cluster's
Timestamp (the earliest block of its group: a Timestamp plus a signed 16-bit block offset) in four
unsigned bytes, so `ff ff ff ff` with a later block, or a small Timestamp with an earlier block,
would overflow there. The script refuses such a value with a message naming it."""
import contextlib
import io
import pathlib
import struct
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import mkv_audio_lead as m  # noqa: E402

EBML = 0x1A45DFA3
TRACKS, TRACK_ENTRY, TRACK_NUMBER, TRACK_TYPE, CRC32, VOID = 0x1654AE6B, 0xAE, 0xD7, 0x83, 0xBF, 0xEC


def uint(n):
    return n.to_bytes(max(1, (n.bit_length() + 7) // 8), "big")


def track_entry(number, kind):
    return m.elem(TRACK_ENTRY, m.elem(TRACK_NUMBER, uint(number)) + m.elem(TRACK_TYPE, uint(kind)))


def simple_block(track, rel_ms):
    """A SimpleBlock; the offset from the Cluster's Timestamp is a SIGNED 16-bit field."""
    return m.elem(m.SIMPLEBLOCK, bytes([0x80 | track]) + struct.pack(">h", rel_ms) + b"\x80" + b"\x55" * 16)


def mkv(crc, entries=None, timestamp=None, audio_rel=5):
    """EBML header, then a Segment with Tracks (CRC-32 child first, as ffmpeg writes it; video = 1,
    audio = 2), a Void of non-zero bytes, and one Cluster holding a video and an audio block.
    `entries` replaces the two TrackEntry children, `timestamp` the Cluster's Timestamp payload and
    `audio_rel` the audio block's (signed) offset from it."""
    if entries is None:
        entries = track_entry(1, 1) + track_entry(2, 2)
    tracks = m.elem(TRACKS, m.elem(CRC32, crc) + entries)
    padding = m.elem(VOID, b"\xaa" * 20000)
    stamp = uint(0) if timestamp is None else timestamp
    cluster = m.elem(m.CLUSTER, m.elem(m.TIMESTAMP, stamp) + simple_block(1, 0) + simple_block(2, audio_rel))
    segment = m.SEGMENT.to_bytes(4, "big") + b"\x01\xff\xff\xff\xff\xff\xff\xff" + tracks + padding + cluster
    return m.elem(EBML, b"") + segment


def run(crc, lead_s=0.0, **parts):
    with tempfile.TemporaryDirectory() as d:
        src, dst = pathlib.Path(d, "in.mkv"), pathlib.Path(d, "out.mkv")
        src.write_bytes(mkv(crc, **parts))
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            m.main(str(src), str(dst), lead_s)
        return out.getvalue(), dst.read_bytes()


class TracksParsing(unittest.TestCase):
    def test_the_track_table_is_exactly_the_track_entries(self):
        line, _ = run(bytes([0x11, 0x22, 0x33, 0x44]))
        self.assertIn("tracks={1: 1, 2: 2}", line)

    def test_a_crc_that_looks_like_a_track_number_with_a_huge_size_is_not_read_as_one(self):
        # 0xD7 = TrackNumber, 0x83 = TrackType; 0x7F 0xFE = a two-byte size vint of 16382 (0x7F 0xFF
        # is the unknown-size marker), far past the 4-byte CRC.
        for crc in (bytes([0xD7, 0x7F, 0xFE, 0x00]), bytes([0x83, 0x7F, 0xFE, 0x00])):
            with self.subTest(crc=crc.hex()):
                line, out = run(crc)
                self.assertIn("tracks={1: 1, 2: 2}", line)
                self.assertLess(len(line), 200, "the track table must not carry a huge integer")
                self.assertTrue(out.startswith(m.elem(EBML, b"")), "the EBML header is kept")


class WideIntegerFields(unittest.TestCase):
    """The script's controlled failure for a field no muxer writes is `SystemExit` with a message
    naming the field: never a `ValueError` from the summary line nor an `OverflowError` from
    writing the timestamp back."""

    CRC = bytes([0x11, 0x22, 0x33, 0x44])

    def refused(self, **parts):
        with self.assertRaises(SystemExit) as raised:
            run(self.CRC, **parts)
        return str(raised.exception)

    def test_a_track_number_wider_than_ebml_allows_is_refused(self):
        wide = m.elem(TRACK_ENTRY, m.elem(TRACK_NUMBER, b"\x01" * 2000) + m.elem(TRACK_TYPE, uint(1)))
        self.assertIn("TrackNumber", self.refused(entries=wide + track_entry(2, 2)))

    def test_a_track_type_wider_than_ebml_allows_is_refused(self):
        wide = m.elem(TRACK_ENTRY, m.elem(TRACK_NUMBER, uint(1)) + m.elem(TRACK_TYPE, b"\x01" * 2000))
        self.assertIn("TrackType", self.refused(entries=wide + track_entry(2, 2)))

    def test_a_cluster_timestamp_wider_than_ebml_allows_is_refused(self):
        self.assertIn("Cluster Timestamp", self.refused(timestamp=b"\x01" * 2000))

    def test_a_cluster_timestamp_of_five_significant_bytes_is_refused_where_it_is_written_back(self):
        # 2**32 reads fine as an EBML integer (five bytes) and cannot be written in four.
        self.assertIn("does not fit", self.refused(timestamp=b"\x01" + b"\x00" * 4))

    def test_a_cluster_timestamp_that_fits_four_bytes_alone_but_not_with_a_block_offset_is_refused(self):
        # 0xFFFFFFFF is four bytes wide, but the audio block sits 5 ms after it and is written into a
        # cluster of its own: the timestamp that is written back is 2**32 + 4.
        self.assertIn("does not fit", self.refused(timestamp=b"\xff\xff\xff\xff"))

    def test_a_block_offset_below_a_zero_cluster_timestamp_is_refused(self):
        # Timestamp 0, the audio block 5 ms BEFORE it (a signed offset): the audio cluster would have
        # to start at -5, which an unsigned field cannot hold.
        self.assertIn("does not fit", self.refused(timestamp=uint(0), audio_rel=-5))

    def test_a_zero_padded_cluster_timestamp_of_any_ebml_width_is_still_read(self):
        # Bounded by value, as the TrackNumber is: padding does not change a zero.
        for width in (5, 8):
            with self.subTest(width=width):
                line, out = run(self.CRC, timestamp=bytes(width))
                self.assertIn("tracks={1: 1, 2: 2}", line)
                self.assertTrue(out.startswith(m.elem(EBML, b"")))

    def test_an_eight_byte_track_number_is_still_read(self):
        # EBML's maximum, with leading zeros as a padded writer emits it.
        padded = m.elem(TRACK_ENTRY, m.elem(TRACK_NUMBER, bytes(7) + b"\x01") + m.elem(TRACK_TYPE, uint(1)))
        line, _ = run(self.CRC, entries=padded + track_entry(2, 2))
        self.assertIn("tracks={1: 1, 2: 2}", line)


if __name__ == "__main__":
    unittest.main()
