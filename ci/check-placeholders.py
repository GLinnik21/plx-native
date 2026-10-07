#!/usr/bin/env python3
"""The placeholder-accounting gate: every place the UI paints "this is not here yet" must be counted.

`rust-modules/ui/src/placeholder.rs` keeps a per-frame count of the placeholders a frame DRAWS (the
dump driver fails a demo frame that drew one). A counter only proves anything if no draw site can
slip past it, so the list of sites is closed by this grep rather than by memory. Every reference,
in production Rust under `rust-modules/`, to

  * `SKELETON_TOP`, `SKELETON_BOT`, `CARD_PLACEHOLDER` (the placeholder paint) and `CARD_ABSENT`
    (the same stop, the one a site wears to say "absence": a counter in the function does not vouch
    for it, only an exemption that says so does, so switching a placeholder to it cannot silence the
    gate),
  * `SKEL_*` (the skeleton bar and sheet's alphas) and `COOL_850` (the stop behind all of them),
  * `STALE_ALPHA` (the episode strip dimmed while its season reloads),
  * `Spinner::new(` / `Spinner::leading(` / a `Spinner { .. }` literal (a spinner built to be drawn;
    `Spinner::R_PAGE`, `inline_gutter` and the other layout names draw nothing and are not matched),
  * `msg::<name>_loading` / `_loading_c` (the "Loading" captions), and
  * the definition of `skeleton_bar` / `skeleton_sheet`,

must have, INSIDE THE SAME FUNCTION BODY (the innermost `fn` that contains it; at module level, the
three lines above), either a call into `placeholder::` that counts (`note`, `skeleton`, `ground`,
`flat`, `face`, `ink`) or a comment `placeholder-exempt: <reason>` that says why the reference is not
a placeholder DRAW (a value builder counted where it is drawn, an inline busy mark, ABSENCE). A bare
exemption marker with no reason fails. A counter that appears only in a comment, a string or after
code on a line (`code; // placeholder::note(..)`) is not a counter: references and counters are read
from the source with comments and string literals blanked out.

Spinners are paired: a function with N spinner constructions needs N counter calls and reasoned
exemptions between them (each call or marker covers one), so a third spinner added between two
counted ones fails. Any other reference needs one counter call or one marker, however many
references the function holds.

Beyond the references, two rules hold the hooks themselves:

  * every `Reason` variant of `placeholder.rs` must still be named, in production code outside that
    file, by some `Reason::Variant` (deleting the only hook for a reason fails), and
  * `REQUIRED` pins the sites that carry no watched token of their own (a backdrop, the hero logo's
    text, the chip's picture, the info panel's still) to the file that must name their reason.

Skipped, because they cannot be a draw: comments, `const`/`static NAME:` definitions, a file's inline
`#[cfg(test)] mod x { ... }` block (it ends at the first line that is its own indentation and `}`; a
`mod x;` declaration is not one), and test files, which are matched by name: `tests.rs`, `*_tests.rs`,
`*_test.rs`, `*_harness.rs`, or anything under a `tests/` directory. (A file merely CONTAINING
"test" in its name, `latest.rs`, is scanned.)

EXACT BLIND SPOTS, which only review catches:
  * a NEW spinner type, a type alias of `Spinner`, or a new placeholder colour token;
  * a wait caption not named `*_loading` (`*_saving`, `*_searching`, `*_connecting`, "Buffering");
  * an uncounted wait that draws nothing (`Xfade` Hold over a search's results, a section that
    arrives late with no spinner) and one drawn with `COOL_850`-free raw colours;
  * a counter call in the right function on a path the placeholder does not take (the gate reads
    text, not control flow): the unit tests beside each site own that.

    ci/check-placeholders.py            # the gate (make check runs it)
    ci/check-placeholders.py --list     # every reference and how it is satisfied
Exit 0 green, 1 on any unaccounted reference.
"""
from pathlib import Path
import argparse
import re
import sys

REPO = Path(__file__).resolve().parent.parent
ROOT_REL = 'rust-modules'
ABOVE = 3  # a reference outside any function looks this many lines up for its exemption

WATCHED = re.compile(
    r'\b(?:SKELETON_TOP|SKELETON_BOT|CARD_PLACEHOLDER|CARD_ABSENT|STALE_ALPHA|COOL_850)\b'
    r'|\bSKEL_[A-Z0-9_]+\b'
    r'|\bSpinner::(?:new|leading)\('
    r'|(?<!struct )(?<!impl )(?<!for )(?<![\w:])Spinner\s*\{'
    r'|\bmsg::\w*_loading(?:_c)?\b'
    r'|\bfn\s+skeleton_(?:bar|sheet)\b'
)
SPINNER = re.compile(r'\bSpinner::(?:new|leading)\(|(?<!struct )(?<!impl )(?<!for )(?<![\w:])Spinner\s*\{')
DEFINITION = re.compile(r'^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const|static)\s+\w+\s*:')
COUNTER = re.compile(r'\bplaceholder::(?:note|skeleton|ground|flat|face|ink)\b')
COUNTER_INSIDE = re.compile(r'(?<![\w:])(?<!fn )(?:note|skeleton|ground|flat|face|ink)\(')
ABSENT = re.compile(r'\bCARD_ABSENT\b')
EXEMPT = re.compile(r'placeholder-exempt:\s*(\S.*)')
EXEMPT_BARE = re.compile(r'placeholder-exempt:\s*$')
TEST_TAIL = re.compile(r'^\s*#\[cfg\(test\)\]\s*$')
INLINE_MOD = re.compile(r'\s*(?:pub\s+)?mod\s+\w+\s*\{')
FN = re.compile(r'\bfn\s+\w+')
OWN = 'ui/src/placeholder.rs'
TEST_FILE = re.compile(r'(?:^|_)tests?\.rs$|_harness\.rs$')

# Sites with no watched token of their own: (file under rust-modules, `Reason::` variant it must name).
REQUIRED = [
    ('screens/src/home/mod.rs', 'HomeBackdrop'),
    ('screens/src/detail/mod.rs', 'DetailBackdrop'),
    ('ui/src/hero_logo.rs', 'HeroLogoText'),
    ('ui/src/hero_logo.rs', 'HeroLogoAbsent'),
    ('ui/src/widgets.rs', 'ProfileChip'),
    ('appkit/src/info_panel.rs', 'InfoPanelStill'),
]


def is_test_file(rel):
    parts = rel.split('/')
    return bool(TEST_FILE.search(parts[-1])) or 'tests' in parts[:-1]


def lex(text):
    """(code lines, comment lines) of Rust source: `code` has every comment and string/char literal
    blanked to spaces (newlines kept, so numbering holds); `comments` has the comment text per line."""
    n = len(text)
    code = []
    com = []
    i = 0
    block = 0
    while i < n:
        c = text[i]
        two = text[i:i + 2]
        if block:
            if two == '/*':
                block += 1
                code.append('  ')
                com.append('  ')
                i += 2
            elif two == '*/':
                block -= 1
                code.append('  ')
                com.append('  ')
                i += 2
            else:
                code.append('\n' if c == '\n' else ' ')
                com.append(c)
                i += 1
        elif two == '//':
            j = text.find('\n', i)
            j = n if j < 0 else j
            code.append(' ' * (j - i))
            com.append(text[i:j])
            i = j
        elif two == '/*':
            block = 1
            code.append('  ')
            com.append('  ')
            i += 2
        elif c == 'r' and re.match(r'r#*"', text[i:i + 12]) and not (i and (text[i - 1].isalnum() or text[i - 1] == '_')):
            hashes = len(re.match(r'r(#*)"', text[i:]).group(1))
            end = text.find('"' + '#' * hashes, i + 2 + hashes)
            end = n if end < 0 else end + 1 + hashes
            code.append(''.join('\n' if ch == '\n' else ' ' for ch in text[i:end]))
            com.append(''.join('\n' if ch == '\n' else ' ' for ch in text[i:end]))
            i = end
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == '\\' else 1
            j = min(n, j + 1)
            code.append(''.join('\n' if ch == '\n' else ' ' for ch in text[i:j]))
            com.append(''.join('\n' if ch == '\n' else ' ' for ch in text[i:j]))
            i = j
        elif c == "'":
            m = re.match(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|.)|[^\\'])'", text[i:i + 14])
            if m:
                code.append(' ' * len(m.group(0)))
                com.append(' ' * len(m.group(0)))
                i += len(m.group(0))
            else:
                code.append(c)
                com.append(' ')
                i += 1
        else:
            code.append(c)
            com.append('\n' if c == '\n' else ' ')
            i += 1
    return ''.join(code).split('\n'), ''.join(com).split('\n')


def test_mod_lines(code):
    """Indices of lines inside an inline `#[cfg(test)] mod x { ... }`."""
    drop = set()
    i = 0
    while i < len(code):
        if TEST_TAIL.match(code[i]):
            j = next((k for k in range(i + 1, len(code)) if code[k].strip()), None)
            if j is not None and INLINE_MOD.match(code[j]):
                indent = code[j][:len(code[j]) - len(code[j].lstrip())]
                end = next((k for k in range(j + 1, len(code)) if code[k] == indent + '}'), len(code) - 1)
                drop.update(range(i, end + 1))
                i = end
        i += 1
    return drop


def fn_spans(code):
    """[(first line, last line)] (1-based, inclusive) of every `fn` that has a body."""
    flat = '\n'.join(code)
    starts = [0]
    for line in code:
        starts.append(starts[-1] + len(line) + 1)

    def line_of(pos):
        lo, hi = 0, len(code)
        while lo < hi:
            mid = (lo + hi) // 2
            if starts[mid + 1] <= pos:
                lo = mid + 1
            else:
                hi = mid
        return lo + 1

    spans = []
    for m in FN.finditer(flat):
        depth = 0
        k = m.end()
        while k < len(flat):
            ch = flat[k]
            if ch in '([':
                depth += 1
            elif ch in ')]':
                depth -= 1
            elif ch == ';' and depth <= 0:
                k = -1  # a declaration with no body
                break
            elif ch == '{' and depth <= 0:
                break
            k += 1
        if k < 0 or k >= len(flat):
            continue
        d = 0
        e = k
        while e < len(flat):
            if flat[e] == '{':
                d += 1
            elif flat[e] == '}':
                d -= 1
                if d == 0:
                    break
            e += 1
        spans.append((line_of(m.start()), line_of(e)))
    return spans


def region_of(number, spans):
    """The innermost function containing `number`, as (first, last); None at module level."""
    inside = [s for s in spans if s[0] <= number <= s[1]]
    return min(inside, key=lambda s: s[1] - s[0]) if inside else None


def scan(root, own_rel=OWN):
    """[(path, line, text, verdict)] for every watched reference under `root`/rust-modules.

    `verdict` is 'counted', 'exempt', 'exempt-without-reason' or 'UNACCOUNTED'."""
    out = []
    base = Path(root) / ROOT_REL
    for path in sorted(base.rglob('*.rs')):
        rel = path.relative_to(base).as_posix()
        if '/target' in f'/{rel}' or rel.startswith('target') or is_test_file(rel):
            continue
        code, comments = lex(path.read_text(errors='replace'))
        dropped = test_mod_lines(code)
        spans = fn_spans(code)
        counter = COUNTER_INSIDE if rel.endswith(own_rel) else COUNTER
        spent = {}  # region -> counter calls already given to a spinner
        for idx, line in enumerate(code):
            number = idx + 1
            if idx in dropped or DEFINITION.match(line) or not WATCHED.search(line):
                continue
            region = region_of(number, spans)
            lo, hi = region if region else (max(1, number - ABOVE), number)
            counters = [n for n in range(lo, hi + 1) if counter.search(code[n - 1]) and (n - 1) not in dropped]
            marks = [comments[n - 1] for n in range(lo, hi + 1)]
            token = WATCHED.search(line).group(0)
            reasoned = [n for n in range(lo, hi + 1) if EXEMPT.search(comments[n - 1])]
            bare = any(EXEMPT_BARE.search(m) for m in marks)
            verdict = 'UNACCOUNTED'
            if SPINNER.search(line):
                # one counter OR one reasoned exemption per spinner: the Nth spinner of a function
                # needs an Nth call or an Nth marker, so a third spinner between two counted ones fails
                c_used, e_used = spent.get(region, (0, 0))
                if c_used < len(counters):
                    verdict = 'counted'
                    spent[region] = (c_used + 1, e_used)
                elif e_used < len(reasoned):
                    verdict = 'exempt'
                    spent[region] = (c_used, e_used + 1)
                elif bare:
                    verdict = 'exempt-without-reason'
            elif ABSENT.search(line):
                # ABSENCE is declared, never counted: a counter in the same function cannot vouch for it
                verdict = 'exempt' if reasoned else ('exempt-without-reason' if bare else 'UNACCOUNTED')
            elif counters:
                verdict = 'counted'
            elif reasoned:
                verdict = 'exempt'
            elif bare:
                verdict = 'exempt-without-reason'
            out.append((rel, number, token, verdict))
    return out


def reason_names(base):
    """The `Reason` variants of placeholder.rs."""
    path = base / OWN
    if not path.exists():
        return []
    code, _ = lex(path.read_text(errors='replace'))
    text = '\n'.join(code)
    m = re.search(r'pub\s+enum\s+Reason\s*\{(.*?)\n\}', text, re.S)
    return re.findall(r'^\s*(\w+)\s*,', m.group(1), re.M) if m else []


def hook_problems(root, own_rel=OWN):
    """[message] for a `Reason` no production code names, and for a REQUIRED site whose file no longer names its reason."""
    base = Path(root) / ROOT_REL
    problems = []
    named = {}
    for path in sorted(base.rglob('*.rs')):
        rel = path.relative_to(base).as_posix()
        if '/target' in f'/{rel}' or rel.startswith('target') or is_test_file(rel) or rel.endswith(own_rel):
            continue
        code, _ = lex(path.read_text(errors='replace'))
        dropped = test_mod_lines(code)
        text = '\n'.join(l for i, l in enumerate(code) if i not in dropped)
        named[rel] = set(re.findall(r'\bReason::(\w+)', text))
    variants = reason_names(base)
    for v in variants:
        if not any(v in names for names in named.values()):
            problems.append(f'Reason::{v} is named by no production code outside {own_rel}: its hook was removed')
    if variants:
        for rel, v in REQUIRED:
            if v in variants and v not in named.get(rel, set()):
                problems.append(f'{ROOT_REL}/{rel} must name Reason::{v} (its placeholder hook is gone)')
    return problems


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    ap.add_argument('--list', action='store_true', help='print every reference and its verdict')
    ap.add_argument('--root', default=str(REPO), help=argparse.SUPPRESS)
    args = ap.parse_args(argv)
    found = scan(args.root)
    bad = [f for f in found if f[3] in ('UNACCOUNTED', 'exempt-without-reason')]
    hooks = hook_problems(args.root)
    if args.list:
        for rel, number, text, verdict in found:
            print(f'{verdict:22} {rel}:{number}: {text}')
    for rel, number, text, verdict in bad:
        why = ('has an exemption marker with no reason'
               if verdict == 'exempt-without-reason'
               else 'has no placeholder:: counter call and no `placeholder-exempt: <reason>` comment')
        print(f'{ROOT_REL}/{rel}:{number}: `{text}` {why} in its function '
              f'(rust-modules/ui/src/placeholder.rs says where the call goes)', file=sys.stderr)
    for message in hooks:
        print(f'check-placeholders: {message}', file=sys.stderr)
    if bad or hooks:
        print(f'check-placeholders: {len(bad)} unaccounted reference(s) of {len(found)}, '
              f'{len(hooks)} missing hook(s)', file=sys.stderr)
        return 1
    counted = sum(1 for f in found if f[3] == 'counted')
    print(f'check-placeholders: ok ({len(found)} references: {counted} counted, '
          f'{len(found) - counted} exempt with a reason)')
    return 0


if __name__ == '__main__':
    sys.exit(main())
