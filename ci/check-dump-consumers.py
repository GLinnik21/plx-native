#!/usr/bin/env python3
"""The frame-dump switch gate: only the files listed in ci/allow/dump.txt may name it.

`plx_gfx::dump` (`rust-modules/gfx/src/dump.rs`) is a process-global switch that lets the simulator
film the app deterministically for the website video. A branch on it is product code that behaves
differently under the dump than on the television, so it is bounded: the module doc states the
ADMISSION RULE (a branch may replace a timing input with a constant or make a per-iteration stepper
idempotent on a held repeat; it may never skip a state or change what a settled frame shows) and
lists every consumer with the kind it is. This gate holds the list of FILES to that doc:

  * a file under `rust-modules/` that names `dump::armed`, `dump::held_repeat`,
    `dump::set_held_repeat`, `dump::arm`, `dump::disarm`, `dump::Armed` or `dump::HeldRepeat` (the
    two test guards included: a test that arms the switch is a race against its neighbours), or
    imports from the module with a brace list, a glob or an alias, and is not in the list, fails; and
  * a listed file that no longer names it fails as stale (the list is exact, and goes down as well
    as up), and so does a `# count:` line that disagrees with the entries.

References are read from the source with comments and string literals blanked out, so prose that
mentions the switch (as this module's neighbours' docs do) is not a use. Test code in a listed file
is covered by the file's entry.

Adding a line is a design decision, not a way to make a new file pass: read the admission rule in
`dump.rs`, make the branch fit it, add the consumer to the list in that `//!`, then add the file here
with the reason.

    ci/check-dump-consumers.py           # the gate (make check runs it)
    ci/check-dump-consumers.py --list    # every file that names the switch, and the names
Exit 0 green, 1 on any new, stale or miscounted entry.
"""
from pathlib import Path
import argparse
import importlib.util
import re
import sys

REPO = Path(__file__).resolve().parent.parent
ROOT_REL = 'rust-modules'
ALLOW_REL = 'ci/allow/dump.txt'
RULE = 'rust-modules/gfx/src/dump.rs (`//!`, "The admission rule" and "Every consumer")'

_spec = importlib.util.spec_from_file_location('check_placeholders', Path(__file__).with_name('check-placeholders.py'))
_lexer = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_lexer)

NAMES = re.compile(
    r'\bdump::(?:armed|held_repeat|set_held_repeat|arm|disarm|Armed|HeldRepeat)\b'
    r'|\bdump::(?:\{|\*)'
    r'|\bdump\s+as\b'
)


def references(root):
    """{repo-relative path: sorted set of the names it uses} under `root`/rust-modules."""
    out = {}
    base = Path(root) / ROOT_REL
    for path in sorted(base.rglob('*.rs')):
        rel = path.relative_to(Path(root)).as_posix()
        if '/target/' in f'/{rel}':
            continue
        code, _ = _lexer.lex(path.read_text(errors='replace'))
        found = sorted({m.group(0) for line in code for m in NAMES.finditer(line)})
        if found:
            out[rel] = found
    return out


def allowed(root):
    """(entries, declared count) of the allow-list; entries are `path` per line, reason after a tab."""
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
        out.append(f'{rel}: names the frame-dump switch ({", ".join(found[rel])}) and is not in {ALLOW_REL}. '
                   f'A dump branch may replace a timing input with a constant or make a per-iteration stepper '
                   f'idempotent on a held repeat; it may never skip a state or change what a settled frame '
                   f'shows. Read {RULE} before adding anything.')
    for rel in sorted(set(entries) - set(found)):
        out.append(f'{ALLOW_REL}: stale entry {rel} (the file no longer names the switch): delete the line, '
                   f'and its consumer from the list in dump.rs')
    dupes = sorted({e for e in entries if entries.count(e) > 1})
    for rel in dupes:
        out.append(f'{ALLOW_REL}: {rel} is listed twice')
    if declared != len(entries):
        out.append(f'{ALLOW_REL}: `# count:` says {declared} but there are {len(entries)} entries')
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    ap.add_argument('--list', action='store_true', help='print every file that names the switch')
    ap.add_argument('--root', default=str(REPO), help=argparse.SUPPRESS)
    args = ap.parse_args(argv)
    if args.list:
        for rel, names in references(args.root).items():
            print(f'{rel}\t{", ".join(names)}')
        return 0
    bad = problems(args.root)
    if bad:
        for line in bad:
            print(f'check-dump-consumers: FAIL {line}')
        return 1
    entries, _ = allowed(args.root)
    print(f'check-dump-consumers: green: {len(entries)} files name the frame-dump switch, all listed')
    return 0


if __name__ == '__main__':
    sys.exit(main())
