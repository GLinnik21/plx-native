//! The session FILE half: save/peek/update against a real file — atomicity, secure-envelope
//! preservation, concurrent read-modify-write, and torn-write safety.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::TempSession;

#[test]
fn recording_capture_fresh_identity_has_no_persistence_before_attachment() {
    let _serial = crate::testlock::serial();
    let root = TempSession::new("capture-fresh");
    let (saved, entropy, deferred) = load_capturing_entropy();
    assert!(!saved.client_id.is_empty());
    assert!(entropy.is_some());
    assert!(!root.file().exists(), "capturing inputs must not persist before recorder attachment");
    deferred.apply().unwrap();
    assert!(root.file().exists(), "normal fresh persistence executes after attachment");
}

#[test]
fn recording_capture_plaintext_does_not_migrate_before_attachment() {
    use std::os::unix::fs::MetadataExt;
    let _serial = crate::testlock::serial();
    let root = TempSession::new("capture-plaintext");
    let before = serde_json::to_vec(&signed_in()).unwrap();
    std::fs::write(root.file(), &before).unwrap();
    let inode = std::fs::metadata(root.file()).unwrap().ino();
    let (saved, entropy, deferred) = load_capturing_entropy();
    assert!(!saved.client_id.is_empty());
    assert!(entropy.is_none());
    assert!(std::fs::read(root.file()).unwrap() == before, "capture must leave plaintext bytes unchanged");
    assert_eq!(std::fs::metadata(root.file()).unwrap().ino(), inode);
    deferred.apply().unwrap();
    assert_ne!(std::fs::metadata(root.file()).unwrap().ino(), inode, "normal atomic migration runs afterwards");
}

#[test]
fn deferred_capture_never_overwrites_a_newer_session() {
    let _serial = crate::testlock::serial();
    let root = TempSession::new("capture-superseded");
    let (_, _, deferred) = load_capturing_entropy();
    save(&signed_in());
    let before = std::fs::read(root.file()).unwrap();
    assert!(deferred.apply().is_err());
    assert!(std::fs::read(root.file()).unwrap() == before);
}

/// A save lands as a WHOLE file — written to a sibling tmp and renamed over — leaving nothing
/// behind, and the credentials are never on disk in a mode another uid can read (this box is
/// rooted and `/media/developer` is world-readable). The tmp is where the secret exists first,
/// so the 0600 rule has to reach it too.
#[test]
fn a_save_lands_whole_and_leaves_no_temporary_behind() {
    use std::os::unix::fs::PermissionsExt;
    let _g = crate::testlock::serial();
    let t = TempSession::new("whole");

    save(&signed_in());
    assert_eq!(peek().account_token, "acct", "and it reads back");
    assert!(
        !t.tmp().exists(),
        "the tmp file is renamed, not left beside the session"
    );
    let mode = std::fs::metadata(t.file()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "credentials at rest");

    // a sign-out takes the tmp with it: `peek` cannot read one, but a live account token left
    // in a file on a rooted television is not a sign-out
    std::fs::write(t.tmp(), b"{}").unwrap();
    clear();
    assert!(!t.file().exists() && !t.tmp().exists());
}

/// **The route ground's one persisted seed.** A fresh device has recorded nothing, a real
/// hero is remembered across the read-modify-write cycle `update` uses everywhere else, and
/// recording the SAME envelope again is a no-op rather than a second disk write.
#[test]
fn last_hero_blur_round_trips_and_skips_a_redundant_write() {
    let _g = crate::testlock::serial();
    let _t = TempSession::new("last-hero");
    save(&signed_in());
    assert_eq!(last_hero(), None, "a fresh device has shown no hero yet");

    let envelope = [[0.1, 0.2, 0.3]; 4];
    assert!(record_last_hero(envelope), "a new envelope is a real write");
    assert_eq!(last_hero(), Some(envelope));

    assert!(
        !record_last_hero(envelope),
        "recording the same envelope again must not touch the file"
    );

    let second = [[0.9, 0.8, 0.7]; 4];
    assert!(
        record_last_hero(second),
        "a genuinely different hero writes"
    );
    assert_eq!(last_hero(), Some(second), "…and replaces the stored one");
}

/// A temporary LS2/key-store failure must never turn ciphertext back into plaintext or make
/// `load` overwrite it with a newly minted, logged-out client id. The host has no Luna bus,
/// which is the exact unavailable-key condition this policy has to survive.
#[test]
fn an_unopenable_secure_session_is_preserved_without_plaintext_downgrade() {
    let _g = crate::testlock::serial();
    let t = TempSession::new("secure-locked");
    let envelope = SecureEnvelope {
        format: SECURE_FORMAT.to_string(),
        version: 1,
        sealed: crate::keymanager::Sealed {
            backend: crate::keymanager::Backend::Keymanager3,
            key: "plxnative.session.v1".to_string(),
            iv: "AAAAAAAAAAAAAAAAAAAAAA==".to_string(),
            data: "c2VjcmV0".to_string(),
        },
    };
    let original = serde_json::to_vec_pretty(&envelope).unwrap();
    std::fs::write(t.file(), &original).unwrap();

    let (captured, entropy, deferred) = load_capturing_entropy();
    assert!(!captured.client_id.is_empty() && entropy.is_some());
    assert!(std::fs::read(t.file()).unwrap() == original);
    deferred.apply().unwrap();
    assert!(std::fs::read(t.file()).unwrap() == original, "deferred load preserves locked ciphertext too");
    let loaded = load();
    assert!(
        !loaded.client_id.is_empty(),
        "the run still gets an ephemeral id"
    );
    assert_eq!(std::fs::read(t.file()).unwrap(), original);

    save(&signed_in());
    assert_eq!(
        std::fs::read(t.file()).unwrap(),
        original,
        "an unavailable service cannot leak the replacement session as plaintext"
    );
}

#[test]
fn an_unknown_secure_envelope_version_is_locked_and_never_rewritten_as_plaintext() {
    let _g = crate::testlock::serial();
    let t = TempSession::new("secure-future-version");
    let original = br#"{
  "format": "plxnative-secure-session",
  "version": 2,
  "sealed": {
"backend": "keymanager3",
"key": "plxnative.session.v2",
"iv": "future-iv",
"data": "future-ciphertext"
  }
}"#;
    std::fs::write(t.file(), original).unwrap();

    let loaded = load();
    assert!(
        !loaded.client_id.is_empty(),
        "the run still gets an ephemeral id"
    );
    assert_eq!(
        std::fs::read(t.file()).unwrap(),
        original,
        "rollback must preserve an envelope it does not understand"
    );

    save(&signed_in());
    assert_eq!(
        std::fs::read(t.file()).unwrap(),
        original,
        "a future secure envelope must shadow every plaintext replacement"
    );
}

#[test]
fn a_precreated_tmp_symlink_cannot_redirect_session_bytes() {
    use std::os::unix::fs::symlink;
    let _g = crate::testlock::serial();
    let t = TempSession::new("tmp-symlink");
    let victim = t.dir.join("attacker-readable");
    std::fs::write(&victim, b"unchanged").unwrap();
    symlink(&victim, t.tmp()).unwrap();

    save(&signed_in());

    assert_eq!(std::fs::read(&victim).unwrap(), b"unchanged");
    assert_eq!(peek().account_token, "acct");
}

#[test]
fn a_quality_choice_persists_without_replacing_other_session_state() {
    let _g = crate::testlock::serial();
    let _t = TempSession::new("quality");
    let mut s = signed_in();
    s.sources.push(SourceRef {
        machine_id: "server-a".into(),
        token: "server-token".into(),
        address: "192.168.0.10".into(),
        port: 32400,
        ..Default::default()
    });
    save(&s);

    assert!(update(|cur| Some(
        cur.with_playback_quality(PlaybackQuality::P720)
    )));
    let landed = peek();
    assert_eq!(landed.playback_quality(), PlaybackQuality::P720);
    assert_eq!(landed.account_token, "acct");
    assert_eq!(landed.sources.len(), 1);
    assert_eq!(landed.sources[0].machine_id, "server-a");
}

#[test]
fn auto_sign_in_persists_without_replacing_other_session_state() {
    let _g = crate::testlock::serial();
    let _t = TempSession::new("auto-sign-in");
    let mut s = signed_in();
    s.user.uuid = "u-kid".into();
    s.sources.push(SourceRef {
        machine_id: "server-a".into(),
        token: "server-token".into(),
        address: "192.168.0.10".into(),
        port: 32400,
        ..Default::default()
    });
    save(&s);
    assert!(!peek().auto_sign_in());

    assert!(set_auto_sign_in(true));
    let landed = peek();
    assert!(landed.auto_sign_in());
    assert_eq!(landed.account_token, "acct");
    assert_eq!(landed.user.uuid, "u-kid");
    assert_eq!(landed.sources.len(), 1);

    assert!(
        !set_auto_sign_in(true),
        "setting the same value again must not touch the file"
    );
    assert!(set_auto_sign_in(false));
    assert!(!peek().auto_sign_in());
}

/// `take_ready` / a profile switch `save` a whole snapshot they loaded at the start of the
/// flow. That snapshot must carry the switch, or the next boot forgets it.
#[test]
fn a_full_save_of_a_switch_snapshot_keeps_auto_sign_in() {
    let _g = crate::testlock::serial();
    let _t = TempSession::new("auto-sign-in-save");
    let mut s = signed_in();
    s.user.uuid = "u-admin".into();
    save(&s);
    assert!(set_auto_sign_in(true));

    let mut snap = peek();
    snap.user.uuid = "u-kid".into();
    save(&snap);

    let landed = peek();
    assert!(
        landed.auto_sign_in(),
        "a whole-file replace of a loaded snapshot must not drop the switch"
    );
    assert_eq!(landed.user.uuid, "u-kid");
    assert_eq!(landed.account_token, "acct");
}

#[test]
fn loading_legacy_json_without_an_id_repairs_only_the_id_not_the_quality() {
    let _g = crate::testlock::serial();
    let t = TempSession::new("legacy-no-id");
    std::fs::write(t.file(), br#"{"account_token":"legacy-account"}"#).unwrap();

    let loaded = load();
    assert!(
        !loaded.client_id.is_empty(),
        "the ordinary identifier repair still happens"
    );
    assert_eq!(loaded.account_token, "legacy-account");
    assert_eq!(loaded.playback_quality(), PlaybackQuality::Original);
    assert_eq!(
        loaded.playback_quality, None,
        "a parsable old file is not fresh and must not acquire a default choice"
    );

    let saved: Session = serde_json::from_slice(&std::fs::read(t.file()).unwrap()).unwrap();
    assert_eq!(saved.playback_quality(), PlaybackQuality::Original);
    assert_eq!(saved.playback_quality, None);
}

#[test]
fn loading_with_no_file_records_the_gated_fresh_default() {
    let _g = crate::testlock::serial();
    let t = TempSession::new("fresh-quality");
    assert!(!t.file().exists());

    let loaded = load();
    assert_eq!(
        loaded.playback_quality,
        Some(PlaybackQuality::Auto),
        "the production readiness gate gives only a genuinely fresh install Auto"
    );
    let saved: Session = serde_json::from_slice(&std::fs::read(t.file()).unwrap()).unwrap();
    assert_eq!(
        saved.playback_quality,
        Some(PlaybackQuality::Auto),
        "freshness is decided once and stored explicitly"
    );
}

/// **Two writers, one file, and neither may lose the other's work.** Each thread runs exactly
/// the read-modify-write cycle the two real writers run — `auth`'s roster refresh growing
/// `sources`, the search-recents worker growing one profile's terms — and when they are done
/// every update from both must be in the file.
///
/// This is the bug in its own shape: the roster worker re-read the file, a profile pick landed
/// after that read, and its save put the pre-switch profile back — the next boot resuming as
/// the wrong person. `update` makes the read and the write one step under one lock, so the
/// interleaving that loses an update cannot be constructed.
#[test]
fn concurrent_read_modify_writes_never_lose_an_update() {
    let _g = crate::testlock::serial();
    let _t = TempSession::new("lost-update");
    save(&signed_in());

    // A dozen each is plenty and is deliberately not more: every cycle ends in the `sync_all`
    // that makes the rename mean something, and on this host that is an `F_FULLFSYNC` — the
    // whole host suite is meant to cost well under a second.
    const N: usize = 12;
    std::thread::scope(|sc| {
        sc.spawn(|| {
            for i in 0..N {
                update(|s| {
                    let mut next = s.clone();
                    next.sources.push(SourceRef {
                        machine_id: format!("m{i}"),
                        address: "192.168.0.10".into(),
                        port: 32400,
                        token: "tok".into(),
                        ..Default::default()
                    });
                    Some(next)
                });
            }
        });
        sc.spawn(|| {
            for i in 0..N {
                update(|s| {
                    let mut next = s.clone();
                    let mut terms = next.recents_for("uu-1").to_vec();
                    terms.push(format!("term-{i}"));
                    next.set_recents_for("uu-1", terms);
                    Some(next)
                });
            }
        });
    });

    let s = peek();
    assert_eq!(s.client_id, "cid-1", "the credentials survived every cycle");
    assert_eq!(s.account_token, "acct");
    assert_eq!(
        s.sources.len(),
        N,
        "a roster entry was overwritten by the other writer"
    );
    assert_eq!(
        s.recents_for("uu-1").len(),
        N,
        "a search term was overwritten by the other writer"
    );
}

/// **A reader outside the lock never sees half a session.** The reader here deliberately does
/// NOT go through `peek` — that takes the same lock, so it could not observe a torn file even
/// if `save` still truncated in place. It reads the path the way everything else on the device
/// does, which is also the window a crash or a power cut reads through: with `O_TRUNC` the
/// bytes at that path are empty for as long as the write takes, and an unparseable session
/// file is a QR code on the next boot, not a stale roster.
#[test]
fn a_reader_outside_the_lock_never_sees_half_a_session() {
    let _g = crate::testlock::serial();
    let t = TempSession::new("torn");
    save(&signed_in());

    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|sc| {
        sc.spawn(|| {
            for i in 0..20 {
                update(|s| {
                    let mut next = s.clone();
                    // a payload big enough that one `write_all` is several pages — a torn read
                    // must not depend on the file happening to be tiny
                    next.home_users.push(HomeUserRef {
                        uuid: format!("uuid-{i}"),
                        title: format!("A profile with a long enough name to be worth {i} bytes"),
                        thumb: format!("https://plex.direct/photo/:/transcode?url=library%2Fmetadata%2F{i}"),
                        ..Default::default()
                    });
                    Some(next)
                });
            }
            done.store(true, std::sync::atomic::Ordering::Release);
        });
        let file = t.file();
        let mut reads = 0u32;
        while !done.load(std::sync::atomic::Ordering::Acquire) {
            let bytes = std::fs::read(&file).expect("the path always names a complete file");
            let s: Session = serde_json::from_slice(&bytes)
                .unwrap_or_else(|e| panic!("torn session file after {reads} clean reads: {e}"));
            assert_eq!(
                s.client_id, "cid-1",
                "a partial read is a signed-out device"
            );
            reads += 1;
        }
    });
    assert_eq!(peek().home_users.len(), 20);
}

/// `update` must never CREATE a session. A missing or unparseable file reads back as a default
/// `Session`, and writing one field onto that leaves a `client_id`-less file where a live
/// session used to be — the silent sign-out every list in this struct is soft-parsed to
/// prevent, arriving instead by the door built to fix it. It is also what a sign-out racing a
/// background worker would otherwise produce: `clear()` removes the file, and the worker in
/// flight puts a roster back with no credentials under it.
#[test]
fn update_refuses_a_file_that_holds_no_session() {
    let _g = crate::testlock::serial();
    let t = TempSession::new("refuse");

    // no file at all — the state straight after `clear()`
    assert!(!update(|s| Some(Session {
        account_token: "acct".into(),
        ..s.clone()
    })));
    assert!(
        !t.file().exists(),
        "a refused cycle must not create the file it refused to write"
    );

    // a file that does not parse: the same answer, and the bytes are left alone rather than
    // replaced with a freshly minted session
    std::fs::write(t.file(), b"{ not json").unwrap();
    assert!(!update(|_| Some(signed_in())));
    assert_eq!(std::fs::read(t.file()).unwrap(), b"{ not json");
}

/// The roster's own leniency must not weaken the roster the picker draws from: a managed user
/// whose stored `thumb` is a `null` costs that user, not the session.
#[test]
fn a_malformed_home_user_costs_that_tile_and_not_the_session() {
    let s: Session = serde_json::from_str(
        r#"{"client_id":"c","home_users":[{"uuid":"a","title":"A","thumb":null},
                                          {"uuid":"b","title":"B","thumb":"","admin":true}]}"#,
    )
    .expect("one bad tile must not fail the file");
    assert_eq!(s.home_users.len(), 1);
    assert_eq!(s.account(None).name.as_deref(), Some("B"));
}

