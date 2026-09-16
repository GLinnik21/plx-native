# Fixtures written by the published releases

Every file here was produced by the tagged release's own code, run as a host unit test in a
detached worktree of that tag. `generators.patch` is the complete set of test-only additions that
were applied to produce them; nothing else in those trees was changed. All values are synthetic
(documentation addresses from RFC 5737, placeholder tokens and identifiers).

| File | Written by | How |
|---|---|---|
| `v0.6.0-errors-yes.consent.json`, `v0.6.0-no.consent.json` | `v0.6.0` | `consent::apply` then `telemetry::record` |
| `v0.6.0.session.json` | `v0.6.0` | `plex::session::save` (host plaintext form) |
| `v0.6.5-errors-yes-declined-extension.consent.json` | `v0.6.5` | the 0.6.0 Yes file loaded by 0.6.5, extension declined with `apply_extension`, then `record` |
| `v0.6.5-both-yes.consent.json` | `v0.6.5` | fresh `apply(true, true)` then `record` |
| `v0.6.6-json-store/` | `v0.6.6` | the 0.6.0 session and the 0.6.5 decision migrated by 0.6.6's host `session::load` and `telemetry::persistence::load` |
| `v0.6.6-db8-record.json` | `v0.6.6` | 0.6.6's storage-helper backend (ACL tier, webOS 4) committing `SessionComplete` and `ConsentComplete` for the same data; the DB8 object it put |

To produce `v0.6.6-db8-record.json` the generator mounts 0.6.6's `storage_service/backend.rs` into
the library test build, which needs one import path changed (`crate::state` to `super::state`) as
0.7 does. The patch shows it.

Regenerate from the repository root with, for each of `v0.6.0`, `v0.6.5` and `v0.6.6` in that
order: `git worktree add --detach <dir> <tag>`, apply that tag's part of `generators.patch`, then
`PLX_FIXTURE_OUT=<out> cargo +nightly test --lib plx_fixture_gen`. The DB8 record carries random
operation and auth-generation IDs, so a regenerated file is equivalent rather than byte-identical.

These are host-produced. Secure-envelope contents and a real television's DB8 cannot be produced
off-device; a package upgrade on a set is still a device check.
