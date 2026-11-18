"""Source-only oracle for the four remaining data captured-input families (not Rust gates)."""
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "ci"))
from rust_test_modules import group_ends, lex

ROOT = Path(__file__).resolve().parents[2]
FAMILIES = ("search.rs", "search/scope.rs", "search/recents.rs", "pms.rs", "metadata.rs", "viewstate.rs")
AMBIENT = re.compile(r"(?:plx_plex::plex::(?:session::)?(?:server_ids|server_roster_gen|server_facts_gen|server_facts|client_for|current_server|current_snapshot|current_gen|peek|peek_settled|visible_generation|current_profile_key|tenure)|crate::stores::tape::active)\s*\(")

def production(text):
    # Reuse the Rust lexer so comments/literals cannot forge attributes or braces.
    tokens = lex(text)
    ends = group_ends(tokens)
    test_cfgs = (lex('cfg(test)'), lex('cfg(any(test, feature = "test-support"))'))
    kept, i, size = [], 0, len(tokens)
    while i < size:
        start, test_only = i, False
        # Each balanced attribute is consumed once, including nested token groups.
        while (i + 1 < size and tokens[i] == ('code', '#')
               and tokens[i + 1] == ('code', '[') and i + 1 in ends):
            end = ends[i + 1]
            test_only |= tokens[i + 2:end] in test_cfgs
            i = end + 1
        item = i
        if item < size and tokens[item] == ('code', 'pub'):
            item += 1
            if item < size and tokens[item] == ('code', '(') and item in ends:
                item = ends[item] + 1
        if test_only and item < size and tokens[item] in (
                ('code', 'fn'), ('code', 'mod'), ('code', 'impl')):
            end = item + 1
            while end < size and tokens[end] not in (('code', '{'), ('code', ';')):
                if tokens[end] in (('code', '('), ('code', '[')):
                    if end not in ends:
                        break
                    end = ends[end] + 1
                else:
                    end += 1
            if end < size and tokens[end] == ('code', ';'):
                i = end + 1
                continue
            if end < size and tokens[end] == ('code', '{') and end in ends:
                i = ends[end] + 1
                continue
            # Malformed items fail closed; keep their reads and never rescan a header.
            i = end
        if i == start:
            i += 1
        kept.extend(tokens[start:i])
    # Keep path punctuation adjacent for AMBIENT; literals remain opaque separators.
    return ''.join(value if kind == 'code' else '""' for kind, value in kept)

def main():
    failures = []
    for family in FAMILIES:
        text = production((ROOT / "rust-modules/data/src" / family).read_text(encoding="utf-8"))
        for match in AMBIENT.finditer(text):
            failures.append(f"{family}: ambient machine read: {match.group()}")
    for family in ("search", "metadata", "pms", "viewstate"):
        ledger = ROOT / "ci/allow/globals" / f"data-{family}.txt"
        if any(line.strip() and not line.lstrip().startswith("#") for line in ledger.read_text().splitlines()):
            failures.append(f"{family}: retired ledger still has entries")
    metadata = (ROOT / "rust-modules/data/src/metadata.rs").read_text(encoding="utf-8")
    if not re.search(r"#\[serde\(skip, default\)\]\s*pub source_credit: String", metadata):
        failures.append("metadata: captured presentation credit must not enter recorded Detail JSON")
    if failures:
        print("\n".join(failures))
        return 1
    print("PASS: Search/Metadata/PMS/ViewState captured-input source boundary; runtime gates not run")
    return 0

if __name__ == "__main__":
    sys.exit(main())
