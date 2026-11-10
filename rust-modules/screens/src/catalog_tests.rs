//! The catalog page's tests: the state x membership table, the pill faces, the status icons, the
//! Coming line, text fit in every shipped language, the summary clamp, the focus contract, what a
//! press and BACK ask for, and that the page never reads the metadata layer.
use plx_base::fontcov::advances::ShippedMeasure;
use plx_data::watchlist::model::Kind;
use plx_machine::machine::{
    Cx, Edge, Effects, EntryId, FocusKey, FocusRead, Fx, Host, InputEvent, InputKind, InputOwner, Key, LogicalState,
    Machine, Measure, PressId, PressRead, Source, Stamped, Tick,
};
use plx_platform::i18n::{language_on_this_thread_for_test, msg, Preference, SHIPPED};
use plx_ui::fit::HEADROOM;
use plx_ui::fixture::FixtureMeasure;
use plx_ui::icons::Icon;
use plx_ui::screen::{At, Dir, Focusable, ScreenEvent, Step};
use plx_ui::{detail_layout, theme};

use super::*;
use crate::registry::{AppMsg, PageMemory};

const LANGUAGES: [Preference; SHIPPED.len()] = SHIPPED;
const ENTRY: EntryId = EntryId(9);
const HEX: [u8; 12] = [7; 12];

/// A host with no stores: the page reads its record from itself.
struct CatHost;
impl Host for CatHost {
    type Arg = crate::registry::SettingsPage;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = ();
    type Init = crate::registry::NoInit;
    type Memory = PageMemory;
}

fn loaded(status: Status, release: Option<&str>) -> Load {
    Load::Loaded {
        title: "Hollow Coast".into(),
        year: Some(2027),
        kind: Kind::Movie,
        summary: "Two lines that set up the story.".into(),
        genres: vec!["Thriller".into(), "Drama".into(), "Mystery".into()],
        art_url: String::new(),
        release_date: release.map(str::to_owned),
        status,
    }
}

fn page(load: Load, membership: Membership) -> CatalogScreen {
    let mut s = CatalogScreen::new(ENTRY, HEX);
    s.input(CatalogViewModel { load, membership });
    s
}

fn cx(focus: Option<FocusKey<u32>>) -> Cx<'static, CatHost> {
    Cx {
        views: (),
        tick: Tick { ms: 0, dt_us: 16_667 },
        measure: &FixtureMeasure,
        press: PressRead::default(),
        focus: FocusRead { current: focus, ..Default::default() },
        owner: InputOwner::Entry(ENTRY),
    }
}

fn pill() -> Option<FocusKey<u32>> {
    Some(FocusKey { entry: ENTRY, elem: PILL_ELEM })
}

/// Step one event and return the effects it queued.
fn step(s: &mut CatalogScreen, ev: ScreenEvent<CatHost>, focus: Option<FocusKey<u32>>) -> Vec<Stamped<CatHost>> {
    let mut present = plx_machine::present::Present::new();
    let mut out = Vec::new();
    {
        let mut fx = Effects::new(
            &mut out,
            plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(9)),
            &mut present,
        );
        Machine::<CatHost>::step(s, &ev, &cx(focus), &mut fx);
    }
    out
}

fn asked(fx: Vec<Stamped<CatHost>>) -> Vec<CatalogOut> {
    fx.into_iter()
        .filter_map(|e| match e.fx {
            Fx::App(AppFx::Catalog(out)) => Some(out),
            _ => None,
        })
        .collect()
}

fn back_asked(fx: &[Stamped<CatHost>]) -> bool {
    fx.iter().any(|e| matches!(&e.fx, Fx::App(AppFx::Content(ContentReq::Back))))
}

fn press_key(sym: Key) -> ScreenEvent<CatHost> {
    ScreenEvent::Input(InputEvent {
        at: Default::default(),
        source: Source::Script,
        kind: InputKind::Key { key: sym, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
    })
}

/// **State x membership.** The pill, the focus group and the press are decided by the pair; only
/// a loaded record has any of them, and `Unknown` wears the Add face.
#[test]
fn state_by_membership_table() {
    let states: [(&str, Load, bool); 4] = [
        ("loading", Load::Loading, false),
        ("loaded", loaded(Status::None, None), true),
        ("not found", Load::NotFound, false),
        ("unreachable", Load::Unreachable, false),
    ];
    for (name, load, has_pill) in states {
        for (membership, filled, press) in [
            (Membership::On, true, CatalogOut::Remove { hex: HEX }),
            (Membership::Off, false, CatalogOut::Add { hex: HEX }),
            (Membership::Unknown, false, CatalogOut::Add { hex: HEX }),
        ] {
            let mut s = page(load.clone(), membership);
            let c = cx(None);
            let mut groups = Vec::new();
            Focusable::<CatHost>::groups(&s, &c, &mut groups);
            assert_eq!(groups.len(), usize::from(has_pill), "{name} {membership:?}: groups");
            assert_eq!(
                Focusable::<CatHost>::place(&s, &PILL_ELEM, &c, At::SpringTarget).is_some(),
                has_pill,
                "{name} {membership:?}: place"
            );
            assert_eq!(pill_face(membership).filled, filled, "{name} {membership:?}: face");
            let out = asked(step(&mut s, ScreenEvent::Activate(PILL_ELEM), pill()));
            assert_eq!(out, if has_pill { vec![press] } else { vec![] }, "{name} {membership:?}: press");
        }
    }
}

/// **The pill's faces:** outline bookmark + *Add to Watchlist*, filled bookmark + *On Watchlist*.
#[test]
fn the_pill_face_table() {
    let _g = language_on_this_thread_for_test(Preference::En);
    assert_eq!(pill_face(Membership::Off).icon, Icon::Bookmark);
    assert_eq!(pill_face(Membership::Unknown).icon, Icon::Bookmark);
    assert_eq!(pill_face(Membership::On).icon, Icon::BookmarkFill);
    assert_eq!(pill_label(Membership::Off).to_str(), Ok("Add to Watchlist"));
    assert_eq!(pill_label(Membership::Unknown).to_str(), Ok("Add to Watchlist"));
    assert_eq!(pill_label(Membership::On).to_str(), Ok("On Watchlist"));
}

/// **The status line's icon:** clock for a dateless coming-soon, server-slash for a released
/// title no server holds, none otherwise; a dated coming-soon title says it in the Coming line.
#[test]
fn the_status_line_icon_table() {
    let _g = language_on_this_thread_for_test(Preference::En);
    let dated = Some("2027-03-12");
    assert_eq!(status_icon(Status::ComingSoon, None), Some(Icon::Clock));
    assert_eq!(status_icon(Status::ComingSoon, Some("soon")), Some(Icon::Clock));
    assert_eq!(status_icon(Status::ComingSoon, dated), None);
    assert_eq!(status_icon(Status::NotOnAnyServer, dated), Some(Icon::CircleSlash));
    assert_eq!(status_icon(Status::NotOnAnyServer, None), Some(Icon::CircleSlash));
    assert_eq!(status_icon(Status::None, dated), None);
    assert_eq!(status_text(Status::ComingSoon, None), Some("Coming soon"));
    assert_eq!(status_text(Status::NotOnAnyServer, None), Some("Not on your servers"));
    assert_eq!(status_text(Status::ComingSoon, dated), None);
    assert_eq!(status_text(Status::None, None), None);
    assert_eq!(status_icon(Status::ServerOffline, dated), Some(Icon::ServerSlash));
    assert_eq!(status_text(Status::ServerOffline, dated), Some("Server offline"));
}

/// **The Coming line** is the long month-and-day phrase, and only an unreleased title with a
/// usable date has one.
#[test]
fn the_coming_line() {
    let _g = language_on_this_thread_for_test(Preference::En);
    assert_eq!(coming_line(Status::ComingSoon, Some("2027-03-12")).as_deref(), Some("Coming 12 March"));
    assert_eq!(coming_line(Status::ComingSoon, None), None);
    assert_eq!(coming_line(Status::ComingSoon, Some("2027-13-40")), None);
    assert_eq!(coming_line(Status::ComingSoon, Some("tomorrow")), None);
    assert_eq!(coming_line(Status::NotOnAnyServer, Some("2027-03-12")), None);
    assert_eq!(coming_line(Status::None, Some("2027-03-12")), None);
}

/// The meta line: kind, at most two genres, then the year.
#[test]
fn the_meta_line() {
    let _g = language_on_this_thread_for_test(Preference::En);
    let genres = ["Thriller".to_string(), "Drama".to_string(), "Mystery".to_string()];
    assert_eq!(meta_parts(Kind::Movie, &genres, Some(2027)), ["Movie", "Thriller", "Drama", "2027"]);
    assert_eq!(meta_parts(Kind::Show, &[], None), ["TV Show"]);
    assert_eq!(meta_parts(Kind::Movie, &[String::new()], Some(0)), ["Movie"]);
}

/// **Every sentence fits its slot in every shipped language**, on the longest month: the Coming
/// line within the hero text column, the status sentences too, and the pill's measured width
/// before the right margin. Measured with the device's own advances.
#[test]
fn every_line_fits_in_every_shipped_language() {
    let col = detail_layout::HERO_TEXT_W * HEADROOM;
    let mut over = Vec::new();
    for language in LANGUAGES {
        let _g = language_on_this_thread_for_test(language);
        for month in 1..=12 {
            let release = format!("2027-{month:02}-28");
            let line = coming_line(Status::ComingSoon, Some(&release)).expect("a dated line");
            let w = ShippedMeasure.width_str(&line, theme::size::HEADLINE, true);
            if w > col {
                over.push(format!("{} coming {release}: {w}px of {col}px: {line:?}", language.tag()));
            }
        }
        for text in [msg::watchlist_state_coming_soon(), msg::watchlist_state_on_no_server()] {
            let w = ShippedMeasure.width_str(text, theme::size::LABEL, false) + STATUS_ICON_D + theme::space::SM;
            if w > col {
                over.push(format!("{} status: {w}px of {col}px: {text:?}", language.tag()));
            }
        }
        for m in [Membership::On, Membership::Off] {
            let label = pill_label(m);
            let w = hero_layout::pill_w(&ShippedMeasure, label, theme::size::BODY, true, None);
            let room = (plx_ui::consts::SCR_W - 2.0 * plx_ui::consts::MARGIN_X) * HEADROOM;
            if w > room {
                over.push(format!("{} pill {m:?}: {w}px of {room}px: {label:?}", language.tag()));
            }
        }
    }
    assert!(over.is_empty(), "lines that do not fit:\n  {}", over.join("\n  "));
}

/// **A long title elides inside the column** (it is a text title: a discover item has no logo).
#[test]
fn a_long_title_elides_within_the_column() {
    let (_, _, _, sz) = (0.0, 0.0, 0.0, theme::size::HERO as i32);
    let title = "The Extraordinarily Long and Winding Chronicle of a Coast That Was Never Mapped ".repeat(3);
    let col = detail_layout::HERO_TEXT_W;
    let line = plx_gfx::text::elide_by(&title, col, false, |t| ShippedMeasure.width_str(t, sz, true));
    assert!(ShippedMeasure.width_str(&line, sz, true) <= col, "{line:?}");
    assert!(line.chars().count() < title.chars().count());
}

/// **The summary is clamped** to the hero's line budget however long it is, so the pill always
/// lands inside the safe area; the rows stack in order, each under the last.
#[test]
fn the_summary_is_line_clamped_and_the_rows_stack_in_order() {
    let long = "A sentence that keeps going and going. ".repeat(200);
    for measure in [&FixtureMeasure as &dyn Measure, &ShippedMeasure] {
        let tall = stack(true, true, &long, measure);
        assert!(tall.summary_h <= plx_ui::hero_syn_h(plx_ui::HERO_SYN_MAXLINES) + 0.5, "{}", tall.summary_h);
        assert!(tall.pill + hero_layout::CD <= plx_ui::consts::SCR_H - plx_ui::consts::MARGIN_Y, "{tall:?}");
        let rows = [tall.coming.unwrap(), tall.meta, tall.status.unwrap(), tall.summary, tall.pill];
        assert!(rows.windows(2).all(|w| w[0] < w[1]), "{rows:?}");
        let bare = stack(false, false, "", measure);
        assert_eq!((bare.coming, bare.status, bare.summary_h), (None, None, 0.0));
        assert!(bare.pill < tall.pill);
    }
}

/// **One focus group, one element.** It exists only for a loaded record, the pill is where the hero
/// row's first control goes, and every direction is an edge.
#[test]
fn the_focus_contract() {
    let s = page(loaded(Status::None, None), Membership::Off);
    let c = cx(None);
    let mut groups = Vec::new();
    Focusable::<CatHost>::groups(&s, &c, &mut groups);
    assert_eq!((groups.len(), groups[0].len), (1, 1));
    let placed = Focusable::<CatHost>::place(&s, &PILL_ELEM, &c, At::SpringTarget).unwrap();
    let expect = s.pill_rect(&FixtureMeasure).unwrap();
    assert_eq!(placed.rect, expect);
    assert_eq!(placed.rect.x, plx_ui::consts::MARGIN_X);
    assert_eq!(placed.rect.h, hero_layout::CD);
    assert!(Focusable::<CatHost>::place(&s, &1, &c, At::SpringTarget).is_none());
    let k = FocusKey { entry: ENTRY, elem: PILL_ELEM };
    for dir in [Dir::Up, Dir::Down, Dir::Left, Dir::Right] {
        assert!(matches!(Focusable::<CatHost>::neighbour(&s, k, dir, &c), Step::Edge));
    }
    assert_eq!(Focusable::<CatHost>::group_of(&s, &PILL_ELEM, &c), Some(GROUP));
    assert_eq!(Focusable::<CatHost>::group_of(&s, &9, &c), None);
    // a width change on the flip moves the pill's right edge, never its left
    let on = page(loaded(Status::None, None), Membership::On);
    assert_eq!(on.pill_rect(&FixtureMeasure).unwrap().x, expect.x);
}

/// **A press asks for the write the face stands for, from OK and from a click, and nothing else.**
/// BACK leaves.
#[test]
fn a_press_asks_for_the_write_and_back_leaves() {
    let mut off = page(loaded(Status::NotOnAnyServer, None), Membership::Off);
    assert_eq!(asked(step(&mut off, ScreenEvent::PressCommit(PressId(0)), pill())), [CatalogOut::Add { hex: HEX }]);
    assert_eq!(asked(step(&mut off, ScreenEvent::Activate(PILL_ELEM), None)), [CatalogOut::Add { hex: HEX }]);
    let mut on = page(loaded(Status::NotOnAnyServer, None), Membership::On);
    assert_eq!(asked(step(&mut on, ScreenEvent::PressCommit(PressId(0)), pill())), [CatalogOut::Remove { hex: HEX }]);
    // nothing focused: a stray commit asks for nothing
    assert!(asked(step(&mut on, ScreenEvent::PressCommit(PressId(0)), None)).is_empty());
    let fx = step(&mut on, press_key(Key::Back), pill());
    assert!(back_asked(&fx));
    assert!(asked(fx).is_empty());
}

/// **plx.tv unreachable** (no list): membership is Unknown, so the page wears the Add face and a
/// press asks to add. The face going back after a refused write is the app handing the screen
/// `Off` again; the screen follows its input and holds no membership of its own.
#[test]
fn an_unreachable_list_wears_the_add_face_and_a_press_follows_the_input() {
    let mut s = page(loaded(Status::None, None), Membership::Unknown);
    assert_eq!(pill_label(s.view_model().membership).to_str(), pill_label(Membership::Off).to_str());
    assert_eq!(asked(step(&mut s, ScreenEvent::Activate(PILL_ELEM), pill())), [CatalogOut::Add { hex: HEX }]);
    // the press itself flipped nothing
    assert_eq!(s.view_model().membership, Membership::Unknown);
    // the store's ledger flips it, then reverts when plex.tv refuses
    let before = LogicalState::hash(&s);
    s.input(CatalogViewModel { load: loaded(Status::None, None), membership: Membership::On });
    assert_eq!(asked(step(&mut s, ScreenEvent::Activate(PILL_ELEM), pill())), [CatalogOut::Remove { hex: HEX }]);
    assert_ne!(LogicalState::hash(&s), before);
    s.input(CatalogViewModel { load: loaded(Status::None, None), membership: Membership::Unknown });
    assert_eq!(LogicalState::hash(&s), before);
}

/// **A match landing while the page is open re-derives only the status line**: the status moves the
/// state hash and the rows below the status row, and nothing above it.
#[test]
fn a_status_change_moves_only_the_status_row() {
    let _g = language_on_this_thread_for_test(Preference::En);
    let none = stack(false, false, "x", &FixtureMeasure);
    let slash = stack(false, true, "x", &FixtureMeasure);
    assert_eq!(none.meta, slash.meta);
    assert!(slash.pill > none.pill);
    let a = page(loaded(Status::NotOnAnyServer, None), Membership::Off);
    let b = page(loaded(Status::None, None), Membership::Off);
    assert_ne!(LogicalState::hash(&a), LogicalState::hash(&b));
}

/// **The route word and the shape** are the registry's: the heartbeat word is byte-identical to
/// the route word, and the screen names the metadata layer nowhere (it is not a Detail mode).
#[test]
fn the_word_and_the_metadata_wall() {
    assert_eq!(CatalogScreen::WORD, "catalog");
    assert_eq!(Screen::<CatHost>::name(&page(Load::Loading, Membership::Unknown)), "catalog");
    let source = include_str!("catalog.rs");
    let needle = ["metadata", "::", "current"].concat();
    assert!(!source.contains(&needle));
    assert!(!source.contains("MetadataLike"));
}

/// A frame of every state draws without panicking (the recording walk, no GL), and the loaded
/// frame registers the pill as a stop.
#[test]
fn every_state_draws_and_the_pill_registers_a_stop() {
    use plx_ui::screen::{DrawFrame, Screen};
    let _g = language_on_this_thread_for_test(Preference::En);
    let loads = [
        Load::Loading,
        Load::NotFound,
        Load::Unreachable,
        loaded(Status::ComingSoon, Some("2027-03-12")),
        loaded(Status::ComingSoon, None),
        loaded(Status::NotOnAnyServer, None),
        loaded(Status::None, None),
    ];
    for load in loads {
        for membership in [Membership::On, Membership::Off, Membership::Unknown] {
            let mut s = page(load.clone(), membership);
            let c = cx(pill());
            let mut f = DrawFrame::new(&c, plx_ui::Painter::recording());
            plx_ui::screen::record_stops_while_recording(|| {
                plx_gfx::gfx::without_frame_clear(|| Screen::<CatHost>::draw(&mut s, &mut f))
            });
            let loaded = matches!(load, Load::Loaded { .. });
            assert_eq!(f.stops().iter().any(|st| st.key.elem == PILL_ELEM), loaded, "{load:?} {membership:?}");
        }
    }
}

#[test]
fn a_page_with_no_record_draws_its_verdict_string() {
    assert_eq!(verdict_text(&Load::NotFound), Some(msg::watchlist_state_on_no_server()));
    assert_eq!(verdict_text(&Load::Unreachable), Some(msg::watchlist_empty_unreachable()));
    assert_eq!(verdict_text(&Load::Loading), None);
    assert_eq!(verdict_text(&loaded(Status::None, None)), None);
}

/// The hero's rows stand the named rungs apart whichever of the Coming line and the status line is
/// absent, and an empty summary leaves the pill one LG under the last row.
#[test]
fn the_hero_stack_keeps_its_rungs_when_rows_are_absent() {
    let m = FixtureMeasure;
    let (sm, md, lg) = (theme::space::SM, theme::space::MD, theme::space::LG);
    let meta_h = m.cap_h(theme::size::BODY);
    let full = stack(true, true, "A summary.", &m);
    let coming = full.coming.unwrap();
    assert!((coming - (detail_layout::TITLE_BOTTOM + sm)).abs() < 0.01);
    assert!((full.meta - (coming + m.cap_h(theme::size::HEADLINE) + sm)).abs() < 0.01);
    let status = full.status.unwrap();
    assert!((status - (full.meta + meta_h + sm)).abs() < 0.01);
    assert!((full.summary - (status + STATUS_ICON_D + md)).abs() < 0.01);
    assert!((full.pill - (full.summary + full.summary_h + lg)).abs() < 0.01);
    let bare = stack(false, false, "", &m);
    assert_eq!((bare.coming, bare.status, bare.summary_h), (None, None, 0.0));
    assert!((bare.meta - (detail_layout::TITLE_BOTTOM + sm)).abs() < 0.01);
    assert!((bare.summary - (bare.meta + meta_h)).abs() < 0.01, "an empty summary takes no gap");
    assert!((bare.pill - (bare.summary + lg)).abs() < 0.01);
}

/// **The hero shade stands behind exactly what the page draws**: one block behind the title (what the
/// logo painted), one behind the copy from the Coming line (or the meta line) down to the summary's
/// foot, and one behind the pill, in whichever of the optional rows the status gives the page. The
/// copy's block is as wide as the widest line the page draws in it, worked out here from the measure
/// row by row, and not as wide as the 943 px column the lines are laid into.
#[test]
fn the_hero_shade_stands_behind_the_copy_the_page_draws() {
    let _g = language_on_this_thread_for_test(Preference::En);
    let m = FixtureMeasure;
    let x = plx_ui::consts::MARGIN_X;
    for (status, release) in [(Status::ComingSoon, Some("2027-03-12")), (Status::NotOnAnyServer, None), (Status::None, None)] {
        let load = loaded(status, release);
        let s = page(load.clone(), Membership::Off);
        let stack = s.stack(&m).unwrap();
        let shade = s.hero_shade(&m).expect("a loaded page has a shade");
        let blocks = shade.blocks();
        assert_eq!(blocks.len(), 3, "{status:?}: the title, the copy and the pill");
        let title = HeroLogo::new(0, "", "Hollow Coast", LogoRung::Hero).painted(title_band(), &m);
        assert_eq!(blocks[0], (title, theme::shade::COPY_A), "{status:?}: the title is what the logo painted");
        assert!(title.w < detail_layout::HERO_TEXT_W * 0.8, "{status:?}: a typed title is its own width, not the column's ({})", title.w);
        let top = stack.coming.unwrap_or(stack.meta);
        let foot = stack.summary + stack.summary_h;
        // the widest line, row by row, as the page draws them
        let Load::Loaded { year, kind, genres, summary, .. } = &load else { unreachable!() };
        let words: Vec<String> = meta_parts(*kind, genres, *year);
        let dot = m.width_str("\u{b7}", theme::size::BODY, false);
        let meta_w = words.iter().map(|w| m.width_str(w, theme::size::BODY, false)).sum::<f32>()
            + (words.len() - 1) as f32 * (dot + 2.0 * theme::space::SM);
        let mut rows = vec![meta_w, m.width_str(summary, theme::size::LABEL, false)];
        if let Some(line) = coming_line(status, release) {
            rows.push(m.width_str(&line, theme::size::HEADLINE, true));
        }
        if let (Some(_), Some(text)) = (status_icon(status, release), status_text(status, release)) {
            rows.push(STATUS_ICON_D + theme::space::SM + m.width_str(text, theme::size::LABEL, false));
        }
        let widest = rows.iter().copied().fold(0.0, f32::max);
        assert!(widest < detail_layout::HERO_TEXT_W * 0.8, "{status:?}: the premise: these lines are well inside the column ({widest})");
        let copy = Rect::new(x, top, widest, foot - top);
        let got = blocks[1].0;
        assert_eq!((got.x, got.y, got.h), (copy.x, copy.y, copy.h), "{status:?}: the copy runs from its first row to the summary's foot");
        assert!((got.w - copy.w).abs() < 0.01, "{status:?}: the copy is as wide as its widest line ({widest}), not {}", got.w);
        assert_eq!(blocks[1].1, theme::shade::COPY_A);
        // every row the page draws between them is inside that block
        let drawn = [
            Some((stack.meta, stack.meta + m.cap_h(theme::size::BODY))),
            stack.coming.map(|y| (y, y + m.cap_h(theme::size::HEADLINE))),
            stack.status.map(|y| (y, y + STATUS_ICON_D)),
        ];
        for (y0, y1) in drawn.into_iter().flatten() {
            assert!(top <= y0 && y1 <= foot, "{status:?}: the row {y0}..{y1} is outside the copy's block {top}..{foot}");
        }
        assert_eq!(blocks[2], (s.pill_rect(&m).unwrap(), theme::shade::CONTROLS_A), "{status:?}: the pill's block is its own rect");
    }
}

/// **A summary that wraps is as wide as its widest wrapped line**, the catalog's own column (943 px)
/// being only the room it wrapped into.
#[test]
fn a_wrapped_summary_shades_to_its_widest_line() {
    let _g = language_on_this_thread_for_test(Preference::En);
    let m = FixtureMeasure;
    let mut load = loaded(Status::None, None);
    let Load::Loaded { summary, .. } = &mut load else { unreachable!() };
    *summary = "A long enough synopsis that it has to wrap onto a second line inside the hero column, \
                with plenty of words so the first line fills the column and the last one is short."
        .into();
    let s = page(load, Membership::Off);
    let stack = s.stack(&m).unwrap();
    assert!(stack.summary_h > m.line_h(theme::size::LABEL) * 1.5, "the premise: the summary wraps ({})", stack.summary_h);
    let ink = s.copy_ink(&stack, &m);
    let (left, right) = *ink.last().expect("the summary is the last row");
    assert_eq!(left, plx_ui::consts::MARGIN_X);
    assert!(right > plx_ui::consts::MARGIN_X + detail_layout::HERO_TEXT_W * 0.6, "the first line fills most of the column ({right})");
    assert!(right <= plx_ui::consts::MARGIN_X + detail_layout::HERO_TEXT_W, "…and no line leaves it ({right})");
}

/// The shade is held on the copy and the pill and is exactly nothing away from them: the old corner
/// wedge darkened the whole left of the art, from the top chrome to the foot, and this does not. Nor
/// does it reach past the last letter: right of the widest line (and the feather) is bare art.
#[test]
fn the_hero_shade_is_nothing_away_from_the_copy() {
    let _g = language_on_this_thread_for_test(Preference::En);
    let m = FixtureMeasure;
    let s = page(loaded(Status::ComingSoon, Some("2027-03-12")), Membership::Off);
    let stack = s.stack(&m).unwrap();
    let shade = s.hero_shade(&m).unwrap();
    let mid = |y0: f32, h: f32| (plx_ui::consts::MARGIN_X + 200.0, y0 + h * 0.5);
    for (what, (x, y), floor) in [
        ("the meta line", mid(stack.meta, m.cap_h(theme::size::BODY)), theme::shade::COPY_A),
        ("the summary", mid(stack.summary, stack.summary_h), theme::shade::COPY_A),
        ("the pill", {
            let r = s.pill_rect(&m).unwrap();
            (r.x + r.w * 0.5, r.y + r.h * 0.5)
        }, theme::shade::CONTROLS_A),
    ] {
        assert!(shade.at(x, y, 1.0) >= floor - 1e-4, "{what} at ({x:.0},{y:.0}) is under its shade's {floor}");
    }
    let copy = shade.blocks()[1].0;
    let past_the_text = copy.x + copy.w + theme::shade::PAD + theme::shade::FEATHER + 4.0;
    assert!(past_the_text < plx_ui::consts::MARGIN_X + detail_layout::HERO_TEXT_W, "the premise: that is still inside the old column box");
    for (what, (x, y)) in [
        ("the top chrome", (960.0, 60.0)),
        ("the top left, where the wedge began", (96.0, 170.0)),
        ("the right of the art", (1700.0, 800.0)),
        ("the foot of the panel", (300.0, 1060.0)),
        ("the summary's row, past its last letter", (past_the_text, stack.summary + stack.summary_h * 0.5)),
    ] {
        assert_eq!(shade.at(x, y, 1.0), 0.0, "{what} at ({x:.0},{y:.0}) is bare art");
    }
}

/// A page with a record lays the shade over its art as the scrim layer's cells (this page has no
/// ground pass to carry it in); a page with none has no copy on the art and lays nothing.
#[test]
fn a_loaded_page_lays_the_shade_over_its_art_and_a_bare_page_lays_none() {
    use plx_ui::screen::{DrawFrame, Screen};
    use plx_ui::widgets::{cut_cells, SCRIM_CELL_H, SCRIM_CELL_W};
    let _g = language_on_this_thread_for_test(Preference::En);
    let cells = cut_cells(Rect::FULL, SCRIM_CELL_W, SCRIM_CELL_H).count();
    let layers = |load: Load| {
        let mut s = page(load, Membership::Off);
        let c = cx(None);
        let mut f = DrawFrame::new(&c, plx_ui::Painter::recording());
        let drawn = plx_ui::draw_census::capture(|| {
            plx_ui::screen::record_stops_while_recording(|| {
                plx_gfx::gfx::without_frame_clear(|| Screen::<CatHost>::draw(&mut s, &mut f))
            })
        });
        drawn.iter().filter(|(tag, _)| *tag == 26).count()
    };
    assert_eq!(layers(loaded(Status::None, None)), cells, "the shade, one layer a cell");
    for load in [Load::Loading, Load::NotFound, Load::Unreachable] {
        assert_eq!(layers(load.clone()), 0, "{load:?} draws no copy on the art, so no shade");
    }
}
