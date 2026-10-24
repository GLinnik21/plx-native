//! The application's reconcile frame, played by the data crate's tests: refill the registry's
//! answer, step, and start the reads the step sent, the way `Bridge::reconcile_pump` and the
//! application's `StoreRunner` do (the data crate cannot name either). The loopback-PMS tests run
//! their reads on a real worker thread through it, so a read can be held out while the frame loop
//! goes on, as in production.

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

/// Start every read-back batch the store has sent.
pub(super) fn start_jobs(stores: &mut Stores) {
    for job in stores.take_jobs_of(super::super::StoreId::ViewReconcile) {
        let StoreJob::ViewReconcile(job, port) = job else { unreachable!("another store's job on the reconcile outbox") };
        start(job, port);
    }
}

/// One frame of the reconcile pass: the registry's answer, the step, and the reads it sent started.
pub(super) fn frame(stores: &mut Stores, directory: &DirectorySnapshot) -> bool {
    let changed = stores.reconcile_pump(directory.view(), &registry_keys()).changed;
    start_jobs(stores);
    changed
}
