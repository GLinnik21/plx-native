# Shared persistence: implementation and acceptance ledger

Approved 2026-09-11. Implement on the 0.6 maintenance line first, then integrate the same core
into 0.7 without replacing its newer Session fields. This is a work ledger, not release evidence.

## Contract

- Canonical per-install `state/` prepared by the IPK; stable/debug isolated. Simulator state
  belongs to its instance runtime root. Legacy paths are migration sources, never permanent fallback.
- Typed Session/Consent adapters own schema, validation, keymanager policy and migrations.
  Internal RecordStore exposes read/commit, not filenames or SQL. JSON remains the on-disk format.
- Versioned records contain an exact legacy JSON payload or a cleared/revoked state. A present
  canonical record is terminal, including when unreadable; never resurrect a legacy credential.
- Migration: validate source, atomic destination write, file/parent sync, readback, then source
  cleanup. Cleanup failure is separate from persistence. Preserve unknown secure envelopes.
- Sign-out/withdrawal commits a durable token-free/off record before best-effort cleanup.
  Stop account use/reporting immediately; report failed durability/cleanup honestly.
- In-memory typed snapshots use independent RwLocks. Short coordinator sections serialize
  edits/revisions/enqueue. Blocking I/O runs on one bounded FIFO writer; UI polls receipts.
  No write lock held over I/O, no async runtime, no exit-time-flush durability promise.
- Late auth work is invalidated on sign-out. No older revision can overwrite a newer one.
- Future SQLite backend implements the record contract through its own transactions, not our
  file replacement primitive. SQL collections need their own repository. No SQLite now.
- Image blobs are disposable, bounded and separate from critical state and its write queue;
  connect existing avatar-cache in 0.7. Profile credentials/PIN verifiers are not disposable.
  Existing report queue retains consent/purge rules; no resend migration.

## Implementation batches

- [x] Establish module boundaries without behavior change.
- [ ] JSON backend + bounded safe filesystem operations + injected-failure contract tests.
- [ ] Bounded FIFO worker + tickets; snapshot reads never wait for disk.
- [ ] Session canonical migration, versioning, unknown-field preservation and revocation record.
- [ ] Session asynchronous writes, ordered revisions and visible completion/errors at all callers.
- [ ] Consent canonical migration + asynchronous persistence + revocation/error reporting.
- [ ] UI/boot integration: no new UI-thread I/O; existing fresh-auth and keymanager behavior retained.
- [ ] Per-tag 0.6.x direct/chained migration fixtures, concurrent/failure tests, documentation audit.
- [ ] Host/default/hostsim/shipping checks, ARM build and actual IPK metadata verification.
- [ ] Package upgrade preservation on older dev TV and reporter's newer webOS (restart alone insufficient).
- [ ] 0.7 integration preserving profiles, PIN, last_library, auto_sign_in and current consent scopes.
- [ ] Release preparation/publication under cut-release workflow, only after required gates.

## Scope of assurance

Forward upgrades from every published 0.6.x, directly to the patch/0.7 or through the patch.
No downgrade, uninstall/reinstall, filesystem rollback, physical data loss or lost-key guarantee.
No new data collection or consent-scope expansion. Do not change existing settings-reset policy.

## Evidence so far

- Queue staging currently passes 10 focused host tests: typed cross-domain FIFO, off-thread
  execution, nonblocking Full rejection without running rejected work, worker disconnect,
  dropped-ticket durability, explicit startup refusal, and zero-capacity coverage. The executor
  remains test-staged until Session and Consent consumers wire the shared production instance;
  this is not async production or device evidence.
- Previous issue-76 test build proves app-local restart on the reporter's Lite 11.2; the earlier
  isolated dev-TV probe proves upgrade preservation only on older firmware. Neither proves this
  new migration or the newer firmware's package upgrade.

### Published 0.6 fixture matrix

`tests/fixtures/persistence/manifest.json` records the exact source commit for v0.6.0 through
v0.6.5 and the consent fields each tag shipped. The companion synthetic fixtures cover Session
credentials/settings/pins/recents/source metadata, consent Yes and stored No, and secure-envelope
v1 with identity absent before 0.6.3 and present afterward. They contain no real credentials,
identifiers, addresses, or device output.

The child `session::migration_tests` matrix (when enabled by the session module) proves direct
legacy-to-canonical migration, exact opaque payload transport, canonical reopen, routine consent
rewrite, stale legacy copies after clear, and the secure-envelope shape. It does not claim an
old-binary/0.7 executable guarantee, package-upgrade preservation, key-manager cryptographic
validity, or TV behavior; those remain separate 0.7/device/IPK acceptance gates.
