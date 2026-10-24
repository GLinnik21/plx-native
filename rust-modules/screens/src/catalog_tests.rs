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
