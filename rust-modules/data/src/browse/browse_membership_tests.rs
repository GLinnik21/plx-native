//! A read-back that patches a row of a listing whose MEMBERSHIP depends on viewing (the Unwatched
//! switch, ordering by last viewed) marks that section stale; the stale listing is re-run exactly
//! once, on request, and never by the patch itself.

use super::test_support::*;
use super::*;
use crate::viewstate::{ViewAt, ViewEdit};

const WATCHED_FACTS: ViewEdit = ViewEdit::Facts { watched: true, unwatched: false, resume_ms: 0 };

fn resident(sid: ServerId, browse: &mut TestBrowse) {
    let state = browse.state.state_mut(0).unwrap();
    state.fetch = SecFetch::Ready;
    state.total = 1;
    state.items = SecItems::from_vec(vec![Some(PmsMovie { sid, rk: "7".into(), unwatched: true, ..Default::default() })]);
    state.cursor = Some(Arc::new(Cursor { at: CursorAt::SlotIndex(0), scroll: 0.0 }));
}

fn apply(browse: &mut TestBrowse, sid: ServerId, edit: ViewEdit) -> bool {
    browse.state.run_owned(&browse.adapter, crate::stores::browse::BrowseCmd::ApplyView {
        at: ViewAt::leaf(sid, "7"), edit }, &ClientKeys::default(), &mut Vec::new())
}

fn stale(browse: &mut TestBrowse) -> bool {
    browse.state.listing_snapshot().view().membership_stale()
}

#[test]
fn a_facts_patch_in_an_unwatched_filtered_listing_marks_it_stale_without_requerying() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _) = registered_page_source();
    assert!(browse.state.set_unwatched(true));
    resident(sid, &mut browse);
    let gen = browse.state.query_gen();
    assert!(apply(&mut browse, sid, WATCHED_FACTS));
    let row = browse.state.states()[0].items.get(0).unwrap();
    assert!(row.watched && !row.unwatched, "the row is patched where it stands");
    assert!(stale(&mut browse), "…and the listing knows its membership is out of date");
    assert_eq!(browse.state.query_gen(), gen, "the patch itself never re-queries under the cursor");
    assert_eq!(browse.state.states()[0].fetch, SecFetch::Ready);
    assert!(!browse.state.requery_owed());
}

#[test]
fn the_optimistic_press_alone_does_not_mark_a_listing_stale() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _) = registered_page_source();
    assert!(browse.state.set_unwatched(true));
    resident(sid, &mut browse);
    assert!(apply(&mut browse, sid, ViewEdit::Watched(true)));
    assert!(!stale(&mut browse), "only the server's answer says the membership changed");
}

#[test]
fn a_listing_whose_query_ignores_viewing_stays_current() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _) = registered_page_source();
    resident(sid, &mut browse);
    assert!(apply(&mut browse, sid, WATCHED_FACTS));
    assert!(!stale(&mut browse), "ordered by title with no filter: the row stays where it is");
}

#[test]
fn ordering_by_last_viewed_makes_a_facts_patch_stale() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _) = registered_page_source();
    resident(sid, &mut browse);
    browse.state.state_mut(0).unwrap().sorts = Arc::new(vec![SortEntry {
        key: "lastViewedAt".into(), desc_key: "lastViewedAt:desc".into(), title: "Last Viewed".into(), default_desc: true }]);
    assert!(apply(&mut browse, sid, WATCHED_FACTS));
    assert!(stale(&mut browse));
}

#[test]
fn requery_stale_runs_the_query_once_and_keeps_the_readers_place() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _) = registered_page_source();
    assert!(browse.state.set_unwatched(true));
    resident(sid, &mut browse);
    assert!(apply(&mut browse, sid, WATCHED_FACTS));
    let gen = browse.state.query_gen();
    let shown = browse.state.listing_snapshot().view().id().unwrap().query;

    assert!(browse.state.requery_stale());
    assert_eq!(browse.state.query_gen(), gen.wrapping_add(1), "exactly one re-fetch: a late page of the old fetch is dropped");
    assert_eq!(browse.state.listing_snapshot().view().id().unwrap().query, shown,
        "…but the screen-facing query identity stands: a membership refresh is not a query edit");
    assert!(!stale(&mut browse), "the flag is spent");
    let state = &browse.state.states()[0];
    assert_eq!((state.fetch, state.total), (SecFetch::Loading, -1));
    assert!(state.cursor.is_some(), "a membership re-query is not a new query: the reader's place stands");
    assert!(browse.state.requery_owed(), "the first page is owed with no Library page ticking the store");

    assert!(!browse.state.requery_stale(), "a second ask has nothing to do");
    assert_eq!(browse.state.query_gen(), gen.wrapping_add(1));

    browse.state.set_unwatched(false); // a real query edit
    assert_ne!(browse.state.listing_snapshot().view().id().unwrap().query, shown, "a query edit still moves it");
    browse.state.set_unwatched(true);

    browse.state.set_unwatched(false); // the reader does something else: the Library page is ticking again
    assert!(!browse.state.requery_owed());
}

#[test]
fn a_stale_section_that_is_not_showing_forgets_its_pages() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _) = registered_page_source();
    browse.append_sections(0, vec![(2, "Other".into(), SecKind::Movie)]);
    assert!(browse.state.set_unwatched(true));
    resident(sid, &mut browse);
    browse.state.want(0, PAGE, None); // the window the grid last showed, kept as section 0 is left
    browse.state.set_cur(1);
    assert!(apply(&mut browse, sid, WATCHED_FACTS));
    let first = &browse.state.states()[0];
    assert_eq!((first.fetch, first.total), (SecFetch::Loading, -1), "its next visit fetches afresh");
    assert!(!first.membership_stale);
}

#[test]
fn an_agreeing_read_back_still_marks_an_unwatched_filtered_listing_stale() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _) = registered_page_source();
    assert!(browse.state.set_unwatched(true));
    resident(sid, &mut browse);
    assert!(apply(&mut browse, sid, ViewEdit::Watched(true)));
    assert!(!stale(&mut browse), "the press alone says nothing");
    assert!(apply(&mut browse, sid, WATCHED_FACTS), "marking the listing stale is a change the store must publish");
    let row = browse.state.states()[0].items.get(0).unwrap();
    assert!(row.watched && !row.unwatched);
    assert!(stale(&mut browse), "the server's answer agrees with the row, and that answer is what marks staleness");
    assert!(!apply(&mut browse, sid, WATCHED_FACTS), "a second agreeing answer changes nothing");
}

#[test]
fn an_agreeing_read_back_resets_a_stale_section_that_is_not_showing() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _) = registered_page_source();
    browse.append_sections(0, vec![(2, "Other".into(), SecKind::Movie)]);
    assert!(browse.state.set_unwatched(true));
    resident(sid, &mut browse);
    browse.state.want(0, PAGE, None);
    browse.state.set_cur(1);
    assert!(apply(&mut browse, sid, ViewEdit::Watched(true)));
    assert!(apply(&mut browse, sid, WATCHED_FACTS), "the reset is a change the store must publish");
    let first = &browse.state.states()[0];
    assert_eq!((first.fetch, first.total), (SecFetch::Loading, -1), "its next visit fetches afresh");
}

fn listings_changed(browse: &mut TestBrowse, sid: ServerId, library_on_stack: bool) -> bool {
    browse.state.run_owned(&browse.adapter, crate::stores::browse::BrowseCmd::ListingsChanged { sid, library_on_stack },
        &ClientKeys::default(), &mut Vec::new())
}

#[test]
fn created_or_removed_items_mark_the_open_section_stale_while_a_library_page_is_up() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _) = registered_page_source();
    resident(sid, &mut browse);
    let gen = browse.state.query_gen();

    assert!(listings_changed(&mut browse, sid, true), "marking the listing stale is a change the store must publish");
    assert!(stale(&mut browse));
    assert_eq!(browse.state.query_gen(), gen, "nothing is fetched under the reader: the Library page re-queries it when covered");
    let state = &browse.state.states()[0];
    assert_eq!((state.fetch, state.total), (SecFetch::Ready, 1));
    assert!(state.items.get(0).is_some());
    assert!(!browse.state.requery_owed());

    assert!(!listings_changed(&mut browse, sid, true), "a second report changes nothing");
}

#[test]
fn created_or_removed_items_re_query_at_once_when_no_library_page_is_up() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _) = registered_page_source();
    resident(sid, &mut browse);
    let gen = browse.state.query_gen();
    let shown = browse.state.listing_snapshot().view().id().unwrap().query;

    assert!(listings_changed(&mut browse, sid, false));
    assert_eq!(browse.state.query_gen(), gen.wrapping_add(1), "exactly one re-fetch: a late page of the old fetch is dropped");
    assert_eq!(browse.state.listing_snapshot().view().id().unwrap().query, shown, "not a query edit");
    assert!(!stale(&mut browse), "the flag is spent");
    let state = &browse.state.states()[0];
    assert_eq!((state.fetch, state.total), (SecFetch::Loading, -1));
    assert!(state.cursor.is_some(), "nobody is looking, but the reader's place stands for their return");
    assert!(browse.state.requery_owed(), "the first page is owed with no Library page ticking the store");
}

#[test]
fn created_or_removed_items_make_the_servers_other_sections_forget_their_pages() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _) = registered_page_source();
    browse.append_sections(0, vec![(2, "Other".into(), SecKind::Movie)]);
    resident(sid, &mut browse);
    browse.state.set_cur(1);
    assert!(listings_changed(&mut browse, sid, true));
    let first = &browse.state.states()[0];
    assert_eq!((first.fetch, first.total), (SecFetch::Loading, -1), "its next visit fetches afresh");
}

#[test]
fn another_servers_listings_are_left_alone() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _) = registered_page_source();
    resident(sid, &mut browse);
    let other = ServerId::from_raw(sid.raw().wrapping_add(7));
    assert!(!listings_changed(&mut browse, other, false));
    assert!(!listings_changed(&mut browse, other, true));
    assert_eq!(browse.state.states()[0].fetch, SecFetch::Ready);
    assert!(!stale(&mut browse));
}

#[test]
fn a_stale_section_that_is_left_forgets_its_pages_and_a_current_one_keeps_its_window() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _) = registered_page_source();
    browse.append_sections(0, vec![(2, "Other".into(), SecKind::Movie)]);
    resident(sid, &mut browse);
    browse.state.want(0, PAGE, None);
    assert!(listings_changed(&mut browse, sid, true));
    assert!(stale(&mut browse));
    browse.state.set_cur(1);
    let first = &browse.state.states()[0];
    assert_eq!((first.fetch, first.total), (SecFetch::Loading, -1), "coming back fetches afresh, not the list from before the change");
    assert!(first.cursor.is_none() && !first.membership_stale);

    // A section that was current and not stale keeps its recent window exactly as before.
    browse.state.set_cur(0);
    browse.state.want(0, PAGE, None);
    resident(sid, &mut browse);
    browse.state.set_cur(1);
    let first = &browse.state.states()[0];
    assert_eq!((first.fetch, first.total), (SecFetch::Ready, 1), "an up-to-date section left behind keeps its window");
    assert!(first.items.get(0).is_some());
}
