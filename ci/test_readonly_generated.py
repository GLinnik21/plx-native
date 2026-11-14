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
        self.module = self.root / 'platform/src/i18n/mod.rs'
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

    def test_additional_language_remains_data_only(self):
        entries = json.loads(self.catalogue.read_text(encoding='utf-8'))
        entries.append(dict(tag='sv', native_name='Svenska', region='sv-SE', script='Latn', plurals=['one', 'other'], row=entries[-1]['row'] + 1))
        self.catalogue.write_text(json.dumps(entries), encoding='utf-8')
        self.assertIn('Sv', self.evidence()['variants']['Preference'])
        entries[-1]['native_name'] = 'text "); Cell<Callback> { // escaped data'
        self.catalogue.write_text(json.dumps(entries), encoding='utf-8')
        self.assertIsNotNone(self.evidence())  # Rust Debug-escaped string, never a source slot.

    def test_wrong_module_and_same_name_are_not_certified(self):
        wrong = self.root / 'platform/src/wrong.rs'
        wrong.write_text('pub enum Preference { Payload(std::cell::Cell<u32>) }', encoding='utf-8')
        self.assertIsNone(generated.provenance(self.root, wrong))
        self.assertFalse(Proof(self.root, gate).inert(wrong, ['Preference']))
        consumer = self.root / 'session/src/shadow.rs'
        consumer.parent.mkdir(parents=True, exist_ok=True)
        consumer.write_text('#[path="../../platform/src/i18n/mod.rs"] mod language;', encoding='utf-8')
        self.assertFalse(Proof(self.root, gate).inert(consumer, ['language', ':', ':', 'Preference']))

    def test_all_producer_namespace_and_feature_mutations_fail_closed(self):
        mutations = [
            ('platform/src/i18n/mod.rs', '/languages.rs', '/other.rs'),
            ('platform/src/i18n/mod.rs', 'include!', '#[cfg(feature="rogue")] include!'),
            ('platform/src/i18n/mod.rs', 'include!', 'pub enum Preference { Payload(std::cell::Cell<u32>) } include!'),
            ('platform/src/lib.rs', 'pub mod i18n;', '#[path="other.rs"] pub mod i18n;'),
            ('platform/build.rs', 'catalog::build', 'other::build'),
            ('platform/build_support/catalog.rs', 'System,\\n', 'System(std::cell::Cell<u32>),\\n'),
            ('platform/build_support/catalog.rs', 's.variant', 'callback()'),
            ('platform/build_support/catalog.rs', 'write("languages.rs"', 'write("rogue.rs"'),
            ('build_support/install_identities.rs', 'pub fn emit', '#[cfg(feature="rogue")] pub fn emit'),
            ('platform/Cargo.toml', '[package]', '[features]\nrogue = []\n[package]'),
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
        self.module = self.root / 'platform/src/rogue.rs'
        self.module.write_text('enum Holder { Value(#[cfg_attr(feature="rogue", payload)] u32) }', encoding='utf-8')
        self.assertFalse(Proof(self.root, gate).inert(self.module, ['Holder']))

    def test_grouped_self_import_binds_module_not_self(self):
        self.assertEqual(imports(['plx_plex', ':', ':', 'probe', ':', ':', '{', 'self', '}']), [('probe', ('plx_plex', 'probe'))])

    def test_consumer_namespace_shadow_is_not_canonical(self):
        path = self.root / 'session/src/shadow.rs'
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text('mod plx_platform { pub mod i18n { pub enum Preference { Payload(std::cell::Cell<u32>) } } }', encoding='utf-8')
        self.assertFalse(Proof(self.root, gate).inert(path, ['plx_platform', ':', ':', 'i18n', ':', ':', 'Preference']))
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

    def test_local_root_version_cannot_participate_in_resolution(self):
        name = dependency._table(dependency._sections((self.root / 'Cargo.toml').read_text(encoding='utf-8'))['package'])['name']
        path = self.root / 'Cargo.lock'
        text = path.read_text(encoding='utf-8')
        path.write_text(text.replace('dependencies = [', 'dependencies = [\n "' + name + '",', 1), encoding='utf-8')
        with self.assertRaisesRegex(ValueError, 'participates in dependency resolution'):
            dependency._workspace_hashes(self.root, dependency._manifest_paths(self.root))
        self.assertFalse(self.evidence() is not None)

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
