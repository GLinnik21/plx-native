#!/usr/bin/env python3
"""The language-list gate: code never lists UI languages, `i18n::SHIPPED` does.

A UI language is `locales/<tag>/` plus its `locales/languages.json` entry (docs/localization.md,
"Adding a language"). Every "each language" loop therefore reads the generated `SHIPPED` list. A
hand-written list that names two or more registered languages, `[Preference::En, Preference::Es]`,
bare or inside a tuple or `resolve(..)`, ships a new language untested: #771's Italian was one such
list away from being dead, and #783 had to find about twenty of them by grep.

This reads the Rust source with comments and strings blanked out, tracks bracket depth (rustfmt
splits these lists over lines, so a line grep cannot see them) and flags any single bracketed list
whose own elements name two or more distinct `Preference::<Lang>` / `Language::<Lang>` variants.
`<Lang>` comes from the registry, so the rule needs no edit when a language is added.

The files in ci/allow/language-lists.txt are the exemptions: pinned, reviewed translations (a golden
table iterates its own rows and never falls back with `_ =>`) and the like, one `path<TAB>reason`
per line, compared to the tree as an exact set with a `# count:` header. It is a strong convention,
not a proof: a test can still build a list from `vec![]` pushes.

    ci/check-language-lists.py           # the gate (make check runs it)
    ci/check-language-lists.py --list    # every file with such a list, and the lists
Exit 0 green, 1 on any new, stale or miscounted entry.
"""
from pathlib import Path
import argparse
import importlib.util
import json
import re
import sys

REPO = Path(__file__).resolve().parent.parent
ALLOW_REL = 'ci/allow/language-lists.txt'

_spec = importlib.util.spec_from_file_location('check_placeholders', Path(__file__).with_name('check-placeholders.py'))
_lexer = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_lexer)


def variants(root):
    registry = json.loads((Path(root) / 'locales' / 'languages.json').read_text())
    return sorted(e['tag'].capitalize() for e in registry)


def lists(code, langs):
    """[(line, sorted variants)] for every bracketed list whose own elements name 2+ languages."""
    pattern = re.compile(r'\b(?:Preference|Language)::(' + '|'.join(langs) + r')\b')
    stack, found = [], []
    for number, line in enumerate(code, 1):
        for token in re.finditer(r'\[|\]|' + pattern.pattern, line):
            text = token.group(0)
            if text == '[':
                stack.append((number, set()))
            elif text == ']':
                if stack:
                    opened, names = stack.pop()
                    if len(names) >= 2:
                        found.append((opened, sorted(names)))
            elif stack:
                stack[-1][1].add(text.split('::')[1])
    return found


def references(root):
    out = {}
    langs = variants(root)
    for path in sorted((Path(root) / 'rust-modules').rglob('*.rs')) + sorted((Path(root) / 'src').rglob('*.rs')):
        rel = path.relative_to(Path(root)).as_posix()
        if '/target' in f'/{rel}':
            continue
        code, _ = _lexer.lex(path.read_text(errors='replace'))
        found = lists(code, langs)
        if found:
            out[rel] = found
    return out


def allowed(root):
    entries, declared = [], None
    for line in (Path(root) / ALLOW_REL).read_text().split('\n'):
        m = re.match(r'#\s*count:\s*(\d+)', line)
        if m and declared is None:
            declared = int(m.group(1))
        if not line.strip() or line.lstrip().startswith('#'):
            continue
        entries.append(line.split('\t')[0].strip())
    return entries, declared


def problems(root):
    found = references(root)
    entries, declared = allowed(root)
    out = []
    for rel in sorted(set(found) - set(entries)):
        where = ', '.join(f'line {n}' for n, _ in found[rel])
        out.append(f'{rel}: a hand-written list names several languages ({where}). Loop over '
                   f'`plx_platform::i18n::SHIPPED` instead; only a pinned, reviewed translation table belongs in {ALLOW_REL}')
    for rel in sorted(set(entries) - set(found)):
        out.append(f'{ALLOW_REL}: stale entry {rel} (the file no longer lists languages): delete the line')
    for rel in sorted({e for e in entries if entries.count(e) > 1}):
        out.append(f'{ALLOW_REL}: {rel} is listed twice')
    if declared != len(entries):
        out.append(f'{ALLOW_REL}: `# count:` says {declared} but there are {len(entries)} entries')
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    ap.add_argument('--list', action='store_true', help='print every file with such a list')
    ap.add_argument('--root', default=str(REPO), help=argparse.SUPPRESS)
    args = ap.parse_args(argv)
    if args.list:
        for rel, found in references(args.root).items():
            for number, names in found:
                print(f'{rel}:{number}\t{", ".join(names)}')
        return 0
    bad = problems(args.root)
    if bad:
        for line in bad:
            print(f'check-language-lists: FAIL {line}')
        return 1
    entries, _ = allowed(args.root)
    print(f'check-language-lists: green: {len(entries)} files hold a pinned language table, all listed')
    return 0


if __name__ == '__main__':
    sys.exit(main())
