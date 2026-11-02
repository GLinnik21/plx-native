# HUD registry tooling

The registry lives in `GLinnik21/plxnative-huds`. It uses the app's compiler, schema and real
text renderer through an immutable Linux authoring bundle. PR12's acceptance record belongs in
the registry's `IMPLEMENTATION.md`; this document describes the app-side interfaces.

`hud-simulator.yml` builds an optimized `plxnative-sim` with the existing `site-video-sim`
target and the repository's host FFmpeg. `tools/hud_bundle.py` packages explicit tracked fonts,
licenses, native dependencies, schema, fixture corpus, synthetic mock and capture tools. The
archive and its manifest have SHA-256 pins, the full source commit and the reported app version.
An unreleased build reports `-dev` and is explicitly marked development. A manual CI run with
`publish_development=true` publishes a prerelease utility under `hud-simulator-<full source SHA>`
for registry testing; it does not create a `v*` app release tag. A normal app release
attaches its own simulator once; uploads do not overwrite existing assets. The Linux bundle
needs Ubuntu 24.04-compatible glibc, the system Mesa driver, Xvfb and ffmpeg/Pillow for previews.
It relocates independently of the source/build checkout. No real Plex configuration is included.

Headless commands are `--hud-info`, `--hud-text-metrics`, `--hud-inspect FILE`, `--hud-schema`,
`--hud-export builtin:classic`, and `--hud-lint FILE --format json --lang all`. Inspection expands
the compiler's collapsed states through its actual lookup into content, transport and section
valuations, with native control and scrub hit rectangles. Preview tools click those hit areas
through the shared frame writer's input queue. `tools/screenshots.py` owns the mock, runtime,
process, refusal checks and PNG output for both normal stills and virtual-clock selections.

`--hud-lint ... --text-skew PROFILE` applies explicitly supplied registry headroom at each
supported type rung. The default and shipping TV fit pass keep zero margin. `--hud-text-metrics`
measures the lint's exact app-text candidates in every language/rung/weight with real SDL_ttf,
including chapter fallbacks, episode prefixes and clock-width envelopes. Font hashes include
all installed fallback faces. On the TV, the `hud-text-metrics` dev trigger writes the same
report after text initialization into its runtime root. Capture that under the TV lock using
a synthetic guest session; compare it with the pinned Linux report in the registry's calibration
tool. Neither report is a substitute for the other renderer's actual measurements.

After a successful stable app release, `release.yml` dispatches the simulator workflow on the
released tag. Once attached, it sends `app-release` repository_dispatch to the registry. This
cross-repository API needs `HUD_REGISTRY_DISPATCH_TOKEN`, restricted to the registry repository
with the permission needed for repository dispatch. A personal developer login is not embedded
in the artifact or copied into Actions. The registry also checks weekly for missed releases.

After configuring the scoped secret, a maintainer can dispatch `hud-simulator.yml` with
`dispatch_probe=true`. This skips the simulator build and calls the same narrowly addressed
registry API as stable notification, using event type `app-release-probe`. The registry runs
its ordinary release-discovery/re-lint backstop without accepting an invented app-release pin.
The receiving workflow must already be on the registry's default branch. This verifies delivery
and credential scope without publishing or installing a TV app release.

The initial native measurement contains 12,276 samples across all eleven shipped languages,
nine type rungs and both weights. Its registry-only margins range from 4 to 9 pixels. Reports
retain exact font hashes and host/TV source identities; the registry stores the evidence and
tracks when a margin is carried across translation changes with the same native text stack.
