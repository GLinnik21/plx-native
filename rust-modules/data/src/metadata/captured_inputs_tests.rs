use super::*;
use crate::captured_store_fixture::{key, roster};
use plx_machine::job::Port;
use std::sync::atomic::Ordering;
fn copy(sid: plx_plex::plex::ServerId, owner: &str) -> AltCopy {
    AltCopy { sid, library: "Library".into(), owner: Some(owner.into()), rk: "4".into(),
        dur_ms: 1000, res: "1080".into(), width: 1920, height: 1080 }
}
#[test]
fn captured_alt_scope_regrades_facts_without_retiring_work_and_fences_same_generation_key_moves() {
    let _serial = plx_base::testlock::serial();
    let a = roster(&[(61, Some(key(1)), "captured"), (101, None, "unkeyed")], 3, 4);
    let sid = plx_plex::plex::ServerId::from_raw(61);
    let mut state = MetadataState::default();
    state.current = Some(Detail { sid, rk: "4".into(), ..Default::default() });
    let adapter = std::sync::Arc::new(MetadataAdapter::default());
    request_alt_sources(&mut state, &adapter, sid, "4", "plex://movie/script", &a);
    let crate::stores::StoreJob::MetaAltSources(job, port) = state.jobs.pop().unwrap() else { panic!("alt job") };
    assert_eq!(job.req.others.iter().map(|s| s.raw()).collect::<Vec<_>>(), [61, 101], "all registered slots, not Browse's partial projection");
    assert_eq!(job.req.keys.iter().count(), 1, "eligibility is still a full-key lookup");
    let generation = adapter.alt_gen.load(Ordering::SeqCst);
    let b = roster(&[(61, Some(key(1)), "corrected"), (101, None, "unkeyed")], 3, 4);
    let gate = plx_machine::landgate::Gate::default();
    pump_alt_sources_with_library(&mut state, &adapter, &b, true).finish(None, &gate);
    assert_eq!(adapter.alt_gen.load(Ordering::SeqCst), generation, "facts-only regrading does not discard a valid resolve");
    assert_eq!(state.current.as_ref().unwrap().source(), "corrected");
    port.done(job.addr, vec![copy(sid, "stale worker credit")]);
    assert!(pump_alt_sources_with_library(&mut state, &adapter, &b, true).finish(None, &gate));
    assert_eq!(state.alt.copies[0].owner.as_deref(), Some("corrected"), "landing regrades even after the facts epoch was consumed");
    request_alt_sources(&mut state, &adapter, sid, "4", "plex://movie/script", &b);
    let crate::stores::StoreJob::MetaAltSources(old, old_port) = state.jobs.pop().unwrap() else { panic!("alt job") };
    let c = roster(&[(61, Some(key(2)), "corrected"), (101, None, "unkeyed")], 3, 4);
    pump_alt_sources_with_library(&mut state, &adapter, &c, true).finish(None, &gate);
    assert_ne!(adapter.alt_gen.load(Ordering::SeqCst), old.addr.req.0);
    old_port.done(old.addr, vec![copy(sid, "late")]);
    assert!(adapter.alt_slot.lock().unwrap_or_else(|e| e.into_inner()).is_none(), "the old full key cannot fill the replacement mailbox");
    request_alt_sources(&mut state, &adapter, sid, "4", "plex://movie/script", &c);
    let crate::stores::StoreJob::MetaAltSources(refused, refused_port) = state.jobs.pop().unwrap() else { panic!("alt job") };
    refused_port.refused(refused.addr);
    pump_alt_sources_with_library(&mut state, &adapter, &c, true).finish(None, &gate);
    assert_eq!(adapter.alt_done.load(Ordering::SeqCst), adapter.alt_gen.load(Ordering::SeqCst), "refusal settles the addressed request");
    assert!(state.jobs.is_empty());
}
