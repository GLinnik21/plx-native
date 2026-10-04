#!/usr/bin/env python3
"""Reject English literals flowing into product text-rendering boundaries.

This is a deliberately bounded source check, not a Rust type checker. It tokenizes comments,
raw/escaped strings, balanced expressions and test-only items, then follows visible local and
constant bindings into the shared UI text APIs. Server values and catalog accessor calls contain
no literal UI prose. Function-return dataflow remains a review responsibility. Diagnostic field
names and values are both on screen and both boundaries; keycap names and machine/log APIs are not.

Beyond the widget constructors, the boundaries include the few places product text is stored
before a screen draws it: sign-in failures (`fail_login`, `fail_empty_home_roster`, `.error =`),
the playback verdict (`.verdict =`), subtitle-engine faults (`error_frame`, and `Err(` inside
`player/ass.rs`), the HUD kicker and busy read-out, tile labels, linked shelf headings, poster marks, and `title:` alongside the
`caption:`/`readout:`/`panel:` field initialisers. A prose `const` is followed
across files: a boundary that names another module's `const X: &str = "…"` is a finding here.
"""
from __future__ import annotations

from dataclasses import dataclass
import functools
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
    'Field': (0, 1), 'ValueChip': (0, 1),
}
# Associated functions and enum variants that carry display text: `Owner::name(...)`.
PATHS = {('Kicker', 'Context'): (0,), ('Busy', 'Readout'): (1,), ('TileLabel', 'titled'): (0, 1),
         # The shared linked shelf heading / Filmography entry: title and count are both drawn.
         ('LinkedHeading', 'entry'): (0, 1), ('LinkedHeading', 'heading'): (0, 1)}
# Calls by bare name (free function or method) whose argument is text a screen later shows.
CALLS = {'fail_login': (0,), 'fail_empty_home_roster': (0,), 'error_frame': (1,),
         # `widgets::poster_label(p, rect, radius, text, measure)`: the mark on a poster tile.
         'poster_label': (3,)}
# Calls that are boundaries only inside one file: `player/ass.rs` returns its faults as `Err(..)`,
# and each one becomes the subtitle read-out.
FILE_CALLS = {'rust-modules/media/src/player/ass.rs': {'Err': (0,)}}
# `x.field = <text>` assignments that store product text for a later draw.
ASSIGNED = ('error', 'verdict', 'play_verdict')
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


# Compiled once and matched at an offset (`pattern.match(source, i)`), never against `source[i:]`:
# slicing copied the rest of the file for every token, which made the scan quadratic in file size
# and was nearly all of this gate's wall time. `Match.end()` is absolute when an offset is given.
RAW_STRING = re.compile(r'(?:[bc])?r(#{0,255})"')
QUOTED_STRING = re.compile(r'(?:[bc])?"')
CHAR_LITERAL = re.compile(r"(?:b)?'(?:\\(?:u\{[^}]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'")
SPACE = re.compile(r'\s+')  # `\s` is `str.isspace()` for str patterns
WORD = re.compile(r'[A-Za-z_][A-Za-z_0-9]*')


@functools.lru_cache(maxsize=None)
def tokenize(source: str) -> list[Token]:
    """Token list for one file. Cached: `const_table` and `scan` tokenize the same file, and
    neither mutates the result."""
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
            i = SPACE.match(source, i).end()
        else:
            raw = RAW_STRING.match(source, i)
            quoted = QUOTED_STRING.match(source, i)
            char = CHAR_LITERAL.match(source, i)
            word = WORD.match(source, i)
            if raw:
                content = raw.end()
                ending = '"' + raw[1]
                end = source.find(ending, content)
                if end < 0: raise ValueError(f'unterminated raw string at line {line}')
                tokens.append(Token(source[content:end], ln, True))
                i = end + len(ending)
            elif quoted:
                content = quoted.end()
                i = content
                while i < len(source):
                    if source[i] == '\\': i += 2
                    elif source[i] == '"': break
                    else: i += 1
                tokens.append(Token(decode(source[content:i]), ln, True))
                i += 1
            elif char:
                i = char.end()  # a single character cannot contain a UI message
            elif word:
                tokens.append(Token(word[0], ln))
                i = word.end()
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


def prose_consts(source: str) -> dict[str, str]:
    """Module-level `const NAME: &str = "prose";` (or `&CStr`) definitions in one file."""
    tokens, found = tokenize(source), {}
    for i, t in enumerate(tokens[:-6]):
        if t.literal or t.text != 'const' or not re.fullmatch(r'[A-Z][A-Z0-9_]*', tokens[i + 1].text): continue
        j = i + 2
        while j < len(tokens) and tokens[j].text not in ('=', ';'): j += 1
        if j + 2 < len(tokens) and tokens[j].text == '=' and tokens[j + 1].literal \
                and tokens[j + 2].text == ';' and prose(tokens[j + 1].text):
            found[tokens[i + 1].text] = tokens[j + 1].text
    return found


def scan(source: str, calls: dict[str, tuple[int, ...]] | None = None,
         consts: dict[str, str] | None = None) -> list[Finding]:
    tokens = tokenize(source)
    calls = {**CALLS, **(calls or {})}
    consts = consts or {}
    scopes, stack = [], []
    # The innermost open bracket of any kind, so `f(a, title: &str)` (a parameter) is told apart
    # from `S { a, title: "…" }` (a field initialiser).
    inside, brackets = [], []
    for i, t in enumerate(tokens):
        scopes.append(tuple(stack))
        inside.append(brackets[-1] if brackets else '')
        if not t.literal and t.text == '{': stack.append(i)
        elif not t.literal and t.text == '}' and stack: stack.pop()
        if not t.literal and t.text in ('(', '[', '{'): brackets.append(t.text)
        elif not t.literal and t.text in (')', ']', '}') and brackets: brackets.pop()
    # Only simple named bindings. Destructuring and helper function returns stay review-owned.
    bindings: dict[str, list[tuple[int, int, int, tuple[int, ...], bool]]] = {}
    for i, t in enumerate(tokens):
        if t.literal or t.text not in ('let', 'const', 'static'): continue
        j = i + 1
        if j < len(tokens) and tokens[j].text == 'mut': j += 1
        if j >= len(tokens) or not re.fullmatch(r'[A-Za-z_]\w*', tokens[j].text): continue
        # `let Some(x) = …` / `let Point { .. } = …` are patterns, not a binding named `Some`.
        if j + 1 < len(tokens) and tokens[j + 1].text not in ('=', ':', ';'): continue
        name = tokens[j].text
        equal = j + 1
        while equal < len(tokens) and tokens[equal].text not in ('=', ';', '{'): equal += 1
        if equal == len(tokens) or tokens[equal].text != '=': continue
        end = equal + 1
        while end < len(tokens) and tokens[end].text != ';':
            if not tokens[end].literal and tokens[end].text in ('(', '[', '{'): end = matching(tokens, end)
            end += 1
        bindings.setdefault(name, []).append((i, equal + 1, end, scopes[i], t.text != 'let'))

    def compared(i: int) -> bool:
        # `codec == "hevc"` is a test on a value, not text flowing onward to the screen.
        before = i >= 2 and tokens[i - 1].text == '=' and tokens[i - 2].text in ('=', '!')
        after = i + 2 < len(tokens) and tokens[i + 1].text in ('=', '!') and tokens[i + 2].text == '='
        return before or after

    def literals(start: int, end: int, seen: frozenset[int] = frozenset()) -> list[Token]:
        result = []
        for i in range(start, end):
            t = tokens[i]
            if t.literal:
                if not compared(i): result.append(t)
            elif t.text in bindings and (i == start or tokens[i - 1].text not in ('.', '::')):
                choices = [b for b in bindings[t.text]
                           if b[0] not in seen and (b[0] < i or b[4])
                           and scopes[i][:len(b[3])] == b[3] and not (b[1] <= i < b[2])]
                if choices:
                    chosen = max(choices, key=lambda b: (len(b[3]), b[0]))
                    result.extend(literals(chosen[1], chosen[2], seen | {chosen[0]}))
                elif t.text in consts:
                    result.append(Token(consts[t.text], t.line, True))
            elif t.text in consts and (i + 1 == end or tokens[i + 1].text != '('):
                # Another module's prose constant, by path (`owner::TITLE`) or by import.
                result.append(Token(consts[t.text], t.line, True))
        return result

    found = set()
    for i, token in enumerate(tokens):
        if token.literal or token.text != '(' or i == 0: continue
        name = tokens[i - 1].text
        positions, boundary = (), ''
        if name == 'new' and i >= 4 and tokens[i - 2].text == '::':
            owner = tokens[i - 3].text
            positions, boundary = CONSTRUCTORS.get(owner, ()), owner + '::new'
        elif i >= 4 and tokens[i - 2].text == '::' and (tokens[i - 3].text, name) in PATHS:
            positions, boundary = PATHS[(tokens[i - 3].text, name)], tokens[i - 3].text + '::' + name
        elif i >= 2 and tokens[i - 2].text == '.' and name in METHODS:
            positions, boundary = METHODS[name], '.' + name
        elif name in calls and (i < 2 or tokens[i - 2].text != 'fn'):
            positions, boundary = calls[name], name
        if not positions: continue
        args = arguments(tokens, i, matching(tokens, i))
        for pos in positions:
            if pos < len(args):
                for literal in literals(*args[pos]):
                    if prose(literal.text): found.add(Finding(literal.line, literal.text, boundary))
    # Player error structs store their UI text before the HUD gets it. Treat these named
    # message fields as boundaries too; stable failure IDs live on other fields.
    for i, token in enumerate(tokens[:-2]):
        if token.literal or token.text not in ('caption', 'readout', 'panel', 'title'): continue
        if tokens[i + 1].text != ':' or i == 0 or tokens[i - 1].text not in ('{', ','): continue
        if inside[i] != '{': continue
        end = i + 2
        while end < len(tokens) and tokens[end].text not in (',', ';', '}'):
            if not tokens[end].literal and tokens[end].text in ('(', '[', '{'): end = matching(tokens, end)
            end += 1
        for literal in literals(i + 2, end):
            if prose(literal.text): found.add(Finding(literal.line, literal.text, token.text + ':'))
    for i, token in enumerate(tokens[:-2]):
        if token.literal or token.text not in ASSIGNED or i == 0 or tokens[i - 1].text != '.': continue
        if tokens[i + 1].text != '=' or tokens[i + 2].text == '=': continue
        end = i + 2
        while end < len(tokens) and tokens[end].text not in (';', '}'):
            if not tokens[end].literal and tokens[end].text in ('(', '[', '{'): end = matching(tokens, end)
            end += 1
        for literal in literals(i + 2, end):
            if prose(literal.text): found.add(Finding(literal.line, literal.text, '.' + token.text + ' ='))
    return sorted(found, key=lambda f: (f.line, f.boundary, f.text))


# The layer crates split out of rust-modules/src whose files this gate reads (docs/module-layers.md).
# `platform` holds three of the listed product files below (webos.rs, tv/device.rs, devcaps/dv.rs);
# `gfx` holds `overdraw.rs`, which this gate read as `ui/overdraw.rs` before the split, and `ui`
# is the whole of what this gate read as `rust-modules/src/ui`.
LAYER_SRCS = ('rust-modules/platform/src', 'rust-modules/gfx/src', 'rust-modules/ui/src')
# The constants table reads EVERY layer crate, not only the three above: a prose `const` is followed
# across files, and `rust-modules/src` held all of them before the split. Without `plex` (which
# also took `PlaybackQuality::label`, the quality row's text, out of the listed `route/decision.rs`)
# a boundary naming one of its constants would pass unread.
CONST_SRCS = (*LAYER_SRCS, 'rust-modules/base/src', 'rust-modules/machine/src', 'rust-modules/net/src',
              'rust-modules/plex/src', 'rust-modules/telemetry/src', 'rust-modules/data/src',
              'rust-modules/session/src',
              'rust-modules/media/src',
              'rust-modules/appkit/src',
              'rust-modules/screens/src')


def missing_roots(root: Path) -> list[str]:
    """The directories this gate walks that are not there. A listed FILE that is missing is reported
    by `main`; a missing DIRECTORY would make `rglob` yield nothing, which reads as "no findings".
    Every crate split so far moved files this gate named, and it skipped them without a word."""
    dirs = ['rust-modules/src', *CONST_SRCS]
    return [d for d in dirs if not (root / d).is_dir()]


def source_paths(root: Path):
    src = root / 'rust-modules/src'
    platform = root / LAYER_SRCS[0]
    for base, folders in ((root / LAYER_SRCS[2], ('',)), (root / 'rust-modules/appkit/src', ('',)), (root / 'rust-modules/screens/src', ('',))):
        for folder in folders:
            for path in sorted((base / folder).rglob('*.rs')):
                if path.name not in FIXTURES and not any('test' in part for part in path.relative_to(base).parts): yield path
    for rel in ('app/chrome.rs', 'app/diagnostics.rs', 'app/playback.rs', 'lab/toast.rs'):
        yield src / rel
    # `player/{ass, mod, shared, sidecar}.rs` and `route/{decision, plan}.rs` moved with the media split.
    for rel in ('player/ass.rs', 'player/mod.rs', 'player/shared.rs', 'player/sidecar.rs', 'route/decision.rs', 'route/plan.rs'):
        yield root / 'rust-modules/media/src' / rel
    for rel in ('webos.rs', 'tv/device.rs', 'devcaps/dv.rs'):
        yield platform / rel
    # `metadata.rs` and `person.rs` were listed under `rust-modules/src` until the data split.
    for rel in ('metadata.rs', 'person.rs'):
        yield root / 'rust-modules/data/src' / rel
    yield root / LAYER_SRCS[1] / 'overdraw.rs'
    # `PlaybackQuality::label` was `route/decision.rs`'s (listed above) until the plex split.
    yield root / 'rust-modules/plex/src/plex/session.rs'
    # `auth/owner.rs` (the session owner's read-outs) moved with the session split.
    yield root / 'rust-modules/session/src/auth/owner.rs'


def const_table(root: Path) -> dict[str, str]:
    """Every product module's prose constants, so a boundary in one file sees another's."""
    table = {}
    for base in ('rust-modules/src', *CONST_SRCS):
        for path in sorted((root / base).rglob('*.rs')):
            rel = path.relative_to(root / base)
            if path.name in FIXTURES or any('test' in part for part in rel.parts): continue
            table.update(prose_consts(path.read_text()))
    return table


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=ROOT)
    args = parser.parse_args()
    exception_path = args.root / 'ci/localization-exceptions.json'
    exceptions = json.loads(exception_path.read_text()) if exception_path.exists() else []
    used, failures = set(), []
    consts = const_table(args.root)
    failures.extend(f'{d}: a directory this gate reads is missing (moved? fix the path in ci/check-localization.py)'
                    for d in missing_roots(args.root))
    for path in source_paths(args.root):
        rel = path.relative_to(args.root).as_posix()
        if not path.is_file():
            # Not skipped: a listed file that moved would otherwise leave the gate green and unread.
            failures.append(f'{rel}: a listed product file is missing (moved? fix the path in ci/check-localization.py)')
            continue
        for f in scan(path.read_text(), FILE_CALLS.get(rel), consts):
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
