#!/usr/bin/env python3
"""Exact-file wall-clock ports; no directories, stale paths, or machine modules."""
import argparse
import configparser
import importlib.util
import re
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_test_modules as rtm
spec = importlib.util.spec_from_file_location('machine_shape', Path(__file__).with_name('check-machine-shape.py'))
shape = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shape)
ROOT = Path(__file__).resolve().parent.parent
SCOPES = ('rust-modules/data/src', 'rust-modules/session/src', 'rust-modules/ui/src',
          'rust-modules/machine/src', 'rust-modules/appkit/src', 'rust-modules/screens/src',
          'rust-modules/src/app', 'rust-modules/plex/src/plex/session.rs',
          'rust-modules/media/src/route/backend_policy.rs', 'rust-modules/media/src/route/plan.rs',
          'rust-modules/media/src/route/plan_memory.rs', 'rust-modules/media/src/player/lifecycle.rs')
WALL = re.compile(r'(?:Instant|SystemTime)::(?:now|elapsed)|\.elapsed\(')
def wall_sites(tokens):
    alias = any(tokens[i] in ('Instant', 'SystemTime') and tokens[i + 1:i + 2] == ['as'] for i in range(len(tokens) - 1))
    return alias or bool(WALL.search(''.join(tokens)))
def ports(root, ini):
    config = configparser.ConfigParser(interpolation=None, strict=True)
    config.read_string(ini.read_text(encoding='utf-8'))
    if config.defaults(): raise ValueError('ports.ini: DEFAULT exemptions are not permitted')
    registry = root / 'ci/machines.ini'
    machines = configparser.ConfigParser(interpolation=None)
    if registry.exists(): machines.read(registry, encoding='utf-8')
    listed = {'rust-modules/' + section.split('::')[0] for section in machines.sections()
              if not section.startswith('module ')}
    result = set()
    for name in config.sections():
        path = Path(name)
        if (path.is_absolute() or not name.startswith('rust-modules/') or path.suffix != '.rs'
                or any(part in ('.', '..') for part in name.split('/'))
                or any(c in name for c in '\\*?[]:') or name != path.as_posix()):
            raise ValueError(f'{name}: port must be one exact repo-relative Rust file')
        if set(config[name]) != {'kind', 'reason'} or config[name]['kind'] not in ('worker', 'adapter', 'instrument'):
            raise ValueError(f'{name}: expected kind (worker/adapter/instrument) and reason only')
        if not config[name]['reason'].strip(): raise ValueError(f'{name}: missing reason')
        target = root / path
        if not target.is_file() or target.is_symlink() or not target.resolve().is_relative_to(root.resolve()):
            raise ValueError(f'{name}: stale or escaping port path')
        tokens = shape.production_tokens(target.read_text(encoding='utf-8'))
        code = ' '.join(tokens)
        if name in listed or re.search(r'\bfn (?:step|transition)\b|\bimpl [^{;]*\bMachine\b', code):
            raise ValueError(f'{name}: machine logic cannot be exempted as a port')
        if not wall_sites(tokens): raise ValueError(f'{name}: stale port without a wall-clock operation')
        result.add(name)
    return result

# `wall_sites` needs one of these words in the source: `WALL` spells `Instant::`, `SystemTime::` or
# `.elapsed(`, and the alias test needs the token `Instant` or `SystemTime`. A file without any of
# them cannot hold a wall-clock operation, so this rule does not lex or classify it: lexing the
# ~690 files of the scopes (and the wholly-test classification of every scope) was most of this
# gate's 4.6 s of CPU, run once per `ci/check-deps.sh` that selects `wall` (every full run; the
# harness's gate self-tests run only the rule they plant for, so only its whole-script tests and
# its every-rule sweep, which runs `--only wall` too, run it).
# The skip changes what an UNLEXABLE file does here, never what a wall site does: a file with no
# wall word and an unterminated raw string no longer fails THIS rule. `ci/check-list-caps.py` (a step
# of ci/check-python-steps.txt) reports it instead: it runs `rtm.lex` over every production file under
# every `rust-modules/*/src` and `rust-modules/src` (all these scopes), and `rtm.wholly_test_files` over
# each of those roots, and lets the ValueError fail the step. Test-named files (`*_tests.rs`, `tests.rs`,
# a `tests/` directory) and wholly-test files are the ones it does not lex: test code, which rustc rejects
# when it is not Rust. `wholly_test_files(scope)` is computed only for a scope holding a wall site, so an
# unbalanced token group fails this rule only then (ci/test_check_ports.py pins both halves).
NEEDS_WALL_WORD = re.compile(r'Instant|SystemTime|elapsed')

def check(root=ROOT, ini=None):
    exempt = ports(root, ini or root / 'ci/ports.ini')
    scope_paths = [root / scope for scope in SCOPES]
    wholly_tests = {}
    def is_wholly_test(path):
        """`path` is in the union of `wholly_test_files(scope)` over the directory scopes holding it,
        each computed on its first use: a scope whose files carry no wall-clock word never is."""
        for scope in scope_paths:
            if scope.is_dir() and scope in path.parents:
                if scope not in wholly_tests: wholly_tests[scope] = rtm.wholly_test_files(scope)
                if path.resolve() in wholly_tests[scope]: return True
        return False
    problems = []
    files = set()
    for path in scope_paths:
        files.update(path.rglob('*.rs') if path.is_dir() else ([path] if path.exists() else []))
    for path in sorted(files):
        name = path.relative_to(root).as_posix()
        if name in exempt: continue
        source = path.read_text(encoding='utf-8')
        if not NEEDS_WALL_WORD.search(source): continue
        try: has_wall_site = wall_sites(shape.production_tokens(source))
        except ValueError:
            # A file with a wall word that cannot be lexed fails this gate, EXCEPT a wholly-test file,
            # which this rule never lexed (it skipped those first). Classifying before lexing every
            # such file would compute all seven scopes' wholly-test sets (about twice this gate's time);
            # classifying only the file that failed to lex costs nothing on a valid tree.
            if is_wholly_test(path): continue
            raise
        if has_wall_site and not is_wholly_test(path):
            problems.append(f'{name}: wall clock outside ci/ports.ini')
    return problems

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=ROOT)
    args = parser.parse_args()
    try: problems = check(args.root)
    except (ValueError, configparser.Error, OSError) as error: problems = [str(error)]
    if problems:
        print('\n'.join(problems), file=sys.stderr)
        return 1
    print('wall: all data/session and machine scopes; exact ports only')
    return 0
if __name__ == '__main__': sys.exit(main())
