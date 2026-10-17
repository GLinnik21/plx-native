#!/usr/bin/env python3
"""Restore checked source inputs into a fresh tree and check the isolated pinned Rust sysroot.

The bundled rust-src is restored NEXT TO the tree (`rust-runtime-src`) as the corresponding source of the
prebuilt std; it is never installed into the toolchain, because the build does not use it.
"""
import argparse
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile

import deps as deps_module
from rust_toolchain import TARGET
from source_bundle import fail, safe_name, validate


def extract_regular(archive_path, destination):
    """Never follow links or extract special files, even after archive validation."""
    with tarfile.open(archive_path, 'r:*') as archive:
        for entry in archive:
            safe_name(entry.name)
            if not entry.isfile():
                fail('restoration accepts regular-file archives only: ' + entry.name)
            path = destination / entry.name
            if path.exists() or path.is_symlink():
                fail('restoration would overwrite an existing input: ' + entry.name)
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open('xb') as out:
                shutil.copyfileobj(archive.extractfile(entry), out)
            path.chmod(entry.mode & 0o777)


def require_prebuilt_std(sysroot, target=TARGET):
    """The rebuild links the toolchain's PREBUILT std for `target`; refuse a sysroot that has none.

    The release this project builds with (rust-modules/rust-toolchain.toml) needs neither `rust-src`
    nor `-Z build-std`: its ARMv7 std ships as a rust-std component. A sysroot without it would send
    cargo looking for std sources, which this restore deliberately does not provide.
    """
    if not list((sysroot / 'lib/rustlib' / target / 'lib').glob('libstd-*.rlib')):
        fail('isolated toolchain has no prebuilt std for ' + target + ' (rustup target add ' + target + ')')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', type=Path)
    parser.add_argument('--expect-snapshot', required=True)
    parser.add_argument('--destination', required=True, type=Path, help='must not exist')
    parser.add_argument('--rust-sysroot', required=True, type=Path,
                        help='isolated pinned public toolchain: prebuilt std for the ARM target, NO rust-src; shared rustup paths refused')
    args = parser.parse_args()
    manifest = validate(args.archive, args.expect_snapshot)
    destination = args.destination.resolve()
    sysroot = args.rust_sysroot.resolve()
    if destination.exists():
        fail('source destination must not exist')
    if '.rustup' in sysroot.parts or not (sysroot / 'bin/rustc').is_file():
        fail('provide an isolated public Rust toolchain outside shared rustup')
    if (sysroot / 'lib/rustlib/src').exists() or (sysroot / 'lib/rustlib/src').is_symlink():
        fail('isolated toolchain has rust-src; supply a sysroot without it (the rebuild must not need it)')
    require_prebuilt_std(sysroot)
    deps = {dep['id']: dep for dep in manifest['dependencies']}
    identity = subprocess.check_output([str(sysroot / 'bin/rustc'), '-Vv']).decode()
    if 'commit-hash: ' + deps['rust-runtime']['version'] + '\n' not in identity:
        fail('isolated Rust compiler differs from bundled runtime revision')
    destination.mkdir(parents=True)
    extract_regular(args.archive, destination)
    # The bundled upstream inputs go where the build looks for them: the download cache
    # (`vendor/downloads` of the destination, or $PLX_DEPS_CACHE when set, the way ci/deps.py
    # resolves it), under the names the manifest gives them. A build then finds each one
    # present and verified, and fetches nothing.
    pins = destination / 'ci/dependencies.json'
    if pins.is_file():
        for dep in json.loads(pins.read_text()):
            if not dep.get('bundled'):
                continue
            by_name = {Path(entry['path']).name: entry for entry in deps[dep['id']]['sources']}
            for asset in deps_module.assets_of(dep).values():
                source = destination / by_name[asset['name']]['path']
                shared = bool(os.environ.get('PLX_DEPS_CACHE'))
                if shared:
                    target = deps_module.cached_path(dep, asset, deps_module.cache_dir().resolve())
                else:
                    target = destination / deps_module.cached_path(dep, asset, Path('vendor/downloads'))
                if shared and target.is_file() and deps_module.digest(target) == asset['sha256']:
                    continue  # a shared cache may already hold the verified bytes
                if target.exists():
                    fail('recipe source path already exists: ' + str(target))
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source, target)
    crate_root = destination / 'rust-modules'
    extract_regular(destination / deps['cargo-vendor']['sources'][0]['path'], crate_root)
    config = crate_root / '.cargo/config.toml'
    current = config.read_text()
    if '[source.' in current:
        fail('existing Cargo source replacement needs explicit reconciliation')
    with config.open('a') as stream:
        stream.write('\n[source.crates-io]\nreplace-with = "bundled-sources"\n\n'
                     '[source.bundled-sources]\ndirectory = ' + json.dumps(str(crate_root / 'cargo-vendor')) + '\n')
    with tempfile.TemporaryDirectory(prefix='rust-source-', dir=destination) as temp:
        staging = Path(temp)
        extract_regular(destination / deps['rust-runtime']['sources'][0]['path'], staging)
        shutil.move(str(staging / 'rust-src'), str(destination / 'rust-runtime-src'))
        shutil.move(str(staging / 'rust-notices'), str(destination / 'rust-runtime-notices'))
    print(json.dumps({'restore': 'PASS', 'rebuild': 'NOT_RUN',
                      'configuration_change': 'Cargo source replacement points to supplied vendor tree',
                      'next_step': 'Link this isolated toolchain with rustup, then make RUST_TOOLCHAIN=<link-name> RELEASE=1 using isolated build outputs'}))


if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        print('FAIL: ' + str(error), file=sys.stderr)
        sys.exit(1)
