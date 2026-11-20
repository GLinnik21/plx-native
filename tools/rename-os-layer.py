#!/usr/bin/env python3
"""Rename the bottom layer crate `plx_platform` to `plx_os` (Wave R of the platform-ports plan).

    python3 -I tools/rename-os-layer.py              apply (idempotent)
    python3 -I tools/rename-os-layer.py --reverse    undo the rewrites the forward pass makes
    python3 -I tools/rename-os-layer.py --check      list every old-name site left; exit 1 on any
    python3 -I tools/rename-os-layer.py --equivalence BASE
                                                     prove HEAD == script(BASE) up to a printed list
    python3 -I tools/rename-os-layer.py --reverse-patch FILE [--output OUT]
                                                     rewrite a post-rename patch (`git diff`/`format-patch` output)
                                                     to the old names so it applies on a release line
    --root DIR   run against DIR instead of the checkout this file lives in (a scratch copy, an
                 open PR's worktree, a release line); DIR need not be a git repository.

WHAT IT DOES. Moves `rust-modules/platform/` to `rust-modules/os/` (`git mv` inside a repository, a
plain rename elsewhere) and rewrites, in every text file that is not exempt:
  * the package `plx_platform` -> `plx_os` (manifests, `Cargo.lock`, Rust, Makefile, CI, docs);
  * the path `rust-modules/platform` -> `rust-modules/os`, the bare paths relative to `rust-modules/`
    (`platform/src/..`, `../platform/..`, `path = "../platform"`) and the directory mentions
    `` `platform/` ``;
  * the layer token: the section `[platform]`, the token in a `uses` list, `` `platform` `` and
    `platform::<member>` in prose, and the quoted `"platform"` in a list of layer names;
  * the fence keyword `[port X]` -> `[platform X]` (and the gate's `startswith('port ')`);
  * `SRC_PLATFORM` -> `SRC_OS`, the build-bench scenario id, the cargo-seed manifest part.
It does NOT run rustfmt, does not touch `Cargo.lock`'s order (cargo re-sorts it), and leaves the two
reviewed certificates (`ci/proofs/`) and their pins to be regenerated and re-pinned by hand.
Prose that names the layer ("the platform layer", a table cell, a heading, a list of layers) is rewritten by
the enumerated per-file sites in LITERALS: a sweep of every standalone `platform` found them one by one. A
`platform` that is the host platform, a Plex or Sentry field, `sys.platform` or the webOS platform is left alone.

`--reverse` rewrites only the sites the forward pass writes: a backport cherry-pick written after the
rename can be turned back into the old names to apply on a release line. Its weak patterns (a
backticked `os`, a quoted "os") are skipped where `os` already means the operating system.

`--reverse-patch` is the backport route: a fix written after the rename has hunks in the new names and
paths; each hunk line is rewritten with the reverse rules keyed by the file it belongs to (the enumerated
sites are per file), so `git apply` accepts it on a line that still carries the old names. Run `git
apply --check` on the result before `git apply`. A weak pattern (a bare `os` that may be the operating
system) can turn a natural `os` into `platform` in a file `--reverse` does not know: every line one
changes is printed to standard error as `file:line: old -> new`, for the person applying the patch to read.

`--check` fails on any hit of the three spellings (the package; the path, in its bare and `../`
forms, also after a `/`; the layer token, also unquoted in a list written with spaces, commas or
arrows between layer names unless SEP_KEEP names the hit, as a table cell, or before `layer`/`crate`),
on every enumerated literal site the forward pass rewrites, and on any
quoted `"platform"` that is not classified (a JSON key, a Sentry field, `sys.platform`): a quoted
`platform` in a list of layer names is never a keeper, because `walk` over a missing directory
passes silently (Fable N1). It is a PR gate, not a standing CI step: later waves legitimately use
the word `platform` (`plx_platform_webos`, `[platform webos]`).

`--equivalence BASE` applies the forward pass to a copy of BASE and the reverse pass to a copy of
HEAD and compares each with the other side. Only these differences are allowed, and they are
printed: (a) the two certificates and the hash lines that pin them, (b) `rust-modules/Cargo.lock`
as a sorted-line multiset (the renamed packages re-sort, no version moves), (c) a `.rs` hunk whose
tokens are the same multiset (a reflow or `use` reorder the shorter name causes), (d) this tool, its
test and its two lines (a comment and the step) in `ci/check-python-steps.txt`. Anything else fails.

Stdlib only; run with -I.
"""
import argparse
import difflib
import io
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

OLD_DIR, NEW_DIR = 'rust-modules/platform', 'rust-modules/os'
SELF = 'tools/rename-os-layer.py'
SELF_TEST = 'ci/test_rename_os_layer.py'
STEPS_FILE = 'ci/check-python-steps.txt'
STEPS_LINE = 'python3 ci/test_rename_os_layer.py'
STEPS_COMMENT = '# tools/rename-os-layer.py'

# Dated records and the plan keep the old name on purpose. `ci/proofs/` holds the two reviewed
# certificates: they are regenerated by their producers, never rewritten, but `--check` reads them.
EXEMPT = [
    r'^docs/release-audits/',
    r'^docs/measurements/',
    r'^docs/(?:.*/)?[^/]*-20\d\d-\d\d-\d\d[^/]*\.md$',
    r'^docs/platform-ports\.md$',  # W0's copy of the plan: its fact sections quote the old names
]
CERTIFICATES = ('ci/proofs/platform-generated-enums.json', 'ci/proofs/serde-json-immutable.json')
PIN_FILES = ('ci/readonly_generated.py', 'ci/readonly_dependencies.py')
LOCK = 'rust-modules/Cargo.lock'

# Files where `os` already means the operating system: the weak reverse patterns skip them.
NATURAL_OS = (
    'docs/disk-budget.md', 'tools/disk-budget.py',
    'rust-modules/telemetry/src/telemetry/crashreport.rs', 'rust-modules/telemetry/src/telemetry/sentry.rs',
)
SECTION_FILES = ('ci/module-layers.ini', 'docs/module-layers.md', 'rust-modules/Cargo.toml')
LAYERS = ('base', 'machine', 'net', 'gfx', 'ui', 'plex', 'telemetry', 'data', 'session', 'media', 'appkit', 'screens')
MEMBERS = 'storage|i18n|webos|tv|keymanager|devcaps|imgcache|labcfg|playback_trace'

PKG_OLD = re.compile(r'plx_platform(?![A-Za-z0-9_])')
PKG_NEW = re.compile(r'plx_os(?![A-Za-z0-9_])')
DIR_OLD = re.compile(r'rust-modules/platform(?![A-Za-z0-9_-])')
DIR_NEW = re.compile(r'rust-modules/os(?![A-Za-z0-9_-])')
BARE_FOLLOW = r'(?=(?:src|build_support|i18n)(?![A-Za-z0-9_-])|build\.rs|Cargo\.toml)'
# A bare path starts after a character that cannot continue a name or another path; the one exception
# is a `/` directly before a `../` (`"/../platform/src"`, a joined path), which still reaches the layer.
BARE_START = r'(?:(?<![A-Za-z0-9_./~-])|(?<=/)(?=\.\./))'
BARE_OLD = re.compile(BARE_START + r'((?:\.\./)*)platform/' + BARE_FOLLOW)
BARE_NEW = re.compile(BARE_START + r'((?:\.\./)*)os/' + BARE_FOLLOW)
UP_OLD = re.compile(BARE_START + r'((?:\.\./)+)platform(?=["\'])')
UP_NEW = re.compile(BARE_START + r'((?:\.\./)+)os(?=["\'])')
DIRLIST_OLD, DIRLIST_NEW = '`platform/`', '`os/`'
TICK_OLD, TICK_NEW = '`platform`', '`os`'
SECTION_OLD, SECTION_NEW = '[platform]', '[os]'
RUN_OLD = re.compile(r'\b(base|machine) platform\b')
RUN_NEW = re.compile(r'\b(base|machine) os\b')
# `platform` beside a layer name with a space, comma, `>` or `-` between them: `for c in net platform gfx`,
# `--only machine,platform`, `base -> platform -> ui`. Every hit is a site unless SEP_KEEP names it.
SEP_OLD = re.compile(r'\b(?:%s)\b[ ,>-]+platform\b|\bplatform\b[ ,>-]+(?:%s)\b' % (('|'.join(LAYERS),) * 2))
CELL_OLD = re.compile(r'\|\s*platform\s*\|')  # a markdown table cell that is the layer's name: `| platform | members |`
NOUN_OLD = re.compile(r"\bplatform(?:'s)? (?:layer|crate)\b")  # `the platform layer`, `the platform crate's build script`
ROW_OLD = re.compile(r'(\+ )platform(?= (?:machine )?base\b)')  # the `+ platform machine base` column of the layer table
ROW_NEW = re.compile(r'(\+ )os(?= (?:machine )?base\b)')
MODPATH_OLD = re.compile(r'(?<![A-Za-z0-9_:])platform::(?=(?:' + MEMBERS + r')\b)')
MODPATH_NEW = re.compile(r'(?<![A-Za-z0-9_:])os::(?=(?:' + MEMBERS + r')\b)')
SRC_OLD, SRC_NEW = re.compile(r'\bSRC_PLATFORM\b'), re.compile(r'\bSRC_OS\b')
FENCE_OLD = re.compile(r'\[port (?=[^\]\s]+\])')
FENCE_NEW = re.compile(r'\[platform (?=[^\]\s]+\])')
QUOTED = re.compile(r'''(["'])platform\1''')
QUOTED_NEW = re.compile(r'''(["'])os\1''')

# (file, old, new): one enumerated site each. `--check` flags `old` in `file` too.
LITERALS = [
    ('ci/check-module-layers.py', "startswith('port ')", "startswith('platform ')"),
    ('ci/check-module-layers.py', "len('port ')", "len('platform ')"),
    ('rust-modules/Cargo.toml', 'net/, platform/, gfx/', 'net/, os/, gfx/'),
    ('rust-modules/Cargo.toml', '"platform"', '"os"'),  # `members` and the two `path = "platform"`
    ('Makefile', 'base -> platform -> ui/plex', 'base -> os -> ui/plex'),
    ('ci/check-deps.sh', 'platform holds forks', 'os holds forks'),
    ('tools/cargo-seed.py', '"rust-modules", "platform", "Cargo.toml"', '"rust-modules", "os", "Cargo.toml"'),
    ('tools/cargo-seed.py', 'part("manifest-platform"', 'part("manifest-os"'),
    ('tools/build-bench.py', '  platform  ...after appending', '  os        ...after appending'),
    ('tools/build-bench.py', '"platform": "Edit leaf', '"os": "Edit leaf'),
    ('ci/test_build_bench.py', "art('/platform',", "art('/os',"),
    ('ci/test_build_bench.py', 'machine,platform,gfx', 'machine,os,gfx'),
    ('ci/test_build_bench.py', 'by_id["platform"]', 'by_id["os"]'),
    # The layer named without a quote, a backtick or a `base`/`machine` neighbour the general rules read:
    # the layer graph's own row (`os` keeps the `machine base` column where it was) and two layer lists.
    ('docs/module-layers.md', 'platform' + ' ' * 77 + 'machine base', 'os' + ' ' * 83 + 'machine base'),
    ('docs/watchlist-plan.md', 'base, machine, platform. NOT net', 'base, machine, os. NOT net'),
    ('rust-modules/base/src/eventlog/scrub.rs', 'machine, net, platform, gfx, ui', 'machine, net, os, gfx, ui'),
    # The sweep of every standalone `platform`/`Platform` left after the general rules: each hit that names
    # the layer (its crate, a table cell, a heading, the layer's own constants and locals) and none that
    # is the host platform, a Plex or Sentry field or `sys.platform`. Where one file mixes `platform's` and
    # `the platform`, the longer entry comes first, so `--reverse` restores each form exactly.
    ('.claude/hooks/release-config-check-test.py', "the platform layer crate's source", "the os layer crate's source"),
    ('Makefile', 'of the platform layer crate', 'of the os layer crate'),
    ('ci/check-localization.py', 'platform = root / LAYER_SRCS[0]', 'os_src = root / LAYER_SRCS[0]'),
    ('ci/check-localization.py', 'yield platform / rel', 'yield os_src / rel'),
    ('ci/list-caps.ini', '# ---- platform ' + '-' * 60, '# ---- os ' + '-' * 60),
    ('ci/readonly_dependencies.py', 'or the platform enum producer', 'or the os enum producer'),
    ('ci/test_build_bench.py', 'PLATFORM_LEAF = "rust-modules/os/src/devcaps.rs"', 'OS_LEAF = "rust-modules/os/src/devcaps.rs"'),
    ('ci/test_build_bench.py', 'PLATFORM_LEAF: "pub fn platform() {}\\n"', 'OS_LEAF: "pub fn os() {}\\n"'),
    ('ci/test_build_not_always_dirty.py', "belong to the platform crate's build script", "belong to the os crate's build script"),
    ('ci/test_build_not_always_dirty.py', 'of the platform crate, whose build', 'of the os crate, whose build'),
    ('ci/test_build_not_always_dirty.py', 'PLATFORM_CRATE', 'OS_CRATE'),
    ('ci/test_storage_package_isolated.py', 'source files with the platform layer crate', 'source files with the os layer crate'),
    ('tools/build-bench.py', 'leaf file of the PLATFORM layer crate', 'leaf file of the os layer crate'),
    ('tools/build-bench.py', 'stay fresh, the platform crate and the app crate', 'stay fresh, the os crate and the app crate'),
    ('tools/build-bench.py', 'PLATFORM_LEAF_FILE', 'OS_LEAF_FILE'),
    ('tools/build-bench.py', 'PLATFORM_PACKAGE', 'OS_PACKAGE'),
    ('.agents/agents/fw-compat-reviewer.md', 'with the platform feature `uri-transport`', "with the os crate's feature `uri-transport`"),
    ('docs/module-layers.md', "platform's `i18n storage webos`", "os's `i18n storage webos`"),
    ('docs/module-layers.md', 'L15 took the platform\n', 'L15 took the os\n'),
    ('docs/module-layers.md', '| platform | `webos storage', '| os | `webos storage'),
    ('docs/module-layers.md', 'machine sits below platform.', 'machine sits below os.'),
    ('docs/module-layers.md', 'lab config to platform', 'lab config to os'),
    ('docs/module-layers.md', 'are the platform module `labcfg`', 'are the os module `labcfg`'),
    ('docs/module-layers.md', 'platform owns its types', 'os owns its types'),
    ('docs/module-layers.md', '### Split 3: platform', '### Split 3: os'),
    ('docs/module-layers.md', 'the platform crate cannot see it', 'the os crate cannot see it'),
    ('docs/module-layers.md', 'The platform layer does not know', 'The os layer does not know'),
    ('docs/module-layers.md', 'it rebuilds the platform crate', 'it rebuilds the os crate'),
    ('docs/watchlist-plan.md', '| `os::i18n` | platform |', '| `os::i18n` | os |'),
    ('docs/sans-io.md', 'the actual platform language include', 'the actual os language include'),
    ('docs/playback-diagnostics.md', 'The platform adapter owns native logging', "The os layer's adapter owns native logging"),
    ('rust-modules/Cargo.toml', 'The platform layer (ci/module-layers.ini `[os]`)', 'The os layer (ci/module-layers.ini `[os]`)'),
    ('rust-modules/Cargo.toml', "Likewise the platform layer's seams", "Likewise the os layer's seams"),
    ('rust-modules/build.rs', "generated by the platform layer's own build script", "generated by the os layer's own build script"),
    ('rust-modules/storage/Cargo.toml', 'the platform layer crate (`os/src/storage_service', 'the os layer crate (`os/src/storage_service'),
    ('rust-modules/storage/build.rs', "the platform crate's own build script uses", "the os crate's own build script uses"),
    ('rust-modules/os/Cargo.toml', 'PlxNative platform layer: what this television', 'PlxNative os layer: what this television'),
    ('rust-modules/os/build.rs', 'Code generation for the platform layer:', 'Code generation for the os layer:'),
    ('rust-modules/os/src/devcaps.rs', '`plex` sits above the platform layer;', '`plex` sits above the os layer;'),
    ('rust-modules/os/src/i18n/mod.rs', 'here in the platform layer and never in a machine', 'here in the os layer and never in a machine'),
    ('rust-modules/os/src/i18n/mod.rs', "this is the platform layer's read of the OS time zone", "this is the os layer's read of the OS time zone"),
    ('rust-modules/os/src/storage/state.rs', '(the platform crate and the storage helper)', '(the os crate and the storage helper)'),
    ('rust-modules/src/app/adapters/stores.rs', 'as the platform layer reads it from the OS', 'as the os layer reads it from the OS'),
    ('rust-modules/src/app/boot.rs', "and platform's `storage_service/`", "and the os layer's `storage_service/`"),
    ('rust-modules/src/lab/mod.rs', 'live in `plx_os::labcfg` (platform):', 'live in `plx_os::labcfg` (the os layer):'),
    ('rust-modules/plex/src/plex/CLAUDE.md', "to platform's `ACCOUNT_BOUND_PREFERENCES`", "to the os layer's `ACCOUNT_BOUND_PREFERENCES`"),
    ('rust-modules/plex/src/plex/CLAUDE.md', "(platform's own\n", "(the os layer's own\n"),
    ('rust-modules/plex/src/plex/CLAUDE.md', 'with nothing sheddable left the platform still does', 'with nothing sheddable left the os layer still does'),
    ('rust-modules/plex/src/plex/transcoder.rs', '(the platform layer intersects it', '(the os layer intersects it'),
    ('rust-modules/plex/src/plex/session/persistence.rs', 'the platform', 'the os layer'),
    ('rust-modules/plex/src/plex/session/persistence.rs', 'The platform', 'The os layer'),
    ('rust-modules/plex/src/plex/session/prefs.rs', "to platform's account-bound list", "to the os layer's account-bound list"),
    ('rust-modules/plex/src/plex/session/prefs.rs', "(platform's counting rule)", "(the os layer's counting rule)"),
    ('rust-modules/plex/src/plex/session/prefs/track_memory.rs', "(platform's counting rule)", "(the os layer's counting rule)"),
    ('rust-modules/plex/src/plex/session/tier1_characterisation_tests.rs', 'fed through the platform state codec', "fed through the os layer's state codec"),
    ('rust-modules/plex/src/plex/session/tier1_characterisation_tests.rs', 'through the platform codec', "through the os layer's codec"),
    ('rust-modules/plex/src/plex/session/tier1_characterisation_tests.rs', 'to the platform codec', "to the os layer's codec"),
    ('rust-modules/plex/src/plex/session/tier1_characterisation_tests.rs', 'the platform codec, then', "the os layer's codec, then"),
    ('rust-modules/plex/src/plex/session/prefs_tests.rs', "account-bound keys equal platform's", "account-bound keys equal the os layer's"),
    ('rust-modules/plex/src/plex/session/prefs_tests.rs', '**T-account.** Platform names', '**T-account.** The os layer names'),
    ('rust-modules/plex/src/plex/session/prefs_tests.rs', 'every key platform\n', 'every key the os layer\n'),
    ('rust-modules/plex/src/plex/session/prefs_tests.rs', 'that platform does not clear', 'that the os layer does not clear'),
    ('rust-modules/plex/src/plex/session/prefs_tests.rs', 'let platform: BTreeSet<&str>', 'let os_bound: BTreeSet<&str>'),
    ('rust-modules/plex/src/plex/session/prefs_tests.rs', 'assert_eq!(declared, platform, "account-bound rows and platform\'s ACCOUNT_BOUND_PREFERENCES',
     'assert_eq!(declared, os_bound, "account-bound rows and the os layer\'s ACCOUNT_BOUND_PREFERENCES'),
    ('rust-modules/plex/src/plex/session/prefs_tests.rs', '// Every key platform clears', '// Every key the os layer clears'),
    ('rust-modules/plex/src/plex/session/prefs_tests.rs', 'for key in &platform {', 'for key in &os_bound {'),
    ('rust-modules/plex/src/plex/session/prefs_tests.rs', '"{key}: platform clears it at sign-out', '"{key}: the os layer clears it at sign-out'),
    ('rust-modules/plex/src/plex/session/prefs_tests.rs', '!platform.contains("auto_sign_in")', '!os_bound.contains("auto_sign_in")'),
    ('rust-modules/plex/src/plex/session/prefs/track_memory_document_tests.rs', "the guard against platform's OWN code", "the guard against the os layer's OWN code"),
    ('rust-modules/plex/src/plex/session/prefs/track_memory_document_tests.rs', "and platform's own apply", "and the os layer's own apply"),
    ('rust-modules/plex/src/plex/session/prefs/track_memory_document_tests.rs', "the projection is platform's own document", "the projection is the os layer's own document"),
    ('rust-modules/plex/src/plex/session/prefs/track_memory_document_tests.rs', 'through the platform codec', "through the os layer's codec"),
    ('rust-modules/plex/src/plex/session/prefs/track_memory_document_tests.rs', 'the platform', 'the os layer'),
    ('rust-modules/plex/src/plex/session/prefs/track_memory_document_tests.rs', 'The platform', 'The os layer'),
]

# A quoted `platform` that is not the layer. Never a keeper if its line also looks like a layer list.
KEEP = [
    (r'^docs/plex-openapi\.json$', 'PMS OpenAPI JSON key'),
    (r'^site/media/[^/]*\.manifest\.json$', 'site render manifest field'),
    (r'^rust-modules/telemetry/src/', 'Sentry event `platform` field'),
    (r'^ci/patches/sentry-native-[^/]*\.patch$', 'sentry-native event `platform` field'),
    (r'^tests/test_replay_fixtures\.py$', 'sys.platform mock'),
    (r'^tests/test_site_(?:stills|video)\.py$', 'site render manifest field'),
    (r'^tools/(?:site_stills|site_video|hud_bundle)\.py$', 'render/bundle manifest field'),
]


# (file, line regex, label): a separator-list hit where `platform` is not the layer. Both must match.
SEP_KEEP = [
    (r'^docs/plex-api-design\.md$', r'client_id, product, version, platform, session,', 'Plex client-identification field list'),
    (r'^rust-modules/session/src/auth/owner\.rs$', r'machine, platform API', 'prose: a platform API'),
]


def quoted_layers(line, name):
    return {layer for layer in LAYERS if re.search(r'''["']%s["']''' % layer, line)} - {name}


def head_name(rel):
    """`rel` as the head tree names it. The per-file tables (LITERALS, NATURAL_OS, SECTION_FILES) are keyed by head
    paths, but a reverse run (and a forward dry run) reads files that still sit under `rust-modules/platform/`."""
    return NEW_DIR + rel[len(OLD_DIR):] if rel.startswith(OLD_DIR + '/') else rel


def rewrite(text, rel, reverse, weak=True):
    """The forward (or reverse) rewrite of one file's text; the same text back when nothing applies.
    `weak=False` (reverse only) skips the patterns whose new form is the bare word `os` (a backticked
    `os`, a quoted "os" in a layer list, `base os`, `+ os machine base`, `os::member`, `os/` in a
    directory list): `reverse_patch` runs both ways and names every line the weak patterns change."""
    rel = head_name(rel)
    natural = rel in NATURAL_OS
    if not reverse:
        text = PKG_OLD.sub('plx_os', text)
        text = DIR_OLD.sub(NEW_DIR, text)
        text = BARE_OLD.sub(lambda m: m.group(1) + 'os/', text)
        text = UP_OLD.sub(lambda m: m.group(1) + 'os', text)
        text = text.replace(DIRLIST_OLD, DIRLIST_NEW).replace(TICK_OLD, TICK_NEW).replace(SECTION_OLD, SECTION_NEW)
        text = RUN_OLD.sub(r'\1 os', text)
        text = ROW_OLD.sub(r'\1os', text)
        text = MODPATH_OLD.sub('os::', text)
        text = SRC_OLD.sub('SRC_OS', text)
        text = FENCE_OLD.sub('[platform ', text)
        text = ''.join(QUOTED.sub(r'\1os\1', line) if len(quoted_layers(line, 'platform')) >= 2 else line
                       for line in text.splitlines(keepends=True))
        for path, old, new in LITERALS:
            if path == rel: text = text.replace(old, new)
        return text
    # Reverse. The fence keyword goes back BEFORE `[os]` becomes `[platform]`: they share a word.
    text = FENCE_NEW.sub('[port ', text)
    for path, old, new in LITERALS:
        if path == rel: text = text.replace(new, old)
    if not natural and weak:
        text = ''.join(QUOTED_NEW.sub(r'\1platform\1', line) if len(quoted_layers(line, 'os')) >= 2 else line
                       for line in text.splitlines(keepends=True))
        text = text.replace(TICK_NEW, TICK_OLD)
    if rel in SECTION_FILES: text = text.replace(SECTION_NEW, SECTION_OLD)
    if weak:
        text = RUN_NEW.sub(r'\1 platform', text)
        text = ROW_NEW.sub(r'\1platform', text)
        text = MODPATH_NEW.sub('platform::', text)
    text = SRC_NEW.sub('SRC_PLATFORM', text)
    if weak: text = text.replace(DIRLIST_NEW, DIRLIST_OLD)
    text = UP_NEW.sub(lambda m: m.group(1) + 'platform', text)
    text = BARE_NEW.sub(lambda m: m.group(1) + 'platform/', text)
    text = DIR_NEW.sub(OLD_DIR, text)
    return PKG_NEW.sub('plx_platform', text)


def is_exempt(rel):
    return rel in (SELF, SELF_TEST) or any(re.search(pattern, rel) for pattern in EXEMPT)


def list_files(root):
    """Tracked regular files (any regular file outside `.git` when `root` is not a repository)."""
    if (root / '.git').exists():
        out = subprocess.run(['git', '-C', str(root), 'ls-files', '-z'], capture_output=True, check=True).stdout
        names = [n for n in out.decode('utf-8').split('\0') if n]
    else:
        names = [str(p.relative_to(root)) for p in sorted(root.rglob('*')) if '.git' not in p.relative_to(root).parts]
    return sorted(n for n in names if (root / n).is_file() and not (root / n).is_symlink())


def read_text(path):
    try: return path.read_bytes().decode('utf-8')
    except UnicodeDecodeError: return None


def move_directory(root, reverse):
    src, dst = (NEW_DIR, OLD_DIR) if reverse else (OLD_DIR, NEW_DIR)
    if not (root / src).is_dir(): return False
    if (root / dst).exists(): raise SystemExit(f'both {src} and {dst} exist: resolve by hand, nothing moved')
    if (root / '.git').exists():
        subprocess.run(['git', '-C', str(root), 'mv', src, dst], check=True)
    else:
        (root / src).rename(root / dst)
    return True


def apply(root, reverse, dry_run=False):
    moved = False if dry_run else move_directory(root, reverse)
    files = changed = 0
    for rel in list_files(root):
        if is_exempt(rel) or rel in CERTIFICATES: continue
        text = read_text(root / rel)
        if text is None: continue
        files += 1
        new = rewrite(text, rel, reverse)
        if new != text:
            changed += 1
            if not dry_run: (root / rel).write_bytes(new.encode('utf-8'))
            else: print(rel)
    print(f"{'reverse' if reverse else 'forward'}: {'moved the directory, ' if moved else ''}"
          f"{changed} of {files} text files {'would change' if dry_run else 'rewritten'}")
    return 0


# ---- --reverse-patch -----------------------------------------------------------------------

def reverse_patch(text, report=None):
    """The unified diff `text`, written in the new names, rewritten to the old names. Each hunk line is
    rewritten on its own (no rule spans lines), so every hunk header's line counts still hold. When
    `report` is a list, every hunk line a weak pattern changed is appended as `file:line: old -> new`
    (line = the new-side number, the old-side one for a `-` line): those rewrites turn an `os` that may
    be the operating system back into `platform`, so the person applying the patch reads each one."""
    out, rel, old_no, new_no, old_left, new_left = [], None, 0, 0, 0, 0
    for line in text.splitlines(keepends=True):
        header = re.match(r'diff --git a/(\S+) b/(\S+)', line)  # no hunk line starts with `diff` or `@@`
        hunk = re.match(r'@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@', line)
        in_hunk = old_left > 0 or new_left > 0
        if header:
            rel, old_left, new_left = header.group(2), 0, 0
            out.append(rewrite(line, rel, True))
        elif hunk:
            old_no, new_no = int(hunk.group(1)), int(hunk.group(3))
            old_left, new_left = int(hunk.group(2) or 1), int(hunk.group(4) or 1)
            out.append(line)
        elif rel is not None and not in_hunk and line.startswith(('--- ', '+++ ', 'rename from ', 'rename to ', 'copy from ', 'copy to ')):
            out.append(rewrite(line, rel, True))
        elif rel is not None and in_hunk and line[:1] in (' ', '+', '-'):
            body = line[1:]
            rewritten = rewrite(body, rel, True)
            if report is not None and rewritten != rewrite(body, rel, True, weak=False):
                report.append(f'{rel}:{old_no if line[0] == "-" else new_no}: {body.rstrip(chr(10))} -> {rewritten.rstrip(chr(10))}')
            out.append(line[0] + rewritten)
            if line[0] != '+': old_no, old_left = old_no + 1, old_left - 1
            if line[0] != '-': new_no, new_left = new_no + 1, new_left - 1
        else:
            if in_hunk and line in ('\n', '\r\n'):  # an editor stripped the blank context line's space
                old_no, old_left, new_no, new_left = old_no + 1, old_left - 1, new_no + 1, new_left - 1
            out.append(line)
    return ''.join(out)


# ---- --check --------------------------------------------------------------------------------

def check_patterns():
    """[(class, compiled regex)]: every spelling of the old name."""
    return [
        ('1 package plx_platform', re.compile(r'plx_platform(?!_webos|_desktop|_tizen)')),
        ('2 path rust-modules/platform', re.compile(r'rust-modules/platform(?!-webos|-desktop|-tizen)')),
        ('2 bare path platform/..', BARE_OLD),
        ('2 manifest path ../platform', UP_OLD),
        ('2 directory `platform/`', re.compile(re.escape(DIRLIST_OLD))),
        ('3 layer token [platform]', re.compile(re.escape(SECTION_OLD))),
        ('3 layer token `platform`', re.compile(re.escape(TICK_OLD))),
        ('3 layer token in a list', RUN_OLD),
        ('3 layer token in the layer table', ROW_OLD),
        ('3 layer token in a table cell', CELL_OLD),
        ('3 layer token before layer/crate', NOUN_OLD),
        ('3 layer token platform::member', MODPATH_OLD),
        ('3 fence keyword [port X]', FENCE_OLD),
        ('3 SRC_PLATFORM', re.compile(r'\bSRC_PLATFORM\b')),
    ]


def classify_quoted(rel, line):
    """The keeper label of a quoted `platform`, or None when nothing classifies it."""
    if len(quoted_layers(line, 'platform')) >= 2: return None  # a list of layers
    for pattern, label in KEEP:
        if re.search(pattern, rel): return label
    return None


def classify_separator(rel, line):
    """The keeper label of a separator-list hit that is not the layer, or None."""
    for path, pattern, label in SEP_KEEP:
        if re.search(path, rel) and re.search(pattern, line): return label
    return None


def check(root):
    problems, kept = [], {}
    patterns = check_patterns()
    files = list_files(root)
    for rel in files:
        if is_exempt(rel): continue
        if rel.startswith(OLD_DIR + '/'): problems.append((rel, 0, 'directory still named rust-modules/platform', ''))
        text = read_text(root / rel)
        if text is None: continue
        for number, line in enumerate(text.splitlines(), 1):
            flagged = False
            for label, pattern in patterns:
                if pattern.search(line): problems.append((rel, number, label, line.strip())); flagged = True
            if not flagged and SEP_OLD.search(line):
                keeper = classify_separator(rel, line)
                if keeper is None: problems.append((rel, number, '3 layer token in a separator list', line.strip()))
                else: kept.setdefault(keeper, []).append(rel)
            if rel == 'ci/module-layers.ini' and line.startswith('uses') and re.search(r'\bplatform\b', line):
                problems.append((rel, number, '3 layer token in a uses line', line.strip()))
            for path, old, _ in LITERALS:
                if path == head_name(rel) and old in line: problems.append((rel, number, 'enumerated site', line.strip()))
            if QUOTED.search(line):
                label = classify_quoted(rel, line)
                if label is None: problems.append((rel, number, 'UNCLASSIFIED quoted "platform"', line.strip()))
                else: kept.setdefault(label, []).append(rel)
    for rel, number, label, line in problems:
        print(f'{rel}:{number}: [{label}] {line[:110]}')
    for label, paths in sorted(kept.items()):
        print(f'kept ({label}): {len(paths)} hits in {len(set(paths))} files')
    exempt = [rel for rel in files if is_exempt(rel) and rel not in (SELF, SELF_TEST)]
    hits = sum(1 for rel in exempt if read_text(root / rel) and re.search(r'plx_platform|rust-modules/platform', read_text(root / rel)))
    print(f'exempt (dated records, the plan): {hits} of {len(exempt)} exempt files name the old name and keep it')
    print(f'check: {len(problems)} old-name site(s) left' if problems else 'check: 0 old-name sites left')
    return 1 if problems else 0


# ---- --equivalence ---------------------------------------------------------------------------

def export(root, rev, dest):
    data = subprocess.run(['git', '-C', str(root), 'archive', '--format=tar', rev], capture_output=True, check=True).stdout
    with tarfile.open(fileobj=io.BytesIO(data)) as tar:
        if hasattr(tarfile, 'data_filter'): tar.extractall(dest, filter='data')
        else: tar.extractall(dest)


def tree_files(root):
    return {str(p.relative_to(root)): p for p in sorted(root.rglob('*')) if p.is_file() or p.is_symlink()}


def tokens(text):
    return sorted(t for t in re.findall(r'[A-Za-z0-9_]+|[^\sA-Za-z0-9_]', text) if t != ',')


def hunks(expected, actual):
    a, b = expected.splitlines(), actual.splitlines()
    return [(a[i1:i2], b[j1:j2], i1 + 1) for tag, i1, i2, j1, j2 in difflib.SequenceMatcher(None, a, b, autojunk=False).get_opcodes() if tag != 'equal']


def compare(expected_root, actual_root, leg, notes):
    """Failures between two trees, with the allowed classes appended to `notes`."""
    expected, actual = tree_files(expected_root), tree_files(actual_root)
    fails = []
    for rel in sorted(set(expected) | set(actual)):
        # Name every file as the head names it, so the two legs print one row per file.
        shown = NEW_DIR + rel[len(OLD_DIR):] if rel.startswith(OLD_DIR + '/') else rel
        if rel in (SELF, SELF_TEST) and rel not in expected:
            notes.append((leg, 'd tool', shown, 'added by the PR'))
            continue
        if rel not in actual: fails.append((leg, rel, 'missing from the head side')); continue
        if rel not in expected: fails.append((leg, rel, 'only on the head side')); continue
        if expected[rel].is_symlink() or actual[rel].is_symlink():
            if expected[rel].is_symlink() != actual[rel].is_symlink() or os.readlink(expected[rel]) != os.readlink(actual[rel]):
                fails.append((leg, rel, 'symlink differs'))
            continue
        a, b = expected[rel].read_bytes(), actual[rel].read_bytes()
        if a == b: continue
        ta, tb = a.decode('utf-8', 'replace'), b.decode('utf-8', 'replace')
        if rel in CERTIFICATES:
            notes.append((leg, 'a certificate', shown, 'regenerated'))
        elif rel in PIN_FILES:
            mask = lambda text: re.sub(r'[0-9a-f]{64}', 'HASH', text)
            if mask(ta) == mask(tb):
                lines = [i for _, _, i in hunks(ta, tb)]
                notes.append((leg, 'a pin', shown, f'digest lines only, at line(s) {lines}'))
            else: fails.append((leg, rel, 'a pin file differs in more than digests'))
        elif rel == LOCK:
            if sorted(ta.splitlines()) == sorted(tb.splitlines()): notes.append((leg, 'b lock', shown, 'same lines, re-sorted: no version moved'))
            else: fails.append((leg, rel, 'Cargo.lock differs beyond a re-sort'))
        elif rel == STEPS_FILE and [l for l in tb.splitlines() if l != STEPS_LINE and not l.startswith(STEPS_COMMENT)] == ta.splitlines():
            notes.append((leg, 'd tool', shown, 'the comment and the step line that run the test'))
        elif rel.endswith('.rs') and all(tokens('\n'.join(x)) == tokens('\n'.join(y)) for x, y, _ in hunks(ta, tb)):
            notes.append((leg, 'c reflow', shown, f'{len(hunks(ta, tb))} hunk(s) with the same tokens'))
        else:
            first = next(iter(hunks(ta, tb)), ([], [], 0))
            fails.append((leg, rel, f'differs, first hunk at line {first[2]}: {first[0][:2]} -> {first[1][:2]}'))
    return fails


def equivalence(root, base):
    if subprocess.run(['git', '-C', str(root), 'status', '--short'], capture_output=True, text=True, check=True).stdout.strip():
        print('note: the working tree is not clean; the comparison reads the committed HEAD', file=sys.stderr)
    head = subprocess.run(['git', '-C', str(root), 'rev-parse', '--short', 'HEAD'], capture_output=True, text=True, check=True).stdout.strip()
    notes, fails = [], []
    with tempfile.TemporaryDirectory(prefix='rename-os-equivalence-') as scratch:
        scratch = Path(scratch)
        for name, rev in (('base', base), ('head', 'HEAD')):
            (scratch / name).mkdir()
            export(root, rev, scratch / name)
            shutil.copytree(scratch / name, scratch / (name + '-work'), symlinks=True)
        apply(scratch / 'base-work', reverse=False)
        apply(scratch / 'head-work', reverse=True)
        # Leg 1: script(BASE) against HEAD. Leg 2: BASE against reverse(HEAD), which also proves the reverse.
        fails += compare(scratch / 'base-work', scratch / 'head', 'forward(BASE) vs HEAD', notes)
        fails += compare(scratch / 'base', scratch / 'head-work', 'BASE vs reverse(HEAD)', notes)
    print(f'equivalence of HEAD {head} with the script applied to {base}: allowed differences')
    for cls in ('a certificate', 'a pin', 'b lock', 'c reflow', 'd tool'):
        rows = sorted({(rel, why) for _, c, rel, why in notes if c == cls})
        print(f'  ({cls}) {len(rows)} file(s)')
        for rel, why in rows: print(f'      {rel}: {why}')
    for leg, rel, why in fails: print(f'FAIL [{leg}] {rel}: {why}')
    print(f'equivalence: {len(fails)} failure(s)' if fails else 'equivalence: ok')
    return 1 if fails else 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parent.parent)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--reverse', action='store_true', help='rewrite the new names back to the old')
    mode.add_argument('--check', action='store_true', help='list every old-name site left; exit 1 on any')
    mode.add_argument('--reverse-patch', metavar='FILE', type=Path, help='rewrite a post-rename patch to the old names')
    mode.add_argument('--equivalence', metavar='BASE', help='prove HEAD equals script(BASE) up to an enumerated list')
    parser.add_argument('--output', type=Path, help='with --reverse-patch: write here instead of standard output')
    parser.add_argument('-n', '--dry-run', action='store_true', help='list the files that would change, change nothing')
    args = parser.parse_args(argv)
    root = args.root.resolve()
    if args.reverse_patch:
        weak = []
        patch = reverse_patch(args.reverse_patch.read_bytes().decode('utf-8'), weak)
        for entry in weak: print(entry, file=sys.stderr)
        if weak: print(f'reverse-patch: {len(weak)} line(s) rewritten by a weak pattern (a bare `os` read as the layer); '
                       'read each before applying', file=sys.stderr)
        if args.output: args.output.write_bytes(patch.encode('utf-8'))
        else: sys.stdout.write(patch)
        return 0
    if args.check: return check(root)
    if args.equivalence: return equivalence(root, args.equivalence)
    return apply(root, args.reverse, args.dry_run)


if __name__ == '__main__':
    sys.exit(main())
