#!/usr/bin/env python3
"""
Host unit tests for the harness itself (`tests/run.py`) and for `tools/netcond.py` — stdlib
`unittest`, no TV, no PMS, no OUTBOUND network. Run by `make check` beside the Rust suite and
`ci/flavor.py --selftest`.

("no network" needs one qualification since the netcond tests landed: they bind and drive real
LOOPBACK sockets, because a token bucket is only interesting where it meets a socket, and a test
against the arithmetic alone would grade a function the proxy does not call. Nothing leaves the
machine, and nothing there binds a fixed port.)

Why this file exists at all: run.py is 2100 lines of Python that decides WHAT gets driven on the
one television, and until 2026-08-22 nothing tested a line of it. The specific thing it guards is
the skip channel — the rule that an `item` key this installation cannot resolve skips the cases
that need it instead of killing the run. That rule is invisible on the maintainer's machine, whose
overlay resolves all twelve keys, so a regression in it would be found only by the next stranger
who tried the suite and concluded the harness was broken. Every assertion here is about a code path
that a full local overlay never enters.

The load_manifest tests deliberately read the REAL tests/manifest.json (only the overlay is
synthetic): the invariants being checked — every case ends with an `rk` or a `skip`, a nested key
skips only its owner — are properties of the tracked matrix as it actually stands, and a case added
tomorrow that breaks one of them should fail here rather than on the television.
"""
import importlib.util
import inspect
import io
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock

TESTS_DIR = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(TESTS_DIR)
sys.path.insert(0, TESTS_DIR)
# APPENDED, not inserted: `tools/` is a grab-bag of scripts, and putting it ahead of `tests/` means
# the day somebody adds a `tools/run.py` the import below silently binds the wrong module and this
# whole suite grades a file nobody meant.
sys.path.append(os.path.join(REPO_ROOT, "tools"))
import netcond  # noqa: E402
import run  # noqa: E402  (path juggling above is the point)
import serve_fixtures  # noqa: E402

_FIXTURE_GEN_SPEC = importlib.util.spec_from_file_location(
    "plx_make_fixtures", os.path.join(TESTS_DIR, "fixtures", "make_fixtures.py"))
fixturegen = importlib.util.module_from_spec(_FIXTURE_GEN_SPEC)
_FIXTURE_GEN_SPEC.loader.exec_module(fixturegen)


def _manifest():
    with open(run.MANIFEST) as f:
        return json.load(f)


def _overlay(items):
    """A minimal, syntactically complete overlay carrying `items` and nothing optional."""
    return {"pms": {"host": "10.0.0.2", "port": 32400}, "tv": "10.0.0.3", "items": items}


class _Overlay:
    """Point run.MANIFEST_LOCAL at a temp overlay for the duration of a `with` block."""

    def __init__(self, local):
        self.local = local

    def __enter__(self):
        self.fh = tempfile.NamedTemporaryFile("w", suffix=".json", delete=False)
        json.dump(self.local, self.fh)
        self.fh.close()
        self.saved = run.MANIFEST_LOCAL
        run.MANIFEST_LOCAL = self.fh.name
        return self

    def __exit__(self, *exc):
        run.MANIFEST_LOCAL = self.saved
        os.unlink(self.fh.name)


class ItemResolution(unittest.TestCase):
    def test_placeholder_reads_as_absent(self):
        """The stranger's dominant path is `cp` the example, which ships all twelve keys bracketed.
        If only the ABSENT branch skipped, that path would still die — one guard further down."""
        items = {"present": 1234, "blank": "<ratingKey>"}
        self.assertEqual(run._item_rk(items, "present"), "1234")   # ints are stringified
        self.assertIsNone(run._item_rk(items, "blank"))
        self.assertIsNone(run._item_rk(items, "absent"))

    def test_reason_distinguishes_the_two(self):
        items = {"blank": "<ratingKey>"}
        self.assertIn("template placeholder", run._item_missing_reason(items, "blank"))
        self.assertIn("no `items` entry", run._item_missing_reason(items, "absent"))

    def test_resolve_sets_rk_or_skip_never_both(self):
        entries = [{"name": "a", "item": "have"}, {"name": "b", "item": "havenot"},
                   {"name": "c"}]  # an fps scene that needs no library item
        run._resolve_items(entries, {"have": 7})
        self.assertEqual(entries[0]["rk"], "7")
        self.assertNotIn("skip", entries[0])
        # `rk` stays ABSENT on a skip: every consumer sits behind main()'s partition, so a partition
        # that is ever wrong must raise KeyError naming the case — not drive the TV at some sentinel.
        self.assertNotIn("rk", entries[1])
        self.assertIn("skip", entries[1])
        self.assertNotIn("rk", entries[2])
        self.assertNotIn("skip", entries[2])


class FpsIdentity(unittest.TestCase):
    def test_every_route_except_login_gets_the_temporary_test_identity(self):
        """FPS evidence must not depend on this debug install having been signed in by hand."""
        for route in ("home", "detail", "itemmenu", "person", "library", "search", "account", "player"):
            scene = {"route": route, "tier": "player" if route == "player" else "ui"}
            self.assertTrue(run.fps_scene_needs_token(scene), route)
        self.assertFalse(run.fps_scene_needs_token({"route": "login", "tier": "ui"}))
        self.assertTrue(run.fps_scene_needs_token({"route": "login", "tier": "ui"}, True),
                        "a shared-server scene still needs its primary credential")


class LoadManifest(unittest.TestCase):
    """The whole overlay merge, against the real tracked matrix."""

    def _load(self, local):
        with _Overlay(local):
            return run.load_manifest()

    def test_empty_items_skips_everything_and_does_not_exit(self):
        m = self._load(_overlay({}))
        cases = m["cases"]
        self.assertTrue(cases, "the tracked manifest has no cases?")
        self.assertTrue(all(c.get("skip") for c in cases))
        self.assertTrue(all("rk" not in c for c in cases))

    def test_every_case_ends_with_an_rk_or_a_skip(self):
        """The invariant behind all eight `case["rk"]` subscripts downstream."""
        for local in (_overlay({}), _overlay(self._all_keys())):
            m = self._load(local)
            for e in m["cases"] + m.get("fps_scenes", []):
                if e.get("item") is None:
                    continue
                self.assertTrue(("rk" in e) != bool(e.get("skip")),
                                f"{e['name']}: rk={e.get('rk')!r} skip={e.get('skip')!r}")

    def test_partial_library_runs_the_rest(self):
        """The whole point: one resolvable shape must yield runnable cases, not a dead run."""
        m = self._load(_overlay({"movie_h264_ac3_1080p": 42}))
        runnable = [c["name"] for c in m["cases"] if not c.get("skip")]
        self.assertIn("dp_h264_ac3_1080p", runnable)
        self.assertIn("seek_inplace_h264", runnable)
        self.assertTrue(any(c.get("skip") for c in m["cases"]), "nothing skipped?")

    def test_a_missing_nested_key_skips_only_its_owner(self):
        """`expect_up_next` / `setup.also_reset` name a SECOND item. A library with the episode but
        not its successor used to lose all 21 cases to the one case that needs the pair."""
        items = self._all_keys()
        owner = next(c["name"] for c in _manifest()["cases"]
                     if any(o.get("expect_up_next") for o in c.get("operations", [])))
        nested = {o["expect_up_next"] for c in _manifest()["cases"]
                  for o in c.get("operations", []) if o.get("expect_up_next")}
        for k in nested:
            items.pop(k, None)
        m = self._load(_overlay(items))
        skipped = {c["name"] for c in m["cases"] if c.get("skip")}
        self.assertIn(owner, skipped)
        self.assertLess(len(skipped), len(m["cases"]), "a nested key took the whole matrix down")

    def test_full_overlay_skips_nothing(self):
        """The maintainer's own path must be untouched by any of this."""
        m = self._load(_overlay(self._all_keys()))
        self.assertFalse([c["name"] for c in m["cases"] if c.get("skip")])
        self.assertFalse([s["name"] for s in m.get("fps_scenes", []) if s.get("skip")])

    def test_placeholders_outside_items_are_still_fatal(self):
        """No run of any size can proceed without these, so they keep the loud death."""
        local = _overlay({})
        local["pms"]["host"] = "<pms-host>"
        with self.assertRaises(SystemExit):
            self._load(local)

    def test_bracketed_items_are_no_longer_fatal(self):
        """The exact shape of an untouched `cp` of the example."""
        keys = self._all_keys()
        self._load(_overlay({k: "<ratingKey>" for k in keys}))  # must not raise

    @staticmethod
    def _all_keys():
        m = _manifest()
        keys = set()
        for e in m["cases"] + m.get("fps_scenes", []):
            if e.get("item"):
                keys.add(e["item"])
            for k in e.get("setup", {}).get("also_reset", []):
                keys.add(k)
            for o in e.get("operations", []):
                if o.get("expect_up_next"):
                    keys.add(o["expect_up_next"])
        return {k: i + 100 for i, k in enumerate(sorted(keys))}


class AudioSwitchAssertion(unittest.TestCase):
    """`op_audio_native` graded against the literal "hevc" until 2026-08-22 — a fact about one
    library, not about the player. Anyone mapping an h264 episode to that case's shape saw a
    perfectly native switch reported as a failure."""

    NATIVE = "audio switch (native) idx=1\n"

    # The real line, as ff.rs:2010 writes it (RE_CODEC at run.py:692 reads codec= and the WxH).
    FF = "ff: v=#0 codec={0} codec_id=173 1920x1080 trc=1 pri=1 spc=1 a=#1 dur_ns=1\n"

    def _log(self, codecs):
        return [self.NATIVE] + [self.FF.format(c) for c in codecs]

    def test_unchanged_codec_passes_whatever_it_is(self):
        for codec in ("hevc", "h264", "av1"):
            ok, why = run.op_audio_native(self._log([codec, codec]))
            self.assertTrue(ok, f"{codec}: {why}")

    def test_a_changed_codec_still_fails(self):
        ok, why = run.op_audio_native(self._log(["hevc", "h264"]))
        self.assertFalse(ok)
        self.assertIn("hevc -> h264", why)

    def test_no_native_line_fails(self):
        ok, _ = run.op_audio_native([self.FF.format("hevc")])
        self.assertFalse(ok)


class PipelineTier(unittest.TestCase):
    """The Plex-free tier. Every assertion here is about a path the maintainer's own machine takes
    only when it runs --pipeline, and about the two ways this tier can silently do the wrong thing:
    drive the television with nothing to grade, or forge a shell command through the trigger."""

    def _pipeline_cases(self):
        return _manifest()["pipeline_cases"]

    def test_every_case_ends_with_a_path_or_a_skip(self):
        """The invariant behind `case["fixture"]`/`case["path"]` downstream. Run against an EMPTY
        pack, which is every machine that has not built one."""
        cases = self._pipeline_cases()
        self.assertTrue(cases, "the tracked manifest has no pipeline cases?")
        with tempfile.TemporaryDirectory() as empty:
            run._resolve_fixtures(cases, empty)
        for c in cases:
            self.assertTrue(("path" in c) != bool(c.get("skip")),
                            f"{c['name']}: path={c.get('path')!r} skip={c.get('skip')!r}")

    def test_a_present_fixture_resolves_and_a_missing_one_skips(self):
        cases = [{"name": "have", "fixture": "a.mkv"}, {"name": "havenot", "fixture": "b.mkv"},
                 {"name": "unnamed"}]
        with tempfile.TemporaryDirectory() as d:
            with open(os.path.join(d, "a.mkv"), "wb") as f:
                f.write(b"\0" * 16)   # not real media: ffprobe fails, so the length check no-ops
            run._resolve_fixtures(cases, d)
            self.assertEqual(cases[0]["path"], os.path.join(d, "a.mkv"))
            self.assertNotIn("skip", cases[0])
        self.assertNotIn("path", cases[1])
        self.assertIn("b.mkv", cases[1]["skip"])
        self.assertIn("no `fixture`", cases[2]["skip"])

    def test_the_deepest_seek_is_what_the_length_check_reads(self):
        """A pack regenerated shorter than the manifest seeks must SKIP, not fail as a player
        regression — so the depth this computes has to be the deepest thing the case asks for."""
        self.assertEqual(run._case_depth_s({"operations": [{"op": "play"}],
                                            "expect": {"min_pos_climb_s": 8}}), 8)
        self.assertEqual(run._case_depth_s(
            {"operations": [{"op": "seek", "mode": "inplace", "target_s": 40}], "expect": {}}), 40)
        self.assertEqual(run._case_depth_s(
            {"operations": [{"op": "seek", "mode": "rapid", "script": "20,+10,55", "final_s": 55}],
             "expect": {}}), 55)

    def test_the_trigger_payload_is_json_the_app_can_read_and_the_shell_cannot_break(self):
        """Pin both the app-readable JSON and today's compact, pasteable payload vocabulary.

        `apply_triggers` now quotes arbitrary apostrophes too, so this is no longer the command's
        security boundary; the no-apostrophe property remains useful for copied case headers and
        has a twin in rust-modules/src/dev.rs.
        """
        for c in self._pipeline_cases():
            files = run.triggers_for_case(c, url_base="http://192.0.2.10:8020")
            self.assertEqual(files[0][0], "plxnative-playurl")
            payload = files[0][1]
            self.assertNotIn("'", payload, f"{c['name']}: would break the single-quoted printf")
            spec = json.loads(payload)
            self.assertEqual(spec["url"], f"http://192.0.2.10:8020/{c['fixture']}")
            for k, v in c.get("declare", {}).items():
                self.assertEqual(spec[k], v)

    def test_the_integration_tier_pins_original_quality(self):
        """The PMS matrix must not inherit an Auto preference from an earlier TV run."""
        files = run.triggers_for_case({"rk": "1234", "operations": [{"op": "play"}]})
        self.assertEqual(files, [
            ("plxnative-play", "1234"),
            ("plxnative-quality", "original"),
            ("plxnative-stats", None),
        ])

    def test_an_integration_case_can_explicitly_grade_auto(self):
        files = run.triggers_for_case({
            "rk": "1234", "quality": "auto", "operations": [{"op": "play"}],
        })
        self.assertEqual(dict(files)["plxnative-quality"], "auto")

    def test_a_declaration_that_was_never_read_fails_rather_than_passing(self):
        """THE false-PASS this tier is most exposed to: the engine's fallthrough arm produces
        `a="AC3"` for an unrecognised or EMPTY audio codec, so a trigger that never got read at all
        yields exactly the right payload for the AC-3 baseline. That is why cases exist whose
        expected load_audio is "AC3 PLUS" and "AAC" — they cannot be reached by accident."""
        unread = ['load: v=H264 a="AC3" fps=0.000 dv=present:0 P0/0 el:0 atmos:0']
        ok, _ = run.a_load_decl(unread, {"load_video": "H265", "load_audio": "AC3 PLUS"})
        self.assertFalse(ok, "an unread declaration must not satisfy an HEVC/E-AC-3 case")
        expected = ['load: v=H265 a="AC3 PLUS" fps=24.000 dv=present:1 P8/1 el:0 atmos:0']
        ok, why = run.a_load_decl(expected, {"load_video": "H265", "load_audio": "AC3 PLUS",
                                             "load_dovi": "P8/1", "load_atmos": False})
        self.assertTrue(ok, why)
        # And the manifest must actually carry such cases, or the defence above is theoretical.
        audios = {c["expect"].get("load_audio") for c in self._pipeline_cases()}
        self.assertTrue({"AC3 PLUS", "AAC"} <= audios,
                        f"no case declares an audio codec the fallthrough cannot produce: {audios}")

    def test_a_missing_load_line_is_a_failure_not_a_silent_pass(self):
        ok, why = run.a_load_decl(["ff: v=#0 codec=h264 codec_id=27 1920x1080 a=#1"],
                                  {"load_video": "H264"})
        self.assertFalse(ok)
        self.assertIn("no `load:` line", why)

    def test_the_wire_assertion_sees_a_seek_that_never_reached_the_demuxer(self):
        """The pump logs its seek intent whether or not the AVIO was ever reached, so a 206 counted
        on the wire is the only proof the Range reopen actually happened."""
        self.assertFalse(run.a_server_wire((3, 0), 2, 1)[0])
        self.assertTrue(run.a_server_wire((3, 1), 2, 1)[0])
        self.assertFalse(run.a_server_wire((0, 0), 1, 0)[0])

    def test_the_stream_path_assertion_catches_playing_the_wrong_thing(self):
        """A stale plxnative-play from a by-hand session plays a LIBRARY ITEM through a pipeline
        case. Everything else would still pass; the opened path is what tells them apart."""
        good = ["stream: host=192.0.2.10 port=8020 path=/pipe_h264_ac3_1080p.mkv"]
        self.assertTrue(run.a_stream_path(good, "pipe_h264_ac3_1080p.mkv")[0])
        bad = ["stream: host=192.0.2.10 port=32400 path=/library/parts/12/file.mkv?X-Plex-Token=x"]
        self.assertFalse(run.a_stream_path(bad, "pipe_h264_ac3_1080p.mkv")[0])
        self.assertFalse(run.a_stream_path([], "pipe_h264_ac3_1080p.mkv")[0])

    def test_the_audio_lane_assertion_reads_the_fed_index(self):
        ln = ["ff: v=#0 codec=h264 codec_id=27 1920x1080 trc=1 pri=1 spc=1 a=#2 dur_ns=60000000000"]
        self.assertTrue(run.a_audio_lane(ln, 2)[0])
        self.assertFalse(run.a_audio_lane(ln, 3)[0])
        self.assertFalse(run.a_audio_lane([], 2)[0])

    def test_pos_climb_has_no_timeline_fallback(self):
        """`a_timeline_climb` falls back to the 10 s /:/timeline series. That reporter is never
        spawned here (no ratingKey), so accepting its absence would make a broken assertion read as
        a pass. Pin that the pipeline one refuses a log carrying only timeline lines."""
        timeline_only = ["timeline playing t=10s/60s", "timeline playing t=20s/60s"]
        self.assertTrue(run.a_timeline_climb(timeline_only, 8)[0], "the server one still folds back")
        self.assertFalse(run.a_timeline_climb(timeline_only, 8, dense_only=True)[0],
                         "the synthetic one must not")
        heartbeat = [f"loop=60 route=player overlay=none pos={t}s vtick=5" for t in (2, 14)]
        self.assertTrue(run.a_timeline_climb(heartbeat, 8, dense_only=True)[0])

    def test_the_pipeline_tier_needs_no_overlay_at_all(self):
        """Requirement 1, as a test: a stranger with no manifest.local.json must still load."""
        saved = run.MANIFEST_LOCAL
        run.MANIFEST_LOCAL = os.path.join(TESTS_DIR, "no-such-overlay.json")
        try:
            m = run.load_manifest(pipeline_only=True, tv_override="10.0.0.9")
            self.assertEqual(m["tv"], "10.0.0.9")
            self.assertTrue(m["pipeline_cases"])
            # ...but a TV address is still required, and is the ONLY thing this path can die for.
            # `.tv-host` is the maintainer's own fallback and exists on this machine, so point the
            # lookup at a path that cannot.
            saved_host = run.TV_HOST_FILE
            run.TV_HOST_FILE = os.path.join(TESTS_DIR, "no-such-tv-host")
            try:
                with self.assertRaises(SystemExit):
                    run.load_manifest(pipeline_only=True, tv_override=None)
            finally:
                run.TV_HOST_FILE = saved_host
        finally:
            run.MANIFEST_LOCAL = saved


class ResolutionSpike(unittest.TestCase):
    """The offline half of a single-Load 720p -> 1080p -> 720p device experiment."""

    @staticmethod
    def _case():
        return next(c for c in _manifest()["pipeline_cases"]
                    if c["name"] == "pipe_h264_aac_resolution_spike")

    @staticmethod
    def _gst_line(ms, payload):
        whole = int(ms // 1000)
        nanos = int(round((ms - whole * 1000) * 1_000_000))
        return f"0:00:{whole:02d}.{nanos:09d} 123 GST_DEBUG {payload}"

    def _good_trace(self):
        return [self._gst_line(
                    500 + i * (1000 / 24),
                    "gst_lx_videosink_render:<lxvideosink0> [PLAYING] received buffer")
                for i in range(410)]

    @staticmethod
    def _source_info(width, height):
        return ('smp_cb type=4 num=0 str={"context":"test","video":'
                f'{{"width":{width},"height":{height}}}}}')

    def test_manifest_requires_one_load_one_wire_open_and_no_reload(self):
        c = self._case()
        self.assertEqual(c["run_secs"], 35)
        self.assertEqual(c["expect"]["starfish_resolution_sequence"],
                         ["1280x720", "1920x1080", "1280x720"])
        self.assertEqual(c["expect"]["resolution_boundaries_s"], [8, 16])
        self.assertEqual(c["expect"]["load_count_exact"], 1)
        self.assertTrue(c["expect"]["require_audio_feed_ready"])
        self.assertTrue(c["expect"]["no_reload"])
        self.assertEqual(c["expect"]["server_opens_exact"], 1)
        self.assertEqual(c["expect"]["server_range_opens_exact"], 0)
        files = dict(run.triggers_for_case(c, url_base="http://192.0.2.10:8020"))
        self.assertEqual(files["plxnative-gstlog"], c["gst_trace"]["debug"])
        self.assertEqual(files["plxnative-gstlog"], "lxvideosink:6")

    def test_apply_triggers_removes_a_stale_trace_before_arming(self):
        saved = run.RUNDIR
        run.RUNDIR = "/tmp/com.beb.plxnative.debug"
        try:
            with mock.patch.object(run, "ssh") as ssh:
                run.apply_triggers("192.0.2.20", [("plxnative-gstlog", "GST_EVENT:6")])
            command = ssh.call_args.args[1]
            self.assertIn("rm -f /tmp/com.beb.plxnative.debug/plxnative-gst.log", command)
            self.assertIn("plxnative-gstlog", command)
        finally:
            run.RUNDIR = saved

    def test_exact_session_and_wire_counts_reject_hidden_reopens(self):
        load = 'load: v=H264 a="AAC" fps=24.000 dv=present:0 P0/0 el:0 atmos:0'
        self.assertTrue(run.a_load_count([load], 1)[0])
        self.assertFalse(run.a_load_count([load, load], 1)[0])
        self.assertTrue(run.a_no_reload([load])[0])
        self.assertFalse(run.a_no_reload([load, "reload_at: 8000ms"])[0])
        self.assertTrue(run.a_server_wire((1, 0), 1, 0, 1, 0)[0])
        self.assertFalse(run.a_server_wire((2, 1), 1, 0, 1, 0)[0])

    def test_audio_readiness_requires_an_accepted_starfish_feed(self):
        accepted = "feed a#4 sz=512 fed=42666667 reply=O qbytes=1024"
        rejected = "feed a#1 sz=512 fed=0 reply=B qbytes=2048"
        self.assertTrue(run.a_audio_feed_ready([accepted])[0])
        self.assertFalse(run.a_audio_feed_ready([rejected])[0])
        self.assertFalse(run.a_audio_feed_ready([])[0])

    def test_starfish_source_info_requires_the_exact_resolution_sequence(self):
        c = self._case()
        lines = [self._source_info(1280, 720),
                 ('smp_cb type=4 num=0 str={"sourceInfo":{"context":"test","video":'
                  '{"width":1920,"height":1080}}}'),
                 self._source_info(1280, 720)]
        want = c["expect"]["starfish_resolution_sequence"]
        self.assertTrue(run.a_starfish_resolution_sequence(lines, want)[0])
        self.assertFalse(run.a_starfish_resolution_sequence(lines[:-1], want)[0])

    def test_physical_packet_verifier_rejects_a_track_stored_in_one_lump(self):
        good = [
            {"stream_index": 0, "pts_time": "0.000", "pos": "100"},
            {"stream_index": 1, "pts_time": "0.000", "pos": "200"},
            {"stream_index": 1, "pts_time": "0.021", "pos": "300"},
            {"stream_index": 0, "pts_time": "0.042", "pos": "400"},
            {"stream_index": 1, "pts_time": "0.043", "pos": "500"},
            {"stream_index": 0, "pts_time": "0.083", "pos": "600"},
        ]
        skew, counts = fixturegen.physical_av_interleave(list(reversed(good)), 0, 1)
        self.assertLess(skew, 0.050)  # input order is irrelevant; byte `pos` is the authority
        self.assertEqual(counts, {0: 3, 1: 3})

        lumped = [
            {"stream_index": 0, "pts_time": f"{i / 10:.1f}", "pos": str(i * 100)}
            for i in range(11)
        ] + [
            {"stream_index": 1, "pts_time": f"{i / 10:.1f}", "pos": str(2000 + i * 100)}
            for i in range(11)
        ]
        skew, _ = fixturegen.physical_av_interleave(lumped, 0, 1)
        self.assertGreaterEqual(skew, 1.0)
        with self.assertRaises(ValueError):
            fixturegen.physical_av_interleave(lumped[:4], 0, 1)

    def test_gst_trace_accepts_metronomic_boundaries(self):
        c = self._case()
        ok, why = run.a_gst_resolution_trace(self._good_trace(), c["expect"], c["gst_trace"])
        self.assertTrue(ok, why)

    def test_gst_trace_rejects_a_boundary_stall(self):
        c = self._case()
        stalled = [ln for ln in self._good_trace()
                   if "received buffer" not in ln or not (8500 <= run.gst_clock_ms(ln) <= 8800)]
        ok, why = run.a_gst_resolution_trace(stalled, c["expect"], c["gst_trace"])
        self.assertFalse(ok)
        self.assertIn("first picture", why)


class DefaultTier(unittest.TestCase):
    """Which tier a bare `./tests/run.py` runs. Inverted 2026-08-22: the synthetic pipeline tier
    is the default and `--server` opts into the library-backed one. Pinned here because the
    inversion is invisible from any single code path — it is one boolean in `main()` — and getting
    it backwards means a bare command either demands credentials nobody has, or silently grades a
    tier the operator did not ask for."""

    @staticmethod
    def _list(*flags):
        """`./tests/run.py <flags> --list` as a completed process. `--list` is offline and
        side-effect free, which is what makes spawning the real CLI the honest way to ask which
        tier a set of flags selects — the rule lives in `main()` and cannot be imported."""
        return subprocess.run([sys.executable, os.path.join(TESTS_DIR, "run.py"), *flags, "--list"],
                              capture_output=True, text=True, timeout=120)

    def test_the_bare_command_is_the_synthetic_tier(self):
        """Documented in three places (tests/README.md, CLAUDE.md, --help); pinned in one."""
        out = self._list()
        self.assertIn("pipe_", out.stdout, "a bare --list must show the synthetic cases")
        self.assertNotIn("dp_h264_ac3_1080p", out.stdout,
                         "a bare --list must NOT show the library-backed cases")

    def test_server_opts_into_the_library_tier(self):
        self.assertIn("dp_h264_ac3_1080p", self._list("--server").stdout)

    def test_contradictory_tier_flags_refuse(self):
        """`--pipeline` names the default, so pairing it with --server/--fps is two instructions,
        not a preference — honouring either one silently is how somebody trusts the wrong result."""
        for extra in ("--server", "--fps", "--fps-player"):
            self.assertNotEqual(self._list("--pipeline", extra).returncode, 0,
                                f"--pipeline {extra} should refuse")

    def test_the_manifest_declares_a_frame_rate_axis(self):
        """Every fixture ran at 24p until 2026-08-22, so `engine::fps_rational`'s branches had one
        input between them. Pin that the matrix now carries both sides of its split: a
        1001-denominator broadcast rate and an integer one."""
        rates = {c["declare"].get("fps") for c in _manifest()["pipeline_cases"]}
        self.assertTrue(any(abs(r - 59.94) < 0.01 for r in rates if r),
                        f"no 1001-denominator rate in the matrix: {rates}")
        self.assertTrue(any(r and r > 24 and float(r).is_integer() for r in rates),
                        f"no integer high frame rate in the matrix: {rates}")

    def test_the_direct_play_payload_matrix_is_fully_covered(self):
        """The player direct-plays exactly {H264,H265} x {AC3,AC3 PLUS,AAC} — route.rs's codec gate
        and plex::DP_AUDIO_CODECS. Six payload combinations, and this suite is the only tier that
        can reach all six without owning media of every shape."""
        combos = {(c["expect"].get("load_video"), c["expect"].get("load_audio"))
                  for c in _manifest()["pipeline_cases"]}
        want = {(v, a) for v in ("H264", "H265") for a in ("AC3", "AC3 PLUS", "AAC")}
        self.assertEqual(want - combos, set(), f"uncovered Load payload combinations: {want - combos}")


class ResolutionMatrix(unittest.TestCase):
    """LG App Self Checklist #50 / #51, which is graded as a MATRIX and was answered as pieces.

    Two halves, and the second is the one that rots: the assertion has to be EXACT, and the matrix
    has to stay complete as cases are added and renamed. A `min_video_width` cannot tell 720x480
    from 720x576, so a matrix built on it would read as covered while grading nothing.
    """

    CELLS = {("h264", "720x480"), ("h264", "1280x720"), ("h264", "1920x1080"),
             ("h264", "3840x2160"), ("hevc", "720x480"), ("hevc", "1280x720"),
             ("hevc", "1920x1080"), ("hevc", "3840x2160")}

    FF = ("ff: v=#0 codec={0} codec_id=27 {1} trc=1 pri=1 spc=1 a=#1 dur_ns=60000000000\n")

    @staticmethod
    def _cases():
        return _manifest()["pipeline_cases"]

    def test_all_eight_cells_exist(self):
        got = {(c["expect"]["codec"], c["expect"]["video_size"]) for c in self._cases()
               if "resolution-matrix" in c.get("covers", [])}
        self.assertEqual(self.CELLS - got, set(), f"uncovered resolution x codec cells: "
                                                  f"{self.CELLS - got}")

    def test_every_matrix_cell_grades_the_size_exactly(self):
        """...and none of them falls back to a width bound, which is the shape being replaced."""
        for c in self._cases():
            if "resolution-matrix" not in c.get("covers", []):
                continue
            self.assertIn("video_size", c["expect"], c["name"])
            self.assertNotIn("min_video_width", c["expect"],
                             f"{c['name']}: video_size subsumes min_video_width — keeping both "
                             f"leaves two statements of one number that nothing keeps in step")

    def test_a_codec_rejects_the_wrong_raster(self):
        """The whole value of the exact form: a 4:3 SD clip must not satisfy a 16:9 SD cell."""
        ok, why = run.a_codec([self.FF.format("h264", "720x480")], "h264", 0, "720x480")
        self.assertTrue(ok, why)
        ok, why = run.a_codec([self.FF.format("h264", "720x576")], "h264", 0, "720x480")
        self.assertFalse(ok, "720x576 must not pass a 720x480 cell")
        self.assertIn("720x576", why)
        # ...and a width bound would have passed it, which is the point.
        self.assertTrue(run.a_codec([self.FF.format("h264", "720x576")], "h264", 700)[0])

    def test_a_codec_still_grades_the_codec_and_the_width_alone(self):
        """The non-matrix cases pass no `size`, and their behaviour must be bit-identical."""
        self.assertTrue(run.a_codec([self.FF.format("hevc", "3840x2160")], "hevc", 3800)[0])
        self.assertFalse(run.a_codec([self.FF.format("h264", "3840x2160")], "hevc", 3800)[0])
        self.assertFalse(run.a_codec([self.FF.format("hevc", "1920x1080")], "hevc", 3800)[0])
        self.assertFalse(run.a_codec([], "hevc", 0, "1920x1080")[0])

    def test_each_matrix_fixture_is_named_by_exactly_one_cell(self):
        """A cell reusing another's file would be a matrix with a hole in it that reads as full —
        the exact failure `pipe_hevc_aac_mp4`-as-the-FHD-HEVC-rung would have been."""
        fixtures = [c["fixture"] for c in self._cases()
                    if "resolution-matrix" in c.get("covers", [])]
        self.assertEqual(len(fixtures), len(set(fixtures)), f"a fixture serves two cells: "
                                                            f"{sorted(fixtures)}")


class CompletionCase(unittest.TestCase):
    """LG #46's first half — a stream that runs OUT and an app that leaves the player.

    Every other case in both tiers is built so the clip CANNOT end inside its window, so this is
    the one place the finish path is exercised at all, and the assertion behind it has to refuse
    three near-misses that each look like a pass.
    """

    EOS = "EOS reached: playpos=19s/20s -> ended\n"
    TORN = "stop_bufferfeed: torn down\n"
    POS = "loop=60 route=player overlay=none pos=19s vtick=5\n"

    def test_the_finish_needs_both_lines_in_order(self):
        self.assertTrue(run.a_finished([self.POS, self.EOS, self.TORN])[0])

    def test_a_teardown_without_an_eos_is_not_a_finish(self):
        """Every stop tears the engine down, the harness's own close included — so an unordered
        match would pass on a clip that never ended."""
        ok, why = run.a_finished([self.POS, self.TORN])
        self.assertFalse(ok)
        self.assertIn("EOS reached", why)
        ok, why = run.a_finished([self.TORN, self.EOS])
        self.assertFalse(ok, "a teardown BEFORE the EOS is the previous session's, not this one's")

    def test_an_earlier_reload_teardown_does_not_poison_the_finish(self):
        """`teardown` writes the same line for a `for_reload` stop — a seek that escalated to
        `reload_at`, or an app-switch suspend. Comparing the FIRST teardown's index against the
        EOS fails such a run for its whole cap, reading as "the player froze on the last frame",
        which is precisely the false regression this assertion exists to avoid."""
        ok, why = run.a_finished([self.TORN, self.POS, self.EOS, self.TORN])
        self.assertTrue(ok, why)

    def test_an_eos_that_never_tore_down_is_a_frozen_last_frame(self):
        ok, why = run.a_finished([self.POS, self.EOS])
        self.assertFalse(ok)
        self.assertIn("froze", why)

    def test_only_a_case_that_asks_to_reach_the_end_may_name_the_short_clip(self):
        """The rule is not "exactly one case", which is what it said while there was one — it is
        that a case graded through a TEARDOWN has to have asked for one. The short clip ends inside
        every window, so any case naming it without `reaches_eos` would have its assertions cut off
        by a finish it never declared."""
        cases = _manifest()["pipeline_cases"]
        eos = [c for c in cases if c["expect"].get("reaches_eos")]
        self.assertTrue(eos, "no case reaches EOS — LG #46 has nothing behind it")
        short = {c["fixture"] for c in eos}
        self.assertEqual(len(short), 1, f"the completion cases disagree on which clip ends: {short}")
        strays = [c["name"] for c in cases
                  if c["fixture"] in short and not c["expect"].get("reaches_eos")]
        self.assertFalse(strays, f"the short clip ENDS mid-window; {strays} would be graded "
                                 f"through a teardown they never asked for")

    def test_the_replay_case_grades_both_halves_of_46(self):
        """#46 is "replay AFTER completion", so the case that grades the replay must also be the
        one that reaches the end — a replay asserted without a finish behind it would pass on a
        stream that was merely restarted mid-play."""
        cases = _manifest()["pipeline_cases"]
        replays = [c for c in cases if c["expect"].get("replays")]
        self.assertEqual(len(replays), 1, f"expected one replay case, got {[c['name'] for c in replays]}")
        c = replays[0]
        self.assertTrue(c["expect"].get("reaches_eos"), f"{c['name']} replays without finishing")
        # The second viewing must fetch the clip again — `teardown` closed the socket and cleared
        # the URL — so a floor of 2 opens is the wire-side half of the same assertion.
        self.assertGreaterEqual(c["expect"].get("server_opens_min", 0), 2, c["name"])

    def test_the_replay_trigger_carries_the_number_the_case_grades(self):
        """One statement, not two: `expect.replays` is what the harness writes into
        `plxnative-replay` AND what `a_replayed` counts, so they cannot drift."""
        c = next(c for c in _manifest()["pipeline_cases"] if c["expect"].get("replays"))
        files = dict(run.triggers_for_case(c, url_base="http://192.0.2.10:8020"))
        self.assertEqual(files.get("plxnative-replay"), str(c["expect"]["replays"]))
        # ...and every other case must NOT arm it, or a one-shot boot silently becomes a loop.
        for other in _manifest()["pipeline_cases"]:
            if other["expect"].get("replays"):
                continue
            self.assertNotIn("plxnative-replay",
                             dict(run.triggers_for_case(other, url_base="http://192.0.2.10:8020")),
                             other["name"])

    def test_a_replay_is_seen_as_a_fall_then_a_climb(self):
        """The three signals, and the three near-misses that each look like a pass."""
        rep = "replay: starting the finished stream again (0 left)\n"
        load = 'load: v=H264 a="AC3" fps=24.000 dv=present:0 P0/0 el:0 atmos:0\n'
        pos = [f"loop=60 route=player overlay=none pos={t}s vtick=5\n" for t in (2, 10, 19)]
        pos2 = [f"loop=60 route=player overlay=none pos={t}s vtick=5\n" for t in (1, 9, 18)]
        good = [load] + pos + [rep, load] + pos2
        ok, why = run.a_replayed(good, 1)
        self.assertTrue(ok, why)

        # (a) the app never re-entered — no `replay:` line at all.
        ok, why = run.a_replayed([load] + pos, 1)
        self.assertFalse(ok)
        self.assertIn("never re-entered", why)
        # (b) it fired more often than asked: a loop, which every other signal would accept.
        ok, why = run.a_replayed([load] + pos + [rep, load] + pos2 + [rep, load] + pos2, 1)
        self.assertFalse(ok)
        self.assertIn("loop", why)
        # (c) it fired and the payload was never rebuilt — one `load:` for one replay.
        ok, why = run.a_replayed([load] + pos + [rep] + pos2, 1)
        self.assertFalse(ok)
        self.assertIn("rebuilt its payload", why)
        # (d) it fired, reloaded, and playback CARRIED ON rather than restarting.
        carried = [f"loop=60 route=player overlay=none pos={t}s vtick=5\n" for t in (20, 28, 37)]
        ok, why = run.a_replayed([load] + pos + [rep, load] + carried, 1)
        self.assertFalse(ok)
        self.assertIn("never fell", why)
        # (e) it restarted and then stalled at the join — a fall with no second viewing.
        stalled = ["loop=60 route=player overlay=none pos=0s vtick=5\n"] * 3
        ok, why = run.a_replayed([load] + pos + [rep, load] + stalled, 1)
        self.assertFalse(ok)
        self.assertIn("did not play", why)

    def test_the_climb_is_measured_from_the_drop_and_not_from_the_global_floor(self):
        """THE false PASS this assertion shipped with, and the ordering the field will produce.

        The first version anchored the post-drop climb at the global floor, which is a VALUE — so
        it only landed in viewing 2 when viewing 2 happened to reach viewing 1's minimum. The
        `pos=` heartbeat is 1 Hz and free-running, so viewing 1 logging `pos=0s` while viewing 2's
        first sample lands at `pos=1s` put the anchor back in viewing 1 and measured VIEWING 1'S
        OWN CLIMB: `[0,5,10,19,1]` read as "fell 18s then climbed 19s" and PASSED with the second
        viewing having produced one sample and zero seconds of playback.

        It is also the state the harness normally grades, because it exits the moment every
        assertion passes — so this was not a corner, it was the common case. The class's other
        near-miss test passes only because its series starts at 2 rather than 0, which is exactly
        the kind of accident a regression test is for.
        """
        rep = "replay: starting the finished stream again (0 left)\n"
        load = 'load: v=H264 a="AC3" fps=24.000 dv=present:0 P0/0 el:0 atmos:0\n'

        def pos(t):
            return f"loop=60 route=player overlay=none pos={t}s vtick=5\n"

        # Viewing 1 reaches 0; viewing 2's only sample is 1, so the global floor sits in viewing 1.
        stalled = [load, pos(0), pos(5), pos(10), pos(19), rep, load, pos(1)]
        ok, why = run.a_replayed(stalled, 1)
        self.assertFalse(ok, "a replay with one sample and no playback must not pass")
        self.assertIn("did not play", why)
        # ...and the same shape with a real second viewing still passes.
        played = [load, pos(0), pos(5), pos(10), pos(19), rep, load, pos(1), pos(9), pos(18)]
        ok, why = run.a_replayed(played, 1)
        self.assertTrue(ok, why)

    def test_a_pack_too_short_to_be_played_twice_skips_the_replay_case(self):
        """The EOS bound is per VIEWING: a clip that fits once inside the cap need not fit twice."""
        case = {"name": "rep", "fixture": "c.mkv", "run_secs": 60,
                "expect": {"reaches_eos": True, "replays": 1, "min_pos_climb_s": 8}}
        with tempfile.TemporaryDirectory() as d:
            with open(os.path.join(d, "c.mkv"), "wb") as f:
                f.write(b"\0" * 16)
            saved = run._probe_fixture
            try:
                # 25 s fits once inside 60 * 0.6 = 36, and twice does not.
                run._probe_fixture = lambda p: (25.0, [("video", "h264"), ("audio", "ac3")])
                run._resolve_fixtures([case], d)
                self.assertIn("skip", case)
                self.assertIn("2x", case["skip"])
                once = dict(case)
                once.pop("skip", None)
                once["expect"] = dict(case["expect"], replays=0)
                run._resolve_fixtures([once], d)
                self.assertNotIn("skip", once)
            finally:
                run._probe_fixture = saved

    def test_a_pack_too_long_to_finish_skips_rather_than_fails(self):
        """A `--secs`/`--quick` regeneration is the realistic way to break this, and `a_finished`
        failing on a 300 s clip reads as the app freezing on the last frame."""
        case = {"name": "eos", "fixture": "c.mkv", "run_secs": 60,
                "expect": {"reaches_eos": True, "min_pos_climb_s": 8}}
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "c.mkv")
            with open(path, "wb") as f:
                f.write(b"\0" * 16)
            saved = run._probe_fixture
            try:
                run._probe_fixture = lambda p: (300.0, [("video", "h264"), ("audio", "ac3")])
                run._resolve_fixtures([case], d)
                self.assertIn("skip", case)
                self.assertIn("to the END", case["skip"])
                # ...and the same clip inside the budget resolves.
                short = dict(case)
                short.pop("skip", None)
                run._probe_fixture = lambda p: (20.0, [("video", "h264"), ("audio", "ac3")])
                run._resolve_fixtures([short], d)
                self.assertNotIn("skip", short)
                self.assertEqual(short["path"], path)
            finally:
                run._probe_fixture = saved


class AutoNetworkProfile(unittest.TestCase):
    """The no-Plex TV case drives one live fast→slow→fast response schedule."""

    def _case(self):
        return next(c for c in _manifest()["pipeline_cases"]
                    if c["name"] == "pipe_auto_original_slow_recover")

    def test_trigger_carries_auto_policy_source_rate_and_same_origin_hls_root(self):
        files = dict(run.triggers_for_case(self._case(), url_base="http://192.0.2.10:8020"))
        self.assertEqual(files["plxnative-quality"], "auto")
        spec = json.loads(files["plxnative-playurl"])
        self.assertEqual(spec["auto_source_kbps"], 8000)
        self.assertEqual(spec["auto_hls_base"], "http://192.0.2.10:8020/__abr")
        self.assertEqual(spec["url"], "http://192.0.2.10:8020/pipe_h264_aac_mp4.mp4")

    def test_assertion_requires_a_measured_collapse_then_a_committed_original(self):
        down = ("auto: Original -> HLS ImminentStarvation measured=3998kbps safe=3198kbps "
                "need=10800kbps buf=2900ms slope=-1200ms/s starve=4 windows=1 target=2000kbps")
        up = "abr: committed Up to 20000kbps 1920x1080"
        request = "abr: source sustainable again at 60321kbps; requesting Original"
        committed = "auto: recovered Original direct play; retiring HLS encoder"
        ok, why = run.a_auto_network_recovery([down, up, request, committed], 5000, 20000)
        self.assertTrue(ok, why)
        self.assertIn("ImminentStarvation", why, "the reason code is the field worth reporting")
        # The rung the ladder happens to be on when the probe fires is NOT graded any more: PMS
        # producing 20 Mbit/s of H.264 says the server can encode, not that the link can carry the
        # remux, and on this fixture the probe gate (three healthy segments) usually beats the
        # ladder (five). What must still hold is the ORDER: collapse, then probe, then route.
        self.assertTrue(run.a_auto_network_recovery([down, request, committed], 5000, 20000)[0],
                        "recovery from a middle rung is the design, not a failure")
        self.assertFalse(run.a_auto_network_recovery([request, committed, down], 5000, 20000)[0],
                         "a probe that predates the collapse is not recovery from it")
        self.assertFalse(run.a_auto_network_recovery([down, up], 5000, 20000)[0],
                         "the top transcode rung is not Original recovery")
        self.assertFalse(run.a_auto_network_recovery([down, up, request], 5000, 20000)[0],
                         "a requested transition is not a committed route")
        self.assertFalse(run.a_auto_network_recovery(
            [down,
             up,
             "abr: source sustainable again at 12000kbps; requesting Original",
             committed], 5000, 20000)[0],
            "the probe that justified the return must clear the bar the case sets")
        self.assertFalse(run.a_auto_network_recovery([
            down.replace("measured=3998kbps", "measured=7000kbps"),
            up,
            request,
            committed,
        ], 5000, 20000)[0], "the shaped 4 Mbit/s leg must be measured, not merely assumed")

    def test_the_rung_shape_assertion_can_fail_on_overreach_not_only_on_stalling(self):
        """`a_abr_shape` exists for the failure every other assertion here is blind to.

        A controller that spends a 6 Mbit/s link reaching for 20 Mbit/s still plays: it rebuffers,
        recovers, climbs, and satisfies the position climb, the codec, the Load declaration and
        `no_playing_error`. Only a CEILING can see it -- so the first thing to grade is that the
        ceiling actually rejects something.
        """
        steady = lambda kbps: f"abr: steady current={kbps}kbps safe=9000kbps pending=0kbps"
        modest = [steady(720), steady(2000), steady(4000), steady(4000)]
        ok, why = run.a_abr_shape(modest, {"ceiling_kbps": 8000, "floor_kbps": 2000})
        self.assertTrue(ok, why)
        self.assertIn("settled 4000kbps", why)

        greedy = modest + [steady(20000)]
        self.assertFalse(run.a_abr_shape(greedy, {"ceiling_kbps": 8000})[0],
                         "20 Mbit/s on a link graded for 8 is exactly what the ceiling is for")

        # …and the opposite failure, which a ceiling alone would pass with flying colours.
        parked = [steady(720)] * 6
        self.assertFalse(run.a_abr_shape(parked, {"floor_kbps": 2000})[0],
                         "a controller parked on the bootstrap rung never overreaches either")

        # The flap guard reads COMMITS, not the steady lines: a link that really collapses should
        # move, and the bound is on how often, not on whether.
        flapping = [steady(20000)]
        for _ in range(5):
            flapping += ["abr: committed Down to 3000kbps 1280x536",
                         "abr: committed Up to 20000kbps 1920x1080"]
        self.assertFalse(run.a_abr_shape(flapping, {"max_commits": 8})[0],
                         "ten visible quality changes is the product failure this bound names")
        self.assertTrue(run.a_abr_shape(
            [steady(20000), "abr: committed Down to 3000kbps 1280x536"], {"max_commits": 8})[0],
            "moving once on a real collapse is correct and must not trip the flap guard")

        # A run with no controller at all must FAIL rather than vacuously pass: an empty trail
        # satisfies every bound above by containing nothing.
        self.assertFalse(run.a_abr_shape(["nothing to do with abr"], {"ceiling_kbps": 8000})[0],
                         "no `abr: steady` line means the controller never ran")

        # Settle bounds are read from the LAST steady line, not from the extremes.
        recovering = [steady(20000), steady(320), steady(320), steady(16000)]
        self.assertTrue(run.a_abr_shape(recovering, {"settle_min_kbps": 8000})[0])
        self.assertFalse(run.a_abr_shape(recovering[:-1], {"settle_min_kbps": 8000})[0],
                         "a run that ended on the floor did not recover, whatever it reached before")

    def test_every_shaped_abr_case_grades_something_a_position_climb_cannot(self):
        """Each shaped profile must carry at least one `abr_shape` bound.

        Without one the case is an expensive way to assert that playback works -- which the rest of
        this tier already does, on a healthy link, in less time.

        SCOPED to the shaped family on 2026-08-26, when a second family of `pipe_abr_*` cases
        appeared: the `pipe_abr_pin_*` census (measurement step M4) is unshaped BY DESIGN, because
        its subject is the AU queue byte cap and a shaped leg would measure the shaper instead, and
        it grades nothing BY DESIGN, because it exists to produce the baseline against which a
        bound could later be written. The old expectation was not wrong, it was under-scoped —
        every case it was written about still has to satisfy it, and the census family has its own
        rule in `test_every_census_case_is_unshaped_pinned_and_grades_nothing` below.

        SCOPED AGAIN on 2026-08-27 for a THIRD family, `pipe_abr_band_*`, which is neither: it
        shapes the link *because* the link is the independent variable, and it grades nothing for
        the census's reason. The three families are separated by what varies and what is asserted —
        census holds the link still and asserts nothing, band sweeps the link and asserts nothing,
        shaped disturbs the link and must assert the recovery. Its own rule is
        `test_every_band_case_sweeps_a_derived_ladder_of_legs`.
        """
        shaped = [c for c in _manifest()["pipeline_cases"]
                  if c["name"].startswith("pipe_abr_")
                  and not c["name"].startswith("pipe_abr_pin_")
                  and not c["name"].startswith("pipe_abr_band_")]
        self.assertGreaterEqual(len(shaped), 4, "the bad-network profiles are missing")
        for case in shaped:
            with self.subTest(case["name"]):
                self.assertIn("network_profile", case, "a shaped case must shape the link")
                bounds = case["expect"].get("abr_shape") or {}
                self.assertTrue(bounds, "no abr_shape bound — this case grades nothing new")
                unknown = set(bounds) - abr_shape_keys()
                self.assertFalse(
                    unknown,
                    f"abr_shape key in {case['name']} that a_abr_shape does not implement: "
                    f"{sorted(unknown)}. A bound nothing reads is silently never graded.")

    def test_every_census_case_is_unshaped_pinned_and_grades_nothing(self):
        """The complementary rule for the M4 census family (plan I0-J / I1-C).

        Three properties, each of which the census is worthless without:
        UNSHAPED, so the AU queue byte cap is the only limiter; PINNED to an exact actuator by
        request rate, so the rung being measured is the rung named; and carrying an EMPTY
        `abr_shape`, so every I0 metric is reported and none is asserted. A bound here would be a
        number guessed before the measurement that justifies it.
        """
        census = [c for c in _manifest()["pipeline_cases"]
                  if c["name"].startswith("pipe_abr_pin_")]
        self.assertGreaterEqual(len(census), 5, "the M4 census points are missing")
        ladder = {320, 720, 2000, 4000, 6000, 8000, 10000, 12000, 14000, 16000, 18000, 20000, 22000}
        for case in census:
            with self.subTest(case["name"]):
                # Unshaped is the DEFAULT and the intent: the AU queue byte cap is the subject,
                # and a shaped leg risks measuring the shaper. Two points depart from it, and the
                # departure is now HISTORICAL: the pin needed six segments of reserve in BOTH
                # directions until 2026-08-27 and so could not transact down from the ladder top,
                # which is where an unshaped link puts the controller. It needs two going down now
                # (PIN_MIN_RESERVE_SEGMENTS_DOWN), and the P2 census landed every unshaped pin, so
                # a flat leg here is a leftover the next census can drop rather than a rule this
                # test is enforcing. A
                # departure has to be a single flat leg and has to say why, in the case, rather
                # than be discovered later in a graph nobody can explain. Whether the queue still
                # bound is not a manifest rule and cannot be: the measurement checks it.
                profile = case.get("network_profile")
                if profile is not None:
                    self.assertEqual(len(profile), 1, "a shaped census point must be FLAT")
                    self.assertTrue(case.get("_shaped_reason"),
                                    "a shaped census point must record why it departs")
                self.assertIn(case.get("abr_pin"), ladder, "pin must name a real actuator request")
                self.assertEqual(case["name"], f"pipe_abr_pin_{case['abr_pin']}",
                                 "the name and the pin are two statements of one number")
                self.assertEqual(case["expect"].get("abr_shape"), {},
                                 "the census reports metrics and grades none of them")
                self.assertIn("auto_network", case, "the census must run the Auto HLS path")

    def test_every_band_case_sweeps_a_derived_ladder_of_legs(self):
        """The `pipe_abr_band_*` family: hold a rung, sweep the LINK, assert nothing.

        These exist for the one region no measurement has ever entered — `A/D` in [0.80, 1.05],
        0 of 366 samples in the whole pre-existing corpus, and the boundary the admission rule is
        keyed on. Waiting for a link to wander into it does not work; the shaper walks `A/D`
        through it directly while a pin holds the rung still, so the load is the only thing moving.

        Four properties. MULTI-LEG, because a flat profile is a census point and cannot sweep
        anything. PINNED, because the whole construction assumes the controller cannot escape the
        rung — the pin short-circuits the decision before the fast-down path, which is also what
        makes it safe to sit past `A/D = 1.0`. EMPTY `abr_shape`, for the census's reason: a bound
        written before the band has ever been observed is a number somebody guessed. And a
        `_band_note` carrying the ARITHMETIC, because every leg rate here is derived from that
        rung's own measured `(bytes, A, C)` rather than chosen, and a derived number whose
        derivation is not written down is indistinguishable from a picked one six months later.
        """
        band = [c for c in _manifest()["pipeline_cases"]
                if c["name"].startswith("pipe_abr_band_")]
        self.assertGreaterEqual(len(band), 2, "the unobserved-band sweeps are missing")
        ladder = {320, 720, 2000, 4000, 6000, 8000, 10000, 12000, 14000, 16000, 18000, 20000, 22000}
        for case in band:
            with self.subTest(case["name"]):
                profile = case.get("network_profile") or []
                self.assertGreater(len(profile), 1,
                                   "a band sweep with one leg sweeps nothing — that is a census point")
                rates = [leg["kbps"] for leg in profile]
                self.assertGreaterEqual(len(set(rates)), 3,
                                        "a sweep needs at least three distinct rates to have an "
                                        "interior — two is a step, which the shaped family covers")
                self.assertIn(case.get("abr_pin"), ladder, "pin must name a real actuator request")
                self.assertEqual(case["name"], f"pipe_abr_band_{case['abr_pin']}",
                                 "the name and the pin are two statements of one number")
                self.assertEqual(case["expect"].get("abr_shape"), {},
                                 "a band sweep reports metrics and grades none of them")
                self.assertIn("auto_network", case, "the band sweep must run the Auto HLS path")
                note = case.get("_band_note", "")
                self.assertIn("A/D", note, "the note must say which load band the legs target")
                self.assertTrue(any(str(r) in note for r in rates),
                                "the note must show the arithmetic for the rates it uses")

    def test_the_census_covers_both_sides_of_the_predicted_binding_crossover(self):
        """The audio lane is predicted to bind below ~1.66 Mbit/s of wire and the video lane above
        it, so a census that sampled only one side could not test that prediction at all."""
        pins = {c["abr_pin"] for c in _manifest()["pipeline_cases"]
                if c["name"].startswith("pipe_abr_pin_")}
        self.assertTrue(any(p <= 720 for p in pins), "no predicted audio-bound point")
        self.assertTrue(any(p >= 10000 for p in pins), "no deep video-bound point")
        self.assertTrue({16000, 20000} <= pins,
                        "the 6,000 ms guard collision is only visible at the top rungs")

    def test_case_declares_a_real_mid_transfer_slow_leg_and_recovery_leg(self):
        legs = self._case()["network_profile"]
        self.assertGreater(legs[0]["kbps"], legs[1]["kbps"])
        self.assertEqual(legs[1]["kbps"], 4000)
        self.assertGreater(legs[2]["kbps"], legs[1]["kbps"])
        self.assertLess(legs[0]["until_s"], legs[1]["until_s"])
        self.assertLess(legs[1]["until_s"], legs[2]["until_s"])

    def test_one_open_body_changes_rate_fast_slow_fast(self):
        """The schedule changes underneath one response, as a router limit does."""
        server = serve_fixtures.FixtureServer.__new__(serve_fixtures.FixtureServer)
        server.lock = threading.Lock()
        server.rate_profile = []
        server.rate_started = None
        server.set_network_profile([
            {"until_s": 0.02, "kbps": 40000},
            {"until_s": 0.12, "kbps": 4000},
            {"until_s": 1.00, "kbps": 40000},
        ])

        class Clock:
            now = 0.0

            def sleep(self, seconds):
                self.now += seconds

        clock = Clock()
        body = io.BytesIO()
        with mock.patch.object(serve_fixtures.time, "monotonic", side_effect=lambda: clock.now), \
             mock.patch.object(serve_fixtures.time, "sleep", side_effect=clock.sleep) as sleeps:
            for _ in range(4):
                server.write_body(body, b"x" * (64 * 1024))

        delays = [call.args[0] for call in sleeps.call_args_list]
        self.assertEqual(len(body.getvalue()), 4 * 64 * 1024)
        self.assertEqual(len(delays), 4)
        self.assertAlmostEqual(delays[0], 64 * 1024 * 8 / 40_000_000, places=6)
        self.assertAlmostEqual(delays[1], delays[0], places=6)
        self.assertAlmostEqual(delays[2], 64 * 1024 * 8 / 4_000_000, places=6)
        self.assertAlmostEqual(delays[3], delays[0], places=6)

    def test_missing_private_hls_segments_skip_instead_of_failing_on_the_tv(self):
        case = {"name": "abr", "fixture": "main.mkv", "auto_network": {"source_kbps": 8000},
                "expect": {"min_pos_climb_s": 1}}
        with tempfile.TemporaryDirectory() as root:
            with open(os.path.join(root, "main.mkv"), "wb") as stream:
                stream.write(b"x")
            saved = run._probe_fixture
            try:
                run._probe_fixture = lambda _path: None
                run._resolve_fixtures([case], root)
                self.assertIn("segment fixtures", case["skip"])
            finally:
                run._probe_fixture = saved


class NetcondRate(unittest.TestCase):
    """`tools/netcond.py`'s `rate:<kbps>` — the mode LG #43 CASE1's legs are produced with.

    Driven through the REAL proxy (`start_proxy` -> `serve_conn` -> `relay`) over loopback rather
    than against the bucket alone: the bucket is arithmetic and cannot be wrong in an interesting
    way, while everything that HAS been wrong here lives at the seam — the scope thrown away
    before it reached `relay`, tokens charged for bytes that never arrived, a mode that could not
    be changed under an open transfer.

    Graded from ABOVE only. A shaper must not EXCEED what was asked for; a lower bound would be
    grading this machine's scheduler under whatever else `make check` is running, which is the
    flaky direction.
    """

    #: 0.128 s per throttled pull. Every timing below is a multiple of that, and the class as a
    #: whole is budgeted at half a second: `make check` is the command run before every other one.
    N = 32 * 1024
    KBPS = 2048.0
    #: The wall time a full transfer CANNOT beat at KBPS.
    FLOOR_S = N * 8 / (KBPS * 1000)

    def setUp(self):
        # A cleaned-up temp dir, like every other temp use in this file: six of these leaked per
        # `make check` while it was a bare `mkdtemp`, and `make check` is the command run most.
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.ctl = os.path.join(self.tmp, "netcond.mode")
        # The proxy narrates every connection; a test runner is not where that belongs.
        saved_sink = netcond.SINK
        netcond.SINK = lambda _msg: None
        self.addCleanup(lambda: setattr(netcond, "SINK", saved_sink))
        self.origin, oport = netcond.start_origin(self.N)
        self.addCleanup(self.origin.close)
        self.mode = netcond.Mode(self.ctl, "pass")
        self._set("pass")
        self.proxy, self.port = netcond.start_proxy(
            0, ("127.0.0.1", oport), self.mode, bind="127.0.0.1")
        self.addCleanup(self.proxy.close)

    def _set(self, raw):
        with open(self.ctl, "w") as f:
            f.write(raw)

    def _pull(self, path="/library/parts/1/file.mkv"):
        netcond.BUCKET.reset()
        c = socket.create_connection(("127.0.0.1", self.port), timeout=30)
        try:
            c.sendall(f"GET {path} HTTP/1.1\r\nHost: x\r\n\r\n".encode())
            t0 = time.monotonic()
            got = b""
            while len(got) < self.N:
                b = c.recv(65536)
                if not b:
                    break
                got += b
            return time.monotonic() - t0, got
        finally:
            c.close()

    def test_the_bucket_never_hands_out_more_than_the_rate(self):
        """1 kbps = 1000 bits/s, decimal — the unit the checklist item states its legs in."""
        b = netcond.RateBucket()
        t0 = time.monotonic()
        total = 0
        while time.monotonic() - t0 < 0.1:
            total += b.take(512, 1 << 20)
        # 512 kbps = 64000 B/s; over the elapsed window, plus the burst capacity it may hold.
        allowed = 64000.0 * (time.monotonic() - t0) + max(64000.0 * b.BURST_S, b.MIN_CAP)
        self.assertLessEqual(total, allowed, f"granted {total} bytes, ceiling {allowed:.0f}")

    def test_the_bucket_starts_empty(self):
        """A full one hands the first quarter-second a free burst — which, on transfers this
        short, is most of the transfer, and makes an absent throttle measure as a working one.

        Graded against what a FULL bucket would grant rather than against zero: `take` refills from
        the wall clock, so any descheduling between the constructor and the first call accrues real
        tokens (2 bytes = 31 us at 512 kbps, and an 8-up `make check` produces exactly that). An
        exact `== 0` here is a test of the machine's scheduler, not of the bucket.
        """
        b = netcond.RateBucket()
        fresh = b.take(512, 1 << 20)
        full = max(512 * 1000 / 8 * b.BURST_S, b.MIN_CAP)
        self.assertLess(fresh, full / 8,
                        f"a fresh bucket granted {fresh} bytes; a FULL one grants {full:.0f}")

    def test_a_rate_mode_shapes_a_real_transfer(self):
        self._set("pass")
        free_s, free = self._pull()
        self.assertEqual(len(free), self.N)
        self._set(f"rate:{self.KBPS:g}")
        slow_s, slow = self._pull()
        self.assertEqual(slow, free, "the shaper corrupted or truncated the body")
        self.assertGreater(slow_s, free_s, "rate: changed nothing")
        self.assertLessEqual(len(slow) * 8 / slow_s / 1000, self.KBPS * 1.35,
                             f"measured faster than the requested {self.KBPS:g} kbps "
                             f"(floor {self.FLOOR_S:.3f}s, took {slow_s:.3f}s)")

    def test_a_scoped_rate_leaves_other_connections_alone(self):
        """The half that was broken until 2026-08-23: `relay` took `Mode.split`, which discards the
        scope, so a scoped mode really applied to every open connection. Scoping is the whole
        reason a #43 leg can throttle the media stream while the control calls stay fast."""
        self._set(f"rate:{self.KBPS:g}@/library/parts")
        slow_s, slow = self._pull("/library/parts/1/file.mkv")
        fast_s, fast = self._pull("/:/timeline?x=1")
        self.assertEqual(len(slow), self.N)
        self.assertEqual(len(fast), self.N)
        # Both bounds sit on the SLOW side of what they grade: a throttled transfer can only be
        # made slower by a busy machine, and an unthrottled loopback pull measures in milliseconds
        # against a 64 ms allowance. Neither can be tripped by scheduling noise.
        self.assertGreater(slow_s, self.FLOOR_S * 0.8,
                           f"the in-scope connection was not throttled ({slow_s:.3f}s)")
        self.assertLess(fast_s, self.FLOOR_S * 0.5,
                        f"the out-of-scope connection WAS throttled ({fast_s:.3f}s) — the scope is "
                        f"not being honoured per connection")

    def test_a_malformed_mode_passes_rather_than_killing_the_connection(self):
        """The control file is edited by hand mid-experiment; `int()` raising inside a relay thread
        drops a live connection with a traceback that reads like a proxy bug."""
        self.assertIsNone(netcond.arg_of("rate:", "rate"))
        self.assertIsNone(netcond.arg_of("rate:fast", "rate"))
        self.assertIsNone(netcond.arg_of("delay:soon", "delay"))
        self.assertEqual(netcond.arg_of("rate:512", "rate"), 512.0)
        self.assertEqual(netcond.arg_of("delay:250", "delay"), 250.0)
        self.assertIsNone(netcond.arg_of("stall", "rate"))
        self._set("rate:oops")
        _s, body = self._pull()
        self.assertEqual(len(body), self.N)

    def test_the_mode_is_live_under_an_open_transfer(self):
        """One scripted run has to cover four CASE1 legs; four launches is not the same experiment,
        because the app's own state differs between them."""
        self._set(f"rate:{self.KBPS / 4:g}")
        t = threading.Timer(0.1, self._set, args=("pass",))
        t.start()
        self.addCleanup(t.cancel)
        live_s, body = self._pull()
        self.assertEqual(len(body), self.N)
        # A quarter of the rate is four times the floor; releasing it has to land well inside that.
        self.assertLess(live_s, self.FLOOR_S * 4,
                        "releasing the mode mid-transfer changed nothing")


# ---------------------------------------------------------------------------
# The ABR observation metrics added by increment I0 of docs/adaptive-playback-plan.md.
#
# Classification (plan §8): every test in this block is a MATHEMATICAL INVARIANT or an
# INTEGRATION test. None of them is a policy-choice test — nothing here asserts that any rung,
# threshold, cooldown or decision is correct, and nothing here may be given a bound taken from a
# run of the code it grades.
# ---------------------------------------------------------------------------
def _sample_line(current=10000, media=9800, net=40000, buf=8000, vbuf=8000,
                 abuf="8200ms", dur=2000, prod=300, n=5, decision="stay", target=0):
    return (f"[  12.345] abr: sample current={current}kbps media={media}kbps net={net}kbps "
            f"buf={buf}ms vbuf={vbuf}ms abuf={abuf} dur={dur}ms prod={prod}pm n={n} "
            f"decision={decision} target={target}kbps reason=None")


def _stamped(lines, stamps):
    """A `StampedLines` with the arrival times a live run would have recorded."""
    return run.StampedLines(lines, stamps)


class AbrTraceMetrics(unittest.TestCase):
    """MATHEMATICAL INVARIANT: the metrics compute what their names say, on hand-built input."""

    def test_a_sample_line_round_trips_every_field(self):
        got = run.abr_samples([_sample_line(abuf="none", decision="prime_down", target=4000)])
        self.assertEqual(len(got), 1)
        self.assertEqual(got[0]["current_kbps"], 10000)
        self.assertEqual(got[0]["media_kbps"], 9800)
        self.assertEqual(got[0]["buf_ms"], 8000)
        self.assertIsNone(got[0]["abuf_ms"], "a silent lane must be None, not 0")
        self.assertEqual(got[0]["decision"], "prime_down")
        self.assertEqual(got[0]["target_kbps"], 4000)
        self.assertEqual(got[0]["dur_ms"], 2000)
        # fetch span is derived, never logged: media x dur / net.
        self.assertAlmostEqual(got[0]["fetch_ms"], 9800 * 2000 / 40000)
        self.assertIsNone(got[0]["at"], "a plain list carries no arrival stamp")

    def test_the_minimum_reserve_is_the_minimum_observed(self):
        lines = [_sample_line(buf=8000), _sample_line(buf=1200), _sample_line(buf=6000)]
        self.assertEqual(run.abr_min_buf_ms(run.abr_samples(lines)), 1200)
        self.assertIsNone(run.abr_min_buf_ms([]), "no samples must read as absent, not as 0")

    def test_min_buf_sees_a_trough_that_the_steady_line_cannot(self):
        """INTEGRATION, and the whole justification for the new log line.

        `abr: steady` is emitted only on `Decision::Stay`. The lowest-reserve segment of a
        drawdown is by construction the one that decides to move, so it emits no steady line at
        all — a minimum read from that source is blind to exactly the sample it is named for.
        Here the trough is a `prime_down` segment: the metric must see it, and it must be lower
        than anything the steady-only view could have reported.
        """
        lines = [_sample_line(buf=9000), _sample_line(buf=900, decision="prime_down", target=4000),
                 _sample_line(buf=7000)]
        steady_only = [s["buf_ms"] for s in run.abr_samples(lines) if s["decision"] == "stay"]
        self.assertEqual(run.abr_min_buf_ms(run.abr_samples(lines)), 900)
        self.assertGreater(min(steady_only), 900,
                           "the fixture no longer exercises the blindness it exists to prove")

    def test_the_dip_window_comes_from_the_shaper_not_from_the_observations(self):
        """MATHEMATICAL INVARIANT: window from the plant, value from observed transport.

        Six segments arrive one second apart; the shaper declares a degraded leg covering the
        third and fourth. `net=` is held CONSTANT across all six, so a metric that inferred the
        dip from the app's own delivery could not find a window at all — which is the property the
        earlier `net < 0.5 * peak` version lacked. `current=` is constant too, so a metric derived
        from the controller's chosen rung could not produce the expected value either.
        """
        stamps = [100.0, 101.0, 102.0, 103.0, 104.0, 105.0]
        media = [9800, 9800, 2100, 1900, 9800, 9800]
        lines = _stamped([_sample_line(net=40000, media=m) for m in media], stamps)
        windows = [(101.6, 103.4, 3000)]          # covers the samples at 102 and 103
        kbps, note = run.abr_dip_max_kbps(run.abr_samples(lines), windows)
        self.assertEqual(kbps, 2100)
        self.assertIn("2 segment(s)", note)
        self.assertIn("3000kbps", note)

    def test_a_segment_that_only_overlaps_the_dip_still_counts(self):
        """The span is `[at - fetch_ms, at]`: a segment half of which crossed the bad link was
        affected by it. Here the sample ARRIVES after the leg ends but began inside it."""
        lines = _stamped([_sample_line(net=1000, media=2000, dur=2000)], [110.0])
        # fetch = 2000 x 2000 / 1000 = 4000 ms, so the span is [106.0, 110.0].
        self.assertEqual(run.abr_dip_max_kbps(run.abr_samples(lines), [(104.0, 107.0, 500)])[0],
                         2000)
        self.assertIsNone(run.abr_dip_max_kbps(run.abr_samples(lines), [(100.0, 105.0, 500)])[0])

    def test_a_flat_profile_has_no_dip_and_says_so(self):
        lines = _stamped([_sample_line()] * 6, [float(i) for i in range(6)])
        kbps, note = run.abr_dip_max_kbps(run.abr_samples(lines), [])
        self.assertIsNone(kbps)
        self.assertIn("no degraded leg", note)

    def test_an_unstamped_log_reports_that_it_cannot_be_placed(self):
        """A plain list (a non-stream_case path) must say why rather than guess."""
        kbps, note = run.abr_dip_max_kbps(run.abr_samples([_sample_line()]), [(1.0, 2.0, 500)])
        self.assertIsNone(kbps)
        self.assertIn("arrival stamp", note)

    def test_stalls_come_from_the_media_clock_not_from_the_buffer_model(self):
        """MATHEMATICAL INVARIANT: a stall is `pos=` failing to advance, and nothing else.

        Not the starvation horizon, not `buffered < threshold`, not `starving()`. The series here
        advances, holds for three beats, advances, holds for one: max 3, total 4.
        """
        beats = [1, 2, 3, 3, 3, 3, 4, 5, 5, 6]
        lines = [f"loop=60 fps=60 pos={p}s" for p in beats]
        self.assertEqual(run.abr_stalls(lines), (3, 4, len(beats)))

    def test_a_run_too_short_to_judge_reports_absence(self):
        self.assertEqual(run.abr_stalls(["loop=60 fps=60 pos=1s"]), (None, None, 1))

    def test_raster_changes_count_transitions_not_commits(self):
        """MATHEMATICAL INVARIANT: eight rungs share 1920x1080 and are eventless to a viewer."""
        lines = [
            "abr: committed Up to 8000kbps 1920x1080",
            "abr: committed Up to 14000kbps 1920x1080",   # same raster: not an event
            "abr: committed Down to 4000kbps 1280x720",   # a raster band crossing
            "abr: committed Up to 8000kbps 1920x1080",    # and back
        ]
        self.assertEqual(run.abr_raster_changes(lines), 2)

    def test_characterisation_reports_the_three_baseline_observations(self):
        """INTEGRATION: the text increment I1 records about unmodified HEAD."""
        lines = [
            "abr: history switches=2 since_last=41000 advanced=0ms",
            "abr: seed rung=720kbps prior=2100kbps slow=2100kbps fast=2100kbps unc=500pm n=1 pin=none",
            _sample_line(current=720, media=700, buf=1958, decision="prime_down", target=320),
        ]
        notes = run.abr_characterisation(lines)
        self.assertTrue(any("first segment" in n and "buf=1958ms" in n for n in notes))
        self.assertTrue(any("seed:" in n and "prior=2100kbps" in n for n in notes))
        self.assertTrue(any("advanced=0ms" in n for n in notes), "the decay input must be visible")

    def test_the_shape_story_reports_every_metric_even_with_no_samples(self):
        """INTEGRATION: a case whose controller never logged a sample must SAY the metrics are
        blind rather than quietly reporting nothing."""
        ok, story = run.a_abr_shape(["abr: steady current=8000kbps"], {})
        self.assertTrue(ok, "no bound was requested, so nothing may fail")
        self.assertIn("min_buf_ms=n/a", story)
        self.assertIn("WARNING", story)


class AbrLogLineContract(unittest.TestCase):
    """MATHEMATICAL INVARIANT: the app's format string and this harness's regex are ONE statement.

    They are written in two languages in two files, and nothing but this test keeps them in step.
    The failure mode is silent and total: rename or reorder a field in `ff.rs` and every metric in
    `a_abr_shape` reads `n/a` forever, which looks exactly like a controller that never ran. That
    is the same shape as the stale-regex bug this project has already had once, when the heartbeat
    fields were renamed and an old log read as "no samples".
    """

    FF = os.path.join(REPO_ROOT, "rust-modules", "src", "ff.rs")

    def _emitted_fields(self, prefix):
        """The `name=` fields of the app's format literal for `abr: <prefix>`, in order."""
        with open(self.FF) as f:
            src = f.read()
        start = src.index(f'"abr: {prefix} ')
        end = src.index('",', start)
        # A trailing backslash continues a Rust string literal and strips the newline plus the
        # next line's indentation, so join the same way the compiler does before reading fields.
        literal = re.sub(r"\\\s*\n\s*", "", src[start:end])
        return re.findall(r"(\w+)=", literal)

    def _regex_fields(self, pattern):
        return re.findall(r"(\w+)=", pattern.pattern)

    def test_the_sample_line_emits_exactly_the_fields_the_harness_parses(self):
        emitted = self._emitted_fields("sample")
        parsed = self._regex_fields(run.RE_ABR_SAMPLE)
        self.assertEqual(emitted[: len(parsed)], parsed,
                         f"ff.rs emits {emitted}, run.py parses {parsed}")
        self.assertIn("reason", emitted, "the decision reason must stay on the line")

    def test_the_seed_and_history_lines_match_their_regexes(self):
        for prefix, pattern in (("seed", run.RE_ABR_SEED), ("history", run.RE_ABR_HISTORY)):
            with self.subTest(line=prefix):
                self.assertEqual(self._emitted_fields(prefix), self._regex_fields(pattern))

    def test_a_rendered_line_actually_matches(self):
        """Belt and braces: the field NAMES agreeing is not the same as the line parsing."""
        self.assertIsNotNone(run.RE_ABR_SAMPLE.search(_sample_line()))
        self.assertIsNotNone(run.RE_ABR_SAMPLE.search(_sample_line(abuf="none", buf=-1)))


class ShaperSchedule(unittest.TestCase):
    """MATHEMATICAL INVARIANT: the plant's account of what it did to the link."""

    def _server(self, profile, started=1000.0):
        srv = serve_fixtures.FixtureServer.__new__(serve_fixtures.FixtureServer)
        srv.lock = threading.Lock()
        srv.rate_profile = [(float(l["until_s"]), int(l["kbps"])) for l in profile]
        srv.rate_started = started
        return srv

    def test_legs_become_absolute_intervals_on_the_shared_clock(self):
        srv = self._server([{"until_s": 20, "kbps": 40000}, {"until_s": 23, "kbps": 200},
                            {"until_s": 240, "kbps": 40000}])
        self.assertEqual(srv.rate_windows(),
                         [(1000.0, 1020.0, 40000), (1020.0, 1023.0, 200),
                          (1023.0, None, 40000)])

    def test_the_dip_is_every_leg_below_the_fastest(self):
        """`pipe_abr_oscillating_link`'s shape: alternating legs, ending slow.

        The final leg extends to infinity by construction — a profile's last entry is what the
        shaper holds for the rest of the run — so its window is open-ended, and a case that ends
        degraded has a dip that runs to the end of the log. That is the real manifest's shape.
        """
        srv = self._server([{"until_s": 12, "kbps": 20000}, {"until_s": 20, "kbps": 3000},
                            {"until_s": 28, "kbps": 20000}, {"until_s": 36, "kbps": 3000}])
        self.assertEqual([(a, b) for a, b, _ in srv.dip_windows()],
                         [(1012.0, 1020.0), (1028.0, None)])

    def test_a_bounded_final_leg_closes_its_window(self):
        srv = self._server([{"until_s": 20, "kbps": 40000}, {"until_s": 23, "kbps": 200},
                            {"until_s": 240, "kbps": 40000}])
        self.assertEqual([(a, b) for a, b, _ in srv.dip_windows()], [(1020.0, 1023.0)])

    def test_a_flat_or_unstarted_profile_has_no_windows(self):
        self.assertEqual(self._server([{"until_s": 240, "kbps": 6000}]).dip_windows(), [])
        srv = self._server([{"until_s": 20, "kbps": 40000}, {"until_s": 40, "kbps": 200}])
        srv.rate_started = None      # no response body yet: the phase clock has not begun
        self.assertEqual(srv.rate_windows(), [])


class StampedLog(unittest.TestCase):
    """MATHEMATICAL INVARIANT: the arrival clock survives the paths the harness actually uses."""

    def test_stamps_track_lines_and_survive_a_snapshot(self):
        log = run.StampedLines()
        log.append("a")
        log.append("b")
        self.assertEqual(len(log.stamps), 2)
        self.assertLessEqual(log.stamps[0], log.stamps[1])
        snap = log.snapshot()
        log.append("c")
        self.assertEqual(list(snap), ["a", "b"], "a snapshot must not follow later appends")
        self.assertEqual(len(snap.stamps), 2)

    def test_it_is_still_an_ordinary_list_to_every_existing_caller(self):
        log = run.StampedLines(["loop=60 fps=60 pos=3s"], [1.0])
        self.assertEqual(run.playpos_secs(log), [(3, "loop=60 fps=60 pos=3s")])
        self.assertEqual(list(log), ["loop=60 fps=60 pos=3s"])


class AbrTriggers(unittest.TestCase):
    """INTEGRATION: the two manifest keys measurement steps M4 and I5/I6 depend on."""

    BASE = {"name": "t", "fixture": "f.ts", "operations": [], "declare": {},
            "auto_network": {"source_kbps": 60000}}

    def _names(self, case):
        return dict(run.triggers_for_case(case, url_base="http://h:8020"))

    def test_a_pin_becomes_a_trigger_and_is_absent_by_default(self):
        self.assertNotIn("plxnative-abrpin", self._names(dict(self.BASE)))
        self.assertEqual(self._names({**self.BASE, "abr_pin": 14000})["plxnative-abrpin"], "14000")

    def test_the_policy_selector_becomes_a_trigger_and_is_absent_by_default(self):
        """It must ride the manifest: `apply_triggers` wipes every plxnative-* before each case,
        so a hand-armed A/B selector cannot survive into the case it is meant to switch."""
        self.assertNotIn("plxnative-abrpolicy", self._names(dict(self.BASE)))
        self.assertEqual(
            self._names({**self.BASE, "abr_policy": "legacy"})["plxnative-abrpolicy"], "legacy")

    def test_no_manifest_case_carries_a_new_abr_bound_yet(self):
        """POLICY GUARD, not a policy test. Increment I0 adds the metrics and deliberately grades
        none of them: a bound written before the I1 baseline exists is a number somebody guessed.
        If a later increment adds one on purpose, delete this test in that commit and say so.
        """
        # It searched `cases` -- the SERVER tier -- until 2026-08-26, where 0 of 21 carry an
        # `abr_shape` block at all: the guard could not fire, and the 11 cases that do carry one
        # live in `pipeline_cases`. Both lists are searched now, so moving a case between tiers
        # cannot evade it either.
        manifest = _manifest()
        lists = [k for k, v in manifest.items() if isinstance(v, list)]
        named = [f"{k}:{c['name']}.{metric}" for k in lists for c in manifest[k]
                 for metric in ("min_buf_ms", "max_stall_s", "raster_changes_max")
                 if metric in (c.get("expect") or {}).get("abr_shape", {})]
        self.assertEqual(named, [], f"cases already assert an I0 metric: {named}")
        # And the guard must be able to SEE the cases it is guarding, or it is vacuous again.
        carriers = [c["name"] for k in lists for c in manifest[k]
                    if "abr_shape" in (c.get("expect") or {})]
        self.assertTrue(carriers, "no case carries abr_shape -- this guard is searching nothing")


class AbrLadderFixtures(unittest.TestCase):
    """MATHEMATICAL INVARIANT: every rung the route can request resolves to a distinct clip."""

    def test_every_rung_has_its_own_rate_targeted_clip(self):
        """The reachable reserve is `queue_bytes / media_rate`, so rungs that share a file report
        the same reserve and measurement step M4 can measure nothing.

        SCOPED TO >= 6000 until 2026-08-26, and the gap it left was measured: rungs 2000 and 4000
        both mapped to `pipe_abr_720p.ts` and delivered the identical 3 183 kbps, so the ladder
        was non-monotone in relief there and an adjacent-pair experiment across that step measured
        nothing at all. Every rung now has its own clip, so the check covers every rung.
        """
        files = list(serve_fixtures.ABR_FIXTURE.values())
        self.assertEqual(len(set(files)), len(files), f"rungs share a clip: {sorted(files)}")

    def test_every_rung_is_rate_targeted_not_quality_targeted(self):
        """A CRF clip's bitrate is a CONSEQUENCE, so the rung does not deliver what it names.

        Measured before this was enforced: the four low rungs ran 1.57x to 1.90x of their own
        request while the rate-targeted ones sat inside 1.14x, which is most of the 2.4x
        nominal/delivered spread that refuted the admission rule (board finding R1).
        """
        shapes = fixturegen.TIERS["pipeline"]["shapes"]
        for rung, rel in sorted(serve_fixtures.ABR_FIXTURE.items(), key=lambda kv: int(kv[0])):
            with self.subTest(rung=rung):
                video = shapes[rel[: -len(".ts")]]["video"]
                self.assertIn("vbr", video,
                              f"rung {rung} is encoded to a QUALITY ({video.get('crf')}), so what "
                              "it delivers is whatever that happens to cost")
                audio = sum(int(str(a.get("br", "0k")).rstrip("k") or 0)
                            for a in shapes[rel[: -len(".ts")]].get("audio", []))
                self.assertEqual(video["vbr"] + audio, int(rung),
                                 f"rung {rung}'s video target plus its audio track must sum to "
                                 "the rung, or the muxed stream misses the rung it names")

    def test_the_generator_declares_every_clip_the_server_serves(self):
        shapes = fixturegen.TIERS["pipeline"]["shapes"]
        for rung, rel in serve_fixtures.ABR_FIXTURE.items():
            self.assertIn(rel[: -len(".ts")], shapes, f"rung {rung} names an ungenerated clip")

    def test_a_rate_targeted_clip_encodes_to_its_target_not_to_a_quality(self):
        shape = fixturegen.TIERS["pipeline"]["shapes"]["pipe_abr_1080p_10m"]
        args = fixturegen.venc_args(shape["video"])
        self.assertIn("-b:v", args)
        self.assertNotIn("-crf", args)
        # rung request minus the 192 kbps audio track, so the muxed stream lands on the rung.
        self.assertEqual(args[args.index("-b:v") + 1], "9808k")




# ---------------------------------------------------------------------------------------------
# The log-line contract between the Rust app and this harness.
#
# These two lines are the ONLY record of what a quality transaction cost and what a segment
# acquisition was made of, and the app truncates its event log every launch -- so a regex that
# silently stops matching does not fail loudly, it reports "no samples" and every derived
# statistic quietly becomes a statement about the empty set.
#
# The test is DIFFERENTIAL, not a golden literal: it reads the format strings out of ff.rs and
# compares the field names and their ORDER against what the regex captures. A frozen example line
# would keep passing after someone renamed a field in Rust, which is the exact failure this is
# here to catch.
# ---------------------------------------------------------------------------------------------

FF_RS = os.path.join(REPO_ROOT, "rust-modules", "src", "ff.rs")

# `name=` in a format string or a regex pattern. Deliberately anchored on the `=`, because that is
# what both sides actually agree on -- the surrounding placeholder/capture syntax differs.
_FIELD = re.compile(r"([a-z][a-z0-9_]*)=")


def _rust_format_string(source, opening):
    """The single Rust string literal starting with `opening`, with `\\`-continuations joined.

    Rust eats the newline AND the next line's leading whitespace after a trailing backslash, so
    the rendered line is what this reconstructs -- not the source layout.
    """
    start = source.index(opening)
    out = []
    i = start + 1                       # step over the opening quote, or the loop ends at once
    while i < len(source):
        ch = source[i]
        if ch == "\\":
            nxt = source[i + 1]
            if nxt == "\n":
                i += 2
                while i < len(source) and source[i] in " \t":
                    i += 1
                continue
            out.append(ch + nxt)
            i += 2
            continue
        if ch == '"':
            break
        out.append(ch)
        i += 1
    return "".join(out)


class LogLineContract(unittest.TestCase):
    """The Rust format string and the harness regex name the same fields, in the same order."""

    def setUp(self):
        with open(FF_RS, encoding="utf-8") as fh:
            self.ff = fh.read()

    def _assert_contract(self, opening, pattern, fields, label):
        rendered = _rust_format_string(self.ff, opening)
        rust_names = _FIELD.findall(rendered)
        regex_names = _FIELD.findall(pattern.pattern)
        self.assertEqual(
            rust_names,
            regex_names,
            f"{label}: ff.rs emits {rust_names} but run.py's regex reads {regex_names}. "
            "One side was changed without the other; the harness would report 'no samples' "
            "rather than failing, so this is the only place it can be caught.",
        )
        self.assertEqual(
            pattern.groups,
            len(fields),
            f"{label}: {pattern.groups} capture groups but {len(fields)} field names to zip "
            "them against -- the parser would silently drop or misalign columns.",
        )
        # Names and order agreeing does not prove the UNITS do: `decided={}ms` and `decided={}`
        # carry the same field name. So render the format with placeholder values and require the
        # regex to match the result -- that pins every literal between the captures, suffixes
        # included.
        rendered_line = rendered.replace("{:?}", "Up").replace("{}", "1")
        self.assertRegex(
            rendered_line,
            pattern,
            f"{label}: the regex does not match ff.rs's own format string rendered with "
            f"placeholder values -- a separator or a unit suffix differs.\n  {rendered_line}",
        )

    def test_abr_tx_regex_matches_the_rust_format_string(self):
        self._assert_contract('"abr: tx {:?}', run.RE_ABR_TX, run.TX_FIELDS, "abr: tx")

    def test_hls_segment_regex_matches_the_rust_format_string(self):
        self._assert_contract('"hls: segment={}', run.RE_HLS_SEGMENT, run.SEGMENT_FIELDS,
                              "hls: segment")

    def test_abr_tx_parses_a_committed_upshift(self):
        line = (
            "abr: tx Up 4000->6000kbps outcome=committed decided=3065ms total=9563ms "
            "control=118ms prime=94ms master=12ms media=12ms warmup=2210ms graded=1804ms "
            "buf_start=24835ms buf_decided=21770ms feed=6498ms buf_fed=24918ms "
            "buf_end=24918ms cur_acq_before=1583ms net=41200kbps fast=41200kbps "
            "slow=39800kbps unc=120pm"
        )
        rows = run.abr_transactions([line])
        self.assertEqual(len(rows), 1, "the committed-upshift shape must parse")
        row = rows[0]
        self.assertEqual(row["decided_ms"], 3065)
        self.assertEqual(row["feed_ms"], 6498)
        self.assertEqual(row["prime_ms"] + row["master_ms"] + row["media_ms"], row["control_ms"],
                         "the three control legs are a partition of `control=`, not a sample of it")

    def test_abr_tx_reads_none_as_absent_and_never_as_zero(self):
        line = (
            "abr: tx Up 4000->6000kbps outcome=prime_refused decided=41ms total=41ms "
            "control=none prime=none master=none media=none warmup=none graded=none "
            "buf_start=24835ms buf_decided=24835ms feed=nonems buf_fed=nonems "
            "buf_end=24835ms cur_acq_before=1583ms net=41200kbps fast=41200kbps "
            "slow=39800kbps unc=120pm"
        ).replace("control=none", "control=nonems").replace(
            "prime=none ", "prime=nonems ").replace("master=none ", "master=nonems ").replace(
            "media=none ", "media=nonems ").replace("warmup=none ", "warmup=nonems ").replace(
            "graded=none ", "graded=nonems ")
        rows = run.abr_transactions([line])
        self.assertEqual(len(rows), 1, "an early reject still emits a full line")
        self.assertIsNone(rows[0]["control_ms"], "'none' is absence; 0 would claim it was instant")
        self.assertIsNone(rows[0]["feed_ms"])
        self.assertEqual(rows[0]["outcome"], "prime_refused")

    def test_hls_segment_parses_and_keeps_a_missing_ttfb_distinguishable(self):
        line = (
            "hls: segment=42 bytes=1048576 raster=1920x1080 v=48 a=94 tail_skew_ms=-12 "
            "audio_pts_recovered=0 not_ready=1 open_ms=27 ttfb_ms=311 open_probe_ms=340 "
            "first_au_ms=352 total_ms=1583"
        )
        rows = run.hls_segments([line])
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["ttfb_ms"], 311)
        self.assertEqual(rows[0]["open_ms"], 27)
        self.assertEqual(rows[0]["not_ready"], 1)
        self.assertEqual(rows[0]["bytes"], 1048576)
        never = run.hls_segments([line.replace("ttfb_ms=311", "ttfb_ms=-1")])
        self.assertEqual(never[0]["ttfb_ms"], -1,
                         "no byte ever arrived is -1, which is not a fast first byte")


class AbrSegmentVariation(unittest.TestCase):
    """A rung must deliver DIFFERENT segments, not one file ninety times.

    The P1 device run logged 593 segments carrying exactly ten distinct byte sizes -- one per
    fixture file -- because `_resolve` discarded the sequence number. That made `bytes` an exact
    function of `rung`, which is why the transport model had ten data points to fit however long
    the suite ran. This is the regression guard for the fix.
    """

    def _root(self, parts_per_rung):
        """A throwaway pack: `pipe_abr_240p.ts` plus its `_01.._0N` siblings, distinct sizes."""
        root = tempfile.mkdtemp()
        base = serve_fixtures.ABR_FIXTURE["320"]
        stem, _, ext = base.rpartition(".")
        for i in range(parts_per_rung):
            name = base if i == 0 else f"{stem}_{i:02d}.{ext}"
            with open(os.path.join(root, name), "wb") as fh:
                fh.write(b"\0" * (1000 + i))          # a distinct size per part
        self.addCleanup(shutil.rmtree, root, True)
        return root

    def _server(self, root):
        srv = serve_fixtures.FixtureServer.__new__(serve_fixtures.FixtureServer)
        srv.root = os.path.realpath(root)
        srv.lock = threading.Lock()
        srv._abr_parts = {}
        return srv

    def test_a_rung_cycles_through_every_segment_it_has(self):
        srv = self._server(self._root(6))
        parts = srv.abr_parts(serve_fixtures.ABR_FIXTURE["320"])
        self.assertEqual(len(parts), 6, "all six cut segments must be discovered")
        self.assertEqual(len(set(parts)), 6, "and they must be six DIFFERENT files")

    def test_an_old_single_file_pack_still_works(self):
        """Segment 0 keeps the unsuffixed name precisely so this stays true."""
        srv = self._server(self._root(1))
        parts = srv.abr_parts(serve_fixtures.ABR_FIXTURE["320"])
        self.assertEqual(parts, [serve_fixtures.ABR_FIXTURE["320"]],
                         "a pack without the cut siblings must degrade to the old behaviour")

    def test_the_sequence_number_selects_the_segment(self):
        """The defect in one assertion: sequence 0..5 must not all resolve to one file."""
        srv = self._server(self._root(6))
        parts = srv.abr_parts(serve_fixtures.ABR_FIXTURE["320"])
        picked = [parts[n % len(parts)] for n in range(12)]
        self.assertEqual(len(set(picked)), 6,
                         "twelve sequence numbers reached only "
                         f"{len(set(picked))} distinct file(s) — the sequence is being ignored")
        self.assertEqual(picked[0], picked[6], "and the cycle must repeat, not run out")

    def test_every_abr_shape_declares_a_cut(self):
        """Derived from the generator, so a rung added tomorrow cannot quietly serve one file."""
        shapes = fixturegen.TIERS["pipeline"]["shapes"]
        served = {rel[: -len(".ts")] for rel in serve_fixtures.ABR_FIXTURE.values()}
        for key in sorted(served):
            with self.subTest(key):
                self.assertGreaterEqual(
                    int(shapes[key].get("hls_segments") or 0), 2,
                    f"{key} is served as an ABR rung but is not cut into segments, so that rung "
                    "delivers one byte size for the whole playback")


class SharedLinkShaping(unittest.TestCase):
    """The fixture server's rate limiter must shape the LINK, not each response separately.

    It shaped each response independently until 2026-08-26, so N concurrent transfers each got the
    full nominal rate. That is what made every Original-probe measurement on this tier
    inadmissible: a probe runs beside the segment stream, and the pair was measured at 1.89x the
    rate the profile asked for. These tests are about the arithmetic of `write_body`'s virtual
    clock, driven through a real server object but writing to an in-memory sink, so they are fast
    and bind no socket.
    """

    class _Sink:
        """Stands in for a socket. `write_body` only ever calls write() and flush()."""
        def __init__(self):
            self.n = 0

        def write(self, data):
            self.n += len(data)

        def flush(self):
            pass

    def _server(self, kbps):
        srv = serve_fixtures.FixtureServer.__new__(serve_fixtures.FixtureServer)
        srv.lock = threading.Lock()
        srv.rate_profile = [(1e9, kbps)]
        srv.rate_started = None
        srv.link_free_at = None
        return srv

    def _drive(self, srv, writers, chunk, chunks):
        started = time.monotonic()
        threads = []
        for _ in range(writers):
            sink = self._Sink()
            th = threading.Thread(
                target=lambda s=sink: [srv.write_body(s, b"x" * chunk) for _ in range(chunks)])
            th.start()
            threads.append(th)
        for th in threads:
            th.join()
        return time.monotonic() - started

    def _delivered_kbps(self, kbps, writers, chunk, chunks):
        elapsed = self._drive(self._server(kbps), writers, chunk, chunks)
        return (writers * chunks * chunk * 8) / elapsed / 1000.0

    def test_the_link_is_shared_so_concurrent_writers_cannot_exceed_it(self):
        """N writers must not deliver N times the link.

        Stated as a ONE-SIDED bound on aggregate throughput, which is the only overhead-robust
        form. Thread setup and loop overhead can make delivery slower and never faster, so an
        over-delivery cannot be an artefact of a busy host -- while an under-delivery says nothing
        about the shaper.

        Two other formulations were tried first and both measure overhead rather than shaping. An
        elapsed-time ratio between one and two writers reads ~1.5x for a true 2.0x, because the
        fixed cost does not scale with the writer count. A throughput ratio is worse: at these
        sizes a single writer is overhead-dominated and under-reads by ~30%, which moves the ratio
        to 1.36 and is indistinguishable from the defect it is meant to detect.

        Under the per-response shaper this replaced, each writer got the full nominal rate, so
        three of them delivered about three times the link.
        """
        chunk, chunks, nominal = 8192, 12, 4000
        for writers in (1, 2, 3):
            with self.subTest(writers=writers):
                delivered = self._delivered_kbps(nominal, writers, chunk, chunks)
                self.assertLess(
                    delivered, nominal * 1.30,
                    f"{writers} writer(s) delivered {delivered:.0f} kbps over a nominal "
                    f"{nominal} kbps link — a shared link delivers the same total however many "
                    "writers there are, so the shaping is per-response (the 1.89x defect)")

    def test_an_unshaped_server_does_not_sleep_at_all(self):
        srv = self._server(4000)
        srv.rate_profile = []
        elapsed = self._drive(srv, 2, 65536, 8)
        self.assertLess(elapsed, 0.5, "no profile installed means no shaping, at any size")

    def test_an_idle_link_banks_no_credit(self):
        """`max(now, link_free_at)` — otherwise a pause lets the next chunk through for free."""
        srv = self._server(1000)
        srv.write_body(self._Sink(), b"x" * 8192)
        time.sleep(0.15)
        started = time.monotonic()
        srv.write_body(self._Sink(), b"x" * 8192)
        after_idle = time.monotonic() - started
        expected = 8192 * 8 / (1000 * 1000.0)
        self.assertGreater(after_idle, expected * 0.7,
                           "the chunk after an idle gap was not charged for the link")


def abr_shape_keys():
    """The bounds `a_abr_shape` actually implements, read from its SOURCE.

    This was a hand-written set of five, and it omitted the three metrics the function has read
    since increment I0 (`min_buf_ms`, `max_stall_s`, `raster_changes_max`) -- so a case using one
    of them would have been rejected as an unknown key even though the grader honours it.

    Deriving it from the implementation is the same trick as the log-line contract above, and for
    the same reason: a restated list drifts silently, and both directions of the drift are bad. An
    unlisted-but-implemented key blocks a legitimate bound; a listed-but-unimplemented key lets a
    case declare a bound that nothing ever reads, which passes forever.
    """
    src = inspect.getsource(run.a_abr_shape)
    return set(re.findall(r'spec\.get\("([a-z_]+)"', src))


class EarlyExitSoundness(unittest.TestCase):
    """Early exit is sound only for MONOTONE assertions. These are the exceptions.

    The failure it guards is a false PASS, not a false fail: `max_commits` counts events over
    whatever window it was given, so a run that stops early scores LOWER and a regression that
    adds rung changes can slip under the bound. Observed on the real matrix -- 7 changes on a full
    window, passing a 5-bound early, 8 on a later full window, same binary.
    """

    def test_abr_shape_never_grades_on_a_partial_window(self):
        allowed, why = run.early_exit_allowed({"expect": {"abr_shape": {"max_commits": 5}}}, {})
        self.assertFalse(allowed, "a commit COUNT cannot be graded on a truncated window")
        self.assertIn("window", why, "the reason has to say why, it appears in the run output")

    def test_gst_trace_never_grades_early(self):
        allowed, _ = run.early_exit_allowed({"expect": {}, "gst_trace": True}, {})
        self.assertFalse(allowed)

    def test_an_ordinary_case_still_exits_early(self):
        allowed, why = run.early_exit_allowed({"expect": {"codec": "h264", "no_error": True}}, {})
        self.assertTrue(allowed, "early exit is the default and is what keeps the suite quick")
        self.assertEqual(why, "")

    def test_no_early_needs_no_explanation(self):
        allowed, why = run.early_exit_allowed({"expect": {}}, {"no_early": True})
        self.assertFalse(allowed)
        self.assertEqual(why, "", "the operator asked for it; printing a reason is noise")

    def test_every_manifest_case_with_abr_shape_is_covered(self):
        """Not a fixture -- the REAL matrix. A case added tomorrow is covered by construction."""
        with open(os.path.join(REPO_ROOT, "tests", "manifest.json"), encoding="utf-8") as fh:
            manifest = json.load(fh)
        cases = [c for c in manifest.get("pipeline_cases", [])
                 if "abr_shape" in (c.get("expect") or {})]
        self.assertTrue(cases, "the matrix should still carry abr_shape cases")
        for case in cases:
            allowed, _ = run.early_exit_allowed(case, {})
            self.assertFalse(allowed, f"{case.get('name')} would grade a commit count early")


class TheAbrWindowLineMatchesTheHarnessRegex(unittest.TestCase):
    """**The other half of a contract whose two sides never meet at runtime.**

    The app formats `abr: window` on a television (`rust-modules/src/abr/window.rs`,
    `AdmissionReadout::log_line`) and this harness parses it on a Mac. Nothing links them, so a
    field renamed on one side and not the other produces "no `abr: window` lines" -- which reads
    exactly like the feature never ran, i.e. like a total regression, on the one tier where the
    only copy of the evidence is the captured log.

    So the Rust test module pins the exact wire form as string constants and this reads them back
    out of the source. It is a source-extraction test rather than a fixture on purpose: a fixture
    copied here would drift with the regex it is supposed to grade, and agree with it forever.
    """

    WINDOW_RS = os.path.join(REPO_ROOT, "rust-modules", "src", "abr", "window.rs")

    @classmethod
    def wire_examples(cls):
        """Every `const … : &str = // wire-example` literal in `window.rs`, un-escaped.

        Rust's `\\` line continuation swallows the newline AND the leading whitespace of the next
        line, which is what lets the source stay inside a line limit while the emitted line is one
        long string. Reproducing that here is the whole extraction.
        """
        with open(cls.WINDOW_RS, encoding="utf-8") as fh:
            source = fh.read()
        out = []
        pattern = r'// wire-example\n\s*"((?:[^"\\]|\\[\s\S])*)"'
        for body in re.findall(pattern, source):
            out.append(re.sub(r"\\\n\s*", "", body))
        return out

    def test_the_examples_are_present(self):
        examples = self.wire_examples()
        self.assertGreaterEqual(len(examples), 2, f"no wire examples found in {self.WINDOW_RS}")
        self.assertTrue(all(e.startswith("abr: window ") for e in examples), examples)

    def test_every_example_parses(self):
        for line in self.wire_examples():
            with self.subTest(line=line):
                rows = run.abr_windows([line])
                self.assertEqual(len(rows), 1, "RE_ABR_WINDOW no longer matches what the app logs")

    def test_a_filling_verdict_parses_as_not_computed_rather_than_zero(self):
        filling = [ln for ln in self.wire_examples() if "verdict=filling" in ln]
        self.assertTrue(filling, "the filling state needs an example; it is every playback's first n")
        row = run.abr_windows(filling)[0]
        for field in ("bound_ms", "demand_ms", "supply_ms", "excess_ms"):
            self.assertEqual(row[field], -1, f"{field} must say NOT COMPUTED, not zero")
        self.assertLess(row["have"], row["want"])

    def test_a_full_verdict_parses_every_term(self):
        full = [ln for ln in self.wire_examples() if "verdict=admit" in ln]
        self.assertTrue(full)
        row = run.abr_windows(full)[0]
        self.assertEqual(row["have"], row["want"])
        self.assertEqual((row["sustainable"], row["survivable"]), (1, 1))
        self.assertLessEqual(row["demand_ms"], row["supply_ms"], "condition (1), as logged")
        self.assertGreaterEqual(row["excess_ms"], 0)

    def test_the_window_line_does_not_also_match_the_sample_regex(self):
        """Both are `abr: ` lines emitted on the same segment; a `search` that matched both would
        double-count every segment in `abr_samples`, which several statistics average over."""
        for line in self.wire_examples():
            self.assertIsNone(run.RE_ABR_SAMPLE.search(line))
            self.assertEqual(run.abr_samples([line]), [])


if __name__ == "__main__":
    unittest.main(verbosity=1)
