# Privacy

**PlxNative sends nothing to its developer.** No analytics, no telemetry, no crash upload, no
"anonymous usage statistics", and no endpoint of mine exists for it to send them to. This document
is the whole account of what the app stores, reads and reaches, written to be checkable rather than
reassuring — every claim names the file that implements it.

*Applies to version 0.5.0. If you are reading this in the repository it describes `main`; the release
notes say when it changed.*

---

## The short version

| | |
|---|---|
| Sent to the developer | **Nothing.** |
| Sent to Plex | What signing in to Plex requires, and what playing a file requires. |
| Sent to your server | The requests any Plex client makes. |
| Stored on the TV | Your session token, where you were last, and three log files. |
| Third-party analytics | **None.** There is no SDK for one in the binary. |
| Advertising identifiers | **Never read.** LG's `LGUDID` is not called. |

There is no account with me, because there is no *me* to have an account with — no server, no
database, no mailing list.

---

## What leaves the television

**`plex.tv` and `discover.provider.plex.tv`, over TLS.** Signing in, refreshing your server list, and
looking up cast members. Plex sees what its own API is told: a client identifier this install
generates once, the product name and version, and your account's activity. Plex's handling of that is
governed by [Plex's privacy policy](https://www.plex.tv/about/privacy-legal/privacy-preferences/),
not by this one — I am a third-party client and I receive none of it.

**Your Plex Media Servers**, at a LAN address where one answers and a public one otherwise. Browsing,
playback, and the progress reports that make "resume where you left off" work. Your server is yours.

**Nothing else.** `ci/check-elf.sh` and the per-release audit under `docs/release-audits/` measure the
outbound hostnames in the shipped binary, so this is a property of the bytes rather than a promise in
prose.

---

## What is stored on the television

**Your session** — `<app id>-auth.json` under `/media/developer` or `/media/internal`, created 0600
through `open(2)`'s own mode argument (`plex/session.rs`). It holds one access token per server your
account can reach, your profile list, and a randomly generated client identifier. Signing out deletes
it and the identifier is regenerated next time.

**Where you were** — `<app id>-lastplace.json` beside it: the page, your profile id, one server id and
one item id, so the app reopens where you left it.

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

## If telemetry is ever added

It has been designed and not built. Recording the terms here in advance, so that a future version can
be held to them rather than announcing them alongside the thing itself:

1. **Opt-in, off by default, and never bundled.** Crash reports and usage statistics would be two
   independent switches, because they are two different questions.
2. **Nothing stored to enable it before you say yes.** No identifier is generated until you opt in.
3. **You can read the payload on screen before it is sent**, in full, as it would be transmitted.
4. **Compiled out entirely** unless the build asks for it, so a binary without it does not carry a
   dormant SDK — you can check with `strings`.
5. **Never**: media titles, ratingKeys, search terms, subtitle text, server names or addresses, your
   Plex account, or anything derived from the MAC address or serial number.
6. The literal structure sent would be documented **in this file**, field by field, before it ships.
7. **Turning it off would stop collection and discard anything not yet sent — but what had already
   been sent would age out on a retention clock rather than being erased on request.** Stated here
   because it is a limitation rather than a choice: the candidate services can only delete data that
   belongs to an *account*, and the entire design is that these reports belong to nobody. That is the
   right trade — an identifier that made erasure possible would be an identifier worth having in the
   first place — but it is not the same as deletion and would not be described as if it were.

None of that exists today. `make check` builds no telemetry code and there is no endpoint.

### The schema, as it stands

Term 6 says the structure would be documented here before it ships, so here it is as it is being
built — **no sender exists, nothing is stored, and nothing leaves the television**. It is written
down now precisely because a document produced alongside a working uploader is a document nobody
had to live with.

| event | fields |
|---|---|
| `app.launch` | *(none)* |
| `route.entered` | `screen` — one of a fixed list of screen names |
| `signin.completed` | *(none)* |

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

There is no data of yours in my possession, so there is nothing to request, correct or delete — the
files above are on your own television and uninstalling removes them. If that ever changes this
document changes with it, in the same commit.

**Questions:** open an issue at <https://github.com/GLinnik21/plx-native/issues>, or e-mail
glinnik21@gmail.com. Security reports go through [SECURITY.md](SECURITY.md) instead.

*PlxNative is an independent application. It is not produced by, endorsed by, or affiliated with
Plex, Inc. or LG Electronics Inc.*
