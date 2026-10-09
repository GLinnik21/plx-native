#!/usr/bin/env python3
"""Host checks for the strict result/accounting boundary of the real simulator replay gate."""
import importlib.util
import contextlib
import io
import json
from pathlib import Path
import re
import tempfile
import threading
import time
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('replay_fixtures', Path(__file__).with_name('replay_fixtures.py'))
replay = importlib.util.module_from_spec(spec)
spec.loader.exec_module(replay)


class ReplayGateTests(unittest.TestCase):
    def setUp(self):
        self.summary = ('replay: done frames=7 graded=5 ' +
                        ' '.join(field + '=0' for field in replay.DIFFS) + ' verdict=SAME')

    def test_complete_clean_summary_and_successful_exit_are_required(self):
        self.assertEqual(replay.verdict(self.summary, 0, (7, 5)), self.summary)
        for log, code, counts in [('', 0, (7, 5)), (self.summary, 1, (7, 5)),
                                  (self.summary, 0, (8, 5)), (self.summary, 0, (7, 6)),
                                  (self.summary + '\n' + self.summary, 0, (7, 5)),
                                  (self.summary + ' frames=7', 0, (7, 5)),
                                  ('replay: REFUSED invalid\n' + self.summary, 0, (7, 5)),
                                  (self.summary.replace('SAME', 'DIVERGED'), 0, (7, 5))]:
            with self.subTest(log=log, code=code, counts=counts), self.assertRaises(ValueError):
                replay.verdict(log, code, counts)

    def test_every_difference_counter_is_mandatory_and_zero(self):
        for field in replay.DIFFS:
            for bad in (self.summary.replace(field + '=0', field + '=1'),
                        self.summary.replace(field + '=0', '')):
                with self.subTest(field=field), self.assertRaises(ValueError):
                    replay.verdict(bad, 0, (7, 5))

    def test_fixture_discovery_never_silently_skips_a_missing_manifest_or_segment(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            with self.assertRaises(ValueError):
                replay.discover(root)
            fixture = root / 'new-recording'
            fixture.mkdir()
            with self.assertRaisesRegex(ValueError, 'no manifest'):
                replay.discover(root)
            (fixture / 'manifest.json').write_text('{}')
            with self.assertRaisesRegex(ValueError, 'no recording segments'):
                replay.discover(root)
            (fixture / 'rec-0000.jsonl').write_text('')
            with self.assertRaisesRegex(ValueError, 'empty frame/grade'):
                replay.discover(root)
            rows = [{'f': 0, 't': 'tick'}, {'f': 0, 't': 'st', 'hash': 1},
                    {'f': 1, 't': 'tick'}]
            (fixture / 'rec-0000.jsonl').write_text('\n'.join(map(json.dumps, rows)))
            self.assertEqual(replay.discover(root), [fixture])
            self.assertEqual(replay.expected_counts(fixture), (2, 1))


class ParallelRunTests(unittest.TestCase):
    """The scheduler around `run_fixture`, against a fake replay instead of a simulator."""

    def fixtures(self, root, names):
        rows = [{'f': 0, 't': 'tick'}, {'f': 0, 't': 'st', 'hash': 1}]
        made = []
        for index, name in enumerate(names):
            fixture = root / name
            fixture.mkdir()
            (fixture / 'manifest.json').write_text('{}')
            ticks = [{'f': n, 't': 'tick'} for n in range(1, index + 1)]
            (fixture / 'rec-0000.jsonl').write_text('\n'.join(map(json.dumps, rows + ticks)))
            made.append(fixture)
        return made

    def run_all(self, fixtures, jobs, run):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            failures = replay.run_all(Path('sim'), Path('pkg'), fixtures, Path('out'), 5, jobs, run)
        return failures, out.getvalue(), err.getvalue()

    def test_jobs_flag_beats_environment_beats_cpu_default(self):
        self.assertEqual(replay.resolve_jobs(None, {}, 3), 3)
        self.assertEqual(replay.resolve_jobs(None, {}, 64), replay.MAX_DEFAULT_JOBS)
        self.assertEqual(replay.resolve_jobs(None, {}, None), 1)
        self.assertEqual(replay.resolve_jobs(None, {replay.JOBS_ENV: ''}, 8), replay.MAX_DEFAULT_JOBS)
        self.assertEqual(replay.resolve_jobs(None, {replay.JOBS_ENV: '1'}, 8), 1)
        self.assertEqual(replay.resolve_jobs(None, {replay.JOBS_ENV: '12'}, 2), 12)
        self.assertEqual(replay.resolve_jobs(2, {replay.JOBS_ENV: '1'}, 8), 2)
        for flag, env in [(0, {}), (-1, {}), (None, {replay.JOBS_ENV: '0'}),
                          (None, {replay.JOBS_ENV: 'many'})]:
            with self.subTest(flag=flag, env=env), self.assertRaises(ValueError):
                replay.resolve_jobs(flag, env, 4)

    def test_longest_recording_is_scheduled_first_and_every_mode_runs(self):
        with tempfile.TemporaryDirectory() as temp:
            short, middle, long = self.fixtures(Path(temp), ['a-short', 'b-middle', 'c-long'])
            order = replay.replay_order([short, middle, long])
            self.assertEqual(len(order), 6)
            self.assertEqual({mode for _, mode in order}, set(replay.MODES))
            self.assertEqual([fixture for fixture, _ in order[:2]], [long, long])
            self.assertEqual([fixture for fixture, _ in order[-2:]], [short, short])

    def test_a_failing_replay_does_not_stop_the_others_and_fails_the_run(self):
        with tempfile.TemporaryDirectory() as temp:
            fixtures = self.fixtures(Path(temp), ['one', 'two', 'three'])
            ran = []

            def fake(binary, assets, fixture, mode, output, timeout):
                ran.append((fixture.name, mode))
                if (fixture.name, mode) == ('two', 'resolve'):
                    raise ValueError('two/resolve: replay summary is ambiguous or diverged')
                if (fixture.name, mode) == ('three', 'targets'):
                    raise OSError('disk went away')
                if (fixture.name, mode) == ('one', 'targets'):
                    raise RuntimeError('worker bug')
                return f'{fixture.name}/{mode}: replay: done verdict=SAME'

            for jobs in (1, 3):
                ran.clear()
                with self.subTest(jobs=jobs):
                    failures, out, err = self.run_all(fixtures, jobs, fake)
                    self.assertEqual(len(ran), 6)
                    self.assertEqual(len(failures), 3)
                    self.assertEqual(out.count('verdict=SAME'), 3)
                    self.assertEqual(err.count('FAIL: '), 3)
                    self.assertIn('RuntimeError: worker bug', err)

    def test_workers_overlap_up_to_the_bound_and_never_beyond(self):
        with tempfile.TemporaryDirectory() as temp:
            fixtures = self.fixtures(Path(temp), ['a', 'b', 'c', 'd'])
            lock, live, peak = threading.Lock(), [0], [0]

            def fake(binary, assets, fixture, mode, output, timeout):
                with lock:
                    live[0] += 1
                    peak[0] = max(peak[0], live[0])
                time.sleep(0.05)
                with lock:
                    live[0] -= 1
                return f'{fixture.name}/{mode}: ok'

            for jobs, expected in ((1, 1), (3, 3)):
                peak[0] = 0
                with self.subTest(jobs=jobs):
                    failures, _, _ = self.run_all(fixtures, jobs, fake)
                    self.assertEqual((failures, peak[0]), ([], expected))

    def test_each_report_is_one_whole_line(self):
        with tempfile.TemporaryDirectory() as temp:
            fixtures = self.fixtures(Path(temp), ['a', 'b'])
            _, out, _ = self.run_all(fixtures, 4, lambda b, a, f, m, o, t: f'{f.name}/{m}: ok')
            lines = out.splitlines()
            self.assertEqual(len(lines), 4)
            self.assertTrue(all(line.startswith(('a/', 'b/')) and ': ok [' in line for line in lines))

    def test_shards_partition_the_replay_list_and_none_is_empty(self):
        with tempfile.TemporaryDirectory() as temp:
            fixtures = self.fixtures(Path(temp), ['a', 'b', 'c', 'd', 'e'])
            everything = replay.replay_order(fixtures)
            self.assertEqual(len(everything), 10)
            for count in (1, 2, 3, 7, 10):
                with self.subTest(count=count):
                    slices = [replay.select_shard(everything, (index, count))
                              for index in range(1, count + 1)]
                    # Every replay runs exactly once across the shards.
                    self.assertEqual(sorted(sum(slices, []), key=str), sorted(everything, key=str))
                    self.assertEqual(len({replay for each in slices for replay in each}), 10)
                    self.assertLessEqual(max(map(len, slices)) - min(map(len, slices)), 1)
            with self.assertRaisesRegex(ValueError, 'selects no replay'):
                replay.select_shard(everything, (11, 11))

    def test_shards_pair_long_recordings_with_short_ones(self):
        with tempfile.TemporaryDirectory() as temp:
            fixtures = self.fixtures(Path(temp), ['a', 'b', 'c'])  # 1, 2 and 3 ticks over a base of 1
            frames = {fixture: replay.expected_counts(fixture)[0] for fixture in fixtures}
            order = replay.replay_order(fixtures)
            loads = [sum(frames[fixture] for fixture, _ in replay.select_shard(order, (index, 3)))
                     for index in (1, 2, 3)]
            # Six replays of 1, 1, 2, 2, 3, 3 frames over three shards: dealing 1..3 then 3..1 gives
            # 3+1, 3+1 and 2+2; a plain round-robin would give 3+2, 3+2 and 1+1.
            self.assertEqual(sorted(loads), [4, 4, 4])

    def test_the_shard_flag_is_strict(self):
        self.assertEqual(replay.parse_shard('2/3'), (2, 3))
        for bad in ('0/3', '4/3', '1/0', '1', '1/', 'a/b', '-1/3', '1/3/3', ''):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                replay.parse_shard(bad)

    def test_run_all_runs_only_the_selected_replays(self):
        with tempfile.TemporaryDirectory() as temp:
            fixtures = self.fixtures(Path(temp), ['one', 'two', 'three'])
            ran = []

            def fake(binary, assets, fixture, mode, output, timeout):
                ran.append((fixture.name, mode))
                return f'{fixture.name}/{mode}: ok'

            for shard in ((1, 2), (2, 2)):
                chosen = replay.select_shard(replay.replay_order(fixtures), shard)
                out = io.StringIO()
                with contextlib.redirect_stdout(out):
                    replay.run_all(Path('sim'), Path('pkg'), fixtures, Path('out'), 5, 2, fake,
                                   replays=chosen)
            self.assertEqual(len(ran), 6)
            self.assertEqual(len(set(ran)), 6)

    def test_run_fixture_gives_each_replay_its_own_runtime_dir_and_a_timeout(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            fixture = self.fixtures(root, ['solo'])[0]
            (root / 'sim.sh').write_text('#!/bin/sh\nsleep 5\n')
            (root / 'sim.sh').chmod(0o755)
            outputs = root / 'out'
            outputs.mkdir()
            with mock.patch.object(replay.sys, 'platform', 'linux'):
                with self.assertRaisesRegex(ValueError, 'solo/targets: timeout; evidence'):
                    replay.run_fixture(root / 'sim.sh', root, fixture, 'targets', outputs, 0.2)
                with self.assertRaisesRegex(ValueError, 'solo/resolve: timeout; evidence'):
                    replay.run_fixture(root / 'sim.sh', root, fixture, 'resolve', outputs, 0.2)
            runtimes = sorted(path.name.rsplit('-', 1)[0] for path in outputs.iterdir())
            self.assertEqual(runtimes, ['solo-resolve', 'solo-targets'])
            for path in outputs.iterdir():
                self.assertIn(path.name.split('-')[1], (path / 'plxnative-recplay').read_text())


class WorkflowShardTests(unittest.TestCase):
    """Simulator CI splits the gate across runners; a shard left out of its matrix would silently
    stop running, so the workflow and the script's partition are held together here."""

    def test_the_macos_job_runs_every_shard_exactly_once(self):
        workflow = (Path(__file__).resolve().parents[1] / '.github/workflows/simulators.yml').read_text()
        job = workflow[workflow.index('\n  macos:\n'):workflow.index('\n  macos-host-tests:\n')]
        shards = re.search(r'^\s+shard: \[([0-9, ]+)\]$', job, re.M)
        count = re.search(r'^\s+REPLAY_SHARDS: (\d+)$', job, re.M)
        self.assertTrue(shards and count, 'the macos job lost its shard matrix or REPLAY_SHARDS')
        listed = [int(n) for n in shards[1].split(',')]
        self.assertEqual(listed, list(range(1, int(count[1]) + 1)))
        self.assertIn('--shard "${{ matrix.shard }}/$REPLAY_SHARDS"', job)
        self.assertNotIn('fail-fast: true', job)


if __name__ == '__main__':
    unittest.main()
