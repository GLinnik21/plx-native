//! The application's clock (restructure spec §4.1): ONE door to frame time, so a replay can drive
//! the loop on recorded ticks. `now()` reads `SDL_GetTicks` — the only call site of it in `app/`
//! (the §15.2 gate: `SDL_GetTicks(` only in the clock and the instruments) — unless a replay has
//! set the frame's recorded tick, in which case every read in the frame answers that tick: the
//! ingest stamp, each press arm, the HUD linger, a script delay.
//!
//! Phase 2 shape: a thread-local override rather than a `Clock` value threaded through every
//! signature. The loop still reads the clock at several points in a frame (ingest, each key
//! arm, the HUD deadline after a blocking resolve); live, those differ by the microseconds
//! between them exactly as before; under a replay they are all the frame's tick, which is what
//! makes the machines that are machines today replay deterministically. Main-thread only — the
//! loop is the only reader, and a worker that needs wall time uses `Instant` (an adapter).

use std::cell::Cell;

#[cfg(not(test))]
extern "C" {
    fn SDL_GetTicks() -> u32;
}

/// The host test binary links no SDL (`diag::heartbeat` carries the same stub for the
/// performance counter); a test reads the clock only through the replay override.
#[cfg(test)]
#[allow(non_snake_case)]
unsafe fn SDL_GetTicks() -> u32 {
    0
}

thread_local! {
    /// The replay override: `Some(ms)` while a recorded frame is being replayed.
    static REPLAY: Cell<Option<u32>> = const { Cell::new(None) };
}

/// Milliseconds since SDL init — or, under a replay, the recorded tick of the frame in flight.
pub(crate) fn now() -> u32 {
    if let Some(ms) = REPLAY.with(|c| c.get()) {
        return ms;
    }
    // SAFETY: SDL is initialised before the loop that reads the clock exists; the call takes no
    // arguments and touches no memory of ours.
    unsafe { SDL_GetTicks() }
}

/// The replay driver sets the frame's time before the frame reads it.
pub(crate) fn set_replay(ms: u32) {
    REPLAY.with(|c| c.set(Some(ms)));
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_replay_tick_overrides_the_clock_for_the_thread() {
        super::set_replay(4242);
        assert_eq!(super::now(), 4242);
        super::REPLAY.with(|c| c.set(None));
    }
}
