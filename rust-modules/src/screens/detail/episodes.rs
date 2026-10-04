//! Episode-filmstrip state, geometry and painting for [`super::DetailScreen`].
//!
//! A filmstrip cell owns two element identities: the still plays (and may be held for its item
//! menu), while the text block opens the episode's own detail page. Encoding the row in the key
//! keeps both targets distinct through restoration and reconciliation.

use std::ffi::CString;

use plx_data::metadata::{Detail, Episode};
use plx_machine::machine::GroupId;
use plx_ui::text_lift::{TextLift, TOP_CENTRE};
use plx_ui::text_view::TextView;
use plx_ui::widgets::{self, PosterMark};
use plx_ui::{on_axis, theme, Painter, Rect};

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
/// The still's corner radius, shared by its scrim, progress bar and the label block's platter.
const CARD_RADIUS: f32 = 12.0;
/// The platter's padding around the label block on every edge.
const PLATTER_PAD: f32 = theme::space::MD;
/// The label block's wrap width: the column inset by [`PLATTER_PAD`], focused or not.
const TEXT_W: f32 = W - 2.0 * PLATTER_PAD;

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
    OpenDetail(plx_plex::plex::ServerId, String),
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
    plx_ui::consts::MARGIN_X + i as f32 * (W + GAP)
}

pub(crate) fn still_rect(i: usize, top: f32, scroll: f32) -> Rect {
    Rect::new(strip_x(i) - scroll, top, W, H)
}

pub(crate) fn meta_layout(ep: &Episode, measure: &dyn plx_machine::machine::Measure) -> (f32, f32, f32) {
    let title_h = TextView::new(&ep.title, theme::size::BODY, theme::TEXT_PRIMARY)
        .bold()
        .with_measure(measure)
        .leading(TITLE_LEAD)
        .max_lines(2)
        .measure_h(TEXT_W)
        .max(TITLE_LEAD);
    let summary_y = TITLE_DY + title_h + theme::space::MD;
    let summary_h = if ep.summary.is_empty() {
        0.0
    } else {
        TextView::new(&ep.summary, theme::size::CAPTION, theme::TEXT_SECONDARY)
            .with_measure(measure)
            .leading(SUMMARY_LEAD)
            .max_lines(SUMMARY_MAX_LINES)
            .measure_h(TEXT_W)
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

pub(crate) fn meta_rect(ep: &Episode, i: usize, top: f32, scroll: f32, measure: &dyn plx_machine::machine::Measure) -> Rect {
    let (_, _, h) = meta_layout(ep, measure);
    Rect::new(
        strip_x(i) - scroll - TEXT_PAD_X,
        top + H + META_TOP - TEXT_PAD_Y,
        W + 2.0 * TEXT_PAD_X,
        h + 2.0 * TEXT_PAD_Y,
    )
}

pub(crate) fn block_h(d: &Detail, measure: &dyn plx_machine::machine::Measure) -> f32 {
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
            label: plx_ui::fmt::time_left(ep.dur_ms - ep.resume_ms),
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
            plx_ui::fmt::dur_long(ep.dur_ms)
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

/// The kicker's cap-top offset from its texture origin — fixed by the font, so callers compute it
/// once per draw rather than once per cell.
fn kicker_cap_top() -> f32 {
    plx_gfx::text::text_cap_band(theme::size::CAPTION, 0).0
}

pub(crate) fn draw(
    p: Painter,
    d: &Detail,
    top: f32,
    scroll: f32,
    focused: Option<(usize, Row)>,
    scale: impl Fn(usize) -> f32,
    lift: impl Fn(usize) -> TextLift,
    measure: &dyn plx_machine::machine::Measure,
    meta: plx_data::metadata::MetadataView<'_>,
) {
    let stale = if meta.season_loading() {
        STALE_ALPHA
    } else {
        1.0
    };
    let p = p.alpha(stale).translate(-scroll, top);
    let cap_top = kicker_cap_top();
    for (i, ep) in d.episodes.iter().take(MAX_ITEMS).enumerate() {
        let x = strip_x(i);
        if !on_axis(x - scroll, W, plx_ui::consts::SCR_W, 0.0) {
            continue;
        }
        draw_cell(p, d, i, ep, focused, scale(i), &lift(i), cap_top, measure);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_focused(
    p: Painter,
    d: &Detail,
    index: usize,
    row: Row,
    top: f32,
    scroll: f32,
    scale: f32,
    lift: &TextLift,
    measure: &dyn plx_machine::machine::Measure,
    meta: plx_data::metadata::MetadataView<'_>,
) {
    let Some(episode) = d.episodes.get(index) else {
        return;
    };
    let stale = if meta.season_loading() {
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
        lift,
        kicker_cap_top(),
        measure,
    );
}

/// One filmstrip cell. `scale` is the still's pop spring; `lift` is the label block's own focus
/// state (earned by the TEXT stop alone). The block's plate shows for either stop, at whichever of
/// the two is further along.
#[allow(clippy::too_many_arguments)]
fn draw_cell(
    p: Painter,
    d: &Detail,
    i: usize,
    ep: &Episode,
    focused: Option<(usize, Row)>,
    scale: f32,
    lift: &TextLift,
    kicker_cap_top: f32,
    measure: &dyn plx_machine::machine::Measure,
) {
    let x = strip_x(i);
    let still_focused = focused == Some((i, Row::Still));
    let focused_here = still_focused || scale > 1.001;
    let pop = widgets::pop_factor(scale, theme::EP_CARD_FOCUS_SCALE);
    let card = Rect::new(x, -theme::EP_CARD_FOCUS_LIFT * pop, W, H);
    widgets::draw_card_peaked(
        p,
        card,
        d.sid.raw(),
        &ep.thumb,
        (640, 360),
        CARD_RADIUS,
        focused_here,
        scale,
        theme::EP_CARD_FOCUS_SCALE,
    );
    let drawn = if focused_here {
        card.scaled(scale)
    } else {
        card
    };
    let st = state(ep);
    widgets::art_scrim(
        p,
        drawn,
        CARD_RADIUS,
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
        widgets::progress_bar(p, drawn, CARD_RADIUS, 5.0, frac);
    }

    // Every episode's label block draws in the same ink; focus is marked by the shared
    // `ui::text_lift` treatment. The plate shows while the still pops or the text is lifted; the
    // scale and shadow belong to the text stop alone, and the still above does not follow them.
    let text_top = H + META_TOP;
    let (date_y, summary_y, content_h_with_pad) = meta_layout(ep, measure);
    let text_x = x + PLATTER_PAD;
    let labels = |p: Painter| {
        if let Ok(kicker) = CString::new(plx_platform::i18n::msg::browse_detail_episode_number(ep.index as i64)) {
            p.text(
                kicker.as_ptr(),
                text_x,
                text_top,
                theme::size::CAPTION,
                theme::EP_META_INK,
                0,
                1,
            );
        }
        TextView::new(&ep.title, theme::size::BODY, theme::EP_TITLE_INK)
        .bold()
        .with_measure(measure)
        .leading(TITLE_LEAD)
        .max_lines(2)
        .draw(p, Rect::new(text_x, text_top + TITLE_DY, TEXT_W, 0.0));
        if !ep.summary.is_empty() {
            TextView::new(&ep.summary, theme::size::CAPTION, theme::EP_SUMMARY_INK)
            .with_measure(measure)
            .leading(SUMMARY_LEAD)
            .max_lines(SUMMARY_MAX_LINES)
            .draw(p, Rect::new(text_x, text_top + summary_y, TEXT_W, 0.0));
        }
        let date = plx_ui::fmt::pretty_date(&ep.aired, 0);
        if let Ok(date) = CString::new(date) {
            let width = p.text(
                date.as_ptr(),
                text_x,
                text_top + date_y,
                theme::size::MICRO,
                theme::EP_META_INK,
                0,
                0,
            );
            if !ep.rating.is_empty() {
                let (top, baseline) = plx_gfx::text::text_cap_band(theme::size::MICRO, 0);
                plx_ui::widgets::keyline_chip(
                    p,
                    text_x + width + theme::space::SM,
                    text_top + date_y + (top + baseline) * 0.5,
                    &ep.rating,
                    theme::EP_META_INK,
                    measure,
                );
            }
        }
    };
    let plate_factor = pop.max(lift.factor());
    if plate_factor <= 0.0 {
        return labels(p);
    }
    // Hug the content: the top sits `PLATTER_PAD` above the kicker's cap-top, the bottom
    // `PLATTER_PAD` below the date/rating row `meta_layout` measured to.
    let platter_top = text_top + kicker_cap_top - PLATTER_PAD;
    let content_h = content_h_with_pad - META_BOTTOM_PAD - kicker_cap_top;
    let platter = Rect::new(x, platter_top, W, content_h + 2.0 * PLATTER_PAD);
    plx_ui::text_lift::draw(p, platter, CARD_RADIUS, lift, plate_factor, TOP_CENTRE, labels);
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
        assert!(plx_ui::widgets::STILL_LINE_BOT > BAR_H);
        assert!(
            plx_ui::widgets::STILL_SCRIM_H_1
                > plx_ui::widgets::STILL_LINE_BOT + plx_ui::widgets::STILL_GLYPH_D
        );
        let card = Rect::new(0.0, 0.0, W, H);
        for scale in [1.0, 1.045, theme::EP_CARD_FOCUS_SCALE, plx_ui::widgets::CARD_FOCUS_SCALE] {
            let drawn = card.scaled(scale);
            let bar = Rect::new(drawn.x, drawn.y + drawn.h - BAR_H, drawn.w, BAR_H);
            assert_eq!((bar.x, bar.w), (drawn.x, drawn.w), "the bar is full bleed");
            assert!((bar.y + bar.h - (drawn.y + drawn.h)).abs() < 0.01);
            assert!(plx_ui::widgets::STILL_SCRIM_H_1 < drawn.h);
        }
    }

    fn cell_census(scale: f32, lift: &TextLift) -> Vec<(u64, Rect)> {
        let measure = plx_ui::fixture::FixtureMeasure;
        let d = Detail::default();
        let ep = Episode { dur_ms: 100, ..Default::default() };
        plx_ui::draw_census::capture(|| {
            draw_cell(Painter::recording(), &d, 0, &ep, None, scale, lift, kicker_cap_top(), &measure)
        })
    }

    fn settled_lift(focused: bool) -> TextLift {
        let mut lift = TextLift::new();
        for _ in 0..240 {
            lift.step(focused, 1.0 / 60.0);
        }
        lift
    }

    /// The label block's plate shows while either stop of the cell holds focus, and nothing of it
    /// survives once focus has left and the spring has settled.
    #[test]
    fn the_label_plate_shows_for_either_stop_and_leaves_nothing_at_rest() {
        let _g = plx_base::testlock::serial();
        let rest = cell_census(1.0, &TextLift::new());
        assert!(
            cell_census(theme::EP_CARD_FOCUS_SCALE, &TextLift::new()).len() > rest.len(),
            "the still's pop shows the plate"
        );
        assert!(
            cell_census(1.0, &settled_lift(true)).len() > rest.len(),
            "the text stop's lift shows the plate"
        );

        let mut lift = settled_lift(true);
        for _ in 0..240 {
            lift.step(false, 1.0 / 60.0);
        }
        assert_eq!(cell_census(1.0, &lift), rest);
    }

    #[test]
    fn strip_x_matches_the_shared_shelf_formula() {
        for i in 0..20 {
            assert_eq!(
                strip_x(i),
                plx_ui::card_row::tile_rect(
                    i,
                    plx_ui::consts::MARGIN_X,
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
