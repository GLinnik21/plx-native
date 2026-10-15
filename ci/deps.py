#!/usr/bin/env python3
"""The one reader of ci/dependencies.json, and the one fetch-and-verify routine behind it.

Every non-Rust third-party input of this repository (the FFmpeg and Sentry Native sources, the
libass font stack, the webOS NDK, the nanosvg headers, the webosbrew toolbox packages) is declared
once in ci/dependencies.json with the version, the upstream URL (plus transport-only mirrors) and
the sha256 of the exact bytes. Nothing is copied into the repository, and no script carries a
literal version or digest of its own: shell reads a field with

    python3 ci/deps.py get ffmpeg version
    python3 ci/deps.py get webos-ndk assets.linux-aarch64.sha256

and gets the bytes with

    python3 ci/deps.py fetch ffmpeg --into vendor/downloads      # prints the verified path
    python3 ci/deps.py stage nanosvg --into vendor/nanosvg       # fetch, then copy under its file names
    python3 ci/deps.py fetch-all --exclude webos-ndk             # everything a CI cache entry must hold
    python3 ci/deps.py cache-dir                                 # where fetched archives live
    (every fetch also reads and fills the machine-wide store under $PLX_BUILD_CACHE/deps)
    python3 ci/deps.py verify sentry-native                      # its patches still match their digests
    python3 ci/deps.py check                                     # schema + patch digests, no network

The canonical upstream URL is tried first, then each mirror. Every source must yield the same
pinned bytes: a source that answers with other bytes is a hard failure that names it, never a
reason to try the next one. Downloads are written to a pid-unique file and renamed, so concurrent
builds (the ARM and host builds, several cargo build scripts) never see a partial file and never
unlink one another's.

Standard library only (macOS system Python 3.9 included).
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / 'ci/dependencies.json'
# One directory for every fetched archive, so CI caches one path keyed on the manifest.
DEFAULT_CACHE = ROOT / 'vendor/downloads'

# Per-source bound on the curl step: a dead host must hand over to the next source quickly.
# Three attempts (the first plus --retry 2) of --connect-timeout 15 plus two --retry-delay 2
# pauses is at most ~49 s to give up on a host that never answers (a refused or reset
# connection fails at once); --max-time caps one attempt at 5 minutes for a stalled
# transfer. With the one mirror FreeType carries today, a dead primary costs ~49 s before
# the mirror is tried.
CURL_BOUNDS = ['--connect-timeout', '15', '--retry', '2', '--retry-delay', '2', '--max-time', '300']

HEX = set('0123456789abcdef')


def cache_dir():
    """The directory fetched archives live in: $PLX_DEPS_CACHE, else vendor/downloads."""
    return Path(os.environ.get('PLX_DEPS_CACHE') or DEFAULT_CACHE)


def machine_cache():
    """The machine-wide store shared by every checkout and worktree: $PLX_BUILD_CACHE/deps (the
    variable ci/build-ffmpeg.sh keys its tree and tarball by, default ~/.cache/plxnative), or None
    when PLX_BUILD_CACHE is set empty. A fresh worktree finds its archives here without a network."""
    root = os.environ['PLX_BUILD_CACHE'] if 'PLX_BUILD_CACHE' in os.environ else str(Path.home() / '.cache/plxnative')
    return Path(root).resolve() / 'deps' if root else None


def shared_path(asset):
    """Where the machine-wide store keeps one asset: named by its digest, so two pins never collide."""
    store = machine_cache()
    return None if store is None else store / asset['sha256'][:16] / asset['name']


def publish(source, target):
    """Copy a verified file to `target` by rename of a pid-unique file. Best effort for a cache:
    a read-only or full volume must not fail a build that already holds the bytes."""
    target = Path(target)
    try:
        target.parent.mkdir(parents=True, exist_ok=True)
        fd, candidate = tempfile.mkstemp(prefix=target.name + '.', dir=str(target.parent))
        os.close(fd)
        try:
            shutil.copyfile(source, candidate)
            os.chmod(candidate, 0o644)
            os.replace(candidate, target)
        finally:
            Path(candidate).unlink(missing_ok=True)
    except OSError:
        pass


def digest(path):
    h = hashlib.sha256()
    with open(path, 'rb') as stream:
        for block in iter(lambda: stream.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def is_sha256(value):
    return isinstance(value, str) and len(value) == 64 and set(value) <= HEX


def load(path=MANIFEST):
    entries = json.loads(Path(path).read_text())
    if not isinstance(entries, list):
        raise ValueError('the dependency manifest must be a list')
    return entries


def entry(name, entries=None):
    for dep in (load() if entries is None else entries):
        if dep['id'] == name:
            return dep
    raise ValueError('no dependency named ' + name + ' in ci/dependencies.json')


def sources_of(asset):
    """The canonical upstream URL first, then transport-only fallbacks. Every one must yield
    the same pinned bytes; a mirror is never a different release and never recorded as origin."""
    mirrors = asset.get('mirrors', [])
    if not isinstance(mirrors, list) or not all(isinstance(url, str) and url for url in mirrors):
        raise ValueError('mirrors of ' + asset.get('id', asset.get('name', '?')) + ' must be a list of URLs')
    return [asset['url'], *mirrors]


def assets_of(dep):
    """The downloadable files of one dependency as {key: asset}.

    A single-archive entry (`archive`, `url`, `sha256` at its top level) has one asset keyed
    `archive`; a multi-file entry carries `assets`, a mapping from a short key to
    {name, url, sha256[, mirrors]}."""
    if 'assets' in dep:
        return {key: dict(asset, id=dep['id']) for key, asset in dep['assets'].items()}
    return {'archive': {'id': dep['id'], 'name': dep['archive'], 'url': dep['url'],
                        'mirrors': dep.get('mirrors', []), 'sha256': dep['sha256']}}


def cached_path(dep, asset, folder):
    """Where an asset lives under `folder`: an archive flat by its own name, the files of a
    multi-file entry under a directory named for the entry and its (shortened) version."""
    if 'assets' in dep:
        return Path(folder) / (dep['id'] + '-' + str(dep['version'])[:12]) / asset['name']
    return Path(folder) / asset['name']


def fetch_asset(asset, target, label='deps'):
    """Return `target` holding the asset's pinned bytes, downloading only if it does not already."""
    target = Path(target)
    if target.is_file() and digest(target) == asset['sha256']:
        return target
    shared = shared_path(asset)
    if shared is not None and shared.is_file() and digest(shared) == asset['sha256']:
        publish(shared, target)
        if target.is_file() and digest(target) == asset['sha256']:
            print(label + ': ' + asset['name'] + ' taken from ' + str(shared), flush=True)
            return target
    target.parent.mkdir(parents=True, exist_ok=True)
    fd, candidate = tempfile.mkstemp(prefix=target.name + '.', dir=str(target.parent))
    os.close(fd)
    failures = []
    try:
        for url in sources_of(asset):
            result = subprocess.run(['curl', '-fsSL', *CURL_BOUNDS, url, '-o', candidate])
            if result.returncode != 0:
                failures.append(url + ' (curl exit ' + str(result.returncode) + ')')
                print(label + ': ' + asset['name'] + ' not available from ' + url +
                      ' (curl exit ' + str(result.returncode) + '); trying the next source', flush=True)
                continue
            # A source that answers with the wrong bytes is never skipped past: a mirror
            # serving something else must be seen, not hidden behind a later source.
            if digest(candidate) != asset['sha256']:
                raise ValueError('source checksum mismatch: ' + asset['name'] + ' from ' + url)
            os.replace(candidate, target)
            if shared is not None:
                publish(target, shared)
            print(label + ': ' + asset['name'] + ' fetched from ' + url, flush=True)
            return target
        raise ValueError('could not download ' + asset['name'] + ' from any source: ' + '; '.join(failures))
    finally:
        Path(candidate).unlink(missing_ok=True)


def fetch(dep, folder=None, keys=None, label='deps'):
    """Fetch (or find in the cache) the selected assets of `dep`; return {key: Path}."""
    folder = Path(folder) if folder is not None else cache_dir()
    found = {}
    for key, asset in assets_of(dep).items():
        if keys is not None and key not in keys:
            continue
        found[key] = fetch_asset(asset, cached_path(dep, asset, folder), label)
    return found


def stage(dep, destination, folder=None):
    """Fetch every asset of `dep`, then place each under `destination` by its file name.

    The copy is a rename of a pid-unique file, and an already-correct file is left alone, so
    several build scripts staging at once leave one consistent directory. When every file is
    already there and matches, it returns without touching the download directory or the network."""
    destination = Path(destination)
    # Already in place and correct: nothing to fetch, so an intact destination works offline even
    # when the download directory is gone.
    wanted = [destination / asset['name'] for asset in assets_of(dep).values()]
    if all(path.is_file() and digest(path) == asset['sha256']
           for path, asset in zip(wanted, assets_of(dep).values())):
        return
    destination.mkdir(parents=True, exist_ok=True)
    for key, source in fetch(dep, folder).items():
        asset = assets_of(dep)[key]
        target = destination / asset['name']
        if target.is_file() and digest(target) == asset['sha256']:
            continue
        fd, candidate = tempfile.mkstemp(prefix=target.name + '.', dir=str(destination))
        os.close(fd)
        try:
            shutil.copyfile(source, candidate)
            os.chmod(candidate, 0o644)
            os.replace(candidate, target)
        finally:
            Path(candidate).unlink(missing_ok=True)


def get(dep, field):
    """A dotted field path (`version`, `assets.linux-aarch64.sha256`) of one entry, as text."""
    value = dep
    for part in field.split('.'):
        if isinstance(value, list) and part.isdigit() and int(part) < len(value):
            value = value[int(part)]
        elif isinstance(value, dict) and part in value:
            value = value[part]
        else:
            raise ValueError(dep['id'] + ' has no field ' + field)
    if isinstance(value, (dict, list)):
        return json.dumps(value)
    return str(value)


def patches_of(dep):
    patches = dep.get('patches', [])
    if not isinstance(patches, list):
        raise ValueError('patches of ' + dep['id'] + ' must be a list')
    return patches


def problems(entries=None, root=ROOT):
    """Every schema violation of the manifest, and every patch whose file or digest is wrong.
    Offline: it reads the manifest and the patch files, and touches nothing else."""
    entries = load(root / 'ci/dependencies.json') if entries is None else entries
    found, seen = [], set()
    for dep in entries:
        name = dep.get('id') if isinstance(dep, dict) else None
        if not isinstance(name, str) or not name:
            found.append('an entry has no id')
            continue
        if name in seen:
            found.append('duplicate id: ' + name)
        seen.add(name)
        for key in ('version', 'license'):
            if not isinstance(dep.get(key), str) or not dep[key]:
                found.append(name + ': missing ' + key)
        if ('assets' in dep) == ('archive' in dep):
            found.append(name + ': needs exactly one of `archive` (+ url, sha256) or `assets`')
            continue
        for key, asset in assets_of(dep).items():
            where = name + ('.' + key if 'assets' in dep else '')
            if not isinstance(asset.get('name'), str) or not asset['name'] or '/' in asset['name']:
                found.append(where + ': bad file name')
            if not is_sha256(asset.get('sha256')):
                found.append(where + ': sha256 must be 64 lowercase hex digits')
            try:
                urls = sources_of(asset)
            except (KeyError, ValueError) as error:
                found.append(where + ': ' + str(error))
                continue
            for url in urls:
                if not (isinstance(url, str) and url.startswith('https://')):
                    found.append(where + ': every source must be an https URL: ' + str(url))
        try:
            for patch in patches_of(dep):
                path = root / patch['path']
                if not is_sha256(patch.get('sha256')):
                    found.append(name + ': patch ' + patch.get('path', '?') + ' needs a sha256')
                elif not path.is_file():
                    found.append(name + ': patch missing: ' + patch['path'])
                elif digest(path) != patch['sha256']:
                    found.append(name + ': patch digest mismatch: ' + patch['path'])
        except (KeyError, ValueError, TypeError) as error:
            found.append(name + ': ' + str(error))
    return found


def apply_patch(source, patch):
    """Apply `patch` (-p1) to the extracted tree `source` exactly, or raise ValueError and change nothing.

    `git apply` is a strict plain-patch tool only when it does not discover a repository: `source`
    usually sits inside this work tree (gitignored), so GIT_CEILING_DIRECTORIES stops discovery at
    `source` and GIT_DIR, GIT_WORK_TREE and GIT_INDEX_FILE are removed (as ci/build-libass.py does).
    `patch(1)` is not used: it accepts fuzz, so a crash-path hunk could land in the wrong place."""
    source = Path(source).resolve()
    patch = Path(patch).resolve()
    if shutil.which('git') is None:
        raise ValueError('git is required to apply ' + patch.name + ' (see setup-environment)')
    env = {k: v for k, v in os.environ.items() if k not in ('GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE')}
    env['GIT_CEILING_DIRECTORIES'] = str(source.parent)
    for check in (['--check'], []):
        result = subprocess.run(['git', 'apply', *check, str(patch)], cwd=source, env=env,
                                capture_output=True, text=True)
        if result.returncode != 0:
            raise ValueError(patch.name + ' does not apply exactly: ' + (result.stdout + result.stderr).strip())


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest='command', required=True)
    p = sub.add_parser('get', help='print one field of one entry')
    p.add_argument('name')
    p.add_argument('field')
    p = sub.add_parser('fetch', help='download and verify; print the cached path of each asset')
    p.add_argument('name')
    p.add_argument('--asset', action='append', help='an assets key (repeatable); default: all')
    p.add_argument('--into', help='cache directory (default: $PLX_DEPS_CACHE or vendor/downloads)')
    p = sub.add_parser('stage', help='fetch every asset and place it under DIR by its file name')
    p.add_argument('name')
    p.add_argument('--into', required=True)
    p = sub.add_parser('fetch-all', help='fetch every asset of every entry, except those named by --exclude')
    p.add_argument('--exclude', action='append', default=[], help='an entry id to skip (repeatable)')
    p.add_argument('--into', help='cache directory (default: $PLX_DEPS_CACHE or vendor/downloads)')
    sub.add_parser('cache-dir', help='print the directory fetched archives live in')
    p = sub.add_parser('verify', help='check the patches of one entry against their pinned digests (offline)')
    p.add_argument('name')
    p = sub.add_parser('apply-patch', help='apply a patch to an extracted tree, strictly (no fuzz)')
    p.add_argument('source')
    p.add_argument('patch')
    sub.add_parser('check', help='validate the manifest and its patch digests (offline)')
    args = parser.parse_args(argv)
    try:
        if args.command == 'cache-dir':
            print(cache_dir())
            return 0
        if args.command == 'apply-patch':
            apply_patch(args.source, args.patch)
            return 0
        if args.command in ('check', 'verify'):
            found = problems()
            if args.command == 'verify':
                entry(args.name)
                found = [line for line in found if line.startswith(args.name + ':')]
            for line in found:
                print('deps: ' + line, file=sys.stderr)
            return 1 if found else 0
        if args.command == 'fetch-all':
            entries = load()
            unknown = set(args.exclude) - {dep['id'] for dep in entries}
            if unknown:
                raise ValueError('no dependency named ' + ', '.join(sorted(unknown)))
            sys.stdout, real = sys.stderr, sys.stdout
            try:
                for dep in entries:
                    if dep['id'] not in args.exclude:
                        fetch(dep, args.into)
            finally:
                sys.stdout = real
            return 0
        dep = entry(args.name)
        if args.command == 'get':
            print(get(dep, args.field))
        elif args.command == 'fetch':
            keys = set(args.asset) if args.asset else None
            known = set(assets_of(dep))
            if keys and not keys <= known:
                raise ValueError(args.name + ' has no asset ' + ', '.join(sorted(keys - known)))
            # Progress goes to stderr: stdout is the list of paths, for `$(...)`.
            sys.stdout.flush()
            real = sys.stdout
            sys.stdout = sys.stderr
            try:
                paths = fetch(dep, args.into, keys)
            finally:
                sys.stdout = real
            for path in paths.values():
                print(path)
        else:
            sys.stdout, real = sys.stderr, sys.stdout
            try:
                stage(dep, args.into)
            finally:
                sys.stdout = real
    except (ValueError, KeyError, OSError) as error:
        print('deps: ' + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
