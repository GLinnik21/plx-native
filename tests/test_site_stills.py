"""tools/site_stills.py: which stills the refresh owns, and the adopt step's refusals.

Offline: no simulator, no ffmpeg. Artifacts are built in a private temp tree that stands in for the repository.
"""
import importlib.util
import json
import pathlib
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
import zlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
_spec = importlib.util.spec_from_file_location("site_stills", ROOT / "tools" / "site_stills.py")
ss = importlib.util.module_from_spec(_spec)
sys.modules["site_stills"] = ss
_spec.loader.exec_module(ss)
_spec = importlib.util.spec_from_file_location("screenshots_tool", ROOT / "tools" / "screenshots.py")
shots = importlib.util.module_from_spec(_spec)
sys.modules["screenshots_tool"] = shots
_spec.loader.exec_module(shots)


def tracked(prefix):
    out = subprocess.run(["git", "ls-files", prefix], capture_output=True, text=True, cwd=ROOT, check=True).stdout
    return set(out.split())


class FileSet(unittest.TestCase):
    def test_automated_and_manual_are_disjoint_and_cover_every_simulator_made_still(self):
        auto, manual = set(ss.output_files()), set(ss.output_files(manual=True))
        self.assertFalse(auto & manual)
        committed = tracked("docs/screenshots") | tracked("site/media")
        film = {p for p in committed if p.startswith("site/media/feel")}
        # what is committed and in neither set is the film and the photograph taken on the television, nothing else
        self.assertEqual(committed - auto - manual - film, {"docs/screenshots/navblur-transition.jpg"})
        self.assertLessEqual(auto | manual, committed)

    def test_the_film_and_the_television_photograph_are_never_in_a_set(self):
        for p in ss.output_files() + ss.output_files(manual=True):
            self.assertNotIn("feel", p.split("/")[-1])
            self.assertNotIn("navblur", p)

    def test_every_simulator_scene_is_automated_and_only_the_television_photograph_is_manual(self):
        self.assertEqual(ss.MANUAL_SCENES, {})
        self.assertEqual(ss.output_files(manual=True), [])
        self.assertEqual(set(ss.automated_scenes()), {s["name"] for s in ss.scene_table()})
        for rel in ("docs/screenshots/player.jpg", "site/media/closeup-player.jpg", "site/media/closeup-player-glow.png",
                    "site/media/og-card.jpg", "docs/screenshots/ux-home-hero.jpg"):
            self.assertIn(rel, ss.output_files())

    def test_a_manual_scene_would_take_everything_cut_from_it(self):
        """The mechanism stays for the day a scene cannot be made on the runner: its figure, WebP, phone copy, glow
        and the card composed around it all leave the automated set together."""
        saved = dict(ss.MANUAL_SCENES)
        self.addCleanup(lambda: (ss.MANUAL_SCENES.clear(), ss.MANUAL_SCENES.update(saved)))
        ss.MANUAL_SCENES.update({"site-up-next": "x", "home": "y"})
        manual = ss.output_files(manual=True)
        self.assertIn("site/media/closeup-player-glow.png", manual)
        self.assertIn("site/media/closeup-player-narrow.webp", manual)
        self.assertIn("site/media/og-card.jpg", manual)  # composed around the `home` figure
        self.assertNotIn("site/media/closeup-glass.jpg", manual)
        self.assertFalse(set(manual) & set(ss.output_files()))


IDENTICAL = {"identical": True, "pixels": 0, "worst": 0, "bound": 1, "files": []}


class Adopt(unittest.TestCase):
    def setUp(self):
        self.tmp = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.repo, self.art = self.tmp / "repo", self.tmp / "art"
        for rel in ss.output_files():
            for base, body in ((self.repo, b"old " + rel.encode()), (self.art / "files", b"new " + rel.encode())):
                (base / rel).parent.mkdir(parents=True, exist_ok=True)
                (base / rel).write_bytes(body)
        # `plan_adopt` reads the scene manifest and the variant tools from `root`
        for rel in ("tests/screenshots/scenes.json",):
            (self.repo / rel).parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / rel, self.repo / rel)
        self.write_manifest()

    def write_manifest(self, **over):
        m = {"schema": 1, "platform": "linux-ci", "tree_hash": {"combined": "x"},
             "stability": {"scenes": {n: dict(IDENTICAL) for n in ss.automated_scenes()}},
             "files": {rel: {"sha256": ss.sha256_file(self.art / "files" / rel)} for rel in ss.output_files()}}
        m.update(over)
        (self.art / ss.MANIFEST_NAME).write_text(json.dumps(m))

    def adopt(self, write=False):
        return ss.adopt(self.art, root=self.repo, write=write, say=lambda *_: None)

    def test_a_dry_run_lists_and_writes_nothing(self):
        self.assertEqual(sorted(self.adopt()), ss.output_files())
        self.assertFalse((self.repo / ss.MANIFEST_REL).exists())
        self.assertEqual((self.repo / "site/media/closeup-glass.jpg").read_bytes(), b"old site/media/closeup-glass.jpg")

    def test_write_copies_the_changed_files_and_the_manifest(self):
        self.adopt(write=True)
        self.assertEqual((self.repo / "site/media/closeup-glass.jpg").read_bytes(), b"new site/media/closeup-glass.jpg")
        self.assertTrue((self.repo / ss.MANIFEST_REL).is_file())

    def test_identical_stills_write_nothing_not_even_the_manifest(self):
        for rel in ss.output_files():
            shutil.copyfile(self.art / "files" / rel, self.repo / rel)
        self.assertEqual(self.adopt(write=True), [])
        self.assertFalse((self.repo / ss.MANIFEST_REL).exists())

    def test_refusals(self):
        scenes = {n: dict(IDENTICAL) for n in ss.automated_scenes()}
        one = next(iter(scenes))
        noisy = dict(scenes, **{one: {"identical": False, "pixels": 9, "worst": 1, "bound": 1, "files": ["a.jpg"]}})
        popped = dict(scenes, **{one: {"identical": False, "pixels": 4000, "worst": 90, "bound": 99, "files": ["a.jpg"]}})
        short = {k: v for k, v in scenes.items() if k != one}
        for over, why in (({"platform": "darwin-local"}, "platform"),
                          ({"stability": {"scenes": popped}}, "not stable"),  # the artifact's own `bound` is not believed
                          ({"stability": {"scenes": short}}, "stability verdict"),
                          ({"stability": {}}, "stability verdict"),
                          ({"schema": 2}, "schema")):
            self.write_manifest(**over)
            with self.assertRaisesRegex(ss.Failure, why):
                self.adopt()
        self.write_manifest(stability={"scenes": noisy})  # one level of rounding on nine pixels is the noise
        self.assertTrue(self.adopt())

    def test_a_tampered_file_an_extra_file_and_a_missing_file_are_refused(self):
        self.write_manifest()
        victim = self.art / "files" / "site/media/closeup-glass.jpg"
        victim.write_bytes(b"tampered")
        with self.assertRaisesRegex(ss.Failure, "sha256"):
            self.adopt()
        self.write_manifest()
        (self.art / "files" / "site" / "media" / "feel-poster.jpg").write_bytes(b"x")
        with self.assertRaisesRegex(ss.Failure, "exactly"):
            self.adopt()
        (self.art / "files" / "site" / "media" / "feel-poster.jpg").unlink()
        victim.unlink()
        with self.assertRaises(ss.Failure):
            self.adopt()

    def test_a_manifest_naming_another_file_set_is_refused(self):
        m = json.loads((self.art / ss.MANIFEST_NAME).read_text())
        m["files"]["docs/screenshots/navblur-transition.jpg"] = {"sha256": "0"}
        (self.art / ss.MANIFEST_NAME).write_text(json.dumps(m))
        with self.assertRaisesRegex(ss.Failure, "file set"):
            self.adopt()


def png(w, h, pixel):
    """An 8-bit RGB PNG (filter 0) of `pixel(x, y) -> (r, g, b)`, and nothing else a test needs."""
    rows = b"".join(b"\0" + b"".join(bytes(min(255, v) for v in pixel(x, y)) for x in range(w)) for y in range(h))

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def unpng(data, w, h):
    raw = zlib.decompress(b"".join(_idat(data)))
    return b"".join(raw[y * (w * 3 + 1) + 1:(y + 1) * (w * 3 + 1)] for y in range(h))


def _idat(data):
    i = 8
    while i < len(data):
        n, kind = struct.unpack(">I4s", data[i:i + 8])
        if kind == b"IDAT":
            yield data[i + 8:i + 8 + n]
        i += 12 + n


W, H = 192, 108


def scene_pixel(x, y):
    """A smooth backdrop, a 24x36 'poster' with texture, a text-like stripe: what a shelf is made of."""
    if 40 <= x < 64 and 30 <= y < 66:
        return ((x * 37 + y * 11) % 200 + 20, (x * 5 + y * 29) % 180 + 40, (x * 19 + y * 7) % 160 + 60)
    if 100 <= x < 180 and 50 <= y < 54:
        return (235, 235, 235)
    return (30 + x // 4, 40 + y // 3, 90 + (x + y) // 8)


class Stability(unittest.TestCase):
    """The stability gate, on synthetic captures (`screenshots.py`'s own comparison, ffmpeg's decode stood in
    for by the PNG reader above). What it must let through is the renderer's rounding; what it must stop is
    a picture that is different."""

    def setUp(self):
        self._raw = shots.raw_rgb
        shots.raw_rgb = lambda data, w, h: unpng(data, w, h)
        self.addCleanup(setattr, shots, "raw_rgb", self._raw)
        self.defaults = {"max_delta": 1}

    def report(self, other, scene=None):
        scene = scene or {"name": "x"}
        return shots.pair_report(scene, png(W, H, scene_pixel), png(W, H, other), self.defaults, (W, H))

    def test_the_default_bound_is_the_manifests_one_level_and_site_up_next_has_its_two(self):
        self.assertEqual(json.loads(ss.SCENES.read_text())["defaults"]["max_delta"], 1)
        self.assertEqual(ss.bounds_by_scene()["site-up-next"], 2)
        self.assertEqual(ss.bounds_by_scene()["home"], 1)

    def test_identical_captures_are_identical(self):
        r = self.report(scene_pixel)
        self.assertTrue(r["identical"] and r["ok"] and r["pixels"] == 0)

    def test_one_level_of_rounding_in_a_blur_is_stable(self):
        def noisy(x, y):
            p = scene_pixel(x, y)
            return tuple(c + 1 if (x * 7 + y * 13) % 5 == 0 and 60 < y < 100 else c for c in p)
        r = self.report(noisy)
        self.assertGreater(r["pixels"], 100)
        self.assertEqual(r["worst"], 1)
        self.assertTrue(r["ok"])

    def test_a_poster_that_did_not_load_is_not(self):
        def missing(x, y):
            return (40, 42, 48) if 40 <= x < 64 and 30 <= y < 66 else scene_pixel(x, y)
        r = self.report(missing)
        self.assertFalse(r["ok"])
        self.assertGreater(r["worst"], 20)
        self.assertLessEqual(r["pixels"], 24 * 36)

    def test_a_poster_that_is_dark_and_nearly_the_placeholder_is_still_caught(self):
        # a poster only 3 levels from the placeholder's grey is a different picture: the bound is one level
        def dark(x, y):
            return (40 + (x + y) % 2 * 3, 40, 40) if 40 <= x < 64 and 30 <= y < 66 else scene_pixel(x, y)
        def placeholder(x, y):
            return (40, 40, 40) if 40 <= x < 64 and 30 <= y < 66 else scene_pixel(x, y)
        got = shots.pair_report({"name": "x"}, png(W, H, dark), png(W, H, placeholder), self.defaults, (W, H))
        self.assertEqual(got["worst"], 3)
        self.assertFalse(got["ok"])

    def test_a_line_of_text_a_pixel_over_is_not_noise(self):
        def moved(x, y):
            return scene_pixel(x, y - 1) if 100 <= x < 180 and 49 <= y < 56 else scene_pixel(x, y)
        self.assertFalse(self.report(moved)["ok"])

    def test_a_scene_with_a_reasoned_wider_bound_takes_two_levels_and_not_three(self):
        def shift(by):
            return lambda x, y: tuple(c + by if x % 9 == 0 else c for c in scene_pixel(x, y))
        scene = {"name": "x", "max_delta": 2}
        self.assertTrue(self.report(shift(2), scene)["ok"])
        self.assertFalse(self.report(shift(3), scene)["ok"])

    def stable(self, pixels, retry=True):
        caps = iter(png(W, H, f) for f in pixels)
        calls = []

        def take():
            calls.append(1)
            return next(caps)
        got, r = shots.stable_capture(take, {"name": "x"}, self.defaults, (W, H), retry)
        return got, r, len(calls)

    @staticmethod
    def stalled(x, y):  # a capture caught mid-fade: 13 levels off on a fifth of the picture
        p = scene_pixel(x, y)
        return tuple(c + 13 for c in p) if x < 40 else p

    def test_two_agreeing_captures_take_two_and_adopt_the_first(self):
        got, r, n = self.stable([scene_pixel, scene_pixel])
        self.assertEqual((n, r["captures"], r["ok"]), (2, 2, True))
        self.assertEqual(got, png(W, H, scene_pixel))

    def test_one_stalled_capture_of_three_is_discarded_wherever_it_falls(self):
        for order in ((scene_pixel, self.stalled, scene_pixel), (self.stalled, scene_pixel, scene_pixel)):
            got, r, n = self.stable(list(order))
            self.assertEqual((n, r["captures"], r["ok"]), (3, 3, True))
            self.assertEqual(got, png(W, H, scene_pixel))  # never the stalled one
            self.assertGreater(r["discarded"]["worst"], 1)
            self.assertEqual(r["worst"], 0)

    def test_three_captures_that_never_agree_fail_and_without_the_retry_two_are_enough_to_fail(self):
        def other(x, y):
            return tuple(c + 40 for c in scene_pixel(x, y)) if y < 60 else scene_pixel(x, y)
        _, r, n = self.stable([scene_pixel, self.stalled, other])
        self.assertEqual((n, r["ok"], r["captures"]), (3, False, 3))
        self.assertIn("1-3", r["pairs"])
        _, r, n = self.stable([scene_pixel, self.stalled], retry=False)
        self.assertEqual((n, r["ok"]), (2, False))

    def test_the_failure_names_the_files_of_the_scene(self):
        report = {"home": {"identical": False, "pixels": 5000, "worst": 60, "files": ["home.jpg", "og-card.jpg"]},
                  "library": dict(IDENTICAL)}
        over = ss.unstable(report)
        self.assertEqual(len(over), 1)
        self.assertIn("home.jpg, og-card.jpg", over[0])
        self.assertIn("60", over[0])


class NeedsRender(unittest.TestCase):
    """An app whose inputs did not change commits no stills, whatever the renderer's noise: the decision is the
    inputs' tree hash against the committed manifest, taken in a throwaway repository laid out like this one."""

    def setUp(self):
        self.repo = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.repo, ignore_errors=True)
        for rel in ss.TREE_PATHS:
            path = self.repo / rel
            if path.suffix:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(rel)
            else:
                (path / "a").mkdir(parents=True, exist_ok=True)
                (path / "a" / "f").write_text(rel)
        (self.repo / "docs").mkdir()
        (self.repo / "docs" / "note.md").write_text("one")
        self.git("init", "-q", "-b", "main")
        self.commit("first")

    def git(self, *args):
        return subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", "-C", str(self.repo), *args],
                              capture_output=True, text=True, check=True).stdout.strip()

    def commit(self, msg):
        self.git("add", "-A")
        self.git("commit", "-q", "-m", msg)

    def decide(self, **kw):
        return ss.needs_render("HEAD", root=self.repo, manifest_path=self.repo / "m.json", **kw)[0]

    def record(self):
        (self.repo / "m.json").write_text(json.dumps({"tree_hash": ss.tree_hashes(root=self.repo)}))

    def test_a_release_with_unchanged_inputs_renders_nothing_and_so_commits_nothing(self):
        self.record()
        self.assertFalse(self.decide())
        (self.repo / "docs" / "note.md").write_text("two")  # the docs, the workflows, the film: not the stills' inputs
        self.commit("docs only")
        self.assertFalse(self.decide())

    def test_a_change_to_any_input_renders(self):
        for rel in ("rust-modules/a/f", "assets/a/f", "tests/mock_pms.py", "site/og/a/f", "pkg/appfont.ttf",
                    "tools/render-site-glows.py", "tools/site_stills.py"):
            self.record()
            self.assertFalse(self.decide(), rel)
            (self.repo / rel).write_text("changed " + rel)
            self.commit(rel)
            self.assertTrue(self.decide(), rel)

    def test_force_renders_regardless(self):
        self.record()
        self.assertTrue(self.decide(force=True))


class Decide(unittest.TestCase):
    def test_forced_and_unreadable_manifests_render_and_a_matching_tree_does_not(self):
        self.assertTrue(ss.needs_render(force=True)[0])
        self.assertTrue(ss.needs_render(manifest_path="/nonexistent/m.json")[0])
        with tempfile.TemporaryDirectory() as d:
            m = pathlib.Path(d, "m.json")
            m.write_text(json.dumps({"tree_hash": ss.tree_hashes()}))
            self.assertFalse(ss.needs_render(manifest_path=m)[0])

    def test_commit_message_for_each_combination_and_for_none(self):
        stills = {"tree_hash": {"combined": "a" * 64}, "files": {"a": {}, "b": {}}}
        msg = ss.commit_message("v1.2.3", "https://run", "f" * 40, stills=stills)
        self.assertTrue(msg.startswith("Site: stills re-rendered for v1.2.3\n"))
        self.assertIn("revert this commit", msg)
        self.assertTrue(ss.commit_message("", "u", "f" * 40, stills=stills).startswith("Site: stills re-rendered for main@ffffffff"))
        with self.assertRaises(ss.Failure):
            ss.commit_message("v1.2.3", "u", "s")


if __name__ == "__main__":
    unittest.main()
