//! The Collections count the library header shows beside "Collections" (`LibraryLike::collections_count`).
//!
//! One `type=18` `Size=0` probe per library section (`plx_plex` `Client::collections_count`), run
//! off the frame thread so the frame never waits on the server, and kept for the session: a refusal
//! (HTTP 401/403) is final for the credential it was asked under, a count is re-read after
//! [`REFRESH_MS`], and a probe that learned nothing (transport failure, 404) is retried after
//! [`RETRY_MS`] while the header keeps the Collections control off the row (it is drawn only with a
//! known count). The cache holds one small entry for every section of every server asked about
//! (friends' shares included) and never evicts; the header reads it every frame for the library it
//! names, so a lookup is a short lock that starts at most one probe per section at a time.
//!
//! **Every section is warmed when the directory lands** ([`warm`], called by the bridge on a frame
//! whose browse publication or server roster changed): each distinct `(server, section key)` is
//! asked through the same lookup and the same cache, one `Size=0` read each, **one at a time on ONE
//! worker** ([`Walk`]), so a directory of many sections is a trickle of requests and not a burst of
//! threads. A library switch usually finds its answer already there and the control is on the row in
//! the first frame of the switch. The section the header names is never behind that walk: its own
//! lookup starts its probe at once, and the walk, reaching it later, finds it asked and passes. A
//! section the walk could not ask (no client yet) is asked when the header first names it.
//!
//! A controlled run (record / replay) never probes: the worker's landing time is not on the tape,
//! so the header would differ between a recording and its replay. Its control stays off the row
//! (nothing is known), as it does while a probe has learned nothing. A unit test never probes
//! either: it reads only the answers its rig seeded ([`seed_for_test`]).
use plx_plex::plex::collections::CollectionsCount;
use plx_plex::plex::{Client, ServerId};
use plx_screens::registry::Forbidden;
use std::collections::VecDeque;
use std::sync::Mutex;

/// How long a known count is trusted before it is read again (collections made on another device).
const REFRESH_MS: u32 = 300_000;
/// How long to wait before asking again after an answer that said nothing.
const RETRY_MS: u32 = 20_000;

/// `(server, grant epoch, section key)`. The epoch is the client's [`Client::grant_epoch`], the
/// documented key for a cache of what a server SERVED: it moves with a new client (a re-pointed
/// server slot) and across every revocation (a profile switch, a sign-out, a withdrawn plaintext
/// grant), so a count or a refusal is only ever served to the identity it was asked for, a final
/// 401 is asked again for the next one, and a sign-out or a new client starts from nothing. A live
/// token refreshed in place keeps it, and with it the answers.
type Key = (u16, u32, i64);

/// A section to warm: its server and key.
type Target = (ServerId, i64);

/// The cache key of `section` on `sid`, asked through `client`.
fn key_of(sid: ServerId, client: &Client, section: i64) -> Key { (sid.raw(), client.grant_epoch(), section) }

#[derive(Clone, Copy)]
struct Entry {
    /// What the last probe that said something said.
    answer: Option<CollectionsCount>,
    /// The frame clock (`Tick::ms`) when the probe that produced `answer` (or the last empty one)
    /// was first seen to have returned. Stamped by the next lookup, never by the worker: the app
    /// reads no wall clock.
    at: Option<u32>,
    /// A worker returned and the next lookup owes `at` its stamp.
    settled: bool,
    probing: bool,
}

static CACHE: Mutex<Vec<(Key, Entry)>> = Mutex::new(Vec::new());

fn lock() -> std::sync::MutexGuard<'static, Vec<(Key, Entry)>> {
    CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Should `entry` be probed now? Pure, so the schedule is testable without a clock or a server.
/// A stamp NEWER than `now` (the warm walk reads the clock of the frame that started it, and a
/// later frame may have stamped since) is no age at all.
fn wants_probe(entry: &Entry, now: u32) -> bool {
    if entry.probing || entry.settled { return false; }
    let age = entry.at.map(|at| match now.wrapping_sub(at) {
        behind if behind > u32::MAX / 2 => 0,
        age => age,
    });
    match (entry.answer, age) {
        (_, None) => true,
        (Some(CollectionsCount::Forbidden), _) => false,
        (Some(CollectionsCount::Count(_)), Some(age)) => age >= REFRESH_MS,
        (Some(CollectionsCount::Unknown) | None, Some(age)) => age >= RETRY_MS,
    }
}

/// What the header may show for `answer`: `None` = not known, `Some(Err)` = refused.
fn shown(answer: Option<CollectionsCount>) -> Option<Result<u32, Forbidden>> {
    match answer? {
        CollectionsCount::Count(n) => Some(Ok(n)),
        CollectionsCount::Forbidden => Some(Err(Forbidden)),
        CollectionsCount::Unknown => None,
    }
}

/// Record what a finished probe learned, then ask for a frame: a resting page runs no ticks, so
/// without the wake the header would keep the control off the row after its count landed (or keep
/// a control the answer says to hide) until the next input.
fn land(key: Key, learned: CollectionsCount) {
    {
        let mut cache = lock();
        if let Some((_, entry)) = cache.iter_mut().find(|(seen, _)| *seen == key) {
            // An answer that said nothing never replaces one that did.
            if learned != CollectionsCount::Unknown || entry.answer.is_none() { entry.answer = Some(learned); }
            entry.settled = true;
            entry.probing = false;
        }
    }
    plx_machine::idle::invalidate();
}

/// The cache's answer for `key`, and whether the schedule says the caller now OWNS a probe of it
/// (the entry is marked as being probed, so nobody else starts one). The one place an entry is
/// made, stamped and claimed, for the on-demand lookup and the warm walk alike: whoever claims
/// first probes, the other finds it taken.
fn claim(key: Key, now: u32) -> (Option<CollectionsCount>, bool) {
    let mut cache = lock();
    let index = match cache.iter().position(|(seen, _)| *seen == key) {
        Some(index) => index,
        None => {
            cache.push((key, Entry { answer: None, at: None, settled: false, probing: false }));
            cache.len() - 1
        }
    };
    let entry = &mut cache[index].1;
    if entry.settled { entry.settled = false; entry.at = Some(now); }
    let launch = wants_probe(entry, now);
    if launch { entry.probing = true; }
    (entry.answer, launch)
}

/// A probe that could not be started (the OS refused the thread): hand the claim back as a probe
/// that learned nothing, so the schedule asks again after [`RETRY_MS`].
fn give_up(key: Key) {
    if let Some((_, entry)) = lock().iter_mut().find(|(seen, _)| *seen == key) {
        entry.probing = false;
        entry.settled = true;
    }
}

/// The GET behind a claimed `key`, blocking: runs on a worker, never on the frame thread.
fn probe(client: &Client, key: Key) {
    let learned = std::panic::catch_unwind(|| client.collections_count(key.2))
        .unwrap_or(CollectionsCount::Unknown);
    land(key, learned);
}

/// The count for the directory's section `section` (an index into its table, which names the
/// server as well as the key), starting its probe the first time it is asked for. `now` is the
/// frame clock in milliseconds. `None` while it is not known.
pub(crate) fn count(
    directory: plx_data::stores::browse::DirectoryView<'_>,
    section: usize,
    now: u32,
) -> Option<Result<u32, Forbidden>> {
    // A controlled run (replay, record) never reaches the network: the probe would be a request the
    // run's own script did not make, and its landing time is not on the tape.
    if plx_data::stores::tape::active() { return None; }
    let section = directory.sections().get(section)?;
    let sid = section.sid?;
    // A unit test never reaches it either, and its rig has no server to ask: it reads what the rig
    // seeded (`seed_for_test`), and a section it seeded nothing for is a probe that has not answered.
    if cfg!(test) { return seeded(sid, section.key); }
    ask(sid, section.key, now)
}

/// What a unit test's rig seeded for `(sid, key)`; nothing outside a test build.
fn seeded(sid: ServerId, key: i64) -> Option<Result<u32, Forbidden>> {
    #[cfg(test)]
    {
        let seeds = SEEDS.lock().unwrap_or_else(|e| e.into_inner());
        shown(seeds.iter().find(|(seen, _)| *seen == (sid.raw(), key)).map(|(_, answer)| *answer))
    }
    #[cfg(not(test))]
    {
        let _ = (sid, key);
        None
    }
}

/// What the test rig has seeded: `(server, section key)` and the probe's answer for it.
#[cfg(test)]
static SEEDS: Mutex<Vec<((u16, i64), CollectionsCount)>> = Mutex::new(Vec::new());

/// Seed the answer the probe of `(sid, key)` gives a unit test (replacing an earlier one): a
/// `Count(n)` puts the control on the row, `Count(0)` or `Forbidden` hides it, `Unknown` is a probe
/// that has learned nothing.
#[cfg(test)]
pub(crate) fn seed_for_test(sid: ServerId, key: i64, answer: CollectionsCount) {
    let mut seeds = SEEDS.lock().unwrap_or_else(|e| e.into_inner());
    seeds.retain(|(seen, _)| *seen != (sid.raw(), key));
    seeds.push(((sid.raw(), key), answer));
}

/// Forget every seeded answer (a rig's end, and the start of the next one).
#[cfg(test)]
pub(crate) fn clear_seeds_for_test() {
    SEEDS.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// What one landing of the directory is: its epoch, the table's generation and size, and the server
/// roster's generation. The same stamp means the same sections on the same clients, so the warm pass
/// has nothing new to ask; a server re-pointed at a new client moves the last (its cached answers
/// are of the client left, [`Key`]).
type Landing = (u32, u32, usize, u32);

/// The sections a landed directory asks about: each distinct `(server, section key)` once, in
/// table order. A section with no server (a fixture's) has nothing to ask.
fn targets(directory: plx_data::stores::browse::DirectoryView<'_>) -> Vec<Target> {
    let mut seen: Vec<Target> = Vec::new();
    for section in directory.sections() {
        if let Some(sid) = section.sid {
            if !seen.contains(&(sid, section.key)) { seen.push((sid, section.key)); }
        }
    }
    seen
}

/// The decision behind [`warm`], with the asking passed in so it is testable without a server:
/// when `directory` is a landing `warmed` has not seen (at server roster generation `roster`), ask
/// `each` for every target once and remember the landing. The same landing again asks nothing.
fn warm_with(
    directory: plx_data::stores::browse::DirectoryView<'_>,
    roster: u32,
    warmed: &mut Option<Landing>,
    mut each: impl FnMut(ServerId, i64),
) {
    let Some(epoch) = directory.epoch() else { return };
    let landing = (epoch, directory.sections_gen(), directory.sections().len(), roster);
    if *warmed == Some(landing) { return; }
    *warmed = Some(landing);
    for (sid, key) in targets(directory) { each(sid, key); }
}

/// The last landing [`warm`] asked about.
static WARMED: Mutex<Option<Landing>> = Mutex::new(None);

/// Is a warm pass worth making on a frame whose browse publication did (`browse_changed`) or did
/// not change? A landing is a browse publication, but a server re-pointed at a new client moves
/// only the roster, so that is read too (one atomic).
pub(crate) fn warm_due(browse_changed: bool) -> bool {
    due(browse_changed, *WARMED.lock().unwrap_or_else(|e| e.into_inner()), plx_plex::plex::server_roster_gen())
}

/// [`warm_due`] with the last landing and the roster generation passed in.
fn due(browse_changed: bool, warmed: Option<Landing>, roster: u32) -> bool {
    browse_changed || warmed.is_none_or(|landing| landing.3 != roster)
}

/// Warm the probe for every section of `directory`, once per landing (see the module doc). The
/// bridge calls it on a frame [`warm_due`] names; a call that is not a new landing costs a
/// comparison.
pub(crate) fn warm(directory: plx_data::stores::browse::DirectoryView<'_>, now: u32) {
    if cfg!(test) || plx_data::stores::tape::active() { return; }
    let mut asked = Vec::new();
    {
        let mut warmed = WARMED.lock().unwrap_or_else(|e| e.into_inner());
        warm_with(directory, plx_plex::plex::server_roster_gen(), &mut warmed, |sid, key| asked.push((sid, key)));
    }
    walk(&WALK, asked, now, |job| plx_base::task::spawn_small("collections-warm", job), warm_one);
}

/// **The warm pass's sections still to ask, walked by ONE worker**: a directory of many servers'
/// sections (friends' shares included) is read one section at a time, never one thread and one GET
/// each at once, and a landing that arrives while the walk runs joins the queue the same worker
/// drains.
struct Walk {
    queue: VecDeque<Target>,
    /// A worker is draining `queue` (it clears this itself, under the lock, when `queue` is dry).
    running: bool,
    /// The frame clock of the latest landing, which the schedule reads for the sections walked.
    now: u32,
}

impl Walk {
    const fn new() -> Self { Self { queue: VecDeque::new(), running: false, now: 0 } }

    /// Queue the `targets` not already waiting. `true` = no worker was running and the caller must
    /// start one (it is counted as running from here).
    fn push(&mut self, targets: impl IntoIterator<Item = Target>, now: u32) -> bool {
        for target in targets {
            if !self.queue.contains(&target) { self.queue.push_back(target); }
        }
        self.now = now;
        let start = !self.running && !self.queue.is_empty();
        if start { self.running = true; }
        start
    }

    /// The worker's next section, with the clock to schedule it by; `None` when the queue is dry,
    /// which is also the moment the walk stops being `running`: the check and the clearing are one
    /// lock, so a landing that arrives after it starts a worker of its own.
    fn next(&mut self) -> Option<(Target, u32)> {
        let target = self.queue.pop_front();
        if target.is_none() { self.running = false; }
        target.map(|target| (target, self.now))
    }

    /// The worker could not be started: nothing will drain the queue.
    fn abandon(&mut self) {
        self.queue.clear();
        self.running = false;
    }
}

static WALK: Mutex<Walk> = Mutex::new(Walk::new());

/// Queue `targets` on `queue` and, if no worker is draining it, start the one: `spawn` runs the job
/// on a worker (`false` = the OS refused the thread) and `each` asks one section.
fn walk<F>(
    queue: &'static Mutex<Walk>,
    targets: Vec<Target>,
    now: u32,
    spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> bool,
    each: F,
) where F: Fn(Target, u32) + Send + 'static {
    let lock = move || queue.lock().unwrap_or_else(|e| e.into_inner());
    if !lock().push(targets, now) { return; }
    let started = spawn(Box::new(move || {
        // The next section is taken in its own statement: a `while let ... = lock().next()` keeps
        // the guard to the end of the body, and `each` is a blocking GET the frame thread's `warm`
        // would then wait out on this same lock.
        loop {
            let next = lock().next();
            let Some((target, now)) = next else { break };
            // A section that panics is a section not asked, not a walk that stops.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| each(target, now)));
        }
    }));
    if !started { lock().abandon(); }
}

/// The walk's step: ask one section through the same cache as the header's own lookup. A section
/// the header (or an earlier landing) already asked is taken and passes; a server with no client
/// yet is asked when the header first names it.
fn warm_one((sid, section): Target, now: u32) {
    let Some(client) = plx_plex::plex::client_for(sid) else { return };
    let key = key_of(sid, client, section);
    if claim(key, now).1 { probe(client, key); }
}

/// The on-demand answer for `(sid, section)` from the cache, starting its probe AT ONCE (a thread of
/// its own, never behind the warm walk) when the schedule says so.
fn ask(sid: ServerId, section: i64, now: u32) -> Option<Result<u32, Forbidden>> {
    let client = plx_plex::plex::client_for(sid)?;
    let key = key_of(sid, client, section);
    let (answer, launch) = claim(key, now);
    if launch && !plx_base::task::spawn_small("collections-count", move || probe(client, key)) {
        give_up(key);
    }
    shown(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(answer: Option<CollectionsCount>, age_s: u32, probing: bool, now: u32) -> Entry {
        Entry { answer, at: Some(now - age_s * 1000), settled: false, probing }
    }

    #[test]
    fn a_section_is_probed_once_until_its_answer_goes_stale() {
        let now = 100_000_000;
        let fresh = Entry { answer: None, at: None, settled: false, probing: false };
        assert!(wants_probe(&fresh, now), "never asked: ask");
        assert!(!wants_probe(&Entry { probing: true, ..fresh }, now), "one worker per section");
        assert!(!wants_probe(&Entry { settled: true, ..fresh }, now), "a returned worker is stamped first");
        let count = Some(CollectionsCount::Count(3));
        assert!(!wants_probe(&entry(count, 10, false, now), now), "a count is kept");
        assert!(wants_probe(&entry(count, 301, false, now), now), "…and re-read when old");
        assert!(!wants_probe(&entry(Some(CollectionsCount::Forbidden), 9_000, false, now), now), "a refusal is final");
        assert!(!wants_probe(&entry(None, 5, false, now), now), "an empty answer waits before a retry");
        assert!(wants_probe(&entry(None, 21, false, now), now));
        // The warm walk reads the clock of the frame that began it; a later frame may have stamped
        // since. A stamp from the walk's future is no age, not a wrapped-around very old one.
        let ahead = Entry { answer: count, at: Some(now + 5_000), settled: false, probing: false };
        assert!(!wants_probe(&ahead, now), "a count stamped after the clock being read is fresh");
        assert!(!wants_probe(&Entry { answer: None, ..ahead }, now), "and so is an empty answer");
    }

    fn table(sections: &[(Option<u16>, i64)]) -> plx_data::stores::browse::DirectorySnapshot {
        use plx_data::stores::browse::{DirectorySnapshot, SectionView, SrcRow};
        DirectorySnapshot::fixture(3, 0, sections.iter().enumerate().map(|(i, &(sid, key))| SectionView {
            sid: sid.map(plx_plex::plex::ServerId::from_raw),
            key,
            kind: plx_data::browse::SecKind::Movie,
            row: SrcRow { section: i, title: format!("Library {i}"), pinned: true, ..Default::default() },
        }).collect())
    }

    /// **A landed directory asks about EVERY section**, not only the current one: one read per
    /// distinct `(server, key)`, in table order (the same key on two servers is two sections), none
    /// for a section with no server, and nothing again for the same landing. A directory that is
    /// replaced (a new epoch) is a new landing and is asked again, the cache keeping the answers
    /// the schedule still trusts.
    #[test]
    fn a_landed_directory_warms_every_section_once() {
        let snapshot = table(&[(Some(1), 5), (Some(1), 6), (Some(2), 5), (None, 9), (Some(1), 5)]);
        let mut warmed = None;
        let mut asked = Vec::new();
        warm_with(snapshot.view(), 1, &mut warmed, |sid, key| asked.push((sid.raw(), key)));
        assert_eq!(asked, [(1, 5), (1, 6), (2, 5)], "every distinct section of every server, once");
        warm_with(snapshot.view(), 1, &mut warmed, |sid, key| asked.push((sid.raw(), key)));
        assert_eq!(asked.len(), 3, "the same landing asks nothing again");
        let later = table(&[(Some(1), 5), (Some(1), 6), (Some(2), 5), (Some(2), 7)]);
        let mut again = Vec::new();
        warm_with(later.view(), 1, &mut warmed, |sid, key| again.push((sid.raw(), key)));
        assert_eq!(again, [(1, 5), (1, 6), (2, 5), (2, 7)], "a table of another size is a new landing");
        let mut none = Vec::new();
        warm_with(plx_data::stores::browse::DirectoryView::empty_for_test(), 1, &mut warmed, |sid, key| none.push((sid.raw(), key)));
        assert!(none.is_empty(), "no directory yet, nothing to ask");
    }

    /// **A server re-pointed at a new client is a new landing** even when the table did not move
    /// (the roster generation is in the stamp): the cached answers are of the client left, and the
    /// warm pass asks the new one. The bridge calls the pass on such a frame too ([`due`]).
    #[test]
    fn a_re_pointed_server_warms_again_and_the_bridge_calls_the_pass_for_it() {
        let snapshot = table(&[(Some(1), 5), (Some(2), 5)]);
        let mut warmed = None;
        let mut asked = Vec::new();
        warm_with(snapshot.view(), 7, &mut warmed, |sid, key| asked.push((sid.raw(), key)));
        assert_eq!(asked.len(), 2);
        assert!(!due(false, warmed, 7), "nothing changed: the frame skips the pass");
        assert!(due(true, warmed, 7), "a browse publication calls it");
        assert!(due(false, warmed, 8), "so does a roster that moved under an unchanged table");
        assert!(due(false, None, 7), "and so does a frame before any landing");
        warm_with(snapshot.view(), 8, &mut warmed, |sid, key| asked.push((sid.raw(), key)));
        assert_eq!(asked.len(), 4, "the same table on a new roster is asked again");
    }

    fn jobs() -> std::sync::Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>> { Default::default() }

    /// **The warm pass is ONE worker**: a landing that finds a worker alive joins its queue instead
    /// of starting another, the worker asks every queued section once, in order, with the latest
    /// landing's clock, and stops when the queue is dry (a later landing starts a new one).
    #[test]
    fn the_warm_walk_is_one_worker_that_asks_every_section_in_order() {
        use std::sync::Arc;
        static QUEUE: Mutex<Walk> = Mutex::new(Walk::new());
        let target = |sid: u16, key: i64| (ServerId::from_raw(sid), key);
        let asked: Arc<Mutex<Vec<(u16, i64, u32)>>> = Default::default();
        let each = || { let asked = Arc::clone(&asked); move |(sid, key): Target, now: u32| asked.lock().unwrap().push((sid.raw(), key, now)) };
        let started = jobs();
        let spawn = || { let started = Arc::clone(&started); move |job: Box<dyn FnOnce() + Send>| { started.lock().unwrap().push(job); true } };

        walk(&QUEUE, vec![target(1, 5), target(1, 6), target(2, 5)], 100, spawn(), each());
        assert_eq!(started.lock().unwrap().len(), 1, "the first landing starts a worker");
        assert!(asked.lock().unwrap().is_empty(), "and the frame thread asked nothing itself");
        walk(&QUEUE, vec![target(2, 5), target(2, 7)], 200, spawn(), each());
        assert_eq!(started.lock().unwrap().len(), 1, "a landing during the walk starts no second worker");

        let worker = started.lock().unwrap().pop().unwrap();
        worker();
        assert_eq!(*asked.lock().unwrap(), [(1, 5, 200), (1, 6, 200), (2, 5, 200), (2, 7, 200)],
            "every section once, in order (the repeated one is not asked twice), by the latest clock");

        walk(&QUEUE, vec![target(1, 5)], 300, spawn(), each());
        assert_eq!(started.lock().unwrap().len(), 1, "a dry queue lets the next landing start a worker");
        started.lock().unwrap().pop().unwrap()();
        assert_eq!(asked.lock().unwrap().last(), Some(&(1, 5, 300)));
    }

    /// **The worker never holds the queue's lock while it asks a section**: `each` is a blocking GET,
    /// and the frame thread's `warm` -> `walk` locks the same queue to push, so a guard that lived
    /// across `each` (a `while let ... = lock().next()` keeps its temporary to the loop's end) made
    /// the frame wait out a whole network round-trip (225 ms at boot on the TV).
    #[test]
    fn the_warm_worker_does_not_hold_the_queue_while_it_asks_a_section() {
        use std::sync::{mpsc, Arc};
        static QUEUE: Mutex<Walk> = Mutex::new(Walk::new());
        let (entered_tx, entered) = mpsc::channel::<()>();
        let (release, released) = mpsc::channel::<()>();
        let released = Arc::new(Mutex::new(released));
        let spawn = |job: Box<dyn FnOnce() + Send>| { std::thread::spawn(job); true };
        let each = { let released = Arc::clone(&released); move |_: Target, _| {
            entered_tx.send(()).unwrap();
            let _ = released.lock().unwrap().recv();
        } };
        walk(&QUEUE, vec![(ServerId::from_raw(1), 5)], 1, spawn, each);
        entered.recv_timeout(std::time::Duration::from_secs(10)).expect("the worker reached its section");
        let free = !matches!(QUEUE.try_lock(), Err(std::sync::TryLockError::WouldBlock));
        release.send(()).unwrap();
        assert!(free, "the queue is locked while a section is being asked, so the frame thread would wait on the GET");
    }

    /// A worker the OS refuses leaves no queue behind it, so the next landing starts one; and a
    /// section that panics when asked does not stop the sections after it.
    #[test]
    fn the_warm_walk_survives_a_refused_thread_and_a_section_that_panics() {
        use std::sync::Arc;
        static QUEUE: Mutex<Walk> = Mutex::new(Walk::new());
        let target = |key: i64| (ServerId::from_raw(3), key);
        let asked: Arc<Mutex<Vec<i64>>> = Default::default();
        let each = || { let asked = Arc::clone(&asked); move |(_, key): Target, _| {
            if key == 2 { panic!("a section the server could not answer"); }
            asked.lock().unwrap().push(key);
        } };
        walk(&QUEUE, vec![target(1), target(2)], 1, |_job| false, each());
        let started = jobs();
        let spawn = { let started = Arc::clone(&started); move |job: Box<dyn FnOnce() + Send>| { started.lock().unwrap().push(job); true } };
        walk(&QUEUE, vec![target(3), target(2), target(4)], 2, spawn, each());
        assert_eq!(started.lock().unwrap().len(), 1, "the refused worker left no 'running' behind");
        started.lock().unwrap().pop().unwrap()();
        assert_eq!(*asked.lock().unwrap(), [3, 4], "the refused landing's sections are gone, the panicking one did not stop the walk");
    }

    /// **The section the header names is never behind the walk.** The walk queues a section and
    /// claims it only when its turn comes; the header's own lookup claims it at once (and starts its
    /// probe), and the walk, reaching it later, finds it taken. The other way round, the lookup
    /// finds the walk's probe in flight and starts no second one.
    #[test]
    fn the_section_the_header_names_is_probed_at_once_whatever_the_walk_holds() {
        let (by_header, by_walk): (Key, Key) = ((u16::MAX, 11, -11), (u16::MAX, 12, -12));
        let now = 1_000;
        assert!(claim(by_header, now).1, "the lookup starts the probe of a section the walk has not reached");
        assert!(!claim(by_header, now + 16).1, "and the walk, reaching it later, finds it taken");
        assert!(claim(by_walk, now).1, "the walk starts the probe of the section it reached first");
        assert!(!claim(by_walk, now + 16).1, "and the lookup waits for that one");
        lock().retain(|(key, _)| *key != by_header && *key != by_walk);
    }

    /// **A count is served only to the identity it was asked for** ([`Key`]): a server re-pointed
    /// at a new client and a profile switch (the token revoked, then another admitted) each start
    /// from nothing, so another profile's count is never shown for 300 s and a final 401 is asked
    /// again; a live token refreshed in place is the same identity and keeps its answers.
    #[test]
    fn the_cache_is_keyed_on_the_identity_the_answer_was_asked_for() {
        let _guard = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("probe-key", "127.0.0.1", 9, "tok-a", "fixture");
        let client = plx_plex::plex::client_for(sid).unwrap();
        let first = key_of(sid, client, 5);
        client.set_token("tok-a2");
        assert_eq!(key_of(sid, client, 5), first, "a live token replaced by another live one is the same identity");
        client.set_token("");
        client.set_token("tok-b");
        assert_ne!(key_of(sid, client, 5), first, "a revocation then another admission is a new identity");
        let admitted = key_of(sid, client, 5);
        let again = plx_plex::plex::register_for_test("probe-key", "127.0.0.1", 10, "tok-b", "fixture");
        let replaced = plx_plex::plex::client_for(again).unwrap();
        assert_ne!(key_of(again, replaced, 5), admitted, "a new client for the server starts from nothing");
        plx_plex::plex::reset_servers_for_test();
    }

    /// What a unit test's rig seeds is what the header reads, and nothing else is known.
    #[test]
    fn a_rig_reads_only_what_it_seeded() {
        let _guard = plx_base::testlock::serial();
        clear_seeds_for_test();
        let snapshot = table(&[(Some(u16::MAX), 5), (Some(u16::MAX), 6), (Some(u16::MAX), 7), (None, 8)]);
        let sid = ServerId::from_raw(u16::MAX);
        let read = |section| count(snapshot.view(), section, 0);
        assert_eq!(read(0), None, "an unseeded section is a probe that has not answered");
        seed_for_test(sid, 5, CollectionsCount::Count(4));
        seed_for_test(sid, 6, CollectionsCount::Count(0));
        seed_for_test(sid, 7, CollectionsCount::Forbidden);
        assert_eq!(read(0), Some(Ok(4)));
        assert_eq!(read(1), Some(Ok(0)), "none");
        assert!(matches!(read(2), Some(Err(Forbidden))), "a refusal");
        assert_eq!(read(3), None, "a section with no server has nothing to ask");
        seed_for_test(sid, 5, CollectionsCount::Count(9));
        assert_eq!(read(0), Some(Ok(9)), "a later seed replaces an earlier one");
        clear_seeds_for_test();
        assert_eq!(read(0), None);
    }

    #[test]
    fn a_landed_answer_wakes_a_frame() {
        let key: Key = (u16::MAX, u32::MAX, -7);
        lock().push((key, Entry { answer: None, at: None, settled: false, probing: true }));
        let _ = plx_machine::idle::take_local_damage();
        land(key, CollectionsCount::Count(4));
        assert!(plx_machine::idle::take_local_damage() > 0, "the landing must request a frame");
        let mut cache = lock();
        let at = cache.iter().position(|(k, _)| *k == key).unwrap();
        let entry = cache.remove(at).1;
        assert_eq!(entry.answer, Some(CollectionsCount::Count(4)));
        assert!(entry.settled && !entry.probing);
    }

    #[test]
    fn the_header_sees_a_count_a_refusal_or_nothing() {
        assert_eq!(shown(None), None);
        assert_eq!(shown(Some(CollectionsCount::Unknown)), None);
        assert_eq!(shown(Some(CollectionsCount::Count(0))), Some(Ok(0)));
        assert!(matches!(shown(Some(CollectionsCount::Forbidden)), Some(Err(Forbidden))));
    }
}
