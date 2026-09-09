//! Paint from the instance and its retained frame views. No live Search/roster/focus reads.
use super::*;
use std::ffi::CString;
use crate::search::scope::{ScopeSource, SourceScopeSnapshot};
use crate::ui::card_row::{self, TileLabel};
use crate::ui::label::{HAlign, Label, VAlign};
use crate::ui::widgets::{Art, Button, StatusKind, StatusOverlay};
use crate::ui::{theme, Env, Painter, View};
use crate::ui::consts::{MARGIN_X, SCR_H, SCR_W};

const CARET_W: f32 = 5.0;
const CARET_GAP: f32 = 8.0;
const GHOST_GAP: f32 = theme::size::BODY as f32;

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
    pub(super) fn prepare<H: SearchLike>(&mut self, query: &str, caret: usize, owner: &str, cx: &Cx<'_, H>) {
        let q = query.split('\0').next().unwrap_or("");
        let mut caret = caret.min(q.len());
        while !q.is_char_boundary(caret) { caret -= 1; }
        if self.query != q || self.caret != caret || self.run.is_empty() {
            self.query = q.into(); self.caret = caret;
            self.run = if q.trim().is_empty() { c"Search your library".into() } else { cstring(q) };
            self.head = cstring(&q[..caret]);
            self.run_w = cx.measure.width(&self.run, theme::size::HERO, true);
            self.head_w = if q.trim().is_empty() { 0.0 } else { cx.measure.width(&self.head, theme::size::HERO, true) };
        }
        let scope = H::search(cx).scope();
        if self.scope.as_ref().is_none_or(|old| !old.same_publication(scope)) {
            self.scope_line = scope_text(scope.sources()).map(|line|
                cstring(&elide(&line, layout::FIELD.w, theme::size::CAPTION, false, cx.measure)));
            self.scope = Some(scope.clone());
        }
        let recents = H::search(cx).recents();
        if self.recents.as_ref().is_none_or(|old| !old.same_publication(recents)) {
            self.recent_runs = recents.terms().iter().map(|term|
                cstring(&elide(term, 820.0 - 2.0 * crate::ui::table::CONTENT_X, theme::size::HEADLINE, true, cx.measure))).collect();
            self.recents = Some(recents.clone());
        }
        for (i, kind) in crate::search::KINDS.iter().enumerate() {
            if self.titles[i].is_empty() { self.titles[i] = cstring(kind.title()); }
            let key = H::search(cx).shelves().get(i).map(|shelf| (shelf.kind, shelf.items.len()));
            if self.count_keys[i] != key {
                self.count_keys[i] = key;
                self.counts[i] = key.map_or_else(CString::default, |(kind, n)| cstring(&format!("{n} {}", kind.count_word(n))));
            }
        }
        if self.owner.to_bytes() != owner.as_bytes() { self.owner = cstring(owner); }
    }
}

pub(super) fn draw<H: SearchLike>(screen: &SearchScreen, f: &mut DrawFrame<'_, '_, H>) {
    crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
    let p = f.painter.alpha(f.page_alpha);
    screen.ground.draw(p, Rect::FULL);
    field(screen, f, p);
    let p = p.alpha(screen.fade.alpha());
    if !screen.recents.is_empty() { recents(screen, f, p); }
    else if screen.rows.is_empty() { empty(screen, f, p); }
    for (i, row) in screen.rows.iter().enumerate() {
        let (kinds, n) = screen.kinds();
        let top = layout::top(&kinds[..n], i, |j| screen.rows[j].motion.band_expand()) - screen.scroll.pos;
        if !crate::ui::on_axis(top, layout::block_h(row.kind, row.motion.band_expand()), SCR_H, 0.0) { continue; }
        let title = &screen.render.titles[layout::ordinal(row.kind) as usize];
        let width = f.cx.measure.width(&title, theme::size::HEADLINE, true);
        let cap = f.cx.measure.cap_h(theme::size::HEADLINE);
        Label::new(title.as_ptr(), theme::size::HEADLINE, theme::TEXT_HEADING).bold().v(VAlign::Baseline)
            .draw(p, Rect::new(MARGIN_X, top - row.motion.lift(), width, cap));
        let count = &screen.render.counts[i];
        Label::new(count.as_ptr(), theme::size::BODY, theme::TEXT_TERTIARY).v(VAlign::Baseline)
            .draw(p, Rect::new(MARGIN_X + width + theme::space::SM, top - row.motion.lift(), 0.0, cap));
        if screen.owner_row == Some(i) && screen.owner_alpha.pos > 0.02 && !screen.render.owner.is_empty() {
            let mut x = MARGIN_X + width + theme::space::SM + f.cx.measure.width(count, theme::size::BODY, false);
            let p = p.alpha(screen.owner_alpha.pos.clamp(0.0, 1.0));
            for (run, ink) in [(c"·", theme::TEXT_SEPARATOR), (screen.render.owner.as_c_str(), theme::TEXT_TERTIARY)] {
                x += theme::space::XS;
                Label::new(run.as_ptr(), theme::size::BODY, ink).v(VAlign::Baseline)
                    .draw(p, Rect::new(x, top - row.motion.lift(), 0.0, cap));
                x += f.cx.measure.width(run, theme::size::BODY, false);
            }
        }
        let focused = f.cx.focus.current.and_then(|key| row.elems.iter().position(|elem| *elem == key.elem));
        for col in 0..row.elems.len() {
            if focused != Some(col) { tile(screen, i, col, false, f, p); }
        }
        if let Some(col) = focused { tile(screen, i, col, true, f, p); }
    }
}

fn field<H: SearchLike>(screen: &SearchScreen, f: &mut DrawFrame<'_, '_, H>, p: Painter) {
    let data = &screen.render;
    let rect = Rect::new(layout::FIELD.x, layout::FIELD.y - screen.scroll.pos, layout::FIELD.w, layout::FIELD.h);
    let blank = data.query.trim().is_empty();
    let hot = screen.hot.pos;
    let ink = if blank { theme::cross(theme::TEXT_TERTIARY, theme::TEXT_SECONDARY, hot) }
        else { theme::cross(theme::TEXT_SECONDARY,
            if screen.editing { theme::FIELD_EDITING_INK } else { theme::FIELD_WAITING_INK }, hot) };
    let (run_dx, caret_dx) = run_layout(data.run_w, data.head_w, rect.w, screen.editing);
    let (cap_top, cap_base) = crate::text::text_cap_band(theme::size::HERO, 1);
    let pad = (crate::text::text_height(theme::size::HERO, 1) - rect.h * 0.5 - (cap_top + cap_base) * 0.5).max(0.0);
    {
        let _clip = f.clip(p, Rect::new(rect.x, rect.y, rect.w, rect.h + pad));
        Label::new(data.run.as_ptr(), theme::size::HERO, ink).bold()
            .draw(p, Rect::new(rect.x + run_dx, rect.y, rect.w, rect.h));
        let text_y = crate::text::text_vcenter_y(theme::size::HERO, 1, rect.cy());
        if screen.editing && screen.blink_us < super::BLINK_US {
            p.rect(Rect::new(rect.x + caret_dx, text_y + cap_top, CARET_W, cap_base - cap_top),
                0.0, theme::TEXT_PRIMARY, theme::TEXT_PRIMARY, 0.0);
        }
        if data.query.trim().chars().count() + 1 == crate::search::MIN_QUERY {
            let y = crate::text::baseline_y(theme::size::BODY, 0, theme::size::HERO, 1, text_y);
            p.text(c"one more character".as_ptr(), rect.x + caret_dx + CARET_W + GHOST_GAP, y,
                theme::size::BODY, theme::cross(theme::TEXT_TERTIARY, theme::TEXT_SECONDARY, hot), 0, 0);
        }
    }
    if let Some(line) = &data.scope_line {
        Label::new(line.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY).draw(p,
            Rect::new(rect.x, layout::SCOPE_Y - screen.scroll.pos, rect.w, theme::size::CAPTION as f32 * 1.35));
    }
    stop(screen, FIELD, ElemKind::Bare, f, p);
}

fn recents<H: SearchLike>(screen: &SearchScreen, f: &mut DrawFrame<'_, '_, H>, p: Painter) {
    let env = Env::inert();
    Label::new(c"RECENT SEARCHES".as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY).draw(p,
        Rect::new(MARGIN_X + crate::ui::table::CONTENT_X, layout::CONTENT_TOP - screen.scroll.pos, 0.0, crate::ui::table::HDR_H));
    for (index, elem) in screen.recents.iter().enumerate() {
        let Some(key) = screen.keys.iter().find(|key| key.elem == *elem) else { continue };
        let Identity::Recent(_) = &key.identity else { continue };
        let rect = layout::recent(index, screen.scroll.pos);
        let focused = f.cx.focus.current == Some(screen.key(*elem));
        if focused {
            p.rrect(Rect::new(rect.x + crate::ui::table::SIDE, rect.y + crate::ui::table::PILL_INSET,
                rect.w - 2.0 * crate::ui::table::SIDE, rect.h - 2.0 * crate::ui::table::PILL_INSET),
                crate::ui::table::PILL_RAD, crate::ui::table::PILL_RAD, theme::ACCENT);
        }
        let Some(run) = screen.render.recent_runs.get(index) else { continue };
        Label::new(run.as_ptr(), theme::size::HEADLINE, if focused { theme::ACCENT_INK } else { theme::TEXT_PRIMARY })
            .bold().draw(p, Rect::new(rect.x + crate::ui::table::CONTENT_X, rect.y, 0.0, rect.h));
        stop(screen, *elem, ElemKind::Bare, f, p);
    }
    let rect = layout::clear(screen.recents.len(), screen.scroll.pos, f.cx.measure);
    Button::new(c"Clear recent searches".as_ptr(), theme::size::BODY, rect)
        .focused(f.cx.focus.current == Some(screen.key(CLEAR))).draw(&env, p);
    stop(screen, CLEAR, ElemKind::Control, f, p);
}

fn empty<H: SearchLike>(screen: &SearchScreen, f: &DrawFrame<'_, '_, H>, p: Painter) {
    let state = H::search(f.cx).state();
    if screen.draft.pending() || state == crate::search::State::Searching { return; }
    let bottom = if screen.editing { SCR_H - layout::KEYBOARD_H } else { SCR_H };
    let rect = Rect::new(0.0, layout::CONTENT_TOP - screen.scroll.pos, SCR_W, bottom - layout::CONTENT_TOP);
    if state == crate::search::State::Failed {
        StatusOverlay::new(rect, c"Search didn’t reach the server", StatusKind::Failed)
            .reason(c"Your libraries are fine — try again in a moment.").draw(&Env::inert(), p);
        return;
    }
    let queried = screen.real_query();
    let header = if queried { c"SEARCH RESULTS" } else { c"RECENT SEARCHES" };
    let statement = if queried {
        let shell = f.cx.measure.width(c"No results for “”", theme::size::TITLE, true);
        format!("No results for “{}”", elide(screen.draft.query().trim(), 1200.0 - shell, theme::size::TITLE, true, f.cx.measure))
    } else { "Nothing searched yet".into() };
    let statement = cstring(&statement);
    let hh = f.cx.measure.cap_h(theme::size::CAPTION);
    let sh = f.cx.measure.cap_h(theme::size::TITLE);
    let top = rect.y + (rect.h - hh - sh - theme::space::MD) * 0.5;
    Label::new(header.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
        .h(HAlign::Center).v(VAlign::CapTop).draw(p, Rect::new(rect.x, top, rect.w, 0.0));
    Label::new(statement.as_ptr(), theme::size::TITLE, theme::TEXT_HEADING).bold()
        .h(HAlign::Center).v(VAlign::CapTop).draw(p, Rect::new(rect.x, top + hh + theme::space::MD, rect.w, 0.0));
}

fn tile<H: SearchLike>(screen: &SearchScreen, row: usize, col: usize, focused: bool, f: &mut DrawFrame<'_, '_, H>, p: Painter) {
    let view = H::search(f.cx);
    let Some(shelf) = view.shelves().get(row) else { return };
    let Some(item) = shelf.items.get(col) else { return };
    let model = &screen.rows[row];
    let style = layout::style(model.kind);
    let rest = screen.row_rect(row, col, At::Drawn);
    let pop = model.motion.scale(col);
    let press = if focused && f.press.scale > 0.0 { f.press.scale } else { 1.0 };
    let scale = pop * press;
    let rect = rest.scaled(scale);
    if !crate::ui::on_axis(rect.x, rect.w, SCR_W, 32.0) { return; }
    let art = match (model.kind, item) {
        (Kind::Episode, Item::Media(media)) => Art::Still(Some(media)),
        (_, Item::Media(media)) => Art::Poster(Some(media)),
        (Kind::Person, Item::Tag(tag)) => Art::Person { sid: tag.sid, key: &tag.thumb, res: (300, 300) },
        (_, Item::Tag(tag)) if tag.thumb.is_empty() => Art::Poster(None),
        (_, Item::Tag(tag)) => Art::Thumb { sid: tag.sid, key: &tag.thumb, res: (250, 375) },
    };
    let resume = match item { Item::Media(media) if model.kind != Kind::Episode => media.resume_frac(), _ => None };
    if focused {
        let sid = match item { Item::Media(media) => media.sid, Item::Tag(tag) => tag.sid };
        let handle = view.scope().sources().iter().find(|source| source.sid == sid && !source.owned).map_or("", |source| source.handle.as_str());
        let fact = subtitle(model.kind, item, handle);
        card_row::draw_focused(p, art, rect, scale, &style, resume,
            &TileLabel::titled(item.title(), &fact).revealed(model.motion.band_reveal()));
    } else { card_row::draw_tile(p, art, rect, scale, &style, resume); }
    if let (Kind::Episode, Item::Media(media)) = (model.kind, item) {
        crate::ui::widgets::still_overlay(p, media, rect, style.tile_radius(rect, scale), false);
    }
    stop(screen, model.elems[col], if matches!(model.kind, Kind::Movie | Kind::Show | Kind::Episode) { ElemKind::Card } else { ElemKind::Bare }, f, p);
}

fn stop<H: SearchLike>(screen: &SearchScreen, elem: u32, kind: ElemKind, f: &mut DrawFrame<'_, '_, H>, p: Painter) {
    use crate::ui::screen::{Activate, Hover, Stop};
    let Some(placed) = <SearchScreen as Focusable<H>>::place(screen, &elem, f.cx, At::Drawn) else { return };
    f.stop(p, Stop { key: screen.key(elem), rect: placed.rect, rest_rect: placed.rest_rect, clip: placed.clip,
        hover: Hover::Focus, activate: if kind == ElemKind::Bare { Activate::Immediate } else { Activate::Press },
    });
}

fn cstring(text: &str) -> CString { CString::new(text).unwrap_or_default() }
fn elide(text: &str, width: f32, size: i32, bold: bool, measure: &dyn crate::ui::machine::Measure) -> String {
    crate::text::elide_by(text, width, false, |s| measure.width(&cstring(s), size, bold))
}
fn run_layout(run: f32, caret: f32, width: f32, editing: bool) -> (f32, f32) {
    let available = (width - if editing { CARET_W + CARET_GAP } else { 0.0 }).max(0.0);
    let overflow = (caret - available).max(0.0).min((run - available).max(0.0));
    (-overflow, (caret - overflow + CARET_GAP).min((width - CARET_W).max(0.0)))
}

fn subtitle(kind: Kind, item: &Item, handle: &str) -> String {
    let mut parts = Vec::new();
    match item {
        Item::Media(media) if kind == Kind::Episode => {
            let date = crate::ui::fmt::pretty_date(&media.aired, media.year as i64);
            if !date.is_empty() { parts.push(date); }
        }
        Item::Media(media) if media.year > 0 => parts.push(media.year.to_string()),
        Item::Tag(tag) if kind == Kind::Collection && tag.count > 0 => parts.push(format!("{} item{}", tag.count, if tag.count == 1 { "" } else { "s" })),
        _ => {}
    }
    if !handle.is_empty() { parts.push(handle.into()); }
    parts.join(" · ")
}

fn source_label(source: &ScopeSource) -> String {
    if source.owned { return if source.name.is_empty() { "your server".into() } else { source.name.clone() }; }
    let named: Vec<_> = source.libraries.iter().filter(|name| !name.is_empty()).collect();
    if named.len() == 1 { named[0].clone() }
    else if !source.handle.is_empty() { source.handle.clone() }
    else { "a shared server".into() }
}
fn join(names: &[String]) -> String {
    match names { [] => String::new(), [one] => one.clone(), [a, b] => format!("{a} and {b}"),
        _ => format!("{} and {}", names[..names.len()-1].join(", "), names.last().unwrap()) }
}
fn name_set(sources: &[&ScopeSource]) -> String {
    let shares: Vec<_> = sources.iter().filter(|source| !source.owned).collect();
    if shares.len() <= 2 { return join(&sources.iter().map(|source| source_label(source)).collect::<Vec<_>>()); }
    let libraries = shares.iter().map(|source| source.libraries.iter().filter(|name| !name.is_empty()).count()).sum::<usize>();
    let mut names: Vec<_> = sources.iter().filter(|source| source.owned).map(|source| source_label(source)).collect();
    names.push(if libraries > 0 { format!("{libraries} shared libraries") } else { format!("{} shared sources", shares.len()) });
    join(&names)
}
fn scope_text(sources: &[ScopeSource]) -> Option<String> {
    if sources.is_empty() { return None; }
    let (live, down): (Vec<_>, Vec<_>) = sources.iter().partition(|source| source.live);
    if down.is_empty() {
        let mut line = format!("Searching {}", name_set(&live));
        let mut shares = live.iter().filter(|source| !source.owned && !source.handle.is_empty());
        if let (2, Some(source), None) = (live.len(), shares.next(), shares.next()) {
            if source_label(source) != source.handle { line.push_str(" · "); line.push_str(&source.handle); }
        }
        Some(line)
    } else if live.is_empty() { Some(format!("{} unreachable", name_set(&down))) }
    else { Some(format!("{} unreachable · results from {} only", name_set(&down), name_set(&live))) }
}
