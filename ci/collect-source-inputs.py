#!/usr/bin/env python3
"""Collect pinned public dependency sources from explicit archives and installed Rust sources.

The Rust toolchain is the one pinned in rust-modules/rust-toolchain.toml (override with
--rust-toolchain). The binary links that release's PREBUILT `armv7-unknown-linux-gnueabi` std, so the
`rust-runtime` input packs the `rust-src` component of the SAME release: it is the corresponding
source of the std the binary contains, not an input of the rebuild (which uses the prebuilt std and
needs no rust-src, no `-Z build-std`). The packaging job installs rust-src for exactly this.
"""
import argparse
import json
import os
import html
from pathlib import Path
import re
import subprocess
import sys
import tempfile

import deps as deps_module
from rust_toolchain import TARGET, pinned
from source_bundle import canonical, digest, fail, read_regular, scan, write_archive


def run(*args):
    """Run a tool and return its stdout; on failure, show what the tool itself said.

    The CalledProcessError alone names only the command and exit status — a `cargo vendor` that
    exits 101 then reads as nothing at all. The home directory is shortened to `~` so a local run
    does not print the developer's account path; a CI runner's paths are public anyway.
    """
    result = subprocess.run(args, capture_output=True)
    if result.returncode:
        home = str(Path.home())
        for name, stream in [('stdout', result.stdout), ('stderr', result.stderr)]:
            text = stream.decode(errors='replace').strip().replace(home, '~')
            if text:
                print('--- ' + args[0] + ' ' + name + ' ---\n' + text, file=sys.stderr)
        raise subprocess.CalledProcessError(result.returncode, args, result.stdout, result.stderr)
    return result.stdout.decode().strip()


def pack_tree(root, prefix, contents):
    for path in sorted(root.rglob('*')):
        if path.is_symlink():
            fail('source collection symlink: ' + str(path.relative_to(root)))
        if path.is_file():
            data, mode = read_regular(root, str(path.relative_to(root)))
            contents[prefix + '/' + str(path.relative_to(root))] = data, mode


def field(text, name):
    match = re.search(r'^' + re.escape(name) + r'\s*=\s*"([^"]+)"', text, re.M)
    if not match:
        fail('missing manifest field: ' + name)
    return match.group(1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument('--archive-root', type=Path, required=True,
                        help='checkout holding existing pinned vendor downloads; no downloads performed')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--rust-toolchain', default=pinned(),
                        help='default: the channel of rust-modules/rust-toolchain.toml (or $RUST_TOOLCHAIN)')
    parser.add_argument('--inventory', type=Path, help='reviewed project inventory; otherwise derive source-set runtime licenses from supplied upstream declarations')
    args = parser.parse_args()
    root, output = args.root.resolve(), args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    source_rows, deps = [], []
    # Every dependency the manifest marks `bundled` rides the source bundle with its exact
    # upstream inputs (FFmpeg and Sentry Native, the statically contained libass font stack, the
    # nanosvg headers), not URL offers. The archives are read from the cache the build filled
    # (`vendor/downloads` of --archive-root, or $PLX_DEPS_CACHE when set, the way ci/deps.py
    # resolves it); nothing is downloaded here.
    manifest = json.loads((root / 'ci/dependencies.json').read_text())
    recipes = {'ffmpeg': ['ci/build-ffmpeg.sh'], 'sentry-native': ['ci/build-sentry-native.sh'],
               'nanosvg': ['Makefile', 'src/svg.c']}
    # The private ASS library statically contains the whole libass stack: its recipe is the facade
    # and the build configuration, which is not a download of any one of the five.
    libass_recipe = ['ci/build-libass.sh', 'ci/build-libass.py', 'src/ass.c', 'src/ass_fonts.c',
                     'src/ass_text.c', 'src/ass_internal.h', 'src/ass_composite.h', 'include/ass.h']
    for dep in manifest:
        if not dep.get('bundled'):
            continue
        sources = []
        for asset in deps_module.assets_of(dep).values():
            if os.environ.get('PLX_DEPS_CACHE'):
                data, _ = read_regular(deps_module.cache_dir().resolve(),
                                       deps_module.cached_path(dep, asset, Path('.')).as_posix())
            else:
                relative = deps_module.cached_path(dep, asset, Path('vendor/downloads'))
                data, _ = read_regular(args.archive_root.resolve(), relative.as_posix())
            if digest(data) != asset['sha256']:
                fail('pinned source archive checksum mismatch: ' + asset['name'])
            (output / asset['name']).write_bytes(data)
            sources.append({'path': asset['name'], 'sha256': asset['sha256']})
        # The pinned local patches ride with the recipe, each checked against its pinned digest.
        patches = []
        for patch in dep.get('patches', []):
            patch_data, _ = read_regular(root, patch['path'])
            if digest(patch_data) != patch['sha256']:
                fail('pinned patch checksum mismatch: ' + patch['path'])
            patches.append(patch['path'])
        recipe = (libass_recipe if dep.get('group') == 'libass' else recipes[dep['id']]) + \
            ['ci/deps.py', 'ci/dependencies.json']
        deps.append({'id': dep['id'], 'version': dep['version'], 'license': dep['license'],
                     'sources': sources, 'recipes': recipe, 'patches': patches})
    with tempfile.TemporaryDirectory(prefix='collect-', dir=output) as temp:
        vendor = Path(temp) / 'cargo-vendor'
        # Only Cargo's explicitly selected locked sources are read, never a whole private cache.
        # `vendor` takes EVERY package in Cargo.lock — all targets, all features, dev-dependencies
        # included — while the release ARM build downloads only its own graph. The caller must
        # `cargo fetch --locked` (no --target) first; --offline then fails loudly on a gap
        # instead of this script downloading anything itself.
        run('cargo', '+' + args.rust_toolchain, 'vendor', '--offline', '--locked',
            '--manifest-path', str(root / 'rust-modules/Cargo.toml'), str(vendor))
        contents = {}
        pack_tree(vendor, 'cargo-vendor', contents)
        write_archive(output / 'cargo-vendor.tar.gz', contents, 0)
        components = []
        for path in sorted(vendor.glob('*/Cargo.toml')):
            text = path.read_text()
            checksum = json.loads(path.with_name('.cargo-checksum.json').read_text())
            components.append({'id': field(text, 'name'), 'version': field(text, 'version'),
                               'license': field(text, 'license'), 'registry_checksum': checksum['package']})
    (output / 'cargo-components.json').write_bytes(canonical(components) + b'\n')
    crate_license = ' AND '.join('(' + x + ')' for x in sorted({x['license'] for x in components}))
    deps.append({'id': 'cargo-vendor', 'version': 'Cargo.lock SHA256 ' + digest((root / 'rust-modules/Cargo.lock').read_bytes()),
                 'license': crate_license, 'sources': [{'path': 'cargo-vendor.tar.gz',
                     'sha256': digest((output / 'cargo-vendor.tar.gz').read_bytes())}],
                 'recipes': ['rust-modules/Cargo.toml', 'rust-modules/Cargo.lock', 'rust-modules/.cargo/config.toml'],
                 'patches': []})
    sysroot = Path(run('rustc', '+' + args.rust_toolchain, '--print', 'sysroot'))
    rust_identity = run('rustc', '+' + args.rust_toolchain, '-Vv')
    revision = re.search(r'^commit-hash: (.+)$', rust_identity, re.M).group(1)
    sources = sysroot / 'lib/rustlib/src/rust'
    if not (sources / 'library/Cargo.lock').is_file():
        fail('the rust-src component of ' + args.rust_toolchain + ' is not installed '
             '(`rustup component add rust-src --toolchain ' + args.rust_toolchain + '`); the bundle carries the '
             'sources of the prebuilt std the binary links')
    contents = {}
    pack_tree(sources, 'rust-src', contents)
    pack_tree(sysroot / 'share/doc/rust/licenses', 'rust-notices/licenses', contents)
    for name in ['COPYRIGHT.html', 'COPYRIGHT-library.html']:
        data, mode = read_regular(sysroot / 'share/doc/rust', name)
        contents['rust-notices/' + name] = data, mode
    runtime_filename = 'rust-src-' + revision[:12] + '.tar.gz'
    runtime_licenses = {}
    for path in sorted(sources.rglob('Cargo.toml')):
        match = re.search(r'^license\s*=\s*"([^"]+)"', path.read_text(), re.M)
        if match:
            runtime_licenses.setdefault(match.group(1), []).append(str(path.relative_to(sources)))
    (output / 'rust-source-license-map.json').write_bytes(canonical(runtime_licenses) + b'\n')
    contents['rust-notices/source-license-map.json'] = canonical(runtime_licenses) + b'\n', 0o644
    write_archive(output / runtime_filename, contents, 0)
    notices = (sysroot / 'share/doc/rust/COPYRIGHT-library.html').read_text()
    expressions = set(runtime_licenses) | {html.unescape(x.strip()) for x in
        re.findall(r'<p><b>License:</b> (.*?)</p>', notices)}
    # Cargo's historical slash spelling denotes alternatives; retain original expressions in evidence.
    legacy = {'MIT/Apache-2.0': 'MIT OR Apache-2.0', 'Unlicense/MIT': 'Unlicense OR MIT'}
    runtime_license = ' AND '.join('(' + legacy.get(x, x) + ')' for x in sorted(expressions))
    if args.inventory:
        inventory = json.loads(args.inventory.read_text())
        component = next((c for c in inventory['components'] if c['id'] == 'rust-std-runtime'), None)
        if component and component['spdx'] not in {'NOASSERTION', 'UNKNOWN'}:
            runtime_license = component['spdx']
    deps.append({'id': 'rust-runtime', 'version': revision, 'license': runtime_license,
                 'sources': [{'path': runtime_filename, 'sha256': digest((output / runtime_filename).read_bytes())}],
                 'recipes': ['Makefile'], 'patches': [],
                 'license_scope': 'Supplied source-set aggregate; final binary inclusion remains unproven',
                 'license_evidence': ['rust-notices/source-license-map.json', 'rust-notices/COPYRIGHT-library.html',
                                      'rust-src/library/compiler-builtins/compiler-builtins/Cargo.toml']})
    for dep in deps:
        for entry in dep['sources']:
            data = (output / entry['path']).read_bytes()
            scan(data, entry['path'])
            source_rows.append(dict(entry, bytes=len(data), pattern_scan='PASS'))
    spec = {'build_environment': {'rustc': rust_identity,
                                 'rust_target': TARGET,
                                 'rust_std': 'prebuilt rust-std of the same release (no -Z build-std); its sources are the rust-runtime input',
                                 'ndk': 'Record exact final build identity before bundle creation',
                                 'configuration': 'Record actual RELEASE/FLAVOR/features before bundle creation'},
            'dependencies': deps,
            'excluded_inputs': [
                {'component': 'glibc/GCC runtime source closure', 'status': 'BLOCKED',
                 'reason': 'Exact linked runtime objects and source/exception coverage require final linker and toolchain evidence.'},
                {'component': 'LG firmware/sysroot', 'status': 'BLOCKED',
                 'reason': 'Proprietary firmware not copied; source acquisition does not establish GPL exclusion/distribution basis.'}]}
    (output / 'source-inputs.json').write_bytes(canonical(spec) + b'\n')
    (output / 'source-collection.json').write_bytes(canonical(source_rows) + b'\n')
    print(json.dumps({'source_inputs': str(output / 'source-inputs.json'), 'archives': source_rows,
                      'completeness': 'NOT_PROVEN', 'rebuild': 'NOT_RUN'}))


if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        # run() has already printed the failing tool's own output above this line.
        print('FAIL: ' + str(error), file=sys.stderr)
        sys.exit(1)
