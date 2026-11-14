"""Bounded structural proof for shared reads of a registered owned state.

Only immutable standard containers are axioms. Local fields and aliases are followed through
module imports; opaque/ambiguous types, callbacks, raw pointers and interior authority fail closed.
Safe Rust then prevents an &self method from changing the proved owned state.
"""
from functools import lru_cache
from pathlib import Path
import re

PRIMITIVES = set('bool char str u8 u16 u32 u64 u128 usize i8 i16 i32 i64 i128 isize f32 f64'.split())
WRAPPERS = {'Vec', 'Option', 'Arc', 'String', 'VecDeque', 'BTreeMap', 'BTreeSet', 'Result', 'PathBuf'}
STD = {'std::vec::Vec', 'alloc::vec::Vec', 'std::option::Option', 'core::option::Option',
       'std::sync::Arc', 'alloc::sync::Arc', 'std::string::String', 'alloc::string::String',
       'std::collections::VecDeque', 'std::collections::BTreeMap', 'std::collections::BTreeSet', 'std::path::PathBuf',
       'std::result::Result', 'core::result::Result'}

def cache_proof(method):
    """Memoize one immutable Proof snapshot, not repository authority or a writer exemption."""
    def freeze(value):
        if isinstance(value, (list, tuple)): return tuple(freeze(v) for v in value)
        if isinstance(value, dict): return tuple(sorted((k, freeze(v)) for k, v in value.items()))
        if isinstance(value, set): return frozenset(freeze(v) for v in value)
        return value
    def cached(self, *args, **kwargs):
        if not hasattr(self, 'memo'): self.memo = {}
        key = method.__name__, freeze(args), freeze(kwargs)
        if key not in self.memo: self.memo[key] = method(self, *args, **kwargs)
        return self.memo[key]
    return cached

class Tokens(list):
    """A token list that is built once and never changed, so one lexed source can be shared and can
    remember what is derived from it (`derived`) instead of every consumer recomputing it.

    Every mutator raises: a caller that needs a changeable copy writes `list(tokens)`."""
    def __init__(self, tokens=()):
        super().__init__(tokens)
        self.derived = {}

    def _frozen(self, *args, **kwargs):
        raise TypeError('Tokens are shared and read-only; copy with list(tokens)')
    __setitem__ = __delitem__ = __iadd__ = __imul__ = _frozen
    append = extend = insert = pop = remove = clear = reverse = sort = _frozen


def derived(tokens, key, compute):
    """compute(tokens), remembered on a shared `Tokens` list; a plain list is computed every time."""
    if not isinstance(tokens, Tokens): return compute(tokens)
    if key not in tokens.derived: tokens.derived[key] = compute(tokens)
    return tokens.derived[key]


class Shadows:
    """What in one token list can shadow a builtin macro or the `std` crate: its macro definitions,
    every token of its `use` items and whether `mod std` / `as std` appear. One pass over the list
    answers what a rescan at every macro call used to."""
    def __init__(self, tokens):
        self.macro_rules, self.macro_items, self.imported, self.std_named = set(), set(), set(), False
        count = len(tokens)
        for j, token in enumerate(tokens):
            if token == 'macro_rules' and j + 2 < count and tokens[j + 1] == '!': self.macro_rules.add(tokens[j + 2])
            elif token == 'macro' and j + 2 < count: self.macro_items.add(tokens[j + 1])
            elif token == 'use':
                end = j + 1
                while end < count and tokens[end] != ';': end += 1
                self.imported.update(tokens[j + 1:end])
            elif token in ('mod', 'as') and j + 1 < count and tokens[j + 1] == 'std': self.std_named = True


def shadows_of(tokens):
    return derived(tokens, 'shadows', Shadows)


@lru_cache(maxsize=64)
def lexed(source):
    """(raw lexer tokens, their bracket ends) of one source TEXT, shared and never changed: the module
    walk and each child lookup read the same file's text once per check, so this lexes it once."""
    import rust_test_modules as rtm
    tokens = rtm.lex(source)
    return Tokens(tokens), rtm.group_ends(tokens)


def macro_invocation(tokens, bang, ends):
    """A bang after an identifier is a macro, unlike logical negation after if/return."""
    return (bang > 0 and bang + 1 in ends and tokens[bang - 1].isidentifier() and
            tokens[bang - 1] not in {'if', 'while', 'for', 'return', 'match', 'let', 'else', 'yield', 'break', 'continue'})

def split(tokens, delimiter=','):
    result, current, depth = [], [], 0
    for index, token in enumerate(tokens):
        if token == delimiter and not depth:
            result.append(current); current = []; continue
        depth += (token in '([{<') - (token in ')]}>' and not (token == '>' and index and tokens[index - 1] == '-'))
        current.append(token)
    if current: result.append(current)
    return result

def imports(tokens, prefix=()):
    if '{' in tokens:
        start = tokens.index('{'); depth, stop = 1, start + 1
        while depth and stop < len(tokens):
            depth += (tokens[stop] == '{') - (tokens[stop] == '}'); stop += 1
        base = tuple(t for t in tokens[:start] if t != ':')
        return [entry for item in split(tokens[start + 1:stop - 1]) for entry in imports(item, prefix + base)]
    if 'as' in tokens:
        at = tokens.index('as'); alias = tokens[at + 1]; tokens = tokens[:at]
    else: alias = (prefix[-1] if prefix else '') if tokens == ['self'] else tokens[-1] if tokens else ''
    target = prefix if tokens == ['self'] else prefix + tuple(t for t in tokens if t != ':')
    return [(alias, target)]


class Proof:
    def __init__(self, root, shape):
        self.root, self.shape = Path(root), shape
        self.cache, self.visiting, self.audit = {}, set(), set()
        self.real_paths = {}
        self.real_root = self.real(self.root)
        self.crates = {}
        for manifest in self.root.glob('*/Cargo.toml'):
            text = manifest.read_text(encoding='utf-8')
            sections = dict(re.findall(r'(?ms)^\[([^\]]+)\]\s*$(.*?)(?=^\[|\Z)', text))
            match = re.search(r'(?m)^name\s*=\s*"([A-Za-z0-9_-]+)"', sections.get('lib', ''))
            if match is None: match = re.search(r'(?m)^name\s*=\s*"([A-Za-z0-9_-]+)"', sections.get('package', ''))
            name = match[1].replace('-', '_') if match else ''
            if name: self.crates[name] = manifest.parent / 'src/lib.rs'

    def real(self, path):
        """Path(path).resolve(), looked up once per path for this proof's lifetime: the tree is not
        changed while a proof is being made (module() already memoizes every file it reads)."""
        if path not in self.real_paths: self.real_paths[path] = Path(path).resolve()
        return self.real_paths[path]

    def module(self, path):
        path = self.real(path)
        if path in self.cache: return self.cache[path]
        from readonly_generated import declarations
        generated = declarations(self.root, path)
        text = path.read_text(encoding='utf-8')
        # With nothing generated the appended newline lexes to no token; leaving it off lets the module
        # share the memoized tokens of the same text the checker's own walk already lexed.
        tokens = self.shape.production_tokens(text + '\n' + generated if generated else text)
        if generated: self.audit.add('source-backed generated data ' + path.relative_to(self.real_root).as_posix())
        ends = self.shape.ends_of(tokens)
        reverse = {end: begin for begin, end in ends.items()}
        definitions, uses = {}, []
        def field_type(index, item):
            remaining = item
            while remaining[:2] == ['#', '[']:
                groups = self.shape.ends_of(remaining)
                stop = groups.get(1)
                if stop is None or remaining[2:3] not in (['serde'], ['doc'], ['cfg'], ['allow'], ['deny'], ['forbid']): return ['<unproved-attribute>']
                remaining = remaining[stop + 1:]
            return self.shape.shape(index, item)[2]
        for i, token in enumerate(tokens[:-1]):
            if token == 'use':
                end = i + 1
                while end < len(tokens) and tokens[end] != ';': end += 1
                uses.extend(imports(tokens[i + 1:end]))
            if token not in ('struct', 'enum', 'type'): continue
            name, j = tokens[i + 1], i + 2
            parameters = []
            if tokens[j:j + 1] == ['<']:
                depth, stop = 1, j + 1
                while depth and stop < len(tokens):
                    depth += (tokens[stop] == '<') - (tokens[stop] == '>'); stop += 1
                parameters = [part[0] for part in split(tokens[j + 1:stop - 1]) if part and part[0] != "'"]
                j = stop
            if token == 'type':
                if tokens[j:j + 1] != ['=']: continue
                stop = j + 1
                while stop < len(tokens) and tokens[stop] != ';': stop += 1
                payload = [tokens[j + 1:stop]]
            else:
                if tokens[j:j + 1] == [';']: payload = []
                elif j not in ends or tokens[j] not in ('{', '('): continue
                elif token == 'struct': payload = [field_type(k, item) for k, item in enumerate(split(tokens[j + 1:ends[j]]))]
                else:
                    payload = []
                    for variant in split(tokens[j + 1:ends[j]]):
                        while variant[:2] == ['#', '[']:
                            stop = variant.index(']'); variant = variant[stop + 1:]
                        if len(variant) > 1 and variant[1] in ('(', '{'):
                            items = split(variant[2:-1])
                            payload.extend(field_type(k, item) for k, item in enumerate(items))
            # Attribute macros (including conditional ones) can rewrite the original layout.
            start = i
            if start and tokens[start - 1] == 'pub': start -= 1
            elif start and tokens[start - 1] == ')' and start - 1 in reverse:
                begin = reverse[start - 1]
                if begin and tokens[begin - 1] == 'pub': start = begin - 1
            attributes = []
            while start and tokens[start - 1] == ']' and start - 1 in reverse:
                begin = reverse[start - 1]
                if not begin or tokens[begin - 1] != '#': break
                attributes.append(tokens[begin + 1:start - 1])
                start = begin - 1
            for attribute in attributes:
                if attribute[0] == 'serde':
                    derives = [a for a in attributes if a[0] == 'derive']
                    serde_derive = any('serde' in a and any(n in a for n in ('Serialize', 'Deserialize')) for a in derives)
                    serde_derive |= any(any(n in a for n in ('Serialize', 'Deserialize')) and
                        any(alias in a and target in (('serde', 'Serialize'), ('serde', 'Deserialize')) for alias, target in uses) for a in derives)
                    if serde_derive: continue
                if attribute[0] not in ('derive', 'repr', 'cfg', 'doc', 'allow', 'deny', 'forbid', 'non_exhaustive'):
                    payload = [['<unproved-attribute>']]
            definitions.setdefault(name, []).append((parameters, payload, token))
        self.cache[path] = (tokens, ends, definitions, uses)
        return self.cache[path]








    @cache_proof
    def child(self, path, name):
        """Resolve one declared child, including a literal path attribute; conditional paths fail."""
        import rust_test_modules as rtm
        path = self.real(path)
        tokens, ends = lexed(path.read_text(encoding='utf-8'))
        reverse = {b: a for a, b in ends.items()}
        mods = derived(tokens, 'mods', lambda t: [i for i in range(len(t) - 2) if t[i] == ('code', 'mod')])
        matches = [i for i in mods if tokens[i:i + 3] == [('code', 'mod'), ('code', name), ('code', ';')]]
        if len(matches) != 1: return None
        before = matches[0] - 1
        if tokens[before:before + 1] == [('code', 'pub')]: before -= 1
        paths = []
        while before in reverse and tokens[before] == ('code', ']'):
            begin = reverse[before]
            if tokens[begin - 1:begin] != [('code', '#')]: break
            attr = tokens[begin + 1:before]
            if attr[:1] == [('code', 'cfg_attr')] and ('code', 'path') in attr: return None
            if attr[:1] == [('code', 'path')]:
                if len(attr) != 3 or attr[1] != ('code', '=') or attr[2][0] != 'string': return None
                paths.append(attr[2][1])
            if attr[:1] == [('code', 'cfg')] and rtm.cfg_without_tests(attr[2:-1]) == {False}: return None
            before = begin - 2
        directory = path.parent if path.name in ('lib.rs', 'mod.rs', 'main.rs') else path.with_suffix('')
        candidates = [path.parent / paths[0]] if len(paths) == 1 else [directory / (name + '.rs'), directory / name / 'mod.rs'] if not paths else []
        found = [p.resolve() for p in candidates if p.is_file() and p.resolve().is_relative_to(self.real_root)]
        if len(found) != 1: return None
        from readonly_generated import canonical_child
        if not canonical_child(self.root, path, name, found[0]): return None
        if not hasattr(self, 'parent_paths'): self.parent_paths = {}
        self.parent_paths[found[0]] = path
        return found[0]

    def parent(self, path):
        path = Path(path).resolve()
        if path in getattr(self, 'parent_paths', {}): return self.parent_paths[path]
        directory = path.parent if path.name not in ('lib.rs', 'mod.rs', 'main.rs') else path.parent.parent
        candidates = [directory.with_suffix('.rs'), directory / 'mod.rs']
        if directory.name == 'src': candidates = [directory / 'lib.rs', directory / 'main.rs']
        return next((p.resolve() for p in candidates if p.is_file() and p.resolve() != path.resolve()), None)

    @cache_proof
    def resolve(self, path, names, seen=None):
        path, names = self.real(path), tuple(names)
        key = (path, names)
        seen = set() if seen is None else seen
        if key in seen: return None
        seen = seen | {key}
        if not names: return None
        if names[0] in ('std', 'core', 'alloc'):
            local = self.module(path)
            if any(local[0][i:i + 2] in (['mod', names[0]], ['as', names[0]]) for i in range(len(local[0]) - 1)): return None
            return ('std', names[-1]) if '::'.join(names) in STD else None
        if names[0] in self.crates:
            local = self.module(path)
            if names[0] in local[2] or any(alias == names[0] for alias, _ in local[3]) or any(local[0][i:i + 2] in (['mod', names[0]], ['crate', names[0]]) for i in range(len(local[0]) - 1)): return None
            return self.resolve(self.crates[names[0]], names[1:], seen)
        if names[0] == 'self': return self.resolve(path, names[1:], seen)
        if names[0] == 'super':
            parent = self.parent(path)
            return self.resolve(parent, names[1:], seen) if parent else None
        if names[0] == 'crate':
            for ancestor in path.parents:
                if (ancestor / 'Cargo.toml').is_file():
                    return self.resolve(ancestor / 'src/lib.rs', names[1:], seen)
            return None
        tokens, ends, definitions, uses = self.module(path)
        if len(names) == 1 and names[0] in definitions:
            if len(definitions[names[0]]) != 1: return None
            return (path, names[0])
        explicit = [target for alias, target in uses if alias == names[0]]
        if len(explicit) == 1: return self.resolve(path, explicit[0] + names[1:], seen)
        if len(explicit) > 1: return None
        if len(names) > 1:
            child = self.child(path, names[0])
            if child is not None: return self.resolve(child, names[1:], seen)
        found = [self.resolve(path, target[:-1] + names, seen) for alias, target in uses if alias == '*']
        found = {value for value in found if value is not None}
        if len(found) == 1: return found.pop()
        if len(names) == 1 and names[0] in PRIMITIVES | {'Vec', 'Option', 'String', 'Result'}:
            return ('std', names[0])
        return None

    def generic_context(self, path, name):
        """Prove each machine parameter through a closed, private sealing module, not Copy alone."""
        tokens, ends, _, uses = self.module(path)
        start = next((i + 2 for i in range(len(tokens) - 2)
                      if tokens[i:i + 2] == ['struct', name]), None)
        if start is None or tokens[start:start + 1] != ['<']: return {}
        stop, depth = start + 1, 1
        while stop < len(tokens) and depth:
            depth += (tokens[stop] == '<') - (tokens[stop] == '>'); stop += 1
        result = {}
        for parameter in split(tokens[start + 1:stop - 1]):
            if '=' in parameter:
                default = parameter[parameter.index('=') + 1:]
                if not self.inert(path, default): return None
                parameter = parameter[:parameter.index('=')]
            if len(parameter) != 3 or parameter[1] != ':': return None
            generic, _, bound = parameter
            if not self.closed_bound(path, bound): return None
            result[generic] = ['u64']  # proof certificate: no mutable authority, not a Rust substitution
        return result

    def closed_bound(self, path, bound):
        tokens, ends, _, uses = self.module(path)
        matches = [i for i in range(len(tokens) - 1) if tokens[i:i + 2] == ['trait', bound]]
        if len(matches) != 1 or any(alias in (bound, '*') for alias, _ in uses): return False
        at = matches[0]
        # The empty public bound has no associated types, callbacks or defaults.
        signature = tokens[at + 2:at + 12]
        if len(signature) != 10 or signature[:5] != [':', 'Copy', '+', 'Ord', '+'] or signature[6:8] != [':', ':'] or signature[9] != '{': return False
        module, seal = signature[5], signature[8]
        before = at - 2 if tokens[at - 1:at] == ['pub'] else at - 1
        def outer_attribute(index):
            opening = next((a for a, b in ends.items() if b == index and tokens[a] == '['), None)
            return opening is not None and tokens[opening - 1:opening] == ['#']
        if outer_attribute(before): return False
        if sum(tokens[i:i + 4] == [module, ':', ':', seal] for i in range(len(tokens) - 3)) != 1: return False
        if ends.get(at + 11) != at + 12: return False
        mods = [i for i in range(len(tokens) - 2) if tokens[i:i + 3] == ['mod', module, '{']]
        if len(mods) != 1: return False
        begin = mods[0]
        if begin and (tokens[begin - 1] in ('pub', ')') or outer_attribute(begin - 1)): return False
        if any(a < begin < b for a, b in ends.items() if tokens[a] == '{'): return False
        # Additional modules/includes or top-level macros could hide another sealing impl.
        if any(t == 'mod' and i != begin or t == 'include' for i, t in enumerate(tokens)): return False
        for i, t in enumerate(tokens):
            if t == '!' and macro_invocation(tokens, i, ends):
                if (tokens[i - 1] not in ('matches', 'assert', 'debug_assert', 'vec', 'format', 'format_args', 'write', 'writeln', 'panic')
                        or tokens[max(0, i - 3):i - 1] == [':', ':']
                        or not self.builtin(path, tokens[i - 1])): return False
            if tokens[i:i + 2] == ['derive', '('] and i + 1 in ends:
                if any(t not in ('Clone', 'Copy', 'Debug', 'PartialEq', 'Eq', 'PartialOrd', 'Ord', 'Default', ',') for t in tokens[i + 2:ends[i + 1]]): return False
        pos, end = begin + 3, ends[begin + 2]
        if tokens[pos:pos + 5] != ['pub', 'trait', seal, '{', '}']: return False
        pos += 5
        count = 0
        while pos < end:
            if tokens[pos:pos + 3] != ['impl', seal, 'for']: return False
            body = pos + 3
            while body < end and tokens[body] != '{': body += 1
            if body >= end or ends[body] != body + 1: return False
            target = tokens[pos + 3:body]
            # Resolve from the inline module's lexical parent; no generics, aliases or blanket impls.
            if target[:3] == ['super', ':', ':']: target = target[3:]
            if '<' in target or not self.inert(path, target): return False
            count += 1
            pos = body + 2
        return count > 0

    def inert(self, path, tokens, substitutions=None):
        # Cache only completed roots: recursive coinductive successes are provisional.
        substitutions = substitutions or {}
        concrete = tuple(part for token in tokens for part in substitutions.get(token, [token]))
        key = (self.real(path), concrete)
        if self.visiting: return self._inert(path, list(concrete))
        if not hasattr(self, 'inert_roots'): self.inert_roots = {}
        if key not in self.inert_roots:
            self.inert_roots[key] = self._inert(path, list(concrete))
        return self.inert_roots[key]

    def _inert(self, path, tokens, substitutions=None):
        substitutions = substitutions or {}
        tokens = [part for token in tokens for part in substitutions.get(token, [token])]
        if not tokens: return True
        if any(t in tokens for t in ('mut', '*', 'unsafe', 'dyn', 'fn', 'impl', '!', '=')): return False
        if tokens[0] == '&':
            tokens = tokens[1:]
            if tokens[:1] == ["'"]: tokens = tokens[2:]
            return self.inert(path, tokens, substitutions)
        if tokens[0] == '(' and tokens[-1] == ')': return all(self.inert(path, t, substitutions) for t in split(tokens[1:-1]))
        if tokens[0] == '[' and tokens[-1] == ']': return self.inert(path, split(tokens[1:-1], ';')[0], substitutions)
        if '<' in tokens:
            start = tokens.index('<')
            if tokens[-1] != '>': return False
            names = [t for t in tokens[:start] if t != ':']
            args = [arg for arg in split(tokens[start + 1:-1]) if arg and arg[0] != "'"]
        else: names, args = [t for t in tokens if t != ':'], []
        target = self.resolve(path, names)
        if target is None:
            # Foreign names stay opaque. Only the independently source-backed locked certificate
            # may prove an absolute dependency type; missing certificate support fails closed.
            if tokens[:2] != [':', ':']: return False
            if not self.dependency_certificate(path, tokens): return False
            self.audit.add('locked source certificate ' + ' '.join(tokens))
            return True
        owner, name = target
        if owner == 'std':
            return (name in PRIMITIVES and not args or name in ('String', 'PathBuf') and not args
                    or name in WRAPPERS - {'String', 'PathBuf'} and len(args) == (2 if name in ('Result', 'BTreeMap') else 1)
                    and all(self.inert(path, arg, substitutions) for arg in args))
        if not all(self.inert(path, arg, substitutions) for arg in args): return False
        parameters, payloads, kind = self.module(owner)[2][name][0]
        if len(parameters) != len(args): return False
        key = (owner, name, tuple(tuple(a) for a in args))
        if key in self.visiting: return True
        self.visiting.add(key)
        sub = dict(zip(parameters, args))
        result = all(self.inert(owner, field, sub) for field in payloads)
        self.visiting.remove(key)
        if result: self.audit.add(owner.relative_to(self.real_root).as_posix() + '::' + name)
        return result

    @cache_proof
    def dependency_certificate(self, path, tokens):
        """Cache exact dependency evidence only within this immutable source snapshot."""
        try:
            from readonly_dependencies import provenance
        except ModuleNotFoundError as error:
            if error.name == 'readonly_dependencies': return None
            raise
        namespace = self.module(path)
        context = {'imports': namespace[3], 'definitions': list(namespace[2])}
        # `certify` is `provenance(...) is not None`: one verification, not two.
        return provenance(self.root, Path(path), tokens, namespace_context=context)

    def shared_method(self, path, state_type, method):
        names = [t for t in state_type[:state_type.index('<')] if t != ':'] if '<' in state_type else [t for t in state_type if t != ':']
        resolved = self.nominal(path, state_type)
        target = resolved[:2] if resolved else None
        if target and target[0] == 'std':
            methods = {
                'Option': {'as_ref', 'is_some', 'is_none'},
                'String': {'as_str', 'as_bytes', 'len', 'is_empty', 'clone'},
                'Vec': {'as_slice', 'iter', 'len', 'is_empty', 'first', 'last', 'get', 'clone'},
                'VecDeque': {'iter', 'len', 'is_empty', 'front', 'back', 'get', 'clone'},
                'BTreeMap': {'iter', 'keys', 'values', 'len', 'is_empty', 'get', 'contains_key', 'clone'},
                'Arc': {'as_ref', 'clone'},
                'PathBuf': {'as_path', 'clone'},
            }
            if target[1] in PRIMITIVES:
                return method in {'min', 'max', 'saturating_add', 'saturating_sub', 'saturating_mul', 'wrapping_add', 'wrapping_sub', 'checked_add', 'checked_sub', 'then_some', 'then'} and self.inert(path, state_type)
            if target[1] == 'Arc' and method not in methods['Arc'] and '<' in state_type:
                return self.shared_method(path, state_type[state_type.index('<') + 1:-1], method)
            copied_option = target[1] == 'Option' and method in {'map', 'map_or', 'map_or_else', 'and_then', 'or_else', 'or', 'and', 'filter', 'flatten', 'zip', 'unwrap_or', 'unwrap_or_else', 'is_some_and', 'is_none_or'} and self.copy_value(path, state_type)
            return (method in methods.get(target[1], set()) or copied_option) and self.inert(path, state_type)
        if not target or target[0] == 'std' or not self.inert(path, state_type): return False
        owner, name = target
        tokens, ends, _, _ = self.module(owner)
        impls, functions, _ = self.shape.item_bodies(tokens)
        matches = [(a, b, sig) for a, b, n, sig in functions if n == method
                   and any(x < a < b < y and self.shape.inherent_target(h, name) for x, y, h in impls)]
        if len(matches) != 1: return False
        if any('unsafe' in tokens[x:y] for x, y, h in impls if self.shape.inherent_target(h, name)): return False
        a, b, sig = matches[0]
        for i in range(a + 1, b):
            if tokens[i] == '!' and macro_invocation(tokens, i, ends):
                if (i < 1 or tokens[i - 1] not in ('matches', 'assert', 'debug_assert', 'vec', 'format', 'format_args', 'write', 'writeln', 'panic')
                        or tokens[max(0, i - 3):i - 1] == [':', ':'] or not self.builtin(owner, tokens[i - 1])):
                    return False
        shared = sig[:4] in (['(', '&', 'self', ')'], ['(', '&', 'self', ','])
        copied = sig[:3] in (['(', 'self', ')'], ['(', 'self', ',']) and self.copy_value(path, state_type)
        if 'unsafe' in tokens[a:b] or not (shared or copied): return False
        if not any(sig[i:i + 2] == ['-', '>'] for i in range(len(sig) - 1)): return True
        arrow = next(i for i in range(len(sig) - 1) if sig[i:i + 2] == ['-', '>'])
        result = [part for token in sig[arrow + 2:] for part in (state_type if token == 'Self' else [token])]
        if result[:6] == ['impl', 'Iterator', '<', 'Item', '=', '&'] and result[-1:] == ['>']:
            # No opaque iterator authority: prove the entire expression from standard field iterators.
            if not shared or any(alias in ('Iterator', '*') for alias, _ in self.module(owner)[3]): return False
            if any(tokens[i] in ('trait', 'type', 'struct', 'enum', 'mod', 'as') and tokens[i + 1:i + 2] == ['Iterator'] for i in range(len(tokens) - 1)): return False
            item = result[6:-1]
            if not item or not self.inert(owner, item): return False
            body = tokens[a + 1:b]
            leaves = [body] if len(body) == 7 else [body[:7], body[10:-1]] if body[7:10] == ['.', 'chain', '('] and body[-1:] == [')'] else []
            if not leaves: return False
            for leaf in leaves:
                if leaf[:2] != ['self', '.'] or leaf[3:] != ['.', 'iter', '(', ')']: return False
                field = self.field(owner, [name], leaf[2])
                target = self.nominal(*field) if field else None
                if target is None or target[0] != 'std' or target[1] not in ('Option', 'Vec', 'VecDeque', 'BTreeSet') or len(target[3]) != 1: return False
                if not self.inert(*field) or self.nominal(target[4], target[3][0]) != self.nominal(owner, item): return False
            return True
        return self.inert(owner, result)


    def shared_field(self, state_path, state_type, view_path, view_type, field):
        target = self.nominal(view_path, view_type)
        if target is None or target[0] == 'std': return False
        owner, name = target[:2]
        tokens, _, definitions, _ = self.module(owner)
        if any('<unproved-attribute>' in payload for _, payloads, _ in definitions[name] for payload in payloads): return False
        fields = self.shape.struct_fields(' '.join(tokens), name)
        found = [typ for _, key, typ in fields or [] if key == field]
        if len(found) != 1: return False
        typ = found[0]
        if typ[:1] != ['&'] or 'mut' in typ or '*' in typ: return False
        typ = typ[1:]
        if typ[:1] == ["'"]: typ = typ[2:]
        return (self.resolve(owner, [t for t in typ if t != ':']) == self.resolve(state_path, [t for t in state_type if t != ':'])
                and self.inert(owner, typ))


    def builtin(self, path, name, seen=None):
        path = self.real(path)
        seen = set() if seen is None else seen
        if path in seen: return True
        seen = seen | {path}
        tokens, _, _, uses = self.module(path)
        if 'macro_use' in tokens: return False
        shadows = shadows_of(tokens)
        if name in shadows.macro_rules or name in shadows.macro_items: return False
        if any(alias == name for alias, _ in uses): return False
        for alias, target in uses:
            if alias != '*': continue
            if target[:-1] == ('super',):
                parent = self.parent(path)
                if parent is None or not self.builtin(parent, name, seen): return False
            else: return False
        return True

    @cache_proof
    def discriminant_method(self, path, name, method):
        """A bounded match-only reader may discard opaque generic payloads, never expose them."""
        tokens, ends, _, _ = self.module(path)
        impls, functions, _ = self.shape.item_bodies(tokens)
        matches = [(a, b, sig) for a, b, n, sig in functions if n == method and
                   any(x < a < b < y and self.shape.inherent_target(h, name) for x, y, h in impls)]
        if len(matches) != 1: return False
        a, b, sig = matches[0]
        params_end = self.shape.ends_of(sig).get(0)
        params = split(sig[1:params_end]) if params_end is not None else []
        if not params or params[0] != ['&', 'self']: return False
        if any(':' not in p or not self.inert(path, p[p.index(':') + 1:]) for p in params[1:]): return False
        result = sig[params_end + 1:]
        if result[:2] != ['-', '>'] or '&' in result or not self.inert(path, result[2:]): return False
        body = tokens[a + 1:b]
        if body[:3] != ['match', 'self', '.'] or len(body) < 6 or body[4] != '{': return False
        if any(t in body for t in ('unsafe', 'mut', 'ref', 'let', 'fn', 'move', '!')): return False
        if body.count('self') != 1: return False
        if any(t == '.' and i != 2 and body[i - 1:i] != ['.'] and body[i + 1:i + 2] != ['.'] for i, t in enumerate(body)): return False
        field = self.field(path, [name], body[3])
        if field is None: return False
        target = self.nominal(*field)
        if target is None or target[2] != 'enum': return False
        owner, enum_name = target[:2]
        enum_tokens, enum_ends, _, _ = self.module(owner)
        declaration = next(i for i in range(len(enum_tokens) - 1) if enum_tokens[i:i + 2] == ['enum', enum_name])
        begin = declaration + 2
        while begin < len(enum_tokens) and enum_tokens[begin] != '{': begin += 1
        if begin not in enum_ends: return False
        known_fields = {}
        for variant in split(enum_tokens[begin + 1:enum_ends[begin]]):
            if len(variant) < 3 or variant[1] != '{': continue
            for part in split(variant[2:-1]):
                if len(part) > 2 and part[1] == ':': known_fields.setdefault(part[0], []).append(part[2:])
        local_ends = self.shape.ends_of(body)
        if local_ends.get(4) not in (len(body) - 1, len(body) - 2): return False
        for arm in split(body[5:local_ends[4]]):
            arrow = next((i for i in range(len(arm) - 1) if arm[i:i + 2] == ['=', '>']), None)
            if arrow is None or 'if' in arm[:arrow]: return False
            pattern = arm[:arrow]
            for i, token in enumerate(pattern):
                if not token.isidentifier() or not token[0].islower() or token == '_': continue
                if pattern[i + 1:i + 2] == [':']: continue
                field_name = pattern[i - 2] if i >= 2 and pattern[i - 1] == ':' else token
                types = known_fields.get(field_name, [])
                if not types or not all(self.inert(owner, t) for t in types): return False
        for i, token in enumerate(body):
            if token != '(' or not i or not body[i - 1].isidentifier(): continue
            if body[i - 1] != 'Some' or not self.builtin(path, 'Some'): return False
            if any(tokens[j:j + 2] == ['fn', 'Some'] for j in range(len(tokens) - 1)): return False
        return True

    def copy_value(self, path, typ, seen=None):
        """Closed layout proof for value reads; a user Copy bound alone proves nothing."""
        typ = list(typ)
        if not self.inert(path, typ): return False
        if typ[:1] == ['('] and typ[-1:] == [')']:
            return all(self.copy_value(path, p, seen) for p in split(typ[1:-1]))
        if typ[:1] == ['['] and typ[-1:] == [']']:
            return self.copy_value(path, split(typ[1:-1], ';')[0], seen)
        target = self.nominal(path, typ)
        if target is None: return False
        owner, name, kind, args, context = target
        if owner == 'std':
            return name in PRIMITIVES and not args or name in ('Option', 'Result') and all(self.copy_value(context, a, seen) for a in args)
        tokens, ends, definitions, _ = self.module(owner)
        declaration = next((i for i in range(len(tokens) - 1) if tokens[i] in ('struct', 'enum') and tokens[i + 1] == name), None)
        if declaration is None: return False
        reverse = {b: a for a, b in ends.items()}
        before = declaration - 1
        if tokens[before:before + 1] == ['pub']: before -= 1
        elif before in reverse and tokens[reverse[before] - 1:reverse[before]] == ['pub']: before = reverse[before] - 2
        derived = False
        while before in reverse and tokens[before] == ']':
            begin = reverse[before]
            if tokens[begin - 1:begin] != ['#']: break
            attr = tokens[begin + 1:before]
            derived |= attr[:2] == ['derive', '('] and 'Copy' in attr
            before = begin - 2
        if not derived: return False
        seen = set() if seen is None else seen
        key = owner, name
        if key in seen: return False
        parameters, payloads, _ = definitions[name][0]
        substitutions = dict(zip(parameters, args))
        return all(self.copy_value(owner, [part for t in p for part in substitutions.get(t, [t])], seen | {key}) for p in payloads)

    @cache_proof
    def field(self, path, typ, field):
        typ = list(typ)
        while typ[:1] == ['&']:
            typ = typ[1:]
            if typ[:1] == ["'"]: typ = typ[2:]
            if typ[:1] == ['mut']: typ = typ[1:]
        if '<' in typ:
            at = typ.index('<')
            if typ[0] in ('Option', 'Arc', 'Box') and typ[-1] == '>': return self.field(path, typ[at + 1:-1], field)
        target = self.nominal(path, typ)
        if not target or target[0] == 'std': return None
        owner, name, kind, args, context = target
        tokens, _, definitions, _ = self.module(owner)
        fields = self.shape.struct_fields(tokens, name)
        found = [t for _, n, t in fields or [] if n == field]
        if len(found) != 1: return None
        parameters = definitions[name][0][0]
        substitutions = dict(zip(parameters, args))
        return owner, [part for token in found[0] for part in substitutions.get(token, [token])]

    @cache_proof
    def nominal(self, path, typ, seen=None):
        """Resolve a real declaration (including transparent aliases), never a prose type name."""
        typ = list(typ)
        if not typ or any(t in typ for t in ('*', 'dyn', 'fn', 'impl', '!')): return None
        while typ[:1] == ['&']:
            typ = typ[1:]
            if typ[:1] == ["'"]: typ = typ[2:]
            if typ[:1] == ['mut']: typ = typ[1:]
        at = typ.index('<') if '<' in typ else len(typ)
        args = [p for p in split(typ[at + 1:-1]) if p and p[0] != "'"] if at < len(typ) and typ[-1:] == ['>'] else []
        target = self.resolve(path, [t for t in typ[:at] if t != ':'])
        if target is None: return None
        owner, name = target
        if owner == 'std': return owner, name, 'std', args, path
        parameters, payloads, kind = self.module(owner)[2][name][0]
        if kind != 'type': return owner, name, kind, args, path
        if len(parameters) != len(args): return None
        seen = set() if seen is None else seen
        key = (owner, name)
        if key in seen: return None
        substitutions = dict(zip(parameters, args))
        concrete = [p for t in payloads[0] for p in substitutions.get(t, [t])]
        return self.nominal(owner, concrete, seen | {key})

    def effect_output(self, path, typ, effect):
        """The output is a mutable standard Vec of the registered effect enum, possibly aliased."""
        typ = list(typ)
        if typ[:1] != ['&']: return False
        typ = typ[1:]
        if typ[:1] == ["'"]: typ = typ[2:]
        if typ[:1] != ['mut']: return False
        target = self.nominal(path, typ[1:])
        if target is None or target[:3] != ('std', 'Vec', 'std') or len(target[3]) != 1: return False
        item = self.nominal(target[4], target[3][0])
        expected = self.nominal(path, effect)
        return item is not None and expected is not None and item[:3] == expected[:3] and item[2] == 'enum'
