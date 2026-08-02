# PlxNative

An unofficial native [Plex](https://www.plex.tv/) client for **LG webOS 4.x** televisions.

Not affiliated with, endorsed by, or sponsored by Plex GmbH or LG Electronics.

It is a real application rather than a web app in a wrapper: an Apple-TV-style gallery UI drawn
directly with OpenGL ES 2, video decoded by the TV's own hardware pipeline onto the video plane,
and a media pipeline — HTTP, demux, buffer feed — that runs in-process. Almost all of it is Rust.

## What it does

- Sign in to your Plex account on the TV (on-screen QR code), pick a Plex Home profile, browse
  your libraries, and play.
- **Direct play wherever the TV can decode it**, including HEVC, 4K and 10-bit; server-side
  transcode only when it genuinely cannot.
- Hardware video on the TV's overlay plane, with the UI composited over it.
- Resume, seek and scrub, chapters, audio- and subtitle-track switching (including image subtitles),
  Skip Intro / Skip Credits, Up Next and auto-advance, and progress reported back to your server.

**Your library never leaves your network.** The app talks to your own Plex Media Server, to
`plex.tv` for sign-in, and to `discover.provider.plex.tv` for cast biographies. There is no
analytics, no telemetry and no crash reporting of any kind.

## Requirements

- An **LG TV on webOS 4.0–4.9**. This bound is real: the app binds the decoder to the video plane
  through LG's `libAcbAPI`, which does not exist from webOS 5.0 onward. See
  [`docs/distribution.md`](docs/distribution.md) §3 for what a newer-webOS port would take.
- A way to install unsigned apps — the [Homebrew Channel](https://github.com/webosbrew/webos-homebrew-channel),
  or LG Developer Mode.
- A Plex Media Server on your network, and a Plex account.

**Root is not required.** The app runs inside LG's stock jail as an ordinary unprivileged app
(device-verified: uid 6910, no capabilities). Root is only useful for the development loop below.

If you install through **Developer Mode** rather than the Homebrew Channel, note that LG expires a
Dev Mode session after 1000 hours and *uninstalls your apps* when it does. The Homebrew Channel has
no such expiry.

## Installing

Download the `.ipk` from the [latest release](https://github.com/GLinnik21/plex-native/releases)
and install it with the Homebrew Channel or
[dev-manager-desktop](https://github.com/webosbrew/dev-manager-desktop).

The release also publishes a `sha256`. It is worth checking: there is no code signing anywhere in
this distribution chain, so that hash is the whole integrity story. Builds are **reproducible** —
the same commit and toolchain produce a byte-identical package on any machine — so you can rebuild
from source and compare.

## Building from source

Cross-compiled from macOS or Linux to 32-bit ARM. From a clean clone:

```sh
make setup-env        # one-time: downloads and relocates the webOS NDK (~1 GB, into ~/webos-ndk)
rustup toolchain install nightly --component rust-src   # -Z build-std needs the std sources
make                  # builds pkg/plxnative
make ipk              # builds the installable pkg/com.beb.plxnative_<version>_arm.ipk
```

`make check` runs the host unit suite (~0.3 s, no TV) plus the lint gate. Run it before anything
else — it is the only signal that does not require a television.

### Developing against a real TV

The rest of the dev loop assumes a **rooted** TV reachable over ssh, because it deploys by copying
the binary straight into the installed app directory:

```sh
make TV=<ip> deploy   # scp the binary + assets
make TV=<ip> run      # launch, hold it up, then print the on-device event log
make TV=<ip> test     # deploy + run
./tests/run.py        # the on-device regression suite
./tests/run.py --fps  # the frame-rate regression scenes
```

The device is the real gate: nothing on the host draws a pixel, decodes a frame or talks to the
TV's media stack. `./tests/run.py` needs a `tests/manifest.local.json` describing *your* server and
a few items in *your* library — copy `tests/manifest.local.json.example` and fill it in.

For release artifacts, add `RELEASE=1` to **every** invocation that produces or ships a binary
(`make RELEASE=1 ipk`). It drops the developer feature set: the on-screen counter, and the whole
`/tmp` trigger surface the test harness drives the app through.

## Layout

| Path | What |
|---|---|
| `rust-modules/src/` | the application — UI, event loop, input, playback, the Plex data layer |
| `rust-modules/src/ui/` | the UI as a design system: theme tokens, components, screens |
| `rust-modules/src/player/` | the buffer-feed video engine and its worker threads |
| `src/` | the only C: a boot shim, the LG media-API seam, and the SVG rasterizer |
| `docs/` | verified PMS API reference, design notes, and the distribution analysis |
| `tests/` | the on-device regression suite |
| `ci/`, `.github/` | packaging, ELF/package assertions, release automation |

`CLAUDE.md` is the orientation document — architecture, the build, and the non-obvious conventions
that took a while to learn. Each major subsystem has its own alongside the code.

## Contributing

Issues and pull requests are welcome. Two things worth knowing before you start:

- **Run `make check` before you push**, with the nightly toolchain the Makefile uses — a bare
  `cargo test` picks up a different one, and they have disagreed.
- **Anything touching playback, the UI or input needs to be checked on a real TV.** There is no
  host runtime and no emulator that can run this (see `docs/` for why that was investigated and
  ruled out). Say in the PR what you verified and how.

## Licence

[MIT](LICENSE), © 2026 Gleb Linnik.

The PlxNative name and its logo/splash artwork are excluded — see the reservation in `LICENSE`.

Third-party components, their licences, and the notices that must travel with a built package are
in [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) and `licenses/`. Notably, the app links the
TV's own FFmpeg and GLib **dynamically**, under LGPL-2.1 §6(b); both files ship inside the `.ipk`
so the notice travels with the binary rather than only with this repository.

"Plex", "Rotten Tomatoes", "IMDb", "TMDB", "LG" and "webOS" are trademarks of their respective
owners. Where they appear in the application, they identify whose service or score is being shown.
