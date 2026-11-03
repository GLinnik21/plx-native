"""Coverage of the HUD matrix generated from the compiler's reachable states."""
import importlib.util
import hashlib
import json
import os
import subprocess
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))
import hud_preview as preview


def inspection():
    states = []
    for opened in (0, 1, 2):  # modal: chapters+info is deliberately unreachable
        for paused in (False, True):
            for episode,pair in ((False,False),(True,True),(True,False)):
                for chapters in (False,True):
                    if not chapters and opened & 1:
                        continue
                    states.append({"slot": "discs", "open": opened, "paused": paused, "navigation_pair": pair,
                                   "episode": episode, "chapters": chapters, "scrub": [90, 700, 1700, 60],
                                   "controls": [{"id": "subs", "label":"Subtitles", "menu": "tracks", "rect": [100, 800-opened*100, 52, 52]}]})
    return {"languages": [{"lang": lang, "states": states} for lang in preview.LONG_TITLES]}


class Matrix(unittest.TestCase):
    def test_every_reachable_section_transport_pair_and_opener_position_is_covered(self):
        scenes = preview.plan(inspection())
        sections = [scene for scene in scenes if scene["name"].startswith("sections-")]
        self.assertEqual(len(sections), 30)
        self.assertTrue(any(s["real_episode"] and not s["navigation_pair"] for s in sections))
        self.assertTrue(any(not s["chapters"] for s in sections))
        self.assertFalse(any(not s["chapters"] and s["open"] & 1 for s in sections))
        self.assertEqual({s["open"] for s in sections}, {0, 1, 2})
        menus = [scene for scene in scenes if scene.get("opener")]
        self.assertEqual(len(menus), 3)
        self.assertEqual({tuple(s["opener"]["point"]) for s in menus}, {(126,826),(126,726),(126,626)})
        self.assertEqual({s["lang"] for s in scenes if s.get("long_title")}, set(preview.LONG_TITLES))
        self.assertEqual({s["open"] for s in scenes if s.get("captions")}, {0,1,2})
        seek = next(s for s in scenes if s["name"] == "seeking")
        self.assertEqual(seek["scrub_point"], [940,730])
        cfg = preview.capture_scene(seek)["virtual"]
        self.assertEqual(cfg["clicks"], "600:940.0:730.0")
        self.assertLess(cfg["select"], 6)
        self.assertGreater(cfg["frames"], 6)

    def test_isolated_episode_parents_have_real_mock_art_and_a_queue_of_one(self):
        movie = {"type":"movie","ratingKey":"102","Media":[{"Part":[{"id":1020}]}]}
        lib = SimpleNamespace(items={102:movie,201:{"type":"show"},2011:{"type":"season"}},
                              images={201:{"logo":"real-show-logo"},2011:{"thumb":"real-season-thumb"}})
        spec = {"navigation_pair":False,"real_episode":True,"background":"dark","chapters":True}
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary); hud=root/"example.json"; hud.write_text("{}")
            preview.fixture(spec,hud)(SimpleNamespace(lib=lib),root)
        self.assertEqual(lib.images[99100],lib.images[201])
        self.assertEqual(lib.images[99101],lib.images[2011])
        self.assertEqual(movie["grandparentRatingKey"],lib.items[99100]["ratingKey"])
        queue=preview.screenshots.mock_pms.Library.continuous_queue(lib,movie)
        self.assertEqual(queue,[movie])

    def test_long_chapters_replace_the_pms_tag_read_by_the_player(self):
        movie={"Media":[{"Part":[{"id":1020}]}],
               "Chapter":[{"startTimeOffset":0,"tag":"Short chapter"}]}
        spec={"navigation_pair":False,"real_episode":False,"background":"dark","chapters":True,
              "long_title":True,"lang":"de"}
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);hud=root/"example.json";hud.write_text("{}")
            preview.fixture(spec,hud)(SimpleNamespace(lib=SimpleNamespace(items={102:movie})),root)
        self.assertEqual(movie["Chapter"][0]["tag"],preview.LONG_TITLES["de"])
        self.assertNotIn("title",movie["Chapter"][0])

    def test_directory_is_packed_once_and_capture_is_bound_to_the_frozen_archive(self):
        archive = b"canonical archive fixture"
        digest = hashlib.sha256(archive).hexdigest()
        native_info = {"protocol": 1, "supported_formats": [{"package": "1.0", "hud": "1.0"}],
                       "limits": {"package": {"1.0": {"artifact_bytes": 2097152}}}}
        compiled = {**inspection(), "protocol": 1, "package_sha256": digest,
                    "manifest": {"package_format": "1.0", "hud": {"format": "1.0", "entry": "hud.json"}}}
        packed = []
        def native(command, **kwargs):
            if command[1] == "--hud-info":
                value = native_info
            elif command[1] == "--hud-pack":
                packed.append(command)
                Path(command[command.index("--out")+1]).write_bytes(archive)
                value = {"protocol": 1, "package_sha256": digest, "bytes": len(archive),
                         "manifest": compiled["manifest"], "findings": []}
            else:
                self.assertEqual(command[1], "--hud-inspect")
                self.assertEqual(Path(command[2]).read_bytes(), archive)
                value = compiled
            return subprocess.CompletedProcess(command, 0, json.dumps(value), "")
        def capture(binary, scene, defaults, movie, refresh, **kwargs):
            # Editing source after snapshot creation cannot change either captured package or SHA.
            (source / "hud.json").write_text('{"changed_after_pack":true}')
            (kwargs["prepare"])(SimpleNamespace(lib=SimpleNamespace(items={102:{"Media":[{"Part":[{"id":1020}]}]}})), runtime)
            self.assertEqual((runtime / "preview.plxhud").read_bytes(), archive)
            return b"png fixture", "synthetic fixture log"
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);source=root/"source";source.mkdir();(source/"hud.json").write_text("{}")
            runtime=root/"runtime";runtime.mkdir()
            # The subprocess interface is intentionally real-shaped; no second Python packer exists.
            with patch.object(preview.subprocess, "run", side_effect=native), patch.object(preview.screenshots, "capture", side_effect=capture):
                result=preview.run(Path("/fixture/bin"),source,root/"previews",["frame-bright", "frame-dark"])
        self.assertEqual(len(packed),1)
        self.assertEqual(result["package_sha256"],digest)

    def test_cli_rejects_a_symlink_input_before_packing_or_inspecting(self):
        info = {"protocol": 1, "supported_formats": [{"package": "1.0", "hud": "1.0"}],
                "limits": {"package": {"1.0": {"artifact_bytes": 2097152}}}}
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            linked = root / "linked"
            linked.symlink_to(source, target_is_directory=True)
            for mode in ([], ["--plan"]):
                with self.subTest(mode=mode), patch.object(sys, "argv", ["hud_preview.py", "--bin", "/native", "--hud", str(linked), "--out", str(root / "out"), *mode]):
                    with patch("hud_package.native_json", return_value=info) as native:
                        with self.assertRaisesRegex(ValueError, "symlink"):
                            preview.main()
                        self.assertEqual(native.call_count, 1)
                        self.assertEqual(native.call_args.args[1], "--hud-info")

    @unittest.skipUnless(hasattr(os, "mkfifo"), "requires a POSIX FIFO")
    def test_special_archive_input_is_rejected_without_opening_it(self):
        info = {"protocol": 1, "supported_formats": [{"package": "1.0", "hud": "1.0"}],
                "limits": {"package": {"1.0": {"artifact_bytes": 2097152}}}}
        with tempfile.TemporaryDirectory() as temporary:
            fifo = Path(temporary) / "blocked.plxhud"
            os.mkfifo(fifo)
            with patch("hud_package.native_json", return_value=info), patch.object(Path, "open", side_effect=AssertionError("special input was opened")):
                with self.assertRaisesRegex(ValueError, "regular file"):
                    with preview.snapshot(Path("/native"), fifo):
                        self.fail("special input admitted")

    def test_a_new_shipped_language_requires_a_long_title_sample(self):
        value = inspection()
        value["languages"].append({"lang":"xx", "states":[]})
        with self.assertRaisesRegex(ValueError, "language"):
            preview.plan(value)


if __name__ == "__main__":
    unittest.main()
