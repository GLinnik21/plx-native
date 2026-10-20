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
a regex over the declaration line sees nothing. Only a definition a bare name can reach counts:
`type Item = String;` inside an `impl` or `trait` block is an ASSOCIATED type (spelled
`Self::Item`), so it is not unioned with a module-level `enum Item`. Plain `Copy` data stays out:
GL object names and uniform locations (`c_uint`/`c_int`), function pointers (the signature of a
`fn(..) -> ..` is not read: a pointer owns nothing), `[c_int; 4]`, raw FFI handles (`*mut c_void`,
`[*mut TtfFont; N]`), flags, counters, and the std plain-data types `PLAIN` lists (times, `NonZero*`,
`Wrapping`, atomics, and the `Cell`-family and `ManuallyDrop`/`MaybeUninit`/`PhantomData` wrappers,
each plain only while the parameter it wraps is: `RefCell<Vec<u8>>` still owns a `Vec`). A pointer
or reference to a heap type IS reported, so a human decides who frees it. A CamelCase name the
script can neither find in the tree nor recognise as std plain data is reported as heap (an
unresolvable type is not evidence of absence), with the reason in the third column; when the type
is in fact plain data the detector misread, the cure is `PLAIN` here, never an allowlist entry.
A static declared by a `macro_rules!` (`static mut $name`) has no type text yet: it is reported as
heap under the macro's own name and `!` (`cache!`), with the line in the third column, so the
allowlist can name it (a `$` in an allowlist name would anchor in `grep -E` and never match).

Known gap. Only `static mut` is read. A non-`mut` static whose type is an `UnsafeCell`/`SyncCell`
wrapper around heap, made `Sync` by an `unsafe impl`, frees memory the same way and is NOT seen;
the tree has none today. Seeing it needs the declaration's synchronisation story (a `Mutex`/`RwLock`
/`OnceLock` makes the same text safe, and `thread_local!` declares non-`mut` statics of exactly
that shape on purpose), which is a different rule from this one, so it is left to review.

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
# A wrapper in this list is plain only while the names inside its angle brackets are: the scan reads
# every name of the declared type, so `RefCell<Vec<u8>>` still finds its `Vec`.
PLAIN = frozenset("""
    Option Result Self Ordering NonNull PhantomData
    Cell RefCell UnsafeCell MaybeUninit ManuallyDrop Wrapping Saturating
    Duration Instant SystemTime
    Atomic AtomicBool AtomicI8 AtomicI16 AtomicI32 AtomicI64 AtomicIsize AtomicU8 AtomicU16
    AtomicU32 AtomicU64 AtomicUsize AtomicPtr
""".split())
# `NonZero<T>` and its aliases `NonZeroU8` .. `NonZeroUsize`, `NonZeroI8` .. `NonZeroIsize`.
PLAIN_NONZERO = re.compile(r'NonZero(?:[UI](?:8|16|32|64|128|size))?')

# `(?<!')` keeps `&'static mut T` (a reference type, dozens in the tree) out; `\$` admits the name of
# a static a `macro_rules!` declares (`scan` renames it to its macro).
DECL = re.compile(r"(?<!')\bstatic\s+mut\s+(?:r#)?(\$?[A-Za-z_][A-Za-z_0-9]*)\s*:")
DEF = re.compile(r'\b(struct|enum|union|type)[ \t\n]+([A-Za-z_][A-Za-z_0-9]*)')
IDENT = re.compile(r'[A-Za-z_][A-Za-z_0-9]*')
# A block whose body holds ASSOCIATED items: the head of an `impl` or `trait` (attributes, visibility,
# `unsafe`, `default`/`auto` first). A `fn` head that merely mentions `impl Trait` does not match.
ASSOC_HEAD = re.compile(
    r'\s*(?:#!?\[[^\]]*\]\s*)*(?:pub\b\s*(?:\([^)]*\)\s*)?)?(?:unsafe\s+)?(?:default\s+|auto\s+)?(?:impl|trait)\b')
STRUCTURE = re.compile(r'[{}()\[\];]')
MACRO_RULES = re.compile(r'\bmacro_rules!\s*([A-Za-z_][A-Za-z_0-9]*)\s*(?=[{(\[])')


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


def without_variant_names(body):
    """An enum's `body` (the text after its name) with each variant's own name removed: `Idle` is
    CamelCase but names no type, while a variant's payload (`Loaded(Vec<u8>)`) does. Attributes on
    a variant are kept; a discriminant (`= 2`) holds no type name."""
    start = body.find('{')
    if start < 0:
        return body
    out, depth, i, n, want = [body[:start]], 0, start, len(body), False
    while i < n:
        c = body[i]
        if c in '{([':
            depth += 1
            want = want or (c == '{' and depth == 1)
        elif c in '})]':
            depth -= 1
        elif depth == 1:
            if c == ',':
                want = True
            elif want and c == '#':  # a variant attribute: copied whole, its brackets matched
                j, nest = i, 0
                while j < n:
                    nest += (body[j] == '[') - (body[j] == ']')
                    j += 1
                    if nest == 0 and body[j - 1] == ']':
                        break
                out.append(body[i:j])
                i = j
                continue
            elif want and (c.isalpha() or c == '_'):
                i = IDENT.match(body, i).end()
                want = False
                continue
        out.append(c)
        i += 1
    return ''.join(out)


def definitions(text):
    """[(name, body)] for every `struct`/`enum`/`union`/`type` item in `text` that a bare name can
    reach. A `type` directly inside an `impl` or `trait` body is an associated type and is skipped;
    one in a `fn` body inside an `impl` is a local alias and stays."""
    found, stack, pos, head, nest = [], [], 0, 0, 0
    for m in DEF.finditer(text):
        for t in STRUCTURE.finditer(text, pos, m.start()):
            c, at = t.group(), t.start()
            if c == '{':
                stack.append(ASSOC_HEAD.match(text, head, at) is not None)
                head, nest = at + 1, 0
            elif c == '}':
                if stack:
                    stack.pop()
                head, nest = at + 1, 0
            elif c in '([':
                nest += 1
            elif c in ')]':
                nest = max(nest - 1, 0)
            elif nest == 0:  # a `;` outside any bracket ends the item before it
                head = at + 1
        pos = m.start()
        if not (stack and stack[-1]):
            body = definition_body(text, m.end())
            found.append((m.group(2), without_variant_names(body) if m.group(1) == 'enum' else body))
    return found


def macro_spans(text):
    """[(start, end, name)] for every `macro_rules! name { .. }` in `text` (the delimiters matched)."""
    spans = []
    for m in MACRO_RULES.finditer(text):
        depth, i, n = 0, m.end(), len(text)
        while i < n:
            c = text[i]
            if c in '{([':
                depth += 1
            elif c in '})]':
                depth -= 1
                if depth == 0:
                    break
            i += 1
        spans.append((m.end(), i, m.group(1)))
    return spans


def macro_label(spans, at):
    """`name!` of the innermost `macro_rules!` whose body holds `at`, else `macro!`."""
    inner = [s for s in spans if s[0] <= at < s[1]]
    return f'{max(inner)[2] if inner else "macro"}!'


def scan(root):
    """([(path, name, type_text, line)] for every `static mut`, {type name: [body, ...]}) over the
    tree. A macro-declared static (`$name`) comes back named `macro!` for its `macro_rules!`."""
    decls, defs = [], {}
    for path in rust_files(root):
        text, _ = blank(path.read_text(encoding='utf-8', errors='replace'))
        rel = path.relative_to(root).as_posix()
        spans = None
        for m in DECL.finditer(text):
            name = m.group(1)
            if name.startswith('$'):
                spans = macro_spans(text) if spans is None else spans
                name = macro_label(spans, m.start())
            decls.append((rel, name, declared_type(text, m.end()), text.count('\n', 0, m.start()) + 1))
        for name, body in definitions(text):
            defs.setdefault(name, []).append(body)
    return decls, defs


def is_plain_std(name):
    return name in PLAIN or PLAIN_NONZERO.fullmatch(name) is not None


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
            elif not is_plain_std(name) and looks_like_a_type(name):
                return True, (f'names `{name}`, which is defined nowhere in the tree and is not a std '
                              f'plain-data type PLAIN lists (plain data? add it to PLAIN, do not allowlist)')
    return False, ''


def heap_statics(root):
    decls, defs = scan(root)
    found = {}
    for rel, name, type_text, line in decls:
        if name.endswith('!'):
            heap, why = True, f'is declared through macro `{name}` (line {line}), so its type is not read'
        else:
            heap, why = owner(type_text, defs)
        if heap:
            found.setdefault((rel, name), (' '.join(type_text.split()), why))
    return found


def main(argv):
    root = Path(argv[1]) if len(argv) > 1 else Path(__file__).resolve().parent.parent
    found = heap_statics(root)
    for (rel, name), (type_text, why) in sorted(found.items()):
        print(f'{rel}\t{name}\t{type_text} — {why}')
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
