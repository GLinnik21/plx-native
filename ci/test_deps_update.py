#!/usr/bin/env python3
"""ci/deps-update.py and .github/workflows/deps-update.yml: the manifest's update sources are valid,
tag parsing and version ordering work per source type (fixtures, no network), a bump rewrites the
right strings, the dependency-graph snapshot has the submission API's shape, and the workflow keeps
its security shape (minimal permissions, SHA-pinned actions, untrusted strings never in a script).
"""
import contextlib
import hashlib
import importlib.util
import json
import io
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import types
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / 'ci'))
spec = importlib.util.spec_from_file_location('deps_update', ROOT / 'ci/deps-update.py')
du = importlib.util.module_from_spec(spec)
spec.loader.exec_module(du)

WORKFLOW = ROOT / '.github/workflows/deps-update.yml'


# Everything that asserts a version, digest or URL reads this frozen copy, never the live manifest:
# the weekly updater edits ci/dependencies.json (and THIRD-PARTY-NOTICES.md) on every bump PR, so a
# test pinned to the live pin would fail on the updater's own output. Only the generic checks
# (live_manifest) read the live files, and they assert no specific version.
FIXTURES = ROOT / 'ci/testdata'
FIXTURE_MANIFEST = FIXTURES / 'dependencies.json'
FIXTURE_NOTICES = FIXTURES / 'THIRD-PARTY-NOTICES.md'


def manifest():
    return json.loads(FIXTURE_MANIFEST.read_text())


def live_manifest():
    return json.loads((ROOT / 'ci/dependencies.json').read_text())


def dep(name):
    return next(d for d in manifest() if d['id'] == name)


RELEASES = [
    {'tag_name': 'v1.0.18', 'draft': False, 'prerelease': False, 'published_at': '2026-02-01T00:00:00Z'},
    {'tag_name': 'v1.0.19-rc1', 'draft': False, 'prerelease': True, 'published_at': '2026-03-01T00:00:00Z'},
    {'tag_name': 'v1.0.20', 'draft': True, 'prerelease': False, 'published_at': '2026-04-01T00:00:00Z'},
    {'tag_name': 'v1.0.9', 'draft': False, 'prerelease': False, 'published_at': '2026-01-01T00:00:00Z'},
    {'tag_name': 'nightly', 'draft': False, 'prerelease': False, 'published_at': '2026-05-01T00:00:00Z'},
]


def fake_github(payloads):
    def get(url, accept=None):
        for needle, body in payloads.items():
            if needle in url:
                return json.dumps(body) if not isinstance(body, str) else body
        raise AssertionError('unexpected request ' + url)
    return get


class Manifest(unittest.TestCase):
    def test_every_entry_declares_a_valid_update_source(self):
        self.assertEqual(du.update_problems(live_manifest()), [])

    def test_the_committed_file_is_what_the_script_would_write(self):
        text = (ROOT / 'ci/dependencies.json').read_text()
        self.assertEqual(du.dump(json.loads(text)), text)

    def test_held_entries_carry_their_reason(self):
        held = {d['id']: d['update']['hold'] for d in manifest() if 'hold' in d['update']}
        self.assertEqual(sorted(held), ['webos-ndk', 'webosbrew-toolbox'])
        self.assertTrue(all(len(reason) > 20 for reason in held.values()))

    def test_a_missing_or_bad_update_is_reported(self):
        entry = dep('fribidi')
        for bad in ({}, {'type': 'nope'}, {'type': 'manual'},
                    dict(entry['update'], repo='not a repo'),
                    dict(entry['update'], tag_pattern=r'^v\d+$'),
                    dict(entry['update'], tag_format='{version}')):
            broken = dict(entry, update=bad)
            self.assertTrue(du.update_problems([broken]), bad)
        self.assertTrue(du.update_problems([{k: v for k, v in entry.items() if k != 'update'}]))


class Versions(unittest.TestCase):
    def test_numbers_compare_as_numbers(self):
        self.assertGreater(du.version_key('1.10'), du.version_key('1.9'))
        self.assertGreater(du.version_key('9.0.1'), du.version_key('9.0'))
        self.assertGreater(du.version_key('14.5.0'), du.version_key('9.9.9'))

    def test_tag_patterns(self):
        self.assertEqual(du.version_of('v1.0.17', r'^v(\d+\.\d+\.\d+)$'), '1.0.17')
        self.assertIsNone(du.version_of('1.0.17', r'^v(\d+\.\d+\.\d+)$'))
        self.assertIsNone(du.version_of('v1.0.17-rc1', r'^v(\d+\.\d+\.\d+)$'))
        self.assertEqual(du.version_of('libunibreak_8_1', dep('libunibreak')['update']['tag_pattern']), '8.1')
        self.assertEqual(du.version_of('libunibreak_8_1_2', dep('libunibreak')['update']['tag_pattern']), '8.1.2')

    def test_tag_format_round_trips_the_pinned_version(self):
        for name in ('fribidi', 'libunibreak', 'harfbuzz'):
            d = dep(name)
            self.assertEqual(du.version_of(du.current_tag(d), d['update']['tag_pattern']), d['version'])
        self.assertEqual(du.current_tag(dep('libunibreak')), 'libunibreak_8_0')


class Sources(unittest.TestCase):
    def test_github_release_skips_drafts_prereleases_and_other_tags(self):
        d = dep('fribidi')
        with mock.patch.object(du, 'http_get', fake_github({'/releases': RELEASES})):
            release = du.latest(d)
        self.assertEqual(release['version'], '1.0.18')  # 1.0.18 > 1.0.9 numerically
        self.assertEqual(release['tag'], 'v1.0.18')
        self.assertEqual(release['url'], 'https://github.com/fribidi/fribidi/releases/tag/v1.0.18')
        self.assertTrue(du.is_outdated(d, release))

    def test_github_release_ordered_by_publication(self):
        d = dep('webos-ndk')
        releases = [{'tag_name': 'webos-aaaaaaa.2', 'published_at': '2026-01-01T00:00:00Z'},
                    {'tag_name': 'webos-bbbbbbb.1', 'published_at': '2026-06-01T00:00:00Z'}]
        with mock.patch.object(du, 'http_get', fake_github({'/releases': releases})):
            release = du.latest(d)
        self.assertEqual(release['version'], 'webos-bbbbbbb.1')
        self.assertTrue(du.is_outdated(d, release))
        self.assertFalse(du.is_outdated(d, {'version': d['version']}))

    def test_github_tags(self):
        d = dict(dep('fribidi'), update={'type': 'github-tags', 'repo': 'fribidi/fribidi',
                                         'tag_pattern': r'^v(\d+\.\d+\.\d+)$'})
        with mock.patch.object(du, 'http_get', fake_github({'/tags': [{'name': 'v1.0.2'}, {'name': 'v1.0.10'}, {'name': 'junk'}]})):
            self.assertEqual(du.latest(d)['version'], '1.0.10')

    def test_commit_pins_compare_by_sha(self):
        d = dep('nanosvg')
        new = 'a' * 40
        with mock.patch.object(du, 'http_get', fake_github({'/commits/HEAD': {'sha': new}})):
            release = du.latest(d)
        self.assertEqual(release['version'], new)
        self.assertEqual(release['url'], 'https://github.com/memononen/nanosvg/commit/' + new)
        self.assertTrue(du.is_outdated(d, release))
        self.assertFalse(du.is_outdated(d, {'version': d['version']}))

    def test_listing(self):
        page = ('<a href="ffmpeg-8.1.tar.xz">x</a><a href="ffmpeg-9.0.2.tar.xz">x</a>'
                '<a href="ffmpeg-9.0.2.tar.xz.asc">x</a><a href="ffmpeg-snapshot.tar.bz2">x</a><a href="ffmpeg-9.0.tar.xz">x</a>')
        with mock.patch.object(du, 'http_get', lambda url, accept=None: page):
            release = du.latest(dep('ffmpeg'))
        self.assertEqual(release['version'], '9.0.2')
        self.assertTrue(du.is_outdated(dep('ffmpeg'), release))

    def test_no_matching_release_is_an_error_not_a_guess(self):
        with mock.patch.object(du, 'http_get', fake_github({'/releases': [{'tag_name': 'x', 'draft': False}]})):
            with self.assertRaises(du.BumpError):
                du.latest(dep('fribidi'))

    def test_a_failed_lookup_is_reported_per_entry(self):
        def boom(url, accept=None):
            raise du.BumpError('offline')
        with mock.patch.object(du, 'http_get', boom):
            row = du.status(dep('fribidi'))
        self.assertEqual((row['error'], row['outdated']), ('offline', False))

    def test_a_manual_entry_makes_no_request(self):
        with mock.patch.object(du, 'http_get', side_effect=AssertionError('network')):
            row = du.status(dep('webosbrew-toolbox'))
        self.assertIsNone(row['latest'])


class Rewrite(unittest.TestCase):
    def test_tag_then_version_in_names_and_urls(self):
        new = du.rewritten(dep('libunibreak'), {'version': '8.1', 'tag': 'libunibreak_8_1', 'url': ''})
        self.assertEqual(new['version'], '8.1')
        self.assertEqual(new['archive'], 'libunibreak-8.1.tar.gz')
        self.assertEqual(new['url'], 'https://github.com/adah1972/libunibreak/releases/download/libunibreak_8_1/libunibreak-8.1.tar.gz')

    def test_v_prefixed_tag(self):
        new = du.rewritten(dep('fribidi'), {'version': '1.0.18', 'tag': 'v1.0.18', 'url': ''})
        self.assertEqual(new['url'], 'https://github.com/fribidi/fribidi/releases/download/v1.0.18/fribidi-1.0.18.tar.xz')

    def test_mirrors_follow(self):
        new = du.rewritten(dep('freetype'), {'version': '2.15.0', 'tag': 'freetype-2.15.0.tar.xz', 'url': ''})
        self.assertIn('/2.15.0/freetype-2.15.0.tar.xz', new['mirrors'][0])

    def test_commit_pin_rewrites_every_asset_url(self):
        sha = 'b' * 40
        new = du.rewritten(dep('nanosvg'), {'version': sha, 'tag': sha, 'url': ''})
        self.assertTrue(all(sha in a['url'] for a in new['assets'].values()))
        self.assertEqual([a['name'] for a in new['assets'].values()], ['nanosvg.h', 'nanosvgrast.h'])

    def test_bump_writes_the_digest_and_never_touches_patches(self):
        scratch = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, scratch)
        path = scratch / 'm.json'
        shutil.copy(FIXTURE_MANIFEST, path)
        notices = scratch / 'NOTICES.md'
        shutil.copy(FIXTURE_NOTICES, notices)
        before = json.loads(path.read_text())
        release = {'version': '1.0.18', 'tag': 'v1.0.18', 'url': 'https://example.invalid/r'}
        with mock.patch.object(du, 'latest', return_value=release), \
                mock.patch.object(du, 'download_digest', return_value='c' * 64):
            old, new, notes = du.bump('fribidi', path=path, body_file=scratch / 'body.md', notices=notices)
        after = json.loads(path.read_text())
        changed = [a['id'] for a, b in zip(before, after) if a != b]
        self.assertEqual(changed, ['fribidi'])
        self.assertEqual(next(d for d in after if d['id'] == 'fribidi')['sha256'], 'c' * 64)
        body = (scratch / 'body.md').read_text()
        for needle in ('https://example.invalid/r', 'fw-compat-reviewer', 'THIRD-PARTY-NOTICES.md', '1.0.17'):
            self.assertIn(needle, body)

    def test_a_patch_that_no_longer_applies_fails_loudly_after_writing(self):
        scratch = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, scratch)
        path = scratch / 'm.json'
        shutil.copy(FIXTURE_MANIFEST, path)
        notices = scratch / 'NOTICES.md'
        shutil.copy(FIXTURE_NOTICES, notices)
        release = {'version': '9.9.9', 'tag': '9.9.9', 'url': 'https://example.invalid/r'}
        with mock.patch.object(du, 'latest', return_value=release), \
                mock.patch.object(du, 'download_digest', return_value='d' * 64), \
                mock.patch.object(du, 'check_patches', side_effect=du.PatchError('x.patch no longer applies')):
            with self.assertRaises(du.PatchError) as raised:
                du.bump('sentry-native', path=path, body_file=scratch / 'body.md', notices=notices)
        self.assertEqual(raised.exception.version, '9.9.9')
        self.assertEqual(next(d for d in json.loads(path.read_text()) if d['id'] == 'sentry-native')['version'], '9.9.9')
        self.assertIn('no longer applies', (scratch / 'body.md').read_text())
        self.assertIn('**Sentry Native 9.9.9**', notices.read_text())  # the notices still follow the manifest

    def test_held_and_current_entries_are_not_bumped(self):
        with self.assertRaises(du.BumpError):
            du.bump('webos-ndk')
        with mock.patch.object(du, 'latest', return_value={'version': dep('fribidi')['version'], 'tag': 'v', 'url': ''}):
            with self.assertRaises(du.BumpError):
                du.bump('fribidi')

    def test_an_unexpected_version_string_never_reaches_a_branch_name(self):
        release = {'version': '1.0.18; rm -rf', 'tag': 'v1.0.18', 'url': ''}
        with mock.patch.object(du, 'latest', return_value=release):
            with self.assertRaises(du.BumpError):
                du.bump('fribidi', path=FIXTURE_MANIFEST)


class FilteringTarfile:
    """The `tarfile` module as Python 3.12 has it, for hosts that predate `data_filter`: the filter
    argument is accepted (the hardening itself is Python's, not under test) and dropped."""
    data_filter = staticmethod(lambda member, path: member)

    @staticmethod
    def open(*args, **kwargs):
        tar = tarfile.open(*args, **kwargs)
        original = tar.extractall
        tar.extractall = lambda path, members=None, filter=None: original(path, members=members)
        return tar


def tarball(path, files, top='x-1'):
    with tarfile.open(path, 'w:gz') as tar:
        for name, text in files.items():
            data = text.encode()
            info = tarfile.TarInfo(top + '/' + name)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))


PATCH = """--- a/f.c
+++ b/f.c
@@ -1,7 +1,7 @@
 one
 two
 three
-four
+FOUR
 five
 six
 seven
"""
SOURCE = 'one\ntwo\nthree\nfour\nfive\nsix\nseven\n'


class Notices(unittest.TestCase):
    def moved(self, name, version):
        old = dep(name)
        new = dict(old, version=version, sha256='e' * 64, archive=old.get('archive', '').replace(old['version'], version))
        return old, new

    def test_every_rule_names_a_manifest_entry_and_matches_the_committed_notices(self):
        # Live files, no specific version: the committed notices must follow the committed manifest.
        text = (ROOT / 'THIRD-PARTY-NOTICES.md').read_text()
        live = {d['id']: d for d in live_manifest()}
        self.assertEqual(sorted(set(du.NOTICE_ROWS) - set(live)), [])
        for name, rows in du.NOTICE_ROWS.items():
            for row in rows:
                self.assertIn(du.notice_text(row, live[name]), text, name)

    def test_each_entry_with_rows_is_rewritten_and_the_old_strings_are_gone(self):
        text = FIXTURE_NOTICES.read_text()
        for name in du.NOTICE_ROWS:
            old, new = self.moved(name, '99.1.2')
            out = du.update_notices(old, new, text)
            for row in du.NOTICE_ROWS[name]:
                self.assertNotIn(du.notice_text(row, old), out, name)
                self.assertIn(du.notice_text(row, new), out, name)
        old, new = self.moved('ffmpeg', '99.1.2')
        out = du.update_notices(old, new, text)
        self.assertNotIn(old['sha256'], out)
        self.assertIn('FFMPEG_VERSION=99.1.2 FFMPEG_SHA256=' + 'e' * 64, out)

    def test_a_row_that_is_not_there_fails_and_rewrites_nothing(self):
        old, new = self.moved('libass', '0.18.0')
        with self.assertRaises(du.BumpError) as raised:
            du.update_notices(old, new, '| libass | 0.17.4 |\n')
        self.assertIn('libass', str(raised.exception))

    def test_an_entry_without_rows_leaves_the_notices_alone(self):
        old, new = self.moved('nanosvg', 'f' * 40)
        self.assertEqual(du.update_notices(old, new, 'x'), 'x')

    def test_ffmpeg_bump_keeps_the_notices_in_step_with_the_manifest(self):
        scratch = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, scratch)
        path, notices = scratch / 'm.json', scratch / 'NOTICES.md'
        shutil.copy(FIXTURE_MANIFEST, path)
        shutil.copy(FIXTURE_NOTICES, notices)
        release = {'version': '9.1', 'tag': '9.1', 'url': 'https://example.invalid/r'}
        with mock.patch.object(du, 'latest', return_value=release), \
                mock.patch.object(du, 'download_digest', return_value='a' * 64):
            du.bump('ffmpeg', path=path, notices=notices)
        text = notices.read_text()
        self.assertIn('ffmpeg-9.1.tar.xz', text)
        self.assertIn('a' * 64, text)
        self.assertNotIn(dep('ffmpeg')['sha256'], text)

    def test_a_missing_notices_row_makes_the_bump_a_draft_that_says_so(self):
        scratch = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, scratch)
        path, notices = scratch / 'm.json', scratch / 'NOTICES.md'
        shutil.copy(FIXTURE_MANIFEST, path)
        notices.write_text('nothing here\n')
        release = {'version': '1.0.18', 'tag': 'v1.0.18', 'url': 'https://example.invalid/r'}
        with mock.patch.object(du, 'latest', return_value=release), \
                mock.patch.object(du, 'download_digest', return_value='a' * 64):
            with self.assertRaises(du.PatchError) as raised:
                du.bump('fribidi', path=path, body_file=scratch / 'body.md', notices=notices)
        self.assertEqual(raised.exception.version, '1.0.18')
        self.assertIn('THIRD-PARTY-NOTICES.md', (scratch / 'body.md').read_text())
        self.assertEqual(notices.read_text(), 'nothing here\n')
        self.assertEqual(next(d for d in json.loads(path.read_text()) if d['id'] == 'fribidi')['version'], '1.0.18')


class PatchCheck(unittest.TestCase):
    def setUp(self):
        self.scratch = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.scratch)
        (self.scratch / 'ci/patches').mkdir(parents=True)
        (self.scratch / 'ci/patches/p.patch').write_text(PATCH)
        self.archive = self.scratch / 'upstream.tar.gz'
        self.dep = {'id': 'x', 'version': '1', 'archive': 'x-1.tar.gz', 'url': 'https://example.invalid/x',
                    'patches': [{'path': 'ci/patches/p.patch', 'sha256': '0', 'result': {'f.c': '0' * 64}}]}

    def run_check(self, source, **kw):
        tarball(self.archive, {'f.c': source})
        with mock.patch.object(du, 'tarfile', FilteringTarfile), \
                mock.patch.object(du, 'download_to', lambda url, path: shutil.copy(self.archive, path)):
            return du.check_patches(self.dep, root=self.scratch, **kw)

    def test_result_digests_are_recomputed_in_place(self):
        notes = self.run_check(SOURCE)
        patched = SOURCE.replace('four', 'FOUR').encode()
        self.assertEqual(self.dep['patches'][0]['result']['f.c'], hashlib.sha256(patched).hexdigest())
        self.assertEqual(len(notes), 1)

    def test_a_patch_that_only_applies_with_fuzz_is_rejected_like_the_build_does(self):
        # patch(1) -F2 would take this (the first context line differs); git apply, like the build, does not.
        with self.assertRaises(du.PatchError) as raised:
            self.run_check(SOURCE.replace('one', 'uno'))
        self.assertIn('ci/patches/p.patch no longer applies', str(raised.exception))
        self.assertEqual(self.dep['patches'][0]['result']['f.c'], '0' * 64)

    def test_no_data_filter_refuses_before_downloading(self):
        legacy = types.SimpleNamespace(open=tarfile.open)
        with mock.patch.object(du, 'tarfile', legacy), \
                mock.patch.object(du, 'download_to', side_effect=AssertionError('downloaded')):
            with self.assertRaises(du.BumpError) as raised:
                du.check_patches(self.dep, root=self.scratch)
        self.assertIn('data_filter', str(raised.exception))

    def test_an_archive_over_the_size_cap_is_refused(self):
        with mock.patch.object(du, 'MAX_EXTRACT_BYTES', 10):
            with self.assertRaises(du.BumpError) as raised:
                self.run_check(SOURCE)
        self.assertIn('refusing', str(raised.exception))


class CommandLine(unittest.TestCase):
    def run_main(self, argv, **patches):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = du.main(argv)
        return code, out.getvalue(), err.getvalue()

    def rows(self, version, tag):
        return [{'id': 'fribidi', 'outdated': True, 'hold': None, 'error': None,
                 'release': {'version': version, 'tag': tag, 'url': ''}}]

    def test_list_outdated_applies_the_safe_check(self):
        with mock.patch.object(du, 'status', side_effect=lambda d: self.rows('1.0.18; rm -rf', 'v1')[0]
                               if d['id'] == 'fribidi' else dict(self.rows('1', 'v')[0], outdated=False)):
            code, out, err = self.run_main(['--list-outdated'])
        self.assertEqual((code, out), (1, ''))
        self.assertIn('refusing an unexpected version', err)
        with mock.patch.object(du, 'status', side_effect=lambda d: self.rows('1.0.18', 'v1.0.18')[0]
                               if d['id'] == 'fribidi' else dict(self.rows('1', 'v')[0], outdated=False)):
            code, out, err = self.run_main(['--list-outdated'])
        self.assertEqual((code, out), (0, 'fribidi\t1.0.18\n'))

    def test_bump_prints_the_version_it_used_first(self):
        old, new = dep('fribidi'), dict(dep('fribidi'), version='1.0.19')
        with mock.patch.object(du, 'bump', return_value=(old, new, [])):
            code, out, err = self.run_main(['--bump', 'fribidi'])
        self.assertEqual((code, out.splitlines()[0]), (0, '1.0.19'))
        failing = du.PatchError('x')
        failing.version = '1.0.19'
        with mock.patch.object(du, 'bump', side_effect=failing):
            code, out, err = self.run_main(['--bump', 'fribidi'])
        self.assertEqual((code, out.splitlines()[0]), (3, '1.0.19'))


class Snapshot(unittest.TestCase):
    def setUp(self):
        self.snap = du.snapshot(manifest(), 'f' * 40, 'refs/heads/main', '42')

    def test_top_level_shape_of_the_submission_api(self):
        self.assertEqual(self.snap['version'], 0)
        self.assertEqual(self.snap['sha'], 'f' * 40)
        self.assertEqual(self.snap['ref'], 'refs/heads/main')
        self.assertEqual(self.snap['job'], {'correlator': 'plxnative-deps-manifest', 'id': '42'})
        self.assertTrue(all(self.snap['detector'].get(k) for k in ('name', 'version', 'url')))
        self.assertRegex(self.snap['scanned'], r'^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$')

    def test_one_resolved_package_per_entry(self):
        (file_manifest,) = self.snap['manifests'].values()
        self.assertEqual(file_manifest['file'], {'source_location': 'ci/dependencies.json'})
        self.assertEqual(sorted(file_manifest['resolved']), sorted(d['id'] for d in manifest()))
        for package in file_manifest['resolved'].values():
            self.assertRegex(package['package_url'], r'^pkg:(github|generic)/')
            self.assertIn(package['relationship'], ('direct', 'indirect'))
            self.assertIn(package['scope'], ('runtime', 'development'))

    def test_purls(self):
        resolved = self.snap['manifests']['ci/dependencies.json']['resolved']
        self.assertEqual(resolved['fribidi']['package_url'], 'pkg:github/fribidi/fribidi@v1.0.17')
        self.assertEqual(resolved['nanosvg']['package_url'], 'pkg:github/memononen/nanosvg@' + dep('nanosvg')['version'])
        self.assertEqual(resolved['ffmpeg']['package_url'],
                         'pkg:generic/ffmpeg@9.0?download_url=https://ffmpeg.org/releases/ffmpeg-9.0.tar.xz')
        self.assertEqual(resolved['ffmpeg']['scope'], 'runtime')
        self.assertEqual(resolved['webos-ndk']['scope'], 'development')

    def test_it_is_plain_json(self):
        self.assertEqual(json.loads(json.dumps(self.snap)), self.snap)


class WorkflowShape(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.text = WORKFLOW.read_text()
        cls.code = '\n'.join(l for l in cls.text.splitlines() if not l.lstrip().startswith('#'))

    def test_triggers(self):
        self.assertRegex(self.code, r'(?m)^  schedule:\n    - cron: ')
        self.assertIn('workflow_dispatch:', self.code)
        self.assertRegex(self.code, r'(?m)^  push:\n    branches: \[main\]\n    paths:\n      - ci/dependencies\.json')
        self.assertNotIn('pull_request', self.code)  # no privileged trigger on pull request code

    def test_default_permissions_are_none_and_each_job_declares_its_own(self):
        self.assertRegex(self.code, r'(?m)^permissions: \{\}$')
        jobs = re.findall(r'(?m)^  ([a-z-]+):\n(?:    .*\n)*?    permissions:\n((?:      .*\n)+)', self.code)
        grants = {name: dict(re.findall(r'(\w[\w-]*): (\w+)', body)) for name, body in jobs}
        self.assertEqual(grants['propose'], {'contents': 'write', 'pull-requests': 'write', 'actions': 'write'})
        self.assertEqual(grants['graph'], {'contents': 'write'})

    def test_every_action_is_pinned_by_sha(self):
        uses = re.findall(r'uses: (\S+)(.*)', self.code)
        self.assertTrue(uses)
        for ref, rest in uses:
            self.assertRegex(ref, r'@[0-9a-f]{40}$')
            self.assertRegex(rest, r'# v\d+')

    def test_checkouts_keep_no_credential(self):
        self.assertEqual(self.code.count('persist-credentials: false'), self.code.count('actions/checkout@'))

    def test_untrusted_strings_reach_the_shell_through_env_only(self):
        self.assertNotRegex(self.code, r'\$\{\{\s*(github\.event\.(pull_request|head_commit|issue)|github\.head_ref)')
        for script in re.findall(r'run: \|\n((?:\s{10}.*\n)+)', self.code):
            self.assertNotIn('${{', script)

    def test_ci_is_dispatched_on_each_branch_and_said_to_be_why(self):
        self.assertIn('gh workflow run ci.yml --ref', self.code)
        self.assertIn('GITHUB_TOKEN', self.text)
        self.assertRegex(self.text, r'(?i)GITHUB_TOKEN[^\n]*(do not|does not|start no|trigger no)')

    def test_the_pull_request_body_is_generated_by_the_script(self):
        self.assertIn('--body-file', self.code)
        self.assertIn('--list-outdated', self.code)

    def test_one_branch_per_dependency_and_version(self):
        self.assertIn('deps/${name}-${version}', self.code)

    def test_a_branch_is_deleted_only_after_the_ownership_test_and_only_for_the_bot_s_prs(self):
        close = self.code[self.code.index('gh pr list --state open --limit 100'):]
        self.assertIn('app/github-actions', close)
        self.assertLess(close.index('owned FETCH_HEAD'), close.index('--delete-branch'))
        self.assertEqual(self.code.count('--delete-branch'), 1)
        self.assertRegex(close, r'gh pr close "\$old" --comment')  # the fallback keeps the branch

    def shell_owned(self, setup):
        body = re.search(r'(?ms)^ *owned\(\) \{\n(.*?)^ *\}$', self.code).group(1)
        tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, tmp)
        bot = '41898282+github-actions[bot]@users.noreply.github.com'
        script = ('set -euo pipefail\ncd "$1"\ngit init -q -b main .\n'
                  'export GIT_AUTHOR_NAME=n GIT_COMMITTER_NAME=n\n'
                  'export GIT_AUTHOR_EMAIL=person@example.com GIT_COMMITTER_EMAIL=person@example.com\n'
                  'git commit -q --allow-empty -m base\n' + setup.replace('BOT', bot) +
                  '\nbase=main; bot_email=' + bot + '\nset +e\nowned() {\n' + body + '\n}\n'
                  'for b in bot human amended empty; do owned "$b" && echo "$b yes" || echo "$b no"; done\n')
        out = subprocess.run(['bash', '-c', script, 'x', str(tmp)], capture_output=True, text=True, check=True).stdout
        return dict(line.split() for line in out.splitlines() if line.endswith(('yes', 'no')))

    def test_ownership_needs_the_bot_as_author_and_committer_of_every_commit(self):
        result = self.shell_owned("""
for b in bot human amended empty; do git branch $b main; done
bot() { GIT_AUTHOR_EMAIL=BOT GIT_COMMITTER_EMAIL=BOT git commit -q --allow-empty -m "$1"; }
git checkout -q bot; bot a; bot b
git checkout -q human; git commit -q --allow-empty -m h
git checkout -q amended; GIT_AUTHOR_EMAIL=BOT git commit -q --allow-empty -m rebased
git checkout -q main
""")
        self.assertEqual(result, {'bot': 'yes', 'human': 'no', 'amended': 'no', 'empty': 'no'})

    def test_one_foreign_commit_among_the_bot_s_makes_a_branch_foreign(self):
        result = self.shell_owned("""
for b in bot human amended empty; do git branch $b main; done
git checkout -q bot
GIT_AUTHOR_EMAIL=BOT GIT_COMMITTER_EMAIL=BOT git commit -q --allow-empty -m a
git commit -q --allow-empty -m person
git checkout -q main
""")
        self.assertEqual(result['bot'], 'no')

    def test_a_foreign_commit_is_seen_even_when_the_log_is_longer_than_a_pipe_buffer(self):
        # Actions runs `shell: bash` with -eo pipefail. A reader that quits on its first match
        # (`grep -q`) leaves `git log` writing into a closed pipe: SIGPIPE, status 141, and a `!` in
        # front of the pipeline turns that into "owned". It only happens when the log outruns the pipe
        # buffer, so this branch carries a person's newest commit on top of thousands of the bot's.
        stamp = r'author b <BOT> 1700000000 +0000\ncommitter b <BOT> 1700000000 +0000\ndata 1\nx\n'
        result = self.shell_owned("""
for b in human amended empty; do git branch $b main; done
{
  printf 'commit refs/heads/bot\\n""" + stamp + r"""from refs/heads/main\n\n'
  for i in $(seq 6000); do printf 'commit refs/heads/bot\n""" + stamp + r"""\n'; done
  printf 'commit refs/heads/bot\nauthor p <person@example.com> 1700000000 +0000\ncommitter p <person@example.com> 1700000000 +0000\ndata 1\nx\n\n'
} | git fast-import --quiet
""")
        self.assertEqual(result['bot'], 'no')

    def test_a_declined_bump_is_not_proposed_again_and_points_at_update_hold(self):
        self.assertIn('--state closed', self.code)
        self.assertIn('mergedAt == null', self.code)
        self.assertIn('update.hold', self.code)

    def test_every_per_dependency_step_failure_is_recorded_and_skipped(self):
        for needle in ('git checkout -q -f -B deps/work "$base" || { failed=1; continue; }',
                       'git commit -q -m "Dependencies: ${name} -> ${used}" || { failed=1; continue; }',
                       'g push --force origin "HEAD:refs/heads/${branch}" || { failed=1; continue; }'):
            self.assertIn(needle, self.code)

    def test_bumps_are_based_on_the_default_branch_even_when_dispatched_from_another_ref(self):
        self.assertIn('base="origin/${DEFAULT_BRANCH}"', self.code)
        self.assertIn('+refs/heads/${DEFAULT_BRANCH}:refs/remotes/origin/${DEFAULT_BRANCH}', self.code)
        self.assertEqual(self.code.count('ref: ${{ github.event.repository.default_branch }}'),
                         self.code.count('actions/checkout@'))

    def test_the_branch_name_uses_the_version_the_script_reports(self):
        self.assertIn('used=$(sed -n 1p "$bumped")', self.code)
        self.assertIn('branch="deps/${name}-${used}"', self.code)

    def test_an_unchanged_branch_is_not_pushed_again(self):
        self.assertIn('git diff --quiet FETCH_HEAD HEAD', self.code)

    def test_the_notices_are_committed_with_the_manifest(self):
        self.assertIn('git add ci/dependencies.json THIRD-PARTY-NOTICES.md', self.code)

    def test_nothing_is_checked_with_patch_and_its_fuzz(self):
        self.assertNotIn('--dry-run', self.code)
        self.assertNotIn('--dry-run', (ROOT / 'ci/deps-update.py').read_text().replace('patch --dry-run', ''))

    def test_the_graph_job_runs_the_snapshot_and_posts_it(self):
        self.assertIn('--snapshot', self.code)
        self.assertIn('dependency-graph/snapshots', self.code)


if __name__ == '__main__':
    unittest.main()
