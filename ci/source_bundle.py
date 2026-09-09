"""Source distribution mechanics; never a legal-compliance or rebuild attestation."""
import bz2
import gzip
import lzma
import hashlib
import io
import json
import posixpath
import re
import tarfile
import zipfile
import zlib
from pathlib import Path, PurePosixPath

SCHEMA = 1
# SPDX GPL-3.0-or-later plain text, retained verbatim as the project LICENSE.
GPL_SHA256 = "fb981668c18a279e285fc4d83fba1e836cc84dd4daa73c9697d3cfd2d8aca6e0"
ROOT_FILES = {'Makefile', 'LICENSE', 'LICENSING.md', 'THIRD-PARTY-NOTICES.md',
              'README.md', 'CONTRIBUTING.md', 'DCO', 'PRIVACY.md', 'SECURITY.md',
              'TRADEMARKS.md', '.gitignore', 'AGENTS.md', 'CLAUDE.md'}
ROOT_DIRS = {'src', 'include', 'rust-modules', 'assets', 'ci', 'tools', 'docs', 'tests', 'licenses', '.agents'}
PKG_FILES = {'OFL.txt', 'appfont.ttf', 'appfont-bold.ttf', 'appfont-cjk.ttf',
             'appinfo.json', 'icon.png', 'icon160.png', 'icon320.png', 'icon400.png',
             'largeIcon.png', 'splash.png', 'telemetry.local.json.example'}
PRIVATE_NAMES = {'.tv-host', '.tv-mac', '.tv-dpad-pass', '.tv-remote-url',
                 'config.local.h', 'manifest.local.json', 'auth.json', 'lab.json',
                 'telemetry.local.json', 'local.env', '.env', 'id_rsa', 'id_ed25519'}
# flate2 1.1.9 crate checksum 843fba2746e448b37e26a819579957415c8cef339bf08564fe8b7ddbd959573c;
# .cargo-checksum.json verifies this deliberately invalid decompression regression fixture.
# Raw-byte secret checks still run. No path wildcard or unknown compressed data is exempt.
PUBLIC_CORRUPT_FIXTURES = {
    '083dd284aa1621916a2d0f66ea048c8d3ba7a722b22d0d618722633f51e7d39c':
        'flate2 1.1.9 tests/corrupt-gz-file.bin',
}
MAX_FILE = 512 * 1024 * 1024
MAX_TOTAL = 3 * 1024 * 1024 * 1024


def fail(message):
    raise ValueError(message)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=True).encode()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def safe_name(name):
    p = PurePosixPath(name)
    if not name or p.is_absolute() or '..' in p.parts or '\\' in name or str(p) != name:
        fail('unsafe archive path: ' + name)
    if any(part in PRIVATE_NAMES or part in {'.git', 'plxnative-recordings'} for part in p.parts):
        fail('private path forbidden: ' + name)
    return p


def allowed(name):
    p = safe_name(name)
    # Optional upstream API reference contains credential-shaped public examples; not a build input.
    if name == 'docs/plex-openapi.json' or name.startswith('docs/measurements/'):
        return False
    if name in {'ipkroot/ctl/control', '.agents/skills/wake-tv/wake-tv.sh'} or name.startswith('.claude/hooks/'):
        return True
    if name in ROOT_FILES or p.parts[0] in ROOT_DIRS:
        return not any(part in {'target', '__pycache__'} for part in p.parts)
    if p.parts[0] == 'pkg':
        return (name[4:] in PKG_FILES or
                (len(p.parts) == 4 and p.parts[1] == 'resources' and p.name == 'appinfo.json') or
                name in {'pkg/dev/icon.png', 'pkg/dev/largeIcon.png'})
    return (name.startswith('vendor/nanosvg/') or name == 'vendor/sentry-native/webos-arm32.patch'
            or name.startswith('.github/'))


def read_regular(root, name):
    safe_name(name)
    path = root / name
    # Reject links in ancestors too; a tracked directory can itself have been replaced.
    for parent in [path, *path.parents]:
        if parent == root:
            break
        if parent.is_symlink():
            fail('symlink input forbidden: ' + name)
    if not path.is_file():
        fail('missing regular input: ' + name)
    if path.stat().st_size > MAX_FILE:
        fail('input too large: ' + name)
    return path.read_bytes(), 0o755 if path.stat().st_mode & 0o111 else 0o644


# Public demo endpoint in the checksum-pinned upstream Android sample, not our configuration.
# Only this pattern is exempt; literal private values and all other credential checks still run.
SENTRY_DSN_PATTERN = rb'https?://[0-9a-fA-F]{24,}@[A-Za-z0-9.-]*ingest[A-Za-z0-9.-]*sentry\.io/'
PUBLIC_DEMO_DSN_FILES = {
    'sentry-native-0.16.6/ndk/sample/src/main/java/io/sentry/ndk/sample/MainActivity.java':
        '5c67de55db824517dc03b4d3aba066f45bb6e94425ce7c176733aad75719b1be',
}

def scan(data, name, private_values=(), depth=0, budget=None):
    """Inspect actual serialized bytes, recursively including dependency source archives."""
    if budget is None:
        budget = [0]
    budget[0] += len(data)
    if depth > 8 or len(data) > MAX_FILE or budget[0] > MAX_TOTAL:
        fail('archive scan resource limit: ' + name)
    for value in private_values:
        if value and value in data:
            fail('private value found in: ' + name)  # Never print the matched value.
    patterns = [rb'-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----[\r\n]+[A-Za-z0-9+/=\r\n]{32,}',
                rb'PLXNATIVE_' + rb'PRIVATE_SENTINEL_[A-Za-z0-9]+',
                rb'\bgh[pousr]_[A-Za-z0-9]{30,}',
                rb'\bAKIA[A-Z0-9]{16}\b',
                SENTRY_DSN_PATTERN,
                rb'\bphc_[A-Za-z0-9]{24,}',
                rb'(?i)(?:X-Plex-Token|SENTRY_AUTH_TOKEN)\s*[=:]\s*["\']?[A-Za-z0-9_-]{20,}']
    for pattern in patterns:
        if not re.search(pattern, data):
            continue
        if (pattern == SENTRY_DSN_PATTERN
                and PUBLIC_DEMO_DSN_FILES.get(name.rsplit(':', 1)[-1]) == digest(data)):
            continue
        fail('credential pattern found in: ' + name)
    if data.startswith(b'PK\x03\x04'):
        with zipfile.ZipFile(io.BytesIO(data)) as z:
            for entry in z.infolist():
                safe_name(entry.filename.rstrip('/'))
                if (entry.external_attr >> 16) & 0o170000 == 0o120000:
                    fail('nested zip symlink forbidden: ' + name)
                if not entry.is_dir():
                    if entry.file_size > MAX_FILE:
                        fail('nested member too large: ' + name)
                    scan(z.read(entry), name + ':' + entry.filename, private_values, depth + 1, budget)
        return
    try:
        archive = tarfile.open(fileobj=io.BytesIO(data), mode='r:*')
    except (tarfile.ReadError, zlib.error, EOFError, OSError):
        # Dependency examples can be compressed single files, not only tarballs.
        decoder = (gzip.GzipFile if data.startswith(b'\x1f\x8b') else
                   lzma.LZMAFile if data.startswith(b'\xfd7zXZ\x00') else
                   bz2.BZ2File if data.startswith(b'BZh') else None)
        if decoder:
            try:
                stream = (decoder(fileobj=io.BytesIO(data), mode='rb') if decoder is gzip.GzipFile
                          else decoder(io.BytesIO(data), 'rb'))
                with stream:
                    unpacked = stream.read(MAX_FILE + 1)
            except (OSError, EOFError, zlib.error):
                if digest(data) in PUBLIC_CORRUPT_FIXTURES:
                    return
                fail('unreadable compressed source: ' + name)
            scan(unpacked, name + ':uncompressed', private_values, depth + 1, budget)
        return
    with archive:
        for entry in archive:
            member_name = entry.name.removeprefix('./').rstrip('/')
            if member_name in {'', '.'} and entry.isdir():
                continue
            safe_name(member_name)
            if entry.issym() or entry.islnk():
                target = entry.linkname
                resolved = posixpath.normpath(posixpath.join(posixpath.dirname(member_name), target)
                                             if entry.issym() else target)
                if target.startswith('/') or resolved == '..' or resolved.startswith('../'):
                    fail('nested archive link escapes: ' + name)
                safe_name(resolved)
            elif entry.isfile():
                if entry.size > MAX_FILE:
                    fail('nested member too large: ' + name)
                scan(archive.extractfile(entry).read(), name + ':' + member_name,
                     private_values, depth + 1, budget)
            elif not entry.isdir():
                fail('special nested member forbidden: ' + name)


def snapshot(files):
    return digest(canonical(files))


def write_archive(path, contents, epoch):
    with path.open('wb') as raw:
        with gzip.GzipFile(fileobj=raw, mode='wb', filename='', mtime=epoch) as gz:
            with tarfile.open(fileobj=gz, mode='w', format=tarfile.PAX_FORMAT) as archive:
                for name, (data, mode) in sorted(contents.items()):
                    entry = tarfile.TarInfo(name)
                    entry.size, entry.mode, entry.mtime = len(data), mode, epoch
                    entry.uid = entry.gid = 0
                    archive.addfile(entry, io.BytesIO(data))


def validate(path, expected_snapshot=None, private_values=()):
    contents, modes = {}, {}
    total = 0
    with tarfile.open(path, 'r:*') as archive:
        for entry in archive:
            safe_name(entry.name)
            if not entry.isfile() or entry.name in contents:
                fail('nonregular or duplicate bundle member: ' + entry.name)
            total += entry.size
            if entry.size > MAX_FILE or total > MAX_TOTAL:
                fail('bundle too large')
            if entry.mode not in {0o644, 0o755}:
                fail('unexpected bundle permissions: ' + entry.name)
            contents[entry.name] = archive.extractfile(entry).read()
            modes[entry.name] = entry.mode
    scan_budget = [0]
    for name, data in contents.items():
        scan(data, name, private_values, budget=scan_budget)
    manifest = json.loads(contents.pop('SOURCE-MANIFEST.json'))
    if manifest.get('schema') != SCHEMA or manifest.get('license') != 'GPL-3.0-or-later':
        fail('source manifest schema/license mismatch')
    actual = {name: {'sha256': digest(data), 'mode': modes[name]} for name, data in contents.items()}
    if actual != manifest['files']:
        fail('source file set/hash/mode mismatch')
    if snapshot(actual) != manifest['snapshot_sha256']:
        fail('source snapshot mismatch')
    if expected_snapshot and expected_snapshot != manifest['snapshot_sha256']:
        fail('source snapshot differs from expected candidate')
    for required in ['LICENSE', 'LICENSING.md', 'THIRD-PARTY-NOTICES.md', 'Makefile',
                     'rust-modules/Cargo.toml', 'rust-modules/Cargo.lock', 'ipkroot/ctl/control']:
        if required not in contents:
            fail('missing required source: ' + required)
    license_text = contents['LICENSE']
    if digest(license_text) != GPL_SHA256:
        fail('missing complete GPLv3 text')
    # Check actual metadata, not only the generated manifest claim.
    cargo = contents['rust-modules/Cargo.toml'].decode()
    if not re.search(r'^license\s*=\s*"GPL-3\.0-or-later"\s*$', cargo, re.M):
        fail('Cargo metadata must be GPL-3.0-or-later')
    if 'pkg/appinfo.json' in contents:
        # webOS appinfo has no required license field. Control/Cargo carry the election.
        if json.loads(contents['pkg/appinfo.json']).get('license', 'GPL-3.0-or-later') != 'GPL-3.0-or-later':
            fail('package metadata must be GPL-3.0-or-later')
    control = contents['ipkroot/ctl/control'].decode()
    if not re.search(r'^License:\s*GPL-3\.0-or-later\s*$', control, re.M):
        fail('control metadata must be GPL-3.0-or-later')
    dependencies = manifest['dependencies']
    ids = [dep['id'] for dep in dependencies]
    if len(ids) != len(set(ids)):
        fail('duplicate dependency identity')
    for required in ['ffmpeg', 'sentry-native', 'cargo-vendor', 'rust-runtime']:
        if required not in ids:
            fail('missing dependency source mapping: ' + required)
    for dep in dependencies:
        for key in ['id', 'version', 'license']:
            if not dep.get(key) or dep[key] in {'UNKNOWN', 'NOASSERTION'}:
                fail('unresolved dependency ' + key)
        if not dep.get('sources'):
            fail('missing dependency sources: ' + dep['id'])
        for entry in dep['sources']:
            if not entry['path'].startswith('dependencies/' + dep['id'] + '/'):
                fail('dependency source outside its namespace: ' + dep['id'])
            if entry['path'] not in actual or actual[entry['path']]['sha256'] != entry['sha256']:
                fail('missing or wrong dependency source: ' + dep['id'])
        for recipe in dep.get('recipes', []) + dep.get('patches', []):
            if recipe not in actual:
                fail('missing dependency recipe/patch: ' + recipe)
    for name in contents:
        if not name.startswith('dependencies/') and not allowed(name):
            fail('outside source allowlist: ' + name)
    if not manifest.get('build_environment') or not manifest.get('git_commit'):
        fail('missing build/snapshot identity')
    return manifest
