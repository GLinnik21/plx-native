# PlxNative Privacy Policy

Applies to PlxNative 0.5.0. Last updated 3 September 2026.

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

Those lifetimes differ. Signing out removes the sign-in, the servers registered with it and their
tokens. Your optional-reporting answers and the two identifiers are kept apart from the sign-in
and are **not** removed by signing out, so that a decision you have already made is not put to
you again. A queued report is deleted once sent, or at the moment you switch its category off. The log
rotates continuously. **webOS gives an application no way to run code as it is removed**, so the
sign-in and the reporting answers can survive an uninstall — use Delete all local data before
uninstalling if you want nothing of PlxNative left on the television.

## Optional crash reports

Crash reporting is off until you choose to share it. If enabled, PlxNative sends technical crash
details to Sentry in Germany. A report may include the signal, code addresses, thread information,
internal component labels, app and webOS versions, television model and hardware compatibility
details needed to reproduce and symbolicate the failure.

Every crash and error report carries a **Crash report ID**: a random identifier created on this
television when you turn crash reports on, sent as the report's `user.id`. It exists so that
reports from one television are counted once rather than once per crash — Sentry's "users
affected" figure is the number of distinct Crash report IDs an issue has reached — which is what
tells a problem that hit many televisions apart from one television that hit it many times. It is
not derived from your Plex account, your television or anything about you, and it is never sent
with product analytics. Settings shows it while crash reports are on. Turning crash reports off
deletes the local identifier; enabling them later creates a new one. Reports already sent keep
the old identifier, so copy it down first if you intend to ask for their deletion.

The same independent choice also covers a handled playback-error report when playback reaches its
explicit terminal error screen. That report contains a fixed failure kind, delivery and quality
classes, coarse raster, rate, HTTP and buffer classes, whether a first picture appeared, and at
most 32 typed playback transitions with bucketed elapsed times. It contains no title, ratingKey,
URL, path, playhead, duration, exact bitrate, server identity, address, token, account or profile,
and is not joined to the product analytics identifier or `playback_id`. It carries the same Crash
report ID as a crash report. Buffering, seeking, holding
a low quality, or rejecting an adaptive-bitrate candidate does not by itself send a report.
The closed diagnostic vocabulary includes terminal kinds such as `playback_interrupted` and
`original_rollback`; HLS direction `refresh`; delivery reason `original_open_rollback`; and
Original-check outcomes `started`, `succeeded`, `no_body`, `deadline`, `transport`,
`inconclusive`, `server_state` and `refused`.

## Optional product analytics

Product analytics is a separate choice and is off until you choose to share it. If enabled,
PlxNative sends typed screen and feature events and broad sign-in and playback outcome classes to
PostHog in Germany. Reports use a random installation identifier and may include the app version,
webOS version, television model and SoC, and whether the selected server is local, remote or
relayed. Turning product analytics off deletes the local identifier; enabling it later creates a
new one.

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
| `playback.failed` | `playback_id` — a random number minted per attempt, never stored and never reused; `mode` — `direct` or `transcode`; `kind` — `decision_refused` / `no_video_transcode_target` / `no_video_track` / `media_source` / `playback_interrupted` / `tv_pipeline` / `original_rollback` / `unspecified` |
| `playback.cancelled` | `playback_id` — a random number minted per attempt, never stored and never reused; `mode` — `direct` or `transcode` |
| `playback.abandoned` | `playback_id` — a random number minted per attempt, never stored and never reused; `mode` — `direct` or `transcode` |
| `playback.quality` | `playback_id` — a random number minted per attempt, never stored and never reused; `rebuffers` — `0` / `1` / `2-3` / `4+`; `buffering` — `none` / `<2s` / `2-10s` / `10s+` — never the interval |
| `playback.ended` | `playback_id` — a random number minted per attempt, never stored and never reused; `mode` — `direct` or `transcode`; `watched` — `abandoned` / `some` / `most` / `finished` — never a position or a duration |

## Never included in optional reports

Optional reports have no fields for media titles, Plex accounts or profile names, searches, server
names or addresses, access tokens, subtitle text, or exact viewing history.

## Your choices

Crash reports and product analytics are independent. You can enable either, both or neither during
setup, and change either choice later in Settings → Privacy & data. Withdrawing a choice stops new
reports of that category, removes queued records that are no longer permitted, and deletes that
category's identifier from this television.

To ask what a category holds for your installation, or to have it deleted, write to the contact
below and quote the identifier Settings shows for that category — the Crash report ID for crash
and error reports, the Analytics ID for product analytics. Each identifier is the only handle its
reports carry, so a request without it cannot be matched to anything.

## Contact and non-affiliation

Privacy questions may be sent to `support@plxnative.com`. Security vulnerabilities may be reported
privately through GitHub Security Advisories for `GLinnik21/plx-native`.

PlxNative is an independent, unofficial application. It is not produced by, endorsed by, or
affiliated with Plex, Inc. or LG Electronics Inc.
