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
- **The session file.** `<id>-auth.json` holds one access token per server your account can reach.
  It is encrypted with the firmware's authenticated Key Manager where
  `com.webos.service.keymanager3` is available and permitted, with a 0600 plaintext compatibility
  fallback otherwise. The legacy `com.palm.keymanager` AES-CFB interface is not used because it
  provides no authenticated-encryption operation. The file is always created 0600 through
  `open(2)`'s own mode argument, and every open of it (and of the telemetry decision file, the
  telemetry spool, and every marker/probe file beside them) repairs the mode back to 0600 in place
  if it has grown group/other bits, rather than refusing to read or append to a file this install
  still owns. **Repairing the mode is not the same claim as trusting the content it protected while
  it was wide open**: a mode widened only to add a group/other READ bit is a disclosure problem and
  the content still loads, but any group/other WRITE bit means another uid could have rewritten the
  bytes, so that content is never trusted — the session and consent files are discarded rather than
  parsed, a marker or probe file is ignored and deleted, and the telemetry spool is truncated rather
  than appended onto. **A write-widened SESSION file is QUARANTINED rather than deleted**: it is
  renamed to `<id>-auth.json.untrusted` beside itself, still 0600, and never parsed or opened again
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
- **The Developer Mode shared-namespace exposure, and why it is not the same claim as the above.**
  A **sideloaded** (Developer Mode / Homebrew) install runs under `jail_native_devmode.conf`, which
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
  **This is a property of Developer Mode itself, not a bug in this app**, and it does not apply to a
  **retail** install: `jail_native.conf` gives a store-distributed app `mountappdir` — only its own
  directory in the mount namespace, nothing else on the device visible to it at all. A report
  describing this exposure (substitution OR replay) on a retail install (were one ever to exist) is
  in scope; the same exposure on a sideloaded install, reachable only by another process the user
  chose to sideload beside this one, is disclosed here rather than treated as a vulnerability of
  this app, and is not itself something to report. A storage error report may name which of these
  tiers (`developer`/`internal`/`app_dir`/`runtime`/`other`) a candidate session file was found or
  rejected at — a category word, never the path itself.
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
and product analytics separately. The first answer remains a draft; answering the second records
both choices, and only a **Share** answer enables that category and permits its POSTs to
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
