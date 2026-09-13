//! Published-0.6 direct migration/reopen matrix.
//!
//! Fixtures are synthetic, but their JSON shapes are taken from the tagged 0.6 record schemas.
//! This exercises the Session entry points rather than treating `migrate_exact` as a substitute
//! for a boot, an edit, a sign-out, or a secure-envelope read.

use crate::storage::{RecordKey, RecordState, RecordStore};
use serde_json::Value;
use std::path::PathBuf;

const TAGS: &[&str] = &["v0.6.0", "v0.6.1", "v0.6.2", "v0.6.3", "v0.6.4", "v0.6.5"];

fn session_fixture(tag: &str) -> &'static str {
    match tag {
        "v0.6.0" => include_str!("../../../../tests/fixtures/persistence/v0.6.0/session.json"),
        "v0.6.1" => include_str!("../../../../tests/fixtures/persistence/v0.6.1/session.json"),
        "v0.6.2" => include_str!("../../../../tests/fixtures/persistence/v0.6.2/session.json"),
        "v0.6.3" => include_str!("../../../../tests/fixtures/persistence/v0.6.3/session.json"),
        "v0.6.4" => include_str!("../../../../tests/fixtures/persistence/v0.6.4/session.json"),
        "v0.6.5" => include_str!("../../../../tests/fixtures/persistence/v0.6.5/session.json"),
        _ => panic!("unknown fixture tag {tag}"),
    }
}

fn consent_fixture(tag: &str, yes: bool) -> &'static str {
    match (tag, yes) {
        ("v0.6.0", true) => {
            include_str!("../../../../tests/fixtures/persistence/v0.6.0/consent-yes.json")
        }
        ("v0.6.0", false) => {
            include_str!("../../../../tests/fixtures/persistence/v0.6.0/consent-no.json")
        }
        ("v0.6.1", true) => {
            include_str!("../../../../tests/fixtures/persistence/v0.6.1/consent-yes.json")
        }
        ("v0.6.1", false) => {
            include_str!("../../../../tests/fixtures/persistence/v0.6.1/consent-no.json")
        }
        ("v0.6.2", true) => {
            include_str!("../../../../tests/fixtures/persistence/v0.6.2/consent-yes.json")
        }
        ("v0.6.2", false) => {
            include_str!("../../../../tests/fixtures/persistence/v0.6.2/consent-no.json")
        }
        ("v0.6.3", true) => {
            include_str!("../../../../tests/fixtures/persistence/v0.6.3/consent-yes.json")
        }
        ("v0.6.3", false) => {
            include_str!("../../../../tests/fixtures/persistence/v0.6.3/consent-no.json")
        }
        ("v0.6.4", true) => {
            include_str!("../../../../tests/fixtures/persistence/v0.6.4/consent-yes.json")
        }
        ("v0.6.4", false) => {
            include_str!("../../../../tests/fixtures/persistence/v0.6.4/consent-no.json")
        }
        ("v0.6.5", true) => {
            include_str!("../../../../tests/fixtures/persistence/v0.6.5/consent-yes.json")
        }
        ("v0.6.5", false) => {
            include_str!("../../../../tests/fixtures/persistence/v0.6.5/consent-no.json")
        }
        _ => panic!("unknown fixture tag {tag}"),
    }
}

fn envelope_fixture(tag: &str) -> &'static str {
    match tag {
        "v0.6.0" => {
            include_str!("../../../../tests/fixtures/persistence/v0.6.0/secure-envelope-v1.json")
        }
        "v0.6.1" => {
            include_str!("../../../../tests/fixtures/persistence/v0.6.1/secure-envelope-v1.json")
        }
        "v0.6.2" => {
            include_str!("../../../../tests/fixtures/persistence/v0.6.2/secure-envelope-v1.json")
        }
        "v0.6.3" => {
            include_str!("../../../../tests/fixtures/persistence/v0.6.3/secure-envelope-v1.json")
        }
        "v0.6.4" => {
            include_str!("../../../../tests/fixtures/persistence/v0.6.4/secure-envelope-v1.json")
        }
        "v0.6.5" => {
            include_str!("../../../../tests/fixtures/persistence/v0.6.5/secure-envelope-v1.json")
        }
        _ => panic!("unknown fixture tag {tag}"),
    }
}

struct FixtureRoot {
    dir: PathBuf,
    root: PathBuf,
}

impl FixtureRoot {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "plxnative-persistence-matrix-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self {
            root: dir.join("state"),
            dir,
        }
    }

    fn legacy_session(&self) -> PathBuf {
        self.dir.join("auth.json")
    }
    fn legacy_consent(&self) -> PathBuf {
        self.dir.join("telemetry.json")
    }
}

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        super::redirect_for_test(None);
        crate::telemetry::redirect_for_test(None);
        crate::paths::redirect_persistent_state_root_for_test(None);
        crate::keymanager::disarm_for_test();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn assert_session_fields(tag: &str, actual: &super::Session, expected: &super::Session) {
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(expected).unwrap(),
        "{tag}: every typed session field"
    );
    assert_eq!(actual.client_id, expected.client_id, "{tag}: client id");
    assert_eq!(
        actual.account_token, expected.account_token,
        "{tag}: account token"
    );
    assert_eq!(
        actual.server.machine_id, expected.server.machine_id,
        "{tag}: primary source id"
    );
    assert_eq!(
        actual.server.name, expected.server.name,
        "{tag}: primary source name"
    );
    assert_eq!(
        (
            actual.user.id,
            actual.user.uuid.as_str(),
            actual.user.title.as_str(),
            actual.user.thumb.as_str(),
            actual.user.token.as_str(),
        ),
        (
            expected.user.id,
            expected.user.uuid.as_str(),
            expected.user.title.as_str(),
            expected.user.thumb.as_str(),
            expected.user.token.as_str(),
        ),
        "{tag}: selected profile identity"
    );
    assert_eq!(actual.home_users.len(), 1, "{tag}: profile metadata");
    assert_eq!(
        actual.home_users[0].uuid, expected.home_users[0].uuid,
        "{tag}: roster profile"
    );
    assert_eq!(actual.sources.len(), 1, "{tag}: source metadata");
    assert_eq!(
        actual.sources[0].machine_id, expected.sources[0].machine_id,
        "{tag}: source id"
    );
    assert_eq!(
        actual.home_pins, expected.home_pins,
        "{tag}: exact pin setting"
    );
    let pins = actual.home_pins.first().expect("fixture has a pin setting");
    assert_eq!(pins.user, actual.user.uuid, "{tag}: pin owner");
    assert!(pins.asked, "{tag}: profile was asked");
    assert_eq!(pins.on.len(), 1, "{tag}: pinned-on setting");
    assert_eq!(pins.off.len(), 1, "{tag}: pinned-off setting");
    assert_eq!(
        (
            pins.on[0].machine_id.as_str(),
            pins.on[0].key,
            pins.off[0].machine_id.as_str(),
            pins.off[0].key,
        ),
        (
            expected.home_pins[0].on[0].machine_id.as_str(),
            expected.home_pins[0].on[0].key,
            expected.home_pins[0].off[0].machine_id.as_str(),
            expected.home_pins[0].off[0].key,
        ),
        "{tag}: exact on/off library identities"
    );
    assert_eq!(
        actual.recent_searches, expected.recent_searches,
        "{tag}: exact profile-scoped recents"
    );
    assert_eq!(
        actual.playback_quality(),
        expected.playback_quality(),
        "{tag}: quality setting"
    );
}

#[test]
fn every_published_06_fixture_is_byte_exact_at_the_migration_boundary() {
    let _g = crate::testlock::serial();
    for tag in TAGS {
        let fixture = FixtureRoot::new(&format!("{tag}-exact-payload"));
        let legacy = fixture.legacy_session();
        let bytes = session_fixture(tag).as_bytes();
        std::fs::write(&legacy, bytes).unwrap();
        super::redirect_for_test(Some(legacy.clone()));

        let (commit, source) = super::persistence::migrate_exact(&legacy, bytes);
        assert!(
            matches!(commit, super::persistence::CanonicalCommit::Durable { .. }),
            "{tag}: exact migration commit"
        );
        assert_eq!(source, legacy, "{tag}: migration source identity");
        let store = crate::storage::open(fixture.root.clone()).unwrap();
        let Some(crate::storage::Record {
            state: RecordState::Data { payload },
            ..
        }) = store.load(RecordKey::Session).unwrap()
        else {
            panic!("{tag}: exact migration did not write canonical data");
        };
        assert_eq!(
            payload.as_bytes(),
            bytes,
            "{tag}: first canonical payload before typed normalization"
        );
    }
}

#[test]
fn every_published_06_session_fixture_migrates_reopens_and_accepts_a_routine_edit() {
    let _g = crate::testlock::serial();
    for tag in TAGS {
        let fixture = FixtureRoot::new(tag);
        let legacy = fixture.legacy_session();
        let expected: super::Session = serde_json::from_str(session_fixture(tag)).unwrap();
        std::fs::write(&legacy, session_fixture(tag)).unwrap();
        super::redirect_for_test(Some(legacy.clone()));

        let migrated = super::load();
        assert_session_fields(tag, &migrated, &expected);

        let store = crate::storage::open(fixture.root.clone()).unwrap();
        let Some(record) = store.load(RecordKey::Session).unwrap() else {
            panic!("{tag}: Session::load did not create a canonical record");
        };
        assert!(
            matches!(record.state, RecordState::Data { .. }),
            "{tag}: canonical session state"
        );

        let edited_term = format!("routine-edit-{tag}");
        assert!(
            super::update_ordinary(|current| {
                let mut next = current.clone();
                next.recent_searches[0].terms.push(edited_term.clone());
                let pins = &mut next.home_pins[0];
                std::mem::swap(&mut pins.on, &mut pins.off);
                Some(next)
            }),
            "{tag}: Session::update_ordinary must admit a routine edit"
        );
        let revision = super::ordinary_persistence_revision();
        crate::storage_worker::drain_for_test();
        let status = super::poll_ordinary_persistence();
        assert_eq!(status.latest_revision, revision, "{tag}: receipt revision");
        assert_eq!(
            status.latest,
            Some(super::async_persistence::LatestStatus::Durable),
            "{tag}: ordinary edit receipt"
        );
        assert_eq!(
            status.durable_revision,
            Some(revision),
            "{tag}: durable ordinary revision"
        );

        // Clear the process snapshot while retaining the fixture's canonical root.
        super::redirect_for_test(Some(legacy));
        let reopened = super::load();
        let mut expected_after_edit = expected.clone();
        expected_after_edit.recent_searches[0]
            .terms
            .push(edited_term.clone());
        let pins = &mut expected_after_edit.home_pins[0];
        std::mem::swap(&mut pins.on, &mut pins.off);
        assert_session_fields(tag, &reopened, &expected_after_edit);
        assert!(
            reopened.recent_searches[0]
                .terms
                .iter()
                .any(|term| term == &edited_term),
            "{tag}: routine edit survived canonical reopen"
        );
    }
}

#[test]
fn every_published_06_consent_fixture_preserves_yes_or_no_identifiers_scopes_and_declines() {
    let _g = crate::testlock::serial();
    for tag in TAGS {
        for yes in [true, false] {
            let fixture =
                FixtureRoot::new(&format!("{tag}-consent-{}", if yes { "yes" } else { "no" }));
            let legacy = fixture.legacy_consent();
            let expected: Value = serde_json::from_str(consent_fixture(tag, yes)).unwrap();
            std::fs::write(&legacy, expected.to_string()).unwrap();
            crate::telemetry::redirect_for_test(Some(legacy.clone()));

            let loaded = crate::telemetry::persistence::load(std::slice::from_ref(&legacy));
            let actual = serde_json::to_value(&loaded).unwrap();
            assert_eq!(
                actual["asked_version"], expected["asked_version"],
                "{tag} yes={yes}: policy version"
            );
            assert_eq!(
                actual["errors"], expected["errors"],
                "{tag} yes={yes}: errors choice"
            );
            assert_eq!(
                actual["usage"], expected["usage"],
                "{tag} yes={yes}: usage choice"
            );
            assert_eq!(
                actual["install_id"], expected["install_id"],
                "{tag} yes={yes}: usage identifier"
            );
            assert_eq!(
                actual["errors_id"], expected["errors_id"],
                "{tag} yes={yes}: errors identifier"
            );
            if *tag >= "v0.6.3" {
                for field in [
                    "errors_scope",
                    "usage_scope",
                    "errors_declined_scope",
                    "usage_declined_scope",
                ] {
                    assert_eq!(actual[field], expected[field], "{tag} yes={yes}: {field}");
                }
            }
            assert_eq!(
                crate::telemetry::persistence::record_with_legacy(&loaded, &[]),
                crate::telemetry::persistence::PersistResult::Durable,
                "{tag} yes={yes}: canonical consent rewrite"
            );
            let reopened = crate::telemetry::persistence::load(&[]);
            assert_eq!(
                serde_json::to_value(reopened).unwrap(),
                actual,
                "{tag} yes={yes}: canonical reopen"
            );
        }
    }
}

#[test]
fn clear_is_terminal_even_if_a_stale_legacy_session_returns() {
    let _g = crate::testlock::serial();
    let fixture = FixtureRoot::new("stale-after-clear");
    let legacy = fixture.legacy_session();
    std::fs::write(&legacy, session_fixture("v0.6.5")).unwrap();
    super::redirect_for_test(Some(legacy.clone()));
    assert!(
        !super::load().account_token.is_empty(),
        "legacy fixture signed in before clear"
    );
    assert!(
        super::clear(),
        "Session::clear must durably write its terminal record"
    );

    std::fs::write(&legacy, session_fixture("v0.6.5")).unwrap();
    super::redirect_for_test(Some(legacy));
    let after = super::load();
    assert!(
        after.account_token.is_empty(),
        "a stale legacy credential cannot revive after clear"
    );
    assert!(
        after.sources.is_empty(),
        "a stale legacy source cannot revive after clear"
    );
    assert!(matches!(
        super::persistence::load(),
        super::persistence::CanonicalRead::Cleared { .. }
    ));
}

#[test]
fn secure_v1_fixtures_cover_tagged_identity_shapes_and_a_readable_envelope() {
    let _g = crate::testlock::serial();
    for tag in TAGS {
        let value: Value = serde_json::from_str(envelope_fixture(tag)).unwrap();
        assert_eq!(
            value["format"], "plxnative-secure-session",
            "{tag}: secure format"
        );
        assert_eq!(value["version"], 1, "{tag}: secure version");
        assert_eq!(
            value["sealed"]["backend"], "keymanager3",
            "{tag}: secure backend"
        );
        for field in ["key", "iv", "data"] {
            assert!(
                value["sealed"][field]
                    .as_str()
                    .is_some_and(|value| !value.is_empty()),
                "{tag}: sealed {field}"
            );
        }
        let identity = value["sealed"].get("identity").and_then(Value::as_str);
        match *tag {
            "v0.6.0" | "v0.6.1" | "v0.6.2" => {
                assert_eq!(identity, None, "{tag}: pre-identity envelope")
            }
            "v0.6.3" => assert_eq!(identity, Some("anonymous")),
            "v0.6.4" => assert_eq!(identity, Some("app_id")),
            "v0.6.5" => assert_eq!(identity, Some("named")),
            _ => unreachable!(),
        }
    }

    let fixture = FixtureRoot::new("secure-readable");
    let legacy = fixture.legacy_session();
    std::fs::write(&legacy, envelope_fixture("v0.6.3")).unwrap();
    super::redirect_for_test(Some(legacy));
    crate::keymanager::arm_for_test(vec![
        (
            "begin",
            Ok(serde_json::json!({"returnValue": true, "handle": "h-dec"})),
        ),
        (
            "finish",
            Ok(
                serde_json::json!({"returnValue": true, "output": b64_encode(session_fixture("v0.6.3").as_bytes())}),
            ),
        ),
    ]);
    let opened = super::load();
    crate::keymanager::disarm_for_test();
    let expected: super::Session = serde_json::from_str(session_fixture("v0.6.3")).unwrap();
    assert_session_fields("v0.6.3 secure", &opened, &expected);
}

#[test]
fn unavailable_secure_fixture_requires_fresh_reauthentication_and_then_reopens() {
    let _g = crate::testlock::serial();
    let fixture = FixtureRoot::new("secure-unavailable-fresh-reauth");
    let legacy = fixture.legacy_session();
    std::fs::write(&legacy, envelope_fixture("v0.6.3")).unwrap();
    super::redirect_for_test(Some(legacy.clone()));
    crate::keymanager::arm_for_test(vec![("begin", Err(()))]);
    let unavailable = super::load();
    assert!(
        unavailable.account_token.is_empty(),
        "unavailable key manager must not fabricate a session"
    );
    assert!(
        super::secure_unavailable(),
        "the actual keymanager mock reports its unavailable state"
    );

    let fresh: super::Session = serde_json::from_str(session_fixture("v0.6.3")).unwrap();
    assert!(
        super::save_after_reauthentication(&fresh).persisted(),
        "fresh reauthentication replaces an unavailable envelope"
    );
    crate::keymanager::disarm_for_test();

    super::redirect_for_test(Some(legacy));
    let reopened = super::load();
    assert_session_fields("fresh reauthentication", &reopened, &fresh);
}

fn b64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - index * 6)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
