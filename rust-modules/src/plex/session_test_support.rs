//! Shared fixtures and helpers for the `plex::session` test modules split out below.

use super::*;

/// The file a signed-in device holds today, once discovery has reached two servers. Written
/// as literal JSON rather than by serialising a `Session`, because the thing under test is
/// what happens when the bytes on disk are not what this build expects.
pub(super) fn two_server_json() -> &'static str {
    r#"{"client_id":"cid-1","account_token":"acct",
        "server":{"name":"Mac mini","machine_id":"aaaa1111","address":"192.168.0.10",
                  "port":32400,"token":"tok-own"},
        "user":{"id":7,"uuid":"u-7","title":"Gleb","thumb":"","token":"tok-user"},
        "home_users":[{"uuid":"u-7","title":"Gleb","thumb":"","protected":false,"admin":true}],
        "sources":[
          {"machine_id":"aaaa1111","name":"Mac mini","shared_by":"","owned":true,
           "address":"192.168.0.10","port":32400,"token":"tok-own"},
          {"machine_id":"bbbb2222","name":"nas-home","shared_by":"friend","owned":false,
           "address":"203.0.113.9","port":31234,"token":"tok-share"}],
        "home_pins":[{"user":"u-7","asked":true,
                      "on":[{"machine_id":"bbbb2222","key":1}],
                      "off":[{"machine_id":"aaaa1111","key":1}]}]}"#
}

pub(super) fn dialable_home(users: usize, uuid: &str, auto: bool) -> Session {
    let home_users = (0..users)
        .map(|i| HomeUserRef {
            uuid: format!("u-{i}"),
            title: format!("User {i}"),
            protected: i == 0,
            admin: i == 0,
            ..Default::default()
        })
        .collect();
    Session {
        client_id: "c".into(),
        account_token: "acct".into(),
        server: ServerRef {
            address: "192.168.0.10".into(),
            port: 32400,
            token: "t".into(),
            ..Default::default()
        },
        user: UserRef {
            uuid: uuid.into(),
            token: "ut".into(),
            ..Default::default()
        },
        home_users,
        auto_sign_in: auto,
        ..Default::default()
    }
}

// ---- The FILE half: one writer at a time, and a whole file or none of it -------------------
//
// Everything below drives the real `save`/`peek`/`update` against a real file, so it needs a
// file it may have. `TempSession` redirects [`TEST_FILE`] — a crate global, which is why every
// test here holds `crate::testlock::serial()` for its whole body (`src/lib.rs`): several
// modules call `session::load` indirectly, and one running in parallel would read and WRITE
// the file being graded.
//
// This is a LOCAL, second `TempSession` distinct from the crate-level `pub(crate) TempSession`
// declared earlier in `session.rs` (the one `browse`/`onboard`/`auth` share) — the duplication
// predates this split and is preserved as-is rather than folded in, to keep this split a pure
// move. Callers that need the local one explicitly import it (`use
// super::test_support::TempSession;`) because a bare `use super::*;` also brings the
// crate-level struct of the same name into scope, and two same-named glob imports are ambiguous.

/// Point this module's file at a directory of this test's own, and take it back on drop.
pub(super) struct TempSession {
    pub(super) dir: std::path::PathBuf,
}

impl TempSession {
    pub(super) fn new(tag: &str) -> TempSession {
        // `env::temp_dir()` is right HERE and wrong in `dev.rs` (whose test warns against it):
        // there a literal path stops meeting a read that resolves its own root, while this
        // test is choosing the path that BOTH halves resolve to.
        let dir = std::env::temp_dir()
            .join(format!("plxnative-session-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); // a previous run that died mid-test
        std::fs::create_dir_all(&dir).expect("a writable temp dir");
        super::redirect_for_test(Some(dir.join("auth.json")));
        TempSession { dir }
    }
    pub(super) fn file(&self) -> std::path::PathBuf {
        self.dir.join("auth.json")
    }
    pub(super) fn tmp(&self) -> std::path::PathBuf {
        self.dir.join("auth.json.tmp")
    }
}

impl Drop for TempSession {
    fn drop(&mut self) {
        super::redirect_for_test(None);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub(super) fn signed_in() -> Session {
    Session {
        client_id: "cid-1".into(),
        account_token: "acct".into(),
        ..Default::default()
    }
}

pub(super) fn creds(uuid: &str, token: &str, pin: Option<&str>) -> ProfileCreds {
    ProfileCreds {
        uuid: uuid.into(),
        user: UserRef {
            uuid: uuid.into(),
            token: token.into(),
            ..Default::default()
        },
        server: ServerRef {
            machine_id: "m".into(),
            address: "10.0.0.5".into(),
            port: 32400,
            token: token.into(),
            origin_url: "https://10-0-0-5.abc.plex.direct:32400".into(),
            ..Default::default()
        },
        sources: vec![],
        pin: pin.map(PinVerifier::new),
        extensions: Default::default(),
    }
}
