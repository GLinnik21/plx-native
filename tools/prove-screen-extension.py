#!/usr/bin/env python3
"""Actual AppHost new-screen compiler oracle; never edits the product or Git metadata.

Run with Python3.12 and the repository-pinned Rust toolchain in a clean environment.
--output must be ignored/private.
The product overlay changes exactly three Rust files. Separate private-crate harness
roots only add acceptance tests; no feature, replacement Home, fixture Host or IO gate.
--baseline-app supplies the frozen pre-repair words.rs/run.rs for the compiler RED.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path

import shutil
import subprocess
import time
import tomllib

ROOT = Path(__file__).resolve().parents[1]
PRODUCTION = ('rust-modules/screens/src/review_static.rs',
              'rust-modules/screens/src/lib.rs', 'rust-modules/screens/src/registry.rs')

SCREEN = r'''use crate::registry::{AppArg, AppFx};
use plx_machine::machine::{Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, Host, InputKind, Key, LogicalState, Machine, NavOp};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Placed, Screen, ScreenEvent, Seat, Step};
use plx_ui::Rect;
pub struct ReviewStatic { entry: EntryId, fired: bool }
impl ReviewStatic {
    pub fn new(entry: EntryId) -> Self { Self { entry, fired: false } }
}
impl LogicalState for ReviewStatic {
    fn write(&self, c: &mut Canon) { c.u32(self.entry.0).bool(self.fired); }
    fn probe(&self, out: &mut String) { out.push_str(if self.fired { "extension=fired" } else { "extension=ready" }); }
}
impl<H: Host<Arg=AppArg, Fx=AppFx, Elem=u32>> Machine<H> for ReviewStatic {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, event: &Self::Ev, _: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        if matches!(event, ScreenEvent::Input(input) if matches!(input.kind,
            InputKind::Key { key: Key::Right, edge: Edge::Down, .. })) && !self.fired {
            self.fired = true;
            fx.push(Fx::App(AppFx::Store(plx_data::stores::StoreId::Metadata,
                plx_data::stores::StoreCmd::Metadata(plx_data::stores::metadata::MetadataCmd::Clear))));
            fx.push(Fx::Nav(NavOp::Push(AppArg::Search)));
            return Handled::Yes;
        }
        Handled::No
    }
}
impl<H: Host<Arg=AppArg, Fx=AppFx, Elem=u32>> Focusable<H> for ReviewStatic {
    fn groups(&self, _: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec { id: GroupId(37), kind: GroupKind::Row { wrap: false },
            seat: Seat::Nearest, reachable: AxisMask::BOTH, edge: [EdgeRule::Geometric; 4],
            extent: Rect::new(100.0, 100.0, 200.0, 100.0), len: 1, elem: ElemKind::Control });
    }
    fn group_of(&self, key: &u32, _: &Cx<'_, H>) -> Option<GroupId> { (*key == 0x4001).then_some(GroupId(37)) }
    fn neighbour(&self, _: FocusKey<u32>, _: Dir, _: &Cx<'_, H>) -> Step<u32> { Step::Edge }
    fn place(&self, key: &u32, _: &Cx<'_, H>, _: At) -> Option<Placed> {
        let rect = Rect::new(100.0, 100.0, 200.0, 100.0);
        (*key == 0x4001).then_some(Placed { rect, rest_rect: rect, clip: Rect::FULL, index: Some(0) })
    }
    fn reconcile(&self, _: FocusKey<u32>, _: &Cx<'_, H>) -> FocusKey<u32> { FocusKey { entry: self.entry, elem: 0x4001 } }
    fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, H>) -> FocusKey<u32> { FocusKey { entry: self.entry, elem: 0x4001 } }
}
impl<H: Host<Arg=AppArg, Fx=AppFx, Elem=u32>> Screen<H> for ReviewStatic {
    fn name(&self) -> &'static str { "review_static" }
    fn state(&self) -> &dyn LogicalState { self }
    fn crumb(&self, _: &Cx<'_, H>) -> Option<std::borrow::Cow<'_, str>> { None }
    fn prepare(&mut self, _: &mut Budget, _: &Cx<'_, H>) {}
    fn draw(&mut self, _: &mut DrawFrame<'_, '_, H>) {}
    fn render(&self) -> plx_ui::screen::RenderStrategy { plx_ui::screen::RenderStrategy::Page }
    fn memory_at(&self, _: Option<FocusKey<u32>>) -> H::Memory { Default::default() }
}
'''

TEST = r'''
#[cfg(test)] mod private_extension_acceptance {
    use super::*;
    #[test]
    fn fresh_static_screen_uses_product_registration_only() {
        let _serial = plx_base::testlock::serial();
        let mut owners = runtime_owners::Harness::new(|| 0);
        let mut bridge = owners.bridge();
        let mut d = plx_ui::dispatch::Dispatcher::<bridge::AppHost>::new();
        let tick = |ms| plx_machine::machine::Tick { ms, dt_us: 16_000 };
        bridge::nav_root(&mut d, AppArg::Home);
        bridge::frame(&mut d, &mut bridge, tick(0), vec![]);
        let home = d.nav.top_page().unwrap().id;
        bridge::nav_push(&mut d, AppArg::ReviewStatic);
        bridge::frame(&mut d, &mut bridge, tick(1), vec![]);
        let fresh = d.nav.top_page().unwrap().id;

        assert_ne!(fresh, home);
        assert!(matches!(d.top_arg(), Some(AppArg::ReviewStatic)));
        assert_eq!(words::route_word(d.top_arg().unwrap()), "review_static");
        assert_eq!(d.top_arg().unwrap().route_probe().kind, plx_screens::registry::ProbeKind::Generic);
        assert_eq!(d.nav.top_page().unwrap().inst.as_ref().unwrap().screen.name(), "review_static");
        bridge::frame(&mut d, &mut bridge, tick(2), bridge::script_key(plx_machine::machine::Key::Down, tick(2)));
        assert_eq!(d.focus_record(), Some((fresh.0, 0x4001, Some(37))));
        assert_eq!(bridge::content_probe(&d, &bridge), " group=37 elem=16385 extension=ready");
        plx_data::metadata::set_current_for_test(bridge.metadata_mut().state_mut(),
            Some(plx_data::metadata::Detail { rk: "extension-owned".into(), ..Default::default() }));
        bridge::frame(&mut d, &mut bridge, tick(3), bridge::script_key(plx_machine::machine::Key::Right, tick(3)));
        assert!(bridge.metadata_view().current().is_none());
        assert!(matches!(d.top_arg(), Some(AppArg::Search)));
        bridge::nav_pop(&mut d);
        bridge::frame(&mut d, &mut bridge, tick(4), vec![]);
        assert!(matches!(d.top_arg(), Some(AppArg::ReviewStatic)));
        assert_eq!(d.nav.top_page().unwrap().id, fresh);
        assert_eq!(bridge::content_probe(&d, &bridge), " group=37 elem=16385 extension=fired");
        let content = bridge::content_probe(&d, &bridge);
        let line = crate::focusprobe::line(&plx_media::route::PlaybackSession::IDLE,
            words::route_word(d.top_arg().unwrap()), crate::focusprobe::Screen::Content,
            crate::focusprobe::Hud { focus: 0, btn: 0, tab: 0, visible: false },
            plx_appkit::player_hud::ControlSlot::Discs(plx_appkit::player_hud::Controls::NONE), &content, bridge.metadata_view(), false);
        assert_eq!(line, "focus route=review_static group=37 elem=16385 extension=fired press=0");
        bridge::nav_pop(&mut d);
        bridge::frame(&mut d, &mut bridge, tick(5), vec![]);
        assert!(matches!(d.top_arg(), Some(AppArg::Home)));
        assert_eq!(d.nav.top_page().unwrap().id, home);
        assert!(d.pending_app_command().is_none());
        bridge.assert_no_store_jobs_left_for_test();
    }
}
'''


def digest(path):
    data = os.readlink(path).encode() if path.is_symlink() else path.read_bytes()
    return hashlib.sha256(data).hexdigest()


def write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding='utf-8', newline='\n')


def replace_once(text, old, new):
    if text.count(old) != 1:
        raise RuntimeError(f'expected one registration anchor: {old!r}')
    return text.replace(old, new, 1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--baseline-app', type=Path, help='optional frozen pre-repair app files for the historical RED oracle')
    parser.add_argument('--seed-target', type=Path, required=True)
    parser.add_argument('--resume', action='store_true', help='reuse this proof\'s isolated artifact copy after a failed attempt')
    parser.add_argument('--public-scenario', action='store_true', help='also add a real dev scenario and manifest scene, then run existing manifest word validation')
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=args.resume)
    env = {k: v for k, v in os.environ.items() if not k.startswith('GIT_')}
    env.update(CARGO_BUILD_JOBS='2', RUST_TEST_THREADS='1', CARGO_INCREMENTAL='0')
    names = subprocess.check_output(['git', 'ls-files', '-z', '--cached', '--others', '--exclude-standard'], cwd=ROOT, env=env).decode().split('\0')
    names = sorted(set(n for n in names if n and ((ROOT / n).is_file() or (ROOT / n).is_symlink())))
    before = {n: digest(ROOT / n) for n in names}
    overlay = output / 'product'
    for name in names:
        source, dest = ROOT / name, overlay / name
        dest.parent.mkdir(parents=True, exist_ok=True)
        if source.is_symlink():
            if not dest.is_symlink():
                dest.symlink_to(os.readlink(source))
        else:
            shutil.copyfile(source, dest)
    write(overlay / PRODUCTION[0], SCREEN)
    lib = overlay / PRODUCTION[1]
    write(lib, lib.read_text() + '\npub mod review_static;\n')
    registry = overlay / PRODUCTION[2]
    source = registry.read_text()
    for old, new in (
        ('pub enum AppArg {', 'pub enum AppArg {\n    ReviewStatic,'),
        ('let (word, kind) = match self {', 'let (word, kind) = match self {\n            Self::ReviewStatic => ("review_static", Generic),'),
        ('Self::Login => { c.u32(0).u32(0); }', 'Self::ReviewStatic => { c.u32(0).u32(14); }\n            Self::Login => { c.u32(0).u32(0); }'),
        ('AppArg::Login\n            | AppArg::Profiles', 'AppArg::ReviewStatic | AppArg::Login\n            | AppArg::Profiles'),
        ('AppArg::Login => 1,', 'AppArg::ReviewStatic => 31,\n            AppArg::Login => 1,'),
        ('match arg {\n            AppArg::LibraryMenu', 'match arg {\n            AppArg::ReviewStatic => Box::new(crate::review_static::ReviewStatic::new(entry)),\n            AppArg::LibraryMenu'),
        ('AppArg{Login,', 'AppArg{ReviewStatic,Login,'),
        ('AppArg::Search, AppArg::Watchlist, AppArg::Player,\n    ]', 'AppArg::Search, AppArg::Watchlist, AppArg::Player, AppArg::ReviewStatic,\n    ]'),
    ):
        if old == 'AppArg::Login\n            | AppArg::Profiles':
            assert source.count(old) == 2  # chrome and the surface/page census
            source = source.replace(old, new)
        else:
            source = replace_once(source, old, new)
    marker = source.index('pub const SCREEN_SHAPES: &[&str] = &[')
    close = source.index('];', marker)
    source = source[:close] + '    "ReviewStatic{entry:u32,fired:bool}",\n' + source[close:]
    write(registry, source)
    production = list(PRODUCTION)
    test_source = TEST
    if args.public_scenario:
        scenario_name = 'rust-modules/src/dev/scenarios.rs'
        manifest_name = 'tests/manifest.json'
        production.extend((scenario_name, manifest_name))
        scenario = overlay / scenario_name
        text = scenario.read_text()
        anchor = 'pub(crate) unsafe fn each_frame(app: &mut App, fr: &mut Frame) -> bool {'
        text = replace_once(text, anchor, anchor + '\n    if plx_base::devtrig::flag("review-static") && matches!(app.route(), AppArg::Home) { review_static_open(&mut app.pages); }')
        text += '\npub(crate) fn review_static_open(d: &mut plx_ui::dispatch::Dispatcher<crate::app::bridge::AppHost>) {\n    crate::app::bridge::nav_push(d, AppArg::ReviewStatic);\n}\n'
        write(scenario, text)
        scene = overlay / manifest_name
        added = '    {"name":"review-static", "tier":"ui", "route":"review_static", "loop_floor":1, "run_secs":18, "warmup_s":1, "triggers":{"plxnative-review-static":true}},\n'
        write(scene, replace_once(scene.read_text(), '"fps_scenes": [\n', '"fps_scenes": [\n' + added))
        test_source = replace_once(test_source, 'bridge::nav_push(&mut d, AppArg::ReviewStatic);', 'crate::dev::scenarios::review_static_open(&mut d);')
    changed = sorted(n for n in names if digest(overlay / n) != before[n]) + [PRODUCTION[0]]
    assert sorted(changed) == sorted(production), changed
    # Harness-only source roots outside the product overlay; production sources remain unchanged.
    harness = output / 'harness'
    harness.mkdir(exist_ok=args.resume)
    # Preserve relative include_str/include_bytes paths as well as Rust module lookup.
    for parent, children in ((output, overlay.iterdir()), (harness, (overlay / 'rust-modules').iterdir())):
        for child in children:
            link = parent / child.name
            if child.name != 'src' and not child.name.startswith('target') and not link.exists() and not link.is_symlink():
                link.symlink_to(child, target_is_directory=child.is_dir())
    # Keep normal Rust module-directory lookup; #[path] wrappers change nested mod lookup.
    for folder, skip in (('src', {'lib.rs', 'app'}), ('src/app', {'mod.rs'})):
        dest = harness / folder
        dest.mkdir(parents=True, exist_ok=True)
        for child in (overlay / 'rust-modules' / folder).iterdir():
            link = dest / child.name
            if child.name not in skip and not link.exists() and not link.is_symlink():
                link.symlink_to(child, target_is_directory=child.is_dir())
    write(harness / 'src/lib.rs', (overlay / 'rust-modules/src/lib.rs').read_text())
    write(harness / 'src/app/mod.rs', (overlay / 'rust-modules/src/app/mod.rs').read_text() + test_source)
    manifest = overlay / 'rust-modules/Cargo.toml'
    write(manifest, replace_once(manifest.read_text(), '\n[lib]\n', f'\n[lib]\npath = "{(harness / "src/lib.rs").as_posix()}"\n'))
    target = output / 'target'
    # Copy artifact seed, never share a writable target with the product or another build.
    if not target.exists():
        shutil.copytree(args.seed_target.resolve(), target, ignore=shutil.ignore_patterns('incremental'))
    env['CARGO_TARGET_DIR'] = str(target)
    env['PLXNATIVE_RUNTIME_DIR'] = str(output / 'runtime')
    Path(env['PLXNATIVE_RUNTIME_DIR']).mkdir(exist_ok=args.resume)
    channel = tomllib.loads((ROOT / 'rust-modules/rust-toolchain.toml').read_text())['toolchain']['channel']
    command = ['cargo', f'+{channel}', 'test', '--lib', '-p', 'plxnative-modules',
               'app::private_extension_acceptance::fresh_static_screen_uses_product_registration_only', '--', '--exact', '--test-threads=1']
    if args.public_scenario:
        command = command[:command.index('app::private_extension_acceptance::fresh_static_screen_uses_product_registration_only')] + ['--',
            'app::private_extension_acceptance::fresh_static_screen_uses_product_registration_only',
            'app::words::heartbeat_word_tests::every_manifest_route_word_is_one_the_heartbeat_prints', '--exact', '--test-threads=1']
    result = {'product_source_footprint': sorted(production), 'source_before': before, 'command': command, 'public_scenario': args.public_scenario}
    if args.baseline_app:
        result['baseline_app_hashes'] = {name: digest(args.baseline_app / name) for name in ('words.rs', 'run.rs')}
    for phase in (('red', 'green') if args.baseline_app else ('green',)):
        for file in ('words.rs', 'run.rs'):
            dest = overlay / 'rust-modules/src/app' / file
            shutil.copyfile((args.baseline_app if phase == 'red' else ROOT / 'rust-modules/src/app') / file, dest)
        start = time.monotonic()
        with (output / f'{phase}.log').open('wb') as log:
            process = subprocess.run(command, cwd=manifest.parent, env=env, stdout=log, stderr=subprocess.STDOUT)
        log = (output / f'{phase}.log').read_text(errors='replace')
        footprint = sorted([n for n in names if (n.endswith('.rs') or n == 'tests/manifest.json') and digest(overlay / n) != before[n]] + [PRODUCTION[0]])
        expected = sorted([*production, 'rust-modules/src/app/words.rs', 'rust-modules/src/app/run.rs']) if phase == 'red' else sorted(production)
        assert footprint == expected, footprint
        result[phase] = {'exit': process.returncode, 'seconds': time.monotonic() - start, 'product_source_footprint': footprint}
        write(output / 'receipt.json', json.dumps(result, indent=2) + '\n')
        if phase == 'red':
            assert (process.returncode != 0 and log.count('error[E0004]') == 2
                    and log.count('error[') == 2 and log.count('\nerror:') == 1
                    and 'non-exhaustive patterns' in log and 'words.rs' in log and 'run.rs' in log), log[-12000:]
        else:
            passed = '2 passed; 0 failed' if args.public_scenario else '1 passed; 0 failed'
            assert process.returncode == 0 and passed in log, log[-12000:]
    after_names = subprocess.check_output(['git', 'ls-files', '-z', '--cached', '--others', '--exclude-standard'], cwd=ROOT, env=env).decode().split('\0')
    after_names = sorted(set(n for n in after_names if n and ((ROOT / n).is_file() or (ROOT / n).is_symlink())))
    after = {n: digest(ROOT / n) for n in after_names}
    assert before == after, 'product changed while the isolated proof ran'
    result['source_unchanged'] = True
    result['overlay_hashes'] = {n: digest(overlay / n) for n in production}
    suffix = 'scenario' if args.public_scenario else 'static'
    for phase in ('red', 'green'):
        log = output / f'{phase}.log'
        if log.exists():
            shutil.copyfile(log, output / f'{phase}-{suffix}.log')
    write(output / f'receipt-{suffix}.json', json.dumps(result, indent=2) + '\n')
    write(output / 'receipt.json', json.dumps(result, indent=2) + '\n')
    print(json.dumps({k: result[k] for k in ('product_source_footprint', 'red', 'green', 'source_unchanged') if k in result}, indent=2))


if __name__ == '__main__':
    main()
