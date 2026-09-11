#!/usr/bin/env python3
"""Create a local, deterministic source candidate from tracked allowlisted files."""
import argparse
import json
import os
import re
from pathlib import Path
import subprocess
import sys
import tempfile

from source_bundle import (SCHEMA, allowed, canonical, digest, fail, read_regular,
                           safe_name, snapshot, validate, write_archive)


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args])


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[1])
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--dependencies', type=Path, required=True,
                   help='JSON dependency/source map and build_environment; shape is the owner\'s '
                        'private source-bundle notes, not part of this repository')
    p.add_argument('--dependency-root', type=Path, required=True)
    p.add_argument('--private-values', type=Path, help='local JSON list of forbidden literal values; never archived')
    p.add_argument('--binary', action='append', default=[], metavar='NAME=PATH')
    args = p.parse_args()
    root = args.root.resolve()
    spec = json.loads(args.dependencies.read_text())
    contents = {}
    transformations = {}
    private_values = [v.encode() for v in json.loads(args.private_values.read_text())] if args.private_values else []
    for name in git(root, 'ls-files', '--others', '--exclude-standard', '-z').decode().split('\0'):
        if name and allowed(name):
            fail('untracked source must be committed before bundling: ' + name)
    for item in git(root, 'ls-files', '-s', '-z').decode().split('\0'):
        if not item:
            continue
        metadata, name = item.split('\t', 1)
        mode = metadata.split()[0]
        if mode == '160000':
            fail('submodule needs explicit source support: ' + name)
        # Deliberate allowlist: untracked files and gitignored inputs never enter here.
        if allowed(name):
            data, filemode = read_regular(root, name)
            if name.startswith(('docs/release-audits/', 'docs/release-notes/')):
                original = data
                data = re.sub(rb'(?mi)^.*\| telemetry endpoints \|.*$',
                    b'| telemetry endpoints | Redacted in the source reconstruction copy; original release record unchanged |', data)
                for value in sorted(private_values, key=len, reverse=True):
                    if value: data = data.replace(value, b'<redacted-private-value>')
                if data != original:
                    transformations[name] = {'original_sha256': digest(original),
                        'operation': 'Redact confidential literals in copied release record; repository original unchanged'}
            contents[name] = data, filemode
    deps = []
    for dep in spec['dependencies']:
        dep = dict(dep)
        safe_name(dep['id'])
        if '/' in dep['id']:
            fail('dependency id must be one path component')
        sources = []
        for source in dep.get('sources', []):
            data, mode = read_regular(args.dependency_root.resolve(), source['path'])
            if digest(data) != source['sha256']:
                fail('dependency source checksum mismatch: ' + dep['id'])
            destination = 'dependencies/' + dep['id'] + '/' + source['path']
            if destination in contents:
                fail('duplicate dependency source path')
            contents[destination] = (data, mode)
            sources.append({'path': destination, 'sha256': digest(data)})
        dep['sources'] = sources
        deps.append(dep)
    files = {name: {'sha256': digest(data), 'mode': mode} for name, (data, mode) in contents.items()}
    manifest = {'schema': SCHEMA, 'license': 'GPL-3.0-or-later',
                'git_commit': git(root, 'rev-parse', 'HEAD').decode().strip(),
                'snapshot_sha256': snapshot(files), 'files': files, 'dependencies': deps,
                'build_environment': spec['build_environment'],
                'excluded_inputs': spec.get('excluded_inputs', []),
                'omitted_optional_files': {'docs/plex-openapi.json':
                    'Optional upstream API reference; not required for build; credential-shaped examples omitted',
                    'docs/measurements/': 'Historical raw measurements are not build inputs; originals unchanged'},
                'copied_document_transformations': transformations,
                'rebuild_status': 'NOT_RUN'}
    contents['SOURCE-MANIFEST.json'] = (canonical(manifest) + b'\n', 0o644)
    epoch = int(git(root, 'show', '-s', '--format=%ct', 'HEAD'))
    private_values = [v.encode() for v in json.loads(args.private_values.read_text())] if args.private_values else []
    args.output.parent.mkdir(parents=True, exist_ok=True)
    fd, temp = tempfile.mkstemp(dir=args.output.parent, suffix='.source.tar.gz')
    os.close(fd)
    temp = Path(temp)
    try:
        write_archive(temp, contents, epoch)
        validate(temp, manifest['snapshot_sha256'], private_values)
        os.replace(temp, args.output)
    finally:
        temp.unlink(missing_ok=True)
    binaries = {}
    for binary in args.binary:
        name, path = binary.split('=', 1)
        safe_name(name)
        binaries[name] = {'sha256': digest(Path(path).read_bytes())}
    release = {'schema': SCHEMA, 'source_archive_sha256': digest(args.output.read_bytes()),
               'source_snapshot_sha256': manifest['snapshot_sha256'], 'binaries': binaries,
               'correspondence_status': 'NOT_PROVEN', 'rebuild_status': 'NOT_RUN'}
    args.output.with_name(args.output.name + '.release.json').write_bytes(canonical(release) + b'\n')
    print(json.dumps({'archive': str(args.output), 'snapshot_sha256': manifest['snapshot_sha256'],
                      'archive_validation': 'PASS', 'rebuild': 'NOT_RUN', 'binary_correspondence': 'NOT_PROVEN'}))


if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        print('FAIL: ' + str(error), file=sys.stderr)
        sys.exit(1)
