#!/usr/bin/env python3
"""Negative controls exercise finished archives, not only generator branches."""
import io
import gzip
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from source_bundle import canonical, digest, read_regular, snapshot, validate, write_archive


class BundleTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.path = self.root / 'candidate.tar.gz'
        self.contents = {
            'LICENSE': (Path(os.environ.get('PLXNATIVE_TEST_GPL_TEXT', Path(__file__).resolve().parents[1] / 'LICENSE')).read_bytes(), 0o644),
            'LICENSING.md': (b'GPL-3.0-or-later', 0o644),
            'THIRD-PARTY-NOTICES.md': (b'third party', 0o644),
            'ipkroot/ctl/control': (b'License: GPL-3.0-or-later\n', 0o644),
            'Makefile': (b'all:\n\ttrue\n', 0o644),
            'rust-modules/Cargo.toml': (b'[package]\nlicense = "GPL-3.0-or-later"\n', 0o644),
            'rust-modules/Cargo.lock': (b'version = 4\n', 0o644),
            'pkg/appinfo.json': (b'{"id":"com.example.test"}', 0o644),
            'vendor/sentry-native/webos-arm32.patch': (b'patch source', 0o644),
        }
        deps = []
        for name in ['ffmpeg', 'sentry-native', 'cargo-vendor', 'rust-runtime']:
            path = 'dependencies/' + name + '/source.txt'
            data = ('editable source for ' + name).encode()
            self.contents[path] = data, 0o644
            deps.append({'id': name, 'version': 'pinned-test', 'license': 'MIT',
                         'sources': [{'path': path, 'sha256': digest(data)}], 'recipes': ['Makefile'],
                         'patches': ['vendor/sentry-native/webos-arm32.patch'] if name == 'sentry-native' else []})
        self.manifest = {'schema': 1, 'license': 'GPL-3.0-or-later', 'dependencies': deps,
                         'build_environment': {'compiler': 'fixture'}, 'git_commit': 'a' * 40}
        self.refresh()

    def tearDown(self):
        self.tmp.cleanup()

    def refresh(self):
        self.manifest['files'] = {name: {'sha256': digest(data), 'mode': mode}
                                  for name, (data, mode) in self.contents.items()}
        self.manifest['snapshot_sha256'] = snapshot(self.manifest['files'])

    def write(self):
        write_archive(self.path, dict(self.contents, **{'SOURCE-MANIFEST.json':
                                                      (canonical(self.manifest), 0o644)}), 1234)

    def check(self, **kwargs):
        self.write()
        return validate(self.path, **kwargs)

    def test_valid_and_deterministic(self):
        self.check(expected_snapshot=self.manifest['snapshot_sha256'])
        first = self.path.read_bytes()
        self.write()
        self.assertEqual(first, self.path.read_bytes())

    def test_missing_gpl(self):
        del self.contents['LICENSE']
        self.refresh()
        with self.assertRaisesRegex(ValueError, 'missing required source'):
            self.check()

    def test_truncated_gpl(self):
        self.contents['LICENSE'] = b'GNU GENERAL PUBLIC LICENSE', 0o644
        self.refresh()
        with self.assertRaisesRegex(ValueError, 'complete GPL'):
            self.check()

    def test_metadata_mit(self):
        for name, data in [('rust-modules/Cargo.toml', b'license = "MIT"'),
                           ('pkg/appinfo.json', b'{"license":"MIT"}'),
                           ('ipkroot/ctl/control', b'License: MIT\n')]:
            with self.subTest(name=name):
                old = self.contents[name]
                self.contents[name] = data, 0o644
                self.refresh()
                with self.assertRaisesRegex(ValueError, 'metadata must be'):
                    self.check()
                self.contents[name] = old

    def test_snapshot_mismatch(self):
        with self.assertRaisesRegex(ValueError, 'expected candidate'):
            self.check(expected_snapshot='0' * 64)

    def test_changed_file_and_mode(self):
        self.contents['Makefile'] = b'changed', 0o755
        with self.assertRaisesRegex(ValueError, 'set/hash/mode mismatch'):
            self.check()

    def test_missing_dependency_source(self):
        del self.contents['dependencies/ffmpeg/source.txt']
        self.refresh()
        with self.assertRaisesRegex(ValueError, 'wrong dependency source'):
            self.check()

    def test_missing_patch(self):
        del self.contents['vendor/sentry-native/webos-arm32.patch']
        self.refresh()
        with self.assertRaisesRegex(ValueError, 'recipe/patch'):
            self.check()

    def test_urls_do_not_replace_sources(self):
        self.manifest['dependencies'][0]['sources'] = []
        self.manifest['dependencies'][0]['url'] = 'https://example.org/source'
        with self.assertRaisesRegex(ValueError, 'missing dependency sources'):
            self.check()

    def test_missing_runtime(self):
        self.manifest['dependencies'].pop()
        with self.assertRaisesRegex(ValueError, 'rust-runtime'):
            self.check()

    def test_unknown_license(self):
        self.manifest['dependencies'][0]['license'] = 'NOASSERTION'
        with self.assertRaisesRegex(ValueError, 'unresolved dependency license'):
            self.check()

    def test_corrupt_compression_requires_exact_reviewed_hash(self):
        data = b'\x1f\x8bintentionally invalid gzip fixture'
        self.contents['dependencies/ffmpeg/corrupt.bin'] = data, 0o644
        self.refresh()
        with self.assertRaisesRegex(ValueError, 'unreadable compressed'):
            self.check()
        with patch('source_bundle.PUBLIC_CORRUPT_FIXTURES', {digest(data): 'synthetic reviewed fixture'}):
            self.check()
            self.contents['dependencies/ffmpeg/corrupt.bin'] = data + b'changed', 0o644
            self.refresh()
            with self.assertRaisesRegex(ValueError, 'unreadable compressed'):
                self.check()

    def test_compressed_single_file(self):
        self.contents['dependencies/ffmpeg/example.txt.gz'] = gzip.compress(b'public example'), 0o644
        self.refresh()
        self.check()

    def test_compressed_single_file_secret(self):
        data = b'PLXNATIVE_' + b'PRIVATE_SENTINEL_compressed'
        self.contents['dependencies/ffmpeg/example.txt.gz'] = gzip.compress(data), 0o644
        self.refresh()
        with self.assertRaisesRegex(ValueError, 'credential pattern'):
            self.check()

    def test_private_key_body(self):
        data = b'-----BEGIN ' + b'PRIVATE KEY-----\n' + b'a' * 48 + b'\n'
        self.contents['src/key.c'] = data, 0o644
        self.refresh()
        with self.assertRaisesRegex(ValueError, 'credential pattern'):
            self.check()

    def test_documented_key_header_is_not_a_key(self):
        data = b'/* \"-----BEGIN ' + b'PRIVATE KEY-----\\n...\\n-----END PRIVATE KEY-----\\n\" */'
        self.contents['src/public-example.c'] = data, 0o644
        self.refresh()
        self.check()

    def test_private_sentinel(self):
        self.contents['src/private.c'] = b'PLXNATIVE_' + b'PRIVATE_SENTINEL_secret', 0o644
        self.refresh()
        with self.assertRaisesRegex(ValueError, 'credential pattern'):
            self.check()

    def test_private_literal(self):
        self.contents['src/private.c'] = b'private-household-hostname', 0o644
        self.refresh()
        with self.assertRaisesRegex(ValueError, 'private value'):
            self.check(private_values=[b'private-household-hostname'])

    def test_private_path(self):
        self.contents['pkg/auth.json'] = b'{}', 0o644
        self.refresh()
        with self.assertRaisesRegex(ValueError, 'private path'):
            self.check()

    def nested_archive(self, *, link=None, secret=False):
        out = io.BytesIO()
        with tarfile.open(fileobj=out, mode='w:gz') as archive:
            info = tarfile.TarInfo('source/file')
            if link:
                info.type, info.linkname = tarfile.SYMTYPE, link
                archive.addfile(info)
            else:
                data = b'PLXNATIVE_' + b'PRIVATE_SENTINEL_nested' if secret else b'normal'
                info.size = len(data)
                archive.addfile(info, io.BytesIO(data))
        self.contents['dependencies/ffmpeg/nested.tar.gz'] = out.getvalue(), 0o644
        self.refresh()

    def test_nested_secret(self):
        self.nested_archive(secret=True)
        with self.assertRaisesRegex(ValueError, 'credential pattern'):
            self.check()

    def test_nested_symlink_escape(self):
        self.nested_archive(link='../../outside')
        with self.assertRaisesRegex(ValueError, 'link escapes'):
            self.check()

    def test_public_demo_exception_is_hash_scoped_and_keeps_literal_checks(self):
        from source_bundle import scan
        sample = b'https://' + b'0' * 32 + b'@o0.ingest.sentry.io/1'
        name = 'fixture/sample.java'
        with patch('source_bundle.PUBLIC_DEMO_DSN_FILES', {name: digest(sample)}):
            scan(sample, 'outer.tar.gz:' + name)
            with self.assertRaisesRegex(ValueError, 'credential pattern'):
                scan(sample + b'changed', 'outer.tar.gz:' + name)
            with self.assertRaisesRegex(ValueError, 'credential pattern'):
                scan(sample, 'another.java')
            with self.assertRaisesRegex(ValueError, 'private value'):
                scan(sample, name, [sample])

    def test_input_symlink_escape(self):
        (self.root / 'link').symlink_to('/etc/passwd')
        with self.assertRaisesRegex(ValueError, 'symlink input'):
            read_regular(self.root, 'link')

    def test_outer_symlink(self):
        self.write()
        with tarfile.open(self.root / 'bad.tar', 'w') as archive:
            info = tarfile.TarInfo('src/link')
            info.type, info.linkname = tarfile.SYMTYPE, '/etc/passwd'
            archive.addfile(info)
        with self.assertRaisesRegex(ValueError, 'nonregular'):
            validate(self.root / 'bad.tar')

    def test_path_escape(self):
        self.contents['../outside'] = b'file', 0o644
        self.refresh()
        with self.assertRaisesRegex(ValueError, 'unsafe archive path'):
            self.check()

    def test_restore_wires_supplied_cargo_and_runtime_sources(self):
        self.contents['rust-modules/.cargo/config.toml'] = b'[build]\n', 0o644
        for name, files in [
            ('cargo-vendor', {'cargo-vendor/test-crate/Cargo.toml': (b'[package]\nname="fixture"\n', 0o644)}),
            ('rust-runtime', {'rust-src/library/Cargo.lock': (b'fixture runtime source', 0o644),
                              'rust-notices/LICENSE': (b'fixture retained notice', 0o644)})]:
            nested = self.root / (name + '.tar.gz')
            write_archive(nested, files, 0)
            member = 'dependencies/' + name + '/' + nested.name
            self.contents[member] = nested.read_bytes(), 0o644
            dep = next(d for d in self.manifest['dependencies'] if d['id'] == name)
            dep['sources'] = [{'path': member, 'sha256': digest(nested.read_bytes())}]
        self.refresh()
        self.write()
        sysroot = self.root / 'isolated-toolchain'
        (sysroot / 'bin').mkdir(parents=True)
        compiler = sysroot / 'bin/rustc'
        compiler.write_text('#!/bin/sh\nprintf "commit-hash: pinned-test\\n"\n')
        compiler.chmod(0o755)
        destination = self.root / 'restored'
        result = subprocess.run([sys.executable, str(Path(__file__).with_name('restore-source-inputs.py')),
                                 str(self.path), '--expect-snapshot', self.manifest['snapshot_sha256'],
                                 '--destination', str(destination), '--rust-sysroot', str(sysroot)],
                                capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((sysroot / 'lib/rustlib/src/rust/library/Cargo.lock').read_bytes(), b'fixture runtime source')
        self.assertTrue((destination / 'rust-modules/cargo-vendor/test-crate/Cargo.toml').is_file())
        self.assertIn(str(destination / 'rust-modules/cargo-vendor'),
                      (destination / 'rust-modules/.cargo/config.toml').read_text())
        self.assertTrue((destination / 'vendor/ffmpeg-build/source.txt').is_file())
        self.assertTrue((destination / 'vendor/source.txt').is_file())
        result = subprocess.run([sys.executable, str(Path(__file__).with_name('restore-source-inputs.py')),
                                 str(self.path), '--expect-snapshot', self.manifest['snapshot_sha256'],
                                 '--destination', str(destination), '--rust-sysroot', str(sysroot)],
                                capture_output=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn(b'destination must not exist', result.stderr)

    def producer_fixture(self):
        repo = self.root / 'repo'
        downloads = self.root / 'downloads'
        repo.mkdir()
        downloads.mkdir()
        deps = []
        for name, (data, mode) in self.contents.items():
            if name.startswith('dependencies/'):
                path = downloads / name
            else:
                path = repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            path.chmod(mode)
        for original in self.manifest['dependencies']:
            deps.append(dict(original))
        spec = self.root / 'inputs.json'
        spec.write_text(json.dumps({'dependencies': deps, 'build_environment': {'compiler': 'fixture'}}))
        for command in [['init', '-q'], ['add', '.'],
                        ['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
                         'commit', '-qm', 'fixture']]:
            subprocess.run(['git', '-C', str(repo)] + command, check=True, capture_output=True)
        script = str(Path(__file__).with_name('make-source-bundle.py'))
        command = [sys.executable, script, '--root', str(repo), '--output', str(self.path),
                   '--dependencies', str(spec), '--dependency-root', str(downloads)]
        return repo, command

    def test_producer_round_trip_and_dirty_snapshot(self):
        repo, command = self.producer_fixture()
        result = subprocess.run(command, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        first = self.path.read_bytes()
        before = validate(self.path)['snapshot_sha256']
        result = subprocess.run(command, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(first, self.path.read_bytes())
        (repo / 'Makefile').write_text('all:\n\techo changed\n')
        result = subprocess.run(command, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotEqual(before, validate(self.path)['snapshot_sha256'])

    def test_producer_untracked_source_refused(self):
        repo, command = self.producer_fixture()
        (repo / 'src').mkdir()
        (repo / 'src/new.c').write_text('new source')
        result = subprocess.run(command, capture_output=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn(b'untracked source', result.stderr)

    def test_producer_keeps_package_and_host_check_inputs(self):
        repo, command = self.producer_fixture()
        required = ['licenses/retained.txt', '.claude/hooks/host-check.py',
                    '.agents/skills/wake-tv/wake-tv.sh']
        for name in required:
            path = repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('required source fixture')
        subprocess.run(['git', '-C', str(repo), 'add', '.'], check=True)
        result = subprocess.run(command, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        files = validate(self.path)['files']
        for name in required + ['ipkroot/ctl/control']:
            self.assertIn(name, files)

    def test_producer_ignored_private_file_not_archived(self):
        repo, command = self.producer_fixture()
        (repo / '.gitignore').write_text('.tv-host\n')
        subprocess.run(['git', '-C', str(repo), 'add', '.gitignore'], check=True)
        (repo / '.tv-host').write_bytes(b'PLXNATIVE_' + b'PRIVATE_SENTINEL_hidden')
        result = subprocess.run(command, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn('.tv-host', validate(self.path)['files'])

    def test_checker_cli_rejects_wrong_snapshot(self):
        self.write()
        result = subprocess.run([sys.executable, str(Path(__file__).with_name('check-source-bundle.py')),
                                 str(self.path), '--expect-snapshot', '0' * 64], capture_output=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn(b'expected candidate', result.stderr)


if __name__ == '__main__':
    unittest.main()
