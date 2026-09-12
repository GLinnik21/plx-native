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
/// Heading cap top to card top — the SHARED shelf pitch, stated as the sum rather than as the 60
/// it has always been, so the three detail shelves move together (see [`super::related::LABEL_H`]).
pub(crate) const LABEL_H: f32 = crate::ui::consts::TITLE_DY + crate::ui::consts::CARD_DY;
const SLOT: f32 = 230.0;
/// How far a FOCUSED headshot grows past the row box. It is not part of the label band — it is the
/// focus pop of a 190-tall circle, half of which falls below the row — so it rides the same
/// expansion the band does rather than being reserved on every frame.
pub(crate) const FOCUS_POP: f32 = RowStyle::CAST.h * (RowStyle::CAST.focus_scale - 1.0) * 0.5;

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

/// The cast row's under-band is FIXED, and it is the one detail shelf that may not take the shared
/// collapse: it draws a name and a role under EVERY headshot, focused or not, so the room is
/// occupied on every frame. Related and Extras draw only the focused tile's label, which is what
/// lets them give it back ([`super::related::block_h`]). Measured on the panel first: collapsed,
/// the cast names printed straight through the Extras heading.
pub(crate) fn block_h() -> f32 {
    LABEL_H + RowStyle::CAST.h + card_row::UNDER_LABEL_H + FOCUS_POP
}

pub(crate) fn draw(
    p: Painter,
    d: &Detail,
    row: &CardRow,
    top: f32,
    focused: Option<usize>,
    measure: &dyn crate::ui::machine::Measure,
) {
    // tc: t() answers a &str; the draw API eats NUL-terminated pointers (i18n's bridge)
    let mut cast_head = [0u8; crate::i18n::TC_MAX];
    let cast_head = crate::i18n::tc("Cast & Crew", &mut cast_head);
    p.text(
        cast_head.as_ptr().cast(),
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

    /// Cast is the one detail shelf whose band may NOT collapse, so its block reserves the label
    /// room on every frame — plus the focus pop, which falls below the row box and is the part a
    /// fixed `UNDER_H` used to swallow. Measured on the panel first: with the band collapsed, the
    /// always-drawn names printed through the next section's heading.
    #[test]
    fn the_cast_block_covers_its_always_drawn_labels_on_every_frame() {
        let under = block_h() - LABEL_H - RowStyle::CAST.h;
        assert!(under >= card_row::UNDER_LABEL_H + pop_drop(RowStyle::CAST.focus_scale));
        assert!(under > card_row::LABEL_BAND_COLLAPSED);
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
