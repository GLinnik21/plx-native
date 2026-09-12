//! Episode-filmstrip state, geometry and painting for [`super::DetailScreen`].
//!
//! A filmstrip cell owns two element identities: the still plays (and may be held for its item
//! menu), while the text block opens the episode's own detail page. Encoding the row in the key
//! keeps both targets distinct through restoration and reconciliation.

use std::ffi::CString;

use crate::metadata::{Detail, Episode};
use crate::ui::machine::GroupId;
use crate::ui::text_view::TextView;
use crate::ui::widgets::{self, PosterMark};
use crate::ui::{on_axis, theme, Painter, Rect};

pub(crate) const EPISODES_ELEM_RANGE_START: u32 = 128;
pub(crate) const EPISODES_ELEM_RANGE_END: u32 = 640;
pub(crate) const EPISODES_GROUP: GroupId = GroupId(2);

pub(crate) const MAX_ITEMS: usize =
    ((EPISODES_ELEM_RANGE_END - EPISODES_ELEM_RANGE_START) / 2) as usize;
pub(crate) const W: f32 = 420.0;
pub(crate) const H: f32 = 236.0;
pub(crate) const GAP: f32 = 28.0;
pub(crate) const META_TOP: f32 = 30.0;
const TITLE_DY: f32 = 46.0;
const TITLE_LEAD: f32 = 34.0;
const SUMMARY_LEAD: f32 = 34.0;
const SUMMARY_MAX_LINES: usize = 8;
const META_BOTTOM_PAD: f32 = 24.0;
const TEXT_PAD_X: f32 = theme::space::XS;
const TEXT_PAD_Y: f32 = theme::space::SM;
pub(crate) const STALE_ALPHA: f32 = 0.35;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Row {
    #[default]
    Still,
    Text,
}

pub(crate) fn elem(index: usize, row: Row) -> Option<u32> {
    (index < MAX_ITEMS)
        .then_some(EPISODES_ELEM_RANGE_START + index as u32 * 2 + u32::from(row == Row::Text))
}

pub(crate) fn locate(key: u32) -> Option<(usize, Row)> {
    if !(EPISODES_ELEM_RANGE_START..EPISODES_ELEM_RANGE_END).contains(&key) {
        return None;
    }
    let raw = key - EPISODES_ELEM_RANGE_START;
    Some((
        (raw / 2) as usize,
        if raw & 1 == 0 { Row::Still } else { Row::Text },
    ))
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    None,
    Play(usize),
    OpenDetail(crate::plex::ServerId, String),
}

pub(crate) fn action(d: &Detail, key: u32, loading: bool) -> Action {
    if loading {
        return Action::None;
    }
    let Some((i, row)) = locate(key) else {
        return Action::None;
    };
    let Some(ep) = d.episodes.get(i) else {
        return Action::None;
    };
    match row {
        Row::Still => Action::Play(i),
        Row::Text => Action::OpenDetail(d.sid, ep.rk.clone()),
    }
}

pub(crate) fn strip_x(i: usize) -> f32 {
    crate::ui::consts::MARGIN_X + i as f32 * (W + GAP)
}

pub(crate) fn still_rect(i: usize, top: f32, scroll: f32) -> Rect {
    Rect::new(strip_x(i) - scroll, top, W, H)
}

pub(crate) fn meta_layout(ep: &Episode, measure: &dyn crate::ui::machine::Measure) -> (f32, f32, f32) {
    let title_h = TextView::new(&ep.title, theme::size::BODY, theme::TEXT_PRIMARY)
        .bold()
        .with_measure(measure)
        .leading(TITLE_LEAD)
        .max_lines(2)
        .measure_h(W)
        .max(TITLE_LEAD);
    let summary_y = TITLE_DY + title_h + theme::space::MD;
    let summary_h = if ep.summary.is_empty() {
        0.0
    } else {
        TextView::new(&ep.summary, theme::size::CAPTION, theme::TEXT_SECONDARY)
            .with_measure(measure)
            .leading(SUMMARY_LEAD)
            .max_lines(SUMMARY_MAX_LINES)
            .measure_h(W)
    };
    let date_y = summary_y
        + summary_h
        + if ep.aired.is_empty() {
            0.0
        } else {
            theme::space::MD
        };
    let bottom = if ep.aired.is_empty() {
        summary_y + summary_h
    } else {
        date_y + theme::size::MICRO as f32
    };
    (date_y, summary_y, bottom + META_BOTTOM_PAD)
}

pub(crate) fn meta_rect(ep: &Episode, i: usize, top: f32, scroll: f32, measure: &dyn crate::ui::machine::Measure) -> Rect {
    let (_, _, h) = meta_layout(ep, measure);
    Rect::new(
        strip_x(i) - scroll - TEXT_PAD_X,
        top + H + META_TOP - TEXT_PAD_Y,
        W + 2.0 * TEXT_PAD_X,
        h + 2.0 * TEXT_PAD_Y,
    )
}

pub(crate) fn block_h(d: &Detail, measure: &dyn crate::ui::machine::Measure) -> f32 {
    H + d
        .episodes
        .iter()
        .map(|e| meta_layout(e, measure).2)
        .fold(0.0, f32::max)
        + META_TOP
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Glyph {
    Play,
    Watched,
    None,
}

struct State {
    glyph: Glyph,
    label: String,
    progress: Option<f32>,
}

fn state(ep: &Episode) -> State {
    let in_progress = ep.dur_ms > 0 && ep.resume_ms > 0 && ep.resume_ms < ep.dur_ms;
    if in_progress {
        return State {
            glyph: Glyph::None,
            label: crate::ui::fmt::time_left(ep.dur_ms - ep.resume_ms),
            progress: Some((ep.resume_ms as f32 / ep.dur_ms as f32).clamp(0.0, 1.0)),
        };
    }
    State {
        glyph: if ep.watched {
            Glyph::Watched
        } else {
            Glyph::Play
        },
        label: if ep.dur_ms > 0 {
            crate::ui::fmt::dur_long(ep.dur_ms)
        } else {
            String::new()
        },
        progress: None,
    }
}

pub(crate) fn watch_state(ep: &Episode) -> PosterMark {
    let st = state(ep);
    if st.progress.is_some() {
        PosterMark::InProgress
    } else if st.glyph == Glyph::Watched {
        PosterMark::Watched
    } else {
        PosterMark::None
    }
}

pub(crate) fn draw(
    p: Painter,
    d: &Detail,
    top: f32,
    scroll: f32,
    focused: Option<(usize, Row)>,
    scale: impl Fn(usize) -> f32,
    measure: &dyn crate::ui::machine::Measure,
) {
    let stale = if crate::metadata::season_loading() {
        STALE_ALPHA
    } else {
        1.0
    };
    let p = p.alpha(stale).translate(-scroll, top);
    for (i, ep) in d.episodes.iter().take(MAX_ITEMS).enumerate() {
        let x = strip_x(i);
        if !on_axis(x - scroll, W, crate::ui::consts::SCR_W, 0.0) {
            continue;
        }
        draw_cell(p, d, i, ep, focused, scale(i), measure);
    }
}

pub(crate) fn draw_focused(
    p: Painter,
    d: &Detail,
    index: usize,
    row: Row,
    top: f32,
    scroll: f32,
    scale: f32,
    measure: &dyn crate::ui::machine::Measure,
) {
    let Some(episode) = d.episodes.get(index) else {
        return;
    };
    let stale = if crate::metadata::season_loading() {
        STALE_ALPHA
    } else {
        1.0
    };
    draw_cell(
        p.alpha(stale).translate(-scroll, top),
        d,
        index,
        episode,
        Some((index, row)),
        scale,
        measure,
    );
}

fn draw_cell(
    p: Painter,
    d: &Detail,
    i: usize,
    ep: &Episode,
    focused: Option<(usize, Row)>,
    scale: f32,
    measure: &dyn crate::ui::machine::Measure,
) {
    let x = strip_x(i);
    let row = focused.filter(|(at, _)| *at == i).map(|(_, row)| row);
    let still_focused = row == Some(Row::Still);
    let card = Rect::new(x, 0.0, W, H);
    widgets::draw_card(
        p,
        card,
        d.sid,
        &ep.thumb,
        (640, 360),
        12.0,
        still_focused || scale > 1.001,
        scale,
    );
    let drawn = if still_focused || scale > 1.001 {
        card.scaled(scale)
    } else {
        card
    };
    let st = state(ep);
    widgets::art_scrim(
        p,
        drawn,
        12.0,
        widgets::STILL_SCRIM_H_1,
        widgets::STILL_SCRIM_A,
    );
    widgets::still_line(
        p,
        drawn,
        watch_state(ep),
        "",
        &st.label,
        true,
        st.progress.is_some(),
        measure,
    );
    if let Some(frac) = st.progress {
        widgets::progress_bar(p, drawn, 12.0, 5.0, frac);
    }

    let text_top = H + META_TOP;
    let dim = theme::TEXT_TERTIARY;
    let (date_y, summary_y, _) = meta_layout(ep, measure);
    if row == Some(Row::Text) {
        widgets::text_block_highlight(p, meta_rect(ep, i, 0.0, 0.0, measure));
    }
    if let Ok(kicker) = CString::new(format!("EPISODE {}", ep.index)) {
        p.text(
            kicker.as_ptr(),
            x,
            text_top,
            theme::size::CAPTION,
            dim,
            0,
            1,
        );
    }
    TextView::new(
        &ep.title,
        theme::size::BODY,
        if row.is_some() {
            theme::TEXT_PRIMARY
        } else {
            theme::TEXT_SECONDARY
        },
    )
    .bold()
    .with_measure(measure)
    .leading(TITLE_LEAD)
    .max_lines(2)
    .draw(p, Rect::new(x, text_top + TITLE_DY, W, 0.0));
    if !ep.summary.is_empty() {
        TextView::new(
            &ep.summary,
            theme::size::CAPTION,
            if row.is_some() {
                theme::TEXT_SECONDARY
            } else {
                dim
            },
        )
        .with_measure(measure)
        .leading(SUMMARY_LEAD)
        .max_lines(SUMMARY_MAX_LINES)
        .draw(p, Rect::new(x, text_top + summary_y, W, 0.0));
    }
    let date = crate::ui::fmt::pretty_date(&ep.aired, 0);
    if let Ok(date) = CString::new(date) {
        let width = p.text(
            date.as_ptr(),
            x,
            text_top + date_y,
            theme::size::MICRO,
            dim,
            0,
            0,
        );
        if !ep.rating.is_empty() {
            let (top, baseline) = crate::text::text_cap_band(theme::size::MICRO, 0);
            crate::ui::widgets::keyline_chip(
                p,
                x + width + theme::space::SM,
                text_top + date_y + (top + baseline) * 0.5,
                &ep.rating,
                dim,
                measure,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn still_and_text_have_distinct_stable_keys() {
        for i in 0..MAX_ITEMS {
            let still = elem(i, Row::Still).unwrap();
            let text = elem(i, Row::Text).unwrap();
            assert_ne!(still, text);
            assert_eq!(locate(still), Some((i, Row::Still)));
            assert_eq!(locate(text), Some((i, Row::Text)));
        }
    }

    #[test]
    fn in_progress_wins_over_watched() {
        let ep = Episode {
            watched: true,
            resume_ms: 30,
            dur_ms: 100,
            ..Default::default()
        };
        assert_eq!(watch_state(&ep), PosterMark::InProgress);
    }

    #[test]
    fn a_resume_at_the_end_is_not_in_progress() {
        let ep = Episode {
            watched: true,
            resume_ms: 100,
            dur_ms: 100,
            ..Default::default()
        };
        assert_eq!(watch_state(&ep), PosterMark::Watched);
    }

    #[test]
    fn an_episode_still_resolves_its_three_states_into_one_mark() {
        let fresh = Episode {
            dur_ms: 100,
            ..Default::default()
        };
        let partial = Episode {
            dur_ms: 100,
            resume_ms: 25,
            ..Default::default()
        };
        let watched = Episode {
            dur_ms: 100,
            watched: true,
            ..Default::default()
        };
        assert_eq!(
            (state(&fresh).glyph, state(&fresh).progress),
            (Glyph::Play, None)
        );
        assert_eq!(state(&partial).glyph, Glyph::None);
        assert_eq!(state(&partial).progress, Some(0.25));
        assert_eq!(
            (state(&watched).glyph, state(&watched).progress),
            (Glyph::Watched, None)
        );
    }

    #[test]
    fn an_episode_resolves_the_same_three_states_for_the_menu_opened_on_it() {
        for (ep, expected) in [
            (
                Episode {
                    dur_ms: 100,
                    ..Default::default()
                },
                PosterMark::None,
            ),
            (
                Episode {
                    dur_ms: 100,
                    resume_ms: 25,
                    ..Default::default()
                },
                PosterMark::InProgress,
            ),
            (
                Episode {
                    dur_ms: 100,
                    watched: true,
                    ..Default::default()
                },
                PosterMark::Watched,
            ),
        ] {
            assert_eq!(watch_state(&ep), expected);
        }
    }

    #[test]
    fn the_state_line_clears_the_full_bleed_bar_at_every_pop_phase() {
        const BAR_H: f32 = 5.0;
        assert!(crate::ui::widgets::STILL_LINE_BOT > BAR_H);
        assert!(
            crate::ui::widgets::STILL_SCRIM_H_1
                > crate::ui::widgets::STILL_LINE_BOT + crate::ui::widgets::STILL_GLYPH_D
        );
        let card = Rect::new(0.0, 0.0, W, H);
        for scale in [1.0, 1.045, crate::ui::widgets::CARD_FOCUS_SCALE] {
            let drawn = card.scaled(scale);
            let bar = Rect::new(drawn.x, drawn.y + drawn.h - BAR_H, drawn.w, BAR_H);
            assert_eq!((bar.x, bar.w), (drawn.x, drawn.w), "the bar is full bleed");
            assert!((bar.y + bar.h - (drawn.y + drawn.h)).abs() < 0.01);
            assert!(crate::ui::widgets::STILL_SCRIM_H_1 < drawn.h);
        }
    }

    #[test]
    fn strip_x_matches_the_shared_shelf_formula() {
        for i in 0..20 {
            assert_eq!(
                strip_x(i),
                crate::ui::card_row::tile_rect(
                    i,
                    crate::ui::consts::MARGIN_X,
                    W + GAP,
                    0.0,
                    0.0,
                    (W, H),
                )
                .x
            );
        }
    }
}
