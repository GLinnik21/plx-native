//! The text-input half that is SDL: the bytes of an `SDL_TEXTINPUT` event in this build's layout,
//! and the calls that raise and dismiss the system keyboard. The state a field's editing leaves
//! behind (what the application has asked for, the queue of commits, the focus handshake) is the
//! application's own `textinput` module; this file is only what knows SDL.
//!
//! **Trap 1, the offset.** webOS inserts a `Uint32 inputSource` at `+12` of `SDL_TextInputEvent`,
//! so the UTF-8 text starts at **+16** on the television, not the `+12` that stock SDL 2.0.4's
//! `SDL_events.h` declares and that a desktop's SDL2 really uses. That header lies about the LG
//! build; the NDK sysroot's fork copy is the proof. [`TEXT_OFF`] is that choice, `cfg!` and not
//! `#[cfg]` so both arms stay compiled and the one nobody is building cannot rot, and
//! `decode_text_at` takes the offset as an argument so `make check` grades both layouts. A single
//! hard-coded number ships garbage on one of the two platforms and fails nothing.
//!
//! The calls are stock public SDL2 API, so they are a plain link (`sys.rs`); whether the PANEL rises
//! is a firmware question the application probes at run time ([`has_screen_keyboard`]) rather than
//! assumes.
//!
//! **Do not fake the event with `SDL_PushEvent`** (measured 2026-08-14, recorded in the
//! application's `textinput` module). The Mac's `libSDL2` is sdl2-compat forwarding into SDL3, whose
//! text event carries a `char *text` pointer where SDL2's carries an inline `char text[32]`; the
//! compat layer dereferences it while converting the pushed event and SIGSEGVs inside SDL. So
//! [`encode_event`] builds the bytes and the `txt:` token hands them to the decode directly, and
//! nothing in `inject.rs` ever pushes a text event.
//!
//! Under `cfg(test)` and the `test-support` feature the four calls a host test could reach are
//! replaced by [`host_test_sdl`]'s stubs: a host test has no window and no panel.
use crate::event::SDL_TEXTINPUT;
use crate::sys;
#[cfg(not(any(test, feature = "test-support")))]
use crate::sys::{
    SDL_GetWindowFlags, SDL_HasScreenKeyboardSupport, SDL_StartTextInput, SDL_StopTextInput,
};
#[cfg(any(test, feature = "test-support"))]
use host_test_sdl::{
    SDL_GetWindowFlags, SDL_HasScreenKeyboardSupport, SDL_StartTextInput, SDL_StopTextInput,
};
use std::os::raw::c_void;

/// The four SDL entry points the keyboard calls reach, stubbed for a HOST TEST BINARY.
///
/// A host test cannot call them: no window exists, and on a desktop `SDL_StartTextInput` would
/// switch text events on for the rest of the process. `cfg(test)` does not cross a crate boundary,
/// so the application's own tests get these stubs through the `test-support` feature, which its
/// `[dev-dependencies]` enable and no shipped build does.
///
/// **Only a test is stubbed.** `hostsim` links desktop SDL2 and takes the real arm, which is the
/// whole point: the simulator is where the decode path is exercised by typing.
// the stubs carry SDL's own C names ON PURPOSE: `use`d unqualified by the arms above, so the call
// sites read identically in both builds and a signature drift shows up as a type error there
#[allow(non_snake_case)]
#[cfg(any(test, feature = "test-support"))]
mod host_test_sdl {
    use std::os::raw::{c_int, c_void};
    /// No window, so no focus bit: the application's `has_focus` answers `Some(false)` under test,
    /// the same answer a real television gives when the panel cannot rise.
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

/// `SDL_TEXTINPUTEVENT_TEXT_SIZE` — `char text[32]`, inline in the event rather than a pointer.
/// Both headers agree on the size; they disagree only about where it starts (trap 1).
pub const TEXT_CAP: usize = 32;

/// Where that array begins in the raw event bytes. **Trap 1**, and the only number in this file
/// that is wrong on the platform you are not currently building.
///
/// `cfg!` and not `#[cfg]`, matching [`crate::event::decode_key`]: both arms stay compiled on both
/// platforms, so the one nobody is building cannot rot into a compile error nobody sees for a month.
pub const TEXT_OFF: usize = if cfg!(feature = "hostsim") { 12 } else { 16 };

/// `SDL_LOG_CATEGORY_INPUT` / `SDL_LOG_PRIORITY_DEBUG`, from `SDL_log.h`.
#[cfg(not(any(test, feature = "test-support")))]
const SDL_LOG_CATEGORY_INPUT: std::os::raw::c_int = 3;
#[cfg(not(any(test, feature = "test-support")))]
const SDL_LOG_PRIORITY_DEBUG: std::os::raw::c_int = 2;

/// Ask LG's SDL to narrate its own keyboard lifecycle into stderr (`/tmp/plxnative-stderr.log`).
///
/// The driver already logs `[WebOSShowScreenKeyboard] called`, `[TextModelLeave] called`,
/// `[TextModelInputPanelState] called - state: %d` and `[WebOSHideScreenKeyboard] called` at
/// `INPUT`/`DEBUG`, which is below the default priority, so none of it prints. One call makes the
/// whole sequence readable without patching SDL. Cheap and permanent: a handful of lines per
/// keyboard session, on a log that is already the primary debugging surface. A no-op under a test.
pub fn trace_driver() {
    #[cfg(not(any(test, feature = "test-support")))]
    unsafe {
        sys::SDL_LogSetPriority(SDL_LOG_CATEGORY_INPUT, SDL_LOG_PRIORITY_DEBUG);
    }
}

/// `SDL_StartTextInput`: raise the system keyboard (the caller keeps its own "have I asked" edge).
pub fn start_input() {
    unsafe { SDL_StartTextInput() };
}

/// `SDL_StopTextInput`: dismiss it.
pub fn stop_input() {
    unsafe { SDL_StopTextInput() };
}

/// Does this build/firmware claim a system keyboard? `SDL_HasScreenKeyboardSupport`, which a
/// firmware answers however it likes: the caller probes it, never assumes it.
pub fn has_screen_keyboard() -> bool {
    unsafe { SDL_HasScreenKeyboardSupport() != 0 }
}

/// `SDL_GetWindowFlags` of `win`: the one way to read `SDL_WINDOW_INPUT_FOCUS`, which is what
/// decides whether the panel rises and is never reported as an error.
pub fn window_flags(win: *mut c_void) -> u32 {
    unsafe { SDL_GetWindowFlags(win) }
}

/// What the boot log's `keyboard:` line reports about the system keyboard. The fields are SDL's own
/// numbers, unconverted, so the line keeps printing exactly what it always has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyboardProbe {
    /// `SDL_HasScreenKeyboardSupport()`.
    pub support: i32,
    /// `SDL_IsTextInputActive()`.
    pub active: i32,
    /// `SDL_GetWindowFlags(win)`.
    pub wflags: u32,
}

/// Read the three facts the `keyboard:` boot line prints, in one call.
pub fn keyboard_probe(win: *mut c_void) -> KeyboardProbe {
    unsafe {
        KeyboardProbe {
            support: SDL_HasScreenKeyboardSupport(),
            active: sys::SDL_IsTextInputActive(),
            wflags: SDL_GetWindowFlags(win),
        }
    }
}

/// What [`encode_event`]'s inverse reads out of these bytes, at THIS platform's offset. Exposed so a
/// caller can log the decode without duplicating the choice of offset, which is the one thing in
/// this file that must never be written down twice.
pub fn decode(ev: &[u8]) -> String {
    decode_text_at(ev, TEXT_OFF)
}

/// The bytes an `SDL_TEXTINPUT` event carries for `text`, in whichever layout [`decode_text_at`]
/// reads — the inverse of it, and, like [`crate::inject::encode_key`] and
/// [`crate::event::decode_key`], the pair is only ever correct together. Nothing in the compiler
/// couples them; `encode_decode_round_trips` does.
///
/// It exists so this seam can be DRIVEN, by the application's `txt:` remote token. A real
/// `SDL_TEXTINPUT` needs a human at a keyboard — no trigger raises the panel, and typing into the
/// simulator means somebody physically typing — so without it nothing automated could ever put a
/// character in the search field, on either platform.
///
/// The bytes are a faithful event in THIS platform's layout, and `txt:` hands them to the decode
/// rather than to `SDL_PushEvent`: pushing a synthetic `SDL_TEXTINPUT` segfaults inside the Mac's
/// SDL, for the reason in the module doc.
pub fn encode_event(text: &str) -> [u8; 128] {
    let mut ev = [0u8; 128];
    ev[0..4].copy_from_slice(&SDL_TEXTINPUT.to_ne_bytes());
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
/// `event::decode_key` and `inject::encode_key` are pure and tested as a pair.
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
    let text = if n == TEXT_CAP {
        drop_cut_tail(text)
    } else {
        text
    };
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

    /// A synthetic event in one platform's layout: type at +0, the text at `off`, NUL-terminated
    /// if it fits. `[u8; 128]` because that is what `event::poll` reads into.
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
                assert_eq!(
                    got,
                    "y".repeat(TEXT_CAP - kept),
                    "lead {lead:#x}, {kept} of {need} bytes"
                );
                assert!(
                    !got.contains('\u{fffd}'),
                    "a cut must not invent a character"
                );
            }
            // …and the same codepoint that FITS is untouched, which is what proves the rule cuts
            // only what was actually truncated.
            let mut raw = vec![b'y'; TEXT_CAP - need];
            raw.extend_from_slice(whole.as_bytes());
            assert_eq!(
                decode_text_at(&ev_with(16, &raw), 16),
                format!("{}{whole}", "y".repeat(TEXT_CAP - need))
            );
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
        assert_eq!(
            decode_text_at(&ev_with(16, "é".as_bytes())[..17], 16),
            "\u{fffd}"
        );
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
            assert!(
                got.contains('\u{fffd}'),
                "{name} must survive as a replacement, got {got:?}"
            );
        }
    }

    /// The other edge of the same cut: a commit ORPHANED from its lead byte — the remainder of a
    /// codepoint the previous event was cut inside — must not arrive as one U+FFFD per byte, which
    /// is the row-#16 substitution again, one character later.
    #[test]
    fn a_commit_orphaned_from_its_lead_byte_is_dropped_rather_than_replaced() {
        // "🎬" is F0 9F 8E AC; cut after F0, the next commit opens with its three tail bytes.
        assert_eq!(
            decode_text_at(&ev_with(16, b"\x9f\x8e\xacstar"), 16),
            "star"
        );
        // …and a commit that is nothing BUT an orphaned remainder yields nothing, rather than one
        // replacement character per byte.
        assert_eq!(decode_text_at(&ev_with(16, b"\x80"), 16), "");
        // …and a commit that legitimately STARTS with a multi-byte character is untouched.
        assert_eq!(
            decode_text_at(&ev_with(16, "🎬star".as_bytes()), 16),
            "🎬star"
        );
    }

    /// **Capital and small letters transfer as themselves.** The panel's Shift/Caps is resolved on
    /// the television — it commits the CASED character — so the app's whole obligation is to carry
    /// the bytes through without touching them, and this is what proves it does. (A decoder that
    /// normalised case would be invisible until someone searched for a title that differs only by
    /// it.) Deliberately cites no checklist row number: `docs/lg-self-checklist.md` records no such
    /// row, and a number this repo cannot corroborate does not belong in a permanent comment.
    #[test]
    fn letter_case_transfers_exactly_as_the_panel_committed_it() {
        for s in [
            "A", "a", "Wallace", "WALLACE", "wallace", "WaLlAcE", "Ç", "ç", "Ä", "ä",
        ] {
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
            ("　", " "), // U+3000 IDEOGRAPHIC SPACE
            ("“", "\""),
            ("’", "'"),
            ("－", "-"),
        ] {
            let got = decode_text_at(&ev_with(16, typed.as_bytes()), 16);
            assert_eq!(
                got, typed,
                "the character pressed must be the character that arrives"
            );
            assert_eq!(decode_text_at(&encode_event(typed), TEXT_OFF), typed);
        }
        // The ASCII originals still arrive as themselves — nothing here rewrites in either
        // direction. `\` is the one the row calls out, and the rest are what a URL layer or a
        // shell would eat if anything on the path were not byte-transparent.
        for s in [
            "\\", "/", "&", "%", "+", "#", "?", "=", "\"", "'", "<", ">", " ", "\t",
        ] {
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
        assert_eq!(
            decode_text_at(&ev_with(16, "Amélie".as_bytes()), 16),
            "Amélie"
        );
        assert_eq!(
            decode_text_at(&ev_with(16, "千と千尋".as_bytes()), 16),
            "千と千尋"
        );
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
        assert_eq!(
            decode_text_at(&encode_event(&long), TEXT_OFF),
            "x".repeat(TEXT_CAP - 1)
        );
        // 16 CJK codepoints = 48 bytes: the 31-byte limit falls mid-codepoint, so the cut must
        // back up to 30 bytes (10 characters) rather than emit a replacement character.
        let cjk = "千".repeat(16);
        let got = decode_text_at(&encode_event(&cjk), TEXT_OFF);
        assert_eq!(got, "千".repeat(10));
        assert!(
            !got.contains('\u{fffd}'),
            "the cut must not split a codepoint"
        );
    }
}
