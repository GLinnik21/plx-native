# Troubleshooting

Haven't installed FinNative yet? Start with the [installation guide](install-finnative.md).
This page is about what to do when something doesn't work.

## The app tile does nothing

FinNative needs **webOS 4.0 or newer**. Older firmware can't start it, so the tile just does
nothing when you select it. See [Will it work on my
television?](../README.md#will-it-work-on-my-television) for the full picture, including sets
that start but hit a specific known problem.

## Developer Mode removed the app

LG Developer Mode expires and removes every app it installed unless you renew the session before
it lapses. [Renew it in the Developer Mode app](install-finnative.md#important-developer-mode-expires),
or reinstall FinNative if the session already expired. Only a rooted TV with Homebrew Channel has
no expiry.

## Can't sign in, or no server is found

The sign-in screen says what stopped it:

- **"No Jellyfin server answered on this network. Use Manual sign-in to enter its address."**
  FinNative looked for servers on your home network and none answered. Check the server is
  running, or choose **Manual sign-in** and type its address, for example `192.168.1.20:8096`.
  A server on another network, or one that doesn't answer discovery, has to be entered this way.
- **"Couldn't reach your Jellyfin server. Check the address and that the server is on."** Nothing
  answered at the address you typed. Check the address, the port (Jellyfin's default is `8096`)
  and that the TV can reach that network.
- **"There's no Jellyfin server at that address."** Something answered, but it isn't Jellyfin.
  Check the address and port.
- **"This server isn't encrypted, so it can only be used at its IP address on your home network,
  like http://192.168.1.20:8096. Or use its https:// address."** FinNative sends your password
  over plain `http://` only to a numeric address on your home network. Type the server's local IP
  address, or its `https://` address.
- **"Wrong username or password."** The server refused them.
- **"Quick Connect is off on your Jellyfin server."** An administrator can turn it on in the
  Jellyfin dashboard under **General**, or you can sign in with a password from **Manual sign-in**.

If none of that explains it, [collect the logs](#collecting-logs) and
[open an issue](https://github.com/tiper/fin-native/issues) with your TV model, webOS platform
release and Jellyfin server version.

## "Playback failed" on some items

FinNative asks your server what to do before every video. When the TV can't play a file as it is,
the server has to convert it, and that fails when the server can't transcode or its administrator
switched transcoding off for your user. Items the TV plays directly still work. Check the
server's playback settings in the Jellyfin dashboard, or try a different version of the item.

## Seeking or resuming fails through Cloudflare

A server behind Cloudflare can answer FinNative's first request for a position in a file with the
whole file instead, which FinNative refuses, so seeking or resuming can fail there. Connecting to the server's local address on your home network avoids it.

## Playback reports `jail_missing_rtkmem`

This is a specific problem on Realtek **k5lp/k3lp** sets: LG's Developer Mode sandbox on some of
these sets withholds a device (`/dev/rtkmem`) that native video decoding needs. FinNative detects
this and reports it instead of crashing. See the [Known
issues](../README.md#known-issues) table for which sets are affected.

On a **rooted TV with Homebrew Channel**, the failure screen offers **Repair**, which asks
Homebrew Channel's elevated service to patch the sandbox. Read the [full repair procedure and its
limits](native-video-sandbox.md) before using it: it only runs once per app session, and a timeout
leaves the outcome unknown until you fully close and reopen FinNative. **Without root, there is
currently no fix.**

Don't use Repair for a plain black screen, a refused transcode, an unsupported codec, or any
failure other than `jail_missing_rtkmem` — it addresses that one sandbox problem and nothing else.

## Collecting logs

FinNative keeps a few small log files on the TV. When you report a problem, they are what lets
the problem be found. You can fetch them yourself with [webOS Dev
Manager](install-finnative.md#2-connect-your-computer-to-the-tv). On a television that is not
rooted, Dev Manager should be able to download them, because they are readable by the app's group.

### Which log to attach

| What happened | Attach |
| --- | --- |
| Playback failed, stalled or stopped (for example "Playback stopped after it had started") | the event log |
| The app closed by itself, vanished, or the TV returned to the launcher | the crash log **and** the event log |
| The app does not start, or shows a black screen and exits | the stderr log **and** the crash log |

If you are not sure, attach all three. They are small.

### Where they are

FinNative keeps its logs in a folder of its own under `/tmp/`, so it never overwrites
PlxNative's:

| Install | Where the log files are |
| --- | --- |
| FinNative (release) | in the folder `/tmp/io.github.tiper.finnative/` |
| FinNative debug build | in the folder `/tmp/io.github.tiper.finnative.debug/` |

The three files have the same names in both places (they keep PlxNative's names):

- `plxnative-events.log` — the event log: what the app did, line by line.
- `plxnative-crash.log` — the crash log: a record each time the app was stopped by a fault.
- `plxnative-stderr.log` — what the app and the TV's own libraries printed to the error stream.

### Fetch them before you reopen the app

The event log and the stderr log start empty **every time FinNative launches**. If you reproduce
the problem, close FinNative and open it again, the log of the failure is gone. So:

1. Reproduce the problem.
2. Leave the app as it is, or just go back to the launcher. Do not launch FinNative again yet.
3. Fetch the logs, as below.

The crash log is different: it only grows, so earlier crashes stay in it across relaunches. It is
cleared when the TV restarts, because `/tmp` is.

### Fetching them with Dev Manager

1. Open **webOS Dev Manager** on your computer and select your TV. It must be connected the same
   way you installed FinNative.
2. Open the **Files** view. It lists the TV's folders, and its breadcrumb bar at the top starts
   at `/`.
3. Click `/`, then double-click **tmp**, then **io.github.tiper.finnative** (or
   **io.github.tiper.finnative.debug** for a debug build).
4. Click a log file to select it, then click the **Download** button in the toolbar and choose
   where to save it on your computer.
5. Attach the downloaded files to your [GitHub issue](https://github.com/tiper/fin-native/issues).
   If a file is large, compress it first.

If a file is missing, the app has not run since the TV last restarted, or you are looking in the
wrong folder for your install. If Dev Manager shows the file but cannot download it, say so in
your report instead of giving up; the project wants to know.

### What to include in the report

Logs help most with a few facts beside them:

- your TV model (from the sticker on the back, or Settings, then About This TV) and webOS
  platform release;
- the FinNative version and your Jellyfin server version;
- whether the item was Direct Play or a chosen quality;
- what you did and what happened.

On a playback failure screen, the small line at the bottom adds facts the project needs. For
example:

```
FinNative 0.1.0 · webOS 9.2.4 · k24 · K24_DVB · BOARD_DV_1ST · playback_interrupted
```

It reads: the FinNative version, the webOS release, then what the TV reports about itself — on
many sets the first of these is the platform codename (`k24` here) rather than the model on the
sticker, followed by the chip or board and the hardware revision — and last the failure code. Some
parts can be missing on some TVs. It does not replace your TV model from the sticker; send both.
Copy the line exactly into your report, or photograph the screen.

### What the logs contain

Every line of the event log is cleaned before it is written: tokens and other credentials, server and
profile names, network addresses and search queries are removed or rewritten, and
titles and subtitle text are never written. What remains can include item identifiers from your
server. Someone with access to the same server could match
one to a title, so think before you post a log in a public issue. The crash and stderr logs are
not cleaned line by line the same way; they hold fault addresses and whatever the TV's libraries
print. [The privacy policy](../PRIVACY.md) and [what the app
writes](install-finnative.md#what-it-writes) spell this out. Never post an access token or a
passphrase, even if you see one in a file.

### Sending a report from the app instead

There is no "send logs" button in the app, and FinNative sends no crash reports or analytics, so
an issue with your logs and TV details is the only way a problem reaches the project.

## AV1 and VP9 files don't play

FinNative direct plays AV1 only on a television whose own codec table lists an AV1 decoder, and
VP9 only on one whose table lists a VP9 decoder, and 10-bit (Profile 2) VP9, HDR10 included, only on a
webOS 4 television, the one family PlxNative has measured it on. On any other set, or for a 10-bit VP9 file
on a newer webOS, the server has to convert the item, which fails when your server can't transcode or
transcoding is switched off.

If your TV decodes AV1 or VP9 and the item still doesn't play, [collect the event log](#collecting-logs)
before you reopen the app and attach it to a [GitHub issue](https://github.com/tiper/fin-native/issues).
The log's `devcaps:` line says `av1=true` (or `vp9=true`) when the TV reports that decoder, and
`av1=false` (`vp9=false`) when it doesn't, which tells us right away whether the TV or the app is
at fault.

## Other playback failures

Try the current release first, then photograph the failure screen and [collect the
logs](#collecting-logs) before you reopen the app. Include:

- your TV model (from the sticker on the back, or Settings, then About This TV) and webOS
  platform release;
- the FinNative version and your Jellyfin server version;
- whether the item was Direct Play or a chosen quality;
- what happened after you pressed Play.

Report it in [GitHub Issues](https://github.com/tiper/fin-native/issues), not to the PlxNative
project.
