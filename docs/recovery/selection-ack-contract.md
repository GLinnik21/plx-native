# Approved prerequisite: correlated profile selection acceptance

Parent read /tmp/codex-p12-session-selection-ack-proposal.md completely and actual UI runtime
RED /tmp/codex-p12-session-ui-red-profiles.log (38 passed, 3 failed). Inspected all three tests.
The carried-command test emits real UI SelectProfile then ticks the unchanged read. The other
two use seeded local pad state and controlled publications; they demonstrate indistinguishable
old/new reads, NOT full production dispatcher evidence. Production carry proof remains owed.

APPROVED exact additive API from the proposal:

- SessionCmd::SelectProfileWithReply { index: usize, pin: Option<String>, reply: ReplyTo }.
- SessionSnapshot.flow_epoch: u64, derived from the existing SessionInit.epoch, included in
  borrowed publication equality/construction; no new epoch allocator or copied authority.
- AppMsg::SelectionReply { correlation: u32, accepted: bool, flow_epoch: u64 } through a typed
  SessionFx and normal addressed ScreenEvent::Async(RequestId(correlation), message).
- Existing core/dev SelectProfile stays unchanged. Both commands invoke the same transition
  once. Every accepted selection gets its actual checked full-width epoch, including same-user
  fast Ready. Rejection is finite and never implies successful authentication.

UI must retain its request correlation and acceptance state before setting submitting, ignore
old reads while command acceptance is unknown, bind accepted E, wait on reads older than E,
consume only matching E, and discard superseded E when a newer flow is observed. A result may
already be in the retained read when its acceptance ACK arrives: do not require observing
Switching or false pin_denied. Match both async RequestId and payload correlation; stale,
duplicate, foreign and late-after-pad-close replies cannot settle another local request.
Checked UI correlation exhaustion emits no command and must not strand a submitting spinner.
No fresh counter, timing workaround, second reducer, global facade or recursive stepping.

Fresh first paint remains clear. A stale-denial suppression rule must be keyed to the relevant
flow identity rather than waiting for an intermediate false read. Local pending/accepted epoch
and any remaining denial-gating decisions belong in canonical screen state. Epoch comparisons
are ordinary full-u64 comparisons because allocation is checked and never wraps.

## Implementation split and ordering

Boyle owns owner/Snapshot/SessionFx/AppMsg/Bridge API implementation and its core tests. Reserve
Socrates' exact registry mounter-bound/constructor/DismissPinError locations as before. Produce
a coherent immutable core checkpoint containing this API, and report the SHA before UI imports
it. Existing core changes can be checkpointed with it if required; do not reset/discard them.
Socrates owns screen logic/tests and local fixtures. Until the API checkpoint arrives, continue
independent tests/docs or prepare changes, but do not manufacture local API substitutes.
Parent will import the named core checkpoint into the UI branch without overwriting its dirty
work (commit UI WIP before normal merge). No raw working-history push.

Acceptance: actual UI command emission and unchanged-read carry; fresh old denial vs fast new
denial; acceptance/refusal/checked exhaustion; fast result before ACK; stale/duplicate response;
pad close/reopen and superseded flows; canonical differences for decision state. Core must prove
production Bridge addressing and normal pre/post-budget carry, epochs above u32::MAX, accepted
fast path and rejected/exhausted paths. Existing auth/PIN policy tests remain unchanged in meaning.
If the exact protocol has a concrete counterexample, report before implementing a workaround.
