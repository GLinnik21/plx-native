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
- **TLS.** Certificate verification is on for every request (`net.rs`). Anything that disables,
  downgrades or bypasses it is in scope; so is any path where a failure to *set* a TLS option
  results in a request going out anyway.
- **The session file.** `<id>-auth.json` holds one access token per server your account can reach.
  It is created 0600 through `open(2)`'s own mode argument. Any way to read it from another process,
  or to make the app write it somewhere world-readable, is in scope.
- **The bundled FFmpeg.** Built from unmodified FFmpeg 9.0 with demuxers, parsers and subtitle
  decoders only — it is fed untrusted bytes from the network, so parser bugs reachable through
  `ff.rs` are in scope. Report FFmpeg's own bugs upstream as well.

## What is not in scope

- Anything requiring root on the television. This app is installed by people who have **already**
  rooted their set; a report that begins "as root" describes the starting conditions, not a
  vulnerability.
- The webosbrew Homebrew Channel, webOS itself, LG's own libraries, or Plex Media Server. Report
  those to their maintainers.
- Missing hardening that costs nothing to an attacker who is already executing code in the app's
  jail, unless you can show a concrete consequence.

## What this app does not have

No account of its own, no server, no payment path, and no user-generated content. It signs in to
**your** Plex account and talks to **your** servers.

**It does have telemetry, and that hedge used to say it did not.** A release binary carries a Sentry
DSN and a PostHog project key — both **write-only ingest credentials**, publishable by design, which
permit sending to a project and grant no read of anything in it. After an explicit opt-in (two
switches, both off by default) it POSTs to `ingest.de.sentry.io` and `eu.i.posthog.com`. The Sentry
**auth token** is the real secret in this system: it can read and delete the project, it never
enters the binary, and it exists only as a GitHub Actions secret used by `sentry-cli` in the release
workflow.

In scope for a report, and worth naming since a "no telemetry endpoint" line told researchers not to
look here: the consent gate failing open, an identifier existing before an opt-in or surviving a
withdrawal, anything that gets a runtime string past `diag::schema`'s no-owned-strings guarantee,
and the spool's file mode or its contents. [PRIVACY.md](PRIVACY.md) is the full account of what
leaves the television.
