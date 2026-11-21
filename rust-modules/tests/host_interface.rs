//! W7a's gate: a platform built against the application crate through `plxnative_modules::host`, with the hook
//! tables of `plx_os::tv` and `plx_media`'s `PlayerAdapter` from their own crates (`docs/platform-ports.md`,
//! Target shape 8).
//!
//! This is an integration test, a separate crate that can name only the `pub` items of the application
//! crate, which is the point: a platform crate that is a sibling of the application has the same view. It
//! builds a `static PLATFORM` from a fake `Shell`, `Surface` and `FrameProbe` and boots it.
//!
//! **What "boots it" can mean here.** The real `run_application` runs, through [`host::run`], up to the
//! fake's `Shell::open`, which refuses: the boot ends with that `Exit`. It cannot go further, and no
//! fake could take it further: after `open` the boot makes real GL calls (`init_gl` links the renderer's
//! programs), which need a current GL context, and a surface that is not a real window has none. So this
//! test proves the construction interface and the boot order (the services installed, `init` before
//! `open`, `open` asked for the window the application means to ask for, the refusal returned as the
//! entry's `Exit`), not the loop. The loop's own fake-surface tests are in-crate (`make test-hostsim`).
//!
//! **The boot test needs `hostsim`** (the recipe `make test-host-interface` passes it): only there is the
//! runtime root steerable by `PLXNATIVE_RUNTIME_DIR`, so the boot's event log, crash log and diagnostics
//! land in a throwaway directory and not in a real install's. It refuses to run without that variable.
//! The rest of the file compiles in every configuration (`make lint` and `check-cargo-lint` build it),
//! which type-checks the interface from outside in the default non-hostsim feature set too.
//!
//! The application replaces the process's panic hook while it boots (`install_panic_logger`): it wraps
//! the previous hook, so a panic still reaches the test harness, and additionally writes one line to the
//! event log and the crash log in the runtime root. Only one test here boots, because the services are
//! installed once per process.
#![cfg_attr(not(feature = "hostsim"), allow(dead_code))]

use plxnative_modules::host::{
    Action, Composition, Edge, EventSink, Exit, FrameProbe, Lifecycle, MainThread, MediaBoot, OpenAsk, Placement,
    Platform, PointerEdge, RawKey, Services, Shell, Surface,
};
#[cfg(feature = "hostsim")]
use plxnative_modules::host::Release;
use std::ffi::c_void;
#[cfg(feature = "hostsim")]
use std::ffi::CString;
use std::sync::Mutex;

/// The refusal the fake `open` ends the boot with: a status no real refusal uses.
const REFUSAL: i32 = 73;

/// Fake services: the production tables' `NONE` groups, and one hook of its own, so the test can see that
/// the platform's table was the one installed.
static SERVICES: Services = Services {
    identity: plx_os::tv::IdentityHooks { system_locale: fake_locale, ..plx_os::tv::IdentityHooks::NONE },
    secure: plx_os::tv::SecureHooks::NONE,
    system: plx_os::tv::SystemHooks::NONE,
    storage: plx_os::tv::StorageHooks::NONE,
    legacy_webos_media: plx_os::tv::LegacyWebosMedia::NONE,
};

fn fake_locale() -> plx_os::tv::LocaleReply {
    plx_os::tv::LocaleReply::Reply("fake-host-locale".into())
}

/// A `Shell` of `Sync` state alone: an ordered call log and the window it was asked to open.
struct FakeShell {
    log: Mutex<Vec<&'static str>>,
    /// The window `open` was asked for: its title, placement and HiDPI flag.
    asked: Mutex<Option<(String, Placement, bool)>>,
}

impl FakeShell {
    fn note(&self, what: &'static str) {
        self.log.lock().unwrap().push(what);
    }
}

impl Shell for FakeShell {
    fn init(&self, _mt: &MainThread) -> Result<(), Exit> {
        self.note("init");
        Ok(())
    }
    fn open(&self, _mt: &MainThread, ask: &OpenAsk) -> Result<Box<dyn Surface>, Exit> {
        self.note("open");
        *self.asked.lock().unwrap() = Some((ask.title.to_string_lossy().into_owned(), ask.placement, ask.high_dpi));
        Err(Exit::refused(REFUSAL))
    }
    fn bind_window(&self, _win: *mut c_void) {
        self.note("bind_window");
    }
    fn grab_surface(&self, _win: *mut c_void) {
        self.note("grab_surface");
    }
    fn release_surface(&self) -> bool {
        self.note("release_surface");
        false
    }
    fn arm_opaque_region(&self) {
        self.note("arm_opaque_region");
    }
    fn opaque_route(&self, _player: bool) {
        self.note("opaque_route");
    }
    fn clear_opaque_region(&self) {
        self.note("clear_opaque_region");
    }
    fn pump_bus(&self) {
        self.note("pump_bus");
    }
    fn hide_pointer(&self) {
        self.note("hide_pointer");
    }
    fn show_pointer_after_motion(&self) {
        self.note("show_pointer_after_motion");
    }
    fn hide_is_cursor_swap(&self) -> bool {
        false
    }
}

static SHELL: FakeShell = FakeShell { log: Mutex::new(Vec::new()), asked: Mutex::new(None) };

struct FakeProbe;

impl FrameProbe for FakeProbe {
    fn request(&self) {}
    fn prepare_begin(&self) {}
    fn prepare_end(&self) {}
    fn waiting(&self) {}
    fn acquired(&self) {}
    fn fields(&self) -> String {
        String::new()
    }
}

/// A surface that is not SDL's: it has no queue of bytes (`poll` is left at its default) and delivers its
/// events through the sink. Thread-affine state is its own (`!Sync`: a `Cell`), as a toolkit's would be.
struct FakeSurface {
    frames: std::cell::Cell<u32>,
}

impl Surface for FakeSurface {
    fn window(&self) -> *mut c_void {
        std::ptr::null_mut()
    }
    fn swap(&self) {
        self.frames.set(self.frames.get() + 1);
    }
    fn set_swap_interval(&self, _interval: i32) {}
    fn deliver(&self, sink: &mut dyn EventSink) {
        sink.lifecycle(Lifecycle::DidForeground);
        sink.pointer(10.0, 20.0, PointerEdge::Move);
        sink.wheel(-1);
        sink.text("ok");
        assert!(sink.key(Some(Action::Ok), None, Edge::Down), "an action alone is a key");
        assert!(!sink.key(None, None, Edge::Down), "a key of neither action nor pair is refused");
        sink.quit();
    }
    unsafe fn quit(&self) {}
}

fn early_init(_mt: &MainThread) {}

fn compose(_adapter: &mut plx_media::player::adapter::PlayerAdapter, _controlled: bool) -> Composition {
    Composition::default()
}

/// The platform, as a sibling crate states it: a `static`, so every part of it is `Sync`.
static PLATFORM: Platform =
    Platform::new(&SERVICES, &SHELL, &FakeProbe, MediaBoot { early_init, compose });

fn assert_sync<T: Sync>(_: &T) {}

/// What the two real platform crates name from `host` beyond the construction types, type-checked here as a sibling crate
/// sees it (W7b): the SDL helpers, and the ready media values with the simulator's sink. Nothing runs; a name that stopped
/// being `pub`, or changed its shape, or lost its `cfg`, is a compile error.
const _: fn() = || {
    let _: unsafe fn(&MainThread, plxnative_modules::host::InitHooks) -> Result<(), Exit> = plxnative_modules::host::sdl_init;
    let _: fn(&MainThread, &OpenAsk) -> Result<Box<dyn Surface>, Exit> = plxnative_modules::host::sdl_open;
    let _: MediaBoot = plxnative_modules::host::WEBOS_MEDIA;
    #[cfg(feature = "hostsim")]
    let _: MediaBoot = plxnative_modules::host::FEED_MEDIA;
    #[cfg(all(feature = "hostsim", feature = "play-feed"))]
    let _ = &plxnative_modules::host::HOST_SINK;
};

/// The registration data is `Sync` (a `static` of it compiles), and the thread-affine surface is not
/// required to be: a `Surface` holding a `Cell` is accepted as `Box<dyn Surface>`.
#[test]
fn the_platform_is_registration_data_and_the_surface_may_be_thread_affine() {
    assert_sync(&PLATFORM);
    let surface: Box<dyn Surface> = Box::new(FakeSurface { frames: std::cell::Cell::new(0) });
    surface.swap();
    surface.set_swap_interval(1);
    assert!(surface.window().is_null());
    let mut ev = [0u8; 128];
    assert!(!surface.poll(&mut ev), "a surface with no SDL queue has nothing to poll");
}

/// What a fake surface delivers is what its `EventSink` receives, in order, from outside the crate: the
/// sink is implementable here with the public vocabulary alone.
#[test]
fn an_event_sink_is_implementable_from_outside_and_receives_a_surfaces_events_in_order() {
    #[derive(Default)]
    struct Recorder(Vec<String>);
    impl EventSink for Recorder {
        fn key(&mut self, action: Option<Action>, raw: Option<RawKey>, edge: Edge) -> bool {
            self.0.push(format!("key {action:?} {raw:?} {edge:?}"));
            action.is_some() || raw.is_some()
        }
        fn text(&mut self, text: &str) {
            self.0.push(format!("text {text}"));
        }
        fn pointer(&mut self, x: f32, y: f32, edge: PointerEdge) {
            self.0.push(format!("pointer {x} {y} {edge:?}"));
        }
        fn wheel(&mut self, dy: i32) {
            self.0.push(format!("wheel {dy}"));
        }
        fn lifecycle(&mut self, edge: Lifecycle) {
            self.0.push(format!("lifecycle {edge:?}"));
        }
        fn quit(&mut self) {
            self.0.push("quit".into());
        }
    }
    let surface = FakeSurface { frames: std::cell::Cell::new(0) };
    let mut sink = Recorder::default();
    surface.deliver(&mut sink);
    assert_eq!(
        sink.0,
        [
            "lifecycle DidForeground",
            "pointer 10 20 Move",
            "wheel -1",
            "text ok",
            "key Some(Ok) None Down",
            "key None None Down",
            "quit",
        ]
    );
}

/// **The one source rule of this file**, and it holds this file's own spelling only: every path into the
/// application crate starts `plxnative_modules::host`. What the crate itself makes reachable is not read here;
/// that is pinned in-crate, by the source pins in `host.rs`'s tests (the crate root's `pub` items, and `host.rs` as
/// the only `pub use` that names `app`). The lower-layer crates a platform crate lists as its
/// own dependencies (`plx_os`, `plx_media`) are fine to name, and this file must (a `Services` is built from
/// `plx_os::tv`'s hook tables, `MediaBoot::compose` takes `plx_media`'s `PlayerAdapter`); an edit to this file that
/// names the application crate any other way fails here.
#[test]
fn every_path_into_the_application_crate_goes_through_host() {
    let source = include_str!("host_interface.rs");
    // Split, so this test's own text never matches.
    let needle = concat!("plxnative_", "modules");
    let mut seen = 0;
    for (at, _) in source.match_indices(needle) {
        seen += 1;
        let line = source[..at].matches('\n').count() + 1;
        assert!(
            source[at + needle.len()..].starts_with("::host"),
            "line {line}: a name of the application crate that is not a path into `host`"
        );
    }
    assert!(seen > 0, "the scan found no path at all: it is not reading this file");
}

/// The entries of `dir` whose name starts with `plxnative-`, sorted.
#[cfg(feature = "hostsim")]
fn plxnative_entries(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("plxnative-"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// **The boot of the fake platform, to its refusal.** The real `run_application` runs on `PLATFORM`:
/// it installs the platform's services, brings the shell up (`init`), reads the stored session, then asks
/// the shell for the window (`open`), which refuses. The test asserts the order, the window asked for, the
/// installed services and the returned `Exit`, and that the boot's event log landed in the throwaway runtime root.
/// It also PRINTS the `plxnative-*` entries of `/tmp` (where a device's trigger files live) and of the runtime root
/// before and after, for a reader of `--nocapture`; it does not assert on `/tmp`, which other checkouts' test runs
/// write under concurrently.
#[cfg(feature = "hostsim")]
#[test]
fn run_boots_the_fake_platform_through_init_then_open_and_returns_the_refusal() {
    let runtime = std::env::var_os("PLXNATIVE_RUNTIME_DIR").map(std::path::PathBuf::from).expect(
        "PLXNATIVE_RUNTIME_DIR must name a throwaway directory: this test boots the application, which writes \
         its logs and reads its triggers under the runtime root (`make test-host-interface` sets it)",
    );
    assert!(runtime.is_dir(), "the runtime root {} is not a directory", runtime.display());
    let tmp = std::path::Path::new("/tmp");
    // The recipe's own throwaway directories (`plxnative-check.XXXXXX`) come and go with other runs.
    let outside = |names: Vec<String>| -> Vec<String> {
        names.into_iter().filter(|n| !n.starts_with("plxnative-check.")).collect()
    };
    let tmp_before = outside(plxnative_entries(tmp));
    let runtime_before = std::fs::read_dir(&runtime).map(|rd| rd.count()).unwrap_or(0);

    let host = CString::new("127.0.0.1").unwrap();
    let exit = plxnative_modules::host::run(&PLATFORM, &host, 32400);

    assert_eq!(exit, Exit { code: REFUSAL, release: Release::Retained }, "the fake's refusal is the entry's Exit");
    assert_eq!(*SHELL.log.lock().unwrap(), ["init", "open"], "init, then open, and nothing after the refusal");
    let (title, placement, high_dpi) = SHELL.asked.lock().unwrap().clone().expect("open was asked for a window");
    assert_eq!(title, "PlxNative", "the title a hostsim build asks for");
    assert_eq!(placement, Placement::Desktop, "a hostsim build asks for the desktop placement");
    eprintln!("open was asked for: title {title:?}, {placement:?}, high_dpi {high_dpi}");
    assert!(
        matches!(plx_os::tv::system_locale(), plx_os::tv::LocaleReply::Reply(ref l) if l == "fake-host-locale"),
        "the services the application installed are the platform's own table"
    );

    let tmp_after = outside(plxnative_entries(tmp));
    let runtime_after: Vec<String> = std::fs::read_dir(&runtime)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    eprintln!("/tmp/plxnative-* (outside plxnative-check.*) before: {tmp_before:?}");
    eprintln!("/tmp/plxnative-* (outside plxnative-check.*) after:  {tmp_after:?}");
    eprintln!("runtime root {}: {runtime_before} entries before, after: {runtime_after:?}", runtime.display());
    assert!(
        runtime_after.iter().any(|n| n == "plxnative-events.log"),
        "the boot's event log is not in the runtime root {}: {runtime_after:?}",
        runtime.display()
    );
}
