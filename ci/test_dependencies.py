#!/usr/bin/env python3
"""The one manifest of non-Rust third-party dependencies (ci/dependencies.json) stays the only place
a version or a checksum is written.

Offline: reads the manifest, the patch files and the scripts. A script that restates a pinned sha256
(anywhere) or a pinned version (as an assignment) has a second copy that can drift from the
manifest, so it fails here.
"""
import json
import re
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

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

    def test_the_attached_ffmpeg_script_runs_without_the_manifest_tooling(self):
        sh = (ROOT / 'ci/build-ffmpeg.sh').read_text()
        for name in ('FFMPEG_VERSION', 'FFMPEG_SHA256', 'FFMPEG_TARBALL'):
            self.assertIn(name, sh)
        notices = (ROOT / 'THIRD-PARTY-NOTICES.md').read_text()
        self.assertIn('FFMPEG_TARBALL=', notices)
        self.assertIn(deps.entry('ffmpeg')['sha256'], notices)


class Offline(unittest.TestCase):
    """stage() and fetch() work without a network whenever the bytes are already somewhere."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: __import__('shutil').rmtree(self.tmp, ignore_errors=True))
        self.body = b'pinned bytes\n'
        sha = __import__('hashlib').sha256(self.body).hexdigest()
        # An unreachable source: any attempt to download fails this test.
        self.dep = {'id': 'fake', 'version': '1', 'license': 'x', 'assets': {
            'one': {'name': 'one.h', 'url': 'https://127.0.0.1:9/one.h', 'sha256': sha}}}
        self.env = mock.patch.dict(os.environ, {'PLX_BUILD_CACHE': str(self.tmp / 'machine'),
                                                'PLX_DEPS_CACHE': str(self.tmp / 'downloads')})
        self.env.start()
        self.addCleanup(self.env.stop)
        self.asset = self.dep['assets']['one']

    def test_an_intact_destination_needs_neither_downloads_nor_network(self):
        dest = self.tmp / 'vendor/nanosvg'
        dest.mkdir(parents=True)
        (dest / 'one.h').write_bytes(self.body)
        deps.stage(self.dep, dest)  # would raise on the unreachable source if it fetched
        self.assertFalse((self.tmp / 'downloads').exists())

    def test_a_wrong_destination_file_is_not_trusted(self):
        dest = self.tmp / 'vendor/nanosvg'
        dest.mkdir(parents=True)
        (dest / 'one.h').write_bytes(b'tampered')
        with self.assertRaises(ValueError):
            deps.stage(self.dep, dest)

    def test_the_machine_wide_cache_serves_a_fresh_checkout(self):
        shared = deps.shared_path(self.asset)
        shared.parent.mkdir(parents=True)
        shared.write_bytes(self.body)
        dest = self.tmp / 'vendor/nanosvg'
        deps.stage(self.dep, dest)
        self.assertEqual((dest / 'one.h').read_bytes(), self.body)
        self.assertEqual((self.tmp / 'downloads/fake-1/one.h').read_bytes(), self.body)

    def test_a_corrupt_machine_cache_entry_is_not_used(self):
        shared = deps.shared_path(self.asset)
        shared.parent.mkdir(parents=True)
        shared.write_bytes(b'corrupt')
        with self.assertRaises(ValueError):
            deps.fetch(self.dep)

    def test_a_download_fills_the_machine_wide_cache(self):
        source = self.tmp / 'src.h'
        source.write_bytes(self.body)
        self.asset['url'] = source.as_uri()
        deps.fetch(self.dep)
        self.assertEqual(deps.shared_path(self.asset).read_bytes(), self.body)

    def test_an_empty_PLX_BUILD_CACHE_disables_the_machine_cache(self):
        with mock.patch.dict(os.environ, {'PLX_BUILD_CACHE': ''}):
            self.assertIsNone(deps.machine_cache())


class StrictPatchTest(unittest.TestCase):
    """The sentry-native patch is a crash-path change: a hunk that lands by fuzz must fail the build."""

    ORIGINAL = ''.join('line %d\n' % n for n in range(1, 21))

    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.tmp = Path(self.scratch.name)
        self.source = self.tmp / 'src'
        self.source.mkdir()
        (self.source / 'f.c').write_text(self.ORIGINAL)

    def patch(self, context):
        path = self.tmp / 'x.patch'
        path.write_text('--- a/f.c\n+++ b/f.c\n@@ -9,5 +9,6 @@\n %s\n line 10\n line 11\n+added\n line 12\n line 13\n'
                        % context)
        return path

    def test_a_patch_whose_context_matches_applies(self):
        deps.apply_patch(self.source, self.patch('line 9'))
        self.assertIn('added\n', (self.source / 'f.c').read_text())

    def test_a_patch_that_only_applies_with_fuzz_is_rejected_and_leaves_the_file_alone(self):
        # patch(1) takes this (one wrong context line is fuzz 1); git apply does not.
        with self.assertRaises(ValueError):
            deps.apply_patch(self.source, self.patch('line nine'))
        self.assertEqual((self.source / 'f.c').read_text(), self.ORIGINAL)

    def test_the_sentry_build_applies_its_patch_through_that_strict_routine(self):
        script = (ROOT / 'ci/build-sentry-native.sh').read_text()
        self.assertNotRegex(script, r'(?m)^\s*patch\s')
        self.assertIn('apply-patch', script)


if __name__ == '__main__':
    unittest.main()
