#!/usr/bin/env python3
"""The machine-shape gate: every state machine listed in `ci/machines.ini` keeps its state private.

A Sans-IO machine has one writer, `step(&mut self, event, &mut effects)`; its state is private so a
caller cannot poke it, and a new way in is a new event (docs/sans-io.md). This gate makes that a
fact the build checks instead of a habit:

  * FAILS on any `pub`, `pub(crate)`, `pub(super)` or `pub(in ..)` field of a listed struct, and on a
    section whose file or struct is gone, so the registry cannot go stale.
  * REPORTS, never fails: one line per machine with the number of `bool` and `Option<..>` fields.
    A pile of flags and optionals is how a state machine decays into a bag of booleans; the count is
    there to be read in review. It is a report-only ratchet: nothing here turns it red.

    ci/check-machine-shape.py
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
    tokens = [text for kind, text in rtm.lex(source) if kind == 'code']
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


def check(modules=MODULES, ini=INI):
    """([problem], [report line]) for every machine in the registry."""
    parser = configparser.ConfigParser(interpolation=None, comment_prefixes=('#',), inline_comment_prefixes=None)
    try:
        with open(ini, encoding='utf-8') as f:
            parser.read_file(f)
    except (OSError, configparser.Error) as error:
        return [f'{Path(ini).name} does not parse: {error}'], []
    problems, report = [], []
    for section in parser.sections():
        file, _, name = section.partition('::')
        path = Path(modules) / file
        if not name or not path.is_file():
            problems.append(f'machines.ini [{section}]: no such file under rust-modules/ (stale entry: fix or remove it)')
            continue
        fields = struct_fields(path.read_text(), name)
        if fields is None:
            problems.append(f'machines.ini [{section}]: {file} declares no struct {name} (stale entry: fix or remove it)')
            continue
        for is_pub, field, _ in fields:
            if is_pub:
                problems.append(f'{file}::{name}.{field} is public: a machine\'s state is private and its one '
                                f'writer is `step`; a new way to change it is a new event (docs/sans-io.md)')
        bools = sum(1 for _, _, t in fields if t == ['bool'])
        options = sum(1 for _, _, t in fields if t[:2] == ['Option', '<'])
        entry = parser[section]
        report.append(f'machine {section} ({entry.get("event", "?")} -> {entry.get("effect", "?")}): '
                      f'{len(fields)} fields, {bools} bool, {options} Option')
    return problems, report


def main():
    problems, report = check()
    for line in report: print(line)
    for problem in problems: print(f'check-machine-shape: {problem}', file=sys.stderr)
    if problems: return 1
    print(f'check-machine-shape: ok ({len(report)} machine{"" if len(report) == 1 else "s"}; '
          f'flag counts are report-only)')
    return 0


if __name__ == '__main__':
    sys.exit(main())
