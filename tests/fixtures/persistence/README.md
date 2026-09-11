# Published 0.6 persistence fixtures

These fixtures are synthetic, deterministic representatives of the JSON shapes that were shipped
by each published 0.6.x tag. They contain no real Plex tokens, IDs, addresses, key-manager output,
network data, or device paths. `session.json` has representative settings, pins, recents, source
metadata, and placeholder credentials; the consent pair covers an opt-in and a stored No.

The fixture manifest records both each exact tag object ID and its peeled source commit used to
inspect each schema. The matrix is a
direct legacy-to-canonical and canonical-reopen check for this maintenance line. It is not an
old-binary compatibility guarantee, does not execute a 0.7 build, and does not prove package
upgrade behavior on a television. Those gates remain the documented device/IPK work in
`docs/persistence-plan.md` and are reused when the same adapter lands on 0.7.

Secure-envelope fixtures are shape-only v1 envelopes. Tests assert byte-preserving transport and
the migration matrix exercises an unavailable key-manager path with `arm_for_test`; existing
session tests cover readable/fresh-reauth branches. These mocks never claim device-key behavior.
