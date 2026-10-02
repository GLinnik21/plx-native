#!/usr/bin/env python3
"""The module dependency graph of rust-modules/src, read from Rust tokens.

Every edge is a place where one module NAMES another: a `crate::`/`super::`/`self::`/`$crate::`
path (also as a `#[serde(with = "…")]`-style string), a `use` tree leaf, a bare top-level path
written in the crate root, or an invocation of a `#[macro_export]` macro (bare or
`crate::`-qualified). That is exactly the set of references that
would have to become an `other_crate::` path — and therefore a `[dependencies]` entry — if the two
modules lived in different crates, which is the question this graph exists to answer
(`docs/module-layers.md`). Method calls and trait dispatch name nothing and correctly add no edge:
across crates they resolve through the transitive dependency graph without a direct dependency.

Each reference carries `test`: true when it sits under `cfg(test)` — a `#[cfg(test)]` item, a
test-only module, or a file reachable only through test modules. `Crate.test_items` records the
items that are themselves `cfg(test)` inside a module that is not, and `test_item_refs` the
references that name one of those or a test-only module: what a dependent crate's tests could no
longer see once the two sides are separate crates, since a dependency is never built `cfg(test)`. `cfg` predicates other than
`test` count as possibly-on, so the graph is the UNION of every feature configuration: a cycle in
any configuration is a cycle.

Deliberately lexical, like the other structure gates: no rustc, no rust-analyzer, ~3 s for the
whole tree, and `ci/test_module_graph.py` pins every resolution rule.
"""
from pathlib import Path
import collections
import os
import re

from rust_test_modules import cfg_without_tests

# Comments, string literals and character literals are found first and blanked out of the source
# (a string becomes a `\x00N\x00` placeholder for `Tokens.strings[N]`). The search is for ONE
# character class, so the text between literals is never visited in Python; prefixes (`b"`, `r#"`)
# are read backwards from the quote, nested block comments and escapes forwards from it.
SPECIAL = re.compile(r"[\"'/]")
CHAR = re.compile(r"'(?:\\(?:u\{[^}]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'")
IDENT_CHAR = frozenset('ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_')
# What survives blanking, one line at a time: words, the punctuation the resolver reads, string
# placeholders. Numbers are matched only so their suffixes (`1u32`) cannot read as words; every
# other character is skipped by the search itself.
TOKEN = re.compile(r"[0-9][0-9A-Za-z_]*|([A-Za-z_][A-Za-z_0-9]*|::|[#!\[\](){};,$*=<>]|\x00[0-9]+\x00)")
BLOCK_EDGE = re.compile(r'/\*|\*/')
STRING_RUN = re.compile(r'[^"\\]*')
WORD = re.compile(r'[A-Za-z_][A-Za-z_0-9]*\Z')

STRING = '"'  # the text of every string-literal token; its value is in `Tokens.strings`
# A `{` after one of these opens the item's body, so the item ends where that group closes.
BLOCK_ITEMS = frozenset({'fn', 'mod', 'impl', 'trait', 'struct', 'enum', 'union', 'macro_rules'})
# Words that may precede the keyword naming an item.
QUALIFIERS = frozenset({'pub', 'crate', 'const', 'async', 'unsafe', 'extern', 'default', 'safe'})
# The items a `#[cfg(test)]` can hide that a path can name, recorded in `Crate.test_items`.
NAMED_ITEMS = frozenset({'fn', 'struct', 'enum', 'union', 'trait', 'type', 'const', 'static'})
# The token before a `use` that makes it a use DECLARATION (`impl Trait + use<'a>` is not one).
USE_FOLLOWS = frozenset({';', '{', '}', ']', ')', 'pub', None})
OPENERS = {'(': ')', '[': ']', '{': '}'}
CLOSERS = frozenset(OPENERS.values())
PATH_HEADS = frozenset({'crate', 'super', 'self'})
SERDE_PATH = re.compile(r'(?:crate|super|self)(?:::[A-Za-z_][A-Za-z_0-9]*)+\Z')


def blank(source):
    """`source` with comments and literals removed (newlines kept), and the string values."""
    out, strings, pos, n = [], [], 0, len(source)
    search = SPECIAL.search
    while True:
        m = search(source, pos)
        if m is None: break
        at = m.start()
        c = source[at]
        if c == '/':
            nxt = source[at + 1:at + 2]
            if nxt == '/':
                out.append(source[pos:at])
                end = source.find('\n', at)
                pos = n if end < 0 else end
            elif nxt == '*':
                out.append(source[pos:at])
                depth, end = 1, at + 2
                while depth:
                    edge = BLOCK_EDGE.search(source, end)
                    if edge is None: end = n; break
                    depth += 1 if edge[0] == '/*' else -1
                    end = edge.end()
                out.append('\n' * source.count('\n', at, end))
                pos = end
            else:
                out.append(source[pos:at + 1]); pos = at + 1
            continue
        if c == "'":
            lit = CHAR.match(source, at)
            if lit is None:  # a lifetime or a label
                out.append(source[pos:at + 1]); pos = at + 1
            else:
                begin = at - 1 if at and source[at - 1] == 'b' and (at < 2 or source[at - 2] not in IDENT_CHAR) else at
                out.append(source[pos:begin]); pos = lit.end()
            continue
        # A double quote: a plain, byte or C string, or a raw one whose `r#…` sits just before it.
        k = at
        while k > pos and source[k - 1] == '#': k -= 1
        hashes = at - k
        raw = k > 0 and source[k - 1] == 'r'
        if raw:
            k -= 1
            if k > 0 and source[k - 1] in 'bc': k -= 1
            if k > 0 and source[k - 1] in IDENT_CHAR:  # `r` ends an identifier: not a prefix
                raw, k = False, at
        elif hashes:
            k = at  # stray `#`s before a plain string belong to the code
        if not raw and k > 0 and source[k - 1] in 'bc' and (k < 2 or source[k - 2] not in IDENT_CHAR):
            k -= 1
        out.append(source[pos:k])
        start = at + 1
        if raw:
            close = source.find('"' + '#' * hashes, start)
            if close < 0: raise ValueError('unterminated Rust raw string')
            value, pos = source[start:close], close + 1 + hashes
        else:
            j = start
            while True:
                j = STRING_RUN.match(source, j).end()
                if j >= n or source[j] == '"': break
                j += 2  # a backslash and the character it escapes
            value, pos = re.sub(r'\\([\\"])', r'\1', source[start:j]), j + 1
        out.append(f' \x00{len(strings)}\x00 ' + '\n' * source.count('\n', at, pos))
        strings.append(value)
    out.append(source[pos:])
    return ''.join(out), strings


class Tokens:
    """A file's code tokens: `text[i]`, `lines[i]`, string values by index, group closers."""

    def __init__(self, source):
        cleaned, values = blank(source)
        text, lines, strings = [], [], {}
        findall = TOKEN.findall
        for number, line in enumerate(cleaned.split('\n'), 1):
            found = [t for t in findall(line) if t]
            if not found: continue
            if '\x00' in line:
                for k, t in enumerate(found):
                    if t[0] == '\x00':
                        strings[len(text) + k] = values[int(t[1:-1])]
                        found[k] = STRING
            text.extend(found)
            lines.extend([number] * len(found))
        self.text, self.lines, self.strings = text, lines, strings
        ends, stack = {}, []
        for k, t in enumerate(text):
            if t in OPENERS: stack.append((OPENERS[t], k))
            elif t in CLOSERS and stack:
                want, opened = stack.pop()
                if t == want: ends[opened] = k
                else: stack.clear()
        self.ends = ends

    def line(self, i):
        return self.lines[i]


Ref = collections.namedtuple('Ref', 'source target file line test kind path')
# A `cfg(test)` item of a module that is not test-only. `owner` is None for a module-level item,
# else the type or trait whose impl/trait block holds it (`Type::item` is how a path names it).
TestItem = collections.namedtuple('TestItem', 'module owner name file line')


class Crate:
    """Modules, files and resolved references of one crate rooted at `root/<entry>`."""

    def __init__(self, root, entry='lib.rs'):
        self.root = Path(root).resolve()
        self.modules = {(): {'files': set(), 'test': False}}
        raw = []                   # (module, segments, file, line, test, kind)
        macro_calls = []           # (name, module, file, line, test)
        self._exported = {}        # macro name -> defining module, for `#[macro_export]`
        self._defined = []         # (macro name, defining module) for every `macro_rules!`
        self._macro_use = set()    # modules declared `#[macro_use] mod …;`
        self.test_items = {}       # (module, owner, name) -> TestItem
        done = set()
        queue = collections.deque([(self.root / entry, (), self.root, self.root, False)])
        while queue:
            item = queue.popleft()
            if item[:2] in done: continue
            done.add(item[:2])
            self._walk_file(*item, queue, raw, macro_calls)
        # The macros any module can invoke by bare name: `#[macro_export]` ones, and the ones a
        # `#[macro_use]` module (or its descendant) defines, which are in textual scope after it.
        self.exported_macros = {name: module for name, module in self._defined
                                if any(module[:len(m)] == m for m in self._macro_use)}
        self.exported_macros.update(self._exported)
        # Resolve only now: a path into a module the walk had not declared yet would otherwise
        # stop at that module's parent.
        refs = []
        for module, written, file, line, test, kind in raw:
            segments = self.absolute(written, module)
            if segments is None: continue
            if kind == 'path' and len(segments) == 1 and segments[0] in self.exported_macros:
                target = self.exported_macros[segments[0]]  # `crate::name!` names its definer
            else:
                target = self.resolve(segments)
            refs.append(Ref(module, target, file, line, test, kind, '::'.join(segments)))
        for name, module, file, line, test in macro_calls:
            target = self.exported_macros.get(name)
            if target is not None:
                refs.append(Ref(module, target, file, line, test, 'macro', name + '!'))
        self.refs = refs

    def rel(self, path):
        return Path(os.path.relpath(path, self.root)).as_posix()

    def test_item_refs(self):
        """[(Ref, label, provider module)] for every `cfg(test)` reference that names a test-only
        module or a `test_items` entry: by path (`crate::net::clear()`, `use crate::pms::movie`,
        `crate::pms::HubsSnapshot::empty_for_test()`), or through a `use` of its module — a glob
        (`use crate::gfx::backdrop::*` and a bare `commit`) or the module itself
        (`use crate::plex::session;` and `session::reads_for_test()`), where the file's tokens are
        searched for the module's `cfg(test)` item names. The label is the module (`testlock`) or
        the item. Not seen: a method call, trait dispatch through a `cfg(test)` impl, an associated
        item called through a `use`d type name, and a module imported under another name."""
        module_items = collections.defaultdict(set)  # module -> its module-level cfg(test) names
        for module, owner, item in self.test_items:
            if owner is None: module_items[module].add(item)
        tokens = {}

        def file_tokens(rel):
            if rel not in tokens: tokens[rel] = Tokens((self.root / rel).read_text()).text
            return tokens[rel]

        found = []
        for ref in self.refs:
            if not ref.test: continue
            info = self.modules.get(ref.target)
            if info is not None and info['test']:
                found.append((ref, name(ref.target), ref.target))
                continue
            segments = ref.path.split('::')
            if ref.kind == 'macro' or tuple(segments[:len(ref.target)]) != ref.target: continue
            rest = segments[len(ref.target):]
            if ref.kind == 'use' and rest in ([], ['*']) and ref.target in module_items:
                text = file_tokens(ref.file)
                if rest:
                    named = module_items[ref.target] & set(text)
                else:
                    alias = ref.target[-1]
                    named = {text[k + 2] for k in range(len(text) - 2)
                             if text[k] == alias and text[k + 1] == '::' and text[k + 2] in module_items[ref.target]}
                found.extend((ref, f'{name(ref.target)}::{item}', ref.target) for item in sorted(named))
                continue
            keys = [((ref.target, None, rest[0]), 1)] if rest else []
            if len(rest) > 1: keys.append(((ref.target, rest[0], rest[1]), 2))
            for key, used in keys:
                if key in self.test_items:
                    found.append((ref, '::'.join(segments[:len(ref.target) + used]), ref.target))
                    break
        return found

    def _declare(self, module, test):
        info = self.modules.get(module)
        if info is None:
            self.modules[module] = info = {'files': set(), 'test': test}
        else:
            info['test'] = info['test'] and test
        return info

    def resolve(self, segments):
        """The deepest declared module an absolute path (segments from the crate root) names."""
        best = ()
        for k in range(1, len(segments) + 1):
            if tuple(segments[:k]) in self.modules: best = tuple(segments[:k])
            else: break
        return best

    def absolute(self, segments, module):
        """Absolute segments for a path written in `module`, or None when it leaves the crate."""
        head = segments[0]
        if head == 'crate' or head == '$crate':
            return list(segments[1:])
        if head == 'self':
            return list(module) + list(segments[1:])
        if head == 'super':
            base, rest = list(module), list(segments)
            while rest and rest[0] == 'super':
                if not base: raise ValueError(f'`super` above the crate root in {module}')
                base.pop(); rest.pop(0)
            if rest and rest[0] == 'self': rest.pop(0)
            return base + rest
        if module == () and (head,) in self.modules:
            return list(segments)
        return None

    def _walk_file(self, path, module, directory, attribute_base, file_test, queue, raw, macro_calls):
        rel = self.rel(path)
        tok = Tokens(path.read_text())
        text, ends, strings = tok.text, tok.ends, tok.strings
        n = len(text)
        self._declare(module, file_test)['files'].add(rel)

        def at(i):
            return text[i] if 0 <= i < n else None

        def group_end(i):
            end = ends.get(i)
            if end is None: raise ValueError(f'{rel}:{tok.line(i)}: unbalanced Rust token group')
            return end

        def skip_attributes(j):
            while at(j) == '#' and at(j + 1) == '[':
                j = group_end(j + 1) + 1
            return j

        def item_start(j):
            """The first token after any further attributes and a visibility."""
            j = skip_attributes(j)
            if at(j) == 'pub':
                j += 1
                if at(j) == '(': j = group_end(j) + 1
            return j

        def item_end(j, limit):
            """Index of the last token of the item (or statement, field, arm) starting at `j`."""
            j = skip_attributes(j)
            block, decided, k = False, None, j
            while k < limit:
                t = text[k]
                if t == '(' or t == '[': k = group_end(k) + 1; continue
                if t == '{':
                    if block or decided is None: return group_end(k)
                    k = group_end(k) + 1; continue
                # A macro-invocation item (`thread_local! { … }`) ends with its brace group, and
                # takes no `;` after it.
                if t == '!' and decided == k - 1 and at(k + 1) == '{': return group_end(k + 1)
                if t == ';' or t == ',': return k
                if t in CLOSERS: return k - 1
                if decided is None and t not in QUALIFIERS and WORD.match(t):
                    decided, block = k, t in BLOCK_ITEMS
                k += 1
            return limit - 1

        def use_leaves(start, stop):
            seq, leaves = [], []
            k = start
            while k < stop:
                if text[k] == '$' and k + 1 < stop and text[k + 1] == 'crate':
                    seq.append('$crate'); k += 2
                else:
                    seq.append(text[k]); k += 1

            def tree(pos, prefix):
                path = list(prefix)
                if pos < len(seq) and seq[pos] == '::': pos += 1  # `::name` is an extern crate
                while pos < len(seq):
                    t = seq[pos]
                    if t == '{':
                        pos += 1
                        while pos < len(seq) and seq[pos] != '}':
                            pos = tree(pos, path)
                            if pos < len(seq) and seq[pos] == ',': pos += 1
                        return pos + 1
                    if t == '*':
                        leaves.append(path + ['*']); return pos + 1  # resolves to `path`'s module
                    if t == ',' or t == '}':
                        leaves.append(path); return pos
                    if t == 'as':
                        leaves.append(path); return pos + 2
                    if t == '::' or (t == 'self' and path):  # `a::{self}` names `a`
                        pos += 1; continue
                    path.append(t); pos += 1
                leaves.append(path)
                return pos
            tree(0, [])
            return [leaf for leaf in leaves if leaf]

        enclosing = []  # the innermost `{` holding each token, built on first use

        def holder(i):
            if not enclosing:
                stack = []
                for k, t in enumerate(text):
                    if t == '}' and stack: stack.pop()
                    enclosing.append(stack[-1] if stack else None)
                    if t == '{': stack.append(k)
            return enclosing[i]

        def block_owner(o):
            """What the `{` at `o` opens: ('mod', None) for an inline module, ('impl', name) for an
            impl or trait body (the self type, or the trait), else (None, None) — a fn body, a
            struct's fields, an expression."""
            j = o - 1
            while j >= 0 and text[j] not in (';', '}', '{'): j -= 1
            head = text[j + 1:o]
            for k, t in enumerate(head):
                if t == 'mod': return 'mod', None
                if t in ('fn', 'struct', 'enum', 'union'): return None, None
                if t == 'trait': return 'impl', head[k + 1] if k + 1 < len(head) else None
                if t == 'impl':
                    tail = head[k + 1:]
                    if tail[:1] == ['<']:
                        depth = 0
                        for m, u in enumerate(tail):
                            depth += (u == '<') - (u == '>')
                            if depth == 0: tail = tail[m + 1:]; break
                    if 'for' in tail: tail = tail[tail.index('for') + 1:]
                    owner = None
                    for u in tail:
                        if u in ('<', 'where'): break
                        if WORD.match(u) and u not in ('dyn', 'mut', 'crate', 'super', 'self'): owner = u
                    return 'impl', owner
            return None, None

        def note_test_item(j, mod):
            """Record the `cfg(test)` item starting at `j` in a module that is not test-only."""
            o = holder(j)
            kind, owner = ('mod', None) if o is None else block_owner(o)
            if kind is None: return
            k = item_start(j)
            while True:
                t = at(k)
                if t in ('async', 'unsafe', 'default', 'safe'): k += 1
                elif t == 'const' and at(k + 1) in ('fn', 'unsafe', 'async', 'extern'): k += 1
                elif t == 'extern': k += 2 if at(k + 1) == STRING else 1
                else: break
            t = at(k)
            if t == 'static' and at(k + 1) == 'mut': k += 1
            if t in NAMED_ITEMS and k + 1 < n and WORD.match(text[k + 1]):
                key = (mod, owner, text[k + 1])
                self.test_items.setdefault(key, TestItem(mod, owner, text[k + 1], rel, tok.line(k)))
            elif t == 'impl' and owner is None:
                # A whole `#[cfg(test)] impl` block: each item directly in its body.
                body = k
                while body < n and text[body] != '{':
                    body = group_end(body) + 1 if text[body] in ('(', '[') else body + 1
                if body >= n: return
                _, impl_owner = block_owner(body)
                m, stop_at = body + 1, group_end(body)
                while m < stop_at:
                    u = text[m]
                    if u in OPENERS: m = group_end(m) + 1; continue
                    if u in ('fn', 'const', 'type') and m + 1 < stop_at and WORD.match(text[m + 1]):
                        key = (mod, impl_owner, text[m + 1])
                        self.test_items.setdefault(key, TestItem(mod, impl_owner, text[m + 1], rel, tok.line(m)))
                    m += 1

        # Only these tokens can start anything the walk records; everything else is stepped over
        # in bulk. In the crate root ANY word may head a path (a top-level module is in scope
        # there by name); `absolute` keeps the ones that name a module once the walk is done.
        root = module == ()
        interesting = {'#', 'mod', 'include', 'macro_rules', 'use', '!', '$'} | PATH_HEADS

        # Scopes: (last token index, module, directory, attribute base, test). Items nest, so a
        # stack popped by position is exact.
        scopes = [(n, module, directory, attribute_base, file_test)]
        path_attr = {}    # index of a `mod` keyword -> its `#[path]` value
        exported = set()  # index of a `macro_rules` keyword under `#[macro_export]`
        macro_use = set() # index of a `mod` keyword under `#[macro_use]`
        i = 0
        while i < n:
            t = text[i]
            if t not in interesting and not root: i += 1; continue
            while scopes[-1][0] < i: scopes.pop()
            _, mod, mod_dir, attr_base, test = scopes[-1]

            if t == '#' and (at(i + 1) == '[' or (at(i + 1) == '!' and at(i + 2) == '[')):
                inner = text[i + 1] == '!'
                open_at = i + 2 if inner else i + 1
                stop = group_end(open_at)
                attr = text[open_at + 1:stop]
                if attr[:2] == ['cfg', '('] and cfg_without_tests(
                        [('code', w) for w in attr[2:-1]]) == {False}:
                    if inner:
                        end = scopes[-1][0]
                        scopes.append((end, mod, mod_dir, attr_base, True))
                        if end == n: self._declare(mod, True)
                    else:
                        if not test: note_test_item(stop + 1, mod)
                        scopes.append((item_end(stop + 1, scopes[-1][0]), mod, mod_dir, attr_base, True))
                elif not inner and len(attr) == 3 and attr[:2] == ['path', '='] and attr[2] == STRING:
                    k = item_start(stop + 1)
                    if at(k) == 'mod': path_attr[k] = strings[open_at + 3]
                elif not inner and attr == ['macro_export']:
                    k = item_start(stop + 1)
                    if at(k) == 'macro_rules': exported.add(k)
                elif not inner and attr == ['macro_use']:
                    k = item_start(stop + 1)
                    if at(k) == 'mod': macro_use.add(k)
                elif attr[:1] == ['serde']:
                    # `with = "crate::x::codec"` and friends are paths the derive's generated code
                    # calls, spelled as strings. A relative one names something already in scope.
                    for k in range(open_at + 1, stop):
                        value = strings.get(k, '')
                        if text[k] == STRING and SERDE_PATH.match(value):
                            raw.append((mod, value.split('::'), rel, tok.line(k), test, 'path'))
                i = stop + 1; continue

            if t == 'mod' and i + 2 < n and WORD.match(text[i + 1]) and text[i + 2] in (';', '{'):
                name = text[i + 1]
                child = mod + (name,)
                self._declare(child, test)
                if i in macro_use: self._macro_use.add(child)
                explicit = path_attr.get(i)
                if text[i + 2] == ';':
                    if explicit:
                        candidates = [(attr_base / explicit, None)]
                    else:
                        candidates = [(mod_dir / (name + '.rs'), mod_dir / name),
                                      (mod_dir / name / 'mod.rs', mod_dir / name)]
                    for target, child_dir in candidates:
                        if target.is_file():
                            target = target.resolve()
                            if child_dir is None:
                                child_dir = target.parent if target.name == 'mod.rs' else target.with_suffix('')
                            queue.append((target, child, child_dir, target.parent, test))
                            break
                    else:
                        raise ValueError(f'{rel}:{tok.line(i)}: no file for `mod {name};`')
                    i += 3; continue
                stop = group_end(i + 2)
                nested = attr_base / explicit if explicit else mod_dir / name
                scopes.append((stop, child, nested, nested, test))
                self.modules[child]['files'].add(rel)
                i += 3; continue

            if t == 'include' and at(i + 1) == '!' and at(i + 2) == '(':
                stop = group_end(i + 2)
                if stop == i + 4 and text[i + 3] == STRING:
                    target = path.parent / strings[i + 3]
                    if target.suffix == '.rs' and target.is_file():
                        queue.append((target.resolve(), mod, mod_dir, attr_base, test))
                i = stop + 1; continue

            if t == 'macro_rules' and at(i + 1) == '!' and i + 2 < n and WORD.match(text[i + 2]):
                if i in exported: self._exported[text[i + 2]] = mod
                self._defined.append((text[i + 2], mod))
                i += 3; continue

            prev = text[i - 1] if i else None
            if t == 'use' and prev in USE_FOLLOWS:
                stop = i + 1
                while stop < n and text[stop] != ';':
                    stop = group_end(stop) + 1 if text[stop] == '{' else stop + 1
                for leaf in use_leaves(i + 1, stop):
                    raw.append((mod, leaf, rel, tok.line(i), test, 'use'))
                i = stop + 1; continue

            if t == '$' and at(i + 1) == 'crate' and at(i + 2) == '::' and prev != '::':
                t, i = '$crate', i + 1
            if (t in PATH_HEADS or t == '$crate' or (root and WORD.match(t))) and at(i + 1) == '::' and prev != '::':
                segments, k = [t], i + 1
                while k + 1 < n and text[k] == '::' and WORD.match(text[k + 1]):
                    segments.append(text[k + 1]); k += 2
                raw.append((mod, segments, rel, tok.line(i), test, 'path'))
                i = k; continue

            if t == '!' and i and WORD.match(text[i - 1]) and at(i + 1) in OPENERS and at(i - 2) != '::':
                macro_calls.append((text[i - 1], mod, rel, tok.line(i - 1), test))
            i += 1


def name(module):
    return '::'.join(module) if module else 'crate'


def sccs(nodes, adjacency):
    """Tarjan's strongly connected components, iteratively; sorted members, sorted components."""
    index, low, on_stack, stack, out = {}, {}, set(), [], []
    for root in sorted(nodes):
        if root in index: continue
        index[root] = low[root] = len(index)
        stack.append(root); on_stack.add(root)
        work = [(root, iter(sorted(adjacency.get(root, ()))))]
        while work:
            node, successors = work[-1]
            for nxt in successors:
                if nxt not in index:
                    index[nxt] = low[nxt] = len(index)
                    stack.append(nxt); on_stack.add(nxt)
                    work.append((nxt, iter(sorted(adjacency.get(nxt, ())))))
                    break
                if nxt in on_stack: low[node] = min(low[node], index[nxt])
            else:
                work.pop()
                if work: low[work[-1][0]] = min(low[work[-1][0]], low[node])
                if low[node] == index[node]:
                    comp = []
                    while True:
                        x = stack.pop(); on_stack.discard(x); comp.append(x)
                        if x == node: break
                    out.append(sorted(comp))
    return sorted(out)


def module_edges(crate, depth=1, include_test=False):
    """{(from, to): [Ref...]} between distinct module prefixes of length `depth` (names)."""
    result = collections.defaultdict(list)
    for ref in crate.refs:
        if ref.test and not include_test: continue
        a, b = ref.source[:depth], ref.target[:depth]
        if a == b or (a and b and (a[:len(b)] == b or b[:len(a)] == a)):
            continue  # within one module, or a module naming its own ancestor/descendant
        result[(name(a), name(b))].append(ref)
    return result


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--src', type=Path, default=Path(__file__).resolve().parent.parent / 'rust-modules' / 'src')
    parser.add_argument('--depth', type=int, default=1, help='module path length to group by (default 1)')
    parser.add_argument('--test', action='store_true', help='include cfg(test) references')
    parser.add_argument('--edges', action='store_true', help='print every edge with its reference count')
    args = parser.parse_args()
    crate = Crate(args.src)
    graph = module_edges(crate, args.depth, args.test)
    if args.edges:
        for (a, b), refs in sorted(graph.items()):
            print(f'{a} -> {b}\t{len(refs)}')
    adjacency, nodes = collections.defaultdict(set), set()
    for a, b in graph:
        adjacency[a].add(b); nodes |= {a, b}
    cycles = [c for c in sccs(nodes, adjacency) if len(c) > 1]
    for comp in cycles:
        print(f'cycle of {len(comp)}: {" ".join(comp)}')
    if not cycles:
        print('no cycles')
