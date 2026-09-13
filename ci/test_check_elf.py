#!/usr/bin/env python3
"""Exercise the real ELF gate with synthetic tools; no compiler, NDK, ELF or device needed."""
import concurrent.futures
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
HOOK = "_ZN17StarfishMediaAPIs20callbackFunctionHookEixPKc"
LOAD = "_ZN17StarfishMediaAPIs4LoadEPKcPFvixS1_PvES2_"
FILLER = "synthetic-padding-line\n" * 100000  # More than 2 MiB, far beyond a pipe buffer.

FAKE_TOOL = r'''#!/usr/bin/env python3
import json, os, signal, sys, time
from pathlib import Path
signal.signal(signal.SIGPIPE, signal.SIG_DFL)
tool = Path(sys.argv[0]).name
key = tool + (":" + sys.argv[1] if tool == "readelf" else "")
spec = json.loads(Path(os.environ["ELF_TEST_SPEC"]).read_text())
if key == "readelf:-h" and "ELF_TEST_BARRIER" in os.environ:
    barrier = Path(os.environ["ELF_TEST_BARRIER"])
    (barrier / ("ready-" + os.environ["ELF_TEST_RUN"])).touch()
    until = time.monotonic() + 5
    while len(list(barrier.glob("ready-*"))) < 2:
        if time.monotonic() > until: sys.exit(99)
        time.sleep(0.01)
    scratch = sorted(str(p) for p in Path(os.environ["TMPDIR"]).glob("check-elf.*"))
    (barrier / ("seen-" + os.environ["ELF_TEST_RUN"])).write_text(json.dumps(scratch))
record = spec[key]
data = record["output"].encode()
while data:
    count = os.write(1, data)
    data = data[count:]
sys.exit(record.get("exit", 0))
'''


def defaults():
    return {key: {"output": value} for key, value in {
        "readelf:-h": "Class: ELF32\nMachine: ARM\nFlags: soft-float\nType: EXEC\n",
        "readelf:-A": "Tag_CPU_arch: v7\n",
        "readelf:--dyn-syms": f"1: 00000000 16 FUNC GLOBAL DEFAULT 1 {HOOK}\n",
        "readelf:-rW": "No relocations\n",
        "readelf:-l": "  LOAD 0x000000 0x00010000 0x00010000\n",
        "readelf:-n": "Build ID: " + "a" * 40 + "\n",
        "readelf:-d": "Shared library: [libalpha.so]\nShared library: [libbeta.so]\n",
        "objdump": "  00: dmb ish\n" * 101,
        "strings": "YOUR_PMS_HOST\n",
    }.items()}


class ElfGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="elf-gate-tests-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "ci").mkdir()
        (self.root / "tools").mkdir()
        (self.root / "scratch").mkdir()
        source = (ROOT / "ci/check-elf.sh").read_text()
        # Baseline isolation ONLY: its absolute legacy scratch path must not clobber another
        # checkout. The fixed script has no such literal, so its copy is byte-for-byte unchanged.
        source = source.replace("/tmp/dt-needed.actual", str(self.root / "legacy-needed.actual"))
        self.script = self.root / "ci/check-elf.sh"
        self.script.write_text(source)
        (self.root / "ci/expected-dt-needed.txt").write_text("libalpha.so\nlibbeta.so\n")
        for name in ("readelf", "objdump", "strings"):
            path = self.root / "tools" / name
            path.write_text(FAKE_TOOL)
            path.chmod(0o755)
        self.spec = self.root / "spec.json"

    def run_gate(self, spec=None, extra_env=None):
        if spec is not None:
            self.spec.write_text(json.dumps(spec))
        env = os.environ.copy()
        env.update({"READELF": str(self.root / "tools/readelf"),
                    "OBJDUMP": str(self.root / "tools/objdump"),
                    "PATH": str(self.root / "tools") + os.pathsep + env.get("PATH", ""),
                    "TMPDIR": str(self.root / "scratch"), "CI": "true",
                    "ELF_TEST_SPEC": str(self.spec)})
        env.update(extra_env or {})
        return subprocess.run(["bash", str(self.script), "synthetic.elf"], cwd=self.root,
                              env=env, capture_output=True, text=True, timeout=20)

    def assert_pass(self, spec):
        result = self.run_gate(spec)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("all ELF assertions passed", result.stdout)

    def test_small_valid_artifact(self):
        self.assert_pass(defaults())

    def test_early_required_placeholder_with_large_tail(self):
        spec = defaults()
        spec["strings"]["output"] = "YOUR_PMS_HOST\n" + FILLER
        self.assert_pass(spec)

    def test_early_forbidden_strings_with_placeholder_at_eof(self):
        for forbidden, diagnostic in [("/home/synthetic_builder/work/file.rs", "build-host paths"),
                                      ("/Users/synthetic_builder/work/file.rs", "build-host paths"),
                                      ("10.23.45.67", "private IP address")]:
            with self.subTest(forbidden=forbidden):
                spec = defaults()
                spec["strings"]["output"] = forbidden + "\n" + FILLER + "YOUR_PMS_HOST\n"
                result = self.run_gate(spec)
                self.assertNotEqual(result.returncode, 0, "forbidden value was allowed: " + result.stdout)
                self.assertIn(diagnostic, result.stdout)

    def test_early_forbidden_relocation_with_large_tail(self):
        spec = defaults()
        spec["readelf:-rW"]["output"] = LOAD + "\n" + FILLER
        result = self.run_gate(spec)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("dynamic relocation", result.stdout)

    def test_large_metadata_outputs_still_pass(self):
        for key in ["readelf:-h", "readelf:-A", "readelf:--dyn-syms", "readelf:-n", "readelf:-d", "readelf:-l"]:
            with self.subTest(key=key):
                spec = defaults()
                if key == "readelf:-l":
                    spec[key]["output"] *= 70000  # Force sort's early-head SIGPIPE too.
                else:
                    spec[key]["output"] += FILLER
                self.assert_pass(spec)

    def test_producer_failure_is_never_success_even_after_valid_output(self):
        for key in defaults():
            with self.subTest(key=key):
                spec = defaults()
                spec[key]["exit"] = 7
                spec[key]["output"] += FILLER
                result = self.run_gate(spec)
                self.assertNotEqual(result.returncode, 0, "producer failed but gate passed: " + key)
                self.assertEqual(list((self.root / "scratch").iterdir()), [])

    def test_predicates_still_reject_invalid_artifacts(self):
        for key, replacement in [("readelf:-A", "Tag_CPU_arch: v6\n"),
                                 ("readelf:-n", "Build ID: abc\n"),
                                 ("readelf:-d", "Shared library: [unexpected.so]\n"),
                                 ("readelf:--dyn-syms", ""), ("strings", "no placeholder\n"),
                                 ("objdump", "  00: dmb ish\n")]:
            with self.subTest(key=key):
                spec = defaults()
                spec[key]["output"] = replacement
                result = self.run_gate(spec)
                self.assertNotEqual(result.returncode, 0, key)

    def test_parallel_invocations_have_private_scratch_and_remove_it(self):
        self.spec.write_text(json.dumps(defaults()))
        barrier = self.root / "barrier"
        barrier.mkdir()
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            runs = [pool.submit(self.run_gate, extra_env={"ELF_TEST_BARRIER": str(barrier),
                    "ELF_TEST_RUN": str(n)}) for n in range(2)]
            for run in runs:
                result = run.result()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for n in range(2):
            seen = json.loads((barrier / f"seen-{n}").read_text())
            self.assertEqual(len(seen), 2, "each invocation must reserve private scratch")
            self.assertEqual(len(set(seen)), 2)
        self.assertEqual(list((self.root / "scratch").iterdir()), [])


class MakeCheckContractTests(unittest.TestCase):
    def test_host_check_runs_elf_gate_regressions(self):
        lines = (ROOT / "Makefile").read_text().splitlines()
        start = next(i for i, line in enumerate(lines) if line.startswith("check:"))
        recipe = []
        for line in lines[start + 1:]:
            if line and not line.startswith(("\t", "#")):
                break
            recipe.append(line)
        self.assertIn("\tpython3 ci/test_check_elf.py", recipe)


if __name__ == "__main__":
    unittest.main()
