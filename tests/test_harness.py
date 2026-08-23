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
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest

TESTS_DIR = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(TESTS_DIR)
sys.path.insert(0, TESTS_DIR)
# APPENDED, not inserted: `tools/` is a grab-bag of scripts, and putting it ahead of `tests/` means
# the day somebody adds a `tools/run.py` the import below silently binds the wrong module and this
# whole suite grades a file nobody meant.
sys.path.append(os.path.join(REPO_ROOT, "tools"))
import netcond  # noqa: E402
import run  # noqa: E402  (path juggling above is the point)


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
        """`apply_triggers` writes trigger content through a single-quoted `printf` with NO
        escaping, so ONE apostrophe anywhere in this string ends the quoting and hands the rest to
        the television's shell as a command. JSON's own syntax has none; this pins that the values
        we compose carry none either. Its twin lives in rust-modules/src/dev.rs."""
        for c in self._pipeline_cases():
            files = run.triggers_for_case(c, url_base="http://192.0.2.10:8020")
            self.assertEqual(files[0][0], "plxnative-playurl")
            payload = files[0][1]
            self.assertNotIn("'", payload, f"{c['name']}: would break the single-quoted printf")
            spec = json.loads(payload)
            self.assertEqual(spec["url"], f"http://192.0.2.10:8020/{c['fixture']}")
            for k, v in c.get("declare", {}).items():
                self.assertEqual(spec[k], v)

    def test_the_integration_trigger_is_untouched(self):
        """One function, two heads — so pin that the head this tier did not touch still writes the
        ratingKey trigger and nothing else."""
        files = run.triggers_for_case({"rk": "1234", "operations": [{"op": "play"}]})
        self.assertEqual(files, [("plxnative-play", "1234")])

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

    def test_the_manifest_carries_exactly_one_eos_case_and_it_owns_its_fixture(self):
        cases = _manifest()["pipeline_cases"]
        eos = [c for c in cases if c["expect"].get("reaches_eos")]
        self.assertEqual(len(eos), 1, f"expected one completion case, got {[c['name'] for c in eos]}")
        others = [c["name"] for c in cases
                  if c["fixture"] == eos[0]["fixture"] and c is not eos[0]]
        self.assertFalse(others, f"the short clip ENDS mid-window; {others} would be graded "
                                 f"through a teardown they never asked for")

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
        self.tmp = tempfile.mkdtemp()
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


if __name__ == "__main__":
    unittest.main(verbosity=1)
