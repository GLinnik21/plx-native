#!/usr/bin/env python3
"""Keep ci/dependencies.json current: find upstream releases, bump an entry, describe it for GitHub.

Dependabot reads package manifests, not this file, so this script does the same job for the
non-Rust inputs. Each manifest entry names where its releases come from in an `update` object; the
script has no per-dependency knowledge. The types:

    github-release  {repo, tag_pattern[, tag_format, order]}  GitHub releases (drafts and prereleases skipped)
    github-tags     {repo, tag_pattern[, tag_format]}         GitHub tags, for repositories that publish none
    github-commit   {repo[, ref]}                             the newest commit of a ref (default: the default branch);
                                                              for entries pinned to a commit, not a release
    listing         {url, file_pattern}                       an HTML directory listing of release files
    manual          {}                                        not tracked (the entry is also held)

`tag_pattern` is a regex with ONE group capturing the version part of a tag (`_` reads as `.`);
`tag_format` rebuilds a tag from a version (`v{version}`, `libunibreak_{version_}`; default
`{version}`), which is how the current pin's tag is known and how a bump rewrites URLs. `order:
published` ranks releases by publication date instead of by version (for a version that is a hash).
An optional `hold` string freezes the entry: it is reported but never bumped, and the string is the
reason.

    python3 ci/deps-update.py --report [--json]        pinned vs latest, per entry
    python3 ci/deps-update.py --list-outdated          "name<TAB>latest" for each entry a bump may change
    python3 ci/deps-update.py --bump NAME [--body-file F]
                                                       download the new asset(s), recompute sha256, rewrite the
                                                       entry's version / urls / sha256 and its rows of
                                                       THIRD-PARTY-NOTICES.md (never a patch; a patch's pinned
                                                       `result` digests are recomputed). Prints the version used
                                                       on its first line. Exit 3 when a pinned patch no longer
                                                       applies (strict `git apply`, like the build) or a notices
                                                       row is not found (the manifest IS written, so the PR
                                                       shows it).
    python3 ci/deps-update.py --snapshot --sha S --ref R   GitHub dependency-submission JSON (stdout)

GITHUB_TOKEN, when set, authenticates the GitHub API (60 -> 5000 requests/hour). Standard library
only (macOS system Python 3.9 included).
"""
import argparse
import copy
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / 'ci/dependencies.json'
NOTICES = ROOT / 'THIRD-PARTY-NOTICES.md'
# The most an upstream source archive may unpack to (bytes) and how many members it may have.
MAX_EXTRACT_BYTES = 2 << 30
MAX_EXTRACT_MEMBERS = 200000
TYPES = {'github-release', 'github-tags', 'github-commit', 'listing', 'manual'}
SAFE = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]*$')
REPO = re.compile(r'^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$')
# Only the dependency-graph submission needs these; the detector is this script.
DETECTOR = {'name': 'plxnative-deps-update', 'version': '1',
            'url': 'https://github.com/GLinnik21/plx-native/blob/main/ci/deps-update.py'}


class BumpError(Exception):
    pass


class PatchError(BumpError):
    """The bump needs a person (a patch, or the notices): the manifest IS written and `version` is the
    version it moved to, so the pull request can still be opened as a draft."""
    version = None


# --- reading upstream -------------------------------------------------------------------------

def http_get(url, accept=None):
    """The body of `url` as text. The one network read of the lookups, replaced in tests."""
    headers = {'User-Agent': 'plxnative-deps-update', 'Accept': accept or '*/*'}
    token = os.environ.get('GITHUB_TOKEN')
    if token and url.startswith('https://api.github.com/'):
        headers['Authorization'] = 'Bearer ' + token
        headers['X-GitHub-Api-Version'] = '2022-11-28'
    try:
        with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=60) as response:
            return response.read().decode('utf-8', 'replace')
    except (urllib.error.URLError, OSError) as error:
        raise BumpError('cannot read ' + url + ': ' + str(error))


def github_json(path):
    return json.loads(http_get('https://api.github.com' + path, 'application/vnd.github+json'))


def version_key(version):
    """Sort key: runs of digits compare as numbers and rank above words (`1.10` > `1.9`)."""
    return tuple((1, int(part)) if part.isdigit() else (0, part)
                 for part in re.findall(r'\d+|[A-Za-z]+', version))


def version_of(tag, pattern):
    """The version a tag names under `pattern`, or None when the tag is not a release tag."""
    found = re.match(pattern, tag)
    if not found or not found.groups() or not found.group(1):
        return None
    return found.group(1).replace('_', '.')


def format_tag(update, version):
    return update.get('tag_format', '{version}').format(version=version, version_=version.replace('.', '_'))


def current_tag(dep):
    return format_tag(dep.get('update', {}), dep['version'])


def newest(candidates, by_published=False):
    """The newest of [(version, tag, published)]: highest version, or latest publication."""
    if not candidates:
        return None
    if by_published:
        return max(candidates, key=lambda c: c[2] or '')
    return max(candidates, key=lambda c: version_key(c[0]))


def latest(dep):
    """What upstream's newest release is: {version, tag, url}. Raises BumpError when it cannot tell."""
    update = dep['update']
    kind = update['type']
    if kind == 'github-release':
        releases = github_json('/repos/' + update['repo'] + '/releases?per_page=100')
        found = [(version_of(r['tag_name'], update['tag_pattern']), r['tag_name'], r.get('published_at'))
                 for r in releases if not r.get('draft') and not r.get('prerelease')]
        best = newest([c for c in found if c[0]], update.get('order') == 'published')
        link = 'https://github.com/' + update['repo'] + '/releases/tag/'
    elif kind == 'github-tags':
        tags = github_json('/repos/' + update['repo'] + '/tags?per_page=100')
        found = [(version_of(t['name'], update['tag_pattern']), t['name'], None) for t in tags]
        best = newest([c for c in found if c[0]])
        link = 'https://github.com/' + update['repo'] + '/tree/'
    elif kind == 'github-commit':
        commit = github_json('/repos/' + update['repo'] + '/commits/' + update.get('ref', 'HEAD'))
        best = (commit['sha'], commit['sha'], None)
        link = 'https://github.com/' + update['repo'] + '/commit/'
    elif kind == 'listing':
        page = http_get(update['url'])
        names = [href.rstrip('/').rsplit('/', 1)[-1] for href in re.findall(r'href="([^"]+)"', page)]
        found = [(version_of(n, update['file_pattern']), n, None) for n in names]
        best = newest([c for c in found if c[0]])
        link = update['url'].rstrip('/') + '/'
    else:
        raise BumpError(dep['id'] + ' is not tracked (update type manual)')
    if best is None:
        raise BumpError('no release of ' + dep['id'] + ' matches its tag pattern')
    return {'version': best[0], 'tag': best[1], 'url': link + best[1]}


def is_outdated(dep, newest_release):
    if dep['update']['type'] == 'github-commit':
        return dep['version'] != newest_release['version']
    if dep['update'].get('order') == 'published':
        return dep['version'] != newest_release['version']
    return version_key(newest_release['version']) > version_key(dep['version'])


def status(dep):
    """{id, pinned, latest, outdated, hold, error, release} for one entry."""
    row = {'id': dep['id'], 'pinned': dep['version'][:12] if dep['update']['type'] == 'github-commit' else dep['version'],
           'latest': None, 'outdated': False, 'hold': dep['update'].get('hold'), 'error': None, 'release': None}
    if dep['update']['type'] == 'manual':
        return row
    try:
        row['release'] = latest(dep)
    except (BumpError, KeyError, ValueError) as error:
        row['error'] = str(error)
        return row
    version = row['release']['version']
    row['latest'] = version[:12] if dep['update']['type'] == 'github-commit' else version
    row['outdated'] = is_outdated(dep, row['release'])
    return row


# --- the manifest -----------------------------------------------------------------------------

def load(path=MANIFEST):
    return json.loads(Path(path).read_text())


def dump(entries):
    return json.dumps(entries, indent=2, ensure_ascii=False) + '\n'


def update_problems(entries):
    """Schema violations of the `update` objects. Offline."""
    found = []
    for dep in entries:
        name = dep.get('id', '?')
        update = dep.get('update')
        if not isinstance(update, dict) or update.get('type') not in TYPES:
            found.append(name + ': update.type must be one of ' + ', '.join(sorted(TYPES)))
            continue
        kind = update['type']
        for key in ('hold',):
            if key in update and not (isinstance(update[key], str) and update[key].strip()):
                found.append(name + ': update.hold must be a non-empty reason')
        if kind == 'manual' and not update.get('hold'):
            found.append(name + ': a manual entry must carry update.hold (the reason it is not tracked)')
        if kind in ('github-release', 'github-tags', 'github-commit') and not REPO.match(str(update.get('repo'))):
            found.append(name + ': update.repo must be owner/name')
        pattern_key = 'file_pattern' if kind == 'listing' else 'tag_pattern'
        if kind in ('github-release', 'github-tags', 'listing'):
            try:
                if re.compile(update[pattern_key]).groups != 1:
                    found.append(name + ': update.' + pattern_key + ' needs exactly one capture group')
            except (KeyError, re.error, TypeError) as error:
                found.append(name + ': update.' + pattern_key + ' is not a regex: ' + str(error))
            else:
                # The pinned version must be what the pattern would extract from the pinned tag.
                tag = format_tag(update, dep.get('version', ''))
                if kind != 'listing' and version_of(tag, update[pattern_key]) != dep.get('version') \
                        and update.get('order') != 'published':
                    found.append(name + ': tag_format/tag_pattern do not round-trip the pinned version ' + dep.get('version', ''))
        if kind == 'listing' and not str(update.get('url', '')).startswith('https://'):
            found.append(name + ': update.url must be https')
        if kind == 'github-commit' and not re.match(r'^[0-9a-f]{40}$', dep.get('version', '')):
            found.append(name + ': a github-commit entry is pinned to a 40-digit commit')
    return found


def substitute(value, replacements):
    for old, new in replacements:
        value = value.replace(old, new)
    return value


def rewritten(dep, release):
    """A copy of `dep` moved to `release`: tag, then version, replaced in every name and URL."""
    update = dep['update']
    new_version = release['version']
    new_tag = release['tag'] if update['type'] != 'listing' else format_tag(update, new_version)
    pairs = [(current_tag(dep), new_tag), (dep['version'], new_version)]
    pairs = sorted({p for p in pairs if p[0] != p[1]}, key=lambda p: -len(p[0]))
    out = copy.deepcopy(dep)
    out['version'] = new_version
    if 'release_tag' in out:
        raise BumpError(dep['id'] + ': release_tag cannot be rewritten automatically')

    def fix(holder):
        for key in ('archive', 'name', 'url'):
            if key in holder:
                holder[key] = substitute(holder[key], pairs)
        if 'mirrors' in holder:
            holder['mirrors'] = [substitute(url, pairs) for url in holder['mirrors']]

    fix(out)
    for asset in out.get('assets', {}).values():
        fix(asset)
    return out


def download_digest(url):
    """sha256 of the bytes at `url`, streamed. The network read of a bump, replaced in tests."""
    h = hashlib.sha256()
    request = urllib.request.Request(url, headers={'User-Agent': 'plxnative-deps-update'})
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            for block in iter(lambda: response.read(1 << 20), b''):
                h.update(block)
    except (urllib.error.URLError, OSError) as error:
        raise BumpError('cannot download ' + url + ': ' + str(error))
    return h.hexdigest()


def download_to(url, path):
    request = urllib.request.Request(url, headers={'User-Agent': 'plxnative-deps-update'})
    try:
        with urllib.request.urlopen(request, timeout=120) as response, open(path, 'wb') as out:
            for block in iter(lambda: response.read(1 << 20), b''):
                out.write(block)
    except (urllib.error.URLError, OSError) as error:
        raise BumpError('cannot download ' + url + ': ' + str(error))


def pin_assets(dep, notes):
    """Recompute sha256 for every asset of the rewritten entry; a mirror that is unreachable is
    dropped (noted), one that serves other bytes fails the bump."""
    holders = [dep] if 'assets' not in dep else list(dep['assets'].values())
    for holder in holders:
        holder['sha256'] = download_digest(holder['url'])
        kept = []
        for mirror in holder.get('mirrors', []):
            try:
                other = download_digest(mirror)
            except BumpError as error:
                notes.append('mirror dropped, it does not serve the new file yet: ' + mirror)
                continue
            if other != holder['sha256']:
                raise BumpError('mirror ' + mirror + ' serves different bytes than ' + holder['url'])
            kept.append(mirror)
        if 'mirrors' in holder:
            if kept:
                holder['mirrors'] = kept
            else:
                del holder['mirrors']
    # keep key order stable: sha256 after url/mirrors
    return dep


def require_data_filter():
    if not hasattr(tarfile, 'data_filter'):
        raise BumpError('this Python (' + sys.version.split()[0] + ') has no tarfile.data_filter, so an upstream '
                        'archive cannot be unpacked safely; run the bump with Python 3.12+ (the workflow does)')


def unpack_source(archive, tree):
    """Extract an upstream tarball into the throwaway `tree`. Refuses (BumpError) without
    `tarfile.data_filter` (Python < 3.12 and the security backports), because a bare `extractall`
    follows absolute paths and links out of `tree`; and refuses an archive that unpacks to more than
    MAX_EXTRACT_BYTES / MAX_EXTRACT_MEMBERS."""
    require_data_filter()
    with tarfile.open(archive) as tar:
        members = tar.getmembers()
        if len(members) > MAX_EXTRACT_MEMBERS or sum(m.size for m in members) > MAX_EXTRACT_BYTES:
            raise BumpError(Path(archive).name + ' unpacks to more than ' + str(MAX_EXTRACT_BYTES >> 20) +
                            ' MiB or ' + str(MAX_EXTRACT_MEMBERS) + ' members; refusing')
        tar.extractall(tree, members=members, filter='data')  # an upstream tarball, into a throwaway directory


def file_digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def check_patches(dep, root=ROOT):
    """Apply each pinned patch to the new sources exactly as ci/build-libass.py does (strict
    `git apply --check`, then `git apply`, never `patch` and its fuzz), and recompute the patch's
    pinned `result` digests IN PLACE in `dep` from the patched files. Returns notes for the pull
    request. Raises PatchError with git's own words."""
    patches = dep.get('patches', [])
    if not patches or 'archive' not in dep:
        return []
    if shutil.which('git') is None:
        raise BumpError('git is required to check the ' + dep['id'] + ' patches')
    notes = []
    with tempfile.TemporaryDirectory() as scratch:
        archive = Path(scratch) / dep['archive']
        tree = Path(scratch) / 'src'
        require_data_filter()  # before anything is downloaded
        download_to(dep['url'], archive)
        unpack_source(archive, tree)
        tops = [p for p in tree.iterdir()]
        source = tops[0] if len(tops) == 1 and tops[0].is_dir() else tree
        # `source` is outside any repository; the ceiling keeps git from discovering one anyway.
        env = {k: v for k, v in os.environ.items() if k not in ('GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE')}
        env['GIT_CEILING_DIRECTORIES'] = str(source.parent)
        for patch in patches:
            path = (root / patch['path']).resolve()
            for extra in (['--check'], []):
                result = subprocess.run(['git', 'apply', *extra, str(path)], cwd=source, env=env,
                                        capture_output=True, text=True)
                if result.returncode != 0:
                    raise PatchError(patch['path'] + ' no longer applies to ' + dep['id'] + ' ' + dep['version'] +
                                     ':\n' + (result.stdout + result.stderr).strip())
            for relative in sorted(patch.get('result', {})):
                target = source / relative
                if not target.is_file():
                    raise PatchError(patch['path'] + ' left no ' + relative + ' to pin')
                digest = file_digest(target)
                if digest != patch['result'][relative]:
                    notes.append('pinned result digest of ' + relative + ' (patch ' + patch['path'] + ') recomputed')
                patch['result'][relative] = digest
    return notes


# THIRD-PARTY-NOTICES.md states some pins in prose. Per entry id: exact strings, with {version},
# {sha256} and {archive} filled from the entry before and after the bump. Only these known rows are
# rewritten; if one is not found as written the bump fails loudly (draft pull request) instead of
# leaving the notices stale or guessing at a regex.
NOTICE_ROWS = {
    'ffmpeg': ['FFmpeg **{version}**, unmodified',
               '`{archive}`, sha256 `{sha256}`',
               'FFMPEG_VERSION={version} FFMPEG_SHA256={sha256} FFMPEG_TARBALL=/path/to/{archive}',
               '| FFmpeg (libavformat / libavcodec / libavutil) | **{version}, shipped in this package** |'],
    'libass': ['| libass | {version} |', 'checksum-verified libass {version} release'],
    'freetype': ['| FreeType | {version} |'],
    'fribidi': ['| FriBidi | {version} |'],
    'harfbuzz': ['| HarfBuzz | {version} |'],
    'libunibreak': ['| libunibreak | {version} |'],
    'sentry-native': ['**Sentry Native {version}**'],
}


def notice_text(row, dep):
    return row.format(version=dep['version'], sha256=dep.get('sha256', ''), archive=dep.get('archive', ''))


def update_notices(old, new, text):
    """`text` of THIRD-PARTY-NOTICES.md with the rows of `old` rewritten for `new`. Raises
    BumpError naming every expected row that is not in `text` (nothing is rewritten then)."""
    rows = NOTICE_ROWS.get(old['id'], [])
    missing = [notice_text(r, old) for r in rows if notice_text(r, old) not in text]
    if missing:
        raise BumpError('THIRD-PARTY-NOTICES.md does not contain the expected text for ' + old['id'] +
                        ' (update it by hand): ' + '; '.join(repr(m) for m in missing))
    for row in rows:
        text = text.replace(notice_text(row, old), notice_text(row, new))
    return text


def pr_title(old, new):
    return 'Dependencies: ' + new['id'] + ' ' + old['version'][:12] + ' -> ' + new['version'][:12]


def pr_body(old, new, release, notes, failure=None):
    lines = ['Automated bump of `' + new['id'] + '` in `ci/dependencies.json` (`ci/deps-update.py`): `' +
             old['version'] + '` -> `' + new['version'] + '`.', '',
             'Upstream release: ' + release['url'], '',
             'The version, URLs and sha256 changed, as did the matching rows of `THIRD-PARTY-NOTICES.md`; patches '
             'were NOT touched (their pinned `result` digests are recomputed).']
    if failure:
        lines += ['', '**A pinned patch no longer applies, or THIRD-PARTY-NOTICES.md could not be updated. '
                      'This PR needs a person.**',
                  '', '```', failure, '```']
    for note in notes:
        lines += ['', '- ' + note]
    lines += ['', '## Review', '',
              '- [ ] FFmpeg, Sentry Native, curl-adjacent and NDK/toolchain bumps need the `fw-compat-reviewer` agent '
              '(the binary must still start on every firmware) before merge.',
              '- [ ] Check `THIRD-PARTY-NOTICES.md` against the new release (licence, copyright holders).',
              '- [ ] CI is dispatched on this branch by the workflow (a `GITHUB_TOKEN` push starts no `pull_request` run).',
              '', 'Generated by `.github/workflows/deps-update.yml`.']
    return '\n'.join(lines) + '\n'


def bump(name, path=MANIFEST, body_file=None, notices=NOTICES):
    """Move one entry to upstream's newest release. Returns (old, new, notes); raises BumpError, and
    PatchError (with `.version`) after the manifest has been written, when a patch or the notices
    need a person."""
    entries = load(path)
    index = next((i for i, d in enumerate(entries) if d['id'] == name), None)
    if index is None:
        raise BumpError('no dependency named ' + name)
    old = entries[index]
    if old['update'].get('hold'):
        raise BumpError(name + ' is held: ' + old['update']['hold'])
    release = latest(old)
    if not is_outdated(old, release):
        raise BumpError(name + ' is already at ' + old['version'])
    for text in (release['version'], release['tag']):
        if not SAFE.match(text):
            raise BumpError('refusing an unexpected version/tag string: ' + repr(text))
    notes = []
    new = pin_assets(rewritten(old, release), notes)
    failures = []
    try:
        notes += check_patches(new)  # also recomputes the patches' pinned result digests in `new`
    except PatchError as error:
        failures.append(str(error))
    entries[index] = new
    Path(path).write_text(dump(entries))
    try:
        notices_path = Path(notices)
        notices_path.write_text(update_notices(old, new, notices_path.read_text()))
    except BumpError as error:
        failures.append(str(error))
    failure = '\n\n'.join(failures) or None
    if body_file:
        Path(body_file).write_text(pr_body(old, new, release, notes, failure))
    if failure:
        error = PatchError(failure)
        error.version = new['version']
        raise error
    return old, new, notes


# --- dependency graph -------------------------------------------------------------------------

def purl_of(dep):
    update = dep['update']
    if update['type'] in ('github-release', 'github-tags', 'github-commit'):
        ref = dep['version'] if update['type'] == 'github-commit' else current_tag(dep)
        return 'pkg:github/' + update['repo'].lower() + '@' + ref
    first = next(iter(sorted(a['url'] for a in assets_urls(dep))))
    return 'pkg:generic/' + dep['id'] + '@' + dep['version'] + '?download_url=' + urllib.parse.quote(first, safe=':/')


def assets_urls(dep):
    return [{'url': a['url']} for a in dep['assets'].values()] if 'assets' in dep else [{'url': dep['url']}]


def snapshot(entries, sha, ref, run_id='0', now=None):
    """The body of POST /repos/{owner}/{repo}/dependency-graph/snapshots (dependency submission API)."""
    resolved = {}
    for dep in entries:
        purl = purl_of(dep)
        resolved[dep['id']] = {
            'package_url': purl,
            'relationship': 'direct',
            'scope': 'runtime' if dep.get('bundled') else 'development',
            'metadata': {'license': dep['license']},
        }
    return {
        'version': 0,
        'sha': sha,
        'ref': ref,
        'job': {'correlator': 'plxnative-deps-manifest', 'id': str(run_id)},
        'detector': DETECTOR,
        'scanned': (now or datetime.datetime.now(datetime.timezone.utc)).strftime('%Y-%m-%dT%H:%M:%SZ'),
        'manifests': {'ci/dependencies.json': {
            'name': 'ci/dependencies.json',
            'file': {'source_location': 'ci/dependencies.json'},
            'resolved': resolved,
        }},
    }


# --- command line -----------------------------------------------------------------------------

def print_table(rows):
    header = ['name', 'pinned', 'latest', 'status']
    body = []
    for row in rows:
        if row['error']:
            state = 'error: ' + row['error']
        elif row['hold']:
            state = 'HELD' + ('' if row['latest'] is None else ' (' + ('behind' if row['outdated'] else 'current') + ')') + ': ' + row['hold']
        elif row['latest'] is None:
            state = 'not tracked'
        else:
            state = 'OUTDATED' if row['outdated'] else 'current'
        body.append([row['id'], row['pinned'], row['latest'] or '-', state])
    widths = [max(len(r[i]) for r in [header] + body) for i in range(3)]
    for r in [header] + body:
        print('  '.join(r[i].ljust(widths[i]) for i in range(3)) + '  ' + r[3])


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--report', action='store_true')
    mode.add_argument('--list-outdated', action='store_true')
    mode.add_argument('--bump', metavar='NAME')
    mode.add_argument('--snapshot', action='store_true')
    parser.add_argument('--json', action='store_true', help='with --report: machine-readable')
    parser.add_argument('--body-file', help='with --bump: write the pull request body here')
    parser.add_argument('--sha')
    parser.add_argument('--ref')
    args = parser.parse_args(argv)
    try:
        entries = load()
        if args.snapshot:
            if not args.sha or not args.ref:
                parser.error('--snapshot needs --sha and --ref')
            print(json.dumps(snapshot(entries, args.sha, args.ref, os.environ.get('GITHUB_RUN_ID', '0')), indent=2))
            return 0
        if args.bump:
            try:
                old, new, notes = bump(args.bump, body_file=args.body_file)
            except PatchError as error:
                print(error.version)  # first line: the version the manifest moved to (the branch name)
                raise
            print(new['version'])  # first line: the version actually used (the workflow's branch name)
            print(pr_title(old, new))
            for note in notes:
                print('note: ' + note)
            return 0
        rows = [status(dep) for dep in entries]
        if args.list_outdated:
            unsafe = False
            for row in rows:
                if row['error']:
                    print('deps-update: ' + row['id'] + ': ' + row['error'], file=sys.stderr)
                elif row['outdated'] and not row['hold']:
                    version = row['release']['version']
                    if not SAFE.match(version) or not SAFE.match(row['release']['tag']):
                        print('deps-update: ' + row['id'] + ': refusing an unexpected version/tag string: ' +
                              repr(version), file=sys.stderr)
                        unsafe = True
                    else:
                        print(row['id'] + '\t' + version)
            return 1 if unsafe or any(r['error'] for r in rows) else 0
        if args.json:
            print(json.dumps(rows, indent=2))
        else:
            print_table(rows)
        return 1 if any(r['error'] for r in rows) else 0
    except PatchError as error:
        print('deps-update: PATCH DOES NOT APPLY: ' + str(error), file=sys.stderr)
        return 3
    except (BumpError, OSError, ValueError, KeyError) as error:
        print('deps-update: ' + str(error), file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
