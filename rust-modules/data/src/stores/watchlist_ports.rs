//! Worker-only transport. Registry and session readers are boot-selected by the runner.
use crate::watchlist::{WatchlistAuth, WatchlistIo};
use crate::watchlist::ledger::Intent;
use crate::watchlist::model::Reach;
use plx_plex::plex::{Client, KeyedClients, ServerId, GuidKind, GuidMatch, GuidLookupError};
use plx_plex::plex::account::AccountClient;
use plx_plex::plex::watchlist::{WatchPage, WatchItem, WatchlistError};
use super::Snapshot;

pub struct LiveWatchlistIo {
    pub input: Snapshot,
    pub lookup: fn(ServerId) -> Option<&'static Client>,
    pub lease: fn(&WatchlistAuth) -> bool,
}
impl Reach for LiveWatchlistIo {
    fn reachable(&self, sid: ServerId) -> bool { self.input.reachable(sid) }
    fn roster(&self) -> Vec<ServerId> { self.input.roster() }
    fn copy_rank(&self, sid: ServerId) -> u8 { self.input.copy_rank(sid) }
}
impl WatchlistIo for LiveWatchlistIo {
    fn page(&self, auth: &WatchlistAuth, start: u32, size: u32, etag: Option<&str>) -> Result<WatchPage, WatchlistError> {
        let client = AccountClient::new(&auth.client_id, Some(&auth.credential));
        let mut transport = client.watchlist_transport();
        if !(self.lease)(auth) { return Err(WatchlistError::Unauthorized); }
        client.watchlist_page(start, size, etag, &mut transport)
    }
    fn released(&self, auth: &WatchlistAuth) -> Result<Vec<WatchItem>, WatchlistError> {
        let client = AccountClient::new(&auth.client_id, Some(&auth.credential));
        let mut transport = client.watchlist_transport();
        if !(self.lease)(auth) { return Err(WatchlistError::Unauthorized); }
        client.watchlist_released(&mut transport)
    }
    fn write(&self, auth: &WatchlistAuth, hex: &str, intent: Intent) -> Result<(), WatchlistError> {
        let client = AccountClient::new(&auth.client_id, Some(&auth.credential));
        let mut transport = client.watchlist_transport();
        if !(self.lease)(auth) { return Err(WatchlistError::Unauthorized); }
        match intent {
            Intent::Add => client.watchlist_add(hex, &mut transport),
            Intent::Remove => client.watchlist_remove(hex, &mut transport),
        }
    }
    fn find(&self, sid: ServerId, kind: GuidKind, guids: &[String]) -> Result<Vec<GuidMatch>, GuidLookupError> {
        KeyedClients::new(&self.input.keys, self.lookup).client(sid)
            .ok_or(GuidLookupError::Unanswered)?.find_by_guids(kind, guids)
    }
    fn roster_gen(&self) -> u32 { self.input.roster_gen }
    fn facts_gen(&self) -> u32 { self.input.facts_gen }
    fn today(&self) -> Option<String> { self.input.today.clone() }
}

/// The algorithm sees captured facts even when its transport is scripted.
pub struct CapturedIo<'a> { pub input: &'a Snapshot, pub transport: &'a dyn WatchlistIo, pub cancelled: &'a (dyn Fn() -> bool + Sync) }
impl Reach for CapturedIo<'_> {
    fn reachable(&self, sid: ServerId) -> bool { self.input.reachable(sid) }
    fn roster(&self) -> Vec<ServerId> { self.input.roster() }
    fn copy_rank(&self, sid: ServerId) -> u8 { self.input.copy_rank(sid) }
}
impl WatchlistIo for CapturedIo<'_> {
    fn page(&self, a: &WatchlistAuth, s: u32, n: u32, e: Option<&str>) -> Result<WatchPage, WatchlistError> { if (self.cancelled)() { Err(WatchlistError::Transport) } else { self.transport.page(a, s, n, e) } }
    fn released(&self, a: &WatchlistAuth) -> Result<Vec<WatchItem>, WatchlistError> { if (self.cancelled)() { Err(WatchlistError::Transport) } else { self.transport.released(a) } }
    fn write(&self, a: &WatchlistAuth, h: &str, i: Intent) -> Result<(), WatchlistError> { self.transport.write(a, h, i) }
    fn find(&self, s: ServerId, k: GuidKind, g: &[String]) -> Result<Vec<GuidMatch>, GuidLookupError> { if (self.cancelled)() { Err(GuidLookupError::Unanswered) } else { self.transport.find(s, k, g) } }
    fn roster_gen(&self) -> u32 { self.input.roster_gen }
    fn facts_gen(&self) -> u32 { self.input.facts_gen }
    fn today(&self) -> Option<String> { self.input.today.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stores::watchlist_job::SnapshotData;
    #[test]
    fn stream_jobs_account_page_released_and_write_recheck_lease() {
        let io = LiveWatchlistIo { input: Snapshot::default(), lookup: |_| panic!("no server lookup"), lease: |_| false };
        let auth = crate::watchlist::WatchlistInput::for_test(1).auth().unwrap().clone();
        assert!(matches!(io.page(&auth, 0, 1, None), Err(WatchlistError::Unauthorized)));
        assert!(matches!(io.released(&auth), Err(WatchlistError::Unauthorized)));
        assert!(matches!(io.write(&auth, "000000000000000000000000", Intent::Add), Err(WatchlistError::Unauthorized)));
    }
#[test]
    fn stream_jobs_live_find_rechecks_full_captured_client_key() {
        use plx_plex::plex::{ClientKey, ClientKeys};
        let _serial = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("watchlist-key-test", "127.0.0.1", 9, "old", "cid");
        let client = plx_plex::plex::client_for(sid).unwrap();
        let keys = ClientKeys::of([(sid, client)]);
        let before = keys.key(sid).unwrap();
        client.set_token("new");
        let after = ClientKey::of(client);
        assert_eq!(before.instance, after.instance);
        assert_ne!(before.token, after.token);
        let io = LiveWatchlistIo { input: Snapshot::from(SnapshotData { keys, ..SnapshotData::default() }), lookup: plx_plex::plex::client_for, lease: |_| true };
        assert!(matches!(io.find(sid, GuidKind::Movie, &["plex://movie/test".into()]), Err(GuidLookupError::Unanswered)));
        plx_plex::plex::reset_servers_for_test();
    }

    #[test]
    fn stream_jobs_each_worker_panic_posts_exactly_one_terminal() {
        use crate::stores::watchlist_job::*;
        use crate::watchlist::{Tag, What};
        use crate::watchlist::model::Kind;
        use plx_machine::machine::{Addr, MachineId, RequestId};
        use plx_plex::plex::{ClientKey, ClientKeys};
        use std::sync::Arc;
        for kind in 0..3 {
            let sid = ServerId::from_raw(0);
            let tag = Tag { profile_gen: 1, epoch: 2, req: 3 };
            let addr = Addr { to: MachineId::Store(crate::stores::StoreId::Watchlist.ord()), req: RequestId(tag.req) };
            let input = Snapshot::from(SnapshotData { keys: ClientKeys::from_keys([(sid, ClientKey { instance: 1, token: 2, grant: 3 })]),
                reach: vec![(sid, true, 0)], ..SnapshotData::default() });
            let io = LiveWatchlistIo { input: input.clone(), lookup: |_| panic!("scripted lookup panic"), lease: |_| panic!("scripted account lease panic") };
            let mail = Arc::new(stream::StreamMailbox::<What>::default());
            let port = WatchlistStreamPort::new(addr, tag, mail.clone());
            let rows = vec![("plex://movie/010101010101010101010101".into(), Kind::Movie)];
            match kind {
                0 => finish_list_job(&port, addr, ListReq { tag, input, auth: AuthValue(crate::watchlist::WatchlistInput::for_test(1).auth().unwrap().clone()), known: Default::default(), known_gen: 0, etag: None }, &io),
                1 => finish_rematch_job(&port, addr, RematchReq { tag, input, rows }, &io),
                _ => finish_delta_job(&port, addr, DeltaReq { tag, input, sid, rows: Arc::new(rows) }, &io),
            }
            port.refused(addr);
            let batch = mail.take();
            assert_eq!(batch.len(), 1);
            assert!(matches!(batch[0].event, stream::StreamEvent::End(stream::StreamEnd::Panicked)));
            assert!(batch[0].ack_terminal());
            assert!(!port.is_owed());
        }
    }
}
