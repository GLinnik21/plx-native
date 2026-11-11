"""Shared state readers are accepted only after resolving their immutable payload graph."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_check_machine_writer import verdict, STRICT
import unittest
INI = STRICT + 'state_reads = immutable\n'
SOURCE = '''
struct Payload { value: String }
enum State { Idle, Active(Payload) }
impl State {
    fn active(&self) -> bool { matches!(self, Self::Active(_)) }
    fn payload(&self) -> Option<&Payload> { match self { Self::Idle => None, Self::Active(p) => Some(p) } }
    fn consume(self) -> Self { Self::Idle }
}
struct View<'a> { state: &'a State }
struct Machine { state: State }
impl Machine {
    fn new() -> Self { Self { state: State::Idle } }
    fn step(&mut self) { self.state = State::Idle; }
    fn read(&self) -> bool { self.state.active() }
    fn view(&self) -> View<'_> { View { state: &self.state } }
}
'''
class Readonly(unittest.TestCase):
    def test_shared_getter_view_and_owned_intrinsic_transition(self):
        self.assertEqual(verdict(SOURCE, INI), [])
        source = SOURCE.replace('self.state = State::Idle;', 'let old = std::mem::replace(&mut self.state, State::Idle); self.state = old.consume();')
        self.assertEqual(verdict(source, INI), [])
    def test_receiver_mutation_consumption_and_mutable_reference_are_not_readers(self):
        for change in ('fn active(&mut self)', 'fn active(self)'):
            self.assertTrue(verdict(SOURCE.replace('fn active(&self)', change), INI))
        self.assertTrue(verdict(SOURCE.replace('self.state.active()', 'self.state.consume()'), INI))
        self.assertTrue(verdict(SOURCE + 'impl Machine { fn leak(&mut self) -> &mut State { &mut self.state } }', INI))
    def test_nested_interior_authority_and_aliases_fail_closed(self):
        for typ, prefix in [('Cell<u8>', 'use std::cell::Cell;'), ('Hidden', 'type Hidden = std::sync::Arc<std::cell::Cell<u8>>;'),
                            ('Mutex<String>', 'use std::sync::Mutex;'), ('*mut u8', ''), ('fn()', ''),
                            ('Unknown', ''), ('Foreign', 'use foreign::Foreign;')]:
            source = prefix + SOURCE.replace('value: String', 'value: ' + typ)
            self.assertTrue(verdict(source, INI), typ)
    def test_alias_to_plain_data_and_standard_wrapper_is_resolved(self):
        source = 'type Text = String; type Values = Vec<Option<Text>>;' + SOURCE.replace('value: String', 'value: Values')
        self.assertEqual(verdict(source, INI), [])
    def test_immutable_shared_alias_cannot_launder_cell(self):
        source = SOURCE + 'impl Machine { fn alias(&self) { let alias = &self.state; alias.active(); } }'
        self.assertEqual(verdict(source, INI), [])
        self.assertTrue(verdict('use std::cell::Cell;' + source.replace('value: String', 'value: Cell<u8>'), INI))
    def test_mutable_alias_and_hidden_macro_still_fail(self):
        for code in ('let alias = &mut self.state;', 'mutate!(self, state);', 'give_away(self);',
                     'self.state = State::Idle;', 'let alias = self.state;', 'replace_elsewhere(&mut self.state);'):
            self.assertTrue(verdict(SOURCE + 'impl Machine { fn helper(&mut self) { ' + code + ' } }', INI), code)
    def test_child_alias_payload_is_resolved_and_checked(self):
        source = SOURCE.replace('struct Payload { value: String }', 'mod payload; use payload::Payload;')
        child = 'fixture/src/machine/payload.rs'
        self.assertEqual(verdict(source, INI, {child: 'pub struct Payload { pub value: String }'}), [])
        self.assertTrue(verdict(source, INI, {child: 'pub struct Payload { pub value: std::sync::atomic::AtomicU8 }'}))
    def test_standard_type_name_shadow_cannot_become_an_axiom(self):
        self.assertTrue(verdict('type String = std::cell::Cell<u8>;' + SOURCE, INI))
    def test_unsafe_accessor_and_opaque_result_are_rejected(self):
        self.assertTrue(verdict(SOURCE.replace('matches!(self, Self::Active(_))', 'unsafe { mutate(self); true }'), INI))
        self.assertTrue(verdict(SOURCE.replace('fn active(&self) -> bool', 'fn active(&self) -> Unknown'), INI))
class Regression(unittest.TestCase):
    def test_test_only_fields_and_impl_trait_parameters_do_not_hide_writers(self):
        source = SOURCE.replace('struct Machine { state: State }', 'struct Machine { state: State, #[cfg(test)] counter: u32 }')
        self.assertEqual(verdict(source, INI), [])
        source += 'impl Machine { fn helper(&mut self, callback: &mut impl FnMut()) { self.state = State::Idle; } }'
        self.assertTrue(verdict(source, INI))
    def test_qualified_or_shadowed_macros_cannot_rewrite_shared_places(self):
        for code in ('evil::matches!(&self.state, _);', 'rewrite!(&self.state);'):
            self.assertTrue(verdict(SOURCE + 'impl Machine { fn helper(&self) { ' + code + ' } }', INI))
        source = 'macro_rules! matches { ($($x:tt)*) => {} }' + SOURCE
        self.assertTrue(verdict(source, INI))
    def test_shared_view_guarded_field_must_remain_immutable(self):
        for field in ("state: &'a mut State", "state: *mut State"):
            self.assertTrue(verdict(SOURCE.replace("state: &'a State", field), INI))
        self.assertTrue(verdict(SOURCE.replace("struct View<'a>", "#[rewrite_view] struct View<'a>"), INI))
    def test_procedural_attributes_cannot_change_proved_payload_layout(self):
        source = SOURCE.replace('struct Payload', '#[rewrite_payload] struct Payload')
        self.assertTrue(verdict(source, INI))
        source = SOURCE.replace('struct Payload', '#[cfg_attr(feature = "shipping", rewrite_payload)] struct Payload')
        self.assertTrue(verdict(source, INI))
    def test_standard_module_and_intrinsic_shadow_fail_closed(self):
        source = 'mod std { mod sync { type Arc<T> = Hidden; } }' + SOURCE.replace('value: String', 'value: std::sync::Arc<u8>')
        self.assertTrue(verdict(source, INI))
        source = 'use evil as std;' + SOURCE.replace('self.state = State::Idle;', 'std::mem::replace(&mut self.state, State::Idle);')
        self.assertTrue(verdict(source, INI))
    def test_caller_generic_argument_cannot_resolve_to_another_inert_type(self):
        source = SOURCE.replace('struct Payload { value: String }', 'mod wrappers; struct Hidden { v: std::cell::Cell<u8> } struct Payload { value: wrappers::Wrap<Hidden> }')
        child = 'fixture/src/machine/wrappers.rs'
        self.assertTrue(verdict(source, INI, {child: 'struct Hidden { v: u8 } pub type Wrap<T> = Vec<T>;'}))
    def test_unrelated_state_owner_is_resolved_not_module_exempted(self):
        source = SOURCE + 'struct Other { state: u8 } impl Other { fn change(&mut self) { self.state = 3; } }'
        self.assertEqual(verdict(source, INI), [])
        source += 'fn bad(m: &mut Machine) { m.state = State::Idle; }'
        self.assertTrue(verdict(source, INI))
    def test_test_support_children_are_test_only_but_other_features_are_not(self):
        child = 'fixture/src/machine/helper.rs'
        module = '#[cfg(any(test, feature = "test-support"))] mod helper;'
        self.assertEqual(verdict(SOURCE + module, INI, {child: 'fn poke(m: &mut super::Machine) { m.state = State::Idle; }'}), [])
        self.assertTrue(verdict(SOURCE + module.replace('test-support', 'shipping'), INI, {child: 'fn poke(m: &mut super::Machine) { m.state = State::Idle; }'}))
if __name__ == '__main__': unittest.main()
