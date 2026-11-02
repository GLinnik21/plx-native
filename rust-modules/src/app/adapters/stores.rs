//! The runner for the jobs the data layer's stores emit (`plx_data::stores::StoreJob`).
//!
//! A store's step asks for IO by pushing a typed job on its outbox; it starts no thread and reads
//! no clock. The `Bridge` takes the jobs right after the step that emitted them
//! (`Bridge::metadata_run`, `Bridge::person_pump`, `Bridge::collection_pump`,
//! `Bridge::catalog_open`, `Bridge::catalog_pump`, `Bridge::subsearch_pump`, `Bridge::search_pump`,
//! `Bridge::reconcile_pump`, `Bridge::viewstate_run`, `Bridge::viewstate_pump`,
//! `Bridge::browse_discover_pump`, or
//! `Bridge::start_store_jobs` after a command run on the bare store)
//! and hands each to [`StoreRunner::start`], which owns everything the data layer must not: the
//! tape's admission record, the worker thread and the process-global client the request reads. A
//! kind that is not on the tape (the catalog read, a plex.tv call that names no client; subtitle
//! search; the Search screen's fetches and its recents writes; view reconcile; the Library's
//! section-hubs, directory and page reads; the view-state writes; the Metadata season switch,
//! episode-page window and Related window) is started with no admission record, as its inline
//! spawn was. The detail pump keeps a counted backstop for a sender that did neither
//! (`Bridge::start_late_store_jobs`).
//!
//! **Three admission families** (`plx_data::stores::Admission`, stated by the job kind and matched
//! exhaustively): `Tape` kinds record their launch on the tape; `Direct` kinds are started with no
//! record; a `Home` kind (the Hubs read and the Library's source discovery) records its launch in a
//! controlled Home run's own ordered ledger (`HomeIo::admit`). A controlled arm starts its Home jobs with
//! [`StoreRunner::start_admitted`], handing over the run's `HomeIo`; every other sender, the live
//! arms and the landing and fan-out drains included, starts them with [`StoreRunner::start`], which
//! launches a Home kind as a direct one. A Home job that reaches `start` from a controlled arm has
//! skipped its admission, which a replay would catch as a missing line.
//!
//! **One runner per `Bridge`, chosen when the Bridge is built**, as the consent adapter is: `Live`
//! in production, and in a host test the `Held` runner, which parks each job until the test runs
//! it on the test thread (`Bridge::run_held_store_jobs_for_test`). Holding is what lets a test
//! choose the moment a request is "in flight" and the moment it "settles", where a worker thread
//! made both depend on the scheduler: a fetch for an unknown server returns in microseconds, so a
//! fast run settled a request before an "in flight" assertion while a loaded CI runner had not
//! settled it after a hundred `yield_now`s. `Held` parks per kind (`LiveKind`): [`StoreRunner::held`]
//! leaves the Home Hubs read, the Metadata season switch, the Library listing's page read and the
//! Library's source discovery live (the rigs that poll for a landing, the dump and return-hold
//! tests, `menu_play_season_tests` and the library route rigs that wait for a grid to settle, need a
//! real answer on a worker; discovery is live so the rigs that run every parked job on the test thread
//! (the hostsim helpers of `app/run.rs`) never run its blocking reads there),
//! [`StoreRunner::park_page_reads`] parks the page read as well, and [`StoreRunner::held_all`] parks
//! every kind. Every held runner parks episode-page and Related window jobs.
//!
//! **The registry is read here, not in a step.** A step that decides from "which client does this
//! server slot hold" is handed the answer as an input (`plx_plex::plex::ClientKeys`), which the
//! Bridge fills with [`refill_client_keys`] immediately before the step, in the same call. A job
//! carries the key its step decided on, and the runner starts it only if the registry still holds
//! that client at the moment of the launch. (A view-state write also carries the whole snapshot its
//! step decided under: its worker reaches the title's OTHER copies only through
//! `plx_plex::plex::KeyedClients` built from it, with the registry read passed in from here. The
//! Detail job carries no key: `start_detail` resolves its server once, at the admission, and the
//! whole chain runs through that client. The season, episode-page and Related window jobs carry
//! none either: `start_meta_season`, `start_meta_episodes` and `start_meta_related` resolve the
//! server's client once, at the launch, and hand it to the worker body; a server with none answers
//! the failure without a GET; `docs/sans-io.md`, "The store job lane".)

use plx_data::browse::bands::Zone;
use plx_data::browse::discovery_job::{
    finish_browse_discovery_job, BrowseDiscoveryPort, BrowseDiscoveryReq,
};
use plx_data::browse::listing_jobs::{
    finish_browse_directory_job, finish_browse_page_job, BrowseDirectoryPort, BrowseDirectoryReq,
    BrowsePagePort, BrowsePageReq,
};
use plx_data::browse::section_hubs::{finish_browse_hubs_job, BrowseHubsPort, BrowseHubsReq};
use plx_data::collection::{finish_collection_job, CollectionPort, CollectionReq};
use plx_data::metadata::{
    fetch_full, finish_detail_fetch, finish_episodes_job, finish_related_job, finish_season_job, DetailPort,
    DetailReq, MetaEpisodesPort, MetaEpisodesReq, MetaRelatedPort, MetaRelatedReq, MetaSeasonPort, MetaSeasonReq,
};
use plx_data::person::{finish_person_job, PersonPort, PersonReq};
use plx_data::stores::catalog::{finish_catalog_job, CatalogPort, CatalogReq};
use plx_data::stores::viewreconcile::{finish_reconcile_job, ReconcilePort, ReconcileReq};
use plx_data::pms::{launch_hubs_job, HubRequest, HubsPort};
use plx_data::stores::{Admission, StoreJob};
use plx_data::search::recents::{finish_save_recents_job, SaveRecentsPort, SaveRecentsReq};
use plx_data::search::{finish_search_job, SearchPort, SearchReq};
use plx_data::subsearch::{finish_subsearch_job, SubSearchPort, SubSearchReq};
use plx_data::viewstate::{finish_view_write_job, ViewWritePort, ViewWriteReq};
use plx_machine::job::{Job, Port};
use plx_plex::plex::{Client, ClientKey, ClientKeys};

use crate::app::bootstrap::HomeIo;

/// The viewer's time zone for a bands read, as the platform layer reads it from the OS: this runner is
/// where the zone may be read (the data job takes it as two plain functions, `Zone`, and no machine
/// reads it), and a calendar family's edges are converted with it.
fn os_zone() -> Zone {
    Zone { local_midnight: plx_platform::i18n::local_midnight, local_date: plx_platform::i18n::local_calendar_date }
}

/// Fill `keys` with the registry's answer for every active server slot, in place (a pump that runs
/// every frame allocates nothing for it). The only loop over `client_for` that feeds a step: an
/// active slot can still hold no client, so the walk asks each slot for its client and keeps the
/// ones that answer, which makes `keys.key(sid)` equal `client_for(sid).map(ClientKey::of)` for
/// every `sid`, a retired or unregistered one included.
pub(crate) fn refill_client_keys(keys: &mut ClientKeys) {
    keys.refill(plx_plex::plex::server_ids().filter_map(|sid| {
        plx_plex::plex::client_for(sid).map(|client| (sid, ClientKey::of(client)))
    }));
}

/// The client a job's step decided on, if the registry still holds it: `client_for(sid)` accepted
/// only when it is the very client (and credential) the step's [`ClientKeys`] named. A job the
/// registry has moved on from (a sign-out, a re-registration, a retoken) is not started.
///
/// Capture at the step (`pms::kick_with` states the rule): the worker is handed THIS server's own
/// `&'static Client`, never `client()`. A slot re-pointed mid-request cannot redirect a fetch
/// already out (`plex::servers` leaks each client precisely so the reference stays live), and a
/// worker that asked which server is current would answer for whichever machine the user has since
/// walked onto. The key compare is what makes the step's decision and the launch agree on one
/// object, since the registry can move between the two.
fn resolve(sid: plx_plex::plex::ServerId, key: ClientKey) -> Option<&'static Client> {
    plx_plex::plex::client_for(sid).filter(|client| ClientKey::of(client) == key)
}

/// The registry no longer holds the client a job's step decided on.
struct Gone;

/// The client a person job runs through: none for the two plex.tv reads, which ask no server; for a
/// read of a server, [`resolve`]'s answer, or [`Gone`] when the registry has moved on from the
/// client the step named.
fn person_client(req: &PersonReq) -> Result<Option<&'static Client>, Gone> {
    match req.server() {
        None => Ok(None),
        Some((sid, key)) => resolve(sid, key).map(Some).ok_or(Gone),
    }
}

/// TEST ONLY: what [`StoreRunner::launch_hubs`] does on this test thread in place of starting the
/// worker. The seams stand where the production boundary is: the OS refusing a thread (below the
/// claim `owed` reads, which the refusal gives back) and a worker whose answer comes late.
#[cfg(test)]
#[derive(Clone, Copy)]
enum HubsLaunch {
    Real,
    Refused,
    Late { extra: u32, items: usize },
}

#[cfg(test)]
thread_local! {
    static HUBS_LAUNCH_FOR_TEST: std::cell::Cell<HubsLaunch> = const { std::cell::Cell::new(HubsLaunch::Real) };
}

#[cfg(test)]
fn hubs_launch_for_test() -> HubsLaunch {
    HUBS_LAUNCH_FOR_TEST.with(|mode| mode.get())
}

#[cfg(test)]
fn with_hubs_launch_for_test<R>(mode: HubsLaunch, f: impl FnOnce() -> R) -> R {
    struct Restore(HubsLaunch);
    impl Drop for Restore {
        fn drop(&mut self) {
            HUBS_LAUNCH_FOR_TEST.with(|mode| mode.set(self.0));
        }
    }
    let _restore = Restore(HUBS_LAUNCH_FOR_TEST.with(|current| current.replace(mode)));
    f()
}

/// TEST ONLY: every Hubs launch this test thread makes inside `f` is refused by the system (no
/// worker starts), including a launch made from a nested call into the Bridge. The section hubs'
/// reads (`StoreJob::BrowseHubs`) and the Library's listing reads (`StoreJob::BrowseDirectory`,
/// `StoreJob::BrowsePage`) are launches of this seam too: the system refuses their thread alike.
#[cfg(test)]
pub(crate) fn with_refused_hubs_launches_for_test<R>(f: impl FnOnce() -> R) -> R {
    with_hubs_launch_for_test(HubsLaunch::Refused, f)
}

/// TEST ONLY: replace only the network read of every Hubs launch this test thread makes inside `f`
/// with a worker that answers with `items` movies LATE: it posts only after the adapter's mailbox
/// has been taken at least once since the launch (so a take that does not wait finds it empty,
/// every time), then yields `extra` more times so its post lands at a varying point of the poll
/// loop of a take that does. Both waits are counts, never clocks, and bounded.
#[cfg(test)]
pub(crate) fn with_late_hubs_launches_for_test<R>(extra: u32, items: usize, f: impl FnOnce() -> R) -> R {
    with_hubs_launch_for_test(HubsLaunch::Late { extra, items }, f)
}

pub(crate) enum StoreRunner {
    /// Start every job on a worker thread.
    Live,
    /// TEST ONLY: park the jobs of every kind not in `live` until [`StoreRunner::run_held`]: after
    /// its tape admission for a kind that is on the tape, after the registry's key check for one
    /// that is not but reads a server, and with neither for a Catalog job (a plex.tv read that
    /// names no client) or a season, episode-page or Related window job, which carries no key
    /// (`run_held` resolves its client when it runs the job). A kind in `live` starts as `Live`
    /// starts it.
    #[cfg(test)]
    Held { parked: Vec<StoreJob>, live: &'static [LiveKind] },
}

/// TEST ONLY: a kind of job a [`StoreRunner::Held`] starts on a worker instead of parking. It names
/// a kind and not a store: Browse emits several kinds (`BrowseHubs`, `BrowseDirectory`,
/// `BrowsePage`, `BrowseDiscovery`), and a rig that waits for the Library grid's page landing must not also get the
/// section hubs' read live; Metadata's season switch runs live while
/// its detail fetch stays parked.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveKind {
    /// The Home Hubs read (`StoreJob::Hubs`).
    Hubs,
    /// The Metadata season switch (`StoreJob::MetaSeason`).
    MetaSeason,
    /// A Library listing page read (`StoreJob::BrowsePage`).
    BrowsePage,
    /// A Library source discovery (`StoreJob::BrowseDiscovery`). Live because a parked one would
    /// run its blocking GETs on the test thread wherever a rig runs every parked job
    /// (`Bridge::run_held_store_jobs_for_test`, which the hostsim rigs of `app/run.rs` call): against
    /// a fixture that parks its first request until the test releases it (`Rig::serving_with`) or a
    /// server that accepts and never answers, where today's worker blocks harmlessly off-thread.
    /// No rig waits for a discovery landing from a real worker: every one fabricates it.
    BrowseDiscovery,
}

impl StoreRunner {
    pub(crate) fn live() -> Self {
        Self::Live
    }

    /// TEST ONLY: [`StoreRunner::Held`] for every kind but the Home Hubs read, the Metadata season
    /// switch, the Library listing's page read and the Library's source discovery, which start on a
    /// worker as they do in production:
    /// a rig that waits for a Home or a grid landing, and the menu-play season wait
    /// (`menu_play_season_tests`, which polls `metadata_pump_season` for a landing and never runs held
    /// jobs), need a real one. No rig waits for an episode-page or a Related window landing through a
    /// worker (the Related rigs land the window with `land_related_for_test`), so those are parked.
    #[cfg(test)]
    pub(crate) fn held() -> Self {
        Self::Held { parked: Vec::new(), live: &[LiveKind::Hubs, LiveKind::MetaSeason, LiveKind::BrowsePage, LiveKind::BrowseDiscovery] }
    }

    /// TEST ONLY: park the Library listing's page reads of a held runner too (the Home Hubs read
    /// and the season switch stay live): the rig that grades which reads an entry started takes them
    /// from the runner.
    #[cfg(test)]
    pub(crate) fn park_page_reads(&mut self) {
        if let Self::Held { live, .. } = self {
            *live = &[LiveKind::Hubs, LiveKind::MetaSeason, LiveKind::BrowseDiscovery];
        }
    }

    /// TEST ONLY: [`StoreRunner::Held`] for every kind, Hubs, the season switch and the page read
    /// included. The test runs the parked jobs (`Bridge::run_held_store_jobs_of_for_test`) when it
    /// chooses to settle them.
    #[cfg(test)]
    pub(crate) fn held_all() -> Self {
        Self::Held { parked: Vec::new(), live: &[] }
    }

    /// Start `job`, admitting it through the tape when its kind is taped (Detail, Collection,
    /// Person; Catalog, subtitle search and view reconcile are not). Exactly one answer reaches its
    /// port: the fetch's own, or a refusal when nothing was started.
    pub(crate) fn start(&mut self, job: StoreJob) {
        match job {
            StoreJob::Detail(job, port) => self.start_detail(job, port),
            StoreJob::Collection(job, port) => self.start_collection(job, port),
            StoreJob::Person(job, port) => self.start_person(job, port),
            StoreJob::Hubs(job, port) => self.start_hubs(job, port),
            StoreJob::Catalog(job, port) => self.start_catalog(job, port),
            StoreJob::SubtitleSearch(job, port) => self.start_subsearch(job, port),
            StoreJob::ViewReconcile(job, port) => self.start_reconcile(job, port),
            StoreJob::BrowseHubs(job, port) => self.start_browse_hubs(job, port),
            StoreJob::BrowseDirectory(job, port) => self.start_browse_directory(job, port),
            StoreJob::BrowsePage(job, port) => self.start_browse_page(job, port),
            StoreJob::BrowseDiscovery(job, port) => self.start_browse_discovery(job, port),
            StoreJob::Search(job, port) => self.start_search(job, port),
            StoreJob::SaveRecents(job, port) => self.start_save_recents(job, port),
            StoreJob::ViewWrite(job, port) => self.start_view_write(job, port),
            StoreJob::MetaSeason(job, port) => self.start_meta_season(job, port),
            StoreJob::MetaEpisodes(job, port) => self.start_meta_episodes(job, port),
            StoreJob::MetaRelated(job, port) => self.start_meta_related(job, port),
        }
    }

    /// [`StoreRunner::start`] for a controlled Home run's arm: a job of the `Home` family is
    /// admitted through the run's own ledger (`home`), which records the launch or, under replay,
    /// answers it from the recording. Every other family starts as `start` starts it. A step run
    /// outside a controlled run has no ledger (`home: None`) and starts its Home jobs directly.
    pub(crate) fn start_admitted(&mut self, job: StoreJob, home: Option<&mut HomeIo>) {
        match (job.admission(), home) {
            (Admission::Home, Some(io)) => self.admit_home(job, io),
            (Admission::Tape | Admission::Home | Admission::Direct, _) => self.start(job),
        }
    }

    /// Admit one Home job through `io`. Exactly one answer reaches its port, or none under replay: a
    /// recorded launch (`admitted: true`) starts nothing now, because the tape's own recorded result
    /// answers it, and a recorded refusal is answered here, once. Recording, the launch is the
    /// recorded boolean.
    fn admit_home(&mut self, job: StoreJob, io: &mut HomeIo) {
        match job {
            StoreJob::Hubs(job, port) => {
                let addr = job.addr;
                let request = job.req.admission();
                let refusal = port.clone();
                if !io.admit(request, || self.launch_hubs(job, port)) {
                    refusal.refused(addr);
                }
            }
            StoreJob::BrowseDiscovery(job, port) => {
                let addr = job.addr;
                let request = job.req.admission();
                let refusal = port.clone();
                if !io.admit(request, || self.launch_browse_discovery(job, port)) {
                    refusal.refused(addr);
                }
            }
            // Not a Home kind: `Admission::Home` routed here only the kinds the arm above names.
            other @ (StoreJob::Detail(..)
                | StoreJob::Collection(..)
                | StoreJob::Person(..)
                | StoreJob::Catalog(..)
                | StoreJob::SubtitleSearch(..)
                | StoreJob::ViewReconcile(..)
                | StoreJob::BrowseHubs(..)
                | StoreJob::BrowseDirectory(..)
                | StoreJob::BrowsePage(..)
                | StoreJob::Search(..)
                | StoreJob::SaveRecents(..)
                | StoreJob::ViewWrite(..)
                | StoreJob::MetaSeason(..)
                | StoreJob::MetaEpisodes(..)
                | StoreJob::MetaRelated(..)) => self.start(other),
        }
    }

    /// A Hubs read outside a controlled run's ledger starts with no record, as its inline spawn
    /// did. A launch that did not happen is refused, and the refusal is the job's answer: the next
    /// Hubs step finds it and backs the source off.
    fn start_hubs(&mut self, job: Job<HubRequest>, port: HubsPort) {
        let addr = job.addr;
        let refusal = port.clone();
        if !self.launch_hubs(job, port) {
            refusal.refused(addr);
        }
    }

    /// Start the fetch on a worker, if the registry still holds the client the step decided on. The
    /// claim `owed` reads is raised here (`launch_hubs_job`), on the calling thread before the
    /// worker exists; a launch that starts nothing gives it back and the caller refuses the job. A
    /// parked job (`Held`) holds the claim from the park: it is dropped back by the take of its
    /// landing, or by the refusal when the client is gone by the time the job runs.
    fn launch_hubs(&mut self, job: Job<HubRequest>, port: HubsPort) -> bool {
        let (sid, key) = job.req.server();
        let Some(client) = resolve(sid, key) else { return false };
        #[cfg(test)]
        match (&mut *self, hubs_launch_for_test()) {
            // The OS refusing the thread, below the claim.
            (_, HubsLaunch::Refused) => return launch_hubs_job(job, port, client, |_work| false),
            (_, HubsLaunch::Late { extra, items }) => return plx_data::pms::launch_late_hubs_job_for_test(
                job, port, client, |work| plx_base::task::spawn_small("hubs", work), extra, items),
            (Self::Held { parked, live }, HubsLaunch::Real) if !live.contains(&LiveKind::Hubs) => {
                port.claim();
                parked.push(StoreJob::Hubs(job, port));
                return true;
            }
            _ => {}
        }
        launch_hubs_job(job, port, client, |work| plx_base::task::spawn_small("hubs", work))
    }

    fn start_detail(&mut self, job: Job<DetailReq>, port: DetailPort) {
        // A request supersedes the one before it, so a fetch still parked here belongs to a
        // request that is already retired. Answer it, as the thread it stands for would have,
        // so its landing reservation can be handed back; the answer is discarded by the pump.
        #[cfg(test)]
        if let Self::Held { parked, .. } = self {
            Self::retire_parked_details(parked);
        }
        let addr = job.addr;
        let (sid, gen) = (job.req.sid, addr.req.0);
        // The ONE read of the registry for this job: the admission records the instance of the
        // client it names, and the launch runs the chain through that very client. A job
        // carries no `ClientKey` (the step that emitted it reads no registry), so what the
        // registry holds NOW is the client the page is opened on; the capture is made here, at the
        // admission, and the whole chain, the opening season's episodes included, runs through it.
        let client = plx_plex::plex::client_for(sid);
        let request = serde_json::json!({"store":"metadata",
            "sid":sid.raw(),"rk":job.req.rk,"gen":gen,
            "client":client.map(|c| c.instance_gen())});
        let admission = port.clone();
        if !plx_data::stores::tape::admit(request, || self.launch_detail(job, port, client)) {
            // Nothing was started, so nothing will ever answer on its own: the refusal is what
            // settles the spinner, exactly one event for the request.
            admission.refused(addr);
        }
    }

    /// TEST ONLY: answer the detail fetches still parked as superseded (see [`Self::start_detail`]),
    /// and keep every other store's parked job: a newer detail request retires only the requests it
    /// supersedes. A `match` over every job kind, so a store that joins the lane must say what a
    /// newer detail request does to its parked jobs. A parked season or page job is kept: the request
    /// already superseded the season (`begin_detail_request`), so a parked season lands a generation
    /// the pump discards, and a parked page lands keys the pump's item check rejects. A parked
    /// Related job is kept too: its landing is rejected by the pump's item check, and it holds the
    /// single-flight claim until it runs, as a worker still reading would.
    #[cfg(test)]
    fn retire_parked_details(held: &mut Vec<StoreJob>) {
        for parked in std::mem::take(held) {
            match parked {
                StoreJob::Detail(old, old_port) => {
                    old_port.done(old.addr, ((old.req.sid, old.req.rk), None));
                }
                kept @ (
                    StoreJob::Collection(..)
                    | StoreJob::Person(..)
                    | StoreJob::Hubs(..)
                    | StoreJob::Catalog(..)
                    | StoreJob::SubtitleSearch(..)
                    | StoreJob::ViewReconcile(..)
                    | StoreJob::BrowseHubs(..)
                    | StoreJob::BrowseDirectory(..)
                    | StoreJob::BrowsePage(..)
                    | StoreJob::BrowseDiscovery(..)
                    | StoreJob::Search(..)
                    | StoreJob::SaveRecents(..)
                    | StoreJob::ViewWrite(..)
                    | StoreJob::MetaSeason(..)
                    | StoreJob::MetaEpisodes(..)
                    | StoreJob::MetaRelated(..)
                ) => held.push(kept),
            }
        }
    }

    /// Start the fetch on a worker through `client`, the one [`Self::start_detail`] resolved. A
    /// server the registry holds no client for still starts (the worker answers the failure, as
    /// the chain always did when its first read found no client): the page settles on that answer.
    fn launch_detail(&mut self, job: Job<DetailReq>, port: DetailPort, client: Option<&'static Client>) -> bool {
        match self {
            Self::Live => {
                let Job { addr, req: DetailReq { sid, rk, prefer } } = job;
                plx_base::task::spawn_small("detail", move || {
                    finish_detail_fetch(&port, addr, (sid, rk.clone()), client, |client| {
                        fetch_full(client, sid, &rk, prefer)
                    });
                })
            }
            #[cfg(test)]
            Self::Held { parked: held, .. } => {
                held.push(StoreJob::Detail(job, port));
                true
            }
        }
    }

    fn start_collection(&mut self, job: Job<CollectionReq>, port: CollectionPort) {
        let addr = job.addr;
        let request = job.req.admission(addr);
        let admission = port.clone();
        if !plx_data::stores::tape::admit(request, || self.launch_collection(job, port)) {
            // Nothing was started, so nothing will ever answer on its own: the refusal hands the
            // page's claim back, and the next pump asks again.
            admission.refused(addr);
        }
    }

    /// Start the request on a worker, if the registry still holds the client the step decided on.
    /// The check is made HERE, at the launch the tape records, so a replay (which never launches)
    /// and a recording agree on the one boolean they both hold.
    fn launch_collection(&mut self, job: Job<CollectionReq>, port: CollectionPort) -> bool {
        let (sid, key) = job.req.server();
        let Some(client) = resolve(sid, key) else { return false };
        match self {
            Self::Live => {
                let Job { addr, req } = job;
                plx_base::task::spawn_small("collection", move || {
                    finish_collection_job(&port, addr, client, req);
                })
            }
            #[cfg(test)]
            Self::Held { parked: held, .. } => {
                held.push(StoreJob::Collection(job, port));
                true
            }
        }
    }

    fn start_person(&mut self, job: Job<PersonReq>, port: PersonPort) {
        let addr = job.addr;
        let request = job.req.admission();
        let admission = port.clone();
        if !plx_data::stores::tape::admit(request, || self.launch_person(job, port)) {
            // Nothing was started, so nothing will ever answer on its own: the refusal hands the
            // slot's claim back, and the next pump asks again.
            admission.refused(addr);
        }
    }

    /// Start the request on a worker, if the registry still holds the client the step decided on
    /// (a local job; the two plex.tv reads name no client). The check is made HERE, at the launch
    /// the tape records, so a replay (which never launches) and a recording agree on the one
    /// boolean they both hold.
    fn launch_person(&mut self, job: Job<PersonReq>, port: PersonPort) -> bool {
        let Ok(client) = person_client(&job.req) else { return false };
        match self {
            Self::Live => {
                let Job { addr, req } = job;
                plx_base::task::spawn_small("person", move || {
                    finish_person_job(&port, addr, client, req);
                })
            }
            #[cfg(test)]
            Self::Held { parked: held, .. } => {
                held.push(StoreJob::Person(job, port));
                true
            }
        }
    }

    /// The catalog read is not on the tape and names no client, so the launch is the whole
    /// decision, as it was for the inline spawn. A launch that did not happen is refused, and the
    /// refusal is the job's answer: the pump that takes it owes the request and asks again after
    /// the store's wait. (No host test can make `spawn_small` fail, so that branch is read, not run.)
    fn start_catalog(&mut self, job: Job<CatalogReq>, port: CatalogPort) {
        let addr = job.addr;
        let refusal = port.clone();
        if !self.launch_catalog(job, port) {
            refusal.refused(addr);
        }
    }

    fn launch_catalog(&mut self, job: Job<CatalogReq>, port: CatalogPort) -> bool {
        match self {
            Self::Live => {
                let Job { addr, req } = job;
                plx_base::task::spawn_small("catalog-fetch", move || {
                    finish_catalog_job(&port, addr, req);
                })
            }
            #[cfg(test)]
            Self::Held { parked: held, .. } => {
                held.push(StoreJob::Catalog(job, port));
                true
            }
        }
    }

    /// A subtitle-search request is not on the tape, so nothing is admitted or recorded: the
    /// launch is the whole decision. A launch that did not happen
    /// (the registry no longer holds the client the step decided on, or the thread ceiling) is
    /// refused, which hands the request's claim back and tells the pump to back off.
    fn start_subsearch(&mut self, job: Job<SubSearchReq>, port: SubSearchPort) {
        let addr = job.addr;
        let refusal = port.clone();
        if !self.launch_subsearch(job, port) {
            refusal.refused(addr);
        }
    }

    /// Start the request on a worker, if the registry still holds the client the step decided on.
    fn launch_subsearch(&mut self, job: Job<SubSearchReq>, port: SubSearchPort) -> bool {
        let (sid, key) = job.req.server();
        let Some(client) = resolve(sid, key) else { return false };
        match self {
            Self::Live => {
                let Job { addr, req } = job;
                plx_base::task::spawn_small("subsearch", move || {
                    finish_subsearch_job(&port, addr, client, req);
                })
            }
            #[cfg(test)]
            Self::Held { parked: held, .. } => {
                held.push(StoreJob::SubtitleSearch(job, port));
                true
            }
        }
    }

    /// A search fetch is not on the tape, so nothing is admitted or recorded: the launch is the
    /// whole decision. A launch that did not happen (the registry no longer holds the client the
    /// step decided on, or the thread ceiling) is refused, which hands the source's claim back and
    /// tells the pump to back off, instead of the respawn on the very next frame a refused
    /// inline spawn was.
    fn start_search(&mut self, job: Job<SearchReq>, port: SearchPort) {
        let addr = job.addr;
        let refusal = port.clone();
        if !self.launch_search(job, port) {
            refusal.refused(addr);
        }
    }

    /// Start the fetch on a worker, if the registry still holds the client the step decided on.
    /// The client is resolved ONCE, here, and the worker is handed that `&'static Client`: it reads
    /// no registry, so a slot a roster change re-points mid-flight cannot redirect a fetch already
    /// out. The answer of a fetch the roster has since moved on from is caught by the roster-
    /// generation supersede, which drops it at the next pump.
    fn launch_search(&mut self, job: Job<SearchReq>, port: SearchPort) -> bool {
        let (sid, key) = job.req.server();
        let Some(client) = resolve(sid, key) else { return false };
        match self {
            Self::Live => {
                let Job { addr, req } = job;
                plx_base::task::spawn_small("search", move || {
                    finish_search_job(&port, addr, client, req);
                })
            }
            #[cfg(test)]
            Self::Held { parked: held, .. } => {
                held.push(StoreJob::Search(job, port));
                true
            }
        }
    }

    /// A recents write is not on the tape either, and names no client: it drains the in-memory
    /// queue into the session file. A launch that did not happen (the thread ceiling) is refused,
    /// which the store remembers, so the next recents edit asks for the drain again.
    fn start_save_recents(&mut self, job: Job<SaveRecentsReq>, port: SaveRecentsPort) {
        let addr = job.addr;
        let refusal = port.clone();
        if !self.launch_save_recents(job, port) {
            refusal.refused(addr);
        }
    }

    fn launch_save_recents(&mut self, job: Job<SaveRecentsReq>, port: SaveRecentsPort) -> bool {
        match self {
            Self::Live => {
                let Job { addr, req } = job;
                plx_base::task::spawn_small("recents-save", move || {
                    finish_save_recents_job(&port, addr, req);
                })
            }
            #[cfg(test)]
            Self::Held { parked: held, .. } => {
                held.push(StoreJob::SaveRecents(job, port));
                true
            }
        }
    }

    /// A season switch is not on the tape: the launch is the whole decision. A launch that did not
    /// happen (the thread ceiling) is refused, which catches the season's DONE up, so the episode
    /// row does not keep its spinner.
    fn start_meta_season(&mut self, job: Job<MetaSeasonReq>, port: MetaSeasonPort) {
        let addr = job.addr;
        let refusal = port.clone();
        if !self.launch_meta_season(job, port) {
            refusal.refused(addr);
        }
    }

    /// Start the fetch on a worker. The registry is read ONCE, here, for the show's own server: the
    /// worker is handed that `&'static Client` and asks no registry. A server it holds none for
    /// still launches, and its worker answers the failure without a GET, as the inline fetch did
    /// when its read found no client.
    fn launch_meta_season(&mut self, job: Job<MetaSeasonReq>, port: MetaSeasonPort) -> bool {
        match self {
            Self::Live => Self::spawn_meta_season(job, port),
            #[cfg(test)]
            Self::Held { parked, live } => {
                if live.contains(&LiveKind::MetaSeason) {
                    return Self::spawn_meta_season(job, port);
                }
                parked.push(StoreJob::MetaSeason(job, port));
                true
            }
        }
    }

    fn spawn_meta_season(job: Job<MetaSeasonReq>, port: MetaSeasonPort) -> bool {
        let client = plx_plex::plex::client_for(job.req.sid);
        let Job { addr, req } = job;
        plx_base::task::spawn_small("season", move || finish_season_job(&port, addr, client, req))
    }

    /// An episode-page window is not on the tape either. A launch that did not happen is refused,
    /// which drops the job's inflight keys, so the next `WantEpisodes` asks for the pages again.
    fn start_meta_episodes(&mut self, job: Job<MetaEpisodesReq>, port: MetaEpisodesPort) {
        let addr = job.addr;
        let refusal = port.clone();
        if !self.launch_meta_episodes(job, port) {
            refusal.refused(addr);
        }
    }

    /// Start the reads on a worker through the client resolved here, once, for the show's server.
    fn launch_meta_episodes(&mut self, job: Job<MetaEpisodesReq>, port: MetaEpisodesPort) -> bool {
        match self {
            Self::Live => {
                let client = plx_plex::plex::client_for(job.req.sid);
                let Job { addr, req } = job;
                plx_base::task::spawn_small("episode-pages", move || finish_episodes_job(&port, addr, client, req))
            }
            #[cfg(test)]
            Self::Held { parked, .. } => {
                parked.push(StoreJob::MetaEpisodes(job, port));
                true
            }
        }
    }

    /// A Related window read is not on the tape either. A launch that did not happen is refused,
    /// which gives the single-flight claim back in the same call, so the next edge asks again.
    fn start_meta_related(&mut self, job: Job<MetaRelatedReq>, port: MetaRelatedPort) {
        let addr = job.addr;
        let refusal = port.clone();
        if !self.launch_meta_related(job, port) {
            refusal.refused(addr);
        }
    }

    /// Start the read on a worker through the client resolved here, once, for the loaded item's
    /// server. A server the registry holds none for still launches, and its worker answers the
    /// failure without a GET, as the inline worker did when its read found no client. A parked job
    /// (`Held`) holds the claim from the park, as a worker that has not answered does.
    fn launch_meta_related(&mut self, job: Job<MetaRelatedReq>, port: MetaRelatedPort) -> bool {
        match self {
            Self::Live => {
                let client = plx_plex::plex::client_for(job.req.sid);
                let Job { addr, req } = job;
                plx_base::task::spawn_small("related-tail", move || finish_related_job(&port, addr, client, req))
            }
            #[cfg(test)]
            Self::Held { parked, .. } => {
                parked.push(StoreJob::MetaRelated(job, port));
                true
            }
        }
    }

    /// A read-back batch is not on the tape either: the launch is the whole decision. A launch that
    /// did not happen is refused, and the refusal is the flight's answer: the pump that takes it
    /// puts the batch back in the queue and waits before it asks again.
    fn start_reconcile(&mut self, job: Job<ReconcileReq>, port: ReconcilePort) {
        let addr = job.addr;
        let refusal = port.clone();
        if !self.launch_reconcile(job, port) {
            refusal.refused(addr);
        }
    }

    /// Start the read on a worker, if the registry still holds the client the step decided on.
    fn launch_reconcile(&mut self, job: Job<ReconcileReq>, port: ReconcilePort) -> bool {
        let (sid, key) = job.req.server();
        let Some(client) = resolve(sid, key) else { return false };
        match self {
            Self::Live => {
                let Job { addr, req } = job;
                plx_base::task::spawn_small("viewreconcile", move || {
                    finish_reconcile_job(&port, addr, client, req);
                })
            }
            #[cfg(test)]
            Self::Held { parked: held, .. } => {
                held.push(StoreJob::ViewReconcile(job, port));
                true
            }
        }
    }

    /// A view-state write is not on the tape either: the launch is the whole decision (a controlled
    /// run refuses every write before it reaches the store, `viewstate_controlled_tests`). A launch
    /// that did not happen is refused, and the refusal is the write's answer: nothing left the
    /// device, so the pump that takes it judges the write again from its own inputs (a moved profile,
    /// a vanished server or a re-tokened client drops it) and puts it back at the head of the queue.
    fn start_view_write(&mut self, job: Job<ViewWriteReq>, port: ViewWritePort) {
        let addr = job.addr;
        let refusal = port.clone();
        if !self.launch_view_write(job, port) {
            refusal.refused(addr);
        }
    }

    /// Start the write on a worker, if the registry still holds the client the step decided on for
    /// the origin. The origin is resolved ONCE, here, and the worker is handed that `&'static
    /// Client`; the other copies of the title are reached only through the job's own snapshot, and
    /// only while each client still carries the key in it (`finish_view_write_job`). The registry
    /// read the worker needs for that is passed in here, so no data-layer step names it.
    fn launch_view_write(&mut self, job: Job<ViewWriteReq>, port: ViewWritePort) -> bool {
        let (sid, key) = job.req.server();
        let Some(origin) = resolve(sid, key) else { return false };
        match self {
            Self::Live => {
                let Job { addr, req } = job;
                plx_base::task::spawn_small("viewstate", move || {
                    finish_view_write_job(&port, addr, origin, plx_plex::plex::client_for, req);
                })
            }
            #[cfg(test)]
            Self::Held { parked: held, .. } => {
                held.push(StoreJob::ViewWrite(job, port));
                true
            }
        }
    }

    /// A section-hubs read is not on the tape either (and a controlled run never emits it, so the
    /// Home ledger holds no record of it): the launch is the whole decision. A launch that did not
    /// happen (the registry no longer holds the client the step decided on, or the thread ceiling) is
    /// refused, which hands the section hubs' one claim back; the store's next step settles the section to
    /// the state a read that never started leaves, and the pump after asks again.
    fn start_browse_hubs(&mut self, job: Job<BrowseHubsReq>, port: BrowseHubsPort) {
        let addr = job.addr;
        let refusal = port.clone();
        if !self.launch_browse_hubs(job, port) {
            refusal.refused(addr);
        }
    }

    /// Start the read on a worker, if the registry still holds the client the step decided on. The
    /// claim was raised by the step, so a parked job (`Held`) holds it from the park: it is dropped
    /// back by the take of its landing, or by the refusal when the client is gone by the time the
    /// job runs.
    fn launch_browse_hubs(&mut self, job: Job<BrowseHubsReq>, port: BrowseHubsPort) -> bool {
        let (sid, key) = job.req.server();
        let Some(client) = resolve(sid, key) else { return false };
        #[cfg(test)]
        if matches!(hubs_launch_for_test(), HubsLaunch::Refused) {
            return false; // the OS refusing the thread
        }
        match self {
            Self::Live => {
                let Job { addr, req } = job;
                plx_base::task::spawn_small("libhubs", move || {
                    finish_browse_hubs_job(&port, addr, client, req);
                })
            }
            #[cfg(test)]
            Self::Held { parked: held, .. } => {
                held.push(StoreJob::BrowseHubs(job, port));
                true
            }
        }
    }

    /// A Library directory read (genres, first letters) is not on the tape either, and a controlled
    /// run never emits it: the launch is the whole decision. A launch that did not happen (the
    /// registry no longer holds the client the step decided on, or the thread ceiling) is refused,
    /// which hands that directory's claim back; the store's next step settles the refusal, and the
    /// menu's next tick asks again.
    fn start_browse_directory(&mut self, job: Job<BrowseDirectoryReq>, port: BrowseDirectoryPort) {
        let addr = job.addr;
        let refusal = port.clone();
        if !self.launch_browse_directory(job, port) {
            refusal.refused(addr);
        }
    }

    /// Start the read on a worker, if the registry still holds the client the step decided on. The
    /// claim was raised by the step, so a parked job (`Held`) holds it from the park: it is dropped
    /// back by the take of its landing, or by the refusal when the client is gone by the time the
    /// job runs.
    fn launch_browse_directory(&mut self, job: Job<BrowseDirectoryReq>, port: BrowseDirectoryPort) -> bool {
        let (sid, key) = job.req.server();
        let Some(client) = resolve(sid, key) else { return false };
        #[cfg(test)]
        if matches!(hubs_launch_for_test(), HubsLaunch::Refused) {
            return false; // the OS refusing the thread
        }
        match self {
            Self::Live => {
                let Job { addr, req } = job;
                plx_base::task::spawn_small("directory", move || {
                    finish_browse_directory_job(&port, addr, client, req, os_zone());
                })
            }
            #[cfg(test)]
            Self::Held { parked: held, .. } => {
                held.push(StoreJob::BrowseDirectory(job, port));
                true
            }
        }
    }

    /// A Library listing page read is not on the tape (a controlled run never emits it): the launch
    /// is the whole decision, as for a directory read ([`Self::start_browse_directory`]). A refused
    /// page hands the page claim back; the next pump asks again for the then-first missing page.
    fn start_browse_page(&mut self, job: Job<BrowsePageReq>, port: BrowsePagePort) {
        let addr = job.addr;
        let refusal = port.clone();
        if !self.launch_browse_page(job, port) {
            refusal.refused(addr);
        }
    }

    /// Start the read on a worker, if the registry still holds the client the step decided on (see
    /// [`Self::launch_browse_directory`] for the claim of a parked job).
    fn launch_browse_page(&mut self, job: Job<BrowsePageReq>, port: BrowsePagePort) -> bool {
        let (sid, key) = job.req.server();
        let Some(client) = resolve(sid, key) else { return false };
        #[cfg(test)]
        if matches!(hubs_launch_for_test(), HubsLaunch::Refused) {
            return false; // the OS refusing the thread
        }
        #[cfg(test)]
        if let Self::Held { parked, live } = self {
            if !live.contains(&LiveKind::BrowsePage) {
                parked.push(StoreJob::BrowsePage(job, port));
                return true;
            }
        }
        let Job { addr, req } = job;
        plx_base::task::spawn_small("page", move || {
            finish_browse_page_job(&port, addr, client, req);
        })
    }

    /// A source discovery is a `Home` kind: a controlled run's arm admits it through the run's
    /// ledger ([`StoreRunner::start_admitted`], [`Self::admit_home`]) and every other sender starts
    /// it here, with no record, as its unrecorded spawn was. A launch that did not happen (the
    /// registry no longer holds the client the step decided on, or the thread ceiling) is refused,
    /// which hands the discovery claim back; the store's next step settles the refusal, and the
    /// source backs off before it is asked again.
    fn start_browse_discovery(&mut self, job: Job<BrowseDiscoveryReq>, port: BrowseDiscoveryPort) {
        let addr = job.addr;
        let refusal = port.clone();
        if !self.launch_browse_discovery(job, port) {
            refusal.refused(addr);
        }
    }

    /// Start the read on a worker, if the registry still holds the client the step decided on. The
    /// claim was raised by the step, so a parked job (`Held`) holds it from the park: it is dropped
    /// back by the apply of its landing, or by the refusal when the client is gone by the time the
    /// job runs. The check is made HERE, at the launch a controlled run records, so a replay (which
    /// never launches) and a recording agree on the one boolean they both hold.
    fn launch_browse_discovery(&mut self, job: Job<BrowseDiscoveryReq>, port: BrowseDiscoveryPort) -> bool {
        let (sid, key) = job.req.server();
        let Some(client) = resolve(sid, key) else { return false };
        #[cfg(test)]
        if matches!(hubs_launch_for_test(), HubsLaunch::Refused) {
            return false; // the OS refusing the thread
        }
        #[cfg(test)]
        if let Self::Held { parked, live } = self {
            if !live.contains(&LiveKind::BrowseDiscovery) {
                parked.push(StoreJob::BrowseDiscovery(job, port));
                return true;
            }
        }
        let Job { addr, req } = job;
        // Not counted by `drain_workers_for_test` (`task.rs`): it dials whatever a roster names, so
        // a test that runs one waits on its answer instead.
        plx_base::task::spawn_small("sources", move || {
            finish_browse_discovery_job(&port, addr, client, req);
        })
    }

    /// TEST ONLY: run every parked job on THIS thread through the production completion path
    /// (`finish_detail_fetch` over `fetch_full`, `finish_collection_job`, `finish_person_job`,
    /// `finish_hubs_job`, `finish_catalog_job`, `finish_subsearch_job`, `finish_search_job`,
    /// `finish_save_recents_job`, `finish_season_job`, `finish_episodes_job`, `finish_reconcile_job`,
    /// `finish_browse_hubs_job`, `finish_view_write_job`, `finish_browse_directory_job`,
    /// `finish_browse_page_job`, `finish_browse_discovery_job`) and return how many ran.
    /// Their landings are queued for the next pump. Under `Live` there is nothing parked.
    #[cfg(test)]
    pub(crate) fn run_held(&mut self) -> usize {
        self.run_held_where(|_| true)
    }

    /// TEST ONLY: take the parked jobs without running them, in the order they were parked. A test
    /// that fabricates the answer itself reads each job and drops it; the claim a parked Hubs job
    /// raised stays up, as it does for a worker that has not answered.
    #[cfg(test)]
    pub(crate) fn take_held(&mut self) -> Vec<StoreJob> {
        match self {
            Self::Live => Vec::new(),
            Self::Held { parked, .. } => std::mem::take(parked),
        }
    }

    /// TEST ONLY: [`Self::run_held`] for the jobs ONE store emitted, leaving the others parked. A
    /// rig that feeds one page runs only that page's reads, as a worker thread would have, and
    /// does not settle a Detail fetch some other test is holding on purpose.
    #[cfg(test)]
    pub(crate) fn run_held_of(&mut self, id: plx_data::stores::StoreId) -> usize {
        self.run_held_where(|job| job.store() == id)
    }

    #[cfg(test)]
    fn run_held_where(&mut self, wanted: impl Fn(&StoreJob) -> bool) -> usize {
        let Self::Held { parked, .. } = self else { return 0 };
        let (run, keep): (Vec<_>, Vec<_>) = std::mem::take(parked).into_iter().partition(|job| wanted(job));
        *parked = keep;
        let n = run.len();
        for job in run {
            match job {
                StoreJob::Detail(job, port) => {
                    // The client is resolved when the job runs (a live one is resolved by
                    // `start_detail`, at the admission); a server with none answers the failure.
                    let DetailReq { sid, rk, prefer } = job.req;
                    finish_detail_fetch(&port, job.addr, (sid, rk.clone()), plx_plex::plex::client_for(sid), |client| {
                        fetch_full(client, sid, &rk, prefer)
                    });
                }
                StoreJob::Collection(job, port) => {
                    // The registry may have moved on since the job was parked: a request whose
                    // client is gone is refused, as the launch would have refused it.
                    let (sid, key) = job.req.server();
                    match resolve(sid, key) {
                        Some(client) => finish_collection_job(&port, job.addr, client, job.req),
                        None => port.refused(job.addr),
                    }
                }
                StoreJob::Person(job, port) => {
                    // As above, for a local job; the two plex.tv reads name no client.
                    match person_client(&job.req) {
                        Ok(client) => finish_person_job(&port, job.addr, client, job.req),
                        Err(Gone) => port.refused(job.addr),
                    }
                }
                StoreJob::Hubs(job, port) => {
                    // The claim was raised at the park. The registry may have moved on since: a
                    // request whose client is gone gives the claim back and is refused, as the
                    // launch would have refused it before raising it.
                    let (sid, key) = job.req.server();
                    match resolve(sid, key) {
                        Some(client) => plx_data::pms::finish_hubs_job(job, client, &port),
                        None => {
                            port.unclaim();
                            port.refused(job.addr);
                        }
                    }
                }
                StoreJob::Catalog(job, port) => finish_catalog_job(&port, job.addr, job.req),
                StoreJob::SubtitleSearch(job, port) => {
                    // The registry may have moved on since the job was parked: a request whose
                    // client is gone is refused, as the launch would have refused it.
                    let (sid, key) = job.req.server();
                    match resolve(sid, key) {
                        Some(client) => finish_subsearch_job(&port, job.addr, client, job.req),
                        None => port.refused(job.addr),
                    }
                }
                StoreJob::ViewReconcile(job, port) => {
                    let (sid, key) = job.req.server();
                    match resolve(sid, key) {
                        Some(client) => finish_reconcile_job(&port, job.addr, client, job.req),
                        None => port.refused(job.addr),
                    }
                }
                StoreJob::BrowseHubs(job, port) => {
                    // The claim was raised by the step. The registry may have moved on since the
                    // job was parked: a request whose client is gone is refused, which gives the
                    // claim back, as the launch would have refused it.
                    let (sid, key) = job.req.server();
                    match resolve(sid, key) {
                        Some(client) => finish_browse_hubs_job(&port, job.addr, client, job.req),
                        None => port.refused(job.addr),
                    }
                }
                StoreJob::BrowseDirectory(job, port) => {
                    // As for the section hubs' read: the claim was raised by the step, and a request
                    // whose client is gone is refused, which gives it back.
                    let (sid, key) = job.req.server();
                    match resolve(sid, key) {
                        Some(client) => finish_browse_directory_job(&port, job.addr, client, job.req, os_zone()),
                        None => port.refused(job.addr),
                    }
                }
                StoreJob::BrowsePage(job, port) => {
                    let (sid, key) = job.req.server();
                    match resolve(sid, key) {
                        Some(client) => finish_browse_page_job(&port, job.addr, client, job.req),
                        None => port.refused(job.addr),
                    }
                }
                StoreJob::BrowseDiscovery(job, port) => {
                    // As for the section hubs' read: the claim was raised by the step, and a request
                    // whose client is gone is refused, which gives it back.
                    let (sid, key) = job.req.server();
                    match resolve(sid, key) {
                        Some(client) => finish_browse_discovery_job(&port, job.addr, client, job.req),
                        None => port.refused(job.addr),
                    }
                }
                StoreJob::Search(job, port) => {
                    // The registry may have moved on since the job was parked: a fetch whose client
                    // is gone is refused, as the launch would have refused it.
                    let (sid, key) = job.req.server();
                    match resolve(sid, key) {
                        Some(client) => finish_search_job(&port, job.addr, client, job.req),
                        None => port.refused(job.addr),
                    }
                }
                StoreJob::SaveRecents(job, port) => finish_save_recents_job(&port, job.addr, job.req),
                StoreJob::ViewWrite(job, port) => {
                    // The registry may have moved on since the job was parked: a write whose
                    // origin client is gone, or carries a moved credential, is refused, as the
                    // launch would have refused it. Nothing was sent.
                    let (sid, key) = job.req.server();
                    match resolve(sid, key) {
                        Some(origin) => finish_view_write_job(
                            &port, job.addr, origin, plx_plex::plex::client_for, job.req),
                        None => port.refused(job.addr),
                    }
                }
                // The client is resolved when the job runs, as a live launch resolves it at the
                // launch; a server with none answers the failure.
                StoreJob::MetaSeason(job, port) => {
                    finish_season_job(&port, job.addr, plx_plex::plex::client_for(job.req.sid), job.req);
                }
                StoreJob::MetaEpisodes(job, port) => {
                    finish_episodes_job(&port, job.addr, plx_plex::plex::client_for(job.req.sid), job.req);
                }
                StoreJob::MetaRelated(job, port) => {
                    finish_related_job(&port, job.addr, plx_plex::plex::client_for(job.req.sid), job.req);
                }
            }
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_data::stores::browse::DirectorySnapshot;
    use plx_data::stores::collection::{CollectionCmd, CollectionStore};
    use plx_data::stores::hubs::{HubsCmd, HubsStore};
    use plx_data::stores::metadata::{MetadataCmd, MetadataStore};
    use plx_data::stores::person::{PersonCmd, PersonStore};
    use plx_data::stores::search::{SearchCmd, SearchStore};
    use plx_data::stores::subsearch::SubSearchStore;
    use plx_data::stores::tape;
    use plx_data::stores::viewreconcile::ViewReconcileStore;
    use plx_data::subsearch::{SearchStatus, SubSearchCmd};
    use plx_plex::plex::ServerId;

    use crate::app::bridge::test_support::Pms;
    use crate::app::bridge::Bridge;

    /// Puts the tape back to its inactive state when a test ends, however it ends.
    struct TapeReset;
    impl Drop for TapeReset {
        fn drop(&mut self) { tape::reset_for_test(); }
    }

    fn request(store: &mut MetadataStore, sid: ServerId, rk: &str) -> StoreJob {
        assert!(store.run(MetadataCmd::RequestDetail { sid, rk: rk.to_string() }));
        let mut jobs = store.take_jobs();
        assert_eq!(jobs.len(), 1, "one accepted request emits one job");
        jobs.remove(0)
    }

    /// A fresh store with its first detail request out (generation 1) and the tape in replay
    /// holding the one admission record a recording of that request would hold, `admitted` as
    /// given. Returns the store, the job the request emitted and that record.
    fn replaying(admitted: bool) -> (MetadataStore, StoreJob, serde_json::Value) {
        let sid = ServerId::UNSET;
        tape::init(None, true);
        let mut store = MetadataStore::default();
        let job = request(&mut store, sid, "42");
        let admission = serde_json::json!({"content_resource":true, "admitted":admitted,
            "request":{"store":"metadata","sid":sid.raw(),"rk":"42","gen":1,"client":null}});
        tape::begin([admission.clone()].into(), Default::default());
        (store, job, admission)
    }

    /// What the live landing holds for the request, read the way a live run's pump reads it. A
    /// replaying pump takes its landings from the tape's recorded replies and never looks at the
    /// live landing, so the tape is retired first, after its verdict has been taken.
    fn pump_the_live_landing(store: &mut MetadataStore) {
        tape::reset_for_test();
        store.pump_detail();
    }

    /// Replay with `"admitted":false`: the tape says the recorded run's launch was refused, so
    /// nothing may start and the runner itself settles the request, with one refusal. The tape's
    /// own verdict has to read clean too: a mismatched admission also answers "not admitted", so a
    /// request whose bytes drifted from the record would pass every other assertion here.
    #[test]
    fn a_job_the_tape_does_not_admit_is_refused_once_and_the_request_settles() {
        let _serial = plx_base::testlock::serial();
        let _tape = TapeReset;
        let (mut store, job, admission) = replaying(false);
        let sid = ServerId::UNSET;

        StoreRunner::live().start(job);
        // Had a worker been started it would have answered by the time this returns.
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![admission], None),
            "the request the runner admitted is byte-for-byte the recorded one");
        assert!(store.view().detail_loading(), "the refusal is queued, and lands on the pump");
        assert_eq!(plx_data::metadata::detail_inflight_for_test(store.adapter_ref()), 1);

        pump_the_live_landing(&mut store);
        assert!(!store.view().detail_loading(), "the refusal settled the spinner");
        assert_eq!(store.view().detail_request_status(sid, "42"), Some(false));
        assert!(store.view().current().is_none(), "a refusal installs nothing");
        assert_eq!(plx_data::metadata::detail_inflight_for_test(store.adapter_ref()), 0,
            "one refusal handed the one reservation back");
    }

    /// Replay with `"admitted":true`: the recorded run launched a worker, and replay never does, so
    /// the runner answers nothing. The request stays owed, to be settled by the replayed landing.
    #[test]
    fn a_job_the_tape_admits_launches_nothing_under_replay_and_stays_owed() {
        let _serial = plx_base::testlock::serial();
        let _tape = TapeReset;
        let (mut store, job, admission) = replaying(true);
        let sid = ServerId::UNSET;

        StoreRunner::live().start(job);
        // Had a worker been started it would have answered by the time this returns.
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![admission], None),
            "the request the runner admitted is byte-for-byte the recorded one");
        pump_the_live_landing(&mut store);
        assert!(store.view().detail_loading(), "no worker and no refusal answered, so the request is still out");
        assert_eq!(store.view().detail_request_status(sid, "42"), Some(true));
        assert!(store.view().current().is_none());
        assert_eq!(plx_data::metadata::detail_inflight_for_test(store.adapter_ref()), 1,
            "an admitted replay launch holds its reservation until the recorded landing");
    }

    /// `Held` parks each admitted job, and a newer request retires the one parked before it: that
    /// one is answered as the thread it stands for would have answered, so its reservation comes
    /// back on the next pump, and only the newest job is left to run.
    #[test]
    fn a_held_job_is_answered_as_superseded_when_the_next_request_starts() {
        let _serial = plx_base::testlock::serial();
        let sid = ServerId::UNSET;
        let mut store = MetadataStore::default();
        let mut runner = StoreRunner::held();
        let (old, new) = (request(&mut store, sid, "old"), request(&mut store, sid, "new"));

        runner.start(old);
        runner.start(new);
        assert_eq!(plx_data::metadata::detail_inflight_for_test(store.adapter_ref()), 2);
        store.pump_detail();
        assert_eq!(plx_data::metadata::detail_inflight_for_test(store.adapter_ref()), 1,
            "the superseded job's answer handed its slot back");
        assert!(store.view().detail_loading(), "the newest request is still out");

        assert_eq!(runner.run_held(), 1, "only the newest job was still parked");
        store.pump_detail();
        assert!(!store.view().detail_loading());
        assert_eq!(plx_data::metadata::detail_inflight_for_test(store.adapter_ref()), 0);
    }

    // ---- the Collection lane: each outcome of a collection job reaches its port once ----

    /// Puts the tape, the registry and the workers back to their inactive state when a test ends,
    /// however it ends. Declare it AFTER the serial guard, so it drops first.
    struct CollectionRig;
    impl Drop for CollectionRig {
        fn drop(&mut self) {
            plx_base::task::drain_workers_for_test();
            tape::reset_for_test();
            plx_plex::plex::reset_servers_for_test();
        }
    }

    /// A server registered at a port nothing listens on (a read that starts fails at once) and a
    /// collection store whose first request, the header of `50001`, is out: the job its pump
    /// emitted, keyed to the client the registry held when it did.
    fn collection_request() -> (ServerId, CollectionStore, StoreJob) {
        use plx_data::collection::{CollectionTarget, PAGE_SIZE};
        use plx_plex::plex::collections::CollectionRef;
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("runner", "127.0.0.1", 9, "t", "cid");
        let keys = registry_keys();
        let mut store = CollectionStore::default();
        let id = CollectionRef { sid, rk: "50001".into(), sec: 1, tag: 7, name: "Set".into() };
        assert!(store.run(CollectionCmd::Open { target: CollectionTarget { id, want: PAGE_SIZE } }, &keys));
        store.pump(plx_machine::landgate::fixture_gate(), &keys);
        let mut jobs = store.take_jobs();
        assert_eq!(jobs.len(), 1, "one open page, one request");
        (sid, store, jobs.remove(0))
    }

    /// The registry's answer as of now, as the Bridge fills it before a step.
    fn registry_keys() -> ClientKeys {
        let mut keys = ClientKeys::default();
        refill_client_keys(&mut keys);
        keys
    }

    /// The admission record of that request, `admitted` as given.
    fn collection_admission(sid: ServerId, admitted: bool) -> serde_json::Value {
        let client = plx_plex::plex::client_for(sid).expect("registered").instance_gen();
        serde_json::json!({"content_resource":true,"admitted":admitted,
            "request":{"store":"collection","slot":0,"gen":1,"sid":sid.raw(),"client":client,
                "job":{"Header":{"rk":"50001"}}}})
    }

    fn collection_claim_held(store: &CollectionStore) -> bool { store.adapter_for_test().busy_for_test() }

    /// Recording, the registry still holding the client the step decided on: the worker starts,
    /// reads, and posts its ONE answer; taking it releases the claim.
    #[test]
    fn a_collection_job_whose_client_is_still_held_is_admitted_and_answered_once() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, mut store, job) = collection_request();
        tape::init(None, false);

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![collection_admission(sid, true)], None),
            "the request the runner admitted is the recorded shape, admitted");
        assert!(store.adapter_for_test().has_mail_for_test(), "the worker answered");
        tape::reset_for_test();
        let generation = store.generation_for_test();
        store.pump(plx_machine::landgate::fixture_gate(), &registry_keys());
        assert_eq!(store.generation_for_test(), generation, "the same listing took the answer: it was not rebuilt");
        assert!(!collection_claim_held(&store), "the one answer released the claim");
        assert_eq!(store.view().current().expect("open").status, plx_data::collection::CollectionStatus::Failed,
            "the unreachable server's failure is what landed");
        assert!(!store.has_jobs(), "a failed read is retried after its hold-off, not at once");
    }

    /// Replay with `"admitted":false`: the recorded run's launch was refused, so nothing starts and
    /// the runner settles the request with one refusal, handing the claim back. The tape's own
    /// verdict has to read clean too: a request whose bytes drifted from the record also answers
    /// "not admitted".
    #[test]
    fn a_collection_job_the_tape_does_not_admit_is_refused_once_and_its_claim_comes_back() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, mut store, job) = collection_request();
        let admission = collection_admission(sid, false);
        tape::init(None, true);
        tape::begin([admission.clone()].into(), Default::default());

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![admission], None),
            "the request the runner admitted is byte-for-byte the recorded one");
        assert!(!collection_claim_held(&store), "the refusal handed the claim back");
        assert!(!store.adapter_for_test().has_mail_for_test(), "a refusal answers nothing");
        tape::reset_for_test();
        store.pump(plx_machine::landgate::fixture_gate(), &registry_keys());
        assert_eq!(store.take_jobs().len(), 1, "the next pump asks again");
    }

    /// Replay with `"admitted":true`: the recorded run launched a worker and replay never does, so
    /// the runner answers nothing. The request stays owed, to be settled by the replayed landing.
    #[test]
    fn a_collection_job_the_tape_admits_launches_nothing_under_replay_and_stays_owed() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, store, job) = collection_request();
        let admission = collection_admission(sid, true);
        tape::init(None, true);
        tape::begin([admission.clone()].into(), Default::default());

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![admission], None));
        assert!(collection_claim_held(&store), "no worker and no refusal answered: the claim is held");
        assert!(!store.adapter_for_test().has_mail_for_test());
    }

    /// The registry moved on between the step and the launch (a retoken here): the runner does not
    /// start a request through a credential the step never asked with. The tape records the launch
    /// as not admitted, and the refusal hands the claim back.
    #[test]
    fn a_collection_job_whose_client_the_registry_has_moved_on_from_is_not_started() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, store, job) = collection_request();
        plx_plex::plex::client_for(sid).expect("registered").set_token("rotated");
        tape::init(None, false);

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![collection_admission(sid, false)], None),
            "recorded as a launch that did not happen");
        assert!(!collection_claim_held(&store), "the refusal handed the claim back");
        assert!(!store.adapter_for_test().has_mail_for_test(), "nothing was read, so nothing answered");
    }

    /// `Held` parks each admitted job, and a newer DETAIL request retires only the detail fetches
    /// parked before it: another store's parked job is not a request that detail request supersedes.
    #[test]
    fn a_newer_detail_request_leaves_a_parked_collection_job_to_run() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, mut store, job) = collection_request();
        let mut runner = StoreRunner::held();
        let mut metadata = MetadataStore::default();

        runner.start(job);
        runner.start(request(&mut metadata, ServerId::UNSET, "d1"));
        assert_eq!(runner.run_held(), 2, "the collection job was kept, and the detail job parked after it");

        assert!(store.adapter_for_test().has_mail_for_test(), "the kept job ran and answered");
        let generation = store.generation_for_test();
        store.pump(plx_machine::landgate::fixture_gate(), &registry_keys());
        assert_eq!(store.generation_for_test(), generation, "the same listing took the answer");
        assert!(!collection_claim_held(&store), "the answer released the claim");
    }

    /// A parked job runs later, and the registry may have moved on by then: the held runner
    /// refuses it, as the launch would have, so the claim is handed back and nothing answers.
    #[test]
    fn a_parked_collection_job_whose_client_is_gone_when_it_runs_is_refused() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, store, job) = collection_request();
        let mut runner = StoreRunner::held();

        runner.start(job);
        assert!(collection_claim_held(&store), "parked, still out");
        plx_plex::plex::reset_servers_for_test();
        assert_eq!(runner.run_held(), 1);

        assert!(!collection_claim_held(&store), "the refusal handed the claim back");
        assert!(!store.adapter_for_test().has_mail_for_test());
    }

    // ---- the Person lane: each outcome of a person job reaches its port once ----

    const PERSON_GUID: &str = "sf06c144a";
    /// The Media read of server 0: slot `0 * 3 + K_MEDIA`. The plex.tv reads are slots 48 and 49.
    const MEDIA: usize = 1;
    const PROFILE: usize = 48;

    /// A server registered at a port nothing listens on (a read that starts fails at once) and a
    /// person store opened on it, with the jobs its first pump emitted, keyed to the client the
    /// registry held when it did. `controlled` arms the tape first, so the pump also emits the two
    /// plex.tv reads (a controlled run needs no session). Returns the server's id, the store, and
    /// the Media read and the plex.tv reads apart.
    fn person_requests(controlled: bool) -> (ServerId, PersonStore, StoreJob, Vec<StoreJob>) {
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("runner", "127.0.0.1", 9, "t", "cid");
        assert_eq!(sid.raw(), 0, "slot arithmetic below assumes the first server");
        if controlled { tape::init(None, false); }
        let mut store = PersonStore::default();
        assert!(store.run(PersonCmd::Open {
            sid, key: "19".into(), guid: PERSON_GUID.into(), name: "Actor".into(), thumb: String::new() }));
        store.pump(plx_machine::landgate::fixture_gate(), &registry_keys());
        let (mut local, mut global) = (Vec::new(), Vec::new());
        for job in store.take_jobs() {
            let StoreJob::Person(ref j, _) = job else { panic!("not a person job") };
            if j.req.server().is_some() { local.push(job) } else { global.push(job) }
        }
        assert_eq!(local.len(), 1, "one source, one read to start with");
        (sid, store, local.remove(0), global)
    }

    /// The admission record of the Media read, `admitted` as given.
    fn media_admission(sid: ServerId, admitted: bool) -> serde_json::Value {
        let client = plx_plex::plex::client_for(sid).expect("registered").instance_gen();
        serde_json::json!({"content_resource":true,"admitted":admitted,
            "request":{"store":"person","slot":MEDIA,"gen":1,"arg":["19"],"guid":PERSON_GUID,
                "local":"19","client":client,"sid":sid.raw()}})
    }

    fn profile_admission(admitted: bool) -> serde_json::Value {
        serde_json::json!({"content_resource":true,"admitted":admitted,
            "request":{"store":"person","slot":PROFILE,"gen":1,"arg":[PERSON_GUID],"guid":PERSON_GUID}})
    }

    /// Recording, the registry still holding the client the step decided on: the worker starts,
    /// reads, and posts its ONE answer; taking it releases the claim.
    #[test]
    fn a_person_job_whose_client_is_still_held_is_admitted_and_answered_once() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, mut store, job, _) = person_requests(false);
        tape::init(None, false);

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![media_admission(sid, true)], None),
            "the request the runner admitted is the recorded shape, admitted");
        assert!(store.adapter_for_test().has_mail_for_test(MEDIA), "the worker answered");
        tape::reset_for_test();
        store.pump(plx_machine::landgate::fixture_gate(), &registry_keys());
        assert!(!store.adapter_for_test().busy_for_test(MEDIA), "taking the one answer released the claim");
    }

    /// The plex.tv reads name no client, so the registry has no say: a controlled run's Profile
    /// job is admitted and answered (the failure value) with no server registered at all.
    #[test]
    fn a_plex_tv_job_is_admitted_whatever_the_registry_holds_and_answered_once() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, store, _, mut global) = person_requests(true);
        let profile = global.remove(0);
        plx_plex::plex::reset_servers_for_test();

        StoreRunner::live().start(profile);
        plx_base::task::drain_workers_for_test();

        let (requests, failure) = tape::finish();
        assert_eq!(failure, None);
        assert_eq!(requests, vec![profile_admission(true)]);
        assert!(store.adapter_for_test().has_mail_for_test(PROFILE), "the worker answered");
    }

    /// Replay with `"admitted":false`: the recorded run's launch was refused, so nothing starts and
    /// the runner settles the request with one refusal, handing the claim back. The tape's own
    /// verdict has to read clean too.
    #[test]
    fn a_person_job_the_tape_does_not_admit_is_refused_once_and_its_claim_comes_back() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, store, job, _) = person_requests(false);
        let admission = media_admission(sid, false);
        tape::init(None, true);
        tape::begin([admission.clone()].into(), Default::default());

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![admission], None),
            "the request the runner admitted is byte-for-byte the recorded one");
        assert!(!store.adapter_for_test().busy_for_test(MEDIA), "the refusal handed the claim back");
        assert!(!store.adapter_for_test().has_mail_for_test(MEDIA), "a refusal answers nothing");
    }

    /// Replay with `"admitted":true`: the recorded run launched a worker and replay never does, so
    /// the runner answers nothing. The request stays owed, to be settled by the replayed landing.
    #[test]
    fn a_person_job_the_tape_admits_launches_nothing_under_replay_and_stays_owed() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, store, job, _) = person_requests(false);
        let admission = media_admission(sid, true);
        tape::init(None, true);
        tape::begin([admission.clone()].into(), Default::default());

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![admission], None));
        assert!(store.adapter_for_test().busy_for_test(MEDIA), "no worker and no refusal answered: the claim is held");
        assert!(!store.adapter_for_test().has_mail_for_test(MEDIA));
    }

    /// The registry moved on between the step and the launch (a retoken here): the runner does not
    /// start a request through a credential the step never asked with. The tape records the launch
    /// as not admitted, and the refusal hands the claim back.
    #[test]
    fn a_person_job_whose_client_the_registry_has_moved_on_from_is_not_started() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, store, job, _) = person_requests(false);
        plx_plex::plex::client_for(sid).expect("registered").set_token("rotated");
        tape::init(None, false);

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![media_admission(sid, false)], None),
            "recorded as a launch that did not happen");
        assert!(!store.adapter_for_test().busy_for_test(MEDIA), "the refusal handed the claim back");
        assert!(!store.adapter_for_test().has_mail_for_test(MEDIA), "nothing was read, so nothing answered");
    }

    /// `Held` parks each admitted job, and a newer DETAIL request retires only the detail fetches
    /// parked before it: parked person jobs (up to six at once) are not requests it supersedes, and
    /// each is answered exactly once when the held jobs run.
    #[test]
    fn a_newer_detail_request_leaves_parked_person_jobs_to_run() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, store, job, global) = person_requests(true);
        let mut runner = StoreRunner::held();
        let mut metadata = MetadataStore::default();

        runner.start(job);
        for job in global { runner.start(job); }
        runner.start(request(&mut metadata, ServerId::UNSET, "d1"));
        assert_eq!(runner.run_held(), 4, "the media read and both plex.tv reads were kept; the detail job parked after them");

        let adapter = store.adapter_for_test();
        for slot in [MEDIA, PROFILE, PROFILE + 1] {
            assert!(adapter.has_mail_for_test(slot), "slot {slot}'s kept job ran and answered");
        }
    }

    /// A parked job runs later, and the registry may have moved on by then: the held runner refuses
    /// it, as the launch would have, so the claim is handed back and nothing answers. A plex.tv job
    /// parked beside it names no client and runs.
    #[test]
    fn a_parked_person_job_whose_client_is_gone_when_it_runs_is_refused() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, store, job, mut global) = person_requests(true);
        let mut runner = StoreRunner::held();

        runner.start(job);
        runner.start(global.remove(0));
        let adapter = store.adapter_for_test();
        assert!(adapter.busy_for_test(MEDIA) && adapter.busy_for_test(PROFILE), "parked, still out");
        plx_plex::plex::reset_servers_for_test();
        assert_eq!(runner.run_held(), 2);

        assert!(!adapter.busy_for_test(MEDIA), "the refusal handed the claim back");
        assert!(!adapter.has_mail_for_test(MEDIA));
        assert!(adapter.has_mail_for_test(PROFILE), "the plex.tv read needs no client and answered");
    }

    // ---- the subtitle-search lane: each outcome of a search job reaches its port once ----

    /// A server registered at a port nothing listens on (a read that starts fails at once) and a
    /// search store whose first request is out: the job its pump emitted, keyed to the client the
    /// registry held when it did.
    fn search_request() -> (ServerId, SubSearchStore, StoreJob) {
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("runner", "127.0.0.1", 9, "t", "cid");
        let mut store = SubSearchStore::default();
        assert!(store.run(SubSearchCmd::Open { sid, rk: "506548".into(), lang: "nl".into() }));
        store.pump(plx_machine::landgate::fixture_gate(), &registry_keys());
        let mut jobs = store.take_jobs();
        assert_eq!(jobs.len(), 1, "one open search, one request");
        (sid, store, jobs.remove(0))
    }

    fn search_claim_held(store: &SubSearchStore) -> bool { store.adapter_for_test().busy_for_test() }

    /// Recording, the registry still holding the client the step decided on: the worker starts,
    /// reads, and posts its ONE answer; taking it releases the claim. The kind is not on the tape,
    /// so nothing is recorded: it launches live, under replay too.
    #[test]
    fn a_search_job_whose_client_is_still_held_is_started_answered_once_and_not_taped() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, mut store, job) = search_request();
        tape::init(None, false);

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![], None), "the kind is untaped: the runner records no admission");
        assert!(store.adapter_for_test().has_mail_for_test(), "the worker answered");
        tape::reset_for_test();
        store.pump(plx_machine::landgate::fixture_gate(), &registry_keys());
        assert!(!search_claim_held(&store), "the one answer released the claim");
        assert_eq!(store.view().status(), SearchStatus::Failed, "the unreachable server's failure is what landed");
        assert!(!store.has_jobs(), "a failed read is the viewer's to retry, not the pump's");
    }

    /// Replay launches the same worker the recording did: there is no admission record to consult,
    /// and the search is a live read the harness's mock server answers.
    #[test]
    fn a_search_job_launches_live_under_replay_as_the_inline_spawn_did() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, store, job) = search_request();
        tape::init(None, true);
        tape::begin(Default::default(), Default::default());

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![], None));
        assert!(store.adapter_for_test().has_mail_for_test(), "a worker started and answered");
    }

    /// The registry moved on between the step and the launch (a retoken here): the runner does not
    /// start a request through a credential the step never asked with, and the refusal hands the
    /// claim back.
    #[test]
    fn a_search_job_whose_client_the_registry_has_moved_on_from_is_refused() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, mut store, job) = search_request();
        plx_plex::plex::client_for(sid).expect("registered").set_token("rotated");

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert!(!search_claim_held(&store), "the refusal handed the claim back");
        assert!(!store.adapter_for_test().has_mail_for_test(), "nothing was read, so nothing answered");
        // The pump that sees the refusal backs off, so the request is not emitted again at once.
        store.pump(plx_machine::landgate::fixture_gate(), &registry_keys());
        assert!(!store.has_jobs(), "backed off: no request at once");
    }

    /// `Held` parks the job without running it, and a newer DETAIL request retires only the detail
    /// fetches parked before it: another store's parked job is not a request it supersedes.
    #[test]
    fn a_newer_detail_request_leaves_a_parked_search_job_to_run() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, mut store, job) = search_request();
        let mut runner = StoreRunner::held();
        let mut metadata = MetadataStore::default();

        runner.start(job);
        assert!(search_claim_held(&store), "parked, still out");
        runner.start(request(&mut metadata, ServerId::UNSET, "d1"));
        assert_eq!(runner.run_held(), 2, "the search job was kept, and the detail job parked after it");

        assert!(store.adapter_for_test().has_mail_for_test(), "the kept job ran and answered");
        store.pump(plx_machine::landgate::fixture_gate(), &registry_keys());
        assert!(!search_claim_held(&store), "the answer released the claim");
    }

    /// A parked job runs later, and the registry may have moved on by then: the held runner
    /// refuses it, as the launch would have, so the claim is handed back and nothing answers.
    #[test]
    fn a_parked_search_job_whose_client_is_gone_when_it_runs_is_refused() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, store, job) = search_request();
        let mut runner = StoreRunner::held();

        runner.start(job);
        assert!(search_claim_held(&store), "parked, still out");
        plx_plex::plex::reset_servers_for_test();
        assert_eq!(runner.run_held(), 1);

        assert!(!search_claim_held(&store), "the refusal handed the claim back");
        assert!(!store.adapter_for_test().has_mail_for_test());
    }

    /// The Bridge's frame: `subsearch_pump` refills the keys, steps, and starts the request in the
    /// one call, so a held runner has the job and the frame check is green; the held job's answer
    /// is the next pump's to take.
    #[test]
    fn the_bridge_pump_starts_the_search_it_emitted_in_the_same_call() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("runner", "127.0.0.1", 9, "t", "cid");
        let mut bridge = Bridge::for_test(|| 0);

        assert!(bridge.subsearch_run(SubSearchCmd::Open { sid, rk: "506548".into(), lang: "nl".into() }));
        bridge.assert_no_store_jobs_left_for_test();
        bridge.subsearch_pump();
        bridge.assert_no_store_jobs_left_for_test();
        assert_eq!(bridge.run_held_store_jobs_for_test(), 1, "the held runner was handed the search");

        let _frame = plx_base::task::FrameScope::enter();
        assert!(bridge.subsearch_pump(), "the answer is the next pump's to take");
        bridge.assert_no_store_jobs_left_for_test();
    }

    // ---- the search lane: each outcome of a search job reaches its port once ----

    /// A server registered at a port nothing listens on (a read that starts fails at once) and a
    /// search store whose first request is out: the job its pump emitted, keyed to the client the
    /// registry held when it did.
    fn search_store_request() -> (ServerId, SearchStore, StoreJob) {
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("runner", "127.0.0.1", 9, "t", "cid");
        let mut store = SearchStore::default();
        search_store_pump(&mut store, 0.0); // the first pump observes the roster
        assert!(store.run(SearchCmd::SetQuery("wallace".into())));
        search_store_pump(&mut store, 0.3);
        let mut jobs = store.take_jobs();
        assert_eq!(jobs.len(), 1, "one registered server, one request");
        (sid, store, jobs.remove(0))
    }

    fn search_store_pump(store: &mut SearchStore, dt: f32) {
        let directory = DirectorySnapshot::default();
        store.pump_with_directory_and_gate(dt, directory.view(), plx_machine::landgate::fixture_gate(), &registry_keys());
    }

    fn search_store_claim_held(store: &SearchStore, sid: ServerId) -> bool {
        store.adapter_for_test().busy_for_test(sid.raw() as usize)
    }

    fn search_store_has_mail(store: &SearchStore, sid: ServerId) -> bool {
        store.adapter_for_test().has_mail_for_test(sid.raw() as usize)
    }

    /// Recording, the registry still holding the client the step decided on: the worker starts,
    /// reads, and posts its ONE answer; taking it releases the claim. The kind is not on the tape,
    /// so nothing is recorded: it launches live, under replay too.
    #[test]
    fn a_search_fetch_whose_client_is_still_held_is_started_answered_once_and_not_taped() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, mut store, job) = search_store_request();
        tape::init(None, false);

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![], None), "the kind is untaped: the runner records no admission");
        assert!(search_store_has_mail(&store, sid), "the worker answered");
        tape::reset_for_test();
        search_store_pump(&mut store, 0.016);
        assert!(!search_store_claim_held(&store, sid), "the one answer released the claim");
        assert!(!store.has_jobs(), "a failed fetch is backed off, not asked again at once");
    }

    /// Replay launches the same worker the recording did: there is no admission record to consult,
    /// and the search is a live read the harness's mock server answers.
    #[test]
    fn a_search_fetch_launches_live_under_replay_as_the_inline_spawn_did() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, store, job) = search_store_request();
        tape::init(None, true);
        tape::begin(Default::default(), Default::default());

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![], None));
        assert!(search_store_has_mail(&store, sid), "a worker started and answered");
    }

    /// The registry moved on between the step and the launch (a retoken here): the runner does not
    /// start a fetch through a credential the step never asked with, and the refusal hands the
    /// claim back. The pump that sees it backs the source off, so the fetch is not emitted again at
    /// once, which is the respawn spin a refused inline spawn used to be.
    #[test]
    fn a_search_fetch_whose_client_the_registry_has_moved_on_from_is_refused_and_backs_off() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, mut store, job) = search_store_request();
        plx_plex::plex::client_for(sid).expect("registered").set_token("rotated");

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert!(!search_store_claim_held(&store, sid), "the refusal handed the claim back");
        assert!(!search_store_has_mail(&store, sid), "nothing was read, so nothing answered");
        search_store_pump(&mut store, 0.016);
        assert!(!store.has_jobs(), "backed off: no request at once");
        for _ in 0..30 {
            search_store_pump(&mut store, 0.016);
            assert!(!store.has_jobs(), "and none while the wait runs");
        }
    }

    /// `Held` parks the job without running it, and a newer DETAIL request retires only the detail
    /// fetches parked before it: another store's parked job is not a request it supersedes.
    #[test]
    fn a_newer_detail_request_leaves_a_parked_search_fetch_to_run() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, mut store, job) = search_store_request();
        let mut runner = StoreRunner::held();
        let mut metadata = MetadataStore::default();

        runner.start(job);
        assert!(search_store_claim_held(&store, sid), "parked, still out");
        runner.start(request(&mut metadata, ServerId::UNSET, "d1"));
        assert_eq!(runner.run_held(), 2, "the search job was kept, and the detail job parked after it");

        assert!(search_store_has_mail(&store, sid), "the kept job ran and answered");
        search_store_pump(&mut store, 0.016);
        assert!(!search_store_claim_held(&store, sid), "the answer released the claim");
    }

    /// A parked job runs later, and the registry may have moved on by then: the held runner
    /// refuses it, as the launch would have, so the claim is handed back and nothing answers.
    #[test]
    fn a_parked_search_fetch_whose_client_is_gone_when_it_runs_is_refused() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, mut store, job) = search_store_request();
        let mut runner = StoreRunner::held();

        runner.start(job);
        assert!(search_store_claim_held(&store, sid), "parked, still out");
        plx_plex::plex::reset_servers_for_test();
        assert_eq!(runner.run_held(), 1);

        assert!(!search_store_claim_held(&store, sid), "the refusal handed the claim back");
        assert!(!search_store_has_mail(&store, sid));
        search_store_pump(&mut store, 0.016);
        assert!(!store.has_jobs());
    }

    /// The Bridge's frame: `search_pump` refills the keys, steps, and starts the fetch in the one
    /// call, so a held runner has the job and the frame check is green; the held job's answer is
    /// the next pump's to take.
    #[test]
    fn the_bridge_pump_starts_the_search_fetch_it_emitted_in_the_same_call() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        plx_plex::plex::reset_servers_for_test();
        plx_plex::plex::register_for_test("runner", "127.0.0.1", 9, "t", "cid");
        let mut bridge = Bridge::for_test(|| 0);

        bridge.search_pump(0.0); // the first pump observes the roster
        assert!(bridge.search_run(SearchCmd::SetQuery("wallace".into())));
        bridge.assert_no_store_jobs_left_for_test();
        bridge.search_pump(0.3);
        bridge.assert_no_store_jobs_left_for_test();
        assert_eq!(bridge.run_held_store_jobs_for_test(), 1, "the held runner was handed the fetch");

        let _frame = plx_base::task::FrameScope::enter();
        assert!(bridge.search_pump(0.016), "the answer is the next pump's to take");
        bridge.assert_no_store_jobs_left_for_test();
    }

    // ---- the recents lane: a save job reaches its port once ----

    /// A recents store whose history has one edit queued for the profile the test is watching, and
    /// the job that edit emitted.
    fn save_request() -> (SearchStore, StoreJob) {
        let mut store = SearchStore::default();
        let generation = plx_data::search::recents::snapshot().generation();
        assert!(store.run(SearchCmd::RememberRecent { profile_generation: generation, term: "alpha".into() }));
        let mut jobs = store.take_jobs();
        assert_eq!(jobs.len(), 1, "one edit, one write");
        (store, jobs.remove(0))
    }

    fn saved(profile: &str) -> Vec<String> {
        plx_plex::plex::session::peek().recents_for(profile).to_vec()
    }

    /// The worker starts, drains the queue into the session file and answers; the kind is not on the
    /// tape, so nothing is recorded.
    #[test]
    fn a_save_job_is_started_writes_the_history_and_is_not_taped() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let session = plx_plex::plex::session::TempSession::new("runner-recents-live");
        session.watching("runner-recents-live");
        let (_store, job) = save_request();
        assert!(saved("runner-recents-live").is_empty(), "nothing is written until the job runs");
        tape::init(None, false);

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![], None), "the kind is untaped: the runner records no admission");
        assert_eq!(saved("runner-recents-live"), ["alpha"], "the worker wrote the queued history");
    }

    /// `Held` parks the job without running it, and a newer DETAIL request retires only the detail
    /// fetches parked before it: the parked write is not a request it supersedes, and runs once when
    /// the test runs the held jobs.
    #[test]
    fn a_newer_detail_request_leaves_a_parked_save_job_to_run() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let session = plx_plex::plex::session::TempSession::new("runner-recents-held");
        session.watching("runner-recents-held");
        let (_store, job) = save_request();
        let mut runner = StoreRunner::held();
        let mut metadata = MetadataStore::default();

        runner.start(job);
        assert!(saved("runner-recents-held").is_empty(), "parked, still unwritten");
        runner.start(request(&mut metadata, ServerId::UNSET, "d1"));
        assert_eq!(runner.run_held(), 2, "the save job was kept, and the detail job parked after it");

        assert_eq!(saved("runner-recents-held"), ["alpha"]);
    }

    /// The Bridge's recents edit: `search_run` takes the write its step emitted and hands it to the
    /// runner in the one call, so the frame check is green and the held job writes when it is run.
    #[test]
    fn the_bridge_starts_the_recents_write_an_edit_emitted_in_the_same_call() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let session = plx_plex::plex::session::TempSession::new("runner-recents-bridge");
        session.watching("runner-recents-bridge");
        let mut bridge = Bridge::for_test(|| 0);
        let generation = plx_data::search::recents::snapshot().generation();

        assert!(bridge.search_run(SearchCmd::RememberRecent { profile_generation: generation, term: "alpha".into() }));
        bridge.assert_no_store_jobs_left_for_test();
        assert!(saved("runner-recents-bridge").is_empty(), "held, not yet written");
        assert_eq!(bridge.run_held_store_jobs_for_test(), 1, "the held runner was handed the write");
        assert_eq!(saved("runner-recents-bridge"), ["alpha"]);
    }

    // ---- the view-reconcile lane: each outcome of a read-back job reaches its port once ----

    /// A server registered at a port nothing listens on (a read that starts fails at once) and a
    /// reconcile store whose first batch, item `1`, is out: the job its pump sent, keyed to the
    /// client the registry held when it did.
    fn reconcile_request() -> (ServerId, ViewReconcileStore, StoreJob) {
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("runner", "127.0.0.1", 9, "t", "cid");
        let mut store = ViewReconcileStore::default();
        store.enqueue([(sid, "1".to_string())]);
        reconcile_pump(&mut store, false);
        let mut jobs = store.take_jobs();
        assert_eq!(jobs.len(), 1, "one queued item, one batch");
        (sid, store, jobs.remove(0))
    }

    /// One pump of the store with the registry's answer as of now; `held` is true while a write is
    /// queued or out (nothing is sent then, but landings are taken).
    fn reconcile_pump(store: &mut ViewReconcileStore, held: bool) {
        store.pump_with_gate(plx_machine::landgate::fixture_gate(), !held, &registry_keys(), &mut |_| {});
    }

    /// Recording, the registry still holding the client the step decided on: the worker starts,
    /// reads, and posts its ONE answer; taking it settles the flight and requeues nothing (a read
    /// that ran and failed is not retried). The kind is not on the tape: nothing is recorded.
    #[test]
    fn a_reconcile_job_whose_client_is_still_held_is_started_answered_once_and_not_taped() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, mut store, job) = reconcile_request();
        tape::init(None, false);

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![], None), "the kind is untaped: the runner records no admission");
        tape::reset_for_test();
        assert_eq!(store.flights_for_test(), 1, "the answer is the next pump's to take");
        reconcile_pump(&mut store, true);
        assert_eq!(store.flights_for_test(), 0, "the one answer settled the flight");
        assert!(store.pending_for_test().is_empty(), "a read that ran and failed is not retried");
        assert!(!store.has_jobs());
    }

    /// Replay launches the same worker the recording did: there is no admission record to consult.
    #[test]
    fn a_reconcile_job_launches_live_under_replay_as_the_inline_spawn_did() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, mut store, job) = reconcile_request();
        tape::init(None, true);
        tape::begin(Default::default(), Default::default());

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![], None));
        tape::reset_for_test();
        reconcile_pump(&mut store, true);
        assert_eq!(store.flights_for_test(), 0, "a worker started and answered");
        assert!(store.pending_for_test().is_empty());
    }

    /// The registry moved on between the step and the launch (a retoken here): the runner does not
    /// start a read through a credential the step never asked with. The refusal is the flight's
    /// answer, so the next pump settles it and puts the batch back in the queue.
    #[test]
    fn a_reconcile_job_whose_client_the_registry_has_moved_on_from_is_refused_and_requeued() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, mut store, job) = reconcile_request();
        plx_plex::plex::client_for(sid).expect("registered").set_token("rotated");

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(store.flights_for_test(), 1, "the refusal is the next pump's to take");
        reconcile_pump(&mut store, true);
        assert_eq!(store.flights_for_test(), 0, "the refusal settled the flight");
        assert_eq!(store.pending_for_test(), [(sid, "1".to_string())], "nothing was read: the item is owed again");
        assert!(!store.has_jobs(), "...after the wait, not at once");
    }

    /// `Held` parks the job without running it, and a newer DETAIL request retires only the detail
    /// fetches parked before it: another store's parked job is not a request it supersedes.
    #[test]
    fn a_newer_detail_request_leaves_a_parked_reconcile_job_to_run() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, mut store, job) = reconcile_request();
        let mut runner = StoreRunner::held();
        let mut metadata = MetadataStore::default();

        runner.start(job);
        runner.start(request(&mut metadata, ServerId::UNSET, "d1"));
        assert_eq!(runner.run_held(), 2, "the reconcile job was kept, and the detail job parked after it");

        reconcile_pump(&mut store, true);
        assert_eq!(store.flights_for_test(), 0, "the kept job ran and answered");
        assert!(store.pending_for_test().is_empty());
    }

    /// A parked job runs later, and the registry may have moved on by then: the held runner
    /// refuses it, as the launch would have, so the flight is answered and its item is owed again.
    #[test]
    fn a_parked_reconcile_job_whose_client_is_gone_when_it_runs_is_refused() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, mut store, job) = reconcile_request();
        let mut runner = StoreRunner::held();

        runner.start(job);
        assert_eq!(store.flights_for_test(), 1, "parked, still out");
        plx_plex::plex::reset_servers_for_test();
        assert_eq!(runner.run_held(), 1);

        reconcile_pump(&mut store, true);
        assert_eq!(store.flights_for_test(), 0, "the refusal settled the flight");
        assert_eq!(store.pending_for_test(), [(sid, "1".to_string())]);
    }

    /// The Bridge's frame: `reconcile_pump` refills the keys, steps, and starts the read it sent in
    /// the one call, so a held runner has the job and the frame check is green; the held job's
    /// answer is the next pump's to take.
    #[test]
    fn the_bridge_pump_starts_the_read_it_sent_in_the_same_call() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("runner", "127.0.0.1", 9, "t", "cid");
        let mut bridge = Bridge::for_test(|| 0);

        bridge.owe_read_back(vec![(sid, "1".to_string())]);
        let _ = bridge.reconcile_pump();
        bridge.assert_no_store_jobs_left_for_test();
        assert!(bridge.read_back_pending_for_test().is_empty(), "the item left in a batch");
        assert_eq!(bridge.run_held_store_jobs_for_test(), 1, "the held runner was handed the read");

        let _ = bridge.reconcile_pump();
        bridge.assert_no_store_jobs_left_for_test();
        assert!(bridge.read_back_pending_for_test().is_empty(), "the answer settled the flight; nothing is owed");
    }

    // ---- the view-write lane: a household write starts only through the client the step chose ----

    /// A loopback listener that counts the connections made to it and answers none: a write sent
    /// here fails at once, so what the count shows is whether the write was sent at all.
    struct Sink {
        port: i32,
        hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Sink {
        fn new() -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            use std::sync::Arc;
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
            listener.set_nonblocking(true).expect("nonblocking");
            let port = listener.local_addr().expect("bound").port() as i32;
            let (hits, stop) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
            let (count, halt) = (Arc::clone(&hits), Arc::clone(&stop));
            let thread = std::thread::spawn(move || {
                while !halt.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok(_) => { count.fetch_add(1, Ordering::SeqCst); }
                        Err(_) => std::thread::sleep(std::time::Duration::from_millis(1)),
                    }
                }
            });
            Self { port, hits, stop, thread: Some(thread) }
        }

        /// The connections made, after giving the listener time to accept what was sent.
        fn settled_hits(&self) -> usize {
            std::thread::sleep(std::time::Duration::from_millis(60));
            self.hits.load(std::sync::atomic::Ordering::SeqCst)
        }

        /// Wait (bounded) for the first connection.
        fn wait_for_a_hit(&self) -> usize {
            for _ in 0..400 {
                if self.hits.load(std::sync::atomic::Ordering::SeqCst) > 0 { break; }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            self.settled_hits()
        }
    }

    impl Drop for Sink {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(thread) = self.thread.take() { let _ = thread.join(); }
        }
    }

    /// The profile identity the presses of this lane are made under.
    const VIEW_PROFILE: u32 = 7;

    /// A server registered at `sink` and a ViewState owner whose first write, `Watched` on item
    /// `1`, is out: the job its step sent, keyed to the client the registry held when it did.
    fn view_write_request(sink: &Sink) -> (ServerId, plx_data::stores::Stores, StoreJob) {
        use plx_data::stores::viewstate::ViewStateCmd;
        use plx_data::viewstate::{Scope, Write};
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("runner", "127.0.0.1", sink.port, "t", "cid");
        plx_plex::plex::client_for(sid).expect("registered").set_link(plx_plex::plex::probe::Location::Local);
        let mut stores = plx_data::stores::Stores::default();
        let directory = DirectorySnapshot::default();
        assert!(stores.viewstate_run(
            ViewStateCmd::Request { sid, rk: "1".into(), write: Write::Watched, scope: Scope::Leaf, guid: String::new() },
            directory.view(), &registry_keys(), VIEW_PROFILE).changed);
        let mut jobs = stores.take_jobs_of(plx_data::stores::StoreId::ViewState);
        assert_eq!(jobs.len(), 1, "one press, one write");
        (sid, stores, jobs.remove(0))
    }

    /// One pump of the ViewState owner with the registry's answer as of now.
    fn view_pump(stores: &mut plx_data::stores::Stores, profile: u32) {
        let directory = DirectorySnapshot::default();
        let _ = stores.viewstate_pump(directory.view(), &registry_keys(), profile);
    }

    /// The write is not out any more and nothing is queued behind it.
    fn view_idle(stores: &plx_data::stores::Stores) -> bool {
        !stores.viewstate.borrow().is_busy_for_test()
    }

    /// The step judged the refused write again from its own inputs and dropped it: no job goes
    /// out under the credential the registry holds now, nothing is queued or in flight, and the
    /// press is retired from the ledger.
    fn assert_dropped_not_sent_again(stores: &mut plx_data::stores::Stores) {
        assert!(
            stores.take_jobs_of(plx_data::stores::StoreId::ViewState).is_empty(),
            "a key that moved is never sent again under the new one"
        );
        assert!(view_idle(stores), "nothing is queued or in flight");
        assert!(stores.view_ledger.since(0).is_empty(), "the press is retired");
    }

    /// Recording, the registry still holding the client the step chose: the worker starts, sends
    /// the write, and posts its ONE answer; taking it settles the write. The kind is not on the
    /// tape: nothing is recorded.
    #[test]
    fn a_view_write_whose_client_is_still_held_is_started_answered_once_and_not_taped() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let sink = Sink::new();
        let (_, mut stores, job) = view_write_request(&sink);
        tape::init(None, false);

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![], None), "the kind is untaped: the runner records no admission");
        tape::reset_for_test();
        assert_eq!(sink.wait_for_a_hit(), 1, "the write left the device once");
        assert!(!view_idle(&stores), "the answer is the next pump's to take");
        view_pump(&mut stores, VIEW_PROFILE);
        assert!(view_idle(&stores), "the one answer settled the write");
        assert!(stores.view_ledger.since(0).is_empty(), "the sink took none, so the press is retired");
        assert!(stores.take_jobs_of(plx_data::stores::StoreId::ViewState).is_empty(), "and no write is sent again");
    }

    /// The registry moved on between the step and the launch (a retoken here): the runner does not
    /// start a write through a credential the step never decided on, and nothing leaves the device.
    /// The refusal is the write's answer, and the next pump drops the write rather than sending it
    /// under the credential the registry holds now (it cannot tell a switch from a refresh).
    #[test]
    fn a_view_write_whose_origin_key_moved_is_not_started() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let sink = Sink::new();
        let (sid, mut stores, job) = view_write_request(&sink);
        plx_plex::plex::client_for(sid).expect("registered").set_token("rotated");

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(sink.settled_hits(), 0, "nothing was sent through the credential the step never asked with");
        assert!(!view_idle(&stores), "the refusal is the next pump's to take");
        view_pump(&mut stores, VIEW_PROFILE);
        assert_dropped_not_sent_again(&mut stores);
    }

    /// `Held` parks the write without running it, and a newer DETAIL request retires only the
    /// detail fetches parked before it: a household write is not a request it supersedes, and the
    /// write that was parked still runs and answers.
    #[test]
    fn a_parked_view_write_is_kept_by_retire_parked_details() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let sink = Sink::new();
        let (_, mut stores, job) = view_write_request(&sink);
        let mut runner = StoreRunner::held();
        let mut metadata = MetadataStore::default();

        runner.start(job);
        runner.start(request(&mut metadata, ServerId::UNSET, "d1"));
        assert_eq!(runner.run_held(), 2, "the write was kept, and the detail job parked after it");

        assert_eq!(sink.wait_for_a_hit(), 1, "the kept write ran");
        view_pump(&mut stores, VIEW_PROFILE);
        assert!(view_idle(&stores));
    }

    /// A parked write runs later and answers through the production completion path, once.
    #[test]
    fn a_held_view_write_runs_and_answers_once() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let sink = Sink::new();
        let (_, mut stores, job) = view_write_request(&sink);
        let mut runner = StoreRunner::held();

        runner.start(job);
        assert!(!view_idle(&stores), "parked, still out");
        assert_eq!(sink.settled_hits(), 0, "and not sent");
        assert_eq!(runner.run_held(), 1);
        assert_eq!(runner.run_held(), 0, "the job ran, so nothing is left to run");

        assert_eq!(sink.wait_for_a_hit(), 1, "the write left the device once");
        view_pump(&mut stores, VIEW_PROFILE);
        assert!(view_idle(&stores), "the one answer settled the write");
        view_pump(&mut stores, VIEW_PROFILE);
        assert!(view_idle(&stores), "a second pump finds nothing more");
        assert!(stores.take_jobs_of(plx_data::stores::StoreId::ViewState).is_empty(), "and sends no write again");
    }

    /// A parked write runs later, and the registry may have moved on by then: the held runner
    /// refuses it as the launch would have, so nothing is sent and the next pump drops the write.
    #[test]
    fn a_parked_view_write_whose_origin_key_moved_before_it_runs_is_refused() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let sink = Sink::new();
        let (sid, mut stores, job) = view_write_request(&sink);
        let mut runner = StoreRunner::held();

        runner.start(job);
        plx_plex::plex::client_for(sid).expect("registered").set_token("rotated");
        assert_eq!(runner.run_held(), 1);

        assert_eq!(sink.settled_hits(), 0, "the parked write was not sent through the moved credential");
        view_pump(&mut stores, VIEW_PROFILE);
        assert_dropped_not_sent_again(&mut stores);
    }

    // ---- the Bridge's drain, which feeds the runner ----

    /// What `playback::describe_movie` does: a command run on the bare store, which emits a job.
    fn run_on_the_bare_store(bridge: &mut Bridge) {
        let (sid, rk) = (ServerId::UNSET, "42".to_string());
        assert!(bridge.metadata_mut().run(MetadataCmd::RequestDetail { sid, rk }));
    }

    fn run_the_detail_pump(bridge: &mut Bridge) {
        let _frame = plx_base::task::FrameScope::enter();
        bridge.metadata_pump_detail();
    }

    /// The two ways a sender keeps its job out of the backstop: through `Bridge::metadata_run`, or
    /// by draining right after a command on the bare store. Both leave the frame check green after
    /// the pump has run.
    #[test]
    fn a_sender_that_drains_its_own_job_leaves_nothing_for_the_backstop() {
        let _serial = plx_base::testlock::serial();
        let mut bridge = Bridge::for_test(|| 0);

        assert!(bridge.metadata_run(MetadataCmd::RequestDetail { sid: ServerId::UNSET, rk: "42".into() }));
        run_the_detail_pump(&mut bridge);
        bridge.assert_no_store_jobs_left_for_test();

        run_on_the_bare_store(&mut bridge);
        assert_eq!(bridge.start_store_jobs(), 1, "the drain started the job the bare command emitted");
        run_the_detail_pump(&mut bridge);
        bridge.assert_no_store_jobs_left_for_test();
    }

    /// A sender that does neither is caught at the frame's end while its job is still on the outbox.
    #[test]
    #[should_panic(expected = "sits on an outbox")]
    fn a_job_left_on_an_outbox_fails_the_frame_check() {
        let _serial = plx_base::testlock::serial();
        let mut bridge = Bridge::for_test(|| 0);
        run_on_the_bare_store(&mut bridge);
        bridge.assert_no_store_jobs_left_for_test();
    }

    /// ...and still caught once the detail pump has started it. The outbox is empty by then, so a
    /// check that read only the outbox would pass, which is the frame `update()` ends with: the
    /// backstop rescues the request, and the count is what keeps the bypass from being silent.
    #[test]
    #[should_panic(expected = "backstop had to start")]
    fn a_job_the_pump_had_to_start_late_fails_the_frame_check_though_the_outbox_is_empty() {
        let _serial = plx_base::testlock::serial();
        let mut bridge = Bridge::for_test(|| 0);
        run_on_the_bare_store(&mut bridge);
        run_the_detail_pump(&mut bridge);
        assert!(!bridge.metadata_mut().has_jobs(), "the backstop started it: the outbox is empty");
        bridge.assert_no_store_jobs_left_for_test();
    }

    // ---- the catalog lane: a plex.tv read, no tape record and no client to resolve ----

    use plx_data::catalog::CatalogIo;
    use plx_data::stores::catalog::CatalogStore;
    use plx_data::watchlist::WatchlistAuth;
    use plx_plex::plex::watchlist::{CatalogItem, WatchItem, WatchlistError};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    const HEX: [u8; 12] = [7; 12];

    /// A scripted plex.tv: every read answers with a title.
    struct Scripted {
        reads: AtomicUsize,
    }

    impl Scripted {
        fn new() -> Arc<Self> {
            Arc::new(Self { reads: AtomicUsize::new(0) })
        }
    }

    impl CatalogIo for Scripted {
        fn fetch(&self, _: &WatchlistAuth, hex: &str) -> Result<CatalogItem, WatchlistError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(CatalogItem {
                item: WatchItem { hex: hex.into(), title: Some("T".into()), ..Default::default() },
                genres: Vec::new(),
                content_rating: None,
                duration_ms: None,
            })
        }
        fn today(&self) -> Option<String> {
            None
        }
    }

    fn catalog_request(io: &Arc<Scripted>) -> (CatalogStore, StoreJob) {
        let mut store = CatalogStore::with_io(io.clone());
        store.open(HEX, &plx_data::watchlist::WatchlistInput::for_test(1));
        let mut jobs = store.take_jobs();
        assert_eq!(jobs.len(), 1, "an open emits one job");
        (store, jobs.remove(0))
    }

    /// Live: the worker starts, reads once and posts its ONE answer; the kind is not on the tape,
    /// so nothing is recorded, and the answer is the next pump's to take, once.
    #[test]
    fn a_catalog_job_is_started_answered_once_and_not_taped() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let io = Scripted::new();
        let (mut store, job) = catalog_request(&io);
        tape::init(None, false);

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![], None), "the kind is untaped: the runner records no admission");
        tape::reset_for_test();
        assert_eq!(io.reads.load(Ordering::SeqCst), 1, "one read");
        assert!(store.pump(&plx_data::watchlist::WatchlistInput::profile_for_test(1)), "the answer landed");
        assert!(matches!(store.load, plx_data::catalog::Load::Loaded(_)));
        assert!(!store.pump(&plx_data::watchlist::WatchlistInput::profile_for_test(1)), "...once");
        assert!(!store.has_jobs());
    }

    /// Held parks the job without running it, and `run_held` runs it through the same completion
    /// path: one answer, and nothing left parked.
    #[test]
    fn a_held_catalog_job_runs_once_when_the_test_runs_it() {
        let _serial = plx_base::testlock::serial();
        let io = Scripted::new();
        let (mut store, job) = catalog_request(&io);
        let mut runner = StoreRunner::held();

        runner.start(job);
        assert!(!store.pump(&plx_data::watchlist::WatchlistInput::profile_for_test(1)), "parked: nothing has run");
        assert_eq!(io.reads.load(Ordering::SeqCst), 0);
        assert_eq!(runner.run_held(), 1);
        assert!(store.pump(&plx_data::watchlist::WatchlistInput::profile_for_test(1)));
        assert_eq!(runner.run_held(), 0, "nothing left parked");
        assert_eq!(io.reads.load(Ordering::SeqCst), 1);
    }

    /// A newer DETAIL request retires only the detail fetches parked before it: a catalog read is
    /// not a request it supersedes. `run_held_of` takes one store's jobs and leaves the rest.
    #[test]
    fn a_newer_detail_request_leaves_a_parked_catalog_job_to_run() {
        let _serial = plx_base::testlock::serial();
        let io = Scripted::new();
        let (mut store, job) = catalog_request(&io);
        let mut runner = StoreRunner::held();
        let mut metadata = MetadataStore::default();

        runner.start(job);
        runner.start(request(&mut metadata, ServerId::UNSET, "d1"));
        assert_eq!(runner.run_held_of(plx_data::stores::StoreId::Catalog), 1, "the catalog read was kept");
        assert!(store.pump(&plx_data::watchlist::WatchlistInput::profile_for_test(1)));
        assert_eq!(runner.run_held(), 1, "the detail job parked after it is still there for its own test");
    }

    /// The Bridge's catalog entry points start the read they emitted in the same call, the pump's
    /// re-open under a new profile included, so the frame check finds nothing on an outbox.
    #[test]
    fn the_bridge_catalog_entry_points_start_the_read_they_emitted_in_the_same_call() {
        let _serial = plx_base::testlock::serial();
        let io = Scripted::new();
        let mut bridge = Bridge::for_test(|| 0);
        *bridge.catalog_for_test() = CatalogStore::with_io(io.clone());
        bridge.seat_watchlist_account_for_test(true);

        bridge.catalog_open(HEX);
        bridge.assert_no_store_jobs_left_for_test();
        assert_eq!(bridge.run_held_store_jobs_for_test(), 1, "the open's read was parked by the runner");
        // The app seats another profile: the pass drops the old profile's landing and owes the title a
        // read, which the same call opens under the identity read now.
        bridge.seat_watchlist_account_for_test(true);
        assert!(bridge.catalog_pump(), "the landing is for a profile that is gone: the page asks again");
        bridge.assert_no_store_jobs_left_for_test();
        assert_eq!(bridge.run_held_store_jobs_for_test(), 1, "the pump's re-open was started in the pump's own call");
        assert!(bridge.catalog_pump());
        assert!(matches!(bridge.catalog_for_test().load, plx_data::catalog::Load::Loaded(_)));
        bridge.assert_no_store_jobs_left_for_test();
    }

    // ---- the Hubs lane: each outcome of a Hubs job reaches its port once ----

    /// A server registered at a port nothing listens on and a Hubs store whose refetch is out: the
    /// job its step emitted, keyed to the client the registry held when it did.
    fn hubs_request() -> (ServerId, HubsStore, StoreJob) {
        plx_plex::plex::reset_servers_for_test();
        let mut hubs = HubsStore::default();
        assert!(hubs.run(HubsCmd::Reset).changed);
        let sid = plx_plex::plex::register_for_test("runner", "127.0.0.1", 9, "t", "cid");
        assert!(plx_plex::plex::set_current(sid));
        let directory = DirectorySnapshot::default();
        assert!(hubs.run_with_directory(HubsCmd::RefetchHubs, directory.view(), &registry_keys()).changed);
        let mut jobs = hubs.take_jobs();
        assert_eq!(jobs.len(), 1, "one source, one read");
        (sid, hubs, jobs.remove(0))
    }

    /// The request id the job's one answer is filed under.
    fn hubs_id(job: &StoreJob) -> u32 {
        let StoreJob::Hubs(job, _) = job else { panic!("not a Hubs job") };
        job.addr.req.0
    }

    /// What a controlled Home run records for the job, `admitted` as given.
    fn hubs_record(job: &StoreJob, admitted: bool) -> serde_json::Value {
        let StoreJob::Hubs(job, _) = job else { panic!("not a Hubs job") };
        let mut record = job.req.admission();
        record["admitted"] = serde_json::json!(admitted);
        record
    }

    /// A controlled Home run's ledger: recording (`replay: false`), or replaying `admissions`.
    fn home_io(replay: bool, admissions: Vec<serde_json::Value>) -> HomeIo {
        let mt = unsafe { plx_base::task::MainThread::assume() };
        let publisher = plx_plex::plex::session::ProfilePublisher::scoped(&mt);
        HomeIo { replay, preferences: Default::default(), requests: Vec::new(),
            admissions: admissions.into(), failure: None, profile: publisher.snapshot() }
    }

    /// The registry holds a different client for `sid` than the one the job's step decided on, as
    /// after a sign-out and a sign-in to the same slot.
    fn replace_the_client() {
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("runner", "127.0.0.1", 9, "t2", "cid2");
        assert!(plx_plex::plex::set_current(sid));
    }

    /// The client `sid` names signs in again under a new token: its key moves on, and the table the
    /// Browse store holds is still the one it was (a re-registration would resync the roster).
    fn move_the_key(sid: ServerId) {
        plx_plex::plex::client_for(sid).expect("registered").set_token("new");
    }

    /// Recording, the registry still holding the client the step decided on: the claim is raised on
    /// this thread before the worker exists, the worker posts its ONE answer, and taking it lowers
    /// the claim.
    #[test]
    fn a_hubs_job_whose_client_is_still_held_runs_on_a_worker_and_answers_once() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, hubs, job) = hubs_request();
        assert!(!hubs.owed(), "nothing is out until the runner launches it");

        StoreRunner::live().start(job);
        assert!(hubs.owed(), "the claim was raised by the launch, before any answer");
        plx_base::task::drain_workers_for_test();

        assert_eq!(hubs.take_results().len(), 1, "the worker answered once");
        assert!(!hubs.owed(), "taking the one answer lowered the claim");
        assert!(hubs.refused_for_test().is_empty(), "a launch that happened is never also refused");
        assert!(!hubs.has_jobs());
    }

    /// The registry moved on between the step and the launch: nothing starts, no claim is raised,
    /// and the one answer is the refusal.
    #[test]
    fn a_hubs_job_whose_client_is_gone_is_refused_once_and_raises_no_claim() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, hubs, job) = hubs_request();
        let id = hubs_id(&job);
        replace_the_client();

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(hubs.refused_for_test(), [id], "one refusal, filed under the request");
        assert!(!hubs.owed(), "a launch that never happened holds no claim");
        assert!(hubs.take_results().is_empty(), "no worker, so no landing");
    }

    /// The system refuses the thread, below the claim: the claim is given back and the one answer
    /// is the refusal.
    #[test]
    fn a_hubs_job_the_system_refuses_to_start_gives_its_claim_back_and_is_refused_once() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, hubs, job) = hubs_request();
        let id = hubs_id(&job);

        with_refused_hubs_launches_for_test(|| StoreRunner::live().start(job));
        plx_base::task::drain_workers_for_test();

        assert_eq!(hubs.refused_for_test(), [id]);
        assert!(!hubs.owed(), "the refused launch gave its claim back");
        assert!(hubs.take_results().is_empty());
    }

    /// `held_all` parks the job with its claim up, and `run_held` runs it on the test thread
    /// through the production completion: one answer, nothing left parked.
    #[test]
    fn a_held_hubs_job_holds_its_claim_and_runs_once_when_the_test_runs_it() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, hubs, job) = hubs_request();
        let mut runner = StoreRunner::held_all();

        runner.start(job);
        assert!(hubs.owed(), "a parked job is a worker that has not answered");
        assert!(hubs.take_results().is_empty());

        assert_eq!(runner.run_held_of(plx_data::stores::StoreId::Hubs), 1);
        assert_eq!(hubs.take_results().len(), 1);
        assert!(!hubs.owed());
        assert_eq!(runner.run_held(), 0, "nothing left parked");
        assert!(hubs.refused_for_test().is_empty());
    }

    /// A parked job whose client is gone by the time the test runs it gives the claim back and is
    /// refused, as the launch would have refused it.
    #[test]
    fn a_held_hubs_job_whose_client_is_gone_by_then_gives_its_claim_back_and_is_refused() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, hubs, job) = hubs_request();
        let id = hubs_id(&job);
        let mut runner = StoreRunner::held_all();

        runner.start(job);
        assert!(hubs.owed());
        replace_the_client();
        assert_eq!(runner.run_held(), 1);

        assert_eq!(hubs.refused_for_test(), [id]);
        assert!(!hubs.owed(), "the gone branch lowered the claim the park raised");
        assert!(hubs.take_results().is_empty());
    }

    /// `held` leaves the Hubs read live, as production runs it: a rig that waits for a Home landing
    /// gets a real one, and nothing is parked for it.
    #[test]
    fn the_held_runner_starts_a_hubs_job_on_a_worker_as_production_does() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, hubs, job) = hubs_request();
        let mut runner = StoreRunner::held();

        runner.start(job);
        assert_eq!(runner.run_held(), 0, "nothing was parked");
        plx_base::task::drain_workers_for_test();

        assert_eq!(hubs.take_results().len(), 1, "the worker answered");
        assert!(!hubs.owed());
    }

    // ---- the section hubs' lane: a read the Browse store's step claimed, started or refused here ----

    use plx_data::stores::browse::{BrowseCmd, BrowseStore, LibraryWork, SectionAddress};
    use plx_plex::plex::ClientKey;

    /// The command that asks the current section for its hubs.
    fn browse_hubs_cmd(browse: &mut BrowseStore) -> BrowseCmd {
        let id = browse.hubs_snapshot().view().id().expect("a section is current");
        let target = SectionAddress { epoch: id.epoch, sid: id.sid, section: id.section };
        BrowseCmd::Addressed { target, work: LibraryWork::Hubs { may_publish: true } }
    }

    /// A server registered at a port nothing listens on and a Browse store whose current section's
    /// hubs read is out: the job its step emitted, keyed to the client the registry held when it did,
    /// with the section hubs' one claim up from that step.
    fn browse_hubs_request() -> (ServerId, BrowseStore, StoreJob) {
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("runner", "127.0.0.1", 9, "t", "cid");
        let other = plx_plex::plex::register_for_test("runner-shared", "127.0.0.1", 10, "t", "cid");
        assert!(plx_plex::plex::set_current(sid));
        let mut browse = BrowseStore::default();
        browse.seed_registered_table_for_test([sid, other]);
        browse.run(BrowseCmd::SetCur(0), &registry_keys());
        let cmd = browse_hubs_cmd(&mut browse);
        browse.run(cmd, &registry_keys());
        let mut jobs = browse.take_jobs();
        assert_eq!(jobs.len(), 1, "one section, one read");
        let job = jobs.remove(0);
        assert!(matches!(job, StoreJob::BrowseHubs(..)), "a section-hubs job");
        assert!(browse.hubs_read_out_for_test(), "the step raised the one claim");
        (sid, browse, job)
    }

    /// The request id the job's one answer is filed under.
    fn browse_hubs_id(job: &StoreJob) -> u32 {
        let StoreJob::BrowseHubs(job, _) = job else { panic!("not a section-hubs job") };
        job.addr.req.0
    }

    /// The key the job's step decided on.
    fn browse_hubs_key(job: &StoreJob) -> ClientKey {
        let StoreJob::BrowseHubs(job, _) = job else { panic!("not a section-hubs job") };
        job.req.server().1
    }

    /// After the runner refused the read, the store's next step settles the section to the state of
    /// a read that never started and asks again with the registry's CURRENT key: the one new job,
    /// returned.
    fn the_next_step_asks_again(browse: &mut BrowseStore) -> StoreJob {
        assert!(!browse.hubs_read_out_for_test(), "the refusal gave the claim back in the call that refused");
        let cmd = browse_hubs_cmd(browse);
        browse.run(cmd, &registry_keys());
        let mut jobs = browse.take_jobs();
        assert_eq!(jobs.len(), 1, "the settled section asked again");
        jobs.remove(0)
    }

    /// The registry still holds the client the step decided on: the worker posts its ONE answer and
    /// the store's pump takes it, which lowers the claim the step raised. The launch raises nothing.
    #[test]
    fn a_browse_hubs_job_whose_client_is_still_held_runs_on_a_worker_and_answers_once() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, mut browse, job) = browse_hubs_request();
        assert!(!browse.hubs_answer_waiting_for_test(), "nothing has answered before the launch");

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert!(browse.hubs_answer_waiting_for_test(), "the worker answered");
        assert!(browse.hubs_read_out_for_test(), "the claim holds until the pump takes the answer");
        let _outcome = browse.pump_with_gate(plx_machine::landgate::fixture_gate(), &registry_keys(), &plx_plex::plex::session::peek());
        assert!(!browse.hubs_answer_waiting_for_test(), "the pump took the one answer");
        assert!(!browse.hubs_read_out_for_test(), "…which lowered the claim");
        // The same pump asks for the grid's first page (the Library section it landed shelves for),
        // which is the page job's: what a landed shelf read must not do is ask for shelves again.
        let asked = browse.take_jobs();
        assert!(asked.iter().all(|job| matches!(job, StoreJob::BrowsePage(..))),
            "and a read that landed asks for no more shelves: {asked:?}");
    }

    /// The registry moved on between the step and the launch: nothing starts and the one answer is
    /// the refusal, which lowers the claim. The next step asks again with the new key.
    #[test]
    fn a_browse_hubs_job_whose_client_is_gone_is_refused_once_and_the_next_step_asks_again() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, mut browse, job) = browse_hubs_request();
        let first = browse_hubs_key(&job);
        replace_the_client();

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert!(!browse.hubs_answer_waiting_for_test(), "no worker started, so no landing");
        let again = the_next_step_asks_again(&mut browse);
        assert_ne!(browse_hubs_key(&again), first, "the new read asks with the registry's current key");
    }

    /// The system refuses the thread: the same single refusal, the same settle.
    #[test]
    fn a_browse_hubs_job_the_system_refuses_to_start_is_refused_once_and_the_next_step_asks_again() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, mut browse, job) = browse_hubs_request();
        let first = browse_hubs_key(&job);

        with_refused_hubs_launches_for_test(|| StoreRunner::live().start(job));
        plx_base::task::drain_workers_for_test();

        assert!(!browse.hubs_answer_waiting_for_test());
        let again = the_next_step_asks_again(&mut browse);
        assert_eq!(browse_hubs_key(&again), first, "nothing moved: the same client is asked again");
    }

    /// `held_all` parks the job with the claim the step raised still up, and `run_held` runs it on
    /// the test thread through the production completion: one answer, nothing left parked.
    #[test]
    fn a_held_browse_hubs_job_holds_its_claim_and_runs_once_when_the_test_runs_it() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, mut browse, job) = browse_hubs_request();
        let mut runner = StoreRunner::held_all();

        runner.start(job);
        assert!(browse.hubs_read_out_for_test(), "a parked job is a read that has not answered");
        assert!(!browse.hubs_answer_waiting_for_test());

        assert_eq!(runner.run_held_of(plx_data::stores::StoreId::Browse), 1);
        assert!(browse.hubs_answer_waiting_for_test());
        let _outcome = browse.pump_with_gate(plx_machine::landgate::fixture_gate(), &registry_keys(), &plx_plex::plex::session::peek());
        assert!(!browse.hubs_read_out_for_test());
        assert_eq!(runner.run_held(), 0, "nothing left parked");
    }

    /// A parked job whose client is gone by the time the test runs it is refused, as the launch
    /// would have refused it; the next step asks again.
    #[test]
    fn a_held_browse_hubs_job_whose_client_is_gone_by_then_is_refused_and_the_next_step_asks_again() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, mut browse, job) = browse_hubs_request();
        let first = browse_hubs_key(&job);
        let mut runner = StoreRunner::held_all();

        runner.start(job);
        assert!(browse.hubs_read_out_for_test());
        replace_the_client();
        assert_eq!(runner.run_held(), 1);

        assert!(!browse.hubs_answer_waiting_for_test());
        let again = the_next_step_asks_again(&mut browse);
        assert_ne!(browse_hubs_key(&again), first);
    }

    /// `held` parks the section hubs' read, unlike Hubs' (which stays live): no rig of the suite
    /// waits for a section-hubs landing from a real worker, and a parked read is one the test
    /// settles when it chooses to (the `hostsim` rig that needs the request to reach the server runs
    /// it with `run_held_store_jobs_of_for_test`). The id on the parked job is the one the store
    /// addressed.
    #[test]
    fn the_held_runner_parks_a_browse_hubs_job() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, browse, job) = browse_hubs_request();
        let id = browse_hubs_id(&job);
        let mut runner = StoreRunner::held();

        runner.start(job);
        plx_base::task::drain_workers_for_test();
        assert!(!browse.hubs_answer_waiting_for_test(), "no worker answered: the job is parked");

        assert_eq!(runner.run_held_of(plx_data::stores::StoreId::Browse), 1, "parked under {id}");
        assert!(browse.hubs_answer_waiting_for_test());
    }

    /// A section-hubs read is `Admission::Direct`: handed a Home ledger it starts as `start` starts
    /// it and records nothing, and a ledger that has no record of it is not a failed run.
    #[test]
    fn a_browse_hubs_job_is_started_directly_and_leaves_the_home_ledger_untouched() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, browse, job) = browse_hubs_request();
        assert_eq!(job.admission(), Admission::Direct);
        let mut runner = StoreRunner::held_all();
        let mut io = home_io(true, Vec::new());

        runner.start_admitted(job, Some(&mut io));

        assert!(io.requests.is_empty() && io.admissions.is_empty() && io.failure.is_none(),
            "the ledger records Home jobs only");
        assert_eq!(runner.run_held(), 1, "the job was started as `start` starts it");
        assert!(browse.hubs_answer_waiting_for_test());
    }

    // ---- the Library's listing reads: a page or a directory list the Browse store's step claimed ----

    /// A server registered at a port nothing listens on and a Browse store whose current section is
    /// a Library section of it: its first step asks for the grid's first page.
    fn registered_browse() -> (ServerId, BrowseStore) {
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("runner", "127.0.0.1", 9, "t", "cid");
        let other = plx_plex::plex::register_for_test("runner-shared", "127.0.0.1", 10, "t", "cid");
        assert!(plx_plex::plex::set_current(sid));
        let mut browse = BrowseStore::default();
        browse.seed_registered_table_for_test([sid, other]);
        browse.run(BrowseCmd::SetCur(0), &registry_keys());
        (sid, browse)
    }

    /// The one job the store has left on its outbox, of the kind `wanted` names.
    #[track_caller]
    fn the_one_job(browse: &mut BrowseStore, wanted: fn(&StoreJob) -> bool, what: &str) -> StoreJob {
        let (mut jobs, rest): (Vec<_>, Vec<_>) = browse.take_jobs().into_iter().partition(|job| wanted(job));
        assert_eq!(jobs.len(), 1, "one {what} read: {jobs:?} (and {rest:?})");
        // a pump after the registry moved also steps the source discovery, which asks for its own
        // read: not this test's subject, so the runner refuses it
        for other in rest {
            let StoreJob::BrowseDiscovery(job, port) = other else { panic!("an unexpected job: {other:?}") };
            port.refused(job.addr);
        }
        jobs.remove(0)
    }

    fn is_page(job: &StoreJob) -> bool { matches!(job, StoreJob::BrowsePage(..)) }
    fn is_directory(job: &StoreJob) -> bool { matches!(job, StoreJob::BrowseDirectory(..)) }

    /// The store's step that asks for the grid's page (the pump).
    fn page_step(browse: &mut BrowseStore) {
        let _ = browse.pump_with_gate(plx_machine::landgate::fixture_gate(), &registry_keys(), &plx_plex::plex::session::peek());
    }

    /// The command that asks the current section for its genre list.
    fn browse_directory_cmd(browse: &mut BrowseStore) -> BrowseCmd {
        let id = browse.hubs_snapshot().view().id().expect("a section is current");
        let target = SectionAddress { epoch: id.epoch, sid: id.sid, section: id.section };
        BrowseCmd::Addressed { target, work: LibraryWork::Genres }
    }

    /// The store's step that asks for the genre list (the filter menu's tick).
    fn directory_step(browse: &mut BrowseStore) {
        let cmd = browse_directory_cmd(browse);
        browse.run(cmd, &registry_keys());
    }

    /// The page job the store's first pump left, keyed to the client the registry held when it did,
    /// with the page claim up from that step.
    fn browse_page_request() -> (ServerId, BrowseStore, StoreJob) {
        let (sid, mut browse) = registered_browse();
        page_step(&mut browse);
        let job = the_one_job(&mut browse, is_page, "page");
        assert!(browse.page_read_out_for_test(), "the step raised the page claim");
        (sid, browse, job)
    }

    /// The genre list's job, with its claim up.
    fn browse_directory_request() -> (ServerId, BrowseStore, StoreJob) {
        let (sid, mut browse) = registered_browse();
        directory_step(&mut browse);
        let job = the_one_job(&mut browse, is_directory, "directory");
        assert!(browse.directory_read_out_for_test(), "the step raised the directory claim");
        (sid, browse, job)
    }

    /// The key the job's step decided on.
    fn listing_key(job: &StoreJob) -> ClientKey {
        match job {
            StoreJob::BrowsePage(job, _) => job.req.server().1,
            StoreJob::BrowseDirectory(job, _) => job.req.server().1,
            other => panic!("not a listing job: {other:?}"),
        }
    }

    /// After the runner refused the page, the store's next step settles it and asks again with the
    /// registry's CURRENT key: the one new job, returned.
    fn the_next_page_step_asks_again(browse: &mut BrowseStore) -> StoreJob {
        assert!(!browse.page_read_out_for_test(), "the refusal gave the claim back in the call that refused");
        page_step(browse);
        the_one_job(browse, is_page, "page")
    }

    /// The same for the genre list: the menu's next tick asks again.
    fn the_next_directory_step_asks_again(browse: &mut BrowseStore) -> StoreJob {
        assert!(!browse.directory_read_out_for_test(), "the refusal gave the claim back in the call that refused");
        directory_step(browse);
        the_one_job(browse, is_directory, "directory")
    }

    /// The registry still holds the client the step decided on: the worker posts its ONE answer and
    /// the store's pump takes it, which lowers the claim the step raised. The launch raises nothing.
    #[test]
    fn a_browse_page_job_whose_client_is_still_held_runs_on_a_worker_and_answers_once() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, mut browse, job) = browse_page_request();
        assert!(!browse.page_answer_waiting_for_test(), "nothing has answered before the launch");

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert!(browse.page_answer_waiting_for_test(), "the worker answered");
        assert!(browse.page_read_out_for_test(), "the claim holds until the pump takes the answer");
        page_step(&mut browse);
        assert!(!browse.page_answer_waiting_for_test(), "the pump took the one answer");
        assert!(!browse.page_read_out_for_test(), "…which lowered the claim");
        assert!(!browse.has_jobs(), "and a page that landed (failed, here) asks for nothing at once");
    }

    /// The registry moved on between the step and the launch: nothing starts and the one answer is
    /// the refusal, which lowers the claim. The next step asks again with the new key.
    #[test]
    fn a_browse_page_job_whose_client_is_gone_is_refused_once_and_the_next_step_asks_again() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, mut browse, job) = browse_page_request();
        let first = listing_key(&job);
        move_the_key(sid);

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert!(!browse.page_answer_waiting_for_test(), "no worker started, so no landing");
        let again = the_next_page_step_asks_again(&mut browse);
        assert_ne!(listing_key(&again), first, "the new read asks with the registry's current key");
    }

    /// The system refuses the thread: the same single refusal, the same settle.
    #[test]
    fn a_browse_page_job_the_system_refuses_to_start_is_refused_once_and_the_next_step_asks_again() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, mut browse, job) = browse_page_request();
        let first = listing_key(&job);

        with_refused_hubs_launches_for_test(|| StoreRunner::live().start(job));
        plx_base::task::drain_workers_for_test();

        assert!(!browse.page_answer_waiting_for_test());
        let again = the_next_page_step_asks_again(&mut browse);
        assert_eq!(listing_key(&again), first, "nothing moved: the same client is asked again");
    }

    /// `held_all` parks the job with the claim the step raised still up, and `run_held` runs it on
    /// the test thread through the production completion: one answer, nothing left parked.
    #[test]
    fn a_held_browse_page_job_holds_its_claim_and_runs_once_when_the_test_runs_it() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, mut browse, job) = browse_page_request();
        let mut runner = StoreRunner::held_all();

        runner.start(job);
        assert!(browse.page_read_out_for_test(), "a parked job is a read that has not answered");
        assert!(!browse.page_answer_waiting_for_test());

        assert_eq!(runner.run_held_of(plx_data::stores::StoreId::Browse), 1);
        assert!(browse.page_answer_waiting_for_test());
        page_step(&mut browse);
        assert!(!browse.page_read_out_for_test());
        assert_eq!(runner.run_held(), 0, "nothing left parked");
    }

    /// A parked job whose client is gone by the time the test runs it is refused, as the launch
    /// would have refused it; the next step asks again.
    #[test]
    fn a_held_browse_page_job_whose_client_is_gone_by_then_is_refused_and_the_next_step_asks_again() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, mut browse, job) = browse_page_request();
        let first = listing_key(&job);
        let mut runner = StoreRunner::held_all();

        runner.start(job);
        assert!(browse.page_read_out_for_test());
        move_the_key(sid);
        assert_eq!(runner.run_held(), 1);

        assert!(!browse.page_answer_waiting_for_test());
        let again = the_next_page_step_asks_again(&mut browse);
        assert_ne!(listing_key(&again), first);
    }

    /// `held` starts the grid's page read on a worker, as production does: the library rigs wait
    /// for a grid to settle (the route rigs for a failed page, `session_dispatch_tests` for a request
    /// that reaches the loopback server) and nothing is parked for them. `park_page_reads` parks it
    /// for the rig that grades which reads an entry started.
    #[test]
    fn the_held_runner_starts_a_browse_page_job_on_a_worker_unless_told_to_park_it() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, browse, job) = browse_page_request();
        let mut runner = StoreRunner::held();

        runner.start(job);
        plx_base::task::drain_workers_for_test();
        assert!(browse.page_answer_waiting_for_test(), "a worker answered: nothing is parked");
        assert_eq!(runner.run_held(), 0);

        let (_sid, browse, job) = browse_page_request();
        runner.park_page_reads();
        runner.start(job);
        plx_base::task::drain_workers_for_test();
        assert!(!browse.page_answer_waiting_for_test(), "no worker answered: the job is parked");
        assert_eq!(runner.run_held_of(plx_data::stores::StoreId::Browse), 1);
        assert!(browse.page_answer_waiting_for_test());
    }

    /// The directory's lane, same five branches: a worker that answers once, a client gone, a thread
    /// refused, a parked job and a parked job whose client is gone by the time it runs.
    #[test]
    fn a_browse_directory_job_whose_client_is_still_held_runs_on_a_worker_and_answers_once() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, mut browse, job) = browse_directory_request();
        assert!(!browse.directory_answer_waiting_for_test(), "nothing has answered before the launch");

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert!(browse.directory_answer_waiting_for_test(), "the worker answered");
        assert!(browse.directory_read_out_for_test(), "the claim holds until the pump takes the answer");
        page_step(&mut browse);
        assert!(!browse.directory_answer_waiting_for_test(), "the pump took the one answer");
        assert!(!browse.directory_read_out_for_test(), "…which lowered the claim");
    }

    #[test]
    fn a_browse_directory_job_whose_client_is_gone_is_refused_once_and_the_next_step_asks_again() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, mut browse, job) = browse_directory_request();
        let first = listing_key(&job);
        move_the_key(sid);

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert!(!browse.directory_answer_waiting_for_test(), "no worker started, so no landing");
        let again = the_next_directory_step_asks_again(&mut browse);
        assert_ne!(listing_key(&again), first, "the new read asks with the registry's current key");
    }

    #[test]
    fn a_browse_directory_job_the_system_refuses_to_start_is_refused_once_and_the_next_step_asks_again() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, mut browse, job) = browse_directory_request();
        let first = listing_key(&job);

        with_refused_hubs_launches_for_test(|| StoreRunner::live().start(job));
        plx_base::task::drain_workers_for_test();

        assert!(!browse.directory_answer_waiting_for_test());
        let again = the_next_directory_step_asks_again(&mut browse);
        assert_eq!(listing_key(&again), first, "nothing moved: the same client is asked again");
    }

    #[test]
    fn a_held_browse_directory_job_holds_its_claim_and_runs_once_when_the_test_runs_it() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, mut browse, job) = browse_directory_request();
        let mut runner = StoreRunner::held_all();

        runner.start(job);
        assert!(browse.directory_read_out_for_test(), "a parked job is a read that has not answered");
        assert!(!browse.directory_answer_waiting_for_test());

        assert_eq!(runner.run_held_of(plx_data::stores::StoreId::Browse), 1);
        assert!(browse.directory_answer_waiting_for_test());
        page_step(&mut browse);
        assert!(!browse.directory_read_out_for_test());
        assert_eq!(runner.run_held(), 0, "nothing left parked");
    }

    #[test]
    fn a_held_browse_directory_job_whose_client_is_gone_by_then_is_refused_and_the_next_step_asks_again() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, mut browse, job) = browse_directory_request();
        let first = listing_key(&job);
        let mut runner = StoreRunner::held_all();

        runner.start(job);
        assert!(browse.directory_read_out_for_test());
        move_the_key(sid);
        assert_eq!(runner.run_held(), 1);

        assert!(!browse.directory_answer_waiting_for_test());
        let again = the_next_directory_step_asks_again(&mut browse);
        assert_ne!(listing_key(&again), first);
    }

    /// `held` parks the filter menus' value lists: no rig of the suite waits for a landing of one
    /// from a real worker (the suites ran green with it parked; the rigs that want one run it with
    /// `run_held_store_jobs_of_for_test`).
    #[test]
    fn the_held_runner_parks_a_browse_directory_job() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, browse, job) = browse_directory_request();
        let mut runner = StoreRunner::held();

        runner.start(job);
        plx_base::task::drain_workers_for_test();
        assert!(!browse.directory_answer_waiting_for_test(), "no worker answered: the job is parked");

        assert_eq!(runner.run_held_of(plx_data::stores::StoreId::Browse), 1);
        assert!(browse.directory_answer_waiting_for_test());
    }

    /// Both listing reads are `Admission::Direct`: handed a Home ledger they start as `start` starts
    /// them and record nothing, and a ledger that has no record of them is not a failed run.
    #[test]
    fn the_listing_jobs_are_started_directly_and_leave_the_home_ledger_untouched() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        // Each request builds its own registry, so one is built per turn: both up front would leave
        // the first job's client gone by the time the runner resolves it.
        let requests: [fn() -> (ServerId, BrowseStore, StoreJob); 2] = [browse_page_request, browse_directory_request];
        for request in requests {
            let (_sid, browse, job) = request();
            assert_eq!(job.admission(), Admission::Direct);
            let mut runner = StoreRunner::held_all();
            let mut io = home_io(true, Vec::new());

            runner.start_admitted(job, Some(&mut io));

            assert!(io.requests.is_empty() && io.admissions.is_empty() && io.failure.is_none(),
                "the ledger records Home jobs only");
            assert_eq!(runner.run_held(), 1, "the job was started as `start` starts it");
            assert!(browse.page_answer_waiting_for_test() || browse.directory_answer_waiting_for_test());
        }
    }

    /// A loopback PMS that ANSWERS the three reads a source's discovery makes: the section list (one
    /// Movies library), the server's own name, and the item count of a section. A worker handed the
    /// client the runner resolved lands a section; one handed none (or a dead port) lands a failure,
    /// so only the landing tells the two apart.
    fn discovery_server() -> Pms {
        Pms::serve(|path| match path {
            "/library/sections" => ("200 OK",
                r#"{"MediaContainer":{"size":1,"Directory":[{"key":"1","type":"movie","title":"Movies"}]}}"#.into()),
            "/" => ("200 OK", r#"{"MediaContainer":{"friendlyName":"Loopback PMS"}}"#.into()),
            _ => ("200 OK", r#"{"MediaContainer":{"size":0,"totalSize":3}}"#.into()),
        })
    }

    fn is_discovery(job: &StoreJob) -> bool { matches!(job, StoreJob::BrowseDiscovery(..)) }

    /// A server registered at `port` and a Browse store whose first discovery pass has asked for its
    /// section list: the job the step left, keyed to the client the registry held when it did, with
    /// the discovery claim up from that step.
    fn discovery_request_at(port: i32) -> (ServerId, BrowseStore, StoreJob) {
        assert!(plx_net::net::global_init() && plx_net::net::available() && plx_net::net::threaded_tls_ready());
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("runner", "127.0.0.1", port, "t", "cid");
        assert!(plx_plex::plex::set_current(sid));
        plx_plex::plex::client_for(sid).unwrap().set_link(plx_plex::plex::probe::Location::Local);
        let mut browse = BrowseStore::default();
        discovery_step(&mut browse);
        let job = the_one_job(&mut browse, is_discovery, "discovery");
        assert!(browse.discovery_owed(), "the step raised the discovery claim");
        (sid, browse, job)
    }

    /// The store's discovery pass, with the registry's keys as of now.
    fn discovery_step(browse: &mut BrowseStore) {
        let _ = browse.discover_pump_with_gate(plx_machine::landgate::fixture_gate(), &registry_keys(), &plx_plex::plex::session::peek());
    }

    /// The `"sources"` worker is not counted by `drain_workers_for_test` (it dials whatever a roster
    /// names), so a test waits for its answer by polling the mailbox, a bounded count of passes.
    #[track_caller]
    fn wait_for_the_discovery_answer(browse: &BrowseStore) {
        for _ in 0..5000 {
            if browse.discovery_answer_waiting_for_test() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the discovery worker never answered");
    }

    /// The pass after an answer: the landing is applied (the section is in the table, the claim
    /// came down in `apply_discovery`) and, the same step, the source's next question (the item
    /// counts of what landed) is asked: ONE new discovery job, with the claim up for it.
    /// (`sections_done` is the first source's flag, from `discovery_policy_for_test`.)
    #[track_caller]
    fn assert_the_section_landed(browse: &mut BrowseStore) {
        discovery_step(browse);
        assert_eq!(browse.section_count_for_test(), 1, "the loopback's one library landed");
        let (_claimed, _retry, sections_done) = browse.discovery_policy_for_test();
        assert!(sections_done, "the source's section list is done");
        let next = browse.take_jobs();
        assert_eq!(next.iter().filter(|job| is_discovery(job)).count(), 1,
            "the landing was applied (it lowered the claim) and the counts are the next ask");
        assert!(browse.discovery_owed(), "the claim now belongs to the counts read");
    }

    /// The pass after a refusal: nothing is asked for yet (the source backs off), and the claim is
    /// down. `retry_cd` is one short of the full 600-step backoff: the refusal is settled first, then
    /// the countdown runs.
    #[track_caller]
    fn assert_settled_into_the_backoff(browse: &mut BrowseStore) {
        assert!(!browse.discovery_owed(), "the refusal gave the claim back in the call that refused");
        discovery_step(browse);
        assert!(browse.take_jobs().is_empty(), "the refused source is in its backoff: nothing is asked");
        let (claimed, retry, sections_done) = browse.discovery_policy_for_test();
        assert!(!claimed);
        assert_eq!(retry, 599, "settled to the full backoff, then one step of the countdown");
        assert!(!sections_done, "nothing was learned of the server");
    }

    /// The key a discovery job decided on.
    fn discovery_key(job: &StoreJob) -> plx_plex::plex::ClientKey {
        let StoreJob::BrowseDiscovery(inner, _) = job else { panic!("not a discovery job: {job:?}") };
        inner.req.server().1
    }

    /// The pass after a refusal whose client was ALSO re-pointed (its token moved, which is what
    /// "gone" means to the registry): the claim is down, and the step's roster sync re-arms the
    /// source, so it is asked again at once, with the new key, not after the backoff.
    #[track_caller]
    fn assert_refused_and_asked_again_with_the_new_key(browse: &mut BrowseStore, first: plx_plex::plex::ClientKey) {
        assert!(!browse.discovery_owed(), "the refusal gave the claim back in the call that refused");
        discovery_step(browse);
        let again = the_one_job(browse, is_discovery, "discovery");
        assert_ne!(discovery_key(&again), first, "asked again with the client the registry holds now");
    }

    /// The registry still holds the client the step decided on: the worker reads from the server
    /// through it and posts its ONE answer, and the pump that applies it lands the section and lowers
    /// the claim. The step raised the claim; the launch raises nothing.
    #[test]
    fn a_browse_discovery_job_whose_client_is_held_runs_on_a_worker_and_answers_once() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let pms = discovery_server();
        let (_sid, mut browse, job) = discovery_request_at(pms.port);
        assert_eq!(job.admission(), Admission::Home);
        assert!(!browse.discovery_answer_waiting_for_test(), "nothing has answered before the launch");

        StoreRunner::live().start(job);
        wait_for_the_discovery_answer(&browse);

        assert!(browse.discovery_owed(), "the claim holds until the answer is applied");
        assert_the_section_landed(&mut browse);
    }

    /// The registry moved on between the step and the launch: nothing starts, the one answer is the
    /// refusal (the claim is down in that call), and the next step's roster sync, which sees the
    /// re-pointed client, asks again at once (the backoff of a refusal the sync did not re-arm is
    /// pinned by the refused-thread test below).
    #[test]
    fn a_browse_discovery_job_whose_client_is_gone_is_refused_once_and_asked_again_with_the_new_key() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let pms = discovery_server();
        let (sid, mut browse, job) = discovery_request_at(pms.port);
        let first = discovery_key(&job);
        move_the_key(sid);

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert!(!browse.discovery_answer_waiting_for_test(), "no worker started, so no landing");
        assert_refused_and_asked_again_with_the_new_key(&mut browse, first);
    }

    /// The system refuses the thread: the same single refusal, the same settle.
    #[test]
    fn a_browse_discovery_job_the_system_refuses_to_start_is_refused_once_and_backs_off_before_it_asks_again() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let pms = discovery_server();
        let (_sid, mut browse, job) = discovery_request_at(pms.port);

        with_refused_hubs_launches_for_test(|| StoreRunner::live().start(job));
        plx_base::task::drain_workers_for_test();

        assert!(!browse.discovery_answer_waiting_for_test());
        assert_settled_into_the_backoff(&mut browse);
    }

    /// `held_all` parks the job with the claim the step raised still up, and `run_held` runs it on
    /// the test thread through the client it resolves then: one answer, nothing left parked, and the
    /// section lands.
    #[test]
    fn a_held_browse_discovery_job_holds_its_claim_and_runs_once_when_the_test_runs_it() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let pms = discovery_server();
        let (_sid, mut browse, job) = discovery_request_at(pms.port);
        let mut runner = StoreRunner::held_all();

        runner.start(job);
        assert!(browse.discovery_owed(), "a parked job is a read that has not answered");
        assert!(!browse.discovery_answer_waiting_for_test());

        assert_eq!(runner.run_held_of(plx_data::stores::StoreId::Browse), 1);
        assert!(browse.discovery_answer_waiting_for_test());
        assert_the_section_landed(&mut browse);
        assert_eq!(runner.run_held(), 0, "nothing left parked");
    }

    /// A parked job whose client is gone by the time the test runs it is refused, as the launch would
    /// have refused it; the next step, which sees the re-pointed client, asks again at once.
    #[test]
    fn a_held_browse_discovery_job_whose_client_is_gone_by_then_is_refused_and_asked_again() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let pms = discovery_server();
        let (sid, mut browse, job) = discovery_request_at(pms.port);
        let first = discovery_key(&job);
        let mut runner = StoreRunner::held_all();

        runner.start(job);
        assert!(browse.discovery_owed());
        move_the_key(sid);
        assert_eq!(runner.run_held(), 1);

        assert!(!browse.discovery_answer_waiting_for_test());
        assert_refused_and_asked_again_with_the_new_key(&mut browse, first);
    }

    /// `held` starts a source discovery on a worker, as production does (nothing is parked: no rig
    /// of the suite waits for a landing from it, and a parked one would run its blocking reads on
    /// the test thread wherever a rig runs every parked job); `held_all` parks it.
    #[test]
    fn the_held_runner_starts_a_browse_discovery_job_on_a_worker_and_held_all_parks_it() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let pms = discovery_server();
        let (_sid, browse, job) = discovery_request_at(pms.port);
        let mut runner = StoreRunner::held();

        runner.start(job);
        wait_for_the_discovery_answer(&browse);
        assert_eq!(runner.run_held(), 0, "a worker answered: nothing is parked");

        let (_sid, browse, job) = discovery_request_at(pms.port);
        let mut runner = StoreRunner::held_all();
        runner.start(job);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(!browse.discovery_answer_waiting_for_test(), "no worker answered: the job is parked");
        assert_eq!(runner.run_held_of(plx_data::stores::StoreId::Browse), 1);
        assert!(browse.discovery_answer_waiting_for_test());
    }

    /// A controlled run recording: the ledger holds exactly the job's own descriptor with `admitted`
    /// added, and a worker answers.
    #[test]
    fn a_browse_discovery_job_is_recorded_with_its_own_descriptor_and_started() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let pms = discovery_server();
        let (_sid, mut browse, job) = discovery_request_at(pms.port);
        let StoreJob::BrowseDiscovery(inner, _) = &job else { unreachable!() };
        let mut record = inner.req.admission();
        record["admitted"] = serde_json::json!(true);
        let mut io = home_io(false, Vec::new());

        StoreRunner::live().start_admitted(job, Some(&mut io));
        wait_for_the_discovery_answer(&browse);

        assert_eq!(io.requests, [record], "the descriptor and `admitted`, and nothing else");
        assert!(io.failure.is_none());
        assert_the_section_landed(&mut browse);
    }

    /// A controlled run replaying: a recorded launch (`admitted: true`) starts nothing and answers
    /// nothing (the recorded result answers it, and the claim stays up until it is applied); a
    /// recorded refusal is answered here, once, and the next step settles it.
    #[test]
    fn a_replayed_browse_discovery_job_launches_nothing_and_a_recorded_refusal_is_answered_once() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let pms = discovery_server();
        for admitted in [true, false] {
            let (_sid, mut browse, job) = discovery_request_at(pms.port);
            let StoreJob::BrowseDiscovery(inner, _) = &job else { unreachable!() };
            let mut record = inner.req.admission();
            record["admitted"] = serde_json::json!(admitted);
            let mut io = home_io(true, vec![record.clone()]);
            let mut runner = StoreRunner::live();

            runner.start_admitted(job, Some(&mut io));
            std::thread::sleep(std::time::Duration::from_millis(20));

            assert_eq!(io.requests, [record], "the replay records what it was told");
            assert!(io.admissions.is_empty() && io.failure.is_none());
            assert!(!browse.discovery_answer_waiting_for_test(), "a replay starts no worker (admitted: {admitted})");
            if admitted {
                assert!(browse.discovery_owed(), "the recorded launch's claim stays up until its result applies");
            } else {
                assert_settled_into_the_backoff(&mut browse);
            }
        }
    }

    /// A controlled run recording: the launch is the recorded boolean. The ledger holds the job's
    /// own descriptor, and a worker answers.
    #[test]
    fn a_home_job_recorded_as_launched_starts_a_worker_and_is_recorded_admitted() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, hubs, job) = hubs_request();
        let record = hubs_record(&job, true);
        let mut io = home_io(false, Vec::new());

        StoreRunner::live().start_admitted(job, Some(&mut io));
        plx_base::task::drain_workers_for_test();

        assert_eq!(io.requests, [record]);
        assert!(io.failure.is_none());
        assert_eq!(hubs.take_results().len(), 1);
        assert!(hubs.refused_for_test().is_empty());
    }

    /// A controlled run recording a launch the system refused: `admitted:false` is what the ledger
    /// holds, the claim is back, and the one answer is the refusal.
    #[test]
    fn a_home_job_the_system_refuses_is_recorded_not_admitted_and_refused_once() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, hubs, job) = hubs_request();
        let (id, record) = (hubs_id(&job), hubs_record(&job, false));
        let mut io = home_io(false, Vec::new());

        with_refused_hubs_launches_for_test(|| StoreRunner::live().start_admitted(job, Some(&mut io)));

        assert_eq!(io.requests, [record]);
        assert_eq!(hubs.refused_for_test(), [id]);
        assert!(!hubs.owed());
    }

    /// A controlled run recording a job whose client is already gone: the same refusal, recorded.
    #[test]
    fn a_home_job_whose_client_is_gone_is_recorded_not_admitted_and_refused_once() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, hubs, job) = hubs_request();
        let (id, record) = (hubs_id(&job), hubs_record(&job, false));
        replace_the_client();
        let mut io = home_io(false, Vec::new());

        StoreRunner::live().start_admitted(job, Some(&mut io));

        assert_eq!(io.requests, [record]);
        assert_eq!(hubs.refused_for_test(), [id]);
        assert!(!hubs.owed());
    }

    /// Replay with `"admitted":true`: the recorded run launched a worker, and replay never does, so
    /// the runner answers nothing and raises no claim: the recording's own landing settles it.
    #[test]
    fn a_home_job_the_ledger_admits_launches_nothing_under_replay_and_answers_nothing() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, hubs, job) = hubs_request();
        let record = hubs_record(&job, true);
        let mut io = home_io(true, vec![record.clone()]);

        StoreRunner::live().start_admitted(job, Some(&mut io));
        // Had a worker been started it would have answered by the time this returns.
        plx_base::task::drain_workers_for_test();

        assert_eq!(io.requests, [record], "the replay re-records what it was told");
        assert!(io.admissions.is_empty() && io.failure.is_none(), "the one recorded line was consumed clean");
        assert!(hubs.take_results().is_empty(), "no worker ran");
        assert!(hubs.refused_for_test().is_empty(), "and no refusal was invented");
        assert!(!hubs.owed(), "replay never raises the claim a worker would have");
    }

    /// Replay with `"admitted":false`: the recorded launch was refused, so nothing starts and the
    /// runner settles the job with exactly one refusal.
    #[test]
    fn a_home_job_the_ledger_refuses_is_refused_once_under_replay_and_starts_nothing() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, hubs, job) = hubs_request();
        let (id, record) = (hubs_id(&job), hubs_record(&job, false));
        let mut io = home_io(true, vec![record.clone()]);

        StoreRunner::live().start_admitted(job, Some(&mut io));
        plx_base::task::drain_workers_for_test();

        assert_eq!(io.requests, [record]);
        assert!(io.failure.is_none());
        assert_eq!(hubs.refused_for_test(), [id], "one refusal, not two");
        assert!(hubs.take_results().is_empty());
        assert!(!hubs.owed());
    }

    /// Replay against a ledger line that is not this request: the run has failed (`failure`), the
    /// runner does not launch, and the job is still answered exactly once.
    #[test]
    fn a_home_job_the_ledger_does_not_recognise_fails_the_run_and_is_refused_once() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, hubs, job) = hubs_request();
        let id = hubs_id(&job);
        let mut other = hubs_record(&job, true);
        other["req"] = serde_json::json!(id + 1000);
        let mut io = home_io(true, vec![other]);

        StoreRunner::live().start_admitted(job, Some(&mut io));
        plx_base::task::drain_workers_for_test();

        assert_eq!(io.failure, Some("mismatched synchronous admission"));
        assert_eq!(hubs.refused_for_test(), [id]);
        assert!(hubs.take_results().is_empty());
        assert!(!hubs.owed());
    }

    /// A Home job from a step run outside a controlled run has no ledger: it starts directly and
    /// nothing is recorded.
    #[test]
    fn a_home_job_with_no_ledger_starts_directly() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_sid, hubs, job) = hubs_request();

        StoreRunner::live().start_admitted(job, None);
        plx_base::task::drain_workers_for_test();

        assert_eq!(hubs.take_results().len(), 1);
        assert!(hubs.refused_for_test().is_empty());
    }

    /// A job of another family is started as `start` starts it, whether or not a ledger is handed
    /// over: the ledger records Home jobs only.
    #[test]
    fn a_job_of_another_admission_family_leaves_the_home_ledger_untouched() {
        let _serial = plx_base::testlock::serial();
        let mut store = MetadataStore::default();
        let job = request(&mut store, ServerId::UNSET, "d1");
        assert_eq!(job.admission(), Admission::Tape);
        let mut runner = StoreRunner::held();
        let mut io = home_io(false, Vec::new());

        runner.start_admitted(job, Some(&mut io));

        assert!(io.requests.is_empty(), "a Tape job records on the tape, not in the Home ledger");
        assert_eq!(runner.run_held(), 1, "and it was started as `start` starts it");
    }

    // ---- the metadata read lane: a season switch and an episode-page window ----

    /// A server registered at a port nothing listens on (a read that starts fails at once) and a
    /// metadata store holding a two-season show of it: season 1 listed, 200 episodes of which page 0
    /// is loaded, so the window below page 0 is a hole.
    fn show_store() -> (ServerId, MetadataStore) {
        show_store_at(9)
    }

    /// [`show_store`] with the server registered at `port`. A test that claims the worker was
    /// handed the client the runner resolved dials a loopback that answers (`episodes_server`): a
    /// read that reaches it lands, and a worker handed no client answers the same failure a dead
    /// port does, so only a landing tells the two apart.
    fn show_store_at(port: i32) -> (ServerId, MetadataStore) {
        assert!(plx_net::net::global_init() && plx_net::net::available() && plx_net::net::threaded_tls_ready());
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("runner", "127.0.0.1", port, "t", "cid");
        plx_plex::plex::client_for(sid).unwrap().set_link(plx_plex::plex::probe::Location::Local);
        let mut store = MetadataStore::default();
        show_on(&mut store, sid);
        (sid, store)
    }

    /// A loopback PMS that answers every read with a two-episode `/children` list.
    fn episodes_server() -> Pms {
        Pms::serve(|_| {
            ("200 OK", r#"{"MediaContainer":{"size":2,"Metadata":[
                {"ratingKey":"ep-first","type":"episode","title":"First"},
                {"ratingKey":"ep-second","type":"episode","title":"Second"}]}}"#.into())
        })
    }

    /// The episode keys the show page lists, from the first page the list holds.
    fn listed(store: &MetadataStore) -> Vec<String> {
        store.view().current().map(|d| d.episodes.iter_loaded().map(|(_, e)| e.rk.clone()).collect()).unwrap_or_default()
    }

    /// The keys of the episodes the list holds from `EPISODE_PAGE` on: the page the window asked for.
    fn second_page(store: &MetadataStore) -> Vec<String> {
        use plx_data::metadata::EPISODE_PAGE;
        store.view().current().map(|d| {
            d.episodes.iter_loaded().filter(|(i, _)| *i >= EPISODE_PAGE).map(|(_, e)| e.rk.clone()).collect()
        }).unwrap_or_default()
    }

    fn show_on(store: &mut MetadataStore, sid: ServerId) {
        use plx_data::metadata::{Detail, Episode, EpisodeList, Season, EPISODE_PAGE};
        let season = |index: i64| Season {
            rk: format!("sk{index}"), index, title: format!("Season {index}"), leaf_count: 0, viewed_leaf_count: 0,
        };
        let mut episodes = EpisodeList::with_total(200);
        episodes.set_page(0, (0..EPISODE_PAGE).map(|i| Episode { rk: format!("e{i}"), ..Default::default() }).collect());
        plx_data::metadata::set_current_for_test(store.state_mut(), Some(Detail {
            sid, rk: "show".into(), is_show: true, seasons: vec![season(1), season(2)], cur_season: 0,
            episodes, ..Default::default()
        }));
    }

    /// The switch to season 2, as the season tabs ask for it: the job its step emitted.
    fn season_request() -> (ServerId, MetadataStore, StoreJob) {
        season_request_at(9)
    }

    fn season_request_at(port: i32) -> (ServerId, MetadataStore, StoreJob) {
        let (sid, mut store) = show_store_at(port);
        assert!(store.run(MetadataCmd::LoadSeason(1)));
        let mut jobs = store.take_jobs();
        assert_eq!(jobs.len(), 1, "one switch, one request");
        (sid, store, jobs.remove(0))
    }

    /// A page past the loaded one: the window the Detail screen asks for as the list scrolls.
    fn pages_want(store: &mut MetadataStore) -> bool {
        use plx_data::metadata::EPISODE_PAGE;
        store.run(MetadataCmd::WantEpisodes { lo: EPISODE_PAGE, hi: EPISODE_PAGE + 10, focus: None, restore: None })
    }

    fn pages_request() -> (ServerId, MetadataStore, StoreJob) {
        pages_request_at(9)
    }

    fn pages_request_at(port: i32) -> (ServerId, MetadataStore, StoreJob) {
        let (sid, mut store) = show_store_at(port);
        assert!(pages_want(&mut store));
        let mut jobs = store.take_jobs();
        assert_eq!(jobs.len(), 1, "one window, one request");
        (sid, store, jobs.remove(0))
    }

    fn season_pump(store: &mut MetadataStore) -> bool {
        store.pump_season_with_gate(plx_machine::landgate::fixture_gate())
    }

    /// The season tab and the loading flag, as the Detail screen reads them.
    fn season_state(store: &MetadataStore) -> (usize, bool) {
        let view = store.view();
        (view.current().map(|d| d.cur_season).expect("a show is loaded"), view.season_loading())
    }

    /// The page job still holds its page when the same window is asked again (so no second job),
    /// and has given it back once the window yields a new job.
    fn pages_held(store: &mut MetadataStore) -> bool {
        let asked = pages_want(store);
        let _ = store.take_jobs();
        !asked
    }

    /// The registry still holds the client: the worker starts, reads from the server the launch
    /// resolved a client for, and posts its ONE answer. The server answers, so a landing (the tab
    /// stays on the new season, its episodes listed) is what a worker handed the client does; one
    /// handed none puts the tab back. The kind is not on the tape, so nothing is recorded.
    #[test]
    fn a_season_job_whose_client_is_held_is_started_answered_once_and_not_taped() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let pms = episodes_server();
        let (_, mut store, job) = season_request_at(pms.port);
        assert_eq!(job.admission(), Admission::Direct);
        assert_eq!(season_state(&store), (1, true), "the tab flipped and the request is out");
        tape::init(None, false);

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![], None), "the kind is untaped: the runner records no admission");
        tape::reset_for_test();
        assert!(season_pump(&mut store), "the read reached the server and its answer landed");
        assert_eq!(season_state(&store), (1, false), "the request settled with the tab on the new season");
        assert_eq!(listed(&store), ["ep-first", "ep-second"]);
    }

    /// Replay launches the same worker the recording did: there is no admission record to consult.
    #[test]
    fn a_season_job_launches_live_under_replay_as_the_inline_spawn_did() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, mut store, job) = season_request();
        tape::init(None, true);
        tape::begin(Default::default(), Default::default());

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![], None));
        tape::reset_for_test();
        season_pump(&mut store);
        assert_eq!(season_state(&store), (0, false), "a worker started and answered");
    }

    /// The registry holds no client for the show's server by the launch: the worker still starts
    /// and answers the failure without a GET, as the inline fetch did when its read found no client.
    #[test]
    fn a_season_job_whose_server_has_no_client_answers_the_failure() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, mut store, job) = season_request();
        plx_plex::plex::reset_servers_for_test();

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        season_pump(&mut store);
        assert_eq!(season_state(&store), (0, false), "answered once, as a failure");
    }

    /// `held()` runs the season switch on a worker as production does (the menu-play wait polls for
    /// its landing and never runs held jobs), with the client it resolved; `held_all()` parks it
    /// until the test runs it.
    #[test]
    fn the_held_runner_starts_a_season_job_on_a_worker_as_production_does() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let pms = episodes_server();
        let (_, mut store, job) = season_request_at(pms.port);
        let mut runner = StoreRunner::held();

        runner.start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(runner.run_held(), 0, "nothing was parked: the job started");
        assert!(season_pump(&mut store), "the worker read from the server and its answer landed");
        assert_eq!(season_state(&store), (1, false), "the request settled with the tab on the new season");
    }

    /// The parked job resolves its client when the test runs it, and reads through it.
    #[test]
    fn a_held_season_job_runs_once_when_the_test_runs_it() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let pms = episodes_server();
        let (_, mut store, job) = season_request_at(pms.port);
        let mut runner = StoreRunner::held_all();

        runner.start(job);
        plx_base::task::drain_workers_for_test();
        assert_eq!(season_state(&store), (1, true), "parked: still out");
        assert_eq!(runner.run_held(), 1);

        assert!(season_pump(&mut store), "the run read from the server and its answer landed");
        assert_eq!(season_state(&store), (1, false), "the run answered once");
        assert_eq!(listed(&store), ["ep-first", "ep-second"]);
        assert_eq!(runner.run_held(), 0, "and nothing is left to run twice");
    }

    /// The registry holds the client: the window's worker reads the page from the server the launch
    /// resolved a client for and posts its ONE answer. The server answers, so the page is seated
    /// when the pump takes it; a worker handed no client lands the page as a failure instead. Not
    /// taped.
    #[test]
    fn a_page_job_whose_client_is_held_is_started_answered_once_and_not_taped() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let pms = episodes_server();
        let (_, mut store, job) = pages_request_at(pms.port);
        assert_eq!(job.admission(), Admission::Direct);
        assert!(pages_held(&mut store), "the page is out: asking again starts nothing");
        tape::init(None, false);

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![], None), "the kind is untaped: the runner records no admission");
        tape::reset_for_test();
        store.pump(plx_machine::landgate::fixture_gate());
        assert_eq!(second_page(&store), ["ep-first", "ep-second"], "the read reached the server and its page is seated");
    }

    #[test]
    fn a_page_job_launches_live_under_replay_as_the_inline_spawn_did() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, mut store, job) = pages_request();
        tape::init(None, true);
        tape::begin(Default::default(), Default::default());

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![], None));
        tape::reset_for_test();
        store.pump(plx_machine::landgate::fixture_gate());
        assert!(!pages_held(&mut store), "a worker started and answered");
    }

    /// No client by the launch: the job still answers, each page as a failure, so no page stays
    /// owed.
    #[test]
    fn a_page_job_whose_server_has_no_client_answers_the_failure() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, mut store, job) = pages_request();
        plx_plex::plex::reset_servers_for_test();

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        store.pump(plx_machine::landgate::fixture_gate());
        assert!(!pages_held(&mut store), "answered: the page came back");
    }

    /// `held()` parks the page jobs (no rig waits for a page landing); the test runs them, and a
    /// parked job resolves its client when it runs and reads through it.
    #[test]
    fn a_held_page_job_runs_once_when_the_test_runs_it() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let pms = episodes_server();
        let (_, mut store, job) = pages_request_at(pms.port);
        let mut runner = StoreRunner::held();

        runner.start(job);
        plx_base::task::drain_workers_for_test();
        assert!(pages_held(&mut store), "parked, still out");
        assert_eq!(runner.run_held(), 1);

        store.pump(plx_machine::landgate::fixture_gate());
        assert_eq!(second_page(&store), ["ep-first", "ep-second"], "the run read from the server and its page is seated");
        assert_eq!(runner.run_held(), 0);
    }

    /// A newer DETAIL request retires only the detail fetches parked before it: a parked season
    /// switch and a parked page job are another read's, and still run.
    #[test]
    fn a_newer_detail_request_leaves_parked_season_and_page_jobs_to_run() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, mut seasons, season_job) = season_request();
        let (_, mut pages, pages_job) = pages_request();
        let mut runner = StoreRunner::held_all();
        let mut metadata = MetadataStore::default();

        runner.start(season_job);
        runner.start(pages_job);
        runner.start(request(&mut metadata, ServerId::UNSET, "d1"));
        assert_eq!(runner.run_held(), 3, "both kept, and the detail job parked after them");

        season_pump(&mut seasons);
        assert_eq!(season_state(&seasons), (0, false), "the season job ran and answered");
        pages.pump(plx_machine::landgate::fixture_gate());
        assert!(!pages_held(&mut pages), "and the page job did");
    }

    /// A season job a sender left on the bare store is started by the pump that finds it, in that
    /// call: the pump is a guard (no step emits a job from a landing today), and what it finds it
    /// starts.
    #[test]
    fn the_bridge_season_pump_starts_a_metadata_job_it_finds_in_the_same_call() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, _) = show_store();
        let mut bridge = Bridge::for_test(|| 0);
        show_on(bridge.metadata_mut(), sid);
        assert!(bridge.metadata_mut().run(MetadataCmd::LoadSeason(1)));
        assert!(bridge.metadata_mut().has_jobs());

        bridge.metadata_pump_season();

        bridge.assert_no_store_jobs_left_for_test();
    }

    /// The dispatcher's entry: `metadata_run` starts the job its command emitted in that call.
    #[test]
    fn the_bridge_metadata_run_starts_the_season_switch_and_the_page_window_it_emitted() {
        use plx_data::metadata::EPISODE_PAGE;
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, _) = show_store();
        let mut bridge = Bridge::for_test(|| 0);
        show_on(bridge.metadata_mut(), sid);

        assert!(bridge.metadata_run(MetadataCmd::WantEpisodes { lo: EPISODE_PAGE, hi: EPISODE_PAGE + 10, focus: None, restore: None }));
        bridge.assert_no_store_jobs_left_for_test();
        assert_eq!(bridge.run_held_store_jobs_for_test(), 1, "the held runner parked the page window");
        assert!(bridge.metadata_run(MetadataCmd::LoadSeason(1)));
        bridge.assert_no_store_jobs_left_for_test();
    }

    // ---- a Related window read -----------------------------------------------------------------

    /// Cards before the tail in the loopback's `/similar` listing (the preview the page holds) and
    /// the tail behind them.
    const RELATED_PREVIEW: usize = 3;
    const RELATED_TAIL: usize = 100;

    /// The value of a query parameter that is a number, e.g. `X-Plex-Container-Start`.
    fn container_param(query: &str, name: &str) -> Option<usize> {
        query.split('&').find_map(|pair| pair.strip_prefix(name)?.strip_prefix('=')?.parse().ok())
    }

    /// A loopback PMS holding the one hub listing the Related row pages: the preview `h0..h3`, then
    /// the tail `t0..t100`, answered for the `X-Plex-Container-Start/Size` window asked, with the
    /// `offset` and `totalSize` the pager checks.
    fn related_listing(_path: &str, query: &str) -> (&'static str, String) {
        let total = RELATED_PREVIEW + RELATED_TAIL;
        let start = container_param(query, "X-Plex-Container-Start").unwrap_or(0);
        let size = container_param(query, "X-Plex-Container-Size").unwrap_or(usize::MAX);
        let cards: Vec<String> = (start.min(total)..start.saturating_add(size).min(total)).map(|at| {
            let rk = if at < RELATED_PREVIEW { format!("h{at}") } else { format!("t{}", at - RELATED_PREVIEW) };
            format!(r#"{{"ratingKey":"{rk}","type":"movie","title":"{rk}"}}"#)
        }).collect();
        ("200 OK", format!(
            r#"{{"MediaContainer":{{"size":{},"totalSize":{total},"offset":{start},"Metadata":[{}]}}}}"#,
            cards.len(), cards.join(","),
        ))
    }

    /// A movie page whose Related row is a 3-card head and the tail window `0..24` of the hub behind
    /// it, on a server registered at `port`.
    fn related_store_at(port: i32) -> (ServerId, MetadataStore) {
        assert!(plx_net::net::global_init() && plx_net::net::available() && plx_net::net::threaded_tls_ready());
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("runner", "127.0.0.1", port, "t", "cid");
        plx_plex::plex::client_for(sid).unwrap().set_link(plx_plex::plex::probe::Location::Local);
        let mut store = MetadataStore::default();
        related_on(&mut store, sid);
        (sid, store)
    }

    fn related_on(store: &mut MetadataStore, sid: ServerId) {
        use plx_data::metadata::{Detail, RelatedTail, TailHub};
        let card = |rk: String| plx_data::pms::PmsMovie { sid, rk, sec: 1, ..Default::default() };
        let mut related: Vec<_> = (0..RELATED_PREVIEW).map(|i| card(format!("h{i}"))).collect();
        related.extend((0..24).map(|n| card(format!("t{n}"))));
        plx_data::metadata::set_current_for_test(store.state_mut(), Some(Detail {
            sid, rk: "movie".into(), kind: "movie".into(), related,
            related_tail: RelatedTail {
                hubs: vec![TailHub {
                    key: "/library/metadata/1/similar".into(), preview: RELATED_PREVIEW, len: Some(RELATED_TAIL),
                }],
                head: RELATED_PREVIEW, positions: (0..24).collect(), offset: 0, end: 24, total: RELATED_TAIL,
                more: true, ..Default::default()
            },
            ..Default::default()
        }));
    }

    /// The edge ask the Related shelf makes at the end of the window `0..24`.
    fn related_ask(store: &mut MetadataStore) -> bool {
        store.run(MetadataCmd::WantRelated { before: false, seen: (0, 24) })
    }

    /// The edge ask, and the one job it emitted.
    fn related_request_at(port: i32) -> (ServerId, MetadataStore, StoreJob) {
        let (sid, mut store) = related_store_at(port);
        assert!(related_ask(&mut store));
        let mut jobs = store.take_jobs();
        assert_eq!(jobs.len(), 1, "one ask, one request");
        (sid, store, jobs.remove(0))
    }

    /// The window the page holds, as `(offset, end)`.
    fn related_window(store: &MetadataStore) -> (usize, usize) {
        let tail = &store.view().current().expect("a movie is loaded").related_tail;
        (tail.offset, tail.end)
    }

    /// `(a read is out, the reader withdrew it)`.
    fn related_claim(store: &MetadataStore) -> (bool, bool) {
        plx_data::metadata::related_claim_for_test(store.adapter_ref())
    }

    /// How many answers wait in the mailbox for the pump: a read that answered the failure left one,
    /// a launch the runner refused left none.
    fn related_landed(store: &MetadataStore) -> usize {
        plx_data::metadata::related_landed_for_test(store.adapter_ref())
    }

    /// The registry holds the client: the worker starts, reads the next window from the server the
    /// launch resolved a client for, and posts its ONE answer. The server answers, so a window past
    /// the first is what a worker handed the client lands; one handed none lands a failure and the
    /// window stays. The kind is not on the tape, so nothing is recorded.
    #[test]
    fn a_related_job_whose_client_is_held_is_started_answered_once_and_not_taped() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let pms = Pms::serve_query(related_listing);
        let (_, mut store, job) = related_request_at(pms.port);
        assert_eq!(job.admission(), Admission::Direct);
        assert_eq!(related_claim(&store), (true, false), "the read is out");
        tape::init(None, false);

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![], None), "the kind is untaped: the runner records no admission");
        tape::reset_for_test();
        assert_eq!(related_claim(&store), (false, false), "answered once: the claim came back");
        assert!(store.pump_related(), "the read reached the server and its window landed");
        assert!(related_window(&store).1 > 24, "the window slid on past where it stood");
    }

    /// Replay launches the same worker the recording did: there is no admission record to consult.
    /// The server answers, so the window a worker handed the client lands tells a launch from a
    /// refusal (which gives the claim back too, and lands nothing) and from a dropped client.
    #[test]
    fn a_related_job_launches_live_under_replay_as_the_inline_spawn_did() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let pms = Pms::serve_query(related_listing);
        let (_, mut store, job) = related_request_at(pms.port);
        tape::init(None, true);
        tape::begin(Default::default(), Default::default());

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![], None));
        tape::reset_for_test();
        assert_eq!(related_claim(&store), (false, false), "a worker started and answered");
        assert!(store.pump_related(), "the worker read from the server and its window landed");
        assert!(related_window(&store).1 > 24, "the window slid on past where it stood");
    }

    /// The registry holds no client for the page's server by the launch: the worker still starts and
    /// answers the failure without a GET, as the inline worker did when its read found no client.
    #[test]
    fn a_related_job_whose_server_has_no_client_answers_the_failure() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, mut store, job) = related_request_at(9);
        plx_plex::plex::reset_servers_for_test();

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(related_claim(&store), (false, false), "answered once, as a failure");
        assert_eq!(related_landed(&store), 1, "the failure landed: a refused launch answers nothing");
        assert!(!store.pump_related(), "a failure installs nothing");
        assert_eq!(related_window(&store), (0, 24));
    }

    /// `held()` parks the Related job (no rig waits for a window landing): the claim stays up as a
    /// worker that has not answered holds it, and the test runs it. A parked job resolves its client
    /// when it runs and reads through it.
    #[test]
    fn a_held_related_job_is_parked_and_runs_once_when_the_test_runs_it() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let pms = Pms::serve_query(related_listing);
        let (_, mut store, job) = related_request_at(pms.port);
        let mut runner = StoreRunner::held();

        runner.start(job);
        plx_base::task::drain_workers_for_test();
        assert_eq!(related_claim(&store), (true, false), "parked: still out");
        assert!(!related_ask(&mut store), "and a second ask starts nothing beside it");
        assert!(!store.has_jobs());
        assert_eq!(runner.run_held(), 1);

        assert_eq!(related_claim(&store), (false, false), "the run answered once");
        assert!(store.pump_related(), "the run read from the server and its window landed");
        assert!(related_window(&store).1 > 24);
        assert_eq!(runner.run_held(), 0, "and nothing is left to run twice");
    }

    /// A parked job whose server has no client by the time it runs answers the failure.
    #[test]
    fn a_held_related_job_whose_client_is_gone_answers_the_failure() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let pms = Pms::serve_query(related_listing);
        let (_, mut store, job) = related_request_at(pms.port);
        let mut runner = StoreRunner::held_all();
        runner.start(job);
        plx_plex::plex::reset_servers_for_test();

        assert_eq!(runner.run_held(), 1);

        assert_eq!(related_claim(&store), (false, false), "the run answered");
        assert_eq!(related_landed(&store), 1, "as a failure that landed");
        assert!(!store.pump_related(), "as a failure: nothing installs");
        assert_eq!(related_window(&store), (0, 24));
        assert!(related_ask(&mut store), "and the next edge asks again");
    }

    /// The reader moved away while the job was parked: the answer is discarded when the job runs, the
    /// claim comes back with it, and the next edge asks again.
    #[test]
    fn a_cancel_while_the_related_job_is_parked_discards_the_answer_when_it_runs() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let pms = Pms::serve_query(related_listing);
        let (_, mut store, job) = related_request_at(pms.port);
        let mut runner = StoreRunner::held_all();
        runner.start(job);

        store.run(MetadataCmd::CancelRelated);
        assert_eq!(related_claim(&store), (true, true), "withdrawn while parked");
        assert_eq!(runner.run_held(), 1);

        assert_eq!(related_claim(&store), (false, false), "the run gave the claim back and spent the flag");
        assert!(!store.pump_related(), "the withdrawn answer installs nothing");
        assert_eq!(related_window(&store), (0, 24));
        assert!(related_ask(&mut store), "the next edge asks again");
    }

    /// The dispatcher's entry: `metadata_run` starts the job its command emitted in that call, for an
    /// edge ask and for a seek alike. The held runner parks it.
    #[test]
    fn the_bridge_metadata_run_starts_the_related_window_it_emitted() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, _) = related_store_at(9);
        let mut bridge = Bridge::for_test(|| 0);
        related_on(bridge.metadata_mut(), sid);

        assert!(bridge.metadata_run(MetadataCmd::WantRelated { before: false, seen: (0, 24) }));
        bridge.assert_no_store_jobs_left_for_test();
        assert_eq!(bridge.run_held_store_jobs_for_test(), 1, "the held runner parked the edge's window");

        assert!(bridge.metadata_run(MetadataCmd::SeekRelated { at: 60, seen: (0, 24) }));
        bridge.assert_no_store_jobs_left_for_test();
        assert_eq!(bridge.run_held_store_jobs_for_test(), 1, "and the seek's");
    }
}
