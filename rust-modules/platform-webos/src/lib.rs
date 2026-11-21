//! PlxNative's webOS platform: everything that exists only because the target is an LG television
//! running webOS, written against the application's published face (`plxnative_modules::host`).
//!
//! This is the crate the ARM staticlib is built from (`cargo rustc -p plx_platform_webos --lib
//! --crate-type staticlib`), and the application is its dependency, never the reverse: the boot shim
//! `src/main.c` calls the C entries this crate exports (`plex_run`, `plx_launch_args`, and the Starfish
//! sink's `acb_on_event`), and they hand the application its two tables and its entry. A desktop
//! executable never links it.
//!
//! - [`port`]: `plex_run`, the two tables that fill the platform (`SERVICES`, what `plx_os::tv` serves,
//!   and `PLATFORM`, those plus the application's `Shell` and `FrameProbe`), and the process entry;
//! - [`system`]: the LG half of the window (the Wayland surface, the opaque region, the pointer, the LS2
//!   bus pump, the frame-probe clock) and the two steps around `SDL_Init`;
//! - [`webos`]: the television's identity, capability probe, repair, toast and the Starfish/ACB video sink;
//! - [`keymanager`]: the public LS2 key stores (keymanager3, the legacy Palm service, or unavailable).
//!
//! The crate is a mounted module of the `platform webos` layer (`ci/module-layers.ini`): it may name every
//! layer and the application's `host`, nothing names it, and it names no other platform
//! (`docs/platform-ports.md`, W7). The one thing `plx_os` keeps of all this is the interface the table
//! fills (`plx_os::tv`).
mod keymanager;
mod port;
mod system;
mod webos;

#[cfg(test)]
mod sdl_pin_tests {
    /// **The SDL functions this crate declares are the four that stay, in the two files that hold them.**
    /// The stock SDL shell (the window, the GL context, the queue, the text-input panel) is `plx_shell_sdl`'s;
    /// what is LG's is `system.rs`'s `SDL_GetWindowWMInfo` handshake (the television's SDL writes a larger
    /// `SDL_SysWMinfo` than the headers declare, so the buffer and the version query stay verbatim) with the
    /// `SDL_GL_GetAttribute` read of the surface it describes, and `webos.rs`'s `SDL_MinimizeWindow` for the
    /// Home key. A fifth declaration here is a second SDL seam beside the shell crate's. Reads the sources,
    /// because no host test can see a declaration. The application crate holds itself to `clock.rs`'s
    /// `SDL_GetTicks` the same way (`app::platform`'s tests).
    #[test]
    fn the_webos_crate_declares_exactly_the_sdl_functions_it_keeps() {
        fn walk(dir: &std::path::Path, src: &std::path::Path, found: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).expect("read a source directory") {
                let path = entry.expect("a directory entry").path();
                if path.is_dir() {
                    walk(&path, src, found);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    let rel = path.strip_prefix(src).expect("under src").to_string_lossy().replace('\\', "/");
                    let text = std::fs::read_to_string(&path).expect("read a source file");
                    // Split with `concat!`, so this file's own text is never what it matches.
                    for piece in text.split(concat!("fn ", "SDL_")).skip(1) {
                        let name: String = piece.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                        found.push(format!("{rel}: SDL_{name}"));
                    }
                }
            }
        }
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found = Vec::new();
        walk(&src, &src, &mut found);
        found.sort();
        assert_eq!(
            found,
            [
                "system.rs: SDL_GL_GetAttribute",
                "system.rs: SDL_GetVersion",
                "system.rs: SDL_GetWindowWMInfo",
                "webos.rs: SDL_MinimizeWindow",
            ],
            "an SDL function is declared outside the four this crate keeps"
        );
    }
}
