"""The demo library and the screenshot manifest: the contracts `make screenshots` relies on.

The first half needs nothing but this checkout — the manifests, the QR encoder, the plex.tv
stand-in and the scene manifest. The second half serves the catalog itself, which needs the
derived artwork cache (`make demo-library`, ~390 MB of downloads); it is skipped, loudly, where
the cache is absent, so a fresh clone's `make check` stays offline.
"""
import contextlib
import importlib.util
import io
import json
import pathlib
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
import unittest.mock
import urllib.parse
import zlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tests"))

import mock_pms  # noqa: E402
from demo_library import qr  # noqa: E402  (tests/demo_library/qr.py)

# tools/demo_library.py shares its name with the tests/demo_library package, so it is loaded by
# path under another name rather than through sys.path.
_spec = importlib.util.spec_from_file_location("demo_library_tool", ROOT / "tools" / "demo_library.py")
tool = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(tool)
_spec = importlib.util.spec_from_file_location("screenshots_tool", ROOT / "tools" / "screenshots.py")
screenshots = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(screenshots)

SCENES = ROOT / "tests" / "screenshots" / "scenes.json"
CATALOG = ROOT / "tests" / "demo_library" / "catalog.json"
RUST = ROOT / "rust-modules" / "src"
# Armed by tools/screenshots.py on every scene, not by the manifest.
DRIVER_TRIGGERS = {"token", "plextv", "stillclock"}

HAVE_CACHE = (mock_pms.demo_cache_dir() / "derived").is_dir() and shutil.which("ffmpeg") and shutil.which("ffprobe")


def decode_png(data):
    """(width, height, colour type, raw scanlines) of an unfiltered 8-bit PNG."""
    assert data[:8] == b"\x89PNG\r\n\x1a\n"
    pos, idat, ihdr = 8, b"", None
    while pos < len(data):
        n, tag = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + n]
        if tag == b"IHDR":
            ihdr = struct.unpack(">IIBBBBB", body)
        elif tag == b"IDAT":
            idat += body
        pos += 12 + n
    w, h, depth, colour = ihdr[:4]
    assert depth == 8
    return w, h, colour, zlib.decompress(idat)


class Manifests(unittest.TestCase):
    def test_the_manifests_validate(self):
        assets, catalog = tool.load()
        self.assertTrue(tool.check(assets, catalog))

    def test_every_asset_is_pinned_and_openly_licensed(self):
        assets, _ = tool.load()
        for aid, a in assets.items():
            with self.subTest(asset=aid):
                self.assertRegex(a["sha256"], r"^[0-9a-f]{64}$")
                self.assertGreater(a["bytes"], 0)
                self.assertTrue(a["url"].startswith("https://"), a["url"])
                self.assertTrue(a["licence"].startswith(("CC BY", "CC0", "Public domain")), a["licence"])

    def test_the_hero_and_its_alternatives_are_distinct_films(self):
        _, catalog = tool.load()
        films = {m["id"] for m in catalog["movies"]}
        pool = [catalog["hero"], *catalog["hero_alternatives"]]
        self.assertEqual(len(pool), len(set(pool)))
        self.assertLessEqual(set(pool), films)

    def test_check_refuses_an_unknown_reference(self):
        assets, catalog = tool.load()
        broken = dict(catalog, watched=[*catalog["watched"], "no-such-film"])
        with self.assertRaises(AssertionError):
            tool.check(assets, broken)

    def test_every_hero_candidate_has_a_logo_cut_from_its_own_poster(self):
        # The home hero draws a film's clearLogo; a candidate without one falls back to text.
        _, catalog = tool.load()
        films = {m["id"]: m for m in catalog["movies"]}
        for film in (catalog["hero"], *catalog["hero_alternatives"]):
            with self.subTest(hero=film):
                self.assertIn("logo", films[film])
                self.assertEqual(films[film]["logo"]["asset"], films[film]["poster"]["asset"])

    def test_check_refuses_a_logo_cut_from_another_films_poster(self):
        assets, catalog = tool.load()
        movies = [dict(m) for m in catalog["movies"]]
        donor = next(m for m in movies if "logo" in m)
        other = next(m for m in movies if m["poster"]["asset"] != donor["poster"]["asset"])
        other["logo"] = donor["logo"]
        with self.assertRaises(AssertionError):
            tool.check(assets, dict(catalog, movies=movies))

    def test_check_refuses_a_stand_in_anywhere_but_an_episode(self):
        assets, catalog = tool.load()
        movies = [dict(m) for m in catalog["movies"]]
        movies[0]["stand_in"] = True
        with self.assertRaises(AssertionError):
            tool.check(assets, dict(catalog, movies=movies))

    def test_check_refuses_a_share_alike_licence(self):
        assets, catalog = tool.load()
        aid = next(iter(assets))
        broken = dict(assets, **{aid: dict(assets[aid], licence="CC BY-SA 4.0")})
        with self.assertRaises(AssertionError):
            tool.check(broken, catalog)


class Credits(unittest.TestCase):
    """CREDITS.md is written by the same command that writes the images, so it cannot lag them."""

    def test_the_committed_credits_are_what_the_manifests_say(self):
        assets, catalog = tool.load()
        with tempfile.TemporaryDirectory() as d:
            dst = pathlib.Path(d) / "CREDITS.md"
            tool.credits(assets, catalog, dst)
            self.assertEqual(dst.read_text(), tool.CREDITS.read_text(),
                             "docs/screenshots/CREDITS.md is stale: run `make screenshots`")

    def test_a_screenshot_run_writes_the_credits_beside_its_images(self):
        with tempfile.TemporaryDirectory() as d:
            screenshots.write_credits(pathlib.Path(d))
            self.assertEqual((pathlib.Path(d) / "CREDITS.md").read_text(), tool.CREDITS.read_text())


class RenderSet(unittest.TestCase):
    """A run replaces the figure set whole, or leaves it as it was."""

    JOBS = [({"name": "a"}, None, [{"file": "a.jpg"}]),
            ({"name": "b"}, None, [{"file": "b.jpg", "dest": "site"}])]

    @staticmethod
    def write(scene, hero, outputs, stage):
        o = outputs[0]
        (stage / o.get("dest", "docs") / o["file"]).write_bytes(b"new")

    def test_a_failure_of_any_kind_leaves_every_destination_as_it_was(self):
        def render(scene, hero, outputs, stage):
            self.write(scene, hero, outputs, stage)
            if scene["name"] == "b":
                raise subprocess.CalledProcessError(1, ["ffmpeg"])
        with tempfile.TemporaryDirectory() as d:
            docs, site = pathlib.Path(d) / "docs", pathlib.Path(d) / "site"
            docs.mkdir()
            (docs / "a.jpg").write_bytes(b"old")
            with contextlib.redirect_stderr(io.StringIO()):
                failed = screenshots.render_set(self.JOBS, render, {"docs": docs, "site": site})
            self.assertEqual(failed, ["b"])
            self.assertEqual(sorted(p.name for p in docs.iterdir()), ["a.jpg"])
            self.assertEqual((docs / "a.jpg").read_bytes(), b"old")
            self.assertFalse(site.exists())

    def test_a_clean_run_moves_each_output_to_its_destination_and_the_credits_beside_the_docs(self):
        with tempfile.TemporaryDirectory() as d:
            docs, site = pathlib.Path(d) / "docs", pathlib.Path(d) / "site"
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(screenshots.render_set(self.JOBS, self.write, {"docs": docs, "site": site}), [])
            self.assertEqual(sorted(p.name for p in docs.iterdir()), ["CREDITS.md", "a.jpg"])
            self.assertEqual(sorted(p.name for p in site.iterdir()), ["b.jpg"])


class OutputSpec(unittest.TestCase):
    """`render_scale` and `crop`: a crop is in canvas coordinates, whatever the scale."""

    CANVAS = (1920, 1080)

    def spec(self, out, **scene):
        return screenshots.output_spec(dict({"name": "t"}, **scene), out, self.CANVAS)

    def test_an_uncropped_output_is_the_whole_canvas_at_the_scale(self):
        self.assertEqual(self.spec({"file": "a.jpg"}), ("docs", "a.jpg", (0, 0, 1920, 1080), (1920, 1080), 2))
        self.assertEqual(self.spec({"file": "a.jpg"}, render_scale=3)[2:4], ((0, 0, 5760, 3240), (5760, 3240)))

    def test_a_crop_scales_with_the_render_and_keeps_its_size_unless_one_is_named(self):
        dest, _, crop, size, q = self.spec({"file": "g.jpg", "dest": "site", "crop": [680, 0, 880, 160],
                                            "quality": 4}, render_scale=3)
        self.assertEqual((dest, crop, size, q), ("site", (2040, 0, 2640, 480), (2640, 480), 4))
        self.assertEqual(self.spec({"file": "g.jpg", "crop": [680, 0, 880, 160], "size": "1320x240"},
                                   render_scale=3)[3], (1320, 240))

    def test_a_fractional_crop_rounds_its_edges_not_its_size(self):
        # 846.667 canvas px at 3x is 2540 px: the edges round, so the size is what they enclose.
        _, _, crop, size, _ = self.spec({"file": "t.jpg", "crop": [100.333, 20, 846.667, 513.333]},
                                        render_scale=3)
        self.assertEqual(crop, (301, 60, 2540, 1540))
        self.assertEqual(size, (2540, 1540))

    def test_what_the_driver_cannot_honour_is_refused(self):
        bad = [
            ({"file": "a.jpg"}, {"render_scale": 5}),
            ({"file": "a.jpg"}, {"render_scale": 2.5}),
            ({"file": "a.jpg"}, {"render_scale": True}),
            ({"file": "a.jpg", "crop": [1800, 0, 200, 100]}, {}),
            ({"file": "a.jpg", "crop": [0, 0, 0, 100]}, {}),
            ({"file": "a.jpg", "crop": [-1, 0, 10, 10]}, {}),
            ({"file": "a.jpg", "crop": [0, 0, "10", 10]}, {}),
            ({"file": "a.jpg", "dest": "elsewhere"}, {}),
            ({"file": "../a.jpg"}, {}),
            ({"file": "a.png"}, {}),
            ({"file": "a.jpg", "quality": 0}, {}),
            ({"file": "a.jpg", "crop": [0, 0, 880, 160], "size": "880x200"}, {}),
        ]
        for out, scene in bad:
            with self.subTest(out=out, scene=scene), self.assertRaises(ValueError):
                self.spec(out, **scene)

    def test_every_manifest_output_resolves(self):
        manifest = json.loads(SCENES.read_text())
        canvas = screenshots.size_of(manifest["canvas"])
        for s in manifest["scenes"]:
            for o in s["outputs"]:
                with self.subTest(scene=s["name"], file=o["file"]):
                    screenshots.resolve_output(s, o, canvas)


class CardSpec(unittest.TestCase):
    """A `card` output is composed around another output of its scene, never cut from the capture."""

    SCENE = {"name": "home", "outputs": [{"file": "home.jpg", "size": "1600x900"},
                                         {"file": "og-card.jpg", "dest": "site", "card": "home.jpg"}]}

    def test_a_card_resolves_to_its_source_output(self):
        dest, file, source = screenshots.card_spec(self.SCENE, self.SCENE["outputs"][1])
        self.assertEqual((dest, file, source["file"]), ("site", "og-card.jpg", "home.jpg"))

    def test_a_card_without_a_source_or_with_pixel_keys_is_refused(self):
        for out in ({"file": "og.jpg", "card": "missing.jpg"},
                    {"file": "og.jpg", "card": "og.jpg"},
                    {"file": "og.jpg", "card": "home.jpg", "crop": [0, 0, 10, 10]},
                    {"file": "og.png", "card": "home.jpg"}):
            scene = dict(self.SCENE, outputs=[*self.SCENE["outputs"], out])
            with self.subTest(out=out), self.assertRaises(ValueError):
                screenshots.card_spec(scene, out)

    def test_the_manifest_renders_the_link_preview_from_the_home_figure(self):
        manifest = json.loads(SCENES.read_text())
        cards = [(s["name"], o) for s in manifest["scenes"] for o in s["outputs"] if "card" in o]
        self.assertEqual(cards, [("home", {"file": "og-card.jpg", "dest": "site", "card": "home.jpg"})])


class Fetch(unittest.TestCase):
    def test_a_failed_download_leaves_no_partial_file(self):
        aid, asset = next(iter(tool.load()[0].items()))
        with tempfile.TemporaryDirectory() as d, \
                unittest.mock.patch.object(tool, "cache_dir", return_value=pathlib.Path(d)), \
                unittest.mock.patch.object(tool.urllib.request, "urlopen", side_effect=OSError("offline")), \
                contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaises(OSError):
                tool.fetch({aid: asset})
            self.assertEqual(list((pathlib.Path(d) / "src").iterdir()), [])


class Chapters(unittest.TestCase):
    chapters = staticmethod(mock_pms.CatalogLibrary._chapters)

    def test_chapters_tile_the_film(self):
        rows = self.chapters([{"start": "0:00", "title": "A"}, {"start": "1:41", "title": "B"}], 300_000, "x")
        self.assertEqual([(r["index"], r["tag"], r["startTimeOffset"], r["endTimeOffset"]) for r in rows],
                         [(1, "A", 0, 101_000), (2, "B", 101_000, 300_000)])

    def test_malformed_chapters_are_refused(self):
        for marks in ([{"start": "0:05", "title": "late"}],
                      [{"start": "0:00", "title": "a"}, {"start": "0:00", "title": "b"}],
                      [{"start": "0:00", "title": "a"}, {"start": "9:00", "title": "past the end"}]):
            with self.subTest(marks=marks), self.assertRaises(ValueError):
                self.chapters(marks, 300_000, "x")

    def test_the_catalog_chapters_parse(self):
        _, catalog = tool.load()
        for m in catalog["movies"]:
            if m.get("chapters"):
                self.assertEqual(len(self.chapters(m["chapters"], m["minutes"] * 60_000, m["id"])),
                                 len(m["chapters"]))


class Qr(unittest.TestCase):
    def setUp(self):
        self.m = qr.encode(mock_pms.DEMO_QR_TEXT, "M")

    def test_the_symbol_has_the_right_version_and_finder_patterns(self):
        # 20 bytes in byte mode at ECC M needs version 2: 25x25 modules.
        self.assertEqual(len(self.m), 25)
        self.assertTrue(all(len(row) == 25 for row in self.m))
        finder = [[max(abs(x - 3), abs(y - 3)) in (0, 1, 3) for x in range(7)] for y in range(7)]
        for ox, oy in ((0, 0), (18, 0), (0, 18)):
            with self.subTest(corner=(ox, oy)):
                self.assertEqual([row[ox:ox + 7] for row in self.m[oy:oy + 7]], finder)

    def test_timing_patterns_and_the_dark_module(self):
        self.assertEqual([self.m[6][x] for x in range(8, 17)], [x % 2 == 0 for x in range(8, 17)])
        self.assertEqual([self.m[y][6] for y in range(8, 17)], [y % 2 == 0 for y in range(8, 17)])
        self.assertTrue(self.m[25 - 8][8])

    def test_the_format_information_says_ecc_m_with_a_valid_bch_code(self):
        bits = [self.m[8][x] for x in (0, 1, 2, 3, 4, 5, 7)] + [self.m[8][8], self.m[7][8]] \
            + [self.m[y][8] for y in (5, 4, 3, 2, 1, 0)]
        word = sum(b << (14 - i) for i, b in enumerate(bits)) ^ 0x5412
        rem = word
        for i in range(14, 9, -1):
            if rem >> i & 1:
                rem ^= 0x537 << (i - 10)
        self.assertEqual(rem, 0, "BCH(15,5) remainder")
        self.assertEqual(word >> 13, 0b00, "ECC level M")

    def test_the_plex_style_png_is_white_modules_on_a_transparent_ground(self):
        w, h, colour, raw = decode_png(qr.png(self.m, scale=2, border=0, plex_style=True))
        self.assertEqual((w, h, colour), (50, 50, 4))
        stride = 1 + 2 * w
        px = lambda x, y: raw[y * stride + 1 + 2 * x: y * stride + 3 + 2 * x]
        self.assertEqual(px(0, 0), b"\xff\xff")      # finder corner: dark module, opaque white
        self.assertEqual(px(2, 2), b"\xff\x00")      # finder's light ring: transparent
        self.assertEqual({raw[y * stride] for y in range(h)}, {0})

    def test_the_default_png_is_black_on_white_grayscale(self):
        w, h, colour, raw = decode_png(qr.png(self.m, scale=1, border=4))
        self.assertEqual((w, h, colour), (33, 33, 0))
        self.assertEqual(raw[1], 255)                # quiet zone
        self.assertEqual(raw[4 * 34 + 1 + 4], 0)     # first finder module


class PlexTvStandIn(unittest.TestCase):
    """`plxnative-plextv` points the sign-in at the mock: the pin never links."""

    def setUp(self):
        self.pms = mock_pms.MockPms(mock_pms.Library())

    def test_a_pin_is_minted_with_the_demo_code_and_stays_pending(self):
        status, _, body = self.pms.handle("POST", "/api/v2/pins?strong=false")
        self.assertEqual(status, 201)
        pin = json.loads(body)
        self.assertEqual((pin["id"], pin["code"], pin["authToken"]), (mock_pms.DEMO_PIN_ID, "DEMO", None))
        status, _, body = self.pms.handle("GET", f"/api/v2/pins/{mock_pms.DEMO_PIN_ID}")
        self.assertEqual((status, json.loads(body)["authToken"]), (200, None))

    def test_the_qr_is_a_plex_style_png(self):
        status, ctype, body = self.pms.handle("GET", "/api/v2/pins/qr/DEMO")
        self.assertEqual((status, ctype), (200, "image/png"))
        self.assertEqual(decode_png(body)[2], 4)
        self.assertEqual(body, self.pms.handle("GET", "/api/v2/pins/qr/DEMO")[2])


class SceneManifest(unittest.TestCase):
    def setUp(self):
        self.manifest = json.loads(SCENES.read_text())
        self.scenes = self.manifest["scenes"]

    def test_names_and_outputs_are_unique(self):
        names = [s["name"] for s in self.scenes]
        files = [o["file"] for s in self.scenes for o in s["outputs"]]
        self.assertEqual(len(names), len(set(names)))
        self.assertEqual(len(files), len(set(files)))

    def test_every_documented_figure_has_a_scene(self):
        files = {o["file"] for s in self.scenes for o in s["outputs"]}
        readme = {"home.jpg", "library.jpg", "search.jpg", "player.jpg"}
        ux = {f"ux-{n}.jpg" for n in ("home-hero", "home-shelves", "signin", "library-grid", "library-sort",
                                      "search", "item-menu", "account-menu", "detail", "failure")}
        self.assertLessEqual(readme | ux, files)

    def test_sizes_and_the_canvas_parse(self):
        self.assertRegex(self.manifest["canvas"], r"^\d+x\d+$")
        for s in self.scenes:
            for o in s["outputs"]:
                if "size" in o:
                    self.assertRegex(o["size"], r"^\d+x\d+$", s["name"])
                self.assertTrue(o["file"].endswith(".jpg"), o["file"])

    def test_the_site_close_ups_are_all_rendered_and_supersampled(self):
        site = {o["file"]: s for s in self.scenes for o in s["outputs"]
                if o.get("dest") == "site" and "card" not in o}
        self.assertEqual(set(site), {"closeup-glass.jpg", "closeup-glass-narrow.jpg",
                                     "closeup-tiles.jpg", "closeup-player.jpg"})
        for file, s in site.items():
            with self.subTest(file=file):
                self.assertGreaterEqual(s.get("render_scale", 1), 3)

    def test_every_scene_names_its_state_and_a_log_line_proving_it(self):
        for s in self.scenes:
            with self.subTest(scene=s["name"]):
                self.assertTrue(s.get("state"))
                self.assertTrue(s.get("expect"))

    def test_a_loosened_bound_is_explained(self):
        for s in self.scenes:
            if s.get("free_regions") or "max_delta" in s:
                self.assertTrue(s.get("tolerance_reason"), s["name"])
            for x, y, w, h in s.get("free_regions", []):
                self.assertTrue(w > 0 and h > 0 and x >= 0 and y >= 0, s["name"])

    def test_every_trigger_is_one_the_app_reads(self):
        read = set()
        for path in RUST.rglob("*.rs"):
            read |= set(re.findall(r'dev::(?:read|flag)\("([a-z0-9_]+)"\)', path.read_text()))
            read |= set(re.findall(r'\b(?:read|flag)\("([a-z0-9_]+)"\)', path.read_text()))
        for s in self.scenes:
            for name in s.get("triggers", {}):
                with self.subTest(scene=s["name"], trigger=name):
                    self.assertIn(name, read)
                    self.assertNotIn(name, DRIVER_TRIGGERS, "the driver owns this trigger")

    def test_exactly_one_scene_renders_the_hero_variants(self):
        self.assertEqual(sum(1 for s in self.scenes if s.get("hero_variants")), 1)


@unittest.skipUnless(HAVE_CACHE, "no derived demo cache (make demo-library) or no ffmpeg/ffprobe")
class Catalog(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.lib = mock_pms.CatalogLibrary(CATALOG)
        cls.pms = mock_pms.MockPms(cls.lib)
        cls.catalog = json.loads(CATALOG.read_text())

    def get(self, path, pms=None):
        status, ctype, data = (pms or self.pms).handle("GET", path)
        self.assertEqual((status, ctype), (200, "application/json"), path)
        return json.loads(data)["MediaContainer"]

    def test_keys_follow_catalog_order(self):
        for n, m in enumerate(self.catalog["movies"]):
            self.assertEqual(self.lib.items[101 + n]["title"], m["title"])
        for n, s in enumerate(self.catalog["shows"]):
            self.assertEqual(self.lib.items[201 + n]["title"], s["title"])

    def test_the_hero_heads_continue_watching_with_progress(self):
        cw = self.get("/hubs/continueWatching?count=12")["Hub"][0]
        self.assertEqual(cw["title"], "Continue Watching")
        head = cw["Metadata"][0]
        self.assertEqual(head["ratingKey"], str(self.lib.by_slug[self.catalog["hero"]]))
        self.assertGreater(head["viewOffset"], 0)
        self.assertLess(head["viewOffset"], head["duration"])

    def test_hero_swaps_the_head_and_keeps_the_rest_in_order(self):
        for film in self.catalog["hero_alternatives"]:
            with self.subTest(hero=film):
                lib = mock_pms.CatalogLibrary(CATALOG, hero=film)
                rows = lib.continue_watching()
                self.assertEqual(rows[0]["ratingKey"], str(lib.by_slug[film]))
                others = [r["ratingKey"] for r in rows[1:]]
                base = [r["ratingKey"] for r in self.lib.continue_watching() if r["ratingKey"] != str(lib.by_slug[film])]
                self.assertEqual(others, base)

    def test_the_player_film_is_the_complete_sintel_with_its_chapters(self):
        rk = self.lib.by_slug["sintel"]
        it = self.get(f"/library/metadata/{rk}?includeChapters=1")["Metadata"][0]
        self.assertEqual(it["duration"], 888_064)
        self.assertEqual(it["Chapter"][-1]["endTimeOffset"], it["duration"])
        self.assertEqual([c["tag"] for c in it["Chapter"]][:2], ["Snowbound", "The Shaman's Hut"])
        self.assertLess(it["viewOffset"], 446_000, "the resume point sits before the pinned pause")

    def test_a_continuous_queue_of_an_episode_carries_the_rest_of_its_show(self):
        rk = self.lib.by_slug["caminandes/1/2"]
        path = f"/playQueues?type=video&uri=server%3A%2F%2Fx%2Flibrary%2Fmetadata%2F{rk}&continuous=1"
        q = self.get(path)
        self.assertEqual([m["ratingKey"] for m in q["Metadata"]],
                         [str(rk), str(self.lib.by_slug["caminandes/1/3"])])
        self.assertEqual([m["playQueueItemID"] for m in q["Metadata"]], [1, 2])
        self.assertEqual(q["playQueueSelectedItemID"], 1)
        # without continuous, and for a movie, the queue is the item alone
        self.assertEqual(len(self.get(path.replace("&continuous=1", ""))["Metadata"]), 1)
        film = self.lib.by_slug["sintel"]
        self.assertEqual(len(self.get(f"/playQueues?uri=library%2Fmetadata%2F{film}&continuous=1")["Metadata"]), 1)

    def test_a_stand_in_episode_plays_a_real_file_of_its_catalog_length(self):
        rk = self.lib.by_slug["caminandes/1/2"]
        it = self.get(f"/library/metadata/{rk}")["Metadata"][0]
        part = it["Media"][0]["Part"][0]
        self.assertEqual(it["duration"], 180_000)
        self.assertIn(part["id"], self.lib.media_files)
        self.assertEqual(self.lib.media_files[part["id"]].name, "stand-in.mp4")

    def test_an_unknown_hero_is_refused(self):
        with self.assertRaises(ValueError):
            mock_pms.CatalogLibrary(CATALOG, hero="no-such-film")

    def test_home_hubs_are_titled_like_a_real_server(self):
        hubs = self.get("/hubs?count=12")["Hub"]
        self.assertEqual([h["title"] for h in hubs],
                         ["Continue Watching", *(h["title"] for h in self.catalog["hubs"])])
        self.assertTrue(all(h["Metadata"] for h in hubs))
        for key, recent in (("1", "Recently Added Movies"), ("2", "Recently Added TV")):
            titles = [h["title"] for h in self.get(f"/hubs/sections/{key}")["Hub"]]
            self.assertEqual(titles, ["Continue Watching", recent])

    def test_artwork_is_served_and_a_missing_image_is_a_404(self):
        rk = self.lib.by_slug[self.catalog["hero"]]
        status, ctype, data = self.pms.handle(
            "GET", f"/photo/:/transcode?url=/library/metadata/{rk}/thumb/1&width=300&height=450&minSize=1")
        self.assertEqual((status, ctype), (200, "image/jpeg"))
        self.assertEqual(data[:2], b"\xff\xd8")
        status, _, _ = self.pms.handle("GET", "/photo/:/transcode?url=/library/metadata/99999/thumb/1&width=10&height=10")
        self.assertEqual(status, 404)

    def test_a_clear_logo_is_served_as_a_transparent_png(self):
        # The path the app asks for (`ui/hero_logo.rs`), through the photo transcoder.
        ask = "/photo/:/transcode?url=/library/metadata/{}/clearLogo&width=600&height=240&minSize=1"
        rk = self.lib.by_slug[self.catalog["hero"]]
        status, ctype, data = self.pms.handle("GET", ask.format(rk))
        self.assertEqual((status, ctype), (200, "image/png"))
        w, h = struct.unpack(">II", data[16:24])
        self.assertEqual(data[25], 6, "colour type 6 is RGBA")
        self.assertTrue(w >= 600 and h >= 240, (w, h))
        bare = next(k for k in self.lib.items if "clearLogo" not in self.lib.images.get(k, {}))
        self.assertEqual(self.pms.handle("GET", ask.format(bare))[0], 404)

    def search(self, query, limit=12):
        # 12 is what the app asks for (`search::LIMIT`).
        hubs = self.get("/hubs/search?" + urllib.parse.urlencode({"query": query, "limit": limit}))["Hub"]
        return {h["type"]: [r.get("title") or r.get("tag") for r in h.get("Metadata", h.get("Directory", []))]
                for h in hubs}

    def test_search_caps_each_hub_at_limit_and_its_size_says_what_it_returned(self):
        hubs = self.get("/hubs/search?query=an&limit=2")["Hub"]
        movie = next(h for h in hubs if h["type"] == "movie")
        self.assertEqual((movie["size"], len(movie["Metadata"])), (2, 2))
        # No limit: PMS's own default of three rows per hub.
        hubs = self.get("/hubs/search?query=an")["Hub"]
        self.assertEqual(next(h for h in hubs if h["type"] == "movie")["size"], 3)

    def test_a_genre_match_brings_that_genres_movies_after_the_title_hits(self):
        hubs = {h["type"]: h for h in self.get("/hubs/search?query=co&limit=12")["Hub"]}
        rows = hubs["movie"]["Metadata"]
        # Direct title hits first, carrying no reason ...
        self.assertEqual([r["title"] for r in rows[:2]], ["Cosmos Laundromat", "Coffee Run"])
        self.assertTrue(all("reason" not in r for r in rows[:2]))
        # ... then the related films, each saying why it is there: the Comedy genre's, and
        # Caligari for Conrad Veidt, who acts in it.
        why = {r["title"]: (r["reason"], r["reasonTitle"]) for r in rows[2:]}
        self.assertEqual(why["Sprite Fright"], ("genre", "Comedy"))
        self.assertEqual(why["The Cabinet of Dr. Caligari"], ("actor", "Conrad Veidt"))
        # Only movies are related; Caminandes is a comedy, and still no show hit.
        self.assertEqual(self.search("an")["show"], [])

    def test_an_actor_match_brings_the_movies_they_act_in(self):
        hubs = {h["type"]: h for h in self.get("/hubs/search?query=halina&limit=12")["Hub"]}
        self.assertEqual([t["tag"] for t in hubs["actor"]["Directory"]], ["Halina Reijn"])
        self.assertEqual([(r["title"], r["reason"]) for r in hubs["movie"]["Metadata"]], [("Sintel", "actor")])
        # A director is not an actor: Fritz Lang's name brings no film.
        self.assertNotIn("Metropolis", self.search("fritz")["movie"])

    def test_search_matches_the_start_of_a_word_not_the_middle(self):
        # Word-prefix, as the mock assumes PMS does: "sp" begins Spring, Sprite and Space;
        # "in" sits inside Spring and Sintel and begins no word, so it finds no film.
        hits = self.search("sp")
        self.assertEqual(hits["movie"], ["Spring", "Sprite Fright", "Plan 9 from Outer Space"])
        self.assertEqual(self.search("in")["movie"], [])
        # Every word of the query must begin a word of the name, in any order.
        self.assertEqual(self.search("st te")["movie"], ["Tears of Steel"])
        self.assertEqual(self.search("sherlock holmes")["show"], ["Sherlock Holmes"])
        # People and collections match by the same rule.
        self.assertIn("Fritz Lang", self.search("fr")["actor"])
        self.assertEqual(self.search("si")["collection"], ["Silent Classics"])

    def test_a_one_character_search_finds_nothing(self):
        # PMS answers a one-character query with every hub empty (docs: tests/manifest.json).
        self.assertTrue(all(not rows for rows in self.search("s").values()))

    def test_two_libraries_serve_the_same_bytes(self):
        other = mock_pms.MockPms(mock_pms.CatalogLibrary(CATALOG))
        for path in ("/hubs?count=12", "/library/sections/1/all?sort=titleSort:asc", "/hubs/search?query=the",
                     f"/library/metadata/{self.lib.by_slug[self.catalog['hero']]}"):
            with self.subTest(path=path):
                self.assertEqual(self.pms.handle("GET", path), other.handle("GET", path))

    def test_nothing_reads_the_wall_clock(self):
        now = self.catalog["now"]
        for it in self.lib.items.values():
            self.assertLessEqual(it.get("addedAt", 0), now)
            self.assertLessEqual(it.get("lastViewedAt", 0), now)


if __name__ == "__main__":
    if not HAVE_CACHE:
        print("test_demo_library: the Catalog cases are SKIPPED — no derived demo cache "
              f"({mock_pms.demo_cache_dir()}); `make demo-library` builds it", file=sys.stderr)
    unittest.main(verbosity=1)
