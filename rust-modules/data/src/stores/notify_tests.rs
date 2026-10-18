//! The notification store against a fake connector: when it connects, when it does not, what a
//! reconnect owes, and that nothing here ever opens a real stream.

use super::*;
use crate::stores::Stores;
use plx_plex::plex::notify::PlayState;
use plx_plex::plex::{self, GrantEvidence};

/// Every connection the store asked for, in order, and whether the next ask is refused.
#[derive(Default)]
struct Log {
    links: Vec<(ServerId, Link)>,
    refuse: bool,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Log>>);

impl Fake {
    fn lock(&self) -> std::sync::MutexGuard<'_, Log> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn connects(&self) -> usize {
        self.lock().links.len()
    }
    fn link(&self, n: usize) -> Link {
        self.lock().links[n].1.clone()
    }
}

impl Connector for Fake {
    fn connect(&mut self, sid: ServerId, link: Link) -> bool {
        let mut log = self.lock();
        if log.refuse {
            return false;
        }
        log.links.push((sid, link));
        true
    }
}

/// The registry is process-global: hold its lock for the test and empty it on the way out.
struct Rig {
    _lock: plx_base::testlock::Serial,
    store: NotifyStore,
    fake: Fake,
    sid: ServerId,
}

impl Drop for Rig {
    fn drop(&mut self) {
        plex::reset_servers_for_test();
    }
}

/// One server registered with `token`, described as `evidence` (or not at all).
fn rig_with(token: &str, evidence: Option<GrantEvidence>) -> Rig {
    let lock = plx_base::testlock::serial();
    plex::reset_servers_for_test();
    let sid = plex::register_for_test("machine-a", "127.0.0.1", 9, token, "cid-own");
    if let Some(evidence) = evidence {
        plex::describe_server(sid, "Server A", "", evidence);
    }
    let fake = Fake::default();
    let store = NotifyStore::with_connector_for_test(Box::new(fake.clone()));
    Rig { _lock: lock, store, fake, sid }
}

fn rig() -> Rig {
    rig_with("tok-a", Some(GrantEvidence::ours()))
}

impl Rig {
    fn pump(&mut self) -> NotifyOut {
        self.store.pump(false)
    }
    /// Pump `n` frames, discarding the output.
    fn pump_n(&mut self, n: u64) {
        for _ in 0..n {
            let _ = self.pump();
        }
    }
    fn client(&self) -> &'static plex::Client {
        plex::client_for(self.sid).expect("registered")
    }
}

fn play(rating_key: &str, state: PlayState) -> Notification {
    Notification::Play(vec![PlaySession {
        session_key: Some("1".into()),
        client_identifier: Some("other".into()),
        rating_key: Some(rating_key.into()),
        key: None,
        view_offset_ms: None,
        state,
    }])
}

#[test]
fn first_connect_no_epoch() {
    let mut r = rig();
    let _ = r.pump();
    assert_eq!(r.fake.connects(), 1);
    r.fake.link(0).push(Notification::Ping);
    let out = r.pump();
    assert!(out.resynced.is_empty(), "the first connection has missed nothing: {out:?}");
    assert_eq!(r.store.resync_epoch(r.sid), 0);
}

#[test]
fn reconnect_after_live_bumps_epoch() {
    let mut r = rig();
    r.pump_n(1);
    r.fake.link(0).push(Notification::Ping);
    r.pump_n(1);
    r.fake.link(0).finish(NotifyEnd::Closed);
    r.pump_n(1);
    assert_eq!(r.fake.connects(), 1, "backing off");
    r.pump_n(2 * FRAMES_PER_S * 2);
    assert_eq!(r.fake.connects(), 2, "the retry opened");
    assert_eq!(r.store.resync_epoch(r.sid), 0, "not live again yet: nothing is known to be missed");
    r.fake.link(1).push(Notification::Ping);
    let out = r.pump();
    assert_eq!(out.resynced, vec![(r.sid, 1)]);
    assert_eq!(r.store.resync_epoch(r.sid), 1);
    r.fake.link(1).push(Notification::Ping);
    assert!(r.pump().resynced.is_empty(), "one bump per reconnect");
}

#[test]
fn silent_backs_off_in_frames_and_doubles() {
    let mut r = rig();
    r.pump_n(1);
    r.fake.link(0).finish(NotifyEnd::Silent);
    r.pump_n(1);
    // 2 s = 120 frames, plus at most a quarter jitter.
    r.pump_n(2 * FRAMES_PER_S - 5);
    assert_eq!(r.fake.connects(), 1, "still inside the first backoff");
    r.pump_n(2 * FRAMES_PER_S / 4 + 10);
    assert_eq!(r.fake.connects(), 2, "the first backoff is 2 s plus jitter");
    // The second failure waits twice as long.
    r.fake.link(1).finish(NotifyEnd::Silent);
    r.pump_n(1);
    r.pump_n(4 * FRAMES_PER_S - 5);
    assert_eq!(r.fake.connects(), 2, "the second backoff is 4 s");
    r.pump_n(4 * FRAMES_PER_S / 4 + 10);
    assert_eq!(r.fake.connects(), 3);
}

#[test]
fn the_backoff_never_exceeds_a_minute_and_a_quarter() {
    let mut r = rig();
    for round in 0..12 {
        r.pump_n(1);
        let n = r.fake.connects();
        r.fake.link(n - 1).finish(NotifyEnd::Transport);
        r.pump_n(1);
        r.pump_n(75 * FRAMES_PER_S);
        assert_eq!(r.fake.connects(), n + 1, "round {round}: the retry opened within 75 s");
    }
}

#[test]
fn suspend_cancels_resume_reconnects() {
    let mut r = rig();
    r.pump_n(1);
    let first = r.fake.link(0);
    assert!(!first.is_cancelled());
    r.store.suspend();
    assert!(first.is_cancelled(), "WILL background cancels at once, not on the next frame");
    r.pump_n(30);
    assert_eq!(r.fake.connects(), 1, "nothing opens while suspended");
    r.store.resume();
    r.pump_n(1);
    assert_eq!(r.fake.connects(), 2, "resume reconnects without waiting out a backoff");
}

#[test]
fn resume_forgives_a_backoff_but_not_a_park() {
    let mut r = rig();
    r.pump_n(1);
    r.fake.link(0).finish(NotifyEnd::Closed);
    r.pump_n(1);
    r.store.resume();
    r.pump_n(1);
    assert_eq!(r.fake.connects(), 2, "the backoff was forgiven");
    r.fake.link(1).finish(NotifyEnd::Refused { status: 403 });
    r.pump_n(1);
    r.store.resume();
    r.store.network_changed();
    r.pump_n(10);
    assert_eq!(r.fake.connects(), 2, "a refusal is not about the network");
}

#[test]
fn network_changed_cancels_and_reconnects() {
    let mut r = rig();
    r.pump_n(1);
    r.fake.link(0).push(Notification::Ping);
    r.pump_n(1);
    let first = r.fake.link(0);
    r.store.network_changed();
    assert!(first.is_cancelled());
    r.pump_n(1);
    assert_eq!(r.fake.connects(), 2);
    r.fake.link(1).push(Notification::Ping);
    assert!(r.pump().resynced.is_empty(), "the foreground re-reads on its own: a deliberate reconnect is no gap");
}

#[test]
fn foreground_reconnect_does_not_resync() {
    let mut r = rig();
    r.pump_n(1);
    r.fake.link(0).push(Notification::Ping);
    r.pump_n(1);
    // DID foreground: suspend (WILL), then network_changed + resume.
    r.store.suspend();
    r.store.network_changed();
    r.store.resume();
    r.pump_n(1);
    assert_eq!(r.fake.connects(), 2);
    r.fake.link(1).push(Notification::Ping);
    assert!(r.pump().resynced.is_empty());
    assert_eq!(r.store.resync_epoch(r.sid), 0);
    // An unexpected end afterwards still owes one.
    r.fake.link(1).finish(NotifyEnd::Closed);
    r.pump_n(2 * FRAMES_PER_S * 2);
    r.fake.link(2).push(Notification::Ping);
    assert_eq!(r.pump().resynced, vec![(r.sid, 1)]);
}

#[test]
fn flapping_server_backs_off_and_resyncs_once() {
    let mut r = rig();
    r.pump_n(1);
    let mut gaps = Vec::new();
    for _ in 0..5 {
        let n = r.fake.connects();
        r.fake.link(n - 1).push(Notification::Ping);
        r.pump_n(1);
        r.fake.link(n - 1).finish(NotifyEnd::Closed);
        let mut waited = 0u64;
        while r.fake.connects() == n {
            r.pump_n(1);
            waited += 1;
            assert!(waited < 100 * FRAMES_PER_S, "never redialled");
        }
        gaps.push(waited);
    }
    assert!(gaps[4] > gaps[0] * 4, "a flapping server escalates its backoff: {gaps:?}");
    assert!(r.store.resync_epoch(r.sid) <= 1, "one resync per escalation, got {}", r.store.resync_epoch(r.sid));
    // Held live past the idle window, it is healthy again: the next drop starts from the base.
    let n = r.fake.connects();
    r.fake.link(n - 1).push(Notification::Ping);
    r.pump_n(26 * FRAMES_PER_S);
    r.fake.link(n - 1).finish(NotifyEnd::Closed);
    r.pump_n(1);
    r.pump_n(2 * FRAMES_PER_S * 2);
    assert_eq!(r.fake.connects(), n + 1, "a stable connection resets the backoff");
}

#[test]
fn servers_ending_in_one_pump_get_different_deadlines() {
    let mut r = rig();
    let sid_b = plex::register_for_test("machine-b", "127.0.0.2", 9, "tok-b", "cid-own");
    plex::describe_server(sid_b, "Server B", "", GrantEvidence::ours());
    r.pump_n(1);
    assert_eq!(r.fake.connects(), 2);
    for n in 0..2 {
        r.fake.link(n).push(Notification::Ping);
    }
    r.pump_n(1);
    for n in 0..2 {
        r.fake.link(n).finish(NotifyEnd::Closed);
    }
    r.pump_n(1);
    let untils: Vec<u64> = r
        .store
        .conns
        .iter()
        .map(|c| match c.state {
            State::Backoff { until } => until,
            s => panic!("{s:?}"),
        })
        .collect();
    assert_eq!(untils.len(), 2);
    assert_ne!(untils[0], untils[1], "servers that dropped together must not knock on the same frame");
}

fn refuse_gate(_: &plex::Client) -> bool {
    false
}
fn open_gate(_: &plex::Client) -> bool {
    true
}

#[test]
fn a_refused_origin_is_not_dialled_and_reconnects_when_the_grant_returns() {
    let mut r = rig();
    r.store.set_gate_for_test(refuse_gate);
    r.pump_n(70 * FRAMES_PER_S);
    assert_eq!(r.fake.connects(), 0, "a plaintext origin with no live grant is not dialled");
    r.store.set_gate_for_test(open_gate);
    r.pump_n(1);
    assert_eq!(r.fake.connects(), 1, "discovery re-minting the grant brings the feed back");
}

#[test]
fn a_gate_refusal_mid_stream_cancels_and_recovers() {
    let mut r = rig();
    r.pump_n(1);
    r.fake.link(0).push(Notification::Ping);
    r.pump_n(1);
    r.store.set_gate_for_test(refuse_gate);
    r.pump_n(1);
    assert!(r.fake.link(0).is_cancelled());
    r.store.set_gate_for_test(open_gate);
    r.pump_n(1);
    assert_eq!(r.fake.connects(), 2);
    r.fake.link(1).push(Notification::Ping);
    assert_eq!(r.pump().resynced, vec![(r.sid, 1)], "the stream was lost while the app ran: a gap is owed");
}

#[test]
fn a_local_refusal_backs_off_rather_than_parking() {
    let mut r = rig();
    r.pump_n(1);
    r.fake.link(0).finish(NotifyEnd::Refused { status: 0 });
    r.pump_n(1);
    r.pump_n(2 * FRAMES_PER_S * 2);
    assert_eq!(r.fake.connects(), 2, "status 0 is this build's own gate: retried, never parked for the session");
}

#[test]
fn token_change_reconnects_and_drops_old_mailbox() {
    let mut r = rig();
    r.pump_n(1);
    let old = r.fake.link(0);
    old.push(play("1", PlayState::Playing));
    r.client().set_token("tok-b");
    let out = r.pump();
    assert!(old.is_cancelled(), "the old token's connection is cancelled");
    assert!(out.plays.is_empty(), "…and what it had already queued is dropped: {out:?}");
    assert_eq!(r.fake.connects(), 2, "reconnected under the new token");
    old.push(play("2", PlayState::Playing));
    r.fake.link(1).push(play("3", PlayState::Paused));
    let out = r.pump();
    assert_eq!(out.plays.len(), 1);
    assert_eq!(out.plays[0].sessions[0].rating_key.as_deref(), Some("3"));
}

#[test]
fn empty_token_never_connects() {
    let mut r = rig_with("", Some(GrantEvidence::ours()));
    r.pump_n(5);
    assert_eq!(r.fake.connects(), 0, "a profile switch in flight has no token to stream with");
    r.client().set_token("tok-new");
    r.pump_n(1);
    assert_eq!(r.fake.connects(), 1, "…and connects the moment it has one");
    r.client().set_token("");
    r.pump_n(1);
    assert!(r.fake.link(0).is_cancelled(), "a token blanked mid-stream cancels it");
    r.pump_n(5);
    assert_eq!(r.fake.connects(), 1);
}

#[test]
fn parked_on_401_until_token_gen() {
    let mut r = rig();
    r.pump_n(1);
    r.fake.link(0).finish(NotifyEnd::Refused { status: 401 });
    r.pump_n(1);
    r.store.resume();
    r.store.network_changed();
    r.pump_n(70 * FRAMES_PER_S);
    assert_eq!(r.fake.connects(), 1, "a 401 is not retried with the same token");
    r.client().set_token("tok-b");
    r.pump_n(1);
    assert_eq!(r.fake.connects(), 2, "a new token un-parks it");
}

#[test]
fn parked_on_other_4xx_and_unsupported() {
    for end in [
        NotifyEnd::Refused { status: 403 },
        NotifyEnd::Refused { status: 404 },
        NotifyEnd::Unsupported,
    ] {
        let mut r = rig();
        r.pump_n(1);
        r.fake.link(0).finish(end);
        r.pump_n(1);
        r.pump_n(70 * FRAMES_PER_S);
        assert_eq!(r.fake.connects(), 1, "{end:?} parks the server for the session");
        r.client().set_token("tok-b");
        r.pump_n(70 * FRAMES_PER_S);
        assert_eq!(r.fake.connects(), 1, "{end:?}: a new token does not un-park a session park");
        r.store.reset();
        r.pump_n(1);
        assert_eq!(r.fake.connects(), 2, "{end:?}: a reset forgets it");
    }
}

#[test]
fn a_5xx_is_retried_with_backoff() {
    let mut r = rig();
    r.pump_n(1);
    r.fake.link(0).finish(NotifyEnd::Refused { status: 503 });
    r.pump_n(1);
    r.pump_n(2 * FRAMES_PER_S * 2);
    assert_eq!(r.fake.connects(), 2);
}

#[test]
fn off_when_gate_armed() {
    let r = rig();
    let mut stores = Stores::default();
    stores.notify = NotifyStore::with_connector_for_test(Box::new(r.fake.clone()));
    stores.landgate.arm_recording();
    let _ = stores.notify_pump();
    let _ = stores.notify_pump();
    assert_eq!(r.fake.connects(), 0, "a recording owns the run: no live stream");
    stores.landgate.disarm();
    let _ = stores.notify_pump();
    assert_eq!(r.fake.connects(), 1);
    stores.landgate.arm_replay(vec![vec![(1, 1)]]);
    let _ = stores.notify_pump();
    assert!(r.fake.link(0).is_cancelled(), "arming mid-stream cancels the connection");
    stores.landgate.disarm();
}

#[test]
fn off_when_data_io_disabled() {
    let mut r = rig();
    r.client().disable_data_io();
    r.pump_n(5);
    assert_eq!(r.fake.connects(), 0);
    assert_eq!(r.client().denied_data_requests(), 0, "asking is not a denied request");
}

#[test]
fn data_io_disabled_mid_stream_cancels_it() {
    let mut r = rig();
    r.pump_n(1);
    r.client().disable_data_io();
    r.pump_n(1);
    assert!(r.fake.link(0).is_cancelled());
}

#[test]
fn enabled_const_connects_to_an_owned_server() {
    // The shipped constant is true, and the store built the way production builds it (its flag from
    // the constant) opens exactly one connection to an owned server.
    assert!(NOTIFY_ENABLED, "notifications ship ON for owned servers (docs/pms-api.md §9)");
    let lock = plx_base::testlock::serial();
    plex::reset_servers_for_test();
    let sid = plex::register_for_test("machine-a", "127.0.0.1", 9, "tok", "cid-own");
    plex::describe_server(sid, "Server A", "", GrantEvidence::ours());
    let fake = Fake::default();
    let mut store = NotifyStore::new(NOTIFY_ENABLED, Box::new(fake.clone()));
    for _ in 0..10 {
        let _ = store.pump(false);
    }
    assert_eq!(fake.connects(), 1);
    plex::reset_servers_for_test();
    drop(lock);
}

#[test]
fn non_owned_never_connects() {
    for evidence in [GrantEvidence::outside(), GrantEvidence::household(7)] {
        let mut r = rig_with("tok", Some(evidence));
        r.pump_n(5);
        assert_eq!(r.fake.connects(), 0, "{evidence:?}: a share or a household member's server stays dark");
    }
}

#[test]
fn an_undescribed_server_fails_closed() {
    let mut r = rig_with("tok", None);
    r.pump_n(5);
    assert_eq!(r.fake.connects(), 0, "nothing has said this account owns it");
    plex::describe_server(r.sid, "Server A", "", GrantEvidence::ours());
    r.pump_n(1);
    assert_eq!(r.fake.connects(), 1, "…and it connects once something has");
}

#[test]
fn losing_ownership_cancels_the_connection() {
    let mut r = rig();
    r.pump_n(1);
    plex::describe_server(r.sid, "Server A", "", GrantEvidence::outside());
    r.pump_n(1);
    assert!(r.fake.link(0).is_cancelled());
    r.pump_n(5);
    assert_eq!(r.fake.connects(), 1);
}

#[test]
fn drop_cancels() {
    let mut r = rig();
    r.pump_n(1);
    let link = r.fake.link(0);
    assert!(!link.is_cancelled());
    let old = std::mem::replace(&mut r.store, NotifyStore::new(true, Box::new(Fake::default())));
    drop(old);
    assert!(link.is_cancelled(), "dropping the store cancels every worker");
}

#[test]
fn spawn_refused_backs_off() {
    let mut r = rig();
    r.fake.lock().refuse = true;
    r.pump_n(1);
    assert_eq!(r.fake.connects(), 0);
    r.fake.lock().refuse = false;
    r.pump_n(2 * FRAMES_PER_S - 5);
    assert_eq!(r.fake.connects(), 0, "a refused spawn waits out a backoff like any failure");
    r.pump_n(2 * FRAMES_PER_S / 4 + 10);
    assert_eq!(r.fake.connects(), 1);
}

#[test]
fn pings_and_ignored_are_not_output_but_plays_are() {
    let mut r = rig();
    r.pump_n(1);
    let link = r.fake.link(0);
    link.push(Notification::Ping);
    link.push(Notification::Ignored);
    assert_eq!(r.pump(), NotifyOut::default());
    link.push(play("5", PlayState::Stopped));
    let out = r.pump();
    assert_eq!(out.plays.len(), 1);
    assert_eq!(out.plays[0].sid, r.sid);
    assert_eq!(out.plays[0].sessions[0].state, PlayState::Stopped);
}

#[test]
fn library_changes_are_output_per_server_beside_the_plays() {
    use plx_plex::plex::notify::{LibraryEntry, LibraryKind, LibraryState};
    let mut r = rig();
    r.pump_n(1);
    let link = r.fake.link(0);
    let entry = |id: &str, state| LibraryEntry {
        item_id: id.into(),
        kind: LibraryKind::Show,
        state,
        section: None,
    };
    link.push(Notification::Library(vec![entry("7", LibraryState::Created)]));
    link.push(Notification::Library(vec![entry("8", LibraryState::Deleted)]));
    let out = r.pump();
    assert!(out.plays.is_empty());
    assert_eq!(out.library.len(), 1, "one batch per server and pump");
    assert_eq!(out.library[0].sid, r.sid);
    let ids: Vec<&str> = out.library[0].entries.iter().map(|e| e.item_id.as_str()).collect();
    assert_eq!(ids, ["7", "8"]);
}

#[test]
fn reset_closes_everything_and_the_next_connect_is_a_first_connect() {
    let mut r = rig();
    r.pump_n(1);
    r.fake.link(0).push(Notification::Ping);
    r.pump_n(1);
    let first = r.fake.link(0);
    r.store.reset();
    assert!(first.is_cancelled());
    r.pump_n(1);
    assert_eq!(r.fake.connects(), 2);
    r.fake.link(1).push(Notification::Ping);
    assert!(r.pump().resynced.is_empty(), "a new profile has no gap to resync");
    assert_eq!(r.store.resync_epoch(r.sid), 0);
}

#[test]
fn a_worker_ending_after_a_cancel_is_never_read() {
    let mut r = rig();
    r.pump_n(1);
    let old = r.fake.link(0);
    r.store.suspend();
    old.finish(NotifyEnd::Refused { status: 403 });
    r.store.resume();
    r.pump_n(1);
    assert_eq!(r.fake.connects(), 2, "the cancelled connection's end cannot park its successor");
}
