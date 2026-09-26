# Troubleshooting

Haven't installed PlxNative yet? Start with the [installation guide](install-and-verify.md).
This page is about what to do when something doesn't work.

## The app tile does nothing

PlxNative needs **webOS 4.0 or newer** — older firmware can't start it at all, so the tile just
does nothing when you select it. See [Will it work on my
television?](../README.md#will-it-work-on-my-television) for the full picture, including sets
that start but hit a specific known problem.

## Developer Mode removed the app

LG Developer Mode expires and removes every app it installed unless you renew the session before
it lapses. [Renew it in the Developer Mode app](install-and-verify.md#important-developer-mode-expires),
or reinstall PlxNative if the session already expired. Installing Homebrew Channel through
Developer Mode does not remove that requirement — only a rooted TV with Homebrew Channel has no
expiry.

## Sign-in doesn't survive closing the app

This used to happen and is fixed: PlxNative now keeps your sign-in in its own private storage, and
uses the TV's key manager service where the platform provides one. If you still see it on the
current release, use **Details → Send report** on the sign-in screen, or [open an
issue](https://github.com/GLinnik21/plx-native/issues) with your TV model and webOS platform
release.

## Playback reports `jail_missing_rtkmem`

This is a specific problem on Realtek **k5lp/k3lp** sets: LG's Developer Mode sandbox on some of
these sets withholds a device (`/dev/rtkmem`) that native video decoding needs. PlxNative detects
this and reports it instead of crashing. See the [Known
issues](../README.md#known-issues) table for which sets are affected.

On a **rooted TV with Homebrew Channel**, the failure screen offers **Repair**, which asks
Homebrew Channel's elevated service to patch the sandbox. Read the [full repair procedure and its
limits](native-video-sandbox.md) before using it: it only runs once per app session, and a timeout
leaves the outcome unknown until you fully close and reopen PlxNative. **Without root, there is
currently no fix.**

Don't use Repair for a plain black screen, a refused transcode, an unsupported codec, or any
failure other than `jail_missing_rtkmem` — it addresses that one sandbox problem and nothing else.

## Other playback failures

Try the current release first, then photograph the failure screen. Include:

- your TV model and webOS platform release;
- the PlxNative version;
- whether the item was Direct Play or a chosen quality;
- what happened after you pressed Play.

Report it in [GitHub Issues](https://github.com/GLinnik21/plx-native/issues).

If you're sharing an old event log, know that log redaction has improved across releases — a log
written by a much older PlxNative version can still show an address that a current release would
scrub before writing it. Where you can, capture a fresh log on the release you're reporting
against.
