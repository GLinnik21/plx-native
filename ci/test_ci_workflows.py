#!/usr/bin/env python3
"""Pins the shape of CI changes whose failure is silent (everything stays green).

* the site film refreshes after a stable release only, never blocks one, and holds its one write token in `publish` (`SiteVideoRelease`);
* `cache-cleanup.yml` runs on `pull_request_target`, which is safe only while it never checks out
  or runs pull request code and holds nothing but `actions: write`;
* the nightly's and the release candidate's Sentry debug-file upload is skipped on a pull
  request's dry run and nowhere else (a release, the schedule and a dispatched dry run keep it);
* the tests against the bundled FFmpeg and libass run in a job of their own, beside the replays,
  and still run;
* the host FFmpeg and libass builds in the simulator jobs are CACHED where the build scripts really
  write, under a key that names every input the scripts key on (a cache that "hits" and still
  rebuilds, or one that outlives a changed input, both stay green);
* ci.yml starts on every pull request and its `changes` job gates the heavy jobs; both required
  status names report always (`RequiredChecksReportOnEveryPr`);
* ci.yml's `paths` filter (`CiPathFilter`, evaluated by ci/ci_paths.py, the code the `changes` job runs): evaluated with GitHub's rules, a docs-only or
  repository-metadata-only change (issue forms, a PR template, prose, images) starts no run, while
  every file a gate reads (PRIVACY.md, the credits pair, an `include_str!` target, the workflows)
  still does, and no other workflow carries a catch-all `'**'` filter;
* the nightly Homebrew Channel repository: the nightly build generates its manifest (and debug never
  does), a real nightly can only be cut from main, the manifest reaches the release, and the site
  stages the repository and the guide.

Text-level on purpose, like ci/test_ci_timeouts.py (no YAML library on a stock runner).
"""
from __future__ import annotations

import importlib.util
import json
import os
import re
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = ROOT / ".github/workflows"
sys.path.insert(0, str(ROOT / "ci"))
import ci_paths  # noqa: E402
import deps  # noqa: E402


def text(name):
    return (WORKFLOWS / name).read_text()


def code(name):
    """The workflow without comment lines, so a comment cannot satisfy or trip an assertion."""
    return "\n".join(l for l in text(name).splitlines() if not l.lstrip().startswith("#"))


def job_body(name, job):
    lines = code(name).splitlines()
    start = next(i for i, l in enumerate(lines) if l == f"  {job}:")
    end = next((i for i in range(start + 1, len(lines)) if re.match(r"^  [A-Za-z0-9_-]+:\s*$", lines[i])), len(lines))
    return "\n".join(lines[start:end])


class CacheCleanup(unittest.TestCase):
    def test_privileged_trigger_never_runs_pull_request_code(self):
        body = code("cache-cleanup.yml")
        self.assertRegex(body, r"(?m)^  pull_request_target:\n    types: \[closed\]")
        self.assertNotIn("checkout", body)
        self.assertNotIn("actions/", body.replace("actions: write", ""))
        # Only the number is read from the event, and through the environment, not the script.
        self.assertEqual(sorted(set(re.findall(r"github\.event\.[A-Za-z_.]+", body))),
                         ["github.event.pull_request.number"])
        self.assertNotRegex(body, r"run:[^\n]*\$\{\{")
        self.assertNotIn("\n      - uses:", body)

    def test_token_holds_actions_write_and_nothing_else(self):
        body = code("cache-cleanup.yml")
        block = re.search(r"(?m)^permissions:\n((?:  .*\n)+)", body + "\n").group(1)
        self.assertEqual(block.split(), ["actions:", "write"])
        self.assertNotIn("permissions:", job_body("cache-cleanup.yml", "delete-pull-request-caches"))

    def test_prune_is_manual_and_dry_by_default(self):
        body = code("cache-prune.yml")
        self.assertRegex(body, r"(?m)^on:\n  workflow_dispatch:")
        self.assertNotRegex(body, r"(?m)^  (push|pull_request|pull_request_target|schedule):")
        self.assertRegex(body, r"dry_run:[\s\S]*?default: true")
        self.assertIn("--delete", body)


class SentryUpload(unittest.TestCase):
    def test_only_a_pull_requests_nightly_run_skips_the_upload(self):
        nightly = code("nightly.yml")
        self.assertEqual(re.findall(r"upload-debug-files: (.*)", nightly),
                         ["${{ github.event_name != 'pull_request' }}"])
        self.assertNotIn("upload-debug-files", code("release.yml"))

    def test_only_a_pull_requests_candidate_run_skips_the_upload(self):
        self.assertEqual(re.findall(r"upload-debug-files: (.*)", code("rc.yml")),
                         ["${{ github.event_name != 'pull_request' }}"])

    def test_the_input_defaults_to_uploading_and_the_step_honours_it(self):
        build = code("build-package.yml")
        self.assertRegex(build, r"upload-debug-files:\n(?:        .*\n)*?        type: boolean\n        default: true\n")
        step = build[build.index("Upload debug files to Sentry"):build.index("ELF + packaging assertions")]
        self.assertIn("UPLOAD: ${{ inputs.upload-debug-files }}", step)
        self.assertIn("ci/check-debug-files.py", step)
        self.assertLess(step.index("ci/check-debug-files.py"), step.index('"$UPLOAD" != true'))
        self.assertLess(step.index('"$UPLOAD" != true'), step.index("debug-files upload"))


class BundledLibrarySymbols(unittest.TestCase):
    """A frame inside a bundled `.so` symbolicates only if its unstripped symbols were kept, carry the
    shipped copy's build id, were checked to show symbols, and were uploaded. Every link in that chain
    fails silently (the upload still succeeds), so each is pinned here."""

    def step(self):
        build = code("build-package.yml")
        return build[build.index("Upload debug files to Sentry"):build.index("ELF + packaging assertions")]

    def test_every_symbol_file_is_checked_for_features_before_the_token_guarded_upload(self):
        step = self.step()
        self.assertIn("python3 ci/check-debug-files.py pkg/plxnative.debug pkg/symbols/*.debug", step)
        self.assertLess(step.index("ci/check-debug-files.py"), step.index('"$UPLOAD" != true'))
        self.assertLess(step.index('"$UPLOAD" != true'), step.index("debug-files upload pkg/symbols"))

    def test_libraries_upload_without_sources_and_sentry_crash_is_not_uploaded(self):
        step = self.step()
        self.assertNotIn("sentry-crash", step)
        self.assertNotRegex(step, r"--include-sources[^\n]*pkg/symbols")
        self.assertEqual(len(re.findall(r"debug-files upload", step)), 2)

    def test_the_build_makes_them_and_the_build_asserts_one_per_shipped_library(self):
        build = code("build-package.yml")
        self.assertIn("make ipk symbols symbols-libs", build)
        self.assertIn('pkg/symbols/$(basename "$lib").debug', build)

    def test_the_symbol_files_are_kept_as_a_separate_build_artifact(self):
        build = code("build-package.yml")
        self.assertIn("name: ${{ inputs.artifact }}-symbols", build)
        self.assertIn("path: pkg/symbols/*.debug", build)
        # ...never inside the artifact that becomes the release's dist/.
        main = build[build.index("name: ${{ inputs.artifact }}\n"):build.index("name: ${{ inputs.artifact }}-symbols")]
        self.assertNotIn("pkg/symbols", main)

    def test_the_cross_builds_give_the_libraries_a_build_id_and_ffmpeg_keeps_the_unstripped_copy(self):
        ffmpeg = (ROOT / "ci/build-ffmpeg.sh").read_text()
        # cross branch only: the HOST build links with Apple's ld, which has no --build-id
        self.assertEqual(ffmpeg.count("--build-id=sha1"), 2)   # the comment names it once more
        self.assertIn("--sysroot=\"$SYSROOT\" \\\n    --extra-ldflags='-Wl,--build-id=sha1'", ffmpeg)
        # the copy is taken BEFORE the strip, from the link output, into a directory no lib glob reaches
        keep = ffmpeg.index('cp "$elf" "$PREFIX/lib/unstripped/')
        self.assertLess(keep, ffmpeg.index('"${CROSS}strip" --strip-unneeded "$target"'))
        libass = (ROOT / "ci/build-libass.py").read_text()
        self.assertIn("'-Wl,--build-id=sha1'", libass)
        self.assertIn("if not host:", libass[libass.index("--version-script"):][:400])

    def test_makefile_target_covers_ffmpeg_and_libass_from_their_unstripped_copies(self):
        make = (ROOT / "Makefile").read_text()
        self.assertIn("FFMPEG_UNSTRIPPED_DIR = $(FFMPEG_PREFIX)/lib/unstripped", make)
        self.assertIn("LIBASS_UNSTRIPPED     = vendor/libass-prefix/lib/libass-plx.so.0", make)
        self.assertIn("symbols-libs: $(SO_SYMBOLS)", make)
        self.assertIn("sentry-symbols: symbols symbols-libs", make)
        self.assertRegex(make, r"\.PHONY:[^\n]* symbols-libs ")
        self.assertIn("pkg/symbols/", (ROOT / ".gitignore").read_text())


class NightlyHomebrewRepository(unittest.TestCase):
    def test_the_nightly_build_generates_its_manifest_and_release_does_too(self):
        self.assertEqual(re.findall(r"homebrew-manifest: (.*)", code("nightly.yml")), ["true"])
        self.assertEqual(re.findall(r"homebrew-manifest: (.*)", code("release.yml")), ["true"])

    def test_only_debug_is_refused_a_manifest(self):
        build = code("build-package.yml")
        self.assertIn('[ "$HOMEBREW_MANIFEST" = true ] && [ "$FLAVOR" = debug ]', build)
        self.assertNotIn('[ "$FLAVOR" != stable ]', build)

    def test_the_manifest_is_named_for_the_app_id_it_describes(self):
        build = code("build-package.yml")
        start = build.index("Generate the Homebrew Channel manifest")
        step = build[start:build.index("LGPL corresponding source", start)]
        self.assertIn("app_id=com.beb.plxnative.nightly", step)
        self.assertIn("app_id=com.beb.plxnative\n", step)
        self.assertIn('-o "pkg/${app_id}.manifest.json"', step)
        # the hash gate also pins the id and the version, not just the bytes
        self.assertIn("manifest['id']}_{manifest['version']}_arm.ipk", step)

    def test_a_real_nightly_is_refused_off_main_and_a_dry_run_is_not(self):
        plan = job_body("nightly.yml", "plan")
        guard = plan[plan.index("Refuse a real nightly from any branch but main"):plan.index("- id: date")]
        self.assertIn("github.event_name == 'workflow_dispatch' && inputs.dry_run != true", guard)
        self.assertIn('"$REF" = "refs/heads/main"', guard)

    def test_the_manifest_is_published_with_the_release_and_checked_after(self):
        publish = job_body("nightly.yml", "publish")
        self.assertIn("dist/com.beb.plxnative.nightly.manifest.json", publish.split("gh release create")[1].split("--target")[0])
        self.assertLess(publish.index("gh release create"),
                        publish.index("--pattern com.beb.plxnative.nightly.manifest.json"))

    def test_check_package_grades_a_nightly_without_the_build_environment(self):
        # In CI, check-package.py runs as its OWN step, without the PLX_NIGHTLY_DATE the build step
        # had. It must take the date from the build stamp, or flavor.control_for dies at import and
        # every nightly fails the packaging gate (a Makefile run passes only because its recipe
        # environment carries the exported date).
        checker = (WORKFLOWS.parent.parent / "ci/check-package.py").read_text()
        self.assertIn('flavor.control_for((ROOT / "ipkroot/ctl/control").read_text(), FLAVOR or "stable", _NIGHTLY_DATE)',
                      checker)

    def test_the_source_bundle_is_named_for_the_reported_version_not_the_package_filename(self):
        # The package version carries the cut date as its patch, so `<filename version>-nightly-<date>`
        # names a version nothing reports; the label comes from check-package's own derivation.
        build = code("build-package.yml")
        self.assertIn("--print-nightly-label pkg/.build-config", build)
        self.assertIn('label="${nightly_label:-$version${rc:+-rc.$rc}}"', build)
        self.assertNotIn('label="$version${nightly_date:+-nightly-$nightly_date}', build)

    def test_the_site_stages_the_repository_and_the_guide(self):
        pages = code("pages.yml")
        self.assertIn('ci/nightly.py repo-json --repo "${{ github.repository }}" --out-dir _site/nightly', pages)
        self.assertIn("render-doc-page.py docs/nightly-builds.md > _site/nightly/index.html", pages)
        self.assertIn('"docs/nightly-builds.md"', pages)
        # the repository files must land AFTER the guide page, in the same directory
        self.assertLess(pages.index("docs/nightly-builds.md > _site/nightly/index.html"),
                        pages.index("ci/nightly.py repo-json"))

    def test_the_site_stamps_the_hero_version_from_the_package(self):
        pages = code("pages.yml")
        self.assertIn('"pkg/appinfo.json"', pages)
        self.assertIn('json.load(open("pkg/appinfo.json"))["version"]', pages)
        # the stamp edits the staged copy, after it is copied, and fails unless it hit exactly one line
        self.assertLess(pages.index("cp site/index.html _site/index.html"), pages.index('path = "_site/index.html"'))
        self.assertIn("if n != 1:", pages)
        # the pattern it rewrites is still the source's hero line
        hero = re.findall(r"Version \d+\.\d+\.\d+(?=<span)", (ROOT / "site/index.html").read_text())
        self.assertEqual(len(hero), 1, hero)


class HostToolTests(unittest.TestCase):
    def test_ffmpeg_and_ass_tests_run_in_their_own_job_not_behind_the_replay(self):
        sim = code("simulators.yml")
        macos, tests = job_body("simulators.yml", "macos"), job_body("simulators.yml", "macos-host-tests")
        for gate in ("make check-ffmpeg", "make check-ass"):
            self.assertEqual(sim.count(gate), 1, gate)
            self.assertIn(gate, tests)
            self.assertNotIn(gate, macos)
        self.assertNotIn("needs:", tests)
        # The simulator job keeps everything else it had.
        for step in ("make sim-macos", "tests/replay_fixtures.py", "tools/sim-smoke.py"):
            self.assertIn(step, macos)


def cache_steps(job):
    """Every `actions/cache` step of a simulators.yml job: {"path": [...], "key": str}."""
    found = []
    for step in re.split(r"(?m)^      - ", job_body("simulators.yml", job))[1:]:
        if "uses: actions/cache@" not in step:
            continue
        lines = step.splitlines()
        at = next(i for i, l in enumerate(lines) if l.strip().startswith("path:"))
        inline = lines[at].split("path:", 1)[1].strip()
        paths = [inline] if inline not in ("", "|") else []
        for l in lines[at + 1:]:
            if not l.startswith("            "):
                break
            paths.append(l.strip())
        key = next(l.split("key:", 1)[1].strip() for l in lines if l.strip().startswith("key:"))
        found.append({"path": paths, "key": key})
    return found


def cache_of(job, path):
    """The one cache step of `job` that stores `path`."""
    hits = [c for c in cache_steps(job) if path in c["path"]]
    assert len(hits) == 1, f"{job}: {len(hits)} cache steps store {path}"
    return hits[0]


def hashed(key):
    """The files a key passes to hashFiles()."""
    return set(re.findall(r"'([^']+)'", " ".join(re.findall(r"hashFiles\(([^)]*)\)", key))))


def makefile_list(variable):
    """The words of a Makefile variable assigned with backslash continuations."""
    text = (ROOT / "Makefile").read_text()
    body = re.search(rf"(?m)^{variable}\s*=((?:.*\\\n)*.*)$", text).group(1)
    return body.replace("\\\n", " ").split()


class HostLibraryCaches(unittest.TestCase):
    """A cache hit must mean no rebuild, and a changed input must mean one.

    The first version cached `vendor/ffmpeg-prefix-host` and `vendor/ffmpeg-build`, logged "Cache
    hit", and rebuilt FFmpeg anyway on every run (libass, which had no cache at all, likewise): the
    restored header is older than the fresh checkout's `ci/build-ffmpeg.sh`, so make re-ran the
    script, and the script keeps its objects under `~/.cache/plxnative/ffmpeg`, which nothing saved.
    """

    MACOS_JOBS = ("macos", "macos-host-tests")
    LIBASS_JOBS = ("linux", "macos", "macos-host-tests")
    FFMPEG_WORK = "~/.cache/plxnative/ffmpeg"
    LIBASS_PREFIX = "vendor/libass-prefix-host"
    DOWNLOADS = "vendor/downloads"

    def test_no_cache_stores_a_prefix_make_would_judge_by_mtime(self):
        # A restored prefix is OLDER than the checkout's script, so make re-runs the script. Cache
        # the script's own work tree instead, which the script re-validates by content.
        for job in self.LIBASS_JOBS:
            for cache in cache_steps(job):
                for path in cache["path"]:
                    self.assertNotIn("ffmpeg-prefix", path, job)
                    self.assertNotIn("ffmpeg-build", path, job)
        self.assertNotIn("restore-keys", code("simulators.yml"),
                         "a prefix match would restore a build of different inputs")

    def test_the_cache_paths_are_where_the_scripts_write(self):
        sh = (ROOT / "ci/build-ffmpeg.sh").read_text()
        self.assertIn("CACHE_ROOT=${PLX_BUILD_CACHE-$HOME/.cache/plxnative}", sh)
        self.assertIn('WORK="$CACHE_ROOT/ffmpeg/$ARCHTAG-$VERSION-$KEY"', sh)
        # The pinned tarball lives in the same directory, so a hit needs no download either.
        self.assertIn('CACHED_TAR="$CACHE_ROOT/ffmpeg/ffmpeg-$VERSION-', sh)
        py = (ROOT / "ci/build-libass.py").read_text()
        self.assertIn(f"ROOT / ('{self.LIBASS_PREFIX}' if host", py)
        self.assertIn("sources = deps.cache_dir()", py)
        self.assertEqual(deps.cache_dir(), ROOT / self.DOWNLOADS)
        self.assertIn("stamp = prefix / '.dependencies-key'", py)
        for job in self.MACOS_JOBS:
            cache_of(job, self.FFMPEG_WORK)
        for job in self.LIBASS_JOBS:
            cache_of(job, self.LIBASS_PREFIX)
            self.assertIn("uses: ./.github/actions/deps-cache", job_body("simulators.yml", job), job)

    def test_the_ffmpeg_key_names_every_input_the_script_keys_on(self):
        sh = (ROOT / "ci/build-ffmpeg.sh").read_text()
        # The script hashes these into its own tree key; the workflow key must move with them.
        inherited = set(re.findall(r'"\$ROOT/(ci/[\w.-]+)"', re.search(r"INHERITED=.*", sh).group(0)))
        self.assertEqual(inherited, {"ci/arm-cc.py", "ci/check-link-evidence.py"})
        for job in self.MACOS_JOBS:
            key = cache_of(job, self.FFMPEG_WORK)["key"]
            self.assertEqual(hashed(key), {"ci/build-ffmpeg.sh", "ci/deps.py", "ci/dependencies.json", *inherited}, job)
            self.assertIn("runner.os", key)
            self.assertIn("runner.arch", key)
            # The compiler, SDK and build environment are inputs the script reads from the machine.
            self.assertIn("steps.toolchain.outputs.id", key)
        self.assertEqual(cache_of("macos", self.FFMPEG_WORK)["key"],
                         cache_of("macos-host-tests", self.FFMPEG_WORK)["key"],
                         "one entry must serve both macOS jobs")

    def test_the_libass_key_names_every_input_the_recipe_lists(self):
        py = (ROOT / "ci/build-libass.py").read_text()
        # Everything build-libass.py folds into its own `.dependencies-key`, besides the toolchain.
        # The pinned patches are folded into the key too (their bytes), by the path the pins name.
        pins = json.loads((ROOT / "ci/dependencies.json").read_text())
        patches = {patch["path"] for dep in pins if dep.get("group") == "libass"
                   for patch in dep.get("patches", [])}
        self.assertTrue(patches, "the font-face patch is pinned")
        read = {"ci/build-libass.py", "ci/dependencies.json", "ci/deps.py", *patches,
                *re.findall(r"ROOT / '(ci/[\w.-]+)'\)\.read_bytes", py)}
        listed = set(makefile_list("LIBASS_INPUTS"))
        self.assertTrue(read <= listed, read - listed)
        for job in self.LIBASS_JOBS:
            key = cache_of(job, self.LIBASS_PREFIX)["key"]
            self.assertEqual(hashed(key), listed, job)
            self.assertIn("runner.os", key)
            self.assertIn("runner.arch", key)
            self.assertIn("steps.toolchain.outputs.id", key)
        self.assertEqual(len({cache_of(j, self.LIBASS_PREFIX)["key"] for j in self.LIBASS_JOBS}), 1)

    def test_the_simulator_path_filter_names_every_libass_input_but_the_cache_keys_files(self):
        # A change to a patch or a facade source must start the workflow that rebuilds libass.
        text = (ROOT / ".github/workflows/simulators.yml").read_text()
        filter_ = text[:text.index("\njobs:")]
        for path in makefile_list("LIBASS_INPUTS"):
            if path.startswith(("src/", "include/", "ci/patches/libass-", "ci/build-libass")):
                self.assertIn("'" + path + "'", filter_, path)

    def test_the_toolchain_is_identified_before_any_cache_is_restored(self):
        for job in self.LIBASS_JOBS:
            body = job_body("simulators.yml", job)
            step = body.index("id: toolchain")
            self.assertIn("ci/host-toolchain-id.sh", body[step:step + 200])
            self.assertIn('"$GITHUB_OUTPUT"', body[step:step + 200])
            self.assertLess(step, body.index("uses: actions/cache@"), job)
        triggers = text("simulators.yml").split("pull_request:")[0]
        self.assertIn("'ci/host-toolchain-id.sh'", triggers)

    def test_the_toolchain_id_moves_with_what_a_build_reads_from_the_machine(self):
        names = ("CFLAGS", "CPPFLAGS", "LDFLAGS", "CXXFLAGS", "PKG_CONFIG_PATH", "RELEASE")

        def ident(**extra):
            env = {k: v for k, v in os.environ.items() if k not in names}
            env.update(extra)
            return subprocess.run(["sh", str(ROOT / "ci/host-toolchain-id.sh")], env=env,
                                  capture_output=True, text=True, check=True).stdout.strip()

        base = ident()
        # Hex, because tools/prune-gh-caches.py reads a trailing hex segment as a hash generation.
        self.assertRegex(base, r"^[0-9a-f]{16}$")
        self.assertEqual(base, ident())
        for name in names:
            self.assertNotEqual(base, ident(**{name: "1"}), name)

    def test_a_cache_miss_still_builds_from_the_pinned_sources(self):
        # The macOS tests job exists to test the bundled libraries built from the pinned sources; a
        # cached build of the same inputs is the same thing, and every build step is still there.
        self.assertIn("make check-ffmpeg", job_body("simulators.yml", "macos-host-tests"))
        self.assertIn("make check-ass", job_body("simulators.yml", "macos-host-tests"))
        self.assertIn("make sim-macos", job_body("simulators.yml", "macos"))
        self.assertIn("make sim-linux", job_body("simulators.yml", "linux"))


class DependabotNotices(unittest.TestCase):
    """`dependabot-notices.yml` runs on `pull_request_target` with a write token, which is safe only while
    it executes nothing from the pull request: gated on the bot and a same-repository head, the PR
    checked out as data beside the trusted base-branch generator, no credential persisted, write
    permissions on the one job only."""

    NAME = "dependabot-notices.yml"

    def test_trigger_is_scoped_to_the_cargo_files(self):
        body = code(self.NAME)
        self.assertRegex(body, r"(?m)^  pull_request_target:\n    types: \[opened, synchronize, reopened\]\n    paths:")
        self.assertNotRegex(body, r"(?m)^  (push|pull_request|workflow_run|issue_comment|schedule):")
        self.assertRegex(body, r"(?m)^permissions: \{\}$")

    def test_the_job_is_gated_on_the_bot_and_a_same_repository_head(self):
        job = job_body(self.NAME, "regenerate")
        for clause in ("github.actor == 'dependabot[bot]'", "github.event.pull_request.user.login == 'dependabot[bot]'",
                       "github.event.pull_request.head.repo.full_name == github.repository",
                       "github.event.pull_request.base.ref == github.event.repository.default_branch"):
            self.assertIn(clause, job)
        self.assertEqual(re.findall(r"(?m)^      (\w+): write$", job), ["contents", "actions"])
        self.assertIn("timeout-minutes:", job)

    def test_pull_request_code_is_data_never_executed(self):
        job = job_body(self.NAME, "regenerate")
        # any pinned SHA and any `# vX.Y.Z` comment: Dependabot bumps both, the security shape is what is asserted
        checkouts = re.findall(r"actions/checkout@[0-9a-f]{40}[^\n]*\n        with:\n((?:          .*\n)+)", job)
        self.assertEqual(len(checkouts), 2)
        for block in checkouts:
            self.assertIn("persist-credentials: false", block)
        self.assertIn("path: trusted", checkouts[0])
        self.assertIn("path: pr", checkouts[1])
        self.assertNotIn("ref:", checkouts[0])   # the base branch
        self.assertIn("ref: ${{ github.event.pull_request.head.sha }}", checkouts[1])
        # the single code path: the base branch's generator, isolated, pointed at the PR's files
        self.assertIn("python3 -I trusted/ci/gen-third-party-notices.py --root pr", job)
        self.assertNotRegex(job, r"(?m)^\s+(?:make|cargo|\./|bash pr|python3 pr|sh pr)\b")
        self.assertNotIn("uses: ./", job)
        # PR-controlled strings reach the shell only through env:, never `${{ }}` inside a script
        for run in re.findall(r"(?ms)^        run: \|\n(.*?)(?=^      - |\Z)", job):
            self.assertNotIn("${{", run)

    def test_a_new_crate_stops_the_job_before_anything_is_committed(self):
        job = job_body(self.NAME, "regenerate")
        # two generator steps (the std snapshot of a toolchain bump, then the lock): both refuse a new crate
        refusals = re.findall(r"\n            2\)(.*?)\n            \*\)", job, re.S)
        self.assertEqual(len(refusals), 2)
        for refusal in refusals:
            self.assertIn("exit 1", refusal)
            self.assertIn("licence decision", refusal)
        self.assertLess(job.rindex("exit 1"), job.index("git -C pr commit"))

    def test_a_toolchain_bump_is_re_derived_from_the_new_release_before_the_notices_are_generated(self):
        job = job_body(self.NAME, "regenerate")
        triggers = code(self.NAME)
        self.assertIn("rust-modules/rust-toolchain.toml", triggers[:triggers.index("jobs:")])
        steps = [job.index(s) for s in ("--print-stale-pin", "rustup toolchain install", "--derive-toolchain",
                                        "gen-third-party-notices.py --root pr >gen.out")]
        self.assertEqual(steps, sorted(steps))
        # the install and the derive run only when the pin moved; the pin reaches the shell through env:
        self.assertEqual(job.count("if: steps.pin.outputs.pin != ''"), 2)
        self.assertIn("PIN: ${{ steps.pin.outputs.pin }}", job)
        self.assertIn('rustup toolchain install "$PIN" --profile minimal --component rust-src', job)
        # the data file may be pushed only with a moved pin
        self.assertIn("PIN_MOVED", job)
        self.assertIn("ci/third-party-crates.json", job[job.index("allowed="):])

    def test_cargo_never_runs_here_and_the_backstop_is_in_ci(self):
        # A PR's .cargo/config.toml can name a credential-provider program, so cargo on its files with a
        # write token is refused; ci.yml's host-lint job proves the crate set with cargo instead.
        self.assertNotRegex(code(self.NAME), r"(?m)^\s+(?:run: )?.*\bcargo\b")
        lint = job_body("ci.yml", "host-lint")
        self.assertIn("python3 ci/gen-third-party-notices.py --check --verify-graph", lint)
        header = (WORKFLOWS / self.NAME).read_text()
        self.assertIn("@dependabot recreate", header)
        self.assertNotIn("`@dependabot rebase` or", header)


class ToolchainPin(unittest.TestCase):
    """`rust-modules/rust-toolchain.toml` is the ONE spelling of the Rust release, so a Dependabot bump of it
    (the `rust-toolchain` entry of dependabot.yml) is a one-line change that cannot leave a stale copy."""

    # The files that must carry the release: the pin itself, and the snapshot / generated table derived from it
    # (ci/gen-third-party-notices.py checks them against the pin, and --derive-toolchain rewrites them).
    DERIVED = {"rust-modules/rust-toolchain.toml", "ci/third-party-crates.json", "THIRD-PARTY-NOTICES.md"}

    def test_dependabot_bumps_the_toolchain_file(self):
        body = (ROOT / ".github/dependabot.yml").read_text()
        entry = body[body.index("- package-ecosystem: rust-toolchain"):]
        entry = entry[:entry.index("\n\n")]
        self.assertIn("directory: /rust-modules", entry)
        self.assertIn("interval: weekly", entry)
        self.assertIn("prefix: Deps", entry)
        self.assertRegex((ROOT / "rust-modules/rust-toolchain.toml").read_text(), r'(?m)^channel = "1\.\d+\.\d+"$')

    def test_no_second_file_spells_the_release(self):
        import rust_toolchain
        pin = rust_toolchain.pinned_channel()
        literal = re.compile(r"(?<![\w.])" + re.escape(pin) + r"(?![\w.]|-)")
        files = subprocess.run(["git", "-C", str(ROOT), "ls-files", "-z"], capture_output=True, text=True, check=True).stdout.split("\0")
        stray = []
        for rel in filter(None, files):
            if rel in self.DERIVED or rel.endswith((".lock", ".svg", ".png", ".jpg", ".ico", ".woff2", ".ttf", ".gz", ".zip")):
                continue
            try:
                body = (ROOT / rel).read_text()
            except (UnicodeDecodeError, OSError):
                continue
            if literal.search(body):
                stray.append(rel)
        self.assertEqual(stray, [], f"{pin} is spelled outside rust-modules/rust-toolchain.toml: read the file instead of copying the version")


class BuildBenchWorkflow(unittest.TestCase):
    """The daily build-time measurement (build-bench.yml): a schedule that can only ever run main's tip.

    It runs a macOS runner for tens of minutes a day and its second job holds a token that can push to
    a branch, so the shape that keeps both bounded is pinned: no trigger a pull request can pull, the
    repository and default-branch guards, an empty workflow-level grant, the write permission on the
    publish job alone, bounded timeouts.
    """

    NAME = "build-bench.yml"

    def test_the_only_triggers_are_the_schedule_and_a_manual_dispatch(self):
        body = code(self.NAME)
        on = body[body.index("\non:\n") + 1:body.index("\npermissions:")]
        self.assertEqual(re.findall(r"(?m)^  ([a-z_]+):", on), ["schedule", "workflow_dispatch"])
        self.assertRegex(on, r"(?m)^    - cron: \"\d+ \d+ \* \* \*\"$")   # once a day, not hourly
        for trigger in ("pull_request", "pull_request_target", "push", "workflow_run", "issue_comment"):
            self.assertNotIn(trigger, on)

    def test_it_runs_only_for_this_repository_and_its_default_branch(self):
        for job in ("measure", "gate"):
            body = job_body(self.NAME, job)
            self.assertIn("github.repository == 'GLinnik21/plx-native'", body, job)
            self.assertIn("github.ref_name == github.event.repository.default_branch", body, job)

    def test_the_workflow_grants_nothing_and_only_publish_may_write(self):
        body = code(self.NAME)
        self.assertRegex(body, r"(?m)^permissions: \{\}$")
        for job in ("measure", "gate"):
            self.assertEqual(re.findall(r"contents: (\w+)", job_body(self.NAME, job)), ["read"], job)
        self.assertEqual(re.findall(r"contents: (\w+)", job_body(self.NAME, "publish")), ["write"])
        self.assertEqual(len(re.findall(r"(?m)^\s+[a-z-]+: (?:write|read)\b", body)), 3)
        # nothing in the jobs that run the build can push: their checkouts keep no credential
        for job in ("measure", "gate"):
            self.assertIn("persist-credentials: false", job_body(self.NAME, job), job)

    def test_the_jobs_are_bounded_with_room_to_spare(self):
        # The first real run took 29 minutes of a 35-minute limit; the limits now leave a slow day
        # more than twice what a leg is expected to take (~13 min base, ~10 min top).
        for job, ceiling in (("measure", 30), ("gate", 40), ("publish", 10)):
            m = re.search(r"(?m)^    timeout-minutes: (\d+)$", job_body(self.NAME, job))
            self.assertIsNotNone(m, job)
            self.assertLessEqual(int(m.group(1)), ceiling, job)
        self.assertGreaterEqual(int(re.search(r"(?m)^    timeout-minutes: (\d+)$", job_body(self.NAME, "measure")).group(1)), 25)

    def test_the_measurement_is_split_into_legs_that_every_scenario_belongs_to_once(self):
        measure = job_body(self.NAME, "measure")
        legs = dict(re.findall(r"- leg: (\w+)\n\s+daily: (\S+)", measure))
        self.assertEqual(sorted(legs), ["base", "top"])
        daily = [sid for ids in legs.values() for sid in ids.split(",")]
        self.assertEqual(len(daily), len(set(daily)), "a scenario in two legs is timed twice")
        top = re.search(r"TOP: (\S+)", measure).group(1).split(",")
        self.assertEqual(sorted(top), sorted(legs["top"].split(",")), "the plan step must route a manual list the way the daily one is split")
        self.assertIn("fail-fast: false", measure)       # a failing leg must not cancel the other
        self.assertNotIn("check", daily)                 # the gate has its own job
        self.assertIn("--runs 3", measure)               # every row keeps three runs
        # every leg leaves its own document for the publish job
        self.assertIn("build-bench-rows-${{ matrix.leg }}", measure)
        self.assertIn("bench-$LEG.json", measure)

    def test_a_red_gate_makes_the_run_red_but_not_the_record_empty(self):
        gate = job_body(self.NAME, "gate")
        step = gate[gate.index("the whole gate once"):gate.index("Keep the document")]
        # no continue-on-error: this was how a broken gate stayed invisible
        self.assertNotIn("continue-on-error", job_body(self.NAME, "gate"))
        self.assertNotIn("continue-on-error", job_body(self.NAME, "measure"))
        self.assertRegex(step, r"timeout-minutes: \d+")
        self.assertIn("--only check", step)
        # the document is uploaded however the step ended
        upload = gate[gate.index("Keep the document"):]
        self.assertIn("if: always()", upload)
        # ...and the rows job never runs it (a slow, runner-dependent row must not delay the rest)
        self.assertNotIn("--only check", job_body(self.NAME, "measure"))

    def test_the_record_waits_for_the_measurements_but_not_for_them_to_be_green(self):
        publish = job_body(self.NAME, "publish")
        self.assertIn("needs: [measure, gate]", publish)
        # runs when a measurement job was red (its document is still there), not when nothing ran or the run was cancelled
        self.assertIn("!cancelled() && needs.measure.result != 'skipped'", publish)
        self.assertNotIn("needs.measure.result == 'success'", publish)
        self.assertNotIn("needs.gate", publish)
        # a leg that left no document records nothing, rather than a day without half its rows
        self.assertIn("left no document: nothing recorded", publish)
        self.assertRegex(publish, r"python3 \.\./tools/build-history\.py")
        self.assertRegex(publish, r"python3 \.\./tools/ci-summary\.py --history ci-history\.json --bench build-history\.json --quarantine \.\./ci/flaky\.txt --out ci-summary\.json")

    def test_measurements_do_not_overlap_and_a_lost_push_race_is_retried(self):
        body = code(self.NAME)
        self.assertRegex(body, r"(?m)^concurrency:\n  group: build-bench\n  cancel-in-progress: false$")
        publish = job_body(self.NAME, "publish")
        self.assertIn("for attempt in 1 2 3", publish)
        self.assertIn("git reset -q --hard FETCH_HEAD", publish)
        # the data branch is only ever pushed to, never force-pushed
        self.assertNotIn("--force", publish)
        self.assertNotRegex(publish, r"push[^\n]* -f\b")
        self.assertIn("HEAD:refs/heads/ci-metrics", publish)

    def test_every_action_is_pinned_to_a_commit(self):
        for line in code(self.NAME).splitlines():
            m = re.match(r"\s*(?:- )?uses: (\S+)", line)
            if m and not m.group(1).startswith("./"):  # a composite action in this repository is not a pin target
                self.assertRegex(m.group(1), r"@[0-9a-f]{40}$", line)

    def test_the_daily_set_is_one_the_benchmark_knows(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location("build_bench_for_wf", ROOT / "tools" / "build-bench.py")
        bb = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(bb)
        daily = [sid for ids in re.findall(r"daily: (\S+)", code(self.NAME)) for sid in ids.split(",")]
        self.assertTrue(set(daily) <= set(bb.ALL_SCENARIOS), daily)
        self.assertNotIn("check", daily)   # the gate has its own, failure-tolerant step
        self.assertNotIn("arm", daily)     # no ARM archive on a runner; the benchmark never builds FFmpeg


class CiPathFilter(unittest.TestCase):
    """ci.yml's `paths` list (the `&ci_paths` anchor, shared by `push` and `pull_request`), run through
    GitHub's matching rules, because a wrong line here fails silently in BOTH directions: a gate-read
    file excluded means a run that should have caught a break never starts, and a prose file left
    in means ~20 runner-minutes per documentation edit (#261, #513).

    The rules (docs.github.com, "Workflow syntax", filter pattern cheat sheet): patterns are
    evaluated in order; a `!` pattern removes what an earlier pattern matched, a later positive
    pattern puts it back; `*` matches any characters but `/`; `**` matches any characters including
    `/`, and `**/` also matches no directory at all; `?` matches one character but `/`."""

    @staticmethod
    def anchor():
        return ci_paths.patterns(text("ci.yml"))

    @staticmethod
    def triggers(path, patterns=None):
        return ci_paths.triggers(path, patterns if patterns is not None else CiPathFilter.anchor())

    def test_only_push_filters_by_path_and_pull_request_starts_on_every_pr(self):
        # A PR the filter skips starts no run, so the required checks never report. The `changes` job
        # applies the same list to the PR's files instead (RequiredChecksReportOnEveryPr).
        self.assertRegex(code("ci.yml"), r"(?m)^  pull_request:\n  workflow_dispatch:")
        self.assertEqual(len(re.findall(r"(?m)^\s+paths:", code("ci.yml"))), 1)
        self.assertEqual(len(re.findall(r"(?m)^\s+paths: &ci_paths$", text("ci.yml"))), 1)
        self.assertNotIn("*ci_paths", code("ci.yml"))
        self.assertIn("paths: &ci_paths", code("ci.yml").split("  pull_request:")[0])

    def test_the_changes_jobs_evaluator_is_this_evaluator(self):
        # ci/ci_paths.py is what the job runs, on the list it reads from ci.yml: same code, same cases.
        def run(files):
            return subprocess.run(
                [sys.executable, str(ROOT / "ci/ci_paths.py")], input="\n".join(files), text=True,
                capture_output=True, check=True).stdout.strip()
        self.assertEqual(run(list(self.MUST_NOT_TRIGGER)), "false")
        self.assertEqual(run(["README.md", "docs/x.md", "Makefile"]), "true")
        for path in self.MUST_TRIGGER:
            self.assertEqual(run([path]), "true", path)
        for path in self.MUST_NOT_TRIGGER:
            self.assertEqual(run([path]), "false", path)
        self.assertEqual(run(["tools/requirements-docs.txt"]), "false")
        self.assertEqual(run([]), "true", "no files learned: fail closed")

    def test_the_matcher_follows_githubs_rules(self):
        t = self.triggers
        self.assertTrue(t("a/b/c.rs", ["**"]))
        self.assertTrue(t("x", ["**"]))
        self.assertFalse(t("a/b.md", ["**", "!a/**"]))
        self.assertTrue(t("a/b.md", ["**", "!a/**", "a/b.md"]), "a later positive pattern re-includes")
        self.assertFalse(t("a/b.md", ["**", "a/b.md", "!a/**"]), "a later negative pattern excludes again")
        self.assertTrue(t("a/b/c.md", ["**", "!a/*.md"]), "`*` does not cross `/`")
        self.assertFalse(t("a/b/c.md", ["**", "!a/**/*.md"]))
        self.assertFalse(t("a/c.md", ["**", "!a/**/*.md"]), "`**/` also matches no directory")
        self.assertFalse(t("README.md", ["**", "!README.md"]))
        self.assertTrue(t("docs/README.md", ["**", "!README.md"]), "a bare name is anchored at the root")

    # Every path here is read by a gate or ships, so skipping the run for it would hide a break.
    MUST_TRIGGER = (
        "rust-modules/ui/src/widgets.rs", "rust-modules/Cargo.lock", "rust-modules/build.rs", "src/main.c",
        "Makefile", ".gitignore", "ci/check-package.py", "ci/check-python-steps.txt",
        "tests/test_harness.py", "tests/README.md", "tools/check-parallel.py", "locales/en/settings.json",
        "pkg/appinfo.json", "assets/icons/play.svg",
        ".github/workflows/ci.yml", ".github/workflows/pages.yml", ".github/workflows/release.yml",
        ".github/actions/host-rust/action.yml",
        ".claude/hooks/outbound-guard.py", ".claude/hooks/outbound-guard-test.py",
        ".agents/skills/wake-tv/wake-tv.sh",
        "PRIVACY.md", "LICENSE", "LICENSING.md", "TRADEMARKS.md", "THIRD-PARTY-NOTICES.md",
        "docs/screenshots/CREDITS.md", "site/credits.html", "site/index.html", "site/ci/index.html",
        "docs/measurements/p1b-logs/pipe_abr_pin_320.log",
    )
    # Prose, images and repository metadata: no gate reads them (the evidence is in ci.yml).
    MUST_NOT_TRIGGER = (
        ".github/ISSUE_TEMPLATE/bug_report.yml", ".github/ISSUE_TEMPLATE/config.yml",
        ".github/FUNDING.yml", ".github/PULL_REQUEST_TEMPLATE.md", ".github/pull_request_template.md",
        ".github/CODEOWNERS", ".github/dependabot.yml",
        "README.md", "CONTRIBUTING.md", "SECURITY.md", "AGENTS.md", "CLAUDE.md",
        "docs/agent-reference.md", "docs/troubleshooting.md", "docs/release-notes/v0.8.0.md",
        "docs/plex-openapi.json", "docs/screenshots/home.jpg", "docs/assets/sentry-wordmark-dark.svg",
        "docs/webosbrew-package.yml", "docs/measurements/m2-verbose-rerun.txt",
        "site/404.html", "site/media/closeup-glass.jpg", "tools/render-doc-page.py",
        "rust-modules/media/src/player/CLAUDE.md", ".agents/skills/ui-sim/SKILL.md",
        ".claude/settings.json", ".claude/workflows/swarm-gate.js", ".claude/agents/doc-claim-auditor.md",
        ".codex/agents/doc-claim-auditor.toml",
    )

    def test_what_a_gate_reads_triggers(self):
        patterns = self.anchor()
        for path in self.MUST_TRIGGER:
            with self.subTest(path):
                self.assertTrue(self.triggers(path, patterns), f"{path} is read by a gate but starts no run")
                self.assertTrue((ROOT / path).exists(), f"{path} is gone: update MUST_TRIGGER")

    def test_prose_images_and_repository_metadata_do_not(self):
        patterns = self.anchor()
        for path in self.MUST_NOT_TRIGGER:
            with self.subTest(path):
                self.assertFalse(self.triggers(path, patterns), f"{path} starts the whole CI for nothing")

    def test_every_compile_time_include_triggers(self):
        # `include_str!`/`include_bytes!` pull a file into the build, so a change to it changes what the
        # crate tests (PRIVACY.md, LICENSE, the locale and fixture files...) compile against.
        patterns, seen = self.anchor(), 0
        include = re.compile(r'include_(?:str|bytes)!\(\s*"([^"]+)"')
        for folder, dirs, files in os.walk(ROOT / "rust-modules"):
            dirs[:] = [d for d in dirs if not d.startswith("target")]
            for name in files:
                if not name.endswith(".rs"):
                    continue
                source = Path(folder, name)
                for target in include.findall(source.read_text(errors="replace")):
                    resolved = (source.parent / target).resolve()
                    try:
                        relative = resolved.relative_to(ROOT).as_posix()
                    except ValueError:
                        continue
                    seen += 1
                    with self.subTest(f"{source.relative_to(ROOT).as_posix()} -> {relative}"):
                        self.assertTrue(self.triggers(relative, patterns), f"{relative} is include_str!ed")
        self.assertGreater(seen, 50)

    def test_only_ci_has_a_catch_all_filter(self):
        # Every other workflow lists the files it builds from, so a documentation push cannot start it.
        for wf in sorted(WORKFLOWS.glob("*.yml")):
            catch_all = re.findall(r"(?m)^\s+- ['\"]?\*\*['\"]?\s*$", code(wf.name))
            self.assertEqual(len(catch_all), 1 if wf.name == "ci.yml" else 0, wf.name)


class RequiredChecksReportOnEveryPr(unittest.TestCase):
    """`main` requires `host checks (NOT a device gate)` and `cross-build (armv7) + artifact assertions`.
    ci.yml starts on every PR; a `changes` job decides whether the heavy jobs run, and each required
    name is an always-running aggregator that is green when they were skipped for a reason."""

    HEAVY = ("host-lint", "host-unit-default", "host-unit-hostsim", "host-python", "host-python-harness", "cross-build")
    REQUIRED = {"host-checks": "host checks (NOT a device gate)",
                "cross-build-required": "cross-build (armv7) + artifact assertions"}

    def test_every_heavy_job_waits_for_changes_and_is_gated(self):
        for job in self.HEAVY:
            body = job_body("ci.yml", job)
            with self.subTest(job):
                self.assertRegex(body, r"(?m)^    needs: changes$")
                self.assertRegex(body, r"(?m)^    if: needs\.changes\.outputs\.gated == 'true'$")

    def test_no_job_escapes_the_gate(self):
        jobs = re.findall(r"(?m)^  ([a-z][a-z0-9-]*):$", code("ci.yml").split("\njobs:\n", 1)[1])
        self.assertEqual(sorted(jobs), sorted(("changes",) + self.HEAVY + tuple(self.REQUIRED)))

    def test_changes_job_is_cheap_least_privileged_and_fails_closed(self):
        body = job_body("ci.yml", "changes")
        self.assertIn("runs-on: ubuntu-latest", body)
        self.assertIn("pull-requests: read", body)
        self.assertEqual(code("ci.yml").count("pull-requests: read"), 1, "only that job may read pull requests")
        self.assertRegex(body, r"gated: \$\{\{ steps\.gate\.outputs\.gated \}\}")
        self.assertIn("ci/ci_paths.py", body)
        self.assertRegex(body, r"(?m)^          gated=true$")
        self.assertIn("--paginate", body)

    def test_required_names_exist_once_and_run_always(self):
        for job, name in self.REQUIRED.items():
            body = job_body("ci.yml", job)
            with self.subTest(job):
                self.assertIn(f"name: {name}", body)
                self.assertRegex(body, r"(?m)^    if: always\(\)$")
                self.assertIn("needs.changes.outputs.gated", body)
                self.assertIn("needs.changes.result", body)
            self.assertEqual(len(re.findall(rf"(?m)^    name: {re.escape(name)}$", code("ci.yml"))), 1, name)

    def test_the_heavy_cross_build_does_not_carry_the_required_name(self):
        self.assertNotIn("name: cross-build (armv7) + artifact assertions", job_body("ci.yml", "cross-build"))
        self.assertRegex(job_body("ci.yml", "cross-build-required"), r"needs: \[changes, cross-build\]")

    def test_a_skip_is_green_only_when_changes_said_gated_false(self):
        body = job_body("ci.yml", "cross-build-required")
        self.assertIn('[ "$CHANGES" = success ]', body)
        self.assertIn('[ "$BUILD" = success ]', body)
        self.assertIn('[ "$GATED" = false ] && [ "$BUILD" = skipped ]', body)
        host = job_body("ci.yml", "host-checks")
        self.assertIn('[ "$GATED" = false ]', host)
        self.assertIn("=skipped$", host)
        self.assertIn("=success$", host)


class SiteVideoRelease(unittest.TestCase):
    """The site's film refreshes after a STABLE release and never blocks one (docs/agent-reference.md, "The site
    demo video"). A hole in any of these stays green: a schedule would render hourly-long jobs for nothing, a
    write token on the render job would hand it to the code being rendered, a failing hook would turn a published
    release red, and a push with GITHUB_TOKEN deploys nothing."""

    def jobs(self):
        lines = code("site-video.yml").splitlines()
        start = lines.index("jobs:")
        return [m.group(1) for l in lines[start + 1:] if (m := re.match(r"^  ([A-Za-z0-9_-]+):\s*$", l))]

    def test_it_is_dispatched_only_never_scheduled_nor_started_by_a_push_or_a_pull_request(self):
        body = code("site-video.yml")
        on = body[body.index("\non:") + 1:body.index("\npermissions:")]
        self.assertEqual(re.findall(r"(?m)^  ([a-z_]+):", on), ["workflow_dispatch"])
        self.assertNotIn("cron", on)
        # No input names a ref: the run renders its own checkout (CodeQL: cache poisoning via execution of untrusted code)
        self.assertEqual(re.findall(r"(?m)^      ([a-z_]+):", on), ["force", "publish"])
        self.assertNotIn("inputs.ref", code("site-video.yml"))

    def test_the_jobs_are_decide_render_publish_pages_in_that_chain(self):
        self.assertEqual(self.jobs(), ["decide", "render", "stills", "publish", "pages"])
        self.assertIn("needs: decide", job_body("site-video.yml", "render"))
        self.assertIn("needs.decide.outputs.needs_render == 'true'", job_body("site-video.yml", "render"))
        publish = job_body("site-video.yml", "publish")
        self.assertIn("needs: [decide, render, stills]", publish)
        self.assertIn("needs.render.result == 'success' || needs.stills.result == 'success'", publish)
        # a failed or cancelled film stops the publish; a failed stills job (a scene over its noise bound) leaves the
        # film's refresh alone and the run red
        self.assertIn("needs.render.result != 'failure'", publish)
        self.assertIn("needs.render.result != 'cancelled'", publish)
        self.assertNotIn("needs.stills.result != 'failure'", publish)
        stills = job_body("site-video.yml", "stills")
        self.assertIn("needs: decide", stills)
        self.assertIn("needs.decide.outputs.needs_stills == 'true'", stills)

    def test_only_publish_holds_a_write_token_and_the_workflow_default_is_read(self):
        body = code("site-video.yml")
        self.assertRegex(body, r"(?m)^permissions:\n  contents: read\n")
        self.assertEqual(body.count("contents: write"), 1)
        self.assertIn("contents: write", job_body("site-video.yml", "publish"))
        for job in ("decide", "render", "stills"):
            self.assertNotIn("write", job_body("site-video.yml", job).replace("--write", ""))
        # nothing the workflow checks out for rendering keeps the token on disk
        self.assertIn("persist-credentials: false", job_body("site-video.yml", "render"))
        self.assertIn("persist-credentials: false", job_body("site-video.yml", "stills"))
        self.assertIn("persist-credentials: false", job_body("site-video.yml", "decide"))
        # a dry run holds no credential either: only the checkout that a publish uses keeps it
        publish = job_body("site-video.yml", "publish")
        self.assertEqual(publish.count("persist-credentials: true"), 1)
        self.assertEqual(publish.count("persist-credentials: false"), 1)
        # the one secret is the deploy key main's ruleset lets push, and only a publish's checkout of main holds it
        self.assertEqual(re.findall(r"secrets\.\w+", body), ["secrets.MAIN_PUSH_KEY"])
        with_key = [c for c in self.checkouts("publish") if "secrets.MAIN_PUSH_KEY" in c]
        self.assertEqual(len(with_key), 1)
        self.assertRegex(with_key[0], r"(?m)^\s+ref: main\s*$")
        self.assertIn("env.PUBLISH == 'true'", with_key[0])

    def checkouts(self, job):
        """[(step text)] for each `actions/checkout` step of a job."""
        body = job_body("site-video.yml", job)
        steps = re.split(r"(?m)^      - ", body)
        return [st for st in steps if "actions/checkout@" in st]

    def test_nothing_renders_a_ref_a_job_computed_and_the_write_token_never_runs_the_rendered_code(self):
        """The CodeQL finding: a checkout whose `ref:` comes from an input or a job output, followed by cache steps that
        execute what they restored, is cache poisoning from a dispatched ref into main's caches."""
        for job in ("decide", "render", "stills"):
            for step in self.checkouts(job):
                self.assertNotRegex(step, r"(?m)^\s+ref:", f"{job}'s checkout names a ref")
        render = job_body("site-video.yml", "render")
        self.assertEqual(len(self.checkouts("render")), 1)
        self.assertNotIn("needs.decide.outputs.sha", "\n".join(self.checkouts("render")))
        # the publish job checks out MAIN literally (the trusted code that lands the commit) for a publish; its other
        # checkout is the dispatched ref's own, for a dry run that holds no credential. Neither names an expression.
        publish_checkouts = self.checkouts("publish")
        self.assertEqual(len(publish_checkouts), 2)
        with_main = [c for c in publish_checkouts if re.search(r"(?m)^\s+ref: main\s*$", c)]
        self.assertEqual(len(with_main), 1)
        self.assertIn("env.PUBLISH == 'true'", with_main[0])
        self.assertIn("persist-credentials: true", with_main[0])
        other = [c for c in publish_checkouts if c not in with_main][0]
        self.assertIn("env.PUBLISH != 'true'", other)
        self.assertNotRegex(other, r"(?m)^\s+ref:")
        self.assertNotIn("${{", "".join(re.findall(r"(?m)^\s+ref:.*$", "\n".join(publish_checkouts))))
        # caches live in the render job, which holds no write token and no secret; the write job restores nothing
        publish = job_body("site-video.yml", "publish")
        for cached in ("actions/cache", "rust-cache", "setup-python", "cache:"):
            self.assertNotIn(cached, publish)
            self.assertNotIn(cached, job_body("site-video.yml", "decide"))
        self.assertIn("actions/cache@", render)
        # the artifact is named by the commit the render ran on
        self.assertIn("site-video-${{ github.sha }}", render)

    def test_publishing_is_refused_off_main_and_for_anything_but_a_stable_tag_on_main(self):
        decide = job_body("site-video.yml", "decide")
        self.assertIn("refs/heads/main", decide)
        self.assertIn("release-ref", decide)
        self.assertIn("github.ref_type", decide)
        self.assertIn("merge-base --is-ancestor", decide)
        self.assertIn("merge-base --is-ancestor", job_body("site-video.yml", "publish"))
        # any other ref renders as a dry run
        self.assertIn("neither main nor a stable release tag", decide)
        self.assertIn("--expect-rev", job_body("site-video.yml", "publish"))

    def test_publish_adopts_through_the_tool_and_rebases_before_it_pushes_never_forcing(self):
        publish = job_body("site-video.yml", "publish")
        self.assertIn("site_video.py adopt", publish)
        self.assertIn("--derive", publish)
        self.assertIn("git pull --rebase", publish)
        self.assertIn("git push origin HEAD:main", publish)
        self.assertNotRegex(publish, r"push[^\n]*(--force|-f\b|\+HEAD)")
        self.assertIn("github-actions[bot]", publish)
        self.assertIn("commit-message", publish)

    def test_the_stills_ride_the_same_run_and_the_same_commit_on_the_same_terms(self):
        """The simulator-made stills refresh with the film: every scene captured twice and held to its noise bound
        (never to byte identity: llvmpipe's blur is not bit-stable), adopted by MAIN's tool from an artifact (data),
        one allow-listed bot commit, no second workflow."""
        body = code("site-video.yml")
        stills, publish = job_body("site-video.yml", "stills"), job_body("site-video.yml", "publish")
        self.assertEqual(len(re.findall(r"(?m)^name:", body)), 1)
        self.assertEqual(stills.count("site_stills.py render"), 1)
        self.assertNotIn("site_stills.py compare", stills)  # no byte-identity gate
        self.assertIn("site_stills.py stability-table", stills)
        self.assertNotIn("continue-on-error", stills)
        self.assertIn("site_stills.py manifest", stills)
        self.assertIn("actions/upload-artifact@", stills)
        # the pinned ffmpeg is the only encoder the stills see
        self.assertIn("site_video.py ffmpeg-fetch", stills)
        self.assertIn('echo "$RUNNER_TEMP/pinned-bin" >> "$GITHUB_PATH"', stills)
        # publish: main's tool, the expected revision, the allow-list, one commit
        self.assertIn('site_stills.py adopt "$RUNNER_TEMP/stills" --write --expect-rev "$SHA"', publish)
        self.assertIn("allowed=", publish)
        self.assertNotIn("navblur", publish)
        # the allow-list admits every file the refresh may write (a scene returned to the automated set once
        # failed the dry run here: its image was written by `adopt` and refused by the list) and no photograph
        admitted = re.compile(re.search(r"(?m)^\s*stills='([^']+)'$", publish).group(1))
        sys.path.insert(0, str(ROOT / "tools"))
        import site_stills
        for path in site_stills.output_files():
            if path.startswith("docs/screenshots/") or path.startswith("site/media/"):
                self.assertTrue(admitted.search(path), f"{path} is written by the refresh but not allowed to change")
        self.assertFalse(admitted.search("docs/screenshots/navblur-transition.jpg"))
        self.assertEqual(publish.count("git commit -F"), 1)
        self.assertIn("--stills-manifest", publish)
        self.assertEqual(code("release.yml").count("gh workflow run site-video.yml"), 1)
        for name in ("rc.yml", "nightly.yml"):
            self.assertNotIn("site_stills", code(name))

    def publish_runs(self, **results):
        """Evaluate the publish job's `if:` for the given job results (decide defaults to success)."""
        publish = job_body("site-video.yml", "publish")
        cond = re.search(r"(?m)^    if: >-\n((?:      .*\n?)+)", publish).group(1)
        expr = " ".join(cond.split())
        results.setdefault("decide", "success")
        expr = re.sub(r"needs\.(\w+)\.result", lambda m: repr(results.get(m.group(1), "skipped")), expr)
        expr = expr.replace("always()", "True").replace("&&", " and ").replace("||", " or ").replace("!=", " != ")
        return eval(expr, {"__builtins__": {}})  # noqa: S307 - the workflow's own boolean expression, over string literals

    def test_a_failed_stills_job_does_not_stop_the_film_and_a_failed_film_stops_everything(self):
        self.assertTrue(self.publish_runs(render="success", stills="failure"))   # the film lands, the run stays red
        self.assertTrue(self.publish_runs(render="success", stills="success"))
        self.assertTrue(self.publish_runs(render="success", stills="skipped"))   # stills' inputs are the committed ones
        self.assertTrue(self.publish_runs(render="skipped", stills="success"))   # the film's inputs are the committed ones
        self.assertFalse(self.publish_runs(render="failure", stills="success"))  # a failed film blocks the stills too
        self.assertFalse(self.publish_runs(render="cancelled", stills="success"))
        self.assertFalse(self.publish_runs(render="success", stills="cancelled"))
        self.assertFalse(self.publish_runs(render="skipped", stills="skipped"))  # nothing rendered, nothing to publish
        self.assertFalse(self.publish_runs(render="skipped", stills="failure"))  # nothing to publish either
        self.assertFalse(self.publish_runs(decide="failure", render="success", stills="success"))

    def test_each_adopt_step_runs_only_for_the_artifact_that_exists(self):
        publish = job_body("site-video.yml", "publish")
        for name, job in (("Adopt the film", "render"), ("Adopt the stills", "stills"), ("Download the film (data)", "render"),
                          ("Download the stills (data)", "stills")):
            step = publish[publish.index(f"- name: {name}"):]
            step = step[:step.index("\n      - ", 1)] if "\n      - " in step[1:] else step
            self.assertIn(f"needs.{job}.result == 'success'", step, name)

    def test_the_deploy_is_a_dispatch_of_pages_on_main_because_a_tag_run_may_not_deploy_pages(self):
        """The `github-pages` environment admits deployments from `main` only (checked with the API), so a run on a
        release tag cannot call pages.yml; it dispatches it on main, where the push's own commit is the tip."""
        pages = job_body("site-video.yml", "pages")
        self.assertIn("needs.publish.outputs.committed == 'true'", pages)
        self.assertIn('gh workflow run pages.yml --repo "$GITHUB_REPOSITORY" --ref main', pages)
        self.assertIn("actions: write", pages)
        self.assertNotIn("uses: ./.github/workflows/pages.yml", pages)
        self.assertNotIn("pages: write", pages)
        self.assertRegex(code("pages.yml"), r"(?m)^  workflow_dispatch:\n")
        # pages.yml gained nothing for this: it deploys main's tip, which holds the bot's commit
        self.assertNotIn("inputs.ref", code("pages.yml"))

    def test_every_action_is_pinned_to_a_commit(self):
        for line in code("site-video.yml").splitlines():
            m = re.search(r"uses:\s*(\S+)", line)
            if m and not m.group(1).startswith("./"):
                self.assertRegex(m.group(1), r"@[0-9a-f]{40}$", line)

    def test_a_published_stable_release_dispatches_it_and_nothing_else_does(self):
        hook = job_body("release.yml", "site-video")
        self.assertIn("continue-on-error: true", hook)
        self.assertIn("needs: [guard, publish]", hook)
        self.assertIn("needs.publish.result == 'success'", hook)
        self.assertIn("needs.guard.outputs.line == 'main'", hook)
        self.assertIn("release-ref", hook)
        # dispatched ON THE TAG: the run renders its own checkout, and its caches are the tag's, never main's
        self.assertIn('gh workflow run site-video.yml --repo "$GITHUB_REPOSITORY" --ref "$TAG" -f publish=true', hook)
        self.assertNotIn("--ref main", hook)
        self.assertIn("actions: write", hook)
        self.assertNotIn("contents: write", hook)
        # the release's own jobs gained no new permission, and the new one exists once
        self.assertEqual(code("release.yml").count("actions: write"), 1)
        self.assertEqual(code("site-video.yml").count("actions: write"), 1)
        # candidates and nightlies never refresh the film
        for name in ("rc.yml", "nightly.yml"):
            self.assertNotIn("site-video", code(name))
        # `publish` is the job that creates the release; the hook must come after it, never gate it
        self.assertNotIn("site-video", job_body("release.yml", "publish"))
        self.assertNotIn("site-video", job_body("release.yml", "build"))


class ReleaseMainPushKey(unittest.TestCase):
    """main's required-checks ruleset rejects the workflow token, so the two jobs that push a line check out with the
    MAIN_PUSH_KEY deploy key; the release TAG must still go over the token, or it fires `push: tags` a second time."""

    def test_the_pushing_checkouts_carry_the_deploy_key(self):
        for job in ("prepare", "publish"):
            self.assertIn("ssh-key: ${{ secrets.MAIN_PUSH_KEY }}", job_body("release.yml", job))
        self.assertEqual(code("release.yml").count("secrets.MAIN_PUSH_KEY"), 2)

    def test_the_tag_is_pushed_with_the_token_not_the_key(self):
        prepare = job_body("release.yml", "prepare")
        pushes = re.findall(r"(?m)^\s+git push .*$", prepare)
        self.assertEqual(len(pushes), 2, pushes)
        tag_push = [p for p in pushes if "$tag" in p]
        self.assertEqual(len(tag_push), 1, pushes)
        self.assertIn("x-access-token:${GH_TOKEN}@", tag_push[0])
        self.assertNotIn("origin", tag_push[0])
        self.assertIn("GH_TOKEN: ${{ github.token }}", prepare)
        self.assertIn("tags: ['v*']", code("release.yml"))


class PerTestResults(unittest.TestCase):
    """The flaky-test pipeline (tools/ci-test-results.py -> artifact -> tools/ci-history.py -> tools/ci-summary.py).
    Every link of it is silent when it breaks: a job that stops uploading, an artifact name the collector no
    longer matches, or a summary that stops reading the quarantine list just leaves the page empty."""

    JOBS = {"host-unit-default": "host unit tests (default features)", "host-unit-hostsim": "host unit tests (hostsim)"}

    def test_both_host_unit_jobs_record_and_upload_their_results_under_a_name_the_collector_reads(self):
        history = importlib.util.spec_from_file_location("ci_history", ROOT / "tools/ci-history.py")
        module = importlib.util.module_from_spec(history)
        history.loader.exec_module(module)
        for job, name in self.JOBS.items():
            body = job_body("ci.yml", job)
            self.assertIn(f"name: {name}", body, "the job renamed: the page's job label must follow")
            self.assertIn(f'--job "{name}"', body)
            self.assertIn('--log "$RUNNER_TEMP/make-check.log"', body)
            m = re.search(r"name: (test-results-[a-z-]+-)\$\{\{ github\.run_attempt \}\}", body)
            self.assertTrue(m, f"{job} must upload test-results-<slug>-<run_attempt>")
            self.assertRegex(m.group(1) + "1", module.RESULTS_ARTIFACT.pattern)
            # a red job is the one whose results matter, and a job that died before `make` has no log to read
            step = body[body.index("Record the per-test results"):]
            self.assertIn("if: always()", step)
            self.assertIn("if-no-files-found: ignore", body)

    def test_the_collector_reads_the_quarantine_list_from_main_in_both_writers_of_the_summary(self):
        metrics = code("ci-metrics.yml")
        self.assertIn("ci/flaky.txt", metrics)
        self.assertIn("--quarantine ci/flaky.txt", metrics)
        self.assertIn("ci/flaky.txt", code("build-bench.yml"))
        # artifacts are listed through the same token that lists the jobs: no new permission
        self.assertRegex(metrics, r"(?m)^      actions: read ")
        self.assertNotIn("actions: write", metrics)


if __name__ == "__main__":
    unittest.main()
