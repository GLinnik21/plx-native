"""tools/site_video.py: the pure logic of the site video's encode, gates, sentinel scan and adopt step.

Everything here is offline and needs no ffmpeg: frames are tiny synthetic RGB buffers, manifests and
artifacts are built in a private temp dir, and the "download" is a fake opener serving a shell script
that answers `-encoders`. The one case that needs a real ffmpeg (an encode and an SSIM of a 4-frame
master) is OFF unless PLXNATIVE_SITE_VIDEO_FFMPEG names an ffmpeg with libsvtav1 and libx264; it says so.
"""
import hashlib
import importlib.util
import io
import json
import os
import pathlib
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
import unittest.mock
import urllib.error
import zipfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
_spec = importlib.util.spec_from_file_location("site_video", ROOT / "tools" / "site_video.py")
sv = importlib.util.module_from_spec(_spec)
sys.modules["site_video"] = sv
_spec.loader.exec_module(sv)

SENT = b"\xff\x00\xfe"
REAL_FFMPEG = os.environ.get("PLXNATIVE_SITE_VIDEO_FFMPEG")


def frame(width, height, pixels=(), fill=b"\x10\x20\x30"):
    """An RGB24 frame of `fill` with the sentinel at each (x, y) in `pixels`."""
    buf = bytearray(fill * (width * height))
    for x, y in pixels:
        buf[(y * width + x) * 3:(y * width + x) * 3 + 3] = SENT
    return bytes(buf)


class SentinelScanner(unittest.TestCase):
    W, H = 32, 24

    def scan(self, pixels, **kw):
        return sv.scan_frame(frame(self.W, self.H, pixels), self.W, self.H, **kw)

    def test_a_clean_frame_passes(self):
        self.assertIsNone(self.scan([]))

    def test_an_8x8_block_is_a_hit_with_its_box(self):
        hit = self.scan([(x, y) for x in range(5, 13) for y in range(4, 12)])
        self.assertEqual(hit, (64, (5, 4, 12, 11)))

    def test_a_64_pixel_row_run_is_a_hit_and_a_63_pixel_run_is_not(self):
        wide = 80
        self.assertEqual(sv.scan_frame(frame(wide, 2, [(x, 0) for x in range(64)]), wide, 2)[0], 64)
        self.assertIsNone(sv.scan_frame(frame(wide, 2, [(x, 0) for x in range(63)]), wide, 2))

    def test_63_pixels_in_a_block_are_not_a_hit(self):
        px = [(x, y) for x in range(8) for y in range(8)][:63]
        self.assertIsNone(self.scan(px))

    def test_a_connected_l_shape_across_rows_counts_as_one_region(self):
        px = [(x, 3) for x in range(30)] + [(0, y) for y in range(4, 24)]  # 30 + 20 = 50: below
        self.assertIsNone(self.scan(px))
        px += [(x, 23) for x in range(1, 21)]  # 70, connected through column 0
        self.assertEqual(self.scan(px)[0], 70)

    def test_a_diagonal_chain_is_not_connected(self):
        wide = 80
        self.assertIsNone(sv.scan_frame(frame(wide, 80, [(i, i) for i in range(70)]), wide, 80))

    def test_a_scatter_of_isolated_pixels_and_two_separate_blobs_pass(self):
        scatter = [(x, y) for x in range(0, 32, 2) for y in range(0, 24, 2)]  # 192 pixels, none adjacent
        self.assertIsNone(self.scan(scatter))
        a = [(x, y) for x in range(0, 6) for y in range(0, 6)]  # 36
        b = [(x, y) for x in range(10, 16) for y in range(0, 6)]  # 36, a gap column apart
        self.assertIsNone(self.scan(a + b))

    def test_near_miss_colours_are_not_the_sentinel(self):
        buf = bytearray(frame(self.W, self.H))
        for i in range(100):
            buf[i * 3:i * 3 + 3] = b"\xff\x00\xfd"
        self.assertIsNone(sv.scan_frame(bytes(buf), self.W, self.H))

    def test_bytes_that_straddle_two_pixels_are_not_a_pixel(self):
        # ... (a, FF, 00)(FE, FF, 00)(FE, ...): the sentinel byte pattern at offset 1, never at a pixel.
        row = bytes([1, 0xff, 0x00, 0xfe, 0xff, 0x00, 0xfe, 7, 7]) * 40
        self.assertIsNone(sv.scan_frame(row, 3 * 40, 1))
        long_straddle = b"\x01" + SENT * 65 + b"\x01\x01"  # 65 consecutive sentinel triplets, one byte off the pixel grid
        self.assertIsNone(sv.scan_frame(long_straddle, 66, 1))
        # ... and does not hide a true run that follows it.
        wide = 70
        buf = bytearray(b"\x01\xff\x00" + b"\xfe\xff\x00" * 0 + b"\xfe\x01\x01" + SENT * 64)  # straddle, then 64 true pixels
        buf += b"\x00" * (wide * 3 - len(buf))
        self.assertEqual(sv.scan_frame(bytes(buf), wide, 1)[0], 64)

    def test_the_wrong_frame_size_is_an_error(self):
        with self.assertRaises(ValueError):
            sv.scan_frame(b"\x00" * 10, 4, 4)

    def test_the_minimum_run_is_a_parameter(self):
        self.assertEqual(self.scan([(1, 1), (2, 1), (3, 1)], min_run=3)[0], 3)

    def test_scan_a_1080p_frame_quickly(self):
        import time
        w, h = 1920, 1080
        clean = frame(w, h)
        start = time.perf_counter()
        for _ in range(5):
            self.assertIsNone(sv.scan_frame(clean, w, h))
        self.assertLess((time.perf_counter() - start) / 5, 0.25, "a clean 1080p frame should scan in well under 250 ms")
        dirty = frame(w, h, [(x, y) for x in range(100, 110) for y in range(500, 510)])
        start = time.perf_counter()
        self.assertEqual(sv.scan_frame(dirty, w, h)[0], 100)
        self.assertLess(time.perf_counter() - start, 1.0)


class Trickle(io.RawIOBase):
    """A pipe that returns at most `step` bytes a read."""

    def __init__(self, data, step):
        self.data, self.pos, self.step = data, 0, step

    def readable(self):
        return True

    def read(self, n=-1):
        chunk = self.data[self.pos:self.pos + min(n, self.step)]
        self.pos += len(chunk)
        return chunk


class SentinelStream(unittest.TestCase):
    W, H = 16, 8

    def test_short_reads_are_reassembled_and_clean_frames_are_teed(self):
        frames = [frame(self.W, self.H, fill=bytes([i, i, i])) for i in range(3)]
        tee = io.BytesIO()
        n, hit = sv.scan_stream(Trickle(b"".join(frames), 37), self.W, self.H, tee)
        self.assertEqual((n, hit), (3, None))
        self.assertEqual(tee.getvalue(), b"".join(frames))

    def test_the_first_hit_names_its_frame_and_stops_the_tee(self):
        bad = frame(self.W, self.H, [(x, y) for x in range(8) for y in range(8)])
        tee = io.BytesIO()
        n, hit = sv.scan_stream(io.BytesIO(frame(self.W, self.H) * 2 + bad + frame(self.W, self.H)), self.W, self.H, tee)
        self.assertEqual(hit.frame, 2)
        self.assertEqual(hit.pixels, 64)
        self.assertIn("frame 2", hit.message())
        self.assertEqual(len(tee.getvalue()), 2 * self.W * self.H * 3)

    def test_a_stream_that_ends_inside_a_frame_is_an_error(self):
        with self.assertRaises(sv.Failure):
            sv.scan_stream(io.BytesIO(frame(self.W, self.H)[:-5]), self.W, self.H)

    def test_the_cli_exits_1_on_a_hit_and_0_on_a_clean_stream(self):
        w, h = 16, 8
        bad = frame(w, h, [(x, y) for x in range(8) for y in range(8)])
        with tempfile.TemporaryDirectory() as tmp:
            good_path, bad_path = pathlib.Path(tmp, "good.rgb"), pathlib.Path(tmp, "bad.rgb")
            good_path.write_bytes(frame(w, h) * 2)
            bad_path.write_bytes(frame(w, h) + bad)
            with unittest.mock.patch.object(sys, "stderr", io.StringIO()):
                self.assertEqual(sv.main(["scan-sentinel", "--size", "16x8", str(good_path)]), 0)
                self.assertEqual(sv.main(["scan-sentinel", "--size", "16x8", str(bad_path)]), 1)
                self.assertEqual(sv.main(["scan-sentinel", "--size", "bogus", str(bad_path)]), 1)


def tsv(rows, t_ms=None):
    """A frames.tsv; each row's t_ms is T(n) unless `t_ms` (a list) overrides it."""
    return sv.TSV_HEADER + "\n" + "".join(
        f"{n}\t{sv.frame_ms(n) if t_ms is None else t_ms[i]}\t{h}\t{d}\t0\n" for i, (n, h, d) in enumerate(rows))


GOOD_ROWS = [(0, "0123456789abcdef", 0), (1, "fedcba9876543210", 0), (2, "00000000000000ff", 0)]


def record(**over):
    rec = {"schema": 1, "platform": "linux-ci", "storyboard_sha256": "a" * 64,
           "frames": {"count": 3, "fps": 60, "width": 1920, "height": 1080},
           "placeholder_debt": {"frames_with_debt": 0},
           "sentinel": {"scanned_frames": 3, "hits": 0},
           "hero_pool": {"logged": ["10", "11"], "eligible": ["10", "11", "12"]},
           "opened_rating_keys": ["11"]}
    rec.update(over)
    return rec


FACTS = {"hero_logged": ["10"], "hero_eligible": ["10"], "hero_turns": [], "opened": ["11"], "not_in_video": ["20"],
         "playback": []}


class FramesTsvAndRecord(unittest.TestCase):
    def test_a_good_file_parses(self):
        rows = sv.parse_frames_tsv(tsv(GOOD_ROWS))
        self.assertEqual([r[0] for r in rows], [0, 1, 2])

    def test_each_malformation_is_refused(self):
        bad = {
            "no header": "0\t0\t0123456789abcdef\t0\t0\n",
            "a gap in n": tsv([(0, "0123456789abcdef", 0), (2, "0123456789abcdef", 0)]),
            "a short hash": tsv([(0, "0123", 0)]),
            "an upper-case hash": tsv([(0, "0123456789ABCDEF", 0)]),
            "no frames": sv.TSV_HEADER + "\n",
            "a missing column": sv.TSV_HEADER + "\n0\t0\t0123456789abcdef\t0\n",
            "a negative debt": tsv([(0, "0123456789abcdef", -1)]),
        }
        for what, text in bad.items():
            with self.subTest(what), self.assertRaises(sv.Failure):
                sv.parse_frames_tsv(text)

    def test_debt_on_any_written_frame_fails_the_gate_and_names_it(self):
        rows = sv.parse_frames_tsv(tsv([(0, "0" * 16, 0), (1, "1" * 16, 3), (2, "2" * 16, 0)]))
        res = {r.name: r for r in sv.gate_frames_tsv(rows, record(), 3)}
        self.assertEqual(res["placeholder-debt"].status, sv.FAIL)
        self.assertIn("frame 1", res["placeholder-debt"].detail)
        self.assertEqual(res["frames/count"].status, sv.PASS)

    def test_t_ms_is_pinned_to_the_drivers_rounding(self):
        # The same values as framedump.rs's `t_of_n_is_the_rounded_sixtieth_of_a_second`.
        self.assertEqual([sv.frame_ms(n) for n in range(10)], [0, 17, 33, 50, 67, 83, 100, 117, 133, 150])
        self.assertEqual((sv.frame_ms(59), sv.frame_ms(60), sv.frame_ms(600)), (983, 1000, 10_000))
        for n in (35_999, 36_000, 36_001, 1_000_003):
            self.assertEqual(sv.frame_ms(n), int(n * 1000 / 60 + 0.5), n)

    def test_a_skipped_stretch_of_virtual_time_fails_the_t_ms_gate(self):
        # The measured cut master: t_ms ran 0, 17, 200 and every other gate passed it.
        rows = sv.parse_frames_tsv(tsv(GOOD_ROWS, t_ms=[0, 17, 200]))
        res = {r.name: r for r in sv.gate_frames_tsv(rows, record(), 3)}
        self.assertEqual(res["frames/t_ms"].status, sv.FAIL)
        self.assertIn("frame 2 has 200, want 33", res["frames/t_ms"].detail)
        for t in ([1, 17, 33], [0, 16, 33], [0, 17, 34]):  # one ms off, either side, on any row
            rows = sv.parse_frames_tsv(tsv(GOOD_ROWS, t_ms=t))
            self.assertEqual({r.name: r.status for r in sv.gate_frames_tsv(rows, record(), 3)}["frames/t_ms"], sv.FAIL, t)

    def test_zero_debt_passes_and_a_record_that_disagrees_fails(self):
        rows = sv.parse_frames_tsv(tsv(GOOD_ROWS))
        self.assertEqual({r.status for r in sv.gate_frames_tsv(rows, record(), 3)}, {sv.PASS})
        res = {r.name: r for r in sv.gate_frames_tsv(rows, record(placeholder_debt={"frames_with_debt": 2}), 3)}
        self.assertEqual(res["placeholder-debt"].status, sv.FAIL)

    def test_the_frame_count_must_agree_everywhere(self):
        rows = sv.parse_frames_tsv(tsv(GOOD_ROWS))
        res = {r.name: r for r in sv.gate_frames_tsv(rows, record(), 4)}
        self.assertEqual(res["frames/count"].status, sv.FAIL)
        self.assertIn("the master has 4", res["frames/count"].detail)

    def test_run_twice_equality_compares_everything_but_the_timing_column(self):
        with tempfile.TemporaryDirectory() as tmp:
            a, b, c = (pathlib.Path(tmp, n) for n in "abc")
            a.write_text(tsv(GOOD_ROWS))
            b.write_text(tsv(GOOD_ROWS))
            c.write_text(tsv(GOOD_ROWS[:2] + [(2, "00000000000000fe", 0)]))
            self.assertEqual(sv.gate_run_twice(a, b).status, sv.PASS)
            diff = sv.gate_run_twice(a, c)
            self.assertEqual(diff.status, sv.FAIL)
            self.assertIn("first differing frame 2", diff.detail)
            self.assertEqual(sv.gate_run_twice(a, None).status, sv.FAIL)  # not supplied: fail closed

    def test_run_twice_ignores_how_many_holds_preceded_a_frame_but_not_its_pixels(self):
        with tempfile.TemporaryDirectory() as tmp:
            a, b, c = (pathlib.Path(tmp, n) for n in "abc")
            a.write_text(tsv(GOOD_ROWS))
            b.write_text(tsv(GOOD_ROWS).replace("\t0\t0\n", "\t0\t347\n", 1))
            c.write_text(tsv(GOOD_ROWS).replace("0123456789abcdef", "0123456789abcdee"))
            self.assertNotEqual(a.read_bytes(), b.read_bytes())
            self.assertEqual(sv.gate_run_twice(a, b).status, sv.PASS, "holds is timing")
            self.assertEqual(sv.gate_run_twice(a, c).status, sv.FAIL, "a pixel is not")

    def test_every_hero_page_shown_must_pass_the_hero_report(self):
        good = sv.gate_hero_pool(record(hero_pool={"logged": ["10", "11"], "eligible": ["10", "11", "12"],
                                                   "turns": ["11", "10"]}))
        self.assertEqual(good.status, sv.PASS)
        self.assertIn("2 page turn(s)", good.detail)
        self.assertEqual(sv.gate_hero_pool(record()).status, sv.PASS)
        bad = sv.gate_hero_pool(record(hero_pool={"logged": ["10", "99"], "eligible": ["10"]}))
        self.assertEqual(bad.status, sv.FAIL)
        self.assertIn("99", bad.detail)
        self.assertEqual(sv.gate_hero_pool(record(hero_pool={"logged": [], "eligible": ["10"]})).status, sv.FAIL)

    def test_a_hero_page_flagged_not_in_video_fails_even_when_it_passes_the_report(self):
        got = sv.gate_hero_pool(record(hero_pool={"logged": ["10", "13"], "eligible": ["10", "13"]},
                                       not_in_video=["13"]))
        self.assertEqual(got.status, sv.FAIL)
        self.assertIn("13", got.detail)
        self.assertEqual(sv.gate_hero_pool(record(not_in_video=["99"])).status, sv.PASS)

    def test_the_hero_pool_is_fail_closed_with_no_skip_for_any_script(self):
        empty = {"logged": [], "eligible": []}
        for over in ({}, {"storyboard": {"script": "hold"}}, {"storyboard": {"script": "feel"}}):
            got = sv.gate_hero_pool(record(hero_pool=empty, **over))
            self.assertEqual(got.status, sv.FAIL, over)
        self.assertEqual(sv.gate_hero_pool(record(hero_pool={"logged": ["10"], "eligible": []})).status, sv.FAIL)

    def test_the_inline_sentinel_record_must_cover_the_run_and_be_clean(self):
        self.assertEqual(sv.gate_sentinel_record(record()).status, sv.PASS)
        self.assertEqual(sv.gate_sentinel_record(record(sentinel={"scanned_frames": 3, "hits": 1})).status, sv.FAIL)
        short = sv.gate_sentinel_record(record(sentinel={"scanned_frames": 2, "hits": 0}))
        self.assertEqual(short.status, sv.FAIL)
        self.assertIn("did not cover", short.detail)

    def test_the_render_record_shape(self):
        self.assertEqual(sv.validate_render_record(record()), [])
        broken = record()
        del broken["hero_pool"]["eligible"]
        broken["opened_rating_keys"] = [11]
        broken["frames"]["count"] = "3"
        problems = sv.validate_render_record(broken)
        self.assertTrue(any("hero_pool.eligible" in p for p in problems), problems)
        self.assertTrue(any("opened_rating_keys" in p for p in problems), problems)
        self.assertTrue(any("frames.count" in p for p in problems), problems)
        self.assertTrue(sv.validate_render_record([]))

    def test_every_documented_field_is_checked(self):
        for dotted in sv.RENDER_RECORD_FIELDS:
            rec = record()
            parent, _, leaf = dotted.rpartition(".")
            holder = sv._dig(rec, parent)[0] if parent else rec
            del holder[leaf]
            with self.subTest(dotted):
                self.assertTrue(any(dotted in p for p in sv.validate_render_record(rec)))


class RenderLauncher(unittest.TestCase):
    """`render`: the pieces that need no simulator. The run itself is `make site-video-sim` plus a Mac (a
    preview) or `.github/workflows/site-video.yml` (the canonical render), and its determinism is graded by the
    run-twice gate."""

    def test_the_ffv1_argv_is_the_input_contract(self):
        argv = sv.ffv1_argv("/x/ffmpeg", "m.mkv")
        joined = " ".join(argv)
        for want in ("-f rawvideo -pix_fmt rgb24 -s 1920x1080", "-framerate 60", "-c:v ffv1 -level 3 -g 1", "-an"):
            self.assertIn(want, joined)
        self.assertEqual(argv[-1], "m.mkv")

    def test_the_master_is_muxed_bitexact_so_identical_frames_make_an_identical_file(self):
        argv = sv.ffv1_argv("/x/ffmpeg", "m.mkv")
        joined = " ".join(argv)
        self.assertIn("-fflags +bitexact", joined)
        self.assertIn("-flags:v +bitexact", joined)
        self.assertLess(argv.index("-fflags"), len(argv) - 1, "an OUTPUT option: after -i, before the file")
        self.assertGreater(argv.index("-fflags"), argv.index("-i"))

    def test_the_locale_and_time_zone_are_pinned_whatever_the_host_has(self):
        env = sv.sim_env("/rt", "/out", "/rt/fifo", 1, 0, False, 1,
                         base={"LANG": "be_BY.UTF-8", "LC_TIME": "de_DE", "LANGUAGE": "be", "TZ": "Asia/Tokyo", "PATH": "/bin"})
        self.assertEqual((env["LANG"], env["LC_ALL"], env["TZ"]), ("C", "C", "UTC"))
        self.assertNotIn("LC_TIME", env)
        self.assertNotIn("LANGUAGE", env)
        self.assertEqual(env["PATH"], "/bin")

    def test_the_script_and_the_hold_injection_reach_the_simulator_only_when_asked(self):
        plain = sv.sim_env("/rt", "/out", "/rt/fifo", 1, 0, False, 1, base={})
        self.assertNotIn("PLXNATIVE_DUMP_KEYS", plain)
        self.assertNotIn("PLXNATIVE_DUMP_EXTRA_HOLDS", plain)
        on = sv.sim_env("/rt", "/out", "/rt/fifo", 1, 0, False, 1, base={}, keys="100:ok", extra_holds="r7")
        self.assertEqual((on["PLXNATIVE_DUMP_KEYS"], on["PLXNATIVE_DUMP_EXTRA_HOLDS"]), ("100:ok", "r7"))
        zero = sv.sim_env("/rt", "/out", "/rt/fifo", 1, 0, False, 1, base={}, extra_holds=0)
        self.assertEqual(zero["PLXNATIVE_DUMP_EXTRA_HOLDS"], "0", "k=0 is a value, not 'unset'")

    def test_a_failed_run_leaves_no_master_for_encode_to_adopt(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = pathlib.Path(tmp)
            (out / "master.mkv").write_bytes(b"m")
            (out / "frames.tsv").write_text("t")
            (out / "dump.json").write_text("{}")
            sv.quarantine(out)
            self.assertFalse((out / "master.mkv").exists() or (out / "frames.tsv").exists())
            self.assertEqual((out / "master.partial.mkv").read_bytes(), b"m")
            self.assertTrue((out / "frames.partial.tsv").exists() and (out / "dump.json").exists())
            sv.quarantine(out)  # nothing left to move: not an error

    def test_the_simulator_env_drops_every_inherited_plxnative_variable(self):
        env = sv.sim_env("/rt", "/out", "/rt/fifo", 180, 60, False, 60000,
                         base={"PLXNATIVE_TV": "x", "PATH": "/bin", "PLXNATIVE_DUMP_NO_HOLD": "1"})
        self.assertEqual(env["PATH"], "/bin")
        self.assertNotIn("PLXNATIVE_TV", env)
        self.assertNotIn("PLXNATIVE_DUMP_NO_HOLD", env, "a developer's switch must not reach a render")
        self.assertNotIn("PLXNATIVE_DUMP_ALLOW_ABSENT", env, "off unless asked")
        self.assertEqual((env["PLXNATIVE_DUMP"], env["PLXNATIVE_DUMP_FRAMES"], env["PLXNATIVE_WIN"]),
                         ("/out", "180", "1920x1080"))
        on = sv.sim_env("/rt", "/out", "/rt/fifo", 1, 0, True, 1, base={}, no_hold=True)
        self.assertEqual((on["PLXNATIVE_DUMP_ALLOW_ABSENT"], on["PLXNATIVE_DUMP_NO_HOLD"]), ("1", "1"))

    def test_the_record_it_builds_carries_the_facts(self):
        dump = {"frames_with_debt": 0, "wall_ms": 9}
        rec = sv.build_render_record({"scene": "home"}, dump, 180, 180, "darwin-local", FACTS)
        self.assertEqual(rec["hero_pool"], {"logged": ["10"], "eligible": ["10"], "turns": []})
        self.assertEqual(rec["opened_rating_keys"], ["11"])
        self.assertEqual(rec["not_in_video"], ["20"])

    def test_the_record_says_whether_the_hold_hint_was_suppressed(self):
        dump = {"frames_with_debt": 0}
        self.assertEqual(sv.build_render_record({"scene": "home"}, dump, 3, 3, "x", FACTS)["hold_hint"], "default")
        film = sv.build_render_record({"scene": "home", "hold_hint": "suppressed"}, dump, 3, 3, "x", FACTS)
        self.assertEqual(film["hold_hint"], "suppressed")
        self.assertEqual(sv.HINT_OFF, {"hintoff": ""}, "the trigger the film arms at boot (`app::boot`)")

    def test_the_record_it_builds_passes_the_validator(self):
        dump = {"frames_with_debt": 0, "width": 1920, "height": 1080, "preroll": 60, "iterations": 5, "holds": 2,
                "hold_reasons": {}, "unconverted_takes": [], "clock_origin_ms": 1000000, "wall_ms": 9}
        rec = sv.build_render_record({"scene": "home"}, dump, 180, 180, "darwin-local", FACTS)
        self.assertEqual(sv.validate_render_record(rec), [])
        self.assertEqual(rec["frames"], {"count": 180, "fps": 60, "width": 1920, "height": 1080})
        self.assertEqual(rec["sentinel"], {"scanned_frames": 180, "hits": 0})
        again = sv.build_render_record({"scene": "home"}, dump, 180, 180, "darwin-local", FACTS)
        self.assertEqual(rec["storyboard_sha256"], again["storyboard_sha256"], "a pure function of the storyboard")

    def test_a_missing_simulator_is_refused_before_anything_starts(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(sv.Failure) as cm:
                sv.run_render(pathlib.Path(tmp, "nope"), tmp, "ffmpeg", 3)
            self.assertIn("make site-video-sim", str(cm.exception))


class Triggers(unittest.TestCase):
    def test_a_trigger_is_a_lower_case_name_and_a_value(self):
        self.assertEqual(sv.parse_triggers(["detail=102", "heropin=0"]), {"detail": "102", "heropin": "0"})
        self.assertEqual(sv.parse_triggers(None), {})
        self.assertEqual(sv.parse_triggers(["x="]), {"x": ""})

    def test_a_name_that_could_leave_the_instance_root_is_refused(self):
        for bad in ("../x=1", "Detail=1", "a/b=1", "=1", "detail"):
            with self.assertRaises(sv.Failure, msg=bad):
                sv.parse_triggers([bad])


class HoldInjectionGate(unittest.TestCase):
    """`compare_hold_injection`: the logic behind `site_video.py hold-gate`. A render with extra held
    repeats on every clean frame must write the frames of the render with none."""

    @staticmethod
    def rows(hashes, holds):
        return [(i, (i * 1000 + 30) // 60, h, 0, k) for i, (h, k) in enumerate(zip(hashes, holds))]

    def test_identical_frames_with_more_holds_pass(self):
        base = self.rows([1, 2, 3], [0, 0, 0])
        inj = self.rows([1, 2, 3], [2, 0, 3])
        r = sv.compare_hold_injection(base, inj)
        self.assertEqual(r.status, sv.PASS, r.detail)
        self.assertIn("5 extra", r.detail)

    def test_one_different_pixel_fails_and_names_the_frame_and_the_column(self):
        r = sv.compare_hold_injection(self.rows([1, 2, 3], [0, 0, 0]), self.rows([1, 9, 3], [1, 1, 1]))
        self.assertEqual(r.status, sv.FAIL)
        self.assertIn("frame 1", r.detail)
        self.assertIn("hash", r.detail)

    def test_time_and_debt_and_length_are_compared_too(self):
        base = self.rows([1, 2], [0, 0])
        skewed = [(0, 0, 1, 0, 1), (1, 99, 2, 0, 1)]
        self.assertIn("t_ms", sv.compare_hold_injection(base, skewed).detail)
        debt = [(0, 0, 1, 0, 1), (1, 17, 2, 4, 1)]
        self.assertIn("debt", sv.compare_hold_injection(base, debt).detail)
        self.assertEqual(sv.compare_hold_injection(base, self.rows([1], [1])).status, sv.FAIL)

    def test_an_injection_that_did_not_run_is_not_a_pass(self):
        same = self.rows([1, 2, 3], [0, 0, 0])
        r = sv.compare_hold_injection(same, list(same))
        self.assertEqual(r.status, sv.FAIL)
        self.assertIn("did not run", r.detail)

    def test_nothing_written_is_not_a_pass(self):
        self.assertEqual(sv.compare_hold_injection([], []).status, sv.FAIL)

    def test_the_holds_column_alone_never_fails_it(self):
        base = self.rows([5, 6], [3, 3])
        inj = self.rows([5, 6], [7, 9])
        self.assertEqual(sv.compare_hold_injection(base, inj).status, sv.PASS)


class SizeGate(unittest.TestCase):
    def make(self, tmp, today, new):
        site, out = pathlib.Path(tmp, "site"), pathlib.Path(tmp, "out")
        site.mkdir()
        out.mkdir()
        for name in sv.MEDIA_NAMES:
            (site / name).write_bytes(b"x" * today)
            (out / name).write_bytes(b"y" * new)
        return out, site

    def test_the_limit_is_1_25x_of_the_file_read_now(self):
        with tempfile.TemporaryDirectory() as tmp:
            out, site = self.make(tmp, 1000, 1250)
            self.assertEqual({r.status for r in sv.gate_sizes(out, site)}, {sv.PASS})
            (out / sv.VIDEOS[0].name).write_bytes(b"y" * 1251)
            res = {r.name: r for r in sv.gate_sizes(out, site)}
            self.assertEqual(res[f"size/{sv.VIDEOS[0].name}"].status, sv.FAIL)
            self.assertIn("1.25x", res[f"size/{sv.VIDEOS[0].name}"].detail)
            # today's file is whatever is in site/media NOW, not a constant: grow it and the same encode passes
            (site / sv.VIDEOS[0].name).write_bytes(b"x" * 2000)
            res = {r.name: r for r in sv.gate_sizes(out, site)}
            self.assertEqual(res[f"size/{sv.VIDEOS[0].name}"].status, sv.PASS)

    def test_a_missing_encode_fails_and_a_missing_site_file_is_reported_not_passed(self):
        with tempfile.TemporaryDirectory() as tmp:
            out, site = self.make(tmp, 1000, 900)
            (out / sv.POSTERS[0].name).unlink()
            (site / sv.POSTERS[1].name).unlink()
            res = {r.name: r for r in sv.gate_sizes(out, site)}
            self.assertEqual(res[f"size/{sv.POSTERS[0].name}"].status, sv.FAIL)
            self.assertEqual(res[f"size/{sv.POSTERS[1].name}"].status, sv.SKIP)

    def test_the_real_site_media_has_every_file_the_gate_compares_with(self):
        for name in sv.MEDIA_NAMES:
            self.assertTrue((sv.SITE_MEDIA / name).is_file(), name)


class MeasurementHelpers(unittest.TestCase):
    def test_ssim_stats_and_the_gate(self):
        text = "n:1 Y:0.99 U:0.99 V:0.99 All:0.990000 (20.0)\nn:2 Y:0.9 U:0.9 V:0.9 All:0.960000 (14.0)\n"
        values = sv.parse_ssim_stats(text)
        self.assertEqual(values, [0.99, 0.96])
        res = sv.gate_ssim("a.mp4", values, 0.97, 0.985, 2)
        self.assertEqual(res.status, sv.FAIL)
        self.assertIn("worst frame 1", res.detail)
        self.assertEqual(sv.gate_ssim("a.mp4", [0.99, 0.98], 0.97, 0.985, 2).status, sv.PASS)
        self.assertEqual(sv.gate_ssim("a.mp4", [0.99, 0.98], 0.97, 0.985, 3).status, sv.FAIL)  # a frame went missing
        self.assertEqual(sv.gate_ssim("a.mp4", [0.99, 0.975, 0.975], 0.97, 0.985, 3).status, sv.FAIL)  # mean

    def test_vmaf_is_detected_by_the_filter_list_and_a_missing_filter_is_reported_not_hidden(self):
        listing = " TS ssim               VV->V      Calculate the SSIM.\n .. libvmaf            VV->V      Calculate the VMAF.\n"

        def fake(out):
            return lambda argv, **kw: subprocess.CompletedProcess(argv, 0, out, "")

        self.assertTrue(sv.have_filter("/t/ffmpeg", "libvmaf", fake(listing)))
        self.assertFalse(sv.have_filter("/t/ffmpeg", "libvmaf", fake(listing.replace("libvmaf", "xpsnr"))))
        res = sv.gate_vmaf("a.mp4", 90.0, 70.0)
        self.assertEqual(res.status, sv.FAIL)
        self.assertEqual(sv.gate_vmaf("a.mp4", 95.0, 85.0).status, sv.PASS)
        # the skip is a named result, which `format_results` prints and `gates` summarises on stderr
        self.assertIn("SKIP", sv.format_results([sv.Result("vmaf/a.mp4", sv.SKIP, "libvmaf is not in this ffmpeg")]))

    def test_the_seam_gate(self):
        self.assertEqual(sv.gate_seam("m", 0.995).status, sv.PASS)
        self.assertEqual(sv.gate_seam("m", 0.9949).status, sv.FAIL)

    def test_thresholds_are_the_specs_and_one_table(self):
        t = sv.THRESHOLDS
        self.assertEqual((t["ssim_min"], t["ssim_mean"], t["size_ratio"], t["seam_ssim"]), (0.97, 0.985, 1.25, 0.995))
        self.assertIn("calibrated on the first render, then frozen", pathlib.Path(sv.__file__).read_text().lower())

    def test_parse_probe_reads_the_stream_line_and_the_last_frame_count(self):
        info = sv.parse_probe(
            "  Stream #0:0[0x1](und): Video: av1 (libdav1d) (Main) (av01 / 0x31307661), yuv420p(tv, bt709, progressive), "
            "1280x720 [SAR 1:1 DAR 16:9], 1381 kb/s, 60 fps, 60 tbr, 15360 tbn (default)\n"
            "frame=  600 fps=0.0\rframe= 1290 fps=0.0 q=-1.0 Lsize=N/A\n")
        self.assertEqual((info["codec"], info["pix_fmt"], info["width"], info["height"], info["fps"], info["frames"]),
                         ("av1", "yuv420p", 1280, 720, 60.0, 1290))
        self.assertFalse(info["audio"])
        self.assertTrue(sv.parse_probe("Stream #0:0: Video: h264, yuv420p, 8x8, 1 fps\nStream #0:1: Audio: aac, 44100 Hz")["audio"])
        with self.assertRaises(sv.Failure):
            sv.parse_probe("nothing useful")

    def test_the_format_gate_names_each_deviation(self):
        video = sv.VIDEOS[0]
        good = {"codec": "av1", "pix_fmt": "yuv420p", "color": "tv, bt709, progressive", "width": 1280, "height": 720,
                "fps": 60.0, "frames": 10, "audio": False, "profile": "Main"}
        self.assertEqual(sv.gate_format(good, video.name, video, 10).status, sv.PASS)
        bad = dict(good, codec="h264", pix_fmt="yuv420p10le", color="pc, bt470bg", width=1920, frames=9, audio=True, fps=30.0)
        res = sv.gate_format(bad, video.name, video, 10)
        self.assertEqual(res.status, sv.FAIL)
        for needle in ("codec h264", "pix_fmt yuv420p10le", "tv", "bt709", "1920x720", "9 frames", "audio", "30.0 fps"):
            self.assertIn(needle, res.detail)
        poster = sv.POSTERS[1]
        jpg = {"codec": "mjpeg", "pix_fmt": "yuvj420p", "color": "pc", "width": 960, "height": 540, "fps": 25.0,
               "frames": 1, "audio": False, "profile": "Baseline"}
        self.assertEqual(sv.gate_format(jpg, poster.name, poster, 10).status, sv.PASS)

    def test_flat_fraction_and_the_banding_gate(self):
        w, h = 16, 8
        smooth = bytes((x * 15 + y) % 256 for y in range(h) for x in range(w))  # changes everywhere
        banded = bytes((y // 4) * 40 for y in range(h) for x in range(w))  # two big flat steps
        self.assertLess(sv.flat_fraction(smooth, w, h), 0.2)
        self.assertGreater(sv.flat_fraction(banded, w, h), 0.9)
        box = (0, 0, w, h)
        ok = sv.gate_banding("a.mp4", [smooth], [smooth], box)
        self.assertEqual(ok.status, sv.PASS)
        res = sv.gate_banding("a.mp4", [banded], [smooth], box)  # the encode flattened a gradient the master had
        self.assertEqual(res.status, sv.FAIL)
        self.assertEqual(sv.gate_banding("a.mp4", [banded], [], box).status, sv.FAIL)

    def test_the_roi_box_is_even_and_inside_the_frame(self):
        for w, h in ((1920, 1080), (1280, 720)):
            x, y, bw, bh = sv.roi_box(w, h)
            self.assertTrue(all(v % 2 == 0 for v in (x, y, bw, bh)))
            self.assertLessEqual(x + bw, w)
            self.assertLessEqual(y + bh, h)

    def test_evenly_spaced_frames_include_both_ends(self):
        self.assertEqual(sv.evenly_spaced(1290, 4), [0, 430, 859, 1289])
        self.assertEqual(sv.evenly_spaced(3, 12), [0, 1, 2])
        self.assertEqual(sv.evenly_spaced(10, 1), [0])

    def test_platform_is_linux_ci_only_on_a_github_linux_runner(self):
        self.assertEqual(sv.current_platform({"GITHUB_ACTIONS": "true"}, "Linux"), "linux-ci")
        self.assertEqual(sv.current_platform({}, "Linux"), "linux-local")
        self.assertEqual(sv.current_platform({"GITHUB_ACTIONS": "true"}, "Darwin"), "darwin-local")


class GatesCommand(unittest.TestCase):
    def test_the_gates_are_handed_absolute_paths_because_ffmpeg_probes_run_in_a_scratch_directory(self):
        seen = {}

        def fake(d, m, fb, *a, **k):
            seen.update(dir=d, master=m, frames_b=fb)
            return [sv.Result("x", sv.PASS, "")]
        args = type("A", (), {"dir": "out/deliver", "master": "out/a/master.mkv", "frames_b": "out/b/frames.tsv",
                              "site_media": "site/media", "ffmpeg": None, "cache": None})()
        with unittest.mock.patch.object(sv, "_ffmpeg_from", return_value=("/t/ffmpeg", True)), \
                unittest.mock.patch.object(sv, "run_gates", fake), \
                unittest.mock.patch.object(sv, "write_gates_json", return_value=True) as written:
            self.assertEqual(sv.cmd_gates(args), 0)
        self.assertTrue(all(os.path.isabs(v) for v in seen.values()), seen)
        self.assertTrue(os.path.isabs(written.call_args[0][0]))


class Environment(unittest.TestCase):
    GLX = ("name of display: :99\nOpenGL vendor string: Mesa\n"
           "OpenGL renderer string: llvmpipe (LLVM 17.0.6, 256 bits)\n"
           "OpenGL core profile version string: 4.5 (Core Profile) Mesa 24.0.9-0ubuntu0.2\n"
           "OpenGL version string: 4.5 (Compatibility Profile) Mesa 24.0.9-0ubuntu0.2\n")

    def test_glxinfo_yields_the_renderer_mesa_and_llvm_versions(self):
        got = sv.parse_glx_info(self.GLX)
        self.assertEqual((got["mesa"], got["llvm"]), ("24.0.9-0ubuntu0.2", "17.0.6"))
        self.assertEqual(got["renderer"], "llvmpipe (LLVM 17.0.6, 256 bits)")

    def test_a_missing_glxinfo_leaves_every_field_none_rather_than_failing(self):
        self.assertEqual(set(sv.parse_glx_info("").values()), {None})

    def test_the_record_names_the_image_the_cpu_and_the_steering_variables(self):
        with tempfile.TemporaryDirectory() as root:
            (pathlib.Path(root) / "etc").mkdir()
            (pathlib.Path(root) / "proc").mkdir()
            (pathlib.Path(root) / "etc/os-release").write_text('NAME="Ubuntu"\nPRETTY_NAME="Ubuntu 24.04.3 LTS"\n')
            (pathlib.Path(root) / "proc/cpuinfo").write_text("processor\t: 0\nmodel name\t: AMD EPYC 7763 64-Core Processor\n")
            env = {"ImageOS": "ubuntu24", "ImageVersion": "20251006.1.0", "LP_NATIVE_VECTOR_WIDTH": "256", "TZ": "UTC"}
            got = sv.collect_environment(env, run=lambda *a, **k: subprocess.CompletedProcess(a, 1, "", ""), root=root)
        self.assertEqual(got["runner_image"], {"os": "ubuntu24", "version": "20251006.1.0"})
        self.assertEqual(got["os"], "Ubuntu 24.04.3 LTS")
        self.assertEqual(got["cpu"]["model"], "AMD EPYC 7763 64-Core Processor")
        self.assertEqual(got["env"]["LP_NATIVE_VECTOR_WIDTH"], "256")
        self.assertIsNone(got["env"]["LC_ALL"])
        self.assertIsNone(got["gl"]["mesa"])


class EncodeArgv(unittest.TestCase):
    def plan(self):
        return dict(sv.encode_plan("/t/ffmpeg", "/m/master.mkv", "/o"))

    def test_the_site_names_match_index_html_and_styles(self):
        html = (ROOT / "site" / "index.html").read_text()
        feel = html[html.index('id="feel"'):html.index('id="why"')]
        sources = re.findall(r'<source\s+(?:media="[^"]*"\s+)?src="media/([^"]+)"', feel)
        self.assertEqual(sources, [v.name for v in sv.VIDEOS])
        for v in sv.VIDEOS:
            self.assertIn(f"{v.height}p60", v.name)
            self.assertIn(v.codec, v.name)
        css = (ROOT / "site" / "styles.css").read_text()
        for p in sv.POSTERS:
            self.assertIn(f"media/{p.name}", css)

    def test_av1_is_exactly_the_specified_settings(self):
        argv = self.plan()["feel-1080p60.av1.mp4"]
        self.assertEqual(argv, [
            "/t/ffmpeg", "-hide_banner", "-nostdin", "-loglevel", "warning", "-stats", "-y", "-i", "/m/master.mkv",
            "-map", "0:v:0", "-an", "-fps_mode", "passthrough",
            "-vf", "scale=1920:1080:flags=lanczos+accurate_rnd+full_chroma_int+error_diffusion:in_range=full:"
                   "out_range=tv:out_color_matrix=bt709,format=yuv420p",
            "-c:v", "libsvtav1", "-preset", "4", "-crf", "22", "-g", "480", "-svtav1-params", "tune=0:film-grain=0",
            "-pix_fmt", "yuv420p",
            "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv",
            "-movflags", "+faststart", "/o/feel-1080p60.av1.mp4"])

    def test_the_phone_av1_keeps_a_longer_gop_and_nothing_else_differs(self):
        small, big = self.plan()["feel-720p60.av1.mp4"], self.plan()["feel-1080p60.av1.mp4"]
        self.assertEqual(small[small.index("-g") + 1], sv.AV1_720_GOP)
        self.assertEqual(big[big.index("-g") + 1], "480")
        self.assertEqual(small[small.index("-crf") + 1], sv.AV1_720_CRF)
        self.assertEqual(big[big.index("-crf") + 1], "22")
        self.assertGreater(int(sv.AV1_720_GOP), 480, "the phone file's size headroom is bought with its GOP")
        small[small.index("-g") + 1] = "480"
        small[small.index("-crf") + 1] = "22"
        self.assertEqual([a for a in small if "1280" not in a and "720" not in a],
                         [a for a in big if "1920" not in a and "1080" not in a])

    def test_the_encodes_are_tuned_for_headroom_and_no_floor_was_moved(self):
        """The 1080p AV1 failed min VMAF 79.69 < 80.0 on the Linux render at CRF 28; the fix is the encode, never the floor."""
        self.assertEqual((sv.THRESHOLDS["vmaf_min"], sv.THRESHOLDS["vmaf_mean"]), (80.0, 93.0))
        self.assertEqual((sv.THRESHOLDS["ssim_min"], sv.THRESHOLDS["ssim_mean"], sv.THRESHOLDS["size_ratio"]),
                         (0.97, 0.985, 1.25))
        for name in ("feel-1080p60.av1.mp4", "feel-720p60.av1.mp4"):
            argv = self.plan()[name]
            self.assertLessEqual(int(argv[argv.index("-crf") + 1]), 22, name)

    def test_h264_is_exactly_the_specified_settings(self):
        argv = self.plan()["feel-720p60.h264.mp4"]
        self.assertEqual(argv, [
            "/t/ffmpeg", "-hide_banner", "-nostdin", "-loglevel", "warning", "-stats", "-y", "-i", "/m/master.mkv",
            "-map", "0:v:0", "-an", "-fps_mode", "passthrough",
            "-vf", "scale=1280:720:flags=lanczos+accurate_rnd+full_chroma_int+error_diffusion:in_range=full:"
                   "out_range=tv:out_color_matrix=bt709,format=yuv420p",
            "-c:v", "libx264", "-preset", "veryslow", "-crf", "18", "-tune", "animation", "-profile:v", "high",
            "-pix_fmt", "yuv420p", "-x264-params", "aq-mode=3:deblock=-1,-1",
            "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv",
            "-movflags", "+faststart", "/o/feel-720p60.h264.mp4"])

    def test_every_encode_drops_audio_and_nothing_names_libaom(self):
        plan = self.plan()
        self.assertEqual(sorted(plan), sorted(sv.MEDIA_NAMES))
        for name, argv in plan.items():
            with self.subTest(name):
                self.assertNotIn("libaom-av1", argv)
                self.assertNotIn("libaom", " ".join(argv))
        for v in sv.VIDEOS:
            self.assertIn("-an", plan[v.name])
            self.assertEqual(plan[v.name][plan[v.name].index("-c:v") + 1], "libsvtav1" if v.codec == "av1" else "libx264")

    def test_the_posters_are_frame_zero_as_bt601_full_range_jpeg(self):
        argv = self.plan()["feel-poster-narrow.jpg"]
        self.assertEqual(argv[argv.index("-frames:v") + 1], "1")
        self.assertNotIn("-ss", argv)  # frame 0, not a seek
        vf = argv[argv.index("-vf") + 1]
        self.assertIn("scale=960:540", vf)
        self.assertIn("out_range=pc:out_color_matrix=bt601", vf)
        self.assertTrue(vf.endswith("format=yuvj420p"))

    def test_the_encode_dither_is_on(self):
        for name, argv in self.plan().items():
            self.assertIn("error_diffusion", argv[argv.index("-vf") + 1], name)

    def test_run_encode_runs_the_plan_and_stops_at_the_first_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            tsv_path = pathlib.Path(tmp, "frames.tsv")
            tsv_path.write_text(tsv(GOOD_ROWS))
            seen = []

            def run(argv, **kw):
                if argv[1:3] == ["-hide_banner", "-encoders"]:
                    return subprocess.CompletedProcess(argv, 0, " V..... libsvtav1  x\n V....D libx264  y\n", "")
                seen.append(argv[-1])
                return subprocess.CompletedProcess(argv, 0 if len(seen) < 3 else 1)

            with self.assertRaises(sv.Failure):
                sv.run_encode("/t/ffmpeg", "/m.mkv", tsv_path, pathlib.Path(tmp, "out"), run=run)
            self.assertEqual([pathlib.Path(p).name for p in seen], [sv.VIDEOS[0].name, sv.VIDEOS[1].name, sv.VIDEOS[2].name])

    def test_run_encode_refuses_an_ffmpeg_without_the_encoders_before_running_anything(self):
        with tempfile.TemporaryDirectory() as tmp:
            tsv_path = pathlib.Path(tmp, "frames.tsv")
            tsv_path.write_text(tsv(GOOD_ROWS))
            calls = []

            def run(argv, **kw):
                calls.append(argv)
                return subprocess.CompletedProcess(argv, 0, " V....D libaom-av1  libaom\n V....D libx264  y\n", "")

            with self.assertRaises(sv.Failure) as cm:
                sv.run_encode("/t/ffmpeg", "/m.mkv", tsv_path, pathlib.Path(tmp, "out"), run=run)
            self.assertIn("libsvtav1", str(cm.exception))
            self.assertIn("never falls back to it", str(cm.exception))
            self.assertEqual(len(calls), 1)


FAKE_FFMPEG = """#!/bin/sh
case "$*" in
  *-encoders*) printf ' V..... libsvtav1  SVT-AV1\\n V....D libx264  x264\\n V....D libaom-av1  aom\\n' ;;
  *-version*) echo 'ffmpeg version fake' ;;
esac
"""
FAKE_NO_SVT = FAKE_FFMPEG.replace(" V..... libsvtav1  SVT-AV1\\n", "")


def tar_xz(member, data):
    import tarfile
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:xz") as t:
        info = tarfile.TarInfo(member)
        info.size, info.mode = len(data), 0o755
        t.addfile(info, io.BytesIO(data))
    return buf.getvalue()


class FakeOpener:
    """Serves {url: bytes or an HTTP status} and counts requests."""

    def __init__(self, table):
        self.table, self.requests = table, []

    def __call__(self, req, timeout=None):
        url = req.full_url
        self.requests.append(url)
        got = self.table.get(url, 404)
        if isinstance(got, int):
            raise urllib.error.HTTPError(url, got, "fake", {}, None)
        return io.BytesIO(got)


class FetchFfmpeg(unittest.TestCase):
    def pin(self, archive, urls=("http://x/ffmpeg.tar.xz",), sha=None):
        return sv.Pin("linux-x86_64", urls, sha or hashlib.sha256(archive).hexdigest(), "tar.xz", "/bin/ffmpeg",
                      "test", "GPL", "test")

    def test_a_good_download_is_verified_extracted_checked_and_then_cached(self):
        archive = tar_xz("ffmpeg-1/bin/ffmpeg", FAKE_FFMPEG.encode())
        opener = FakeOpener({"http://x/ffmpeg.tar.xz": archive})
        with tempfile.TemporaryDirectory() as cache:
            path = sv.fetch_ffmpeg(self.pin(archive), cache, opener, sleep=lambda s: None)
            self.assertTrue(os.access(path, os.X_OK))
            self.assertTrue(str(path).startswith(cache))
            self.assertEqual(len(opener.requests), 1)
            self.assertEqual(sv.fetch_ffmpeg(self.pin(archive), cache, opener, sleep=lambda s: None), path)
            self.assertEqual(len(opener.requests), 1, "the cached, re-verified binary is not downloaded again")
            path.write_bytes(path.read_bytes() + b"# tampered\n")  # a changed cached binary is not trusted
            sv.fetch_ffmpeg(self.pin(archive), cache, opener, sleep=lambda s: None)
            self.assertEqual(len(opener.requests), 2)

    def test_a_hash_mismatch_fails_closed_and_leaves_nothing_behind(self):
        archive = tar_xz("ffmpeg-1/bin/ffmpeg", FAKE_FFMPEG.encode())
        opener = FakeOpener({"http://x/ffmpeg.tar.xz": archive + b"tampered"})
        with tempfile.TemporaryDirectory() as cache:
            with self.assertRaises(sv.Failure) as cm:
                sv.fetch_ffmpeg(self.pin(archive), cache, opener, sleep=lambda s: None)
            self.assertIn("refusing", str(cm.exception))
            self.assertEqual([p for p in pathlib.Path(cache).rglob("*") if p.is_file()], [])

    def test_an_ffmpeg_without_libsvtav1_is_refused_even_with_libaom_present(self):
        archive = tar_xz("ffmpeg-1/bin/ffmpeg", FAKE_NO_SVT.encode())
        opener = FakeOpener({"http://x/ffmpeg.tar.xz": archive})
        with tempfile.TemporaryDirectory() as cache:
            with self.assertRaises(sv.Failure) as cm:
                sv.fetch_ffmpeg(self.pin(archive), cache, opener, sleep=lambda s: None)
            self.assertIn("libsvtav1", str(cm.exception))
            self.assertIn("never falls back", str(cm.exception))
            self.assertEqual([p for p in pathlib.Path(cache).rglob("ffmpeg")], [])

    def test_a_404_tries_the_next_url_and_5xx_backs_off_but_retries(self):
        archive = tar_xz("ffmpeg-1/bin/ffmpeg", FAKE_FFMPEG.encode())
        opener = FakeOpener({"http://mirror/ffmpeg.tar.xz": archive})
        pin = self.pin(archive, urls=("http://gone/ffmpeg.tar.xz", "http://mirror/ffmpeg.tar.xz"))
        with tempfile.TemporaryDirectory() as cache:
            sv.fetch_ffmpeg(pin, cache, opener, sleep=lambda s: None)
        self.assertEqual(opener.requests, ["http://gone/ffmpeg.tar.xz", "http://mirror/ffmpeg.tar.xz"])
        flaky = FakeOpener({"http://x/ffmpeg.tar.xz": 503})
        waits = []
        with tempfile.TemporaryDirectory() as cache, self.assertRaises(sv.Failure) as cm:
            sv.fetch_ffmpeg(self.pin(archive), cache, flaky, sleep=waits.append)
        self.assertEqual(len(flaky.requests), 4)
        self.assertEqual(waits, [1, 2, 4, 8])
        self.assertIn("mirror", str(cm.exception))

    def test_resolve_never_falls_back_to_a_system_ffmpeg(self):
        with tempfile.TemporaryDirectory() as cache, unittest.mock.patch.object(sv, "host_pin_key", return_value="linux-x86_64"):
            with unittest.mock.patch.object(shutil, "which", return_value="/usr/bin/ffmpeg") as which:
                with self.assertRaises(sv.Failure) as cm:
                    sv.resolve_ffmpeg(None, cache)
            which.assert_not_called()
            self.assertIn("ffmpeg-fetch", str(cm.exception))

    def test_an_explicit_ffmpeg_is_checked_and_marked_non_canonical(self):
        with tempfile.TemporaryDirectory() as tmp:
            good, bad = pathlib.Path(tmp, "good"), pathlib.Path(tmp, "bad")
            for p, body in ((good, FAKE_FFMPEG), (bad, FAKE_NO_SVT)):
                p.write_text(body)
                p.chmod(p.stat().st_mode | stat.S_IXUSR)
            err = io.StringIO()
            with unittest.mock.patch.object(sys, "stderr", err):
                path, canonical = sv.resolve_ffmpeg(str(good))
                self.assertFalse(canonical)
                self.assertIn("NOT the pinned", err.getvalue())
                with self.assertRaises(sv.Failure):
                    sv.resolve_ffmpeg(str(bad))

    def test_the_pins_are_complete_and_state_their_licence(self):
        self.assertIn("linux-x86_64", sv.PINS)
        for pin in sv.PINS.values():
            self.assertRegex(pin.sha256, r"^[0-9a-f]{64}$")
            self.assertTrue(all(u.startswith("https://") for u in pin.urls))
            self.assertIn("GPL", pin.licence)
        src = pathlib.Path(sv.__file__).read_text()
        self.assertIn("BtbN/FFmpeg-Builds", src)
        self.assertIn("libsvtav1", src)

    def test_the_encoder_listing_parser(self):
        listing = (" ------\n V..... libsvtav1            SVT-AV1(Scalable Video Technology for AV1) encoder (codec av1)\n"
                   " V....D libx264              libx264 H.264\n A....D aac                  AAC\n")
        self.assertEqual(sv.parse_encoders(listing), {"libsvtav1", "libx264", "aac"})


def fake_artifact(tmp, platform="linux-ci", passed=True, dirty=False, tamper=None, extra_member=None):
    """A directory of six fake media files, a frames.tsv and a manifest; returns (dir, zip path, manifest)."""
    out = pathlib.Path(tmp, "out")
    out.mkdir()
    for i, name in enumerate(sv.MEDIA_NAMES):
        (out / name).write_bytes(f"media {i}".encode() * 50)
    (out / "frames.tsv").write_text(tsv(GOOD_ROWS))
    gates = {"passed": passed, "results": [{"name": "x", "status": "pass", "detail": ""}]}
    manifest = sv.build_manifest(out, record(), dict(gates, passed=True), {"ffmpeg": {"sha256": "f" * 64}},
                                 {"combined": "c" * 64}, dirty, "d" * 64, platform)
    manifest["gates"]["passed"] = passed
    (out / sv.MANIFEST_NAME).write_text(json.dumps(manifest))
    if tamper:
        (out / tamper).write_bytes(b"not what was hashed")
    zpath = pathlib.Path(tmp, "artifact.zip")
    sv.make_artifact_zip(out, manifest, zpath)
    if extra_member:
        with zipfile.ZipFile(zpath, "a") as z:
            z.writestr(extra_member, b"x")
    return out, zpath, manifest


class ManifestAndAdopt(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = self._tmp.name
        self.site = pathlib.Path(self.tmp, "site-media")
        self.site.mkdir()
        (self.site / "feel-poster.jpg").write_bytes(b"OLD POSTER")
        self.say = []

    def tearDown(self):
        self._tmp.cleanup()

    def adopt(self, zpath, write=False):
        run = unittest.mock.Mock(side_effect=sv.Failure("no git in the test"))
        with unittest.mock.patch.object(sv, "tree_hashes", run):
            return sv.adopt(zpath, self.site, write, self.say.append)

    def test_a_manifest_lists_every_output_with_its_hash_and_verifies(self):
        out, _, manifest = fake_artifact(self.tmp)
        self.assertEqual(sorted(manifest["outputs"]), sorted(sv.MEDIA_NAMES))
        self.assertEqual(manifest["outputs"]["feel-poster.jpg"]["sha256"], sv.sha256_file(out / "feel-poster.jpg"))
        self.assertEqual(sorted(manifest["artifacts"]), ["frames.tsv"])
        for key in ("platform", "tools", "storyboard_sha256", "catalog_sha256", "tree_hash", "opened_rating_keys"):
            self.assertIn(key, manifest)
        self.assertEqual(sv.verify_manifest(manifest, out), [])

    def test_a_changed_byte_a_missing_file_or_a_changed_size_fails_verification(self):
        out, _, manifest = fake_artifact(self.tmp)
        target = out / "feel-720p60.av1.mp4"
        target.write_bytes(target.read_bytes()[:-1] + b"!")
        self.assertTrue(any("feel-720p60.av1.mp4" in p for p in sv.verify_manifest(manifest, out)))
        target.write_bytes(target.read_bytes() + b"more")
        self.assertTrue(any("bytes" in p for p in sv.verify_manifest(manifest, out)))
        target.unlink()
        self.assertTrue(any("absent" in p for p in sv.verify_manifest(manifest, out)))

    def test_a_manifest_naming_a_path_or_dropping_an_output_is_refused(self):
        out, _, manifest = fake_artifact(self.tmp)
        sneaky = json.loads(json.dumps(manifest))
        sneaky["artifacts"]["../evil"] = {"sha256": "0" * 64, "bytes": 1}
        self.assertTrue(any("plain file name" in p for p in sv.verify_manifest(sneaky, out)))
        short = json.loads(json.dumps(manifest))
        del short["outputs"]["feel-poster.jpg"]
        self.assertTrue(any("exactly the six" in p for p in sv.verify_manifest(short, out)))
        self.assertTrue(sv.verify_manifest({"schema": 99}, out))

    def test_no_manifest_is_written_over_failed_gates_or_missing_outputs(self):
        out, _, _ = fake_artifact(self.tmp)
        args = (record(), {"passed": False, "results": []}, {}, {}, False, "d", "linux-ci")
        with self.assertRaises(sv.Failure):
            sv.build_manifest(out, *args)
        (out / "feel-poster.jpg").unlink()
        with self.assertRaises(sv.Failure):
            sv.build_manifest(out, record(), {"passed": True, "results": []}, {}, {}, False, "d", "linux-ci")

    def test_adopt_refuses_a_manifest_whose_platform_is_not_linux_ci(self):
        for label in ("darwin-local", "linux-local", "windows-local", "", None):
            with self.subTest(label), tempfile.TemporaryDirectory() as tmp:
                _, zpath, _ = fake_artifact(tmp, platform=label)
                with self.assertRaises(sv.Failure) as cm:
                    self.adopt(zpath, write=True)
                self.assertIn("linux-ci", str(cm.exception))
                self.assertEqual(sorted(p.name for p in self.site.iterdir()), ["feel-poster.jpg"])
                self.assertEqual((self.site / "feel-poster.jpg").read_bytes(), b"OLD POSTER")

    def test_adopt_refuses_failed_gates_a_dirty_tree_and_a_tampered_file(self):
        cases = {"failed gates": dict(passed=False), "dirty tree": dict(dirty=True),
                 "tampered output": dict(tamper="feel-1080p60.av1.mp4")}
        for what, kw in cases.items():
            with self.subTest(what), tempfile.TemporaryDirectory() as tmp:
                _, zpath, _ = fake_artifact(tmp, **kw)
                with self.assertRaises(sv.Failure):
                    self.adopt(zpath, write=True)
                self.assertEqual((self.site / "feel-poster.jpg").read_bytes(), b"OLD POSTER")

    def test_adopt_refuses_a_zip_that_escapes_its_directory(self):
        for member in ("../escape.txt", "/abs.txt", "a\\b.txt"):
            with self.subTest(member), tempfile.TemporaryDirectory() as tmp:
                _, zpath, _ = fake_artifact(tmp, extra_member=member)
                with self.assertRaises(sv.Failure) as cm:
                    self.adopt(zpath)
                self.assertIn("unsafe", str(cm.exception))

    def test_a_dry_run_writes_nothing_and_write_copies_the_six_files_and_the_manifest(self):
        out, zpath, _ = fake_artifact(self.tmp)
        plan = self.adopt(zpath)
        self.assertEqual(len(plan), 7)
        self.assertEqual((self.site / "feel-poster.jpg").read_bytes(), b"OLD POSTER")
        self.assertEqual(sorted(p.name for p in self.site.iterdir()), ["feel-poster.jpg"])
        self.assertTrue(any("dry run" in line for line in self.say))
        self.adopt(zpath, write=True)
        self.assertEqual(sorted(p.name for p in self.site.iterdir()), sorted(sv.MEDIA_NAMES + (sv.MANIFEST_NAME,)))
        for name in sv.MEDIA_NAMES:
            self.assertEqual((self.site / name).read_bytes(), (out / name).read_bytes())
        committed = json.loads((self.site / sv.MANIFEST_NAME).read_text())
        self.assertEqual(sv.verify_manifest(committed, self.site, ("outputs",)), [])  # what S7's make check will assert

    def test_the_artifact_zip_is_a_function_of_its_files(self):
        out, zpath, manifest = fake_artifact(self.tmp)
        again = pathlib.Path(self.tmp, "again.zip")
        sv.make_artifact_zip(out, manifest, again)
        self.assertEqual(sv.sha256_file(zpath), sv.sha256_file(again))

    def test_tree_hashes_come_from_git_trees_of_everything_that_can_change_a_pixel(self):
        calls = []

        def run(argv, **kw):
            calls.append(argv)
            if "rev-parse" in argv:
                return subprocess.CompletedProcess(argv, 0, "tree-" + argv[-1].split(":")[-1] + "\n", "")
            return subprocess.CompletedProcess(argv, 0, " M rust-modules/gfx/src/x.rs\n", "")

        trees, dirty = sv.tree_hashes(run, "/repo")
        self.assertEqual([k for k in trees if k != "combined"],
                         ["rust-modules", "assets", "locales", "src", "tests/video"])
        self.assertEqual(trees["rust-modules"], "tree-rust-modules")
        self.assertTrue(dirty)
        # the dirty check names the same paths as the hash: a change to `gfx`, `machine`, the dump
        # driver or the storyboard is not missed because only four UI crates were named
        status = next(c for c in calls if "status" in c)
        self.assertEqual(status[status.index("--") + 1:], list(sv.TREE_PATHS))

    def test_a_change_under_rust_modules_moves_the_combined_hash(self):
        def combined(sha):
            def run(argv, **kw):
                if "rev-parse" in argv:
                    return subprocess.CompletedProcess(argv, 0, (sha if argv[-1] == "HEAD:rust-modules" else "same") + "\n", "")
                return subprocess.CompletedProcess(argv, 0, "", "")
            return sv.tree_hashes(run, "/repo")[0]["combined"]
        self.assertNotEqual(combined("a"), combined("b"), "a gfx/machine/framedump.rs change must change the tree hash")


class CommittedSiteMedia(unittest.TestCase):
    """What is committed in `site/media/` is the Linux CI film the manifest says it is, and the page
    declares the codecs the files really carry. These read the repository, not a fixture."""

    MEDIA = ROOT / "site" / "media"

    def manifest(self):
        return json.loads((self.MEDIA / sv.MANIFEST_NAME).read_text())

    def test_the_manifest_is_the_linux_ci_render(self):
        m = self.manifest()
        self.assertEqual(m["platform"], "linux-ci", "only the Linux workflow's render may be committed")
        self.assertTrue(m["gates"]["passed"])
        self.assertFalse(m["tree_dirty"])

    def test_every_listed_file_is_present_with_its_sha256(self):
        m = self.manifest()
        self.assertEqual(sorted(m["outputs"]), sorted(sv.MEDIA_NAMES))
        for name, entry in sorted(m["outputs"].items()):
            path = self.MEDIA / name
            self.assertTrue(path.is_file(), f"{name} is listed in the manifest but absent from site/media")
            self.assertEqual(sv.sha256_file(path), entry["sha256"], f"{name} differs from the manifest")
            self.assertEqual(path.stat().st_size, entry["bytes"], name)
        self.assertEqual(sv.verify_manifest(m, self.MEDIA, ("outputs",)), [])

    @staticmethod
    def box_payload(data, fourcc, size):
        i = data.find(fourcc)
        assert i > 0, f"no {fourcc!r} box"
        return data[i + 4:i + 4 + size]

    def codec_string(self, video):
        """The RFC 6381 string a player needs for this file, read from its sample-description box."""
        data = (self.MEDIA / video.name).read_bytes()
        if video.codec == "h264":
            _, profile, compat, level = self.box_payload(data, b"avcC", 4)
            return f"avc1.{profile:02X}{compat:02X}{level:02X}"
        b1, b2 = self.box_payload(data, b"av1C", 4)[1:3]
        profile, level = b1 >> 5, b1 & 31
        tier = "H" if b2 & 0x80 else "M"
        depth = 12 if b2 & 0x20 else 10 if b2 & 0x40 else 8
        return f"av01.{profile}.{level:02d}{tier}.{depth:02d}"

    def test_index_html_declares_the_codecs_the_files_carry(self):
        html = (ROOT / "site" / "index.html").read_text()
        feel = html[html.index('id="feel"'):html.index('id="why"')]
        declared = dict(re.findall(r'src="media/([^"]+)"\s+type=\'video/mp4; codecs="([^"]+)"\'', feel))
        self.assertEqual(sorted(declared), sorted(v.name for v in sv.VIDEOS))
        for v in sv.VIDEOS:
            self.assertEqual(declared[v.name], self.codec_string(v),
                             f"the <source> for {v.name} declares a different codec string than the file carries")


class RealFfmpeg(unittest.TestCase):
    @unittest.skipUnless(REAL_FFMPEG, "set PLXNATIVE_SITE_VIDEO_FFMPEG=<ffmpeg with libsvtav1 + libx264> to run the encode smoke case "
                                      "(needs a real ffmpeg; kept out of `make check`)")
    def test_a_four_frame_master_encodes_and_grades(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = pathlib.Path(tmp)
            master, out = tmp / "master.mkv", tmp / "out"
            subprocess.run([REAL_FFMPEG, "-hide_banner", "-v", "error", "-f", "lavfi", "-i",
                            "gradients=s=1920x1080:r=60:d=0.07:speed=0.00001,format=bgr0", "-frames:v", "4",
                            "-c:v", "ffv1", "-level", "3", "-g", "1", str(master)], check=True)
            (tmp / "frames.tsv").write_text(tsv([(i, f"{i:016x}", 0) for i in range(4)]))
            sv.run_encode(REAL_FFMPEG, master, tmp / "frames.tsv", out)
            info = sv.probe(REAL_FFMPEG, out / "feel-720p60.h264.mp4")
            self.assertEqual((info["codec"], info["width"], info["frames"]), ("h264", 1280, 4))
            values = sv.ssim_against_master(REAL_FFMPEG, out / "feel-720p60.h264.mp4", master, sv.video_filter(1280, 720), "yuv420p")
            self.assertEqual(len(values), 4)
            self.assertGreater(min(values), 0.97)


class PairingTests(unittest.TestCase):
    def test_ssim_and_vmaf_pair_frames_by_index_not_by_timestamp(self):
        seen = []

        def fake_run(argv, **kw):
            seen.append(argv[argv.index("-lavfi") + 1])
            raise sv.Failure("stop")

        for fn, args in ((sv.ssim_against_master, ("f", "d.mp4", "m.mkv", "scale=1:1", "yuv420p")),
                         (sv.vmaf_against_master, ("f", "d.mp4", "m.mkv", "scale=1:1"))):
            with self.assertRaises(sv.Failure):
                fn(*args, run=fake_run)
        for lavfi in seen:
            self.assertIn("[0:v]settb=1/60,setpts=N,", lavfi)
            self.assertIn("[1:v]settb=1/60,setpts=N,", lavfi)
        self.assertEqual(len(seen), 2)


class ManifestStoryboardTests(unittest.TestCase):
    def test_the_manifest_compares_the_file_a_render_played_not_the_records_own_hash(self):
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            path = pathlib.Path(d, "feel.json")
            path.write_bytes(b'{"schema": 1}')
            sha = sv.sha256_file(path)
            self.assertIsNone(sv.storyboard_file_problem({"storyboard_sha256": "x" * 64, "storyboard": {"file_sha256": sha}}, path))
            self.assertIn("differs", sv.storyboard_file_problem({"storyboard": {"file_sha256": "0" * 64}}, path))
            self.assertIsNone(sv.storyboard_file_problem({"storyboard": {"script": "hold"}}, path), "a plain render names no file")


class StoryboardTests(unittest.TestCase):
    """`tests/video/feel.json`: positions and durations compiled to the driver's key schedule."""

    def test_the_committed_storyboard_loads_and_compiles_keys_in_order_inside_the_run(self):
        sb, sha = sv.load_storyboard()
        self.assertRegex(sha, r"^[0-9a-f]{64}$")
        plan = sv.compile_storyboard(sb)
        keys = [(int(v), k) for v, k in (part.split(":") for part in plan["keys"].split(","))]
        self.assertEqual([k for _, k in keys], ["right", "right", "left", "left", "down", "right", "right", "ok", "back",
                          "up", "up", "right", "right", "ok", "right", "right", "back", "down"])
        self.assertEqual([v for v, _ in keys], sorted(v for v, _ in keys))
        self.assertLess(keys[-1][0] + sv.KEY_DOWN_FRAMES, plan["preroll"] + plan["frames"])
        self.assertTrue(27 * 60 <= plan["frames"] <= 30 * 60, plan["frames"])

    def test_a_beat_that_does_nothing_or_names_a_title_key_is_refused(self):
        import tempfile
        for beat in ({"name": "x"}, {"name": "x", "keys": ["menu"]}):
            with tempfile.NamedTemporaryFile("w", suffix=".json") as f:
                json.dump({"schema": 1, "fps": 60, "boot": {"preroll_ms": 0}, "beats": [beat]}, f)
                f.flush()
                with self.assertRaises(sv.Failure):
                    sv.load_storyboard(f.name)

    def test_durations_compile_to_frames_and_a_gap_separates_keys(self):
        sb = {"fps": 60, "boot": {"preroll_ms": 1000}, "beats": [
            {"name": "hold", "hold_ms": 500}, {"name": "two", "keys": ["up", "right"], "gap_ms": 100, "rest_ms": 200}]}
        plan = sv.compile_storyboard(sb)
        self.assertEqual(plan["preroll"], 60)
        self.assertEqual(plan["keys"], "90:up,102:right")
        self.assertEqual([(b["start"], b["end"]) for b in plan["beats"]], [(0, 30), (30, 30 + 6 + 6 + 6 + 12)])
        self.assertFalse(plan["loop"], "a storyboard that does not ask to loop is not gated on it")
        self.assertTrue(sv.compile_storyboard({**sb, "end": {"match_first_frame": True}})["loop"])

    def test_the_committed_storyboard_asks_to_end_on_its_first_frame(self):
        sb, _ = sv.load_storyboard()
        self.assertTrue(sv.compile_storyboard(sb)["loop"], "feel.json `end.match_first_frame` is read by `storyboard/loop`")

    def test_the_storyboard_pages_the_hero_and_opens_a_shelf_card_not_a_hero_page(self):
        sb, _ = sv.load_storyboard()
        names = [b["name"] for b in sb["beats"]]
        self.assertEqual(names[0], "hero")
        self.assertEqual(names[-1], "to-hero")
        by = {b["name"]: b for b in sb["beats"]}
        # The app's own edge rule: `right` on Info turns the carousel forward, `left` on Continue turns it
        # back, any other `left`/`right` moves between the two buttons. Walk the hero section (up to the
        # first `down`) and require what the film promises: two or three distinct pages shown, no OK on
        # the hero (the Detail is a shelf card's, never a title whose hero page the film showed), and the
        # first page again when the hero section ends.
        focus, page, shown = "continue", 0, [0]
        for b in sb["beats"]:
            for key in b.get("keys", ()):
                if key == "down":
                    break
                self.assertNotEqual(key, "ok", "no OK in the hero section")
                if key == "right" and focus == "continue":
                    focus = "info"
                elif key == "left" and focus == "info":
                    focus = "continue"
                elif key in ("left", "right"):
                    page += 1 if key == "right" else -1
                    shown.append(page)
            else:
                continue
            break
        self.assertIn(len(set(shown)), (2, 3), shown)
        self.assertEqual((page, focus), (0, "continue"), "the hero ends where it began")
        # The Detail: `down` to the shelf, two `right` to its third card (Tears of Steel), then OK, a long
        # rest on the Detail, and `back` to the shelf. The hold hint is suppressed, so no rest is capped.
        keys = [(b["name"], k) for b in sb["beats"] for k in b.get("keys", ())]
        flat = [k for _, k in keys]
        down = flat.index("down")
        self.assertEqual(flat[down:down + 5], ["down", "right", "right", "ok", "back"])
        self.assertGreaterEqual(by["detail"]["rest_ms"], 3500)
        self.assertEqual(by["detail"]["keys"], ["ok"])
        self.assertEqual(by["back-shelf"]["keys"], ["back"])
        self.assertEqual(flat.count("ok"), 2, "one OK on the shelf card and one on the Movies tab")
        self.assertLess(names.index("detail"), names.index("library"))

    def test_a_rest_verdict_tolerates_a_springs_tail_and_a_sliver(self):
        self.assertTrue(sv.rest_verdict(0, 0.0))
        self.assertTrue(sv.rest_verdict(sv.REST_TOLERANCE, 0.5), "a 3/255 tail over the whole frame is still rest")
        self.assertTrue(sv.rest_verdict(80, sv.REST_MAX_MOVED), "a few hundred pixels is not motion")
        self.assertFalse(sv.rest_verdict(sv.REST_TOLERANCE + 1, sv.REST_MAX_MOVED * 2))

    def test_the_rest_gate_names_a_beat_whose_last_frames_still_move(self):
        metrics = [{"beat": "still", "frames": [8, 20], "max_delta": 2, "moved": 0.0},
                   {"beat": "tail", "frames": [30, 42], "max_delta": 3, "moved": 0.4},
                   {"beat": "moving", "frames": [50, 62], "max_delta": 90, "moved": 0.02}]
        bad = sv.gate_beats_at_rest(metrics)
        self.assertEqual(len(bad), 1)
        self.assertIn("moving", bad[0])
        self.assertIn("2.00%", bad[0])
        self.assertEqual(sv.gate_beats_at_rest(metrics[:2]), [])

    @unittest.skipUnless(importlib.util.find_spec("PIL"), "Pillow absent (`python3 -m pip install Pillow`)")
    def test_rest_metrics_measures_decoded_frames_not_bytes(self):
        from PIL import Image
        beats = [{"name": "moving", "start": 0, "end": 20}, {"name": "still", "start": 20, "end": 40},
                 {"name": "tail", "start": 40, "end": 60}, {"name": "one-frame", "start": 60, "end": 61}]
        asked = []

        def fake_run(argv, check=False):
            asked.append(argv)
            out = pathlib.Path(argv[-1]).parent
            wanted = re.findall(r"eq\(n\\,(\d+)\)", argv[argv.index("-vf") + 1])
            for i, n in enumerate(sorted(int(x) for x in wanted)):
                img = Image.new("RGB", (100, 100), (10, 10, 10))
                if n == 19:
                    img.paste((200, 10, 10), (0, 0, 50, 50))
                if n == 59:
                    img.paste((13, 10, 10), (0, 0, 100, 100))
                img.save(out / f"{i + 1:06d}.png")

        got = {m["beat"]: m for m in sv.rest_metrics("ffmpeg", "m.mkv", beats, window=12, run=fake_run)}
        self.assertNotIn("one-frame", got, "a one-frame beat has nothing to compare")
        self.assertEqual((got["moving"]["frames"], got["moving"]["max_delta"]), ([7, 19], 190))
        self.assertAlmostEqual(got["moving"]["moved"], 0.25)
        self.assertEqual((got["still"]["max_delta"], got["still"]["moved"]), (0, 0.0))
        self.assertEqual((got["tail"]["max_delta"], got["tail"]["moved"]), (3, 0.0))
        self.assertEqual(sv.gate_beats_at_rest(list(got.values()))[0].split(" ")[0], "moving")
        self.assertEqual(len(asked), 1, "one decode for every probe frame")

    def test_the_storyboard_gate_refuses_an_opened_not_in_video_title_and_a_moving_beat(self):
        ok = sv.gate_storyboard_record(record(not_in_video=["20", "21"], storyboard={"rest": [
            {"beat": "a", "frames": [0, 12], "max_delta": 1, "moved": 0.0}]}))
        self.assertEqual([r.status for r in ok], [sv.PASS, sv.PASS, sv.SKIP, sv.SKIP], "no request audit, no loop measurement")
        self.assertEqual([r.name for r in ok], ["storyboard/opened", "storyboard/rest", "storyboard/no-playback", "storyboard/loop"])
        opened = sv.gate_storyboard_record(record(not_in_video=["11"], storyboard={"rest": []}))
        self.assertEqual(opened[0].status, sv.FAIL)
        self.assertIn("11", opened[0].detail)
        moving = sv.gate_storyboard_record(record(not_in_video=[], storyboard={"rest": [
            {"beat": "library", "frames": [0, 12], "max_delta": 90, "moved": 0.1}]}))
        self.assertEqual(moving[1].status, sv.FAIL)
        self.assertIn("library", moving[1].detail)
        plain = sv.gate_storyboard_record(record(not_in_video=[]))
        self.assertEqual(plain[1].status, sv.SKIP)

    def test_a_film_that_asked_the_mock_for_playback_fails(self):
        by_name = lambda results: {r.name: r for r in results}  # noqa: E731
        clean = by_name(sv.gate_storyboard_record(record(not_in_video=[], playback_requests=[])))
        self.assertEqual(clean["storyboard/no-playback"].status, sv.PASS)
        played = by_name(sv.gate_storyboard_record(record(not_in_video=[], playback_requests=["/library/parts/5/1/file.mkv"])))
        self.assertEqual(played["storyboard/no-playback"].status, sv.FAIL)
        self.assertIn("/library/parts/5", played["storyboard/no-playback"].detail)
        unaudited = by_name(sv.gate_storyboard_record(record(not_in_video=[])))
        self.assertEqual(unaudited["storyboard/no-playback"].status, sv.SKIP)

    def test_a_loop_film_must_end_within_one_level_of_its_first_frame(self):
        loop = lambda **kw: {n.name: n for n in sv.gate_storyboard_record(  # noqa: E731
            record(not_in_video=[], storyboard={"loop": {"frames": [0, 1640], **kw}}))}["storyboard/loop"]
        ok = loop(max_delta=1, differing=129)
        self.assertEqual(ok.status, sv.PASS)
        self.assertIn("129 px", ok.detail)
        self.assertIn("1/255", ok.detail)
        bad = loop(max_delta=2, differing=129)
        self.assertEqual(bad.status, sv.FAIL)
        self.assertIn("does not end on its first frame", bad.detail)
        self.assertEqual(loop(max_delta=0, differing=0).status, sv.PASS)
        none = {n.name: n for n in sv.gate_storyboard_record(record(not_in_video=[]))}["storyboard/loop"]
        self.assertEqual(none.status, sv.SKIP, "a record without a loop measurement (no `end.match_first_frame`) is not gated")

    @unittest.skipUnless(importlib.util.find_spec("PIL"), "Pillow absent (`python3 -m pip install Pillow`)")
    def test_loop_metrics_compares_the_first_and_last_decoded_frames(self):
        from PIL import Image
        asked = []

        def fake_run(argv, check=False):
            asked.append(argv)
            out = pathlib.Path(argv[-1]).parent
            self.assertEqual(sorted(re.findall(r"eq\(n\\,(\d+)\)", argv[argv.index("-vf") + 1])), ["0", "99"])
            first = Image.new("RGB", (40, 40), (10, 10, 10))
            last = Image.new("RGB", (40, 40), (10, 10, 10))
            last.paste((11, 10, 9), (0, 0, 5, 4))
            first.save(out / "000001.png")
            last.save(out / "000002.png")

        got = sv.loop_metrics("ffmpeg", "m.mkv", 100, run=fake_run)
        self.assertEqual(got, {"frames": [0, 99], "max_delta": 1, "differing": 20})
        self.assertEqual(len(asked), 1, "one decode for both frames")

    def test_film_facts_reads_the_hero_and_the_titles_the_mock_served(self):
        import threading
        import types
        pms = types.SimpleNamespace(
            lib=types.SimpleNamespace(by_slug={"sintel": 10, "tears": 11, "cosmos": 20}),
            lock=threading.Lock(),
            request_log=[{"method": "GET", "path": "/library/metadata/11"},
                         {"method": "GET", "path": "/library/metadata/11"},
                         {"method": "GET", "path": "/library/metadata/10/children"},
                         {"method": "PUT", "path": "/library/metadata/12"},
                         {"method": "GET", "path": "/library/metadata/10"}])
        catalog = {"hero": "sintel", "movies": [{"id": "cosmos", "not_in_video": True}, {"id": "tears"}, {"id": "gone", "not_in_video": True}]}
        facts = sv.film_facts(pms, None, catalog, "...home: hero pinned at slot 0...")
        self.assertEqual(facts["hero_logged"], ["10"])
        self.assertEqual(facts["opened"], ["11", "10"], "a page is one GET of the title; a repeat or a child path is not another")
        self.assertEqual(facts["not_in_video"], ["20"])
        self.assertEqual(facts["playback"], [])
        quiet = sv.film_facts(pms, "tears", catalog, "no hero line here")
        self.assertEqual((quiet["hero_logged"], quiet["hero_eligible"]), ([], ["11"]))

    def test_film_facts_reads_the_hero_pages_shown_from_the_event_log(self):
        import threading
        import types
        pms = types.SimpleNamespace(
            lib=types.SimpleNamespace(by_slug={"sintel": 10, "spring": 12, "tears": 11, "cosmos": 20}),
            lock=threading.Lock(), request_log=[])
        catalog = {"hero": "sintel", "movies": [{"id": "cosmos", "not_in_video": True}]}
        log = ("home: hero pinned at slot 0\nhome: hero page slot 1 rk=12\nx: y\n"
               "home: hero page slot 0 rk=10\nhome: hero page slot 7 rk=20\n")
        facts = sv.film_facts(pms, None, catalog, log, hero_ok=["sintel", "spring", "cosmos", "absent"])
        self.assertEqual(facts["hero_logged"], ["10", "12", "20"], "the pinned page, then each page turned to, once")
        self.assertEqual(facts["hero_turns"], ["12", "10", "20"], "every turn, in order")
        self.assertEqual(facts["hero_eligible"], ["10", "12"], "a flagged title is never eligible; an unknown slug is skipped")
        rec = sv.build_render_record({"scene": "home"}, {"frames_with_debt": 0}, 3, 3, "darwin-local", facts)
        self.assertEqual(rec["hero_pool"]["turns"], ["12", "10", "20"])
        self.assertEqual(sv.gate_hero_pool(rec).status, sv.FAIL, "a flagged page was shown: the gate says so")

    def test_film_facts_lists_every_playback_request_and_no_page_open(self):
        import threading
        import types
        pms = types.SimpleNamespace(
            lib=types.SimpleNamespace(by_slug={"sintel": 10}), lock=threading.Lock(),
            request_log=[{"method": "GET", "path": "/library/metadata/10"},
                         {"method": "GET", "path": "/library/parts/7/1/file.mkv"},
                         {"method": "POST", "path": "/:/timeline"},
                         {"method": "GET", "path": "/video/:/transcode/universal/start.mkv"}])
        facts = sv.film_facts(pms, None, {"hero": "sintel", "movies": []}, "")
        self.assertEqual(facts["opened"], ["10"])
        self.assertEqual(facts["playback"], ["/library/parts/7/1/file.mkv", "/:/timeline",
                                             "/video/:/transcode/universal/start.mkv"])


if __name__ == "__main__":
    if not REAL_FFMPEG:
        print("test_site_video: the real-ffmpeg encode case is SKIPPED (set PLXNATIVE_SITE_VIDEO_FFMPEG)", file=sys.stderr)
    unittest.main(verbosity=1)
