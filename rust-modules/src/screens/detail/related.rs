//! Related-shelf geometry and actions for [`super::DetailScreen`].

use crate::metadata::Detail;
use crate::pms::PmsMovie;
use crate::ui::card_row::{self, CardRow, RowStyle};
use crate::ui::machine::GroupId;
use crate::ui::widgets::Art;
use crate::ui::{theme, Painter, Rect};

pub(crate) const RELATED_ELEM_RANGE_START: u32 = 640;
pub(crate) const RELATED_ELEM_RANGE_END: u32 = 1152;
pub(crate) const RELATED_GROUP: GroupId = GroupId(3);
pub(crate) const LABEL_H: f32 = 46.0;
pub(crate) const UNDER_H: f32 = 54.0;

pub(crate) fn elem(index: usize) -> Option<u32> {
    (index < (RELATED_ELEM_RANGE_END - RELATED_ELEM_RANGE_START) as usize)
        .then_some(RELATED_ELEM_RANGE_START + index as u32)
}

pub(crate) fn locate(key: u32) -> Option<usize> {
    (RELATED_ELEM_RANGE_START..RELATED_ELEM_RANGE_END)
        .contains(&key)
        .then(|| (key - RELATED_ELEM_RANGE_START) as usize)
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    None,
    OpenDetail(crate::plex::ServerId, String),
}

pub(crate) fn action(d: &Detail, key: u32) -> Action {
    let Some(m) = locate(key).and_then(|i| d.related.get(i)) else {
        return Action::None;
    };
    if m.rk.is_empty() {
        return Action::None;
    }
    Action::OpenDetail(m.sid, m.rk.clone())
}

pub(crate) fn item<'a>(d: &'a Detail, key: u32) -> Option<&'a PmsMovie> {
    d.related.get(locate(key)?)
}

pub(crate) fn rect(row: &CardRow, index: usize, top: f32, at_drawn: bool) -> Rect {
    let base = card_row::tile_rect(
        index,
        crate::ui::consts::MARGIN_X,
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

pub(crate) fn block_h() -> f32 {
    LABEL_H + RowStyle::HOME.h + UNDER_H
}

pub(crate) fn draw(p: Painter, d: &Detail, row: &CardRow, top: f32, focused: Option<usize>) {
    let lift = row.lift();
    p.text(
        c"Related".as_ptr(),
        crate::ui::consts::MARGIN_X,
        top - lift,
        theme::size::HEADLINE,
        theme::TEXT_HEADING,
        0,
        1,
    );
    card_row::strip(
        p,
        row,
        d.related.len(),
        focused.map(|i| i as i32).unwrap_or(-1),
        top + LABEL_H,
        (RowStyle::HOME.w, RowStyle::HOME.h),
        RowStyle::HOME.w + RowStyle::HOME.gap,
        &RowStyle::HOME,
        crate::ui::consts::SCR_W,
        |i| Art::Poster(d.related.get(i)),
        |i| d.related.get(i).and_then(|m| m.resume_frac()),
        |i| card_row::TileLabel::title(&d.related[i].title),
        |_, _, _, _| {},
    );
}

pub(crate) fn draw_focused(
    p: Painter,
    d: &Detail,
    row: &CardRow,
    index: usize,
    top: f32,
    press: f32,
) {
    let Some(item) = d.related.get(index) else {
        return;
    };
    let base = card_row::tile_rect(
        index,
        crate::ui::consts::MARGIN_X,
        RowStyle::HOME.w + RowStyle::HOME.gap,
        row.scroll_x(),
        top + LABEL_H,
        (RowStyle::HOME.w, RowStyle::HOME.h),
    );
    let scale = row.scale(index) * press;
    card_row::draw_focused(
        p,
        Art::Poster(Some(item)),
        base.scaled(scale),
        scale,
        &RowStyle::HOME,
        item.resume_frac(),
        &card_row::TileLabel::title(&item.title),
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

    #[test]
    fn the_related_menus_anchor_is_the_tile_the_shelf_drew() {
        let mut row = CardRow::new();
        for _ in 0..120 {
            row.update(12, Some(11), &RowStyle::HOME, 1.0 / 60.0);
        }
        for i in 0..12 {
            let expected = card_row::tile_rect(
                i,
                crate::ui::consts::MARGIN_X,
                RowStyle::HOME.w + RowStyle::HOME.gap,
                row.scroll_x(),
                200.0 + LABEL_H,
                (RowStyle::HOME.w, RowStyle::HOME.h),
            )
            .scaled(row.scale(i));
            let actual = rect(&row, i, 200.0, true);
            assert_eq!(
                (actual.x, actual.y, actual.w, actual.h),
                (expected.x, expected.y, expected.w, expected.h)
            );
        }
    }
}
