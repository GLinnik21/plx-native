//! **The memory door's media half** (`prefs::{MemoryMirror, remember, recall}`) with the subtitle
//! offsets as its first tenant, and the one declared delta it carries (`track-memory-plan.md`
//! §3.7, delta 1).
//!
//! * **Delta 1** — a correction tuned and replayed before its file write lands is restored. The
//!   same delta covers its other edge: a pick whose file write fails or is dropped by
//!   `queue_update`'s guard stays recalled until the next restart or tenure/stamp reseed.
//! * **T6** — the mirror is reseeded on an account-stamp change under an unchanged tenure, and not
//!   when neither moved.
//! * **Revocation** — a write after a sign-out is refused and what the mirror held is invisible.
//! * **A seat** (P-d11) — the door answers and writes only for the seated profile, so every test
//!   that remembers seats its profile first (`TempSession::seated`); `offset_tuned_while_unseated_
//!   is_not_saved` is the one that does not.
use super::prefs_door_tests::Parked;
use super::test_support::apply_plan;
use super::test_support::*;
use super::*;
use plx_plex::plex::session::{self as session, Session, SubtitleOffsetEntry};

fn signed_in(token: &str) -> Session {
    Session { client_id: "cid-1".into(), account_token: token.into(), ..Default::default() }
}

fn offset(machine_id: &str, rating_key: &str, offset_ms: i64) -> SubtitleOffsetEntry {
    SubtitleOffsetEntry {
        machine_id: machine_id.into(),
        rating_key: rating_key.into(),
        offset_ms,
        extensions: Default::default(),
    }
}

fn key(machine_id: &str, rating_key: &str) -> (String, String) {
    (machine_id.to_string(), rating_key.to_string())
}

/// A playback of `rk` on a registered server `machine`, the way the existing resume test builds one.
fn playing(ps: &mut PlaybackSession, machine: &str, rk: &str) -> ServerId {
    let sid = plx_plex::plex::register_for_test(machine, "10.0.0.9", 32400, "tok", "cid-memory");
    apply_plan(ps, Plan::default(), rk);
    swap_cur_sid_for_test(ps, sid);
    sid
}

/// Leave the process as the next test expects it.
fn tidy() {
    plx_plex::plex::reset_servers_for_test();
    crate::player::reset_subtitle();
    crate::player::set_subtitle_offset(0);
    unseed_memory_for_test::<SubtitleOffsetEntry>();
}

/// **Delta 1.** An offset tuned and then replayed before its write lands is restored. The storage
/// worker is parked, so the file cannot have it yet; the read used to be `peek()` of the file
/// (write-after) and found nothing, so a replay inside the write's latency started at 0.
#[test]
fn offset_replayed_before_its_write_lands_is_restored() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let _session = session::TempSession::new("memory-delta-1");
    session::save(&signed_in("acct"));
    _session.seated("", "acct");
    assert_eq!(session::peek().client_id, "cid-1");
    seed_memory_for_test::<SubtitleOffsetEntry>(&session::peek());
    let sid = playing(&mut ps, "machine-delta-1", "rk-replay");

    let parked = Parked::new();
    crate::player::set_subtitle_offset(1_800);
    persist_subtitle_offset(&ps, 1_800);
    assert_eq!(
        session::peek().subtitle_offset_for("", "machine-delta-1", "rk-replay"),
        None,
        "setup: the write has not landed, the worker is parked behind it"
    );

    reset_track_selection(sid, "rk-replay", None);
    assert_eq!(crate::player::subtitle_offset_ms(), 1_800, "the replay restores the pick the file does not have yet");

    parked.release();
    plx_base::storage_worker::drain_for_test();
    assert_eq!(session::peek().subtitle_offset_for("", "machine-delta-1", "rk-replay"), Some(1_800), "and the write still lands");
    tidy();
}

/// **Delta 1, the other edge.** The mirror is written before the outcome of the file write is
/// known and nothing takes the entry back out, so a pick whose write is dropped by
/// `queue_update`'s guard (the client id changed under an unchanged tenure and token) stays
/// recalled until a restart or a tenure/stamp reseed, while the file never gets it. This also pins
/// that `remember`'s queued write really goes through that guard: an unguarded write would land
/// the pick in the new client id's file.
#[test]
fn a_pick_whose_write_the_guard_drops_stays_recalled_and_never_reaches_the_file() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let _session = session::TempSession::new("memory-dropped-write");
    session::save(&signed_in("acct"));
    _session.seated("", "acct");
    assert_eq!(session::peek().client_id, "cid-1");
    seed_memory_for_test::<SubtitleOffsetEntry>(&session::peek());
    let _sid = playing(&mut ps, "machine-dropped", "rk-dropped");

    let parked = Parked::new();
    persist_subtitle_offset(&ps, 1_800);
    session::save(&Session { client_id: "cid-2".into(), account_token: "acct".into(), ..Default::default() });
    parked.release();
    plx_base::storage_worker::drain_for_test();

    assert_eq!(session::peek().client_id, "cid-2", "setup: the client id moved under the queued write");
    assert_eq!(
        session::peek().subtitle_offset_for("", "machine-dropped", "rk-dropped"),
        None,
        "the guard dropped the write: the file never got the pick"
    );
    assert_eq!(
        recall::<SubtitleOffsetEntry>("", &key("machine-dropped", "rk-dropped")).map(|e| e.offset_ms),
        Some(1_800),
        "and the mirror keeps it, declared delta 1"
    );
    tidy();
}

/// Setting the offset back to Original forgets it at once too: the mirror is write-through in both
/// directions, not only for a new pick.
#[test]
fn an_offset_reset_to_original_is_forgotten_before_its_write_lands() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let _session = session::TempSession::new("memory-forget");
    session::save(&signed_in("acct"));
    _session.seated("", "acct");
    let _ = session::peek();
    seed_memory_for_test::<SubtitleOffsetEntry>(&session::peek());
    let sid = playing(&mut ps, "machine-forget", "rk-forget");
    persist_subtitle_offset(&ps, 900);
    plx_base::storage_worker::drain_for_test();

    let parked = Parked::new();
    persist_subtitle_offset(&ps, 0);
    crate::player::set_subtitle_offset(0);
    reset_track_selection(sid, "rk-forget", None);
    assert_eq!(crate::player::subtitle_offset_ms(), 0, "Original is not restored as 900");
    assert_eq!(recall::<SubtitleOffsetEntry>("", &key("machine-forget", "rk-forget")), None);
    parked.release();
    plx_base::storage_worker::drain_for_test();
    assert_eq!(session::peek().subtitle_offset_for("", "machine-forget", "rk-forget"), None);
    tidy();
}

/// A restore seeds the mirror from the record, so what a previous run remembered is found at the
/// next boot, and a profile's entry is not another profile's.
#[test]
fn restore_install_rows_seeds_the_mirror_from_the_record() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("memory-restore");
    unseed_memory_for_test::<SubtitleOffsetEntry>();
    let mut record = signed_in("acct");
    record.set_subtitle_offset_for("kid", "m", "rk", Some(-400));
    record.set_subtitle_offset_for("", "m", "owners", Some(250));

    _session.seated("kid", "acct");
    assert_eq!(recall::<SubtitleOffsetEntry>("kid", &key("m", "rk")), None, "never restored: nothing is answered");
    restore_install_rows(&record);
    assert_eq!(recall::<SubtitleOffsetEntry>("kid", &key("m", "rk")).map(|e| e.offset_ms), Some(-400));
    assert_eq!(recall::<SubtitleOffsetEntry>("kid", &key("m", "other")), None, "another item has none");
    assert_eq!(recall::<SubtitleOffsetEntry>("", &key("m", "owners")), None, "another profile is not the seated one");
    _session.seated("", "acct");
    assert_eq!(recall::<SubtitleOffsetEntry>("", &key("m", "owners")).map(|e| e.offset_ms), Some(250), "seated, it is found");
    assert_eq!(recall::<SubtitleOffsetEntry>("", &key("m", "rk")), None, "and it has not the other profile's entry");
    tidy();
}

/// **SO: the door stamps what it writes with the signed-in account.** The mirror reads and writes
/// under the stamp it was seeded with, and the queued file write under the file's own: a pick lands
/// in the file as a record stamped for the account, a record a previous build left unstamped is
/// adopted by that write, and a record stamped for another account is neither recalled nor kept.
#[test]
fn the_door_stamps_its_writes_adopts_legacy_records_and_drops_another_accounts() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let _session = session::TempSession::new("memory-so-stamp");
    let mut record = signed_in("acct");
    // A legacy (unstamped) record of this profile, and a record of another account's.
    record.prefs.subtitle_offsets = serde_json::from_value(serde_json::json!([
        {"user": "", "items": [{"machine_id": "machine-so", "rating_key": "rk-legacy", "offset_ms": 700}]},
        {"user": "kid", "account": "ffffffffffffffff",
         "items": [{"machine_id": "machine-so", "rating_key": "rk-foreign", "offset_ms": 800}]},
    ]))
    .unwrap();
    session::save(&record);
    _session.seated("", "acct");
    assert_eq!(session::peek().client_id, "cid-1");
    seed_memory_for_test::<SubtitleOffsetEntry>(&session::peek());
    let _sid = playing(&mut ps, "machine-so", "rk-new");
    let stamp = plx_plex::plex::grant::account_key("acct");

    assert_eq!(recall::<SubtitleOffsetEntry>("", &key("machine-so", "rk-legacy")).map(|e| e.offset_ms), Some(700),
        "the legacy record is the account's own");
    // Seat "kid" for this one read: the seat gate answers `None` for any unseated profile, so
    // without it this would not grade the account stamp at all.
    _session.seated("kid", "acct");
    assert_eq!(recall::<SubtitleOffsetEntry>("kid", &key("machine-so", "rk-foreign")), None,
        "another account's record reads as nothing");
    _session.seated("", "acct");

    persist_subtitle_offset(&ps, 1_800);
    plx_base::storage_worker::drain_for_test();

    let stored = session::peek().prefs.subtitle_offsets.clone();
    assert_eq!(stored.len(), 1, "the foreign record is pruned and the legacy one adopted in place: {stored:?}");
    assert_eq!((stored[0].user.as_str(), stored[0].account.as_str()), ("", stamp.as_str()));
    let rks: Vec<_> = stored[0].items.iter().map(|e| (e.rating_key.as_str(), e.offset_ms)).collect();
    assert_eq!(rks, [("rk-legacy", 700), ("rk-new", 1_800)]);
    assert_eq!(recall::<SubtitleOffsetEntry>("", &key("machine-so", "rk-new")).map(|e| e.offset_ms), Some(1_800));
    tidy();
}

/// **SO: the mirror half of the door stamps too.** A restore seeds the mirror from a file that
/// already holds a STAMPED record of the item; re-tuning it, or forgetting it, must reach that same
/// record. A mirror write that dropped the stamp would land in a second, unstamped record, and
/// `recall` (which reads the stamped record first) would keep answering the old value until a
/// restart.
#[test]
fn a_retune_or_a_forget_reaches_the_stamped_record_the_restore_seeded() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let _session = session::TempSession::new("memory-so-mirror-stamp");
    let mut record = signed_in("acct");
    record.set_subtitle_offset_for("", "machine-x", "rk-k", Some(900));
    session::save(&record);
    _session.seated("", "acct");
    assert_eq!(session::peek().client_id, "cid-1");
    seed_memory_for_test::<SubtitleOffsetEntry>(&session::peek());
    let _sid = playing(&mut ps, "machine-x", "rk-k");
    let stamp = plx_plex::plex::grant::account_key("acct");
    assert_eq!(
        recall::<SubtitleOffsetEntry>("", &key("machine-x", "rk-k")).map(|e| e.offset_ms),
        Some(900),
        "setup: the seeded mirror holds the stamped pick"
    );

    let parked = Parked::new();
    persist_subtitle_offset(&ps, 1_200);
    assert_eq!(
        recall::<SubtitleOffsetEntry>("", &key("machine-x", "rk-k")).map(|e| e.offset_ms),
        Some(1_200),
        "the re-tune replaces the stamped pick in the mirror, not a second unstamped one beside it"
    );
    persist_subtitle_offset(&ps, 0);
    assert_eq!(
        recall::<SubtitleOffsetEntry>("", &key("machine-x", "rk-k")),
        None,
        "and forgetting it removes the stamped pick"
    );
    persist_subtitle_offset(&ps, 1_200);
    parked.release();
    plx_base::storage_worker::drain_for_test();

    let stored = session::peek().prefs.subtitle_offsets.clone();
    assert_eq!(stored.len(), 1, "one record in the file: {stored:?}");
    assert_eq!((stored[0].user.as_str(), stored[0].account.as_str()), ("", stamp.as_str()));
    let items: Vec<_> = stored[0].items.iter().map(|e| (e.rating_key.as_str(), e.offset_ms)).collect();
    assert_eq!(items, [("rk-k", 1_200)]);
    tidy();
}

/// A never-restored mirror answers nothing and refuses a write: there is no valid session to
/// remember into, and the refusal leaves the file alone.
#[test]
fn a_mirror_nobody_restored_refuses_a_write_and_answers_nothing() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("memory-unseeded");
    session::save(&signed_in("acct"));
    _session.seated("", "acct");
    unseed_memory_for_test::<SubtitleOffsetEntry>();

    assert!(!remember::<SubtitleOffsetEntry>("", key("m", "rk"), |_| Some(offset("m", "rk", 5))));
    plx_base::storage_worker::drain_for_test();
    assert_eq!(recall::<SubtitleOffsetEntry>("", &key("m", "rk")), None);
    assert_eq!(session::peek().subtitle_offset_for("", "m", "rk"), None, "and nothing was queued");
    tidy();
}

/// **P-d11: an offset tuned while nobody is seated is not saved.** A dev-token run (or a replay) seats
/// nobody, and `""` is both the account owner and "no profile published": the door asks the seat, so
/// the pick neither enters the mirror nor reaches the file. On `main` it was written into the owner's
/// `""` record of whatever account was on disk, which for a test is a real person's record. Once the
/// owner IS seated, the very same call saves (the control that keeps this from passing for the wrong
/// reason).
#[test]
fn offset_tuned_while_unseated_is_not_saved() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("memory-unseated");
    session::save(&signed_in("acct"));
    assert_eq!(session::current_seat(), session::Seat::Unseated, "setup: nobody is seated");
    seed_memory_for_test::<SubtitleOffsetEntry>(&session::peek());
    playing(&mut ps, "machine-unseated", "rk-unseated");

    crate::player::set_subtitle_offset(1_800);
    persist_subtitle_offset(&ps, 1_800);
    plx_base::storage_worker::drain_for_test();
    assert_eq!(recall::<SubtitleOffsetEntry>("", &key("machine-unseated", "rk-unseated")), None, "not remembered");
    assert_eq!(session::peek().subtitle_offset_for("", "machine-unseated", "rk-unseated"), None, "and not written");

    session.seated("", "acct");
    persist_subtitle_offset(&ps, 1_800);
    plx_base::storage_worker::drain_for_test();
    assert_eq!(
        session::peek().subtitle_offset_for("", "machine-unseated", "rk-unseated"),
        Some(1_800),
        "seated as the owner, the same pick is saved"
    );
    tidy();
}

/// **T6.** The mirror is reseeded when the account stamp moved and the tenure did not (a new
/// sign-in replaces the token without revoking anything), and is NOT reseeded when neither moved
/// (it is ahead of the file by the writes still queued, so reseeding would lose them).
#[test]
fn t6_the_mirror_is_reseeded_on_a_stamp_change_with_the_tenure_unchanged() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("memory-t6");
    session::save(&signed_in("acct-a"));
    _session.seated("", "acct-a");
    let _ = session::peek();

    let mut account_a = signed_in("acct-a");
    account_a.set_subtitle_offset_for("", "m", "from-file-a", Some(100));
    let mut account_b = signed_in("acct-b");
    account_b.set_subtitle_offset_for("", "m", "from-file-b", Some(200));
    seed_memory_for_test::<SubtitleOffsetEntry>(&account_a);
    let tenure = session::tenure();

    // A pick the file does not have yet.
    assert!(remember::<SubtitleOffsetEntry>("", key("m", "ahead"), |_| Some(offset("m", "ahead", 300))));
    let seen = |rk: &str| recall::<SubtitleOffsetEntry>("", &key("m", rk)).map(|e| e.offset_ms);

    // Neither the tenure nor the stamp moved: the mirror keeps what it is ahead by.
    restore_install_rows(&account_a);
    assert_eq!((seen("from-file-a"), seen("ahead")), (Some(100), Some(300)), "same stamp: not reseeded");

    // The stamp moved, the tenure did not: reseeded from the new record.
    restore_install_rows(&account_b);
    assert_eq!(session::tenure(), tenure, "setup: nothing was revoked");
    assert_eq!(seen("from-file-b"), Some(200), "the new account's record is what the mirror holds");
    assert_eq!((seen("from-file-a"), seen("ahead")), (None, None), "and the previous account's is gone");

    // Control: the same record again is not a second reseed.
    assert!(remember::<SubtitleOffsetEntry>("", key("m", "ahead-b"), |_| Some(offset("m", "ahead-b", 400))));
    restore_install_rows(&account_b);
    assert_eq!(seen("ahead-b"), Some(400));
    plx_base::storage_worker::drain_for_test();
    tidy();
}

/// **A write after revocation is refused, and its mirror entry is invisible.** A sign-out moves the
/// tenure: what the mirror held is no longer answered, a new pick is refused (and nothing is
/// queued), and the entry is not there after a later sign-in until a restore seeds it from a file
/// that actually has it.
#[test]
fn a_write_after_revocation_is_refused_and_the_mirror_entry_is_invisible() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let _session = session::TempSession::new("memory-revoked");
    session::save(&signed_in("acct"));
    _session.seated("", "acct");
    assert_eq!(session::peek().client_id, "cid-1");
    seed_memory_for_test::<SubtitleOffsetEntry>(&session::peek());
    let sid = playing(&mut ps, "machine-revoked", "rk-revoked");

    // Remembered while signed in, and landed.
    persist_subtitle_offset(&ps, 700);
    plx_base::storage_worker::drain_for_test();
    assert_eq!(recall::<SubtitleOffsetEntry>("", &key("machine-revoked", "rk-revoked")).map(|e| e.offset_ms), Some(700));

    session::revoke_cached_session();
    assert_eq!(recall::<SubtitleOffsetEntry>("", &key("machine-revoked", "rk-revoked")), None, "revoked: invisible");

    // A pick made after the sign-out is refused outright.
    assert!(!remember::<SubtitleOffsetEntry>("", key("machine-revoked", "rk-new"), |_| Some(offset("machine-revoked", "rk-new", 5))));
    assert_eq!(recall::<SubtitleOffsetEntry>("", &key("machine-revoked", "rk-new")), None);
    reset_track_selection(sid, "rk-revoked", None);
    assert_eq!(crate::player::subtitle_offset_ms(), 0, "and a replay finds nothing while revoked");

    // A later sign-in proves the credentials again, but the mirror is not valid until a restore
    // reseeds it from the record; the refused pick never reached the file.
    session::save(&signed_in("acct"));
    plx_base::storage_worker::drain_for_test();
    assert_eq!(recall::<SubtitleOffsetEntry>("", &key("machine-revoked", "rk-new")), None, "still invisible before a restore");
    restore_install_rows(&session::peek());
    assert_eq!(recall::<SubtitleOffsetEntry>("", &key("machine-revoked", "rk-new")), None, "the refused pick was never written");
    tidy();
}

/// `remember`'s delta is the viewer's offset only: a key a newer build wrote on the entry (carried in
/// its extensions) is the FILE's, taken from the entry that is there when the write runs, and an
/// unchanged re-pick writes nothing.
#[test]
fn a_repick_keeps_the_entrys_other_keys_and_an_unchanged_one_writes_nothing() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let _session = session::TempSession::new("memory-extensions");
    _session.seated("", "acct");
    let mut with_extension = signed_in("acct");
    with_extension.prefs.subtitle_offsets = serde_json::from_value(serde_json::json!([
        {"user":"","items":[{"machine_id":"machine-ext","rating_key":"rk-ext","offset_ms":100,"future":"kept"}]}
    ])).unwrap();
    session::save(&with_extension);
    seed_memory_for_test::<SubtitleOffsetEntry>(&session::peek());
    let _sid = playing(&mut ps, "machine-ext", "rk-ext");

    persist_subtitle_offset(&ps, 250);
    plx_base::storage_worker::drain_for_test();
    let stored = serde_json::to_value(&session::peek().prefs.subtitle_offsets).unwrap();
    assert_eq!(stored[0]["items"][0]["offset_ms"], 250);
    assert_eq!(stored[0]["items"][0]["future"], "kept", "the entry's other keys survive a re-pick");

    let before = stored.clone();
    persist_subtitle_offset(&ps, 250);
    plx_base::storage_worker::drain_for_test();
    assert_eq!(serde_json::to_value(&session::peek().prefs.subtitle_offsets).unwrap(), before);
    tidy();
}

// ---- #830: the mirror follows the OWNER, not the sign-in token -----------------------------------

const OWNER_7: &str = "o1:19afc47bcea99412";

/// A session of `token` whose roster names admin `owner`, holding one offset for the owner profile
/// stamped as an owner-stamped build writes it.
fn owned(token: &str, owner: i64, rating_key: &str, offset_ms: i64) -> Session {
    let stamp = match owner {
        7 => OWNER_7.to_string(),
        _ => "o1:38e53b36e68382ab".to_string(),
    };
    let mut session = signed_in(token);
    session.home_users = vec![session::HomeUserRef { id: owner, uuid: format!("synthetic-owner-{owner}"), admin: true, ..Default::default() }];
    session.prefs.subtitle_offsets = vec![session::prefs::ProfileList {
        user: String::new(), account: stamp, items: vec![offset("m", rating_key, offset_ms)], ..Default::default()
    }];
    session
}

/// **T6, by owner.** A sign-in by the same person mints a new token and keeps the stamp, so the
/// restore that follows does NOT reseed the mirror (it is ahead of the file by the writes still
/// queued, and a reseed would lose them); a sign-in by another owner moves the stamp and does.
#[test]
fn t6_by_owner_the_mirror_is_kept_across_a_same_owner_resignin_and_reseeded_for_another_owner() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("memory-t6-owner");
    session::save(&signed_in("tok-1"));
    _session.seated("", "tok-1");
    let _ = session::peek();
    let first = owned("tok-1", 7, "from-file-1", 100);
    seed_memory_for_test::<SubtitleOffsetEntry>(&first);
    let seen = |rk: &str| recall::<SubtitleOffsetEntry>("", &key("m", rk)).map(|e| e.offset_ms);
    assert_eq!(seen("from-file-1"), Some(100), "setup: the mirror was seeded under the owner stamp");
    assert!(remember::<SubtitleOffsetEntry>("", key("m", "ahead"), |_| Some(offset("m", "ahead", 300))));

    restore_install_rows(&owned("tok-2", 7, "from-file-2", 200));
    assert_eq!((seen("from-file-1"), seen("ahead")), (Some(100), Some(300)),
        "the same owner under a new token: not reseeded, the mirror keeps its writes in flight");

    restore_install_rows(&owned("tok-3", 8, "from-file-3", 400));
    assert_eq!(seen("from-file-3"), Some(400), "another owner: reseeded from the new record");
    assert_eq!((seen("from-file-1"), seen("ahead")), (None, None), "and the previous owner's picks are gone");
    plx_base::storage_worker::drain_for_test();
    tidy();
}

/// **G3, the queued write.** The mirror was seeded under one stamp and the file's own stamp moved
/// before the write ran (the roster landed: the owner became known). The pick is in the mirror,
/// stamped as the mirror is, so the file write must not re-derive a stamp of its own: it is
/// dropped, never written under either, and the mirror keeps the pick until the next restore
/// reseeds it (the bounded loss `queue_update`'s token guard already has).
#[test]
fn remember_drops_the_edit_when_the_file_stamp_differs_from_the_mirror_seed() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let _session = session::TempSession::new("memory-g3-drop");
    session::save(&signed_in("acct"));
    _session.seated("", "acct");
    seed_memory_for_test::<SubtitleOffsetEntry>(&session::peek());
    let _sid = playing(&mut ps, "machine-g3", "rk-g3");

    let parked = Parked::new();
    persist_subtitle_offset(&ps, 1_800);
    assert!(session::update(|cur| Some(Session {
        home_users: vec![session::HomeUserRef { id: 7, uuid: "synthetic-owner-7".into(), admin: true, ..Default::default() }],
        ..cur.clone()
    })), "the roster lands while the write is queued");
    parked.release();
    plx_base::storage_worker::drain_for_test();

    let stored = session::peek().prefs.subtitle_offsets.clone();
    assert!(stored.iter().all(|r| r.items.is_empty()), "the file's stamp moved under the queued write, which was dropped: {stored:?}");
    assert_eq!(recall::<SubtitleOffsetEntry>("", &key("machine-g3", "rk-g3")).map(|e| e.offset_ms), Some(1_800),
        "and the mirror keeps the pick");

    // After a restore the mirror is seeded under the stamp the file has, and the door writes again.
    session::publish_seat_for_test(Some(session::UserRef::default()), session::current_gen().wrapping_add(1), OWNER_7.into());
    seed_memory_for_test::<SubtitleOffsetEntry>(&session::peek());
    persist_subtitle_offset(&ps, 2_400);
    plx_base::storage_worker::drain_for_test();
    let stored = session::peek().prefs.subtitle_offsets.clone();
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0].account, OWNER_7, "reseeded under the owner stamp, the pick lands under it");
    tidy();
}
