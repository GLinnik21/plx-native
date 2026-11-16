# Owned UI runtime review: publication and acceptance

The seven bounded source-contract repairs passed independent review on 2026-11-16.
[Public contract matrix](ui-review-evidence.md), [product frame protocol](ui-review-frame-protocol-2026-11-16.md),
[extension proof](ui-review-extension-verification-2026-11-16.md), and
[owned-policy evidence](ui-review-modal-verification-2026-11-16.md) describe the boundaries.
Earlier checkpoints retain their original source identities and are historical, not the current verdict.

## Reviewed source before integration

Base: `990d0439444c7e8180fe6437e95fdcd9136daae9`.
Frozen source SHA256: `434411c931ec55d70bb8a88862ccc12cdcee9a75ec44f898436e40373f2e9e4f`
(2571 paths, excluding only the two generated receipts named in the extension evidence).

All six sequential gates passed with unchanged source, HEAD and index:
33 source controls, dependency/layer guards, shipping feature check, named lint,
one complete `make check` (139/139 manifest steps), and a fresh release simulator build.
Actual default app1437/UI1661/screens2178/appkit955 and hostsim app1656/feed352 passed;
the 745-test harness passed with four skips. Skips and ignored cases are not execution.

The rebuilt release simulator measured 24154712 bytes and SHA256
`6852e76f2a99d568b96558754ab00da7221ff283071127bcb67575047734308c`.
A separate fresh actual-renderer V13 Filmography/Detail/Back/Back run passed strict Targets
and Resolve replay: each exited zero, exactly one SAME, frames=graded=589, all eight
counters zero, first_difference=null. Privacy admission, retained focus and ReturnState
assertions passed. Full source/HEAD/raw-index/staged/binary/tape identities stayed unchanged;
owned processes were stopped. This is llvmpipe logical/runtime evidence, not ARM/TV,
active Player presentation, glyph, idle, visual or panel-on performance acceptance.

## Publication boundary

A separate task branch carries only the reviewed semantic patch and its public proof files.
EOL-only churn, private reports, job logs, build output and device recordings are excluded.
Historical fixture/manifest bytes remain unchanged. Concurrent main changes are integrated
by an ordinary merge, never by replacing main with the reviewed old tree.

Integration retains main's newly captured Home fixture byte-for-byte as historical V12 evidence.
Its recorder preflight initially failed with StateShape (recorded13512213413816145485 versus
current16894190109976774341). The expectation now pins that historical fingerprint, just like
the other historical captures; the assertion that current V13 refuses it remains enabled.
No tape, codec acceptance rule or golden value was rewritten to make current replay pass.

Post-integration local gates and exact-head public CI are still pending at this checkpoint.
The PR remains draft until the independently coordinated exact-head native verification
and all applicable CI checks pass. No native result or landing is claimed by this receipt.
