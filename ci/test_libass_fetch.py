#!/usr/bin/env python3
"""The libass source fetch: canonical URL first, mirrors as a transport fallback, one pinned hash.

The build downloads FreeType from a single host that has been unreachable for hours at a time
(download.savannah.gnu.org, 2026-10-05/06), which reddened every CI run that missed the libass
cache. The pins in ci/libass-dependencies.json may now carry `mirrors`; whichever source answers,
the bytes must match the pinned sha256, and a wrong answer is a hard failure that names its source.

No real network: sources are a local HTTP server and a closed loopback port.
"""
import contextlib
import hashlib
import importlib.util
import io
import json
import socket
import tempfile
import threading
import unittest
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
        deps = {d['id']: d for d in json.loads((ROOT / 'ci/libass-dependencies.json').read_text())}
        freetype = deps['freetype']
        self.assertTrue(freetype['url'].startswith('https://download.savannah.gnu.org/'))
        self.assertEqual(build_libass.sources_of(freetype)[0], freetype['url'])
        self.assertEqual(freetype['mirrors'], [
            'https://downloads.sourceforge.net/project/freetype/freetype2/2.14.3/freetype-2.14.3.tar.xz'])
        self.assertEqual(freetype['sha256'], '36bc4f1cc413335368ee656c42afca65c5a3987e8768cc28cf11ba775e785a5f')

    def test_every_mirror_is_https_and_names_the_pinned_archive(self):
        for dep in json.loads((ROOT / 'ci/libass-dependencies.json').read_text()):
            for url in dep.get('mirrors', []):
                self.assertTrue(url.startswith('https://'), url)
                self.assertTrue(url.split('?')[0].endswith('/' + dep['archive']), url)


if __name__ == '__main__':
    unittest.main()
