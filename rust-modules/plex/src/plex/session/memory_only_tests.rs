//! The persistence mode of a platform with no credential store ([`StoreMode::MemoryOnly`]): the
//! cell [`peek`] answers from is the authority, no credential byte reaches the data directory, and
//! only the install's own file (client id, language, Interface resolution) survives a launch.
//!
//! Every case runs against a real data directory of its own: a canonical root, the legacy candidate
//! list and the install file all live under one temporary tree, so "nothing was written" is a scan
//! of that whole tree and not a probe of one path. The `Durable` cases beside them are the
//! controls: they prove the same scan DOES find the credential when the platform keeps one, so a
//! clean scan under `MemoryOnly` is not a scan that could not have seen it.
use super::*;
use plx_os::tv::secure::{set_mode_for_test, ModeReset, StoreMode};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const ACCOUNT: &str = "mem-acct-3b7c1e90-account-token";
const PMS: &str = "mem-pms-9d42f5a1-server-token";
const USER: &str = "mem-user-5e08c7b3-user-token";
const SHARE: &str = "mem-share-1a6f2d84-source-token";
const SECRETS: [&str; 4] = [ACCOUNT, PMS, USER, SHARE];

/// A data directory of this test's own, and the mode the process runs in while it lives.
struct Install {
    dir: PathBuf,
    mode: Option<ModeReset>,
}

impl Install {
    fn new(tag: &str, mode: StoreMode) -> Install {
        let dir = std::env::temp_dir().join(format!("plxnative-memory-only-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("state")).unwrap();
        std::fs::create_dir_all(dir.join("legacy")).unwrap();
        redirect_for_test(None);
        plx_base::paths::redirect_persistent_state_root_for_test(Some(dir.join("state")));
        redirect_candidates_for_test(Some(vec![dir.join("legacy").join("auth.json")]));
        Install { dir, mode: Some(set_mode_for_test(mode)) }
    }

    /// Change the mode without ending the fixture: a platform does not switch, but a test plants
    /// state under one mode and boots under another.
    fn run_as(&mut self, mode: StoreMode) {
        self.mode = None;
        self.mode = Some(set_mode_for_test(mode));
    }

    /// A new process on the same directory: nothing cached, nothing held in memory.
    fn relaunch(&self) {
        redirect_for_test(None);
    }

    fn install_file(&self) -> PathBuf {
        self.dir.join("legacy").join("auth.language.json")
    }

    fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut found = BTreeMap::new();
        let mut pending = vec![self.dir.clone()];
        while let Some(next) = pending.pop() {
            for entry in std::fs::read_dir(&next).unwrap() {
                let path = entry.unwrap().path();
                if std::fs::symlink_metadata(&path).unwrap().is_dir() {
                    pending.push(path);
                } else {
                    found.insert(path.clone(), std::fs::read(&path).unwrap_or_default());
                }
            }
        }
        found
    }

    /// The files under the whole data directory that hold any byte run of a credential.
    fn files_holding_a_credential(&self) -> Vec<PathBuf> {
        self.files().into_iter()
            .filter(|(_, bytes)| SECRETS.iter().any(|secret| bytes.windows(secret.len()).any(|run| run == secret.as_bytes())))
            .map(|(path, _)| path)
            .collect()
    }

    fn file_names(&self) -> Vec<String> {
        self.files().keys().map(|path| path.file_name().unwrap().to_string_lossy().into_owned()).collect()
    }
}

impl Drop for Install {
    fn drop(&mut self) {
        publish_profile_for_test(None, current_gen().wrapping_add(1));
        redirect_candidates_for_test(None);
        plx_base::paths::redirect_persistent_state_root_for_test(None);
        // Proven, so it also ends a revocation the test left in the cell.
        redirect_for_test(None);
        let _ = std::fs::remove_dir_all(&self.dir);
        drop(self.mode.take());
    }
}

/// `base` signed in: every credential field carries a long, distinctive token.
fn signed_in_over(base: &Session) -> Session {
    Session {
        account_token: ACCOUNT.into(),
        server: ServerRef { machine_id: "aaaa1111".into(), token: PMS.into(), ..Default::default() },
        user: UserRef { uuid: "u-7".into(), token: USER.into(), ..Default::default() },
        sources: vec![SourceRef { machine_id: "bbbb2222".into(), token: SHARE.into(), ..Default::default() }],
        ..base.clone()
    }
}

fn signed_in_with(client_id: &str) -> Session {
    signed_in_over(&Session { client_id: client_id.into(), ..Default::default() })
}

/// What a sign-in does to the session layer: boot (the install id is read or minted), then the one
/// whole-record write only a completed authorization performs, merged over what the disk holds.
fn sign_in(minted: &str) -> async_persistence::LiveWrite {
    load_with_id(|| minted.to_owned());
    replace_after_reauthentication_with_outcome(|_| true, signed_in_over)
        .expect("nothing fenced the sign-in")
        .expect("a sign-in that carries a token writes")
}

fn assert_ephemeral(write: &async_persistence::LiveWrite) {
    assert_eq!(write.classify(), async_persistence::CompletionOutcome::Ephemeral);
    assert!(write.commit.is_none(), "no canonical commit was attempted");
    assert_eq!(write.outcome, async_persistence::PersistOutcome::Memory);
}

#[test]
fn a_memory_only_sign_in_leaves_no_credential_byte_in_the_data_directory() {
    let _serial = plx_base::testlock::serial();
    let install = Install::new("sign-in", StoreMode::MemoryOnly);
    let write = sign_in("install-1");
    assert_ephemeral(&write);
    assert!(write.install_choices_kept());

    let held = peek();
    assert_eq!(held.account_token, ACCOUNT, "this run is signed in");
    assert_eq!(held.server.token, PMS);
    assert_eq!(held.user.token, USER);
    assert_eq!(held.client_id, "install-1");
    assert!(!cache_revoked());

    assert_eq!(install.files_holding_a_credential(), Vec::<PathBuf>::new());
    assert_eq!(install.file_names(), ["auth.language.json"], "the install's own file is the one thing written");
}

/// The event log says "memory only" once per sign-in, though a sign-in writes twice (discovery, then
/// the final commit), and again for the next account the cell takes.
#[test]
fn a_memory_only_sign_in_logs_one_line_however_many_writes_it_takes() {
    let _serial = plx_base::testlock::serial();
    let _install = Install::new("one-log-line", StoreMode::MemoryOnly);
    let said = |log: &str| log.matches("signed in for this run only").count();
    let read_log = || std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default();
    let log = plx_base::eventlog::with_private_log(|| {
        assert_ephemeral(&sign_in("install-1"));
        save_fresh_reauthentication(&signed_in_with("install-1"));
        read_log()
    });
    assert_eq!(said(&log), 1, "{log}");
    let log = plx_base::eventlog::with_private_log(|| {
        clear_for_erase(false, None);
        save_fresh_reauthentication(&signed_in_with("install-1"));
        save_fresh_reauthentication(&signed_in_with("install-1"));
        read_log()
    });
    assert_eq!(said(&log), 1, "a new sign-in after a sign-out says it again, once: {log}");
}

#[test]
fn the_same_sign_in_on_a_durable_platform_does_write_the_credential() {
    let _serial = plx_base::testlock::serial();
    let install = Install::new("durable-control", StoreMode::Durable);
    let write = sign_in("install-1");
    assert!(matches!(write.classify(), async_persistence::CompletionOutcome::Durable(_)));
    assert!(write.commit.is_some());
    assert!(!install.files_holding_a_credential().is_empty(),
        "the scan sees a credential where one was written, so a clean scan means something");
}

#[test]
fn an_edit_after_a_memory_only_sign_in_neither_refuses_nor_loses_the_session() {
    let _serial = plx_base::testlock::serial();
    let install = Install::new("edit-after-sign-in", StoreMode::MemoryOnly);
    assert_ephemeral(&sign_in("install-1"));
    let stamp = crate::plex::grant::account_key(ACCOUNT);
    publish_seat_for_test(Some(UserRef { uuid: "u-7".into(), ..Default::default() }), current_gen().wrapping_add(1), stamp.clone());

    // A per-profile pick: the door `media::route::prefs::choose` writes through.
    let write = prefs::ProfileWrite::capture().expect("the profile is seated");
    assert!(prefs::persist_profile_hud(&write, "a-hud"), "an ephemeral write took the edit");
    let held = peek();
    assert_eq!(held.account_token, ACCOUNT, "the edit did not lose the session");
    assert!(prefs::own_prefs(&held.prefs.profile_prefs, "u-7", &stamp).is_some(), "and it landed");

    // A whole-session edit through the ordinary door.
    let edited = update_with_outcome(|cur| {
        let mut next = cur.clone();
        next.prefs.auto_sign_in = true;
        Some(next)
    }).expect("an edit over a signed-in cell is not refused");
    assert_ephemeral(&edited);
    assert!(peek().prefs.auto_sign_in);
    assert_eq!(peek().server.token, PMS);

    // The two install-wide choices reach their file, so their picker may promise the next launch.
    assert!(set_language(plx_os::i18n::Preference::Be));
    assert!(set_ui_resolution(UiResolution::Hd720));
    let (kept, id) = install_preferences::read().expect("the install file");
    assert_eq!(kept, Retained { language: plx_os::i18n::Preference::Be, ui_resolution: UiResolution::Hd720 });
    assert_eq!(id.as_deref(), Some("install-1"));
    assert_eq!(peek().account_token, ACCOUNT);
    assert_eq!(install.files_holding_a_credential(), Vec::<PathBuf>::new());
}

#[test]
fn a_language_pick_is_not_promised_when_the_install_file_cannot_be_written() {
    let _serial = plx_base::testlock::serial();
    let install = Install::new("install-file-unwritable", StoreMode::MemoryOnly);
    assert_ephemeral(&sign_in("install-1"));
    // A file where the candidate's directory should be: no write can land.
    let blocked = install.dir.join("blocked");
    std::fs::write(&blocked, b"not a directory").unwrap();
    redirect_candidates_for_test(Some(vec![blocked.join("auth.json")]));
    assert!(!set_language(plx_os::i18n::Preference::Be), "the next launch cannot be promised");
    assert!(!set_ui_resolution(UiResolution::Hd720));
    assert_eq!(peek().account_token, ACCOUNT, "the session is still this run's");
}

#[test]
fn sign_out_then_erase_leaves_nothing() {
    let _serial = plx_base::testlock::serial();
    let install = Install::new("sign-out-erase", StoreMode::MemoryOnly);
    assert_ephemeral(&sign_in("install-1"));
    assert!(set_language(plx_os::i18n::Preference::Be));

    // Sign-out: the session leaves memory, the install's own choices stay.
    let signed_out = clear_for_erase(false, None);
    assert_eq!(signed_out.outcome, ClearOutcome::Durable { legacy_swept: true });
    assert!(signed_out.preference_failures.is_empty());
    assert_eq!(signed_out.retained.language, plx_os::i18n::Preference::Be);
    assert!(peek().account_token.is_empty() && peek().server.token.is_empty());
    assert!(cache_revoked());
    assert_eq!(install.files_holding_a_credential(), Vec::<PathBuf>::new());
    assert_eq!(install.file_names(), ["auth.language.json"], "no revocation marker, no credential file");
    let (kept, id) = install_preferences::read().expect("sign-out keeps the install file");
    assert_eq!(kept.language, plx_os::i18n::Preference::Be);
    assert_eq!(id.as_deref(), Some("install-1"), "the next sign-in is the same install");

    // The revoked cell answers the id from the file and stays revoked.
    assert_eq!(load_login_client_id(), "install-1");
    assert!(cache_revoked());
    assert!(update_with_outcome(|cur| Some(cur.clone())).is_none(), "a revoked cell takes no edit");

    // Delete all local data: the install file goes too.
    let erased = clear_for_erase(true, None);
    assert_eq!(erased.outcome, ClearOutcome::Durable { legacy_swept: true });
    assert!(erased.preference_failures.is_empty());
    assert_eq!(erased.retained, Retained::default());
    assert_eq!(install.files(), BTreeMap::new(), "the data directory holds no file at all");

    // Only a fresh authorization ends the revocation.
    save_fresh_reauthentication(&signed_in_with("install-2"));
    assert_eq!(peek().account_token, ACCOUNT);
    assert!(!cache_revoked());
}

#[test]
fn a_new_process_ignores_planted_credentials_and_keeps_only_the_install_choices() {
    let _serial = plx_base::testlock::serial();
    let mut install = Install::new("relaunch", StoreMode::Durable);
    // An earlier durable build left a canonical record and a legacy file, with per-profile settings.
    let mut old = signed_in_with("old-install");
    old.prefs.auto_sign_in = true;
    old.prefs.trailer_autoplay = false;
    save(&old);
    std::fs::write(install.dir.join("legacy").join("auth.json"), serde_json::to_vec(&old).unwrap()).unwrap();
    assert!(!install.files_holding_a_credential().is_empty(), "setup: the credentials are on disk");

    install.run_as(StoreMode::MemoryOnly);
    let kept = Retained { language: plx_os::i18n::Preference::Be, ui_resolution: UiResolution::Hd720 };
    assert!(install_preferences::save_v2(kept, "planted-install"));
    let planted = install.files();

    install.relaunch();
    let booted = load_with_id(|| panic!("a stored install id is never re-minted"));
    assert_eq!(booted.client_id, "planted-install", "the same install");
    assert_eq!(booted.language, plx_os::i18n::Preference::Be);
    assert_eq!(booted.ui_resolution(), UiResolution::Hd720);
    assert!(booted.account_token.is_empty(), "the planted credentials are not read: sign in again");
    assert!(booted.server.token.is_empty() && booted.user.token.is_empty());
    assert!(!booted.prefs.auto_sign_in, "per-profile and other settings are forgotten each launch");
    assert!(booted.prefs.trailer_autoplay, "the first-run defaults apply");
    assert_eq!(install.files(), planted, "booting touched neither the planted credentials nor the install file");

    // Signing in again changes no file either: the choices it would write are already the file's.
    assert_ephemeral(&sign_in("never-minted"));
    assert_eq!(peek().client_id, "planted-install");
    assert_eq!(install.files(), planted);
}

#[test]
fn a_boot_with_no_install_file_mints_an_id_and_keeps_it_for_the_next_launch() {
    let _serial = plx_base::testlock::serial();
    let install = Install::new("first-run", StoreMode::MemoryOnly);
    let first = load_with_id(|| "minted-1".into());
    assert_eq!(first.client_id, "minted-1");
    assert!(first.prefs.trailer_autoplay);
    assert_eq!(install_preferences::read().and_then(|(_, id)| id).as_deref(), Some("minted-1"));
    assert_eq!(install.file_names(), ["auth.language.json"]);

    install.relaunch();
    let second = load_with_id(|| panic!("the id is on file"));
    assert_eq!(second.client_id, "minted-1", "the same client id after a restart");
}

/// A canonical root that is a regular file: every canonical commit fails with an IO error, which
/// is the failure the legacy fallback exists for.
fn break_the_canonical_store(install: &Install) {
    let broken = install.dir.join("canonical-is-a-file");
    std::fs::write(&broken, b"not a directory").unwrap();
    plx_base::paths::redirect_persistent_state_root_for_test(Some(broken));
}

fn write_with_a_failing_canonical_store(install: &Install) -> async_persistence::LiveWrite {
    break_the_canonical_store(install);
    let _io = io();
    save_locked_with_authority(&signed_in_with("install-1"), SaveAuthority::FreshReauthentication)
}

#[test]
fn a_failing_canonical_write_drives_the_legacy_fallback_on_a_durable_platform() {
    let _serial = plx_base::testlock::serial();
    let install = Install::new("fallback-control", StoreMode::Durable);
    let write = write_with_a_failing_canonical_store(&install);
    assert!(matches!(write.commit, Some(persistence::CanonicalCommit::Failed(_))), "the canonical write failed");
    assert_eq!(write.outcome, async_persistence::PersistOutcome::PersistedPlaintext, "and the fallback wrote");
    let fallback = install.dir.join("legacy").join("auth.json");
    assert!(std::fs::read(&fallback).unwrap().windows(ACCOUNT.len()).any(|run| run == ACCOUNT.as_bytes()));
}

#[test]
fn a_failing_canonical_write_never_reaches_the_legacy_fallback_in_memory_only() {
    let _serial = plx_base::testlock::serial();
    let install = Install::new("fallback-memory-only", StoreMode::MemoryOnly);
    let write = write_with_a_failing_canonical_store(&install);
    assert_ephemeral(&write);
    assert_eq!(install.files_holding_a_credential(), Vec::<PathBuf>::new());
    assert!(!install.dir.join("legacy").join("auth.json").exists(), "no plaintext credential file");
    assert!(!install.dir.join("legacy").join("auth.session-revoked").exists());
    assert_eq!(peek().account_token, ACCOUNT, "and the cell still holds the session");
}

/// Under a refused canonical store a sign-out must not leave revocation markers or sweep files in
/// memory-only mode either: there is nothing it could have written.
#[test]
fn a_memory_only_sign_out_over_a_failing_canonical_store_still_completes() {
    let _serial = plx_base::testlock::serial();
    let install = Install::new("sign-out-failing-store", StoreMode::MemoryOnly);
    assert_ephemeral(&write_with_a_failing_canonical_store(&install));
    let out = clear_for_erase(false, None);
    assert_eq!(out.outcome, ClearOutcome::Durable { legacy_swept: true });
    assert!(peek().account_token.is_empty());
    assert_eq!(install.files_holding_a_credential(), Vec::<PathBuf>::new());
    // Beside the file that stands in for the broken canonical root, only the install file exists.
    let mut written = install.file_names();
    written.retain(|name| name != "canonical-is-a-file");
    assert_eq!(written, ["auth.language.json"]);
}

// ---- the install file ------------------------------------------------------------------------

fn read_file(path: &Path) -> String {
    String::from_utf8(std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn the_version_1_file_is_byte_identical_to_what_webos_and_desktop_have_always_written() {
    let _serial = plx_base::testlock::serial();
    let install = Install::new("v1-bytes", StoreMode::Durable);
    let be = plx_os::i18n::Preference::Be;
    assert!(install_preferences::save(Retained { language: be, ui_resolution: UiResolution::Hd720 }));
    assert_eq!(read_file(&install.install_file()), r#"{"version":1,"language":"be","ui_resolution":"720p"}"#);
    assert!(install_preferences::save(Retained { language: be, ui_resolution: UiResolution::Auto }));
    assert_eq!(read_file(&install.install_file()), r#"{"version":1,"language":"be"}"#);
    assert_eq!(install_preferences::load(), Some(Retained { language: be, ui_resolution: UiResolution::Auto }));
    assert_eq!(install_preferences::read().map(|(_, id)| id), Some(None), "a version 1 file carries no id");
}

#[test]
fn the_version_2_file_carries_the_client_id_and_round_trips() {
    let _serial = plx_base::testlock::serial();
    let install = Install::new("v2-file", StoreMode::MemoryOnly);
    let kept = Retained { language: plx_os::i18n::Preference::Be, ui_resolution: UiResolution::Hd720 };
    assert!(install_preferences::save_v2(kept, "id-1"));
    assert_eq!(read_file(&install.install_file()),
        r#"{"version":2,"language":"be","ui_resolution":"720p","client_id":"id-1"}"#);
    assert_eq!(install_preferences::read(), Some((kept, Some("id-1".to_owned()))));
    assert_eq!(install_preferences::load(), Some(kept), "load accepts version 2 too");

    // An unchanged write leaves the file alone: a write renames a new inode into place.
    use std::os::unix::fs::MetadataExt;
    let inode = std::fs::metadata(install.install_file()).unwrap().ino();
    assert!(install_preferences::save_v2(kept, "id-1"));
    assert_eq!(std::fs::metadata(install.install_file()).unwrap().ino(), inode);

    // An empty id has nothing to carry: the version 1 file, and none at all for the defaults.
    assert!(install_preferences::erase().is_empty());
    assert!(install_preferences::save_v2(Retained::default(), ""));
    assert_eq!(install.files(), BTreeMap::new());
    assert!(install_preferences::save_v2(kept, ""));
    assert_eq!(read_file(&install.install_file()), r#"{"version":1,"language":"be","ui_resolution":"720p"}"#);
}

#[test]
fn a_file_that_is_not_a_well_formed_version_1_or_2_is_refused() {
    let _serial = plx_base::testlock::serial();
    let install = Install::new("refusals", StoreMode::MemoryOnly);
    let accepted = |body: &str| {
        std::fs::write(install.install_file(), body).unwrap();
        install_preferences::read()
    };
    assert!(accepted(r#"{"version":1,"language":"be"}"#).is_some());
    assert!(accepted(r#"{"version":2,"language":"be","client_id":"id-1"}"#).is_some());
    for refused in [
        r#"{"version":1,"language":"be","client_id":"id-1"}"#,
        r#"{"version":2,"language":"be"}"#,
        r#"{"version":2,"language":"be","client_id":""}"#,
        r#"{"version":3,"language":"be","client_id":"id-1"}"#,
        r#"{"version":2,"language":"be","client_id":"id-1","account_token":"x"}"#,
        r#"not json"#,
    ] {
        assert_eq!(accepted(refused), None, "{refused}");
    }
}
