# UI restructure recovery

These branches preserve the project after a power outage:

- `backup/ui-phase8-recovery`: integrated work through the Library read-view and engine projection batches, plus local main's development-telemetry Makefile update.
- `backup/library-owned-recovery`: the same base plus the unfinished owned Library screen. It is a WIP checkpoint, not a verified or released build; integration is in progress and the latest screen edits have not passed host or shipping checks.

The branches are sanitized source snapshots. Original working history is retained locally;
unpublished historical objects containing household network literals were deliberately not pushed.
Credentials, runtime recordings, private native screenshots, build caches and the television's
configuration are not included. Documentation addresses and the project's documented synthetic
LAN stand-in replace private network values without changing the LAN-classification test.

The controlling specification is [the approved v4 plan](ui-plan-v4.md). The latest detailed evidence
and incomplete requirements are in [the phase-8 checkpoint](../measurements/ui-restructure-phase8-2026-09-08.md).
Next: finish Library's owned Screen, engine-owned focus, MasterDetail rail/grid and deferred
transactions; then complete Search/system keyboard, closed replay, and phases 9–12. Do not treat
the retained views or an adapter around legacy globals as completion.

Read `AGENTS.md` and the repository skills before working. Use `make check` and the shipping-feature
check; UI changes also need simulator and native proof. A cloud environment needs its own toolchain
setup and explicit device connectivity. Never assume old background processes or TV leases survived
the outage. Daytime TV instructions: leave the backlight on, keep audio muted, and acquire the lock.

## Latest outage checkpoint

Source checkpoint: `dcad3f537d4e5965858efbb09561a7189a8e6dd1` on `fleet/phase8-library-owned`.
The integrated branch also includes opt-in synthetic rail/pagination fixtures (seven focused tests passed).
This backup is for recovery, not a completion or release claim.

## Latest outage checkpoint

Source checkpoint: `b25f7cceab12b7ddd900d633353561bbe3ec751d` on `fleet/phase8-library-owned`.
The integrated branch also includes opt-in synthetic rail/pagination fixtures (seven focused tests passed).
This backup is for recovery, not a completion or release claim.
