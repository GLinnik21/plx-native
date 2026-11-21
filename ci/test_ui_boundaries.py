#!/usr/bin/env python3
"""Positive/negative controls for screen IO and retained command boundaries."""
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
import ui_boundaries as boundary


class BoundaryTests(unittest.TestCase):
    def test_closed_arg_ladders_require_registry_registration(self):
        for source in (
            'fn word(arg: &AppArg) { match arg { AppArg::Home => "home", AppArg::Search => "search" } }',
            'fn probe() { helper(); } fn helper() { match app.route() { AppArg::Home => {}, AppArg::Settings(_) => {} } }',
            'use registry::AppArg as Renamed; fn helper(a: Renamed) { match a { Renamed::Home => 0, Renamed::Search => 1 } }',
        ):
            with self.subTest(source=source):
                self.assertTrue(boundary.external_arg_match_violations(source))

    def test_partial_resource_queries_and_descriptor_dispatch_pass(self):
        for source in (
            'match app.route() { AppArg::Player => play(), _ => idle() }',
            'match app.route().route_probe().kind { ProbeKind::Generic => generic(), ProbeKind::Player => player() }',
            '// match arg { AppArg::Home => 0 }\nfn word(a: &AppArg) { a.route_word() }',
            '#[cfg(test)] fn fixture() { match a { AppArg::Home => 0 } }',
        ):
            with self.subTest(source=source):
                self.assertEqual(boundary.external_arg_match_violations(source), [])

    def test_product_has_no_external_closed_arg_ladder(self):
        self.assertEqual(boundary.check_extension_registration(Path('.')), [])

    def test_page_census_cannot_move_to_an_external_array_or_wrapper(self):
        for source in (
            '#[cfg(test)] fn every_route() { vec![AppArg::Home] }',
            'fn every_route() { helper() } fn helper() { vec![AppArg::Home] }',
        ):
            self.assertTrue(boundary.page_census_violations(source))
        self.assertEqual(boundary.page_census_violations(
            '#[cfg(test)] fn every_route() { plx_screens::registry::every_page_arg() }'), [])

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

    def _extension_tree(self, directory, planted=None):
        """The three roots the closed-ladder gate reads, empty, with one file planted in one of them."""
        root = Path(directory)
        for rel in ('rust-modules/src', 'rust-modules/platform-webos/src', 'rust-modules/platform-desktop/src'):
            (root / rel).mkdir(parents=True)
            (root / rel / 'lib.rs').write_text('')
        if planted:
            rel, source = planted
            target = root / rel
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(source)
        return root

    def test_the_platform_crates_are_read_for_a_closed_arg_ladder(self):
        """W7b: `port.rs`, `system.rs`, `platform_desktop.rs` and `bin/sim.rs` left `rust-modules/src` for the two
        platform crates, and this gate read only `rust-modules/src`: a closed `AppArg` match in any of them passed.
        RED against the one-root gate: each platform crate's files fail, and are named."""
        ladder = 'fn word(arg: &AppArg) { match arg { AppArg::Home => "home", AppArg::Search => "search" } }'
        for rel in ('rust-modules/platform-webos/src/system.rs', 'rust-modules/platform-webos/src/webos/toast.rs',
                    'rust-modules/platform-desktop/src/bin/sim.rs', 'rust-modules/platform-desktop/src/lib.rs'):
            with self.subTest(file=rel), TemporaryDirectory() as directory:
                root = self._extension_tree(directory, (rel, ladder))
                errors = boundary.check_extension_registration(root)
                self.assertEqual(len(errors), 1, errors)
                self.assertIn(rel, errors[0])

    def test_the_application_crate_is_still_read_beside_the_platform_crates(self):
        ladder = 'fn word(arg: &AppArg) { match arg { AppArg::Home => "home", AppArg::Search => "search" } }'
        with TemporaryDirectory() as directory:
            root = self._extension_tree(directory, ('rust-modules/src/app/words2.rs', ladder))
            errors = boundary.check_extension_registration(root)
            self.assertEqual(len(errors), 1, errors)
            self.assertIn('rust-modules/src/app/words2.rs', errors[0])

    def test_a_platform_crate_with_no_ladder_passes(self):
        with TemporaryDirectory() as directory:
            root = self._extension_tree(directory, ('rust-modules/platform-webos/src/system.rs',
                                                    'fn word(arg: &AppArg) { arg.route_word() }'))
            self.assertEqual(boundary.check_extension_registration(root), [])

    def test_a_missing_platform_root_is_reported_not_read_as_no_ladder(self):
        with TemporaryDirectory() as directory:
            root = self._extension_tree(directory)
            (root / 'rust-modules/platform-webos/src/lib.rs').unlink()
            (root / 'rust-modules/platform-webos/src').rmdir()
            errors = boundary.check_extension_registration(root)
            self.assertEqual(len(errors), 1, errors)
            self.assertIn('rust-modules/platform-webos/src: a directory this gate reads is missing', errors[0])

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
                       'struct Bridge { renamed: Vec<ExternalAppFx> }',
                       'struct Bridge { renamed: Vec<ResourceAppFx> }',
                       'struct Bridge { renamed: Vec<InlineAppFx> }',
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
            'impl RootPage { fn rebuild() { plx_os::i18n::saved_preference(); } }',
            'impl RootPage { fn rebuild() { plx_media::route::under_posters(); } }',
            'use plx_media::route as playback; impl RootPage { fn rebuild() { playback::under_posters(); } }',
            'use plx_media::route::{under_posters as read}; impl RootPage { fn rebuild() { read(); } }',
            'use plx_os::i18n::saved_preference as pick; impl LanguagePage { fn new() { pick(); } }',
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

    def test_product_frame_owner_and_negative_controls(self):
        run = Path('rust-modules/src/app/run.rs').read_text(encoding='utf-8')
        dispatch = Path('rust-modules/ui/src/dispatch.rs').read_text(encoding='utf-8')
        self.assertEqual(boundary.product_frame_violations(run, dispatch), [])
        mutations = (
            (run.replace('Dispatcher::product_frame(&mut runtime);', 'update(app, fr);'), dispatch),
            (run.replace('Dispatcher::product_frame(&mut runtime);', 'Dispatcher::product_frame(&mut runtime); ingest(app, fr);'), dispatch),
            (run.replace('Dispatcher::product_frame(&mut runtime);', 'Dispatcher::product_frame(&mut runtime); runtime.prepare();'), dispatch),
            (run, dispatch.replace('runtime.ingest();', 'runtime.ingest(); runtime.ingest();')),
            (run, dispatch.replace('runtime.pump();', 'runtime.step();')),
            (run, dispatch.replace('if runtime.admit(demand) {', 'if true {')),
            (run, dispatch.replace('if swapped {', 'if true {')),
            (run, dispatch.replace('finish_presentation(swapped)', 'finish_presentation(true)')),
            (run.replace('fn finish_prepare(app: &mut App) {', 'fn finish_prepare(app: &mut App) { app.pages.present.acknowledge(0);'), dispatch),
        )
        for changed_run, changed_dispatch in mutations:
            with self.subTest(change=mutations.index((changed_run, changed_dispatch))):
                self.assertTrue(boundary.product_frame_violations(changed_run, changed_dispatch))
        # Comments, strings and test-only decoys cannot satisfy the production owner pin.
        bad = run.replace('Dispatcher::product_frame(&mut runtime);', 'legacy();')
        decoys = '\n// Dispatcher::product_frame(&mut runtime);\n#[cfg(test)] fn decoy() { Dispatcher::product_frame(&mut runtime); }'
        self.assertTrue(boundary.product_frame_violations(bad + decoys, dispatch))

    def test_actual_player_draw_is_logically_read_only_through_helpers(self):
        source = Path('rust-modules/screens/src/player/mod.rs').read_text(encoding='utf-8')
        self.assertEqual(boundary.player_draw_violations(source), [])
        for write in ('self.drawn_watch = None;', 'self.auto_skip.presented();',
                      'self.auto_skip.cancel();', 'self.drawn_watch.take();'):
            changed = source.replace('let measure = df.measure;', 'let measure = df.measure; ' + write)
            self.assertTrue(boundary.player_draw_violations(changed), write)
        changed = source.replace('let measure = df.measure;', 'let measure = df.measure; self.hidden_write();')
        changed += '\nimpl PlayerScreen { fn hidden_write(&mut self) { self.auto_skip.presented(); } }'
        self.assertTrue(boundary.player_draw_violations(changed))
        self.assertTrue(boundary.player_draw_violations(source.replace('self.drawn_watch.as_ref()', 'None.as_ref()')))
        forged = source + '\nfn external_write() { match ev { ScreenEvent::App(AppMsg::PlayerPresented { watch, .. }) => self.auto_skip.presented(), _ => {} } }'
        self.assertTrue(boundary.player_draw_violations(forged))
        self.assertEqual(boundary.player_draw_violations(source + '\n#[cfg(test)] fn hidden_write() { self.auto_skip.presented(); }'), [])

    def test_typed_effect_partition_and_observer_negative_controls(self):
        bridge = Path('rust-modules/src/app/bridge.rs').read_text(encoding='utf-8')
        dispatch = Path('rust-modules/ui/src/dispatch.rs').read_text(encoding='utf-8')
        self.assertEqual(boundary.effect_partition_violations(bridge, dispatch), [])
        for changed_bridge, changed_dispatch in (
            (bridge + '\nfn external_app_fx() -> bool { true }', dispatch),
            (bridge.replace('fx: InlineAppFx', 'fx: AppFx'), dispatch),
            (bridge, dispatch.replace('ExternalFx<H>', 'H::Fx')),
            (bridge, dispatch + '\nimpl Rig<Host> for Malicious { fn route_app_fx(&self, fx: Fx) { drop(fx); } }'),
            (bridge, dispatch.replace('plx_machine::machine::AppEffect::route(app_fx)', 'rig.route_app_fx(app_fx)')),
            (bridge, dispatch.replace('plx_machine::machine::AppEffect::route(app_fx)', 'drop(app_fx)')),
            (bridge, dispatch.replace('plx_machine::machine::AppEffect::route(app_fx)',
                                      'plx_machine::machine::AppEffect::route(app_fx); plx_machine::machine::AppEffect::route(app_fx)')),
            (bridge.replace('let command = d.take_app_command()?;',
                            '#[cfg(test)] let observed = true; let command = d.take_app_command()?;'), dispatch),
            (bridge.replace('observation.finish(self, outcome);', ''), dispatch),
            (bridge.replace('self.preference_effect(request, &mut out);', 'unreachable!();'), dispatch),
        ):
            self.assertTrue(boundary.effect_partition_violations(changed_bridge, changed_dispatch))
        decoys = '\n// fn external_app_fx() -> bool {}\n#[cfg(test)] fn external_app_fx() -> bool { true }'
        self.assertEqual(boundary.effect_partition_violations(bridge + decoys, dispatch), [])

    def test_actual_observer_fanout_rejects_drops_duplicates_and_second_authority(self):
        dispatch = Path('rust-modules/ui/src/dispatch.rs').read_text(encoding='utf-8')
        bridge = Path('rust-modules/src/app/bridge.rs').read_text(encoding='utf-8')
        self.assertEqual(boundary.observer_violations(dispatch, bridge), [])
        for before, after in (
            ('PassiveObservers { owned, caller }', 'PassiveObservers { owned: None, caller }'),
            ('FrameObservers::Owned(tap) => Some(tap)', 'FrameObservers::Owned(_) => None'),
            ('self.caller.$name($($arg),*);', ''),
            ('self.caller.$name($($arg),*);', 'self.caller.$name($($arg),*); self.caller.$name($($arg),*);'),
            ('owned.$name($($arg),*);', ''),
            ('owned.$name($($arg),*);', 'owned.$name($($arg),*); owned.$name($($arg),*);'),
            ('result(f: u64, addr: &Addr, msg: &H::Msg);', ''),
            ('FrameObservers::Replay(replay) => call(replay)', 'FrameObservers::Replay(replay) => call(caller)'),
            ('FrameObservers::Caller | FrameObservers::Owned(_) => call(&mut LiveAuthority)',
             'FrameObservers::Caller | FrameObservers::Owned(caller) => call(caller)'),
            ('pub trait Tap<H: Host> {', 'pub trait Tap<H: Host> { fn resolution_mode(&self) -> Option<bool> { Some(false) }'),
            ('pub trait Tap<H: Host> {', 'pub trait Tap<H: Host> { fn resolve_id(&mut self) {}'),
            ('let mut once = || *cached.get_or_insert_with(&mut *hash);', 'let mut once = || hash();'),
        ):
            with self.subTest(before=before):
                self.assertIn(before, dispatch)
                self.assertTrue(boundary.observer_violations(dispatch.replace(before, after), bridge))
        changed_bridge = bridge.replace('FrameObservers::Replay(self.resources.rec)', 'FrameObservers::Owned(self.resources.rec)')
        self.assertNotEqual(changed_bridge, bridge)
        self.assertTrue(boundary.observer_violations(dispatch, changed_bridge))

    def test_owned_ui_state_rejects_renamed_logical_storage(self):
        cases = (
            ('modal', 'static AGAIN: AtomicU32 = AtomicU32::new(0);'),
            ('modal', 'thread_local! { static LIFT_OWNS: Cell<bool> = Cell::new(false); }'),
            ('modal', '#[cfg(feature = "devtriggers")] thread_local! { static RENAMED_LIFT: Cell<bool> = Cell::new(false); }'),
            ('modal', 'static mut RENAMED: bool = false;'),
            ('modal', 'thread_local! { static RENAMED: Cell<bool> = Cell::new(false); }'),
            ('modal', 'thread_local! { static CACHE: Cell<f32> = Cell::new(0.0); }'),
            ('modal', 'thread_local! { static OTHER: RefCell<ModalRead> = RefCell::new(read); }'),
            ('modal', 'thread_local! { static OTHER: RefCell<Option<ModalPresentation>> = RefCell::new(None); }'),
            ('modal', 'struct Renamed { input_covered: bool, veil: f32 } thread_local! { static CACHE: RefCell<Renamed> = RefCell::new(value); }'),
            ('hint', 'thread_local! { static RENAMED: Cell<bool> = Cell::new(false); }'),
            ('hint', 'thread_local! { static CACHE: RefCell<HoldHintRead> = RefCell::new(read); }'),
            ('hint', 'struct Renamed { learned: bool, card: bool, pill: bool } static OWNER: Mutex<Renamed> = Mutex::new(read);'),
            ('navigation', 'thread_local! { static PAGE_IMAGE_HELD: Cell<bool> = Cell::new(false); }'),
            ('navigation', 'thread_local! { static RENAMED_PAGE_POLICY: Cell<bool> = Cell::new(false); }'),
            ('navigation', 'static GENERIC: AtomicBool = AtomicBool::new(false);'),
            ('navigation', 'thread_local! { static CACHE: RefCell<Option<PageImageRead>> = RefCell::new(None); }'),
            ('navigation', 'fn page_image_held() -> bool { false }'),
            ('navigation', 'fn set_page_image_held(held: bool) {}'),
        )
        for family, source in cases:
            with self.subTest(family=family, source=source):
                self.assertTrue(boundary.owned_ui_state_violations(source, family))

    def test_owned_ui_state_keeps_instance_facts_and_render_caches(self):
        sources = (
            ('modal', 'struct Owner { modal: ModalRead, presentation: ModalPresentation }'),
            ('hint', 'struct Fixture { read: HoldHintRead } impl Fixture { fn event(&mut self, event: HoldHintEvent) { self.read.apply(event); } }'),
            ('modal', 'thread_local! { static CACHE: std::cell::RefCell<FrameCache> = RefCell::new(FrameCache::new()); static HELD: Cell<Held> = Cell::new(Held::Nothing); static TARGET: RefCell<Option<plx_base::surface::PageTarget>> = RefCell::new(None); static CACHE_OWNER: RefCell<Option<crate::containers::modal::ModalCacheKey>> = RefCell::new(None); }'),
            ('modal', 'struct ModalCacheKey { owner: std::sync::Arc<()>, layers: Vec<(EntryId, bool)> } impl ModalCacheKey { fn same_snapshot(&self, other: &Self) -> bool { Arc::ptr_eq(&self.owner, &other.owner) && self.layers == other.layers } }'),
            ('hint', '#[cfg(test)] mod fixtures { thread_local! { static LESSON: Cell<bool> = Cell::new(false); } }'),
            ('modal', '// static OPEN_COUNT: AtomicU32 = value;\nfn decoy() { let s = "static AGAIN: Cell<bool> = value;"; }'),
            ('navigation', 'struct Owner { image: PageImage } impl Owner { fn read(&self, entry: EntryId, instance: InstanceId) -> PageImageRead { self.image.read(entry, instance) } }'),
        )
        for family, source in sources:
            with self.subTest(family=family, source=source):
                self.assertEqual(boundary.owned_ui_state_violations(source, family), [])

    def test_owned_ui_state_cache_identity_cannot_publish_policy(self):
        for source in (
            'struct ModalCacheKey { owner: Arc<()>, layers: Vec<(EntryId, bool)>, read: ModalRead }',
            'fn renamed_policy(input: &ModalPresentation, key: &ModalCacheKey) -> bool { key.same_snapshot(&input.cache_key()) }',
            'thread_local! { static CACHE_OWNER: RefCell<Option<ModalRead>> = RefCell::new(None); }',
            'thread_local! { static CACHE_OWNER: RefCell<Option<ModalCacheKey>> = RefCell::new(None); } fn page_veiled() -> bool { CACHE_OWNER.with_borrow(|value| value.is_some()) }',
            'thread_local! { static HELD: Cell<Held> = Cell::new(Held::Nothing); } fn page_veiled() -> bool { renamed() } fn renamed() -> bool { HELD.with(|value| value.get() != Held::Nothing) }',
        ):
            with self.subTest(source=source):
                self.assertTrue(boundary.owned_ui_state_violations(source, 'modal'))

    def test_owned_ui_state_checks_real_roots_and_detects_reintroduction(self):
        roots = (
            ('hint', Path('rust-modules/ui/src/hold_hint.rs')),
            ('modal', Path('rust-modules/ui/src/containers/modal.rs')),
            ('modal', Path('rust-modules/ui/src/popover.rs')),
            ('navigation', Path('rust-modules/ui/src/containers/transition.rs')),
            ('navigation', Path('rust-modules/ui/src/dispatch.rs')),
            ('navigation', Path('rust-modules/ui/src/cards/landing.rs')),
        )
        self.assertEqual(boundary.check_owned_ui_state(Path('.')), [])
        for family, path in roots:
            source = path.read_text(encoding='utf-8')
            with self.subTest(path=path):
                self.assertEqual(boundary.owned_ui_state_violations(source, family), [])
                self.assertTrue(boundary.owned_ui_state_violations(source + '\nthread_local! { static RENAMED_LOGIC: Cell<bool> = Cell::new(false); }', family))

    def test_check_deps_runs_screen_boundary_gate(self):
        script = Path('ci/check-deps.sh').read_text(encoding='utf-8')
        self.assertIn('python3 ci/ui_boundaries.py', script)


if __name__ == '__main__':
    unittest.main()
