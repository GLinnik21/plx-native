"""Source-backed generated data adversaries, native/offline with no Cargo cache."""
from pathlib import Path
import importlib.util
import json
import shutil
import tempfile
import unittest
import readonly_generated as generated
import readonly_dependencies as dependency
from machine_readonly import Proof, imports

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('generated_gate', ROOT / 'ci/check-machine-shape.py')
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class GeneratedProofTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='generated-data-proof-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / 'rust-modules'
        certificate = json.loads(generated.CERTIFICATE_PATH.read_text(encoding='utf-8'))
        paths = set(certificate['manifests']) | set(certificate['sources']) | {'Cargo.lock'}
        paths |= {p for p, value in certificate['configs'].items() if value is not None}
        paths.add('../locales/languages.json')
        for relative in paths:
            out = self.root / relative
            out.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / 'rust-modules' / relative, out)
        self.module = self.root / 'os/src/i18n/mod.rs'
        self.catalogue = self.root.parent / 'locales/languages.json'

    def evidence(self):
        return generated.provenance(self.root, self.module)

    def test_actual_pipeline_and_closed_layout(self):
        evidence = self.evidence()
        self.assertIsNotNone(evidence)
        self.assertEqual(evidence['variants']['Preference'][0], 'System')
        self.assertEqual(evidence['variants']['Language'][-1], 'Pseudo')
        proof = Proof(self.root, gate)
        for name in ('Preference', 'Language'):
            self.assertTrue(proof.inert(self.module, [name]))
            self.assertTrue(proof.copy_value(self.module, [name]))
        for name in ('LocaleContext', 'OnceLock'):
            self.assertFalse(proof.inert(self.module, [name]))

    def test_machine_macro_source_layout(self):
        module = self.root / 'machine/src/machine.rs'
        proof = Proof(self.root, gate)
        for name in ('StoreOrd', 'RequestId', 'InstanceId', 'MachineId'):
            self.assertTrue(proof.inert(module, [name]), name)
        original = module.read_text(encoding='utf-8')
        module.write_text(original.replace('pub u32', 'pub std::cell::Cell<u32>', 1), encoding='utf-8')
        self.assertFalse(Proof(self.root, gate).inert(module, ['StoreOrd']))

    def test_machine_producer_and_namespace_mutations_fail_closed(self):
        module = self.root / 'machine/src/machine.rs'
        mutations = [
            ('machine/src/lib.rs', 'pub mod machine;', '#[path="other.rs"] pub mod machine;'),
            ('machine/src/machine.rs', 'pub struct AppCommandId(pub u64);', 'pub struct AppCommandId(pub std::cell::Cell<u64>);'),
            ('machine/src/machine.rs', 'pub struct $name(pub u32);', 'pub struct $name(pub std::cell::Cell<u32>);'),
            ('machine/src/machine.rs', '$(#[$m])*', '#[cfg_attr(feature="rogue", payload)] $(#[$m])*'),
            ('machine/src/machine.rs', 'StoreOrd\n', 'StoreOrd, Callback\n'),
            ('machine/src/machine.rs', 'StoreOrd\n', '#[cfg_attr(feature="rogue", payload)] StoreOrd\n'),
        ]
        for relative, old, new in mutations:
            with self.subTest(path=relative, mutation=new):
                path = self.root / relative
                original = path.read_text(encoding='utf-8')
                self.assertIn(old, original)
                path.write_text(original.replace(old, new, 1), encoding='utf-8')
                self.assertIsNone(generated.provenance(self.root, module))
                self.assertFalse(Proof(self.root, gate).inert(module, ['StoreOrd']))
                path.write_text(original, encoding='utf-8')
        self.assertIsNotNone(generated.provenance(self.root, module))
        self.assertTrue(Proof(self.root, gate).inert(module, ['StoreOrd']))

    def test_reviewed_runtime_contracts_are_not_generated_data(self):
        module = self.root / 'machine/src/machine.rs'
        self.assertIsNotNone(generated.provenance(self.root, module))
        proof = Proof(self.root, gate)
        self.assertFalse(proof.inert(module, ['PresentationAuthority']))
        self.assertFalse(proof.copy_value(module, ['PresentationReceipt', '<', 'u32', '>']))
        self.assertFalse(proof.inert(module, ['PresentationReceipt', '<', 'std', ':', ':', 'cell', ':', ':', 'Cell', '<', 'u32', '>', '>']))
        declarations = generated.declarations(self.root, module)
        self.assertEqual(declarations.count('pub struct '), 10)
        self.assertNotIn('Presentation', declarations)
        self.assertNotIn('AppEffect', declarations)

    def test_certificate_payload_and_manifest_tampering_fail_closed(self):
        from unittest.mock import patch
        certificate = json.loads(generated.CERTIFICATE_PATH.read_text(encoding='utf-8'))
        forged = self.root.parent / 'forged-certificate.json'
        for key, value in (('machine_newtype', 'any Copy type'), ('grammar', {})):
            with self.subTest(certificate_key=key):
                changed = dict(certificate, **{key: value})
                forged.write_text(json.dumps(changed), encoding='utf-8')
                with patch.object(generated, 'CERTIFICATE_PATH', forged):
                    self.assertIsNone(self.evidence())
        for relative in ('machine/Cargo.toml', 'os/Cargo.toml', 'Cargo.lock'):
            with self.subTest(path=relative):
                path = self.root / relative
                original = path.read_bytes()
                path.write_bytes(original + b'\n# unreviewed whole-file change\n')
                self.assertIsNone(self.evidence())
                path.write_bytes(original)
        self.assertIsNotNone(self.evidence())

    def test_additional_language_remains_data_only(self):
        entries = json.loads(self.catalogue.read_text(encoding='utf-8'))
        entries.append(dict(tag='sv', native_name='Svenska', region='sv-SE', script='Latn', plurals=['one', 'other'], row=entries[-1]['row'] + 1))
        self.catalogue.write_text(json.dumps(entries), encoding='utf-8')
        self.assertIn('Sv', self.evidence()['variants']['Preference'])
        entries[-1]['native_name'] = 'text "); Cell<Callback> { // escaped data'
        self.catalogue.write_text(json.dumps(entries), encoding='utf-8')
        self.assertIsNotNone(self.evidence())  # Rust Debug-escaped string, never a source slot.

    def test_wrong_module_and_same_name_are_not_certified(self):
        wrong = self.root / 'os/src/wrong.rs'
        wrong.write_text('pub enum Preference { Payload(std::cell::Cell<u32>) }', encoding='utf-8')
        self.assertIsNone(generated.provenance(self.root, wrong))
        self.assertFalse(Proof(self.root, gate).inert(wrong, ['Preference']))
        consumer = self.root / 'session/src/shadow.rs'
        consumer.parent.mkdir(parents=True, exist_ok=True)
        consumer.write_text('#[path="../../os/src/i18n/mod.rs"] mod language;', encoding='utf-8')
        self.assertFalse(Proof(self.root, gate).inert(consumer, ['language', ':', ':', 'Preference']))

    def test_all_producer_namespace_and_feature_mutations_fail_closed(self):
        mutations = [
            ('os/src/i18n/mod.rs', '/languages.rs', '/other.rs'),
            ('os/src/i18n/mod.rs', 'include!', '#[cfg(feature="rogue")] include!'),
            ('os/src/i18n/mod.rs', 'include!', 'pub enum Preference { Payload(std::cell::Cell<u32>) } include!'),
            ('os/src/lib.rs', 'pub mod i18n;', '#[path="other.rs"] pub mod i18n;'),
            ('os/build.rs', 'catalog::build', 'other::build'),
            ('os/build_support/catalog.rs', 'System,\\n', 'System(std::cell::Cell<u32>),\\n'),
            ('os/build_support/catalog.rs', 's.variant', 'callback()'),
            ('os/build_support/catalog.rs', 'write("languages.rs"', 'write("rogue.rs"'),
            ('build_support/install_identities.rs', 'pub fn emit', '#[cfg(feature="rogue")] pub fn emit'),
            ('os/Cargo.toml', '[package]', '[features]\nrogue = []\n[package]'),
        ]
        for relative, old, new in mutations:
            with self.subTest(path=relative, mutation=new):
                path = self.root / relative
                original = path.read_text(encoding='utf-8')
                self.assertIn(old, original)
                path.write_text(original.replace(old, new, 1), encoding='utf-8')
                self.assertIsNone(self.evidence())
                path.write_text(original, encoding='utf-8')
        self.assertIsNotNone(self.evidence())

    def test_catalogue_injection_schema_and_identifiers_fail_closed(self):
        original = self.catalogue.read_text(encoding='utf-8')
        cases = [('tag', 'en), Payload(Cell<u32>), Evil('), ('tag', 'r#en'), ('tag', 'qps'),
                 ('tag', 'EN'), ('variant', 'Cell'), ('row', True), ('row', 1000),
                 ('script', 'Latn); callback()'), ('plurals', ['other', 'other']),
                 ('excluded_regions', ['BR', 'BR']), ('native_name', '\x00')]
        for key, value in cases:
            with self.subTest(key=key, value=value):
                entries = json.loads(original)
                entries[0][key] = value
                self.catalogue.write_text(json.dumps(entries), encoding='utf-8')
                self.assertIsNone(self.evidence())
        self.catalogue.write_text('[{"tag":"en","tag":"es"}]', encoding='utf-8')
        self.assertIsNone(self.evidence())

    def test_conditional_field_attribute_is_opaque(self):
        self.module = self.root / 'os/src/rogue.rs'
        self.module.write_text('enum Holder { Value(#[cfg_attr(feature="rogue", payload)] u32) }', encoding='utf-8')
        self.assertFalse(Proof(self.root, gate).inert(self.module, ['Holder']))

    def test_grouped_self_import_binds_module_not_self(self):
        self.assertEqual(imports(['plx_plex', ':', ':', 'probe', ':', ':', '{', 'self', '}']), [('probe', ('plx_plex', 'probe'))])

    def test_consumer_namespace_shadow_is_not_canonical(self):
        path = self.root / 'session/src/shadow.rs'
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text('mod plx_os { pub mod i18n { pub enum Preference { Payload(std::cell::Cell<u32>) } } }', encoding='utf-8')
        self.assertFalse(Proof(self.root, gate).inert(path, ['plx_os', ':', ':', 'i18n', ':', ':', 'Preference']))
        self.assertEqual(imports(['plx_plex', ':', ':', 'probe', ':', ':', '{', 'self', '}']), [('probe', ('plx_plex', 'probe'))])

    def change_root_release(self, version, manifest=True, lock=True):
        path = self.root / 'Cargo.toml'
        text = path.read_text(encoding='utf-8')
        package = dependency._table(dependency._sections(text)['package'])
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
                self.assertTrue(self.evidence() is not None)

    def test_local_root_release_must_match_lock(self):
        self.change_root_release('4.5.6', lock=False)
        self.assertFalse(self.evidence() is not None)

    def test_local_root_lock_must_match_manifest(self):
        self.change_root_release('4.5.6', manifest=False)
        self.assertFalse(self.evidence() is not None)

    def test_local_root_release_is_not_dependency_or_source_approval(self):
        self.change_root_release('4.5.6')
        path = self.root / 'Cargo.lock'
        original = path.read_text(encoding='utf-8')
        package = dependency._table(dependency._sections((self.root / 'Cargo.toml').read_text(encoding='utf-8'))['package'])
        entry = 'name = "' + package['name'] + '"\nversion = "4.5.6"'
        for extra in ('\nsource = "git+https://example.org/evil"', '\nchecksum = "' + '0' * 64 + '"'):
            with self.subTest(extra=extra):
                path.write_text(original.replace(entry, entry + extra), encoding='utf-8')
                self.assertFalse(self.evidence() is not None)
        path.write_text(original, encoding='utf-8')
        path = self.root / 'Cargo.toml'
        path.write_text(path.read_text(encoding='utf-8') + '\n# unreviewed metadata\n', encoding='utf-8')
        self.assertFalse(self.evidence() is not None)

    def test_local_root_prerelease_and_noncanonical_versions_fail_closed(self):
        originals = {p: (self.root / p).read_bytes() for p in ('Cargo.toml', 'Cargo.lock')}
        for version in ('01.2.3', '1.2.3-rogue', '1.2.3+rogue', '1.2'):
            with self.subTest(version=version):
                for p, data in originals.items():
                    (self.root / p).write_bytes(data)
                self.change_root_release(version)
                self.assertFalse(self.evidence() is not None)

    def test_bare_carriage_return_is_not_checkout_normalization(self):
        for relative in ('Cargo.toml', 'Cargo.lock'):
            path = self.root / relative
            original = path.read_bytes()
            path.write_bytes(original.replace(b'\r\n', b'\n').replace(b'\n', b'\r'))
            self.assertFalse(self.evidence() is not None)
            path.write_bytes(original)

    def test_local_root_duplicate_lock_identity_fails_closed(self):
        self.change_root_release('4.5.6')
        name = dependency._table(dependency._sections((self.root / 'Cargo.toml').read_text(encoding='utf-8'))['package'])['name']
        path = self.root / 'Cargo.lock'
        path.write_text(path.read_text(encoding='utf-8') + '\n[[package]]\nname = "' + name + '"\nversion = "4.5.6"\n', encoding='utf-8')
        with self.assertRaisesRegex(ValueError, 'ambiguous/nonlocal'):
            dependency._workspace_hashes(self.root, dependency._manifest_paths(self.root))
        self.assertFalse(self.evidence() is not None)

    def root_identity(self):
        package = dependency._table(dependency._sections((self.root / 'Cargo.toml').read_text(encoding='utf-8'))['package'])
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
                    dependency._workspace_hashes(self.root, dependency._manifest_paths(self.root))
                self.assertFalse(self.evidence() is not None)

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

    def test_a_bare_name_path_dependent_of_the_root_does_not_make_its_release_resolve(self):
        # With a platform-style crate depending on the root by path, the root's release number still moves nothing the
        # certificate pins: the masked manifest hash and the masked lock hash are unchanged, and the producer still runs.
        self.add_root_dependent()
        paths = dependency._manifest_paths(self.root)
        self.assertIn('plx_platform_probe/Cargo.toml', paths)
        before = dependency._workspace_hashes(self.root, paths)
        certificate = generated.produce_certificate(self.root)
        self.assertIn('plx_platform_probe/Cargo.toml', certificate['manifests'])
        for version in ('0.9.0', '4.5.6'):
            with self.subTest(version=version):
                self.change_root_release(version)
                after = dependency._workspace_hashes(self.root, paths)
                self.assertEqual(after['manifests']['Cargo.toml'], before['manifests']['Cargo.toml'])
                self.assertEqual(after['lock_sha256'], before['lock_sha256'])
                self.assertEqual(after, before)
                self.assertEqual(generated.produce_certificate(self.root), certificate)
                self.assertEqual(certificate['lock'], before['lock_sha256'])

    def test_a_root_dependency_with_a_version_is_rejected(self):
        # Every spelling of a dependency on the root that carries a `version` key: inline, a bare string, a rename,
        # a subtable, the other dependency tables and a target table.
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
                    dependency._workspace_hashes(self.root, dependency._manifest_paths(self.root))
                with self.assertRaisesRegex(ValueError, 'root dependency declares a version'):
                    generated.produce_certificate(self.root)
                self.assertFalse(self.evidence() is not None)

    def test_a_workspace_root_dependency_with_a_version_is_rejected(self):
        name, _ = self.root_identity()
        self.add_root_dependent('[dependencies]\n' + name + ' = { workspace = true }\n')
        manifest = self.root / 'Cargo.toml'
        manifest.write_text(manifest.read_text(encoding='utf-8') + '\n[workspace.dependencies]\n' + name + ' = { path = ".", version = "0.10.0" }\n', encoding='utf-8')
        with self.assertRaisesRegex(ValueError, 'root dependency declares a version'):
            dependency._workspace_hashes(self.root, dependency._manifest_paths(self.root))
        self.assertFalse(self.evidence() is not None)

    def test_other_dependencies_and_a_versionless_rename_of_the_root_are_not_the_guard_s_business(self):
        name, _ = self.root_identity()
        self.add_root_dependent('[dependencies]\nplx_app = { package = "' + name + '", path = ".." }\n'
                                'serde = { version = "1" }\n\n[dependencies.other]\npath = "../other"\nversion = "1"\n')
        dependency._workspace_hashes(self.root, dependency._manifest_paths(self.root))

    def test_local_root_release_does_not_unpin_dependency_version(self):
        self.change_root_release('4.5.6')
        path = self.root / 'Cargo.lock'
        original = path.read_text(encoding='utf-8')
        old = 'name = "serde_json"\nversion = "1.0.154"'
        self.assertIn(old, original)
        path.write_text(original.replace(old, 'name = "serde_json"\nversion = "1.0.155"'), encoding='utf-8')
        self.assertFalse(self.evidence() is not None)

    def test_lf_and_crlf_producer_inputs_keep_identity(self):
        certificate = json.loads(generated.CERTIFICATE_PATH.read_text(encoding='utf-8'))
        paths = set(certificate['sources']) | set(certificate['manifests']) | {'Cargo.lock'}
        paths |= {p for p, digest in certificate['configs'].items() if digest is not None}
        for ending in (b'\n', b'\r\n'):
            for relative in paths:
                path = self.root / relative
                path.write_bytes(path.read_bytes().replace(b'\r\n', b'\n').replace(b'\n', ending))
            self.assertIsNotNone(self.evidence())


if __name__ == '__main__':
    unittest.main()
