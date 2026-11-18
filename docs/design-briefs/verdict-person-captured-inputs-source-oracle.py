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
        for name in ('person_run_from', 'person_pump_with_seed_read'):
            body = text.split('fn ' + name + '(', 1)[1].split('\n    }', 1)[0]
            self.assertEqual(body.count('self.capture_store_session()'), 1)
            self.assertLess(body.index('self.capture_store_session()'), body.index('refill_client_keys'))
            self.assertNotIn('.clone()', body)
            self.assertNotIn('.collect', body)
        delivery = text.split('fn deliver_person_open_with_seed_read(', 1)[1].split('\n    }', 1)[0]
        self.assertIn('self.pending_person_open.take()', delivery)
        self.assertLess(delivery.index('pending.scope != owner.scope.0'), delivery.index('self.stores.person_run'))
        self.assertLess(delivery.index('pending.tenure != capture.tenure'), delivery.index('self.person_capture.refresh'))
        self.assertIn('self.deliver_person_open_with_seed_read(read)', body)
        self.assertLess(body.index('if let Some(person) = self.stores.person_view().current()'), body.index('self.person_capture.refresh'))
        self.assertIn('self.person_pump_with_seed_read(&mut plx_base::devtrig::read)', text)
        self.assertIn('self.deliver_person_open_with_seed_read(&mut plx_base::devtrig::read)', text)
        self.assertIn('AppMsg::PersonCommand { from, command }', text)
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
    def test_explicit_scope_precedes_controlled_shortcut(self):
        text = (ROOT / 'rust-modules/data/src/person.rs').read_text(encoding='utf-8')
        fields = text.split('pub struct PersonInput', 1)[1].split('pub struct PersonState', 1)[0]
        for field in ('profile_scope', 'owner_epoch', 'profile_generation', 'profile_key', 'tenure'):
            self.assertIn(field, fields)
        body = text.split('fn observe_session(', 1)[1].split('fn current(', 1)[0]
        self.assertLess(body.index('scope_changed'), body.index('input.controlled'))
    def test_requested_behavior_regressions_are_present(self):
        text = (ROOT / 'rust-modules/data/src/person/captured_inputs_tests.rs').read_text(encoding='utf-8')
        self.assertEqual(text.count('#[test]'), 12)
        self.assertIn('Arc::ptr_eq(&s, &session)', text)
        self.assertIn('an orphan cannot release', text)
        self.assertIn('assert!(state.pump(&adapter, &facts))', text)
        boundary = (ROOT / 'rust-modules/data/src/person/boundary_tests.rs').read_text(encoding='utf-8')
        self.assertEqual(boundary.count('#[test]'), 5)
        self.assertIn('explicit_scope_edges_retire_claims_and_evidence_before_any_landing_even_controlled', boundary)
        self.assertIn('same_generation_retry_rejects_old_refusal_success_address_and_key', boundary)
    def test_refusal_is_addressed_and_observed_in_runner_drain(self):
        text = (ROOT / 'rust-modules/data/src/person.rs').read_text(encoding='utf-8')
        body = text.split('fn refused(&self, addr: Addr)', 1)[1].split('impl PersonState', 1)[0]
        self.assertIn('self.matches(addr, pending)', body)
        self.assertIn('PersonLaunchRefused { addr, request:', body)
        self.assertNotIn('release_for', body)
        self.assertIn('addr == self.addr && addr == pending.addr && *self.request == *pending.request', text)
        bridge = (ROOT / 'rust-modules/src/app/bridge.rs').read_text(encoding='utf-8')
        drain = bridge.split('fn start_jobs(', 1)[1].split('fn start_late_store_jobs', 1)[0]
        self.assertIn('self.stores.person.land_launch_refusals()', drain)
    def test_session_capture_commits_only_after_closing_publication_fence(self):
        text = (ROOT / 'rust-modules/src/app/bridge.rs').read_text(encoding='utf-8')
        body = text.split('fn refill_store_session(', 1)[1].split('fn search_input', 1)[0]
        self.assertLess(body.index('if revision() != before'), body.index('self.session = settled'))
        self.assertNotIn('Arc::new', body)
        self.assertNotIn('collect', body)
if __name__ == '__main__':
    unittest.main()
