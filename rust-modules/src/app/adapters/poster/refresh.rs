//! Bounded stale-while-revalidate lane. Cached pixels reach the render queue immediately;
//! slow or offline servers cannot occupy either demand decoder or the main-thread store lock.
//! Only idle source periods start refreshes. A key gets at most one attempt per account epoch
//! within the bounded recent-attempt set, so revisiting stale art cannot hammer an offline PMS.

use crate::imgcache::DiskKey;
use std::collections::{HashSet, VecDeque};
use std::sync::{Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

const QUEUE_CAP: usize = 32;
const RECENT_CAP: usize = 8192;

struct Job {
    client: &'static crate::plex::Client,
    path: String,
    key: DiskKey,
    epoch: u64,
    token_gen: u32,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    seen: HashSet<DiskKey>,
    order: VecDeque<DiskKey>,
    epoch: u64,
    running: bool,
}
impl Queue {
    fn push(&mut self, job: Job) -> bool {
        if !self.running || job.epoch < self.epoch { return false; }
        if self.epoch != job.epoch {
            self.jobs.clear();
            self.seen.clear();
            self.order.clear();
            self.epoch = job.epoch;
        }
        if self.jobs.len() >= QUEUE_CAP || self.seen.contains(&job.key) { return false; }
        if self.order.len() == RECENT_CAP {
            if let Some(old) = self.order.pop_front() { self.seen.remove(&old); }
        }
        self.seen.insert(job.key.clone());
        self.order.push_back(job.key.clone());
        self.jobs.push_back(job);
        true
    }
}

static QUEUE: Mutex<Option<Queue>> = Mutex::new(None);
static READY: Condvar = Condvar::new();
static WORKER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

pub(super) fn enqueue(client: &'static crate::plex::Client, path: String, key: DiskKey, epoch: u64, token_gen: u32) {
    if epoch != crate::imgcache::generation() { return; }
    let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    if q.as_mut().is_some_and(|q| q.push(Job { client, path, key, epoch, token_gen })) {
        READY.notify_one();
    }
}

pub(super) fn init() {
    *QUEUE.lock().unwrap_or_else(|e| e.into_inner()) = Some(Queue { running: true, ..Default::default() });
    let worker = crate::task::spawn("image-refresh", run);
    if worker.is_none() {
        QUEUE.lock().unwrap_or_else(|e| e.into_inner()).as_mut().unwrap().running = false;
    }
    *WORKER.lock().unwrap_or_else(|e| e.into_inner()) = worker;
}

pub(super) fn shutdown() {
    if let Some(q) = QUEUE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        q.running = false;
        q.jobs.clear();
    }
    READY.notify_all();
    if let Some(h) = WORKER.lock().unwrap_or_else(|e| e.into_inner()).take() {
        crate::task::join("image-refresh", h);
    }
}

fn run() {
    loop {
        let job = {
            let mut guard = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                let Some(q) = guard.as_mut() else { return; };
                if !q.running { return; }
                if !q.jobs.is_empty() && super::store_idle() && crate::ui::tex::pending_bytes() == 0 {
                    break q.jobs.pop_front().unwrap();
                }
                guard = READY.wait_timeout(guard, Duration::from_millis(250))
                    .map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
            }
        };
        if job.epoch != crate::imgcache::generation() || !grant_is_current(&job) { continue; }
        // Failed refresh leaves both the cached file and already-published texture untouched.
        if let Some(bytes) = super::fetch_image(job.client, &job.path) {
            let (mut w, mut h) = (0, 0);
            let px = crate::img::img_decode_rgba(bytes.as_ptr(), bytes.len() as i32, &mut w, &mut h);
            if !px.is_null() {
                crate::img::img_free(px);
                crate::imgcache::write_at(job.epoch, &job.key, &bytes);
            }
        }
    }
}

/// Profile changes retain disk images but retire their old request credentials. A client can
/// also be replaced at a new origin without changing the account's disk-cache epoch.
fn grant_is_current(job: &Job) -> bool {
    crate::plex::client_for(job.client.id()).is_some_and(|live|
        std::ptr::eq(live, job.client) && live.token_gen() == job.token_gen)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn job(client: &'static crate::plex::Client, i: usize, epoch: u64) -> Job {
        let path = format!("/photo/:/transcode?url=%2Fthumb%2F{i}&width=250&height=375");
        Job { client, key: crate::imgcache::classify("fixture", &path).unwrap(), path, epoch, token_gen: client.token_gen() }
    }
    #[test]
    fn refreshes_are_bounded_deduplicated_and_retired_with_the_account() {
        let _guard = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test("refresh-fixture", "127.0.0.1", 1, "fixture", "fixture");
        let client = crate::plex::client_for(sid).unwrap();
        let old_grant = job(client, 0, 1);
        assert!(grant_is_current(&old_grant));
        let job = |i, epoch| job(client, i, epoch);
        let mut q = Queue { running: true, ..Default::default() };
        assert!(q.push(job(0, 1)));
        assert!(!q.push(job(0, 1)));
        q.jobs.pop_front();
        assert!(!q.push(job(0, 1)), "failed attempts must not spin on every revisit");
        for i in 1..=QUEUE_CAP { assert!(q.push(job(i, 1))); }
        assert!(!q.push(job(QUEUE_CAP + 1, 1)));
        assert!(q.push(job(0, 2)));
        assert_eq!(q.jobs.len(), 1);
        assert_eq!(q.seen.len(), 1);
        assert!(!q.push(job(1, 1)), "delayed old-account work cannot displace current refreshes");
        assert_eq!(q.jobs.len(), 1);
        assert_eq!(q.epoch, 2);
        q.running = false;
        assert!(!q.push(job(1, 2)));
        crate::plex::register_for_test("refresh-fixture", "127.0.0.1", 1, "changed-grant", "fixture");
        assert!(!grant_is_current(&old_grant), "queued old-profile credentials must never refresh");
        crate::plex::reset_servers_for_test();
    }
}
