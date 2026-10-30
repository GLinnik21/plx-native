//! **`HudGeometry`: what the layout reports back.** The facts that FOCUS, the POINTER, the overscan
//! audit and the control-row STAND-INS (Skip, Up Next) need to know about where the HUD is, as values
//! rather than as the classic layout's constants.
//!
//! **The compiled package fills it** (`CompiledState::geometry`): [`geometry`] for the whole of it, [`row_len`]
//! for how many stops the row group holds, [`row_command`] / [`row_enabled`] / [`row_primary`] for what a
//! press performs. These are the ONE place a consumer asks where the HUD is. A consumer reads the report
//! and so is indifferent to which package produced it: the player's `Focusable` (group extents and row
//! length), the pointer stops, the caption band and the stand-ins never name `CTRL_Y`, `CTRL_H`,
//! `CTRL_RIGHT` themselves.
//!
//! **A package with no state for the frame** (`frame_state` answers `None`: a user package whose
//! valuation the compile did not reach; `builtin:classic` has all of them, which a test pins) is decided
//! HERE, once, for the draw and the geometry together: the report is empty (every rect zero, no stop in the
//! row, the package's own caption band), and `render::draw` draws nothing and says so. The user-package
//! fallback to classic is PR10b's.
//!
//! This module also names the HUD's focus GROUPS. They are the shared `FocusEngine` groups the
//! player's `Focusable` declares, and the one set of numbers the HUD's motion and drawing compare a
//! focus against, so the screen's `nav.focus` is a [`GroupId`] and not a bare integer.

use plx_machine::machine::GroupId;
use plx_media::player::text_style::HudBand;
use plx_ui::Rect;

use super::command::HudCommand;
use super::model::HudModel;
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

/// The rects a layout reports for one frame: [`CompiledState::geometry`] (all of it), or [`HudGeometry::empty`]
/// for a package with no state for the frame.
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
    /// **The report of a frame the package has no state for**: every rect zero (nothing to grab, press or
    /// place), no item and no tab, and the package's own caption band, so a caption still stands clear of
    /// whatever the package does draw.
    pub fn empty(band: HudBand) -> Self {
        Self {
            grab: Rect::default(),
            playbar: Rect::default(),
            row: Rect::default(),
            row_anchor: RowAnchor::default(),
            items: [None; MAX_ROW_ITEMS],
            tab_row: Rect::default(),
            title: Rect::default(),
            tabs: [None; HUD_TABS],
            band,
        }
    }

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

/// **The whole geometry report for one frame**: the grab band, the bar, the row at its floor width and its
/// anchor, item `i` of whatever occupies the row at the rect it is DRAWN at, each tab's pill, the title and
/// tab-row origins and the caption band. It copies what the compile placed (the compile measured, once,
/// outside the draw), so it needs no measurer and is safe anywhere: a pointer event, the caption draw, the
/// stand-ins. [`HudGeometry::empty`] where the package has no state for the frame.
pub fn geometry(compiled: &Compiled, slot: ControlSlot, chapters: bool, paused: bool) -> HudGeometry {
    match frame_state(compiled, slot, chapters, paused) {
        Some((state, skip)) => state.geometry(skip),
        None => HudGeometry::empty(compiled.band()),
    }
}

/// **How many stops the row group holds** for the frame: the replaced row's declared controls for a
/// package's discs (a row wider than classic's five has a stop for each), `[credits, next]` for Up Next,
/// `[pill]` for Skip, and none where the package has no state for the frame (a stop with no rect cannot be
/// placed).
pub fn row_len(compiled: &Compiled, slot: ControlSlot, chapters: bool, paused: bool) -> usize {
    match (slot, frame_state(compiled, slot, chapters, paused)) {
        (_, None) => 0,
        (ControlSlot::Discs(_), Some((state, _))) => state.row_ids.len(),
        _ => slot.items().max(0) as usize,
    }
}

/// The compiled state of the discs for the frame and the `Controls` the slot sampled, when the occupant is
/// the discs and the package has a state for the frame.
fn package_row<'c>(compiled: &'c Compiled, slot: ControlSlot, model: &HudModel) -> Option<(&'c CompiledState, Controls)> {
    let ControlSlot::Discs(controls) = slot else { return None };
    let (state, _) = frame_state(compiled, slot, model.chapters, model.paused)?;
    Some((state, controls))
}

/// **What a press of the row's item `i` performs**, `None` for an absent or disabled one (it has no stop).
/// For the discs it is the command of the control the package placed at that stable index
/// (`CompiledState::row_ids`), with its parameters, enabled by the command's own predicate
/// ([`HudCommand::enabled`]) over the frame's `model` (with the slot's sampled `Controls`, so the discs
/// and the model cannot disagree); none where the package has no state for the frame. A stand-in's row
/// (Skip, Up Next) is the slot's own table. For `builtin:classic` the discs' answer is classic's table (the
/// test next to this fn pins it).
pub fn row_command(compiled: &Compiled, slot: ControlSlot, model: &HudModel, i: i32) -> Option<HudCommand> {
    match slot {
        ControlSlot::Discs(controls) => {
            let (state, _) = frame_state(compiled, slot, model.chapters, model.paused)?;
            let id = state.row_ids.get(usize::try_from(i).ok()?)?.as_deref()?;
            let act = state.control(id)?.act?;
            act.enabled(&HudModel { controls, ..*model }).then_some(act)
        }
        _ => slot.command(i),
    }
}

/// Does item `i` of the row have a focus stop and a pointer target? [`row_command`] for the discs,
/// [`ControlSlot::enabled`] for a stand-in.
pub fn row_enabled(compiled: &Compiled, slot: ControlSlot, model: &HudModel, i: i32) -> bool {
    match slot {
        ControlSlot::Discs(_) => row_command(compiled, slot, model, i).is_some(),
        _ => slot.enabled(i),
    }
}

/// **Where the cursor parks when the row appears.** Classic's rule over commands: an enabled Next, else an
/// enabled Previous, else an enabled Play/Pause, else the first enabled control (classic has no Play/Pause
/// control, so for it that is `Controls::primary`: Next, Previous, then Subtitles). A stand-in's is the
/// slot's own.
pub fn row_primary(compiled: &Compiled, slot: ControlSlot, model: &HudModel) -> i32 {
    let Some((state, _)) = package_row(compiled, slot, model) else { return slot.primary_btn() };
    let len = i32::try_from(state.row_ids.len()).unwrap_or(0);
    let at = |want: Option<HudCommand>| (0..len).find(|&i| row_command(compiled, slot, model, i).is_some_and(|c| want.is_none_or(|w| c == w)));
    at(Some(HudCommand::Next))
        .or_else(|| at(Some(HudCommand::Previous)))
        .or_else(|| at(Some(HudCommand::TogglePlay)))
        .or_else(|| at(None))
        .unwrap_or_else(|| slot.primary_btn())
}
