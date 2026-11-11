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

def check(root=ROOT, ini=None):
    exempt = ports(root, ini or root / 'ci/ports.ini')
    tests = set()
    for scope in SCOPES:
        path = root / scope
        if path.is_dir(): tests.update(rtm.wholly_test_files(path))
    problems = []
    files = set()
    for scope in SCOPES:
        path = root / scope
        files.update(path.rglob('*.rs') if path.is_dir() else ([path] if path.exists() else []))
    for path in sorted(files):
        name = path.relative_to(root).as_posix()
        if name in exempt or path.resolve() in tests: continue
        if wall_sites(shape.production_tokens(path.read_text(encoding='utf-8'))):
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
