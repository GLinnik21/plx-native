# Every 0.6.3 report, proved to ARRIVE — dev television, 2026-09-10

Stage B of the 0.6.3 consent/scope work. The question is not "did the app log that it sent
something" — a `telemetry:` line saying so is the app's own claim about itself. The question is
whether the event is **in Sentry** and the property is **in PostHog**, and every row below is
answered from the receiving end, by query, with the device log kept only as the corroborating half.

Everything here was taken on the **debug** install (`com.beb.plxnative.debug`), under
`tools/tv-lock.sh` leases, panel off, no media played, no watch history written. The stable install
(`com.beb.plxnative`) was never launched, deployed to or read from; its four files under
`/media/developer` still carry their pre-session mtimes.

## Provenance — the same for every scenario

| fact | value |
|---|---|
| branch / sha | `consent/scope-versions-v0.6` @ `ef98b347` |
| build | `make FLAVOR=debug deploy` (dev feature set; **not** `RELEASE=1` — the fake key-manager seam is compiled out of a release build) |
| first log line | `install: id=com.beb.plxnative.debug flavour=debug runtime=/tmp/com.beb.plxnative.debug features=dev APPID_env=com.beb.plxnative.debug` |
| second log line | `appdir: /media/developer/apps/usr/palm/applications/com.beb.plxnative.debug (from current_exe)` |
| version the binary reports | `0.6.3-dev` — Sentry `release: plxnative@0.6.3-dev`, PostHog `app_version = '0.6.3-dev'`, and the on-screen diagnostics line `PlxNative 0.6.3-dev · webOS 4.10.2 · m16p3 · M19_DVB · BOARD_PT_1ST` in `telemetry-oneoff-signin-tv-2026-09-10.png` |
| telemetry configuration lines | `telemetry: env=development sentry=yes posthog=yes` — **both endpoints configured, and both are the DEV pair** |
| GNU build id (Sentry `dist`) | `b80bf56c7f82862794571576992db41a3fdf0876` |
| device | webOS TV `release=4.10.2 codename=goldilocks2-grampians api=4.1.0`, `model=m16p3 board=M19_DVB hw=BOARD_PT_1ST`, `devjail: soc=m16p3 rtkmem=n/a` |

**What this proves and what it does not.** `env=development` plus the Sentry project
`plx-native-dev` says every event below travelled the **DEV** ingest pair — the one
`pkg/telemetry.local.json` holds on a developer's machine. It says nothing about the RELEASE pair,
which is only ever injected by the release workflow and physically absent from this checkout. The
release pair is evidenced separately and independently:

* **PostHog** already holds `signin.started`/`signin.completed` rows at `app_version = '0.6.2'`
  from three televisions that are not this one (`device_model` `o22`, `o22n2`, `lm21u`, three
  distinct `distinct_id`s, 2026-09-10 between 19:15 and 20:54 Europe/Minsk) — the shipped 0.6.2
  build reporting from the field.
* **Sentry** already holds the k5lp crash from issue #74 on the release project.

The dev set is `m16p3`; no row below and no row there can be confused for the other.

The real keymanager3 is **absent on this firmware** (`keymanager: begin(decrypt) refused
errorCode=-1`, the hub's "Service does not exist" shape), so the reporters' issue-#76 shape is
reproduced with the dev-only fake service, `plxnative-keymanager=perprocess`. Every launch that
used it says so in its own log before any `seal`/`open` can run:
`keymanager: FAKE service armed mode=perprocess`.

## Step 0 — precondition audit

`/media/developer`, before anything was launched on the deployed build:

```
-rw-------  com.beb.plxnative.debug-auth.json                4399    (0600)
-rw-------  com.beb.plxnative.debug-secure-storage.proven       65    (0600)
-rw-------  com.beb.plxnative.debug-secure-storage.refused     102    (0600)
-rw-------  com.beb.plxnative.debug-telemetry-crashmark.json    22    (0600)
-rwxrwxrwx  com.beb.plxnative.debug-telemetry-spool.bin       2226    (0777)   <-- widened
-rwxrwxrwx  com.beb.plxnative.debug-telemetry.json             162    (0777)   <-- widened
```

Two of the six were **0777**, exactly the finding
`docs/measurements/credential-storage-native-apps-2026-09-10.md` records. The self-heal that landed
on this branch (`session::repair_owned_mode`, reached through `read_owned_regular`) fixed both on
the very first launch, and said so:

```
perm: repaired com.beb.plxnative.debug-telemetry.json was 777
perm: repaired com.beb.plxnative.debug-telemetry-spool.bin was 777
```

Modes immediately afterwards: **both 0600**, every other file unchanged. The repair is `fchmod` on
the already-open, already-ownership-checked fd, so it cannot be raced by a peer in the shared
`/media/developer` namespace.

Stored consent as found was **Yes/Yes but at `asked_version: 4`** — i.e. both categories on, with
the Errors scope-5/scope-6 and Usage scope-6 extensions still pending, which the boot line reports
as `telemetry: answered=false errors=true usage=true id=yes errors_id=yes`. Reaching the standing
sign-in report (Errors scope 5) and the storage report (Errors scope 6) therefore required
answering that extension, which was done deliberately, once, through the real UI
(`plxnative-consent` + two `ok` tokens on the remote FIFO — see
`telemetry-consent-extension-tv-2026-09-10.png`, which shows the extension wording
"Asking again because what is collected has changed" and the extension's own answer pair
`Keep on, with this` / `Keep as before`). Result on disk:

```json
{ "asked_version": 6, "errors": true, "usage": true,
  "install_id": "ea2d34ca…", "errors_id": "6d10e3af…",
  "errors_scope": 6, "usage_scope": 6,
  "errors_declined_scope": 0, "usage_declined_scope": 0 }
```

**Both identifiers survived the extension** — `apply_extension`'s `resolve` reuses an existing id
rather than minting — which is what makes `user.id` below checkable against a value that was
already on the television before this session started.

## The scenarios

Times are UTC. `errors_id` values are abbreviated to their first eight hex characters here; each
was compared in full against the file on the television and matched byte for byte.

| # | scenario | trigger set | key log lines (scrubbed) | arrival, from the receiving end | deviations |
|---|---|---|---|---|---|
| **N** | **Negative control** — failed sign-in read-out, Send NOT pressed. 17:54:13 → 17:56:01 | consent file, session file and spool moved aside; `plxnative-signinfail=error`, `plxnative-login`; `--no-token` | `telemetry: answered=false errors=false usage=false id=none errors_id=none` · `telemetry: env=development sentry=yes posthog=yes` · `auth: ERROR Couldn't create a sign-in code — check the connection.` · **zero** `telemetry: … -> ` and **zero** `telemetry: flushed` lines; spool 0 bytes at close | **Sentry: 0 rows.** `search_events(org=gleb-linnik, region=https://de.sentry.io, project=plx-native-dev, dataset=errors, query="release:plxnative@0.6.3-dev", period=1h)` → *No results found*; the wider `query="environment:development", period=1d` also → *No results found*. **PostHog: 0 rows.** `SELECT event, timestamp, properties.app_version, properties.session_storage FROM events WHERE timestamp >= toDateTime('2026-09-10 17:54:13','UTC') AND timestamp <= toDateTime('2026-09-10 17:57:30','UTC')` → empty result set | The spool had to be moved aside too: it held 3 records queued by an **earlier** session, and `sender::send_one` re-checks the endpoint but **not** consent at flush time, so those would have gone out inside the window and made "zero rows" untrue for a reason that has nothing to do with this control. Restored afterwards. |
| **3** | **One-off "Send report", pre-consent** | same state as N (no consent, no session); `plxnative-signinfail=error`, `plxnative-login`; `--no-token`; FIFO `right`, `ok` at 18:00:16 | `telemetry: answered=false errors=false usage=false id=none errors_id=none` · `auth: ERROR Couldn't create a sign-in code…` · `telemetry: flushed 1 of 1 record(s)` — **exactly one** record left the device | **Sentry event `341fa6881d72847099050f9e34a88b88`** (issue `PLX-NATIVE-DEV-1`, 18:00:14Z): `SignInError`, tags `signin.consent=one_off`, `signin.kind=pin_create`, `signin.link=dns`, `signin.storage=secure_refused`, `dist=b80bf56c…`, `release=plxnative@0.6.3-dev`. Context `signin`: `curl_rc: 6`, `unanswered: "one"`, `failing_for: "under_10s"`, `code_generation: 1`. **No `user` object at all** — the issue reads `Users Impacted: 0`. Screen: `telemetry-oneoff-signin-tv-2026-09-10.png` | none |
| **3b** | **The one-off press changes no standing state** | — | — | `com.beb.plxnative.debug-telemetry.json` **absent before and absent after** the press (`ls` → *No such file or directory* both times), so there is no md5 to differ; boot line still `answered=false errors=false usage=false id=none errors_id=none` after it. **PostHog 0 rows** for 17:59:00–18:03:00 UTC (same SQL, different bounds) — no `app.launch`, no `route.entered`, nothing. The only thing the television emitted for that launch is the `one_off` Sentry event above | Proving "unchanged bytes" by md5 was not possible in the strongest form the brief asked for, because the *stronger* fact held: the file did not exist at all, before or after. |
| **1** | **Storage error report (issue #76 shape)** | proven + refused markers moved aside, probe removed; `plxnative-keymanager=perprocess`; two launches (18:03:45 plant, 18:04:11 check) | launch 1: `keymanager: FAKE service armed mode=perprocess` · `keymanager: generateKey -> created` · `session protection: keymanager3` · `session: secure storage has not yet been proven on this install; keeping the 0600 file and probing` (probe file appears). launch 2: `keymanager: finish(decrypt) refused errorCode=-20030` · `session: secure storage is marked refused on this install; keeping the 0600 file` · `telemetry: flushed 2 of 2 record(s)`. Refused marker on disk: `{"key_outcome":"created","reason":"envelope_unopenable","refused_at_version":"0.6.3-dev","stage":"finish_decrypt"}` | **Sentry** (issue `PLX-NATIVE-DEV-2`, 18:04:19Z): `StorageError`, tags `storage.stage=finish_decrypt`, `storage.class=secure_refused`, `user.id=6d10e3af…` (= the `errors_id` on the television). Context `storage`: `key_outcome:"created"`, `refused_marker:true`, `registered_with_app_id:false`, `service_error_code:-20030`, `stage:"finish_decrypt"`, `class:"secure_refused"`. **PostHog**: every usage row from these launches carries `session_storage` — `unknown` on `app.launch`, then `plaintext` (launch 1, before the refusal) and `secure_refused` (launch 2) on `route.entered` | The brief expected a log line `storage: … stage=finish_decrypt key_outcome=created`. **No such line exists** — nothing in `telemetry/storage.rs` or `plex/session.rs` logs the report's stage/outcome. The same two facts are on disk (the refused marker JSON) and in the event (`storage` context). Worth adding a line, or worth deleting the expectation; recorded either way. |
| **4** | **Standing sign-in report** | consent at scope 6, session present; `plxnative-signinfail=error`, `plxnative-login`; `--no-token`; launch 18:04:49 | `telemetry: answered=true errors=true usage=true id=yes errors_id=yes` · `auth: ERROR Couldn't create a sign-in code…` · `telemetry: flushed 3 of 3 record(s)` | **Sentry** (issue `PLX-NATIVE-DEV-1`, second event, 18:09:36Z): `SignInError`, `signin.consent=standing`, `signin.kind=pin_create`, `signin.link=dns`, `signin.storage=secure_refused`, **`user.id=6d10e3af…`** — the errors_id, present where the one-off event has no user at all. **PostHog**: `signin.started` and `signin.failed` at 18:04:58Z, both carrying `session_storage=secure_refused` | **The event's Sentry timestamp is delivery time, not occurrence time.** `signin::event_body` writes no `timestamp` field (unlike `storage::event_body`, which carries `occurred_at_ms`), so Sentry stamps receipt. That surfaced a harness error of mine: I closed the app 30 s after launch, before the sender's next flush, leaving the report in the durable spool (3404 bytes); a later ordinary launch drained it at 18:09:36. The corroboration that this is the scenario-4 report and not a fresh failure is that the draining launch's own sign-in **succeeded** (`auth: pin created (code 1 of 4, 900s to authorize)`, no `auth: ERROR`). Incidentally a clean proof that the spool survives a process death. |
| **5** | **Crash report** | consent at scope 6; `plxnative-crashtest=segv`; crash 18:12:27, healthy relaunch 18:13:10 | crash run: `telemetry: native ARM crash capture active` · `crashtest: DELIBERATE crash, kind=segv signal=11` · the C tracer's `*** SIGNAL 11 (SIGSEGV) addr=0x0 pc=0x242474 lr=0x242460` + registers + the `bin:`/`at:` maps lines, appended to `plxnative-crash.log` (which survives, as designed). relaunch: `telemetry: native crash envelopes queued=1 rejected=0` · `telemetry: crash log had 1 report(s), queued 0, native_wins=1, symbols=yes` · `telemetry: flushed 4 of 4 record(s)` | **Sentry event `8c5f1d7314b743ec677993eabbe3f49c`** (issue `PLX-NATIVE-DEV-3`, 18:12:35Z): `SIGSEGV`, level `fatal`, mechanism `signalhandler`, culprit **`plxnative_modules::dev::crash_on_purpose`**, stack `crash_on_purpose ← plex_run ← __libc_start_main`, **5 threads** listed by name (`plxnative` crashed + four `sentry-*` threads with frames), `os: Linux 4.4.84 / build 169.gld4tv.5`, contexts `webos {release: 4.10.2, api: 4.1.0, codename: goldilocks2-grampians}` and `hardware {model: m16p3, soc: M19_DVB, revision: BOARD_PT_1ST, install: devmode, rtkmem: n/a}`, `dist=b80bf56c…`, `user.id=6d10e3af…` | `native_wins=1` — the local C fault event was consumed by the native envelope, so one process death became one Sentry event, exactly as the import contract says. |
| **2** | **Deferred storage report, then consent** | consent file moved aside; markers + probe removed; `plxnative-keymanager=perprocess`; launch 1 18:13:55 (plant), launch 2 18:14:32 (`plxnative-consent` armed); Yes/Yes answered at **18:14:59** via FIFO `ok`,`ok` | launch 2: `telemetry: answered=false errors=false usage=false id=none errors_id=none` · `keymanager: finish(decrypt) refused errorCode=-20030` · refused marker armed · **no `telemetry: flushed` line and the spool stayed 0 bytes** — nothing queued, the report held in the in-memory `DEFERRED` queue. After the two `ok`s: `telemetry: flushed 1 of 1 record(s)` and a `telemetry.json` written with `asked_version: 6, errors_scope: 6, usage_scope: 6` | **Sentry event `87881d9cf6f44a6e5ae2581b9c1aef97`** (issue `PLX-NATIVE-DEV-2`, second occurrence): `StorageError`, `storage.stage=finish_decrypt`, `storage.class=secure_refused`, same `storage` context (`service_error_code:-20030`, `key_outcome:"created"`, `refused_marker:true`). **Its Sentry timestamp is `18:14:41Z` — eighteen seconds BEFORE the consent answer at 18:14:59** — which is `storage::event_body`'s `occurred_at_ms` doing precisely what it exists for: a held report is dated to when the failure happened, not to when `replay_deferred` got to it. `user.id=2c17afc3…`, the errors_id the Yes had just minted — the id is read at send time, the timestamp at failure time | The empty-spool-plus-no-flush pair is the deferral evidence, because **nothing logs a deferral**: `storage::report_error`'s `Deferred` arm writes no line. Same recommendation as scenario 1 — one line, or drop the expectation. |
| **6** | **Usage funnel** | — | — | For `app_version = '0.6.3-dev'` this session PostHog holds `app.launch` (×6), `route.entered` (×6, `screen` ∈ {`home`,`login`}), `signin.started` (×2) and `signin.failed` (×1). **`session_storage` rides every one of them** and took three of its six values here: `unknown`, `plaintext`, `secure_refused` | `signin.completed` and `signin.cancelled` were **not** produced and **need the owner's QR sign-in** — a real authorization on plex.tv, which no dev seam can stand in for. `signin.started`/`signin.failed` did NOT need one: `plxnative-signinfail=error` reaches them. |

## What the negative control actually rules out

Between 17:54:13 and 17:57:30 UTC the dev Sentry project received nothing at all — and, queried at
that moment, had received nothing at `environment:development` for the preceding day, so the five
events below are also the project's only content for this window — and the PostHog project received
no row of any kind. The nearest neighbouring PostHog rows, at 17:53:52 and 17:54:08 UTC, are the 0.6.2 field
televisions described above — different `app_version`, different `device_model`, different
`distinct_id`. So the five events in the table are the five presses and faults that produced them,
and the app sent nothing on the launch where nothing was asked of it.

## Deviations and things a reader should not over-read

1. **The dev set has no keymanager3.** `errorCode=-1` is the hub's service-absent answer. Every
   storage result above is the *fake* backend's behaviour, which reproduces the reporters' shape
   (a key that does not survive the process) and reproduces nothing about a firmware that has the
   real service. The `absent` and `healthy` modes are unexercised here.
2. **Two expected log lines do not exist** — the storage report's stage/outcome (scenario 1) and
   the deferral (scenario 2). Both facts were proved from other surfaces.
3. **`signin::event_body` carries no timestamp**, so a spooled sign-in report is dated on arrival.
   `storage::event_body` does carry one. The asymmetry is deliberate in the storage case; whether
   the sign-in case wants the same treatment is a real question this run raised.
4. **A flush does not re-check consent.** Records already in the durable spool are sent on the next
   flush whatever the decision file says at that moment. That is visible in the spool carve-out
   the negative control needed, and it is a consent-lifetime question worth a decision rather than
   a bug this run is claiming.
5. **No PushNotification was sent before the first device command.** The tool is not in this
   agent's toolset; the coordinating session was told so before the first command instead.
6. **No audio mute exists on this firmware.** `luna://com.webos.service.audio/master/setMute` and
   `/master/getMute` both answer `Unknown method`. Nothing in this session played media, so the
   set produced no audio; the panel was off for every run except the two deliberate screen
   captures.

## State handed back

Every file moved aside was moved back, verified by md5 where one existed:

* `com.beb.plxnative.debug-auth.json` — restored, md5 `a88e991b…`, identical to the pre-session file.
* `…-secure-storage.proven` / `…-secure-storage.refused` — the originals restored (the markers this
  session created were removed first).
* `…-telemetry-spool.bin` — the original 729-byte queue restored.
* `…-telemetry.json` — the scope-6 file, i.e. the same `install_id` and `errors_id` the install had
  before, with `asked_version` moved 4 → 6 by the extension answered in step 0. That change is
  deliberate and is the one piece of stored state this session left different; the throwaway
  `errors_id` minted inside scenario 2 was discarded with its file.
* Runtime root `/tmp/com.beb.plxnative.debug` holds only the three `*.log` files and the FIFO — no
  `plxnative-*` trigger of any kind.
* App relaunched interactively (`tv-session.sh down`), panel off, `tools/tv-lock.sh status` reports
  **FREE**.
