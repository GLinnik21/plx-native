#!/usr/bin/env python3
"""Complete transition arguments, including the actual ten-machine HUD bypass."""
import configparser
import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('shape', ROOT / 'ci/check-machine-shape.py')
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)
REGISTRY = configparser.ConfigParser(interpolation=None)
REGISTRY.read(ROOT / 'ci/machines.ini')
ENTRIES = [s for s in REGISTRY if s != 'DEFAULT' and not s.startswith('module ')]
COUNTS = {'synthetic': 0, 'accepted': 0, 'rejected': 0}
FACTS = {
    'play-uri/src/uriprobe.rs::UriProbe': ('now_ms', 'u64', 'monotonic_ms_u64'),
    'session/src/auth/owner.rs::SessionMachine': ('tick_ms', 'u32', 'monotonic_ms_u32'),
    'playback-contract/src/state.rs::BackendState': ('capabilities', 'Option<Capabilities>', 'playback_capabilities'),
}
CAPS = 'pub struct Capabilities { ' + ''.join('pub ' + n + ': bool,' for n in (
    'progressive_uri', 'hls_uri', 'packet_feed', 'pause', 'seek', 'audio_track',
    'subtitle_track', 'audio_off', 'subtitle_off')) + ' }'


def fixture(section, extra='', prefix='', aux=None, modify=None, ini_change=None, caps=CAPS):
    file, name = section.split('::')
    entry = dict(REGISTRY[section])
    event, effect = entry['event'], entry['effect']
    fact = FACTS.get(section)
    if fact:
        entry['aux_facts'] = fact[0] + ':' + fact[2]
    arguments = 'event: ' + event
    if fact:
        arguments += ', ' + fact[0] + ': ' + (fact[1] if aux is None else aux)
    if effect != 'none':
        arguments += ', effects: &mut Vec<' + effect + '>'
    if extra:
        arguments += ', ' + extra
    source = '#![deny(clippy::wildcard_enum_match_arm)]\n' + prefix
    if fact and fact[2] == 'playback_capabilities':
        source += 'use crate::Capabilities;\n'
    source += 'enum ' + event + ' { Input }\n'
    if effect != 'none':
        source += 'enum ' + effect + ' { Output }\n'
    source += 'enum RawState { Idle } type RawAlias = RawState; struct Context { next: RawState }\n'
    source += 'struct ' + name + ' { ' + entry['state_field'] + ': RawState }\n'
    source += 'impl ' + name + ' {\n'
    if entry['constructor'].isupper():
        source += 'const ' + entry['constructor'] + ': Self = Self { ' + entry['state_field'] + ': RawState::Idle };\n'
    else:
        source += 'fn ' + entry['constructor'] + '() -> Self { Self { ' + entry['state_field'] + ': RawState::Idle } }\n'
    source += 'fn transition(&mut self, ' + arguments + ') { self.' + entry['state_field'] + ' = RawState::Idle; }\n}'
    if modify:
        source = modify(source)
    if ini_change:
        ini_change(entry)
    with tempfile.TemporaryDirectory() as temp:
        modules = Path(temp) / 'rust-modules'
        path = modules / file
        path.parent.mkdir(parents=True)
        path.write_text(source, encoding='utf-8')
        if fact and fact[2] == 'playback_capabilities':
            (modules / 'playback-contract/Cargo.toml').write_text('[package]\nname = "plx_playback_contract"\nversion = "0.0.0"\n', encoding='utf-8')
            (modules / 'playback-contract/src/lib.rs').write_text(caps, encoding='utf-8')
        registry = configparser.ConfigParser(interpolation=None)
        registry[section] = entry
        registry['module ' + file] = {'wildcard_enum_match_arm': 'deny'}
        ini = Path(temp) / 'machines.ini'
        with ini.open('w', encoding='utf-8') as stream:
            registry.write(stream)
        problems = gate.check(modules, ini)[0]
        COUNTS['synthetic'] += 1
        COUNTS['rejected' if problems else 'accepted'] += 1
        return problems


class TransitionArguments(unittest.TestCase):
    @classmethod
    def tearDownClass(cls):
        print('transition-argument fixture verdicts:', COUNTS)

    def test_actual_hud_full_gate_raw_state_repro(self):
        target = (ROOT / 'rust-modules/data/src/hudpacks.rs').resolve()
        old = Path.read_text
        original = old(target, encoding='utf-8')
        source = original.replace(
            'fn transition(&mut self, ev: HudEvent, fx: &mut Vec<HudEffect>) -> Handled {',
            'fn transition(&mut self, ev: HudEvent, fx: &mut Vec<HudEffect>, next: Op) -> Handled { self.op = next;', 1
        ).replace('self.transition(ev, fx) }', 'self.transition(ev, fx, Op::Idle) }', 1)
        self.assertIn('self.op = next;', source)
        self.assertIn('self.transition(ev, fx, Op::Idle)', source)
        def read(path, *args, **kwargs):
            return source if path.resolve() == target else old(path, *args, **kwargs)
        with patch.object(Path, 'read_text', read):
            problems, report = gate.check(ROOT / 'rust-modules', ROOT / 'ci/machines.ini')
        self.assertEqual(sum(line.startswith('machine ') for line in report), 10)
        self.assertTrue(any('unbound transition argument' in p for p in problems), problems)

    def test_every_cohort_machine_rejects_unbound_arguments(self):
        self.assertEqual(len(ENTRIES), 10)
        for section in ENTRIES:
            for extra in ('next: RawState', 'renamed: RawAlias', 'ctx: Context', 'extra: u64',
                          'next: &RawState', 'next: &mut RawState', 'cb: fn(RawState)',
                          'cb: impl FnOnce()', 'next: *mut RawState', 'bag: Option<Context>'):
                with self.subTest(machine=section, extra=extra):
                    self.assertTrue(fixture(section, extra), (section, extra))

    def test_synthetic_canonical_signatures(self):
        for section in ENTRIES:
            with self.subTest(machine=section):
                self.assertEqual(fixture(section), [])

    def test_duplicate_event_or_effect(self):
        for section in ENTRIES:
            entry = REGISTRY[section]
            for extra in ('another: ' + entry['event'], 'event: ' + entry['event']):
                with self.subTest(machine=section, extra=extra):
                    self.assertTrue(fixture(section, extra))
            if entry['effect'] != 'none':
                self.assertTrue(fixture(section, 'other: &mut Vec<' + entry['effect'] + '>'))

    def test_closed_fact_slots(self):
        for section, (name, typ, kind) in FACTS.items():
            for bad in ('RawState', 'RawAlias', '&' + typ, '&mut ' + typ, 'Context', 'fn()', 'T'):
                with self.subTest(machine=section, typ=bad):
                    self.assertTrue(fixture(section, aux=bad))
            self.assertTrue(fixture(section, name + ': ' + typ))
            self.assertTrue(fixture(section, ini_change=lambda e: e.pop('aux_facts')))
            for value in (name + ':unknown', name + ':RawState', 'renamed:' + kind,
                          name + ':' + kind + ',' + name + ':' + kind, 'event:' + kind,
                          'effects:' + kind):
                with self.subTest(machine=section, declaration=value):
                    self.assertTrue(fixture(section, ini_change=lambda e: e.update(aux_facts=value)))

    def test_unknown_auxiliary_declarations_fail_closed(self):
        section = 'data/src/hudpacks.rs::HudStore'
        for value in ('ctx:Context', 'next:RawState', 'next:monotonic_ms_u32'):
            self.assertTrue(fixture(section, 'next: RawState', ini_change=lambda e: e.update(aux_facts=value)))

    def test_primitive_shadow_and_alias_are_not_clock_facts(self):
        for section, (name, typ, kind) in FACTS.items():
            if typ not in ('u32', 'u64'):
                continue
            for prefix in ('type ' + typ + ' = RawState;', 'use crate::RawState as ' + typ + ';',
                           'mod shadow { pub type ' + typ + ' = super::RawState; } use shadow::*;'):
                with self.subTest(machine=section, prefix=prefix):
                    self.assertTrue(fixture(section, prefix=prefix))
            self.assertTrue(fixture(section, prefix='type Clock = ' + typ + ';', aux='Clock'))
            self.assertTrue(fixture(section, modify=lambda s: s.replace('fn transition(', 'fn transition<T>(')))

    def test_fact_layout_and_canonical_primitive_identity(self):
        for section, (name, typ, _) in FACTS.items():
            if typ in ('u32', 'u64'):
                self.assertEqual(fixture(section, aux='::core::primitive::' + typ), [])
                self.assertEqual(fixture(section, aux='std::primitive::' + typ), [])
                self.assertTrue(fixture(section, aux='core::primitive::' + typ,
                                        prefix='mod core { pub mod primitive { pub type ' + typ + ' = super::super::RawState; } }'))
                self.assertTrue(fixture(section, modify=lambda s: s.replace('impl ', 'impl<' + typ + '> ', 1)))
        section = 'playback-contract/src/state.rs::BackendState'
        for caps in (CAPS.replace('pause: bool', 'pause: u64'), CAPS.replace('pause: bool,', ''),
                     CAPS.replace('pause: bool', 'pause: Context'),
                     CAPS + ' type bool = u8;', 'pub type Capabilities = RawState;'):
            with self.subTest(caps=caps):
                self.assertTrue(fixture(section, caps=caps))
        self.assertTrue(fixture(section, prefix='type Capabilities = RawState;'))
        self.assertTrue(fixture(section, prefix='type Hidden = &mut Option<Capabilities>;', aux='Hidden'))
        self.assertTrue(fixture(section, prefix='type Hidden = &mut Capabilities;', aux='Option<Hidden>'))
        self.assertTrue(fixture(section, prefix='type Option<T> = &mut T;'))

    def test_uri_wildcard_replacements_are_complete_and_equivalent(self):
        import re
        source = (ROOT / 'rust-modules/play-uri/src/uriprobe.rs').read_text(encoding='utf-8')
        protocol = (ROOT / 'rust-modules/play-uri/src/uri_protocol.rs').read_text(encoding='utf-8')
        enum = protocol.split('enum Method {', 1)[1].split('}', 1)[0]
        variants = set(re.findall(r'\b(\w+)\s*,', enum))
        inner = source.split('call {} ok", match method {', 1)[1].split('})))', 1)[0]
        fallback = inner.split('Method::Subscribe => "subscribe",', 1)[1].split('=> "play"', 1)[0]
        inner_variants = set(re.findall(r'Method::(\w+)', fallback))
        self.assertEqual(inner_variants, variants - {'Load', 'Subscribe'})
        outer = source.split('Work::ServiceAfterVideo { method, ok, message } => {', 1)[1].split('if self.media_id.is_none() && !denied', 1)[0]
        tail = outer.rsplit('=> {}', 1)[0].rsplit('})));', 1)[1]
        outer_variants = set(re.findall(r'Method::(\w+)', tail))
        self.assertEqual(outer_variants, {'Register', 'Load', 'Subscribe', 'Play', 'Unload', 'Dropped', 'Other'})
        unconditional = {'NotifyForeground', 'Pause', 'Seek', 'SelectTrack', 'SetSubtitleEnable', 'SetWindowId'}
        for method in variants:
            for denied in (False, True):
                for confirmed in (False, True):
                    for has_events in (False, True):
                        guarded = method in {'Load', 'Subscribe', 'Play'} and (denied or confirmed and not has_events)
                        earlier = method in unconditional or guarded
                        self.assertEqual(not earlier, method in outer_variants and not earlier)
                        old = 'load' if method == 'Load' else 'subscribe' if method == 'Subscribe' else 'play'
                        new = 'load' if method == 'Load' else 'subscribe' if method == 'Subscribe' else 'play' if method in inner_variants else None
                        self.assertEqual(old, new)

    def test_actual_session_tick_and_entire_cohort(self):
        source = (ROOT / 'rust-modules/session/src/auth/owner.rs').read_text(encoding='utf-8')
        self.assertIn('tick_ms: u32', source)
        problems, report = gate.check(ROOT / 'rust-modules', ROOT / 'ci/machines.ini')
        self.assertEqual(sum(line.startswith('machine ') for line in report), 10)
        self.assertEqual(problems, [])


if __name__ == '__main__':
    unittest.main()
