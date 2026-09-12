//! Cast-and-crew shelf geometry and Person navigation.

use std::ffi::CString;

use crate::metadata::Detail;
use crate::plex::ServerId;
use crate::ui::card_row::{self, CardRow, RowStyle};
use crate::ui::machine::GroupId;
use crate::ui::widgets::Art;
use crate::ui::{theme, Painter, Rect};

pub(crate) const CAST_ELEM_RANGE_START: u32 = 1152;
pub(crate) const CAST_ELEM_RANGE_END: u32 = 1664;
pub(crate) const CAST_GROUP: GroupId = GroupId(4);
pub(crate) const LABEL_H: f32 = 60.0;
const SLOT: f32 = 230.0;
const UNDER_H: f32 = 92.0 + 190.0 * (RowStyle::CAST.focus_scale - 1.0) * 0.5;

pub(crate) fn elem(index: usize) -> Option<u32> {
    (index < (CAST_ELEM_RANGE_END - CAST_ELEM_RANGE_START) as usize)
        .then_some(CAST_ELEM_RANGE_START + index as u32)
}

pub(crate) fn locate(key: u32) -> Option<usize> {
    (CAST_ELEM_RANGE_START..CAST_ELEM_RANGE_END)
        .contains(&key)
        .then(|| (key - CAST_ELEM_RANGE_START) as usize)
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    None,
    OpenPerson {
        sid: ServerId,
        key: String,
        guid: String,
        name: String,
        thumb: String,
    },
}

pub(crate) fn action(d: &Detail, key: u32) -> Action {
    let Some(c) = locate(key).and_then(|i| d.credit(i)) else {
        return Action::None;
    };
    let person_key = c.person_key();
    if person_key.is_empty() {
        return Action::None;
    }
    Action::OpenPerson {
        sid: d.sid,
        key: person_key,
        guid: c.tag_key.clone(),
        name: c.tag.clone(),
        thumb: c.thumb.clone(),
    }
}

pub(crate) fn rect(row: &CardRow, index: usize, top: f32, at_drawn: bool) -> Rect {
    let base = card_row::tile_rect(
        index,
        crate::ui::consts::MARGIN_X,
        SLOT,
        row.scroll_x(),
        top + LABEL_H,
        (RowStyle::CAST.w, RowStyle::CAST.h),
    );
    if at_drawn {
        base.scaled(row.scale(index))
    } else {
        base.scaled(RowStyle::CAST.focus_scale)
    }
}

pub(crate) fn block_h() -> f32 {
    LABEL_H + RowStyle::CAST.h + UNDER_H
}

pub(crate) fn draw(
    p: Painter,
    d: &Detail,
    row: &CardRow,
    top: f32,
    focused: Option<usize>,
    measure: &dyn crate::ui::machine::Measure,
) {
    p.text(
        c"Cast & Crew".as_ptr(),
        crate::ui::consts::MARGIN_X,
        top - row.lift(),
        theme::size::HEADLINE,
        theme::TEXT_HEADING,
        0,
        1,
    );
    let row_y = top + LABEL_H;
    card_row::strip(
        p,
        row,
        d.credits_len(),
        focused.map(|i| i as i32).unwrap_or(-1),
        row_y,
        (RowStyle::CAST.w, RowStyle::CAST.h),
        SLOT,
        &RowStyle::CAST,
        crate::ui::consts::SCR_W,
        |i| {
            d.credit(i)
                .map(|c| Art::Person {
                    sid: d.sid,
                    key: c.thumb.as_str(),
                    res: (300, 300),
                })
                .unwrap_or(Art::Person {
                    sid: d.sid,
                    key: "",
                    res: (300, 300),
                })
        },
        |_| None,
        |_| card_row::TileLabel::default(),
        |p, i, x, is_focused| {
            let Some(c) = d.credit(i) else { return };
            label(
                p,
                &c.tag,
                &c.role,
                x + RowStyle::CAST.w * 0.5,
                row_y,
                is_focused,
                if is_focused {
                    pop_drop(row.scale(i))
                } else {
                    0.0
                },
                measure,
            );
        },
        measure,
    );
}

fn pop_drop(scale: f32) -> f32 {
    (RowStyle::CAST.h * (scale - 1.0) * 0.5).max(0.0)
}

fn label(
    p: Painter,
    name: &str,
    role: &str,
    cx: f32,
    row_y: f32,
    focused: bool,
    drop: f32,
    measure: &dyn crate::ui::machine::Measure,
) {
    let budget = SLOT - 12.0;
    let name_elided = crate::text::elide_by(name, budget, false, |t| {
        measure.width_str(t, theme::size::LABEL, true)
    });
    if let Ok(name) = CString::new(name_elided) {
        p.text(
            name.as_ptr(),
            cx,
            row_y + RowStyle::CAST.h + 26.0 + drop,
            theme::size::LABEL,
            if focused {
                theme::TEXT_PRIMARY
            } else {
                theme::TEXT_SECONDARY
            },
            1,
            i32::from(focused),
        );
    }
    if role.is_empty() {
        return;
    }
    let role_elided = crate::text::elide_by(role, budget, false, |t| {
        measure.width_str(t, theme::size::CAPTION, false)
    });
    if let Ok(role) = CString::new(role_elided) {
        p.text(
            role.as_ptr(),
            cx,
            row_y + RowStyle::CAST.h + 58.0 + drop,
            theme::size::CAPTION,
            theme::TEXT_TERTIARY,
            1,
            0,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cast_pop_never_crosses_the_next_slot() {
        assert!(RowStyle::CAST.w * RowStyle::CAST.focus_scale < SLOT);
        assert!(pop_drop(0.9) >= 0.0);
    }

    #[test]
    fn the_cast_pop_is_clearly_visible_and_never_touches_a_neighbour() {
        assert!(RowStyle::CAST.focus_scale > 1.10);
        assert!(RowStyle::CAST.w * RowStyle::CAST.focus_scale < SLOT);
        assert!(SLOT - RowStyle::CAST.w * RowStyle::CAST.focus_scale > 10.0);
    }

    #[test]
    fn cast_pop_drop_tracks_the_live_scale_and_never_goes_negative() {
        assert_eq!(pop_drop(1.0), 0.0);
        assert_eq!(pop_drop(0.95), 0.0);
        assert!(pop_drop(1.05) < pop_drop(RowStyle::CAST.focus_scale));
    }

    #[test]
    fn cast_under_h_covers_the_worst_case_label_drop() {
        assert!(UNDER_H >= 92.0 + pop_drop(RowStyle::CAST.focus_scale));
    }

    #[test]
    fn ok_on_a_crew_tile_opens_that_crew_members_page_not_an_actor_at_the_same_index() {
        fn credit(name: &str, role: &str, id: i64) -> crate::metadata::Cast {
            crate::metadata::Cast {
                tag: name.into(),
                role: role.into(),
                thumb: String::new(),
                id,
                tag_key: String::new(),
            }
        }
        let detail = Detail {
            sid: ServerId::UNSET,
            cast: vec![credit("Actor", "Role", 1)],
            crew: vec![credit("Writer", "Writer", 2)],
            ..Default::default()
        };
        assert_eq!(
            action(&detail, elem(1).unwrap()),
            Action::OpenPerson {
                sid: ServerId::UNSET,
                key: "2".into(),
                guid: String::new(),
                name: "Writer".into(),
                thumb: String::new(),
            }
        );
    }
}
