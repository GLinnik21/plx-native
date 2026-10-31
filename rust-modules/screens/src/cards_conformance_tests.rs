//! Tier 2 card-screen conformance (shared-card-sections plan, section 4): the black-box
//! cases of `plx_ui::cards::conformance` run against today's screens. Every failing case is listed
//! in `ci/allow/cards-conformance.txt` with its reason; the list only shrinks. A listed case that
//! now passes fails the suite, as does an unlisted failure. Each screen implements
//! `CardHarness` in a child module of its own file (it reads that screen's private geometry).
use plx_ui::cards::conformance::{check_expected, run_all, Mount, Outcome};

const EXPECTED: &str = include_str!("../../../ci/allow/cards-conformance.txt");

/// Every in-scope card screen with a harness. Collection, Person, Search, Library (grid and
/// shelves), Home and Detail's four shelves (Related is `detail_shelves`) are in scope; the rest are in [`PENDING`].
fn table() -> Vec<(&'static str, Mount)> {
    vec![
        ("collection", super::collection::cards_harness::mount as Mount),
        ("person", super::person::cards_harness::mount as Mount),
        ("search", super::search::cards_harness::mount as Mount),
        ("library_grid", super::library::cards_harness::mount_grid as Mount),
        ("library_shelves", super::library::cards_harness::mount_shelves as Mount),
        ("home", super::home::cards_harness::mount as Mount),
        ("detail_shelves", super::detail::cards_harness::mount as Mount),
        ("detail_collection", super::detail::cards_harness::mount_collection as Mount),
        ("detail_extras", super::detail::cards_harness::mount_extras as Mount),
        ("detail_cast", super::detail::cards_harness::mount_cast as Mount),
        ("watchlist", super::watchlist::cards_harness::mount as Mount),
    ]
}

/// In-scope screens whose harness is not built yet; the registry test keeps the two lists whole.
const PENDING: &[&str] = &[];
const IN_SCOPE: &[&str] = &["collection", "person", "search", "library_grid", "library_shelves", "home", "detail_shelves", "detail_collection", "detail_extras", "detail_cast", "watchlist"];

#[test]
fn every_in_scope_card_screen_is_in_the_table_or_pending() {
    let t = table();
    for s in IN_SCOPE {
        assert!(t.iter().any(|(n, _)| n == s) || PENDING.contains(s), "{s} is in scope but neither tabled nor pending");
    }
    for (n, _) in &t {
        assert!(!PENDING.contains(n), "{n} has a harness: remove it from PENDING");
    }
}

#[test]
fn card_screens_match_the_expected_failure_list() {
    // The cases assume the default Under posters setting (the caption band sizes every row below
    // the focused one), and five of the eleven harnesses read the setting through `captions::policy`
    // from the process-wide cell the Settings tests write: pinned here, for every mount of the run.
    let _policy = crate::registry::captions::PolicyPin::default_setting();
    let matrix = run_all(&table());
    for (s, c, o) in &matrix {
        eprintln!("{s:12} {c:18} {}", match o {
            Outcome::Pass => "pass".into(),
            Outcome::Fail(m) => format!("FAIL {m}"),
            Outcome::Unsupported(w) => format!("unsupported: {w}"),
        });
    }
    let complaints = check_expected(&matrix, EXPECTED);
    assert!(complaints.is_empty(), "\n{}", complaints.join("\n"));
}

/// Focus never moves under the user, in any interleaving: cards arriving before every card shown in
/// the same tick as a focus change keep the view in place, on every screen whose harness can stage
/// the arrival (`CardHarness::shift`). The Search result row, the Library section hubs and the
/// Library All grid must; the rest report `Unsupported` until their harness can shift.
#[test]
fn cards_arriving_in_the_tick_of_a_focus_change_keep_the_view_in_place() {
    use plx_ui::cards::conformance::shifted_under_a_focus_change;
    let _guard = plx_base::testlock::serial();
    let _policy = crate::registry::captions::PolicyPin::default_setting();
    let mut bad = Vec::new();
    for (screen, mount) in table() {
        let outcome = shifted_under_a_focus_change(mount);
        eprintln!("{screen:18} {outcome:?}");
        let must = ["search", "library_shelves", "library_grid"].contains(&screen);
        match outcome {
            Outcome::Pass => {}
            Outcome::Unsupported(_) if !must => {}
            other => bad.push(format!("{screen}: {other:?}")),
        }
    }
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}

/// **No tabled harness follows the process-wide Under posters cell while the pin is held.** Five of
/// the eleven harnesses (collection, home, person, search, library) read the setting through
/// `captions::policy` and none pins it itself; the Settings tests write the cell under
/// `testlock::serial()`. This holds the cell on a two-line value (112 px band against the default's
/// 80) while each harness is mounted and stepped, with the write repeated under the mounted page as
/// a Settings test's next write would be, and grades that a lower card stands where the default puts
/// it. Without the pin the policy-reading harnesses move by the 32 px between the bands.
#[test]
fn every_card_harness_follows_the_pin_not_the_live_setting() {
    use plx_plex::plex::session::{UnderPosters, UnderPostersDetails};
    use plx_ui::screen::By;
    /// Puts the live cell back on its default, a panic included.
    struct Live;
    impl Drop for Live {
        fn drop(&mut self) {
            plx_media::route::restore_under_posters(UnderPosters::DEFAULT);
        }
    }
    let _serial = plx_base::testlock::serial();
    let _live = Live;
    let _policy = crate::registry::captions::PolicyPin::default_setting();
    let lower_card_y = |live: UnderPosters, mount: Mount| {
        plx_media::route::restore_under_posters(live);
        let mut h = mount(14);
        let cards = h.cards();
        h.focus(cards[2.min(cards.len() - 1)], By::Dir);
        h.tick(60);
        plx_media::route::restore_under_posters(live);
        h.tick(1);
        h.drawn_rect(cards[7.min(cards.len() - 1)], 1.0).map(|rect| rect.y)
    };
    let two_lines = UnderPosters { title: true, details: UnderPostersDetails::Genre, sort_value: false };
    let mut moved = Vec::new();
    for (screen, mount) in table() {
        let (default, held) = (lower_card_y(UnderPosters::DEFAULT, mount), lower_card_y(two_lines, mount));
        if default != held {
            moved.push(format!("{screen}: {default:?} with the default, {held:?} with two lines held live"));
        }
    }
    assert!(moved.is_empty(), "\n{}", moved.join("\n"));
}
