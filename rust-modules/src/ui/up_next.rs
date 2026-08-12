//! **Up Next** — the next-episode tile in the transport HUD (`Player Screen.dc.html`, the control
//! row's third occupant). It appears when the playing episode reaches its CREDITS marker and the
//! PlayQueue has another episode behind it: the next episode's still, its address and name, and a
//! countdown that starts it on its own.
//!
//! The data is `route::up_next()` — lifted from the `continuous=1` PlayQueue that every playback
//! already creates, so nothing here asks the server what plays next.
//!
//! It TAKES THE PLACE of the transport's Subtitles/Audio discs — the same control row the Skip
//! button uses, the same right edge — with the still and its caption stacked directly above. The
//! countdown is the primary button's own fill sweep (`Button::progress`), so the control and the
//! timer are one object rather than a button beside a rail.
//!
//! **Two buttons, and they are deliberately not equals** (the design's own note): *Next Episode*
//! takes the [`ctrl_slot`](crate::ui::player_hud::ctrl_slot) width — the floor that keeps the row
//! from visibly shrinking when it appears — while *Watch Credits* takes only its own label's, since
//! a second full-width capsule would read as a pair of equals when one continues watching and the
//! other does nothing at all. They are laid out in that spatial order, so LEFT reaches Watch
//! Credits and RIGHT returns.
//!
//! It is HUD furniture, not an overlay: playback keeps running underneath and the credits keep
//! rolling. That is what makes the countdown honest — `app.rs` holds the HUD up for as long as it
//! is armed, so the timer is never running behind a hidden transport.
//!
//! **The row's cursor is not kept here.** It is `hud_nav.btn`, exactly as it is for the discs, so
//! LEFT/RIGHT need no special case and [`ControlSlot::items`](crate::ui::player_hud::ControlSlot)
//! answers for the clamp. This module owns the countdown and nothing else — which is also why the
//! cancel rule lives at `app.rs`'s one frame block rather than being spread across the key arms.
//!
//! *(A full-screen post-play CARD — show poster, DISPLAY title, countdown ring — was built for the
//! cross-show case and lives at commit `84ff9328`. It is not here because its trigger does not
//! exist: `continuous=1` is per-show, and a final episode returns `totalCount=1`, so the queue
//! never hands us a different show. Sourcing it from Continue Watching is the owner's call.)*
#![allow(dead_code)]
use crate::route::UpNext;
use crate::ui::label::{HAlign, Label};
use crate::ui::theme;
use crate::ui::widgets::{draw_card, Button};
use crate::ui::{Env, Painter, Rect, View};
use std::ffi::CString;
use std::os::raw::c_int;
use std::ptr::{addr_of, addr_of_mut};

/// How long the tile holds before starting the next episode by itself. Long enough to read the
/// title and reach for the remote, short enough that "always the next episode" stays automatic.
pub(crate) const COUNTDOWN_MS: u32 = 10_000;

/// The row's two items, in the order they are DRAWN — so `hud_nav.btn` and the arrow keys agree
/// without a translation table (LEFT lands on 0 because 0 is the left one).
pub(crate) const BTN_CREDITS: c_int = 0;
pub(crate) const BTN_NEXT: c_int = 1;
/// Where the focus ring parks when the row appears: the PRIMARY, which here is the right-hand item
/// rather than the usual 0. `ControlSlot::primary_btn` is what reads this, so the appear edge in
/// `app.rs` stays one line for every occupant.
pub(crate) const PRIMARY_BTN: c_int = BTN_NEXT;

/// `SDL_GetTicks` deadline at which the next episode auto-starts. 0 = not armed (the tile is not
/// up, or the countdown was cancelled).
static mut DEADLINE: u32 = 0;
/// The user cancelled the auto-advance for THIS segment (they took hold of the row). Latched,
/// because `tick` runs every frame and would otherwise re-arm the countdown the very next one.
/// Cleared when the segment ends, so the next episode's credits get their own chance.
static mut CANCELLED: bool = false;

/// Whether this owns the control row this frame. The precedence itself lives in ONE place —
/// [`crate::ui::player_hud::slot_for`] — so this is just a read of the resolved slot.
pub(crate) fn is_shown(slot: crate::ui::player_hud::ControlSlot) -> bool {
    matches!(slot, crate::ui::player_hud::ControlSlot::UpNext(_))
}

/// Per-frame tick: arm the countdown the moment the tile appears, and forget everything about it
/// once the segment is over. Idempotent — an already-running countdown is never pushed forward.
///
/// It arms on APPEARANCE, never on focus: the tile's whole promise is that it starts the next
/// episode on its own. Focus only ever [`cancel`]s it.
pub(crate) fn tick(slot: crate::ui::player_hud::ControlSlot, now: u32) {
    if !is_shown(slot) {
        // segment over (or the queue emptied): drop the deadline AND the cancel latch, so the
        // next episode's credits arm normally instead of inheriting this one's refusal
        reset();
        return;
    }
    if (unsafe { addr_of!(DEADLINE).read() }) == 0 && !(unsafe { addr_of!(CANCELLED).read() }) {
        unsafe { addr_of_mut!(DEADLINE).write(now.wrapping_add(COUNTDOWN_MS).max(1)) }
    }
}

/// PURE — may the countdown keep running with the row focused this way?
///
/// ONE rule for every way a user can take hold of the transport, and it is a STEADY STATE rather
/// than an edge on purpose: focus off the control row means they are driving something else, and
/// focus on *Watch Credits* means they have reached for the alternative. Either way, engaging the
/// transport is not consent to be pulled into the next episode. Arrowing back to *Next Episode*
/// does not re-arm — [`cancel`] latches — so a countdown, once refused, stays refused for the
/// segment.
pub(crate) fn countdown_may_run(row_focused: bool, btn: c_int) -> bool {
    row_focused && btn == BTN_NEXT
}

/// Stop the auto-advance without hiding the tile — the user took hold of the row, or is leaving.
/// The tile stays up as a plain "OK to play" target; only the clock is off.
pub(crate) fn cancel() {
    unsafe {
        addr_of_mut!(DEADLINE).write(0);
        addr_of_mut!(CANCELLED).write(true);
    }
}

/// True while the countdown is running (drives holding the HUD up, so the timer is never invisible).
pub(crate) fn armed() -> bool {
    (unsafe { addr_of!(DEADLINE).read() }) != 0
}

/// Milliseconds left (0 when disarmed or elapsed).
///
/// `SDL_GetTicks` wraps every ~49 days, so the comparison is the codebase's wrapping idiom
/// (`wrapping_sub` against the 0x8000_0000 half-range), not `now < deadline` — the same guard
/// `app.rs` uses for its deferred-refresh deadline.
fn remaining_ms(now: u32) -> u32 {
    let d = unsafe { addr_of!(DEADLINE).read() };
    if d == 0 {
        return 0;
    }
    let left = d.wrapping_sub(now);
    if left == 0 || left >= 0x8000_0000 {
        0
    } else {
        left
    }
}

/// True once the countdown has run out — `app.rs` polls this and starts the next episode.
pub(crate) fn expired(now: u32) -> bool {
    armed() && remaining_ms(now) == 0
}

/// The descriptor to start, cloned off the `&'static` store. Cloning is mandatory, not tidiness:
/// `route::request_play_up_next` clears `UP_NEXT` as its first act, so handing it a borrow of the
/// static would be a use-after-free the borrow checker cannot see through a `'static` lifetime.
pub(crate) fn take() -> Option<UpNext> {
    cancel();
    crate::route::up_next().cloned()
}

/// Per-session reset — a new playback must not inherit the previous episode's countdown OR its
/// cancel latch.
pub(crate) fn reset() {
    unsafe {
        addr_of_mut!(DEADLINE).write(0);
        addr_of_mut!(CANCELLED).write(false);
    }
}

// ---- geometry: the control row's slot, with the next episode's still stacked above it. The still
// takes the PRIMARY button's width and right edge, so the group is one clean two-edge column — an
// earlier version gave the still its own width and drew it permanently scaled, which put its right
// edge 8px past the pill's and its left 15px inside, a near-miss that reads as unfinished from the
// couch. ----

/// still → caption: the caption is the still's label, so it hugs it…
const CAP_GAP: f32 = theme::space::XS;
/// …and the buttons are separate actionable objects, so they get more air. Unequal gaps are what
/// create the grouping; an earlier 8/8 left the caption belonging to neither.
const BTN_GAP: f32 = theme::space::MD;
/// between the two buttons — the design's `--space-sm`, tighter than the gap to the caption above
/// so the pair reads as one row before it reads as two controls
const PILL_GAP: f32 = theme::space::SM;
const CAPTION_H: f32 = 30.0;

const NEXT_LABEL: &str = "Next Episode";
const CREDITS_LABEL: &core::ffi::CStr = c"Watch Credits";

/// The caption is RIGHT-ALIGNED text, so it may run wider than the still/button column without
/// breaking it — the column's edges are the two solid rectangles, and a text run has no left edge
/// to break. It gets its own budget because clamping it to the button's width elided the episode
/// NAME, which is the one thing the caption is for.
const CAPTION_W: f32 = 460.0;

/// The tile's four frames, in draw order up the column.
pub(crate) struct Layout {
    pub(crate) still: Rect,
    pub(crate) caption: Rect,
    pub(crate) credits: Rect,
    pub(crate) next: Rect,
}

/// PURE — the whole column from the two MEASURED pill widths, so the draw and the pointer hit-test
/// are one arrangement and the arrangement is host-testable. Measuring is the impure half and lives
/// in [`layout`]: `text_width` is `TTF_SizeUTF8`, which the host suite cannot even link against.
pub(crate) fn layout_of(next_w: f32, credits_w: f32) -> Layout {
    use crate::ui::player_hud::{CTRL_H, CTRL_RIGHT, CTRL_Y};
    let next = Rect::new(CTRL_RIGHT - next_w, CTRL_Y, next_w, CTRL_H);
    let credits = Rect::new(next.x - PILL_GAP - credits_w, next.y, credits_w, next.h);
    let caption = Rect::new(CTRL_RIGHT - CAPTION_W, CTRL_Y - BTN_GAP - CAPTION_H, CAPTION_W, CAPTION_H);
    let still_h = (next_w * 9.0 / 16.0).round();
    let still = Rect::new(CTRL_RIGHT - next_w, caption.y - CAP_GAP - still_h, next_w, still_h);
    Layout { still, caption, credits, next }
}

/// [`layout_of`] over the two measured widths. The primary takes the same control-row slot the
/// Subtitles/Audio pair occupies — never narrower than the pair it replaces, so the row does not
/// visibly shrink when it appears — while *Watch Credits* takes only its own label's width:
/// deliberately NOT a second `ctrl_slot`, because that floor exists to hold the row's right edge
/// steady, which is the primary's job, and two equal capsules would say the two choices are
/// equivalent.
pub(crate) fn layout() -> Layout {
    layout_of(
        crate::ui::player_hud::ctrl_slot(NEXT_LABEL).w,
        Button::pill_w(CREDITS_LABEL.as_ptr(), theme::size::BODY, false),
    )
}

/// Pointer hit-test: which of the row's two buttons is under (cx, cy) — [`BTN_CREDITS`],
/// [`BTN_NEXT`], or None. The still and its caption are NOT targets: with two actions in the row a
/// click on the artwork has no single obvious meaning, and guessing one is how a stray click starts
/// an episode the user did not ask for. The caller has already established that this owns the row.
pub(crate) fn hit(cx: f32, cy: f32) -> Option<c_int> {
    let l = layout();
    if l.credits.contains(cx, cy) {
        Some(BTN_CREDITS)
    } else if l.next.contains(cx, cy) {
        Some(BTN_NEXT)
    } else {
        None
    }
}

/// The caption — `"Up Next · S2, E4 · Laura"`. PRIMARY and bold, not secondary: it sits at y≈760
/// where the HUD scrim is only ~0.28, so over a bright end card a secondary grey measures ~1.3:1 —
/// invisible. It is also the only thing distinguishing this episode's ID from the now-playing one
/// on the same band, hence the explicit "Up Next ·" kicker rather than a bare "S2, E4".
fn caption(u: &UpNext) -> String {
    if u.season > 0 || u.index > 0 {
        format!("Up Next \u{b7} {}", crate::ui::fmt::episode_kicker(u.season, u.index, &u.ep_title))
    } else {
        format!("Up Next \u{b7} {}", u.ep_title)
    }
}

pub(crate) fn draw(p: Painter, focused: bool, btn: c_int, now: u32) {
    let Some(u) = crate::route::up_next() else { return };
    let l = layout();
    // NOT scaled: `CARD_FOCUS_SCALE` is the terminal value of a focus spring the shelves drive, and
    // passing it as a constant drew the still permanently 7% oversized — overhanging the column on
    // both sides. The still is decoration on a focusable row, not a focusable tile itself, so it
    // keeps the card treatment (sheen + resting shadow) at rest scale.
    draw_card(p, l.still, &u.thumb, (480, 270), 10.0, false, 1.0);

    if let Ok(cs) = CString::new(crate::text::elide(&caption(u), l.caption.w, theme::size::CAPTION, 1, false)) {
        Label::new(cs.as_ptr(), theme::size::CAPTION, theme::TEXT_PRIMARY)
            .bold()
            .h(HAlign::Right)
            .draw(p, l.caption);
    }

    let e = Env::inert();
    Button::new(CREDITS_LABEL.as_ptr(), theme::size::BODY, l.credits)
        .focused(focused && btn == BTN_CREDITS)
        .draw(&e, p);

    // The primary IS the countdown — its fill sweeps left→right and, at the far edge, the episode
    // starts. Driven straight off the remaining MILLISECONDS and redrawn every frame, so the sweep
    // is continuous; the label carries no seconds, because the pill's width is derived from its
    // label and a ticking numeral would resize the button and slide its centred text every second.
    let Ok(label) = CString::new(NEXT_LABEL) else { return };
    let mut b = Button::new(label.as_ptr(), theme::size::BODY, l.next).focused(focused && btn == BTN_NEXT);
    if armed() {
        // It animates from a CLOCK, so `ui::idle`'s spring instrumentation cannot see it — the trap
        // `Xfade::tick` and `Spinner::draw` both shipped frozen in. The player route bypasses the
        // frame gate outright today, so this changes nothing now; it is what keeps that reversible.
        crate::ui::idle::invalidate();
        b = b.progress(1.0 - (remaining_ms(now) as f32 / COUNTDOWN_MS as f32).clamp(0.0, 1.0));
    }
    b.draw(&e, p);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one cancel rule, exhaustively. It is a STEADY STATE, so it has to answer for every
    /// (row focused × which button) pair rather than for a transition: the countdown survives only
    /// while the ring is resting on the primary, which is where the appear edge parks it.
    #[test]
    fn the_countdown_runs_only_while_the_ring_rests_on_the_primary() {
        assert!(countdown_may_run(true, BTN_NEXT), "the resting state the appear edge creates");
        assert!(
            !countdown_may_run(true, BTN_CREDITS),
            "reaching for Watch Credits is reaching for the alternative"
        );
        for b in [BTN_CREDITS, BTN_NEXT] {
            assert!(
                !countdown_may_run(false, b),
                "focus off the row is the user driving the transport, whatever btn still says"
            );
        }
    }

    /// The ring parks on the RIGHT item, which is the one place in the app where a row's primary is
    /// not index 0 — a Skip pill's only item is, and so is the first disc. If this ever silently
    /// became 0 the countdown would arm and be cancelled by its own resting state on the very next
    /// frame, and the tile would look like a timer that never runs.
    #[test]
    fn the_primary_is_the_right_hand_item_and_the_countdown_survives_it() {
        assert_eq!(PRIMARY_BTN, BTN_NEXT);
        assert!(countdown_may_run(true, PRIMARY_BTN));
    }

    /// Geometry, from the design's own column: the two pills share a baseline and the control row's
    /// right edge, Watch Credits sits `space::SM` to the LEFT of the primary (so LEFT reaches it),
    /// and the still shares the primary's width and edge in a 16:9 frame. Driven through the PURE
    /// half with the two widths supplied, because the impure one measures with `TTF_SizeUTF8` and
    /// the host suite cannot link SDL2_ttf at all.
    #[test]
    fn the_column_is_right_anchored_on_one_edge() {
        // the real orders of magnitude: `ctrl_slot`'s floor is 236, and "Watch Credits" measures a
        // little under it — which is the asymmetry the design is making a point of
        let l = layout_of(243.0, 226.0);
        let (n, c, t, cap) = (l.next, l.credits, l.still, l.caption);
        let right = crate::ui::player_hud::CTRL_RIGHT;
        assert_eq!(n.x + n.w, right, "the primary holds the row's right edge");
        assert_eq!(t.x + t.w, right, "…and the still shares it");
        assert_eq!(cap.x + cap.w, right, "…and the caption's right-aligned run is measured from it");
        assert_eq!(t.w, n.w, "one column, one pair of edges");
        assert_eq!(t.h, (n.w * 9.0 / 16.0).round(), "16:9 — it is a video frame");
        assert!(c.x + c.w < n.x, "Watch Credits is drawn LEFT of the primary, so LEFT reaches it");
        assert_eq!(n.x - (c.x + c.w), PILL_GAP);
        assert_eq!(c.y, n.y, "the pair shares a baseline");
        assert_eq!(c.h, n.h);
        // stacked bottom-up with the unequal gaps that do the grouping
        assert_eq!(cap.y + cap.h + BTN_GAP, n.y);
        assert_eq!(t.y + t.h + CAP_GAP, cap.y);
        assert!(CAP_GAP < BTN_GAP, "the caption belongs to the still, not to the buttons");
        // The whole tile sits ABOVE the control row's own band and inside the frame — it is the
        // tallest thing the transport ever puts there, and the still is what could run off the top.
        assert!(t.y > 0.0, "the column fits on the panel");
        assert!(c.x > 0.0, "…and does not run off its left edge");
    }

    /// The caption names the successor and says so: the kicker survives, the address comes from the
    /// shared `fmt::episode_kicker` (so the HUD, the pre-roll ctx line and this cannot drift), and an
    /// unaddressed leaf degrades to the bare title instead of drawing "S0, E0".
    #[test]
    fn the_caption_is_the_shared_episode_vocabulary_behind_an_up_next_kicker() {
        let ep = UpNext { season: 2, index: 4, ep_title: "Laura".into(), ..Default::default() };
        assert_eq!(caption(&ep), "Up Next \u{b7} S2, E4 \u{b7} Laura");
        let bare = UpNext { ep_title: "Laura".into(), ..Default::default() };
        assert_eq!(caption(&bare), "Up Next \u{b7} Laura", "no address, no empty ordinal");
    }
}
