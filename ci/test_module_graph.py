#!/usr/bin/env python3
"""ci/module_graph.py's resolution rules and ci/check-module-layers.py's verdicts, on synthetic
crates. Each case writes a tiny tree to a temp dir; nothing here reads rust-modules/."""
import contextlib
import importlib.util
import io
import tempfile
import unittest
from pathlib import Path

import module_graph

_spec = importlib.util.spec_from_file_location('check_module_layers', Path(__file__).with_name('check-module-layers.py'))
gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gate)


class Tree:
    def __init__(self, files):
        self._temp = tempfile.TemporaryDirectory(prefix='module-graph-')
        self.root = Path(self._temp.name).resolve()
        for name, source in files.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(source)

    def __enter__(self): return self
    def __exit__(self, *exc): self._temp.cleanup()


def refs(files, test=None):
    """{(source, target, test)} as `::` names for a crate made of `files`."""
    with Tree(files) as tree:
        crate = module_graph.Crate(tree.root)
    return {(module_graph.name(r.source), module_graph.name(r.target), r.test) for r in crate.refs
            if test is None or r.test == test}


LIB = 'mod a; mod b; mod c;\n'


class Resolution(unittest.TestCase):
    def test_crate_paths_resolve_to_the_deepest_declared_module(self):
        found = refs({'lib.rs': LIB, 'a.rs': 'fn f() { crate::b::inner::g(); crate::c::Item::new(); }',
                      'b.rs': 'pub mod inner;', 'b/inner.rs': '', 'c.rs': ''})
        self.assertEqual(found, {('a', 'b::inner', False), ('a', 'c', False)})

    def test_a_path_into_a_module_declared_later_in_the_walk_still_reaches_it(self):
        # `a` is walked before `b/mod.rs` declares `deep`: resolution must wait for the whole tree.
        found = refs({'lib.rs': LIB, 'a.rs': 'use crate::b::deep::X;', 'b/mod.rs': 'mod deep;',
                      'b/deep.rs': '', 'c.rs': ''})
        self.assertIn(('a', 'b::deep', False), found)

    def test_super_and_self_climb_from_file_and_inline_modules(self):
        found = refs({'lib.rs': LIB, 'a.rs': 'mod x; fn f() { self::x::g(); }',
                      'a/x.rs': 'fn g() { super::super::b::h(); } mod t { fn k() { super::super::super::c::z(); } }',
                      'b.rs': '', 'c.rs': ''})
        self.assertEqual(found, {('a', 'a::x', False), ('a::x', 'b', False), ('a::x::t', 'c', False)})

    def test_use_trees_expand_every_leaf(self):
        found = refs({'lib.rs': LIB, 'a.rs': 'use crate::{b::{self, Thing as T}, c::*};', 'b.rs': '', 'c.rs': ''})
        self.assertEqual(found, {('a', 'b', False), ('a', 'c', False)})

    def test_bare_top_level_paths_count_only_in_the_crate_root(self):
        found = refs({'lib.rs': LIB + 'fn root() { a::f(); std::mem::drop(1); }',
                      'a.rs': 'fn f() { b::g(); }',  # outside the root `b` is not in scope
                      'b.rs': '', 'c.rs': ''})
        self.assertEqual(found, {('crate', 'a', False)})

    def test_generic_arguments_start_their_own_path(self):
        found = refs({'lib.rs': LIB, 'a.rs': 'fn f() { crate::b::D::<crate::c::H>::new(); }',
                      'b.rs': '', 'c.rs': ''})
        self.assertEqual(found, {('a', 'b', False), ('a', 'c', False)})

    def test_exported_macros_name_their_defining_module(self):
        found = refs({'lib.rs': LIB,
                      'a.rs': '#[macro_export]\nmacro_rules! m { () => { $crate::c::f() } }',
                      'b.rs': 'fn f() { m!(); crate::m!(); }', 'c.rs': ''})
        self.assertEqual(found, {('a', 'c', False), ('b', 'a', False)})

    def test_macro_use_modules_export_their_macros_by_textual_scope(self):
        found = refs({'lib.rs': '#[macro_use]\nmod a;\nmod b;\nmod c;',
                      'a.rs': 'macro_rules! shared { () => {} }',
                      'b.rs': 'macro_rules! private_to_b { () => {} } fn f() { shared!(); private_to_b!(); }',
                      'c.rs': 'fn g() { private_to_b!(); }'})
        self.assertEqual(found, {('b', 'a', False)})

    def test_comments_strings_and_char_literals_name_nothing(self):
        found = refs({'lib.rs': LIB, 'a.rs': '\n'.join([
            '// crate::b::x', '/// [`crate::b::y`]', '/* outer /* crate::b::z */ still comment crate::b */',
            'const S: &str = "crate::b::s \\" crate::b::t";', 'const R: &str = r#"crate::b::r " "#;',
            "const Q: char = '\"'; fn lt<'a>(x: &'a str) -> &'a str { crate::c::kept(x) }",
            'const B: &[u8] = b"crate::b::bytes";']), 'b.rs': '', 'c.rs': ''})
        self.assertEqual(found, {('a', 'c', False)})

    def test_cfg_test_items_files_and_modules_are_test_references(self):
        found = refs({'lib.rs': LIB + '#[cfg(test)] mod only;', 'only.rs': 'fn f() { crate::c::x(); }',
                      'a.rs': '\n'.join([
                          '#[cfg(test)] use crate::b::One;',
                          '#[cfg(test)]\nfn helper() { crate::b::two(); }',
                          '#[cfg(not(test))] fn real() { crate::c::three(); }',
                          '#[cfg(any(test, feature = "x"))] fn either() { crate::c::four(); }',
                          '#[cfg(test)] mod tests { use super::*; fn t() { crate::c::five(); } }',
                          'fn after() { crate::b::six(); }']),
                      'b.rs': '', 'c.rs': ''})
        self.assertEqual(found, {('a', 'b', True), ('a', 'b', False), ('a', 'c', False),
                                 ('a::tests', 'a', True), ('a::tests', 'c', True), ('only', 'c', True)})

    def test_inner_cfg_test_attribute_marks_the_whole_file(self):
        found = refs({'lib.rs': LIB, 'a.rs': '#![cfg(test)]\nfn f() { crate::b::g(); }', 'b.rs': '', 'c.rs': ''})
        self.assertEqual(found, {('a', 'b', True)})

    def test_path_attributes_and_include_place_files_in_the_declaring_module(self):
        found = refs({'lib.rs': LIB, 'a.rs': '#[path = "a_tests.rs"]\n#[cfg(test)]\nmod checks;\ninclude!("a_extra.rs");',
                      'a_tests.rs': 'fn t() { crate::b::x(); }', 'a_extra.rs': 'fn e() { crate::c::y(); }',
                      'b.rs': '', 'c.rs': ''})
        self.assertEqual(found, {('a::checks', 'b', True), ('a', 'c', False)})

    def test_serde_attribute_strings_are_paths(self):
        found = refs({'lib.rs': LIB, 'a.rs': '\n'.join([
            '#[derive(Deserialize)] struct S {',
            '    #[serde(with = "crate::b::codec", default = "local::d")] x: u8,',
            '    #[doc = "crate::c::not_a_path"] y: u8 }']), 'b.rs': '', 'c.rs': ''})
        self.assertEqual(found, {('a', 'b', False)})

    def test_a_cfg_test_brace_macro_item_ends_at_its_group(self):
        # `thread_local! { … }` takes no `;`: the cfg(test) region must stop at its brace group,
        # not run on through the production items after it.
        found = refs({'lib.rs': LIB, 'a.rs': '\n'.join([
            '#[cfg(test)]\nthread_local! { static X: u8 = 0; }',
            '#[cfg(test)] mod t { fn x() { crate::c::y(); } }',
            'fn after() { crate::b::z(); }']), 'b.rs': '', 'c.rs': ''})
        self.assertEqual(found, {('a::t', 'c', True), ('a', 'b', False)})

    def test_cfg_test_items_of_production_modules_are_recorded_with_their_owner(self):
        with Tree({'lib.rs': LIB, 'b.rs': '', 'c.rs': '', 'a.rs': '\n'.join([
                '#[cfg(test)] pub(crate) fn helper() {}',
                '#[cfg(test)] pub(crate) static mut FLAG: u8 = 0;',
                'pub struct S { #[cfg(test)] field: u8 }',
                'impl S { pub fn real() {} #[cfg(test)] pub(crate) const fn fixture() -> u8 { 0 } }',
                '#[cfg(test)] impl crate::b::Trait for S { fn shown(&self) {} }',
                'fn body() { #[cfg(test)] fn local() {} }',
                'mod inner { #[cfg(test)] pub fn deep() {} }',
                '#[cfg(test)] mod tests { #[cfg(test)] fn hidden() {} }'])}) as tree:
            crate = module_graph.Crate(tree.root)
        self.assertEqual(set(crate.test_items), {
            (('a',), None, 'helper'), (('a',), None, 'FLAG'), (('a',), 'S', 'fixture'),
            (('a',), 'S', 'shown'), (('a', 'inner'), None, 'deep')})

    def test_references_to_cfg_test_items_by_path_glob_and_module(self):
        files = {'lib.rs': LIB, 'b.rs': '', 'c.rs': '\n'.join([
            'pub fn real() {} pub struct S;',
            '#[cfg(test)] pub fn helper() {}',
            'impl S { #[cfg(test)] pub fn fixture() {} }',
            '#[cfg(test)] pub mod support { pub fn s() {} }']),
            'a.rs': '\n'.join([
                'fn production() { crate::c::real(); }',
                '#[cfg(test)] mod by_path { fn t() { crate::c::helper(); crate::c::S::fixture(); crate::c::real(); } }',
                '#[cfg(test)] mod by_module { use crate::c::support::s; }',
                '#[cfg(test)] mod by_glob { use crate::c::*; fn t() { helper(); } }',
                '#[cfg(test)] mod by_alias { use crate::c; fn t() { c::helper(); c::real(); } }'])}
        with Tree(files) as tree:
            crate = module_graph.Crate(tree.root)
            found = sorted((module_graph.name(r.source), label) for r, label, _ in crate.test_item_refs())
        self.assertEqual(found, [('a::by_alias', 'c::helper'), ('a::by_glob', 'c::helper'),
                                 ('a::by_module', 'c::support'), ('a::by_path', 'c::S::fixture'),
                                 ('a::by_path', 'c::helper')])

    def test_precise_capturing_use_is_not_a_use_declaration(self):
        found = refs({'lib.rs': LIB, 'a.rs': "fn f<'a>(x: &'a u8) -> impl Sized + use<'a> { crate::b::g(x) }",
                      'b.rs': '', 'c.rs': ''})
        self.assertEqual(found, {('a', 'b', False)})

    def test_sccs_finds_the_cycle_and_leaves_a_diamond_alone(self):
        adjacency = {'a': {'b', 'c'}, 'b': {'d'}, 'c': {'d'}, 'd': set(), 'x': {'y'}, 'y': {'x'}}
        cycles = [c for c in module_graph.sccs(set(adjacency) | {'y'}, adjacency) if len(c) > 1]
        self.assertEqual(cycles, [['x', 'y']])


LAYERS = """
[low]
uses =
members = c
[high]
uses = low
members = crate a b
"""


class Gate(unittest.TestCase):
    def run_gate(self, files, layers=LAYERS, allow=None, *extra):
        with Tree(files) as tree:
            (tree.root / 'layers.ini').write_text(layers)
            allow_path = tree.root / 'allow.txt'
            if allow is not None: allow_path.write_text(allow)
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                code = gate.main(['--src', str(tree.root), '--layers', str(tree.root / 'layers.ini'),
                                  '--allow', str(allow_path), *extra])
            remaining = allow_path.read_text() if allow_path.exists() else None
        return code, out.getvalue(), remaining

    CLEAN = {'lib.rs': LIB, 'a.rs': 'fn f() { crate::c::g(); crate::b::h(); }', 'b.rs': '', 'c.rs': ''}
    UPWARD = dict(CLEAN, **{'c.rs': 'fn g() { crate::a::f(); }'})
    ENTRY = 'rust-modules/src/c.rs\ta\tL0 test step\n'

    def test_downward_and_same_layer_references_are_green(self):
        code, out, _ = self.run_gate(self.CLEAN, allow='# count: 0\n')
        self.assertEqual(code, 0, out)

    def test_an_upward_reference_fails_and_names_the_site(self):
        code, out, _ = self.run_gate(self.UPWARD, allow='# count: 0\n')
        self.assertEqual(code, 1)
        self.assertIn('c.rs names a ([high]), which [low] may not use: c.rs:1 a::f', out)

    def test_a_test_only_upward_reference_fails_too(self):
        code, out, _ = self.run_gate(dict(self.CLEAN, **{'c.rs': '#[cfg(test)] mod t { fn g() { crate::a::f(); } }'}),
                                     allow='# count: 0\n')
        self.assertEqual(code, 1)
        self.assertIn('(test)', out)

    def test_an_allowlisted_reference_is_green_and_a_stale_entry_fails(self):
        self.assertEqual(self.run_gate(self.UPWARD, allow='# count: 1\n' + self.ENTRY)[0], 0)
        code, out, _ = self.run_gate(self.CLEAN, allow='# count: 1\n' + self.ENTRY)
        self.assertEqual(code, 1)
        self.assertIn('stale allowlist entry rust-modules/src/c.rs\ta', out)

    def test_prune_drops_only_stale_entries(self):
        allow = '# count: 2\n' + self.ENTRY + 'rust-modules/src/b.rs\tc\tL0 gone\n'
        code, _, remaining = self.run_gate(self.UPWARD, LAYERS, allow, '--prune')
        self.assertEqual(code, 0)
        self.assertIn(self.ENTRY, remaining)
        self.assertNotIn('b.rs', remaining)
        self.assertTrue(remaining.startswith('# count: 1\n'))
        self.assertEqual(self.run_gate(self.UPWARD, allow=remaining)[0], 0)

    def test_the_declared_count_must_match_the_entries(self):
        code, out, _ = self.run_gate(self.UPWARD, allow='# count: 2\n' + self.ENTRY)
        self.assertEqual(code, 1)
        self.assertIn('declares count 2 but has 1', out)

    def test_a_module_in_no_layer_fails(self):
        code, out, _ = self.run_gate(dict(self.CLEAN, **{'lib.rs': LIB + 'mod d;', 'd.rs': ''}), allow='# count: 0\n')
        self.assertEqual(code, 1)
        self.assertIn('module d belongs to no layer', out)

    def test_a_submodule_member_overrides_its_parent(self):
        layers = LAYERS.replace('members = c', 'members = c b::core')
        files = dict(self.CLEAN, **{'b.rs': 'mod core; fn x() { crate::a::f(); }', 'b/core.rs': 'fn y() { crate::b::x(); }'})
        code, out, _ = self.run_gate(files, layers, '# count: 0\n')
        self.assertEqual(code, 1)
        self.assertIn('b/core.rs names b ([high]), which [low] may not use', out)
        self.assertNotIn('b.rs names a', out)

    def test_cfg_test_items_named_across_layers_are_reported_not_failed(self):
        files = dict(self.CLEAN, **{
            'c.rs': '#[cfg(test)] pub fn helper() {} #[cfg(test)] pub mod support {}',
            'b.rs': '#[cfg(test)] pub fn own() {}',
            'a.rs': '#[cfg(test)] mod t { fn x() { crate::c::helper(); crate::b::own(); } use crate::c::support; }'})
        code, out, _ = self.run_gate(files, allow='# count: 0\n')
        self.assertEqual(code, 0, out)
        self.assertIn('2 cfg(test) items still named across layers', out)
        code, out, _ = self.run_gate(files, LAYERS, '# count: 0\n', '--report')
        self.assertEqual(code, 0, out)
        self.assertIn('[low] 2 items\n    c::helper x1 from high\n    c::support x1 from high\n', out)
        self.assertNotIn('b::own', out, 'a same-layer cfg(test) name is not a split hazard')
        self.assertIn('low        2 cfg(test) items named across layers, here or below', out)
        code, out, _ = self.run_gate(self.CLEAN, LAYERS, '# count: 0\n', '--report')
        self.assertIn('low        ready', out)

    PORT = LAYERS + '[port p]\nmembers = b c\n'

    def test_a_reference_into_a_port_from_outside_it_fails_even_down_the_graph(self):
        code, out, _ = self.run_gate(self.CLEAN, self.PORT, '# count: 0\n')
        self.assertEqual(code, 1)
        self.assertIn('a.rs names c from outside [port p], which only the port itself may name: a.rs:1 c::g', out)
        self.assertIn('a.rs names b from outside [port p]', out)
        self.assertNotIn('names a', out, 'the port may name what its layer may')

    def test_port_entries_are_allowlisted_pruned_and_do_not_hold_up_the_split(self):
        entries = 'rust-modules/src/a.rs\tb\tL0 port step\nrust-modules/src/a.rs\tc\tL0 port step\n'
        code, out, _ = self.run_gate(self.CLEAN, self.PORT, '# count: 2\n' + entries)
        self.assertEqual(code, 0, out)
        self.assertIn('2 migration entries left in', out)
        self.assertIn('2 of them into [port p]', out)
        code, _, remaining = self.run_gate(self.CLEAN, self.PORT, '# count: 2\n' + entries, '--prune')
        self.assertEqual((code, remaining), (0, gate.ALLOW_HEADER.format(count=2) + entries))
        code, out, _ = self.run_gate(self.CLEAN, self.PORT, '# count: 2\n' + entries, '--report')
        self.assertIn('port p: 2 (file, member) entries, 2 references (2 production)', out)
        self.assertIn('    low        ready', out)
        self.assertIn('    high       ready', out)
        code, out, _ = self.run_gate(dict(self.CLEAN, **{'a.rs': ''}), self.PORT, '# count: 2\n' + entries)
        self.assertEqual(code, 1)
        self.assertIn('stale allowlist entry rust-modules/src/a.rs\tc — it no longer names that layer or port', out)

    def test_port_config_errors_fail(self):
        for layers, message in [
            (LAYERS + '[port p]\nmembers = ghost\n', 'ghost is listed in [port p] but is not a module'),
            (LAYERS + '[port p]\nmembers = c\n[port q]\nmembers = c\n', 'c is listed in [port p] and [port q]'),
            (LAYERS + '[port p]\nmembers = c\nuses = low\n', "[port p] has unknown key(s) ['uses']"),
        ]:
            with self.subTest(message=message):
                code, out, _ = self.run_gate(self.CLEAN, layers, '# count: 0\n')
                self.assertEqual(code, 1)
                self.assertIn(message, out)

    def test_layer_config_errors_fail(self):
        for layers, message in [
            ('[low]\nuses = high\nmembers = c\n[high]\nuses = low\nmembers = crate a b\n', 'layer cycle: high low'),
            ('[low]\nuses = nowhere\nmembers = c\n[high]\nuses = low\nmembers = crate a b\n', 'uses unknown layer nowhere'),
            (LAYERS.replace('members = c', 'members = c ghost'), 'ghost is listed in [low] but is not a module'),
            (LAYERS.replace('members = c', 'members = c a'), 'a is listed in [low] and [high]'),
        ]:
            with self.subTest(message=message):
                code, out, _ = self.run_gate(self.CLEAN, layers, '# count: 0\n')
                self.assertEqual(code, 1)
                self.assertIn(message, out)


if __name__ == '__main__':
    unittest.main()
