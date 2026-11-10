//! Finite runner ports drive the production cursor; no account, socket, or worker is needed.
use super::*;
use plx_data::stores::notify::{NotifyInput, NotifyOut, NotifyStore};
use plx_plex::plex::{self, ServerFacts, ServerId};
use plx_plex::plex::notify::{Notification, NotifyEnd};

struct ResetServers;
impl Drop for ResetServers {
    fn drop(&mut self) { plex::reset_servers_for_test(); }
}

struct Rig { store: NotifyStore, keys: ClientKeys, client: &'static Client, frame: u64 }
impl Rig {
    fn new() -> Self {
        plex::reset_servers_for_test();
        let sid = plex::register_for_test("notify-runner", "127.0.0.1", 9, "script-token", "script-client");
        let client = plex::client_for(sid).expect("registered notify runner client");
        let facts: &'static ServerFacts = Box::leak(Box::new(ServerFacts {
            name: "Script".into(), handle: String::new(), owned: true, home: false, owner_id: 0,
        }));
        let mut keys = ClientKeys::default();
        keys.refill_roster_with_stream_policy(client.id(), 1, 1, [client.id()], |_| Some(client),
            |_| Some(facts), |_| None, |_| true);
        Self { store: NotifyStore::default(), keys, client, frame: 0 }
    }
    fn tick(&mut self, mut start: impl FnMut(StoreJob)) -> NotifyOut {
        self.frame += 1;
        let input = NotifyInput { keys: &self.keys, armed: false };
        let mut pass = self.store.begin(&input);
        while let Some(mut turn) = self.store.prepare_next(&mut pass, &input) {
            for job in self.store.take_jobs() { start(job); }
            self.store.settle_admission(&mut pass, &mut turn);
            self.store.finish_turn(&mut pass, turn, &input);
        }
        let out = self.store.end(pass);
        assert!(!self.store.has_jobs());
        out
    }
    fn quiet_until(&mut self, frame: u64) {
        while self.frame + 1 < frame { self.tick(|_| panic!("retry before deadline")); }
    }
}
fn delay(frame: u64, sid: ServerId, attempts: u32) -> u64 {
    let d = (2u64 << attempts.min(8)).min(60) * 60;
    frame + d + frame.wrapping_add(u64::from(sid.raw()) * 7919) % (d / 4).max(1)
}

#[test]
fn initial_spawn_false_is_exactly_once_at_origin_frame_with_exact_jitter() {
    let _serial = plx_base::testlock::serial();
    let _reset = ResetServers;
    let mut r = Rig::new();
    let c = r.client;
    let mut runner = StoreRunner::live();
    let mut launches = 0;
    r.tick(|job| {
        let StoreJob::NotifyStream(job, port) = job else { panic!("Notify") };
        let late = port.clone(); let addr = job.addr;
        runner.start_notify_stream_with(job, port, |_| Some(c), |_, _, _| { launches += 1; false });
        late.refused(addr); // repeated initial refusal cannot double-backoff
        late.done(addr, NotifyTerminal::NotStarted);
    });
    assert_eq!(launches, 1);
    let until = delay(1, c.id(), 0);
    r.quiet_until(until);
    r.tick(|job| {
        let StoreJob::NotifyStream(job, port) = job else { panic!("Notify") };
        runner.start_notify_stream_with(job, port, |_| Some(c), |_, _, _| { launches += 1; false });
    });
    assert_eq!(launches, 2);
    r.quiet_until(delay(until, c.id(), 1));
}

#[test]
fn accepted_held_release_false_is_ordinary_current_frame_and_preserves_attempts() {
    let _serial = plx_base::testlock::serial();
    let _reset = ResetServers;
    let mut r = Rig::new(); let c = r.client;
    r.tick(|job| { let StoreJob::NotifyStream(job, port) = job else { panic!("Notify") }; port.refused(job.addr); });
    let first = delay(1, c.id(), 0);
    r.quiet_until(first);
    let mut held = StoreRunner::held_all();
    r.tick(|job| {
        let StoreJob::NotifyStream(job, port) = job else { panic!("Notify") };
        held.start_notify_stream_with(job, port, |_| Some(c), |_, _, _| panic!("park, not launch"));
    });
    for _ in 0..7 { r.tick(|_| panic!("held acceptance is not completion")); }
    let jobs = held.take_held(); assert_eq!(jobs.len(), 1);
    let StoreJob::NotifyStream(job, port) = jobs.into_iter().next().unwrap() else { panic!("Notify") };
    StoreRunner::release_notify_stream_with(job, port, |_| Some(c), |_, _, _| false);
    r.tick(|_| panic!("terminal is observed, not retried in this frame"));
    let observed = r.frame;
    let until = delay(observed, c.id(), 1);
    r.quiet_until(until);
    let mut emitted = 0;
    r.tick(|job| {
        emitted += 1;
        let StoreJob::NotifyStream(job, port) = job else { panic!("Notify") }; port.refused(job.addr);
    });
    assert_eq!(emitted, 1);
}

#[test]
fn cancellation_and_orphan_release_never_launch_or_poison_replacement() {
    let _serial = plx_base::testlock::serial();
    let _reset = ResetServers;
    let mut r = Rig::new(); let c = r.client;
    let mut held = StoreRunner::held_all();
    r.tick(|job| {
        let StoreJob::NotifyStream(job, port) = job else { panic!("Notify") };
        held.start_notify_stream_with(job, port, |_| Some(c), |_, _, _| panic!("held"));
    });
    let StoreJob::NotifyStream(old, port) = held.take_held().pop().unwrap() else { panic!("Notify") };
    let old_addr = old.addr;
    r.store.reset();
    assert!(port.is_cancelled());
    StoreRunner::release_notify_stream_with(old, port.clone(), |_| panic!("cancel before lookup"), |_, _, _| panic!("cancel before launch"));
    r.tick(|job| {
        let StoreJob::NotifyStream(job, fresh) = job else { panic!("Notify") };
        assert!(job.addr.req.0 > old_addr.req.0);
        fresh.admit(job.addr, true);
        fresh.post(job.addr, Notification::Ping);
        port.post(old_addr, Notification::Ping);
        port.done(old_addr, NotifyTerminal::End(NotifyEnd::Closed));
    });
    r.tick(|_| panic!("old terminal cannot retry the new live connection"));
    r.store.suspend();
}

#[test]
fn held_release_rechecks_full_key_even_when_generations_do_not_move() {
    let _serial = plx_base::testlock::serial();
    let _reset = ResetServers;
    thread_local! { static CANDIDATE: std::cell::Cell<Option<&'static Client>> = const { std::cell::Cell::new(None) }; }
    struct CandidateGuard(Option<&'static Client>);
    impl Drop for CandidateGuard {
        fn drop(&mut self) { CANDIDATE.with(|slot| slot.set(self.0)); }
    }
    let mut r = Rig::new(); let c = r.client;
    let _candidate_guard = CandidateGuard(CANDIDATE.with(|slot| slot.replace(Some(c))));
    let captured_keys = r.keys.clone();
    let mut held = StoreRunner::held_all();
    r.tick(|job| {
        let StoreJob::NotifyStream(job, port) = job else { panic!("Notify") };
        held.start_notify_stream_with(job, port, |_| Some(c), |_, _, _| panic!("held"));
    });
    let StoreJob::NotifyStream(job, port) = held.take_held().pop().unwrap() else { panic!("Notify") };
    c.set_token("replacement");
    assert_eq!(r.keys, captured_keys); // Captured roster and its generations have not moved.
    assert_ne!(job.req.key, ClientKey::of(c));
    let scripted = NotifyTransport::Script {
        eligible: |_| CANDIDATE.with(|slot| slot.get()),
        run: |_, _, _, _| panic!("expired"),
    };
    let NotifyTransport::Script { eligible, .. } = scripted else { unreachable!() };
    assert!(eligible(&job.req).is_some_and(|candidate| std::ptr::eq(candidate, c)));
    assert!(scripted.resolve(&job.req).is_none());
    let mut current_req = job.req.clone();
    current_req.key = ClientKey::of(c);
    assert!(scripted.resolve(&current_req).is_some_and(|candidate| std::ptr::eq(candidate, c)));
    let mut launched = false;
    StoreRunner::release_notify_stream_with(job, port, |req| scripted.resolve(req),
        |_, _, _| { launched = true; false });
    assert!(!launched);
    // Still-owned captured input observes ordinary failure; later host capture retires it.
    r.tick(|_| panic!("no immediate retry"));
    let until = delay(r.frame, c.id(), 0);
    r.quiet_until(until);
    let mut retries = 0;
    r.tick(|job| {
        retries += 1;
        let StoreJob::NotifyStream(job, port) = job else { panic!("Notify") };
        port.refused(job.addr);
    });
    assert_eq!(r.frame, until);
    assert_eq!(retries, 1);
}
