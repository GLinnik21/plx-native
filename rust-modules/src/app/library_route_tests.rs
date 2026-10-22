//! **The Library's routes, key by key** (route lane D1; the inventory is `routes.md` R5-R11, R16):
//! the "+N" list, Sort & Filter and its "N of M" read-out, the Collections page and its chain to a
//! movie's Detail and back, the Recommended page, the breadcrumb, the A-Z rail and the heartbeat
//! word every library page wears. `page_regions_tests` owns the columns every page that wears the
//! tab bar shares (NAV, HEADER, CONTENT, BACK); this file owns what a press on a control OPENS and
//! where focus lands when it comes back.
//!
//! Every row drives the real `Dispatcher<AppHost>` + `Bridge` with scripted keys, and every request
//! a screen raises is PERFORMED by the loop's own functions (`bridge_test_support::Pump`:
//! `content::library_navigation`, `content::library_page_request`, `content_back`), not asserted as
//! emitted. A press that the screen answers and the loop drops, as OK on a Collections tile once
//! was (`Bridge::library_selection` read only the plain grid), is a red row here.
//!
//! Rows are declarative: a mount, the keys, and where focus and the page on top should be, in the
//! words `here` prints (`header:collections`, `grid:2,3`, `shelf:0,1`, `rail:M`, `nav:movies`, `menu:unwatched`).

use super::*;
use super::test_support::World;
use plx_data::browse::GenreEntry;
use plx_data::stores::browse::SecKind;
use plx_machine::machine::Chrome;
use plx_ui::screen::ScreenArg;
use plx_screens::library::menu::LibraryMenu;
use plx_screens::library::{LibraryScreen, RecommendedScreen};
use plx_screens::watchlist::header::Count;
use plx_ui::dispatch::STRIP_BASE;
use plx_ui::widgets::{TAB_KEY_MOVIES, TAB_KEY_WATCHLIST};

// ---- the vocabulary ------------------------------------------------------------------------------

/// Where focus is, in words: `nav:<pill>`, `header:<stop>` (a library name is `header:name<section>`),
/// `grid:<row>,<col>`, `shelf:<row>,<col>`, `rail:<letter>`, `menu:<row identity>`, or `page:<elem>`
/// on a page with no words of its own. A Library page that wears the "N of M" read-out appends it
/// as ` [37 of 120 films]`.
fn here(w: &World) -> String {
    let Some(focus) = w.focus() else { return "none".into() };
    if focus.elem >= STRIP_BASE {
        return match focus.elem {
            TAB_KEY_MOVIES => "nav:movies".into(),
            TAB_KEY_WATCHLIST => "nav:watchlist".into(),
            elem => format!("nav:{elem:#x}"),
        };
    }
    let any = w.focused_screen().and_then(|screen| screen.as_any());
    if let Some(page) = any.and_then(|any| any.downcast_ref::<LibraryScreen>()) {
        let readout = page.probe_readout().map_or(String::new(), |readout| format!(" [{readout}]"));
        let (names, _) = page.probe_names();
        if let Some((stop, _)) = page.probe_header_stops().into_iter().find(|(_, elem)| *elem == focus.elem) {
            return match names.iter().find(|(elem, _)| *elem == focus.elem) {
                Some((_, section)) => format!("header:name{section}{readout}"),
                None => format!("header:{stop}{readout}"),
            };
        }
        let (region, row, col, _, _) = page.probe_viewport(Some(focus));
        if region == "rail" {
            let (letters, at) = page.probe_rail(Some(focus));
            return format!("rail:{}{readout}", at.map_or("?".into(), |at| letters[at].0.clone()));
        }
        return if region == "grid" { format!("grid:{row},{col}{readout}") } else { format!("{region}{readout}") };
    }
    if let Some(menu) = any.and_then(|any| any.downcast_ref::<LibraryMenu>()) {
        return format!("menu:{}", menu.probe_row(Some(focus)).unwrap_or("?"));
    }
    if let Some(page) = any.and_then(|any| any.downcast_ref::<RecommendedScreen>()) {
        let (region, row, col, _, _) = page.probe_viewport(Some(focus));
        return if region == "shelf" { format!("shelf:{row},{col}") } else { region.into() };
    }
    format!("page:{}", focus.elem)
}

/// The page on top, by the name a row uses for it.
fn top(w: &World) -> &'static str {
    match w.d.top_arg() {
        None => "no page",
        Some(AppArg::LibraryCollections { .. }) => "collections",
        Some(AppArg::LibraryRecommended { .. }) => "recommended",
        Some(AppArg::Library) => "library",
        Some(AppArg::Content(ContentArg::Collection(_))) => "collection",
        Some(AppArg::Content(ContentArg::Detail { .. })) => "detail",
        Some(arg) => crate::app::words::route_word(arg),
    }
}

/// One key and what it must leave behind.
struct Step {
    key: Key,
    /// The request the loop must have PERFORMED for this press, as a prefix of its label; `""` is
    /// "none was raised". Exactly one request is expected otherwise.
    did: String,
    /// The page on top once the press has settled (`top`).
    top: String,
    /// Where focus is (`here`). `@n` is where focus was just before step `n` was pressed, `@-n`
    /// where it was before the step `n` back: a Back must restore the element, not a look-alike.
    /// A trailing `*` matches a prefix.
    at: String,
    /// Runs just before the key: the data a request already performed would have brought back.
    prep: fn(&mut World),
}

fn nothing(_: &mut World) {}

/// The Movies library, reached by its strip pill, settled, with focus climbed into its header (the
/// page arrives in its first poster, one Up below the header the rows below start from).
fn library(w: &mut World) { w.open_library(); w.climb_into_header(); }

fn press(key: Key, did: &str, top: &str, at: impl Into<String>) -> Step {
    Step { key, did: did.into(), top: top.into(), at: at.into(), prep: nothing }
}

/// `step`, once `prep` has run.
fn after(prep: fn(&mut World), step: Step) -> Step { Step { prep, ..step } }

/// A route: a mount, the keys, and where each leaves the app. `end` is graded after the last key.
struct Row {
    name: &'static str,
    setup: fn(&mut World),
    /// Where focus is before the first key.
    start: &'static str,
    steps: Vec<Step>,
    end: fn(&mut World) -> Vec<String>,
}

fn no_checks(_: &mut World) -> Vec<String> { Vec::new() }

fn matches_words(want: &str, got: &str) -> bool {
    want.strip_suffix('*').map_or(want == got, |prefix| got.starts_with(prefix))
}

/// Every row in a fresh world; every deviation of every row is reported together.
fn run(rows: Vec<Row>) {
    let mut fails = Vec::new();
    for row in rows {
        let mut w = World::new(row.name);
        (row.setup)(&mut w);
        let start = here(&w);
        if start != row.start {
            fails.push(format!("{}: premise: focus should start on {}, not {start}", row.name, row.start));
            continue;
        }
        let mut before: Vec<String> = Vec::new();
        for (index, step) in row.steps.iter().enumerate() {
            (step.prep)(&mut w);
            before.push(here(&w));
            let mut did = w.press(step.key);
            did.extend(w.settle(40));
            let want_at = match step.at.strip_prefix('@') {
                Some(n) => {
                    let n = n.parse::<isize>().expect("@n names a step");
                    before[usize::try_from(if n < 0 { index as isize + n } else { n }).expect("a step before this one")].clone()
                }
                None => step.at.clone(),
            };
            let (got_top, got_at) = (top(&w), here(&w));
            let did_ok = did.len() == usize::from(!step.did.is_empty()) && did.iter().all(|label| label.starts_with(&step.did));
            if !(did_ok && got_top == step.top.as_str() && matches_words(&want_at, &got_at)) {
                fails.push(format!(
                    "{}: step {index} ({:?} from {}): expected [did {:?}, top {}, at {want_at}], saw [did {did:?}, top {got_top}, at {got_at}]",
                    row.name, step.key, before[index], step.did, step.top));
                break;
            }
        }
        fails.extend((row.end)(&mut w).into_iter().map(|fail| format!("{}: {fail}", row.name)));
    }
    assert!(fails.is_empty(), "\n{}", fails.join("\n"));
}

// ---- data a performed request would have brought back -------------------------------------------

/// The Collections page's own listing answered: `n` collections.
fn collections_answered(w: &mut World) {
    w.bridge.stores.browse.borrow_mut().seed_collections_for_test(12);
    w.frames(40);
}

/// The Collections page's listing answered, with the first-letter counts the rail is made of.
fn collections_lettered(w: &mut World) {
    w.bridge.stores.browse.borrow_mut().seed_letter_counts_for_test(&[("A", 6), ("M", 6)]);
    collections_answered(w);
}

/// The collection screen's members answered: nine films.
fn members_answered(w: &mut World) {
    let sid = w.own;
    let items: Vec<_> = (0..9).map(|i| plx_data::pms::PmsMovie { sid, rk: format!("m{i}"), title: format!("Movie {i}"), ..Default::default() }).collect();
    w.bridge.stores.collection.install_for_test(items, plx_data::collection::CollectionStatus::Ready);
    w.frames(40);
}

/// The server's answer to a filter: `n` of the library's films remain.
fn films_answered<const N: usize>(w: &mut World) {
    w.bridge.stores.browse.borrow_mut().seed_items_for_test(N);
    w.frames(30);
}

// ---- R7 + R8: Collections, and the chain to a film and back --------------------------------------

#[test]
fn the_collections_page_opens_from_its_control_and_back_returns_to_it() {
    run(vec![Row {
        name: "r7-collections",
        setup: library,
        start: "header:collections",
        steps: vec![
            // The listing has not answered in this rig, so the page reads Failed and arrives on its
            // Retry (the content region's first stop); BACK leaves the page from there.
            press(Key::Ok, "library:OpenListing { listing: Collections", "collections", "status"),
            press(Key::Back, "content:Back", "library", "header:collections"),
            // …and again: the control is still the one that opens it.
            press(Key::Ok, "library:OpenListing { listing: Collections", "collections", "status"),
            press(Key::Back, "content:Back", "library", "header:collections"),
        ],
        end: |w| {
            let mut fails = Vec::new();
            if w.d.top_arg().map(|arg| arg.chrome()) != Some(Chrome::TabBar) { fails.push("the Library wears the tab bar".into()); }
            fails
        },
    }]);
}

/// R8: the chain the owner's report broke at `library_selection` (OK on a Collections tile was
/// dropped by the loop). Every request is performed, and every Back puts focus back on the element
/// it left, not on a look-alike.
#[test]
fn a_collection_opens_from_the_collections_page_to_a_film_and_every_back_returns_to_where_it_left() {
    run(vec![Row {
        name: "r8-chain",
        setup: library,
        start: "header:collections",
        steps: vec![
            /* 0 */ press(Key::Ok, "library:OpenListing { listing: Collections", "collections", "status"),
            // The answer lands under the page's own seat, which follows it into the first tile.
            /* 1 */ after(collections_answered, press(Key::Right, "", "collections", "grid:0,1")),
            /* 2 */ press(Key::Ok, "library:Detail", "collection", "page:0"),
            // The screen opens on its header (elem 0) until its members arrive; its grid numbers
            // them from 4096, six to a row.
            /* 3 */ after(members_answered, press(Key::Down, "", "collection", "page:4102")),
            /* 4 */ press(Key::Right, "", "collection", "page:4103"),
            /* 5 */ press(Key::Ok, "content:Push", "detail", "page:0"),
            /* 6 */ press(Key::Back, "content:Back", "collection", "@5"),
            /* 7 */ press(Key::Back, "content:Back", "collections", "@2"),
            /* 8 */ press(Key::Back, "content:Back", "library", "@0"),
        ],
        end: no_checks,
    }]);
}

// ---- R9 + R10: Recommended, and the crumb --------------------------------------------------------

#[test]
fn recommended_opens_a_film_and_back_returns_to_the_card_then_the_control() {
    run(vec![Row {
        name: "r9-recommended",
        setup: library,
        start: "header:collections",
        steps: vec![
            /* 0 */ press(Key::Right, "", "library", "header:recommended"),
            /* 1 */ press(Key::Ok, "library:OpenListing { listing: Recommended", "recommended", "shelf:0,0"),
            /* 2 */ press(Key::Right, "", "recommended", "shelf:0,1"),
            /* 3 */ press(Key::Down, "", "recommended", "shelf:1,1"),
            /* 4 */ press(Key::Ok, "library:Detail", "detail", "page:0"),
            /* 5 */ press(Key::Back, "content:Back", "recommended", "@4"),
            // The first Back from a lower shelf rests the page on its first row; the next leaves.
            /* 6 */ press(Key::Back, "", "recommended", "shelf:0,1"),
            /* 7 */ press(Key::Back, "content:Back", "library", "header:recommended"),
        ],
        end: no_checks,
    }]);
}

#[test]
fn the_crumb_root_pops_to_the_library_and_up_from_it_is_the_movies_pill() {
    run(vec![
        Row {
            name: "r10-crumb-collections",
            setup: library,
            start: "header:collections",
            steps: vec![
                press(Key::Ok, "library:OpenListing { listing: Collections", "collections", "status"),
                press(Key::Up, "", "collections", "header:crumb"),
                press(Key::Up, "", "collections", "nav:movies"),
                press(Key::Down, "", "collections", "header:crumb"),
                press(Key::Ok, "content:Back", "library", "header:collections"),
            ],
            end: no_checks,
        },
        Row {
            name: "r10-crumb-recommended",
            setup: library,
            start: "header:collections",
            steps: vec![
                press(Key::Right, "", "library", "header:recommended"),
                press(Key::Ok, "library:OpenListing { listing: Recommended", "recommended", "shelf:0,0"),
                press(Key::Up, "", "recommended", "crumb"),
                press(Key::Up, "", "recommended", "nav:movies"),
                press(Key::Down, "", "recommended", "crumb"),
                press(Key::Ok, "content:Back", "library", "header:recommended"),
            ],
            end: no_checks,
        },
    ]);
}

// ---- R5: the "+N" list ---------------------------------------------------------------------------

/// Eleven Movie libraries: the header names the first two and folds the rest behind "+N".
fn many_libraries(w: &mut World) {
    {
        let mut browse = w.bridge.stores.browse.borrow_mut();
        for k in 0..9 {
            browse.append_section_for_test(k % 2, 10 + k as i64, &format!("Extra library number {k}"), SecKind::Movie);
        }
    }
    w.bridge.refresh_browse_directory();
    w.open_library();
    w.climb_into_header();
    let (names, folded) = w.library().probe_names();
    assert_eq!(names.iter().map(|(_, section)| *section).collect::<Vec<_>>(), [0, 4], "premise: the visible names");
    assert_eq!(folded, [6, 8, 10, 12], "premise: the folded libraries");
}

/// The fold after a pick: the picked name has the LAST visible slot, the one it took returns to the
/// list, and the store (not just the header) is on the new library.
fn the_pick_took_the_last_slot(w: &mut World) -> Vec<String> {
    let (names, folded) = w.library().probe_names();
    let visible: Vec<usize> = names.iter().map(|(_, section)| *section).collect();
    let mut fails = Vec::new();
    if visible != [0, 12] { fails.push(format!("the names are {visible:?}, expected the picked 12 in the last slot after 0")); }
    if folded != [4, 6, 8, 10] { fails.push(format!("the list holds {folded:?}, expected the displaced 4 back and the picked 12 gone")); }
    let current = w.bridge.stores.browse.borrow().current_for_test();
    if current != 12 { fails.push(format!("the store is on library {current}, not the picked 12")); }
    if w.d.surface_up() { fails.push("the menu is still up".into()); }
    fails
}

fn nothing_was_picked(w: &mut World) -> Vec<String> {
    let (names, folded) = w.library().probe_names();
    let visible: Vec<usize> = names.iter().map(|(_, section)| *section).collect();
    let mut fails = Vec::new();
    if visible != [0, 4] || folded != [6, 8, 10, 12] { fails.push(format!("the fold changed: names {visible:?}, list {folded:?}")); }
    let current = w.bridge.stores.browse.borrow().current_for_test();
    if current != 0 { fails.push(format!("the store left library 0 for {current}")); }
    fails
}

#[test]
fn the_plus_n_list_picks_a_library_into_the_last_slot_and_back_picks_nothing() {
    run(vec![
        Row {
            name: "r5-pick",
            setup: many_libraries,
            start: "header:name0",
            steps: vec![
                press(Key::Right, "", "library", "header:name4"),
                press(Key::Right, "", "library", "header:more"),
                // The list holds the folded libraries (6, 8, 10, 12) as `section:<source>:<key>`.
                press(Key::Ok, "library:Menu { kind: Sources", "library", "menu:section:0:12"),
                press(Key::Down, "", "library", "menu:section:0:14"),
                press(Key::Down, "", "library", "menu:section:0:16"),
                press(Key::Down, "", "library", "menu:section:0:18"),
                press(Key::Ok, "", "library", "header:name12"),
            ],
            end: |w| { w.frames(90); the_pick_took_the_last_slot(w) },
        },
        Row {
            name: "r5-back",
            setup: many_libraries,
            start: "header:name0",
            steps: vec![
                press(Key::Right, "", "library", "header:name4"),
                press(Key::Right, "", "library", "header:more"),
                press(Key::Ok, "library:Menu { kind: Sources", "library", "menu:section:0:12"),
                press(Key::Down, "", "library", "menu:section:0:14"),
                press(Key::Back, "", "library", "header:more"),
            ],
            end: |w| { w.frames(90); nothing_was_picked(w) },
        },
    ]);
}

/// **Every header opener stays the lifted, focused control while its menu is up** (design review
/// S12): the dispatcher names the page's element as the surface's lifted opener, the one decision
/// the dispatcher's lift pass paints from, for the `+N` list as for Sort & Filter. The `+N` list named
/// none, so its page capsule sat dimmed under the scrim.
fn the_menu_lifts_its_opener(stop: &'static str) -> impl Fn(&mut World) -> Vec<String> {
    move |w: &mut World| {
        w.frames(30);
        let want = w.library().probe_header_stops().into_iter().find(|(name, _)| *name == stop).map(|(_, elem)| elem);
        let page = w.d.nav.top_page().map(|e| e.id);
        let lifted = w.d.lifted_opener();
        match (lifted, page, want) {
            (Some((host, Some(key))), Some(page), Some(elem)) if host == page && key.entry == page && key.elem == elem => Vec::new(),
            other => vec![format!("the {stop} opener is not the lifted element: {other:?}")],
        }
    }
}

#[test]
fn the_plus_n_list_and_sort_filter_both_lift_their_opener() {
    // `end` is a plain `fn`: one per stop, the closure's `stop` carried by the function's body.
    fn plus_n(w: &mut World) -> Vec<String> { the_menu_lifts_its_opener("more")(w) }
    fn sort(w: &mut World) -> Vec<String> { the_menu_lifts_its_opener("sort")(w) }
    run(vec![
        Row {
            name: "s12-plus-n",
            setup: many_libraries,
            start: "header:name0",
            steps: vec![
                press(Key::Right, "", "library", "header:name4"),
                press(Key::Right, "", "library", "header:more"),
                press(Key::Ok, "library:Menu { kind: Sources", "library", "menu:section:0:12"),
            ],
            end: plus_n,
        },
        Row {
            name: "s12-sort-filter",
            setup: library,
            start: "header:collections",
            steps: {
                let mut steps = to_sort_filter();
                steps.push(press(Key::Ok, "library:Menu { kind: SortFilter", "library", "menu:unwatched"));
                steps
            },
            end: sort,
        },
    ]);
}

/// **The opener pill is painted AFTER the panel, in the real dispatcher draw** (design review S12,
/// code review R1/R3): with Library Sort & Filter open, every text run the frame draws is
/// censused (`record_walk` makes the root painters record: no GL), and the pill's run must come
/// later than the panel's rows, the order that lets the glass grow out from under a pill that does
/// not change. The page pass leaves the pill out (`DrawFrame::lifted`), so the lift pass is its
/// ONLY copy. Red while the lift lived in the application's page closure: the dispatcher's own
/// draw (this test, and a surfaces-only plan) painted the panel and never the pill.
#[test]
fn the_sort_filter_pill_is_painted_after_the_panel_in_the_dispatchers_own_draw() {
    use plx_ui::frame::backdrop::Z;
    let mut w = World::movies_library("route-sort-lift-order", false);
    w.climb_into_header();
    for (key, at) in [(Key::Right, "header:recommended"), (Key::Right, "header:sort"), (Key::Ok, "menu:unwatched")] {
        w.press(key);
        w.frames(30);
        assert_eq!(here(&w), at, "premise: {key:?} lands on {at}");
    }
    assert!(w.d.lifted_opener().is_some(), "premise: the menu names the pill as its lifted opener");
    let mut glass = plx_ui::frame::glass::GlassPlan::new();
    let sources = glass.sources.clone();
    sources.borrow_mut().begin(w.d.backdrop_layers(1.0));
    let words = plx_ui::draw_census::capture_text(|| {
        let _declarations = plx_ui::frame::backdrop::discover(sources.clone());
        w.d.draw_with_glass_below(&mut w.bridge, &mut glass, true, Z::ALL);
    });
    let words: Vec<&str> = words.iter().map(|(s, _)| s.as_str()).collect();
    let at = |word: &str| words.iter().position(|w| *w == word);
    let pills: Vec<usize> = (0..words.len()).filter(|i| words[*i] == "Sort & Filter").collect();
    let (panel_first, panel_last) = (at("SORT").expect("the panel drew its Sort section"), at("Genre").expect("…and its last Filter row"));
    assert_eq!(pills.len(), 1, "the pill is drawn once, by the lift (the page pass left it out): {words:?}");
    assert!(pills[0] > panel_last && panel_first < panel_last,
        "the pill's run ({}) comes after the panel's rows ({panel_first}..={panel_last}): {words:?}", pills[0]);
}

/// **A library the server says has no collections has no Collections stop** (the rig's probe is
/// seeded per section, `collections_probe::seed_for_test`, so the app tier models what the live
/// probe reports): none, or a refusal (HTTP 403), puts nothing on the header, and the arrows walk
/// Recommended then Sort & Filter. With the rig's count the control is the first stop.
#[test]
fn a_library_without_collections_has_no_collections_stop() {
    use plx_plex::plex::collections::CollectionsCount;
    for (name, answer, collections) in [
        ("route-no-collections-none", CollectionsCount::Count(0), false),
        ("route-no-collections-403", CollectionsCount::Forbidden, false),
        ("route-no-collections-unknown", CollectionsCount::Unknown, false),
        ("route-collections-some", CollectionsCount::Count(7), true),
    ] {
        let mut w = World::movies_library_answering(name, answer);
        let stops: Vec<&str> = w.library().probe_header_stops().into_iter().map(|(stop, _)| stop).collect();
        assert_eq!(stops.contains(&"collections"), collections, "{name}: the header's stops are {stops:?}");
        w.climb_into_header();
        assert_eq!(here(&w), if collections { "header:collections" } else { "header:recommended" }, "{name}");
        w.press(Key::Right);
        w.frames(10);
        assert_eq!(here(&w), if collections { "header:recommended" } else { "header:sort" }, "{name}");
    }
}

// ---- R6: Sort & Filter and the "N of M" read-out ---------------------------------------------------

/// The Sort & Filter control, from the Library's arrival on the Collections control.
fn to_sort_filter() -> Vec<Step> {
    vec![
        press(Key::Right, "", "library", "header:recommended"),
        press(Key::Right, "", "library", "header:sort"),
    ]
}

fn with_genres(w: &mut World) {
    w.open_library();
    w.climb_into_header();
    let genre = |id: &str, title: &str| GenreEntry { id: id.into(), title: title.into() };
    w.bridge.stores.browse.borrow_mut().seed_query_choices_for_test(Vec::new(), vec![genre("g1", "Drama"), genre("g2", "Comedy")]);
    w.frames(30);
}

#[test]
fn a_filter_shows_n_of_m_beside_the_control_and_clears_when_reset() {
    // The Watchlist's header wears the same sentence from the same message.
    assert_eq!(Count::Of { shown: 37, total: 120 }.readout().as_deref(), Some("37 of 120"));
    assert_eq!(Count::All(120).readout(), None);

    let unwatched = |from: Vec<Step>| {
        let mut steps = from;
        steps.extend([
            press(Key::Ok, "library:Menu { kind: SortFilter", "library", "menu:unwatched"),
            // A toggle keeps the menu up…
            press(Key::Ok, "", "library", "menu:unwatched"),
            // …and Back closes it onto the control that opened it. The filtered answer is not in
            // yet, so the row says nothing (the grid is mid-fade, the count would be a guess).
            press(Key::Back, "", "library", "header:sort*"),
            // The server answered: 37 of the library's 120.
            after(films_answered::<37>, press(Key::Right, "", "library", "header:sort [37 of 120 films]")),
            // Reset it, from the same control.
            press(Key::Ok, "library:Menu { kind: SortFilter", "library", "menu:unwatched"),
            press(Key::Ok, "", "library", "menu:unwatched"),
            press(Key::Back, "", "library", "header:sort*"),
            after(films_answered::<120>, press(Key::Right, "", "library", "header:sort")),
        ]);
        steps
    };
    run(vec![
        Row { name: "r6-unwatched", setup: library, start: "header:collections", steps: unwatched(to_sort_filter()), end: no_checks },
        Row {
            name: "r6-genre",
            setup: with_genres,
            start: "header:collections",
            steps: {
                let mut steps = to_sort_filter();
                steps.extend([
                    press(Key::Ok, "library:Menu { kind: SortFilter", "library", "menu:unwatched"),
                    press(Key::Down, "", "library", "menu:genre"),
                    // The Genre row opens the genre list in place; a pick closes the menu.
                    press(Key::Ok, "", "library", "menu:genre:all"),
                    press(Key::Down, "", "library", "menu:genre:g1"),
                    press(Key::Ok, "", "library", "header:sort*"),
                    after(films_answered::<37>, press(Key::Right, "", "library", "header:sort [37 of 120 films]")),
                ]);
                steps
            },
            end: no_checks,
        },
    ]);
}

// ---- R11: the A-Z rail ---------------------------------------------------------------------------

/// 120 films in four letters of thirty: A, C, M and Z. The other twenty-two letters have no title.
fn lettered(w: &mut World) {
    w.bridge.stores.browse.borrow_mut().seed_letter_counts_for_test(&[("A", 30), ("C", 30), ("M", 30), ("Z", 30)]);
    w.open_library();
    w.climb_into_header();
    w.frames(30);
}

/// A step with nothing performed on the Library, for words that are built, not written.
fn step_at(key: Key, at: String) -> Step { press(key, "", "library", at) }

/// `key` once per grid row in `rows`, each landing in column `col` of that row.
fn grid_column(key: Key, rows: std::ops::RangeInclusive<usize>, col: usize) -> Vec<Step> {
    rows.map(|row| step_at(key, format!("grid:{row},{col}"))).collect()
}

#[test]
fn the_rail_is_entered_only_from_the_last_column_and_walks_over_empty_letters() {
    run(vec![
        Row {
            name: "r11-walk",
            setup: lettered,
            start: "header:collections",
            steps: {
                let mut steps = vec![
                    // The page arrived on its first poster, so Down from the header returns to it.
                    press(Key::Down, "", "library", "grid:0,0"),
                    // Not the last column: Right stays in the grid.
                    press(Key::Right, "", "library", "grid:0,1"),
                    press(Key::Right, "", "library", "grid:0,2"),
                    press(Key::Right, "", "library", "grid:0,3"),
                    press(Key::Right, "", "library", "grid:0,4"),
                    press(Key::Right, "", "library", "grid:0,5"),
                    // The last column: Right enters the rail on the letter under the card.
                    press(Key::Right, "", "library", "rail:A"),
                    press(Key::Right, "", "library", "rail:A"),
                ];
                // Down steps over B, D-L and N-Y; Up steps back over them; both ends hold.
                for letter in ["C", "M", "Z", "Z"] { steps.push(step_at(Key::Down, format!("rail:{letter}"))); }
                for letter in ["M", "C", "A", "A"] { steps.push(step_at(Key::Up, format!("rail:{letter}"))); }
                // The grid followed the rail to the start of its letter: A begins at the first card.
                steps.push(press(Key::Left, "", "library", "grid:0,0"));
                steps
            },
            end: no_checks,
        },
        Row {
            name: "r11-left-returns",
            setup: lettered,
            start: "header:collections",
            steps: {
                // Down from the header returns to the cell Up left from, the page's first poster;
                // column 3 is reached by walking.
                let mut steps = vec![press(Key::Down, "", "library", "grid:0,0")];
                steps.extend((1..=3).map(|col| step_at(Key::Right, format!("grid:0,{col}"))));
                steps.extend(grid_column(Key::Down, 1..=2, 3));
                // Row 2, last column: Right enters the rail and Left comes back to the SAME card.
                steps.extend(grid_column(Key::Right, 2..=2, 4));
                steps.extend(grid_column(Key::Right, 2..=2, 5));
                steps.extend([press(Key::Right, "", "library", "rail:A"), press(Key::Left, "", "library", "@-1")]);
                // Row 7 is in C (items 30-59): the rail opens on C and Left returns to row 7.
                steps.extend(grid_column(Key::Down, 3..=7, 5));
                steps.extend([press(Key::Right, "", "library", "rail:C"), press(Key::Left, "", "library", "@-1")]);
                // Having moved on the rail, Left lands on the first card of the letter it is on.
                steps.extend([
                    press(Key::Right, "", "library", "rail:C"),
                    press(Key::Down, "", "library", "rail:M"),
                    press(Key::Left, "", "library", "grid:10,0"),
                ]);
                steps
            },
            end: no_checks,
        },
        Row {
            // The Collections page keeps the rail: the rail is the ordinary library page's (it
            // asks the server for the first letters of whatever type the page lists, collections
            // included), and so are its doors.
            name: "r11-collections",
            setup: library,
            start: "header:collections",
            steps: {
                let mut steps = vec![
                    press(Key::Ok, "library:OpenListing { listing: Collections", "collections", "status"),
                    // The answer lands under the page's own seat, which follows it into the first tile.
                    after(collections_lettered, press(Key::Right, "", "collections", "grid:0,1")),
                ];
                steps.extend((2..=5).map(|col| press(Key::Right, "", "collections", format!("grid:0,{col}"))));
                steps.extend([
                    press(Key::Right, "", "collections", "rail:A"),
                    press(Key::Left, "", "collections", "@-1"),
                    press(Key::Right, "", "collections", "rail:A"),
                    press(Key::Down, "", "collections", "rail:M"),
                    press(Key::Left, "", "collections", "grid:1,0"),
                ]);
                steps
            },
            end: no_checks,
        },
    ]);
}


// ---- R16: the three library pages as one route ---------------------------------------------------

/// What a library page must say of itself: the heartbeat word, the chrome and the screen's own name.
fn wears_the_library_route(w: &World, page: &str) -> Vec<String> {
    let mut fails = Vec::new();
    let Some(arg) = w.d.top_arg() else { return vec![format!("{page}: no page is up")] };
    let word = crate::app::words::route_word(arg);
    if word != "library" { fails.push(format!("{page}: the heartbeat word is {word}")); }
    if arg.chrome() != Chrome::TabBar { fails.push(format!("{page}: the chrome is {:?}", arg.chrome())); }
    let name = w.d.top_screen().map(|screen| screen.name());
    if name != Some(word) { fails.push(format!("{page}: the mounted screen answers {name:?}, not the route word {word}")); }
    if w.d.nav.tabs.strip.is_empty() { fails.push(format!("{page}: the strip is not published")); }
    fails
}

#[test]
fn the_three_library_pages_wear_one_route_word_and_the_tab_bar() {
    let mut w = World::new("r16-word");
    w.open_library();
    w.climb_into_header();
    let mut fails = wears_the_library_route(&w, "the grid");
    w.press(Key::Ok);
    w.frames(40);
    assert!(w.on(&AppArg::LibraryCollections { section: 0 }), "premise: the Collections page is up");
    fails.extend(wears_the_library_route(&w, "Collections"));
    w.press(Key::Back);
    w.frames(60);
    w.press(Key::Right);
    w.press(Key::Ok);
    w.frames(40);
    assert!(w.on(&AppArg::LibraryRecommended { section: 0 }), "premise: the Recommended page is up");
    fails.extend(wears_the_library_route(&w, "Recommended"));
    assert!(fails.is_empty(), "\n{}", fails.join("\n"));
}

#[test]
fn opening_collections_again_does_not_stack_a_second_page() {
    let mut w = World::new("r16-twice");
    w.open_library();
    w.climb_into_header();
    let base = w.d.nav.tabs.stack.depth();
    // A double press: the second OK lands while the first push is still on its way.
    w.tap(Key::Ok, 0);
    w.tap(Key::Ok, 0);
    w.frames(80);
    assert!(w.on(&AppArg::LibraryCollections { section: 0 }), "premise: the Collections page is up, not {}", top(&w));
    assert_eq!(w.d.nav.tabs.stack.depth(), base + 1, "two OKs opened one page");
    // One Back is home, and opening it afterwards leaves exactly one page again: nothing of the
    // first visit is left on the stack to be stacked upon.
    for visit in 0..2 {
        w.press(Key::Back);
        w.frames(60);
        assert_eq!((top(&w), w.d.nav.tabs.stack.depth()), ("library", base), "visit {visit}: one Back leaves the page");
        w.press(Key::Ok);
        w.frames(60);
        assert_eq!((top(&w), w.d.nav.tabs.stack.depth()), ("collections", base + 1), "visit {visit}: the page opens once");
    }

    // Landing on the page (`show_page`: the dev `--screen` boot, "Go to ...", the auth landings) is
    // a LANDING and not a push: a page already on top is left alone, one already on the stack is
    // popped to, and only a page that is nowhere is pushed.
    w.press(Key::Back);
    w.frames(60);
    let page = AppArg::LibraryCollections { section: 1 };
    super::show_page(&mut w.d, page.clone());
    w.frames(80);
    let entry = w.d.nav.top_page().map(|e| e.id);
    assert_eq!((top(&w), w.d.nav.tabs.stack.depth()), ("collections", base + 1), "landing on the page pushes it once");
    super::show_page(&mut w.d, page.clone());
    w.frames(80);
    assert_eq!((w.d.nav.top_page().map(|e| e.id), w.d.nav.tabs.stack.depth()), (entry, base + 1), "landing on it again leaves it be");
    super::show_page(&mut w.d, AppArg::Library);
    w.frames(80);
    assert_eq!((top(&w), w.d.nav.tabs.stack.depth()), ("library", base), "landing on the Library pops to it");
    super::show_page(&mut w.d, page);
    w.frames(80);
    assert_eq!((top(&w), w.d.nav.tabs.stack.depth()), ("collections", base + 1), "and the page opens once more, not twice");
}

/// **The Recommended page's first publication is not held by the press that opened it.** OK on the
/// Library header's Recommended control pushes the page from the press's commit, while the press is
/// still springing back (live until it rests). The page's hubs are its whole content and none are
/// committed yet, so nothing is under the finger to protect: the landing is committed on the frame
/// it is asked for, and the page leaves its spinner, instead of waiting out the spring (for ever on
/// the frame dump's frozen clock: crx-f3's dump stalled 120 s on `library-spinner`).
#[test]
fn the_recommended_pages_first_hubs_land_while_the_press_that_opened_it_still_springs() {
    let mut w = World::movies_library_unshelved("rec-first-publication");
    w.walk_header_to(9);
    w.tap(Key::Ok, 0);
    let mut frames = 0;
    while !w.on(&AppArg::LibraryRecommended { section: 0 }) {
        w.frames(1);
        frames += 1;
        assert!(frames < 90, "premise: OK on the Recommended control opened the page, the top page is {}", top(&w));
    }
    assert!(w.d.input.press.is_live(), "premise: the press that opened the page is still springing back");
    assert_ne!(w.bridge.section_hubs.view().publication(), plx_data::stores::browse::Publication::Committed,
        "premise: nothing is committed for the page yet");
    // The worker's answer arrives, staged behind the gate.
    w.bridge.stores.browse.borrow_mut().stage_shelves_for_test(0);
    w.frames(2);
    assert!(w.d.input.press.is_live(), "premise: the press has not rested yet");
    assert_eq!(w.bridge.section_hubs.view().publication(), plx_data::stores::browse::Publication::Committed,
        "the first hubs are committed while the press springs, so the page leaves its spinner");
}

// ---- the focus fingerprint the Recommended page prints ---------------------------------------------

/// The one `focus route=library` grammar (`region= row= col=`) both Library pages answer, which the
/// fps scenes read: #843 let the Recommended page answer the empty line, so `library-shelves-deep`
/// waited for a `region=grid` the page never printed and failed on every run.
#[test]
fn the_recommended_page_prints_the_librarys_fingerprint_on_a_shelf_card() {
    let mut w = World::recommended("rec-fingerprint");
    let probe = |w: &World| content_probe(&w.d, &w.bridge);
    let on_a_card = probe(&w);
    assert!(on_a_card.contains(" region=shelf row=0 col=0 "), "the page seats its first shelf card: {on_a_card}");
    assert!(on_a_card.contains(" card=1 menu=0 "), "a published card is focused: {on_a_card}");
    assert!(on_a_card.contains(" sid=") && !on_a_card.contains(" rk=-"), "it names the card's server and ratingKey: {on_a_card}");
    w.press(Key::Down);
    let lower = probe(&w);
    assert!(lower.contains(" region=shelf row=1 col="), "Down moves the fingerprint to the next shelf: {lower}");
    w.press(Key::Up);
    w.press(Key::Up);
    let crumb = probe(&w);
    assert!(crumb.contains(" region=crumb row=-1 col=-1 ") && crumb.contains(" card=0 "),
        "the crumb row is a region of its own: {crumb}");
}
