//! Layout + input + animation constants, mirroring src/ui_home.h and ui_home.c.
//! Single source so the hand-tuned pixel offsets can't drift between widgets.
//!
//! The input half is the keycodes AND the vocabulary built on them: [`is_ok`]/[`is_back`] and
//! [`classify`], which resolves a raw `(sym, wcode)` pair to the one [`Key`] `app.rs`'s ladder
//! dispatches on. A spelling belongs here with the code it matches, and its test with it: the
//! ladder itself sits inside the SDL event loop, where no host test reaches it (which is the
//! premise `tools/keytable.py` was written on).
#![allow(dead_code)]
use std::os::raw::{c_int, c_uint};

pub const CARD_W: f32 = 250.0;
pub const CARD_H: f32 = 375.0;
pub const GAP: f32 = 30.0;
/// **The safe area's LEFT/RIGHT keep-out — 5% of [`SCR_W`], and the reason it is 96 rather than 90.**
///
/// A television overscans: the panel shows less than the frame it is handed, by a margin the set
/// decides and the app cannot query. LG's App Self Checklist item #2 asks that *"buttons, texts and
/// logos on the main page are placed within the overscan frame"*, and the broadcast convention that
/// phrase names is **5% of each edge** — 96px horizontally and [`MARGIN_Y`] 54px vertically on this
/// 1920×1080 canvas. (LG's current *developer* guide states a laxer 20px; the 5% frame is the
/// stricter of the two published numbers and is the one this app is graded against, because a
/// margin that satisfies 5% satisfies 20px and not the other way round.)
///
/// It was **90** until 2026-08-23 — 4.7%, i.e. six pixels INSIDE the exclusion zone, on every screen
/// in the app at once. `docs/lg-self-checklist.md` recorded that as passing, reading "4.7% against a
/// 5% frame" as clearance when it is the opposite: a smaller margin puts content NEARER the edge.
/// Nothing had ever measured it, which is why
/// `tests::no_required_content_enters_the_safe_area_exclusion_zone` now does — it grades the
/// composed rects, not this literal, so a future audit is free to move this number again without
/// rewriting the test.
pub const MARGIN_X: f32 = 96.0;
/// **The safe area's TOP/BOTTOM keep-out — 5% of [`SCR_H`].** [`MARGIN_X`]'s missing twin: until
/// 2026-08-23 the vertical bound was simply unstated, so nothing in the tree bounded it and nothing
/// could check it. It bit in four places — the shared top bar sat at y=44, the detail page's pinned
/// compact title at 40 (its tallest logo reaching 22), and the Home/Library/Person scroll reveals
/// left 24/16/40px under a focused card.
///
/// Smaller than [`MARGIN_X`] because it is 5% of the SHORTER axis; the frame is a percentage of each
/// dimension, not one distance.
pub const MARGIN_Y: f32 = 54.0;
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

/// **The safe area itself** — the box every piece of REQUIRED content has to fit inside, as one
/// value so no caller re-derives it from the two margins and gets a sign wrong.
///
/// "Required" is the whole content of the rule and the reason this is a predicate rather than a
/// clip: a full-bleed hero backdrop, a shelf peeking off the bottom edge, a scrim, the page ground —
/// all of those are *supposed* to reach the panel edge, and clipping them would be the bug. What
/// must be inside is what the viewer has to read or press: text, controls, marks, logos.
pub const SAFE: crate::ui::Rect =
    crate::ui::Rect::new(MARGIN_X, MARGIN_Y, SCR_W - 2.0 * MARGIN_X, SCR_H - 2.0 * MARGIN_Y);

/// Is `r` wholly inside [`SAFE`]? The one test the audit's table asks, per rect.
///
/// A hair of tolerance (`EPS`) because several of the rects handed here are the result of an f32
/// derivation that lands on the boundary exactly — `SCR_W - MARGIN_X - w` then `+ w` is not bit-for-
/// bit `SCR_W - MARGIN_X` — and a control flush against the frame is compliant, not a violation.
#[inline]
pub fn inside_safe(r: crate::ui::Rect) -> bool {
    const EPS: f32 = 0.01;
    r.x >= SAFE.x - EPS
        && r.y >= SAFE.y - EPS
        && r.x + r.w <= SAFE.x + SAFE.w + EPS
        && r.y + r.h <= SAFE.y + SAFE.h + EPS
}

// hero <-> grid continuum
/// Shelf top in hero view. Re-derived once the peek row stopped magnifying its focused cell: the
/// peek used to be judged off that popped tile, whose 1.09 scale about its centre lifted its top
/// edge `CARD_H * 0.09 / 2 ≈ 17px` above every other card in the row. Un-popping the row dropped
/// the whole shelf by that much, so the peek is 17px shallower here to keep the composition the
/// hero view was tuned to (828 → 811; card top = `PEEK_Y + CARD_DY` = 837, as the popped one was).
pub const PEEK_Y: f32 = 811.0;
// shelf top in grid view — leaves the first hub title (row_y − TITLE_DY, lifted up to ~10 more when its
// leftmost card magnifies) a clear space::MD under the profile chip (bottom edge 126).
// 176 until 2026-08-23: it moved with the top bar, which dropped 18px so its track clears MARGIN_Y
// (`widgets::TOP_BAR_Y`). The clearance under the chip is what this number IS, so it follows the bar
// rather than staying put and letting the heading crowd it.
pub const GRID_TOP_Y: f32 = 194.0;

// SDL keycodes (scancode | SDLK_SCANCODE_MASK, or ASCII)
pub const SDLK_RIGHT: c_uint = 79 | (1 << 30);
pub const SDLK_LEFT: c_uint = 80 | (1 << 30);
pub const SDLK_DOWN: c_uint = 81 | (1 << 30);
pub const SDLK_UP: c_uint = 82 | (1 << 30);
pub const SDLK_RETURN: c_uint = 13;
pub const SDLK_KP_ENTER: c_uint = 88 | (1 << 30);
/// **The name is wrong and the value is left alone deliberately: 77 is `SDL_SCANCODE_END`.**
/// `SDL_SCANCODE_SELECT` is 119; 77 sits in the `INSERT`/`HOME`/`PAGEUP`/`DELETE`/`END`/`PAGEDOWN`
/// run at 73–78, and the television's own evdev table confirms it — entry 107 (`KEY_END`) is what
/// produces 77. So [`is_ok`] accepts a keyboard's **End** key and would NOT accept a remote's real
/// SELECT. Renaming it is the honest fix and is not this change's to make: the name is read from
/// `ui/profiles.rs`'s test, and repointing the value would make `is_ok` answer a different key on
/// no evidence that any remote sends 119 either. Recorded in `docs/remote-keys.md` §9, to be
/// settled by the capture in §7 — with everything else of its kind, in one pass.
pub const SDLK_SELECT: c_uint = 77 | (1 << 30);
pub const SDLK_ESCAPE: c_uint = 27;
pub const SDLK_PAGEUP: c_uint = 75 | (1 << 30);
pub const SDLK_PAGEDOWN: c_uint = 78 | (1 << 30);
/// Backspace and Clear — **the system keyboard's own two edit keys**, and the reason they are named
/// here rather than left as literals in the screens that read text.
///
/// The television's on-screen panel does not edit the field itself. It commits printable characters
/// as `SDL_TEXTINPUT` and then forwards every EDIT intent to the app as an ordinary key, expecting
/// the app to own the text: `◀`/`▶` arrive as [`SDLK_LEFT`]/[`SDLK_RIGHT`] and mean *move the
/// caret*, its delete key arrives as [`SDLK_BACKSPACE`], and its **Clear all** button arrives as
/// [`SDLK_CLEAR`] (wcode 156, sym `0x4000009C` — device-measured 2026-08-15). A screen that ignores
/// them has a keyboard whose buttons visibly do nothing, which is exactly how this shipped.
pub const SDLK_BACKSPACE: c_uint = 8;
pub const SDLK_CLEAR: c_uint = 156 | (1 << 30);
// **`WCODE_CH_UP` / `WCODE_CH_DOWN` = 33 / 34 USED TO LIVE HERE, and they were the DIGITS.**
// They are deleted rather than kept-but-unused, and this note stays because the next reader will
// otherwise re-derive them from the same public table that produced them.
//
// They entered as "Magic-Remote CH▲/CH▼ rocker — webOS keyCodes 33/34", carrying the caveat
// *"verify the raw wcodes in the event log on a new remote"* — a caveat nobody ever spent. 33/34
// really are ChannelUp/ChannelDown, but in the CEA-2014-A / LG **web** keyCode namespace, where
// they are the browser's PageUp/PageDown. This app receives native SCANCODES: the same namespace
// error that retired 412/417 (see [`WCODE_REWIND`]). In THAT namespace 33 and 34 are
// `SDL_SCANCODE_4` and `SDL_SCANCODE_5`, produced by evdev 5 `KEY_4` and evdev 6 `KEY_5` — which
// is a fact about the television, readable offline in its own table (`libSDL2-2.0.so.0.4.1`, file
// offset `0x92840`, entries 5 and 6), not an inference.
//
// So [`page_dir`] answered a NUMBER KEY: pressing `5` on a remote keypad paged the Library grid.
// The real rocker is [`WCODE_CH_UP_KEY`]/[`WCODE_CH_DOWN_KEY`] (300/301) and was already bound
// beside them, so nothing is lost. Those two do NOT inherit the freed names: a constant that
// silently changes value is how a doc or a log predating the change comes to read as the opposite
// of what it says.
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
/// **REWIND and FAST-FORWARD — the codes that took over the `alt: true` machinery.**
///
/// This pair used to be `WCODE_DPAD_LEFT`/`WCODE_DPAD_RIGHT` = 412/417, described as "the
/// alternate codes that arrive beside the ordinary [`SDLK_LEFT`]/[`SDLK_RIGHT`] syms". **That was
/// never true and those codes have never fired.** They entered in the initial commit as bare
/// literals with no note, next to a measured pair that carries an explicit *"verified from the raw
/// key log"* — and 412/413/415/417 are the CEA-2014-A / LG **web** keyCodes for Rewind/Stop/Play/
/// Fast-forward, a different namespace from the native scancodes this app actually receives (under
/// it BACK would be 461, and BACK measures 482).
///
/// Settled two ways on 2026-08-22. LG's own evdev->scancode table, at file offset `0x92840` of the
/// television's `libSDL2-2.0.so.0.4.1`, produces 412 and 417 only from evdev 524 and 556 — which
/// `linux/input.h` does not name at all — while the D-pad is evdev 105/106/103/108 -> **80/79/82/
/// 81**. And 336 real key lines captured off the dev set's own remote show exactly 80/79/81/82 for
/// the D-pad, 40 for OK, 482 for BACK, and **no 412 or 417 at any point**.
///
/// So the `alt: true` DESIGN was right and only its codes were wrong: a press that seeks in the
/// player and does nothing on Home is precisely what a transport key should be, and that is what
/// these two now carry. From the same table: evdev 168 `KEY_REWIND` -> 452, evdev 208
/// `KEY_FASTFORWARD` -> 451.
pub const WCODE_REWIND: c_uint = 452;
pub const WCODE_FASTFORWARD: c_uint = 451;
/// **STOP, as the television actually spells it.** [`WCODE_STOP`] (413) comes from unnamed evdev
/// 534 in the same table; the real ones are evdev 128 `KEY_STOP` -> 120 and evdev 166 `KEY_STOPCD`
/// -> 260. 413 is KEPT beside them rather than deleted — unlike 412/417 there is no positive
/// evidence it never fires, only that nothing in `linux/input.h` names its producer.
pub const WCODE_STOP_KEY: c_uint = 120;
pub const WCODE_STOP_CD: c_uint = 260;
/// evdev 164 `KEY_PLAYPAUSE` -> 261. A TOGGLE, which is why it gets its own [`Key`] variant rather
/// than joining [`WCODE_PLAY`] or [`WCODE_PAUSE`]: `key_play` un-pauses and `key_pause` pauses, and
/// one key that does whichever is needed cannot be either of them.
pub const WCODE_PLAYPAUSE: c_uint = 261;
/// evdev 174 `KEY_EXIT` -> 505. LG's checklist item 38 wants the app terminated on this press.
pub const WCODE_EXIT: c_uint = 505;
/// **The channel rocker, and since 2026-08-23 the ONLY spelling of it** — evdev 402/403
/// `KEY_CHANNELUP`/`KEY_CHANNELDOWN` -> **300/301** in the television's own table. The digits
/// 33/34 used to sit beside these in [`page_dir`], described as the rocker and hedged as "almost
/// certainly NOT"; the note where they were deleted, a few dozen lines up, is why. The `_KEY`
/// suffix is evdev's (`KEY_CHANNELUP`) and is kept precisely so these names stay distinct from the
/// retired pair.
pub const WCODE_CH_UP_KEY: c_uint = 300;
pub const WCODE_CH_DOWN_KEY: c_uint = 301;

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
/// still tests by hand where it needs them: the CH▲/CH▼ rocker ([`WCODE_CH_UP_KEY`]/
/// [`WCODE_CH_DOWN_KEY`] and the `SDLK_PAGE*` syms, in the Library's paging arm via [`page_dir`]),
/// the system keyboard's edit keys ([`SDLK_BACKSPACE`]/[`SDLK_CLEAR`], read by the two screens that
/// own a text field — `ui::search::key` and `ui::library::key`, the Sources panel's address row,
/// each gated on its own editing state and each returning whether it took the press, so a third
/// field joins the same door rather than adding one) and the digits the who's-watching PIN keypad
/// types from. **So `Other` does NOT mean unbound and must never be made inert** — [`is_bound`] is
/// the predicate that answers *is this press one the app binds at all*, and it is a strict superset
/// of `classify != Other`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Up,
    Down,
    /// LEFT and RIGHT carry a flag because the ladder asks about them in two ways that accept
    /// DIFFERENT sets, and the difference is behaviour rather than an accident of spelling.
    /// `alt` is `false` when the press arrived as the plain [`SDLK_LEFT`]/[`SDLK_RIGHT`] sym, and
    /// `true` when it arrived only as a TRANSPORT key, [`WCODE_REWIND`]/[`WCODE_FASTFORWARD`]
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
    /// One key that does whichever of the two is needed — evdev `KEY_PLAYPAUSE`. Its own variant
    /// because `key_play` and `key_pause` are each one direction of the toggle.
    PlayPause,
    Stop,
    /// The remote's EXIT key. Terminates the app outright rather than raising the exit alert:
    /// the alert exists so BACK at Home's root cannot quit by accident, and a key labelled EXIT
    /// carries no such ambiguity.
    Exit,
    /// The Magic Remote reporting that its pointer auto-hid ([`WCODE_POINTER_HIDDEN`]).
    PointerHidden,
    Other,
}

/// Resolve a raw `(sym, wcode)` pair to the one [`Key`] the ladder acts on. Pure — no SDL, no
/// globals — which is what lets the ladder's vocabulary be graded by the host suite at all.
///
/// **The precedence is deliberate, because the two fields are independent and can both be
/// filled.** The `sym` field is asked FIRST and settles the four directions: a press carrying
/// `SDLK_LEFT` in `sym` and [`WCODE_REWIND`] in `wcode` is a plain `Left { alt: false }`, which
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
    if sym == WCODE_REWIND || wcode == WCODE_REWIND {
        return Key::Left { alt: true };
    }
    if sym == WCODE_FASTFORWARD || wcode == WCODE_FASTFORWARD {
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
    if wcode == WCODE_PLAYPAUSE || sym == WCODE_PLAYPAUSE {
        return Key::PlayPause;
    }
    // The codes settled from LG's table are matched in `wcode` ALONE, which is the field the
    // television fills for them; only the legacy web-namespace spellings are also tested as a `sym`,
    // because that is how they arrived and narrowing them would be the behaviour change the comment
    // above warns about in the other direction.
    if matches!(wcode, WCODE_STOP | WCODE_STOP_KEY | WCODE_STOP_CD) || sym == WCODE_STOP {
        return Key::Stop;
    }
    if wcode == WCODE_EXIT || sym == WCODE_EXIT {
        return Key::Exit;
    }
    if is_back(sym, wcode) {
        return Key::Back;
    }
    Key::Other
}

/// **Which way the Library grid pages, if this press pages it at all.** `Some(-1)` up, `Some(1)`
/// down, `None` for everything else.
///
/// One predicate because the set was being spelled TWICE, in two shapes: a six-term guard in
/// `app.rs`'s ladder and a three-term direction test in `key_library_page`, where the second had to
/// stay a subset of the first and nothing checked that it did. Adding the real channel rocker
/// (300/301 — see [`WCODE_CH_UP_KEY`]) beside the digits meant editing both, which is the shape
/// that goes wrong on the third code. Here it is one edit, next to [`classify`], and the test table
/// below can see it.
///
/// **It is a separate predicate from [`classify`] and not a [`Key`] variant, which is the thing to
/// understand before "making [`Key::Other`] inert".** A paging press classifies as `Other` and is
/// then taken by the ladder's own Library arm — so `Other` is not a synonym for *unbound*, and
/// [`is_bound`] is the predicate that answers that question.
#[inline]
pub fn page_dir(sym: c_uint, wcode: c_uint) -> Option<c_int> {
    if sym == SDLK_PAGEUP || wcode == WCODE_CH_UP_KEY {
        return Some(-1);
    }
    if sym == SDLK_PAGEDOWN || wcode == WCODE_CH_DOWN_KEY {
        return Some(1);
    }
    None
}

/// **Is this press one the app binds ANYWHERE — the whole key map, as one predicate.**
///
/// It exists for a single caller and a single rule: `app.rs`'s `begin_fresh_press` runs for EVERY
/// fresh press, *before* the ladder has decided whether anything takes it, and two of the things it
/// does are global — it un-dismisses the player HUD and it aborts an armed tvOS click. So an
/// unsupported key (LG checklist item 40: the colour buttons, GUIDE, INFO, the number pad on a
/// screen with no keypad, a universal remote's whole extra half) woke the transport and cancelled a
/// press in flight. The invariant is *a press consumed by neither a route-specific handler nor the
/// global key ladder must produce no global side effect*, and this is its guard.
///
/// **Four sources, because the map has four and not one:**
/// 1. [`classify`] — every named [`Key`].
/// 2. [`page_dir`] — the Library pager, a SEPARATE predicate (see its doc).
/// 3. [`SDLK_BACKSPACE`] / [`SDLK_CLEAR`] — the television keyboard's own edit keys, read inside
///    the Search screen (`ui::search::key`), which the classifier never sees.
/// 4. An ASCII digit **in `sym`** — the who's-watching PIN keypad types straight from the remote's
///    number buttons (`ui::profiles`' own `digit_of`, which owns that behaviour).
///
/// **The `sym` field ONLY, and that is a deliberate divergence from `digit_of`, which also reads
/// the range out of `wcode`.** `wcode` is a SCANCODE (`app.rs::decode_key`), and 48–57 there are
/// not digits at all: they are `]` `\` `#` `;` `'` `` ` `` `,` `.` `/` and CapsLock. The digits'
/// scancodes are 30–39 — which is exactly why 33/34 turned out to be `4` and `5` a few dozen lines
/// up. Mirroring `digit_of` would put the retired mis-reading straight back into the gate this
/// function exists to be, so this takes the spelling the evidence supports: a remote's number key
/// arrives with its ASCII keycode in `sym` (evdev 5 -> scancode 33 -> `SDLK_4` = 52). The cost is
/// that `is_bound` is no longer a strict superset of `digit_of` for a wcode-only punctuation press
/// — which reaches no HUD and no armed click, because the PIN pad has neither. `profiles` is not
/// this module's to edit; `docs/remote-keys.md` §9 carries it as an open finding for the capture
/// that settles both.
///
/// **It still OVER-approximates in one place** and the alternative is worse. A digit is bound only
/// on the PIN pad, so a number press on Home or during playback answers `true` here and still wakes
/// the HUD. Narrowing that means making this route-aware, i.e. a second copy of the ladder's own
/// order — the exact duplication [`page_dir`] was written to end. A number key is also, unlike a
/// colour button, a key this app really does bind, so calling it bound is honest.
pub fn is_bound(sym: c_uint, wcode: c_uint) -> bool {
    classify(sym, wcode) != Key::Other
        || page_dir(sym, wcode).is_some()
        || sym == SDLK_BACKSPACE
        || sym == SDLK_CLEAR
        || (b'0' as c_uint..=b'9' as c_uint).contains(&sym)
}

// spring stiffnesses (from ui_home.c, redistributed 1:1 to their owning views)
pub const K_SCALE: f32 = 320.0;
pub const K_SCROLL: f32 = 170.0;
pub const K_SNAP: f32 = 200.0;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Rect;

    /// **The frame itself.** 5% of each axis at the authored canvas, and the two margins are not the
    /// same number — the frame is a percentage of each DIMENSION, not one distance, which is the
    /// mistake a single `MARGIN` would bake in.
    #[test]
    fn the_safe_area_is_five_percent_of_each_axis() {
        assert_eq!(MARGIN_X, SCR_W * 0.05);
        assert_eq!(MARGIN_Y, SCR_H * 0.05);
        assert_eq!((SAFE.x, SAFE.y, SAFE.w, SAFE.h), (96.0, 54.0, 1728.0, 972.0));
        assert!(inside_safe(SAFE), "the frame contains itself");
        assert!(!inside_safe(Rect::new(SAFE.x - 1.0, SAFE.y, 10.0, 10.0)), "one px left of it does not");
        assert!(!inside_safe(Rect::new(SAFE.x, SAFE.y - 1.0, 10.0, 10.0)), "nor one px above");
        assert!(!inside_safe(Rect::FULL), "nor the whole panel");
    }

    /// **NO REQUIRED CONTENT ENTERS THE OVERSCAN EXCLUSION ZONE, ON EITHER AXIS.**
    ///
    /// LG's App Self Checklist item #2 asks that the buttons, texts and logos on the main page sit
    /// inside the overscan frame; this is that sentence, executable, over the outermost rect of
    /// every screen and panel in the app.
    ///
    /// **It grades the composed geometry, never the tokens.** There is deliberately no
    /// `assert_eq!(MARGIN_X, 96.0)` here: that passes today and forbids the next audit from moving
    /// the margin, or from giving one screen a correction of its own, which is the fix such an audit
    /// most often needs. What is asserted is the requirement — so a future change that moves a token
    /// AND keeps every screen inside the frame passes without this test being rewritten to permit
    /// it, and one that moves a screen's own y by hand fails without anyone remembering to come
    /// here. Six of the rows below were OUTSIDE the frame when this was written; the ones that were
    /// worst — the A–Z rail at 32px, the detail page's pinned logo at 32, the top bar at 18 — were
    /// all in geometry no token could have described.
    ///
    /// **"Required" is doing real work in that sentence.** A full-bleed hero backdrop, a page
    /// ground, a scrim, a shelf peeking off the bottom edge and the focus GLOW that overflows a
    /// poster are all supposed to reach the panel edge; bounding them would be the bug. What is
    /// graded is what a viewer has to read or press.
    ///
    /// **So tiles are entered at REST, and that is a decision rather than an oversight.** A focused
    /// card is drawn `RowStyle::HOME`'s 1.09 about its own centre, which puts the first column's
    /// painted edge ~11px past the margin, and `GLOW_PAD` spills 48 further. Neither is new content:
    /// the pop MAGNIFIES ink already inside the frame, strictly containing its resting rect
    /// (`widgets`' own note on the control pop), and the caption under it — the TEXT — does not
    /// scale at all. The line this draws is between decoration that overflows and *the thing
    /// itself*: the detail page's pinned compact logo IS graded at its upward spill, because there
    /// the spill is the logo, and a clearLogo is one of the three things item #2 names.
    ///
    /// **What it cannot see**, so that a green run is not read as more than it is: text is graded by
    /// the box a screen lays it out in, not by rasterized ink (the host suite cannot link
    /// SDL2_ttf — the boundary `StatusOverlay::bands` documents), so a run that overflows its own
    /// column is `text::elide`'s business and not this test's. Rects whose width is a measured
    /// label are entered degenerate, with the EDGE that matters and a zero extent the other way.
    #[test]
    fn no_required_content_enters_the_safe_area_exclusion_zone() {
        let mut r: Vec<(&'static str, Rect)> = Vec::new();

        // **A probe that quietly stops contributing is a screen that quietly stops being audited**,
        // and an `assert!` loop over an empty table passes. So each one is required to contribute,
        // individually: a table-wide floor cannot see one probe of eight going silent, which is what
        // a `r.len() >= N` guard was actually doing here.
        let mut probe = |name: &str, f: &dyn Fn(&mut Vec<(&'static str, Rect)>)| {
            let before = r.len();
            f(&mut r);
            assert!(r.len() > before, "the {name} probe contributed nothing — it stopped being audited");
        };

        // ---- the shared chrome, and the screens composed on it ------------------------------
        probe("widgets", &crate::ui::widgets::overscan_rects);
        probe("library", &crate::ui::library::overscan_rects);
        probe("detail", &crate::ui::detail::overscan_rects);
        probe("player_hud", &crate::ui::player_hud::overscan_rects);

        // ---- the panels, each at the widest/tallest state its own clamp admits ---------------
        probe("account_menu", &crate::ui::account_menu::overscan_rects);
        probe("track_menu", &crate::ui::track_menu::overscan_rects);
        probe("more_menu", &crate::ui::more_menu::overscan_rects);
        probe("stats", &crate::ui::stats::overscan_rects);
        drop(probe);

        // ---- the screens whose outermost geometry is already public here --------------------
        // Home: the hero's text column and its action row start at the margin; the grid view's
        // first shelf heading is the highest ink the page draws under the bar.
        r.push(("home hero text column", Rect::new(MARGIN_X, 380.0, crate::ui::home::HERO_COL_W, 400.0)));
        r.push(("home first shelf heading (grid view)", Rect::new(MARGIN_X, GRID_TOP_Y - TITLE_DY, 400.0, TITLE_DY)));
        r.push(("home first shelf card (grid view)", Rect::new(MARGIN_X, GRID_TOP_Y + CARD_DY, CARD_W, CARD_H)));
        // …and the focused card's block at the BOTTOM of its reveal: card + the 96px label band,
        // which is what `home::update`'s and `library`'s reveal rules keep clear of the edge.
        r.push(("home focused card block, revealed", Rect::new(MARGIN_X, SCR_H - MARGIN_Y - CARD_H - 96.0, CARD_W, CARD_H + 96.0)));

        // Search: the query capsule, and the scope line beside it.
        r.push(("search field", crate::ui::search::FIELD));
        r.push(("search first shelf heading", Rect::new(MARGIN_X, crate::ui::search::CONTENT_TOP, 400.0, 40.0)));

        // Person: the portrait at the margin, and the air the reveal keeps under a shelf.
        r.push(("person portrait", Rect::new(MARGIN_X, 96.0, 320.0, 320.0)));
        r.push(("person shelf block, revealed", Rect::new(MARGIN_X, SCR_H - MARGIN_Y - CARD_H, CARD_W, CARD_H)));

        // Onboarding + login: both centre or hang off the same margin.
        r.push(("onboard copy column", Rect::new(MARGIN_X, 150.0, crate::ui::home::HERO_COL_W, 500.0)));

        for (name, rect) in r {
            assert!(
                inside_safe(rect),
                "{name} at ({}, {}) {}x{} leaves the {}x{} safe area at ({}, {})",
                rect.x, rect.y, rect.w, rect.h, SAFE.w, SAFE.h, SAFE.x, SAFE.y,
            );
        }
    }

    /// The Library pager's key set, which used to be spelled twice in `app.rs` in two shapes.
    #[test]
    fn the_pager_answers_both_spellings_and_nothing_else() {
        assert_eq!(page_dir(SDLK_PAGEUP, 0), Some(-1));
        assert_eq!(page_dir(SDLK_PAGEDOWN, 0), Some(1));
        assert_eq!(page_dir(0, WCODE_CH_UP_KEY), Some(-1)); // evdev 402/403, the real rocker
        assert_eq!(page_dir(0, WCODE_CH_DOWN_KEY), Some(1));
        assert_eq!(page_dir(SDLK_UP, 0), None, "the D-pad is not the pager");
        assert_eq!(page_dir(0, WCODE_FASTFORWARD), None);
    }

    /// **The mis-binding this file shipped with until 2026-08-23, pinned from both sides.** 33/34
    /// are `SDL_SCANCODE_4`/`SDL_SCANCODE_5`, so the pager was answering the remote's NUMBER KEYS;
    /// they are the CEA-2014-A **web** keyCodes for the channel rocker, the same namespace error as
    /// 412/417. A digit must reach no arm of the ladder — and must still be `is_bound`, because the
    /// PIN keypad types from it.
    #[test]
    fn a_number_key_does_not_page_the_library() {
        for (sym, wcode, what) in [
            (b'4' as c_uint, 33, "the digit 4"),
            (b'5' as c_uint, 34, "the digit 5"),
        ] {
            assert_eq!(page_dir(sym, wcode), None, "{what} is not the channel rocker");
            assert_eq!(classify(sym, wcode), Key::Other, "{what} names no Key either");
            assert!(is_bound(sym, wcode), "{what} IS bound — the who's-watching PIN keypad");
        }
        // …and the bare scancodes, in case a future remote sends one with no sym beside it.
        assert_eq!(page_dir(0, 33), None);
        assert_eq!(page_dir(0, 34), None);
    }

    /// [`is_bound`] is a strict SUPERSET of `classify != Other`, and the three things it adds are
    /// exactly the three the ladder handles outside the classifier. Getting this wrong in either
    /// direction is a real bug: too narrow and a real key stops un-dismissing the HUD, too wide and
    /// item 40's whole point is lost.
    #[test]
    fn the_bound_set_is_the_whole_map_and_no_more() {
        // every named Key
        for (sym, wcode) in [
            (SDLK_UP, 0),
            (SDLK_LEFT, 0),
            (SDLK_RETURN, 0),
            (SDLK_ESCAPE, 0),
            (0, WCODE_BACK),
            (0, WCODE_PLAY),
            (0, WCODE_PAUSE),
            (0, WCODE_PLAYPAUSE),
            (0, WCODE_STOP_KEY),
            (0, WCODE_EXIT),
            (0, WCODE_REWIND),
            (0, WCODE_FASTFORWARD),
            (0, WCODE_POINTER_HIDDEN),
        ] {
            assert!(is_bound(sym, wcode), "sym={sym} wcode={wcode} names a Key");
        }
        // the three sets `classify` does NOT name and the ladder still binds
        assert!(is_bound(SDLK_PAGEDOWN, 0) && is_bound(0, WCODE_CH_UP_KEY), "the pager");
        assert!(is_bound(SDLK_BACKSPACE, 42) && is_bound(SDLK_CLEAR, 156), "the panel's edit keys");
        assert!(is_bound(b'0' as c_uint, 39) && is_bound(b'9' as c_uint, 38), "the PIN digits");
        // and the ones that must stay unbound. 269 is `SDL_SCANCODE_AC_HOME` (evdev 172
        // `KEY_HOMEPAGE`) and 270 `AC_BACK`; 412/417 are the retired web-namespace codes.
        for (sym, wcode, what) in [
            (0, 269, "HOME"),
            (0, 270, "AC_BACK — not the remote's BACK, which is 482"),
            (0, 412, "the retired 412"),
            (0, 417, "the retired 417"),
            (0, 0, "nothing at all"),
            (b'a' as c_uint, 4, "a letter"),
            // THE `is_bound` counterpart of the 33/34 retirement: 48-57 in the SCANCODE field are
            // `]` `\` `;` `'` `,` `.` `/` and CapsLock, not digits (the digits are 30-39). Reading
            // ASCII out of `wcode` is the one namespace error this whole change exists to stop.
            (0, 49, "scancode 49, which is backslash and NOT the digit 1"),
            (0, 55, "scancode 55, which is a full stop and NOT the digit 7"),
        ] {
            assert!(!is_bound(sym, wcode), "{what} must reach no arm");
            assert_eq!(classify(sym, wcode), Key::Other, "{what}");
        }
    }

    /// **The transport codes settled from LG's own evdev->scancode table**, and the two that were
    /// retired by it. Kept apart from the big table above because these are the arms whose
    /// provenance is a decompiled firmware table plus 336 captured key lines, not a guess — and
    /// because the LAST two assertions are the ones that would silently regress if somebody
    /// "restored" the old D-pad names: 412/417 must now reach no direction at all.
    #[test]
    fn the_codes_lg_actually_sends() {
        // evdev 168 KEY_REWIND -> 452, evdev 208 KEY_FASTFORWARD -> 451
        assert_eq!(classify(0, WCODE_REWIND), Key::Left { alt: true });
        assert_eq!(classify(0, WCODE_FASTFORWARD), Key::Right { alt: true });
        // evdev 164 KEY_PLAYPAUSE -> 261, a toggle and so its own variant
        assert_eq!(classify(0, WCODE_PLAYPAUSE), Key::PlayPause);
        // evdev 128 KEY_STOP -> 120 and evdev 166 KEY_STOPCD -> 260, beside the legacy 413
        assert_eq!(classify(0, WCODE_STOP_KEY), Key::Stop);
        assert_eq!(classify(0, WCODE_STOP_CD), Key::Stop);
        assert_eq!(classify(0, WCODE_STOP), Key::Stop);
        // evdev 174 KEY_EXIT -> 505
        assert_eq!(classify(0, WCODE_EXIT), Key::Exit);
        // 412/417 are produced only by evdev 524/556, which `linux/input.h` does not name, and
        // never appeared in 336 real presses from this remote. They are not the D-pad, and the
        // D-pad (80/79) reaches `classify` as the plain SDLK_LEFT/RIGHT syms instead.
        assert_eq!(classify(0, 412), Key::Other, "412 is not a direction");
        assert_eq!(classify(0, 417), Key::Other, "417 is not a direction");
    }

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
            // the transport keys that carry `alt: true` — REWIND and FAST-FORWARD, in
            // each field on its own (see WCODE_REWIND for why these are not the D-pad)
            (WCODE_REWIND, 0, Key::Left { alt: true }),
            (0, WCODE_REWIND, Key::Left { alt: true }),
            (WCODE_FASTFORWARD, 0, Key::Right { alt: true }),
            (0, WCODE_FASTFORWARD, Key::Right { alt: true }),
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
            (SDLK_PAGEUP, 0),
            (SDLK_PAGEDOWN, 0),
            (0, WCODE_CH_UP_KEY),
            (0, WCODE_CH_DOWN_KEY),
            (SDLK_BACKSPACE, 42),
            (SDLK_CLEAR, 156),
        ] {
            assert_eq!(classify(sym, wcode), Key::Other, "sym={sym} wcode={wcode}");
            assert!(is_bound(sym, wcode), "…and every one of them is still BOUND");
        }
    }

    /// The two horizontal tests accept different sets, and that is the whole reason `alt` exists:
    /// an alternate-code LEFT reaches the player's scrub and not the four-way nav dispatch.
    #[test]
    fn the_two_horizontal_tests_accept_different_sets() {
        for (plain, alt) in [
            (classify(SDLK_LEFT, 0), classify(0, WCODE_REWIND)),
            (classify(SDLK_RIGHT, 0), classify(0, WCODE_FASTFORWARD)),
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
        let l = classify(SDLK_LEFT, WCODE_REWIND);
        let r = classify(SDLK_RIGHT, WCODE_FASTFORWARD);
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
