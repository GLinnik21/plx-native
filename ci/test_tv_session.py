#!/usr/bin/env python3
"""Host-only regression tests for tv-session.sh's deploy verification.

The command under test is the real Bash ``ensure_binary`` function.  The Makefile query,
deploy command, local hash tools, and TV query are mocked in a temporary harness so these
cases never contact a television or run a real deploy.
"""
from __future__ import annotations

import os
import pathlib
import subprocess
import tempfile
import textwrap
import unittest


SCRIPT = pathlib.Path(__file__).parents[1] / "tools" / "tv-session.sh"


class EnsureBinary(unittest.TestCase):
    def run_case(
        self,
        *,
        local_before: str,
        local_after: str,
        remote_before: str,
        remote_after: str,
        deploy_status: int = 0,
        hash_status: int = 0,
        remote_status: int = 0,
        hash_output: str | None = None,
        presence_before: str = "present",
        presence_after: str = "present",
        presence_status: int = 0,
    ) -> tuple[int, str, int]:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "pkg").mkdir()
            (root / "tools").mkdir()
            (root / "pkg" / "plxnative").write_bytes(b"placeholder")
            script_link = root / "tools" / "tv-session.sh"
            script_link.symlink_to(SCRIPT)

            state = root / "state"
            state.write_text("before", encoding="utf-8")
            bin_dir = root / "bin"
            bin_dir.mkdir()

            (bin_dir / "make").write_text(
                textwrap.dedent(
                    f"""\
                    #!/bin/sh
                    case " $* " in
                      *" print-flavor "*)
                        printf '%s\\n' debug com.beb.plxnative.debug "$0/app" "$0/run" "$0/events" 8911 fake-host
                        ;;
                      *)
                        printf '%s\\n' deploy-called >> "{root / 'calls'}"
                        if [ "{deploy_status}" -ne 0 ]; then
                          echo 'simulated deploy failure' >&2
                          exit {deploy_status}
                        fi
                        printf '%s' deployed > "{state}"
                        ;;
                    esac
                    """
                ),
                encoding="utf-8",
            )
            (bin_dir / "md5").write_text(
                textwrap.dedent(
                    f"""\
                    #!/bin/sh
                    if [ "{hash_status}" -ne 0 ]; then exit {hash_status}; fi
                    if [ "{hash_output}" != "None" ]; then
                      printf '%s' "{hash_output}"
                    elif [ -f "{state}" ] && [ "$(cat "{state}")" = deployed ]; then
                      printf '%s' "{local_after}"
                    else
                      printf '%s' "{local_before}"
                    fi
                    """
                ),
                encoding="utf-8",
            )
            (bin_dir / "md5sum").write_text(
                textwrap.dedent(
                    f"""\
                    #!/bin/sh
                    if [ "{hash_status}" -ne 0 ]; then exit {hash_status}; fi
                    printf '%s  %s\\n' "{local_before}" "$1"
                    """
                ),
                encoding="utf-8",
            )
            for command in bin_dir.iterdir():
                command.chmod(0o755)

            harness = textwrap.dedent(
                f"""\
                source "$1" selftest
                tvq() {{
                  local presence="{presence_before}"
                  [ "$(cat "{state}")" = deployed ] && presence="{presence_after}"
                  case "$1" in
                    "cd "*)
                      printf '%s\\n' "$presence"
                      return {presence_status}
                      ;;
                  esac
                  [ "$presence" = missing ] && return 1
                  if [ "{remote_status}" -ne 0 ]; then return {remote_status}; fi
                  if [ "$(cat "{state}")" = deployed ]; then
                    printf '%s  %s\\n' "{remote_after}" "$APPDIR/plxnative"
                  else
                    printf '%s  %s\\n' "{remote_before}" "$APPDIR/plxnative"
                  fi
                }}
                ensure_binary
                rc=$?
                printf 'RESULT:%s\\n' "$rc"
                """
            )
            environment = os.environ.copy()
            environment["PATH"] = f"{bin_dir}:{environment['PATH']}"
            result = subprocess.run(
                ["bash", "-c", harness, "ensure-binary", str(script_link)],
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                env=environment,
                check=False,
                timeout=10,
            )
            calls = (root / "calls").read_text(encoding="utf-8").count("deploy-called") if (root / "calls").exists() else 0
            return result.returncode, result.stdout, calls

    def assert_result(self, result: tuple[int, str, int], expected: int, calls: int) -> str:
        process_status, output, deploy_calls = result
        self.assertEqual(process_status, 0, output)
        self.assertIn(f"RESULT:{expected}", output)
        self.assertEqual(deploy_calls, calls, output)
        return output

    def test_unchanged_binary_skips_deploy(self):
        output = self.assert_result(
            self.run_case(
                local_before="a" * 32,
                local_after="a" * 32,
                remote_before="a" * 32,
                remote_after="a" * 32,
            ),
            expected=0,
            calls=0,
        )
        self.assertIn("deployed binary matches local build", output)

    def test_rebuild_during_deploy_is_verified_against_new_local_binary(self):
        output = self.assert_result(
            self.run_case(
                local_before="a" * 32,
                local_after="b" * 32,
                remote_before="c" * 32,
                remote_after="b" * 32,
            ),
            expected=0,
            calls=1,
        )
        self.assertIn("deployed + md5 verified", output)

    def test_real_post_deploy_mismatch_fails(self):
        output = self.assert_result(
            self.run_case(
                local_before="a" * 32,
                local_after="b" * 32,
                remote_before="c" * 32,
                remote_after="c" * 32,
            ),
            expected=1,
            calls=1,
        )
        self.assertIn("deploy did not land", output)

    def test_failed_deploy_fails_with_make_output(self):
        output = self.assert_result(
            self.run_case(
                local_before="a" * 32,
                local_after="b" * 32,
                remote_before="c" * 32,
                remote_after="b" * 32,
                deploy_status=7,
            ),
            expected=1,
            calls=1,
        )
        self.assertIn("make FLAVOR=debug deploy failed", output)
        self.assertIn("simulated deploy failure", output)

    def test_empty_local_hash_fails_closed(self):
        output = self.assert_result(
            self.run_case(
                local_before="a" * 32,
                local_after="a" * 32,
                remote_before="",
                remote_after="",
                hash_output="",
            ),
            expected=1,
            calls=0,
        )
        self.assertIn("local binary hash is empty", output)

    def test_failed_remote_hash_fails_closed(self):
        output = self.assert_result(
            self.run_case(
                local_before="a" * 32,
                local_after="a" * 32,
                remote_before="a" * 32,
                remote_after="",
                remote_status=1,
            ),
            expected=1,
            calls=0,
        )
        self.assertIn("could not read deployed binary hash", output)

    def test_failed_local_hash_fails_closed(self):
        output = self.assert_result(
            self.run_case(
                local_before="a" * 32,
                local_after="a" * 32,
                remote_before="a" * 32,
                remote_after="a" * 32,
                hash_status=1,
            ),
            expected=1,
            calls=0,
        )
        self.assertIn("could not read local binary hash", output)

    def test_empty_remote_hash_fails_closed(self):
        output = self.assert_result(
            self.run_case(
                local_before="a" * 32,
                local_after="a" * 32,
                remote_before="",
                remote_after="",
            ),
            expected=1,
            calls=0,
        )
        self.assertIn("deployed binary hash is empty", output)

    def test_missing_binary_deploys_once_and_verifies_rebuilt_bytes(self):
        output = self.assert_result(
            self.run_case(
                local_before="a" * 32, local_after="b" * 32,
                remote_before="", remote_after="b" * 32,
                presence_before="missing",
            ), expected=0, calls=1,
        )
        self.assertIn("deployed + md5 verified", output)

    def test_binary_still_missing_after_deploy_fails_without_retry(self):
        output = self.assert_result(
            self.run_case(
                local_before="a" * 32, local_after="b" * 32,
                remote_before="", remote_after="",
                presence_before="missing", presence_after="missing",
            ), expected=1, calls=1,
        )
        self.assertIn("could not re-read deployed binary hash", output)

    def test_unconfirmed_absence_never_deploys(self):
        for response, status in [("missing", 255), ("", 0), ("garbage", 0),
                                 ("missing extra", 0), ("present", 1)]:
            with self.subTest(response=response, status=status):
                self.assert_result(
                    self.run_case(
                        local_before="a" * 32, local_after="b" * 32,
                        remote_before="", remote_after="b" * 32,
                        presence_before=response, presence_status=status,
                    ), expected=1, calls=0,
                )

    def test_present_binary_with_invalid_hash_never_deploys(self):
        self.assert_result(
            self.run_case(
                local_before="a" * 32, local_after="b" * 32,
                remote_before="invalid", remote_after="b" * 32,
            ), expected=1, calls=0,
        )


class GuestIdentity(unittest.TestCase):
    """Regression for the 2026-09-10 TV session 5 defect: `up --guest` printed a warning and
    then silently pushed the OWNER's token anyway, so a real playback wrote progress into the
    household Plex account (clearing it needed /:/unscrobble, which also reset that item's
    viewCount -- real data loss, not a test artifact).

    `resolve_identity` is the single place that decision is made now, and it is host-only and
    pure enough to unit-test directly: it never calls ssh/tv, and its one subprocess
    (`tests/run.py --print-test-token`, tv-session.sh's re-use of run.py's own managed-user
    resolution) is replaced here with a fake `python3` on PATH so these cases never touch
    plex.tv, src/config.local.h or a television.
    """

    def _run(self, script_body, *, guest_ok):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "tools").mkdir()
            (root / "tests").mkdir()
            # content is irrelevant -- the fake python3 below never reads it -- but
            # resolve_guest_token's `cd "$REPO/tests"` must have somewhere real to land.
            (root / "tests" / "run.py").write_text("# stub for GuestIdentity tests\n", encoding="utf-8")
            script_link = root / "tools" / "tv-session.sh"
            script_link.symlink_to(SCRIPT)

            bin_dir = root / "bin"
            bin_dir.mkdir()
            # Answers the ONE batched `make -s ... print-flavor print-appid print-appdir
            # print-rundir print-eventlog print-appport print-tv` query the script makes at
            # parse time, in that exact order -- same shape as EnsureBinary's fake `make` above.
            (bin_dir / "make").write_text(
                "#!/bin/sh\n"
                "printf '%s\\n' debug com.beb.plxnative.debug /app /run /events 8911 fake-host\n",
                encoding="utf-8",
            )
            (bin_dir / "python3").write_text(
                textwrap.dedent(
                    f"""\
                    #!/bin/sh
                    if [ "$1" = "run.py" ] && [ "$2" = "--print-test-token" ]; then
                      if [ "{guest_ok}" = "1" ]; then
                        printf '%s' fake-guest-token-xyz
                        exit 0
                      fi
                      echo 'no test_user in manifest.local.json -- nothing to resolve as a guest identity' >&2
                      exit 1
                    fi
                    exit 0
                    """
                ),
                encoding="utf-8",
            )
            for command in bin_dir.iterdir():
                command.chmod(0o755)

            # `selftest` as $1 is what lets the script's own top-of-file HOST/TV requirement
            # skip itself (see the script's `[ -n "${HOST:-}" ] || [ "${1:-}" = selftest ]`
            # guard) -- the same trick cmd_selftest's own invocation relies on.
            harness = 'source "$1" selftest\n' + script_body
            environment = os.environ.copy()
            environment["PATH"] = f"{bin_dir}:{environment['PATH']}"
            result = subprocess.run(
                ["bash", "-c", harness, "guest-identity", str(script_link)],
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                env=environment,
                check=False,
                timeout=10,
            )
            return result.returncode, result.stdout

    def test_guest_resolves_the_managed_user_token(self):
        rc, out = self._run(
            textwrap.dedent(
                """\
                guest=1; owner=0; no_token=0; server_set=0; server_slot=""
                resolve_identity; echo "RC:$?"
                echo "PUSH_GUEST:$push_guest"
                echo "PUSH_OWNER:$push_owner"
                echo "TOKEN:$GUEST_TOKEN"
                """
            ),
            guest_ok="1",
        )
        self.assertEqual(rc, 0, out)
        self.assertIn("RC:0", out)
        self.assertIn("PUSH_GUEST:1", out)
        self.assertIn("PUSH_OWNER:0", out)
        self.assertIn("TOKEN:fake-guest-token-xyz", out)

    def test_unresolvable_guest_refuses_and_never_falls_back_to_owner(self):
        rc, out = self._run(
            textwrap.dedent(
                """\
                guest=1; owner=0; no_token=0; server_set=0; server_slot=""
                resolve_identity; echo "RC:$?"
                echo "PUSH_GUEST:$push_guest"
                echo "PUSH_OWNER:$push_owner"
                """
            ),
            guest_ok="0",
        )
        self.assertEqual(rc, 0, out)  # the bash PROCESS exits clean; resolve_identity itself fails
        self.assertIn("RC:1", out)
        self.assertIn("PUSH_GUEST:0", out)
        # THE regression: a failed guest resolution must never leave push_owner=1, which is what
        # would silently boot the household account instead of refusing.
        self.assertIn("PUSH_OWNER:0", out)
        self.assertIn("cannot resolve a guest identity", out)
        self.assertIn("up --owner", out)

    def test_owner_identity_is_explicit(self):
        rc, out = self._run(
            textwrap.dedent(
                """\
                guest=0; owner=1; no_token=0; server_set=0; server_slot=""
                resolve_identity; echo "RC:$?"
                echo "PUSH_OWNER:$push_owner"
                echo "DESC:$identity_desc"
                """
            ),
            guest_ok="0",
        )
        self.assertEqual(rc, 0, out)
        self.assertIn("RC:0", out)
        self.assertIn("PUSH_OWNER:1", out)
        self.assertIn("DESC:owner", out)

    def test_bare_up_identity_defaults_to_owner(self):
        # Neither --guest nor --owner: this is what a plain `tv-session.sh up` resolves to, and
        # it must say so (identity_desc) BEFORE cmd_up goes anywhere near the television.
        rc, out = self._run(
            textwrap.dedent(
                """\
                guest=0; owner=0; no_token=0; server_set=0; server_slot=""
                resolve_identity; echo "RC:$?"
                echo "PUSH_OWNER:$push_owner"
                echo "DESC:$identity_desc"
                """
            ),
            guest_ok="0",
        )
        self.assertEqual(rc, 0, out)
        self.assertIn("PUSH_OWNER:1", out)
        self.assertIn("DESC:owner", out)

    def test_no_token_identity_is_the_stored_session_not_a_fourth_owner_path(self):
        rc, out = self._run(
            textwrap.dedent(
                """\
                guest=0; owner=0; no_token=1; server_set=0; server_slot=""
                resolve_identity; echo "RC:$?"
                echo "PUSH_GUEST:$push_guest"
                echo "PUSH_OWNER:$push_owner"
                echo "DESC:$identity_desc"
                """
            ),
            guest_ok="0",
        )
        self.assertEqual(rc, 0, out)
        self.assertIn("PUSH_GUEST:0", out)
        self.assertIn("PUSH_OWNER:0", out)
        self.assertIn("stored session", out)

    def test_up_refuses_guest_combined_with_a_screen_that_forces_the_stored_session(self):
        # --screen profiles forces no_token=1 (the picker needs no injected token at all) --
        # this must REFUSE rather than silently drop --guest and boot the stored session/owner.
        # Exits before any TV contact, so no `tv`/`tvq`/lock mocking is needed here.
        rc, out = self._run(
            'cmd_up --screen profiles --guest\necho "AFTER:$?"\n',
            guest_ok="1",
        )
        self.assertEqual(rc, 1, out)
        self.assertNotIn("AFTER:", out)  # cmd_up's `exit 1` must end the process, not just return
        self.assertIn("cannot be combined", out)


if __name__ == "__main__":
    unittest.main()
