//! The request menu (docs/seerr.md): which server, profile and folder each row stands on, what a
//! request names, the rows each answer builds, and the menu stepped over a real Seerr store fed
//! through the store's own job path (a scripted seam, no network): what it asks, how a choice moves
//! the check, and what Send asks.
use super::*;
use crate::registry::{AppMsg, PageMemory};
use plx_data::search::view::SearchView;
use plx_data::seerr::{Availability, QualityProfile, RootFolder, Season, SeerrError, SeerrIo, SeerrSession, SeerrUser,
    Title, TitleDetail};
use plx_data::stores::StoreJob;
use plx_data::watchlist::WatchlistInput;
use plx_machine::machine::{FocusRead, Host, InstanceId, MachineId, PressRead, Stamped, Tick};
use plx_machine::present::Present;
use plx_ui::fixture::FixtureMeasure;
use plx_ui::screen::ScreenArg;
use std::sync::{Arc, Mutex};

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

const ENTRY: EntryId = EntryId(90);
const TMDB: i64 = 1399;

fn server(id: i64, is4k: bool, is_default: bool, profile: i64, folder: &str) -> ArrServer {
    ArrServer { id, name: format!("s{id:08x}"), is4k, is_default, profile: Some(profile), folder: Some(folder.into()),
        ..Default::default() }
}

/// A 4K server marked default (never offered), the ordinary default, and a second ordinary one.
fn servers() -> Vec<ArrServer> {
    vec![server(0, true, true, 5, "/4k"), server(1, false, true, 4, "/tv"), server(2, false, false, 1, "/archive")]
}

fn options(server: i64) -> ArrOptions {
    ArrOptions {
        profiles: vec![QualityProfile { id: 1, name: "s00000011".into() }, QualityProfile { id: 4, name: "s00000014".into() }],
        folders: vec![RootFolder { path: "/tv".into(), free: Some(3 << 40) },
            RootFolder { path: format!("/srv{server}"), free: None }],
    }
}

#[test]
fn every_row_stands_on_the_servers_own_choice_until_the_viewer_changes_it() {
    let servers = servers();
    let none = effective(&servers, false, &Choices::default());
    assert_eq!((none.server.map(|s| s.id), none.profile, none.folder.as_deref()), (Some(1), Some(4), Some("/tv")),
        "the non-4K default server and its defaults");
    let picked = effective(&servers, false, &Choices { server: Some(2), ..Default::default() });
    assert_eq!((picked.server.map(|s| s.id), picked.profile, picked.folder.as_deref()), (Some(2), Some(1), Some("/archive")));
    let four_k = effective(&servers, false, &Choices { server: Some(0), ..Default::default() });
    assert_eq!(four_k.server.map(|s| s.id), Some(1), "a 4K server is never chosen");
    let mine = effective(&servers, false, &Choices { profile: Some(1), folder: Some("/srv1".into()), ..Default::default() });
    assert_eq!((mine.profile, mine.folder.as_deref()), (Some(1), Some("/srv1")));
    let anime = vec![ArrServer { anime_profile: Some(6), anime_folder: Some("/anime".into()), ..server(1, false, true, 4, "/tv") }];
    let shown = effective(&anime, true, &Choices::default());
    assert_eq!((shown.profile, shown.folder.as_deref()), (Some(6), Some("/anime")), "an anime show's defaults");
}

#[test]
fn a_request_names_only_what_the_viewer_changed() {
    let servers = servers();
    assert_eq!(request_options(&servers, false, &Choices::default()), RequestOptions::default(),
        "nothing changed: the server decides everything");
    let same = Choices { server: Some(1), profile: Some(4), folder: Some("/tv".into()) };
    assert_eq!(request_options(&servers, false, &same), RequestOptions::default(), "the defaults picked by hand");
    assert_eq!(request_options(&servers, false, &Choices { server: Some(2), ..Default::default() }),
        RequestOptions { server: Some(2), ..Default::default() }, "another server, on its own defaults");
    assert_eq!(request_options(&servers, false, &Choices { profile: Some(1), folder: Some("/srv1".into()), ..Default::default() }),
        RequestOptions { server: None, profile: Some(1), folder: Some("/srv1".into()) });
    assert_eq!(request_options(&[], false, &Choices::default()), RequestOptions::default(), "no servers known");
}

fn labels(form: Form<RowId, Act, Infallible>) -> Vec<(String, bool)> {
    let mut table: FormTable<RowId, Act, Infallible> = FormTable::new(plx_ui::table_screen::BAND_BASE);
    table.open(form, None);
    (0..table.table.n_rows() as usize)
        .filter_map(|i| plx_ui::table::row_mut_in(&mut table.table.sections, i).map(|r| (r.label.clone(), r.checked)))
        .collect()
}

#[test]
fn the_rows_follow_each_answer_and_check_the_choice_in_force() {
    let servers = servers();
    let chosen = effective(&servers, false, &Choices::default());
    let loading = labels(form("Send", Load::Loading, Load::Loading, &chosen));
    assert_eq!(loading, [("Send".into(), false), (msg::seerr_menu_loading().into(), false)]);
    let failed = labels(form("Send", Load::Failed, Load::Loading, &chosen));
    assert_eq!(failed[1].0, msg::seerr_menu_unavailable());

    let opts = options(1);
    let rows = labels(form("Send", Load::Ready(&servers), Load::Ready(&opts), &chosen));
    assert_eq!(rows, [
        ("Send".into(), false),
        ("s00000001".into(), true), ("s00000002".into(), false), // the two ordinary servers, no 4K one
        ("s00000011".into(), false), ("s00000014".into(), true),
        ("/tv".into(), true), ("/srv1".into(), false),
    ]);
    let single = vec![server(1, false, true, 4, "/tv")];
    let rows = labels(form("Send", Load::Ready(&single), Load::Ready(&opts), &effective(&single, false, &Choices::default())));
    assert!(!rows.iter().any(|(l, _)| l == "s00000001"), "one server is not a choice: {rows:?}");
}

/// Signs in as a user who may choose, answers the menu's reads, records what a request named.
struct Script {
    sent: Mutex<Vec<SeerrOp>>,
}
impl SeerrIo for Script {
    fn sign_in(&self, _: &str, _: &plx_data::seerr::SeerrLogin) -> Result<(SeerrSession, SeerrUser), SeerrError> {
        Ok((SeerrSession::for_test("connect.sid=x"),
            SeerrUser { id: 1, name: "s00000001".into(), permissions: plx_plex::plex::seerr::PERMISSION_ADMIN }))
    }
    fn run(&self, _: &str, _: &SeerrSession, _: &SeerrUser, op: &SeerrOp) -> Result<SeerrData, SeerrError> {
        Ok(match op {
            SeerrOp::Detail { kind, tmdb } => SeerrData::Detail(TitleDetail {
                title: Title { kind: Some(*kind), tmdb: *tmdb, ..Default::default() },
                seasons: vec![Season { number: 1, episodes: Some(8), status: Availability::Available },
                    Season { number: 2, episodes: Some(8), status: Availability::Unknown }],
                ..Default::default()
            }),
            SeerrOp::Servers { .. } => SeerrData::Servers(servers()),
            SeerrOp::ServerOptions { server, .. } => SeerrData::ServerOptions(options(*server)),
            SeerrOp::Request { .. } => {
                self.sent.lock().unwrap().push(op.clone());
                SeerrData::Requested
            }
            other => panic!("the request menu asked for {other:?}"),
        })
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
    fn new() -> Self {
        let script = Arc::new(Script { sent: Mutex::new(Vec::new()) });
        let mut seerr = SeerrStore::with_io(script.clone());
        seerr.set_address("http://127.0.0.1:5055");
        let mut fixture = Self { search: plx_data::stores::search::SearchStore::default().snapshot(), seerr, script,
            input: WatchlistInput::for_test(1), measure: FixtureMeasure };
        fixture.ask(Slot::Detail(MediaKind::Tv, TMDB), SeerrOp::Detail { kind: MediaKind::Tv, tmdb: TMDB });
        fixture
    }

    fn ask(&mut self, slot: Slot, op: SeerrOp) {
        self.seerr.ask(slot, op, &self.input);
        self.land();
    }

    fn land(&mut self) {
        for job in self.seerr.take_jobs() {
            let StoreJob::Seerr(job, port) = job else { unreachable!() };
            plx_data::stores::seerr::finish_seerr_job(&port, job.addr, job.req);
        }
        self.seerr.pump(self.input.profile());
    }

    fn step(&self, screen: &mut SeerrRequestScreen, ev: ScreenEvent<SeerrHost>) -> Vec<Stamped<SeerrHost>> {
        let cx = Cx { views: (self.search.view(), &self.seerr), tick: Tick::default(), measure: &self.measure,
            focus: FocusRead::default(), press: PressRead::default(),
            owner: plx_machine::machine::InputOwner::Entry(ENTRY) };
        let mut out = Vec::new();
        let mut present = Present::new();
        let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(91)), &mut present);
        Machine::<SeerrHost>::step(screen, &ev, &cx, &mut fx);
        out
    }

    /// Ask what `out` asks of the store, as the Bridge would, and land the answers.
    fn serve(&mut self, out: &[Stamped<SeerrHost>]) -> Vec<Slot> {
        let mut asked = Vec::new();
        for stamped in out {
            if let Fx::App(AppFx::Store(StoreId::Seerr, StoreCmd::Seerr(SeerrCmd::Ask { slot, op }))) = &stamped.fx {
                asked.push(slot.clone());
                self.seerr.ask(slot.clone(), op.clone(), &self.input);
            }
        }
        self.land();
        asked
    }

    /// Step `ev`, serve what it asked, then tick so the menu reads the answers.
    fn run(&mut self, screen: &mut SeerrRequestScreen, ev: ScreenEvent<SeerrHost>) -> (Vec<Slot>, Vec<Stamped<SeerrHost>>) {
        let out = self.step(screen, ev);
        let mut asked = self.serve(&out);
        let tick = self.step(screen, ScreenEvent::Tick(Tick::default()));
        asked.extend(self.serve(&tick));
        self.step(screen, ScreenEvent::Tick(Tick::default()));
        (asked, out)
    }
}

fn menu() -> SeerrRequestScreen {
    SeerrRequestScreen::new(ENTRY, SeerrRequestArg { kind: MediaKind::Tv, tmdb: TMDB, anchor: [0; 4] })
}

fn elem(screen: &SeerrRequestScreen, id: RowId) -> u32 {
    assert!(screen.form.index_of(&id).is_some(), "{id:?} is a row");
    id.key().0
}

/// The menu asks for the servers when it mounts and for the default server's options once they
/// land; another server asks for its own and starts its rows on its defaults.
#[test]
fn the_menu_asks_for_the_servers_then_the_chosen_servers_options() {
    let mut fixture = Fixture::new();
    let mut screen = menu();
    let (asked, _) = fixture.run(&mut screen, ScreenEvent::Mount);
    assert_eq!(asked, [Slot::Servers(MediaKind::Tv), Slot::ServerOptions(MediaKind::Tv, 1)]);
    assert_eq!(screen.form.selected_id(), Some(&RowId::Send), "the menu opens on Send");
    assert!(screen.form.index_of(&RowId::Folder(1)).is_some(), "the default server's folders are rows");

    let server = elem(&screen, RowId::Server(1));
    let (asked, _) = fixture.run(&mut screen, ScreenEvent::Activate(server));
    assert_eq!(asked, [Slot::ServerOptions(MediaKind::Tv, 2)]);
    assert_eq!(screen.choices, Choices { server: Some(2), ..Default::default() });
}

/// Send asks for the seasons still missing, naming only what was changed, and closes the menu; a
/// choice keeps it open.
#[test]
fn send_requests_the_missing_seasons_with_what_was_changed_and_closes() {
    let mut fixture = Fixture::new();
    let mut screen = menu();
    fixture.run(&mut screen, ScreenEvent::Mount);
    let profile = elem(&screen, RowId::Profile(0));
    let (_, out) = fixture.run(&mut screen, ScreenEvent::Activate(profile));
    assert!(!out.iter().any(|s| matches!(s.fx, Fx::Nav(NavOp::Dismiss(_)))), "a choice keeps the menu open");
    let send = elem(&screen, RowId::Send);
    let (asked, out) = fixture.run(&mut screen, ScreenEvent::Activate(send));
    assert!(out.iter().any(|s| matches!(s.fx, Fx::Nav(NavOp::Dismiss(ENTRY)))), "Send closes the menu");
    assert_eq!(asked, [Slot::Request(MediaKind::Tv, TMDB)]);
    assert_eq!(*fixture.script.sent.lock().unwrap(), [SeerrOp::Request { kind: MediaKind::Tv, tmdb: TMDB, seasons: vec![2],
        options: RequestOptions { profile: Some(1), ..Default::default() } }]);
}

/// The menu's own words (its section headings, notes and the free-space line) fit the panel it is
/// drawn in, in every shipped language. Server names, profile names and paths are server text.
#[test]
fn the_menus_own_words_fit_in_every_language() {
    use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
    let servers = servers();
    let opts = options(1);
    let mut out = Vec::new();
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        for (servers, options) in [(Load::Ready(&servers), Load::Ready(&opts)), (Load::Loading, Load::Loading),
            (Load::Failed, Load::Loading), (Load::Ready(&servers), Load::Failed)]
        {
            let chosen = effective(&servers_or_none(servers), false, &Choices::default()).server.cloned();
            let chosen = Effective { server: chosen.as_ref(), profile: Some(4), folder: Some("/tv".into()) };
            let mut table: FormTable<RowId, Act, Infallible> = FormTable::new(plx_ui::table_screen::BAND_BASE);
            table.open(form(msg::seerr_title_request(), servers, options, &chosen), None);
            // the panel's own width, as `SeerrRequestScreen::frame` sizes it
            let width = table.table.measured_width(&plx_base::fontcov::advances::ShippedMeasure)
                .clamp(plx_ui::table::MENU_MIN_W, plx_ui::table::MENU_MAX_W);
            out.extend(table.table.app_fit_failures(width, language.tag()));
        }
    }
    plx_ui::table::assert_no_fit_failures(&out);
}

fn servers_or_none(load: Load<&Vec<ArrServer>>) -> Vec<ArrServer> {
    match load {
        Load::Ready(s) => s.clone(),
        _ => Vec::new(),
    }
}
