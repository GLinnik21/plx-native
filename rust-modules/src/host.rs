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
//! it is `plx_os::tv::Services`, already a `pub` type of a lower crate, which this module re-exports beside them.
//! A platform crate names the application crate's types through `host` alone; the hook tables a `Services` is
//! built from (`plx_os::tv`) and `PlayerAdapter`, which `MediaBoot::compose` takes (`plx_media`), come from
//! their own crates, which it depends on directly.
//!
//! The split is by thread. A [`Platform`] is registration data in a `static`: function pointers and
//! `&'static dyn` of `Sync` types, [`Shell`] included as a factory. The thread-affine state (the window,
//! the GL context, a toolkit's handles) lives only in the [`Surface`] that [`Shell::open`] returns on the
//! loop thread and that the application owns from then on.
//!
//! Beside the platform's own types, the vocabulary their signatures name is re-exported from the lower
//! layers it lives in ([`MainThread`], [`Action`], [`Edge`], [`RawKey`], [`Lifecycle`], [`PointerEdge`],
//! [`OpenAsk`], [`Placement`]), so a platform crate names the application crate's types, and the vocabulary they use, through `host` alone.
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

/// **The owner's ruling, held in the source.** `tests/host_interface.rs` is a separate crate and reads only its own
/// spelling, so nothing there stops `lib.rs` from growing `pub mod app;` or `pub use app::platform::Shell;`: every
/// test would still pass. These pins read the crate's own source, in the style of the `include_str!` pins of
/// `port.rs` and `app/boot.rs`, and fail on that edit. The reason is Decision 3's amendment of 2026-11-21
/// (`docs/platform-ports.md`): `host` is the only way a platform crate reaches the application's types.
#[cfg(test)]
mod surface_pin_tests {
    use std::path::Path;

    /// What the crate root may make `pub`, by the head of the item: `host`, and the simulator binary's own doors
    /// (a separate crate, so they must be `pub`), each behind `feature = "hostsim"`. `host` alone is open to a
    /// television build.
    const ROOT_ITEMS: [(&str, bool); 7] = [
        ("pub mod host", false),
        ("pub mod hud_cli", true),
        ("pub mod platform_desktop", true),
        ("pub fn sim_runtime_dir", true),
        ("pub fn publish_sim_webos_major", true),
        ("pub fn sim_events_log", true),
        ("pub use app::synthetic_home_initial", true),
    ];

    const HOW_TO_PROCEED: &str = "The application is reachable only through `host` (the crate root also keeps the simulator's \
         `hostsim`-only doors, ROOT_ITEMS), because the owner's 2026-11-21 amendment to Decision 3 \
         (docs/platform-ports.md) makes `host` the only way a platform crate reaches the application. If this edit is \
         deliberate, extend ROOT_ITEMS in this test (or the one allowed `pub use` below) AND record the reason in that \
         amendment; do not publish an application item from lib.rs or from another module.";

    /// The text of `item`'s head: up to its first `;`, `{`, `(` or `<`.
    fn head(line: &str) -> &str {
        let end = line.find([';', '{', '(', '<']).unwrap_or(line.len());
        line[..end].trim()
    }

    /// Whether the attributes directly above line `at` gate the item behind `feature = "hostsim"` (a `not(...)` does
    /// not count), skipping the doc and line comments that sit between them.
    fn gated_behind_hostsim(lines: &[&str], at: usize) -> bool {
        let mut gated = false;
        for line in lines[..at].iter().rev().map(|l| l.trim()) {
            if line.starts_with("#[") {
                gated |= line.starts_with("#[cfg(") && line.contains("feature = \"hostsim\"") && !line.contains("not(");
            } else if !line.starts_with("//") {
                break;
            }
        }
        gated
    }

    #[test]
    fn the_crate_root_publishes_host_and_the_simulators_hostsim_doors_and_nothing_else() {
        let source = include_str!("lib.rs");
        let lines: Vec<&str> = source.lines().collect();
        let mut seen = Vec::new();
        for (at, line) in lines.iter().enumerate() {
            let line = line.trim_start();
            // `pub(crate)` has no space after `pub`, so it is not public and is not matched.
            if !line.starts_with("pub ") {
                continue;
            }
            let item = head(line);
            let Some(&(_, hostsim_only)) = ROOT_ITEMS.iter().find(|(known, _)| *known == item) else {
                panic!("lib.rs line {} makes `{item}` public. {HOW_TO_PROCEED}", at + 1);
            };
            assert!(
                !hostsim_only || gated_behind_hostsim(&lines, at),
                "lib.rs line {}: `{item}` is a simulator door and must be behind `feature = \"hostsim\"`, or a \
                 television build exposes it. {HOW_TO_PROCEED}",
                at + 1
            );
            seen.push(item);
        }
        for (item, _) in ROOT_ITEMS {
            assert!(seen.contains(&item), "lib.rs no longer makes `{item}` public: update ROOT_ITEMS to match. {HOW_TO_PROCEED}");
        }
        // An exported macro is a public path into the crate that a `pub ` line scan cannot see.
        assert!(!source.contains(concat!("#[macro_", "export")), "lib.rs exports a macro. {HOW_TO_PROCEED}");
    }

    /// `pub use` statements of `text`, each as `(line number, its text after `pub use` with whitespace removed)`.
    /// A statement runs to its `;`, so a `{ ... }` list over several lines is one.
    fn pub_uses(text: &str) -> Vec<(usize, String)> {
        let mut found = Vec::new();
        let mut lines = text.lines().enumerate();
        while let Some((at, line)) = lines.next() {
            let Some(rest) = line.trim_start().strip_prefix("pub use ") else { continue };
            let mut statement = rest.to_string();
            while !statement.contains(';') {
                match lines.next() {
                    Some((_, more)) => statement.push_str(more),
                    None => break,
                }
            }
            found.push((at + 1, statement.split_whitespace().collect()));
        }
        found
    }

    fn rust_sources(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir).expect("the source directory").map(|e| e.expect("an entry").path()).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                rust_sources(&path, root, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let name = path.strip_prefix(root).expect("under the root").to_string_lossy().into_owned();
                out.push((name, std::fs::read_to_string(&path).expect("a source file")));
            }
        }
    }

    #[test]
    fn host_is_the_only_place_that_re_exports_the_application() {
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
        let mut files = Vec::new();
        rust_sources(root, root, &mut files);
        assert!(files.iter().any(|(name, _)| name == "host.rs"), "the scan did not read this crate's own sources");
        let mut in_host = 0;
        for (name, text) in &files {
            for (at, statement) in pub_uses(text) {
                let names_app = ["crate::app::", "crate::{app", "app::", "{app"].iter().any(|p| statement.trim_start_matches("::").starts_with(p));
                if !names_app {
                    continue;
                }
                if name == "host.rs" {
                    in_host += 1;
                } else {
                    // The one other: the root's `hostsim`-only `synthetic_home_initial`, which the simulator binary calls.
                    assert!(
                        name == "lib.rs" && statement == "app::synthetic_home_initial;",
                        "{name} line {at} re-exports from the application (`pub use {statement}`). {HOW_TO_PROCEED}"
                    );
                }
            }
        }
        assert!(in_host >= 2, "host.rs re-exports the application's types: the scan found {in_host} `pub use` of `crate::app`");
    }
}
