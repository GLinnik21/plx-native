# Player HUD bundle authoring

A Player HUD is a source directory built into one `.plxhud` archive by the native SDK. It changes
the player's declarative layout; playback, app labels, focus, menus and captions remain app-owned.

Copy `examples/minimal/`, give manifest.json a unique reverse-DNS ID, name, author and SPDX license,
and start its plugin version at `1.0.0`. Package format and `hud.format` are both `1.0`.
Use `schema/manifest/1.0.json` for metadata and resource declarations and `schema/hud/1.0.json` for
hud.json. To declare the optional playbar end-time lane, set `hud.format` to `"1.1"` and use
`schema/hud/1.1.json`. Resource IDs refer to ordinary package-relative SVG files, never inline data or URLs.

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

## Developing a HUD on the TV

`tools/hud-dev` (in the PlxNative repository; the SDK archive does not ship it) puts a source folder, or a `.plxhud`,
on your television while you edit it. Use a TV that runs a PlxNative build with the Player HUD page, on the same home
network as the machine you edit on. The tool needs a `plxnative-sim` (`--sim PATH`, else the one on `PATH`, else the
repository's `make sim-macos` build) and the repository's `tools/hud_package.py`, which it imports.

```sh
/usr/bin/python3 tools/hud-dev my-hud --sim bin/plxnative-sim
```

On a Mac run it with `/usr/bin/python3`, the interpreter the macOS firewall allows by default: any other Python
(Homebrew's included) has its incoming connections dropped without any message until someone clicks "allow incoming
connections" for that path. The tool refuses to start on a machine with no
private IPv4 address (10/8, 172.16/12, 192.168/16), the only kind the TV fetches over plain `http://`. It prints the
address to type, `http://<your address>:8731/my-hud.plxhud` (`--port` changes the port; `--host` names the address to
print when the machine has several), and the line below.

1. On the TV, open Settings > Video & playback > Player HUD > Install from address, type that address, press OK, and
   confirm the offer. The offer reads "From <your address>", "Development source" and "Unreviewed"; nothing is installed before you
   confirm.
2. Edit the source. Each time a file changes the tool lints it (`--hud-lint`, every language) and packs it with the
   same native packer `--hud-pack` uses. A source with findings is printed (file, pointer, hint) and is **not
   served**: the TV keeps getting the last clean build. A clean build is served at once.
3. On the TV, in the Player HUD page, focus your HUD's row and press **Play** (the remote's play key). The TV fetches
   the same address again and shows the offer for the new build; OK installs it. Select the HUD and start a video
   to see it.

Every clean build is served under a version above the one before, because the TV installs a fetched package only
when its `version` is higher than the installed one. The comparator reads `MAJOR.MINOR.PATCH` as three plain numbers
(no pre-release part), so the tool raises the patch of the copy it serves by the seconds since 2026-01-01: it is
higher after every change and after a restart of the tool, and never below your own version. Your files, including
`manifest.json`, are never touched. Play on a row does something only for a package installed from a development
address; a registry or drop-folder install has no such action.

**Remove the dev install before installing the registry one.** A dev build's patch number (millions, growing by one a
second) is above any release that only raises the patch: a dev build installed as `1.0.26000000` makes the published
`1.0.4` "not newer", so the TV refuses it and does not treat it as an update. Before you install the published
version, open the Player HUD page and use the row named after your HUD whose detail reads "Remove" (the page's existing
way to remove a package), then install from the gallery. The TV also records the dev address in its installed list. A
PlxNative build older than this feature reads that whole list as read-only: it does not list the dev HUD and cannot
remove it, so remove the dev install on a newer build before you go back to an older one.

Submit the source folder, attribution README and maintainer contact to
https://github.com/GLinnik21/plxnative-huds . CI builds once and binds linting, all-language/native
previews and publication to the same archive hash. The initial format has no compatibility reader
for standalone JSON, scripts, fonts, bitmap artwork or plugin-localized labels.
