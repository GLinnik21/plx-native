//! Episode-filmstrip state, geometry and painting for [`super::DetailScreen`].
//!
//! A filmstrip cell owns two element identities: the still plays (and may be held for its item
//! menu), while the text block opens the episode's own detail page. Encoding the row in the key
//! keeps both targets distinct through restoration and reconciliation.

use std::ffi::CString;

use plx_data::metadata::{Detail, Episode};
use plx_machine::machine::GroupId;
use plx_ui::hstack::{Align, HStack, Item};
use plx_ui::text_lift::{TextLift, TOP_CENTRE};
use plx_ui::text_view::TextView;
use plx_ui::vstack::{Anchor, VStack};
use plx_ui::widgets::{self, PosterMark};
use plx_ui::{on_axis, theme, Painter, Rect};

pub const EPISODES_ELEM_RANGE_START: u32 = 1 << 30;
pub const EPISODES_ELEM_RANGE_END: u32 = 1 << 31;
pub const EPISODES_GROUP: GroupId = GroupId(2);

/// Episodes the id block can name: two ids each, so a season of any length the server sends has a
/// local id per episode and no screen-side count limits what can be reached.
const ID_SPAN: usize = ((EPISODES_ELEM_RANGE_END - EPISODES_ELEM_RANGE_START) / 2) as usize;
/// Cells either side of the screen that are keyed and laid out as well, so a press that scrolls the
/// strip finds its neighbour already seated.
pub const LEAD_CELLS: usize = 1;
pub const W: f32 = 420.0;
pub const H: f32 = 236.0;
pub const GAP: f32 = 28.0;
pub const META_TOP: f32 = 30.0;
const TITLE_DY: f32 = 46.0;
const TITLE_LEAD: f32 = 34.0;
const SUMMARY_LEAD: f32 = 34.0;
const SUMMARY_MAX_LINES: usize = 8;
const META_BOTTOM_PAD: f32 = 24.0;
const TEXT_PAD_X: f32 = theme::space::XS;
const TEXT_PAD_Y: f32 = theme::space::SM;
pub const STALE_ALPHA: f32 = 0.35;
/// The still's corner radius, shared by its scrim, progress bar and the label block's platter.
const CARD_RADIUS: f32 = 12.0;
/// The platter's padding around the label block on every edge.
const PLATTER_PAD: f32 = theme::space::MD;
/// The label block's wrap width: the column inset by [`PLATTER_PAD`], focused or not.
const TEXT_W: f32 = W - 2.0 * PLATTER_PAD;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Row {
    #[default]
    Still,
    Text,
}

pub fn elem(index: usize, row: Row) -> Option<u32> {
    (index < ID_SPAN)
        .then_some(EPISODES_ELEM_RANGE_START + index as u32 * 2 + u32::from(row == Row::Text))
}

pub fn locate(key: u32) -> Option<(usize, Row)> {
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
pub enum Action {
    None,
    Play(usize),
    OpenDetail(plx_plex::plex::ServerId, String),
}

pub fn action(d: &Detail, key: u32, loading: bool) -> Action {
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

pub fn strip_x(i: usize) -> f32 {
    plx_ui::consts::MARGIN_X + i as f32 * (W + GAP)
}

/// The strip scroll that stands episode `i` of `len` at the strip's leading edge, so the episode a
/// viewer left off at opens first in view with the ones to watch after it. Clamped like
/// `scroll_into_view`: a season's last episodes do not scroll past its end.
pub fn seat_scroll(i: usize, len: usize) -> f32 {
    let pitch = W + GAP;
    let view_w = plx_ui::consts::SCR_W - 2.0 * plx_ui::consts::MARGIN_X;
    let end = (len as f32 * pitch - GAP - view_w).max(0.0);
    (i as f32 * pitch).min(end)
}

pub fn still_rect(i: usize, top: f32, scroll: f32) -> Rect {
    Rect::new(strip_x(i) - scroll, top, W, H)
}

/// **The label block's one stack**, title / summary / date row: an empty summary, or neither air
/// date nor rating chip (one row paints both), is ABSENT, not a zero-high row, so there is one `MD`
/// between the blocks that are there and none after the last (the date sits one gap under the
/// title when there is no summary, and a title-only platter ends at the title). `meta_layout` reads
/// it at the heights the episode measured, [`reserved_text_h`] at the tallest it can have.
fn meta_items(title_h: f32, summary_h: Option<f32>, has_row: bool) -> [Item; 3] {
    [
        Item::new(title_h),
        summary_h.map_or(Item::absent(), Item::new),
        if has_row { Item::new(theme::size::MICRO as f32) } else { Item::absent() },
    ]
}

pub fn meta_layout(ep: &Episode, measure: &dyn plx_machine::machine::Measure) -> (f32, f32, f32) {
    let title_h = TextView::new(&ep.title, theme::size::BODY, theme::TEXT_PRIMARY)
        .bold()
        .with_measure(measure)
        .leading(TITLE_LEAD)
        .max_lines(2)
        .measure_h(TEXT_W)
        .max(TITLE_LEAD);
    let summary_h = if ep.summary.is_empty() {
        0.0
    } else {
        TextView::new(&ep.summary, theme::size::CAPTION, theme::TEXT_SECONDARY)
            .with_measure(measure)
            .leading(SUMMARY_LEAD)
            .max_lines(SUMMARY_MAX_LINES)
            .measure_h(TEXT_W)
    };
    let items = meta_items(title_h, (!ep.summary.is_empty()).then_some(summary_h), !(ep.aired.is_empty() && ep.rating.is_empty()));
    let stack = VStack::new(&items, theme::space::MD, Anchor::Top(TITLE_DY));
    // Where a summary WOULD stand (unread when there is none: the draw skips an empty one): the
    // same stack with a zero-high summary present.
    let probe = meta_items(title_h, Some(0.0), false);
    let (summary_y, _) = stack.span(1).or_else(|| VStack::new(&probe, theme::space::MD, Anchor::Top(TITLE_DY)).span(1)).unwrap_or((TITLE_DY, 0.0));
    let bottom = TITLE_DY + stack.height();
    let date_y = stack.span(2).map_or(summary_y + summary_h, |(y, _)| y);
    (date_y, summary_y, bottom + META_BOTTOM_PAD)
}

pub fn meta_rect(ep: &Episode, i: usize, top: f32, scroll: f32, measure: &dyn plx_machine::machine::Measure) -> Rect {
    let (_, _, h) = meta_layout(ep, measure);
    Rect::new(
        strip_x(i) - scroll - TEXT_PAD_X,
        top + H + META_TOP - TEXT_PAD_Y,
        W + 2.0 * TEXT_PAD_X,
        h + 2.0 * TEXT_PAD_Y,
    )
}

/// The tallest label block any episode can have: a two-line title, a summary at its line cap and a
/// date row. A season longer than a page reserves this for its whole life, so a page landing never
/// changes the strip's height under the viewer.
fn reserved_text_h() -> f32 {
    let items = meta_items(2.0 * TITLE_LEAD, Some(SUMMARY_MAX_LINES as f32 * SUMMARY_LEAD), true);
    TITLE_DY + VStack::new(&items, theme::space::MD, Anchor::Top(TITLE_DY)).height() + META_BOTTOM_PAD
}

pub fn block_h(d: &Detail, measure: &dyn plx_machine::machine::Measure) -> f32 {
    if d.episodes.len() > plx_data::metadata::EPISODE_PAGE {
        return H + reserved_text_h() + META_TOP;
    }
    H + d
        .episodes
        .iter_loaded()
        .map(|(_, e)| meta_layout(e, measure).2)
        .fold(0.0, f32::max)
        + META_TOP
}

/// The episode indices whose cells reach the screen at strip scroll `scroll`, widened by `lead`
/// cells each side and clamped to a season of `len`. Pure index arithmetic, so what is keyed,
/// laid out and drawn follows the screen and not the season's length.
pub fn visible(scroll: f32, len: usize, lead: usize) -> std::ops::Range<usize> {
    let pitch = W + GAP;
    let first = ((scroll - plx_ui::consts::MARGIN_X - W) / pitch).floor().max(0.0) as usize;
    let last = ((scroll + plx_ui::consts::SCR_W - plx_ui::consts::MARGIN_X) / pitch).ceil().max(0.0) as usize;
    first.saturating_sub(lead).min(len)..(last + lead).min(len)
}

/// A few cells' springs, keyed by episode: the focused cell and the one just left are the only
/// ones with motion, so a season of any length needs no per-episode state. A cell that owns no
/// slot reads as at rest.
#[derive(Clone)]
pub struct Cells {
    owner: [usize; SLOTS],
    scale: [plx_ui::Spring; SLOTS],
    lift: [TextLift; SLOTS],
}

const SLOTS: usize = 8;
const NONE: usize = usize::MAX;

impl Cells {
    pub fn new() -> Self {
        Self { owner: [NONE; SLOTS], scale: [plx_ui::Spring::at(1.0); SLOTS], lift: [TextLift::new(); SLOTS] }
    }

    pub fn scale(&self, i: usize) -> f32 {
        let s = i % SLOTS;
        if self.owner[s] == i { self.scale[s].pos } else { 1.0 }
    }

    pub fn lift(&self, i: usize) -> TextLift {
        let s = i % SLOTS;
        if self.owner[s] == i { self.lift[s] } else { TextLift::new() }
    }

    /// Advances every owned cell toward its target. `still` / `text` are the focused cell by stop.
    pub fn step(&mut self, still: Option<usize>, text: Option<usize>, dt: f32) {
        for i in [still, text].into_iter().flatten() {
            let s = i % SLOTS;
            if self.owner[s] != i {
                self.owner[s] = i;
                self.scale[s] = plx_ui::Spring::at(1.0);
                self.lift[s] = TextLift::new();
            }
        }
        for s in 0..SLOTS {
            let i = self.owner[s];
            if i == NONE {
                continue;
            }
            let held = still == Some(i);
            self.scale[s].step(if held { theme::EP_CARD_FOCUS_SCALE } else { 1.0 }, 300.0, dt);
            // A released cell comes to EXACT rest by the shared landing rule, as a shelf's released tile
            // does (`CardRow::update`): `step` alone parks it one ulp above 1.0 for good, so it would never
            // read as at rest ([`widgets::pop_factor`] must be exactly 0.0 there).
            if !held {
                self.scale[s].land(1.0, dt);
            }
            self.lift[s].step(text == Some(i), dt);
        }
    }
}

impl Default for Cells {
    fn default() -> Self {
        Self::new()
    }
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
            plx_ui::fmt::duration(ep.dur_ms)
        } else {
            String::new()
        },
        progress: None,
    }
}

pub fn watch_state(ep: &Episode) -> PosterMark {
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

/// The strip cell at strip x `x`, which the sentinel build covers flat while its season reloads:
/// the cell shows the PREVIOUS season's art through the [`STALE_ALPHA`] cascade, which can never be
/// an exact colour, so the cover is what the pixel oracle sees. Each draw site counts the cell
/// first (`placeholder::note`, `StaleEpisodes`).
fn cell_rect(x: f32) -> Rect {
    Rect::new(x, 0.0, W, H)
}

/// The strip, as the PAGE pass draws it. `scale` is each cell's pop spring and `dip` the press
/// machine's factor for its still (`PressRead::dip_of`; `1.0` for every cell but the pressed one),
/// so a press dips the page's own copy as it dips a card. `left_out` names the cell a surface lifts
/// out of the dim (`DrawFrame::leaves_out`): the page draws NOTHING of it, still or label plate,
/// because the opener lift ([`draw_focused`]) is its only copy.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    p: Painter,
    d: &Detail,
    top: f32,
    scroll: f32,
    focused: Option<(usize, Row)>,
    scale: impl Fn(usize) -> f32,
    dip: impl Fn(usize) -> f32,
    left_out: impl Fn(usize) -> bool,
    lift: impl Fn(usize) -> TextLift,
    measure: &dyn plx_machine::machine::Measure,
    meta: plx_data::metadata::MetadataView<'_>,
) {
    // read ONCE per strip: `season_loading` is two SeqCst loads, and the per-cell hook below must
    // stay a branch on this local
    let reloading = meta.season_loading();
    let stale = if reloading {
        STALE_ALPHA
    } else {
        1.0
    };
    let p = p.alpha(stale).translate(-scroll, top);
    let cap_top = kicker_cap_top();
    for i in visible(scroll, d.episodes.len(), 0) {
        let x = strip_x(i);
        if !on_axis(x - scroll, W, plx_ui::consts::SCR_W, 0.0) {
            continue;
        }
        let Some(ep) = d.episodes.get(i) else {
            // A page that has not landed: the still's own skeleton, at the cell's own place.
            plx_ui::placeholder::skeleton(p, plx_ui::placeholder::Reason::CardSkeleton, "", cell_rect(x), CARD_RADIUS);
            continue;
        };
        if left_out(i) {
            continue;
        }
        draw_cell(p, d, i, ep, focused, scale(i), dip(i), &lift(i), cap_top, measure);
        if reloading {
            plx_ui::placeholder::note(p, plx_ui::placeholder::Reason::StaleEpisodes, &ep.rk);
            plx_ui::placeholder::stale_cover(p, cell_rect(x), CARD_RADIUS);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn draw_focused(
    p: Painter,
    d: &Detail,
    index: usize,
    row: Row,
    top: f32,
    scroll: f32,
    scale: f32,
    dip: f32,
    lift: &TextLift,
    measure: &dyn plx_machine::machine::Measure,
    meta: plx_data::metadata::MetadataView<'_>,
) {
    let Some(episode) = d.episodes.get(index) else {
        return;
    };
    let reloading = meta.season_loading();
    let stale = if reloading {
        STALE_ALPHA
    } else {
        1.0
    };
    let cell = p.alpha(stale).translate(-scroll, top);
    draw_cell(
        cell,
        d,
        index,
        episode,
        Some((index, row)),
        scale,
        dip,
        lift,
        kicker_cap_top(),
        measure,
    );
    if reloading {
        plx_ui::placeholder::note(cell, plx_ui::placeholder::Reason::StaleEpisodes, &episode.rk);
        plx_ui::placeholder::stale_cover(cell, cell_rect(strip_x(index)), CARD_RADIUS);
    }
}

/// One filmstrip cell. `scale` is the still's pop spring and `dip` the press's factor on top of it
/// (the still is drawn at `scale * dip`; the pop that earns the label plate follows the spring
/// alone, as a card's does, so a press does not blink the plate out); `lift` is the label block's
/// own focus state (earned by the TEXT stop alone). The block's plate shows for either stop, at
/// whichever of the two is further along.
#[allow(clippy::too_many_arguments)]
fn draw_cell(
    p: Painter,
    d: &Detail,
    i: usize,
    ep: &Episode,
    focused: Option<(usize, Row)>,
    scale: f32,
    dip: f32,
    lift: &TextLift,
    kicker_cap_top: f32,
    measure: &dyn plx_machine::machine::Measure,
) {
    let x = strip_x(i);
    let still_focused = focused == Some((i, Row::Still));
    let pop = widgets::pop_factor(scale, theme::EP_CARD_FOCUS_SCALE);
    // A cell is drawn at its spring's scale for as long as the spring is off rest: the pop factor is
    // exactly 0.0 only at rest (a released cell lands there, `Cells::step`), so no threshold of its own.
    let focused_here = still_focused || pop > 0.0;
    let drawn_scale = scale * dip;
    let card = Rect::new(x, -theme::EP_CARD_FOCUS_LIFT * widgets::pop_factor(drawn_scale, theme::EP_CARD_FOCUS_SCALE), W, H);
    #[cfg(feature = "devtriggers")]
    plx_ui::card_probe::begin_card();
    widgets::draw_card_peaked(
        p,
        card,
        d.sid.raw(),
        &ep.thumb,
        (640, 360),
        CARD_RADIUS,
        focused_here,
        drawn_scale,
        theme::EP_CARD_FOCUS_SCALE,
    );
    let drawn = if focused_here {
        card.scaled(drawn_scale)
    } else {
        card
    };
    // `plxnative-focusx`: the strip is not a `cards::Shelf`, so it reports its focused still itself.
    #[cfg(feature = "devtriggers")]
    if still_focused && !p.is_recording() {
        let at = p.to_screen(drawn).0;
        plx_ui::card_probe::focused(at.x, at.y, at.w, i, d.episodes.len(), i);
    }
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
        if let Ok(kicker) = CString::new(plx_ui::fmt::episode_card_kicker(ep.index)) {
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
                // The date run, one SM, then the chip: the chip's left edge is the marker's x.
                let run = [Item::new(width), Item::new(0.0)];
                let chip_x = HStack::new(&run, theme::space::SM, Align::Leading(text_x)).span(1).map_or(text_x, |(x, _)| x);
                plx_ui::widgets::keyline_chip(
                    p,
                    chip_x,
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
        for i in (0..2000).chain([ID_SPAN - 1]) {
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
            draw_cell(Painter::recording(), &d, 0, &ep, None, scale, 1.0, lift, kicker_cap_top(), &measure)
        })
    }

    /// **The words one episode's card drew** (the kicker over its title, its title, its date).
    fn card_words(ep: &Episode) -> Vec<String> {
        let measure = plx_ui::fixture::FixtureMeasure;
        let d = Detail::default();
        plx_ui::draw_census::capture_text(|| {
            draw_cell(Painter::recording(), &d, 0, ep, None, 1.0, 1.0, &TextLift::new(), kicker_cap_top(), &measure)
        })
        .into_iter()
        .map(|(text, _)| text)
        .collect()
    }

    /// **A card for an episode the server did not number never reads "EPISODE 0"**: its kicker is the
    /// kind word instead (its date, when it has one, is already the card's own bottom row, so the
    /// kicker does not say it twice), and a numbered episode keeps "EPISODE N".
    #[test]
    fn a_numberless_episode_card_never_reads_episode_zero() {
        let _g = plx_base::testlock::serial();
        let kind = plx_platform::i18n::msg::browse_kind_episode().to_uppercase();
        let dated = Episode { title: "Pilot".into(), aired: "2016-04-21".into(), ..Default::default() };
        let words = card_words(&dated);
        assert!(!words.iter().any(|w| w.contains("EPISODE 0") || w.contains('0') && w.to_uppercase().contains("EPISODE")), "{words:?}");
        assert!(words.contains(&kind), "the kicker says {kind:?}: {words:?}");
        assert!(words.contains(&plx_ui::fmt::pretty_date("2016-04-21", 0)), "the date row is still there: {words:?}");
        let undated = Episode { title: "Pilot".into(), ..Default::default() };
        assert!(card_words(&undated).contains(&kind), "an episode with no number and no date says what it is");
        let numbered = Episode { index: 3, title: "Pilot".into(), ..Default::default() };
        assert!(card_words(&numbered).contains(&plx_platform::i18n::msg::browse_detail_episode_number(3)), "a numbered one keeps EPISODE N");
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

    /// **A released strip cell lands exactly at rest by the shared rule** (`Spring::land`, the one
    /// `CardRow::update` uses for a shelf's released tile). `Spring::step` alone parks a released
    /// spring one ulp off its target forever, so without the landing the cell never reads as at rest
    /// (a pop factor of 1e-6 instead of exactly 0.0, the focus program instead of the resting split).
    /// The reference is the same spring stepped alone: until the landing the cell is bit-for-bit that
    /// spring (same constants, same targets), and the landing moves it by no more than the epsilon.
    #[test]
    fn a_released_strip_cell_lands_exactly_at_rest_by_the_shared_rule() {
        let dt = 1.0 / 60.0;
        let mut cells = Cells::new();
        let mut reference = plx_ui::Spring::at(1.0);
        let step = |cells: &mut Cells, reference: &mut plx_ui::Spring, focused: Option<usize>| {
            cells.step(focused, None, dt);
            reference.step(if focused == Some(2) { theme::EP_CARD_FOCUS_SCALE } else { 1.0 }, 300.0, dt);
        };
        for _ in 0..90 {
            step(&mut cells, &mut reference, Some(2));
        }
        assert_eq!(cells.scale(2), reference.pos, "the focused cell is the plain spring");
        let mut landed = false;
        for frame in 1..=600 {
            step(&mut cells, &mut reference, None);
            let shown = cells.scale(2);
            if shown == reference.pos {
                continue;
            }
            let epsilon = plx_ui::LAND_REL * (1.0 + reference.pos.max(1.0));
            assert!(
                (shown - reference.pos).abs() <= epsilon,
                "frame {frame}: the cell moved {} in its landing, against the epsilon {epsilon}",
                (shown - reference.pos).abs()
            );
            assert_eq!(shown, 1.0, "it lands exactly at rest");
            landed = true;
            break;
        }
        assert!(landed, "the released cell never landed: it is parked at {} (target 1.0)", cells.scale(2));
        assert_eq!(widgets::pop_factor(cells.scale(2), theme::EP_CARD_FOCUS_SCALE), 0.0);
    }

    /// The width of the still's card as `draw_cell` placed it at spring position `scale`: the widest
    /// rect within a pop of the cell's width (the art scrim, drawn over the card's own rect).
    fn drawn_card_width(scale: f32) -> f32 {
        cell_census(scale, &TextLift::new())
            .iter()
            .map(|(_, r)| r.w)
            .filter(|w| *w >= W * 0.99 && *w <= W * 1.2)
            .fold(0.0, f32::max)
    }

    /// **The still is drawn at the scale its spring says, at every scale**: a released cell's card
    /// shrinks continuously into its resting rect. The pop factor is exactly 0.0 at rest
    /// ([`widgets::pop_factor`]), so no threshold is needed to tell a resting cell from a popped one.
    #[test]
    fn a_released_still_is_drawn_at_its_spring_scale_all_the_way_to_rest() {
        let _g = plx_base::testlock::serial();
        for scale in [1.0, 1.0004, 1.0009, 1.0011, 1.003, 1.02, theme::EP_CARD_FOCUS_SCALE] {
            let w = drawn_card_width(scale);
            assert!(
                (w - W * scale).abs() < 0.001,
                "scale {scale}: the card is drawn {w} wide, its spring says {}",
                W * scale
            );
        }
    }

    #[test]
    fn strip_x_matches_the_shared_shelf_formula() {
        for i in 0..20 {
            assert_eq!(
                strip_x(i),
                plx_ui::cards::tile_rect(
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

    /// Q6: an empty summary is ABSENT from the meta stack, so there is one gap wherever two blocks
    /// meet: the date stands one MD under the title (not two), and a title-only platter ends at the
    /// title with no trailing MD. A summary, when there is one, keeps its MD on both sides.
    #[test]
    fn the_meta_block_has_one_gap_between_blocks_and_none_trailing() {
        let measure = plx_ui::fixture::FixtureMeasure;
        let md = theme::space::MD;
        let full = Episode { title: "Pilot".into(), summary: "word ".repeat(30), aired: "2020-01-01".into(), ..Default::default() };
        let (date_y, summary_y, bottom) = meta_layout(&full, &measure);
        let title_h = summary_y - TITLE_DY - md;
        assert!(title_h >= TITLE_LEAD);
        assert!(date_y > summary_y + md, "a summary keeps its MD before the date");
        assert!((bottom - (date_y + theme::size::MICRO as f32 + META_BOTTOM_PAD)).abs() < 0.01);
        // (a) no summary, a date: one MD under the title.
        let dated = Episode { summary: String::new(), ..full.clone() };
        let (date_y, _, bottom) = meta_layout(&dated, &measure);
        assert!((date_y - (TITLE_DY + title_h + md)).abs() < 0.01, "the date stands one MD under the title: {date_y}");
        assert!((bottom - (date_y + theme::size::MICRO as f32 + META_BOTTOM_PAD)).abs() < 0.01);
    }

    /// Q6 (b): a title-only platter (no summary, no date, no rating) ends at the title with no trailing MD.
    #[test]
    fn a_title_only_platter_has_no_trailing_md() {
        let measure = plx_ui::fixture::FixtureMeasure;
        let full = Episode { title: "Pilot".into(), summary: "word ".repeat(30), ..Default::default() };
        let title_h = meta_layout(&full, &measure).1 - TITLE_DY - theme::space::MD;
        let title = Episode { title: "Pilot".into(), ..Default::default() };
        let (_, _, bottom) = meta_layout(&title, &measure);
        assert!((bottom - (TITLE_DY + title_h + META_BOTTOM_PAD)).abs() < 0.01, "no trailing MD: {bottom}");
    }

    /// A rating chip with no air date, no summary: the date row is present for the chip, so the
    /// platter (which ends [`PLATTER_PAD`] under the last row `meta_layout` measured) reaches it.
    #[test]
    fn a_rating_chip_without_a_date_stays_inside_the_platter() {
        let _g = plx_base::testlock::serial();
        let measure = plx_ui::fixture::FixtureMeasure;
        let ep = Episode { title: "Pilot".into(), rating: "TV-14".into(), ..Default::default() };
        let (_, _, bottom) = meta_layout(&ep, &measure);
        let text_top = H + META_TOP;
        let platter_bottom = text_top + (bottom - META_BOTTOM_PAD) + PLATTER_PAD;
        let d = Detail::default();
        let runs = plx_ui::draw_census::capture_runs(|| {
            draw_cell(Painter::recording(), &d, 0, &ep, None, 1.0, 1.0, &TextLift::new(), kicker_cap_top(), &measure)
        });
        let chip = runs.iter().find(|r| r.text == "TV-14").expect("the rating chip is drawn");
        assert!(chip.at.y + theme::size::MICRO as f32 <= platter_bottom + 0.5, "chip run at {} + MICRO past platter bottom {platter_bottom}", chip.at.y);
    }
}

#[cfg(test)]
mod stack_tests {
    use super::*;

    /// The reserved label block is the meta stack at its tallest, to the bit.
    #[test]
    fn the_reserved_block_is_the_meta_stack_at_its_tallest() {
        let md = theme::space::MD;
        let old = TITLE_DY + 2.0 * TITLE_LEAD + md + SUMMARY_MAX_LINES as f32 * SUMMARY_LEAD + md + theme::size::MICRO as f32 + META_BOTTOM_PAD;
        assert_eq!(reserved_text_h(), old);
    }

    /// With an empty summary the fallback `summary_y` is where a summary would stand: one MD under the title.
    #[test]
    fn a_missing_summary_would_stand_one_md_under_the_title() {
        let measure = plx_ui::fixture::FixtureMeasure;
        let full = Episode { title: "Pilot".into(), summary: "word ".repeat(30), aired: "2020-01-01".into(), ..Default::default() };
        let none = Episode { summary: String::new(), ..full.clone() };
        assert_eq!(meta_layout(&none, &measure).1, TITLE_DY + (meta_layout(&full, &measure).1 - TITLE_DY - theme::space::MD) + theme::space::MD);
    }
}
