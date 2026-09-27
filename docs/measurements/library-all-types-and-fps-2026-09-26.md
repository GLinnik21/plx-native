# Library All types, compact rows, and scrolling — 2026-09-26

This records the initial implementation on base `a64e1e34`, preserved in commit
`105c9ea1` before integration. Its performance numbers and test counts are evidence
for that implementation. The subsequent forward-port onto `aa1fd12a` preserves the
current backdrop discovery, artwork cropping, owned store gates and text metrics;
the old measurements do not establish performance of that newer renderer.

TV Shows → All now offers TV Shows, Seasons, and Episodes. Seasons retain their own
posters and show captions; episodes use four columns of landscape stills with the
shared episode labels, watched state, and resume treatment. Unfocused All rows use
the shared collapsed caption band. The focused row opens with the shared spring,
and paging, hit testing, scrolling, and restored viewports use the same geometry.

## Native measurement

The scene is a 525-episode library on the dev television, `FLAVOR=debug`, at the
native 1920×1080 viewport. A host-timed FIFO driver alternates eight Down and eight
Up presses, 250 ms apart, between rows 3 and 11. Each sample starts after six seconds
of motion. Screenshots are taken before that warm-up, outside the measured window.
All render-profiler, draw-mask, capture-recording, and focus-trace triggers are
cleared for production pacing. The rates below are app-present heartbeat samples,
not per-frame latency percentiles.

| Configuration | Panel | Median FPS | Range | Samples |
|---|---|---:|---:|---:|
| Original episode grid, before performance fixes | On | 48 | 47–49 | 25 |
| Compact rows, optimized image geometry, separate gradient draw | Off | 57 | 53–59 | 24 |
| Compact rows, optimized image geometry, fused gradient | Off | 58 | 55–60 | 24 |
| Fused gradient, longer confirmation | Off | 57 | 53–60 | 60 |
| Final: also skip offscreen document controls | Off | 58 | 55–61 | 60 |

The initial baseline predates the request to keep the panel off and uses the older,
more widely spaced rows. It is context, not a controlled A/B comparison. The two
24-sample comparisons use identical compact geometry and screen-off conditions.
The fused path was retained after this small measured advantage. The final run
also skips offscreen library pills and All toolbar preparation; its mean was
57.75 FPS and its settled idle sample was 0–1 FPS. Presentation still sleeps once
the row springs settle. The result is close to 60 FPS, not a claim of a locked
60 FPS on every frame.

Local evidence bundles:

- `/tmp/plxnative-episode-fps-motion-valid`
- `/tmp/plxnative-grid-two-pass-collapsed`
- `/tmp/plxnative-grid-packed-collapsed`
- `/tmp/plxnative-grid-final-confirmation`
- `/tmp/plxnative-grid-controls-cull` (final, including `pacing.json`)

## Changes responsible for the improvement

- Data-only effects no longer capture navigation bookmarks. Navigation effects
  retain their emission-time return state, including when mixed with store work.
- Unchanged or stale hub publications no longer invalidate the library each frame.
- Native text widths and fitted episode labels are bounded caches. Recording and
  replay still observe their own measurement requests, and failed metrics are not
  retained. Cropped text skips unnecessary GL state changes.
- Invisible cards skip composition while their buffered artwork still warms.
  Offscreen library pills and All headings/toolbars skip string preparation,
  metrics, and painting. Conservative bounds retain partial controls, focus
  shadows, painter translations, and blur-source coverage; navigation and
  recorded stops are unchanged.
- A compact gradient shader replaces repeated rounded gradient bands. Loaded
  episode stills with opaque white paint combine that gradient with the image;
  fades, other tints, missing artwork, and optional-program failures keep the
  separate path. Premultiplied blending preserves the two-pass result and restores
  ordinary blending before the next primitive.
- Card geometry is folded on the CPU, and most interior pixels return before
  rounded-edge calculations. Fractional sizes, small radii, circles, shadows,
  transparency, and the ordinary image path retain their original semantics.

Simulator captures checked the type menu, season posters/captions, episode stills,
collapsed rows, toolbar visibility after scrolling back, and identical
focus/viewport restoration after Detail/Back. Native
captures checked the final card rendering with the panel off. ARM and macOS builds,
the shipping-feature check, 24 graphics tests, 21 text tests, and both GLES2 and
desktop-core shader validation passed. The ELF assertions and firmware symbol
inventory checks passed for webOS 4.4.2 through 11.2.0; inventory checks establish
loader compatibility, not UI verification on every firmware.

The final `make check` Rust suites passed **3,426 default-feature tests** and
**3,466 hostsim tests**, with one ignored test in each suite. The Library subset
passed all 126 tests. The complete make target still stops at the pre-existing
dependency failures described below.

Two test fixtures exposed timing assumptions under repeated verification: an
accepted HTTP socket inherited nonblocking mode, and a detail-refresh test used a
fixed number of yields instead of waiting for its workers to release their actual
request reservations. Both fixes are test-only and retain the original behavior
assertions, including admission and pending-state checks. The publication group
also passed 20 repetitions with fresh runtime directories.

The dependency gate's two pre-existing failures were reproduced on pristine
`a64e1e34`: one `libm` match in `ui/widgets_test_support.rs` and 30 thread-spawn
matches in test files outside the allowlist. The initial changed tree had the same two
failures and no new dependency-gate violations. The statics gate passes separately.
