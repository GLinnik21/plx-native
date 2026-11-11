#!/usr/bin/env python3
"""Registry-backed machine privacy, opt-in writer checks and enum-lint policy.

All listed machines retain the private-fields gate and report-only bool/Option counts. Explicit
strict contracts add a conservative lexical single-writer check; see machines.ini for its limits.
Listed enum-policy modules must carry the Clippy deny; Clippy classifies the matched types.
"""
from pathlib import Path
import configparser
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_test_modules as rtm  # noqa: E402

REPO = Path(__file__).resolve().parent.parent
MODULES = REPO / 'rust-modules'
INI = REPO / 'ci' / 'machines.ini'
OPEN, SHUT = '([{<', ')]}>'


def struct_fields(source, name):
    """[(is_pub, field name or index, type tokens)] of the braced or tuple struct `name`, or None
    when the source declares no such struct. Comments and strings are gone after lexing."""
    tokens = rust_tokens(source)
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
            fields.append(current); current = []
            continue
        depth += (token in OPEN) - (token in SHUT and tokens[k - 1] != '-')
        current.append(token)
    if current: fields.append(current)
    return [shape(index, field) for index, field in enumerate(fields)]


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


def rust_tokens(source):
    """Code tokens with raw identifiers normalized; literals cannot join adjacent code."""
    raw = [text if kind == 'code' else '<literal>' for kind, text in rtm.lex(source)]
    out, i = [], 0
    while i < len(raw):
        if raw[i:i + 2] == ['r', '#'] and i + 2 < len(raw):
            out.append(raw[i + 2]); i += 3
        else:
            out.append(raw[i]); i += 1
    return out


def ends_of(tokens):
    return rtm.group_ends([('code', token) for token in tokens])


def production_tokens(source):
    """Erase items proven cfg(test)-only, retaining token positions for remaining groups."""
    tokens = rust_tokens(source)
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
            if tokens[j] == ';':
                j += 1; break
            if tokens[j] in ('(', '[') and j in ends:
                j = ends[j] + 1; continue
            j += 1
        tokens[i:j] = ['<test>'] * (j - i)
        i = j
    return tokens


def item_bodies(tokens):
    """Return inherent impl and function spans; nested functions remain separate."""
    ends = ends_of(tokens)
    impls, functions = [], []
    for i, token in enumerate(tokens):
        if token not in ('impl', 'fn'): continue
        j = i + 1
        if token == 'fn':
            if j >= len(tokens): continue
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
    if any(tokens[j:j + 3] == ['macro_rules', '!', macro] for j in range(len(tokens) - 2)):
        return False
    for j, token in enumerate(tokens):
        if token == 'use':
            end = j + 1
            while end < len(tokens) and tokens[end] != ';': end += 1
            if macro in tokens[j + 1:end] or '*' in tokens[j + 1:end]: return False
    return True


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
        imported = False
        for j, token in enumerate(tokens):
            if token == 'use':
                end = j + 1
                while end < len(tokens) and tokens[end] != ';': end += 1
                if enum_name in tokens[j + 1:end] or '*' in tokens[j + 1:end]:
                    imported = True
                    break
        if declarations == 1 and not imported and variants and all(
                t == ',' or (t.isidentifier() and t[0].isupper()) for t in variants):
            safe.add(enum_name)
    return safe


def writer_problems(source, name, field, transition, constructor, label, require_methods=True):
    tokens = production_tokens(source)
    impls, functions, ends = item_bodies(tokens)
    owned = [(a, b) for a, b, h in impls if inherent_target(h, name)]

    def context(index):
        containing = [(a, b, n) for a, b, n, _ in functions if a < index < b]
        if not containing: return None
        a, b, method = min(containing, key=lambda x: x[1] - x[0])
        parents = [(x, y) for x, y, _, _ in functions if x < a < y]
        return method if not parents and any(x < a < b < y for x, y in owned) else None

    problems = []
    fields = struct_fields(source, name) or []
    field_types = [typ for _, field_name, typ in fields if field_name == field]
    plain_struct = any(tokens[j:j + 3] == ['struct', name, '{'] for j in range(len(tokens) - 2))
    copy_field = (plain_struct and len(field_types) == 1 and len(field_types[0]) == 1
                  and field_types[0][0] in copy_enum_types(tokens))
    safe_matches = {i: span for i, token in enumerate(tokens) if token == '!'
                    and (span := readonly_match(tokens, i, ends, field)) is not None}
    # These wrappers consume only the bool result, with no formatting expressions.
    safe_asserts = set()
    for i, token in enumerate(tokens):
        if token != '!' or i + 1 not in ends: continue
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
    for i in range(len(tokens) - 6):
        if tokens[i:i + 6] != ['const', constructor, ':', 'Self', '=', 'Self']:
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

    for i, token in enumerate(tokens):
        owner = context(i)
        if token == 'self' and owner is not None:
            if tokens[i + 1:i + 2] != ['.'] and not any(start == i for start, _ in safe_matches.values()):
                problems.append(f'{location(i)}: machine reference may escape the authorized writer')
        if token == field and i and tokens[i - 1] == '.':
            start = i - 1
            while start > 0 and tokens[start - 1] not in (';', '{', '}'):
                start -= 1
            prefix = tokens[start:i]
            # A shared reference can also escape and mutate interior-mutable state later.
            borrowed = ('&' in prefix or
                        any(prefix[j:j + 2] == ['ref', 'mut'] for j in range(len(prefix) - 1)))
            enclosing = [(a, b, sig) for a, b, _, sig in functions if a < i < b]
            signature = min(enclosing, key=lambda x: x[1] - x[0])[2] if enclosing else []
            arrow = next((j for j in range(len(signature) - 1) if signature[j:j + 2] == ['-', '>']), len(signature))
            result = signature[arrow + 2:]
            reference_result = '&' in result or 'DerefMut' in result
            forwarded = '=' in prefix or '(' in prefix
            matched_read = any(start + 2 == i and comma == i + 1 for start, comma in safe_matches.values())
            copied_argument = copy_argument(i, prefix)
            if (reference_result or (not matched_read and (borrowed or 'addr_of_mut' in prefix or
                    (owner != transition and ((forwarded and not copied_argument) or '!' in prefix or mutation_in_tail(tokens[i + 1:])))))):
                problems.append(f'{location(i)}: state projection may mutate or escape the authorized writer')
        # Destructuring may bind by reference; constructing a machine is restricted to its constructor.
        if token in (name, 'Self') and i + 1 in ends and tokens[i + 1] == '{':
            if i and tokens[i - 1] in ('struct', 'enum', 'impl'): continue
            if any(a == i + 1 for a, _, _, _ in functions) or any(a == i + 1 for a, _, _ in impls): continue
            stop = ends[i + 1]
            pattern = tokens[stop + 1:stop + 2] == ['='] or tokens[stop + 1:stop + 3] == ['=', '>']
            if field in tokens[i + 2:stop] and ((pattern and owner != transition) or (not pattern and owner != constructor and not (require_methods and any(i == start for start, _, _ in constants)))):
                problems.append(f'{location(i)}: state construction/destructuring outside {constructor} or {transition}')
        if token == '!' and i + 1 in ends and owner != transition:
            args = tokens[i + 2:ends[i + 1]]
            if ('self' in args or field in args) and i not in safe_matches and i not in safe_asserts:
                problems.append(f'{location(i)}: macro may hide a state mutation outside {transition}')
        if token == '*' and tokens[i + 1:i + 3] == ['self', '='] and owner != transition:
            problems.append(f'{location(i)}: replacement of self outside {transition}')
    return problems


def wildcard_problems(source, label):
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
    problems = [] if found else [f'{label}: module must have #![deny(clippy::wildcard_enum_match_arm)]']
    for i in range(len(tokens) - 2):
        bracket = i + 2 if tokens[i:i + 3] == ['#', '!', '['] else i + 1
        if tokens[i] != '#' or bracket not in ends or tokens[bracket] != '[': continue
        attr = tokens[bracket + 1:ends[bracket]]
        if ('allow' in attr or 'expect' in attr) and any(x in attr for x in ('wildcard_enum_match_arm', 'warnings', 'all')):
            problems.append(f'{label}: wildcard enum lint may not be locally suppressed')
    return problems


def production_sources(path, root):
    """Registered module and its production-reachable child and include files."""
    pending, seen = [path.resolve()], set()
    while pending:
        current = pending.pop()
        if not current.is_relative_to(root):
            raise ValueError(f'production include escapes rust-modules: {current}')
        if current in seen: continue
        seen.add(current)
        yield current
        pending.extend(target for target, test in rtm.file_edges(current) if not test)


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
            problems.extend(wildcard_problems(path.read_text(encoding='utf-8'), f'module {file}'))
            report.append(f'enum policy {file}: clippy::wildcard_enum_match_arm deny')
            continue
        unknown = set(entry) - {'event', 'effect', 'step'} - strict_keys
        if unknown:
            problems.append(f'machines.ini [{section}]: unknown keys: {", ".join(sorted(unknown))}')
        source = path.read_text(encoding='utf-8')
        fields = struct_fields(source, name)
        if fields is None:
            problems.append(f'machines.ini [{section}]: {file} declares no struct {name} (stale entry: fix or remove it)')
            continue
        for is_pub, field, _ in fields:
            if is_pub:
                problems.append(f"{file}::{name}.{field} is public: a machine's state is private and its one "
                                f"writer is step; a new way to change it is a new event (docs/sans-io.md)")
        bools = sum(1 for _, _, t in fields if t == ['bool'])
        options = sum(1 for _, _, t in fields if t[:2] == ['Option', '<'])
        report.append(f'machine {section} ({entry.get("event", "?")} -> {entry.get("effect", "?")}): '
                      f'{len(fields)} fields, {bools} bool, {options} Option')
        if strict_keys & set(entry):
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
            try:
                for child in production_sources(path, root):
                    child_source = source if child == path else child.read_text(encoding='utf-8')
                    problems.extend(writer_problems(child_source, name, field, transition, constructor,
                                                    child.relative_to(root).as_posix(),
                                                    require_methods=child == path))
            except (OSError, ValueError) as error:
                problems.append(f'machines.ini [{section}]: cannot analyze production sources: {error}')
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
