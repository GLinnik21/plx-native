//! **`HudGeometry`: what the layout reports back.** The facts that FOCUS, the POINTER, the overscan
//! audit and the control-row STAND-INS (Skip, Up Next) need to know about where the HUD is, as values
//! rather than as the classic layout's constants.
//!
//! The classic layout (`classic.rs`) is the one that fills it today: `classic::frame_geometry()` for the
//! facts that do not depend on what occupies the row, `classic::geometry(..)` for the whole of it. A
//! consumer reads the report and so is indifferent to which layout produced it: the player's
//! `Focusable` (group extents and row length), the pointer stops, `overscan_rects` and the stand-ins
//! (`up_next`, `skip_pill`) never name `CTRL_Y`, `CTRL_H`, `CTRL_RIGHT` or `scrub_hit_rect` themselves.
//!
//! This module also names the HUD's focus GROUPS. They are the shared `FocusEngine` groups the
//! player's `Focusable` declares, and the one set of numbers the HUD's motion and drawing compare a
//! focus against, so the screen's `nav.focus` is a [`GroupId`] and not a bare integer.

use super::state::BTN_N;
use plx_machine::machine::GroupId;
use plx_media::player::text_style::HudBand;
use plx_ui::Rect;

/// The scrubber's group: a free-form region, extent [`HudGeometry::grab`].
pub const GROUP_SCRUB: GroupId = GroupId(0);
/// The control row's group: the discs, the Skip pill or the Up Next pair, one stop per item.
pub const GROUP_ROW: GroupId = GroupId(1);
/// The bottom tab pills' group: Info, then Chapters when the item has any.
pub const GROUP_TABS: GroupId = GroupId(2);
/// The failure read-out's action row.
pub const GROUP_FAILURE: GroupId = GroupId(3);

/// How many tabs the bottom row can hold: Info, then Chapters.
pub(super) const HUD_TABS: usize = 2;

/// Where the control row hangs: its right edge, its top and its height. The stand-ins that take
/// the row's place (Skip, Up Next) land on exactly these or the transport visibly jumps when a
/// segment begins; the classic layout reports its `CTRL_RIGHT`, `CTRL_Y` and `CTRL_H`.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct RowAnchor {
    /// the row's right edge
    pub right: f32,
    /// the row's top
    pub top: f32,
    /// the row's height (one disc's diameter)
    pub h: f32,
}

/// The rects a layout reports for one frame. Built by `classic::frame_geometry()` (the
/// slot-independent fields, `items` and `tabs` left `None`) or by `classic::geometry(..)` (all of it).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct HudGeometry {
    /// The scrubber's GRAB band: deliberately much taller than the bar, because it is a pointer
    /// grab zone, and registered as the LOWEST stop so every control over it outranks it (#162).
    pub grab: Rect,
    /// The bar itself.
    pub playbar: Rect,
    /// The control row's band at its floor width: the row GROUP's extent only, not any item's hit
    /// region.
    pub row: Rect,
    /// Where the row hangs; see [`RowAnchor`].
    pub row_anchor: RowAnchor,
    /// Item `i` of whatever occupies the row, at the rect it is DRAWN at; `None` past the
    /// occupant's last item and at an absent disc.
    pub items: [Option<Rect>; BTN_N as usize],
    /// The bottom tab row's origin and height, at its narrowest (no width): the overscan audit's
    /// "player bottom tab row".
    pub tab_row: Rect,
    /// The title block's origin: the overscan audit's "player title / elapsed clock".
    pub title: Rect,
    /// Tab `i`'s pill, `None` for a tab the row does not carry.
    pub tabs: [Option<Rect>; HUD_TABS],
}

impl HudGeometry {
    /// **The caption band this layout occupies** (`docs/hud-plugins.md`, "Captions: the band a HUD
    /// occupies"): the compact row's top, the highest interactive extent, whichever slot occupies
    /// the row, because the Skip and Up Next stand-ins land on `row_anchor` exactly. A value from
    /// the layout, so the caption draws and the libass lift take it rather than a constant.
    pub fn band(&self) -> HudBand {
        HudBand::from_controls_top(self.row_anchor.top)
    }

    /// **Where along the bar a pointer at `x` is pointing**, `0..1`. X only, on purpose: an engaged
    /// drag tracks the pointer even when it wanders off the band vertically, which is why
    /// `PlayerScreen::handle_drag` never re-tests the hit once a click has seated the gesture.
    pub fn scrub_frac(&self, x: f32) -> f32 {
        ((x - self.playbar.x) / self.playbar.w).clamp(0.0, 1.0)
    }
}
