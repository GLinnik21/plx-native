#!/usr/bin/env python3
"""The User-Agent gate: one file applies the User-Agent, and no other names one.

Boot installs the process's single `User-Agent` with `net::set_user_agent` (which refuses a value
holding a control character), and every transport sends that value: the `net` easy path, the
player's HTTPS source (`curlio`) and the plaintext stream. Each of those once named its own, and a
fourth that does would put a different client name on the wire for the same process. The values are
applied in ONE place, `rust-modules/net/src/net.rs` (`apply_user_agent` for a curl handle,
`user_agent_header_line` for hand-built request text); the rest call those.

This fails when, in non-test Rust under `rust-modules/*/src` and `rust-modules/src`, or in `src/**/*.c`
and `*.h`, any file but that one

  * names `CURLOPT_USERAGENT`, or
  * holds a string literal that is a `User-Agent` header: it begins with `User-Agent`, or has it
    after a line break, followed by `:` or the end of the string (any case).

Comments are ignored, so is test code (whole test files and every `#[cfg(test)]` item, exactly as
`ci/check-list-caps.py` reads them), and so is prose that merely mentions the header in a longer string.
It also fails when the allowed file no longer names `CURLOPT_USERAGENT`: the allow-list is one file
and must stay the place that applies the value, not a stale entry. Add no second file here; call the
helpers in `net.rs`.

    ci/check-user-agent.py            # the gate (check-python-steps.txt runs it)
"""
from pathlib import Path
import importlib.util
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_test_modules  # noqa: E402

_spec = importlib.util.spec_from_file_location('check_list_caps', Path(__file__).with_name('check-list-caps.py'))
_caps = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_caps)
production_files, src_roots, without_test_items = _caps.production_files, _caps.src_roots, _caps.without_test_items

REPO = Path(__file__).resolve().parent.parent
ALLOWED = 'rust-modules/net/src/net.rs'
OPTION = 'CURLOPT_USERAGENT'
# A string literal that is the header: at its start or after a line break (`\n` still escaped, as the
# Rust lexer leaves it, or a real one), then `User-Agent` and `:` or the end.
HEADER = re.compile(r'(?:\A|\n|\\n)\s*user-agent\s*(?::|\Z)', re.IGNORECASE)
# What a C file is made of for this gate: a comment, a string, a character, or a word.
C_TOKEN = re.compile(r'//[^\n]*|/\*.*?\*/|"(?:\\.|[^"\\\n])*"|\'(?:\\.|[^\'\\\n])*\'|[A-Za-z_][A-Za-z_0-9]*', re.DOTALL)


def rust_uses(text):
    """The reasons a Rust source names its own User-Agent."""
    found = set()
    for kind, value in without_test_items(rust_test_modules.lex(text)):
        if kind == 'code' and value == OPTION: found.add(OPTION)
        elif kind == 'string' and HEADER.search(value): found.add('a "User-Agent" header literal')
    return sorted(found)


def c_uses(text):
    found = set()
    for m in C_TOKEN.finditer(text):
        token = m[0]
        if token == OPTION: found.add(OPTION)
        elif token[0] == '"' and HEADER.search(token[1:-1]): found.add('a "User-Agent" header literal')
    return sorted(found)


def sources(repo):
    """(path relative to the repo, reader) for every file the gate reads."""
    modules = repo / 'rust-modules'
    if modules.is_dir():
        for root in src_roots(modules):
            for path in production_files(root):
                yield path.relative_to(repo).as_posix(), rust_uses
    c_root = repo / 'src'
    if c_root.is_dir():
        for path in sorted(c_root.rglob('*')):
            if path.is_file() and path.suffix in ('.c', '.h'):
                yield path.relative_to(repo).as_posix(), c_uses


def check(repo=REPO):
    """[problem]: every file but ALLOWED that names its own User-Agent, and a stale allow-list."""
    problems, applies = [], False
    for rel, reader in sources(repo):
        uses = reader((repo / rel).read_text())
        if rel == ALLOWED:
            applies = OPTION in uses
        elif uses:
            problems.append(f'{rel} names its own User-Agent ({", ".join(uses)}). The process sends one, '
                            f'installed at boot: apply it with `net::apply_user_agent` (a curl handle) or '
                            f'`net::user_agent_header_line` (hand-built request text) from {ALLOWED}.')
    if not applies:
        problems.append(f'the allow-list names {ALLOWED}, which does not name {OPTION} (moved or gone): '
                        'point ALLOWED in ci/check-user-agent.py at the one file that applies the User-Agent.')
    return problems


def main():
    problems = check()
    if problems:
        print('check-user-agent: FAIL')
        for problem in problems: print(f'  {problem}')
        return 1
    print(f'check-user-agent: ok ({ALLOWED} is the one place that applies the User-Agent)')
    return 0


if __name__ == '__main__':
    sys.exit(main())
