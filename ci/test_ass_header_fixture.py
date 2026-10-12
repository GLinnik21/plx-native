"""Check malformed fixture input and validation under optimized Python."""
from pathlib import Path
import importlib.util
import subprocess
import sys
import unittest
from tempfile import TemporaryDirectory
from unittest.mock import patch

FIXTURES = Path(__file__).resolve().parents[1] / "tests" / "fixtures"


spec = importlib.util.spec_from_file_location("fixture", FIXTURES / "make_ass_header_fixture.py")
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class AssHeaderFixtureTests(unittest.TestCase):
    def test_empty_block_reports_invalid_input(self):
        for ident in (b"\xa1", b"\xa3"):
            for body in (b"", b"\x82", b"\x82\0", b"\x82\0\0"):
                with self.subTest(ident=ident, body=body):
                    with self.assertRaises(ValueError):
                        fixture.terminate_packets(fixture.element(ident, body), 2)

    def test_bad_element_lengths_fail_without_hanging(self):
        script = (
            "import sys; sys.path.insert(0, sys.argv[1]); "
            "import make_ass_header_fixture as f; list(f.elements(bytes.fromhex(sys.argv[2])))"
        )
        for data in ("00", "8100", "81", "88"):
            with self.subTest(data=data):
                result = subprocess.run(
                    [sys.executable, "-c", script, str(FIXTURES), data],
                    capture_output=True, text=True, timeout=30,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("ValueError", result.stderr)

    def source(self, packet_track=130):
        def track(number):
            return fixture.element(b"\xae", fixture.element(b"\xd7", bytes([number]))
                                   + fixture.element(b"\x86", b"S_TEXT/ASS")
                                   + fixture.element(b"\x63\xa2", b"header"))

        def block(number):
            return fixture.element(b"\xa1", (0x4000 | number).to_bytes(2, "big")
                                   + b"\0\0\0caption")
        tracks = fixture.element(bytes.fromhex("1654ae6b"), track(130) + track(131))
        cluster = fixture.element(bytes.fromhex("1f43b675"),
                                  fixture.element(b"\xa0", block(packet_track))
                                  + fixture.element(b"\xa0", block(packet_track))
                                  + fixture.element(b"\xa0", block(131)))
        return fixture.element(bytes.fromhex("18538067"), tracks + cluster)

    def test_track_numbers_from_the_inventory_choose_packets(self):
        segment = list(fixture.elements(self.source()))[0][1]
        tracks, cluster = list(fixture.elements(segment))
        _, number = fixture.replace_headers(tracks[1], fixture.HEADER.read_bytes())
        result, changed = fixture.terminate_packets(cluster[1], number)
        self.assertEqual(changed, 2)
        payloads = [list(fixture.elements(body))[0][1]
                    for _, body, _ in fixture.elements(result)]
        self.assertEqual([p.endswith(b"\0") for p in payloads], [True, True, False])

    def test_no_matching_packet_fails_before_publication(self):
        source = self.source(packet_track=129)

        def fake_ffmpeg(args, **kwargs):
            Path(args[-1]).write_bytes(source)

        with TemporaryDirectory() as temp:
            output = Path(temp) / "fixture.mkv"
            with patch.object(fixture, "OUTPUT", output), \
                 patch.object(fixture.subprocess, "run", side_effect=fake_ffmpeg):
                with self.assertRaisesRegex(ValueError, "changed 0"):
                    fixture.main()
            self.assertFalse(output.exists())

    def test_probe_validation_survives_python_optimization(self):
        script = """
import sys
sys.path.insert(0, sys.argv[1])
import make_ass_header_fixture as f
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch
def fake_ffmpeg(args, **kwargs):
    Path(args[-1]).write_bytes(bytes.fromhex(sys.argv[2]))
with TemporaryDirectory() as temp:
    with patch.object(f, 'OUTPUT', Path(temp) / 'fixture.mkv'), \\
         patch.object(f.subprocess, 'run', side_effect=fake_ffmpeg), \\
         patch.object(f.subprocess, 'check_output', return_value=b'{"streams":[]}'):
        f.main()
"""
        result = subprocess.run(
            [sys.executable, "-O", "-c", script, str(FIXTURES), self.source().hex()],
            capture_output=True, text=True, timeout=30,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Expected two ASS tracks with the captured headers", result.stderr)


if __name__ == "__main__":
    unittest.main()
