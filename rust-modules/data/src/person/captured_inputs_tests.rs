use super::*;

const ORIGIN: ServerId = ServerId::from_raw(71);
const SHARE: ServerId = ServerId::from_raw(72);

fn input<'a>(keys: &'a ClientKeys, session: Option<&'a Arc<plx_plex::plex::session::Session>>) -> PersonInput<'a> {
    PersonInput { keys, session, visible_generation: 9, controlled: false,
                profile_scope: 0, owner_epoch: 0, profile_generation: 0, tenure: 0, profile_key: "",
        profile_refused: false, credits_refused: false, dev_bio: None, dev_credits: None }
}

fn open_page(state: &mut PersonState, adapter: &Arc<PersonAdapter>, input: &PersonInput<'_>) {
    open(state, adapter, ORIGIN, "7", "plex://person/captured", "Actor", "", input);
}

fn roster(slots: &[ServerId], generation: u32) -> ClientKeys {
    let mut keys = ClientKeys::default();
    keys.refill_roster(ServerId::UNSET, generation, 0, slots.iter().copied(), |_| None, |_| None, |_| None);
    keys
}

#[test]
fn captured_roster_and_settled_identity_override_conflicting_process_state() {
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("person-captured-vs-ambient");
    plx_plex::plex::reset_servers_for_test();
    let ambient = plx_plex::plex::register_for_test("ambient-person", "127.0.0.1", 1, "a", "cid");
    let keys = roster(&[SHARE, SHARE], 400);
    let session = Arc::new(plx_plex::plex::session::Session {
        client_id: "captured-client".into(), account_token: "captured-token".into(), ..Default::default()
    });
    let facts = input(&keys, Some(&session));
    let mut state = PersonState::default();
    let adapter = Arc::new(PersonAdapter::default());
    open_page(&mut state, &adapter, &facts);
    assert_eq!(state.current().unwrap().srcs.iter().map(|s| s.sid).collect::<Vec<_>>(), [SHARE, ORIGIN]);
    assert!(!state.current().unwrap().srcs.iter().any(|s| s.sid == ambient));
    state.pump(&adapter, &facts);
    let jobs = std::mem::take(&mut state.jobs);
    assert_eq!(jobs.len(), 2, "roster-only slots cannot emit local jobs");
    for job in jobs {
        let crate::stores::StoreJob::Person(job, _) = job else { panic!("wrong store") };
        match job.req.work {
            PersonWork::Profile(Identity::Settled(s)) | PersonWork::Credits(Identity::Settled(s)) => assert!(Arc::ptr_eq(&s, &session)),
            _ => panic!("captured identity was lost"),
        }
    }
}

#[test]
fn origin_is_appended_once_even_without_a_registered_client() {
    for slots in [vec![], vec![SHARE], vec![ORIGIN, SHARE, ORIGIN]] {
        let keys = roster(&slots, 1);
        let srcs = sources(ORIGIN, "7", "Actor", &keys);
        assert_eq!(srcs.iter().filter(|s| s.sid == ORIGIN).count(), 1);
        assert_eq!(srcs.len(), if slots.is_empty() { 1 } else { 2 });
        assert!(srcs.iter().find(|s| s.sid == ORIGIN).unwrap().resolved);
    }
}

#[test]
fn unchanged_capture_keeps_generation_publication_and_identity_string_allocations() {
    let keys = roster(&[SHARE], 33);
    let session = Arc::new(plx_plex::plex::session::Session { client_id: "client".into(), account_token: "token".into(), ..Default::default() });
    let facts = input(&keys, Some(&session));
    let mut state = PersonState::default();
    let adapter = Arc::new(PersonAdapter::default());
    open_page(&mut state, &adapter, &facts);
    state.pump(&adapter, &facts);
    let generation = state.generation;
    let revision = state.view().revision();
    let pointers = (state.session_identity.0.as_ptr(), state.session_identity.1.as_ptr(), state.current().unwrap().srcs.as_ptr());
    for _ in 0..8 { assert!(!state.pump(&adapter, &facts)); }
    assert_eq!((state.generation, state.view().revision()), (generation, revision));
    assert_eq!((state.session_identity.0.as_ptr(), state.session_identity.1.as_ptr(), state.current().unwrap().srcs.as_ptr()), pointers);
}

#[test]
fn roster_only_changes_rebuild_but_a_live_token_refresh_does_not() {
    let key = ClientKey { instance: 10, token: 1, grant: 3 };
    let mut keys = ClientKeys::from_keys([(ORIGIN, key)]);
    let mut state = PersonState::default();
    let adapter = Arc::new(PersonAdapter::default());
    open_page(&mut state, &adapter, &input(&keys, None));
    let generation = state.generation;
    keys.refill([(ORIGIN, ClientKey { token: 2, ..key })]);
    assert!(!sync_roster(&mut state, &adapter, &input(&keys, None)));
    assert_eq!(state.generation, generation);
    keys = roster(&[ORIGIN, SHARE], 2);
    assert!(sync_roster(&mut state, &adapter, &input(&keys, None)));
    assert_eq!(state.current().unwrap().srcs.len(), 2, "a no-client registered share is not dropped");
}

#[test]
fn suspended_session_defers_global_jobs_and_recovery_uses_supplied_identity() {
    let keys = roster(&[], 0);
    let mut state = PersonState::default();
    let adapter = Arc::new(PersonAdapter::default());
    open_page(&mut state, &adapter, &input(&keys, None));
    state.pump(&adapter, &input(&keys, None));
    assert!(state.jobs.is_empty());
    let session = Arc::new(plx_plex::plex::session::Session { client_id: "recovered".into(), ..Default::default() });
    state.pump(&adapter, &input(&keys, Some(&session)));
    assert_eq!(state.jobs.len(), 2);
}

#[test]
fn controlled_capture_ignores_session_edges_and_uses_supplied_dev_seed() {
    let keys = roster(&[], 0);
    let mut facts = input(&keys, None);
    facts.controlled = true;
    facts.dev_bio = Some("must not seed controlled biography");
    facts.dev_credits = Some("3");
    let mut state = PersonState::default();
    let adapter = Arc::new(PersonAdapter::default());
    open_page(&mut state, &adapter, &facts);
    let generation = state.generation;
    facts.visible_generation += 1;
    state.pump(&adapter, &facts);
    assert_eq!(state.generation, generation);
    assert!(state.current().unwrap().bio.is_empty());
    assert!(state.current().unwrap().credited);
    assert_eq!(state.current().unwrap().credits[0].credits.len(), 3);
    assert!(!state.jobs.is_empty());
    assert!(state.jobs.iter().all(|j| matches!(j, crate::stores::StoreJob::Person(Job { req: PersonReq { work: PersonWork::Profile(Identity::Controlled) | PersonWork::Credits(Identity::Controlled), .. }, .. }, _))));
}

#[test]
fn changed_seed_rebuilds_credits_without_a_holdings_change() {
    let keys = roster(&[], 0);
    let mut facts = input(&keys, None);
    facts.controlled = true;
    facts.dev_credits = Some("3");
    let mut state = PersonState::default();
    let adapter = Arc::new(PersonAdapter::default());
    open_page(&mut state, &adapter, &facts);
    state.pump(&adapter, &facts);
    assert_eq!(state.current().unwrap().credits[0].credits.len(), 3);
    facts.dev_credits = Some("5");
    assert!(state.pump(&adapter, &facts));
    assert_eq!(state.current().unwrap().credits[0].credits.len(), 5);
    let rows = state.current().unwrap().credits.as_ptr();
    assert!(!state.pump(&adapter, &facts));
    assert_eq!(state.current().unwrap().credits.as_ptr(), rows);
}

#[test]
fn changed_identity_at_the_same_generation_retires_its_claim() {
    let keys = roster(&[], 0);
    let a = Arc::new(plx_plex::plex::session::Session { client_id: "a".into(), ..Default::default() });
    let b = Arc::new(plx_plex::plex::session::Session { client_id: "b".into(), ..Default::default() });
    let mut state = PersonState::default();
    let adapter = Arc::new(PersonAdapter::default());
    open_page(&mut state, &adapter, &input(&keys, Some(&a)));
    state.pump(&adapter, &input(&keys, Some(&a)));
    let old = state.generation;
    state.pump(&adapter, &input(&keys, Some(&b)));
    assert!(state.generation > old);
}

#[test]
fn captured_refusal_settles_in_this_turn_without_a_job_or_retry() {
    let keys = roster(&[], 0);
    let session = Arc::new(plx_plex::plex::session::Session { client_id: "client".into(), ..Default::default() });
    let mut facts = input(&keys, Some(&session));
    facts.profile_refused = true;
    facts.credits_refused = true;
    let mut state = PersonState::default();
    let adapter = Arc::new(PersonAdapter::default());
    open_page(&mut state, &adapter, &facts);
    assert!(state.pump(&adapter, &facts));
    let person = state.current().unwrap();
    assert!(person.profiled && person.profile_tried && person.credited);
    assert!(state.jobs.is_empty());
    assert!(!adapter.mailbox(F_PROFILE).busy());
    assert!(!state.pump(&adapter, &facts));
}

#[test]
fn profile_edge_retires_old_answer_without_releasing_the_new_claim() {
    let keys = roster(&[], 0);
    let a = Arc::new(plx_plex::plex::session::Session { client_id: "a".into(), account_token: "a".into(), ..Default::default() });
    let b = Arc::new(plx_plex::plex::session::Session { client_id: "b".into(), account_token: "b".into(), ..Default::default() });
    let mut state = PersonState::default();
    let adapter = Arc::new(PersonAdapter::default());
    open_page(&mut state, &adapter, &input(&keys, Some(&a)));
    state.pump(&adapter, &input(&keys, Some(&a)));
    let old = state.generation;
    let mut next = input(&keys, Some(&b));
    next.visible_generation += 1;
    state.pump(&adapter, &next);
    assert_ne!(old, state.generation);
    assert!(adapter.mailbox(F_PROFILE).busy());
    adapter.land(F_PROFILE, old, Landing::Profile(None));
    state.pump(&adapter, &next);
    assert!(adapter.mailbox(F_PROFILE).busy(), "an orphan cannot release its replacement's claim");
    assert!(!state.current().unwrap().profile_tried);
}
#[test]
fn a_retained_publication_cannot_be_changed_by_another_owners_capture_or_pump() {
    let keys = roster(&[], 0);
    let facts = input(&keys, None);
    let mut first = PersonState::default();
    let first_adapter = Arc::new(PersonAdapter::default());
    open_page(&mut first, &first_adapter, &facts);
    let retained = first.view();
    let revision = retained.revision();
    let person = retained.current().unwrap();
    let identity = (person as *const Person, person.key.as_str(), person.name.as_str());
    let mut second = PersonState::default();
    let second_adapter = Arc::new(PersonAdapter::default());
    open_page(&mut second, &second_adapter, &facts);
    second.pump(&second_adapter, &facts);
    close(&mut second, &second_adapter);
    assert_eq!(retained.revision(), revision);
    let held = retained.current().unwrap();
    assert_eq!((held as *const Person, held.key.as_str(), held.name.as_str()), identity);
}

#[test]
fn a_new_grant_rebuilds_while_token_only_refresh_keeps_loaded_shelves() {
    let original = ClientKey { instance: 5, token: 7, grant: 11 };
    let mut keys = ClientKeys::from_keys([(ORIGIN, original)]);
    let mut state = PersonState::default();
    let adapter = Arc::new(PersonAdapter::default());
    open_page(&mut state, &adapter, &input(&keys, None));
    let generation = state.generation;
    apply_landing(&mut state, fx(ORIGIN, K_MEDIA).unwrap(), Landing::Media(Some(MediaLanding {
        shelves: [Shelf { items: vec![PmsMovie { sid: ORIGIN, rk: "movie".into(), ..Default::default() }], total: 1, roles: Vec::new() }, Shelf::default()], matches: Vec::new(),
    })));
    keys.refill([(ORIGIN, ClientKey { token: 8, ..original })]);
    assert!(!sync_roster(&mut state, &adapter, &input(&keys, None)));
    assert_eq!(state.current().unwrap().shelf(0).len(), 1);
    assert_eq!(state.generation, generation);
    keys.refill([(ORIGIN, ClientKey { token: 9, grant: 12, ..original })]);
    assert!(sync_roster(&mut state, &adapter, &input(&keys, None)));
    assert!(state.current().unwrap().shelf(0).is_empty());
    assert!(state.generation > generation);
}
