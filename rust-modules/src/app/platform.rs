//! The platform the application runs on, as only the loop reads it.
//!
//! [`Platform`] is what the platform's entry installs once ([`install`]) before the loop starts. It
//! carries three things, split by who reads them:
//!
//! - `services`: [`plx_os::tv::Services`], what the LOWER layers read through the private `tv`
//!   accessors (identity, the secure store, Home and the toast, the legacy webOS media verbs). It
//!   is installed into `tv` by [`install`] and read from nowhere in the app but there.
//! - `shell`: the window and the bus, [`Shell`]. Its one consumer is the loop (`app::boot` brings
//!   the window up, `app::run` drives it every frame), so it is not in the bottom layer's process
//!   global.
//! - `diag`: the six frame probes, [`FrameProbe`], read by the same loop and by the one clear hook
//!   `app::boot` hands the renderer.
//!
//! `Shell`, `FrameProbe` and `Platform` are app-private on purpose: no type a platform's shell has
//! to match leaves the application crate until the platform contract is settled. The webOS values
//! are built by `port.rs`; `ci/check-deps.sh`'s `frame` gate pins the call text of the three
//! privileged calls (`pump_bus`, `opaque_route`, `clear_opaque_region`) to `app/run.rs`.
use plx_os::tv::Services;
use std::os::raw::c_void;
use std::sync::OnceLock;

/// The platform's window and bus, as the frame loop drives them: the SDL window and its surface, the
/// opaque-region hint, and the platform bus pump. Every call runs on the loop thread, and every
/// hook is required: a platform with nothing to do states [`Platform::HOST_TEST`]'s no-ops.
pub(crate) trait Shell: Sync {
    /// Hand the platform the SDL window, once, at boot: the window is created deep inside boot and
    /// the platform needs it a long way from there (`textinput::bind`'s shape and its reason).
    fn bind_window(&self, win: *mut c_void);
    /// Take the surface the window presents through.
    fn grab_surface(&self, win: *mut c_void);
    /// Let go of the surface, without destroying SDL's objects; foreground takes it again. Answers
    /// whether a surface was held, so the caller can tell a release from a second, idempotent one.
    fn release_surface(&self) -> bool;
    /// Arm the opaque-region hint at boot.
    fn arm_opaque_region(&self);
    /// Route the opaque region for the frame about to present: `player` is whether the video plane
    /// shows through.
    fn opaque_route(&self, player: bool);
    /// Drop the opaque-region hint.
    fn clear_opaque_region(&self);
    /// Service the platform bus once; the frame loop calls this every frame.
    fn pump_bus(&self);
}

/// The dev frame probe (`/tmp/plxnative-framecb`): the platform's presentation-callback clock, read
/// by the loop and by the renderer's clear hook. Unarmed, every call is a latched flag read.
pub(crate) trait FrameProbe: Sync {
    /// Ask for this frame's presentation callback. Call on a PRESENTING frame, before the swap.
    fn request(&self);
    /// `prepare_window` is next: starts the frame's `p` stretch.
    fn prepare_begin(&self);
    /// `prepare_window` has returned: closes the frame's `p` stretch.
    fn prepare_end(&self);
    /// The frame's first framebuffer command is next: the wait for a back buffer starts here.
    fn waiting(&self);
    /// The frame's first framebuffer command has returned: the back buffer is acquired.
    fn acquired(&self);
    /// The probe's log fields since the previous line; `""` unarmed.
    fn fields(&self) -> String;
}

/// Everything the application needs from its platform. Built once by the platform's entry
/// (`port.rs` for webOS) and installed before `run_application`.
pub(crate) struct Platform {
    pub(crate) services: &'static Services,
    pub(crate) shell: &'static dyn Shell,
    pub(crate) diag: &'static dyn FrameProbe,
}

/// The shell of a process with no platform: a host test, where nothing owns a window or a bus.
struct HostShell;

impl Shell for HostShell {
    fn bind_window(&self, _win: *mut c_void) {}
    fn grab_surface(&self, _win: *mut c_void) {}
    fn release_surface(&self) -> bool {
        false
    }
    fn arm_opaque_region(&self) {}
    fn opaque_route(&self, _player: bool) {}
    fn clear_opaque_region(&self) {}
    fn pump_bus(&self) {}
}

/// The frame probe of a process with no platform: always unarmed.
struct HostProbe;

impl FrameProbe for HostProbe {
    fn request(&self) {}
    fn prepare_begin(&self) {}
    fn prepare_end(&self) {}
    fn waiting(&self) {}
    fn acquired(&self) {}
    fn fields(&self) -> String {
        String::new()
    }
}

impl Platform {
    /// No platform installed: the loop's host tests, where nothing calls `plex_run`. Every entry is
    /// what the no-port defaults answered before the shell and the probe left `tv`.
    pub(crate) const HOST_TEST: Platform =
        Platform { services: &Services::HOST_TEST, shell: &HostShell, diag: &HostProbe };
}

static INSTALLED: OnceLock<&'static Platform> = OnceLock::new();

/// Once, first thing the process does: install the platform's `tv` services for the lower layers and
/// keep the platform for the loop. A second call is refused and changes nothing.
pub(crate) fn install(platform: &'static Platform) {
    let _ = plx_os::tv::install(platform.services);
    let _ = INSTALLED.set(platform);
}

/// The installed platform, or [`Platform::HOST_TEST`] when none was (a host test, where every
/// entry is a no-op). The loop reads its shell and its frame probe through this.
pub(crate) fn platform() -> &'static Platform {
    INSTALLED.get().copied().unwrap_or(&Platform::HOST_TEST)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Every answer of the host-test platform, pinned.** The destructuring names each field of
    /// the platform and has no `..`, so a field added to it breaks this test until it is pinned.
    /// The shell and the probe are driven through plain locals (`s`, `p`): the `frame` gate pins the
    /// call text of the loop's own receiver, and this is the no-op one.
    #[test]
    fn the_host_test_platform_answers_nothing_and_says_so() {
        let Platform { services, shell: s, diag: p } = Platform::HOST_TEST;

        assert!(
            matches!((services.identity.system_locale)(), plx_os::tv::LocaleReply::NoPlatform),
            "the services are the host-test services (`tv`'s own tests pin each answer)"
        );

        let window = std::ptr::null_mut();
        s.bind_window(window);
        s.grab_surface(window);
        assert!(!s.release_surface(), "no surface was ever held");
        s.arm_opaque_region();
        s.opaque_route(true);
        s.opaque_route(false);
        s.clear_opaque_region();
        s.pump_bus();

        p.request();
        p.prepare_begin();
        p.prepare_end();
        p.waiting();
        p.acquired();
        assert_eq!(p.fields(), "", "an unarmed probe has nothing to log");
    }

    /// Nothing is installed in a test process, so the loop reads the host-test platform.
    #[test]
    fn an_uninstalled_process_reads_the_host_test_platform() {
        assert!(INSTALLED.get().is_none(), "no test installs a platform; the platform's own entry does");
        let Platform { shell: s, diag: p, .. } = *platform();
        s.pump_bus();
        assert_eq!(p.fields(), "");
    }
}
