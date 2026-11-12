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
STRICT = f'[{FILE}::Machine]\nevent = Event\neffect = Fx\nwriter_policy = strict\nstate_field = state\ntransition = transition\nconstructor = new\n'
TYPES = 'enum Event { Set } enum Fx { Changed }\n'
HEADER = TYPES + 'struct Machine { state: u8 }\n'
GOOD = '''impl Machine {
    fn new() -> Self { Self { state: 0 } }
    fn transition(&mut self, event: Event, fx: &mut Vec<Fx>) { match event { Event::Set => { self.state = 1; fx.push(Fx::Changed); } } }
    fn read(&self) -> u8 { self.state }
}
'''


def verdict(source, ini=STRICT, extra=None):
    if '::Machine]' in ini and '[module ' + FILE + ']' not in ini:
        ini += f'\n[module {FILE}]\nwildcard_enum_match_arm = deny\n'
        source = '#![deny(clippy::wildcard_enum_match_arm)]\n' + source
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
    fn transition(&mut self, event: Event, fx: &mut Vec<Fx>) { self.state = 1; }
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
        for method in ('fn transition(&mut self, event: Event, fx: &mut Vec<Fx>) { self.state = 2; }',
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
        src = TYPES + '''#[derive(Clone, Copy)] enum State { Idle, Active }
struct Machine { state: State }
impl Machine {
    fn new() -> Self { Self { state: State::Idle } }
    fn transition(&mut self, event: Event, fx: &mut Vec<Fx>) { self.state = State::Active; }
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
        src = HEADER + GOOD + 'impl Machine { pub(crate) fn step(&mut self, event: Event, fx: &mut Vec<Fx>) { self.transition(event, fx); } }'
        self.assertEqual(verdict(src), [])
        self.assertTrue(verdict(src + 'impl Machine { pub fn update(&mut self) { self.state = 4; } }'))
        self.assertTrue(verdict(HEADER + GOOD.replace('fn transition', 'pub(crate) fn transition')))

    def test_strict_keys_and_stale_contract(self):
        for ini in (f'[{FILE}::Machine]\nstate_field = state\n',
                    STRICT.replace('transition = transition\n', ''),
                    STRICT.replace('constructor = new', 'constructor = '),
                    STRICT.replace('state_field = state', 'state_field = missing'),
                    STRICT.replace('transition = transition', 'transition = missing'),
                    STRICT.replace('constructor = new', 'constructor = missing')):
            with self.subTest(ini=ini):
                self.assertTrue(verdict(HEADER + GOOD, ini))

    def test_raw_field_and_invalid_writer_signatures(self):
        raw = HEADER.replace('state:', 'r#state:') + GOOD.replace('.state', '.r#state').replace('{ state:', '{ r#state:')
        self.assertEqual(verdict(raw), [])
        self.assertTrue(verdict(HEADER + GOOD.replace('fn new() -> Self', 'fn new(&mut self) -> Self')))
        self.assertTrue(verdict(HEADER + GOOD.replace('fn transition(&mut self, event: Event, fx: &mut Vec<Fx>)', 'fn transition(&self)')))
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
    def test_policy_cannot_be_downgraded_in_root_child_or_include(self):
        ini = f'[module {FILE}]\nwildcard_enum_match_arm = deny\n'
        header = '#![deny(clippy::wildcard_enum_match_arm)]\n'
        for level in ('allow', 'expect', 'warn'):
            for attribute in (f'#[{level}(clippy::wildcard_enum_match_arm)]',
                              f'#[cfg_attr(feature = "shipping", {level}(clippy::wildcard_enum_match_arm))]'):
                weak = attribute + ' fn weak() {}'
                with self.subTest(level=level, attribute=attribute):
                    self.assertTrue(verdict(header + weak, ini))
                    self.assertTrue(verdict(header + 'mod child;', ini, {'fixture/src/machine/child.rs': weak}))
                    self.assertTrue(verdict(header + 'include!("included.rs");', ini, {'fixture/src/included.rs': weak}))
        wire = 'fn wire(s: &str) { match s { "known" => (), _ => () } }'
        self.assertEqual(verdict(header + wire, ini), [])

    def test_missing_stale_or_unknown_policy_fails(self):
        for ini in (f'[module {FILE}]\n', f'[module {FILE}]\nwildcard_enum_match_arm = maybe\n',
                    '[module missing.rs]\nwildcard_enum_match_arm = deny\n'):
            self.assertTrue(verdict('', ini))



class EventContract(unittest.TestCase):
    def test_event_binding_rejects_setters_missing_and_prose_metadata(self):
        self.assertEqual(verdict(HEADER + GOOD), [])
        for source, ini in (
            (HEADER + GOOD.replace('event: Event', 'event: u8'), STRICT),
            (HEADER + GOOD.replace('event: Event', 'event: State'), STRICT),
            (HEADER + GOOD.replace('event: Event', 'event: &mut Event'), STRICT),
            (HEADER + GOOD, STRICT.replace('event = Event', 'event = State')),
            (HEADER + GOOD, STRICT.replace('event = Event', 'event = Event (prose)')),
            (HEADER + GOOD, STRICT.replace('event = Event\n', '')),
            (HEADER + GOOD, STRICT.replace('effect = Fx\n', '')),
            (HEADER.replace('enum Event { Set }', 'struct Event;') + GOOD, STRICT),
        ):
            with self.subTest(source=source, ini=ini): self.assertTrue(verdict(source, ini))
        raw = HEADER.replace('state: u8', 'state: Event') + GOOD
        self.assertTrue(verdict(raw), 'an owned state enum is not an admission event')

    def test_effect_binding_requires_real_mutable_registered_enum_output(self):
        for typ in ('u8', 'Vec<u8>', '&Vec<Fx>', '&mut Vec<Event>', '&mut impl FnMut(Fx)', '&mut Unknown'):
            with self.subTest(typ=typ):
                self.assertTrue(verdict(HEADER + GOOD.replace('&mut Vec<Fx>', typ)))
        self.assertTrue(verdict(HEADER.replace('enum Fx { Changed }', 'struct Fx;') + GOOD))
        self.assertTrue(verdict(HEADER + GOOD, STRICT.replace('effect = Fx', 'effect = none (later)')))

    def test_borrowed_event_and_real_effect_alias_are_bound(self):
        for alias in ('type Effects = Vec<Fx>;', 'type Effects = std::vec::Vec<Fx>;'):
            self.assertEqual(verdict(alias + HEADER + GOOD.replace('event: Event', 'event: &Event').replace('&mut Vec<Fx>', '&mut Effects')), [])
        child = 'fixture/src/machine/output.rs'
        source = 'mod output; use output::Effects;' + HEADER.replace('enum Fx { Changed }', 'use output::Fx;') + GOOD.replace('&mut Vec<Fx>', '&mut Effects')
        self.assertEqual(verdict(source, extra={child: 'pub enum Fx { Changed } pub type Effects = Vec<Fx>;'}), [])
        self.assertTrue(verdict('type Vec<T> = Unknown<T>;' + HEADER + GOOD))

    def test_pure_view_exact_none_has_no_effect_channel(self):
        source = HEADER + GOOD.replace(', fx: &mut Vec<Fx>', '').replace(' fx.push(Fx::Changed);', '')
        ini = STRICT.replace('effect = Fx', 'effect = none')
        self.assertEqual(verdict(source, ini), [])
        self.assertTrue(verdict(HEADER + GOOD, ini))

    def test_every_owned_field_and_production_child_is_guarded(self):
        source = HEADER.replace('state: u8', 'state: u8, pending: Vec<u8>, next_command: u64, cache: String') + GOOD.replace('state: 0', 'state: 0, pending: Vec::new(), next_command: 0, cache: String::new()')
        self.assertEqual(verdict(source), [])
        for field in ('state', 'pending', 'next_command', 'cache'):
            for body in (f'self.{field} = panic!("rogue");', f'let alias = &mut self.{field}; unknown(alias);', f'unknown!(self.{field});'):
                with self.subTest(field=field, body=body):
                    self.assertTrue(verdict(source + 'impl Machine { fn rogue(&mut self) { ' + body + ' } }'))
                    self.assertTrue(verdict(source + 'mod child;', extra={'fixture/src/machine/child.rs': 'fn rogue(m: &mut super::Machine) { ' + body.replace('self.', 'm.') + ' }'}))
        self.assertTrue(verdict(source + 'impl Machine { fn drain(&mut self) { self.pending.clear(); self.next_command = 0; } }'))

    def test_mutating_helpers_do_not_inherit_reducer_authority(self):
        for helper in ('fn helper(&mut self) { self.state = 5; }', 'fn helper(&mut self) { unknown(self); }'):
            source = HEADER + GOOD.replace('self.state = 1;', 'self.helper();') + 'impl Machine { ' + helper + ' }'
            self.assertTrue(verdict(source))
        self.assertTrue(verdict(HEADER + GOOD.replace('self.state = 1;', 'self.unknown();')))
        for body in ('let mut nested = || { self.state = 5; }; nested();',
                     'let mut nested = || self.state = 5; nested();',
                     'let mut nested = move || { *self = Self::new(); }; nested();',
                     'fn nested(m: &mut Machine) { m.state = 5; } nested(self);',
                     'unknown!(self);', 'unknown!(self.state);'):
            with self.subTest(body=body): self.assertTrue(verdict(HEADER + GOOD.replace('self.state = 1;', body)))

    def test_whole_self_alias_and_unknown_calls_cannot_escape(self):
        for body in ('let alias = self; unknown(alias);', 'let alias = &mut *self; unknown(alias);',
                     'unknown(self);', '*self = Self::new();', 'Self::setter(self, 0);'):
            with self.subTest(body=body): self.assertTrue(verdict(HEADER + GOOD + 'impl Machine { fn rogue(&mut self) { ' + body + ' } }'))
        source = 'type Alias = Machine;' + HEADER + GOOD + 'fn rogue(m: &mut Alias) { *m = Machine::new(); }'
        self.assertTrue(verdict(source))

    def test_constructors_are_fresh_not_machine_factories(self):
        for signature in ('fn new(other: Self) -> Self', 'fn new(other: Machine) -> Self', 'fn new(other: &Machine) -> Self'):
            self.assertTrue(verdict(HEADER + GOOD.replace('fn new() -> Self', signature)))
        self.assertTrue(verdict('type Alias = Machine;' + HEADER + GOOD.replace('fn new() -> Self', 'fn new(other: Alias) -> Self')))
        self.assertTrue(verdict(HEADER + GOOD.replace('Self { state: 0 }', 'Self { state: 0, ..other }')))

    def test_proved_shared_decision_is_not_a_mutself_helper(self):
        source = HEADER + GOOD.replace('self.state = 1;', 'self.state = self.decide();') + 'impl Machine { fn decide(&self) -> u8 { 1 } }'
        self.assertEqual(verdict(source), [])
        self.assertTrue(verdict(source.replace('fn decide(&self)', 'fn decide(&mut self)')))


class ProductionClosure(unittest.TestCase):
    def test_conditional_paths_nested_paths_and_unresolved_modules_fail_closed(self):
        for attribute in ('#[cfg_attr(feature = "shipping", path = "alternate.rs")]',
                          '#[cfg_attr(feature = "shipping", cfg_attr(feature = "other", path = "alternate.rs"))]'):
            source = HEADER + GOOD + attribute + ' mod child;'
            with self.subTest(attribute=attribute):
                self.assertTrue(verdict(source, extra={'fixture/src/machine/child.rs': '', 'fixture/src/alternate.rs': 'fn rogue(m: &mut super::Machine) { m.state = 2; }'}))
                self.assertEqual(verdict(HEADER + GOOD + '#[cfg(test)]' + attribute + ' mod missing;'), [])
        self.assertTrue(verdict(HEADER + GOOD + 'mod generated;'))
        self.assertTrue(verdict(HEADER + GOOD + '#[path = "absent.rs"] mod generated;'))
        self.assertEqual(verdict(HEADER + GOOD + '#[cfg_attr(feature = "shipping", allow(dead_code))] mod child;', extra={'fixture/src/machine/child.rs': ''}), [])

    def test_static_includes_are_checked_opaque_missing_includes_are_rejected(self):
        for include in ('include!(concat!(env!("OUT_DIR"), "/generated.rs"));', 'include!(NAME);', 'include!("missing.rs");'):
            with self.subTest(include=include):
                self.assertTrue(verdict(HEADER + GOOD + include))
                self.assertEqual(verdict(HEADER + GOOD + '#[cfg(test)] mod tests { ' + include + ' }'), [])
        included = 'fixture/src/included.rs'
        self.assertEqual(verdict(HEADER + GOOD + 'include!("included.rs");', extra={included: ''}), [])
        self.assertTrue(verdict(HEADER + GOOD + 'include!("included.rs");', extra={included: 'fn rogue(m: &mut Machine) { m.state = 2; }'}))
        self.assertTrue(verdict(HEADER + GOOD + 'include!("included.rs");', extra={included: 'include!(NAME);'}))

    def test_explicit_and_nested_child_paths_are_exhausted(self):
        source = HEADER + GOOD + '#[path = "alternate.rs"] mod child;'
        self.assertEqual(verdict(source, extra={'fixture/src/alternate.rs': ''}), [])
        self.assertTrue(verdict(source, extra={'fixture/src/alternate.rs': '#![warn(clippy::wildcard_enum_match_arm)]'}))
        for level in ('allow', 'expect', 'warn'):
            source = HEADER + GOOD + 'mod outer { mod child; }'
            self.assertTrue(verdict(source, extra={'fixture/src/machine/outer/child.rs': f'#![{level}(clippy::wildcard_enum_match_arm)]'}))


class AlternateAuthorityRegression(unittest.TestCase):
    def test_type_alias_impl_and_child_named_transition_are_not_authorities(self):
        self.assertTrue(verdict('type Alias = Machine;' + HEADER + GOOD + 'impl Alias { fn rogue(&mut self) { self.state = 3; } }'))
        self.assertTrue(verdict('type Alias = Machine;' + HEADER + GOOD + 'impl SomeTrait for Alias { fn rogue(&mut self) { unknown(self); } }'))
        source = HEADER + GOOD + 'mod child;'
        self.assertTrue(verdict(source, extra={'fixture/src/machine/child.rs': 'use super::Machine; impl Machine { fn transition(&mut self, next: u8) { self.state = next; } }'}))


if __name__ == '__main__':
    unittest.main()
