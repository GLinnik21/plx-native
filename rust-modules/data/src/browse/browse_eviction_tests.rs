//! Library grid eviction: `SecItems` keeps a few pages of a long listing loaded, and every index
//! stays reachable because an evicted page is a hole that is fetched again when wanted.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use crate::stores::page_cache::{Keep, MAX_LOADED};

const TOTAL: usize = 20_000;

fn item(i: usize) -> PmsMovie {
    PmsMovie {
        sid: super::ServerId::UNSET,
        rk: format!("{}", i + 1),
        title: format!("Item {i}"),
        ..Default::default()
    }
}

/// The rows a server would return for page `page` of a `TOTAL`-item listing.
fn page_rows(page: usize) -> Vec<PmsMovie> {
    let start = page * PAGE;
    (start..(start + PAGE).min(TOTAL)).map(item).collect()
}

fn keep(wanted: std::ops::Range<usize>, restore: Option<usize>) -> Keep {
    Keep { wanted, focus: None, restore }
}

/// Pages holding rows, as the store sees them.
fn loaded(items: &SecItems) -> Vec<usize> {
    (0..items.pages.len()).filter(|&p| items.pages[p].is_some()).collect()
}

fn listing() -> SecItems {
    let mut items = SecItems::default();
    items.resize(TOTAL);
    items
}

#[test]
fn loaded_pages_stay_at_most_eight_across_a_full_walk() {
    let mut items = listing();
    // The grid's wanted window moves a screen at a time from the top to the end; each step lands
    // the page the window asks for, as `pump_owned_with_gate` does.
    for page in 0..TOTAL.div_ceil(PAGE) {
        let start = page * PAGE;
        items.land(start, page_rows(page), &keep(start..start + 12, None));
        assert!(
            loaded(&items).len() <= MAX_LOADED,
            "page {page} landed and {} pages stay loaded",
            loaded(&items).len()
        );
    }
    assert_eq!(items.len(), TOTAL, "eviction never shrinks the listing");
    assert!(items.get(TOTAL - 1).is_some(), "the last page is the one just landed");
    assert!(items.get(0).is_some(), "page 0 is not on screen at the end, but it is always kept");
}

#[test]
fn wanted_range_moved_back_reports_holes_and_refills_at_the_same_indices() {
    let mut items = listing();
    for page in 0..TOTAL.div_ceil(PAGE) {
        let start = page * PAGE;
        items.land(start, page_rows(page), &keep(start..start + 12, None));
    }
    let middle = 150 * PAGE;
    assert!(items.page_missing(150), "the middle page was evicted by the walk");
    assert!(items.get(middle + 5).is_none());

    items.land(middle, page_rows(150), &keep(middle..middle + 12, None));
    for offset in [0, 1, PAGE - 1] {
        let got = items.get(middle + offset).expect("the landed page reads back");
        assert_eq!(got.title, format!("Item {}", middle + offset));
        assert_eq!(got.rk, format!("{}", middle + offset + 1));
    }
    assert!(!items.page_missing(150));
    assert!(items.get(middle + PAGE).is_none(), "the next page was never asked for");
}

#[test]
fn page_zero_and_the_restore_target_survive_eviction() {
    let mut items = listing();
    for page in 0..10 {
        items.set_page_for_test(page);
    }
    let restore = 4 * PAGE + 5;
    items.evict(&keep(540..552, Some(restore)));
    assert_eq!(
        loaded(&items),
        vec![0, 4, 7, 8, 9],
        "page 0, the restore page, and 9 with two pages either side"
    );
    assert!(items.get(restore).is_some(), "the restore target still reads");
    assert!(items.get(0).is_some());
}

#[test]
fn jump_to_a_deep_index_loads_its_neighbourhood_with_indices_unchanged() {
    let mut items = listing();
    // Scroll part of the way down first, so the jump has pages of its own to evict.
    for page in 0..=20 {
        let start = page * PAGE;
        items.land(start, page_rows(page), &keep(start..start + 12, None));
    }
    let target = 250 * PAGE + 7;
    for page in 248..=252 {
        let start = page * PAGE;
        items.land(start, page_rows(page), &keep(target - 30..target + 30, None));
    }
    assert_eq!(items.len(), TOTAL, "a jump does not change the listing's length");
    let held = loaded(&items);
    assert!(held.len() <= MAX_LOADED);
    assert!(
        [248, 249, 250, 251, 252].iter().all(|p| held.contains(p)),
        "the jump's neighbourhood loads: {held:?}"
    );
    assert!(held.contains(&0), "page 0 is always kept");
    assert!(held.iter().all(|&p| p == 0 || p >= 248), "the walk's pages were evicted: {held:?}");
    assert_eq!(items.get(target).map(|m| m.title.as_str()), Some("Item 15007"));
    assert_eq!(items.get(target).map(|m| m.rk.as_str()), Some("15008"));
    assert_eq!(items.get(target - 30).map(|m| m.title.as_str()), Some("Item 14977"));
    assert!(items.get(TOTAL).is_none(), "past the listing is still past it");
}

#[test]
fn an_edit_aimed_at_an_evicted_index_is_a_no_op() {
    let mut items = listing();
    for page in 0..10 {
        items.set_page_for_test(page);
    }
    items.evict(&keep(540..552, None));
    let evicted = 5 * PAGE + 3;
    assert!(items.page_missing(5));
    let rk = format!("{}", evicted + 1);
    assert!(!items.set_watched(super::ServerId::UNSET, &rk, true), "the row is not held");
    assert!(items.page_missing(5), "an edit does not bring the page back");
    assert!(items.get(evicted).is_none());
    assert!(items.set_watched(super::ServerId::UNSET, &format!("{}", 9 * PAGE + 1), true));
}

#[test]
fn the_three_sections_left_most_recently_keep_their_last_window_and_the_rest_keep_no_pages() {
    let _guard = plx_base::testlock::serial();
    let mut state = BrowseState::default();
    seed_two_source_table_for_owner_test(&mut state);
    for key in 3..5 {
        append_section_for_owner_test(&mut state, 0, key, "More", SecKind::Movie);
    }
    assert_eq!(state.sections.len(), 6);
    let window = 9 * PAGE..9 * PAGE + 12;

    // Section 0 is the one the viewer scrolls down. Its pages are loaded while it is current.
    state.set_cur(0);
    state.want(window.start, window.end, None);
    state.states[0].total = TOTAL as i64;
    state.states[0].items.resize(TOTAL);
    for page in 0..10 {
        state.states[0].items.set_page_for_test(page);
    }

    // Leaving it keeps only the pages of the window it last showed, one of the three most recent.
    state.set_cur(1);
    assert_eq!(loaded(&state.states[0].items), vec![9]);
    assert_eq!(state.states[0].items.len(), TOTAL);

    state.want(0, 1, None);
    state.set_cur(2);
    state.set_cur(3);
    assert_eq!(loaded(&state.states[0].items), vec![9], "still one of the last three left");

    // A fourth section left since puts it past the three, which hold no pages but keep their total.
    state.set_cur(4);
    assert!(loaded(&state.states[0].items).is_empty());
    assert_eq!(state.states[0].items.len(), TOTAL, "the total stays, so the section reads holes");
    assert!(state.states[0].items.page_missing(9));
}

/// The focus the grid sends with its want reaches the keep rule of the page that lands: a page far
/// from the wanted rows survives its own landing because the focused slot is on it.
#[test]
fn the_grids_focus_index_reaches_the_landing_keep_rule() {
    let _guard = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("keep-focus", "127.0.0.1", 9, "synthetic", "fixture");
    let client = plx_plex::plex::client_for(sid).unwrap();
    let far = 100;
    let focus = far * PAGE + 3;
    for (sent, kept) in [(None, false), (Some(focus), true)] {
        let mut state = BrowseState::default();
        seed_registered_table_for_owner_test(&mut state, [sid, sid]);
        prepare_page_for_owner_test(&mut state, sid);
        let sec = state.cur();
        state.states[sec].total = TOTAL as i64;
        state.states[sec].items.resize(TOTAL);
        for page in 0..MAX_LOADED {
            state.states[sec].items.set_page_for_test(page);
        }
        state.want(0, 12, sent);
        let adapter = Arc::new(BrowseAdapter::default());
        adapter.fetching.store(true, Ordering::SeqCst);
        *adapter.page_result.lock().unwrap() = Some(PageResult {
            client, token_gen: client.token_gen(), gen: state.query_gen(), sec, start: far * PAGE,
            items: page_rows(far), total: TOTAL as i64, sorts: None, restored: None, genres: None, genre: None,
            resolved: Default::default(),
        });
        assert!(state.pump_owned(&adapter).changed, "the page landed");
        let held = loaded(&state.states[sec].items);
        assert!(!held.is_empty() && held.len() <= MAX_LOADED, "the landing ran and evicted: {held:?}");
        assert_eq!(held.contains(&far), kept, "focus {sent:?}: the landed page's fate");
    }
    plx_plex::plex::reset_servers_for_test();
}

impl SecItems {
    /// Lands page `page` in full, without eviction: the state a test starts from.
    fn set_page_for_test(&mut self, page: usize) {
        for (offset, m) in page_rows(page).into_iter().enumerate() {
            self.set(page * PAGE + offset, m);
        }
    }
}
