#!/usr/bin/env python3
"""ci/check-placeholders.py's verdicts on synthetic trees. Every case but one runs on a
synthetic tree in a private temp dir; `test_the_real_tree_names_every_required_site` and
`test_the_gate_is_run_by_make_check` read the REAL tree (read-only).

The classes `Defeated` and `Hooks` are the ways the first version of the gate was beaten on a
synthetic tree (a counter 30 lines away in another function, a comment, a file named `latest.rs`,
`CARD_ABSENT`, a third spinner between two counted ones); each of those cases fails on that version."""
import contextlib
import importlib.util
import io
import tempfile
import unittest
from pathlib import Path

_spec = importlib.util.spec_from_file_location('check_placeholders', Path(__file__).with_name('check-placeholders.py'))
gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gate)


def make_tree(tmp, files):
    for name, source in files.items():
        path = Path(tmp) / 'rust-modules' / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(source)


def verdicts(files):
    """[(file, line, text, verdict)] for a tree made of `files` ({relative path: source})."""
    with tempfile.TemporaryDirectory(prefix='check-placeholders-') as tmp:
        make_tree(tmp, files)
        return gate.scan(tmp)


def only(files):
    found = verdicts(files)
    assert len(found) == 1, found
    return found[0][3]


def fn(body):
    return f'fn f(p: P) {{\n{body}\n}}\n'


class References(unittest.TestCase):
    def test_a_bare_skeleton_paint_fails(self):
        self.assertEqual(only({'ui/src/a.rs': fn('    p.rect(r, 1.0, theme::SKELETON_TOP, theme::SKELETON_BOT, 0.0);')}),
                         'UNACCOUNTED')

    def test_every_watched_name_is_watched(self):
        for line in ('theme::SKELETON_TOP', 'theme::SKELETON_BOT', 'theme::CARD_PLACEHOLDER', 'STALE_ALPHA',
                     'theme::CARD_ABSENT', 'theme::white(SKEL_BAR_A)', 'COOL_850',
                     'widgets::Spinner::new(1.0, 2.0, 3.0)', 'Spinner::leading(x, y)',
                     'let s = Spinner { r: 1.0, ..d };',
                     'plx_platform::i18n::msg::browse_home_loading_c()', 'msg::settings_audio_loading()',
                     'pub fn skeleton_bar(p: Painter) {', 'fn skeleton_sheet(p: Painter) {'):
            with self.subTest(line=line):
                self.assertEqual(only({'ui/src/a.rs': f'fn f() {{ {line} }}\n'}), 'UNACCOUNTED')

    def test_a_counter_beside_it_in_the_same_function_passes(self):
        src = fn('    plx_ui::placeholder::note(p, Reason::DetailSpinner, "");\n    Spinner::new(1.0, 2.0, 3.0).draw(e, p);')
        self.assertEqual(only({'screens/src/a.rs': src}), 'counted')

    def test_the_counter_may_come_after_the_reference(self):
        src = fn('    Spinner::new(1.0, 2.0, 3.0);\n\n\n    placeholder::note(p, r, "");')
        self.assertEqual(only({'a.rs': src}), 'counted')

    def test_every_counting_wrapper_counts(self):
        for call in ('note(p, r, "")', 'skeleton(p, r, "", rect, 4.0)', 'ground(p, r, "", rect, 4.0)',
                     'flat(p, r, "", rect, 4.0)', 'face(p, r, "", rect, 4.0, a, b)', 'ink(p, r, "", col)'):
            with self.subTest(call=call):
                src = fn(f'    placeholder::{call};\n    theme::CARD_PLACEHOLDER;')
                self.assertEqual(only({'ui/src/a.rs': src}), 'counted')

    def test_a_helper_that_does_not_count_is_not_a_counter(self):
        for call in ('stale_cover(p, r, 4.0)', 'painter(p)', 'sentinel_fill(p, r, 4.0)', 'note_absent(p, r, "")'):
            with self.subTest(call=call):
                src = fn(f'    placeholder::{call};\n    STALE_ALPHA;')
                self.assertEqual(only({'ui/src/a.rs': src}), 'UNACCOUNTED')

    def test_a_reasoned_exemption_passes_and_a_bare_one_fails(self):
        self.assertEqual(only({'a.rs': fn('    // placeholder-exempt: an inline busy mark\n    Spinner::leading(x, y);')}), 'exempt')
        self.assertEqual(only({'a.rs': fn('    // placeholder-exempt:\n    Spinner::leading(x, y);')}), 'exempt-without-reason')

    def test_a_module_level_reference_looks_a_few_lines_up(self):
        self.assertEqual(only({'a.rs': '// placeholder-exempt: absence\nstatic S: Spinner = Spinner::new(1.0, 2.0, 3.0);\n'.replace('static S: Spinner = ', 'let _ = ')}), 'exempt')
        far = '// placeholder-exempt: absence\n' + '\n' * 6 + 'let _ = Spinner::new(1.0, 2.0, 3.0);\n'
        self.assertEqual(only({'a.rs': far}), 'UNACCOUNTED')


class Defeated(unittest.TestCase):
    """Each of these passed the first gate."""

    def test_a_counter_in_another_function_does_not_count(self):
        src = ('fn one(p: P) {\n    placeholder::note(p, r, "");\n}\n'
               'fn two(p: P) {\n    Spinner::new(1.0, 2.0, 3.0);\n}\n')
        self.assertEqual([f[3] for f in verdicts({'screens/src/login.rs': src})], ['UNACCOUNTED'])

    def test_a_third_spinner_between_two_counted_ones_fails(self):
        src = ('fn f(p: P, a: bool) {\n'
               '    if a {\n        placeholder::note(p, r, "a");\n        Spinner::new(1.0, 2.0, 3.0);\n    }\n'
               '    Spinner::new(4.0, 5.0, 6.0);\n'
               '    placeholder::note(p, r, "b");\n    Spinner::new(7.0, 8.0, 9.0);\n}\n')
        self.assertEqual([f[3] for f in verdicts({'a.rs': src})], ['counted', 'counted', 'UNACCOUNTED'])

    def test_an_exempt_spinner_does_not_use_up_a_counter(self):
        src = fn('    placeholder::note(p, r, "");\n    Spinner::new(1.0, 2.0, 3.0);\n'
                 '    // placeholder-exempt: an inline busy mark\n    Spinner::leading(x, y);')
        self.assertEqual([f[3] for f in verdicts({'a.rs': src})], ['counted', 'exempt'])

    def test_a_trailing_comment_is_not_a_counter(self):
        src = fn('    p.rrect(r, 8.0, 8.0, theme::CARD_PLACEHOLDER); // TODO placeholder::note(p, r, "")')
        self.assertEqual(only({'a.rs': src}), 'UNACCOUNTED')

    def test_a_block_comment_or_a_string_is_not_a_counter(self):
        for text in ('/* placeholder::note(p, r, "") */', 'let _s = "placeholder::note(p, r, \\"\\")";',
                     'let _s = r#"placeholder::note(p, r, "")"#;'):
            with self.subTest(text=text):
                self.assertEqual(only({'a.rs': fn(f'    {text}\n    STALE_ALPHA;')}), 'UNACCOUNTED')

    def test_a_trailing_exemption_marker_still_counts_as_a_comment(self):
        src = fn('    Spinner::new(1.0, 2.0, 3.0); // placeholder-exempt: an inline busy mark')
        self.assertEqual(only({'a.rs': src}), 'exempt')

    def test_a_file_that_merely_contains_test_in_its_name_is_scanned(self):
        for name in ('ui/src/latest.rs', 'ui/src/contest.rs', 'ui/src/testpat.rs', 'ui/src/attestation.rs'):
            with self.subTest(name=name):
                self.assertEqual(only({name: fn('    theme::SKELETON_TOP;')}), 'UNACCOUNTED')

    def test_absence_is_declared_not_counted(self):
        beside_a_counter = fn('    placeholder::note(p, r, "");\n    p.rrect_sheened(r, 8.0, theme::CARD_ABSENT);')
        self.assertEqual(only({'a.rs': beside_a_counter}), 'UNACCOUNTED')
        declared = fn('    // placeholder-exempt: absence, nothing will arrive\n    p.rrect_sheened(r, 8.0, theme::CARD_ABSENT);')
        self.assertEqual(only({'a.rs': declared}), 'exempt')

    def test_a_raw_skeleton_colour_and_a_spinner_literal_are_watched(self):
        self.assertEqual(only({'a.rs': fn('    let c = theme::white(SKEL_BAR_A);')}), 'UNACCOUNTED')
        self.assertEqual(only({'a.rs': fn('    let s = Spinner { r: 6.0, ..Spinner::default() };')}), 'UNACCOUNTED')

    def test_the_spinner_type_itself_is_not_a_reference(self):
        src = 'pub struct Spinner {\n    pub r: f32,\n}\nimpl Spinner {\n}\nimpl View for Spinner {\n}\n'
        self.assertEqual(verdicts({'a.rs': src}), [])

    def test_inside_placeholder_rs_a_note_in_another_function_does_not_count(self):
        src = 'pub fn note() {\n    note(p, r, k);\n}\npub fn ground() {\n    theme::CARD_PLACEHOLDER;\n}\n'
        self.assertEqual([f[3] for f in verdicts({'ui/src/placeholder.rs': src})], ['UNACCOUNTED'])


class Skipped(unittest.TestCase):
    def test_comments_and_definitions_are_not_references(self):
        src = ('// SKELETON_TOP is the top stop\n/// see [`Spinner::new`] and msg::x_loading\n'
               'pub const STALE_ALPHA: f32 = 0.35;\nconst CARD_PLACEHOLDER: [f32; 4] = X;\n'
               'pub(crate) const SKELETON_BOT: [f32; 4] = Y;\nstatic COOL_850: [f32; 4] = Z;\n')
        self.assertEqual(verdicts({'a.rs': src}), [])

    def test_layout_names_of_the_spinner_draw_nothing(self):
        src = 'fn f() { Spinner::inline_gutter(); Spinner::R_PAGE; Spinner::dot_r(3.0); Spinner::PERIOD_MS; }\n'
        self.assertEqual(verdicts({'a.rs': src}), [])

    def test_an_inline_test_module_is_skipped_but_code_after_it_is_not(self):
        src = ('#[cfg(test)]\nmod tests {\n    fn t() { Spinner::new(1.0, 2.0, 3.0); }\n}\n'
               'fn after() { Spinner::new(1.0, 2.0, 3.0); }\n')
        found = verdicts({'a.rs': src})
        self.assertEqual([(f[1], f[3]) for f in found], [(5, 'UNACCOUNTED')])

    def test_a_declared_test_module_does_not_hide_the_file(self):
        src = '#[cfg(test)]\nmod tests;\nfn f() { Spinner::new(1.0, 2.0, 3.0); }\n'
        self.assertEqual(only({'a.rs': src}), 'UNACCOUNTED')

    def test_test_and_harness_files_are_skipped(self):
        for name in ('ui/src/widgets_tests.rs', 'screens/src/library/cards_harness.rs', 'a/tests.rs', 'a/b_test.rs',
                     'a/tests/case.rs', 'a/tests.rs'):
            with self.subTest(name=name):
                self.assertEqual(verdicts({name: 'fn f() { Spinner::new(1.0, 2.0, 3.0); }\n'}), [])


class Lexer(unittest.TestCase):
    def test_lifetimes_char_literals_and_braces_in_strings_do_not_confuse_a_function_body(self):
        src = ('fn a<\'x>(s: &\'x str) -> char {\n    let _c = \'{\';\n    let _d = "}";\n    placeholder::note(p, r, s);\n    \'a\'\n}\n'
               'fn b() {\n    Spinner::new(1.0, 2.0, 3.0);\n}\n')
        self.assertEqual([f[3] for f in verdicts({'a.rs': src})], ['UNACCOUNTED'])

    def test_a_trait_declaration_has_no_body_and_no_span(self):
        src = 'trait T {\n    fn one(&self);\n}\nfn two() {\n    placeholder::note(p, r, "");\n    Spinner::new(1.0, 2.0, 3.0);\n}\n'
        self.assertEqual([f[3] for f in verdicts({'a.rs': src})], ['counted'])

    def test_nested_functions_use_the_innermost(self):
        src = ('fn outer() {\n    placeholder::note(p, r, "");\n    fn inner() {\n        Spinner::new(1.0, 2.0, 3.0);\n    }\n}\n')
        self.assertEqual([f[3] for f in verdicts({'a.rs': src})], ['UNACCOUNTED'])


ENUM = 'pub enum Reason {\n    CardSkeleton,\n    HomeBackdrop,\n    HeroLogoText,\n}\n'


class Hooks(unittest.TestCase):
    def problems(self, files):
        with tempfile.TemporaryDirectory(prefix='check-placeholders-') as tmp:
            make_tree(tmp, files)
            return gate.hook_problems(tmp)

    def test_a_reason_with_no_hook_left_fails(self):
        files = {'ui/src/placeholder.rs': ENUM,
                 'ui/src/widgets.rs': 'fn f() { note(p, Reason::CardSkeleton, ""); }\n',
                 'screens/src/home/mod.rs': 'fn g() { note(p, Reason::HomeBackdrop, ""); }\n'}
        out = self.problems(files)
        self.assertTrue(out and all('HeroLogoText' in m for m in out), out)

    def test_a_hook_that_survives_only_in_a_comment_or_a_test_does_not_count(self):
        files = {'ui/src/placeholder.rs': ENUM,
                 'ui/src/widgets.rs': 'fn f() { note(p, Reason::CardSkeleton, ""); }\n',
                 'screens/src/home/mod.rs': ('fn g() { note(p, Reason::HomeBackdrop, ""); }\n'
                                             '// note(p, Reason::HeroLogoText, "")\n'
                                             '#[cfg(test)]\nmod t {\n    fn x() { Reason::HeroLogoText; }\n}\n'),
                 'ui/src/hero_logo_tests.rs': 'fn x() { Reason::HeroLogoText; }\n'}
        out = self.problems(files)
        self.assertTrue(out and all('HeroLogoText' in m for m in out), out)

    def test_every_reason_named_is_green(self):
        files = {'ui/src/placeholder.rs': ENUM,
                 'ui/src/widgets.rs': 'fn f() { note(p, Reason::CardSkeleton, ""); }\n',
                 'screens/src/home/mod.rs': 'fn g() { note(p, Reason::HomeBackdrop, ""); }\n',
                 'ui/src/hero_logo.rs': 'fn h() { ink(p, Reason::HeroLogoText, "", c); }\n'}
        self.assertEqual(self.problems(files), [])

    def test_a_required_site_must_stay_in_its_file(self):
        files = {'ui/src/placeholder.rs': ENUM,
                 'ui/src/widgets.rs': ('fn f() { note(p, Reason::CardSkeleton, ""); }\n'
                                       'fn g() { note(p, Reason::HomeBackdrop, ""); }\n'),
                 'ui/src/hero_logo.rs': 'fn h() { ink(p, Reason::HeroLogoText, "", c); }\n'}
        out = self.problems(files)
        self.assertEqual(len(out), 1, out)
        self.assertIn('screens/src/home/mod.rs must name Reason::HomeBackdrop', out[0])

    def test_the_real_tree_names_every_required_site(self):
        self.assertEqual(gate.hook_problems(gate.REPO), [])
        self.assertTrue({v for _, v in gate.REQUIRED} <= set(gate.reason_names(gate.REPO / gate.ROOT_REL)))


class Gate(unittest.TestCase):
    def test_the_gate_is_run_by_make_check(self):
        steps = (gate.REPO / 'ci' / 'check-python-steps.txt').read_text().split('\n')
        self.assertIn('python3 ci/check-placeholders.py', [l.strip() for l in steps],
                      'dropping the gate from the check steps disables it silently')


    def run_main(self, files):
        with tempfile.TemporaryDirectory(prefix='check-placeholders-') as tmp:
            make_tree(tmp, files)
            out, err = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                code = gate.main(['--root', tmp])
            return code, out.getvalue(), err.getvalue()

    def test_exit_codes_and_the_failure_names_the_line(self):
        code, out, err = self.run_main({'a.rs': 'fn f() {\n    Spinner::new(1.0, 2.0, 3.0);\n}\n'})
        self.assertEqual(code, 1)
        self.assertIn('a.rs:2:', err)
        self.assertIn('Spinner::new(', err)
        code, out, err = self.run_main({'a.rs': 'fn f() {\n    // placeholder-exempt: why\n    Spinner::new(1.0, 2.0, 3.0);\n}\n'})
        self.assertEqual((code, err), (0, ''))
        self.assertIn('1 references', out)

    def test_a_missing_hook_fails_the_gate(self):
        code, _, err = self.run_main({'ui/src/placeholder.rs': ENUM})
        self.assertEqual(code, 1)
        self.assertIn('missing hook', err)


if __name__ == '__main__':
    unittest.main()
