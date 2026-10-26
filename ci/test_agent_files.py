#!/usr/bin/env python3
"""The custom agents' files stay complete: every canonical agent has its Claude symlink and Codex wrapper.

`.agents/agents/<name>.md` is the one canonical prompt; `.claude/agents/<name>.md` is a relative symlink
to it and `.codex/agents/<name>.toml` a thin wrapper around it (AGENTS.md, "Harness-specific files").
Nothing else reads these files before an agent is spawned, and CI's path filter ignores them, so a
renamed agent, a copied (not linked) file, a wrapper nobody added or a dropped `model:` line would
otherwise surface only as a spawn that quietly runs on the wrong model or not at all.

Per agent in `.agents/agents/*.md`: the frontmatter `name` equals the filename stem, an explicit
`model:` line, a non-empty `description`, `.claude/agents/<name>.md` is a RELATIVE symlink that resolves
to the canonical file, and `.codex/agents/<name>.toml` exists, sets `name = "<stem with underscores>"` and
names `.agents/agents/<stem>.md` (a wrapper copied from a sibling still names the sibling). In the other
direction, a `.claude/agents/*.md` or `.codex/agents/*.toml` whose stem has no canonical file is an orphan
(a renamed or deleted agent). The cases on synthetic trees in a private temp dir show each check firing;
the last class runs the checks over the real repository.
"""
from pathlib import Path
import os
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent

# Agents that legitimately lack an explicit `model:` line, each with the reason. Only that one check is
# exempted; every other check still applies. An exemption that is no longer needed fails below.
NO_EXPLICIT_MODEL = {
    "doc-claim-auditor":
        "its frontmatter predates the explicit-model rule and names no model, so a spawn inherits the "
        "caller's; pinning one changes what every audit costs, which is the owner's call, not this guard's",
}


def frontmatter(text):
    """The `key: value` pairs of a leading `---` block, parsed by hand. A value of `>`, `|` or nothing is
    followed by indented lines, folded into one string. Returns None without a closed block."""
    lines = text.splitlines()
    if not lines or lines[0].strip() != "---":
        return None
    fields, key = {}, None
    for line in lines[1:]:
        if line.strip() == "---":
            return {k: " ".join(v).strip() for k, v in fields.items()}
        if line[:1] in (" ", "\t") or not line.strip():
            if key is not None and line.strip():
                fields[key].append(line.strip())
            continue
        name, sep, value = line.partition(":")
        if not sep:
            return None
        key = name.strip()
        value = value.strip()
        fields[key] = [] if value in ("", ">", "|", ">-", "|-") else [value]
    return None


def problems(root, no_explicit_model=NO_EXPLICIT_MODEL):
    """Every way an agent under `root` is incomplete, as 'name: what is wrong' strings."""
    found = []
    canonical = sorted((root / ".agents" / "agents").glob("*.md"))
    for path in canonical:
        stem = path.stem
        fields = frontmatter(path.read_text())
        if fields is None:
            found.append(f"{stem}: no closed `---` frontmatter block at the top of {path.name}")
        else:
            if fields.get("name") != stem:
                found.append(f"{stem}: frontmatter name is {fields.get('name')!r}, not the filename stem")
            if not fields.get("model") and stem not in no_explicit_model:
                found.append(f"{stem}: no explicit `model:` line in the frontmatter")
            if not fields.get("description"):
                found.append(f"{stem}: empty or missing description")
        link = root / ".claude" / "agents" / f"{stem}.md"
        if not link.is_symlink():
            found.append(f"{stem}: .claude/agents/{stem}.md is not a symlink to the canonical file")
        else:
            if os.path.isabs(os.readlink(link)):
                found.append(f"{stem}: .claude/agents/{stem}.md links by absolute path, which breaks every other clone")
            if link.resolve() != path.resolve():
                found.append(f"{stem}: .claude/agents/{stem}.md resolves to {link.resolve()}, not {path}")
        wrapper = root / ".codex" / "agents" / f"{stem}.toml"
        if not wrapper.is_file():
            found.append(f"{stem}: .codex/agents/{stem}.toml is missing")
        else:
            text = wrapper.read_text()
            ident = stem.replace("-", "_")
            if f'name = "{ident}"' not in text:
                found.append(f"{stem}: .codex/agents/{stem}.toml does not set `name = \"{ident}\"` (copied from another wrapper?)")
            if f".agents/agents/{stem}.md" not in text:
                found.append(f"{stem}: .codex/agents/{stem}.toml never names .agents/agents/{stem}.md (copied from another wrapper?)")
    stems = {path.stem for path in canonical}
    for pattern in (".claude/agents/*.md", ".codex/agents/*.toml"):
        for extra in sorted(root.glob(pattern)):
            if extra.stem not in stems:
                found.append(f"{extra.stem}: {extra.relative_to(root)} has no canonical "
                             f".agents/agents/{extra.stem}.md (a renamed or deleted agent?)")
    return found


WRAPPER = ('name = "{ident}"\ndescription = "A wrapper."\ndeveloper_instructions = """\n'
           'Read `.agents/agents/{name}.md` completely before starting.\n"""\n')
GOOD = "---\nname: {name}\ndescription: >\n  Review a thing.\n  Use it twice.\ntools: Read\nmodel: opus\n---\n\n# body\n"


def agent(root, name, text=None, link=True, wrapper=True):
    """A canonical agent `name` under `root`, with its relative symlink and Codex wrapper unless refused."""
    for d in (".agents/agents", ".claude/agents", ".codex/agents"):
        (root / d).mkdir(parents=True, exist_ok=True)
    (root / ".agents/agents" / f"{name}.md").write_text(GOOD.format(name=name) if text is None else text)
    if link:
        os.symlink(f"../../.agents/agents/{name}.md", root / ".claude/agents" / f"{name}.md")
    if wrapper:
        (root / ".codex/agents" / f"{name}.toml").write_text(WRAPPER.format(name=name, ident=name.replace("-", "_")))


class Checks(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def only(self, fragment, **kw):
        found = problems(self.root, **kw)
        self.assertEqual(len(found), 1, found)
        self.assertIn(fragment, found[0])

    def test_a_complete_agent_has_no_problems(self):
        agent(self.root, "x-agent")
        self.assertEqual(problems(self.root), [])

    def test_a_name_that_is_not_the_filename_stem(self):
        agent(self.root, "x-agent", GOOD.format(name="y-agent"))
        self.only("not the filename stem")

    def test_no_explicit_model_line(self):
        agent(self.root, "x-agent", GOOD.format(name="x-agent").replace("model: opus\n", ""))
        self.only("no explicit `model:`")

    def test_an_empty_model_line_is_not_explicit(self):
        agent(self.root, "x-agent", GOOD.format(name="x-agent").replace("model: opus", "model:"))
        self.only("no explicit `model:`")

    def test_a_description_that_is_empty_or_a_bare_fold_marker(self):
        for body in ("description:\n", "description: >\n"):
            with self.subTest(body=body):
                with tempfile.TemporaryDirectory() as d:
                    root = Path(d)
                    agent(root, "x-agent", f"---\nname: x-agent\n{body}tools: Read\nmodel: opus\n---\n")
                    found = problems(root)
                    self.assertEqual(len(found), 1, found)
                    self.assertIn("empty or missing description", found[0])

    def test_a_missing_or_unclosed_frontmatter(self):
        for text in ("# no frontmatter\n", "---\nname: x-agent\nmodel: opus\n"):
            with self.subTest(text=text):
                with tempfile.TemporaryDirectory() as d:
                    root = Path(d)
                    agent(root, "x-agent", text)
                    found = problems(root)
                    self.assertEqual(len(found), 1, found)
                    self.assertIn("frontmatter block", found[0])

    def test_a_missing_symlink(self):
        agent(self.root, "x-agent", link=False)
        self.only("not a symlink")

    def test_a_copy_where_a_symlink_belongs(self):
        agent(self.root, "x-agent", link=False)
        (self.root / ".claude/agents/x-agent.md").write_text(GOOD.format(name="x-agent"))
        self.only("not a symlink")

    def test_a_symlink_to_another_file(self):
        agent(self.root, "x-agent", link=False)
        agent(self.root, "y-agent")
        os.symlink("../../.agents/agents/y-agent.md", self.root / ".claude/agents/x-agent.md")
        self.only("resolves to")

    def test_an_absolute_symlink(self):
        agent(self.root, "x-agent", link=False)
        os.symlink(self.root / ".agents/agents/x-agent.md", self.root / ".claude/agents/x-agent.md")
        self.only("absolute path")

    def test_a_missing_codex_wrapper(self):
        agent(self.root, "x-agent", wrapper=False)
        self.only(".codex/agents/x-agent.toml is missing")

    def test_a_renamed_or_deleted_canonical_file_leaves_orphans(self):
        agent(self.root, "x-agent")
        (self.root / ".agents/agents/x-agent.md").unlink()
        found = problems(self.root)
        self.assertEqual(len(found), 2, found)
        self.assertTrue([f for f in found if ".claude/agents/x-agent.md has no canonical" in f], found)
        self.assertTrue([f for f in found if ".codex/agents/x-agent.toml has no canonical" in f], found)

    def test_a_wrapper_copied_from_a_sibling_still_names_the_sibling(self):
        agent(self.root, "x-agent")
        agent(self.root, "y-agent")
        sibling = WRAPPER.format(name="x-agent", ident="x_agent")
        (self.root / ".codex/agents/y-agent.toml").write_text(sibling)
        found = problems(self.root)
        self.assertEqual(len(found), 2, found)
        self.assertTrue([f for f in found if 'name = "y_agent"' in f], found)
        self.assertTrue([f for f in found if ".agents/agents/y-agent.md" in f], found)

    def test_an_exemption_covers_only_the_model_check(self):
        agent(self.root, "x-agent", GOOD.format(name="y-agent").replace("model: opus\n", ""), wrapper=False)
        found = problems(self.root, no_explicit_model={"x-agent": "a reason"})
        self.assertEqual(len(found), 2, found)
        self.assertFalse([f for f in found if "model" in f])


class TheRepository(unittest.TestCase):
    def test_the_agents_are_found(self):
        names = {p.stem for p in (ROOT / ".agents/agents").glob("*.md")}
        self.assertTrue({"doc-claim-auditor", "fw-compat-reviewer"} <= names, names)

    def test_every_agent_is_complete(self):
        self.assertEqual(problems(ROOT), [])

    def test_an_exemption_is_still_needed(self):
        for name, reason in NO_EXPLICIT_MODEL.items():
            path = ROOT / ".agents/agents" / f"{name}.md"
            self.assertTrue(path.is_file(), f"{name} is gone: drop its exemption")
            fields = frontmatter(path.read_text())
            self.assertFalse(fields and fields.get("model"), f"{name} names a model now: drop its exemption")
            self.assertTrue(reason)

    def test_check_python_steps_runs_the_guard(self):
        steps = (ROOT / "ci" / "check-python-steps.txt").read_text().splitlines()
        self.assertTrue("python3 ci/test_agent_files.py" in steps,
                        "ci/check-python-steps.txt no longer runs this guard")


if __name__ == "__main__":
    unittest.main()
