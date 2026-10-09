# Troubleshooting

Haven't installed PlxNative yet? Start with the [installation guide](install-and-verify.md).
This page is about what to do when something doesn't work.

## The app tile does nothing

PlxNative needs **webOS 4.0 or newer** (webOS 3.4.3 has also been tested and works). Older
firmware may not be able to start it at all, so the tile just does nothing when you select it. See [Will it work on my
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
current release, [open an issue](https://github.com/GLinnik21/plx-native/issues/new/choose)
with your TV model and webOS platform release. If the sign-in screen shows a failure, also use
**Details → Send report** and quote the **Report ID** it then shows in your issue, as described in
[Can't sign in, or no servers are found](#cant-sign-in-or-no-servers-are-found). A report on its
own carries no way to reply to you.

## Can't sign in, or no servers are found

The sign-in screen says what stopped it. The wording that matters most:

- **"Signed in as *your account*. This Plex account has no server yet."** Sign-in worked, but
  plex.tv listed no Plex Media Server on that account, so the app had nothing to connect to. This
  is about the account, not your network. Check that you signed in with the same Plex account your
  server is signed in to, and that it is the account you use in Plex's own apps. Signed in
  with a different account, you will see exactly this.
- **"plex.tv listed your servers, but none of them answered. Make sure your Plex Media Server is on
  and online, then try again."** The account has a server, but the TV could not reach it. Check
  that it is running and connected.
- **"Your Plex server refused the connection — check its network access settings."** The server
  answered and said no.
- **"This TV couldn't make a secure connection to plex.tv. Check the TV's date and time, then try
  again."**, or on the Home screen **"This TV's clock looks wrong. Connect the TV to the internet
  once, then try again."** A certificate is only valid between two dates, so a TV with the wrong
  date or time can fail to make a secure connection. Set it right in the TV's settings.

If none of that explains it, press **Details** on the failure screen. Once a report has been sent
or saved, the card shows **Report ID** followed by letters and digits in groups of four
(for example `41de 4cd3 88e4 0416 54de 38f2 787c 3922`). If it shows no Report ID yet and offers
**Send report**, press that, then open **Details** again. The report says which sign-in step failed
and how the connection answered; it never includes your account name, tokens, PIN, sign-in code or
network addresses.

A report carries no name or account, so I can only find yours by its Report ID. Please quote the
**Report ID** when you ask for help, either in [Discussions >
Q&A](https://github.com/GLinnik21/plx-native/discussions/categories/q-a) or in a [GitHub
issue](https://github.com/GLinnik21/plx-native/issues/new/choose), together with your TV model and
webOS platform release.

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

## Collecting logs

PlxNative keeps a few small log files on the TV. When you report a problem, they are what lets
the problem be found. You can fetch them yourself with [webOS Dev
Manager](install-and-verify.md#2-connect-your-computer-to-the-tv). On a television that is not
rooted, Dev Manager should be able to download them, because they are readable by the app's group.

### Which log to attach

| What happened | Attach |
| --- | --- |
| Playback failed, stalled or stopped (for example "Playback stopped after it had started") | the event log |
| The app closed by itself, vanished, or the TV returned to the launcher | the crash log **and** the event log |
| The app does not start, or shows a black screen and exits | the stderr log **and** the crash log |

If you are not sure, attach all three. They are small.

### Where they are

Where the files are depends on which PlxNative you run. The regular release keeps its logs
directly in `/tmp/`. PlxNative Nightly keeps its own copies in a folder of its own, so it never
overwrites the regular app's logs.

| Install | Where the log files are |
| --- | --- |
| PlxNative (release) | directly in `/tmp/`, for example `/tmp/plxnative-events.log` |
| PlxNative Nightly | in the folder `/tmp/com.beb.plxnative.nightly/` |

The three files have the same names in both places:

- `plxnative-events.log` — the event log: what the app did, line by line.
- `plxnative-crash.log` — the crash log: a record each time the app was stopped by a fault.
- `plxnative-stderr.log` — what the app and the TV's own libraries printed to the error stream.

### Fetch them before you reopen the app

The event log and the stderr log start empty **every time PlxNative launches**. If you reproduce
the problem, close PlxNative and open it again, the log of the failure is gone. So:

1. Reproduce the problem.
2. Leave the app as it is, or just go back to the launcher. Do not launch PlxNative again yet.
3. Fetch the logs, as below.

The crash log is different: it only grows, so earlier crashes stay in it across relaunches. It is
cleared when the TV restarts, because `/tmp` is.

### Fetching them with Dev Manager

1. Open **webOS Dev Manager** on your computer and select your TV. It must be connected the same
   way you installed PlxNative.
2. Open the **Files** view. It lists the TV's folders, and its breadcrumb bar at the top starts
   at `/`.
3. Click `/`, then double-click **tmp**. For PlxNative Nightly, then double-click
   **com.beb.plxnative.nightly**.
4. Click a log file to select it, then click the **Download** button in the toolbar and choose
   where to save it on your computer.
5. Attach the downloaded files to your [GitHub issue](https://github.com/GLinnik21/plx-native/issues/new/choose).
   If a file is large, compress it first.

If a file is missing, the app has not run since the TV last restarted, or you are looking in the
wrong folder for your install. If Dev Manager shows the file but cannot download it, say so in
your report instead of giving up; the project wants to know.

### What to include in the report

Logs help most with a few facts beside them:

- your TV model (from the sticker on the back, or Settings, then About This TV) and webOS
  platform release;
- the PlxNative version, and how you installed it (Dev Manager, Homebrew Channel or nightly);
- whether the item was Direct Play or a chosen quality;
- what you did and what happened.

On a playback failure screen, the small line at the bottom adds facts the project needs. For
example:

```
PlxNative 0.8.0 · webOS 9.2.4 · k24 · K24_DVB · BOARD_DV_1ST · playback_interrupted
```

It reads: the PlxNative version, the webOS release, then what the TV reports about itself — on
many sets the first of these is the platform codename (`k24` here) rather than the model on the
sticker, followed by the chip or board and the hardware revision — and last the failure code. Some
parts can be missing on some TVs. It does not replace your TV model from the sticker; send both.
Copy the line exactly into your report, or photograph the screen.

### What the logs contain

Every line of the event log is cleaned before it is written: tokens and other credentials, server and
profile names, network addresses, Plex GUIDs and search queries are removed or rewritten, and
titles and subtitle text are never written. What remains includes ratingKeys,
which are Plex's item numbers on your server. Someone with access to the same server could match
one to a title, so think before you post a log in a public issue. The crash and stderr logs are
not cleaned line by line the same way; they hold fault addresses and whatever the TV's libraries
print. [The privacy policy](../PRIVACY.md) and [what the app
writes](install-and-verify.md#what-it-writes) spell this out. Never post a Plex token or a
passphrase, even if you see one in a file.

### Sending a report from the app instead

There is no "send logs" button in the app, and the playback failure screen has no **Send report**
button (only the sign-in screen has one). Two optional switches in **Settings → Privacy & data** send technical details to the
project by themselves. **Crash reports** is off until you turn it on. With it on, the app sends a
crash report, and also a short report when playback reaches the failure screen: the failure kind
and coarse connection and quality details, with no title, address or token. It does not send these
log files, and it does not replace an issue with your TV details. Turning it on is your choice and
never needed to get help.

## Other playback failures

Try the current release first, then photograph the failure screen and [collect the
logs](#collecting-logs) before you reopen the app. Include:

- your TV model (from the sticker on the back, or Settings, then About This TV) and webOS
  platform release;
- the PlxNative version;
- whether the item was Direct Play or a chosen quality;
- what happened after you pressed Play.

Report it in [GitHub Issues](https://github.com/GLinnik21/plx-native/issues/new/choose).

If you're sharing an old event log, know that log redaction has improved across releases — a log
written by a much older PlxNative version can still show an address that a current release would
scrub before writing it. Where you can, capture a fresh log on the release you're reporting
against.
