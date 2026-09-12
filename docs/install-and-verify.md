# Installing PlxNative, and checking what you downloaded

This page does not change between releases. A release note tells you what is new in one version; this tells you how to install any of them, how to check that the file you have is the file that was published, and what the app does on your television once it is there.

Per-release facts — the hash, the sizes, the payload, what was tested on which set — are in that version's [technical audit](https://github.com/GLinnik21/plx-native/tree/main/docs/release-audits).

## Which file to download

A release attaches five files. **You need the first one.**

| File | What it is |
|---|---|
| `com.beb.plxnative_X.Y.Z_arm.ipk` | The app. |
| `com.beb.plxnative.manifest.json` | The Homebrew Channel's manifest — how the Channel finds and verifies the update. |
| `ipk.sha256` | The checksum, for `sha256sum -c`. |
| `ffmpeg-9.0.tar.xz` | The pristine upstream FFmpeg source, published because we are obliged to. |
| `build-ffmpeg.sh` | The complete configure invocation that produced the bundled FFmpeg libraries. |

## Installing it

Two routes, and one of them is better:

- **The [Homebrew Channel](https://github.com/webosbrew/webos-homebrew-channel)** — install any `.ipk` you point it at, whether or not it is in the catalogue. **Prefer this.**
- **[dev-manager-desktop](https://github.com/webosbrew/dev-manager-desktop)**, over LG Developer Mode.

**Developer Mode expires.** LG ends a Dev Mode session after about 1000 hours and *uninstalls the apps installed through it* when it does. dev-manager-desktop can renew the session before that happens; the Homebrew Channel has no expiry at all. This is the whole reason for the ranking.

**Normal use does not require a rooted television.** The app runs as an unprivileged uid inside LG's sandbox, with no capabilities and no `requiredPermissions` declared. Some Realtek sets have an incomplete sandbox that blocks native video; the [in-app issue #74 repair](native-video-sandbox.md) requires rooted Homebrew Channel access. For unrooted Developer Mode installs, the [signed-configuration guide](non-root-video-sandbox.md) describes a separate option that still needs confirmation on the affected hardware.

## Checking what you downloaded

Nothing anywhere in this distribution chain is signed — there is no code signing in the webosbrew path at all — so the sha256 in the release note is what tells you the file you have is the file that was published there.

```sh
shasum -a 256 com.beb.plxnative_X.Y.Z_arm.ipk        # macOS, Linux
sha256sum -c ipk.sha256                              # with the checksum asset beside it
certutil -hashfile com.beb.plxnative_X.Y.Z_arm.ipk SHA256   # Windows
```

**If the Homebrew Channel installs the update for you from its catalogue, you have nothing to do.** It fetches that release's `com.beb.plxnative.manifest.json`, hashes the download on the television, and refuses to install a package that does not match. Pointing the Channel at a bare `.ipk` yourself skips that check, so do it yourself.

**On rebuilding it to compare.** Two builds of one commit on one machine produce a byte-identical `.ipk`. It is **not** reproducible across machines yet — the bundled FFmpeg records the toolchain paths it was built against — so a hash from your own rebuild will differ, and that is not tampering. Each audit's `Reproducibility evidence` section shows exactly which paths a given package carries.

Every release is built and uploaded by GitHub Actions from the tag. If a release's assets were uploaded by a person rather than by `github-actions[bot]`, the build and verification gates did not run — the audit records the uploader for exactly this reason.

## What the app does on your television

Invariant across releases. Where a release changes one of these, its note says so and its audit measures it.

**Legacy unknown-format exception:** an unrecognized secure envelope found during migration is kept
for a newer build rather than guessed at or overwritten. Sign out or Delete all local data removes
it deliberately.

**Current persistent state.** Session and reporting consent share one fixed-ID object in a private
DB8 kind owned by the packaged `<appid>.storage` service. The DB8 object contains only
`_id/_kind/_rev` and an opaque typed JSON string. The helper's transient Unix socket is mode 0600
and accepts only the app's jailed UID. The package contains no writable canonical state directory.
On newer/unknown webOS, fresh auth attempts Keymanager3 first and falls back to a private-DB8
ACL-only envelope when crypto fails, with consent-gated storage diagnostics. Ordinary public
settings never downgrade existing healthy ciphertext. The policy selects ACL-only directly for
reported webOS majors 1–4; runtime evidence currently covers webOS 4.10.2 only.

- `/tmp/plxnative-events.log`, `/tmp/plxnative-stderr.log` and `/tmp/plxnative-crash.log` — the first two truncated each launch, the crash log append-only so it survives a restart. Every line is scrubbed **before it is written**: tokens, header and query credentials, hostnames (including the `plex.direct` names that encode your LAN address), bare addresses, Plex GUIDs, search queries and your server and profile names are rewritten, and media titles, search terms and subtitle text are never written at all. What remains is ratingKeys — server-local item numbers, which are what a playback bug is diagnosed from. Someone with access to the same server could map one back to an item, so still think before posting a log publicly. [`PRIVACY.md`](https://github.com/GLinnik21/plx-native/blob/main/PRIVACY.md) is the full contract.
- Legacy `<id>-auth.json`, `state/auth.json`, `state/session.json`, `state/consent.json` and telemetry
  decision files are read only as migration inputs. They are cleaned only after an authenticated
  destination readback.
- The reporting spool and crashmark remain bounded owner-only files. Image data and logs are
  disposable and are not stored in DB8.

### Historical file backend (migration context, not the current write path)

The paragraphs and table below document the file/key-probe backend written by earlier 0.6 builds.
They explain the shapes the migration reader must recognize and the risks of the old shared
Developer Mode namespace. Current television writes do not use these paths or plaintext-fallback
rules.

  **Which identity the key belongs to, and why two televisions answer differently.** LG's key manager keys a key to the LS2 identity of whoever asked for it: the application id when the system bus supplies one, otherwise the name the bus minted for that connection. A client that registers anonymously is therefore a *different owner on every launch*, which is what a sealed session that never reopens looks like from the inside — the file is intact, the key is simply not this launch's to use. PlxNative now asks the bus for a stable identity in a fixed order and falls back to an anonymous registration only when every stable shape is refused: first its own application id declared as an *application service* (the rule's first key), then the same application id taken as a plain *bus name* (the rule's second key). The event log says which it got, as one line — `keymanager: identity=app_id (…)`, `keymanager: identity=named (…)` or `keymanager: identity=anonymous (…)`, the last carrying both refusals so the line says which shape the bus was answering about — and an error report about storage carries the same fact as two fields. On a 2019-era set (webOS 4.x) the answer is always anonymous, deliberately: the video plane on those televisions is bound through LG's `libAcbAPI`, which is handed this app's id at start-up and holds it for as long as the app runs, so claiming that name for the session key would cost you a picture to save a sign-in. **Both stable shapes ask for that same bus name, so both are skipped there** — such a television is settled as anonymous without the bus being asked anything at all. webOS 5.0 removed `libAcbAPI` altogether, so on a newer television nothing else in the app wants the name — the application-id identity is what this build asks for there; whether the hub actually grants it is unmeasured, that gap being exactly the set of televisions where the key manager exists in the first place. The plain-bus-name shape exists because of what the development television actually answered on 2026-09-10: asked at start-up, *before* `libAcbAPI` had taken anything, that television refused the application-service registration with `-1027 Invalid permissions` — for its own application id **and for no name at all** — while a plain anonymous registration was granted and completed a call. A refusal of a request that asks for no name cannot be a name-already-taken verdict, so on that firmware the refusal is about the registration *method*, not about the name — which is what put the shape the television's own permission file does list, the application id as a plain bus name, on the list to try. **It has since been measured: the hub GRANTS it, at boot, on that same television** — but the ACB gate above keeps webOS 4 from ever asking for it once granted, so no television has yet SEALED anything under it. **All of this is a probable cause under test, not an established one:** the ownership rule is read out of LG's own key-manager binary, but no one has yet watched a set without `libAcbAPI` seal a sign-in under its application id or its bus name and reopen it after a power cycle. That is what the identity line in the log, and the same fact on a storage error report, are there to settle.

  **The identity is recorded in the file, not decided again on each launch** — otherwise the fix would be a second source of the same instability. A protected sign-in says which identity protected it, and is only ever reopened through that same one; a file written by an older version says nothing, which means the anonymous identity those versions always used. If a television will not grant that identity on some later launch, nothing has been learned about the key and nothing is decided: the file is kept exactly as it is, no "this television refuses protected storage" verdict is recorded, and the reason travels as `identity_unavailable`. In the same spirit, the record that protected storage has been *proven* to work on your television is a record about one identity — an install proven under the application id has proven nothing about the bus name or the anonymous one, so a launch whose identity differs keeps the mode-0600 file and re-earns the proof for the identity it actually has.

  **If the key service stops answering, the recognized sealed sign-in is preserved until you authorize a fresh QR sign-in.** A timeout, an unreachable service, or an unavailable bus identity does not establish that the key is unusable. The launch reports `secure_unavailable` and offers **Try again**. A newly authorized sign-in may replace the recognized envelope with owner-only plaintext; cached credentials, profile switching, and preference updates do not authorize that replacement. An unproven install stays plaintext until a later launch proves its probe can be reopened; an already-proven install can seal again if the service recovers. Persistence can still fail when no candidate accepts a write, and the save outcome reports that failure. If nobody signs in, three unanswered launches recorded in `secure-storage.unavailable` lead to the refused-storage fallback. `identity_unavailable` never escalates this way, because failure to obtain a bus identity establishes nothing about the key.

  **A saved sign-in another app on the television could have rewritten is set aside, not thrown away.** Every one of PlxNative's own files is created readable and writable by nobody but PlxNative, and one found with that loosened — so that some other app installed on the same television could have changed its contents — is never read as a sign-in: nothing in it is believed, and PlxNative asks you to sign in again. The file itself is renamed beside itself with `.untrusted` on the end, still readable only by PlxNative, and nothing in the app ever opens it again. It is kept because those bytes are the only record of what happened, for you or for whoever you ask about it; signing out, or Delete all local data, removes it along with everything else.

The write itself may still fail outright — a jail-profile directory that looks writable and is not. When it does, PlxNative keeps the sign-in usable for the REST of that run (so the app you are looking at is not silently signed out) but logs the failure and reports it as a `write_failed` storage error; the sign-in will be asked for again on the next launch rather than surviving one it was never actually written to disk for.

**Every persistent file below is app-owned, mode 0600, and stays that way even if something in the
Developer Mode shared namespace widens it** (the `/media/developer` claim is specifically for the
measured Developer Mode profile; this document makes no equivalent claim about retail or Homebrew
jails). A file opened for reading or appending that is still owned by this install and is still a
regular file, but whose mode has grown group/other bits, is repaired to 0600 in place rather than
refused — refusing used to mean a corrupted mode was a silent, permanent outage of the mechanism
reading it. A file that is not this install's own, or not a regular file, is rejected outright,
never parsed as ours.

**Repairing the mode is not the same claim as trusting the content it protected while it was wide open.** A mode widened to add only group/other READ (`0o044`/`0o055`) is a disclosure problem — the bytes are still whatever this install last wrote — and the file loads normally once repaired. A mode widened to add any group/other WRITE bit (`0o022`) means another uid on the shared namespace could have rewritten those bytes, so the content is no longer trustworthy: the session file and the consent decision are never loaded from such a copy (the session falls back to no session / the sign-in screen; consent resets to both categories unanswered), a marker or probe file is ignored and deleted outright (so it cannot keep lying on every later boot either), and the telemetry spool is truncated rather than appended onto. This split is what the table's last column names below.

**What none of this can see: a replay.** `O_NOFOLLOW` on the final `open(2)` defeats a symlink swapped in at the leaf name; it says nothing about who can write to the ENCLOSING directory. The shared-directory observations in this paragraph are specifically for the measured Developer Mode profile: a peer with rename rights there can move this install's own, currently-valid, correctly-owned, correctly-0600 file ASIDE, let a fresh write replace it, and later move the OLD bytes back — same owner, same mode, a shape this build parses fine. **Every check in this table (ownership, regular-file, the trust-vs-repair split above) is powerless against that**, because the replayed file really is this install's own, only stale. This is a known, undetected limitation of that Developer Mode namespace, pinned by `plex::session::tests::a_replayed_older_valid_session_file_is_indistinguishable_from_current` and the consent file's equivalent test. No equivalent retail/Homebrew claim is made here.

| file (search-order candidate names vary; shown relative) | writer | mode at creation | mode enforced on open |
|---|---|---|---|
| `state/` — packaged app-local fallback directory | package metadata | uid 0, gid 5000, 0775 | shipped empty; runtime children are not package data |
| `state/session.json` — canonical signed-in record | `session::save`/`update` via the Record backend | 0600 | canonical temp files use `.session.json.new-*`; payload is never parsed from an untrusted source |
| `state/consent.json` — canonical reporting decision | `telemetry::record` via the Record backend | 0600 | canonical temp files use `.consent.json.new-*` |
| `<prefix>secure-storage.refused` marker | `write_refused_marker` via `write_atomic` | 0600 | repaired to 0600 if widened; content discarded (ignored and deleted) if it was writable |
| `<prefix>secure-storage.proven` marker | `write_proven_marker` via `write_atomic` | 0600 | repaired to 0600 if widened; content discarded (ignored and deleted) if it was writable |
| `<prefix>secure-storage.unavailable` — the unanswered-launch counter | `note_service_unavailable` via `write_atomic` | 0600 | repaired to 0600 if widened; content discarded (ignored and deleted) if it was writable |
| `<prefix>secure-probe.json` | `plant_probe` via `write_atomic` | 0600 | repaired to 0600 if widened; content discarded (ignored and deleted) if it was writable |
| `state/consent.json` — canonical reporting decision | `telemetry::record` via the Record backend | 0600 | canonical temp files use `.consent.json.new-*`; legacy decision files are migration inputs only |
| `<id>-telemetry-spool.bin` — queued reports awaiting a flush | `telemetry::spool::resolve` (first creation) via `write_atomic`; ordinary appends reopen the existing file | 0600 | repaired to 0600 if widened (append and read both); content discarded (truncated, not appended onto) if it was writable |
| `<id>-telemetry-crashmark.json` — how much of the crash log has been reported | `telemetry::crashreport::write_mark` via `write_atomic` | 0600 | ownership + regular-file check, repaired to 0600 if widened |
| `plxnative-events.log` / `plxnative-stderr.log` / `plxnative-crash.log` (runtime root) | `crate::log` / the C crash tracer / the Rust panic hook (`app.rs::install_panic_logger`, `plxnative-crash.log` only) | 0600 | ownership + regular-file check, repaired to 0600 if widened |
| the runtime root itself (`/tmp/<app id>` for a flavoured install) | `paths::ensure_runtime_dir` | 1777, deliberately — see the doc on that function | not a data file; shared by design |

The app does not persist the last screen: an authenticated cold launch starts on Home. Upgrades
remove the retired `<id>-lastplace.json` bookmark written by older builds.

A crash writes no core file.

**What it reads outside its own directory:** the television's own codec table at `/etc/umediaserver/device_codec_capability_config.json`, and its firmware identity at `/var/run/nyx/os_info.json` and `/var/run/nyx/device_info.json`. All three are published by the platform, read once at boot, never written.

**What it reaches:** `plex.tv` and `discover.provider.plex.tv` over TLS, and the Plex Media Servers your account can reach — your own and any shared with you — over HTTPS whenever a token is present. Stable builds refuse token-bearing plaintext HTTP; developer-trigger builds may enable it for a local lab and log that exception. **And, only if you switched it on, Sentry and PostHog in the European Union** — two switches, both off by default, both reversible, described in full in [PRIVACY.md](https://github.com/GLinnik21/plx-native/blob/main/PRIVACY.md). There is a third, narrower door: the sign-in screen's one-off sign-in error report, sent to Sentry only if you explicitly press "Send report", under no switch and carrying no identifier. Nothing is sent anywhere else. A build carries an endpoint only if one was compiled into it, so `strings` on the binary answers the question directly, and each release audit reports what it found there.

**What listens:** nothing. A release build compiles out the whole `/tmp` trigger surface, the remote-control FIFO and the TCP capture listener that exist in a development build. Each audit measures this on the shipped bytes rather than asserting it.

## Scope

Movies and TV shows, from a Plex Media Server your account can reach. No music, no photos, no live TV, no DVR. There is deliberately nowhere on the television to type a server address — configure servers on a phone or a PC, and the app offers what your account already knows about.

## The bundled FFmpeg

The package contains three FFmpeg shared libraries — `libavformat-plx.so.63`, `libavcodec-plx.so.63` and `libavutil-plx.so.61` — built from **FFmpeg 9.0**, unmodified, and licensed **LGPL-2.1-or-later**. Demuxers, parsers, bitstream filters and subtitle decoders only: video and audio are decoded by the television's own hardware.

The complete corresponding source accompanies every release, as LGPL-2.1 §6 requires and not as a courtesy: `ffmpeg-9.0.tar.xz` is the pristine upstream tarball with no patches applied, and `build-ffmpeg.sh` is the complete configure invocation that produced the libraries. It is built with `--disable-everything` plus an explicit component list, and **without** `--enable-gpl`, `--enable-version3` or `--enable-nonfree`, so no GPL or non-free component is present. Each audit quotes the configure string recorded inside `libavutil` itself, which is the primary evidence for that.

They are ordinary shared libraries, `dlopen`ed by absolute path out of the app's own directory under exactly those names, so they can neither shadow nor be shadowed by the television's own FFmpeg — and a build of your own with the same names replaces ours. Full licence text travels inside the package, in `THIRD-PARTY-NOTICES.md` and `licenses/`.

The bundled build is configured `--disable-network` with `file` as its only protocol, so it cannot open a URL at all; everything it demuxes arrives through the app's own transport.
