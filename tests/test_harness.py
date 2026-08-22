#!/usr/bin/env python3
"""
Host unit tests for the harness itself (`tests/run.py`) — stdlib `unittest`, no TV, no PMS, no
network, ~0.05s. Run by `make check` beside the Rust suite and `ci/flavor.py --selftest`.

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
import sys
import tempfile
import unittest

TESTS_DIR = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, TESTS_DIR)
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
        self.assertTrue(run.a_timeline_climb(timeline_only, 8)[0], "the integration one still folds back")
        self.assertFalse(run.a_pos_climb(timeline_only, 8)[0], "the pipeline one must not")
        heartbeat = [f"loop=60 route=player overlay=none pos={t}s vtick=5" for t in (2, 14)]
        self.assertTrue(run.a_pos_climb(heartbeat, 8)[0])

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

    def _args(self, *argv):
        import argparse
        # Re-parsing main()'s parser is not possible without running main(), so assert on the
        # rule instead, in the same form main() computes it. If that expression ever changes,
        # this test is the thing that has to change with it — which is the point.
        return argv

    def test_the_bare_command_is_the_synthetic_tier(self):
        """Documented in three places (tests/README.md, CLAUDE.md, --help); pinned in one."""
        import subprocess
        out = subprocess.run([sys.executable, os.path.join(TESTS_DIR, "run.py"), "--list"],
                             capture_output=True, text=True, timeout=120)
        self.assertIn("pipe_", out.stdout, "a bare --list must show the synthetic cases")
        self.assertNotIn("dp_h264_ac3_1080p", out.stdout,
                         "a bare --list must NOT show the library-backed cases")

    def test_server_opts_into_the_library_tier(self):
        import subprocess
        out = subprocess.run([sys.executable, os.path.join(TESTS_DIR, "run.py"), "--server", "--list"],
                             capture_output=True, text=True, timeout=120)
        self.assertIn("dp_h264_ac3_1080p", out.stdout)

    def test_contradictory_tier_flags_refuse(self):
        """`--pipeline` names the default, so pairing it with --server/--fps is two instructions,
        not a preference — honouring either one silently is how somebody trusts the wrong result."""
        import subprocess
        for extra in (["--server"], ["--fps"], ["--fps-player"]):
            out = subprocess.run(
                [sys.executable, os.path.join(TESTS_DIR, "run.py"), "--pipeline"] + extra + ["--list"],
                capture_output=True, text=True, timeout=120)
            self.assertNotEqual(out.returncode, 0, f"--pipeline {extra} should refuse")

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


if __name__ == "__main__":
    unittest.main(verbosity=1)
