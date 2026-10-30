//! **`HudGeometry`: what the layout reports back.** The facts that FOCUS, the POINTER, the overscan
//! audit and the control-row STAND-INS (Skip, Up Next) need to know about where the HUD is, as values
//! rather than as the classic layout's constants.
//!
//! **The compiled package fills it** (`CompiledState::geometry`): [`frame_geometry`] for the facts that do
//! not depend on what occupies the row, [`geometry`] for the whole of it, [`row_len`] for how many stops
//! the row group holds. These three are the ONE place a consumer asks where the HUD is, and the only
//! place that names `classic::frame_geometry` / `classic::geometry`: while a screen holds no package, or
//! the package has no state for the frame (the draw then falls back to `classic::draw_hud`, so the
//! geometry takes the same exit), they answer with the hand-written layout's report. A consumer reads the
//! report and so is indifferent to which layout produced it: the player's `Focusable` (group extents and
//! row length), the pointer stops, the caption band and the stand-ins (`up_next`, `skip_pill`) never name
//! `CTRL_Y`, `CTRL_H`, `CTRL_RIGHT` or `scrub_hit_rect` themselves. Stage 3 deletes the classic arm with
//! `draw_hud`.
//!
//! This module also names the HUD's focus GROUPS. They are the shared `FocusEngine` groups the
//! player's `Focusable` declares, and the one set of numbers the HUD's motion and drawing compare a
//! focus against, so the screen's `nav.focus` is a [`GroupId`] and not a bare integer.

use plx_machine::machine::{GroupId, Measure};
use plx_media::player::text_style::HudBand;
use plx_ui::Rect;

use super::classic;
use super::command::HudCommand;
use super::model::HudModel;
use super::motion::HudMotion;
use super::state::{ControlSlot, Controls};
use crate::hud_format::compile::{Compiled, CompiledState, SkipKind};
use crate::hud_format::reach::Slot;

/// The scrubber's group: a free-form region, extent [`HudGeometry::grab`].
pub const GROUP_SCRUB: GroupId = GroupId(0);
/// The control row's group: the discs, the Skip pill or the Up Next pair, one stop per item.
pub const GROUP_ROW: GroupId = GroupId(1);
/// The bottom tab pills' group: Info, then Chapters when the item has any.
pub const GROUP_TABS: GroupId = GroupId(2);
/// The failure read-out's action row.
pub const GROUP_FAILURE: GroupId = GroupId(3);

/// **The most controls a layout can place in the control row**: the format's bound on one segment
/// ([`crate::hud_format::schema::MAX_SEGMENT_ITEMS`]), so a row a package can write is a row the report
/// holds and the focus group and the pointer stops can walk. Classic's is five ([`super::state::BTN_N`]);
/// the compiler reports a row wider than this as a finding rather than dropping its tail.
pub const MAX_ROW_ITEMS: usize = crate::hud_format::schema::MAX_SEGMENT_ITEMS;

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
#[derive(Clone, Copy, Debug, PartialEq)]
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
    pub items: [Option<Rect>; MAX_ROW_ITEMS],
    /// The bottom tab row's origin and height, at its narrowest (no width): the overscan audit's
    /// "player bottom tab row".
    pub tab_row: Rect,
    /// The title block's origin: the overscan audit's "player title / elapsed clock".
    pub title: Rect,
    /// Tab `i`'s pill, `None` for a tab the row does not carry.
    pub tabs: [Option<Rect>; HUD_TABS],
    /// **The caption band this layout occupies** (`docs/hud-plugins.md`, "Captions: the band a HUD
    /// occupies"), the ONE value every caption reader takes: the same in every slot and every state, so
    /// a caption does not move with the focus or the offer. Classic's is the compact row's top
    /// ([`HudBand::from_controls_top`] of `row_anchor.top`, which is [`HudBand::CLASSIC`]); a compiled
    /// package's is the top of its highest drawn extent (`hud_format::compile`).
    pub band: HudBand,
}

impl HudGeometry {
    /// **The caption band this layout occupies**: the [`HudGeometry::band`] field, whichever slot
    /// occupies the row. A value from the layout, so the caption draws and the libass lift take it
    /// rather than a constant.
    pub fn band(&self) -> HudBand {
        self.band
    }

    /// **Where along the bar a pointer at `x` is pointing**, `0..1`. X only, on purpose: an engaged
    /// drag tracks the pointer even when it wanders off the band vertically, which is why
    /// `PlayerScreen::handle_drag` never re-tests the hit once a click has seated the gesture.
    pub fn scrub_frac(&self, x: f32) -> f32 {
        ((x - self.playbar.x) / self.playbar.w).clamp(0.0, 1.0)
    }
}

/// **The compiled state the transport is in this frame**, with the Skip pill's kind: the compile's
/// [`Compiled::lookup`] over the frame's slot and content (`open == 0`; the sections draw themselves in
/// this release). The state returned is the BASE state; [`CompiledState::skip_variant`] picks the credits
/// layout of a Skip state, and [`CompiledState::geometry`] does so itself from the kind.
///
/// `episode` is the ROW's pair boolean (`Controls::pair`), not the kicker's predicate: PR5c's golden
/// equates `Discs(PAIR)` with `episode = true`. The renderer and every geometry consumer take their state
/// from here, so what is drawn and what is hit cannot be two states.
pub fn frame_state(compiled: &Compiled, slot: ControlSlot, chapters: bool, paused: bool) -> Option<(&CompiledState, SkipKind)> {
    let (kind, skip) = match slot {
        ControlSlot::Discs(_) => (Slot::Discs, SkipKind::Intro),
        ControlSlot::UpNext(_) => (Slot::UpNext, SkipKind::Intro),
        ControlSlot::Skip(pr) => (
            Slot::Skip,
            if pr.kind == plx_data::metadata::MarkerKind::Credits { SkipKind::Credits } else { SkipKind::Intro },
        ),
    };
    let episode = matches!(slot, ControlSlot::Discs(c) if c.pair());
    compiled.lookup(0, kind, chapters, episode, paused).map(|s| (s, skip))
}

/// **The geometry facts that do not depend on what occupies the row**, for the frame: the grab band, the
/// bar, the row at its floor width and its anchor, the title and tab-row origins, the caption band. From
/// the compiled package's state for the frame when it has one, else classic's. It measures nothing, so it
/// is safe where there is no measurer (a pointer event, the caption draw).
pub fn frame_geometry(compiled: Option<&Compiled>, slot: ControlSlot, chapters: bool, paused: bool) -> HudGeometry {
    match compiled.and_then(|c| frame_state(c, slot, chapters, paused)) {
        Some((state, skip)) => state.geometry(skip),
        None => classic::frame_geometry(),
    }
}

/// **The whole geometry report for one frame**: [`frame_geometry`] plus item `i` of whatever occupies the
/// row at the rect it is DRAWN at and each tab's pill. The compiled package's report copies what the
/// compile placed; classic's measures (the Skip pill, the Up Next pair and the pills are as wide as their
/// labels), like the draw it describes.
pub fn geometry(
    compiled: Option<&Compiled>,
    slot: ControlSlot,
    motion: &HudMotion,
    chapters: bool,
    paused: bool,
    measure: &dyn Measure,
) -> HudGeometry {
    match compiled.and_then(|c| frame_state(c, slot, chapters, paused)) {
        Some((state, skip)) => state.geometry(skip),
        None => classic::geometry(slot, motion, if chapters { 2 } else { 1 }, measure),
    }
}

/// **How many stops the row group holds** for the frame: the replaced row's declared controls for a
/// package's discs (a row wider than classic's five has a stop for each), `[credits, next]` for Up Next,
/// `[pill]` for Skip, and [`ControlSlot::items`] where the package has no state for the frame.
pub fn row_len(compiled: Option<&Compiled>, slot: ControlSlot, chapters: bool, paused: bool) -> usize {
    match (slot, compiled.and_then(|c| frame_state(c, slot, chapters, paused))) {
        (ControlSlot::Discs(_), Some((state, _))) => state.row_ids.len(),
        _ => slot.items().max(0) as usize,
    }
}

/// The compiled state of the discs for the frame and the `Controls` the slot sampled, when the occupant is
/// the discs and the package has a state for the frame. `None` is "ask the slot" (classic's table).
fn package_row<'c>(compiled: Option<&'c Compiled>, slot: ControlSlot, model: &HudModel) -> Option<(&'c CompiledState, Controls)> {
    let ControlSlot::Discs(controls) = slot else { return None };
    let (state, _) = frame_state(compiled?, slot, model.chapters, model.paused)?;
    Some((state, controls))
}

/// **What a press of the row's item `i` performs**, `None` for an absent or disabled one (it has no stop).
/// For the discs of a package it is the command of the control the package placed at that stable index
/// (`CompiledState::row_ids`), with its parameters, enabled by the command's own predicate
/// ([`HudCommand::enabled`]) over the frame's `model` (with the slot's sampled `Controls`, so the discs
/// and the model cannot disagree). Everywhere else it is the slot's: classic's table, a stand-in's none.
/// For `builtin:classic` the two are one answer (the test next to this fn pins it).
pub fn row_command(compiled: Option<&Compiled>, slot: ControlSlot, model: &HudModel, i: i32) -> Option<HudCommand> {
    match package_row(compiled, slot, model) {
        Some((state, controls)) => {
            let id = state.row_ids.get(usize::try_from(i).ok()?)?.as_deref()?;
            let act = state.control(id)?.act?;
            act.enabled(&HudModel { controls, ..*model }).then_some(act)
        }
        None => slot.command(i),
    }
}

/// Does item `i` of the row have a focus stop and a pointer target? [`row_command`] for the discs of a
/// package, [`ControlSlot::enabled`] elsewhere.
pub fn row_enabled(compiled: Option<&Compiled>, slot: ControlSlot, model: &HudModel, i: i32) -> bool {
    match package_row(compiled, slot, model) {
        Some(_) => row_command(compiled, slot, model, i).is_some(),
        None => slot.enabled(i),
    }
}

/// **Where the cursor parks when the row appears.** Classic's rule over commands: an enabled Next, else an
/// enabled Previous, else an enabled Play/Pause, else the first enabled control (classic has no Play/Pause
/// control, so for it that is `Controls::primary`: Next, Previous, then Subtitles).
pub fn row_primary(compiled: Option<&Compiled>, slot: ControlSlot, model: &HudModel) -> i32 {
    let Some((state, _)) = package_row(compiled, slot, model) else { return slot.primary_btn() };
    let len = i32::try_from(state.row_ids.len()).unwrap_or(0);
    let at = |want: Option<HudCommand>| (0..len).find(|&i| row_command(compiled, slot, model, i).is_some_and(|c| want.is_none_or(|w| c == w)));
    at(Some(HudCommand::Next))
        .or_else(|| at(Some(HudCommand::Previous)))
        .or_else(|| at(Some(HudCommand::TogglePlay)))
        .or_else(|| at(None))
        .unwrap_or_else(|| slot.primary_btn())
}
