#!/usr/bin/env python3
"""Exercise the real ELF gate with synthetic tools; no compiler, NDK, ELF or device needed."""
import concurrent.futures
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import check_steps

ROOT = Path(__file__).resolve().parent.parent
HOOK = "_ZN17StarfishMediaAPIs20callbackFunctionHookEixPKc"
LOAD = "_ZN17StarfishMediaAPIs4LoadEPKcPFvixS1_PvES2_"
FILLER = "synthetic-padding-line\n" * 100000  # More than 2 MiB, far beyond a pipe buffer.
# Hang guards, not budgets. This file's own timeouts used to be 20 s per gate run and 5 s for the
# parallel-invocation rendezvous, which a quiet machine meets many times over; on a 3-core macOS runner
# with a cargo build beside it the run that pipes 2.8 MiB through the gate took longer than 20 s and
# the whole step went red with TimeoutExpired (build-bench run 37694891086; the same tests finish in
# 15 s on a dev Mac and in 92 s there). They exist so a gate that DEADLOCKS (the SIGPIPE regressions
# this file pins) fails the test instead of hanging the suite, so they only have to be far above any
# machine's honest time.
GATE_HANG_GUARD = 240
RENDEZVOUS_HANG_GUARD = 120

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
    until = time.monotonic() + RENDEZVOUS_HANG_GUARD
    while len(list(barrier.glob("ready-*"))) < 2:
        if time.monotonic() > until: sys.exit(99)
        time.sleep(0.01)
    scratch = sorted(str(p) for p in Path(os.environ["TMPDIR"]).glob("check-elf.*"))
    (barrier / ("seen-" + os.environ["ELF_TEST_RUN"])).write_text(json.dumps(scratch))
# A spec entry "<key>:<file name>" answers for that one file; the storage helper is graded beside the
# main binary and the two must be able to disagree.
record = spec.get(key + ":" + Path(sys.argv[-1]).name, spec[key])
data = record["output"].encode()
while data:
    count = os.write(1, data)
    data = data[count:]
sys.exit(record.get("exit", 0))
'''.replace("RENDEZVOUS_HANG_GUARD", str(RENDEZVOUS_HANG_GUARD))


def defaults():
    return {key: {"output": value} for key, value in {
        "readelf:-h": "Class: ELF32\nMachine: ARM\nFlags: soft-float\nType: EXEC\n",
        "readelf:-A": "Tag_CPU_arch: v7\n",
        "readelf:--dyn-syms": f"1: 00000000 16 FUNC GLOBAL DEFAULT 1 {HOOK}\n",
        "readelf:-rW": "No relocations\n",
        "readelf:-l": "  LOAD 0x000000 0x00010000 0x00010000\n",
        "readelf:-n": "Build ID: " + "a" * 40 + "\n",
        "readelf:-d": "Shared library: [libalpha.so]\nShared library: [libbeta.so]\n",
        "readelf:-d:plxnative-storage": "Shared library: [libgamma.so]\nShared library: [libalpha.so]\n",
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
        (self.root / "ci/expected-dt-needed-storage.txt").write_text("libalpha.so\nlibgamma.so\n")
        (self.root / "plxnative-storage").write_text("synthetic helper")
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
                              env=env, capture_output=True, text=True, timeout=GATE_HANG_GUARD)

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

    def test_three_component_locale_or_version_data_is_not_an_ipv4_address(self):
        spec = defaults()
        spec["strings"]["output"] += "10.11.12\n172.16.12\n192.168.12\n"
        self.assert_pass(spec)

    def test_private_shaped_subsequence_inside_longer_dotted_data_is_not_an_address(self):
        spec = defaults()
        spec["strings"]["output"] += "1.2.3.4.5.6.7.8.9.10.11.12.13.14\n"
        self.assert_pass(spec)

    def test_complete_private_ipv4_ranges_remain_forbidden(self):
        for address in ("10.11.12.13", "10.0.0.1", "10.255.255.255",
                        "172.16.0.1", "172.31.255.255", "192.168.0.1", "192.168.255.255"):
            with self.subTest(address=address):
                spec = defaults()
                spec["strings"]["output"] += "http://" + address + ":32400/library\n"
                result = self.run_gate(spec)
                self.assertNotEqual(result.returncode, 0, "private address was allowed")
                self.assertIn("private IP address", result.stdout)
                self.assertIn(address, result.stdout, "the gate must identify the full four-octet address")

    def test_neighbouring_public_ipv4_ranges_are_allowed(self):
        spec = defaults()
        spec["strings"]["output"] += "9.11.12.13\n11.11.12.13\n172.15.255.255\n172.32.0.1\n192.167.0.1\n192.169.0.1\n"
        self.assert_pass(spec)

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

    def test_storage_helper_dt_needed_is_graded_against_its_own_list(self):
        for label, output in [("an extra library", "Shared library: [libalpha.so]\nShared library: [libgamma.so]\n"
                                                   "Shared library: [libdelta.so]\n"),
                              ("a missing library", "Shared library: [libalpha.so]\n"),
                              ("the main binary's list", "Shared library: [libalpha.so]\nShared library: [libbeta.so]\n")]:
            with self.subTest(label=label):
                spec = defaults()
                spec["readelf:-d:plxnative-storage"]["output"] = output
                result = self.run_gate(spec)
                self.assertNotEqual(result.returncode, 0, "helper drift was allowed: " + result.stdout)
                self.assertIn("storage helper DT_NEEDED drifted", result.stdout)

    def test_storage_helper_must_be_arm_soft_float_v7(self):
        for key, replacement, diagnostic in [
                ("readelf:-h:plxnative-storage", "Class: ELF32\nMachine: x86-64\nFlags: soft-float\n", "not ARM"),
                ("readelf:-h:plxnative-storage", "Class: ELF32\nMachine: ARM\nFlags: hard-float\n", "soft-float"),
                ("readelf:-A:plxnative-storage", "Tag_CPU_arch: v6\n", "not ARMv7")]:
            with self.subTest(key=key, diagnostic=diagnostic):
                spec = defaults()
                spec[key] = {"output": replacement}
                result = self.run_gate(spec)
                self.assertNotEqual(result.returncode, 0, "wrong helper ELF was allowed: " + result.stdout)
                self.assertIn(diagnostic, result.stdout)

    def test_absent_storage_helper_fails_in_ci_and_is_skipped_elsewhere(self):
        (self.root / "plxnative-storage").unlink()
        result = self.run_gate(defaults())
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("storage helper ships in the package", result.stdout)
        result = self.run_gate(defaults(), extra_env={"CI": "false"})
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("SKIP", result.stdout)

    def test_storage_helper_path_can_be_named(self):
        (self.root / "plxnative-storage").unlink()
        (self.root / "elsewhere").write_text("synthetic helper")
        spec = defaults()
        spec["readelf:-d:elsewhere"] = {"output": "Shared library: [libdelta.so]\n"}
        result = self.run_gate(spec, extra_env={"STORAGE_BIN": "elsewhere"})
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("storage helper DT_NEEDED drifted", result.stdout)

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
        # `make check` itself is just `tools/check-lock.py`'s machine-wide queue wrapper (two
        # slots) around `check-unlocked`, which fans out to `check-python` and so to the step manifest
        # (ci/check-python-steps.txt) this asserts on; `make check` still runs it, just queued.
        lines = (ROOT / "Makefile").read_text().splitlines()

        def recipe_of(target):
            start = next(i for i, line in enumerate(lines) if line.startswith(target + ":"))
            recipe = []
            for line in lines[start + 1:]:
                if line and not line.startswith(("\t", "#")):
                    break
                recipe.append(line)
            return recipe

        # `check-unlocked` runs the `check-cargo` and `check-python` branches side by side; the
        # Python-only gates (this one) live in the second.
        unlocked = "\n".join(recipe_of("check-unlocked"))
        self.assertIn("check-cargo", unlocked)
        self.assertIn("check-python", unlocked)
        self.assertIn("python3 ci/test_check_elf.py", check_steps.commands())


if __name__ == "__main__":
    unittest.main()
