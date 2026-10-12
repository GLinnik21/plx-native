"""Check malformed fixture input and validation under optimized Python."""
from pathlib import Path
import subprocess
import sys
import unittest

FIXTURES = Path(__file__).resolve().parents[1] / "tests" / "fixtures"


class AssHeaderFixtureTests(unittest.TestCase):
    def test_bad_element_lengths_fail_without_hanging(self):
        script = (
            "import sys; sys.path.insert(0, sys.argv[1]); "
            "import make_ass_header_fixture as f; list(f.elements(bytes.fromhex(sys.argv[2])))"
        )
        for data in ("00", "8100", "81", "88"):
            with self.subTest(data=data):
                result = subprocess.run(
                    [sys.executable, "-c", script, str(FIXTURES), data],
                    capture_output=True, text=True, timeout=2,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("ValueError", result.stderr)

    def test_probe_validation_survives_python_optimization(self):
        script = """
import sys
sys.path.insert(0, sys.argv[1])
import make_ass_header_fixture as f
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch
def fake_ffmpeg(args, **kwargs):
    Path(args[-1]).write_bytes(bytes.fromhex('1a45dfa380'))
with TemporaryDirectory() as temp:
    with patch.object(f, 'OUTPUT', Path(temp) / 'fixture.mkv'), \\
         patch.object(f.subprocess, 'run', side_effect=fake_ffmpeg), \\
         patch.object(f.subprocess, 'check_output', return_value=b'{"streams":[]}'):
        f.main()
"""
        result = subprocess.run(
            [sys.executable, "-O", "-c", script, str(FIXTURES)],
            capture_output=True, text=True, timeout=3,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ValueError", result.stderr)


if __name__ == "__main__":
    unittest.main()
