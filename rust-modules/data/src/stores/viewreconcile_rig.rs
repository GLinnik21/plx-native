//! The application's reconcile frame, played by the data crate's tests: refill the registry's
//! answer, step, and start the writes and reads the step sent, the way `Bridge::reconcile_pump` and
//! the application's `StoreRunner` do (the data crate cannot name either). The loopback-PMS tests
//! run their writes and reads on a real worker thread through it, so a read can be held out while
//! the frame loop goes on, as in production.

use super::*;
use crate::stores::browse::DirectorySnapshot;
use crate::stores::Stores;

/// The registry's answer as of now: what the Bridge's refill makes of it before a step.
pub(super) fn registry_keys() -> ClientKeys {
    ClientKeys::of(plx_plex::plex::server_ids().filter_map(|sid| plx_plex::plex::client_for(sid).map(|c| (sid, c))))
}

/// What the runner does with one read: start it on a worker through the client the step decided on,
/// if the registry still holds that client, and refuse it otherwise (or when no thread can start).
pub(super) fn start(job: Job<ReconcileReq>, port: ReconcilePort) {
    let (sid, key) = job.req.server();
    let Job { addr, req } = job;
    let Some(client) = plx_plex::plex::client_for(sid).filter(|client| ClientKey::of(client) == key) else {
        return port.refused(addr);
    };
    let worker = port.clone();
    if !plx_base::task::spawn_small("viewreconcile", move || finish_reconcile_job(&worker, addr, client, req)) {
        port.refused(addr);
    }
}

/// What the runner does with one view-state write: start it on a worker through the origin's client
/// the step decided on, if the registry still holds that client, and refuse it otherwise (or when
/// no thread can start). The fan-out reaches the other copies through the job's own snapshot.
pub(super) fn start_view_write(job: Job<crate::viewstate::ViewWriteReq>, port: crate::viewstate::ViewWritePort) {
    let (sid, key) = job.req.server();
    let Job { addr, req } = job;
    let Some(origin) = plx_plex::plex::client_for(sid).filter(|client| ClientKey::of(client) == key) else {
        return port.refused(addr);
    };
    let worker = port.clone();
    if !plx_base::task::spawn_small("viewstate", move || {
        crate::viewstate::finish_view_write_job(&worker, addr, origin, plx_plex::plex::client_for, req)
    }) {
        port.refused(addr);
    }
}

/// Start every write and every read-back batch the stores have sent, in the order the application
/// drains them (`Stores::RECONCILE_FILLS`).
pub(super) fn start_jobs(stores: &mut Stores) {
    for &id in Stores::RECONCILE_FILLS {
        for job in stores.take_jobs_of(id) {
            match job {
                StoreJob::ViewWrite(job, port) => start_view_write(job, port),
                StoreJob::ViewReconcile(job, port) => start(job, port),
                _ => unreachable!("another store's job on the {id:?} outbox"),
            }
        }
    }
}

/// One frame of the reconcile pass: the registry's answer, the step, and the writes and reads it sent
/// started.
pub(super) fn frame(stores: &mut Stores, directory: &DirectorySnapshot) -> bool {
    let changed = stores.reconcile_pump(directory.view(), &registry_keys(), 0).changed;
    start_jobs(stores);
    changed
}
