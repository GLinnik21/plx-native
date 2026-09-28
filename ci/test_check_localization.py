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
            KeyHint::new(c"", c"BACK", c"");
            Label::new(c"…".as_ptr(), 24, ink);
        }'''
        self.assertEqual(self.texts(source), set())

    def test_diagnostic_values_and_player_captions_are_covered(self):
        source = '''fn error() { ErrorShape { caption: c"Playback failed", kind: Failure::Load,
            readout: "Cannot connect", panel: "No video", detail: server.reason() } }
        fn draw() { Field::new("Server", "Unavailable"); }'''
        self.assertEqual(self.texts(source), {'Playback failed', 'Cannot connect', 'No video', 'Server', 'Unavailable'})

    def test_utf8_and_escaped_quotes_survive_tokenization(self):
        source = 'fn draw(){Row::new("Інфармацыя ' + chr(92) + 'u{0406}");}'
        self.assertEqual(self.texts(source), {'Інфармацыя І'})
        source = 'fn draw(){Row::new("Title ' + chr(92) + '"quoted' + chr(92) + '"");}'
        self.assertEqual(self.texts(source), {'Title "quoted"'})

    def test_diagnostic_field_names_are_on_screen_too(self):
        source = '''fn rows() { Field::new("Connection", server.value());
            Field::new(msg::browse_diagnostics_field_video(), "—"); }'''
        self.assertEqual(self.texts(source), {'Connection'})

    def test_sign_in_failures_are_stored_product_text(self):
        source = '''const UNREACHABLE: &str = "Couldn't load profiles";
        fn fail_login(&mut self, message: &str) { self.state.error = message.to_owned(); }
        fn step(&mut self) {
            self.fail_login("Couldn't start sign-in", None, emit);
            self.fail_empty_home_roster(UNREACHABLE);
            self.state.error = "Couldn't switch profile".into();
            self.state.error = msg::browse_auth_switch_retry().into();
            fail_login(msg::browse_auth_signed_out(), None, emit);
        }'''
        self.assertEqual(self.texts(source),
                         {"Couldn't start sign-in", "Couldn't load profiles", "Couldn't switch profile"})

    def test_playback_verdicts_are_stored_product_text(self):
        source = '''fn plan() {
            plan.verdict = Some(format!("Force Direct Play is enabled. {why}"));
            ps.play_verdict = Some("Audio needs conversion".into());
            plan.verdict = Some(PlayVerdict::Server(v));
        }'''
        self.assertEqual(self.texts(source), {'Force Direct Play is enabled. {why}', 'Audio needs conversion'})

    def test_subtitle_engine_faults_are_covered_and_err_only_in_its_file(self):
        source = '''fn render() -> Result<(), &'static str> {
            let frame = error_frame(key, "Couldn't render this subtitle");
            if bad { return Err("This subtitle track is too large"); }
            Ok(())
        }'''
        self.assertEqual(self.texts(source), {"Couldn't render this subtitle"})
        self.assertEqual({f.text for f in checker.scan(source, {'Err': (0,)})},
                         {"Couldn't render this subtitle", 'This subtitle track is too large'})

    def test_hud_kicker_busy_readout_and_tile_labels_are_boundaries(self):
        source = '''fn draw() {
            let k = Kicker::Context(c"Trailer".as_ptr());
            let b = Busy::Readout(StatusKind::Working, c"Buffering…");
            let t = TileLabel::titled("Title", "Caption");
            let ok = Kicker::Context(msg::browse_detail_trailer_c().as_ptr());
        }'''
        self.assertEqual(self.texts(source), {'Trailer', 'Buffering…', 'Title', 'Caption'})

    def test_another_modules_prose_constant_is_followed(self):
        consts = checker.prose_consts('''pub(crate) const REFUSED: &str = "Switching profiles isn't available";
            pub(crate) const KEY: &str = "plx:context/trailer"; const N: usize = 3;''')
        self.assertEqual(consts, {'REFUSED': "Switching profiles isn't available", 'KEY': 'plx:context/trailer'})
        source = '''fn step(&mut self) { self.state.error = owner::REFUSED.into(); log(owner::KEY); }'''
        self.assertEqual({f.text for f in checker.scan(source, None, consts)}, {"Switching profiles isn't available"})

    def test_patterns_comparisons_and_parameters_are_not_text_flow(self):
        source = '''fn plan(title: &str, codec: &str) {
            let dv = decide(codec == "hevc");
            let failure = if dv { Some(Failure::Video) } else { None };
            if let Some(v) = refusal(&mc) { log(&format!("refused {v}")); }
            plan.verdict = Some(PlayVerdict::Forced(failure));
            plan.verdict = Some(PlayVerdict::Server(v));
        }'''
        self.assertEqual(self.texts(source), set())

    def test_title_field_initialisers_are_boundaries(self):
        source = '''fn tabs() -> Tab { Tab { title: "Other".to_string(), key: "other" } }'''
        self.assertEqual(self.texts(source), {'Other'})

    def test_linked_headings_and_poster_marks_are_boundaries(self):
        source = '''fn draw() {
            LinkedHeading::entry("Filmography", count).draw(p, x, y, 0.0, &m, measure);
            LinkedHeading::heading(&shelf.title, "items").bounded(w);
            let mark = format!("SEASON {}", n);
            widgets::poster_label(p, rect, radius, &mark, measure);
            widgets::poster_label(p, rect, radius, &msg::browse_collection_season_mark(n), measure);
        }'''
        self.assertEqual(self.texts(source), {'Filmography', 'items', 'SEASON {}'})

if __name__ == '__main__':
    unittest.main()
