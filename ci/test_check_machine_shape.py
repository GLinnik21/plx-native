#!/usr/bin/env python3
"""ci/check-machine-shape.py's verdicts on synthetic trees: a public field of a listed machine fails
(`pub`, `pub(crate)`, `pub(super)`, `pub(in ..)`, a tuple field), a stale entry fails, the `bool` and
`Option<..>` counts are reported and never fail, and the real registry is green and wired into
check-python-steps.txt. Each case writes a tiny tree to a private temp dir."""
import importlib.util
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
_spec = importlib.util.spec_from_file_location('check_machine_shape', ROOT / 'ci' / 'check-machine-shape.py')
gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gate)

FILE = 'media/src/player/lifecycle.rs'
INI = f'[{FILE}::Machine]\nevent = Ev\neffect = Fx\nstep = inherent\n'


def verdict(source, ini=INI, file=FILE):
    """(problems, report) for a one-file tree holding `source` at `file`."""
    with tempfile.TemporaryDirectory() as temp:
        modules = Path(temp) / 'rust-modules'
        (modules / file).parent.mkdir(parents=True)
        (modules / file).write_text(source)
        registry = Path(temp) / 'machines.ini'
        registry.write_text(ini)
        return gate.check(modules=modules, ini=registry)


class Visibility(unittest.TestCase):
    def test_private_fields_pass(self):
        problems, report = verdict('pub struct Machine { state: u8, next_id: u64 }')
        self.assertEqual(problems, [])
        self.assertEqual(len(report), 1)

    def test_every_pub_spelling_of_a_field_fails(self):
        for visibility in ('pub', 'pub(crate)', 'pub(super)', 'pub(in crate::player)'):
            with self.subTest(visibility=visibility):
                problems, _ = verdict(f'pub struct Machine {{ state: u8, {visibility} next_id: u64 }}')
                self.assertEqual(len(problems), 1, problems)
                self.assertIn('Machine.next_id is public', problems[0])

    def test_a_pub_field_behind_an_attribute_or_doc_comment_fails(self):
        problems, _ = verdict('pub struct Machine {\n    /// the id\n    #[allow(dead_code)]\n    pub next_id: u64,\n}')
        self.assertEqual(len(problems), 1, problems)

    def test_a_tuple_field_is_graded_by_index(self):
        problems, _ = verdict('pub struct Machine(u8, pub u64);')
        self.assertEqual(len(problems), 1, problems)
        self.assertIn('Machine.1 is public', problems[0])

    def test_pub_in_a_comment_or_a_string_is_no_field(self):
        problems, _ = verdict('pub struct Machine {\n    // pub nope: u8,\n    state: &\'static str, /* pub x: u8 */\n}\n'
                              'const S: &str = "pub struct Machine { pub a: u8 }";')
        self.assertEqual(problems, [])

    def test_only_the_listed_struct_is_graded(self):
        problems, _ = verdict('pub struct MachineLog { pub x: u8 }\npub struct Machine { x: u8 }\n'
                              'pub struct Other { pub y: u8 }')
        self.assertEqual(problems, [])

    def test_generics_with_a_default_and_a_where_clause_do_not_hide_the_body(self):
        problems, report = verdict(
            'pub struct Machine<A = crate::route::Attempt>\nwhere\n    A: Copy,\n{\n    state: State<A>,\n    pub next_id: u64,\n}')
        self.assertEqual(len(problems), 1, problems)
        self.assertIn('Machine.next_id', problems[0])
        self.assertEqual(len(report), 1)  # a machine that fails still reports its counts


class Counts(unittest.TestCase):
    def test_bool_and_option_fields_are_counted_and_never_fail(self):
        problems, report = verdict(
            'pub struct Machine {\n    a: bool,\n    b: bool,\n    c: Option<u8>,\n    d: Option<Vec<(u8, bool)>>,\n'
            '    e: Vec<bool>,\n    f: fn(u8) -> bool,\n    g: HashMap<u8, Option<u8>>,\n}')
        self.assertEqual(problems, [])
        self.assertEqual(report, ['machine media/src/player/lifecycle.rs::Machine (Ev -> Fx): 7 fields, 2 bool, 2 Option'])

    def test_a_unit_struct_has_no_fields(self):
        problems, report = verdict('pub struct Machine;')
        self.assertEqual(problems, [])
        self.assertIn('0 fields, 0 bool, 0 Option', report[0])


class Registry(unittest.TestCase):
    def test_a_missing_file_is_a_stale_entry(self):
        problems, _ = verdict('pub struct Machine { x: u8 }', file='media/src/player/elsewhere.rs')
        self.assertEqual(len(problems), 1, problems)
        self.assertIn('stale entry', problems[0])

    def test_a_missing_struct_is_a_stale_entry(self):
        problems, _ = verdict('pub struct Renamed { x: u8 }')
        self.assertEqual(len(problems), 1, problems)
        self.assertIn('declares no struct Machine', problems[0])

    def test_a_malformed_registry_fails(self):
        problems, _ = verdict('pub struct Machine { x: u8 }', ini='[unclosed\n')
        self.assertEqual(len(problems), 1, problems)
        self.assertIn('does not parse', problems[0])


class Repository(unittest.TestCase):
    def test_the_real_registry_is_green_and_lists_the_foreground_machine(self):
        problems, report = gate.check()
        self.assertEqual(problems, [])
        self.assertTrue(any('lifecycle.rs::ForegroundLifecycle' in line for line in report), report)

    def test_the_gate_and_its_test_are_wired_into_check_python_steps(self):
        steps = (ROOT / 'ci' / 'check-python-steps.txt').read_text().splitlines()
        for line in ('python3 ci/test_check_machine_shape.py', 'python3 ci/check-machine-shape.py'):
            self.assertTrue(line in steps, f'{line!r} is not a line of ci/check-python-steps.txt')


if __name__ == '__main__':
    unittest.main()
