//! Extras shelf: captioned preview tiles for trailers, behind-the-scenes, and the rest.
//!
//! OK plays a playable extra only. A missing thumb draws the card placeholder. The hero Trailer
//! disc is a separate control and stays until a preview path replaces it.

use plx_data::metadata::{extra_play_context, Detail, Extra};
use crate::registry::PlayIntent;
use plx_ui::cards::{self as ui_cards, RowStyle, TileLabel};
use plx_machine::machine::GroupId;
use plx_ui::widgets::Art;
use plx_ui::{theme, Painter};

pub const EXTRAS_ELEM_RANGE_START: u32 = 1728;
/// Stops before published detail keys (`FIRST_ITEM_ELEM` 2048). 32 tiles is the shelf cap.
pub const EXTRAS_ELEM_RANGE_END: u32 = 1760;
pub const EXTRAS_GROUP: GroupId = GroupId(6);
/// Heading cap top to card top — the SHARED shelf pitch, as on [`super::related`].
pub const LABEL_H: f32 = plx_ui::consts::TITLE_DY + plx_ui::consts::CARD_DY;
const MAX: usize = (EXTRAS_ELEM_RANGE_END - EXTRAS_ELEM_RANGE_START) as usize;
const STYLE: RowStyle = RowStyle::EPISODE;

pub fn elem(index: usize) -> Option<u32> {
    (index < MAX).then_some(EXTRAS_ELEM_RANGE_START + index as u32)
}

pub fn locate(key: u32) -> Option<usize> {
    (EXTRAS_ELEM_RANGE_START..EXTRAS_ELEM_RANGE_END)
        .contains(&key)
        .then(|| (key - EXTRAS_ELEM_RANGE_START) as usize)
}

pub fn len(d: &Detail) -> usize {
    d.extras.len().min(MAX)
}

/// `band` is this shelf's live label-band expansion — see [`super::related::block_h`]. The old
/// fixed `TileLabel::height(true)` is exactly `card_row::under_band(1.0)`, so a FOCUSED extras
/// shelf is unchanged and only the unfocused one gives its room back.
pub fn block_h(band: f32) -> f32 {
    LABEL_H + STYLE.h + ui_cards::under_band(band)
}

/// Play fields for one extra. `None` when the tile is missing or has no playable file.
pub fn play(d: &Detail, key: u32) -> Option<PlayIntent> {
    let extra = locate(key).and_then(|i| d.extras.get(i)).filter(|e| e.playable())?;
    Some(PlayIntent::Item {
        sid: plx_media::route::item_sid(d.sid),
        rk: extra.rk.clone(),
        part: extra.part.clone(),
        vcodec: extra.vcodec.clone(),
        acodec: extra.acodec.clone(),
        title: extra.hud_title(d.title.as_str()).to_string(),
        context: extra_play_context(extra).into(),
    })
}

fn thumb<'a>(d: &'a Detail, extra: &'a Extra) -> Art<'a> {
    Art::Thumb {
        sid: d.sid.raw(),
        key: extra.thumb.as_str(),
        res: (STYLE.w as i32, STYLE.h as i32),
    }
}

/// Card `i`'s thumbnail; a missing extra draws the card placeholder.
pub fn art(d: &Detail, i: usize) -> Art<'_> {
    d.extras.get(i).map(|e| thumb(d, e)).unwrap_or(Art::Thumb {
        sid: d.sid.raw(),
        key: "",
        res: (STYLE.w as i32, STYLE.h as i32),
    })
}

/// Card `i`'s label block: its title (its caption when it has none) over its caption.
pub fn label(d: &Detail, i: usize) -> TileLabel {
    let e = &d.extras[i];
    let title = if e.title.is_empty() {
        e.caption()
    } else {
        e.title.as_str()
    };
    TileLabel::titled(title, e.caption())
}

/// The shelf's heading, `lift` being the row's live label lift (`Shelf::heading_lift`).
pub fn draw_heading(p: Painter, top: f32, lift: f32) {
    p.text(
        plx_platform::i18n::msg::browse_detail_extras_c().as_ptr(),
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
    fn the_extras_range_abuts_about_and_stays_below_published_keys() {
        assert_eq!(super::super::about::ABOUT_ELEM_RANGE_END, EXTRAS_ELEM_RANGE_START);
        assert!(EXTRAS_ELEM_RANGE_END <= 2048);
        assert!(EXTRAS_ELEM_RANGE_START < EXTRAS_ELEM_RANGE_END);
    }

    #[test]
    fn every_extras_key_round_trips() {
        for i in 0..MAX {
            assert_eq!(locate(elem(i).unwrap()), Some(i));
        }
        assert!(elem(MAX).is_none());
    }

    #[test]
    fn ok_plays_a_playable_extra_and_refuses_one_without_a_file() {
        let playable = Extra {
            rk: "9".into(),
            part: "/p".into(),
            title: "Featurette".into(),
            subtype: "behindTheScenes".into(),
            extra_type: 5,
            ..Default::default()
        };
        let dead = Extra {
            rk: "8".into(),
            title: "No file".into(),
            subtype: "trailer".into(),
            extra_type: 1,
            ..Default::default()
        };
        let d = Detail {
            title: "Movie".into(),
            extras: vec![playable, dead],
            ..Default::default()
        };
        let intent = play(&d, elem(0).unwrap()).expect("playable extra");
        match intent {
            PlayIntent::Item { rk, part, context, .. } => {
                assert_eq!(rk, "9");
                assert_eq!(part, "/p");
                assert_eq!(context, plx_data::metadata::EXTRA_CONTEXT);
            }
            PlayIntent::Movie(_) => panic!("an extra is not the parent movie"),
        }
        assert!(play(&d, elem(1).unwrap()).is_none());
    }
}
