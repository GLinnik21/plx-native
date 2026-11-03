//! The Library's source discovery as a store job: the read that lists a server's sections and the
//! item count of each, which the Library pill and the Sources list are built from.
//!
//! A Browse step that wants a source discovered raises the discovery claim, takes the server's
//! [`ClientKey`] from the borrowed [`ClientKeys`] and **leaves the read as a job on the store's
//! outbox** ([`StoreJob::BrowseDiscovery`]). Nothing here starts a worker or reads the server
//! registry: the application's runner starts the job, only if the registry still holds the client
//! the step decided on ([`BrowseDiscoveryReq::server`]), and hands the body that client. The body
//! reads the server through it and posts ONE answer to the job's port
//! ([`finish_browse_discovery_job`]); a read that could not start is refused instead
//! ([`Port::refused`]), which lowers the claim in the call that refused it, and the store's next
//! step settles it ([`super::BrowseState`], `discovery_settle`) with the backoff a refused start
//! always had.
//!
//! The answer lands in the adapter's `src_result` exactly as the worker's did, and the landing
//! (`land_discovery_owned_with_gate`, the controlled run's recorded result) is untouched. It keeps
//! its identity commit (`commit_reachability_if_current`): the answer carries the client the body
//! ran through and the key's credential generation, so a client that was re-pointed or retokened
//! while the read was out is not the server the answer is blamed on.
//!
//! The kind is [`crate::stores::Admission::Home`]: a controlled Home run records its launch in the
//! run's ordered ledger, with the descriptor [`BrowseDiscoveryReq::admission`] (the key set the
//! replay fixtures hold), and starts it with no record anywhere else.

use super::{project_sections, SectionQuery, SrcJob, SrcLanding, SrcWhat};
use plx_machine::job::Port;
use plx_machine::machine::Addr;
use plx_plex::plex::{Client, ClientKey, ServerId};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

/// The one request a [`StoreJob::BrowseDiscovery`](crate::stores::StoreJob::BrowseDiscovery)
/// carries: everything the body needs, captured by the step. `key` is the client the step decided
/// on; the runner starts the request only if the registry still holds that client at the launch.
#[derive(Debug, PartialEq)]
pub struct BrowseDiscoveryReq {
    /// The section table's epoch when the step asked: a landing across a `Reset` is dropped.
    epoch: u32,
    /// The source's index in the table when the step asked.
    source: usize,
    sid: ServerId,
    key: ClientKey,
    job: SrcJob,
    /// Whether the server's own name is asked for (the source has none yet).
    want_name: bool,
}

impl BrowseDiscoveryReq {
    pub(super) fn new(epoch: u32, source: usize, sid: ServerId, key: ClientKey, job: SrcJob, want_name: bool) -> Self {
        Self { epoch, source, sid, key, job, want_name }
    }

    /// The server to ask and the client the step decided to ask it through.
    pub fn server(&self) -> (ServerId, ClientKey) {
        (self.sid, self.key)
    }

    /// What a controlled Home run records for this launch (`HomeIo::admit` adds `admitted`). The
    /// keys and their order are the recorded shape: a replay compares it byte for byte, and
    /// `bootstrap::validate_admission` refuses any other key set. `sid`, `client` and `token_gen`
    /// are the registry's answer the step was handed (`ClientKeys::key`), which is what the client
    /// the step used to read itself said.
    pub fn admission(&self) -> serde_json::Value {
        serde_json::json!({
            "epoch": self.epoch,
            "source": self.source,
            "sid": self.sid.raw(),
            "client": self.key.instance,
            "token_gen": self.key.token,
            "name": self.want_name,
            "sections": matches!(self.job, SrcJob::Sections),
            "counts": match &self.job {
                SrcJob::Sections => Vec::new(),
                SrcJob::Counts(keys) => keys.clone(),
            },
        })
    }
}

/// A discovery request's answer, opaque to the runner: the table epoch and source it was asked
/// for, and what the server said.
pub struct BrowseDiscoveryAnswer(u32, usize, SrcLanding);

/// Where a discovery request's ONE answer goes: the store's mailbox, captured with the job when it
/// was emitted. A store that has since been `Reset` holds a new adapter, and this port still
/// reaches the one the request was made from, the retired one nobody pumps. Two ports are equal
/// when they reach the same adapter.
#[derive(Clone)]
pub struct BrowseDiscoveryPort(Arc<super::BrowseAdapter>);

impl BrowseDiscoveryPort {
    pub(super) fn of(adapter: &Arc<super::BrowseAdapter>) -> Self {
        Self(Arc::clone(adapter))
    }
}

impl PartialEq for BrowseDiscoveryPort {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for BrowseDiscoveryPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BrowseDiscoveryPort({:p})", Arc::as_ptr(&self.0))
    }
}

impl Port<BrowseDiscoveryAnswer> for BrowseDiscoveryPort {
    /// The request's answer (a panicked read answers its job's failure), written to the mailbox
    /// unconditionally, as the worker wrote it. The claim is NOT lowered here: it drops when the
    /// answer is applied (`apply_discovery`), after its epoch check.
    fn done(&self, _addr: Addr, out: BrowseDiscoveryAnswer) {
        let BrowseDiscoveryAnswer(epoch, source, landing) = out;
        *self.0.src_result.lock().unwrap_or_else(|e| e.into_inner()) = Some((epoch, source, landing));
    }

    /// The request was never started, so nothing will land and nothing will release its claim:
    /// hand the claim back and remember the refusal ([`crate::stores::Fetch::refuse_for`]); the
    /// store settles it at the head of its next step. The mailbox is never touched, so a refusal
    /// can never be recorded as a result.
    fn refused(&self, addr: Addr) {
        self.0.src_fetching.refuse_for(addr.req.0);
    }
}

/// The request's IO body and its completion: read the source through `client` and post the answer
/// to `port`. The answer is posted OUTSIDE the guard, so a read that panics lands as the FAILURE of
/// the job it was doing (never a success of another).
pub fn finish_browse_discovery_job(port: &BrowseDiscoveryPort, addr: Addr, client: &'static Client,
    req: BrowseDiscoveryReq) {
    finish_discovery_with(port, addr, client, req, read_source);
}

/// [`finish_browse_discovery_job`] over `read`, the call that reaches the server: the seam a test
/// swaps for a read that panics.
fn finish_discovery_with(port: &BrowseDiscoveryPort, addr: Addr, client: &'static Client,
    req: BrowseDiscoveryReq, read: impl FnOnce(&'static Client, SrcJob, bool) -> (String, SrcWhat)) {
    let BrowseDiscoveryReq { epoch, source, key, job, want_name, .. } = req;
    let is_sections = matches!(job, SrcJob::Sections);
    let (name, what) = catch_unwind(AssertUnwindSafe(|| read(client, job, want_name))).unwrap_or_else(|_| {
        // a panicking fetch is a FAILURE of the job it was doing, never a success of another:
        // reporting a panicked count probe as a failed section list would drop the source's
        // whole library list on the floor.
        let what = if is_sections { SrcWhat::Sections(None) } else { SrcWhat::Counts(Vec::new()) };
        (String::new(), what)
    });
    port.done(addr, BrowseDiscoveryAnswer(epoch, source, SrcLanding { client, token_gen: key.token, name, what }));
}

/// The reads of one discovery job through `client`: the server's own name (when the source has
/// none) and its section list or the item count of each wanted section.
fn read_source(client: &'static Client, job: SrcJob, want_name: bool) -> (String, SrcWhat) {
    // the server naming ITSELF, so a roster that never reached plex.tv still heads its
    // group with a machine name. One request, once, per source.
    let name = if want_name { client.friendly_name().unwrap_or_default() } else { String::new() };
    let what = match job {
        SrcJob::Sections => SrcWhat::Sections(client.sections().map(|mc| project_sections(&mc))),
        SrcJob::Counts(keys) => {
            let mut out = Vec::new();
            for k in keys {
                // size=0: PMS answers with `totalSize` and no items at all, so a
                // library's count costs a header rather than a page.
                let q = SectionQuery {
                    section_key: k,
                    sort: "",
                    filters: &[],
                    start: 0,
                    size: 0,
                    include_meta: false,
                };
                if let Some(mc) = client.section_items_query(&q) {
                    out.push((k, mc.total_size));
                }
            }
            SrcWhat::Counts(out)
        }
    };
    (name, what)
}

#[cfg(test)]
#[path = "discovery_job_tests.rs"]
mod tests;
