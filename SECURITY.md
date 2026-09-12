# Security policy

## Reporting a vulnerability

Report privately, **not** as a public issue:

- **GitHub Security Advisories** — <https://github.com/GLinnik21/plx-native/security/advisories/new>
  (preferred: it is private, it threads, and it produces a CVE if one is warranted)
- or e-mail **glinnik21@gmail.com** with `PlxNative security` in the subject.

This is a one-person unpaid project, so the honest service level is: acknowledged within **7 days**,
an assessment within **30**. If you have not heard back in a week, assume the mail was lost and open
a public issue saying only *"sent a security report on <date>, no reply"* — with no details.

Please give me a reasonable window to ship a fix before disclosing. There is no bounty; I will credit
you in the release note unless you ask me not to.

## What is in scope

The app, its packaging, and the host-side tools in `tools/` and `ci/`. Concretely, the things worth
looking at:

- **The native-video sandbox repair.** On the affected Realtek sandbox failure, a viewer can
  explicitly confirm a repair through Homebrew Channel's local `exec` service. The app itself
  stays unprivileged; the existing Homebrew Channel service must already run as root. The
  request runs LG's jailer with its `native` profile for this install only, after validating
  the app id and exact installation path. It accepts no shell command from a server, media
  item, or text input. No repair runs automatically at boot or on a playback attempt, and
  only one repair attempt is admitted per process. A timeout does not prove the remote
  command stopped, so it never automatically retries. Bypassing confirmation, targeting
  another application, or injecting shell syntax into this operation is in scope.
- **The `/tmp` trigger surface.** `/tmp` is mode 1777 in webOS's production jail, so any co-resident
  process can create files there. Roughly forty `plxnative-*` files change behaviour, and three are
  outright takeovers — `plxnative-token` beats the signed-in session, `plxnative-servers` injects a
  server and its token, `plxnative-url` replaces the stream. **All of it is compiled out of a
  release build** by dropping the `devtriggers` cargo feature, and `ci/check-elf.sh` measures that
  on the shipped bytes rather than asserting it. A release binary that still carries any of it is a
  valid report, and a serious one.
- **The event log.** `plxnative-events.log` is created 0600 and every line goes through
  `diag::scrub::scrub_local` before the write. A line that reaches it carrying a credential, a Plex
  token, a `plex.direct` hostname, a household name or anything about what is being watched is a
  valid report — see [PRIVACY.md](PRIVACY.md) for the contract that is meant to hold.
- **TLS.** Certificate verification is on for every HTTPS request (`net.rs`). Stable builds refuse
  any PMS control or media URL that would carry a Plex token over plaintext HTTP; only an explicit
  developer-trigger build can allow that lab path, and it logs the exception without the URL.
  Anything that disables, downgrades or bypasses these rules is in scope; so is any path where a
  failure to *set* a security option results in a request going out anyway.
- **The canonical session record.** A packaged native helper owns one private DB8 kind and one
  fixed-ID object containing typed Session, Consent and operation-ledger state as an opaque JSON
  string. The app/helper socket is mode 0600 and requires matching kernel peer credentials. On new
  and unknown webOS a fresh auth write attempts authenticated Keymanager3 first, then may use an
  explicit private-DB8 ACL-only envelope if crypto fails; that downgrade must emit closed,
  consent-gated diagnostics. Public settings/routine refreshes never downgrade existing healthy
  ciphertext. The policy selects ACL-only directly for reported webOS majors 1–4; runtime evidence
  currently covers webOS 4.10.2 only.
  Keymanager removal deletes the service's DB8 key blob; exact 11.2 firmware does not establish
  irreversible hardware revocation or protection against a root-level restoration of old DB8
  storage.
  Routine encrypted auth replacement is two-phase: the prior envelope remains active while a
  bounded pending candidate is persisted and authenticated, and only a second exact-revision CAS
  promotes it. A helper exit between those phases therefore leaves the prior sign-in readable.
  The legacy `com.palm.keymanager` AES-CFB interface is not used. Legacy `<id>-auth.json`,
  `state/auth.json`, `state/session.json` and consent files are read only as migration sources.
  Every active read of those legacy session/consent files and of the runtime telemetry spool and
  crashmark repairs the mode back to 0600 in place
  if it has grown group/other bits, rather than refusing to read or append to a file this install
  still owns. **Repairing the mode is not the same claim as trusting the content it protected while
  it was wide open**: a mode widened only to add a group/other READ bit is a disclosure problem and
  the content still loads, but any group/other WRITE bit means another uid could have rewritten the
  bytes, so that content is never trusted — the session and consent files are discarded rather than
  parsed, a marker or probe file is ignored and deleted, and the active runtime telemetry spool is
  truncated rather than appended onto. Persistent legacy telemetry queues are cleanup-only: their
  records are never opened or imported. **A write-widened SESSION file is QUARANTINED rather than deleted**: it is
  renamed to the source name with `.untrusted` beside itself, still 0600, and never parsed or opened again
  by anything in the app — it is kept for the television's owner to inspect, those bytes being the
  only record of what was tampered with. **Only the most recent one is kept**: a later tampering
  replaces that file rather than accumulating a series beside the session. Signing out and Delete
  all local data remove it with the session it belonged to; if the rename cannot be made (a peer
  holding that name as a directory, say), **or if the mode repair itself did not take** (a
  read-only remount, or a jail that denies the operation — the file would otherwise be moved aside
  still world-writable, token and all), the file is deleted outright, because the rule that matters
  is that it must not still be at the name the next launch reads. The consent file and the telemetry spool are deliberately
  unchanged: a discarded decision and a truncated spool leave nothing worth keeping. A downgrade of an existing encrypted file, a way to read it from another
  process, or a way to make the app write it somewhere world-readable is in scope.
- **The packaged storage service.** The IPK carries the native helper and package/service metadata,
  but no writable canonical state directory or runtime record. DB8 owns durability; the helper's
  transient `/tmp/<appid>.storage-runtime` contains only a private rendezvous/socket and a
  payload-free failure stage. Normal uninstall removes the private kind; package updates retain it.
- **The Developer Mode shared-namespace exposure, and why it is not the same claim as the above.**
  A **sideloaded Developer Mode** install runs under `jail_native_devmode.conf`, which
  mounts `/media/developer` **read-write for the whole directory**, measured `drwxrwxrwx` root:root
  — every homebrew app has its own uid but shares one gid, so **mode is the entire boundary** a file
  there can draw against a sibling app, and a peer that cannot read a 0600 file can still `unlink`
  or `rename` it out from under this app (a denial/substitution primitive, not a disclosure one —
  `write_atomic`'s `O_NOFOLLOW` + `create_new` and `read_owned_regular`'s ownership + regular-file
  check are what turn a FOREIGN or symlinked substitution into "rejected", never "parsed as ours").
  **`O_NOFOLLOW` protects only the final path component** — it says nothing about who can write to
  the enclosing directory, and every candidate path here passes through `/media/developer` (measured
  `drwxrwxrwx` root:root, no sticky bit — every Developer Mode app's own uid can rename or unlink
  entries directly under it) or, for this install's own directory, a subdirectory of it. **A peer
  able to rename in that shared directory can therefore REPLAY an old, genuinely-ours, correctly-
  0600 file back over a current one — moving it aside, waiting, and moving it back — and no
  ownership/mode/regular-file check anywhere in this app can distinguish that from the file it wrote
  itself.** That is a known, undetected limitation of every mode/ownership check described in this
  document, not a guarantee any of them make; it is pinned by a test
  (`plex::session::tests::a_replayed_older_valid_session_file_is_indistinguishable_from_current`,
  and its consent-file twin) precisely so it stays documented rather than silently assumed away.
  **This paragraph is limited to the measured Developer Mode namespace.** It is a property of that
  profile, not a claim about every webOS installation mode. This document does not assert the
  `mountappdir` layout or cross-app isolation of a retail/Homebrew jail because that behavior has
  not been verified in this environment. A storage error report may name which candidate tier
  (`developer`/`internal`/`app_dir`/`runtime`/`other`) was found or rejected — a category word, never
  the path itself.
- **The bundled FFmpeg.** Built from unmodified FFmpeg 9.0 with demuxers, parsers and subtitle
  decoders only — it is fed untrusted bytes from the network, so parser bugs reachable through
  `ff.rs` are in scope. Report FFmpeg's own bugs upstream as well.

## What is not in scope

- Post-compromise access by an attacker who already has root on the television. Root is not an app
  prerequisite; a report whose only precondition is an already-rooted OS describes a platform
  compromise rather than an app sandbox escape.
- The webosbrew Homebrew Channel, webOS itself, LG's own libraries, or Plex Media Server. Report
  those to their maintainers.
- Missing hardening that costs nothing to an attacker who is already executing code in the app's
  jail, unless you can show a concrete consequence.

## What this app does not have

No account of its own, no server, no payment path, and no user-generated content. It signs in to
**your** Plex account and talks to **your** servers.

**It does have telemetry, and that hedge used to say it did not.** A release binary carries a Sentry
DSN and a PostHog project key — both **write-only ingest credentials**, publishable by design, which
permit sending to a project and grant no read of anything in it. First run asks about crash reports
and product analytics separately. The first answer remains a draft; answering the second publishes
both requested choices and queues their record on the bounded persistence worker. A withdrawal
closes its runtime gate and removes its identifier immediately; an enable becomes effective only
after its prospective local cutoff completes. The UI does not call Pending saved and reports a
failed or uncertain write. Only a **Share** answer enables that category and permits its POSTs to
`ingest.de.sentry.io` or `eu.i.posthog.com`. The one deliberate exception is the sign-in screen's
one-off "Send report" press: it is its own consent, POSTs to Sentry under no standing answer
either way, and carries no identifier — a researcher can tell it apart from the consent gate
failing open (below) by that missing identifier. `BACK` navigates without recording a refusal.
Later changes live under Account → Settings → Privacy & data, where **Done** commits and `BACK`
discards.
The Sentry
**auth token** is the real secret in this system: it can read and delete the project, it never
enters the binary, and it exists only as a GitHub Actions secret used by `sentry-cli` in the release
workflow.

In scope for a report, and worth naming since a "no telemetry endpoint" line told researchers not to
look here: the consent gate failing open, an identifier existing before product analytics is
explicitly enabled or surviving its withdrawal, anything that gets a runtime string past
`diag::schema`'s no-owned-strings guarantee,
and the spool's file mode or its contents. [PRIVACY.md](PRIVACY.md) is the full account of what
leaves the television.
