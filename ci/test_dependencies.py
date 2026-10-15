#!/usr/bin/env python3
"""The one manifest of non-Rust third-party dependencies (ci/dependencies.json) stays the only place
a version or a checksum is written.

Offline: reads the manifest, the patch files and the scripts. A script that restates a pinned sha256
(anywhere) or a pinned version (as an assignment) has a second copy that can drift from the
manifest, so it fails here.
"""
import json
import re
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / 'ci'))
import deps  # noqa: E402

# Everything that fetches, verifies, keys on or checks a pinned dependency.
SCRIPTS = [
    'ci/build-ffmpeg.sh', 'ci/build-sentry-native.sh', 'ci/build-libass.py', 'ci/source_bundle.py',
    'ci/collect-source-inputs.py', 'ci/restore-source-inputs.py', 'ci/check-package.py', 'Makefile',
    '.github/actions/webos-ndk/action.yml', '.github/actions/host-rust/action.yml',
    'tools/fwcompat.py', 'rust-modules/build_support/host_link.rs',
    '.github/workflows/ci.yml', '.github/workflows/build-package.yml',
    '.github/workflows/simulators.yml', '.github/workflows/site-video.yml',
]
SHA = re.compile(r'\b[0-9a-f]{64}\b')


def manifest():
    return json.loads((ROOT / 'ci/dependencies.json').read_text())


def digests(entry):
    return [a['sha256'] for a in deps.assets_of(entry).values()]


class Manifest(unittest.TestCase):
    def test_schema_is_valid(self):
        self.assertEqual(deps.problems(), [])

    def test_every_patch_matches_its_pinned_hash(self):
        patches = [p for e in manifest() for p in e.get('patches', [])]
        self.assertTrue(patches)
        for patch in patches:
            path = ROOT / patch['path']
            self.assertTrue(path.is_file(), patch['path'])
            self.assertEqual(deps.digest(path), patch['sha256'], patch['path'])

    def test_no_unpinned_patch_file(self):
        pinned = {p['path'] for e in manifest() for p in e.get('patches', [])}
        present = {p.relative_to(ROOT).as_posix() for p in (ROOT / 'ci/patches').glob('*.patch')}
        self.assertEqual(present, pinned)

    def test_the_old_per_recipe_manifest_is_gone(self):
        self.assertFalse((ROOT / 'ci/libass-dependencies.json').exists())


class NoSecondCopy(unittest.TestCase):
    def test_no_script_restates_a_pinned_sha256(self):
        pinned = {d for e in manifest() for d in digests(e)}
        pinned |= {p['sha256'] for e in manifest() for p in e.get('patches', [])}
        for name in SCRIPTS:
            for found in SHA.findall((ROOT / name).read_text()):
                self.assertNotIn(found, pinned, name + ' restates a manifest checksum')

    def test_no_script_assigns_a_pinned_version(self):
        for entry in manifest():
            version = entry['version']
            if len(version) < 5:
                continue  # "9.0" is too short to grep for without false hits
            assign = re.compile(r'(?:^|\s)[A-Za-z_]*(?:VERSION|RELEASE|TAG|REL)\w*\s*[:=]+\s*["\']?'
                                + re.escape(version) + r'\b', re.M)
            for name in SCRIPTS:
                self.assertIsNone(assign.search((ROOT / name).read_text()),
                                  name + ' assigns the pinned version of ' + entry['id'])

    def test_no_script_carries_a_download_url_of_a_pinned_dependency(self):
        for entry in manifest():
            for asset in deps.assets_of(entry).values():
                for name in SCRIPTS:
                    self.assertNotIn(asset['url'], (ROOT / name).read_text(), name)

    def test_ffmpeg_version_and_sha_are_read_not_written(self):
        sh = (ROOT / 'ci/build-ffmpeg.sh').read_text()
        self.assertIn('get ffmpeg version', sh)
        self.assertIn('get ffmpeg sha256', sh)


if __name__ == '__main__':
    unittest.main()
