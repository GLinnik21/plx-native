# Author a Player HUD 1.0 bundle

Start from `examples/minimal/` and the manifest/HUD schemas. Packages are data-only directories
compiled by the native SDK into `.plxhud` archives. Package format and HUD format both start at
1.0; an initial plugin version is 1.0.0.

Keep identity and resource paths in manifest.json and layout/palette in hud.json. Put SVG masks
in ordinary resource files. Do not inline assets, base64-encode binaries, invent commands,
translate app-owned labels, or implement focus/navigation logic. Use app theme tokens and widgets.

Run the native `--hud-pack DIR --out FILE --format json`, then lint that exact archive with
`--hud-lint FILE --lang all --format json`. Address every finding, using its `file` and JSON
`path`. Render and inspect the native state matrix before submitting the source directory.

The canonical packer preserves source bytes and condition order. Do not sort or normalize HUD
condition objects. README files are not archive resources; legal text belongs in declared notices.
No executable code, external resource URLs, custom fonts, bitmaps or plugin-localized text in 1.0.
