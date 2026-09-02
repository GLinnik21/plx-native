//! **The exit alert** — the app's ONE decision alert, and the only place PlxNative asks a question
//! before doing something the user cannot take back.
//!
//! BACK at Home's root used to quit outright. That is the correct *destination* — Home is the root
//! and there is nowhere further back to go — but it is a one-press exit from a screen the user
//! reaches by pressing BACK a few times too many, and on a television the remote's BACK is the key
//! people lean on. So the press now raises this, and only *Exit* sets `running = false`.
//!
//! **It is a `Popover`, not a `Route`.** Every other modal that owns the whole frame is a route
//! because it can stand on more than one page (`Account { over }`, `ItemMenu { over }`) or because
//! it lives inside the player's `Overlay`. This one has exactly one host, forever: Home's root. A
//! `Route` arm for it would add a variant to `route_wears_tab_bar`, `page_of`, `Nav` and
//! `node_route` that could only ever answer "Home", i.e. four more places to keep a constant
//! true. What it DOES take from the route family is the modal discipline — `app.rs` gives it the
//! first arm of both the key ladder and the pointer chain, so while it is up nothing behind it can
//! be driven. A control that is not drawn must not be activatable, and a page under an alert is
//! not being drawn *for the user* even though its pixels are still on the panel.
//!
//! The scrim belongs to the PAGE (`Popover::scrim` + `content_painter`) — `app.rs` calls
//! [`draw_scrim`] at the end of its page closure, so the dim reaches the blur source and the panel's
//! frosted ground is not brighter than the screen it sits in. There is no [`Opener`] to lift: this
//! alert is about the app, not about an element on the page, and lifting a card out of the dim
//! would point it at something it is not talking about.
//!
//! [`Opener`]: crate::ui::popover::Opener
#[cfg(test)]
use crate::ui::consts::{SCR_H, SCR_W};
use crate::ui::consts::{SDLK_LEFT, SDLK_RIGHT};
use crate::ui::decision_alert::{Choice as AlertChoice, DecisionAlert};
#[cfg(test)]
use crate::ui::widgets::StatusOverlay;
use crate::ui::{theme, Rect};
use std::os::raw::c_int;
use std::ptr::{addr_of, addr_of_mut};

/// The question. One line, and it names the app rather than saying "this app" — the alert can be
/// raised from a Home screen that fills the panel, so the only thing identifying what is about to
/// close is this sentence.
const QUESTION: &core::ffi::CStr = c"Exit PlxNative?";
/// The two answers, in screen order. `Cancel` first because it is the safe one and it is the one
/// focus rests on.
const CANCEL: &core::ffi::CStr = c"Cancel";
const EXIT: &core::ffi::CStr = c"Exit";

/// Which answer the ring is on. Two states rather than an index, so nothing downstream can hold a
/// number that is not one of the two answers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Choice {
    /// Dismiss the alert and stay in the app. **The default** — an alert that opens on its own
    /// destructive answer turns a stray OK into the thing it exists to prevent.
    Cancel,
    /// Quit. The only path in the app that sets `running = false` from a key press.
    Exit,
}

static mut ALERT: DecisionAlert = DecisionAlert::new();
fn alert() -> &'static mut DecisionAlert {
    unsafe { &mut *addr_of_mut!(ALERT) }
}

pub fn is_open() -> bool {
    unsafe { (*addr_of!(ALERT)).is_open() }
}

/// The answer the ring is on — read by the draw, by `app.rs`'s OK arm and by the host tests.
pub fn focus() -> Choice {
    match unsafe { (*addr_of!(ALERT)).choice() } {
        AlertChoice::Cancel => Choice::Cancel,
        AlertChoice::Destructive => Choice::Exit,
    }
}

/// Raise the alert, always on [`Choice::Cancel`].
///
/// The reset is unconditional and is not a convenience: this panel is opened by the same key it is
/// dismissed with, so a session that cancelled once from *Exit* would re-open sitting on *Exit*,
/// and the second BACK-then-OK would quit where the first did not.
pub fn open() {
    alert().open();
}

pub fn close() {
    alert().dismiss();
}

pub fn update(dt: f32) {
    alert().update(dt);
}

/// LEFT/RIGHT between the two answers — pure, so the focus model is host-testable without a window.
///
/// **Clamped, not wrapped.** With two items a wrap makes LEFT and RIGHT the same key, so the ring
/// would cross onto *Exit* on either direction and the safe answer would have no side of its own.
pub(crate) fn step(cur: Choice, sym: c_int) -> Choice {
    match sym as u32 {
        SDLK_LEFT => Choice::Cancel,
        SDLK_RIGHT => Choice::Exit,
        _ => cur,
    }
}

pub fn move_focus(sym: c_int) {
    alert().set_choice(match step(focus(), sym) {
        Choice::Cancel => AlertChoice::Cancel,
        Choice::Exit => AlertChoice::Destructive,
    });
}

/// Commit the focused answer and take the alert down. The CALLER performs it — this module knows
/// what was chosen and nothing about how the app quits.
pub fn on_ok() -> Choice {
    let c = focus();
    close();
    c
}

/// Everything the alert places: the sheet, the question's band, and the two answers' frames.
///
/// One struct rather than four functions because the draw and the pointer hit test must read ONE
/// derivation — a click that lands somewhere other than the pill it looks like it is on is the
/// failure this shape exists to prevent, and it is exactly what two independent geometry functions
/// produce the first time one of them is edited.
pub(crate) struct Layout {
    pub(crate) panel: Rect,
    pub(crate) question: Rect,
    pub(crate) cancel: Rect,
    pub(crate) exit: Rect,
}

/// The layout, **pure**, from the one measured input: how tall a line of the question's type is.
///
/// Split this way for `up_next::layout_of`'s reason — the impure half measures through SDL2_ttf,
/// which the host suite cannot LINK (not "cannot answer": a test that calls it fails at `ld`, the
/// same boundary `StatusOverlay::bands` documents). So the measurement is the caller's and every
/// placement decision in the panel is gradeable without a window.
pub(crate) fn layout(q_h: f32) -> Layout {
    let shared = crate::ui::decision_alert::layout(q_h);
    Layout {
        panel: shared.panel,
        question: shared.question,
        cancel: shared.cancel,
        exit: shared.destructive,
    }
}

/// [`layout`] with the question measured for real — the draw's and the hit test's one entry point.
/// **Not callable from a host test**: `text_height` reaches SDL2_ttf.
fn measured() -> Layout {
    layout(crate::text::text_height(theme::size::TITLE, 1))
}

impl Layout {
    /// The answer a point lands on, or `None` for anywhere else.
    ///
    /// **A miss is not a dismissal**, which is the one place this differs from every menu in the
    /// app. A menu is dismissed by clicking away from it because "not that one" is a complete
    /// answer to "which of these?"; "not either" is no answer at all to a yes/no question, and the
    /// safe reading of a click on the scrim is that the user missed.
    pub(crate) fn hit(&self, x: f32, y: f32) -> Option<Choice> {
        if self.cancel.contains(x, y) {
            Some(Choice::Cancel)
        } else if self.exit.contains(x, y) {
            Some(Choice::Exit)
        } else {
            None
        }
    }
}

/// Pointer-down on an answer: PARK the ring on it and report the hit. It does not answer the
/// question — the caller arms the tvOS press on a `true` and spends it through `commit_exit_alert`
/// once the bounce has played, exactly as the key path does. A control face dips under a click as
/// well as under OK (the design system's `Button` is driven by `onPointerDown`/`onPointerUp`), and a
/// click that ended the process on the button-DOWN could show neither half of that.
///
/// A miss parks nothing and reports `false`, which is [`Layout::hit`]'s rule intact: "not either" is
/// no answer to a yes/no question, so a click on the scrim leaves the alert exactly as it was.
pub fn press_at(x: f32, y: f32) -> bool {
    alert().press_at(x, y)
}

/// The modal dim, drawn as part of the HOST PAGE — see the module doc and `Popover::scrim`.
pub fn draw_scrim() {
    alert().draw_scrim();
}

pub fn draw() {
    alert().draw(QUESTION, CANCEL, EXIT);
}

crate::dev::latched_flag!(
    /// `/tmp/plxnative-noexitconfirm` — BACK at Home's root quits outright, as it did before this
    /// alert existed.
    ///
    /// It exists for one class of caller: a headless path that closes the app by pressing BACK.
    /// Nothing in the tree does that today — `make kill`, `tests/run.py` and `tools/tv-session.sh`
    /// all close through SAM's `closeByAppId`, which is the only teardown that works on a wedged
    /// app anyway, and no manifest case injects `back` at all. So this is a door left open rather
    /// than a dependency being served, and a door is the honest fix: the alternative for a script
    /// that finds itself parked behind the alert is to teach it two more remote tokens and a race.
    ///
    /// **Not a DIAG trigger.** Arming it changes what a key press does, which is automation, so it
    /// also suppresses the who's-watching picker — exactly what a headless run wants. It is
    /// compiled out entirely under `RELEASE=1` (`dev::flag` is `false` at COMPILE time without the
    /// `devtriggers` feature), so a public build always asks.
    pub fn skip_confirm = "noexitconfirm";
);

#[cfg(test)]
mod tests {
    use super::*;

    /// LEFT/RIGHT over two answers, including both ends. The clamp is the property: LEFT from the
    /// left-hand answer must NOT arrive on the destructive one.
    #[test]
    fn left_right_walk_the_pair_and_clamp_at_both_ends() {
        assert_eq!(step(Choice::Cancel, SDLK_RIGHT as c_int), Choice::Exit);
        assert_eq!(step(Choice::Exit, SDLK_LEFT as c_int), Choice::Cancel);
        assert_eq!(
            step(Choice::Cancel, SDLK_LEFT as c_int),
            Choice::Cancel,
            "no wrap onto Exit"
        );
        assert_eq!(
            step(Choice::Exit, SDLK_RIGHT as c_int),
            Choice::Exit,
            "no wrap off the end"
        );
    }

    /// Anything that is not LEFT or RIGHT leaves the ring where it is — the alert swallows the
    /// rest of the remote rather than letting an unrelated key move focus onto *Exit*.
    #[test]
    fn an_unrelated_key_does_not_move_the_ring() {
        use crate::ui::consts::{SDLK_DOWN, SDLK_UP};
        assert_eq!(step(Choice::Cancel, SDLK_UP as c_int), Choice::Cancel);
        assert_eq!(step(Choice::Cancel, SDLK_DOWN as c_int), Choice::Cancel);
    }

    /// The cap band of one line of `size::TITLE` bold — what `measured()` hands `layout` on the
    /// device. Written out because the host cannot open a font; the tests below are about where
    /// things go GIVEN a measurement, and every one of them holds for any plausible value.
    const Q_H: f32 = theme::size::TITLE as f32;

    /// The design's frames, and the invariant behind them: two EQUAL widths, the stated gap, both
    /// inside the sheet, and the row centred on it.
    #[test]
    fn the_two_answers_are_equal_frames_inside_the_panel() {
        let l = layout(Q_H);
        assert_eq!(l.panel.w, 660.0);
        assert!(
            (l.panel.cx() - SCR_W * 0.5).abs() < 0.01,
            "the sheet is centred"
        );
        assert!((l.panel.cy() - SCR_H * 0.5).abs() < 0.01);

        assert_eq!(l.cancel.w, 260.0);
        assert_eq!(l.exit.w, 260.0, "two equal choices, two equal frames");
        assert_eq!(
            l.cancel.h,
            StatusOverlay::CTRL_H,
            "the app's one control height"
        );
        assert_eq!(l.exit.h, StatusOverlay::CTRL_H);
        assert_eq!(l.cancel.y, l.exit.y, "one row");
        assert!(
            (l.exit.x - (l.cancel.x + l.cancel.w) - 20.0).abs() < 0.01,
            "the design's 20px gap"
        );
        assert!(
            l.cancel.x >= l.panel.x && l.exit.x + l.exit.w <= l.panel.x + l.panel.w,
            "both answers inside the sheet"
        );
        assert!(
            ((l.cancel.x + l.exit.x + l.exit.w) * 0.5 - l.panel.cx()).abs() < 0.01,
            "the pair is centred on the sheet, not pinned to one edge"
        );
        // the stated bottom padding, measured off the drawn frames rather than restated
        assert!(((l.panel.y + l.panel.h) - (l.cancel.y + l.cancel.h) - 34.0).abs() < 0.01);
    }

    /// The question sits in the sheet's content column on the design's padding, and the sheet is
    /// tall enough to hold what the design puts in it — **whatever the font measures**. The sweep
    /// is the point: the panel fits its content, so a font swap or a rung change must not push the
    /// question into the answers.
    #[test]
    fn the_sheet_fits_its_content_at_any_measurement() {
        for q_h in [1.0, Q_H, 64.0, 120.0] {
            let l = layout(q_h);
            assert!(
                (l.question.y - l.panel.y - 48.0).abs() < 0.01,
                "48 above the question"
            );
            assert!(
                (l.question.x - l.panel.x - 48.0).abs() < 0.01,
                "48 either side"
            );
            assert!((l.question.w - (l.panel.w - 96.0)).abs() < 0.01);
            assert!(
                l.question.y + l.question.h <= l.cancel.y,
                "the question clears the answers"
            );
            assert!(
                l.cancel.y + l.cancel.h <= l.panel.y + l.panel.h,
                "…and the answers clear the sheet's own bottom edge (q_h={q_h})"
            );
        }
    }

    /// The pointer resolves to exactly the pill it looks like it is on, and a click that misses
    /// both is not an answer — the one place this panel deliberately differs from a menu.
    #[test]
    fn a_click_hits_the_pill_it_looks_like_and_a_miss_is_not_an_answer() {
        let l = layout(Q_H);
        assert_eq!(l.hit(l.cancel.cx(), l.cancel.cy()), Some(Choice::Cancel));
        assert_eq!(l.hit(l.exit.cx(), l.exit.cy()), Some(Choice::Exit));
        // the 20px alley between them belongs to neither
        assert_eq!(
            l.hit((l.cancel.x + l.cancel.w + l.exit.x) * 0.5, l.cancel.cy()),
            None
        );
        assert_eq!(
            l.hit(l.panel.cx(), l.panel.y + 8.0),
            None,
            "the question is not a control"
        );
        assert_eq!(
            l.hit(10.0, 10.0),
            None,
            "a click on the scrim is a miss, not a dismissal"
        );
        // and the edges belong to the pill they are drawn on, both ends
        assert_eq!(l.hit(l.cancel.x, l.cancel.y), Some(Choice::Cancel));
        assert_eq!(
            l.hit(l.exit.x + l.exit.w, l.exit.y + l.exit.h),
            Some(Choice::Exit)
        );
    }

    /// The sheet's corner is the token, and it is the outer shape rather than a fifth capsule: a
    /// panel radius at or above its own controls' fully-rounded ends reads as one big button.
    ///
    /// This lane was built from a paraphrase and argued 32 from that observation alone; the real
    /// argument is the design's and lives on [`theme::ALERT_PANEL_RAD`]. They agree, and the upper
    /// bound below is why it is worth keeping as a test: `capsule * 2.0` is 60, which is exactly
    /// the value the design says it TRIED and rejected — "one whole `--control-h`, so the panel
    /// would read as round as the pills inside it". The assertion and the spec are the same
    /// sentence reached from two directions.
    #[test]
    fn the_sheet_reads_as_a_sheet_and_not_as_a_control() {
        let capsule = StatusOverlay::CTRL_H * 0.5;
        assert_eq!(theme::ALERT_PANEL_RAD, 32.0);
        assert!(
            theme::ALERT_PANEL_RAD > capsule,
            "the sheet is the OUTER shape"
        );
        assert!(
            theme::ALERT_PANEL_RAD < capsule * 2.0,
            "…but a sheet corner near double its controls' is a button, not a container"
        );
    }
}
