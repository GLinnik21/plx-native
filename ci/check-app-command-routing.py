#!/usr/bin/env python3
"""Prove that unreachable app-effect arms really yield to the FIFO runner."""
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


def violations(source):
    tokens = production_tokens(source)
    routers = list(functions(tokens, 'app_effect'))
    classifiers = [body for body in functions(tokens, 'external_app_fx')
                   if text(body).startswith('matches!(fx,')]
    runners = list(functions(tokens, 'execute_yielded_command'))
    if len(routers) != 1 or len(classifiers) != 1 or len(runners) != 1:
        return ['missing unique app router, pure classifier or FIFO runner']
    classifier = classifiers[0]
    if not re.fullmatch(r'matches!\(fx,AppFx::\w+\(_\)(?:\|AppFx::\w+\(_\))*\)', text(classifier)):
        return ['external classification must be the explicit pure variant match']
    external = variants(classifier)
    routed = set()
    errors = command_storage_violations(source)
    for head, body in arms(routers[0], ['match', 'fx']):
        names = variants(head)
        if text(body) == 'unreachable!(STRING)':
            routed |= names
            if not names or not names <= external:
                errors.append('unreachable variant is not externally yielded: ' + ','.join(sorted(names)))
        elif names & external:
            errors.append('external variant still executes in app_effect: ' + ','.join(sorted(names & external)))
    if routed != external:
        errors.append('external classification and unreachable routing differ')
    runner = runners[0]
    if text(runner).count('d.take_app_command()?') != 1 or text(tokens).count('.take_app_command(') != 1:
        errors.append('runner must take exactly one dispatcher FIFO head through the sole drain')
    executed = set()
    for head, body in arms(runner, ['match', 'command', '.', 'effect']):
        if not text(body).startswith('unreachable!'):
            executed |= variants(head)
            if re.search(r'\.(?:push|push_back|extend)\(', text(body)):
                errors.append('runner arm parks work instead of completing its taken head')
    if not external <= executed:
        errors.append('yielded variant lacks runner execution: ' + ','.join(sorted(external - executed)))
    return errors


def main():
    try:
        errors = violations(BRIDGE.read_text(encoding='utf-8'))
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
