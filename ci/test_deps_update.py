#!/usr/bin/env python3
"""ci/deps-update.py and .github/workflows/deps-update.yml: the manifest's update sources are valid,
tag parsing and version ordering work per source type (fixtures, no network), a bump rewrites the
right strings, the dependency-graph snapshot has the submission API's shape, and the workflow keeps
its security shape (minimal permissions, SHA-pinned actions, untrusted strings never in a script).
"""
import importlib.util
import json
import re
import shutil
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / 'ci'))
spec = importlib.util.spec_from_file_location('deps_update', ROOT / 'ci/deps-update.py')
du = importlib.util.module_from_spec(spec)
spec.loader.exec_module(du)

WORKFLOW = ROOT / '.github/workflows/deps-update.yml'


def manifest():
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
        self.assertEqual(du.update_problems(manifest()), [])

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
        shutil.copy(ROOT / 'ci/dependencies.json', path)
        before = json.loads(path.read_text())
        release = {'version': '1.0.18', 'tag': 'v1.0.18', 'url': 'https://example.invalid/r'}
        with mock.patch.object(du, 'latest', return_value=release), \
                mock.patch.object(du, 'download_digest', return_value='c' * 64):
            old, new, notes = du.bump('fribidi', path=path, body_file=scratch / 'body.md')
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
        shutil.copy(ROOT / 'ci/dependencies.json', path)
        release = {'version': '9.9.9', 'tag': '9.9.9', 'url': 'https://example.invalid/r'}
        with mock.patch.object(du, 'latest', return_value=release), \
                mock.patch.object(du, 'download_digest', return_value='d' * 64), \
                mock.patch.object(du, 'check_patches', side_effect=du.PatchError('x.patch no longer applies')):
            with self.assertRaises(du.PatchError):
                du.bump('sentry-native', path=path, body_file=scratch / 'body.md')
        self.assertEqual(next(d for d in json.loads(path.read_text()) if d['id'] == 'sentry-native')['version'], '9.9.9')
        self.assertIn('no longer applies', (scratch / 'body.md').read_text())

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
                du.bump('fribidi', path=ROOT / 'ci/dependencies.json')


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

    def test_the_graph_job_runs_the_snapshot_and_posts_it(self):
        self.assertIn('--snapshot', self.code)
        self.assertIn('dependency-graph/snapshots', self.code)


if __name__ == '__main__':
    unittest.main()
