#!/usr/bin/env python3
"""List, and with --delete remove, the GitHub Actions caches that can never be restored again.

Two kinds, and nothing else:

* `closed-pr`   a cache under `refs/pull/<N>/merge` (or `/head`) whose pull request is no longer
                open. Only that pull request could read it. `cache-cleanup.yml` deletes these when a
                pull request closes; this catches the ones that predate it or whose cleanup failed.
* `superseded`  a cache whose key is the same as a NEWER cache's on the same ref except for its
                trailing hash segments. `Swatinem/rust-cache` and `actions/cache` keys end in the
                lockfile / config hash (`...-Linux-x64-6a3d882d-e8c46ae0`); when `Cargo.lock` or the
                toolchain moves, the next save has a new hash and the old generation is never exact-
                matched again. Restores take the newest entry that matches a `restore-keys` prefix,
                so the newest generation of each family on a ref is the one that is restored from
                and is always kept. Generations are compared within one ref only: a pull request's
                entry is never judged against `main`'s.

Dry run is the default: it prints what WOULD be deleted and changes nothing. `--delete` removes
those entries with `gh cache delete <id>`. The token needs `actions: write` for --delete and
`actions: read` and `pull-requests: read` for the listing (an admin's own `gh auth login` has both).

    tools/prune-gh-caches.py                  # dry run against the current repository
    tools/prune-gh-caches.py --delete         # do it
    tools/prune-gh-caches.py --selftest
"""
import argparse
import json
import re
import subprocess
import sys

# A hash generation is one or more trailing `-<8..64 hex digits>` segments: rust-cache ends in two
# 8-digit ones, `actions/cache` keys built from `hashFiles()` end in one 64-digit one.
HASH_TAIL = re.compile(r'(-[0-9a-f]{8,64})+$')
PULL_REF = re.compile(r'^refs/pull/(\d+)/(?:merge|head)$')


def family(key):
    """The key with its trailing hash segments removed; a key with none is its own family."""
    return HASH_TAIL.sub('', key) or key


def plan(caches, open_pulls):
    """Return [(reason, cache)] to delete, oldest last-use first within a reason.

    `caches` are `gh cache list --json id,key,ref,sizeInBytes,createdAt` rows; `open_pulls` is the
    set of open pull request numbers. Pure: no I/O, so the rules are testable."""
    doomed, survivors = [], []
    for cache in caches:
        pull = PULL_REF.match(cache['ref'])
        if pull and int(pull.group(1)) not in open_pulls:
            doomed.append(('closed-pr', cache))
        else:
            survivors.append(cache)
    newest = {}
    for cache in survivors:
        slot = (cache['ref'], family(cache['key']))
        if slot not in newest or cache['createdAt'] > newest[slot]['createdAt']:
            newest[slot] = cache
    for cache in survivors:
        if newest[(cache['ref'], family(cache['key']))] is not cache:
            doomed.append(('superseded', cache))
    return doomed


def gh(*args):
    done = subprocess.run(['gh', *args], capture_output=True, text=True, check=False)
    if done.returncode != 0:
        raise SystemExit(f'gh {" ".join(args[:2])} failed: {done.stderr.strip()}')
    return done.stdout


def list_caches(repo_args):
    return json.loads(gh('cache', 'list', '--limit', '1000', '--json',
                         'id,key,ref,sizeInBytes,createdAt,lastAccessedAt', *repo_args))


def list_open_pulls(repo_args):
    # A failed or truncated listing must not make an open pull request look closed.
    rows = json.loads(gh('pr', 'list', '--state', 'open', '--limit', '1000', '--json', 'number',
                         *repo_args))
    if len(rows) >= 1000:
        raise SystemExit('refusing to guess which pull requests are open: 1000+ listed')
    return {row['number'] for row in rows}


def render(doomed, total_bytes):
    lines = []
    for reason, cache in sorted(doomed, key=lambda item: (item[0], item[1]['ref'], item[1]['key'])):
        lines.append(f'{reason:10} {cache["sizeInBytes"] / 1e6:7.1f} MB  {cache["ref"]:24} '
                     f'{cache["createdAt"][:10]}  {cache["key"]}')
    freed = sum(cache['sizeInBytes'] for _, cache in doomed)
    lines.append(f'{len(doomed)} cache(s), {freed / 1e9:.2f} GB of {total_bytes / 1e9:.2f} GB')
    return '\n'.join(lines)


def selftest():
    def row(ref, key, day, size=100):
        return {'id': len(key) * 7 + day, 'ref': ref, 'key': key, 'sizeInBytes': size,
                'createdAt': f'2026-10-{day:02d}T00:00:00Z'}
    lint = 'v0-rust-host-lint-Linux-x64-6a3d882d-'
    caches = [
        row('refs/heads/main', lint + 'aaaaaaaa', 1),           # older generation on main
        row('refs/heads/main', lint + 'bbbbbbbb', 3),           # newest on main: kept
        row('refs/heads/main', 'v0-rust-other-Linux-x64-6a3d882d-aaaaaaaa', 1),  # own family
        row('refs/pull/7/merge', lint + 'aaaaaaaa', 2),         # open PR, only entry: kept
        row('refs/pull/8/merge', lint + 'cccccccc', 2),         # closed PR: gone
        row('refs/pull/8/head', 'x-' + '0' * 64, 2),            # closed PR head ref: gone
        row('refs/heads/main', 'webos-ndk-Linux-ARM64-webos-d7ed7ee.6', 1),     # no hash tail
        row('refs/heads/main', 'host-ffmpeg-macOS-ARM64-' + 'a' * 64, 1),
        row('refs/heads/main', 'host-ffmpeg-macOS-ARM64-' + 'b' * 64, 5),
    ]
    doomed = plan(caches, {7})
    got = sorted((reason, cache['ref'], cache['key']) for reason, cache in doomed)
    want = sorted([
        ('superseded', 'refs/heads/main', lint + 'aaaaaaaa'),
        ('closed-pr', 'refs/pull/8/merge', lint + 'cccccccc'),
        ('closed-pr', 'refs/pull/8/head', 'x-' + '0' * 64),
        ('superseded', 'refs/heads/main', 'host-ffmpeg-macOS-ARM64-' + 'a' * 64),
    ])
    assert got == want, (got, want)
    assert family(lint + 'aaaaaaaa') == 'v0-rust-host-lint-Linux-x64'
    assert family('webos-ndk-Linux-ARM64-webos-d7ed7ee.6') == 'webos-ndk-Linux-ARM64-webos-d7ed7ee.6'
    assert family('a' * 8) == 'a' * 8, 'a key that is only a hash is its own family, not empty'
    assert plan([], set()) == []
    # Nothing is judged across refs: the same family on two refs keeps both newest entries.
    both = [row('refs/heads/main', lint + 'aaaaaaaa', 1), row('refs/pull/7/merge', lint + 'bbbbbbbb', 9)]
    assert plan(both, {7}) == []
    print('prune-gh-caches selftest: ok')


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    parser.add_argument('--repo', help='OWNER/REPO (default: the current directory\'s repository)')
    parser.add_argument('--delete', action='store_true', help='delete; without it only list')
    parser.add_argument('--selftest', action='store_true')
    args = parser.parse_args()
    if args.selftest:
        selftest()
        return 0
    repo_args = ['--repo', args.repo] if args.repo else []
    caches = list_caches(repo_args)
    doomed = plan(caches, list_open_pulls(repo_args))
    print(render(doomed, sum(cache['sizeInBytes'] for cache in caches)))
    if not args.delete:
        print('dry run: nothing deleted (--delete to remove)')
        return 0
    for _, cache in doomed:
        gh('cache', 'delete', str(cache['id']), *repo_args)
    print(f'deleted {len(doomed)} cache(s)')
    return 0


if __name__ == '__main__':
    sys.exit(main())
