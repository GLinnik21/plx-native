#!/usr/bin/env python3
"""Source-only regressions for the captured-input oracle; no Rust build required."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "captured_inputs_oracle",
    ROOT / "docs/design-briefs/verdict-data-captured-inputs-source-oracle.py")
ORACLE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ORACLE)
READ = "plx_plex::plex::session::server_ids()"


def adversarial(count):
    return "#[cfg(test)]#[" + "]#[" * count + "\nfn live() { " + READ + "; }"


def probe(count, timeout=2):
    start = time.perf_counter()
    try:
        result = subprocess.run(
            [sys.executable, str(Path(__file__).resolve()), "--adversarial", str(count)],
            capture_output=True, text=True, timeout=timeout, check=True)
    except subprocess.TimeoutExpired:
        return {"repetitions": count, "timeout_seconds": timeout,
                "elapsed_seconds": time.perf_counter() - start, "status": "timeout"}
    return {"repetitions": count, "timeout_seconds": timeout,
            "elapsed_seconds": time.perf_counter() - start, "status": "pass",
            "child": json.loads(result.stdout)}


class ProductionSourceTests(unittest.TestCase):
    def reads(self, source):
        return len(list(ORACLE.AMBIENT.finditer(ORACLE.production(source))))

    def test_test_items_do_not_mask_later_production(self):
        for item in ("fn helper()", "mod checks", "impl Helper"):
            with self.subTest(item=item):
                source = f"fn before() {{ {READ}; }} #[cfg(test)] {item} {{ {READ}; }} fn after() {{ {READ}; }}"
                self.assertEqual(self.reads(source), 2)

    def test_repeated_nested_attributes_and_visibility(self):
        for attrs in (
            '#[cfg(test)] #[allow(dead_code)] #[doc = "[}]"]',
            '#[allow(clippy::lint(reason = "nested [ ]"))] /* comment */ #[cfg(test)]',
            '#[cfg(any(test, feature = "test-support"))] #[allow(unused)]',
            '#[cfg(test)]' + '#[allow(dead_code)]' * 1000,
        ):
            with self.subTest(attrs=attrs[:100]):
                self.assertEqual(self.reads(f'{attrs} pub(crate) fn helper() {{ {READ}; }} fn live() {{ {READ}; }}'), 1)

    def test_balanced_body_ignores_strings_characters_and_nested_comments(self):
        source = r'''#[cfg(test)] mod checks {
            const RAW: &str = r###"} /* #[cfg(test)]"###;
            const TEXT: &str = "} \" {";
            const CHAR: char = '}';
            /* } /* nested } */ still } */
            fn helper() { if true { READ; } }
        }
        fn live() { READ; }'''.replace("READ", READ)
        self.assertEqual(self.reads(source), 1)

    def test_comments_and_literals_do_not_forge_test_ownership(self):
        for prefix in (
            '// #[cfg(test)] mod fake {}\n',
            '/* #[cfg(test)] /* nested */ mod fake {} */',
            'const EXAMPLE: &str = r#"#[cfg(test)] mod fake {}"#;',
            'const EXAMPLE: &str = "#[cfg(test)] mod fake {}";',
        ):
            with self.subTest(prefix=prefix):
                self.assertEqual(self.reads(prefix + f" fn live() {{ {READ}; }}"), 1)

    def test_non_test_cfg_and_unrelated_items_keep_real_reads(self):
        for prefix in ('#[cfg(not(test))]', '#[cfg(any(test, feature="real"))]',
                       '#[cfg(test)] use example;'):
            with self.subTest(prefix=prefix):
                self.assertEqual(self.reads(prefix + f" fn live() {{ {READ}; }}"), 1)

    def test_external_test_module_does_not_mask_following_item(self):
        self.assertEqual(self.reads(f"#[cfg(test)] mod checks; fn live() {{ {READ}; }}"), 1)

    def test_unterminated_test_body_fails_closed(self):
        self.assertEqual(self.reads(f"#[cfg(test)] fn broken() {{ {READ};"), 1)

    def test_all_ambient_families_still_detected(self):
        for call in (READ, "plx_plex::plex::client_for()", "crate::stores::tape::active()"):
            with self.subTest(call=call):
                self.assertEqual(self.reads(f"#[cfg(test)] mod checks {{}} fn live() {{ {call}; }}"), 1)

    def test_original_oracle_and_real_read_mutation(self):
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(ORACLE.main(), 0)
        with tempfile.TemporaryDirectory(prefix="captured-oracle-") as temp:
            root = Path(temp)
            for family in ORACLE.FAMILIES:
                dest = root / "rust-modules/data/src" / family
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_text((ROOT / "rust-modules/data/src" / family).read_text(encoding="utf-8"), encoding="utf-8")
            for family in ("search", "metadata", "pms", "viewstate"):
                dest = root / "ci/allow/globals" / f"data-{family}.txt"
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_text("")
            with (root / "rust-modules/data/src/search.rs").open("a", encoding="utf-8") as out:
                out.write(f"\n#[cfg(test)] mod oracle_checks {{}}\nfn real_ambient_read() {{ {READ}; }}\n")
            old_root = ORACLE.ROOT
            try:
                ORACLE.ROOT = root
                output = io.StringIO()
                with contextlib.redirect_stdout(output):
                    self.assertEqual(ORACLE.main(), 1)
                self.assertIn("search.rs: ambient machine read:", output.getvalue())
            finally:
                ORACLE.ROOT = old_root

    def test_long_malformed_attributes_finish_and_keep_real_read(self):
        for count in (32, 20000, 80000):
            with self.subTest(count=count):
                result = probe(count)
                self.assertEqual(result["status"], "pass", result)
                self.assertEqual(result["child"]["ambient_reads"], 1)


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--adversarial":
        source = adversarial(int(sys.argv[2]))
        start = time.perf_counter()
        result = ORACLE.production(source)
        print(json.dumps({"characters": len(source), "production_seconds": time.perf_counter() - start,
                          "ambient_reads": len(list(ORACLE.AMBIENT.finditer(result)))}))
    elif len(sys.argv) == 2 and sys.argv[1] == "--timings":
        print(json.dumps([probe(count) for count in (32, 20000, 80000)]))
    else:
        unittest.main()
