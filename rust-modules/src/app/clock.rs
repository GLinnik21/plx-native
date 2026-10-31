//! The application's clock (restructure spec §4.1): ONE door to frame time, so a replay can drive
//! the loop on recorded ticks. `today()` is the same door for the calendar date: the ONE place the
//! application asks the platform what day it is (`plx_platform::i18n::local_calendar_today`), once a
//! minute. Its one caller is the LOOP, which publishes the answer once per iteration
//! (`run::publish_frame` -> `Bridge::publish_today`); a screen that words a date ("Today",
//! "Yesterday") RECEIVES it as `AppViews::today` (`LibraryLike::today`) and reads no clock, the way
//! a frame's time arrives on a `Tick`. `now()` reads `SDL_GetTicks` — the only call site of it in `app/`
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
    /// The last read of the platform's calendar date and the minute (of the frame clock) it was read in.
    static TODAY: Cell<Option<(u32, Option<[i32; 3]>)>> = const { Cell::new(None) };
}

/// The date every replay frame answers: a recording has no calendar of its own, and a fixed one keeps
/// a replay of a date-sorted grid identical on every run.
pub(crate) const REPLAY_DATE: [i32; 3] = [2026, 1, 1];

/// Milliseconds since SDL init — or, under a replay, the recorded tick of the frame in flight.
pub(crate) fn now() -> u32 {
    if let Some(ms) = REPLAY.with(|c| c.get()) {
        return ms;
    }
    // SAFETY: SDL is initialised before the loop that reads the clock exists; the call takes no
    // arguments and touches no memory of ours.
    unsafe { SDL_GetTicks() }
}

/// The replay driver sets the frame's time before the frame reads it, and with it the date
/// ([`REPLAY_DATE`]).
pub(crate) fn set_replay(ms: u32) {
    REPLAY.with(|c| c.set(Some(ms)));
}

/// **Today's calendar date** in the OS time zone, `[year, month, day]`: read from the platform at
/// most once a minute of the frame clock ([`now`]), so a midnight is noticed within a minute and a
/// frame pays one thread-local read. Under a replay (or a test that set one) it is the pinned date.
/// `None` when the platform cannot say (an unrepresentable `time_t`): a date then reads as month
/// and year. Main-thread only, like the clock.
///
/// **Only the loop calls it** (`run::publish_frame`, once per iteration), and publishes the answer
/// into the Bridge's views. A screen or a host impl never calls it: `AppHost::today` returns the
/// frame's publication.
pub(crate) fn today() -> Option<[i32; 3]> {
    // A recorded Library grid sorted by a date must word the same on every run and on every day.
    if REPLAY.with(|c| c.get()).is_some() {
        return Some(REPLAY_DATE);
    }
    let minute = now() / 60_000;
    if let Some((at, date)) = TODAY.with(|c| c.get()) {
        if at == minute { return date; }
    }
    let date = plx_platform::i18n::local_calendar_today();
    TODAY.with(|c| c.set(Some((minute, date))));
    date
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_replay_tick_overrides_the_clock_for_the_thread() {
        super::set_replay(4242);
        assert_eq!(super::now(), 4242);
        super::REPLAY.with(|c| c.set(None));
    }

    #[test]
    fn a_published_date_reaches_a_screen_through_its_views_and_no_clock() {
        use crate::app::bridge::{AppHost, Bridge};
        use plx_machine::machine::{EntryId, InputOwner, Tick};
        use plx_screens::registry::LibraryLike;
        use plx_ui::dispatch::CxParts;
        let mut bridge = Bridge::for_test(|| 0);
        let parts = CxParts { tick: Tick::default(), press: Default::default(), focus: Default::default(),
            owner: InputOwner::Entry(EntryId(0)) };
        // What a screen sees: a `Cx` over the Bridge's views, asked through the Library's host trait.
        let seen = |bridge: &Bridge| {
            let cx = parts.cx::<AppHost>(
                bridge.views_with(plx_media::route::idle_session_for_test()), &plx_ui::fixture::FixtureMeasure);
            <AppHost as LibraryLike>::today(&cx)
        };
        assert_eq!(bridge.views().today, None);
        assert_eq!(seen(&bridge), None, "before the loop's first publication the host has no date");
        bridge.publish_today(Some([2001, 2, 3]));
        assert_eq!(bridge.views().today, Some([2001, 2, 3]));
        assert_eq!(seen(&bridge), Some([2001, 2, 3]), "a screen receives the published date");
        // The host reads no clock for it: a replay pinning another date changes nothing it answers.
        super::set_replay(100);
        assert_ne!(super::today(), Some([2001, 2, 3]));
        assert_eq!(seen(&bridge), Some([2001, 2, 3]), "the answer is the publication, not `today()`");
        bridge.publish_today(None);
        assert_eq!(seen(&bridge), None, "a publication of nothing is nothing");
        super::REPLAY.with(|c| c.set(None));
    }

    #[test]
    fn the_date_is_pinned_under_a_replay_and_read_once_a_minute_otherwise() {
        super::REPLAY.with(|c| c.set(None));
        super::TODAY.with(|c| c.set(None));
        // Live: the platform's date, cached for the minute (the host test's frame clock stands at 0).
        let live = super::today();
        assert!(live.is_some_and(|[year, month, day]| year >= 2026 && (1..=12).contains(&month) && (1..=31).contains(&day)));
        super::TODAY.with(|c| c.set(Some((0, Some([2001, 2, 3])))));
        assert_eq!(super::today(), Some([2001, 2, 3]), "within the minute the read is not repeated");
        super::TODAY.with(|c| c.set(Some((7, Some([2001, 2, 3])))));
        assert_eq!(super::today(), live, "a new minute reads again");
        // A replay frame pins the date, whatever the wall clock says.
        super::set_replay(100);
        assert_eq!(super::today(), Some(super::REPLAY_DATE));
        super::REPLAY.with(|c| c.set(None));
        super::TODAY.with(|c| c.set(None));
    }
}
