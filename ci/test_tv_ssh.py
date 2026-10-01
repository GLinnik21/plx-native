#!/usr/bin/env python3
"""Grade `tools/tv-ssh`, the one ssh/scp front door to the television, against FAKE `ssh` and
`sshpass` binaries on PATH. No television, no network, no real ssh: the question is purely which
programs get run, with what, and what text reaches the terminal.

What it pins:
  * key accepted        -> `sshpass` is never invoked (and need not even be installed);
  * key rejected        -> the call is retried through `sshpass -p alpine`, and only then;
  * host unreachable    -> no password attempt at all, a fast failure, a message that says "the TV";
  * the placeholders    -> `ssh tv CMD` and `scp F tv:PATH` expand to root@<host> and keep every
                           argument (quoting, spaces, `scp -O`) as ONE argument, stdin intact;
  * no leak             -> neither the password nor the television's address appears on stdout or
                           stderr of the wrapper in any path, errors included;
  * the Makefile        -> `make -n deploy` expands through the wrapper: no `sshpass`, no password,
                           no address anywhere in the commands make would echo.

The address used throughout is from the RFC 5737 documentation range.
"""
import os
import re
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WRAPPER = ROOT / "tools" / "tv-ssh"
HOST = "192.0.2.77"
PASSWORD = "alpine"
IPV4 = re.compile(r"\b\d{1,3}(?:\.\d{1,3}){3}\b")

# Behaviour is chosen by FAKE_MODE: key-ok | key-denied | unreachable.
# `sshpass -p PW ssh ...` is faked as "run ssh with FAKE_VIA_SSHPASS=1", which every mode but
# `unreachable` treats as an accepted password login.
FAKE_SSH = r"""#!/bin/sh
printf 'ssh' >> "$FAKE_LOG"; for a in "$@"; do printf ' [%s]' "$a" >> "$FAKE_LOG"; done; echo >> "$FAKE_LOG"
case "$FAKE_MODE" in
  unreachable)
    echo "ssh: connect to host $FAKE_HOST port 22: Operation timed out" >&2; exit 255 ;;
  key-denied)
    if [ -z "$FAKE_VIA_SSHPASS" ]; then
      echo "root@$FAKE_HOST: Permission denied (publickey,password)." >&2; exit 255
    fi ;;
esac
# Accepted. Echo what the remote command would see; `cat` proves stdin was not eaten by a probe.
eval "last=\${$#}"
if [ "$last" = true ]; then exit 0; fi
if [ "$last" = cat ]; then exec cat; fi
printf 'ran: %s\n' "$last"
exit "${FAKE_REMOTE_RC:-0}"
"""

FAKE_SSHPASS = r"""#!/bin/sh
printf 'sshpass' >> "$FAKE_LOG"; for a in "$@"; do printf ' [%s]' "$a" >> "$FAKE_LOG"; done; echo >> "$FAKE_LOG"
[ "$1" = -p ] || exit 2
shift 2
FAKE_VIA_SSHPASS=1 exec "$@"
"""

FAKE_SCP = r"""#!/bin/sh
printf 'scp' >> "$FAKE_LOG"; for a in "$@"; do printf ' [%s]' "$a" >> "$FAKE_LOG"; done; echo >> "$FAKE_LOG"
case "$FAKE_MODE" in
  unreachable) echo "ssh: connect to host $FAKE_HOST port 22: Operation timed out" >&2; exit 255 ;;
  key-denied) [ -n "$FAKE_VIA_SSHPASS" ] || { echo "root@$FAKE_HOST: Permission denied (publickey,password)." >&2; exit 255; } ;;
esac
exit 0
"""


def write_exe(path, text):
    path.write_text(text)
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


class TvSshTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="tv-ssh-test-")
        self.addCleanup(tmp.cleanup)
        self.dir = Path(tmp.name)
        self.bin = self.dir / "bin"
        self.bin.mkdir()
        write_exe(self.bin / "ssh", FAKE_SSH)
        write_exe(self.bin / "scp", FAKE_SCP)
        self.log = self.dir / "calls.log"
        self.log.write_text("")
        self.with_sshpass = True

    def run_wrapper(self, mode, *args, stdin=None, with_sshpass=True, extra_env=None, host=HOST):
        if with_sshpass:
            write_exe(self.bin / "sshpass", FAKE_SSHPASS)
        elif (self.bin / "sshpass").exists():
            (self.bin / "sshpass").unlink()
        env = {
            # Only the fakes plus the system basics: a real sshpass elsewhere must not be reachable.
            "PATH": f"{self.bin}:/usr/bin:/bin",
            "HOME": str(self.dir),
            "FAKE_LOG": str(self.log),
            "FAKE_MODE": mode,
            "FAKE_HOST": HOST,
            "PLX_TV_ADDR": host,
        }
        env.update(extra_env or {})
        started = time.monotonic()
        proc = subprocess.run(
            [str(WRAPPER), *args], input=stdin, capture_output=True, text=True, env=env, timeout=30)
        proc.elapsed = time.monotonic() - started
        return proc

    def calls(self):
        return [ln for ln in self.log.read_text().splitlines() if ln]

    def assert_no_leak(self, proc):
        text = proc.stdout + proc.stderr
        self.assertNotIn(HOST, text)
        self.assertNotIn(PASSWORD, text)
        self.assertIsNone(IPV4.search(text), text)

    # ---- key accepted --------------------------------------------------------------------
    def test_key_accepted_never_touches_sshpass(self):
        proc = self.run_wrapper("key-ok", "ssh", "tv", "echo hello", with_sshpass=False)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("ran: echo hello", proc.stdout)
        self.assertFalse([c for c in self.calls() if c.startswith("sshpass")])
        self.assert_no_leak(proc)

    def test_key_accepted_even_with_sshpass_installed_does_not_use_it(self):
        proc = self.run_wrapper("key-ok", "ssh", "tv", "true")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertFalse([c for c in self.calls() if c.startswith("sshpass")])

    def test_key_attempt_is_batch_publickey_with_host_key_options(self):
        self.run_wrapper("key-ok", "ssh", "tv", "true")
        first = self.calls()[0]
        for want in ("[BatchMode=yes]", "[PreferredAuthentications=publickey]",
                     "[StrictHostKeyChecking=no]", "[UserKnownHostsFile=/dev/null]",
                     f"[root@{HOST}]"):
            self.assertIn(want, first)
        self.assertRegex(first, r"\[ConnectTimeout=\d+\]")

    # ---- key rejected --------------------------------------------------------------------
    def test_key_rejected_falls_back_to_sshpass_with_the_password(self):
        proc = self.run_wrapper("key-denied", "ssh", "tv", "echo hello")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("ran: echo hello", proc.stdout)
        sshpass = [c for c in self.calls() if c.startswith("sshpass")]
        self.assertEqual(len(sshpass), 1, self.calls())
        self.assertTrue(sshpass[0].startswith(f"sshpass [-p] [{PASSWORD}] [ssh]"), sshpass[0])
        self.assert_no_leak(proc)

    def test_key_rejected_without_sshpass_says_so_and_names_no_address(self):
        proc = self.run_wrapper("key-denied", "ssh", "tv", "true", with_sshpass=False)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("sshpass", proc.stderr)
        self.assert_no_leak(proc)

    def test_scp_key_rejected_falls_back(self):
        proc = self.run_wrapper("key-denied", "scp", "pkg/plxnative", "tv:/media/x/plxnative.new")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        sshpass = [c for c in self.calls() if c.startswith("sshpass")]
        self.assertEqual(len(sshpass), 1, self.calls())
        self.assertIn("[scp]", sshpass[0])
        self.assert_no_leak(proc)

    # ---- unreachable ---------------------------------------------------------------------
    def test_unreachable_never_tries_the_password_and_fails_fast(self):
        proc = self.run_wrapper("unreachable", "ssh", "tv", "echo hello")
        self.assertEqual(proc.returncode, 255)
        self.assertFalse([c for c in self.calls() if c.startswith("sshpass")], self.calls())
        self.assertIn("the TV", proc.stderr)
        self.assertLess(proc.elapsed, 5)
        self.assert_no_leak(proc)

    def test_unreachable_scp_is_the_same(self):
        proc = self.run_wrapper("unreachable", "scp", "a", "tv:/tmp/")
        self.assertEqual(proc.returncode, 255)
        self.assertFalse([c for c in self.calls() if c.startswith("sshpass")], self.calls())
        self.assert_no_leak(proc)

    def test_no_tv_configured_is_the_make_style_sentence(self):
        proc = self.run_wrapper("key-ok", "ssh", "tv", "true", host="",
                                extra_env={"TV": "", "TV_HOST": "", "PLX_TV_NO_HOST_FILE": "1"})
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("no TV configured", proc.stderr)
        self.assertEqual(self.calls(), [])

    # ---- argument handling ---------------------------------------------------------------
    def test_scp_placeholder_expands_and_keeps_legacy_flag(self):
        self.run_wrapper("key-ok", "scp", "a b", "c", "tv:/tmp/dir/")
        scp = [c for c in self.calls() if c.startswith("scp")][-1]
        self.assertIn("[-O]", scp)
        self.assertIn("[a b]", scp)
        self.assertIn(f"[root@{HOST}:/tmp/dir/]", scp)
        self.assertIn("[StrictHostKeyChecking=no]", scp)

    def test_scp_download_placeholder_in_first_position(self):
        self.run_wrapper("key-ok", "scp", "tv:/tmp/x.jsonl", "pkg/x.jsonl")
        scp = [c for c in self.calls() if c.startswith("scp")][-1]
        self.assertIn(f"[root@{HOST}:/tmp/x.jsonl]", scp)
        self.assertIn("[pkg/x.jsonl]", scp)

    def test_remote_command_stays_one_argument(self):
        cmd = "test -d '/media/a b' && echo \"it's $HOME\""
        proc = self.run_wrapper("key-ok", "ssh", "tv", cmd)
        self.assertIn(f"ran: {cmd}", proc.stdout)

    def test_explicit_root_at_host_form_is_accepted(self):
        proc = self.run_wrapper("key-denied", "ssh", f"root@{HOST}", "echo hi", host="")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(len([c for c in self.calls() if c.startswith("sshpass")]), 1)

    def test_stdin_reaches_the_command(self):
        proc = self.run_wrapper("key-ok", "ssh", "tv", "cat", stdin="payload-line\n")
        self.assertEqual(proc.stdout, "payload-line\n")
        proc = self.run_wrapper("key-denied", "ssh", "tv", "cat", stdin="payload-line\n")
        self.assertEqual(proc.stdout, "payload-line\n")

    def test_exit_status_of_the_remote_command_is_propagated(self):
        proc = self.run_wrapper("key-ok", "ssh", "tv", "false", extra_env={"FAKE_REMOTE_RC": "7"})
        self.assertEqual(proc.returncode, 7)


class MakefileDryRun(unittest.TestCase):
    """`make -n deploy` prints every command the real run would echo. None of them may carry the
    password, `sshpass`, or the address -- the transcript of a deploy is read by people and agents."""

    def test_dry_run_deploy_is_silent_about_address_and_password(self):
        env = dict(os.environ)
        env.pop("TV", None)
        proc = subprocess.run(
            ["make", "-n", "deploy", f"TV={HOST}", "FLAVOR=debug"],
            cwd=ROOT, capture_output=True, text=True, env=env, timeout=300)
        self.assertEqual(proc.returncode, 0, proc.stderr[-500:])
        out = proc.stdout + proc.stderr
        self.assertIn("tools/tv-ssh", out)
        # Report the offending LINES only: a failing dry run is thousands of lines long.
        leaks = [ln[:160] for ln in out.splitlines()
                 if HOST in ln or "sshpass" in ln or PASSWORD in ln]
        self.assertEqual(leaks[:5], [], f"{len(leaks)} leaking lines")


if __name__ == "__main__":
    unittest.main(verbosity=2)
