# PlxNative Privacy Policy

Applies to PlxNative 0.6.0. Last updated 4 September 2026.

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

PlxNative has a developer-only input/frame recorder used to reproduce and test bugs. It is not
part of the app you installed: a release build has the developer-trigger inputs, including the
recorder, compiled out — there is no code path in a release binary that can open, write or read a
recording, on this television or off it. The unconditional `plxnative-*.log` files are create-only
diagnostics, not trigger inputs. The recorder exists only in development builds used to build and
test PlxNative itself, is started only by explicitly arming it on that build, and its recordings
never leave the device it was made on.

PlxNative stores your Plex account token and a separate token for each server you use. For every
profile on your account you have switched to on this television, it also keeps that profile's own
server access token(s), so a later switch still works with no internet, and, for a PIN-protected
profile, a one-way check computed from that PIN rather than the PIN itself. It also keeps the
addresses and identifiers of those servers, the profile names and pictures on your account —
pictures are also cached as files so the profile picker still shows faces with no internet — your
Home library choices, your recent searches, your playback quality preference, and local technical
logs: a small rotating event log and a bounded storage status snapshot. It also stores your answers
to the two optional-reporting questions, the random Crash report ID if you turned crash reports on,
the random Analytics ID if you turned product analytics on, any report waiting to be sent, a
marker recording how much of the crash log has already been read, and — for a server you were
asked about — whether you allowed it to be reached without encryption on your home network (the
answer only, for the Plex account that gave it: the permission itself is never stored, and ends
when a fresh check of the server no longer reaches the same address, at a sign-in, sign-out or
profile switch, or when the app returns from the background).
It keeps no bookmark of its own for where you stopped watching: playback position is held by your
Plex Media Server. The Settings screen can sign out and remove PlxNative data from this television.

Those lifetimes differ. Signing out removes the sign-in, the servers registered with it and their
tokens, every profile's own cached server access token(s) and PIN check, the cached profile
pictures, your answers about unencrypted connections — and with them your optional-reporting
answers, both identifiers and any queued report,
because those choices were made by the person who signed in and say nothing about whoever signs
in next: the next sign-in is asked afresh. Switching between the profiles of one Plex account is
not a sign-out and keeps all of it, including the server access token(s) and PIN check cached for
a profile you are not currently using, so that profile can be switched to again with no internet.
A queued report is deleted once sent, or at the moment you switch
its category off or sign out. The event log rotates continuously and the storage snapshot is
replaced when its bounded status changes. **webOS gives an application no way to run code as it
is removed**, so the sign-in and the reporting answers can survive an uninstall — use Delete all
local data before uninstalling if you want nothing of PlxNative left on the television.

## Optional crash reports

Crash reporting is off until you choose to share it. If enabled, PlxNative sends technical crash
details to Sentry in Germany. A report may include the signal, code addresses, thread information,
internal component labels, app and webOS versions, television model and hardware compatibility
details needed to reproduce and symbolicate the failure.

A native crash report may also contain the last 16 window diagnostic steps: entering or leaving
background, querying the native window, and beginning or completing the first frame after return.
These steps include technical timestamps, fixed labels, whether playback was active, whether display/surface handles were
available, and SDL's version numbers. They contain no window addresses or viewing content.
They are collected only while crash reporting is enabled and accompany a crash rather than being
sent as usage events. Disabling crash reports removes the local trace.

Every crash and error report carries a **Crash report ID**: a random identifier created on this
television when you turn crash reports on, sent as the report's `user.id`. It exists so that
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

The same choice also covers a **sign-in problem report** when signing in fails. It contains the
`kind` of sign-in step that failed — or, when the failure was inside the app rather than on the
network (the app's own sign-in work refused, stopped or left unfinished), which of those fixed
internal kinds it was — the connection's `link` class, an `http_status` or network error number
(`curl_rc`) when there is one, how long the connection went `unanswered` and how long sign-in had
been `failing_for` (both bucketed), how many codes were shown (`code_generation`, capped), and, when
a sign-in could not be saved, a `persistence` failure class, the `keymanager_stage` a key-service
step stopped at and the key service's own numeric `service_error_code`. A storage-helper failure
also carries fixed startup, connection, activation or backend stages, wire/DB8 error codes, and
up to eight failed storage-candidate errno numbers. It carries no candidate paths, file owners or helper generation identifiers.
When a server was found but answered only over an unencrypted connection, it also carries a fixed
outcome class (`absent`, `timeout`, `dns`, `tls`, `refused` and the like) for each secure route to
that server — `https_lan`, `https_public`, `https_custom` and `https_relay` — and, about the
unencrypted answer, only fixed facts: whether plex.tv marked that connection local
(`plaintext_local`), whether plex.tv saw this television behind the server's own network address
(`public_address_matches`), whether the server is `owned` by the signed-in account, whether it
requires secure connections (`https_required`), the kind of address it was (`plaintext_scope`:
private, link-local, unique-local, loopback, public or a name) and its family
(`plaintext_family`: v4, v6 or unknown), whether the app could offer to connect without encryption
on your home network or the fixed reason it could not (`plaintext_eligibility`: eligible,
https_required, not_local, not_same_network, not_private_address, identity_unverified,
https_unsettled or https_answered) and, when it asked, what became of the question
(`plaintext_consent`: offered, accepted, declined or revoked) — never the address itself, the
server or whose it is. When the account has no
server, it carries how many other devices plex.tv listed (`resources`, bucketed) and whether that
happened right after signing in or on a retry (`discovery_trigger`).
It also carries whether the report was `consent`ed to as a standing choice or as a one-off, the app version and when it
happened. It never includes your account name, tokens, PIN, sign-in code or network addresses. With
crash reports on, it is sent automatically, carries the Crash report ID, and the sign-in screen
shows the random **Report ID** of that one report. With them off, or not yet decided, the sign-in
screen can offer **Send report** instead: that sends one report, only when you press it, with no
Crash report ID and with a random **Report ID** shown on screen that identifies only that one
report — not you or this television. A stalled wait is only ever reported that way, never
automatically.

## Optional product analytics

Product analytics is a separate choice and is off until you choose to share it. If enabled,
PlxNative sends typed screen and feature events and broad sign-in and playback outcome classes to
PostHog in Germany. Reports carry a random Analytics ID created when you turn product analytics on
and may include the app version, webOS version, television model and SoC, and whether the selected
server is local, remote or relayed. Turning product analytics off, or signing out, deletes the
local identifier; enabling it later creates a new one.

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
| `server_connection` | `local` / `remote` / `relay` / `unknown` — omitted entirely on an event with no one server, such as `app.launch`, `route.entered` or a `signin.*` event |
| `ip_version` | `v4` / `v6` / `unknown` — omitted entirely on an event with no one server, same as `server_connection` |
| `rtkmem` | `ok` / `missing` / `n/a` — the k5lp/k3lp `/dev/rtkmem` jail pre-flight |
| `install` | `devmode` / `homebrew` / `unknown` — never the install path |

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
names or addresses, access tokens, subtitle text, or exact viewing history.

## Your choices

Crash reports and product analytics are independent. You can enable either, both or neither during
setup, and change either choice later in Settings → Privacy & data. Withdrawing a choice stops new
reports of that category, removes queued records that are no longer permitted, and deletes that
category's identifier from this television. Signing out does the same for both categories at once.
One report that the sender had already picked up at the moment you withdraw or sign out may still
be sent; no further report is picked up after it.

To ask what a category holds for your installation, or to have it deleted, write to the contact
below and quote the identifier Settings shows for that category — the Crash report ID for crash
and error reports, the Analytics ID for product analytics. Each identifier is the only handle its
reports carry, so a request without it cannot be matched to anything. A one-off sign-in problem
report is sent only when you press Send report; quote the Report ID it showed to ask about it.

## Contact and non-affiliation

Privacy questions may be sent to `support@plxnative.com`. Security vulnerabilities may be reported
privately through GitHub Security Advisories for `GLinnik21/plx-native`.

PlxNative is an independent, unofficial application. It is not produced by, endorsed by, or
affiliated with Plex, Inc. or LG Electronics Inc.
