//! The DOCUMENT per-show track memory lives in: the DB8 state the platform caps at 256 KiB
//! (`MAX_LOGICAL_BYTES`), and the guard in `persistence::commit_session_with` that keeps a preference
//! write from being refused because per-profile memory grew. The data layer itself (the entry, its
//! bounds, the stamp, the household budget) is `track_memory_tests`.
//!
//! * the guard against platform's OWN code: the projection it sizes is exactly what
//!   `CanonicalState::apply` produces, and what it sheds is what makes `apply` pass;
//! * the order it sheds in, and only as far as the document needs;
//! * the worst-case household, written through the production coordinator;
//! * a preference write over the cap (delta C-d1: this used to be refused, now it lands);
//! * sign-out, and the previous build reading the real shape.
use super::super::{
    persistence, split_canonical, v0, HomePins, HomeUserRef, LastLibrary, LibraryView, LibraryViews, PinVerifier,
    PinnedLib, PlaintextChoice, PlaybackQuality, ProfileCreds, RecentSearches, SaveAuthority, ServerRef, Session,
    SkipInterval, SourceRef, SubtitleOffsets, TypedLib, UserRef,
};
use super::track_memory::*;
use super::track_memory_tests::{put, show, typical, worst, STAMP};
use super::ProfileList;
use plx_platform::storage::state::{self, logical_size, MAX_LOGICAL_BYTES};
use plx_platform::storage::wire::{
    AuthLoad, ErrorCode, MigrationMutation, ProtectionRequest, Request, SecretString, WireMutation,
};
use plx_platform::storage::client;
use serde_json::{json, Value};

// ---- the household, at its bounds --------------------------------------------------------------

/// The largest Plex Home: sixteen managed users.
const USERS: usize = 16;
/// Servers each user's credentials list (the sources each switch resolved): one per server, so
/// sixteen at the largest roster. Nothing caps a profile's sources.
const SOURCES: usize = USERS;

fn uuid(n: usize) -> String {
    format!("{n:08x}-89ab-cdef-0123-456789abcdef")
}
fn machine(n: usize) -> String {
    format!("{n:040x}")
}
fn pin() -> String {
    plx_base::spki::pin_from_spki_der(b"synthetic spki")
}
fn address() -> String {
    format!("192-168-100-200.{}.plex.direct", "a".repeat(32))
}
fn server(n: usize) -> ServerRef {
    ServerRef {
        name: "s".repeat(48),
        machine_id: machine(n),
        address: address(),
        port: 32400,
        token: "k".repeat(20),
        origin_url: format!("https://{}:32400", address()),
        ..Default::default()
    }
}
fn source(n: usize) -> SourceRef {
    SourceRef {
        machine_id: machine(n),
        name: "n".repeat(48),
        shared_by: "f".repeat(32),
        owned: true,
        home: true,
        owner_id: 123_456_789,
        address: address(),
        port: 32400,
        token: "k".repeat(20),
        origin_url: format!("https://{}:32400", address()),
        ..Default::default()
    }
}
fn thumb() -> String {
    format!("https://plex.tv/users/{}/avatar?c=1700000000", "a".repeat(40))
}
fn user_ref(n: usize) -> UserRef {
    UserRef {
        id: n as i64 + 1,
        uuid: uuid(n),
        title: "t".repeat(48),
        thumb: thumb(),
        token: "u".repeat(20),
        plex_tv_token: Some("p".repeat(20)),
        ..Default::default()
    }
}

/// A signed-in household of `users` Plex Home users: the credentials (the protected half), and no
/// per-profile memory yet.
fn household(users: usize, sources: usize) -> Session {
    let (mut home_users, mut profiles) = (Vec::new(), Vec::new());
    for u in 0..users {
        home_users.push(HomeUserRef {
            id: u as i64 + 1,
            uuid: uuid(u),
            title: "t".repeat(48),
            thumb: thumb(),
            protected: true,
            admin: u == 0,
            ..Default::default()
        });
        profiles.push(ProfileCreds {
            uuid: uuid(u),
            user: user_ref(u),
            server: server(0),
            sources: (0..sources).map(source).collect(),
            pin: Some(PinVerifier { salt: "ab".repeat(16), hash: "cd".repeat(32), iters: PinVerifier::ITERS, extensions: Default::default() }),
            ..Default::default()
        });
    }
    Session {
        client_id: uuid(99),
        account_token: "a".repeat(20),
        server: server(0),
        user: user_ref(0),
        home_users,
        sources: (0..sources).map(source).collect(),
        profiles,
        ..Default::default()
    }
}

fn view(k: usize) -> LibraryView {
    LibraryView {
        machine_id: machine(k),
        key: i64::MIN + k as i64,
        sort: "s".repeat(96),
        desc: false,
        unwatched: true,
        genre: "g".repeat(64),
        listing: "collections".into(),
        listing_sort: "l".repeat(96),
        listing_desc: true,
        extensions: Default::default(),
    }
}

/// How many entries of each per-profile list a profile holds.
#[derive(Clone, Copy)]
struct Fill {
    /// Per profile; the household budget, not this, decides how many stay.
    track: usize,
    offsets: usize,
    views: usize,
}
const FULL: Fill = Fill { track: CAP, offsets: SubtitleOffsets::CAP, views: LibraryViews::CAP };

/// Fill every list of `session` for its first `users` Home users, with every string at its bound.
fn fill_memory(session: &mut Session, users: usize, fill: Fill) {
    for u in 0..users {
        let user = uuid(u);
        for k in 0..fill.offsets {
            session.set_subtitle_offset_for(&user, &machine(k), &format!("{:010}", u * 100 + k), Some(i64::MIN));
        }
        for k in 0..fill.views {
            session.set_view_for(&user, view(k));
        }
        session.prefs.last_library.push(LastLibrary {
            user: user.clone(),
            libs: ["movie", "show", "music"]
                .map(|kind| TypedLib { kind: kind.into(), machine_id: machine(u), key: i64::MIN, extensions: Default::default() })
                .to_vec(),
            extensions: Default::default(),
        });
        let libs = |base: usize| -> Vec<PinnedLib> {
            (0..4).map(|k| PinnedLib { machine_id: machine(base + k), key: i64::MIN, extensions: Default::default() }).collect()
        };
        session.home_pins.push(HomePins { user: user.clone(), asked: true, on: libs(0), off: libs(8), extensions: Default::default() });
        session.recent_searches.push(RecentSearches {
            user: user.clone(),
            terms: (0..4).map(|k| format!("{k}{}", "t".repeat(63))).collect(),
            extensions: Default::default(),
        });
    }
    // round-robin, so the household budget is shared the way a household would share it
    for n in 0..fill.track {
        for u in 0..users {
            assert!(put(&mut session.prefs.track_memory, &uuid(u), worst((u * 1000 + n) as u32)));
        }
    }
}

/// Sixteen servers' worth of pins and consent: the account-bound lists that are not sheddable.
fn with_server_lists(mut session: Session, servers: usize) -> Session {
    for n in 0..servers {
        session = session.with_server_key_pin(&machine(n), &pin()).expect("a well-formed pin");
        session = session.with_plaintext_choice(STAMP, &machine(n), PlaintextChoice::Revoked);
    }
    session
}

fn count(preferences: &Value, key: &str) -> usize {
    preferences.get(key).and_then(Value::as_array).map_or(0, |a| {
        a.iter().map(|r| r.get("items").or(r.get("libs")).and_then(Value::as_array).map_or(1, Vec::len)).sum()
    })
}

// ---- the helper-backed store (a private copy of the A0 fixtures) ------------------------------

#[derive(Default)]
struct Db8 {
    record: Option<Value>,
}
impl plx_platform::storage::keymanager::Rpc for Db8 {
    fn call(&mut self, uri: &str, payload: &Value) -> Result<Value, ErrorCode> {
        match uri {
            "luna://com.palm.db/get" => Ok(json!({"returnValue":true,"results":self.record.iter().cloned().collect::<Vec<_>>()})),
            "luna://com.palm.db/put" => {
                let mut next = payload["objects"][0].clone();
                if let Some(current) = &self.record {
                    assert_eq!(next["_rev"], current["_rev"], "real coordinator must CAS the loaded revision");
                }
                next["_rev"] = json!(1 + self.record.as_ref().map_or(0, |r| r["_rev"].as_u64().unwrap_or(0)));
                let rev = next["_rev"].clone();
                self.record = Some(next);
                Ok(json!({"returnValue":true,"results":[{"id":"plxstate.stable","rev":rev}]}))
            }
            _ => panic!("the ACL-tier scenario must not invoke a Keymanager RPC"),
        }
    }
}
type Store = plx_platform::storage::backend::Backend<Db8>;

fn store() -> Store {
    plx_platform::storage::backend::Backend::new(Db8::default(), state::Flavor::Stable, "com.beb.plxnative.storage".into())
}
fn try_commit(store: &mut Store, session: &Session, authority: SaveAuthority) -> persistence::CanonicalCommit {
    let mut transport = |request: Request| Ok(store.dispatch(request));
    persistence::commit_session_with(session, false, authority, 4, &mut transport)
}
fn commit(store: &mut Store, session: &Session, authority: SaveAuthority) {
    let result = try_commit(store, session, authority);
    assert!(matches!(result, persistence::CanonicalCommit::Durable { .. }), "{authority:?}: {result:?}");
}
fn snapshot(store: &mut Store) -> client::Snapshot {
    let mut transport = |request: Request| Ok(store.dispatch(request));
    let client::Load::Present(snapshot) = client::load_with(&mut transport).expect("the helper answers") else {
        panic!("a stored record")
    };
    snapshot
}
fn stored_preferences(store: &mut Store) -> Value {
    snapshot(store).state.public.preferences
}
fn opened(store: &mut Store) -> Session {
    let mut transport = |request: Request| Ok(store.dispatch(request));
    let persistence::CanonicalRead::Opened { session, .. } = persistence::load_helper_with(&mut transport) else {
        panic!("the session opens")
    };
    session
}
fn logged() -> String {
    std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default()
}
/// Put an unrelated future preference of `len` bytes into the stored record, the way a newer
/// build's record would carry one.
fn seed_pad(store: &mut Store, len: usize) {
    let record = store.rpc.record.as_mut().expect("a record to seed");
    let mut state: Value = serde_json::from_str(record["state"].as_str().unwrap()).unwrap();
    state["public"]["preferences"]["future_pad"] = json!("p".repeat(len));
    record["state"] = json!(state.to_string());
}

// ---- states built by hand, and platform's own apply -------------------------------------------

fn canonical(public: &state::PublicPayload, envelope: usize) -> state::CanonicalState {
    let mut canonical = state::CanonicalState::new(state::Flavor::Stable, state::Generation([1; 16]));
    canonical.public = public.clone();
    canonical.auth_envelope = Some("e".repeat(envelope));
    canonical.migrations.session.progress = state::MigrationProgress::Complete;
    canonical.migrations.consent.progress = state::MigrationProgress::Complete;
    canonical
}

/// What the platform makes of `mutation` on `state`, with the worst-case operation id the guard
/// sizes, and the plaintext digest the helper stamps on the ledger entry of an auth write before it
/// persists the candidate (`storage_service::backend`).
fn real_apply(state: &state::CanonicalState, mutation: state::Mutation) -> Result<state::CanonicalState, state::StateError> {
    real_apply_as(state, state::Generation([255; 16]), mutation)
}
fn real_apply_as(
    state: &state::CanonicalState,
    id: state::Generation,
    mutation: state::Mutation,
) -> Result<state::CanonicalState, state::StateError> {
    let auth = matches!(mutation, state::Mutation::ReplaceAuth { .. });
    let operation = state::Operation::new(id, state.expected(), mutation)?;
    let (mut next, _) = state.apply(&operation)?;
    if auth {
        next.operations.last_mut().expect("the write is in the ledger").verification =
            Some(state::ReceiptVerification::Auth { plaintext_digest: Some(state::digest_bytes(b"the sealed plaintext")) });
    }
    Ok(next)
}
fn update(public: &state::PublicPayload) -> state::Mutation {
    state::Mutation::UpdatePreferences { public: public.clone() }
}

/// A public payload carrying `fill` for `users` users, and nothing else worth sizing.
fn public_with_memory(users: usize, fill: Fill) -> (state::PublicPayload, String) {
    let mut session = household(users, 1);
    fill_memory(&mut session, users, fill);
    split_canonical(&session).expect("a splittable session")
}

/// `public` with an unrelated future preference sized so the UPDATE write lands exactly `delta`
/// logical bytes over the cap (negative: under it). The pad is not memory, so the guard may never
/// touch it.
fn over_by(base: &state::CanonicalState, public: state::PublicPayload, delta: i64) -> state::PublicPayload {
    over_by_for(base, public, persistence::DocumentWrite::Update, "", delta)
}

/// The same for any `write` of `protected` credentials.
fn over_by_for(
    base: &state::CanonicalState,
    mut public: state::PublicPayload,
    write: persistence::DocumentWrite,
    protected: &str,
    delta: i64,
) -> state::PublicPayload {
    public.preferences["future_pad"] = json!("");
    let at_zero = persistence::projected_size(base, &public, write, protected).unwrap() as i64;
    let len = MAX_LOGICAL_BYTES as i64 + delta - at_zero;
    assert!(len >= 0, "the fixture is already over the cap before the pad ({at_zero})");
    public.preferences["future_pad"] = json!("p".repeat(len as usize));
    assert_eq!(
        persistence::projected_size(base, &public, write, protected).unwrap() as i64,
        MAX_LOGICAL_BYTES as i64 + delta
    );
    public
}

// ---- the projection is platform's own document -------------------------------------------------

/// **The guard sizes what the platform builds.** For an `UpdatePreferences` the projected size is
/// EXACTLY the logical size of the state `CanonicalState::apply` returns, with a full ledger, an
/// empty one, and a stored refresh that the update cancels; for a one-put `ReplaceAuth` it is exactly
/// that too (the consent a replace keeps included); and a staged one is a document the platform
/// accepts as `pending_auth` and promotes.
///
/// It holds at every revision, in particular where `revision + 1` is one digit wider than
/// `revision` (9 -> 10, 99 -> 100): the platform stamps the new revision on the document, so a
/// projection that kept the old one would be a byte short exactly there.
#[test]
fn the_projection_is_the_document_the_platform_builds() {
    for revision in [9, 40, 99, 999] {
        projection_is_the_platform_document_at(revision);
    }
}

fn projection_is_the_platform_document_at(revision: u64) {
    use persistence::Seal;
    let (public, protected) = public_with_memory(3, Fill { track: 4, offsets: 3, views: 3 });
    // the size of the envelope each way of protecting the credentials produces (the estimate reads
    // only the state's auth context, which every base below shares)
    let envelope_of = |seal| persistence::sealed_envelope_estimate(&canonical(&public, 0), &protected, seal);
    assert!(envelope_of(Seal::Acl) < envelope_of(Seal::Keymanager), "the ACL envelope is the smaller");
    assert!(envelope_of(Seal::Keymanager) <= envelope_of(Seal::Either), "either is the larger of the two");
    let envelope = envelope_of(Seal::Keymanager);
    // a ledger holds one entry per revision at most, so a young document cannot hold a full one
    for (case, ledger, consent) in [("empty ledger", 0, false), ("full ledger", state::LEDGER_CAPACITY.min(revision as usize), true)] {
        let case = format!("revision {revision}, {case}");
        let mut base = canonical(&public, envelope);
        base.revision = revision;
        base.auth_generation = state::Generation([7; 16]);
        for n in 0..ledger {
            base.operations.push(state::LedgerEntry {
                id: state::Generation([n as u8 + 1; 16]),
                digest: "0".repeat(64),
                result: state::Applied {
                    revision: revision - ledger as u64 + 1 + n as u64,
                    epoch: 0,
                    auth_generation: base.auth_generation,
                },
                verification: Some(state::ReceiptVerification::Public),
            });
        }
        if consent {
            base.public.consent = json!({"answered": ["x".repeat(300)]});
            base.public.scopes = json!({"s": "y".repeat(200)});
        }
        let mut next = public.clone();
        next.preferences["playback_quality"] = json!("1080p_20_mbps");

        let real = real_apply(&base, update(&next)).expect("an update applies");
        let projected = persistence::projected_size(&base, &next, persistence::DocumentWrite::Update, "").unwrap();
        assert_eq!(projected, logical_size(&serde_json::to_value(&real).unwrap()), "{case}: update");

        for seal in [Seal::Acl, Seal::Keymanager, Seal::Either] {
            let replace = state::Mutation::ReplaceAuth {
                auth_generation: state::Generation([255; 16]),
                auth_envelope: "x".repeat(envelope_of(seal)),
                public: next.clone(),
            };
            let real = real_apply(&base, replace).expect("a replace applies");
            let projected =
                persistence::projected_size(&base, &next, persistence::DocumentWrite::Replace(seal), &protected).unwrap();
            assert_eq!(projected, logical_size(&serde_json::to_value(&real).unwrap()), "{case}: replace, {seal:?}");
        }
    }

    // a sign-in supersedes a legacy import still waiting in the document (and an update may not run
    // beside one, so the replace is the only write that meets it)
    let mut waiting = canonical(&public, envelope);
    waiting.revision = revision;
    waiting.migrations.session = state::DomainMigration {
        progress: state::MigrationProgress::Pending,
        pending_import: Some("m".repeat(5_000)),
    };
    let mut next = public.clone();
    next.preferences["playback_quality"] = json!("1080p_20_mbps");
    let replace = state::Mutation::ReplaceAuth {
        auth_generation: state::Generation([255; 16]),
        auth_envelope: "x".repeat(envelope),
        public: next.clone(),
    };
    let real = real_apply(&waiting, replace).expect("a sign-in applies beside a waiting import");
    assert_eq!(real.migrations.session.pending_import, None);
    assert_eq!(
        persistence::projected_size(&waiting, &next, persistence::DocumentWrite::Replace(Seal::Keymanager), &protected).unwrap(),
        logical_size(&serde_json::to_value(&real).unwrap()),
        "the waiting import goes with the replace"
    );

    // a strict refresh is STAGED: the active state stays as it is, and the new public half and a
    // freshly sealed envelope ride in `pending_auth` beside the old ones. Built here the way
    // `Backend::stage_strict` builds it. The platform validates it, AND the document it becomes once
    // promoted, so the projection is the larger of the two.
    let mut base = canonical(&public, envelope);
    base.revision = revision;
    let operation = state::Operation::new(
        state::Generation([255; 16]),
        base.expected(),
        state::Mutation::ReplaceAuth {
            auth_generation: state::Generation([255; 16]),
            auth_envelope: "x".repeat(envelope),
            public: next.clone(),
        },
    )
    .unwrap();
    let mut staged = base.clone();
    staged.pending_auth = Some(state::PendingAuth { operation, plaintext_digest: state::digest_bytes(b"the sealed plaintext") });
    staged.encode().expect("the platform accepts the staged document");
    let (promoted, _) = staged.pending_candidate().expect("and its candidate").expect("is there");
    promoted.encode().expect("and the promoted one validates");
    let staged_size = logical_size(&serde_json::to_value(&staged).unwrap());
    let promoted_size = logical_size(&serde_json::to_value(&promoted).unwrap());
    assert_eq!(
        persistence::projected_size(&base, &next, persistence::DocumentWrite::Staged, &protected).unwrap(),
        staged_size.max(promoted_size),
        "staged {staged_size}, promoted {promoted_size}"
    );
    assert!(staged_size > promoted_size, "the staged document is the larger: it carries both halves");

    // an update cancels a refresh stored beside it, so it is sized without it
    // (the staged operation holds id 255; the update draws its own)
    let real = real_apply_as(&staged, state::Generation([254; 16]), update(&next)).expect("an update cancels the pending refresh");
    assert!(real.pending_auth.is_none());
    assert_eq!(
        persistence::projected_size(&staged, &next, persistence::DocumentWrite::Update, "").unwrap(),
        logical_size(&serde_json::to_value(&real).unwrap()),
        "an update is sized without the refresh it cancels"
    );
}

// ---- the envelope: sized by the way the credentials are protected ------------------------------

/// A keymanager that seals nothing and answers with what AES-GCM would: the plaintext plus a 16-byte
/// tag, base64. That is all `keymanager::seal` needs to build an envelope of the real size.
struct LengthKeymanager;
impl plx_platform::storage::keymanager::Rpc for LengthKeymanager {
    fn call(&mut self, uri: &str, payload: &Value) -> Result<Value, ErrorCode> {
        // base64 of `n` zero bytes
        let zeros = |n: usize| format!("{}{}", "AAAA".repeat(n / 3), ["", "AA==", "AAA="][n % 3]);
        Ok(if uri.ends_with("/begin") {
            json!({"returnValue": true, "handle": "1", "iv": zeros(12)})
        } else if uri.ends_with("/finish") {
            let data = payload["data"].as_str().expect("the inner document");
            let padding = data.bytes().rev().take_while(|b| *b == b'=').count();
            json!({"returnValue": true, "output": zeros(data.len() / 4 * 3 - padding + 16)})
        } else {
            json!({"returnValue": true})
        })
    }
}

/// **A keymanager envelope is bigger than its plaintext by more than base64.** The ciphertext seals
/// the INNER document `{"context", "payload"}`, whose payload is the plaintext JSON-escaped (every
/// quote of a credential bundle costs a byte), so sizing the raw plaintext falls short by a third of
/// the escapes and the context, and a staged write sized that way is refused by the platform. The
/// estimate is held to the envelope `keymanager::seal` really builds, small and at the largest roster.
#[test]
fn the_keymanager_estimate_covers_the_envelope_the_helper_seals() {
    let state = canonical(&Default::default(), 0);
    let worst = state::Generation([255; 16]);
    let mut context = state.auth_context();
    context.auth_generation = worst;
    for (users, sources) in [(1, 0), (2, 1), (USERS, USERS)] {
        let (_, protected) = split_canonical(&household(users, sources)).unwrap();
        let sealed = plx_platform::storage::keymanager::seal(
            &mut LengthKeymanager,
            &worst.key_name(),
            &protected,
            serde_json::to_value(&context).unwrap(),
        )
        .expect("the length-preserving keymanager seals");
        let estimate = persistence::sealed_envelope_estimate(&state, &protected, persistence::Seal::Keymanager);
        let quotes = protected.bytes().filter(|b| *b == b'"').count();
        assert!(
            sealed.len() <= estimate && estimate - sealed.len() <= 8,
            "{users}x{sources}: sealed {} bytes, estimated {estimate} ({} bytes of plaintext, {quotes} quotes)",
            sealed.len(),
            protected.len()
        );
        // the earlier figure, base64 of the raw plaintext plus a flat 512, was under it
        assert!(users < 2 || (protected.len() + 16).div_ceil(3) * 4 + 512 < sealed.len(), "{users}x{sources}");
    }
}

/// How many bytes narrower than the worst case a stored document prints its random identifiers: the
/// projection prints them at three digits a byte, a real draw at one to three. The new generation
/// is in the document three times (itself, its ledger entry, the envelope's context) and the
/// operation id once.
fn narrower_than_worst(stored: &state::CanonicalState) -> usize {
    let spare = |id: &state::Generation| id.0.iter().map(|b| 3 - b.to_string().len()).sum::<usize>();
    3 * spare(&stored.auth_generation) + stored.operations.last().map_or(0, |entry| spare(&entry.id))
}

/// The state a first write starts from names its flavor in the document and again in the envelope's
/// context: a nightly install writes one byte more of each than a stable one, so it is sized as one.
#[test]
fn a_first_write_is_sized_under_the_widest_flavor() {
    let width = |flavor| serde_json::to_string(&flavor).unwrap().len();
    let widest = [state::Flavor::Stable, state::Flavor::Debug, state::Flavor::Nightly].map(width).into_iter().max();
    assert_eq!(Some(width(persistence::fresh_state().flavor)), widest);
}

/// **The ACL projection is the document the helper stores.** On the ACL-only tier (webOS 4.x, where
/// every credential change, profile switch and fresh sign-in is a one-put replace) the envelope is
/// the plaintext JSON-escaped beside the auth context, not its base64. Held to the record the
/// production coordinator and the real backend leave behind: exactly the projected size, less the
/// digits the random identifiers did not use.
#[test]
fn the_acl_projection_is_the_document_the_helper_stores() {
    plx_base::eventlog::with_private_log(|| {
        for (users, sources) in [(3, 1), (USERS, USERS)] {
            let mut store = store();
            let session = household(users, sources);
            commit(&mut store, &session, SaveAuthority::FreshReauthentication);
            let mut changed = session.clone();
            changed.account_token = "b".repeat(20);
            fill_memory(&mut changed, users, Fill { track: 2, offsets: 2, views: 2 });
            let (public, protected) = split_canonical(&changed).unwrap();
            let before = snapshot(&mut store).state;
            let projected = persistence::projected_size(
                &before,
                &public,
                persistence::DocumentWrite::Replace(persistence::Seal::Acl),
                &protected,
            )
            .unwrap();

            commit(&mut store, &changed, SaveAuthority::Routine);
            let stored = snapshot(&mut store).state;
            let real = logical_size(&serde_json::to_value(&stored).unwrap());
            assert!(!logged().contains("prefs: evicted"), "{users}x{sources}: nothing needed shedding: {}", logged());
            assert_eq!(projected - narrower_than_worst(&stored), real, "{users}x{sources}: projected {projected}, stored {real}");
        }
    });
}

// ---- the order, and only as far as needed ------------------------------------------------------

fn lists(preferences: &Value) -> (usize, usize, usize) {
    (count(preferences, "track_memory"), count(preferences, "subtitle_offsets"), count(preferences, "library_sorts"))
}

/// `public` without the named memory lists.
fn without(public: &state::PublicPayload, keys: &[&str]) -> state::PublicPayload {
    let mut public = public.clone();
    for key in keys {
        public.preferences.as_object_mut().unwrap().remove(*key);
    }
    public
}

/// **The guard sheds `track_memory`, then `subtitle_offsets`, then `library_sorts`, each oldest
/// first from the record holding the most, and only as far as the document needs** — and what is
/// left is a document the platform applies. An unrelated preference is never touched.
///
/// Each tier's overshoot is measured, not guessed: the unrelated pad is sized so the document is over
/// the cap by exactly what a list (or several) take out of it, plus one byte for "past".
#[test]
fn the_guard_sheds_in_order_and_only_as_far_as_the_document_needs() {
    let (public, _) = public_with_memory(4, Fill { track: 5, offsets: 6, views: 6 });
    let base = canonical(&public, 2_000);
    let size = |public: &state::PublicPayload| {
        let mut padded = public.clone();
        padded.preferences["future_pad"] = json!("");
        persistence::projected_size(&base, &padded, persistence::DocumentWrite::Update, "").unwrap() as i64
    };
    let full_size = size(&public);
    let take = |keys: &[&str]| full_size - size(&without(&public, keys));
    let (no_track, no_offsets, no_all) = (
        take(&["track_memory"]),
        take(&["track_memory", "subtitle_offsets"]),
        take(&["track_memory", "subtitle_offsets", "library_sorts"]),
    );
    let full = lists(&public.preferences);
    assert!(full.0 > 8 && full.1 > 8 && full.2 > 8, "{full:?}");
    assert!(0 < no_track && no_track < no_offsets && no_offsets < no_all);

    // fits: nothing happens
    for delta in [-50_000, 0] {
        let mut next = over_by(&base, public.clone(), delta);
        let before = next.clone();
        assert!(persistence::fit_document(&base, &mut next, persistence::DocumentWrite::Update, "").is_empty(), "{delta}");
        assert_eq!(next, before);
    }

    // (case, how far over the cap, lists touched, lists emptied)
    let tiers = [
        ("one byte", 1, [true, false, false], [false, false, false]),
        ("past track memory", no_track + 1, [true, true, false], [true, false, false]),
        ("past offsets", no_offsets + 1, [true, true, true], [true, true, false]),
        ("exactly everything", no_all, [true, true, true], [true, true, true]),
        ("past everything", no_all + 1, [true, true, true], [true, true, true]),
    ];
    for (case, delta, touched, emptied) in tiers {
        let mut next = over_by(&base, public.clone(), delta);
        let pad = next.preferences["future_pad"].clone();
        let shed = persistence::fit_document(&base, &mut next, persistence::DocumentWrite::Update, "");
        let after = lists(&next.preferences);
        let (was, now) = ([full.0, full.1, full.2], [after.0, after.1, after.2]);
        let names = ["track_memory", "subtitle_offsets", "library_sorts"];
        assert_eq!(next.preferences["future_pad"], pad, "{case}: an unrelated preference is never shed");
        let reported: Vec<&str> = shed.iter().map(|(name, _)| *name).collect();
        let expected: Vec<&str> = (0..3).filter(|&i| touched[i]).map(|i| names[i]).collect();
        assert_eq!(reported, expected, "{case}: the lists are shed in order, and a later one only after the earlier is gone");
        for i in 0..3 {
            assert_eq!(now[i] < was[i], touched[i], "{case}: {} {was:?} -> {now:?}", names[i]);
            assert_eq!(now[i] == 0, emptied[i], "{case}: {} {was:?} -> {now:?}", names[i]);
            if let Some((_, evicted)) = shed.iter().find(|(name, _)| *name == names[i]) {
                assert_eq!(was[i] - now[i], *evicted, "{case}: {}", names[i]);
            } else {
                assert_eq!(next.preferences.get(names[i]), public.preferences.get(names[i]), "{case}: {} untouched", names[i]);
            }
        }
        let applied = real_apply(&base, update(&next));
        if case == "past everything" {
            // nothing sheddable is left and the document is still over: the guard never invents room,
            // so the platform refuses this write exactly as it did before the guard
            assert!(applied.is_err(), "{case}");
        } else {
            let applied = applied.unwrap_or_else(|e| panic!("{case}: {e:?}"));
            // and it shed no more than it had to: what is left over is smaller than the entry shed last
            // (a list that empties also frees its key, which the guard does not count)
            let room = MAX_LOGICAL_BYTES - logical_size(&serde_json::to_value(&applied).unwrap());
            assert!(room < 545, "{case}: shed {room} bytes more than the cap required");
        }
    }

    // the evictions are the OLDEST of the LARGEST record: ties to the earlier record
    let mut next = over_by(&base, public.clone(), 1);
    persistence::fit_document(&base, &mut next, persistence::DocumentWrite::Update, "");
    let before: Vec<ProfileList<TrackMemoryEntry>> = serde_json::from_value(public.preferences["track_memory"].clone()).unwrap();
    let after: Vec<ProfileList<TrackMemoryEntry>> = serde_json::from_value(next.preferences["track_memory"].clone()).unwrap();
    assert_eq!(before.len(), after.len());
    assert_eq!(before[0].items[1..], after[0].items[..], "the first record held the most (a tie) and lost its oldest entry");
    assert_eq!(before[1..], after[1..]);

    // and so are the other two lists: the order inside a list is a rule of its own, not of the first
    for (key, field) in [("track_memory", "items"), ("subtitle_offsets", "items"), ("library_sorts", "libs")] {
        a_list_is_shed_oldest_first_from_the_largest_record(key, field);
    }
}

/// **Inside one list the guard takes the OLDEST entry of the record holding the MOST entries, and on
/// a tie the earlier record**, for each of the three lists alike (`library_sorts` is not a
/// `ProfileList`, so its record is a different type; the rule is one).
///
/// Five profiles whose records hold `[3, 6, 6, 4, 6]` entries, so that the largest record, the first
/// of a tie, the oldest entry and the newest entry are all different things: the record that loses
/// an entry is `1, 2, 4, 1, 2, 4, 1` (each loses its front, and a record that has lost one is no
/// longer the largest). For every number of evictions the document is made over the cap by exactly
/// what they free, and what the guard leaves is the list with exactly those entries gone.
fn a_list_is_shed_oldest_first_from_the_largest_record(key: &str, field: &str) {
    const KEEP: [usize; 5] = [3, 6, 6, 4, 6];
    const VICTIMS: [usize; 7] = [1, 2, 4, 1, 2, 4, 1];
    let (mut public, _) = public_with_memory(KEEP.len(), Fill { track: 6, offsets: 6, views: 6 });
    // only this list can be shed, so the first evictions are its own
    for other in ["track_memory", "subtitle_offsets", "library_sorts"].into_iter().filter(|other| *other != key) {
        public.preferences.as_object_mut().unwrap().remove(other);
    }
    let records = public.preferences[key].as_array_mut().unwrap();
    assert_eq!(records.len(), KEEP.len(), "{key}");
    for (record, keep) in records.iter_mut().zip(KEEP) {
        let entries = record[field].as_array_mut().unwrap();
        assert_eq!(entries.len(), 6, "{key}: the fixture holds six an entry");
        entries.drain(..6 - keep);
    }
    let base = canonical(&public, 2_000);
    let size = |public: &state::PublicPayload| {
        persistence::projected_size(&base, public, persistence::DocumentWrite::Update, "").unwrap() as i64
    };
    let full = size(&public);
    let mut expected = public.clone();
    for (step, victim) in VICTIMS.into_iter().enumerate() {
        expected.preferences[key][victim][field].as_array_mut().unwrap().remove(0);
        let mut next = over_by(&base, public.clone(), full - size(&expected));
        let shed = persistence::fit_document(&base, &mut next, persistence::DocumentWrite::Update, "");
        next.preferences.as_object_mut().unwrap().remove("future_pad");
        let shed: Vec<(&str, usize)> = shed.into_iter().collect();
        assert_eq!(shed, vec![(key, step + 1)], "{key}: after {} evictions", step + 1);
        assert_eq!(
            next.preferences[key], expected.preferences[key],
            "{key}: after {} evictions the records {:?} each lost their oldest",
            step + 1,
            &VICTIMS[..=step]
        );
    }
}

/// A payload whose lists do not parse as this build's types is left alone, not rewritten: shedding
/// is for data this build wrote.
#[test]
fn the_guard_leaves_a_list_it_cannot_read_alone() {
    let (mut public, _) = public_with_memory(2, Fill { track: 2, offsets: 2, views: 2 });
    public.preferences["track_memory"] = json!({"version": 9, "profiles": {"x": "y".repeat(2_000)}});
    public.preferences["subtitle_offsets"] = json!("not a list");
    let base = canonical(&public, 2_000);
    let mut next = over_by(&base, public.clone(), 100);
    let (track, offsets) = (next.preferences["track_memory"].clone(), next.preferences["subtitle_offsets"].clone());
    let shed = persistence::fit_document(&base, &mut next, persistence::DocumentWrite::Update, "");
    assert_eq!(next.preferences["track_memory"], track);
    assert_eq!(next.preferences["subtitle_offsets"], offsets);
    assert!(shed.iter().all(|(name, _)| *name == "library_sorts"), "{shed:?}");
}

// ---- which write is sent, and so which document is sized ---------------------------------------

fn snapshot_of(state: state::CanonicalState, auth: AuthLoad) -> client::Snapshot {
    client::Snapshot { db_rev: "1".into(), state, auth, protection: None }
}

/// The shape the guard assumes for every combination of what is written and what is stored. The
/// rows are the policy of `protection_for_auth_write` read off its branches: a routine (not fresh)
/// credential write is strict, and so STAGED, when the record already holds non-ACL ciphertext, or
/// holds none and the firmware is newer than 4.x; a strict `ReplaceAuth` is the only write the
/// helper stages, and a fresh sign-in, a migration and a public-only write never are.
#[test]
fn the_guard_assumes_the_shape_of_the_write_that_is_sent() {
    use persistence::DocumentWrite::{Replace, Staged, Update};
    use persistence::Seal::{Acl, Either};
    let acl = json!({"format": "db8-acl-only-v1"}).to_string();
    let keymanager = json!({"format": "db8-keymanager-v1"}).to_string();
    let plain = || AuthLoad::Plaintext { payload: SecretString("{}".into()) };
    let locked = || AuthLoad::Locked { code: ErrorCode::Authentication };
    let with = |envelope: Option<&str>| {
        let mut s = canonical(&Default::default(), 0);
        s.auth_envelope = envelope.map(str::to_owned);
        s
    };
    let routine = SaveAuthority::Routine;
    let fresh = SaveAuthority::FreshReauthentication;
    let public_only = SaveAuthority::PublicOnly;
    // (migration, authority, unchanged, major, stored envelope, auth load, expected)
    let rows: Vec<(bool, SaveAuthority, bool, u32, Option<&str>, AuthLoad, Option<persistence::DocumentWrite>)> = vec![
        (true, routine, false, 5, Some(&acl), plain(), Some(Replace(Either))),
        (true, public_only, false, 4, None, plain(), Some(Replace(Acl))),
        (false, public_only, false, 5, Some(&keymanager), plain(), Some(Update)),
        (false, routine, true, 5, Some(&keymanager), plain(), Some(Update)),
        (false, fresh, true, 5, Some(&keymanager), plain(), Some(Replace(Either))),
        (false, fresh, false, 4, Some(&acl), plain(), Some(Replace(Acl))),
        (false, routine, false, 4, Some(&acl), plain(), Some(Replace(Acl))),
        (false, routine, false, 5, Some(&acl), plain(), Some(Replace(Acl))),
        // a record that already holds healthy ciphertext keeps it, whatever the firmware says
        (false, routine, false, 4, Some(&keymanager), plain(), Some(Staged)),
        (false, routine, false, 4, None, plain(), Some(Replace(Acl))),
        (false, routine, false, 5, Some(&keymanager), plain(), Some(Staged)),
        (false, routine, false, 5, None, plain(), Some(Staged)),
        (false, routine, false, 5, Some(&keymanager), locked(), None),
        (false, fresh, false, 5, Some(&keymanager), locked(), Some(Replace(Either))),
    ];
    for (n, (migration, authority, unchanged, major, envelope, auth, expected)) in rows.into_iter().enumerate() {
        let snapshot = snapshot_of(with(envelope), auth);
        assert_eq!(persistence::document_write(&snapshot, migration, authority, unchanged, major), expected, "row {n}");
    }
}

/// The guard's name for the write a wire mutation makes, read off the mutation itself.
fn sent_as(mutation: &WireMutation) -> Option<persistence::DocumentWrite> {
    use persistence::DocumentWrite::{Replace, Staged, Update};
    use persistence::Seal;
    let seal = |protection: &ProtectionRequest| match protection {
        ProtectionRequest::Db8AclOnlyExplicit => Seal::Acl,
        ProtectionRequest::KeymanagerRequired => Seal::Keymanager,
        ProtectionRequest::KeymanagerWithAclFallback => Seal::Either,
    };
    match mutation {
        WireMutation::UpdatePreferences { .. } => Some(Update),
        WireMutation::ReplaceAuth { protection: ProtectionRequest::KeymanagerRequired, .. } => Some(Staged),
        WireMutation::ReplaceAuth { protection, .. } => Some(Replace(seal(protection))),
        WireMutation::AdvanceMigration { migration: MigrationMutation::SessionComplete { protection, .. } } => {
            Some(Replace(seal(protection)))
        }
        _ => None,
    }
}

/// The same, against the mutation `commit_session_with` really sends through the production
/// coordinator (the ACL-only tier of the fake store: update, replace and the unchanged refresh),
/// the way the credentials are protected included: that decides how big the envelope is. A staged
/// write needs a Keymanager envelope the fake cannot open; the rows above cover it.
#[test]
fn the_guard_sizes_the_mutation_that_is_actually_sent() {
    use persistence::DocumentWrite::{Replace, Update};
    use persistence::Seal::Acl;
    let mut store = store();
    let session = household(2, 1);
    commit(&mut store, &session, SaveAuthority::FreshReauthentication);
    let mut changed = session.clone();
    changed.account_token = "b".repeat(20);

    let cases: [(&str, &Session, SaveAuthority, persistence::DocumentWrite); 5] = [
        ("public only", &session, SaveAuthority::PublicOnly, Update),
        ("unchanged credentials", &session, SaveAuthority::Routine, Update),
        ("changed credentials", &changed, SaveAuthority::Routine, Replace(Acl)),
        ("fresh sign-in", &session, SaveAuthority::FreshReauthentication, Replace(Acl)),
        ("fresh sign-in, changed", &changed, SaveAuthority::FreshReauthentication, Replace(Acl)),
    ];
    for (case, candidate, authority, sent) in cases {
        let loaded = snapshot(&mut store);
        let unchanged = matches!(&loaded.auth, AuthLoad::Plaintext { payload } if super::super::protected_matches(candidate, &payload.0));
        let guessed = persistence::document_write(&loaded, false, authority, unchanged, 4).expect("a write is sent");
        let mut seen = Vec::new();
        let mut transport = |request: Request| {
            if let Request::Commit { mutation, .. } = &request {
                seen.push(sent_as(mutation));
            }
            Ok(store.dispatch(request))
        };
        let result = persistence::commit_session_with(candidate, false, authority, 4, &mut transport);
        assert!(matches!(result, persistence::CanonicalCommit::Durable { .. }), "{case}: {result:?}");
        assert_eq!(seen, [Some(sent)], "{case}");
        assert_eq!(guessed, sent, "{case}: the guard sized a different write than the one sent");
    }
}

// ---- C-d1: a preference write over the cap lands ----------------------------------------------

/// **Delta C-d1.** Until the guard, a document at the cap refused EVERY write — quality, pins,
/// consent — because per-profile memory had grown (`LogicalTooLarge`). Now the write that would take
/// the document over evicts memory instead, `track_memory` first and only as many entries as it
/// takes, and lands; the unrelated preference stored beside it is untouched, and the eviction is
/// logged.
#[test]
fn preference_write_over_the_cap_evicts_memory_and_lands() {
    plx_base::eventlog::with_private_log(|| {
        let mut store = store();
        let session = household(2, 1);
        commit(&mut store, &session, SaveAuthority::FreshReauthentication);

        // the write: ten new track-memory entries and a quality change, sized to land 2.5 entries over
        let mut next = session.clone();
        for n in 0..10 {
            assert!(put(&mut next.prefs.track_memory, "", worst(n)));
        }
        next.prefs.playback_quality = Some(PlaybackQuality::P1080High);
        let entry = logical_size(&serde_json::to_value(worst(0)).unwrap()) + 1;
        let (public, _) = split_canonical(&next).unwrap();
        let loaded = snapshot(&mut store);
        let sized = over_by(&loaded.state, public, (entry * 5 / 2) as i64);
        let pad = sized.preferences["future_pad"].as_str().unwrap().len();
        seed_pad(&mut store, pad);
        let stored = snapshot(&mut store);
        assert!(logical_size(&serde_json::to_value(&stored.state).unwrap()) <= MAX_LOGICAL_BYTES, "the stored record is valid");

        commit(&mut store, &next, SaveAuthority::PublicOnly);

        let preferences = stored_preferences(&mut store);
        assert_eq!(preferences["playback_quality"], json!("1080p_20_mbps"), "the unrelated write landed");
        assert_eq!(preferences["future_pad"].as_str().map(str::len), Some(pad), "an unrelated preference is not shed");
        let kept: Vec<ProfileList<TrackMemoryEntry>> = serde_json::from_value(preferences["track_memory"].clone()).unwrap();
        let shows: Vec<&str> = kept[0].items.iter().map(|e| e.show_rk.as_str()).collect();
        let newest: Vec<String> = (3..10).map(show).collect();
        assert_eq!(shows, newest, "exactly three entries went, the oldest");
        assert!(logged().contains("prefs: evicted 3 track_memory entries to fit the 256 KiB document"), "{}", logged());
        assert_eq!(logged().matches("prefs: evicted").count(), 1, "{}", logged());
        assert!(logical_size(&serde_json::to_value(&snapshot(&mut store).state).unwrap()) <= MAX_LOGICAL_BYTES);
    });
}

/// A write that fits is not touched: nothing is evicted and nothing is logged.
#[test]
fn a_write_that_fits_evicts_nothing() {
    plx_base::eventlog::with_private_log(|| {
        let mut store = store();
        let mut session = household(2, 1);
        fill_memory(&mut session, 2, Fill { track: 6, offsets: 4, views: 4 });
        commit(&mut store, &session, SaveAuthority::FreshReauthentication);
        let before = stored_preferences(&mut store);
        session.prefs.playback_quality = Some(PlaybackQuality::P1080High);
        commit(&mut store, &session, SaveAuthority::PublicOnly);
        let after = stored_preferences(&mut store);
        for key in ["track_memory", "subtitle_offsets", "library_sorts"] {
            assert_eq!(after[key], before[key], "{key}");
        }
        assert!(!logged().contains("prefs: evicted"), "{}", logged());
    });
}

/// **An ACL write that fits is not evicted for.** The envelope of a credential write on the ACL-only
/// tier is the plaintext beside a small context; sized as a keymanager envelope (a third bigger) the
/// same write would look over the cap and the guard would evict memory from a write the platform
/// stores. This one is sized to land exactly AT the cap as the ACL tier writes it, with the keymanager
/// figure over it: nothing is evicted and nothing is logged.
#[test]
fn an_acl_credential_write_that_fits_evicts_nothing() {
    use persistence::DocumentWrite::Replace;
    use persistence::Seal;
    plx_base::eventlog::with_private_log(|| {
        let mut store = store();
        let session = with_server_lists(household(USERS, USERS), USERS);
        commit(&mut store, &session, SaveAuthority::FreshReauthentication);
        let mut changed = session.clone();
        changed.account_token = "b".repeat(20);
        fill_memory(&mut changed, USERS, Fill { track: 4, offsets: 4, views: 4 });
        let (public, protected) = split_canonical(&changed).unwrap();
        let before = snapshot(&mut store).state;

        let sized = over_by_for(&before, public, Replace(Seal::Acl), &protected, 0);
        let pad = sized.preferences["future_pad"].as_str().unwrap().len();
        assert!(
            persistence::projected_size(&before, &sized, Replace(Seal::Keymanager), &protected).unwrap() > MAX_LOGICAL_BYTES,
            "the window exists: sized as a keymanager envelope this write would be over the cap"
        );
        seed_pad(&mut store, pad);
        assert!(logical_size(&serde_json::to_value(&snapshot(&mut store).state).unwrap()) <= MAX_LOGICAL_BYTES);

        commit(&mut store, &changed, SaveAuthority::Routine);

        assert!(!logged().contains("prefs: evicted"), "{}", logged());
        let preferences = stored_preferences(&mut store);
        assert_eq!(lists(&preferences), lists(&sized.preferences), "no memory was evicted");
        assert_eq!(preferences["future_pad"].as_str().map(str::len), Some(pad));
        assert_eq!(opened(&mut store).account_token, changed.account_token, "and the credential write landed");
        let stored = snapshot(&mut store).state;
        assert!(logical_size(&serde_json::to_value(&stored).unwrap()) <= MAX_LOGICAL_BYTES);
        stored.encode().expect("and it passes the platform's own validation");
    });
}

// ---- the worst-case household ------------------------------------------------------------------

/// **The worst-case household's document fits.** Sixteen Home users, each holding the credentials of
/// sixteen servers (the roster's largest; nothing caps a profile's sources), every per-profile list at
/// its cap with every string at its bound (`track_memory` at the 32 KiB budget), sixteen servers' pins
/// and consent, four recent searches each. Together that is far more than 256 KiB, and it is written
/// through the production coordinator on the ACL-only tier (webOS 4.x): the guard sheds the three
/// memory lists in order until the document passes the platform's own validation, the credentials
/// and every unsheddable preference are untouched, and an unrelated quality write afterwards lands.
///
/// The figures it stands on are asserted, not assumed: with ALL per-profile memory gone the same
/// household's document, sealed either way, is under the cap, so shedding can always reach it for
/// every write that puts ONE envelope in the document (an update, a one-put replace, a first write).
///
/// **The limit it also states.** At this size one envelope is over a third of the cap, so a STRICT
/// refresh (staged: the old and the new envelope side by side) cannot fit even with every list shed.
/// That comes from the credentials alone and was so before per-profile memory existed (the platform
/// refuses that write as it always did); `a_staged_write_is_guarded_where_two_envelopes_fit` covers
/// the staged shape where the household leaves room for it.
#[test]
fn worst_case_household_document_fits() {
    use persistence::Seal;
    plx_base::eventlog::with_private_log(|| {
        let mut worst_case = with_server_lists(household(USERS, SOURCES), USERS);
        fill_memory(&mut worst_case, USERS, FULL);

        // every list is at its cap, strings at their bounds
        assert_eq!(worst_case.prefs.subtitle_offsets.iter().map(|r| r.items.len()).collect::<Vec<_>>(), [SubtitleOffsets::CAP; USERS]);
        assert_eq!(worst_case.prefs.library_views.iter().map(|r| r.libs.len()).collect::<Vec<_>>(), [LibraryViews::CAP; USERS]);
        assert_eq!((worst_case.prefs.server_key_pins.len(), worst_case.prefs.plaintext_consent.len()), (USERS, USERS));
        assert!(worst_case.recent_searches.iter().all(|r| r.terms.len() == data_recents_cap()));
        assert!(worst_case.profiles.iter().all(|p| p.sources.len() == SOURCES) && SOURCES == USERS, "sixteen sources each");
        let budget_used = super::list_size(&worst_case.prefs.track_memory);
        assert!(
            (BUDGET_BYTES - 520..=BUDGET_BYTES).contains(&budget_used),
            "track memory fills its household budget: {budget_used} of {BUDGET_BYTES}"
        );

        let (public, protected) = split_canonical(&worst_case).unwrap();
        let memory = ["track_memory", "subtitle_offsets", "library_sorts"].map(|k| logical_size(&public.preferences[k]));
        let unguarded = logical_size(&serde_json::to_value(&public).unwrap()) + protected.len();
        assert!(unguarded > MAX_LOGICAL_BYTES, "the fixture is worst case: {unguarded} bytes before any shedding");

        // THE FIGURES THE GUARD STANDS ON: everything that is not per-profile memory, with the
        // envelope the helper seals this credential bundle into (either way), plus a full ledger.
        let without_memory = without(&public, &["track_memory", "subtitle_offsets", "library_sorts"]);
        let remainder = |seal| {
            let envelope = persistence::sealed_envelope_estimate(&canonical(&without_memory, 0), &protected, seal);
            let mut remainder = canonical(&without_memory, envelope);
            for n in 0..state::LEDGER_CAPACITY {
                remainder.operations.push(state::LedgerEntry {
                    id: state::Generation([255; 16]),
                    digest: "0".repeat(64),
                    result: state::Applied { revision: 1 + n as u64, epoch: 0, auth_generation: remainder.auth_generation },
                    verification: Some(state::ReceiptVerification::Public),
                });
            }
            (envelope, logical_size(&serde_json::to_value(&remainder).unwrap()))
        };
        let ((acl_envelope, acl_bytes), (keymanager_envelope, keymanager_bytes)) = (remainder(Seal::Acl), remainder(Seal::Keymanager));
        assert!(
            keymanager_bytes < MAX_LOGICAL_BYTES,
            "without per-profile memory the household is {keymanager_bytes} bytes with a keymanager envelope \
             ({keymanager_envelope}; {acl_bytes} with the ACL one, {acl_envelope}; credentials {}): it must fit one",
            protected.len()
        );
        assert!(acl_bytes < keymanager_bytes);
        // ... and not two: one envelope here is over a third of the cap
        assert!(2 * keymanager_envelope > MAX_LOGICAL_BYTES - (keymanager_bytes - keymanager_envelope), "{keymanager_envelope}");
        assert!(memory[0] <= BUDGET_BYTES);

        let mut store = store();
        // a sign-in lands first, the memory arrives afterwards
        commit(&mut store, &with_server_lists(household(USERS, SOURCES), USERS), SaveAuthority::FreshReauthentication);
        let signed_in = snapshot(&mut store).state;
        commit(&mut store, &worst_case, SaveAuthority::FreshReauthentication);

        let loaded = snapshot(&mut store);
        let document = logical_size(&serde_json::to_value(&loaded.state).unwrap());
        assert!(document <= MAX_LOGICAL_BYTES, "the stored document is {document} bytes");
        loaded.state.encode().expect("and it passes the platform's own validation");
        let preferences = &loaded.state.public.preferences;
        let kept = lists(preferences);
        let sheds = logged().matches("prefs: evicted").count();
        assert!(sheds >= 2 && logged().contains("track_memory entries to fit the 256 KiB document"), "{}", logged());
        assert_eq!(kept.0, 0, "track memory goes first and entirely: {kept:?}");
        assert!(kept.1 < 16 * SubtitleOffsets::CAP && (kept.1 == 0 || kept.2 == 16 * LibraryViews::CAP), "{kept:?}");
        // what is not memory is exactly what was written
        assert_eq!(count(preferences, "server_key_pins"), USERS);
        assert_eq!(count(preferences, "plaintext_consent"), USERS);
        assert_eq!(count(preferences, "last_library"), USERS * 3);
        assert_eq!(loaded.state.public.recents.as_array().map(Vec::len), Some(USERS));
        assert_eq!(loaded.state.public.pins.as_array().map(Vec::len), Some(USERS));
        let reopened = opened(&mut store);
        assert_eq!(reopened.profiles.len(), USERS, "the credentials are untouched");
        assert_eq!(reopened.home_users.len(), USERS);
        assert_eq!(reopened.account_token, worst_case.account_token);
        assert!(reopened.profiles.iter().all(|p| p.sources.len() == SOURCES));

        // THE LIMIT, stated: a STRICT refresh stages the new envelope beside the stored one, and two
        // of them do not fit at this size however little else the document holds. The guard sheds
        // everything it may and sends the write, and the platform refuses it as it always did.
        let mut bare = public.clone();
        let mut at_rest = signed_in.clone();
        at_rest.auth_envelope = Some("e".repeat(keymanager_envelope));
        persistence::fit_document(&at_rest, &mut bare, persistence::DocumentWrite::Staged, &protected);
        assert_eq!(lists(&bare.preferences), (0, 0, 0), "everything sheddable went");
        let staged = persistence::projected_size(&at_rest, &bare, persistence::DocumentWrite::Staged, &protected).unwrap();
        assert!(staged > MAX_LOGICAL_BYTES, "a staged write at this size is {staged} bytes with nothing sheddable left");

        // and an unrelated write lands
        let mut quality = worst_case.clone();
        quality.prefs.playback_quality = Some(PlaybackQuality::P1080High);
        commit(&mut store, &quality, SaveAuthority::PublicOnly);
        assert_eq!(opened(&mut store).prefs.playback_quality, Some(PlaybackQuality::P1080High));
        // and one written from the session the previous boot read back
        let mut again = reopened;
        again.prefs.playback_quality = Some(PlaybackQuality::Original);
        commit(&mut store, &again, SaveAuthority::PublicOnly);
        assert_eq!(opened(&mut store).prefs.playback_quality, Some(PlaybackQuality::Original));
        assert!(snapshot(&mut store).state.encode().is_ok());
    });
}

/// Servers each user's credentials hold in the household of `a_staged_write_is_guarded_where_two_envelopes_fit`:
/// the most whose credentials still leave room for a second envelope beside the first.
const STAGED_SOURCES: usize = 4;

/// **A staged write is guarded where the household leaves room for it.** The strict tier STAGES the
/// next credential write: the document then carries the old public half and envelope AND the new
/// ones in `pending_auth`. Sixteen users with `STAGED_SOURCES` servers each and full memory: the guard
/// sheds the memory the staged document cannot hold, and the document it builds is one the platform
/// accepts, and the document it becomes once promoted (the state the helper builds, step by step).
#[test]
fn a_staged_write_is_guarded_where_two_envelopes_fit() {
    use persistence::Seal;
    let mut household_with_memory = with_server_lists(household(USERS, STAGED_SOURCES), USERS);
    fill_memory(&mut household_with_memory, USERS, FULL);
    let (public, protected) = split_canonical(&household_with_memory).unwrap();

    let mut store = store();
    commit(&mut store, &with_server_lists(household(USERS, STAGED_SOURCES), USERS), SaveAuthority::FreshReauthentication);
    let mut at_rest = snapshot(&mut store).state;
    // the stored envelope, as the strict tier would have sealed it
    let envelope = persistence::sealed_envelope_estimate(&at_rest, &protected, Seal::Keymanager);
    at_rest.auth_envelope = Some("e".repeat(envelope));

    let mut staged_public = public.clone();
    let shed = persistence::fit_document(&at_rest, &mut staged_public, persistence::DocumentWrite::Staged, &protected);
    assert!(shed.iter().any(|(name, _)| *name == "track_memory"), "{shed:?}");
    assert!(
        persistence::projected_size(&at_rest, &staged_public, persistence::DocumentWrite::Staged, &protected).unwrap()
            <= MAX_LOGICAL_BYTES
    );
    let operation = state::Operation::new(
        state::Generation([255; 16]),
        at_rest.expected(),
        state::Mutation::ReplaceAuth {
            auth_generation: state::Generation([255; 16]),
            auth_envelope: "x".repeat(envelope),
            public: staged_public,
        },
    )
    .unwrap();
    at_rest.pending_auth = Some(state::PendingAuth { operation, plaintext_digest: state::digest_bytes(b"the plaintext") });
    at_rest.encode().expect("the platform accepts the worst-case staged document");
    let (promoted, _) = at_rest.pending_candidate().expect("with a candidate").expect("that is there");
    promoted.encode().expect("and the one it is promoted to");
}

/// Recents hold four terms a profile (`plx_data::search::recents::CAP`, which this crate cannot name).
fn data_recents_cap() -> usize {
    4
}

// ---- the first write: no record to measure -------------------------------------------------------

/// What the guard assumes for a first write onto an ABSENT record. A fresh sign-in may fall back to
/// the ACL-only tier, a legacy import always may, and a routine first write on newer firmware fails
/// closed, which is the strict tier and so a STAGED write; a public-only write has nothing to update
/// and is refused before it leaves.
#[test]
fn the_guard_assumes_the_shape_of_a_first_write() {
    use persistence::DocumentWrite::{Replace, Staged};
    use persistence::Seal::{Acl, Either};
    let (routine, fresh, public_only) =
        (SaveAuthority::Routine, SaveAuthority::FreshReauthentication, SaveAuthority::PublicOnly);
    let rows = [
        (true, routine, 4, Some(Replace(Acl))),
        (true, routine, 5, Some(Replace(Either))),
        (false, public_only, 4, None),
        (false, public_only, 5, None),
        (false, fresh, 4, Some(Replace(Acl))),
        (false, fresh, 5, Some(Replace(Either))),
        (false, routine, 4, Some(Replace(Acl))),
        (false, routine, 5, Some(Staged)),
    ];
    for (n, (migration, authority, major, expected)) in rows.into_iter().enumerate() {
        assert_eq!(persistence::missing_write(migration, authority, major), expected, "row {n}");
    }
}

/// **A first write is guarded too.** The memory that does not fit is not only what a running install
/// grew: a legacy file migrating in, or a sign-in on a session that already carries memory, meets an
/// ABSENT record, and the helper starts it from a fresh state. The same household as
/// `worst_case_household_document_fits` is over the cap before any shedding, and lands through each
/// kind of first write; without the guard the platform refuses it.
#[test]
fn a_first_write_onto_no_record_is_guarded_too() {
    plx_base::eventlog::with_private_log(|| {
        let mut worst_case = with_server_lists(household(USERS, SOURCES), USERS);
        fill_memory(&mut worst_case, USERS, FULL);
        let (public, protected) = split_canonical(&worst_case).unwrap();
        assert!(logical_size(&serde_json::to_value(&public).unwrap()) + protected.len() > MAX_LOGICAL_BYTES);

        for (case, migration, authority) in [
            ("sign-in", false, SaveAuthority::FreshReauthentication),
            ("routine first write", false, SaveAuthority::Routine),
            ("legacy migration", true, SaveAuthority::Routine),
        ] {
            let mut store = store();
            let mut seen = Vec::new();
            let result = {
                let mut transport = |request: Request| {
                    if let Request::Commit { mutation, .. } = &request {
                        seen.push(sent_as(mutation));
                    }
                    Ok(store.dispatch(request))
                };
                persistence::commit_session_with(&worst_case, migration, authority, 4, &mut transport)
            };
            assert!(matches!(result, persistence::CanonicalCommit::Durable { .. }), "{case}: {result:?}");
            assert_eq!(seen, [persistence::missing_write(migration, authority, 4)], "{case}: the write that was sent");
            let stored = snapshot(&mut store).state;
            stored.encode().expect("the stored document passes the platform's own validation");
            assert!(logical_size(&serde_json::to_value(&stored).unwrap()) <= MAX_LOGICAL_BYTES, "{case}");
            assert_eq!(lists(&stored.public.preferences).0, 0, "{case}: track memory goes first");
            assert_eq!(opened(&mut store).profiles.len(), USERS, "{case}: the credentials are untouched");
        }
        assert!(logged().matches("prefs: evicted").count() >= 3, "{}", logged());
    });
}

// ---- sign-out, and the previous build -----------------------------------------------------------

/// **Sign-out drops the track memory** through the production coordinator (`Mutation::ClearTenure`),
/// beside the keys that already went, and the offsets with it (SO): every per-profile memory list is
/// account-bound.
#[test]
fn sign_out_drops_the_track_memory() {
    let mut store = store();
    let mut session = household(2, 1);
    for n in 0..3 {
        assert!(put(&mut session.prefs.track_memory, &uuid(0), typical(n)));
    }
    session.set_subtitle_offset_for(&uuid(0), &machine(0), "rk-1", Some(1_500));
    let session = with_server_lists(session, 2);
    commit(&mut store, &session, SaveAuthority::FreshReauthentication);
    let before = stored_preferences(&mut store);
    for key in ["track_memory", "subtitle_offsets", "server_key_pins", "plaintext_consent"] {
        assert!(before.get(key).is_some(), "control: `{key}` is stored before sign-out");
    }

    let mut transport = |request: Request| Ok(store.dispatch(request));
    assert!(matches!(persistence::commit_clear_with(&mut transport), persistence::CanonicalCommit::Durable { .. }));
    let after = stored_preferences(&mut store);
    for key in ["track_memory", "subtitle_offsets", "server_key_pins", "plaintext_consent"] {
        assert!(after.get(key).is_none(), "`{key}` does not survive sign-out: {after}");
    }
}

/// **A0 item 16, the real shape.** A0 feeds tier 1 a track memory in shapes nothing writes. This is
/// the shape this build writes (an array of profile records, each with an account stamp and its
/// entries), through the platform codec and the PREVIOUS build's frozen parse: credentials and today's
/// keys intact, the key opaque, and the previous build's own rewrite of it byte-identical; this
/// build then reads back exactly what it wrote.
#[test]
fn tier1_the_real_track_memory_shape_rides_through_the_previous_build() {
    let mut session = household(2, 1);
    for n in 0..3 {
        assert!(put(&mut session.prefs.track_memory, &uuid(0), typical(n)));
    }
    assert!(put(&mut session.prefs.track_memory, &uuid(1), worst(7)));
    session.set_subtitle_offset_for(&uuid(0), &machine(0), "rk-1", Some(1_500));
    session.prefs.skip_interval = SkipInterval::Seconds30;
    let (public, protected) = split_canonical(&session).unwrap();
    let real = public.preferences["track_memory"].clone();
    assert_eq!(real.as_array().map(Vec::len), Some(2), "{real}");
    assert_eq!(real[0]["account"], json!(STAMP));
    assert_eq!(real[0]["items"][0]["machine_id"].as_str().map(str::len), Some(40));

    let bytes = canonical(&public, 2_000).encode().expect("encode validates");
    let decoded = state::CanonicalState::decode(&bytes, state::Flavor::Stable).expect("decode validates");
    assert_eq!(decoded.public.preferences, public.preferences, "the codec keeps it byte for byte");

    // the previous build
    let old = v0::CanonicalSessionPreferences::parse(&decoded.public.preferences);
    assert_eq!(old.extensions.keys().collect::<Vec<_>>(), ["track_memory"], "exactly the new key is opaque");
    assert_eq!(old.extensions.get("track_memory"), Some(&real));
    assert_eq!(old.skip_interval, SkipInterval::Seconds30);
    assert_eq!(old.subtitle_offsets.len(), 1);
    assert_eq!(old.subtitle_offsets[0].items[0].offset_ms, 1_500);
    assert_eq!(serde_json::to_value(&old).unwrap()["track_memory"], real, "its own rewrite keeps the value");

    // this build
    let joined = super::super::join_canonical(&decoded.public, &protected).expect("the credentials still open");
    assert_eq!(joined.prefs.track_memory, session.prefs.track_memory);
    assert_eq!(joined.account_token, session.account_token);
    assert_eq!(joined.profiles.len(), 2);
    assert_eq!(serde_json::to_value(&joined.server).unwrap(), serde_json::to_value(&session.server).unwrap());

    // the legacy file: the previous build's frozen `Session` carries it opaque, and keeps the credentials
    let file = serde_json::to_value(&session).unwrap();
    assert_eq!(file["track_memory"], real);
    let old: v0::Session = serde_json::from_value(file.clone()).expect("the previous build still reads the file");
    assert_eq!(old.extensions.0.get("track_memory"), Some(&real));
    assert_eq!(old.account_token, session.account_token);
    assert_eq!(serde_json::to_value(&old).unwrap()["track_memory"], real);
    let back: Session = serde_json::from_value(file).unwrap();
    assert_eq!(back.prefs.track_memory, session.prefs.track_memory);
}
