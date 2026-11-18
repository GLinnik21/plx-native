"""Library fixture contracts: the mock must exercise the real rail and sparse-page boundaries."""
import calendar
import contextlib
import datetime
import io
import json
import hashlib
import os
import pathlib
import re
import shutil
import socket
import struct
import subprocess
import tempfile
import time
import unittest
import urllib.parse
import urllib.request
import zlib

from mock_pms import (
    ENHANCEMENT_AAC_51_RK, ENHANCEMENT_AC3_2CH_RK, ENHANCEMENT_DEFAULT_SRT_RK,
    ENHANCEMENT_DV_P8_RK, ENHANCEMENT_EXTERNAL_SRT_RK, EXTRA_MEDIA_RK_BASE, PLEX_DIRECT_HASH,
    CAPTION_EDGE_RK_BASE, RESUME_ON_DECK_SHOW_RK, RESUME_PLAIN_SHOW_RK,
    CatalogLibrary, Library, MockPms, add_cw_art_fixture, demo_cache_dir, plaintext_only_lan_resources, serve,
)


HAS_FFMPEG_AND_FFPROBE = shutil.which("ffmpeg") and shutil.which("ffprobe")
HAS_DEMO_CACHE = bool(HAS_FFMPEG_AND_FFPROBE) and (demo_cache_dir() / "derived").is_dir()
CATALOG = pathlib.Path(__file__).resolve().parent / "demo_library" / "catalog.json"
FIXTURES = pathlib.Path(__file__).resolve().parent / "demo_library" / "fixtures"


@contextlib.contextmanager
def local_zone(name):
    """Run with the process's local time zone set to `name` (a platform without `tzset` skips)."""
    if not hasattr(time, "tzset"):
        raise unittest.SkipTest("no time.tzset on this platform")
    before = os.environ.get("TZ")
    os.environ["TZ"] = name
    time.tzset()
    try:
        yield
    finally:
        if before is None:
            del os.environ["TZ"]
        else:
            os.environ["TZ"] = before
        time.tzset()


def get(pms, path):
    status, content_type, data = pms.handle("GET", path)
    assert status == 200 and content_type == "application/json"
    return json.loads(data)["MediaContainer"]


class LibraryRail(unittest.TestCase):
    def test_catalog_recent_hub_pages_through_its_provider_key(self):
        lib = object.__new__(CatalogLibrary)
        lib.__dict__.update(Library(movies=60).__dict__)
        lib.catalog = {"hubs": [{"hubIdentifier": "home.movies.recent", "recent": "movie"}]}
        pms = MockPms(lib)
        response = get(pms, "/hubs/demo/home.movies.recent"
                           "?X-Plex-Container-Start=36&X-Plex-Container-Size=24")
        self.assertEqual(response["offset"], 36)
        self.assertEqual(response["Metadata"], lib.recent("movie", 1000)[36:60])
        self.assertEqual(response["totalSize"], len(lib.recent("movie", 1000)))

    def test_collection_ids_and_collection_routes_match_pms(self):
        pms = MockPms(Library())
        collections = get(pms, "/library/sections/1/collections")["Metadata"]
        self.assertTrue(collections)
        row = collections[0]
        self.assertNotEqual(row["index"], int(row["ratingKey"]))
        own = get(pms, f"/library/metadata/{row['ratingKey']}")["Metadata"]
        self.assertEqual(own, [row])
        children = get(pms, f"/library/collections/{row['ratingKey']}/children"
                            "?X-Plex-Container-Start=0&X-Plex-Container-Size=2")
        self.assertLessEqual(len(children["Metadata"]), 2)
        self.assertEqual(children["offset"], 0)
        via_all = get(pms, "/library/sections/1/all?type=18&sort=titleSort:asc"
                           "&X-Plex-Container-Start=0&X-Plex-Container-Size=2")
        self.assertEqual(via_all["Metadata"], sorted(collections, key=lambda c: c["titleSort"])[:2])
        status, ctype, body = pms.handle(
            "GET", f"/library/collections/{row['ratingKey']}/children", headers={
                "X-Plex-Container-Start": "0", "X-Plex-Container-Size": "1",
            })
        self.assertEqual((status, ctype), (200, "application/json"))
        self.assertLessEqual(len(json.loads(body)["MediaContainer"]["Metadata"]), 1)
        self.assertTrue(any(c.get("childCount") == 0 for c in collections),
                        "the fixture includes an empty collection")

    def test_collection_listing_matches_the_library_type_menu_contract(self):
        """The Library's Collections type: `all?type=18&includeMeta=1` declares only titleSort,
        `firstCharacter?type=18` counts collections, and only a collection with members has art."""
        pms = MockPms(Library())
        first = get(pms, "/library/sections/1/all?type=18&includeMeta=1"
                         "&X-Plex-Container-Start=0&X-Plex-Container-Size=60")
        kinds = first["Meta"]["Type"]
        self.assertEqual([k["type"] for k in kinds], ["collection"])
        self.assertEqual([s["key"] for s in kinds[0]["Sort"]], ["titleSort"])
        self.assertEqual(first["totalSize"], len(first["Metadata"]))
        letters = get(pms, "/library/sections/1/firstCharacter?type=18")["Directory"]
        self.assertEqual(sum(d["size"] for d in letters), first["totalSize"])
        self.assertEqual([d["title"] for d in letters],
                         sorted({row["titleSort"][0].upper() for row in first["Metadata"]}),
                         "the rail's letters come in the listing's titleSort order")
        for row in first["Metadata"]:
            self.assertEqual("thumb" in row, row["childCount"] > 0, row["title"])

    def test_collections_count_probe_reads_total_size_of_a_zero_size_type_18_window(self):
        """The header's "Collections · N": `all?type=18` with `Size=0` returns no rows and the
        section's collection count as `totalSize` (docs/pms-api.md section 2b)."""
        pms = MockPms(Library())
        listed = get(pms, "/library/sections/1/collections")["Metadata"]
        probe = get(pms, "/library/sections/1/all?type=18"
                         "&X-Plex-Container-Start=0&X-Plex-Container-Size=0")
        self.assertEqual(probe.get("Metadata", []), [])
        self.assertEqual(probe["totalSize"], len(listed))
        self.assertGreater(probe["totalSize"], 0)
        shows = get(pms, "/library/sections/2/all?type=18"
                         "&X-Plex-Container-Start=0&X-Plex-Container-Size=0")
        self.assertEqual(shows["totalSize"], 0, "a section with no collections counts zero")

    def test_a_smart_collection_is_opt_in_and_listed_with_smart_and_a_count(self):
        plain = get(MockPms(Library()), "/library/sections/1/collections")["Metadata"]
        self.assertTrue(all(str(r["smart"]) == "0" for r in plain), "the default data has none")
        lib = Library()
        lib.smart_collections = True
        pms = MockPms(lib)
        rows = get(pms, "/library/sections/1/collections")["Metadata"]
        smart = [r for r in rows if str(r["smart"]) == "1"]
        self.assertEqual(len(smart), 1)
        self.assertEqual(smart[0]["subtype"], "movie")
        self.assertGreater(smart[0]["childCount"], 0)
        self.assertEqual(len(rows), len(plain) + 1)
        probe = get(pms, "/library/sections/1/all?type=18"
                         "&X-Plex-Container-Start=0&X-Plex-Container-Size=0")
        self.assertEqual(probe["totalSize"], len(plain) + 1)

    def test_search_with_include_collections_answers_full_collection_rows(self):
        pms = MockPms(Library())
        listed = get(pms, "/library/sections/1/collections")["Metadata"]
        name = listed[0]["title"]
        query = urllib.parse.quote(name.split()[0])

        def hub(path):
            return next(h for h in get(pms, path)["Hub"] if h["hubIdentifier"] == "collection")

        # without the flag: tag rows under Directory — no ratingKey, no thumb
        tags = hub(f"/hubs/search?query={query}&limit=12")
        self.assertNotIn("Metadata", tags)
        self.assertTrue(all("ratingKey" not in t and "thumb" not in t for t in tags["Directory"]))
        # with it: the collection's own full rows under Metadata, both ids intact
        rows = hub(f"/hubs/search?query={query}&limit=12&includeCollections=1")
        self.assertNotIn("Directory", rows)
        self.assertEqual(rows["size"], len(rows["Metadata"]))
        row = next(r for r in rows["Metadata"] if r["title"] == name)
        self.assertEqual({k: v for k, v in row.items() if k != "score"}, listed[0])
        self.assertEqual(row["type"], "collection")
        self.assertNotEqual(row["index"], int(row["ratingKey"]))
        self.assertTrue(row["thumb"])
        # every other hub is unchanged by the flag
        plain = [h for h in get(pms, f"/hubs/search?query={query}&limit=12")["Hub"]
                 if h["hubIdentifier"] != "collection"]
        flagged = [h for h in get(pms, f"/hubs/search?query={query}&limit=12&includeCollections=1")["Hub"]
                   if h["hubIdentifier"] != "collection"]
        self.assertEqual(plain, flagged)

    def test_a_member_movies_related_carries_its_whole_collection(self):
        lib = Library()
        pms = MockPms(lib)
        member = next(it for it in lib.items.values()
                      if it["type"] == "movie" and it.get("Collection"))
        tag = member["Collection"][0]
        hubs = get(pms, f"/library/metadata/{member['ratingKey']}/related")["Hub"]
        own = [h for h in hubs if h["hubIdentifier"].startswith("collection.related.")]
        self.assertEqual(len(own), len(member["Collection"]))
        hub = own[0]
        self.assertEqual(hub["title"], tag["tag"])
        self.assertIn(f"tagId={tag['id']}", hub["key"])
        self.assertTrue(hub["key"].startswith(f"/library/sections/{member['librarySectionID']}/all?"))
        keys = [m["ratingKey"] for m in hub["Metadata"]]
        self.assertIn(member["ratingKey"], keys, "the member lists itself")
        self.assertEqual(keys, [m["ratingKey"] for m in lib.collection_rows(tag["id"])])
        loner = next(it for it in lib.items.values()
                     if it["type"] == "movie" and not it.get("Collection"))
        hubs = get(pms, f"/library/metadata/{loner['ratingKey']}/related")["Hub"]
        self.assertFalse(any(h["hubIdentifier"].startswith("collection.related") for h in hubs))

    def test_default_generated_data_is_stable(self):
        # #266 added canNormalizeLoudness to every generated audio stream and five fixed
        # enhancement fixture movies (own section id, invisible to sections 1/2) to every
        # Library — both hashes moved when that landed.
        hashes = {
            1: "c900e01d9e26ead0c63f0e2cfbf5bfad1cf410ba9a047ca701f41806246e2564",
            7: "e59fb64f00c787449263070404aed8060e07bcf644ebcc3a2184626e416d6616",
        }
        for seed, expected in hashes.items():
            payload = json.dumps(Library(seed=seed).__dict__, sort_keys=True).encode()
            self.assertEqual(hashlib.sha256(payload).hexdigest(), expected)

    def test_default_letter_counts_match_the_actual_section(self):
        pms = MockPms(Library())
        for section, count in ((1, 48), (2, 6)):
            with self.subTest(section=section):
                letters = get(pms, f"/library/sections/{section}/firstCharacter")["Directory"]
                self.assertEqual(letters, [{"key": "s", "title": "S", "size": count}])

    def test_empty_section_has_no_letter_stops(self):
        pms = MockPms(Library(movies=0))
        self.assertEqual(get(pms, "/library/sections/1/firstCharacter")["Directory"], [])

    def test_rail_buckets_cover_real_page_boundaries_and_only_their_section(self):
        pms = MockPms(Library(movies=321, rail_fixture=True))
        rows = []
        for start in range(0, 321, 60):
            page = get(pms, f"/library/sections/1/all?X-Plex-Container-Start={start}&X-Plex-Container-Size=60")
            self.assertEqual(page["totalSize"], 321)
            self.assertEqual(page["offset"], start)
            self.assertEqual(page["size"], min(60, 321 - start))
            rows.extend(page["Metadata"])
        self.assertEqual(len({row["ratingKey"] for row in rows}), 321)
        self.assertEqual([r["titleSort"] for r in rows], sorted(r["titleSort"] for r in rows))
        self.assertTrue(all(re.fullmatch(r"s[0-9a-f]{8}", r["title"]) for r in rows))
        letters = get(pms, "/library/sections/1/firstCharacter")["Directory"]
        self.assertEqual([r["title"] for r in letters], list("ACFMZ"))
        self.assertEqual(sum(r["size"] for r in letters), 321)
        offset = 0
        for letter in letters:
            prefix = letter["title"] + " "
            size = letter["size"]
            self.assertGreater(size, 60)
            self.assertTrue(all(r["titleSort"].startswith(prefix) for r in rows[offset:offset + size]))
            first = get(pms, f"/library/sections/1/all?X-Plex-Container-Start={offset}&X-Plex-Container-Size=1")["Metadata"][0]
            self.assertEqual(first["ratingKey"], rows[offset]["ratingKey"])
            offset += size
        self.assertEqual(sum(r["size"] for r in get(pms, "/library/sections/2/firstCharacter")["Directory"]), 6)

    def test_count_cannot_overlap_the_show_key_namespace(self):
        for count in (-1, 1001):
            with self.assertRaises(ValueError):
                Library(movies=count)
        library = Library(movies=1000, rail_fixture=True)
        self.assertEqual(len(library.section_items("1", {})), 1000)
        self.assertEqual(len(library.section_items("2", {})), 6)

    def test_new_sort_titles_and_letter_labels_stay_in_the_closed_alphabet(self):
        alphabet = json.loads((pathlib.Path(__file__).parent / "fixtures/replay/ALPHABET.json").read_text())
        def allowed(text):
            return text in alphabet["literals"] or any(re.fullmatch(p, text) for p in alphabet["patterns"])
        lib = Library(rail_fixture=True)
        for section in ("1", "2"):
            for row in lib.section_items(section, {}):
                self.assertTrue(allowed(row["titleSort"]))
            for row in lib.first_characters(section):
                self.assertTrue(allowed(row["key"]))
                self.assertTrue(allowed(row["title"]))
        for rejected in ("A The Godfather", "A s123456789", "B s12345678", "A s12345678 token=secret"):
            self.assertFalse(allowed(rejected), rejected)

    def test_serve_exposes_opt_in_rail_data_over_the_real_http_handler(self):
        server, _ = serve(0, movies=121, rail_fixture=True)
        try:
            base = f"http://127.0.0.1:{server.server_address[1]}"
            with urllib.request.urlopen(base + "/library/sections/1/firstCharacter", timeout=5) as response:
                rows = json.load(response)["MediaContainer"]["Directory"]
            self.assertEqual([r["title"] for r in rows], list("ACFMZ"))
            self.assertEqual(sum(r["size"] for r in rows), 121)
            with urllib.request.urlopen(base + "/library/sections/1/all?X-Plex-Container-Start=120&X-Plex-Container-Size=60", timeout=5) as response:
                page = json.load(response)["MediaContainer"]
            self.assertEqual((page["offset"], page["size"], page["totalSize"]), (120, 1, 121))
        finally:
            server.shutdown()
            server.server_close()


class LetterRailFilters(unittest.TestCase):
    """`firstCharacter` takes the same terms as `/all` (docs/pms-api.md, "Letter index"): its counts
    describe the filtered listing, so the app's jump offsets (prefix sums) land on the right title."""

    def counts(self, pms, section, terms):
        query = f"?{terms}" if terms else ""
        return get(pms, f"/library/sections/{section}/firstCharacter{query}")["Directory"]

    def total(self, pms, section, terms):
        joined = (terms + "&" if terms else "")
        return get(pms, f"/library/sections/{section}/all?{joined}"
                        "X-Plex-Container-Start=0&X-Plex-Container-Size=0")["totalSize"]

    def assert_offsets_land_on_their_letter(self, pms, section, terms):
        """Sum == totalSize, and the item at each prefix-summed offset starts that letter."""
        letters = self.counts(pms, section, terms)
        self.assertEqual(sum(r["size"] for r in letters), self.total(pms, section, terms), terms)
        joined = (terms + "&" if terms else "")
        offset = 0
        for letter in letters:
            first = get(pms, f"/library/sections/{section}/all?{joined}sort=titleSort"
                             f"&X-Plex-Container-Start={offset}&X-Plex-Container-Size=1")["Metadata"][0]
            self.assertTrue(first["titleSort"].startswith(letter["title"] + " "), (terms, letter, offset))
            offset += letter["size"]
        return letters

    def test_counts_follow_unwatched_genre_and_both_for_movies(self):
        pms = MockPms(Library(movies=60, rail_fixture=True))
        genre = next(iter(pms.lib.genres.values()))["id"]
        plain = self.assert_offsets_land_on_their_letter(pms, 1, "")
        for terms in ("unwatched=1", f"genre={genre}", f"unwatched=1&genre={genre}"):
            with self.subTest(terms=terms):
                filtered = self.assert_offsets_land_on_their_letter(pms, 1, terms)
                self.assertLess(sum(r["size"] for r in filtered), sum(r["size"] for r in plain))
        by_letter = {r["title"]: r["size"] for r in self.counts(pms, 1, "unwatched=1")}
        self.assertTrue(any(by_letter.get(r["title"], 0) < r["size"] for r in plain),
                        "the fixture has watched titles, so some letter shrinks")

    def fully_watched_show_fixture(self):
        lib = Library(movies=0, rail_fixture=True)
        show = next(it for it in lib.items.values() if it["type"] == "show")
        for episode in lib.leaves(int(show["ratingKey"])):
            episode["viewCount"] = 1
        lib._roll_up()
        return MockPms(lib)

    def test_shows_are_scoped_by_type_and_unwatched_leaves(self):
        pms = self.fully_watched_show_fixture()
        everything = self.counts(pms, 2, "type=2")
        filtered = self.assert_offsets_land_on_their_letter(pms, 2, "type=2&unwatchedLeaves=1")
        self.assertLess(sum(r["size"] for r in filtered), sum(r["size"] for r in everything),
                        "the fixture has a fully watched show")

    def test_unwatched_is_not_unwatched_leaves_on_shows(self):
        """A real PMS answers the two keys differently for shows (docs/pms-api.md, "Unwatched":
        30 of 33 against 24), so the mock must not treat them as one predicate: an app that sent
        `unwatched` for a show listing would otherwise get the filtered rail and grid by luck.
        The Rust half is `the_letters_request_carries_the_terms_the_grid_sends`
        (`data/src/browse/listing_jobs_tests.rs`), which pins the key the app sends for shows; this
        pins that the mock can tell a wrong key from the right one. Only the DIVERGENCE is
        modelled (the row's own, never-set `viewCount`), not PMS's undocumented rule."""
        pms = self.fully_watched_show_fixture()
        everything = sum(r["size"] for r in self.counts(pms, 2, "type=2"))
        leaves = sum(r["size"] for r in self.counts(pms, 2, "type=2&unwatchedLeaves=1"))
        plain = sum(r["size"] for r in self.counts(pms, 2, "type=2&unwatched=1"))
        self.assertLess(leaves, everything)
        self.assertNotEqual(plain, leaves)
        self.assertEqual(self.total(pms, 2, "type=2&unwatched=1"), plain, "counts still sum to totalSize")

    def test_a_filter_that_matches_nothing_answers_an_empty_directory(self):
        pms = MockPms(Library(movies=60, rail_fixture=True))
        self.assertEqual(self.counts(pms, 1, "genre=999999"), [])
        self.assertEqual(self.total(pms, 1, "genre=999999"), 0)

    def test_collections_keep_their_own_counts(self):
        pms = MockPms(Library())
        with_unwatched = self.counts(pms, 1, "type=18&unwatched=1")
        self.assertEqual(with_unwatched, self.counts(pms, 1, "type=18"))


class ResumeFixture(unittest.TestCase):
    """`--resume-fixture`: a show that has been watched up to S12E14, and one that has not."""

    def test_the_on_deck_show_carries_its_episode_only_when_asked(self):
        pms = MockPms(Library(resume_fixture=True))
        show = get(pms, f"/library/metadata/{RESUME_ON_DECK_SHOW_RK}?includeOnDeck=1")["Metadata"][0]
        deck = show["OnDeck"]["Metadata"]
        self.assertEqual((deck["parentIndex"], deck["index"], deck["type"]), (12, 14, "episode"))
        self.assertGreater(deck["viewOffset"], 0)
        self.assertEqual(show["childCount"], 12)
        self.assertNotIn("OnDeck", get(pms, f"/library/metadata/{RESUME_ON_DECK_SHOW_RK}")["Metadata"][0])

    def test_the_other_show_has_no_on_deck_and_is_unwatched(self):
        pms = MockPms(Library(resume_fixture=True))
        show = get(pms, f"/library/metadata/{RESUME_PLAIN_SHOW_RK}?includeOnDeck=1")["Metadata"][0]
        self.assertNotIn("OnDeck", show)
        self.assertEqual(show["viewedLeafCount"], 0)

    def test_the_resume_episode_heads_continue_watching_and_a_season_pages_over_one_screen(self):
        pms = MockPms(Library(resume_fixture=True))
        deck = get(pms, "/hubs/continueWatching?count=12")["Hub"][0]["Metadata"]
        self.assertEqual(deck[0]["grandparentRatingKey"], str(RESUME_ON_DECK_SHOW_RK))
        seasons = get(pms, f"/library/metadata/{RESUME_ON_DECK_SHOW_RK}/children")["Metadata"]
        self.assertEqual([s["index"] for s in seasons], list(range(1, 13)))
        self.assertEqual(len(get(pms, f"/library/metadata/{seasons[11]['ratingKey']}/children")["Metadata"]), 30)

    def test_off_by_default_and_the_generated_library_does_not_move(self):
        plain, fixture = Library(), Library(resume_fixture=True)
        self.assertEqual(plain.on_deck, {})
        self.assertNotIn(RESUME_ON_DECK_SHOW_RK, plain.items)
        for rk, item in plain.items.items():
            if item["type"] in ("movie", "show", "season", "episode") and rk < 970000:
                self.assertEqual(item, {**fixture.items[rk],
                                        **{k: item[k] for k in ("leafCount", "viewedLeafCount", "childCount")
                                           if k in item}}, rk)


class CaptionEdgeCases(unittest.TestCase):
    """`--caption-edge-cases`: the items an under-poster caption falls back on, which no generated
    item is. Off by default, and then no generated byte moves."""

    def lib(self):
        return Library(caption_edge_cases=True)

    def test_a_movie_with_no_year_keeps_its_length_and_one_with_nothing_keeps_nothing(self):
        items = self.lib().items
        year_less, bare = items[CAPTION_EDGE_RK_BASE], items[CAPTION_EDGE_RK_BASE + 1]
        for it in (year_less, bare):
            self.assertEqual(it["type"], "movie")
            self.assertNotIn("year", it)
            self.assertNotIn("originallyAvailableAt", it)
        self.assertGreater(year_less["duration"], 0)
        self.assertTrue(year_less["Genre"])
        for key in ("duration", "Genre"):
            self.assertNotIn(key, bare)

    def test_the_edge_items_head_the_a_to_z_listings(self):
        pms = MockPms(self.lib())
        movies = get(pms, "/library/sections/1/all?sort=titleSort")["Metadata"]
        self.assertEqual([m["ratingKey"] for m in movies[:2]],
                         [str(CAPTION_EDGE_RK_BASE), str(CAPTION_EDGE_RK_BASE + 1)])
        shows = get(pms, "/library/sections/2/all?sort=titleSort")["Metadata"]
        self.assertEqual({s["ratingKey"] for s in shows[:2]},
                         {str(CAPTION_EDGE_RK_BASE + 2), str(CAPTION_EDGE_RK_BASE + 9)})

    def test_a_show_has_no_year_and_no_seasons(self):
        show = self.lib().items[CAPTION_EDGE_RK_BASE + 2]
        self.assertEqual((show["type"], show["childCount"]), ("show", 0))
        self.assertNotIn("year", show)

    def test_a_show_lists_an_untitled_season_zero_before_season_one_and_a_numberless_episode(self):
        pms = MockPms(self.lib())
        show_rk = CAPTION_EDGE_RK_BASE + 9
        seasons = get(pms, f"/library/metadata/{show_rk}/children")["Metadata"]
        self.assertEqual([(s["index"], s["title"]) for s in seasons][0], (0, ""))
        self.assertEqual([s["index"] for s in seasons], [0, 1])
        for season in seasons:
            eps = get(pms, f"/library/metadata/{season['ratingKey']}/children")["Metadata"]
            self.assertEqual([e.get("index") for e in eps], [None, 1], "the numberless one sorts first")
            self.assertNotIn("year", eps[0])
            self.assertNotIn("duration", eps[0])
        self.assertEqual(len(get(pms, f"/library/metadata/{show_rk}/allLeaves")["Metadata"]), 4)

    def test_two_collections_are_named_by_more_than_a_face_can_set(self):
        pms = MockPms(self.lib())
        rows = get(pms, "/library/sections/1/collections")["Metadata"]
        long = [r for r in rows if len(r["title"].split()) >= 5]
        self.assertEqual(len(long), 2)
        self.assertEqual(sorted("thumb" in r for r in long), [False, True], "a fan and a neutral tile")
        self.assertGreater(max(r["childCount"] for r in long), 0)

    def test_the_section_listings_still_answer_with_the_new_items(self):
        pms = MockPms(self.lib())
        for path in ("/library/sections/1/all?sort=year:desc", "/library/sections/1/all",
                     "/library/sections/2/all?sort=year", "/library/sections/2/all?type=4",
                     "/hubs?count=12", "/hubs/continueWatching?count=12"):
            with self.subTest(path=path):
                get(pms, path)

    def test_off_by_default_and_the_generated_library_does_not_move(self):
        plain, flagged = Library(), self.lib()
        self.assertNotIn(CAPTION_EDGE_RK_BASE, plain.items)
        self.assertEqual(len(plain.collections), 4)
        for rk, item in plain.items.items():
            kept = {k: v for k, v in flagged.items[rk].items()
                    if k not in ("Collection", "leafCount", "viewedLeafCount", "childCount")}
            self.assertEqual({k: v for k, v in item.items()
                              if k not in ("Collection", "leafCount", "viewedLeafCount", "childCount")},
                             kept, rk)


class BandQueries(unittest.TestCase):
    """The value-sort rail's count probes, answered as a real PMS 1.43.5 was measured to answer them
    (docs/pms-api.md, "Count probes at band edges"). Every expectation is computed from the library's
    own items, the way `tools/pms-band-probe.py` checks a server against its own listing."""
    COUNT_HEADERS = {"X-Plex-Container-Start": "0", "X-Plex-Container-Size": "0"}

    @classmethod
    def setUpClass(cls):
        cls.lib = Library(seed=3, movies=60, shows=15, band_fixture=True)
        cls.pms = MockPms(cls.lib)
        cls.movies = [it for it in cls.lib.items.values()
                      if it["type"] == "movie" and it["librarySectionID"] == 1]
        cls.shows = [it for it in cls.lib.items.values()
                     if it["type"] == "show" and it["librarySectionID"] == 2]

    @staticmethod
    def term(key, op, value):
        """One filter term spelled as the probe tool does: the key side URL-encoded."""
        text = ("%.4f" % value).rstrip("0").rstrip(".") if isinstance(value, float) else str(value)
        return urllib.parse.quote(key + op.rstrip("="), safe=".") + "=" + text

    @staticmethod
    def mine(it, field):
        """The number `field` holds on an item (None: no value), read off the item itself."""
        if field == "unviewedLeafCount":
            return it["leafCount"] - it["viewedLeafCount"]
        if field == "originallyAvailableAt":
            day = it.get(field)
            return None if day is None else calendar.timegm(datetime.date.fromisoformat(day).timetuple())
        return it.get(field)

    @staticmethod
    def passes(op, value, edge):
        """What a term keeps: a missing value fails `>>=` `>=` `=` and passes `<<=` `<=`."""
        if value is None:
            return op in ("<<=", "<=")
        return {">>=": value > edge, ">=": value >= edge, "<<=": value < edge,
                "<=": value <= edge, "=": value == edge}[op]

    def container(self, path, headers=None):
        status, _, data = self.pms.handle("GET", path, headers=headers or {})
        self.assertEqual(status, 200)
        return json.loads(data)["MediaContainer"]

    def count(self, section, kind, *terms):
        """The count-only probe: Start 0 and Size 0 as headers, no rows, `totalSize` is the answer."""
        path = f"/library/sections/{section}/all?type={kind}" + "".join("&" + t for t in terms)
        mc = self.container(path, self.COUNT_HEADERS)
        self.assertEqual((mc["size"], mc.get("Metadata", [])), (0, []))
        return mc["totalSize"]

    def rows(self, section, kind, sort=None, start=None, size=None):
        path = f"/library/sections/{section}/all?type={kind}" + (f"&sort={sort}" if sort else "")
        if start is not None:
            path += f"&X-Plex-Container-Start={start}&X-Plex-Container-Size={size}"
        return self.container(path)["Metadata"]

    def test_the_fixture_spreads_the_values_the_bands_are_cut_from(self):
        for field in ("rating", "audienceRating", "duration", "addedAt", "lastViewedAt", "year",
                      "originallyAvailableAt", "viewCount"):
            held = {self.mine(it, field) for it in self.movies} - {None}
            self.assertGreaterEqual(len(held), 3, field)
        for field in ("rating", "lastViewedAt", "originallyAvailableAt"):
            held = [self.mine(it, field) for it in self.movies]
            self.assertTrue(None in held and any(v is not None for v in held), field)
        years = {datetime.datetime.fromtimestamp(it["addedAt"], datetime.timezone.utc).year
                 for it in self.movies}
        self.assertGreaterEqual(len(years), 4, "added over several years")
        self.assertGreater(len({self.mine(it, "unviewedLeafCount") for it in self.shows}), 5)
        viewed = [it.get("lastViewedAt") for it in self.shows]
        self.assertTrue(None in viewed and any(v is not None for v in viewed), "shows nobody watched")

    def test_every_operator_counts_every_family_the_way_the_server_does(self):
        for field in ("rating", "audienceRating", "duration", "addedAt", "lastViewedAt", "year",
                      "originallyAvailableAt", "viewCount"):
            held = [self.mine(it, field) for it in self.movies]
            distinct = sorted(set(held) - {None})
            edge = distinct[len(distinct) // 2]
            for op in (">>=", ">=", "<<=", "<=", "="):
                with self.subTest(field=field, op=op):
                    self.assertEqual(self.count(1, 1, self.term(field, op, edge)),
                                     sum(self.passes(op, v, edge) for v in held))
        for field in ("addedAt", "lastViewedAt", "year", "originallyAvailableAt"):
            held = [self.mine(it, field) for it in self.shows]
            edge = sorted(set(held) - {None})[3]
            for alias in (field, "show." + field):
                with self.subTest(show=alias):
                    self.assertEqual(self.count(2, 2, self.term(alias, ">>=", edge)),
                                     sum(self.passes(">>=", v, edge) for v in held))

    def test_a_float_edge_is_a_float_and_the_spellings_agree(self):
        held = [it.get("rating") for it in self.movies]

        def above(edge):
            return sum(v is not None and v > edge for v in held)
        value = next(r for r in sorted(set(held) - {None})
                     if r != int(r) and above(round(r - 0.01, 4)) != above(int(r)))
        edge = round(value - 0.01, 4)
        self.assertEqual(self.count(1, 1, self.term("rating", ">>=", edge)), above(edge))
        self.assertEqual(self.count(1, 1, f"rating>>={edge}"), above(edge), "raw spelling")
        self.assertEqual(self.count(1, 1, self.term("rating", ">=", value)), above(edge))

    def test_items_with_no_value_sort_first_ascending_and_last_descending(self):
        for field in ("rating", "lastViewedAt", "originallyAvailableAt"):
            blank = sum(self.mine(it, field) is None for it in self.movies)
            self.assertTrue(0 < blank < len(self.movies))
            for sort, first in ((f"{field}:desc", False), (f"{field}:asc", True),
                                (f"{field}:desc:nullsLast", False), (f"{field}:asc:nullsLast", False)):
                with self.subTest(sort=sort):
                    got = [self.mine(it, field) for it in self.rows(1, 1, sort)]
                    lead, tail = (got[:blank], got[blank:]) if first else (
                        got[len(got) - blank:], got[:len(got) - blank])
                    self.assertEqual(lead, [None] * blank)
                    self.assertNotIn(None, tail)
                    self.assertEqual(tail, sorted(tail, reverse=sort.split(":")[1] == "desc"))

    def test_greater_excludes_no_value_items_and_less_includes_them(self):
        for field in ("rating", "lastViewedAt"):
            held = [self.mine(it, field) for it in self.movies]
            nonnull = [v for v in held if v is not None]
            below, above = min(nonnull) - 1, max(nonnull) + 1
            self.assertEqual(self.count(1, 1, self.term(field, ">>=", below)), len(nonnull))
            self.assertEqual(self.count(1, 1, self.term(field, ">=", below)), len(nonnull))
            self.assertEqual(self.count(1, 1, self.term(field, "<<=", below)), len(held) - len(nonnull))
            self.assertEqual(self.count(1, 1, self.term(field, "<=", above)), len(held))
            self.assertEqual(self.count(1, 1, self.term(field, "=", below)), 0)

    def test_a_probe_at_zero_leaves_out_no_value_items_and_only_the_inclusive_one_counts_stored_zeros(self):
        # Measured (docs/pms-api.md, "Stored zeros"): `>>=-1` and `>=0` count every item that holds a
        # number, `>>=0` only those above zero; an item with no value is in none of them.
        for field, items in (("rating", self.movies), ("duration", self.movies),
                             ("unviewedLeafCount", self.shows)):
            held = [self.mine(it, field) for it in items]
            nonnull = [v for v in held if v is not None]
            kind, section = (1, 1) if items is self.movies else (2, 2)
            with self.subTest(field=field):
                self.assertEqual(self.count(section, kind, self.term(field, ">>=", -1)), len(nonnull))
                self.assertEqual(self.count(section, kind, self.term(field, ">=", 0)), len(nonnull))
                self.assertEqual(self.count(section, kind, self.term(field, ">>=", 0)),
                                 sum(v > 0 for v in nonnull))
        self.assertLess(len([it for it in self.movies if "rating" in it]), len(self.movies), "unrated movies")
        # A server that STORES a zero the listing hides (movie `viewCount`: 25 of 44 on the measured one).
        lib = Library(seed=3, movies=20, shows=3, band_fixture=True)
        movies = [it for it in lib.items.values() if it["type"] == "movie" and it["librarySectionID"] == 1]
        unplayed = [it for it in movies if "viewCount" not in it]
        played = sum(it.get("viewCount", 0) > 0 for it in movies)
        self.assertGreaterEqual(len(unplayed), 4)
        for it in unplayed[:4]:
            it["viewCount"] = 0
        self.pms = MockPms(lib)  # on the instance: the shared server is back after the test
        for op, edge, want in ((">>=", -1, played + 4), (">=", 0, played + 4), (">>=", 0, played)):
            self.assertEqual(self.count(1, 1, self.term("viewCount", op, edge)), want, f"{op}{edge}")

    def test_a_negative_bare_number_on_a_date_field_is_an_absolute_epoch(self):
        # Measured (docs/pms-api.md): `-N` without a unit is a number, not a relative date. The mock's
        # release dates reach back to 1950, so the probes below tell an absolute reading from both an
        # ignored term and a clamp at zero.
        sixties = -315619200  # 1960-01-01 UTC
        for field, items in (("originallyAvailableAt", self.movies), ("addedAt", self.movies),
                             ("originallyAvailableAt", self.shows)):
            held = [self.mine(it, field) for it in items]
            kind, section = (1, 1) if items is self.movies else (2, 2)
            for op, edge in ((">=", sixties), (">>=", sixties - 1), (">=", -1), (">>=", -1),
                             ("<<=", -1), ("<=", sixties), (">=", 0)):
                with self.subTest(field=field, op=op, edge=edge):
                    self.assertEqual(self.count(section, kind, self.term(field, op, edge)),
                                     sum(self.passes(op, v, edge) for v in held))
        held = [self.mine(it, "originallyAvailableAt") for it in self.movies]
        before_1970 = sum(v is not None and v < 0 for v in held)
        before_1960 = sum(v is not None and v < sixties for v in held)
        self.assertGreater(before_1960, 0)
        self.assertGreater(before_1970, before_1960)
        self.assertEqual(self.count(1, 1, self.term("originallyAvailableAt", ">=", sixties)),
                         len([v for v in held if v is not None]) - before_1960)
        self.assertNotEqual(self.count(1, 1, self.term("originallyAvailableAt", ">=", sixties)),
                            self.count(1, 1, self.term("originallyAvailableAt", ">=", 0)), "not a clamp at 0")

    def test_the_count_probe_needs_a_start_as_well_as_a_size(self):
        total = len(self.movies)
        path = "/library/sections/1/all?type=1"
        for label, query, headers in (
                ("header", path, self.COUNT_HEADERS),
                ("query", path + "&X-Plex-Container-Start=0&X-Plex-Container-Size=0", {})):
            mc = self.container(query, headers)
            self.assertEqual((mc["size"], mc["totalSize"], mc["offset"], mc.get("Metadata", [])),
                             (0, total, 0, []), label)
        for label, query, headers in (
                ("header", path, {"X-Plex-Container-Size": "0"}),
                ("query", path + "&X-Plex-Container-Size=0", {})):
            mc = self.container(query, headers)
            self.assertEqual((mc["size"], "totalSize" in mc), (total, False), label)

    def test_offsets_are_the_count_at_or_above_the_edge_descending_and_its_complement_ascending(self):
        for section, kind, field, items in (
                (1, 1, "rating", self.movies), (1, 1, "lastViewedAt", self.movies),
                (1, 1, "addedAt", self.movies), (1, 1, "originallyAvailableAt", self.movies),
                (2, 2, "unviewedLeafCount", self.shows), (2, 2, "addedAt", self.shows)):
            held = [self.mine(it, field) for it in items]
            distinct = sorted(set(held) - {None})
            for edge in distinct[1::max(1, len(distinct) // 4)]:
                with self.subTest(field=field, edge=edge):
                    c = self.count(section, kind, self.term(field, ">=", edge))
                    unit = 0.01 if isinstance(edge, float) else 1
                    self.assertEqual(c, self.count(section, kind,
                                                   self.term(field, ">>=", round(edge - unit, 4))),
                                     "`>=E` is `>>=E-1unit`")
                    self.assertEqual(c, sum(v is not None and v >= edge for v in held))
                    # descending: the first item below E sits at offset count(>=E)
                    top, under = self.rows(section, kind, field + ":desc", c - 1, 2)[:2]
                    self.assertGreaterEqual(self.mine(top, field), edge)
                    self.assertTrue(self.mine(under, field) is None or self.mine(under, field) < edge)
                    # ascending: the first item at or above E sits at total - count(>=E)
                    offset = len(held) - c
                    before, first = self.rows(section, kind, field + ":asc", offset - 1, 2)[:2]
                    self.assertTrue(self.mine(before, field) is None or self.mine(before, field) < edge)
                    self.assertGreaterEqual(self.mine(first, field), edge)
                    self.assertEqual(self.count(section, kind, self.term(field, "<<=", edge)), offset)

    def test_a_shows_unplayed_count_rolls_up_from_its_episodes_and_sorts_and_filters(self):
        unwatched = {}
        for ep in self.lib.items.values():
            if ep["type"] == "episode":
                unwatched[ep["grandparentRatingKey"]] = unwatched.get(ep["grandparentRatingKey"], 0) + (
                    0 if ep.get("viewCount", 0) > 0 else 1)
        want = {show["ratingKey"]: unwatched.get(show["ratingKey"], 0) for show in self.shows}
        listed = self.rows(2, 2, "unviewedLeafCount:desc")
        self.assertEqual({r["ratingKey"]: self.mine(r, "unviewedLeafCount") for r in listed}, want)
        order = [self.mine(r, "unviewedLeafCount") for r in listed]
        self.assertEqual(order, sorted(order, reverse=True))
        edge = sorted(set(want.values()))[2]
        self.assertEqual(self.count(2, 2, self.term("unviewedLeafCount", ">>=", edge)),
                         sum(v > edge for v in want.values()))

    def test_episode_added_at_is_the_newest_episode_descending_and_the_oldest_ascending(self):
        times = {}
        for ep in self.lib.items.values():
            if ep["type"] == "episode":
                times.setdefault(ep["grandparentRatingKey"], []).append(ep["addedAt"])
        newest = {rk: max(v) for rk, v in times.items()}
        oldest = {rk: min(v) for rk, v in times.items()}
        desc = [newest[r["ratingKey"]] for r in self.rows(2, 2, "episode.addedAt:desc")]
        self.assertEqual(desc, sorted(desc, reverse=True))
        asc_rows = self.rows(2, 2, "episode.addedAt:asc")
        by_oldest = [oldest[r["ratingKey"]] for r in asc_rows]
        by_newest = [newest[r["ratingKey"]] for r in asc_rows]
        self.assertEqual(by_oldest, sorted(by_oldest))
        self.assertNotEqual(by_newest, sorted(by_newest), "ascending is NOT the descending mirror")
        mid = sorted(newest.values())[len(newest) // 2]
        self.assertEqual(self.count(2, 2, self.term("episode.addedAt", ">>=", mid)),
                         sum(v > mid for v in newest.values()), "any episode newer")
        older = self.count(2, 2, self.term("episode.addedAt", "<<=", mid))
        self.assertEqual(older, sum(v < mid for v in oldest.values()), "any episode older")
        self.assertNotEqual(older, sum(v < mid for v in newest.values()), "not: the newest is older")
        # the ascending offset of the first show with no episode older than `low` is count(<<=)
        low = sorted(oldest.values())[len(oldest) // 2]
        offset = self.count(2, 2, self.term("episode.addedAt", "<<=", low))
        self.assertGreater(offset, 0)
        self.assertLess(oldest[asc_rows[offset - 1]["ratingKey"]], low)
        self.assertGreaterEqual(oldest[asc_rows[offset]["ratingKey"]], low)

    def test_an_unknown_or_unsupported_term_is_ignored_and_answers_the_whole_library(self):
        total = len(self.movies)
        for term in ("bogusfield%3E%3E=100", "rating%3E%3E=-3w", "rating%3E%3E=nan", "rating%21=5",
                     "rating%3E%3E=", "episode.addedAt%3E%3E=1", "unviewedLeafCount%3E%3E=100"):
            self.assertEqual(self.count(1, 1, term), total, term)
        self.assertEqual(self.count(2, 2, "leafCount%3E%3E=100"), len(self.shows), "a REAL attribute")
        self.assertEqual(self.count(2, 2, "show.id=1"), len(self.shows))

    def test_release_dates_compare_as_utc_midnight_epochs(self):
        day = next(m["originallyAvailableAt"] for m in self.movies if m.get("originallyAvailableAt"))
        midnight = calendar.timegm(datetime.date.fromisoformat(day).timetuple())
        held = [self.mine(m, "originallyAvailableAt") for m in self.movies]
        self.assertGreaterEqual(held.count(midnight), 1)
        # In a zone that is not UTC (a CI runner usually is), where a day read as LOCAL midnight
        # (`time.mktime`) lands hours away from the UTC midnight the server compares.
        for zone in ("Asia/Tokyo", "America/Los_Angeles"):
            with self.subTest(zone=zone), local_zone(zone):
                self.assertNotEqual(time.mktime(datetime.date.fromisoformat(day).timetuple()), midnight)
                self.assertEqual(self.count(1, 1, self.term("originallyAvailableAt", "=", midnight)),
                                 held.count(midnight))
                self.assertEqual(
                    self.count(1, 1, self.term("originallyAvailableAt", "=", midnight + 3600)), 0)
                self.assertEqual(self.count(1, 1, self.term("originallyAvailableAt", ">>=", midnight)),
                                 sum(v is not None and v > midnight for v in held))

    def test_sort_menus_follow_the_server_per_type_under_the_fixture_only(self):
        def keys(pms, section, kind):
            typed = "" if kind is None else f"type={kind}&"
            status, _, data = pms.handle(
                "GET", f"/library/sections/{section}/all?{typed}includeMeta=1"
                       "&X-Plex-Container-Start=0&X-Plex-Container-Size=0")
            active = [t for t in json.loads(data)["MediaContainer"]["Meta"]["Type"] if t["active"]]
            self.assertEqual(len(active), 1)
            return [s["key"] for s in active[0]["Sort"]]
        self.assertEqual(keys(self.pms, 1, 1), [
            "titleSort", "originallyAvailableAt", "rating", "audienceRating", "duration", "addedAt",
            "lastViewedAt", "mediaHeight", "random"])
        self.assertEqual(keys(self.pms, 2, 2), [
            "titleSort", "originallyAvailableAt", "rating", "audienceRating", "unviewedLeafCount",
            "episode.addedAt", "lastViewedAt", "random"])
        short = ["titleSort", "addedAt", "originallyAvailableAt", "rating", "lastViewedAt", "random"]
        self.assertEqual(keys(MockPms(Library()), 1, 1), short)
        # no `type` takes the section's own; seasons and episodes have the lists measured for them
        self.assertEqual(keys(self.pms, 2, None), keys(self.pms, 2, 2))
        self.assertEqual(keys(self.pms, 1, None), keys(self.pms, 1, 1))
        self.assertEqual(len(keys(self.pms, 2, 3)), 2)
        self.assertEqual(len(keys(self.pms, 2, 4)), 7)
        for kind in (3, 4):
            self.assertNotEqual(keys(MockPms(Library()), 2, kind), keys(self.pms, 2, kind), kind)
        self.assertEqual(keys(MockPms(Library()), 2, 4), short, "the default library keeps its short list")

    # --- a show library's SEASONS (type 3) and EPISODES (type 4) listings --------------------------

    TV_TITLE_EPISODES = ("show.titleSort,season.index:nullsLast,episode.index:nullsLast,"
                         "episode.originallyAvailableAt:nullsLast,episode.titleSort,episode.id")
    TV_TITLE_EPISODES_DESC = TV_TITLE_EPISODES.replace("show.titleSort", "show.titleSort:desc", 1)

    _rail_library = None

    def use_a_library_with_letters(self):
        """This test reads a band-fixture library whose shows' sort titles start with five different letters
        (`rail_fixture`): the generated names all share one, so a letters index would have a single bucket."""
        cls = type(self)
        if cls._rail_library is None:
            lib = Library(seed=3, movies=60, shows=15, band_fixture=True, rail_fixture=True)
            cls._rail_library = (lib, MockPms(lib))
        self.lib, self.pms = cls._rail_library
        self.shows = [it for it in self.lib.items.values() if it["type"] == "show" and it["librarySectionID"] == 2]

    def tv_meta(self, kind):
        """`Meta.Type[]` of an `includeMeta=1` read of type `kind`: every entry, and the active one's sorts."""
        mc = self.container(f"/library/sections/2/all?type={kind}&includeMeta=1",
                            {"X-Plex-Container-Start": "0", "X-Plex-Container-Size": "0"})
        types = mc["Meta"]["Type"]
        return types, [t for t in types if t["active"]]

    def test_the_seasons_and_episodes_menus_are_the_two_and_the_seven_the_server_declares(self):
        sort_rows = lambda t: [(s["key"], s["descKey"], s["defaultDirection"]) for s in t["Sort"]]
        types, active = self.tv_meta(3)
        self.assertEqual(([t["type"] for t in types], [t["type"] for t in active]),
                         (["show", "season", "episode"], ["season"]), "only the listing's own type is active")
        self.assertEqual(sort_rows(active[0]), [
            ("show.titleSort,index", "show.titleSort:desc,index", "asc"), ("random", "random:desc", "desc")])
        types, active = self.tv_meta(4)
        self.assertEqual([t["type"] for t in active], ["episode"])
        self.assertEqual(sort_rows(active[0]), [
            (self.TV_TITLE_EPISODES, self.TV_TITLE_EPISODES_DESC, "asc"),
            ("originallyAvailableAt", "originallyAvailableAt:desc", "desc"), ("rating", "rating:desc", "desc"),
            ("audienceRating", "audienceRating:desc", "desc"), ("addedAt", "addedAt:desc", "desc"),
            ("lastViewedAt", "lastViewedAt:desc", "desc"), ("random", "random:desc", "desc")])

    def tv_expected_order(self, kind, desc):
        """The ratingKeys of the seasons (3) or episodes (4) in the Title order the server declares, built the
        other way round from the mock: show by show (reversed for the `descKey`), each show's seasons by
        index, each season's episodes by index."""
        out = []
        for show in sorted(self.shows, key=lambda s: s["titleSort"], reverse=desc):
            seasons = sorted((it for it in self.lib.items.values() if it["type"] == "season"
                              and it["parentRatingKey"] == show["ratingKey"]), key=lambda s: s["index"])
            for season in seasons:
                if kind == 3:
                    out.append(season["ratingKey"])
                else:
                    out += [e["ratingKey"] for e in sorted(
                        (it for it in self.lib.items.values() if it["type"] == "episode"
                         and it["parentRatingKey"] == season["ratingKey"]), key=lambda e: e["index"])]
        return out

    def test_seasons_and_episodes_are_ordered_by_the_declared_title_key_and_not_by_the_literal_one(self):
        self.use_a_library_with_letters()
        keys = lambda sort, kind: [r["ratingKey"] for r in self.rows(2, kind, sort)]
        titles = {3: ("show.titleSort,index", "show.titleSort:desc,index"),
                  4: (self.TV_TITLE_EPISODES, self.TV_TITLE_EPISODES_DESC)}
        for kind, (asc, desc) in titles.items():
            quote = lambda sort: urllib.parse.quote(sort, safe=":,.")
            self.assertGreater(len(keys(None, kind)), 20)
            self.assertEqual(keys(None, kind), self.tv_expected_order(kind, False), "the default order")
            self.assertEqual(keys(quote(asc), kind), self.tv_expected_order(kind, False), "the declared key")
            self.assertEqual(keys(quote(desc), kind), self.tv_expected_order(kind, True), "the declared descKey")
            self.assertNotEqual(keys("titleSort:asc", kind), self.tv_expected_order(kind, False),
                                f"type {kind}: the literal titleSort is another order, as measured")
        # `:desc` appended to the composite key is not its descKey: the shows do not reverse
        wrong = urllib.parse.quote(self.TV_TITLE_EPISODES + ":desc", safe=":,.")
        self.assertNotEqual(keys(wrong, 4), self.tv_expected_order(4, True))

    def test_the_letters_of_seasons_and_episodes_count_by_show_and_land_on_the_listings_boundaries(self):
        self.use_a_library_with_letters()
        shows = {s["ratingKey"]: s for s in self.shows}
        for kind, unwatched_key in ((3, "unwatchedLeaves"), (4, "unwatched")):
            for terms in ((), (f"{unwatched_key}=1",)):
                path = f"/library/sections/2/firstCharacter?type={kind}" + "".join("&" + t for t in terms)
                letters = [(d["title"], d["size"]) for d in self.container(path)["Directory"]]
                counted = self.count(2, kind, *terms)
                self.assertEqual(sum(size for _, size in letters), counted, (kind, terms, "sum == count"))
                self.assertGreater(len(letters), 1)
                for desc in (False, True):
                    sort = urllib.parse.quote({(3, False): "show.titleSort,index", (3, True): "show.titleSort:desc,index",
                                               (4, False): self.TV_TITLE_EPISODES, (4, True): self.TV_TITLE_EPISODES_DESC}[kind, desc],
                                              safe=":,.")
                    rows = self.rows(2, kind, sort + "".join("&" + t for t in terms))
                    walk = letters[::-1] if desc else letters
                    boundaries = [title for title, size in walk for _ in range(size)]
                    group = "grandparentRatingKey" if kind == 4 else "parentRatingKey"
                    glyphs = [shows[r[group]]["titleSort"][0].upper() for r in rows]
                    self.assertEqual(glyphs, boundaries, (kind, terms, desc))
        # the Unwatched episodes are fewer than the episodes: the term narrows the letters too
        narrowed = self.container("/library/sections/2/firstCharacter?type=4&unwatched=1")["Directory"]
        self.assertLess(sum(d["size"] for d in narrowed), self.count(2, 4))

    def test_episodes_hold_the_values_their_value_sorts_and_range_terms_read(self):
        """MOCK MODEL, NOT MEASURED for `rating` and `lastViewedAt`: a real PMS held no value in either on an
        episode (docs/pms-api.md, "Count probes for seasons and episodes"), so the exact `rating>>=` and
        `lastViewedAt>=` counts below say what the MOCK does with the values it was given. Only `addedAt` and
        `originallyAvailableAt` (UTC midnight) were verified on a server; `audienceRating` is rounded there."""
        episodes = [it for it in self.lib.items.values() if it["type"] == "episode" and it["librarySectionID"] == 2]
        for field in ("rating", "audienceRating", "originallyAvailableAt", "lastViewedAt"):
            held = [self.mine(it, field) for it in episodes]
            self.assertTrue(any(v is None for v in held) and any(v is not None for v in held), field)
        self.assertTrue(all("addedAt" in it for it in episodes), "an episode is always added at some time")
        for it in episodes:
            for field in ("rating", "audienceRating"):
                if field in it:
                    self.assertEqual(it[field], round(it[field], 1), "tenths, as the listing prints them")
        # a range term compares the number the row holds: ratings as floats, air dates as UTC midnight
        for field, op, edge in (("rating", ">>=", 6.0), ("audienceRating", "<<=", 5.0), ("addedAt", ">>=", 1_640_995_200),
                                ("lastViewedAt", ">=", 1_640_995_200),
                                ("originallyAvailableAt", ">>=", calendar.timegm((2010, 6, 1, 0, 0, 0)))):
            expected = sum(self.passes(op, self.mine(it, field), edge) for it in episodes)
            self.assertEqual(self.count(2, 4, self.term(field, op, edge)), expected, (field, op))
            self.assertGreater(expected, 0)
            self.assertLess(expected, len(episodes))
        # sort=audienceRating: no-value rows first ascending, last descending; the rest in order
        asc = self.rows(2, 4, "audienceRating:asc")
        desc = self.rows(2, 4, "audienceRating:desc")
        self.assertEqual(len(asc), len(episodes))
        values = lambda rows: [r.get("audienceRating") for r in rows]
        bare = sum(v is None for v in values(asc))
        self.assertGreater(bare, 0)
        self.assertEqual(values(asc)[:bare], [None] * bare)
        self.assertEqual(values(desc)[-bare:], [None] * bare)
        held = values(asc)[bare:]
        self.assertEqual(held, sorted(held))
        self.assertEqual(values(desc)[:-bare], held[::-1])
        self.assertEqual([r["ratingKey"] for r in self.rows(2, 4, "audienceRating")], [r["ratingKey"] for r in asc],
                         "no direction is ascending")

    def test_the_tv_listings_of_a_default_library_keep_their_own_order_and_letters(self):
        lib = Library()
        self.assertNotIn("band_fixture", lib.__dict__)
        for kind in ("season", "episode"):
            got = lib.section_items("2", {"type": "3" if kind == "season" else "4"})
            rows = [it for it in lib.items.values() if it["type"] == kind and it["librarySectionID"] == 2]
            self.assertEqual([it["ratingKey"] for it in got],
                             [it["ratingKey"] for it in sorted(rows, key=lambda it: it["titleSort"])], kind)
        letters = lib.first_characters("2", {"type": "4"})
        own = {}
        for it in lib.items.values():
            if it["type"] == "episode" and it["librarySectionID"] == 2:
                own[it["titleSort"][0].upper()] = own.get(it["titleSort"][0].upper(), 0) + 1
        self.assertEqual({d["title"]: d["size"] for d in letters}, own, "counted by the row's own title, as before")

    def test_off_by_default_and_the_older_sorts_order_as_they_always_did(self):
        lib = Library()
        self.assertNotIn("band_fixture", lib.__dict__)
        old = {"titleSort": lambda it: it["titleSort"], "addedAt": lambda it: it["addedAt"],
               "originallyAvailableAt": lambda it: it.get("originallyAvailableAt", ""),
               "year": lambda it: it.get("year", 0), "rating": lambda it: it.get("rating", 0.0),
               "lastViewedAt": lambda it: it.get("lastViewedAt", 0)}
        for section, kind in ((1, "movie"), (2, "show")):
            base = [it for it in lib.items.values()
                    if it["type"] == kind and it["librarySectionID"] == section]
            for field, key in old.items():
                for direction in ("asc", "desc"):
                    expected = sorted(base, key=key, reverse=direction == "desc")
                    got = lib.section_items(str(section), {"sort": f"{field}:{direction}"})
                    self.assertEqual([it["ratingKey"] for it in got],
                                     [it["ratingKey"] for it in expected], (kind, field, direction))

    def test_the_flag_reaches_the_server_over_http(self):
        server, pms = serve(0, movies=30, shows=4, band_fixture=True)
        try:
            self.assertTrue(pms.lib.band_fixture)
            base = f"http://127.0.0.1:{server.server_address[1]}"
            request = urllib.request.Request(
                base + "/library/sections/1/all?type=1&rating%3E%3E=7.99",
                headers={"X-Plex-Container-Start": "0", "X-Plex-Container-Size": "0"})
            with urllib.request.urlopen(request, timeout=5) as response:
                mc = json.loads(response.read())["MediaContainer"]
            held = [it.get("rating") for it in pms.lib.items.values()
                    if it["type"] == "movie" and it["librarySectionID"] == 1]
            self.assertEqual((mc["size"], mc["totalSize"]),
                             (0, sum(v is not None and v > 7.99 for v in held)))
        finally:
            server.shutdown()
            server.server_close()


class HomeHubsFlag(unittest.TestCase):
    """#395: `--home-hubs N` makes `/hubs` answer exactly N hubs, so the app's Home row cap can be
    exercised at 18 and 170 rows."""
    PATH = "/hubs?count=12&excludeContinueWatching=1"

    def test_home_hubs_flag_serves_the_requested_number_of_hubs(self):
        pms = MockPms(Library())
        pms.home_hubs = 18
        hubs = get(pms, self.PATH)["Hub"]
        self.assertEqual(len(hubs), 18)
        for h in hubs[1:]:
            self.assertTrue(h["Metadata"])
            for it in h["Metadata"]:
                self.assertTrue(it["title"] and it["thumb"])
            self.assertEqual(h["size"], len(h["Metadata"]))
        self.assertEqual(len({h["hubIdentifier"] for h in hubs}), 18)
        self.assertEqual(len({h["title"] for h in hubs}), 18)
        shelf = next(h for h in hubs if h["hubIdentifier"] == "mock.shelf.3")
        self.assertEqual(get(pms, shelf["key"])["Metadata"][:12], shelf["Metadata"])
        self.assertLessEqual(len(get(pms, "/hubs?count=2")["Hub"][-1]["Metadata"]), 2)

    def test_home_hubs_default_leaves_the_response_unchanged(self):
        pms = MockPms(Library())
        self.assertEqual(pms.home_hubs, 0)
        before = pms.handle("GET", self.PATH)
        hubs = json.loads(before[2])["MediaContainer"]["Hub"]
        self.assertEqual([h["hubIdentifier"] for h in hubs],
                         ["home.continue", "home.movies.recent", "home.television.recent"])


class HomeSeasonsFlag(unittest.TestCase):
    """`--home-seasons` adds a Recently Added shelf of SEASON items to `/hubs`: what Home's captions
    are graded on for a tile that is not a movie, a show or an episode."""
    PATH = "/hubs?count=12&excludeContinueWatching=1"

    def test_the_shelf_holds_seasons_that_name_their_show_and_pages_through_its_key(self):
        pms = MockPms(Library(shows=3, seasons=2))
        pms.home_seasons = True
        hubs = get(pms, self.PATH)["Hub"]
        self.assertEqual([h["hubIdentifier"] for h in hubs],
                         ["home.continue", "mock.seasons", "home.movies.recent", "home.television.recent"])
        shelf = hubs[1]
        self.assertEqual(len(shelf["Metadata"]), 6)
        self.assertEqual(shelf["size"], 6)
        for it in shelf["Metadata"]:
            self.assertEqual(it["type"], "season")
            self.assertTrue(it["title"] and it["parentTitle"] and it["thumb"], it)
            self.assertGreater(it["index"], 0)
        self.assertEqual(get(pms, shelf["key"])["Metadata"][:12], shelf["Metadata"])

    def test_the_default_leaves_the_response_unchanged(self):
        pms = MockPms(Library())
        self.assertFalse(pms.home_seasons)
        hubs = json.loads(pms.handle("GET", self.PATH)[2])["MediaContainer"]["Hub"]
        self.assertEqual([h["hubIdentifier"] for h in hubs],
                         ["home.continue", "home.movies.recent", "home.television.recent"])
        self.assertFalse(get(pms, "/hubs/mock/seasons").get("Metadata"), "its key answers nothing unless asked for")

    def test_the_flag_reaches_the_server(self):
        server, pms = serve(0, home_seasons=True)
        try:
            self.assertTrue(pms.home_seasons)
        finally:
            server.shutdown()
            server.server_close()


class SectionHubsFlag(unittest.TestCase):
    """#412: `--section-hubs N` makes `/hubs/sections/<id>` answer exactly N hubs and
    `--section-hubs-linked M` makes M of them promoted collections, so the Library's shelf bound
    can be exercised at 170 shelves."""
    PATH = "/hubs/sections/1"

    @staticmethod
    def _pms(n, linked=0):
        pms = MockPms(Library())
        pms.section_hubs, pms.section_hubs_linked = n, linked
        return pms

    def test_default_leaves_the_response_unchanged(self):
        pms = MockPms(Library())
        self.assertEqual((pms.section_hubs, pms.section_hubs_linked), (0, 0))
        hubs = get(pms, self.PATH)["Hub"]
        self.assertEqual([h["hubIdentifier"] for h in hubs],
                         ["movie.inprogress.1", "movie.recentlyadded.1"])

    def test_serves_exactly_n_distinct_hubs_each_with_items(self):
        pms = self._pms(170)
        hubs = get(pms, self.PATH)["Hub"]
        self.assertEqual(len(hubs), 170)
        self.assertEqual(len({h["hubIdentifier"] for h in hubs}), 170)
        self.assertEqual(len({h["title"] for h in hubs[2:]}), 168)
        for h in hubs[2:]:
            self.assertTrue(h["hubIdentifier"].startswith("mock.section.1.shelf."))
            self.assertTrue(h["Metadata"])
            self.assertEqual(h["size"], len(h["Metadata"]))
        shelf = hubs[5]
        self.assertEqual(shelf["key"], f"/hubs/mock/section/1/shelf/{5 - 1}")
        self.assertEqual(get(pms, shelf["key"])["Metadata"][:12], shelf["Metadata"])
        # the second section pads too, with its own kind
        self.assertEqual(len(get(pms, "/hubs/sections/2")["Hub"]), 170)

    def test_linked_hubs_are_promoted_collections_the_mock_resolves(self):
        pms = self._pms(170, 40)
        hubs = get(pms, self.PATH)["Hub"]
        self.assertEqual(len(hubs), 170)
        linked = [h for h in hubs if h["hubIdentifier"].startswith("custom.collection.")]
        self.assertEqual(len(linked), 40)
        self.assertEqual(len({h["hubIdentifier"] for h in hubs}), 170)
        for h in linked:
            # the shape plx_plex's promoted_collection_link accepts: section 1, a rating key, and
            # a listing key naming that same collection
            _, _, section, rk, _ = h["hubIdentifier"].split(".")
            self.assertEqual(section, "1")
            self.assertEqual(h["key"], f"/library/collections/{rk}/children")
            self.assertTrue(h["Metadata"])
            self.assertEqual(get(pms, h["key"])["Metadata"][:len(h["Metadata"])], h["Metadata"])
        # spread through the list, not bunched at the end
        first = next(i for i, h in enumerate(hubs) if h in linked)
        self.assertLess(first, 10)

    def test_the_shipped_scene_arguments_hold(self):
        # tests/manifest.json library-shelves-deep: 170 hubs, 40 of them linked
        pms = self._pms(170, 40)
        hubs = get(pms, self.PATH)["Hub"]
        self.assertEqual(sum(h["hubIdentifier"].startswith("custom.collection.") for h in hubs), 40)

    def test_the_flags_reach_the_server_and_the_cli_refuses_nonsense(self):
        server, pms = serve(0, section_hubs=9, section_hubs_linked=3)
        try:
            self.assertEqual((pms.section_hubs, pms.section_hubs_linked), (9, 3))
        finally:
            server.shutdown()
            server.server_close()
        for argv in (["--section-hubs", "-1"], ["--section-hubs", "2", "--section-hubs-linked", "3"]):
            p = subprocess.run(["python3", str(pathlib.Path(__file__).with_name("mock_pms.py")),
                                "--port", "0", *argv], capture_output=True, text=True, timeout=30)
            self.assertNotEqual(p.returncode, 0, argv)


class DoviAndExtraMedia(unittest.TestCase):
    """The DOVI*/container derivation `--extra-media` relies on — the field NAMES a real PMS
    sends (docs/pms-api.md, verified live 2026-08-21), mapped from ffprobe's own "DOVI
    configuration record" side-data shape rather than probed against a real DV file here."""

    def test_dovi_wire_maps_the_real_pms_field_names(self):
        stream = {
            "side_data_list": [{
                "side_data_type": "DOVI configuration record",
                "dv_version_major": 1, "dv_version_minor": 0,
                "dv_profile": 8, "dv_level": 6,
                "rpu_present_flag": 1, "el_present_flag": 0, "bl_present_flag": 1,
                "dv_bl_signal_compatibility_id": 1,
            }],
        }
        self.assertEqual(Library._dovi_wire(stream), {
            "DOVIPresent": True, "DOVIProfile": 8, "DOVIBLCompatID": 1,
            "DOVIELPresent": False, "DOVILevel": 6, "DOVIVersion": "1.0",
            "DOVIBLPresent": True, "DOVIRPUPresent": True,
        })

    def test_dovi_wire_is_silent_without_a_configuration_record(self):
        # Silence must not convict (metadata::Dovi's rule): an ordinary SDR/HDR10 stream, one
        # with no side-data at all, and one whose side-data is unrelated (e.g. embedded cover
        # art) must all send NONE of the DOVI* keys rather than a false profile/compat id of 0.
        self.assertEqual(Library._dovi_wire({"side_data_list": []}), {})
        self.assertEqual(Library._dovi_wire({}), {})
        self.assertEqual(
            Library._dovi_wire({"side_data_list": [{"side_data_type": "Something Else"}]}), {})

    def test_resolution_label_buckets_by_decoded_size_not_a_hardcoded_1080p(self):
        self.assertEqual(Library._resolution_label(3840, 1604), ("4k", "4K"))
        self.assertEqual(Library._resolution_label(1920, 1080), ("1080", "1080p"))
        self.assertEqual(Library._resolution_label(1280, 720), ("720", "720p"))
        self.assertEqual(Library._resolution_label(64, 64), ("sd", "SD"))

    def test_extra_media_ids_never_collide_with_a_large_generated_library_or_the_verify_block(self):
        lib = Library(movies=1000, shows=50, rail_fixture=True)
        self.assertNotIn(EXTRA_MEDIA_RK_BASE, lib.items)
        self.assertLess(max(lib.items), EXTRA_MEDIA_RK_BASE)

    @unittest.skipUnless(
        HAS_FFMPEG_AND_FFPROBE,
        "needs ffmpeg+ffprobe; the host CI runner has neither — the field mapping is covered by the pure tests above",
    )
    def test_extra_media_container_and_content_type_come_from_the_file_extension(self):
        with tempfile.TemporaryDirectory() as tmp:
            mp4 = pathlib.Path(tmp) / "clip.mp4"
            mkv = pathlib.Path(tmp) / "clip.mkv"
            for out in (mp4, mkv):
                subprocess.check_call([
                    "ffmpeg", "-y", "-v", "error", "-f", "lavfi", "-i",
                    "color=size=64x64:rate=24:duration=1", "-f", "lavfi", "-i",
                    "sine=frequency=330:duration=1", "-c:v", "libx264", "-c:a", "aac", "-t", "1",
                    str(out)])
            server, pms = serve(0, movies=0, extra_media=[mp4, mkv])
            try:
                lib = pms.lib
                rks = sorted(lib.extra_media_files)
                self.assertEqual(rks, [EXTRA_MEDIA_RK_BASE, EXTRA_MEDIA_RK_BASE + 1])
                base = f"http://127.0.0.1:{server.server_address[1]}"
                for rk, want_container, want_ct in (
                        (rks[0], "mp4", "video/mp4"), (rks[1], "mkv", "video/x-matroska")):
                    media = lib.items[rk]["Media"][0]
                    part = media["Part"][0]
                    self.assertEqual(media["container"], want_container)
                    self.assertTrue(part["key"].endswith(f"/file.{want_container}"), part["key"])
                    self.assertEqual(lib.media_content_type[part["id"]], want_ct)
                    # the actual byte-serving path (Handler._media), not the synthetic-item
                    # fallback in MockPms.handle — this is the header the app's part probe sees.
                    with urllib.request.urlopen(base + part["key"], timeout=5) as response:
                        self.assertEqual(response.headers["Content-Type"], want_ct)
            finally:
                server.shutdown()
                server.server_close()

    @unittest.skipUnless(
        HAS_FFMPEG_AND_FFPROBE,
        "needs ffmpeg+ffprobe; the host CI runner has neither — the field mapping is covered by the pure tests above",
    )
    def test_startup_prints_ratingkey_to_filename_for_every_extra_media_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            clip = pathlib.Path(tmp) / "clip.mp4"
            subprocess.check_call([
                "ffmpeg", "-y", "-v", "error", "-f", "lavfi", "-i",
                "color=size=64x64:rate=24:duration=1", "-f", "lavfi", "-i",
                "sine=frequency=330:duration=1", "-c:v", "libx264", "-c:a", "aac", "-t", "1",
                str(clip)])
            server, pms = serve(0, movies=0, extra_media=[clip])
            try:
                self.assertEqual(pms.lib.extra_media_files, {EXTRA_MEDIA_RK_BASE: clip})
            finally:
                server.shutdown()
                server.server_close()


@unittest.skipUnless(
    HAS_FFMPEG_AND_FFPROBE,
    "needs ffmpeg+ffprobe; the host CI runner has neither -- the stream wire is the ffprobe record's",
)
class ExtraMediaAudioSelection(unittest.TestCase):
    """`--extra-media-audio N`: the shape PMS reports when the profile's audio language picks a
    track other than the file's default one (`selected` on stream N, the file's `default` flag left
    as the file wrote it), which the app's `audio_intents` honours as the server's own pick."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.clip = pathlib.Path(cls.tmp.name) / "two-audio.mp4"
        # Two audio streams, the FIRST the file's default (as `pipe_uri_carrier_720p24_2a.mp4` is).
        subprocess.check_call([
            "ffmpeg", "-y", "-v", "error", "-f", "lavfi", "-i", "color=size=64x64:rate=24:duration=1",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=1",
            "-f", "lavfi", "-i", "sine=frequency=880:duration=1",
            "-map", "0:v", "-map", "1:a", "-map", "2:a", "-c:v", "libx264", "-c:a", "aac",
            "-metadata:s:a:0", "language=eng", "-metadata:s:a:1", "language=ger",
            "-disposition:a:0", "default", "-disposition:a:1", "0", "-t", "1", str(cls.clip)])

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def audio(self, lib):
        streams = lib.items[EXTRA_MEDIA_RK_BASE]["Media"][0]["Part"][0]["Stream"]
        return [s for s in streams if s["streamType"] == 2]

    def test_without_the_flag_the_files_default_stream_is_the_selected_one(self):
        audio = self.audio(Library(movies=0, extra_media=[self.clip]))
        self.assertEqual([(s["languageCode"], s["default"], s["selected"]) for s in audio],
                         [("eng", True, True), ("ger", False, False)])

    def test_the_flag_selects_the_nth_audio_stream_and_only_that_one(self):
        audio = self.audio(Library(movies=0, extra_media=[self.clip], extra_media_audio=2))
        self.assertEqual([(s["languageCode"], s["default"], s["selected"]) for s in audio],
                         [("eng", True, False), ("ger", False, True)],
                         "PMS reports one selected audio stream, and the file's default flag is untouched")

    def test_the_flag_names_the_default_stream_too(self):
        audio = self.audio(Library(movies=0, extra_media=[self.clip], extra_media_audio=1))
        self.assertEqual([(s["languageCode"], s["default"], s["selected"]) for s in audio],
                         [("eng", True, True), ("ger", False, False)])

    def test_the_selected_stream_is_what_the_wire_answers(self):
        server, pms = serve(0, movies=0, extra_media=[self.clip], extra_media_audio=2)
        try:
            meta = get(pms, f"/library/metadata/{EXTRA_MEDIA_RK_BASE}")["Metadata"][0]
            streams = meta["Media"][0]["Part"][0]["Stream"]
            self.assertEqual([s["selected"] for s in streams if s["streamType"] == 2], [False, True])
        finally:
            server.shutdown()
            server.server_close()

    def test_an_ordinal_the_file_cannot_satisfy_is_refused_at_startup(self):
        for bad in (0, -1, 3):
            with self.subTest(n=bad), self.assertRaises(ValueError) as why:
                Library(movies=0, extra_media=[self.clip], extra_media_audio=bad)
            self.assertIn("--extra-media-audio", str(why.exception))
        with self.assertRaises(ValueError) as why:
            Library(movies=0, extra_media_audio=2)
        self.assertIn("--extra-media", str(why.exception))



@unittest.skipUnless(HAS_FFMPEG_AND_FFPROBE, "no ffmpeg/ffprobe")
class ExtraMediaSubtitle(unittest.TestCase):
    """`--extra-media-subtitle N`: the Nth subtitle stream of an `--extra-media` file is the server-selected
    one (`selected` on it alone), the file's `default` flag untouched: the shape PMS reports when a profile's
    subtitle preference picks an embedded track, which the app reads as `server-selected subtitle: ...`."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        root = pathlib.Path(cls.tmp.name)
        for n, text in enumerate(("1\n00:00:00,000 --> 00:00:01,000\nA\n", "1\n00:00:00,000 --> 00:00:01,000\nB\n")):
            (root / f"s{n}.srt").write_text(text, encoding="utf-8")
        cls.clip = root / "two-subs.mkv"
        subprocess.check_call([
            "ffmpeg", "-y", "-v", "error", "-f", "lavfi", "-i", "color=size=64x64:rate=24:duration=1",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=1",
            "-i", str(root / "s0.srt"), "-i", str(root / "s1.srt"),
            "-map", "0:v", "-map", "1:a", "-map", "2", "-map", "3", "-c:v", "libx264", "-c:a", "aac", "-c:s", "srt",
            "-metadata:s:s:0", "language=eng", "-metadata:s:s:1", "language=deu",
            "-disposition:s:0", "0", "-disposition:s:1", "0", "-t", "1", str(cls.clip)])

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def subs(self, lib):
        streams = lib.items[EXTRA_MEDIA_RK_BASE]["Media"][0]["Part"][0]["Stream"]
        return [(s["languageCode"], s["selected"]) for s in streams if s["streamType"] == 3]

    def test_without_the_flag_no_subtitle_is_selected(self):
        self.assertEqual(self.subs(Library(movies=0, extra_media=[self.clip])), [("eng", False), ("deu", False)])

    def test_the_flag_selects_the_nth_subtitle_and_only_that_one(self):
        for n, want in ((1, [("eng", True), ("deu", False)]), (2, [("eng", False), ("deu", True)])):
            with self.subTest(n=n):
                self.assertEqual(self.subs(Library(movies=0, extra_media=[self.clip], extra_media_subtitle=n)), want)

    def test_an_ordinal_the_file_cannot_satisfy_is_refused_at_startup(self):
        for bad in (0, -1, 3):
            with self.subTest(n=bad), self.assertRaises(ValueError):
                Library(movies=0, extra_media=[self.clip], extra_media_subtitle=bad)

    def test_it_needs_extra_media(self):
        with self.assertRaises(ValueError):
            Library(movies=0, extra_media_subtitle=1)

class PartFaults(unittest.TestCase):
    """`--cut-part-after` and `--part-range-answers-200`: opt-in faults on media part bytes, for
    reproducing a direct-play body cut (and a server that ignores Range) against the mock."""

    BODY = bytes(range(256)) * 8  # 2048 bytes

    def _serve(self, **faults):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        clip = pathlib.Path(tmp.name) / "clip.mkv"
        clip.write_bytes(self.BODY)
        server, pms = serve(0, movies=0, **faults)
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        pms.lib.media_files = {77: clip}
        pms.lib.media_content_type = {77: "video/x-matroska"}
        return f"http://127.0.0.1:{server.server_address[1]}/library/parts/77/1/file.mkv"

    def _get(self, url, range_from=None):
        request = urllib.request.Request(
            url, headers={} if range_from is None else {"Range": f"bytes={range_from}-"})
        with urllib.request.urlopen(request, timeout=5) as response:
            try:
                body = response.read()
            except Exception as e:  # http.client.IncompleteRead carries what arrived
                body = getattr(e, "partial", b"")
            return response.status, response.headers.get("Content-Range"), body

    def test_off_by_default_serves_whole_and_ranged(self):
        url = self._serve()
        self.assertEqual(self._get(url), (200, None, self.BODY))
        self.assertEqual(self._get(url, 100), (206, "bytes 100-2047/2048", self.BODY[100:]))

    def test_the_first_response_of_a_part_is_cut_and_the_ranged_reopen_is_honest(self):
        url = self._serve(cut_part_after=500)
        status, _, body = self._get(url, 0)
        self.assertEqual((status, body), (206, self.BODY[:500]))
        self.assertEqual(self._get(url, 500), (206, "bytes 500-2047/2048", self.BODY[500:]))
        # cut once per part, not once per request
        self.assertEqual(self._get(url, 0)[2], self.BODY)

    def test_a_head_or_a_short_read_does_not_spend_the_cut(self):
        url = self._serve(cut_part_after=500)
        head = urllib.request.Request(url, method="HEAD")
        with urllib.request.urlopen(head, timeout=5) as response:
            self.assertEqual(response.status, 200)
        # a ranged read that ends before the cut point carries no more than BYTES bytes
        short = urllib.request.Request(url, headers={"Range": "bytes=0-99"})
        with urllib.request.urlopen(short, timeout=5) as response:
            self.assertEqual(response.read(), self.BODY[:100])
        status, _, body = self._get(url, 0)
        self.assertEqual((status, body), (206, self.BODY[:500]))

    def test_a_cut_past_the_end_of_the_body_cuts_nothing(self):
        url = self._serve(cut_part_after=5000)
        self.assertEqual(self._get(url, 0)[2], self.BODY)

    def test_range_answers_200_only_past_byte_zero(self):
        url = self._serve(part_range_answers_200=True)
        self.assertEqual(self._get(url, 0)[:2], (206, "bytes 0-2047/2048"))
        self.assertEqual(self._get(url, 500), (200, None, self.BODY))


class PlaintextOnlyLan(unittest.TestCase):
    """PLX-NATIVE-10: `--plaintext-only-lan`'s wire shape and the fail-mode/advertise-ip
    plumbing, kept separate from the (also-passing) live-socket assertions in mock_pms.py's own
    `--selftest`, which additionally proves the https route really fails a TLS handshake."""

    def test_resources_shape_has_no_relay_and_the_two_documented_connections(self):
        lib = Library(seed=7)
        rows = plaintext_only_lan_resources(lib, "127.0.0.1", 32499, 32500, "s0a0b0c0d")
        self.assertEqual(len(rows), 1)
        res = rows[0]
        self.assertEqual(res["clientIdentifier"], lib.machine)
        self.assertEqual(res["provides"], "server")
        self.assertIs(res["owned"], True)
        self.assertIs(res["httpsRequired"], False)
        self.assertIs(res["publicAddressMatches"], True)
        self.assertIsNone(res["sourceTitle"])
        conns = res["connections"]
        self.assertEqual(len(conns), 2)
        self.assertFalse(any(c["relay"] for c in conns))
        https = next(c for c in conns if c["protocol"] == "https")
        http = next(c for c in conns if c["protocol"] == "http")
        self.assertEqual(https, {"protocol": "https", "address": "127.0.0.1", "port": 32500,
                                  "uri": f"https://127-0-0-1.{PLEX_DIRECT_HASH}.plex.direct:32500",
                                  "local": True, "relay": False, "IPv6": False})
        self.assertEqual(http, {"protocol": "http", "address": "127.0.0.1", "port": 32499,
                                 "uri": "http://127.0.0.1:32499", "local": True, "relay": False,
                                 "IPv6": False})

    def test_resource_discovery_advertises_only_the_bound_mock(self):
        pms = MockPms(Library(seed=7))
        status, ctype, body = pms.handle("GET", "/api/v2/resources")
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body), [], "an unbound fixture advertises no listener")
        self.assertEqual(pms.unknown, [])
        server, pms = serve(0, seed=7)
        try:
            status, _, body = pms.handle("GET", "/api/v2/resources")
            self.assertEqual(status, 200)
            resources = json.loads(body)
            self.assertEqual(len(resources), 1)
            self.assertEqual(resources[0]["clientIdentifier"], pms.lib.machine)
            self.assertEqual(resources[0]["connections"], [{
                "protocol": "http", "address": "127.0.0.1", "port": server.server_address[1],
                "uri": f"http://127.0.0.1:{server.server_address[1]}", "local": True,
                "relay": False, "IPv6": False,
            }])
        finally:
            server.shutdown()
            server.server_close()

    def test_advertise_ip_none_falls_back_to_loopback_with_a_warning_when_undetectable(self):
        import ipaddress
        server, pms = serve(0, seed=7, plaintext_only_lan=True, advertise_ip=None)
        try:
            ipaddress.IPv4Address(pms.plaintext_only_lan["ip"])  # a real, parseable IPv4
            # a real LAN IPv4 needs no warning; the loopback fallback always gets one.
            if pms.plaintext_only_lan["ip"] == "127.0.0.1":
                buf = io.StringIO()
                with contextlib.redirect_stderr(buf):
                    server2, pms2 = serve(0, seed=7, plaintext_only_lan=True, advertise_ip=None)
                try:
                    self.assertEqual(pms2.plaintext_only_lan["ip"], "127.0.0.1")
                    self.assertIn("NOT LAN-eligible", buf.getvalue())
                finally:
                    server2.shutdown()
                    server2.server_close()
        finally:
            server.shutdown()
            server.server_close()

    def test_unreachable_fail_mode_opens_no_listener_and_refuses(self):
        server, pms = serve(0, seed=7, plaintext_only_lan=True, advertise_ip="127.0.0.1",
                            insecure_fail_mode="unreachable")
        try:
            self.assertIsNone(server.insecure_fail_listener)
            with self.assertRaises(OSError):
                socket.create_connection(
                    ("127.0.0.1", pms.plaintext_only_lan["fail_port"]), timeout=5).close()
        finally:
            server.shutdown()
            server.server_close()

    def test_identity_over_the_plaintext_connection_is_tokenless_and_matches_the_resource(self):
        server, pms = serve(0, seed=7, plaintext_only_lan=True, advertise_ip="127.0.0.1")
        try:
            base = f"http://127.0.0.1:{server.server_address[1]}"
            with urllib.request.urlopen(base + "/identity", timeout=5) as response:
                identity = json.load(response)["MediaContainer"]
            self.assertEqual(identity["machineIdentifier"], pms.lib.machine)
        finally:
            server.shutdown()
            server.server_close()


class MarkerAccess826(unittest.TestCase):
    def test_viewer_grants_are_separate_from_the_server_subscription(self):
        pms = MockPms(Library())
        def features():
            status, _, body = pms.handle("GET", "/api/v2/features")
            self.assertEqual(status, 200)
            return {row["id"] for row in json.loads(body)}
        self.assertEqual(features(), {"intro-markers", "credits-markers"})
        pms.handle("POST", "/_mock/config", json.dumps({"plex_pass": False}).encode())
        self.assertEqual(features(), {"intro-markers", "credits-markers"})
        pms.handle("POST", "/_mock/config", json.dumps({"plex_pass": True, "viewer_intro_skip": False}).encode())
        self.assertEqual(features(), {"credits-markers"})
        pms.handle("POST", "/_mock/config", json.dumps({"viewer_credits_skip": False}).encode())
        self.assertEqual(features(), set())


class Enhancement266(unittest.TestCase):
    """#266 (Boost Dialog / Normalize Loudness) mock support: the plex-pass/loudness-capability
    flags, the /decision enhancement shapes, the runtime /_mock/config toggle, the request log,
    and --transcode-fixture."""

    def _audio(self, item):
        return next(s for s in item["Media"][0]["Part"][0]["Stream"] if s["streamType"] == 2)

    def test_no_plex_pass_flag(self):
        pms = MockPms(Library())
        self.assertIs(get(pms, "/identity")["myPlexSubscription"], True)
        self.assertEqual(get(pms, "/identity")["version"], "1.41.0.0000-synthetic")

        server, pms2 = serve(0, seed=1, plex_pass=False)
        try:
            identity = json.loads(pms2.handle("GET", "/identity")[2])["MediaContainer"]
            self.assertIs(identity["myPlexSubscription"], False)
            # tools/mock-guest.py keys off this exact version string — untouched by the flag.
            self.assertEqual(identity["version"], "1.41.0.0000-synthetic")
        finally:
            server.shutdown()
            server.server_close()

        # also flippable live
        pms.handle("POST", "/_mock/config", json.dumps({"plex_pass": False}).encode())
        self.assertIs(get(pms, "/identity")["myPlexSubscription"], False)

    def test_loudness_attr_default_and_flag(self):
        lib_on = Library(movies=1)
        self.assertEqual(self._audio(lib_on.items[1001])["canNormalizeLoudness"], "1")
        self.assertEqual(
            self._audio(lib_on.items[ENHANCEMENT_AC3_2CH_RK])["canNormalizeLoudness"], "1")

        lib_off = Library(movies=1, loudness_analysis=False)
        self.assertNotIn("canNormalizeLoudness", self._audio(lib_off.items[1001]))
        self.assertNotIn("canNormalizeLoudness",
                         self._audio(lib_off.items[ENHANCEMENT_AC3_2CH_RK]))

    def test_fixture_items_have_the_documented_properties(self):
        lib = Library()
        ac3 = lib.items[ENHANCEMENT_AC3_2CH_RK]
        self.assertEqual((ac3["Media"][0]["audioCodec"], ac3["Media"][0]["audioChannels"]),
                         ("ac3", 2))
        aac = lib.items[ENHANCEMENT_AAC_51_RK]
        self.assertEqual((aac["Media"][0]["audioCodec"], aac["Media"][0]["audioChannels"]),
                         ("aac", 6))
        dv = lib.items[ENHANCEMENT_DV_P8_RK]
        video = next(s for s in dv["Media"][0]["Part"][0]["Stream"] if s["streamType"] == 1)
        self.assertEqual(video["DOVIProfile"], 8)
        self.assertIs(video["DOVIPresent"], True)
        default_srt = lib.items[ENHANCEMENT_DEFAULT_SRT_RK]
        sub = next(s for s in default_srt["Media"][0]["Part"][0]["Stream"] if s["streamType"] == 3)
        self.assertIs(sub["default"], True)
        self.assertIs(sub["selected"], True)
        external = lib.items[ENHANCEMENT_EXTERNAL_SRT_RK]
        ext_sub = next(s for s in external["Media"][0]["Part"][0]["Stream"]
                       if s["streamType"] == 3 and s.get("external"))
        self.assertIs(ext_sub["selected"], True)
        self.assertEqual(ext_sub["codec"], "srt")
        self.assertTrue(ext_sub["key"])
        # invisible to the ordinary section listings/rails/counts
        # A real server answers a total only to a paged request: the size-0 window is the read.
        self.assertEqual(get(MockPms(lib), "/library/sections/1/all"
                             "?X-Plex-Container-Start=0&X-Plex-Container-Size=0")["totalSize"], 48)

    def test_decision_enhancement_shapes(self):
        pms = MockPms(Library())
        rk = ENHANCEMENT_AC3_2CH_RK
        base_q = f"path=%2Flibrary%2Fmetadata%2F{rk}&directPlay=0&directStream=1"
        path = f"/video/:/transcode/universal/decision?{base_q}"

        baseline = get(pms, path)
        self.assertEqual(baseline["generalDecisionCode"], 1000)
        self.assertEqual(baseline["Metadata"], [])
        # a param of 0 is byte-identical to no param at all
        self.assertEqual(get(pms, path + "&normalizeLoudness=0"), baseline)
        self.assertEqual(get(pms, path + "&boostDialog=0&normalizeLoudness=0"), baseline)

        on = get(pms, path + "&normalizeLoudness=1")
        part = on["Metadata"][0]["Media"][0]["Part"][0]
        self.assertEqual(part["decision"], "transcode")
        video = next(s for s in part["Stream"] if s["streamType"] == 1)
        audio = next(s for s in part["Stream"] if s["streamType"] == 2)
        self.assertEqual(video["decision"], "copy")
        self.assertEqual(audio["decision"], "transcode")
        self.assertEqual(audio["codec"], "ac3")
        self.assertEqual(audio["channels"], 2, "source channel count is preserved")

        # the MDE shape with a live param mimics the same M1 outcome
        mde_q = f"path=%2Flibrary%2Fmetadata%2F{rk}&directPlay=1&directStreamAudio=1&boostDialog=1"
        mde = get(pms, f"/video/:/transcode/universal/decision?{mde_q}")
        mde_part = mde["Metadata"][0]["Media"][0]["Part"][0]
        self.assertEqual(mde_part["decision"], "transcode")
        mde_audio = next(s for s in mde_part["Stream"] if s["streamType"] == 2)
        self.assertEqual((mde_audio["decision"], mde_audio["codec"]), ("transcode", "ac3"))

        pms.refuse_enhancements = True
        refused = get(pms, path + "&normalizeLoudness=1")
        self.assertEqual(refused["generalDecisionCode"], 2000)
        self.assertEqual(refused["Metadata"], [])
        self.assertTrue(refused["transcodeDecisionText"])
        pms.refuse_enhancements = False

        pms.ignore_enhancements = True
        ignored = get(pms, path + "&normalizeLoudness=1")
        ig_part = ignored["Metadata"][0]["Media"][0]["Part"][0]
        self.assertEqual(ig_part["decision"], "transcode")
        ig_audio = next(s for s in ig_part["Stream"] if s["streamType"] == 2)
        self.assertEqual(ig_audio["decision"], "copy")

    def test_refuse_conversions_declines_only_what_asks_the_server_to_convert(self):
        rk = ENHANCEMENT_AC3_2CH_RK
        base = f"/video/:/transcode/universal/decision?path=%2Flibrary%2Fmetadata%2F{rk}"
        direct = base + "&hasMDE=1&directPlay=1&directStreamAudio=0"
        remux = base + "&directPlay=0&directStream=1"
        pms = MockPms(Library())
        self.assertIs(pms.refuse_conversions, False)
        self.assertEqual(get(pms, remux)["generalDecisionCode"], 1000, "off: as ever")
        pms.refuse_conversions = True
        self.assertEqual(get(pms, direct)["generalDecisionCode"], 1000, "the direct-play probe is unaffected")
        refused = get(pms, remux)
        self.assertEqual(refused["generalDecisionCode"], 2000)
        self.assertEqual(refused["Metadata"], [])
        self.assertTrue(refused["transcodeDecisionText"])
        status, _, body = pms.handle("POST", "/_mock/config", json.dumps({"refuse_conversions": False}).encode())
        self.assertEqual(status, 200)
        self.assertIs(json.loads(body)["refuse_conversions"], False)
        self.assertEqual(get(pms, remux)["generalDecisionCode"], 1000)

    @staticmethod
    def _requests(pms):
        status, ctype, body = pms.handle("GET", "/_mock/requests")
        assert status == 200 and ctype == "application/json", (status, ctype)
        return json.loads(body)

    def test_mock_config_runtime_toggle(self):
        pms = MockPms(Library())
        status, _, body = pms.handle(
            "POST", "/_mock/config", json.dumps({"refuse_enhancements": True}).encode())
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body), {
            "refuse_enhancements": True, "ignore_enhancements": False, "refuse_conversions": False,
            "plex_pass": True, "viewer_intro_skip": True, "viewer_credits_skip": True})
        self.assertIs(pms.refuse_enhancements, True)
        self.assertIs(pms.ignore_enhancements, False)

        status, _, body = pms.handle("POST", "/_mock/config", json.dumps({"bogus": True}).encode())
        self.assertEqual(status, 400)

        # a case can resolve once, then refuse only from the next decision on
        pms2 = MockPms(Library())
        rk = ENHANCEMENT_AC3_2CH_RK
        path = (f"/video/:/transcode/universal/decision?path=%2Flibrary%2Fmetadata%2F{rk}"
                "&directPlay=0&directStream=1&normalizeLoudness=1")
        self.assertEqual(get(pms2, path)["generalDecisionCode"], 1000)
        pms2.handle("POST", "/_mock/config", json.dumps({"refuse_enhancements": True}).encode())
        self.assertEqual(get(pms2, path)["generalDecisionCode"], 2000)

    def test_request_log(self):
        pms = MockPms(Library())
        pms.handle("GET", "/identity?X-Plex-Token=SECRET&foo=bar",
                  headers={"X-Plex-Session-Identifier": "sess-1"})
        log = self._requests(pms)
        entry = next(e for e in log if e["path"] == "/identity")
        self.assertEqual(entry["method"], "GET")
        self.assertEqual(entry["query"], {"foo": "bar"})
        self.assertNotIn("X-Plex-Token", entry["query"])
        self.assertEqual(entry["session"], "sess-1")

        status, _, _ = pms.handle("DELETE", "/_mock/requests")
        self.assertEqual(status, 200)
        after = self._requests(pms)
        self.assertEqual(len(after), 1)
        self.assertEqual((after[0]["method"], after[0]["path"]), ("GET", "/_mock/requests"))

    @unittest.skipUnless(
        HAS_FFMPEG_AND_FFPROBE,
        "needs ffmpeg+ffprobe; the host CI runner has neither",
    )
    def test_transcode_fixture_served(self):
        with tempfile.TemporaryDirectory() as tmp:
            clip = pathlib.Path(tmp) / "fixture.mkv"
            subprocess.check_call([
                "ffmpeg", "-y", "-v", "error", "-f", "lavfi", "-i",
                "color=size=64x64:rate=24:duration=1", "-f", "lavfi", "-i",
                "sine=frequency=330:duration=1", "-c:v", "libx264", "-c:a", "aac", "-t", "1",
                str(clip)])
            server, pms = serve(0, movies=0, transcode_fixture=clip)
            try:
                base = f"http://127.0.0.1:{server.server_address[1]}"
                url = base + "/video/:/transcode/universal/start.mkv?path=x"
                with urllib.request.urlopen(url, timeout=5) as response:
                    self.assertEqual(response.status, 200)
                    self.assertEqual(response.read(), clip.read_bytes())
                    self.assertEqual(response.headers["Accept-Ranges"], "bytes")
                request = urllib.request.Request(url, headers={"Range": "bytes=1-3"})
                with urllib.request.urlopen(request) as response:
                    self.assertEqual(response.status, 206)
                    self.assertEqual(response.read(), clip.read_bytes()[1:4])
                    self.assertTrue(response.headers["Content-Range"].startswith("bytes 1-3/"))
            finally:
                server.shutdown()
                server.server_close()

    def test_missing_transcode_fixture_file_is_rejected(self):
        with self.assertRaises(ValueError):
            serve(0, movies=0, transcode_fixture=pathlib.Path("/no/such/fixture.mkv"))


# The fields the detail page draws that the demo catalog can supply (docs: the S1a stage of the
# demo-library plan). Every one is asserted against what the MOCK serves, because a field the mock
# omits is a field the screenshots and the site video silently never show.
@unittest.skipUnless(HAS_DEMO_CACHE, "no derived demo cache (make demo-library) or no ffmpeg/ffprobe")
class CatalogDetailFields(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.lib = CatalogLibrary(CATALOG)
        cls.pms = MockPms(cls.lib)
        cls.catalog = json.loads(CATALOG.read_text())

    def detail(self, rk, pms=None):
        return get(pms or self.pms, f"/library/metadata/{rk}")["Metadata"][0]

    def rk_of(self, film):
        return int(self.lib.by_slug[film])

    def test_a_complete_title_serves_the_fields_the_detail_page_draws(self):
        for film in ("sintel", "tears-of-steel"):
            with self.subTest(film=film):
                rec = next(m for m in self.catalog["movies"] if m["id"] == film)
                d = self.detail(self.rk_of(film))
                self.assertEqual(d["tagline"], rec["tagline"])
                self.assertEqual([c["tag"] for c in d["Country"]], rec["countries"])
                self.assertEqual([w["tag"] for w in d["Writer"]], rec["writers"])
                self.assertEqual([r["tag"] for r in d["Role"]], [c["name"] for c in rec["cast"]])
                self.assertEqual([r.get("role", "") for r in d["Role"]],
                                 [c.get("role", "") for c in rec["cast"]])
                self.assertEqual([x["tag"] for x in d["Director"]], rec["directors"])

    def test_an_uncited_rating_is_never_served(self):
        # Wikidata holds no review score and no certification for these two, so the mock serves
        # none: the app then shows "Unrated" and no ratings row, not an invented number.
        for film in ("sintel", "tears-of-steel"):
            with self.subTest(film=film):
                d = self.detail(self.rk_of(film))
                self.assertNotIn("contentRating", d)
                self.assertEqual(d.get("Rating", []), [])
                for flat in ("rating", "audienceRating", "ratingImage", "audienceRatingImage"):
                    self.assertNotIn(flat, d)

    def test_the_related_hub_is_the_whole_list_so_no_row_waits_for_a_page(self):
        hubs = get(self.pms, f"/library/metadata/{self.rk_of('tears-of-steel')}/related")["Hub"]
        related = next(h for h in hubs if h["hubIdentifier"] == "related")
        self.assertFalse(related["more"])
        self.assertEqual(related["size"], related["totalSize"])
        self.assertEqual(len(related["Metadata"]), len(self.catalog["movies"]) - 1)
        self.assertTrue(all(m.get("thumb") for m in related["Metadata"]), "every card has a poster")

    def test_a_person_without_a_catalog_photo_is_served_without_one(self):
        # Only the catalog's `people` have a portrait; for everyone else the app draws its glyph.
        for film, jobs in (("sintel", ("Role", "Director", "Writer")), ("tears-of-steel", ("Director", "Writer"))):
            d = self.detail(self.rk_of(film))
            for job in jobs:
                for person in d[job]:
                    self.assertNotIn("thumb", person, (film, job, person))

    def test_a_person_with_a_catalog_photo_is_served_it_through_the_transcoder(self):
        d = self.detail(self.rk_of("tears-of-steel"))
        people = {p["tag"]: p for p in d["Role"]}
        self.assertEqual({name for name, p in people.items() if "thumb" in p}, set(self.catalog["people"]))
        self.assertEqual(len(people), 7, "the whole cast")
        seen = set()
        for name in self.catalog["people"]:
            ctype, body = self.lib.image(people[name]["thumb"], 300, 300)
            self.assertEqual(ctype, "image/jpeg")
            self.assertEqual(body[:2], b"\xff\xd8", name)
            seen.add(body)
        self.assertEqual(len(seen), len(self.catalog["people"]), "each person has their own picture")

    def _temp_library(self, edit):
        """A CatalogLibrary over a copy of the catalog that `edit` has changed."""
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        root = pathlib.Path(tmp.name)
        cat = json.loads(CATALOG.read_text())
        edit(cat)
        (root / "catalog.json").write_text(json.dumps(cat))
        shutil.copy(CATALOG.parent / "assets.json", root / "assets.json")
        lib = CatalogLibrary(root / "catalog.json")
        return lib, MockPms(lib)

    def test_a_cited_content_rating_and_ratings_are_served_as_a_server_sends_them(self):
        cited = {"source": "https://example.invalid/cite", "retrieved": "2026-10-07"}

        def edit(cat):
            m = next(m for m in cat["movies"] if m["id"] == "sintel")
            m["contentRating"] = dict(cited, value="PG")
            m["ratings"] = [dict(cited, image="rottentomatoes://image.rating.ripe", value=9.1, type="critic"),
                            dict(cited, image="imdb://image.rating", value=7.5, type="audience")]

        lib, pms = self._temp_library(edit)
        d = self.detail(int(lib.by_slug["sintel"]), pms)
        self.assertEqual(d["contentRating"], "PG")
        self.assertEqual(d["Rating"], [
            {"image": "rottentomatoes://image.rating.ripe", "value": 9.1, "type": "critic"},
            {"image": "imdb://image.rating", "value": 7.5, "type": "audience"}])
        self.assertEqual((d["rating"], d["ratingImage"]), (9.1, "rottentomatoes://image.rating.ripe"))
        self.assertEqual((d["audienceRating"], d["audienceRatingImage"]), (7.5, "imdb://image.rating"))

    def test_show_creators_are_writers_and_episodes_inherit_the_shows_credits(self):
        def edit(cat):
            show = next(s for s in cat["shows"] if s["id"] == "caminandes")
            show["creators"] = ["Pablo Vazquez"]
            show["cast"] = [{"name": "A Voice", "role": "Koro"}]
            show["countries"] = ["Netherlands"]

        lib, pms = self._temp_library(edit)
        show = self.detail(int(lib.by_slug["caminandes"]), pms)
        self.assertEqual([w["tag"] for w in show["Writer"]], ["Pablo Vazquez"])
        self.assertEqual([c["tag"] for c in show["Country"]], ["Netherlands"])
        ep = self.detail(int(lib.by_slug["caminandes/1/1"]), pms)
        for job in ("Director", "Writer", "Role"):
            self.assertEqual(ep[job], show[job], job)
        self.assertEqual([c["tag"] for c in ep["Role"]], ["A Voice"])


@unittest.skipUnless(HAS_DEMO_CACHE, "no derived demo cache (make demo-library) or no ffmpeg/ffprobe")
class CatalogImageLists(unittest.TestCase):
    """The demo library's rows carry a PMS-shaped `Image[]`, as a modern PMS sends one with every
    row. Without it the app reads a row as an older server's and asks the LEGACY
    `/library/metadata/<rk>/clearLogo` for every title, which this library answers with a 404 for
    each title in the catalog's `logo_none_approved` (Metropolis, in the Continue Watching deck);
    the frame dump of the site film fails closed on a failed art request, so the film's render
    died at its fifth frame on exactly that."""
    KINDS = ("movie", "show", "season", "episode")

    @classmethod
    def setUpClass(cls):
        cls.lib = CatalogLibrary(CATALOG)
        cls.pms = MockPms(cls.lib)
        cls.catalog = json.loads(CATALOG.read_text())

    def rows(self):
        return [it for it in self.lib.items.values() if it["type"] in self.KINDS]

    def title_rk(self, it):
        """The rating key of the TITLE a row's logo belongs to: a season's or an episode's is its show's."""
        return {"season": it.get("parentRatingKey"), "episode": it.get("grandparentRatingKey")}.get(
            it["type"], it["ratingKey"])

    def has_logo(self, it):
        slug = next(s for s, rk in self.lib.by_slug.items() if str(rk) == str(self.title_rk(it)))
        rec = next(r for r in self.catalog["movies"] + self.catalog["shows"] if r["id"] == slug)
        return "logo" in rec

    def test_every_row_carries_the_background_and_a_logo_only_where_its_title_has_one(self):
        for it in self.rows():
            with self.subTest(rk=it["ratingKey"], title=it["title"]):
                images = {i["type"]: i["url"] for i in it["Image"]}
                self.assertIn("coverPoster", images)
                self.assertIn("background", images)
                title = self.title_rk(it)
                if self.has_logo(it):
                    self.assertEqual(images["clearLogo"], f"/library/metadata/{title}/clearLogo/{self.lib.now}",
                                     "a STAMPED path naming the title's key, the show's for a season or an episode")
                else:
                    self.assertNotIn("clearLogo", images, "a title without a logo says so by listing none")
                self.assertEqual("snapshot" in images, it["type"] == "episode")

    def test_the_deck_has_a_list_on_every_row_and_metropolis_lists_no_logo(self):
        deck = self.lib.continue_watching()
        self.assertGreater(len(deck), 1)
        for it in deck:
            self.assertIn("Image", it, f"{it['title']}: a row with no list gets the legacy logo lookup")
        metropolis = next(it for it in deck if it["title"] == "Metropolis")
        self.assertIn("metropolis", self.catalog["logo_none_approved"])
        self.assertEqual({i["type"] for i in metropolis["Image"]}, {"coverPoster", "background"})

    def test_every_image_a_list_names_is_served(self):
        for it in self.rows():
            for image in it["Image"]:
                with self.subTest(rk=it["ratingKey"], type=image["type"]):
                    status, content_type, data = self.pms.handle("GET", image["url"])
                    self.assertEqual(status, 200, image["url"])
                    self.assertTrue(content_type.startswith("image/") and data)

    def test_the_stamped_logo_is_the_logo_the_unstamped_path_served(self):
        rk = self.lib.by_slug["sintel"]
        stamped = self.lib.image(f"/library/metadata/{rk}/clearLogo/{self.lib.now}", 600, 240)
        self.assertIsNotNone(stamped)
        self.assertEqual(stamped, self.lib.image(f"/library/metadata/{rk}/clearLogo", 600, 240),
                         "the logo the film and the stills draw is the same bytes under either path")


class DetailFixtures(unittest.TestCase):
    """`tests/demo_library/fixtures/detail-<rk>.json` is the mock's `/library/metadata/<rk>`
    answer for each fixture title, committed so the data crate can parse it
    (`rust-modules/data/src/metadata_demo_fixture_tests.rs`) without a derived-art cache."""
    TITLES = {"sintel": 102, "tears-of-steel": 105}

    def test_each_fixture_carries_every_field_the_catalog_supplies(self):
        cat = json.loads(CATALOG.read_text())
        for film, rk in self.TITLES.items():
            with self.subTest(film=film):
                rec = next(m for m in cat["movies"] if m["id"] == film)
                d = json.loads((FIXTURES / f"detail-{rk}.json").read_text())["MediaContainer"]["Metadata"][0]
                self.assertEqual(d["title"], rec["title"])
                self.assertEqual(d["tagline"], rec["tagline"])
                self.assertEqual([c["tag"] for c in d["Country"]], rec["countries"])
                self.assertEqual([w["tag"] for w in d["Writer"]], rec["writers"])
                self.assertEqual([x["tag"] for x in d["Director"]], rec["directors"])
                self.assertEqual([r["tag"] for r in d["Role"]], [c["name"] for c in rec["cast"]])

    @unittest.skipUnless(HAS_DEMO_CACHE, "no derived demo cache (make demo-library) or no ffmpeg/ffprobe")
    def test_each_fixture_is_what_the_mock_serves_now(self):
        pms = MockPms(CatalogLibrary(CATALOG))
        for film, rk in self.TITLES.items():
            with self.subTest(film=film):
                status, _, body = pms.handle("GET", f"/library/metadata/{rk}")
                self.assertEqual(status, 200)
                self.assertEqual(json.loads(body), json.loads((FIXTURES / f"detail-{rk}.json").read_text()),
                                 "regenerate with `python3 tools/demo_library.py fixtures`")


def first_of(lib, kind):
    return next(it for it in lib.items.values() if it["type"] == kind)


def window(path, start, size, headers=False):
    """`path` paged by the container window, as a query parameter or, with `headers`, as headers."""
    if headers:
        return path, {"X-Plex-Container-Start": str(start), "X-Plex-Container-Size": str(size)}
    sep = "&" if "?" in path else "?"
    return f"{path}{sep}X-Plex-Container-Start={start}&X-Plex-Container-Size={size}", None


def fetch(pms, path, headers=None):
    status, ctype, data = pms.handle("GET", path, headers=headers)
    assert (status, ctype) == (200, "application/json"), (status, ctype, path)
    return json.loads(data)["MediaContainer"]


class ContainerPaging(unittest.TestCase):
    """The PMS paging contract (docs/pms-api.md, "Paging, observed"): a request pages only when it
    carries the container window, as a query parameter or as a header; a paged answer echoes
    `offset` and carries the listing's `totalSize`; an unpaged answer carries neither."""

    def assertPaged(self, container, start, rows, total):
        self.assertEqual(container["totalSize"], total)
        self.assertEqual(container["offset"], start)
        self.assertEqual(container["size"], len(rows))

    def test_unpaged_answers_carry_no_total_and_no_offset(self):
        lib = Library()
        pms = MockPms(lib)
        show = first_of(lib, "show")
        season = next(it for it in lib.items.values() if it["type"] == "season")
        title = first_of(lib, "movie")["title"]
        for path in ("/library/sections/1/all", f"/library/metadata/{show['ratingKey']}/children",
                     f"/library/metadata/{season['ratingKey']}/children",
                     f"/library/metadata/{first_of(lib, 'movie')['ratingKey']}/similar",
                     "/hubs/continueWatching/items", "/hubs", "/hubs/sections/1",
                     f"/library/search?query={title}&searchTypes=movies"):
            with self.subTest(path=path):
                container = fetch(pms, path)
                self.assertNotIn("totalSize", container)
                self.assertNotIn("offset", container)

    def test_a_size_without_a_start_is_ignored(self):
        """docs/pms-api.md, "Paging gotcha": a query-param Size without Start changes nothing."""
        pms = MockPms(Library())
        ignored = fetch(pms, "/library/sections/1/all?X-Plex-Container-Size=5")
        self.assertEqual(len(ignored["Metadata"]), 48)
        self.assertNotIn("totalSize", ignored)
        self.assertNotIn("offset", ignored)

    def test_window_as_query_and_as_header_answer_alike(self):
        pms = MockPms(Library())
        by_query = fetch(pms, window("/library/sections/1/all", 10, 5)[0])
        by_header = fetch(pms, *window("/library/sections/1/all", 10, 5, headers=True))
        self.assertEqual(by_header, by_query)
        self.assertPaged(by_query, 10, by_query["Metadata"], 48)
        self.assertEqual(len(by_query["Metadata"]), 5)

    def test_size_zero_is_a_paged_request_that_reads_the_total(self):
        pms = MockPms(Library())
        page = fetch(pms, window("/library/sections/1/all", 0, 0)[0])
        self.assertEqual(page["Metadata"], [])
        self.assertPaged(page, 0, [], 48)

    def test_start_past_the_end_is_empty_and_still_echoes_the_offset(self):
        pms = MockPms(Library())
        page = fetch(pms, window("/library/sections/1/all", 500, 10)[0])
        self.assertEqual(page["Metadata"], [])
        self.assertPaged(page, 500, [], 48)

    def test_children_of_a_show_and_of_a_season_page(self):
        lib = Library()
        pms = MockPms(lib)
        show = first_of(lib, "show")["ratingKey"]
        season = next(it["ratingKey"] for it in lib.items.values() if it["type"] == "season")
        for parent in (show, season):
            with self.subTest(parent=parent):
                full = fetch(pms, f"/library/metadata/{parent}/children")["Metadata"]
                self.assertGreaterEqual(len(full), 2)
                page = fetch(pms, window(f"/library/metadata/{parent}/children", 1, 1)[0])
                self.assertEqual(page["Metadata"], full[1:2])
                self.assertPaged(page, 1, full[1:2], len(full))

    def test_extras_page_and_default_library_has_none(self):
        lib = Library(extras=3)
        pms = MockPms(lib)
        movie = first_of(lib, "movie")["ratingKey"]
        full = fetch(pms, f"/library/metadata/{movie}/extras")["Metadata"]
        self.assertEqual(len(full), 3)
        self.assertEqual({row["type"] for row in full}, {"clip"})
        self.assertEqual(len({row["ratingKey"] for row in full}), 3)
        self.assertNotIn("totalSize", fetch(pms, f"/library/metadata/{movie}/extras"))
        page = fetch(pms, window(f"/library/metadata/{movie}/extras", 1, 2)[0])
        self.assertEqual(page["Metadata"], full[1:3])
        self.assertPaged(page, 1, full[1:3], 3)
        default = MockPms(Library())
        self.assertEqual(fetch(default, f"/library/metadata/{movie}/extras")["Metadata"], [])

    def test_person_media_pages(self):
        lib = Library()
        pms = MockPms(lib)
        pid = max(lib.people, key=lambda p: len(lib.person_media(p)))
        self.assertGreaterEqual(len(lib.person_media(pid)), 2, "the fixture gives a person two credits")
        full = fetch(pms, f"/library/people/{pid}/media")["Metadata"]
        self.assertEqual(full, lib.person_media(pid))
        page = fetch(pms, window(f"/library/people/{pid}/media", 1, 1)[0])
        self.assertEqual(page["Metadata"], full[1:2])
        self.assertPaged(page, 1, full[1:2], len(full))

    def test_similar_pages_over_the_same_type_without_the_item_itself(self):
        lib = Library()
        pms = MockPms(lib)
        movie = first_of(lib, "movie")
        full = fetch(pms, f"/library/metadata/{movie['ratingKey']}/similar")["Metadata"]
        self.assertEqual(len(full), sum(1 for it in lib.items.values() if it["type"] == "movie") - 1)
        self.assertNotIn(movie["ratingKey"], {row["ratingKey"] for row in full})
        self.assertEqual({row["type"] for row in full}, {"movie"})
        page = fetch(pms, window(f"/library/metadata/{movie['ratingKey']}/similar", 2, 3)[0])
        self.assertEqual(page["Metadata"], full[2:5])
        self.assertPaged(page, 2, full[2:5], len(full))

    def test_continue_watching_items_page_contiguously(self):
        lib = Library()
        pms = MockPms(lib)
        # The default library already has in-progress items; the test sets its own five.
        for it in lib.items.values():
            it["viewOffset"] = 0
        for n, movie in enumerate([it for it in lib.items.values() if it["type"] == "movie"][:5]):
            movie["viewOffset"] = 1000
            movie["lastViewedAt"] = 1_700_000_000 + n
        full = lib.continue_watching()
        self.assertEqual(len(full), 5)
        first = fetch(pms, window("/hubs/continueWatching/items", 0, 3)[0])
        second = fetch(pms, window("/hubs/continueWatching/items", 3, 3)[0])
        self.assertPaged(first, 0, first["Metadata"], 5)
        self.assertPaged(second, 3, second["Metadata"], 5)
        self.assertEqual(first["Metadata"] + second["Metadata"], full)

    def test_hubs_page_by_hub_on_both_hub_listings(self):
        lib = Library()
        pms = MockPms(lib)
        pms.home_hubs = 6
        full = fetch(pms, "/hubs")["Hub"]
        self.assertEqual(len(full), 6)
        page = fetch(pms, window("/hubs", 2, 3)[0])
        self.assertEqual(page["Hub"], full[2:5])
        self.assertPaged(page, 2, full[2:5], 6)
        header_page = fetch(pms, *window("/hubs", 2, 3, headers=True))
        self.assertEqual(header_page, page)
        section = fetch(pms, "/hubs/sections/1")["Hub"]
        self.assertEqual(len(section), 2)
        section_page = fetch(pms, window("/hubs/sections/1", 1, 1)[0])
        self.assertEqual(section_page["Hub"], section[1:2])
        self.assertPaged(section_page, 1, section[1:2], 2)

    def test_library_search_rows_page_by_offset_and_limit_caps_the_total(self):
        lib = Library()
        pms = MockPms(lib)
        prefix = first_of(lib, "movie")["title"][:2]
        hits = [it for it in lib.items.values() if it["type"] == "movie" and it["title"].startswith(prefix)]
        self.assertGreaterEqual(len(hits), 2, "the prefix matches at least two movies")
        path = f"/library/search?query={prefix}&searchTypes=movies"
        full = fetch(pms, path)["SearchResult"]
        self.assertEqual(len(full), len(hits))
        for row in full:
            self.assertEqual(set(row), {"Metadata", "score"})
            self.assertTrue(row["Metadata"]["title"].startswith(prefix))
        page = fetch(pms, window(path, 1, 1)[0])
        self.assertEqual(page["SearchResult"], full[1:2])
        self.assertPaged(page, 1, full[1:2], len(full))
        capped = fetch(pms, window(f"{path}&limit=1", 0, 5)[0])
        self.assertEqual(len(capped["SearchResult"]), 1)
        self.assertPaged(capped, 0, capped["SearchResult"], 1)

    def test_library_search_types_order_and_unknown_kinds(self):
        lib = Library()
        pms = MockPms(lib)
        # Known titles under one prefix, so the order each type lists in is what is asserted.
        movie = first_of(lib, "movie")
        show = first_of(lib, "show")
        episode = next(it for it in lib.items.values() if it["type"] == "episode")
        movie["title"], show["title"], episode["title"] = "s0000abc0", "s0000abc1", "s0000abc2"
        person = lib.people[1]
        person["tag"] = "s0000abc9"
        query = "s0000abc"

        def titles(kinds):
            rows = fetch(pms, f"/library/search?query={query}&searchTypes={kinds}")["SearchResult"]
            return [row["Metadata"]["title"] for row in rows]

        self.assertEqual(titles("tv"), ["s0000abc1", "s0000abc2"], "shows first, then episodes")
        self.assertEqual(titles("movies,tv"), ["s0000abc0", "s0000abc1", "s0000abc2"],
                         "one mixed list in movies, shows, episodes order")
        people = fetch(pms, f"/library/search?query={query}&searchTypes=people")["SearchResult"]
        self.assertEqual(len(people), 1)
        self.assertEqual(people[0]["Directory"]["tagKey"], person["tagKey"])
        self.assertEqual(people[0]["Directory"]["count"], len(lib.person_media(person["id"])))
        self.assertIn("score", people[0])
        for kinds in ("shows", "bogus", "music"):
            with self.subTest(searchTypes=kinds):
                empty = fetch(pms, window(f"/library/search?query={query}&searchTypes={kinds}", 0, 10)[0])
                self.assertEqual(empty["SearchResult"], [])
                self.assertPaged(empty, 0, [], 0)


class BifStills(unittest.TestCase):
    """`/library/parts/<part>/indexes/sd/<ms>` through the photo transcoder: the scrub preview's
    stills."""

    @unittest.skipUnless(HAS_FFMPEG_AND_FFPROBE, "no ffmpeg/ffprobe")
    def test_a_part_with_a_file_answers_a_jpeg_in_the_films_aspect_and_one_without_404s(self):
        with tempfile.TemporaryDirectory() as tmp:
            film = pathlib.Path(tmp) / "film.mkv"
            subprocess.run(["ffmpeg", "-v", "error", "-f", "lavfi", "-i",
                            "testsrc=size=480x200:rate=5:duration=25", "-pix_fmt", "yuv420p", str(film)],
                           check=True)
            lib = object.__new__(CatalogLibrary)
            lib.__dict__.update(Library(movies=3).__dict__)
            lib._scaled, lib._lock = {}, __import__("threading").Lock()
            lib.media_files = {7459: film}
            url = "/library/parts/7459/indexes/sd/14000"
            ctype, data = lib.image(url, "320", "180")
            self.assertEqual((ctype, data[:2]), ("image/jpeg", b"\xff\xd8"))
            probe = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "stream=width,height",
                                    "-of", "csv=p=0", "-"], input=data, capture_output=True)
            self.assertEqual(probe.stdout.decode().strip(), "320,134", "320 wide, the film's 2.4:1 aspect")
            # the nearest 10 s still: 14 s and 10 s are the same picture
            self.assertEqual(lib.image("/library/parts/7459/indexes/sd/10000", "320", "180")[1], data)
            self.assertIsNone(lib.image("/library/parts/7460/indexes/sd/0", "320", "180"), "no file: 404")
            self.assertIsNone(lib.image("/library/parts/7459/indexes/sd/9999999", "320", "180"),
                              "past the end: 404")

    def test_without_any_media_file_there_is_no_index(self):
        lib = object.__new__(CatalogLibrary)
        lib.__dict__.update(Library(movies=3).__dict__)
        lib._scaled, lib._lock = {}, __import__("threading").Lock()
        lib.media_files = {}
        self.assertIsNone(lib.image("/library/parts/7459/indexes/sd/0", "320", "180"))


def png_info(data):
    """(width, height, colour type) of a PNG, after checking its signature."""
    assert data[:8] == b"\x89PNG\r\n\x1a\n", data[:8]
    width, height = struct.unpack(">II", data[16:24])
    return width, height, data[25]


def png_alphas(data):
    """The alpha byte of every pixel of an RGBA PNG (filter 0 rows, as the mock writes them)."""
    width, height, ctype = png_info(data)
    assert ctype == 6, ctype
    idat, at = b"", 8
    while at < len(data):
        size, tag = struct.unpack(">I4s", data[at:at + 8])
        if tag == b"IDAT":
            idat += data[at + 8:at + 8 + size]
        at += 12 + size
    raw = zlib.decompress(idat)
    stride = 1 + width * 4
    return [raw[y * stride + 1 + x * 4 + 3] for y in range(height) for x in range(width)]


def transcode(pms, url, width, height):
    quoted = urllib.parse.quote(url, safe="")
    return pms.handle("GET", f"/photo/:/transcode?width={width}&height={height}&minSize=1&url={quoted}")


def discover_record(pms, hx):
    """The Discover stand-in's metadata record of catalog id `hx`."""
    status, ctype, data = pms.handle("GET", f"/library/metadata/{hx}", b"", {})
    assert (status, ctype) == (200, "application/json"), (hx, status)
    return json.loads(data)["MediaContainer"]["Metadata"][0]


class CwArtFixture(unittest.TestCase):
    """`--cw-art-fixture`: the Continue Watching items of the 420x236 card's art paths. Off by default,
    and then no generated byte moves."""

    CATALOG_ID = re.compile(r"plex://(movie|show)/[0-9a-f]{24}")
    NAMES = ["North Ridge", "Lumen Drift", "The Quiet Harbour", "Brass Hours", "Tin Lighthouse"]

    def make(self, role="primary"):
        pms = MockPms(Library(seed=1, movies=12, shows=3, seasons=1, episodes=2))
        add_cw_art_fixture(pms, role)
        return pms

    def deck(self, pms):
        hub = get(pms, "/hubs/continueWatching?count=12")["Hub"][0]
        return {row.get("grandparentTitle") or row["title"]: row for row in hub["Metadata"]}

    def test_off_by_default_and_the_generated_library_does_not_move(self):
        plain, fixture = MockPms(Library(seed=1, movies=12, shows=3, seasons=1, episodes=2)), self.make()
        self.assertEqual(plain.lib.cw_art, {})
        self.assertNotIn("cw_art", plain.lib.__dict__)
        self.assertNotIn("cw_extra", plain.lib.__dict__)
        self.assertEqual([r for r in fixture.lib.continue_watching() if int(r["ratingKey"]) < 940000],
                         plain.lib.continue_watching())
        for rk, item in plain.lib.items.items():
            self.assertEqual(item, fixture.lib.items[rk], rk)
        self.assertEqual(len(plain.discover.catalog), 16)

    def test_the_five_cards_carry_the_owners_labels_and_head_the_deck(self):
        pms = self.make()
        rows = pms.lib.continue_watching()[:5]
        self.assertEqual([r.get("grandparentTitle") or r["title"] for r in rows], self.NAMES)

        def left(row):
            return (row["duration"] - row.get("viewOffset", 0)) // 60_000

        deck = self.deck(pms)
        north = deck["North Ridge"]
        self.assertEqual((north["parentIndex"], north["index"], left(north)), (2, 4, 38))
        self.assertEqual(round(100 * north["viewOffset"] / north["duration"]), 60)
        lumen = deck["Lumen Drift"]
        self.assertEqual((lumen["type"], left(lumen)), ("movie", 72))  # 1h 12m
        self.assertEqual(round(100 * lumen["viewOffset"] / lumen["duration"]), 35)
        harbour = deck["The Quiet Harbour"]
        self.assertEqual((harbour["parentIndex"], harbour["index"], left(harbour)), (1, 7, 41))
        brass = deck["Brass Hours"]  # on deck: a new episode, no resume point
        self.assertEqual((brass["parentIndex"], brass["index"]), (2, 1))
        self.assertNotIn("viewOffset", brass)
        self.assertEqual(deck["Tin Lighthouse"]["type"], "movie")

    def test_the_section_hubs_list_them_in_their_own_sections(self):
        pms = self.make()
        shows = get(pms, "/hubs/sections/2")["Hub"][0]["Metadata"]
        movies = get(pms, "/hubs/sections/1")["Hub"][0]["Metadata"]
        self.assertEqual([r["grandparentTitle"] for r in shows[:3]], ["North Ridge", "The Quiet Harbour", "Brass Hours"])
        self.assertEqual([r["title"] for r in movies[:2]], ["Lumen Drift", "Tin Lighthouse"])

    def test_catalog_ids_sit_on_the_dedicated_items_only(self):
        pms = self.make()
        deck = self.deck(pms)
        self.assertRegex(deck["Lumen Drift"]["guid"], self.CATALOG_ID)
        episodes = ("North Ridge", "The Quiet Harbour", "Brass Hours")
        for name in episodes:
            # An episode's own guid is no catalog id; its show's is, and is the cover's key.
            self.assertTrue(deck[name]["guid"].startswith("plex://episode/"), name)
            self.assertRegex(deck[name]["grandparentGuid"], self.CATALOG_ID)
            show = get(pms, f"/library/metadata/{deck[name]['grandparentRatingKey']}")["Metadata"][0]
            self.assertEqual(show["guid"], deck[name]["grandparentGuid"])
        self.assertEqual(len({deck[n]["grandparentGuid"] for n in episodes}), 3)
        tin = deck["Tin Lighthouse"]  # the guid-less item
        self.assertEqual(tin["guid"][:8], "local://")
        self.assertNotRegex(tin["guid"], self.CATALOG_ID)
        # Everything else keeps the guid it had: no generated item gained a 24-hex id from the fixture.
        plain = MockPms(Library(seed=1, movies=12, shows=3, seasons=1, episodes=2))
        for rk, item in plain.lib.items.items():
            self.assertEqual(item.get("guid"), pms.lib.items[rk].get("guid"), rk)

    def test_a_fixture_title_is_watchlist_eligible_and_a_blanket_bind_leaves_it(self):
        pms = self.make()
        deck = self.deck(pms)
        guid = deck["Lumen Drift"]["guid"]
        found = pms.handle("GET", "/library/all?type=1&guid=" + urllib.parse.quote(guid, safe=""), b"", {})
        self.assertEqual([r["title"] for r in json.loads(found[2])["MediaContainer"]["Metadata"]], ["Lumen Drift"])
        pms.discover.configure({"guids": "all"}, pms.lib)
        pms.discover.configure({"episode_shows": "catalog"}, pms.lib)
        after = self.deck(pms)
        self.assertEqual(after["Lumen Drift"]["guid"], guid)
        self.assertEqual(after["Tin Lighthouse"]["guid"], deck["Tin Lighthouse"]["guid"])
        for name in ("North Ridge", "The Quiet Harbour", "Brass Hours"):
            self.assertEqual(after[name]["grandparentGuid"], deck[name]["grandparentGuid"])

    def test_the_pms_image_list_names_the_art_each_item_has(self):
        deck = self.deck(self.make())

        def kinds(row):
            return {i["type"]: i["url"] for i in row["Image"]}

        north = kinds(deck["North Ridge"])
        self.assertEqual(set(north), {"coverPoster", "background", "clearLogo", "snapshot"})
        # An episode names its SHOW's background and clearLogo (the episode's own is the still).
        self.assertIn(f"/library/metadata/{deck['North Ridge']['grandparentRatingKey']}/clearLogo/", north["clearLogo"])
        self.assertIn(f"/library/metadata/{deck['North Ridge']['ratingKey']}/", north["snapshot"])
        self.assertEqual(north["background"], deck["North Ridge"]["art"])
        lumen = kinds(deck["Lumen Drift"])
        self.assertEqual(set(lumen), {"coverPoster", "background", "clearLogo", "snapshot"})
        self.assertRegex(lumen["clearLogo"], r"^/library/metadata/940101/clearLogo/\d+$")  # stamped
        self.assertNotIn("clearLogo", kinds(deck["Brass Hours"]))  # the title-in-type item has none
        self.assertIn("clearLogo", kinds(deck["The Quiet Harbour"]))  # the composite item has one
        self.assertIn("clearLogo", kinds(deck["Tin Lighthouse"]))

    def test_the_art_is_synthetic_png_scaled_like_the_photo_transcoder(self):
        pms = self.make()
        deck = self.deck(pms)
        for name in ("Lumen Drift", "Tin Lighthouse"):
            status, ctype, data = transcode(pms, deck[name]["art"], 420, 236)
            self.assertEqual((status, ctype, png_info(data)), (200, "image/png", (420, 236, 2)), name)
        # A clearLogo asked at 600x240 comes back covering the box at its own 5:1: 1200x240, PNG-alpha.
        logo_path = {i["type"]: i["url"] for i in deck["Lumen Drift"]["Image"]}["clearLogo"]
        status, ctype, data = transcode(pms, logo_path, 600, 240)
        self.assertEqual((status, ctype, png_info(data)), (200, "image/png", (1200, 240, 6)))
        alphas = png_alphas(data)
        self.assertEqual(alphas[0], 0)  # transparent corner
        self.assertIn(255, alphas)  # and an opaque mark
        # The same bytes direct, by the item's own path.
        self.assertEqual(pms.handle("GET", logo_path + "?width=600&height=240")[2], data)
        # A 16:9 snapshot and a 2:3 poster.
        snap = {i["type"]: i["url"] for i in deck["North Ridge"]["Image"]}["snapshot"]
        self.assertEqual(png_info(transcode(pms, snap, 420, 236)[2])[:2], (420, 236))
        self.assertEqual(png_info(transcode(pms, deck["Lumen Drift"]["thumb"], 250, 375)[2])[:2], (250, 375))
        # Two titles do not share art.
        self.assertNotEqual(transcode(pms, deck["Lumen Drift"]["art"], 420, 236)[2],
                            transcode(pms, deck["Tin Lighthouse"]["art"], 420, 236)[2])

    def test_a_title_without_a_clearlogo_answers_404_for_it_direct_and_through_the_transcoder(self):
        pms = self.make()
        brass = self.deck(pms)["Brass Hours"]
        logo = f"/library/metadata/{brass['grandparentRatingKey']}/clearLogo/1700000003"
        self.assertEqual(pms.handle("GET", logo)[0], 404)
        self.assertEqual(transcode(pms, logo, 600, 240)[0], 404)
        # Its background and poster are there.
        self.assertEqual(transcode(pms, brass["art"], 420, 236)[0], 200)
        self.assertEqual(transcode(pms, brass["grandparentThumb"], 250, 375)[0], 200)

    def test_a_cover_is_a_gradient_and_its_title_wordmark_and_no_stray_strip_above_it(self):
        """The fixture's cover once also carried a thin light strip near its top-left corner: on a
        card it read as a UI element (a dash at the corner of the first Continue Watching tile) that
        the owner's boards do not have. The title is baked into the lower part; nothing above it is
        lighter than the gradient can be."""
        pms = self.make()
        deck = self.deck(pms)
        hx = deck["North Ridge"]["grandparentGuid"].rsplit("/", 1)[1]
        pms.discover.art_base = "http://127.0.0.1:41999"
        data = pms.handle("GET", f"/_mock/art/{hx}/coverArt.png?width=420&height=236")[2]
        w, h, channels = png_info(data)
        self.assertEqual((w, h, channels), (420, 236, 2))
        idat, at = b"", 8
        while at < len(data):
            n, tag = struct.unpack(">I4s", data[at:at + 8])
            if tag == b"IDAT":
                idat += data[at + 8:at + 8 + n]
            at += 12 + n
        raw = zlib.decompress(idat)
        stride = 1 + w * 3
        rows = [raw[y * stride + 1:(y + 1) * stride] for y in range(h)]  # filter 0 rows, RGB
        top = max(max(row) for row in rows[: h * 2 // 5])
        self.assertLess(top, 200, "a near-white pixel above the title wordmark: a stray strip on the cover")

    def test_discover_answers_a_cover_for_the_cover_title_at_an_absolute_url_the_mock_serves(self):
        pms = self.make()
        pms.discover.art_base = "http://127.0.0.1:41999"
        deck = self.deck(pms)
        hx = deck["North Ridge"]["grandparentGuid"].rsplit("/", 1)[1]
        covers = [i for i in discover_record(pms, hx)["Image"] if i["type"] == "coverArt"]
        self.assertEqual(len(covers), 1)
        url = covers[0]["url"]
        self.assertEqual(url, f"http://127.0.0.1:41999/_mock/art/{hx}/coverArt.png")
        # The photo transcoder takes it as `url=`, and resolves it to a 16:9 PNG at the box asked.
        status, ctype, data = transcode(pms, url, 420, 236)
        self.assertEqual((status, ctype, png_info(data)), (200, "image/png", (420, 236, 2)))
        # It is also fetchable directly, the same bytes; and it differs from the same title's background.
        self.assertEqual(pms.handle("GET", f"/_mock/art/{hx}/coverArt.png?width=420&height=236")[2], data)
        self.assertNotEqual(transcode(pms, deck["North Ridge"]["art"], 420, 236)[2], data)
        # The film's record carries one too, under its own catalog id.
        lumen = deck["Lumen Drift"]["guid"].rsplit("/", 1)[1]
        self.assertTrue(any(i["type"] == "coverArt" for i in discover_record(pms, lumen)["Image"]))

    def test_discover_answers_no_cover_in_two_shapes_and_none_for_the_guidless_item(self):
        pms = self.make()
        deck = self.deck(pms)
        harbour = deck["The Quiet Harbour"]["grandparentGuid"].rsplit("/", 1)[1]
        record = discover_record(pms, harbour)
        self.assertTrue(record["Image"])  # a body with an Image[]...
        self.assertNotIn("coverArt", [i["type"] for i in record["Image"]])  # ...and no cover in it
        self.assertEqual(pms.handle("GET", f"/_mock/art/{harbour}/coverArt.png")[0], 404)
        brass = deck["Brass Hours"]["grandparentGuid"].rsplit("/", 1)[1]
        self.assertNotIn("Image", discover_record(pms, brass))  # a body with no Image key at all
        self.assertEqual(transcode(pms, f"http://127.0.0.1:1/_mock/art/{brass}/coverArt.png", 420, 236)[0], 404)
        # Tin Lighthouse has no catalog id, so there is nothing to read.
        self.assertIsNone(pms.discover.by_hex(deck["Tin Lighthouse"]["guid"].rsplit("/", 1)[1]))
        self.assertEqual(pms.handle("GET", "/_mock/art/" + "0" * 24 + "/coverArt.png")[0], 404)

    def test_the_twin_server_holds_only_the_shared_title_under_its_own_rating_key(self):
        twin = self.make("twin")
        rows = [r for r in twin.lib.continue_watching() if int(r["ratingKey"]) >= 940000]
        self.assertEqual([r["title"] for r in rows], ["Lumen Drift"])
        mine = self.deck(self.make())["Lumen Drift"]
        self.assertEqual(mine["guid"], rows[0]["guid"])
        self.assertNotEqual(mine["ratingKey"], rows[0]["ratingKey"])
        self.assertEqual(rows[0]["ratingKey"], "949001")
        self.assertEqual(transcode(twin, rows[0]["art"], 420, 236)[0], 200)

    def test_one_signed_in_account_lists_both_servers_over_the_real_http_handler(self):
        twin_server, twin = serve(0, seed=2, movies=0, shows=1, seasons=1, episodes=1, cw_art_fixture="twin")
        server, pms = serve(0, seed=1, movies=4, shows=1, seasons=1, episodes=1, cw_art_fixture="primary",
                            cw_art_twin=[twin_server.server_address[1]])
        try:
            base = f"http://127.0.0.1:{server.server_address[1]}"
            with urllib.request.urlopen(base + "/api/v2/resources", timeout=5) as response:
                resources = json.load(response)
            self.assertEqual([r["clientIdentifier"] for r in resources], [pms.lib.machine, twin.lib.machine])
            self.assertNotEqual(pms.lib.machine, twin.lib.machine)
            self.assertEqual(resources[1]["connections"][0]["port"], twin_server.server_address[1])
            # The cover's url names the primary's own listener, and it answers over HTTP.
            hx = self.deck(pms)["North Ridge"]["grandparentGuid"].rsplit("/", 1)[1]
            with urllib.request.urlopen(base + f"/library/metadata/{hx}", timeout=5) as response:
                url = json.load(response)["MediaContainer"]["Metadata"][0]["Image"][0]["url"]
            self.assertTrue(url.startswith(base + "/_mock/art/"), url)
            with urllib.request.urlopen(url, timeout=5) as response:
                self.assertEqual(png_info(response.read())[:2], (1280, 720))
            # A twin that is not there leaves one server.
            twin_server.shutdown()
            twin_server.server_close()
            with urllib.request.urlopen(base + "/api/v2/resources", timeout=5) as response:
                self.assertEqual(len(json.load(response)), 1)
        finally:
            server.shutdown()
            server.server_close()

    @unittest.skipUnless(HAS_DEMO_CACHE, "no derived demo cache (make demo-library) or no ffmpeg/ffprobe")
    def test_the_demo_catalog_takes_the_fixture_too_and_keeps_its_own_guids(self):
        plain = MockPms(CatalogLibrary(CATALOG))
        pms = MockPms(CatalogLibrary(CATALOG))
        add_cw_art_fixture(pms)
        deck = self.deck(pms)
        self.assertEqual(list(deck)[:5], self.NAMES)
        self.assertRegex(deck["Lumen Drift"]["guid"], self.CATALOG_ID)
        for rk, item in plain.lib.items.items():
            self.assertEqual(item["guid"], pms.lib.items[rk]["guid"], rk)
            self.assertNotRegex(item["guid"], self.CATALOG_ID)
        self.assertEqual(transcode(pms, deck["Lumen Drift"]["art"], 420, 236)[0], 200)


class PmsAudioVocabulary(unittest.TestCase):
    """The mock speaks PMS's codec names, not FFmpeg's: DTS is `dca` on the wire."""

    def test_dts_is_dca_on_the_wire_and_other_codecs_pass_through(self):
        from mock_pms import pms_audio_codec
        self.assertEqual(pms_audio_codec("dts"), "dca")
        self.assertEqual([pms_audio_codec(c) for c in ("ac3", "eac3", "aac", "truehd")],
                         ["ac3", "eac3", "aac", "truehd"])
        stream = Library(movies=1)._stream_wire({"codec_type": "audio", "codec_name": "dts", "index": 1}, 11)
        self.assertEqual(stream["codec"], "dca")
        media = Library(movies=1)._media(__import__("random").Random(1), 5, 50, 1000, acodec="dts")[0]
        self.assertEqual(media["audioCodec"], "dca")
        self.assertEqual(media["Part"][0]["Stream"][1]["codec"], "dca")


if __name__ == "__main__":
    unittest.main()
