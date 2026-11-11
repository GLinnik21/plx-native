"""Person machine boundary source oracle; no Rust execution or shared target lease."""
from pathlib import Path
import re
import unittest
ROOT = Path(__file__).resolve().parents[2]
class PersonCapturedInputs(unittest.TestCase):
    def test_machine_has_no_ambient_inputs(self):
        text = (ROOT / 'rust-modules/data/src/person.rs').read_text(encoding='utf-8').split('#[cfg(test)]\nmod tests')[0]
        for spelling in ('server_ids(', 'server_roster_gen(', 'session::peek', 'session_watch', 'tape::active(', 'tape::credits(', 'devtrig::read(', 'discover::person_profile_refused(', 'discover::person_credits_refused('):
            with self.subTest(spelling=spelling):
                self.assertFalse(spelling in text, spelling)
    def test_roster_uses_registered_slots_not_only_active_keys(self):
        text = (ROOT / 'rust-modules/data/src/person.rs').read_text(encoding='utf-8')
        self.assertTrue('keys.slots().chain(std::iter::once(origin))' in text)
        self.assertTrue('input.keys.roster_gen()' in text)
    def test_inputless_machine_pump_is_refused(self):
        text = (ROOT / 'rust-modules/data/src/stores/person.rs').read_text(encoding='utf-8')
        self.assertFalse('&ClientKeys::default()' in text)
        self.assertTrue('Person pump requires captured inputs' in text)
    def test_host_capture_precedes_open_and_pump(self):
        text = (ROOT / 'rust-modules/src/app/bridge.rs').read_text(encoding='utf-8')
        for name in ('person_run', 'person_pump'):
            body = text.split('fn ' + name + '(', 1)[1].split('\n    }', 1)[0]
            self.assertEqual(body.count('self.capture_store_session()'), 1)
            self.assertLess(body.index('self.capture_store_session()'), body.index('refill_client_keys'))
            self.assertNotIn('.clone()', body)
            self.assertNotIn('.collect', body)
    def test_stable_roster_sync_does_not_copy_page_or_roster(self):
        text = (ROOT / 'rust-modules/data/src/person.rs').read_text(encoding='utf-8')
        body = text.split('fn sync_roster(', 1)[1].split('fn src_of(', 1)[0]
        self.assertNotIn('.clone()', body)
        self.assertIn('old.iter().copied().eq(', body)
        self.assertIn('input.keys.slots()', body)
    def test_person_ledger_is_empty_and_global_pin_matches_actual_entries(self):
        import ast
        ledger = ROOT / 'ci/allow/globals/data-person.txt'
        self.assertEqual([s for s in ledger.read_text(encoding='utf-8').splitlines() if s.strip() and not s.startswith('#')], [])
        total = sum(int(p.read_text(encoding='utf-8').splitlines()[0].split(':')[1]) for p in ledger.parent.glob('*.txt'))
        tree = ast.parse((ROOT / 'tests/test_harness.py').read_text(encoding='utf-8'))
        pins = [ast.literal_eval(n.value) for n in ast.walk(tree) if isinstance(n, ast.Assign) and any(isinstance(t, ast.Name) and t.id == 'PINNED_LEDGER_TOTALS' for t in n.targets)]
        self.assertEqual(len(pins), 1)
        self.assertEqual(pins[0]['globals'], total)
    def test_requested_behavior_regressions_are_present(self):
        text = (ROOT / 'rust-modules/data/src/person/captured_inputs_tests.rs').read_text(encoding='utf-8')
        self.assertEqual(text.count('#[test]'), 10)
        self.assertIn('Arc::ptr_eq(&s, &session)', text)
        self.assertIn('an orphan cannot release', text)
        self.assertIn('assert!(state.pump(&adapter, &facts))', text)
if __name__ == '__main__':
    unittest.main()
