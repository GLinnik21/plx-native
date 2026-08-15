//! Layout + input + animation constants, mirroring src/ui_home.h and ui_home.c.
//! Single source so the hand-tuned pixel offsets can't drift between widgets.
//!
//! The input half is the keycodes AND the vocabulary built on them: [`is_ok`]/[`is_back`] and
//! [`classify`], which resolves a raw `(sym, wcode)` pair to the one [`Key`] `app.rs`'s ladder
//! dispatches on. A spelling belongs here with the code it matches, and its test with it: the
//! ladder itself sits inside the SDL event loop, where no host test reaches it (which is the
//! premise `tools/keytable.py` was written on).
#![allow(dead_code)]
use std::os::raw::c_uint;

pub const CARD_W: f32 = 250.0;
pub const CARD_H: f32 = 375.0;
pub const GAP: f32 = 30.0;
pub const MARGIN_X: f32 = 90.0;
pub const ROW_TITLE_H: f32 = 30.0;
pub const ROW_PITCH: f32 = CARD_H + ROW_TITLE_H + 144.0; // 549: room for the shelf title above + the focused card's title AND caption below (clears the next shelf's title)
/// Hub-title cap top above the shelf's `row_y` origin — the heading draws at `row_y − TITLE_DY`,
/// minus whatever `CardRow::lift` has raised it by. Named because it is a LAYOUT relationship two
/// other constants here are derived against ([`CARD_DY`]'s air, [`GRID_TOP_Y`]'s clearance under the
/// profile chip) and because the shelf heading is now a multi-run flow rather than one `p.text`.
pub const TITLE_DY: f32 = 34.0;
/// Card top below the shelf's `row_y` origin (the hub title draws at `row_y − `[`TITLE_DY`]): the air
/// between a section title and its posters, held on magnification too because `title_lift` raises the
/// title by the same amount the popped card's top rises.
pub const CARD_DY: f32 = 26.0;
pub const CONTENT_Y: f32 = 200.0;
pub const GLOW_PAD: f32 = 48.0;
pub(crate) use crate::surface::{LOGICAL_H as SCR_H, LOGICAL_W as SCR_W};

// hero <-> grid continuum
/// Shelf top in hero view. Re-derived once the peek row stopped magnifying its focused cell: the
/// peek used to be judged off that popped tile, whose 1.09 scale about its centre lifted its top
/// edge `CARD_H * 0.09 / 2 ≈ 17px` above every other card in the row. Un-popping the row dropped
/// the whole shelf by that much, so the peek is 17px shallower here to keep the composition the
/// hero view was tuned to (828 → 811; card top = `PEEK_Y + CARD_DY` = 837, as the popped one was).
pub const PEEK_Y: f32 = 811.0;
// shelf top in grid view — leaves the first hub title (row_y − TITLE_DY, lifted up to ~10 more when its
// leftmost card magnifies) a clear space::MD under the profile chip (bottom edge 108)
pub const GRID_TOP_Y: f32 = 176.0;

// SDL keycodes (scancode | SDLK_SCANCODE_MASK, or ASCII)
pub const SDLK_RIGHT: c_uint = 79 | (1 << 30);
pub const SDLK_LEFT: c_uint = 80 | (1 << 30);
pub const SDLK_DOWN: c_uint = 81 | (1 << 30);
pub const SDLK_UP: c_uint = 82 | (1 << 30);
pub const SDLK_RETURN: c_uint = 13;
pub const SDLK_KP_ENTER: c_uint = 88 | (1 << 30);
pub const SDLK_SELECT: c_uint = 77 | (1 << 30);
pub const SDLK_ESCAPE: c_uint = 27;
pub const SDLK_PAGEUP: c_uint = 75 | (1 << 30);
pub const SDLK_PAGEDOWN: c_uint = 78 | (1 << 30);
/// Backspace and Clear — **the system keyboard's own two edit keys**, and the reason they are named
/// here rather than left as literals in the one screen that reads text.
///
/// The television's on-screen panel does not edit the field itself. It commits printable characters
/// as `SDL_TEXTINPUT` and then forwards every EDIT intent to the app as an ordinary key, expecting
/// the app to own the text: `◀`/`▶` arrive as [`SDLK_LEFT`]/[`SDLK_RIGHT`] and mean *move the
/// caret*, its delete key arrives as [`SDLK_BACKSPACE`], and its **Clear all** button arrives as
/// [`SDLK_CLEAR`] (wcode 156, sym `0x4000009C` — device-measured 2026-08-15). A screen that ignores
/// them has a keyboard whose buttons visibly do nothing, which is exactly how this shipped.
pub const SDLK_BACKSPACE: c_uint = 8;
pub const SDLK_CLEAR: c_uint = 156 | (1 << 30);
/// Magic-Remote CH▲/CH▼ rocker — webOS keyCodes 33/34 (page the Library grid). Matched
/// alongside the SDLK_PAGE* syms; verify the raw wcodes in the event log on a new remote.
pub const WCODE_CH_UP: c_uint = 33;
pub const WCODE_CH_DOWN: c_uint = 34;
/// Magic-Remote transport keys. The ONE home for these wcodes: [`classify`] below resolves them
/// for the key handler, and `app.rs`'s remote-injection token map and desktop-keyboard stand-in
/// both build presses from these names.
pub const WCODE_PAUSE: c_uint = 72;
pub const WCODE_STOP: c_uint = 413;
/// The other codes the PAUSE and PLAY arms accept beside [`WCODE_PAUSE`]/[`WCODE_PLAY`], each in
/// EITHER field. Named here because [`classify`] is where they are matched now; they came over as
/// bare literals from the two `app.rs` arms that spelled them. The sets are the ones
/// `docs/ui-viewtree-plan.md` §C carries as an invariant — PAUSE 72/415, PLAY 450/19/402.
pub const WCODE_PAUSE_ALT: c_uint = 415;
pub const WCODE_PLAY_ALT_A: c_uint = 19;
pub const WCODE_PLAY_ALT_B: c_uint = 402;
/// The Magic Remote reporting that its on-screen pointer AUTO-HID. The ladder's arm for it has an
/// empty body — the press is swallowed there rather than reaching the arms below it.
pub const WCODE_POINTER_HIDDEN: c_uint = 0x1e4;
/// webOS BACK. This Magic Remote sends 482 (0x1E2); 461 is kept for other remotes. Named here
/// because [`is_back`] calls itself the ONE BACK predicate, so the code it matches lives with it
/// rather than being re-inlined by whoever needs to synthesize a press.
pub const WCODE_BACK: c_uint = 482;
pub const WCODE_PLAY: c_uint = 450;
/// Magic-Remote D-pad LEFT/RIGHT — the ALTERNATE codes that arrive beside the ordinary
/// [`SDLK_LEFT`]/[`SDLK_RIGHT`] syms, matched in BOTH fields (as a `sym` and as a `wcode`). Named
/// here rather than repeated at each of the sites in `app.rs` that carried them raw, and for the
/// same reason [`WCODE_BACK`] is: a code belongs with the predicate that matches it, which for
/// these is [`classify`]. The press it resolves to carries `alt: true`, which is what keeps the
/// two horizontal tests in the ladder apart — see [`Key::Left`].
pub const WCODE_DPAD_LEFT: c_uint = 412;
pub const WCODE_DPAD_RIGHT: c_uint = 417;

/// OK/confirm press — RETURN, keypad ENTER, or the remote's SELECT. The ONE OK predicate
/// (app.rs + the login/profiles screens all route through it).
#[inline]
pub fn is_ok(sym: c_uint) -> bool {
    sym == SDLK_RETURN || sym == SDLK_KP_ENTER || sym == SDLK_SELECT
}
/// webOS BACK — ESC / 'q' (dev keyboards) or the remote BACK wcodes (this Magic Remote sends
/// 482 = 0x1E2; 461 kept for other remotes). The ONE BACK predicate.
#[inline]
pub fn is_back(sym: c_uint, wcode: c_uint) -> bool {
    sym == SDLK_ESCAPE || sym == 'q' as c_uint || wcode == 461 || wcode == WCODE_BACK
}

/// One press, in the vocabulary `app.rs`'s key ladder dispatches on — resolved by [`classify`]
/// from the TWO independent fields a webOS `SDL_KeyboardEvent` carries (the SDL `sym` at +24 and
/// the webOS `wcode` at +20; the raw-offset reading is a gotcha in the root `CLAUDE.md`).
///
/// [`Key::Other`] is every press `classify` does not name, which includes spellings the ladder
/// still tests by hand where it needs them: the CH▲/CH▼ rocker ([`WCODE_CH_UP`]/[`WCODE_CH_DOWN`]
/// and the `SDLK_PAGE*` syms, in the Library's paging arm) and the system keyboard's edit keys
/// ([`SDLK_BACKSPACE`]/[`SDLK_CLEAR`], read inside the Search screen).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Up,
    Down,
    /// LEFT and RIGHT carry a flag because the ladder asks about them in two ways that accept
    /// DIFFERENT sets, and the difference is behaviour rather than an accident of spelling.
    /// `alt` is `false` when the press arrived as the plain [`SDLK_LEFT`]/[`SDLK_RIGHT`] sym, and
    /// `true` when it arrived only as the Magic Remote's [`WCODE_DPAD_LEFT`]/[`WCODE_DPAD_RIGHT`]
    /// (in either field). The non-player four-way nav dispatch matches `alt: false` alone, so an
    /// alternate-code LEFT on Home reaches no arm that acts on it; the player's scrub arm and the
    /// Chapters strip match both. Preserve that asymmetry — it is what the flag is for.
    Left {
        alt: bool,
    },
    Right {
        alt: bool,
    },
    Ok,
    Back,
    Play,
    Pause,
    Stop,
    /// The Magic Remote reporting that its pointer auto-hid ([`WCODE_POINTER_HIDDEN`]).
    PointerHidden,
    Other,
}

/// Resolve a raw `(sym, wcode)` pair to the one [`Key`] the ladder acts on. Pure — no SDL, no
/// globals — which is what lets the ladder's vocabulary be graded by the host suite at all.
///
/// **The precedence is deliberate, because the two fields are independent and can both be
/// filled.** The `sym` field is asked FIRST and settles the four directions: a press carrying
/// `SDLK_LEFT` in `sym` and [`WCODE_DPAD_LEFT`] in `wcode` is a plain `Left { alt: false }`, which
/// is what keeps the four-way nav dispatch matching it — classify that pair as the ALTERNATE
/// spelling instead and Home's navigation stops answering it. Only a press whose `sym` names none
/// of the four plain direction syms can come back `alt: true`.
///
/// After the directions the order is the ladder's own — pointer, OK, PAUSE, PLAY, STOP, BACK. The
/// two alternate D-pad codes are resolved up here beside the plain ones rather than down where the
/// scrub arm tests them, so a pair carrying one of those AND a transport code resolves as the
/// direction rather than as the transport key. Both fields describe ONE press — `app.rs`'s
/// `decode_key` reads them off a single event and its `remote_token_key` builds each synthetic
/// pair from a single token — so a pair naming two DIFFERENT keys is not a shape either path is
/// built to carry; the order above settles it rather than leaving it to whichever test came first.
pub fn classify(sym: c_uint, wcode: c_uint) -> Key {
    match sym {
        SDLK_UP => return Key::Up,
        SDLK_DOWN => return Key::Down,
        SDLK_LEFT => return Key::Left { alt: false },
        SDLK_RIGHT => return Key::Right { alt: false },
        _ => {}
    }
    if sym == WCODE_DPAD_LEFT || wcode == WCODE_DPAD_LEFT {
        return Key::Left { alt: true };
    }
    if sym == WCODE_DPAD_RIGHT || wcode == WCODE_DPAD_RIGHT {
        return Key::Right { alt: true };
    }
    if wcode == WCODE_POINTER_HIDDEN {
        return Key::PointerHidden;
    }
    if is_ok(sym) {
        return Key::Ok;
    }
    // Field for field as the two transport arms spelled them, asymmetries included: [`WCODE_PAUSE`]
    // and [`WCODE_PLAY`] are taken in the `wcode` field ALONE, while their alternates — and
    // [`WCODE_STOP`] — are taken in either. Widening one of those is a behaviour change, not a
    // tidy-up.
    if wcode == WCODE_PAUSE || sym == WCODE_PAUSE_ALT || wcode == WCODE_PAUSE_ALT {
        return Key::Pause;
    }
    if wcode == WCODE_PLAY
        || sym == WCODE_PLAY_ALT_A
        || wcode == WCODE_PLAY_ALT_A
        || sym == WCODE_PLAY_ALT_B
        || wcode == WCODE_PLAY_ALT_B
    {
        return Key::Play;
    }
    if sym == WCODE_STOP || wcode == WCODE_STOP {
        return Key::Stop;
    }
    if is_back(sym, wcode) {
        return Key::Back;
    }
    Key::Other
}

// spring stiffnesses (from ui_home.c, redistributed 1:1 to their owning views)
pub const K_SCALE: f32 = 320.0;
pub const K_SCROLL: f32 = 170.0;
pub const K_SNAP: f32 = 200.0;

#[cfg(test)]
mod tests {
    use super::*;

    /// The two horizontal tests the ladder asks, copied here as the `matches!` patterns `app.rs`
    /// spells at those arms — so the asymmetry between them is asserted rather than described.
    /// `nav4` is the non-player four-way nav dispatch; `lr` is the player's scrub arm (and the
    /// Chapters strip, and the scrub's key-up/auto-repeat companions).
    fn nav4(k: Key) -> bool {
        matches!(k, Key::Up | Key::Down | Key::Left { alt: false } | Key::Right { alt: false })
    }
    fn lr(k: Key) -> bool {
        matches!(k, Key::Left { .. } | Key::Right { .. })
    }

    #[test]
    fn every_spelling_lands_on_its_own_key() {
        let cases: [(c_uint, c_uint, Key); 27] = [
            (SDLK_UP, 0, Key::Up),
            (SDLK_DOWN, 0, Key::Down),
            (SDLK_LEFT, 0, Key::Left { alt: false }),
            (SDLK_RIGHT, 0, Key::Right { alt: false }),
            // the alternate D-pad codes, in each field on their own
            (WCODE_DPAD_LEFT, 0, Key::Left { alt: true }),
            (0, WCODE_DPAD_LEFT, Key::Left { alt: true }),
            (WCODE_DPAD_RIGHT, 0, Key::Right { alt: true }),
            (0, WCODE_DPAD_RIGHT, Key::Right { alt: true }),
            // OK: RETURN, keypad ENTER, the remote's SELECT
            (SDLK_RETURN, 0, Key::Ok),
            (SDLK_KP_ENTER, 0, Key::Ok),
            (SDLK_SELECT, 0, Key::Ok),
            // BACK: the two dev-keyboard syms and the two remote wcodes
            (SDLK_ESCAPE, 0, Key::Back),
            ('q' as c_uint, 0, Key::Back),
            (0, 461, Key::Back),
            (0, WCODE_BACK, Key::Back),
            (0, WCODE_PAUSE, Key::Pause),
            (WCODE_PAUSE_ALT, 0, Key::Pause),
            (0, WCODE_PAUSE_ALT, Key::Pause),
            (0, WCODE_PLAY, Key::Play),
            (WCODE_PLAY_ALT_A, 0, Key::Play),
            (0, WCODE_PLAY_ALT_A, Key::Play),
            (WCODE_PLAY_ALT_B, 0, Key::Play),
            (0, WCODE_PLAY_ALT_B, Key::Play),
            (WCODE_STOP, 0, Key::Stop),
            (0, WCODE_STOP, Key::Stop),
            (0, WCODE_POINTER_HIDDEN, Key::PointerHidden),
            (0, 0, Key::Other),
        ];
        for (sym, wcode, want) in cases {
            assert_eq!(classify(sym, wcode), want, "sym={sym} wcode={wcode}");
        }
    }

    /// The spellings the ladder still tests by hand, which must NOT be swallowed by a named key:
    /// the CH▲/CH▼ rocker and the system keyboard's two edit keys (the pairs `remote_token_key`
    /// builds for them).
    #[test]
    fn the_arms_that_still_spell_their_own_keys_classify_as_other() {
        for (sym, wcode) in [
            (SDLK_PAGEUP, WCODE_CH_UP),
            (SDLK_PAGEDOWN, WCODE_CH_DOWN),
            (SDLK_BACKSPACE, 42),
            (SDLK_CLEAR, 156),
        ] {
            assert_eq!(classify(sym, wcode), Key::Other, "sym={sym} wcode={wcode}");
        }
    }

    /// The two horizontal tests accept different sets, and that is the whole reason `alt` exists:
    /// an alternate-code LEFT reaches the player's scrub and not the four-way nav dispatch.
    #[test]
    fn the_two_horizontal_tests_accept_different_sets() {
        for (plain, alt) in [
            (classify(SDLK_LEFT, 0), classify(0, WCODE_DPAD_LEFT)),
            (classify(SDLK_RIGHT, 0), classify(0, WCODE_DPAD_RIGHT)),
        ] {
            assert!(nav4(plain), "the plain sym is what the nav dispatch takes");
            assert!(lr(plain), "…and the scrub arm takes it too");
            assert!(!nav4(alt), "the four-way nav dispatch takes the plain syms only");
            assert!(lr(alt), "the scrub arm takes the alternate codes as well");
        }
    }

    /// Both fields can name the same key on one press, and then the PLAIN sym wins. Collapse that
    /// pair to the alternate spelling and `nav4` stops matching it — which is Home's navigation,
    /// for an event that carries a perfectly ordinary `SDLK_LEFT`.
    #[test]
    fn a_plain_sym_beside_its_own_alternate_code_stays_plain() {
        let l = classify(SDLK_LEFT, WCODE_DPAD_LEFT);
        let r = classify(SDLK_RIGHT, WCODE_DPAD_RIGHT);
        assert_eq!(l, Key::Left { alt: false });
        assert_eq!(r, Key::Right { alt: false });
        assert!(nav4(l) && nav4(r));
    }

    /// UP and DOWN have no alternate spelling in this vocabulary, so their identity is the `sym`
    /// field alone — whatever else rides in `wcode`.
    #[test]
    fn the_vertical_syms_ignore_the_wcode_field() {
        for wcode in [0, WCODE_PAUSE, WCODE_BACK, WCODE_POINTER_HIDDEN] {
            assert_eq!(classify(SDLK_UP, wcode), Key::Up, "wcode={wcode}");
            assert_eq!(classify(SDLK_DOWN, wcode), Key::Down, "wcode={wcode}");
        }
    }
}
