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
    cargo-internal path, then copies (not symlinks or hardlinks) the binary
    to the plain `release/<name>` path Make actually names — so arm-cc.py's
    evidence lands beside the INTERNAL path, not beside the copy.
    `.resolve()` cannot find it: there is no symlink to follow. That internal
    path is usually `target/<triple>/release/deps/<crate>-<hash>`, but it is
    not a stable contract: with `-Z build-std` (nightly, unstable) it has
    also been observed at `target/<triple>/release/build/<pkg>/<hash>/out/
    <crate>` — a directory shaped like a build-script OUT_DIR rather than
    `deps/`, seen on a `rustup toolchain install nightly` picked up fresh by
    CI while a dev machine's pinned nightly still used the old layout. So
    search the whole `<triple>` output tree rather than guessing one
    location, and match on the recorded elf_sha256 instead of a path, so a
    stale sibling with the right name but the wrong build can never be
    picked by accident.
    """
    # Walk up from `.../release/<name>` (or `.../release/deps/<name>`) to the
    # `<triple>` directory — the root cargo confines every internal output
    # for this build under, on every layout observed so far.
    root = source.parent
    while root.name not in ('release', 'debug') and root.parent != root:
        root = root.parent
    if root.name in ('release', 'debug'):
        root = root.parent
    else:
        root = source.parent  # fallback: search only beside the source
    crate = source.name.replace('-', '_')
    digest = hashlib.sha256(source.read_bytes()).hexdigest()
    for candidate_json in sorted(root.glob(f'**/{crate}-*.link.json')) + \
            sorted(root.glob(f'**/{crate}.link.json')):
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
