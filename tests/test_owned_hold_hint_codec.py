"""Isolated source controls; runtime assertions live beside the Rust codec."""
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]

class OwnedHoldHintCodec(unittest.TestCase):
    def test_new_shape_and_no_historical_boot_fallback(self):
        recorder = (ROOT / 'rust-modules/src/app/recorder.rs').read_text()
        bootstrap = (ROOT / 'rust-modules/src/app/bootstrap.rs').read_text()
        self.assertIn('ControlledHomeInitV13', recorder)
        self.assertIn('HoldHintEffectsV1{Learned|ReadCard|ReadPill}', recorder)
        self.assertNotIn('.or_else(|_| Recording::load', bootstrap)
        self.assertIn('incompatible owner-input/presentation state; requires V13 capture', bootstrap)

    def test_closed_typed_effect_and_runtime_oracles(self):
        effects = (ROOT / 'rust-modules/src/app/bootstrap/effects.rs').read_text()
        recorder = (ROOT / 'rust-modules/src/app/recorder_hold_hint_tests.rs').read_text()
        self.assertIn('Fx::HoldHint(event)', effects)
        for event in ['Learned', 'ReadCard', 'ReadPill']:
            self.assertIn('HoldHintEvent::' + event, effects)
        self.assertIn('validate_hold_hint', effects)
        self.assertIn('hold_hint_two_owners_replay_identical_prefix', recorder)
        self.assertIn('hold_hint_preflight_rejects_structural_mutations', recorder)

if __name__ == '__main__':
    unittest.main()
