#!/usr/bin/env python3
"""Backstop compiler-checked inline/external effects and their single FIFO executor."""
from pathlib import Path
import re
import sys

import rust_test_modules as rust
from ui_boundaries import production_tokens, command_storage_violations


BRIDGE = Path("rust-modules/src/app/bridge.rs")


def functions(tokens, name):
    ends = rust.group_ends(tokens)
    for i in range(len(tokens) - 1):
        if tokens[i:i + 2] != [('code', 'fn'), ('code', name)]:
            continue
        j = i + 2
        while j < len(tokens) and tokens[j] != ('code', '{'):
            j = ends[j] + 1 if j in ends else j + 1
        if j < len(tokens):
            yield tokens[j + 1:ends[j]]


def text(tokens):
    return ''.join(value if kind == 'code' else 'STRING' for kind, value in tokens)


def variants(tokens):
    return set(re.findall(r'AppFx::(\w+)', text(tokens)))


def arms(tokens, subject):
    ends = rust.group_ends(tokens)
    prefix = [('code', value) for value in subject]
    for start in range(len(tokens) - len(prefix)):
        if tokens[start:start + len(prefix)] != prefix:
            continue
        opened = start + len(prefix)
        if tokens[opened] != ('code', '{'):
            continue
        stop, i = ends[opened], opened + 1
        result = []
        while i < stop:
            if tokens[i] == ('code', ','):
                i += 1
                continue
            head = i
            while i < stop and tokens[i:i + 2] != [('code', '='), ('code', '>')]:
                i = ends[i] + 1 if i in ends else i + 1
            if i == stop:
                raise ValueError('missing match arm arrow')
            pattern, i = tokens[head:i], i + 2
            body = i
            if tokens[i] == ('code', '{'):
                i = ends[i] + 1
            else:
                while i < stop and tokens[i] != ('code', ','):
                    i = ends[i] + 1 if i in ends else i + 1
            result.append((pattern, tokens[body:i]))
        return result
    raise ValueError('missing match ' + ''.join(subject))


def enum_variants(tokens, name):
    ends = rust.group_ends(tokens)
    for i in range(len(tokens) - 2):
        if tokens[i:i + 2] != [('code', 'enum'), ('code', name)]:
            continue
        j = i + 2
        while j < len(tokens) and tokens[j] != ('code', '{'):
            j += 1
        stop, j, names = ends[j], j + 1, set()
        while j < stop:
            if tokens[j] == ('code', ','):
                j += 1
                continue
            names.add(tokens[j][1])
            j += 1
            while j < stop and tokens[j] != ('code', ','):
                j = ends[j] + 1 if j in ends else j + 1
        return names
    return set()


def violations(source, registry_source=None):
    tokens = production_tokens(source)
    registry = production_tokens(source if registry_source is None else registry_source)
    routers = list(functions(tokens, 'app_effect'))
    conversions = list(functions(registry, 'route'))
    runners = list(functions(tokens, 'execute_yielded_command'))
    if len(routers) != 1 or len(conversions) != 1 or len(runners) != 1:
        return ['missing unique typed app conversion, inline executor or FIFO runner']
    errors = command_storage_violations(source)
    if list(functions(tokens, 'external_app_fx')):
        errors.append('boolean classifier cannot replace consuming typed conversion')
    declared = enum_variants(registry, 'AppFx')
    inline = enum_variants(registry, 'InlineAppFx')
    external = enum_variants(registry, 'ExternalAppFx')
    if not declared or not inline or not external or inline & external:
        errors.append('missing or overlapping typed effect partitions')
    converted, admitted_inline, admitted_external = set(), set(), set()
    for head, body in arms(conversions[0], ['match', 'self']):
        names = re.findall(r'Self::(\w+)', text(head))
        target = re.fullmatch(r'(Inline|External)\((InlineAppFx|ExternalAppFx)::(\w+)\(.*\)\)', text(body))
        if len(names) != 1 or names[0] in converted or not target:
            errors.append('conversion must consume each emission into exactly one typed partition')
            continue
        converted.update(names)
        kind, enum, name = target.groups()
        if kind == 'Inline' and enum == 'InlineAppFx':
            admitted_inline.add(name)
        elif kind == 'External' and enum == 'ExternalAppFx':
            admitted_external.add(name)
        else:
            errors.append('conversion partition and payload type differ')
    if converted != declared or admitted_inline != inline or admitted_external != external:
        errors.append('conversion does not exhaust the emission and admitted types')
    for head, body in arms(routers[0], ['match', 'fx']):
        if 'ExternalAppFx::' in text(head) or 'AppFx::' in text(head).replace('InlineAppFx::', '') or 'unreachable!' in text(body):
            errors.append('external operation reaches inline execution')
    runner = runners[0]
    if text(runner).count('d.take_app_command()?') != 1 or text(tokens).count('.take_app_command(') != 1:
        errors.append('runner must take exactly one dispatcher FIFO head through the sole drain')
    executed = set()
    for head, body in arms(runner, ['match', 'command', '.', 'effect']):
        pattern = text(head)
        names = set(re.findall(r'ExternalAppFx::(\w+)', pattern))
        explicit = all(re.fullmatch(r'Ok\((?:\w+@)?ExternalAppFx::\w+\([^|]*\)\)', part)
                       for part in pattern.split('|'))
        if not explicit and not re.fullmatch(r'Err\(\w+\)', pattern):
            errors.append('runner must explicitly match external variants or Err')
        if names:
            executed |= names
            if 'unreachable!' in text(body) or not re.search(r'(?:execute_\w+|session_effect|preference_effect|catalog_effect)\(', text(body)):
                errors.append('yielded variant lacks real runner execution')
            if re.search(r'\.(?:push|push_back|extend)\(', text(body)):
                errors.append('runner arm parks work instead of completing its taken head')
    if external != executed:
        errors.append('yielded variant lacks runner execution: ' + ','.join(sorted(external - executed)))
    return errors


def main():
    try:
        errors = violations(BRIDGE.read_text(encoding='utf-8'),
                            Path('rust-modules/screens/src/registry.rs').read_text(encoding='utf-8'))
        root = Path('rust-modules/src')
        tests = rust.wholly_test_files(root)
        for path in sorted((root / 'app').rglob('*.rs')):
            if path == BRIDGE or path.resolve() in tests:
                continue
            source = path.read_text(encoding='utf-8')
            errors.extend(f'{path}: {error}' for error in command_storage_violations(source))
            if '.take_app_command(' in text(production_tokens(source)):
                errors.append(f'{path}: FIFO head taken outside the unique runner')
    except (OSError, ValueError) as error:
        errors = [str(error)]
    for error in errors:
        print('app-command-routing: ' + error, file=sys.stderr)
    return bool(errors)


if __name__ == '__main__':
    sys.exit(main())
