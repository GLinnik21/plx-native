//! A launch link (`app::deeplink`) driven through production frames against a one-thread loopback
//! PMS that knows one item: the link lands on that item's detail page with Home alone underneath,
//! unwinding whatever page the app was on; a key the server does not know leaves Home in place;
//! and a television with no account signed in keeps the link pending without touching the stack.

use super::*;
use super::test_support::{frame, tick, Pms};
use crate::app::deeplink::{self, DeepLink};
use crate::app::input;
use plx_plex::plex::ServerId;

const KNOWN: &str = "12345";

/// Knows one movie, `KNOWN`; every other path is a 404.
fn one_movie(path: &str) -> (&'static str, String) {
    if path == format!("/library/metadata/{KNOWN}") {
        ("200 OK", format!(r#"{{"MediaContainer":{{"size":1,"Metadata":[
            {{"ratingKey":"{KNOWN}","type":"movie","title":"Film","viewOffset":60000,"duration":5400000}}]}}}}"#))
    } else {
        ("404 Not Found", String::new())
    }
}

struct Rig {
    ps: plx_media::route::PlaybackSession,
    pa: plx_media::player::adapter::PlayerAdapter,
    d: Dispatcher<AppHost>,
    bridge: Bridge,
    link: DeepLink,
    wait: Option<input::MenuPlayAwait>,
    sid: ServerId,
    i: u32,
    _pms: Pms,
    _guard: plx_base::testlock::Serial,
}

impl Rig {
    /// Signed in to the loopback server, with `root` mounted as the stack's root.
    fn new(root: AppArg) -> Rig {
        let guard = plx_base::testlock::serial();
        assert!(plx_net::net::global_init() && plx_net::net::available() && plx_net::net::threaded_tls_ready());
        let mt = unsafe { plx_base::task::MainThread::assume() };
        let pms = Pms::serve(one_movie);
        let _ = plx_platform::tv::launch::take();
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("deeplink", "127.0.0.1", pms.port, "t", "c-deeplink");
        plx_plex::plex::client_for(sid).unwrap().set_link(plx_plex::plex::probe::Location::Local);
        assert!(plx_plex::plex::set_current(sid));
        let mut d = Dispatcher::<AppHost>::new();
        let mut bridge = Bridge::for_test(|| 0);
        frame(&mut d, &mut bridge, root, tick(0), vec![]);
        d.nav.tabs.stack.transition = Box::new(plx_ui::containers::transition::Immediate);
        Rig {
            ps: plx_media::route::PlaybackSession::default(),
            pa: plx_media::player::adapter::PlayerAdapter::new(mt),
            d,
            bridge,
            link: DeepLink::default(),
            wait: None,
            sid,
            i: 1,
            _pms: pms,
            _guard: guard,
        }
    }

    /// Production frames, the link and the menu-play wait ticked where `run.rs` ticks them (after
    /// the detail pump), until both settle or ~5 s pass (the bound is a count of 2 ms naps: no clock reads in `app/`).
    fn run(&mut self) {
        for _ in 0..2500 {
            self.once();
            if self.link.is_idle() && self.wait.is_none() && !self.d.has_pending_navigation() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("the link never settled");
    }

    /// `n` production frames of the link, for a test whose link is meant to keep waiting.
    fn frames(&mut self, n: u32) {
        for _ in 0..n {
            self.once();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// One production frame with the link and the menu-play wait ticked where `run.rs` ticks them.
    fn once(&mut self) {
        {
            // Under `cfg(test)` a detail fetch is parked until the test runs it; run it here, off
            // the frame, where the worker thread would have.
            self.bridge.metadata_mut().adapter_ref().run_held_detail_fetches_for_test();
            {
                let _frame = plx_base::task::FrameScope::enter();
                self.bridge.metadata_pump_detail();
                let now = tick(self.i).ms;
                deeplink::frame(&mut self.d, &mut self.link, &mut self.wait, true, false, now);
                unsafe {
                    input::menu_play_tick(&mut self.ps, &mut self.pa, &mut self.d, &mut self.bridge,
                        &mut self.wait, now);
                }
            }
            super::frame(&mut self.d, &mut self.bridge, tick(self.i), vec![]);
            self.i += 1;
        }
    }

    fn stack(&self) -> Vec<AppArg> {
        self.d.nav.tabs.stack.entries.iter().map(|e| e.arg.clone()).collect()
    }
}

fn payload(rk: &str) -> String {
    format!(r#"{{"event":"launch","parameters":{{"plex":{{"ratingKey":"{rk}","action":"open","unknown":1}}}}}}"#)
}

#[test]
fn a_link_lands_on_its_detail_page_with_home_alone_behind_it() {
    let mut rig = Rig::new(AppArg::Home);
    // Whatever stands above Home when the link is due is unwound: here, another item's page.
    open_detail(&mut rig.d, &mut rig.bridge, rig.sid, "other", None, None);
    frame(&mut rig.d, &mut rig.bridge, AppArg::Content(ContentArg::Detail { sid: rig.sid, rk: "other".into() }),
        tick(rig.i), vec![]);
    assert_eq!(rig.stack().len(), 2);

    // Arrives the way a relaunch does, mid-session, and is taken by the tick.
    plx_platform::tv::launch::offer(&payload(KNOWN));
    rig.run();
    assert!(rig.stack() == vec![
        AppArg::Home,
        AppArg::Content(ContentArg::Detail { sid: rig.sid, rk: KNOWN.into() }),
    ], "want [Home, Detail({KNOWN})], have {} entries", rig.stack().len());
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn a_key_the_server_does_not_know_leaves_home_in_place() {
    let mut rig = Rig::new(AppArg::Home);
    plx_platform::tv::launch::offer(&payload("999"));
    rig.run();
    assert!(rig.stack() == vec![AppArg::Home], "Home alone stays");
    plx_plex::plex::reset_servers_for_test();
}

/// Sign-in is never interrupted and no longer drops the link: it stays pending, and the stack is
/// left alone. Its two endings are the two `..._sign_in` tests below.
#[test]
fn a_link_with_no_account_waits_through_sign_in() {
    let mut rig = Rig::new(AppArg::Login);
    plx_platform::tv::launch::offer(&payload(KNOWN));
    rig.frames(60);
    assert!(rig.stack() == vec![AppArg::Login], "sign-in stays alone");
    assert!(!rig.link.is_idle(), "the link is still pending");
    plx_plex::plex::reset_servers_for_test();
}

/// The payload for `KNOWN` naming the rig's server by its machine identifier.
fn payload_naming(rig: &Rig) -> String {
    let machine = plx_plex::plex::client_for(rig.sid).unwrap().machine_id().to_owned();
    format!(r#"{{"event":"launch","parameters":{{"plex":{{"server":"{machine}","ratingKey":"{KNOWN}","action":"open"}}}}}}"#)
}

/// A consent question over Home is a surface the viewer has not answered: the link opens nothing
/// under it, and lands once the question is dismissed.
#[test]
fn a_link_waits_under_the_consent_sheet() {
    let mut rig = Rig::new(AppArg::Home);
    open_first_run_consent_at(&mut rig.d, 0);
    rig.frames(20);
    assert!(rig.d.surface_up(), "premise: the sheet is up");
    plx_platform::tv::launch::offer(&payload(KNOWN));
    rig.frames(60);
    assert!(rig.bridge.metadata_view().detail_request_status(rig.sid, KNOWN).is_none(),
        "the link asked for its item under the sheet");
    assert!(rig.stack() == vec![AppArg::Home], "the stack moved under the sheet");
    dismiss_surfaces(&mut rig.d);
    rig.run();
    assert!(rig.stack() == vec![
        AppArg::Home,
        AppArg::Content(ContentArg::Detail { sid: rig.sid, rk: KNOWN.into() }),
    ], "the link lands after the dismiss");
    plx_plex::plex::reset_servers_for_test();
}

/// The owner's rule: a link that names its server waits through sign-in and resolves against the
/// roster of the account that signed in.
#[test]
fn a_link_with_a_server_survives_sign_in() {
    let mut rig = Rig::new(AppArg::Login);
    plx_platform::tv::launch::offer(&payload_naming(&rig));
    rig.frames(20);
    nav_root(&mut rig.d, AppArg::Home);
    rig.run();
    assert!(rig.stack() == vec![
        AppArg::Home,
        AppArg::Content(ContentArg::Detail { sid: rig.sid, rk: KNOWN.into() }),
    ], "the link named the server and should have survived sign-in");
    plx_plex::plex::reset_servers_for_test();
}

/// A link naming no server means "the signed-in server"; one that waited across a sign-in cannot
/// know which account it was meant for, so it is dropped, and says so.
#[test]
fn a_serverless_link_that_waited_across_sign_in_is_dropped() {
    plx_base::eventlog::with_private_log(|| {
        let mut rig = Rig::new(AppArg::Login);
        plx_platform::tv::launch::offer(&payload(KNOWN));
        rig.frames(20);
        nav_root(&mut rig.d, AppArg::Home);
        rig.run();
        assert!(rig.stack() == vec![AppArg::Home], "Home stays");
        let log = std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default();
        assert!(log.contains("deeplink: dropped (signed in after the link arrived; it names no server)"),
            "log was: {log}");
        plx_plex::plex::reset_servers_for_test();
    });
}
