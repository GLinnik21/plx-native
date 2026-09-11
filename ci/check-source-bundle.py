#!/usr/bin/env python3
"""Inspect the finished archive without extracting it or trusting a producer PASS."""
import argparse
import json
from pathlib import Path
import sys
from source_bundle import digest, fail, validate


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', type=Path)
    parser.add_argument('--expect-snapshot', required=True)
    parser.add_argument('--private-values', type=Path)
    parser.add_argument('--release-manifest', type=Path)
    parser.add_argument('--binary', action='append', default=[], metavar='NAME=PATH')
    args = parser.parse_args()
    values = [v.encode() for v in json.loads(args.private_values.read_text())] if args.private_values else []
    manifest = validate(args.archive, args.expect_snapshot, values)
    if args.release_manifest:
        release = json.loads(args.release_manifest.read_text())
        if release['source_archive_sha256'] != digest(args.archive.read_bytes()):
            fail('release manifest source archive hash mismatch')
        if release['source_snapshot_sha256'] != manifest['snapshot_sha256']:
            fail('release manifest source snapshot mismatch')
        supplied = dict(binary.split('=', 1) for binary in args.binary)
        if set(supplied) != set(release['binaries']):
            fail('every release-manifest binary must be supplied for validation')
        for name, path in supplied.items():
            if release['binaries'][name]['sha256'] != digest(Path(path).read_bytes()):
                fail('release manifest binary hash mismatch: ' + name)
    elif args.binary:
        fail('--binary requires --release-manifest')
    print(json.dumps({'archive_validation': 'PASS', 'snapshot_sha256': manifest['snapshot_sha256'],
                      'rebuild': 'NOT_RUN', 'binary_correspondence': 'NOT_PROVEN'}))


if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, OSError) as error:
        print('FAIL: ' + str(error), file=sys.stderr)
        sys.exit(1)
