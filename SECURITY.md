# Security policy

## Reporting a vulnerability

Report privately, **not** as a public issue, through **GitHub Security Advisories**:
<https://github.com/tiper/fin-native/security/advisories/new>. It is private, it threads, and it
produces a CVE if one is warranted.

FinNative is a one-person unpaid project, so reports are answered as soon as I can. If you have not
heard back in a couple of weeks, open a public issue saying only *"sent a security report on
<date>, no reply"*, with no details.

Please give me a reasonable window to ship a fix before disclosing. There is no bounty; I will credit
you in the release note unless you ask me not to.

**Report FinNative problems here, not to PlxNative.** FinNative is built on
[PlxNative](https://github.com/GLinnik21/plx-native); a vulnerability that is in PlxNative's own code
as well is passed on to its maintainer once it is understood.

## What is in scope

The app, its packaging, and the host-side tools in `tools/` and `ci/`. Much of what follows is the
PlxNative engine FinNative runs on, and it names Plex where that engine does; in FinNative the one
server is a Jellyfin server, and its access token is held to the same rules. Concretely, the things
worth looking at:

- **The `/tmp` trigger surface.** `/tmp` is mode 1777 in webOS's production jail, so any co-resident
  process can create files there. Roughly forty `plxnative-*` files change behaviour, and three are
  outright takeovers — `plxnative-token` beats the signed-in session, `plxnative-servers` injects a
  server and its token, `plxnative-url` replaces the stream. **All of it is compiled out of a
  release build** by dropping the `devtriggers` cargo feature, and `ci/check-elf.sh` measures that
  on the shipped bytes rather than asserting it. A release binary that still carries any of it is a
  valid report, and a serious one.
- **The event log.** `plxnative-events.log` — like `plxnative-crash.log` and `plxnative-stderr.log`
  beside it — is created 0640 (owner read/write, the app's own group read, never world-readable),
  so that a Developer Mode user can fetch it with webOS Dev Manager on a television that is not
  rooted. Every event-log line goes through `eventlog::scrub::scrub_local` before the write. A line
  that reaches it carrying a credential, a Plex token, a `plex.direct` hostname, a household name or
  anything about what is being watched is a valid report — see [PRIVACY.md](PRIVACY.md) for the
  contract that is meant to hold. The panic text the Rust panic hook appends to the crash log goes
  through the same scrubber. All three files are opened with `O_NOFOLLOW` as a regular file this app
  owns with exactly one link, so a symlink or a hard link planted at their names in the shared
  `/tmp` is refused and never chmod'ed or truncated. Stated plainly, the residual exposure: another
  native app on the same television that runs in the app's group can read all three files, and the
  stderr log (whatever aborts and the television's own libraries print), the crash log's C-side
  records (a faulting address, registers, the crash's wall-clock second, and the memory-map lines
  holding the faulting pc, the lr and our own binary) and the C-side lines the shim and
  the Starfish seam write into the event log are not passed through that scrubber. A credential in
  any of them is a valid report too.
- **TLS.** Certificate verification is on for every HTTPS request, with one bounded exception and
  one fallback that relaxes nothing. The exception: when a Plex server's certificate fails only its
  validity-date check (the television has no battery-backed clock) and a public key was remembered
  for that exact host and port from an earlier fully verified connection, the request is repeated
  with the chain-and-date check replaced by a pin on that key; the name check stays on. plex.tv,
  telemetry and any host with no remembered key never take this path (`net.rs`, `net::keypin`).
  The fallback: when a `*.plex.direct` server's certificate fails because the television's own
  trust store lacks its issuer (Let's Encrypt's 2025 roots on a 2020 firmware), the request is
  repeated once with the CA file set to `le-roots.pem`, the four public ISRG roots shipped in the
  package. Chain, dates and name are all still verified, against a different root set. It is
  offered only for a host name that is a `*.plex.direct` name (a URL whose userinfo merely names
  one does not count), never for plex.tv or telemetry. A redirect is a request of its own: it is
  verified against the television's own store like any other, and the bundle is offered for it only
  if its target is itself a `*.plex.direct` name that fails the same way.
  When the bundle verifies the chain and only the dates fail (a wrong clock too), the remembered
  key above is still tried. Stable builds refuse
  any PMS control or media URL that would carry a Plex token over plaintext HTTP, with one
  consented exception: a server that answers only unencrypted at a numeric private address on the
  television's own network, where the person answered "Connect without encryption?" for that
  server (`plex::grant`). The grant names that server and that exact origin, is never persisted,
  and is revoked on sign-in, sign-out and return to the foreground, at a profile switch whose
  roster does not install that server at that origin, whenever a fresh probe no longer reaches it,
  and at once when the person says no (a refusal is written again until it reaches the disk). A
  token sent over plaintext to any origin outside a live grant, or to a server other than the one
  it names, is in scope. A Jellyfin server's token goes out under the same rule: only over TLS, or
  under a live grant for exactly that plaintext origin (`jellyfin::client`).
  Only an explicit developer-trigger build can otherwise allow the lab path, and it logs the
  exception without the URL.
  Anything that disables, downgrades or bypasses these rules is in scope — including a key-mode
  request that accepts a different key or a wrong name, or a bundled-roots request that accepts a
  certificate those roots do not verify or is offered for a host that is not a `*.plex.direct` name;
  so is any path where a failure to *set* a security option results in a request going out anyway.
- **The session file.** `<id>-auth.json` holds one access token per server your account can reach.
  It is encrypted with the firmware's authenticated Key Manager where
  `com.webos.service.keymanager3` is available and permitted, with a 0600 plaintext compatibility
  fallback otherwise. The legacy `com.palm.keymanager` AES-CFB interface is not used because it
  provides no authenticated-encryption operation. The file is always created 0600 through
  `open(2)`'s own mode argument. A downgrade of an existing encrypted file, a way to read it from
  another process, or a way to make the app write it somewhere world-readable is in scope.
- **The bundled FFmpeg.** Built from unmodified FFmpeg 9.0 with demuxers, parsers, subtitle
  decoders, and the audio decoders and AC3 encoder of Settings > Playback > Convert audio — it is
  fed untrusted bytes from the network, so parser and decoder bugs reachable through `ff.rs` and
  `ff_audio.rs` are in scope. Report FFmpeg's own bugs upstream as well.

## What is not in scope

- Post-compromise access by an attacker who already has root on the television. Root is not an app
  prerequisite; a report whose only precondition is an already-rooted OS describes a platform
  compromise rather than an app sandbox escape.
- The webosbrew Homebrew Channel, webOS itself, LG's own libraries, Jellyfin Server or Plex Media
  Server. Report those to their maintainers.
- Missing hardening that costs nothing to an attacker who is already executing code in the app's
  jail, unless you can show a concrete consequence.

## What this app does not have

No account of its own, no server, no payment path, and no user-generated content. It signs in to
**your** Jellyfin server and talks only to it.

**No telemetry.** The reporting code FinNative inherits from PlxNative (Sentry crash reports and
PostHog product analytics) sends nothing unless a build carries a Sentry DSN and a PostHog key, and
builds made from this repository carry neither, so FinNative never asks about reports. A FinNative
build from this repository that sends anything anywhere except your Jellyfin server is in scope,
and so is a consent prompt appearing in one. [PRIVACY.md](PRIVACY.md) is the full account.
