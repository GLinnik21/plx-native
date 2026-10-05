#!/usr/bin/env python3
"""Grade the ONE television-address / Wake-on-LAN-MAC resolver, `tools/tv-config.sh`, and every
reader that must go through it. No television, no network, no real ssh: every path is a throwaway
git repository plus a linked worktree, and every value is from the documentation ranges (RFC 5737
addresses, `aa:bb:cc:dd:ee:ff`).

The problem it pins: `.tv-host` / `.tv-mac` are gitignored per-checkout files, so a linked worktree
has neither -- and worktrees are where the parallel agents live. The order, for the address and the
MAC alike, is

    this checkout's file  ->  the main checkout's file  ->  the per-USER file

where the per-user file is `${PLX_TV_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/plxnative}/tv-host`
(`tv-mac`). The environment sits in front of all of that, and each tool keeps the variables it always
honoured (tv-ssh: PLX_TV_ADDR, TV, TV_HOST; wake-tv: TV_HOST, TV, TV_MAC). Whitespace is stripped and
an empty file is "absent". `PLX_TV_NO_HOST_FILE=1` (tv-ssh's escape hatch) switches every file off.

What it covers:
  * the resolver itself, from a worktree that has no .tv-* file of its own;
  * the precedence between the three files, the empty-file fall-through, the escape hatch, the
    default location, and `set-mac` (writes the USER file, mode 600);
  * tools/tv-ssh, which keeps its env precedence and now ends at the user file;
  * wake-tv.sh, which persists a MAC it learns from ARP under the user dir and never in the repo;
  * the two Python readers (tests/run.py, tools/stream-screen.py) through tools/tv_config.py;
  * parity between the shell resolver's `dir` and the one the outbound guard hook computes.
"""
import importlib.util
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "tests"))
from test_tv_ssh import FAKE_SSH, write_exe  # noqa: E402  (the fake ssh every tv-ssh test uses)

HOST_USER = "192.0.2.50"
HOST_MAIN = "192.0.2.51"
HOST_REPO = "192.0.2.52"
HOST_ENV = "192.0.2.53"
HOST_ENV2 = "192.0.2.54"
MAC_USER = "aa:bb:cc:dd:ee:01"
MAC_MAIN = "aa:bb:cc:dd:ee:02"
MAC_REPO = "aa:bb:cc:dd:ee:03"
MAC_ARP = "aa:bb:cc:dd:ee:ff"

# Every script a reader needs, at the path it has in the real tree (each locates the repo from its
# own location, so the copies must keep their relative layout).
TREE = (
    "tools/tv-config.sh",
    "tools/tv-ssh",
    ".agents/skills/wake-tv/wake-tv.sh",
)

GIT = ["git", "-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false"]


def read(path):
    return Path(path).read_text()


class Fixture(unittest.TestCase):
    """A main checkout and a linked worktree cut from it, both without any .tv-* file, plus an
    empty per-user config dir. `self.run_*` helpers give a scrubbed environment: only PATH, a HOME
    inside the sandbox and PLX_TV_CONFIG_DIR, so nothing of the machine's own can leak in."""

    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="tv-config-test-")
        self.addCleanup(tmp.cleanup)
        self.tmp = Path(tmp.name)
        self.home = self.tmp / "home"
        self.home.mkdir()
        self.cfg = self.tmp / "cfg"            # NOT created: the resolver must cope with it absent
        self.main = self.tmp / "main"
        self.lane = self.tmp / "lane"
        self.main.mkdir()
        self.git(self.main, "init", "-q")
        for rel in TREE:
            src = ROOT / rel
            if not src.exists():                # tolerated so the new-file tests FAIL, not crash setUp
                continue
            dst = self.main / rel
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src, dst)
        (self.main / "README").write_text("x\n")
        self.git(self.main, "add", "-A")
        self.git(self.main, "commit", "-q", "-m", "seed")
        self.git(self.main, "worktree", "add", "-q", "-b", "lane", str(self.lane))

    # -- helpers ------------------------------------------------------------------------
    def git(self, cwd, *args):
        subprocess.run([*GIT, "-C", str(cwd), *args], check=True, capture_output=True)

    def env(self, **extra):
        env = {"PATH": os.environ["PATH"], "HOME": str(self.home),
               "PLX_TV_CONFIG_DIR": str(self.cfg)}
        for k, v in extra.items():
            if v is None:
                env.pop(k, None)
            else:
                env[k] = v
        return env

    def resolver(self, repo, *args, **extra_env):
        return subprocess.run([str(repo / "tools" / "tv-config.sh"), *args], capture_output=True,
                              text=True, env=self.env(**extra_env), timeout=30)

    def host(self, repo=None, **extra_env):
        p = self.resolver(repo or self.lane, "host", **extra_env)
        self.assertEqual(p.returncode, 0, p.stderr)
        return p.stdout

    def mac(self, repo=None, **extra_env):
        p = self.resolver(repo or self.lane, "mac", **extra_env)
        self.assertEqual(p.returncode, 0, p.stderr)
        return p.stdout

    def put(self, where, name, text):
        where = Path(where)
        where.mkdir(parents=True, exist_ok=True)
        (where / name).write_text(text)


class ResolverTests(Fixture):
    # (a) a worktree with no file of its own reaches the user dir
    def test_worktree_resolves_the_user_dir_host(self):
        self.put(self.cfg, "tv-host", HOST_USER + "\n")
        self.assertFalse((self.lane / ".tv-host").exists())
        self.assertEqual(self.host(), HOST_USER)

    def test_worktree_resolves_the_user_dir_mac(self):
        self.put(self.cfg, "tv-mac", MAC_USER + "\n")
        self.assertEqual(self.mac(), MAC_USER)

    def test_nothing_configured_prints_nothing_and_succeeds(self):
        self.assertEqual(self.host(), "")
        self.assertEqual(self.mac(), "")

    # (b) repo file > main-checkout file > user dir (the env sits in front, in each tool)
    def test_host_precedence_repo_over_main_over_user(self):
        self.put(self.cfg, "tv-host", HOST_USER)
        self.assertEqual(self.host(), HOST_USER)
        self.put(self.main, ".tv-host", HOST_MAIN)
        self.assertEqual(self.host(), HOST_MAIN)
        self.put(self.lane, ".tv-host", HOST_REPO)
        self.assertEqual(self.host(), HOST_REPO)

    def test_mac_precedence_repo_over_main_over_user(self):
        self.put(self.cfg, "tv-mac", MAC_USER)
        self.assertEqual(self.mac(), MAC_USER)
        self.put(self.main, ".tv-mac", MAC_MAIN)
        self.assertEqual(self.mac(), MAC_MAIN)
        self.put(self.lane, ".tv-mac", MAC_REPO)
        self.assertEqual(self.mac(), MAC_REPO)

    def test_main_checkout_resolves_its_own_file_and_the_user_dir(self):
        # In the main checkout --git-common-dir is the RELATIVE `.git`; the resolver must anchor it.
        self.put(self.cfg, "tv-host", HOST_USER)
        self.assertEqual(self.host(self.main), HOST_USER)
        self.put(self.main, ".tv-host", HOST_MAIN)
        self.assertEqual(self.host(self.main), HOST_MAIN)

    def test_a_relative_common_dir_is_handled_from_a_subdirectory(self):
        self.put(self.main, ".tv-host", HOST_MAIN)
        sub = self.main / "tools"
        p = subprocess.run([str(sub / "tv-config.sh"), "host"], cwd=str(self.tmp),
                           capture_output=True, text=True, env=self.env(), timeout=30)
        self.assertEqual(p.stdout, HOST_MAIN, p.stderr)

    # (c) an empty or whitespace-only file is absent, and values are stripped
    def test_empty_and_whitespace_files_fall_through(self):
        self.put(self.cfg, "tv-host", HOST_USER)
        self.put(self.main, ".tv-host", "")
        self.put(self.lane, ".tv-host", "  \n\t\r\n")
        self.assertEqual(self.host(), HOST_USER)
        self.put(self.cfg, "tv-mac", MAC_USER)
        self.put(self.main, ".tv-mac", "\n")
        self.put(self.lane, ".tv-mac", "   ")
        self.assertEqual(self.mac(), MAC_USER)

    def test_values_are_stripped_of_whitespace_and_newlines(self):
        self.put(self.cfg, "tv-host", "  %s \r\n\n" % HOST_USER)
        self.assertEqual(self.host(), HOST_USER)

    def test_only_an_empty_user_file_means_nothing(self):
        self.put(self.cfg, "tv-host", "\n")
        self.assertEqual(self.host(), "")

    # the escape hatch tv-ssh's own tests lean on
    def test_no_host_file_switches_every_file_off(self):
        self.put(self.cfg, "tv-host", HOST_USER)
        self.put(self.main, ".tv-host", HOST_MAIN)
        self.put(self.lane, ".tv-host", HOST_REPO)
        self.assertEqual(self.host(PLX_TV_NO_HOST_FILE="1"), "")

    # the location itself
    def test_dir_default_is_dot_config_plxnative_under_home(self):
        p = self.resolver(self.lane, "dir", PLX_TV_CONFIG_DIR=None)
        self.assertEqual(p.stdout, str(self.home / ".config" / "plxnative"), p.stderr)

    def test_dir_follows_xdg_config_home(self):
        p = self.resolver(self.lane, "dir", PLX_TV_CONFIG_DIR=None,
                          XDG_CONFIG_HOME=str(self.tmp / "xdg"))
        self.assertEqual(p.stdout, str(self.tmp / "xdg" / "plxnative"), p.stderr)

    def test_dir_override_wins_over_xdg(self):
        p = self.resolver(self.lane, "dir", XDG_CONFIG_HOME=str(self.tmp / "xdg"))
        self.assertEqual(p.stdout, str(self.cfg), p.stderr)

    def test_files_are_read_from_the_default_location(self):
        self.put(self.home / ".config" / "plxnative", "tv-host", HOST_USER)
        self.assertEqual(self.host(PLX_TV_CONFIG_DIR=None), HOST_USER)

    # set-mac: the one writer
    def test_set_mac_writes_the_user_file_privately_and_never_the_repo(self):
        p = self.resolver(self.lane, "set-mac", " %s \n" % MAC_ARP)
        self.assertEqual(p.returncode, 0, p.stderr)
        f = self.cfg / "tv-mac"
        self.assertEqual(f.read_text().strip(), MAC_ARP)
        self.assertEqual(stat.S_IMODE(f.stat().st_mode), 0o600)
        self.assertFalse((self.lane / ".tv-mac").exists())
        self.assertFalse((self.main / ".tv-mac").exists())
        self.assertEqual(self.mac(), MAC_ARP)

    def test_set_mac_refuses_an_empty_value(self):
        p = self.resolver(self.lane, "set-mac", "  ")
        self.assertNotEqual(p.returncode, 0)
        self.assertFalse((self.cfg / "tv-mac").exists())

    def test_unknown_subcommand_is_a_usage_error(self):
        p = self.resolver(self.lane, "bogus")
        self.assertEqual(p.returncode, 2)
        self.assertIn("usage", p.stderr)


class TvSshTests(Fixture):
    """`tools/tv-ssh`'s configured_host: env first (PLX_TV_ADDR, TV, TV_HOST in that order), then the
    resolver. A fake ssh on a PATH holding only the few system tools the wrapper needs."""

    def setUp(self):
        super().setUp()
        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        write_exe(self.bin / "ssh", FAKE_SSH)
        for tool in ("bash", "sh", "env", "dirname", "cat", "grep", "tr", "git", "mkdir", "chmod"):
            found = shutil.which(tool)
            if found:
                (self.bin / tool).symlink_to(found)
        self.log = self.tmp / "calls.log"
        self.log.write_text("")

    def probe_host(self, **extra_env):
        env = self.env(PATH=str(self.bin), FAKE_LOG=str(self.log), FAKE_MODE="key-ok",
                       FAKE_HOST=HOST_USER, **extra_env)
        p = subprocess.run([str(self.lane / "tools" / "tv-ssh"), "ssh", "tv", "echo hi"],
                           capture_output=True, text=True, env=env, timeout=30)
        calls = [ln for ln in self.log.read_text().splitlines() if ln.startswith("ssh ")]
        self.log.write_text("")
        for host in (HOST_USER, HOST_MAIN, HOST_REPO, HOST_ENV, HOST_ENV2):
            if "[root@%s]" % host in (calls[0] if calls else ""):
                return host, p
        return None, p

    def test_configured_host_reaches_the_user_dir_from_a_worktree(self):
        self.put(self.cfg, "tv-host", HOST_USER)
        host, p = self.probe_host()
        self.assertEqual(host, HOST_USER, p.stderr)

    def test_precedence_env_repo_main_user(self):
        self.put(self.cfg, "tv-host", HOST_USER)
        self.assertEqual(self.probe_host()[0], HOST_USER)
        self.put(self.main, ".tv-host", HOST_MAIN)
        self.assertEqual(self.probe_host()[0], HOST_MAIN)
        self.put(self.lane, ".tv-host", HOST_REPO)
        self.assertEqual(self.probe_host()[0], HOST_REPO)
        self.assertEqual(self.probe_host(TV_HOST=HOST_ENV2)[0], HOST_ENV2)
        self.assertEqual(self.probe_host(TV_HOST=HOST_ENV2, TV=HOST_ENV)[0], HOST_ENV)
        self.assertEqual(self.probe_host(TV_HOST=HOST_ENV2, TV=HOST_ENV,
                                         PLX_TV_ADDR=HOST_USER)[0], HOST_USER)

    def test_empty_files_fall_through_to_the_user_dir(self):
        self.put(self.cfg, "tv-host", HOST_USER)
        self.put(self.lane, ".tv-host", "\n")
        self.put(self.main, ".tv-host", " ")
        self.assertEqual(self.probe_host()[0], HOST_USER)

    def test_no_host_file_escape_hatch_covers_the_user_dir_too(self):
        self.put(self.cfg, "tv-host", HOST_USER)
        self.put(self.main, ".tv-host", HOST_MAIN)
        host, p = self.probe_host(PLX_TV_NO_HOST_FILE="1")
        self.assertIsNone(host)
        self.assertIn("no TV configured", p.stderr)
        self.assertEqual(self.log.read_text(), "")

    def test_no_tv_anywhere_names_the_user_file_in_the_error(self):
        host, p = self.probe_host()
        self.assertIsNone(host)
        self.assertIn("no TV configured", p.stderr)
        self.assertIn("plxnative/tv-host", p.stderr)


class WakeTvTests(Fixture):
    """wake-tv.sh `status` against a fake ssh and a stub `arp`: no packet is ever sent. The point is
    where a MAC learned from ARP is WRITTEN -- the user dir, so every lane benefits."""

    def setUp(self):
        super().setUp()
        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        write_exe(self.bin / "ssh", FAKE_SSH)
        write_exe(self.bin / "arp", "#!/bin/sh\necho \"? ($2) at %s on en0 ifscope [ethernet]\"\n" % MAC_ARP)
        self.log = self.tmp / "calls.log"
        self.log.write_text("")
        self.script = self.lane / ".agents" / "skills" / "wake-tv" / "wake-tv.sh"

    def status(self, **extra_env):
        env = self.env(PATH=str(self.bin) + os.pathsep + os.environ["PATH"], FAKE_LOG=str(self.log),
                       FAKE_MODE="key-ok", FAKE_HOST=HOST_USER, **extra_env)
        return subprocess.run(["bash", str(self.script), "status"], capture_output=True, text=True,
                              env=env, timeout=60)

    def test_a_learned_mac_is_cached_in_the_user_dir_not_the_repo(self):
        self.put(self.cfg, "tv-host", HOST_USER)
        p = self.status()
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn("TV: UP", p.stdout)
        self.assertEqual(read(self.cfg / "tv-mac").strip(), MAC_ARP)
        self.assertFalse((self.lane / ".tv-mac").exists())
        self.assertFalse((self.main / ".tv-mac").exists())
        self.assertEqual(stat.S_IMODE((self.cfg / "tv-mac").stat().st_mode), 0o600)

    def test_a_mac_already_known_is_not_relearned(self):
        self.put(self.cfg, "tv-host", HOST_USER)
        self.put(self.cfg, "tv-mac", MAC_USER)
        p = self.status()
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(read(self.cfg / "tv-mac").strip(), MAC_USER)

    def test_an_env_mac_is_not_cached(self):
        self.put(self.cfg, "tv-host", HOST_USER)
        p = self.status(TV_MAC=MAC_REPO)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertFalse((self.cfg / "tv-mac").exists())

    def test_the_host_comes_from_the_user_dir_in_a_worktree(self):
        # No TV_HOST/TV in the env, no .tv-host anywhere: the fake ssh was probed at HOST_USER.
        self.put(self.cfg, "tv-host", HOST_USER)
        self.status()
        calls = self.log.read_text()
        self.assertIn("[root@%s]" % HOST_USER, calls)

    def test_no_host_anywhere_is_a_clear_error(self):
        p = self.status()
        self.assertEqual(p.returncode, 2)
        self.assertIn("no TV host", p.stderr)


class MakefileTests(Fixture):
    """The Makefile's `TV ?=` line, lifted out of the real Makefile into a two-line makefile run in
    the sandbox worktree (the whole Makefile needs the toolchain's inputs just to parse)."""

    def make_tv(self, *args, **extra_env):
        import re
        line = re.search(r"^TV\s+\?=.*$", read(ROOT / "Makefile"), re.M)
        self.assertIsNotNone(line, "the Makefile no longer has a `TV ?=` line")
        (self.lane / "tv.mk").write_text(line.group(0) + "\nprint-tv:\n\t@echo '$(TV)'\n")
        p = subprocess.run(["make", "-s", "-f", "tv.mk", "print-tv", *args], cwd=str(self.lane),
                           capture_output=True, text=True, env=self.env(**extra_env), timeout=60)
        self.assertEqual(p.returncode, 0, p.stderr)
        return p.stdout.strip()

    def test_make_tv_reaches_the_user_dir_from_a_worktree(self):
        self.put(self.cfg, "tv-host", HOST_USER)
        self.assertEqual(self.make_tv(), HOST_USER)

    def test_make_tv_precedence_command_line_repo_main_user(self):
        self.put(self.cfg, "tv-host", HOST_USER)
        self.put(self.main, ".tv-host", HOST_MAIN)
        self.assertEqual(self.make_tv(), HOST_MAIN)
        self.put(self.lane, ".tv-host", HOST_REPO)
        self.assertEqual(self.make_tv(), HOST_REPO)
        self.assertEqual(self.make_tv("TV=" + HOST_ENV), HOST_ENV)

    def test_make_tv_is_empty_without_the_resolver(self):
        (self.lane / "tools" / "tv-config.sh").unlink()
        self.put(self.cfg, "tv-host", HOST_USER)
        self.assertEqual(self.make_tv(), "")


class PythonReaderTests(Fixture):
    """tests/run.py and tools/stream-screen.py ask the shell resolver through tools/tv_config.py.
    The module's SCRIPT is pointed at the sandbox worktree's copy so no real file is consulted."""

    def setUp(self):
        super().setUp()
        import tv_config
        self.tv_config = tv_config
        patcher = mock.patch.object(tv_config, "SCRIPT", str(self.lane / "tools" / "tv-config.sh"))
        patcher.start()
        self.addCleanup(patcher.stop)
        envp = mock.patch.dict(os.environ, {"PLX_TV_CONFIG_DIR": str(self.cfg), "HOME": str(self.home)})
        envp.start()
        self.addCleanup(envp.stop)

    def test_tv_config_host_and_mac(self):
        self.put(self.cfg, "tv-host", HOST_USER)
        self.put(self.cfg, "tv-mac", MAC_USER)
        self.assertEqual(self.tv_config.host(), HOST_USER)
        self.assertEqual(self.tv_config.mac(), MAC_USER)

    def test_tv_config_without_a_resolver_is_empty_not_an_error(self):
        with mock.patch.object(self.tv_config, "SCRIPT", str(self.tmp / "no-such-script")):
            self.assertEqual(self.tv_config.host(), "")
            self.assertEqual(self.tv_config.mac(), "")

    def test_run_py_pipeline_manifest_uses_the_user_dir_address(self):
        self.put(self.cfg, "tv-host", HOST_USER)
        import run
        with mock.patch.object(run, "MANIFEST_LOCAL", str(self.tmp / "no-such-overlay.json")):
            m = run.load_manifest(pipeline_only=True)
        self.assertEqual(m["tv"], HOST_USER)

    def test_run_py_explicit_tv_still_wins(self):
        self.put(self.cfg, "tv-host", HOST_USER)
        import run
        with mock.patch.object(run, "MANIFEST_LOCAL", str(self.tmp / "no-such-overlay.json")):
            m = run.load_manifest(pipeline_only=True, tv_override=HOST_ENV)
        self.assertEqual(m["tv"], HOST_ENV)

    def test_stream_screen_default_host_uses_the_user_dir(self):
        self.put(self.cfg, "tv-host", HOST_USER)
        spec = importlib.util.spec_from_file_location("stream_screen", ROOT / "tools" / "stream-screen.py")
        mod = importlib.util.module_from_spec(spec)
        # Importing runs `TV_HOST = os.environ.get("TV_HOST") or _default_tv_host()`; the sandbox
        # SCRIPT keeps that from reading anything real.
        with mock.patch.dict(os.environ, {"TV_HOST": ""}):
            spec.loader.exec_module(mod)
        self.assertEqual(mod._default_tv_host(), HOST_USER)
        self.assertEqual(mod.TV_HOST, HOST_USER)


class GuardParityTests(Fixture):
    """The outbound guard (a Python hook) cannot shell out to find the user dir, so it computes it
    itself; this keeps that computation and the shell resolver's `dir` the same rule."""

    def guard(self):
        spec = importlib.util.spec_from_file_location(
            "oguard", ROOT / ".claude" / "hooks" / "outbound-guard.py")
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        return mod

    def test_user_config_dir_matches_the_shell_resolver(self):
        g = self.guard()
        for extra in ({"PLX_TV_CONFIG_DIR": str(self.cfg)},
                      {"PLX_TV_CONFIG_DIR": None, "XDG_CONFIG_HOME": str(self.tmp / "xdg")},
                      {"PLX_TV_CONFIG_DIR": None}):
            shell = self.resolver(self.lane, "dir", **extra).stdout
            with mock.patch.dict(os.environ, {k: v for k, v in self.env(**extra).items()}, clear=True):
                self.assertEqual(g.user_config_dir(), shell, extra)


if __name__ == "__main__":
    unittest.main(verbosity=2)
