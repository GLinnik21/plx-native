//! S4 host capture value, coherence and allocation contracts; no registry or network required.
use super::*;
use crate::plex::{Origin, probe::Outcome};

fn client(slot: u16) -> &'static Client {
    Box::leak(Box::new(Client::new(ServerId::from_raw(slot), "machine-with-capacity",
        Origin::http("127.0.0.1", 9), "token", "capture-test")))
}

#[test]
fn roster_capture_keeps_all_slots_and_never_guesses_current() {
    let a = client(51);
    let empty = ServerId::from_raw(52);
    let missing = ServerId::from_raw(53);
    let mut keys = ClientKeys::default();
    keys.refill_roster(missing, 7, 9, [a.id(), empty], |sid| (sid == a.id()).then_some(a), |_| None, |_| None);
    assert_eq!(keys.slots().collect::<Vec<_>>(), [a.id(), empty]);
    assert_eq!(keys.iter().count(), 1);
    assert_eq!(keys.key(empty), None);
    assert_eq!(keys.current(), missing);
    assert_eq!((keys.roster_gen(), keys.facts_gen()), (7, 9));
    assert_eq!(ClientKeys::of([(a.id(), a)]).current(), ServerId::UNSET);
    assert_eq!(ClientKeys::from_keys([(a.id(), ClientKey::of(a))]).current(), ServerId::UNSET);
}

#[test]
fn refill_rechecks_eligibility_without_any_generation_move() {
    let a = client(54);
    let mut keys = ClientKeys::default();
    for eligible in [true, false, true, false] {
        keys.refill_roster(a.id(), 1, 1, [a.id()], |_| eligible.then_some(a), |_| None, |_| None);
        assert_eq!(keys.key(a.id()).is_some(), eligible);
        assert_eq!(keys.slots().count(), 1);
        assert_eq!(keys.facts(a.id()).unwrap().source.is_some(), eligible);
    }
}

#[test]
fn probe_is_observed_before_tier_and_repoint_or_retoken_rejects_the_pair() {
    let a = client(55);
    let b = client(55);
    let mut keys = ClientKeys::default();
    let tier = crate::plex::decode_link(1).expect("known tier encoding");
    keys.refill_roster(a.id(), 1, 1, [a.id()], |_| Some(a), |_| None, |_| {
        a.set_link(tier);
        Some(Outcome::Reachable)
    });
    assert_eq!(keys.facts(a.id()).unwrap().source, Some((Some(Outcome::Reachable), Some(tier))));
    let mut reads = 0;
    keys.refill_roster(a.id(), 1, 1, [a.id()], |_| {
        reads += 1;
        Some(if reads == 1 { a } else { b })
    }, |_| None, |_| Some(Outcome::Reachable));
    assert_eq!(keys.key(a.id()), None);
    assert_eq!(keys.facts(a.id()).unwrap().source, None);
    keys.refill_roster(a.id(), 1, 1, [a.id()], |_| Some(a), |_| None, |_| {
        a.set_token("changed-in-probe");
        Some(Outcome::Reachable)
    });
    assert_eq!(keys.key(a.id()), None);
    assert_eq!(keys.facts(a.id()).unwrap().source, None);
}

#[test]
fn identity_moves_cover_token_grant_and_instance_and_old_values_stay_immutable() {
    let a = client(56);
    let b = client(56);
    let mut keys = ClientKeys::default();
    keys.refill_roster(a.id(), 3, 4, [a.id()], |_| Some(a), |_| None, |_| None);
    let old = keys.clone();
    let original = old.key(a.id()).unwrap();
    a.set_token("refresh");
    keys.refill_roster(a.id(), 3, 4, [a.id()], |_| Some(a), |_| None, |_| None);
    let refresh = keys.key(a.id()).unwrap();
    assert_ne!(original.token, refresh.token);
    assert_eq!(original.grant, refresh.grant);
    a.set_token("");
    a.set_token("new-profile");
    keys.refill_roster(a.id(), 3, 4, [a.id()], |_| Some(a), |_| None, |_| None);
    assert_ne!(refresh.grant, keys.key(a.id()).unwrap().grant);
    keys.refill_roster(a.id(), 3, 4, [a.id()], |_| Some(b), |_| None, |_| None);
    assert_ne!(original.instance, keys.key(a.id()).unwrap().instance);
    assert_eq!(old.key(a.id()), Some(original));
    assert_eq!(old.facts(a.id()).unwrap().machine_id, "machine-with-capacity");
}

#[test]
fn unchanged_refill_reuses_buffers_machine_strings_and_immutable_description() {
    let a = client(57);
    let facts = Box::leak(Box::new(crate::plex::ServerFacts {
        name: "Server".into(), handle: "Household".into(), owned: false, home: true, owner_id: 42,
    }));
    let mut keys = ClientKeys::default();
    keys.refill_roster(a.id(), 2, 5, [a.id()], |_| Some(a), |_| Some(facts), |_| None);
    let key_buffer = keys.0.as_ptr();
    let metadata_buffer = keys.1.as_ptr();
    let string = keys.facts(a.id()).unwrap().machine_id.as_ptr();
    let capacities = (keys.0.capacity(), keys.1.capacity(), keys.facts(a.id()).unwrap().machine_id.capacity());
    let old = keys.clone();
    for _ in 0..16 {
        keys.refill_roster(a.id(), 2, 5, [a.id()], |_| Some(a), |_| Some(facts), |_| None);
        assert_eq!(keys, old);
        assert_eq!(keys.0.as_ptr(), key_buffer);
        assert_eq!(keys.1.as_ptr(), metadata_buffer);
        assert_eq!(keys.facts(a.id()).unwrap().machine_id.as_ptr(), string);
        assert_eq!((keys.0.capacity(), keys.1.capacity(), keys.facts(a.id()).unwrap().machine_id.capacity()), capacities);
        assert!(std::ptr::eq(keys.facts(a.id()).unwrap().described.unwrap(), facts));
    }
}

#[test]
fn losing_eligibility_during_capture_rejects_the_key_even_when_identity_does_not_move() {
    let a = client(58);
    let mut reads = 0;
    let mut keys = ClientKeys::default();
    keys.refill_roster(a.id(), 1, 1, [a.id()], |_| {
        reads += 1;
        (reads == 1).then_some(a)
    }, |_| None, |_| Some(Outcome::Reachable));
    assert_eq!(keys.key(a.id()), None);
    assert_eq!(keys.facts(a.id()).unwrap().source, None);
}

#[test]
fn capture_remains_a_cloneable_send_sync_value_not_a_client_handle() {
    fn value<T: Clone + Eq + Send + Sync>() {}
    value::<ClientKeys>();
    assert_eq!(ClientKeys::default().iter().count(), 0);
}

#[test]
fn stream_policy_is_fresh_each_fill_fail_closed_and_reuses_s4_buffers() {
    let a = client(61);
    let mut keys = ClientKeys::default();
    assert!(!ClientKeys::of([(a.id(), a)]).stream_allowed(a.id()));
    keys.refill_roster(a.id(), 1, 1, [a.id()], |_| Some(a), |_| None, |_| None);
    assert!(!keys.stream_allowed(a.id()), "compatibility capture cannot authorize streaming");
    let buffers = (keys.0.as_ptr(), keys.1.as_ptr());
    let string = keys.facts(a.id()).unwrap().machine_id.as_ptr();
    let mut previous = keys.clone();
    for allowed in [true, false, true, false] {
        keys.refill_roster_with_stream_policy(a.id(), 1, 1, [a.id()], |_| Some(a),
            |_| None, |_| None, |_| allowed);
        assert_eq!(keys.stream_allowed(a.id()), allowed);
        assert_eq!(keys.facts(a.id()).unwrap().stream_allowed, allowed);
        assert_eq!((keys.0.as_ptr(), keys.1.as_ptr()), buffers);
        assert_eq!(keys.facts(a.id()).unwrap().machine_id.as_ptr(), string);
        assert_ne!(keys, previous, "stream policy participates in equality without generation moves");
        previous = keys.clone();
    }
    assert!(!keys.stream_allowed(ServerId::UNSET));
}

#[test]
fn stream_policy_runs_after_probe_and_incoherent_capture_clears_permission() {
    let a = client(62);
    let stage = std::cell::Cell::new(0);
    let mut reads = 0;
    let mut keys = ClientKeys::default();
    keys.refill_roster_with_stream_policy(a.id(), 1, 1, [a.id()], |_| {
        reads += 1;
        (reads == 1).then_some(a)
    }, |_| None, |_| { stage.set(1); None }, |_| {
        assert_eq!(stage.get(), 1);
        stage.set(2);
        true
    });
    assert_eq!(stage.get(), 2);
    assert_eq!(keys.key(a.id()), None);
    assert!(!keys.stream_allowed(a.id()));
    assert!(!keys.facts(a.id()).unwrap().stream_allowed);
    // A credential identity move inside the policy fence rejects the same row.
    keys.refill_roster_with_stream_policy(a.id(), 1, 1, [a.id()], |_| Some(a),
        |_| None, |_| None, |c| { c.set_token("changed-in-policy"); true });
    assert_eq!(keys.key(a.id()), None);
    assert!(!keys.stream_allowed(a.id()));
}
