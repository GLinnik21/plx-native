"""The mock's Discover stand-in (docs/watchlist-plan.md W45): wire shapes the Rust client reads
(`plex::watchlist`), the fixture's tile states, and in-memory add/remove.

Run: python3 -I tests/test_mock_pms_discover.py   (-I drops the script dir, so it is added below)
"""
import json
import pathlib
import sys
import unittest
import urllib.parse

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

from mock_pms import DiscoverFixture, Library, MockPms  # noqa: E402

LIST = "/library/sections/watchlist/all?includeCollections=1&includeExternalMedia=1&sort=watchlistedAt:desc"


def make():
    return MockPms(Library(seed=3, movies=24, shows=6, seasons=1, episodes=2))


def call(pms, method, path):
    status, ctype, data = pms.handle(method, path, b"", {})
    return status, ctype, data


def jcall(pms, method, path, want=200):
    status, ctype, data = call(pms, method, path)
    assert status == want, (path, status, data[:200])
    assert ctype == "application/json"
    return json.loads(data)["MediaContainer"]


def page(pms, variant="all", start=0, size=100):
    return jcall(pms, "GET", f"/library/sections/watchlist/{variant}?X-Plex-Container-Start={start}"
                             f"&X-Plex-Container-Size={size}")


class DiscoverMock(unittest.TestCase):
    def test_list_pages_and_caps_size_at_100(self):
        pms = make()
        full = page(pms)
        self.assertEqual(full["totalSize"], 14)
        self.assertEqual(full["size"], 14)
        self.assertEqual(full["librarySectionID"], "watchlist")
        self.assertTrue(all("watchlistedAt" not in m for m in full["Metadata"]))
        second = page(pms, start=5, size=5)
        self.assertEqual([m["ratingKey"] for m in second["Metadata"]],
                         [m["ratingKey"] for m in full["Metadata"][5:10]])
        self.assertEqual(second["offset"], 5)
        self.assertEqual(call(pms, "GET", LIST + "&X-Plex-Container-Start=0&X-Plex-Container-Size=101")[0], 400)

    def test_ids_are_24_hex_and_guids_carry_them(self):
        for m in page(make())["Metadata"]:
            self.assertRegex(m["ratingKey"], r"^[0-9a-f]{24}$")
            self.assertEqual(m["guid"], f"plex://{m['type']}/{m['ratingKey']}")

    def test_released_variant_drops_the_unreleased(self):
        pms = make()
        released = page(pms, "released")
        unreleased = {c["hex"] for c in pms.discover.catalog if not c["released"]}
        self.assertEqual(len(unreleased), 3)
        self.assertEqual(released["totalSize"], 11)
        self.assertFalse(unreleased & {m["ratingKey"] for m in released["Metadata"]})

    def test_fixture_covers_the_states_one_mock_can_produce(self):
        pms = make()
        held = {}
        for c in pms.discover.catalog[:14]:
            kind = {"movie": "1", "show": "2"}[c["kind"]]
            guid = urllib.parse.quote(c["guid"], safe="")
            rows = jcall(pms, "GET", f"/library/all?type={kind}&guid={guid}")["Metadata"]
            held[c["hex"]] = bool(rows)
            if rows:
                self.assertEqual(rows[0]["guid"], c["guid"])
                self.assertTrue(rows[0]["ratingKey"].isdigit())
        states = set()
        for c in pms.discover.catalog[:14]:
            states.add(("Available" if c["released"] else "ComingSoonMatched") if held[c["hex"]]
                       else ("NotOnAnyServer" if c["released"] else "ComingSoon"))
        self.assertEqual(states, {"Available", "ComingSoonMatched", "ComingSoon", "NotOnAnyServer"})
        self.assertEqual(sum(held.values()), 7)
        self.assertGreaterEqual(sum(1 for c in pms.discover.catalog[:14] if c["kind"] == "show"), 4)

    def test_a_guid_batch_is_one_request(self):
        pms = make()
        cs = [c for c in pms.discover.catalog[:14] if c["kind"] == "movie" and c["held"]]
        joined = ",".join(urllib.parse.quote(c["guid"], safe="") for c in cs)
        rows = jcall(pms, "GET", f"/library/all?type=1&guid={joined}")["Metadata"]
        self.assertEqual({r["guid"] for r in rows}, {c["guid"] for c in cs})

    def test_future_titles_have_a_future_date_and_released_a_past_one(self):
        for c in make().discover.catalog:
            self.assertEqual(c["date"] > "2027-01-01", not c["released"], c["title"])

    def test_user_state_is_an_array_with_watchlisted_at_only_while_listed(self):
        pms = make()
        listed = pms.discover.catalog[0]["hex"]
        unlisted = pms.discover.catalog[15]["hex"]
        state = jcall(pms, "GET", f"/library/metadata/{listed}/userState")["UserState"]
        self.assertIsInstance(state, list)
        self.assertIsInstance(state[0]["watchlistedAt"], int)
        state = jcall(pms, "GET", f"/library/metadata/{unlisted}/userState")["UserState"]
        self.assertIsInstance(state, list)
        self.assertNotIn("watchlistedAt", state[0])

    def test_metadata_answers_a_catalog_item_and_404s_the_unknown(self):
        pms = make()
        c = pms.discover.catalog[12]
        m = jcall(pms, "GET", f"/library/metadata/{c['hex']}")["Metadata"][0]
        self.assertEqual((m["ratingKey"], m["type"], m["title"]), (c["hex"], "show", c["title"]))
        self.assertTrue(m["Genre"])
        self.assertEqual(call(pms, "GET", "/library/metadata/" + "0" * 24)[0], 404)
        status, ctype, data = call(pms, "GET", f"/library/metadata/{c['hex']}/thumb/1")
        self.assertEqual((status, ctype), (200, "image/png"))
        self.assertTrue(data.startswith(b"\x89PNG"))

    def test_remove_and_add_change_later_lists_in_memory(self):
        pms = make()
        target = page(pms)["Metadata"][2]["ratingKey"]
        self.assertEqual(jcall(pms, "PUT", f"/actions/removeFromWatchlist?ratingKey={target}"), {"size": 0})
        after = page(pms)
        self.assertEqual(after["totalSize"], 13)
        self.assertNotIn(target, [m["ratingKey"] for m in after["Metadata"]])
        self.assertNotIn("watchlistedAt",
                         jcall(pms, "GET", f"/library/metadata/{target}/userState")["UserState"][0])
        jcall(pms, "PUT", f"/actions/removeFromWatchlist?ratingKey={target}")  # idempotent
        self.assertEqual(page(pms)["totalSize"], 13)
        jcall(pms, "PUT", f"/actions/addToWatchlist?ratingKey={target}")
        jcall(pms, "PUT", f"/actions/addToWatchlist?ratingKey={target}")  # idempotent
        again = page(pms)
        self.assertEqual(again["totalSize"], 14)
        self.assertEqual(again["Metadata"][0]["ratingKey"], target)  # newest first
        added = pms.discover.catalog[14]["hex"]
        jcall(pms, "PUT", f"/actions/addToWatchlist?ratingKey={added}")
        self.assertEqual(page(pms)["totalSize"], 15)
        self.assertIn(added, [m["ratingKey"] for m in page(pms, "released")["Metadata"]])
        self.assertEqual([w[0] for w in pms.writes], ["PUT"] * 5)
        self.assertTrue(all("ratingKey=" in w[1] for w in pms.writes))

    def test_bad_writes_are_refused(self):
        pms = make()
        self.assertEqual(call(pms, "PUT", "/actions/addToWatchlist?ratingKey=nothex")[0], 400)
        self.assertEqual(call(pms, "PUT", "/actions/addToWatchlist?ratingKey=" + "a" * 24)[0], 404)
        self.assertEqual(call(pms, "GET", "/actions/addToWatchlist?ratingKey=" + "a" * 24)[0], 405)

    def test_state_is_per_process_object(self):
        a, b = make(), make()
        jcall(a, "PUT", f"/actions/removeFromWatchlist?ratingKey={a.discover.catalog[0]['hex']}")
        self.assertEqual(page(a)["totalSize"], 13)
        self.assertEqual(page(b)["totalSize"], 14)

    def test_guid_lookup_can_be_made_to_fail(self):
        pms = make()
        status, obj = pms.discover.configure({"guid_lookup": "fail"})
        self.assertEqual(status, 200)
        guid = urllib.parse.quote(pms.discover.catalog[0]["guid"], safe="")
        self.assertEqual(call(pms, "GET", f"/library/all?type=1&guid={guid}")[0], 503)
        pms.discover.configure({"guid_lookup": "ok"})
        self.assertEqual(call(pms, "GET", f"/library/all?type=1&guid={guid}")[0], 200)
        self.assertEqual(pms.discover.configure({"x": 1})[0], 400)

    def test_list_modes_reset_the_list_over_the_control(self):
        pms = make()
        status, ctype, data = pms.handle("POST", "/_mock/discover", json.dumps({"list": "empty"}).encode(), {})
        self.assertEqual(status, 200)
        empty = page(pms)
        self.assertEqual((empty["totalSize"], empty.get("Metadata", [])), (0, []))
        self.assertEqual(page(pms, "released")["totalSize"], 0)
        pms.discover.configure({"list": "movies_only"})
        movies = page(pms)
        self.assertEqual({m["type"] for m in movies["Metadata"]}, {"movie"})
        listed_movies = [c for c in pms.discover.catalog[:DiscoverFixture.LISTED] if c["kind"] == "movie"]
        self.assertEqual(movies["totalSize"], len(listed_movies))
        self.assertLess(movies["totalSize"], 14)
        self.assertGreater(movies["totalSize"], 0)
        pms.discover.configure({"list": "default"})
        self.assertEqual(page(pms)["totalSize"], 14)
        # Add/remove still work from a reset state.
        pms.discover.configure({"list": "empty"})
        jcall(pms, "PUT", f"/actions/addToWatchlist?ratingKey={pms.discover.catalog[0]['hex']}")
        self.assertEqual(page(pms)["totalSize"], 1)

    def test_list_mode_validation(self):
        pms = make()
        self.assertEqual(pms.discover.configure({"list": "bogus"})[0], 400)
        self.assertEqual(pms.discover.configure({})[0], 400)
        self.assertEqual(page(pms)["totalSize"], 14)  # a refused patch changes nothing
        self.assertEqual(pms.discover.configure({"list": "empty", "x": 1})[0], 400)
        self.assertEqual(page(pms)["totalSize"], 14)

    def test_a_title_bound_by_guids_all_can_be_added_and_listed(self):
        pms = make()
        before = page(pms)["totalSize"]
        status, _ = pms.discover.configure({"guids": "all"}, pms.lib)
        self.assertEqual(status, 200)
        it = next(i for i in pms.lib.items.values() if i.get("type") == "movie"
                  and i["guid"] not in {c["guid"] for c in pms.discover.catalog[:DiscoverFixture.LISTED]})
        hx = it["guid"].rsplit("/", 1)[1]
        self.assertEqual(page(pms)["totalSize"], before, "known, but not on the list")
        jcall(pms, "PUT", f"/actions/addToWatchlist?ratingKey={hx}")
        listed = page(pms)
        self.assertEqual(listed["totalSize"], before + 1)
        self.assertEqual(listed["Metadata"][0]["title"], it["title"])
        jcall(pms, "PUT", f"/actions/removeFromWatchlist?ratingKey={hx}")
        self.assertEqual(page(pms)["totalSize"], before)
        self.assertEqual(call(pms, "PUT", f"/actions/addToWatchlist?ratingKey={'0' * 24}")[0], 404,
                         "a guid nothing bound is still unknown")

    def test_scenario_modes_are_all_known_to_the_mock(self):
        scenario = json.loads((pathlib.Path(__file__).resolve().parent / "scenarios"
                               / "watchlist_captures.json").read_text())
        modes = {c["mock"] for c in scenario["captures"]}
        self.assertLessEqual(modes, set(DiscoverFixture.LIST_MODES))

    def test_ordinary_pms_paths_are_untouched(self):
        pms = make()
        self.assertGreater(len(jcall(pms, "GET", "/library/sections/1/all")["Metadata"]), 0)
        self.assertEqual(len(DiscoverFixture.TITLES), 16)


if __name__ == "__main__":
    unittest.main()
