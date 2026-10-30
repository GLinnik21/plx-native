#!/usr/bin/env python3
"""ci/check-ledger-ratchet.py against fixture repositories (`git init` in a private temp dir, a base
commit and a head commit), run end to end as the gate is: exit status and the text it prints.

What it pins: a family's entry count that rises fails and is named as `name: base -> head`; one that falls,
a same-count rename, an entry that moves between two files of one family, and a `# count:` header bumped
over unchanged entries all pass; a new area file that raises an existing family's total fails, and so does
an entry added to a file that was empty; a counter the base does not have (a new top-level file, a new
family directory, a new pin key) is a new gate's own list: it passes and the ok line names it as not
compared; a raised pin in `tests/test_harness.py` fails and a lowered one passes; with no base the check
skips and says so. Also the base resolution (the override, `GITHUB_BASE_REF`, `origin/main`,
the MERGE-BASE rather than the tip of main), the exit-2 cases that must never read as a skip, and the real
tree: the pin tables of `tests/test_harness.py` still parse and the CI job that runs the gate fetches the
full history it needs. Nothing here reads the real repository's history or writes outside a temp dir."""
import importlib.util
import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
SCRIPT = HERE / "check-ledger-ratchet.py"
_spec = importlib.util.spec_from_file_location("check_ledger_ratchet", SCRIPT)
gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gate)

HARNESS = "tests/test_harness.py"


def clean_env(**extra):
    """No inherited GIT_* and, above all, no GITHUB_BASE_REF / PLX_LEDGER_BASE: CI sets the first on a pull
    request, and a fixture that means to have no base must not inherit the real one."""
    env = {k: v for k, v in os.environ.items()
           if not k.startswith("GIT_") and k not in ("GITHUB_BASE_REF", "PLX_LEDGER_BASE")}
    env["GIT_CONFIG_GLOBAL"] = os.devnull
    env["GIT_CONFIG_NOSYSTEM"] = "1"
    env.update(extra)
    return env


def ledger(*entries, header=None):
    """A ledger file: a `# count:` header (the number of entries unless `header` says otherwise), a comment, entries."""
    n = len(entries) if header is None else header
    return f"# count: {n}\n# what this list is\n" + "".join(f"{e}\treason\n" for e in entries)


def harness(ledger_totals, allowlist_counts, extra=""):
    """A tests/test_harness.py with the two pin tables inside a class, comments among the rows."""
    def rows(table):
        return "".join(f'        "{k}": {v},  # a comment\n' for k, v in table.items())
    return ("import unittest\n\n\nclass DepGates(unittest.TestCase):\n"
            f"    PINNED_ALLOWLIST_COUNTS = {{\n        # leading comment\n{rows(allowlist_counts)}    }}\n\n"
            f"    PINNED_LEDGER_TOTALS = {{\n{rows(ledger_totals)}    }}\n{extra}")


class Repo:
    """A throwaway repository: `commit(files)` writes `{path: text}` (None deletes) and commits."""

    def __init__(self):
        self._tmp = tempfile.TemporaryDirectory(prefix="ledger-ratchet-")
        self.path = Path(self._tmp.name).resolve()
        self.git("init", "-q")
        self.git("symbolic-ref", "HEAD", "refs/heads/main")

    def cleanup(self):
        self._tmp.cleanup()

    def git(self, *args):
        out = subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c",
                              "commit.gpgsign=false", "-C", str(self.path), *args],
                             capture_output=True, text=True, env=clean_env())
        if out.returncode:
            raise AssertionError("git %s failed: %s%s" % (" ".join(args), out.stdout, out.stderr))
        return out.stdout.strip()

    def commit(self, files, message="c"):
        for name, text in files.items():
            path = self.path / name
            if text is None:
                path.unlink()
                continue
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        self.git("add", "-A")
        self.git("commit", "-q", "--allow-empty", "-m", message)
        return self.git("rev-parse", "HEAD")

    def run(self, **env):
        out = subprocess.run([sys.executable, str(SCRIPT), "--repo", str(self.path)],
                             capture_output=True, text=True, env=clean_env(**env))
        return out.returncode, out.stdout + out.stderr


BASE = {
    "ci/allow/forks/media.txt": ledger("m.rs::a", "m.rs::sink"),
    "ci/allow/forks/plex.txt": ledger("p.rs::a"),
    "ci/allow/workers/plex.txt": ledger("w.rs::a::spawn"),
    "ci/allow/wall.txt": ledger("w.rs", "x.rs"),
    "ci/allow/threads.txt": "# count: 0\n# none\n",
    HARNESS: harness({"workers": 3, "forks": 3}, {"wall.txt": 2, "threads.txt": 0}),
}


class Ratchet(unittest.TestCase):
    def setUp(self):
        self.repo = Repo()
        self.addCleanup(self.repo.cleanup)
        self.base = self.repo.commit(BASE, "base")

    def head(self, files):
        """Commit `files` on top of the base and run the gate against the base (the override)."""
        self.repo.commit(files, "head")
        return self.repo.run(PLX_LEDGER_BASE=self.base)

    # -- the ledger counts -----------------------------------------------------------------------

    def test_an_unchanged_tree_passes_and_names_nothing_as_new(self):
        code, out = self.head({})
        self.assertEqual(code, 0, out)
        self.assertIn("ledger-ratchet: ok", out)
        self.assertNotIn("not compared", out)

    def test_a_family_whose_entry_count_rises_fails_and_is_named(self):
        code, out = self.head({"ci/allow/forks/plex.txt": ledger("p.rs::a", "p.rs::b")})
        self.assertEqual(code, 1, out)
        self.assertIn("ci/allow/forks/: 3 \u2192 4", out)
        self.assertNotIn("ci/allow/workers/", out)
        words = " ".join(out.split())  # the message wraps; the sentences are what is pinned
        self.assertIn("only shrink", words)
        self.assertIn("remove the entry by fixing the site so the rule passes", words)
        self.assertIn('(for a Sans-IO ledger, move the work behind an effect, docs/sans-io-guide.md, "The ledgers")', words)
        self.assertNotIn("instead of listing the site", words)
        self.assertIn("owner's call", words)
        self.assertIn("merging past this check", words)

    def test_every_raised_count_is_named(self):
        code, out = self.head({"ci/allow/workers/plex.txt": ledger("w.rs::a::spawn", "w.rs::b::spawn"),
                               "ci/allow/wall.txt": ledger("w.rs", "x.rs", "y.rs")})
        self.assertEqual(code, 1, out)
        self.assertIn("ci/allow/workers/: 1 \u2192 2", out)
        self.assertIn("ci/allow/wall.txt: 2 \u2192 3", out)

    def test_a_count_that_falls_passes(self):
        code, out = self.head({"ci/allow/forks/media.txt": ledger("m.rs::a")})
        self.assertEqual(code, 0, out)

    def test_a_same_count_rename_passes(self):
        code, out = self.head({"ci/allow/forks/media.txt": ledger("m.rs::a", "m.rs::raw_sink")})
        self.assertEqual(code, 0, out)

    def test_an_entry_that_moves_between_two_files_of_a_family_passes(self):
        code, out = self.head({"ci/allow/forks/media.txt": ledger("m.rs::a"),
                               "ci/allow/forks/plex.txt": ledger("p.rs::a", "m.rs::sink")})
        self.assertEqual(code, 0, out)

    def test_an_entry_that_moves_between_families_fails(self):
        code, out = self.head({"ci/allow/forks/media.txt": ledger("m.rs::a"),
                               "ci/allow/workers/plex.txt": ledger("w.rs::a::spawn", "m.rs::sink")})
        self.assertEqual(code, 1, out)
        self.assertIn("ci/allow/workers/: 1 \u2192 2", out)

    def test_a_new_top_level_file_with_entries_is_a_new_gates_list_and_passes_named(self):
        """No existing gate reads a file name that did not exist, so the list cannot excuse an old rule's site."""
        code, out = self.head({"ci/allow/newrule.txt": ledger("a.rs", "b.rs")})
        self.assertEqual(code, 0, out)
        self.assertIn("ledger-ratchet: ok", out)
        self.assertIn("; new, not compared: ci/allow/newrule.txt (2)", out)

    def test_a_new_family_directory_passes_named_and_an_empty_new_file_too(self):
        code, out = self.head({"ci/allow/newfam/x.txt": ledger("a.rs")})
        self.assertEqual(code, 0, out)
        self.assertIn("new, not compared: ci/allow/newfam/ (1)", out)
        self.repo.commit({"ci/allow/newfam/x.txt": None, "ci/allow/empty.txt": ledger()}, "empty")
        code, out = self.repo.run(PLX_LEDGER_BASE=self.base)
        self.assertEqual(code, 0, out)

    def test_every_new_counter_is_named_and_the_ones_the_base_has_are_not(self):
        code, out = self.head({"ci/allow/newrule.txt": ledger("a.rs"), "ci/allow/newfam/x.txt": ledger("a.rs", "b.rs")})
        self.assertEqual(code, 0, out)
        self.assertIn("new, not compared: ci/allow/newfam/ (2), ci/allow/newrule.txt (1)", out)
        self.assertNotIn("wall.txt (", out)

    def test_a_new_area_file_in_an_existing_family_still_counts_toward_the_family(self):
        """The engine globs the family, so this IS a new site of an old rule: the family total is what rose."""
        code, out = self.head({"ci/allow/forks/newarea.txt": ledger("n.rs::a")})
        self.assertEqual(code, 1, out)
        self.assertIn("ci/allow/forks/: 3 \u2192 4", out)
        self.assertNotIn("new, not compared", out)

    def test_an_entry_added_to_a_list_that_was_empty_fails(self):
        """`threads.txt` is `# count: 0` at the base: it exists there, so 0 -> 1 is growth, not a new list."""
        code, out = self.head({"ci/allow/threads.txt": ledger("t.rs")})
        self.assertEqual(code, 1, out)
        self.assertIn("ci/allow/threads.txt: 0 \u2192 1", out)

    def test_a_raised_count_fails_even_when_a_new_list_comes_with_it(self):
        code, out = self.head({"ci/allow/wall.txt": ledger("w.rs", "x.rs", "y.rs"), "ci/allow/newrule.txt": ledger("a.rs")})
        self.assertEqual(code, 1, out)
        self.assertIn("ci/allow/wall.txt: 2 \u2192 3", out)
        self.assertNotIn("newrule", out)

    def test_a_deleted_file_is_zero_and_a_header_bump_over_unchanged_entries_passes(self):
        code, out = self.head({"ci/allow/forks/plex.txt": None,
                               "ci/allow/wall.txt": ledger("w.rs", "x.rs", header=9)})
        self.assertEqual(code, 0, out)

    def test_a_new_entry_with_the_header_and_the_pin_raised_together_fails(self):
        """The hole this closes: the header equals the entries and the pin equals the header."""
        code, out = self.head({
            "ci/allow/wall.txt": ledger("w.rs", "x.rs", "y.rs"),
            HARNESS: harness({"workers": 3, "forks": 3}, {"wall.txt": 3, "threads.txt": 0}),
        })
        self.assertEqual(code, 1, out)
        self.assertIn("ci/allow/wall.txt: 2 \u2192 3", out)
        self.assertIn('PINNED_ALLOWLIST_COUNTS["wall.txt"]: 2 \u2192 3', out)

    def test_blank_and_comment_lines_are_not_entries_and_other_extensions_are_ignored(self):
        text = "# count: 2\n\n   \n  # an indented comment\nreal.rs\tone\n  \nreal2.rs\n# tail\n"
        self.assertEqual(gate.entries(text), 2)
        code, out = self.head({"ci/allow/wall.txt": text, "ci/allow/README.md": "a\nb\nc\n",
                               "ci/allow/forks/notes.json": "{\n}\n"})
        self.assertEqual(code, 0, out)

    # -- the pin tables --------------------------------------------------------------------------

    def test_a_pin_that_rises_fails_and_one_that_falls_passes(self):
        code, out = self.head({HARNESS: harness({"workers": 4, "forks": 3}, {"wall.txt": 2, "threads.txt": 0})})
        self.assertEqual(code, 1, out)
        self.assertIn('PINNED_LEDGER_TOTALS["workers"]: 3 \u2192 4', out)
        self.assertNotIn("forks", out.split("\n\n")[0])
        self.repo.git("reset", "-q", "--hard", self.base)
        code, out = self.head({HARNESS: harness({"workers": 1, "forks": 3}, {"wall.txt": 0, "threads.txt": 0})})
        self.assertEqual(code, 0, out)

    def test_a_new_pin_key_is_a_new_gates_pin_and_passes_named(self):
        code, out = self.head({HARNESS: harness({"workers": 3, "forks": 3, "extra": 1},
                                                {"wall.txt": 2, "threads.txt": 0})})
        self.assertEqual(code, 0, out)
        self.assertIn('new, not compared: PINNED_LEDGER_TOTALS["extra"] (1)', out)
        self.repo.git("reset", "-q", "--hard", self.base)
        code, out = self.head({HARNESS: harness({"workers": 3, "forks": 3, "extra": 0},
                                                {"wall.txt": 2, "threads.txt": 0, "newrule.txt": 0})})
        self.assertEqual(code, 0, out)
        self.assertIn('PINNED_ALLOWLIST_COUNTS["newrule.txt"] (0)', out)

    def test_a_pin_the_base_has_is_compared_whatever_new_keys_come_with_it(self):
        code, out = self.head({HARNESS: harness({"workers": 3, "forks": 3, "extra": 1},
                                                {"wall.txt": 3, "threads.txt": 0})})
        self.assertEqual(code, 1, out)
        self.assertIn('PINNED_ALLOWLIST_COUNTS["wall.txt"]: 2 \u2192 3', out)
        self.assertNotIn("extra", out)

    def test_a_pin_table_that_disappears_cannot_be_compared(self):
        code, out = self.head({HARNESS: "import unittest\n"})
        self.assertEqual(code, 2, out)
        self.assertIn("PINNED_ALLOWLIST_COUNTS", out)
        self.assertIn("no longer defines", out)
        self.repo.commit({HARNESS: None}, "drop the file")
        self.assertEqual(self.repo.run(PLX_LEDGER_BASE=self.base)[0], 2)

    def test_a_pin_table_that_is_not_a_literal_cannot_be_compared(self):
        source = BASE[HARNESS].replace('"wall.txt": 2,  # a comment', '"wall.txt": len("ab"),')
        code, out = self.head({HARNESS: source})
        self.assertEqual(code, 2, out)
        self.assertIn("not a literal", out)

    def test_pin_tables_are_read_wherever_they_are_assigned(self):
        tables = gate.pin_tables("PINNED_LEDGER_TOTALS = {'a': 1}\n"
                                 "class C:\n    PINNED_ALLOWLIST_COUNTS: dict = {'b.txt': 2}\n    OTHER = {'c': 3}\n", "x")
        self.assertEqual(tables, {"PINNED_LEDGER_TOTALS": {"a": 1}, "PINNED_ALLOWLIST_COUNTS": {"b.txt": 2}})

    def test_a_base_without_the_harness_has_no_pins_to_compare(self):
        """Every pin key is new then, so none is compared; the ledger counts still are."""
        repo = Repo()
        self.addCleanup(repo.cleanup)
        base = repo.commit({"ci/allow/wall.txt": ledger("a")}, "base")
        repo.commit({HARNESS: harness({"workers": 1}, {})}, "head")
        code, out = repo.run(PLX_LEDGER_BASE=base)
        self.assertEqual(code, 0, out)
        self.assertIn('new, not compared: PINNED_LEDGER_TOTALS["workers"] (1)', out)
        repo.commit({"ci/allow/wall.txt": ledger("a", "b")}, "raise")
        self.assertEqual(repo.run(PLX_LEDGER_BASE=base)[0], 1)


class Base(unittest.TestCase):
    """Which commit the head is compared with."""

    def setUp(self):
        self.repo = Repo()
        self.addCleanup(self.repo.cleanup)
        self.base = self.repo.commit(BASE, "base")
        self.raise_wall = {"ci/allow/wall.txt": ledger("w.rs", "x.rs", "y.rs")}

    def test_no_base_at_all_skips_with_one_line_saying_so(self):
        self.repo.commit(self.raise_wall, "head")
        code, out = self.repo.run()
        self.assertEqual(code, 0, out)
        self.assertEqual(len(out.strip().splitlines()), 1, out)
        self.assertIn("skipped", out)
        self.assertIn("no origin/main", out)

    def test_origin_main_is_the_base_when_nothing_else_is_set(self):
        self.repo.git("update-ref", "refs/remotes/origin/main", self.base)
        self.repo.commit(self.raise_wall, "head")
        code, out = self.repo.run()
        self.assertEqual(code, 1, out)
        self.assertIn("ci/allow/wall.txt: 2 \u2192 3", out)
        self.assertIn("merge-base of HEAD and origin/main", out)

    def test_the_base_is_the_merge_base_not_the_tip_of_main(self):
        """Main lowered a count after this branch left it; the branch, unchanged, must still pass."""
        self.repo.git("checkout", "-q", "-b", "lane")
        self.repo.commit({"ci/allow/threads.txt": "# count: 0\n# none\n# lane\n"}, "lane")
        self.repo.git("checkout", "-q", "main")
        tip = self.repo.commit({"ci/allow/wall.txt": ledger("w.rs")}, "main lowers wall")
        self.repo.git("update-ref", "refs/remotes/origin/main", tip)
        self.repo.git("checkout", "-q", "lane")
        code, out = self.repo.run()
        self.assertEqual(code, 0, out)
        self.assertIn(self.base[:9], out)

    def test_a_push_to_main_is_its_own_base(self):
        tip = self.repo.commit(self.raise_wall, "pushed")
        self.repo.git("update-ref", "refs/remotes/origin/main", tip)
        code, out = self.repo.run()
        self.assertEqual(code, 0, out)

    def test_github_base_ref_names_the_branch_to_compare_with(self):
        self.repo.git("update-ref", "refs/remotes/origin/main", self.base)
        self.repo.commit(self.raise_wall, "head")
        tip = self.repo.git("rev-parse", "HEAD")
        self.repo.git("update-ref", "refs/remotes/origin/release/v1", tip)
        code, out = self.repo.run(GITHUB_BASE_REF="release/v1")
        self.assertEqual(code, 0, out)
        self.assertIn("origin/release/v1", out)
        code, out = self.repo.run(GITHUB_BASE_REF="main")
        self.assertEqual(code, 1, out)

    def test_github_base_ref_without_that_branch_is_an_error_not_a_skip(self):
        self.repo.git("update-ref", "refs/remotes/origin/main", self.base)
        self.repo.commit(self.raise_wall, "head")
        code, out = self.repo.run(GITHUB_BASE_REF="gone")
        self.assertEqual(code, 2, out)
        self.assertIn("fetch-depth: 0", out)
        self.assertNotIn("skipped", out)

    def test_an_empty_github_base_ref_is_unset(self):
        self.repo.git("update-ref", "refs/remotes/origin/main", self.base)
        self.repo.commit(self.raise_wall, "head")
        self.assertEqual(self.repo.run(GITHUB_BASE_REF="")[0], 1)

    def test_the_override_wins_and_must_name_a_commit(self):
        self.repo.git("update-ref", "refs/remotes/origin/main", self.base)
        later = self.repo.commit(self.raise_wall, "later")
        self.repo.commit({}, "head")
        self.assertEqual(self.repo.run(PLX_LEDGER_BASE=later)[0], 0)
        self.assertEqual(self.repo.run()[0], 1)
        code, out = self.repo.run(PLX_LEDGER_BASE="no-such-rev")
        self.assertEqual(code, 2, out)
        self.assertIn("PLX_LEDGER_BASE=no-such-rev", out)


class RealTree(unittest.TestCase):
    def test_the_real_pin_tables_parse_and_name_the_four_families(self):
        tables = gate.pin_tables((ROOT / HARNESS).read_text(encoding="utf-8"), HARNESS)
        self.assertEqual(sorted(tables), sorted(gate.PIN_TABLES))
        self.assertTrue({"workers", "globals", "forks", "fxlanes"} <= set(tables["PINNED_LEDGER_TOTALS"]))
        self.assertTrue(tables["PINNED_ALLOWLIST_COUNTS"])

    def test_the_real_ledgers_are_counted_by_family_and_by_file(self):
        counted = gate.counts(gate.head_texts(ROOT))
        for family in ("workers", "globals", "forks", "fxlanes"):
            self.assertIn(f"ci/allow/{family}/", counted)
        for name in ("blocking", "wall", "layers", "threads", "statics"):
            self.assertIn(f"ci/allow/{name}.txt", counted)

    def test_entries_agree_with_the_headers_check_deps_enforces(self):
        """`entries` is `ci/check-deps.sh`'s own rule, so the two agree on every file in the tree."""
        for path, text in gate.head_texts(ROOT).items():
            declared = re.search(r"^# count: *(\d+)", text, re.M)
            self.assertIsNotNone(declared, path)
            self.assertEqual(gate.entries(text), int(declared.group(1)), path)

    def test_the_ci_job_that_runs_the_gate_fetches_the_full_history(self):
        ci = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        parts = re.split(r"^  ([a-z][a-z0-9-]*):\n", ci[ci.index("\njobs:\n"):], flags=re.M)
        jobs = dict(zip(parts[1::2], parts[2::2]))
        runners = [name for name, block in jobs.items() if re.search(r"make check-python-rest(?![\w-])", block)]
        self.assertTrue(runners, "no ci.yml job runs `make check-python-rest`")
        for name in runners:
            self.assertRegex(jobs[name], r"fetch-depth:\s*0\b",
                             f"{name} runs the ledger ratchet, which needs origin/<base> and the history behind it")


if __name__ == "__main__":
    unittest.main()
