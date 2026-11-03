# Player HUD bundle authoring

A Player HUD is a source directory built into one `.plxhud` archive by the native SDK. It changes
the player's declarative layout; playback, app labels, focus, menus and captions remain app-owned.

Copy `examples/minimal/`, give manifest.json a unique reverse-DNS ID, name, author and SPDX license,
and start its plugin version at `1.0.0`. Package format and `hud.format` are both `1.0`.
Use `schema/manifest/1.0.json` for metadata and resource declarations and `schema/hud/1.0.json` for
hud.json. Resource IDs refer to ordinary package-relative SVG files, never inline data or URLs.

Build and validate using the same immutable artifact:

```sh
plxnative-sim --hud-pack my-hud --out my-hud.plxhud --format json
plxnative-sim --hud-lint my-hud.plxhud --lang all --format json
plxnative-sim --hud-inspect my-hud.plxhud
python3 tools/hud_preview.py --bin bin/plxnative-sim --hud my-hud.plxhud --out previews
```

The published Linux SDK runs on Ubuntu 24.04 x86_64, including a VM or WSL. Its native preview
matrix needs Mesa, Xvfb, ffmpeg and Pillow. `xvfb-run -a` provides the display for previews.
No Rust build or real Plex account is needed. Native previews use the licensed synthetic library.

Findings name the archive member in `file` and a document-local JSON pointer in `path`; SVG and
archive failures use an empty pointer. Fix all findings and inspect every generated state.
Do not shorten app translations or remove commands to hide a fit error. The registry applies
measured per-rung TV headroom; the television still compiles with its own native text widths.

Keep theme tokens, size rungs, commands and shared native menus. Preserve condition-object order.
The native packer writes canonical STORE ZIP32 entries and preserves the payload bytes exactly.
Only the manifest, HUD, declared SVG resources and UTF-8 legal notices enter the archive. README
files stay in source control. Standard ZIP tools can open an artifact for editing; rebuild it with
the native packer before distribution.

Submit the source folder, attribution README and maintainer contact to
https://github.com/GLinnik21/plxnative-huds . CI builds once and binds linting, all-language/native
previews and publication to the same archive hash. The initial format has no compatibility reader
for standalone JSON, scripts, fonts, bitmap artwork or plugin-localized labels.
