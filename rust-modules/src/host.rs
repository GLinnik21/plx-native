//! The cross-crate construction interface: what a platform crate needs to build its `static` [`Platform`]
//! and hand it to the application (`docs/platform-ports.md`, Target shape 8).
//!
//! The application's modules are private (`mod app;`), so a crate that is a sibling of this one cannot
//! name `app::Platform` or implement a crate-private `Shell`. This is the one narrow public face: re-exports
//! of the types a platform builds or implements, an [`EventSink`] through which it reports input, and
//! [`run`], the one function in here (it hands the platform to the application's entry). No other module of the
//! application is public to a television build. A `hostsim` build also keeps the simulator binary's own doors,
//! each behind `feature = "hostsim"` in `lib.rs` (`grep -n 'feature = "hostsim"' rust-modules/src/lib.rs`):
//! `platform_desktop::run`, `hud_cli`, `sim_runtime_dir`, `sim_events_log`, `publish_sim_webos_major` and
//! `synthetic_home_initial`, the one `app` item re-exported at the crate root. `HostEvent`, the loop's own event type, stays
//! private: a platform reports input through [`EventSink`] and the application converts it. The owner's ruling (2026-11-21, Decision 3
//! amended): the eight application-crate items `Platform`, `Shell`, `Surface`, `FrameProbe`, `MediaBoot`,
//! `Composition`, `Exit` and `Release` are reachable only through this module. `Services` is not one of the eight:
//! it is `plx_os::tv::Services`, already a `pub` type of a lower crate, which this module re-exports so that a
//! platform crate names `host` alone.
//!
//! The split is by thread. A [`Platform`] is registration data in a `static`: function pointers and
//! `&'static dyn` of `Sync` types, [`Shell`] included as a factory. The thread-affine state (the window,
//! the GL context, a toolkit's handles) lives only in the [`Surface`] that [`Shell::open`] returns on the
//! loop thread and that the application owns from then on.
//!
//! Beside the platform's own types, the vocabulary their signatures name is re-exported from the lower
//! layers it lives in ([`MainThread`], [`Action`], [`Edge`], [`RawKey`], [`Lifecycle`], [`PointerEdge`],
//! [`OpenAsk`], [`Placement`]), so a platform crate names the application's own types through `host` alone.
//! The two SDL helpers (`sdl_init`, `sdl_open`) are not here: W7b publishes what the two real platforms need.
use std::ffi::CStr;
use std::os::raw::c_int;

pub use crate::app::platform::{Composition, EventSink, FrameProbe, MediaBoot, Platform, Shell, Surface};
pub use crate::app::{Exit, Release};
pub use plx_base::task::MainThread;
pub use plx_machine::machine::Edge;
pub use plx_os::tv::Services;
pub use plx_shell_sdl::event::{Lifecycle, PointerEdge};
pub use plx_shell_sdl::{OpenAsk, Placement};
pub use plx_ui::consts::{Action, RawKey};

/// Boot the application on `platform` and return the status it ended with: the application's entry
/// (`run_application`), which installs the platform's services, brings the window up through its
/// [`Shell`], runs the loop and tears down. Ending the process is the caller's, as for the television's
/// and the desktop's own entries: a process that hosts the application never does.
///
/// `pms_host` and `pms_port` are the pair `plex_run` hands the entry and `platform_desktop::run` takes.
pub fn run(platform: &'static Platform, pms_host: &CStr, pms_port: u16) -> Exit {
    crate::app::run_application(platform, pms_host.as_ptr(), c_int::from(pms_port))
}
