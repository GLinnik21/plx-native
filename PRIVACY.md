# Privacy

**PlxNative sends nothing to its developer unless you switch it on, and it is off until you do.**
Two switches — crash reports, and which screens and formats get used — both off by default, both
reversible, and the screen that asks shows you the exact messages before you answer. This document
is the whole account of what the app stores, reads and reaches, written to be checkable rather than
reassuring — every claim names the file that implements it.

*Applies to version 0.5.0. If you are reading this in the repository it describes `main`; the release
notes say when it changed.*

---

## The short version

| | |
|---|---|
| Sent to the developer | **Nothing, unless you switch it on.** Two switches, both off by default, and you see the exact messages before you answer. |
| Sent to Plex | What signing in to Plex requires, and what playing a file requires. |
| Sent to your server | The requests any Plex client makes. |
| Stored on the TV | Your session token, where you were last, your answer to the two switches, and three log files. |
| Third-party analytics | **Off by default.** If you turn it on: Sentry and PostHog, in the EU, with no identity attached. There is no SDK for either in the binary — the messages are built by hand, and the schema is below. |
| Advertising identifiers | **Never read.** LG's `LGUDID` is not called. |

There is no account with me, because there is no *me* to have an account with — no server, no
database, no mailing list. The switches above do not create one: what they send is joined to a
random number that television invented for itself and can delete, not to you.

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
off by default. What they receive, field by field, is the table further down, and that table is
generated from the code rather than written beside it.

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
holds the two booleans and, only after you turn one on, the random identifier described above.
Turning both off deletes that identifier from the file.

**Two files that exist only while telemetry is on**, beside it and both 0600:
`<app id>-telemetry-spool.bin` is the queue of messages waiting to be sent — capped at half a
megabyte, oldest dropped first, and emptied as they go — and `<app id>-telemetry-crashmark.json`
holds a single number, how much of the crash log has already been reported. That number is why a
crash is reported once rather than on every launch, and why the crash log itself can stay
append-only for you to read.

**Three log files** in `/tmp`, all created 0600 (`src/main.c`), two truncated each launch and the
crash log append-only so it survives a restart. `/tmp` is cleared by a reboot.

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

## What is read and never written

The television's own codec table (`/etc/umediaserver/device_codec_capability_config.json`) and its
firmware identity (`/var/run/nyx/os_info.json`, `/var/run/nyx/device_info.json`). All three are
published by the platform and read once at boot, to decide what can be played directly.

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
questions, so they are separate answers. The screen that asks shows you the exact messages that
would be sent, in full, before you answer — and it is asked once: dismissing it with both off IS a
no, and it is not asked again.

These were written down as terms before any of it was built, so a version could be held to them
rather than announcing them alongside the thing itself. They are now the description of what ships,
and each one is checked by something rather than promised:

1. **Opt-in, off by default, never bundled.** Two independent switches.
2. **Nothing is stored to enable it before you say yes.** No identifier exists until you opt in, and
   turning both switches off deletes it. If the television has no source of randomness the opt-in is
   refused outright rather than an identifier being invented from a clock or a MAC address.
3. **You can read the payload on screen before it is sent**, in full, as it would be transmitted.
   That preview is generated from the same code that builds the real messages, so an event nobody
   documented appears in front of you rather than in a dashboard.
4. **A build carries an endpoint only if one was compiled into it.** A binary built without one has
   no address to send to — `strings` on the binary answers that, and each release audit reports
   what it found there.
5. **Never**: media titles, ratingKeys, search terms, subtitle text, server names or addresses, your
   Plex account, or anything derived from the MAC address or serial number. This is structural, not
   careful: there is no field in the message type that could hold text this app read at runtime.
6. The literal structure sent is documented **in this file**, field by field, below — and the table
   is generated from the code, so an event or a field that is not in it fails the build.
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
document nobody had to live with — and it is kept honest mechanically rather than by intention: a
new event that is not in this table fails `make check`.

| event | fields |
|---|---|
| `app.launch` | *(none)* |
| `route.entered` | `screen` — one of a fixed list of screen names |
| `signin.completed` | *(none)* |
| `playback.requested` | `playback_id` — a random number minted per attempt, never stored and never reused |
| `playback.started` | `playback_id` — a random number minted per attempt, never stored and never reused; `mode` — `direct` or `transcode`; `raster` — `sd` / `hd` / `fhd` / `uhd` / `unknown` — never the raster; `fps` — a fixed rung: `24`/`25`/`30`/`50`/`60`/`100`/`other`/`unknown` — never the measured rate; `video` — a codec name from a fixed table; anything else is `other`; `audio` — a codec name from a fixed table; anything else is `other`; `startup` — `<1s` / `1-3s` / `3-10s` / `10s+` — never the interval |
| `playback.failed` | `playback_id` — a random number minted per attempt, never stored and never reused; `mode` — `direct` or `transcode`; `kind` — `decision_refused` / `no_video_transcode_target` / `no_video_track` / `unspecified` |
| `playback.ended` | `playback_id` — a random number minted per attempt, never stored and never reused; `mode` — `direct` or `transcode`; `watched` — `abandoned` / `some` / `most` / `finished` — never a position or a duration |

**`playback_id` is not an identifier of you or of this television.** It is a random number minted
afresh each time Play is pressed, never written to disk and never reused. It exists so that the four
events of one attempt can be joined to each other — without it, "how often does playback fail"
becomes two unrelated counters. It cannot link two playbacks, let alone two televisions.

**Everything descriptive on those events is a CLASS, not a measurement**, and that is deliberate.
An exact duration, an exact raster, an exact frame rate and a codec together are enough to identify
one particular file in one particular library. The classes answer the questions this exists to
answer — does 4K HEVC fail more often than 1080p h264, does playback take longer to start on large
files — and identify nothing. **No title, rating key, file name, path, server name or address
appears on any of them**, and there is no field that could carry one.

Three things are true of that table by construction rather than by care, and
`rust-modules/src/diag/schema.rs` is where you can check each one:

- **No field can hold text this app read at runtime.** Every value is either absent or one of a
  fixed set of names compiled into the binary. A test greps the type for an owned string and fails
  the build if one appears.
- **The list is exhaustive.** One enum, one serialiser, one name list, and a test that fails if any
  of the three falls behind the others.
- **This table is part of that check.** A new event that is not listed here fails `make check`, so
  the document cannot lag the code.

What is deliberately *not* in it: anything identifying the television, the account or the household.
Model, firmware and app version are per-session facts that would belong to an upload envelope, not
to every record, and no envelope exists yet either.

---

## Your rights

**With both switches off, there is no data of yours in my possession** — nothing to request,
correct or delete, because nothing was ever sent.

**With one on, there is, and here is the honest shape of it.** What has been sent carries no name,
no account and no address — only the random per-attempt numbers described above — so I cannot find
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
