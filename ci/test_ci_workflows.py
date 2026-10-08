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
* ci.yml's `paths` filter (`CiPathFilter`): evaluated with GitHub's rules, a docs-only or
  repository-metadata-only change (issue forms, a PR template, prose, images) starts no run, while
  every file a gate reads (PRIVACY.md, the credits pair, an `include_str!` target, the workflows)
  still does, and no other workflow carries a catch-all `'**'` filter;
* the nightly Homebrew Channel repository: the nightly build generates its manifest (and debug never
  does), a real nightly can only be cut from main, the manifest reaches the release, and the site
  stages the repository and the guide.

Text-level on purpose, like ci/test_ci_timeouts.py (no YAML library on a stock runner).
"""
from __future__ import annotations

import os
import re
import subprocess
import sys
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
        self.assertRegex(publish, r"python3 \.\./tools/ci-summary\.py --history ci-history\.json --bench build-history\.json --out ci-summary\.json")

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
            if m:
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
        lines = text("ci.yml").splitlines()
        start = next(i for i, l in enumerate(lines) if l.strip() == "paths: &ci_paths")
        patterns = []
        for line in lines[start + 1:]:
            stripped = line.strip()
            if stripped.startswith("#") or not stripped:
                continue
            if not stripped.startswith("- "):
                break
            patterns.append(stripped[2:].strip().strip("'\""))
        return patterns

    @staticmethod
    def regex(pattern):
        out, i = [], 0
        while i < len(pattern):
            if pattern.startswith("**/", i):
                out.append("(?:.*/)?")
                i += 3
            elif pattern.startswith("**", i):
                out.append(".*")
                i += 2
            elif pattern[i] == "*":
                out.append("[^/]*")
                i += 1
            elif pattern[i] == "?":
                out.append("[^/]")
                i += 1
            else:
                out.append(re.escape(pattern[i]))
                i += 1
        return re.compile("".join(out) + r"\Z")

    @classmethod
    def triggers(cls, path, patterns=None):
        included = False
        for pattern in patterns if patterns is not None else cls.anchor():
            negative = pattern.startswith("!")
            if cls.regex(pattern[1:] if negative else pattern).match(path):
                included = not negative
        return included

    def test_pull_request_uses_the_same_list_as_push(self):
        self.assertRegex(text("ci.yml"), r"(?m)^  pull_request:\n    paths: \*ci_paths$")
        self.assertEqual(len(re.findall(r"(?m)^\s+paths: &ci_paths$", text("ci.yml"))), 1)

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
        self.assertNotIn("secrets.", body)

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
        self.assertIn("persist-credentials: false", other)
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


if __name__ == "__main__":
    unittest.main()
