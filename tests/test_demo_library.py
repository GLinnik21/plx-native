"""The demo library and the screenshot manifest: the contracts `make screenshots` relies on.

The first half needs nothing but this checkout — the manifests, the QR encoder, the plex.tv
stand-in and the scene manifest. The second half serves the catalog itself, which needs the
derived artwork cache (`make demo-library`, ~390 MB of downloads); it is skipped, loudly, where
the cache is absent, so a fresh clone's `make check` stays offline.
"""
import collections
import contextlib
import copy
import importlib.util
import io
import json
import os
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
        assets, catalog = tool.load()
        films = {m["id"]: m for m in catalog["movies"]}
        for film in (catalog["hero"], *catalog["hero_alternatives"]):
            with self.subTest(hero=film):
                self.assertIn("logo", films[film])
                logo = films[film]["logo"]["asset"]
                if logo != films[film]["poster"]["asset"]:  # else the film's own official title card
                    self.assertEqual((assets[logo]["kind"], assets[logo]["of"]), ("logo-title-card", film))

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

    # ---- cited metadata (tagline, contentRating, ratings, countries, creators) ---------------

    CITED = {"source": "https://www.wikidata.org/wiki/Q42", "retrieved": "2026-10-07"}

    def _with(self, **fields):
        """The catalog with `fields` merged into its first movie."""
        assets, catalog = tool.load()
        movies = [dict(m) for m in catalog["movies"]]
        movies[0].update(fields)
        return assets, dict(catalog, movies=movies)

    def test_check_accepts_a_cited_rating_and_content_rating(self):
        assets, catalog = self._with(
            ratings=[dict(self.CITED, image="imdb://image.rating", value=7.5, type="audience"),
                     dict(self.CITED, image="rottentomatoes://image.rating.ripe", value=9.1, type="critic")],
            contentRating=dict(self.CITED, value="PG"), countries=["Netherlands"])
        self.assertTrue(tool.check(assets, catalog))

    def test_check_refuses_a_rating_without_a_source_and_retrieval_date(self):
        row = {"image": "imdb://image.rating", "value": 7.5, "type": "audience"}
        for missing in ("source", "retrieved"):
            with self.subTest(missing=missing):
                cited = {k: v for k, v in self.CITED.items() if k != missing}
                assets, catalog = self._with(ratings=[dict(row, **cited)])
                with self.assertRaises(AssertionError):
                    tool.check(assets, catalog)

    def test_check_refuses_tmdb_rating_images_and_rt_certified(self):
        for image in ("themoviedb://image.rating", "tmdb://image.rating",
                      "rottentomatoes://image.rating.certified", "metacritic://image.rating"):
            with self.subTest(image=image):
                assets, catalog = self._with(
                    ratings=[dict(self.CITED, image=image, value=7.5, type="critic")])
                with self.assertRaises(AssertionError):
                    tool.check(assets, catalog)

    def test_check_refuses_an_uncited_content_rating(self):
        assets, catalog = self._with(contentRating={"value": "PG"})
        with self.assertRaises(AssertionError):
            tool.check(assets, catalog)

    def test_check_refuses_a_tagline_that_is_not_marked_demo(self):
        # The first movie now carries its own marked tagline, so the unmarked case clears the mark.
        assets, catalog = self._with(tagline="Our own words.", demo_values=[])
        with self.assertRaises(AssertionError):
            tool.check(assets, catalog)
        assets, catalog = self._with(tagline="Our own words.", demo_values=["tagline"])
        self.assertTrue(tool.check(assets, catalog))

    def test_check_refuses_creators_on_a_movie(self):
        assets, catalog = self._with(creators=["Someone"])
        with self.assertRaises(AssertionError):
            tool.check(assets, catalog)

    def test_the_two_complete_titles_record_where_each_fact_came_from(self):
        _, catalog = tool.load()
        films = {m["id"]: m for m in catalog["movies"]}
        for film in ("sintel", "tears-of-steel"):
            with self.subTest(film=film):
                rec = films[film]
                self.assertTrue(rec["countries"])
                self.assertTrue(rec["writers"])
                self.assertIn("tagline", rec["demo_values"])
                self.assertTrue(rec["sources"])
                for src in rec["sources"]:
                    self.assertTrue(src["url"].startswith("https://"))
                    self.assertRegex(src["retrieved"], r"^\d{4}-\d{2}-\d{2}$")


class Completeness(unittest.TestCase):
    """`check --complete`: what a title must carry to be shown in the demo library, judged on the
    committed manifests plus the Rust layout constants `HERO_PINS` re-reads (no image is ever
    opened and nothing needs the network)."""

    # The pending list may only SHRINK. Its length per category is pinned here, in the test, not in
    # the list's own file: growing the list means editing this table, which a reviewer sees. When a
    # content PR fixes a gap it deletes the entry from `pending.json` AND lowers the number here;
    # a category that reaches zero leaves the table.
    PENDING_CEILING = {"country": 1, "creators": 1, "writers": 1}

    CITED = {"source": "https://www.wikidata.org/wiki/Q42", "retrieved": "2026-10-07"}

    def setUp(self):
        self.assets, self.catalog = tool.load()

    def _gaps(self, key, **changes):
        """The gaps of movie `key` after `changes` (a None value removes the field), bar `hero_art`:
        no title declares a subject box yet, and `HeroGeometry` is where that category is tested."""
        movies = []
        for m in self.catalog["movies"]:
            m = copy.deepcopy(m)
            if m["id"] == key:
                for k, v in changes.items():
                    m.pop(k, None) if v is None else m.__setitem__(k, v)
            movies.append(m)
        gaps = tool.complete_gaps(self.assets, dict(self.catalog, movies=movies)).get(key, [])
        return [g for g in gaps if g != "hero_art"]

    def test_sintel_and_tears_of_steel_are_complete_and_sprite_fright_is_kept_out_of_the_hero_pool(self):
        gaps = tool.complete_gaps(self.assets, self.catalog)
        self.assertNotIn("sintel", gaps)
        self.assertNotIn("tears-of-steel", gaps)
        # Sprite Fright has no still that clears the hero zones, so it is `not_hero`, not a gap
        sprite = next(m for m in self.catalog["movies"] if m["id"] == "sprite-fright")
        self.assertTrue(sprite["not_hero"])
        self.assertNotIn("sprite-fright", gaps)
        self.assertNotIn("sprite-fright", tool.hero_pool(self.catalog))

    def test_home_and_the_movies_tab_never_show_one_poster_in_both_rows_they_stack(self):
        # Continue Watching sits directly above Recently Added on Home and on the Movies tab. Seven
        # cards fill the row, so the first seven of each must be different films with real posters.
        cat = self.catalog
        movies = {m["id"]: m for m in cat["movies"]}
        cw = [e["item"] for e in cat["continue_watching"] if e["item"] in movies]
        ra = [i for i in cat["added_order"] if i in movies]
        self.assertGreaterEqual(len(cw), 7, "the Movies tab's Continue Watching row must fill the width")
        first_cw, first_ra = cw[:7], ra[:7]
        self.assertEqual(first_cw[0], cat["hero"])
        self.assertIn("Blender Open Movies", movies[first_cw[1]].get("collections", []))
        self.assertEqual(set(first_cw) & set(first_ra), set())
        for film in first_cw + first_ra:
            with self.subTest(film=film):
                self.assertFalse(movies[film].get("still_as_poster"), "a still is not a poster")
        # a show's episode is in the row too, but past the seven visible cards
        episodes = [n for n, e in enumerate(cat["continue_watching"]) if e["item"] not in movies]
        self.assertTrue(all(n >= 7 for n in episodes))

    def test_each_required_field_is_a_gap_when_absent(self):
        # field removed from a complete title -> the category it is reported under
        cases = {"title": "title", "year": "year", "summary": "summary", "genres": "genres",
                 "directors": "directors", "tagline": "tagline", "cast": "cast",
                 "writers": "writers", "countries": "country", "poster": "poster",
                 "art": "backdrop", "logo": "logo"}
        for field, category in cases.items():
            with self.subTest(field=field):
                self.assertEqual(self._gaps("sintel", **{field: None}).count(category), 1)

    def test_a_cited_cast_none_or_writers_none_stands_in_for_the_list(self):
        self.assertIn("cast", self._gaps("sintel", cast=[]))
        self.assertEqual(self._gaps("sintel", cast=[], cast_none=dict(self.CITED, note="no performers")), [])
        self.assertIn("writers", self._gaps("sintel", writers=[]))
        self.assertEqual(self._gaps("sintel", writers=[], writers_none=dict(self.CITED, note="checked")), [])

    def test_check_refuses_an_uncited_cast_none_or_a_contradicting_one(self):
        for field in ("cast_none", "writers_none"):
            with self.subTest(field=field):
                for bad in ({"note": "trust me"}, dict(self.CITED, source="http://x")):
                    movies = [dict(m) for m in self.catalog["movies"]]
                    movies[0][field] = bad
                    with self.assertRaises(AssertionError):
                        tool.check(self.assets, dict(self.catalog, movies=movies))
        movies = [dict(m) for m in self.catalog["movies"]]
        sintel = next(m for m in movies if m["id"] == "sintel")
        sintel["cast_none"] = dict(self.CITED, note="x")  # sintel HAS a cast
        with self.assertRaises(AssertionError):
            tool.check(self.assets, dict(self.catalog, movies=movies))

    def test_a_tagline_must_be_listed_as_demo_text(self):
        self.assertIn("tagline", self._gaps("sintel", demo_values=[]))

    def test_the_art_must_be_the_kind_its_role_needs(self):
        # a portrait one-sheet is not a backdrop
        self.assertIn("backdrop", self._gaps("sintel", art={"asset": "sintel-poster", "mode": "cover"}))
        # a header is not a poster
        self.assertIn("poster", self._gaps("sintel", poster={"asset": "bs-sintel-header"}))
        # a logo is cut from a poster-kind asset
        logo = dict(copy.deepcopy(next(m for m in self.catalog["movies"] if m["id"] == "sintel")["logo"]),
                    asset="bs-sintel-header")
        self.assertIn("logo", self._gaps("sintel", logo=logo))

    def test_an_episode_needs_a_still(self):
        shows = copy.deepcopy(self.catalog["shows"])
        ep = shows[0]["seasons"][0]["episodes"][0]
        key = f"{shows[0]['id']}/1/{ep['index']}"
        self.assertNotIn(key, tool.complete_gaps(self.assets, dict(self.catalog, shows=shows)))
        ep.pop("thumb")
        self.assertEqual(tool.complete_gaps(self.assets, dict(self.catalog, shows=shows))[key], ["still"])
        ep["thumb"] = {"asset": "sintel-poster"}  # a poster is not a still
        self.assertEqual(tool.complete_gaps(self.assets, dict(self.catalog, shows=shows))[key], ["still"])

    def test_a_show_needs_creators_where_a_movie_needs_directors(self):
        shows = copy.deepcopy(self.catalog["shows"])
        shows[0].pop("creators", None)
        self.assertIn("creators", tool.complete_gaps(self.assets, dict(self.catalog, shows=shows))[shows[0]["id"]])

    def test_every_asset_carries_a_kind_and_its_dimensions(self):
        for field, bad in (("kind", None), ("kind", "banner"), ("width", None), ("width", 0),
                           ("height", None), ("height", "720")):
            with self.subTest(field=field, bad=bad):
                a = {k: v for k, v in self.assets["sintel-poster"].items() if k != field}
                if bad is not None:
                    a[field] = bad
                with self.assertRaises(AssertionError):
                    tool.check(dict(self.assets, **{"sintel-poster": a}), self.catalog)

    def test_a_poster_role_gap_is_never_hidden_by_the_kinds(self):
        # every use of an asset in a role its kind does not fit is a reported gap
        roles = {"poster": ({"poster"}, "poster"), "art": ({"backdrop", "still"}, "backdrop"),
                 "thumb": ({"still"}, "still")}
        gaps = tool.complete_gaps(self.assets, self.catalog)
        for key, _, rec in tool.item_keys(self.catalog):
            for role, (kinds, category) in roles.items():
                if role in rec and self.assets[rec[role]["asset"]]["kind"] not in kinds:
                    if role == "poster" and rec[role].get("still_as_poster"):
                        # the one way a non-poster fills the role: declared, and approved by the owner
                        self.assertIn(key, self.catalog["still_poster_approved"])
                        self.assertNotIn(category, gaps.get(key, []), f"{key}: {role}")
                        continue
                    self.assertIn(category, gaps.get(key, []), f"{key}: {role}")

    def test_assets_must_be_cc0_public_domain_or_cc_by_without_share_alike(self):
        tool.check_assets_complete(self.assets)
        for licence in ("CC BY-SA 4.0", "CC BY-SA 3.0", "CC BY 2.0", "CC BY-NC 4.0", "GFDL"):
            with self.subTest(licence=licence):
                broken = dict(self.assets, **{"sintel-poster": dict(self.assets["sintel-poster"], licence=licence)})
                with self.assertRaises(AssertionError):
                    tool.check_assets_complete(broken)

    def test_a_headshot_must_not_carry_the_personality_rights_tag(self):
        shot = dict(self.assets["sintel-poster"], kind="headshot", tags=[])
        tool.check_assets_complete(dict(self.assets, face=shot))
        for tags in (["Personality-Rights"], ["cc-by-4.0", "personality rights"]):
            with self.subTest(tags=tags):
                with self.assertRaises(AssertionError):
                    tool.check_assets_complete(dict(self.assets, face=dict(shot, tags=tags)))
        # no recorded tags is "not looked at", not "clear"
        with self.assertRaises(AssertionError):
            tool.check_assets_complete(dict(self.assets, face={k: v for k, v in shot.items() if k != "tags"}))

    def test_the_committed_gaps_are_exactly_the_pending_list(self):
        gaps = tool.complete_gaps(self.assets, self.catalog)
        self.assertEqual(gaps, tool.load_pending(), "pending.json must list every current gap and "
                         "nothing that is fixed: delete the entry you fixed; never add one")

    def test_a_new_gap_is_refused_and_so_is_a_stale_pending_entry(self):
        gaps = tool.complete_gaps(self.assets, self.catalog)
        tool.check_complete(self.assets, self.catalog, pending=gaps)
        with self.assertRaises(AssertionError):
            tool.check_complete(self.assets, self.catalog, pending={})
        with self.assertRaises(AssertionError):
            tool.check_complete(self.assets, self.catalog, pending=dict(gaps, sintel=["cast"]))

    def test_the_pending_list_never_grows(self):
        counts = collections.Counter(f for fields in tool.load_pending().values() for f in fields)
        self.assertEqual(dict(counts), self.PENDING_CEILING,
                         "pending grew (never allowed) or shrank (lower PENDING_CEILING to match)")

    def test_pending_names_only_known_items_and_categories(self):
        keys = {k for k, _, _ in tool.item_keys(self.catalog)}
        for key, fields in tool.load_pending().items():
            self.assertIn(key, keys)
            self.assertEqual(fields, sorted(set(fields)))
            self.assertLessEqual(set(fields), tool.GAP_CATEGORIES)

    def test_the_command_runs_offline_and_passes_with_the_list_in_place(self):
        r = subprocess.run([sys.executable, str(ROOT / "tools" / "demo_library.py"), "check", "--complete"],
                           capture_output=True, text=True,
                           env=dict(os.environ, PLXNATIVE_DEMO_CACHE=str(ROOT / "no-such-cache")))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("pending", r.stdout)


class HeroGeometry(unittest.TestCase):
    """The hero-art check: a declared subject box in SOURCE pixels is mapped into the derived
    1920x1080 frame by arithmetic alone (no image is opened) and held against the text zones the
    Rust layout pins. The expectations below are worked by hand from the cover rule: scale to cover,
    then crop at the anchor."""

    def setUp(self):
        self.assets, self.catalog = tool.load()

    def test_the_pinned_hero_declares_a_box_without_changing_a_pixel_of_its_backdrop(self):
        # Sintel's derived backdrop is in docs/screenshots/home.jpg and the site close-ups: declaring
        # its subject must leave the recipe stamp (what decides the pixels) as it was.
        films = {m["id"]: m for m in self.catalog["movies"]}
        art = films["sintel"]["art"]
        self.assertEqual(tool.recipe_stamp(art), {"asset": "bs-sintel-header", "mode": "cover", "anchor": "right"})
        v = tool.hero_geometry(self.assets, films["sintel"])
        self.assertEqual(v["verdict"], "pass", v["reasons"])
        # 2048x872 -> scale 1080/872, cut at the right: x*1.2385-616.4, y*1.2385
        for got, want in zip(v["box"], (1047.0, 290.0, 1322.0, 701.0)):
            self.assertAlmostEqual(got, want, delta=1.0)

    def test_tears_of_steel_is_a_cover_crop_with_the_face_clear_of_every_text_zone(self):
        films = {m["id"]: m for m in self.catalog["movies"]}
        v = tool.hero_geometry(self.assets, films["tears-of-steel"])
        self.assertEqual(v["verdict"], "pass", v["reasons"])
        # 3840x1600 -> scale 0.675, cut at the left (nothing taken from it): x*0.675, y*0.675
        for got, want in zip(v["box"], (1181.25, 121.5, 1795.5, 700.65)):
            self.assertAlmostEqual(got, want, delta=1.0)
        self.assertGreaterEqual(v["centre_x"], tool.HERO_CENTRE_MIN_X)
        self.assertEqual({n: a for n, a in v["overlaps"].items() if a}, {})

    def art(self, w, h, subject, **recipe):
        """(assets, record) for a title whose backdrop is a `w`x`h` source with `recipe`."""
        assets = {"src": {"kind": "backdrop", "width": w, "height": h}}
        art = dict({"asset": "src", "mode": "cover", "anchor": "center"}, **recipe)
        if subject is not None:
            art["subject"] = subject
        return assets, {"id": "t", "art": art}

    def verdict(self, w, h, subject, **recipe):
        assets, rec = self.art(w, h, subject, **recipe)
        return tool.hero_geometry(assets, rec)

    # ---- the mapping -------------------------------------------------------------------------

    def test_a_source_of_the_frames_aspect_is_only_scaled(self):
        # 3840x2160 -> scale 1/2, no crop: (2400,200,3200,1000) -> (1200,100,1600,500)
        v = self.verdict(3840, 2160, [2400, 200, 3200, 1000])
        self.assertEqual(v["box"], [1200, 100, 1600, 500])
        self.assertEqual(v["verdict"], "pass", v)

    def test_a_right_anchor_pins_the_sources_right_edge_to_the_frames(self):
        # 2048x858 is wider than 16:9: the height decides, s = 1080/858 = 180/143, and a right
        # anchor keeps the right edge, so x' = 1920 - (2048 - x) * s and y' = y * s.
        s = 180 / 143
        box = tool.hero_map_box(2048, 858, {"mode": "cover", "anchor": "right"}, [1500, 100, 1900, 500])
        for got, want in zip(box, [1920 - 548 * s, 100 * s, 1920 - 148 * s, 500 * s]):
            self.assertAlmostEqual(got, want, places=6)
        self.assertAlmostEqual(box[0], 1230.2098, places=3)  # worked out longhand

    def test_a_centre_anchor_crops_the_overflow_equally(self):
        # same source: the scaled picture is 2048*s = 2577.9021 wide, 657.9021 over, 328.9510 a side
        box = tool.hero_map_box(2048, 858, {"mode": "cover", "anchor": "center"}, [1000, 100, 1500, 500])
        self.assertAlmostEqual(box[0], 1000 * 180 / 143 - 328.9510, places=3)
        self.assertAlmostEqual(box[0], 929.7902, places=3)
        self.assertAlmostEqual(box[2], 1500 * 180 / 143 - 328.9510, places=3)
        # no anchor at all is the centre (`derive_image`'s default)
        self.assertEqual(tool.hero_map_box(2048, 858, {}, [1000, 100, 1500, 500]), box)

    def test_a_left_anchor_keeps_the_left_edge(self):
        box = tool.hero_map_box(2048, 858, {"mode": "cover", "anchor": "left"}, [100, 0, 400, 858])
        self.assertAlmostEqual(box[0], 100 * 180 / 143, places=6)
        self.assertAlmostEqual(box[2], 400 * 180 / 143, places=6)

    def test_a_portrait_source_is_cropped_by_height_and_upper_sits_at_thirty_percent(self):
        # 1000x2000: the width decides, s = 1.92, scaled height 3840, 2760 over; `upper` drops
        # 30% of it above the frame (828) -> y' = y*1.92 - 828; x is centred (no overflow)
        box = tool.hero_map_box(1000, 2000, {"mode": "cover", "anchor": "upper"}, [500, 500, 900, 700])
        for got, want in zip(box, [960, 132, 1728, 516]):
            self.assertAlmostEqual(got, want, places=6)
        # `crop: focus-right` is the right anchor by another name (`derive_image`'s own table)
        a = tool.hero_map_box(2048, 858, {"crop": "focus-right"}, [1500, 100, 1900, 500])
        b = tool.hero_map_box(2048, 858, {"anchor": "right"}, [1500, 100, 1900, 500])
        self.assertEqual(a, b)

    def test_the_anchor_table_is_the_one_the_deriver_cuts_with(self):
        # the arithmetic and the ffmpeg crop must name the same anchors the same way
        for anchor, x in (("right", "iw-ow"), ("left", "0"), ("center", "(iw-ow)/2"), ("upper", "(iw-ow)/2")):
            self.assertIn(f":{x}:", tool._cover(1920, 1080, anchor))
        self.assertIn("(ih-oh)*3/10", tool._cover(1920, 1080, "upper"))
        self.assertEqual(set(tool.COVER_ANCHORS), {"right", "left", "upper"})

    # ---- the verdict -------------------------------------------------------------------------

    def test_extend_is_banned_for_hero_art(self):
        v = self.verdict(2048, 858, [1500, 100, 1900, 500], mode="extend", anchor="right")
        self.assertEqual(v["verdict"], "fail")
        self.assertEqual(v["reasons"], ["extend"])
        self.assertIsNone(v["box"])

    def test_an_unknown_mode_is_refused_too(self):
        self.assertEqual(self.verdict(2048, 858, [1500, 100, 1900, 500], mode="stretch")["reasons"], ["mode"])

    def test_a_title_without_a_declared_subject_fails(self):
        v = self.verdict(2048, 858, None, anchor="right")
        self.assertEqual((v["verdict"], v["reasons"]), ("fail", ["no-subject"]))

    def test_a_malformed_subject_fails(self):
        for bad in ([1, 2, 3], [10, 10, 10, 50], [50, 10, 10, 50], "1,2,3,4", [1, 2, 3, "4"], [-1, 0, 5, 5]):
            with self.subTest(subject=bad):
                self.assertEqual(self.verdict(2048, 858, bad)["reasons"], ["bad-subject"])

    def test_a_box_outside_the_source_image_fails(self):
        for box in ([1500, 100, 2049, 500], [100, 100, 500, 859]):
            with self.subTest(box=box):
                v = self.verdict(2048, 858, box, anchor="right")
                self.assertEqual((v["verdict"], v["reasons"]), ("fail", ["outside-source"]))
        # the whole image is in bounds; what fails it is the crop, not the source
        self.assertEqual(self.verdict(2048, 858, [0, 0, 2048, 858])["reasons"], ["outside-frame"])

    def test_a_subject_the_cover_crop_cuts_off_fails(self):
        # centre anchor loses 328.9 scaled px (261 source px) on each side of the 2048x858 art
        v = self.verdict(2048, 858, [100, 100, 600, 500])
        self.assertEqual((v["verdict"], v["reasons"]), ("fail", ["outside-frame"]))
        self.assertLess(v["box"][0], 0)
        # the portrait case: a box low in the source falls below the frame
        v = self.verdict(1000, 2000, [500, 1000, 900, 1500], anchor="upper")
        self.assertEqual(v["reasons"], ["outside-frame"])

    def test_a_subject_in_the_text_column_fails_and_names_the_zone(self):
        v = self.verdict(3840, 2160, [600, 700, 1200, 1000])  # frame (300,350)-(600,500)
        self.assertEqual(v["verdict"], "fail")
        self.assertIn("overlap:home text column", v["reasons"])
        self.assertGreater(v["overlaps"]["home text column"], 0)

    def test_a_subject_on_the_shelf_row_fails(self):
        v = self.verdict(3840, 2160, [2800, 1700, 3400, 2100])  # frame (1400,850)-(1700,1050)
        self.assertIn("overlap:home shelf row", v["reasons"])

    def test_a_subject_in_the_starring_column_fails(self):
        v = self.verdict(3840, 2160, [3000, 1420, 3400, 1540])  # frame (1500,710)-(1700,770)
        self.assertEqual(v["reasons"], ["overlap:detail starring column"])  # y 770 is above the shelf row

    def test_a_subject_over_the_detail_synopsis_or_facts_fails(self):
        v = self.verdict(3840, 2160, [1800, 700, 2000, 900])  # frame (900,350)-(1000,450)
        self.assertIn("overlap:detail text column", v["reasons"])
        v = self.verdict(3840, 2160, [2100, 1420, 2500, 1500])  # frame (1050,710)-(1250,750)
        self.assertEqual(v["reasons"], ["centre-left", "overlap:detail facts row"])

    def test_touching_a_zone_is_not_overlapping_it(self):
        # frame (1039,350)-(1400,500): its left edge is the detail text column's right edge
        v = self.verdict(3840, 2160, [2078, 700, 2800, 1000])
        self.assertEqual(v["verdict"], "pass", v)
        v = self.verdict(3840, 2160, [2076, 700, 2800, 1000])  # one source pixel further in
        self.assertIn("overlap:detail text column", v["reasons"])

    def test_a_subject_left_of_sixty_percent_fails_on_its_centre(self):
        v = self.verdict(3840, 2160, [2080, 700, 2400, 1000])  # frame (1040,350)-(1200,500): centre 1120
        self.assertEqual(v["reasons"], ["centre-left"])
        self.assertAlmostEqual(v["centre_x"], 1120)

    def test_a_right_weighted_subject_clear_of_every_zone_passes(self):
        v = self.verdict(2048, 872, [1500, 100, 1900, 500], anchor="right")
        self.assertEqual((v["verdict"], v["reasons"]), ("pass", []), v)
        # the scrim is advisory: the share of the box under it is reported, never gated
        self.assertGreater(v["scrim_share"], 0)

    # ---- eligibility and the gate ------------------------------------------------------------

    def test_a_title_in_the_hero_pool_is_eligible_and_one_outside_it_is_not(self):
        cat = {"movies": [{"id": "a"}, {"id": "b"}, {"id": "c"}, {"id": "d"}],
               "shows": [{"id": "s", "seasons": [{"index": 1, "episodes": [{"index": 1}]}]}],
               "hero": "a", "hero_alternatives": [], "added_order": ["b"],
               "continue_watching": [{"item": "s/1/1", "progress": 0.5}]}
        self.assertEqual(tool.hero_eligible(cat), ["a", "b", "s"])  # an episode counts for its show

    def test_every_catalog_title_not_flagged_not_hero_is_in_the_hero_pool_today(self):
        titles = {m["id"] for m in self.catalog["movies"] + self.catalog["shows"]}
        flagged = {m["id"] for m in self.catalog["movies"] + self.catalog["shows"] if m.get("not_hero")}
        self.assertTrue(flagged)
        self.assertEqual(set(tool.hero_eligible(self.catalog)), titles - flagged)

    def _gap(self, w, h, subject, eligible=True, **recipe):
        assets, rec = self.art(w, h, subject, **recipe)
        cat = {"movies": [rec], "shows": [], "hero": "t" if eligible else "u", "hero_alternatives": [],
               "added_order": [], "continue_watching": []}
        return tool.hero_gap(assets, cat, rec)

    def test_hero_art_is_a_gap_unless_the_geometry_passes(self):
        self.assertTrue(self._gap(2048, 872, None, anchor="right"))
        self.assertTrue(self._gap(2048, 872, [1500, 100, 1900, 500], anchor="center"))
        self.assertFalse(self._gap(2048, 872, [1500, 100, 1900, 500], anchor="right"))

    def test_a_title_outside_the_hero_pool_is_never_a_hero_art_gap(self):
        self.assertFalse(self._gap(2048, 872, None, eligible=False, anchor="right"))

    def test_a_title_with_no_art_is_a_backdrop_gap_not_a_hero_art_gap(self):
        movies = copy.deepcopy(self.catalog["movies"])
        sintel = next(m for m in movies if m["id"] == "sintel")
        del sintel["art"]
        gaps = tool.complete_gaps(self.assets, dict(self.catalog, movies=movies))
        self.assertEqual(gaps["sintel"], ["backdrop"])

    def test_a_title_with_art_is_a_hero_art_gap_only_while_it_is_hero_eligible_and_lacks_a_passing_subject(self):
        gaps = tool.complete_gaps(self.assets, self.catalog)
        eligible = set(tool.hero_eligible(self.catalog))
        for m in self.catalog["movies"] + self.catalog["shows"]:
            if "art" not in m:
                continue
            with self.subTest(title=m["id"]):
                verdict = tool.hero_geometry(self.assets, m)["verdict"]
                self.assertEqual("hero_art" in gaps.get(m["id"], []), m["id"] in eligible and verdict == "fail")
        # every title the home hero can show declares a passing box, so none is a gap
        self.assertEqual(sorted(k for k, g in gaps.items() if "hero_art" in g), [])

    def test_a_passing_declared_subject_clears_the_pending_entry(self):
        movies = copy.deepcopy(self.catalog["movies"])
        charge = next(m for m in movies if m["id"] == "charge")
        self.assertEqual((charge["art"]["mode"], charge["art"]["anchor"]), ("cover", "right"))
        del charge["art"]["subject"]
        undeclared = dict(self.catalog, movies=movies)
        self.assertEqual(tool.complete_gaps(self.assets, undeclared)["charge"], ["hero_art"])
        pending = dict(tool.load_pending(), charge=["hero_art"])
        tool.check_complete(self.assets, undeclared, pending=pending)  # listed, so the gate holds
        with self.assertRaises(AssertionError) as e:  # declared and passing, the entry is stale: refused
            tool.check_complete(self.assets, self.catalog, pending=pending)
        self.assertIn("hero_art", str(e.exception))

    def test_the_subject_does_not_change_the_derive_stamp(self):
        a = {"asset": "x", "mode": "cover", "anchor": "right"}
        self.assertEqual(tool.recipe_stamp(a), tool.recipe_stamp(dict(a, subject=[1, 2, 3, 4])))
        self.assertNotEqual(tool.recipe_stamp(a), tool.recipe_stamp(dict(a, anchor="left")))

    def test_two_recipes_for_one_asset_derive_to_two_paths_and_a_subject_to_one(self):
        # The cache is shared by every checkout on a machine: a file named by the title alone is
        # overwritten by whichever checkout derived last, so the name carries the recipe's hash.
        assets = {"x": {"sha256": "a" * 64}, "y": {"sha256": "b" * 64}}
        rec = {"art": {"asset": "x", "mode": "cover", "anchor": "right"}}
        path = lambda r, role="art", cache="/c": tool.derived_file(assets, cache, "film", r, role)
        again = {"art": dict(rec["art"], subject=[1, 2, 3, 4])}
        self.assertEqual(path(rec), path(again))  # a declaration about the picture changes no pixel
        for other in ({"art": dict(rec["art"], anchor="left")}, {"art": dict(rec["art"], asset="y")},
                      {"art": dict(rec["art"], mode="extend")}):
            self.assertNotEqual(path(rec), path(other), other)
        self.assertNotEqual(path(rec), path({"poster": rec["art"]}, "poster"))
        self.assertRegex(str(path(rec)), r"^/c/derived/film/art-[0-9a-f]{12}\.jpg$")
        self.assertRegex(str(path({"logo": {"asset": "x", "box": [0, 0, 1, 1]}}, "logo")), r"/logo-[0-9a-f]{12}\.png$")
        self.assertRegex(str(tool.derived_file(assets, "/c", "show/1/2", {"minutes": 3}, "stand-in")),
                         r"/derived/show_1_2/stand-in-[0-9a-f]{12}\.mp4$")
        before = path(rec)
        with unittest.mock.patch.object(tool, "DERIVE_VERSION", tool.DERIVE_VERSION + 1):
            self.assertNotEqual(before, tool.derived_file(assets, "/c", "film", rec, "art"))

    def test_the_mock_serves_a_file_from_the_path_derive_wrote(self):
        assets = {"x": {"sha256": "a" * 64}}
        rec = {"art": {"asset": "x", "mode": "cover", "anchor": "right"}}
        self.assertEqual(mock_pms.demo_derived_file(assets, "/c", "film", rec, "art"),
                         tool.derived_file(assets, "/c", "film", rec, "art"))

    # ---- the zones are pinned to the Rust layout ---------------------------------------------

    def test_the_python_zone_numbers_equal_the_rust_constants_they_copy(self):
        tool.check_hero_pins()

    def test_the_zones_are_built_from_the_pins_and_every_pin_feeds_a_zone(self):
        zones = tool.hero_zones()
        self.assertEqual([z["name"] for z in zones if not z.get("advisory")],
                         ["home text column", "home shelf row", "detail text column",
                          "detail facts row", "detail starring column"])
        by = {z["name"]: z["rect"] for z in zones}
        self.assertEqual(by["home text column"], [96, 260, 756, 776])
        self.assertEqual(by["home shelf row"], [0, 777, 1920, 1080])
        self.assertEqual(by["detail text column"], [96, 298, 1039, 1080])
        self.assertEqual(by["detail facts row"], [1039, 702, 1264, 830])
        self.assertEqual(by["detail starring column"], [1264, 702, 1824, 1080])
        self.assertEqual(by["hero scrim"], [0, 162, 1536, 1080])
        used = {n for z in zones for n in z["uses"]}
        self.assertEqual(used, set(tool.HERO_PINS) - {n for n, p in tool.HERO_PINS.items() if p[1] == "text"})

    def test_a_drifted_python_copy_fails_naming_the_constant_and_its_file(self):
        for name, wrong in (("MARGIN_X", 90.0), ("COL_W", 600.0), ("PEEK_Y", 828.0), ("HERO_TEXT_W", 990.0),
                            ("HERO_LOGO_TOP_HOME", 300.0), ("HERO_SCRIM_W", 1500.0)):
            with self.subTest(name=name):
                pins = {k: list(v) for k, v in tool.HERO_PINS.items()}
                pins[name][-1] = wrong
                with self.assertRaises(AssertionError) as e:
                    tool.check_hero_pins(pins)
                self.assertIn(name, str(e.exception))
                self.assertIn(tool.HERO_PINS[name][0], str(e.exception))

    def test_a_changed_or_removed_rust_constant_fails_the_pin_check(self):
        with tempfile.TemporaryDirectory() as d:
            root = pathlib.Path(d)
            for pin in (*tool.HERO_PINS.values(), *tool.HERO_POOL_PINS.values()):
                dst = root / pin[0]
                if not dst.exists():
                    dst.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy(ROOT / pin[0], dst)
            tool.check_hero_pins(root=root)  # the copy is faithful
            f = root / "rust-modules" / "ui" / "src" / "landing_hero.rs"
            f.write_text(f.read_text().replace("pub const COL_W: f32 = 660.0;", "pub const COL_W: f32 = 700.0;"))
            with self.assertRaises(AssertionError) as e:
                tool.check_hero_pins(root=root)
            self.assertIn("COL_W", str(e.exception))
            # a constant that has vanished is as stale as one that moved
            f.write_text(f.read_text().replace("pub const COL_W: f32 = 700.0;", ""))
            with self.assertRaises(AssertionError) as e:
                tool.check_hero_pins(root=root)
            self.assertIn("COL_W", str(e.exception))

    def test_a_changed_layout_formula_fails_the_pin_check(self):
        # the zones that are a FORMULA (the shelf's heading line) pin the source text of the rule
        with tempfile.TemporaryDirectory() as d:
            root = pathlib.Path(d)
            for pin in tool.HERO_PINS.values():
                dst = root / pin[0]
                if not dst.exists():
                    dst.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy(ROOT / pin[0], dst)
            f = root / "rust-modules" / "screens" / "src" / "home" / "mod.rs"
            f.write_text(f.read_text().replace("row_y - TITLE_DY - lift", "row_y - lift"))
            with self.assertRaises(AssertionError) as e:
                tool.check_hero_pins(root=root)
            self.assertIn("HOME_SHELF_HEADING_RULE", str(e.exception))

    def test_rust_expressions_are_evaluated_from_the_pinned_names(self):
        ev = tool._rust_expr
        self.assertEqual(ev("0.80 * crate::consts::SCR_W", {"SCR_W": 1920.0}), 1536.0)
        self.assertEqual(ev("HERO_TEXT_BOTTOM + theme::space::MD", {"HERO_TEXT_BOTTOM": 692.0, "MD": 24.0}), 716.0)
        self.assertEqual(ev("0.15 * crate::consts::SCR_H", {"SCR_H": 1080.0}), 162.0)
        with self.assertRaises(AssertionError):
            ev("UNKNOWN * 2.0", {})
        with self.assertRaises(AssertionError):
            ev("__import__('os')", {})

    # ---- the command -------------------------------------------------------------------------

    def _run(self, *args):
        return subprocess.run([sys.executable, str(ROOT / "tools" / "demo_library.py"), *args],
                              capture_output=True, text=True,
                              env=dict(os.environ, PLXNATIVE_DEMO_CACHE=str(ROOT / "no-such-cache")))

    def test_hero_report_prints_each_eligible_title_offline(self):
        r = self._run("hero-report")
        self.assertEqual(r.returncode, 0, r.stderr)
        for key in ("sintel", "tears-of-steel", "spring", "charge", "cosmos-laundromat", "hero", "metropolis",
                    "safety-last", "the-daily-dweebs", "the-general", "singularity"):
            self.assertRegex(r.stdout, rf"(?m)^{key} .*\bpass\b")
        for key in ("wing-it", "sprite-fright"):  # `not_hero` titles are not in the report
            self.assertNotRegex(r.stdout, rf"(?m)^{key} ")
        self.assertIn("hero art: 11 pass, 0 fail, 0 skip", r.stdout)
        self.assertIn("home text column", r.stdout)  # the zone table, with its pins
        self.assertIn("rust-modules/ui/src/landing_hero.rs", r.stdout)

    def test_hero_report_shows_the_mapped_box_and_overlaps_of_a_declared_title(self):
        cat = copy.deepcopy(self.catalog)
        next(m for m in cat["movies"] if m["id"] == "sintel")["art"]["subject"] = [100, 100, 500, 500]
        with contextlib.redirect_stdout(io.StringIO()) as out:
            tool.hero_report(self.assets, cat)
        line = next(x for x in out.getvalue().splitlines() if x.startswith("sintel "))
        self.assertIn("outside-frame", line)
        self.assertIn("[100, 100, 500, 500]", line)

    def test_check_complete_runs_the_pin_check(self):
        with unittest.mock.patch.object(tool, "check_hero_pins", side_effect=AssertionError("drift")):
            with self.assertRaises(AssertionError):
                tool.check_complete(self.assets, self.catalog)


class TitleFlags(unittest.TestCase):
    """The per-title flags and approval lists the module docstring describes: `not_hero`,
    `not_in_video`, `poster.still_as_poster` with `still_poster_approved`, and the
    `logo-title-card` asset kind."""

    def setUp(self):
        self.assets, self.catalog = tool.load()

    def cat(self, **per_title):
        """The catalog with `{id: {field: value}}` merged into those titles (a None value removes)."""
        cat = copy.deepcopy(self.catalog)
        top = {k: per_title.pop(k) for k in list(per_title)
               if k in cat or k in ("still_poster_approved", "logo_none_approved")}
        cat.update(top)
        for rec in cat["movies"] + cat["shows"]:
            for k, v in per_title.get(rec["id"], {}).items():
                rec.pop(k, None) if v is None else rec.__setitem__(k, v)
        return cat

    def refused(self, cat, assets=None):
        with self.assertRaises(AssertionError) as e:
            tool.check(assets or self.assets, cat)
        return str(e.exception)

    # ---- not_hero ----------------------------------------------------------------------------

    def test_not_hero_takes_a_title_out_of_hero_eligibility_and_its_hero_art_gap(self):
        self.assertNotIn("big-buck-bunny", tool.hero_eligible(self.catalog))  # committed: flagged
        open_ = self.cat(**{"big-buck-bunny": {"not_hero": None}})
        self.assertIn("big-buck-bunny", tool.hero_eligible(open_))
        rec = next(m for m in open_["movies"] if m["id"] == "big-buck-bunny")
        self.assertTrue(tool.hero_gap(self.assets, open_, rec))  # eligible, no subject: a gap
        cat = self.cat(**{"big-buck-bunny": {"not_hero": True}})
        self.assertTrue(tool.check(self.assets, cat))
        self.assertNotIn("big-buck-bunny", tool.hero_eligible(cat))
        rec = next(m for m in cat["movies"] if m["id"] == "big-buck-bunny")
        self.assertEqual(tool.hero_gap(self.assets, cat, rec), [])

    def test_not_hero_is_true_or_absent(self):
        for bad in (False, "yes", 1):
            with self.subTest(bad=bad):
                self.assertIn("not_hero", self.refused(self.cat(**{"big-buck-bunny": {"not_hero": bad}})))

    def test_not_hero_is_refused_wherever_the_apps_hero_pool_could_take_the_title(self):
        slots = tool.hero_pool(self.catalog)
        for why, key in (("the pinned hero", self.catalog["hero"]),
                         ("hero_alternatives", self.catalog["hero_alternatives"][0]),
                         ("the hero pool's slots", slots[2]), ("the hero pool's slots", slots[-1])):
            with self.subTest(why=why):
                msg = self.refused(self.cat(**{key: {"not_hero": True}}))
                self.assertIn(key, msg)
                self.assertIn("not_hero", msg)

    def test_the_hero_pool_follows_the_apps_rule(self):
        def film(i, art=True):
            return dict({"id": i}, **({"art": {"asset": "a"}} if art else {}))
        cat = {"movies": [film("a"), film("b", art=False), film("c"), film("d"), film("e"), film("f"),
                          film("g"), film("h"), film("i"), film("j")],
               "shows": [{"id": "s", "art": {"asset": "a"}, "seasons": []}],
               "hero": "e", "hero_alternatives": [], "continue_watching": [
                   {"item": "a", "progress": 0.1}, {"item": "s/1/2", "progress": 0.1}, {"item": "b", "progress": 0.1}],
               "added_order": ["j", "i", "h", "g", "f", "e", "d", "c", "b", "a", "s"]}
        # the hero heads the deck (added when it is not in it), an episode and a show have no art the
        # app reads, a movie with none is skipped, a repeat is dropped, and the pool stops at HERO_MAX
        self.assertEqual(tool.hero_pool(cat), ["e", "a", "j", "i", "h", "g", "f", "d"])
        self.assertEqual(tool.hero_pool(cat, hero="a")[:3], ["a", "j", "i"])

    def test_the_pools_size_is_pinned_to_the_rust_constant(self):
        tool.check_hero_pins()
        with tempfile.TemporaryDirectory() as d:
            for pin in (*tool.HERO_PINS.values(), *tool.HERO_POOL_PINS.values()):
                dst = pathlib.Path(d) / pin[0]
                if not dst.exists():
                    dst.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy(ROOT / pin[0], dst)
            tool.check_hero_pins(root=d)
            pms = pathlib.Path(d) / "rust-modules" / "data" / "src" / "pms.rs"
            pms.write_text(pms.read_text().replace("const HERO_MAX: usize = 8;", "const HERO_MAX: usize = 6;"))
            with self.assertRaises(AssertionError) as e:
                tool.check_hero_pins(root=d)
            self.assertIn("HERO_MAX", str(e.exception))

    # ---- not_in_video ------------------------------------------------------------------------

    def test_not_in_video_is_true_or_absent_and_never_on_the_pinned_hero_or_its_alternatives(self):
        self.assertTrue(tool.check(self.assets, self.cat(**{"big-buck-bunny": {"not_in_video": True}})))
        self.assertIn("not_in_video", self.refused(self.cat(**{"big-buck-bunny": {"not_in_video": "no"}})))
        for key in (self.catalog["hero"], self.catalog["hero_alternatives"][0]):
            with self.subTest(key=key):
                self.assertIn(key, self.refused(self.cat(**{key: {"not_in_video": True}})))

    def test_a_title_without_a_logo_by_the_owners_exemption_is_never_opened_in_the_video(self):
        cat = self.cat(**{"logo_none_approved": [*self.catalog.get("logo_none_approved", []), "big-buck-bunny"],
                          "big-buck-bunny": {"not_in_video": None}})
        self.assertIn("big-buck-bunny", self.refused(cat))
        cat = self.cat(**{"logo_none_approved": [*self.catalog.get("logo_none_approved", []), "big-buck-bunny"],
                          "big-buck-bunny": {"not_in_video": True}})
        self.assertTrue(tool.check(self.assets, cat))
        self.assertIn("no-such-title", self.refused(self.cat(logo_none_approved=["no-such-title"])))

    # ---- a still as the poster ---------------------------------------------------------------

    STILL = {"asset": "bs-wing-it-header", "still_as_poster": True}

    def _gaps(self, key, cat):
        return tool.complete_gaps(self.assets, cat).get(key, [])

    def test_a_backdrop_as_the_poster_is_a_gap_until_it_is_declared_and_approved(self):
        bare = {"asset": "bs-wing-it-header"}
        self.assertIn("poster", self._gaps("wing-it", self.cat(**{"wing-it": {"poster": bare}})))
        both = self.cat(**{"wing-it": {"poster": self.STILL}, "still_poster_approved": self.catalog["still_poster_approved"]})
        self.assertTrue(tool.check(self.assets, both))
        self.assertNotIn("poster", self._gaps("wing-it", both))

    def test_a_declared_still_poster_needs_the_owners_approval_and_the_approval_needs_the_declaration(self):
        approved = self.catalog["still_poster_approved"]
        msg = self.refused(self.cat(**{"wing-it": {"poster": self.STILL},
                                       "still_poster_approved": [k for k in approved if k != "wing-it"]}))
        self.assertIn("still_poster_approved", msg)
        msg = self.refused(self.cat(**{"wing-it": {"poster": {"asset": "bs-wing-it-header"}},
                                       "still_poster_approved": approved}))
        self.assertIn("still_as_poster", msg)
        self.assertIn("still_as_poster", self.refused(self.cat(
            **{"wing-it": {"poster": dict(self.STILL, still_as_poster=False)}, "still_poster_approved": approved})))

    def test_a_real_poster_needs_no_flag_and_the_flag_names_a_still_or_a_backdrop(self):
        poster = {"asset": "sintel-poster", "still_as_poster": True}
        msg = self.refused(self.cat(sintel={"poster": poster}, still_poster_approved=["sintel"]))
        self.assertIn("still_as_poster", msg)

    # ---- the logo-title-card asset kind ------------------------------------------------------

    def card(self, **fields):
        base = dict(self.assets["sintel-poster"], kind="logo-title-card", of="the-general", origin="official-title-card")
        base.update(fields)
        return {k: v for k, v in base.items() if v is not None}

    def with_card(self, title="the-general", **fields):
        assets = dict(self.assets, **{"the-general-card": self.card(**fields)})
        cat = self.cat(**{title: {"logo": {"asset": "the-general-card"}}})
        return assets, cat

    def test_a_title_card_is_the_logo_source_of_its_own_title_and_satisfies_the_logo_gap(self):
        assets, cat = self.with_card()
        self.assertTrue(tool.check(assets, cat))
        self.assertNotIn("logo", tool.complete_gaps(assets, cat).get("the-general", []))
        self.assertIn("logo", tool.complete_gaps(self.assets, self.catalog).get("the-general", ["logo"]))

    def test_a_title_card_of_another_film_is_refused_as_a_logo(self):
        assets, cat = self.with_card(title="sintel")  # the card says it is of the-general
        self.assertIn("own card", self.refused(cat, assets))

    def test_a_title_card_records_what_it_is_of_and_where_it_comes_from(self):
        for field, bad in (("of", None), ("of", "no-such-film"), ("origin", None), ("origin", "fan-art")):
            with self.subTest(field=field, bad=bad):
                assets, cat = self.with_card(**{field: bad})
                self.refused(cat, assets)
        for origin in ("official-title-card", "film-frame"):
            assets, cat = self.with_card(origin=origin)
            self.assertTrue(tool.check(assets, cat))

    def test_a_title_card_is_a_source_for_a_logo_and_nothing_else(self):
        assets, _ = self.with_card()
        for role, recipe in (("poster", {"asset": "the-general-card"}),
                             ("art", {"asset": "the-general-card", "mode": "cover"})):
            with self.subTest(role=role):
                cat = self.cat(**{"the-general": {role: recipe}})
                self.assertIn("title card", self.refused(cat, assets))

    def test_a_title_cards_licence_rules_are_those_of_every_asset(self):
        for licence in ("CC BY-SA 4.0", "CC BY-NC 4.0", "CC BY 2.0"):
            with self.subTest(licence=licence):
                assets, cat = self.with_card(licence=licence)
                try:
                    tool.check(assets, cat)
                except AssertionError:
                    continue  # refused by `check` itself (share-alike, non-commercial)
                with self.assertRaises(AssertionError):
                    tool.check_assets_complete(assets)
        assets, cat = self.with_card(licence="CC BY 4.0")
        tool.check_assets_complete(assets)
        for field in ("url", "sha256", "licence", "author", "attribution", "source_page"):
            with self.subTest(field=field):
                assets, cat = self.with_card(**{field: ""})
                self.refused(cat, assets)

    def test_a_title_card_is_credited_as_the_films_own_title_card(self):
        assets, cat = self.with_card()
        with tempfile.TemporaryDirectory() as d, contextlib.redirect_stdout(io.StringIO()):
            tool.credits(assets, cat, dst=pathlib.Path(d) / "CREDITS.md")
            text = (pathlib.Path(d) / "CREDITS.md").read_text()
            self.assertIn("clear logo, the film's own title card", text)
            works = tool.site_works(assets, cat)
        entry = next(w for w in works if w["id"] == "the-general")["entries"]
        self.assertTrue(any("title card" in " ".join(e["changes"]) for e in entry))

    @unittest.skipUnless(importlib.util.find_spec("PIL"), "Pillow absent")
    def test_a_title_card_on_a_transparent_ground_is_trimmed_to_its_ink(self):
        from PIL import Image
        with tempfile.TemporaryDirectory() as d:
            src, dst = pathlib.Path(d) / "card.png", pathlib.Path(d) / "logo.png"
            card = Image.new("RGBA", (40, 30), (0, 0, 0, 0))
            card.paste((200, 10, 10, 255), (8, 12, 20, 18))  # a 12x6 patch of ink
            card.save(src)
            tool.derive_logo({"card": src}, dst, {"asset": "card"})
            out = Image.open(dst)
            self.assertEqual((out.mode, out.size), ("RGBA", (12, 6)))
            self.assertEqual(out.getpixel((0, 0)), (200, 10, 10, 255))
            Image.new("RGB", (40, 30), (9, 9, 9)).save(src)  # no transparent ground: refused, not guessed
            with self.assertRaises(SystemExit):
                tool.derive_logo({"card": src}, dst, {"asset": "card"})


    @unittest.skipUnless(importlib.util.find_spec("PIL"), "Pillow absent")
    def test_a_title_card_with_keys_is_keyed_to_its_lettering_and_trimmed(self):
        # A film frame (white lettering on a black ground, no alpha) is cut with the same keys a
        # poster is: the ground goes transparent, the lettering takes `fill`, the result is trimmed.
        from PIL import Image
        with tempfile.TemporaryDirectory() as d:
            src, dst = pathlib.Path(d) / "frame.png", pathlib.Path(d) / "logo.png"
            frame = Image.new("RGB", (60, 40), (10, 10, 10))
            frame.paste((250, 250, 250), (20, 15, 40, 25))  # a 20x10 patch of lettering
            frame.paste((90, 90, 90), (2, 2, 6, 6))  # a grey speck the key must drop
            frame.save(src)
            recipe = {"asset": "frame", "box": [0, 0, 60, 40], "scale": 1, "keys": [
                {"signal": "light", "levels": [150, 200], "fill": "#f2f2f2", "smooth": 3}]}
            tool.derive_logo({"frame": src}, dst, recipe)
            out = Image.open(dst)
            self.assertEqual((out.mode, out.size), ("RGBA", (20, 10)))
            self.assertEqual(out.getpixel((10, 5)), (242, 242, 242, 255))
            self.assertEqual(tool.derive_logo({"frame": src}, dst, recipe), None)  # deterministic
            first = dst.read_bytes()
            tool.derive_logo({"frame": src}, dst, recipe)
            self.assertEqual(dst.read_bytes(), first)


class OwnerDecisions(unittest.TestCase):
    """The owner's 2026-10-08 art decisions, held to the committed catalog (not to a synthetic one)."""

    def setUp(self):
        self.assets, self.catalog = tool.load()
        self.films = {t["id"]: t for t in self.catalog["movies"] + self.catalog["shows"]}

    def test_these_titles_are_never_a_hero(self):
        for key in ("his-girl-friday", "sherlock-jr", "the-kid", "plan-9-from-outer-space", "a-trip-to-the-moon",
                    "the-cabinet-of-dr-caligari"):
            self.assertTrue(self.films[key].get("not_hero"), key)
            self.assertNotIn(key, tool.hero_eligible(self.catalog))
            self.assertNotIn(key, tool.hero_pool(self.catalog))

    def test_these_titles_are_hero_art_and_pass_the_geometry_without_extend(self):
        for key in ("metropolis", "the-general", "safety-last", "the-daily-dweebs", "charge", "cosmos-laundromat",
                    "hero", "singularity", "spring"):
            with self.subTest(key=key):
                self.assertIn(key, tool.hero_eligible(self.catalog))
                v = tool.hero_geometry(self.assets, self.films[key])
                self.assertEqual((v["verdict"], v["reasons"]), ("pass", []), v)
                self.assertEqual(self.films[key]["art"]["mode"], "cover")

    def test_a_title_with_no_clear_logo_is_so_by_decision_and_is_never_opened_in_the_video(self):
        # Hubblecast and Nosferatu from the first decision; the four others (2026-10-08) are the titles
        # whose lettering could not be cut cleanly from their poster or a title frame: no ugly logo ships.
        none = ["hubblecast", "metropolis", "nosferatu", "sita-sings-the-blues", "the-cabinet-of-dr-caligari",
                "the-general"]
        self.assertEqual(sorted(self.catalog["logo_none_approved"]), none)
        for key in none:
            self.assertTrue(self.films[key].get("not_in_video"), key)
            self.assertNotIn("logo", tool.complete_gaps(self.assets, self.catalog).get(key, []))

    def test_a_still_serves_as_the_poster_only_where_the_owner_approved_it(self):
        self.assertEqual(sorted(self.catalog["still_poster_approved"]),
                         ["caminandes", "glass-half", "hero", "hubblecast", "singularity", "sita-sings-the-blues",
                          "wing-it"])
        for key in self.catalog["still_poster_approved"]:
            self.assertTrue(self.films[key]["poster"]["still_as_poster"], key)
        flagged = {k for k, t in self.films.items() if t.get("poster", {}).get("still_as_poster")}
        self.assertEqual(flagged, set(self.catalog["still_poster_approved"]))

    def test_a_video_frame_backdrop_is_a_stable_url_the_fetch_can_repeat(self):
        for key in ("night-of-the-living-dead-1968-frame", "the-general-1926-frame", "sherlock-jr-1924-frame",
                    "the-kid-1921-frame"):
            url = self.assets[key]["url"]
            self.assertRegex(url, r"/thumb/.*\.webm/1280px-seek%3D\d+-.*\.webm\.jpg$", key)
            self.assertEqual(self.assets[key]["licence"], "Public domain")

    def test_a_production_photograph_is_said_not_to_be_a_film_frame(self):
        for key in ("metropolis-maschinenmensch-still", "a-trip-to-the-moon-1902-still",
                    "plan-9-from-outer-space-1957-still"):
            self.assertIn("not a frame", self.assets[key]["notes"], key)


@unittest.skipUnless(importlib.util.find_spec("PIL"), "Pillow absent (`python3 -m pip install Pillow`)")
class AssetDimensions(unittest.TestCase):
    """`fetch` checks each source's declared width and height against the file itself."""

    def _png(self, d, w, h):
        from PIL import Image
        path = pathlib.Path(d) / "src" / "pic.png"
        path.parent.mkdir(parents=True)
        Image.new("RGB", (w, h), (12, 34, 56)).save(path)
        return {"pic": {"url": "https://example.invalid/pic.png", "sha256": tool.sha256(path),
                        "bytes": path.stat().st_size, "kind": "still", "width": w, "height": h}}

    def _fetch(self, d, assets):
        with unittest.mock.patch.object(tool, "cache_dir", return_value=pathlib.Path(d)), \
                unittest.mock.patch.object(tool.urllib.request, "urlopen", side_effect=OSError("offline")), \
                contextlib.redirect_stdout(io.StringIO()):
            return tool.fetch(assets)

    def test_a_matching_size_passes_without_the_network(self):
        with tempfile.TemporaryDirectory() as d:
            assets = self._png(d, 7, 5)
            self.assertEqual(list(self._fetch(d, assets)), ["pic"])

    def test_a_wrong_width_or_height_is_refused(self):
        for field in ("width", "height"):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as d:
                assets = self._png(d, 7, 5)
                assets["pic"][field] += 1
                with self.assertRaises(SystemExit) as raised:
                    self._fetch(d, assets)
                self.assertIn("pic", str(raised.exception))

    def test_the_committed_sizes_match_the_cached_files_when_present(self):
        assets, _ = tool.load()
        cached = {aid: a for aid, a in assets.items() if a["kind"] != "media" and tool.source_path(aid, a).exists()}
        if not cached:
            self.skipTest("no source cache; `make demo-library` fetches it")
        from PIL import Image
        for aid, a in cached.items():
            with self.subTest(asset=aid), Image.open(tool.source_path(aid, a)) as im:
                self.assertEqual(im.size, (a["width"], a["height"]))


class Credits(unittest.TestCase):
    """CREDITS.md is written by the same command that writes the images, so it cannot lag them."""

    def test_the_committed_credits_are_what_the_manifests_say(self):
        assets, catalog = tool.load()
        with tempfile.TemporaryDirectory() as d:
            dst = pathlib.Path(d) / "CREDITS.md"
            tool.credits(assets, catalog, dst)
            self.assertEqual(dst.read_text(), tool.CREDITS.read_text(),
                             "docs/screenshots/CREDITS.md is stale: run `make screenshots`")

    def test_the_committed_site_credits_page_is_what_the_manifests_say(self):
        assets, catalog = tool.load()
        with tempfile.TemporaryDirectory() as d:
            dst = pathlib.Path(d) / "credits.html"
            tool.site_credits(assets, catalog, dst)
            self.assertEqual(dst.read_text(), tool.SITE_CREDITS.read_text(),
                             "site/credits.html is stale: run `python3 tools/demo_library.py site-credits`")

    def test_the_site_credits_page_credits_every_asset_and_links_its_licence(self):
        assets, catalog = tool.load()
        page = tool.SITE_CREDITS.read_text()
        for aid, a in assets.items():
            self.assertIn(f'href="{a["source_page"]}"', page, f"{aid}: no source link on the credits page")
        for name, url in tool.LICENCE_TEXTS.items():
            if any(a["licence"] == name for a in assets.values()):
                self.assertIn(f'<a href="{url}" rel="license">{name}</a>', page)
        for m in catalog["movies"] + catalog["shows"]:
            self.assertIn(f'id="{m["id"]}"', page)

    def test_the_landing_page_footer_links_the_credits(self):
        self.assertIn('href="credits.html"', (ROOT / "site" / "index.html").read_text())
        self.assertIn("cp site/credits.html _site/credits.html",
                      (ROOT / ".github" / "workflows" / "pages.yml").read_text())

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
        # The application AND every layer crate (`rust-modules/<layer>/src`): a trigger is read where
        # its owner lives, and the media layer (`simvideo`, `playurl`, ...) is one of them.
        roots = [RUST, *sorted((ROOT / "rust-modules").glob("*/src"))]
        for path in (p for root in roots for p in root.rglob("*.rs")):
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
        self.assertRegex(self.lib.media_files[part["id"]].name, r"^stand-in-[0-9a-f]{12}\.mp4$")

    def test_an_unknown_hero_is_refused(self):
        with self.assertRaises(ValueError):
            mock_pms.CatalogLibrary(CATALOG, hero="no-such-film")

    def test_a_title_marked_not_hero_cannot_be_pinned_as_the_hero(self):
        flagged = [t["id"] for t in self.catalog["movies"] + self.catalog["shows"] if t.get("not_hero")]
        self.assertTrue(flagged)
        for key in flagged:
            with self.subTest(hero=key), self.assertRaises(ValueError) as e:
                mock_pms.CatalogLibrary(CATALOG, hero=key)
            self.assertIn("not_hero", str(e.exception))

    def home_pool(self, lib, pms):
        """The titles the app's hero pool takes from the mock's Home response: `merge` in
        `data/src/pms.rs`, restated from what the hubs actually carry."""
        slug_of = {str(rk): slug for slug, rk in lib.by_slug.items() if "/" not in slug}
        status, _, data = pms.handle("GET", "/hubs?count=12")
        self.assertEqual(status, 200)
        pool = []
        for hub in json.loads(data)["MediaContainer"]["Hub"]:
            if not (hub["hubIdentifier"] == "home.continue" or "recent" in hub["hubIdentifier"]):
                continue
            for row in hub["Metadata"]:
                if "art" not in row or row["type"] == "season":
                    continue  # the app needs landscape art, and skips a season
                title = slug_of[row.get("grandparentRatingKey", row["ratingKey"])]
                if title not in pool:
                    pool.append(title)
        return pool[:int(tool.HERO_POOL_PINS["HERO_MAX"][3])]

    def test_the_hero_pool_the_check_rebuilds_is_the_one_the_mocks_home_hands_the_app(self):
        for hero in (None, *self.catalog["hero_alternatives"]):
            with self.subTest(hero=hero):
                lib = mock_pms.CatalogLibrary(CATALOG, hero=hero)
                pool = self.home_pool(lib, mock_pms.MockPms(lib))
                self.assertEqual(pool, tool.hero_pool(self.catalog, hero=hero))
                flagged = {t["id"] for t in self.catalog["movies"] + self.catalog["shows"] if t.get("not_hero")}
                self.assertFalse(flagged & set(pool), "a not_hero title is in the mock's hero pool")

    def test_home_hubs_are_titled_like_a_real_server(self):
        hubs = self.get("/hubs?count=12")["Hub"]
        self.assertEqual([h["title"] for h in hubs],
                         ["Continue Watching", *(h["title"] for h in self.catalog["hubs"])])
        self.assertTrue(all(h["Metadata"] for h in hubs))
        for key, rest in (("1", ["Recently Added Movies", *self.catalog["collections"]]),
                          ("2", ["Recently Added TV"])):
            titles = [h["title"] for h in self.get(f"/hubs/sections/{key}")["Hub"]]
            self.assertEqual(titles, ["Continue Watching", *rest])

    def test_a_library_lists_its_collections_as_shelves_after_recently_added(self):
        hubs = self.get("/hubs/sections/1")["Hub"][2:]
        self.assertTrue(hubs)
        pinned = self.catalog.get("collection_order", {})
        for h in hubs:
            self.assertRegex(h["hubIdentifier"], r"^custom\.collection\.1\.(\d+)\.\1$")
            self.assertTrue(all(m["librarySectionID"] == 1 for m in h["Metadata"]))
            if h["title"] in pinned:
                # a custom order is served as pinned (the website's glass close-up depends on it)
                want = [str(self.lib.by_slug[s]) for s in pinned[h["title"]]][:len(h["Metadata"])]
                self.assertEqual([m["ratingKey"] for m in h["Metadata"]], want, h["title"])
            else:
                added = [m["addedAt"] for m in h["Metadata"]]
                self.assertEqual(added, sorted(added, reverse=True), h["title"])
        self.assertIn("Blender Open Movies", pinned)
        self.assertTrue(any(h["title"] not in pinned for h in hubs), "one collection keeps the default")

    def test_a_collection_composite_is_a_poster_and_an_empty_collection_has_none(self):
        """The server's automatic composite (a 2x2 of members' posters) serves as JPEG at the
        requested size; the empty collection sends no thumb, so the client draws its neutral tile."""
        rows = self.get("/library/sections/1/all?type=18")["Metadata"]
        with_art = [r for r in rows if r.get("thumb")]
        self.assertTrue(with_art and any(not r.get("thumb") for r in rows))
        url = urllib.parse.quote(with_art[0]["thumb"], safe="")
        status, ctype, data = self.pms.handle("GET", f"/photo/:/transcode?width=250&height=375&minSize=1&url={url}")
        self.assertEqual((status, ctype), (200, "image/jpeg"))
        self.assertTrue(data.startswith(b"\xff\xd8"))

    def test_check_refuses_a_collection_order_that_is_not_the_whole_collection(self):
        assets, catalog = tool.load()
        order = dict(catalog["collection_order"])
        order["Blender Open Movies"] = order["Blender Open Movies"][:-1]
        with self.assertRaises(AssertionError):
            tool.check(assets, dict(catalog, collection_order=order))

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
        self.assertEqual(self.search("hubble")["show"], ["Hubblecast"])
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
