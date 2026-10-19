//! Shared driver helpers for bridge.rs's split test modules: frame/tick drivers, route
//! argument builders, and the directory-policy fixture used by the ViewState/chrome tests.

use super::*;

/// **Put `route` on top, then run one frame** — the TEST driver that replaced `sync_page`.
///
/// It is deliberately the same three-line decision `sync_page` made every frame in production
/// (reuse an entry that is already on the stack, otherwise root a peer or stack a page), kept
/// here so that ~240 existing assertions still read as "drive a frame on this screen". What
/// changed is WHO says it: production asks for the op it wants at the press, and no frame
/// derives a navigation from a second copy of the route.
pub(super) fn goto(d: &mut Dispatcher<AppHost>, want: AppArg) {
    if d.has_pending_navigation() { return; }
    super::show_page(d, want);
}

pub(super) fn frame(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, route: AppArg, tick: Tick, inputs: Vec<InputEvent<u32>>) -> (&'static str, FrameReport) {
    goto(d, route);
    presented(super::frame(d, rig, tick, inputs))
}

/// …and the same driver for the two frames that also want the effect tap or a supplied result
/// set. Only the `goto` is the test's: everything after it is production's own frame.
pub(super) fn frame_with_tap(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, route: AppArg, tick: Tick,
    inputs: Vec<InputEvent<u32>>, tap: &mut dyn plx_ui::dispatch::Tap<AppHost>)
    -> (&'static str, FrameReport) {
    goto(d, route);
    presented(super::frame_with_tap(d, rig, tick, inputs, tap))
}

pub(super) fn frame_with_results(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, route: AppArg, tick: Tick,
    inputs: Vec<InputEvent<u32>>, take: impl FnOnce() -> AppResults,
    tap: &mut dyn plx_ui::dispatch::Tap<AppHost>) -> (&'static str, FrameReport) {
    goto(d, route);
    presented(super::frame_with_results(d, rig, tick, inputs, take, tap))
}

/// A detail page's argument, for the tests that used to name `Route::Detail` and let a trail
/// supply the identity. The fold puts the identity ON the argument, which is what makes the
/// trail seeding this helper used to do unnecessary.
pub(super) fn detail_arg(rk: &str) -> AppArg {
    AppArg::Content(ContentArg::Detail { sid: plx_plex::plex::ServerId::UNSET, rk: rk.into() })
}

/// …and a person page's.
pub(super) fn person_arg(key: &str) -> AppArg {
    AppArg::Content(ContentArg::Person {
        sid: plx_plex::plex::ServerId::UNSET,
        key: key.into(),
        guid: format!("tag://{key}"),
        name: String::new(),
        thumb: String::new(),
    })
}


pub(super) fn tick(i: u32) -> Tick {
    Tick {
        ms: i * 16,
        dt_us: 16_000,
    }
}

pub(super) use super::super::words::every_route;

pub(super) fn notices(d: &Dispatcher<AppHost>) -> String {
    let mut s = String::new();
    if let Some(sc) = d.top_screen() {
        sc.state().probe(&mut s);
    }
    s
}

pub(super) fn directory_policy_fixture(
    own: plx_plex::plex::ServerId,
    hidden: plx_plex::plex::ServerId,
) -> plx_data::stores::browse::DirectorySnapshot {
    let section = |sid, key, section, title: &str, pinned| {
        plx_data::stores::browse::SectionView {
            sid: Some(sid),
            key,
            kind: plx_data::stores::browse::SecKind::Movie,
            row: plx_data::stores::browse::SrcRow {
                section,
                title: title.into(),
                pinned,
                current: section == 0,
                ..Default::default()
            },
        }
    };
    plx_data::stores::browse::DirectorySnapshot::fixture(41, 0, vec![
        section(own, 1, 0, "Retained Movies", true),
        section(hidden, 2, 1, "Hidden Movies", false),
    ])
}

pub(super) struct DirectoryPolicyCleanup;

impl Drop for DirectoryPolicyCleanup {
    fn drop(&mut self) {
        // Search and Hubs are now owned per-Bridge (`rig`, dropped with the test's own stack
        // frame), so there is no process-wide store state left for this cleanup to reset.
        plx_plex::plex::reset_servers_for_test();
    }
}

/// A test frame is a presenting frame: the product drains the Tracks/More text prewarm on the
/// presenting side of the present decision (`app::run::prepare_window`), after the frame's update.
fn presented<R>(frame: R) -> R {
    plx_ui::panel_motion::PanelMotion::drain_queued_text_for_test();
    frame
}

/// A one-thread loopback PMS for route tests: `respond` maps each request's path to a status line
/// and a JSON body. Dropping it stops and joins the thread, so a test that never dials still ends.
pub(crate) struct Pms {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub(crate) port: i32,
}

impl Pms {
    pub(crate) fn serve(respond: fn(&str) -> (&'static str, String)) -> Pms {
        use std::io::{Read, Write};
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port() as i32;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                let Ok((mut conn, _)) = plx_base::testnet::accept(&listener) else {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    continue;
                };
                let mut buf = [0u8; 4096];
                let n = conn.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]);
                let path = head.split_whitespace().nth(1).unwrap_or("");
                let (status, body) = respond(path.split('?').next().unwrap_or(""));
                let _ = write!(
                    conn,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
            }
        });
        Pms { stop, thread: Some(thread), port }
    }
}

impl Drop for Pms {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
