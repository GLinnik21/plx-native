//! Query lifecycle: initial scope, debounce coalescing, min-query gating, and reset.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn initial_query_scope_comes_from_the_retained_directory() {
    let _guard = crate::testlock::serial();
    reset();
    let sid = ServerId::from_raw(3);
    let directory = crate::stores::browse::DirectorySnapshot::fixture(9, 0, vec![
        crate::stores::browse::SectionView {
            borrowed: false,
            sid: Some(sid),
            key: 41,
            kind: crate::stores::browse::SecKind::Movie,
            row: crate::stores::browse::SrcRow {
                section: 0, title: "Retained".into(), pinned: true, current: true,
                ..Default::default()
            },
        },
    ]);

    run_with_directory(
        crate::stores::search::SearchCmd::SetQuery("wallace".into()),
        directory.view(),
    );

    assert_eq!(FAV_GEN.load(Ordering::SeqCst), directory.view().sections_gen());
    assert_eq!(favs(), directory.view().favorite_sections());
    reset();
}

#[test]
fn equal_generation_browse_owners_replace_the_favourite_snapshot() {
    let _guard = crate::testlock::serial();
    reset();
    let sid = ServerId::from_raw(3);
    let directory = |key, title: &str| crate::stores::browse::DirectorySnapshot::fixture(
        9,
        0,
        vec![crate::stores::browse::SectionView {
            borrowed: false,
            sid: Some(sid),
            key,
            kind: crate::stores::browse::SecKind::Movie,
            row: crate::stores::browse::SrcRow {
                section: 0,
                title: title.into(),
                pinned: true,
                current: true,
                ..Default::default()
            },
        }],
    );
    let alpha = directory(41, "Alpha");
    let beta = directory(42, "Beta");
    assert_eq!(alpha.view().sections_gen(), beta.view().sections_gen(),
        "the regression requires equal owner-local generations");
    set_query_from_directory("wallace", alpha.view());
    let first_gen = GEN.load(Ordering::SeqCst);

    pump_with_directory(0.0, beta.view());

    assert_eq!(favs(), beta.view().favorite_sections(),
        "the resident query must adopt the second owner's favourites");
    assert_ne!(GEN.load(Ordering::SeqCst), first_gen,
        "the result generation projected under Alpha must be superseded");
    reset();
}

/// [`terms`] is THE predicate — the store's own gate and the one the owned `screens::search::mod.rs` asks — so it is
/// graded here rather than at each screen that used to re-spell it. Two properties, both of
/// which a re-spelling has got wrong: the trim (the FIELD's spaces are not part of what is being
/// looked for) and the floor counted in CHARACTERS, since a two-letter Cyrillic query is four
/// bytes and a `len()` test would have let a ONE-letter one through to a server that answers
/// every hub empty.
#[test]
fn terms_trims_and_counts_characters_not_bytes() {
    assert_eq!(terms("wa"), Some("wa"), "exactly MIN_QUERY is a real query");
    assert_eq!(terms("  wallace  "), Some("wallace"));
    assert_eq!(
        terms("w"),
        None,
        "one character costs a round trip and returns nothing"
    );
    assert_eq!(terms(" w "), None, "…and padding it does not make it one");
    assert_eq!(terms("   "), None);
    assert_eq!(terms(""), None);
    assert_eq!(
        terms("к"),
        None,
        "one letter, two bytes — a byte count would have asked"
    );
    assert_eq!(terms("ко"), Some("ко"));
}

/// **A control byte in a seeded query never reaches the store**, and NUL is the one that
/// bites: it survives every `String` operation on the way in from a boot trigger's file, and
/// `CString::new` refuses it at the far end — so the field's run and the empty statement both
/// blank over a query the app still believes it is holding. Filtered at [`set_query`], the one
/// entrance to the query, rather than at a seeding CALLER: the owned Search screen seeds
/// through `stores::search::SearchCmd`, so a filter living at the legacy mount would have been
/// deleted with it (legacy `a_control_byte_in_the_seed_never_reaches_the_query`).
#[test]
fn a_control_byte_in_a_seeded_query_never_reaches_the_store() {
    let _g = fresh();
    set_query("wal\0lace");
    assert_eq!(query(), "wallace");
    assert!(
        std::ffi::CString::new(query()).is_ok(),
        "…so every run drawn from it can be drawn at all"
    );
    set_query("a\tb\nc");
    assert_eq!(query(), "abc", "a hand-edited file's tab or newline is not a query");
    set_query("wallace ");
    assert_eq!(
        query(),
        "wallace ",
        "an ordinary trailing space is the FIELD's own text and survives"
    );
}

/// The query state machine: below `MIN_QUERY` nothing is asked at all, at it the screen goes
/// pending, and a new query drops the old answer at once rather than leaving results for a
/// string that is no longer on screen.
#[test]
fn set_query_gates_on_min_query_and_drops_the_previous_answer() {
    let _g = fresh();
    register(1); // slot 0 has to be in the live window for the seeded answer below to be read
    set_query("w");
    assert_eq!(
        state(),
        State::Idle,
        "one character returns every hub empty — asking is pure latency"
    );
    assert!(!settling(), "and nothing is owed a fetch");

    set_query("wa");
    assert_eq!(state(), State::Searching);
    assert!(settling());
    assert_eq!(query(), "wa");

    // seed an answer the honest way, then type on: it must not survive into the next query
    unsafe { (*addr_of_mut!(SRC))[0] = answered(0, vec![media("A Close Shave")]) };
    rebuild();
    assert_eq!(shelves().len(), 1);
    set_query("wal");
    assert!(
        shelves().is_empty(),
        "results for a string that is no longer on screen"
    );
    assert_eq!(state(), State::Searching);
    assert_eq!(
        unsafe { (*addr_of!(SRC))[0].status },
        Status::Pending,
        "every source is asked again"
    );

    // …and back below the floor is Idle again, not a search that never answers
    set_query("w");
    assert_eq!(state(), State::Idle);
    assert!(!settling());
    reset();
}

/// The field draws the query VERBATIM, but the server is asked the trimmed one — so pressing
/// the space bar must repaint without superseding an answer that is still correct. Getting this
/// wrong re-asks the whole roster for identical terms on every trailing keystroke.
#[test]
fn a_trailing_space_repaints_but_does_not_re_ask() {
    let _g = fresh();
    register(1); // slot 0 has to be in the live window for the seeded answer below to be read
    set_query("wallace");
    unsafe { (*addr_of_mut!(SRC))[0] = answered(0, vec![media("A Close Shave")]) };
    rebuild();
    let gen = GEN.load(Ordering::SeqCst);

    set_query("wallace ");
    assert_eq!(query(), "wallace ", "the FIELD draws what was typed");
    assert_eq!(
        GEN.load(Ordering::SeqCst),
        gen,
        "the same terms are not a new search"
    );
    assert_eq!(
        shelves().len(),
        1,
        "the answer is still correct — it must not be dropped"
    );

    set_query("wallace g");
    assert_ne!(
        GEN.load(Ordering::SeqCst),
        gen,
        "different terms ARE a new search"
    );
    assert!(shelves().is_empty());
    reset();
}

/// Typing coalesces into ONE fetch: the accumulator restarts on every keystroke and only
/// releases the fetch once the query has held still for `SETTLE_S`. Without it a five-letter
/// word costs four round trips per source.
#[test]
fn the_debounce_coalesces_a_burst_of_keystrokes_into_one_fetch() {
    let _g = fresh();
    set_query("wa");
    assert!(settling());
    pump(SETTLE_S * 0.6);
    assert!(settling(), "still inside the settle window");
    set_query("wal"); // another keystroke restarts it
    pump(SETTLE_S * 0.6);
    assert!(
        settling(),
        "the fetch is owed to the LAST keystroke, not the first of the burst"
    );
    pump(SETTLE_S * 0.6);
    assert!(!settling(), "the query held still — now it may be asked");
    // and it stays released: the accumulator must not re-arm itself frame after frame
    pump(0.016);
    assert!(!settling());
    reset();
}

/// A landing for the query you have already typed past must not repopulate the one on screen —
/// and it must still release the single-flight claim while being dropped, or that source can
/// never search again.
#[test]
fn a_landing_from_the_previous_query_is_discarded_but_still_releases_the_fetch() {
    let _g = fresh();
    register(1);
    set_query("wal");
    let stale = GEN.load(Ordering::SeqCst);
    set_query("wallace"); // supersedes: the fetch above is now about a string nobody typed

    IN_FLIGHT[0].store(true, Ordering::SeqCst);
    hold_off();
    land(0, stale, Some(answered(0, vec![media("Wallander")]).items));
    assert!(!pump(0.0), "a superseded landing must not publish");
    assert!(
        shelves().is_empty(),
        "the previous query's results leaked in"
    );
    assert_eq!(
        state(),
        State::Searching,
        "a discarded landing must not settle the spinner"
    );
    assert!(
        !IN_FLIGHT[0].load(Ordering::SeqCst),
        "the take must release the single-flight even for a landing it drops"
    );
    // …and it must not arm a backoff either, which would delay the CURRENT query's first answer
    assert_eq!(
        unsafe { (*addr_of!(SRC))[0].retry_cd },
        RETRY_FRAMES - 1,
        "only the sentinel ticked"
    );

    let fresh_gen = GEN.load(Ordering::SeqCst);
    land(
        0,
        fresh_gen,
        Some(answered(0, vec![media("A Close Shave")]).items),
    );
    hold_off();
    assert!(
        pump(0.0),
        "the landing for the query ON SCREEN is a change the screen must see"
    );
    assert_eq!(titles(&shelves()[0]), ["A Close Shave"]);
    assert_eq!(state(), State::Ready);
    crate::plex::reset_servers_for_test();
    reset();
}

/// A screen that can never be answered must not spin forever. With no server registered nothing
/// can land, so a `State` recomputed only on a landing would sit on `Searching` for good — and
/// this is reachable: `/tmp/plxnative-search=<q>` forces the route whether or not a server was
/// ever installed.
#[test]
fn an_empty_roster_settles_instead_of_spinning_forever() {
    let _g = fresh();
    set_query("wallace");
    assert_eq!(
        state(),
        State::Searching,
        "the query starts out pending, as typed"
    );
    assert_eq!(nsrc(), 0, "no server can ever answer it");
    assert!(
        pump(SETTLE_S * 2.0),
        "the verdict changed, so the screen must repaint"
    );
    assert_eq!(state(), State::Failed);
    assert!(!pump(0.016), "…and it is not news a second time");
    reset();
}

/// `reset` (an account switch) and `supersede` (a keystroke) both drop the mailboxes — and a
/// single-flight claim is cleared ONLY by a take, so they must clear every one themselves or
/// the next query never fetches. The `browse.rs` latch, once per source.
#[test]
fn reset_clears_every_claim_backoff_and_answer() {
    let _g = fresh();
    set_query("wallace");
    for i in 0..NSRC {
        IN_FLIGHT[i].store(true, Ordering::SeqCst);
        unsafe {
            let s = &mut (*addr_of_mut!(SRC))[i];
            *s = answered(0, vec![media("A Close Shave")]);
            s.retry_cd = RETRY_FRAMES;
        }
    }

    reset();

    for i in 0..NSRC {
        assert!(
            !IN_FLIGHT[i].load(Ordering::SeqCst),
            "source {i} stayed latched — the screen wedges"
        );
        assert_eq!(unsafe { (*addr_of!(SRC))[i].retry_cd }, 0);
        assert_eq!(
            unsafe { (*addr_of!(SRC))[i].status },
            Status::Pending,
            "source {i} kept the last account's answer"
        );
    }
    assert!(query().is_empty() && shelves().is_empty());
    assert_eq!(state(), State::Idle);
    assert!(!settling());
}
