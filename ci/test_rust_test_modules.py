#!/usr/bin/env python3
import tempfile
import unittest
from pathlib import Path
from rust_test_modules import wholly_test_files

class TestModuleDiscovery(unittest.TestCase):
    def classify(self, files):
        with tempfile.TemporaryDirectory(prefix='rust-module-graph-') as temp:
            root = Path(temp).resolve()
            for name, source in files.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source)
            return {p.relative_to(root).as_posix() for p in wholly_test_files(root)}

    def test_path_attributes_order_comments_visibility_and_neutral_filename(self):
        for declaration in (
            '#[cfg(test)]\n#[path="support.rs"]\nmod checks;',
            '#[path = "support.rs"] /* between attributes */ #[cfg(test)] pub(crate) mod checks;',
            '#[cfg(test)] #[allow(dead_code)] #[path=r#"support.rs"#] pub mod checks;',
        ):
            with self.subTest(declaration=declaration):
                self.assertEqual(self.classify({'lib.rs': declaration, 'support.rs': 'fn support(){}'}), {'support.rs'})

    def test_production_path_reference_wins_over_test_reference(self):
        self.assertEqual(self.classify({
            'lib.rs': '#[cfg(test)] #[path="shared.rs"] mod checks; #[path="shared.rs"] mod production;',
            'shared.rs': 'fn run(){}',
        }), set())

    def test_plain_modules_and_descendants_inherit_test_ownership(self):
        self.assertEqual(self.classify({
            'lib.rs': '#[cfg(test)] mod checks;',
            'checks.rs': 'mod child; include!("extra.rs");',
            'checks/child.rs': 'fn child(){}',
            'extra.rs': 'fn extra(){}',
            'orphan_test_support.rs': 'fn production_until_proven_otherwise(){}',
        }), {'checks.rs', 'checks/child.rs', 'extra.rs'})

    def test_inline_test_module_include_and_nested_path(self):
        self.assertEqual(self.classify({
            'lib.rs': 'mod parent;',
            'parent.rs': '#[cfg(test)] mod checks { include!("extra.rs"); #[path="helper.rs"] mod helper; }',
            'extra.rs': 'fn extra(){}',
            'parent/checks/helper.rs': 'fn helper(){}',
        }), {'extra.rs', 'parent/checks/helper.rs'})

    def test_cfg_expression_must_require_test(self):
        declarations = {
            'all(test, feature="optional")': True,
            'all(feature="optional", test)': True,
            'any(test, feature="optional")': False,
            'not(test)': False,
            'not(not(test))': True,
        }
        for cfg, test_only in declarations.items():
            with self.subTest(cfg=cfg):
                files = {'lib.rs': f'#[cfg({cfg})] #[path="helper.rs"] mod helper;', 'helper.rs': ''}
                self.assertEqual(self.classify(files), {'helper.rs'} if test_only else set())

    def test_quoted_declarations_and_nested_comments_cannot_launder_production(self):
        source = r'''const EXAMPLE: &str = r###"#[cfg(test)] #[path="helper.rs"] mod helper;"###;
        /* #[cfg(test)] /* nested */ #[path="helper.rs"] mod helper; */
        const BRACE: char = '}';
        #[path="helper.rs"] mod real;'''
        self.assertEqual(self.classify({'lib.rs': source, 'helper.rs': ''}), set())

if __name__ == '__main__':
    unittest.main()
