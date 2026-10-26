#!/usr/bin/env python3
"""tools/pr-land.py against a fake `gh`: the gate's refusals, the wait loop, the merge and its verify.

Hermetic. `gh` is a script first on PATH that answers from a scenario file and logs every call; the
sleeper and the clock are injected, so no test waits. The merge's verify step runs real `git` against
a bare origin and a clone made in a temporary directory. The secret scan imports a COPY of
.claude/hooks/outbound-guard.py from a temporary repo root holding a fake `.tv-host`, with the per-user
TV config pointed at an empty directory, so no real private file is read.

What this pins:

* #387: a failed check never reaches `gh pr merge`, and neither does a pending, missing, timed-out,
  draft, closed, behind, dirty or head-moved PR; `--auto` and `gh run list` are never used;
* the polling: pending turns green, an empty list after a push turns green, UNKNOWN turns CLEAN, a
  required check that never appears is exit 4, the backoff is 30 to 60 s, a base requiring nothing
  (release/v1.2) needs the same check list on two polls, `gh` errors are retried and then given up on;
* UNSTABLE (the required checks pass, a non-required one is not done): a running row is exit 4 and
  turns green on CLEAN, a failed row is exit 1 naming it, all rows passing is lag and never green;
* the order when several things are wrong at once: refusals, head moved, red, a skipping required
  check, not yet (the order of `judge`, which the script's docstring states);
* the merge: the exact `gh pr merge` arguments, the `expected head oid` race (retried when the head
  is unchanged and then refused, exit 5 when it moved), and the verify against origin/main, whose every
  failure says that the merge went through and which never ends in a traceback on odd JSON;
* a planted secret in the subject or the body is refused before any `gh` call, without being repeated.

The JSON below is real output of read-only gh calls on this public repository.
"""
from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "tools" / "pr-land.py"
spec = importlib.util.spec_from_file_location("pr_land", SCRIPT)
pr_land = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pr_land)

# Real output of read-only gh calls against this public repository, captured 2026-10-26.
# `gh pr view 1049 --json headRefOid,state,isDraft,baseRefName,mergeStateStatus,mergeCommit` (open, CLEAN)
VIEW_OPEN = json.loads('{"baseRefName":"main","headRefOid":"0c12ea80de40072eb5fd053eb12f67dfe602e4e9","isDraft":false,"mergeCommit":null,"mergeStateStatus":"CLEAN","state":"OPEN"}')
# the same call on #1036 (merged): mergeStateStatus is UNKNOWN, mergeCommit is an object
VIEW_MERGED = json.loads('{"baseRefName":"main","headRefOid":"6e83ee99877b94620bda32f49d5cf612e032077f","isDraft":false,"mergeCommit":{"oid":"0a367de758374e1cd264b149ae57e40ef0ed1dc3"},"mergeStateStatus":"UNKNOWN","state":"MERGED"}')
# `gh pr checks 1049 --json name,state,bucket,workflow`: 19 rows, all pass; CodeQL's workflow is ""
CHECKS_OPEN = json.loads(
    '[{"bucket":"pass","name":"host checks (NOT a device gate)","state":"SUCCESS","workflow":"CI"},{"bucket":"pass","name":"cross-build (armv7) + artifact assertions","state":"SUCCESS","workflow":"CI"},{"bucket":"pass","name":"CodeQL","state":"SUCCESS","workflow":""},{"bucket":"pass","name":"armv7 cross-build and artifact assertions","state":"SUCCESS","workflow":"CI"},{"bucket":"pass","name":"host unit tests (default features)","state":"SUCCESS","workflow":"CI"},{"bucket":"pass","name":"host harness tests","state":"SUCCESS","workflow":"CI"},{"bucket":"pass","name":"host python/shell/C gates","state":"SUCCESS","workflow":"CI"},{"bucket":"pass","name":"host lint + type-checks","state":"SUCCESS","workflow":"CI"},{"bucket":"pass","name":"host unit tests (hostsim)","state":"SUCCESS","workflow":"CI"},{"bucket":"pass","name":"Windows launcher syntax","state":"SUCCESS","workflow":"Simulator CI"},{"bucket":"pass","name":"macOS simulator build + launch","state":"SUCCESS","workflow":"Simulator CI"},{"bucket":"pass","name":"which gates apply","state":"SUCCESS","workflow":"CI"},{"bucket":"pass","name":"macOS tests against bundled FFmpeg and ASS","state":"SUCCESS","workflow":"Simulator CI"},{"bucket":"pass","name":"Linux simulator build + launch","state":"SUCCESS","workflow":"Simulator CI"},{"bucket":"pass","name":"Analyze (rust)","state":"SUCCESS","workflow":"CodeQL"},{"bucket":"pass","name":"Analyze (python)","state":"SUCCESS","workflow":"CodeQL"},{"bucket":"pass","name":"Analyze (javascript-typescript)","state":"SUCCESS","workflow":"CodeQL"},{"bucket":"pass","name":"Analyze (c-cpp)","state":"SUCCESS","workflow":"CodeQL"},{"bucket":"pass","name":"Analyze (actions)","state":"SUCCESS","workflow":"CodeQL"}]'
)
# the non-pass rows of #1039 (three failures) and #1007 (five skipping), unchanged
FAILED_ROWS = json.loads('[{"bucket":"fail","name":"host checks (NOT a device gate)","state":"FAILURE","workflow":"CI"},{"bucket":"fail","name":"host unit tests (default features)","state":"FAILURE","workflow":"CI"},{"bucket":"fail","name":"host unit tests (hostsim)","state":"FAILURE","workflow":"CI"}]')
SKIPPING_ROWS = json.loads('[{"bucket":"skipping","name":"refresh site latest-nightly pointer","state":"SKIPPED","workflow":"Nightly"},{"bucket":"skipping","name":"plan today\'s nightly","state":"SKIPPED","workflow":"Nightly"},{"bucket":"skipping","name":"prune nightly releases older than 30 days","state":"SKIPPED","workflow":"Nightly"},{"bucket":"skipping","name":"publish nightly prerelease","state":"SKIPPED","workflow":"Nightly"},{"bucket":"skipping","name":"ARM build + gates","state":"SKIPPED","workflow":"Nightly"}]')
# `gh api repos/{owner}/{repo}/rules/branches/main`; release/v1.2 answers `[]`
RULES_MAIN = json.loads('[{"type":"deletion","ruleset_source_type":"Repository","ruleset_source":"GLinnik21/plx-native","ruleset_id":25077421},{"type":"non_fast_forward","ruleset_source_type":"Repository","ruleset_source":"GLinnik21/plx-native","ruleset_id":25077421},{"type":"required_status_checks","parameters":{"strict_required_status_checks_policy":false,"do_not_enforce_on_create":false,"required_status_checks":[{"context":"cross-build (armv7) + artifact assertions","integration_id":15368},{"context":"host checks (NOT a device gate)","integration_id":15368}]},"ruleset_source_type":"Repository","ruleset_source":"GLinnik21/plx-native","ruleset_id":25077421}]')
# `gh pr checks` on a branch with no checks (#1030): exit 1, empty stdout, this on stderr
NO_CHECKS_STDERR = "no checks reported on the 'fix-869-letter-rail' branch\n"
# the check-runs of merge commit 0a367de75 (#1036), reduced to the fields the script prints
CHECK_RUNS = json.loads(
    '{"total_count":18,"check_runs":[{"name":"host checks (NOT a device gate)","status":"completed","conclusion":"failure"},{"name":"cross-build (armv7) + artifact assertions","status":"completed","conclusion":"failure"},{"name":"armv7 cross-build and artifact assertions","status":"completed","conclusion":"cancelled"},{"name":"host unit tests (default features)","status":"completed","conclusion":"cancelled"},{"name":"host lint + type-checks","status":"completed","conclusion":"cancelled"},{"name":"host harness tests","status":"completed","conclusion":"cancelled"},{"name":"host python/shell/C gates","status":"completed","conclusion":"cancelled"},{"name":"host unit tests (hostsim)","status":"completed","conclusion":"cancelled"},{"name":"Analyze (python)","status":"completed","conclusion":"success"},{"name":"Analyze (javascript-typescript)","status":"completed","conclusion":"success"},{"name":"Analyze (rust)","status":"completed","conclusion":"success"},{"name":"Analyze (actions)","status":"completed","conclusion":"success"},{"name":"Analyze (c-cpp)","status":"completed","conclusion":"success"},{"name":"macOS tests against bundled FFmpeg and ASS","status":"completed","conclusion":"cancelled"},{"name":"Windows launcher syntax","status":"completed","conclusion":"success"},{"name":"Linux simulator build + launch","status":"completed","conclusion":"cancelled"},{"name":"macOS simulator build + launch","status":"completed","conclusion":"cancelled"},{"name":"which gates apply","status":"completed","conclusion":"success"}]}'
)


HEAD = VIEW_OPEN["headRefOid"]
OTHER_HEAD = "b" * 40
SUBJECT = "Fix the thing (#1049)"
BODY = "What changed and why.\n\nGates: make check green.\n"
# A value that passes outbound-guard's `usable()` (8+ chars, no whitespace, not a placeholder word);
# it is planted in a FAKE `.tv-host` of a temporary root, so no real private value is involved.
PLANTED = "PLANTED-SECRET-00000000"
# A fake 22-character token value; the test formats it in after `X-Plex-Token=` at run time so this
# source never holds the key next to a 20+ character value (ci/source_bundle.py would refuse the file).
TOKEN_SHAPED = "AbCdEfGh0123456789xyZq"
HOST_CHECKS = "host checks (NOT a device gate)"
HOSTSIM = "host unit tests (hostsim)"
CODEQL_RUST = "Analyze (rust)"          # not a required context of main; it finishes long after the others
UNSTABLE = dict(VIEW_OPEN, mergeStateStatus="UNSTABLE")
MERGE_WENT_THROUGH = "gh pr merge succeeded; "
RACE = "GraphQL: expected head oid does not match the current head (mergePullRequest)\n"

GIT_ENV = {
    "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.invalid",
    "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.invalid",
}

# The fake `gh`: it classifies the call by its arguments, answers from the scenario file (a queue per
# kind; the last answer repeats), and appends every call to a log. A call no scenario entry covers
# (`gh run list`, a misspelt subcommand) exits 99 and is logged as "unexpected".
FAKE_GH = r'''#!%(python)s
import json, os, sys, time

argv = sys.argv[1:]


def kind(argv):
    if argv[:2] == ["pr", "checks"]:
        return "checks"
    if argv[:2] == ["pr", "merge"]:
        return "merge"
    if argv[:2] == ["pr", "view"]:
        fields = argv[argv.index("--json") + 1] if "--json" in argv else ""
        if fields == "body":
            return "body"
        if "mergeCommit" in fields:
            return "verify"
        if "mergeStateStatus" in fields:
            return "view"
        return "reread"
    if argv[:1] == ["api"] and len(argv) > 1:
        if "/rules/branches/" in argv[1]:
            return "rules"
        if "/check-runs" in argv[1]:
            return "runs"
    return "unexpected"


stdin = ""
if "--body-file" in argv and argv[argv.index("--body-file") + 1] == "-":
    stdin = sys.stdin.read()
k = kind(argv)
path = os.environ["FAKE_GH_SCENARIO"]
state = json.load(open(path))
n = state["count"].get(k, 0)
state["count"][k] = n + 1
json.dump(state, open(path, "w"))
with open(os.environ["FAKE_GH_LOG"], "a") as log:
    log.write(json.dumps({"kind": k, "argv": argv, "stdin": stdin}) + "\n")
queue = state["responses"].get(k)
if not queue:
    sys.stderr.write("fake gh: no scripted answer for %%s: %%s\n" %% (k, argv))
    sys.exit(99)
r = queue[min(n, len(queue) - 1)]
time.sleep(r.get("delay", 0))
sys.stdout.write(r["stdout"])
sys.stderr.write(r["stderr"])
sys.exit(r["rc"])
'''


def resp(stdout="", stderr="", rc=0, delay=0):
    if not isinstance(stdout, str):
        stdout = json.dumps(stdout)
    return {"stdout": stdout, "stderr": stderr, "rc": rc, "delay": delay}


def view(**over):
    return dict(VIEW_OPEN, **over)


def mark(name, bucket, state, rows=None):
    return [dict(c, bucket=bucket, state=state) if c["name"] == name else c for c in (rows or CHECKS_OPEN)]


def without(*names, rows=None):
    return [c for c in (rows or CHECKS_OPEN) if c["name"] not in names]


class Clock:
    """Time that only moves when the script sleeps: no test waits."""

    def __init__(self):
        self.t = 1000.0
        self.sleeps = []

    def now(self):
        return self.t

    def sleep(self, seconds):
        self.sleeps.append(seconds)
        self.t += seconds


class Result:
    def __init__(self, code, out, err, sleeps, calls):
        self.code, self.out, self.err, self.sleeps, self.calls = code, out, err, sleeps, calls

    def text(self):
        return "exit %s\n--- stdout\n%s--- stderr\n%s--- gh calls\n%s" % (
            self.code, self.out, self.err, "\n".join(json.dumps(c) for c in self.calls))

    def of(self, kind):
        return [c for c in self.calls if c["kind"] == kind]

    def kinds(self):
        return [c["kind"] for c in self.calls]


def make_repo(base):
    """A bare origin with `main`, a clone one commit behind it, and a commit that only the clone has.
    Returns the clone's path, the commit that is on origin/main only, and the local-only commit."""
    def git(cwd, *args):
        done = subprocess.run(["git", "-C", str(cwd), *args], check=True, capture_output=True, text=True,
                              env=dict(os.environ, **GIT_ENV))
        return done.stdout.strip()

    origin, seed, clone = base / "origin.git", base / "seed", base / "clone"
    env = dict(os.environ, **GIT_ENV)
    subprocess.run(["git", "init", "-q", "--bare", "-b", "main", str(origin)], check=True, env=env)
    subprocess.run(["git", "init", "-q", "-b", "main", str(seed)], check=True, env=env)
    git(seed, "commit", "-q", "--allow-empty", "-m", "first")
    git(seed, "remote", "add", "origin", str(origin))
    git(seed, "push", "-q", "origin", "main")
    subprocess.run(["git", "clone", "-q", str(origin), str(clone)], check=True, env=env)
    git(seed, "commit", "-q", "--allow-empty", "-m", "the squash")
    git(seed, "push", "-q", "origin", "main")
    squash = git(seed, "rev-parse", "HEAD")
    git(clone, "checkout", "-q", "-b", "local-only")
    git(clone, "commit", "-q", "--allow-empty", "-m", "never pushed")
    local_only = git(clone, "rev-parse", "HEAD")
    git(clone, "checkout", "-q", "main")
    return clone, squash, local_only


class Sandbox:
    """A fake `gh` first on PATH, a scenario for it, a temporary repo root holding a copy of the real
    outbound-guard, and (with git=True) a clone whose origin has the commit `gh` calls the merge commit."""

    def __init__(self, git=False, **scenario):
        self.git, self.over = git, scenario

    def __enter__(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.base = Path(self._tmp.name)
        self.bin = self.base / "bin"
        self.bin.mkdir()
        gh = self.bin / "gh"
        gh.write_text(FAKE_GH % {"python": sys.executable})
        gh.chmod(0o755)
        self.root = self.base / "root"
        (self.root / ".claude" / "hooks").mkdir(parents=True)
        shutil.copy(ROOT / ".claude" / "hooks" / "outbound-guard.py", self.root / ".claude" / "hooks")
        (self.base / "cfg").mkdir()
        (self.base / "home").mkdir()
        self.cwd = self.base / "work"
        self.cwd.mkdir()
        self.squash = self.local_only = "c" * 40
        if self.git:
            self.cwd, self.squash, self.local_only = make_repo(self.base)
        responses = {
            "view": [resp(VIEW_OPEN)],
            "checks": [resp(CHECKS_OPEN)],
            "rules": [resp(RULES_MAIN)],
            "merge": [resp()],
            "reread": [resp({"headRefOid": HEAD, "state": "OPEN"})],
            "verify": [resp({"state": "MERGED", "baseRefName": "main", "mergeCommit": {"oid": self.squash}})],
            "body": [resp({"body": "The PR body.\n"})],
            "runs": [resp(CHECK_RUNS)],
        }
        for kind, value in self.over.items():
            responses[kind] = value(self) if callable(value) else value
        self.scenario = self.base / "scenario.json"
        self.scenario.write_text(json.dumps({"responses": responses, "count": {}}))
        self.log = self.base / "gh.log"
        return self

    def __exit__(self, *exc):
        self._tmp.cleanup()

    def body(self, text=BODY, name="body.md"):
        path = self.base / name
        path.write_text(text)
        return str(path)

    def plant(self):
        (self.root / ".tv-host").write_text(PLANTED + "\n")

    def env(self):
        return dict(GIT_ENV, PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                    FAKE_GH_SCENARIO=str(self.scenario), FAKE_GH_LOG=str(self.log),
                    PLX_TV_CONFIG_DIR=str(self.base / "cfg"), HOME=str(self.base / "home"))

    def calls(self):
        if not self.log.exists():
            return []
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def run(self, *argv):
        clock, out, err = Clock(), io.StringIO(), io.StringIO()
        before = os.getcwd()
        with mock.patch.dict(os.environ, self.env()), contextlib.redirect_stdout(out), \
                contextlib.redirect_stderr(err):
            os.chdir(self.cwd)
            try:
                code = pr_land.main(list(argv), sleep=clock.sleep, now=clock.now, root=self.root)
            finally:
                os.chdir(before)
        return Result(code, out.getvalue(), err.getvalue(), clock.sleeps, self.calls())

    def merge(self, *extra, sha=HEAD):
        return self.run("merge", "1049", "--sha", sha, "--subject", SUBJECT, "--body-file", self.body(), *extra)


class Base(unittest.TestCase):
    def check(self, r, code):
        self.assertEqual(r.code, code, r.text())
        # Never `gh run list`, never --auto, and never a call the scenario did not expect.
        for call in r.calls:
            self.assertNotEqual(call["kind"], "unexpected", r.text())
            self.assertNotEqual(call["argv"][:2], ["run", "list"], r.text())
            self.assertNotIn("--auto", call["argv"], r.text())


class Fix387(Base):
    """#387 merged red because `gh pr merge` was chained after a watcher that had said FAILED."""

    def test_a_failed_check_never_reaches_gh_pr_merge(self):
        with Sandbox(checks=[resp(mark(HOSTSIM, "fail", "FAILURE"))]) as sb:
            r = sb.merge()
            self.check(r, 1)
            self.assertEqual(r.of("merge"), [], r.text())
            self.assertIn(HOSTSIM, r.out)
            self.assertIn("RED", r.out)

    def test_the_three_real_failures_of_1039_are_red(self):
        rows = CHECKS_OPEN
        for failed in FAILED_ROWS:
            rows = mark(failed["name"], failed["bucket"], failed["state"], rows)
        with Sandbox(checks=[resp(rows)]) as sb:
            r = sb.merge()
            self.check(r, 1)
            self.assertEqual(r.of("merge"), [])
            for failed in FAILED_ROWS:
                self.assertIn(failed["name"], r.out)

    def test_a_cancelled_check_is_red_too(self):
        with Sandbox(checks=[resp(mark(HOSTSIM, "cancel", "CANCELLED"))]) as sb:
            r = sb.merge()
            self.check(r, 1)
            self.assertEqual(r.of("merge"), [])


class GateTests(Base):
    def test_real_green_output_passes_and_prints_the_head(self):
        with Sandbox() as sb:
            r = sb.run("gate", "1049", "--sha", HEAD)
            self.check(r, 0)
            self.assertIn("GREEN", r.out)
            self.assertIn(HEAD[:12], r.out)
            self.assertEqual(r.sleeps, [])
            self.assertEqual(r.of("rules")[0]["argv"], ["api", "repos/{owner}/{repo}/rules/branches/main"])
            self.assertEqual(r.of("checks")[0]["argv"],
                             ["pr", "checks", "1049", "--json", "name,state,bucket,workflow"])

    def test_gate_without_a_sha_still_prints_the_head_to_pass_to_merge(self):
        with Sandbox() as sb:
            r = sb.run("gate", "1049")
            self.check(r, 0)
            self.assertIn(HEAD, r.out)

    def test_a_different_head_than_asserted_is_exit_5(self):
        with Sandbox() as sb:
            r = sb.run("gate", "1049", "--sha", OTHER_HEAD)
            self.check(r, 5)
            self.assertIn("HEAD MOVED", r.out)
            r = sb.merge(sha=OTHER_HEAD)
            self.check(r, 5)
            self.assertEqual(r.of("merge"), [])

    def test_a_draft_is_refused(self):
        with Sandbox(view=[resp(view(isDraft=True))]) as sb:
            r = sb.run("gate", "1049", "--sha", HEAD)
            self.check(r, 3)
            self.assertIn("draft", r.out)

    def test_a_merged_pr_is_refused_not_merged_again(self):
        with Sandbox(view=[resp(VIEW_MERGED)]) as sb:
            r = sb.run("gate", "1049", "--sha", VIEW_MERGED["headRefOid"])
            self.check(r, 3)
            self.assertIn("MERGED", r.out)

    def test_a_base_other_than_main_or_release_is_refused(self):
        with Sandbox(view=[resp(view(baseRefName="feature/x"))]) as sb:
            r = sb.run("gate", "1049", "--sha", HEAD)
            self.check(r, 3)
            self.assertIn("feature/x", r.out)

    def test_behind_and_dirty_say_to_merge_main_into_the_branch(self):
        for status in ("BEHIND", "DIRTY"):
            with Sandbox(view=[resp(view(mergeStateStatus=status))]) as sb:
                r = sb.run("gate", "1049", "--sha", HEAD)
                self.check(r, 3)
                self.assertIn("merge origin/main into the branch", r.out)
                self.assertIn(status, r.out)

    def test_a_merge_state_nobody_listed_is_refused_by_name(self):
        for status in ("HAS_HOOKS", "DRAFT", "SOMETHING_NEW"):
            with Sandbox(view=[resp(view(mergeStateStatus=status))]) as sb:
                r = sb.run("gate", "1049", "--sha", HEAD)
                self.check(r, 3)
                self.assertIn(status, r.out)

    # UNSTABLE is GitHub's "mergeable with a non-passing commit status": every REQUIRED check passes
    # while a non-required one (CodeQL's "Analyze (rust)") is still running or failed. On #1051 the
    # required checks passed ten minutes before CodeQL finished. The rows decide, as for BLOCKED.
    def test_unstable_with_a_non_required_check_still_running_is_exit_4_at_once(self):
        with Sandbox(view=[resp(UNSTABLE)], checks=[resp(mark(CODEQL_RUST, "pending", "IN_PROGRESS"))]) as sb:
            r = sb.run("gate", "1049", "--sha", HEAD)
            self.check(r, 4)
            self.assertIn(CODEQL_RUST, r.out)
            self.assertEqual(r.sleeps, [])

    def test_unstable_with_a_non_required_check_failed_is_red_and_names_it(self):
        with Sandbox(view=[resp(UNSTABLE)], checks=[resp(mark(CODEQL_RUST, "fail", "FAILURE"))]) as sb:
            r = sb.run("gate", "1049", "--sha", HEAD)
            self.check(r, 1)
            self.assertIn("RED", r.out)
            self.assertIn(CODEQL_RUST, r.out)

    def test_unstable_with_every_row_passing_is_lag_and_never_green(self):
        with Sandbox(view=[resp(UNSTABLE)]) as sb:
            r = sb.run("gate", "1049", "--sha", HEAD)
            self.check(r, 4)
            self.assertEqual(r.sleeps, [30, 30])
            self.assertIn("UNSTABLE", r.out)

    def test_several_things_wrong_at_once_follow_the_documented_order(self):
        skipping = mark(HOST_CHECKS, "skipping", "SKIPPED")
        # refusals, then head moved, then red, then a skipping required check, then not yet
        with Sandbox(view=[resp(view(isDraft=True))], checks=[resp(FAILED_ROWS)]) as sb:
            self.check(sb.run("gate", "1049", "--sha", OTHER_HEAD), 3)
        with Sandbox(checks=[resp(skipping)]) as sb:
            self.check(sb.run("gate", "1049", "--sha", OTHER_HEAD), 5)
        with Sandbox(checks=[resp(mark(HOSTSIM, "fail", "FAILURE", skipping))]) as sb:
            self.check(sb.run("gate", "1049", "--sha", HEAD), 1)
        with Sandbox(checks=[resp(mark(CODEQL_RUST, "pending", "IN_PROGRESS", skipping))]) as sb:
            self.check(sb.run("gate", "1049", "--sha", HEAD), 3)

    def test_a_required_context_that_lost_its_name_is_refused_not_a_traceback(self):
        nameless = [{"type": "required_status_checks",
                     "parameters": {"required_status_checks": [{"integration_id": 15368}]}}]
        with Sandbox(rules=[resp(nameless)]) as sb:
            r = sb.run("gate", "1049", "--sha", HEAD)
            self.check(r, 3)
            self.assertIn("context", r.out)

    def test_a_required_check_that_is_skipping_is_not_green(self):
        with Sandbox(checks=[resp(mark(HOST_CHECKS, "skipping", "SKIPPED"))]) as sb:
            r = sb.run("gate", "1049", "--sha", HEAD)
            self.check(r, 3)
            self.assertIn(HOST_CHECKS, r.out)

    def test_skipping_checks_that_are_not_required_are_fine(self):
        with Sandbox(checks=[resp(CHECKS_OPEN + SKIPPING_ROWS)]) as sb:
            self.check(sb.run("gate", "1049", "--sha", HEAD), 0)

    def test_an_unknown_bucket_is_refused_not_guessed_at(self):
        with Sandbox(checks=[resp(mark(HOSTSIM, "sideways", "WEIRD"))]) as sb:
            r = sb.run("gate", "1049", "--sha", HEAD)
            self.check(r, 3)
            self.assertIn("sideways", r.out)

    def test_json_that_lost_a_field_is_refused(self):
        broken = {k: v for k, v in VIEW_OPEN.items() if k != "headRefOid"}
        with Sandbox(view=[resp(broken)]) as sb:
            r = sb.run("gate", "1049")
            self.check(r, 3)
            self.assertIn("headRefOid", r.out + r.err)

    def test_pending_is_exit_4_at_once_without_polling(self):
        with Sandbox(checks=[resp(mark(HOSTSIM, "pending", "IN_PROGRESS"))]) as sb:
            r = sb.run("gate", "1049", "--sha", HEAD)
            self.check(r, 4)
            self.assertIn(HOSTSIM, r.out)
            self.assertEqual(r.sleeps, [])

    def test_no_checks_reported_is_pending_not_a_gh_failure(self):
        with Sandbox(checks=[resp("", NO_CHECKS_STDERR, rc=1)]) as sb:
            r = sb.run("gate", "1049", "--sha", HEAD)
            self.check(r, 4)
            self.assertIn("no checks", r.out)
            self.assertEqual(len(r.of("checks")), 1)

    def test_a_missing_required_check_is_exit_4(self):
        with Sandbox(checks=[resp(without(HOST_CHECKS))]) as sb:
            r = sb.run("gate", "1049", "--sha", HEAD)
            self.check(r, 4)
            self.assertIn(HOST_CHECKS, r.out)

    def test_every_ruleset_contributes_required_contexts(self):
        extra = {"type": "required_status_checks", "ruleset_id": 2,
                 "parameters": {"required_status_checks": [{"context": "extra gate"}]}}
        with Sandbox(rules=[resp(RULES_MAIN + [extra])]) as sb:
            r = sb.run("gate", "1049", "--sha", HEAD)
            self.check(r, 4)
            self.assertIn("extra gate", r.out)

    def test_unknown_merge_state_settles_for_about_a_minute_then_gives_up(self):
        with Sandbox(view=[resp(view(mergeStateStatus="UNKNOWN"))]) as sb:
            r = sb.run("gate", "1049", "--sha", HEAD)
            self.check(r, 4)
            self.assertEqual(r.sleeps, [30, 30])
            self.assertIn("UNKNOWN", r.out)

    def test_unknown_turns_clean(self):
        with Sandbox(view=[resp(view(mergeStateStatus="UNKNOWN")), resp(VIEW_OPEN)]) as sb:
            r = sb.run("gate", "1049", "--sha", HEAD)
            self.check(r, 0)
            self.assertEqual(r.sleeps, [30])

    def test_blocked_with_everything_green_is_repolled(self):
        with Sandbox(view=[resp(view(mergeStateStatus="BLOCKED")), resp(VIEW_OPEN)]) as sb:
            r = sb.run("gate", "1049", "--sha", HEAD)
            self.check(r, 0)
            self.assertEqual(r.sleeps, [30])


class WaitTests(Base):
    def test_pending_turns_green(self):
        pending = mark(HOSTSIM, "pending", "IN_PROGRESS")
        with Sandbox(checks=[resp(pending), resp(CHECKS_OPEN)]) as sb:
            r = sb.run("wait", "1049", "--sha", HEAD)
            self.check(r, 0)
            self.assertEqual(r.sleeps, [30])
            self.assertIn("GREEN", r.out)

    def test_exit_8_with_json_is_still_read_as_pending(self):
        pending = mark(HOSTSIM, "pending", "QUEUED")
        with Sandbox(checks=[resp(pending, rc=8), resp(CHECKS_OPEN)]) as sb:
            self.check(sb.run("wait", "1049", "--sha", HEAD), 0)

    def test_empty_after_a_push_turns_green(self):
        with Sandbox(checks=[resp("", NO_CHECKS_STDERR, rc=1), resp(CHECKS_OPEN)]) as sb:
            r = sb.run("wait", "1049", "--sha", HEAD)
            self.check(r, 0)
            self.assertEqual(r.sleeps, [30])
            self.assertEqual(len(r.of("checks")), 2)

    def test_a_required_check_that_never_shows_up_is_exit_4(self):
        with Sandbox(checks=[resp(without(HOST_CHECKS))]) as sb:
            r = sb.run("wait", "1049", "--sha", HEAD, "--timeout", "2m")
            self.check(r, 4)
            self.assertEqual(r.sleeps, [30, 45, 45])
            self.assertIn("TIMEOUT", r.out)
            self.assertIn(HOST_CHECKS, r.out)

    def test_polling_backs_off_from_30_to_60_seconds(self):
        with Sandbox(checks=[resp(mark(HOSTSIM, "pending", "PENDING"))]) as sb:
            r = sb.run("wait", "1049", "--sha", HEAD, "--timeout", "10m")
            self.check(r, 4)
            self.assertEqual(r.sleeps[:4], [30, 45, 60, 60])
            self.assertEqual(sum(r.sleeps), 600)

    def test_a_failure_while_others_are_pending_is_red_at_once(self):
        rows = mark(HOSTSIM, "pending", "PENDING")
        rows = mark("Windows launcher syntax", "fail", "FAILURE", rows)
        with Sandbox(checks=[resp(rows)]) as sb:
            r = sb.run("wait", "1049", "--sha", HEAD)
            self.check(r, 1)
            self.assertEqual(r.sleeps, [])

    def test_a_push_while_waiting_is_exit_5(self):
        pending = mark(HOSTSIM, "pending", "PENDING")
        with Sandbox(checks=[resp(pending)], view=[resp(VIEW_OPEN), resp(view(headRefOid=OTHER_HEAD))]) as sb:
            r = sb.run("wait", "1049", "--sha", HEAD)
            self.check(r, 5)
            self.assertEqual(r.sleeps, [30])

    def test_a_gh_error_is_retried(self):
        with Sandbox(checks=[resp("", "HTTP 502: Bad Gateway", rc=1), resp(CHECKS_OPEN)]) as sb:
            r = sb.run("wait", "1049", "--sha", HEAD)
            self.check(r, 0)
            self.assertEqual(r.sleeps, [30])

    def test_five_gh_errors_in_a_row_are_not_retried_forever(self):
        with Sandbox(checks=[resp("", "HTTP 502: Bad Gateway", rc=1)]) as sb:
            r = sb.run("wait", "1049", "--sha", HEAD)
            self.check(r, 3)
            self.assertEqual(len(r.of("checks")), 5)
            self.assertIn("HTTP 502", r.out + r.err)

    def test_the_release_base_waits_for_two_equal_check_lists(self):
        with Sandbox(view=[resp(view(baseRefName="release/v1.2"))], rules=[resp([])]) as sb:
            r = sb.run("wait", "1049", "--sha", HEAD)
            self.check(r, 0)
            self.assertEqual(r.sleeps, [30])
            self.assertEqual(r.of("rules")[0]["argv"], ["api", "repos/{owner}/{repo}/rules/branches/release/v1.2"])

    def test_a_check_list_that_keeps_growing_keeps_the_release_base_waiting(self):
        grows = [resp(CHECKS_OPEN[:3]), resp(CHECKS_OPEN[:4]), resp(CHECKS_OPEN[:4])]
        with Sandbox(view=[resp(view(baseRefName="release/v1.2"))], rules=[resp([])], checks=grows) as sb:
            r = sb.run("wait", "1049", "--sha", HEAD)
            self.check(r, 0)
            self.assertEqual(r.sleeps, [30, 45])

    def test_no_required_checks_and_no_checks_at_all_never_goes_green(self):
        with Sandbox(view=[resp(view(baseRefName="release/v1.2"))], rules=[resp([])],
                     checks=[resp("", NO_CHECKS_STDERR, rc=1)]) as sb:
            self.check(sb.run("wait", "1049", "--sha", HEAD, "--timeout", "5m"), 4)

    def test_timeout_formats(self):
        for text, seconds in (("90s", 90), ("2m", 120), ("1h", 3600), ("45", 45), (" 7m ", 420)):
            self.assertEqual(pr_land.duration(text), seconds, text)
        for text in ("", "soon", "-5m", "1.5h", "5 m"):
            with self.assertRaises(Exception, msg=text):
                pr_land.duration(text)
        with Sandbox(checks=[resp(mark(HOSTSIM, "pending", "PENDING"))]) as sb:
            r = sb.run("wait", "1049", "--sha", HEAD, "--timeout", "90s")
            self.check(r, 4)
            self.assertEqual(r.sleeps, [30, 45, 15])

    def test_the_help_says_to_run_long_waits_in_the_background(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.assertEqual(pr_land.main(["--help"]), 0)
        self.assertIn("run_in_background", out.getvalue())


class MergeTests(Base):
    def test_green_path_merges_with_the_exact_arguments_then_verifies(self):
        with Sandbox(git=True) as sb:
            r = sb.merge()
            self.check(r, 0)
            merge = r.of("merge")
            self.assertEqual(len(merge), 1)
            self.assertEqual(merge[0]["argv"], ["pr", "merge", "1049", "--squash", "--match-head-commit", HEAD,
                                                "--subject", SUBJECT, "--body-file", "-"])
            self.assertEqual(merge[0]["stdin"], BODY)
            self.assertEqual(r.kinds()[-3:], ["merge", "verify", "runs"])
            self.assertIn("MERGED", r.out)
            self.assertIn(sb.squash[:12], r.out)
            self.assertIn("Analyze (rust): success", r.out)

    def test_pending_checks_are_waited_for_before_the_merge(self):
        pending = mark(HOSTSIM, "pending", "IN_PROGRESS")
        with Sandbox(git=True, checks=[resp(pending), resp(CHECKS_OPEN)]) as sb:
            r = sb.merge()
            self.check(r, 0)
            self.assertEqual(r.sleeps, [30])
            self.assertEqual(len(r.of("merge")), 1)

    def test_a_timeout_never_merges(self):
        with Sandbox(checks=[resp(mark(HOSTSIM, "pending", "PENDING"))]) as sb:
            r = sb.merge("--timeout", "1m")
            self.check(r, 4)
            self.assertEqual(r.of("merge"), [])

    def test_unstable_waits_for_the_non_required_check_and_merges_only_on_clean(self):
        views = [resp(UNSTABLE), resp(UNSTABLE), resp(VIEW_OPEN)]
        checks = [resp(mark(CODEQL_RUST, "pending", "IN_PROGRESS")), resp(CHECKS_OPEN)]
        with Sandbox(git=True, view=views, checks=checks) as sb:
            r = sb.merge()
            self.check(r, 0)
            self.assertEqual(r.sleeps, [30, 45])        # a running row, then GitHub lag, then CLEAN
            kinds = r.kinds()
            self.assertEqual(kinds[:kinds.index("merge")].count("view"), 3)
            self.assertEqual(len(r.of("merge")), 1)

    def test_unstable_with_a_non_required_check_failed_never_reaches_gh_pr_merge(self):
        with Sandbox(view=[resp(UNSTABLE)], checks=[resp(mark(CODEQL_RUST, "fail", "FAILURE"))]) as sb:
            r = sb.merge()
            self.check(r, 1)
            self.assertEqual(r.of("merge"), [], r.text())
            self.assertIn(CODEQL_RUST, r.out)

    def test_unstable_that_never_turns_clean_times_out_without_merging(self):
        with Sandbox(view=[resp(UNSTABLE)]) as sb:
            r = sb.merge("--timeout", "1m")
            self.check(r, 4)
            self.assertEqual(r.of("merge"), [], r.text())
            self.assertIn("UNSTABLE", r.out)

    def test_the_head_race_succeeds_on_retry(self):
        with Sandbox(git=True, merge=[resp("", RACE, rc=1), resp()]) as sb:
            r = sb.merge()
            self.check(r, 0)
            self.assertEqual(len(r.of("merge")), 2)
            self.assertEqual(r.sleeps, [15])
            self.assertEqual(len(r.of("reread")), 1)

    def test_the_head_race_with_the_head_moved_is_exit_5(self):
        moved = resp({"headRefOid": OTHER_HEAD, "state": "OPEN"})
        with Sandbox(merge=[resp("", RACE, rc=1)], reread=[moved]) as sb:
            r = sb.merge()
            self.check(r, 5)
            self.assertEqual(len(r.of("merge")), 1)
            self.assertEqual(r.sleeps, [15])
            self.assertIn("HEAD MOVED", r.out)

    def test_a_head_race_that_never_clears_gives_up_after_three_retries(self):
        # every re-read shows the head still at the gated sha, so the PR is not "not at S" (exit 5)
        with Sandbox(merge=[resp("", RACE, rc=1)]) as sb:
            r = sb.merge()
            self.check(r, 3)
            self.assertEqual(len(r.of("merge")), 4)
            self.assertEqual(r.sleeps, [15, 15, 15])
            self.assertIn("REFUSED", r.out)
            self.assertIn("still says the head differs", r.out)
            self.assertNotIn("HEAD MOVED", r.out)

    def test_a_race_that_was_really_a_lost_answer_goes_on_to_verify(self):
        with Sandbox(git=True, merge=[resp("", RACE, rc=1)],
                     reread=[resp({"headRefOid": HEAD, "state": "MERGED"})]) as sb:
            r = sb.merge()
            self.check(r, 0)
            self.assertEqual(len(r.of("merge")), 1)
            self.assertIn("MERGED", r.out)

    def test_any_other_merge_failure_is_refused_without_a_retry(self):
        with Sandbox(merge=[resp("", "Pull request is not mergeable: base branch policy\n", rc=1)]) as sb:
            r = sb.merge()
            self.check(r, 3)
            self.assertEqual(len(r.of("merge")), 1)
            self.assertEqual(r.sleeps, [])
            self.assertIn("not mergeable", r.out + r.err)

    def test_a_merge_call_that_hangs_is_not_retried(self):
        with Sandbox(merge=[resp(delay=5)]) as sb:
            with mock.patch.object(pr_land, "GH_TIMEOUT", 1):
                r = sb.merge()
            self.check(r, 3)
            self.assertEqual(len(r.of("merge")), 1)
            self.assertIn("did not finish", r.out)

    def test_verify_fails_when_the_merge_commit_is_not_on_origin_main(self):
        stray = lambda sb: [resp({"state": "MERGED", "baseRefName": "main", "mergeCommit": {"oid": sb.local_only}})]
        with Sandbox(git=True, verify=stray) as sb:
            r = sb.merge()
            self.check(r, 1)
            self.assertIn("origin/main", r.out + r.err)
            self.assertIn(MERGE_WENT_THROUGH, r.out)
            self.assertEqual(len(r.of("merge")), 1)

    def test_verify_fails_when_the_pr_is_not_merged_after_all(self):
        opened = resp({"state": "OPEN", "baseRefName": "main", "mergeCommit": None})
        with Sandbox(git=True, verify=[opened]) as sb:
            r = sb.merge()
            self.check(r, 1)
            self.assertEqual(r.sleeps, [5, 5])
            self.assertIn("OPEN, not MERGED", r.out)
            self.assertIn(MERGE_WENT_THROUGH, r.out)

    def test_verify_with_a_merged_state_but_no_merge_commit_does_not_say_not_merged(self):
        for commit in (None, {"oid": None}, {"oid": 5}, "0a367de75"):
            merged = resp({"state": "MERGED", "baseRefName": "main", "mergeCommit": commit})
            with Sandbox(git=True, verify=[merged]) as sb:
                r = sb.merge()
                self.check(r, 1)
                self.assertEqual(r.sleeps, [5, 5])
                self.assertIn(MERGE_WENT_THROUGH + "the pull request is MERGED but", r.out)
                self.assertNotIn("not MERGED", r.out)

    def test_every_verify_failure_says_the_merge_went_through(self):
        # a checkout that cannot fetch (no repository at all), and a merged PR that cannot be re-read
        with Sandbox() as sb:
            r = sb.merge()
            self.check(r, 1)
            self.assertIn("VERIFY FAILED #1049: " + MERGE_WENT_THROUGH + "git fetch origin main failed", r.out)
        with Sandbox(verify=[resp("", "HTTP 502\n", rc=1)]) as sb:
            r = sb.merge()
            self.check(r, 1)
            self.assertIn("VERIFY FAILED #1049: " + MERGE_WENT_THROUGH + "could not read the merged", r.out)
            self.assertEqual(len(r.of("merge")), 1)

    def test_odd_check_run_json_after_a_merge_is_noted_not_a_traceback(self):
        for runs in ({"check_runs": [{"status": "completed"}, "junk", None,
                                     {"name": "kept", "status": "completed", "conclusion": "success"}]},
                     {"check_runs": None}, {"total_count": 0}):
            with Sandbox(git=True, runs=[resp(runs)]) as sb:
                r = sb.merge()
                self.check(r, 0)
                self.assertIn("MERGED #1049", r.out)
                if isinstance(runs.get("check_runs"), list):
                    self.assertIn("  kept: success", r.out)
                    self.assertIn("check-run", r.err)
                else:
                    self.assertIn("unavailable", r.out)

    def test_body_from_pr_sends_the_pr_body(self):
        with Sandbox(git=True) as sb:
            r = sb.run("merge", "1049", "--sha", HEAD, "--subject", SUBJECT, "--body-from-pr")
            self.check(r, 0)
            self.assertEqual(r.of("merge")[0]["stdin"], "The PR body.\n")
            self.assertEqual(r.of("body")[0]["argv"], ["pr", "view", "1049", "--json", "body"])

    def test_a_merge_needs_exactly_one_body(self):
        with Sandbox() as sb:
            for argv in (["merge", "1049", "--sha", HEAD, "--subject", SUBJECT],
                         ["merge", "1049", "--sha", HEAD, "--subject", SUBJECT, "--body-from-pr",
                          "--body-file", sb.body()]):
                r = sb.run(*argv)
                self.check(r, 2)
            self.assertEqual(sb.calls(), [])

    def test_usage_errors_are_exit_2_before_any_gh_call(self):
        with Sandbox() as sb:
            blank = sb.body("  \n", name="blank.md")
            for argv in (["merge", "1049", "--sha", HEAD[:7], "--subject", SUBJECT, "--body-file", blank],
                         ["merge", "1049", "--sha", HEAD, "--subject", "", "--body-file", sb.body()],
                         ["merge", "1049", "--sha", HEAD, "--subject", SUBJECT, "--body-file", blank],
                         ["merge", "1049", "--sha", HEAD, "--subject", SUBJECT, "--body-file", "/no/such/file"],
                         ["merge", "1049", "--sha", HEAD, "--subject", SUBJECT, "--body-file", sb.body(),
                          "--timeout", "soon"],
                         ["wait", "1049"], ["gate"], ["bogus"], []):
                self.check(sb.run(*argv), 2)
            self.assertEqual(sb.calls(), [])


class SecretScanTests(Base):
    def test_a_planted_value_in_the_body_is_refused_before_any_gh_call(self):
        with Sandbox() as sb:
            sb.plant()
            body = sb.body("Tested against the set at %s today.\n" % PLANTED)
            r = sb.run("merge", "1049", "--sha", HEAD, "--subject", SUBJECT, "--body-file", body)
            self.check(r, 3)
            self.assertEqual(sb.calls(), [], "gh was called before the scan")
            self.assertNotIn(PLANTED, r.out + r.err, "the refusal must not repeat the value")
            self.assertIn(".tv-host", r.out + r.err)

    def test_a_planted_value_in_the_subject_is_refused_before_any_gh_call(self):
        with Sandbox() as sb:
            sb.plant()
            r = sb.run("merge", "1049", "--sha", HEAD, "--subject", "Fix %s (#1049)" % PLANTED,
                       "--body-file", sb.body())
            self.check(r, 3)
            self.assertEqual(sb.calls(), [])
            self.assertNotIn(PLANTED, r.out + r.err)

    def test_a_token_shaped_value_is_refused_by_the_shape_rules(self):
        with Sandbox() as sb:
            body = sb.body("Debugging with X-Plex-Token=%s on the box.\n" % TOKEN_SHAPED)
            r = sb.run("merge", "1049", "--sha", HEAD, "--subject", SUBJECT, "--body-file", body)
            self.check(r, 3)
            self.assertEqual(sb.calls(), [])
            self.assertNotIn(TOKEN_SHAPED, r.out + r.err)

    def test_a_planted_value_in_the_pr_body_is_refused_before_the_merge_call(self):
        with Sandbox(body=[resp({"body": "Seen on %s.\n" % PLANTED})]) as sb:
            sb.plant()
            r = sb.run("merge", "1049", "--sha", HEAD, "--subject", SUBJECT, "--body-from-pr")
            self.check(r, 3)
            self.assertEqual(r.of("merge"), [])
            self.assertNotIn(PLANTED, r.out + r.err)

    def test_clean_text_goes_through_with_a_planted_value_present(self):
        with Sandbox(git=True) as sb:
            sb.plant()
            self.check(sb.merge(), 0)


class CommandLineTests(unittest.TestCase):
    def cli(self, sb, *argv):
        env = dict(os.environ, **sb.env())
        return subprocess.run([sys.executable, str(SCRIPT), *argv], capture_output=True, text=True,
                              env=env, cwd=sb.cwd)

    def test_the_script_is_a_cli_that_exits_with_the_verdict(self):
        with Sandbox() as sb:
            done = self.cli(sb, "gate", "1049", "--sha", HEAD)
            self.assertEqual((done.returncode, "GREEN" in done.stdout), (0, True), done.stdout + done.stderr)
            done = self.cli(sb, "gate", "1049", "--sha", OTHER_HEAD)
            self.assertEqual(done.returncode, 5, done.stdout + done.stderr)

    def test_no_arguments_is_a_usage_error(self):
        done = subprocess.run([sys.executable, str(SCRIPT)], capture_output=True, text=True)
        self.assertEqual(done.returncode, 2)

    def test_help_documents_the_exit_codes(self):
        done = subprocess.run([sys.executable, str(SCRIPT), "--help"], capture_output=True, text=True)
        self.assertEqual(done.returncode, 0)
        for word in ("run_in_background", "head moved", "timeout", "refused"):
            self.assertIn(word, done.stdout)


if __name__ == "__main__":
    unittest.main()
