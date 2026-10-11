#!/usr/bin/env python3
"""The list-cap registry: every cap-shaped bound on a collection says what it bounds and how content
past it is reached.

The owner's rule: a hard cap on what the user can REACH in a list (N items, N rows, N episodes, N
tabs) is never acceptable; a bound on WORK (per frame, per request, a memory window, concurrency) is
fine. This gate does not decide which is which. It makes every cap-shaped site in non-test Rust under
`rust-modules/*/src` and `rust-modules/src` carry a decision, in `ci/list-caps.ini`, that a reviewer
can read and a later change cannot silently reverse.

WHAT IS A HIT (lexical, over comment- and string-blanked tokens, like the request lint):
  * `.take(ARG)`      ARG is a literal or an ALL_CAPS constant (with path prefixes).
  * `.truncate(…)`    any argument: every truncation of a Vec or String is a decision.
  * `.min(ARG)`       ARG is a literal or an ALL_CAPS constant, AND either the receiver ends in
                      `.len()` / `.count()` or the constant's name is cap-worded (MAX, CAP, LIMIT,
                      WINDOW, PAGE, PREVIEW, BUDGET, DEEP, CAST, CHUNK, ROWS, LINES, ITEMS, CARDS, TRACKS, TABS,
                      SHOWN, COUNT). A bare `x.min(8)` on a coordinate or a
                      duration is geometry, not a list bound, and is not a hit.
  * `const`/`static` NAME: of an integer type with a cap-worded NAME (the words above, split on `_`).
  * `[T; NAME]`       a fixed array whose length is a cap-worded constant: a list with a ceiling.
  * `[..ARG]`         a slice cut by a literal or an ALL_CAPS constant.
A use of a constant that is itself a hit as a definition (`.take(PEOPLE_CAST)` where
`const PEOPLE_CAST` is registered) is covered by the definition's entry and is not a second hit:
the definition's reason must therefore describe every use of the constant.

WHAT IS NOT: a `.min(` on geometry or time, a `.take(n)` with a variable, `.skip`/`.nth`, and caps
spelled in arithmetic (`MAX * 2`): none of those can be told from a work bound lexically, and a
registry that lists them is a registry nobody reads. Test code is out of scope: whole test files
(rust_test_modules.wholly_test_files, `*_tests.rs`, `tests.rs`) and every `#[cfg(test)]` item.

Each hit is keyed by (file relative to rust-modules/, enclosing symbol): the function it sits in,
else the const/static/struct/impl it belongs to. Line numbers are not part of the key, so edits
elsewhere in the file do not break it; several hits in one symbol share one entry, whose reason must
cover all of them. `ci/list-caps.ini` has one section per key:

    class   work        bounds work or memory (per frame, per request, a window, concurrency);
                        every item past it is still reachable
            preview     a summary length whose full list is reachable: `route` names how
            format      a limit of a format, protocol or hardware, not of a list the user browses;
                        the reason says which
            not-a-list  text length, retries, geometry, a buffer
            reach       a real cap on reachable content. Allowed ONLY with `status = open` and a
                        reason that says what the user loses; printed loudly, counted in the summary
    reason  one sentence
    route   required for `preview`: the screen/gesture that reaches the full list
    status  `open`, required for and only for `reach`

Fails on: a hit with no section (the message gives the section to add), a section matching nothing
(stale: remove or re-key it), an unknown class, a `preview` with no route, a `reach` that is not
`status = open` or has no reason, `status` on anything but `reach`.

    ci/check-list-caps.py            # the gate (check-python-steps.txt runs it)
    ci/check-list-caps.py --sites    # every hit, its symbol, and its class
"""
from pathlib import Path
import argparse
import collections
import configparser
import importlib.util
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_test_modules  # noqa: E402

_spec = importlib.util.spec_from_file_location('check_plex_requests',
                                               Path(__file__).with_name('check-plex-requests.py'))
_plex = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_plex)
without_test_items, code, impl_type = _plex.without_test_items, _plex.code, _plex.impl_type

REPO = Path(__file__).resolve().parent.parent
MODULES = REPO / 'rust-modules'
INI = REPO / 'ci' / 'list-caps.ini'
CLASSES = ('work', 'preview', 'format', 'not-a-list', 'reach')
COUNTED = ('work', 'preview', 'format', 'not-a-list')
KEYS = {'class', 'reason', 'route', 'status'}
WORD = re.compile(r'[A-Za-z_][A-Za-z_0-9]*\Z')
CAPS_NAME = re.compile(r'[A-Z][A-Z_0-9]*\Z')
CAP_WORDS = frozenset({'MAX', 'CAP', 'LIMIT', 'WINDOW', 'PAGE', 'PREVIEW', 'BUDGET', 'DEEP', 'CAST', 'CHUNK',
                       'ROWS', 'LINES', 'ITEMS', 'CARDS', 'TRACKS', 'TABS', 'SHOWN', 'COUNT'})
CONST_TYPES = frozenset({'usize', 'u32', 'u16', 'u64', 'i32', 'i64', 'isize'})
LITERAL_SUFFIX = re.compile(r'_?[0-9_]*(usize|u8|u16|u32|u64|i32|i64|isize)?\Z')
ITEM_START = frozenset({None, ';', '{', '}', ']', 'unsafe', 'pub', ')'})
TEST_FILE_NAMES = ('tests.rs',)


def cap_worded(name):
    return bool(CAPS_NAME.match(name)) and bool(CAP_WORDS & set(name.split('_')))


def group_end(tokens, i):
    """Index of the bracket that closes the one opened at token i, else len(tokens)."""
    opener = code(tokens, i)
    closer = {'(': ')', '[': ']', '{': '}'}[opener]
    depth = 0
    for j in range(i, len(tokens)):
        text = code(tokens, j)
        if text == opener: depth += 1
        elif text == closer:
            depth -= 1
            if depth == 0: return j
    return len(tokens)


def literal_at(tokens, i, stop):
    """The end (exclusive) of an integer literal starting at token i, or None."""
    j = i
    while j < stop and len(code(tokens, j) or '') == 1 and code(tokens, j).isdigit(): j += 1
    if j == i: return None
    if j < stop and WORD.match(code(tokens, j) or '') and LITERAL_SUFFIX.match(code(tokens, j)): j += 1
    return j


def cap_arg(tokens, lo, hi):
    """Does tokens[lo:hi] read as a literal or an ALL_CAPS constant (with path prefixes)? Returns
    (ok, constant name or None)."""
    if lo >= hi: return False, None
    if literal_at(tokens, lo, hi) == hi: return True, None
    name, j = None, lo
    while j < hi:
        text = code(tokens, j)
        if text is None: return False, None
        if text == ':': j += 1; continue
        if not WORD.match(text): return False, None
        if code(tokens, j + 1) == ':' and code(tokens, j + 2) == ':': j += 1; continue  # a path prefix
        if CAPS_NAME.match(text) and j == hi - 1: name = text
        else: return False, None
        j += 1
    return (name is not None), name


class Scope:
    __slots__ = ('kind', 'name')

    def __init__(self, kind, name):
        self.kind, self.name = kind, name


def symbol_of(stack, item):
    for scope in reversed(stack):
        if scope.kind == 'fn': return scope.name
    if item: return item
    for scope in reversed(stack):
        if scope.name: return scope.name
    return '<item>'


def hits_in(tokens):
    """([(symbol, kind, constant or None)], [constant]): the cap-shaped sites in one file's tokens,
    and the cap-worded constants it defines."""
    found, defs = [], []
    stack, pending, depth = [], None, 0
    for i in range(len(tokens)):
        text = code(tokens, i)
        if text is None: continue
        prev, nxt = code(tokens, i - 1), code(tokens, i + 1)
        if text in ('(', '['): depth += 1
        elif text in (')', ']'): depth -= 1
        if text == 'fn' and nxt and WORD.match(nxt) and prev != '.':
            pending = ('fn', nxt, depth)
        elif text in ('struct', 'enum', 'trait') and nxt and WORD.match(nxt) and prev in ITEM_START:
            pending = ('struct', nxt, depth)
        elif text == 'impl' and prev in ITEM_START:
            pending = ('impl', impl_type(tokens, i), depth)
        elif text == '{':
            kind, name = (pending[0], pending[1]) if pending else ('block', None)
            stack.append(Scope(kind, name)); pending = None
        elif text == '}':
            if stack: stack.pop()
        elif text == ';' and pending and pending[2] == depth:
            pending = None
        if text in ('const', 'static') and nxt and CAPS_NAME.match(nxt) and code(tokens, i + 2) == ':':
            if code(tokens, i + 3) in CONST_TYPES and cap_worded(nxt):
                found.append((symbol_of(stack, nxt), 'const', None)); defs.append(nxt)
        elif text in ('take', 'truncate', 'min') and prev == '.' and nxt == '(':
            close = group_end(tokens, i + 1)
            ok, name = cap_arg(tokens, i + 2, close)
            if text == 'truncate':
                found.append((symbol_of(stack, None), 'truncate', name))
                continue
            if not ok: continue
            if text == 'min':
                after_len = code(tokens, i - 2) == ')' and code(tokens, i - 3) == '(' \
                    and code(tokens, i - 4) in ('len', 'count')
                if not (after_len or (name and cap_worded(name))): continue
            found.append((symbol_of(stack, None), text, name))
        elif text == '.' and nxt == '.' and prev == '[':
            ok, name = cap_arg(tokens, i + 2, group_end(tokens, i - 1))
            if ok: found.append((symbol_of(stack, None), 'slice', name))
        elif text == ';' and nxt and cap_worded(nxt) and code(tokens, i + 2) == ']' and depth > 0:
            found.append((symbol_of(stack, None), 'array', nxt))
    return found, defs


def production_files(src_root):
    tests = rust_test_modules.wholly_test_files(src_root)
    for path in sorted(src_root.rglob('*.rs')):
        if path.resolve() in tests: continue
        if path.name.endswith('_tests.rs') or path.name in TEST_FILE_NAMES: continue
        if 'tests' in path.relative_to(src_root).parts[:-1]: continue
        yield path


def src_roots(modules):
    roots = [d / 'src' for d in sorted(modules.iterdir()) if (d / 'src').is_dir() and d.name != 'src']
    if (modules / 'src').is_dir(): roots.append(modules / 'src')
    return roots


def read_hits(modules=MODULES):
    """{(file, symbol): [kind]} with file relative to rust-modules/ (`screens/src/detail/mod.rs`),
    after dropping uses of constants that are themselves registered definitions."""
    raw, all_defs = {}, set()
    for root in src_roots(modules):
        for path in production_files(root):
            tokens = without_test_items(rust_test_modules.lex(path.read_text()))
            found, defs = hits_in(tokens)
            all_defs.update(defs)
            rel = path.relative_to(modules).as_posix()
            for symbol, kind, name in found:
                raw.setdefault((rel, symbol), []).append((kind, name))
    sites = {}
    for key, entries in raw.items():
        kept = [kind for kind, name in entries if not (name and name in all_defs and kind != 'const')]
        if kept: sites[key] = sorted(set(kept))
    return sites


def section_name(file, symbol):
    return f'{file}::{symbol}'


def read_registry(path):
    parser = configparser.ConfigParser(interpolation=None, comment_prefixes=('#',),
                                       inline_comment_prefixes=None)
    try:
        with open(path, encoding='utf-8') as f:
            parser.read_file(f)
    except (OSError, configparser.Error) as error:
        return {}, [f'{Path(path).name} does not parse: {error}']
    return {name: dict(parser[name]) for name in parser.sections()}, []


def entry_problems(name, section):
    problems = []
    unknown = set(section) - KEYS
    if unknown: problems.append(f'[{name}] has unknown key(s) {sorted(unknown)}')
    cls = section.get('class', '')
    if cls not in CLASSES:
        problems.append(f'[{name}] class = {cls!r} is not one of {", ".join(CLASSES)}')
    if not section.get('reason', '').strip():
        problems.append(f'[{name}] has an empty reason: one sentence saying why this class')
    if cls == 'preview' and not section.get('route', '').strip():
        problems.append(f'[{name}] is a preview with no route: add `route = <the screen or gesture that '
                        'reaches the full list>`, or class it `reach` with status = open')
    if cls != 'preview' and 'route' in section:
        problems.append(f'[{name}] has route, which only a preview names')
    if cls == 'reach' and section.get('status', '').strip() != 'open':
        problems.append(f'[{name}] is a reach cap and so needs `status = open` and a reason saying what the '
                        'user loses: a cap on reachable content is tracked, never decided. Remove the cap, '
                        'or prove a route and class it preview')
    if cls != 'reach' and 'status' in section:
        problems.append(f'[{name}] has status, which only a reach entry carries')
    return problems


def check(modules=MODULES, ini=INI):
    """(sites, registry, [problem])."""
    sites = read_hits(modules)
    registry, problems = read_registry(ini)
    for key in sorted(sites):
        if section_name(*key) in registry: continue
        problems.append(f'cap site {section_name(*key)} ({", ".join(sites[key])}) has no section in '
                        f'ci/list-caps.ini. Read the code, then add:\n    [{section_name(*key)}]\n'
                        '    class = work | preview | format | not-a-list | reach\n'
                        '    reason = <one sentence>\n'
                        '    route = <preview only: how the full list is reached>\n'
                        '    status = open   (reach only)')
    wanted = {section_name(*key) for key in sites}
    for name in sorted(registry):
        if name not in wanted:
            problems.append(f'[{name}] matches no cap site in rust-modules: remove the section (or re-key '
                            'it to the symbol that now holds the cap)')
            continue
        problems += entry_problems(name, registry[name])
    return sites, registry, problems


def summary_lines(sites, registry):
    """The report of a passing run: each open reach cap loudly, then the one-line summary."""
    counts = collections.Counter(registry[section_name(*key)]['class'] for key in sites)
    lines = [f'check-list-caps: REACH CAP (open): {section_name(*key)}: {registry[section_name(*key)]["reason"]}'
             for key in sorted(sites) if registry[section_name(*key)]['class'] == 'reach']
    lines.append(f'check-list-caps: {len(sites)} sites, every one classed: '
                 + ', '.join(f'{c} {counts.get(c, 0)}' for c in COUNTED) + f', reach(open) {counts.get("reach", 0)}')
    return lines


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--sites', action='store_true', help='print every cap site and its class')
    args = parser.parse_args(argv)
    sites, registry, problems = check()
    if args.sites:
        for key in sorted(sites):
            name = section_name(*key)
            print(f'{name}  [{", ".join(sites[key])}]  {registry.get(name, {}).get("class", "UNCLASSED")}')
        return 0
    if problems:
        print('check-list-caps: FAIL')
        for problem in problems: print(f'  {problem}')
        return 1
    for line in summary_lines(sites, registry): print(line)
    return 0


if __name__ == '__main__':
    sys.exit(main())
