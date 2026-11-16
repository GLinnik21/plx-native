# Dispatcher-owned product frame checkpoint — 2026-11-16

Base: isolated `990d0439444c7e8180fe6437e95fdcd9136daae9`, uncommitted. Earlier verified
player/outcome/codec repairs remain intact. This checkpoint supersedes only the top-level-frame
ownership finding, not the earlier receipt's source hash or whole-check result.

## Demonstrated cause and repair

The regression `product_loop_has_one_dispatcher_owned_platform_protocol` failed on the old
product loop: `product loop still sequences native phases outside the dispatcher owner`.
The dispatcher previously stepped the logical tree, while the App independently sequenced IO,
publication, resource preparation and swap acknowledgement. Empty Bridge hooks did not own those
native phases. `FrameRuntime<H>` now explicitly lends each resource port to the dispatcher-owned
protocol, without importing App into UI/machine, globals, unsafe borrow inversion or RefCell.

The runtime holder's unsafe calls are the existing native IO operations, not a new resource alias.
The protocol's phase state rejects duplicate logical steps; the existing dispatcher algorithm
retains the PRE/POST limits, one navigation commit and one publication continuation. The one gate
owns admission and acknowledges only after the native draw/swap callback reports completion.
Blocked and non-returned swaps retain damage/wake. Native preparation is never invoked blocked.

## Phase contract: preserve visible timing rather than silently imposing the schematic order

| Boundary | Before | After |
|---|---|---|
| IO facts | pump, ingest, playback/clock/results, publication before logical input | same order, explicit owner callbacks |
| Logical frame (§3.3) | input/results, containers/pages Tick, timers, PRE drain, nav commit, POST drain | same dispatcher algorithm, exactly once |
| Post-commit native work | content requests/update and latest HUD/resource publications | same point; continuation spends only remaining POST |
| Admission (§8) | App took gate snapshot after all publications | dispatcher takes snapshot at that same boundary; native lifecycle/backbuffer port may refuse |
| Preparation | screen prepare then native uploads under one opened budget | same order/budget, only on admission; empty Bridge preparation not invoked |
| Draw/swap | native draw, capture, straighten, actual swap, accounting, App ack | same native IO order; dispatcher ack immediately after returned callback |
| Opaque region | every iteration after prepare, including blocked | same unconditional branch coverage, before draw |

`NavCommit`/`TickDrain` heartbeat stamp names historically bracketed additional native work in
App, not the dispatcher's internal Tick-before-commit order. Those stamps and script/UX timing
are preserved; callbacks do not add a second Tick or commit to make the prose appear true.

## Verification scope

The actual AppHost trace fixture executes native pump/results, the production step/command runner,
and production publication continuation. It substitutes deterministic SDL input and GPU IO only.
It covers same-frame setup/input facts, resource arrival, the exact final POST prefix/carry,
no second nav commit, ordered once-only callbacks, blocked frames and a swap that does not complete.
The token-based source guard has negative controls for escaped phases, duplicate/reordered
callbacks, bypassed admission, unconditional acknowledgement and comment/test-only decoys.
No new test modules, cap constants, allowlist entries or disabled checks are introduced.

Focused results and the frozen source identity are recorded in the handoff. The previous full
`make check` receipt remains historical evidence for its own source, not this modified tree.
The final whole check belongs after the remaining review nodes, not this checkpoint.

## Separate remaining work

Bridge/AppFx decomposition is intentionally not combined with this change. Ambient modal/hold-hint
logical TLS, modal phase ownership, and new-screen/type ergonomics acceptance remain open. Host
verification does not establish TV/native GPU, ARM linkage or visual/performance acceptance.
No commit, push, merge, device operation or parent-checkout source edit is part of this checkpoint.
