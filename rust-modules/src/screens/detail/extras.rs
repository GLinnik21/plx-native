//! Extras shelf: captioned preview tiles for trailers, behind-the-scenes, and the rest.
//!
//! OK plays a playable extra only. A missing thumb draws the card placeholder. The hero Trailer
//! disc is a separate control and stays until a preview path replaces it.

use crate::metadata::{extra_play_context, Detail, Extra};
use crate::screens::registry::PlayIntent;
use crate::ui::card_row::{self, CardRow, RowStyle, TileLabel};
use crate::ui::machine::GroupId;
use crate::ui::widgets::Art;
use crate::ui::{theme, Painter, Rect};

pub(crate) const EXTRAS_ELEM_RANGE_START: u32 = 1728;
/// Stops before published detail keys (`FIRST_ITEM_ELEM` 2048). 32 tiles is the shelf cap.
pub(crate) const EXTRAS_ELEM_RANGE_END: u32 = 1760;
pub(crate) const EXTRAS_GROUP: GroupId = GroupId(6);
/// Heading cap top to card top — the SHARED shelf pitch, as on [`super::related`].
pub(crate) const LABEL_H: f32 = crate::ui::consts::TITLE_DY + crate::ui::consts::CARD_DY;
const MAX: usize = (EXTRAS_ELEM_RANGE_END - EXTRAS_ELEM_RANGE_START) as usize;
const STYLE: RowStyle = RowStyle::EPISODE;

pub(crate) fn elem(index: usize) -> Option<u32> {
    (index < MAX).then_some(EXTRAS_ELEM_RANGE_START + index as u32)
}

pub(crate) fn locate(key: u32) -> Option<usize> {
    (EXTRAS_ELEM_RANGE_START..EXTRAS_ELEM_RANGE_END)
        .contains(&key)
        .then(|| (key - EXTRAS_ELEM_RANGE_START) as usize)
}

pub(crate) fn len(d: &Detail) -> usize {
    d.extras.len().min(MAX)
}

/// `band` is this shelf's live label-band expansion — see [`super::related::block_h`]. The old
/// fixed `TileLabel::height(true)` is exactly `card_row::under_band(1.0)`, so a FOCUSED extras
/// shelf is unchanged and only the unfocused one gives its room back.
pub(crate) fn block_h(band: f32) -> f32 {
    LABEL_H + STYLE.h + card_row::under_band(band)
}

pub(crate) fn rect(row: &CardRow, index: usize, top: f32, at_drawn: bool) -> Rect {
    let base = card_row::tile_rect(
        index,
        crate::ui::consts::MARGIN_X,
        STYLE.w + STYLE.gap,
        row.scroll_x(),
        top + LABEL_H,
        (STYLE.w, STYLE.h),
    );
    if at_drawn {
        base.scaled(row.scale(index))
    } else {
        base.scaled(STYLE.focus_scale)
    }
}

/// Play fields for one extra. `None` when the tile is missing or has no playable file.
pub(crate) fn play(d: &Detail, key: u32) -> Option<PlayIntent> {
    let extra = locate(key).and_then(|i| d.extras.get(i)).filter(|e| e.playable())?;
    Some(PlayIntent::Item {
        sid: crate::route::item_sid(d.sid),
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
        sid: d.sid,
        key: extra.thumb.as_str(),
        res: (STYLE.w as i32, STYLE.h as i32),
    }
}

pub(crate) fn draw(
    p: Painter,
    d: &Detail,
    row: &CardRow,
    top: f32,
    focused: Option<usize>,
    measure: &dyn crate::ui::machine::Measure,
) {
    let n = len(d);
    if n == 0 {
        return;
    }
    let lift = row.lift();
    p.text(
        c"Extras".as_ptr(),
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
        n,
        focused.map(|i| i as i32).unwrap_or(-1),
        top + LABEL_H,
        (STYLE.w, STYLE.h),
        STYLE.w + STYLE.gap,
        &STYLE,
        crate::ui::consts::SCR_W,
        |i| d.extras.get(i).map(|e| thumb(d, e)).unwrap_or(Art::Thumb {
            sid: d.sid,
            key: "",
            res: (STYLE.w as i32, STYLE.h as i32),
        }),
        |_| None,
        |i| {
            let e = &d.extras[i];
            let title = if e.title.is_empty() {
                e.caption()
            } else {
                e.title.as_str()
            };
            TileLabel::titled(title, e.caption())
        },
        |_, _, _, _| {},
        measure,
    );
}

pub(crate) fn draw_focused(
    p: Painter,
    d: &Detail,
    row: &CardRow,
    index: usize,
    top: f32,
    press: f32,
    measure: &dyn crate::ui::machine::Measure,
) {
    let Some(extra) = d.extras.get(index) else {
        return;
    };
    let base = card_row::tile_rect(
        index,
        crate::ui::consts::MARGIN_X,
        STYLE.w + STYLE.gap,
        row.scroll_x(),
        top + LABEL_H,
        (STYLE.w, STYLE.h),
    );
    let scale = row.scale(index) * press;
    let title = if extra.title.is_empty() {
        extra.caption()
    } else {
        extra.title.as_str()
    };
    card_row::draw_focused(
        p,
        thumb(d, extra),
        base.scaled(scale),
        scale,
        &STYLE,
        None,
        &TileLabel::titled(title, extra.caption()),
        measure,
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
                assert_eq!(context, crate::metadata::EXTRA_CONTEXT);
            }
            PlayIntent::Movie(_) => panic!("an extra is not the parent movie"),
        }
        assert!(play(&d, elem(1).unwrap()).is_none());
    }
}
