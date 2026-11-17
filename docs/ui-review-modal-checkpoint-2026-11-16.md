# Modal / hold-hint review checkpoint — 2026-11-16

Work remains an uncommitted patch in the isolated review tree on base
`990d0439444c7e8180fe6437e95fdcd9136daae9`. This checkpoint is not a verification receipt.
The prior typed-routing source identity `6ac95351369f3f70fd57c31eb8aa46be8dbf78bda2c25ab14379964ca5e5bb4e`
describes the starting checkpoint, not the source being edited here.

## Demonstrated cause

Before repair, the actual Rust regression
`containers::tests::two_dispatchers_keep_their_modal_presentation_on_the_same_thread`
constructed two independent Dispatchers on the same thread. A opened a Compact surface;
its veil was true. Stepping resting B replaced the TLS veil with false. The assertion
`stepping B must not replace A's modal presentation` failed, left false / right true.
`test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 1637 filtered out`.
Serializing threads cannot repair this same-thread cross-owner authority.

## Implemented ownership boundaries

* ModalStack remains the sole phase/input/host-policy owner. `ModalRead` is a derived
  immutable step/render input on Cx, not another phase model. The veil publisher is removed.
* `ModalPresentation` carries the captured readout and an owner-addressed GL snapshot key.
  Native host begin/page passes take it explicitly. Bridge no longer mirrors surface lifetimes
  into process counters; Popover motion cannot publish host policy.
* Context-bound GPU resources remain render caches. Their key separates runtime identity even
  when both Dispatchers mint the same EntryId, and includes the cached-layer signature so
  dismissal invalidates the old composite. Cache keys never answer logical policy questions.
* Hold learning, install preference persistence and strict current recording contracts are joined
  and executed by the actual UI/app suites; final gate identities are kept in the public receipt.
* The additional `LIFT_OWNS` logical publisher was removed. Input owns gesture/key/instance-bound
  render evidence, admitted only from an actual visible page walk; Press receives explicit policy.
  The original two-Dispatcher long-hold PageDamage counterexample now passes unchanged.

## Owned navigation page-image repair

The former `PAGE_IMAGE_HELD` TLS writer in `Dispatcher::draw_with` and all five production
readers are removed. `Cx::page_image: PageImageRead` carries an immutable projection of the
Dispatcher's actual `PageImage` and the current mounted navigation entry/instance. Landing
held/step, Shelf tick, Library dormant and Recommended unseen decisions take that input.
Generic cards import neither App nor the screen registry; input publishes no page policy.

The compiled same-thread regression `another_dispatchers_live_draw_cannot_step_a_held_owners_landing`
failed before repair: A captured Held, B drew Live, then A's actual landing ghost alpha advanced
to `0.46615097` instead of remaining `1.0` (0 passed / 1 failed). Source/HEAD/index were stable
through that executed RED. The unchanged ghost assertion is retained as the repair oracle.

Each step revalidates captured entry AND instance against the live tree, including addressed
post-commit deliveries. Constructors/staged bodies start neutral; a remount at the same entry
rejects old-body pixels. At the dip floor a DIFFERENT committed entry is still hidden by this
owner's outgoing image before its first capture draw, so its Enter/first Tick receives a hidden
projection of the current navigation identity, not permission to draw outgoing pixels. A second
actual Dispatcher regression reproduced that phase gap RED before the correction (0 passed / 1
failed); existing Recommended BACK tests retain their unchanged release/band assertions.
Replacement capture remains held for
the following tick; the next visible Live plan releases it. Failed capture produces no held
identity. Reset clears owned image metadata. Source/discovery/recording/prewarm walks cannot
publish or release it; renderer eviction or another owner's GPU frame cannot change policy.
No retained logical latch is added, so Canon/STATE_SHAPE remain unchanged: this is derived
from the existing owned image state, not an excluded new mutable policy cell. Capture identity
metadata prevents a new body at the same EntryId from reusing old pixels. Existing freeze,
prewarm, placeholder debt, present scope and rest-frame transport remain in the draw protocol.
Final acceptance depends on the joined executable gates in the [public verification receipt](ui-review-modal-verification-2026-11-16.md).

## Verification boundary

Both page-image counterexamples are compiled/executed RED then GREEN, including the unchanged
Recommended BACK release/band assertions. UI1650/0 and app1429/0 passed after the phase correction;
appkit955/0 and screens2175/0 passed in the joined inventory. Exact final sourceguard, shipping,
named lint, full-check and fresh renderer outcomes are recorded separately in the public receipt.
No historical fixture is changed. No TV/device or hardware rendering/performance evidence is
claimed. No commit, push, publication or parent-checkout source operation has occurred.
