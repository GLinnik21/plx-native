#!/usr/bin/env python3
"""Validate complete, byte-bound linker evidence for every specified shipped ELF."""
import argparse
import hashlib
import json
from pathlib import Path
import re

FORBIDDEN = re.compile(rb'libglibc[_-]polyfills(?:\.a)?|_getauxval_polyfill|glibc_polyfills_init', re.I)


def check_inputs(link_map, trace):
    if not link_map.strip() or not trace.strip():
        raise ValueError('missing linker map or input trace')
    for data in (link_map, trace):
        if FORBIDDEN.search(data):
            raise ValueError('forbidden glibc-polyfills input/member')
    if b'LOAD ' not in link_map:
        raise ValueError('not a GNU linker input map')


def check_elf(elf, evidence=None):
    elf = Path(elf)
    base = Path(evidence) if evidence else elf
    paths = [Path(str(base) + suffix) for suffix in ('.link.map', '.link.trace', '.link.json')]
    link_map, trace = paths[0].read_bytes(), paths[1].read_bytes()
    record = json.loads(paths[2].read_text())
    check_inputs(link_map, trace)
    if elf.read_bytes()[:4] != b'\x7fELF':
        raise ValueError(f'{elf.name}: not ELF')
    for field, data in [('elf_sha256', elf.read_bytes()), ('map_sha256', link_map), ('trace_sha256', trace)]:
        if record.get(field) != hashlib.sha256(data).hexdigest():
            raise ValueError(f'{elf.name}: {field} mismatch')
    if record.get('archive_excluded') is not True:
        raise ValueError('archive exclusion missing')


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('elf', nargs='+', type=Path)
    a = p.parse_args()
    for elf in a.elf:
        check_elf(elf)
        print(f'PASS linker inputs and SHA-256: {elf.name}')

if __name__ == '__main__':
    main()
