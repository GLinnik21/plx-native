"""Reviewed locked-source certificates, not an external-type name allowlist.

Runtime is Python 3.10 stdlib-only, offline, and executes no Cargo or crate code.
The producer reads a SHA256-verified crate archive as data BEFORE inspecting it.
The pinned certificate is a review boundary: regeneration also requires reviewing
its source/grammar and updating CERTIFICATE_SHA256 here. Unknown types stay opaque.

Assumptions match the local structural checker: sound Rust/std owned containers,
Cargo honoring the checked-in locked graph/config, and no out-of-tree compiler or
Cargo overrides. This cannot attest an arbitrary build environment or unsafe code.
"""
from functools import lru_cache
from pathlib import Path
import argparse
import hashlib
import json
import re
import tarfile

CERTIFICATE_PATH = Path(__file__).with_name('proofs') / 'serde-json-immutable.json'
CERTIFICATE_SHA256 = '7a299a63a440720a7f7313fe985519efb77b6f6a2963b5b0d92fc3bc3be987be'
SCHEMA = 'plx.readonly.locked-json.v1'
REGISTRY = 'registry+https://github.com/rust-lang/crates.io-index'
SUPPORTED = ['alloc', 'arbitrary_precision', 'default', 'float_roundtrip', 'raw_value', 'std', 'unbounded_depth']
GRAMMAR = {
    'Value': {'enum': {'Null': [], 'Bool': ['bool'], 'Number': ['Number'],
                       'String': ['alloc::string::String'], 'Array': ['alloc::vec::Vec<Value>'],
                       'Object': ['Map<alloc::string::String,Value>']}},
    'Number': {'fields': {'n': 'N'}},
    'N': {'not(arbitrary_precision)': {'enum': {'PosInt': ['u64'], 'NegInt': ['i64'], 'Float': ['f64']}},
          'arbitrary_precision': 'alloc::string::String'},
    'Map<K,V>': {'fields': {'map': 'MapImpl<K,V>'}},
    'MapImpl<K,V>': {'not(preserve_order)': 'alloc::collections::BTreeMap<K,V>',
                     'preserve_order': {'unsupported': 'indexmap::IndexMap<K,V,BuildHasherImpl>'}},
    'axioms': ['bool', 'u64', 'i64', 'f64', 'alloc::string::String',
               'alloc::vec::Vec<inert>', 'alloc::collections::BTreeMap<inert,inert>'],
    'closure': 'least finite owned JSON tree; all recursive edges cross owned containers',
}
SOURCE_PATHS = ['Cargo.toml', 'src/lib.rs', 'src/value/mod.rs', 'src/number.rs', 'src/map.rs', 'build.rs']


def _sha(data):
    return hashlib.sha256(data).hexdigest()


def _local_sha(data):
    # Git checkouts may use CRLF on Windows and LF in CI; no other byte is ignored.
    return _sha(data.replace(b'\r\n', b'\n'))


def _json(data):
    return json.dumps(data, sort_keys=True, separators=(',', ':'), ensure_ascii=True).encode('ascii')


_RAW_STRING = re.compile(r'(?:b|c)?r(#+)?"')
_LITERAL = re.compile(r'(?:b|c)?"(?:\\.|[^"\\])*"|(?:b)?\'(?:\\.|[^\'\\])\'')
_TOKEN = re.compile(r'r#[A-Za-z_]\w*|[A-Za-z_]\w*|::|[^\s]')


def _tokens(text):
    """Lex comments/literals before examining Rust identifiers; never expand macros.

    Each pattern matches AT the cursor (`pattern.match(text, i)`): slicing `text[i:]` per token
    copied the rest of the file every time and made a 700 KB source cost seconds.
    """
    result, i = [], 0
    while i < len(text):
        if text[i].isspace():
            i += 1
        elif text.startswith('//', i):
            stop = text.find('\n', i); i = len(text) if stop < 0 else stop
        elif text.startswith('/*', i):
            depth, i = 1, i + 2
            while depth:
                if i >= len(text): raise ValueError('unclosed Rust comment')
                if text.startswith('/*', i): depth += 1; i += 2
                elif text.startswith('*/', i): depth -= 1; i += 2
                else: i += 1
        else:
            raw = _RAW_STRING.match(text, i)
            if raw:
                end = '"' + (raw[1] or '')
                stop = text.find(end, raw.end())
                if stop < 0: raise ValueError('unclosed raw string')
                result.append('"<raw>"'); i = stop + len(end); continue
            literal = _LITERAL.match(text, i)
            if literal:
                result.append(literal[0]); i = literal.end(); continue
            token = _TOKEN.match(text, i)
            result.append(token[0].removeprefix('r#')); i = token.end()
    return result


def _table(text):
    """Bounded TOML key/value tables used by Cargo dependency/feature metadata.

    Unsupported TOML (multiline strings, dotted assignments, quoted keys) fails
    closed; this is not a replacement for a general TOML parser.
    """
    clean, quote, escaped = [], None, False
    i = 0
    while i < len(text):
        c = text[i]
        if quote:
            clean.append(c)
            if c == quote and not escaped: quote = None
            escaped = c == '\\' and not escaped
        elif c in ('"', "'"):
            if text[i:i + 3] == c * 3: raise ValueError('multiline TOML string unsupported')
            quote = c; clean.append(c)
        elif c == '#':
            end = text.find('\n', i); i = len(text) if end < 0 else end; continue
        else: clean.append(c)
        i += 1
    if quote: raise ValueError('unclosed TOML string')
    tokens = re.findall(r'"(?:\\.|[^"\\])*"|\'[^\']*\'|[A-Za-z0-9_+.-]+|[^\s]', ''.join(clean))
    pos = 0
    def value():
        nonlocal pos
        if pos >= len(tokens): raise ValueError('missing TOML value')
        t = tokens[pos]; pos += 1
        if t == '[':
            items = []
            while pos < len(tokens) and tokens[pos] != ']':
                items.append(value())
                if tokens[pos] != ',': break
                pos += 1
            if pos >= len(tokens) or tokens[pos] != ']': raise ValueError('unsupported TOML array')
            pos += 1; return items
        if t == '{':
            items = {}
            while pos < len(tokens) and tokens[pos] != '}':
                key = tokens[pos]; pos += 1
                if not re.fullmatch(r'[A-Za-z0-9_-]+', key) or key in items or tokens[pos] != '=':
                    raise ValueError('unsupported inline TOML table')
                pos += 1; items[key] = value()
                if tokens[pos] != ',': break
                pos += 1
            if pos >= len(tokens) or tokens[pos] != '}': raise ValueError('unsupported inline TOML table')
            pos += 1; return items
        if t.startswith('"'): return json.loads(t)
        if t.startswith("'"): return t[1:-1]
        if t in ('true', 'false'): return t == 'true'
        if re.fullmatch(r'[0-9]+', t): return int(t)
        raise ValueError('unsupported TOML value: ' + t)
    result = {}
    while pos < len(tokens):
        key = tokens[pos]; pos += 1
        if not re.fullmatch(r'[A-Za-z0-9_-]+', key) or key in result or tokens[pos] != '=':
            raise ValueError('unsupported TOML assignment')
        pos += 1; result[key] = value()
    return result


def _sections(text):
    matches = list(re.finditer(r'(?m)^\[(\[?)([^\]\n]+)\]\]?[ \t]*(?:#[^\n]*)?$', text))
    result = {}
    for i, match in enumerate(matches):
        if match[1]: continue
        name = match[2]
        if name in result: raise ValueError('duplicate TOML section')
        result[name] = text[match.end():matches[i + 1].start() if i + 1 < len(matches) else len(text)]
    return result


def _locked(root):
    text = (root / 'Cargo.lock').read_text(encoding='utf-8')
    if _table(text.split('[[package]]')[0]).get('version') not in (3, 4):
        raise ValueError('unsupported Cargo.lock format')
    packages = [_table(part) for part in text.split('[[package]]')[1:]]
    matches = [p for p in packages if p.get('name') == 'serde_json']
    if len(matches) != 1: raise ValueError('ambiguous locked serde_json package')
    package = matches[0]
    if package.get('source') != REGISTRY or not re.fullmatch('[0-9a-f]{64}', package.get('checksum', '')):
        raise ValueError('not the canonical crates.io archive identity')
    if any(x.split(' ')[0] in ('indexmap', 'foldhash') for x in package.get('dependencies', [])):
        raise ValueError('preserve_order dependency branch is not certified')
    return {k: package[k] for k in ('name', 'version', 'source', 'checksum')}, package.get('dependencies', [])


def _manifest_paths(root):
    root_sections = _sections((root / 'Cargo.toml').read_text(encoding='utf-8'))
    workspace = _table(root_sections['workspace'])
    members = workspace['members']
    if not isinstance(members, list) or any(not isinstance(m, str) or not re.fullmatch(r'[A-Za-z0-9_-]+', m) for m in members):
        raise ValueError('unsupported workspace member pattern')
    return ['Cargo.toml'] + [m + '/Cargo.toml' for m in sorted(set(members))]


WORKSPACE_HASHING = 'sha256-utf8-bytes-crlf-to-lf-local-root-version-v1'


def _mask_local_version(text, section, version):
    matches = list(re.finditer(r'(?m)^version[ \t]*=[ \t]*"([^"\n]+)"[ \t]*(?:#[^\n]*)?$', section))
    if len(matches) != 1 or matches[0][1] != version or text.count(section) != 1:
        raise ValueError('unsupported local root version assignment')
    match = matches[0]
    start = text.index(section) + match.start(1)
    end = start + len(version)
    return text[:start] + '<local-root-version>' + text[end:]


def _workspace_hashes(root, paths):
    """Pin all bytes except the root app's coherent, non-resolving release number.

    The app build.rs reports this number; it does not select the locked JSON
    source or the platform enum producer. Member/dependency versions, features,
    source identities, comments and all other metadata remain byte-pinned.
    """
    manifest = (root / 'Cargo.toml').read_bytes().replace(b'\r\n', b'\n').decode('utf-8')
    section = _sections(manifest)['package']
    package = _table(section)
    name, version = package['name'], package['version']
    if not isinstance(name, str) or not isinstance(version, str) or not re.fullmatch(r'(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)', version):
        raise ValueError('unsupported local root package version')
    lock = (root / 'Cargo.lock').read_bytes().replace(b'\r\n', b'\n').decode('utf-8')
    entries = [(body, _table(body)) for body in lock.split('[[package]]')[1:]]
    local = [(body, entry) for body, entry in entries if entry.get('name') == name]
    if len(local) != 1 or local[0][1].get('version') != version or {'source', 'checksum'} & set(local[0][1]):
        raise ValueError('ambiguous/nonlocal root lock identity or version mismatch')
    if any(dep.split(' ')[0] == name for _, entry in entries for dep in entry.get('dependencies', [])):
        raise ValueError('root version participates in dependency resolution')
    manifests = {p: _local_sha((root / p).read_bytes()) for p in paths}
    manifests['Cargo.toml'] = _sha(_mask_local_version(manifest, section, version).encode('utf-8'))
    return {'manifests': manifests,
            'lock_sha256': _sha(_mask_local_version(lock, local[0][0], version).encode('utf-8'))}


def _configs(root):
    return {name: _local_sha((root / name).read_bytes()) if (root / name).exists() else None
            for name in ('.cargo/config', '.cargo/config.toml', '../.cargo/config', '../.cargo/config.toml')}


def _version_matches(requirement, version):
    if not isinstance(requirement, str): return False
    if requirement.startswith('='): return requirement[1:] == version
    requirement = requirement.removeprefix('^')
    if not re.fullmatch(r'[1-9][0-9]*(?:\.[0-9]+){0,2}', requirement): return False
    wanted, actual = [int(x) for x in requirement.split('.')], [int(x) for x in version.split('.')]
    return actual[0] == wanted[0] and actual >= wanted + [0] * (3 - len(wanted))


def _bindings(root, paths, package):
    ws = _sections((root / 'Cargo.toml').read_text(encoding='utf-8'))
    inherited = _table(ws.get('workspace.dependencies', ''))
    owners = {}
    for path in paths:
        sections = _sections((root / path).read_text(encoding='utf-8'))
        if any(s.startswith(('patch', 'replace', 'source')) for s in sections):
            raise ValueError('source override requires independent review')
        dependencies = {}
        for section, body in sections.items():
            if section == 'workspace.dependencies': continue
            if section in ('dependencies', 'dev-dependencies', 'build-dependencies') or re.fullmatch(r'target\..+\.(?:dev-|build-)?dependencies', section):
                for alias, entry in _table(body).items():
                    entry = {'version': entry} if isinstance(entry, str) else dict(entry)
                    if entry.get('workspace') is True:
                        if set(entry) - {'workspace', 'features', 'optional', 'default-features'}:
                            raise ValueError('unsupported workspace inheritance')
                        base = inherited[alias]
                        base = {'version': base} if isinstance(base, str) else dict(base)
                        features = base.get('features', []) + entry.get('features', [])
                        base.update({k: v for k, v in entry.items() if k not in ('workspace', 'features')})
                        base['features'] = features; entry = base
                    actual = entry.get('package', alias)
                    if alias == 'serde_json' or actual == 'serde_json':
                        if alias != 'serde_json' or actual != 'serde_json' or set(entry) - {'package', 'version', 'default-features', 'features', 'optional'}:
                            raise ValueError('noncanonical/renamed serde_json dependency')
                        if not _version_matches(entry.get('version'), package['version']):
                            raise ValueError('dependency requirement does not select locked source')
                        if not set(entry.get('features', [])) <= set(SUPPORTED):
                            raise ValueError('unsupported serde_json feature')
                        dependencies.setdefault(alias, []).append(entry)
            elif re.search(r'(?:^|\.)(?:dev-|build-)?dependencies\.serde_json$', section):
                raise ValueError('dependency subtable form unsupported; review required')
        if not dependencies: continue
        for items in _table(sections.get('features', '')).values():
            if not isinstance(items, list): raise ValueError('unsupported feature definition')
            for feature in items:
                if feature.startswith(('serde_json/', 'serde_json?/')) and feature.split('/', 1)[1] not in SUPPORTED:
                    raise ValueError('unsupported forwarded serde_json feature')
        pkg = _table(sections['package'])
        edition = pkg.get('edition')
        if edition == {'workspace': True}: edition = _table(ws.get('workspace.package', '')).get('edition')
        if edition not in ('2018', '2021', '2024'): raise ValueError('absolute external prelude requires Rust 2018+')
        parent = Path(path).parent
        default = 'src/lib.rs' if (root / parent / 'src/lib.rs').is_file() else 'src/main.rs'
        lib = _table(sections.get('lib', '')).get('path', default)
        if lib not in ('src/lib.rs', 'src/main.rs'): raise ValueError('nonstandard crate root requires review')
        lib_path = (parent / lib).as_posix()
        owners[path] = {'dependency': dependencies['serde_json'], 'edition': edition,
                        'lib': lib_path, 'lib_sha256': _local_sha((root / lib_path).read_bytes())}
    return owners


def _item(tokens, prefix):
    starts = [i for i in range(len(tokens)) if tokens[i:i + len(prefix)] == prefix]
    if len(starts) != 1: raise ValueError('missing or ambiguous source declaration')
    start = starts[0]; depth = 0
    for end in range(start, len(tokens)):
        token = tokens[end]
        depth += (token == '{') - (token == '}')
        if (token == '}' and not depth) or (token == ';' and not depth): return tokens[start:end + 1]
    raise ValueError('unclosed source declaration')


def _review(sources):
    expected = {
        'Value': ('src/value/mod.rs', 'pub enum Value', 'pub enum Value { Null, Bool(bool), Number(Number), String(String), Array(Vec<Value>), Object(Map<String, Value>), }'),
        'Number': ('src/number.rs', 'pub struct Number', 'pub struct Number { n: N, }'),
        'N_numeric': ('src/number.rs', '#[cfg(not(feature = "arbitrary_precision"))] #[derive(Copy, Clone)] enum N', '#[cfg(not(feature = "arbitrary_precision"))] #[derive(Copy, Clone)] enum N { PosInt(u64), NegInt(i64), Float(f64), }'),
        'N_string': ('src/number.rs', '#[cfg(feature = "arbitrary_precision")] type N', '#[cfg(feature = "arbitrary_precision")] type N = String;'),
        'Map': ('src/map.rs', 'pub struct Map', 'pub struct Map<K, V> { map: MapImpl<K, V>, }'),
        'Map_tree': ('src/map.rs', '#[cfg(not(feature = "preserve_order"))] type MapImpl', '#[cfg(not(feature = "preserve_order"))] type MapImpl<K, V> = BTreeMap<K, V>;'),
        'Map_ordered_UNSUPPORTED': ('src/map.rs', '#[cfg(feature = "preserve_order")] type MapImpl', '#[cfg(feature = "preserve_order")] type MapImpl<K, V> = IndexMap<K, V, BuildHasherImpl>;'),
    }
    declarations = {}
    for name, (path, prefix, declaration) in expected.items():
        found = _item(_tokens(sources[path]), _tokens(prefix))
        if found != _tokens(declaration): raise ValueError('source structural closure changed: ' + name)
        declarations[name] = {'path': path, 'tokens': found}
    imports = {
        'src/value/mod.rs': ['use alloc::string::String;', 'use alloc::vec::Vec;', 'pub use crate::map::Map;', 'pub use crate::number::Number;'],
        'src/number.rs': ['#[cfg(feature = "arbitrary_precision")] use alloc::string::{String, ToString};'],
        'src/map.rs': ['#[cfg(not(feature = "preserve_order"))] use alloc::collections::{btree_map, BTreeMap};'],
        'src/lib.rs': ['extern crate alloc;', 'pub use crate::value::{from_value, to_value, Map, Number, Value};'],
    }
    for path, entries in imports.items():
        tokens = _tokens(sources[path])
        for entry in entries:
            needle = _tokens(entry)
            if sum(tokens[i:i + len(needle)] == needle for i in range(len(tokens))) != 1:
                raise ValueError('canonical source import changed: ' + entry)
    features = _table(_sections(sources['Cargo.toml'])['features'])
    if set(features) != set(SUPPORTED) | {'preserve_order'}:
        raise ValueError('unknown source feature branch')
    if features['preserve_order'] != ['indexmap', 'alloc', 'dep:foldhash']:
        raise ValueError('unknown ordered map branch')
    return declarations, features


def produce_certificate(root, archive):
    """Data-only producer; its output is NOT trusted until reviewed and hash-pinned."""
    root, archive = Path(root).resolve(), Path(archive)
    package, dependencies = _locked(root)
    data = archive.read_bytes()
    if _sha(data) != package['checksum']: raise ValueError('crate archive checksum mismatch BEFORE inspection')
    # Never extract or execute archive members. Only bounded regular named files are read.
    sources = {}
    with tarfile.open(archive, 'r:gz') as crate:
        members = crate.getmembers()
        if len(members) > 4096 or len({m.name for m in members}) != len(members):
            raise ValueError('ambiguous/oversized archive')
        for path in SOURCE_PATHS:
            member = crate.getmember(package['name'] + '-' + package['version'] + '/' + path)
            if not member.isfile() or member.size > 2000000: raise ValueError('unsafe source member')
            sources[path] = crate.extractfile(member).read().decode('utf-8')
    declared = _table(_sections(sources['Cargo.toml'])['package'])
    if (declared.get('name'), declared.get('version')) != (package['name'], package['version']):
        raise ValueError('archive package identity mismatch')
    declarations, features = _review(sources)
    paths = _manifest_paths(root)
    hashes = _workspace_hashes(root, paths)
    for name, digest in _configs(root).items():
        if digest is not None:
            text = (root / name).read_text(encoding='utf-8')
            if re.search(r'(?m)^\s*\[(?:source|patch|replace|registries)(?:\.|\])', text) or '--extern' in text or '--cfg' in text:
                raise ValueError('Cargo source/compiler override requires review')
    return {'schema': SCHEMA, 'package': package,
            'archive': {'sha256': _sha(data), 'bytes': len(data), 'verified_before_inspection': True,
                        'url': 'https://static.crates.io/crates/{0}/{0}-{1}.crate'.format(package['name'], package['version'])},
            'source_files': {p: {'sha256': _sha(sources[p].encode('utf-8')), 'bytes': len(sources[p].encode('utf-8'))} for p in SOURCE_PATHS},
            'declarations': declarations, 'grammar': GRAMMAR,
            'features': {'definitions': features, 'supported': SUPPORTED, 'unsupported': ['preserve_order'],
                         'locked_dependencies': dependencies,
                         'guard': 'all local manifest/config bytes and complete Cargo.lock pinned except coherent non-resolving root release version; no indexmap/foldhash lock edges'},
            'workspace': {'text_hashing': WORKSPACE_HASHING, 'manifests': hashes['manifests'],
                          'lock_sha256': hashes['lock_sha256'],
                          'configs': _configs(root), 'owners': _bindings(root, paths, package)},
            'trust_boundary': ['reviewed archive bytes and canonical crates.io Cargo identity',
                               'Rust/std sound owned container axioms; arbitrary_precision is owned String',
                               'preserve_order and unknown features/types/configurations rejected',
                               'no attestation of environment/global Cargo config, --config/--extern overrides or compiler unsoundness',
                               'no foreign code execution; runtime requires neither archive/cache/Cargo nor network']}


def _load_certificate():
    certificate = json.loads(CERTIFICATE_PATH.read_text(encoding='utf-8'))
    if _sha(_json(certificate)) != CERTIFICATE_SHA256: raise ValueError('unreviewed certificate')
    if certificate['workspace']['text_hashing'] != WORKSPACE_HASHING:
        raise ValueError('unsupported workspace text hashing')
    if certificate['schema'] != SCHEMA or certificate['grammar'] != GRAMMAR:
        raise ValueError('unsupported certificate grammar/schema')
    if certificate['features']['supported'] != SUPPORTED or certificate['features']['unsupported'] != ['preserve_order']:
        raise ValueError('unsupported certificate feature boundary')
    if set(certificate['source_files']) != set(SOURCE_PATHS): raise ValueError('incomplete source evidence')
    if certificate['archive']['sha256'] != certificate['package']['checksum'] or certificate['package']['source'] != REGISTRY:
        raise ValueError('certificate archive/identity mismatch')
    return certificate


def _root_modules_only(text):
    tokens = _tokens(text)
    i = 0
    while i < len(tokens):
        if tokens[i] == 'pub': i += 1
        if i + 2 >= len(tokens) or tokens[i] != 'mod' or not re.fullmatch(r'[A-Za-z_]\w*', tokens[i + 1]) or tokens[i + 2] != ';':
            return False
        if tokens[i + 1] == 'serde_json': return False
        i += 3
    return True


@lru_cache(maxsize=128)
def _crate_tokens(text):
    """_tokens of one crate source TEXT, shared by every namespace check of the same file; the list is
    only read (a changed file is new text, so a stale entry cannot answer for it)."""
    return _tokens(text)


def _namespace_safe(crate, source, context):
    if context is not None:
        if not isinstance(context, dict) or set(context) - {'imports', 'definitions'}: return False
        if 'serde_json' in context.get('definitions', ()): return False
        if any(entry[0] == 'serde_json' for entry in context.get('imports', ())): return False
    for path in sorted((crate / 'src').rglob('*.rs')):
        if path.is_symlink() or not path.resolve().is_relative_to(crate.resolve()): return False
        tokens = _crate_tokens(path.read_text(encoding='utf-8'))
        for i, token in enumerate(tokens):
            if token == 'serde_json' and i and tokens[i - 1] in ('mod', 'type', 'struct', 'enum', 'union', 'trait', 'as', 'crate'):
                return False
            if token == 'use':
                end = i + 1
                while end < len(tokens) and tokens[end] != ';': end += 1
                used = tokens[i + 1:end]
                if 'serde_json' in used:
                    at = used.index('serde_json')
                    if used[:at] not in ([], ['::']) or used[at + 1:at + 2] != ['::']:
                        return False
    return source.is_file() and not source.is_symlink()


def provenance(rust_modules_root, source_file, type_tokens, namespace_context=None):
    """Return reviewed evidence only for an unambiguous absolute external Value.

    source_file is the declaration's actual owner, NOT the caller's module.
    Relative paths/import aliases are deliberately not resolved by this helper.
    """
    try:
        if isinstance(type_tokens, str) or list(type_tokens) not in (["::", 'serde_json', '::', 'Value'], [':', ':', 'serde_json', ':', ':', 'Value']):
            return None
        root = Path(rust_modules_root).resolve()
        supplied = Path(source_file)
        source = supplied.resolve() if supplied.is_absolute() else (root / supplied).resolve()
        if not source.is_relative_to(root) or supplied.is_symlink(): return None
        certificate = _load_certificate()
        workspace = certificate['workspace']
        if set(_manifest_paths(root)) != set(workspace['manifests']): return None
        hashes = _workspace_hashes(root, _manifest_paths(root))
        if hashes['manifests'] != workspace['manifests']: return None
        if hashes['lock_sha256'] != workspace['lock_sha256'] or _configs(root) != workspace['configs']:
            return None
        package, dependencies = _locked(root)
        if package != certificate['package'] or dependencies != certificate['features']['locked_dependencies']: return None
        owner = source.parent
        while owner != root and not (owner / 'Cargo.toml').is_file(): owner = owner.parent
        manifest = (owner / 'Cargo.toml').relative_to(root).as_posix()
        binding = workspace['owners'].get(manifest)
        if binding is None or not source.is_relative_to(owner / 'src'): return None
        lib_bytes = (root / binding['lib']).read_bytes()
        if _local_sha(lib_bytes) != binding['lib_sha256']: return None
        # A macro/include at crate root could synthesize an extern-prelude binding.
        # Only literal module declarations are understood here; other roots fail closed.
        if not _root_modules_only(lib_bytes.decode('utf-8')): return None
        if not _namespace_safe(owner, source, namespace_context): return None
        return {'schema': SCHEMA, 'certificate_sha256': CERTIFICATE_SHA256,
                'package': package, 'owner_manifest': manifest, 'external_path': '::serde_json::Value',
                'source_files': certificate['source_files'], 'grammar': certificate['grammar'],
                'supported_features': SUPPORTED, 'unsupported_features': ['preserve_order']}
    except (OSError, ValueError, KeyError, TypeError, IndexError, UnicodeError, RecursionError):
        return None


def certify(rust_modules_root, source_file, type_tokens, namespace_context=None):
    return provenance(rust_modules_root, source_file, type_tokens, namespace_context) is not None


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--produce', type=Path, required=True, metavar='VERIFIED_CRATE_ARCHIVE')
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    proof = produce_certificate(args.root, args.produce)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(proof, indent=2, sort_keys=True) + '\n', encoding='utf-8')
    print('REVIEW REQUIRED; canonical certificate SHA256:', _sha(_json(proof)))
