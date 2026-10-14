#!/usr/bin/env python3
"""ci/check-user-agent.py's verdicts on synthetic trees: the one allowed file applies the User-Agent
and everything else is clean; a second `CURLOPT_USERAGENT`, a literal `User-Agent` header (any case,
in a string or a raw string, at the start of a request line too), in Rust or C, fails and names the
file; comments, tests and the identity function that only BUILDS the string do not; and an
allow-list entry that no longer applies anything is itself a failure. Each case writes a tiny tree
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
OTHER = 'rust-modules/media/src/curlio.rs'
CLEAN = {
    ALLOWED: 'const CURLOPT_USERAGENT: c_int = 10018;\n'
             'fn line() -> String { format!("User-Agent: {}\\r\\n", ua()) }\n',
    'rust-modules/plex/src/plex/identity.rs': '/// The HTTP `User-Agent` for libcurl.\n'
                                              'pub fn user_agent() -> String { format!("PlxNative/{}", V) }\n',
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


if __name__ == '__main__':
    unittest.main()
