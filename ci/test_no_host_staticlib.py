#!/usr/bin/env python3
"""Fail if a host cargo build of the app crate would emit a staticlib or any non-rlib artifact.

`rust-modules/Cargo.toml` declares `crate-type = ["rlib"]`: the simulator bin and the host tests
only need the rlib, and the ARM archive `make` links is produced separately by
`cargo rustc --crate-type staticlib` (whose archive `ci/check-staticlib-artifact.py` checks in the
ARM build). A `staticlib` crate type on the crate makes EVERY cargo build of it (`cargo build`, the
simulator, anything that depends on the library) also write a ~200 MB archive that bundles every
upstream crate, and makes the ARM build write an rlib nobody links.

This used to run a full `cargo build --lib` (30-50 s of codegen on the serial cargo path of
`make check`) and read the `compiler-artifact` records for a `.a` filename. Cargo derives every
file it writes for a library unit from that unit's `crate_types`, so the property is decided before
rustc starts, and this reads it there at no compile cost:

  * `cargo build --lib --unit-graph` (nightly, `-Zunstable-options`) prints the resolved unit
    graph of the build the old test ran, without compiling anything. The app library unit's
    `target.crate_types` must be exactly `["rlib"]`;
  * `cargo metadata --no-deps` reports the declared `[lib]` target; its `crate_types` and `kind`
    must be exactly `["rlib"]` too, so a manifest edit is named at the manifest and not only in the
    resolved graph.

The detector is proven RED against a manifest that adds `staticlib` (or `cdylib`) by
`ManifestMutationTests`, which builds a throwaway workspace of symlinks to the real sources with
one edited `Cargo.toml` -- the real tree is never touched.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
CRATE = "plxnative_modules"
# The layer crates split out of the app crate (docs/module-layers.md), each a dependency of it and
# each held to the same rule: an rlib, never an archive. One archive is linked, and it is the app's.
LAYER_CRATES = ("plx_base", "plx_machine", "plx_net", "plx_platform", "plx_gfx", "plx_ui", "plx_plex", "plx_telemetry", "plx_data")
NIGHTLY = os.environ.get("RUST_NIGHTLY", "nightly")
# What the workspace needs for cargo to resolve (not compile) the app package: the manifest and
# lockfile, the `.cargo/config.toml` that cargo finds from the working directory, and the sources
# the manifest names. A mutated copy replaces `Cargo.toml` only.
WORKSPACE_FILES = (".cargo", "Cargo.lock", "build.rs", "build_support", "src", "storage", "base", "machine", "net", "platform", "gfx", "ui", "plex", "telemetry", "data")


def offences(unit_graph=None, metadata=None):
    """Describe every way the app library would be built as something other than an rlib."""
    found = []
    if unit_graph is not None:
        units = [u for u in unit_graph.get("units", []) if (u.get("target") or {}).get("name") == CRATE]
        if not units:
            found.append(("unit graph", f"no {CRATE} library unit at all"))
        for unit in units:
            types = unit["target"].get("crate_types") or []
            if types != ["rlib"]:
                found.append(("unit crate_types", ",".join(types)))
    if metadata is not None:
        targets = [t for p in metadata.get("packages", []) for t in p.get("targets", [])
                   if t.get("name") == CRATE]
        if not targets:
            found.append(("metadata", f"no {CRATE} target at all"))
        for target in targets:
            for field in ("crate_types", "kind"):
                if target.get(field) != ["rlib"]:
                    found.append((f"target {field}", ",".join(target.get(field) or [])))
    return found


def layer_offences(unit_graph):
    """The layer crates that the host library build compiles as anything but an rlib, or not at all."""
    found = []
    for layer in LAYER_CRATES:
        units = [u for u in unit_graph.get("units", []) if (u.get("target") or {}).get("name") == layer]
        if not units:
            found.append((layer, "no library unit at all"))
        found.extend((layer, ",".join(u["target"].get("crate_types") or []))
                     for u in units if (u["target"].get("crate_types") or []) != ["rlib"])
    return found


def cargo(args, cwd):
    env = dict(os.environ)
    env["PATH"] = str(Path.home() / ".cargo/bin") + os.pathsep + env.get("PATH", "")
    # cwd matters: cargo finds rust-modules/.cargo/config.toml (build-std and the codegen flags)
    # from the working directory.
    proc = subprocess.run(["cargo", f"+{NIGHTLY}", *args], cwd=cwd, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, text=True, env=env)
    if proc.returncode != 0:
        raise AssertionError(f"cargo {' '.join(args)} failed: {proc.stderr[-2000:]}")
    return json.loads(proc.stdout)


def resolve(cwd):
    """(unit graph of `cargo build --lib`, `cargo metadata --no-deps`) for the workspace at `cwd`."""
    graph = cargo(["build", "--lib", "--unit-graph", "-Zunstable-options"], cwd)
    meta = cargo(["metadata", "--format-version", "1", "--no-deps"], cwd)
    return graph, meta


class ParserTests(unittest.TestCase):
    def graph(self, types=("rlib",), name=CRATE):
        return {"units": [{"target": {"name": name, "kind": list(types), "crate_types": list(types)},
                           "mode": "build"}]}

    def meta(self, types=("rlib",), name=CRATE):
        return {"packages": [{"targets": [{"name": name, "kind": list(types),
                                           "crate_types": list(types)}]}]}

    def test_rlib_only_is_clean(self):
        self.assertEqual(offences(self.graph(), self.meta()), [])

    def test_flags_staticlib_in_the_unit(self):
        self.assertEqual(offences(self.graph(("rlib", "staticlib"))),
                         [("unit crate_types", "rlib,staticlib")])

    def test_flags_cdylib_in_the_unit(self):
        self.assertEqual(offences(self.graph(("cdylib",))), [("unit crate_types", "cdylib")])

    def test_flags_declared_staticlib_in_metadata(self):
        self.assertEqual(offences(metadata=self.meta(("staticlib", "rlib"))),
                         [("target crate_types", "staticlib,rlib"), ("target kind", "staticlib,rlib")])

    def test_a_missing_library_is_an_offence_not_a_pass(self):
        self.assertEqual(len(offences(self.graph(name="other"), self.meta(name="other"))), 2)

    def test_a_layer_crate_must_be_present_and_an_rlib(self):
        def layers(types=("rlib",), skip=()):
            return {"units": [u for layer in LAYER_CRATES if layer not in skip
                              for u in self.graph(types, name=layer)["units"]]}
        self.assertEqual(layer_offences(layers()), [])
        self.assertEqual(layer_offences(layers(("staticlib",))), [(layer, "staticlib") for layer in LAYER_CRATES])
        self.assertEqual(layer_offences(layers(skip=("plx_machine",))), [("plx_machine", "no library unit at all")])
        self.assertEqual(layer_offences(self.graph()), [(layer, "no library unit at all") for layer in LAYER_CRATES])

    def test_other_crates_may_be_archives(self):
        graph = {"units": [{"target": {"name": "zstd_sys", "kind": ["staticlib"],
                                       "crate_types": ["staticlib"]}, "mode": "build"},
                           *self.graph()["units"]]}
        self.assertEqual(offences(graph), [])


class HostBuildTests(unittest.TestCase):
    def test_host_lib_build_is_rlib_only(self):
        graph, meta = resolve(ROOT / "rust-modules")
        self.assertEqual(offences(graph, meta), [], f"{CRATE} would build more than an rlib")
        self.assertEqual(layer_offences(graph), [], "a layer crate would build more than an rlib")


class ManifestMutationTests(unittest.TestCase):
    """The detector is RED when a manifest adds an archive crate type, against real cargo."""

    def workspace_with(self, crate_type_line):
        tmp = tempfile.TemporaryDirectory(prefix="plx-staticlib-")
        self.addCleanup(tmp.cleanup)
        src = ROOT / "rust-modules"
        dest = Path(tmp.name) / "rust-modules"
        dest.mkdir()
        for name in WORKSPACE_FILES:
            (dest / name).symlink_to(src / name)
        manifest = (src / "Cargo.toml").read_text()
        declared = 'crate-type = ["rlib"]'
        self.assertIn(declared, manifest, "the manifest no longer spells the lib crate-type this "
                      "mutation test edits; update it with the manifest")
        (dest / "Cargo.toml").write_text(manifest.replace(declared, crate_type_line))
        return dest

    def test_unmutated_copy_is_clean(self):
        # The control: the same symlink workspace, manifest byte-identical, is not an offence --
        # so the red cases below are red for the edit and not for the harness.
        graph, meta = resolve(self.workspace_with('crate-type = ["rlib"]'))
        self.assertEqual(offences(graph, meta), [])

    def test_staticlib_added_to_the_manifest_is_caught(self):
        graph, meta = resolve(self.workspace_with('crate-type = ["rlib", "staticlib"]'))
        found = offences(graph, meta)
        self.assertIn(("unit crate_types", "rlib,staticlib"), found)
        self.assertIn(("target crate_types", "rlib,staticlib"), found)

    def test_cdylib_added_to_the_manifest_is_caught(self):
        graph, meta = resolve(self.workspace_with('crate-type = ["cdylib", "rlib"]'))
        self.assertTrue(any("cdylib" in detail for _, detail in offences(graph, meta)))


if __name__ == "__main__":
    sys.exit(unittest.main(verbosity=1))
