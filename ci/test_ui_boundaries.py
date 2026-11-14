#!/usr/bin/env python3
"""Positive/negative controls for screen IO and retained command boundaries."""
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
import ui_boundaries as boundary


class BoundaryTests(unittest.TestCase):
    def test_direct_trigger_and_file_reads_fail(self):
        for source in ('fn tick() { plx_base::devtrig::read("x"); }',
                       'fn tick() { std::fs::read("x"); }',
                       'fn tick() { std::fs::read_to_string("x"); }'):
            with self.subTest(source=source):
                self.assertTrue(boundary.screen_violations(source))

    def test_helper_wrapper_does_not_hide_io(self):
        source = 'fn tick() { helper(); } fn helper() { std::fs::read("x"); }'
        self.assertTrue(boundary.screen_violations(source))

    def test_import_alias_and_function_value_fail(self):
        for source in ('use std::fs as disk; fn helper() { disk::read("x"); }',
                       'use std::{fs as disk};',
                       'use plx_base::devtrig::{read as captured};',
                       'fn helper() { let reader = std::fs::read; }',
                       'use std::fs::File; fn helper() { File::open("x"); }'):
            with self.subTest(source=source):
                self.assertTrue(boundary.screen_violations(source))

    def test_shipping_disabled_developer_io_still_fails(self):
        for cfg in ('feature = "devtriggers"', 'all(feature = "hostsim", not(test))',
                    'any(test, feature = "devtriggers")'):
            self.assertTrue(boundary.screen_violations(f'#[cfg({cfg})] fn helper() {{ std::fs::read("x"); }}'))

    def test_test_only_items_and_literal_decoys_pass(self):
        source = '''
        // std::fs::read("comment")
        fn pure() { let s = r#"std::fs::read("data")"#; }
        #[cfg(all(test, feature = "hostsim"))]
        #[allow(dead_code)] mod fixtures { fn seed() { std::fs::read("x"); } }
        #[cfg(any(test, feature = "test-support"))]
        fn fixture() { plx_base::devtrig::read("x"); }
        fn after_fixture() { consume(captured_policy); }
        '''
        self.assertEqual(boundary.screen_violations(source), [])

    def test_production_after_test_item_is_not_skipped(self):
        source = '#[cfg(test)] mod tests { fn fixture() {} } fn helper() { std::fs::read("x"); }'
        self.assertTrue(boundary.screen_violations(source))

    def test_external_test_module_is_classified_not_named(self):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'lib.rs').write_text('#[cfg(test)] #[path="fixture.rs"] mod tests;')
            (root / 'fixture.rs').write_text('fn seed() { std::fs::read("x"); }')
            self.assertEqual(boundary.check_screens(root), [])
            (root / 'lib.rs').write_text('mod fixture;')
            self.assertTrue(boundary.check_screens(root))

    def test_orphan_named_tests_is_not_exempt(self):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'hidden_tests.rs').write_text('fn seed() { std::fs::read("x"); }')
            self.assertTrue(boundary.check_screens(root))

    def test_io_port_outside_screen_root_is_not_a_screen(self):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'screens').mkdir()
            (root / 'screens/lib.rs').write_text('fn pure() { consume(policy); }')
            (root / 'worker.rs').write_text('fn job(off_frame: &OffFrame) { std::fs::read("x"); }')
            self.assertEqual(boundary.check_screens(root / 'screens'), [])

    def test_transient_consumed_request_vector_is_not_retained_storage(self):
        self.assertEqual(boundary.command_storage_violations('fn execute(reqs: Vec<PlayerReq>) { for req in reqs {} }'), [])
        self.assertEqual(boundary.command_storage_violations('struct Mail { pending: Vec<Arrival> }'), [])
        self.assertEqual(boundary.command_storage_violations('struct Mail { pending: Vec<Arrival>, current: HomeReq }'), [])
        self.assertEqual(boundary.command_storage_violations('struct Host { #[cfg(test)] probe: Vec<HomeReq>, value: u32 }'), [])
        self.assertEqual(boundary.command_storage_violations('#[cfg(test)] struct Probe { reqs: Vec<LoopReq> }'), [])

    def test_new_family_storage_fails_without_old_field_name(self):
        for source in ('struct Bridge { renamed: VecDeque<(Source, HomeReq)> }',
                       'fn helper() { let renamed: Vec<LoopReq> = Vec::new(); }'):
            self.assertTrue(boundary.command_storage_violations(source))

    def test_animation_probe_only_emits_diagnostic_facts(self):
        source = Path('rust-modules/ui/src/anim.rs').read_text(encoding='utf-8')
        self.assertEqual(boundary.screen_violations(source), [])
        text = boundary.code(boundary.production_tokens(source))
        for forbidden in ('storage_worker::', 'task::spawn', 'eventlog::append_record'):
            self.assertNotIn(forbidden, text)
        self.assertIn('pub fn take_settle_log', text)
        runner = Path('rust-modules/src/app/run.rs').read_text(encoding='utf-8')
        self.assertIn('super::anim_diagnostics::drain()', runner)

    def test_card_motion_takes_owner_captured_policy(self):
        source = Path('rust-modules/ui/src/card_motion.rs').read_text(encoding='utf-8')
        self.assertEqual(boundary.screen_violations(source), [])
        text = boundary.code(boundary.production_tokens(source))
        self.assertNotIn('OnceLock', text)
        self.assertIn('pub struct AdmissionPolicy', text)
        runner = Path('rust-modules/src/app/run.rs').read_text(encoding='utf-8')
        self.assertIn('app.bridge.card_motion_policy.enter(app.boot_initial.is_some())', runner)

    def test_v10_ambient_preferences_and_intercrate_scrub_aliases_fail(self):
        for source in (
            'fn construct() { plx_media::route::quality(); }',
            'fn tick() { plx_media::player::subtitle_tone(); }',
            'fn resolve_value() { route::subtitle_size(); }',
            'use plx_media::route as playback; fn resolve_value() { playback::subtitle_size(); }',
            'use plx_media::{route as playback}; fn resolve_value() { playback::subtitle_size(); }',
            'fn resolve_value() { let read = plx_media::route::subtitle_size; }',
            'fn field_readout() { SubtitlePositionPref::live(); }',
            'fn mount() { self.build_rows(plx_media::route::deck_press()); }',
            'use plx_media::route::{deck_press as pick}; fn mount() { pick(); }',
            'fn event() { plx_appkit::player_hud::scrub_step_ns(); }',
            'pub use plx_appkit::player_hud::scrub_step_ns; fn event() { scrub_step_ns(); }',
            'use plx_appkit::player_hud::scrub_step_ns as hop; fn event() { hop(); }',
            'fn event() { input::scrub_step_ns(); }',
            'fn event() { let hop = plx_appkit::player_hud::scrub_step_ns; hop(); }',
            'pub fn scrub_step_ns() -> i64 { plx_media::route::skip_interval().ns() }',
        ):
            with self.subTest(source=source):
                self.assertTrue(boundary.screen_violations(source))

    def test_v10_captured_typed_preferences_and_pure_scrub_pass(self):
        for source in (
            'fn mount() { self.build_rows(inputs.preference::<DeckPressPref>()); }',
            'fn resolve_value() { local.preference::<SubtitleSizePref>(); }',
            'fn event() { input::scrub_step_ns(inputs.preference::<SkipIntervalPref>()); }',
            'use plx_appkit::player_hud::scrub_step_ns as hop; fn event() { hop(pick); }',
            'pub use plx_appkit::player_hud::scrub_step_ns; fn event() { scrub_step_ns(pick); }',
            'pub fn scrub_step_ns(pick: SkipInterval) -> i64 { pick.ns() }',
            '#[cfg(test)] fn fixture() { plx_media::route::deck_press(); }',
        ):
            with self.subTest(source=source):
                self.assertEqual(boundary.screen_violations(source), [])

    def test_shared_scrub_helper_uses_the_same_persistent_guard(self):
        source = Path('rust-modules/appkit/src/player_hud/state.rs').read_text(encoding='utf-8')
        self.assertEqual(boundary.scrub_helper_violations(source), [])
        for source in (
            'pub fn scrub_step_ns() -> i64 { plx_media::route::skip_interval().ns() }',
            'use plx_media::route as playback; pub fn scrub_step_ns(pick: SkipInterval) -> i64 { playback::skip_interval().ns() }',
            'use plx_media::route::{skip_interval as hop}; pub fn scrub_step_ns(pick: SkipInterval) -> i64 { hop().ns() }',
        ):
            with self.subTest(source=source):
                self.assertTrue(boundary.scrub_helper_violations(source))
        self.assertEqual(boundary.scrub_helper_violations(
            'fn draw() { std::fs::read("port"); } pub fn scrub_step_ns(pick: SkipInterval) -> i64 { pick.ns() }'), [])

    def test_adjacent_root_install_and_profile_aliases_fail(self):
        for source in (
            'impl RootPage { fn rebuild() { plx_platform::i18n::saved_preference(); } }',
            'impl RootPage { fn rebuild() { plx_media::route::under_posters(); } }',
            'use plx_media::route as playback; impl RootPage { fn rebuild() { playback::under_posters(); } }',
            'use plx_media::route::{under_posters as read}; impl RootPage { fn rebuild() { read(); } }',
            'use plx_platform::i18n::saved_preference as pick; impl LanguagePage { fn new() { pick(); } }',
            'impl LanguagePage { fn pending() { i18n::current().preference(); } }',
        ):
            with self.subTest(source=source):
                self.assertTrue(boundary.screen_violations(source))
        self.assertEqual(boundary.screen_violations('impl RootPage { fn rebuild() { inputs.preference::<UnderPostersPref>(); inputs.install_language; } }'), [])
        self.assertEqual(boundary.screen_violations('impl LanguagePage { fn pending() { self.selected != self.running_language } }'), [])

    def test_adjacent_player_helper_transitive_reads_and_aliases_fail(self):
        for source in (
            'fn busy() { devtrig::flag("failtest"); }',
            'fn transport_hidden() { busy(); } fn busy() { sample(); } fn sample() { std::fs::read("x"); }',
            'use plx_media::player::error_now as shape; fn captured() { shape(ps); }',
            'fn draw_readout() { wrapper(); } fn wrapper() { plx_media::player::error_caption(ps); }',
            'fn busy_surface_with() { plx_media::player::audio_switching(); }',
            'use plx_media::player as engine; fn busy() { engine::state(ps); }',
            'use plx_media::player::error_captured as read; fn busy() { read(ps, captured); }',
        ):
            with self.subTest(source=source):
                self.assertTrue(boundary.presentation_helper_violations(source))
        self.assertEqual(boundary.presentation_helper_violations('fn busy(p: &Presentation) { p.busy } fn transport_hidden(p: &Presentation) { readout_owns_frame(p.busy); }'), [])
        self.assertEqual(boundary.presentation_helper_violations('fn capture() { devtrig::read("failtest"); player::error_now(ps); }'), [])
        self.assertEqual(boundary.presentation_helper_violations('#[cfg(test)] fn busy() { std::fs::read("fixture"); }'), [])

    def test_adjacent_machine_steps_cannot_resample_player_failure(self):
        for source in (
            'impl Machine for PlayerScreen { fn step() { player::error_now(ps); } }',
            'use plx_media::player::failure_context as context; fn step() { context(ps); }',
            'use plx_media::player as engine; fn helper() { let sample = engine::error_captured; }',
            'fn step() { wrapper(); } fn wrapper() { player::error_caption(ps); }',
        ):
            with self.subTest(source=source):
                self.assertTrue(boundary.screen_violations(source))
        self.assertEqual(boundary.screen_violations(
            'fn step() { transport_hidden(&inputs.player_presentation); inputs.player_presentation.failure; }'), [])

    def test_adjacent_production_helpers_keep_pure_fact_contract(self):
        for name in ('state.rs', 'classic.rs'):
            path = Path('rust-modules/appkit/src/player_hud') / name
            self.assertEqual(boundary.presentation_helper_violations(path.read_text(encoding='utf-8')), [])
        self.assertEqual(boundary.root_settings_violations(Path('rust-modules/screens/src/settings.rs').read_text(encoding='utf-8')), [])

    def test_check_deps_runs_screen_boundary_gate(self):
        script = Path('ci/check-deps.sh').read_text(encoding='utf-8')
        self.assertIn('python3 ci/ui_boundaries.py', script)


if __name__ == '__main__':
    unittest.main()
