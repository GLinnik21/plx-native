#!/usr/bin/env python3
"""The Plex request registry: every request the plex layer can make says whether it pages.

A REQUEST SITE is a call to one of the HTTP doors of rust-modules/plex/src/plex/, made from a
non-test function. The doors are the request methods of the server client (`impl Client`, in
client.rs: `self.get_json(…)` and its siblings, called from any receiver), the request methods of
`impl AccountClient` (plex.tv: `self.get(…)`, `self.get_evidence(…)`, `self.post_raw(…)`, called
as `self.` inside that impl), and the transport calls the account code makes directly
(`plx_net::net::request_evidence(…)`, `http::request_*`). A call from inside a door's own body is
plumbing — the outer caller is the site — so a door is never a site for its own body.

Each site is keyed by (file, enclosing function), and `ci/plex-requests.ini` has one section per
key saying what the request is (`docs/pms-api.md`, "Paging, observed"):

    class     single    one object, not a list
              paged     asks for a window: the function reaches paged_path or PageReq
              bounded   a list the server bounds by an explicit parameter (`parameter =`, the
                        name: `limit`, `count`, …)
              whole     a list read in one response, on purpose
    reason    one line: why this class
    evidence  `path:line` in the repo, or `docs/pms-api.md: <text>` where <text> is a line of that
              doc's "Paging, observed" section
    parameter the query parameter that bounds a `bounded` site
    via       for a `paged` site whose window another function of the same file builds (a path
              helper, or the listing wrapper that calls it): the names of those functions. Each
              must reach paged_path or PageReq, and must be linked to the site by a call either way.
              A `paged` site with no paging name in its own body and no `via` is an error.

`first-page` is not a class: a window that is read from its start and never continued is a
bound or a whole list, and the registry says which. A new request site fails the gate until a
section names it; a section for a site that no longer exists fails the gate as stale.

Lexical, like the other structure gates (rust_test_modules.py's lexer, ~0.4 s). Test code is out
of the enumeration: whole test files (rust_test_modules.wholly_test_files) and every
`#[cfg(test)]` item of the rest.

    ci/check-plex-requests.py            # the gate (check-python-steps.txt runs it)
    ci/check-plex-requests.py --sites    # every site, its doors, and its class
"""
from pathlib import Path
import argparse
import collections
import configparser
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_test_modules  # noqa: E402

REPO = Path(__file__).resolve().parent.parent
PLEX_SRC = REPO / 'rust-modules' / 'plex' / 'src'
PLEX_DIR = PLEX_SRC / 'plex'
INI = REPO / 'ci' / 'plex-requests.ini'
PMS_DOC = REPO / 'docs' / 'pms-api.md'
PAGING_HEADING = '## Paging, observed'
CLASSES = ('single', 'paged', 'bounded', 'whole')
PAGING_NAMES = frozenset({'paged_path', 'PageReq'})
EVIDENCE_LINE = re.compile(r'(?P<path>[A-Za-z0-9_./-]+\.rs):(?P<line>[0-9]+)\Z')
WORD = re.compile(r'[A-Za-z_][A-Za-z_0-9]*\Z')

# The server client's request methods, which the rest of the plex layer calls from any receiver.
CLIENT_DOORS = frozenset({
    'get_json', 'get_json_with_headers', 'get_json_with_headers_until', 'get_json_status',
    'get_bytes', 'get_sidecar_bytes', 'get_void', 'get_ok', 'get_status', 'get_status_until',
    'post_status', 'post_status_until', 'put', 'post_ok', 'post_json', 'fetch_built',
    'fetch_built_outcome',
})
# The server client's private transport. A call is a site only as `self.name(` inside `impl Client`,
# and never from a door's own body (`get_json_status` reads through `send_bulk`).
CLIENT_TRANSPORT = frozenset({'send', 'send_until', 'send_bulk', 'body_2xx', 'body_2xx_bulk'})
# `impl AccountClient`'s request methods, called as `self.name(` inside that impl.
ACCOUNT_DOORS = frozenset({'get', 'get_evidence', 'get_evidence_with', 'get_raw', 'post_raw'})
# Transport calls made directly, path-qualified by the crate or module that owns them.
FREE_DOORS = {
    'request_evidence': frozenset({'net'}),
    'request_probe': frozenset({'http'}),
    'request_bulk': frozenset({'http'}),
    'request_until_outcome': frozenset({'http'}),
}
DOOR_NAMES = CLIENT_DOORS | CLIENT_TRANSPORT | ACCOUNT_DOORS | frozenset(FREE_DOORS)
# The impl a method door lives in. A free door has none.
DOOR_HOME = {**dict.fromkeys(CLIENT_DOORS | CLIENT_TRANSPORT, 'Client'),
             **dict.fromkeys(ACCOUNT_DOORS, 'AccountClient')}
# Tokens after which an `impl` or `fn` begins an item, rather than being a type in a signature.
ITEM_START = frozenset({None, ';', '{', '}', ']', 'unsafe'})


def code(tokens, i):
    """The text of token i when it is code, else None."""
    if 0 <= i < len(tokens) and tokens[i][0] == 'code': return tokens[i][1]
    return None


def item_end(tokens, ends, j):
    """The index that closes the item beginning at token j: its `;`, or the `}` of its body."""
    n = len(tokens)
    while j < n:
        if tokens[j][0] == 'code':
            text = tokens[j][1]
            if text == '#' and code(tokens, j + 1) == '[' and (j + 1) in ends:
                j = ends[j + 1] + 1; continue
            if text == ';': return j
            if text == '{': return ends.get(j, n - 1)
            if text in ('(', '['): j = ends.get(j, n - 1) + 1; continue
        j += 1
    return n - 1


def without_test_items(tokens):
    """The tokens with every item a `#[cfg(test)]` attribute gates removed."""
    ends = rust_test_modules.group_ends(tokens)
    kept, i, n = [], 0, len(tokens)
    while i < n:
        if code(tokens, i) == '#' and code(tokens, i + 1) == '[' and (i + 1) in ends:
            close = ends[i + 1]
            attr = tokens[i + 2:close]
            if attr and attr[0][1] == 'cfg' and rust_test_modules.cfg_without_tests(attr[2:-1]) == {False}:
                i = item_end(tokens, ends, close + 1) + 1
                continue
        kept.append(tokens[i]); i += 1
    return kept


def impl_type(tokens, i):
    """The type an `impl` at token i implements on: `impl<T> Tr for X<T>` → X, `impl X {` → X."""
    depth, angle, first, after_for, j = 0, 0, None, None, i + 1
    while j < len(tokens):
        text = code(tokens, j)
        if text is None: j += 1; continue
        if text in ('{', ';') and depth == 0: break
        if text == 'where' and depth == 0: break
        if text in ('(', '['): depth += 1
        elif text in (')', ']'): depth -= 1
        elif text == '<': angle += 1
        elif text == '>': angle -= 1
        elif WORD.match(text) and angle == 0 and depth == 0:
            if text == 'for' and after_for is None: after_for = ''
            elif after_for == '' and WORD.match(text): after_for = text
            elif first is None and text not in ('unsafe', 'dyn'): first = text
        j += 1
    return after_for or first


class Scope:
    """A function body (or other brace group) on the walk's stack."""
    __slots__ = ('kind', 'name', 'start', 'end')

    def __init__(self, kind, name, start):
        self.kind, self.name, self.start, self.end = kind, name, start, None


def request_sites(tokens):
    """(sites, functions) in one file's tokens: sites as (function name, door name, function scope or
    None) for each request, and functions as every closed function scope, for read_sites' table."""
    # The walk keeps one Scope per open brace. A `fn` or `impl` item names the scope of its body; a
    # `;` at the depth the item began ends it bodiless (a trait's `fn f();`). Parentheses and
    # brackets are counted so the `;` of `[u8; 4]` in a signature is not taken for that.
    stack, pending, depth, found, closed = [], None, 0, [], []
    for i, (kind, text) in enumerate(tokens):
        if kind != 'code': continue
        prev, nxt = code(tokens, i - 1), code(tokens, i + 1)
        if text in ('(', '['): depth += 1
        elif text in (')', ']'): depth -= 1
        if text == 'fn' and nxt is not None and WORD.match(nxt):
            pending = ('fn', nxt, depth)
        elif text == 'impl' and prev in ITEM_START:
            pending = ('impl', impl_type(tokens, i), depth)
        elif text == '{':
            kind_, name = (pending[0], pending[1]) if pending else ('block', None)
            stack.append(Scope(kind_, name, i)); pending = None
        elif text == '}':
            if stack:
                scope = stack.pop()
                scope.end = i
                if scope.kind == 'fn': closed.append(scope)
        elif text == ';' and pending and pending[2] == depth:
            pending = None
        elif text in DOOR_NAMES and nxt == '(':
            fn = next((s for s in reversed(stack) if s.kind == 'fn'), None)
            owner = next((s for s in reversed(stack) if s.kind == 'impl'), None)
            door = door_for(tokens, i, text, prev, fn, owner)
            if door is not None:
                found.append((fn.name if fn else '<item>', door, fn))
    # A function still open at the end of the file ends there (an unbalanced file is a rustc error).
    closed += [scope for scope in stack if scope.kind == 'fn']
    return found, closed


def door_for(tokens, i, text, prev, fn, owner):
    """The door name when the call at token i is a request site, else None."""
    impl = owner.name if owner else None
    # Plumbing: a door's own body, where the door is in its home impl (or is a free door). A fn of
    # the same name in another impl is an ordinary function and its calls are sites.
    if fn is not None and fn.name in DOOR_NAMES and DOOR_HOME.get(fn.name, impl) == impl:
        return None
    receiver = code(tokens, i - 2)
    if prev == '.':
        if text in CLIENT_DOORS: return text
        if text in CLIENT_TRANSPORT and receiver == 'self' and impl == 'Client': return text
        if text in ACCOUNT_DOORS and receiver == 'self' and impl == 'AccountClient': return text
        return None
    # rust_test_modules.lex yields each `:` on its own, so `net::request_evidence` is `net : : name`.
    if prev == ':' and code(tokens, i - 2) == ':' and text in FREE_DOORS \
            and code(tokens, i - 3) in FREE_DOORS[text]:
        return text
    return None


def calls_in(body):
    """The names a token run calls: every word followed by `(`, whatever its receiver."""
    return {body[j][1] for j in range(len(body) - 1)
            if body[j][0] == 'code' and body[j + 1] == ('code', '(') and WORD.match(body[j][1])}


def read_sites(repo=REPO, plex_dir=None, plex_src=None):
    """(sites, functions) for the plex layer's non-test Rust.

    sites: {(file relative to plex_dir, function name): {'doors': set, 'paging': bool}}, one entry per
    request site. functions: {(file, function name): {'paging': bool, 'calls': set}} for every
    function, site or not: `paging` is whether its body names paged_path or PageReq, `calls` the names
    it calls. A name defined twice in one file is one entry (the union), which is the only resolution
    a textual gate can make.
    """
    plex_src = plex_src or repo / 'rust-modules' / 'plex' / 'src'
    plex_dir = plex_dir or plex_src / 'plex'
    test_files = rust_test_modules.wholly_test_files(plex_src)
    sites, functions = {}, {}
    for path in sorted(plex_dir.rglob('*.rs')):
        if path.resolve() in test_files: continue
        tokens = without_test_items(rust_test_modules.lex(path.read_text()))
        rel = path.relative_to(plex_dir).as_posix()
        found, closed = request_sites(tokens)
        for name, door, _ in found:
            sites.setdefault((rel, name), {'doors': set(), 'paging': False})['doors'].add(door)
        for fn in closed:
            body = tokens[fn.start:fn.end if fn.end is not None else len(tokens)]
            entry = functions.setdefault((rel, fn.name), {'paging': False, 'calls': set()})
            entry['paging'] |= any(t[0] == 'code' and t[1] in PAGING_NAMES for t in body)
            entry['calls'] |= calls_in(body)
    for key, entry in sites.items():
        entry['paging'] = functions.get(key, {}).get('paging', False)
    return sites, functions


def section_name(file, name):
    return f'{file}::{name}'


def pms_paging_lines(doc):
    """The lines of docs/pms-api.md's "Paging, observed" section (up to the next `## ` heading)."""
    if not doc.is_file(): return []
    lines = doc.read_text().splitlines()
    if PAGING_HEADING not in lines: return []
    start = lines.index(PAGING_HEADING) + 1
    end = next((i for i in range(start, len(lines)) if lines[i].startswith('## ')), len(lines))
    return lines[start:end]


def check_evidence(text, repo, paging_lines):
    """A problem with the evidence string, or None when it names a real line."""
    located = EVIDENCE_LINE.match(text)
    if located:
        path = repo / located['path']
        if not path.is_file(): return f'evidence names {located["path"]}, which is not a file in the repo'
        count = len(path.read_text().splitlines())
        if not 1 <= int(located['line']) <= count:
            return f'evidence names {located["path"]}:{located["line"]}, past the end of a {count}-line file'
        return None
    if text.startswith('docs/pms-api.md:'):
        fragment = text.split(':', 1)[1].strip()
        if not fragment: return 'evidence "docs/pms-api.md:" has no line of the paging section after the colon'
        if not any(fragment in line for line in paging_lines):
            return f'evidence text is not a line of docs/pms-api.md\'s "{PAGING_HEADING[3:]}" section: {fragment!r}'
        return None
    return 'evidence must be `path:line` in the repo, or `docs/pms-api.md: <a line of the paging section>`'


def read_registry(path):
    """({section: {key: value}}, [problem]) for the ini; a malformed file is one problem."""
    parser = configparser.ConfigParser(interpolation=None, comment_prefixes=('#',),
                                       inline_comment_prefixes=None)
    try:
        with open(path, encoding='utf-8') as f:
            parser.read_file(f)
    except (OSError, configparser.Error) as error:
        return {}, [f'{path.name} does not parse: {error}']
    return {name: dict(parser[name]) for name in parser.sections()}, []


def section_problems(name, section, site, file, fn_name, functions, repo, paging_lines):
    problems = []
    unknown = set(section) - {'class', 'reason', 'evidence', 'parameter', 'via'}
    if unknown: problems.append(f'[{name}] has unknown key(s) {sorted(unknown)}')
    cls = section.get('class', '')
    if cls == 'first-page':
        problems.append(f'[{name}] class = first-page is not allowed: a window read from its start and '
                        'never continued is `bounded` (name the parameter) or `whole`, and the reason says which')
    elif cls not in CLASSES:
        problems.append(f'[{name}] class = {cls!r} is not one of {", ".join(CLASSES)}')
    if not section.get('reason', '').strip():
        problems.append(f'[{name}] has an empty reason: one line saying why this class')
    evidence = section.get('evidence', '').strip()
    if not evidence:
        problems.append(f'[{name}] has an empty evidence: a file:line in the repo, or a line of the paging section of docs/pms-api.md')
    else:
        problem = check_evidence(evidence, repo, paging_lines)
        if problem: problems.append(f'[{name}] {problem}')
    if cls == 'bounded' and not section.get('parameter', '').strip():
        problems.append(f'[{name}] is bounded but names no parameter: add `parameter = <the query parameter that bounds it>`')
    if cls != 'bounded' and 'parameter' in section:
        problems.append(f'[{name}] has parameter, which only a bounded site names')
    via = section.get('via', '').split()
    if via and cls != 'paged':
        problems.append(f'[{name}] names via, which only a paged site names')
    via_ok = bool(via)
    for other in via:
        info = functions.get((file, other))
        if info is None:
            problems.append(f'[{name}] via names {other}, which is not a function of {file}')
            via_ok = False
            continue
        if not info['paging']:
            problems.append(f'[{name}] via names {other}, which never reaches paged_path or PageReq')
            via_ok = False
        # The window is built by `other` only if the two are linked by a call, either way round.
        if other not in functions.get((file, fn_name), {}).get('calls', ()) and \
                fn_name not in info['calls']:
            problems.append(f'[{name}] via names {other}, which neither calls {fn_name} nor is called by it')
            via_ok = False
    if cls == 'paged' and not site['paging'] and not via_ok:
        problems.append(f'[{name}] is paged but neither its function nor the via functions reach paged_path or '
                        'PageReq: page it through rust-modules/plex/src/plex/paging.rs, or class it bounded or '
                        'whole and say why')
    return problems


def check(repo=REPO, ini=INI):
    """(sites, [problem]): the request sites found, and every way the registry disagrees with them."""
    plex_src = repo / 'rust-modules' / 'plex' / 'src'
    sites, functions = read_sites(repo, plex_dir=plex_src / 'plex', plex_src=plex_src)
    registry, problems = read_registry(ini)
    paging_lines = pms_paging_lines(repo / 'docs' / 'pms-api.md')
    wanted = {section_name(file, name): (file, name) for file, name in sites}
    for key in sorted(wanted):
        if key in registry: continue
        file, name = wanted[key]
        doors = ', '.join(sorted(sites[(file, name)]['doors']))
        problems.append(f'request site {key} (calls {doors}) has no section in ci/plex-requests.ini. '
                        f'Add:\n    [{key}]\n    class = single | paged | bounded | whole\n'
                        '    reason = <one line: why this class>\n'
                        '    evidence = <path:line, or docs/pms-api.md: <line of the paging section>>')
    for key in sorted(registry):
        if key not in wanted:
            problems.append(f'[{key}] names a request site that no longer exists in rust-modules/plex/src/plex: '
                            'remove the section (or re-key it to the function that now makes the request)')
            continue
        file, name = wanted[key]
        problems += section_problems(key, registry[key], sites[(file, name)], file, name, functions,
                                     repo, paging_lines)
    return sites, problems


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--sites', action='store_true', help='print every request site and its class')
    args = parser.parse_args(argv)
    sites, problems = check()
    registry, _ = read_registry(INI)
    if args.sites:
        for file, name in sorted(sites):
            key = section_name(file, name)
            print(f'{key}  [{", ".join(sorted(sites[(file, name)]["doors"]))}]  '
                  f'{registry.get(key, {}).get("class", "UNCLASSED")}'
                  f'{"  pages" if sites[(file, name)]["paging"] else ""}')
        return 0
    if problems:
        print('check-plex-requests: FAIL')
        for problem in problems: print(f'  {problem}')
        return 1
    counts = collections.Counter(registry[section_name(f, n)]['class'] for f, n in sites)
    print(f'check-plex-requests: {len(sites)} request sites, every one classed: '
          + ', '.join(f'{c} {counts.get(c, 0)}' for c in CLASSES))
    return 0


if __name__ == '__main__':
    sys.exit(main())
