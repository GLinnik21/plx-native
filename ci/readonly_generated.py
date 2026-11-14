"""Offline source-backed unit-enum proof; certificate changes require source review.

No Cargo, cached output, generator execution, Copy axiom or type-name exemption.
The certificate pins the entire production pipeline and include namespace, not the
language list. Its reviewed template emits unit variants in every configuration.
Only enum values are certified: LocaleContext/OnceLock and methods are not.
Assumes ordinary Cargo/Rust/std/serde execution, no external build/config/compiler
overrides, output tampering or unsafe-code/compiler unsoundness attestation.
"""
from pathlib import Path
import json
import re
from readonly_dependencies import _local_sha, _sha, _json, _manifest_paths, _configs, _workspace_hashes, WORKSPACE_HASHING

CERTIFICATE_PATH = Path(__file__).with_name('proofs') / 'platform-generated-enums.json'
CERTIFICATE_SHA256 = 'b61be9b470960a80bbb097dfa466fa5fd4229f89ef8f1234bb302edabf9f8396'
SOURCES = ['platform/Cargo.toml', 'platform/src/lib.rs', 'platform/src/i18n/mod.rs',
           'platform/build.rs', 'platform/build_support/catalog.rs', 'build_support/install_identities.rs',
           'machine/src/lib.rs', 'machine/src/machine.rs']
GRAMMAR = {'Preference': {'prefix': ['System'], 'catalogue_unit_variants': True},
           'Language': {'catalogue_unit_variants': True, 'suffix': ['Pseudo']}}


MACHINE_SOURCES = ['571415d64a9885248f852b402153feedd4660acbe03606e4796c59659726049f',
                   '03a6e18f576dcbaff66c3d7405923b155302bfb2996bb373dac115ece055a5bf',
                   'd08b44dbff241962286eb7e1f85897073ba9f2f712a2ba935837f901635369a8',
                   '9b1ffb66c7ba2eb9fdffcb4bcbf0a29cda87a003cce69b16c73567c8bc0a65b0']
MACHINE_LIB_SOURCES = ['105afdbb05fb762e6752cef260b6d928b1d828993350aaa71651f96cee0c3188',
                       'c35d39ae620b5607e7035222ab1023799adaf091d6a4f737a847dd70b26c1b36']


I18N_SOURCES = ['40a13d07a43c772b10587e510a69502684abfa1f582ce355b22fd935a8c46f45',
                '2c978e3aa3a9215a2e96ebaec9dd5ae3cf3ee85cf1afb43a36b5f42f17ff4d52']

def source_hashes(root):
    sources = {p: _local_sha((root / p).read_bytes()) for p in SOURCES}
    # Reviewed variants add NavOp::SelectTabWithChild or PressRead identity/live/active, or rewrite
    # the Canon byte-feed loops; the complete doc-only newtype producer and its u32 invocations
    # are unchanged.
    if sources['machine/src/machine.rs'] not in MACHINE_SOURCES: raise ValueError('unreviewed machine source')
    sources['machine/src/machine.rs'] = MACHINE_SOURCES
    # Owned-present migration changes sibling/test modules, never the canonical machine child.
    if sources['machine/src/lib.rs'] not in MACHINE_LIB_SOURCES: raise ValueError('unreviewed machine namespace')
    sources['machine/src/lib.rs'] = MACHINE_LIB_SOURCES
    # Existing consumer variants differ in clock formatting, not include/enum identity.
    if sources['platform/src/i18n/mod.rs'] not in I18N_SOURCES: raise ValueError('unreviewed include namespace')
    sources['platform/src/i18n/mod.rs'] = I18N_SOURCES
    return sources

def produce_certificate(root):
    """Review all executable sources on regeneration; catalogue additions need no pin."""
    root = Path(root).resolve()
    hashes = _workspace_hashes(root, _manifest_paths(root))
    return {'schema': 'plx.readonly.generated-unit-enums.v1', 'package': 'plx_platform',
            'module': 'platform/src/i18n/mod.rs',
            'include': 'include!(concat!(env!("OUT_DIR"), "/languages.rs"));',
            'sources': source_hashes(root),
            'text_hashing': WORKSPACE_HASHING, 'manifests': hashes['manifests'],
            'configs': _configs(root), 'lock': hashes['lock_sha256'],
            'grammar': GRAMMAR, 'machine_newtype': 'reviewed local newtype macro emits pub struct Name(pub u32); doc attributes only',
            'review': ['build.rs -> catalog::build(locales, OUT_DIR), no cfg-dependent producer',
                       'parse_registry derives variant solely from validated ASCII tag',
                       'discover preserves validated LanguageSpec; compile sorts by row',
                       'languages_rs emits only unit variants; Pseudo cfg_attr only allows dead_code',
                       'string slots use Rust Debug escaping; numeric slots are integers',
                       'install_identities writes only install_identities.rs, never languages.rs',
                       'PR1366: PressRead identity/live/active and owned-present sibling/test modules; canonical machine child and doc-only u32 newtype producer unchanged',
                       'PR1385: Canon byte-feed loops inlined (impl Canon and canon_tests only, FNV-1a byte stream unchanged); newtype macro and its ten u32 invocations unchanged'],
            'assumptions': __doc__.splitlines()}




def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result: raise ValueError('duplicate catalogue key')
        result[key] = value
    return result


def language_variants(text):
    entries = json.loads(text, object_pairs_hook=unique_object)
    if not isinstance(entries, list) or not entries: raise ValueError('language array required')
    variants, tags, rows = [], set(), set()
    required = {'tag', 'native_name', 'region', 'script', 'plurals', 'row'}
    last = 0
    for item in entries:
        if not isinstance(item, dict) or not required <= set(item) or set(item) - required - {'excluded_regions'}:
            raise ValueError('language schema')
        tag = item['tag']
        if not isinstance(tag, str) or not re.fullmatch('[a-z]{2,3}', tag) or tag in {'system', 'qps', 'und'} | tags:
            raise ValueError('language tag')
        variant = tag[0].upper() + tag[1:]
        if not re.fullmatch('[A-Z][a-z]{1,2}', variant): raise ValueError('variant identifier')
        native = item['native_name']
        if not isinstance(native, str) or not native.strip() or any(ord(c) < 32 or 127 <= ord(c) <= 159 for c in native):
            raise ValueError('native name')
        if not isinstance(item['region'], str) or not re.fullmatch(re.escape(tag) + '-[A-Z]{2}', item['region']):
            raise ValueError('region')
        if item['script'] not in ('Latn', 'Cyrl'): raise ValueError('script')
        plurals = item['plurals']
        if not isinstance(plurals, list) or any(not isinstance(p, str) for p in plurals) or len(set(plurals)) != len(plurals) or 'other' not in plurals or set(plurals) - {'zero', 'one', 'two', 'few', 'many', 'other'}:
            raise ValueError('plurals')
        excluded = item.get('excluded_regions', [])
        if not isinstance(excluded, list) or any(not isinstance(r, str) or not re.fullmatch('[A-Z]{2}', r) for r in excluded) or len(set(excluded)) != len(excluded):
            raise ValueError('excluded regions')
        row = item['row']
        if type(row) is not int or not last < row < 1000 or row in rows: raise ValueError('row')
        tags.add(tag); rows.add(row); variants.append(variant); last = row
    if 'en' not in tags: raise ValueError('English fallback required')
    return variants


def provenance(rust_modules_root, source_file):
    """Return evidence only at the exact reviewed include, never by enum name."""
    try:
        root = Path(rust_modules_root).resolve()
        path = Path(source_file)
        if not path.is_absolute(): path = root / path
        if path.resolve() not in (root / 'platform/src/i18n/mod.rs', root / 'machine/src/machine.rs') or path.is_symlink(): return None
        cert = json.loads(CERTIFICATE_PATH.read_text(encoding='utf-8'))
        if _sha(_json(cert)) != CERTIFICATE_SHA256 or cert != produce_certificate(root): return None
        for relative in SOURCES:
            p = root / relative
            if p.is_symlink() or not p.resolve().is_relative_to(root): return None
        if cert['include'] not in (root / cert['module']).read_text(encoding='utf-8'): return None
        catalogue = root.parent / 'locales/languages.json'
        if catalogue.is_symlink() or not catalogue.resolve().is_relative_to(root.parent): return None
        variants = language_variants(catalogue.read_text(encoding='utf-8'))
        return {'certificate_sha256': CERTIFICATE_SHA256, 'sources': cert['sources'], 'grammar': GRAMMAR,
                'variants': {'Preference': ['System'] + variants, 'Language': variants + ['Pseudo']}}
    except (OSError, ValueError, KeyError, TypeError, IndexError, UnicodeError, RecursionError):
        return None




def canonical_child(root, parent, name, child):
    """An include's OUT_DIR belongs to its crate, not a foreign #[path] importer."""
    root = Path(root).resolve()
    bindings = {root / 'platform/src/i18n/mod.rs': (root / 'platform/src/lib.rs', 'i18n'),
                root / 'machine/src/machine.rs': (root / 'machine/src/lib.rs', 'machine')}
    expected = bindings.get(Path(child).resolve())
    return expected is None or expected == (Path(parent).resolve(), name)

def declarations(root, path):
    evidence = provenance(root, path)
    if evidence is None: return ''
    if Path(path).resolve() == Path(root).resolve() / 'machine/src/machine.rs':
        from rust_test_modules import lex
        tokens = lex(Path(path).read_text(encoding='utf-8'))
        names = []
        for i in range(len(tokens) - 4):
            if tokens[i:i + 3] != [('code', 'newtype'), ('code', '!'), ('code', '(')]: continue
            j = i + 3
            while tokens[j:j + 2] == [('code', '#'), ('code', '[')]:
                # Only documentation meta is part of this reviewed invocation grammar.
                if tokens[j + 2:j + 4] != [('code', 'doc'), ('code', '=')] or tokens[j + 4][0] != 'string' or tokens[j + 5] != ('code', ']'): return ''
                j += 6
            if tokens[j][0] != 'code' or not re.fullmatch('[A-Za-z_][A-Za-z_0-9]*', tokens[j][1]) or tokens[j + 1:j + 3] != [('code', ')'), ('code', ';')]: return ''
            names.append(tokens[j][1])
        return '\n'.join('#[derive(Clone, Copy)] pub struct ' + name + '(pub u32);' for name in names)
    return '\n'.join('#[derive(Clone, Copy)] pub enum ' + name + ' { ' + ', '.join(variants) + ' }'
                     for name, variants in evidence['variants'].items())
