# Parent decision: bounded Session transfer and owned FIFO

Approved against `/tmp/codex-p12-session-owner-deferral-proposal.md`, based on immutable
checkpoint fcfe1ae88226a48fda8c7a7a8c29814617e25c30. This supplements the frozen R2B-P brief;
the full physical-owner scope and proof obligations remain unchanged.

## Accepted design

- Keep existing Landing limits 64 data / 32 per-owner / 32 total reservations.
- At most ONE outstanding transferred batch, containing at most 96 distinct receipt-bearing
  records. Adapter holds receipt metadata only. Owner inbox, active commit and records still
  carried by the dispatcher share those 96 transfer credits. Acknowledge exact
  `(arrival, full Addr, key)` only after actual processing/discard; no next batch before all
  unique credits return. Use named shared constants, not independently drifting copies of 96.
- While credits remain, new publications stay in the existing bounded Landing and its existing
  overflow/terminal policy applies. This permits another 96 queued records in Landing; therefore
  the two-stage envelope is up to 192 distinct records, NOT 96 overall. This is record accounting,
  not a byte-memory claim: worker buffers, immutable publications, parsed data and commit-plan
  copies remain outside that count. The payload-memory package remains owed.
- Owner has explicit FIFO inbox, active commit receipt and pump-marker state. Retain complete
  sanitized domain envelopes (no native Client pointers). Encode these by the explicit Canon
  writer and include them in owner init/shape and logical dirty/subhash accounting.
- Defer identity/stream-phase checks until FIFO-head processing after earlier commit ACK;
  validate header/request liveness on admission without discarding temporarily busy work.
  Process multiple records in the same frame whenever dispatcher budgets permit.
- Use one payload-free Pump marker and queued typed commit replies. Delete draft envelope
  self-delivery. No generic dispatcher/Landing change or one-result-per-frame throttle.
- Fixture ingress uses the same explicit receipt/admission path, with finite protocol errors
  for oversized or unadmitted supplied batches. Never truncate a fixture or bypass receipts.

## Required safeguards

1. Cancellation/restart must NOT clear adapter receipts for old records still carried in the
   dispatcher. Those payloads still occupy transfer credits; acknowledge them when they reach
   the owner and are discarded. Owner may immediately acknowledge discarded inbox/active
   commit records it actually owns. A late commit ACK cannot acknowledge a newer operation.
2. A duplicate envelope must not acknowledge the unique receipt while its original is still
   in the inbox or active commit. Detect duplicate admission separately from a stale unique
   record. Duplicate/mismatched ACKs must not free another credit or open the next batch early.
3. Unknown/stale unique records still require final receipt acknowledgement, otherwise one
   cancelled request can permanently close the batch gate. Preserve arrival order of the rest.
4. Cancel/Retire and batch receipt acknowledgements are NOT worker completion. A running
   Landing reservation remains until worker acknowledgement under the existing contract.
5. Pump accounting survives cancellation and carry: do not forget a marker already queued
   and schedule a second one accidentally. Preserve that marker or use explicit correlation.
6. AdmissionRejected is separate from sequenced admitted outcomes. It must not retire an
   accepted request merely because no observation arrived yet. Use explicit admission-result
   state/correlation where required; `last_arrival == None` alone does not prove not admitted.
   This is pending-operation protocol state, not a second resource admission counter.
7. Profile activation retains/advances the successful profile stream, cancels obsolete other
   interests, and acknowledges their retained payloads without affecting the new scope.
8. No direct recursive owner stepping to execute commit replies outside the dispatcher, and
   no work-loop escape from pre/post step budgets. Commit execution remains adapter work under
   the borrowed current-owner permit; logical transitions remain typed deliveries.

## Acceptance evidence

Runtime RED/GREEN for the busy-commit loss; actual Bridge FIFO/drain/carry tests covering two
independent commits before first ACK, Registry→SignedIn, Ready→late ProfileRoster, queued
terminal failure, cancellation/restart/erase and stale ACK; duplicate envelope/receipt cases;
full-batch retention across frames while workers refill Landing; no second-batch release early;
admission rejection before/after acceptance; Cancel+Retire preserving physical reservation;
canonical/init/dirty changes with an unchanged UI publication. Direct reducer tests alone do
not prove the production handoff. Existing auth security/persistence tests remain mandatory.

Controlled App boot still requires initially empty queues/receipts. This decision does not add
an arbitrary mid-session application-checkpoint promise or complete the broader replay work.
