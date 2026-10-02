#!/usr/bin/env python3
"""The module-layer gate: rust-modules/src may only depend DOWN its target crate graph.

`ci/module-layers.ini` declares the crates the app is to be split into ("layers"), which modules
each one owns and which layers each may name. `ci/module_graph.py` reads every reference one
module makes to another out of the Rust tokens. This script fails when

  * the declared layer graph has a cycle, names an unknown layer, or lists a member that does not
    exist or twice;
  * a module of the crate belongs to no layer (a new top-level module must be placed);
  * a reference — production or `cfg(test)` — names a layer its own layer does not `use`, and
    ci/allow/layers.txt has no entry for that (file, member) pair;
  * an allowlist entry no longer matches any such reference (the list only shrinks), or the
    list's `# count:` line disagrees with its entries.

So the module graph can get no more cyclic than it is today, and each entry removed from the
allowlist is a step of docs/module-layers.md's plan landing. Exit 0 green, 1 on any failure.

    ci/check-module-layers.py              # the gate (make check-python runs it)
    ci/check-module-layers.py --report     # the remaining debt, grouped by layer edge
    ci/check-module-layers.py --cycles     # strongly connected components of the module graph
    ci/check-module-layers.py --prune      # rewrite the allowlist without its stale entries
"""
from pathlib import Path
import argparse
import collections
import configparser
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import module_graph  # noqa: E402

REPO = Path(__file__).resolve().parent.parent
SRC_REL = 'rust-modules/src'
ALLOW_HEADER = """\
# count: {count}
# The module-layer gate's MIGRATION list (ci/check-module-layers.py, docs/module-layers.md): one
# entry per (file, member) where a file names a module in a layer its own layer may not use, as
# `path<TAB>named member<TAB>plan step`. Every entry is a reference that must move before the
# crate can be split along ci/module-layers.ini; the plan step says how. It only shrinks: a fixed
# entry fails the gate as stale until removed (`ci/check-module-layers.py --prune` does that), and
# a new upward reference fails it until the code is fixed — not until a line is added here. The one
# re-keying allowed: a renamed or split file takes its entries to its new path in the same diff.
"""


class Layers:
    def __init__(self, path):
        parser = configparser.ConfigParser(interpolation=None, comment_prefixes=('#',),
                                           inline_comment_prefixes=None)
        parser.optionxform = str
        with open(path, encoding='utf-8') as f:
            parser.read_file(f)
        self.order = parser.sections()
        self.uses, self.members, self.errors = {}, {}, []
        for layer in self.order:
            section = parser[layer]
            unknown = set(section) - {'uses', 'members'}
            if unknown: self.errors.append(f'[{layer}] has unknown key(s) {sorted(unknown)}')
            self.uses[layer] = section.get('uses', '').split()
            for member in section.get('members', '').split():
                key = tuple(member.split('::')) if member != 'crate' else ()
                if key in self.members:
                    self.errors.append(f'{member} is listed in [{self.members[key]}] and [{layer}]')
                self.members[key] = layer
        for layer, uses in self.uses.items():
            for used in uses:
                if used not in self.uses: self.errors.append(f'[{layer}] uses unknown layer {used}')
                if used == layer: self.errors.append(f'[{layer}] uses itself')
        adjacency = {layer: set(uses) & set(self.uses) for layer, uses in self.uses.items()}
        for comp in module_graph.sccs(set(self.uses), adjacency):
            if len(comp) > 1: self.errors.append(f'layer cycle: {" ".join(comp)}')

    def member_of(self, module):
        """The longest listed prefix of `module` (a tuple), or None. `crate` (the empty path) owns
        lib.rs's own items only — it is never a catch-all for an unplaced module."""
        for k in range(len(module), 0, -1):
            if module[:k] in self.members: return module[:k]
        return () if module == () and () in self.members else None


def member_name(member):
    return module_graph.name(member)


def violations(crate, layers):
    """{(file, member name): [Ref...]} for every reference that names a layer it may not use."""
    found = collections.defaultdict(list)
    for ref in crate.refs:
        source, target = layers.member_of(ref.source), layers.member_of(ref.target)
        if source is None or target is None: continue  # reported as unplaced
        a, b = layers.members[source], layers.members[target]
        if a != b and b not in layers.uses[a]:
            found[(f'{SRC_REL}/{ref.file}', member_name(target))].append(ref)
    return found


def config_errors(crate, layers):
    errors = list(layers.errors)
    for member in layers.members:
        if member not in crate.modules:
            errors.append(f'{member_name(member)} is listed in [{layers.members[member]}] but is not a module of the crate')
    for module in sorted(crate.modules):
        if layers.member_of(module) is None:
            errors.append(f'module {member_name(module)} belongs to no layer: add it (or its top-level module) to ci/module-layers.ini')
    return errors


def read_allowlist(path):
    """(declared count, {(path, member): reason}, [malformed lines])."""
    declared, entries, bad = None, {}, []
    if not path.exists(): return 0, entries, bad
    for number, line in enumerate(path.read_text(encoding='utf-8').splitlines(), 1):
        if number == 1 and line.startswith('# count:'):
            declared = int(line.split(':', 1)[1]); continue
        if not line.strip() or line.lstrip().startswith('#'): continue
        fields = line.split('\t')
        if len(fields) != 3 or not all(fields):
            bad.append(f'{path.name}:{number}: want path<TAB>member<TAB>plan step'); continue
        key = (fields[0], fields[1])
        if key in entries: bad.append(f'{path.name}:{number}: duplicate entry {fields[0]} {fields[1]}')
        entries[key] = fields[2]
    return declared, entries, bad


def write_allowlist(path, entries):
    lines = [ALLOW_HEADER.format(count=len(entries))]
    for (file, member), reason in sorted(entries.items()):
        lines.append(f'{file}\t{member}\t{reason}\n')
    path.write_text(''.join(lines), encoding='utf-8')


def describe(refs, limit=4):
    shown = ', '.join(f'{r.file}:{r.line} {r.path}{" (test)" if r.test else ""}' for r in refs[:limit])
    return shown + (f', +{len(refs) - limit} more' if len(refs) > limit else '')


def report(crate, layers, found, allow):
    by_edge = collections.defaultdict(list)
    for (file, member), refs in found.items():
        source = layers.members[layers.member_of(refs[0].source)]
        by_edge[(source, member)].append((file, refs))
    production = sum(1 for refs in found.values() for r in refs if not r.test)
    print(f'{len(found)} (file, member) entries, {sum(len(r) for r in found.values())} references '
          f'({production} production)\n')
    for (source, member), rows in sorted(by_edge.items(), key=lambda kv: (layers.order.index(kv[0][0]), kv[0][1])):
        refs = [r for _, rs in rows for r in rs]
        items = collections.Counter('::'.join(r.path.split('::')[:len(r.target) + 1]) for r in refs)
        steps = sorted({allow.get((f, member), 'NEW').split(' ')[0] for f, _ in rows})
        target = layers.members[layers.member_of(refs[0].target)]
        print(f'[{source}] -> {member} [{target}]: {len(refs)} refs in {len(rows)} files ({", ".join(steps)})')
        print('    ' + ', '.join(f'{k} x{v}' for k, v in items.most_common(6)))
    # A layer can become its own crate once neither it nor anything below it names upward.
    blocked = collections.Counter(layers.members[layers.member_of(refs[0].source)] for refs in found.values())
    print('\nextractable as a crate (no entry here or in any layer it uses, transitively):')
    for layer in layers.order:
        below, pending = set(), [layer]
        while pending:
            for used in layers.uses[pending.pop()]:
                if used not in below: below.add(used); pending.append(used)
        waiting = blocked[layer] + sum(blocked[b] for b in below)
        print(f'    {layer:10} ' + ('ready' if not waiting else
              f'{blocked[layer]} own entr{"y" if blocked[layer] == 1 else "ies"}, {waiting - blocked[layer]} below'))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--src', type=Path, default=REPO / SRC_REL)
    parser.add_argument('--layers', type=Path, default=REPO / 'ci' / 'module-layers.ini')
    parser.add_argument('--allow', type=Path, default=REPO / 'ci' / 'allow' / 'layers.txt')
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--report', action='store_true', help='print the remaining debt by layer edge')
    mode.add_argument('--cycles', action='store_true', help='print the module graph\'s cycles')
    mode.add_argument('--prune', action='store_true', help='drop stale allowlist entries')
    args = parser.parse_args(argv)

    try:
        crate = module_graph.Crate(args.src)
        layers = Layers(args.layers)
    except (ValueError, OSError, configparser.Error) as error:
        print(f'::error::check-module-layers: {error}')
        return 1
    found = violations(crate, layers)
    declared, allow, malformed = read_allowlist(args.allow)

    if args.cycles:
        # Nodes are the config's members — the units the split moves — so `diag::scrub` and the
        # rest of `diag` are two nodes, not one. An unplaced module stands for itself.
        def unit(module):
            member = layers.member_of(module)
            return module_graph.name(member if member is not None else module[:1])
        for include_test in (False, True):
            adjacency, nodes = collections.defaultdict(set), set()
            for ref in crate.refs:
                if ref.test and not include_test: continue
                a, b = unit(ref.source), unit(ref.target)
                nodes |= {a, b}
                if a != b: adjacency[a].add(b)
            cycles = [c for c in module_graph.sccs(nodes, adjacency) if len(c) > 1]
            label = 'with cfg(test)' if include_test else 'production'
            print(f'{label}: ' + ('; '.join(f'{len(c)} of {len(nodes)} units: {" ".join(c)}' for c in cycles) or 'acyclic'))
        return 0
    if args.report:
        report(crate, layers, found, allow)
        return 0
    if args.prune:
        kept = {key: reason for key, reason in allow.items() if key in found}
        write_allowlist(args.allow, kept)
        print(f'check-module-layers: pruned {len(allow) - len(kept)} stale entr(y/ies), {len(kept)} remain')
        return 0

    errors = config_errors(crate, layers) + malformed
    for key, refs in sorted(found.items()):
        if key not in allow:
            file, member = key
            source = layers.members[layers.member_of(refs[0].source)]
            target = layers.members[layers.member_of(refs[0].target)]
            errors.append(f'{file} names {member} ([{target}]), which [{source}] may not use: {describe(refs)}')
    for key in sorted(set(allow) - set(found)):
        errors.append(f'stale allowlist entry {key[0]}\t{key[1]} — it no longer names that layer; '
                      f'remove it (ci/check-module-layers.py --prune), or, if the file was renamed or '
                      f'split, move the entry to the new path')
    if declared != len(allow) + len(malformed):
        errors.append(f'{args.allow.name} declares count {declared} but has {len(allow) + len(malformed)} entries')

    for error in errors:
        print(f'::error::check-module-layers: {error}')
    if errors:
        print(f'check-module-layers: {len(errors)} failure(s). A reference may only name its own layer or one '
              f'it `uses` in ci/module-layers.ini; see docs/module-layers.md for where code belongs.')
        return 1
    print(f'check-module-layers: green — {len(layers.order)} layers, {len(crate.modules)} modules, '
          f'{len(allow)} migration entries left in {args.allow.relative_to(REPO) if args.allow.is_relative_to(REPO) else args.allow}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
