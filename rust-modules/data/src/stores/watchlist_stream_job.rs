//! Captured Watchlist stream requests and worker completions.
use super::{stream, ports};
use crate::watchlist::{Tag, What, Landed, WatchlistIo, WatchlistAuth, run_list, run_rematch, run_delta};
use crate::watchlist::model::{Kind, Reach};
use plx_machine::machine::Addr;
use plx_plex::plex::{ClientKeys, ServerId};
use std::collections::HashSet;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot(std::sync::Arc<SnapshotData>);

#[derive(Debug, Default, PartialEq)]
pub struct SnapshotData {
    pub keys: ClientKeys,
    pub today: Option<String>,
    pub roster_gen: u32,
    pub facts_gen: u32,
    pub reach: Vec<(ServerId, bool, u8)>,
}
impl std::ops::Deref for Snapshot {
    type Target = SnapshotData;
    fn deref(&self) -> &Self::Target { &self.0 }
}
impl From<SnapshotData> for Snapshot {
    fn from(data: SnapshotData) -> Self { Self(std::sync::Arc::new(data)) }
}
impl Snapshot {
    pub fn new(keys: &ClientKeys, today: Option<String>) -> Self {
        let facts = crate::stores::watchlist::WatchlistFacts { keys, today: today.as_deref() };
        let reach = keys.slots().map(|s| (s, facts.reachable(s), facts.copy_rank(s))).collect();
        SnapshotData { keys: keys.clone(), today, roster_gen: keys.roster_gen(), facts_gen: keys.facts_gen(), reach }.into()
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn scripted(io: &dyn WatchlistIo) -> Self {
        SnapshotData { keys: ClientKeys::default(), today: io.today(), roster_gen: io.roster_gen(), facts_gen: io.facts_gen(),
            reach: io.roster().into_iter().map(|s| (s, io.reachable(s), io.copy_rank(s))).collect() }.into()
    }
}

/// Host-owned retained capture. Equality includes credentials, facts and eligibility, not just generations.
#[derive(Default)]
pub struct SnapshotCache {
    day: Option<i64>,
    snapshot: Option<Snapshot>,
}
impl SnapshotCache {
    pub fn capture(&mut self, keys: &ClientKeys, day: Option<i64>) -> Snapshot {
        if let Some(snapshot) = &self.snapshot {
            if self.day == day && &snapshot.keys == keys { return snapshot.clone(); }
        }
        let snapshot = Snapshot::new(keys, day.map(crate::watchlist::ymd_of_unix_days));
        self.day = day;
        self.snapshot = Some(snapshot.clone());
        snapshot
    }
}
impl Reach for Snapshot {
    fn reachable(&self, sid: ServerId) -> bool { self.reach.iter().any(|(s, up, _)| *s == sid && *up) }
    fn roster(&self) -> Vec<ServerId> { self.reach.iter().map(|(s, _, _)| *s).collect() }
    fn copy_rank(&self, sid: ServerId) -> u8 { self.reach.iter().find(|(s, _, _)| *s == sid).map_or(2, |(_, _, r)| *r) }
}
#[derive(Debug, PartialEq)]
pub struct ListReq { pub tag: Tag, pub input: Snapshot, pub auth: AuthValue,
    pub known: HashSet<String>, pub known_gen: u32, pub etag: Option<String> }
#[derive(Clone)]
pub struct AuthValue(pub WatchlistAuth);
impl std::fmt::Debug for AuthValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { self.0.fmt(f) }
}
impl PartialEq for AuthValue {
    fn eq(&self, b: &Self) -> bool { let a = &self.0; let b = &b.0;
        a.client_id == b.client_id && a.credential == b.credential && a.generation == b.generation
            && a.user.id == b.user.id && a.user.uuid == b.user.uuid }
}
#[derive(Clone, Debug, PartialEq)]
pub struct RematchReq { pub tag: Tag, pub input: Snapshot, pub rows: Vec<(String, Kind)> }
#[derive(Clone, Debug, PartialEq)]
pub struct DeltaReq { pub tag: Tag, pub input: Snapshot, pub sid: ServerId, pub rows: std::sync::Arc<Vec<(String, Kind)>> }
pub type WatchlistStreamPort = stream::StreamPort<What>;
fn finish_stream(port: &WatchlistStreamPort, addr: Addr, input: &Snapshot,
    transport: &dyn WatchlistIo, body: impl FnOnce(&dyn WatchlistIo, &mut dyn FnMut(Landed))) {
    let Some(producer) = port.start() else { return };
    let cancelled = || port.is_cancelled();
    let io = ports::CapturedIo { input, transport, cancelled: &cancelled };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        body(&io, &mut |l| { if !producer.is_cancelled() { producer.post(addr, l.what); } });
    }));
    let end = if result.is_err() { stream::StreamEnd::Panicked }
        else if producer.is_cancelled() { stream::StreamEnd::Cancelled } else { stream::StreamEnd::Done };
    producer.finish(end);
}
pub fn finish_list_job(port: &WatchlistStreamPort, addr: Addr, req: ListReq, io: &dyn WatchlistIo) {
    finish_stream(port, addr, &req.input, io, |io, post| run_list(io, &req.auth.0, req.tag, (&req.known, req.known_gen), req.etag.as_deref(), post));
}
pub fn finish_rematch_job(port: &WatchlistStreamPort, addr: Addr, req: RematchReq, io: &dyn WatchlistIo) {
    finish_stream(port, addr, &req.input, io, |io, post| run_rematch(io, &req.rows, req.tag, post));
}
pub fn finish_delta_job(port: &WatchlistStreamPort, addr: Addr, req: DeltaReq, io: &dyn WatchlistIo) {
    finish_stream(port, addr, &req.input, io, |io, post| run_delta(io, req.sid, &req.rows, req.tag, post));
}

#[cfg(test)]
mod snapshot_cache_tests {
    use super::*;
    use plx_plex::plex::{Client, ClientKey, probe::Outcome};
    use std::sync::Arc;

    fn client() -> &'static Client {
        let sid = plx_plex::plex::register_for_test(
            "watchlist-capture", "127.0.0.1", 9, "old-token", "watchlist-test");
        plx_plex::plex::client_for(sid).unwrap()
    }
    fn keys(client: &'static Client, reachable: bool, allowed: bool) -> ClientKeys {
        let mut keys = ClientKeys::default();
        keys.refill_roster_with_stream_policy(client.id(), 5, 7, [client.id()],
            |_| Some(client), |_| None, |_| reachable.then_some(Outcome::Reachable), |_| allowed);
        keys
    }

    #[test]
    fn watchlist_snapshot_cache_retains_populated_capture_and_buffers_on_stable_passes() {
        let _serial = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let client = client();
        let keys = keys(client, true, true);
        let mut cache = SnapshotCache::default();
        let held = cache.capture(&keys, Some(20_000));
        let reach = held.reach.as_ptr();
        let machine = held.keys.facts(client.id()).unwrap().machine_id.as_ptr();
        let date = held.today.as_ref().unwrap().as_ptr();
        for _ in 0..64 {
            let current = cache.capture(&keys, Some(20_000));
            assert!(Arc::ptr_eq(&held.0, &current.0));
            assert_eq!(current.reach.as_ptr(), reach);
            assert_eq!(current.keys.facts(client.id()).unwrap().machine_id.as_ptr(), machine);
            assert_eq!(current.today.as_ref().unwrap().as_ptr(), date);
            assert_eq!(current.keys.iter().count(), 1);
        }
        plx_plex::plex::reset_servers_for_test();
    }

    #[test]
    fn watchlist_snapshot_cache_tracks_full_keys_facts_eligibility_and_day_without_mutating_old_jobs() {
        let _serial = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let client = client();
        let initial = keys(client, true, true);
        let old_key = ClientKey::of(client);
        let mut cache = SnapshotCache::default();
        let old = cache.capture(&initial, Some(20_000));
        client.set_token("new-token");
        let retokened = keys(client, true, true);
        assert_eq!((initial.roster_gen(), initial.facts_gen()), (retokened.roster_gen(), retokened.facts_gen()));
        let changed_key = cache.capture(&retokened, Some(20_000));
        assert!(!Arc::ptr_eq(&old.0, &changed_key.0));
        assert_ne!(changed_key.keys.key(client.id()), Some(old_key));
        let ineligible = keys(client, true, false);
        let changed_eligibility = cache.capture(&ineligible, Some(20_000));
        assert!(!Arc::ptr_eq(&changed_key.0, &changed_eligibility.0));
        assert!(!changed_eligibility.keys.stream_allowed(client.id()));
        let unreachable = keys(client, false, false);
        let changed_facts = cache.capture(&unreachable, Some(20_000));
        assert!(!Arc::ptr_eq(&changed_eligibility.0, &changed_facts.0));
        assert!(!changed_facts.reachable(client.id()));
        let changed_day = cache.capture(&unreachable, Some(20_001));
        assert!(!Arc::ptr_eq(&changed_facts.0, &changed_day.0));
        assert_ne!(old.today, changed_day.today);
        let no_day = cache.capture(&unreachable, None);
        assert!(no_day.today.is_none());
        assert_eq!(old.keys.key(client.id()), Some(old_key));
        assert!(old.keys.stream_allowed(client.id()));
        assert!(old.reachable(client.id()));
        assert_eq!(old.today.as_deref(), Some(crate::watchlist::ymd_of_unix_days(20_000).as_str()));
        plx_plex::plex::reset_servers_for_test();
    }
}
