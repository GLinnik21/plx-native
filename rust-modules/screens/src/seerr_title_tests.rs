//! The Seerr title page: its status line, its facts, its request line and season chips, and the
//! page stepped over a real Seerr store fed through the store's own job path (a scripted seam, no
//! network): who gets Approve and Decline, what they ask, and the read-back once a decision lands.
//! What its pill offers is the store's decision (`plx_data::stores::seerr::offer`), tested there.
use super::*;
use crate::registry::{AppMsg, PageMemory};
use plx_data::search::view::SearchView;
use plx_data::seerr::{Season, SeerrIo, SeerrSession, SeerrUser, Title, TitleFacts};
use plx_data::stores::StoreJob;
use plx_data::watchlist::WatchlistInput;
use plx_machine::machine::{FocusRead, Host, InstanceId, MachineId, PressRead, Stamped};
use plx_machine::present::Present;
use plx_ui::fixture::FixtureMeasure;
use plx_ui::screen::ScreenArg;
use std::sync::{Arc, Mutex};

fn movie(status: Availability) -> TitleDetail {
    TitleDetail { title: Title { kind: Some(MediaKind::Movie), tmdb: 1, status, ..Default::default() }, ..Default::default() }
}

fn waiting(id: i64) -> MediaRequest {
    MediaRequest { id, kind: MediaKind::Movie, tmdb: 1, status: RequestStatus::PendingApproval, availability: Availability::Pending,
        seasons: Vec::new(), requested_by: Some("s0000a1ex".into()), modified_by: None, created: None, updated: None,
        downloads: Vec::new() }
}

#[test]
fn the_status_line_says_the_decision_then_the_request_then_the_server() {
    let detail = movie(Availability::Unknown);
    assert_eq!(status_line(&detail, None, None), None, "a title nobody asked for has no status");
    assert_eq!(status_line(&detail, Some(&Answer::Ready(SeerrData::Requested)), None), Some(msg::seerr_request_done()));
    assert_eq!(status_line(&detail, Some(&Answer::Failed(SeerrError::AlreadyRequested)), None), Some(msg::seerr_request_already()));
    assert_eq!(status_line(&detail, Some(&Answer::Failed(SeerrError::Forbidden)), None), Some(msg::seerr_request_forbidden()));
    assert_eq!(status_line(&detail, Some(&Answer::Failed(SeerrError::Transport)), None), Some(msg::seerr_request_failed()));
    assert_eq!(status_line(&movie(Availability::Processing), None, None), Some(msg::seerr_status_processing()));
    assert_eq!(status_line(&movie(Availability::Available), Some(&Answer::Loading), None), Some(msg::seerr_status_available()));
    let refused = Answer::Failed(SeerrError::Forbidden);
    assert_eq!(status_line(&detail, Some(&Answer::Ready(SeerrData::Requested)), Some(&refused)), Some(msg::seerr_decide_forbidden()));
    assert_eq!(status_line(&detail, None, Some(&Answer::Failed(SeerrError::AlreadyRequested))), Some(msg::seerr_decide_conflict()));
    assert_eq!(status_line(&detail, None, Some(&Answer::Ready(SeerrData::Decided))), None);
}

#[test]
fn the_facts_line_names_what_tmdb_said_and_leaves_out_what_it_did_not() {
    let _g = plx_base::testlock::serial();
    let mut detail = movie(Availability::Unknown);
    detail.title.year = Some(2024);
    detail.facts = TitleFacts { rating: Some(8.2), runtime_min: Some(166), genres: vec!["s0g".into(), "s1g".into(), "s2g".into()],
        studio: Some("s0st".into()), ..Default::default() };
    assert_eq!(facts(MediaKind::Movie, &detail), [
        msg::browse_kind_movie().to_string(), "2024".into(),
        fmt::duration_minutes_in(plx_platform::i18n::current(), 166),
        fmt::rating_score(fmt::RatingScale::Percent, f64::from(8.2f32)),
        "s0g".into(), "s1g".into(), "s0st".into(),
    ], "two genres at most");
    assert_eq!(facts(MediaKind::Tv, &movie(Availability::Unknown)), [msg::browse_kind_tv_show().to_string()]);
}

#[test]
fn the_request_line_and_the_season_chips_say_where_things_stand() {
    let _g = plx_base::testlock::serial();
    let mut detail = movie(Availability::Pending);
    assert_eq!(request_line(&detail), None, "no request, no line");
    detail.requests = vec![waiting(5)];
    assert_eq!(request_line(&detail).unwrap(),
        format!("{} · {}", msg::seerr_chip_pending(), msg::seerr_row_requested_by("s0000a1ex")));
    detail.seasons = vec![Season { number: 1, episodes: None, status: Availability::Available },
        Season { number: 2, episodes: None, status: Availability::Unknown }];
    assert_eq!(season_chips(&detail), [
        format!("{} · {}", fmt::season_identity(1, ""), msg::seerr_chip_available()),
        format!("{} · {}", fmt::season_identity(2, ""), msg::seerr_chip_missing()),
    ]);
}

#[test]
fn chips_wrap_to_a_new_line_only_when_the_next_does_not_fit() {
    let laid = chip_lines(&[300.0, 300.0, 300.0, 300.0], 100.0, 950.0);
    let lines: Vec<usize> = laid.iter().map(|(_, line)| *line).collect();
    assert_eq!(lines, [0, 0, 0, 1]);
    assert_eq!(laid[3].0, 100.0, "a new line starts at the left edge");
    assert_eq!(chip_lines(&[2000.0], 100.0, 950.0), [(100.0, 0)], "a chip wider than the line still stands");
}

// ---- the page, stepped ----

#[derive(Clone)]
struct Arg;
impl LogicalState for Arg {
    fn write(&self, _: &mut Canon) {}
    fn probe(&self, _: &mut String) {}
}
impl ScreenArg for Arg {
    fn chrome(&self) -> plx_machine::machine::Chrome { plx_machine::machine::Chrome::None }
    fn id(&self) -> plx_machine::machine::ScreenId { plx_machine::machine::ScreenId(1) }
    fn title(&self) -> Option<&str> { None }
    fn same_instance(&self, _: &Self) -> bool { true }
}

struct SeerrHost;
impl Host for SeerrHost {
    type Arg = Arg;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = (SearchView<'a>, &'a SeerrStore);
    type Init = Arg;
    type Memory = PageMemory;
}
impl SearchLike for SeerrHost {
    fn search<'a>(cx: &Cx<'a, Self>) -> SearchView<'a> { cx.views.0 }
    fn seerr<'a>(cx: &Cx<'a, Self>) -> Option<&'a SeerrStore> { Some(cx.views.1) }
}

const ENTRY: EntryId = EntryId(70);

/// Signs in with `permissions`; the title (a movie someone asked for) waits for approval until a
/// decision is sent, which the script records and answers with `decide`.
struct Script {
    permissions: u64,
    decided: Mutex<Vec<SeerrOp>>,
    decide: Result<SeerrData, SeerrError>,
}
impl SeerrIo for Script {
    fn sign_in(&self, _: &str, _: &plx_data::seerr::SeerrLogin) -> Result<(SeerrSession, SeerrUser), SeerrError> {
        Ok((SeerrSession::for_test("connect.sid=x"), SeerrUser { id: 1, name: "s00000001".into(), permissions: self.permissions }))
    }
    fn run(&self, _: &str, _: &SeerrSession, _: &SeerrUser, op: &SeerrOp) -> Result<SeerrData, SeerrError> {
        match op {
            SeerrOp::Detail { .. } => {
                let mut detail = movie(Availability::Pending);
                if self.decided.lock().unwrap().is_empty() {
                    detail.requests = vec![waiting(5)];
                } else {
                    detail.requests = vec![MediaRequest { status: RequestStatus::Approved, ..waiting(5) }];
                }
                Ok(SeerrData::Detail(detail))
            }
            SeerrOp::Decide { .. } => {
                self.decided.lock().unwrap().push(op.clone());
                self.decide.clone()
            }
            other => panic!("the title page asked for {other:?}"),
        }
    }
}

struct Fixture {
    search: plx_data::stores::search::SearchSnapshot,
    seerr: SeerrStore,
    script: Arc<Script>,
    input: WatchlistInput,
    measure: FixtureMeasure,
}

impl Fixture {
    fn new(permissions: u64, decide: Result<SeerrData, SeerrError>) -> Self {
        let script = Arc::new(Script { permissions, decided: Mutex::new(Vec::new()), decide });
        let mut seerr = SeerrStore::with_io(script.clone());
        seerr.set_address("http://127.0.0.1:5055");
        Self { search: plx_data::stores::search::SearchStore::default().snapshot(), seerr, script,
            input: WatchlistInput::for_test(1), measure: FixtureMeasure }
    }

    fn step(&self, screen: &mut SeerrTitleScreen, ev: ScreenEvent<SeerrHost>) -> Vec<Stamped<SeerrHost>> {
        let cx = Cx { views: (self.search.view(), &self.seerr), tick: Tick::default(), measure: &self.measure,
            focus: FocusRead::default(), press: PressRead::default(),
            owner: plx_machine::machine::InputOwner::Entry(ENTRY) };
        let mut out = Vec::new();
        let mut present = Present::new();
        let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(71)), &mut present);
        Machine::<SeerrHost>::step(screen, &ev, &cx, &mut fx);
        out
    }

    /// Ask what `out` asks, as the Bridge would, and land the answers.
    fn serve(&mut self, out: &[Stamped<SeerrHost>]) -> Vec<Slot> {
        let mut asked = Vec::new();
        for stamped in out {
            if let Fx::App(AppFx::Store(StoreId::Seerr, StoreCmd::Seerr(cmd))) = &stamped.fx {
                let (SeerrCmd::Ask { slot, .. } | SeerrCmd::Refresh { slot, .. }) = cmd;
                asked.push(slot.clone());
                self.seerr.run(cmd.clone(), &self.input);
            }
        }
        for job in self.seerr.take_jobs() {
            let StoreJob::Seerr(job, port) = job else { unreachable!() };
            plx_data::stores::seerr::finish_seerr_job(&port, job.addr, job.req);
        }
        self.seerr.pump(self.input.profile());
        asked
    }

    /// Step `ev`, then tick until the page asks nothing more; every slot asked on the way.
    fn run(&mut self, screen: &mut SeerrTitleScreen, ev: ScreenEvent<SeerrHost>) -> Vec<Slot> {
        let out = self.step(screen, ev);
        let mut asked = self.serve(&out);
        for _ in 0..4 {
            let out = self.step(screen, ScreenEvent::Tick(Tick::default()));
            let more = self.serve(&out);
            if more.is_empty() {
                break;
            }
            asked.extend(more);
        }
        asked
    }

    fn controls(&self, screen: &SeerrTitleScreen) -> Vec<u32> {
        screen.controls(Some(&self.seerr)).iter().map(|(elem, _)| *elem).collect()
    }
}

#[test]
fn only_a_request_manager_gets_approve_and_decline() {
    let _g = plx_base::testlock::serial();
    let mut plain = Fixture::new(plx_plex::plex::seerr::PERMISSION_REQUEST_ADVANCED, Ok(SeerrData::Decided));
    let mut screen = SeerrTitleScreen::new(ENTRY, MediaKind::Movie, 1);
    plain.run(&mut screen, ScreenEvent::Mount);
    assert!(plain.controls(&screen).is_empty(), "a requested movie offers no pill, and this user may not decide");
    let mut manager = Fixture::new(plx_plex::plex::seerr::PERMISSION_MANAGE_REQUESTS, Ok(SeerrData::Decided));
    let mut screen = SeerrTitleScreen::new(ENTRY, MediaKind::Movie, 1);
    manager.run(&mut screen, ScreenEvent::Mount);
    assert_eq!(manager.controls(&screen), [APPROVE_ELEM, DECLINE_ELEM]);
    let rects = screen.control_rects(Some(&manager.seerr), &manager.measure);
    assert!(rects[0].1.x + rects[0].1.w < rects[1].1.x, "side by side: {rects:?}");
}

#[test]
fn approving_sends_the_decision_and_reads_the_title_back_once() {
    let _g = plx_base::testlock::serial();
    let mut fixture = Fixture::new(plx_plex::plex::seerr::PERMISSION_ADMIN, Ok(SeerrData::Decided));
    let mut screen = SeerrTitleScreen::new(ENTRY, MediaKind::Movie, 1);
    fixture.run(&mut screen, ScreenEvent::Mount);
    let asked = fixture.run(&mut screen, ScreenEvent::Activate(APPROVE_ELEM));
    assert_eq!(asked, [Slot::Decide(5), Slot::Detail(MediaKind::Movie, 1)], "the decision, then one read-back");
    assert_eq!(*fixture.script.decided.lock().unwrap(), [SeerrOp::Decide { id: 5, decision: Decision::Approve }]);
    assert!(fixture.controls(&screen).is_empty(), "nothing waits any more");
    assert!(fixture.run(&mut screen, ScreenEvent::Tick(Tick::default())).is_empty(), "read back once, not every frame");
}

#[test]
fn a_decision_on_its_way_says_so_and_a_second_press_sends_nothing() {
    let _g = plx_base::testlock::serial();
    let mut fixture = Fixture::new(plx_plex::plex::seerr::PERMISSION_ADMIN, Ok(SeerrData::Decided));
    let mut screen = SeerrTitleScreen::new(ENTRY, MediaKind::Movie, 1);
    fixture.run(&mut screen, ScreenEvent::Mount);
    let out = fixture.step(&mut screen, ScreenEvent::Activate(DECLINE_ELEM));
    for stamped in &out {
        if let Fx::App(AppFx::Store(_, StoreCmd::Seerr(cmd))) = &stamped.fx {
            fixture.seerr.run(cmd.clone(), &fixture.input);
        }
    }
    let labels = screen.controls(Some(&fixture.seerr));
    assert_eq!(labels[1], (DECLINE_ELEM, msg::seerr_decide_declining_c()));
    assert_eq!(labels[0], (APPROVE_ELEM, msg::seerr_decide_approve_c()));
    let again = fixture.step(&mut screen, ScreenEvent::Activate(APPROVE_ELEM));
    assert!(again.iter().all(|s| !matches!(&s.fx, Fx::App(AppFx::Store(..)))), "one decision at a time");
}

#[test]
fn a_refused_decision_says_why_and_keeps_the_buttons() {
    let _g = plx_base::testlock::serial();
    let mut fixture = Fixture::new(plx_plex::plex::seerr::PERMISSION_ADMIN, Err(SeerrError::Forbidden));
    let mut screen = SeerrTitleScreen::new(ENTRY, MediaKind::Movie, 1);
    fixture.run(&mut screen, ScreenEvent::Mount);
    let asked = fixture.run(&mut screen, ScreenEvent::Activate(APPROVE_ELEM));
    assert_eq!(asked, [Slot::Decide(5)], "a refusal is not read back");
    let detail = screen.detail(Some(&fixture.seerr)).unwrap();
    assert_eq!(status_line(detail, None, screen.decision(Some(&fixture.seerr))), Some(msg::seerr_decide_forbidden()));
    assert_eq!(fixture.controls(&screen), [APPROVE_ELEM, DECLINE_ELEM], "it can be tried again");
}
