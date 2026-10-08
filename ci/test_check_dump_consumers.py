#!/usr/bin/env python3
"""ci/check-dump-consumers.py's verdicts on synthetic trees, plus the real tree (read-only) and its wiring.

Each synthetic case is a private temp tree with a `rust-modules/` of Rust files and a
`ci/allow/dump.txt`. The real-tree cases read the repository: the list is exact, and the gate is a
step of `make check` (dropping the line from ci/check-python-steps.txt would disable it silently)."""
import importlib.util
import tempfile
import unittest
from pathlib import Path

_spec = importlib.util.spec_from_file_location('check_dump_consumers', Path(__file__).with_name('check-dump-consumers.py'))
gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gate)

USES = 'pub fn f() -> bool {\n    plx_gfx::dump::armed()\n}\n'


def tree(files, allow):
    """Write `files` ({path under rust-modules: source}) and an allow-list of `allow` paths; return problems."""
    with tempfile.TemporaryDirectory(prefix='check-dump-consumers-') as tmp:
        for name, source in files.items():
            path = Path(tmp) / 'rust-modules' / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(source)
        allow_path = Path(tmp) / 'ci' / 'allow' / 'dump.txt'
        allow_path.parent.mkdir(parents=True)
        allow_path.write_text(f'# count: {len(allow)}\n# header\n' + ''.join(f'rust-modules/{a}\treason\n' for a in allow))
        return gate.problems(tmp)


def raw_allow(allow_text, source=USES):
    """Problems for a tree whose only Rust file is `a.rs` and whose allow-list is exactly `allow_text`."""
    with tempfile.TemporaryDirectory(prefix='check-dump-consumers-') as tmp:
        (Path(tmp) / 'rust-modules').mkdir()
        (Path(tmp) / 'rust-modules' / 'a.rs').write_text(source)
        allow = Path(tmp) / 'ci' / 'allow' / 'dump.txt'
        allow.parent.mkdir(parents=True)
        allow.write_text(allow_text)
        return gate.problems(tmp)


class NewConsumers(unittest.TestCase):
    def test_a_listed_file_passes(self):
        self.assertEqual(tree({'ui/src/a.rs': USES}, ['ui/src/a.rs']), [])

    def test_a_new_file_naming_armed_fails_and_points_at_the_rule(self):
        out = tree({'ui/src/a.rs': USES, 'ui/src/b.rs': USES}, ['ui/src/a.rs'])
        self.assertEqual(len(out), 1, out)
        self.assertIn('ui/src/b.rs', out[0])
        self.assertIn('never skip a state', out[0])
        self.assertIn('dump.rs', out[0])

    def test_every_name_of_the_switch_is_watched(self):
        for line in ('plx_gfx::dump::armed()', 'plx_gfx::dump::held_repeat()', 'plx_gfx::dump::set_held_repeat(true)',
                     'plx_gfx::dump::arm()', 'plx_gfx::dump::disarm()', 'let _g = plx_gfx::dump::Armed::new();',
                     'let _r = plx_gfx::dump::HeldRepeat::new();', 'use plx_gfx::dump::{armed, Armed};',
                     'use plx_gfx::dump::*;', 'use plx_gfx::dump as d;', 'crate::dump::armed()'):
            with self.subTest(line=line):
                out = tree({'ui/src/x.rs': f'fn f() {{ {line} }}\n'}, [])
                self.assertEqual(len(out), 1, out)

    def test_a_test_in_an_unlisted_file_that_arms_the_switch_fails_too(self):
        src = '#[cfg(test)]\nmod t {\n    #[test]\n    fn a() { let _g = plx_gfx::dump::Armed::new(); }\n}\n'
        self.assertEqual(len(tree({'ui/src/t.rs': src}, [])), 1)

    def test_comments_strings_and_a_similar_name_are_not_uses(self):
        src = ('// plx_gfx::dump::armed() is read here\n/// [`plx_gfx::dump::held_repeat`]\n/* dump::arm() */\n'
               'fn f() { let s = "dump::armed()"; let _ = placeholder::armed(); glassload::armed(); '
               'dump::armed_never(); }\n')
        self.assertEqual(tree({'ui/src/c.rs': src}, []), [])

    def test_the_module_that_defines_the_switch_is_not_a_use_of_it(self):
        src = 'pub fn armed() -> bool { false }\npub fn arm() {}\n'
        self.assertEqual(tree({'gfx/src/dump.rs': src}, []), [])


class Ledger(unittest.TestCase):
    def test_a_stale_entry_fails(self):
        out = tree({'ui/src/a.rs': 'fn f() {}\n'}, ['ui/src/a.rs'])
        self.assertEqual(len(out), 1, out)
        self.assertIn('stale', out[0])

    def test_a_count_that_disagrees_fails(self):
        out = raw_allow('# count: 5\nrust-modules/a.rs\treason\n')
        self.assertEqual(len(out), 1, out)
        self.assertIn('count', out[0])

    def test_a_duplicate_entry_fails(self):
        out = raw_allow('# count: 2\nrust-modules/a.rs\tr\nrust-modules/a.rs\tr\n')
        self.assertTrue(any('twice' in p for p in out), out)


class RealTree(unittest.TestCase):
    def test_the_real_tree_is_green(self):
        self.assertEqual(gate.problems(gate.REPO), [])

    def test_the_gate_is_run_by_make_check(self):
        steps = (gate.REPO / 'ci' / 'check-python-steps.txt').read_text().split('\n')
        self.assertIn('python3 ci/check-dump-consumers.py', [l.strip() for l in steps],
                      'dropping the gate from the check steps disables it silently')

    def test_every_consumer_is_named_in_the_admission_rule_doc(self):
        """The consumer list in dump.rs is prose, but every FILE the gate allows must be named in it."""
        doc = (gate.REPO / 'rust-modules' / 'gfx' / 'src' / 'dump.rs').read_text()
        head = doc[:doc.index('#[cfg(any(feature = "hostsim"')]
        self.assertIn('The admission rule', head)
        entries, _ = gate.allowed(gate.REPO)
        for rel in entries:
            # the doc names a file by its path inside its crate (`ui/src/xfade.rs`)
            short = rel.replace('rust-modules/', '', 1)
            self.assertIn(short, head, f'{rel} may name the dump switch but dump.rs does not list it as a consumer')


if __name__ == '__main__':
    unittest.main()
