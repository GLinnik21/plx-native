# PlxNative Privacy Policy

Applies to PlxNative 0.6.3. Last updated 11 September 2026.

## Who is responsible for PlxNative data

Gleb Linnik is responsible only for data PlxNative stores locally and for optional reports you
choose to share. Contact: `support@plxnative.com`.

## Plex services

PlxNative is an independent client for Plex. To sign you in, discover servers and provide Plex
account features, the app communicates directly with Plex services. Plex processes information
received by those services under [Plex’s own Privacy
Policy](https://www.plex.tv/about/privacy-legal/). PlxNative’s developer does not receive that
information.

## Plex Media Servers

To browse and play media, update watch progress and use server features, PlxNative communicates
directly with the Plex Media Servers you select. Those requests are handled by the selected server
and its operator. PlxNative’s developer does not receive them.

## Data stored on this television

PlxNative stores your Plex account token and a separate token for each server you use, the
addresses and identifiers of those servers, the profile you selected together with the profile
names and pictures on your account, your Home library choices, your recent searches, your playback
quality preference, and a small rotating local log. It also stores your answers to the two
optional-reporting questions, the random Crash report ID if you turned crash reports on, the
random Analytics ID if you turned product analytics on, any report waiting to be sent, and a
marker recording how much of the crash log has already been read.
It keeps no bookmark of its own for where you stopped watching: playback position is held by your
Plex Media Server. The Settings screen can sign out and remove PlxNative data from this television.

Your sign-in is protected with this television's own key service when one is available and this
install has shown it can be trusted: the app checks, on a later launch, that the television's key
service can still open something it sealed before, and only after that check has passed does it
seal your actual sign-in with it — until then the sign-in is kept in an owner-only file that only
PlxNative can read, exactly as it is on a television with no such service at all. Two small,
content-only markers on disk record the outcome of that check so it is not repeated forever: one
records that the key service has been shown to work on this install (`secure-storage.proven`), the
other that it has been shown NOT to (`secure-storage.refused`). Neither carries key material.

If that key service later stops answering, your sealed sign-in is left exactly as it is rather
than being replaced: the sign-in screen says so and offers to try again, and only signing in again
replaces what is stored. A third content-only marker (`secure-storage.unavailable`) counts how
many launches in a row have gone unanswered, so a key service that never comes back settles rather
than asking forever; it is removed as soon as one launch reads the sign-in successfully, and it too
carries no key material.

Those lifetimes differ. Signing out removes the sign-in, the servers registered with it and their
tokens — and with them your optional-reporting answers, both identifiers and any queued report,
because those choices were made by the person who signed in and say nothing about whoever signs
in next: the next sign-in is asked afresh. Switching between the profiles of one Plex account is
not a sign-out and keeps them. A queued report is deleted once sent. Switching a category off
deletes that category's own queued reports; signing out, or Delete all local data, destroys
everything queued, including a one-off report you already pressed "Send" for — a one-off report
belongs to no category, so only those two erase it. The log rotates continuously. **webOS gives an application no way to run code as it is removed**, so the
sign-in and the reporting answers can survive an uninstall — use Delete all local data before
uninstalling if you want nothing of PlxNative left on the television.

## Optional crash reports

Crash reporting is off until you choose to share it. If enabled, PlxNative sends technical crash
details to Sentry in Germany. A report may include the signal, code addresses, thread information,
internal component labels, app and webOS versions, television model and hardware compatibility
details needed to reproduce and symbolicate the failure.

Every crash report and every automatic error report carries a **Crash report ID**: a random
identifier created on this television when you turn crash reports on, sent as the report's
`user.id`. The one-off sign-in report described below is the one exception and carries no `user`
field at all. It exists so that
repeated crashes under one Crash report ID are counted once rather than once each — Sentry's
"users affected" figure is the number of distinct Crash report IDs an issue has reached — which is
what tells a problem that hit many people apart from one television that hit it many times. It is not derived from your Plex
account, your television or anything about you, and it is never sent with product analytics.
Settings shows it while crash reports are on. Turning crash reports off, or signing out, deletes
the local identifier; enabling them later creates a new one. Reports already sent keep the old
identifier, so copy it down first if you intend to ask for their deletion.

The same independent choice also covers a handled playback-error report when playback reaches its
explicit terminal error screen. That report contains a fixed failure kind, delivery and quality
classes, coarse raster, rate, HTTP and buffer classes, whether a first picture appeared, and at
most 32 typed playback transitions with bucketed elapsed times. It contains no title, ratingKey,
URL, path, playhead, duration, exact bitrate, server identity, address, token, account or profile,
and is not joined to the product analytics identifier or `playback_id`. It carries the same Crash
report ID as a crash report, and the same television model, SoC, hardware revision, webOS release
and the `rtkmem`/`install` sandbox facts a crash report carries. Buffering, seeking, holding
a low quality, or rejecting an adaptive-bitrate candidate does not by itself send a report.
The closed diagnostic vocabulary includes terminal kinds such as `playback_interrupted` and
`original_rollback`; HLS direction `refresh`; delivery reason `original_open_rollback`; and
Original-check outcomes `started`, `succeeded`, `no_body`, `deadline`, `transport`,
`inconclusive`, `server_state` and `refused`.

The same independent choice also covers a handled sign-in error report when a sign-in attempt
fails, sent automatically the same way a crash report is. That report contains which stage failed
(`pin_create`, `authorization`, `discovery` or `other`), a coarse class of what the last attempt to
reach plex.tv actually did (an HTTP status range such as `answered_4xx`, or a transport class such
as `dns`, `tls`, `timeout` or `transport_other`), that exact HTTP status or curl return code as a
bare number, a bucketed count of consecutive unanswered attempts, a bucketed duration of how long
the attempt had been failing, which automatic code the flow was on, and how your sign-in is
protected on this television right now (`none` / `plaintext` / `secure` / `secure_locked` /
`secure_refused` / `secure_unavailable` / `unknown`). It contains no PIN, sign-in code, token, account, URL, hostname or
address, and carries the same Crash report ID as a crash report, and the same television model,
SoC, hardware revision, webOS release and the `rtkmem`/`install` sandbox facts a crash report
carries.

The same independent choice also covers a handled storage error report when this television's
attempt to seal or open your saved sign-in fails, or the file itself cannot be written at all. That
report contains which step failed
(`generate_key`, `begin_encrypt`, `finish_encrypt`, `begin_decrypt`, `finish_decrypt`,
`roundtrip_mismatch`, `envelope_unparseable`, `envelope_locked`, `no_reply`, `unreachable`,
`write_failed`, `untrusted_mode` — this television found the saved sign-in file writable by
another app on the device, so rather than trust its contents it stopped using them and set the
file aside unread, under the same name with `.untrusted` on the end, which signing out or Delete
all local data removes — or
`identity_unavailable` — the saved sign-in records which system-bus identity protected it, and
this launch could not register as that one, so nothing was decided about the key — your saved
sign-in is left as it is, unless this happened while checking an earlier install for a stale probe
file, in which case that unrelated probe file is removed), the numeric error code the key
service replied with when one was reached, how the session is protected right now (`none` /
`plaintext` / `secure` / `secure_locked` / `secure_refused` / `secure_unavailable` / `unknown`), and whether this install has already
recorded that its key service is refused. When it is known, it also says whether the device key
this television's most recent attempt to protect your sign-in used already `existed` or was newly
`created` — the fact that tells apart a key service that is simply unavailable from one that made a
DIFFERENT key than the one an earlier attempt on this television used — and always says how
that attempt identified itself to the key service, as two Yes/No facts: whether it used an
application identity, and whether it instead used a fixed name on the system bus. Either is `Yes`
only where this television's system bus grants it, never both at once, and which of them (if
either) this app gets is what decides whether a key sealed on one launch is still this app's on
the next; a television that grants neither is `No` to both. Separately from those two facts about
this launch, the report also says which identity protected the saved sign-in (or the probe file)
the report is actually about — `app_id`, `named`, `anonymous`, or `none` where the report is about
nothing protected at all — because the whole question is whether the two differ. It contains no key material, ciphertext, plaintext or
file path, and carries the same Crash report ID as a crash report, and the same television model,
SoC, hardware revision, webOS release and the `rtkmem`/`install` sandbox facts a crash report
carries. It is sent only once you have
answered the crash-reports question Yes; a report found before that question is answered (which
can happen on the very first launch after an update) waits in memory, dated to when it actually
happened, for the rest of that one launch only — it is discarded, never sent, if you answer No or
if the app closes before you answer.

Separately, **whether or not crash reporting is on**, the sign-in screen can offer to send a
**one-off report** about a specific sign-in problem — sent only if you explicitly press "Send
report" on the screen where it is offered. It has the same shape as the automatic report above but
carries **no identifier that persists between reports or identifies you or this television** — not the Crash report ID, not the Analytics ID. It
is not a reporting decision and does not turn anything on: nothing is recorded about the press
itself, and no later change to either optional-reporting switch withdraws a report already sent
this way. Every report of either kind is tagged `standing` (the automatic form, gated on crash
reports being on) or `one_off` (the explicit press, gated on nothing) so the two are never
confused.

## Optional product analytics

Product analytics is a separate choice and is off until you choose to share it. If enabled,
PlxNative sends typed screen and feature events and broad sign-in and playback outcome classes to
PostHog in Germany. Reports carry a random Analytics ID created when you turn product analytics on
and may include the app version, webOS version, television model and SoC, whether the selected
server is local, remote or relayed, and how the saved sign-in is stored on this television. Turning
product analytics off, or signing out, deletes the local identifier; enabling it later creates a new
one.

The Settings screen shows field-by-field example payloads produced through the same serializers
used for real reports.

Every product analytics event also carries this bounded compatibility and connection context:

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
| `rtkmem` | `ok` / `missing` / `n/a` — the k5lp/k3lp `/dev/rtkmem` jail pre-flight |
| `install` | `devmode` / `homebrew` / `unknown` — never the install path |
| `session_storage` | `none` / `plaintext` / `secure` / `secure_locked` / `secure_refused` / `secure_unavailable` / `unknown` — how the saved sign-in is protected on this television, never key material, ciphertext or plaintext |

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
| `playback.failed` | `playback_id` — a random number minted per attempt, never stored and never reused; `mode` — `direct` or `transcode`; `kind` — `decision_refused` / `no_video_transcode_target` / `no_video_track` / `media_source` / `playback_interrupted` / `tv_pipeline` / `original_rollback` / `jail_missing_rtkmem` / `load_timeout` / `unspecified` |
| `playback.cancelled` | `playback_id` — a random number minted per attempt, never stored and never reused; `mode` — `direct` or `transcode` |
| `playback.abandoned` | `playback_id` — a random number minted per attempt, never stored and never reused; `mode` — `direct` or `transcode` |
| `playback.quality` | `playback_id` — a random number minted per attempt, never stored and never reused; `rebuffers` — `0` / `1` / `2-3` / `4+`; `buffering` — `none` / `<2s` / `2-10s` / `10s+` — never the interval |
| `playback.ended` | `playback_id` — a random number minted per attempt, never stored and never reused; `mode` — `direct` or `transcode`; `watched` — `abandoned` / `some` / `most` / `finished` — never a position or a duration |

## Never included in optional reports

Optional reports have no fields for media titles, Plex accounts or profile names, searches, server
names or addresses, access tokens, subtitle text, key material, ciphertext, plaintext, file paths,
or exact viewing history.

## Your choices

Crash reports and product analytics are independent. You can enable either, both or neither during
setup, and change either choice later in Settings → Privacy & data. Withdrawing a choice stops new
reports of that category, removes queued records that are no longer permitted, and deletes that
category's identifier from this television. One report that the sender had already picked up at
the moment you withdraw a category may still be sent; no further report of that category is picked
up after it. Signing out, or Delete all local data, is a harder stop: it purges everything queued
for either category at once, including a one-off report, so nothing further goes out from either
path.

When this notice changes, whether you are asked again depends on what changed. A wording-only
revision — clearer language describing the same data and the same purpose — is never a new
question. A new field that is derivable from a field already described here — a raster figure
beside a width and height you were already told about, say — is checked against the answer you
already gave rather than opening a new question. A wider purpose, or a new field with data
materially different from what you were told, is a new question for the change alone, asked
before PlxNative starts collecting it. That question is never both categories at once: it names
only the category that actually grew, and if you had the other one off, or never turned this one
on to begin with, it is left exactly as it was.

**Declining an expansion is not the same as switching a category off.** If you already have a
category on and a later update grows what it collects, you are asked only about the new part — and
saying no there keeps the category on exactly as you already agreed to it, sends nothing of the new
part, and does not ask again until it grows further still. Turning a category off entirely is a
separate action, done any time in Settings → Privacy & data, which is the paragraph above. The
record of what you accepted, and what you last declined, is kept, so a later expansion is always
checked against the real answer you gave — not against a guess, and not against an earlier no that
covered a smaller change.

To ask what a category holds for your installation, or to have it deleted, write to the contact
below and quote the identifier Settings shows for that category — the Crash report ID for crash
and error reports, the Analytics ID for product analytics. Each identifier is the only handle its
reports carry, so a request without it cannot be matched to anything — and a one-off sign-in
report carries no handle at all, so it cannot be looked up or deleted on request; that is the
trade the no-identifier guarantee makes.

## Contact and non-affiliation

Privacy questions may be sent to `support@plxnative.com`. Security vulnerabilities may be reported
privately through GitHub Security Advisories for `GLinnik21/plx-native`.

PlxNative is an independent, unofficial application. It is not produced by, endorsed by, or
affiliated with Plex, Inc. or LG Electronics Inc.
