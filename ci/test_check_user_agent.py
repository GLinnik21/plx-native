#!/usr/bin/env python3
"""ci/check-user-agent.py's verdicts on synthetic trees: the one allowed file applies the User-Agent
and everything else is clean; a second `CURLOPT_USERAGENT`, a literal `User-Agent` header (any case,
in a string or a raw string, at the start of a request line too), in Rust or C, fails and names the
file; comments, tests and the identity function that only BUILDS the string do not; and an
allow-list entry that no longer applies anything is itself a failure. The other direction is read too:
a file that calls `curl_easy_init` more often than `apply_user_agent`, or builds more request lines by hand
than it calls `user_agent_header_line`, fails and names the file (the allowed file included), and a C file
that opens a curl handle fails; declarations, status lines, comments and test code do not count. Each case writes a tiny tree
to a private temp dir; nothing here reads the real repository."""
import importlib.util
import tempfile
import unittest
import weakref
from pathlib import Path

_spec = importlib.util.spec_from_file_location('check_user_agent', Path(__file__).with_name('check-user-agent.py'))
gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gate)

ALLOWED = gate.ALLOWED
REFUSES = gate.REFUSES
OTHER = 'rust-modules/media/src/curlio.rs'
CLEAN = {
    ALLOWED: 'const CURLOPT_USERAGENT: c_int = 10018;\n'
             'fn line() -> String { format!("User-Agent: {}\\r\\n", ua()) }\n',
    'rust-modules/plex/src/plex/identity.rs': '/// The HTTP `User-Agent` for libcurl.\n'
                                              'pub fn user_agent() -> String { format!("PlxNative/{}", V) }\n',
    REFUSES: 'pub const RESERVED: [&str; 2] = ["host", "user-agent"];\n',
    OTHER: 'fn open() { plx_net::net::apply_user_agent(easy); }\n',
    'src/starfish.c': '/* sets the User-Agent: nothing here */ int x(void) { return 0; }\n',
}


class Tree:
    def __init__(self, files):
        self._temp = tempfile.TemporaryDirectory(prefix='user-agent-')
        weakref.finalize(self, self._temp.cleanup)
        self.repo = Path(self._temp.name).resolve()
        for name, source in files.items():
            path = self.repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(source)

    def problems(self):
        return gate.check(repo=self.repo)


def with_file(name, source):
    return Tree({**CLEAN, name: source})


class Verdicts(unittest.TestCase):
    def test_a_clean_tree_passes(self):
        self.assertEqual(Tree(CLEAN).problems(), [])

    def test_a_second_curlopt_useragent_constant_fails_and_names_the_file(self):
        problems = with_file(OTHER, 'const CURLOPT_USERAGENT: c_int = 10018;\n').problems()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn(OTHER, problems[0])
        self.assertIn('CURLOPT_USERAGENT', problems[0])

    def test_a_literal_user_agent_header_fails_in_any_spelling(self):
        for source in ('fn f() -> &\'static str { "User-Agent: plxnative/0.1\\r\\n" }',
                       'fn f() -> &\'static str { "user-agent:" }',
                       'fn f() { h.insert("USER-AGENT", v); }',
                       'fn f() { let r = format!("GET / HTTP/1.1\\r\\nUser-Agent: {ua}\\r\\n\\r\\n"); }',
                       'fn f() { let r = r#"GET / HTTP/1.1\nUser-Agent: x"#; }'):
            with self.subTest(source=source):
                problems = with_file(OTHER, source + '\n').problems()
                self.assertEqual(len(problems), 1, problems)
                self.assertIn(OTHER, problems[0])

    def test_a_c_file_naming_its_own_user_agent_fails(self):
        for source in ('int f(CURL *c) { return curl_easy_setopt(c, CURLOPT_USERAGENT, "x"); }',
                       'const char *h = "User-Agent: x\\r\\n";'):
            with self.subTest(source=source):
                problems = with_file('src/main.c', source + '\n').problems()
                self.assertEqual(len(problems), 1, problems)
                self.assertIn('src/main.c', problems[0])

    def test_prose_that_merely_mentions_the_header_is_not_a_header(self):
        for source in ('// CURLOPT_USERAGENT and "User-Agent: x" in a comment\nfn f() {}',
                       '/// the `User-Agent` for libcurl\nfn f() {}',
                       'fn f() { log("net: the client\'s User-Agent holds a control character"); }',
                       'fn f() { let user_agent = 1; }'):
            with self.subTest(source=source):
                self.assertEqual(with_file(OTHER, source + '\n').problems(), [])

    def test_a_c_comment_is_not_a_use(self):
        self.assertEqual(with_file('src/main.c', '/* CURLOPT_USERAGENT */ // "User-Agent: x"\nint x;\n').problems(), [])

    def test_tests_may_name_the_header(self):
        body = 'fn t() { assert!(req.contains("User-Agent: x")); }\n'
        self.assertEqual(with_file('rust-modules/media/src/curlio_tests.rs', body).problems(), [])
        self.assertEqual(with_file('rust-modules/media/src/tests.rs', body).problems(), [])
        self.assertEqual(with_file(OTHER, 'fn p() {}\n#[cfg(test)]\nmod tests { ' + body + '}\n').problems(), [])
        self.assertEqual(with_file(OTHER, 'fn p() {}\n#[cfg(test)]\n#[path = "x_tests.rs"]\nmod x;\n').problems(), [])

    def test_the_allowed_file_must_still_apply_the_user_agent(self):
        problems = Tree({**CLEAN, ALLOWED: 'fn nothing() {}\n'}).problems()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn(ALLOWED, problems[0])
        self.assertIn('allow-list', problems[0])
        missing = Tree({k: v for k, v in CLEAN.items() if k != ALLOWED}).problems()
        self.assertEqual(len(missing), 1, missing)

    def test_the_header_module_may_name_the_header_it_refuses_and_nothing_else(self):
        problems = Tree({**CLEAN, REFUSES: 'const R: [&str; 1] = ["user-agent"];\nconst O: c_int = CURLOPT_USERAGENT;\n'}).problems()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn(REFUSES, problems[0])
        self.assertIn('CURLOPT_USERAGENT', problems[0])

    def test_the_header_module_must_still_refuse_the_user_agent(self):
        problems = Tree({**CLEAN, REFUSES: 'pub const RESERVED: [&str; 1] = ["host"];\n'}).problems()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn(REFUSES, problems[0])
        gone = Tree({k: v for k, v in CLEAN.items() if k != REFUSES}).problems()
        self.assertEqual(len(gone), 1, gone)


HANDLE = 'let h = plx_net::net::curl_easy_init();\n'
REQUEST = 'let r = format!("{method} {path} HTTP/1.1\\r\\nHost: {h}\\r\\n{ua}\\r\\n");\n'


class ForgottenUserAgent(unittest.TestCase):
    """A transport that never sends the value: every easy handle applies it, every hand-built request
    line is followed by the helper's line."""

    def test_an_easy_handle_that_never_applies_the_user_agent_fails_and_names_the_file(self):
        problems = with_file(OTHER, 'fn open() { ' + HANDLE + ' }\n').problems()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn(OTHER, problems[0])
        self.assertIn('curl_easy_init', problems[0])
        self.assertIn('apply_user_agent', problems[0])

    def test_an_easy_handle_that_applies_it_passes(self):
        source = 'fn open() { ' + HANDLE + ' plx_net::net::apply_user_agent(h); }\n'
        self.assertEqual(with_file(OTHER, source).problems(), [])

    def test_every_handle_needs_its_own_apply(self):
        source = 'fn a() { ' + HANDLE + ' apply_user_agent(h); }\nfn b() { ' + HANDLE + ' }\n'
        problems = with_file(OTHER, source).problems()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn('2 time(s)', problems[0])

    def test_the_allowed_file_is_held_to_it_too(self):
        stale = CLEAN[ALLOWED] + 'fn open() { let h = curl_easy_init(); }\n'
        problems = Tree({**CLEAN, ALLOWED: stale}).problems()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn(ALLOWED, problems[0])
        fixed = stale + 'fn apply(easy: *mut CURL) { apply_user_agent(easy); }\n'
        self.assertEqual(Tree({**CLEAN, ALLOWED: fixed}).problems(), [])

    def test_a_declaration_a_path_a_comment_and_test_code_are_not_a_handle(self):
        for source in ('fn curl_easy_init() -> *mut CURL;',
                       'fn f() { let present = !plx_net::net::curl::curl_easy_init; }',
                       '// curl_easy_init() is called by the other file\nfn f() {}',
                       '#[cfg(test)]\nmod tests { fn t() { ' + HANDLE + ' } }',
                       'fn f() { log("curl_easy_init() failed"); }'):
            with self.subTest(source=source):
                self.assertEqual(with_file(OTHER, source + '\n').problems(), [])
        self.assertEqual(with_file('rust-modules/media/src/curlio_tests.rs', 'fn t() { ' + HANDLE + ' }\n').problems(), [])

    def test_a_hand_built_request_line_without_the_helper_fails(self):
        problems = with_file(OTHER, 'fn f() { ' + REQUEST + ' }\n').problems()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn(OTHER, problems[0])
        self.assertIn('user_agent_header_line', problems[0])
        for source in ('fn f() { let r = b"GET / HTTP/1.0\\r\\n\\r\\n"; }',
                       'fn f() { let r = "GET /a HTTP/1.1\\nHost: x\\n"; }'):
            with self.subTest(source=source):
                self.assertEqual(len(with_file(OTHER, source + '\n').problems()), 1)

    def test_a_hand_built_request_line_with_the_helper_passes(self):
        source = 'fn f() { let ua = crate::net::user_agent_header_line(); ' + REQUEST + ' }\n'
        self.assertEqual(with_file(OTHER, source).problems(), [])

    def test_status_lines_and_version_checks_are_not_request_lines(self):
        for source in ('fn f() { w("HTTP/1.1 200 OK\\r\\nContent-Length: 0\\r\\n\\r\\n"); }',
                       'fn f() { w(format!("HTTP/1.1 {status} Refused\\r\\n")); }',
                       'fn f() -> bool { h.starts_with(b"HTTP/1.1") }',
                       '#[cfg(test)]\nmod tests { fn t() { ' + REQUEST + ' } }'):
            with self.subTest(source=source):
                self.assertEqual(with_file(OTHER, source + '\n').problems(), [])

    def test_a_c_file_opening_a_curl_handle_fails_but_naming_it_in_a_comment_does_not(self):
        problems = with_file('src/main.c', 'int f(void) { CURL *c = curl_easy_init(); return c != 0; }\n').problems()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn('src/main.c', problems[0])
        self.assertEqual(with_file('src/main.c', '/* curl_easy_init() */ int f(void) { return 0; }\n').problems(), [])


if __name__ == '__main__':
    unittest.main()
