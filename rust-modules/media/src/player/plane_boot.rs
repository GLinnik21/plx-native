//! The backend-neutral boot of the ACB video plane, shared by every backend set: it creates the
//! handle once per process and hands what it learned to the plane owner
//! (`plx_platform::tv::plane`) as an owned value, where Feed's ACB stages and the URI probe's gate
//! read it. It used to live in the Feed engine behind `play-feed`, so a URI-only build never
//! created the handle.
use super::{display, log, plane};
use plx_base::task::MainThread;
use plx_platform::tv::plane::{PlaneFacts, DEFAULT_PLAYER_TYPE};
use std::os::raw::c_int;

/// Bind the decoded video sink to the display plane, whichever way this television does it.
///
/// **Boot-scoped.** Called once, from `plex_run`, and never again — which is why the webOS 5
/// exported window is NOT created here: that one is created per session in `start_bufferfeed`,
/// beside the payload it has to be spliced into, and destroyed by the matching `teardown`.
///
///   - `VP_ACB` — create + initialize the ACB. We deliberately DON'T register our own
///     com.webos.media client; it collides with the pipeline's uMS connection. The handle lives
///     for the process; what teardown calls is `acb_unload`, a session-scoped state change.
///   - `VP_EXPORTED` — nothing to do at boot. There is no handle, and no state mirroring:
///     webOS 5 deleted that sequence outright rather than replacing it.
pub fn acb_init(mt: &MainThread) {
    let facts = match display().window_mode() {
        super::VP_EXPORTED => {
            log("vplane: exported window (webOS 5+) — created per session");
            // Nothing to bind later; the ACB stages are skipped by the plane having no handle.
            PlaneFacts::ABSENT
        }
        super::VP_NONE => {
            log(
                "vplane: this device has NEITHER libAcbAPI nor SDL's exported window — audio \
                 will play and the picture will not appear. Please report your webOS version.",
            );
            PlaneFacts::ABSENT
        }
        _ => acb_init_acb(mt),
    };
    plane().boot(facts);
}

/// The webOS 4.x half of [`acb_init`].
fn acb_init_acb(mt: &MainThread) -> PlaneFacts {
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
    let acb = unsafe { display().plane_create(mt, app_ptr, pt) };
    log(&format!("acb create={acb}"));
    PlaneFacts { handle: acb != 0, player_type: pt }
}
