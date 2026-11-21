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
request, and the library and binary that need it say so. That crate's library is empty without it, so its
dependencies exist only under it: each is `optional = true`, `hostsim` is the one feature that enables them
(`dep:`), and every other feature forwards the application's WEAKLY (`plxnative-modules?/<feature>`), which
adds the optional edge in no build. A default-feature pass, which lists the package beside the application,
therefore enables nothing on the application and compiles nothing of the dependencies, and `make lint`
reports no unused dependency for the empty library. No `version` on the edge either: the read-only
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


def forwards(spec, feature, weak=False):
    return f"{APP_NAME}{'?' if weak else ''}/{feature}" in spec


def reachable(features, start):
    """Every feature `start` turns on, itself included (a spec word without `/` or `dep:` names a feature)."""
    seen, stack = set(), [start]
    while stack:
        name = stack.pop()
        if name in seen:
            continue
        seen.add(name)
        stack.extend(word for word in features.get(name, []) if "/" not in word and not word.startswith("dep:"))
    return seen


def optional_edge_problems(app_features, features, dependencies, exempt):
    """The shape of a platform crate whose application edge is `optional = true`, which only `hostsim` enables."""
    found = []
    enabling = {name for name, spec in features.items() if f"dep:{APP_NAME}" in spec}
    if enabling != {"hostsim"}:
        found.append(f"the features that enable the application are {sorted(enabling)}, not exactly ['hostsim']")
    for feature in sorted(set(app_features) - {"default"} - set(exempt)):
        spec = features.get(feature, [])
        if feature in enabling:
            if not forwards(spec, feature):
                found.append(f"`{feature}` enables the application without forwarding `{APP_NAME}/{feature}`")
        elif forwards(spec, feature):
            found.append(f"`{feature}` forwards `{APP_NAME}/{feature}` strongly, which enables the optional application "
                         f"in every build that turns it on: write `{APP_NAME}?/{feature}`")
        elif not forwards(spec, feature, weak=True):
            found.append(f"`{feature}` does not forward `{APP_NAME}?/{feature}`")
    for name in sorted(features):
        for word in features[name]:
            # (A feature of the application's was judged above; this is any other the manifest invents.)
            if word.startswith(f"{APP_NAME}/") and name not in enabling and name not in app_features:
                found.append(f"`{name}` names `{word}`, a strong forward outside the enabling feature")
    for feature in sorted(enabling & reachable(features, "default")):
        found.append(f"the default features reach `{feature}`, which enables the application")
    for name, entry in sorted(dependencies.items()):
        if name == APP_NAME:
            continue
        if not isinstance(entry, dict) or entry.get("optional") is not True:
            found.append(f"`{name}` is not optional, but the library that uses it is empty without hostsim")
        elif not all(f"dep:{name}" in features.get(feature, []) for feature in enabling):
            found.append(f"`{name}` is optional and no enabling feature pulls it in with `dep:{name}`")
    return found


def problems(app, platform, exempt):
    """Every way `platform` (a parsed manifest) fails to forward `app`'s (a parsed manifest) features."""
    found = []
    app_features = app.get("features", {})
    features = platform.get("features", {})
    if features.get("default") != app_features.get("default"):
        found.append(f"default is {features.get('default')}, the application's is {app_features.get('default')}")
    spec = platform.get("dependencies", {}).get(APP_NAME)
    if not isinstance(spec, dict):
        found.append(f"does not depend on `{APP_NAME}` with an inline table")
        return found
    if spec.get("optional") is True:
        found.extend(optional_edge_problems(app_features, features, platform.get("dependencies", {}), exempt))
    else:
        for feature in sorted(set(app_features) - {"default"} - set(exempt)):
            if feature not in features:
                found.append(f"does not declare the application's feature `{feature}`")
            elif not forwards(features[feature], feature):
                found.append(f"declares `{feature}` without forwarding `{APP_NAME}/{feature}`")
    if spec.get("path") != "..":
        found.append(f"depends on `{APP_NAME}` at {spec.get('path')!r}, not `..`")
    if spec.get("default-features") is not False:
        found.append("the dependency on the application must set `default-features = false`: the platform's own "
                     "`default` forwards the application's defaults, and a dependency default would add them to a "
                     "`--no-default-features` build")
    for key in ("features", "version", "package"):
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

    def test_the_webos_crate_owns_the_two_sink_features(self):
        # `feed-sink` (the Starfish sink's Feed half) and `uri-transport` (the com.webos.media LS2 worker) are
        # what the backends switch on in the webOS port's own code, so they are this crate's features, set by
        # its `play-feed` and `play-uri`; `plx_os`, which held them while the port was its module, and the
        # application, which forwarded them to it, name neither any more.
        webos = load(MODULES / "platform-webos" / "Cargo.toml")["features"]
        self.assertEqual(webos["feed-sink"], [])
        self.assertEqual(webos["uri-transport"], [])
        self.assertIn("feed-sink", webos["play-feed"])
        self.assertIn("uri-transport", webos["play-uri"])
        app = load(MODULES / "Cargo.toml")["features"]
        for crate in (MODULES / "os" / "Cargo.toml", MODULES / "Cargo.toml"):
            features = load(crate).get("features", {})
            for word in ("feed-sink", "uri-transport"):
                self.assertNotIn(word, features, f"{crate}: `{word}` left with the port")
        for feature in ("play-feed", "play-uri"):
            self.assertFalse([f for f in app[feature] if f.startswith("plx_os/")], f"app `{feature}` still forwards into plx_os")

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

    def test_the_desktop_dependencies_exist_only_under_hostsim(self):
        # An empty library with three dependencies reports three unused ones (`make lint`), and a default-feature
        # pass that lists the package would compile all of them for nothing. So every dependency of the desktop
        # crate is optional, `hostsim` alone enables them, and the default features reach none (the weak
        # forwards: `plxnative-modules?/<feature>` adds the edge in no build).
        doc = load(MODULES / "platform-desktop" / "Cargo.toml")
        dependencies = doc["dependencies"]
        self.assertEqual(sorted(dependencies), ["plx_base", "plx_os", APP_NAME])
        for name, spec in dependencies.items():
            self.assertIs(spec.get("optional"), True, name)
        features = doc["features"]
        self.assertEqual(sorted(word for word in features["hostsim"] if word.startswith("dep:")),
                         sorted(f"dep:{name}" for name in dependencies))
        self.assertIn(f"{APP_NAME}/hostsim", features["hostsim"])
        self.assertEqual(reachable(features, "default") & {"hostsim"}, set())
        for feature, words in features.items():
            if feature != "hostsim":
                self.assertFalse([w for w in words if w.startswith("dep:") or w.startswith(f"{APP_NAME}/")], feature)


    def test_the_simulator_is_the_desktop_crates_binary_and_the_applications_no_longer(self):
        # `plxnative-sim` left the application package with W7b: every build of it selects `-p plx_platform_desktop`
        # (ci/test_sim_backend.py), and the application package, which the ARM staticlib's crate and the unit suite
        # depend on, holds no bin target for a `--bins` build to compile.
        self.assertNotIn("bin", load(MODULES / "Cargo.toml"))
        binaries = load(MODULES / "platform-desktop" / "Cargo.toml").get("bin", [])
        self.assertEqual([(b["name"], b["path"]) for b in binaries], [("plxnative-sim", "src/bin/sim.rs")])
        self.assertTrue((MODULES / "platform-desktop" / "src" / "bin" / "sim.rs").is_file())
        self.assertEqual(sorted((MODULES / "src" / "bin").glob("*.rs")), [], "a bin of the application package is back")


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
        for key, value in (("features", ["hostsim"]), ("version", "0.10.0"), ("package", "x")):
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

    # The simulator's shape: an `optional` application edge only `hostsim` enables, weak forwards elsewhere.
    OPT_APP = {"features": {"default": ["a"], "a": ["x/a"], "hostsim": ["x/hostsim"]}}

    def good_optional(self):
        return {
            "features": {"default": ["a"], "a": [f"{APP_NAME}?/a"],
                         "hostsim": [f"dep:{APP_NAME}", f"{APP_NAME}/hostsim", "dep:lower"]},
            "dependencies": {APP_NAME: {"path": "..", "default-features": False, "optional": True},
                             "lower": {"path": "../lower", "optional": True}},
        }

    def test_a_faithful_optional_edge_is_clean(self):
        self.assertEqual(problems(self.OPT_APP, self.good_optional(), set()), [])

    def test_an_optional_edge_with_a_strong_forward_is_found(self):
        # `a = ["plxnative-modules/a"]` enables the optional application in every build that turns `a` on, and
        # `default` turns it on: the default pass would be an application pass again.
        platform = self.good_optional()
        platform["features"]["a"] = [f"{APP_NAME}/a"]
        found = problems(self.OPT_APP, platform, set())
        self.assertTrue(any("strongly" in f for f in found), found)

    def test_an_optional_edge_that_a_default_feature_enables_is_found(self):
        platform = self.good_optional()
        platform["features"]["a"] = [f"{APP_NAME}?/a", "hostsim"]
        found = problems(self.OPT_APP, platform, set())
        self.assertTrue(any("default features reach" in f for f in found), found)
        platform = self.good_optional()
        platform["features"]["a"] = [f"dep:{APP_NAME}", f"{APP_NAME}/a"]
        self.assertTrue(problems(self.OPT_APP, platform, set()))

    def test_a_weak_forward_on_a_required_edge_and_an_optional_edge_without_one_are_found(self):
        platform = self.good()
        platform["features"]["a"] = [f"{APP_NAME}?/a"]
        self.assertEqual(len(problems(self.APP, platform, set())), 1)
        platform = self.good_optional()
        platform["features"]["a"] = []
        self.assertEqual(problems(self.OPT_APP, platform, set()), [f"`a` does not forward `{APP_NAME}?/a`"])
        platform = self.good_optional()
        platform["features"]["hostsim"] = [f"dep:{APP_NAME}", "dep:lower"]
        self.assertEqual(problems(self.OPT_APP, platform, set()),
                         [f"`hostsim` enables the application without forwarding `{APP_NAME}/hostsim`"])

    def test_a_dependency_the_empty_library_does_not_use_must_wait_for_hostsim(self):
        platform = self.good_optional()
        platform["dependencies"]["lower"] = {"path": "../lower"}
        self.assertEqual(len(problems(self.OPT_APP, platform, set())), 1)
        platform = self.good_optional()
        platform["features"]["hostsim"].remove("dep:lower")
        self.assertEqual(len(problems(self.OPT_APP, platform, set())), 1)


if __name__ == "__main__":
    sys.exit(unittest.main(verbosity=1))
