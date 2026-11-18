"""Source contracts for the App-owned runtime cut (not a substitute for Rust tests)."""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[1]
APP = ROOT / "rust-modules/src/app"


def body_after(source: str, declaration: str) -> str:
    start = source.index("{", source.index(declaration)) + 1
    depth = 1
    for end in range(start, len(source)):
        depth += (source[end] == "{") - (source[end] == "}")
        if depth == 0:
            return source[start:end]
    raise AssertionError(f"unclosed declaration: {declaration}")


OWNED_BRIDGE_CONSTRUCTOR = re.compile(
    r"\bBridge::(?:for_test|for_session_test|for_consent_test|for_consent_resource_test|controlled_home|new)\s*\("
)


def retained_hud_owners(source: str) -> list[str]:
    """Follow aliases/wrappers, but allow explicitly captured immutable references."""
    source = re.sub(r"//[^\n]*|/\*.*?\*/", "", source, flags=re.S)
    aliases = dict(re.findall(r"\btype\s+(\w+)(?:\s*<[^;=]+>)?\s*=\s*([^;]+);", source))
    structs = {name: body_after(source, match.group(0))
               for match in re.finditer(r"\bstruct\s+(\w+)\s*\{", source)
               for name in [match.group(1)]}

    def owns(field_type, seen):
        reference = re.match(r"&\s*(?:'\w+\s+)?(.*)", field_type.strip())
        if reference and not re.match(r"mut\b", reference.group(1)):
            return False
        for token in re.findall(r"\b\w+\b", field_type):
            if token in ("HudDriver", "HudStore"):
                return True
            if token not in seen:
                nested = aliases.get(token, structs.get(token, ""))
                if nested and owns(nested, seen | {token}):
                    return True
        return False

    body = body_after(source, "pub(crate) struct BridgeRetained {")
    return [name for name, typ in re.findall(r"(\w+)\s*:\s*([^\n]+)", body)
            if owns(typ, set())]


class RuntimeOwnershipContract(unittest.TestCase):
    def test_bridge_borrows_every_child_and_retained_publication(self):
        source = (APP / "bridge.rs").read_text()
        body = body_after(source, "pub(crate) struct Bridge<'a> {")
        fields = re.findall(r"pub\(super\) (\w+): ([^\n]+),", body)
        self.assertEqual({name for name, _ in fields}, {
            "session", "session_adapter", "consent", "consent_adapter",
            "stores", "store_runner", "mounter", "huds", "retained",
        })
        for name, field_type in fields:
            with self.subTest(field=name):
                self.assertTrue(field_type.startswith("&'a mut "), field_type)

    def test_app_owns_the_tree_instead_of_a_long_lived_bridge(self):
        source = (APP / "mod.rs").read_text()
        body = body_after(source, "pub(crate) struct App {")
        self.assertIn("runtime: runtime_owners::RuntimeOwners", body)
        self.assertIn("bridge: bridge::BridgeRetained", body)
        owners = (APP / "runtime_owners.rs").read_text()
        for child in ("SessionOwner", "ConsentOwner", "StoreOwners", "MounterIo", "HudRuntimeOwner"):
            self.assertIn(f"struct {child}", owners)
        self.assertNotRegex(owners, r"impl[^\n]*Deref")
        self.assertNotIn("Box::leak", owners)

    def test_retained_has_no_logical_hud_owner(self):
        source = (APP / "bridge.rs").read_text()
        self.assertEqual(retained_hud_owners(source), [])

    def test_retained_guard_closes_aliases_and_latent_owned_wrappers(self):
        for typ in ("HudDriver", "HudStore", "Option<Alias>", "Latent", "Generic<u8>",
                    "&'static mut HudStore", "& mut HudDriver"):
            with self.subTest(field_type=typ):
                source = ("type Alias = super::huds::HudDriver; type Generic<T> = (Alias, T); "
                          "struct Latent { driver: Option<Alias> } "
                          "pub(crate) struct BridgeRetained {\n"
                          f"hidden: {typ},\n}}")
                self.assertEqual(retained_hud_owners(source), ["hidden"])
        self.assertEqual(retained_hud_owners(
            "pub(crate) struct BridgeRetained {\ncaptured: &'static HudStore,\n}"), [])

    def test_old_owned_bridge_fixtures_are_retired(self):
        offenders = []
        for path in APP.rglob("*.rs"):
            for number, line in enumerate(path.read_text().splitlines(), 1):
                if OWNED_BRIDGE_CONSTRUCTOR.search(line):
                    offenders.append(f"{path.relative_to(ROOT)}:{number}")
        self.assertEqual(len(offenders), 0,
                         "migrate fixtures to Harness.with_bridge, not leaked owners: "
                         + ", ".join(offenders[:8]))

    def test_every_retired_factory_is_rejected_by_the_source_guard(self):
        for factory in ("for_test", "for_session_test", "for_consent_test",
                        "for_consent_resource_test", "controlled_home", "new"):
            with self.subTest(factory=factory):
                self.assertIsNotNone(OWNED_BRIDGE_CONSTRUCTOR.search(
                    f"let mut rig = Bridge::{factory}(fixture);"))
                self.assertIsNone(OWNED_BRIDGE_CONSTRUCTOR.search(
                    f"let owners = BridgeRetained::{factory}(fixture);"))
        self.assertIsNone(OWNED_BRIDGE_CONSTRUCTOR.search("let mut owner = Harness::new(now);"))


if __name__ == "__main__":
    unittest.main()
