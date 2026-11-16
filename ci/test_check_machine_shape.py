#!/usr/bin/env python3
"""ci/check-machine-shape.py's verdicts on synthetic trees: a public field of a listed machine fails
(`pub`, `pub(crate)`, `pub(super)`, `pub(in ..)`, a tuple field), a stale entry fails, the `bool` and
`Option<..>` counts are reported and never fail, and the real registry is green and wired into
check-python-steps.txt. Each case writes a tiny tree to a private temp dir."""
import configparser
import importlib.util
import shutil
import io
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
_spec = importlib.util.spec_from_file_location('check_machine_shape', ROOT / 'ci' / 'check-machine-shape.py')
gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gate)

FILE = 'media/src/player/lifecycle.rs'
INI = f'[{FILE}::Machine]\nevent = Ev\neffect = Fx\nstep = inherent\n'


def verdict(source, ini=None, file=FILE):
    """(problems, report) for a one-file tree holding `source` at `file`."""
    if ini is None:
        fields = gate.struct_fields(source, 'Machine') or []
        field = fields[0][1] if fields else 'state'
        ini = INI + f'writer_policy = strict\nstate_field = {field}\ntransition = transition\nconstructor = new\n\n[module {FILE}]\nwildcard_enum_match_arm = deny\n'
        source = '#![deny(clippy::wildcard_enum_match_arm)]\n' + source
        initial = ', '.join(f'{key}: 0' for _, key, _ in fields)
        source += f'\nenum Ev {{ Set }} enum Fx {{ Changed }} impl Machine {{ fn new() -> Self {{ Self {{ {initial} }} }} fn transition(&mut self, event: Ev, fx: &mut Vec<Fx>) {{ match event {{ Ev::Set => {{ self.{field} = 1; fx.push(Fx::Changed); }} }} }} }}'
    with tempfile.TemporaryDirectory() as temp:
        modules = Path(temp) / 'rust-modules'
        (modules / file).parent.mkdir(parents=True)
        (modules / file).write_text(source)
        registry = Path(temp) / 'machines.ini'
        registry.write_text(ini)
        problems, report = gate.check(modules=modules, ini=registry)
        return problems, [line for line in report if line.startswith('machine ')]


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
        self.assertTrue(problems)
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
        self.assertTrue(problems)
        self.assertIn('0 fields, 0 bool, 0 Option', report[0])


class Registry(unittest.TestCase):
    def test_a_missing_file_is_a_stale_entry(self):
        problems, _ = verdict('pub struct Machine { x: u8 }', file='media/src/player/elsewhere.rs')
        self.assertTrue(problems)
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


class MandatoryCohort(unittest.TestCase):
    def test_future_machine_cannot_opt_out(self):
        source = 'struct Machine { state: u8 } impl Machine { fn rogue(&mut self) { self.state = 2; } }'
        problems, _ = verdict(source, ini=INI)
        self.assertTrue(problems, 'a newly registered private machine must not opt out')

    def test_every_actual_entry_mutation_matrix(self):
        registry = configparser.ConfigParser(interpolation=None)
        registry.read(ROOT / 'ci/machines.ini')
        entries = [s for s in registry.sections() if not s.startswith('module ')]
        self.assertTrue(entries, 'registry is the only normative cohort list')
        with tempfile.TemporaryDirectory() as temp:
            modules = Path(temp) / 'rust-modules'
            for crate in (ROOT / 'rust-modules').iterdir():
                if not crate.is_dir() or not (crate / 'Cargo.toml').is_file(): continue
                destination = modules / crate.name
                destination.mkdir(parents=True)
                shutil.copyfile(crate / 'Cargo.toml', destination / 'Cargo.toml')
                if (crate / 'src').is_dir():
                    shutil.copytree(crate / 'src', destination / 'src')
            # The dependency proof checks the exact locked workspace, even in mutation fixtures.
            for relative in ('Cargo.toml', 'Cargo.lock', '.cargo/config', '.cargo/config.toml'):
                source = ROOT / 'rust-modules' / relative
                if source.is_file():
                    destination = modules / relative
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(source, destination)
            # Source-backed generated layouts require the actual producers and dynamic input.
            for relative in ('platform/build.rs', 'platform/build_support/catalog.rs', 'build_support/install_identities.rs', '../locales/languages.json'):
                destination = modules / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / 'rust-modules' / relative, destination)
            ini = Path(temp) / 'machines.ini'
            for section in entries:
                file, name = section.split('::')
                policy = 'module ' + file
                path = modules / file
                original = path.read_text(encoding='utf-8')
                contract = dict(registry[section])
                field = contract.get('state_field', 'state')
                def check(entry=contract, module=True, source=original):
                    config = configparser.ConfigParser(interpolation=None)
                    config[section] = entry
                    if module:
                        config[policy] = dict(registry[policy]) if policy in registry else {'wildcard_enum_match_arm': 'deny'}
                    out = io.StringIO()
                    config.write(out)
                    ini.write_text(out.getvalue(), encoding='utf-8')
                    path.write_text(source, encoding='utf-8')
                    return gate.check(modules, ini)[0]
                mutations = {
                    'all-writer-keys': ({k: v for k, v in contract.items() if k not in ('writer_policy', 'state_field', 'transition', 'constructor')}, True, original),
                    'enum-section': (contract, False, original),
                    'enum-header': (contract, True, original.replace('#![deny(clippy::wildcard_enum_match_arm)]', '').replace('#![forbid(clippy::wildcard_enum_match_arm)]', '')),
                    'rogue-writer': (contract, True, original + f'\nimpl {name} {{ fn contract_rogue(&mut self) {{ self.{field} = panic!("rogue"); }} }}\n'),
                    'state-escape': (contract, True, original + f'\nimpl {name} {{ fn contract_escape(&mut self) {{ let escape = &mut self.{field}; }} }}\n'),
                    'suppression': (contract, True, '#![allow(clippy::wildcard_enum_match_arm)]\n' + original),
                }
                for level in ('expect', 'warn'):
                    mutations['suppression-' + level] = (contract, True, f'#![{level}(clippy::wildcard_enum_match_arm)]\n' + original)
                for key in ('writer_policy', 'state_field', 'transition', 'constructor'):
                    mutations['missing-' + key] = ({k: v for k, v in contract.items() if k != key}, True, original)
                for mutation, args in mutations.items():
                    with self.subTest(machine=section, mutation=mutation):
                        self.assertTrue(check(*args), f'{section}: {mutation} escaped enforcement')
                with self.subTest(machine=section, mutation='baseline'):
                    self.assertEqual(check(), [])
                path.write_text(original, encoding='utf-8')
                conditional = original + '#[cfg_attr(feature = "shipping", path = "alternate.rs")] mod contract_child;'
                with self.subTest(machine=section, mutation='conditional-production-child'):
                    self.assertTrue(check(source=conditional))
                path.write_text(original, encoding='utf-8')



class CompleteOwnedCohort(unittest.TestCase):
    def test_every_actual_owned_field_extra_writer_matrix(self):
        from machine_readonly import Proof
        registry = configparser.ConfigParser(interpolation=None)
        registry.read(ROOT / 'ci/machines.ini')
        entries = [s for s in registry.sections() if not s.startswith('module ')]
        self.assertTrue(entries)
        total_fields = total_mutations = 0
        for section in entries:
            file, name = section.split('::')
            contract = registry[section]
            root = ROOT / 'rust-modules'
            original = (root / file).read_text(encoding='utf-8')
            proof = Proof(root, gate)
            fields = gate.struct_fields(gate.production_tokens(original), name) or []
            self.assertTrue(fields)
            bodies = (
                'self.{field} = panic!("rogue");',
                'self.{field} += panic!("rogue");',
                'let alias = &mut self.{field}; unknown(alias);',
                'self.{field}.clear();',
                'unknown!(self.{field});',
                'std::mem::take(&mut self.{field});',
                'let ptr = core::ptr::addr_of_mut!(self.{field});',
            )
            for _, owned_field, actual_type in fields:
                self.assertTrue(actual_type, (section, owned_field))
                total_fields += 1
                for body in bodies:
                    mutation = body.replace('{field}', owned_field)
                    child = f'impl {name} {{ fn contract_field_rogue(&mut self) {{ {mutation} }} }}'
                    with self.subTest(machine=section, owned_field=owned_field, mutation=mutation):
                        self.assertTrue(gate.writer_problems(child, name, contract['state_field'],
                            contract['transition'], contract['constructor'], file,
                            require_methods=False, event=contract['event'], effect=contract['effect'],
                            proof=proof, shared_reads=contract.get('state_reads') == 'immutable',
                            owned_fields=fields), f'{section}.{owned_field}: extra writer escaped')
                    total_mutations += 1
        print(f'owned-field matrix: {len(entries)} machines, {total_fields} fields, {total_mutations} rejected writers')


if __name__ == '__main__':
    unittest.main()
