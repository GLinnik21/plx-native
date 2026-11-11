"""Bounded structural proof for shared reads of a registered owned state.

Only immutable standard containers are axioms. Local fields and aliases are followed through
module imports; opaque/ambiguous types, callbacks, raw pointers and interior authority fail closed.
Safe Rust then prevents an &self method from changing the proved owned state.
"""
from pathlib import Path
import re

PRIMITIVES = set('bool char str u8 u16 u32 u64 u128 usize i8 i16 i32 i64 i128 isize f32 f64'.split())
WRAPPERS = {'Vec', 'Option', 'Arc', 'String'}
STD = {'std::vec::Vec', 'alloc::vec::Vec', 'std::option::Option', 'core::option::Option',
       'std::sync::Arc', 'alloc::sync::Arc', 'std::string::String', 'alloc::string::String'}

def split(tokens, delimiter=','):
    result, current, depth = [], [], 0
    for token in tokens:
        if token == delimiter and not depth:
            result.append(current); current = []; continue
        depth += (token in '([{<') - (token in ')]}>')
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
    else: alias = tokens[-1] if tokens else ''
    return [(alias, prefix + tuple(t for t in tokens if t != ':'))]

class Proof:
    def __init__(self, root, shape):
        self.root, self.shape = Path(root), shape
        self.cache, self.visiting, self.audit = {}, set(), set()
        self.crates = {}
        for manifest in self.root.glob('*/Cargo.toml'):
            text = manifest.read_text(encoding='utf-8')
            sections = dict(re.findall(r'(?ms)^\[([^\]]+)\]\s*$(.*?)(?=^\[|\Z)', text))
            match = re.search(r'(?m)^name\s*=\s*"([A-Za-z0-9_-]+)"', sections.get('lib', ''))
            if match is None: match = re.search(r'(?m)^name\s*=\s*"([A-Za-z0-9_-]+)"', sections.get('package', ''))
            name = match[1].replace('-', '_') if match else ''
            if name: self.crates[name] = manifest.parent / 'src/lib.rs'

    def module(self, path):
        path = Path(path).resolve()
        if path in self.cache: return self.cache[path]
        tokens = self.shape.production_tokens(path.read_text(encoding='utf-8'))
        ends = self.shape.ends_of(tokens)
        reverse = {end: begin for begin, end in ends.items()}
        definitions, uses = {}, []
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
                elif token == 'struct': payload = [self.shape.shape(k, item)[2] for k, item in enumerate(split(tokens[j + 1:ends[j]]))]
                else:
                    payload = []
                    for variant in split(tokens[j + 1:ends[j]]):
                        while variant[:2] == ['#', '[']:
                            stop = variant.index(']'); variant = variant[stop + 1:]
                        if len(variant) > 1 and variant[1] in ('(', '{'):
                            items = split(variant[2:-1])
                            payload.extend(self.shape.shape(k, item)[2] if variant[1] == '{' else item for k, item in enumerate(items))
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

    def parent(self, path):
        path = Path(path)
        directory = path.parent if path.name not in ('lib.rs', 'mod.rs', 'main.rs') else path.parent.parent
        candidates = [directory.with_suffix('.rs'), directory / 'mod.rs']
        if directory.name == 'src': candidates = [directory / 'lib.rs', directory / 'main.rs']
        return next((p.resolve() for p in candidates if p.is_file() and p.resolve() != path.resolve()), None)

    def resolve(self, path, names, seen=None):
        path, names = Path(path).resolve(), tuple(names)
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
            directory = path.parent if path.name in ('lib.rs', 'mod.rs', 'main.rs') else path.with_suffix('')
            candidates = [directory / (names[0] + '.rs'), directory / names[0] / 'mod.rs']
            for child in candidates:
                explicit_path = any(tokens[i:i + 2] == ['mod', names[0]] and 'path' in tokens[max(0, i - 7):i] for i in range(len(tokens) - 1))
                if explicit_path: return None
                if child.is_file(): return self.resolve(child, names[1:], seen)
        found = [self.resolve(path, target[:-1] + names, seen) for alias, target in uses if alias == '*']
        found = {value for value in found if value is not None}
        if len(found) == 1: return found.pop()
        if len(names) == 1 and names[0] in PRIMITIVES | {'Vec', 'Option', 'String'}:
            return ('std', names[0])
        return None

    def inert(self, path, tokens, substitutions=None):
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
        if target is None: return False
        owner, name = target
        if owner == 'std':
            return (name in PRIMITIVES and not args or name == 'String' and not args
                    or name in WRAPPERS - {'String'} and len(args) == 1 and self.inert(path, args[0], substitutions))
        if not all(self.inert(path, arg, substitutions) for arg in args): return False
        parameters, payloads, kind = self.module(owner)[2][name][0]
        if len(parameters) != len(args): return False
        key = (owner, name, tuple(tuple(a) for a in args))
        if key in self.visiting: return True
        self.visiting.add(key)
        sub = dict(zip(parameters, args))
        result = all(self.inert(owner, field, sub) for field in payloads)
        self.visiting.remove(key)
        if result: self.audit.add(owner.relative_to(self.root.resolve()).as_posix() + '::' + name)
        return result

    def shared_method(self, path, state_type, method):
        names = [t for t in state_type if t not in (':', '<', '>')]
        target = self.resolve(path, names)
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
            if tokens[i] == '!' and tokens[i + 1:i + 2] in (['('], ['['], ['{']):
                if (i < 1 or tokens[i - 1] not in ('matches', 'assert', 'debug_assert', 'vec', 'format', 'format_args', 'write', 'writeln', 'panic')
                        or tokens[max(0, i - 3):i - 1] == [':', ':'] or not self.builtin(owner, tokens[i - 1])):
                    return False
        if 'unsafe' in tokens[a:b] or sig[:4] != ['(', '&', 'self', ')'] and sig[:4] != ['(', '&', 'self', ',']: return False
        if not any(sig[i:i + 2] == ['-', '>'] for i in range(len(sig) - 1)): return True
        arrow = next(i for i in range(len(sig) - 1) if sig[i:i + 2] == ['-', '>'])
        return self.inert(owner, sig[arrow + 2:])

    def shared_field(self, state_path, state_type, view_path, view_type, field):
        target = self.resolve(view_path, [t for t in view_type if t != ':'])
        if target is None or target[0] == 'std': return False
        owner, name = target
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
        path = Path(path).resolve()
        seen = set() if seen is None else seen
        if path in seen: return True
        seen = seen | {path}
        tokens, _, _, uses = self.module(path)
        if 'macro_use' in tokens: return False
        if any(tokens[i:i + 3] == ['macro_rules', '!', name] or tokens[i:i + 2] == ['macro', name] for i in range(len(tokens) - 2)):
            return False
        if any(alias == name for alias, _ in uses): return False
        for alias, target in uses:
            if alias != '*': continue
            if target[:-1] == ('super',):
                parent = self.parent(path)
                if parent is None or not self.builtin(parent, name, seen): return False
            else: return False
        return True

    def field(self, path, typ, field):
        typ = list(typ)
        while typ[:1] == ['&']:
            typ = typ[1:]
            if typ[:1] == ["'"]: typ = typ[2:]
            if typ[:1] == ['mut']: typ = typ[1:]
        if '<' in typ:
            at = typ.index('<')
            if typ[0] in ('Option', 'Arc', 'Box') and typ[-1] == '>': return self.field(path, typ[at + 1:-1], field)
        target = self.resolve(path, [t for t in typ if t != ':'])
        if not target or target[0] == 'std': return None
        owner, name = target
        tokens = self.module(owner)[0]
        fields = self.shape.struct_fields(' '.join(tokens), name)
        found = [t for _, n, t in fields or [] if n == field]
        return (owner, found[0]) if len(found) == 1 else None
