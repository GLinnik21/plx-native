//! The application's playback-backend composition at boot: the two decisions that belong to the
//! composition root and to no backend. Media names neither backend's presence in policy, and the
//! plane's boot is not a Feed step.
//!
//!   - [`acb_init`] boots the video plane owner (`plx_platform::tv::plane`) once per process, for
//!     EVERY backend set. It used to live in the Feed engine, so a URI-only build never created the
//!     ACB handle, and a webOS 4.x television then stalled URI HLS for want of a bound plane.
//!   - [`compose_backends`] installs the product URI driver and then tells the route session which
//!     backends the installed drivers form, so policy offers what is physically there instead of
//!     what Cargo features happened to be on.
use plx_base::eventlog::log;
use plx_base::task::MainThread;
use plx_media::player::adapter::PlayerAdapter;
use plx_media::route::PlaybackSession;
use plx_platform::tv::plane::{self, PlaneFacts, PlaneOwner, DEFAULT_PLAYER_TYPE};
use plx_platform::tv::sink::{self, DisplaySink, VP_EXPORTED, VP_NONE};
use std::os::raw::c_int;

/// Bind the decoded video sink to the display plane, whichever way this television does it.
///
/// **Boot-scoped.** Called once, from the boot, and never again — which is why the webOS 5
/// exported window is NOT created here: that one is created per session in `start_bufferfeed`,
/// beside the payload it has to be spliced into, and destroyed by the matching `teardown`.
///
///   - `VP_ACB` — create + initialize the ACB. We deliberately DON'T register our own
///     com.webos.media client; it collides with the pipeline's uMS connection. The handle lives
///     for the process; what teardown calls is `acb_unload`, a session-scoped state change.
///   - `VP_EXPORTED` — nothing to do at boot. There is no handle, and no state mirroring:
///     webOS 5 deleted that sequence outright rather than replacing it.
///
/// It reads the installed display sink and plane owner, the same objects the Feed engine's old
/// boot resolved through `player::display()` and `player::plane()` (no off-frame `Unload` can own
/// the native object before the first boot, so the fence arm of `display()` never applied here).
pub(crate) fn acb_init(mt: &MainThread) {
    acb_init_with(mt, sink::display_installed(), plane::installed());
}

/// [`acb_init`] over the display sink and plane owner the caller names: the boot passes the
/// installed pair, a test passes a scripted sink and its own owner.
fn acb_init_with(mt: &MainThread, display: &dyn DisplaySink, owner: &PlaneOwner) {
    let facts = match display.window_mode() {
        VP_EXPORTED => {
            log("vplane: exported window (webOS 5+) — created per session");
            // Nothing to bind later; the ACB stages are skipped by the plane having no handle.
            PlaneFacts::ABSENT
        }
        VP_NONE => {
            log(
                "vplane: this device has NEITHER libAcbAPI nor SDL's exported window — audio \
                 will play and the picture will not appear. Please report your webOS version.",
            );
            PlaneFacts::ABSENT
        }
        _ => acb_init_acb(mt, display),
    };
    owner.boot(facts);
}

/// The webOS 4.x half of [`acb_init_with`].
fn acb_init_acb(mt: &MainThread, display: &dyn DisplaySink) -> PlaneFacts {
    let mut pt = DEFAULT_PLAYER_TYPE;
    if let Some(s) = plx_base::devtrig::read("ptype") {
        if let Ok(p) = s.parse::<c_int>() {
            pt = p;
        }
    }
    log(&format!("ptype={pt}"));
    // Which app ACB is being initialized for. The third argument of `AcbAPI_initialize` is an app
    // id — that much is certain, and both reference implementations pass one (Kodi's webOS port
    // passes `getenv("APPID")`; see docs/distribution.md). What ACB does with it has not been
    // decompiled here, so the rule is simply that it must be the install SAM actually launched —
    // and with a developer build able to sit beside the shipped one, "the id compiled in" and
    // "the id we were launched as" are no longer the same question.
    //
    // It used to be `env::var("APPID")` with a NULL fallback, which `starfish.c` then turned into
    // the shipped app's literal id. That is a double fallback whose failure is invisible: on any
    // launch where SAM did not export APPID, a developer install would announce itself as the
    // shipped app. `paths::app_id` reads the install directory instead, which is the id webOS
    // registered by definition. The environment is still logged, one line at boot, as the
    // independent witness for whether SAM sets it at all on this firmware.
    let app_c = std::ffi::CString::new(plx_base::paths::app_id()).ok();
    let app_ptr = app_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    let acb = unsafe { display.plane_create(mt, app_ptr, pt) };
    log(&format!("acb create={acb}"));
    PlaneFacts { handle: acb != 0, player_type: pt }
}

/// Compose the backends this boot supplies. A normal boot installs the product URI driver first
/// (a refused install leaves URI out of the set); a controlled run (replay, scripted Home) touches
/// no native port and so installs none. Then the session is told the set the installed drivers
/// form.
pub(crate) fn compose_backends(adapter: &mut PlayerAdapter, session: &mut PlaybackSession,
    controlled: bool) {
    #[cfg(feature = "play-uri")]
    if !controlled && !adapter.install_uri_product(session) {
        log("uri: product driver installation refused");
    }
    #[cfg(not(feature = "play-uri"))]
    let _ = controlled;
    session.install_backends(adapter.composed_backends());
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_base::task::MainThread;
    use plx_platform::tv::sink::{NoSink, VP_ACB};
    use std::os::raw::{c_char, c_long};
    use std::sync::Mutex;

    /// A display sink whose window mode and ACB create answer are scripted, and which records
    /// what `plane_create` was asked for. Every other verb is `NoSink`'s refusal.
    struct BootSink { mode: c_int, create: c_long, asked: Mutex<Vec<(String, c_int)>> }
    impl BootSink {
        fn new(mode: c_int, create: c_long) -> Self {
            Self { mode, create, asked: Mutex::new(Vec::new()) }
        }
    }
    impl DisplaySink for BootSink {
        fn window_mode(&self) -> c_int { self.mode }
        fn window_id(&self) -> &'static std::ffi::CStr { c"" }
        unsafe fn create_window(&self, mt: &MainThread) -> *const c_char { NoSink.create_window(mt) }
        unsafe fn place_window(&self, mt: &MainThread, a: c_int, b: c_int, c: c_int, d: c_int,
            e: c_int, f: c_int) -> c_int { NoSink.place_window(mt, a, b, c, d, e, f) }
        unsafe fn destroy_window(&self, mt: &MainThread) { NoSink.destroy_window(mt) }
        unsafe fn plane_create(&self, _: &MainThread, app_id: *const c_char, player_type: c_int)
            -> c_long {
            let id = std::ffi::CStr::from_ptr(app_id).to_string_lossy().into_owned();
            self.asked.lock().unwrap().push((id, player_type));
            self.create
        }
        unsafe fn plane_initialized(&self, mt: &MainThread) -> bool { NoSink.plane_initialized(mt) }
        #[cfg(feature = "devtriggers")]
        unsafe fn plane_probe_dass_available(&self, mt: &MainThread) -> bool {
            NoSink.plane_probe_dass_available(mt)
        }
        unsafe fn plane_media_id(&self, mt: &MainThread, id: *const c_char) -> c_int {
            NoSink.plane_media_id(mt, id)
        }
        unsafe fn plane_loaded(&self, mt: &MainThread) -> c_int { NoSink.plane_loaded(mt) }
        #[cfg(feature = "devtriggers")]
        unsafe fn plane_probe_connect_dass(&self, mt: &MainThread) -> c_int {
            NoSink.plane_probe_connect_dass(mt)
        }
        unsafe fn plane_finish(&self, mt: &MainThread) -> c_int { NoSink.plane_finish(mt) }
        unsafe fn plane_bind(&self, mt: &MainThread, id: *const c_char) { NoSink.plane_bind(mt, id) }
        unsafe fn plane_send_video_data(&self, mt: &MainThread, info: *const c_char) -> c_int {
            NoSink.plane_send_video_data(mt, info)
        }
        unsafe fn plane_send_atmos(&self, mt: &MainThread, id: *const c_char) -> c_int {
            NoSink.plane_send_atmos(mt, id)
        }
        unsafe fn plane_start(&self, mt: &MainThread, x: c_long, y: c_long, w: c_long, h: c_long) {
            NoSink.plane_start(mt, x, y, w, h)
        }
        unsafe fn plane_unload(&self, mt: &MainThread) -> sink::PlaneUnload { NoSink.plane_unload(mt) }
        fn plane_completions(&self, mt: &MainThread) -> Vec<sink::PlaneCompletion> {
            NoSink.plane_completions(mt)
        }
        unsafe fn plane_pause(&self, mt: &MainThread) { NoSink.plane_pause(mt) }
        unsafe fn plane_resume(&self, mt: &MainThread) { NoSink.plane_resume(mt) }
    }

    /// Run the boot over a scripted sink on a private owner, then offer the owner a DIFFERENT boot:
    /// the owner takes its boot once, so a facts value that survives the offer was the first.
    fn booted(sink: &BootSink) -> PlaneOwner {
        let mt = unsafe { MainThread::assume() };
        let owner = PlaneOwner::new();
        acb_init_with(&mt, sink, &owner);
        owner.boot(PlaneFacts { handle: true, player_type: 99 });
        owner
    }

    #[test]
    fn an_exported_window_device_boots_absent_facts_and_never_creates_a_handle() {
        let sink = BootSink::new(VP_EXPORTED, 1);
        let owner = booted(&sink);
        assert_eq!(owner.facts(), PlaneFacts::ABSENT, "the boot ran, so the later offer was ignored");
        assert!(sink.asked.lock().unwrap().is_empty(), "webOS 5 creates its window per session");
    }

    #[test]
    fn a_device_with_no_plane_at_all_boots_absent_facts() {
        let sink = BootSink::new(VP_NONE, 1);
        let owner = booted(&sink);
        assert_eq!(owner.facts(), PlaneFacts::ABSENT);
        assert!(sink.asked.lock().unwrap().is_empty());
    }

    #[test]
    fn an_acb_device_creates_the_handle_for_this_install_and_records_it() {
        let sink = BootSink::new(VP_ACB, 7);
        let owner = booted(&sink);
        assert_eq!(owner.facts(), PlaneFacts { handle: true, player_type: DEFAULT_PLAYER_TYPE });
        let asked = sink.asked.lock().unwrap();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0], (plx_base::paths::app_id().to_owned(), DEFAULT_PLAYER_TYPE));
    }

    #[test]
    fn an_acb_device_whose_create_fails_boots_without_a_handle() {
        let sink = BootSink::new(VP_ACB, 0);
        let owner = booted(&sink);
        assert_eq!(owner.facts(), PlaneFacts { handle: false, player_type: DEFAULT_PLAYER_TYPE });
        assert_eq!(sink.asked.lock().unwrap().len(), 1);
    }

    #[test]
    #[cfg(feature = "play-uri")]
    fn the_session_set_gains_uri_only_when_the_product_driver_is_installed() {
        let mut adapter = PlayerAdapter::new(unsafe { MainThread::assume() });
        let mut session = PlaybackSession::IDLE;
        // A controlled run installs no driver: the set is what the drivers form, not what
        // Cargo features were on.
        compose_backends(&mut adapter, &mut session, true);
        assert!(!session.backends().contains(plx_playback_contract::BackendKind::Uri));
        compose_backends(&mut adapter, &mut session, false);
        assert!(session.backends().contains(plx_playback_contract::BackendKind::Uri),
            "an installed product driver is offered");
    }
}
