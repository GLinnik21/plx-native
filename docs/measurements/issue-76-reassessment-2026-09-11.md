# Issue #76 / 0.6.4 reassessment — 2026-09-11

This ledger separates code-proven behavior from claims still needing a real reporter or receiver.

## Established from code and host tests

- A fresh sign-in over a recognized secure envelope whose key service is unavailable has an
  explicit recovery path to the owner-only file; the foreign/unrecognized-envelope guard remains
  authoritative.
- Candidate reads use a closed vocabulary, including `unparsable` for readable bytes that match no
  recognized envelope or session shape.
- `SignInNotPersisted` is emitted from the user-visible commit path and deduplicated by save outcome
  and preserve reason.
- Deferred storage reports are discarded at sign-out. Candidate summaries now travel on
  load-owned detached reports through the explicit telemetry API, so probe reports cannot steal the
  session-read summary.
- The consent preview includes `persist_outcome`, `preserve_reason`, and `candidate_reads`.

## Not established

- No real reporter television with `keymanager3` has been run on this 0.6.4 code path. The
  unavailable-envelope recovery is not evidence that a real keymanager3 will reproduce the
  reporters' original loop or grant the same identity.
- Sentry API access and a current one-off development report receipt are established below.
  The fresh recovery itself ran with errors and usage reporting disabled; its successful save
  and readback are device-log evidence, not a received Sentry event.
- The current UI has now been checked on the dev TV and simulator: Details pointer/D-pad/back/new
  code interactions passed, with panel-open/panel-closed captures. This does not establish the
  real-keymanager3 recovery path or media playback.
- A plaintext storage class does not prove an authorized write: it can be the default client-id
  seed or another ordinary plaintext path.

## PostHog receipt evidence

Queried the live [PlxNative PostHog project](https://eu.posthog.com/project/260830/events) on 2026-09-11, with SQL timestamp bounds `>= toDateTime('2026-09-10 00:00:00')` and `< toDateTime('2026-09-12 00:00:00')`, filtering `app.launch` and `signin.started/completed/failed` for versions `0.6.2` and `0.6.3`. The project timezone is Europe/Minsk; the query did not explicitly supply a timezone to `toDateTime`.

| Version | Firmware / SoC | Events received | Storage class |
|---|---|---|---|
| `0.6.2` | `9.2.2 / o22` | 1 started, 1 completed | `plaintext` |
| `0.6.2` | `10.2.2 / o22n2` | 1 started, 1 completed | `plaintext` |
| `0.6.2` | `6.5.3 / lm21u` | 1 started, 1 completed | `plaintext` |
| `0.6.2` | `5.5.0 / o20` | 1 started, 1 completed | `plaintext` |
| `0.6.3` | `5.3.0 / o20` | 1 started, 1 completed | `plaintext` |
| `0.6.3` | `6.5.3 / o20n` | 1 started, 1 completed | `plaintext` |
| `0.6.3` | `5.3.0 / o20` | 1 app launch | `unknown` |

These rows carry `environment=production` and `install=devmode`: the latter describes the installation method, not the telemetry endpoint. Their firmware/SoC differs from the development television (`4.10.2 / m16p3`). They are not linked to individual issue reporters. No `signin.failed` rows matched this query.

Analytics consent and error-report consent are independent. These receipts prove that production analytics works on some newer sets; they do not establish Sentry consent, Sentry delivery, or persistence. In particular, `plaintext` can describe the default client-ID-only file written before authorization. The earlier [dev telemetry proof](telemetry-proof-tv-2026-09-10.md) remains separate evidence about the development configuration.

## Acceptance ledger

Passed in this worktree: final `make check` (2452 default-feature tests and 2481 hostsim tests),
shipping `CARGO_INCREMENTAL=0 cargo +nightly check --manifest-path rust-modules/Cargo.toml --lib
--no-default-features`, repeated current module groups (session 129, keymanager 55, telemetry 217,
UI login 38, each twice), candidate-read tests with `TMPDIR` unset and set, the real two-process
session boundary test, and the cold-load multi-report regression (green after a simulated negative
control that made both summaries disappear).

Pending: any real-keymanager3/reporter confirmation and publication.

## Real QR recovery and process-boundary proof

On 2026-09-11, the user completed a real Plex QR authorization on the debug television.
The source was commit `33db1c36`; the maintenance development version policy labels its debug
build `0.6.5-dev` after the package version was bumped to `0.6.4`.

Before staging, the debug session and telemetry files were archived privately with metadata.
Two healthy fake-keymanager launches produced a recognized sealed session. The test then removed
the test-created proven marker and changed the fake service to `stall`, representing an older
sealed session whose service no longer answers without prior cross-launch proof.

- Process `8869` read `developer:secure`, reported unavailable device key, and retained the sealed
  file. Its cold eligibility had account/PMS/server/local all false.
- The real QR authorization reached discovery and saved `persisted_plaintext`. Direct fresh-save
  disk readback reported `winner=developer result=match`.
- After removing the forced-login trigger, process `9332` started with the same `stall` mode,
  no injected token, and read `developer:plaintext`. Cold account/PMS/server/local were all true;
  the stored-session boot reached Home without QR authorization.

This proves fresh authorization recovery and persistence across a real application process
boundary on the TV. It does not prove real firmware keymanager behavior, a television power cycle,
or the reporters' original cause. No media was played. Sanitized local evidence is retained at
`/tmp/plxnative-064-qr-recovery-evidence.log` and `/tmp/plxnative-064-qr-restart-evidence.log`.

## Current one-off Sentry receipt

With the user's explicit permission, a separate debug `signinfail=error` launch exercised the
real one-off report button. This is a synthetic DNS-shaped sign-in failure, not a failed real
Plex request or the preceding successful QR recovery. The app logged `flushed 1 of 1 record(s)`.

The Sentry API returned event `9172517c28dfb06c1f7db3c676e9c506` in `plx-native-dev`, issue
`PLX-NATIVE-DEV-1`, received at `2026-09-11T09:31:00.866425Z`. It carries
`release=plxnative@0.6.5-dev`, `environment=development`, `consent=one_off`,
`kind=pin_create`, `link=dns`, `curl_rc=6`, `storage=plaintext`, and
`storage_candidate_reads=developer:plaintext`. This proves the one-off delivery path and the
current candidate-summary field reach the receiver without enabling standing telemetry consent.
It does not prove production endpoint delivery or receipt of a fresh-save outcome.

After testing, the original four debug data files were restored and GNU tar's comparison against
the private backup passed before relaunch. The private archive is retained for recovery.

The version bump is complete and the current local stable package was built and checked
successfully. The release is not published yet, so no public artifact hash is claimed here.

The historical red evidence for telemetry findings is recorded in the review session: restoring the
pre-fix paths produced failures for preview fields, candidate-summary retention, and sign-out
purging before the fixes were restored.

## Follow-up acceptance: reporting access and Login copy

The simplified Login copy and reporting changes passed the final host gates; final counts and
packaging evidence are recorded in the acceptance ledger above and the release audit.

The ordinary Login technical footer and duplicate raw transport sentence were removed. Details
retains the diagnostics and a pinned explanation of when sending a report is useful. The report
confirmation also explains that use case, and uses uncapped, left-aligned body text. The auth
handoff now holds a failed fresh save for an explicit report/continue decision keyed by attempt and
warning generation, without repeating a save on each frame. Device evidence still does not
establish real-keymanager3 behavior.

### Handoff integration checkpoint (not final release acceptance)

The new handoff passed `make check` with 2478 default-feature and 2507 hostsim tests
(`/tmp/plxnative-064-warning-full-check.log`), the shipping feature check, and the ARM build
(`/tmp/plxnative-064-warning-arm.log`). A subsequent table/footer spacing correction passed the
shipping check and was captured in the simulator. Earlier package and device gates above predate
this handoff and must not be treated as final verification of the current source.

The actual unwritable-directory regression failed against a negative control that immediately
handed out credentials, then passed with the hold restored. Exact-key lifecycle and one-save
checks also passed (`/tmp/plxnative-064-persistence-handoff-{red,green}.log`). Discovery evidence
cannot be overwritten by the final save until acknowledged. Finalization uses the latest coherent
controller state after roster reconciliation without issuing a second save.

The synthetic `signinfail=save` fixture exercised the warning, Details, and exclusive report
confirmation in the simulator. It does not perform a real failed save: its on-screen storage
facts are the simulator's independent live facts. Captures exposed a collision between the last
clipped table row and pinned reporting guidance; an explicit gap corrected it in the same
long-table fixture. Local before/after captures:
`/tmp/plxnative-064-warning-ui.wPCltl/details-overlap-{before,after}.png`.

At this checkpoint native verification and the unwritable-spool receiver proof were outstanding;
the following device session closes those checks. A current release package and final acceptance
audit are still required.

### Native warning and unwritable-spool receipt

The current debug binary was deployed and its hash verified before process `11151` started.
The synthetic `signinfail=save` fixture displayed the warning; D-pad navigation opened Details,
then its report confirmation alone, and Cancel followed by Continue returned to the fixture QR
screen. Native 1920×1080 captures were inspected:
`/tmp/plxnative-064-warning-native.png`, `...-warning-details-native.png`,
`...-warning-report-native.png`, and `...-warning-continued-native.png`.
Titles/body text fit, body text is left-aligned, and the Details guidance is separated from the
table. This is presentation/action evidence, not a real authorization or failed-save experiment.

A separate process, `11583`, read a synthetic recognized secure envelope with the debug key
service set to `stall`. Its cold snapshot was `developer:secure`, with all four eligibility flags
false and a `no_reply` storage error. The fake ciphertext is only a read-failure fixture; it is not
evidence about the real firmware's cryptography. The live Plex request returned HTTP 200.

After boot resolved its ordinary developer spool, the empty spool file was moved into the private
backup and an empty directory placed at that exact filename. The user-authorized manual one-off
Send therefore could not append to its selected spool. The UI reported **Report sent** with event
ID `4175abcfc8948826378a90b3302e1650`; Sentry's event-detail API confirmed receipt in
`plx-native-dev` at `2026-09-11T11:58:20.995611Z`:

- `environment=development`, `consent=one_off`, `source=storage_details`;
- `storage=secure_unavailable`, `storage_candidate_reads=developer:secure`;
- `storage_cold_errors[0]`: candidate `developer`, stage `no_reply`, class
  `secure_unavailable`, sealed identity `anonymous`, both registration flags false;
- webOS `4.10.2`, model `m16p3`, SoC `M19_DVB`, revision `BOARD_PT_1ST`;
- healthy link (`answered_2xx`, HTTP 200), not a synthetic network failure.

Standing consent stayed `answered=false errors=false usage=false`; no permanent opt-in was
enabled. `Report sent` plus receiver receipt proves the bounded direct fallback when spool writes
fail. Fresh-save arrays were empty in this cold-read test and remain covered by the host
snapshot/serializer and failed-write regressions, not by this particular receiver event.
Sentry filtered the duplicated `contexts.signin.kind` value, but the `signin.kind=authorization`
tag remained intact.

The temporary spool directory was removed, its original file restored, then all four original
debug data files were restored with metadata. GNU tar's comparison against the private archive
passed. Test triggers were cleared, the normal debug app relaunched as process `12100`, the panel
was turned off, and the TV lock released. The stable installation was untouched; the private
backup archive remains available for recovery.

### Current local release package

After the device handback, `make FLAVOR=stable RELEASE=1 ipk` completed successfully against the
current source; all packaging assertions passed. A separate `python3 ci/check-package.py` also
passed. Local logs: `/tmp/plxnative-064-current-stable-package.log` and
`/tmp/plxnative-064-current-package-check.log`. This supersedes the earlier stale local-package
checkpoint. No local package was published or installed over the stable app; CI must build any
public artifact. Publication and the final requirement-by-requirement handoff audit remain pending.

### Final BACK affordance

The requested `Click [BACK] to return` hint uses the existing shared `KeyHint`, on the right of
the Send report row. The full host gate passed again (2478 default-feature and 2507 hostsim tests,
`/tmp/plxnative-064-back-hint-check.log`), as did the shipping configuration and ARM build. Native
process `12555` ran the hash-verified debug binary. The hint fit without overlapping Send report;
BACK dismissed Details and returned to the QR page. Both native captures were inspected:
`/tmp/plxnative-064-back-hint-native.png` and
`/tmp/plxnative-064-back-hint-closed-native.png`. No report was sent in this UI-only follow-up and
no stored credential was replaced.

### Embedded-notice consistency audit

Reviewing the full maintenance-line diff found stale claims in the app's embedded Privacy text,
the Crash reporting question, and its example-report explanation: those still promised no
identifier of any kind or said a one-off report could not be looked up. They now distinguish an
event-specific Report ID from a persistent identifier. The embedded Privacy text also describes
the richer cold/fresh diagnostics and the direct in-memory fallback already described in
`PRIVACY.md`; no reporting scope or default was changed.

Regression assertions failed on those old claims
(`/tmp/plxnative-064-legal-oneoff-red.log`, `/tmp/plxnative-064-consent-oneoff-red.log`), then passed
with the corrected text. The full host gate passed with 2479 default-feature and 2508 hostsim
tests (`/tmp/plxnative-064-final-notice-check.log`), and the shipping configuration passed.
The current reporting-question text fit completely in the inspected simulator capture
`/tmp/plxnative-064-notice-ui.jZdlnP/shot-1.png`; neither reporting choice was enabled.

A separate read-only review of the complete `paths.rs` / `session.rs` maintenance-line diff and
auth call-site implications found no must-fix ownership/trust, recovery-authority, snapshot-lifetime
or prepared-handoff regression. Publication remains pending; the final package must include this
notice correction as well as the BACK hint.
