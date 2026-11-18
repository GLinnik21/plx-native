# Product screen extension whitelist receipt — 2026-11-16

Scope: isolated uncommitted source repair on `990d0439444c7e8180fe6437e95fdcd9136daae9`.
No commit, push, PR, device operation, installation or provider change. Prior source
`6007dd7b85dc200d2417bba6219ee1f07fe78104225b2cfcab803d0dde28222e` and its fullmake420/V13
runtime results are supplied historical evidence, not a remeasurement of this repair.

## Contract and production repair

[Specification §0.5](ui-restructure-spec-v4.md) permits ordinary screen registration, not a new
resource-domain command. The conventional module declaration adds `screens/src/lib.rs` to the
ownscreen/registry production footprint; scenario and manifest edits are needed only for new public
scenarios. A new IO/resource command legitimately needs an app executor/adapter; ordinary screens
instead use the existing `Fx::Nav` and `AppFx::Store` contracts and acquire no IO authority.

`screens/src/registry.rs` now owns `RouteProbe { word, kind }`, the data-only `ProbeKind`, and the
exhaustive `AppArg::route_probe` declaration. Ordinary arguments explicitly select `Generic` in that
registry. `app/words.rs` delegates; the frame probe in `app/run.rs` dispatches the closed descriptor,
not AppArg. The generic projection in `app/bridge.rs` reads this mounted Screen's existing logical
probe plus actual engine group/element, without Any, a callback registry or a new screen downcast.
Legacy schemas remain explicit and existing recorded words, focus bytes, Canon and shape pins are
unchanged. Non-page surfaces do not masquerade as Home. No AppFx case/classifier/runner gate changed.

## Executed permanent compiler and lifecycle proof

Portable generator: [tools/prove-screen-extension.py](../tools/prove-screen-extension.py).
It copies current product inputs into a private overlay, adds a genuinely new `ReviewStatic`
argument, one-control Screen and registry registration/shape, and compiles the actual application
crate. Separate private test roots preserve normal module/relative-fixture paths and append only
acceptance tests. They are harness infrastructure, not extra product source edits or a shipped
feature/test variant. No product fixture or tape is overwritten. Cargo artifacts are copied once
into an isolated private target; no concurrent writable target is shared and no Git metadata or
child fixture commits are copied/exported.

Executed with clean `env -i`, Rust1.100.0, Python3.12.16, two cargo jobs and one libtest thread:

| Oracle | Actual result | Product Rust source footprint |
|---|---|---|
| Frozen original words/run with fresh variant | RED exit101; exactly two E0004 non-exhaustive-pattern errors, solely words.rs and run.rs; no other compiler error | ownscreen, lib, registry, words, run: five |
| Repaired application with identical fresh variant | GREEN exit0; one exact acceptance test passed, 1437 filtered, zero ignored | ownscreen, lib, registry: three |

The successful final oracle uses real scripted Down input to move the dispatcher engine from the
shared Back control into the new screen's group37/element16385; there is **no focus seeding**. Its
exact probe is `focus route=review_static group=37 elem=16385 extension=fired press=0`.
Real Right input emits the existing Metadata Clear and generic Nav Push(Search). Metadata actually
clears; real navigation Pop restores the same new entry and its focused element/state, then Pop
restores the original Home entry. No FixtureHost, Home replacement, synthetic mounter or resource
command is involved. Actual file hashes and measured red5/green3 footprints are asserted by the
program, not inferred from method/line counts. The original unregistered-factory regression remains
unchanged and is not relabelled as this proof.

Successful oracle source SHA256 (sorted path + NUL + binary file SHA256; symlink target bytes):
`bb8514c7a001d6869000b7ae069e6f23c892623abb90a87b2934d05c7eeb9000`,2572paths,
identical before/after with a fresh path enumeration. Historical baseline file hashes:
words.rs `ed882850b6db1eab2383c7b480c67ad3db57fae96dcf4735a1615547b8b2ee59`;
run.rs `0df3bf76a34c8f04d677e5e1d7cd2ed37f71a6bc3acb868add5e9d9b42fe4095`.
Generated source hashes: ownscreen `b7f6b6d6d1f688099aebc8c647a2745809d51ce705d64149773ac8569f1dd6c2`,
lib `ed19987dac200be89093745b71980e8da1b08e3592873049a8bd924e4cf05215`,
registry `a1686903a7a7071055785bc51449d285b8882c532ae359ee8954aa57493f8cbe`.
These generated hashes are from the final no-seed oracle. The private receipt retains per-file
input hashes and exact commands; public matrix/document edits followed this compiler checkpoint.

Re-run current GREEN without any private baseline dependency:
`python3.12 tools/prove-screen-extension.py --output /tmp/new-screen-proof --seed-target rust-modules/target`.
The optional `--baseline-app` accepts frozen original words/run for the historical compiler RED;
`--resume` reuses only that proof's isolated artifact copy after an unsuccessful attempt.

Early scaffold failures were not compiler or acceptance passes: two ambiguous text anchors, then
module-directory and relative-include lookup failures. An initial cursor-free screen expectation
also failed because the engine correctly focused shared Back rather than None. The final fixture
has its own real control and exercises real direction input; assertions were not removed.

## Current joined verification

Default actual application suite:1437 passed,0failed,0ignored (71.51s libtest). The new regression
pins all14 historical page descriptors and exact legacy Login/Profiles/Onboard bytes; all eight
legacy content formatter schemas compare byte-for-byte against the shared content projection.
Existing route, recorder, focus/hit identity and product-extension factory tests remain unchanged.
`ci/test_ui_boundaries.py`:32/32 PASS, including planted direct, helper-wrapped and aliased external
AppArg ladders plus legal partial resource queries and descriptor dispatch. The source gate scans
possibly-production code rather than exempting developer feature arms.

The static-only433 proof above remains valid for its scope. A subsequent same-whitelist finding
was that the manifest validator still obtained its page census from an application-local fixed14
array. The census now lives in the allowed registry (`every_page_arg`); words delegates, and the
legacy first14 append-only registration samples retain their exact golden words/schemas. New
source controls reject both an external array and a helper wrapper. The permanent proof also has
`--public-scenario`: its private overlay adds only the permitted dev/scenarios.rs + manifest.json,
wires a real trigger/helper to the new route, calls that same helper in the actual AppHost test,
and runs the existing manifest route-word validator. New static/public-scenario focused results
are recorded separately; the earlier proof is not relabelled as public-scenario evidence.

Frozen chain434 completed only32 source controls with stable a0570 source/HEAD/index; it was
cancelled during the source gate, before shipping, lint, fullmake or simulator build. Cancellation
is not a source-gate or full-suite pass. No fullmake invocation was consumed. The parent will adopt
the post-census frozen chain after focused compiler proofs; no final acceptance is inferred here.

Post-census focused chain435 completed:33/33 source controls PASS; static compiler RED101 with
exactly two original words/run E0004 errors, then actual AppHost GREEN1/1 (measured5→3 files);
public-scenario compiler likewise RED101 exactly those two errors, then GREEN2/2, including the
existing `every_manifest_route_word_is_one_the_heartbeat_prints` test (measured7→5 files).
The latter five are ownscreen/lib/registry/dev-scenarios/manifest; no external words edit is part of
the overlay. Both modes reenumerated unchanged product inputs. The earlier1437 default suite is
pre-census evidence; a final full-suite rerun remains pending, not silently inferred from these
focused results. No focus seeding, FixtureHost or replaced Home was introduced.

Final inclusive focused-proof source SHA256:
`cd719c45c2a66b700b17259343e2d7e8271f2603729e64839617e9541b974aaf`,2573paths.
Final frozen-gate source (excluding only this generated receipt and the prior generated modal
verification receipt): `434411c931ec55d70bb8a88862ccc12cdcee9a75ec44f898436e40373f2e9e4f`,2571paths.
HEAD remains990d; raw index SHA256
`11f858f7b6fc7c98d179c597b8dd2504dfdff6cda7998aa5f798f72b2351818d`;
staged manifest SHA256 `d68d0474378257b4d1c0e41168c17eacd20c7bb0e63f40614bc4f04d0ffea6ed`
uses literal LF-framed `git ls-files --stage` (different framing from older receipts, not an index
mutation); cached diff empty. Final generated registry hash
`10193a3b914861684250a525111a27035414ff5b5225b89a21b045a34dd843e0`,
scenario hash `10f95d192cbb6e12e8fac87594ef3ca4cc6f61d1766e6a03d957fa698187dd8b`,
manifest hash `83ad02372c0c14a8cfcba8040eed7748a18a21ed00619af39395cd3e3d1628e8`;
ownscreen/lib hashes remain as above. V13 producer/schema constants were not changed by metadata
or the test-only census move; no fingerprint/certificate pin was refreshed.

## Final post-census frozen gates and rebuilt release binary

The single final sequential chain completed on the exact frozen source above. All six actual
step exits were zero, every before/after identity matched, and the six steps shared the same
source/HEAD/raw-index/LF-staged-entry identity; cached diff remained empty. Only this generated
receipt and the generated modal verification receipt were excluded from source hashing.

Environment: WSL Ubuntu22.04, explicit `env -i`, existing uv Python3.12.16 and Rust1.100.0;
`CARGO_BUILD_JOBS=2`, `PLX_TEST_JOBS=2`, `RUST_TEST_THREADS=1`, `PLX_TEST_THREADS=1`,
`PLX_CHECK_STEP_JOBS=2`, no exported `GIT_*`. No installation or shared Git-metadata write.

| Actual command | Result | Seconds including post-step identity capture |
|---|---|---|
| Source boundary unittest discovery | PASS33/33 | 34.94 |
| `bash ci/check-deps.sh` | PASS, all source/layer gates green | 141.64 |
| `make check-nodef` | PASS | 226.38 |
| `make lint` | PASS, exactly the three repository-named shadowed-branch lints | 55.62 |
| One `make check` | PASS; cargo/python/rest/harness branches individually green;139/139 manifest steps green | 974.46 |
| `make sim-linux SIM_TDIR=rust-modules/target-sim` | PASS, actual fresh release build | 472.04 |

Current fullmake default app1437/0, screens2178/0, UI1661/0, appkit955/0; hostsim app1656/0
and feed352/0. The actual745-test harness passed with4 skips. Across the34 Rust result-summary
rows (including nested control invocations), the log reports22402 passed,0 failed,239 ignored;
Python unittest summary rows report133 skipped. These are observed log totals, not unique-test
counts or a claim that ignored tests ran. Expected planted failure text inside passing negative
controls is not a failed outer branch. Earlier1437 results above remain pre-census evidence;
this invocation independently reexecuted the current app1437 suite.

READY_BINARY: `rust-modules/target-sim/release/plxnative-sim`, release default+hostsim,
24154712bytes, freshly measured SHA256
`6852e76f2a99d568b96558754ab00da7221ff283071127bcb67575047734308c`.
The parent was handed this exact frozen identity/path/hash after successful build for a separate
fresh renderer run. No renderer execution is claimed by this gate receipt. Native/ARM, actual TV,
glyph, idle, visual and panel-on performance remain untested here; full source-architecture
acceptance still requires the independent seven-contract review and cannot be inferred from tests.

Invocation accounting: an initial wrapper stopped before gates because it accidentally parsed
2571 (the path count) as part of the SHA. A second preflight completed only33 source controls,
then was stopped between steps to replace inherited shell environment with explicit `env -i`.
Those preflights consumed no fullmake; their checkpoints and the earlier canceled434 checkpoint
were preserved. The final clean chain above consumed exactly one fullmake. No production,
test, policy, fixture, golden or schema source was edited during verification.
