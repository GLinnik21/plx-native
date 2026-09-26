#!/usr/bin/env python3
"""Loopback-only checks for the cache-stress fixture and its evidence gates."""
import importlib.util
import json
from pathlib import Path
import tempfile
import threading
import unittest
import urllib.error
import urllib.parse
import urllib.request


SPEC = importlib.util.spec_from_file_location(
    "image_cache_stress", Path(__file__).resolve().parents[1] / "tools/image-cache-stress.py")
stress = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(stress)


class FixtureTest(unittest.TestCase):
    def setUp(self):
        self.fixture = stress.Fixture()
        self.server = stress.http.server.ThreadingHTTPServer(("127.0.0.1", 0), stress.handler(self.fixture))
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.base = f"http://127.0.0.1:{self.server.server_port}"

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        self.fixture.close()

    def get(self, path):
        with urllib.request.urlopen(self.base + path, timeout=5) as response:
            return response.read()

    def image_path(self, number):
        return "/photo/:/transcode?" + urllib.parse.urlencode(dict(
            url=f"/library/metadata/{number}/thumb/1", width=250, height=375,
            minSize=1, **{"X-Plex-Token": "YOUR_TEST_TOKEN_NEVER_LOG"}))

    def test_real_pagination_has_1200_distinct_posters_and_stable_identity(self):
        items = []
        for start in range(0, 1200, 60):
            page = json.loads(self.get(
                f"/library/sections/1/all?X-Plex-Container-Start={start}&X-Plex-Container-Size=60"))["MediaContainer"]
            self.assertEqual(page["offset"], start)
            self.assertEqual(page["totalSize"], 1200)
            items.extend(page["Metadata"])
        self.assertEqual(len({item["thumb"] for item in items}), 1200)
        self.assertEqual(len({item["ratingKey"] for item in items}), 1200)
        self.assertEqual(json.loads(self.get("/identity"))["MediaContainer"]["machineIdentifier"], stress.MACHINE_ID)
        self.assertEqual(self.fixture.snapshot()["phases"]["cold"]["unique_listing_items"], 1200)

    def test_control_refuses_images_but_keeps_metadata_and_never_logs_tokens(self):
        first = self.get(self.image_path(1))
        last = self.get(self.image_path(1200))
        self.assertTrue(first.startswith(b"\xff\xd8") and first.endswith(b"\xff\xd9"))
        self.assertNotEqual(first, last)
        request = urllib.request.Request(self.base + "/__stress/control", method="POST",
            data=b'{"images":false,"phase":"warm"}', headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=5) as response:
            self.assertFalse(json.load(response)["images"])
        with self.assertRaises(urllib.error.HTTPError) as refused:
            self.get(self.image_path(1))
        self.assertEqual(refused.exception.code, 503)
        self.get("/library/sections/1/all?X-Plex-Token=YOUR_TEST_TOKEN_METADATA")
        self.assertNotIn("YOUR_TEST_TOKEN_NEVER_LOG", json.dumps(self.fixture.records))
        self.assertNotIn("YOUR_TEST_TOKEN_METADATA", json.dumps(self.fixture.records))
        self.assertEqual(self.fixture.snapshot()["phases"]["cold"]["unique_image_keys"], 2)
        self.assertEqual(self.fixture.snapshot()["phases"]["warm"]["image_failures"], 1)

    def test_repeating_one_tile_1200_times_cannot_pass_distinct_image_gate(self):
        records = [dict(phase="cold", kind="image", status=200, bytes=10,
                        key="same", image_id=1, at_s=i) for i in range(1200)]
        self.assertEqual(stress.access_summary(records)["cold"]["unique_image_ids"], 1)
        cold, warm = legs()
        access = {"cold": stress.access_summary(records)["cold"],
                  "warm": {"unique_listing_items": 1200, "image_requests": 0}}
        self.assertIn("cold run did not fetch 1001 distinct tile images", stress.grade(access, cold, warm))


def legs():
    cold = dict(library_samples=160, invalid_pacing=False, pacing=dict(n=9000),
                cache=dict(entries=1200, bytes=40000000, writes=1200, hits=1100, fetches=1200))
    warm = dict(library_samples=160, invalid_pacing=False, pacing=dict(n=9000),
                cache=dict(entries=1200, bytes=40000000, writes=0, hits=2400, fetches=0))
    return cold, warm


class EvidenceTest(unittest.TestCase):
    def test_real_restart_evidence_passes_but_missing_hits_requests_or_duration_fail(self):
        access = {"cold": {"unique_image_ids": 1200, "image_failures": 0},
                  "warm": {"unique_listing_items": 1200, "image_requests": 0}}
        cold, warm = legs()
        self.assertEqual(stress.grade(access, cold, warm), [])
        warm["cache"]["hits"] = 0
        self.assertIn("warm process did not read enough images from disk", stress.grade(access, cold, warm))
        cold, warm = legs()
        access["warm"]["image_requests"] = 1
        self.assertIn("warm process restart requested image bytes from the server", stress.grade(access, cold, warm))
        warm["library_samples"] = 18
        self.assertTrue(any("heartbeat" in error for error in stress.grade(access, cold, warm)))

    def test_baseline_requires_ram_only_and_large_grid_evidence(self):
        baseline = dict(invalid_pacing=False, pacing=dict(n=9000), library_samples=160,
                        cache=dict(hits=0, writes=0, entries=0, fetches=2400))
        access = dict(baseline=dict(unique_image_ids=1200))
        self.assertEqual(stress.grade_baseline(access, baseline), [])
        baseline["cache"]["hits"] = 1
        self.assertIn("RAM-only baseline did not bypass the disk cache", stress.grade_baseline(access, baseline))
        baseline["cache"]["hits"] = 0
        self.assertIn("RAM-only baseline did not load the distinct tile images", stress.grade_baseline({}, baseline))

    def test_metrics_parser_reads_disk_counters_and_refuses_simulator_pacing(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "events.log"
            path.write_text("\n".join([
                "loop=60 route=library fps=59 worstframe=19.1ms budget=3/0",
                "imgcache: hits=1200 misses=0 writes=0 evictions=0 entries=1200 bytes=40000000 fetches=0",
                "loop=60 route=library fps=60 sim=1",
            ]))
            result = stress.event_summary(path)
            self.assertEqual(result["cache"]["hits"], 1200)
            self.assertEqual(result["library_samples"], 2)
            self.assertTrue(result["invalid_pacing"])

    def test_frame_gaps_and_slow_cpu_work_are_separate_and_startup_is_excluded(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "events.log"
            startup = "loop=60 route=library fps=60 frame_n=60 frame_gt16=60 frame_gt33=60 frame_gt50=60 frame_gt100=60 frame_max=999ms frame_p95=999ms frame_p99=999ms"
            path.write_text("\n".join([startup] * 5 + [
                "FRAMEDROP total=40.0 prepare=2.0 draw=35.0 up=1 route=library",
                "loop=60 route=library fps=59 dropped=999 frame_n=59 frame_gt16=20 frame_gt33=2 frame_gt50=1 frame_gt100=0 frame_max=55.5ms frame_p95=25ms frame_p99=56ms",
                "FRAMEDROP total=999.0 prepare=900.0 up=1 route=library", # incomplete window
            ]))
            result = stress.event_summary(path)
            self.assertEqual(result["pacing"]["n"], 59)
            self.assertEqual(result["pacing"]["gt33"], 2)
            self.assertEqual(result["pacing"]["max_ms"], 55.5)
            self.assertEqual(result["slow_cpu_work"]["recorded_slow_frames"], 1)
            self.assertEqual(result["slow_cpu_work"]["worst_frames"][0]["draw"], 35)
        cold, warm = legs()
        del warm["pacing"]
        self.assertTrue(any("frame interval" in error for error in stress.grade({}, cold, warm)))

    def test_access_log_refuses_to_overwrite_prior_evidence(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "access.jsonl"
            path.write_text("prior evidence\n")
            with self.assertRaises(FileExistsError):
                stress.Fixture(access_log=path)
            self.assertEqual(path.read_text(), "prior evidence\n")


if __name__ == "__main__":
    unittest.main()
