//! `nav` — **the page transition's PRESENTATION, published once a frame**: the two cascade alphas
//! a route change rides, the pill the shared strip must read as selected while one is in flight,
//! and the dev blurred-dissolve prototype's ramp.
//!
//! **It is no longer a fader.** It owned an [`Xfade`] and drove it (`begin`/`cancel`/`tick`), which
//! made it the second navigation authority in the app: the loop's `nav_commit` applied its route
//! change at THIS module's floor while the container's own `NavStack` ran `Immediate` and committed
//! at the same frame for its own reasons. Restructure phase 12 (D1) put `PageDip` on the
//! application's page stack — the same schedule, the same 70/1/140 ms, the same continuous-chrome
//! rule and the same reversal on cancel, lifted into `ui::containers::transition` where it can be
//! tested against the container that uses it — and this module became what its readers actually
//! wanted: a place to ASK what the transition looks like this frame.
//!
//! **It is a PUBLICATION, not a mirror.** `app::bridge::frame_with_results` writes it from
//! `NavStack::{page_alpha, chrome_alpha}` immediately after the dispatcher's frame, and nothing
//! else writes it — so it cannot disagree with the container, and a reader that runs before the
//! first frame sees the rest values (1.0, 1.0, no pending pill), which is exactly right.
//!
//! **The DIP itself is unchanged and is documented at [`PageDip`](crate::ui::containers::transition::PageDip).**
//! Every screen's draw opens with `gfx::frame_clear`, so two pages can never be on the panel at
//! once; the outgoing page fades to the app ground, the op applies at the floor, the incoming page
//! fades up off it. `theme::CLEAR_RGB` IS `theme::SURFACE_APP`, so the trough colour and the clear
//! are the same pixel and the swap has no seam. It costs no second pass: the alpha is a value in a
//! cascade every primitive already multiplies through (`Painter::c`).
//!
//! Two alphas, and the split is the whole design. [`page_alpha`] is for content the swap replaces;
//! [`chrome_alpha`] is for the **shared top tab bar**, the SAME control on Home, the Library and
//! Search, which must hold still while the pages swap under it. Its readers are the ones that draw
//! outside a page's own `DrawFrame` — `ui::popover`'s panel and scrim, `widgets::redraw_profile_chip`,
//! and the glass track's "am I settled enough to sample the ground" test.
//!
//! [`view_tab`] is the other half of "the control acknowledges the press on the press frame": the
//! capsule travels to the pressed pill immediately, while the page it names is still fading in.
//! The pending destination is the container's (`NavStack::pending_dest`); which PILL that page is
//! remains the application's answer, so `app::bridge` resolves it before publishing.
//!
//! **Main-thread only**, a `thread_local!` cell rather than a `static mut` — `ci/check-statics.sh`
//! carried this module as a migration entry ("the statics die in 12") and it is now three plain
//! values behind a safe borrow.

use std::cell::Cell;
use std::os::raw::c_int;

/// What a page transition looks like THIS FRAME, as published by the one writer.
#[derive(Clone, Copy)]
struct NavPresentation {
    page: f32,
    chrome: f32,
    /// The pill the destination selects, or -1 for "nothing in flight". -1 rather than
    /// `Option<usize>` so it drops straight into the `c_int` the strip is placed from.
    tab: c_int,
}

impl NavPresentation {
    const fn rest() -> Self {
        NavPresentation { page: 1.0, chrome: 1.0, tab: -1 }
    }
}

thread_local! {
    static NAV: Cell<NavPresentation> = const { Cell::new(NavPresentation::rest()) };
    /// Dev only: swap the dip for a blurred dissolve. Latched at boot from the trigger. Kept as
    /// its own cell because it is armed once per process and read by an unrelated caller.
    static BLUR_DISSOLVE: Cell<bool> = const { Cell::new(false) };
}

/// **Publish this frame's transition**, from the container that owns it. One writer
/// (`app::bridge::frame_with_results`), called after the dispatcher's frame so the values describe
/// the transition as it stands when the loop draws.
pub(crate) fn publish(page: f32, chrome: f32, tab: Option<usize>) {
    NAV.with(|n| n.set(NavPresentation {
        page,
        chrome,
        tab: tab.map(|t| t as c_int).unwrap_or(-1),
    }));
}

/// The cascade alpha for PAGE CONTENT — everything a screen draws that does not survive the swap.
///
/// **Except under the `/tmp/plxnative-navblur` prototype**, where it is a flat 1: that experiment
/// replaces the grey trough with a full-bleed blur cross-faded OVER the page (see
/// [`crate::ui::glassload`]), and a page that also dipped would be showing both transitions at
/// once. Everything else about the transition is unchanged, so [`blur_amount`] is exactly the ramp
/// the dip would have ridden, read the other way up.
pub(crate) fn page_alpha() -> f32 {
    if BLUR_DISSOLVE.with(Cell::get) {
        return 1.0;
    }
    NAV.with(Cell::get).page
}

/// Arm the blurred-dissolve prototype. Boot-time only; nothing reads it more than once a frame.
pub(crate) fn set_blur_dissolve(on: bool) {
    BLUR_DISSOLVE.with(|b| b.set(on));
}

/// How strongly the transition's blur slab should cover the page, 0 at rest and 1 at the floor —
/// the fade the dip would have ridden, inverted. Always 0 unless the prototype is armed, so the
/// caller needs no second test.
pub(crate) fn blur_amount() -> f32 {
    if BLUR_DISSOLVE.with(Cell::get) {
        1.0 - NAV.with(Cell::get).page
    } else {
        0.0
    }
}

/// The cascade alpha for CONTINUOUS CHROME — the shared top band (tab row + profile chip), which is
/// the same object on Home and on the Library and must NOT blink when one replaces the other. Full
/// while the bar exists on both sides of the transition; otherwise it rides the page fade, because
/// a destination with no tab bar has nothing for it to be continuous WITH.
pub(crate) fn chrome_alpha() -> f32 {
    NAV.with(Cell::get).chrome
}

/// The tab pill the shared row must read as SELECTED this frame: the queued destination's while a
/// route change is in flight, the caller's own otherwise. The cross-page twin of
/// `library::view_section`, and the one thing that makes the capsule leave on the PRESS frame.
pub(crate) fn view_tab(own: c_int) -> c_int {
    let t = NAV.with(Cell::get).tab;
    if t >= 0 { t } else { own }
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    //! **The fader's own tests moved with the fader** to
    //! `ui::containers::transition`'s (`continuous_chrome_never_dips_while_the_page_does`,
    //! `a_withdrawn_dip_reverses_and_never_commits`,
    //! `a_retarget_cannot_un_hide_chrome_it_started_hiding`,
    //! `the_dip_reports_motion_and_the_floor_is_alpha_zero`), where they grade the `PageDip` the
    //! application really runs rather than a second implementation of the same schedule. What is
    //! left here is a publication, and what a publication can get wrong is exactly two things.
    use super::*;

    #[test]
    fn a_reader_that_runs_before_the_first_frame_sees_the_rest_values() {
        let _g = crate::testlock::serial();
        publish(1.0, 1.0, None);
        set_blur_dissolve(false);
        assert_eq!(page_alpha(), 1.0);
        assert_eq!(chrome_alpha(), 1.0);
        assert_eq!(view_tab(3), 3, "no pending destination — the caller's own answer stands");
        assert_eq!(blur_amount(), 0.0, "the prototype is not armed");
    }

    /// The two things the publication decides for itself: the pending pill overrides the caller's
    /// own selection, and the blur prototype reads the page ramp the other way up while flattening
    /// the dip it replaces.
    #[test]
    fn the_pending_pill_overrides_and_the_blur_prototype_reads_the_ramp_inverted() {
        let _g = crate::testlock::serial();
        publish(0.25, 1.0, Some(2));
        assert_eq!(view_tab(0), 2, "the capsule travels to the queued destination");
        assert_eq!(page_alpha(), 0.25);
        assert_eq!(chrome_alpha(), 1.0, "continuous chrome does not dip with the page");
        assert_eq!(blur_amount(), 0.0);
        set_blur_dissolve(true);
        assert_eq!(page_alpha(), 1.0, "the page does not ALSO dip under the slab");
        assert_eq!(blur_amount(), 0.75);
        set_blur_dissolve(false);
        publish(1.0, 1.0, None);
    }
}
