//! Account/profile slot scoping and favourite-edit re-arming of a resident query.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// **The fan-out is the exact live-id set, not a prefix of the mailboxes.** Slot numbers are
/// permanent and a sign-out retires the departing account's without renumbering what registers
/// after them, so the `0..server_count()` this used to walk would have asked two slots that
/// resolve to no client at all and never asked the server the user had just signed in to: a
/// Search screen that could only ever report failure, for every account after the first.
#[test]
fn signing_into_a_second_account_searches_its_slots_and_not_the_retired_ones() {
    let _g = fresh();
    register(2);
    assert_eq!(slots(), vec![0, 1]);

    crate::plex::revoke_all();
    assert!(
        slots().is_empty(),
        "there is nothing to ask while signed out"
    );
    assert_eq!(nsrc(), 0);
    assert!(
        live_sources(&slots()).is_empty(),
        "and the retired records reach neither the merge nor the verdict"
    );

    let sid = crate::plex::register_for_test(
        "search-test-next",
        "127.0.0.1",
        1,
        "tok",
        "cid-search-test",
    );
    assert_eq!(
        sid.raw(),
        2,
        "the next account gets a fresh slot, above the retired ones"
    );
    assert_eq!(
        slots(),
        vec![2],
        "and the live id follows it rather than starting at 0"
    );
    assert_eq!(nsrc(), 1);
    assert_eq!(live_sources(&slots()).len(), 1);
}

#[test]
fn a_profile_hole_searches_exact_live_ids_and_supersedes_the_previous_profiles_answers() {
    let _g = fresh();
    register(3);
    hold_off();
    unsafe {
        *addr_of_mut!(QUERY) = Some(Arc::from("wallace"));
        (*addr_of_mut!(SRC))[0].status = Status::Answered;
        *addr_of_mut!(SHELVES) = Some(Arc::new(vec![Shelf {
            kind: Kind::Movie,
            items: vec![media("old-profile")],
        }]));
        *addr_of_mut!(STATE) = State::Ready;
    }

    crate::plex::revoke_for_profile_switch();
    let restored = crate::plex::register_for_test(
        "search-test-2",
        "127.0.0.1",
        1,
        "new",
        "cid-search-test",
    );
    assert_eq!(restored.raw(), 2);
    assert_eq!(
        slots(),
        vec![0, 2],
        "the inactive middle share is not replaced by a range"
    );

    assert!(
        pump(0.0),
        "membership changed the result state and therefore repaints"
    );
    assert_eq!(state(), State::Searching);
    assert!(
        shelves().is_empty(),
        "old-profile tiles disappear before a new source lands"
    );
    let live = live_sources(&slots());
    assert_eq!(live.len(), 2);
    assert!(
        live.iter().all(|s| s.status == Status::Pending),
        "old-profile answers were superseded"
    );
}

/// **A favourite edit under a resident query supersedes it and re-arms the fetch.** Search
/// watched the ROSTER generation alone and the favourite answer moves without it — discovery
/// appending a library, and `apply_pins` recording an edit, both bump `SECTIONS_GEN`, and this
/// screen deliberately runs discovery immediately before its own pump.
///
/// It re-ARMS rather than re-sorting, and that is not a preference: after the fold a `TagHit`
/// no longer carries the section its bit came from (`TagHit::fav`), so the bit cannot be
/// re-derived from what is on screen. Re-arming is cheaper than retaining full
/// contributing-section provenance through both folds.
#[test]
fn a_favourite_edit_supersedes_a_resident_query_and_re_arms_it() {
    let _g = fresh();
    let _t = crate::plex::session::TempSession::new("search-favgen");
    _t.watching("u-search-favgen");
    register(1);
    let stores = crate::stores::Stores::default();
    let mut directory = crate::stores::browse::DirectorySnapshot::default();
    stores.capture_browse(&mut directory);
    set_query_from_directory("wallace", directory.view());
    hold_off();
    pump_with_directory(SETTLE_S + 0.1, directory.view()); // settles and takes the snapshot's generation
    let gen0 = GEN.load(Ordering::SeqCst);
    unsafe { *addr_of_mut!(ARMED) = false };

    // …a library lands, which is what `apply_pins` and discovery both look like from here
    stores.browse.borrow_mut().seed_two_source_table_for_test();
    stores.capture_browse(&mut directory);
    hold_off();
    pump_with_directory(0.016, directory.view());
    assert_ne!(
        GEN.load(Ordering::SeqCst),
        gen0,
        "the resident answer was superseded, so a landing under the old table is discarded"
    );
    assert!(
        unsafe { *addr_of!(ARMED) },
        "…and the query is owed a fresh fetch rather than left on stale shelves"
    );
    assert_eq!(
        FAV_GEN.load(Ordering::SeqCst),
        directory.view().sections_gen(),
        "the snapshot moved with it"
    );

    // …and a SECOND pump with nothing further changed must not re-arm again, or every frame
    // after any edit would supersede the query it just started
    hold_off();
    let gen1 = GEN.load(Ordering::SeqCst);
    pump_with_directory(0.016, directory.view());
    assert_eq!(GEN.load(Ordering::SeqCst), gen1, "it settles");
}

/// The other half, and the one that costs nothing to get wrong until a user types: with NO
/// query resident there is nothing to invalidate, so the snapshot is merely brought up to date.
/// Superseding here would mean the first query after any library landing opened by discarding
/// itself.
#[test]
fn a_favourite_edit_with_no_query_resident_only_refreshes_the_snapshot() {
    let _g = fresh();
    let _t = crate::plex::session::TempSession::new("search-favgen-idle");
    _t.watching("u-search-favgen-idle");
    register(1);
    let stores = crate::stores::Stores::default();
    let mut directory = crate::stores::browse::DirectorySnapshot::default();
    stores.capture_browse(&mut directory);
    hold_off();
    pump_with_directory(0.016, directory.view());
    let gen0 = GEN.load(Ordering::SeqCst);

    stores.browse.borrow_mut().seed_two_source_table_for_test();
    stores.capture_browse(&mut directory);
    hold_off();
    pump_with_directory(0.016, directory.view());
    assert_eq!(
        GEN.load(Ordering::SeqCst),
        gen0,
        "nothing was resident, so nothing was superseded"
    );
    assert_eq!(
        FAV_GEN.load(Ordering::SeqCst),
        directory.view().sections_gen(),
        "…but the snapshot is current, so the next query does not open by re-arming"
    );
}
