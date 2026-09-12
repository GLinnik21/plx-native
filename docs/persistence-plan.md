# Shared persistence: implementation and acceptance ledger

Approved 2026-09-11. The current release goal is the 0.6 maintenance line only. A later 0.7
forward-port must integrate the same record/queue guarantees without replacing its newer Session
fields, but that port is deferred and is not a blocker for the 0.6 release. This is a work ledger,
not release evidence.

## Contract

- The television authority is one private DB8 object owned by the packaged `<appid>.storage`
  service. Stable/debug use distinct service owners and fixed object IDs. The app reaches the
  helper through a mode-0600, same-UID Unix socket; it exposes no credential-returning LS2 method.
- DB8 stores only `_id`, `_kind`, `_rev` and an opaque canonical JSON string. Typed Session/Consent
  adapters own schema and migrations. Keeping the document opaque prevents DB8 from injecting
  reserved fields into nested arrays; the old flattened candidate has one narrow reader which
  removes only the observed DB8-generated `operations[*]._id` before strict validation.
- Public preferences and consent are separate from the protected auth envelope. New/unknown webOS
  attempts Keymanager3 first; a fresh auth write or migration may fall back to a private-DB8
  ACL-only envelope and must retain closed diagnostic evidence. Public/routine mutations never
  downgrade existing healthy ciphertext. The policy selects ACL-only directly for reported webOS
  majors 1–4; runtime evidence currently covers webOS 4.10.2 only.
- Migration: validate source, CAS-write the destination, reload and compare the typed candidate,
  authenticate protected auth, then clean the source. Historical replay proves durability but
  never authorizes migration cleanup. Unknown/future canonical records remain terminal.
- Sign-out is one `ClearTenure` tombstone for Session and Consent. Runtime access/reporting stops at
  admission; cleanup follows confirmed durability and is reported separately.
- In-memory typed snapshots use independent RwLocks. Short coordinator sections serialize
  edits/revisions/enqueue. Blocking I/O runs on one bounded FIFO writer; UI polls receipts.
  No write lock held over I/O, no async runtime, no exit-time-flush durability promise.
- Late auth work is invalidated on sign-out. No older revision can overwrite a newer one.
- A routine refresh over existing ciphertext is two-phase: DB8 first persists a bounded pending
  encrypted candidate while old auth/public/ledger remain active, then promotes it with a second
  exact-revision CAS only after authenticated readback. Restart Load keeps serving old auth;
  matching reconcile/duplicate may resume, while preferences, consent, newer auth or ClearTenure
  can cancel the pending candidate without activating it.
- A future SQLite backend implements the same canonical string/state-operation contract through
  its own transactions and CAS. SQL collections need their own repository. No SQLite now.
- Image blobs are disposable, bounded and separate from critical state and its write queue;
  connect existing avatar-cache in 0.7. Profile credentials/PIN verifiers are not disposable.
  Existing report queue retains consent/purge rules; no resend migration.

## Implementation batches

- [x] Establish module boundaries without behavior change.
- [x] Legacy JSON backend + bounded safe filesystem operations + injected-failure contract tests.
- [x] Bounded FIFO worker + tickets; snapshot reads never wait for disk.
- [x] Session canonical migration, versioning, unknown-field preservation and revocation record.
- [x] Session asynchronous writes, ordered revisions and visible completion/errors at all callers.
- [x] Consent canonical migration + asynchronous persistence + revocation/error reporting.
- [x] UI/boot integration: no new UI-thread persistence I/O; existing fresh-auth and keymanager
      behavior retained.
- [x] Per-tag 0.6.x direct/chained migration fixtures, concurrent/failure tests, documentation audit.
- [x] Private native service, DB8 fixed-ID CAS, typed operation ledger, Keymanager/ACL policy,
      authenticated socket protocol, opaque DB8 document and flattened-record recovery.
- [x] Host/default/hostsim/shipping checks, ARM build and actual IPK metadata verification.
- [x] Package-update preservation on the development television: published v0.6.0 plaintext
      sign-in into v0.6.6, then nonempty current v0.6.6 Session/Consent through the corrected
      same-version package update and restart.
- [ ] Package upgrade on the issue #76 reporter's newer webOS, including a real-Key-Manager
      session; restart alone and the older development television do not settle that path.
- [ ] Deferred after the 0.6 release: 0.7 integration preserving profiles, PIN, last_library,
      auto_sign_in and current consent scopes.
- [ ] Release preparation/publication under cut-release workflow, only after required gates.

## Scope of assurance

The current assurance target is a forward upgrade from every published 0.6.x to the fixing 0.6
patch. The later 0.7 forward-port is deferred future acceptance, not a promise made by this 0.6
ledger. No downgrade, uninstall/reinstall, filesystem rollback, physical data loss or lost-key
guarantee. No new data collection or consent-scope expansion. Do not change existing
settings-reset policy.

## Evidence so far

- On the webOS 4.10.2 development television, the packaged debug app and native helper ran under
  the same jailed UID. The first flattened DB8 candidate reproduced the platform mutation that
  motivated the opaque representation: DB8 added `_id` to `operations[0]`, and strict load failed.
  After the fix, the same candidate migrated in place to an outer `_id/_kind/_rev/state` wrapper.
  A fresh process reopened the account and server without QR (`account=1`, `pms=1`, `server=1`,
  `local=1`), reached Home, and preserved subsequent Home/settings edits. The observed inner state
  reached revision 15 with both migration domains complete, protected auth present and no nested
  DB8 IDs. This proves the old-TV process-restart path, not reboot or newer-TV Keymanager behavior.
- [Exact webOS 11.2 firmware decompilation](measurements/db8-keymanager-webos11-static-proof-2026-09-12.md)
  confirms service-owner checks, repeated `putKind`, private
  kinds, fixed-ID `_rev` CAS, integer revisions, generated service permissions and public
  Keymanager3 operations. It is static compatibility evidence; runtime package activation,
  Keymanager round-trip and reboot/update on that set remain gates.
- The storage helper/state suite passes 85 tests, including consent-before-auth, locked public
  settings, lost-reply reconciliation, authenticated receipt replay after restart, rejection of
  unreadable/mismatched auth, fresh-only Keymanager fallback, strict ciphertext refresh, ledger
  eviction, authenticated migration readback, maximum opaque-document escaping and key lifecycle.

- The bounded FIFO is the production persistence executor for Session and Consent. Focused host
  tests cover typed cross-domain ordering, off-thread execution, nonblocking Full rejection without
  running rejected work, worker disconnect, dropped tickets, explicit startup refusal and
  zero-capacity behavior. Per-operation receipts keep Pending distinct from Durable, Uncertain and
  Failed; cleanup is reported independently.
- The current tree passed 2,648 default and 2,677 host-simulator Rust tests (one ignored in each),
  the Python/hook tail, the shipping-feature check, ARM app/helper builds, debug-IPK package
  assertions, ELF checks and the 4.4.2–11.2 firmware import matrix. These local gates are not CI
  publication evidence.
- Test-only commit `75d23a52` strengthened the published-v0.6 migration matrix without changing
  production behavior; all six focused migration tests passed. The matrix compares each first
  canonical payload byte-for-byte at the exact migration boundary, then checks every typed Session
  field after legacy load and canonical reopen. Its production-coordinator leg changes nonempty
  Home pins and recents, confirms the exact durable receipt revision, and reopens those values.
- On the development television, the initial published-v0.6.0-to-v0.6.6 update preserved the
  installer-facing legacy plaintext Session byte-for-byte, installed the binary from the package,
  and imported its account and profile identities; the profile avatar was refreshed normally. That
  v0.6.0 source already held an empty `home_pins`, so this run cannot show that migration preserved
  a prior Home choice. A later corrected-v0.6.6 package update again installed the packaged binary
  and preserved the existing canonical Session and Consent bytes across installation; the stored
  Session had a nonempty asked Home answer with three libraries on and two off, and the next boot
  retained the account, pins, playback quality and recent searches. The user confirmed Home opened
  without asking the choice again. No cleanup error appeared on that launch.
- After the final receipt/fallback implementation, a release-built debug package was installed
  over the same private DB8 state without clearing it. The next process again restored account,
  PMS credential, local server and profile roster without QR, reached the profile route, and
  durably recorded a later roster refresh through the old-OS ACL-only tier.
- Previous issue-76 field evidence still proves app-local restart on the reporter's Lite 11.2, but
  no v0.6.6 package upgrade has run there. Neither development-TV update establishes a power-cycle,
  real-Key-Manager migration, playback, background lifecycle, sign-out cleanup or consent
  withdrawal.

### Published 0.6 fixture matrix

`tests/fixtures/persistence/manifest.json` records the exact source commit for v0.6.0 through
v0.6.5 and the consent fields each tag shipped. The companion synthetic fixtures cover Session
credentials/settings/pins/recents/source metadata, consent Yes and stored No, and secure-envelope
v1 with identity absent before 0.6.3 and present afterward. They contain no real credentials,
identifiers, addresses, or device output.

The child `session::migration_tests` matrix (when enabled by the session module) proves direct
legacy-to-canonical migration, exact opaque payload transport at the first canonical-record
boundary, canonical reopen with complete typed identity/settings equality, durable asynchronous
pin and recent-search edits, routine consent rewrite, stale legacy copies after clear, and the
secure-envelope shape. It does not claim byte identity after a typed routine rewrite, an
old-binary/0.7 executable guarantee, package-upgrade preservation, key-manager cryptographic
validity, or TV behavior. The 0.7 executable proof is deferred; device and IPK upgrade evidence
remain separate 0.6 acceptance gates.
