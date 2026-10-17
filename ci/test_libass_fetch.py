#!/usr/bin/env python3
"""The libass source fetch: canonical URL first, mirrors as a transport fallback, one pinned hash.

The build downloads FreeType from a single host that has been unreachable for hours at a time
(download.savannah.gnu.org, 2026-10-05/06), which reddened every CI run that missed the libass
cache. The pins in ci/dependencies.json may now carry `mirrors`; whichever source answers,
the bytes must match the pinned sha256, and a wrong answer is a hard failure that names its source.

No real network: sources are a local HTTP server and a closed loopback port.
"""
import contextlib
import hashlib
import importlib.util
import io
import json
import os
import shutil
import socket
import subprocess
import tarfile
import tempfile
import threading
import unittest
from unittest import mock
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('build_libass', ROOT / 'ci/build-libass.py')
build_libass = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build_libass)

GOOD = b'the pinned upstream release bytes\n' * 64
WRONG = b'a different file entirely\n' * 64


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.server.requests.append(self.path)
        body = {'/good': GOOD, '/wrong': WRONG}.get(self.path)
        if body is None:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


def dead_url():
    """A loopback port nothing listens on: the connection is refused at once."""
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    return 'http://127.0.0.1:%d/gone.tar.xz' % port


class Fetch(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        cls.server.requests = []
        cls.base = 'http://127.0.0.1:%d' % cls.server.server_address[1]
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def setUp(self):
        self.server.requests.clear()
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.folder = Path(self.temp.name)
        # The machine-wide store must not remember (or serve) a fixture across tests.
        patch = mock.patch.dict(os.environ, {'PLX_BUILD_CACHE': str(self.folder / 'machine')})
        patch.start()
        self.addCleanup(patch.stop)

    def dep(self, url, mirrors=None):
        dep = {'id': 'fixture', 'version': '1', 'archive': 'fixture-1.tar.xz',
               'sha256': hashlib.sha256(GOOD).hexdigest(), 'url': url}
        if mirrors is not None:
            dep['mirrors'] = mirrors
        return dep

    def fetch(self, dep):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            return build_libass.archive(dep, self.folder), out.getvalue()

    def test_a_dead_primary_falls_back_to_the_mirror(self):
        path, log = self.fetch(self.dep(dead_url(), [self.base + '/good']))
        self.assertEqual(path.read_bytes(), GOOD)
        self.assertIn('fixture-1.tar.xz fetched from ' + self.base + '/good', log)

    def test_a_primary_that_404s_falls_back_to_the_mirror(self):
        path, _ = self.fetch(self.dep(self.base + '/missing', [self.base + '/good']))
        self.assertEqual(path.read_bytes(), GOOD)

    def test_the_primary_serves_when_it_can_and_the_mirror_is_never_asked(self):
        path, log = self.fetch(self.dep(self.base + '/good', [self.base + '/never']))
        self.assertEqual(path.read_bytes(), GOOD)
        self.assertEqual(self.server.requests, ['/good'])
        self.assertIn('fetched from ' + self.base + '/good', log)

    def test_a_mirror_serving_wrong_bytes_is_a_hard_failure_naming_it(self):
        with self.assertRaises(ValueError) as caught:
            self.fetch(self.dep(dead_url(), [self.base + '/wrong']))
        self.assertIn('checksum mismatch', str(caught.exception))
        self.assertIn(self.base + '/wrong', str(caught.exception))
        self.assertEqual(list(self.folder.iterdir()), [])

    def test_a_primary_serving_wrong_bytes_never_falls_through_to_the_mirror(self):
        with self.assertRaises(ValueError) as caught:
            self.fetch(self.dep(self.base + '/wrong', [self.base + '/good']))
        self.assertIn('checksum mismatch', str(caught.exception))
        self.assertIn(self.base + '/wrong', str(caught.exception))
        self.assertEqual(self.server.requests, ['/wrong'])
        self.assertEqual(list(self.folder.iterdir()), [])

    def test_every_source_dead_lists_every_source(self):
        urls = [dead_url(), dead_url(), self.base + '/missing']
        with self.assertRaises(ValueError) as caught:
            self.fetch(self.dep(urls[0], urls[1:]))
        for url in urls:
            self.assertIn(url, str(caught.exception))
        self.assertEqual(list(self.folder.iterdir()), [])

    def test_a_pin_without_mirrors_still_works(self):
        path, _ = self.fetch(self.dep(self.base + '/good'))
        self.assertEqual(path.read_bytes(), GOOD)

    def test_a_cached_archive_needs_no_network(self):
        (self.folder / 'fixture-1.tar.xz').write_bytes(GOOD)
        path, log = self.fetch(self.dep(dead_url(), [dead_url()]))
        self.assertEqual(path.read_bytes(), GOOD)
        self.assertEqual(log, '')

    def test_mirrors_must_be_a_list_of_urls(self):
        with self.assertRaisesRegex(ValueError, 'mirrors of fixture'):
            self.fetch(self.dep(self.base + '/good', 'https://example.org/x'))

    def test_a_dead_host_costs_a_bounded_wait_before_the_next_source(self):
        bounds = dict(zip(build_libass.CURL_BOUNDS[::2], build_libass.CURL_BOUNDS[1::2]))
        retries = int(bounds['--retry'])
        worst = (retries + 1) * int(bounds['--connect-timeout']) + retries * int(bounds['--retry-delay'])
        self.assertLessEqual(worst, 60)


class Manifest(unittest.TestCase):
    def test_freetype_carries_a_sourceforge_mirror_behind_its_canonical_url(self):
        deps = {d['id']: d for d in json.loads((ROOT / 'ci/dependencies.json').read_text())}
        freetype = deps['freetype']
        self.assertTrue(freetype['url'].startswith('https://download.savannah.gnu.org/'))
        self.assertEqual(build_libass.sources_of(freetype)[0], freetype['url'])
        # Derived from the entry's own version: a bump must not break this test.
        v = freetype['version']
        self.assertEqual(freetype['mirrors'], [
            'https://downloads.sourceforge.net/project/freetype/freetype2/%s/freetype-%s.tar.xz' % (v, v)])
        self.assertRegex(freetype['sha256'], r'^[0-9a-f]{64}$')

    def test_every_mirror_is_https_and_names_the_pinned_archive(self):
        for dep in build_libass.stack(json.loads((ROOT / 'ci/dependencies.json').read_text())):
            for url in dep.get('mirrors', []):
                self.assertTrue(url.startswith('https://'), url)
                self.assertTrue(url.split('?')[0].endswith('/' + dep['archive']), url)


GIT_PATCH = b'''diff --git a/libass/f.c b/libass/f.c
index 0000000..1111111 100644
--- a/libass/f.c
+++ b/libass/f.c
@@ -1,2 +1,2 @@
 int a;
-int b;
+int c;
'''
BEFORE = b'int a;\nint b;\n'
AFTER = b'int a;\nint c;\n'


def sha(data):
    return hashlib.sha256(data).hexdigest()


class ApplyPatches(unittest.TestCase):
    """`apply_patches` on a tree nested inside an ignored directory of a git repository, which is
    exactly where `vendor/libass-build/source/libass-<ver>` sits in this repository's work tree."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name).resolve()
        subprocess.run(['git', 'init', '-q', str(self.repo)], check=True)
        (self.repo / '.gitignore').write_text('vendor/\n')
        self.source = self.repo / 'vendor/libass-build/source/libass-9.9'
        (self.source / 'libass').mkdir(parents=True)
        (self.source / 'libass/f.c').write_bytes(BEFORE)
        self.patch = 'ci/patches/libass-0001-fixture.patch'
        (self.repo / self.patch).parent.mkdir(parents=True)
        (self.repo / self.patch).write_bytes(GIT_PATCH)
        self.dep = {'id': 'libass', 'patches': [{
            'path': self.patch, 'sha256': sha(GIT_PATCH), 'result': {'libass/f.c': sha(AFTER)}}]}

    def apply(self, dep=None):
        with contextlib.redirect_stdout(io.StringIO()):
            build_libass.apply_patches(dep or self.dep, self.source, root=self.repo)

    def test_the_premise_git_skips_an_ignored_path_without_the_ceiling(self):
        # Why the ceiling exists: from inside the ignored tree git resolves the patch against the
        # enclosing repository, applies nothing, and still exits 0.
        env = {k: v for k, v in os.environ.items() if not k.startswith('GIT_')}
        result = subprocess.run(['git', 'apply', str(self.repo / self.patch)], cwd=self.source, env=env)
        self.assertEqual(result.returncode, 0)
        self.assertEqual((self.source / 'libass/f.c').read_bytes(), BEFORE)

    def test_the_patch_applies_and_the_result_hashes_match(self):
        self.apply()
        self.assertEqual((self.source / 'libass/f.c').read_bytes(), AFTER)

    def test_the_ceiling_is_the_source_dirs_parent(self):
        seen = []
        real = subprocess.run

        def spy(args, **kwargs):
            seen.append(kwargs.get('env', {}).get('GIT_CEILING_DIRECTORIES'))
            return real(args, **kwargs)

        with mock.patch.object(build_libass.subprocess, 'run', spy):
            self.apply()
        self.assertTrue(seen)
        self.assertEqual(set(seen), {str(self.source.parent)})

    def test_a_git_environment_from_the_caller_does_not_redirect_it(self):
        elsewhere = Path(self.temp.name) / 'elsewhere'
        subprocess.run(['git', 'init', '-q', str(elsewhere)], check=True)
        with mock.patch.dict(os.environ, {'GIT_DIR': str(elsewhere / '.git'),
                                           'GIT_WORK_TREE': str(elsewhere),
                                           'GIT_INDEX_FILE': str(elsewhere / 'index')}):
            self.apply()
        self.assertEqual((self.source / 'libass/f.c').read_bytes(), AFTER)

    def test_a_wrong_result_hash_raises(self):
        self.dep['patches'][0]['result']['libass/f.c'] = sha(b'something else')
        with self.assertRaisesRegex(ValueError, 'libass patch not applied'):
            self.apply()

    def test_a_patch_that_was_not_really_applied_is_caught_by_the_hash_alone(self):
        # Independent of git: if apply "succeeded" without changing the file, the result differs.
        ok = subprocess.CompletedProcess([], 0, '', '')
        with mock.patch.object(build_libass.subprocess, 'run', return_value=ok):
            with self.assertRaisesRegex(ValueError, 'libass patch not applied'):
                self.apply()

    def test_an_already_applied_patch_raises(self):
        self.apply()
        with self.assertRaisesRegex(ValueError, 'does not apply'):
            self.apply()

    def test_a_tampered_patch_file_raises(self):
        (self.repo / self.patch).write_bytes(GIT_PATCH + b'\n')
        with self.assertRaisesRegex(ValueError, 'digest mismatch'):
            self.apply()

    def test_a_missing_patch_file_raises(self):
        (self.repo / self.patch).unlink()
        with self.assertRaisesRegex(ValueError, 'patch missing'):
            self.apply()

    def test_missing_git_is_a_clear_error(self):
        with mock.patch.object(build_libass.shutil, 'which', return_value=None):
            with self.assertRaisesRegex(ValueError, 'git is required'):
                self.apply()

    def test_a_dependency_without_patches_needs_no_git(self):
        with mock.patch.object(build_libass.shutil, 'which', return_value=None):
            self.apply({'id': 'freetype', 'patches': []})
            self.apply({'id': 'freetype'})

    def test_a_patch_file_no_pin_names_fails_the_build(self):
        stray = self.repo / 'ci/patches/libass-0002-stray.patch'
        stray.write_bytes(GIT_PATCH)
        with self.assertRaisesRegex(ValueError, 'not named by any pin: ci/patches/libass-0002-stray.patch'):
            build_libass.check_patch_set([self.dep], root=self.repo)
        build_libass.check_patch_set([self.dep, {'id': 'x', 'patches': [
            {'path': 'ci/patches/libass-0002-stray.patch', 'sha256': sha(GIT_PATCH), 'result': {}}]}],
            root=self.repo)


class PinnedPatches(unittest.TestCase):
    def pins(self):
        return json.loads((ROOT / 'ci/dependencies.json').read_text())

    def test_the_real_pins_name_every_patch_file_with_its_digest(self):
        build_libass.check_patch_set(self.pins())

    def test_the_font_face_patch_is_pinned_on_libass_alone(self):
        by_id = {d['id']: d['patches'] for d in build_libass.stack(self.pins())}
        self.assertEqual([p['path'] for p in by_id['libass']], ['ci/patches/libass-0001-grow-font-faces.patch'])
        self.assertEqual({k for k, v in by_id.items() if v}, {'libass'})
        self.assertEqual(set(by_id['libass'][0]['result']), {'libass/ass_font.c', 'libass/ass_font.h'})

    def test_libunibreak_is_pinned_and_enabled(self):
        deps = {d['id']: d for d in self.pins()}
        self.assertEqual(deps['libunibreak']['license'], 'Zlib')
        self.assertTrue(deps['libunibreak']['url'].endswith(deps['libunibreak']['archive']))
        recipe = (ROOT / 'ci/build-libass.py').read_text()
        self.assertIn("'--enable-libunibreak'", recipe)
        self.assertNotIn('--disable-libunibreak', recipe)
        self.assertIn("'libunibreak.a'", recipe)

    def test_the_real_patch_applies_to_the_real_libass_release_and_reaches_its_result_hashes(self):
        dep = {d['id']: d for d in self.pins()}['libass']
        archive = ROOT / 'vendor/downloads' / dep['archive']
        if not archive.is_file() or sha(archive.read_bytes()) != dep['sha256']:
            self.skipTest('libass source archive not downloaded')
        with tempfile.TemporaryDirectory() as folder:
            build_libass.extract(archive, Path(folder))
            with contextlib.redirect_stdout(io.StringIO()):
                build_libass.apply_patches(dep, Path(folder) / ('libass-' + dep['version']))


if __name__ == '__main__':
    unittest.main()
