# PlxNative

An unofficial native [Plex](https://www.plex.tv/) client for LG webOS 4.x televisions.

*Working and in daily use on a webOS 4.5 set. Not yet packaged as a release — see
[Installing](#installing).*

## Why this exists

The official Plex app on my old LG is slow. Scrolling a shelf stutters, opening a poster takes a
beat too long. It behaves like a web page, and when I went looking, that turned out to be exactly
what it is: a web app running in the TV's Chromium.

Writing a whole client was not my first idea. I tried the obvious things first, and this project is
what was left after both failed:

- **Patching and optimising the web app.** You can get at it and change it, and I did. The ceiling
  isn't in the code, though — it's the browser itself on hardware this old.
- **Kodi with a Plex plugin.** There isn't enough free memory on this TV to run it. Most of the RAM
  is already spoken for by webOS.

What kept nagging at me was the Apple TV app: similar hardware, and its interface simply moves. The
answer turned out to be unglamorous — it's a native app. No browser, no JavaScript, no web view. It
just draws.

So I wrote one. PlxNative is an Apple-TV-inspired Plex client that draws its interface directly with
OpenGL ES 2 and hands video to the TV's own hardware decoder. Almost all of it is Rust.

I use it myself, every day, to watch things off my server in the next room.

## What it looks like

Real screenshots off the television, not mockups.

![Home](docs/screenshots/home.jpg)

**Home** — a hero for whatever you're partway through, shelves underneath.

![Detail](docs/screenshots/detail.jpg)

**Detail** — ratings, cast, and where to carry on.

![Library](docs/screenshots/library.jpg)

**Library** — sort, filter, unwatched-only, and an A–Z rail down the side.

![Player](docs/screenshots/player.jpg)

**Player** — the transport over hardware-decoded 4K. This one is the whole point of the project:
the picture is on the TV's video plane, decoded by the same silicon the built-in apps use, with the
interface drawn on top of it. That's the thing a browser-based client can't do.

## What I'd want you to know before installing

I built this for how *I* watch, so the honest scope is narrower than Plex's:

- **Movies and TV shows.** That's what's in my library, so that's what's here. No music, no photos,
  no live TV or DVR.
- **A local server.** Mine is on the same network as the TV, and that's the only setup I've
  actually tested. Remote servers and relay connections may well work — I don't know, because I've
  never tried.
- **webOS 4.x, not 5 or newer.** Partly because a 4.5 set is the TV I own and can test on. But it's
  also a real technical wall: the app puts decoded video on the TV's hardware plane through LG's
  `libAcbAPI`, and that library is gone from webOS 5.0 onward. Supporting newer sets means finding
  what replaced it and testing on real hardware, not just relaxing a version check.

  **If you have a rooted webOS 5+ TV, I'd love the help.** That's the one thing I can't do from
  here — no emulator can stand in for it (I looked into this properly; see
  [`docs/distribution.md`](docs/distribution.md) §3). Even just telling me what's in
  `/usr/lib` on your set, and whether `libAcbAPI` has a successor there, would move this forward
  more than anything else on the list.
- **One person's spare time.** There will be bugs I haven't hit because I don't watch the way you do.

If that fits, it's genuinely nice to use. If it doesn't, the official app will serve you better.

## What it does

- Sign in on the TV with an on-screen QR code, pick a Plex Home profile, browse, and play.
- **Direct play wherever the TV can decode it** — HEVC, 4K, 10-bit — and ask the server to
  transcode only when it truly can't.
- Hardware video on the TV's overlay plane, with the UI drawn over the top.
- Resume, seek and scrub, chapters, audio and subtitle track switching (including image subtitles),
  Skip Intro / Skip Credits, Up Next with auto-advance, and progress reported back to your server.

**Your library never leaves your network.** The app talks to your own Plex Media Server, to
`plex.tv` to sign in, and to `discover.provider.plex.tv` for cast biographies. No analytics, no
telemetry, no crash reporting. I didn't leave it out to be virtuous — I just never wanted any.

## What you need

- An **LG TV on webOS 4.0–4.9** (see above for why).
- A way to install unsigned apps: the
  [Homebrew Channel](https://github.com/webosbrew/webos-homebrew-channel) — which installs any
  `.ipk` you point it at, listed in its catalogue or not — or LG Developer Mode.
- A Plex Media Server on your network, and a Plex account.

**You don't need a rooted TV.** The app runs inside LG's normal sandbox as an ordinary unprivileged
app — not as root, with no special permissions. I verified that on the device rather than assuming
it; the evidence is in [`docs/distribution.md`](docs/distribution.md) §3.5. Root only matters for
the development loop below.

If you go the **Developer Mode** route rather than the Homebrew Channel, know that LG expires a Dev
Mode session after 1000 hours and *uninstalls your apps* when it does. The Homebrew Channel doesn't.

## Installing

> **There's no published build yet, and it isn't in the Homebrew Channel's app list.** If you want
> to try it today, [build it yourself](#building-it-yourself) — it's two commands once the NDK is
> in place. The rest of this section is how installing will work; the
> [releases page](https://github.com/GLinnik21/plx-native/releases) is the source of truth, and
> this note goes away when there's something on it.

Grab the `.ipk` from the [latest release](https://github.com/GLinnik21/plx-native/releases) and
install it with the Homebrew Channel or
[dev-manager-desktop](https://github.com/webosbrew/dev-manager-desktop).

Each release also publishes a `sha256`, and it's worth checking. Nothing in this distribution chain
is code-signed, so that hash is the only thing standing between you and a tampered package. Builds
are reproducible — the same commit and toolchain produce a byte-identical `.ipk` on any machine — so
you can also just build it yourself and compare.

## Building it yourself

Cross-compiled from macOS or Linux to 32-bit ARM. From a clean clone:

```sh
make setup-env        # one-time: downloads and relocates the webOS NDK (~1 GB, into ~/webos-ndk)
rustup toolchain install nightly --component rust-src   # -Z build-std needs the std sources
make                  # builds pkg/plxnative
make ipk              # builds the installable pkg/com.beb.plxnative_<version>_arm.ipk
```

`make check` runs the host unit suite (~0.3 s, no TV) plus the lint gate. Run it first — it's the
only signal you get without waking a television.

### Developing against a real TV

The rest of the loop assumes a **rooted** TV reachable over ssh, because it deploys by copying the
binary straight into the installed app directory:

```sh
make TV=<ip> deploy   # scp the binary + assets
make TV=<ip> run      # launch, hold it up, then print the on-device event log
make TV=<ip> test     # deploy + run
./tests/run.py        # the on-device regression suite
./tests/run.py --fps  # the frame-rate regression scenes
```

The device is the real test. Nothing on your computer draws a pixel, decodes a frame, or talks to
the TV's media stack, so a green host suite proves much less than it looks like it does.
`./tests/run.py` needs a `tests/manifest.local.json` pointing at *your* server and a few items in
*your* library — copy `tests/manifest.local.json.example` and fill it in.

For anything you intend to ship, add `RELEASE=1` to **every** command that builds or packages
(`make RELEASE=1 ipk`). It drops the developer feature set: the on-screen frame counter, and the
whole `/tmp` trigger surface the test harness drives the app through.

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

`CLAUDE.md` is the orientation document — architecture, the build, and all the non-obvious things
that took a while to work out. Each major subsystem has one of its own next to the code.

## Contributing

Issues and pull requests are welcome, especially from anyone with a TV or a library that differs
from mine — a **rooted webOS 5+ set** most of all (see above), but also a different panel, a remote
server, or media this has never met. Two things worth knowing first:

- **Run `make check` before you push**, using the nightly toolchain the Makefile pins. A bare
  `cargo test` picks up a different one, and the two have disagreed.
- **Anything touching playback, the UI or input has to be checked on a real TV.** There's no host
  runtime, and no emulator can run this — that was investigated properly and ruled out, see `docs/`.
  Say in the PR what you verified and how.

## Licence

[MIT](LICENSE), © 2026 Gleb Linnik.

The PlxNative name and its logo/splash artwork are excluded — see [`TRADEMARKS.md`](TRADEMARKS.md),
which also carries the Plex and LG non-affiliation statements.

Third-party components and their licences are in
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) and `licenses/`. Notably the app links the TV's
own FFmpeg and GLib **dynamically**, under LGPL-2.1 §6(b); both files ship inside the `.ipk`, so the
notice travels with the binary and not only with this repository.

This is an unofficial client. It is not affiliated with, endorsed by, or sponsored by Plex GmbH or
LG Electronics. "Plex", "Rotten Tomatoes", "IMDb", "TMDB", "LG" and "webOS" are trademarks of their
respective owners; where they appear in the app, they identify whose service or score is being shown.
