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
//! comes back as an ordinary `SDL_TEXTINPUT` event, so `app.rs`'s existing `SDL_PollEvent` loop
//! already sees it. moonlight-tv 1.5.8 shipped exactly this against the TV's own SDL.
//!
//! Linking is the plain `extern "C"` case, not `dynlib!`: all 14 firmware images in
//! `tools/fwcompat.py`'s inventories export the whole `SDL_*TextInput*` family. That says nothing
//! about whether the PANEL rises, though — those are stock public API in every SDL2 build — so
//! [`available`] probes at runtime rather than assuming.
//!
//! ## Three traps, all of them silent
//!
//! 1. **The event is shifted.** webOS inserts a `Uint32 inputSource` at `+12`, so the UTF-8 text
//!    starts at **+16**, not the `+12` the vendored `include/SDL2/SDL_events.h` declares. That
//!    header is stock 2.0.4 and lies about this build; the NDK sysroot's fork copy is the proof.
//!    Under `hostsim` the offset really is `+12` — desktop SDL2 is stock — so this is a `cfg`, and
//!    a single hard-coded number ships garbage on one of the two platforms. [`TEXT_OFF`] is that
//!    `cfg`; `decode_text_at` takes the offset as an argument so `make check` grades both.
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
//! `app.rs`'s `txt:` token drives [`on_event`] directly, and what it proves stops one step short
//! of `SDL_PollEvent`'s own delivery.
//!
//! Two dead ends, so nobody spends a day on them again: the Luna route (only four
//! `com.webos.service.ime/*` methods, all in ACGs this app does not hold — it is granted
//! `["public"]`), and a physical USB or Bluetooth keyboard (`/dev/input` is not mounted into our
//! jail; `remote.rs` documents the general case).
#![allow(dead_code)]

use crate::app::{
    SDL_GetWindowFlags, SDL_HasScreenKeyboardSupport, SDL_StartTextInput, SDL_StopTextInput,
    SDL_WINDOW_INPUT_FOCUS,
};
use crate::log;
use std::os::raw::c_void;
use std::ptr::{addr_of, addr_of_mut};

/// `SDL_TEXTINPUTEVENT_TEXT_SIZE` — `char text[32]`, inline in the event rather than a pointer.
/// Both headers agree on the size; they disagree only about where it starts (trap 1).
const TEXT_CAP: usize = 32;

/// Where that array begins in the raw event bytes. **Trap 1**, and the only number in this file
/// that is wrong on the platform you are not currently building.
///
/// `cfg!` and not `#[cfg]`, matching `app::decode_key`: both arms stay compiled on both platforms,
/// so the one nobody is building cannot rot into a compile error nobody sees for a month.
const TEXT_OFF: usize = if cfg!(feature = "hostsim") { 12 } else { 16 };

/// How many un-drained commits to hold before dropping the oldest.
///
/// Not a queue-depth tuning knob — a leak bound. Text events are delivered to the whole app, not
/// to a screen, and on a desktop SDL they are enabled from `SDL_VideoInit` onward (see [`start`]),
/// so every keystroke anywhere in the simulator lands here whether or not anything intends to
/// read it. The screen that cares drains every frame, so a buffer this deep means nobody is
/// draining at all, and the only thing left to protect is memory.
const MAX_PENDING: usize = 256;

/// Committed text, oldest first, waiting for [`drain`].
///
/// Main-thread-only by construction: written from the SDL event loop in `app.rs`, read by the
/// screen's `update` in the same loop. Same `static mut` + `addr_of` idiom as `search.rs`.
static mut PENDING: Vec<String> = Vec::new();

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
}

/// `SDL_GetWindowFlags & SDL_WINDOW_INPUT_FOCUS`, or `None` before [`bind`].
fn has_focus() -> Option<bool> {
    unsafe {
        let win = *addr_of!(WIN);
        if win.is_null() {
            return None;
        }
        Some(SDL_GetWindowFlags(win) & SDL_WINDOW_INPUT_FOCUS != 0)
    }
}

/// Does this build/firmware claim a system keyboard? Probed, never assumed — see the module doc.
/// A `false` here means the field can still be typed into by other means but no panel will rise.
pub(crate) fn available() -> bool {
    unsafe { SDL_HasScreenKeyboardSupport() != 0 }
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
        (*addr_of_mut!(PENDING)).clear();
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
        SDL_StartTextInput();
    }
}

/// Dismiss it. Also idempotent.
pub(crate) fn stop() {
    unsafe {
        // Guarded, and not only for symmetry: `ui::search` calls this on every BACK and every
        // leave, including ones where the field was never edited. Unguarded, that would call
        // `SDL_StopTextInput` on a simulator where text events had been on since `SDL_VideoInit`
        // and turn them off for the rest of the process — the decode path below would then be
        // dead, on the one platform where it can be exercised by typing.
        if !*addr_of!(STARTED) {
            return;
        }
        *addr_of_mut!(STARTED) = false;
        SDL_StopTextInput();
    }
}

/// Take everything committed since the last call, in order, and clear the buffer.
///
/// A `Vec<String>` and not a single `String` because one commit is not one character: an IME can
/// commit a whole word at once, and the caller may want to know where one ended and the next
/// began. Returning owned data (rather than lending a static) keeps the caller free to hold it
/// across a frame.
pub(crate) fn drain() -> Vec<String> {
    unsafe { std::mem::take(&mut *addr_of_mut!(PENDING)) }
}

/// How many commits are waiting for [`drain`]. Diagnostic only — the screen drains rather than
/// counts — but it is what makes the `txt:` remote token observable in the event log while
/// nothing is draining yet.
pub(crate) fn pending() -> usize {
    unsafe { (*addr_of!(PENDING)).len() }
}

/// What [`on_event`] would read out of these bytes, at THIS platform's offset. Exposed so a
/// caller can log the decode without duplicating the choice of offset — which is the one thing in
/// this file that must never be written down twice.
pub(crate) fn decode(ev: &[u8]) -> String {
    decode_text_at(ev, TEXT_OFF)
}

/// One `SDL_TEXTINPUT` event, straight off the wire. Called from `app.rs`'s event ladder.
pub(crate) fn on_event(ev: &[u8]) {
    let s = decode(ev);
    // An empty commit is not a commit. SDL delivers `SDL_TEXTINPUT` for the IME's own bookkeeping
    // as well as for text, and a caller that reads "drain returned something" as "the query moved"
    // would spend a round trip re-asking the server the same question.
    if s.is_empty() {
        return;
    }
    unsafe {
        let pend = &mut *addr_of_mut!(PENDING);
        if pend.len() >= MAX_PENDING {
            pend.remove(0); // see MAX_PENDING: bound the leak, keep the RECENT keystrokes
        }
        pend.push(s);
    }
}

/// The bytes an `SDL_TEXTINPUT` event carries for `text`, in whichever layout [`decode_text_at`]
/// reads — the inverse of it, and, like `app::encode_key`/`app::decode_key`, the pair is only ever
/// correct together. Nothing in the compiler couples them; `encode_decode_round_trips` does.
///
/// It exists so this seam can be DRIVEN, by `app.rs`'s `txt:` remote token. A real
/// `SDL_TEXTINPUT` needs a human at a keyboard — no trigger raises the panel, and typing into the
/// simulator means somebody physically typing — so without it nothing automated could ever put a
/// character in the search field, on either platform.
///
/// The bytes are a faithful event in THIS platform's layout, and `txt:` hands them to
/// [`on_event`] rather than to `SDL_PushEvent`: pushing a synthetic `SDL_TEXTINPUT` segfaults
/// inside the Mac's SDL, for the reason recorded at that call site and in the dead ends above.
pub(crate) fn encode_event(text: &str) -> [u8; 128] {
    let mut ev = [0u8; 128];
    ev[0..4].copy_from_slice(&crate::app::SDL_TEXTINPUT.to_ne_bytes());
    // `text[32]` holds at most 31 bytes plus its terminator — and the cut must land on a CHAR
    // BOUNDARY. Slicing a codepoint in half would make the decoder answer U+FFFD for text that
    // was perfectly valid when it was written, which reads as a decoder bug and is not one.
    let mut n = text.len().min(TEXT_CAP - 1);
    while n > 0 && !text.is_char_boundary(n) {
        n -= 1;
    }
    ev[TEXT_OFF..TEXT_OFF + n].copy_from_slice(&text.as_bytes()[..n]);
    ev
}

/// The committed text in a raw `SDL_TEXTINPUT` event whose `text[]` begins at `off`.
///
/// Pure, and offset-parameterised rather than reading [`TEXT_OFF`] itself, so `make check` can
/// grade BOTH platforms' layouts on a host that only ever builds one of them — the same reason
/// `app::decode_key` and `app::encode_key` are pure and tested as a pair.
///
/// Every failure mode here is a byte string somebody else chose: the panel's own IME writes this
/// field, and the app cannot afford a panic inside the SDL event loop (see `remote.rs`, where
/// exactly that class took the app down). So a short buffer, a missing NUL and invalid UTF-8 are
/// all answers, not faults.
fn decode_text_at(ev: &[u8], off: usize) -> String {
    // `min(ev.len())` before the slice, not after: `text[32]` is the LAST field of the event, so a
    // truncated or short buffer is the ordinary case at the end of the struct, not a malformed one.
    let end = off.saturating_add(TEXT_CAP).min(ev.len());
    let Some(field) = ev.get(off..end) else {
        return String::new(); // off past the end — `get` refuses the inverted range rather than panicking
    };
    // NUL-terminated INSIDE a fixed array: the terminator is the length, and everything after it
    // is whatever the last, longer commit left behind.
    let n = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    // Lossy, never `from_utf8`: a single bad byte would otherwise discard the whole commit — and
    // an IME that emits one is a keystroke the user typed, not an attack.
    String::from_utf8_lossy(&field[..n]).into_owned()
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `PENDING`/`STARTED` are module singletons, so the tests that drive them must not overlap.
    /// Module-local rather than `crate::testlock`: nothing outside this file touches either, and
    /// `testlock` is for globals that move under ANOTHER module's code (see `lib.rs`).
    static BUF: Mutex<()> = Mutex::new(());

    /// A synthetic event in one platform's layout: type at +0, the text at `off`, NUL-terminated
    /// if it fits. `[u8; 128]` because that is what `app.rs` polls into.
    fn ev_with(off: usize, text: &[u8]) -> [u8; 128] {
        let mut ev = [0u8; 128];
        ev[0..4].copy_from_slice(&0x303u32.to_ne_bytes()); // SDL_TEXTINPUT
        ev[off..off + text.len()].copy_from_slice(text);
        ev
    }

    /// **The trap this whole file is shaped around.** LG's fork inserts `Uint32 inputSource` at
    /// +12, so the text is at +16 on the television and at +12 on a desktop. A single hard-coded
    /// offset does not fail loudly on the wrong platform — it reads four bytes of `windowID` (or
    /// of the text's own head) and commits plausible garbage into the search field.
    #[test]
    fn the_text_decodes_at_both_platforms_offsets() {
        assert_eq!(decode_text_at(&ev_with(16, b"webos"), 16), "webos");
        assert_eq!(decode_text_at(&ev_with(12, b"desktop"), 12), "desktop");
        // …and reading one layout with the other's offset is exactly the silent corruption above:
        // at +16 a stock event is four bytes into the text, and at +12 a fork event is inside
        // `inputSource`, which is zero — an empty commit.
        assert_eq!(decode_text_at(&ev_with(12, b"desktop"), 16), "top");
        assert_eq!(decode_text_at(&ev_with(16, b"webos"), 12), "");
    }

    /// The ordinary case: a short string in a fixed 32-byte array, terminated by NUL. Everything
    /// after the terminator is the tail of some LONGER earlier commit, and must not be returned.
    #[test]
    fn a_nul_terminates_the_string_inside_the_fixed_array() {
        let mut ev = ev_with(16, b"ab\0cdefghijklmnop");
        ev[16 + 31] = b'!'; // …up to and including the last byte of text[32]
        assert_eq!(decode_text_at(&ev, 16), "ab");
    }

    /// A commit that fills `text[32]` completely has NO terminator — the array is the length. A
    /// decoder that insists on finding a NUL reads past the field into `SDL_Event`'s padding.
    #[test]
    fn a_full_32_byte_commit_has_no_terminator() {
        let text = [b'x'; TEXT_CAP];
        let ev = ev_with(16, &text);
        assert_eq!(decode_text_at(&ev, 16), "x".repeat(TEXT_CAP));
    }

    /// Invalid UTF-8 must not panic. This runs inside the SDL event loop, where a panic unwinds
    /// out of `plex_run` and takes the app with it — `remote.rs` lost the app exactly this way to
    /// a multi-byte whitespace. The bytes come from the television's own IME, so "that cannot
    /// happen" is not a thing this file gets to assume.
    #[test]
    fn invalid_utf8_is_replaced_rather_than_panicking() {
        let s = decode_text_at(&ev_with(16, b"a\xffb"), 16);
        assert_eq!(s, "a\u{fffd}b");
        // A lone continuation byte, and a truncated multi-byte sequence cut by the array's end.
        assert_eq!(decode_text_at(&ev_with(16, b"\x80"), 16), "\u{fffd}");
        let mut ev = ev_with(16, &[b'y'; TEXT_CAP]);
        ev[16 + TEXT_CAP - 1] = 0xe2; // the head of a 3-byte codepoint, with no room for its tail
        assert!(decode_text_at(&ev, 16).ends_with('\u{fffd}'));
    }

    /// A buffer that ends before (or inside) the text field is an answer, not a fault. `SDL_Event`
    /// is a union and nothing guarantees a caller polled into 128 bytes.
    #[test]
    fn a_short_buffer_yields_an_empty_string() {
        assert_eq!(decode_text_at(&[0u8; 8], 16), "");
        assert_eq!(decode_text_at(&[], 16), "");
        // Ending mid-field returns the part that IS there, rather than nothing.
        let ev = ev_with(16, b"abcdef");
        assert_eq!(decode_text_at(&ev[..19], 16), "abc");
    }

    /// Multi-byte UTF-8 survives intact — the field is bytes, not characters, and a European or
    /// CJK query is the normal case for a media library.
    #[test]
    fn multibyte_text_round_trips() {
        assert_eq!(decode_text_at(&ev_with(16, "Amélie".as_bytes()), 16), "Amélie");
        assert_eq!(decode_text_at(&ev_with(16, "千と千尋".as_bytes()), 16), "千と千尋");
    }

    /// The encoder writes what the decoder reads, on whichever platform this is — the pair that
    /// nothing but this test couples. `app.rs` already lost the simulator's whole remote FIFO to
    /// exactly this: `encode_key` wrote LG's layout while `decode_key` had been taught stock
    /// SDL2's, so every token was accepted and nothing moved.
    #[test]
    fn encode_decode_round_trips() {
        for s in ["a", "wallace", "Amélie", "千と千尋", "star wars"] {
            assert_eq!(decode_text_at(&encode_event(s), TEXT_OFF), s);
        }
    }

    /// Over-long text is cut to fit `text[32]`, and the cut lands on a CHAR BOUNDARY — a codepoint
    /// sliced in half would come back as U+FFFD and look like a decoder fault.
    #[test]
    fn an_over_long_commit_is_cut_on_a_char_boundary() {
        let long = "x".repeat(100);
        assert_eq!(decode_text_at(&encode_event(&long), TEXT_OFF), "x".repeat(TEXT_CAP - 1));
        // 16 CJK codepoints = 48 bytes: the 31-byte limit falls mid-codepoint, so the cut must
        // back up to 30 bytes (10 characters) rather than emit a replacement character.
        let cjk = "千".repeat(16);
        let got = decode_text_at(&encode_event(&cjk), TEXT_OFF);
        assert_eq!(got, "千".repeat(10));
        assert!(!got.contains('\u{fffd}'), "the cut must not split a codepoint");
    }

    /// Commits queue in order and `drain` takes them exactly once — the caller appends them to the
    /// query, so a duplicate is a doubled character and a lost one is a dropped keystroke.
    #[test]
    fn commits_queue_in_order_and_drain_takes_them_once() {
        let _g = BUF.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { (*addr_of_mut!(PENDING)).clear() };
        on_event(&ev_with(TEXT_OFF, b"wal"));
        on_event(&ev_with(TEXT_OFF, b"l"));
        assert_eq!(drain(), ["wal", "l"]);
        assert!(drain().is_empty(), "a second drain must not re-deliver what the first took");
    }

    /// An empty commit is dropped rather than queued: see `on_event`.
    #[test]
    fn an_empty_commit_is_not_queued() {
        let _g = BUF.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { (*addr_of_mut!(PENDING)).clear() };
        on_event(&ev_with(TEXT_OFF, b"\0"));
        on_event(&[0u8; 128]);
        assert!(drain().is_empty());
    }

    /// The leak bound holds, and holds the RECENT end. Nothing drains on Home, and on a desktop
    /// SDL every keystroke there arrives here regardless.
    #[test]
    fn the_pending_buffer_is_bounded() {
        let _g = BUF.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { (*addr_of_mut!(PENDING)).clear() };
        for i in 0..MAX_PENDING + 10 {
            on_event(&ev_with(TEXT_OFF, format!("{i}").as_bytes()));
        }
        let got = drain();
        assert_eq!(got.len(), MAX_PENDING);
        assert_eq!(got.last().unwrap(), &format!("{}", MAX_PENDING + 9));
    }
}
