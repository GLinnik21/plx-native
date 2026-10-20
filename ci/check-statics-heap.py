#!/usr/bin/env python3
"""The tree-wide half of `ci/check-statics.sh`: every `static mut` under rust-modules whose TYPE
OWNS HEAP, printed as `path<TAB>NAME<TAB>type: why` (one per line, sorted).

Why it exists. A `static mut` that owns heap and is replaced, cleared or inserted into frees
memory; two threads doing that to the one static free each other's data. `cargo test` runs tests
on parallel threads, so a render cache written like a boot-time global crashes a suite at random:
`plx_ui`'s `TAB_CACHE` aborted `make test-crate C="plx_ui plx_screens"` with a libmalloc
"pointer being freed was not allocated" (2026-10-20). The cure is `thread_local!` for a frame-loop
cache, which `ci/check-statics.sh` already names for `ui/` and `screens/`; this script extends the
refusal to every crate, so the next `static mut ELIDE_CACHE: Option<HashMap<..>>` fails the gate
instead of a stranger's test run.

What counts as heap. The declared type, with every named type it mentions followed into its own
definition anywhere in the tree (`struct`/`enum`/`union`/`type`, resolved by simple name, all
same-named definitions unioned, so the answer is conservative), mentions one of `HEAP`. That is
what makes `Option<KeyMemo>` (a struct holding nested `HashMap`s) and `GroundProbe<N>` (a struct
holding `Option<Vec<u8>>`) visible where a regex over the declaration line sees nothing. Plain
`Copy` data stays out: GL object names and uniform locations (`c_uint`/`c_int`), function
pointers, `[c_int; 4]`, raw FFI handles (`*mut c_void`, `[*mut TtfFont; N]`), flags, counters.
A type name the script can neither find nor recognise as std plain data is reported as heap (an
unresolvable type is not evidence of absence), with the reason in the third column.

Scope. Every `.rs` file under a `rust-modules/<crate>` directory, tests included, never a
`rust-modules/target*` build tree. Comments and string literals are blanked first
(`module_graph.blank`), the same lexer the layering gates read Rust with.

Usage: `ci/check-statics-heap.py [REPO_ROOT]` (default: the checkout this file lives in).
"""
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from module_graph import blank  # noqa: E402

# Types that own a heap allocation. `Cow` is here because its owned arm does.
HEAP = frozenset("""
    Vec VecDeque BinaryHeap LinkedList HashMap HashSet BTreeMap BTreeSet IndexMap IndexSet
    String CString OsString PathBuf Box Rc Arc Weak Cow
""".split())

# CamelCase names that are plain data or zero-sized wrappers, so naming one decides nothing.
# (The `c_*` FFI scalars and the primitives are lower-case and never looked at.)
PLAIN = frozenset("""
    Option Result Self Cell UnsafeCell MaybeUninit PhantomData NonNull Duration Instant
    AtomicBool AtomicI8 AtomicI16 AtomicI32 AtomicI64 AtomicIsize AtomicU8 AtomicU16 AtomicU32
    AtomicU64 AtomicUsize AtomicPtr Ordering
""".split())

DECL = re.compile(
    r'^[ \t]*(?:pub(?:\([^)\n]*\))?[ \t]+)?static[ \t]+mut[ \t]+([A-Za-z_][A-Za-z_0-9]*)[ \t]*:',
    re.M)
DEF = re.compile(r'\b(?:struct|enum|union|type)[ \t\n]+([A-Za-z_][A-Za-z_0-9]*)')
IDENT = re.compile(r'[A-Za-z_][A-Za-z_0-9]*')


def rust_files(root):
    """Every `.rs` under `rust-modules/<dir>`, except the `target*` build trees."""
    base = Path(root) / 'rust-modules'
    for top in sorted(base.iterdir()):
        if top.is_dir() and not top.name.startswith(('target', '.')):
            yield from sorted(top.rglob('*.rs'))


def declared_type(text, at):
    """The type text after `NAME:` at `at`, up to the `=` or `;` that ends it (angle brackets and
    the `->` of a function-pointer type are tracked, so neither ends it early)."""
    depth, i, n = 0, at, len(text)
    while i < n:
        c = text[i]
        if c == '-' and text[i + 1:i + 2] == '>':
            i += 2
            continue
        if c in '<([':
            depth += 1
        elif c in '>)]':
            depth -= 1
        elif depth <= 0 and c in '=;':
            break
        i += 1
    return text[at:i]


def definition_body(text, at):
    """The text of an item from just after its name: through the balanced `{..}` of a braced item,
    through `(..)` and the closing `;` of a tuple struct, to the `;` of an alias or unit struct."""
    i, n = at, len(text)
    while i < n and text[i] not in '{(;':
        i += 1
    if i >= n:
        return text[at:]
    if text[i] == ';':
        return text[at:i]
    depth = 0
    while i < n:
        if text[i] in '{([':
            depth += 1
        elif text[i] in '})]':
            depth -= 1
            if depth == 0:
                i += 1
                break
        i += 1
    if text[at:i].rstrip().endswith(')') or text[i:i + 1] == ';':
        j = text.find(';', i)
        if j >= 0 and not text[i:j].strip():
            i = j
    return text[at:i]


def type_names(fragment):
    """The type-looking names in a Rust type fragment: CamelCase identifiers. Lower-case words are
    primitives, `c_*` scalars or path segments; SCREAMING_CASE words are const-generic arguments
    and array lengths; a single capital is a generic parameter. None of those is a type to look up."""
    for word in IDENT.findall(fragment):
        if word[0].isupper() and any(ch.islower() for ch in word):
            yield word


def scan(root):
    """([(path, name, type_text)] for every `static mut`, {type name: [body, ...]}) over the tree."""
    decls, defs = [], {}
    for path in rust_files(root):
        text, _ = blank(path.read_text(encoding='utf-8', errors='replace'))
        rel = path.relative_to(root).as_posix()
        for m in DECL.finditer(text):
            decls.append((rel, m.group(1), declared_type(text, m.end())))
        for m in DEF.finditer(text):
            defs.setdefault(m.group(1), []).append(definition_body(text, m.end()))
    return decls, defs


def owner(type_text, defs):
    """(owns_heap, why): follow every named type in `type_text` through `defs`."""
    seen, todo = set(), [type_text]
    while todo:
        for name in type_names(todo.pop()):
            if name in HEAP:
                return True, f'owns `{name}`'
            if name in seen or name in PLAIN:
                continue
            seen.add(name)
            if name not in defs:
                return True, f'names `{name}`, which is defined nowhere in the tree'
            todo.extend(defs[name])
    return False, ''


def heap_statics(root):
    decls, defs = scan(root)
    found = {}
    for rel, name, type_text in decls:
        heap, why = owner(type_text, defs)
        if heap:
            found[(rel, name)] = (' '.join(type_text.split()), why)
    return found


def main(argv):
    root = Path(argv[1]) if len(argv) > 1 else Path(__file__).resolve().parent.parent
    found = heap_statics(root)
    for (rel, name), (type_text, why) in sorted(found.items()):
        print(f'{rel}\t{name}\t{type_text} — {why}')
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
