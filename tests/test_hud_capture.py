"""HUD preview extensions of the shared capture runner; no display or encoder is needed."""
import importlib.util
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = pathlib.Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("hud_screenshots", ROOT / "tools/screenshots.py")
shots = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shots)
DEFAULTS = {"canvas": "1920x1080", "settle_ms": 200, "after_ms": 100}


class Capture(unittest.TestCase):
    def test_virtual_input_replaces_wall_clock_shot_and_keeps_isolation(self):
        scene = {"name": "seek", "lang": "de", "virtual": {"frames": 12, "select": 3, "keys": "900:right"}}
        with patch.dict("os.environ", {"PLXNATIVE_TOKEN": "must-not-leak", "PLXNATIVE_DUMP_NO_HOLD": "1"}):
            env = shots.clean_env(pathlib.Path("/rt"), pathlib.Path("/rt/a.png"), scene, DEFAULTS, pathlib.Path("/assets"))
        self.assertNotIn("PLXNATIVE_TOKEN", env)
        self.assertNotIn("PLXNATIVE_DUMP_NO_HOLD", env)
        self.assertFalse(any(k.startswith("PLXNATIVE_SHOT") for k in env))
        self.assertEqual(env["PLXNATIVE_DUMP_FRAMES"], "12")
        self.assertEqual(env["PLXNATIVE_DUMP_KEYS"], "900:right")
        self.assertEqual(env["PLXNATIVE_APP_DIR"], "/assets")
        self.assertEqual(env["PLXNATIVE_LOCALE"], "de")

    def test_invalid_virtual_frame_bounds_fail_before_launch(self):
        for config in ({"frames": 0}, {"select": -1}, {"select": 1}, {"frames": True}, {"preroll": 9999}, {"no_hold": True}):
            with self.subTest(config=config), self.assertRaises(ValueError):
                shots.virtual_spec({"name": "x", "virtual": config})

    def test_frame_selection_requires_the_complete_debt_free_stream(self):
        defaults = {"canvas": "2x1"}
        scene = {"name": "x", "virtual": {"frames": 2, "select": 1, "preroll": 0}}
        summary = {"frames": 2, "preroll": 0, "width": 2, "height": 1,
                   "failed": None, "frames_with_debt": 0, "unconverted_takes": []}
        with tempfile.TemporaryDirectory() as path:
            root = pathlib.Path(path)
            (root / "frames.rgb").write_bytes(b"abcdef" + b"ghijkl")
            def write(**over):
                (root / "dump.json").write_text(json.dumps({**summary, **over}))
            write()
            with patch.object(shots.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, b"png")) as encode:
                self.assertEqual(shots.virtual_png(root, scene, defaults), b"png")
                self.assertEqual(encode.call_args.kwargs["input"], b"ghijkl")
            for over in ({"failed": "timeout"}, {"frames_with_debt": 1}, {"unconverted_takes": [[1, 2]]}, {"width": 3}):
                write(**over)
                with self.assertRaises(RuntimeError):
                    shots.virtual_png(root, scene, defaults)
            write()
            (root / "frames.rgb").write_bytes(b"short")
            with self.assertRaises(RuntimeError):
                shots.virtual_png(root, scene, defaults)


if __name__ == "__main__":
    unittest.main()
