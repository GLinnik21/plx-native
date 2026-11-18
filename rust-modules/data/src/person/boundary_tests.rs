use super::*;
fn facts(keys: &ClientKeys) -> PersonInput<'_> {
    PersonInput { keys, session: None, visible_generation: 0, controlled: true,
                profile_scope: 0, owner_epoch: 0, profile_generation: 0, tenure: 0, profile_key: "",
        profile_refused: false, credits_refused: false, dev_bio: None, dev_credits: None }
}
fn profile_job(state: &mut PersonState, adapter: &Arc<PersonAdapter>) -> (Job<PersonReq>, PersonPort) {
    let keys = ClientKeys::default();
    if state.current().is_none() { open(state, adapter, ServerId::UNSET, "7", "plex://person/7", "Actor", "", &facts(&keys)); }
    state.pump(adapter, &facts(&keys));
    std::mem::take(&mut state.jobs).into_iter().find_map(|j| match j {
        crate::stores::StoreJob::Person(job, port) if job.req.slot == F_PROFILE => Some((job, port)),
        _ => None,
    }).expect("profile job")
}
#[test]
fn launch_refusal_is_an_addressed_event_not_a_claim_release() {
    let mut state = PersonState::default();
    let adapter = Arc::new(PersonAdapter::default());
    let (job, port) = profile_job(&mut state, &adapter);
    port.refused(job.addr);
    assert!(adapter.mailbox(F_PROFILE).busy(), "only the owner may consume the terminal and release its claim");
}
#[test]
fn wrong_request_address_cannot_refuse_the_live_job() {
    let mut state = PersonState::default();
    let adapter = Arc::new(PersonAdapter::default());
    let (job, port) = profile_job(&mut state, &adapter);
    port.refused(person_addr(job.addr.req.0.wrapping_add(1)));
    assert!(adapter.mailbox(F_PROFILE).busy(), "a different request cannot release this claim");
}

#[test]
fn explicit_scope_edges_retire_claims_and_evidence_before_any_landing_even_controlled() {
    let sid = ServerId::from_raw(71);
    let keys = ClientKeys::from_keys([(sid, ClientKey { instance: 1, token: 2, grant: 3 })]);
    for controlled in [false, true] {
        for edge in 0..5 {
            let session = Arc::new(plx_plex::plex::session::Session { client_id: "same".into(), account_token: "same".into(), ..Default::default() });
            let mut input = facts(&keys);
            input.controlled = controlled;
            input.session = Some(&session);
            let mut state = PersonState::default();
            let adapter = Arc::new(PersonAdapter::default());
            open(&mut state, &adapter, sid, "7", "plex://person/7", "Actor", "", &input);
            state.pump(&adapter, &input);
            let old_generation = state.generation;
            let jobs = std::mem::take(&mut state.jobs);
            let old_ports: Vec<_> = jobs.into_iter().map(|j| match j {
                crate::stores::StoreJob::Person(job, port) => (job, port),
                _ => unreachable!(),
            }).collect();
            apply_landing(&mut state, fx(sid, K_MEDIA).unwrap(), Landing::Media(Some(MediaLanding {
                shelves: [Shelf { items: vec![PmsMovie { sid, rk: "private".into(), ..Default::default() }], total: 1, roles: vec![] }, Shelf::default()],
                matches: vec![("private-guid".into(), "private".into())],
            })));
            seed_dev_credits(&mut state, Some("3"));
            let person = state.current.as_mut().unwrap();
            person.bio = "old private biography".into();
            person.roles.push("old department".into());
            person.born = "old date".into();
            person.profiled = true;
            person.profile_tried = true;
            match edge {
                0 => input.profile_scope += 1,
                1 => input.owner_epoch += 1,
                2 => input.profile_generation += 1,
                3 => input.tenure += 1,
                _ => input.profile_key = "different-tenant",
            }
            // Queue the old result BEFORE the pass: scope must win before the first take.
            for (job, port) in &old_ports {
                let out = if job.req.slot == F_PROFILE {
                    PersonAnswer(Landing::Profile(Some(plx_plex::plex::discover::PersonProfile {
                        summary: "must not land".into(), ..Default::default()
                    })))
                } else if job.req.slot == F_CREDITS { PersonAnswer(Landing::Credits(None)) }
                else { PersonAnswer(Landing::Media(None)) };
                port.done(job.addr, out);
            }
            assert!(state.pump(&adapter, &input));
            assert!(state.generation > old_generation);
            let person = state.current().unwrap();
            assert_eq!(person.name, "Actor", "the mounted header may be held");
            assert!(person.bio.is_empty() && person.roles.is_empty() && person.born.is_empty());
            assert!(person.credits.is_empty() && !person.credited && !person.profile_tried);
            assert!(person.shelves.iter().all(|s| s.items.is_empty()));
            assert!(person.srcs.iter().all(|s| s.matches.is_empty()));
            assert!(state.stamps.values().all(|stamp| *stamp == state.ledger.stamp()));
            assert!(state.jobs.iter().all(|j| matches!(j, crate::stores::StoreJob::Person(job, _) if job.req.generation == state.generation)));
            for (job, port) in old_ports { port.refused(job.addr); }
            assert!(!state.land_launch_refusals(&adapter));
            assert!(adapter.mailbox(F_PROFILE).busy(), "old scope cannot retire the replacement");
        }
    }
}

#[test]
fn same_generation_retry_rejects_old_refusal_success_address_and_key() {
    let keys = ClientKeys::default();
    let mut state = PersonState::default();
    let adapter = Arc::new(PersonAdapter::default());
    let (old, old_port) = profile_job(&mut state, &adapter);
    old_port.done(old.addr, PersonAnswer(Landing::Profile(None)));
    state.pump(&adapter, &facts(&keys));
    state.retry_cd[F_PROFILE] = 0;
    let (new, new_port) = profile_job(&mut state, &adapter);
    assert_eq!(old.req.generation, new.req.generation);
    assert_ne!(old.addr, new.addr);
    old_port.refused(old.addr);
    old_port.done(old.addr, PersonAnswer(Landing::Profile(None)));
    new_port.refused(old.addr);
    assert!(!state.land_launch_refusals(&adapter));
    assert!(adapter.mailbox(F_PROFILE).busy());
    assert!(!adapter.mailbox(F_PROFILE).has_mail());
    let mut wrong = new.req.clone();
    wrong.guid.push_str("-wrong");
    PersonPort::to(&adapter, new.addr, &wrong).refused(new.addr);
    assert!(!state.land_launch_refusals(&adapter));
    assert!(adapter.mailbox(F_PROFILE).busy());
    new_port.refused(new.addr);
    new_port.refused(new.addr);
    assert!(state.land_launch_refusals(&adapter));
    assert!(!state.land_launch_refusals(&adapter), "one terminal only");
    assert!(!adapter.mailbox(F_PROFILE).busy());
    assert_eq!(state.retry_cd[F_PROFILE], RETRY_FRAMES);
    assert!(!state.current().unwrap().profiled, "temporary refusal cannot permanently settle Discover");
    state.jobs.clear();
    state.pump(&adapter, &facts(&keys));
    assert!(state.jobs.is_empty(), "no same-drain retry or next-frame launch spin");
}

#[test]
fn complete_request_including_client_key_must_match_the_claim() {
    let sid = ServerId::from_raw(71);
    let key = ClientKey { instance: 1, token: 2, grant: 3 };
    let keys = ClientKeys::from_keys([(sid, key)]);
    let mut state = PersonState::default();
    let adapter = Arc::new(PersonAdapter::default());
    open(&mut state, &adapter, sid, "7", "plex://person/7", "Actor", "", &facts(&keys));
    state.pump(&adapter, &facts(&keys));
    let (job, port) = std::mem::take(&mut state.jobs).into_iter().find_map(|j| match j {
        crate::stores::StoreJob::Person(job, port) if job.req.server().is_some() => Some((job, port)), _ => None,
    }).unwrap();
    let mut wrong = job.req.clone();
    if let PersonWork::Local { key, .. } = &mut wrong.work { key.grant += 1; }
    let stale = PersonPort::to(&adapter, job.addr, &wrong);
    stale.refused(job.addr);
    stale.done(job.addr, PersonAnswer(Landing::Media(None)));
    assert!(!state.land_launch_refusals(&adapter));
    assert!(adapter.mailbox(job.req.slot).busy());
    assert!(!adapter.mailbox(job.req.slot).has_mail());
    port.refused(job.addr);
    assert!(state.land_launch_refusals(&adapter));
}
