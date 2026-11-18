"""A checkout hook must route the very first plain Cargo build, including in another worktree."""
import json
import os
import runpy
from unittest import mock
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'tools' / 'build-root.py'


class CheckoutSetup(unittest.TestCase):
    def test_worktree_hook(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp).resolve()
            env = dict(os.environ, HOME=str(base), GIT_CONFIG_GLOBAL=os.devnull)
            root = base / 'main'
            module = root / 'rust-modules'
            module.mkdir(parents=True)
            (module / 'Cargo.toml').touch()
            (root / 'fuzz').mkdir()
            (root / 'fuzz/Cargo.toml').touch()

            def run(*args, cwd=root, check=True):
                return subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True, check=check)

            run('git', 'init', '-q')
            run('git', 'add', '.')
            run('git', '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', 'commit', '-qm', 'test')
            run(sys.executable, SCRIPT, 'prepare')
            self.assertFalse((module / 'target').exists())
            das = base / 'storage'
            das.mkdir()
            run(sys.executable, SCRIPT, 'install', str(das))
            run(sys.executable, SCRIPT, 'prepare')
            custom = module / 'cache'
            run(sys.executable, SCRIPT, 'prepare', custom)
            self.assertFalse(custom.exists())
            lane = base / 'lane with space'
            run('git', 'worktree', 'add', '-q', '--detach', lane)
            for checkout in (root, lane):
                target = checkout / 'rust-modules/target'
                self.assertTrue(target.is_symlink())
                self.assertIn(das, target.resolve().parents)
                fuzz = checkout / 'fuzz/target'
                self.assertTrue(fuzz.is_symlink())
                self.assertEqual(fuzz.resolve().parent, target.resolve().parent)
                self.assertNotEqual(fuzz.resolve(), (checkout / 'rust-modules/target-fuzz').resolve())
                self.assertEqual(run('git', 'status', '--porcelain', cwd=checkout).stdout, '')
            self.assertNotEqual((module / 'target').resolve(), (lane / 'rust-modules/target').resolve())
            lane_target = (lane / 'rust-modules/target').resolve()
            moved = base / 'moved'
            run('git', 'worktree', 'move', lane, moved)
            self.assertNotIn(str(lane_target), run(sys.executable, SCRIPT, 'orphans').stdout.splitlines())
            run(sys.executable, SCRIPT, 'prepare', cwd=moved)
            self.assertEqual((moved / 'rust-modules/target').resolve(), lane_target)
            run('git', 'worktree', 'remove', moved)
            orphans = run(sys.executable, SCRIPT, 'orphans').stdout.splitlines()
            self.assertIn(str(lane_target), orphans)
            self.assertNotIn(str((module / 'target').resolve()), orphans)
            installed = root / '.git/hooks/plxnative-build-root.py'
            for harness, relative in [('claude', '.claude/settings.json'), ('codex', '.codex/hooks.json')]:
                settings = base / relative
                settings.parent.mkdir(exist_ok=True)
                settings.write_text('{"other": true, "hooks": {"SessionStart": []}}')
                run(sys.executable, SCRIPT, 'install-' + harness)
                run(sys.executable, SCRIPT, 'install-' + harness)
                config = json.loads(settings.read_text())
                self.assertTrue(config['other'])
                self.assertEqual(len(config['hooks']['SessionStart']), 1)
            # Model the native client's checkout path that skips Git hooks.
            run('git', '-c', 'core.hooksPath=' + os.devnull, 'worktree', 'add', '-q', '--detach', lane)
            self.assertFalse((lane / 'rust-modules/target').exists())
            subprocess.run([sys.executable, installed, 'session-event'], env=env, cwd=root, check=True,
                           input=json.dumps({'cwd': str(lane)}), text=True)
            self.assertTrue((lane / 'rust-modules/target').is_symlink())
            das.rename(base / 'offline')
            self.assertNotEqual(run(sys.executable, SCRIPT, 'prepare', check=False).returncode, 0)
            self.assertFalse(das.exists())

    @unittest.skipUnless(sys.platform == 'darwin', 'uses macOS atomic directory exchange')
    def test_migration_checks_for_concurrent_writers_and_keeps_original(self):
        helper = runpy.run_path(str(SCRIPT))
        migrate = helper['move_idle']
        with tempfile.TemporaryDirectory() as tmp:
            checkout = Path(tmp)
            source = checkout / 'target'
            external = checkout / 'external'
            target = external / 'target'
            source.mkdir()
            external.mkdir()
            (external / '.checkout').write_text(str(checkout))
            (source / 'object').write_text('original')
            real_run = subprocess.run

            def probe(busy):
                def run(cmd, *args, **kwargs):
                    if 'probe' in cmd:
                        return subprocess.CompletedProcess(cmd, int(busy), 'fixture', '')
                    return real_run(cmd, *args, **kwargs)
                return run

            with mock.patch.object(subprocess, 'run', probe(True)):
                migrate(checkout, source, target)
            self.assertFalse(target.exists())
            target.mkdir()
            (target / 'object').write_text('other123')
            with mock.patch.object(subprocess, 'run', probe(False)):
                with self.assertRaisesRegex(ValueError, 'both retained'):
                    migrate(checkout, source, target)
                self.assertFalse(source.is_symlink())
                (target / 'object').write_text('original')
                migrate(checkout, source, target)
            self.assertTrue(source.is_symlink())
            self.assertEqual((source / 'object').read_text(), 'original')
            self.assertFalse((checkout / '.target.plx-original').exists())


if __name__ == '__main__':
    unittest.main()
