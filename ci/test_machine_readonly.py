"""Shared state readers are accepted only after resolving their immutable payload graph."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_check_machine_writer import verdict, STRICT
import unittest
INI = STRICT + 'state_reads = immutable\n'
SOURCE = '''
enum Event { Set } enum Fx { Changed }
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
    fn transition(&mut self, event: Event, fx: &mut Vec<Fx>) { self.state = State::Idle; }
    fn read(&self) -> bool { self.state.active() }
    fn view(&self) -> View<'_> { View { state: &self.state } }
}
'''
GENERIC = '''
enum Event { Set } enum Fx { Changed }
pub trait Identity: Copy + Ord + sealed::Sealed {}
mod sealed { pub trait Sealed {} impl Sealed for u64 {} }
impl Identity for u64 {}
struct Payload<T> { id: T }
struct Machine<R: Identity> { state: Option<Payload<R>> }
impl<R: Identity> Machine<R> {
    fn new() -> Self { Self { state: None } }
    fn transition(&mut self, event: Event, fx: &mut Vec<Fx>) { let owned = std::mem::take(&mut self.state); self.state = owned; }
    fn view(&self) -> Option<&Payload<R>> { self.state.as_ref() }
}
'''
class Readonly(unittest.TestCase):
    def test_shared_getter_view_and_owned_intrinsic_transition(self):
        self.assertEqual(verdict(SOURCE, INI), [])
        source = SOURCE.replace('self.state = State::Idle;', 'let old = std::mem::replace(&mut self.state, State::Idle); self.state = old.consume();')
        self.assertEqual(verdict(source, INI), [])
    def test_generic_identity_requires_a_closed_inert_bound(self):
        self.assertEqual(verdict(GENERIC, INI), [])
        for source in (
            GENERIC.replace('R: Identity', 'R: Copy + Ord'),
            GENERIC.replace('mod sealed', 'pub mod sealed'),
            GENERIC.replace('mod sealed', '#[rewrite] mod sealed'),
            GENERIC.replace('pub trait Identity', '#[rewrite] pub trait Identity'),
            GENERIC.replace('impl Sealed for u64 {}', 'impl Sealed for *mut u8 {}'),
            GENERIC.replace('impl Sealed for u64 {}', 'impl Sealed for std::cell::Cell<u8> {}'),
            GENERIC + 'impl sealed::Sealed for Bad {} struct Bad { value: std::cell::Cell<u8> }',
            GENERIC + 'fn hidden() { make_impl!(); }',
            GENERIC + 'mod child;',
        ):
            with self.subTest(source=source): self.assertTrue(verdict(source, INI))

    def test_owned_data_containers_do_not_admit_mutable_authority(self):
        for typ in ('std::collections::VecDeque<String>', 'std::collections::BTreeMap<String, u8>',
                    'Result<std::path::PathBuf, u8>'):
            self.assertEqual(verdict(SOURCE.replace('value: String', 'value: ' + typ), INI), [])
            self.assertTrue(verdict(SOURCE.replace('value: String', 'value: ' + typ.replace('String', 'std::cell::Cell<u8>').replace('PathBuf', 'PathBuf')), INI)) if 'String' in typ else None

    def test_test_only_tail_does_not_hide_a_later_registered_impl(self):
        for tail in ('#[cfg(test)] mod tests { fn x() {} }', '#[cfg(test)] mod tests;'):
            self.assertTrue(verdict(SOURCE + tail + ' impl Machine { fn rogue(&mut self) { self.state = State::Idle; } }', INI))
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
class CompleteReaderRegression(unittest.TestCase):
    def test_closed_scalar_reads_do_not_allow_mutable_aliases_or_consuming_drains(self):
        source = SOURCE.replace('struct Machine { state: State }', 'struct Machine { state: State, counter: u64 }').replace('Self { state: State::Idle }', 'Self { state: State::Idle, counter: 0 }')
        self.assertEqual(verdict(source, INI), [])
        for body in ('let alias = &mut self.counter; unknown(alias);',
                     'let alias = &mut (self.counter); unknown(alias);',
                     'let ref mut alias = self.counter; unknown(alias);',
                     'let nested = || { self.counter = 1; };',
                     'let nested = || { self.counter += 1; };'):
            with self.subTest(body=body):
                self.assertTrue(verdict(source + 'impl Machine { fn rogue(&mut self) { ' + body + ' } }', INI))
        self.assertTrue(verdict(source + 'impl Machine { fn drain(self) { let value = self.state; } }', INI))

    def test_unrelated_copy_derive_does_not_certify_a_noncopy_value(self):
        source = 'enum Event { Set } enum Fx { Changed } #[derive(Copy, Clone)] struct Other { value: u8 } struct Payload { value: u8 } struct Machine { state: Payload } impl Machine { fn new() -> Self { Self { state: Payload { value: 0 } } } fn transition(&mut self, event: Event, fx: &mut Vec<Fx>) {} fn rogue(&mut self) { let alias = self.state; unknown(alias); } }'
        self.assertTrue(verdict(source, INI))

class TypeBindingRegression(unittest.TestCase):
    def test_static_path_payload_is_resolved_and_conditional_path_remains_closed(self):
        source = SOURCE.replace('struct Payload { value: String }', '#[path = "payload_data.rs"] mod payload; use payload::Payload;')
        extra = {'fixture/src/payload_data.rs': 'pub struct Payload { pub value: String }'}
        self.assertEqual(verdict(source, INI, extra), [])
        extra['fixture/src/payload_data.rs'] = 'pub struct Payload { pub value: std::cell::Cell<u8> }'
        self.assertTrue(verdict(source, INI, extra))
        self.assertTrue(verdict(source.replace('#[path = "payload_data.rs"]', '#[cfg_attr(feature = "shipping", path = "payload_data.rs")]'), INI, extra))

    def test_inert_btreeset_elements_do_not_admit_authority(self):
        self.assertEqual(verdict(SOURCE.replace('value: String', 'value: std::collections::BTreeSet<String>'), INI), [])
        self.assertTrue(verdict(SOURCE.replace('value: String', 'value: std::collections::BTreeSet<std::cell::Cell<u8>>'), INI))

    def test_copy_payload_getter_is_not_a_closed_generic_proof(self):
        source = 'enum Event { Set } enum Fx { Changed } struct Machine<T: Copy> { state: T } impl<T: Copy> Machine<T> { fn new(state: T) -> Self { Self { state } } fn transition(&mut self, event: Event, fx: &mut Vec<Fx>) {} fn expose(&self) -> T { self.state } }'
        self.assertTrue(verdict(source))
        self.assertTrue(verdict(source.replace('T: Copy', 'T: Copy + Ord')))

    def test_foreign_json_and_local_replacements_are_not_name_whitelisted(self):
        for prefix, typ in (('use serde_json::Value;', 'Value'),
                            ('mod serde_json { pub struct Value { x: std::cell::Cell<u8> } }', 'serde_json::Value'),
                            ('struct Value { x: *mut u8 }', 'Value')):
            self.assertTrue(verdict(prefix + SOURCE.replace('value: String', 'value: ' + typ), INI))

class SyntaxRegression(unittest.TestCase):
    def test_logical_negation_does_not_become_a_macro_certificate_hole(self):
        self.assertEqual(verdict(GENERIC + 'fn pure() { if !(true) {} }', INI), [])
        self.assertTrue(verdict(SOURCE + 'impl Machine { fn rogue(&mut self) { r#if!(self.state); } }', INI))
        self.assertTrue(verdict(SOURCE + 'impl Machine { fn rogue(&mut self) { unknown!(self.state); } }', INI))

class ConvergenceProjectionRegression(unittest.TestCase):
    ARC = '''enum Event { Set } enum Fx { Changed } struct Payload { value: String } struct Machine { state: std::sync::Arc<Payload> } impl Machine { fn new() -> Self { Self { state: std::sync::Arc::new(Payload { value: String::new() }) } } fn transition(&mut self, event: Event, fx: &mut Vec<Fx>) {} fn same(&self, other: &Payload) -> bool { *other != *self.state } }'''
    ITER = '''enum Event { Set } enum Fx { Changed } struct Payload { left: Option<String>, right: Option<String> } impl Payload { fn items(&self) -> impl Iterator<Item = &String> { self.left.iter().chain(self.right.iter()) } } struct Machine { state: Payload } impl Machine { fn new() -> Self { Self { state: Payload { left: None, right: None } } } fn transition(&mut self, event: Event, fx: &mut Vec<Fx>) {} fn any(&self) -> bool { self.state.items().next().is_some() } }'''
    MATCH = '''enum Event { Set } enum Fx { Changed } enum State { Idle, Active { value: String, loaded: bool } } struct Machine { state: State } impl Machine { fn new() -> Self { Self { state: State::Idle } } fn transition(&mut self, event: Event, fx: &mut Vec<Fx>) { match &mut self.state { State::Active { loaded: flag @ false, .. } => *flag = true, State::Active { .. } => self.state = State::Idle, State::Idle => {} } } }'''
    def test_arc_comparison_is_shared_but_not_a_value_escape(self):
        self.assertEqual(verdict(self.ARC, INI), [])
        for body in ('unknown(*self.state); false', 'let alias = &mut self.state; unknown(alias); false'):
            self.assertTrue(verdict(self.ARC.replace('*other != *self.state', body), INI))
        self.assertTrue(verdict(self.ARC.replace('value: String', 'value: std::cell::Cell<u8>'), INI))
    def test_only_direct_standard_inert_iterator_chains_are_proved(self):
        self.assertEqual(verdict(self.ITER, INI), [])
        for change in ('unknown(self.left.iter())', 'self.left.iter().map(callback)', 'self.left.iter_mut().chain(self.right.iter_mut())'):
            self.assertTrue(verdict(self.ITER.replace('self.left.iter().chain(self.right.iter())', change), INI))
        self.assertTrue(verdict(self.ITER.replace('Option<String>', 'Option<std::cell::Cell<u8>>'), INI))
        self.assertTrue(verdict('trait Iterator {} ' + self.ITER, INI))
    def test_mutable_match_alias_is_confined_to_a_direct_bool_write(self):
        self.assertEqual(verdict(self.MATCH, INI), [])
        for body in ('unknown(flag)', '{ let alias = flag; unknown(alias); }', '{ let nested = || *flag = true; nested(); }', '*flag = callback()'):
            self.assertTrue(verdict(self.MATCH.replace('*flag = true', body), INI))
        self.assertTrue(verdict(self.MATCH.replace('fn transition(', 'fn rogue('), INI))
    def test_declared_backend_path_and_reexport_are_resolved_without_sibling_guess(self):
        source = GENERIC.replace('impl Sealed for u64 {}', 'impl Sealed for super::super::backend::LogicalRequestId {}')
        extra = {'fixture/src/lib.rs': 'mod machine; mod backend;', 'fixture/src/backend.rs': '#[path = "backend_metrics.rs"] mod metrics; pub use metrics::LogicalRequestId;', 'fixture/src/backend_metrics.rs': '#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq)] pub struct LogicalRequestId(u64);'}
        self.assertEqual(verdict(source, INI, extra), [])
        self.assertTrue(verdict(source.replace('super::super::backend::', 'super::super::backend_metrics::'), INI, extra))
        extra['fixture/src/backend_metrics.rs'] = 'pub struct LogicalRequestId(std::cell::Cell<u64>);'
    def test_generated_opaque_layout_is_not_a_name_or_copy_certificate(self):
        source = SOURCE.replace('struct Payload { value: String }', 'mod payload; use payload::Payload;')
        extra = {'fixture/src/machine/payload.rs': 'include!(concat!(env!("OUT_DIR"), "/payload.rs"));', 'fixture/src/machine/payload/guess.rs': '#[derive(Copy, Clone)] pub struct Payload { value: u64 }'}
        self.assertTrue(verdict(source, INI, extra))
        self.assertTrue(verdict(source, INI, extra))


class SharedTokens(unittest.TestCase):
    """The lexed-once token lists the proof and the machine-shape gate share."""
    def test_a_shared_list_is_read_only_and_one_text_is_one_list(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location('check_machine_shape', Path(__file__).with_name('check-machine-shape.py'))
        gate = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(gate)
        tokens = gate.production_tokens('struct A { x: u8 } #[cfg(test)] mod tests { fn t() {} }')
        self.assertIs(gate.production_tokens('struct A { x: u8 } #[cfg(test)] mod tests { fn t() {} }'), tokens)
        self.assertNotIn('tests', tokens)
        for mutate in (lambda: tokens.append('x'), lambda: tokens.__setitem__(0, 'x'), lambda: tokens.pop(), lambda: tokens.sort()):
            with self.assertRaises(TypeError): mutate()
        self.assertEqual(list(tokens)[:3], ['struct', 'A', '{'])

    def test_shadows_are_exactly_what_a_rescan_would_find(self):
        from machine_readonly import Shadows
        tokens = ['macro_rules', '!', 'm', '{', '}', 'macro', 'n', 'use', 'a', ':', ':', '*', ';', 'use', 'b', ';',
                  'mod', 'std', ';', 'macro_rules', '!']
        facts = Shadows(tokens)
        self.assertEqual(facts.macro_rules, {'m'})        # the trailing `macro_rules !` has no name to take
        self.assertEqual(facts.macro_items, {'n'})
        self.assertEqual(facts.imported, {'a', ':', '*', 'b'})
        self.assertTrue(facts.std_named)
        self.assertFalse(Shadows(['use', 'std', ':', ':', 'mem', ';']).std_named)   # importing std is not naming it as a module

if __name__ == '__main__': unittest.main()
