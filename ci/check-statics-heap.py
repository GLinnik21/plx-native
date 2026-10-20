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

What counts as heap. The declared type, with every name it mentions followed into its own
definition anywhere in the tree (`struct`/`enum`/`union`/`type`, resolved by simple name whatever
its spelling and whether or not a std type shares it, all same-named definitions unioned, so the
answer is conservative), mentions one of `HEAP`. That is what makes `Option<KeyMemo>` (a struct
holding nested `HashMap`s) and `GroundProbe<N>` (a struct holding `Option<Vec<u8>>`) visible where
a regex over the declaration line sees nothing. Plain `Copy` data stays out: GL object names and
uniform locations (`c_uint`/`c_int`), function pointers (the signature of a `fn(..) -> ..` is not
read: a pointer owns nothing), `[c_int; 4]`, raw FFI handles (`*mut c_void`, `[*mut TtfFont; N]`),
flags, counters. A pointer or reference to a heap type IS reported, so a human decides who frees
it. A CamelCase name the script can neither find in the tree nor recognise as std plain data is
reported as heap (an unresolvable type is not evidence of absence), with the reason in the third
column; so is a static declared by a `macro_rules!` (`static mut $name`), whose type is not yet
text.

Scope. Every `.rs` file under a `rust-modules/<crate>` directory, tests included, never a
`rust-modules/target*` build tree. Comments and string literals are blanked first
(`module_graph.blank`), the same lexer the layering gates read Rust with. A `static mut` is found
wherever it is written (after an attribute on the same line, on a one-line fn body, with its name
on the next line); `&'static mut T` is a reference type, not a declaration.

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

# CamelCase names that are plain data or zero-sized wrappers, so naming one decides nothing UNLESS
# the tree defines a type of that name (`screens`' own `struct Cell` holds a `String`), which wins.
# (The `c_*` FFI scalars and the primitives are lower-case and never unresolved.)
PLAIN = frozenset("""
    Option Result Self Cell UnsafeCell MaybeUninit PhantomData NonNull Duration Instant
    AtomicBool AtomicI8 AtomicI16 AtomicI32 AtomicI64 AtomicIsize AtomicU8 AtomicU16 AtomicU32
    AtomicU64 AtomicUsize AtomicPtr Ordering
""".split())

# `(?<!')` keeps `&'static mut T` (a reference type, dozens in the tree) out; `\$` admits the name of
# a static a `macro_rules!` declares.
DECL = re.compile(r"(?<!')\bstatic\s+mut\s+(?:r#)?(\$?[A-Za-z_][A-Za-z_0-9]*)\s*:")
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
    """The text of an item from just after its name, to the end of its braced body or the first `;`
    outside any bracket (a tuple struct's `(..);`, an alias's `= ..;`, a unit struct's `;`)."""
    depth, i, n = 0, at, len(text)
    while i < n:
        c = text[i]
        if c == '-' and text[i + 1:i + 2] == '>':
            i += 2
            continue
        if c in '<([{':
            depth += 1
        elif c == '}':
            depth -= 1
            if depth <= 0:
                i += 1
                break
        elif c in '>)]':
            depth = max(depth - 1, 0)
        elif c == ';' and depth <= 0:
            break
        i += 1
    return text[at:i]


FN_PTR = re.compile(r'\bfn\s*\(')


def drop_fn_types(fragment):
    """`fragment` without any `fn(..) -> ..` pointer type: a pointer owns nothing, so neither its
    parameters nor its return type say what the static holds."""
    out, i = [], 0
    while True:
        m = FN_PTR.search(fragment, i)
        if not m:
            out.append(fragment[i:])
            return ''.join(out)
        out.append(fragment[i:m.start()])
        j, depth = m.end(), 1
        while j < len(fragment) and depth:
            depth += (fragment[j] == '(') - (fragment[j] == ')')
            j += 1
        rest = fragment[j:].lstrip()
        if rest.startswith('->'):
            j = len(fragment) - len(rest) + 2
            depth = 0
            while j < len(fragment):
                c = fragment[j]
                if c == '-' and fragment[j + 1:j + 2] == '>':
                    j += 2
                    continue
                if c in '<([':
                    depth += 1
                elif c in '>)]':
                    if depth == 0:
                        break
                    depth -= 1
                elif depth == 0 and c in ',;=':
                    break
                j += 1
        i = j


def looks_like_a_type(word):
    """A CamelCase word. Lower-case words are primitives, `c_*` scalars or path segments; SCREAMING
    words are consts and const-generic arguments; a single capital is a generic parameter."""
    return word[0].isupper() and any(ch.islower() for ch in word)


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
    """(owns_heap, why): follow every name in `type_text` through `defs`."""
    seen, todo = set(), [type_text]
    while todo:
        for name in IDENT.findall(drop_fn_types(todo.pop())):
            if name in HEAP:
                return True, f'owns `{name}`'
            if name in seen:
                continue
            seen.add(name)
            if name in defs:
                todo.extend(defs[name])
            elif name not in PLAIN and looks_like_a_type(name):
                return True, f'names `{name}`, which is defined nowhere in the tree'
    return False, ''


def heap_statics(root):
    decls, defs = scan(root)
    found = {}
    for rel, name, type_text in decls:
        if name.startswith('$'):
            heap, why = True, 'is declared through a macro, so its type is not read'
        else:
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
