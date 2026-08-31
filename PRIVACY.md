# Privacy

**PlxNative sends nothing to its developer unless you switch it on, and it is off until you do.**
Two switches — crash reports, and anonymous usage events such as screens, feature actions, sign-in
outcomes and playback format/outcome classes — both off by default, both
reversible, and the screen that asks shows you the exact schemas (with dynamic values as explicit
placeholders) before you answer. This document
is the whole account of what the app stores, reads and reaches, written to be checkable rather than
reassuring — every claim names the file that implements it.

*Applies to version 0.5.0. If you are reading this in the repository it describes `main`; the release
notes say when it changed.*

---

## The short version

| | |
|---|---|
| Sent to the developer | **Nothing, unless you switch it on.** Two switches, both off by default, and you see the exact schemas before you answer. |
| Sent to Plex | What signing in to Plex requires, and what playing a file requires. |
| Sent to your server | The requests any Plex client makes. |
| Stored on the TV | Your session token, where you were last, your answer to the two switches, three log files, and bounded telemetry queues while enabled. |
| Third-party analytics | **Off by default.** If you turn it on: Sentry and PostHog, in the EU. Crash reports carry no install identifier; usage has one random, locally deletable install id plus app/webOS/model/SoC/hardware and coarse network-path compatibility classes. Sentry Native captures crashes but has no network transport; PlxNative sanitises and sends every message itself. |
| Advertising identifiers | **Never read.** LG's `LGUDID` is not called. |

There is no account with me, because there is no *me* to have an account with — no server, no
database, no mailing list. The switches above do not create one: usage is joined only to a random
number that television invented for itself and can delete; a crash is not joined to even that.

---

## What leaves the television

**`plex.tv` and `discover.provider.plex.tv`, over TLS.** Signing in, refreshing your server list, and
looking up cast members. Plex sees what its own API is told: a client identifier this install
generates once, the product name and version, and your account's activity. Plex's handling of that is
governed by [Plex's privacy policy](https://www.plex.tv/about/privacy-legal/privacy-preferences/),
not by this one — I am a third-party client and I receive none of it.

**Your Plex Media Servers**, at a LAN address where one answers and a public one otherwise. Browsing,
playback, and the progress reports that make "resume where you left off" work. Your server is yours.

**Sentry and PostHog, in the European Union — and only if you switched a telemetry switch on.** Both
off by default. What they receive, field by field, is documented further down. The usage table is
generated from the typed event declarations; the native-crash table is checked against the
sanitizer's own field allowlist.

**Nothing else.** Each release audit under `docs/release-audits/` lists the host-shaped strings
found in the shipped binary, so the claim is checkable against the artifact — with the limit that
script states about itself: it is a FLOOR on what the app can reach and never a proof of absence,
since a hostname assembled at runtime from pieces leaves no literal to find. (This paragraph used to
credit `ci/check-elf.sh` with the same measurement. It makes none — it grades the ELF's class, ABI,
linkage and build identity, and its one grep for a *path* is for build-host paths leaking into the
binary, which is a different thing that happens to share a word.)

---

## What is stored on the television

**Your session** — `<app id>-auth.json` under `/media/developer` or `/media/internal`, created 0600
through `open(2)`'s own mode argument (`plex/session.rs`). It holds one access token per server your
account can reach, your profile list, and a randomly generated client identifier. Signing out deletes
it and the identifier is regenerated next time.

**Where you were** — `<app id>-lastplace.json` beside it: the page, your profile id, one server id and
one item id, so the app reopens where you left it.

**Your answer to the two telemetry switches** — `<app id>-telemetry.json`, beside the other two. It
holds the two booleans and, only while usage analytics is on, the random identifier described above.
Turning usage analytics off deletes that identifier even if crash reporting remains on.

**Two files that exist only while telemetry is on**, beside it and both 0600:
`<app id>-telemetry-spool.bin` is the queue of messages waiting to be sent — capped at half a
megabyte, with crash reports retained ahead of usage events and newest retained within each
category, and emptied as they go — and `<app id>-telemetry-crashmark.json`
holds a single number, how much of the crash log has already been reported. That number is why a
crash is reported once rather than on every launch, and why the crash log itself can stay
append-only for you to read.

**Three log files** in the install's runtime directory under `/tmp`, all created 0600
(`src/main.c`), two truncated each launch and the
crash log append-only so it survives a restart. `/tmp` is cleared by a reboot.

**Native crash working files**, only while crash reporting is enabled and only under that same
runtime directory: `plxnative-sentry-db/` is the 0700 directory shared with the out-of-process
capture daemon, and `plxnative-sentry-pending/` holds 0600 event envelopes until the next healthy
launch has copied them into the ordinary bounded telemetry spool. A clean shutdown removes the
database. Turning crash reporting off stops the daemon and removes both directories; a successfully
queued or rejected envelope is deleted immediately.

### What the log may and may not contain

This is the part worth being precise about, because a log gets photographed and pasted into issue
threads. Every line goes through `diag::scrub::scrub_local` **before it is written to disk**, which
rewrites:

- Plex tokens and any `Authorization`, `Cookie` or `X-Plex-Token` header value
- `token=`, `password=`, `api_key=` and similar query parameters
- hostnames, including `plex.direct` names — *those encode your LAN address in the leftmost label*
- bare IP addresses and ports (loopback and `0.0.0.0` survive; they identify nobody)
- Plex GUIDs and search queries
- your server names, your profile names and their identifiers, once the session has loaded

And these are not written in the first place, because a scrubber cannot reliably recognise them:

- **media titles** — the Up Next line logs a ratingKey, not an episode title (`app.rs`)
- **your search terms** — `search.rs` logs the query's length, never the query
- **subtitle dialogue** — the cue line logs a character count, never the text (`player/mod.rs`)

A test called `no_log_call_site_interpolates_viewing_content` greps the source tree on every
`make check` to keep it that way, and it is itself verified to fail when a leak is reintroduced.

**What remains** is ratingKeys — server-local item numbers. They are the primary handle for
diagnosing a playback bug, the file is 0600, and they stay. If you post a log publicly, someone with
access to the same server could work out which item a number refers to.

---

## Platform inventory

The television's own codec table (`/etc/umediaserver/device_codec_capability_config.json`) and its
firmware identity (`/var/run/nyx/os_info.json`, `/var/run/nyx/device_info.json`). All three are
published by the platform and read once at boot, to decide what can be played directly. When usage
analytics is enabled, the app/webOS/API/codename/model/SoC/hardware-revision compatibility classes
listed below are also included with usage events. No serial number or LG device identifier is read.

**`LGUDID` is deliberately not read.** webOS offers a device identifier through
`luna://com.webos.service.sm/deviceid/getIDs`; it is derived from the MAC address, and this app never
asks for it.

---

## What listens

Nothing. A release build compiles out the `/tmp` trigger surface, the remote-control FIFO and the TCP
capture listener that exist in a development build. Measured per release on the shipped bytes.

---

## Telemetry

**Two switches, both off until you turn them on.** Crash reports and usage statistics are separate
questions, so they are separate answers. The screen that asks shows every exact schema, with values
that only exist at runtime shown as placeholders, before you answer — and it is asked once:
dismissing it with both off IS a
no, and it is not asked again.

These were written down as terms before any of it was built, so a version could be held to them
rather than announcing them alongside the thing itself. They are now the description of what ships,
and each one is checked by something rather than promised:

1. **Opt-in, off by default, never bundled.** Two independent switches.
2. **Nothing is stored to enable it before you say yes.** No usage identifier exists until you opt
   into usage analytics, and turning that switch off deletes it independently of crash reporting.
   If the television has no source of randomness the usage opt-in is refused outright rather than
   an identifier being invented from a clock or a MAC address; anonymous crash reporting does not
   need that identifier.
3. **You can read every payload schema on screen before it is sent.** Usage examples run through
   their real serializer; the native crash example runs through the same path sanitizer as a real
   envelope. Runtime-only addresses, ids and times are visibly labelled placeholders. An event or
   field nobody documented therefore appears in front of you rather than only in a dashboard.
4. **A build carries an endpoint only if one was compiled into it.** A binary built without one has
   no address to send to — `strings` on the binary answers that, and each release audit reports
   what it found there.
5. **Never**: media titles, ratingKeys, search terms, subtitle text, server names or addresses, your
   Plex account, or anything derived from the MAC address or serial number. Usage EVENT types cannot
   hold runtime text; the separate bounded context can hold only the platform compatibility and
   coarse connection classes declared below. Native reports are constrained to SDK machine state, reject user/request
   scopes, and reduce every module/source path to its basename before the durable queue accepts it.
6. The literal structure sent is documented **in this file**, field by field, below. The usage
   table is generated from the typed event declarations, so an event or field missing from it fails
   the build. The native-crash schema is tested against the same field allowlist used by the
   sanitizer and by the on-screen preview.
7. **Turning it off stops collection and discards anything not yet sent — but what has already been
   sent ages out on a retention clock rather than being erased on request.** Stated because it is a
   limitation rather than a choice: these services can only delete data belonging to an *account*,
   and the whole design is that these reports belong to nobody. That is the right trade — an
   identifier that made erasure possible would be an identifier worth not having — but it is not
   the same as deletion and is not described as if it were.

### Where it goes, and for how long

Sentry and PostHog, both in the **European Union**. Sentry receives crash reports; PostHog receives
usage events. Neither receives the other's.

**Kept no longer than 13 months, and in practice less** — that ceiling is the commitment; the actual
figures are the services' own, and both are shorter: PostHog's free plan retains product analytics
for one year, and Sentry retains errors for 30 to 90 days depending on plan. If the PostHog side
ever moves to a paid plan the retention gets *longer*, and that is a change to this document before
it is a change to a billing page.

There is deliberately **no scheduled deletion job**, and it is worth saying why rather than leaving
its absence to be read as an oversight. One was designed and dropped for three independent reasons:
the storage already expires the data sooner than the ceiling above; PostHog has no rolling
delete-by-timestamp — its deletion primitive is *person*-scoped, and these events carry no person by
construction, so the job could not do what it claimed; and a scheduled GitHub Action on a public
repository is disabled after 60 days of inactivity and may be delayed or dropped, which is not a
mechanism a commitment can rest on.

### The schema, as it stands

Term 6 says the structure is documented here before it ships, so here it is. It was written down
before the sender existed, precisely because a document produced alongside a working uploader is a
document nobody had to live with. The usage table is rendered from `EVENT_SPECS`; the native table
is enforced by the sanitizer's field allowlist and an exact preview test. Either drifting fails
`make check`.

Every usage event also carries this compatibility and connection context. Values are capped at 64
ASCII characters and taken from the app build, nyx's platform inventory and the winning Plex
connection classification. Network classes are attached only when an action addresses one exact
server; generic app/screen events use `unknown`, because an account may have N servers and there is
no honest single answer. `server_connection` and `ip_version` are classes only: no address,
hostname, port, server id or server name is included.

| property | value |
|---|---|
| `app_version` | the PlxNative package version |
| `webos_release` | the webOS release reported by nyx |
| `webos_api` | the webOS API version reported by nyx |
| `webos_codename` | the webOS firmware family reported by nyx |
| `device_model` | the LG model/platform class reported by nyx |
| `soc` | the SoC/board class reported by nyx |
| `hardware_revision` | the hardware revision class reported by nyx |
| `server_connection` | `local` / `remote` / `relay` / `unknown` |
| `ip_version` | `v4` / `v6` / `unknown` |

| event | fields |
|---|---|
| `app.launch` | *(none)* |
| `route.entered` | `screen` — one of a fixed list of screen names |
| `signin.completed` | *(none)* |
| `signin.started` | *(none)* |
| `signin.failed` | `kind` — `pin_create` / `authorization` / `discovery` / `other` |
| `signin.cancelled` | *(none)* |
| `feature.used` | `feature` — one of a fixed list of feature names |
| `playback.requested` | `playback_id` — a random number minted per attempt, never stored and never reused |
| `playback.started` | `playback_id` — a random number minted per attempt, never stored and never reused; `mode` — `direct` or `transcode`; `raster` — `sd` / `hd` / `fhd` / `uhd` / `unknown` — never the raster; `fps` — a fixed rung: `24`/`25`/`30`/`50`/`60`/`100`/`other`/`unknown` — never the measured rate; `video` — a codec name from a fixed table; anything else is `other`; `audio` — a codec name from a fixed table; anything else is `other`; `startup` — `<1s` / `1-3s` / `3-10s` / `10s+` — never the interval |
| `playback.failed` | `playback_id` — a random number minted per attempt, never stored and never reused; `mode` — `direct` or `transcode`; `kind` — `decision_refused` / `no_video_transcode_target` / `no_video_track` / `unspecified` |
| `playback.cancelled` | `playback_id` — a random number minted per attempt, never stored and never reused; `mode` — `direct` or `transcode` |
| `playback.abandoned` | `playback_id` — a random number minted per attempt, never stored and never reused; `mode` — `direct` or `transcode` |
| `playback.quality` | `playback_id` — a random number minted per attempt, never stored and never reused; `rebuffers` — `0` / `1` / `2-3` / `4+`; `buffering` — `none` / `<2s` / `2-10s` / `10s+` — never the interval |
| `playback.ended` | `playback_id` — a random number minted per attempt, never stored and never reused; `mode` — `direct` or `transcode`; `watched` — `abandoned` / `some` / `most` / `finished` — never a position or a duration |

When **crash reporting** is on, a fatal native event has this separate schema. It is not joined to
the random usage install id:

| part | fields |
|---|---|
| event | random per-crash `event_id`; crash time; `platform=native`; `level=fatal`; app release; `development` or `production`; ELF build id (`dist`); Sentry Native SDK name/version |
| exception | signal number and fixed signal name; handled=false; the faulting instruction and caller frames; ARM integer registers `r0`–`r10`, `fp`, `ip`, `sp`, `lr`, `pc`, `cpsr` |
| threads | kernel thread ids, internal thread labels, which thread crashed/currently ran, and — for each non-crashing thread whose kernel context can be captured — ARM registers plus at most 32 caller frames |
| modules | basename only; mapped address and size; ELF code/debug id. Directory names are removed before queueing |
| OS context | Linux kernel version and kernel build suffix reported by `uname` |
| webOS context | webOS name, release, release codename and API version from `/var/run/nyx/os_info.json` |
| hardware context | model/platform, SoC/board and hardware revision classes from `/var/run/nyx/device_info.json`; no serial or device id |

The native SDK is compiled with `SENTRY_TRANSPORT=none`. Its out-of-process daemon is what can read
the stopped process safely; it writes an envelope and relaunches PlxNative in spool-only mode.
PlxNative then accepts exactly one bounded native event, drops the envelope DSN plus any user or
request scope, removes absolute path prefixes and queues the JSON through its own consent gate,
retry spool and TLS sender. No minidump, core, attachment, log, breadcrumb, title or server value is
included. A Rust panic fallback sends its validated compile-time source location and a hash of the
panic message, never the message itself.

Every usage event also carries its original event time and a random per-process `session_id`, so
events held while the television is offline remain in the session in which they occurred. The
session id is not reused after the app exits; the timestamp says when the event occurred and never
contains a media position.

**`playback_id` is not an identifier of you or of this television.** It is a random number minted
afresh each time Play is pressed, never written separately and never reused. It exists so that the
lifecycle events of one attempt can be joined to each other — without it, "how often does playback fail"
becomes two unrelated counters. It cannot link two playbacks, let alone two televisions.

**Everything descriptive on those events is a CLASS, not a measurement**, and that is deliberate.
An exact duration, an exact raster, an exact frame rate and a codec together are enough to identify
one particular file in one particular library. The classes answer the questions this exists to
answer — does 4K HEVC fail more often than 1080p h264, does playback take longer to start on large
files — and identify nothing. **No title, rating key, file name, path, server name or address
appears on any of them**, and there is no field that could carry one.

Three things are true of the usage event table by construction rather than by care, and
`rust-modules/src/diag/schema.rs` is where you can check each one:

- **No event field can hold text this app read at runtime.** Every event value is either absent or one of a
  fixed set of names compiled into the binary. A test greps the type for an owned string and fails
  the build if one appears. Runtime platform strings live only in the separately declared, bounded
  context table above.
- **The list is exhaustive.** One enum, one serialiser, one name list, and a test that fails if any
  of the three falls behind the others.
- **This table is part of that check.** A new event that is not listed here fails `make check`, so
  the document cannot lag the code.

What is deliberately *not* in usage events: a serial number, LGUDID, MAC/IP address, hostname,
server identity, account or household content. Model/SoC/hardware and firmware classes are included
because they define Store distribution and media/API compatibility. The native crash event includes
app and kernel build facts because those are required to reproduce and symbolicate a crash, but no
LG id, Plex id or usage install id.

---

## Your rights

**With both switches off, there is no data of yours in my possession** — nothing to request,
correct or delete, because nothing was ever sent.

**With one on, there is, and here is the honest shape of it.** What has been sent carries no name,
Plex/LG account or network address — only the random usage install id (when usage is on), a random
per-process usage session id, per-attempt playback numbers, per-crash event ids, and the
compatibility classes above. I cannot find
"your" records to erase on request even in principle, and neither can Sentry or PostHog, whose
deletion tools work on accounts these reports deliberately do not have. What happens instead is that
it expires: the retention above, never longer than 13 months. Turning a switch off stops collection
at once and destroys anything still queued on the television, which is the part that IS under your
control.

**The files on the television survive an uninstall**, and that is a platform fact rather than a
choice: webOS gives a native app no uninstall hook, so nothing of mine runs on the way out. Delete
them yourself if you want them gone — `<app id>-auth.json`, `<app id>-lastplace.json`,
`<app id>-telemetry.json`, `<app id>-telemetry-spool.bin` and `<app id>-telemetry-crashmark.json`,
under `/media/developer` or `/media/internal` depending on how the app was installed. This is
written down because `rust-modules/src/paths.rs` cites this document as the place a user can learn
it, and until now it did not say so.

**Questions:** open an issue at <https://github.com/GLinnik21/plx-native/issues>, or e-mail
glinnik21@gmail.com. Security reports go through [SECURITY.md](SECURITY.md) instead.

*PlxNative is an independent application. It is not produced by, endorsed by, or affiliated with
Plex, Inc. or LG Electronics Inc.*
