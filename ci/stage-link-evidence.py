#!/usr/bin/env python3
"""Carry validated link evidence across a copy or verified strip transformation."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile

spec = importlib.util.spec_from_file_location('evidence', Path(__file__).with_name('check-link-evidence.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('source', type=Path)
p.add_argument('destination', type=Path)
p.add_argument('--stripped', action='store_true')
p.add_argument('--evidence-base', type=Path)
a = p.parse_args()
if not Path(str(a.source) + '.link.json').is_file():
    a.source = a.source.resolve()
m.check_elf(a.source)
source = a.source.read_bytes()
target = a.destination.read_bytes()
if source != target:
    if not a.stripped:
        raise SystemExit('stage-link-evidence: copy differs')
    sdk = Path(os.environ.get('WEBOS_SDK', str(Path.home() / 'webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot')))
    with tempfile.TemporaryDirectory() as d:
        trial = Path(d) / 'stripped'
        trial.write_bytes(source)
        subprocess.run([str(sdk / 'bin/arm-webos-linux-gnueabi-strip'), '--strip-unneeded', str(trial)], check=True)
        if trial.read_bytes() != target:
            raise SystemExit('stage-link-evidence: target is not the asserted strip transformation')
record = json.loads(Path(str(a.source) + '.link.json').read_text())
record['linked_elf_sha256'] = record['elf_sha256']
record['elf_sha256'] = hashlib.sha256(target).hexdigest()
record['transformation'] = 'strip-unneeded' if source != target else 'copy'
base = a.evidence_base or a.destination
base.parent.mkdir(parents=True, exist_ok=True)
for suffix in ('.link.map', '.link.trace'):
    Path(str(base) + suffix).write_bytes(Path(str(a.source) + suffix).read_bytes())
Path(str(base) + '.link.json').write_text(json.dumps(record, indent=2) + '\n')
m.check_elf(a.destination, base)
