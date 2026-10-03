"""Library fixture contracts: the mock must exercise the real rail and sparse-page boundaries."""
import contextlib
import io
import json
import hashlib
import pathlib
import re
import shutil
import socket
import subprocess
import tempfile
import unittest
import urllib.parse
import urllib.request

from mock_pms import (
    ENHANCEMENT_AAC_51_RK, ENHANCEMENT_AC3_2CH_RK, ENHANCEMENT_DEFAULT_SRT_RK,
    ENHANCEMENT_DV_P8_RK, ENHANCEMENT_EXTERNAL_SRT_RK, EXTRA_MEDIA_RK_BASE, PLEX_DIRECT_HASH,
    Library, MockPms, plaintext_only_lan_resources, serve,
)


HAS_FFMPEG_AND_FFPROBE = shutil.which("ffmpeg") and shutil.which("ffprobe")


def get(pms, path):
    status, content_type, data = pms.handle("GET", path)
    assert status == 200 and content_type == "application/json"
    return json.loads(data)["MediaContainer"]


class LibraryRail(unittest.TestCase):
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
        self.assertEqual(get(MockPms(lib), "/library/sections/1/all")["totalSize"], 48)

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
            "refuse_enhancements": True, "ignore_enhancements": False, "plex_pass": True})
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


if __name__ == "__main__":
    unittest.main()
