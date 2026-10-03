//! Cast-and-crew shelf geometry and Person navigation.

use std::ffi::CString;

use crate::metadata::Detail;
use crate::plex::ServerId;
use crate::ui::card_row::{self, CardRow, RowStyle};
use crate::ui::label::{HAlign, Label, VAlign};
use plx_machine::machine::{GroupId, Measure};
use crate::ui::text_view::TextView;
use crate::ui::widgets::Art;
use crate::ui::{theme, Painter, Rect};

pub(crate) const CAST_ELEM_RANGE_START: u32 = 1152;
pub(crate) const CAST_ELEM_RANGE_END: u32 = 1664;
pub(crate) const CAST_GROUP: GroupId = GroupId(4);
/// Heading cap top to card top — the SHARED shelf pitch, stated as the sum rather than as the 60
/// it has always been, so the three detail shelves move together (see [`super::related::LABEL_H`]).
pub(crate) const LABEL_H: f32 = crate::ui::consts::TITLE_DY + crate::ui::consts::CARD_DY;
const SLOT: f32 = 230.0;
const NAME_GAP: f32 = theme::space::MD + theme::space::XS;
const ROLE_LEADING: f32 = theme::size::CAPTION as f32 + theme::space::XS;
const ROLE_LINES: usize = 2;
// Both caption lines and the largest focus drop are reserved by the shelf's layout owner.
const UNDER_H: f32 = NAME_GAP + theme::size::LABEL as f32 + theme::space::XS
    + ROLE_LEADING * ROLE_LINES as f32 + RowStyle::CAST.h * (RowStyle::CAST.focus_scale - 1.0) * 0.5;
/// How far a focused headshot grows past the row box. The fixed cast shelf reserves this
/// descent with its always-visible caption band, so labels cannot cross the next heading.
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
    LABEL_H + RowStyle::CAST.h + UNDER_H.max(card_row::UNDER_LABEL_H + FOCUS_POP)
}

pub(crate) fn draw(
    p: Painter,
    d: &Detail,
    row: &CardRow,
    top: f32,
    focused: Option<usize>,
    measure: &dyn plx_machine::machine::Measure,
) {
    p.text(
        plx_platform::i18n::msg::browse_detail_cast_c().as_ptr(),
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
                    sid: d.sid.raw(),
                    key: c.thumb.as_str(),
                    res: (300, 300),
                })
                .unwrap_or(Art::Person {
                    sid: d.sid.raw(),
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
                d.credit_role(i).unwrap_or_default(),
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

/// Each person's own text slot, centred under the headshot. The label is PART of its tile: it
/// scrolls past either panel edge with it, at the slot's full width, and is never narrowed or
/// re-elided against the safe frame — which is what printed early ellipses on the names of tiles
/// sliding in or out at both edges.
fn label_frames(cx: f32, row_y: f32, drop: f32, measure: &dyn Measure) -> (Rect, Rect) {
    let budget = SLOT - theme::space::SM;
    let left = cx - budget * 0.5;
    let top = row_y + RowStyle::CAST.h + NAME_GAP + drop;
    let name = Rect::new(left, top, budget, theme::size::LABEL as f32 + theme::space::XS);
    let role = Rect::new(left, top + measure.cap_h(theme::size::LABEL) + theme::space::XS,
        budget, ROLE_LEADING * ROLE_LINES as f32);
    (name, role)
}

fn name_caption(name: &str, width: f32, focused: bool, measure: &dyn Measure) -> String {
    crate::text::elide_by(name, width, false, |text| measure.width_str(text, theme::size::LABEL, focused))
}

fn role_view<'a>(role: &'a str, measure: &'a dyn Measure) -> TextView<'a> {
    TextView::new(role, theme::size::CAPTION, theme::TEXT_TERTIARY)
        .with_measure(measure).h(HAlign::Center).leading(ROLE_LEADING)
        .max_lines(ROLE_LINES).break_long_words()
}

fn label(
    p: Painter,
    name: &str,
    role: &str,
    cx: f32,
    row_y: f32,
    focused: bool,
    drop: f32,
    measure: &dyn Measure,
) {
    let (name_frame, role_frame) = label_frames(cx, row_y, drop, measure);
    if name_frame.w <= 0.0 { return; }
    if let Ok(name) = CString::new(name_caption(name, name_frame.w, focused, measure)) {
        let mut label = Label::new(name.as_ptr(), theme::size::LABEL,
            if focused { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY })
            .h(HAlign::Center).v(VAlign::CapTop);
        if focused { label = label.bold(); }
        label.draw(p, name_frame);
    }
    if !role.is_empty() {
        role_view(role, measure).draw(p, role_frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct LabelMeasure;
    impl Measure for LabelMeasure {
        fn width(&self, text: &std::ffi::CStr, size: i32, _: bool) -> f32 {
            text.to_string_lossy().chars().count() as f32 * size as f32 * 0.62
        }
        fn cap_h(&self, size: i32) -> f32 { size as f32 * 0.75 }
        fn line_h(&self, size: i32) -> f32 { size as f32 * 1.2 }
    }

    #[test]
    fn cast_name_elision_uses_the_remaining_width_for_a_partial_surname() {
        let measure = LabelMeasure;
        let center = crate::ui::consts::MARGIN_X + RowStyle::CAST.w * 0.5;
        let (frame, _) = label_frames(center, 100.0, 0.0, &measure);
        let text = name_caption("Алена Сяргеева 6", frame.w, true, &measure);
        assert!(text.starts_with("Алена С"), "single-line elision must not discard the whole surname: {text}");
        assert!(text.ends_with('…'));
        assert!(measure.width_str(&text, theme::size::LABEL, true) <= frame.w);
    }

    /// Issue 13: a cast label is part of its tile. A headshot scrolling out past either panel edge
    /// carries its name and role with it at the slot's full width — no clamp to the safe frame,
    /// which narrowed and re-elided the names of tiles near both edges. `label_frames` takes no
    /// painter: nothing about a label's frame depends on where the shelf has scrolled to.
    #[test]
    fn a_cast_label_moves_off_screen_with_its_tile_and_keeps_its_own_width() {
        let measure = LabelMeasure;
        let budget = SLOT - theme::space::SM;
        let scr_w = crate::ui::consts::SCR_W;
        for cx in [-40.0, 60.0, scr_w - 60.0, scr_w + 40.0, 2300.0 + scr_w - 60.0] {
            let (name, role) = label_frames(cx, 100.0, pop_drop(RowStyle::CAST.focus_scale), &measure);
            for frame in [name, role] {
                assert_eq!(frame.x, cx - budget * 0.5, "the label rides its tile at {cx}");
                assert_eq!(frame.w, budget, "the label is never re-truncated at {cx}");
            }
            assert!(role.y + role.h <= 100.0 + RowStyle::CAST.h + UNDER_H,
                "both caption lines fit in the space the next shelf reserves");
            let long = "Alexandra Wolkowicz-Harrington";
            assert_eq!(name_caption(long, name.w, false, &measure),
                name_caption(long, budget, false, &measure));
        }
    }

    #[test]
    fn combined_crew_captions_fit_two_caption_lines_at_both_safe_edges() {
        let measure = LabelMeasure;
        let safe = crate::ui::consts::SAFE;
        for preference in [plx_platform::i18n::Preference::En, plx_platform::i18n::Preference::Es, plx_platform::i18n::Preference::Be] {
            let locale = plx_platform::i18n::LocaleContext::resolve(preference, None, None, None, None);
            let caption = plx_platform::i18n::msg::browse_crew_director_writer_in(&locale);
            for center in [safe.x + RowStyle::CAST.w * 0.5,
                safe.x + safe.w - RowStyle::CAST.w * 0.5] {
                let (_, frame) = label_frames(center, 100.0, 0.0, &measure);
                let view = role_view(caption, &measure);
                assert!(!view.truncates(frame.w), "combined {preference:?} job must remain complete");
                assert!(view.measure_h(frame.w) <= frame.h);
                assert!(view.measure_h(frame.w) > ROLE_LEADING, "exercise the old one-line truncation");
            }
        }
    }

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
