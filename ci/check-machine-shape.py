#!/usr/bin/env python3
"""Mandatory registry-backed privacy, single-writer and enum-lint contracts.

Every listed machine has a validated strict writer binding and an enum policy. Bool/Option counts
remain report-only. See machines.ini for the conservative proof limits; Clippy classifies enums.
"""
from functools import lru_cache
from pathlib import Path
import configparser
import sys
from machine_readonly import Tokens, derived, lexed, macro_invocation, shadows_of

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_test_modules as rtm  # noqa: E402

REPO = Path(__file__).resolve().parent.parent
MODULES = REPO / 'rust-modules'
INI = REPO / 'ci' / 'machines.ini'
OPEN, SHUT = '([{<', ')]}>'


def struct_fields(source, name):
    """[(is_pub, field name or index, type tokens)] of the braced or tuple struct `name`, or None
    when the source declares no such struct. Comments and strings are gone after lexing."""
    tokens = source if isinstance(source, list) else rust_tokens(source)
    return derived(tokens, ('struct_fields', name), lambda t: _struct_fields(t, name))


def _struct_fields(source, name):
    tokens = [t for t in source if t != '<test>']
    start = next((i for i in range(len(tokens) - 1) if tokens[i] == 'struct' and tokens[i + 1] == name), None)
    if start is None: return None
    body, angle = start + 2, 0
    while body < len(tokens) and (angle or tokens[body] not in ('{', '(', ';')):  # generics and `where` bounds
        angle += (tokens[body] == '<') - (tokens[body] == '>' and tokens[body - 1] != '-')
        body += 1
    if body >= len(tokens) or tokens[body] == ';': return []
    fields, depth, current = [], 0, []
    for k in range(body + 1, len(tokens)):
        token = tokens[k]
        if depth == 0 and token in ('}', ')'): break
        if depth == 0 and token == ',':
            if current: fields.append(current)
            current = []
            continue
        depth += (token in OPEN) - (token in SHUT and tokens[k - 1] != '-')
        current.append(token)
    if current: fields.append(current)
    fields = [field[next((i for i, t in enumerate(field) if t not in ('<test>', ';')), len(field)):] for field in fields]
    return [shape(index, field) for index, field in enumerate(f for f in fields if f)]


def shape(index, tokens):
    """(is_pub, name, type tokens) of one field's tokens, attributes and visibility stripped."""
    while tokens[:2] == ['#', '[']:
        depth = 0
        for k, token in enumerate(tokens[1:], 1):
            depth += (token == '[') - (token == ']')
            if depth == 0:
                tokens = tokens[k + 1:]
                break
    is_pub = tokens[:1] == ['pub']
    if is_pub:
        tokens = tokens[1:]
        if tokens[:1] == ['(']: tokens = tokens[tokens.index(')') + 1:]
    if len(tokens) > 1 and tokens[1] == ':': return is_pub, tokens[0], tokens[2:]
    return is_pub, str(index), tokens  # a tuple struct's field


# The lexing entry points are pure functions of the source TEXT, and one source is lexed over and over
# (every machine's analysis re-reads its production closure and every module a type resolves through,
# and the registry's mutation matrix re-checks each machine dozens of times with one file changed).
# They memoize on that text, never on a path or an mtime, and hand out one shared read-only `Tokens`.
@lru_cache(maxsize=64)
def rust_tokens(source):
    """Code tokens with raw identifiers normalized; literals cannot join adjacent code."""
    # Interned: a lexed file is mostly a few hundred distinct words repeated, and the memo below keeps
    # many of these lists alive at once.
    raw = [sys.intern(text) if kind == 'code' else '<literal>' for kind, text in rtm.lex(source)]
    if not any(raw[i] == 'r' and raw[i + 1] == '#' for i in range(len(raw) - 2)): return Tokens(raw)
    out, i = [], 0
    while i < len(raw):
        if raw[i:i + 2] == ['r', '#'] and i + 2 < len(raw):
            identifier = raw[i + 2]
            if identifier in ('if', 'while', 'return', 'match', 'let', 'else', 'for', 'yield', 'break', 'continue') and raw[i + 3:i + 4] == ['!']:
                identifier = '_raw_macro_' + identifier
            out.append(identifier); i += 3
        else:
            out.append(raw[i]); i += 1
    return Tokens(out)


def ends_of(tokens):
    return derived(tokens, 'ends', lambda t: rtm.group_ends([('code', token) for token in t]))


@lru_cache(maxsize=384)
def production_tokens(source):
    """Erase items proven cfg(test)-only, keeping item boundaries."""
    tokens = list(rust_tokens(source))
    ends = ends_of(tokens)
    i = 0
    while i + 1 < len(tokens):
        if tokens[i:i + 2] != ['#', '['] or i + 1 not in ends:
            i += 1; continue
        end = ends[i + 1]
        attr = tokens[i + 2:end]
        if len(attr) < 3 or attr[:2] != ['cfg', '('] or attr[-1] != ')':
            i = end + 1; continue
        if rtm.cfg_without_tests([('code', t) for t in attr[2:-1]]) != {False}:
            i = end + 1; continue
        j = end + 1
        while j < len(tokens):
            if tokens[j] == '#' and j + 1 in ends and tokens[j + 1] == '[':
                j = ends[j + 1] + 1; continue
            if tokens[j] == '{' and j in ends:
                j = ends[j] + 1; break
            if tokens[j] in (';', ','):
                j += 1; break
            if tokens[j] == '}': break
            if tokens[j] in ('(', '[') and j in ends:
                j = ends[j] + 1; continue
            j += 1
        boundary = ',' if tokens[j - 1:j] == [','] else ';'
        tokens[i:j] = ['<test>'] * (j - i)
        if j > i: tokens[j - 1] = boundary  # retain an item boundary for the following production impl
        i = j
    return Tokens(token for token in tokens if token != '<test>')


def item_bodies(tokens):
    """Return inherent impl and function spans; nested functions remain separate."""
    return derived(tokens, 'bodies', _item_bodies)


def _item_bodies(tokens):
    ends = ends_of(tokens)
    impls, functions = [], []
    for i, token in enumerate(tokens):
        if token not in ('impl', 'fn'): continue
        if token == 'impl' and i and tokens[i - 1] not in ('}', ';', ']', '{', 'unsafe', 'default'): continue
        j = i + 1
        if token == 'fn':
            if j >= len(tokens) or not tokens[j].isidentifier(): continue
            name = tokens[j]
            while j < len(tokens) and tokens[j] not in ('(', ';', '{'):
                j += 1
            if j not in ends or tokens[j] != '(': continue
            j = ends[j] + 1
        else:
            name = None
        while j < len(tokens) and tokens[j] not in ('{', ';'):
            if tokens[j] in ('(', '[') and j in ends:
                j = ends[j] + 1
            else:
                j += 1
        if j >= len(tokens) or tokens[j] != '{' or j not in ends: continue
        if token == 'impl': impls.append((j, ends[j], tokens[i + 1:j]))
        else: functions.append((j, ends[j], name, tokens[i + 2:j]))
    return impls, functions, ends


def impl_type(header):
    """The implemented Rust type, without impl parameters or an implemented trait."""
    header = list(header)
    if header[:1] == ['<']:
        depth, stop = 1, 1
        while stop < len(header) and depth:
            depth += (header[stop] == '<') - (header[stop] == '>'); stop += 1
        header = header[stop:]
    if 'for' in header: header = header[header.index('for') + 1:]
    if 'where' in header: header = header[:header.index('where')]
    return header


def inherent_target(header, name):
    """Accept only an unambiguous inherent impl of the registered type."""
    if 'for' in header: return False
    if header and header[0] == '<':
        depth, pos = 0, 0
        while pos < len(header):
            depth += (header[pos] == '<') - (header[pos] == '>')
            pos += 1
            if not depth: break
        header = header[pos:]
    return bool(header and header[0] == name and
                (len(header) == 1 or header[1] in ('<', 'where')))


def mutation_in_tail(tokens):
    """Reject writes and calls on a state projection; calls may hide a mutable receiver."""
    for i, token in enumerate(tokens):
        if token in (';', '{', '}'): break
        if token == '=':
            before = tokens[i - 1] if i else ''
            after = tokens[i + 1] if i + 1 < len(tokens) else ''
            shift = i >= 2 and tokens[i - 2:i] in (['<', '<'], ['>', '>'])
            if shift or (before not in ('=', '!', '<', '>') and after not in ('=', '>')): return True
        if token == '(' and i and tokens[i - 1] not in (')', ']'): return True
    return False


def builtin_macro(tokens, bang, macro):
    """Accept an unqualified builtin only when no local definition/import can shadow it."""
    if bang < 1 or tokens[bang - 1] != macro: return False
    if bang >= 3 and tokens[bang - 3:bang - 1] == [':', ':']: return False
    shadows = shadows_of(tokens)
    return macro not in shadows.macro_rules and macro not in shadows.imported and '*' not in shadows.imported


def readonly_match(tokens, bang, ends, field):
    """Recognize a builtin matches! with a direct scrutinee and no bindings or guard."""
    if bang + 1 not in ends or not builtin_macro(tokens, bang, 'matches'):
        return None
    start, stop = bang + 2, ends[bang + 1]
    comma = start + 1
    if tokens[start:start + 3] == ['self', '.', field]: comma = start + 3
    elif tokens[start:start + 1] != ['self']: return None
    if tokens[comma:comma + 1] != [',']: return None
    pattern = tokens[comma + 1:stop]
    if not pattern: return None
    for j, token in enumerate(pattern):
        if token.isidentifier() and token != '_':
            if token in ('self', 'ref', 'mut', 'if'): return None
            if not token[0].isupper() and pattern[j + 1:j + 2] != [':']:
                return None
        elif token not in (':', '{', '}', '(', ')', '[', ']', ',', '.', '|', '_') and not token.isdigit():
            return None
    return start, comma


def copy_enum_types(tokens):
    return derived(tokens, 'copy_enums', _copy_enum_types)


def _copy_enum_types(tokens):
    """Recognize derived-Copy fieldless enums; payloads and aliases stay conservative."""
    ends = ends_of(tokens)
    safe = set()
    for i in range(len(tokens) - 2):
        if tokens[i] != 'enum' or tokens[i + 2] != '{' or i + 2 not in ends: continue
        before = i - 1
        if tokens[before:before + 1] == ['pub']: before -= 1
        if before < 0 or tokens[before] != ']': continue
        attr = next((j for j, end in ends.items() if end == before and tokens[j] == '['), None)
        if attr is None or attr == 0 or tokens[attr - 1] != '#': continue
        derive = tokens[attr + 1:before]
        if len(derive) < 4 or derive[:2] != ['derive', '('] or derive[-1] != ')' or 'Copy' not in derive:
            continue
        variants = tokens[i + 3:ends[i + 2]]
        enum_name = tokens[i + 1]
        declarations = sum(tokens[j] in ('enum', 'struct', 'type', 'union') and tokens[j + 1] == enum_name
                           for j in range(len(tokens) - 1))
        shadows = shadows_of(tokens)
        imported = enum_name in shadows.imported or '*' in shadows.imported
        if declarations == 1 and not imported and variants and all(
                t == ',' or (t.isidentifier() and t[0].isupper()) for t in variants):
            safe.add(enum_name)
    return safe


def closure_spans(tokens, ends):
    """Conservatively delimit closures so a nested writer never inherits method authority."""
    spans = []
    for i, token in enumerate(tokens):
        if token != '|' or i and tokens[i - 1] not in ('=', '(', ',', '{', 'move', 'return', ':'): continue
        stop = i + 1
        while stop < len(tokens) and tokens[stop] not in ('|', ';', '{', '}'): stop += 1
        if tokens[stop:stop + 1] != ['|']: continue
        body = stop + 1
        if tokens[body:body + 1] == ['{'] and body in ends:
            spans.append((i, ends[body])); continue
        end = body
        while end < len(tokens) and tokens[end] not in (';', ',', '}', ')', ']'):
            end = ends[end] + 1 if end in ends else end + 1
        spans.append((i, end))
    return spans


# Closed semantic fact slots required by the current canonical headers, not type-name exemptions.
AUX_FACTS = {
    'play-uri/src/uriprobe.rs::UriProbe': 'now_ms:monotonic_ms_u64',
    'session/src/auth/owner.rs::SessionMachine': 'tick_ms:monotonic_ms_u32',
    'playback-contract/src/state.rs::BackendState': 'capabilities:playback_capabilities',
}
CAPABILITY_FIELDS = ('progressive_uri', 'hls_uri', 'packet_feed', 'pause', 'seek',
                     'audio_track', 'subtitle_track', 'audio_off', 'subtitle_off')


def primitive_fact(proof, path, typ, primitive, headers=()):
    """Require a by-value primitive spelling with no lexical shadow or alias."""
    tokens, _, definitions, uses = proof.module(path)
    if any(alias == '*' for alias, _ in uses): return False
    if any(primitive in header for header in headers): return False
    if typ == [primitive]:
        if primitive in definitions or any(alias == primitive for alias, _ in uses): return False
        if any(tokens[i] in ('union', 'trait', 'mod') and tokens[i + 1] == primitive for i in range(len(tokens) - 1)): return False
        return proof.resolve(path, [primitive]) == ('std', primitive)
    for namespace in ('core', 'std'):
        canonical = [':', ':', namespace, ':', ':', 'primitive', ':', ':', primitive]
        if typ not in (canonical, canonical[2:]): continue
        if namespace in definitions or any(alias == namespace for alias, _ in uses): return False
        if any(tokens[i] in ('mod', 'union', 'trait', 'extern', 'as') and tokens[i + 1] == namespace for i in range(len(tokens) - 1)): return False
        return not any(namespace in header for header in headers)
    return False


def auxiliary_argument(proof, path, param, declaration, headers=()):
    """Two clock widths and one exact nine-boolean capability record; no open model whitelist."""
    if not declaration or len(param) < 3 or param[1] != ':': return False
    name, kind = declaration.split(':')
    if param[0] != name: return False
    typ = param[2:]
    if kind in ('monotonic_ms_u32', 'monotonic_ms_u64'):
        return primitive_fact(proof, path, typ, kind.rsplit('_', 1)[1], headers)
    if kind != 'playback_capabilities' or typ != ['Option', '<', 'Capabilities', '>']: return False
    expected_path = (proof.root / 'playback-contract/src/lib.rs').resolve()
    if any(n in header for header in headers for n in ('Option', 'Capabilities')): return False
    if any(alias == '*' for alias, _ in proof.module(path)[3]): return False
    if proof.resolve(path, ['Option']) != ('std', 'Option'): return False
    if proof.resolve(path, ['Capabilities']) != (expected_path, 'Capabilities'): return False
    target = proof.nominal(path, typ)
    if target is None or target[:3] != ('std', 'Option', 'std') or len(target[3]) != 1: return False
    item = proof.nominal(target[4], target[3][0])

    if item is None or item[:3] != (expected_path, 'Capabilities', 'struct') or item[3]: return False
    tokens, _, definitions, uses = proof.module(expected_path)
    fields = struct_fields(tokens, 'Capabilities')
    if definitions['Capabilities'][0][0] or fields is None: return False
    if [n for _, n, _ in fields] != list(CAPABILITY_FIELDS): return False
    return proof.inert(expected_path, ['Capabilities']) and all(
        primitive_fact(proof, expected_path, t, 'bool') for _, _, t in fields)


def writer_problems(source, name, field, transition, constructor, label, require_methods=True,
                    reader=None, event=None, effect=None, proof=None, shared_reads=False, owned_fields=None, owner_path=None, aux_facts=""):
    """Literal Event/Effects authority covers EVERY owned field, never a selected-state whitelist."""
    tokens = production_tokens(source)
    impls, functions, ends = item_bodies(tokens)
    fields = struct_fields(tokens, name) or owned_fields or []
    owned = [(a, b) for a, b, h in impls if inherent_target(h, name)]
    declared = [(a, b, n, sig) for a, b, n, sig in functions
                if any(x < a < b < y for x, y in owned)
                and not any(x < a < y for x, y, _, _ in functions)]
    problems = []
    if transition != 'transition': problems.append(f'{label}: canonical method must be named transition')
    if event is None or effect is None or proof is None:
        problems.append(f'{label}: missing validated event/effect bindings')
    source_path = proof.root / label if proof is not None else None
    state_path = Path(owner_path) if owner_path is not None else source_path
    from machine_readonly import split
    def parameters(sig):
        end = ends_of(sig).get(0)
        return split(sig[1:end]) if end is not None else []
    def typ(param):
        return param[param.index(':') + 1:] if ':' in param else []
    def leading(a, method):
        fn = max(i for i in range(a) if tokens[i:i + 2] == ['fn', method])
        start = fn - 1
        while start >= 0 and tokens[start] not in (';', '{', '}'): start -= 1
        return tokens[start + 1:fn]
    if require_methods:
        matches = [(a, b, sig) for a, b, n, sig in declared if n == transition]
        if len(matches) != 1: problems.append(f'{label}: canonical transition must be unique')
        for a, b, sig in matches:
            if 'pub' in leading(a, transition): problems.append(f'{label}: canonical transition must be private')
            params = parameters(sig)
            if not params or params[0] != ['&', 'mut', 'self']:
                problems.append(f'{label}: transition must take &mut self')
            if proof is not None and event and effect:
                headers = [h for x, y, h in impls if x < a < b < y]
                names = [p[0] for p in params[1:] if len(p) >= 3 and p[1] == ':' and p[0].isidentifier()]
                if len(names) != len(params) - 1 or len(set(names)) != len(names):
                    problems.append(f'{label}: transition arguments require unique simple bindings')
                aux_params = [p for p in params[1:] if aux_facts and p[:2] == [aux_facts.split(':')[0], ':']]
                if aux_facts and (len(aux_params) != 1 or not auxiliary_argument(proof, source_path, aux_params[0], aux_facts, headers)):
                    problems.append(f'{label}: transition requires exact auxiliary fact {aux_facts}')
                    return problems  # Invalid/opaque facts cannot enter nominal resolution as authority.
                expected = proof.nominal(source_path, rust_tokens(event))
                event_params = [p for p in params[1:] if proof.nominal(source_path, typ(p)) is not None
                                and expected is not None and proof.nominal(source_path, typ(p))[:3] == expected[:3]]
                if expected is None or expected[2] != 'enum' or len(event_params) != 1 or any(typ(p)[:2] == ['&', 'mut'] for p in event_params):
                    problems.append(f'{label}: transition must take registered event enum {event}')
                elif any(proof.nominal(source_path, t) is not None and proof.nominal(source_path, t)[:3] == expected[:3] for _, _, t in fields):
                    problems.append(f'{label}: event enum must not be an owned raw state setter')
                effect_params = [p for p in params[1:] if effect != 'none' and proof.effect_output(source_path, typ(p), rust_tokens(effect))]
                for p in params[1:]:
                    if p not in event_params and p not in effect_params and p not in aux_params:
                        problems.append(f'{label}: unbound transition argument {" ".join(p)}')
                required = 1 + (effect != 'none') + bool(aux_facts)
                if len(params) != required + 1 or len(effect_params) != (effect != 'none'):
                    problems.append(f'{label}: transition argument arity must be self + Event + exact Fx/fact slots')
                if effect == 'none':
                    if any('mut' in p or 'FnMut' in p for p in params[1:]):
                        problems.append(f'{label}: effect = none forbids an effect output')
                elif not any(proof.effect_output(source_path, typ(p), rust_tokens(effect)) for p in params[1:]):
                    problems.append(f'{label}: transition requires &mut Vec<{effect}> or its concrete alias')
                if any('FnMut' in p or 'Fn' in p or 'FnOnce' in p or 'fn' in p or '*' in p for p in params[1:]):
                    problems.append(f'{label}: transition cannot delegate writes/effects to a callback or pointer')
        for a, b, method, sig in declared:
            if method != constructor: continue
            params = parameters(sig)
            if any('Self' in p or name in p or proof is not None and
                   (target := proof.nominal(source_path, typ(p))) is not None and target[1] == name for p in params):
                problems.append(f'{label}: constructor accepts existing machine authority (fresh values only)')
            if any(tokens[i:i + 2] == ['.', '.'] for i in range(a + 1, b)):
                problems.append(f'{label}: constructor must initialize fresh fields without struct updates')
    if not require_methods and any(n in (transition, constructor) for _, _, n, _ in declared):
        problems.append(f'{label}: a production child cannot define an alternate canonical writer/constructor')
    if proof is not None:
        for _, _, header in impls:
            target_type = impl_type(header)
            target = proof.nominal(source_path, target_type)
            if target is not None and target[1] == name and target_type[:1] != [name]:
                problems.append(f'{label}: a machine type alias cannot introduce another authority impl')
    substitutions = proof.generic_context(state_path, name) if proof is not None and fields else {}
    readers = {}
    for _, key, typ_tokens in fields:
        resolved = [part for token in typ_tokens for part in (substitutions or {}).get(token, [token])]
        readers[key] = (proof, state_path, resolved) if proof is not None and shared_reads else None
    if isinstance(reader, tuple): readers[field] = reader
    elif isinstance(reader, dict): readers.update(reader)
    if proof is not None:
        field_types = {k: [part for token in t for part in (substitutions or {}).get(token, [token])] for _, k, t in fields}
        for a, b, method, sig in declared:
            if parameters(sig)[:1] != [['&', 'self']]: continue
            opaque = False
            for i in range(a + 1, b - 2):
                if tokens[i:i + 2] != ['self', '.'] or tokens[i + 2] not in field_types: continue
                position, resolved = i + 2, (state_path, field_types[tokens[i + 2]])
                while tokens[position + 1:position + 2] == ['.'] and tokens[position + 3:position + 4] != ['(']:
                    resolved = proof.field(*resolved, tokens[position + 2]) if resolved else None
                    position += 2
                if resolved is None or not proof.inert(*resolved): opaque = True
            if opaque and not proof.discriminant_method(source_path, name, method):
                problems.append(f'{label}::{method}: shared opaque payload requires a bounded inert/discriminant proof')
    targets = fields or [(False, field, [])]
    contexts = {}
    for _, key, _ in targets:
        problems.extend(_field_writer_problems(source, name, key, transition, constructor, label,
                        require_methods=require_methods, reader=readers.get(key), analysis=(tokens, impls, functions, ends, contexts)))
    for _, _, method, sig in declared:
        params = parameters(sig)
        if params and 'self' in params[0] and '&' not in params[0]:
            problems.append(f'{label}::{method}: consuming the machine must use an owner event')
    machine_methods = {n: [(a, b, sig) for a, b, m, sig in declared if m == n] for _, _, n, _ in declared}
    all_inert = proof is not None and substitutions is not None and all(proof.inert(state_path, [part for token in t for part in substitutions.get(token, [token])]) for _, _, t in fields)
    machine_body = sorted({i for a, b, h in impls if inherent_target(h[h.index('for') + 1:] if 'for' in h else h, name)
                           for i in range(a + 1, b)})
    for i in machine_body:
        token = tokens[i]
        if token == 'unsafe': problems.append(f'{label}: unsafe machine authority cannot be proved')
        if token == 'self' and tokens[i + 1:i + 2] == ['.'] and tokens[i + 3:i + 4] == ['(']:
            method = tokens[i + 2]
            if method == transition: continue
            candidates = machine_methods.get(method, [])
            shared = len(candidates) == 1 and parameters(candidates[0][2])[:1] == [['&', 'self']]
            bounded = proof is not None and proof.discriminant_method(source_path, name, method) if shared and not all_inert else False
            if not shared or not (all_inert or bounded) or 'unsafe' in tokens[candidates[0][0]:candidates[0][1]]:
                problems.append(f'{label}: live machine call {method} is not the canonical event reducer or a proved shared decision')
        if token == '!' and macro_invocation(tokens, i, ends):
            args = tokens[i + 2:ends[i + 1]]
            if 'self' in args and not (tokens[i - 1] in ('matches', 'assert', 'debug_assert', 'vec', 'format', 'format_args', 'write', 'writeln', 'panic')
                    and proof is not None and proof.builtin(source_path, tokens[i - 1]) and builtin_macro(tokens, i, tokens[i - 1])):
                problems.append(f'{label}: unknown macro may capture live machine authority')
    if proof is not None:
        for a, b, method, sig in functions:
            for p in parameters(sig):
                if '&' not in p or 'mut' not in p or ':' not in p: continue
                target = proof.nominal(source_path, typ(p))
                if target is not None and target[1] == name and target[0] == source_path.resolve():
                    problems.append(f'{label}::{method}: mutable machine alias may replace owned fields')
    return list(dict.fromkeys(problems))


def _field_writer_problems(source, name, field, transition, constructor, label, require_methods=True, reader=None, analysis=None):
    if analysis is None:
        tokens = production_tokens(source)
        impls, functions, ends = item_bodies(tokens)
    else: tokens, impls, functions, ends, contexts = analysis
    if analysis is None: contexts = {}
    owned = [(a, b) for a, b, h in impls if inherent_target(h, name)]
    closures = derived(tokens, 'closures', lambda t: closure_spans(t, ends_of(t)))

    def compute_context(index):
        if any(a <= index < b for a, b in closures): return None
        containing = [(a, b, n) for a, b, n, _ in functions if a < index < b]
        if not containing: return None
        a, b, method = min(containing, key=lambda x: x[1] - x[0])
        parents = [(x, y) for x, y, _, _ in functions if x < a < y]
        return method if not parents and any(x < a < b < y for x, y in owned) else None

    def context(index):
        if index not in contexts: contexts[index] = compute_context(index)
        return contexts[index]

    problems = []
    fields = struct_fields(tokens, name) or []
    field_types = [typ for _, field_name, typ in fields if field_name == field]
    plain_struct = derived(tokens, ('plain_struct', name), lambda t: any(t[j:j + 3] == ['struct', name, '{'] for j in range(len(t) - 2)))
    copy_field = (plain_struct and len(field_types) == 1 and len(field_types[0]) == 1
                  and field_types[0][0] in copy_enum_types(tokens))
    bangs = derived(tokens, 'bangs', lambda t: [i for i, token in enumerate(t) if token == '!'])
    safe_matches = {i: span for i in bangs if (span := readonly_match(tokens, i, ends, field)) is not None}
    # These wrappers consume only the bool result, with no formatting expressions.
    safe_asserts = set()
    for i in bangs:
        if i + 1 not in ends: continue
        if not any(builtin_macro(tokens, i, macro) for macro in ('assert', 'debug_assert')): continue
        inner = i + 3
        if inner not in safe_matches or inner + 1 not in ends: continue
        rest = tokens[ends[inner + 1] + 1:ends[i + 1]]
        if rest in ([], [',']): safe_asserts.add(i)

    def copy_argument(index, prefix):
        # A fieldless Copy enum cannot contain a mutable alias. No other bindings are relaxed.
        if not copy_field or tokens[index + 1:index + 2] != [')'] or len(prefix) < 7: return False
        call = prefix[-7:]
        if call[:3] != ['Self', ':', ':'] or call[4:] != ['(', 'self', '.']: return False
        helper = call[3]
        for a, b, method, sig in functions:
            if method != helper or not any(x < a < b < y for x, y in owned): continue
            if len(sig) >= 7 and sig[0] == '(' and sig[2:5] == [':', field_types[0][0], ')']:
                result = sig[5:]
                if result[:2] == ['-', '>'] and '&' not in result and 'DerefMut' not in result:
                    return True
        return False

    def location(index):
        containing = [(a, b, method) for a, b, method, _ in functions if a < index < b]
        method = min(containing, key=lambda item: item[1] - item[0])[2] if containing else None
        return f'{label}::{method}' if method else label

    # Only a direct associated Self struct literal is an authorized constant constructor.
    constants = []
    for i in derived(tokens, 'consts', lambda t: [j for j, token in enumerate(t) if token == 'const']):
        if i >= len(tokens) - 6 or tokens[i:i + 6] != ['const', constructor, ':', 'Self', '=', 'Self']:
            continue
        brace = i + 6
        if brace not in ends or tokens[brace] != '{' or tokens[ends[brace] + 1:ends[brace] + 2] != [';']:
            continue
        parents = [(a, b) for a, b in ends.items() if tokens[a] == '{' and a < i < b]
        if parents and min(parents, key=lambda pair: pair[1] - pair[0]) in owned:
            constants.append((i + 5, brace, ends[brace]))
    if require_methods:
        declared = [(a, b, n, sig) for a, b, n, sig in functions
                    if any(x < a < b < y for x, y in owned)
                    and not any(x < a < y for x, y, _, _ in functions)]
        matches = [sig for _, _, n, sig in declared if n == transition]
        if not matches:
            problems.append(f'{label}: missing inherent method {transition} (stale strict contract)')
        for sig in matches:
            if not any(sig[i:i + 3] == ['&', 'mut', 'self'] for i in range(len(sig) - 2)):
                problems.append(f'{label}: transition {transition} must take &mut self')
        constructors = [(a, b, sig) for a, b, n, sig in declared if n == constructor]
        if not constructors and not constants:
            problems.append(f'{label}: missing inherent constructor {constructor} (stale strict contract)')
        if len(constructors) + len(constants) > 1:
            problems.append(f'{label}: constructor {constructor} must be unique')
        for a, b, sig in constructors:
            if 'self' in sig:
                problems.append(f'{label}: constructor {constructor} must not take self')
            initialized = any(tokens[i] in (name, 'Self') and i + 1 in ends and tokens[i + 1] == '{'
                              and ends[i + 1] < b and field in tokens[i + 2:ends[i + 1]]
                              for i in range(a + 1, b))
            if not initialized:
                problems.append(f'{label}: constructor {constructor} must initialize {field} in a struct literal')
        for _, brace, end in constants:
            if field not in tokens[brace + 1:end]:
                problems.append(f'{label}: constructor {constructor} must initialize {field} in a struct literal')
    # A whole-machine mutable reference can replace the state without naming the field.
    # Reject such helper parameters, including in production child modules. Receivers are
    # checked separately below: the public step may borrow self only to call transition.
    for a, b, method, sig in functions:
        self_type = 'Self' in sig and any(x < a < b < y for x, y in owned)
        if (name in sig or self_type) and '&' in sig and 'mut' in sig:
            problems.append(f'{label}::{method}: mutable machine reference may replace state')

    def self_is_machine(index):
        containing = [(a, b, h) for a, b, h in impls if a < index < b]
        if not containing: return False
        _, _, header = min(containing, key=lambda item: item[1] - item[0])
        return inherent_target(header[header.index('for') + 1:] if 'for' in header else header, name)

    def unrelated_projection(index):
        if reader is None: return False
        if tokens[index + 1:index + 2] == ['('] and tokens[index - 2:index] == ['self', '.'] and reader[0].shared_method(reader[0].root / label, [name], field): return True
        root = index - 2
        while root >= 2 and tokens[root - 1] == '.': root -= 2
        if root >= 0 and tokens[root] == 'self' and root + 2 < index and self_is_machine(index): return True
        receiver = tokens[index - 2] if index >= 2 else ''
        if receiver == 'self':
            if self_is_machine(index): return False
            proof = reader[0]
            enclosing = [(a, b, h) for a, b, h in impls if a < index < b]
            if not enclosing: return False
            header = impl_type(min(enclosing, key=lambda item: item[1] - item[0])[2])
            return proof.field(proof.root / label, header, field) is not None
        proof, state_path, state_type = reader
        source_path = proof.root / label
        enclosing = [(a, b, sig) for a, b, _, sig in functions if a < index < b]
        if not enclosing: return False
        a, b, signature = min(enclosing, key=lambda item: item[1] - item[0])
        for j in range(len(signature) - 1):
            if signature[j:j + 2] == [receiver, ':']:
                stop = j + 2
                while stop < len(signature) and signature[stop] not in (',', ')'): stop += 1
                return proof.field(source_path, signature[j + 2:stop], field) is not None and name not in signature[j + 2:stop]
        # A local borrowed from a different, structurally resolved machine field belongs to that field's owner.
        for j in range(a + 1, index):
            if tokens[j] != 'let': continue
            equal = j + 1
            while equal < index and tokens[equal] not in ('=', ';', '{'): equal += 1
            if tokens[equal:equal + 1] != ['='] or receiver not in tokens[j:equal]: continue
            binding = tokens[j + 1:equal]
            if ':' in binding:
                declared_type = binding[binding.index(':') + 1:]
                target = proof.nominal(source_path, declared_type)
                if target is not None and target[1] != name and proof.field(source_path, declared_type, field) is not None: return True
            pos = equal + 1
            if tokens[pos:pos + 1] == ['&']: pos += 1
            if tokens[pos:pos + 2] != ['self', '.'] or tokens[pos + 2:pos + 3] == [field]: continue
            resolved = (state_path, [name])
            pos += 1
            while tokens[pos:pos + 1] == ['.'] and tokens[pos + 2:pos + 3] != ['(']:
                resolved = proof.field(resolved[0], resolved[1], tokens[pos + 1]) if resolved else None
                pos += 2
            if resolved and name not in resolved[1] and proof.field(resolved[0], resolved[1], field) is not None: return True
        return False

    def shared_read(index):
        if reader is None: return False
        proof, state_path, state_type = reader
        position, resolved = index, (state_path, state_type)
        while True:
            if tokens[position + 1:position + 2] == ['['] and position + 1 in ends:
                target = proof.nominal(*resolved) if resolved else None
                if target is None or target[:3] not in (('std', 'Vec', 'std'), ('std', 'VecDeque', 'std'), ('std', 'BTreeMap', 'std')): return False
                resolved = (target[4], target[3][-1])
                position = ends[position + 1]
            elif tokens[position + 1:position + 2] == ['.'] and tokens[position + 3:position + 4] != ['(']:
                resolved = proof.field(resolved[0], resolved[1], tokens[position + 2]) if resolved else None
                position += 2
            else: break
        if resolved is None or not proof.inert(*resolved): return False
        if tokens[position + 1:position + 3] in (['=', '='], ['!', '='], ['<', '='], ['>', '=']): return True
        for j in range(position + 1, len(tokens)):
            token = tokens[j]
            if token in (';', '{', '}', ','): break
            if token == '=' and (j == 0 or tokens[j - 1] not in ('=', '!', '<', '>')) and tokens[j + 1:j + 2] not in (['='], ['>']): return False
        if tokens[position + 1:position + 2] == ['.'] and tokens[position + 3:position + 4] == ['(']:
            return proof.shared_method(resolved[0], resolved[1], tokens[position + 2])
        if tokens[index - 5:index] in (['!', '=', '*', 'self', '.'], ['=', '=', '*', 'self', '.']):
            target = proof.nominal(*resolved)
            if target is not None and target[0] == 'std' and target[1] == 'Arc': return True
        if tokens[index - 3:index] != ['&', 'self', '.']:
            return proof.copy_value(*resolved)
        if tokens[index - 4:index - 3] == [':'] and tokens[index - 5].isidentifier():
            braces = [(a, b) for a, b in ends.items() if tokens[a] == '{' and a < index < b]
            if not braces: return False
            begin, _ = min(braces, key=lambda pair: pair[1] - pair[0])
            start = begin - 1
            while start > 1 and tokens[start - 2:start] == [':', ':']: start -= 3
            return proof.shared_field(resolved[0], resolved[1], proof.root / label, tokens[start:begin], tokens[index - 5])
        return True

    def owned_take(index, owner):
        if reader is None or owner != transition: return False
        # The borrow is consumed by the standard intrinsic, never exposed to a callback.
        for method in ('replace', 'take'):
            expected = ['std', ':', ':', 'mem', ':', ':', method, '(', '&', 'mut', 'self', '.']
            if tokens[max(0, index - len(expected)):index] == expected:
                return not shadows_of(tokens).std_named
        return False

    def owned_bool_match(index, owner):
        """An in-place enum bool binding may only receive a literal in this reducer arm."""
        if reader is None or owner != transition or tokens[index - 5:index] != ['match', '&', 'mut', 'self', '.']: return False
        brace = index + 1
        if brace not in ends or tokens[brace] != '{': return False
        proof, state_path, state_type = reader
        target = proof.nominal(state_path, state_type)
        if target is None or target[2] != 'enum' or not proof.inert(state_path, state_type): return False
        enum_path, enum_name = target[:2]
        enum_tokens, enum_ends, _, _ = proof.module(enum_path)
        declaration = next((i for i in range(len(enum_tokens) - 1) if enum_tokens[i:i + 2] == ['enum', enum_name]), None)
        if declaration is None: return False
        begin = declaration + 2
        while begin < len(enum_tokens) and enum_tokens[begin] != '{': begin += 1
        if begin not in enum_ends: return False
        bool_fields = set()
        pos = begin + 1
        while pos < enum_ends[begin]:
            variant = enum_tokens[pos]
            if enum_tokens[pos + 1:pos + 2] == ['{'] and pos + 1 in enum_ends:
                stop = enum_ends[pos + 1]
                for j in range(pos + 2, stop - 2):
                    if enum_tokens[j + 1:j + 3] == [':', 'bool']: bool_fields.add((variant, enum_tokens[j]))
                pos = stop + 1
            elif pos + 1 in enum_ends: pos = enum_ends[pos + 1] + 1
            else: pos += 1
            if enum_tokens[pos:pos + 1] == [',']: pos += 1
        pos, bound = brace + 1, False
        while pos < ends[brace]:
            start = pos
            while pos < ends[brace] and tokens[pos:pos + 2] != ['=', '>']:
                pos = ends[pos] + 1 if pos in ends else pos + 1
            if pos >= ends[brace]: return False
            pattern = tokens[start:pos]
            pos += 2
            body_start = pos
            if pos in ends: pos = ends[pos] + 1
            else:
                while pos < ends[brace] and tokens[pos] != ',': pos += 1
            body = tokens[body_start:pos]
            if '@' in pattern:
                if len(pattern) != 14 or pattern[:3] != [enum_name, ':', ':'] or pattern[4] != '{' or pattern[6] != ':' or pattern[8] != '@' or pattern[9] not in ('false', 'true') or pattern[10:] != [',', '.', '.', '}']: return False
                variant, field_name, alias = pattern[3], pattern[5], pattern[7]
                if (variant, field_name) not in bool_fields or body not in (['*', alias, '=', 'true'], ['*', alias, '=', 'false']): return False
                if context(body_start) != transition: return False
                bound = True
            elif any(t.isidentifier() and t[0].islower() and t != '_' for t in pattern): return False
            if tokens[pos:pos + 1] == [',']: pos += 1
        return bound

    for i, token in enumerate(tokens):
        if token not in ('self', field, name, 'Self', '!', '*'): continue
        owner = context(i)
        if token == 'self' and (owner is not None or self_is_machine(i) and any(a < i < b for a, b, _, _ in functions)):
            if tokens[i + 1:i + 2] != ['.'] and not any(start == i for start, _ in safe_matches.values()):
                problems.append(f'{location(i)}: machine reference may escape the authorized writer')
        if token == field and i and tokens[i - 1] == '.' and not unrelated_projection(i):
            start = i - 1
            while start > 0 and tokens[start - 1] not in (';', '{', '}'):
                start -= 1
            prefix = tokens[start:i]
            # A shared reference can also escape and mutate interior-mutable state later.
            root = i - 2
            while root and tokens[root - 1] == '(': root -= 1
            logical_and = tokens[max(0, root - 2):root] == ['&', '&'] and root > 2 and (tokens[root - 3].isidentifier() or tokens[root - 3] in (')', ']', '}'))
            borrowed = (tokens[root - 1:root] == ['&'] and not logical_and or tokens[max(0, root - 2):root] == ['&', 'mut'] or
                        any(prefix[j:j + 2] == ['ref', 'mut'] for j in range(len(prefix) - 1)))
            enclosing = [(a, b, sig) for a, b, _, sig in functions if a < i < b]
            signature = min(enclosing, key=lambda x: x[1] - x[0])[2] if enclosing else []
            arrow = next((j for j in range(len(signature) - 1) if signature[j:j + 2] == ['-', '>']), len(signature))
            result = signature[arrow + 2:]
            reference_result = '&' in result or 'DerefMut' in result
            forwarded = '=' in prefix or '(' in prefix
            matched_read = any(start + 2 == i and comma == i + 1 for start, comma in safe_matches.values())
            copied_argument = copy_argument(i, prefix)
            mutable_borrow = tokens[max(0, root - 2):root] == ['&', 'mut'] or any(prefix[j:j + 2] == ['ref', 'mut'] for j in range(len(prefix) - 1))
            proven_read = not mutable_borrow and shared_read(i)
            intrinsic = owned_take(i, owner) or owned_bool_match(i, owner)
            mutable_result = any(result[j:j + 2] == ['&', 'mut'] for j in range(len(result) - 1))
            if ((reference_result and not proven_read or mutable_result) or (not matched_read and not intrinsic and not proven_read and (borrowed or 'addr_of_mut' in prefix or
                    (owner != transition and ((forwarded and not copied_argument) or '!' in prefix or mutation_in_tail(tokens[i + 1:])))))):
                problems.append(f'{location(i)}: owned field {field} projection may mutate or escape the authorized writer (scope={owner})')
        # Destructuring may bind by reference; constructing a machine is restricted to its constructor.
        if token in (name, 'Self') and (token == name or reader is None or self_is_machine(i)) and i + 1 in ends and tokens[i + 1] == '{':
            if i and tokens[i - 1] in ('struct', 'enum', 'impl'): continue
            if any(a == i + 1 for a, _, _, _ in functions) or any(a == i + 1 for a, _, _ in impls): continue
            stop = ends[i + 1]
            pattern = tokens[stop + 1:stop + 2] == ['='] or tokens[stop + 1:stop + 3] == ['=', '>']
            if field in tokens[i + 2:stop] and ((pattern and owner != transition) or (not pattern and owner != constructor and not (require_methods and any(i == start for start, _, _ in constants)))):
                problems.append(f'{location(i)}: state construction/destructuring outside {constructor} or {transition}')
        if token == '!' and macro_invocation(tokens, i, ends) and owner != transition:
            args = tokens[i + 2:ends[i + 1]]
            if (((reader is None or self_is_machine(i)) and any(t == 'self' and args[j + 1:j + 2] != ['.'] for j, t in enumerate(args))) or any(t == field and j and args[j - 1] == '.' for j, t in enumerate(args))) and i not in safe_matches and i not in safe_asserts and not (reader is not None and (i < 3 or tokens[i - 3:i - 1] != [':', ':']) and tokens[i - 1] in ('matches', 'assert', 'debug_assert', 'vec', 'format', 'format_args', 'write', 'writeln', 'panic') and reader[0].builtin(reader[0].root / label, tokens[i - 1])):
                problems.append(f'{location(i)}: macro may hide a state mutation outside {transition}')
        if token == '*' and tokens[i + 1:i + 3] == ['self', '='] and owner != transition and (reader is None or self_is_machine(i)):
            problems.append(f'{location(i)}: replacement of self outside {transition}')
    return problems





def wildcard_problems(source, label, require_header=True):
    tokens = production_tokens(source)
    ends = ends_of(tokens)
    lint = ['clippy', ':', ':', 'wildcard_enum_match_arm']
    found = False
    i = 0
    while tokens[i:i + 3] == ['#', '!', '['] and i + 2 in ends:
        end = ends[i + 2]
        attr = tokens[i + 3:end]
        if attr in (['deny', '('] + lint + [')'], ['forbid', '('] + lint + [')']): found = True
        i = end + 1
    problems = [] if found or not require_header else [f'{label}: module must have #![deny(clippy::wildcard_enum_match_arm)]']
    for i in range(len(tokens) - 2):
        bracket = i + 2 if tokens[i:i + 3] == ['#', '!', '['] else i + 1
        if tokens[i] != '#' or bracket not in ends or tokens[bracket] != '[': continue
        attr = tokens[bracket + 1:ends[bracket]]
        if any(level in attr for level in ('allow', 'expect', 'warn')) and any(x in attr for x in ('wildcard_enum_match_arm', 'warnings', 'all')):
            problems.append(f'{label}: wildcard enum lint may not be locally suppressed')
    return problems


def production_sources(path, root):
    """Exhaust the registered production closure; never guess conditional/generated paths."""
    root = Path(root).resolve()
    pending, seen = [Path(path).resolve()], set()
    while pending:
        current = pending.pop()
        if not current.is_relative_to(root):
            raise ValueError(f'production include escapes rust-modules: {current}')
        if current in seen: continue
        seen.add(current)
        tokens, ends = lexed(current.read_text(encoding='utf-8'))
        module_dir = current.parent if current.name in ('mod.rs', 'lib.rs', 'main.rs') else current.with_suffix('')
        def walk(start, stop, directory, attribute_base, inherited_test=False):
            i, attrs = start, []
            while i < stop:
                kind, text = tokens[i]
                if kind != 'code': i += 1; continue
                if text == '#' and i + 1 < stop and tokens[i + 1][1] == '[':
                    end = ends.get(i + 1)
                    if end is None: raise ValueError(f'{current}: unresolved attribute')
                    attrs.append(tokens[i + 2:end]); i = end + 1; continue
                test = inherited_test or any(a and a[0][1] == 'cfg' and rtm.cfg_without_tests(a[2:-1]) == {False} for a in attrs)
                conditional_path = any(a and a[0][1] == 'cfg_attr' and any(t == ('code', 'path') for t in a) for a in attrs)
                paths = [a[2][1] for a in attrs if len(a) == 3 and a[:2] == [('code', 'path'), ('code', '=')] and a[2][0] == 'string']
                if not test and conditional_path:
                    raise ValueError(f'{current}: production cfg_attr(path) is unresolved')
                if text == 'mod' and i + 2 < stop:
                    name, form = tokens[i + 1][1], tokens[i + 2][1]
                    if form in (';', '{'):
                        if not test and (len(paths) > 1 or any(a and a[0][1] == 'path' for a in attrs) and not paths):
                            raise ValueError(f'{current}: unresolved module path')
                        if form == ';':
                            candidates = [attribute_base / paths[0]] if paths else [directory / (name + '.rs'), directory / name / 'mod.rs']
                            targets = [p.resolve() for p in candidates if p.is_file()]
                            if not test:
                                if len(targets) != 1: raise ValueError(f'{current}: unresolved production module {name}')
                                pending.extend(targets)
                            i += 3
                        else:
                            end = ends.get(i + 2)
                            if end is None: raise ValueError(f'{current}: unresolved inline module {name}')
                            nested = attribute_base / paths[0] if paths else directory / name
                            walk(i + 3, end, nested, nested, test)
                            i = end + 1
                        attrs = []; continue
                if text == 'include' and tokens[i + 1:i + 2] == [('code', '!')]:
                    end = ends.get(i + 2)
                    if not test:
                        if end != i + 4 or tokens[i + 2][1] != '(' or tokens[i + 3][0] != 'string':
                            raise ValueError(f'{current}: opaque production include!')
                        target = (current.parent / tokens[i + 3][1]).resolve()
                        if not target.is_file(): raise ValueError(f'{current}: unresolved production include!')
                        pending.append(target)
                    i = end + 1 if end is not None else stop
                    attrs = []; continue
                if text in ('{', '(', '['):
                    end = ends.get(i)
                    if end is None: raise ValueError(f'{current}: unbalanced production group')
                    if text == '{': walk(i + 1, end, directory, attribute_base, test)
                    i = end + 1
                    attrs = []; continue
                if text in (';', ','): attrs = []
                i += 1
        walk(0, len(tokens), module_dir, current.parent)
        yield current


def check(modules=MODULES, ini=INI):
    """([problem], [report line]) for every machine and module in the registry."""
    parser = configparser.ConfigParser(interpolation=None, comment_prefixes=('#',), inline_comment_prefixes=None)
    try:
        with open(ini, encoding='utf-8') as f:
            parser.read_file(f)
    except (OSError, configparser.Error) as error:
        return [f'{Path(ini).name} does not parse: {error}'], []
    problems, report = [], []
    if parser.defaults():
        problems.append('machines.ini: DEFAULT keys are not permitted')
    strict_keys = {'writer_policy', 'state_field', 'transition', 'constructor'}
    if not parser.sections():
        problems.append('machines.ini: registry must not be empty')
    for section in parser.sections():
        entry = parser[section]
        is_module = section.startswith('module ')
        file, _, name = section.partition('::')
        if is_module: file = section[len('module '):]
        root = Path(modules).resolve()
        path = (root / file).resolve()
        if not file or not path.is_relative_to(root) or not path.is_file() or (not is_module and not name):
            problems.append(f'machines.ini [{section}]: no such file under rust-modules/ (stale entry: fix or remove it)')
            continue
        file = path.relative_to(root).as_posix()
        if is_module:
            if set(entry) != {'wildcard_enum_match_arm'} or entry.get('wildcard_enum_match_arm') != 'deny':
                problems.append(f'machines.ini [{section}]: module requires wildcard_enum_match_arm = deny')
                continue
            try:
                for child in production_sources(path, root):
                    problems.extend(wildcard_problems(child.read_text(encoding='utf-8'),
                        f'module {child.relative_to(root).as_posix()}', require_header=child == path))
            except (OSError, ValueError) as error:
                problems.append(f'machines.ini [{section}]: cannot analyze enum policy: {error}')
            report.append(f'enum policy {file}: clippy::wildcard_enum_match_arm deny')
            continue
        if 'module ' + file not in parser:
            problems.append(f'machines.ini [{section}]: missing enum policy [module {file}]')
        unknown = set(entry) - {'event', 'effect', 'step', 'state_reads', 'aux_facts'} - strict_keys
        if unknown:
            problems.append(f'machines.ini [{section}]: unknown keys: {", ".join(sorted(unknown))}')
        aux_facts = entry.get('aux_facts', '').strip()
        if aux_facts != AUX_FACTS.get(section, ''):
            problems.append(f'{section}: auxiliary fact contract must be {AUX_FACTS.get(section, "none")}')
            continue
        source = path.read_text(encoding='utf-8')
        fields = struct_fields(source, name)
        if fields is None:
            problems.append(f'machines.ini [{section}]: {file} declares no struct {name} (stale entry: fix or remove it)')
            continue
        for is_pub, field, _ in fields:
            if is_pub:
                problems.append(f"{file}::{name}.{field} is public: a machine's state is private and its one "
                                f"writer is transition; a new way to change it is a new event (docs/sans-io.md)")
        bools = sum(1 for _, _, t in fields if t == ['bool'])
        options = sum(1 for _, _, t in fields if t[:2] == ['Option', '<'])
        report.append(f'machine {section} ({entry.get("event", "?")} -> {entry.get("effect", "?")}): '
                      f'{len(fields)} fields, {bools} bool, {options} Option')
        values = {key: entry.get(key, '').strip() for key in strict_keys}
        if values['writer_policy'] != 'strict' or any(not values[k].isidentifier() for k in ('state_field', 'transition', 'constructor')):
            problems.append(f'machines.ini [{section}]: strict contract requires writer_policy = strict and nonempty state_field, transition and constructor identifiers')
            continue
        field, transition, constructor = (values[k] for k in ('state_field', 'transition', 'constructor'))
        if transition == constructor:
            problems.append(f'machines.ini [{section}]: transition and constructor must differ')
            continue
        if field not in {f for _, f, _ in fields}:
            problems.append(f'machines.ini [{section}]: no state field {field} (stale strict contract)')
            continue
        if transition != 'transition':
            problems.append(f'{section}: canonical method must be named transition')
        event, effect = entry.get('event', '').strip(), entry.get('effect', '').strip()
        if not event or not all(part.isidentifier() for part in event.split('::')) or not effect or not all(part.isidentifier() for part in effect.split('::')):
            problems.append(f'{section}: event/effect require exact Rust type paths (or effect = none)')
            continue
        from machine_readonly import Proof
        from types import SimpleNamespace
        proof = Proof(root, SimpleNamespace(production_tokens=production_tokens, ends_of=ends_of,
            shape=shape, struct_fields=struct_fields, item_bodies=item_bodies, inherent_target=inherent_target))
        if entry.get('state_reads') is not None and entry['state_reads'] != 'immutable':
            problems.append(f'{section}: state_reads must be immutable')
            continue
        if entry.get('state_reads') == 'immutable' and proof.generic_context(path, name) is None:
            problems.append(f'{section}: generic state bounds do not prove immutable identities')
        try:
            for child in production_sources(path, root):
                child_source = source if child == path else child.read_text(encoding='utf-8')
                problems.extend(writer_problems(child_source, name, field, transition, constructor,
                                                child.relative_to(root).as_posix(),
                                                require_methods=child == path, event=event, effect=effect, proof=proof, shared_reads=entry.get('state_reads') == 'immutable', owned_fields=fields, owner_path=path, aux_facts=aux_facts))
        except (OSError, ValueError) as error:
            problems.append(f'machines.ini [{section}]: cannot analyze production sources: {error}')
        if proof.audit:
            report.append(f'immutable state read audit {section}: ' + ', '.join(sorted(proof.audit)))
    return problems, report



def main():
    problems, report = check()
    for line in report: print(line)
    for problem in problems: print(f'check-machine-shape: {problem}', file=sys.stderr)
    if problems: return 1
    machines = sum(line.startswith('machine ') for line in report)
    print(f'check-machine-shape: ok ({machines} machine{"" if machines == 1 else "s"}; '
          f'flag counts are report-only)')
    return 0


if __name__ == '__main__':
    sys.exit(main())
