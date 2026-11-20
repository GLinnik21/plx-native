//! textinput — the system on-screen keyboard, and the text it commits.
//!
//! Four calls wide: [`available`] asks whether this set has a panel, [`start`]/[`stop`] raise and
//! dismiss it, and [`drain`] takes what has been typed. The app draws no keyboard of its own — the
//! television draws it, over our surface, and hands the text back as ordinary SDL events.
//!
//! ## Why this is plain SDL and not a webOS call
//!
//! Stock `SDL_StartTextInput()` / `SDL_StopTextInput()` raise and dismiss the TV's own keyboard.
//! `SDL_webOS.h` misleads by omission — it has no keyboard entry point — but the backend is not in
//! the webOS extension API at all: it is inside LG's Wayland video driver.
//! `Wayland_CreateDevice` writes four real hooks into `SDL_VideoDevice`
//! (`WebOSHasScreenKeyboardSupport`, `Show`, `Hide`, `IsShown`), `SDL_StartTextInput` dispatches to
//! the second, and `WebOSShowScreenKeyboard` is a complete `text_model` IME client. Typed text
//! comes back as an ordinary `SDL_TEXTINPUT` event, so the existing `SDL_PollEvent` loop
//! (`plx_shell_sdl::event`) already sees it. moonlight-tv 1.5.8 shipped exactly this against the TV's own SDL.
//!
//! Linking is the plain `extern "C"` case (declared in `plx_shell_sdl`'s `sys.rs`), not `dynlib!`: all 14
//! firmware images in `tools/fwcompat.py`'s inventories export the whole `SDL_*TextInput*` family. That says nothing
//! about whether the PANEL rises, though — those are stock public API in every SDL2 build — so
//! [`available`] probes at runtime rather than assuming.
//!
//! ## Three traps, all of them silent
//!
//! 1. **The event is shifted.** webOS inserts a `Uint32 inputSource` at `+12`, so the UTF-8 text
//!    starts at **+16**, not the `+12` that stock SDL 2.0.4's `SDL_events.h` declares. That
//!    header lies about this build; the NDK sysroot's fork copy is the proof.
//!    Under `hostsim` the offset really is `+12` — desktop SDL2 is stock — so this is a `cfg`, and
//!    a single hard-coded number ships garbage on one of the two platforms. `plx_shell_sdl::text::TEXT_OFF`
//!    is that `cfg`, and the decoder next to it takes the offset as an argument so `make check` grades
//!    both: the byte layout is the shell crate's, and [`decode`] and [`encode_event`] are re-exported
//!    from it.
//! 2. **`SDL_WINDOW_INPUT_FOCUS` is a precondition.** `SDL_StartTextInput` shows the panel only
//!    `if (SDL_GetKeyboardFocus())` — the window carrying `0x200` — and our window is created
//!    `OPENGL | FULLSCREEN` with neither `SHOWN` nor a focus flag. If it is clear, SDL still
//!    enables text events and still returns void, so the panel simply never rises and nothing
//!    reports anything. Hence [`bind`]: the flag is logged at boot AND again inside [`start`], the
//!    moment it actually decides something.
//! 3. **A wedge we inherit**: the panel cannot be reopened after dismissal (moonlight-tv#435,
//!    reproduced on webOS 7.4). The community fix is in webosbrew's bundled SDL fork; we call the
//!    TV's own, so we get the bug with no patch. `SDL_SetTextInputRect` is a no-op here too —
//!    open and close, no positioning.
//!
//! Everything above was re-verified before it was written, against artefacts rather than memory:
//! `tools/fwcompat.py --lib libSDL2-2.0.so.0 --grep TextInput` (the family, on all 14 images),
//! the NDK sysroot's fork header (`inputSource`, hence `+16`), and `strings` over the real
//! television library kept in the gitignored `sysroot/usr/lib/` — which is SDL `2.0.5-83.gld4tv.15`
//! and does carry `text_model_interface`, `text_model_factory_interface`,
//! `WebOSShowScreenKeyboard` and `WebOSHideScreenKeyboard`. Note the `WebOS*` names are only
//! EXPORTED from webOS 7.4 on; on the dev set's 4.x they are internal, which is a property of that
//! build's symbol visibility and not evidence of absence.
//!
//! ## Do not fake the event with `SDL_PushEvent` (measured 2026-08-14)
//!
//! The obvious way to exercise this without a keyboard is to push a synthetic `SDL_TEXTINPUT`,
//! exactly as `app::remote_synth_key` and `remote_synth_ptr` push synthetic keys and clicks. **It
//! SIGSEGVs inside SDL**, with no Rust panic and no log line — `EXC_BAD_ACCESS`,
//! `KERN_INVALID_ADDRESS at 0x8`, backtrace `libSDL2-2.0.0.dylib SDL_PushEvent_REAL` →
//! `libSDL3.0.dylib SDL_PushEvent_REAL`. The Mac's `libSDL2` is **sdl2-compat forwarding into
//! libSDL3**, and SDL3's text event carries a `char *text` POINTER where SDL2's carries an inline
//! `char text[32]`; the compat layer dereferences it while converting the pushed event. Keys and
//! pointer clicks survive the same trip only because every field they use is a scalar. So
//! the application's `txt:` token drives [`on_event`] directly, and what it proves stops one step short
//! of `SDL_PollEvent`'s own delivery.
//!
//! Two dead ends, so nobody spends a day on them again: the Luna route (only four
//! `com.webos.service.ime/*` methods, all in ACGs this app does not hold — it is granted
//! `["public"]`), and a physical USB or Bluetooth keyboard (`/dev/input` is not mounted into our
//! jail; `remote.rs` documents the general case).
//!
//! ## Text that ARRIVES correctly and DRAWS as boxes (measured 2026-08-23)
//!
//! This module is byte-transparent and the tests below pin that — a full-width `＼`, a CJK run and
//! a Hangul syllable all reach the query as themselves. **They then render as `.notdef` boxes**,
//! because `pkg/appfont*.ttf` is a SUBSET: 2853 codepoints, Latin + Cyrillic + Greek, with no CJK,
//! no Hangul and no full-width forms (`tools/cut-inter.py` cuts it; `text.rs` has no per-glyph
//! fallback — its `DROIDSANS` path is a whole-font last resort for a MISSING file, not a chain).
//!
//! So a reader debugging "my search shows boxes" can stop here: the text arrived, and the gap is
//! the font, one layer down. It is recorded in this module because this is where that question
//! gets asked first, and because it is the half of LG checklist rows #15/#16 that no amount of
//! correctness in this file can answer — the rows are about what is ON THE PANEL, and a Korean or
//! Japanese query is unreadable in the field, in the "No results for …" line, and anywhere else the
//! query is echoed. Fixing it is a font decision (a wider cut, or a real fallback chain), not a
//! text-input one, and it is not in this module's gift.
#![allow(dead_code)]

use crate::app::SDL_WINDOW_INPUT_FOCUS;
use plx_base::eventlog::log;
use std::os::raw::c_void;
use std::ptr::{addr_of, addr_of_mut};

// What the panel IS (the SDL calls that raise and dismiss it, the focus flag, the driver's own log
// priority) and what an `SDL_TEXTINPUT` event looks like in bytes (LG's fork and stock SDL put the
// text at different offsets, and decoding and encoding are only ever correct as a pair) are the
// stock-SDL shell's: `plx_shell_sdl::text`, which stubs the four SDL calls for a host TEST binary
// (its `test-support` feature, enabled for this crate's tests) exactly as this module did. What
// stays here is the state machine around them and the queue.
pub(crate) use plx_shell_sdl::text::{decode, encode_event};

/// How many un-drained commits to hold before dropping the oldest.
///
/// Not a queue-depth tuning knob — a leak bound. Text events are delivered to the whole app, not
/// to a screen, and on a desktop SDL they are enabled from `SDL_VideoInit` onward (see [`start`]),
/// so every keystroke anywhere in the simulator lands here whether or not anything intends to
/// read it. Nothing drains the queue in production (a field with the keyboard, or Search, takes its text through
/// `ingest_text` instead, see [`PENDING`]), so this bound is the whole of the queue's memory
/// discipline.
const MAX_PENDING: usize = 256;

thread_local! {
    /// Committed text, oldest first, waiting for [`drain`].
    ///
    /// Nothing drains it in production: while a field owns text input (`bridge::text_owner`), typed text (the
    /// television's keyboard and the `txt:` remote token alike) goes through `app/run.rs`'s
    /// `ingest_text` into the app's input queue (`app.inputs`) and never reaches this one.
    /// [`queue`] fills it only on every other route, where it is a bounded buffer
    /// ([`MAX_PENDING`]) that [`start`] clears, and [`drain`] is read by the tests and nothing else.
    ///
    /// Main-thread-only by construction: [`queue`] is called from the event loop in
    /// `app/run.rs` and (through [`on_event`]) from the `txt:` remote token in `app/events.rs`, both on the frame thread. A
    /// `thread_local!` for that reason and not a `static mut`:
    /// the queue owns `String`s that `on_event` trims and `start`/`drain` clear, so one process-wide
    /// queue lets two threads free each other's commits, which is how `cargo test`'s parallel threads
    /// aborted `plx_ui` in libmalloc (2026-10-20). `STARTED` and `WIN` below are plain `Copy` data
    /// that free nothing, so they stay statics.
    static PENDING: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Whether WE have asked for the panel. Our own edge, deliberately not `SDL_IsTextInputActive`:
/// SDL's flag starts TRUE on a desktop and FALSE on the television (again, see [`start`]), so
/// reading it would make [`start`] a no-op on one of the two platforms.
static mut STARTED: bool = false;

/// The SDL window, for the focus flag alone. See [`bind`].
static mut WIN: *mut c_void = std::ptr::null_mut();

/// Hand this module the window, once, at boot. The window is the only way to read
/// `SDL_WINDOW_INPUT_FOCUS`, and that flag is trap 2: it is what decides whether the panel rises,
/// it is never reported as an error, and a reading taken at BOOT can be a false negative — the
/// flag is set when the wayland keyboard `enter` arrives, which needs an event loop that has not
/// started yet. So [`start`] re-reads it at the one moment it decides anything.
pub(crate) fn bind(win: *mut c_void) {
    unsafe { *addr_of_mut!(WIN) = win };
    plx_shell_sdl::text::trace_driver();
}

/// `SDL_GetWindowFlags & SDL_WINDOW_INPUT_FOCUS`, or `None` before [`bind`].
fn has_focus() -> Option<bool> {
    unsafe {
        let win = *addr_of!(WIN);
        if win.is_null() {
            return None;
        }
        Some(plx_shell_sdl::text::window_flags(win) & SDL_WINDOW_INPUT_FOCUS != 0)
    }
}

/// Does this build/firmware claim a system keyboard? Probed, never assumed — see the module doc.
/// A `false` here means the field can still be typed into by other means but no panel will rise.
pub(crate) fn available() -> bool {
    plx_shell_sdl::text::has_screen_keyboard()
}

/// Raise the keyboard. Idempotent: calling it while the panel is already up is not an error, and
/// the caller drives it off its own edit state rather than tracking whether it has been called.
///
/// Which means this runs EVERY FRAME while the field is focused, and `SDL_StartTextInput` is not
/// free on this platform: it re-issues `WebOSShowScreenKeyboard` — a real `text_model` IME
/// activation over wayland — each time. [`STARTED`] is what makes the contract true rather than
/// merely harmless.
pub(crate) fn start() {
    unsafe {
        if *addr_of!(STARTED) {
            return;
        }
        *addr_of_mut!(STARTED) = true;
        // Anything typed BEFORE the field opened belongs to whatever was on screen then, not to
        // this query. That is not hypothetical on a desktop: SDL2's `SDL_VideoInit` ends with
        // `if (!SDL_HasScreenKeyboardSupport()) SDL_StartTextInput();`, so on macOS text events
        // are on from boot and every keystroke on Home has been queueing here — without this the
        // first search field to open would come up pre-filled with the user's navigation. On the
        // television the same line means text events are OFF until this call, which is why
        // `SDL_IsTextInputActive` cannot stand in for `STARTED` above.
        PENDING.with(|pending| pending.borrow_mut().clear());
        // Trap 2, at the moment it matters. `support=0` means this firmware has no panel at all;
        // `focus=0` means SDL will enable text events and then silently skip the panel.
        log(&format!(
            "keyboard: start support={} focus={}",
            i32::from(available()),
            match has_focus() {
                Some(f) => i32::from(f).to_string(),
                None => "?".into(), // bind() was never called — a boot-order bug, not a TV fact
            }
        ));
        plx_shell_sdl::text::start_input();
    }
}

/// Dismiss it. Also idempotent.
pub(crate) fn stop() {
    unsafe {
        // Guarded, and not only for symmetry: `screens::search::SearchScreen` requests this on
        // every BACK and every leave, including ones where the field was never edited. Unguarded, that would call
        // `SDL_StopTextInput` on a simulator where text events had been on since `SDL_VideoInit`
        // and turn them off for the rest of the process — the decode path below would then be
        // dead, on the one platform where it can be exercised by typing.
        if !*addr_of!(STARTED) {
            return;
        }
        *addr_of_mut!(STARTED) = false;
        // The other half of `start`'s line, and the pair is the point: `start` is a NO-OP while
        // `STARTED` is set, so a `stop` that never runs makes every later `start` silent and the
        // panel simply never returns — which looks exactly like the driver's own reopen wedge and
        // is not it. With both transitions logged, one log answers which of the two you have.
        log("keyboard: stop");
        plx_shell_sdl::text::stop_input();
    }
}

/// **`SDL_IsScreenKeyboardShown` DOES NOT ANSWER "is the panel on screen" on this firmware, and a
/// feature built on it shipped a dead keyboard.** Recorded here rather than deleted, because it is
/// the obvious API for a real problem and the next person will reach for it.
///
/// The problem is real: the television takes its own panel away after an idle spell and tells the
/// app nothing — `SDL_TEXTINPUT` stops arriving, [`STARTED`] stays set, and the screen goes on
/// drawing a focused field with a blinking caret over a keyboard that has left. The obvious fix is
/// to poll `SDL_IsScreenKeyboardShown` and treat a `false` as the dismissal, latched behind a
/// first `true` so the panel's ASYNCHRONOUS rise cannot be mistaken for one.
///
/// It does not work. Measured on 4.10.0, from the event log, three seconds after a successful
/// `start` and with the panel plainly up and being typed on:
///
/// ```text
/// [110976] key RETURN            ← OK on the field
///          keyboard: start support=1 focus=1
///          keyboard: dismissed by the compositor   ← the poll, while the panel was UP
/// [113977] text src=35 'g'       ← …and every keystroke after it dropped on the floor
/// ```
///
/// So the flag goes true and then false again while the panel is alive — it tracks something in
/// LG's `text_model` activation (its `[TextModelInputPanelState]` callback is the likely source),
/// not the surface. The latch cannot help: the spurious `false` arrives *after* the true. Anything
/// that clears the editing state from this signal drops the user's typing, which is a total failure
/// of the feature to avoid a cosmetic one.
///
/// What replaced it is in `screens::search::SearchScreen`: **an arriving `SDL_TEXTINPUT` is proof the panel
/// is up**, so a commit that lands while the screen thinks it is not editing ADOPTS the panel
/// instead of being dropped. That signal cannot lie, it needs no polling, and it self-heals every
/// route into the mismatch rather than the one this was aimed at.
const _: () = ();

/// Re-take ownership of a panel that is demonstrably already up — see the note above and
/// `screens::search::SearchScreen`. Deliberately does NOT call `SDL_StartTextInput` (the panel is up; asking
/// for it again re-issues a wayland IME activation for nothing) and deliberately does NOT clear
/// [`PENDING`]; clearing it is [`start`]'s fresh-field rule, and the commit that proved the panel up is
/// not in it. That commit travels in `app.inputs`, behind the `SystemKeyboard(true)` input that led here
/// (`app::events::text_inputs`).
pub(crate) fn adopt() {
    unsafe {
        if *addr_of!(STARTED) {
            return;
        }
        *addr_of_mut!(STARTED) = true;
        log("keyboard: adopted (text arrived with the panel up)");
    }
}

/// Take everything committed since the last call, in order, and clear the buffer.
///
/// A `Vec<String>` and not a single `String` because one commit is not one character: an IME can
/// commit a whole word at once, and the caller may want to know where one ended and the next
/// began. Returning owned data (rather than lending a static) keeps the caller free to hold it
/// across a frame.
pub(crate) fn drain() -> Vec<String> {
    PENDING.take()
}

/// How many commits are waiting for [`drain`]. Diagnostic only — nothing in production drains or
/// counts for a decision — but it is what makes the `txt:` remote token observable in the event log
/// on a route whose text goes nowhere.
pub(crate) fn pending() -> usize {
    PENDING.with(|pending| pending.borrow().len())
}

/// One `SDL_TEXTINPUT` event, straight off the wire: [`decode`], then [`queue`]. Called by the
/// `txt:` remote token (`app/events.rs`), which has the bytes [`encode_event`] built. The event
/// loop gets its text already decoded (`HostEvent::Text`) and calls [`queue`] itself.
pub(crate) fn on_event(ev: &[u8]) {
    queue(decode(ev));
}

/// One committed string, queued for [`drain`] and bounded by [`MAX_PENDING`]. The loop's route for a
/// commit no field owns (`app/run.rs`'s `ingest_text_commit`).
pub(crate) fn queue(s: String) {
    // An empty commit is not a commit. SDL delivers `SDL_TEXTINPUT` for the IME's own bookkeeping
    // as well as for text, and a caller that reads "drain returned something" as "the query moved"
    // would spend a round trip re-asking the server the same question.
    if s.is_empty() {
        return;
    }
    PENDING.with(|pending| {
        let mut pend = pending.borrow_mut();
        if pend.len() >= MAX_PENDING {
            pend.remove(0); // see MAX_PENDING: bound the leak, keep the RECENT keystrokes
        }
        pend.push(s);
    });
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use plx_shell_sdl::text::TEXT_OFF;
    use std::sync::Mutex;

    /// `STARTED` is a module singleton (and `PENDING` is per thread, but these tests drive both
    /// together), so the tests that drive them must not overlap.
    /// Module-local rather than `plx_base::testlock`: nothing outside this file touches either, and
    /// `testlock` is for globals that move under ANOTHER module's code (see `lib.rs`).
    static BUF: Mutex<()> = Mutex::new(());

    /// A synthetic event in one platform's layout: type at +0, the text at `off`, NUL-terminated
    /// if it fits. `[u8; 128]` because that is what `plx_shell_sdl::event::poll` reads into.
    fn ev_with(off: usize, text: &[u8]) -> [u8; 128] {
        let mut ev = [0u8; 128];
        ev[0..4].copy_from_slice(&0x303u32.to_ne_bytes()); // SDL_TEXTINPUT
        ev[off..off + text.len()].copy_from_slice(text);
        ev
    }

    /// Commits queue in order and `drain` takes them exactly once — the caller appends them to the
    /// query, so a duplicate is a doubled character and a lost one is a dropped keystroke.
    #[test]
    fn commits_queue_in_order_and_drain_takes_them_once() {
        let _g = BUF.lock().unwrap_or_else(|e| e.into_inner());
        PENDING.with(|pending| pending.borrow_mut().clear());
        on_event(&ev_with(TEXT_OFF, b"wal"));
        on_event(&ev_with(TEXT_OFF, b"l"));
        assert_eq!(drain(), ["wal", "l"]);
        assert!(
            drain().is_empty(),
            "a second drain must not re-deliver what the first took"
        );
    }

    /// An empty commit is dropped rather than queued: see `on_event`.
    #[test]
    fn an_empty_commit_is_not_queued() {
        let _g = BUF.lock().unwrap_or_else(|e| e.into_inner());
        PENDING.with(|pending| pending.borrow_mut().clear());
        on_event(&ev_with(TEXT_OFF, b"\0"));
        on_event(&[0u8; 128]);
        assert!(drain().is_empty());
    }

    /// **The queue is the frame thread's own.** `PENDING` holds heap (a `Vec<String>`) that `on_event`
    /// pushes and trims and `start`/`drain` clear, so one process-wide queue lets two threads free
    /// each other's `String`s: the shape that aborted `plx_ui`'s tests in libmalloc (2026-10-20).
    /// `cargo test` runs tests on parallel threads, so each thread must see only the commits it
    /// queued itself. Every thread overflows the bound, which trims (frees) the oldest commit on
    /// every push.
    #[test]
    fn every_thread_drains_only_the_commits_it_queued() {
        plx_base::testrace::race(4, |t| {
            for round in 0..40 {
                for i in 0..MAX_PENDING + 20 {
                    on_event(&ev_with(TEXT_OFF, format!("t{t}r{round}i{i}").as_bytes()));
                }
                let got = drain();
                assert_eq!(got.len(), MAX_PENDING, "round {round}");
                let own = format!("t{t}r{round}i");
                assert!(
                    got.iter().all(|s| s.starts_with(&own)),
                    "round {round}: drained a commit another thread queued"
                );
                assert!(drain().is_empty());
            }
        });
    }

    /// The leak bound holds, and holds the RECENT end. Nothing drains on Home, and on a desktop
    /// SDL every keystroke there arrives here regardless.
    #[test]
    fn the_pending_buffer_is_bounded() {
        let _g = BUF.lock().unwrap_or_else(|e| e.into_inner());
        PENDING.with(|pending| pending.borrow_mut().clear());
        for i in 0..MAX_PENDING + 10 {
            on_event(&ev_with(TEXT_OFF, format!("{i}").as_bytes()));
        }
        let got = drain();
        assert_eq!(got.len(), MAX_PENDING);
        assert_eq!(got.last().unwrap(), &format!("{}", MAX_PENDING + 9));
    }
}
