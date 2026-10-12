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
use super::prefs_door_tests::Parked;
use super::test_support::apply_plan;
use super::test_support::*;
use super::*;
use plx_plex::plex::session::{self as session, Session};

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
fn restore_all_seeds_the_mirror_from_the_record() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("memory-restore");
    unseed_memory_for_test::<SubtitleOffsetEntry>();
    let mut record = signed_in("acct");
    record.set_subtitle_offset_for("kid", "m", "rk", Some(-400));

    assert_eq!(recall::<SubtitleOffsetEntry>("kid", &key("m", "rk")), None, "never restored: nothing is answered");
    restore_all(&record, Overrides::default());
    assert_eq!(recall::<SubtitleOffsetEntry>("kid", &key("m", "rk")).map(|e| e.offset_ms), Some(-400));
    assert_eq!(recall::<SubtitleOffsetEntry>("", &key("m", "rk")), None, "another profile has none");
    assert_eq!(recall::<SubtitleOffsetEntry>("kid", &key("m", "other")), None, "another item has none");
    tidy();
}

/// A never-restored mirror answers nothing and refuses a write: there is no valid session to
/// remember into, and the refusal leaves the file alone.
#[test]
fn a_mirror_nobody_restored_refuses_a_write_and_answers_nothing() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("memory-unseeded");
    session::save(&signed_in("acct"));
    unseed_memory_for_test::<SubtitleOffsetEntry>();

    assert!(!remember::<SubtitleOffsetEntry>("", key("m", "rk"), |_| Some(offset("m", "rk", 5))));
    plx_base::storage_worker::drain_for_test();
    assert_eq!(recall::<SubtitleOffsetEntry>("", &key("m", "rk")), None);
    assert_eq!(session::peek().subtitle_offset_for("", "m", "rk"), None, "and nothing was queued");
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
    restore_all(&account_a, Overrides::default());
    assert_eq!((seen("from-file-a"), seen("ahead")), (Some(100), Some(300)), "same stamp: not reseeded");

    // The stamp moved, the tenure did not: reseeded from the new record.
    restore_all(&account_b, Overrides::default());
    assert_eq!(session::tenure(), tenure, "setup: nothing was revoked");
    assert_eq!(seen("from-file-b"), Some(200), "the new account's record is what the mirror holds");
    assert_eq!((seen("from-file-a"), seen("ahead")), (None, None), "and the previous account's is gone");

    // Control: the same record again is not a second reseed.
    assert!(remember::<SubtitleOffsetEntry>("", key("m", "ahead-b"), |_| Some(offset("m", "ahead-b", 400))));
    restore_all(&account_b, Overrides::default());
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
    restore_all(&session::peek(), Overrides::default());
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
    let mut with_extension = signed_in("acct");
    with_extension.subtitle_offsets = serde_json::from_value(serde_json::json!([
        {"user":"","items":[{"machine_id":"machine-ext","rating_key":"rk-ext","offset_ms":100,"future":"kept"}]}
    ])).unwrap();
    session::save(&with_extension);
    seed_memory_for_test::<SubtitleOffsetEntry>(&session::peek());
    let _sid = playing(&mut ps, "machine-ext", "rk-ext");

    persist_subtitle_offset(&ps, 250);
    plx_base::storage_worker::drain_for_test();
    let stored = serde_json::to_value(&session::peek().subtitle_offsets).unwrap();
    assert_eq!(stored[0]["items"][0]["offset_ms"], 250);
    assert_eq!(stored[0]["items"][0]["future"], "kept", "the entry's other keys survive a re-pick");

    let before = stored.clone();
    persist_subtitle_offset(&ps, 250);
    plx_base::storage_worker::drain_for_test();
    assert_eq!(serde_json::to_value(&session::peek().subtitle_offsets).unwrap(), before);
    tidy();
}
