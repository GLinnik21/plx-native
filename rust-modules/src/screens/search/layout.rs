//! Search's document geometry. Both engine placement and rendering use these expressions.
use crate::search::Kind;
use crate::ui::card_row::{self, RowStyle};
use crate::ui::consts::{CARD_H, CARD_W, MARGIN_X, MARGIN_Y, SCR_H, SCR_W};
use crate::ui::machine::GroupId;
use crate::ui::Rect;

pub(super) const FIELD: Rect = Rect { x: MARGIN_X, y: 138.0, w: SCR_W - 2.0 * MARGIN_X, h: 80.0 };
pub(super) const SCOPE_Y: f32 = FIELD.y + FIELD.h + 12.0;
pub(super) const CONTENT_TOP: f32 = 300.0;
pub(super) const HEAD_TO_ROW: f32 = 60.0;
pub(super) const KEYBOARD_H: f32 = 324.0;

pub(super) fn ordinal(kind: Kind) -> u32 {
    match kind { Kind::Movie => 0, Kind::Show => 1, Kind::Episode => 2, Kind::Person => 3, Kind::Collection => 4 }
}
pub(super) fn group(kind: Kind) -> GroupId { GroupId(0x5345_4200 + ordinal(kind)) }
pub(super) fn style(kind: Kind) -> RowStyle {
    let (w, h, circular) = match kind {
        Kind::Episode => (420.0, 236.0, false), Kind::Person => (250.0, 250.0, true),
        _ => (CARD_W, CARD_H, false),
    };
    RowStyle { w, h, circular, ..RowStyle::HOME }
}
pub(super) fn block_h(kind: Kind, expansion: f32) -> f32 {
    HEAD_TO_ROW + style(kind).h + crate::ui::consts::UNDER_LABEL_AIR + card_row::under_band(expansion)
}
pub(super) fn top(kinds: &[Kind], index: usize, expansion: impl Fn(usize) -> f32) -> f32 {
    CONTENT_TOP + kinds.iter().take(index).enumerate().map(|(i, kind)| block_h(*kind, expansion(i))).sum::<f32>()
}
pub(super) fn reveal(scroll: f32, kinds: &[Kind], focused: usize) -> f32 {
    if focused >= kinds.len() { return 0.0; }
    let expansion = |i| if i == focused { 1.0 } else { 0.0 };
    let origin = top(kinds, focused, expansion);
    let height = block_h(kinds[focused], 1.0);
    let content = top(kinds, kinds.len(), expansion) + MARGIN_Y;
    card_row::reveal(scroll, origin + height - (SCR_H - MARGIN_Y), origin - CONTENT_TOP, (content - SCR_H).max(0.0))
}
pub(super) fn recent(slot: usize, scroll: f32) -> Rect {
    Rect::new(MARGIN_X, CONTENT_TOP + crate::ui::table::HDR_H + slot as f32 * crate::ui::table::ROW_H - scroll,
        820.0, crate::ui::table::ROW_H)
}
pub(super) fn clear(terms: usize, scroll: f32, measure: &dyn crate::ui::machine::Measure) -> Rect {
    let last = recent(terms, scroll);
    let width = crate::ui::widgets::Button::pill_w_measured(c"Clear recent searches",
        crate::ui::theme::size::BODY, false, false, measure);
    Rect::new(MARGIN_X + crate::ui::table::CONTENT_X, last.y + crate::ui::theme::space::MD, width, 60.0)
}
