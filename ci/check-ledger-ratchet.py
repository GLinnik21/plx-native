#!/usr/bin/env python3
"""The ledger ratchet: no allowlist, ledger or pin may count MORE entries than the base branch has.

The files under `ci/allow/` (the Sans-IO ledgers `workers/`, `globals/`, `forks/`, `fxlanes/`, and the
flat lists `blocking.txt`, `wall.txt`, `layers.txt`, `threads.txt`, `statics.txt`, ...) are meant to
only shrink. Everything that checked that read numbers from the SAME tree: `ci/check-deps.sh` compares
a file's `# count:` header with its lines, and `tests/test_harness.py` compares the headers with
`PINNED_ALLOWLIST_COUNTS` / `PINNED_LEDGER_TOTALS` (`assertLessEqual`). A diff that adds an entry,
raises the header and raises the pin satisfied all three. This compares with the BASE instead.

    ci/check-ledger-ratchet.py            # the gate (check-python-steps.txt runs it)
    ci/check-ledger-ratchet.py --repo DIR # another checkout (ci/test_ledger_ratchet.py's fixtures)

WHAT IS COUNTED. The entry lines of each `.txt` file: a line that is not blank and does not start with
`#` (leading whitespace ignored), which is `ci/check-deps.sh`'s own `grep -cvE '^[[:space:]]*(#|$)'`. The
`# count:` header is never read, because the hole is a header and a pin moved together. Every file
under `ci/allow/` is one entry per line (a `<key>` or `<key><TAB><reason>`, or a bare `# count: 0` for a
list that is empty), so that rule is the whole format.

  * a directory under `ci/allow/` is a FAMILY (`workers/`, `globals/`, `forks/`, `fxlanes/`) and is ONE
    total, so an entry may move between the area files of a family (a re-filed line is not growth);
  * every top-level `ci/allow/*.txt` counts on its own;
  * a file or family that is new in the working tree counts up from 0, and one that is gone is 0;
  * the two pin tables in `tests/test_harness.py` (`PINNED_LEDGER_TOTALS`, `PINNED_ALLOWLIST_COUNTS`)
    are read with `ast`, and each KEY is compared; a key that is new counts from 0.

THE BASE. `PLX_LEDGER_BASE=<rev>` if set (the tests' override); else, when `GITHUB_BASE_REF` is set
(Actions sets it on `pull_request`), the merge-base of HEAD and `origin/$GITHUB_BASE_REF`, which must
exist (a missing ref is an error there, never a skip: CI's `host-python` job checks out with
`fetch-depth: 0`); else the merge-base of HEAD and `origin/main` when that exists; else the check
is skipped, with one line saying so. On a push to `main` HEAD is the base, so every count is equal. The
base's files are read with `git ls-tree` / `git show`; the working tree is read for the head, so a
raise fails before it is committed. Nothing is built.

Exit status: 0 = no count rose (or no base to compare with), 1 = a count rose, 2 = the comparison
could not be made (an unresolvable base, an unparseable pin table).

An exception to this rule is the owner's call, made by merging past the check; there is no label or
switch that waives it.
"""
from __future__ import annotations

import argparse
import ast
import os
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
ALLOW = "ci/allow"
HARNESS = "tests/test_harness.py"
PIN_TABLES = ("PINNED_LEDGER_TOTALS", "PINNED_ALLOWLIST_COUNTS")
GUIDE = 'docs/sans-io-guide.md, "The ledgers"'


class Unavailable(Exception):
    """The comparison cannot be made (exit 2): not a verdict about the counts."""


def git(repo, *args, check=True):
    out = subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True,
                         encoding="utf-8", errors="replace")
    if check and out.returncode:
        raise Unavailable("git %s failed: %s" % (" ".join(args), (out.stderr or out.stdout).strip()))
    return out


def entries(text: str) -> int:
    """The number of entry lines: not blank and not a comment (`ci/check-deps.sh`'s own rule)."""
    return sum(1 for line in text.splitlines() if line.strip() and not line.lstrip().startswith("#"))


def counter_name(path: str) -> str | None:
    """`ci/allow/forks/media.txt` -> `ci/allow/forks/` (a family), `ci/allow/wall.txt` -> itself, other -> None."""
    prefix = ALLOW + "/"
    if not path.startswith(prefix) or not path.endswith(".txt"):
        return None
    rest = path[len(prefix):]
    return prefix + rest.split("/", 1)[0] + "/" if "/" in rest else path


def counts(texts: dict[str, str]) -> dict[str, int]:
    """Entry totals by counter (a family or a top-level file) over `{path: file text}`."""
    totals: dict[str, int] = {}
    for path, text in texts.items():
        name = counter_name(path)
        if name is not None:
            totals[name] = totals.get(name, 0) + entries(text)
    return totals


def head_texts(repo: Path) -> dict[str, str]:
    root = repo / ALLOW
    return {p.relative_to(repo).as_posix(): p.read_text(encoding="utf-8", errors="replace")
            for p in sorted(root.rglob("*.txt")) if p.is_file()} if root.is_dir() else {}


def base_texts(repo: Path, base: str) -> dict[str, str]:
    names = git(repo, "ls-tree", "-r", "--name-only", base, "--", ALLOW).stdout.splitlines()
    return {n: git(repo, "show", f"{base}:{n}").stdout for n in names if counter_name(n)}


def pin_tables(source: str, where: str) -> dict[str, dict[str, int]]:
    """The literal pin dicts assigned anywhere in `source` (a class body here), by table name."""
    try:
        tree = ast.parse(source)
    except SyntaxError as exc:
        raise Unavailable(f"{where} does not parse: {exc}") from exc
    found: dict[str, dict[str, int]] = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.Assign):
            names = [t.id for t in node.targets if isinstance(t, ast.Name)]
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name) and node.value is not None:
            names = [node.target.id]
        else:
            continue
        for name in (n for n in names if n in PIN_TABLES):
            if name in found:
                raise Unavailable(f"{where} assigns {name} twice: the ratchet cannot tell which one the tests read")
            try:
                table = ast.literal_eval(node.value)
            except ValueError as exc:
                raise Unavailable(f"{where}: {name} is not a literal dict ({exc})") from exc
            if not isinstance(table, dict) or not all(
                    isinstance(k, str) and isinstance(v, int) and not isinstance(v, bool) for k, v in table.items()):
                raise Unavailable(f"{where}: {name} is not a dict of str -> int")
            found[name] = table
    return found


def flat(tables: dict[str, dict[str, int]]) -> dict[str, int]:
    """`{'PINNED_LEDGER_TOTALS["workers"]': 7, ...}` over both tables."""
    return {f'{table}["{key}"]': value for table, rows in tables.items() for key, value in rows.items()}


def raised(base: dict[str, int], head: dict[str, int]) -> list[tuple[str, int, int]]:
    """Every name whose head count is above its base count (a name missing from the base is 0)."""
    return [(name, base.get(name, 0), value) for name, value in sorted(head.items()) if value > base.get(name, 0)]


def resolve_base(repo: Path, env) -> tuple[str | None, str]:
    """(base commit or None, how it was found / why there is none)."""
    override = env.get("PLX_LEDGER_BASE")
    if override:
        sha = git(repo, "rev-parse", "--verify", "--quiet", f"{override}^{{commit}}", check=False)
        if sha.returncode or not sha.stdout.strip():
            raise Unavailable(f"PLX_LEDGER_BASE={override} is not a commit of this repository")
        return sha.stdout.strip(), f"PLX_LEDGER_BASE={override}"
    pull_base = env.get("GITHUB_BASE_REF")
    ref = f"origin/{pull_base}" if pull_base else "origin/main"
    present = git(repo, "rev-parse", "--verify", "--quiet", f"refs/remotes/{ref}^{{commit}}", check=False)
    if present.returncode:
        if pull_base:
            raise Unavailable(
                f"GITHUB_BASE_REF={pull_base} but {ref} is not in this clone: the job must check out "
                "with `fetch-depth: 0`, or this check would skip on exactly the runs it exists for")
        return None, "no PLX_LEDGER_BASE, no GITHUB_BASE_REF and no origin/main to compare with"
    mb = git(repo, "merge-base", "HEAD", ref, check=False)
    if mb.returncode or not mb.stdout.strip():
        if pull_base:
            raise Unavailable(f"HEAD and {ref} share no history in this clone (a shallow checkout?)")
        return None, f"HEAD and {ref} share no history in this clone (shallow?)"
    return mb.stdout.strip(), f"merge-base of HEAD and {ref}"


def message(rows: list[tuple[str, int, int]], base: str, how: str) -> str:
    lines = [f"ledger-ratchet: FAILED: {len(rows)} count(s) are above the base ({base[:9]}, {how}):"]
    lines += [f"  {name}: {was} → {now}" for name, was, now in rows]
    lines += [
        "",
        f"The ledgers and allowlists under {ALLOW}/ only shrink ({GUIDE}). This compares with the base",
        "branch, not with this tree, so a raised `# count:` header or a raised pin in tests/test_harness.py",
        "does not make a new line acceptable: move the work behind an effect instead of listing the site.",
        "An exception is the owner's call, made by merging past this check. An agent does not raise a",
        "count: it stops and explains in the PR why the site seems to need an entry.",
    ]
    return "\n".join(lines)


def check(repo: Path, env=None) -> tuple[int, str]:
    """(exit status, the line(s) to print)."""
    env = os.environ if env is None else env
    base, how = resolve_base(repo, env)
    if base is None:
        return 0, f"ledger-ratchet: skipped: {how}"
    base_files, head_files = base_texts(repo, base), head_texts(repo)
    old = git(repo, "show", f"{base}:{HARNESS}", check=False)
    base_tables = pin_tables(old.stdout, f"{base[:9]}:{HARNESS}") if old.returncode == 0 else {}
    harness = repo / HARNESS
    head_tables = (pin_tables(harness.read_text(encoding="utf-8", errors="replace"), HARNESS)
                   if harness.is_file() else {})
    lost = [t for t in base_tables if t not in head_tables]
    if lost:
        raise Unavailable(f"{HARNESS} no longer defines {', '.join(lost)}: the ratchet reads those pin tables")
    head_counts = {**counts(head_files), **flat(head_tables)}
    rows = raised({**counts(base_files), **flat(base_tables)}, head_counts)
    if rows:
        return 1, message(rows, base, how)
    return 0, (f"ledger-ratchet: ok: no count above {base[:9]} ({how}); {len(head_files)} ledger files, "
               f"{sum(counts(head_files).values())} entries, {len(flat(head_tables))} pins")


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--repo", type=Path, default=REPO, help="the checkout to read (default: this one)")
    args = parser.parse_args(argv)
    sys.stdout.reconfigure(errors="backslashreplace")  # the arrow in a message must not crash a C-locale runner
    try:
        status, text = check(args.repo.resolve())
    except Unavailable as exc:
        print(f"ledger-ratchet: cannot compare: {exc}", file=sys.stderr)
        return 2
    print(text)
    return status


if __name__ == "__main__":
    sys.exit(main())
