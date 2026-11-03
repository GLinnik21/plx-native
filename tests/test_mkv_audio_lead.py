"""`tests/mkv_audio_lead.py` reading the Tracks element of a file whose CRC-32 happens to look like a
TrackNumber (0xD7) or TrackType (0x83) field followed by a large size.

ffmpeg's Matroska muxer starts Tracks with a CRC-32 child (id 0xBF, four checksum bytes). The script
used to treat every child of Tracks as a TrackEntry, so it parsed those four checksum bytes as EBML
fields; the checksum depends on the fixture, whose picture is random noise, so about one run in a
hundred it began `D7` or `83` plus a size vint reaching far past the CRC element, `int.from_bytes`
swallowed that slice, and the closing `print` of the track table died with "Exceeds the limit (4300
digits) for integer string conversion" on Python 3.11+ (CI run 40728922069, the macOS FFmpeg job's
late-audio tests; passed on re-run). Host only: a synthetic file, no ffmpeg, no cargo."""
import contextlib
import io
import pathlib
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
    return m.elem(m.SIMPLEBLOCK, bytes([0x80 | track]) + rel_ms.to_bytes(2, "big") + b"\x80" + b"\x55" * 16)


def mkv(crc):
    """EBML header, then a Segment with Tracks (CRC-32 child first, as ffmpeg writes it; video = 1,
    audio = 2), a Void of non-zero bytes, and one Cluster holding a video and an audio block."""
    tracks = m.elem(TRACKS, m.elem(CRC32, crc) + track_entry(1, 1) + track_entry(2, 2))
    padding = m.elem(VOID, b"\xaa" * 20000)
    cluster = m.elem(m.CLUSTER, m.elem(m.TIMESTAMP, uint(0)) + simple_block(1, 0) + simple_block(2, 5))
    segment = m.SEGMENT.to_bytes(4, "big") + b"\x01\xff\xff\xff\xff\xff\xff\xff" + tracks + padding + cluster
    return m.elem(EBML, b"") + segment


def run(crc, lead_s=0.0):
    with tempfile.TemporaryDirectory() as d:
        src, dst = pathlib.Path(d, "in.mkv"), pathlib.Path(d, "out.mkv")
        src.write_bytes(mkv(crc))
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


if __name__ == "__main__":
    unittest.main()
