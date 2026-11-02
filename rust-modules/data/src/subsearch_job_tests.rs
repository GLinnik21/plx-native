//! A subtitle-search request is a typed job the store emits, not a thread it starts
//! (`docs/sans-io.md`), and which client a server holds is an input to the step, not a read of the
//! registry: what a pump emits and when, what the job says, how its answer or refusal comes back,
//! and what a refusal does to the pump that sees it.

use super::*;
use crate::stores::subsearch::SubSearchStore;
use crate::stores::StoreJob;
use plx_machine::landgate::Gate;

const KEY: ClientKey = ClientKey { instance: 7, token: 3, grant: 3 };

fn sid() -> ServerId { ServerId::from_raw(1) }

/// The registry's answer when the server holds the client `KEY` names.
fn holding() -> ClientKeys { ClientKeys::from_keys([(sid(), KEY)]) }

fn hit(key: &str) -> SubHit {
    SubHit { key: key.into(), title: "rel".into(), provider: "OpenSubtitles".into(),
        language: "Nederlands".into(), language_code: "nld".into(), codec: "srt".into(),
        score: 5, hearing_impaired: false, forced: false }
}

/// A model opened on item `506548` of the server, and the mailbox its requests answer into.
struct Rig {
    state: SubSearchState,
    adapter: Arc<SubSearchAdapter>,
}

impl Rig {
    fn opened(lang: &str) -> Self {
        let adapter = Arc::new(SubSearchAdapter::default());
        let mut state = SubSearchState::default();
        assert!(state.run(&adapter, SubSearchCmd::Open { sid: sid(), rk: "506548".into(), lang: lang.into() }));
        assert!(state.jobs.is_empty(), "a command emits no job: the request is the pump's");
        Rig { state, adapter }
    }

    /// One pump with the registry's answer `keys`, and the requests it emitted.
    fn pump(&mut self, keys: &ClientKeys) -> Vec<StoreJob> {
        self.state.pump_with_gate(&self.adapter, &Gate::default(), keys);
        std::mem::take(&mut self.state.jobs)
    }

    fn run(&mut self, cmd: SubSearchCmd) -> bool { self.state.run(&self.adapter, cmd) }

    fn port(&self) -> SubSearchPort { SubSearchPort::of(&self.adapter) }

    /// The job the pump emits for `work` at `generation`, as the step builds it.
    fn job(&self, generation: u32, work: Work) -> StoreJob {
        StoreJob::SubtitleSearch(
            Job { addr: subsearch_addr(generation), req: SubSearchReq { sid: sid(), key: KEY, rk: "506548".into(), work } },
            self.port())
    }

    /// The one request the pump emitted, taken apart.
    fn one(&mut self, keys: &ClientKeys) -> (Job<SubSearchReq>, SubSearchPort) {
        let mut jobs = self.pump(keys);
        assert_eq!(jobs.len(), 1, "one request is out at a time");
        let StoreJob::SubtitleSearch(job, port) = jobs.remove(0) else { panic!("not a subtitle-search job") };
        (job, port)
    }

    fn claim_held(&self) -> bool { self.adapter.busy_for_test() }
}

fn search(lang: &str) -> Work { Work::Search { lang: lang.into() } }

/// A pump over an opened search emits the request and says which client it decided on; a command
/// emits nothing; and one request is out at a time, so a second pump emits nothing until the first
/// is answered.
#[test]
fn an_open_search_emits_one_request_naming_the_client_it_decided_on() {
    let mut rig = Rig::opened("nld");
    let generation = rig.state.generation();

    plx_machine::assert_fx!(rig.pump(&holding()), [rig.job(generation, search("nl"))]);
    assert!(rig.claim_held(), "the request holds the claim while it is out");

    plx_machine::assert_fx!(rig.pump(&holding()), []);
}

/// The request names the item and the work, a pure function of the model and the registry's
/// answer: search, then the download the viewer pressed, then the install poll that follows it,
/// each as the answer to the one before.
#[test]
fn each_answer_makes_the_next_pump_emit_the_next_request() {
    let mut rig = Rig::opened("nl");
    let generation = rig.state.generation();
    plx_machine::assert_fx!(rig.pump(&holding()), [rig.job(generation, search("nl"))]);

    rig.adapter.land_for_test(generation, Landing::Search(Ok(vec![hit("/library/streams/1")])));
    plx_machine::assert_fx!(rig.pump(&holding()), []); // the answer is shown; nothing more is owed
    assert!(!rig.claim_held(), "the answer released the claim");

    assert!(rig.run(SubSearchCmd::Download { gen: generation, hit: 0 }));
    plx_machine::assert_fx!(rig.pump(&holding()),
        [rig.job(generation, Work::Download { key: "/library/streams/1".into() })]);

    // accepted: the install is asynchronous, so the poll waits a beat before it reads
    rig.adapter.land_for_test(generation, Landing::Download(Ok(vec![10])));
    plx_machine::assert_fx!(rig.pump(&holding()), []);
    for _ in 1..POLL_FRAMES { plx_machine::assert_fx!(rig.pump(&holding()), []); }
    plx_machine::assert_fx!(rig.pump(&holding()), [rig.job(generation, Work::Refresh)]);
}

/// A server that holds no client has nothing to ask: no job, no claim, and the next attempt is
/// held off for a stretch of frames, then made at once when the server has a client again.
#[test]
fn a_server_without_a_client_emits_nothing_and_holds_off() {
    let mut rig = Rig::opened("nl");
    let generation = rig.state.generation();
    let none = ClientKeys::default();

    plx_machine::assert_fx!(rig.pump(&none), []);
    assert!(!rig.claim_held(), "no claim was made");
    assert_eq!(rig.state.retry_for_test(), RETRY_FRAMES);

    // The client appears mid-wait: the hold-off still runs its course.
    for _ in 1..RETRY_FRAMES { plx_machine::assert_fx!(rig.pump(&holding()), []); }
    plx_machine::assert_fx!(rig.pump(&holding()), [rig.job(generation, search("nl"))]);
}

/// A refusal hands the request's claim back in the call that refuses it. The pump after it reads
/// the refusal BEFORE it decides to spawn and backs off: with only the release it would find the
/// claim free and emit the same request again, every pump.
#[test]
fn a_refused_request_is_asked_again_only_after_the_backoff() {
    let mut rig = Rig::opened("nl");
    let generation = rig.state.generation();
    let (job, port) = rig.one(&holding());
    assert!(rig.claim_held());

    port.refused(job.addr);
    assert!(!rig.claim_held(), "the refusal released the claim in the same call");
    assert!(!rig.adapter.has_mail_for_test(), "a refusal answers nothing");

    // The refusal is seen one pump after it is made, and holds the full wait.
    plx_machine::assert_fx!(rig.pump(&holding()), []);
    assert_eq!(rig.state.retry_for_test(), RETRY_FRAMES, "the pump that saw it backed off");
    for _ in 1..RETRY_FRAMES { plx_machine::assert_fx!(rig.pump(&holding()), []); }
    plx_machine::assert_fx!(rig.pump(&holding()), [rig.job(generation, search("nl"))]);
    assert!(rig.claim_held(), "...and the request that follows claims again");
}

/// A refusal is observed once: a request refused, asked again and answered is not backed off a
/// second time by the memory of the first.
#[test]
fn a_refusal_backs_off_one_request_and_not_the_one_after() {
    let mut rig = Rig::opened("nl");
    let generation = rig.state.generation();
    let (job, port) = rig.one(&holding());
    port.refused(job.addr);
    for _ in 0..RETRY_FRAMES { let _ = rig.pump(&holding()); }
    let (job, _) = rig.one(&holding()); // the second attempt
    assert_eq!(job.addr.req.0, generation);

    rig.adapter.land_for_test(generation, Landing::Search(Ok(vec![hit("/library/streams/1")])));
    plx_machine::assert_fx!(rig.pump(&holding()), []);
    assert_eq!(rig.state.retry_for_test(), 0, "the answered request left no backoff behind");
}

/// A refusal that arrives after the search has moved on leaves the newer claim alone, and the
/// newer request is not backed off for it: the runner refuses a job on its own schedule, and by
/// then the viewer may have changed the language.
#[test]
fn a_refusal_for_a_superseded_request_leaves_the_newer_one_alone() {
    let mut rig = Rig::opened("nl");
    let (old, old_port) = rig.one(&holding());

    assert!(rig.run(SubSearchCmd::SetLanguage("en".into()))); // supersedes the first search
    let (current, _) = rig.one(&holding());
    assert_ne!(old.addr, current.addr);

    old_port.refused(old.addr);
    assert!(rig.claim_held(), "the old request's refusal did not release the claim the new one holds");
    plx_machine::assert_fx!(rig.pump(&holding()), []);
    assert_eq!(rig.state.retry_for_test(), 0, "...nor did it back the new request off");
}

/// An answer is stamped with its request's generation, so the pump applies the current search's
/// answer and discards one for a search it has since superseded.
#[test]
fn an_answer_applies_to_its_own_search_and_a_superseded_one_is_dropped() {
    let mut rig = Rig::opened("nl");
    let (stale, stale_port) = rig.one(&holding());
    assert!(rig.run(SubSearchCmd::SetLanguage("en".into())));
    let (current, current_port) = rig.one(&holding());

    stale_port.done(stale.addr, SubSearchAnswer(Landing::Search(Ok(vec![hit("/library/streams/9")]))));
    rig.state.pump_with_gate(&rig.adapter, &Gate::default(), &holding());
    assert_eq!(rig.state.view().status(), SearchStatus::Searching, "a superseded search's answer lands nowhere");
    assert!(rig.claim_held(), "...and does not release the claim of the request still out");

    current_port.done(current.addr, SubSearchAnswer(Landing::Search(Ok(vec![hit("/library/streams/1")]))));
    assert!(rig.state.pump_with_gate(&rig.adapter, &Gate::default(), &holding()));
    assert_eq!(rig.state.view().status(), SearchStatus::Ready);
    assert_eq!(rig.state.view().hits().len(), 1);
}

/// A job's port is captured when the job is emitted. A `Reset` between the request and the runner
/// rotates the store's adapter, and the job still answers into the retired one, where nothing is
/// pumped, never into the replacement's landing.
#[test]
fn a_job_emitted_before_a_reset_answers_into_the_retired_adapter() {
    let mut store = SubSearchStore::default();
    assert!(!store.has_jobs());
    assert!(store.run(SubSearchCmd::Open { sid: sid(), rk: "506548".into(), lang: "nl".into() }));
    assert!(!store.has_jobs(), "a command emits no job");
    store.pump(&Gate::default(), &holding());
    assert!(store.has_jobs());
    let mut jobs = store.take_jobs();
    assert!(!store.has_jobs(), "taking the jobs empties the outbox");
    assert_eq!(jobs.len(), 1);
    let StoreJob::SubtitleSearch(job, port) = jobs.remove(0) else { panic!("not a subtitle-search job") };
    assert_eq!(port, store.subsearch_port());

    store.run(SubSearchCmd::Reset);
    assert_ne!(port, store.subsearch_port(), "the job holds the retired adapter's port");
    assert!(store.run(SubSearchCmd::Open { sid: sid(), rk: "506548".into(), lang: "nl".into() }));

    port.done(job.addr, SubSearchAnswer(Landing::Search(Ok(vec![]))));
    assert!(!store.adapter_for_test().has_mail_for_test(), "the replacement's landing never sees the answer");
}
