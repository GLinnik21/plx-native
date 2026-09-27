#!/usr/bin/env python3
"""Regression tests for the source inventory's lexical and scope boundaries."""
import importlib.util
from pathlib import Path
import sys
import unittest

SPEC = importlib.util.spec_from_file_location('localization_inventory', Path(__file__).with_name('check-localization.py'))
checker = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = checker
SPEC.loader.exec_module(checker)

class InventoryTests(unittest.TestCase):
    def texts(self, source):
        return {finding.text for finding in checker.scan(source)}

    def test_direct_c_and_raw_literals_are_rejected(self):
        source = '''fn draw() {
            Row::new("Settings"); Label::new(c"Try again".as_ptr(), 24, ink);
            TextView::new(r#"Use "Back" to return"#, 24, ink);
        }'''
        self.assertEqual(self.texts(source), {'Settings', 'Try again', 'Use "Back" to return'})

    def test_visible_local_and_forward_constant_flow_are_rejected(self):
        source = '''fn draw() {
            let message = format!("Found {} movies", count);
            let cs = CString::new(message).unwrap();
            Label::new(cs.as_ptr(), 24, ink);
            Row::new(TITLE);
        }
        const TITLE: &str = "Library";'''
        self.assertEqual(self.texts(source), {'Found {} movies', 'Library'})

    def test_binding_scope_prevents_cross_function_and_shadow_leaks(self):
        source = '''fn diagnostic() { let title = "Technical log"; log(title); }
        fn draw() {
            let title = catalog::translated_title();
            { let title = "Unused temporary"; log(title); }
            Row::new(title);
        }'''
        self.assertEqual(self.texts(source), set())

    def test_test_items_and_nested_comments_are_not_product_copy(self):
        source = '''/* Row::new("Comment"); /* nested */ */
        #[cfg(test)] #[derive(Default)] struct Fake { title: String }
        #[cfg(test)] fn helper(a: i32, b: i32) { Row::new("Fixture"); }
        #[cfg(test)] mod tests { #[test] fn test_ui() { Row::new("Test"); } }
        struct State { #[cfg(test)] title: String, other: i32 }
        fn draw() {
            let s = State { #[cfg(test)] title: "Test".into(), other: 1 };
            // Row::new("Also a comment");
            Row::new(catalog::title());
        }'''
        self.assertEqual(self.texts(source), set())

    def test_protocol_and_server_strings_do_not_become_false_ui_messages(self):
        source = '''fn load() { log("Connecting"); request("/library/sections"); }
        fn draw() { Row::new(server.title()); TextView::new(msg::name(), size, ink);
            Field::new("Stable diagnostic schema", server.value());
            KeyHint::new(c"", c"BACK", c"");
            Label::new(c"…".as_ptr(), 24, ink);
        }'''
        self.assertEqual(self.texts(source), set())

    def test_diagnostic_values_and_player_captions_are_covered(self):
        source = '''fn error() { ErrorShape { caption: c"Playback failed", kind: Failure::Load,
            readout: "Cannot connect", panel: "No video", detail: server.reason() } }
        fn draw() { Field::new("Server", "Unavailable"); }'''
        self.assertEqual(self.texts(source), {'Playback failed', 'Cannot connect', 'No video', 'Unavailable'})

    def test_utf8_and_escaped_quotes_survive_tokenization(self):
        source = 'fn draw(){Row::new("Інфармацыя ' + chr(92) + 'u{0406}");}'
        self.assertEqual(self.texts(source), {'Інфармацыя І'})
        source = 'fn draw(){Row::new("Title ' + chr(92) + '"quoted' + chr(92) + '"");}'
        self.assertEqual(self.texts(source), {'Title "quoted"'})

if __name__ == '__main__':
    unittest.main()
