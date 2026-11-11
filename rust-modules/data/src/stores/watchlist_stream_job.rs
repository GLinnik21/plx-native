//! Captured Watchlist stream requests and worker completions.
use super::{stream, ports};
use crate::watchlist::{Tag, What, Landed, WatchlistIo, WatchlistAuth, run_list, run_rematch, run_delta};
use crate::watchlist::model::{Kind, Reach};
use plx_machine::machine::Addr;
use plx_plex::plex::{ClientKeys, ServerId};
use std::collections::HashSet;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub keys: ClientKeys,
    pub today: Option<String>,
    pub roster_gen: u32,
    pub facts_gen: u32,
    pub reach: Vec<(ServerId, bool, u8)>,
}
impl Snapshot {
    pub fn new(keys: &ClientKeys, today: Option<String>) -> Self {
        let facts = crate::stores::watchlist::WatchlistFacts { keys, today: today.as_deref() };
        Self { keys: keys.clone(), today: today.clone(), roster_gen: keys.roster_gen(), facts_gen: keys.facts_gen(),
            reach: facts.roster().into_iter().map(|s| (s, facts.reachable(s), facts.copy_rank(s))).collect() }
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn scripted(io: &dyn WatchlistIo) -> Self {
        Self { keys: ClientKeys::default(), today: io.today(), roster_gen: io.roster_gen(), facts_gen: io.facts_gen(),
            reach: io.roster().into_iter().map(|s| (s, io.reachable(s), io.copy_rank(s))).collect() }
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
pub struct DeltaReq { pub tag: Tag, pub input: Snapshot, pub sid: ServerId, pub rows: Vec<(String, Kind)> }
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
