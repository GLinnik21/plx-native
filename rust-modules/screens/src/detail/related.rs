//! Related-shelf geometry and actions for [`super::DetailScreen`].

use plx_data::metadata::Detail;
use plx_data::pms::PmsMovie;
use plx_ui::card_row::{self, RowStyle};
use plx_machine::machine::GroupId;
use plx_ui::{theme, Painter};

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
