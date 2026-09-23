"""Library fixture contracts: the mock must exercise the real rail and sparse-page boundaries."""
import json
import hashlib
import pathlib
import re
import shutil
import subprocess
import tempfile
import unittest
import urllib.request

from mock_pms import EXTRA_MEDIA_RK_BASE, Library, MockPms, serve


HAS_FFMPEG_AND_FFPROBE = shutil.which("ffmpeg") and shutil.which("ffprobe")


def get(pms, path):
    status, content_type, data = pms.handle("GET", path)
    assert status == 200 and content_type == "application/json"
    return json.loads(data)["MediaContainer"]


class LibraryRail(unittest.TestCase):
    def test_default_generated_data_is_byte_identical_to_the_previous_fixture(self):
        hashes = {
            1: "27a636047b17af9f68108417af4cc9b11d70ab84bf43bf27d62a4aeba08c117b",
            7: "c287cf05261367f9e8f4640b1b4617ccb9389ccef5947342413c52b61eaa5472",
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


if __name__ == "__main__":
    unittest.main()
