//! PlxNative's stock-SDL shell: SDL's bring-up and quit, the window and its GL context, the swap,
//! the event queue, the key, pointer, wheel and text layouts of both SDLs this project runs on, and
//! the synthetic events that go back into that queue.
//!
//! This is the half of the platform that is SDL2 and nothing else. The television's LG half (its
//! Wayland handles, the opaque region, the pointer hide, the cursor-sleep hint) is webOS code and
//! stays in the webOS platform crate's `system.rs`, reached only through that platform; this
//! crate names none of it, and none of `ui`, `machine`, `os` or any application type. The contract
//! between the two halves is one raw `SDL_Window*` ([`window::Window::raw`]) and the order of calls
//! on one thread. [`init::InitHooks`] is the one place the platform reaches INTO SDL's bring-up,
//! as two plain function pointers the television fills and a desktop leaves empty.
//!
//! The crate is a mounted module of the `shell_sdl` layer (`ci/module-layers.ini`), which may use
//! `base`, `machine` and `os`; the crate names `base` alone. The application's `HostEvent` stays app-private, and its `Shell` and `Surface` traits are reachable only through
//! the application crate's `host` module (`docs/platform-ports.md`, Target shapes 1 and 8); [`window::Window`] is the one SDL
//! implementation of that `Surface`, and [`event::Decoded`] is the neutral value the application maps
//! onto its own event type.
//!
//! ## One crate, both SDLs
//!
//! The television's SDL is a fork (LG's fork of SDL2) that lays its keyboard, text and wheel events
//! out differently from the stock SDL2 a desktop links. The choice is `cfg!(feature = "hostsim")`
//! and never `#[cfg]`, so both arms of every layout compile in every build and the one nobody is
//! building cannot rot; the `hostsim` feature is forwarded from the application crate, which is the
//! only one that sets it.
//!
//! ## Tests
//!
//! `cfg(test)` does not cross a crate boundary, so the dependents' test builds get the stubs and the
//! event-queue guard through the `test-support` feature ([`testing`]); a shipped build has neither.
pub mod event;
pub mod init;
pub mod inject;
mod sys;
pub mod text;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
pub mod window;

pub use init::{set_hint, InitFailed, InitHooks};
pub use window::{OpenAsk, OpenFailed, Placement, Window};

#[cfg(test)]
mod pins {
    /// Every source file of this crate, by name, for the tests that read source. A new file that is
    /// not listed here is not scanned, which is why the list is checked against the directory.
    const SOURCES: [(&str, &str); 8] = [
        ("lib.rs", include_str!("lib.rs")),
        ("sys.rs", include_str!("sys.rs")),
        ("init.rs", include_str!("init.rs")),
        ("window.rs", include_str!("window.rs")),
        ("event.rs", include_str!("event.rs")),
        ("inject.rs", include_str!("inject.rs")),
        ("text.rs", include_str!("text.rs")),
        ("testing.rs", include_str!("testing.rs")),
    ];

    #[test]
    fn every_source_file_is_scanned() {
        // `*.rs` files and directories only: an editor's swap file or a `.DS_Store` is not source.
        let mut on_disk: Vec<String> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))
            .expect("the crate's src directory")
            .map(|e| e.expect("a directory entry"))
            .filter(|e| e.path().is_dir() || e.path().extension().is_some_and(|x| x == "rs"))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        on_disk.sort();
        let mut listed: Vec<&str> = SOURCES.iter().map(|s| s.0).collect();
        listed.sort();
        assert_eq!(on_disk, listed, "a source file this crate gained is not read by the pins below");
    }

    /// **The television's SDL writes a LARGER `SDL_SysWMinfo` than the headers declare**, so a bare
    /// struct on the stack is the "modular-split crash" and the over-allocated buffer with its
    /// version handshake lives in the webOS platform crate's `system.rs`, beside the rest of the LG
    /// half. Nothing in this crate may call it: not for the window, not for a size, not "just to
    /// read the display". The SDL version query that handshake is built on belongs to the same
    /// half and is pinned with it.
    #[test]
    fn this_crate_never_calls_the_wayland_info_query() {
        for needle in [concat!("SDL_GetWindow", "WMInfo"), concat!("SDL_Get", "Version")] {
            for (name, text) in SOURCES {
                assert!(!text.contains(needle), "{name} names {needle}, the television's over-writing query or its handshake");
            }
        }
    }

    /// The crate links SDL plainly (`sys.rs`), and it must not grow a Wayland, glib or LG-fork call
    /// of its own: those are the television's half and are fenced to the webOS platform by
    /// `ci/check-module-layers.py`, which cannot see an `extern` block. The needles are the
    /// Wayland proxy, display and surface names, the Luna register and call families, glib's main
    /// context, and the LG fork's own webOS-prefixed SDL entry points and helper-prefixed bridge
    /// names; each is split so this file, which the scan reads too, does not name the thing it
    /// forbids.
    #[test]
    fn this_crate_names_no_wayland_or_luna_symbol() {
        for needle in [
            concat!("wl_", "proxy"),
            concat!("wl_", "display"),
            concat!("wl_", "surface"),
            concat!("LS", "Register"),
            concat!("LS", "Call"),
            concat!("g_main_", "context"),
            concat!("SDL_", "webOS"),
            concat!("HELPER", "_"),
        ] {
            for (name, text) in SOURCES {
                assert!(!text.contains(needle), "{name} names {needle}");
            }
        }
    }
}
