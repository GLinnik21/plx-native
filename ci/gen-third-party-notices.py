#!/usr/bin/env python3
"""Render the Rust crate tables of THIRD-PARTY-NOTICES.md section 2.4 from the lockfiles.

The versions in that section used to be typed by hand, which a dependency bump (a person's or
Dependabot's) silently falsifies: the notices then name a version that is not the one shipped. Now
the VERSIONS come from `rust-modules/Cargo.lock` and, for the standard library's own crates, from
the std workspace lock, and the rest (licence election, copyright holders, the grouping) lives in
`ci/third-party-crates.json`, because no lockfile holds those.

    python3 ci/gen-third-party-notices.py            rewrite the generated regions in place
    python3 ci/gen-third-party-notices.py --check    fail (exit 1) if the file differs; `make check` runs this
    python3 ci/gen-third-party-notices.py --root DIR           operate on the checkout at DIR (its lock, data file and notices) instead
                                                               of the one this script sits in; the Dependabot workflow runs the trusted
                                                               script on a pull request's files this way
    python3 ci/gen-third-party-notices.py --verify-graph       also compare the data file's crate set with
                                                               `cargo tree` for the ARM runtime graph (needs cargo)
    python3 ci/gen-third-party-notices.py --verify-toolchain   also compare the std snapshot and rustc string with the
                                                               pinned toolchain's rust-src (needs rustup + that toolchain)
    python3 ci/gen-third-party-notices.py --derive-toolchain   after the pin in rust-toolchain.toml MOVED: re-derive the data file's
                                                               "toolchain" and "std_lock" from that release's rust-src (needs rustup + that
                                                               toolchain with rust-src); the Dependabot workflow runs it, `make notices` does not
    python3 ci/gen-third-party-notices.py --print-stale-pin    print the pin when it differs from the data file's, else nothing

WHAT THE OFFLINE CHECK PROVES. Every registry package in `rust-modules/Cargo.lock` is either a row in
the data file (it ships) or listed in `not_shipped` (it was looked at and does not), so a crate a bump
pulls in cannot reach the binary unclassified; every row exists in a lock; each version cell and the
`compiler_builtins` version in the prose equal the lock's; and the toolchain the data file names is the one
rust-modules/rust-toolchain.toml pins. It cannot tell which side of that line a NEW crate belongs on: that is the human act
the failure message asks for, and `--verify-graph` re-derives it from cargo. `not_shipped` is by NAME
and holds common crates (once_cell, thiserror, base64, ...) that dev and build dependencies pull in, so
only the graph proves that no shipping crate has come to depend on one: `ci.yml`'s host-lint job runs
`--check --verify-graph` on every pull request (`cargo tree --locked` resolves; it runs no build
script), and the same pass rejects a version cell that only a build or dev dependency uses. The licence
and copyright columns come from the data file, not from any lock: nothing here notices that a crate's
licence changed in a new release, so a major bump needs them re-read by a person.

THE STD HALF. The standard library is the pinned stable toolchain's PREBUILT armv7 std, so its
crates' versions are the ones that release was built from: the lock inside its rust-src,
`library/Cargo.lock`, which a host without that component does not have. `std_lock` in the data file
is the snapshot of the rows that link; the offline check trusts it, `--verify-toolchain` proves it
against rust-src when the toolchain is installed (`rustup toolchain install <channel> --profile minimal
--component rust-src`, the channel in rust-modules/rust-toolchain.toml). `compiler_builtins` is a path
member of that workspace, not a registry package, so its version is read from the lock row without a
`source` too. Moving the pin fails the offline check until `toolchain.pin` is moved with it, which
`--derive-toolchain` does: it rewrites `toolchain` and the versions in `std_lock` from the new release's
rust-src. `std_lock` names the std crates that LINK; every other registry package in std's lock is in
`std_not_shipped`. A crate the new release's lock ADDED is in neither, which is a licence decision only a
person can make, so `--derive-toolchain` writes nothing and exits 2 (like an unclassified cargo crate);
a crate that left std's lock is dropped from `std_not_shipped` silently, and one that left `std_lock` exits 1.

EXIT CODES. 0: ok (or, without --check, the file was rewritten). 1: the tree is inconsistent or stale
(a moved version in --check mode, a stale row, a moved pin). 2: a registry package in the lock has no
row and is not in `not_shipped`: a licence decision only a person can make. Nothing is written for 1
or 2 (the rewrite happens only on a consistent tree), and 2 also prints one `unclassified: a b c` line,
which `.github/workflows/dependabot-notices.yml` turns into its error message.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / 'ci'))
from rust_toolchain import pinned  # noqa: E402
DATA = ROOT / 'ci' / 'third-party-crates.json'
NOTICES = ROOT / 'THIRD-PARTY-NOTICES.md'
LOCK = ROOT / 'rust-modules' / 'Cargo.lock'
WORKFLOWS = ROOT / '.github' / 'workflows'
REGENERATE = 'python3 ci/gen-third-party-notices.py'
TOOLCHAIN_FILE = ROOT / 'rust-modules' / 'rust-toolchain.toml'
REGISTRY = 'registry+'


def parse_lock(text, path_packages=()):
    """{name: [versions]} of the registry packages in a Cargo.lock (workspace members have no `source`).

    `path_packages` names members to include anyway (std's `compiler_builtins` is one in its workspace).
    """
    found, current = {}, {}
    for line in text.splitlines() + ['[[package]]']:
        if line.startswith('[[package]]'):
            if current.get('source', '').startswith(REGISTRY) or current.get('name') in path_packages:
                found.setdefault(current['name'], []).append(current['version'])
            current = {}
        m = re.match(r'(name|version|source) = "([^"]*)"', line)
        if m:
            current[m.group(1)] = m.group(2)
    return found


def version_key(v):
    return tuple(int(p) if p.isdigit() else 0 for p in re.split(r'[.+-]', v)[:4])


def crate_versions(name, project, data):
    versions = set(project.get(name, ()))
    if name in data['std_lock']:
        versions.add(data['std_lock'][name])
    return sorted(versions, key=version_key, reverse=True)


def rustc_release(rustc):
    return re.sub(r'\s*\(.*\)$', '', rustc)


def expand(cell, data):
    tokens = {'{rustc}': data['toolchain']['rustc'], '{rustc_release}': rustc_release(data['toolchain']['rustc']),
              '{backtrace}': data['toolchain']['backtrace']}
    for token, value in tokens.items():
        cell = cell.replace(token, value)
    return cell


def render_group(group, project, data):
    cols = group['columns']
    out = ['| ' + ' | '.join(cols) + ' |', '|' + '---|' * len(cols)]
    def line(cells):
        return '| ' + ' | '.join(cells) + ' |'
    for raw in group.get('lead', []):
        out.append(line([expand(c, data) for c in raw]))
    for crate in sorted(group['crates'], key=lambda c: c['name']):
        versions = crate_versions(crate['name'], project, data)
        if not versions:
            raise SystemExit(f'{crate["name"]}: in no lock (neither rust-modules/Cargo.lock nor the std snapshot)')
        cells = [crate['name'], ' and '.join(versions)]
        if 'licence' in crate:
            cells.append(crate['licence'])
        if 'copyright' in crate:
            cells.append(crate['copyright'])
        out.append(line(cells))
    for raw in group.get('trail', []):
        out.append(line([expand(c, data) for c in raw]))
    return '\n'.join(out)


def region(gid):
    begin = f'<!-- BEGIN GENERATED notices:{gid} (edit ci/third-party-crates.json, run {REGENERATE}) -->'
    end = f'<!-- END GENERATED notices:{gid} -->'
    return begin, end


def render(text, data, project):
    """The notices text with every generated region and the compiler_builtins version rewritten."""
    for group in data['groups']:
        begin, end = region(group['id'])
        pattern = re.compile(re.escape(begin) + r'\n.*?\n' + re.escape(end), re.S)
        if not pattern.search(text):
            raise SystemExit(f'THIRD-PARTY-NOTICES.md has no "{begin}" ... "{end}" region for group {group["id"]}')
        body = f'{begin}\n\n{render_group(group, project, data)}\n\n{end}'
        text = pattern.sub(lambda _m: body, text, count=1)
    cb = data['std_lock']['compiler_builtins']
    text, n = re.subn(r'\*\*compiler_builtins \S+\*\*', f'**compiler_builtins {cb}**', text)
    if n != 1:
        raise SystemExit('THIRD-PARTY-NOTICES.md must name "**compiler_builtins <version>**" exactly once')
    return text


def workflow_pins():
    """The toolchain pin(s): the `channel` of rust-modules/rust-toolchain.toml, the repository's one pin."""
    m = re.search(r'^channel\s*=\s*"([^"]+)"', TOOLCHAIN_FILE.read_text(), re.M)
    return {m.group(1)} if m else set()


def unclassified(data, project):
    """Lock packages that are neither a row nor `not_shipped`: new crates awaiting a licence decision."""
    rows = {c['name'] for g in data['groups'] for c in g['crates']}
    return sorted(set(project) - rows - set(data['not_shipped']))


def problems_in(data, project, pins):
    problems = []
    rows = {c['name'] for g in data['groups'] for c in g['crates']}
    not_shipped = set(data['not_shipped'])
    for name in unclassified(data, project):
        problems.append(f'{name} {"/".join(project[name])} is in rust-modules/Cargo.lock but ci/third-party-crates.json neither '
                        f'gives it a row nor lists it in "not_shipped". Decide whether it reaches the ARM binary '
                        f'(`{REGENERATE} --verify-graph` shows it); a shipping crate needs a row (licence, copyright holder).')
    for name in sorted(not_shipped - set(project)):
        problems.append(f'"not_shipped" lists {name}, which rust-modules/Cargo.lock no longer has: drop it.')
    for name in sorted(not_shipped & rows):
        problems.append(f'{name} is both a row and in "not_shipped".')
    for name in sorted(rows - set(project) - set(data['std_lock'])):
        problems.append(f'row {name} is in no lock: rust-modules/Cargo.lock dropped it (remove the row, or it is a std crate '
                        f'and belongs in "std_lock").')
    seen = [c['name'] for g in data['groups'] for c in g['crates']]
    for name in sorted({n for n in seen if seen.count(n) > 1}):
        problems.append(f'{name} has more than one row.')
    if pins != {data['toolchain']['pin']}:
        problems.append(f'rust-modules/rust-toolchain.toml pins {sorted(pins)} but ci/third-party-crates.json "toolchain.pin" is '
                        f'{data["toolchain"]["pin"]}: the std crates and rustc string of the notices describe the pinned toolchain. '
                        f'Re-derive "toolchain" and "std_lock" ({REGENERATE} --verify-toolchain on that toolchain) and move the pin.')
    return problems


def set_root(root):
    """Point every path at another checkout (the code stays this script's)."""
    global ROOT, DATA, NOTICES, LOCK, WORKFLOWS, TOOLCHAIN_FILE
    ROOT = Path(root).resolve()
    DATA = ROOT / 'ci' / 'third-party-crates.json'
    NOTICES = ROOT / 'THIRD-PARTY-NOTICES.md'
    LOCK = ROOT / 'rust-modules' / 'Cargo.lock'
    WORKFLOWS = ROOT / '.github' / 'workflows'
    TOOLCHAIN_FILE = ROOT / 'rust-modules' / 'rust-toolchain.toml'


def load():
    return json.loads(DATA.read_text()), parse_lock(LOCK.read_text())


def std_lock_path(pin):
    out = subprocess.run(['rustup', 'run', pin, 'rustc', '--print', 'sysroot'], capture_output=True, text=True)
    if out.returncode != 0:
        return None
    return Path(out.stdout.strip()) / 'lib' / 'rustlib' / 'src' / 'rust' / 'library'


PIN_RE = re.compile(r'1\.\d+\.\d+')


def read_std(pin):
    """(std lock {name: [versions]}, rustc --version, backtrace version, problems) of the installed `pin` with rust-src."""
    lib = std_lock_path(pin)
    if lib is None or not (lib / 'Cargo.lock').exists():
        return None, None, None, [f'{pin} with rust-src is not installed (`rustup toolchain install {pin} --profile minimal --component rust-src`)']
    std = parse_lock((lib / 'Cargo.lock').read_text(), path_packages=('compiler_builtins',))
    rustc = subprocess.run(['rustup', 'run', pin, 'rustc', '--version'], capture_output=True, text=True).stdout.strip()
    bt = re.search(r'^version = "([^"]+)"', (lib / 'backtrace' / 'Cargo.toml').read_text(), re.M)
    return std, rustc, bt and bt.group(1), []


def std_unclassified(std, data):
    """Registry packages in std's lock that neither link (`std_lock`) nor are listed in `std_not_shipped`."""
    return sorted(set(std) - set(data['std_lock']) - set(data['std_not_shipped']))


def verify_toolchain(data):
    pin = data['toolchain']['pin']
    std, rustc, backtrace, problems = read_std(pin)
    if problems:
        return problems
    for name, version in sorted(data['std_lock'].items()):
        if version not in std.get(name, ()):
            problems.append(f'std_lock {name} {version}: the {pin} std lock has {std.get(name)}')
    for name in std_unclassified(std, data):
        problems.append(f'{name} is in the {pin} std lock but neither in "std_lock" (it links) nor in "std_not_shipped".')
    for name in sorted(set(data['std_not_shipped']) - set(std)):
        problems.append(f'"std_not_shipped" lists {name}, which the {pin} std lock no longer has: drop it.')
    if rustc != data['toolchain']['rustc']:
        problems.append(f'toolchain.rustc is "{data["toolchain"]["rustc"]}" but {pin} reports "{rustc}"')
    if backtrace != data['toolchain']['backtrace']:
        problems.append(f'toolchain.backtrace is {data["toolchain"]["backtrace"]} but {pin} vendors {backtrace}')
    return problems


def stale_pin(data):
    """The pin in rust-toolchain.toml when the data file still describes another release, else None. A pin that is
    not a plain `1.x.y` is refused: the value is a PR's, and it ends up in a `rustup toolchain install` command."""
    pins = workflow_pins()
    if len(pins) != 1 or not PIN_RE.fullmatch(next(iter(pins))):
        raise SystemExit(f'rust-modules/rust-toolchain.toml must pin one plain release (1.x.y), got {sorted(pins)}')
    pin = next(iter(pins))
    return None if pin == data['toolchain']['pin'] else pin


def rewrite_toolchain(text, pin, rustc, backtrace, std_lock, std_not_shipped):
    """The data file with its "toolchain", "std_lock" and "std_not_shipped" members replaced, in the file's own layout
    (one-space indent), so the diff holds only what moved."""
    members = {
        'toolchain': {'pin': pin, 'rustc': rustc, 'backtrace': backtrace},
        'std_lock': dict(sorted(std_lock.items())),
        'std_not_shipped': sorted(std_not_shipped),
    }
    for key, value in members.items():
        body = json.dumps(value, indent=1).replace('\n', '\n ')
        pattern = re.compile(r'^ "' + key + r'": [\{\[].*?^ [\}\]](?=,?$)', re.S | re.M)
        if len(pattern.findall(text)) != 1:
            raise SystemExit(f'ci/third-party-crates.json must hold exactly one "{key}" member in the usual layout')
        text = pattern.sub(lambda _m: f' "{key}": {body}', text, count=1)
    return text


def derive_toolchain(data, text, pin):
    """(new data-file text, problems, unclassified std crates) for the pinned release `pin`, from its rust-src."""
    std, rustc, backtrace, problems = read_std(pin)
    if problems:
        return text, problems, []
    new = {}
    for name in sorted(data['std_lock']):
        versions = std.get(name, [])
        if len(versions) != 1:
            problems.append(f'std_lock lists {name}, but the {pin} std lock has {versions or "no such crate"}: it needs a person '
                            f'(remove the row, or settle which version links).')
        else:
            new[name] = versions[0]
    if not backtrace:
        problems.append(f'cannot read the backtrace version vendored by {pin}')
    added = std_unclassified(std, data)
    if problems or added:
        return text, problems, added
    kept = sorted(set(data['std_not_shipped']) & set(std))
    return rewrite_toolchain(text, pin, rustc, backtrace, new, kept), [], []


def parse_tree(text):
    """{name: {versions}} from `cargo tree --prefix none --format '{p}'` output."""
    tree = {}
    for line in text.splitlines():
        m = re.match(r'(\S+) v(\S+)$', line.replace(' (*)', ''))
        if m:
            tree.setdefault(m.group(1), set()).add(m.group(2))
    return tree


def graph_problems(tree, data, project):
    """Compare the ARM runtime graph with the data file. Classification by name is not enough: `not_shipped`
    holds common crates (once_cell, thiserror, ...) that dev and build dependencies pull in, so a shipping
    crate that starts to depend on one is in the graph with no row; and a row's version cell is read from the
    lock, which also holds versions only a build or dev dependency uses."""
    rows = {c['name'] for g in data['groups'] for c in g['crates']}
    shipped_by_std = set(data['std_lock'])
    problems = []
    for name in sorted(set(tree) - rows):
        problems.append(f'{name} {sorted(tree[name])} is in the ARM runtime graph but has no row (it may be listed in '
                        f'"not_shipped"; a shipping crate needs a row with its licence and copyright holder)')
    for name in sorted(rows - set(tree) - shipped_by_std):
        problems.append(f'row {name} is not in the ARM runtime graph (and is not a std crate): is it still linked?')
    for name in sorted(rows & set(tree)):
        extra = set(project.get(name, ())) - tree[name]
        if extra:
            problems.append(f'{name} {sorted(extra)} is in rust-modules/Cargo.lock but not in the ARM runtime graph (only a build or '
                            f'dev dependency uses it), yet the notices table would list it')
    return problems


def verify_graph(data, project):
    cmd = ['cargo', '+' + pinned(ROOT), 'tree', '--locked', '--manifest-path', str(ROOT / 'rust-modules' / 'Cargo.toml'), '--no-default-features',
           '--target', 'armv7-unknown-linux-gnueabi', '--edges', 'normal,no-proc-macro', '--prefix', 'none', '--format', '{p}']
    out = subprocess.run(cmd, capture_output=True, text=True, env={**os.environ, 'CARGO_INCREMENTAL': '0'})
    if out.returncode != 0:
        return [f'cargo tree failed: {out.stderr.strip()[-400:]}']
    return graph_problems(parse_tree(out.stdout), data, project)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    ap.add_argument('--check', action='store_true', help='fail instead of rewriting when the file would change')
    ap.add_argument('--verify-graph', action='store_true', help='compare the crate set with `cargo tree` (needs cargo)')
    ap.add_argument('--verify-toolchain', action='store_true', help='compare the std snapshot with the pinned toolchain\'s rust-src')
    ap.add_argument('--derive-toolchain', action='store_true', help='re-derive the data file\'s toolchain and std_lock for a moved pin (needs rustup + rust-src)')
    ap.add_argument('--print-stale-pin', action='store_true', help='print the pin when the data file describes another release')
    ap.add_argument('--root', help='operate on the checkout at this directory instead of the script\'s own')
    args = ap.parse_args(argv)
    if args.root:
        set_root(args.root)
    data, project = load()
    if args.print_stale_pin:
        print(stale_pin(data) or '')
        return 0
    if args.derive_toolchain:
        pin = stale_pin(data)
        if pin is None:
            print(f'gen-third-party-notices: the data file already describes {data["toolchain"]["pin"]}: nothing to derive')
            return 0
        wanted, problems, added = derive_toolchain(data, DATA.read_text(), pin)
        if problems or added:
            print('gen-third-party-notices: FAIL')
            for p in problems:
                print(f'  {p}')
            for name in added:
                print(f'  {name} is in the {pin} std lock but is neither in "std_lock" nor in "std_not_shipped": decide whether it links '
                      f'into the ARM binary (a licence and copyright decision), then list it in the data file.')
            if added:
                print('unclassified: ' + ' '.join(added))
                return 2
            return 1
        DATA.write_text(wanted)
        print(f'gen-third-party-notices: re-derived toolchain and std_lock for {pin}')
        return 0
    problems = problems_in(data, project, workflow_pins())
    if args.verify_graph:
        problems += verify_graph(data, project)
    if args.verify_toolchain:
        problems += verify_toolchain(data)
    if problems:
        print('gen-third-party-notices: FAIL')
        for p in problems:
            print(f'  {p}')
        new = unclassified(data, project)
        if new:
            print('unclassified: ' + ' '.join(new))
            return 2
        return 1
    current = NOTICES.read_text()
    wanted = render(current, data, project)
    if wanted == current:
        print(f'gen-third-party-notices: ok (section 2.4 matches rust-modules/Cargo.lock and the {data["toolchain"]["pin"]} std snapshot)')
        return 0
    if args.check:
        print('gen-third-party-notices: FAIL')
        print('  THIRD-PARTY-NOTICES.md section 2.4 no longer matches the lockfile (a dependency version moved).')
        print(f'  Regenerate it: `make notices` (= {REGENERATE}), review the diff, commit it with the bump.')
        return 1
    NOTICES.write_text(wanted)
    print('gen-third-party-notices: rewrote THIRD-PARTY-NOTICES.md')
    return 0


if __name__ == '__main__':
    sys.exit(main())
