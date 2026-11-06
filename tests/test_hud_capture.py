"""HUD preview extensions of the shared capture runner; no display or encoder is needed."""
import contextlib
import importlib.util
import io
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import time
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


class _Server:
    """What `capture` asks of the mock's server: a port, and to be stopped."""
    server_address = ("127.0.0.1", 1)

    def shutdown(self):
        pass

    def server_close(self):
        pass


class _Pms:
    unknown = []


class InstanceRoot(unittest.TestCase):
    """`capture` boots the simulator in a fresh `plxnative-shot-<scene>-*` instance root (a virtual
    capture's frame stream makes it up to 0.7 GiB) and `render_set` stages into `plxnative-shots-*`.
    Both are gone when the run ends, whichever way it ends -- SIGTERM included. Unhandled, SIGTERM
    ends Python at once, with no `finally` and the simulator still running, and the root stays in
    `$TMPDIR`."""

    PNG = (b"\x89PNG\r\n\x1a\n" + (13).to_bytes(4, "big") + b"IHDR" + (1920).to_bytes(4, "big")
           + (1080).to_bytes(4, "big") + b"\x08\x02\x00\x00\x00")
    DEFAULTS = {"canvas": "1920x1080", "settle_ms": 1, "after_ms": 1, "timeout_s": 5, "stillclock_ms": 1}
    # `main` with the pieces that need a real mock catalogue or encoder replaced: argv is the
    # repository root, the fake simulator, the output directory and one scene of the manifest.
    CHILD = """
import importlib.util, pathlib, sys
from unittest.mock import patch
root = pathlib.Path(sys.argv[1])
spec = importlib.util.spec_from_file_location("shots", root / "tools" / "screenshots.py")
shots = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shots)


class Server:
    server_address = ("127.0.0.1", 1)
    shutdown = server_close = lambda self: None


class Pms:
    unknown = []


sys.argv = ["screenshots.py", "--bin", sys.argv[2], "--only", sys.argv[4], "--out", sys.argv[3]]
with patch.object(shots.mock_pms, "serve", return_value=(Server(), Pms())), \
        patch.object(shots.mock_pms, "CatalogLibrary"), patch.object(shots.shutil, "which", return_value="/bin/true"):
    shots.main()
"""

    def capture(self, scratch, script, scene=None, **kwargs):
        """`capture` against a fake simulator (`{png}` in the script is a valid capture), its
        instance roots under `scratch/roots`."""
        sim = scratch / "sim.sh"
        (scratch / "shot.png").write_bytes(self.PNG)
        sim.write_text("#!/bin/sh\n" + script.format(png=scratch / "shot.png"))
        sim.chmod(0o755)
        roots = scratch / "roots"
        roots.mkdir(exist_ok=True)
        with patch.object(shots.tempfile, "tempdir", str(roots)), \
                patch.object(shots.mock_pms, "serve", return_value=(_Server(), _Pms())), \
                patch.object(shots.mock_pms, "discover_stand_in_problem", return_value=None), \
                contextlib.redirect_stdout(io.StringIO()):
            return shots.capture(sim, scene or {"name": "probe"}, self.DEFAULTS, "sintel", False, **kwargs)

    def test_capture_removes_its_instance_root_whether_it_returns_or_raises(self):
        settles = 'cp "{png}" "$PLXNATIVE_SHOT"\n'
        for name, script, scene, kwargs, error in (
                ("settled", settles, {}, {}, None),
                ("refused", "exit 3\n", {}, {}, RuntimeError),
                ("never settles", "exec sleep 30\n", {"timeout_s": 0.2}, {}, RuntimeError),
                ("fixture fails", settles, {}, {"prepare": lambda pms, rt: 1 / 0}, ZeroDivisionError)):
            with self.subTest(name), tempfile.TemporaryDirectory() as temp:
                scratch = pathlib.Path(temp)
                if error is None:
                    self.capture(scratch, script, {"name": "probe", **scene}, **kwargs)
                else:
                    with self.assertRaises(error):
                        self.capture(scratch, script, {"name": "probe", **scene}, **kwargs)
                self.assertEqual(os.listdir(scratch / "roots"), [])

    def test_a_run_stopped_by_sigterm_leaves_no_root_and_no_simulator(self):
        with tempfile.TemporaryDirectory() as temp:
            scratch = pathlib.Path(temp)
            (scratch / "tmp").mkdir()
            sim = scratch / "sim.sh"
            sim.write_text('#!/bin/sh\necho $$ > "$FAKE_SIM_PID"\nexec sleep 60\n')
            sim.chmod(0o755)
            env = dict(os.environ, TMPDIR=str(scratch / "tmp"), FAKE_SIM_PID=str(scratch / "pid"))
            scene = json.loads(shots.SCENES.read_text())["scenes"][0]["name"]
            child = subprocess.Popen([sys.executable, "-I", "-c", self.CHILD, str(shots.ROOT), str(sim),
                                      str(scratch / "out"), scene],
                                     env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            self.addCleanup(lambda: child.poll() is None and child.kill())
            deadline = time.monotonic() + 120
            while not (scratch / "pid").exists() or not (scratch / "pid").read_text().strip():
                self.assertIsNone(child.poll(), "the driver exited before the simulator started")
                self.assertLess(time.monotonic(), deadline, "the simulator never started")
                time.sleep(0.05)
            pid = int((scratch / "pid").read_text())
            self.addCleanup(lambda: subprocess.run(["kill", "-9", str(pid)], stderr=subprocess.DEVNULL))
            child.terminate()
            child.wait(timeout=60)
            self.assertEqual(os.listdir(scratch / "tmp"), [])
            self.assertNotEqual(subprocess.run(["kill", "-0", str(pid)], stderr=subprocess.DEVNULL).returncode, 0,
                                "the simulator is still running")


if __name__ == "__main__":
    unittest.main()
