#!/usr/bin/env python3
"""Find Rust files reachable only through cfg(test) module/include declarations.

Paths are derived from tokenized declarations, never filenames. A production reference wins
when one source file is included in both configurations. Unreferenced files remain production
inputs for the structural gates, preserving their deliberate orphan-file negative controls.
"""
from pathlib import Path
import argparse
import itertools
import re


RAW = re.compile(r'(?:b|c)?r(#{0,255})"')
STRING = re.compile(r'(?:b|c)?"')
CHAR = re.compile(r"(?:b)?'(?:\\(?:u\{[^}]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'")
WORD = re.compile(r'[A-Za-z_][A-Za-z_0-9]*')


def lex(source):
    tokens, i = [], 0
    while i < len(source):
        if source[i].isspace(): i += 1; continue
        if source.startswith('//', i):
            end = source.find('\n', i); i = len(source) if end < 0 else end; continue
        if source.startswith('/*', i):
            depth, i = 1, i + 2
            while i < len(source) and depth:
                if source.startswith('/*', i): depth, i = depth + 1, i + 2
                elif source.startswith('*/', i): depth, i = depth - 1, i + 2
                else: i += 1
            continue
        raw = RAW.match(source, i)
        string = STRING.match(source, i)
        char = CHAR.match(source, i)
        if raw:
            start = raw.end(); suffix = '"' + raw[1]
            end = source.find(suffix, start)
            if end < 0: raise ValueError('unterminated Rust raw string')
            tokens.append(('string', source[start:end])); i = end + len(suffix)
        elif string:
            start = string.end(); i = start
            while i < len(source) and source[i] != '"': i += 2 if source[i] == '\\' else 1
            value = source[start:i]
            value = re.sub(r'\\([\\"])', r'\1', value)
            tokens.append(('string', value)); i += 1
        elif char:
            i = char.end()
        else:
            word = WORD.match(source, i)
            value = word[0] if word else source[i]
            tokens.append(('code', value)); i += len(value)
    return tokens


def close(tokens, start):
    pairs = {'(': ')', '[': ']', '{': '}'}
    stack = [pairs[tokens[start][1]]]
    for i in range(start + 1, len(tokens)):
        kind, text = tokens[i]
        if kind != 'code': continue
        if text in pairs: stack.append(pairs[text])
        elif text in pairs.values():
            if text != stack.pop(): raise ValueError('unbalanced Rust token group')
            if not stack: return i
    raise ValueError('unterminated Rust token group')


def cfg_without_tests(tokens):
    """Possible cfg values with test=false and every unrelated predicate unknown."""
    def expr(i):
        if i >= len(tokens): return {False, True}, i
        name = tokens[i][1]; i += 1
        if i < len(tokens) and tokens[i][1] == '(':
            i += 1; children = []
            while i < len(tokens) and tokens[i][1] != ')':
                value, i = expr(i); children.append(value)
                if i < len(tokens) and tokens[i][1] == ',': i += 1
            i += 1
            if name == 'not' and len(children) == 1: return {not x for x in children[0]}, i
            if name in ('all', 'any'):
                op = all if name == 'all' else any
                return {op(values) for values in itertools.product(*children)}, i
            return {False, True}, i
        if i < len(tokens) and tokens[i][1] == '=':
            return {False, True}, min(i + 2, len(tokens))
        return ({False} if name == 'test' else {False, True}), i
    return expr(0)[0]


def file_edges(path):
    source = path.read_text()
    if not re.search(r'\bmod\s+\w+\s*[;{]|\binclude\s*!\s*\(', source):
        return []
    tokens = lex(source)
    edges = []
    module_dir = path.parent if path.name in ('mod.rs', 'lib.rs', 'main.rs') else path.with_suffix('')

    def walk(start, end, directory, attribute_base, inherited_test):
        i, test_only, explicit_path = start, inherited_test, None
        while i < end:
            kind, text = tokens[i]
            if kind != 'code': i += 1; continue
            if text == '#' and i + 1 < end and tokens[i + 1][1] == '[':
                stop = close(tokens, i + 1); attr = tokens[i + 2:stop]
                if attr and attr[0][1] == 'cfg' and len(attr) > 3:
                    test_only |= cfg_without_tests(attr[2:-1]) == {False}
                elif len(attr) == 3 and attr[0][1] == 'path' and attr[1][1] == '=' and attr[2][0] == 'string':
                    explicit_path = attr[2][1]
                i = stop + 1; continue
            if text == 'mod' and i + 2 < end and tokens[i + 1][0] == 'code':
                name, next_token = tokens[i + 1][1], tokens[i + 2][1]
                if next_token == ';':
                    candidates = [attribute_base / explicit_path] if explicit_path else [directory / (name + '.rs'), directory / name / 'mod.rs']
                    for target in candidates:
                        if target.is_file(): edges.append((target.resolve(), test_only))
                    test_only, explicit_path, i = inherited_test, None, i + 3
                    continue
                if next_token == '{':
                    stop = close(tokens, i + 2)
                    nested = attribute_base / explicit_path if explicit_path else directory / name
                    walk(i + 3, stop, nested, nested, test_only)
                    test_only, explicit_path, i = inherited_test, None, stop + 1
                    continue
            if text == 'include' and i + 3 < end and tokens[i + 1][1] == '!' and tokens[i + 2][1] == '(':
                stop = close(tokens, i + 2)
                if stop == i + 4 and tokens[i + 3][0] == 'string':
                    target = path.parent / tokens[i + 3][1]
                    if target.is_file(): edges.append((target.resolve(), test_only))
                i = stop + 1; continue
            if text == '{':
                stop = close(tokens, i)
                walk(i + 1, stop, directory, attribute_base, test_only)
                i = stop + 1; test_only, explicit_path = inherited_test, None
            elif text in ('(', '['): i = close(tokens, i) + 1
            else:
                if text in (';', ','): test_only, explicit_path = inherited_test, None
                i += 1
    walk(0, len(tokens), module_dir, path.parent, False)
    return edges


def wholly_test_files(root):
    paths = {path.resolve() for path in root.rglob('*.rs')}
    edges = {path: [(target, test) for target, test in file_edges(path) if target in paths] for path in paths}
    incoming = {target for targets in edges.values() for target, _ in targets}
    roots = paths - incoming
    # The crate entry point is always production even if a fixture happens to include it.
    roots |= {root.resolve() / 'lib.rs', root.resolve() / 'main.rs'} & paths
    reached = {path: set() for path in paths}
    pending = [(path, False) for path in roots]
    while pending:
        path, test = pending.pop()
        if test in reached[path]: continue
        reached[path].add(test)
        pending.extend((target, test or edge_test) for target, edge_test in edges[path])
    # Unrooted cycles are deliberately not exempted: this helper must fail closed.
    return {path for path, modes in reached.items() if modes == {True}}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    args = parser.parse_args()
    cwd = Path.cwd()
    for path in sorted(wholly_test_files(args.root)):
        print(path.relative_to(cwd) if path.is_relative_to(cwd) else path)
