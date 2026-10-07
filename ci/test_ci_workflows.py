#!/usr/bin/env python3
"""Pins the shape of three CI changes whose failure is silent (everything stays green).

* `cache-cleanup.yml` runs on `pull_request_target`, which is safe only while it never checks out
  or runs pull request code and holds nothing but `actions: write`;
* the nightly's and the release candidate's Sentry debug-file upload is skipped on a pull
  request's dry run and nowhere else (a release, the schedule and a dispatched dry run keep it);
* the tests against the bundled FFmpeg and libass run in a job of their own, beside the replays,
  and still run;
* the host FFmpeg and libass builds in the simulator jobs are CACHED where the build scripts really
  write, under a key that names every input the scripts key on (a cache that "hits" and still
  rebuilds, or one that outlives a changed input, both stay green);
* the nightly Homebrew Channel repository: the nightly build generates its manifest (and debug never
  does), a real nightly can only be cut from main, the manifest reaches the release, and the site
  stages the repository and the guide.

Text-level on purpose, like ci/test_ci_timeouts.py (no YAML library on a stock runner).
"""
from __future__ import annotations

import os
import re
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = ROOT / ".github/workflows"


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
        self.assertIn("debug-files check", step)
        self.assertLess(step.index("debug-files check"), step.index('"$UPLOAD" != true'))
        self.assertLess(step.index('"$UPLOAD" != true'), step.index("debug-files upload"))


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
    LIBASS_SOURCES = "vendor/libass-sources"

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
        self.assertIn(f"sources = ROOT / '{self.LIBASS_SOURCES}'", py)
        self.assertIn("stamp = prefix / '.dependencies-key'", py)
        for job in self.MACOS_JOBS:
            cache_of(job, self.FFMPEG_WORK)
        for job in self.LIBASS_JOBS:
            cache_of(job, self.LIBASS_PREFIX)
            cache_of(job, self.LIBASS_SOURCES)

    def test_the_ffmpeg_key_names_every_input_the_script_keys_on(self):
        sh = (ROOT / "ci/build-ffmpeg.sh").read_text()
        # The script hashes these into its own tree key; the workflow key must move with them.
        inherited = set(re.findall(r'"\$ROOT/(ci/[\w.-]+)"', re.search(r"INHERITED=.*", sh).group(0)))
        self.assertEqual(inherited, {"ci/arm-cc.py", "ci/check-link-evidence.py"})
        for job in self.MACOS_JOBS:
            key = cache_of(job, self.FFMPEG_WORK)["key"]
            self.assertEqual(hashed(key), {"ci/build-ffmpeg.sh", *inherited}, job)
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
        read = {"ci/build-libass.py", "ci/libass-dependencies.json",
                *re.findall(r"ROOT / '(ci/[\w.-]+)'\)\.read_bytes", py)}
        listed = set(makefile_list("LIBASS_INPUTS"))
        self.assertTrue(read <= listed, read - listed)
        for job in self.LIBASS_JOBS:
            key = cache_of(job, self.LIBASS_PREFIX)["key"]
            self.assertEqual(hashed(key), listed, job)
            self.assertIn("runner.os", key)
            self.assertIn("runner.arch", key)
            self.assertIn("steps.toolchain.outputs.id", key)
            # The tarballs are pinned by checksum, so their entry is shared by every OS and moves
            # only with the pins; a facade edit must not re-save 25 MB of sources.
            sources = cache_of(job, self.LIBASS_SOURCES)["key"]
            self.assertEqual(hashed(sources), {"ci/libass-dependencies.json"}, job)
            self.assertNotIn("runner.os", sources)
        self.assertEqual(len({cache_of(j, self.LIBASS_PREFIX)["key"] for j in self.LIBASS_JOBS}), 1)

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


if __name__ == "__main__":
    unittest.main()
