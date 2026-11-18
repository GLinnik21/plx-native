"""Compile the actual physical owner declarations and loan constructor in isolation.

Machine internals are opaque stubs: these probes exercise Rust ownership, not domain behavior.
The positive control must compile before either negative diagnostic counts as evidence.
"""
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

APP = Path(__file__).resolve().parents[1] / "rust-modules/src/app"


def declaration(source: str, needle: str) -> str:
    start = source.index(needle)
    opening = source.index("{", start)
    depth = 1
    for end in range(opening + 1, len(source)):
        depth += (source[end] == "{") - (source[end] == "}")
        if depth == 0:
            return source[start:end + 1]
    raise AssertionError(f"unclosed declaration: {needle}")


def actual_constructor_source() -> str:
    bridge = (APP / "bridge.rs").read_text()
    owners = (APP / "runtime_owners.rs").read_text()
    physical = "\n".join(declaration(owners, f"pub(crate) struct {name} {{")
                         for name in ("SessionOwner", "ConsentOwner", "StoreOwners",
                                      "MounterIo", "HudRuntimeOwner", "RuntimeOwners"))
    loan = declaration(owners, "pub(crate) fn bridge<'a>")
    borrowed = declaration(bridge, "pub(crate) struct Bridge<'a> {")
    return """
#![allow(dead_code)]
mod plx_session { pub mod auth { pub struct SessionMachine; } }
mod plx_data { pub mod stores { pub struct Stores; } }
mod plx_screens { pub mod registry { pub struct AppMounter; } }
mod app {
    pub mod adapters {
        pub mod session { pub struct SessionAdapter; }
        pub mod consent { pub struct ConsentAdapter; }
        pub mod stores { pub struct StoreRunner; }
        pub mod huds { pub struct HudDriver; }
    }
    pub mod bridge {
        use crate::{plx_session, plx_data};
        use crate::plx_screens::registry::AppMounter;
        pub(crate) struct ConsentMachine;
        pub(crate) struct BridgeRetained { pub(crate) value: u8 }
        BORROWED
    }
    pub mod runtime_owners {
        use crate::{plx_session, plx_data, plx_screens};
        use super::adapters;
        use super::bridge::{Bridge, BridgeRetained, ConsentMachine};
        PHYSICAL
        impl RuntimeOwners { LOAN }
        PROBE
    }
}
""".replace("BORROWED", borrowed).replace("PHYSICAL", physical).replace("LOAN", loan)


class RuntimeOwnershipBorrowProbe(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.rustc = shutil.which("rustc")
        if cls.rustc is None:
            raise AssertionError("rustc is required for actual owner-loan compile probes")
        cls.source = actual_constructor_source()

    def compile(self, probe: str):
        with tempfile.TemporaryDirectory(prefix="plx-owner-borrows-") as tmp:
            source = Path(tmp) / "probe.rs"
            source.write_text(self.source.replace("PROBE", probe))
            return subprocess.run([self.rustc, "--edition=2021", "--crate-type=lib",
                                   "--emit=metadata", str(source), "-o", str(Path(tmp) / "probe.rmeta")],
                                  capture_output=True, text=True)

    def test_actual_constructor_accepts_a_scoped_loan(self):
        result = self.compile("""
fn scoped<'a>(runtime: &'a mut RuntimeOwners, retained: &'a mut BridgeRetained) -> Bridge<'a> {
    runtime.bridge(retained)
}
fn sequential(runtime: &mut RuntimeOwners, retained: &mut BridgeRetained) {
    runtime.bridge(retained).retained.value = 1;
    runtime.bridge(retained).retained.value = 2;
}
""")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_actual_constructor_requires_the_hud_owner_loan(self):
        result = self.compile("""
fn missing_hud<'a>(runtime: &'a mut RuntimeOwners, retained: &'a mut BridgeRetained) -> Bridge<'a> {
    let _held_hud = &mut runtime.hud.driver;
    let turn = runtime.bridge(retained);
    std::hint::black_box(_held_hud);
    turn
}
""")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("E0499", result.stderr)

    def test_actual_constructor_cannot_return_a_local_owner_as_static(self):
        result = self.compile("""
fn escape(mut runtime: RuntimeOwners, mut retained: BridgeRetained) -> Bridge<'static> {
    runtime.bridge(&mut retained)
}
""")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("E0515", result.stderr)

    def test_actual_constructor_refuses_simultaneous_mutable_turns(self):
        result = self.compile("""
fn overlapping(runtime: &mut RuntimeOwners, retained: &mut BridgeRetained) {
    let first = runtime.bridge(retained);
    let second = runtime.bridge(retained);
    first.retained.value = second.retained.value;
}
""")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("E0499", result.stderr)


if __name__ == "__main__":
    unittest.main()
