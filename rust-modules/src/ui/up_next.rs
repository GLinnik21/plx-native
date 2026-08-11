//! **Up Next** — the post-play card, drawn over the live credits. It appears when the playing
//! episode reaches its CREDITS marker and the PlayQueue has another episode behind it
//! (`Plex Pass Awareness.dc.html`, deliverable D): a full-screen horizontal scrim, the show's
//! portrait poster, the next episode's name and address, and a countdown RING beside two pills —
//! **Play now** (filled, the focus start) and **Watch credits** (keyline).
//!
//! The data is `route::up_next()` — lifted from the `continuous=1` PlayQueue that every playback
//! already creates, so nothing here asks the server what plays next.
//!
//! While it is up the card **owns the screen**: `player_hud::draw_hud` draws it INSTEAD of the
//! transport (no scrubber, no discs, no title chrome — the gate is [`card_active`]), and `app.rs`
//! routes the player's keys into [`card_transition`]'s table. The countdown still means what the
//! old control-row tile's meant: it arms the frame the card appears ([`tick`]), cancels — latched
//! per segment — the moment focus reaches Watch credits, and its expiry is `app.rs`'s existing
//! auto-advance poll ([`expired`]). BACK adds a second per-segment latch: it hides the card and
//! leaves playback running, because the credits were the point.
//!
//! It is HUD furniture, not an overlay route: playback keeps running underneath and the credits
//! keep rolling through the right, lightly-scrimmed side of the gradient.
#![allow(dead_code)]
use crate::route::UpNext;
use crate::ui::consts::{SCR_H, SCR_W};
use crate::ui::label::{HAlign, Label, VAlign};
use crate::ui::theme;
use crate::ui::widgets::{draw_card, Button, ControlStyle};
use crate::ui::{Env, Painter, Rect, View};
use std::ffi::CString;
use std::ptr::{addr_of, addr_of_mut};

/// How long the card holds before starting the next episode by itself. Long enough to read the
/// title and reach for the remote, short enough that "always the next episode" stays automatic.
pub(crate) const COUNTDOWN_MS: u32 = 10_000;

/// `SDL_GetTicks` deadline at which the next episode auto-starts. 0 = not armed (the card is not
/// up, or the countdown was cancelled).
static mut DEADLINE: u32 = 0;
/// The user cancelled the auto-advance for THIS segment (focus reached Watch credits, or BACK).
/// Latched, because `tick` runs every frame and would otherwise re-arm the countdown the very
/// next one. Cleared when the segment ends, so the next episode's credits get their own chance.
static mut CANCELLED: bool = false;
/// BACK dismissed the whole card for THIS segment — playback keeps running, the transport comes
/// back, and the card stays away until the segment ends. Same per-segment lifetime as
/// [`CANCELLED`], cleared in the same [`reset`].
static mut HIDDEN: bool = false;
/// The card's own cursor: 0 = Play now (the initial focus, per segment), 1 = Watch credits.
static mut FOCUS: i32 = 0;

/// Whether this owns the control row this frame. The precedence itself lives in ONE place —
/// [`crate::ui::player_hud::slot_for`] — so this is just a read of the resolved slot.
pub(crate) fn is_shown(slot: crate::ui::player_hud::ControlSlot) -> bool {
    matches!(slot, crate::ui::player_hud::ControlSlot::UpNext(_))
}

/// PURE — is the card on screen? The slot must offer it AND the user must not have dismissed it
/// with BACK this segment. The one gate `draw_hud`, every key arm and both pointer paths read, so
/// what is drawn and what dispatches can never be two different answers.
pub(crate) fn card_active_for(shown: bool, hidden: bool) -> bool {
    shown && !hidden
}

/// [`card_active_for`] over the live latch — the impure sampler every `app.rs` arm calls.
pub(crate) fn card_active(slot: crate::ui::player_hud::ControlSlot) -> bool {
    card_active_for(is_shown(slot), unsafe { addr_of!(HIDDEN).read() })
}

/// The card's own cursor (0 = Play now, 1 = Watch credits).
pub(crate) fn focus() -> i32 {
    unsafe { addr_of!(FOCUS).read() }
}

/// Per-frame tick: arm the countdown the moment the card appears, and forget everything about it
/// once the segment is over. Idempotent — an already-running countdown is never pushed forward.
/// A BACK-dismissed segment does not re-arm ([`CANCELLED`] is set with [`HIDDEN`]), and both
/// latches — plus the cursor — drop together when the slot moves on.
pub(crate) fn tick(slot: crate::ui::player_hud::ControlSlot, now: u32) {
    if !is_shown(slot) {
        // segment over (or the queue emptied): drop the deadline AND the per-segment latches, so
        // the next episode's credits arm normally instead of inheriting this one's refusal
        reset();
        return;
    }
    if (unsafe { addr_of!(DEADLINE).read() }) == 0 && !(unsafe { addr_of!(CANCELLED).read() }) {
        unsafe { addr_of_mut!(DEADLINE).write(now.wrapping_add(COUNTDOWN_MS).max(1)) }
    }
}

/// Stop the auto-advance without hiding the card — focus reached Watch credits, or the user is
/// leaving. The card stays up as a plain "OK to play" target; only the clock is off.
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

/// Per-session reset — a new playback must not inherit the previous episode's countdown, its
/// cancel/dismiss latches, or where its cursor was parked.
pub(crate) fn reset() {
    unsafe {
        addr_of_mut!(DEADLINE).write(0);
        addr_of_mut!(CANCELLED).write(false);
        addr_of_mut!(HIDDEN).write(false);
        addr_of_mut!(FOCUS).write(0);
    }
}

// ---- the key table -------------------------------------------------------------------------

/// A player key while the card is up. `Ok` is here so the whole interaction is ONE table; the
/// caller performs the start (`play_up_next`) when the table says so.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CardKey {
    Left,
    Right,
    Ok,
    Back,
    Up,
    Down,
}

/// What one key does to the card — the transition as DATA, so it is host-testable and the arms in
/// `app.rs` cannot each spell a private variant of it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct CardFx {
    /// the cursor after the key
    pub(crate) focus: i32,
    /// stop the countdown (latched — [`CANCELLED`], so nothing re-arms this segment)
    pub(crate) cancel: bool,
    /// dismiss the card for this segment ([`HIDDEN`]); playback continues underneath
    pub(crate) hide: bool,
    /// start the successor now (OK on Play now) — performed by the caller
    pub(crate) play: bool,
}

/// PURE — the card's whole key behaviour. The design's prose says "LEFT reaches Watch credits",
/// but its own DRAWING puts Watch credits to the RIGHT of Play now — and on a television the
/// arrow must move focus the way it points, so the drawing wins and the sentence is read as a
/// mock slip (flagged at implementation, resolved spatially):
///
/// * RIGHT reaches Watch credits, and REACHING it is the cancel — the same focus-off-cancels
///   semantics the control-row tile had. LEFT returns to Play now and does NOT re-arm (the
///   cancel is latched per segment).
/// * OK on Play now plays; OK on Watch credits is nothing beyond the cancel focusing it already
///   performed.
/// * BACK hides the card for this segment and cancels; playback keeps running.
/// * UP/DOWN are nothing — the card owns the screen, there is no transport to walk to.
pub(crate) fn card_transition(key: CardKey, focus: i32) -> CardFx {
    let keep = CardFx { focus, cancel: false, hide: false, play: false };
    match key {
        CardKey::Right => CardFx { focus: 1, cancel: true, ..keep },
        CardKey::Left => CardFx { focus: 0, ..keep },
        CardKey::Ok if focus == 0 => CardFx { play: true, ..keep },
        CardKey::Ok => keep,
        CardKey::Back => CardFx { cancel: true, hide: true, ..keep },
        CardKey::Up | CardKey::Down => keep,
    }
}

/// Apply [`card_transition`] to the live latches. Returns the table's `play` verdict — the one
/// effect the module cannot perform itself (`app.rs`'s `play_up_next` owns the start ritual).
pub(crate) fn handle_key(key: CardKey) -> bool {
    let fx = card_transition(key, focus());
    unsafe { addr_of_mut!(FOCUS).write(fx.focus) };
    if fx.cancel {
        cancel();
    }
    if fx.hide {
        unsafe { addr_of_mut!(HIDDEN).write(true) };
    }
    fx.play
}

// ---- geometry: authored 1080p, left- and bottom-anchored at the HUD margin. The credits keep
// the right two-thirds of the frame; the gradient below is what keeps this column legible. ----

/// Left/bottom anchor — the transport's own 90px margin (`player_hud::SB_X`), so the card sits
/// where the HUD's title block does.
const MARGIN: f32 = 90.0;
/// The show poster, the same 250×375 card the detail page and the home shelves resolve.
const POSTER_W: f32 = 250.0;
const POSTER_H: f32 = 375.0;
const POSTER_RAD: f32 = 10.0;
/// poster → text column (the mock's 36 at 1080p — card-geometry, like `consts.rs`'s gaps, not a
/// stacked-block rhythm gap, which is what the `theme::space` rungs are for)
const COL_GAP: f32 = 36.0;
/// The elide budget for the title and meta line: the column may run a little past the gradient's
/// 46% knee, but never into the right third the credits keep.
const TEXT_W: f32 = 700.0;
/// countdown ring box / radius / stroke (design: "66px box, radius 29, stroke 5")
const RING_S: f32 = 66.0;
const RING_R: f32 = 29.0;
const RING_STROKE: f32 = 5.0;
/// one dot per ~2px of circumference reads as a solid stroke with free round caps
const RING_DOT_STEP: f32 = 2.0;
/// the action pills (h 60 → the r30 capsule falls out of `Button`'s h/2 radius)
const PILL_H: f32 = 60.0;
/// ring → pill → pill — the mock's 40, which IS the block rung
const ROW_GAP: f32 = theme::space::LG;

/// The full-screen ground: black at three stops across the width (the mock's .94 → .82 at 46% →
/// .5), drawn as two `grad4` quads — the renderer's one 2-D gradient — sharing the middle stop.
const GRAD_A0: f32 = 0.94;
const GRAD_A1: f32 = 0.82;
const GRAD_A2: f32 = 0.50;
const GRAD_MID: f32 = 0.46;

const PLAY_LABEL: &core::ffi::CStr = c"Play now";
const CREDITS_LABEL: &core::ffi::CStr = c"Watch credits";

/// The kicker's letter-tracking — the mock's `.12em` of MICRO, drawn per character
/// (`widgets::PASS_TRACK`'s idiom: the renderer has no letter-spacing, and seven constant
/// one-char strings never churn the glyph cache).
const KICKER: &str = "UP NEXT";
const KICKER_TRACK: f32 = theme::size::MICRO as f32 * 0.12;

/// The action row's three frames: (ring box, Play now, Watch credits). ONE geometry for the draw
/// and the pointer hit-test.
fn row_rects() -> (Rect, Rect, Rect) {
    let col_x = MARGIN + POSTER_W + COL_GAP;
    let row_top = SCR_H - MARGIN - RING_S;
    let ring = Rect::new(col_x, row_top, RING_S, RING_S);
    let pill_y = row_top + (RING_S - PILL_H) * 0.5;
    let pw = Button::pill_w(PLAY_LABEL.as_ptr(), theme::size::BODY, false);
    let play = Rect::new(ring.x + RING_S + ROW_GAP, pill_y, pw, PILL_H);
    let cw = Button::pill_w(CREDITS_LABEL.as_ptr(), theme::size::BODY, false);
    let credits = Rect::new(play.x + play.w + ROW_GAP, pill_y, cw, PILL_H);
    (ring, play, credits)
}

/// Pointer hit-test: which of the card's two buttons is under (cx, cy) — 0 = Play now,
/// 1 = Watch credits, None elsewhere. The caller has already established the card is active.
pub(crate) fn hit(cx: f32, cy: f32) -> Option<i32> {
    let (_, play, credits) = row_rects();
    if play.contains(cx, cy) {
        Some(0)
    } else if credits.contains(cx, cy) {
        Some(1)
    } else {
        None
    }
}

/// Move the cursor to button `b`. Reaching Watch credits IS the cancel — pointer and key alike,
/// one rule ([`card_transition`]'s LEFT row).
fn point_focus(b: i32) {
    unsafe { addr_of_mut!(FOCUS).write(b) };
    if b == 1 {
        cancel();
    }
}

/// Pointer hover: the cursor follows the pointer over the two buttons (`more_menu::pointer_focus`'s
/// contract — a miss moves nothing).
pub(crate) fn pointer_focus(mx: f32, my: f32) {
    if let Some(b) = hit(mx, my) {
        point_focus(b);
    }
}

/// Pointer click: park the cursor on the button under the click (cancelling if it is Watch
/// credits, exactly as focusing it does) and report whether a button was hit at all — the caller
/// then dispatches OK on the parked cursor, so a click and a key press are one code path.
pub(crate) fn pointer_press(cx: f32, cy: f32) -> bool {
    match hit(cx, cy) {
        Some(b) => {
            point_focus(b);
            true
        }
        None => false,
    }
}

// ---- the countdown ring --------------------------------------------------------------------

/// PURE — the arc as dot centres: `frac` of a full turn, swept COUNTER-CLOCKWISE from 12 o'clock
/// (design: "at 10ft an arc reads as remaining time, a sweeping fill reads as buffering"), one dot
/// every ~`step` px of circumference, both endpoints included. The SDF painter has no arc
/// primitive; a full-radius rrect is a circle, and overlapping 5px dots every 2px read as a solid
/// stroke with round caps for free (~92 dots for the full track at r=29).
///
/// Screen coordinates (y down): 12 o'clock is angle −π/2, and counter-clockwise on the PANEL is
/// decreasing angle.
pub(crate) fn ring_dots(cx: f32, cy: f32, r: f32, frac: f32, step: f32) -> Vec<(f32, f32)> {
    let sweep = frac.clamp(0.0, 1.0) * std::f32::consts::TAU;
    let n = ((sweep * r / step).ceil() as usize).max(1) + 1;
    (0..n)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 - sweep * (i as f32 / (n - 1) as f32);
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

fn dot(p: Painter, x: f32, y: f32, col: [f32; 4]) {
    let d = RING_STROKE * 0.5;
    p.rect(Rect::new(x - d, y - d, RING_STROKE, RING_STROKE), d, col, col, 0.0);
}

/// Track (full circle, the rail's own white .2, stroke 5), the remaining arc in ACCENT, and the
/// remaining-seconds numeral centred in the box. When the countdown is off (cancelled/expired)
/// only the quiet track remains, so the row keeps its shape.
fn draw_ring(p: Painter, r: Rect, now: u32) {
    // No countdown, no ring AT ALL: once the user cancels, an empty track circle beside the
    // pills reads as a broken element, not a stopped timer (seen on the first device photo).
    // The row keeps the ring's slot so the pills do not shuffle when the clock stops.
    if !armed() {
        return;
    }
    // The track's dots are TRANSLUCENT, so their ~2.5-deep overlap (5px dots every 2px) would
    // composite ~.43 where the token says .20 — compensate per dot for the overlap depth, so the
    // drawn ring lands back on the token's weight. The ACCENT arc needs none: its alpha is 1.
    let k = RING_DOT_STEP / RING_STROKE; // 1 / overlap depth
    let track = theme::with_a(theme::RAIL_TRACK, 1.0 - (1.0 - theme::RAIL_TRACK[3]).powf(k));
    for (x, y) in ring_dots(r.cx(), r.cy(), RING_R, 1.0, RING_DOT_STEP) {
        dot(p, x, y, track);
    }
    // The ring is driven by a CLOCK, not a spring, so `ui::idle`'s spring instrumentation cannot
    // see it — the exact trap `Xfade::tick` and `Spinner::draw` both shipped frozen in. Reported
    // here, in the draw, for `Spinner`'s reasons: only a ring actually ON SCREEN holds the loop
    // awake, and a draw-side report can only ever latch the gate ON for the next frame.
    crate::ui::idle::invalidate();
    let left = remaining_ms(now);
    let frac = left as f32 / COUNTDOWN_MS as f32;
    for (x, y) in ring_dots(r.cx(), r.cy(), RING_R, frac, RING_DOT_STEP) {
        dot(p, x, y, theme::ACCENT);
    }
    // numeral = ceil(remaining seconds), so the card never shows "0" while it still has time
    if let Ok(s) = CString::new(left.div_ceil(1000).to_string()) {
        Label::new(s.as_ptr(), theme::size::LABEL, theme::TEXT_PRIMARY)
            .bold()
            .h(HAlign::Center)
            .draw(p, r);
    }
}

// ---- the card ------------------------------------------------------------------------------

/// One text line of the column, cap-band-placed with its cap TOP at `top`; returns nothing —
/// the column is laid out bottom-up by [`draw`]'s cursor.
fn col_line(p: Painter, text: &str, top: f32, sz: i32, bold: bool, col: [f32; 4]) {
    let Ok(cs) = CString::new(crate::text::elide(text, TEXT_W, sz, bold as i32, false)) else {
        return;
    };
    let mut l = Label::new(cs.as_ptr(), sz, col).v(VAlign::CapTop);
    if bold {
        l = l.bold();
    }
    l.draw(p, Rect::new(MARGIN + POSTER_W + COL_GAP, top, TEXT_W, 0.0));
}

/// The full-card draw — everything on screen while the card owns it. `draw_hud` calls this
/// INSTEAD of the transport (see [`card_active`]); playback runs underneath.
pub(crate) fn draw(p: Painter, now: u32) {
    let Some(u) = crate::route::up_next() else { return };

    // The ground: one horizontal gradient across the whole frame — solid enough on the left for
    // a text column over arbitrary credits, light enough on the right that the credits stay the
    // point. Two quads because `grad4` is bilinear per quad and the mock has three stops.
    let mid = (SCR_W * GRAD_MID).round();
    let (a0, a1, a2) = (theme::scrim_black(GRAD_A0), theme::scrim_black(GRAD_A1), theme::scrim_black(GRAD_A2));
    p.grad4(Rect::new(0.0, 0.0, mid, SCR_H), [a0, a1, a1, a0]);
    p.grad4(Rect::new(mid, 0.0, SCR_W - mid, SCR_H), [a1, a2, a2, a1]);

    // The poster: the SHOW's portrait poster (QueueRow carries `grandparentThumb`), the same
    // 250×375 card the detail page uses; `card_art` falls back to the episode still, which the
    // image transcoder's `minSize=1` fill crops into the portrait frame server-side.
    let poster = Rect::new(MARGIN, SCR_H - MARGIN - POSTER_H, POSTER_W, POSTER_H);
    draw_card(p, poster, u.card_art(), (POSTER_W as i32, POSTER_H as i32), POSTER_RAD, false, 1.0);

    // The action row, bottom-anchored beside the poster's own bottom edge.
    let (ring, play_r, cred_r) = row_rects();
    draw_ring(p, ring, now);
    let e = Env::inert();
    let f = focus();
    Button::new(PLAY_LABEL.as_ptr(), theme::size::BODY, play_r).focused(f == 0).draw(&e, p);
    Button::new(CREDITS_LABEL.as_ptr(), theme::size::BODY, cred_r)
        .style(ControlStyle::Keyline)
        .focused(f == 1)
        .draw(&e, p);

    // The text column, stacked bottom-up above the row: meta, title, kicker. Lines that have
    // nothing to say are skipped and the stack closes over them.
    let mut bottom = ring.y - ROW_GAP;
    let meta = {
        let mut m = if u.season > 0 || u.index > 0 {
            crate::ui::fmt::episode_address(u.season, u.index)
        } else {
            String::new()
        };
        if u.dur_ms > 0 {
            if !m.is_empty() {
                m.push_str(" \u{b7} ");
            }
            m.push_str(&crate::ui::fmt::dur_long(u.dur_ms));
        }
        m
    };
    if !meta.is_empty() {
        let h = crate::text::cap_h(theme::size::CAPTION, 0);
        col_line(p, &meta, bottom - h, theme::size::CAPTION, false, theme::TEXT_SECONDARY);
        bottom -= h + theme::space::SM;
    }
    if !u.ep_title.is_empty() {
        let h = crate::text::cap_h(theme::size::DISPLAY, 1);
        col_line(p, &u.ep_title, bottom - h, theme::size::DISPLAY, true, theme::TEXT_PRIMARY);
        bottom -= h + theme::space::SM;
    }
    // the kicker, tracked per character (see KICKER_TRACK)
    {
        let (cap_top, _) = crate::text::text_cap_band(theme::size::MICRO, 1);
        let h = crate::text::cap_h(theme::size::MICRO, 1);
        let ty = (bottom - h) - cap_top;
        let mut x = MARGIN + POSTER_W + COL_GAP;
        for ch in KICKER.chars() {
            let Ok(c) = CString::new(ch.to_string()) else { continue };
            p.text(c.as_ptr(), x, ty, theme::size::MICRO, theme::TEXT_SECONDARY, 0, 1);
            x += crate::text::text_width(c.as_ptr(), theme::size::MICRO, 1) + KICKER_TRACK;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- the key table: key × focus → focus / cancel / hide / play -------------------------

    /// The design's own interaction line, exhaustively: LEFT reaches Watch credits AND cancels
    /// (from either start), RIGHT returns to Play now and never re-arms, OK plays only on Play
    /// now, BACK hides + cancels wherever the cursor is, UP/DOWN change nothing.
    #[test]
    fn the_card_key_table_is_the_designs_interaction_line() {
        // NB the keys are SPATIAL — Watch credits is DRAWN to the right of Play now, so RIGHT
        // reaches it (the design's prose says LEFT, contradicted by its own drawing; the
        // drawing wins on a television).
        for f in [0, 1] {
            assert_eq!(
                card_transition(CardKey::Right, f),
                CardFx { focus: 1, cancel: true, hide: false, play: false },
                "RIGHT from {f} reaches Watch credits, and reaching it cancels"
            );
            assert_eq!(
                card_transition(CardKey::Left, f),
                CardFx { focus: 0, cancel: false, hide: false, play: false },
                "LEFT from {f} — moving back must NOT re-arm (no cancel to undo, no play)"
            );
            assert_eq!(
                card_transition(CardKey::Back, f),
                CardFx { focus: f, cancel: true, hide: true, play: false },
                "BACK from {f} hides this segment's card and stops the clock"
            );
            for k in [CardKey::Up, CardKey::Down] {
                assert_eq!(
                    card_transition(k, f),
                    CardFx { focus: f, cancel: false, hide: false, play: false },
                    "{k:?} from {f} is nothing — the card owns the screen"
                );
            }
        }
        assert_eq!(
            card_transition(CardKey::Ok, 0),
            CardFx { focus: 0, cancel: false, hide: false, play: true },
            "OK on Play now starts the successor"
        );
        assert_eq!(
            card_transition(CardKey::Ok, 1),
            CardFx { focus: 1, cancel: false, hide: false, play: false },
            "OK on Watch credits is nothing beyond the cancel focusing it already did"
        );
    }

    // ---- the card-active gate --------------------------------------------------------------

    /// The one gate every arm reads: the slot must offer the card AND the user must not have
    /// BACK-dismissed it this segment. A dismissed card is off screen, so nothing may draw it or
    /// dispatch to it — while the slot alone (which still says UpNext) keeps `tick` from
    /// clearing the per-segment latches early.
    #[test]
    fn the_card_is_active_only_while_offered_and_not_dismissed() {
        assert!(card_active_for(true, false));
        assert!(!card_active_for(true, true), "BACK-dismissed: offered but off screen");
        assert!(!card_active_for(false, false), "no offer, no card");
        assert!(!card_active_for(false, true));
    }

    // ---- the ring's dot math ---------------------------------------------------------------

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    /// A full track at the design's numbers (r=29, ~2px step): ~92 dots, both endpoints at
    /// 12 o'clock, and every adjacent pair within a step of its neighbour — that is what makes
    /// overlapping dots read as one solid stroke.
    #[test]
    fn a_full_ring_closes_at_twelve_oclock_and_stays_dense() {
        let (cx, cy, r) = (100.0, 200.0, RING_R);
        let dots = ring_dots(cx, cy, r, 1.0, RING_DOT_STEP);
        let want = (std::f32::consts::TAU * r / RING_DOT_STEP).ceil() as usize + 1;
        assert_eq!(dots.len(), want, "one dot per ~2px of circumference (+ the closing endpoint)");
        assert!(want > 90, "~92 dots at the design's radius/step");
        let first = dots[0];
        let last = dots[dots.len() - 1];
        assert!(close(first.0, cx) && close(first.1, cy - r), "starts at 12 o'clock");
        assert!(close(last.0, first.0) && close(last.1, first.1), "a full turn closes on itself");
        for w in dots.windows(2) {
            let (dx, dy) = (w[1].0 - w[0].0, w[1].1 - w[0].1);
            let d = (dx * dx + dy * dy).sqrt();
            assert!(d > 0.0 || close(first.0, last.0), "dots advance");
            assert!(d <= RING_DOT_STEP + 0.01, "no gap wider than the step (chord ≤ arc)");
        }
    }

    /// The sweep is COUNTER-CLOCKWISE on the panel (y down): the arc leaves 12 o'clock toward
    /// 9 o'clock, a quarter turn ends at 9, a half at 6 — remaining time RETREATING toward 12 as
    /// the fraction shrinks, which is the design's "an arc reads as remaining time".
    #[test]
    fn the_arc_sweeps_counter_clockwise_from_twelve() {
        let (cx, cy, r) = (0.0, 0.0, RING_R);
        let quarter = ring_dots(cx, cy, r, 0.25, RING_DOT_STEP);
        assert!(quarter[1].0 < cx, "the first step off 12 o'clock heads LEFT (toward 9)");
        let q_end = quarter[quarter.len() - 1];
        assert!(close(q_end.0, cx - r) && close(q_end.1, cy), "a quarter turn ends at 9 o'clock");
        let half = ring_dots(cx, cy, r, 0.5, RING_DOT_STEP);
        let h_end = half[half.len() - 1];
        assert!(close(h_end.0, cx) && close(h_end.1, cy + r), "a half turn ends at 6 o'clock");
    }

    /// The empty and out-of-range fractions stay drawable: 0 collapses to the 12 o'clock cap
    /// (round caps come free — a lone dot IS the cap), and a fraction past 1 clamps to the full
    /// turn rather than lapping itself.
    #[test]
    fn empty_and_overfull_fractions_stay_sane() {
        let dots = ring_dots(0.0, 0.0, RING_R, 0.0, RING_DOT_STEP);
        assert_eq!(dots.len(), 2, "the degenerate arc is its endpoints");
        assert!(close(dots[0].0, 0.0) && close(dots[0].1, -RING_R));
        assert!(close(dots[1].0, dots[0].0) && close(dots[1].1, dots[0].1));
        let over = ring_dots(0.0, 0.0, RING_R, 1.5, RING_DOT_STEP);
        let full = ring_dots(0.0, 0.0, RING_R, 1.0, RING_DOT_STEP);
        assert_eq!(over.len(), full.len(), "past-full clamps to one turn");
    }
}
