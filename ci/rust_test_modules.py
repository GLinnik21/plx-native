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


# One alternation, tried in the order the rules are written (the first that matches wins), so a
# token costs one regex call instead of five separate probes at every position. The order is the
# precedence: whitespace, comments, raw string, string, character literal, word, anything else.
TOKEN = re.compile(
    r'(?P<space>\s+)'
    r'|(?P<line>//[^\n]*)'
    r'|(?P<block>/\*)'
    r'|(?P<raw>(?:b|c)?r(?P<hashes>#{0,255})")'
    r'|(?P<string>(?:b|c)?")'
    r"|(?P<char>(?:b)?'(?:\\(?:u\{[^}]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])')"
    r'|(?P<word>[A-Za-z_][A-Za-z_0-9]*)'
    r'|(?P<other>[\s\S])')
BLOCK_EDGE = re.compile(r'/\*|\*/')
STRING_RUN = re.compile(r'[^"\\]*')


def lex(source):
    tokens, i, n = [], 0, len(source)
    match = TOKEN.match
    while i < n:
        m = match(source, i)
        kind = m.lastgroup
        if kind == 'space' or kind == 'line':
            i = m.end()
        elif kind == 'word' or kind == 'other':
            value = m[0]
            tokens.append(('code', value)); i = m.end()
        elif kind == 'block':
            depth, i = 1, m.end()
            while depth:
                edge = BLOCK_EDGE.search(source, i)
                if edge is None: i = n; break
                depth += 1 if edge[0] == '/*' else -1
                i = edge.end()
        elif kind == 'raw':
            start = m.end(); suffix = '"' + m['hashes']
            end = source.find(suffix, start)
            if end < 0: raise ValueError('unterminated Rust raw string')
            tokens.append(('string', source[start:end])); i = end + len(suffix)
        elif kind == 'string':
            start = i = m.end()
            while True:
                i = STRING_RUN.match(source, i).end()
                if i >= n or source[i] == '"': break
                i += 2  # a backslash and the character it escapes
            value = source[start:i]
            value = re.sub(r'\\([\\"])', r'\1', value)
            tokens.append(('string', value)); i += 1
        else:  # a character literal: it cannot name a module, so it yields no token
            i = m.end()
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


def group_ends(tokens):
    """Index of the closer of every bracket group that is balanced, in ONE pass. `close` re-scans a
    group from its opener, so walking nested groups was quadratic in nesting depth. A group that
    is not in the result is unbalanced or unterminated, and `close` on it raises as before."""
    pairs = {'(': ')', '[': ']', '{': '}'}
    closers = set(pairs.values())
    ends, stack = {}, []
    for i, (kind, text) in enumerate(tokens):
        if kind != 'code': continue
        if text in pairs: stack.append((pairs[text], i))
        elif text in closers and stack:
            want, opened = stack.pop()
            if text == want: ends[opened] = i
            else: stack.clear()  # every group still open here is unbalanced
    return ends


def file_edges(path):
    source = path.read_text()
    if not re.search(r'\bmod\s+\w+\s*[;{]|\binclude\s*!\s*\(', source):
        return []
    tokens = lex(source)
    ends = group_ends(tokens)

    def group_end(start):
        end = ends.get(start)
        return close(tokens, start) if end is None else end  # re-scan only to raise the same error
    edges = []
    module_dir = path.parent if path.name in ('mod.rs', 'lib.rs', 'main.rs') else path.with_suffix('')

    def walk(start, end, directory, attribute_base, inherited_test):
        i, test_only, explicit_path = start, inherited_test, None
        while i < end:
            kind, text = tokens[i]
            if kind != 'code': i += 1; continue
            if text == '#' and i + 1 < end and tokens[i + 1][1] == '[':
                stop = group_end(i + 1); attr = tokens[i + 2:stop]
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
                    stop = group_end(i + 2)
                    nested = attribute_base / explicit_path if explicit_path else directory / name
                    walk(i + 3, stop, nested, nested, test_only)
                    test_only, explicit_path, i = inherited_test, None, stop + 1
                    continue
            if text == 'include' and i + 3 < end and tokens[i + 1][1] == '!' and tokens[i + 2][1] == '(':
                stop = group_end(i + 2)
                if stop == i + 4 and tokens[i + 3][0] == 'string':
                    target = path.parent / tokens[i + 3][1]
                    if target.is_file(): edges.append((target.resolve(), test_only))
                i = stop + 1; continue
            if text == '{':
                stop = group_end(i)
                walk(i + 1, stop, directory, attribute_base, test_only)
                i = stop + 1; test_only, explicit_path = inherited_test, None
            elif text in ('(', '['): i = group_end(i) + 1
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
