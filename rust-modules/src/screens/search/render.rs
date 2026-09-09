//! Paint from the instance and its retained frame views. No live Search/roster/focus reads.
use super::*;
use crate::search::scope::{ScopeSource, SourceScopeSnapshot};
use crate::ui::card_row::{self, TileLabel};
use crate::ui::consts::{MARGIN_X, SCR_H, SCR_W};
use crate::ui::label::{HAlign, Label, VAlign};
use crate::ui::widgets::{Art, Button, StatusKind, StatusOverlay};
use crate::ui::{theme, Env, Painter, View};
use std::ffi::{CStr, CString};
use std::os::raw::c_int;

const CARET_W: f32 = 5.0;
const CARET_GAP: f32 = 8.0;
const GHOST_GAP: f32 = theme::size::BODY as f32;
const COUNT_GAP: f32 = theme::space::SM;
const SOURCE_PAD: f32 = theme::space::XS;

#[derive(Default)]
pub(super) struct Resources {
    query: String,
    caret: usize,
    run: CString,
    head: CString,
    run_w: f32,
    head_w: f32,
    scope: Option<SourceScopeSnapshot>,
    scope_line: Option<CString>,
    recents: Option<crate::search::recents::RecentsSnapshot>,
    recent_runs: Vec<CString>,
    titles: [CString; 5],
    count_keys: [Option<(Kind, usize)>; 5],
    counts: [CString; 5],
    owner: CString,
}

impl Resources {
    pub(super) fn prepare<H: SearchLike>(
        &mut self,
        query: &str,
        caret: usize,
        owner: &str,
        cx: &Cx<'_, H>,
    ) {
        let q = query.split('\0').next().unwrap_or("");
        let mut caret = caret.min(q.len());
        while !q.is_char_boundary(caret) {
            caret -= 1;
        }
        if self.query != q || self.caret != caret || self.run.is_empty() {
            self.query = q.into();
            self.caret = caret;
            self.run = if q.trim().is_empty() {
                c"Search your library".into()
            } else {
                cstring(q)
            };
            let (_, head) = run_and_head(q, caret);
            self.head = cstring(head);
            self.run_w = cx.measure.width(&self.run, theme::size::HERO, true);
            self.head_w = if q.trim().is_empty() {
                0.0
            } else {
                cx.measure.width(&self.head, theme::size::HERO, true)
            };
        }
        let scope = H::search(cx).scope();
        if self
            .scope
            .as_ref()
            .is_none_or(|old| !old.same_publication(scope))
        {
            self.scope_line = scope_text(scope.sources()).map(|line| {
                cstring(&elide(
                    &line,
                    layout::FIELD.w,
                    theme::size::CAPTION,
                    false,
                    cx.measure,
                ))
            });
            self.scope = Some(scope.clone());
        }
        let recents = H::search(cx).recents();
        if self
            .recents
            .as_ref()
            .is_none_or(|old| !old.same_publication(recents))
        {
            self.recent_runs = recents
                .terms()
                .iter()
                .map(|term| {
                    cstring(&elide(
                        term,
                        820.0 - 2.0 * crate::ui::table::CONTENT_X,
                        theme::size::HEADLINE,
                        true,
                        cx.measure,
                    ))
                })
                .collect();
            self.recents = Some(recents.clone());
        }
        for (i, kind) in crate::search::KINDS.iter().enumerate() {
            if self.titles[i].is_empty() {
                self.titles[i] = cstring(kind.title());
            }
            let key = H::search(cx)
                .shelves()
                .get(i)
                .map(|shelf| (shelf.kind, shelf.items.len()));
            if self.count_keys[i] != key {
                self.count_keys[i] = key;
                self.counts[i] = key.map_or_else(CString::default, |(kind, n)| {
                    cstring(&format!("{n} {}", kind.count_word(n)))
                });
            }
        }
        if self.owner.to_bytes() != owner.as_bytes() {
            self.owner = cstring(owner);
        }
    }
}

pub(super) fn draw<H: SearchLike>(screen: &SearchScreen, f: &mut DrawFrame<'_, '_, H>) {
    crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
    let p = f.painter.alpha(f.page_alpha);
    screen.ground.draw(p, Rect::FULL);
    field(screen, f, p);
    let p = p.alpha(screen.fade.alpha());
    if !screen.recents.is_empty() {
        recents(screen, f, p);
    } else if screen.rows.is_empty() {
        empty(screen, f, p);
    }
    for (i, row) in screen.rows.iter().enumerate() {
        let (kinds, n) = screen.kinds();
        let top = layout::top(&kinds[..n], i, |j| screen.rows[j].motion.band_expand())
            - screen.scroll.pos;
        if !crate::ui::on_axis(
            top,
            layout::block_h(row.kind, row.motion.band_expand()),
            SCR_H,
            0.0,
        ) {
            continue;
        }
        let slot = layout::ordinal(row.kind) as usize;
        let title = &screen.render.titles[slot];
        let cap = f.cx.measure.cap_h(theme::size::HEADLINE);
        let source = if screen.owner_row == Some(i) && screen.owner_alpha.pos > OWNER_FLOOR {
            screen.render.owner.as_c_str()
        } else {
            c""
        };
        heading_flow(
            title,
            &screen.render.counts[slot],
            source,
            |run, dx, size, bold, ink, faded| {
                let mut label = Label::new(run.as_ptr(), size, ink).v(VAlign::Baseline);
                if bold != 0 {
                    label = label.bold();
                }
                label.draw(
                    if faded {
                        p.alpha(screen.owner_alpha.pos.clamp(0.0, 1.0))
                    } else {
                        p
                    },
                    Rect::new(MARGIN_X + dx, top - row.motion.lift(), 0.0, cap),
                );
                f.cx.measure.width(run, size, bold != 0)
            },
        );
        let focused =
            f.cx.focus
                .current
                .and_then(|key| row.elems.iter().position(|elem| *elem == key.elem));
        for col in 0..row.elems.len() {
            if focused != Some(col) {
                tile(screen, i, col, false, f, p);
            }
        }
        if let Some(col) = focused {
            tile(screen, i, col, true, f, p);
        }
    }
}

fn field<H: SearchLike>(screen: &SearchScreen, f: &mut DrawFrame<'_, '_, H>, p: Painter) {
    let data = &screen.render;
    let rect = Rect::new(
        layout::FIELD.x,
        layout::FIELD.y - screen.scroll.pos,
        layout::FIELD.w,
        layout::FIELD.h,
    );
    let blank = data.query.trim().is_empty();
    let hot = screen.hot.pos;
    let ink = if blank {
        theme::cross(theme::TEXT_TERTIARY, theme::TEXT_SECONDARY, hot)
    } else {
        theme::cross(theme::TEXT_SECONDARY, ink_target(screen.editing), hot)
    };
    let (run_dx, caret_dx) = run_layout(
        data.run_w,
        run_caret_w(blank, data.head_w),
        rect.w,
        screen.editing,
    );
    let (cap_top, cap_base) = crate::text::text_cap_band(theme::size::HERO, 1);
    let pad = descent_pad(
        rect.h,
        crate::text::text_height(theme::size::HERO, 1),
        cap_top,
        cap_base,
    );
    {
        let _clip = f.clip(p, Rect::new(rect.x, rect.y, rect.w, rect.h + pad));
        Label::new(data.run.as_ptr(), theme::size::HERO, ink)
            .bold()
            .draw(p, Rect::new(rect.x + run_dx, rect.y, rect.w, rect.h));
        let text_y = crate::text::text_vcenter_y(theme::size::HERO, 1, rect.cy());
        if caret_shown(screen.editing, screen.blink_us < super::BLINK_US) {
            p.rect(
                Rect::new(
                    rect.x + caret_dx,
                    text_y + cap_top,
                    CARET_W,
                    cap_base - cap_top,
                ),
                0.0,
                theme::TEXT_PRIMARY,
                theme::TEXT_PRIMARY,
                0.0,
            );
        }
        if ghost_shown(&data.query) {
            let y = crate::text::baseline_y(theme::size::BODY, 0, theme::size::HERO, 1, text_y);
            p.text(
                c"one more character".as_ptr(),
                rect.x + caret_dx + CARET_W + GHOST_GAP,
                y,
                theme::size::BODY,
                theme::cross(theme::TEXT_TERTIARY, theme::TEXT_SECONDARY, hot),
                0,
                0,
            );
        }
    }
    if let Some(line) = &data.scope_line {
        Label::new(line.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY).draw(
            p,
            Rect::new(
                rect.x,
                layout::SCOPE_Y - screen.scroll.pos,
                rect.w,
                layout::SCOPE_H,
            ),
        );
    }
    stop(screen, FIELD, ElemKind::Bare, f, p);
}

fn recents<H: SearchLike>(screen: &SearchScreen, f: &mut DrawFrame<'_, '_, H>, p: Painter) {
    let env = Env::inert();
    Label::new(
        c"RECENT SEARCHES".as_ptr(),
        theme::size::CAPTION,
        theme::TEXT_TERTIARY,
    )
    .draw(
        p,
        Rect::new(
            MARGIN_X + crate::ui::table::CONTENT_X,
            layout::CONTENT_TOP - screen.scroll.pos,
            0.0,
            crate::ui::table::HDR_H,
        ),
    );
    let shown = screen.recents.len().min(layout::RECENT_CAP);
    for (index, elem) in screen.recents.iter().take(shown).enumerate() {
        let Some(key) = screen.keys.iter().find(|key| key.elem == *elem) else {
            continue;
        };
        let Identity::Recent(_) = &key.identity else {
            continue;
        };
        let rect = layout::recent(index, screen.scroll.pos);
        let focused = f.cx.focus.current == Some(screen.key(*elem));
        if focused {
            p.rrect(
                Rect::new(
                    rect.x + crate::ui::table::SIDE,
                    rect.y + crate::ui::table::PILL_INSET,
                    rect.w - 2.0 * crate::ui::table::SIDE,
                    rect.h - 2.0 * crate::ui::table::PILL_INSET,
                ),
                crate::ui::table::PILL_RAD,
                crate::ui::table::PILL_RAD,
                theme::ACCENT,
            );
        }
        let Some(run) = screen.render.recent_runs.get(index) else {
            continue;
        };
        Label::new(
            run.as_ptr(),
            theme::size::HEADLINE,
            if focused {
                theme::ACCENT_INK
            } else {
                theme::TEXT_PRIMARY
            },
        )
        .bold()
        .draw(
            p,
            Rect::new(rect.x + crate::ui::table::CONTENT_X, rect.y, 0.0, rect.h),
        );
        stop(screen, *elem, ElemKind::Bare, f, p);
    }
    let rect = layout::clear(shown, screen.scroll.pos, f.cx.measure);
    Button::new(c"Clear recent searches".as_ptr(), theme::size::BODY, rect)
        .focused(f.cx.focus.current == Some(screen.key(CLEAR)))
        .draw(&env, p);
    stop(screen, CLEAR, ElemKind::Control, f, p);
}

fn empty<H: SearchLike>(screen: &SearchScreen, f: &DrawFrame<'_, '_, H>, p: Painter) {
    let state = H::search(f.cx).state();
    if screen.draft.pending() {
        return;
    }
    let Some(empty) = empty_state(state, false) else {
        return;
    };
    let mut rect = layout::empty_band(screen.editing);
    rect.y -= screen.scroll.pos;
    if empty == EmptyState::Fault {
        StatusOverlay::new(rect, c"Search didn’t reach the server", StatusKind::Failed)
            .reason(c"Your libraries are fine — try again in a moment.")
            .draw(&Env::inert(), p);
        return;
    }
    let header = header_of(empty);
    let statement = if empty == EmptyState::NoResults {
        let shell =
            f.cx.measure
                .width(c"No results for “”", theme::size::TITLE, true);
        no_results_line(&elide(
            screen.draft.query().trim(),
            1200.0 - shell,
            theme::size::TITLE,
            true,
            f.cx.measure,
        ))
    } else {
        "Nothing searched yet".into()
    };
    let statement = cstring(&statement);
    let hh = f.cx.measure.cap_h(theme::size::CAPTION);
    let sh = f.cx.measure.cap_h(theme::size::TITLE);
    let top = rect.y + (rect.h - hh - sh - theme::space::MD) * 0.5;
    Label::new(header.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
        .h(HAlign::Center)
        .v(VAlign::CapTop)
        .draw(p, Rect::new(rect.x, top, rect.w, 0.0));
    Label::new(statement.as_ptr(), theme::size::TITLE, theme::TEXT_HEADING)
        .bold()
        .h(HAlign::Center)
        .v(VAlign::CapTop)
        .draw(
            p,
            Rect::new(rect.x, top + hh + theme::space::MD, rect.w, 0.0),
        );
}

pub(super) fn tile<H: SearchLike>(
    screen: &SearchScreen,
    row: usize,
    col: usize,
    focused: bool,
    f: &mut DrawFrame<'_, '_, H>,
    p: Painter,
) {
    let view = H::search(f.cx);
    let Some(shelf) = view.shelves().get(row) else {
        return;
    };
    let Some(item) = shelf.items.get(col) else {
        return;
    };
    let model = &screen.rows[row];
    let style = layout::style(model.kind);
    let rest = screen.row_rect(row, col, At::Drawn);
    let pop = model.motion.scale(col);
    let press = if focused && f.press.scale > 0.0 {
        f.press.scale
    } else {
        1.0
    };
    let scale = pop * press;
    let rect = rest.scaled(scale);
    if !crate::ui::on_axis(rect.x, rect.w, SCR_W, 32.0) {
        return;
    }
    let art = match (model.kind, item) {
        (Kind::Episode, Item::Media(media)) => Art::Still(Some(media)),
        (_, Item::Media(media)) => Art::Poster(Some(media)),
        (Kind::Person, Item::Tag(tag)) => Art::Person {
            sid: tag.sid,
            key: &tag.thumb,
            res: (300, 300),
        },
        (_, Item::Tag(tag)) if tag.thumb.is_empty() => Art::Poster(None),
        (_, Item::Tag(tag)) => Art::Thumb {
            sid: tag.sid,
            key: &tag.thumb,
            res: (250, 375),
        },
    };
    let resume = match item {
        Item::Media(media) if model.kind != Kind::Episode => media.resume_frac(),
        _ => None,
    };
    if focused {
        let sid = match item {
            Item::Media(media) => media.sid,
            Item::Tag(tag) => tag.sid,
        };
        let handle = view
            .scope()
            .sources()
            .iter()
            .find(|source| source.sid == sid && !source.owned)
            .map_or("", |source| source.handle.as_str());
        let fact = subtitle(model.kind, item, handle);
        card_row::draw_focused(
            p,
            art,
            rect,
            scale,
            &style,
            resume,
            &TileLabel::titled(item.title(), &fact).revealed(model.motion.band_reveal()),
        );
    } else {
        card_row::draw_tile(p, art, rect, scale, &style, resume);
    }
    if let (Kind::Episode, Item::Media(media)) = (model.kind, item) {
        crate::ui::widgets::still_overlay(p, media, rect, style.tile_radius(rect, scale), false);
    }
    stop(
        screen,
        model.elems[col],
        if matches!(model.kind, Kind::Movie | Kind::Show | Kind::Episode) {
            ElemKind::Card
        } else {
            ElemKind::Bare
        },
        f,
        p,
    );
}

fn stop<H: SearchLike>(
    screen: &SearchScreen,
    elem: u32,
    kind: ElemKind,
    f: &mut DrawFrame<'_, '_, H>,
    p: Painter,
) {
    if crate::gfx::blur_source_pass() {
        return;
    }
    use crate::ui::screen::{Activate, Hover, Stop};
    let Some(placed) = <SearchScreen as Focusable<H>>::place(screen, &elem, f.cx, At::Drawn) else {
        return;
    };
    f.stop(
        p,
        Stop {
            key: screen.key(elem),
            rect: placed.rect,
            rest_rect: placed.rest_rect,
            clip: placed.clip,
            hover: Hover::Focus,
            activate: if kind == ElemKind::Bare {
                Activate::Immediate
            } else {
                Activate::Press
            },
        },
    );
}

fn cstring(text: &str) -> CString {
    CString::new(text).unwrap_or_default()
}
fn elide(
    text: &str,
    width: f32,
    size: i32,
    bold: bool,
    measure: &dyn crate::ui::machine::Measure,
) -> String {
    crate::text::elide_by(text, width, false, |s| {
        measure.width(&cstring(s), size, bold)
    })
}
fn run_layout(run: f32, caret: f32, width: f32, editing: bool) -> (f32, f32) {
    let available = (width - if editing { CARET_W + CARET_GAP } else { 0.0 }).max(0.0);
    let overflow = (caret - available).max(0.0).min((run - available).max(0.0));
    (
        -overflow,
        (caret - overflow + CARET_GAP).min((width - CARET_W).max(0.0)),
    )
}

fn run_and_head(q: &str, caret: usize) -> (&str, &str) {
    let run = &q[..q.find('\0').unwrap_or(q.len())];
    let mut caret = caret.min(run.len());
    while caret > 0 && !run.is_char_boundary(caret) {
        caret -= 1;
    }
    (run, &run[..caret])
}

fn run_caret_w(blank: bool, head_w: f32) -> f32 {
    if blank {
        0.0
    } else {
        head_w
    }
}

fn ghost_shown(q: &str) -> bool {
    let n = q.trim().chars().count();
    n > 0 && n + 1 == crate::search::MIN_QUERY
}

fn caret_shown(editing: bool, phase_on: bool) -> bool {
    editing && phase_on
}

fn descent_pad(box_h: f32, full_h: f32, cap_top: f32, cap_base: f32) -> f32 {
    (full_h - box_h * 0.5 - (cap_top + cap_base) * 0.5).max(0.0)
}

fn ink_target(editing: bool) -> [f32; 4] {
    if editing {
        theme::FIELD_EDITING_INK
    } else {
        theme::FIELD_WAITING_INK
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmptyState {
    NotYet,
    NoResults,
    Fault,
}

fn empty_state(state: crate::search::State, has_shelves: bool) -> Option<EmptyState> {
    use crate::search::State;
    match state {
        State::Idle => Some(EmptyState::NotYet),
        State::Searching => None,
        State::Ready => (!has_shelves).then_some(EmptyState::NoResults),
        State::Failed => Some(EmptyState::Fault),
    }
}

fn header_of(state: EmptyState) -> &'static CStr {
    match state {
        EmptyState::NoResults => c"SEARCH RESULTS",
        _ => c"RECENT SEARCHES",
    }
}

fn no_results_line(q: &str) -> String {
    format!("No results for \u{201C}{q}\u{201D}")
}

fn heading_flow(
    title: &CStr,
    count: &CStr,
    source: &CStr,
    mut run: impl FnMut(&CStr, f32, c_int, c_int, [f32; 4], bool) -> f32,
) -> f32 {
    let mut dx = run(
        title,
        0.0,
        theme::size::HEADLINE,
        1,
        theme::TEXT_HEADING,
        false,
    );
    if !count.is_empty() {
        dx += COUNT_GAP;
        dx += run(count, dx, theme::size::BODY, 0, theme::TEXT_TERTIARY, false);
    }
    if source.is_empty() {
        return dx;
    }
    dx += SOURCE_PAD;
    dx += run(c"·", dx, theme::size::BODY, 0, theme::TEXT_SEPARATOR, true);
    dx += SOURCE_PAD;
    dx += run(source, dx, theme::size::BODY, 0, theme::TEXT_TERTIARY, true);
    dx
}

fn subtitle(kind: Kind, item: &Item, handle: &str) -> String {
    let mut parts = Vec::new();
    match item {
        Item::Media(media) if kind == Kind::Episode => {
            let date = crate::ui::fmt::pretty_date(&media.aired, media.year as i64);
            if !date.is_empty() {
                parts.push(date);
            }
        }
        Item::Media(media) if media.year > 0 => parts.push(media.year.to_string()),
        Item::Tag(tag) if kind == Kind::Collection && tag.count > 0 => parts.push(format!(
            "{} item{}",
            tag.count,
            if tag.count == 1 { "" } else { "s" }
        )),
        _ => {}
    }
    if !handle.is_empty() {
        parts.push(handle.into());
    }
    parts.join(" · ")
}

fn source_label(source: &ScopeSource) -> String {
    if source.owned {
        return if source.name.is_empty() {
            "your server".into()
        } else {
            source.name.clone()
        };
    }
    let named: Vec<_> = source
        .libraries
        .iter()
        .filter(|name| !name.is_empty())
        .collect();
    if named.len() == 1 {
        named[0].clone()
    } else if !source.handle.is_empty() {
        source.handle.clone()
    } else {
        "a shared server".into()
    }
}
fn join(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        _ => format!(
            "{} and {}",
            names[..names.len() - 1].join(", "),
            names.last().unwrap()
        ),
    }
}
fn name_set(sources: &[&ScopeSource]) -> String {
    let shares: Vec<_> = sources.iter().filter(|source| !source.owned).collect();
    if shares.len() <= 2 {
        return join(
            &sources
                .iter()
                .map(|source| source_label(source))
                .collect::<Vec<_>>(),
        );
    }
    let libraries = shares
        .iter()
        .map(|source| {
            source
                .libraries
                .iter()
                .filter(|name| !name.is_empty())
                .count()
        })
        .sum::<usize>();
    let mut names: Vec<_> = sources
        .iter()
        .filter(|source| source.owned)
        .map(|source| source_label(source))
        .collect();
    names.push(if libraries > 0 {
        format!("{libraries} shared libraries")
    } else {
        format!("{} shared sources", shares.len())
    });
    join(&names)
}
fn scope_text(sources: &[ScopeSource]) -> Option<String> {
    if sources.is_empty() {
        return None;
    }
    let (live, down): (Vec<_>, Vec<_>) = sources.iter().partition(|source| source.live);
    if down.is_empty() {
        let mut line = format!("Searching {}", name_set(&live));
        let mut shares = live
            .iter()
            .filter(|source| !source.owned && !source.handle.is_empty());
        if let (2, Some(source), None) = (live.len(), shares.next(), shares.next()) {
            if source_label(source) != source.handle {
                line.push_str(" · ");
                line.push_str(&source.handle);
            }
        }
        Some(line)
    } else if live.is_empty() {
        Some(format!("{} unreachable", name_set(&down)))
    } else {
        Some(format!(
            "{} unreachable · results from {} only",
            name_set(&down),
            name_set(&live)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{Item, Kind, State, TagHit};

    fn own(name: &str) -> ScopeSource {
        ScopeSource {
            sid: crate::plex::ServerId::UNSET,
            name: name.into(),
            libraries: Vec::new(),
            handle: String::new(),
            owned: true,
            live: true,
        }
    }
    fn share(lib: &str, handle: &str) -> ScopeSource {
        ScopeSource {
            sid: crate::plex::ServerId::UNSET,
            name: "a-hostname".into(),
            libraries: vec![lib.into()],
            handle: handle.into(),
            owned: false,
            live: true,
        }
    }
    fn down(mut source: ScopeSource) -> ScopeSource {
        source.live = false;
        source
    }
    fn cs(s: &str) -> CString {
        CString::new(s).expect("test literal")
    }

    #[test]
    fn the_scope_block_is_one_line_because_the_minimum_hint_lives_in_the_field() {
        assert_eq!(layout::SCOPE_H, theme::size::CAPTION as f32 * 1.35);
        assert!(ghost_shown("a"));
        assert!(!ghost_shown(""));
        assert!(!ghost_shown("ab"));
    }

    #[test]
    fn ink_target_picks_pure_white_off_editing_and_the_editing_stop_on() {
        assert_eq!(ink_target(false), theme::FIELD_WAITING_INK);
        assert_eq!(ink_target(true), theme::FIELD_EDITING_INK);
        assert_ne!(theme::FIELD_WAITING_INK, theme::FIELD_EDITING_INK);
    }

    #[test]
    fn focus_is_carried_by_a_wide_ink_step_and_nothing_else() {
        let idle = theme::cross(theme::TEXT_SECONDARY, ink_target(false), 0.0);
        assert_eq!(idle, theme::TEXT_SECONDARY);
        let waiting = theme::cross(theme::TEXT_SECONDARY, ink_target(false), 1.0);
        let editing = theme::cross(theme::TEXT_SECONDARY, ink_target(true), 1.0);
        assert_eq!(waiting, theme::FIELD_WAITING_INK);
        assert_eq!(editing, theme::FIELD_EDITING_INK);
        let luma = |c: [f32; 4]| (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]) * c[3];
        assert!(luma(waiting) - luma(idle) > 0.15);
        assert!(luma(editing) - luma(idle) > 0.15);
        assert!(luma(waiting) > luma(editing));
    }

    #[test]
    fn the_caret_is_visible_only_during_the_on_phase_of_an_editing_field() {
        assert!(caret_shown(true, true));
        assert!(!caret_shown(true, false));
        assert!(!caret_shown(false, true));
        assert!(!caret_shown(false, false));
    }

    #[test]
    fn a_run_that_fits_starts_at_the_edge_and_the_caret_trails_it() {
        let (run, caret) = run_layout(100.0, 100.0, 756.0, true);
        assert_eq!(run, 0.0);
        assert_eq!(caret, 100.0 + CARET_GAP);
    }

    #[test]
    fn an_overlong_run_slides_left_so_the_caret_lands_on_the_boxs_right_edge() {
        let box_w = 756.0;
        let (run, caret) = run_layout(900.0, 900.0, box_w, true);
        assert!(run < 0.0);
        assert_eq!(run + 900.0, box_w - CARET_GAP - CARET_W);
        assert_eq!(caret + CARET_W, box_w);
    }

    #[test]
    fn with_no_caret_the_run_uses_the_whole_box() {
        let box_w = 756.0;
        let (run, _) = run_layout(900.0, 900.0, box_w, false);
        assert_eq!(run + 900.0, box_w);
    }

    #[test]
    fn an_empty_run_keeps_the_designs_caret_gap() {
        let (run, caret) = run_layout(0.0, 0.0, 756.0, true);
        assert_eq!(run, 0.0);
        assert_eq!(caret, CARET_GAP);
    }

    #[test]
    fn the_caret_follows_the_insertion_point_whether_or_not_the_run_has_slid() {
        for run_w in [0.0, 1.0, 400.0, 753.0, 900.0, 5000.0] {
            let (run, caret) = run_layout(run_w, run_w, 756.0, true);
            assert_eq!(caret, run + run_w + CARET_GAP, "run_w={run_w}");
        }
    }

    #[test]
    fn the_run_scrolls_to_the_caret_rather_than_to_the_tail() {
        let box_w = 756.0;
        let avail = box_w - CARET_GAP - CARET_W;
        assert_eq!(run_layout(2000.0, 0.0, box_w, true), (0.0, CARET_GAP));
        assert_eq!(
            run_layout(2000.0, avail - 10.0, box_w, true),
            (0.0, avail - 10.0 + CARET_GAP)
        );
        let (run, caret) = run_layout(2000.0, 1000.0, box_w, true);
        assert_eq!(run, -(1000.0 - avail));
        assert_eq!(caret + CARET_W, box_w);
        for c in [0.0, 1.0, 500.0, 1999.0, 2000.0] {
            let (_, caret) = run_layout(2000.0, c, box_w, true);
            assert!(
                (0.0..=box_w - CARET_W).contains(&caret),
                "caret at {c} drew at {caret}"
            );
        }
    }

    #[test]
    fn the_head_is_the_text_before_the_insertion_point() {
        assert_eq!(run_and_head("wallace", 0), ("wallace", ""));
        assert_eq!(run_and_head("wallace", 3), ("wallace", "wal"));
        assert_eq!(run_and_head("wallace", 7), ("wallace", "wallace"));
        assert_eq!(run_and_head("wallace ", 8), ("wallace ", "wallace "));
        assert_eq!(run_and_head("", 0), ("", ""));
    }

    #[test]
    fn a_caret_inside_a_multi_byte_character_never_splits_it() {
        for q in ["суббота", "千と千尋", "Amélie", "🎬🎬", "aé千🎬z"] {
            for c in 0..=q.len() + 4 {
                let (run, head) = run_and_head(q, c);
                assert_eq!(run, q);
                assert!(q.starts_with(head), "the head must be a prefix: {head:?}");
                assert!(head.len() <= c.min(q.len()));
                assert!(c.min(q.len()) - head.len() < 4);
            }
        }
    }

    #[test]
    fn a_nul_cuts_the_run_and_the_caret_lands_inside_what_is_left() {
        assert_eq!(run_and_head("wal\0lace", 3), ("wal", "wal"));
        assert_eq!(run_and_head("wal\0lace", 8), ("wal", "wal"));
        assert_eq!(run_and_head("\0wallace", 4), ("", ""));
        assert_eq!(run_and_head("су\0ббота", 99), ("су", "су"));
    }

    #[test]
    fn a_box_too_narrow_for_the_caret_does_not_go_negative() {
        assert_eq!(run_layout(0.0, 0.0, 2.0, true), (0.0, 0.0));
    }

    #[test]
    fn a_blank_field_puts_the_caret_at_the_field_start_not_after_the_placeholder() {
        assert_eq!(run_caret_w(true, 999.0), 0.0);
        assert_eq!(run_caret_w(false, 42.0), 42.0);
        let (run, caret) = run_layout(480.0, run_caret_w(true, 0.0), 756.0, true);
        assert_eq!(run, 0.0);
        assert_eq!(caret, CARET_GAP);
        let (buggy_run, buggy_caret) = run_layout(480.0, 480.0, 756.0, true);
        assert_eq!(buggy_run, 0.0);
        assert_eq!(buggy_caret, 480.0 + CARET_GAP);
        assert!(caret < buggy_caret);
    }

    #[test]
    fn the_clip_reserves_only_the_descent_centering_does_not_already_clear() {
        assert_eq!(descent_pad(80.0, 87.0, 17.0, 70.0), 3.5);
        assert_eq!(descent_pad(80.0, 40.0, 17.0, 20.0), 0.0);
        assert_eq!(descent_pad(80.0, 10.0, 17.0, 70.0), 0.0);
    }

    #[test]
    fn one_source_is_named_and_only_no_source_is_silent() {
        assert_eq!(
            scope_text(&[own("nas-home")]).unwrap(),
            "Searching nas-home"
        );
        assert_eq!(scope_text(&[own("")]).unwrap(), "Searching your server");
        assert_eq!(
            scope_text(&[share("Film Club", "friend")]).unwrap(),
            "Searching Film Club"
        );
        assert_eq!(
            scope_text(&[down(own("nas-home"))]).unwrap(),
            "nas-home unreachable"
        );
        assert_eq!(scope_text(&[]), None);
    }

    #[test]
    fn two_live_sources_name_both_and_attribute_the_share() {
        assert_eq!(
            scope_text(&[own("nas-home"), share("Film Club", "friend")]).unwrap(),
            "Searching nas-home and Film Club · friend"
        );
    }

    #[test]
    fn a_share_is_named_by_its_library_and_never_by_its_hostname() {
        let t = scope_text(&[own("nas-home"), share("Film Club", "friend")]).unwrap();
        assert!(t.contains("Film Club"));
        assert!(!t.contains("a-hostname"));
        assert!(t.contains("nas-home"));
        let mut two = share("Film Club", "friend");
        two.libraries = vec!["Film Club".into(), "Archive".into()];
        let t = scope_text(&[own("nas-home"), two]).unwrap();
        assert_eq!(t, "Searching nas-home and friend");
        assert_eq!(t.matches("friend").count(), 1);
    }

    #[test]
    fn many_shares_collapse_to_a_count_rather_than_becoming_a_list() {
        let mk = |lib: &str, handle: &str| share(lib, handle);
        let many = vec![
            own("nas-home"),
            mk("A", "ann"),
            mk("B", "bob"),
            mk("C", "cat"),
        ];
        assert_eq!(
            scope_text(&many).unwrap(),
            "Searching nas-home and 3 shared libraries"
        );
        let mut blank = vec![
            own("nas-home"),
            share("", "ann"),
            share("", "bob"),
            share("", "cat"),
        ];
        for source in blank.iter_mut().skip(1) {
            source.libraries.clear();
        }
        assert_eq!(
            scope_text(&blank).unwrap(),
            "Searching nas-home and 3 shared sources"
        );
    }

    #[test]
    fn a_share_with_no_handle_is_named_but_not_attributed() {
        assert_eq!(
            scope_text(&[own("nas-home"), share("Film Club", "")]).unwrap(),
            "Searching nas-home and Film Club"
        );
    }

    #[test]
    fn an_unreachable_source_names_itself_and_what_is_left() {
        assert_eq!(
            scope_text(&[own("nas-home"), down(share("Film Club", "friend"))]).unwrap(),
            "Film Club unreachable · results from nas-home only"
        );
    }

    #[test]
    fn your_own_server_going_quiet_reads_the_same_way() {
        assert_eq!(
            scope_text(&[down(own("nas-home")), share("Film Club", "friend")]).unwrap(),
            "nas-home unreachable · results from Film Club only"
        );
    }

    #[test]
    fn with_nothing_answering_there_is_no_results_from_clause_to_write() {
        assert_eq!(
            scope_text(&[down(own("nas-home")), down(share("Film Club", "friend"))]).unwrap(),
            "nas-home and Film Club unreachable"
        );
    }

    #[test]
    fn three_sources_list_plainly_and_attribute_none_of_them() {
        assert_eq!(
            scope_text(&[
                own("nas-home"),
                share("Film Club", "friend"),
                share("Archive", "pal")
            ])
            .unwrap(),
            "Searching nas-home, Film Club and Archive"
        );
    }

    #[test]
    fn two_shares_are_named_but_neither_is_attributed() {
        assert_eq!(
            scope_text(&[share("Film Club", "friend"), share("Archive", "pal")]).unwrap(),
            "Searching Film Club and Archive"
        );
    }

    #[test]
    fn an_unnamed_source_still_produces_a_sentence() {
        assert_eq!(
            scope_text(&[own(""), share("Film Club", "friend")]).unwrap(),
            "Searching your server and Film Club · friend"
        );
        assert_eq!(
            scope_text(&[own("nas-home"), share("", "")]).unwrap(),
            "Searching nas-home and a shared server"
        );
    }

    #[test]
    fn an_undescribed_roster_does_not_call_every_machine_yours() {
        assert_eq!(
            scope_text(&[own(""), share("", "")]).unwrap(),
            "Searching your server and a shared server"
        );
    }

    #[test]
    fn the_empty_state_preserves_state_and_region_copy() {
        assert_eq!(
            empty_state(State::Ready, false),
            Some(EmptyState::NoResults)
        );
        assert_eq!(empty_state(State::Failed, false), Some(EmptyState::Fault));
        assert_eq!(empty_state(State::Idle, false), Some(EmptyState::NotYet));
        assert_eq!(empty_state(State::Searching, false), None);
        assert_eq!(empty_state(State::Ready, true), None);
        assert_eq!(empty_state(State::Failed, true), Some(EmptyState::Fault));
        assert_eq!(empty_state(State::Idle, true), Some(EmptyState::NotYet));
        assert_eq!(header_of(EmptyState::NotYet), c"RECENT SEARCHES");
        assert_eq!(header_of(EmptyState::NoResults), c"SEARCH RESULTS");
    }

    #[test]
    fn the_statement_centres_in_the_space_the_user_can_actually_see() {
        let up = layout::empty_band(true);
        let down = layout::empty_band(false);
        assert_eq!(up.y, layout::CONTENT_TOP);
        assert_eq!(down.y, layout::CONTENT_TOP);
        assert_eq!(down.h - up.h, layout::KEYBOARD_H);
        assert!(up.y + up.h <= SCR_H - layout::KEYBOARD_H);
        assert_eq!(down.y + down.h, SCR_H);
        assert_eq!((up.x, up.w), (0.0, SCR_W));
    }

    #[test]
    fn the_query_is_quoted_back_typographically_and_the_closing_quote_survives() {
        assert_eq!(
            no_results_line("wallace"),
            "No results for \u{201C}wallace\u{201D}"
        );
        assert_eq!(
            no_results_line("wal…"),
            "No results for \u{201C}wal…\u{201D}"
        );
        assert!(!no_results_line("q").contains('"'));
    }

    #[test]
    fn the_heading_states_its_count_and_annotates_only_a_borrowed_source() {
        let w = |s: &str, sz: c_int| s.chars().count() as f32 * sz as f32 * 0.5;
        type Run = (String, f32, c_int, c_int, [f32; 4], bool);
        let runs = |title: &str, count: &str, source: &str| {
            let (t, c, s) = (cs(title), cs(count), cs(source));
            let mut out: Vec<Run> = Vec::new();
            heading_flow(&t, &c, &s, |run, dx, size, bold, ink, faded| {
                let text = run.to_str().unwrap_or_default().to_string();
                out.push((text.clone(), dx, size, bold, ink, faded));
                w(&text, size)
            });
            out
        };
        let bare = runs("Movies", "12 results", "");
        assert_eq!(bare.len(), 2);
        assert_eq!((bare[0].2, bare[0].3), (theme::size::HEADLINE, 1));
        assert_eq!(bare[0].4, theme::TEXT_HEADING);
        assert_eq!((bare[1].2, bare[1].3), (theme::size::BODY, 0));
        assert_eq!(bare[1].4, theme::TEXT_TERTIARY);
        assert_eq!(bare[1].1, w("Movies", theme::size::HEADLINE) + COUNT_GAP);
        let shared = runs("Movies", "12 results", "friend");
        assert_eq!(shared.len(), 4);
        assert_eq!(shared[..2], bare[..2]);
        assert_eq!(shared[2].0, "·");
        assert_eq!(shared[2].4, theme::TEXT_SEPARATOR);
        assert_eq!(
            shared[2].1,
            bare[1].1 + w("12 results", theme::size::BODY) + SOURCE_PAD
        );
        assert_eq!(shared[3].0, "friend");
        assert_eq!(shared[3].4, theme::TEXT_TERTIARY);
        assert_eq!((shared[3].2, shared[3].3), (theme::size::BODY, 0));
        assert!(shared[2].5 && shared[3].5);
        assert!(!shared[0].5 && !shared[1].5);
        assert_eq!(runs("Collections", "", "").len(), 1);
    }

    #[test]
    fn a_caption_identifies_the_result_and_names_a_borrowed_source_last() {
        let film = Item::Media(crate::pms::PmsMovie {
            title: "Wallace & Gromit".into(),
            year: 2005,
            ..Default::default()
        });
        assert_eq!(subtitle(Kind::Movie, &film, ""), "2005");
        assert_eq!(subtitle(Kind::Movie, &film, "friend"), "2005 · friend");
        let ep = Item::Media(crate::pms::PmsMovie {
            kind: 3,
            title: "A Grand Day Out".into(),
            show_title: "Wallace & Gromit".into(),
            season_index: 1,
            ep_index: 3,
            aired: "1989-11-04".into(),
            ..Default::default()
        });
        assert_eq!(subtitle(Kind::Episode, &ep, ""), "4 Nov 1989");
        assert_eq!(
            subtitle(Kind::Episode, &ep, "friend"),
            "4 Nov 1989 · friend"
        );
        let undated = Item::Media(crate::pms::PmsMovie {
            kind: 3,
            title: "A Grand Day Out".into(),
            season_index: 1,
            ep_index: 3,
            ..Default::default()
        });
        assert_eq!(subtitle(Kind::Episode, &undated, ""), "");
        assert_eq!(subtitle(Kind::Episode, &undated, "friend"), "friend");
        let bare = Item::Media(crate::pms::PmsMovie {
            title: "Untitled".into(),
            ..Default::default()
        });
        assert_eq!(subtitle(Kind::Movie, &bare, ""), "");
        assert_eq!(subtitle(Kind::Movie, &bare, "friend"), "friend");
        let person = Item::Tag(TagHit {
            name: "Peter Sallis".into(),
            count: 9,
            ..Default::default()
        });
        assert_eq!(subtitle(Kind::Person, &person, ""), "");
        assert_eq!(
            subtitle(
                Kind::Collection,
                &Item::Tag(TagHit {
                    count: 4,
                    ..Default::default()
                }),
                ""
            ),
            "4 items"
        );
        assert_eq!(
            subtitle(
                Kind::Collection,
                &Item::Tag(TagHit {
                    count: 1,
                    ..Default::default()
                }),
                ""
            ),
            "1 item"
        );
    }
}
