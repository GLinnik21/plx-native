//! Related-shelf geometry and actions for [`super::DetailScreen`].

use plx_data::metadata::Detail;
use plx_data::pms::PmsMovie;
use crate::registry::tile_facts;
use plx_ui::card_row::{self, CardRow, RowStyle};
use plx_machine::machine::GroupId;
use plx_ui::widgets::Art;
use plx_ui::{theme, Painter, Rect};

pub const RELATED_ELEM_RANGE_START: u32 = 640;
pub const RELATED_ELEM_RANGE_END: u32 = 1152;
pub const RELATED_GROUP: GroupId = GroupId(3);
/// Heading cap top to card top — the SHARED shelf pitch (`consts::TITLE_DY + CARD_DY`), the same
/// 60 a Home or Library shelf puts between its heading and its posters. It was a local 46, so the
/// one object this page shares with every browsing screen sat 14px tighter here than anywhere else.
pub const LABEL_H: f32 = plx_ui::consts::TITLE_DY + plx_ui::consts::CARD_DY;

pub fn elem(index: usize) -> Option<u32> {
    (index < (RELATED_ELEM_RANGE_END - RELATED_ELEM_RANGE_START) as usize)
        .then_some(RELATED_ELEM_RANGE_START + index as u32)
}

pub fn locate(key: u32) -> Option<usize> {
    (RELATED_ELEM_RANGE_START..RELATED_ELEM_RANGE_END)
        .contains(&key)
        .then(|| (key - RELATED_ELEM_RANGE_START) as usize)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    None,
    OpenDetail(plx_plex::plex::ServerId, String),
}

pub fn action(d: &Detail, key: u32) -> Action {
    let Some(m) = locate(key).and_then(|i| d.related.get(i)) else {
        return Action::None;
    };
    if m.rk.is_empty() {
        return Action::None;
    }
    Action::OpenDetail(m.sid, m.rk.clone())
}

pub fn item<'a>(d: &'a Detail, key: u32) -> Option<&'a PmsMovie> {
    d.related.get(locate(key)?)
}

pub fn rect(row: &CardRow, index: usize, top: f32, at_drawn: bool) -> Rect {
    let base = card_row::tile_rect(
        index,
        plx_ui::consts::MARGIN_X,
        RowStyle::HOME.w + RowStyle::HOME.gap,
        row.scroll_x(),
        top + LABEL_H,
        (RowStyle::HOME.w, RowStyle::HOME.h),
    );
    if at_drawn {
        base.scaled(row.scale(index))
    } else {
        base.scaled(RowStyle::HOME.focus_scale)
    }
}

/// `band` is this shelf's live label-band expansion ([`CardRow::band_expand`]), 0 collapsed → 1
/// focused. The band is the SHARED collapse every other screen uses, not a fixed reservation: a
/// shelf that holds no focus draws no label, so it gives the room back and the next section's
/// heading rises to the design system's own region gap behind it.
pub fn block_h(band: f32) -> f32 {
    LABEL_H + RowStyle::HOME.h + card_row::under_band(band)
}

/// The shelf's heading, `lift` being the row's live label lift (`Shelf::heading_lift`).
pub fn draw_heading(p: Painter, top: f32, lift: f32) {
    p.text(
        plx_platform::i18n::msg::browse_detail_related_c().as_ptr(),
        plx_ui::consts::MARGIN_X,
        top - lift,
        theme::size::HEADLINE,
        theme::TEXT_HEADING,
        0,
        1,
    );
}

/// A Detail poster shelf's cards under its heading — Related's, and the collection shelf's
/// (`super::collection`), which is the same strip under a linked heading.
pub fn draw_strip(
    p: Painter,
    items: &[PmsMovie],
    row: &CardRow,
    top: f32,
    focused: Option<usize>,
    press: f32,
    measure: &dyn plx_machine::machine::Measure,
) {
    card_row::strip(
        p,
        row,
        items.len(),
        focused.map(|i| i as i32).unwrap_or(-1),
        top + LABEL_H,
        (RowStyle::HOME.w, RowStyle::HOME.h),
        RowStyle::HOME.w + RowStyle::HOME.gap,
        &RowStyle::HOME,
        plx_ui::consts::SCR_W,
        press,
        |i| Art::Poster(items.get(i).map(tile_facts::of)),
        |i| items.get(i).and_then(|m| m.resume_frac()),
        |i| card_row::TileLabel::title(&items[i].title),
        |_, _, _, _| {},
        measure,
    );
}

/// The focused card of a strip drawn by [`draw_strip`], last so its glow sits over its neighbours.
pub fn draw_focused_in(
    p: Painter,
    items: &[PmsMovie],
    row: &CardRow,
    index: usize,
    top: f32,
    press: f32,
    measure: &dyn plx_machine::machine::Measure,
) {
    let Some(item) = items.get(index) else {
        return;
    };
    let base = card_row::tile_rect(
        index,
        plx_ui::consts::MARGIN_X,
        RowStyle::HOME.w + RowStyle::HOME.gap,
        row.scroll_x(),
        top + LABEL_H,
        (RowStyle::HOME.w, RowStyle::HOME.h),
    );
    let scale = row.scale(index) * press;
    card_row::draw_focused(
        p,
        Art::Poster(Some(tile_facts::of(item))),
        base.scaled(scale),
        scale,
        &RowStyle::HOME,
        item.resume_frac(),
        &card_row::TileLabel::title(&item.title)
            .settling(row.settle_lag(items.len(), index, &RowStyle::HOME)),
        measure,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_related_key_round_trips() {
        for i in 0..512 {
            assert_eq!(locate(elem(i).unwrap()), Some(i));
        }
    }
}
