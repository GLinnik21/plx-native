#!/usr/bin/env python3
"""ci/check-plex-requests.py's verdicts on synthetic plex trees: each failure mode, the resolution
rules (a door's own body, the impl a method door belongs to, cfg(test) items and files, `via`), and
the evidence checks. Each case writes a tiny tree to a private temp dir; nothing here reads
rust-modules/ or the real registry."""
import importlib.util
import tempfile
import unittest
from pathlib import Path

_spec = importlib.util.spec_from_file_location('check_plex_requests',
                                               Path(__file__).with_name('check-plex-requests.py'))
gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gate)

PLEX = 'rust-modules/plex/src/plex'

# The server client's door, its plumbing, a paged listing that builds its window in a helper, a
# plain site, and the plex.tv client's method door with a plain transport call beside it.
BASE = {
    f'{PLEX}/paging.rs': (
        'pub struct PageReq { pub start: i64, pub size: i64 }\n'
        'pub fn paged_path(path: &str, req: PageReq) -> String {\n'
        '    format!("{path}?start={}", req.start)\n'
        '}\n'
    ),
    f'{PLEX}/client.rs': (
        'impl Client {\n'
        '    pub(super) fn get_json(&self, path: &str) -> Option<Container> {\n'
        '        self.get_json_with_headers(path, &[])\n'
        '    }\n'
        '    pub(super) fn get_json_with_headers(&self, path: &str, headers: &[&str]) -> Option<Container> {\n'
        '        let body = self.send_bulk(path, headers)?;\n'
        '        parse(body)\n'
        '    }\n'
        '    fn send_bulk(&self, path: &str, headers: &[&str]) -> Option<Vec<u8>> {\n'
        '        http::request_bulk(path, headers)\n'
        '    }\n'
        '}\n'
    ),
    f'{PLEX}/hubs.rs': (
        'impl Client {\n'
        '    pub fn hub_items(&self, key: &str, start: i64) -> Option<Container> {\n'
        '        self.get_json(&hub_page_path(key, start))\n'
        '    }\n'
        '    pub fn home(&self) -> Option<Container> {\n'
        '        self.get_json("/hubs")\n'
        '    }\n'
        '}\n'
        'fn hub_page_path(key: &str, start: i64) -> String {\n'
        '    paged_path(key, PageReq { start, size: 24 })\n'
        '}\n'
    ),
    f'{PLEX}/account.rs': (
        'impl AccountClient {\n'
        '    fn get(&self, url: &str) -> Option<Vec<u8>> {\n'
        '        self.get_raw(url)\n'
        '    }\n'
        '    fn get_raw(&self, url: &str) -> Option<Vec<u8>> {\n'
        '        plx_net::net::request_evidence(url, "GET")\n'
        '    }\n'
        '    pub fn users(&self) -> Option<Vec<u8>> {\n'
        '        self.get("https://plex.tv/users")\n'
        '    }\n'
        '}\n'
    ),
    'docs/pms-api.md': (
        '# PMS API\n'
        '## Paging, observed\n'
        '| Endpoint | Pages? |\n'
        '| `/hubs/search` with a window | no | not per hub |\n'
        '## Next section\n'
        '| `/elsewhere` | not this one |\n'
    ),
}

BASE_INI = (
    '[hubs.rs::hub_items]\n'
    'class = paged\n'
    'via = hub_page_path\n'
    'reason = Windowed by hub_page_path through paged_path.\n'
    f'evidence = {PLEX}/hubs.rs:2\n'
    '\n'
    '[hubs.rs::home]\n'
    'class = whole\n'
    'reason = The hub list, one response.\n'
    f'evidence = {PLEX}/hubs.rs:6\n'
    '\n'
    '[account.rs::users]\n'
    'class = whole\n'
    'reason = The user list, one response.\n'
    'evidence = docs/pms-api.md: | `/hubs/search` with a window | no | not per hub |\n'
)


class Tree:
    """A synthetic repo: `files` written under a private temp dir, and the registry beside them."""

    def __init__(self, files, ini=BASE_INI):
        self._temp = tempfile.TemporaryDirectory(prefix='plex-requests-')
        self.root = Path(self._temp.name).resolve() / 'repo'
        for name, source in {**BASE, **files}.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(source)
        self.ini = self.root / 'plex-requests.ini'
        self.ini.write_text(ini)

    def check(self):
        sites, problems = gate.check(repo=self.root, ini=self.ini)
        return {f'{file}::{name}' for file, name in sites}, problems

    def close(self):
        self._temp.cleanup()


class CheckPlexRequests(unittest.TestCase):
    def tree(self, files=None, ini=BASE_INI):
        tree = Tree(files or {}, ini)
        self.addCleanup(tree.close)
        return tree

    def assertProblem(self, problems, *fragments):
        matches = [p for p in problems if all(f in p for f in fragments)]
        self.assertTrue(matches, f'no problem holds {fragments!r} in {problems!r}')

    def test_clean_tree_passes_and_finds_exactly_the_sites(self):
        sites, problems = self.tree().check()
        self.assertEqual(problems, [])
        # Plumbing (get_json's body, send_bulk, get_raw) is not a site: the outer callers are.
        self.assertEqual(sites, {'hubs.rs::hub_items', 'hubs.rs::home', 'account.rs::users'})

    def test_unlisted_site_fails_and_names_the_section_to_add(self):
        tree = self.tree({f'{PLEX}/hubs.rs': BASE[f'{PLEX}/hubs.rs'].replace(
            'pub fn home(&self)', 'pub fn fresh(&self) -> Option<Container> {\n'
            '        self.get_json("/fresh")\n'
            '    }\n'
            '    pub fn home(&self)')})
        _, problems = tree.check()
        self.assertProblem(problems, 'hubs.rs::fresh', 'has no section', 'class = single | paged | bounded | whole')

    def test_stale_section_fails(self):
        ini = BASE_INI + '\n[hubs.rs::gone]\nclass = whole\nreason = Once.\nevidence = docs/pms-api.md: `/hubs/search` with a window\n'
        _, problems = self.tree(ini=ini).check()
        self.assertProblem(problems, '[hubs.rs::gone]', 'no longer exists')

    def test_unknown_class_fails(self):
        _, problems = self.tree(ini=BASE_INI.replace('class = whole', 'class = most')).check()
        self.assertProblem(problems, '[account.rs::users]', "class = 'most' is not one of")

    def test_first_page_is_refused(self):
        _, problems = self.tree(ini=BASE_INI.replace('class = whole', 'class = first-page')).check()
        self.assertProblem(problems, '[account.rs::users]', 'first-page is not allowed')

    def test_empty_reason_and_evidence_fail(self):
        ini = BASE_INI.replace('reason = The user list, one response.', 'reason =')
        ini = ini.replace('evidence = docs/pms-api.md: | `/hubs/search` with a window | no | not per hub |', 'evidence =')
        _, problems = self.tree(ini=ini).check()
        self.assertProblem(problems, '[account.rs::users]', 'empty reason')
        self.assertProblem(problems, '[account.rs::users]', 'empty evidence')

    def test_bounded_names_a_parameter_and_only_bounded_has_one(self):
        bounded = BASE_INI.replace('[account.rs::users]\nclass = whole', '[account.rs::users]\nclass = bounded')
        _, problems = self.tree(ini=bounded).check()
        self.assertProblem(problems, '[account.rs::users]', 'bounded but names no parameter')
        _, problems = self.tree(ini=bounded + 'parameter = limit\n').check()
        self.assertEqual(problems, [])
        _, problems = self.tree(ini=BASE_INI + 'parameter = limit\n').check()
        self.assertProblem(problems, '[account.rs::users]', 'has parameter, which only a bounded site names')

    def test_paged_needs_paging_in_its_body_or_a_via_function(self):
        _, problems = self.tree(ini=BASE_INI.replace('via = hub_page_path\n', '')).check()
        self.assertProblem(problems, '[hubs.rs::hub_items]', 'neither its function nor the via functions')

    def test_paged_in_its_own_body_needs_no_via(self):
        files = {f'{PLEX}/hubs.rs': BASE[f'{PLEX}/hubs.rs'].replace(
            '        self.get_json(&hub_page_path(key, start))',
            '        self.get_json(&paged_path(key, PageReq { start, size: 24 }))')}
        ini = BASE_INI.replace('via = hub_page_path\n', '')
        _, problems = self.tree(files, ini=ini).check()
        self.assertEqual(problems, [])

    def test_via_must_page_and_must_link_to_the_site(self):
        # A via function that never pages is refused, even when linked.
        files = {f'{PLEX}/hubs.rs': BASE[f'{PLEX}/hubs.rs'].replace(
            '    paged_path(key, PageReq { start, size: 24 })', '    key.to_string()')}
        _, problems = self.tree(files).check()
        self.assertProblem(problems, '[hubs.rs::hub_items]', 'hub_page_path, which never reaches paged_path')
        # A paging function the site neither calls nor is called by is refused.
        ini = BASE_INI.replace('via = hub_page_path', 'via = hub_items_other')
        files = {f'{PLEX}/hubs.rs': BASE[f'{PLEX}/hubs.rs'] + (
            'fn hub_items_other(key: &str) -> String {\n'
            '    paged_path(key, PageReq { start: 0, size: 1 })\n'
            '}\n')}
        _, problems = self.tree(files, ini=ini).check()
        self.assertProblem(problems, '[hubs.rs::hub_items]', 'hub_items_other, which neither calls hub_items')

    def test_via_only_for_paged(self):
        _, problems = self.tree(ini=BASE_INI.replace('class = whole', 'class = whole\nvia = hub_page_path')).check()
        self.assertProblem(problems, '[account.rs::users]', 'names via, which only a paged site names')

    def test_evidence_file_line_must_exist(self):
        ini = BASE_INI.replace(f'evidence = {PLEX}/hubs.rs:6', f'evidence = {PLEX}/hubs.rs:900')
        _, problems = self.tree(ini=ini).check()
        self.assertProblem(problems, '[hubs.rs::home]', 'past the end of a')
        ini = BASE_INI.replace(f'evidence = {PLEX}/hubs.rs:6', f'evidence = {PLEX}/nowhere.rs:1')
        _, problems = self.tree(ini=ini).check()
        self.assertProblem(problems, '[hubs.rs::home]', 'is not a file in the repo')

    def test_evidence_docs_line_must_be_in_the_paging_section(self):
        ini = BASE_INI.replace('| `/hubs/search` with a window | no | not per hub |', '| `/elsewhere` |')
        _, problems = self.tree(ini=ini).check()
        self.assertProblem(problems, '[account.rs::users]', 'is not a line of docs/pms-api.md')

    def test_unknown_key_fails(self):
        _, problems = self.tree(ini=BASE_INI + 'owner = someone\n').check()
        self.assertProblem(problems, '[account.rs::users]', "unknown key(s) ['owner']")

    def test_a_free_transport_call_is_a_site(self):
        # Direct plx_net calls in a non-door function bypass the doors; they must be classed too.
        files = {f'{PLEX}/account.rs': BASE[f'{PLEX}/account.rs'] + (
            'fn fetch_direct(url: &str) -> Option<Vec<u8>> {\n'
            '    plx_net::net::request_evidence(url, "GET")\n'
            '}\n')}
        sites, problems = self.tree(files).check()
        self.assertIn('account.rs::fetch_direct', sites)
        self.assertProblem(problems, 'account.rs::fetch_direct', 'has no section')

    def test_account_get_is_a_site_only_inside_account_client(self):
        files = {f'{PLEX}/other.rs': (
            'impl Other {\n'
            '    fn go(&self) -> Option<Vec<u8>> {\n'
            '        self.get("https://example.invalid")\n'
            '    }\n'
            '}\n')}
        sites, _ = self.tree(files).check()
        self.assertNotIn('other.rs::go', sites)

    def test_impl_for_a_trait_names_the_type_it_is_for(self):
        files = {f'{PLEX}/account.rs': BASE[f'{PLEX}/account.rs'] + (
            'impl Fetch for AccountClient {\n'
            '    fn fetch(&self) -> Option<Vec<u8>> {\n'
            '        self.get("https://plex.tv/trait")\n'
            '    }\n'
            '}\n')}
        sites, _ = self.tree(files).check()
        self.assertIn('account.rs::fetch', sites)

    def test_cfg_test_items_are_not_sites(self):
        files = {f'{PLEX}/hubs.rs': BASE[f'{PLEX}/hubs.rs'] + (
            '#[cfg(test)]\n'
            'mod tests {\n'
            '    fn probe(c: &Client) { c.get_json("/probe"); }\n'
            '}\n'
            '#[cfg(test)]\n'
            'fn helper(c: &Client) { c.get_json("/helper"); }\n')}
        sites, problems = self.tree(files).check()
        self.assertEqual(problems, [])
        self.assertEqual([s for s in sites if s.endswith(('::probe', '::helper'))], [])

    def test_a_wholly_test_file_is_not_a_site(self):
        files = {
            f'{PLEX}/mod.rs': '#[cfg(test)]\nmod client_tests;\n',
            f'{PLEX}/client_tests.rs': 'impl Client {\n    fn t(&self) { self.get_json("/t"); }\n}\n',
        }
        sites, problems = self.tree(files).check()
        self.assertEqual(problems, [])
        self.assertFalse([s for s in sites if s.startswith('client_tests.rs')])

    def test_array_type_in_a_signature_does_not_end_the_function(self):
        files = {f'{PLEX}/hubs.rs': BASE[f'{PLEX}/hubs.rs'] + (
            'impl Client {\n'
            '    pub fn sized(&self, buf: [u8; 4]) -> Option<Container> {\n'
            '        self.get_json("/sized")\n'
            '    }\n'
            '}\n')}
        sites, _ = self.tree(files).check()
        self.assertIn('hubs.rs::sized', sites)

    def test_a_site_inside_a_closure_belongs_to_its_function(self):
        files = {f'{PLEX}/hubs.rs': BASE[f'{PLEX}/hubs.rs'] + (
            'impl Client {\n'
            '    pub fn lazy(&self) -> impl Fn() -> Option<Container> + \'_ {\n'
            '        move || self.get_json("/lazy")\n'
            '    }\n'
            '}\n')}
        sites, _ = self.tree(files).check()
        self.assertIn('hubs.rs::lazy', sites)
        self.assertNotIn('hubs.rs::move', sites)


if __name__ == '__main__':
    unittest.main()
