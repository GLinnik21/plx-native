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
/// `app::overscan_audit_tests::no_required_content_enters_the_safe_area_exclusion_zone` now does
/// — it grades the composed rects, not this literal, so a future audit is free to move this
/// number again without rewriting the test.
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
/// Hub-title cap top above the shelf's `row_y` origin — the heading draws at `row_y − TITLE_DY`,
/// minus whatever `CardRow::lift` has raised it by. Named because it is a LAYOUT relationship two
/// other constants here are derived against ([`CARD_DY`]'s air, [`GRID_TOP_Y`]'s clearance under the
/// profile chip) and because the shelf heading is now a multi-run flow rather than one `p.text`.
pub const TITLE_DY: f32 = 34.0;
/// Card top below the shelf's `row_y` origin (the hub title draws at `row_y − `[`TITLE_DY`]): the air
/// between a section title and its posters, held on magnification too because `title_lift` raises the
/// title by the same amount the popped card's top rises.
pub const CARD_DY: f32 = 26.0;
/// Air kept between the bottom of a focused label block ([`crate::card_row::UNDER_LABEL_H`])
/// and the next row's topmost ink — item 11. The one number Home and Library now BOTH derive their
/// row pitch from, instead of each hand-authoring its own: Home always gave this 22px (folded,
/// unnamed, into the old `ROW_PITCH = CARD_H + ROW_TITLE_H + 144.0`), while Library's old
/// `PITCH = CARD_H + 96.0` gave a two-line label block only 4px of air — glued to the next row's
/// card, which is the bug this item reports. Derived by working the layout backward from the code:
/// `ROW_PITCH − TITLE_DY − CARD_DY − CARD_H − UNDER_LABEL_H` and the old `PITCH − CARD_H −
/// UNDER_LABEL_H` both land here once expressed against the SAME [`crate::card_row::UNDER_LABEL_H`],
/// confirmed against sim screenshots of both grids with a two-line focused label.
pub const UNDER_LABEL_AIR: f32 = 22.0;
/// Room for the shelf title above the row, plus the focused card's under-label block
/// ([`crate::card_row::UNDER_LABEL_H`]) and [`UNDER_LABEL_AIR`] of air below it before the next
/// shelf's own title. Replaces the old `CARD_H + ROW_TITLE_H + 144.0` — same 549px total
/// (`34 + 26 + 375 + 92 + 22`), now built from the same two named constants Library's `PITCH` uses,
/// instead of a bare `144` that silently baked in a DIFFERENT air than Library's `96` did.
///
/// **Since 2026-09-05 it is the pitch of a FOCUSED shelf — the maximum — rather than the pitch of
/// every shelf.** The label block exists only while its shelf holds focus, and reserving all 92px
/// of it on all of them spent most of a 1080 panel on emptiness; a shelf column is now a running
/// sum of `card_row::ROW_PITCH_FIXED + card_row::under_band(e)`, which is exactly this at `e == 1`.
/// Every screen that stacks shelves — Home, the Library, Search and the person page — accumulates
/// it that way, and `card_row::settled_top` (`settled_top_for` with a policy's band) is the closed form a scroll target must use. What is
/// unchanged is that this constant remains the ONE authority on a poster shelf's rhythm: the four
/// screens differ in their fixed part (a landscape row, a typed search block), and in the band only by the
/// Under posters policy a screen hands its poster shelves (`caption_policy::band_h`; the legacy band for a shelf of
/// landscape stills or headshots, which take none).
pub const ROW_PITCH: f32 =
    TITLE_DY + CARD_DY + CARD_H + crate::card_row::UNDER_LABEL_H + UNDER_LABEL_AIR;
pub const CONTENT_Y: f32 = 200.0;
/// Off-screen cull/clip-edge slack for a focused card's glow AND its risen drop-shadow — big enough
/// to cover the worst case, a large poster's `CARD_SHADOW_BLUR + CARD_SHADOW_DY` (44+18=62) plus a
/// 1px AA margin, rounded up.
pub const GLOW_PAD: f32 = 64.0;
pub use plx_base::surface::{LOGICAL_H as SCR_H, LOGICAL_W as SCR_W};

/// **The safe area itself** — the box every piece of REQUIRED content has to fit inside, as one
/// value so no caller re-derives it from the two margins and gets a sign wrong.
///
/// "Required" is the whole content of the rule and the reason this is a predicate rather than a
/// clip: a full-bleed hero backdrop, a shelf peeking off the bottom edge, a scrim, the page ground —
/// all of those are *supposed* to reach the panel edge, and clipping them would be the bug. What
/// must be inside is what the viewer has to read or press: text, controls, marks, logos.
pub const SAFE: crate::Rect = crate::Rect::new(
    MARGIN_X,
    MARGIN_Y,
    SCR_W - 2.0 * MARGIN_X,
    SCR_H - 2.0 * MARGIN_Y,
);

/// **The tolerance of an edge test, in px.** A layout derived through f32 sums lands a hair either side of an
/// exact edge, so two edges closer than this are the same edge and rects that overlap by less than it only
/// touch. [`inside_safe`] reads it; the HUD lint's overlap, `SAFE` and menu checks share it rather than each
/// keeping a copy.
pub const EDGE_TOL: f32 = 0.01;

/// Is `r` wholly inside [`SAFE`]? The one test the audit's table asks, per rect.
///
/// A hair of tolerance ([`EDGE_TOL`]) because several of the rects handed here are the result of an f32
/// derivation that lands on the boundary exactly — `SCR_W - MARGIN_X - w` then `+ w` is not bit-for-
/// bit `SCR_W - MARGIN_X` — and a control flush against the frame is compliant, not a violation.
#[inline]
pub fn inside_safe(r: crate::Rect) -> bool {
    r.x >= SAFE.x - EDGE_TOL
        && r.y >= SAFE.y - EDGE_TOL
        && r.x + r.w <= SAFE.x + SAFE.w + EDGE_TOL
        && r.y + r.h <= SAFE.y + SAFE.h + EDGE_TOL
}

// hero <-> grid continuum
/// Shelf origin in hero view: the card's 26 px inset places its top at 768, which stands 40 px under
/// the hero's action row (top 668, 60 tall: `theme::hero::HOME_ACTIONS_Y`; owner's board CW-13 v2).
/// The dive from here to [`GRID_TOP_Y`] is 548 px: the board's grid scroll.
pub const PEEK_Y: f32 = 742.0;
// shelf top in grid view, and the rest of the FOCUSED shelf on every scroll (`ui_cards::grid_rest_scroll`
// pins it here whatever the Under posters band): leaves the hub title (row_y − TITLE_DY, lifted up to
// ~17 more when its leftmost card magnifies) a clear space::MD under the profile chip (bottom edge 126).
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
/// the webOS `wcode` at +20; the raw-offset reading is a gotcha in `docs/agent-reference.md`).
///
/// [`Key::Other`] is every press `classify` does not name, which includes spellings the ladder
/// still tests by hand where it needs them: the CH▲/CH▼ rocker ([`WCODE_CH_UP_KEY`]/
/// [`WCODE_CH_DOWN_KEY`] and the `SDLK_PAGE*` syms, in the Library's paging arm via [`page_dir`]),
/// the system keyboard's edit keys ([`SDLK_BACKSPACE`]/[`SDLK_CLEAR`], read inside the Search
/// screen) and the digits the who's-watching PIN keypad types from. **So `Other` does NOT mean
/// unbound and must never be made inert** — [`is_bound`] is the predicate that answers *is this
/// press one the app binds at all*, and it is a strict superset of `classify != Other`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Up,
    Down,
    /// LEFT and RIGHT carry a flag because the ladder asks about them in two ways that accept
    /// DIFFERENT sets, and the difference is behaviour rather than an accident of spelling.
    /// [`classify`] sets `alt` to `false` when the press arrived as the plain [`SDLK_LEFT`]/[`SDLK_RIGHT`] sym, and
    /// `true` when it arrived only as a TRANSPORT key, [`WCODE_REWIND`]/[`WCODE_FASTFORWARD`]
    /// (in either field). The non-player four-way nav dispatch matches `alt: false` alone, so an
    /// alternate-code LEFT on Home reaches no arm that acts on it; the player's scrub arm and the
    /// Chapters strip match both. Preserve that asymmetry — it is what the flag is for.
    /// [`classify_input`] also uses `alt: false` for an already-canonical navigation direction;
    /// its screen consumers act on that direction without branching on the raw-code provenance.
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
    /// The remote's EXIT key, and since 2026-09-03 the ONLY key that ends the process. BACK at a
    /// root hands the screen back to the television and leaves the app running (`tv::home::go_home`),
    /// because a press people lean on must not be able to quit by accident; a key labelled EXIT
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
/// direction rather than as the transport key. Both fields describe ONE press — the shell's
/// `plx_shell_sdl::event::decode_key` reads them off a single event and the application's `remote_token_key` builds each
/// synthetic pair from a single token — so a pair naming two DIFFERENT keys is not a shape either path is
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

/// Classify an owned input event. The Input machine's canonical key is authoritative: pointer
/// releases and scripted navigation have no raw SDL pair at all. Only `Other` needs raw fields
/// to distinguish transport actions outside the small navigation alphabet. Screens must use
/// this boundary instead of reclassifying raw fields and losing the machine's decision.
pub fn classify_input(key: plx_machine::machine::Key, sym: c_uint, wcode: c_uint) -> Key {
    use plx_machine::machine::Key as Canonical;
    match key {
        Canonical::Up => Key::Up,
        Canonical::Down => Key::Down,
        Canonical::Left => Key::Left { alt: false },
        Canonical::Right => Key::Right { alt: false },
        Canonical::Ok => Key::Ok,
        Canonical::Back => Key::Back,
        Canonical::Other => classify(sym, wcode),
    }
}

#[cfg(test)]
mod canonical_input_tests {
    use super::*;
    use plx_machine::machine::Key as Canonical;

    #[test]
    fn canonical_keys_work_without_raw_codes_and_outrank_them() {
        for (canonical, expected) in [(Canonical::Up, Key::Up), (Canonical::Down, Key::Down),
            (Canonical::Left, Key::Left { alt: false }), (Canonical::Right, Key::Right { alt: false }),
            (Canonical::Ok, Key::Ok), (Canonical::Back, Key::Back)] {
            assert_eq!(classify_input(canonical, 0, 0), expected);
            assert_eq!(classify_input(canonical, 0, WCODE_PAUSE), expected);
        }
    }

    #[test]
    fn other_preserves_physical_transport_and_scrub_codes() {
        for (sym, wcode) in [(0, WCODE_PLAY), (0, WCODE_PAUSE), (0, WCODE_PLAYPAUSE),
            (0, WCODE_STOP), (0, WCODE_REWIND), (0, WCODE_FASTFORWARD), (0, WCODE_EXIT),
            (0, WCODE_POINTER_HIDDEN), (0, 0)] {
            assert_eq!(classify_input(Canonical::Other, sym, wcode), classify(sym, wcode));
        }
    }
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
/// The app's `begin_fresh_press` runs for EVERY
/// fresh press, *before* the ladder has decided whether anything takes it, and two of the things it
/// does are global — it un-dismisses the player HUD and it aborts an armed tvOS click. So an
/// unsupported key (LG checklist item 40: the colour buttons, GUIDE, INFO, the number pad on a
/// screen with no keypad, a universal remote's whole extra half) woke the transport and cancelled a
/// press in flight. The invariant is *a press consumed by neither a route-specific handler nor the
/// global key ladder must produce no global side effect*, and this is its guard.
///
/// **Five sources in a lab build, four in every other, because the map has never been one:**
/// 1. [`classify`] — every named [`Key`].
/// 2. **The Lab Diagnostics trigger** ([`plx_os::labcfg::is_trigger_key`]) — a key this build really
///    does bind, read from `lab.json` rather than written here. `false` at COMPILE time in every
///    build without the `lab-diagnostics` feature, which is every build anyone can install. It has
///    to be in this predicate or pressing it would also wake the player HUD and abort an armed
///    click — the exact pair of side effects this function exists to withhold.
/// 3. [`page_dir`] — the Library pager, a SEPARATE predicate (see its doc).
/// 4. [`SDLK_BACKSPACE`] / [`SDLK_CLEAR`] — the television keyboard's own edit keys, read inside
///    the owned Search screen (`screens::search`'s key match), which the classifier never sees.
/// 5. An ASCII digit **in `sym`** — the who's-watching PIN keypad types straight from the remote's
///    number buttons ([`pin_digit`], which owns that behaviour).
///
/// **The `sym` field ONLY, and that is a deliberate divergence from [`pin_digit`], which also reads
/// the range out of `wcode`.** `wcode` is a SCANCODE (`plx_shell_sdl::event::decode_key`), and 48–57 there are
/// not digits at all: they are `]` `\` `#` `;` `'` `` ` `` `,` `.` `/` and CapsLock. The digits'
/// scancodes are 30–39 — which is exactly why 33/34 turned out to be `4` and `5` a few dozen lines
/// up. Mirroring [`pin_digit`] would put the retired mis-reading straight back into the gate this
/// function exists to be, so this takes the spelling the evidence supports: a remote's number key
/// arrives with its ASCII keycode in `sym` (evdev 5 -> scancode 33 -> `SDLK_4` = 52). The cost is
/// that `is_bound` is no longer a strict superset of [`pin_digit`] for a wcode-only punctuation press
/// — which reaches no HUD and no armed click, because the PIN pad has neither. `docs/remote-keys.md`
/// §9 carries it as an open finding for the capture that settles both.
///
/// **It still OVER-approximates in one place** and the alternative is worse. A digit is bound only
/// on the PIN pad, so a number press on Home or during playback answers `true` here and still wakes
/// the HUD. Narrowing that means making this route-aware, i.e. a second copy of the ladder's own
/// order — the exact duplication [`page_dir`] was written to end. A number key is also, unlike a
/// colour button, a key this app really does bind, so calling it bound is honest.
pub fn is_bound(sym: c_uint, wcode: c_uint) -> bool {
    is_remote_bound(sym, wcode) || plx_os::labcfg::is_trigger_key(sym, wcode)
}

/// The fixed key map, independent of a lab build's runtime diagnostics configuration: a press is
/// bound exactly when it has an [`Action`]. The four fixed sources [`is_bound`] lists (all but the
/// lab trigger) are the four arms [`action_of`] reads, so there is one spelling of the map and
/// this is its predicate.
pub fn is_remote_bound(sym: c_uint, wcode: c_uint) -> bool {
    action_of(sym, wcode).is_some()
}

/// The raw `(sym, wcode)` pair a key arrived as. Kept beside the [`Action`] because three things
/// still read it: a press with no action (an unsupported key flows on as exactly the pair it was),
/// the lab trigger key (named by `lab.json`, not by an action), and the recorder's tape, whose
/// format IS the pair. A platform with no SDL pair sends none, and the loop builds [`raw_pair`]
/// for its action.
///
/// It lives here, beside [`raw_pair`] and [`action_of`] whose pair it names, so a platform crate
/// reaches it without the application's private events module (`docs/platform-ports.md`, Target
/// shape 8: `host::EventSink::key` takes one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawKey {
    pub sym: c_uint,
    pub wcode: c_uint,
}

/// **A key press by what it MEANS, with no SDL in it** — the vocabulary a platform that has no SDL
/// key pair hands the loop (`HostEvent::Key`'s `action`), and the one the screens move onto when
/// they stop reading `(sym, wcode)` themselves.
///
/// It is the union of the four fixed sources [`is_bound`] documents (everything but the lab
/// trigger, which `lab.json` names at run time), one variant per distinct press the app acts on: [`classify`]'s named keys (with [`Key::Left`]/[`Key::Right`]'s `alt: true` spelling
/// split out as [`Action::Rewind`]/[`Action::FastForward`], which is what it is), [`page_dir`]'s
/// two, the system keyboard's two edit keys and the number keys. [`Key::Other`] has no variant: a
/// press with no action is a press the app does not bind.
///
/// **`Action` and the raw pair are two spellings of one fact and each has exactly one function.**
/// [`raw_pair`] is the ONLY place a `(sym, wcode)` is built from an action, and [`action_of`] the
/// only place one is read back into it; `action_of(raw_pair(a)) == Some(a)` for every action
/// (`action_tests`). So a platform that sends an action reaches the loop's raw readers through the
/// same pair a real remote would have sent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Up,
    Down,
    Left,
    Right,
    Ok,
    Back,
    Play,
    Pause,
    PlayPause,
    Stop,
    /// The transport's REWIND key: [`classify`]'s `Left { alt: true }`. It is not a plain
    /// [`Action::Left`], and a platform that cannot tell them apart would make the player's scrub
    /// and Home's navigation one key.
    Rewind,
    /// [`classify`]'s `Right { alt: true }`; see [`Action::Rewind`].
    FastForward,
    Exit,
    /// The Library grid's pager, up: [`page_dir`]'s `-1`.
    PageUp,
    /// The Library grid's pager, down: [`page_dir`]'s `1`.
    PageDown,
    /// The system keyboard's delete key ([`SDLK_BACKSPACE`]).
    Backspace,
    /// The system keyboard's Clear-all button ([`SDLK_CLEAR`]).
    ClearAll,
    /// A number key, `0..=9`. Build one with [`Action::digit`]; a value above 9 names no key and
    /// [`raw_pair`] gives it a pair no action reads back.
    Digit(u8),
    /// The Magic Remote reporting that its pointer auto-hid; a notification, not a press.
    PointerHidden,
}

impl Action {
    /// The number key for `n`, or `None` above 9.
    pub const fn digit(n: u8) -> Option<Action> {
        if n <= 9 { Some(Action::Digit(n)) } else { None }
    }
}

/// The ONE `(sym, wcode)` pair the app builds for `action` when no SDL event carries one, and the
/// pair the remote FIFO's named tokens (`app/events.rs::remote_token_key`) press.
///
/// Each is the pair a press of that key already reaches the app as in the synthetic path — the
/// pager is `sym` alone, the transport keys `wcode` alone — so a recorded tape and
/// `tests/keytable.json` read the same pairs they always did. Where a key has several real
/// spellings the one chosen is the one the FIFO has always sent ([`Action::Back`] is ESC, not
/// `wcode` 482; [`Action::Stop`] is [`WCODE_STOP`], not the LG-table 120/260), and [`action_of`]
/// still accepts every spelling.
pub const fn raw_pair(action: Action) -> (c_uint, c_uint) {
    match action {
        Action::Up => (SDLK_UP, 0),
        Action::Down => (SDLK_DOWN, 0),
        Action::Left => (SDLK_LEFT, 0),
        Action::Right => (SDLK_RIGHT, 0),
        Action::Ok => (SDLK_RETURN, 0),
        Action::Back => (SDLK_ESCAPE, 0),
        Action::Play => (0, WCODE_PLAY),
        Action::Pause => (0, WCODE_PAUSE),
        Action::PlayPause => (0, WCODE_PLAYPAUSE),
        Action::Stop => (0, WCODE_STOP),
        Action::Rewind => (0, WCODE_REWIND),
        Action::FastForward => (0, WCODE_FASTFORWARD),
        Action::Exit => (0, WCODE_EXIT),
        Action::PageUp => (SDLK_PAGEUP, 0),
        Action::PageDown => (SDLK_PAGEDOWN, 0),
        // The keyboard's backspace arrives with scancode 42 beside its sym (device-measured; see
        // `SDLK_BACKSPACE`), and Clear with 156; neither is read as anything else.
        Action::Backspace => (SDLK_BACKSPACE, 42),
        Action::ClearAll => (SDLK_CLEAR, 156),
        Action::Digit(n) if n <= 9 => (b'0' as c_uint + n as c_uint, 0),
        Action::Digit(_) => (0, 0),
        Action::PointerHidden => (0, WCODE_POINTER_HIDDEN),
    }
}

/// The [`Action`] a raw `(sym, wcode)` press is, or `None` for a press the app does not bind
/// (an unsupported key: the colour buttons, GUIDE, INFO). The ONE decode.
///
/// **The order is the precedence, and it is [`classify`]'s first.** [`classify`] settles every
/// named key exactly as the ladder has always read it (directions by `sym` before the transport
/// codes, `PAUSE` and `PLAY` by `wcode` alone, the alternates in either field), so a pair carrying
/// two keys' fields resolves as it always did. Only a press `classify` calls `Other` reaches the
/// rest, in this order: the Library pager ([`page_dir`]), then the system keyboard's edit keys and
/// the number keys, read from `sym`. That is why the simulator's `(8, 482)` (a desktop Backspace
/// with the BACK `wcode`) is [`Action::Back`] here and Backspace to a screen that reads `sym` for
/// itself: the loop keeps the raw pair beside the action, so both readings survive.
///
/// **A number key is read from `sym` ONLY**, as [`is_remote_bound`] has always read it, because
/// `wcode` is a SCANCODE and 48–57 there are punctuation, not digits (`docs/remote-keys.md` §9).
/// [`pin_digit`], which the PIN keypad still types from, ALSO reads `wcode`; the two differ for a
/// press that carries a digit-range scancode and no digit `sym`. That press keeps typing a digit
/// while the PIN keypad reads the raw pair, and W5d, which moves the keypad onto [`Action::Digit`],
/// decides whether it still should.
pub fn action_of(sym: c_uint, wcode: c_uint) -> Option<Action> {
    match classify(sym, wcode) {
        Key::Up => return Some(Action::Up),
        Key::Down => return Some(Action::Down),
        Key::Left { alt: false } => return Some(Action::Left),
        Key::Left { alt: true } => return Some(Action::Rewind),
        Key::Right { alt: false } => return Some(Action::Right),
        Key::Right { alt: true } => return Some(Action::FastForward),
        Key::Ok => return Some(Action::Ok),
        Key::Back => return Some(Action::Back),
        Key::Play => return Some(Action::Play),
        Key::Pause => return Some(Action::Pause),
        Key::PlayPause => return Some(Action::PlayPause),
        Key::Stop => return Some(Action::Stop),
        Key::Exit => return Some(Action::Exit),
        Key::PointerHidden => return Some(Action::PointerHidden),
        Key::Other => {}
    }
    match page_dir(sym, wcode) {
        Some(dir) if dir < 0 => return Some(Action::PageUp),
        Some(_) => return Some(Action::PageDown),
        None => {}
    }
    match sym {
        SDLK_BACKSPACE => Some(Action::Backspace),
        SDLK_CLEAR => Some(Action::ClearAll),
        s if (b'0' as c_uint..=b'9' as c_uint).contains(&s) => Some(Action::Digit((s - b'0' as c_uint) as u8)),
        _ => None,
    }
}

/// Remote number key → keypad digit, moved unchanged from `screens::profiles` (it was `digit_of`): SDL gives
/// a printable key its ASCII sym and the webOS remote's number buttons carry the same 48-57
/// ('0'-'9') range in `wcode`. The PIN keypad types from it, and it reads BOTH fields, which
/// [`action_of`]'s number-key arm deliberately does not (see there).
pub fn pin_digit(sym: u32, wcode: u32) -> Option<u8> {
    [sym, wcode]
        .into_iter()
        .find(|v| (48..=57).contains(v))
        .map(|v| v as u8)
}

// spring stiffnesses (from ui_home.c, redistributed 1:1 to their owning views)
pub const K_SCALE: f32 = 320.0;
pub const K_SCROLL: f32 = 170.0;
pub const K_SNAP: f32 = 200.0;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Rect;

    /// **The frame itself.** 5% of each axis at the authored canvas, and the two margins are not the
    /// same number — the frame is a percentage of each DIMENSION, not one distance, which is the
    /// mistake a single `MARGIN` would bake in.
    #[test]
    fn the_safe_area_is_five_percent_of_each_axis() {
        assert_eq!(MARGIN_X, SCR_W * 0.05);
        assert_eq!(MARGIN_Y, SCR_H * 0.05);
        assert_eq!(
            (SAFE.x, SAFE.y, SAFE.w, SAFE.h),
            (96.0, 54.0, 1728.0, 972.0)
        );
        assert!(inside_safe(SAFE), "the frame contains itself");
        assert!(
            !inside_safe(Rect::new(SAFE.x - 1.0, SAFE.y, 10.0, 10.0)),
            "one px left of it does not"
        );
        assert!(
            !inside_safe(Rect::new(SAFE.x, SAFE.y - 1.0, 10.0, 10.0)),
            "nor one px above"
        );
        assert!(!inside_safe(Rect::FULL), "nor the whole panel");
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
            assert_eq!(
                page_dir(sym, wcode),
                None,
                "{what} is not the channel rocker"
            );
            assert_eq!(
                classify(sym, wcode),
                Key::Other,
                "{what} names no Key either"
            );
            assert!(
                is_bound(sym, wcode),
                "{what} IS bound — the who's-watching PIN keypad"
            );
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
        assert!(
            is_bound(SDLK_PAGEDOWN, 0) && is_bound(0, WCODE_CH_UP_KEY),
            "the pager"
        );
        assert!(
            is_bound(SDLK_BACKSPACE, 42) && is_bound(SDLK_CLEAR, 156),
            "the panel's edit keys"
        );
        assert!(
            is_bound(b'0' as c_uint, 39) && is_bound(b'9' as c_uint, 38),
            "the PIN digits"
        );
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
            (
                0,
                55,
                "scancode 55, which is a full stop and NOT the digit 7",
            ),
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
    /// `nav4` is the non-player four-way nav dispatch; `lr` is the player's scrub arm (and
    /// the scrub's key-up/auto-repeat companions).
    fn nav4(k: Key) -> bool {
        matches!(
            k,
            Key::Up | Key::Down | Key::Left { alt: false } | Key::Right { alt: false }
        )
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
            assert!(
                is_bound(sym, wcode),
                "…and every one of them is still BOUND"
            );
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
            assert!(
                !nav4(alt),
                "the four-way nav dispatch takes the plain syms only"
            );
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

/// **The `Action` map, graded** (W5b): `action_of` is the one decode of a raw pair, `raw_pair` the one
/// builder, and `is_remote_bound` is `action_of(..).is_some()`. The pairs below are spelled as
/// literals where the key has LG-table or alternate spellings, so the table is not read back from
/// the functions it grades.
#[cfg(test)]
mod action_tests {
    use super::*;

    const ALL: [Action; 28] = [
        Action::Up,
        Action::Down,
        Action::Left,
        Action::Right,
        Action::Ok,
        Action::Back,
        Action::Play,
        Action::Pause,
        Action::PlayPause,
        Action::Stop,
        Action::Rewind,
        Action::FastForward,
        Action::Exit,
        Action::PageUp,
        Action::PageDown,
        Action::Backspace,
        Action::ClearAll,
        Action::PointerHidden,
        Action::Digit(0),
        Action::Digit(1),
        Action::Digit(2),
        Action::Digit(3),
        Action::Digit(4),
        Action::Digit(5),
        Action::Digit(6),
        Action::Digit(7),
        Action::Digit(8),
        Action::Digit(9),
    ];

    /// Every pair a press can carry, as far as the app has ever seen or built one: each named
    /// sym and code, the FIFO's pairs, the simulator's stand-ins, every ASCII sym, and every wcode
    /// to 600 (past the LG-private 450-505 block).
    fn grid() -> impl Iterator<Item = (c_uint, c_uint)> {
        let named = [
            SDLK_UP, SDLK_DOWN, SDLK_LEFT, SDLK_RIGHT, SDLK_RETURN, SDLK_KP_ENTER, SDLK_SELECT,
            SDLK_ESCAPE, SDLK_PAGEUP, SDLK_PAGEDOWN, SDLK_BACKSPACE, SDLK_CLEAR,
            WCODE_PAUSE, WCODE_STOP, WCODE_PAUSE_ALT, WCODE_PLAY_ALT_A, WCODE_PLAY_ALT_B,
            WCODE_POINTER_HIDDEN, WCODE_BACK, WCODE_PLAY, WCODE_REWIND, WCODE_FASTFORWARD,
            WCODE_STOP_KEY, WCODE_STOP_CD, WCODE_PLAYPAUSE, WCODE_EXIT, WCODE_CH_UP_KEY,
            WCODE_CH_DOWN_KEY,
        ];
        (0..256).chain(named).flat_map(|sym| (0..600).map(move |wcode| (sym, wcode)))
    }

    /// The four-term expression `is_remote_bound` was before it became `action_of(..).is_some()`,
    /// kept here verbatim as the oracle the new one is graded against.
    fn old_is_remote_bound(sym: c_uint, wcode: c_uint) -> bool {
        classify(sym, wcode) != Key::Other
            || page_dir(sym, wcode).is_some()
            || sym == SDLK_BACKSPACE
            || sym == SDLK_CLEAR
            || (b'0' as c_uint..=b'9' as c_uint).contains(&sym)
    }

    /// `raw_pair` and `action_of` are inverses, for every action: a platform that sends an action
    /// reaches the loop's raw readers through a pair that decodes back to it.
    #[test]
    fn every_action_reads_back_from_its_own_pair() {
        for action in ALL {
            let (sym, wcode) = raw_pair(action);
            assert_eq!(action_of(sym, wcode), Some(action), "{action:?} via ({sym}, {wcode})");
        }
    }

    /// The pair table, key by key, with the spellings a real remote or the simulator sends.
    #[test]
    fn every_spelling_maps_to_its_action() {
        let table: [(c_uint, c_uint, Action); 40] = [
            (SDLK_UP, 0, Action::Up),
            (SDLK_DOWN, 0, Action::Down),
            (SDLK_LEFT, 0, Action::Left),
            (SDLK_RIGHT, 0, Action::Right),
            (SDLK_RETURN, 0, Action::Ok),
            (SDLK_KP_ENTER, 0, Action::Ok),
            (SDLK_SELECT, 0, Action::Ok),
            (SDLK_ESCAPE, 0, Action::Back),
            (b'q' as c_uint, 0, Action::Back),
            (0, 461, Action::Back),
            (0, 482, Action::Back),
            (0, 450, Action::Play),
            (0, 19, Action::Play),
            (19, 0, Action::Play),
            (0, 402, Action::Play),
            (402, 0, Action::Play),
            (0, 72, Action::Pause),
            (0, 415, Action::Pause),
            (415, 0, Action::Pause),
            (0, 261, Action::PlayPause),
            (261, 0, Action::PlayPause),
            (0, 413, Action::Stop),
            (0, 120, Action::Stop),
            (0, 260, Action::Stop),
            (413, 0, Action::Stop),
            (0, 452, Action::Rewind),
            (452, 0, Action::Rewind),
            (0, 451, Action::FastForward),
            (451, 0, Action::FastForward),
            (0, 505, Action::Exit),
            (505, 0, Action::Exit),
            (SDLK_PAGEUP, 0, Action::PageUp),
            (0, 300, Action::PageUp),
            (SDLK_PAGEDOWN, 0, Action::PageDown),
            (0, 301, Action::PageDown),
            (8, 42, Action::Backspace),
            (SDLK_CLEAR, 156, Action::ClearAll),
            (0, 0x1e4, Action::PointerHidden),
            (b'4' as c_uint, 33, Action::Digit(4)),
            (b'0' as c_uint, 0, Action::Digit(0)),
        ];
        for (sym, wcode, action) in table {
            assert_eq!(action_of(sym, wcode), Some(action), "({sym}, {wcode})");
        }
    }

    /// Rewind and FastForward are `Left`/`Right { alt: true }` and never the plain directions, and
    /// the plain `sym` still wins the pair that carries both (the precedence `classify` documents).
    #[test]
    fn transport_is_not_a_direction_and_a_plain_sym_wins() {
        assert_eq!(action_of(0, WCODE_REWIND), Some(Action::Rewind));
        assert_eq!(action_of(0, WCODE_FASTFORWARD), Some(Action::FastForward));
        assert_eq!(action_of(SDLK_LEFT, WCODE_REWIND), Some(Action::Left));
        assert_eq!(action_of(SDLK_RIGHT, WCODE_FASTFORWARD), Some(Action::Right));
        assert_ne!(action_of(0, WCODE_PLAY), action_of(0, WCODE_PAUSE), "Play is not Pause");
        assert_ne!(action_of(SDLK_PAGEUP, 0), action_of(SDLK_PAGEDOWN, 0), "PageUp is not PageDown");
    }

    /// The simulator's desktop Backspace is `(8, 482)`: BACK to `classify`, so Back here, with the
    /// raw pair kept beside the action so a screen that reads `sym` still sees a Backspace. The
    /// FIFO's `backspace` token, `(8, 42)`, and a bare sym 8 are Backspace.
    #[test]
    fn the_simulators_backspace_is_back_and_the_fifos_is_backspace() {
        assert_eq!(action_of(8, 482), Some(Action::Back));
        assert_eq!(action_of(8, 42), Some(Action::Backspace));
        assert_eq!(action_of(8, 0), Some(Action::Backspace));
    }

    /// A press the app does not bind has no action: the colour buttons (the real remote's never
    /// reach us, and 504 is the code `docs/remote-keys.md` §9 records for KEY_GREEN), GUIDE-class
    /// codes, and a wcode-only number-range scancode.
    #[test]
    fn an_unbound_press_has_no_action() {
        for (sym, wcode) in [(0, 0), (0, 504), (0, 403), (0, 412), (0, 417), (0, 1), (b'a' as c_uint, 4), (0, 50)] {
            assert_eq!(action_of(sym, wcode), None, "({sym}, {wcode})");
            assert!(!is_remote_bound(sym, wcode), "({sym}, {wcode})");
        }
    }

    /// `is_remote_bound` became `action_of(..).is_some()`: over every pair the grid names it
    /// answers what the four-term expression it replaced answered.
    #[test]
    fn the_bound_predicate_is_unchanged_over_the_whole_grid() {
        for (sym, wcode) in grid() {
            assert_eq!(
                is_remote_bound(sym, wcode),
                old_is_remote_bound(sym, wcode),
                "({sym}, {wcode})"
            );
        }
    }

    /// The pager and the edit keys read back as `page_dir` and the sym tests did.
    #[test]
    fn paging_and_editing_agree_with_their_own_readers() {
        for (sym, wcode) in grid().filter(|&(s, w)| classify(s, w) == Key::Other) {
            let paged = match page_dir(sym, wcode) {
                Some(-1) => Some(Action::PageUp),
                Some(1) => Some(Action::PageDown),
                _ => None,
            };
            if paged.is_some() {
                assert_eq!(action_of(sym, wcode), paged, "({sym}, {wcode})");
            } else if sym == SDLK_BACKSPACE {
                assert_eq!(action_of(sym, wcode), Some(Action::Backspace), "({sym}, {wcode})");
            } else if sym == SDLK_CLEAR {
                assert_eq!(action_of(sym, wcode), Some(Action::ClearAll), "({sym}, {wcode})");
            }
        }
    }

    /// Every `classify`-named key has its own action and the action's pair classifies back to it:
    /// the pair a platform's action expands to drives the ladder as the real key would.
    #[test]
    fn an_action_pair_drives_the_ladder_as_its_key() {
        let expect = [
            (Action::Up, Key::Up),
            (Action::Down, Key::Down),
            (Action::Left, Key::Left { alt: false }),
            (Action::Right, Key::Right { alt: false }),
            (Action::Rewind, Key::Left { alt: true }),
            (Action::FastForward, Key::Right { alt: true }),
            (Action::Ok, Key::Ok),
            (Action::Back, Key::Back),
            (Action::Play, Key::Play),
            (Action::Pause, Key::Pause),
            (Action::PlayPause, Key::PlayPause),
            (Action::Stop, Key::Stop),
            (Action::Exit, Key::Exit),
            (Action::PointerHidden, Key::PointerHidden),
        ];
        for (action, key) in expect {
            let (sym, wcode) = raw_pair(action);
            assert_eq!(classify(sym, wcode), key, "{action:?}");
        }
        for action in [Action::PageUp, Action::PageDown, Action::Backspace, Action::ClearAll, Action::Digit(7)] {
            let (sym, wcode) = raw_pair(action);
            assert_eq!(classify(sym, wcode), Key::Other, "{action:?} is taken by hand, not by the ladder");
        }
    }

    /// The pad's digit reader moved here unchanged and still reads BOTH fields (so a wcode-only
    /// press types a digit on the PIN keypad), while `action_of` reads `sym` only, as the bound
    /// predicate always has. The two disagree on a digit-range scancode with no digit sym; the
    /// keypad still reads the raw pair, and W5d decides what the Action does.
    #[test]
    fn the_pin_digit_reads_both_fields_and_the_action_reads_sym() {
        assert_eq!(pin_digit(b'7' as u32, 0), Some(b'7'), "a dev keyboard's sym");
        assert_eq!(pin_digit(0, 55), Some(b'7'), "the remote's wcode, same digit");
        assert_eq!(pin_digit(b'0' as u32, 0), Some(b'0'));
        assert_eq!(pin_digit(999, 0), None, "not a digit in either field");
        assert_eq!(action_of(0, 55), None, "scancode 55 is a full stop, not a digit");
        assert_eq!(action_of(b'7' as c_uint, 0), Some(Action::Digit(7)));
        assert!(!is_remote_bound(0, 55));
    }

    /// A digit above 9 is no key: no constructor makes one, and a hand-built one has a pair nothing reads.
    #[test]
    fn a_digit_above_nine_names_no_key() {
        assert_eq!(Action::digit(9), Some(Action::Digit(9)));
        assert_eq!(Action::digit(10), None);
        assert_eq!(raw_pair(Action::Digit(10)), (0, 0));
        assert_eq!(action_of(0, 0), None);
    }
}
