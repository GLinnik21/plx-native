//! **R14 at the app tier: the Watchlist's routes, key by key** (`routes.md` R14), on the one route
//! rig ([`World`]): a real `Dispatcher<AppHost>` and `Bridge` over a scripted plex.tv, the loop's
//! feeds and request pump, every press a remote key.
//!
//! The disc's halves were each proven alone: the hero tests (`screens/src/home/hero_watchlist_tests`,
//! `detail/hero_watchlist_tests`) show a press EMITS an Add or Remove, `watchlist_press_tests` show a
//! request WRITES. Here the chain runs whole: keys to the disc, OK, the write plex.tv is sent, the
//! face the page then wears, OK again, the removal. And the Watchlist page's own header and Sort &
//! Filter menu, walked by keys.
//!
//! plex.tv is scripted in every test (`watchlist_press_tests::Script`): nothing here has a live path.

use super::*;
use super::test_support::{detail_arg, World};
use plx_data::catalog::Membership;
use plx_data::watchlist::ledger::Intent;
use plx_plex::plex::watchlist::WatchKind;
use plx_screens::watchlist::page::{WatchlistScreen, PILL_ELEM};

/// The movie the Home hero and the Detail page stand for: a catalog id the scripted list (titles
/// `[1; 12]` .. `[40; 12]`) does not hold, so its disc reads "Add" once the list has landed.
const HEX: [u8; 12] = [0xa1; 12];

fn home(w: &World) -> &plx_screens::home::HomeScreen {
    w.d.top_screen().and_then(|s| s.as_any()).and_then(|a| a.downcast_ref()).expect("Home is the top page")
}

fn detail(w: &World) -> &plx_screens::detail::DetailScreen {
    w.d.top_screen().and_then(|s| s.as_any()).and_then(|a| a.downcast_ref()).expect("a Detail page is the top page")
}

fn page(w: &World) -> &WatchlistScreen {
    w.d.top_screen().and_then(|s| s.as_any()).and_then(|a| a.downcast_ref()).expect("the Watchlist is the top page")
}

/// The membership the Home hero's disc wears, and the title it was fed for.
fn home_disc(w: &World) -> Option<([u8; 12], Membership)> {
    home(w).probe_for_test().disc
}

/// Is a menu (the Sort & Filter surface) up over the page?
fn menu_up(w: &World) -> bool {
    w.d.nav.modals.top().is_some()
}

/// **The Home hero's disc, by keys:** Right from Play reaches the disc, OK sends plex.tv exactly one
/// add for the hero's title and the disc wears the filled face, OK again sends the removal and the
/// face goes back, and BACK opens nothing and writes nothing.
#[test]
fn the_home_heros_disc_adds_then_removes_the_title_by_keys() {
    let mut w = World::home_with_disc("r14-home-disc", HEX);
    w.frames(40);
    assert_eq!(home(&w).watchlist_hex(), Some(HEX), "premise: the hero stands for the title");
    assert_eq!(home_disc(&w), Some((HEX, Membership::Off)), "premise: the list landed and the title is not on it");
    assert!(w.writes().is_empty(), "premise: nothing was written");
    let play = w.elem();
    assert!(play.is_some(), "premise: focus is on the hero");

    // Right from Play is the disc (the row is Play, the disc, Info).
    w.press(Key::Right);
    let disc = w.elem();
    assert_ne!(disc, play, "Right from Play moves to the next control");
    assert_eq!(w.writes(), vec![], "moving to the disc writes nothing");

    w.press(Key::Ok);
    assert_eq!(w.writes(), vec![(HEX, Intent::Add)], "OK on the disc sends exactly one add for the hero's title");
    assert_eq!(home_disc(&w), Some((HEX, Membership::On)), "and the disc wears the face of a title on the Watchlist");
    assert_eq!(w.elem(), disc, "the press leaves focus on the disc");

    w.press(Key::Ok);
    assert_eq!(w.writes(), vec![(HEX, Intent::Add), (HEX, Intent::Remove)], "OK again sends the removal");
    assert_eq!(home_disc(&w), Some((HEX, Membership::Off)), "and the face goes back");

    w.press(Key::Back);
    assert!(w.on(&AppArg::Home), "BACK on the disc leaves Home on top");
    assert!(!menu_up(&w), "and opens no menu");
    assert_eq!(w.elem(), disc, "with focus where it was");
    assert_eq!(w.writes().len(), 2, "and writes nothing");
}

/// **The same chain on a movie's Detail page:** Right from Play reaches the bookmark disc, OK adds,
/// OK again removes, and BACK pops the page.
#[test]
fn a_movie_details_disc_adds_then_removes_the_title_by_keys() {
    let mut w = World::detail_with_disc("r14-detail-disc", HEX);
    w.frames(40);
    assert_eq!(detail(&w).watchlist_hex(), Some(HEX), "premise: the page names the title's catalog id");
    assert_eq!(detail(&w).watchlist(), Membership::Off, "premise: the list landed and the title is not on it");
    assert!(w.writes().is_empty(), "premise: nothing was written");
    let play = w.elem();
    assert!(play.is_some(), "premise: focus is on the hero");

    w.press(Key::Right);
    let disc = w.elem();
    assert_ne!(disc, play, "Right from Play moves to the next control");
    assert_eq!(w.writes(), vec![], "moving to the disc writes nothing");

    w.press(Key::Ok);
    assert_eq!(w.writes(), vec![(HEX, Intent::Add)], "OK on the disc sends exactly one add for the page's title");
    assert_eq!(detail(&w).watchlist(), Membership::On, "and the disc wears the face of a title on the Watchlist");
    assert_eq!(w.elem(), disc, "the press leaves focus on the disc");

    w.press(Key::Ok);
    assert_eq!(w.writes(), vec![(HEX, Intent::Add), (HEX, Intent::Remove)], "OK again sends the removal");
    assert_eq!(detail(&w).watchlist(), Membership::Off, "and the face goes back");

    w.press(Key::Back);
    assert!(!w.on(&detail_arg("movie")), "BACK leaves the Detail page");
    assert!(w.on(&AppArg::Home), "for the page it was opened over");
    assert_eq!(w.writes().len(), 2, "and writes nothing");
}

/// **The Watchlist page's header row, by keys.** The row has ONE stop (the Sort & Filter pill: the
/// title is plain text with no plate and no stop, design review S10), so "walking it" is: Up from the first tile reaches the pill,
/// Left and Right keep focus on the pill (they neither leave sideways to the strip nor fall into the
/// grid), and Down returns to the tile Up left from (`PageRegions::CONTENT_HEAD_SEAT`, design review
/// S2: the pill is right-aligned over the sixth column, ~1400 px from the tile the eye left, and
/// the round trip does not go there).
#[test]
fn the_watchlist_header_row_holds_left_and_right_and_down_enters_the_grid() {
    let mut w = World::watchlist("r14-header");
    let first = page(&w).elem_at(0);
    assert!(first.is_some(), "premise: the list landed");
    assert_eq!(w.elem(), first, "premise: the arrival is seated on the first tile");
    assert_eq!(page(&w).probe_header().1, vec![("sort", PILL_ELEM)], "the row's stops, in order: just the pill");

    w.press(Key::Up);
    assert_eq!(w.elem(), Some(PILL_ELEM), "Up from the first tile reaches the header's pill");
    for key in [Key::Left, Key::Right, Key::Right, Key::Left] {
        w.press(key);
        assert_eq!(w.elem(), Some(PILL_ELEM), "{key:?} along the header never leaves it sideways");
    }
    assert!(!w.on_nav(), "and never reaches the strip");

    // The pill is right-aligned over the sixth column; Down still returns to the tile Up left from.
    assert_ne!(page(&w).elem_at(5), first, "premise: the tile under the pill is not the one Up left from");
    w.press(Key::Down);
    assert_eq!(w.elem(), first, "Down returns to the tile Up left from, not the one under the pill");
    assert_ne!(w.elem(), Some(PILL_ELEM));
}

/// **Sort & Filter, by keys:** OK on the pill opens the menu; choosing Title reorders the grid;
/// choosing TV Shows narrows it and the header reads "N of M"; choosing the defaults again (the menu
/// has no Reset row) puts back the full list in its first order and takes the read-out away.
#[test]
fn the_sort_and_filter_menu_reorders_narrows_and_resets_the_grid_by_keys() {
    // Forty titles, every even one a show: Type > TV Shows leaves twenty of forty.
    let script = super::watchlist_press_tests::Script::with_kinds(&(1..=40).collect::<Vec<u8>>(),
        |n| if n % 2 == 0 { WatchKind::Show } else { WatchKind::Movie });
    let mut w = World::watchlist_over("r14-sort-filter", script);
    let order = |w: &World| page(w).tiles().iter().map(|t| t.row.guid.clone()).collect::<Vec<_>>();
    let default_order = order(&w);
    assert_eq!(default_order.len(), 40, "premise: the whole list is on the page");
    assert_eq!(page(&w).count().and_then(|c| c.readout()), None, "premise: nothing narrows the list, nothing is read out");

    // The menu opens on the first row (Type > Movies & Shows); the rows run Type (3), Sort (4), Show (3).
    w.press(Key::Up);
    assert_eq!(w.elem(), Some(PILL_ELEM), "premise: focus is on the pill");
    w.press(Key::Ok);
    assert!(menu_up(&w), "OK on the pill opens the Sort & Filter menu");
    assert_eq!(w.elem(), Some(0), "on its first row");

    // Sort > Title (row 4): the grid is in title order, which is not the date order.
    for _ in 0..4 { w.press(Key::Down); }
    assert_eq!(w.elem(), Some(4), "premise: the highlight is on Sort > Title");
    w.press(Key::Ok);
    let by_title = order(&w);
    assert_eq!(by_title.len(), 40, "a sort drops nothing");
    assert_ne!(by_title, default_order, "choosing Title reorders the grid");
    let titles: Vec<String> = page(&w).tiles().iter().map(|t| t.row.title.clone()).collect();
    assert!(titles.windows(2).all(|pair| pair[0] <= pair[1]), "and the order is the titles': {titles:?}");
    assert_eq!(page(&w).count().and_then(|c| c.readout()), None, "a sort alone narrows nothing");

    // Type > TV Shows (row 2): twenty of the forty, and the header says so.
    for _ in 0..2 { w.press(Key::Up); }
    assert_eq!(w.elem(), Some(2), "premise: the highlight is on Type > TV Shows");
    w.press(Key::Ok);
    assert_eq!(page(&w).tiles().len(), 20, "the grid holds the shows");
    assert_eq!(page(&w).count(), Some(plx_screens::watchlist::header::Count::Of { shown: 20, total: 40 }));
    let readout = page(&w).count().and_then(|c| c.readout()).expect("a narrowed list reads out");
    assert!(readout.contains("20") && readout.contains("40"), "the header's read-out names both numbers: {readout:?}");

    // Back to the defaults: Type > Movies & Shows (row 0), then Sort > Date added (row 3).
    for _ in 0..2 { w.press(Key::Up); }
    w.press(Key::Ok);
    for _ in 0..3 { w.press(Key::Down); }
    assert_eq!(w.elem(), Some(3), "premise: the highlight is on Sort > Date added");
    w.press(Key::Ok);
    assert_eq!(order(&w), default_order, "the full list is back in its first order");
    assert_eq!(page(&w).count().and_then(|c| c.readout()), None, "and the read-out is gone");

    w.press(Key::Back);
    assert!(!menu_up(&w), "BACK closes the menu");
    assert_eq!(w.elem(), Some(PILL_ELEM), "with focus back on the pill");
}

/// **A focused icon-only hero control is its glyph and its focus fill, and nothing else** (owner,
/// 2026-10-20: "I don't want these labels"): no word of any control's name is drawn, and nothing at
/// all is drawn in the band under the hero's control row, on the Home hero with the bookmark disc
/// focused, on the Home hero with Info focused, and on a movie's Detail hero with the bookmark disc
/// focused (on either face of the disc). Read off the frame the product draws once every spring has
/// run to rest.
#[test]
fn a_focused_icon_only_hero_control_draws_no_name_on_the_home_hero_and_the_detail_hero() {
    use plx_platform::i18n::msg::{
        browse_menu_go_episode, browse_menu_go_movie, browse_menu_go_season, browse_menu_go_show,
        watchlist_action_add, watchlist_action_on_list,
    };
    let names = [
        watchlist_action_add(), watchlist_action_on_list(), browse_menu_go_movie(),
        browse_menu_go_show(), browse_menu_go_season(), browse_menu_go_episode(),
    ];
    // Every word the frame draws, sorted: what the page says with the Play control focused (no
    // name belongs to it) is what every other focus must leave exactly as it is.
    let said = |w: &mut World| {
        let mut said: Vec<String> = w.drawn().words.into_iter().map(|(text, _)| text).collect();
        said.sort();
        said
    };
    // One world at a time (the serial lock is not re-entrant): each is built when its turn comes.
    // `flip`: the focus is the bookmark disc, so OK on it turns its face and the other face is read too.
    let cases: [(&str, fn() -> World, &[Key], bool); 3] = [
        ("home, bookmark disc", || World::home_with_disc("r14-nameless-home-disc", HEX), &[Key::Right], true),
        ("home, Info", || World::home_with_disc("r14-nameless-home-info", HEX), &[Key::Right, Key::Right], false),
        ("detail, bookmark disc", || World::detail_with_disc("r14-nameless-detail-disc", HEX), &[Key::Right], true),
    ];
    let mut problems = Vec::new();
    for (case, mount, keys, flip) in cases {
        let mut w = mount();
        w.frames(60);
        let at_play = said(&mut w);
        let play = w.drawn().words.into_iter().find(|(text, _)| text == plx_appkit::hero_layout::play_text(false))
            .unwrap_or_else(|| panic!("{case}: premise: the Play pill draws its label; drew {at_play:?}")).1;
        let focus_at_play = w.elem();
        for key in keys { w.press(*key); }
        for face in 0..(if flip { 2 } else { 1 }) {
            if face == 1 { w.press(Key::Ok); }
            w.frames(90);
            assert_ne!(w.elem(), focus_at_play, "{case}: premise: focus left Play for the icon-only control");
            let drawn = w.drawn();
            let named: Vec<_> = drawn.words.iter().filter(|(text, _)| names.iter().any(|name| text.contains(name))).collect();
            if !named.is_empty() { problems.push(format!("{case}, face {face}: a control's name is drawn: {named:?}")); }
            // The band under the control row, as far down as a caption of the old kind stood: from
            // the Play label's line to the bottom of the `CD`-tall row (centred on it) and a caption
            // below that.
            let row_bottom = play.y + play.h * 0.5 + plx_appkit::hero_layout::CD * 0.5;
            let band = play.y + play.h..row_bottom + plx_ui::theme::space::XS + 2.0 * plx_ui::theme::size::CAPTION as f32;
            let in_band: Vec<_> = drawn.words.iter().filter(|(_, at)| band.contains(&at.y)).collect();
            if !in_band.is_empty() { problems.push(format!("{case}, face {face}: text stands under the control row: {in_band:?}")); }
            let now = said(&mut w);
            if now != at_play { problems.push(format!("{case}, face {face}: focus on the icon-only control changes what the page says: {at_play:?} -> {now:?}")); }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

/// **Sort & Filter lifts its opener** (design review S12), on the Watchlist as on the Library: OK on
/// the pill presents the menu over the page and the dispatcher names the pill as the surface's lifted
/// opener (`Dispatcher::lifted_opener`, the one decision the `Z::OPENER` pass of `Dispatcher::draw_with`
/// paints the lit pill from), and BACK stands the
/// lift down with focus on the pill. `library_route_tests` holds the Library's rows.
#[test]
fn the_watchlists_sort_and_filter_lifts_its_pill_while_the_menu_is_up() {
    let mut w = World::watchlist("r14-opener-lift");
    let page = w.d.nav.top_page().map(|e| e.id).expect("the Watchlist is on top");
    w.press(Key::Up);
    assert_eq!(w.elem(), Some(PILL_ELEM), "premise: focus is on the pill");
    assert_eq!(w.d.lifted_opener(), None, "premise: nothing is lifted while the pill is only focused");

    w.press(Key::Ok);
    assert!(menu_up(&w), "OK on the pill opens the menu");
    let lifted = w.d.lifted_opener();
    assert_eq!(lifted.map(|(host, key)| (host, key.map(|key| (key.entry, key.elem)))), Some((page, Some((page, PILL_ELEM)))),
        "the pill is the lifted opener while its menu is up");

    w.press(Key::Back);
    w.settle(40);
    assert!(!menu_up(&w), "BACK closes the menu");
    assert_eq!(w.d.lifted_opener(), None, "and the lift stands down");
    assert_eq!(w.elem(), Some(PILL_ELEM), "with focus back on the pill");
}
