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


if __name__ == "__main__":
    unittest.main()
