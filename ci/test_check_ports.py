#!/usr/bin/env python3
"""Wall scope and exact-file ports: negative controls cannot exempt machine logic."""
import importlib.util
import tempfile
import unittest
from pathlib import Path
spec = importlib.util.spec_from_file_location('check_ports', Path(__file__).with_name('check-ports.py'))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)
caps_spec = importlib.util.spec_from_file_location('check_list_caps', Path(__file__).with_name('check-list-caps.py'))
caps = importlib.util.module_from_spec(caps_spec)
caps_spec.loader.exec_module(caps)
UNTERMINATED = 'const S: &str = r#"never closed;\n'
class Ports(unittest.TestCase):
    def fixture(self, name='rust-modules/data/src/job_ports.rs', source='fn run() { Instant::now(); }', ini=None):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        path = root / name
        path.parent.mkdir(parents=True)
        path.write_text(source, encoding='utf-8')
        (root / 'ci').mkdir()
        (root / 'ci/ports.ini').write_text(ini if ini is not None else f'[{name}]\nkind = worker\nreason = Captured job transport\n', encoding='utf-8')
        return root
    def test_exact_job_file_is_exempt_but_neighbor_is_not(self):
        root = self.fixture()
        other = root / 'rust-modules/data/src/store.rs'
        other.write_text('fn run() { SystemTime :: now(); }')
        self.assertEqual(gate.check(root), ['rust-modules/data/src/store.rs: wall clock outside ci/ports.ini'])
    def test_all_data_and_session_subdirectories_are_covered(self):
        for area in ('data', 'session'):
            root = self.fixture(name=f'rust-modules/{area}/src/nested/logic.rs', ini='')
            self.assertTrue(gate.check(root))
    def test_test_items_and_comments_are_not_clock_reads(self):
        root = self.fixture(ini='', source='// Instant::now()\nconst S: &str = "SystemTime::now()";\n#[cfg(test)] mod tests { fn f() { Instant::now(); } }')
        self.assertEqual(gate.check(root), [])
    def test_external_test_modules_are_classified_not_named(self):
        root = self.fixture(name='rust-modules/session/src/lib.rs', ini='', source='#[cfg(test)] mod fixture;')
        (root / 'rust-modules/session/src/fixture.rs').write_text('fn test() { Instant::now(); }')
        self.assertEqual(gate.check(root), [])
    def test_spacing_and_noncall_clock_references_are_reads(self):
        for source in ('fn f() { let now = Instant :: now; }', 'fn f() { i . elapsed ( ); }'):
            root = self.fixture(ini='', source=source)
            self.assertTrue(gate.check(root))
    def test_unlexable_file_with_a_wall_word_fails_unless_it_is_wholly_test(self):
        root = self.fixture(ini='', source='fn run() { Instant::now(); }\n' + UNTERMINATED)
        with self.assertRaisesRegex(ValueError, 'unterminated Rust raw string'): gate.check(root)
        root = self.fixture(name='rust-modules/session/src/lib.rs', ini='', source='#[cfg(test)] mod fixture;')
        (root / 'rust-modules/session/src/fixture.rs').write_text('fn test() { Instant::now(); }\n' + UNTERMINATED)
        self.assertEqual(gate.check(root), [])
    def test_unlexable_file_without_a_wall_word_is_left_to_the_list_caps_gate(self):
        root = self.fixture(ini='', source='fn run() {}\n' + UNTERMINATED)
        self.assertEqual(gate.check(root), [])
        with self.assertRaisesRegex(ValueError, 'unterminated Rust raw string'): caps.read_hits(root / 'rust-modules')
    def test_unbalanced_token_group_fails_the_wall_rule_only_when_a_wall_site_needs_classifying(self):
        root = self.fixture(name='rust-modules/data/src/lib.rs', ini='', source='mod other; fn f() { ( }')
        other = root / 'rust-modules/data/src/other.rs'
        other.write_text('fn run() {}')
        self.assertEqual(gate.check(root), [])
        with self.assertRaisesRegex(ValueError, 'unbalanced Rust token group'): caps.read_hits(root / 'rust-modules')
        other.write_text('fn run() { Instant::now(); }')
        with self.assertRaisesRegex(ValueError, 'unbalanced Rust token group'): gate.check(root)
    def test_directories_globs_traversal_and_absolute_paths_are_rejected(self):
        for name in ('rust-modules/data/src', 'rust-modules/data/src/*.rs', '../outside.rs',
                     '/absolute.rs', 'rust-modules/data/src/../x.rs', 'rust-modules/data/src/./x.rs'):
            root = self.fixture(ini=f'[{name}]\nkind = worker\nreason = nope\n')
            with self.assertRaises(ValueError): gate.check(root)
    def test_machine_logic_cannot_be_exempted(self):
        for source in ('struct S { state: u8 } impl S { fn step(&mut self) { Instant::now(); } }',
                       'impl Machine<H> for S { fn run() { Instant::now(); } }',
                       'fn transition() { Instant::now(); }'):
            root = self.fixture(source=source)
            with self.assertRaisesRegex(ValueError, 'machine logic'): gate.check(root)
    def test_registered_machine_file_is_not_a_port_even_without_step_name(self):
        root = self.fixture(source='struct Owner; fn event() { Instant::now(); }')
        (root / 'ci/machines.ini').write_text('[data/src/job_ports.rs::Owner]\nevent = Ev\n')
        with self.assertRaisesRegex(ValueError, 'machine logic'): gate.check(root)
    def test_stale_path_and_clock_free_exemptions_fail(self):
        root = self.fixture(source='fn run() {}')
        with self.assertRaisesRegex(ValueError, 'stale port'): gate.check(root)
        (root / 'rust-modules/data/src/job_ports.rs').unlink()
        with self.assertRaisesRegex(ValueError, 'stale'): gate.check(root)
    def test_unknown_keys_and_missing_reasons_fail(self):
        for extra in ('kind = directory\nreason = bad', 'kind = worker\nreason =',
                      'kind = worker\nreason = x\nscope = data/'):
            root = self.fixture(ini=f'[rust-modules/data/src/job_ports.rs]\n{extra}\n')
            with self.assertRaises(ValueError): gate.check(root)
if __name__ == '__main__': unittest.main()
