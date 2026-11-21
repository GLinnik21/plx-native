"""Native/offline adversaries for the reviewed locked-source proof (no Cargo)."""
from pathlib import Path
import copy
import importlib.util
import json
import os
import io
import shutil
import tarfile
import tempfile
import unittest
from unittest import mock

import readonly_dependencies as proof

ROOT = Path(__file__).resolve().parents[1]
ABSOLUTE = ['::', 'serde_json', '::', 'Value']


class LockedJsonProofTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='locked-json-proof-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / 'rust-modules'
        self.certificate = proof._load_certificate()
        workspace = self.certificate['workspace']
        for relative in list(workspace['manifests']) + ['Cargo.lock']:
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / 'rust-modules' / relative, destination)
        for relative, digest in workspace['configs'].items():
            if digest is not None:
                destination = self.root / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / 'rust-modules' / relative, destination)
        binding = workspace['owners']['plex/Cargo.toml']
        destination = self.root / binding['lib']
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / 'rust-modules' / binding['lib'], destination)
        self.source = self.root / 'plex/src/probe.rs'
        self.source.write_text('pub struct Owned(pub ::serde_json::Value);\n', encoding='utf-8')

    def valid(self, tokens=ABSOLUTE, context=None):
        return proof.certify(self.root, self.source, tokens, context)

    def alter(self, relative, old, new):
        path = self.root / relative
        text = path.read_text(encoding='utf-8')
        self.assertIn(old, text)
        path.write_text(text.replace(old, new, 1), encoding='utf-8')

    def cert_mutation(self, mutate):
        certificate = copy.deepcopy(self.certificate)
        mutate(certificate)
        path = Path(self.temp.name) / 'certificate.json'
        path.write_text(json.dumps(certificate), encoding='utf-8')
        with mock.patch.object(proof, 'CERTIFICATE_PATH', path):
            self.assertFalse(self.valid())

    def test_current_canonical_locked_json(self):
        self.assertTrue(self.valid())
        evidence = proof.provenance(self.root, self.source, ABSOLUTE)
        self.assertEqual(evidence['package'], self.certificate['package'])
        self.assertEqual(evidence['owner_manifest'], 'plex/Cargo.toml')
        self.assertEqual(evidence['certificate_sha256'], proof.CERTIFICATE_SHA256)

    def test_lexer_tokens_are_the_cursor_matched_stream(self):
        # Golden stream from the slicing implementation (text[i:] per probe) that matching at the
        # cursor replaced: nested comments, a lifetime, r#ident, char/byte/string literals, raw strings.
        source = ("use serde_json::Value; /* a /* nested */ comment */ // line\n"
                  "fn f<'a>(x: &'a str) -> r#type { let c = 'x'; let b = b'\\n'; let s = \"a\\\"b\"; let raw = r#\"q\"#; "
                  "let rb = br##\"x\"##;\n  mod serde_json; ::serde_json::Value }\n")
        self.assertEqual(proof._tokens(source), [
            'use', 'serde_json', '::', 'Value', ';', 'fn', 'f', '<', "'", 'a', '>', '(', 'x', ':', '&', "'", 'a',
            'str', ')', '-', '>', 'type', '{', 'let', 'c', '=', "'x'", ';', 'let', 'b', '=', "b'\\n'", ';', 'let',
            's', '=', '"a\\"b"', ';', 'let', 'raw', '=', '"<raw>"', ';', 'let', 'rb', '=', '"<raw>"', ';', 'mod',
            'serde_json', ';', '::', 'serde_json', '::', 'Value', '}'])
        with self.assertRaises(ValueError): proof._tokens('/* /* one */')
        with self.assertRaises(ValueError): proof._tokens('let r = r#"open')

    def test_separate_colon_tokens(self):
        self.assertTrue(self.valid([':', ':', 'serde_json', ':', ':', 'Value']))

    def test_root_relative_source(self):
        self.assertTrue(proof.certify(self.root, 'plex/src/probe.rs', ABSOLUTE))

    def test_current_workspace_source_and_features(self):
        self.assertTrue(proof.certify(ROOT / 'rust-modules', ROOT / 'rust-modules/plex/src/plex/session.rs', ABSOLUTE))
        self.assertEqual(self.certificate['features']['definitions']['arbitrary_precision'], [])
        self.assertEqual(self.certificate['grammar']['N']['arbitrary_precision'], 'alloc::string::String')
        self.assertEqual(self.certificate['grammar']['Value']['enum']['Array'], ['alloc::vec::Vec<Value>'])
        self.assertEqual(self.certificate['grammar']['MapImpl<K,V>']['not(preserve_order)'], 'alloc::collections::BTreeMap<K,V>')

    def test_offline_without_cargo_cache_or_network(self):
        with mock.patch.dict(os.environ, {'PATH': '', 'CARGO_HOME': '/nonexistent', 'RUSTUP_HOME': '/nonexistent'}), \
                mock.patch('socket.create_connection', side_effect=AssertionError('network forbidden')), \
                mock.patch('subprocess.run', side_effect=AssertionError('process forbidden')):
            self.assertTrue(self.valid())

    def test_relative_external_path_rejected(self):
        self.assertFalse(self.valid(['serde_json', '::', 'Value']))

    def test_local_value_with_cell_rejected(self):
        self.source.write_text('use std::cell::Cell; struct Value(Cell<u8>);', encoding='utf-8')
        self.assertFalse(self.valid(['Value']))
        self.assertFalse(self.valid(['crate', '::', 'Value']))

    def test_unknown_foreign_type_and_container_rejected(self):
        self.assertFalse(self.valid(['::', 'evil', '::', 'Value']))
        self.assertFalse(self.valid(['::', 'serde_json', '::', 'Map', '<', 'String', ',', 'Value', '>']))

    def test_callback_raw_reference_and_generic_claims_rejected(self):
        for tokens in (['fn', '(', ')'], ['*', 'mut', 'Value'], ['&'] + ABSOLUTE, ABSOLUTE + ['<', 'T', '>']):
            with self.subTest(tokens=tokens): self.assertFalse(self.valid(tokens))

    def test_type_string_not_tokens_rejected(self):
        self.assertFalse(self.valid('::serde_json::Value'))

    def test_same_named_local_module_rejected(self):
        self.source.write_text('mod serde_json { struct Value(std::cell::Cell<u8>); }', encoding='utf-8')
        self.assertFalse(self.valid())

    def test_same_named_local_type_alias_rejected(self):
        self.source.write_text('type serde_json = evil::Authority;', encoding='utf-8')
        self.assertFalse(self.valid())

    def test_same_named_import_alias_rejected(self):
        self.source.write_text('use evil as serde_json;', encoding='utf-8')
        self.assertFalse(self.valid())

    def test_grouped_same_named_import_rejected(self):
        self.source.write_text('use evil::{serde_json};', encoding='utf-8')
        self.assertFalse(self.valid())

    def test_extern_crate_rename_rejected(self):
        self.source.write_text('extern crate evil as serde_json;', encoding='utf-8')
        self.assertFalse(self.valid())

    def test_raw_identifier_shadow_rejected(self):
        self.source.write_text('mod r#serde_json {}', encoding='utf-8')
        self.assertFalse(self.valid())

    def test_context_does_not_grant_identity(self):
        self.assertFalse(self.valid(context={'definitions': ['serde_json']}))
        self.assertFalse(self.valid(context={'imports': [('serde_json', ('evil',))]}))
        self.assertFalse(self.valid(context={'trust_me': True}))
        self.assertTrue(self.valid(context={'definitions': ['Owned'], 'imports': [('Value', ('serde_json', 'Value'))]}))

    def test_comments_and_literals_do_not_create_shadows(self):
        self.source.write_text('/* outer /* nested */ mod serde_json {} */\n// use evil as serde_json;\nconst S: &str = r###"mod serde_json {}"###;\n', encoding='utf-8')
        self.assertTrue(self.valid())

    def test_shadow_in_other_source_file_rejected(self):
        (self.source.parent / 'other.rs').write_text('use evil as serde_json;', encoding='utf-8')
        self.assertFalse(self.valid())

    def test_changed_crate_root_rejected(self):
        with (self.source.parent / 'lib.rs').open('a', encoding='utf-8') as out: out.write('\nextern crate evil as serde_json;\n')
        self.assertFalse(self.valid())

    def test_wrong_source_crate_rejected_even_when_json_elsewhere(self):
        other = self.root / 'machine/src/probe.rs'
        other.parent.mkdir(parents=True, exist_ok=True)
        other.write_text('struct Value(std::cell::Cell<u8>);', encoding='utf-8')
        self.assertFalse(proof.certify(self.root, other, ABSOLUTE))

    def test_outside_workspace_and_missing_source_rejected(self):
        self.assertFalse(proof.certify(self.root, Path(self.temp.name) / 'elsewhere.rs', ABSOLUTE))
        self.assertFalse(proof.certify(self.root, self.source.with_name('absent.rs'), ABSOLUTE))

    def test_manifest_dependency_renamed_to_evil_rejected(self):
        self.alter('plex/Cargo.toml', 'serde_json = { version = "1"', 'serde_json = { package = "evil", version = "1"')
        self.assertFalse(self.valid())

    def test_canonical_package_under_wrong_alias_rejected(self):
        self.alter('plex/Cargo.toml', 'serde_json = { version = "1"', 'evil = { package = "serde_json", version = "1"')
        self.assertFalse(self.valid())

    def test_manifest_comment_change_requires_review(self):
        with (self.root / 'plex/Cargo.toml').open('a', encoding='utf-8') as out: out.write('\n# changed\n')
        self.assertFalse(self.valid())

    def test_path_dependency_rejected(self):
        self.alter('plex/Cargo.toml', 'serde_json = { version = "1"', 'serde_json = { path = "../evil", version = "1"')
        self.assertFalse(self.valid())

    def test_git_dependency_rejected(self):
        self.alter('plex/Cargo.toml', 'serde_json = { version = "1"', 'serde_json = { git = "https://example.org/evil", version = "1"')
        self.assertFalse(self.valid())

    def test_workspace_patch_rejected(self):
        with (self.root / 'Cargo.toml').open('a', encoding='utf-8') as out: out.write('\n[patch.crates-io]\nserde_json = { path = "evil" }\n')
        self.assertFalse(self.valid())

    def test_cargo_source_replacement_rejected(self):
        with (self.root / '.cargo/config.toml').open('a', encoding='utf-8') as out: out.write('\n[source.crates-io]\nreplace-with = "evil"\n')
        self.assertFalse(self.valid())

    def test_new_repo_config_rejected(self):
        path = self.root.parent / '.cargo/config.toml'
        path.parent.mkdir(exist_ok=True)
        path.write_text('[source.crates-io]\nreplace-with = "evil"\n', encoding='utf-8')
        self.assertFalse(self.valid())

    def test_checksum_change_rejected(self):
        self.alter('Cargo.lock', self.certificate['package']['checksum'], '0' * 64)
        self.assertFalse(self.valid())

    def test_version_change_rejected(self):
        self.alter('Cargo.lock', 'name = "serde_json"\nversion = "1.0.154"', 'name = "serde_json"\nversion = "1.0.155"')
        self.assertFalse(self.valid())

    def test_registry_source_change_rejected(self):
        self.alter('Cargo.lock', 'name = "serde_json"\nversion = "1.0.154"\nsource = "' + proof.REGISTRY + '"', 'name = "serde_json"\nversion = "1.0.154"\nsource = "git+https://example.org/json"')
        self.assertFalse(self.valid())

    def test_duplicate_locked_package_rejected(self):
        with (self.root / 'Cargo.lock').open('a', encoding='utf-8') as out:
            out.write('\n[[package]]\nname = "serde_json"\nversion = "1.0.154"\nsource = "' + proof.REGISTRY + '"\nchecksum = "' + '0' * 64 + '"\n')
        self.assertFalse(self.valid())
        with self.assertRaises(ValueError): proof._locked(self.root)

    def test_unknown_feature_rejected(self):
        self.alter('plex/Cargo.toml', 'features = ["alloc", "std"]', 'features = ["alloc", "std", "mutable_value"]')
        self.assertFalse(self.valid())

    def test_preserve_order_rejected(self):
        self.alter('plex/Cargo.toml', 'features = ["alloc", "std"]', 'features = ["alloc", "std", "preserve_order"]')
        self.assertFalse(self.valid())

    def test_transitive_preserve_order_lock_edge_rejected(self):
        self.alter('Cargo.lock', 'name = "serde_json"\n', 'name = "serde_json"\n')
        path = self.root / 'Cargo.lock'
        text = path.read_text(encoding='utf-8')
        at = text.index('dependencies = [', text.index('name = "serde_json"')) + len('dependencies = [')
        path.write_text(text[:at] + '\n "indexmap",' + text[at:], encoding='utf-8')
        self.assertFalse(self.valid())
        with self.assertRaises(ValueError): proof._locked(self.root)

    def test_certificate_mutable_field_rejected(self):
        self.cert_mutation(lambda cert: cert['grammar']['Value']['enum'].update({'Cell': ['std::cell::Cell<u8>']}))

    def test_certificate_callback_field_rejected(self):
        self.cert_mutation(lambda cert: cert['grammar']['Number']['fields'].update({'callback': 'fn()'}))

    def test_certificate_unknown_schema_field_rejected(self):
        self.cert_mutation(lambda cert: cert.update({'authority': 'trust me'}))

    def test_certificate_feature_boundary_change_rejected(self):
        self.cert_mutation(lambda cert: cert['features']['supported'].append('preserve_order'))

    def test_certificate_source_evidence_change_rejected(self):
        self.cert_mutation(lambda cert: cert['source_files']['src/map.rs'].update({'sha256': '0' * 64}))

    def test_bad_archive_rejected_before_inspection(self):
        archive = Path(self.temp.name) / 'evil.crate'
        archive.write_bytes(b'not the locked archive')
        with mock.patch.object(proof.tarfile, 'open', side_effect=AssertionError('must verify checksum first')):
            with self.assertRaisesRegex(ValueError, 'BEFORE inspection'): proof.produce_certificate(self.root, archive)

    def test_workspace_inheritance_resolves_actual_package(self):
        fixture = Path(self.temp.name) / 'inheritance'
        (fixture / 'member/src').mkdir(parents=True)
        (fixture / 'Cargo.toml').write_text('[workspace.dependencies]\nserde_json = { version = "1", features = ["alloc"] }\n', encoding='utf-8')
        (fixture / 'member/Cargo.toml').write_text('[package]\nname = "member"\nedition = "2021"\n[dependencies]\nserde_json = { workspace = true, features = ["std"] }\n', encoding='utf-8')
        (fixture / 'member/src/lib.rs').write_text('pub mod probe;\n', encoding='utf-8')
        bindings = proof._bindings(fixture, ['member/Cargo.toml'], self.certificate['package'])
        self.assertEqual(bindings['member/Cargo.toml']['dependency'][0]['features'], ['alloc', 'std'])
        (fixture / 'Cargo.toml').write_text('[workspace.dependencies]\nserde_json = { package = "evil", version = "1" }\n', encoding='utf-8')
        with self.assertRaisesRegex(ValueError, 'noncanonical'): proof._bindings(fixture, ['member/Cargo.toml'], self.certificate['package'])

    def test_ordinary_std_owned_data_stays_structurally_inert(self):
        spec = importlib.util.spec_from_file_location('locked_json_shape_fixture', ROOT / 'ci/check-machine-shape.py')
        shape = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(shape)
        import machine_readonly
        checker = machine_readonly.Proof(self.root, shape)
        tokens = shape.production_tokens('std::collections::BTreeMap<String, Vec<Option<u64>>>')
        self.assertTrue(checker.inert(self.source, tokens))
        self.assertFalse(checker.inert(self.source, shape.production_tokens('std::cell::Cell<u8>')))
        self.assertFalse(checker.inert(self.source, shape.production_tokens('evil::OwnedMap<String, u64>')))

    def test_lf_and_crlf_checkouts_have_identical_identity(self):
        workspace = self.certificate['workspace']
        paths = list(workspace['manifests']) + ['Cargo.lock', workspace['owners']['plex/Cargo.toml']['lib']]
        paths += [p for p, digest in workspace['configs'].items() if digest is not None]
        for path in paths:
            file = self.root / path
            file.write_bytes(file.read_bytes().replace(b'\r\n', b'\n'))
        self.assertTrue(self.valid())
        for path in paths:
            file = self.root / path
            file.write_bytes(file.read_bytes().replace(b'\n', b'\r\n'))
        self.assertTrue(self.valid())

    def change_root_release(self, version, manifest=True, lock=True):
        path = self.root / 'Cargo.toml'
        text = path.read_text(encoding='utf-8')
        package = proof._table(proof._sections(text)['package'])
        old, name = package['version'], package['name']
        if manifest:
            path.write_text(text.replace('version = "' + old + '"', 'version = "' + version + '"', 1), encoding='utf-8')
        if lock:
            path = self.root / 'Cargo.lock'
            old_entry = 'name = "' + name + '"\nversion = "' + old + '"'
            text = path.read_text(encoding='utf-8')
            self.assertEqual(text.count(old_entry), 1)
            path.write_text(text.replace(old_entry, 'name = "' + name + '"\nversion = "' + version + '"'), encoding='utf-8')

    def test_coherent_local_root_release_versions_keep_source_identity(self):
        for version in ('0.9.0', '0.10.0', '2.34.56'):
            with self.subTest(version=version):
                self.change_root_release(version)
                self.assertTrue(self.valid())

    def test_local_root_release_must_match_lock(self):
        self.change_root_release('4.5.6', lock=False)
        self.assertFalse(self.valid())

    def test_local_root_lock_must_match_manifest(self):
        self.change_root_release('4.5.6', manifest=False)
        self.assertFalse(self.valid())

    def test_local_root_release_is_not_dependency_or_source_approval(self):
        self.change_root_release('4.5.6')
        path = self.root / 'Cargo.lock'
        original = path.read_text(encoding='utf-8')
        package = proof._table(proof._sections((self.root / 'Cargo.toml').read_text(encoding='utf-8'))['package'])
        entry = 'name = "' + package['name'] + '"\nversion = "4.5.6"'
        for extra in ('\nsource = "git+https://example.org/evil"', '\nchecksum = "' + '0' * 64 + '"'):
            with self.subTest(extra=extra):
                path.write_text(original.replace(entry, entry + extra), encoding='utf-8')
                self.assertFalse(self.valid())
        path.write_text(original, encoding='utf-8')
        path = self.root / 'Cargo.toml'
        path.write_text(path.read_text(encoding='utf-8') + '\n# unreviewed metadata\n', encoding='utf-8')
        self.assertFalse(self.valid())

    def test_local_root_prerelease_and_noncanonical_versions_fail_closed(self):
        originals = {p: (self.root / p).read_bytes() for p in ('Cargo.toml', 'Cargo.lock')}
        for version in ('01.2.3', '1.2.3-rogue', '1.2.3+rogue', '1.2'):
            with self.subTest(version=version):
                for p, data in originals.items():
                    (self.root / p).write_bytes(data)
                self.change_root_release(version)
                self.assertFalse(self.valid())

    def test_bare_carriage_return_is_not_checkout_normalization(self):
        for relative in ('Cargo.toml', 'Cargo.lock'):
            path = self.root / relative
            original = path.read_bytes()
            path.write_bytes(original.replace(b'\r\n', b'\n').replace(b'\n', b'\r'))
            self.assertFalse(self.valid())
            path.write_bytes(original)

    def test_local_root_duplicate_lock_identity_fails_closed(self):
        self.change_root_release('4.5.6')
        name = proof._table(proof._sections((self.root / 'Cargo.toml').read_text(encoding='utf-8'))['package'])['name']
        path = self.root / 'Cargo.lock'
        path.write_text(path.read_text(encoding='utf-8') + '\n[[package]]\nname = "' + name + '"\nversion = "4.5.6"\n', encoding='utf-8')
        with self.assertRaisesRegex(ValueError, 'ambiguous/nonlocal'):
            proof._workspace_hashes(self.root, proof._manifest_paths(self.root))
        self.assertFalse(self.valid())

    def root_identity(self):
        package = proof._table(proof._sections((self.root / 'Cargo.toml').read_text(encoding='utf-8'))['package'])
        return package['name'], package['version']

    def test_local_root_version_cannot_participate_in_resolution(self):
        # A lock dependent that lists the root WITH a version token (alone, or with a source) is rejected: that
        # token is what would let the release number select something.
        name, version = self.root_identity()
        path = self.root / 'Cargo.lock'
        original = path.read_text(encoding='utf-8')
        for token in (name + ' ' + version, name + ' ' + version + ' (path+file:///elsewhere)', name + ' 9.9.9'):
            with self.subTest(token=token):
                path.write_text(original.replace('dependencies = [', 'dependencies = [\n "' + token + '",', 1), encoding='utf-8')
                with self.assertRaisesRegex(ValueError, 'participates in dependency resolution'):
                    proof._workspace_hashes(self.root, proof._manifest_paths(self.root))
                self.assertFalse(self.valid())

    DEPENDENT = '[package]\nname = "plx_platform_probe"\nversion = "0.0.0"\nedition = "2021"\n\n'

    def add_root_dependent(self, tables='[dependencies]\nplxnative-modules = { path = "..", default-features = false }\n'):
        """A workspace member that depends on the root by path, in the manifest and, by bare name, in the lock."""
        name, _ = self.root_identity()
        manifest = self.root / 'Cargo.toml'
        text = manifest.read_text(encoding='utf-8')
        manifest.write_text(text.replace('members = [', 'members = [\n    "plx_platform_probe",', 1), encoding='utf-8')
        member = self.root / 'plx_platform_probe'
        member.mkdir()
        (member / 'Cargo.toml').write_text(self.DEPENDENT + tables, encoding='utf-8')
        lock = self.root / 'Cargo.lock'
        lock.write_text(lock.read_text(encoding='utf-8') + '\n[[package]]\nname = "plx_platform_probe"\nversion = "0.0.0"\n'
                        'dependencies = [\n "' + name + '",\n]\n', encoding='utf-8')

    def synthetic_crate(self):
        """A crate archive whose sources pass the review, and the lock entry that names its checksum, so the producer runs offline."""
        for binding in self.certificate['workspace']['owners'].values():
            destination = self.root / binding['lib']
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / 'rust-modules' / binding['lib'], destination)
        locked, _ = proof._locked(self.root)
        sources = closure_sources(self.certificate)
        sources['Cargo.toml'] = '[package]\nname = "serde_json"\nversion = "' + locked['version'] + '"\n\n' + sources['Cargo.toml']
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode='w:gz') as crate:
            for relative in proof.SOURCE_PATHS:
                body = sources[relative].encode('utf-8')
                member = tarfile.TarInfo('serde_json-' + locked['version'] + '/' + relative)
                member.size = len(body)
                crate.addfile(member, io.BytesIO(body))
        archive = Path(self.temp.name) / 'serde_json.crate'
        archive.write_bytes(data.getvalue())
        lock = self.root / 'Cargo.lock'
        lock.write_text(lock.read_text(encoding='utf-8').replace(locked['checksum'], proof._sha(data.getvalue()), 1), encoding='utf-8')
        return archive

    def test_a_bare_name_path_dependent_of_the_root_does_not_make_its_release_resolve(self):
        # With a platform-style crate depending on the root by path, the root's release number still moves nothing the
        # certificate pins: the masked manifest hash and the masked lock hash are unchanged, and the producer still runs.
        self.add_root_dependent()
        archive = self.synthetic_crate()
        paths = proof._manifest_paths(self.root)
        self.assertIn('plx_platform_probe/Cargo.toml', paths)
        before = proof._workspace_hashes(self.root, paths)
        certificate = proof.produce_certificate(self.root, archive)
        self.assertIn('plx_platform_probe/Cargo.toml', certificate['workspace']['manifests'])
        for version in ('0.9.0', '4.5.6'):
            with self.subTest(version=version):
                self.change_root_release(version)
                after = proof._workspace_hashes(self.root, paths)
                self.assertEqual(after['manifests']['Cargo.toml'], before['manifests']['Cargo.toml'])
                self.assertEqual(after['lock_sha256'], before['lock_sha256'])
                self.assertEqual(after, before)
                self.assertEqual(proof.produce_certificate(self.root, archive), certificate)

    def test_a_root_dependency_with_a_version_is_rejected(self):
        # Every spelling of a dependency on the root that carries a `version` key: inline, a bare string, a rename,
        # a subtable, the other dependency tables, a target table and the workspace's own.
        name, _ = self.root_identity()
        for tables in (
            '[dependencies]\n' + name + ' = { path = "..", version = "0.10.0" }\n',
            '[dependencies]\n' + name + ' = "0.10.0"\n',
            '[dependencies]\nplx_app = { package = "' + name + '", path = "..", version = "0.10" }\n',
            '[dependencies.' + name + ']\npath = ".."\nversion = "0.10.0"\n',
            '[dev-dependencies]\n' + name + ' = { path = "..", version = "0.10.0" }\n',
            '[build-dependencies]\n' + name + ' = { path = "..", version = "0.10.0" }\n',
            "[target.'cfg(unix)'.dependencies]\n" + name + ' = { path = "..", version = "0.10.0" }\n',
            "[target.'cfg(unix)'.dev-dependencies]\n" + name + ' = { path = "..", version = "0.10.0" }\n',
        ):
            with self.subTest(tables=tables):
                path = self.root / 'plx_platform_probe'
                if path.exists():
                    shutil.rmtree(path)
                for relative in ('Cargo.toml', 'Cargo.lock'):
                    shutil.copyfile(ROOT / 'rust-modules' / relative, self.root / relative)
                self.add_root_dependent(tables)
                with self.assertRaisesRegex(ValueError, 'root dependency declares a version'):
                    proof._workspace_hashes(self.root, proof._manifest_paths(self.root))
                self.assertFalse(self.valid())

    def test_a_workspace_root_dependency_with_a_version_is_rejected(self):
        name, _ = self.root_identity()
        self.add_root_dependent('[dependencies]\n' + name + ' = { workspace = true }\n')
        manifest = self.root / 'Cargo.toml'
        manifest.write_text(manifest.read_text(encoding='utf-8') + '\n[workspace.dependencies]\n' + name + ' = { path = ".", version = "0.10.0" }\n', encoding='utf-8')
        with self.assertRaisesRegex(ValueError, 'root dependency declares a version'):
            proof._workspace_hashes(self.root, proof._manifest_paths(self.root))
        self.assertFalse(self.valid())

    def test_other_dependencies_and_a_versionless_rename_of_the_root_are_not_the_guard_s_business(self):
        name, _ = self.root_identity()
        self.add_root_dependent('[dependencies]\nplx_app = { package = "' + name + '", path = ".." }\n'
                                'serde = { version = "1" }\n\n[dependencies.other]\npath = "../other"\nversion = "1"\n')
        proof._workspace_hashes(self.root, proof._manifest_paths(self.root))

    def test_local_root_release_does_not_unpin_dependency_version(self):
        self.change_root_release('4.5.6')
        path = self.root / 'Cargo.lock'
        original = path.read_text(encoding='utf-8')
        old = 'name = "serde_json"\nversion = "1.0.154"'
        self.assertIn(old, original)
        path.write_text(original.replace(old, 'name = "serde_json"\nversion = "1.0.155"'), encoding='utf-8')
        self.assertFalse(self.valid())


def closure_sources(certificate):
    """Synthetic crate sources that pass the structural review, one per source path."""
    sources = {path: '' for path in proof.SOURCE_PATHS}
    for declaration in certificate['declarations'].values():
        sources[declaration['path']] += ' '.join(declaration['tokens']) + '\n'
    sources['src/value/mod.rs'] += 'use alloc::string::String; use alloc::vec::Vec; pub use crate::map::Map; pub use crate::number::Number;'
    sources['src/number.rs'] += '#[cfg(feature = "arbitrary_precision")] use alloc::string::{String, ToString};'
    sources['src/map.rs'] += '#[cfg(not(feature = "preserve_order"))] use alloc::collections::{btree_map, BTreeMap};'
    sources['src/lib.rs'] = 'extern crate alloc; pub use crate::value::{from_value, to_value, Map, Number, Value};'
    sources['Cargo.toml'] = '[features]\n' + '\n'.join(key + ' = ' + json.dumps(value) for key, value in certificate['features']['definitions'].items())
    return sources


class SourceClosureTests(unittest.TestCase):
    def setUp(self):
        self.sources = closure_sources(proof._load_certificate())

    def test_complete_recursive_source_grammar(self):
        declarations, features = proof._review(self.sources)
        self.assertEqual(len(declarations), 7)
        self.assertIn('arbitrary_precision', features)
        self.assertIn('preserve_order', features)

    def test_value_interior_cell_source_rejected(self):
        self.sources['src/value/mod.rs'] = self.sources['src/value/mod.rs'].replace('Bool ( bool )', 'Bool ( Cell < bool > )')
        with self.assertRaisesRegex(ValueError, 'structural closure'): proof._review(self.sources)

    def test_number_callback_source_rejected(self):
        self.sources['src/number.rs'] = self.sources['src/number.rs'].replace('n : N ,', 'n : N , callback : fn ( ) ,')
        with self.assertRaisesRegex(ValueError, 'structural closure'): proof._review(self.sources)

    def test_arbitrary_precision_interior_cell_source_rejected(self):
        self.sources['src/number.rs'] = self.sources['src/number.rs'].replace('type N = String ;', 'type N = Cell < String > ;')
        with self.assertRaisesRegex(ValueError, 'structural closure'): proof._review(self.sources)

    def test_unresolved_foreign_container_source_rejected(self):
        self.sources['src/map.rs'] = self.sources['src/map.rs'].replace('= BTreeMap < K , V >', '= EvilMap < K , V >')
        with self.assertRaisesRegex(ValueError, 'structural closure'): proof._review(self.sources)

    def test_container_import_identity_source_rejected(self):
        self.sources['src/value/mod.rs'] = self.sources['src/value/mod.rs'].replace('use alloc::vec::Vec;', 'use evil::Vec;')
        with self.assertRaisesRegex(ValueError, 'canonical source import'): proof._review(self.sources)

    def test_unknown_source_feature_rejected(self):
        self.sources['Cargo.toml'] += '\nmutable_values = []\n'
        with self.assertRaisesRegex(ValueError, 'unknown source feature'): proof._review(self.sources)

    def test_crate_root_macro_or_alias_expansion_not_assumed(self):
        self.assertTrue(proof._root_modules_only('pub mod http; mod plex;'))
        self.assertFalse(proof._root_modules_only('inject_authority!(); pub mod plex;'))
        self.assertFalse(proof._root_modules_only('include!("generated.rs");'))
        self.assertFalse(proof._root_modules_only('extern crate evil as serde_json;'))
        self.assertFalse(proof._root_modules_only('pub mod serde_json;'))


if __name__ == '__main__':
    unittest.main()
