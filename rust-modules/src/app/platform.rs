//! The platform the application runs on, as only the loop reads it.
//!
//! [`Platform`] is the one value a platform's entry hands `run_application`, so an entry that forgot
//! it does not compile. It carries four things, split by who reads them:
//!
//! - `services`: [`plx_os::tv::Services`], what the LOWER layers read through the private `tv`
//!   accessors (identity, the secure store, Home and the toast, the legacy webOS media verbs).
//!   `run_application` installs it into `tv` once, and nothing else in the app reads it.
//! - `shell`: the window, the bus and the pointer's visibility, [`Shell`]. Its consumers are boot
//!   (`app::boot` brings the window up with the platform it was handed), the loop (`app::run` drives
//!   it every frame, through `App::platform`) and the bridge, which holds the shell for the
//!   dispatcher's two opaque-region hooks and lends it to the pointer's own callers (`input`,
//!   `playback`), which hide and show the Magic Remote's pointer through it.
//! - `diag`: the six frame probes, [`FrameProbe`], read by the loop. The renderer's clear hook takes
//!   plain functions, so the platform's entry installs it (`gfx::install_clear_probe`).
//! - `media`: [`MediaBoot`], the platform's two boot-time playback steps. `early_init` runs once before
//!   the first adapter exists; `compose` installs the platform's drivers on the adapter and hands
//!   back what it composed as a [`Composition`]. The boot then tells the route session what the
//!   platform composed through [`publish_composition`], the one function that does, so the set of
//!   installed drivers and the resolver the planner reads cannot disagree.
//!
//! `Shell`, `FrameProbe` and `Platform` are app-private on purpose: no type a platform's shell has
//! to match leaves the application crate until the platform contract is settled. The webOS value is
//! built by `port.rs`; `ci/check-deps.sh`'s `frame` gate pins the call text of the three
//! privileged calls (`pump_bus`, `opaque_route`, `clear_opaque_region`) to `app/run.rs`.
use plx_base::task::MainThread;
use plx_media::player::adapter::PlayerAdapter;
use plx_media::route::uri_preflight::ResolvePort;
use plx_media::route::PlaybackSession;
use plx_os::tv::Services;
use std::os::raw::c_void;

/// The platform's window, bus and pointer, as the application drives them: the SDL window and its
/// surface, the opaque-region hint, the platform bus pump, and the visibility of the Magic Remote's
/// pointer. Every call runs on the loop thread, and every hook is required: a platform with nothing
/// to do states a no-op, as the desktop and the test-only host platform do.
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
    /// Hide the remote's on-screen pointer: the D-pad took over, or the pointer idled over video.
    /// A platform whose pointer is not the application's to hide, a desktop's mouse, does nothing.
    fn hide_pointer(&self);
    /// Bring the pointer back after [`hide_pointer`](Shell::hide_pointer), on the next pointer motion.
    /// Where the hide was the platform's own call, the platform shows its pointer by itself and this
    /// does nothing.
    fn show_pointer_after_motion(&self);
    /// Whether [`hide_pointer`](Shell::hide_pointer) hides the pointer by swapping in a transparent
    /// cursor rather than by the platform's own call. Then the system's pointer is still up, and the
    /// remote's OK arrives as a click at wherever the invisible pointer sits, which the loop reads as
    /// the OK key (`input::click_is_ok`). `false` where a click is always a real one.
    fn hide_is_cursor_swap(&self) -> bool;
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

/// What a platform's composition installed that the route session also has to know: the resolver the
/// planner asks before it offers a URI source. `Some` only when the physical URI install succeeded,
/// so it is present exactly when the composed set holds a URI driver.
///
/// `Composition::default()` is a platform that composed no URI driver (the Feed or nothing): no resolver.
#[derive(Default)]
pub(crate) struct Composition {
    pub(crate) resolver: Option<ResolvePort>,
}

/// A platform's two playback steps at boot. Both are plain functions: the platform's entry states
/// them once, in its `static`, and the boot calls them through [`Platform::media`].
pub(crate) struct MediaBoot {
    /// Before the first adapter exists, once per process. The caller runs it only on a boot that
    /// touches the television (not a controlled one), so a platform's body never asks.
    pub(crate) early_init: fn(&MainThread),
    /// Install this platform's drivers on the adapter, and return what the session must also be told.
    /// `controlled` is a replay or scripted Home, which touches no native port and so installs no
    /// native driver. It takes no session and publishes nothing: [`publish_composition`] does that.
    pub(crate) compose: fn(&mut PlayerAdapter, controlled: bool) -> Composition,
}

/// Tell the route session what the platform composed: the resolver first, then the backend set the
/// adapter's installed drivers form, and log what it published in the one `compose:` line (the
/// television's boot log shows it). One call, every boot, for every platform: a platform that
/// composed nothing still publishes `BackendSet::NONE` (and no resolver), instead of leaving the
/// session on the Feed-only default, which would offer a backend nothing installed.
pub(crate) fn publish_composition(
    session: &mut PlaybackSession,
    adapter: &PlayerAdapter,
    composed: Composition,
) {
    let set = adapter.composed_backends();
    let named = match (set.uri, set.feed) {
        (true, true) => "uri+feed",
        (true, false) => "uri",
        (false, true) => "feed",
        (false, false) => "none",
    };
    plx_base::eventlog::log(&format!(
        "compose: backends={named} resolver={}",
        if composed.resolver.is_some() { "yes" } else { "no" }
    ));
    session.install_uri_preflight_port(composed.resolver);
    session.install_backends(set);
}

/// Everything the application needs from its platform. Built by the platform's entry (`port.rs` for
/// webOS) with [`Platform::new`] and passed to `run_application`, which keeps it as `App::platform`.
///
/// `services` is private to this module on purpose: its hooks are the raw verbs, and `tv`'s own
/// functions put their guards around them (`sound::output`'s blocking check, `secure`'s test store,
/// `home`'s request counter). The one thing the application does with it is
/// [`install_services`](Platform::install_services); nothing in `app` can call a hook directly.
pub(crate) struct Platform {
    services: &'static Services,
    pub(crate) shell: &'static dyn Shell,
    pub(crate) diag: &'static dyn FrameProbe,
    pub(crate) media: MediaBoot,
}

impl Platform {
    /// A platform's entry states its four parts once, in a `static`.
    pub(crate) const fn new(
        services: &'static Services,
        shell: &'static dyn Shell,
        diag: &'static dyn FrameProbe,
        media: MediaBoot,
    ) -> Self {
        Platform { services, shell, diag, media }
    }

    /// Hand the lower layers the platform's `tv` services, once: `run_application` does it before
    /// anything reads `tv`. A second call is refused by `tv` and changes nothing.
    pub(crate) fn install_services(&self) {
        let _ = plx_os::tv::install(self.services);
    }
}

/// The shell of a process with no platform: a host test, where nothing owns a window or a bus. Test
/// builds only, so no entry can boot the application on it.
#[cfg(test)]
struct HostShell;

#[cfg(test)]
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
    fn hide_pointer(&self) {}
    fn show_pointer_after_motion(&self) {}
    fn hide_is_cursor_swap(&self) -> bool {
        false
    }
}

/// The frame probe of a process with no platform: always unarmed. Test builds only.
#[cfg(test)]
struct HostProbe;

#[cfg(test)]
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

/// A shell whose hide is a cursor swap, counting what the application asks of it. Test builds only.
/// The swap is the webOS 3.x case, where the remote's OK arrives as a click (`input::click_is_ok`), and a
/// test of it needs a platform that says so, which [`Platform::HOST_TEST`] does not: it hides nothing.
#[cfg(test)]
pub(crate) struct SwapShell {
    pub(crate) hides: std::sync::atomic::AtomicUsize,
    pub(crate) shows: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl Shell for SwapShell {
    fn bind_window(&self, _win: *mut c_void) {}
    fn grab_surface(&self, _win: *mut c_void) {}
    fn release_surface(&self) -> bool {
        false
    }
    fn arm_opaque_region(&self) {}
    fn opaque_route(&self, _player: bool) {}
    fn clear_opaque_region(&self) {}
    fn pump_bus(&self) {}
    fn hide_pointer(&self) {
        self.hides.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    fn show_pointer_after_motion(&self) {
        self.shows.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    fn hide_is_cursor_swap(&self) -> bool {
        true
    }
}

#[cfg(test)]
impl SwapShell {
    /// A platform whose shell is a fresh `SwapShell`, and that shell, for a test to read the counts
    /// from. Leaked, because a `Platform` holds its parts for `'static`; every call makes its own
    /// counters, so the shell is no process global and no test shares a count with another.
    pub(crate) fn platform() -> (&'static Platform, &'static SwapShell) {
        let shell: &'static SwapShell = Box::leak(Box::new(SwapShell {
            hides: std::sync::atomic::AtomicUsize::new(0),
            shows: std::sync::atomic::AtomicUsize::new(0),
        }));
        let platform = Platform::new(&Services::HOST_TEST, shell, &HostProbe, MediaBoot::HOST_TEST);
        (Box::leak(Box::new(platform)), shell)
    }
}

#[cfg(test)]
impl MediaBoot {
    /// The media boot of a process with no platform: nothing to initialize and no driver to install.
    pub(crate) const HOST_TEST: MediaBoot = MediaBoot { early_init: host_test_early_init, compose: host_test_compose };
}

#[cfg(test)]
fn host_test_early_init(_mt: &MainThread) {}

#[cfg(test)]
fn host_test_compose(_adapter: &mut PlayerAdapter, _controlled: bool) -> Composition {
    Composition::default()
}

#[cfg(test)]
impl Platform {
    /// No platform: the loop's host tests, which build an `App` without an entry and say so by passing
    /// this to it. Every entry is what the no-port defaults answered before the shell and the probe
    /// left `tv`. It exists in test builds only: a production entry that passes it does not compile.
    pub(crate) const HOST_TEST: Platform =
        Platform::new(&Services::HOST_TEST, &HostShell, &HostProbe, MediaBoot::HOST_TEST);
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
        let Platform { services, shell: s, diag: p, media } = Platform::HOST_TEST;

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
        s.hide_pointer();
        s.show_pointer_after_motion();
        assert!(!s.hide_is_cursor_swap(), "no hide, so a click is always a real one");

        p.request();
        p.prepare_begin();
        p.prepare_end();
        p.waiting();
        p.acquired();
        assert_eq!(p.fields(), "", "an unarmed probe has nothing to log");

        // SAFETY: a host test owns the one thread it runs the media boot on.
        let mt = unsafe { MainThread::assume() };
        (media.early_init)(&mt);
        let mut adapter = PlayerAdapter::new(mt);
        let composed = (media.compose)(&mut adapter, false);
        assert!(composed.resolver.is_none(), "the host platform composes no URI driver");
        assert_eq!(adapter.composed_backends(), plx_playback_contract::BackendSet::NONE,
            "and installs no driver of any kind");
    }

    /// **The one publisher.** A platform that composed nothing still publishes: the session leaves
    /// its Feed-only default for `NONE`, and the resolver is the absence the compose returned.
    #[test]
    fn publishing_a_composition_that_installed_nothing_replaces_the_feed_default_with_none() {
        let mut session = PlaybackSession::IDLE;
        assert_eq!(session.backends(), plx_playback_contract::BackendSet::FEED,
            "the session starts on the Feed-only default");
        let adapter = PlayerAdapter::new(unsafe { MainThread::assume() });
        publish_composition(&mut session, &adapter, Composition::default());
        assert_eq!(session.backends(), plx_playback_contract::BackendSet::NONE);
        assert!(session.uri_preflight_port().is_none());
    }

    /// **The counting shell says what it is and counts what it is asked, per platform.** It is a
    /// cursor swap (the webOS 3.x answer), every `platform()` call has counters of its own, and the
    /// pointer calls are the only ones it counts. The loop's tests read these counts, so the shell they
    /// read is pinned here, where no feature gate hides it.
    #[test]
    fn the_swap_shell_is_a_cursor_swap_that_counts_its_own_pointer_calls() {
        use std::sync::atomic::Ordering::Relaxed;
        let (platform, counted) = SwapShell::platform();
        let (other, other_counted) = SwapShell::platform();
        assert!(platform.shell.hide_is_cursor_swap(), "a swap says so");
        platform.shell.hide_pointer();
        platform.shell.hide_pointer();
        platform.shell.show_pointer_after_motion();
        assert!(!platform.shell.release_surface(), "a shell call that is no pointer call is not counted");
        assert_eq!((counted.hides.load(Relaxed), counted.shows.load(Relaxed)), (2, 1));
        assert_eq!((other_counted.hides.load(Relaxed), other_counted.shows.load(Relaxed)), (0, 0),
            "another platform's counts are its own");
        assert!(!other.shell.release_surface(), "the rest of the shell answers as the host-test one does");
    }
}
