use super::*;
#[test]
fn foreground_state_entry_and_exit_obligations_are_exact_values() {
    type S = ForegroundState<u32>;
    use ForegroundClock::Playing;
    let cases = [
        (S::Idle, None, false, false),
        (S::Suspended { id: 7, saved_ns: 90, clock: Playing }, None, false, false),
        (S::Claimed { id: 7, claim: ForegroundClaim::Prepare { saved_ns: 90, saved_clock: Playing, resume_ns: 80, clock: Playing } },
            Some(ForegroundEffect::Prepare { id: 7, resume_ns: 80 }), false, false),
        (S::Claimed { id: 7, claim: ForegroundClaim::Load { resume_ns: 80, clock: Playing } },
            Some(ForegroundEffect::Load { id: 7, resume_ns: 80, clock: Playing }), true, false),
        (S::Claimed { id: 7, claim: ForegroundClaim::PlayClock },
            Some(ForegroundEffect::PlayClock { id: 7 }), false, false),
        (S::Prepared { id: 7, resume_ns: 80, clock: Playing }, None, false, false),
        (S::LoadPending { id: 7, attempt: 42, resume_ns: 80, clock: Playing }, None, true, true),
        (S::PreparePending { id: 7, saved_ns: 90, saved_clock: Playing, resume_ns: 80, clock: Playing }, None, false, false),
        (S::RecoveryPending { id: 7, resume_ns: 80, clock: Playing }, None, true, false),
        (S::ClockPending { id: 7 }, None, false, false),
    ];
    for (state, entry, load, retire) in cases {
        assert_eq!(state.entry_effect(), entry, "{state:?}");
        assert_eq!(state.exit_retire_effect(), retire.then_some(ForegroundEffect::Retire { id: 7, clock: Playing }), "{state:?}");
        for attempt in [None, Some(42)] {
            for started in [false, true] {
                assert_eq!(state.exit_load_effect(attempt, started), load.then_some(ForegroundEffect::AfterLoad { attempt, clock: Playing, started }), "{state:?}");
            }
        }
    }
}
