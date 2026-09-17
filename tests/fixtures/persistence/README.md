# Published 0.6 persistence fixtures

These fixtures are synthetic, deterministic representatives of the JSON shapes that were shipped
by each published 0.6.x tag. They contain no real Plex tokens, IDs, addresses, key-manager output,
network data, or device paths. `session.json` has representative settings, pins, recents, source
metadata, and placeholder credentials; the consent pair covers an opt-in and a stored No.

The fixture manifest records both each exact tag object ID and its peeled source commit used to
inspect each schema. On 0.7 the matrix runs the direct legacy-to-canonical import and a
canonical reopen through the shipping DB8 helper backend (with a synthetic DB8 RPC) and the host
store. It does not prove package upgrade behavior on a television; that remains device work.

`generated/` holds bytes written by the tagged releases' own code rather than by hand; see its
README.

Secure-envelope fixtures are shape-only v1 envelopes. Tests assert byte-preserving transport and
the migration matrix exercises an unavailable key-manager path with `arm_for_test`; existing
session tests cover readable/fresh-reauth branches. These mocks never claim device-key behavior.
