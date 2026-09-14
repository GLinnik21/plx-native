//! Canonical schema and the shipping client/coordinator path, using synthetic helper transport.
use super::*;
use crate::storage::{client, state, wire::*};
use persistence::CanonicalRead;

fn fixture() -> Session {
    serde_json::from_value(serde_json::json!({
        "client_id":"synthetic-client", "account_token":"synthetic-account",
        "user":{"uuid":"owner","token":"synthetic-profile"},
        "server":{"token":"synthetic-server"},
        "profiles":[{"uuid":"owner","user":{"uuid":"owner","token":"cached"},
                     "pin":{"salt":"01010101010101010101010101010101","hash":"0202020202020202020202020202020202020202020202020202020202020202","iters":20000}}],
        "auto_sign_in":true,"last_library":[{"user":"owner","libs":[{"kind":"movie","machine_id":"machine","key":1}]}],
        "last_hero_blur":[[0.1,0.2,0.3],[0.1,0.2,0.3],[0.1,0.2,0.3],[0.1,0.2,0.3]],
        "future_secret":{"token":"synthetic-future"}
    })).unwrap()
}

fn loaded(public: &state::PublicPayload, auth: AuthLoad) -> Response {
    let mut state = state::CanonicalState::new(state::Flavor::Stable, state::Generation([1; 16]));
    state.public = public.clone();
    state.migrations.session.progress = state::MigrationProgress::Complete;
    Response::Loaded {
        db_rev: "1".into(),
        state: serde_json::to_value(state).unwrap(),
        auth,
        protection: None,
    }
}

fn v1(session: &Session) -> (state::PublicPayload, String) {
    let (public, protected) = split_canonical(session).unwrap();
    let mut protected: Value = serde_json::from_str(&protected).unwrap();
    protected["version"] = 1.into();
    protected.as_object_mut().unwrap().remove("profiles");
    (public, protected.to_string())
}

#[test]
fn v1_routine_public_edit_does_not_upgrade_or_reencrypt_auth() {
    let (mut public, protected) = v1(&Session {
        profiles: Vec::new(),
        ..fixture()
    });
    public.preferences["future_public"] = "keep".into();
    let mut session = join_canonical(&public, &protected).unwrap();
    session.auto_sign_in = false;
    let mut requests = Vec::new();
    let response = loaded(
        &public,
        AuthLoad::Plaintext {
            payload: SecretString(protected),
        },
    );
    let mut transport = |request: Request| {
        let reply = if matches!(request, Request::Load {}) {
            response.clone()
        } else {
            Response::Error {
                code: ErrorCode::Unavailable,
            }
        };
        requests.push(request);
        Ok(reply)
    };
    persistence::commit_session_with(&session, false, SaveAuthority::Routine, 11, &mut transport);
    assert!(
        matches!(&requests[1], Request::Commit { mutation: WireMutation::UpdatePreferences {payload}, .. }
        if payload["preferences"]["future_public"] == "keep" && payload["preferences"]["auto_sign_in"] == false),
        "reading v1 and changing only public preferences must retain exact protected bytes"
    );
}

#[test]
fn public_edit_with_reserved_v1_extension_does_not_open_or_rewrite_auth() {
    let (public, protected) = v1(&Session {
        profiles: Vec::new(),
        ..fixture()
    });
    let mut value: Value = serde_json::from_str(&protected).unwrap();
    value["extensions"]["profiles"] = serde_json::json!([{"token":"opaque-never-active"}]);
    let session = join_canonical(&public, &value.to_string()).unwrap();
    assert!(session.profiles.is_empty());
    assert!(split_canonical(&session).is_err());
    let response = loaded(
        &public,
        AuthLoad::Locked {
            code: ErrorCode::Unavailable,
        },
    );
    let mut count = 0;
    let mut transport = |request: Request| {
        count += 1;
        Ok(match request {
            Request::Load {} => response.clone(),
            Request::Commit {
                mutation: WireMutation::UpdatePreferences { .. },
                ..
            } => Response::Error {
                code: ErrorCode::Unavailable,
            },
            _ => panic!("public-only write crossed protected boundary"),
        })
    };
    persistence::commit_session_with(
        &session,
        false,
        SaveAuthority::PublicOnly,
        11,
        &mut transport,
    );
    assert_eq!(
        count, 2,
        "public-only write must be admitted even with opaque v1 profiles extension"
    );
}

#[test]
fn v2_roundtrip_keeps_target_fields_and_secrets_out_of_public_payload() {
    let session = fixture();
    let (public, auth) = split_canonical(&session).unwrap();
    let public_text = serde_json::to_string(&public).unwrap();
    for secret in [
        "synthetic-account",
        "synthetic-profile",
        "synthetic-server",
        "cached",
        "synthetic-future",
        "salt",
        "hash",
        "profiles",
    ] {
        assert!(!public_text.contains(secret));
    }
    let reopened = join_canonical(&public, &auth).unwrap();
    assert_eq!(
        serde_json::to_value(&session).unwrap(),
        serde_json::to_value(reopened).unwrap()
    );
    assert_eq!(serde_json::from_str::<Value>(&auth).unwrap()["version"], 2);
}

#[test]
fn invalid_v2_profiles_shape_is_schema_error_but_bad_entries_do_not_lose_account() {
    let (public, protected) = split_canonical(&fixture()).unwrap();
    for bad in [
        Value::Null,
        serde_json::json!({}),
        serde_json::json!("profiles"),
    ] {
        let mut auth: Value = serde_json::from_str(&protected).unwrap();
        auth["profiles"] = bad;
        assert!(join_canonical(&public, &auth.to_string()).is_err());
    }
    let mut auth: Value = serde_json::from_str(&protected).unwrap();
    auth.as_object_mut().unwrap().remove("profiles");
    assert!(join_canonical(&public, &auth.to_string()).is_err());
    for bad in [
        serde_json::json!({"uuid":""}),
        serde_json::json!({"uuid":"owner","user":{"uuid":"other"}}),
        serde_json::json!({"uuid":"owner","user":{"uuid":"owner"},"pin":{"salt":"bad","hash":"bad","iters":1}}),
    ] {
        auth["profiles"] = serde_json::json!([bad]);
        let session = join_canonical(&public, &auth.to_string()).unwrap();
        assert_eq!(session.account_token, "synthetic-account");
        assert!(session.profiles.is_empty());
    }
}

#[test]
fn duplicate_uuid_invalidates_all_entries_even_when_one_has_invalid_pin_shape() {
    let (public, protected) = split_canonical(&fixture()).unwrap();
    let mut auth: Value = serde_json::from_str(&protected).unwrap();
    let mut duplicate = auth["profiles"][0].clone();
    duplicate["pin"] = serde_json::json!("malformed");
    auth["profiles"].as_array_mut().unwrap().push(duplicate);
    assert!(join_canonical(&public, &auth.to_string())
        .unwrap()
        .profiles
        .is_empty());
}

#[test]
fn helper_failure_never_becomes_missing_and_legacy_permission() {
    for error in [
        client::ClientError::Unavailable,
        client::ClientError::Authentication,
        client::ClientError::Protocol,
        client::ClientError::Corrupt,
    ] {
        let mut transport = |_: Request| Err(error);
        assert!(matches!(
            persistence::load_helper_with(&mut transport),
            CanonicalRead::Blocked(_)
        ));
    }
}

#[test]
fn lost_commit_reply_reconciles_same_operation_and_does_not_claim_fresh_verification() {
    let operation = state::Generation([7; 16]);
    let mut observed = None;
    let mut calls = 0;
    let mut transport = |request: Request| {
        calls += 1;
        match request {
            Request::Commit {
                operation_id,
                digest,
                ..
            } => {
                observed = Some((operation_id, digest));
                Err(client::ClientError::Protocol)
            }
            Request::Reconcile {
                operation_id,
                digest,
            } => {
                assert_eq!(observed.as_ref(), Some(&(operation_id, digest)));
                Ok(Response::Reconcile {
                    status: ReconcileStatus::Applied,
                    db_rev: Some("2".into()),
                    applied: Some(state::Applied {
                        revision: 2,
                        epoch: 1,
                        auth_generation: state::Generation([2; 16]),
                    }),
                    protection: None,
                })
            }
            _ => panic!("unexpected request"),
        }
    };
    assert!(matches!(
        client::commit_with(
            &mut transport,
            None,
            operation,
            WireMutation::ClearTenure {}
        ),
        Ok(Response::Reconcile {
            status: ReconcileStatus::Applied,
            ..
        })
    ));
    assert_eq!(calls, 2);
}

#[test]
fn malformed_hero_hint_is_discarded_without_losing_credentials() {
    let mut value = serde_json::to_value(fixture()).unwrap();
    value["last_hero_blur"] = serde_json::json!({"future":"shape"});
    let session: Session = serde_json::from_value(value).unwrap();
    assert!(session.last_hero_blur.is_none());
    assert_eq!(session.account_token, "synthetic-account");
    let (_, auth) = split_canonical(&session).unwrap();
    let mut public = state::PublicPayload::default();
    public.preferences =
        serde_json::json!({"auto_sign_in":"bad","last_library":null,"last_hero_blur":"bad"});
    let session = join_canonical(&public, &auth).unwrap();
    assert!(!session.auto_sign_in);
    assert!(session.last_library.is_empty());
    assert!(session.last_hero_blur.is_none());
}

struct Opener {
    identities: Vec<persistence::LegacyIdentity>,
    plaintext: Option<Vec<u8>>,
    failure: persistence::LegacyOpenError,
}
impl persistence::LegacyOpener for Opener {
    fn open(
        &mut self,
        identity: persistence::LegacyIdentity,
        _: &Value,
    ) -> Result<Vec<u8>, persistence::LegacyOpenError> {
        self.identities.push(identity);
        self.plaintext.clone().ok_or(self.failure)
    }
}
fn opener() -> Opener {
    Opener {
        identities: Vec::new(),
        plaintext: None,
        failure: persistence::LegacyOpenError::IdentityRefused,
    }
}

struct MigrationFixture {
    pending: Option<String>,
    data: Option<String>,
    cleared: bool,
    blocked: Option<crate::storage::StoreError>,
    verified: bool,
    wrong_readback: bool,
    commits: usize,
}
impl Default for MigrationFixture {
    fn default() -> Self {
        Self {
            pending: None,
            data: None,
            cleared: false,
            blocked: None,
            verified: true,
            wrong_readback: false,
            commits: 0,
        }
    }
}
impl persistence::MigrationStore for MigrationFixture {
    fn load(&mut self) -> CanonicalRead {
        if let Some(envelope) = &self.pending {
            return CanonicalRead::Pending {
                revision: 1,
                envelope: envelope.clone(),
            };
        }
        if let Some(error) = self.blocked {
            return CanonicalRead::Blocked(error);
        }
        if self.cleared {
            return CanonicalRead::Cleared { revision: 1 };
        }
        match &self.data {
            Some(payload) => CanonicalRead::Data {
                revision: 1,
                payload: payload.clone(),
            },
            None => CanonicalRead::Missing,
        }
    }
    fn import(&mut self, session: &Session) -> persistence::CanonicalCommit {
        self.commits += 1;
        self.pending = None;
        self.data = Some(if self.wrong_readback {
            serde_json::to_string(&Session::default()).unwrap()
        } else {
            serde_json::to_string(session).unwrap()
        });
        persistence::CanonicalCommit::Durable {
            revision: 1,
            verified: self.verified,
            protection: None,
        }
    }
    fn clear(&mut self) -> persistence::CanonicalCommit {
        self.commits += 1;
        self.cleared = true;
        persistence::CanonicalCommit::Durable {
            revision: 1,
            verified: self.verified,
            protection: None,
        }
    }
}

struct LegacyFile {
    dir: std::path::PathBuf,
    path: std::path::PathBuf,
}
impl LegacyFile {
    fn new(name: &str, value: &Value) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "plx-storage-migration-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("auth.json");
        std::fs::write(&path, value.to_string()).unwrap();
        Self { dir, path }
    }
}
impl Drop for LegacyFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
struct TempCanonicalState {
    dir: std::path::PathBuf,
}

#[cfg(test)]
impl TempCanonicalState {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "plx-live-canonical-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::paths::redirect_persistent_state_root_for_test(Some(dir.clone()));
        Self { dir }
    }
}

#[cfg(test)]
impl Drop for TempCanonicalState {
    fn drop(&mut self) {
        crate::paths::redirect_persistent_state_root_for_test(None);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn live_load_and_update_use_the_canonical_authority() {
    let _serial = crate::testlock::serial();
    let _state = TempCanonicalState::new("live-authority");
    let session = fixture();
    assert!(matches!(
        persistence::write_session(&session, SaveAuthority::Routine),
        persistence::CanonicalCommit::Durable { revision: 1, .. }
    ));
    assert_eq!(
        super::peek().account_token,
        "synthetic-account",
        "the live read must consume the canonical store instead of legacy auth.json"
    );
    assert!(super::update(|current| {
        assert_eq!(current.account_token, "synthetic-account");
        let mut next = current.clone();
        next.auto_sign_in = true;
        Some(next)
    }));
    let CanonicalRead::Data { payload, revision } = persistence::load() else {
        panic!("live update must commit through the canonical store");
    };
    assert_eq!(revision, 2);
    assert_eq!(
        serde_json::from_str::<Session>(&payload).unwrap().account_token,
        "synthetic-account"
    );
}

#[test]
fn canonical_cleared_blocked_and_existing_data_all_outrank_reappeared_legacy() {
    let source = LegacyFile::new(
        "canonical-priority",
        &serde_json::to_value(fixture()).unwrap(),
    );
    for mut store in [
        MigrationFixture {
            cleared: true,
            ..Default::default()
        },
        MigrationFixture {
            blocked: Some(crate::storage::StoreError::HelperUnavailable),
            ..Default::default()
        },
        MigrationFixture {
            blocked: Some(crate::storage::StoreError::AuthLocked),
            ..Default::default()
        },
        MigrationFixture {
            blocked: Some(crate::storage::StoreError::InvalidSchema),
            ..Default::default()
        },
        MigrationFixture {
            data: Some(
                serde_json::to_string(&Session {
                    account_token: "canonical".into(),
                    ..Default::default()
                })
                .unwrap(),
            ),
            ..Default::default()
        },
    ] {
        persistence::bootstrap_with(
            &mut store,
            &mut opener(),
            std::slice::from_ref(&source.path),
        );
        assert_eq!(store.commits, 0);
        assert!(source.path.exists());
    }
}

#[test]
fn versioned_wrapper_is_decoded_before_bare_session_and_clear_is_a_tombstone() {
    let session = fixture();
    for (name, state) in [
        (
            "wrapper-data",
            serde_json::json!({"Data":{"payload":serde_json::to_string(&session).unwrap()}}),
        ),
        ("wrapper-cleared", serde_json::json!("Cleared")),
    ] {
        let wrapper = serde_json::json!({"format":"plxnative-record","version":1,"domain":"session","key":"session","revision":7,"state":state});
        let source = LegacyFile::new(name, &wrapper);
        let mut store = MigrationFixture::default();
        let boot = persistence::bootstrap_with(
            &mut store,
            &mut opener(),
            std::slice::from_ref(&source.path),
        );
        assert_eq!(store.commits, 1);
        assert!(!source.path.exists());
        assert!(!boot.cleanup_failed);
        if name.ends_with("cleared") {
            assert!(matches!(boot.state, CanonicalRead::Cleared { .. }));
        } else {
            assert!(matches!(boot.state, CanonicalRead::Data { .. }));
            assert_eq!(
                serde_json::from_str::<Session>(store.data.as_ref().unwrap())
                    .unwrap()
                    .profiles
                    .len(),
                1
            );
        }
    }
}

#[test]
fn old_receipt_or_mismatched_readback_never_authorizes_source_cleanup() {
    for (name, verified, wrong_readback) in [
        ("ledger-only", false, false),
        ("wrong-readback", true, true),
    ] {
        let source = LegacyFile::new(name, &serde_json::to_value(fixture()).unwrap());
        let mut store = MigrationFixture {
            verified,
            wrong_readback,
            ..Default::default()
        };
        let boot = persistence::bootstrap_with(
            &mut store,
            &mut opener(),
            std::slice::from_ref(&source.path),
        );
        assert_eq!(store.commits, 1);
        assert!(source.path.exists());
        assert!(matches!(boot.state, CanonicalRead::Blocked(_)));
    }
}

#[test]
fn recorded_identity_refusal_and_unavailability_preserve_secure_input_without_retry() {
    for (index, (identity, expected)) in [
        (None, persistence::LegacyIdentity::Anonymous),
        (Some("app_id"), persistence::LegacyIdentity::AppId),
        (Some("named"), persistence::LegacyIdentity::Named),
    ]
    .into_iter()
    .enumerate()
    {
        let mut secure = serde_json::json!({"format":"plxnative-secure-session","version":1,"sealed":{"backend":"keymanager3","key":"plxnative.session.v1","iv":"synthetic","data":"synthetic"}});
        if let Some(identity) = identity {
            secure["sealed"]["identity"] = identity.into();
        }
        let source = LegacyFile::new(&format!("identity-{index}"), &secure);
        for failure in [
            persistence::LegacyOpenError::IdentityRefused,
            persistence::LegacyOpenError::Unavailable,
        ] {
            let mut opener = Opener {
                failure,
                ..opener()
            };
            let mut store = MigrationFixture::default();
            let boot = persistence::bootstrap_with(
                &mut store,
                &mut opener,
                std::slice::from_ref(&source.path),
            );
            assert!(matches!(boot.state, CanonicalRead::Blocked(_)));
            assert_eq!(opener.identities, vec![expected]);
            assert_eq!(store.commits, 0);
            assert!(source.path.exists());
        }
    }
}

#[test]
fn secure_open_then_durable_plaintext_readback_is_required_before_retirement() {
    let source = LegacyFile::new(
        "secure-open",
        &serde_json::json!({"format":"plxnative-secure-session","version":1,"sealed":{"backend":"keymanager3","identity":"named"}}),
    );
    let mut opener = Opener {
        plaintext: Some(serde_json::to_vec(&fixture()).unwrap()),
        ..opener()
    };
    let mut store = MigrationFixture::default();
    let boot =
        persistence::bootstrap_with(&mut store, &mut opener, std::slice::from_ref(&source.path));
    assert!(matches!(boot.state, CanonicalRead::Data { .. }));
    assert!(!source.path.exists());
    assert_eq!(opener.identities, vec![persistence::LegacyIdentity::Named]);
}

#[test]
fn unknown_secure_format_remains_opaque_until_explicit_clear() {
    for (index,value) in [serde_json::json!({"format":"future-secure","version":3}),serde_json::json!({"format":"plxnative-secure-session","version":2}),serde_json::json!({"format":"plxnative-secure-session","version":1,"sealed":{"backend":"future"}})].into_iter().enumerate() {
        let source=LegacyFile::new(&format!("unknown-{index}"),&value);let mut store=MigrationFixture::default();let mut opener=opener();
        let boot=persistence::bootstrap_with(&mut store,&mut opener,std::slice::from_ref(&source.path));
        assert!(matches!(boot.state,CanonicalRead::Blocked(_)));assert_eq!(store.commits,0);assert!(source.path.exists());assert!(opener.identities.is_empty());
    }
}

#[test]
fn unknown_fields_survive_all_nested_target_and_legacy_records() {
    let mut value = serde_json::to_value(fixture()).unwrap();
    value["user"]["future"] = "u".into();
    value["server"]["future"] = "s".into();
    value["home_users"] = serde_json::json!([{"uuid":"owner","future":"h"}]);
    value["sources"] = serde_json::json!([{"future":"source"}]);
    value["profiles"][0]["future"] = "cache".into();
    value["profiles"][0]["pin"]["future"] = "verifier".into();
    value["last_library"][0]["future"] = "library".into();
    value["last_library"][0]["libs"][0]["future"] = "typed".into();
    value["home_pins"] = serde_json::json!([{"user":"owner","known":[{"machine_id":"m","key":1,"future":"pin"}],"future":"home"}]);
    value["recent_searches"] =
        serde_json::json!([{"user":"owner","terms":["term"],"future":"recent"}]);
    let session: Session = serde_json::from_value(value).unwrap();
    let (public, auth) = split_canonical(&session).unwrap();
    assert_eq!(
        serde_json::to_value(&session).unwrap(),
        serde_json::to_value(join_canonical(&public, &auth).unwrap()).unwrap()
    );
}

#[test]
fn locked_boot_exposes_only_public_preferences_and_never_offline_profiles() {
    let (public, _) = split_canonical(&fixture()).unwrap();
    let response = loaded(
        &public,
        AuthLoad::Locked {
            code: ErrorCode::Unavailable,
        },
    );
    let mut transport = |_: Request| Ok(response.clone());
    let CanonicalRead::Locked {
        public: session, ..
    } = persistence::load_helper_with(&mut transport)
    else {
        panic!("locked snapshot")
    };
    assert!(session.auto_sign_in);
    assert_eq!(session.last_library.len(), 1);
    assert!(session.account_token.is_empty());
    assert!(session.user.token.is_empty());
    assert!(session.profiles.is_empty());
}

#[derive(Default)]
struct Db8 {
    record: Option<Value>,
    puts: usize,
    gets: usize,
}
impl crate::storage::keymanager::Rpc for Db8 {
    fn call(&mut self, uri: &str, payload: &Value) -> Result<Value, ErrorCode> {
        match uri {
            "luna://com.palm.db/get" => {
                self.gets += 1;
                Ok(
                    serde_json::json!({"returnValue":true,"results":self.record.iter().cloned().collect::<Vec<_>>()}),
                )
            }
            "luna://com.palm.db/put" => {
                let mut next = payload["objects"][0].clone();
                if let Some(current) = &self.record {
                    assert_eq!(
                        next["_rev"], current["_rev"],
                        "real coordinator must CAS the loaded DB8 revision"
                    );
                }
                self.puts += 1;
                next["_rev"] = serde_json::json!(self.puts);
                self.record = Some(next);
                Ok(
                    serde_json::json!({"returnValue":true,"results":[{"id":"plxstate.stable","rev":self.puts}]}),
                )
            }
            _ => panic!("ACL-tier scenario must not bypass DB8 or invoke a Keymanager RPC"),
        }
    }
}

#[test]
fn production_client_coordinator_and_helper_backend_reconcile_committed_lost_reply() {
    let mut backend = crate::storage::backend::Backend::new(
        Db8::default(),
        state::Flavor::Stable,
        "com.beb.plxnative.storage".into(),
    );
    let session = fixture();
    let mut lose_reply = true;
    let mut transport = |request: Request| {
        let commit = matches!(&request, Request::Commit { .. });
        let response = backend.dispatch(request);
        if commit && lose_reply {
            lose_reply = false;
            Err(client::ClientError::Protocol)
        } else {
            Ok(response)
        }
    };
    let result = persistence::commit_session_with(
        &session,
        false,
        SaveAuthority::FreshReauthentication,
        4,
        &mut transport,
    );
    assert!(
        matches!(
            result,
            persistence::CanonicalCommit::Durable {
                verified: false,
                ..
            }
        ),
        "lost reply must reconcile using the helper ledger"
    );
    let CanonicalRead::Opened {
        session: reopened, ..
    } = persistence::load_helper_with(&mut transport)
    else {
        panic!("reopened helper state")
    };
    assert_eq!(
        serde_json::to_value(reopened).unwrap(),
        serde_json::to_value(&session).unwrap()
    );
    let client::Load::Present(before) = client::load_with(&mut transport).unwrap() else {
        panic!("helper snapshot")
    };
    let mut next = session.clone();
    next.auto_sign_in = false;
    assert!(matches!(
        persistence::commit_session_with(
            &next,
            false,
            SaveAuthority::PublicOnly,
            4,
            &mut transport
        ),
        persistence::CanonicalCommit::Durable { verified: true, .. }
    ));
    let client::Load::Present(after) = client::load_with(&mut transport).unwrap() else {
        panic!("helper snapshot")
    };
    assert_eq!(before.state.auth_envelope, after.state.auth_envelope);
    assert_eq!(before.state.auth_generation, after.state.auth_generation);
    let record = backend.rpc.record.as_ref().unwrap();
    assert_eq!(
        record.as_object().unwrap().len(),
        4,
        "DB8 must receive only _id/_kind/_rev/state"
    );
    assert!(record["state"].is_string());
    assert!(backend.rpc.gets >= 4);
    assert_eq!(backend.rpc.puts, 2);
}

#[test]
fn canonical_pending_secure_import_is_not_missing_or_permission_to_import_another_file() {
    let mut pending = state::CanonicalState::new(state::Flavor::Stable, state::Generation([1; 16]));
    pending.migrations.session.pending_import = Some("opaque-unknown-secure-format".into());
    let mut transport = |_: Request| {
        Ok(Response::Loaded {
            db_rev: "1".into(),
            state: serde_json::to_value(&pending).unwrap(),
            auth: AuthLoad::None,
            protection: None,
        })
    };
    assert!(matches!(
        persistence::load_helper_with(&mut transport),
        CanonicalRead::Pending { .. }
    ));
}

#[test]
fn pending_import_retries_only_its_recorded_envelope_and_never_another_legacy_file() {
    let source = LegacyFile::new(
        "pending-no-fallback",
        &serde_json::json!({"client_id":"other","account_token":"never-import"}),
    );
    let envelope=serde_json::json!({"format":"plxnative-secure-session","version":1,"sealed":{"backend":"keymanager3","identity":"named"}}).to_string();
    let mut store = MigrationFixture {
        pending: Some(envelope.clone()),
        ..Default::default()
    };
    let mut unavailable = opener();
    let boot = persistence::bootstrap_with(
        &mut store,
        &mut unavailable,
        std::slice::from_ref(&source.path),
    );
    assert!(matches!(
        boot.state,
        CanonicalRead::Blocked(crate::storage::StoreError::AuthLocked)
    ));
    assert_eq!(store.pending.as_ref(), Some(&envelope));
    assert_eq!(store.commits, 0);
    assert!(source.path.exists());
    assert_eq!(
        unavailable.identities,
        vec![persistence::LegacyIdentity::Named]
    );
    let mut recovered = Opener {
        plaintext: Some(serde_json::to_vec(&fixture()).unwrap()),
        ..opener()
    };
    let boot = persistence::bootstrap_with(
        &mut store,
        &mut recovered,
        std::slice::from_ref(&source.path),
    );
    assert!(matches!(boot.state, CanonicalRead::Data { .. }));
    assert!(store.pending.is_none());
    assert_eq!(store.commits, 1);
    assert!(
        source.path.exists(),
        "unrelated input is not a verified migration source"
    );
}

fn check_v1_extension_bootstrap_roundtrip(keys: &[String]) {
    let expected = Session {
        profiles: Vec::new(),
        ..fixture()
    };
    let (public, protected) = v1(&expected);
    let mut auth: Value = serde_json::from_str(&protected).unwrap();
    for key in keys {
        auth["extensions"][key] = if key == "profiles" {
            serde_json::json!([{"uuid":"opaque-owner","user":{"uuid":"opaque-owner","token":"never-active"}}])
        } else {
            serde_json::json!({"opaque":key,"value":"synthetic-future-secret"})
        };
    }
    let extensions = auth["extensions"].clone();
    let protected = serde_json::to_string_pretty(&auth).unwrap();
    let mut backend = crate::storage::backend::Backend::new(
        Db8::default(),
        state::Flavor::Stable,
        "com.beb.plxnative.storage".into(),
    );
    let mut transport = |request| Ok(backend.dispatch(request));
    assert!(matches!(
        client::commit_with(
            &mut transport,
            None,
            state::Generation([9; 16]),
            WireMutation::ReplaceAuth {
                public: serde_json::to_value(&public).unwrap(),
                payload: SecretString(protected.clone()),
                protection: ProtectionRequest::Db8AclOnlyExplicit,
            }
        ),
        Ok(Response::Commit {
            status: CommitStatus::Committed,
            verified: true,
            ..
        })
    ));
    let client::Load::Present(before) = client::load_with(&mut transport).unwrap() else {
        panic!("seeded v1")
    };
    let mut session = match persistence::load_helper_with(&mut transport) {
        CanonicalRead::Opened {session,..} => session,
        CanonicalRead::Data {payload,..} => serde_json::from_str::<Session>(&payload)
            .expect("helper bootstrap must reopen without flattening opaque fields into known Session fields"),
        _ => panic!("opened Session bootstrap"),
    };
    assert!(session.profiles.is_empty());
    assert_eq!(
        serde_json::to_value(&session.extensions).unwrap(),
        extensions
    );
    assert_eq!(
        serde_json::to_value(super::split_public(&session).unwrap()).unwrap(),
        serde_json::to_value(&public).unwrap()
    );
    let bootstrap = persistence::bootstrap_with(
        &mut persistence::HelperMigration {
            transport: &mut transport,
            major: 4,
        },
        &mut opener(),
        &[],
    );
    assert!(bootstrap.migration.is_none());
    assert!(!bootstrap.cleanup_failed);
    let CanonicalRead::Opened { session: ready, .. } = bootstrap.state else {
        panic!("typed ready bootstrap")
    };
    assert!(ready.profiles.is_empty());
    assert_eq!(serde_json::to_value(&ready.extensions).unwrap(), extensions);
    session = ready;
    session.auto_sign_in = !session.auto_sign_in;
    assert!(matches!(
        persistence::commit_session_with(
            &session,
            false,
            SaveAuthority::Routine,
            4,
            &mut transport
        ),
        persistence::CanonicalCommit::Durable { verified: true, .. }
    ));
    let client::Load::Present(after) = client::load_with(&mut transport).unwrap() else {
        panic!("public edit")
    };
    assert_eq!(before.state.auth_envelope, after.state.auth_envelope);
    assert_eq!(before.state.auth_generation, after.state.auth_generation);
    assert!(matches!(&after.auth, AuthLoad::Plaintext {payload} if payload.0 == protected));
    assert_eq!(
        backend.rpc.puts, 2,
        "bootstrap must not write or upgrade v1"
    );
}

#[test]
fn v1_opaque_profiles_survive_helper_bootstrap_without_becoming_active() {
    check_v1_extension_bootstrap_roundtrip(&["profiles".into()]);
}

#[test]
fn v1_opaque_extensions_cannot_collide_with_any_recognized_session_field() {
    let known = serde_json::to_value(Session::default()).unwrap();
    for key in known.as_object().unwrap().keys() {
        check_v1_extension_bootstrap_roundtrip(std::slice::from_ref(key));
    }
}

#[test]
fn helper_migration_keeps_opened_session_typed_through_exact_readback() {
    let expected = fixture();
    let source = LegacyFile::new(
        "typed-helper-readback",
        &serde_json::to_value(&expected).unwrap(),
    );
    let mut backend = crate::storage::backend::Backend::new(
        Db8::default(),
        state::Flavor::Stable,
        "com.beb.plxnative.storage".into(),
    );
    let mut transport = |request| Ok(backend.dispatch(request));
    let result = persistence::bootstrap_with(
        &mut persistence::HelperMigration {
            transport: &mut transport,
            major: 4,
        },
        &mut opener(),
        std::slice::from_ref(&source.path),
    );
    assert!(matches!(
        result.migration,
        Some(persistence::CanonicalCommit::Durable { verified: true, .. })
    ));
    let CanonicalRead::Opened { session, .. } = result.state else {
        panic!("typed migrated Session")
    };
    assert!(super::protected_fields_equal(&session, &expected));
    assert_eq!(
        serde_json::to_value(super::split_public(&session).unwrap()).unwrap(),
        serde_json::to_value(super::split_public(&expected).unwrap()).unwrap()
    );
    assert!(!result.cleanup_failed);
    assert!(
        !source.path.exists(),
        "exact typed readback must authorize retirement of this source"
    );
    assert_eq!(backend.rpc.puts, 1);
}
