//! Real menu input -> emitted bridge queue -> production drain/performer -> production launch
//! policy -> player mount -> shared exit decision. Only resource acceptance and metadata I/O
//! are substituted. The fixture has no origin, navigation, or Bridge capability.

use super::super::{
    bridge::{self, AppHost, Bridge},
    content, playback,
};
use crate::plex::ServerId;
use crate::screens::registry::{AppArg, ContentArg, ItemMenuKind};
use crate::ui::dispatch::Dispatcher;
use crate::ui::machine::Key;
use crate::ui::screen::ScreenArg;

const SID: ServerId = ServerId::from_raw(0);

#[derive(Debug, PartialEq, Eq)]
enum ResourceCall {
    Movie {
        sid: ServerId,
        rk: String,
        part: String,
        resume_ms: i64,
    },
    Episode(String),
    Describe(ServerId, String),
    Start(i64),
}

struct Resources {
    calls: Vec<ResourceCall>,
    accept_request: bool,
    accept_start: bool,
}

impl playback::PlaybackResources for Resources {
    fn request_movie(
        &mut self,
        _: &mut crate::route::PlaybackSession,
        item: &crate::pms::PmsMovie,
    ) -> bool {
        self.calls.push(ResourceCall::Movie {
            sid: item.sid,
            rk: item.rk.clone(),
            part: item.part.clone(),
            resume_ms: item.resume_ms,
        });
        self.accept_request
    }
    fn request_episode(&mut self, _: &mut crate::route::PlaybackSession, rk: &str) -> bool {
        self.calls.push(ResourceCall::Episode(rk.into()));
        self.accept_request
    }
    fn describe_movie(&mut self, sid: ServerId, rk: &str) {
        self.calls.push(ResourceCall::Describe(sid, rk.into()));
    }
    fn prepare_start(
        &mut self,
        _: &mut crate::route::PlaybackSession,
        _: &mut crate::player::adapter::PlayerAdapter,
        resume_ns: i64,
    ) -> bool {
        self.calls.push(ResourceCall::Start(resume_ns));
        self.accept_start
    }
}

fn frame(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, now: &mut u32, key: Option<Key>) {
    *now += 16;
    let tick = crate::ui::fixture::tick(*now);
    bridge::frame(
        d,
        rig,
        tick,
        key.map(|k| bridge::script_key(k, tick)).unwrap_or_default(),
    );
}

fn settle(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, now: &mut u32) {
    // Include the modal's damped closing spring, which outlasts the page dip.
    for _ in 0..120 {
        frame(d, rig, now, None);
    }
}

fn detail() -> AppArg {
    AppArg::Content(ContentArg::Detail {
        sid: SID,
        rk: "show-7".into(),
    })
}

fn person() -> AppArg {
    AppArg::Content(ContentArg::Person {
        sid: SID,
        key: "person-9".into(),
        guid: String::new(),
        name: String::new(),
        thumb: String::new(),
    })
}

fn chain(host: AppArg, episode: bool, accept_request: bool, accept_start: bool) {
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    let mut now = 0;
    bridge::show_page(&mut d, host.clone());
    settle(&mut d, &mut rig, &mut now);
    let entry = d.nav.top_page().unwrap().id;
    let instance = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    // The played item differs from the host's identity, including Related on Detail.
    let row = crate::pms::PmsMovie {
        sid: SID,
        rk: "played-3".into(),
        part: "/library/parts/3/file.mkv".into(),
        resume_ms: 30_000,
        dur_ns: 600_000_000_000,
        ..Default::default()
    };
    let mut arg = bridge::card_menu_arg(
        &row,
        false,
        matches!(host, AppArg::Home),
        entry,
        d.focus(),
        None,
    );
    if episode {
        // Seed the filmstrip menu's captured subject; activation and request flags come from the
        // real ItemMenuScreen. The live metadata lookup itself is the resource under substitution.
        arg.kind = ItemMenuKind::Episode {
            mark: crate::ui::widgets::PosterMark::None,
        };
        arg.loaded_episode = true;
    }
    bridge::open_item_menu(&mut d, arg);
    settle(&mut d, &mut rig, &mut now);
    assert!(bridge::item_menu_up(&d));
    let menu = d.nav.modals.top().unwrap().entry.id;
    assert_eq!(d.nav.top_page().unwrap().id, entry);
    // Partial movie: Go to Movie / separator / Watched / Unwatched / Play from Start.
    // Filmstrip: Watched / Play from Start.
    for _ in 0..if episode { 1 } else { 3 } {
        frame(&mut d, &mut rig, &mut now, Some(Key::Down));
    }
    assert_eq!(
        d.focus().map(|f| (f.entry, f.elem)),
        Some((menu, if episode { 1 } else { 4 }))
    );
    frame(&mut d, &mut rig, &mut now, Some(Key::Ok));
    assert_eq!(
        d.nav.modals.top().unwrap().phase,
        crate::ui::containers::modal::Phase::Closing
    );

    let mut ps = crate::route::PlaybackSession::default();
    let mut pa =
        crate::player::adapter::PlayerAdapter::new(unsafe { crate::task::MainThread::assume() });
    let mut resources = Resources {
        calls: Vec::new(),
        accept_request,
        accept_start,
    };
    content::drain_item_menu_requests(&mut ps, &mut pa, &mut d, &mut rig, &mut resources);
    let mut expected = if episode {
        vec![ResourceCall::Episode(row.rk.clone())]
    } else {
        vec![ResourceCall::Movie {
            sid: SID,
            rk: row.rk.clone(),
            part: row.part.clone(),
            resume_ms: 30_000,
        }]
    };
    if accept_request {
        if !episode {
            expected.push(ResourceCall::Describe(SID, row.rk.clone()));
        }
        expected.push(ResourceCall::Start(0)); // real PlayFromStart policy drops the captured resume
    }
    assert_eq!(resources.calls, expected);
    content::drain_item_menu_requests(&mut ps, &mut pa, &mut d, &mut rig, &mut resources);
    assert_eq!(
        resources.calls, expected,
        "one activation is drained exactly once"
    );
    settle(&mut d, &mut rig, &mut now);
    if accept_request && accept_start {
        assert!(matches!(d.top_arg(), Some(AppArg::Player)));
        let origin = bridge::player(&d)
            .unwrap()
            .origin
            .expect("production launch must capture its host");
        assert_eq!(origin.entry, entry);
        assert_eq!(origin.instance, Some(instance));
        playback::return_from_player(&mut d);
        settle(&mut d, &mut rig, &mut now);
    }
    assert_eq!(d.nav.top_page().unwrap().id, entry);
    assert_eq!(
        d.nav.top_page().unwrap().inst.as_ref().unwrap().id,
        instance
    );
    assert!(d.top_arg().unwrap().same_instance(&host));
    assert!(
        !bridge::item_menu_up(&d),
        "the activated menu must not reopen on return"
    );
}

#[test]
fn card_menu_activation_returns_to_all_hosts_through_production_launch() {
    let _serial = crate::testlock::serial();
    let _session = crate::plex::session::TempSession::new("menu-card-return");
    crate::plex::reset_servers_for_test();
    // Non-Home first: Home alone can hide a missing origin.
    for host in [
        AppArg::Library,
        AppArg::Search,
        person(),
        detail(),
        AppArg::Home,
    ] {
        chain(host, false, true, true);
    }
}

#[test]
fn filmstrip_menu_activation_returns_to_detail_through_production_launch() {
    let _serial = crate::testlock::serial();
    let _session = crate::plex::session::TempSession::new("menu-filmstrip-return");
    crate::plex::reset_servers_for_test();
    chain(detail(), true, true, true);
}

#[test]
fn refused_menu_resources_leave_the_retained_host_and_do_not_repeat_work() {
    let _serial = crate::testlock::serial();
    let _session = crate::plex::session::TempSession::new("menu-refused-return");
    crate::plex::reset_servers_for_test();
    for episode in [false, true] {
        chain(detail(), episode, false, true);
        chain(detail(), episode, true, false);
    }
}
