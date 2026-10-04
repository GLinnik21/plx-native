#!/usr/bin/env python3
"""Module-cycle ratchet: the app crate's top-level module cycle may lose members, never gain them.

`rust-modules/src` has ~63 top-level modules (the non-test `mod` lines of lib.rs). The intended
layering is gfx/text/i18n < ui < screens < app and plex < route/player < app. A handful of thin
upward references (ui -> screens, ui -> app, gfx/text -> ui, plex -> route) once closed one
strongly connected component holding 44 of them; the module-layer migration (docs/module-layers.md,
gated per reference by ci/check-module-layers.py) cut it to 13 by step L14 (the set since is recorded in
ci/module-cycle-baseline.json), whose remaining edges are ones that
gate allows (this tool sees `diag` as one node). THIS gate only stops the cycle
absorbing more modules. It holds MEMBERSHIP, not edges: another reference between two
modules that are already on the cycle (another `stores` -> `browse`) does not fail it.
`ci/module-cycle-baseline.json` records the modules that sit on the cycle today, and this script
fails when

* a module that is not in the baseline joins a cycle (a new upward reference from a module that was
  outside it, a new module that lands inside it, or two cycles merging). It prints the module and
  the `file:line` references into and out of it that create the path;

and notices (exit 0) when a baseline member has left the cycle, so the gain can be locked in with
`--update-baseline`. `--update-baseline` also accepts a deliberate growth: use it only on purpose.

    ci/check-module-cycle.py                  check against the baseline (what `make check-python` runs)
    ci/check-module-cycle.py --update-baseline
    ci/check-module-cycle.py --report         the cycle, 2-cycle pairs, thin back-edges, layer breaks
    ci/check-module-cycle.py --dot > g.dot    graphviz of the module graph (cycle members filled)

How the graph is built. Starting at lib.rs, the real module tree is walked (`mod x;` resolved to
`x.rs` / `x/mod.rs`, honouring `#[path = "..."]`), so a file reached only through a `#[cfg(test)]`
module (`*_tests.rs`, `tests.rs`, `*_test_support.rs`) is never read. Inside each file comments and
string contents are blanked, then every `#[cfg(test)]` / `#[cfg(all(test, ..))]` item is blanked by
brace matching (a `mod tests { .. }`, a single `fn`/`use`/`let`/struct field...), and the rest is
scanned for `crate::<top-level module>` and the first segments of `use crate::{a, b::c}` groups.
An edge A -> B means "production code in A names `crate::B`". Feature-gated modules
(`devtriggers`, `hostsim`) count as production: the graph is the union over feature sets.

Honest limits. Paths are matched textually, not resolved: `super::`/`self::` hops that leave a
top-level module, `use crate as x`, glob re-exports, `$crate` in macros and names reaching another
module only through a macro or a `pub use` alias are not seen. `#[cfg(test)]` on an expression or
a match arm is blanked up to the next `;`/`,`/closing brace, which can over- or under-shoot on
exotic formatting (extra production text blanked means a missed edge; the reverse cannot happen
because a test-only edge would then be counted). `#[path]` is resolved against the file's own
directory, and `include!` of a literal path is followed; `include!(concat!(env!(..)))` is not.
Nothing here type-checks, so it errs towards fewer edges only in the cases listed.
"""
from __future__ import annotations

import argparse
import bisect
import collections
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "rust-modules" / "src"
BASELINE = ROOT / "ci" / "module-cycle-baseline.json"

# Advisory, used by --report only: an edge from an earlier rank to a later rank of one chain is an
# upward reference (a "layer break"). Nothing is enforced from this table.
LAYER_CHAINS = [
    [["gfx", "text", "i18n"], ["ui"], ["screens"], ["app"]],
    [["plex"], ["route", "player"], ["app"]],
]
THIN_EDGE = 20  # --report: an edge with at most this many references is "thin"


# ---------------------------------------------------------------- lexical masking

_TOKEN = re.compile(r"//[^\n]*|/\*|(?<![\w])(?:br|cr|r)(#*)\"|\"|'")
_STR_BODY = re.compile(r"(?:[^\"\\]|\\.)*\"", re.S)
_CHAR = re.compile(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|.)|[^\\'\n])'")


def _blank(s: str) -> str:
    if "\n" not in s:
        return " " * len(s)
    return "\n".join(" " * len(line) for line in s.split("\n"))


def mask(text: str) -> tuple[str, str]:
    """(no_comments, no_comments_no_strings), both the same length as `text`.

    Comments become spaces in both; string and char literal contents become spaces only in the
    second (the quotes stay, so `extern "C"` keeps its shape). Newlines always survive, so offsets
    and line numbers match the original.
    """
    a: list[str] = []
    b: list[str] = []
    pos = 0
    n = len(text)
    while True:
        m = _TOKEN.search(text, pos)
        if not m:
            break
        start = m.start()
        tok = m.group(0)
        plain = text[pos:start]
        a.append(plain)
        b.append(plain)
        pos = start
        if tok.startswith("//"):
            end = m.end()
            a.append(_blank(text[start:end]))
            b.append(_blank(text[start:end]))
        elif tok == "/*":
            depth, i = 1, m.end()
            while depth and i < n:
                o, c = text.find("/*", i), text.find("*/", i)
                if c < 0:
                    i = n
                    break
                if 0 <= o < c:
                    depth += 1
                    i = o + 2
                else:
                    depth -= 1
                    i = c + 2
            end = i
            a.append(_blank(text[start:end]))
            b.append(_blank(text[start:end]))
        elif tok == "'":
            cm = _CHAR.match(text, start)
            if not cm:  # a lifetime or label: leave it
                a.append("'")
                b.append("'")
                pos = start + 1
                continue
            end = cm.end()
            a.append(text[start:end])
            b.append("'" + _blank(text[start + 1:end - 1]) + "'")
        elif tok == '"':
            sm = _STR_BODY.match(text, m.end())
            end = sm.end() if sm else n
            a.append(text[start:end])
            b.append('"' + _blank(text[m.end():end - 1]) + '"')
        else:  # raw string r#"..."#
            close = '"' + m.group(1)
            e = text.find(close, m.end())
            end = n if e < 0 else e + len(close)
            a.append(text[start:end])
            b.append(text[start:m.end()] + _blank(text[m.end():max(m.end(), end - len(close))]) + close)
        pos = end
    a.append(text[pos:])
    b.append(text[pos:])
    return "".join(a), "".join(b)


# ---------------------------------------------------------------- cfg(test) blanking

_CFG = re.compile(r"#\s*(!?)\s*\[\s*cfg\s*\(")
_ITEM_KW = {"mod", "fn", "impl", "trait", "struct", "enum", "union", "macro_rules", "extern", "unsafe", "async"}
_SEMI_KW = {"use", "static", "type", "let"}
_MODS = re.compile(r"(?:pub\s*(?:\([^)]*\)\s*)?)?")
_WORD = re.compile(r"[A-Za-z_]\w*")


def _balanced(s: str, i: int, open_c: str, close_c: str) -> int:
    """Index just past the bracket that closes the `open_c` at s[i]; len(s) if unbalanced."""
    depth = 0
    for j in range(i, len(s)):
        c = s[j]
        if c == open_c:
            depth += 1
        elif c == close_c:
            depth -= 1
            if depth == 0:
                return j + 1
    return len(s)


def _top_level_args(pred: str) -> list[str]:
    out, depth, cur = [], 0, []
    for c in pred:
        if c in "([":
            depth += 1
        elif c in ")]":
            depth -= 1
        if c == "," and depth == 0:
            out.append("".join(cur).strip())
            cur = []
        else:
            cur.append(c)
    out.append("".join(cur).strip())
    return [x for x in out if x]


def _is_test_predicate(pred: str) -> bool:
    pred = pred.strip()
    if pred == "test":
        return True
    m = re.fullmatch(r"all\s*\((.*)\)", pred, re.S)
    return bool(m) and any(_is_test_predicate(x) for x in _top_level_args(m.group(1)))


def _item_end(s: str, i: int) -> int:
    """End of the item/statement/field that starts at s[i] (after its attributes)."""
    n = len(s)
    while True:  # further attributes
        while i < n and s[i].isspace():
            i += 1
        if s.startswith("#", i):
            j = i + 1
            while j < n and s[j].isspace():
                j += 1
            if j < n and s[j] == "!":
                j += 1
                while j < n and s[j].isspace():
                    j += 1
            if j < n and s[j] == "[":
                i = _balanced(s, j, "[", "]")
                continue
        break
    head = s[i:i + 200]
    head = head[_MODS.match(head).end():]
    w = _WORD.match(head)
    kw = w.group(0) if w else ""
    if kw == "const":
        head_line = re.split(r"[=;{(]", head, 1)[0]
        kw = "fn" if re.search(r"\bfn\b", head_line) else "const"
    if kw in _SEMI_KW or kw == "const":
        mode = "semi"
    elif kw in _ITEM_KW:
        mode = "block"
    else:
        mode = "expr"
    depth = 0
    j = i
    while j < n:
        c = s[j]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            if depth == 0:
                return j  # the enclosing block closes: leave its brace alone
            depth -= 1
            if depth == 0 and c == "}" and mode != "semi":
                k = j + 1
                if mode == "expr":  # `field: T { .. },` / `Pat => { .. },`
                    while k < n and s[k].isspace():
                        k += 1
                    if k < n and s[k] == ",":
                        return k + 1
                    return j + 1
                return j + 1
        elif depth == 0 and c == ";":
            return j + 1
        elif depth == 0 and c == "," and mode == "expr":
            return j + 1
        j += 1
    return n


def blank_cfg_test(a: str, b: str) -> tuple[str, str, bool]:
    """Blank every cfg(test)-only item in both masked texts; True when the whole file is test-only."""
    spans = []
    upto = 0
    for m in _CFG.finditer(b):
        if m.start() < upto:
            continue
        close = _balanced(b, m.end() - 1, "(", ")")
        pred = b[m.end():close - 1]
        if not _is_test_predicate(pred):
            continue
        k = close
        while k < len(b) and b[k].isspace():
            k += 1
        if k >= len(b) or b[k] != "]":
            continue
        if m.group(1) == "!":  # `#![cfg(test)]`: the whole file (or enclosing module) is test code
            return a, b, True
        end = _item_end(b, k + 1)
        spans.append((m.start(), end))
        upto = end
    if not spans:
        return a, b, False
    pa, pb, last = [], [], 0
    for s0, e0 in spans:
        pa.append(a[last:s0])
        pb.append(b[last:s0])
        pa.append(_blank(a[s0:e0]))
        pb.append(_blank(b[s0:e0]))
        last = e0
    pa.append(a[last:])
    pb.append(b[last:])
    return "".join(pa), "".join(pb), False


# ---------------------------------------------------------------- module tree

_MOD_DECL = re.compile(r"\bmod\s+([A-Za-z_]\w*)\s*;")
_PATH_ATTR = re.compile(r"#\s*\[\s*path\s*=\s*\"([^\"]*)\"\s*\]")
_INCLUDE = re.compile(r"\binclude!\s*\(\s*\"([^\"]*)\"\s*\)")


_NEWLINE = re.compile(r"\n")


class Source:
    """One production file: its masked text and the top-level module that owns it."""

    def __init__(self, path: Path, owner: str, text: str):
        self.path, self.owner = path, owner
        self.text = text  # comments, strings and cfg(test) items blanked
        self.newlines = [m.start() for m in _NEWLINE.finditer(text)]

    def line(self, offset: int) -> int:
        return bisect.bisect_left(self.newlines, offset) + 1


def _load(path: Path) -> tuple[str, str, bool] | None:
    try:
        raw = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    a, b = mask(raw)
    return blank_cfg_test(a, b)


def _children(path: Path, a: str, b: str) -> list[tuple[str, Path]]:
    """(module name, file) for each non-test `mod x;` declared in a file, plus literal `include!`s."""
    out = []
    is_root = path.name in ("lib.rs", "main.rs", "mod.rs")
    base = path.parent if is_root else path.parent / path.stem
    for m in _MOD_DECL.finditer(b):
        name = m.group(1)
        # The attributes of this declaration: the text since the previous item ended.
        window = a[max(0, m.start() - 400):m.start()]
        window = window[max(window.rfind(";"), window.rfind("}"), window.rfind("{")) + 1:]
        pm = _PATH_ATTR.search(window)
        if pm:
            cands = [path.parent / pm.group(1)]
        else:
            cands = [base / (name + ".rs"), base / name / "mod.rs"]
            if not is_root:  # also tolerate a sibling layout
                cands.append(path.parent / (name + ".rs"))
        for c in cands:
            if c.is_file():
                out.append((name, c))
                break
    for m in _INCLUDE.finditer(a):
        # Only when the include is not blanked: check that b still has the call at that offset.
        if b[m.start():m.start() + 8] == "include!":
            inc = path.parent / m.group(1)
            if inc.is_file():
                out.append(("", inc))
    return out


def top_level_modules(src: Path) -> list[tuple[str, Path]]:
    loaded = _load(src / "lib.rs")
    if not loaded:
        raise SystemExit(f"module-cycle: cannot read {src / 'lib.rs'}")
    a, b, _ = loaded
    return [(n, p) for n, p in _children(src / "lib.rs", a, b) if n]


def collect(src: Path) -> tuple[list[str], list[Source]]:
    """The top-level module names and every production file, owner-tagged."""
    mods = top_level_modules(src)
    names = sorted({n for n, _ in mods})
    sources: list[Source] = []
    seen: set[Path] = set()
    stack = [(n, p) for n, p in mods]
    while stack:
        owner, path = stack.pop()
        path = path.resolve()
        if path in seen:
            continue
        seen.add(path)
        loaded = _load(path)
        if not loaded:
            continue
        a, b, whole_file_test = loaded
        if whole_file_test:
            continue
        sources.append(Source(path, owner, b))
        for _, child in _children(path, a, b):
            stack.append((owner, child))
    return names, sources


# ---------------------------------------------------------------- graph

_CRATE = re.compile(r"\bcrate\s*::\s*(\{)?\s*([A-Za-z_]\w*)?")


def build_graph(src: Path):
    """(modules, edges, sources) where edges[a][b] is the list of (file, line) naming crate::b in a."""
    names, sources = collect(src)
    modset = set(names)
    edges: dict[str, dict[str, list[tuple[str, int]]]] = collections.defaultdict(lambda: collections.defaultdict(list))
    for s in sources:
        rel = _rel(s.path, src)
        for m in _CRATE.finditer(s.text):
            if m.group(1):  # `crate::{a, b::c}`: the first segment of each depth-0 item
                open_at = s.text.index("{", m.start())
                close = _balanced(s.text, open_at, "{", "}")
                depth = 0
                want = True
                j = open_at
                while j < close:
                    c = s.text[j]
                    if c == "{":
                        depth += 1
                        want = depth == 1
                    elif c == "}":
                        depth -= 1
                    elif c == "," and depth == 1:
                        want = True
                    elif want and depth == 1:
                        w = _WORD.match(s.text, j)
                        if w:
                            t = w.group(0)
                            if t in modset and t != s.owner:
                                edges[s.owner][t].append((rel, s.line(j)))
                            j = w.end()
                            want = False
                            continue
                    j += 1
            elif m.group(2):
                t = m.group(2)
                if t in modset and t != s.owner:
                    edges[s.owner][t].append((rel, s.line(m.start(2))))
    return names, edges, sources


def _rel(path: Path, src: Path) -> str:
    try:
        return str(path.relative_to(src.parent.parent))
    except ValueError:
        return str(path)


def sccs(names: list[str], edges) -> list[list[str]]:
    """Strongly connected components (Tarjan), each sorted, largest first."""
    index: dict[str, int] = {}
    low: dict[str, int] = {}
    on: set[str] = set()
    stack: list[str] = []
    out: list[list[str]] = []
    counter = [0]

    def strong(v: str) -> None:
        index[v] = low[v] = counter[0]
        counter[0] += 1
        stack.append(v)
        on.add(v)
        for w in sorted(edges.get(v, ())):
            if w not in index:
                strong(w)
                low[v] = min(low[v], low[w])
            elif w in on:
                low[v] = min(low[v], index[w])
        if low[v] == index[v]:
            comp = []
            while True:
                w = stack.pop()
                on.discard(w)
                comp.append(w)
                if w == v:
                    break
            out.append(sorted(comp))

    for v in names:
        if v not in index:
            strong(v)
    return sorted(out, key=lambda c: (-len(c), c))


def cycles(names, edges) -> list[list[str]]:
    """Every strongly connected component with more than one module (no module references itself)."""
    return [c for c in sccs(names, edges) if len(c) > 1]


def snapshot(names, edges) -> dict:
    cyc = cycles(names, edges)
    members = cyc[0] if cyc else []
    return {
        "members": members,
        "size": len(members),
        "outside_count": len(names) - len(members),
        "other_cycles": cyc[1:],
    }


# ---------------------------------------------------------------- check

def _fmt_refs(refs, limit=6) -> str:
    shown = "\n".join(f"      {f}:{ln}" for f, ln in refs[:limit])
    more = len(refs) - limit
    return shown + (f"\n      ... and {more} more" if more > 0 else "")


def explain(new: list[str], comp: list[str], edges) -> str:
    """Why `new` (modules outside the baseline) now sit on the cycle `comp`.

    First the references that close it - from a new module into a module that was already on the
    cycle (or, when there is no old part, every reference inside the new set) - then, one
    `file:line` each, how the old cycle reaches the new modules.
    """
    new_set, comp_set = set(new), set(comp)
    closers = [(s, t) for s in new for t in sorted(edges.get(s, {})) if t in comp_set and t not in new_set]
    if not closers:
        closers = [(s, t) for s in new for t in sorted(edges.get(s, {})) if t in new_set]
    entries = [(s, t) for s in comp if s not in new_set for t in sorted(edges.get(s, {})) if t in new_set]
    lines = ["  module(s) newly on a cycle: " + ", ".join(f"'{m}'" for m in new)
             + f"  (cycle of {len(comp)} modules)",
             "    the references that close it:"]
    for s, t in closers:
        lines.append(f"    {s} -> {t}  ({len(edges[s][t])})\n{_fmt_refs(edges[s][t])}")
    if entries:
        lines.append("    and the path into the new module(s) from the existing cycle (first reference of each):")
        for s, t in entries[:8]:
            f, ln = edges[s][t][0]
            lines.append(f"    {s} -> {t}  ({len(edges[s][t])})  {f}:{ln}")
        if len(entries) > 8:
            lines.append(f"    ... and {len(entries) - 8} more")
    return "\n".join(lines)


def check(names, edges, baseline: dict) -> tuple[list[str], list[str]]:
    """(failures, notices) of the current graph against a baseline snapshot."""
    allowed = [set(baseline.get("members", []))] + [set(c) for c in baseline.get("other_cycles", [])]
    failures, notices = [], []
    for comp in cycles(names, edges):
        if any(set(comp) <= a for a in allowed):
            continue
        # Which baseline group(s) does this component touch? The modules NOT in any group are the new ones.
        in_base = set().union(*allowed)
        new = [m for m in comp if m not in in_base]
        if new:
            failures.append(explain(new, comp, edges))
        else:
            touched = [sorted(a) for a in allowed if a & set(comp)]
            failures.append(
                "  two baseline cycles merged: " + " + ".join(f"[{len(t)} modules]" for t in touched)
                + "\n    new cross-references:\n" + "\n".join(
                    f"    {s} -> {t}\n{_fmt_refs(edges[s][t])}"
                    for s in comp for t in sorted(edges.get(s, {}))
                    if t in comp and not any(s in a and t in a for a in allowed)))
    base_members = set(baseline.get("members", []))
    still = set().union(*[set(c) for c in cycles(names, edges)]) if cycles(names, edges) else set()
    left = sorted(base_members - still)
    if left:
        gone = [m for m in left if m not in names]
        moved = [m for m in left if m in names]
        text = f"the cycle shrank: {', '.join(left)} left it"
        if gone:
            text += f" ({', '.join(gone)} no longer exists)"
        notices.append(text + ". Run `ci/check-module-cycle.py --update-baseline` to lock in the gain.")
    elif not failures:
        cur = snapshot(names, edges)
        if cur["outside_count"] != baseline.get("outside_count", cur["outside_count"]) and cur["members"] == baseline.get("members"):
            notices.append(
                f"modules outside the cycle: {cur['outside_count']} (baseline {baseline.get('outside_count')}); "
                "run `ci/check-module-cycle.py --update-baseline` to record it.")
    return failures, notices


# ---------------------------------------------------------------- report / dot

def report(names, edges, out) -> None:
    cyc = cycles(names, edges)
    members = cyc[0] if cyc else []
    ms = set(members)
    p = lambda s="": print(s, file=out)
    p(f"top-level modules: {len(names)}")
    p(f"largest cycle (SCC): {len(members)} modules")
    p("  " + ", ".join(members))
    outside = [m for m in names if m not in ms]
    p(f"outside it ({len(outside)}): " + ", ".join(outside))
    if len(cyc) > 1:
        p(f"other cycles: {[c for c in cyc[1:]]}")
    count = lambda a, b: len(edges.get(a, {}).get(b, ()))
    pairs = []
    for a in members:
        for b in members:
            if a < b and count(a, b) and count(b, a):
                pairs.append((count(a, b) + count(b, a), a, b))
    pairs.sort(reverse=True)
    p(f"\nmutual (2-cycle) pairs: {len(pairs)}; top 10 by references")
    for tot, a, b in pairs[:10]:
        p(f"  {a:>12s} <-> {b:<12s} {a}->{b}: {count(a, b):4d}   {b}->{a}: {count(b, a):4d}")
    p(f"\nthin back-edges (the minority direction of a mutual pair, at most {THIN_EDGE} references):")
    thin = [(a, b) for tot, a, b in pairs for (a, b) in
            ((a, b) if count(a, b) < count(b, a) else (b, a),) if count(a, b) <= THIN_EDGE]
    for a, b in sorted(thin, key=lambda e: (count(*e), e)):
        refs = edges[a][b]
        p(f"  {a} -> {b}  ({len(refs)}; reverse {count(b, a)})")
        for f, ln in refs:
            p(f"      {f}:{ln}")
    p("\nlayer breaks (upward references against the intended layering):")
    rank = {}
    for ci, chain in enumerate(LAYER_CHAINS):
        for r, group in enumerate(chain):
            for m in group:
                rank[(ci, m)] = r
    shown: set[tuple[str, str]] = set()
    breaks = []
    for ci in range(len(LAYER_CHAINS)):
        for a in names:
            for b in edges.get(a, {}):
                if (ci, a) in rank and (ci, b) in rank and rank[(ci, a)] < rank[(ci, b)] and (a, b) not in shown:
                    shown.add((a, b))
                    breaks.append((a, b))
    for a, b in sorted(breaks, key=lambda e: (count(*e), e)):
        refs = edges[a][b]
        p(f"  {a} -> {b}  ({len(refs)})")
        if len(refs) <= THIN_EDGE:
            for f, ln in refs:
                p(f"      {f}:{ln}")
    if not breaks:
        p("  none")


def dot(names, edges, out) -> None:
    cyc = cycles(names, edges)
    ms = set(cyc[0]) if cyc else set()
    p = lambda s: print(s, file=out)
    p("digraph modules {")
    p("  rankdir=LR; node [shape=box, fontsize=10];")
    for m in names:
        p(f'  "{m}"' + (' [style=filled, fillcolor="#f4c7c3"];' if m in ms else ";"))
    for a in sorted(edges):
        for b in sorted(edges[a]):
            both = a in ms and b in ms
            p(f'  "{a}" -> "{b}" [label="{len(edges[a][b])}"' + (', color="#b3261e"' if both else "") + "];")
    p("}")


# ---------------------------------------------------------------- main

def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--src", type=Path, default=SRC, help="crate source directory containing lib.rs")
    ap.add_argument("--baseline", type=Path, default=BASELINE)
    mode = ap.add_mutually_exclusive_group()
    mode.add_argument("--update-baseline", action="store_true", help="rewrite the baseline from the current tree")
    mode.add_argument("--report", action="store_true", help="print the cycle and the thin back-edges")
    mode.add_argument("--dot", action="store_true", help="print the module graph as graphviz")
    args = ap.parse_args(argv)

    names, edges, _ = build_graph(args.src)
    if args.report:
        report(names, edges, sys.stdout)
        return 0
    if args.dot:
        dot(names, edges, sys.stdout)
        return 0
    snap = snapshot(names, edges)
    if args.update_baseline:
        args.baseline.write_text(json.dumps(snap, indent=2) + "\n")
        print(f"module-cycle: baseline written: {snap['size']} modules on the cycle, "
              f"{snap['outside_count']} outside ({args.baseline})")
        return 0
    try:
        baseline = json.loads(args.baseline.read_text())
    except (OSError, ValueError) as e:
        print(f"module-cycle: cannot read baseline {args.baseline}: {e}\n"
              "  create it with `ci/check-module-cycle.py --update-baseline`", file=sys.stderr)
        return 1
    failures, notices = check(names, edges, baseline)
    for n in notices:
        print(f"module-cycle: notice: {n}")
    if failures:
        print("module-cycle: FAIL: the module cycle grew.\n" + "\n".join(failures) + "\n"
              "  Fix: move the shared type down a layer, pass the value in, or invert the call with a trait or\n"
              "  callback. Only if the growth is deliberate: ci/check-module-cycle.py --update-baseline\n"
              "  (see docs/agent-reference.md, 'Module-cycle ratchet').", file=sys.stderr)
        return 1
    print(f"module-cycle: ok ({snap['size']} modules on the cycle, baseline {baseline.get('size')}; "
          f"{snap['outside_count']} outside)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
