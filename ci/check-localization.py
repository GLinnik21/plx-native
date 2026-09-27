#!/usr/bin/env python3
"""Reject English literals flowing into product text-rendering boundaries.

This is a deliberately bounded source check, not a Rust type checker. It tokenizes comments,
raw/escaped strings, balanced expressions and test-only items, then follows visible local and
constant bindings into the shared UI text APIs. Server values and catalog accessor calls contain
no literal UI prose. Function-return dataflow remains a review responsibility. Diagnostic field
names, keycap names and machine/log APIs are not text boundaries; diagnostic values are.
"""
from __future__ import annotations

from dataclasses import dataclass
import argparse
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parent.parent

# Argument positions, not every literal in a widget call: routes, metrics and IDs stay stable.
CONSTRUCTORS = {
    'Label': (0,), 'Button': (0,), 'TextView': (0,), 'Row': (0,), 'Section': (0,),
    'Header': (0,), 'StatusOverlay': (1,), 'DocumentScreen': (1,), 'KeyHint': (0, 2),
    'Field': (1,), 'ValueChip': (0, 1),
}
METHODS = {'text': (0,), 'reason': (0,), 'action': (0,), 'detail': (0,),
           'trailing': (0,), 'lead_quiet': (0,), 'accessory': (0,), 'value': (0,)}
# Dedicated test and engineering fixtures are never part of the product text inventory.
FIXTURES = {'fixture.rs', 'testapp.rs', 'testpat.rs', 'replay.rs'}

@dataclass(frozen=True)
class Token:
    text: str
    line: int
    literal: bool = False


def decode(text: str) -> str:
    return re.sub(r'\\(?:u\{([0-9a-fA-F_]+)\}|x([0-9a-fA-F]{2})|(.))',
                  lambda m: chr(int((m[1] or m[2]).replace('_', ''), 16)) if m[1] or m[2]
                  else {'n': '\n', 'r': '\r', 't': '\t', '0': '\0'}.get(m[3], m[3]), text)


def tokenize(source: str) -> list[Token]:
    tokens = []
    i, line = 0, 1
    while i < len(source):
        start, ln = i, line
        if source.startswith('//', i):
            end = source.find('\n', i)
            i = len(source) if end < 0 else end
        elif source.startswith('/*', i):
            depth, i = 1, i + 2
            while i < len(source) and depth:
                if source.startswith('/*', i): depth, i = depth + 1, i + 2
                elif source.startswith('*/', i): depth, i = depth - 1, i + 2
                else: i += 1
        elif source[i].isspace():
            i += 1
        else:
            raw = re.match(r'(?:[bc])?r(#{0,255})"', source[i:])
            quoted = re.match(r'(?:[bc])?"', source[i:])
            char = re.match(r"(?:b)?'(?:\\(?:u\{[^}]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'", source[i:])
            word = re.match(r'[A-Za-z_][A-Za-z_0-9]*', source[i:])
            if raw:
                content = i + raw.end()
                ending = '"' + raw[1]
                end = source.find(ending, content)
                if end < 0: raise ValueError(f'unterminated raw string at line {line}')
                tokens.append(Token(source[content:end], ln, True))
                i = end + len(ending)
            elif quoted:
                content = i + quoted.end()
                i = content
                while i < len(source):
                    if source[i] == '\\': i += 2
                    elif source[i] == '"': break
                    else: i += 1
                tokens.append(Token(decode(source[content:i]), ln, True))
                i += 1
            elif char:
                i += char.end()  # a single character cannot contain a UI message
            elif word:
                tokens.append(Token(word[0], ln))
                i += word.end()
            else:
                punctuation = source[i:i+2]
                if punctuation not in ('::', '=>', '->'): punctuation = source[i]
                tokens.append(Token(punctuation, ln))
                i += len(punctuation)
        line += source[start:i].count('\n')
    return strip_tests(tokens)


def matching(tokens: list[Token], start: int) -> int:
    pairs = {'(': ')', '[': ']', '{': '}'}
    stack = [pairs[tokens[start].text]]
    for i in range(start + 1, len(tokens)):
        t = tokens[i]
        if t.literal: continue
        if t.text in pairs: stack.append(pairs[t.text])
        elif t.text in pairs.values():
            if t.text != stack.pop(): raise ValueError(f'unbalanced expression at line {t.line}')
            if not stack: return i
    raise ValueError(f'unterminated expression at line {tokens[start].line}')


def strip_tests(tokens: list[Token]) -> list[Token]:
    out, i = [], 0
    while i < len(tokens):
        if tokens[i].text == '#' and i + 1 < len(tokens) and tokens[i + 1].text == '[':
            end = matching(tokens, i + 1)
            attr = ''.join(t.text for t in tokens[i + 2:end])
            if attr in ('cfg(test)', 'test', 'cfg(all(test,feature=hostsim))'):
                # Skip the whole annotated item, including attributes between cfg and its body.
                j = end + 1
                while j < len(tokens):
                    if tokens[j].text == '#' and j + 1 < len(tokens) and tokens[j + 1].text == '[':
                        j = matching(tokens, j + 1) + 1
                    elif tokens[j].text in ('(', '['):
                        j = matching(tokens, j) + 1
                    elif tokens[j].text == '{':
                        i = matching(tokens, j) + 1
                        break
                    elif tokens[j].text in (';', ','):
                        i = j + 1
                        break
                    elif tokens[j].text == '}':
                        i = j
                        break
                    else: j += 1
                else: i = len(tokens)
                continue
        out.append(tokens[i]); i += 1
    return out


def arguments(tokens: list[Token], start: int, end: int) -> list[tuple[int, int]]:
    args, first, i = [], start + 1, start + 1
    while i < end:
        if not tokens[i].literal and tokens[i].text in ('(', '[', '{'):
            i = matching(tokens, i) + 1
        elif tokens[i].text == ',' and not tokens[i].literal:
            args.append((first, i)); first, i = i + 1, i + 1
        else: i += 1
    if first < end: args.append((first, end))
    return args


def prose(text: str) -> bool:
    # Punctuation and numeric readouts are not messages. A word of any length is: "On" matters.
    # Format placeholders themselves are server values/technical numbers, not source prose.
    text = re.sub(r'\{[^{}]*\}', '', text)
    return any(c.isalpha() for c in text)


@dataclass(frozen=True)
class Finding:
    line: int
    text: str
    boundary: str


def scan(source: str) -> list[Finding]:
    tokens = tokenize(source)
    scopes, stack = [], []
    for i, t in enumerate(tokens):
        scopes.append(tuple(stack))
        if not t.literal and t.text == '{': stack.append(i)
        elif not t.literal and t.text == '}' and stack: stack.pop()
    # Only simple named bindings. Destructuring and helper function returns stay review-owned.
    bindings: dict[str, list[tuple[int, int, int, tuple[int, ...], bool]]] = {}
    for i, t in enumerate(tokens):
        if t.literal or t.text not in ('let', 'const', 'static'): continue
        j = i + 1
        if j < len(tokens) and tokens[j].text == 'mut': j += 1
        if j >= len(tokens) or not re.fullmatch(r'[A-Za-z_]\w*', tokens[j].text): continue
        name = tokens[j].text
        equal = j + 1
        while equal < len(tokens) and tokens[equal].text not in ('=', ';', '{'): equal += 1
        if equal == len(tokens) or tokens[equal].text != '=': continue
        end = equal + 1
        while end < len(tokens) and tokens[end].text != ';':
            if not tokens[end].literal and tokens[end].text in ('(', '[', '{'): end = matching(tokens, end)
            end += 1
        bindings.setdefault(name, []).append((i, equal + 1, end, scopes[i], t.text != 'let'))

    def literals(start: int, end: int, seen: frozenset[int] = frozenset()) -> list[Token]:
        result = []
        for i in range(start, end):
            t = tokens[i]
            if t.literal:
                result.append(t)
            elif t.text in bindings and (i == start or tokens[i - 1].text not in ('.', '::')):
                choices = [b for b in bindings[t.text]
                           if b[0] not in seen and (b[0] < i or b[4])
                           and scopes[i][:len(b[3])] == b[3] and not (b[1] <= i < b[2])]
                if choices:
                    chosen = max(choices, key=lambda b: (len(b[3]), b[0]))
                    result.extend(literals(chosen[1], chosen[2], seen | {chosen[0]}))
        return result

    found = set()
    for i, token in enumerate(tokens):
        if token.literal or token.text != '(' or i == 0: continue
        name = tokens[i - 1].text
        positions, boundary = (), ''
        if name == 'new' and i >= 4 and tokens[i - 2].text == '::':
            owner = tokens[i - 3].text
            positions, boundary = CONSTRUCTORS.get(owner, ()), owner + '::new'
        elif i >= 2 and tokens[i - 2].text == '.' and name in METHODS:
            positions, boundary = METHODS[name], '.' + name
        if not positions: continue
        args = arguments(tokens, i, matching(tokens, i))
        for pos in positions:
            if pos < len(args):
                for literal in literals(*args[pos]):
                    if prose(literal.text): found.add(Finding(literal.line, literal.text, boundary))
    # Player error structs store their UI text before the HUD gets it. Treat these named
    # message fields as boundaries too; stable failure IDs live on other fields.
    for i, token in enumerate(tokens[:-2]):
        if token.literal or token.text not in ('caption', 'readout', 'panel'): continue
        if tokens[i + 1].text != ':' or i == 0 or tokens[i - 1].text not in ('{', ','): continue
        end = i + 2
        while end < len(tokens) and tokens[end].text not in (',', ';', '}'):
            if not tokens[end].literal and tokens[end].text in ('(', '[', '{'): end = matching(tokens, end)
            end += 1
        for literal in literals(i + 2, end):
            if prose(literal.text): found.add(Finding(literal.line, literal.text, token.text + ':'))
    return sorted(found, key=lambda f: (f.line, f.boundary, f.text))


def source_paths(root: Path):
    src = root / 'rust-modules/src'
    for folder in ('screens', 'ui'):
        for path in sorted((src / folder).rglob('*.rs')):
            if path.name not in FIXTURES and not any('test' in part for part in path.relative_to(src).parts): yield path
    for rel in ('app/chrome.rs', 'app/diagnostics.rs', 'player/mod.rs', 'player/shared.rs'):
        yield src / rel


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=ROOT)
    args = parser.parse_args()
    exception_path = args.root / 'ci/localization-exceptions.json'
    exceptions = json.loads(exception_path.read_text()) if exception_path.exists() else []
    used, failures = set(), []
    for path in source_paths(args.root):
        if not path.exists(): continue
        rel = path.relative_to(args.root).as_posix()
        for f in scan(path.read_text()):
            hit = next((i for i, e in enumerate(exceptions)
                        if e.get('path') == rel and e.get('text') == f.text
                        and e.get('boundary') == f.boundary and e.get('reason')), None)
            if hit is not None: used.add(hit)
            else: failures.append(f'{rel}:{f.line}: {f.boundary}: {f.text!r}')
    failures.extend(f'unused localization exception: {e}' for i, e in enumerate(exceptions) if i not in used)
    if failures:
        print('Localization inventory: move app-owned text to a scoped catalog key:', file=sys.stderr)
        print('\n'.join(failures), file=sys.stderr)
        return 1
    print('Localization inventory: product text boundaries passed')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
