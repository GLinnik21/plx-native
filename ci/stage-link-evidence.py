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


def find_evidence_by_content(source):
    """Locate the linker's own evidence for `source`'s bytes.

    `cargo rustc --bin <name>` invokes the linker with `-o` pointed at a
    HASHED path under `target/<triple>/release/deps/<crate>-<hash>`, so
    arm-cc.py's evidence lands there — then cargo copies (not symlinks or
    hardlinks) the binary to the plain `release/<name>` path Make actually
    names. `.resolve()` cannot find it: there is no symlink to follow. Match
    on the recorded elf_sha256 instead of guessing a path, so a stale sibling
    with the right name but the wrong build can never be picked by accident.
    """
    deps_dir = source.parent / 'deps'
    if not deps_dir.is_dir():
        return None
    crate = source.name.replace('-', '_')
    digest = hashlib.sha256(source.read_bytes()).hexdigest()
    for candidate_json in sorted(deps_dir.glob(f'{crate}-*.link.json')):
        try:
            record = json.loads(candidate_json.read_text())
        except (OSError, ValueError):
            continue
        if record.get('elf_sha256') == digest:
            return Path(str(candidate_json)[:-len('.link.json')])
    return None


if not Path(str(a.source) + '.link.json').is_file():
    resolved = a.source.resolve()
    if Path(str(resolved) + '.link.json').is_file():
        a.source = resolved
    else:
        by_content = find_evidence_by_content(a.source) if a.source.is_file() else None
        a.source = by_content if by_content is not None else resolved
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
