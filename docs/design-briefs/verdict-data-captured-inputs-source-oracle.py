"""Source-only oracle for the four remaining data captured-input families (not Rust gates)."""
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[2]
FAMILIES = ("search.rs", "search/scope.rs", "search/recents.rs", "pms.rs", "metadata.rs", "viewstate.rs")
AMBIENT = re.compile(r"(?:plx_plex::plex::(?:session::)?(?:server_ids|server_roster_gen|server_facts_gen|server_facts|client_for|current_server|current_snapshot|current_gen|peek|peek_settled|visible_generation|current_profile_key|tenure)|crate::stores::tape::active)\s*\(")

def production(text):
    # Remove balanced test-only items, not everything after the first inline test module.
    pattern = re.compile(r'#\[cfg\((?:test|any\(test, feature = "test-support"\))\)\]\s*(?:#\[[^\n]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?(?:fn|mod|impl)\b')
    spans = []
    for match in pattern.finditer(text):
        start = text.find("{", match.end())
        semi = text.find(";", match.end())
        if semi != -1 and (start == -1 or semi < start):
            spans.append((match.start(), semi + 1))
            continue
        if start == -1:
            continue
        depth, end = 1, start + 1
        while depth and end < len(text):
            depth += (text[end] == "{") - (text[end] == "}")
            end += 1
        spans.append((match.start(), end))
    for start, end in reversed(spans):
        text = text[:start] + " " * (end - start) + text[end:]
    return re.sub(r"(?m)^\s*//.*$", "", text)

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
