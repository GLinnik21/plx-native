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
#[cfg(not(test))]
use crate::app::{SDL_GetWindowFlags, SDL_HasScreenKeyboardSupport, SDL_StartTextInput, SDL_StopTextInput};
#[cfg(test)]
use host_test_sdl::{SDL_GetWindowFlags, SDL_HasScreenKeyboardSupport, SDL_StartTextInput, SDL_StopTextInput};

/// The four SDL entry points this module calls, stubbed for the HOST TEST BINARY only.
///
/// `cargo test --lib` links a Mach-O binary against no SDL at all, and unlike every other SDL user
/// in this crate these calls are reachable from ordinary `pub(crate)` functions that a test can
/// touch — `available()` and `start()` are one `ui::search` call away. An unguarded reference is
/// therefore an undefined symbol at LINK time, which does not fail one test: it stops the whole
/// suite building, exactly as `ff.rs`'s `#[link]` directives used to before that module moved to
/// `dlopen`. SDL is a real link on the device, so the seam here is a `cfg` rather than a `dlopen`.
///
/// **Only `test` is stubbed.** `hostsim` links desktop SDL2 and takes the real arm — which is the
/// whole point, since the simulator is where the decode path can actually be exercised by typing.
// the stubs carry SDL's own C names ON PURPOSE — `use`d unqualified by the arms below, so the call
// sites read identically in both builds and a signature drift shows up as a type error there
#[allow(non_snake_case)]
#[cfg(test)]
mod host_test_sdl {
    use std::os::raw::{c_int, c_void};
    /// No window, so no focus bit — which makes [`super::has_focus`] answer `Some(false)` under
    /// test, the same answer a real television gives when the panel cannot rise.
    pub(super) unsafe fn SDL_GetWindowFlags(_w: *mut c_void) -> u32 {
        0
    }
    /// No panel on the host, matching what the simulator's own boot probe reports (`support=0`).
    pub(super) unsafe fn SDL_HasScreenKeyboardSupport() -> c_int {
        0
    }
    pub(super) unsafe fn SDL_StartTextInput() {}
    pub(super) unsafe fn SDL_StopTextInput() {}
}
use crate::log;
use std::os::raw::{c_int, c_void};
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
    trace_driver();
}

/// `SDL_LOG_CATEGORY_INPUT` / `SDL_LOG_PRIORITY_DEBUG`, from `SDL_log.h`.
const SDL_LOG_CATEGORY_INPUT: c_int = 3;
const SDL_LOG_PRIORITY_DEBUG: c_int = 2;

/// Ask LG's SDL to narrate its own keyboard lifecycle into stderr
/// (`/tmp/plxnative-stderr.log`).
///
/// The driver already logs `[WebOSShowScreenKeyboard] called`, `... called text_model_activate`,
/// `[TextModelLeave] called`, `[TextModelInputPanelState] called - state: %d` and
/// `[WebOSHideScreenKeyboard] called` — at `INPUT`/`DEBUG`, which is below the default priority,
/// so none of it prints. One call makes the whole sequence readable without patching SDL.
///
/// It is here for a specific open question, not as general noise. Disassembly says the panel
/// cannot be REOPENED because `text_model.leave` NULLs the model *before* calling
/// `SDL_StopTextInput`, so `WebOSHideScreenKeyboard` early-returns and `text_model_deactivate` is
/// never sent — after which the compositor ignores every later `activate`. The decisive
/// observation is whether `[TextModelLeave] called` appears BEFORE our own `stop()`: if it does,
/// the compositor dismissed the panel and that path ran; if it does not, our dismissal sent the
/// deactivate and something else is wrong. One is a five-line mitigation, the other is not.
///
/// Cheap and permanent: a handful of lines per keyboard session, on a log that is already the
/// primary debugging surface, on a path a user reaches only by opening a search field.
fn trace_driver() {
    #[cfg(not(test))]
    unsafe {
        crate::app::SDL_LogSetPriority(SDL_LOG_CATEGORY_INPUT, SDL_LOG_PRIORITY_DEBUG);
    }
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
        // The other half of `start`'s line, and the pair is the point: `start` is a NO-OP while
        // `STARTED` is set, so a `stop` that never runs makes every later `start` silent and the
        // panel simply never returns — which looks exactly like the driver's own reopen wedge and
        // is not it. With both transitions logged, one log answers which of the two you have.
        log("keyboard: stop");
        SDL_StopTextInput();
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
/// What replaced it is in `ui::search::pump_text`: **an arriving `SDL_TEXTINPUT` is proof the panel
/// is up**, so a commit that lands while the screen thinks it is not editing ADOPTS the panel
/// instead of being dropped. That signal cannot lie, it needs no polling, and it self-heals every
/// route into the mismatch rather than the one this was aimed at.
const _: () = ();

/// Re-take ownership of a panel that is demonstrably already up — see the note above and
/// `ui::search::pump_text`. Deliberately does NOT call `SDL_StartTextInput` (the panel is up; asking
/// for it again re-issues a wayland IME activation for nothing) and deliberately does NOT clear
/// [`PENDING`], because the whole reason this is being called is that a commit is waiting in it.
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
    // The two edges of a codepoint the fixed array may have CUT — see `drop_cut_tail`. The tail
    // rule is conditional on the array being FULL (`n == TEXT_CAP`), which is the signature of a
    // cut; the head rule is unconditional, because no UTF-8 string may begin with a continuation
    // byte under any circumstances. Deliberately NOT `n == field.len()`, which is also true when
    // the CALLER's buffer ran out before `text[32]` did — a short buffer is the reader's limit,
    // not the panel's, and there is nothing to say the panel cut anything at all.
    let text = drop_orphan_head(&field[..n]);
    let text = if n == TEXT_CAP { drop_cut_tail(text) } else { text };
    // Lossy, never `from_utf8`: a single bad byte would otherwise discard the whole commit — and
    // an IME that emits one is a keystroke the user typed, not an attack.
    String::from_utf8_lossy(text).into_owned()
}

/// Drop a trailing UTF-8 sequence that is well-formed so far but CUT SHORT by the end of the
/// buffer. Everything else — a lone continuation byte, an overlong or out-of-range lead, a
/// surrogate — is left for `from_utf8_lossy` to answer with U+FFFD, because a replacement character
/// is the only visible signal that something arrived broken and it must not be swallowed.
///
/// **This is LG checklist row #16** (`docs/lg-self-checklist.md` §2), which asks that the character
/// which arrives be the character that was pressed. `text[32]` is a fixed array, so a commit that
/// fills it is cut at a BYTE, and that cut lands mid-codepoint for any query that is not pure
/// ASCII. Decoded lossily, the surviving half becomes a `` the user never typed, in their search
/// field — exactly the substitution the row is about. Nothing can recover the character (its other
/// half is not in this event), so the honest answer is the text that WAS whole.
///
/// **The classification is `std`'s, not ours.** `Utf8Error::error_len() == None` is precisely
/// "valid so far, ended mid-sequence", and `valid_up_to()` is where to cut — where a hand-rolled
/// lead-byte width table accepts `0xC0`, `0xF5..=0xF7` and surrogate pairs as merely "incomplete"
/// and deletes real corruption without a trace. Only the LAST sequence is offered to it (walk back
/// at most four bytes to the first non-continuation byte; no sequence is longer), so an interior
/// error earlier in the commit cannot suppress the tail rule.
///
/// **Belt-and-braces for a case this project has not settled.** Stock SDL2 fills `text[32]` with
/// `SDL_utf8strlcpy`, which truncates on a character boundary and always terminates — so on stock
/// SDL this branch is unreachable, and `encode_event` cuts on a boundary too, which is why nothing
/// the simulator or the `txt:` token can drive reaches it either. Whether **LG's fork** does the
/// same is a question for the television's own `libSDL2`, not for a header or another client:
/// `.agents/skills/decompile-tv-lib/` is how the `+16 inputSource` offset above was settled, and it
/// is how this would be. Until then the branch costs one comparison and cannot make a correct
/// commit wrong.
fn drop_cut_tail(b: &[u8]) -> &[u8] {
    for back in 1..=b.len().min(4) {
        let i = b.len() - back;
        if b[i] & 0xC0 == 0x80 {
            continue; // a continuation byte — the sequence it belongs to starts further back
        }
        return match std::str::from_utf8(&b[i..]) {
            Err(e) if e.error_len().is_none() => &b[..i],
            _ => b,
        };
    }
    b // nothing but continuation bytes: garbage, and `from_utf8_lossy`'s to answer
}

/// The other edge of the same cut: drop a leading run of continuation bytes.
///
/// If the panel really does split a codepoint at `text[32]`, the half [`drop_cut_tail`] discards
/// arrives at the START of the NEXT commit — orphaned, with its lead byte in the event before it.
/// Left alone those bytes are one U+FFFD each, which is the row-#16 substitution again, one
/// character later; dropping the head is what makes the rule whole rather than one-sided.
///
/// Unconditional, and safe to be: **no UTF-8 string may begin with a continuation byte**, so this
/// can only ever remove bytes that were already unreadable — never a character the user typed.
fn drop_orphan_head(b: &[u8]) -> &[u8] {
    let n = b.iter().take_while(|&&c| c & 0xC0 == 0x80).count();
    &b[n..]
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
        // A bad byte in the MIDDLE keeps its replacement whatever surrounds it — it is neither
        // edge of a cut, so neither trim rule can claim it. (A LEADING continuation byte is the
        // orphan case and IS dropped; `a_commit_orphaned_from_its_lead_byte_is_dropped_rather_
        // than_replaced` owns that, and `\x80` alone belongs to it rather than here.)
        assert_eq!(decode_text_at(&ev_with(16, b"x\x80y"), 16), "x\u{fffd}y");
        assert_eq!(decode_text_at(&ev_with(16, b"x\xc0y"), 16), "x\u{fffd}y");
    }

    /// **Row #16: a codepoint the fixed array CUT must not arrive as a character nobody typed.**
    /// `text[32]` has no room for the tail of a sequence that starts at its last byte, so a lossy
    /// decode puts a `` in the user's query — the exact substitution the checklist row is about.
    /// The whole prefix is kept; only the half codepoint goes.
    #[test]
    fn a_codepoint_cut_by_the_end_of_the_field_is_dropped_not_replaced() {
        // Each width of lead byte, cut at every point that leaves it incomplete.
        for (lead, whole) in [(0xc3u8, "é"), (0xe5, "千"), (0xf0, "🎬")] {
            let need = whole.len();
            for kept in 1..need {
                let mut raw = vec![b'y'; TEXT_CAP - kept];
                raw.extend_from_slice(&whole.as_bytes()[..kept]);
                assert_eq!(raw.len(), TEXT_CAP);
                let got = decode_text_at(&ev_with(16, &raw), 16);
                assert_eq!(got, "y".repeat(TEXT_CAP - kept), "lead {lead:#x}, {kept} of {need} bytes");
                assert!(!got.contains('\u{fffd}'), "a cut must not invent a character");
            }
            // …and the same codepoint that FITS is untouched, which is what proves the rule cuts
            // only what was actually truncated.
            let mut raw = vec![b'y'; TEXT_CAP - need];
            raw.extend_from_slice(whole.as_bytes());
            assert_eq!(decode_text_at(&ev_with(16, &raw), 16), format!("{}{whole}", "y".repeat(TEXT_CAP - need)));
        }
    }

    /// A TERMINATED commit is as long as the panel meant it to be, so a malformed tail inside one
    /// is genuine garbage and keeps the defensive replacement — the cut rule must not swallow it.
    /// Neither may a SHORT BUFFER be read as a cut: that is the caller's limit, not the panel's.
    #[test]
    fn a_bad_tail_before_a_terminator_still_replaces_rather_than_vanishing() {
        // `ab` + a bare 3-byte lead + NUL: nothing cut this, the byte is simply wrong.
        assert_eq!(decode_text_at(&ev_with(16, b"ab\xe5\0"), 16), "ab\u{fffd}");
        // …and a buffer that ends mid-codepoint is the READER running out, so the tail rule stays
        // off and the bad byte is still reported.
        assert_eq!(decode_text_at(&ev_with(16, "é".as_bytes())[..17], 16), "\u{fffd}");
    }

    /// **A byte no cut could have produced keeps its replacement.** The rule is "valid so far,
    /// ended mid-sequence" and nothing looser — a hand-rolled lead-byte width table calls all of
    /// these merely incomplete and deletes them, which turns real corruption from the panel's IME
    /// into a silent shortening with no signal at all. `std::str::from_utf8` draws the line.
    #[test]
    fn corruption_that_is_not_a_cut_is_reported_rather_than_deleted() {
        for (name, tail) in [
            ("overlong lead", &[0xC0u8][..]),
            ("out-of-range lead", &[0xF5][..]),
            ("never-a-lead byte", &[0xFF][..]),
            ("surrogate half", &[0xED, 0xA0][..]),
        ] {
            let mut raw = vec![b'y'; TEXT_CAP - tail.len()];
            raw.extend_from_slice(tail);
            let got = decode_text_at(&ev_with(16, &raw), 16);
            assert!(got.contains('\u{fffd}'), "{name} must survive as a replacement, got {got:?}");
        }
    }

    /// The other edge of the same cut: a commit ORPHANED from its lead byte — the remainder of a
    /// codepoint the previous event was cut inside — must not arrive as one U+FFFD per byte, which
    /// is the row-#16 substitution again, one character later.
    #[test]
    fn a_commit_orphaned_from_its_lead_byte_is_dropped_rather_than_replaced() {
        // "🎬" is F0 9F 8E AC; cut after F0, the next commit opens with its three tail bytes.
        assert_eq!(decode_text_at(&ev_with(16, b"\x9f\x8e\xacstar"), 16), "star");
        // …and a commit that is nothing BUT an orphaned remainder yields nothing, rather than one
        // replacement character per byte.
        assert_eq!(decode_text_at(&ev_with(16, b"\x80"), 16), "");
        // …and a commit that legitimately STARTS with a multi-byte character is untouched.
        assert_eq!(decode_text_at(&ev_with(16, "🎬star".as_bytes()), 16), "🎬star");
    }

    /// **Capital and small letters transfer as themselves.** The panel's Shift/Caps is resolved on
    /// the television — it commits the CASED character — so the app's whole obligation is to carry
    /// the bytes through without touching them, and this is what proves it does. (A decoder that
    /// normalised case would be invisible until someone searched for a title that differs only by
    /// it.) Deliberately cites no checklist row number: `docs/lg-self-checklist.md` records no such
    /// row, and a number this repo cannot corroborate does not belong in a permanent comment.
    #[test]
    fn letter_case_transfers_exactly_as_the_panel_committed_it() {
        for s in ["A", "a", "Wallace", "WALLACE", "wallace", "WaLlAcE", "Ç", "ç", "Ä", "ä"] {
            assert_eq!(decode_text_at(&ev_with(16, s.as_bytes()), 16), s);
            assert_eq!(decode_text_at(&encode_event(s), TEXT_OFF), s);
        }
    }

    /// **Row #16, its own worked example.** The checklist names the full-width `＼` (U+FF3C) and
    /// asks that it not arrive as a backslash — the substitution a keyboard stack makes when it
    /// folds full-width forms to ASCII, and the one a byte-oriented decoder makes when it reads one
    /// of the three bytes of a full-width form on its own.
    ///
    /// The `lookalike` column is not a second assertion (the equality above already implies it);
    /// it is the table saying what each row is FOR, so the next reader can tell a deliberate pair
    /// from an arbitrary one. The rest are the characters a VKB commonly substitutes or a layer
    /// below eats — the shell and URL metacharacters, and the quote pair a keyboard "smartens".
    #[test]
    fn a_full_width_character_is_not_folded_to_its_ascii_lookalike() {
        for (typed, _lookalike) in [
            ("＼", "\\"), // U+FF3C FULLWIDTH REVERSE SOLIDUS — the row's own example
            ("￥", "\\"), // U+FFE5, the same key on a JP panel, and the same wrong answer
            ("／", "/"),
            ("：", ":"),
            ("　", " "),  // U+3000 IDEOGRAPHIC SPACE
            ("“", "\""),
            ("’", "'"),
            ("－", "-"),
        ] {
            let got = decode_text_at(&ev_with(16, typed.as_bytes()), 16);
            assert_eq!(got, typed, "the character pressed must be the character that arrives");
            assert_eq!(decode_text_at(&encode_event(typed), TEXT_OFF), typed);
        }
        // The ASCII originals still arrive as themselves — nothing here rewrites in either
        // direction. `\` is the one the row calls out, and the rest are what a URL layer or a
        // shell would eat if anything on the path were not byte-transparent.
        for s in ["\\", "/", "&", "%", "+", "#", "?", "=", "\"", "'", "<", ">", " ", "\t"] {
            assert_eq!(decode_text_at(&ev_with(16, s.as_bytes()), 16), s);
        }
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
