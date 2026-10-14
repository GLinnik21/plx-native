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

One more file may hold the header's name, because it REFUSES it: `rust-modules/net/src/net/headers.rs`
lists `user-agent` among the headers a caller's block may not name (libcurl lets a custom
`User-Agent:` line replace the installed one; the plaintext stream would send both). It may not name
`CURLOPT_USERAGENT`, and it must still hold that literal: a header module that stopped refusing the
name would reopen the bypass the one applier closes.

The other direction, a transport that FORGETS to send the value, is read the same way, per file and
in the same non-test code: a file may not call `curl_easy_init` more often than it calls
`apply_user_agent` (each easy handle needs its own apply, and `net.rs` is held to it as well), and may
not hold more hand-built request lines (a string with `... HTTP/1.1` ending the line, the plaintext
stream's) than it calls `user_agent_header_line`. A C file may not open a curl handle at all: it has no
helper to apply the value with, so a request belongs in `plx_net`. This is a count, not a proof that the
call is on the path every request takes; the wire tests (`net_user_agent_tests.rs`,
`stream_user_agent_tests.rs`, `curlio_redirect_tests.rs`) are what prove the value reaches the socket.
A libavformat open of a URL (its own HTTP, its own name) is not visible to this gate; the only one is the
devtriggers-only `ffprobe` probe in `media/src/ff.rs`.

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
REFUSES = 'rust-modules/net/src/net/headers.rs'
OPTION = 'CURLOPT_USERAGENT'
HEADER_USE = 'a "User-Agent" header literal'
# A string literal that is the header: at its start or after a line break (`\n` still escaped, as the
# Rust lexer leaves it, or a real one), then `User-Agent` and `:` or the end.
HEADER = re.compile(r'(?:\A|\n|\\n)\s*user-agent\s*(?::|\Z)', re.IGNORECASE)
# A hand-built request line inside a string: whitespace, `HTTP/1.x`, then the line's end (`\\r\\n`, escaped as
# the lexer leaves it, or a real one). A status line begins the string and is followed by ` 200`, so it
# is not one.
REQUEST_LINE = re.compile(r'\sHTTP/1\.[01](?:\\r)?(?:\\n|\n)')
INIT = 'curl_easy_init'
APPLY = 'apply_user_agent'
LINE = 'user_agent_header_line'
# What a C file is made of for this gate: a comment, a string, a character, or a word.
C_TOKEN = re.compile(r'//[^\n]*|/\*.*?\*/|"(?:\\.|[^"\\\n])*"|\'(?:\\.|[^\'\\\n])*\'|[A-Za-z_][A-Za-z_0-9]*', re.DOTALL)


def calls(tokens, name):
    """How many times `name` is CALLED in a lexed Rust source: the word, then `(`, and not `fn name`."""
    return sum(1 for i, (kind, value) in enumerate(tokens)
               if kind == 'code' and value == name and i + 1 < len(tokens) and tokens[i + 1] == ('code', '(')
               and not (i and tokens[i - 1] == ('code', 'fn')))


def rust_scan(text):
    """(the reasons a Rust source names its own User-Agent, the reasons it forgets to send one)."""
    found, tokens = set(), without_test_items(rust_test_modules.lex(text))
    for kind, value in tokens:
        if kind == 'code' and value == OPTION: found.add(OPTION)
        elif kind == 'string' and HEADER.search(value): found.add(HEADER_USE)
    gaps = []
    handles, applies = calls(tokens, INIT), calls(tokens, APPLY)
    if applies < handles:
        gaps.append(f'it calls {INIT} {handles} time(s) but {APPLY} {applies}: every curl easy handle applies the User-Agent')
    lines = sum(1 for kind, value in tokens if kind == 'string' and REQUEST_LINE.search(value))
    sent = calls(tokens, LINE)
    if sent < lines:
        gaps.append(f'it builds {lines} HTTP request line(s) by hand but calls {LINE} {sent} time(s): every hand-built request sends the User-Agent')
    return sorted(found), gaps


def c_scan(text):
    found, gaps = set(), []
    for m in C_TOKEN.finditer(text):
        token = m[0]
        if token == OPTION: found.add(OPTION)
        elif token[0] == '"' and HEADER.search(token[1:-1]): found.add(HEADER_USE)
        elif token == INIT and re.match(r'\s*\(', text[m.end():]):
            gaps.append(f'it calls {INIT}: C has no helper that applies the User-Agent, so the request belongs in plx_net')
    return sorted(found), gaps[:1]


def sources(repo):
    """(path relative to the repo, reader) for every file the gate reads."""
    modules = repo / 'rust-modules'
    if modules.is_dir():
        for root in src_roots(modules):
            for path in production_files(root):
                yield path.relative_to(repo).as_posix(), rust_scan
    c_root = repo / 'src'
    if c_root.is_dir():
        for path in sorted(c_root.rglob('*')):
            if path.is_file() and path.suffix in ('.c', '.h'):
                yield path.relative_to(repo).as_posix(), c_scan


def check(repo=REPO):
    """[problem]: every file but ALLOWED that names its own User-Agent, a file that opens a transport
    without sending it, and a stale allow-list."""
    problems, applies, refuses = [], False, False
    for rel, reader in sources(repo):
        uses, gaps = reader((repo / rel).read_text())
        if rel == ALLOWED:
            applies = OPTION in uses
        elif rel == REFUSES:
            refuses = HEADER_USE in uses
            uses = [use for use in uses if use != HEADER_USE]
            if uses:
                problems.append(f'{rel} names its own User-Agent ({", ".join(uses)}); it may only REFUSE the header by name.')
        elif uses:
            problems.append(f'{rel} names its own User-Agent ({", ".join(uses)}). The process sends one, '
                            f'installed at boot: apply it with `net::apply_user_agent` (a curl handle) or '
                            f'`net::user_agent_header_line` (hand-built request text) from {ALLOWED}.')
        for gap in gaps if rel == ALLOWED or not uses else ():  # a file already named above says it once
            problems.append(f'{rel} sends no User-Agent: {gap}. Call `net::apply_user_agent` (a curl handle) or '
                            f'`net::user_agent_header_line` (hand-built request text) from {ALLOWED}.')
    if not refuses:
        problems.append(f'{REFUSES} no longer names the User-Agent header (moved or gone): the caller-header check must keep '
                        'refusing a `User-Agent:` line, and ALLOW-listing it here is only for that.')
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
