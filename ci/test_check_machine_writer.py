#!/usr/bin/env python3
"""Strict writer and enum-lint contracts, exercised only on private synthetic trees."""
import importlib.util
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('shape', ROOT / 'ci/check-machine-shape.py')
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)

FILE = 'fixture/src/machine.rs'
STRICT = f'[{FILE}::Machine]\nwriter_policy = strict\nstate_field = state\ntransition = step\nconstructor = new\n'
HEADER = 'struct Machine { state: u8 }\n'
GOOD = '''impl Machine {
    fn new() -> Self { Self { state: 0 } }
    fn step(&mut self) { self.state = 1; }
    fn read(&self) -> u8 { self.state }
}
'''


def verdict(source, ini=STRICT, extra=None):
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp) / 'rust-modules'
        path = root / FILE
        path.parent.mkdir(parents=True)
        path.write_text(source, encoding='utf-8')
        for name, text in (extra or {}).items():
            child = root / name
            child.parent.mkdir(parents=True, exist_ok=True)
            child.write_text(text, encoding='utf-8')
        registry = Path(tmp) / 'machines.ini'
        registry.write_text(ini, encoding='utf-8')
        return gate.check(root, registry)[0]


class Writer(unittest.TestCase):
    def test_constructor_transition_read_and_test_only(self):
        src = HEADER + GOOD + '''
#[cfg(test)] mod tests { fn helper(m: &mut super::Machine) { m.state = 2; } }
impl Machine { #[cfg(test)] fn fixture() -> Self { Self { state: 2 } } }
// self.state = 99;
const TEXT: &str = "self.state = 99;";
'''
        self.assertEqual(verdict(src), [])

    def test_associated_self_constant_constructor(self):
        ini = STRICT.replace('constructor = new', 'constructor = IDLE')
        source = HEADER + '''impl Machine {
    pub const IDLE: Self = Self { state: 0 };
    fn step(&mut self) { self.state = 1; }
}
'''
        self.assertEqual(verdict(source, ini), [])
        self.assertEqual(verdict(source + '#[cfg(test)] mod tests { fn fixture() -> super::Machine { super::Machine { state: 3 } } }', ini), [])
        for replacement in (
            'const IDLE: Self = Self { other: 0 };',
            'const IDLE: Machine = Self { state: 0 };',
            'const IDLE: Self = make();',
            'const WRONG: Self = Self { state: 0 };',
        ):
            with self.subTest(replacement=replacement):
                self.assertTrue(verdict(source.replace('pub const IDLE: Self = Self { state: 0 };', replacement), ini))
        self.assertTrue(verdict(source + ' impl Other { const IDLE: Self = Self { state: 0 }; }', ini))
        self.assertTrue(verdict(source + ' impl Machine { const OTHER: Self = Self { state: 0 }; }', ini))
        self.assertTrue(verdict(source + ' impl Machine { fn helper(&mut self) { self.state = 7; } }', ini))
        self.assertTrue(verdict(source.replace('pub const IDLE: Self = Self { state: 0 };',
                                               'fn fake() { const IDLE: Self = Self { state: 0 }; }'), ini))
        self.assertTrue(verdict(source.replace('pub const IDLE: Self = Self { state: 0 };', '') +
                                ' impl Trait for Machine { const IDLE: Self = Self { state: 0 }; }', ini))

    def test_production_mutations_and_escapes_fail(self):
        examples = [
            'self.state = 3;', 'self.state += 1;', 'self.state <<= 1;', 'self.state >>= 1;', 'self.state.inner.value = 1;',
            '(self.state, other) = (1, 2);', 'let x = &mut self.state;',
            'let x = &mut self.state.inner;', 'std::mem::replace(&mut self.state, 0);',
            'std::mem::take(&mut self.state);', 'std::mem::swap(&mut self.state, x);',
            'self.state.borrow_mut().push(1);', 'self.state.push(1);',
            'let Machine { state: ref mut x, .. } = self;',
            'let Machine { ref mut state, .. } = self;',
            'let Machine { state, .. } = self;',
            'let x = Self { state: 0 };', '*self = Self::new();',
            'let x = &mut self.r#state;', 'let ref mut alias = self.state;',
            'let ptr = core::ptr::addr_of_mut!(self.state);',
            'let alias = &self.state; alias.borrow_mut();',
            'let alias = self.state; alias.borrow_mut();', 'mutate(self.state);',
        ]
        for code in examples:
            with self.subTest(code=code):
                src = HEADER + GOOD + 'impl Machine { fn helper(&mut self) { ' + code + ' } }'
                self.assertTrue(verdict(src), code)

    def test_other_impl_does_not_authorize_writer_or_constructor(self):
        for method in ('fn step(&mut self) { self.state = 2; }',
                       'fn new() -> Machine { Machine { state: 2 } }'):
            self.assertTrue(verdict(HEADER + GOOD + 'struct Other; impl Other { ' + method + ' }'))

    def test_free_helper_cannot_write(self):
        src = HEADER + GOOD + 'fn helper(m: &mut Machine) { m.state = 1; }'
        self.assertTrue(verdict(src))

    def test_machine_alias_cannot_replace_or_escape_state(self):
        examples = (
            'fn helper(m: &mut Machine) { *m = Machine::new(); }',
            'fn helper(m: &mut Machine) { std::mem::replace(m, Machine::new()); }',
            'fn helper(m: &mut super::Machine) { *m = Machine::new(); }',
            'impl Machine { fn helper(m: &mut Self) { *m = Self::new(); } }',
            'impl Machine { fn helper(&mut self) { replace_elsewhere(self); } }',
            'impl Machine { fn helper(&mut self) { let alias = &mut *self; replace_elsewhere(alias); } }',
        )
        for code in examples:
            with self.subTest(code=code):
                self.assertTrue(verdict(HEADER + GOOD + code), code)
        leaked = GOOD.replace('self.state = 1;', 'replace_elsewhere(self);')
        self.assertTrue(verdict(HEADER + leaked))

    def test_macro_cannot_hide_a_field_name(self):
        src = HEADER + GOOD + 'impl Machine { fn helper(&mut self) { mutate!(self, state); } }'
        self.assertTrue(verdict(src))

    def test_readonly_matches_and_copy_enum_helper(self):
        src = '''#[derive(Clone, Copy)] enum State { Idle, Active }
struct Machine { state: State }
impl Machine {
    fn new() -> Self { Self { state: State::Idle } }
    fn step(&mut self) { self.state = State::Active; }
    fn is_idle(snapshot: State) -> bool { matches!(snapshot, State::Idle) }
    fn snapshot(&self) -> bool { Self::is_idle(self.state) }
    fn read(&self) -> bool { matches!(self.state, State::Idle | State::Active) }
    fn assert_snapshot(&self) { assert!(matches!(self.state, State::Idle)); }
    fn debug_snapshot(&self) { debug_assert!(matches!(self.state, State::Active),); }
}
enum Effect { Prepare, Load }
impl Effect { fn awaits(&self) -> bool { matches!(self, Self::Prepare | Self::Load) } }
'''
        self.assertEqual(verdict(src), [])
        self.assertTrue(verdict(src + 'impl Machine { fn leak(&mut self) -> &mut State { self.state } }'))
        self.assertTrue(verdict(src + 'impl Machine { fn alias(&mut self) { let x = self.state; x.mutate(); } }'))
        self.assertTrue(verdict(src + 'impl Machine { fn mutate(&mut self) { self.state.mutate(); } }'))
        self.assertTrue(verdict(src.replace('enum State { Idle, Active }', 'enum State { Idle, Active(&\'static Cell<u8>) }')))

    def test_readonly_macro_does_not_hide_writes_or_aliases(self):
        examples = (
            'matches!({ self.state = 3; self.state }, 3);',
            'matches!({ self.state += 1; self.state }, 3);',
            'matches!({ let x = &mut self.state; *x = 3; self.state }, 3);',
            'matches!(std::mem::replace(&mut self.state, 3), 0);',
            'matches!(self.state, ref mut alias);',
            'matches!(self.state, x if { x.mutate(); true });',
            'matches!(self.state, _ if { self.state = 1; true });',
            'assert!(matches!({ self.state = 3; self.state }, 3));',
            'assert!(matches!(self.state, 0), "{}", { self.state = 1; 0 });',
            'unknown!(self.state);',
            'unknown!(self, state);',
            'evil::matches!(self.state, _);',
        )
        for code in examples:
            with self.subTest(code=code):
                problems = verdict(HEADER + GOOD + 'impl Machine { fn helper(&mut self) { ' + code + ' } }')
                self.assertTrue(problems, code)
                self.assertTrue(any(FILE + '::helper:' in problem for problem in problems), problems)
        shadow = 'macro_rules! matches { ($($x:tt)*) => { mutate(self) } }\n'
        self.assertTrue(verdict(shadow + HEADER + GOOD +
                                'impl Machine { fn helper(&self) { matches!(self.state, 0); } }'))
        imported = 'use other::matches;\n'
        self.assertTrue(verdict(imported + HEADER + GOOD +
                                'impl Machine { fn helper(&self) { matches!(self.state, 0); } }'))

    def test_nested_helper_is_not_writer(self):
        src = HEADER + GOOD.replace('self.state = 1;', 'fn helper(m: &mut Machine) { m.state = 2; }')
        self.assertTrue(verdict(src))

    def test_mutable_accessor_cannot_escape_without_explicit_borrow(self):
        src = HEADER + GOOD + 'impl Machine { fn leak(&mut self) -> &mut u8 { self.state } }'
        self.assertTrue(verdict(src))
        src = HEADER + GOOD + "impl Machine { fn leak<'a>(&'a mut self) -> &'a mut u8 { self.state } }"
        self.assertTrue(verdict(src))

    def test_visibility_does_not_define_writer(self):
        src = HEADER + GOOD.replace('fn step', 'pub(crate) fn step')
        self.assertEqual(verdict(src), [])
        self.assertTrue(verdict(src + 'impl Machine { pub fn update(&mut self) { self.state = 4; } }'))

    def test_strict_keys_and_stale_contract(self):
        for ini in (f'[{FILE}::Machine]\nstate_field = state\n',
                    STRICT.replace('transition = step\n', ''),
                    STRICT.replace('constructor = new', 'constructor = '),
                    STRICT.replace('state_field = state', 'state_field = missing'),
                    STRICT.replace('transition = step', 'transition = missing'),
                    STRICT.replace('constructor = new', 'constructor = missing')):
            with self.subTest(ini=ini):
                self.assertTrue(verdict(HEADER + GOOD, ini))

    def test_raw_field_and_invalid_writer_signatures(self):
        raw = HEADER.replace('state:', 'r#state:') + GOOD.replace('.state', '.r#state').replace('{ state:', '{ r#state:')
        self.assertEqual(verdict(raw), [])
        self.assertTrue(verdict(HEADER + GOOD.replace('fn new() -> Self', 'fn new(&mut self) -> Self')))
        self.assertTrue(verdict(HEADER + GOOD.replace('fn step(&mut self)', 'fn step(&self)')))
        src = HEADER + GOOD + '#[cfg(any(test, feature = "extra"))] fn helper(m: &mut Machine) { m.state = 5; }'
        self.assertTrue(verdict(src))

    def test_writer_cannot_leak_mutable_field_borrow(self):
        src = HEADER + GOOD.replace('self.state = 1;', 'let leaked = &mut self.state; give_away(leaked);')
        self.assertTrue(verdict(src))

    def test_production_child_module_is_checked_but_test_module_is_not(self):
        child = 'fixture/src/machine/helper.rs'
        src = HEADER + GOOD + '\nmod helper;'
        self.assertTrue(verdict(src, extra={child: 'fn poke(m: &mut super::Machine) { m.state = 3; }'}))
        self.assertEqual(verdict(src.replace('mod helper;', '#[cfg(test)] mod helper;'),
                                 extra={child: 'fn poke(m: &mut super::Machine) { m.state = 3; }'}), [])


class WildcardPolicy(unittest.TestCase):
    def test_module_must_really_deny_the_clippy_lint(self):
        ini = f'[module {FILE}]\nwildcard_enum_match_arm = deny\n'
        for src in ('', '// #![deny(clippy::wildcard_enum_match_arm)]',
                    'const S: &str = "#![deny(clippy::wildcard_enum_match_arm)]";',
                    '#![allow(clippy::wildcard_enum_match_arm)]',
                    '#![cfg_attr(test, deny(clippy::wildcard_enum_match_arm))]',
                    '#![deny(clippy::wildcard_enum_match_arm)]\n#[allow(clippy::wildcard_enum_match_arm)] fn x() {}'):
            with self.subTest(src=src):
                self.assertTrue(verdict(src, ini))
        self.assertEqual(verdict('#![deny(clippy::wildcard_enum_match_arm)]\n'
                                 'fn numeric(n: u8) { match n { 0 => (), _ => () } }', ini), [])

    def test_missing_stale_or_unknown_policy_fails(self):
        for ini in (f'[module {FILE}]\n', f'[module {FILE}]\nwildcard_enum_match_arm = maybe\n',
                    '[module missing.rs]\nwildcard_enum_match_arm = deny\n'):
            self.assertTrue(verdict('', ini))


if __name__ == '__main__':
    unittest.main()
