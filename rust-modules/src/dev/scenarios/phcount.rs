//! `/tmp/plxnative-phcount`: a once-a-second `phcount:` event-log line counting the card draws
//! that showed a placeholder. Measurement only; the instrument for `tests/run.py --fps --mock
//! --filter library-burst`, which sums the lines inside its key walk.
//!
//! The counts come from `plx_ui::card_motion_metrics`, which counts at the card primitive
//! (`widgets::resolve_card_art`) whether the card resolved a texture, so a draw with none is a
//! card showing its skeleton. Arming it changes no drawing and no admission decision. It shares
//! that counter with the poster-gate scenes, so do not arm both at once (`arm` resets it).
//!
//! The line is `phcount: frames=F ph_frames=P draws=D ph=N` (`card_motion_metrics::interval_line`),
//! one per second of the frame clock, including the all-zero line of an interval that drew no card.
use plx_ui::card_motion_metrics::{self as metrics, Stats};
use std::cell::Cell;

const INTERVAL_MS: u32 = 1000;

#[derive(Clone, Copy)]
enum State {
    Unchecked,
    Off,
    On { at: u32, prev: Stats },
}

thread_local! { static STATE: Cell<State> = const { Cell::new(State::Unchecked) }; }

/// Called once per loop iteration on the UI thread with the frame clock.
pub(crate) fn tick(now: u32) {
    STATE.with(|cell| match cell.get() {
        State::Off => {}
        State::Unchecked => cell.set(if plx_base::devtrig::read("phcount").is_some() {
            metrics::arm();
            plx_base::eventlog::log("phcount: armed");
            State::On { at: now, prev: metrics::snapshot() }
        } else {
            State::Off
        }),
        State::On { at, prev } => {
            if now.wrapping_sub(at) < INTERVAL_MS {
                return;
            }
            let cur = metrics::snapshot();
            plx_base::eventlog::log(&metrics::interval_line(prev, cur));
            cell.set(State::On { at: now, prev: cur });
        }
    });
}
