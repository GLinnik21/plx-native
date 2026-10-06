#!/usr/bin/env python3
"""Pins the shape of three CI changes whose failure is silent (everything stays green).

* `cache-cleanup.yml` runs on `pull_request_target`, which is safe only while it never checks out
  or runs pull request code and holds nothing but `actions: write`;
* the nightly's Sentry debug-file upload is skipped on a pull request's dry run and nowhere else
  (a release, the schedule and a dispatched dry run keep it);
* the tests against the bundled FFmpeg and libass run in a job of their own, beside the replays,
  and still run;
* the nightly Homebrew Channel repository: the nightly build generates its manifest (and debug never
  does), a real nightly can only be cut from main, the manifest reaches the release, and the site
  stages the repository and the guide.

Text-level on purpose, like ci/test_ci_timeouts.py (no YAML library on a stock runner).
"""
from __future__ import annotations

import re
import unittest
from pathlib import Path

WORKFLOWS = Path(__file__).resolve().parent.parent / ".github/workflows"


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

    def test_the_source_bundle_is_named_for_the_reported_version_not_the_package_filename(self):
        # The package version carries the cut date as its patch, so `<filename version>-nightly-<date>`
        # names a version nothing reports; the label comes from check-package's own derivation.
        build = code("build-package.yml")
        self.assertIn("--print-nightly-label pkg/.build-config", build)
        self.assertIn('label="${nightly_label:-$version}"', build)
        self.assertNotIn('label="$version${nightly_date:+-nightly-$nightly_date}"', build)

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


if __name__ == "__main__":
    unittest.main()
