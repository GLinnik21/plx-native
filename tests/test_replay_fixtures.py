#!/usr/bin/env python3
"""Host checks for the strict result/accounting boundary of the real simulator replay gate."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

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


if __name__ == '__main__':
    unittest.main()
