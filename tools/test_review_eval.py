#!/usr/bin/env python3
"""tools/review-eval.py: the case list's checks, the replayed repository, the two tasks and the score.

Hermetic: a temporary repository stands in for this one (a bug commit, its fix, a later branch and a
tag), so the replay's promise can be shown on it: after `materialize` no ref but `base` and `head` is
left and neither `git log --all` nor a tag reaches the fix. The last class runs `problems` over the
real tools/review-eval/cases.json, where a commit a shallow checkout lacks is skipped, not failed.
"""
import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("review_eval", ROOT / "tools" / "review-eval.py")
review_eval = importlib.util.module_from_spec(spec)
spec.loader.exec_module(review_eval)

KEY = "The parser reads one byte past the end of a short header and returns garbage."
ENV = ["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false"]


def git(cwd, *args):
    done = subprocess.run(["git", "-C", str(cwd), *ENV, *args], capture_output=True, text=True)
    assert done.returncode == 0, done.stderr
    return done.stdout.strip()


def make_repo(base):
    repo = base / "repo"
    repo.mkdir()
    git(repo, "init", "-q", "-b", "main")
    shas = []
    for name, text in (("start (#1)", "a\n"), ("bug (#2)", "b\n"), ("fix\n\nFixes what landed in (#2)", "the fix\n")):  # noqa: E501
        (repo / "f.txt").write_text(text)
        git(repo, "add", "f.txt")
        git(repo, "commit", "-q", "-m", name)
        shas.append(git(repo, "rev-parse", "HEAD"))
    git(repo, "branch", "later")
    git(repo, "tag", "v1")
    return repo, shas


def cases(bug="1" * 40, fix="2" * 40):
    """Four cases, a bug and a control per split. Only `pr-2` (the tune bug) names a commit a test repo
    may hold; the other three name commits no repository has, which `problems` skips."""
    return [{"id": "pr-2", "kind": "bug", "split": "tune", "commit": bug, "fixed_by": fix, "key": KEY},
            {"id": "pr-7", "kind": "control", "split": "tune", "commit": "7" * 40},
            {"id": "pr-8", "kind": "bug", "split": "test", "commit": "8" * 40, "fixed_by": "a" * 40, "key": KEY},
            {"id": "pr-9", "kind": "control", "split": "test", "commit": "9" * 40}]


class Problems(unittest.TestCase):
    def test_a_well_formed_list_has_none(self):
        self.assertEqual(review_eval.problems(cases()), [])

    def broken(self, change):
        got = cases()
        change(got)
        return "\n".join(review_eval.problems(got))

    def test_each_malformed_case_is_named(self):
        self.assertIn("used twice", self.broken(lambda c: c[1].update(id="pr-2")))
        self.assertIn("kind is", self.broken(lambda c: c[0].update(kind="revert")))
        self.assertIn("exactly the fields", self.broken(lambda c: c[1].update(key=KEY)))
        self.assertIn("exactly the fields", self.broken(lambda c: c[0].pop("fixed_by")))
        self.assertIn("split is", self.broken(lambda c: c[0].update(split="dev")))
        self.assertIn("not a full commit id", self.broken(lambda c: c[0].update(commit="abc123")))
        self.assertIn("one sentence", self.broken(lambda c: c[0].update(key="too short.")))
        self.assertIn("one sentence", self.broken(lambda c: c[0].update(key=KEY + "\nAnd a second line.")))

    def test_a_split_without_both_kinds_is_a_problem(self):
        self.assertIn("the test split has no control case", self.broken(lambda c: c.pop()))

    def test_commits_the_clone_has_are_checked_and_missing_ones_are_skipped(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, (start, bug, fix) = make_repo(Path(tmp))
            self.assertEqual(review_eval.problems(cases(bug, fix), repo), [])
            self.assertEqual(review_eval.problems(cases(), repo), [])       # none of these exist here
            said = "\n".join(review_eval.problems(cases(fix, bug), repo))
            self.assertIn("fixed_by does not come after the commit", said)
            # the fix's BODY says (#2) too: a lookup by message finds it, and it is not that pull request
            self.assertIn("not that pull request's squash", said)
            self.assertIn("not a one-parent", "\n".join(review_eval.problems(cases(start, fix), repo)))


class Replay(unittest.TestCase):
    def test_the_replay_holds_the_bug_and_no_way_to_the_fix(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, (start, bug, fix) = make_repo(Path(tmp))
            into = review_eval.materialize(cases(bug, fix)[0], Path(tmp) / "replay", repo)
            self.assertEqual(git(into, "for-each-ref", "--format=%(refname)").split(),
                             ["refs/heads/base", "refs/heads/head"])
            self.assertEqual(git(into, "rev-parse", "base", "head", "HEAD").split(), [start, bug, bug])
            self.assertEqual((into / "f.txt").read_text(), "b\n")
            self.assertNotIn(fix, git(into, "log", "--all", "--format=%H"))
            self.assertNotIn(fix, git(into, "log", "--reflog", "--format=%H"))  # the clone's own reflog names it
            self.assertEqual(git(into, "tag"), "")
            self.assertEqual(git(into, "remote"), "")

    def test_an_existing_directory_is_never_reused(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, (_start, bug, fix) = make_repo(Path(tmp))
            with self.assertRaises(SystemExit):
                review_eval.materialize(cases(bug, fix)[0], Path(tmp), repo)


class Tasks(unittest.TestCase):
    def test_the_reviewers_task_never_carries_the_key(self):
        said = review_eval.task(Path("/replay"), Path("/rules"))
        self.assertIn("base...head", said)
        self.assertNotIn(KEY, said)

    def test_the_grading_task_carries_the_key_for_a_bug_and_none_for_a_control(self):
        bug, control = cases()[:2]
        self.assertIn(KEY, review_eval.grade_task(bug, "1. f.rs:1 MAJOR"))
        self.assertIn('"caught"', review_eval.grade_task(bug, ""))
        said = review_eval.grade_task(control, "No findings.")
        self.assertNotIn("caught", said)
        self.assertIn("No findings.", said)


class Score(unittest.TestCase):
    def verdicts(self):
        return {"pr-2": {"caught": True, "blockers": 0, "majors": 1, "minors": 1},
                "pr-8": {"caught": False, "blockers": 0, "majors": 0, "minors": 0},
                "pr-7": {"blockers": 0, "majors": 0, "minors": 3},
                "pr-9": {"blockers": 1, "majors": 0, "minors": 0}}

    def test_recall_and_the_flagged_controls(self):
        lines, complete = review_eval.score(cases(), self.verdicts())
        self.assertTrue(complete)
        said = "\n".join(lines)
        self.assertIn("recall (bugs caught): 1/2 = 50%; 1.0 findings per case", said)
        self.assertIn("controls flagged BLOCKER/MAJOR: 1/2 = 50%; 2.0 findings per case", said)

    def test_a_minor_on_a_control_is_not_a_flag(self):
        lines, _ = review_eval.score(cases(), self.verdicts(), "tune")
        self.assertIn("controls flagged BLOCKER/MAJOR: 0/1 = 0%", "\n".join(lines))
        self.assertNotIn("pr-8", "\n".join(lines))

    def test_a_missing_or_malformed_verdict_makes_the_run_incomplete(self):
        for spoil in (lambda v: v.pop("pr-9"), lambda v: v["pr-2"].pop("caught"),
                      lambda v: v["pr-2"].update(caught="yes"), lambda v: v["pr-7"].update(minors=-1)):
            verdicts = self.verdicts()
            spoil(verdicts)
            lines, complete = review_eval.score(cases(), verdicts)
            self.assertFalse(complete)
            self.assertIn("NO VERDICT", "\n".join(lines))


class RealCases(unittest.TestCase):
    def test_the_shipped_case_list_is_well_formed(self):
        self.assertEqual(review_eval.problems(review_eval.load(), ROOT), [])

    def test_no_case_is_both_a_bug_and_somebody_elses_fix(self):
        loaded = review_eval.load()
        commits = [c["commit"] for c in loaded]
        self.assertEqual(len(commits), len(set(commits)))
        self.assertFalse(set(commits) & {c["fixed_by"] for c in loaded if c["kind"] == "bug"})


if __name__ == "__main__":
    unittest.main()
