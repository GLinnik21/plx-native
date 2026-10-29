//! Paint from the instance and its retained frame views. No live Search/roster/focus reads.
use super::*;
use crate::registry::tile_facts;
use plx_data::search::scope::{ScopeSource, SourceScopeSnapshot};
use plx_ui::consts::{MARGIN_X, SCR_H};
use plx_ui::label::{HAlign, Label, VAlign};
use plx_ui::vstack::{Anchor, VStack};
use plx_ui::widgets::{Art, Button, StatusKind, StatusOverlay};
use plx_ui::{theme, Env, Painter, View};
use std::ffi::{CStr, CString};

const GHOST_GAP: f32 = theme::size::BODY as f32;

#[derive(Default)]
pub(super) struct Resources {
    /// The query as the field draws it: `plx_ui::text_field`'s measured run.
    field: plx_ui::text_field::FieldRun,
    scope: Option<SourceScopeSnapshot>,
    scope_line: Option<CString>,
    recents: Option<plx_data::search::recents::RecentsSnapshot>,
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
        self.field.prepare(query, caret, plx_platform::i18n::msg::browse_search_placeholder_c(),
            theme::size::HERO, true, cx.measure);
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
                        820.0 - 2.0 * plx_ui::table::CONTENT_X,
                        theme::size::HEADLINE,
                        true,
                        cx.measure,
                    ))
                })
                .collect();
            self.recents = Some(recents.clone());
        }
        let keys = count_keys(H::search(cx).shelves());
        for (i, kind) in plx_data::search::KINDS.iter().enumerate() {
            if self.titles[i].is_empty() {
                self.titles[i] = cstring(kind.title());
            }
            let key = keys[i];
            if self.count_keys[i] != key {
                self.count_keys[i] = key;
                self.counts[i] = key.map_or_else(CString::default, |(kind, n)| {
                    cstring(&kind.count_label(n))
                });
            }
        }
        if self.owner.to_bytes() != owner.as_bytes() {
            self.owner = cstring(owner);
        }
    }
}

/// Each shelf's count, filed under its KIND's slot — the slot `draw` reads the heading from.
///
/// `shelves()` is compact: a kind with no results has no shelf at all. Filing counts by position in
/// that list put every count after a missing kind one slot early, so a search with no TV Shows
/// headed its Episodes shelf with the Cast & Crew count ("Episodes 2 people").
fn count_keys(shelves: &[plx_data::search::Shelf]) -> [Option<(Kind, usize)>; 5] {
    let mut keys = [None; 5];
    for shelf in shelves {
        keys[layout::ordinal(shelf.kind) as usize] = Some((shelf.kind, shelf.items.len()));
    }
    keys
}

pub(super) fn draw<H: SearchLike>(screen: &SearchScreen, f: &mut DrawFrame<'_, '_, H>) {
    plx_gfx::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
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
        let Some((frame, shelf)) = screen.row_frame(f.cx, i) else { continue };
        let top = frame.y - layout::HEAD_TO_ROW;
        if !plx_ui::on_axis(
            top,
            layout::block_h(row.kind, shelf.under_band()),
            SCR_H,
            0.0,
        ) {
            continue;
        }
        let slot = layout::ordinal(row.kind) as usize;
        let title = &screen.render.titles[slot];
        let source = if screen.owner_row == Some(i) && screen.owner_alpha.pos > OWNER_FLOOR {
            screen.render.owner.as_c_str()
        } else {
            c""
        };
        plx_ui::cards::draw_heading_c(
            p,
            title,
            &screen.render.counts[slot],
            source,
            MARGIN_X,
            top - shelf.heading_lift(),
            screen.owner_alpha.pos,
            f.cx.measure,
        );
        if let Some(src) = screen.row_cards(H::search(f.cx), i) {
            shelf.draw(f, p, &src, frame);
        }
    }
    // The Request row (docs/seerr.md): headed like a result row, no count, no source.
    if screen.has_request() {
        if let Some((frame, shelf)) = screen.request_frame(f.cx) {
            let top = frame.y - layout::HEAD_TO_ROW;
            if plx_ui::on_axis(top, layout::block_h(Kind::Movie, shelf.under_band()), SCR_H, 0.0) {
                plx_ui::cards::draw_heading_c(p, plx_platform::i18n::msg::search_request_heading_c(), c"", c"",
                    MARGIN_X, top - shelf.heading_lift(), screen.owner_alpha.pos, f.cx.measure);
                let src = screen.request_cards(f.cx);
                shelf.draw(f, p, &src, frame);
            }
        }
    }
    // A standing note, never a target: the page paints through here, not `StackView::paint`, so it
    // ends the draw itself, over everything the page painted.
    screen.stack.draw_hold_hint(f.painter.alpha(f.page_alpha), f.measure);
}

fn field<H: SearchLike>(screen: &SearchScreen, f: &mut DrawFrame<'_, '_, H>, p: Painter) {
    let data = &screen.render;
    let rect = Rect::new(
        layout::FIELD.x,
        layout::FIELD.y - screen.stack.scroll(),
        layout::FIELD.w,
        layout::FIELD.h,
    );
    let hot = screen.hot.pos;
    let ink = if data.field.blank() {
        theme::cross(theme::TEXT_TERTIARY, theme::TEXT_SECONDARY, hot)
    } else {
        theme::cross(theme::TEXT_SECONDARY, ink_target(screen.editing), hot)
    };
    let caret_on = screen.blink_us < super::BLINK_US;
    let caret_dx = plx_ui::text_field::draw(f, p, rect, &data.field, ink, theme::TEXT_PRIMARY, screen.editing, caret_on);
    if ghost_shown(data.field.text()) {
        let _clip = f.clip(p, plx_ui::text_field::clip_rect(rect, theme::size::HERO));
        let text_y = plx_gfx::text::text_vcenter_y(theme::size::HERO, 1, rect.cy());
        let y = plx_gfx::text::baseline_y(theme::size::BODY, 0, theme::size::HERO, 1, text_y);
        p.text(
            plx_platform::i18n::msg::browse_search_one_more_c().as_ptr(),
            rect.x + caret_dx + plx_ui::text_field::CARET_W + GHOST_GAP,
            y,
            theme::size::BODY,
            theme::cross(theme::TEXT_TERTIARY, theme::TEXT_SECONDARY, hot),
            0,
            0,
        );
    }
    if let Some(line) = &data.scope_line {
        Label::new(line.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY).draw(
            p,
            Rect::new(
                rect.x,
                layout::SCOPE_Y - screen.stack.scroll(),
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
        plx_platform::i18n::msg::browse_search_recents_c().as_ptr(),
        theme::size::CAPTION,
        theme::TEXT_TERTIARY,
    )
    .draw(
        p,
        Rect::new(
            MARGIN_X + plx_ui::table::CONTENT_X,
            layout::CONTENT_TOP - screen.stack.scroll(),
            0.0,
            plx_ui::table::HDR_H,
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
        let rect = layout::recent(index, screen.stack.scroll());
        let focused = f.cx.focus.current == Some(screen.key(*elem));
        if focused {
            p.rrect(
                Rect::new(
                    rect.x + plx_ui::table::SIDE,
                    rect.y + plx_ui::table::PILL_INSET,
                    rect.w - 2.0 * plx_ui::table::SIDE,
                    rect.h - 2.0 * plx_ui::table::PILL_INSET,
                ),
                plx_ui::table::PILL_RAD,
                plx_ui::table::PILL_RAD,
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
            Rect::new(rect.x + plx_ui::table::CONTENT_X, rect.y, 0.0, rect.h),
        );
        stop(screen, *elem, ElemKind::Bare, f, p);
    }
    let rect = layout::clear(shown, screen.stack.scroll(), f.cx.measure);
    Button::new(plx_platform::i18n::msg::browse_search_clear_c().as_ptr(), theme::size::BODY, rect)
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
    rect.y -= screen.stack.scroll();
    if empty == EmptyState::Fault {
        StatusOverlay::new(rect, plx_platform::i18n::msg::browse_search_failed_c(), StatusKind::Failed)
            .reason(plx_platform::i18n::msg::browse_search_failed_detail_c())
            .draw(&Env::inert(), p);
        return;
    }
    let header = header_of(empty);
    let statement = if empty == EmptyState::NoResults {
        let shell =
            f.cx.measure
                .width_str(&plx_platform::i18n::msg::browse_search_no_results(""), theme::size::TITLE, true);
        no_results_line(&elide(
            screen.draft.query().trim(),
            1200.0 - shell,
            theme::size::TITLE,
            true,
            f.cx.measure,
        ))
    } else {
        plx_platform::i18n::msg::browse_search_not_yet().into()
    };
    let statement = cstring(&statement);
    let hh = f.cx.measure.cap_h(theme::size::CAPTION);
    let sh = f.cx.measure.cap_h(theme::size::TITLE);
    let (header_y, statement_y) = empty_pair(rect, hh, sh);
    Label::new(header.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
        .h(HAlign::Center)
        .v(VAlign::CapTop)
        .draw(p, Rect::new(rect.x, header_y, rect.w, 0.0));
    Label::new(statement.as_ptr(), theme::size::TITLE, theme::TEXT_HEADING)
        .bold()
        .h(HAlign::Center)
        .v(VAlign::CapTop)
        .draw(p, Rect::new(rect.x, statement_y, rect.w, 0.0));
}

/// The empty state's two cap tops: the header's band over the statement's, one MD apart, the pair
/// centred in `area`. `Item` is the search's own row type here, so the stack's is named by its path.
pub(super) fn empty_pair(area: Rect, header_h: f32, statement_h: f32) -> (f32, f32) {
    let items = [plx_ui::hstack::Item::new(header_h), plx_ui::hstack::Item::new(statement_h)];
    let pair = VStack::new(
        &items,
        theme::space::MD,
        Anchor::Centre(area.y + area.h * 0.5),
    );
    let at = |i| pair.span(i).map_or(area.y, |(y, _)| y);
    (at(0), at(1))
}

/// What a result tile draws. A collection always draws through `Art::Poster` with its row, so the
/// shared card composite decides between its poster (custom, or #274's fan for the server's
/// composite) and the shared neutral collection tile (`ui::collection_tile`, for a row with no
/// thumb) — this screen draws no collection face of its own. A tag-shaped collection hit is
/// already a thumb-less collection row by the time it lands (`search::CollectionHit::from_tag`).
pub(super) fn tile_art(kind: Kind, item: &Item) -> Art<'_> {
    match (kind, item) {
        (Kind::Episode, Item::Media(media)) => Art::Still(Some(tile_facts::of(media))),
        (_, Item::Media(media)) => Art::Poster(Some(tile_facts::of(media))),
        (_, Item::Collection(hit)) => Art::Poster(Some(tile_facts::of(&hit.item))),
        (Kind::Person, Item::Tag(tag)) => Art::Person {
            sid: tag.sid.raw(),
            key: &tag.thumb,
            res: (300, 300),
        },
        (_, Item::Tag(tag)) if tag.thumb.is_empty() => Art::Poster(None),
        (_, Item::Tag(tag)) => Art::Thumb {
            sid: tag.sid.raw(),
            key: &tag.thumb,
            res: (250, 375),
        },
    }
}

fn stop<H: SearchLike>(
    screen: &SearchScreen,
    elem: u32,
    kind: ElemKind,
    f: &mut DrawFrame<'_, '_, H>,
    p: Painter,
) {
    if !f.records_stops() {
        return;
    }
    use plx_ui::screen::{Activate, Hover, Stop};
    let Some(placed) = <SearchScreen as plx_ui::screen::Focusable<H>>::place(screen, &elem, f.cx, plx_ui::screen::At::Drawn) else {
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
    measure: &dyn plx_machine::machine::Measure,
) -> String {
    plx_gfx::text::elide_by(text, width, false, |s| {
        measure.width(&cstring(s), size, bold)
    })
}
fn ghost_shown(q: &str) -> bool {
    let n = q.trim().chars().count();
    n > 0 && n + 1 == plx_data::search::MIN_QUERY
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

fn empty_state(state: plx_data::search::State, has_shelves: bool) -> Option<EmptyState> {
    use plx_data::search::State;
    match state {
        State::Idle => Some(EmptyState::NotYet),
        State::Searching => None,
        State::Ready => (!has_shelves).then_some(EmptyState::NoResults),
        State::Failed => Some(EmptyState::Fault),
    }
}

fn header_of(state: EmptyState) -> &'static CStr {
    match state {
        EmptyState::NoResults => plx_platform::i18n::msg::browse_search_results_c(),
        _ => plx_platform::i18n::msg::browse_search_recents_c(),
    }
}

fn no_results_line(q: &str) -> String {
    plx_platform::i18n::msg::browse_search_no_results(q)
}

fn source_label(source: &ScopeSource) -> String {
    if source.household {
        return if source.name.is_empty() {
            plx_platform::i18n::msg::browse_search_your_server().into()
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
        plx_platform::i18n::msg::browse_search_shared_server().into()
    }
}
fn join(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => plx_platform::i18n::msg::browse_search_join(a, b),
        _ => plx_platform::i18n::msg::browse_search_join(&names[..names.len() - 1].join(", "), names.last().unwrap()),
    }
}
fn name_set(sources: &[&ScopeSource]) -> String {
    let shares: Vec<_> = sources.iter().filter(|source| !source.household).collect();
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
        .filter(|source| source.household)
        .map(|source| source_label(source))
        .collect();
    names.push(if libraries > 0 {
        plx_platform::i18n::msg::browse_search_shared_libraries(libraries as i64)
    } else {
        plx_platform::i18n::msg::browse_search_shared_sources(shares.len() as i64)
    });
    join(&names)
}
fn scope_text(sources: &[ScopeSource]) -> Option<String> {
    if sources.is_empty() {
        return None;
    }
    let (live, down): (Vec<_>, Vec<_>) = sources.iter().partition(|source| source.live);
    if down.is_empty() {
        let mut line = plx_platform::i18n::msg::browse_search_searching(&name_set(&live));
        let mut shares = live
            .iter()
            .filter(|source| !source.household && !source.handle.is_empty());
        if let (2, Some(source), None) = (live.len(), shares.next(), shares.next()) {
            if source_label(source) != source.handle {
                line.push_str(" · ");
                line.push_str(&source.handle);
            }
        }
        Some(line)
    } else if live.is_empty() {
        Some(plx_platform::i18n::msg::browse_search_unreachable(&name_set(&down)))
    } else {
        Some(plx_platform::i18n::msg::browse_search_partial(&name_set(&live), &name_set(&down)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translated_count_messages_follow_belarusian_grammar_with_separate_number_formatting() {
        use plx_platform::i18n::{LocaleContext, Preference, msg};
        let be = LocaleContext::resolve(Preference::Be, None, Some("en-US"), None, None);
        for (count, expected) in [(0, "0 вынікаў"), (1, "1 вынік"), (2, "2 вынікі"),
            (11, "11 вынікаў"), (21, "21 вынік"), (22, "22 вынікі"), (12345, "12,345 вынікаў")] {
            assert_eq!(msg::browse_search_count_in(&be, count), expected);
        }
        assert_eq!(msg::browse_search_people_in(&be, 2), "2 асобы");
        assert_eq!(msg::browse_search_items_in(&be, 11), "11 элементаў");
        let es = LocaleContext::resolve(Preference::Es, None, None, None, None);
        assert_eq!(msg::browse_search_count_in(&es, 1), "1 resultado");
        assert_eq!(msg::browse_search_count_in(&es, 2), "2 resultados");
    }

    #[test]
    fn translated_search_templates_preserve_user_values_and_sentence_order() {
        use plx_platform::i18n::{LocaleContext, Preference, msg};
        let be = LocaleContext::resolve(Preference::Be, None, None, None, None);
        assert_eq!(msg::browse_search_no_results_in(&be, "Кіна {query}"), "Няма вынікаў для «Кіна {query}»");
        let es = LocaleContext::resolve(Preference::Es, None, None, None, None);
        assert_eq!(msg::browse_search_partial_in(&es, "Biblioteca A", "Servidor B"),
            "No se puede conectar con Servidor B · solo hay resultados de Biblioteca A");
        assert_eq!(msg::browse_search_placeholder_c_in(&be).to_str().unwrap(), "Пошук у вашай бібліятэцы");
    }

    use plx_data::search::{Item, Kind, State};

    fn own(name: &str) -> ScopeSource {
        ScopeSource {
            sid: plx_plex::plex::ServerId::UNSET,
            name: name.into(),
            libraries: Vec::new(),
            handle: String::new(),
            owned: true,
            home: false,
            owner_id: 0,
            household: true,
            live: true,
        }
    }
    fn share(lib: &str, handle: &str) -> ScopeSource {
        ScopeSource {
            sid: plx_plex::plex::ServerId::UNSET,
            name: "a-hostname".into(),
            libraries: vec![lib.into()],
            handle: handle.into(),
            owned: false,
            home: false,
            owner_id: 0,
            household: false,
            live: true,
        }
    }
    /// A managed/Guest profile's own household server: plex.tv sends it `owned: false` (the
    /// account does not own it) but it is the viewer's own family server, not a stranger's share —
    /// `is_household` (`plex/servers.rs:272`) is true and `owner_credit` deliberately returns no
    /// handle for it (`plex/servers.rs:253-280`'s doc table). It must take the same branch `own`
    /// does: named when it has a name, "your server" when it does not.
    fn household(name: &str) -> ScopeSource {
        ScopeSource {
            sid: plx_plex::plex::ServerId::UNSET,
            name: name.into(),
            libraries: Vec::new(),
            handle: String::new(),
            owned: false,
            home: true,
            owner_id: 111_111,
            household: true,
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
    fn a_count_sits_under_its_own_kind_when_an_earlier_kind_found_nothing() {
        let shelf = |kind, n| plx_data::search::Shelf { epoch: 0, window: Default::default(),
            kind,
            items: (0..n).map(|_| Item::Media(Default::default())).collect(),
        };
        // No TV Shows: the compact shelf list skips that kind entirely.
        let keys = count_keys(&[
            shelf(Kind::Movie, 5),
            shelf(Kind::Episode, 4),
            shelf(Kind::Person, 2),
        ]);
        assert_eq!(keys[layout::ordinal(Kind::Movie) as usize], Some((Kind::Movie, 5)));
        assert_eq!(keys[layout::ordinal(Kind::Show) as usize], None);
        assert_eq!(keys[layout::ordinal(Kind::Episode) as usize], Some((Kind::Episode, 4)));
        assert_eq!(keys[layout::ordinal(Kind::Person) as usize], Some((Kind::Person, 2)));
        assert_eq!(keys[layout::ordinal(Kind::Collection) as usize], None);
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

    /// A managed/Guest profile's household server was the whole bug: `source_label`/`name_set` read
    /// raw `owned`, which plex.tv sends `false` for a household grant, so with no other source in
    /// scope the line fell through to the library-count branch and, with an empty handle, read the
    /// literal "a shared server" — describing the viewer's own family server as a stranger's. The
    /// fix reads `household` instead, so a lone household source takes the same branch `own` does.
    #[test]
    fn a_household_profiles_only_source_is_named_not_read_as_a_shared_server() {
        assert_eq!(
            scope_text(&[household("nas-home")]).unwrap(),
            "Searching nas-home"
        );
        assert_eq!(
            scope_text(&[household("")]).unwrap(),
            "Searching your server"
        );
    }

    /// With a household source AND a genuine friend's share in scope, only the friend is a "share":
    /// the friend keeps its handle attribution and its place in the shared-library count, while the
    /// household source is named exactly like an owned server and never counted as shared. This is
    /// the case the doc table at `plex/servers.rs:253-280` draws the line on — household and a
    /// friend's share are graded differently — and the wording must not blur it.
    #[test]
    fn a_household_source_is_named_while_a_friends_share_stays_credited_and_counted() {
        assert_eq!(
            scope_text(&[household("nas-home"), share("Film Club", "friend")]).unwrap(),
            "Searching nas-home and Film Club · friend"
        );
        let many = vec![
            household("nas-home"),
            share("A", "ann"),
            share("B", "bob"),
            share("C", "cat"),
        ];
        assert_eq!(
            scope_text(&many).unwrap(),
            "Searching nas-home and 3 shared libraries",
            "the household source must not inflate — or hide inside — the shared-library count"
        );
    }

    /// An unnamed, unhandled friend's share must still read as "a shared server" even when a
    /// household source is also in scope — the household branch must not accidentally swallow the
    /// external-share fallback wording.
    #[test]
    fn an_unnamed_friends_share_still_reads_as_a_shared_server_beside_a_household_source() {
        assert_eq!(
            scope_text(&[household("nas-home"), share("", "")]).unwrap(),
            "Searching nas-home and a shared server"
        );
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
        use plx_ui::cards::HEADING_SOURCE_PAD;
        let _g = plx_base::testlock::serial();
        // PAINTED through the call Search's result rows make, read back run by run, left to right
        let runs = |title: &str, count: &str, source: &str, alpha: f32| {
            let (t, c, s) = (cs(title), cs(count), cs(source));
            let mut out = plx_ui::draw_census::capture_runs(|| {
                plx_ui::cards::draw_heading_c(
                    plx_ui::Painter::recording(),
                    &t,
                    &c,
                    &s,
                    MARGIN_X,
                    100.0,
                    alpha,
                    &plx_ui::widgets::LegacyMeasure,
                );
            });
            out.sort_by(|a, b| a.at.x.total_cmp(&b.at.x));
            out
        };
        let gaps_are_the_pad = |runs: &[plx_ui::draw_census::Run]| {
            for pair in runs.windows(2) {
                let gap = pair[1].at.x - (pair[0].at.x + pair[0].at.w);
                assert!((gap - HEADING_SOURCE_PAD).abs() < 0.5, "{} -> {}: {gap}", pair[0].text, pair[1].text);
            }
        };
        // the count is a quiet note behind a dot, at the gap every heading's note keeps
        let bare = runs("Movies", "12 results", "", 1.0);
        assert_eq!(bare.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["Movies", "\u{b7}", "12 results"]);
        assert_eq!(bare[0].at.x, MARGIN_X);
        assert_eq!((bare[0].size, bare[0].bold, bare[0].ink), (theme::size::HEADLINE, true, theme::TEXT_HEADING));
        assert_eq!(bare[1].ink, theme::TEXT_SEPARATOR);
        assert_eq!((bare[2].size, bare[2].bold, bare[2].ink), (theme::size::BODY, false, theme::TEXT_TERTIARY));
        gaps_are_the_pad(&bare);
        // a borrowed source is a second note on the same pad, and only it fades with the owner
        let shared = runs("Movies", "12 results", "friend", 0.5);
        assert_eq!(shared.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["Movies", "\u{b7}", "12 results", "\u{b7}", "friend"]);
        gaps_are_the_pad(&shared);
        assert_eq!(shared[2].ink, theme::TEXT_TERTIARY, "the count does not fade");
        assert!((shared[4].ink[3] - theme::TEXT_TERTIARY[3] * 0.5).abs() < 1e-4, "the source rides the owner's alpha: {:?}", shared[4].ink);
        // a hidden count is ABSENT: the title alone, no dot
        assert_eq!(runs("Collections", "", "", 1.0).len(), 1);
    }

    /// A tile that is not a poster keeps this block: an episode still is worded by the one landscape
    /// builder (`registry::captions::still_label`: its title over its air date, its address when it
    /// has no title), a headshot has its name alone, each with the server it came from named last.
    /// (A poster's caption is the Under posters setting's: `a_poster_result_is_worded_by_the_under_posters_builder`.)
    #[test]
    fn a_still_or_headshot_caption_names_a_borrowed_source_last() {
        use crate::registry::captions::still_label;
        let lines = |l: &plx_ui::cards::TileLabel| {
            let text = |c: &Option<std::ffi::CString>| c.as_ref().map(|c| c.to_string_lossy().into_owned());
            [text(&l.title), text(&l.caption)]
        };
        let ep = plx_data::pms::PmsMovie {
            kind: 3,
            title: "A Grand Day Out".into(),
            show_title: "Wallace & Gromit".into(),
            season_index: 1,
            ep_index: 3,
            aired: "1989-11-04".into(),
            ..Default::default()
        };
        assert_eq!(lines(&still_label(&ep, false, "")), [Some("A Grand Day Out".into()), Some("11/4/1989".into())]);
        assert_eq!(
            lines(&still_label(&ep, false, "friend")),
            [Some("A Grand Day Out".into()), Some("11/4/1989 \u{b7} friend".into())]
        );
        let undated = plx_data::pms::PmsMovie {
            kind: 3,
            season_index: 1,
            ep_index: 3,
            ..Default::default()
        };
        assert_eq!(lines(&still_label(&undated, false, "")), [Some("S1 \u{b7} E3".into()), None],
            "an untitled, undated episode says its address alone");
        assert_eq!(lines(&still_label(&undated, false, "friend")), [Some("S1 \u{b7} E3".into()), Some("friend".into())]);
        assert!(still_label(&ep, false, "").shadowed, "a still is shadowed like every other landscape block");
    }

    /// Every collection result reaches the shared card as `Art::Poster` with a kind-4 row, which
    /// is what makes the card draw the shared neutral tile (`ui::collection_tile`) when the row has
    /// no thumb and the poster (or the fan) when it has one. A tag-shaped hit is one of these by
    /// the time it lands (`search::CollectionHit::from_tag`).
    #[test]
    fn a_thumbless_collection_hit_reaches_the_card_as_a_neutral_collection_row() {
        let full = Item::Collection(plx_data::search::CollectionHit {
            item: plx_data::pms::PmsMovie { title: "Shorts".into(), kind: plx_data::pms::KIND_COLLECTION,
                ..Default::default() },
            tag: 7,
        });
        assert!(matches!(tile_art(Kind::Collection, &full),
            Art::Poster(Some(m)) if m.kind == plx_ui::tile::TileKind::Collection && m.thumb.is_empty()
                && m.title == "Shorts"));
        let with_art = Item::Collection(plx_data::search::CollectionHit {
            item: plx_data::pms::PmsMovie { thumb: "/library/metadata/50007/thumb/1".into(),
                kind: plx_data::pms::KIND_COLLECTION, ..Default::default() },
            tag: 7,
        });
        assert!(matches!(tile_art(Kind::Collection, &with_art),
            Art::Poster(Some(m)) if !m.thumb.is_empty()));
    }
}
