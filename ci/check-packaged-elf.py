#!/usr/bin/env python3
"""Verify EVERY ELF in the actual IPK against byte-bound link receipts."""
import argparse
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile

spec = importlib.util.spec_from_file_location('evidence', Path(__file__).with_name('check-link-evidence.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

def check(ipk, evidence):
    raw = ipk.read_bytes()
    if not raw.startswith(b'!<arch>\n'):
        raise ValueError('not ar')
    pos, data = 8, None
    while pos < len(raw):
        header = raw[pos:pos + 60]
        if len(header) != 60 or header[58:] != b'`\n':
            raise ValueError('malformed ar')
        size = int(header[48:58]); name = header[:16].decode().strip().rstrip('/')
        blob = raw[pos + 60:pos + 60 + size]
        if len(blob) != size:
            raise ValueError('truncated ar')
        if name == 'data.tar.gz':
            if data is not None: raise ValueError('duplicate data archive')
            data = blob
        pos += 60 + size + size % 2
    if data is None: raise ValueError('missing data archive')
    found = set()
    with tempfile.TemporaryDirectory() as d, tarfile.open(fileobj=io.BytesIO(data)) as t:
        for member in t:
            if not member.isfile(): continue
            blob = t.extractfile(member).read()
            if blob[:4] != b'\x7fELF': continue
            name = Path(member.name).name
            if name in found: raise ValueError('duplicate ELF basename')
            found.add(name)
            elf = Path(d) / name; elf.write_bytes(blob)
            m.check_elf(elf, evidence / name)
    expected = {p.name[:-len('.link.json')] for p in evidence.glob('*.link.json')}
    if not found or found != expected:
        raise ValueError(f'ELF closure mismatch: actual {sorted(found)}, receipts {sorted(expected)}')
    print(f'PASS actual IPK: {len(found)} ELF files, all link inputs and transformed SHA-256 verified')

if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('ipk', type=Path); p.add_argument('evidence', type=Path)
    a = p.parse_args(); check(a.ipk, a.evidence)
