#!/usr/bin/env python3
"""Cases for the cloud SessionStart unshallow hook. `python3 .claude/hooks/cloud-unshallow-test.py`.

Against a throwaway origin and a `--depth 1` clone of it: the hook must unshallow the clone when
`CLAUDE_CODE_REMOTE=true`, must leave it alone otherwise (a laptop session never fetches), and must
exit 0 even when the fetch fails.
"""
import os
import subprocess
import sys
import tempfile
import unittest

HOOK = os.path.join(os.path.dirname(os.path.abspath(__file__)), "cloud-unshallow.sh")


def git(*args, cwd):
    return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True).stdout.strip()


class Fixture:
    def __enter__(self):
        self.tmp = tempfile.TemporaryDirectory()
        origin = os.path.join(self.tmp.name, "origin")
        os.mkdir(origin)
        git("init", "-q", cwd=origin)
        for n in range(3):
            with open(os.path.join(origin, "f"), "w") as f:
                f.write(str(n))
            git("add", "f", cwd=origin)
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", str(n), cwd=origin)
        self.clone = os.path.join(self.tmp.name, "clone")
        git("clone", "-q", "--depth", "1", "file://" + origin, self.clone, cwd=self.tmp.name)
        return self

    def __exit__(self, *exc):
        self.tmp.cleanup()

    def run(self, remote):
        env = {k: v for k, v in os.environ.items() if k != "CLAUDE_CODE_REMOTE"}
        env["CLAUDE_PROJECT_DIR"] = self.clone
        if remote is not None:
            env["CLAUDE_CODE_REMOTE"] = remote
        return subprocess.run([HOOK], env=env, capture_output=True, text=True, timeout=60)

    def shallow(self):
        return git("rev-parse", "--is-shallow-repository", cwd=self.clone) == "true"


class CloudUnshallow(unittest.TestCase):
    def test_a_cloud_session_gets_the_full_history(self):
        with Fixture() as f:
            self.assertTrue(f.shallow())
            out = f.run("true")
            self.assertEqual(out.returncode, 0, out.stderr)
            self.assertFalse(f.shallow())
            self.assertEqual(git("rev-list", "--count", "HEAD", cwd=f.clone), "3")

    def test_a_local_session_never_fetches(self):
        with Fixture() as f:
            for remote in (None, "", "false"):
                self.assertEqual(f.run(remote).returncode, 0)
                self.assertTrue(f.shallow(), f"CLAUDE_CODE_REMOTE={remote!r} must not fetch")

    def test_a_failed_fetch_still_exits_zero_and_says_why(self):
        with Fixture() as f:
            git("remote", "set-url", "origin", "file:///nonexistent/plx-origin", cwd=f.clone)
            out = f.run("true")
            self.assertEqual(out.returncode, 0)
            self.assertTrue(f.shallow())
            self.assertIn("cloud-unshallow:", out.stderr)


if __name__ == "__main__":
    sys.exit(0 if unittest.main(exit=False).result.wasSuccessful() else 1)
