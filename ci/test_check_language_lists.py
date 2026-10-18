#!/usr/bin/env python3
"""ci/check-language-lists.py's verdicts on synthetic trees, plus the real tree and its wiring."""
import importlib.util
import tempfile
import unittest
from pathlib import Path

_spec = importlib.util.spec_from_file_location('check_language_lists', Path(__file__).with_name('check-language-lists.py'))
gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gate)

REGISTRY = '[{"tag":"en"},{"tag":"es"},{"tag":"it"}]'


def tree(source, allow=()):
    with tempfile.TemporaryDirectory(prefix='check-language-lists-') as tmp:
        root = Path(tmp)
        (root / 'locales').mkdir()
        (root / 'locales' / 'languages.json').write_text(REGISTRY)
        (root / 'rust-modules').mkdir()
        (root / 'rust-modules' / 'a.rs').write_text(source)
        (root / 'ci' / 'allow').mkdir(parents=True)
        (root / 'ci' / 'allow' / 'language-lists.txt').write_text(
            f'# count: {len(allow)}\n' + ''.join(f'{a}\treason\n' for a in allow))
        return gate.problems(root)


class Verdicts(unittest.TestCase):
    def test_a_multiline_list_is_found(self):
        source = 'fn t() {\n    for l in [\n        Preference::En,\n        Preference::Es,\n    ] {}\n}\n'
        self.assertEqual(len(tree(source)), 1)

    def test_tuples_and_qualified_paths_count(self):
        self.assertEqual(len(tree('const X: [(plx_platform::i18n::Language::En, &str); 0] = [(Language::En, "a"), (Language::It, "b")];')), 1)

    def test_one_language_a_registry_slice_and_prose_are_fine(self):
        self.assertEqual(tree('let a = [Preference::En]; let b = SHIPPED; // [Preference::En, Preference::Es]\n'), [])
        self.assertEqual(tree('let s = "[Preference::En, Preference::Es]";'), [])

    def test_unknown_variants_are_not_languages(self):
        self.assertEqual(tree('let a = [Preference::En, Preference::System, Language::Pseudo];'), [])

    def test_the_allow_list_is_exact(self):
        source = 'let a = [Preference::En, Preference::Es];'
        self.assertEqual(tree(source, ['rust-modules/a.rs']), [])
        self.assertTrue(any('stale' in p for p in tree('let a = 1;', ['rust-modules/a.rs'])))

    def test_the_real_tree_is_green(self):
        self.assertEqual(gate.problems(gate.REPO), [])

    def test_the_gate_is_a_step_of_make_check(self):
        steps = (gate.REPO / 'ci' / 'check-python-steps.txt').read_text()
        self.assertIn('ci/check-language-lists.py', steps)


if __name__ == '__main__':
    unittest.main()
