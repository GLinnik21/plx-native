# PlxNative

A fast, unofficial [Plex](https://www.plex.tv/) client for LG webOS televisions. Native, not a web
page — the interface is drawn straight on the GPU at 60 fps, and video plays on the TV's own decoder.

*In daily use on a 2019 LG set. Get started with the
[installation guide](docs/install-and-verify.md).*

## Why this exists

The official Plex app on my old LG is slow. Scrolling a shelf stutters, opening a poster takes a
beat too long. It behaves like a web page because it is one: a web app running in the television's
Chromium. Patching it doesn't help — the ceiling isn't the code, it's the browser.

So I threw the browser away.

PlxNative draws straight on the GPU and hands video to the same silicon the built-in apps use. No
Chromium, no JavaScript, no web view. It just draws. Almost all of it is Rust, and I use it every
day to watch things off my server in the next room.

## What it looks like

Real captures of the app, not mockups: the same code that runs on the television, on the desktop
simulator, browsing a demo library of openly licensed films
([credits](docs/screenshots/CREDITS.md)). `make screenshots` regenerates them.

![Home](docs/screenshots/home.jpg)

**Home** — a rotating hero from what you're partway through and what's just landed, shelves under it.

![Library](docs/screenshots/library.jpg)

**Library** — a library opens on its own shelves: what you're partway through, with the time
left under the focused card, and what just landed. Further down is the full grid, with sort,
filter (including unwatched-only) and an A–Z rail down the side
([pictured here](docs/screenshots/ux-library-grid.jpg)).

![Search](docs/screenshots/search.jpg)

**Search** — one query across every server you can reach, results grouped by kind.

![Player](docs/screenshots/player.jpg)

**Player** — the transport with chapters and track menus, drawn over video the television decodes
itself. (In this capture the frame under it was decoded by the simulator.)

## What it does

- **An interface that keeps up with the remote.** 60 fps on the 2019 set I develop on, with
  frame-rate regression scenes that measure it on the television rather than trusting it.
- **Sign in on the TV** with an on-screen QR code and pick a Plex Home profile.
- **Browse and search your libraries** — on your own servers and on ones shared with you. Your
  servers' libraries, that is, not Plex's catalogue or Watchlist.
- **Direct play** H.264 and HEVC — 4K, 10-bit, Dolby Vision profiles 5 and 8, E-AC-3 Atmos —
  decided against your television's own codec table where it publishes one. Anything else the
  server transcodes, and you can switch a playback to Auto to follow a link whose speed changes.
- **Everything you expect while watching**: resume, seek and scrub, chapters, audio and subtitle
  tracks (including image subtitles), Skip Intro, Skip Credits, Up Next with auto-advance, and
  progress reported back to your server.

## Will it work on my television?

**It needs webOS 4.0 or newer**; older firmware won't start — you'd get a tile that does nothing.
Past that, it most likely will. I develop and test on a 2019 set, and opt-in usage reports show
video playing on sets from 2018 through the newest, on webOS 11 — both direct play and server
transcodes, Developer Mode installs included.

### Known issues

| Where | What happens |
|---|---|
| **Some 2019 sets on LG's k5lp or k3lp chip**, installed through Developer Mode | Video won't play. On these sets LG's Developer Mode sandbox can withhold a device the video path needs — Kodi and Moonlight hit the same wall. The app checks for it and says so instead of crashing; other sets on the same chip play normally. On a rooted TV with Homebrew Channel, the failure screen offers **Repair**, which applies the Homebrew Channel fix to the sandbox (one owner fixed it this way from a root shell; the button itself hasn't been run on an affected set yet). Without root there is no fix. |
| **2018 sets on firmware that reports platform release 3.9.3** | Sign-in and browsing work, but video has never been seen to start on one, and no error is reported either. |
| **Sets on LG's k6hp chip** | A crash seen in opt-in crash reports and not reproduced here, because I have no such set: [#174](https://github.com/GLinnik21/plx-native/issues/174). |

### Rooted or not

**No root is needed.** A regular TV in Developer Mode runs everything except the k5lp/k3lp Repair
above. What Developer Mode costs you is renewal: if the session lapses, LG removes the apps
installed through it ([how to keep them](docs/install-and-verify.md#important-developer-mode-expires)).
A rooted TV with Homebrew Channel has no expiry.

If something goes wrong, [tell me what happened](https://github.com/GLinnik21/plx-native/issues) —
and if you own one of the sets above, it working is as useful a report as it failing.

## Installing

**First time installing an app outside the LG Content Store?** Follow the
[**step-by-step installation guide**](docs/install-and-verify.md). It starts with a regular LG
webOS TV, a computer on the same network, and a Plex account with access to your own or a shared
server. **No root is required.**

Set up LG Developer Mode and connect with webOS Dev Manager, then choose:

- **Install PlxNative directly:** add only PlxNative to the TV; install future `.ipk` updates from
  your computer.
- **Install Homebrew Channel first:** get an app catalogue on the TV, then install PlxNative and
  its updates with the remote.

**Already have Homebrew Channel?** Find [PlxNative in its catalogue](https://repo.webosbrew.org/apps/com.beb.plxnative/)
and select **Install**. Skip the computer setup.

**Developer Mode needs periodic renewal.** If it expires and LG disables Developer Mode, apps
installed through it are removed. Installing Homebrew Channel through Developer Mode does not
remove that requirement. The guide explains [how to renew the session](docs/install-and-verify.md#important-developer-mode-expires).

For manual `.ipk` downloads, [verify the release checksum](docs/install-and-verify.md#verifying-the-package)
before installing. Homebrew Channel verifies catalogue downloads for you.

## Nightly builds

Every day `main` moves, CI cuts a nightly `.ipk` from wherever it stands and publishes it as a
[prerelease](https://github.com/GLinnik21/plx-native/releases?q=nightly). It installs beside the
regular app as **"PlxNative Nightly"** — its own tile, its own sign-in — so trying it never touches
the release you already trust. The newest one is always linked from
[plxnative.com/nightly/latest.json](https://plxnative.com/nightly/latest.json).

**It is not tested on a television.** It passes the same automated build and packaging checks a
release does, but nobody has watched it play. It is not in the Homebrew Channel and does not
update itself — reinstall by hand whenever you want the next one — and each nightly is deleted
30 days after it is published, so link to a specific `.ipk` at your own risk.

## Privacy

**Your library data never reaches me.** The app talks to your Plex server, to `plex.tv` to sign in,
and to `discover.provider.plex.tv` for cast biographies.

**Crash reports and usage analytics are off until you turn them on** — two separate first-run
questions, each answerable with Don't Share, both reversible later under Account → Settings →
Privacy & data. No title, search term, subtitle line, server name or address can appear in any
report. [`PRIVACY.md`](PRIVACY.md) is the whole statement, including the schemas.

## The honest scope

I built this for how *I* watch, so it's narrower than Plex's:

- **Movies and TV shows.** No music, no photos, no live TV or DVR.
- **No typing in server addresses.** Servers come from your Plex account; set them up on a phone or
  PC and choose from what's there. Servers reached through Plex's relay, or that require an
  encrypted connection, are supported but haven't been watched end to end.
- **Styled ASS subtitles** (common on anime) show as plain text when a video plays directly
  ([#73](https://github.com/GLinnik21/plx-native/issues/73)). Pick a lower quality in the player's
  menu and the server draws them into the picture, styling included.
- **One person's spare time.** There will be bugs I haven't hit, because I don't watch the way you do.

If that fits, it's genuinely nice to use. If it doesn't, the official app will serve you better.

## Contributing

Issues and pull requests are welcome, especially from anyone whose television or library differs
from mine — [**docs/building.md**](docs/building.md) is the build and the test loop, and says what
hardware I most need help with. Security issues go through [`SECURITY.md`](SECURITY.md) rather than
a public issue.

## Acknowledgements

Error monitoring for PlxNative is sponsored by [Sentry](https://sentry.io/for/good/).

<a href="https://sentry.io/for/good/">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/sentry-wordmark-light.svg">
    <source media="(prefers-color-scheme: light)" srcset="docs/assets/sentry-wordmark-dark.svg">
    <img alt="Sentry" src="docs/assets/sentry-wordmark-dark.svg" width="160">
  </picture>
</a>

## Licence

[GPL-3.0-or-later](LICENSE), © 2026 Gleb Linnik. The PlxNative name and its brand artwork are excluded — see
[`TRADEMARKS.md`](TRADEMARKS.md), which also carries the Plex and LG non-affiliation statements.
Third-party components and their licences are in
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) and `licenses/` — notably the app ships its own
LGPL build of FFmpeg. Those notices and licence texts ship inside the `.ipk` too, so they travel
with the binary and not only with this repository.

This is an unofficial client, not affiliated with, endorsed by, or sponsored by Plex GmbH or LG
Electronics. "Plex", "Rotten Tomatoes", "IMDb", "TMDB", "LG" and "webOS" are trademarks of their
respective owners; where they appear in the app, they identify whose service or score is being shown.
