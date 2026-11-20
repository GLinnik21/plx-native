#!/usr/bin/env python3
"""tools/rename-os-layer.py on synthetic trees: every spelling of `plx_platform` is rewritten and
reversed byte for byte, a second run changes nothing, `--check` flags every old-name site and fails
on a quoted "platform" it cannot classify (a layer list is never a keeper), and `--equivalence`
accepts exactly the enumerated hand edits and nothing else. Each case writes a tiny tree (or a tiny
git repository) to a private temp dir; it reads nothing of the checkout but the script."""
import contextlib
import importlib.util
import io
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
_spec = importlib.util.spec_from_file_location('rename_os_layer', ROOT / 'tools' / 'rename-os-layer.py')
tool = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(tool)

LOCK_OLD = '''version = 4

[[package]]
name = "plx_net"
version = "0.0.0"
dependencies = [
 "plx_platform",
]

[[package]]
name = "plx_owp"
version = "0.0.0"

[[package]]
name = "plx_platform"
version = "0.0.0"
'''
# What cargo writes after the rename: the same lines, the packages re-sorted by name (`plx_os` sorts before `plx_owp`).
LOCK_NEW_SORTED = '''version = 4

[[package]]
name = "plx_net"
version = "0.0.0"
dependencies = [
 "plx_os",
]

[[package]]
name = "plx_os"
version = "0.0.0"

[[package]]
name = "plx_owp"
version = "0.0.0"
'''

# path -> text. Each file carries the sites of one class; `NATURAL` files carry `os` meaning the OS.
FILES = {
    'rust-modules/Cargo.toml': '# layers: base/, machine/, net/, platform/, gfx/\n[workspace]\nmembers = [\n    "base",\n    "platform",\n]\n'
                               'plx_platform = { path = "platform" }\nplx_platform = { path = "platform", features = ["test-support"] }\n'
                               '# see ci/module-layers.ini `[platform]`\n',
    'rust-modules/platform/Cargo.toml': '[package]\nname = "plx_platform"\n[lib]\nname = "plx_platform"\n',
    'rust-modules/platform/src/lib.rs': '//! plx_platform: the `platform` layer.\nuse plx_platform::tv::sink;\n',
    'rust-modules/net/Cargo.toml': 'plx_platform = { path = "../platform" }\n',
    'rust-modules/net/src/lib.rs': '//! it uses `base`, `machine`, `platform` and `net`.\n// see `platform::storage::diagnostics` and keymanager::platform::Client\n',
    'rust-modules/storage/src/main.rs': '#[path = "../../platform/src/storage_service/wire.rs"]\nmod wire;\n',
    'rust-modules/base/src/scrub.rs': 'let roots = [manifest.join("../net/src"), manifest.join("../platform/src")];\n',
    'rust-modules/Cargo.lock': LOCK_OLD,
    'ci/module-layers.ini': '[machine]\nuses = base\n\n[platform]\n# crate `plx_platform` (rust-modules/platform/); also in [port webos]\nuses = base machine\n\n'
                            '[net]\nuses = base platform\n\n[port webos]\nmembers = webos\n',
    'ci/check-module-layers.py': "order = [s for s in sections if not s.startswith('port ')]\nport = section[len('port '):]\nmsg = f'is listed in [port {port}]'\n",
    'ci/check-deps.sh': 'SRC_PLATFORM=rust-modules/platform/src\nplatform holds forks of its own\n',
    'ci/list-caps.ini': '[platform/src/devcaps.rs::PCM_MAX_CHANNELS]\nreason = x\n',
    'rust-modules/platform/build.rs': '//! Code generation for the platform layer: the catalog.\n',  # a LITERALS entry keyed by the head path
    'ci/test_no_host_staticlib.py': 'WORKSPACE_FILES = (".cargo", "base", "machine", "net", "platform", "gfx", "ui")\n',
    'Makefile': 'RUST_INPUTS := $(shell find rust-modules/net rust-modules/platform rust-modules/gfx -type f)\n# chain (base -> platform -> ui/plex)\n'
                'test: ; cargo test -p plx_base -p plx_platform -p plx_gfx\n',
    'tools/cargo-seed.py': 'part("manifest-platform", mb(os.path.join(root, "rust-modules", "platform", "Cargo.toml")))\n',
    'tools/build-bench.py': 'PLATFORM_LEAF_FILE = "rust-modules/platform/src/devcaps.rs"\nALL = ("noop", "leaf", "machine", "platform", "gfx", "net")\n'
                            '  platform  ...after appending one to a leaf file\nTITLES = {\n    "platform": "Edit leaf (plx_platform devcaps.rs)",\n}\n',
    'ci/test_build_bench.py': "art('/platform', 'plx_platform', '0.0.0')\nonly = \"noop,leaf,machine,platform,gfx\"\nok(by_id[\"platform\"][\"samples\"])\n"
                              'host = doc["host"]["machine"] and doc["host"]["os"]\n',
    '.claude/hooks/release-config-check.py': 'for layer in ("base", "machine", "net", "platform", "gfx", "ui"):\n    pass\n',
    'docs/module-layers.md': 'The split: `machine/`, `platform/`, `gfx/`. `[net] uses = base platform` and `[port webos]`.\n'
                             'The `platform` layer is `plx_platform`; the `[platform]` section.\nnet   gfx   + platform machine base\n',
    # Kept: a quoted `platform` that is not the layer, and the plain word.
    'rust-modules/telemetry/src/telemetry/native.rs': 'json!({"platform": "native"});\nconst K: &[&str] = &[\n    "platform",\n];\n',
    'tests/test_replay_fixtures.py': "mock.patch.object(replay.sys, 'platform', 'linux')\n",
    'docs/plex-openapi.json': '{"platform": "MacOSX"}\n',
    # `os` already means the operating system.
    'rust-modules/telemetry/src/telemetry/sentry.rs': '/// The `os` context a native crash carries, `"os"` and `"net"`, `"gfx"` none.\nlet k = ["os", "webos", "hardware"];\n',
    'tools/disk-budget.py': '# uses only `os` and `sys`\nuse std::os::raw::c_int;\n',
    'docs/disk-budget.md': 'needs only `os` and `sys` and the host platform\n',
    # Exempt: dated records and the plan keep the old name.
    'docs/release-audits/v0.9.0.md': 'rust-modules/platform/src/x.rs plx_platform\n',
    'docs/measurements/acb-2026-11-04.md': 'rust-modules/platform/src/tv/plane.rs\n',
    'docs/some-note-2026-11-16.md': 'plx_platform and `platform`\n',
    'docs/platform-ports.md': 'F36: plx_platform, rust-modules/platform/\n',
    # Certificates are regenerated by their producers, never rewritten.
    'ci/proofs/platform-generated-enums.json': json.dumps({'package': 'plx_platform', 'module': 'platform/src/i18n/mod.rs'}, indent=2) + '\n',
    'ci/proofs/serde-json-immutable.json': json.dumps({'owners': {'platform/Cargo.toml': 'abc'}}, indent=2) + '\n',
    'ci/readonly_generated.py': "CERTIFICATE_SHA256 = '" + '1' * 64 + "'\nSOURCES = ['platform/Cargo.toml']\n",
    'ci/readonly_dependencies.py': "CERTIFICATE_SHA256 = '" + '2' * 64 + "'\n",
    # Names the new waves bring: neither rewritten nor flagged.
    'docs/ports.md': 'crate plx_platform_webos in rust-modules/platform-webos/ and rust-modules/platform-desktop\n',
    'ci/check-python-steps.txt': 'python3 ci/test_module_graph.py\n',
}
NATURAL = ('rust-modules/telemetry/src/telemetry/sentry.rs', 'tools/disk-budget.py', 'docs/disk-budget.md')
EXEMPT = ('docs/release-audits/v0.9.0.md', 'docs/measurements/acb-2026-11-04.md', 'docs/some-note-2026-11-16.md',
          'docs/platform-ports.md', 'ci/proofs/platform-generated-enums.json', 'ci/proofs/serde-json-immutable.json')


def write_tree(root, files):
    for rel, text in files.items():
        path = Path(root) / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding='utf-8')


def read_tree(root):
    root = Path(root)
    return {str(p.relative_to(root)): p.read_text(encoding='utf-8') for p in sorted(root.rglob('*')) if p.is_file() and '.git' not in p.parts}


def run(*args):
    """(exit code, stdout) of the tool's main()."""
    out = io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
        try: code = tool.main(list(args))
        except SystemExit as exit_: code = exit_.code
    return code, out.getvalue()


def run_err(*args):
    """(exit code, stdout, stderr) of the tool's main()."""
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        try: code = tool.main(list(args))
        except SystemExit as exit_: code = exit_.code
    return code, out.getvalue(), err.getvalue()


def git(root, *args):
    env = dict(os.environ, GIT_AUTHOR_NAME='t', GIT_AUTHOR_EMAIL='t@example.invalid', GIT_COMMITTER_NAME='t', GIT_COMMITTER_EMAIL='t@example.invalid',
               GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull)
    return subprocess.run(['git', '-C', str(root), *args], capture_output=True, text=True, check=True, env=env).stdout


class RenameTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='rename-os-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / 'tree'
        write_tree(self.root, FILES)

    def test_forward_rewrites_every_spelling(self):
        code, out = run('--root', str(self.root))
        self.assertEqual(code, 0, out)
        now = read_tree(self.root)
        self.assertNotIn('rust-modules/platform/Cargo.toml', now)
        self.assertEqual(now['rust-modules/os/build.rs'], '//! Code generation for the os layer: the catalog.\n')
        self.assertEqual(now['rust-modules/os/Cargo.toml'], '[package]\nname = "plx_os"\n[lib]\nname = "plx_os"\n')
        self.assertEqual(now['rust-modules/os/src/lib.rs'], '//! plx_os: the `os` layer.\nuse plx_os::tv::sink;\n')
        self.assertEqual(now['rust-modules/Cargo.toml'], '# layers: base/, machine/, net/, os/, gfx/\n[workspace]\nmembers = [\n    "base",\n    "os",\n]\n'
                         'plx_os = { path = "os" }\nplx_os = { path = "os", features = ["test-support"] }\n# see ci/module-layers.ini `[os]`\n')
        self.assertEqual(now['rust-modules/net/Cargo.toml'], 'plx_os = { path = "../os" }\n')
        self.assertEqual(now['rust-modules/net/src/lib.rs'],
                         '//! it uses `base`, `machine`, `os` and `net`.\n// see `os::storage::diagnostics` and keymanager::platform::Client\n')
        self.assertEqual(now['rust-modules/storage/src/main.rs'], '#[path = "../../os/src/storage_service/wire.rs"]\nmod wire;\n')
        self.assertIn('manifest.join("../os/src")', now['rust-modules/base/src/scrub.rs'])
        self.assertEqual(now['rust-modules/Cargo.lock'].count('plx_os'), 2)
        self.assertNotIn('plx_platform', now['rust-modules/Cargo.lock'])
        self.assertEqual(now['ci/module-layers.ini'], '[machine]\nuses = base\n\n[os]\n# crate `plx_os` (rust-modules/os/); also in [platform webos]\nuses = base machine\n\n'
                         '[net]\nuses = base os\n\n[platform webos]\nmembers = webos\n')
        self.assertEqual(now['ci/check-module-layers.py'],
                         "order = [s for s in sections if not s.startswith('platform ')]\nport = section[len('platform '):]\nmsg = f'is listed in [platform {port}]'\n")
        self.assertEqual(now['ci/check-deps.sh'], 'SRC_OS=rust-modules/os/src\nos holds forks of its own\n')
        self.assertEqual(now['ci/list-caps.ini'], '[os/src/devcaps.rs::PCM_MAX_CHANNELS]\nreason = x\n')
        self.assertIn('"machine", "net", "os", "gfx"', now['ci/test_no_host_staticlib.py'])
        self.assertEqual(now['Makefile'].splitlines()[0], 'RUST_INPUTS := $(shell find rust-modules/net rust-modules/os rust-modules/gfx -type f)')
        self.assertIn('base -> os -> ui/plex', now['Makefile'])
        self.assertIn('-p plx_os -p plx_gfx', now['Makefile'])
        self.assertEqual(now['tools/cargo-seed.py'], 'part("manifest-os", mb(os.path.join(root, "rust-modules", "os", "Cargo.toml")))\n')
        self.assertEqual(now['tools/build-bench.py'], 'OS_LEAF_FILE = "rust-modules/os/src/devcaps.rs"\nALL = ("noop", "leaf", "machine", "os", "gfx", "net")\n'
                         '  os        ...after appending one to a leaf file\nTITLES = {\n    "os": "Edit leaf (plx_os devcaps.rs)",\n}\n')
        self.assertEqual(now['ci/test_build_bench.py'], "art('/os', 'plx_os', '0.0.0')\nonly = \"noop,leaf,machine,os,gfx\"\nok(by_id[\"os\"][\"samples\"])\n"
                         'host = doc["host"]["machine"] and doc["host"]["os"]\n')
        self.assertIn('("base", "machine", "net", "os", "gfx", "ui")', now['.claude/hooks/release-config-check.py'])
        self.assertEqual(now['docs/module-layers.md'], 'The split: `machine/`, `os/`, `gfx/`. `[net] uses = base os` and `[platform webos]`.\n'
                         'The `os` layer is `plx_os`; the `[os]` section.\nnet   gfx   + os machine base\n')
        for rel in EXEMPT + ('docs/ports.md', 'rust-modules/telemetry/src/telemetry/native.rs', 'tests/test_replay_fixtures.py', 'docs/plex-openapi.json') + NATURAL:
            self.assertEqual(now[rel], FILES[rel], rel)

    def test_second_run_changes_nothing(self):
        run('--root', str(self.root))
        first = read_tree(self.root)
        code, out = run('--root', str(self.root))
        self.assertEqual(code, 0)
        self.assertIn('0 of', out)
        self.assertEqual(read_tree(self.root), first)

    def test_reverse_restores_every_byte(self):
        run('--root', str(self.root))
        code, _ = run('--root', str(self.root), '--reverse')
        self.assertEqual(code, 0)
        self.assertEqual(read_tree(self.root), FILES)

    def test_reverse_leaves_the_operating_system_alone(self):
        write_tree(self.root, {'rust-modules/net/src/x.rs': 'use std::os::raw; let doc = host["os"]; libc::os::foo();\n'})
        run('--root', str(self.root), '--reverse')
        self.assertEqual((self.root / 'rust-modules/net/src/x.rs').read_text(), 'use std::os::raw; let doc = host["os"]; libc::os::foo();\n')
        for rel in NATURAL:
            self.assertEqual((self.root / rel).read_text(), FILES[rel])

    def test_check_flags_the_old_tree_and_passes_the_new(self):
        code, out = run('--root', str(self.root), '--check')
        self.assertEqual(code, 1)
        for label in ('1 package plx_platform', '2 path rust-modules/platform', '2 bare path platform/..', '2 manifest path ../platform',
                      '2 directory `platform/`', '3 layer token [platform]', '3 layer token `platform`', '3 layer token in a list', '3 layer token in the layer table',
                      '3 layer token platform::member', '3 fence keyword [port X]', '3 SRC_PLATFORM', '3 layer token in a uses line',
                      'enumerated site', 'directory still named rust-modules/platform'):
            self.assertIn('[' + label + ']', out, label)
        self.assertIn('ci/test_no_host_staticlib.py:1: [UNCLASSIFIED quoted "platform"]', out)
        self.assertNotIn('native.rs', out.split('kept')[0])  # a keeper is not a problem
        run('--root', str(self.root))
        code, out = run('--root', str(self.root), '--check')
        # Only the certificates (regenerated by hand) still name the old spelling.
        self.assertEqual(code, 1)
        self.assertEqual({line.split(':')[0] for line in out.splitlines() if ': [' in line},
                         {'ci/proofs/platform-generated-enums.json', 'ci/proofs/serde-json-immutable.json'})
        write_tree(self.root, {'ci/proofs/platform-generated-enums.json': '{"package": "plx_os"}\n', 'ci/proofs/serde-json-immutable.json': '{}\n'})
        code, out = run('--root', str(self.root), '--check')
        self.assertEqual(code, 0, out)
        self.assertIn('check: 0 old-name sites left', out)
        self.assertIn('kept (Sentry event `platform` field)', out)
        self.assertIn('kept (sys.platform mock)', out)

    def test_an_unclassified_quoted_platform_fails_and_a_layer_list_is_never_a_keeper(self):
        run('--root', str(self.root))
        write_tree(self.root, {'ci/proofs/platform-generated-enums.json': '{}\n', 'ci/proofs/serde-json-immutable.json': '{}\n',
                               'tools/new-gate.py': 'LAYERS = "platform", "gfx"\nx = {"platform": 1}\n',
                               'rust-modules/telemetry/src/telemetry/list.rs': 'let l = ["base", "machine", "platform", "gfx"];\n'})
        code, out = run('--root', str(self.root), '--check')
        self.assertEqual(code, 1)
        self.assertIn('tools/new-gate.py:1: [UNCLASSIFIED quoted "platform"]', out)
        self.assertIn('tools/new-gate.py:2: [UNCLASSIFIED quoted "platform"]', out)
        self.assertIn('rust-modules/telemetry/src/telemetry/list.rs:1: [UNCLASSIFIED quoted "platform"]', out)

    def test_check_flags_a_layer_token_in_a_separator_list_and_a_slash_before_dotdot(self):
        run('--root', str(self.root))
        write_tree(self.root, {'ci/proofs/platform-generated-enums.json': '{}\n', 'ci/proofs/serde-json-immutable.json': '{}\n',
                               'ci/new-gate.sh': 'for c in net platform gfx; do :; done\n',
                               'ci/new-args.sh': 'run --only machine,platform now\n',
                               'ci/new-chain.md': 'the chain is base -> platform -> ui\n',
                               'ci/new-order.md': 'platform, plex and data are below\n',
                               'ci/new-path.py': 'root = os.path.join(x, "/../platform/src")\n',
                               'ci/new-path-ok.py': 'root = os.path.join(x, "../os/src", "/../os/src")\n',
                               'ci/new-dir.py': 'root = Path("rust-modules/platform/src")\n',
                               'ci/new-other-dir.py': 'root = Path("x/platform/src")\n',
                               'docs/new-prose.md': 'the platform is a cross-platform thing; ports and the ARM platform stay\n',
                               'docs/new-data.md': 'the data platform stays\n'})
        code, out = run('--root', str(self.root), '--check')
        self.assertEqual(code, 1)
        for path in ('ci/new-gate.sh:1', 'ci/new-args.sh:1', 'ci/new-chain.md:1', 'ci/new-order.md:1'):
            self.assertIn(path + ': [3 layer token in a separator list]', out)
        self.assertIn('ci/new-path.py:1: [2 bare path platform/..]', out)
        self.assertNotIn('ci/new-path-ok.py', out)
        # `rust-modules/platform/` keeps its own class; a `/` before `platform/` that is not a `../` prefix
        # (`x/platform/src`, some other tree's directory) is not a bare path and is not flagged at all.
        self.assertIn('ci/new-dir.py:1: [2 path rust-modules/platform]', out)
        self.assertNotIn('ci/new-dir.py:1: [2 bare', out)
        self.assertNotIn('ci/new-other-dir.py', out)
        # Prose with no layer neighbour is not a site; `data platform` is a neighbour and is a separator hit.
        self.assertNotIn('docs/new-prose.md', out)
        self.assertIn('docs/new-data.md:1: [3 layer token in a separator list]', out)

    def test_check_flags_a_layer_table_cell_and_platform_layer_or_crate(self):
        run('--root', str(self.root))
        write_tree(self.root, {'ci/proofs/platform-generated-enums.json': '{}\n', 'ci/proofs/serde-json-immutable.json': '{}\n',
                               'docs/new-table.md': '| layer | members |\n|---|---|\n| platform | `webos storage` |\n| `os::i18n` | platform | strings |\n',
                               'docs/new-noun.md': "the platform layer crate's source\nthe platform crate's own build script\n",
                               'docs/new-host.md': '| Platform | LG webOS |\nthe webOS platform release, a platform port\n'})
        code, out = run('--root', str(self.root), '--check')
        self.assertEqual(code, 1)
        for site in ('docs/new-table.md:3', 'docs/new-table.md:4'):
            self.assertIn(site + ': [3 layer token in a table cell]', out)
        for site in ('docs/new-noun.md:1', 'docs/new-noun.md:2'):
            self.assertIn(site + ': [3 layer token before layer/crate]', out)
        self.assertNotIn('docs/new-host.md', out)     # the host platform, a column heading and a port are not the layer

    def test_both_quote_styles_are_classified(self):
        run('--root', str(self.root))
        write_tree(self.root, {'ci/proofs/platform-generated-enums.json': '{}\n', 'ci/proofs/serde-json-immutable.json': '{}\n',
                               'tools/other.py': "x = 'platform'\n"})
        code, out = run('--root', str(self.root), '--check')
        self.assertEqual(code, 1)
        self.assertIn("tools/other.py:1: [UNCLASSIFIED quoted \"platform\"] x = 'platform'", out)

    def test_dry_run_changes_nothing(self):
        code, out = run('--root', str(self.root), '--dry-run')
        self.assertEqual(code, 0)
        self.assertIn('would change', out)
        self.assertEqual(read_tree(self.root), FILES)


class ReversePatchTests(unittest.TestCase):
    PATCH = (
        'diff --git a/rust-modules/net/src/x.rs b/rust-modules/net/src/x.rs\n'
        '--- a/rust-modules/net/src/x.rs\n'
        '+++ b/rust-modules/net/src/x.rs\n'
        '@@ -10,3 +10,4 @@ fn f()\n'
        ' let ctx = 1;\n'
        '-// old: the `os` clock\n'
        '+// new: the `os` clock and plx_os::tv\n'
        '+let layers = ["base", "net", "os", "gfx"];\n'
        ' let tail = 2;\n')

    def test_every_weak_rewrite_is_printed_with_its_line_and_the_strong_ones_are_not(self):
        patch = Path(tempfile.mkdtemp(prefix='rename-os-patch-')) / 'fix.patch'
        self.addCleanup(lambda: patch.parent.exists() and __import__('shutil').rmtree(patch.parent))
        patch.write_text(self.PATCH)
        code, out, err = run_err('--reverse-patch', str(patch))
        self.assertEqual(code, 0)
        # The rewrite itself is unchanged: the backport still applies.
        self.assertIn('+// new: the `platform` clock and plx_platform::tv\n', out)
        self.assertIn('+let layers = ["base", "net", "platform", "gfx"];\n', out)
        lines = err.splitlines()
        self.assertIn('rust-modules/net/src/x.rs:11: // old: the `os` clock -> // old: the `platform` clock', lines)
        self.assertIn('rust-modules/net/src/x.rs:11: // new: the `os` clock and plx_os::tv -> // new: the `platform` clock and plx_platform::tv', lines)
        self.assertIn('rust-modules/net/src/x.rs:12: let layers = ["base", "net", "os", "gfx"]; -> let layers = ["base", "net", "platform", "gfx"];', lines)
        self.assertEqual(len([l for l in lines if ': ' in l and ' -> ' in l]), 3, err)
        self.assertNotIn('let ctx', err)  # an unchanged context line is silent

    def test_a_patch_with_only_strong_rewrites_prints_nothing(self):
        patch = Path(tempfile.mkdtemp(prefix='rename-os-patch-')) / 'fix.patch'
        self.addCleanup(lambda: patch.parent.exists() and __import__('shutil').rmtree(patch.parent))
        patch.write_text('diff --git a/rust-modules/os/src/lib.rs b/rust-modules/os/src/lib.rs\n--- a/rust-modules/os/src/lib.rs\n+++ b/rust-modules/os/src/lib.rs\n'
                         '@@ -1,1 +1,2 @@\n use plx_os::tv;\n+use plx_os::devcaps; // rust-modules/os/src\n')
        code, out, err = run_err('--reverse-patch', str(patch))
        self.assertEqual((code, err), (0, ''))
        self.assertIn('+use plx_platform::devcaps; // rust-modules/platform/src\n', out)


class EquivalenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='rename-os-equivalence-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / 'repo'
        self.root.mkdir()
        git(self.root, 'init', '-q', '-b', 'main')
        write_tree(self.root, FILES)
        git(self.root, 'add', '-A')
        git(self.root, 'commit', '-q', '-m', 'base')
        git(self.root, 'tag', 'base')
        run('--root', str(self.root))
        # The hand edits the plan allows: the two certificates and their pin lines, the lock re-sorted.
        write_tree(self.root, {
            'ci/proofs/platform-generated-enums.json': '{"package": "plx_os"}\n',
            'ci/proofs/serde-json-immutable.json': '{"owners": {"os/Cargo.toml": "def"}}\n',
            'rust-modules/Cargo.lock': LOCK_NEW_SORTED,
        })
        for rel, old, new in (('ci/readonly_generated.py', '1' * 64, '3' * 64), ('ci/readonly_dependencies.py', '2' * 64, '4' * 64)):
            path = self.root / rel
            path.write_text(path.read_text().replace(old, new))

    def commit(self):
        git(self.root, 'add', '-A')
        git(self.root, 'commit', '-q', '-m', 'head')

    def test_the_enumerated_edits_are_accepted_and_printed(self):
        self.commit()
        code, out = run('--root', str(self.root), '--equivalence', 'base')
        self.assertEqual(code, 0, out)
        self.assertIn('equivalence: ok', out)
        self.assertIn('(a certificate) 2 file(s)', out)
        self.assertIn('(a pin) 2 file(s)', out)
        self.assertIn('(b lock) 1 file(s)', out)

    def test_a_hand_edit_outside_the_classes_fails(self):
        self.commit()
        path = self.root / 'ci/check-deps.sh'
        path.write_text(path.read_text() + 'echo extra\n')
        git(self.root, 'commit', '-q', '-am', 'edit')
        code, out = run('--root', str(self.root), '--equivalence', 'base')
        self.assertEqual(code, 1)
        self.assertIn('FAIL [forward(BASE) vs HEAD] ci/check-deps.sh', out)
        self.assertIn('FAIL [BASE vs reverse(HEAD)] ci/check-deps.sh', out)

    def test_a_missed_rewrite_fails(self):
        path = self.root / 'rust-modules/net/src/lib.rs'
        path.write_text(path.read_text().replace('`os`', '`platform`'))
        self.commit()
        code, out = run('--root', str(self.root), '--equivalence', 'base')
        self.assertEqual(code, 1)
        self.assertIn('rust-modules/net/src/lib.rs', out)

    def test_a_version_bump_in_the_lock_fails(self):
        (self.root / 'rust-modules/Cargo.lock').write_text(LOCK_NEW_SORTED.replace('version = "0.0.0"', 'version = "0.0.1"', 1))
        self.commit()
        code, out = run('--root', str(self.root), '--equivalence', 'base')
        self.assertEqual(code, 1)
        self.assertIn('Cargo.lock differs beyond a re-sort', out)

    def test_a_pin_file_edit_beyond_digests_fails(self):
        path = self.root / 'ci/readonly_generated.py'
        path.write_text(path.read_text() + "EXTRA = 1\n")
        self.commit()
        code, out = run('--root', str(self.root), '--equivalence', 'base')
        self.assertEqual(code, 1)
        self.assertIn('a pin file differs in more than digests', out)

    def test_a_reflow_with_the_same_tokens_is_allowed_and_listed(self):
        path = self.root / 'rust-modules/os/src/lib.rs'
        path.write_text('//! plx_os: the `os` layer.\nuse plx_os::{\n    tv::sink,\n};\n')
        self.commit()
        code, out = run('--root', str(self.root), '--equivalence', 'base')
        self.assertEqual(code, 1, out)  # `use plx_os::tv::sink;` -> braces: the `{` `}` tokens are new
        path.write_text('//! plx_os: the `os` layer.\nuse plx_os::tv::sink;\n\n')
        git(self.root, 'commit', '-q', '-am', 'blank line')
        code, out = run('--root', str(self.root), '--equivalence', 'base')
        self.assertEqual(code, 0, out)
        self.assertIn('(c reflow) 1 file(s)', out)

    def test_the_tool_and_its_test_are_the_fourth_class(self):
        write_tree(self.root, {'tools/rename-os-layer.py': '# the tool\n', 'ci/test_rename_os_layer.py': '# its test\n',
                               'ci/check-python-steps.txt': 'python3 ci/test_module_graph.py\n# tools/rename-os-layer.py, the rename tool\npython3 ci/test_rename_os_layer.py\n'})
        self.commit()
        code, out = run('--root', str(self.root), '--equivalence', 'base')
        self.assertEqual(code, 0, out)
        self.assertIn('(d tool) 3 file(s)', out)


    def test_a_post_rename_patch_is_rewritten_to_apply_on_the_old_line(self):
        self.commit()
        lib, deps, seed = (self.root / 'rust-modules/os/src/lib.rs', self.root / 'ci/check-deps.sh', self.root / 'tools/cargo-seed.py')
        lib.write_text(lib.read_text() + '// the `os` layer names plx_os::tv and rust-modules/os/src\n')
        deps.write_text(deps.read_text().replace('os holds forks', 'os holds forks (and more)'))
        seed.write_text(seed.read_text().replace('part("manifest-os"', 'part("manifest-os", extra=1) or part("manifest-os"'))
        git(self.root, 'commit', '-q', '-am', 'a fix written after the rename')
        patch = Path(self.temp.name) / 'fix.patch'
        patch.write_text(git(self.root, 'diff', 'HEAD~1', 'HEAD'))
        reversed_patch = Path(self.temp.name) / 'fix-old.patch'
        code, _ = run('--reverse-patch', str(patch), '--output', str(reversed_patch))
        self.assertEqual(code, 0)
        text = reversed_patch.read_text()
        self.assertIn('diff --git a/rust-modules/platform/src/lib.rs b/rust-modules/platform/src/lib.rs', text)
        self.assertIn('the `platform` layer names plx_platform::tv and rust-modules/platform/src', text)
        self.assertIn('part("manifest-platform", extra=1)', text)
        self.assertNotIn('plx_os', text)
        git(self.root, 'checkout', '-q', 'base')  # the release line: old names
        with self.assertRaises(subprocess.CalledProcessError): git(self.root, 'apply', '--check', str(patch))
        git(self.root, 'apply', '--check', str(reversed_patch))
        git(self.root, 'apply', str(reversed_patch))
        self.assertIn('plx_platform::tv', (self.root / 'rust-modules/platform/src/lib.rs').read_text())
        self.assertEqual(run('--root', str(self.root), '--check')[0], 1)  # the old names are back
        self.assertNotIn('rust-modules/os', git(self.root, 'diff'))


if __name__ == '__main__':
    unittest.main()
