"""Coverage of the HUD matrix generated from the compiler's reachable states."""
import importlib.util
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))
import hud_preview as preview


def inspection():
    states = []
    for opened in (0, 1, 2):  # modal: chapters+info is deliberately unreachable
        for paused in (False, True):
            for episode in (False, True):
                states.append({"slot": "discs", "open": opened, "paused": paused, "navigation_pair": episode,
                               "episode": episode, "chapters": True, "scrub": [90, 700, 1700, 60],
                               "controls": [{"id": "subs", "menu": "tracks", "rect": [100, 800-opened*100, 52, 52]}]})
    return {"languages": [{"lang": lang, "states": states} for lang in preview.LONG_TITLES]}


class Matrix(unittest.TestCase):
    def test_every_reachable_section_transport_pair_and_opener_position_is_covered(self):
        scenes = preview.plan(inspection())
        sections = [scene for scene in scenes if scene["name"].startswith("sections-")]
        self.assertEqual(len(sections), 12)
        self.assertEqual({s["open"] for s in sections}, {0, 1, 2})
        menus = [scene for scene in scenes if scene.get("opener")]
        self.assertEqual(len(menus), 6)
        self.assertEqual({tuple(s["opener"]["point"]) for s in menus}, {(126,826),(126,726),(126,626)})
        self.assertEqual({s["lang"] for s in scenes if s.get("long_title")}, set(preview.LONG_TITLES))
        self.assertEqual({s["open"] for s in scenes if s.get("captions")}, {0,1,2})
        seek = next(s for s in scenes if s["name"] == "seeking")
        self.assertEqual(seek["scrub_point"], [940,730])
        cfg = preview.capture_scene(seek)["virtual"]
        self.assertEqual(cfg["clicks"], "900:940.0:730.0")
        self.assertLess(cfg["select"], 6)
        self.assertGreater(cfg["frames"], 6)

    def test_a_new_shipped_language_requires_a_long_title_sample(self):
        value = inspection()
        value["languages"].append({"lang":"xx", "states":[]})
        with self.assertRaisesRegex(ValueError, "language"):
            preview.plan(value)


if __name__ == "__main__":
    unittest.main()
