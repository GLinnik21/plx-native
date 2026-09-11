#!/usr/bin/env python3
"""Restore checked source inputs into a fresh tree and an isolated pinned Rust sysroot."""
import argparse
import json
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile

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


def merge_runtime_vendor(runtime, application):
    """Cargo source replacement also covers build-std's registry dependencies."""
    def identity(folder):
        text = (folder / 'Cargo.toml').read_text()
        fields = []
        for name in ('name', 'version'):
            match = re.search(r'^' + name + r'\s*=\s*"([^"]+)"', text, re.M)
            if not match: fail('missing vendor package identity')
            fields.append(match.group(1))
        return tuple(fields)
    existing = {identity(p): p for p in application.iterdir() if (p / 'Cargo.toml').is_file()}
    for source in runtime.iterdir():
        if not (source / 'Cargo.toml').is_file(): continue
        key = identity(source)
        if key in existing:
            a = json.loads((source / '.cargo-checksum.json').read_text()).get('package')
            b = json.loads((existing[key] / '.cargo-checksum.json').read_text()).get('package')
            if not a or a != b: fail('conflicting runtime/application vendor package: ' + key[0])
            continue
        target = application / ('rust-runtime-' + source.name)
        if target.exists(): fail('runtime vendor target already exists')
        shutil.copytree(source, target)
        existing[key] = target


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', type=Path)
    parser.add_argument('--expect-snapshot', required=True)
    parser.add_argument('--destination', required=True, type=Path, help='must not exist')
    parser.add_argument('--rust-sysroot', required=True, type=Path,
                        help='isolated pinned public toolchain with no rust-src installed; shared rustup paths refused')
    args = parser.parse_args()
    manifest = validate(args.archive, args.expect_snapshot)
    destination = args.destination.resolve()
    sysroot = args.rust_sysroot.resolve()
    if destination.exists():
        fail('source destination must not exist')
    if '.rustup' in sysroot.parts or not (sysroot / 'bin/rustc').is_file():
        fail('provide an isolated public Rust toolchain outside shared rustup')
    runtime_target = sysroot / 'lib/rustlib/src/rust'
    if runtime_target.exists() or runtime_target.is_symlink():
        fail('isolated toolchain already has rust-src; supply a fresh toolchain without it')
    deps = {dep['id']: dep for dep in manifest['dependencies']}
    identity = subprocess.check_output([str(sysroot / 'bin/rustc'), '-Vv']).decode()
    if 'commit-hash: ' + deps['rust-runtime']['version'] + '\n' not in identity:
        fail('isolated Rust compiler differs from bundled runtime revision')
    destination.mkdir(parents=True)
    extract_regular(args.archive, destination)
    for name, folder in [('ffmpeg', 'vendor/ffmpeg-build'), ('sentry-native', 'vendor')]:
        source = destination / deps[name]['sources'][0]['path']
        target = destination / folder / source.name
        if target.exists():
            fail('recipe source path already exists: ' + str(target.relative_to(destination)))
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
        runtime_target.parent.mkdir(parents=True, exist_ok=True)
        shutil.move(str(staging / 'rust-src'), str(runtime_target))
        shutil.move(str(staging / 'rust-notices'), str(destination / 'rust-runtime-notices'))
    runtime_vendor = runtime_target / 'library/vendor'
    if runtime_vendor.is_dir():
        merge_runtime_vendor(runtime_vendor, crate_root / 'cargo-vendor')
    print(json.dumps({'restore': 'PASS', 'rebuild': 'NOT_RUN',
                      'configuration_change': 'Cargo source replacement points to supplied vendor tree',
                      'next_step': 'Link this isolated toolchain with rustup, then make RUST_NIGHTLY=<link-name> RELEASE=1 using isolated build outputs'}))


if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        print('FAIL: ' + str(error), file=sys.stderr)
        sys.exit(1)
