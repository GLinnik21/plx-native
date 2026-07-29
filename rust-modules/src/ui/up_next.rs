//! **Up Next** — the next-episode tile in the transport HUD. It appears when the playing episode
//! reaches its CREDITS marker and the PlayQueue has another episode behind it: the next episode's
//! still, its numbering and name, and a countdown that starts it on its own.
//!
//! The data is `route::up_next()` — lifted from the `continuous=1` PlayQueue that every playback
//! already creates, so nothing here asks the server what plays next.
//!
//! It TAKES THE PLACE of the transport's Subtitles/Audio discs while it is up — the same control
//! row the Skip button uses — with the next episode's still stacked directly above it. The
//! countdown is the button's own fill sweep (`Button::progress`), so the control and the timer are
//! one object rather than a button next to a rail.
//!
//! It is HUD furniture, not an overlay: playback keeps running underneath and the credits keep
//! rolling. That is what makes the countdown honest — `app.rs` holds the HUD up for as long as it
//! is armed, so the timer is never running behind a hidden transport, and moving focus off the
//! button cancels it (engaging the transport is not consent to be yanked into the next episode).
#![allow(dead_code)]
use crate::route::UpNext;
use crate::ui::label::{HAlign, Label};
use crate::ui::theme;
use crate::ui::widgets::{draw_card, Button};
use crate::ui::{Env, Painter, Rect, View};
use std::ffi::CString;
use std::ptr::{addr_of, addr_of_mut};

/// How long the tile holds before starting the next episode by itself. Long enough to read the
/// title and reach for the remote, short enough that "always the next episode" stays automatic.
pub(crate) const COUNTDOWN_MS: u32 = 10_000;

/// `SDL_GetTicks` deadline at which the next episode auto-starts. 0 = not armed (the tile is not
/// up, or the countdown was cancelled).
static mut DEADLINE: u32 = 0;
/// The user cancelled the auto-advance for THIS segment (they moved focus off the tile). Latched,
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

/// Stop the auto-advance without hiding the tile — the user moved focus away, or is leaving. The
/// tile stays up as a plain "OK to play" target; only the clock is off.
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
// takes the BUTTON's width and right edge, so the group is one clean two-edge column — the earlier
// version gave the still its own width and drew it permanently scaled, which put its right edge 8px
// past the pill's and its left 15px inside, a near-miss that reads as unfinished from the couch. ----
/// still → caption: the caption is the still's label, so it hugs it…
const CAP_GAP: f32 = theme::space::XS;
/// …and the button is a separate actionable object, so it gets more air. Unequal gaps are what
/// create the grouping; the previous 8/8 left the caption belonging to neither.
const BTN_GAP: f32 = theme::space::MD;
const CAPTION_H: f32 = 30.0;

/// The button's rect — the same control-row slot the Subtitles/Audio pair occupies, and at least
/// as wide as that pair so the row does not visibly shrink when it appears.
pub(crate) fn rect() -> Rect {
    crate::ui::player_hud::ctrl_slot(LABEL)
}

const LABEL: &str = "Next Episode";

/// The caption is RIGHT-ALIGNED text, so it may run wider than the still/button column without
/// breaking it — the column's edges are the two solid rectangles, and a text run has no left edge
/// to break. It gets its own budget because clamping it to the button's width elided the episode
/// NAME, which is the one thing the caption is for.
const CAPTION_W: f32 = 460.0;

fn caption_rect() -> Rect {
    let y = crate::ui::player_hud::CTRL_Y - BTN_GAP - CAPTION_H;
    Rect::new(crate::ui::player_hud::CTRL_RIGHT - CAPTION_W, y, CAPTION_W, CAPTION_H)
}

fn thumb_rect() -> Rect {
    let c = caption_rect();
    let w = rect().w; // the button's width — one column, one pair of edges
    let h = (w * 9.0 / 16.0).round();
    Rect::new(crate::ui::player_hud::CTRL_RIGHT - w, c.y - CAP_GAP - h, w, h)
}

/// Pointer hit-test — the still, its caption and the button are one target. The caller has already
/// established that this owns the row, so there is no second `is_shown` check here.
pub(crate) fn hit(cx: f32, cy: f32) -> bool {
    let (b, t) = (rect(), thumb_rect());
    let x = t.x.min(b.x);
    Rect::new(x, t.y, (b.x + b.w) - x, (b.y + b.h) - t.y).contains(cx, cy)
}

pub(crate) fn draw(p: Painter, focused: bool, now: u32) {
    let Some(u) = crate::route::up_next() else { return };
    let btn = rect();
    let t = thumb_rect();
    // NOT scaled: `CARD_FOCUS_SCALE` is the terminal value of a focus spring the shelves drive, and
    // passing it as a constant drew the still permanently 7% oversized — overhanging the column on
    // both sides. The still is decoration on a focusable button, not a focusable tile itself, so it
    // keeps the card treatment (sheen + resting shadow) at rest scale.
    draw_card(p, t, &u.thumb, (480, 270), 10.0, false, 1.0);

    // Caption: PRIMARY and bold, not secondary. It sits at y≈760 where the HUD scrim is only ~0.28,
    // so over a bright end card a secondary grey measures ~1.3:1 — invisible. It is also the only
    // thing distinguishing this episode ID from the now-playing one on the same band, hence the
    // explicit "Up Next ·" kicker rather than a bare "S2, E3".
    let name = if u.season > 0 || u.index > 0 {
        format!("Up Next \u{b7} S{}, E{} \u{b7} {}", u.season, u.index, u.ep_title)
    } else {
        format!("Up Next \u{b7} {}", u.ep_title)
    };
    let cap = caption_rect();
    if let Ok(cs) = CString::new(crate::text::elide(&name, cap.w, theme::size::CAPTION, 1, false)) {
        Label::new(cs.as_ptr(), theme::size::CAPTION, theme::TEXT_PRIMARY)
            .bold()
            .h(HAlign::Right)
            .draw(p, cap);
    }

    // The button IS the countdown — its fill sweeps left→right and, at the far edge, the episode
    // starts. Driven straight off the remaining MILLISECONDS and redrawn every frame, so the sweep
    // is continuous; the label carries no seconds, because the pill's width is derived from its
    // label and a ticking numeral would resize the button and slide its centred text every second.
    let Ok(label) = CString::new(LABEL) else { return };
    let mut b = Button::new(label.as_ptr(), theme::size::BODY, btn).focused(focused);
    if armed() {
        b = b.progress(1.0 - (remaining_ms(now) as f32 / COUNTDOWN_MS as f32).clamp(0.0, 1.0));
    }
    b.draw(&Env::inert(), p);
}
