#!/usr/bin/env python3
"""The two platform crates forward the application's features, and enable none of them behind its back.

`plx_platform_webos` (the ARM staticlib's crate) and `plx_platform_desktop` (the simulator's) depend on the
application crate, `plxnative-modules`. The Makefile and the workflows select features with `-p <platform
crate> --no-default-features --features <words>`, and a word is only meaningful on a package that declares it.
So each platform crate declares every feature of the application and forwards it, with the application's own
defaults. A feature added to the application and not forwarded here would pass `--workspace` and every host
gate and fail only the ARM build (or only the simulator's), which is what this pins.

The dependency edge is pinned too, for the reason that decides the whole shape: a feature a dependency spec
names is enabled on the application in EVERY build that includes the platform package, so a platform crate
that wrote `features = ["hostsim"]` there would turn every default-feature `cargo test -p ... -p
plx_platform_desktop` pass into a simulator pass. The desktop's `hostsim` is its own feature, forwarded on
request, and the library and binary that need it say so. No `version` on the edge either: the read-only
certificates (ci/readonly_dependencies.py) refuse a versioned dependency on the root package.

Reads manifests and one source file; compiles nothing and runs no cargo.
"""
from pathlib import Path
import sys
import tomllib
import unittest

ROOT = Path(__file__).resolve().parent.parent
MODULES = ROOT / "rust-modules"
APP_NAME = "plxnative-modules"
# crate directory -> the app features it does not forward, each with the reason.
PLATFORMS = {
    # The demo-video dump build's pixel oracle (`plx_ui`): only the simulator's dump build names it.
    "platform-webos": {"placeholder-sentinel"},
    "platform-desktop": set(),
}


def load(path):
    return tomllib.loads(Path(path).read_text(encoding="utf-8"))


def forwards(spec, feature):
    return f"{APP_NAME}/{feature}" in spec


def problems(app, platform, exempt):
    """Every way `platform` (a parsed manifest) fails to forward `app`'s (a parsed manifest) features."""
    found = []
    app_features = app.get("features", {})
    features = platform.get("features", {})
    if features.get("default") != app_features.get("default"):
        found.append(f"default is {features.get('default')}, the application's is {app_features.get('default')}")
    for feature in sorted(set(app_features) - {"default"} - set(exempt)):
        if feature not in features:
            found.append(f"does not declare the application's feature `{feature}`")
        elif not forwards(features[feature], feature):
            found.append(f"declares `{feature}` without forwarding `{APP_NAME}/{feature}`")
    spec = platform.get("dependencies", {}).get(APP_NAME)
    if not isinstance(spec, dict):
        found.append(f"does not depend on `{APP_NAME}` with an inline table")
        return found
    if spec.get("path") != "..":
        found.append(f"depends on `{APP_NAME}` at {spec.get('path')!r}, not `..`")
    if spec.get("default-features") is not False:
        found.append("the dependency on the application must set `default-features = false`: the platform's own "
                     "`default` forwards the application's defaults, and a dependency default would add them to a "
                     "`--no-default-features` build")
    for key in ("features", "version", "optional", "package"):
        if key in spec:
            found.append(f"the dependency on the application names `{key}`: a dependency feature is unified over "
                         f"every build that includes this package, and a version is refused by the certificates")
    for table in ("dev-dependencies", "build-dependencies"):
        if APP_NAME in platform.get(table, {}):
            found.append(f"`{APP_NAME}` is also in [{table}]: its features would unify with the build's")
    return found


class ForwardedFeatures(unittest.TestCase):
    def test_every_platform_crate_forwards_every_application_feature(self):
        app = load(MODULES / "Cargo.toml")
        for directory, exempt in PLATFORMS.items():
            with self.subTest(directory):
                self.assertEqual(problems(app, load(MODULES / directory / "Cargo.toml"), exempt), [])

    def test_the_exemptions_are_application_features(self):
        features = load(MODULES / "Cargo.toml")["features"]
        for directory, exempt in PLATFORMS.items():
            self.assertLessEqual(exempt, set(features), f"{directory}: a stale exemption")

    def test_a_platform_crate_is_a_mounted_rlib_with_the_host_link_script(self):
        for directory in PLATFORMS:
            with self.subTest(directory):
                doc = load(MODULES / directory / "Cargo.toml")
                name = "plx_" + directory.replace("-", "_")
                self.assertEqual(doc["package"]["name"], name)
                self.assertEqual(doc["package"]["metadata"]["plx"]["mount"], directory.replace("-", "_"))
                self.assertEqual(doc["lib"]["crate-type"], ["rlib"])
                self.assertEqual(doc["package"]["build"], "build.rs")
                self.assertEqual(doc["lints"], {"workspace": True})
                self.assertIn("host_link::emit", (MODULES / directory / "build.rs").read_text(encoding="utf-8"))
                self.assertIn(directory, load(MODULES / "Cargo.toml")["workspace"]["members"])

    def test_the_application_names_no_platform_crate(self):
        app = load(MODULES / "Cargo.toml")
        for table in ("dependencies", "dev-dependencies", "build-dependencies"):
            for alias, spec in app.get(table, {}).items():
                self.assertNotIn("platform", alias, f"[{table}] {alias}: the dependency runs from the platform to the application")
                self.assertNotIn("platform-", spec.get("path", "") if isinstance(spec, dict) else "")

    def test_the_desktop_library_is_empty_without_hostsim(self):
        # The library is `#![cfg(feature = "hostsim")]`, so a default-feature pass that lists the package
        # builds nothing and runs no test; the feature itself is declared above (forwarded as `hostsim`).
        source = (MODULES / "platform-desktop" / "src" / "lib.rs").read_text(encoding="utf-8")
        code = [line for line in source.splitlines() if not line.lstrip().startswith("//")]
        self.assertEqual(code[0], '#![cfg(feature = "hostsim")]')
        for manifest in (MODULES / "platform-desktop" / "Cargo.toml",):
            for binary in load(manifest).get("bin", []):
                self.assertEqual(binary.get("required-features"), ["hostsim"], binary.get("name"))


class TheCheckBites(unittest.TestCase):
    """`problems` is red on each way a platform crate can drift, so the green above is not vacuous."""
    APP = {"features": {"default": ["a"], "a": ["x/a"], "b": [], "c": []}}

    def good(self):
        return {
            "features": {"default": ["a"], "a": [f"{APP_NAME}/a"], "b": [f"{APP_NAME}/b"], "c": [f"{APP_NAME}/c"]},
            "dependencies": {APP_NAME: {"path": "..", "default-features": False}},
        }

    def test_a_faithful_manifest_is_clean(self):
        self.assertEqual(problems(self.APP, self.good(), set()), [])

    def test_a_missing_or_unforwarded_feature_is_found(self):
        platform = self.good()
        del platform["features"]["b"]
        platform["features"]["c"] = []
        found = problems(self.APP, platform, set())
        self.assertIn("does not declare the application's feature `b`", found)
        self.assertIn(f"declares `c` without forwarding `{APP_NAME}/c`", found)
        self.assertEqual(problems(self.APP, platform, {"b", "c"}), [])

    def test_different_defaults_are_found(self):
        platform = self.good()
        platform["features"]["default"] = ["a", "b"]
        self.assertEqual(len(problems(self.APP, platform, set())), 1)

    def test_a_dependency_feature_a_version_or_a_default_are_found(self):
        for key, value in (("features", ["hostsim"]), ("version", "0.10.0"), ("optional", True), ("package", "x")):
            with self.subTest(key):
                platform = self.good()
                platform["dependencies"][APP_NAME][key] = value
                self.assertEqual(len(problems(self.APP, platform, set())), 1)
        platform = self.good()
        del platform["dependencies"][APP_NAME]["default-features"]
        self.assertEqual(len(problems(self.APP, platform, set())), 1)
        platform = self.good()
        platform["dependencies"][APP_NAME]["path"] = "../x"
        self.assertEqual(len(problems(self.APP, platform, set())), 1)

    def test_the_application_in_a_dev_or_build_table_is_found(self):
        platform = self.good()
        platform["dev-dependencies"] = {APP_NAME: {"path": "..", "features": ["hostsim"]}}
        self.assertEqual(len(problems(self.APP, platform, set())), 1)


if __name__ == "__main__":
    sys.exit(unittest.main(verbosity=1))
