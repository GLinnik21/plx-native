//! The poster store's half of the two app-baked deck kinds: how their keys are built, which slot
//! the one-cover-in-flight rule hands a worker, what survives a slot's recycling (the session miss
//! set), and the worker I/O's currency rules. The bake orchestrations themselves are graded with
//! fake I/O in `cover/tests.rs` and `card/tests.rs`.
use super::cover::CoverIo;
use super::miss::{self, Currency, MissSet};
use super::tests::{key_for, one_server};
use super::workers::{WorkerCardIo, WorkerCoverIo};
use super::*;
use plx_plex::plex::artwork::{card_path, cover_parts, card_parts, cover_path};

const HEX: &str = "5d776824999c4a001f9a1b2c";
const ART: &str = "/library/metadata/605/art/1778546279";

fn cover_key() -> String {
    cover_path(HEX).unwrap()
}
fn card_key() -> String {
    card_path("605", ART).unwrap()
}

fn clear_store() {
    store().slots = [Pslot::ZERO; PT_CAP];
}

// ── keys ────────────────────────────────────────────────────────────────────────────────────────

/// A cover or a composite names a recipe, not a server path: the slot key IS the synthetic key,
/// whatever box the consumer draws it in, with no token and well inside a slot.
#[test]
fn the_baked_keys_are_used_as_they_stand_carry_no_token_and_fit_a_slot() {
    let (_g, sid, tok) = one_server();
    for (key, parse_ok) in [(cover_key(), cover_parts(&cover_key()).is_some()), (card_key(), card_parts(&card_key()).is_some())] {
        assert!(parse_ok);
        let built = key_for(sid, &key, 420, 236, 0);
        assert_eq!(built, key, "the key is the slot key");
        assert_eq!(key_for(sid, &key, 840, 472, 0), key, "every box shares one bake");
        assert!(!built.contains("X-Plex-Token") && !built.contains(tok) && !built.contains('?'), "{built}");
        assert!(built.len() <= KEY_MAX && is_fetchable(&built));
    }
    assert!(baked_prefix(&cover_key()) && baked_prefix(&card_key()));
    assert!(is_baked_key(&cover_key()) && is_baked_key(&card_key()));
}

#[test]
fn a_new_logo_stamp_changes_both_the_transcode_and_composite_cache_keys() {
    let (_g, sid, _) = one_server();
    let client = plx_plex::plex::client_for(sid).unwrap();
    let identity = |logo| {
        let request = logo_key(sid, logo).unwrap();
        let transcode = plx_platform::imgcache::classify(&disk_namespace(client), &request).unwrap();
        let composite = card_path(logo, ART).unwrap();
        assert_eq!(built_key(sid, &composite, 420, 236, false).as_deref(), Some(composite.as_str()));
        let (logo, art) = card_parts(&composite).unwrap();
        let io = WorkerCardIo::new(client, logo, art, 0, client.token_gen());
        (request, transcode, io.disk.unwrap())
    };
    let old = identity("/library/metadata/605/clearLogo/1778546279");
    let new = identity("/library/metadata/605/clearLogo/1778546280");
    assert!(new.0.contains("%2FclearLogo%2F1778546280&"));
    assert!(new.0.contains("&format=png&"));
    assert_ne!(old.0, new.0);
    assert_ne!(old.1, new.1);
    assert_ne!(old.2, new.2);
    assert!(logo_key(sid, "").is_none(), "a removed logo must not reuse a legacy hit");
}

/// A card key longer than a slot is refused rather than truncated, like any other (a truncated key
/// never equals its probe again).
#[test]
fn a_baked_key_longer_than_a_slot_is_refused() {
    let (_g, sid, _) = one_server();
    let long = format!("/library/metadata/605/art/{}", "9".repeat(KEY_MAX));
    let key = card_path("605", &long).expect("the builder does not bound it; the store does");
    assert!(key.len() > KEY_MAX);
    assert_eq!(key_for(sid, &key, 420, 236, 0), "");
}

/// Near-misses are not baked keys: they go the ordinary transcode route, as any path would.
#[test]
fn a_near_miss_is_an_ordinary_transcode_not_a_bake() {
    let (_g, sid, _) = one_server();
    for near in ["/plx/cover/ZZ", "/plx/cover/", "/plx/card/605", "/plx/cover/5D776824999C4A001F9A1B2C"] {
        assert!(!is_baked_key(near), "{near}");
        let k = key_for(sid, near, 420, 236, 0);
        assert!(k.starts_with("/photo/:/transcode?"), "{near}: {k}");
    }
}

// ── one cover in flight ─────────────────────────────────────────────────────────────────────────

fn want(slots: &mut [Pslot; PT_CAP], i: usize, key: &str, state: c_int, frame: c_uint) {
    let s = &mut slots[i];
    s.state = state;
    s.visible = true;
    s.frame = frame;
    s.use_ = i as c_uint;
    set_key(s, key);
}

#[test]
fn a_second_cover_waits_while_one_is_loading_and_posters_are_served() {
    let other = cover_path("0123456789abcdef01234567").unwrap();
    let mut slots = [Pslot::ZERO; PT_CAP];
    want(&mut slots, 0, &cover_key(), P_LOADING, 5);
    want(&mut slots, 1, &other, P_WANT, 9); // the newest drawn demand
    want(&mut slots, 2, "/photo/:/transcode?url=%2Fa%2Fthumb", P_WANT, 4);
    assert_eq!(next_wanted(&slots), Some(2), "the poster is served, not a second cover");
    slots[2].state = P_READY;
    assert_eq!(next_wanted(&slots), None, "and with nothing else, the second cover keeps waiting");
    // Speculation is gated the same way.
    slots[1].visible = false;
    assert_eq!(next_wanted(&slots), None);
}

#[test]
fn with_no_cover_loading_a_cover_is_claimed_like_any_other_art() {
    let mut slots = [Pslot::ZERO; PT_CAP];
    want(&mut slots, 0, &cover_key(), P_WANT, 9);
    want(&mut slots, 1, "/photo/:/transcode?url=%2Fa%2Fthumb", P_WANT, 4);
    assert_eq!(next_wanted(&slots), Some(0), "the newest drawn demand first, as before");
    slots[0].state = P_LOADING;
    assert_eq!(next_wanted(&slots), Some(1));
    // A composite is LAN-only and is never held back.
    let mut slots = [Pslot::ZERO; PT_CAP];
    want(&mut slots, 0, &cover_key(), P_LOADING, 5);
    want(&mut slots, 1, &card_key(), P_WANT, 9);
    assert_eq!(next_wanted(&slots), Some(1));
}

// ── the session miss set ────────────────────────────────────────────────────────────────────────

/// A settled miss stays one after its slot is recycled (F9): `settled_failed` answers from the set,
/// and a draw or a warm does not claim the key again.
#[test]
fn the_miss_set_keeps_a_settled_miss_failed_after_its_slot_is_recycled() {
    let (_g, sid, _) = one_server();
    miss::reset_for_test();
    clear_store();
    let key = key_for(sid, &cover_key(), 420, 236, 0);
    assert!(!settled_failed(sid, &key), "never requested is not failed");

    // The bake answered Final: the worker records it (and the slot parks at P_FAILED).
    miss::note(sid, &key, Currency::live(sid));
    // The slot was recycled: nothing in the store names the key.
    assert!(store().slots.iter().all(|s| s.state == P_EMPTY));
    assert!(settled_failed(sid, &key), "the set still says it failed");
    assert!(art_failed(sid, &cover_key(), 420, 236, false), "through the source's `failed` peek too");

    // And it is not asked for again: no slot is claimed by a draw's twin or a warm.
    for touch in [Touch::Warm, Touch::Ahead] {
        let (hit, warm) = lookup(sid, &key, touch);
        assert!(hit.is_none() && matches!(warm, Warm::Known), "{}", baked_prefix(&key));
    }
    assert!(store().slots.iter().all(|s| s.state == P_EMPTY), "no slot was claimed");

    // A composite's miss is latched the same way, and per server.
    let card = key_for(sid, &card_key(), 420, 236, 0);
    miss::note(sid, &card, Currency::live(sid));
    assert!(settled_failed(sid, &card));
    assert!(!settled_failed(ServerId::from_raw(sid.raw() + 1), &card), "another server's key is its own");
    miss::reset_for_test();
}

/// The set is only as good as the identity it was recorded under: a profile switch retokens the
/// same client in place (it moves `token_gen`, not `grant_epoch`), and the next profile asks again.
#[test]
fn the_miss_set_stops_counting_when_the_clients_token_generation_moves() {
    let (_g, sid, _) = one_server();
    miss::reset_for_test();
    clear_store();
    let key = key_for(sid, &cover_key(), 420, 236, 0);
    miss::note(sid, &key, Currency::live(sid));
    assert!(settled_failed(sid, &key));

    plx_plex::plex::client_for(sid).unwrap().set_token("tok-next-profile");
    assert!(!settled_failed(sid, &key), "a new grant asks again");
    assert_ne!(Currency::live(sid).token_gen, 0);
    miss::reset_for_test();
}

#[test]
fn the_miss_set_is_cleared_by_a_generation_change_and_ignores_a_late_answer_of_the_old_one() {
    let at = |g| Currency { cache_gen: g, grant_epoch: 1, token_gen: 1 };
    let mut set = MissSet::default();
    set.insert(1, "/plx/cover/a", at(7));
    set.insert(1, "/plx/cover/b", at(7));
    assert!(set.contains(1, "/plx/cover/a", at(7)) && set.len() == 2);
    // A sign-out moved the disk epoch: the next miss empties the set.
    set.insert(1, "/plx/cover/c", at(8));
    assert_eq!(set.len(), 1);
    assert!(!set.contains(1, "/plx/cover/a", at(7)));
    assert!(!set.contains(1, "/plx/cover/a", at(8)), "and under the new epoch it is simply unknown");
    assert!(set.contains(1, "/plx/cover/c", at(8)));
    // A bake that started before the sign-out answers late: it must not displace the new epoch's.
    set.insert(1, "/plx/cover/d", at(7));
    assert_eq!(set.len(), 1);
    assert!(!set.contains(1, "/plx/cover/d", at(7)));
    // The identity has to match on every field.
    assert!(!set.contains(1, "/plx/cover/c", Currency { grant_epoch: 2, ..at(8) }));
    assert!(!set.contains(1, "/plx/cover/c", Currency { token_gen: 2, ..at(8) }));
    assert!(!set.contains(2, "/plx/cover/c", at(8)));
}

#[test]
fn the_miss_set_is_bounded_and_evicts_the_oldest() {
    let cur = Currency { cache_gen: 1, grant_epoch: 1, token_gen: 1 };
    let mut set = MissSet::default();
    for i in 0..miss::MISS_CAP + 10 {
        set.insert(3, &format!("/plx/cover/{i}"), cur);
    }
    assert_eq!(set.len(), miss::MISS_CAP);
    assert!(!set.contains(3, "/plx/cover/0", cur), "the oldest went");
    assert!(set.contains(3, &format!("/plx/cover/{}", miss::MISS_CAP + 9), cur));
    // Re-recording a key does not grow it.
    set.insert(3, &format!("/plx/cover/{}", miss::MISS_CAP + 9), cur);
    assert_eq!(set.len(), miss::MISS_CAP);
}

/// **A miss is only as lasting as its cause.** A definitive answer holds for the identity; a miss a
/// read that did not answer produced (Discover unreachable for 45 s) stops counting with the memo's
/// window, so the card asks for its cover again instead of wearing its composite for days.
#[test]
fn an_unanswered_miss_expires_with_the_discover_memo_and_a_verdict_does_not() {
    let (_g, sid, _) = one_server();
    miss::reset_for_test();
    clear_store();
    let blip = key_for(sid, &cover_key(), 420, 236, 0);
    let verdict = key_for(sid, &card_key(), 420, 236, 0);
    let cur = Currency::live(sid);

    miss::settle(sid, &blip, cur, false);
    miss::settle(sid, &verdict, cur, true);
    assert!(settled_failed(sid, &blip) && settled_failed(sid, &verdict), "both hold inside the window");
    assert_eq!(miss::UNANSWERED_FOR, plx_plex::plex::account::UNREACHABLE_MEMO, "the Discover memo's own window");

    // The window passes (a zero window is already over): the blip's key is asked for again.
    miss::note_for(sid, &blip, cur, std::time::Duration::ZERO);
    assert!(!settled_failed(sid, &blip), "an expired miss reads as never requested");
    assert!(settled_failed(sid, &verdict), "a verdict is not affected");

    // A verdict that follows an unanswered miss replaces it (the retry found the title has no cover).
    miss::settle(sid, &blip, cur, true);
    assert!(settled_failed(sid, &blip));
    miss::reset_for_test();
}

// ── the worker I/O's currency ───────────────────────────────────────────────────────────────────

/// A bake that straddles a sign-out, a retoken or a profile switch is delivered but never filed: the
/// cover's read was made as the profile it started under, the composite's grant is the client's.
#[test]
fn a_cover_or_card_bake_is_not_persisted_across_a_retoken_or_a_profile_switch() {
    struct Restore(std::sync::Arc<plx_plex::plex::session::CurrentProfile>);
    impl Drop for Restore {
        fn drop(&mut self) {
            plx_plex::plex::session::publish_profile_for_test(self.0.user.clone(), self.0.generation);
        }
    }
    let (_g, sid, _) = one_server();
    let _restore = Restore(plx_plex::plex::session::current_snapshot());
    let profile = |uuid: &str| plx_plex::plex::session::UserRef { uuid: uuid.into(), ..Default::default() };
    plx_plex::plex::session::publish_profile_for_test(Some(profile("profile-a")), 7);
    let client = plx_plex::plex::client_for(sid).unwrap();
    let cover = |c: &'static plx_plex::plex::Client| WorkerCoverIo {
        client: c,
        hex: HEX,
        cache_gen: plx_platform::imgcache::generation(),
        token_gen: c.token_gen(),
        profile: plx_plex::plex::session::current_profile_key(),
        disk: None,
        none_disk: None,
        client_id: "cid".into(),
        credential: None,
    };
    let card = |c: &'static plx_plex::plex::Client| WorkerCardIo {
        client: c,
        srv: sid,
        logo: "605",
        art: ART,
        cache_gen: plx_platform::imgcache::generation(),
        token_gen: c.token_gen(),
        disk: None,
    };

    let started = cover(client);
    assert!(started.still_current() && card(client).still_current(), "an undisturbed bake may persist");
    plx_plex::plex::session::publish_profile_for_test(Some(profile("profile-b")), 8);
    assert!(!started.still_current(), "the profile the cover was read as moved under the bake");
    assert!(card(client).still_current(), "a composite names no profile: only the grant can move it");

    let (started_cover, started_card) = (cover(client), card(client));
    client.set_token("tok-profile-b");
    assert!(!started_cover.still_current() && !started_card.still_current(), "the grant moved");
}

#[test]
fn the_cover_io_asks_the_discover_memo_and_needs_a_client_identifier() {
    let (_g, sid, _) = one_server();
    let client = plx_plex::plex::client_for(sid).unwrap();
    let mut io = WorkerCoverIo {
        client,
        hex: HEX,
        cache_gen: plx_platform::imgcache::generation(),
        token_gen: client.token_gen(),
        profile: String::new(),
        disk: None,
        none_disk: None,
        client_id: String::new(),
        credential: None,
    };
    assert!(!io.identified(), "no client identifier, no read");
    io.client_id = "cid".into();
    assert!(io.identified(), "no account token is needed: the endpoint answers the same without one");

    plx_plex::plex::account::set_discover_unreachable_for_test(false);
    assert!(!io.discover_unreachable());
    plx_plex::plex::account::set_discover_unreachable_for_test(true);
    assert!(io.discover_unreachable(), "the Discover memo, not the plex.tv one");
    plx_plex::plex::account::set_discover_unreachable_for_test(false);
}

// ── the refetch's write rule ────────────────────────────────────────────────────────────────────

fn decode_ok(b: &[u8]) -> Option<usize> {
    (b == b"good").then_some(b.len())
}

/// `refetch_art` writes the refreshed source back under its transcode key only when the bytes
/// DECODED and the bake is still current; the answer is used either way.
#[test]
fn a_refetched_source_is_written_back_only_when_it_decodes_and_the_bake_is_current() {
    use plx_plex::plex::ArtFetch;
    let good = || ArtFetch::Bytes(b"good".to_vec());
    assert_eq!(refetched(good(), || true, decode_ok), (Some(4), Some(b"good".to_vec())));
    assert_eq!(refetched(good(), || false, decode_ok), (Some(4), None), "used, not filed: no longer current");
    for bad in [ArtFetch::Bytes(b"junk".to_vec()), ArtFetch::Bytes(Vec::new()), ArtFetch::Status(404), ArtFetch::NoResponse] {
        let r = refetched(bad, || panic!("currency is asked only of bytes that decoded"), decode_ok);
        assert_eq!(r, (None, None));
    }
}

// ── a stale "no cover" marker is revalidated ────────────────────────────────────────────────────

/// A cover whose bake answered `Final` from a "no cover" marker older than `REFRESH_AFTER` decoded
/// nothing, so the store takes no pixels, and the marker must still reach the refresh lane: the
/// title may have gained a cover, and the privacy statement promises the saved answer is asked
/// again once it is more than a day old. Only a settled answer queues it (a load that failed
/// before any answer is the store's retry, not a revalidation).
#[test]
fn a_stale_no_cover_marker_reaches_the_refresh_lane_though_nothing_decoded() {
    let (_g, sid, _) = one_server();
    let client = plx_plex::plex::client_for(sid).unwrap();
    let marker_key = || {
        plx_platform::imgcache::classify_baked(
            &cover::namespace("en"),
            cover::COVER_KIND,
            HEX,
            cover::COVER_STAMP,
            1,
            1,
        )
        .unwrap()
    };
    let queued = |accepted: bool, baked_final: bool, stale: bool| {
        refresh::arm_for_test();
        let gen = plx_platform::imgcache::generation();
        queue_refresh(accepted, baked_final, stale.then(|| (client, marker_key())), cover_key(), gen, client.token_gen());
        refresh::take_queued_for_test()
    };
    assert_eq!(queued(false, true, true), [cover_key()], "a baked kind's Final from a stale marker");
    assert_eq!(queued(true, false, true), [cover_key()], "a decode served from a stale entry (as before)");
    assert!(queued(false, true, false).is_empty(), "a fresh marker has nothing to revalidate");
    assert!(queued(false, false, true).is_empty(), "no answer was settled: the store's retry, not a refresh");
}
