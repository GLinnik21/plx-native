# Owned presentation damage

`Dispatcher::present` is the sole presentation owner. Content changes reach it as
`PresentEvent::Damage(Provenance)` through an owned `PresentHandle`, a machine's
`Effects::invalidate`, or the app runner's addressed result-landing boundary.
`Present::note_in_scope` addresses one Page or Surface effect without changing the
scope of subsequent work. Mutation-only caches never publish damage or worker wakes.

## Source inventory by semantics

- **Input/navigation/lifecycle:** the app ingress and dispatcher execute typed effects.
  DecisionAlert, DocumentReader, Popover and tab-pulse setters do not independently
  publish ambient damage. Their owning input/navigation effects select the first frame;
  their Tick integration reports ongoing animation. Login's tick-time alert content
  reconciliation and abrupt visibility changes explicitly invalidate through `Effects`.
- **A long hold's stale host card:** `Press::tick` returns a page-change result only
  while a taken long hold visibly moves a card still painted into the host snapshot.
  `InputMachine` turns it into `PressEvent::PageDamage`; the dispatcher applies Page/Input
  damage. A tap or a lifted card produces no page-damage effect. This is not merely motion:
  open-menu snapshots must refresh when the underlying card still belongs to the page.
- **Display-only resource replacement:** `ArtCycle::set_frames` returns whether its
  retained frames changed; HudSettings applies Resource/Texture damage to its owner.
  Test-pattern setters return success to the app ingress. RuntimeWarning returns its
  presentation demand to the runner. Neither setter touches a global presentation flag.
- **Texture results:** texture upload/landing boundaries apply Resource/Texture damage.
  Worker completion requests scheduling only; resource arrival is attributed on the UI
  thread, not by the worker writing a global dirty generation.
- **Store results:** stores return changed outcomes/generations and addressed result
  events. Their Bridge/runner boundary owns presentation effects; cache/store mutation
  is not itself a presentation notification. Admission refusal and terminal failure
  still need their owning boundary observed, even when there is no successful payload.
- **Frame animation:** springs, marquee travel and holds, cross-fades, skeleton sheen,
  art arrival, scroll-chrome ramps, and Up Next's countdown use explicit frame-captured
  motion infrastructure. A discontinuous spring jump is captured separately from a
  continuing animation, so an already-resting jump does not owe another settle frame.
  Frozen draw decorations report nothing. Card-speed sampling may request another
  animation observation without asserting resource damage or worker provenance.

## No compatibility damage adapter

`motion/report.rs` contains no `invalidate`, `take_damage`, `select_damage`,
`present_dirty`, `take_page_damage`, `OwnScope`, or global damage counters. Its
thread-local cells are the explicitly captured frame-motion collector, not a damage
mailbox. `MotionScope` still attributes page/surface animation; it cannot classify content
changes. Main-thread deferred rendering addresses its own Presenter through frame effects;
only actual off-thread completions call `present::wake_from_worker()` directly.
Main-thread deadlines and deferred work use owned `PresentEvent::Wake`: it requests a
frame without content provenance, page damage, or page motion.

Popover host capture uses only `Present::page_damaged()` for content attribution.
Surface damage cannot refresh the frozen host page. Motion and damage reported during
draw are merged before swap acknowledgement; acknowledgement retires only generations
captured at admission. Late draw effects therefore survive the actual swap.
`changed_after_admission` observes those late generations without confusing an owner's
pre-admission scheduling request with new work.

## Ordered app commands and UI acceptance

An external app command yields at its position in the dispatcher's FIFO. Its sender,
mounted lifetime and frozen ReturnState travel with that command; a terminal receipt
resumes the same drain without letting the tail overtake it. Admission is not an async
completion: a later result still needs its addressed owner and scope/epoch fences.
Neither successful execution nor a worker wake alone grants page damage. The landing
boundary applies damage to the actual Page/Surface owner when visible content changes;
refusal, cancellation, stale ownership and failure must not fabricate a successful payload.

The current registry-derived scenario inventory and its per-scenario evidence status are
in `.empryo/reviews/project-ui-architecture-audit-2026-11-13.md`, section Step-7.
Source inventory completeness is not runtime acceptance. Keyboard, pointer, long hold,
back/restore, profile replacement, async arrival, failed/refused results, settled idle
and reopening require separate observations, including surfaces over their real host.
A screenshot request itself requests a frame; that image cannot prove settled idle.
Controlled replay proves only the recorded frames accepted by the current codec and
shape. Preserve a refused historical fixture's raw nonzero result, never rewrite its hashes.
An approved historical FIFO-scheduler refusal needs a separate compatibility oracle:
exact typed reason plus early-no-execution assertions, with negative controls rejecting
unexpected refusal or failure. This is not supported-recording SAME acceptance. Fresh
CURRENT Filmography dynamic-Content recordings must replay in targets and resolve modes;
postfinal captures must identify the actual integrated binary, not an earlier build.

Simulator captures must be opened at a 1920×1080 drawable, with an isolated RuntimeDir
and mock PMS/Discover stand-in. Simulator FPS is not a UI correctness oracle. Cached
modal ground, device text rasterization, video-plane composition and platform root/home
remain unverified until separate TV evidence is collected under `tv-lock`; lack of that
evidence is a limitation, never a passing skipped test.

Step-8's isolated replacement compiler/source results do not upgrade any scenario status.
Feature checks pass, but full default/hostsim runs retain a media timing failure and supported-Python
source gates retain proof/shape/selftest failures. The replacement leaves original HEAD, staged
entries and tracked bytes unchanged; raw index hashes differ and are reported, not reset.
See `.empryo/reviews/step8-recovery-result-2026-11-14.md`; the earlier isolation incident remains
historical evidence. Missing simulator/idle/native evidence remains missing, not a passing skip.
Step-7's single user-authorized WSL restart briefly restored `use_gfxredir=1` and
D3D12 acceleration, but transport subsequently reverted to `0`. Twenty opened
postrestart captures lack a per-capture transport timeline and remain diagnostic,
not accepted evidence. The retained CURRENT Filmography tape proves typed navigation
and focus restoration offline; its targets/resolve roundtrip is still outstanding.
Exact tape/profile hashes, raw legacy nonzero results and separate reason-specific
compatibility checks: `.empryo/reviews/step7-final-evidence-2026-11-13.json`.
Full registry/runtime/idle/native acceptance remains open; see the recovery-result report.

## Regression coverage

Independent Present owners, typed page/surface capture, mutation-only caches without a
global worker wake, long-hold page effects, scheduling without UI provenance, skipped
swaps, and post-admission damage/wakes have host regressions. Original idle, press,
marquee, tab-pulse, art-arrival and host-cache regressions remain; tests observe the
owned page-damage readout where the contract is content damage, and frame motion where
the contract is animation. Host results do not prove device rendering or performance.

## Addressed deferred rendering

GPU field sampling returns `FrameLatch::Pending`; it never schedules through a global door.
The visible `DrawFrame` records `request_frame()`, and the dispatcher consumes that frame-local
effect as `PresentEvent::Wake` on its own Presenter. Settings forwards child requests even when
a leaving child contributes no hit stops. Recording/discovery/blur-source walks cannot request frames.

`GlDims` borrows the owning `PresentHandle`; load-dial preparation receives its Presenter explicitly.
Both schedule only that owner, without page damage or page motion. `report::wake` is removed,
and actual IO completions name `present::wake_from_worker` directly. The `owned-wake` source gate
rejects use of the worker door from UI, dev-frame logic, or the main loop.
