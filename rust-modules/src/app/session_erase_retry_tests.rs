//! `SessionAdapter`'s erase against a session file the process cannot remove: the erase stays
//! pending and retryable and is never reported complete early. The session FILE half is
//! `plex::session`'s; the adapter that drives the erase sits above that layer, so the grading lives
//! here (it was one of `plex::session`'s persistence tests).

use crate::plex::session::{cache_revoked, redirect_for_test, save, Session};

/// The session file redirected into a directory of this test's own, handed back on drop. The same
/// scratch `plex::session`'s persistence tests use; theirs is private to that module.
struct ScratchSession {
    dir: std::path::PathBuf,
}

impl ScratchSession {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir()
            .join(format!("plxnative-session-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); // a previous run that died mid-test
        std::fs::create_dir_all(&dir).expect("a writable temp dir");
        redirect_for_test(Some(dir.join("auth.json")));
        Self { dir }
    }
    fn file(&self) -> std::path::PathBuf {
        self.dir.join("auth.json")
    }
}

impl Drop for ScratchSession {
    fn drop(&mut self) {
        redirect_for_test(None);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn signed_in() -> Session {
    Session {
        client_id: "cid-1".into(),
        account_token: "acct".into(),
        ..Default::default()
    }
}

/// Preserve permissions even when the regression assertion panics.
struct RestorePermissions(Vec<(std::path::PathBuf, std::fs::Permissions)>);
impl RestorePermissions {
    fn set(paths: &[(&std::path::Path, u32)]) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let saved = Self(paths.iter().map(|(path, _)| {
            (path.to_path_buf(), std::fs::metadata(path).unwrap().permissions())
        }).collect());
        for (path, mode) in paths {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(*mode)).unwrap();
        }
        saved
    }
}
impl Drop for RestorePermissions {
    fn drop(&mut self) {
        for (path, permissions) in &self.0 {
            let _ = std::fs::set_permissions(path, permissions.clone());
        }
    }
}

#[test]
fn p1_adapter_does_not_complete_an_erase_with_an_incomplete_sweep() {
    let _serial = plx_base::testlock::serial();
    let file = ScratchSession::new("p1-adapter-erase");
    save(&signed_in());
    let _permissions = RestorePermissions::set(&[
        (file.file().parent().unwrap(), 0o500), (file.file().as_path(), 0o400),
    ]);
    let mt = unsafe { plx_base::task::MainThread::assume() };
    let mut adapter = crate::app::adapters::session::SessionAdapter::live_resources_for_test(&mt, false);
    let mut meta = crate::stores::metadata::MetadataStore::default();
    assert!(adapter.begin_erase(1, false, &mut meta).is_none());
    plx_base::storage_worker::drain_for_test();
    assert!(adapter.take_erased(&mut meta).is_none(), "incomplete erase must remain retryable");
    assert!(cache_revoked());
    drop(_permissions);
    struct ResetClock;
    impl Drop for ResetClock { fn drop(&mut self) { crate::app::clock::set_replay(0); } }
    let _clock = ResetClock;
    crate::app::clock::set_replay(crate::app::clock::now().wrapping_add(1_000));
    let completed = adapter.take_erased(&mut meta);
    plx_base::storage_worker::drain_for_test();
    let completed = completed.or_else(|| adapter.take_erased(&mut meta));
    assert!(matches!(completed, Some(crate::auth::owner::SessionEvent::Erased { epoch: 1, .. })),
        "the same pending erase completes only after a successful retry");
    assert!(cache_revoked());
}
