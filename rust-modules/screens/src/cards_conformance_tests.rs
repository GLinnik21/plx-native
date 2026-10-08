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
    ]
}

/// In-scope screens whose harness is not built yet; the registry test keeps the two lists whole.
const PENDING: &[&str] = &[];
const IN_SCOPE: &[&str] = &["collection", "person", "search", "library_grid", "library_shelves", "home", "detail_shelves", "detail_collection", "detail_extras", "detail_cast"];

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
