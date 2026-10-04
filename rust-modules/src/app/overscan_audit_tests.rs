//! **The overscan audit** — LG's App Self Checklist item #2 over the outermost rect of every
//! screen and panel in the app.
//!
//! It was `ui::consts`' own test, and it cannot stay there: it asks the shared chrome (`ui`), the
//! player and track panels (`appkit`), the account menu and the Search layout (`screens`) and the
//! stats read-out (`app::diagnostics`) for the rects they lay out, so it is a whole-app test and
//! belongs to the layer that owns all of those (docs/module-layers.md, step L13). The safe-area
//! tokens it grades against are still `ui::consts`'.

use plx_ui::consts::{
    inside_safe, CARD_DY, CARD_H, CARD_W, GRID_TOP_Y, MARGIN_X, MARGIN_Y, SAFE, SCR_H, TITLE_DY,
};
use plx_ui::Rect;

/// **NO REQUIRED CONTENT ENTERS THE OVERSCAN EXCLUSION ZONE, ON EITHER AXIS.**
///
/// LG's App Self Checklist item #2 asks that the buttons, texts and logos on the main page sit
/// inside the overscan frame; this is that sentence, executable, over the outermost rect of
/// every screen and panel in the app.
///
/// **It grades the composed geometry, never the tokens.** There is deliberately no
/// `assert_eq!(MARGIN_X, 96.0)` here: that passes today and forbids the next audit from moving
/// the margin, or from giving one screen a correction of its own, which is the fix such an audit
/// most often needs. What is asserted is the requirement — so a future change that moves a token
/// AND keeps every screen inside the frame passes without this test being rewritten to permit
/// it, and one that moves a screen's own y by hand fails without anyone remembering to come
/// here. Six of the rows below were OUTSIDE the frame when this was written; the ones that were
/// worst — the A–Z rail at 32px, the detail page's pinned logo at 32, the top bar at 18 — were
/// all in geometry no token could have described.
///
/// **"Required" is doing real work in that sentence.** A full-bleed hero backdrop, a page
/// ground, a scrim, a shelf peeking off the bottom edge and the focus GLOW that overflows a
/// poster are all supposed to reach the panel edge; bounding them would be the bug. What is
/// graded is what a viewer has to read or press.
///
/// **So tiles are entered at REST, and that is a decision rather than an oversight.** A focused
/// card is drawn `RowStyle::HOME`'s 1.09 about its own centre, which puts the first column's
/// painted edge ~11px past the margin, and `GLOW_PAD` spills 64 further. Neither is new content:
/// the pop MAGNIFIES ink already inside the frame, strictly containing its resting rect
/// (`widgets`' own note on the control pop), and the caption under it — the TEXT — does not
/// scale at all. The line this draws is between decoration that overflows and *the thing
/// itself*: the detail page's pinned compact logo IS graded at its upward spill, because there
/// the spill is the logo, and a clearLogo is one of the three things item #2 names.
///
/// **What it cannot see**, so that a green run is not read as more than it is: text is graded by
/// the box a screen lays it out in, not by rasterized ink (the host suite cannot link
/// SDL2_ttf — the boundary `StatusOverlay::bands` documents), so a run that overflows its own
/// column is `text::elide`'s business and not this test's. Rects whose width is a measured
/// label are entered degenerate, with the EDGE that matters and a zero extent the other way.
#[test]
fn no_required_content_enters_the_safe_area_exclusion_zone() {
    let mut r: Vec<(&'static str, Rect)> = Vec::new();

    // **A probe that quietly stops contributing is a screen that quietly stops being audited**,
    // and an `assert!` loop over an empty table passes. So each one is required to contribute,
    // individually: a table-wide floor cannot see one probe of eight going silent, which is what
    // a `r.len() >= N` guard was actually doing here.
    let mut probe = |name: &str, f: &dyn Fn(&mut Vec<(&'static str, Rect)>)| {
        let before = r.len();
        f(&mut r);
        assert!(
            r.len() > before,
            "the {name} probe contributed nothing — it stopped being audited"
        );
    };

    // ---- the shared chrome, and the screens composed on it ------------------------------
    probe("widgets", &plx_ui::widgets::overscan_rects);
    probe("detail", &plx_ui::detail_layout::overscan_rects);
    probe("player_hud", &plx_appkit::player_hud::overscan_rects);

    // ---- the panels, each at the widest/tallest state its own clamp admits ---------------
    probe("account_menu", &crate::screens::account_menu::overscan_rects);
    probe("track_menu", &plx_appkit::track_menu::overscan_rects);
    probe("more_menu", &plx_appkit::more_menu::overscan_rects);
    probe("stats", &crate::app::diagnostics::overscan_rects);
    drop(probe);

    // ---- the screens whose outermost geometry is already public here --------------------
    // Home: the hero's text column and its action row start at the margin; the grid view's
    // first shelf heading is the highest ink the page draws under the bar.
    r.push((
        "home hero text column",
        Rect::new(MARGIN_X, 380.0, plx_ui::landing_hero::COL_W, 400.0),
    ));
    r.push((
        "home first shelf heading (grid view)",
        Rect::new(MARGIN_X, GRID_TOP_Y - TITLE_DY, 400.0, TITLE_DY),
    ));
    r.push((
        "home first shelf card (grid view)",
        Rect::new(MARGIN_X, GRID_TOP_Y + CARD_DY, CARD_W, CARD_H),
    ));
    // …and the focused card's block at the BOTTOM of its reveal: card + the 96px label band,
    // which is what the owned Home's and `library`'s reveal rules keep clear of the edge.
    r.push((
        "home focused card block, revealed",
        Rect::new(
            MARGIN_X,
            SCR_H - MARGIN_Y - CARD_H - 96.0,
            CARD_W,
            CARD_H + 96.0,
        ),
    ));

    // Search: the bare query line, and the scope line below it.
    r.push(("search field", crate::screens::search::layout::FIELD));
    r.push((
        "search first shelf heading",
        Rect::new(MARGIN_X, crate::screens::search::layout::CONTENT_TOP, 400.0, 40.0),
    ));

    // Person: the portrait at the margin, and the air the reveal keeps under a shelf.
    r.push(("person portrait", Rect::new(MARGIN_X, 96.0, 320.0, 320.0)));
    r.push((
        "person shelf block, revealed",
        Rect::new(MARGIN_X, SCR_H - MARGIN_Y - CARD_H, CARD_W, CARD_H),
    ));

    // Onboarding + login: both centre or hang off the same margin.
    r.push((
        "onboard copy column",
        Rect::new(MARGIN_X, 150.0, plx_ui::landing_hero::COL_W, 500.0),
    ));

    for (name, rect) in r {
        assert!(
            inside_safe(rect),
            "{name} at ({}, {}) {}x{} leaves the {}x{} safe area at ({}, {})",
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            SAFE.w,
            SAFE.h,
            SAFE.x,
            SAFE.y,
        );
    }
}
